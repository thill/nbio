//! # Description
//!
//! This crate aims to make it easier to reason about uni-directional and bi-directional nonblocking I/O.
//!
//! This is done using patterns that extend beyond dealing directly with raw bytes, the [`std::io::Read`] and [`std::io::Write`] traits,
//! and [`std::io::ErrorKind::WouldBlock`] errors. Since this crate's main focus is nonblocking I/O, all [`Session`] implementations provided
//! by this crate are non-blocking by default.
//!
//! # Sessions
//!
//! The core [`Session`] trait encapsulates controlling a single instance of a connection or logical session.
//! To differentiate with the [`std::io::Read`] and [`std::io::Write`] traits that only deal with raw bytes, this
//! crate uses [`Publish`] and [`Receive`] terminology, which utilize associated types to handle any payload type.
//!
//! A [`Session`] impl is typically also either [`Publish`], [`Receive`], or both.
//! While the [`tcp`] module provides a [`Session`] implementation that provides unframed non-blocking binary IO operations,
//! other [`Session`] impls are able to provide significantly more functionality using the same non-blocking patterns.
//!
//! This crate will often use the term `Duplex` to distinguish a [`Session`] that is **both** [`Publish`] and [`Receive`].
//!
//! # Associated Types
//!
//! Sessions operate on implementation-specific [`Receive::ReceivePayload`] and [`Publish::PublishPayload`] types.
//! These types are able to utilize a lifetime `'a`, which is tied to the lifetime of the underlying [`Session`],
//! providing the ability for implementations to reference internal buffers or queues without copying.
//!
//! # Errors
//!
//! The philosophy of this crate is that an [`Err`] should always represent a transport or protocol-level error.
//! An [`Err`] should not be returned by a function as a condition that should be handled during **normal** branching logic.
//! As a result, instead of forcing you to handle [`std::io::ErrorKind::WouldBlock`] everywhere you deal with nonblocking code,
//! this crate will indicate partial receive/publish operations using [`ReceiveOutcome::Idle`], [`ReceiveOutcome::Buffered`],
//! and [`PublishOutcome::Incomplete`] as [`Result::Ok`].
//!
//! # Features
//!
//! The [`Session`] impls in this crate are enabled by certain features.
//! By default, all features are enabled for rapid prototyping.
//! In a production codebase, you will likey want to pick and choose your required features.
//!
//! Feature list:
//! - `http`
//! - `tcp`
//! - `websocket`
//!
//! # Examples
//!
//! ## Streaming TCP
//!
//! The following example shows how to use streaming TCP to publish and receive a traditional stream of bytes.
//!
//! ```no_run
//! use nbio::{Publish, PublishOutcome, Receive, ReceiveOutcome, Session};
//! use nbio::tcp::TcpSession;
//!
//! // establish connection
//! let mut client = TcpSession::connect("192.168.123.456:54321").unwrap();
//!
//! // publish some bytes until completion
//! let mut pending_publish = "hello world!".as_bytes();
//! while let PublishOutcome::Incomplete(pending) = client.publish(pending_publish).unwrap() {
//!     pending_publish = pending;
//!     client.drive().unwrap();
//! }
//!
//! // print received bytes
//! loop {
//!     if let ReceiveOutcome::Payload(payload) = client.receive().unwrap() {
//!         println!("received: {payload:?}");
//!     }
//! }
//! ```
//!
//! ## Framing TCP
//!
//! The following example shows how to [`frame`] messages over TCP to publish and receive payloads framed with a preceeding u64 length field.
//! Notice how it is almost identical to the code above, except it guarantees that read slices are always identical to their corresponding write slices.
//!
//! ```no_run
//! use nbio::{Publish, PublishOutcome, Receive, ReceiveOutcome, Session};
//! use nbio::tcp::TcpSession;
//! use nbio::frame::{FrameDuplex, U64FrameDeserializer, U64FrameSerializer};
//!
//! // establish connection wrapped in a framing session
//! let client = TcpSession::connect("192.168.123.456:54321").unwrap();
//! let mut client = FrameDuplex::new(client, U64FrameDeserializer::new(), U64FrameSerializer::new(), 4096);
//!
//! // publish some bytes until completion
//! let mut pending_publish = "hello world!".as_bytes();
//! while let PublishOutcome::Incomplete(pending) = client.publish(pending_publish).unwrap() {
//!     pending_publish = pending;
//!     client.drive().unwrap();
//! }
//!
//! // print received bytes
//! loop {
//!     if let ReceiveOutcome::Payload(payload) = client.receive().unwrap() {
//!         println!("received: {payload:?}");
//!     }
//! }
//! ```
//!
//! ## HTTP Client
//!
//! The following example shows how to use the [`http`] module to drive an HTTP 1.x request/response using the same non-blocking model.
//! Notice how the primitives of driving a buffered write to completion and receiving a framed response is the same as any other framed session.
//! In fact, the `conn` returned by `client.request(..)` is simply a [`frame::FrameDuplex`] that utilizes a [`http::Http1RequestSerializer`] and
//! [`http::Http1ResponseDeserializer`].
//!
//! ```no_run
//! use http::Request;
//! use nbio::{Receive, Session, ReceiveOutcome};
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
//!     if let ReceiveOutcome::Payload(r) = conn.receive().unwrap() {
//!         println!("Response Body: {}", String::from_utf8_lossy(r.body()));
//!         break;
//!     }
//! }
//! ```
//!
//! ## WebSocket
//!
//! The following example sends a message and then receives all subsequent messages from a websocket connection.
//! Just like the HTTP example, this simply encapsulates [`frame::FrameDuplex`] but utilizes a [`websocket::WebSocketFrameSerializer`]
//! and [`websocket::WebSocketFrameDeserializer`]. All TLS and WebSocket handshaking is taken care of during the
//! [`SessionStatus::Establishing`] [`Session::status`] workflow.
//!
//! ```no_run
//! use nbio::{Publish, PublishOutcome, Receive, Session, SessionStatus, ReceiveOutcome};
//! use nbio::websocket::{Message, WebSocketSession};
//!
//! // create the client and make the request
//! let mut session = WebSocketSession::connect("wss://echo.websocket.org/", None).unwrap();
//! while session.status() == SessionStatus::Establishing {
//!      session.drive().unwrap();
//! }
//!
//! // publish a message
//! let mut pending_publish = Message::Text("hello world!".into());
//! while let PublishOutcome::Incomplete(pending) = session.publish(pending_publish).unwrap() {
//!     pending_publish = pending;
//!     session.drive().unwrap();
//! }
//!
//! // drive and receive messages
//! loop {
//!     session.drive().unwrap();
//!     if let ReceiveOutcome::Payload(r) = session.receive().unwrap() {
//!         println!("Received: {:?}", r);
//!         break;
//!     }
//! }
//! ```

