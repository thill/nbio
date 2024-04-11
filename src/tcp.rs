//! This module provides a TCP [`Session`] implementation and simple [`TcpServer`].

use std::{
    io::{Error, ErrorKind, Read, Write},
    net::{SocketAddr, TcpListener, ToSocketAddrs},
    time::Duration,
};

use tcp_stream::{HandshakeError, MidHandshakeTlsStream, TLSConfig, TcpStream};

use crate::{ReadStatus, Session, TlsSession, WriteStatus};

/// A [`Session`] that encapsulates a [`TcpStream`].
///
/// This implementation does not provide any framing guarantees.
/// Buffers will be returned as they are read from the underlying sockets.
/// Writes may be partially completed, with the remaining slice returned as [`WriteStatus::Pending`].
///
/// Once a client successfully connects, a plain TCP session can initiale a TLS handshake by calling [`TlsSession::to_tls`].
/// The TLS handshake will be driven to completion by calling the [`Session::drive`] function.
/// While a TLS handshake is in progress, calls to `read` and `write` will not be able to consume or produce data.
pub struct StreamingTcpSession {
    read_buffer: Vec<u8>,
    stream: Option<TcpStream>,
    mid_handshake: Option<MidHandshakeTlsStream>,
    tls_handshake_complete: bool,
    is_server_session: bool,
}
impl StreamingTcpSession {
    /// You may wish to use the more convenient `connect(..)` function.
    ///
    /// Create a new StreamingTcpSession with the given buffer length.
    /// You may the underlying stream with `set_stream` or `with_stream`.
    ///
    /// ```no_compile
    /// let session = StreamingTcpSession::new(4096).with_stream(my_stream);
    /// ````
    pub fn new(read_buffer_len: usize) -> Self {
        let mut read_buffer = Vec::new();
        read_buffer.resize(read_buffer_len, 0);
        Self {
            stream: None,
            mid_handshake: None,
            read_buffer,
            tls_handshake_complete: false,
            is_server_session: false,
        }
    }

    /// Connect to the given socket address in nonblocking mode.
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self, Error> {
        Ok(Self::default()
            .with_stream(TcpStream::Plain(std::net::TcpStream::connect(addr)?, true))
            .with_nonblocking(true)?)
    }

    /// Set the underlying stream
    pub fn set_stream(&mut self, stream: TcpStream) {
        self.stream = Some(stream);
        self.mid_handshake = None;
    }

    /// Set nodelay on the underlying stream
    pub fn set_nodelay(&self, nodelay: bool) -> Result<(), Error> {
        match &self.stream {
            Some(x) => x.set_nodelay(nodelay),
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected").into()),
        }
    }

    /// Set nonblocking on the underlying stream
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), Error> {
        match &self.stream {
            Some(x) => x.set_nonblocking(nonblocking),
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected").into()),
        }
    }

    /// Set read_timeout on the underlying stream
    pub fn set_read_timeout(&self, read_timeout: Option<Duration>) -> Result<(), Error> {
        match &self.stream {
            Some(x) => x.set_read_timeout(read_timeout),
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected").into()),
        }
    }

    /// Set ttl on the underlying stream
    pub fn set_ttl(&self, ttl: u32) -> Result<(), Error> {
        match &self.stream {
            Some(x) => x.set_ttl(ttl),
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected").into()),
        }
    }

    /// Set write_timeout on the underlying stream
    pub fn set_write_timeout(&self, write_timeout: Option<Duration>) -> Result<(), Error> {
        match &self.stream {
            Some(x) => x.set_write_timeout(write_timeout),
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected").into()),
        }
    }

    /// Set the underlying stream using a builder pattern
    pub fn with_stream(mut self, stream: TcpStream) -> Self {
        self.set_stream(stream);
        self
    }

    /// Set nodelay on the underlying stream using a builder pattern
    pub fn with_nodelay(self, nodelay: bool) -> Result<Self, Error> {
        self.set_nodelay(nodelay)?;
        Ok(self)
    }

    /// Set nonblocking on the underlying stream using a builder pattern
    pub fn with_nonblocking(self, nonblocking: bool) -> Result<Self, Error> {
        self.set_nonblocking(nonblocking)?;
        Ok(self)
    }

    /// Set read_timeout on the underlying stream using a builder pattern
    pub fn with_read_timeout(self, read_timeout: Option<Duration>) -> Result<Self, Error> {
        self.set_read_timeout(read_timeout)?;
        Ok(self)
    }

    /// Set nonblocking on the underlying stream using a builder pattern
    pub fn with_ttl(self, ttl: u32) -> Result<Self, Error> {
        self.set_ttl(ttl)?;
        Ok(self)
    }

    /// Set write_timeout on the underlying stream using a builder pattern
    pub fn with_write_timeout(self, write_timeout: Option<Duration>) -> Result<Self, Error> {
        self.set_write_timeout(write_timeout)?;
        Ok(self)
    }

    /// Internal use
    fn with_is_server_session(mut self, is_server_session: bool) -> Self {
        self.is_server_session = is_server_session;
        self
    }
}
impl Default for StreamingTcpSession {
    fn default() -> Self {
        Self::new(4096)
    }
}
impl Session for StreamingTcpSession {
    type ReadData = [u8];
    type WriteData = [u8];

    fn is_connected(&self) -> bool {
        match &self.stream {
            Some(x) => x.is_connected(),
            None => self.mid_handshake.is_some(),
        }
    }

    fn try_connect(&mut self) -> Result<bool, Error> {
        match &mut self.stream {
            Some(x) => x.try_connect(),
            None => {
                if self.mid_handshake.is_some() {
                    Ok(true)
                } else {
                    Err(Error::new(ErrorKind::ConnectionReset, "undefined stream"))
                }
            }
        }
    }

