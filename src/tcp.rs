//! This module provides a TCP [`Session`] implementation and simple [`TcpServer`].

use std::{
    io::{Error, ErrorKind, Read, Write},
    net::{SocketAddr, TcpListener, ToSocketAddrs},
    time::Duration,
};

use tcp_stream::{HandshakeError, MidHandshakeTlsStream, TLSConfig, TcpStream};

use crate::{ConnectionStatus, ReadStatus, Session, WriteStatus};

/// Internal state machine of a TCP connection
enum TcpConnection {
    Connecting(TcpStream),
    MidTlsHandshake(MidHandshakeTlsStream),
    Connected(TcpStream),
}

/// A [`Session`] that encapsulates a [`TcpStream`].
///
/// This implementation does not provide any framing guarantees.
/// Buffers will be returned as they are read from the underlying sockets.
/// Writes may be partially completed, with the remaining slice returned as [`WriteStatus::Pending`].
///
/// A plain TCP session can be converted to a TLS session by initiating a TLS handshake via [`TcpSession::into_tls`].
/// The TLS handshake is considered part of the connection process and will be driven to completion by calling the [`Session::drive`] function.
/// While a TLS handshake is in progress, calls to [`Session::status`] will return [`ConnectionStatus::Connecting`] and calls to `read` and `write` will fail.
pub struct TcpSession {
    read_buffer: Vec<u8>,
    connection: Option<TcpConnection>,
}
impl TcpSession {
    /// You may wish to use the more convenient `connect(..)` function.
    ///
    /// Create a new TcpSession with the given stream and a read buffer capacity of 4096.
    ///
    /// ```no_compile
    /// let session = TcpSession::new(my_stream);
    /// ````
    pub fn new(stream: TcpStream) -> Result<Self, Error> {
        stream.set_nonblocking(true)?;
        let mut read_buffer = Vec::new();
        read_buffer.resize(4096, 0);
        Ok(Self {
            connection: if stream.is_connected() {
                Some(TcpConnection::Connected(stream))
            } else {
                Some(TcpConnection::Connecting(stream))
            },
            read_buffer,
        })
    }

    /// Set the underlying read buffer capacity, which must be greater than or equal to the current read buffer length
    pub fn with_read_buffer_capacity(mut self, read_buffer_capacity: usize) -> Result<Self, Error> {
        if read_buffer_capacity < self.read_buffer.len() {
            return Err(Error::new(
                ErrorKind::Other,
                "new read buffer capacity must be greater than or equal to than current length",
            ));
        }
        self.read_buffer.resize(read_buffer_capacity, 0);
        Ok(self)
    }

    /// Connect to the given socket address in nonblocking mode.
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self, Error> {
        Self::new(TcpStream::Plain(std::net::TcpStream::connect(addr)?, true))
    }

    /// Start the TLS handshake.
    ///
    /// While the TLS handshake is in progress, [`Session::status`] will return [`ConnectionStatus::Connecting`].
    /// The TLS handshake can be driven to completion by calling the [`Session::drive`] function
    pub fn into_tls(mut self, domain: &str, config: TLSConfig<'_, '_, '_>) -> Result<Self, Error> {
        let stream = match self.connection.take() {
            Some(TcpConnection::Connecting(x)) => x,
            Some(TcpConnection::Connected(x)) => x,
            Some(TcpConnection::MidTlsHandshake(_)) => {
                return Err(Error::new(
                    ErrorKind::NotConnected,
                    "stream already mid-handshake",
                ))
            }
            None => return Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        };
        match stream.into_tls(domain, config) {
            Ok(x) => {
                self.connection = Some(TcpConnection::Connected(x));
            }
            Err(err) => match err {
                HandshakeError::WouldBlock(x) => {
                    self.connection = Some(TcpConnection::MidTlsHandshake(x));
                }
                HandshakeError::Failure(err) => return Err(err),
            },
        }
        Ok(self)
    }

    /// Set nodelay on the underlying stream
    pub fn set_nodelay(&self, nodelay: bool) -> Result<(), Error> {
        self.stream()?.set_nodelay(nodelay)
    }

    /// Set read_timeout on the underlying stream
    pub fn set_read_timeout(&self, read_timeout: Option<Duration>) -> Result<(), Error> {
        self.stream()?.set_read_timeout(read_timeout)
    }

    /// Set ttl on the underlying stream
    pub fn set_ttl(&self, ttl: u32) -> Result<(), Error> {
        self.stream()?.set_ttl(ttl)
    }

    /// Set write_timeout on the underlying stream
    pub fn set_write_timeout(&self, write_timeout: Option<Duration>) -> Result<(), Error> {
        self.stream()?.set_write_timeout(write_timeout)
    }

    /// Set nodelay on the underlying stream using a builder pattern
    pub fn with_nodelay(self, nodelay: bool) -> Result<Self, Error> {
        self.set_nodelay(nodelay)?;
        Ok(self)
    }

    /// Set read_timeout on the underlying stream using a builder pattern
    pub fn with_read_timeout(self, read_timeout: Option<Duration>) -> Result<Self, Error> {
        self.set_read_timeout(read_timeout)?;
        Ok(self)
    }

    /// Set ttl on the underlying stream using a builder pattern
    pub fn with_ttl(self, ttl: u32) -> Result<Self, Error> {
        self.set_ttl(ttl)?;
        Ok(self)
    }