#[cfg(any(feature = "http"))]
pub extern crate http as hyperium_http;
#[cfg(any(feature = "tcp"))]
pub extern crate tcp_stream;
#[cfg(any(feature = "websocket"))]
pub extern crate tungstenite;

pub mod buffer;
pub mod compat;
pub mod frame;
#[cfg(any(feature = "http"))]
pub mod http;
pub mod liveness;
pub mod mock;
#[cfg(any(feature = "tcp"))]
pub mod tcp;
#[cfg(any(feature = "websocket"))]
pub mod websocket;

use std::{fmt::Debug, io::Error};

/// An instance of a connection or logical session, which may also support [`Receive`], [`Publish`], or dispatching received events to a [`Callback`]/[`CallbackRef`].
///
/// ## Connecting
///
/// Some implementations may not default to an established state, in which case immediate calls to `publish()` and `receive()` will fail.
/// The [`Session::status`] function provides the current status, which will not return `Established` until all required handshakes are complete.
/// When [`Session::status`] returns [`SessionStatus::Establishing`], you may drive the connection process via the [`Session::drive`] function.
///
/// ## Retrying
///
/// The [`Ok`] result of `publish(..)` and `receive(..)` operations may return [`ReceiveOutcome::Idle`], [`ReceiveOutcome::Buffered`], or [`PublishOutcome::Incomplete`].
/// These outcomes indicate that an operation may need to be retried. See [`ReceiveOutcome`] and [`PublishOutcome`] for more details.
///
/// ## Duty Cycles
///
/// The [`Session::drive`] operation is used to finish connecting and to service reading/writing buffered data and to dispatch callbacks.
/// Most, but not all, [`Session`] implementations will require periodic calls to [`Session::drive`] in order to function.
/// Implementations that do not require calls to [`Session::drive`] will no-op when it is called.
///
/// ## Publishing
///
/// Session impls that can publish data will implement [`Publish`].
///
/// ## Receiving
///
/// Session impls that can receive data via polling implement [`Receive`].
/// Impls that receive data via callbacks will accept a [`Callback`] or [`CallbackRef`] as input.
///
/// For cross-compatibilty between [`Receive`] and [`Callback`]/[`CallbackRef`] paradiagms, see the [`callback`] module.
/// - [`callback::CallbackQueue`] impls [`Callback`]
pub trait Session: Debug {
    /// Check the current session status.
    ///
    /// If this returns [`SessionStatus::Establishing`], use [`Session::drive`] to progress the connection process.
    fn status(&self) -> SessionStatus;