    fn drive(&mut self) -> Result<bool, Error> {
        if self.mid_handshake.is_some() {
            let mid_handshake = match self.mid_handshake.take() {
                Some(x) => x,
                None => return Err(Error::new(ErrorKind::Other, "stream is not mid-handshake")),
            };
            match mid_handshake.handshake() {
                Ok(x) => {
                    self.stream = Some(x);
                    self.tls_handshake_complete = true;
                    Ok(true)
                }
                Err(err) => match err {
                    HandshakeError::WouldBlock(x) => {
                        self.mid_handshake = Some(x);
                        Ok(false)
                    }
                    HandshakeError::Failure(err) => Err(err),
                },
            }
        } else {
            Ok(false)
        }
    }

    fn write<'a>(
        &mut self,
        data: &'a Self::WriteData,
    ) -> Result<WriteStatus<'a, Self::WriteData>, Error> {
        if data.is_empty() {
            // nothing to write, nothing to do
            return Ok(WriteStatus::Success);
        }
        let stream = match &mut self.stream {
            Some(x) => x,
            None => {
                if self.mid_handshake.is_some() {
                    return Ok(WriteStatus::Pending(data));
                } else {
                    return Err(Error::new(ErrorKind::NotConnected, "stream not connected").into());
                }
            }
        };
        let wrote = match stream.write(data) {
            Ok(0) => {
                // per rust docs: A return value of 0 typically means that the underlying object is no longer
                // able to accept bytes and will likely not be able to in the future as well, or that the buffer
                // provided is empty.
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "stream underlying write returned 0 instead of WouldBlock",
                ));
            }
            Ok(x) => x,
            Err(err) => match err.kind() {
                ErrorKind::WouldBlock => 0,
                _ => return Err(err.into()),
            },
        };
        if wrote == data.len() {
            Ok(WriteStatus::Success)
        } else {
            Ok(WriteStatus::Pending(&data[wrote..]))
        }
    }

    fn read<'a>(&'a mut self) -> Result<ReadStatus<'a, Self::ReadData>, Error> {
        let stream = match &mut self.stream {
            Some(x) => x,
            None => {
                if self.mid_handshake.is_some() {
                    return Ok(ReadStatus::None);
                } else {
                    return Err(Error::new(ErrorKind::NotConnected, "stream not connected").into());
                }
            }
        };
        let read = match stream.read(self.read_buffer.as_mut_slice()) {
            Ok(x) => x,
            Err(err) => match err.kind() {
                ErrorKind::WouldBlock => 0,
                _ => return Err(err.into()),
            },
        };
        if read == 0 {
            Ok(ReadStatus::None)
        } else {
            Ok(ReadStatus::Data(
                &mut self.read_buffer.as_mut_slice()[..read],
            ))
        }
    }

    fn flush(&mut self) -> Result<(), Error> {
        match &mut self.stream {
            None => Ok(()),
            Some(stream) => stream.flush(),
        }
    }

    fn close(&mut self) -> Result<(), Error> {
        self.stream = None;
        Ok(())
    }
}
impl TlsSession for StreamingTcpSession {
    fn to_tls(&mut self, domain: &str, config: TLSConfig<'_, '_, '_>) -> Result<(), Error> {
        if self.is_server_session {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "to_tls is only supported for client connections",
            ));
        }
        let stream = match self.stream.take() {
            Some(x) => x,
            None => return Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        };
        match stream.into_tls(domain, config) {
            Ok(x) => {
                self.stream = Some(x);
                self.tls_handshake_complete = true;
                Ok(())
            }
            Err(err) => match err {
                HandshakeError::WouldBlock(x) => {
                    self.mid_handshake = Some(x);
                    Ok(())
                }
                HandshakeError::Failure(err) => Err(err),
            },
        }
    }

    fn is_handshake_complete(&self) -> Result<bool, Error> {
        Ok(self.tls_handshake_complete)
    }
}

/// A TcpServer, which produces connected, nonblocking [`StreamingTcpSession`] on calling `accept`.
pub struct TcpServer {
    listener: TcpListener,
}
impl TcpServer {
    /// Encapsulate the given [`TcpListener`]
    pub fn new(listener: TcpListener) -> Self {
        Self { listener }
    }

    /// Bind to the given socket address in nonblocking mode.
    pub fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self, Error> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Self::new(listener))
    }

    /// Set nonblocking on the listener
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), Error> {
        self.listener.set_nonblocking(nonblocking)
    }

    /// Set ttl on the listener
    pub fn set_ttl(&self, ttl: u32) -> Result<(), Error> {
        self.listener.set_ttl(ttl)
    }

    /// Set nonblocking on the listener using a builder pattern
    pub fn with_nonblocking(self, nonblocking: bool) -> Result<Self, Error> {
        self.set_nonblocking(nonblocking)?;
        Ok(self)
    }

    /// Set ttl on the listener using a builder pattern
    pub fn with_ttl(self, ttl: u32) -> Result<Self, Error> {
        self.set_ttl(ttl)?;
        Ok(self)
    }

    /// Accept a new TCP Session, immediately returning None in nonblocking mode if there are no new sessions.
    pub fn accept(&self) -> Result<Option<(StreamingTcpSession, SocketAddr)>, Error> {
        let (stream, addr) = match self.listener.accept() {
            Ok(v) => v,
            Err(err) => match err.kind() {
                ErrorKind::WouldBlock => return Ok(None),
                _ => return Err(err),
            },
        };
        Ok(Some((
            StreamingTcpSession::default()
                .with_stream(TcpStream::Plain(stream, true))
                .with_is_server_session(true)
                .with_nonblocking(true)?,
            addr,
        )))
    }
}
