# nbio

## Description

This crate aims to make it easier to reason about uni-directional and bi-directional nonblocking I/O.

This is done using patterns that extend beyond dealing directly with raw bytes, the [`std::io::Read`] and [`std::io::Write`] traits,
and [`std::io::ErrorKind::WouldBlock`] errors. Since this crate's main focus is nonblocking I/O, all [`Session`] implementations provided
by this crate are non-blocking by default.

## Sessions

The core [`Session`] trait encapsulates controlling a single instance of a connection or logical session.
To differentiate with the [`std::io::Read`] and [`std::io::Write`] traits that only deal with raw bytes, this
crate uses [`Publish`] and [`Receive`] terminology, which utilize associated types to handle any payload type.

A [`Session`] impl is typically also either [`Publish`], [`Receive`], or both.
While the [`tcp`] module provides a [`Session`] implementation that provides unframed non-blocking binary IO operations,
other [`Session`] impls are able to provide significantly more functionality using the same non-blocking patterns.

This crate will often use the term `Duplex` to distinguish a [`Session`] that is **both** [`Publish`] and [`Receive`].

## Associated Types

Sessions operate on implementation-specific [`Receive::ReceivePayload`] and [`Publish::PublishPayload`] types.
These types are able to utilize a lifetime `'a`, which is tied to the lifetime of the underlying [`Session`],
providing the ability for implementations to reference internal buffers or queues without copying.

## Errors

The philosophy of this crate is that an [`Err`] should always represent a transport or protocol-level error.
An [`Err`] should not be returned by a function as a condition that should be handled during **normal** branching logic.
As a result, instead of forcing you to handle [`std::io::ErrorKind::WouldBlock`] everywhere you deal with nonblocking code,
this crate will indicate partial receive/publish operations using [`ReceiveOutcome::Idle`], [`ReceiveOutcome::Buffered`],
and [`PublishOutcome::Incomplete`] as [`Result::Ok`].

## Features

The [`Session`] impls in this crate are enabled by certain features.
By default, all features are enabled for rapid prototyping.
In a production codebase, you will likey want to pick and choose your required features.

Feature list:
- `http`
- `tcp`
- `websocket`

## Examples

### Streaming TCP

The following example shows how to use streaming TCP to publish and receive a traditional stream of bytes.

```rust
use nbio::{Publish, PublishOutcome, Receive, ReceiveOutcome, Session};
use nbio::tcp::TcpSession;

// establish connection
let mut client = TcpSession::connect("192.168.123.456:54321").unwrap();

// publish some bytes until completion
let mut pending_publish = "hello world!".as_bytes();
while let PublishOutcome::Incomplete(pending) = client.publish(pending_publish).unwrap() {
    pending_publish = pending;
    client.drive().unwrap();
}

// print received bytes
loop {
    if let ReceiveOutcome::Payload(payload) = client.receive().unwrap() {
        println!("received: {payload:?}");
    }
}
```

### Framing TCP

The following example shows how to [`frame`] messages over TCP to publish and receive payloads framed with a preceeding u64 length field.
Notice how it is almost identical to the code above, except it guarantees that read slices are always identical to their corresponding write slices.

```rust
use nbio::{Publish, PublishOutcome, Receive, ReceiveOutcome, Session};
use nbio::tcp::TcpSession;
use nbio::frame::{FrameDuplex, U64FrameDeserializer, U64FrameSerializer};

// establish connection wrapped in a framing session
let client = TcpSession::connect("192.168.123.456:54321").unwrap();
let mut client = FrameDuplex::new(client, U64FrameDeserializer::new(), U64FrameSerializer::new(), 4096);

// publish some bytes until completion
let mut pending_publish = "hello world!".as_bytes();
while let PublishOutcome::Incomplete(pending) = client.publish(pending_publish).unwrap() {
    pending_publish = pending;
    client.drive().unwrap();
}

// print received bytes
loop {
    if let ReceiveOutcome::Payload(payload) = client.receive().unwrap() {
        println!("received: {payload:?}");
    }
}
```

### HTTP Client

The following example shows how to use the [`http`] module to drive an HTTP 1.x request/response using the same non-blocking model.
Notice how the primitives of driving a buffered write to completion and receiving a framed response is the same as any other framed session.
In fact, the `conn` returned by `client.request(..)` is simply a [`frame::FrameDuplex`] that utilizes a [`http::Http1RequestSerializer`] and
[`http::Http1ResponseDeserializer`].

```rust
use http::Request;
use nbio::{Receive, Session, ReceiveOutcome};
use nbio::http::HttpClient;
use tcp_stream::OwnedTLSConfig;

// create the client and make the request
let mut client = HttpClient::new();
let mut conn = client
    .request(Request::get("http://icanhazip.com").body(()).unwrap())
    .unwrap();

// drive and read the conn until a full response is received
loop {
    conn.drive().unwrap();
    if let ReceiveOutcome::Payload(r) = conn.receive().unwrap() {
        println!("Response Body: {}", String::from_utf8_lossy(r.body()));
        break;
    }
}
```

### WebSocket

The following example sends a message and then receives all subsequent messages from a websocket connection.
Just like the HTTP example, this simply encapsulates [`frame::FrameDuplex`] but utilizes a [`websocket::WebSocketFrameSerializer`]
and [`websocket::WebSocketFrameDeserializer`]. All TLS and WebSocket handshaking is taken care of during the
[`SessionStatus::Establishing`] [`Session::status`] workflow.

```rust
use nbio::{Publish, PublishOutcome, Receive, Session, SessionStatus, ReceiveOutcome};
use nbio::websocket::{Message, WebSocketSession};

// create the client and make the request
let mut session = WebSocketSession::connect("wss://echo.websocket.org/", None).unwrap();
while session.status() == SessionStatus::Establishing {
     session.drive().unwrap();
}

// publish a message
let mut pending_publish = Message::Text("hello world!".into());
while let PublishOutcome::Incomplete(pending) = session.publish(pending_publish).unwrap() {
    pending_publish = pending;
    session.drive().unwrap();
}

// drive and receive messages
loop {
    session.drive().unwrap();
    if let ReceiveOutcome::Payload(r) = session.receive().unwrap() {
        println!("Received: {:?}", r);
        break;
    }
}
```

License: MIT OR Apache-2.0
