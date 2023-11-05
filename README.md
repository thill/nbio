# nbio

## Description

This crate aims to make bi-directional, nonblocking I/O easier to reason about by using patterns that extend beyond dealing directly
with raw bytes, the [`std::io::Read`] and [`std::io::Write`] traits, and [`std::io::ErrorKind::WouldBlock`] errors.
Since this crate's main focus is nonblocking I/O, all [`Session`] implementations provided by this crate are non-blocking by default.

## Sessions

The core [`Session`] API utilizes associated types to express nonblocking read and write operations.
While the [`tcp`] module provides a [`Session`] implementation that provides unframed non-blocking binary IO operations,
other [`Session`] impls are able to provide significantly more functionality using the same non-blocking patterns.

## Associated Types

Sessions operate on implementation-specific [`Session::ReadData`] and [`Session::WriteData`] types.
Instead of populating a mutable buffer provided by the user a-la [`std::io::Read`] operations, a reference to a received event is returned.
This allows [`Session`] implementations to perform internal buffering, framing, and serialization to support implementation-specific types.

## Errors

The philosophy of this crate is that an [`Err`] should always represent a transport or protocol-level error.
An [`Err`] should not be returned by a function as a condition that should be handled during **normal** branching logic.
As a result, instead of forcing you to handle [`std::io::ErrorKind::WouldBlock`] everywhere you deal with nonblocking code,
this crate will indicate partial read/write operations using [`ReadStatus::None`], [`ReadStatus::Buffered`], and [`WriteStatus::Pending`]
as [`Result::Ok`].

## Non-Blocking Examples

### Streaming TCP

The following example shows how to use streaming TCP to send and receive a traditional stream of bytes.

```rust
use nbio::{ReadStatus, Session, WriteStatus};
use nbio::tcp::StreamingTcpSession;

// establish connection
let mut client = StreamingTcpSession::connect("192.168.123.456:54321").unwrap();

// send some bytes until completion
let mut remaining_write = "hello world!".as_bytes();
while let WriteStatus::Pending(pending) = client.write(remaining_write).unwrap() {
    remaining_write = pending;
    client.drive().unwrap();
}

// print received bytes
loop {
    if let ReadStatus::Data(data) = client.read().unwrap() {
        println!("received: {data:?}");
    }
}
```

### Framing TCP

The following example shows how to [`frame`] messages over TCP to send and receive payloads framed with a preceeding u64 length field.
Notice how it is almost identical to the code above, except it guarantees that read slices are always identical to their corresponding write slices.

```rust
use nbio::{ReadStatus, Session, WriteStatus};
use nbio::tcp::StreamingTcpSession;
use nbio::frame::{FramingSession, U64FramingStrategy};

// establish connection wrapped in a framing session
let client = StreamingTcpSession::connect("192.168.123.456:54321").unwrap();
let mut client = FramingSession::new(client, U64FramingStrategy::new(), 4096);

// send some bytes until completion
let mut remaining_write = "hello world!".as_bytes();
while let WriteStatus::Pending(pending) = client.write(remaining_write).unwrap() {
    remaining_write = pending;
    client.drive().unwrap();
}

// print received bytes
loop {
    if let ReadStatus::Data(data) = client.read().unwrap() {
        println!("received: {data:?}");
    }
}
```

### HTTP Request/Response

The following example shows how to use the [`http`] module to drive an HTTP 1.x request/response using the same non-blocking model.
Notice how the primitives of driving a buffered write to completion and receiving a framed response is the same as any other framed session.
In fact, the `conn` returned by `client.request(..)` is simply a [`frame::FramingSession`] that utilizes a [`http::Http1FramingStrategy`].

```rust
use http::Request;
use nbio::{Session, ReadStatus};
use nbio::http::HttpClient;
use tcp_stream::OwnedTLSConfig;

// create the client and make the request
let mut client = HttpClient::new(OwnedTLSConfig::default());
let mut conn = client
    .request(Request::get("http://icanhazip.com").body(()).unwrap())
    .unwrap();

// drive and read the conn until a full response is received
loop {
    conn.drive().unwrap();
    if let ReadStatus::Data(r) = conn.read().unwrap() {
        // validate the response
        println!("Response Body: {}", String::from_utf8_lossy(r.body()));
        break;
    }
}
```

License: MIT OR Apache-2.0