    /// Force the session to move to a [`SessionStatus::Terminated`] state immediately, performing any necessary immediately graceful close actions as appropriate.
    ///
    /// All subsequent calls to status will return [`SessionStatus::Terminated`] immediately after this function is called.
    fn close(&mut self);

    /// Some implementations will internally buffer payloads or require a duty cycle to drive callbacks.
    /// Those implementations will require `drive(..)` to be called continuously to completely publish and/or receive data.
    /// This function will return [`DriveOutcome::Active`] if work was done, indicating to any scheduler that more work may be pending.
    /// When this function returns [`DriveOutcome::Idle`], only then should it indicate to a scheduler that yielding or idling is appropriate.
    fn drive(&mut self) -> Result<DriveOutcome, Error>;
}

/// Returned by the [`Session::status`] function, providing the current connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionStatus {
    /// Session attempting to connect, handshake, or otherwise establish, and will move to `Established` or `Terminated` as [`Session::drive`] is called.
    Establishing,
    /// Session is currently established, and will move `Terminated` when an unrecoverable error is encountered
    Established,
    /// Session terminal state, connection has been closed
    Terminated,
}

/// Returned by the [`Session::drive`] function, providing the result of the drive operation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriveOutcome {
    /// The drive operation resulted in work being done, which means the user should attempt to call [`Session::drive`] again as soon as possible.
    Active,
    /// The drive operation did not result in any work being done, which means the user may decide to yield or backoff.
    Idle,
}

/// A [`Session`] implementation that can receive payloads via polling.
pub trait Receive: Session {
    /// The type returned by the `receive(..)` function.
    type ReceivePayload<'a>
    where
        Self: 'a;

    /// Attempt to receive a `payload` from the session.
    /// This will return [`ReceiveOutcome::Payload`] when data has been received.
    /// [`ReceiveOutcome::Buffered`] can be used to report that work was completed, but data is not ready.
    /// This means that only [`ReceiveOutcome::None`] should be used to indicate to a scheduler that yielding or idling is appropriate.
    fn receive<'a>(&'a mut self) -> Result<ReceiveOutcome<Self::ReceivePayload<'a>>, Error>;
}

/// Returned by the [`Receive::receive`] function, providing the outcome or information about the receive action.
///
/// The generic type `T` will match the cooresponding [`Receive::ReceivePayload`].
pub enum ReceiveOutcome<T> {
    /// Contains a reference to payload received from the [`Receive::receive`] action.
    Payload(T),

