//! Provides a TCP [`Session`] implementation and simple [`TcpServer`]

use std::{
    fmt::Debug,
    io::{Error, ErrorKind, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

use tcp_stream::TcpStream;

use crate::{
    dns::{AddrResolutionOutcome, AddrResolver, AddrResolverProvider, STD_NAME_RESOLVER_PROVIDER},
    tls::{IntoTls, IntoTlsOutcome, TlsConnector},
    DriveOutcome, Flush, Publish, PublishOutcome, Receive, ReceiveOutcome, Session, SessionStatus,
};

/// Internal state machine of a TCP connection
enum TcpConnection {
    AddressResolution(Box<dyn AddrResolver>, Option<String>),
    Initializing(mio::net::TcpStream, mio::Poll, mio::Events, Option<String>),
    Connecting(TcpStream),
    IntoTls(IntoTls),
    Connected(TcpStream),
}
impl Debug for TcpConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::AddressResolution(_, _) => "AddressResolution",
            Self::Initializing(_, _, _, _) => "Initializing",
            Self::Connecting(_) => "Connecting",
            Self::IntoTls(_) => "IntoTls",
            Self::Connected(_) => "Connected",
        };
        f.write_str(s)
    }
}

/// A [`Session`] that can [`Publish`] and [`Receive`] that encapsulates a [`TcpStream`].
///
/// This implementation does not provide any framing guarantees.
/// Buffers will be returned as they are read from the underlying sockets.
/// Writes may be partially completed, with the remaining slice returned as [`PublishOutcome::Incomplete`].
///
/// A plain TCP session can be converted to a TLS session by initiating a TLS handshake via [`TcpSession::into_tls`].
/// The TLS handshake is considered part of the connection process and will be driven to completion by calling the [`Session::drive`] function.
/// While a TLS handshake is in progress, calls to [`Session::status`] will return [`SessionStatus::Establishing`] and calls to `read` and `write` will fail.
pub struct TcpSession {
    read_buffer: Vec<u8>,
    connection: Option<TcpConnection>,
    tls_connector: Option<Arc<TlsConnector>>,
}
impl TcpSession {
    /// Create a TcpSession that wraps an existing [`TcpStream`]
    ///
    /// You may wish to use the `connect(..)` function, which will start a non-blocking connection request.
    ///
    /// Create a new TcpSession with the given stream and a read buffer capacity of 4096.
    ///
    /// ```no_compile
    /// let session = TcpSession::new(my_stream);
    /// ````
    pub fn new<I: Into<TcpStream>>(stream: I) -> Result<Self, Error> {
        let stream = stream.into();
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        let mut read_buffer = Vec::new();
        read_buffer.resize(4096, 0);
        Ok(Self {
            connection: if stream.is_connected() {
                Some(TcpConnection::Connected(stream))
            } else {
                Some(TcpConnection::Connecting(stream))
            },
            read_buffer,
            tls_connector: None,
        })
    }

