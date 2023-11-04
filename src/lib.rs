//! # Description
//!
//! This crate aims to make bi-direction, nonblocking I/O easier to understand and reason about by introducing
//! new patterns that extend beyond dealing directly with raw-bytes and partial I/O operations.
//! As a result, all [`Session`] implementations provided by this crate are non-blocking by default.
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
//! The philosphy of this crate is that an [`Err`] should always represent a transport or protocol-level error.
//! It should not be returned by a function as a condition that should be handled during **normal** data flow.
//! As a result, instead of forcing you to handle [`std::io::ErrorKind::WouldBlock`] everywhere you deal with nonblocking code,
//! this crate will provide `None`/`Buffered`/`Pending` results as [`Ok`] [`ReadStatus`] or [`WriteStatus`] enums.
//!
//! # Non-Blocking Examples
//!
//! ## Streaming TCP
//!
//! The following example shows how to use streaming TCP to send and receive a traditional stream of bytes.
//!
//! ```no_run
//! use nbio::{ReadStatus, Session, WriteStatus};
//! use nbio::tcp::StreamingTcpSession;
//!
//! // establish connection
//! let mut client = StreamingTcpSession::connect("192.168.123.456:54321").unwrap();
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
//! use nbio::tcp::StreamingTcpSession;
//! use nbio::frame::{FramingSession, U64FramingStrategy};
//!
//! // establish connection wrapped in a framing session
//! let client = StreamingTcpSession::connect("192.168.123.456:54321").unwrap();
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
//! use nbio::{Session, ReadStatus};
//! use nbio::http::HttpClient;
//! use nbio::hyperium_http::Request;
//! use nbio::tcp_stream::OwnedTLSConfig;
//!
//! // create the client and make the request
//! let mut client = HttpClient::new(OwnedTLSConfig::default());
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

use std::io::Error;

use tcp_stream::TLSConfig;

mod buf;

pub mod frame;

pub extern crate tcp_stream;
pub mod tcp;

pub extern crate http as hyperium_http;
pub mod http;

/// A bi-directional connection supporting generic read and write events.
///
/// ## Connecting
///
/// Some implementations may not default to a connected state.
/// The `is_connected` and `try_connect` functions can drive the session to a connected state.
///
/// ## Retrying
///
/// The [`Ok`] result of `read(..)` and `write(..)` operations may return `None`, `Pending`, or `Buffered`.
/// These statuses indicate that a read or write operation may need to be retried.
/// See [`ReadStatus`] and [`WriteStatus`] for more details.
///
/// ## Duty Cycles
pub trait Session {
    /// The type returned by the `write(..)` function.
    type WriteData: ?Sized;

    /// The type returned by the `read(..)` function.
    type ReadData: ?Sized;

    /// Check if the session is connected.
    /// If this returns false, use `try_connect(..)` to drive the connection process.
    fn is_connected(&self) -> bool;

    /// Attempt to drive the connection to an established state.
    /// This will return true when the connection is established.
    fn try_connect(&mut self) -> Result<bool, Error>;

    /// Some implementations will internally buffer messages.
    /// Those implementations will require `drive(..)` to be called continuously to completely read and/or write data.
    /// This function will return true if work was done, indicating to any scheduler that more work may be pending.
    /// When this function returns false, only then should it indicate to a scheduler that yielding or idling is appropriate.
    fn drive(&mut self) -> Result<bool, Error>;

    /// Write the given `WriteData` to the session.
    /// This will return [`WriteStatus::Pending`] if the write is not immediately completed fully.
    fn write<'a>(
        &mut self,
        data: &'a Self::WriteData,
    ) -> Result<WriteStatus<'a, Self::WriteData>, Error>;

    /// Attempt to read a `ReadData` from the session.
    /// This will return [`ReadStatus::Data`] when data has been read.
    /// [`ReadStatus::Buffered`] can be used to report that work was completed, but data is not ready.
    /// This means that only [`ReadStatus::None`] should be used to indicate to a scheduler that yielding or idling is appropriate.
    fn read<'a>(&'a mut self) -> Result<ReadStatus<'a, Self::ReadData>, Error>;

    /// Flush all pending write data, blocking until completion.
    fn flush(&mut self) -> Result<(), Error>;
}

/// Optionally implemented for [`Session`] implementations that support TLS
pub trait TlsSession: Session {
    /// Call this once to transition a connection into TLS mode.
    /// This will return true if the TLS handshake is immediately succesful, or false otherwise.
    ///
    /// Calls to `drive(..)`` will run the TLS handshake to completion.
    /// While the handshake is in progress, reads and writes will stall, reporting `Pending`/`None`.
    fn to_tls(&mut self, domain: &str, config: TLSConfig<'_, '_, '_>) -> Result<(), Error>;
}

/// Returned by the [`Session`] read function, providing the outcome or information about the read action.
///
/// The generic type `T` will match the cooresponding [`Session::ReadData`].
pub enum ReadStatus<'a, T: ?Sized> {
    /// Contains a reference to data read from the underlying [`Session`]
    Data(&'a T),

    /// Data was buffered. This means a partial message was received, but could not be returned as complete `Data`.
    Buffered,

    /// No work was done. This is useful to signal to a scheduler or idle strategy that it may be time to yield.
    None,
}

/// Returned by the [`Session`] write function, providing the outcome of the write action.
///
/// The generic type `T` will match the cooresponding [`Session::ReadData`].
pub enum WriteStatus<'a, T: ?Sized> {
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
    Pending(&'a T),
}
