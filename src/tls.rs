//! TLS Connector implementations

use std::{
    io::{Error, ErrorKind},
    net::Shutdown,
};

use native_tls::Certificate;
use tcp_stream::{HandshakeError, MidHandshakeTlsStream, TLSConfig, TcpStream};

pub enum TlsConnector {
    Native(NativeTlsConnector),
    Rustls(RustlsConnector),
}

impl TlsConnector {
    pub fn start(&self, tcp_stream: TcpStream, domain: &str) -> Result<IntoTls, Error> {
        match self {
            TlsConnector::Native(connector) => connector.start(tcp_stream, domain),
            TlsConnector::Rustls(connector) => connector.start(tcp_stream, domain),
        }
    }

    pub fn try_default() -> Result<Self, Error> {
        Ok(TlsConnector::Rustls(RustlsConnector {
            connector: tcp_stream::RustlsConnector::new_with_native_certs()?,
        }))
    }
}

/// Provides instances of [`IntoTls`] using native-tls
pub struct NativeTlsConnector {
    connector: tcp_stream::NativeTlsConnector,
}

impl From<tcp_stream::NativeTlsConnector> for TlsConnector {
    fn from(connector: tcp_stream::NativeTlsConnector) -> Self {
        TlsConnector::Native(NativeTlsConnector { connector })
    }
}

impl NativeTlsConnector {
    pub fn new(
        config: TLSConfig<'_, '_, '_>,
        accept_invalid_hostnames: bool,
    ) -> Result<Self, Error> {
        let mut builder = tcp_stream::NativeTlsConnector::builder();
        if accept_invalid_hostnames {
            builder.danger_accept_invalid_hostnames(true);
        }

        if let Some(identity) = config.identity {
            match identity {
                tcp_stream::Identity::PKCS12 { der, password } => builder.identity(
                    native_tls::Identity::from_pkcs12(&der, password)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?,
                ),
                tcp_stream::Identity::PKCS8 { pem, key } => builder.identity(
                    native_tls::Identity::from_pkcs8(&pem, key)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?,
                ),
            };
        }

        if let Some(cert_chain) = config.cert_chain {
            let mut cert_chain = std::io::BufReader::new(cert_chain.as_bytes());
            for cert in rustls_pemfile::read_all(&mut cert_chain) {
                match cert {
                    Ok(rustls_pemfile::Item::X509Certificate(cert)) => {
                        builder.add_root_certificate(
                            Certificate::from_der(&cert[..])
                                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?,
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        return Err(Error::new(ErrorKind::Other, e));
                    }
                }
            }
        }

        let connector = builder
            .build()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        Ok(Self { connector })
    }

    fn start(&self, tcp_stream: TcpStream, domain: &str) -> Result<IntoTls, Error> {
        match tcp_stream.into_native_tls(&self.connector, domain) {
            Ok(x) => Ok(IntoTls {
                state: Some(IntoTlsState::Finished(x)),
            }),
            Err(err) => match err {
                HandshakeError::WouldBlock(x) => Ok(IntoTls {
                    state: Some(IntoTlsState::MidHandshake(x)),
                }),
                HandshakeError::Failure(err) => Err(err),
            },
        }
    }
}

/// Provides instances of [`IntoTls`] using rustls
pub struct RustlsConnector {
    connector: tcp_stream::RustlsConnector,
}

impl From<tcp_stream::RustlsConnector> for TlsConnector {
    fn from(connector: tcp_stream::RustlsConnector) -> Self {
        TlsConnector::Rustls(RustlsConnector { connector })
    }
}

impl RustlsConnector {
    pub fn new(use_native_certs: bool) -> Result<Self, Error> {
        let connector = if use_native_certs {
            tcp_stream::RustlsConnector::new_with_native_certs()?
        } else {
            tcp_stream::RustlsConnector::default()
        };
        Ok(Self { connector })
    }

    fn start(&self, tcp_stream: TcpStream, domain: &str) -> Result<IntoTls, Error> {
        match tcp_stream.into_rustls(&self.connector, domain) {
            Ok(x) => Ok(IntoTls {
                state: Some(IntoTlsState::Finished(x)),
            }),
            Err(err) => match err {
                HandshakeError::WouldBlock(x) => Ok(IntoTls {
                    state: Some(IntoTlsState::MidHandshake(x)),
                }),
                HandshakeError::Failure(err) => Err(err),
            },
        }
    }
}

/// [`IntoTls`] outcome that can be polled to completion.
pub enum IntoTlsOutcome {
    Idle,
    Active,
    Finished(TcpStream),
}

/// A mid-handshake [`TcpStream`] that can be polled to handshake completion.
pub struct IntoTls {
    state: Option<IntoTlsState>,
}

impl IntoTls {
    pub fn poll(&mut self) -> Result<IntoTlsOutcome, Error> {
        match self.state.take() {
            Some(IntoTlsState::MidHandshake(x)) => match x.handshake() {
                Ok(x) => Ok(IntoTlsOutcome::Finished(x)),
                Err(err) => match err {
                    HandshakeError::WouldBlock(x) => {
                        self.state = Some(IntoTlsState::MidHandshake(x));
                        Ok(IntoTlsOutcome::Idle)
                    }
                    HandshakeError::Failure(err) => Err(err),
                },
            },
            Some(IntoTlsState::Finished(x)) => Ok(IntoTlsOutcome::Finished(x)),
            None => Err(Error::new(ErrorKind::Other, "done!")),
        }
    }
}

impl Drop for IntoTls {
    fn drop(&mut self) {
        match self.state.take() {
            Some(IntoTlsState::MidHandshake(mut x)) => {
                x.get_mut().shutdown(Shutdown::Both).ok();
            }
            Some(IntoTlsState::Finished(x)) => {
                x.shutdown(Shutdown::Both).ok();
            }
            None => {}
        }
    }
}

enum IntoTlsState {
    MidHandshake(MidHandshakeTlsStream),
    Finished(TcpStream),
}
