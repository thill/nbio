//! # Description
//!
//! This crate aims to make bi-directional, nonblocking I/O easier to reason about by using patterns that extend beyond dealing directly
//! with raw bytes, the [`std::io::Read`] and [`std::io::Write`] traits, and [`std::io::ErrorKind::WouldBlock`] errors.
//! Since this crate's main focus is nonblocking I/O, all [`Session`] implementations provided by this crate are non-blocking by default.
//!
//! # Sessions
//!
//! The core [`Session`] API utilizes associated types to express nonblocking read and write operations.
//! While the [`tcp`] module provides a [`Session`] implementation that provides unframed non-blocking binary IO operations,
//! other [`Session`] impls are able to provide significantly more functionality using the same non-blocking patterns.
//!
//! # Associated Types
//!
//! Sessions operate on implementation-specific [`Session::ReadData`] and [`Session::WriteData`] types.
//! Instead of populating a mutable buffer provided by the user a-la [`std::io::Read`] operations, a reference to a received event is returned.
//! This allows [`Session`] implementations to perform internal buffering, framing, and serialization to support implementation-specific types.
//!
//! # Errors
//!
//! The philosophy of this crate is that an [`Err`] should always represent a transport or protocol-level error.
//! An [`Err`] should not be returned by a function as a condition that should be handled during **normal** branching logic.
//! As a result, instead of forcing you to handle [`std::io::ErrorKind::WouldBlock`] everywhere you deal with nonblocking code,
//! this crate will indicate partial read/write operations using [`ReadStatus::None`], [`ReadStatus::Buffered`], and [`WriteStatus::Pending`]
//! as [`Result::Ok`].
//!
//! # Non-Blocking Examples
//!
//! ## Streaming TCP
//!
//! The following example shows how to use streaming TCP to send and receive a traditional stream of bytes.
//!
//! ```no_run
//! use nbio::{ReadStatus, Session, WriteStatus};
//! use nbio::tcp::TcpSession;
//!
//! // establish connection
//! let mut client = TcpSession::connect("192.168.123.456:54321").unwrap();
//!
//! // send some bytes until completion
//! let mut remaining_write = "hello world!".as_bytes();
//! while let WriteStatus::Pending(pending) = client.write(remaining_write).unwrap() {
//!     remaining_write = pending;
//!     client.drive().unwrap();
//! }
//!
//! // print received bytes
//! loop {
//!     if let ReadStatus::Data(data) = client.read().unwrap() {
//!         println!("received: {data:?}");
//!     }
//! }
//! ```
//!
//! ## Framing TCP
//!
//! The following example shows how to [`frame`] messages over TCP to send and receive payloads framed with a preceeding u64 length field.
//! Notice how it is almost identical to the code above, except it guarantees that read slices are always identical to their corresponding write slices.
//!
//! ```no_run
//! use nbio::{ReadStatus, Session, WriteStatus};
//! use nbio::tcp::TcpSession;
//! use nbio::frame::{FramingSession, U64FramingStrategy};
//!
//! // establish connection wrapped in a framing session
//! let client = TcpSession::connect("192.168.123.456:54321").unwrap();
//! let mut client = FramingSession::new(client, U64FramingStrategy::new(), 4096);
//!
//! // send some bytes until completion
//! let mut remaining_write = "hello world!".as_bytes();
//! while let WriteStatus::Pending(pending) = client.write(remaining_write).unwrap() {
//!     remaining_write = pending;
//!     client.drive().unwrap();
//! }
//!
//! // print received bytes
//! loop {
//!     if let ReadStatus::Data(data) = client.read().unwrap() {
//!         println!("received: {data:?}");
//!     }
//! }
//! ```
//!
//! ## HTTP Request/Response
//!
//! The following example shows how to use the [`http`] module to drive an HTTP 1.x request/response using the same non-blocking model.
//! Notice how the primitives of driving a buffered write to completion and receiving a framed response is the same as any other framed session.
//! In fact, the `conn` returned by `client.request(..)` is simply a [`frame::FramingSession`] that utilizes a [`http::Http1FramingStrategy`].
//!
//! ```no_run
//! use http::Request;
//! use nbio::{Session, ReadStatus};
//! use nbio::http::HttpClient;
//! use tcp_stream::OwnedTLSConfig;
//!
//! // create the client and make the request
//! let mut client = HttpClient::new();
//! let mut conn = client
//!     .request(Request::get("http://icanhazip.com").body(()).unwrap())
//!     .unwrap();
//!
//! // drive and read the conn until a full response is received
//! loop {
//!     conn.drive().unwrap();
//!     if let ReadStatus::Data(r) = conn.read().unwrap() {
//!         // validate the response
//!         println!("Response Body: {}", String::from_utf8_lossy(r.body()));
//!         break;
//!     }
//! }
//! ```

#[cfg(all(feature = "http"))]
pub extern crate http as hyperium_http;
#[cfg(all(feature = "tcp"))]
pub extern crate tcp_stream;
#[cfg(all(feature = "websocket"))]
pub extern crate tungstenite;

pub mod buffer;
pub mod frame;
#[cfg(all(feature = "http"))]
pub mod http;
pub mod mock;
#[cfg(all(feature = "tcp"))]
pub mod tcp;
pub mod util;
#[cfg(all(feature = "websocket"))]
pub mod websocket;

use std::{fmt::Debug, io::Error};