    /// Data was buffered. This means a partial payload was received, but could not be returned as complete `Data`.
    Buffered,

    /// No work was done. This is useful to signal to a scheduler or idle strategy that it may be time to yield.
    Idle,
}
impl<T: Debug> Debug for ReceiveOutcome<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReceiveOutcome::Payload(x) => f.write_str(&format!("ReceiveOutcome::Payload({x:?})")),
            ReceiveOutcome::Buffered => f.write_str("ReceiveOutcome::Buffered"),
            ReceiveOutcome::Idle => f.write_str("ReceiveOutcome::Idle"),
        }
    }
}
impl<T: Clone> Clone for ReceiveOutcome<T> {
    fn clone(&self) -> Self {
        match self {
            ReceiveOutcome::Payload(x) => ReceiveOutcome::Payload(x.clone()),
            ReceiveOutcome::Buffered => ReceiveOutcome::Buffered,
            ReceiveOutcome::Idle => ReceiveOutcome::Idle,
        }
    }
}

/// A [`Session`] implementation that can publish payloads.
pub trait Publish: Session {
    /// The type given to the `publish(..)` function.
    type PublishPayload<'a>
    where
        Self: 'a;

    /// Write the given `payload` to the session.
    ///
    /// This will return [`PublishOutcome::Incomplete`] if the publish is not immediately completed fully,
    /// in which case `T` of `Incomplete(T)` data must be retried.
    ///
    /// Note that it is possible for some implementations that a publish is partially complete, so you must
    /// re-attempt the data encapsulated by `Incomplete`, not the data originally passed into the function.
    /// This guidance can only be ignored when you are not writing generic code and you know that your
    /// [`Publish`] impl is "all-or-none".
    fn publish<'a>(
        &mut self,
        payload: Self::PublishPayload<'a>,
    ) -> Result<PublishOutcome<Self::PublishPayload<'a>>, Error>;
}

/// A [`Publish`] implementation that exposes a blocking flush operation.
pub trait Flush: Publish {
    /// Flush all pending publish data, blocking until completion.
    fn flush(&mut self) -> Result<(), Error>;
}

/// Returned by the [`Publish::publish`] function, providing the outcome of the publish action.
///
/// The generic type `T` will match the cooresponding [`Publish::PublishPayload`].
pub enum PublishOutcome<T> {
    /// The publish action completed fully
    Published,

    /// The publish action was not performed or was partially performed.
    ///
    /// The returned reference must be passed back into the [`Publish::publish`] function for the publish action to complete.
    /// Whether or not the returned reference may consist of partial data depends on the [`Session`] implementation.
    ///
    /// If you are looking for a general retry pattern, it is **always** safe to finish the publish by passing this returned
    /// reference back into the `publish` function for another attempt, but it is only **sometimes** appropriate to return the entire
    /// original publish reference into the `publish` function for a second attempt.
    Incomplete(T),
}
impl<T: Debug> Debug for PublishOutcome<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishOutcome::Published => f.write_str("PublishOutcome::Published"),
            PublishOutcome::Incomplete(x) => {
                f.write_str(&format!("PublishOutcome::Incomplete({x:?})"))
            }
        }
    }
}
impl<T: Clone> Clone for PublishOutcome<T> {
    fn clone(&self) -> Self {
        match self {
            PublishOutcome::Published => PublishOutcome::Published,
            PublishOutcome::Incomplete(x) => PublishOutcome::Incomplete(x.clone()),
        }
    }
}

/// Used by push-oriented receivers to handle moved payloads as they are received.
///
/// See the [`compat`] module for [`Receive`] compatibility
pub trait Callback<T> {
    fn callback(&mut self, payload: T);
}

/// Used by push-oriented receivers to handle payload references as they are received.
///
/// See the [`compat`] module for [`Receive`] compatibility
pub trait CallbackRef<T: ?Sized> {
    fn callback_ref(&mut self, payload: &T);
}