    /// Set the [`TlsConnector`] to use for a TLS handshake.
    pub fn tls_connector(mut self, tls_connector: Arc<TlsConnector>) -> Self {
        self.tls_connector = Some(tls_connector);
        self
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

    fn addr_to_stream(addrs: Vec<SocketAddr>) -> Result<(mio::net::TcpStream, mio::Poll), Error> {
        let mut stream = None;
        let mut err = None;
        for addr in addrs {
            match mio::net::TcpStream::connect(addr) {
                Ok(x) => stream = Some(x),
                Err(x) => err = Some(x),
            }
        }
        let mut stream = match stream {
            Some(x) => x,
            None => match err {
                Some(err) => return Err(err),
                None => return Err(Error::new(ErrorKind::Other, "could not connect to addr")),
            },
        };
        let poll = mio::Poll::new()?;
        poll.registry()
            .register(&mut stream, mio::Token(0), mio::Interest::WRITABLE)?;

        Ok((stream, poll))
    }

    /// Connect to the given socket address.
    ///
    /// This will start connecting using mio, then will transition over to TcpStream by transferring the raw FD or socket.
    /// If `name_resolver_provider` is None, [`crate::dns::StdAddrResolverProvider`] will be used.
    pub fn connect<S: Into<String>>(
        addr: S,
        name_resolver_provider: Option<&dyn AddrResolverProvider>,
        tls_connector: Option<Arc<TlsConnector>>,
    ) -> Result<Self, Error> {
        let name_resolver_provider = name_resolver_provider.unwrap_or(&STD_NAME_RESOLVER_PROVIDER);
        let mut read_buffer = Vec::new();
        read_buffer.resize(4096, 0);
        Ok(Self {
            connection: Some(TcpConnection::AddressResolution(
                name_resolver_provider.start(addr.into()),
                None,
            )),
            read_buffer,
            tls_connector,
        })
    }

    /// Start the TLS handshake.
    ///
    /// While the TLS handshake is in progress, [`Session::status`] will return [`SessionStatus::Connecting`].
    /// The TLS handshake can be driven to completion by calling the [`Session::drive`] function
    pub fn into_tls(mut self, domain: &str) -> Result<Self, Error> {
        let stream = match self.connection.take() {
            Some(TcpConnection::Initializing(stream, poll, events, None)) => {
                self.connection = Some(TcpConnection::Initializing(
                    stream,
                    poll,
                    events,
                    Some(domain.to_owned()),
                ));
                return Ok(self);
            }
            Some(TcpConnection::Initializing(_, _, _, Some(_))) => {
                return Err(Error::new(
                    ErrorKind::NotConnected,
                    "stream already initialized for TLS",
                ))
            }
            Some(TcpConnection::Connecting(x)) => x,
            Some(TcpConnection::Connected(x)) => x,
            Some(TcpConnection::IntoTls(_)) => {
                return Err(Error::new(ErrorKind::Other, "stream already mid-handshake"))
            }
            Some(TcpConnection::AddressResolution(x, _)) => {
                self.connection =
                    Some(TcpConnection::AddressResolution(x, Some(domain.to_owned())));
                return Ok(self);
            }
            None => return Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        };
        let into_tls = match self.tls_connector.as_ref() {
            Some(x) => x.start(stream, domain)?,
            None => TlsConnector::try_default()?.start(stream, domain)?,
        };
        self.connection = Some(TcpConnection::IntoTls(into_tls));
        Ok(self)
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
            Some(TcpConnection::Initializing(_, _, _, _)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream is initializing",
            )),
            Some(TcpConnection::Connecting(x)) => Ok(x),
            Some(TcpConnection::Connected(x)) => Ok(x),
            Some(TcpConnection::IntoTls(_)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream is mid-handshake",
            )),
            Some(TcpConnection::AddressResolution(_, _)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream in address resolution",
            )),
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        }
    }
}
impl Session for TcpSession {
    fn status(&self) -> SessionStatus {
        match &self.connection {
            None => SessionStatus::Terminated,
            Some(TcpConnection::Connected(_)) => SessionStatus::Established,
            Some(TcpConnection::Connecting(_))
            | Some(TcpConnection::IntoTls(_))
            | Some(TcpConnection::Initializing(_, _, _, _))
            | Some(TcpConnection::AddressResolution(_, _)) => SessionStatus::Establishing,
        }
    }