/// A bi-directional connection supporting generic read and write events.
///
/// ## Connecting
///
/// Some implementations may not default to a connected state, in which case immediate calls to `read()` and `write()` will fail.
/// The [`Session::status`] function provides the connection status, which also checks to make sure any required handshakes are also completed.
/// When [`Session::status`] returns [`ConnectionStatus::Connecting`], you may drive the connection process via the [`Session::drive()`] function.
///
/// ## Retrying
///
/// The [`Ok`] result of `read(..)` and `write(..)` operations may return `None`, `Pending`, or `Buffered`.
/// These statuses indicate that a read or write operation may need to be retried.
/// See [`ReadStatus`] and [`WriteStatus`] for more details.
///
/// ## Duty Cycles
///
/// The `drive(..)` operation is used to finish connecting and to service reading or writing buffered data.
/// Some [`Session`] implementations will require periodic calls to `drive(..)` in order to function.
pub trait Session {
    /// The type returned by the `write(..)` function.
    type WriteData<'a>
    where
        Self: 'a;

    /// The type returned by the `read(..)` function.
    type ReadData<'a>
    where
        Self: 'a;

    /// Check if the session is connected.
    ///
    /// If this returns [`ConnectionStatus::Connecting`], use [`Session::drive`] to progress the connection process.
    fn status(&self) -> ConnectionStatus;

    /// Force the connection to move to a [`ConnectionStatus::Closed`] state immediately
    fn close(&mut self);

    /// Some implementations will internally buffer messages.
    /// Those implementations will require `drive(..)` to be called continuously to completely read and/or write data.
    /// This function will return true if work was done, indicating to any scheduler that more work may be pending.
    /// When this function returns false, only then should it indicate to a scheduler that yielding or idling is appropriate.
    fn drive(&mut self) -> Result<bool, Error>;

    /// Write the given `WriteData` to the session.
    /// This will return [`WriteStatus::Pending`] if the write is not immediately completed fully.
    fn write<'a>(
        &mut self,
        data: Self::WriteData<'a>,
    ) -> Result<WriteStatus<Self::WriteData<'a>>, Error>;

    /// Attempt to read a `ReadData` from the session.
    /// This will return [`ReadStatus::Data`] when data has been read.
    /// [`ReadStatus::Buffered`] can be used to report that work was completed, but data is not ready.
    /// This means that only [`ReadStatus::None`] should be used to indicate to a scheduler that yielding or idling is appropriate.
    fn read<'a>(&'a mut self) -> Result<ReadStatus<Self::ReadData<'a>>, Error>;

    /// Flush all pending write data, blocking until completion.
    fn flush(&mut self) -> Result<(), Error>;
}

/// Returned by the [`Session::state`] function, providing the current connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionStatus {
    /// Session attempting to connect or handshake, and will move to `Connected` or `Closed` as [`Session::drive`] is called.
    Connecting,
    /// Session is currently connected, and will move `Closed` when an unrecoverable error is encountered
    Connected,
    /// Session terminal state, connection has been closed
    Closed,
}

/// Returned by the [`Session::read`] function, providing the outcome or information about the read action.
///
/// The generic type `T` will match the cooresponding [`Session::ReadData`].
pub enum ReadStatus<T> {
    /// Contains a reference to data read from the underlying [`Session`]
    Data(T),

    /// Data was buffered. This means a partial message was received, but could not be returned as complete `Data`.
    Buffered,

    /// No work was done. This is useful to signal to a scheduler or idle strategy that it may be time to yield.
    None,
}
impl<T: Debug> Debug for ReadStatus<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadStatus::Data(x) => f.write_str(&format!("ReadStatus::Data({x:?})")),
            ReadStatus::Buffered => f.write_str("ReadStatus::Buffered"),
            ReadStatus::None => f.write_str("ReadStatus::None"),
        }
    }
}
impl<T: Clone> Clone for ReadStatus<T> {
    fn clone(&self) -> Self {
        match self {
            ReadStatus::Data(x) => ReadStatus::Data(x.clone()),
            ReadStatus::Buffered => ReadStatus::Buffered,
            ReadStatus::None => ReadStatus::None,
        }
    }
}

/// Returned by the [`Session::write`] function, providing the outcome of the write action.
///
/// The generic type `T` will match the cooresponding [`Session::ReadData`].
pub enum WriteStatus<T> {
    /// The write action completed fully
    Success,

    /// The write action was not performed or was partially performed.
    ///
    /// The returned reference must be passed back into the [`Session`] `write` function for the write action to complete.
    /// Whether or not the returned reference may consist of partial data depends on the [`Session`] implementation.
    ///
    /// If you are looking for a general retry pattern, it is **always** safe to finish the write by passing this returned
    /// reference back into the `write` function for another attempt, but it is only **sometimes** appropriate to return the entire
    /// original write reference into the `write` function for a second attempt.
    Pending(T),
}
impl<T: Debug> Debug for WriteStatus<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteStatus::Success => f.write_str("WriteStatus::Success"),
            WriteStatus::Pending(x) => f.write_str(&format!("WriteStatus::Pending({x:?})")),
        }
    }
}
impl<T: Clone> Clone for WriteStatus<T> {
    fn clone(&self) -> Self {
        match self {
            WriteStatus::Success => WriteStatus::Success,
            WriteStatus::Pending(x) => WriteStatus::Pending(x.clone()),
        }
    }
}