    /// Set write_timeout on the underlying stream using a builder pattern
    pub fn with_write_timeout(self, write_timeout: Option<Duration>) -> Result<Self, Error> {
        self.set_write_timeout(write_timeout)?;
        Ok(self)
    }

    fn stream<'a>(&'a self) -> Result<&'a TcpStream, Error> {
        match self.connection.as_ref() {
            Some(TcpConnection::Connecting(x)) => Ok(x),
            Some(TcpConnection::Connected(x)) => Ok(x),
            Some(TcpConnection::MidTlsHandshake(_)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream is mid-handshake",
            )),
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        }
    }
}
impl Session for TcpSession {
    type ReadData<'a> = &'a [u8];
    type WriteData<'a> = &'a [u8];

    fn status(&self) -> ConnectionStatus {
        match &self.connection {
            None => ConnectionStatus::Closed,
            Some(TcpConnection::Connected(_)) => ConnectionStatus::Connected,
            Some(TcpConnection::Connecting(_)) | Some(TcpConnection::MidTlsHandshake(_)) => {
                ConnectionStatus::Connecting
            }
        }
    }

    fn close(&mut self) {
        self.connection = None
    }

    fn drive(&mut self) -> Result<bool, Error> {
        match self.connection.take() {
            Some(TcpConnection::Connected(x)) => {
                self.connection = Some(TcpConnection::Connected(x));
                Ok(false)
            }
            Some(TcpConnection::Connecting(mut x)) => {
                if x.try_connect()? {
                    self.connection = Some(TcpConnection::Connected(x));
                    Ok(true)
                } else {
                    self.connection = Some(TcpConnection::Connecting(x));
                    Ok(false)
                }
            }
            Some(TcpConnection::MidTlsHandshake(x)) => match x.handshake() {
                Ok(x) => {
                    self.connection = Some(TcpConnection::Connected(x));
                    Ok(true)
                }
                Err(err) => match err {
                    HandshakeError::WouldBlock(x) => {
                        self.connection = Some(TcpConnection::MidTlsHandshake(x));
                        Ok(false)
                    }
                    HandshakeError::Failure(err) => {
                        self.connection = None;
                        Err(err)
                    }
                },
            },
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        }
    }

    fn write<'a>(
        &mut self,
        data: Self::WriteData<'a>,
    ) -> Result<WriteStatus<Self::WriteData<'a>>, Error> {
        let stream = match self.connection.as_mut() {
            Some(TcpConnection::Connecting(x)) => x,
            Some(TcpConnection::Connected(x)) => x,
            Some(TcpConnection::MidTlsHandshake(_)) => {
                return Err(Error::new(
                    ErrorKind::NotConnected,
                    "stream is mid-handshake",
                ))
            }
            None => return Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        };
        if data.is_empty() {
            // nothing to write, nothing to do
            return Ok(WriteStatus::Success);
        }
        let wrote = match stream.write(data) {
            Ok(0) => {
                // per rust docs: A return value of 0 typically means that the underlying object is no longer
                // able to accept bytes and will likely not be able to in the future as well, or that the buffer
                // provided is empty.
                self.connection = None;
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "stream underlying write returned 0 instead of WouldBlock",
                ));
            }
            Ok(x) => x,
            Err(err) => match err.kind() {
                ErrorKind::WouldBlock => 0,
                _ => {
                    self.connection = None;
                    return Err(err.into());
                }
            },
        };
        if wrote == data.len() {
            Ok(WriteStatus::Success)
        } else {
            Ok(WriteStatus::Pending(&data[wrote..]))
        }
    }

    fn read<'a>(&'a mut self) -> Result<ReadStatus<Self::ReadData<'a>>, Error> {
        let stream = match self.connection.as_mut() {
            Some(TcpConnection::Connecting(x)) => x,
            Some(TcpConnection::Connected(x)) => x,
            Some(TcpConnection::MidTlsHandshake(_)) => {
                return Err(Error::new(
                    ErrorKind::NotConnected,
                    "stream is mid-handshake",
                ))
            }
            None => return Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        };
        let read = match stream.read(self.read_buffer.as_mut_slice()) {
            Ok(x) => x,
            Err(err) => match err.kind() {
                ErrorKind::WouldBlock => 0,
                _ => {
                    self.connection = None;
                    return Err(err.into());
                }
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
        let stream = match self.connection.as_mut() {
            Some(TcpConnection::Connected(x)) => x,
            Some(TcpConnection::Connecting(_)) => {
                return Err(Error::new(ErrorKind::NotConnected, "stream is connecting"))
            }
            Some(TcpConnection::MidTlsHandshake(_)) => {
                return Err(Error::new(
                    ErrorKind::NotConnected,
                    "stream is mid-handshake",
                ))
            }
            None => return Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        };
        stream.flush()
    }
}

/// A TcpServer, which produces connected, nonblocking [`TcpSession`] on calling `accept`.
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
    pub fn accept(&self) -> Result<Option<(TcpSession, SocketAddr)>, Error> {
        let (stream, addr) = match self.listener.accept() {
            Ok(v) => v,
            Err(err) => match err.kind() {
                ErrorKind::WouldBlock => return Ok(None),
                _ => return Err(err),
            },
        };
        Ok(Some((
            TcpSession::new(TcpStream::Plain(stream, true))?,
            addr,
        )))
    }
}