    fn drive(&mut self) -> Result<DriveOutcome, Error> {
        match self.connection.take() {
            Some(TcpConnection::AddressResolution(mut x, tls)) => match x.poll()? {
                AddrResolutionOutcome::Idle => {
                    self.connection = Some(TcpConnection::AddressResolution(x, tls));
                    Ok(DriveOutcome::Idle)
                }
                AddrResolutionOutcome::Active => {
                    self.connection = Some(TcpConnection::AddressResolution(x, tls));
                    Ok(DriveOutcome::Active)
                }
                AddrResolutionOutcome::Resolved(addrs) => {
                    let (stream, poll) = Self::addr_to_stream(addrs)?;
                    let events = mio::Events::with_capacity(1);
                    self.connection = Some(TcpConnection::Initializing(stream, poll, events, tls));
                    Ok(DriveOutcome::Active)
                }
            },

            Some(TcpConnection::Connected(x)) => {
                self.connection = Some(TcpConnection::Connected(x));
                Ok(DriveOutcome::Idle)
            }

            Some(TcpConnection::Initializing(stream, mut poll, mut events, tls)) => {
                poll.poll(&mut events, Some(Duration::ZERO))?;
                if let Ok(Some(err)) | Err(err) = stream.take_error() {
                    return Err(err);
                }
                match stream.peer_addr() {
                    Ok(..) => {
                        // connected
                        let stream: TcpStream = unsafe { into_tcpstream(stream) };
                        stream.set_nonblocking(true)?;
                        stream.set_nodelay(true)?;
                        match tls {
                            None => self.connection = Some(TcpConnection::Connected(stream)),
                            Some(domain) => {
                                let into_tls = match self.tls_connector.as_ref() {
                                    Some(x) => x.start(stream, &domain)?,
                                    None => TlsConnector::try_default()?.start(stream, &domain)?,
                                };
                                self.connection = Some(TcpConnection::IntoTls(into_tls));
                            }
                        }
                        Ok(DriveOutcome::Active)
                    }
                    Err(err) => {
                        // `NotConnected`/`ENOTCONN` => still connecting
                        // `ECONNREFUSED` => failed
                        if err.kind() == ErrorKind::NotConnected
                            || err.raw_os_error() == Some(libc::EINPROGRESS)
                        {
                            self.connection =
                                Some(TcpConnection::Initializing(stream, poll, events, tls));
                            Ok(DriveOutcome::Idle)
                        } else {
                            Err(err)
                        }
                    }
                }
            }
            Some(TcpConnection::Connecting(mut x)) => {
                if x.try_connect()? {
                    self.connection = Some(TcpConnection::Connected(x));
                    Ok(DriveOutcome::Active)
                } else {
                    self.connection = Some(TcpConnection::Connecting(x));
                    Ok(DriveOutcome::Idle)
                }
            }
            Some(TcpConnection::IntoTls(mut x)) => match x.poll()? {
                IntoTlsOutcome::Finished(x) => {
                    self.connection = Some(TcpConnection::Connected(x));
                    Ok(DriveOutcome::Active)
                }
                IntoTlsOutcome::Active => {
                    self.connection = Some(TcpConnection::IntoTls(x));
                    Ok(DriveOutcome::Active)
                }
                IntoTlsOutcome::Idle => {
                    self.connection = Some(TcpConnection::IntoTls(x));
                    Ok(DriveOutcome::Idle)
                }
            },
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        }
    }
}
impl Publish for TcpSession {
    type PublishPayload<'a> = &'a [u8];

    fn publish<'a>(
        &mut self,
        data: Self::PublishPayload<'a>,
    ) -> Result<PublishOutcome<Self::PublishPayload<'a>>, Error> {
        // this impl's drive is only used for establishing a connection, so it does not need to be called here
        let stream = match self.connection.as_mut() {
            Some(TcpConnection::Connected(x)) => Ok(x),
            Some(TcpConnection::AddressResolution(_, _)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream is resolving addresses",
            )),
            Some(TcpConnection::Initializing(_, _, _, _)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream is initializing",
            )),
            Some(TcpConnection::Connecting(_)) => {
                Err(Error::new(ErrorKind::NotConnected, "stream is connecting"))
            }
            Some(TcpConnection::IntoTls(_)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream is mid-handshake",
            )),
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        }?;
        if data.is_empty() {
            // nothing to write, nothing to do
            return Ok(PublishOutcome::Published);
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
            Ok(PublishOutcome::Published)
        } else {
            Ok(PublishOutcome::Incomplete(&data[wrote..]))
        }
    }
}
impl Flush for TcpSession {
    fn flush(&mut self) -> Result<(), Error> {
        let stream = match self.connection.as_mut() {
            Some(TcpConnection::Connected(x)) => Ok(x),
            Some(TcpConnection::Initializing(_, _, _, _)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream is initializing",
            )),
            Some(TcpConnection::Connecting(_)) => {
                Err(Error::new(ErrorKind::NotConnected, "stream is connecting"))
            }
            Some(TcpConnection::AddressResolution(_, _)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream in address resolution",
            )),
            Some(TcpConnection::IntoTls(_)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream is mid-handshake",
            )),
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        }?;
        stream.flush()
    }
}
impl Receive for TcpSession {
    type ReceivePayload<'a> = &'a [u8];
    fn receive<'a>(&'a mut self) -> Result<ReceiveOutcome<Self::ReceivePayload<'a>>, Error> {
        self.drive()?;
        let stream = match self.connection.as_mut() {
            Some(TcpConnection::Connected(x)) => Ok(x),
            Some(TcpConnection::Initializing(_, _, _, _)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream is initializing",
            )),
            Some(TcpConnection::AddressResolution(_, _)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream in address resolution",
            )),
            Some(TcpConnection::Connecting(_)) => {
                Err(Error::new(ErrorKind::NotConnected, "stream is connecting"))
            }
            Some(TcpConnection::IntoTls(_)) => Err(Error::new(
                ErrorKind::NotConnected,
                "stream is mid-handshake",
            )),
            None => Err(Error::new(ErrorKind::NotConnected, "stream not connected")),
        }?;
        let read = match stream.read(self.read_buffer.as_mut_slice()) {
            Ok(x) => Some(x),
            Err(err) => match err.kind() {
                ErrorKind::WouldBlock => None,
                _ => {
                    self.connection = None;
                    return Err(err.into());
                }
            },
        };
        match read {
            None => Ok(ReceiveOutcome::Idle),
            Some(0) => {
                // eof
                Err(Error::new(ErrorKind::UnexpectedEof, "stream is eof"))
            }
            Some(read) => Ok(ReceiveOutcome::Payload(
                &mut self.read_buffer.as_mut_slice()[..read],
            )),
        }
    }
}
impl Debug for TcpSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TcpSession")
            .field("connection", &self.connection)
            .finish()
    }
}
impl Drop for TcpSession {
    fn drop(&mut self) {
        if let Some(mut connection) = self.connection.take() {
            match &mut connection {
                TcpConnection::Initializing(stream, _, _, _) => {
                    stream.shutdown(Shutdown::Both).ok()
                }
                TcpConnection::Connecting(stream) | TcpConnection::Connected(stream) => {
                    stream.shutdown(Shutdown::Both).ok()
                }
                TcpConnection::AddressResolution(_, _) => Some(()),
                TcpConnection::IntoTls(_) => None,
            };
        }
    }
}
/// Extract the [`tcp_stream::TcpStream`] from the [`TcpSession`] if and only if it is in a connected state.
///
/// Note: support for this conversion may be dropped or put behind a feature flag in a future release if we
/// move away from [`tcp_stream`] as our internal [`TcpStream`] impl.
impl From<TcpSession> for Option<tcp_stream::TcpStream> {
    fn from(mut value: TcpSession) -> Self {
        match value.connection.take() {
            Some(TcpConnection::Connected(x)) => Some(x),
            _ => None,
        }
    }
}

#[cfg(unix)]
unsafe fn into_tcpstream(stream: mio::net::TcpStream) -> TcpStream {
    use std::os::fd::{FromRawFd, IntoRawFd};
    TcpStream::from_raw_fd(stream.into_raw_fd())
}

#[cfg(windows)]
unsafe fn into_tcpstream(stream: mio::net::TcpStream) -> TcpStream {
    use std::os::windows::io::{FromRawSocket, IntoRawSocket};
    TcpStream::from_raw_socket(stream.into_raw_socket())
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
