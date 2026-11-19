//! TLS Connector implementations

use std::{
    io::{Error, ErrorKind},
    net::Shutdown,
};

#[cfg(feature = "native-tls")]
use native_tls::Certificate;
#[cfg(feature = "native-tls")]
use rustls_pki_types::{pem::PemObject, CertificateDer};
#[cfg(feature = "native-tls")]
use tcp_stream::TLSConfig;
use tcp_stream::{HandshakeError, MidHandshakeTlsStream, TcpStream};

pub enum TlsConnector {
    #[cfg(feature = "native-tls")]
    Native(NativeTlsConnector),
    #[cfg(feature = "rustls")]
    Rustls(RustlsConnector),
}

impl TlsConnector {
    pub fn start(&self, tcp_stream: TcpStream, domain: &str) -> Result<IntoTls, Error> {
        match self {
            #[cfg(feature = "native-tls")]
            TlsConnector::Native(connector) => connector.start(tcp_stream, domain),
            #[cfg(feature = "rustls")]
            TlsConnector::Rustls(connector) => connector.start(tcp_stream, domain),
            #[cfg(not(any(feature = "native-tls", feature = "rustls")))]
            _ => {
                let _ = (tcp_stream, domain);
                match *self {}
            }
        }
    }

    /// Create the default TLS connector, which uses rustls with the native certificate store.
    ///
    /// Returns [`ErrorKind::Unsupported`] when the `rustls` feature is not enabled.
    pub fn try_default() -> Result<Self, Error> {
        #[cfg(feature = "rustls")]
        {
            Ok(TlsConnector::Rustls(RustlsConnector {
                connector: tcp_stream::RustlsConnector::new_with_native_certs()?,
            }))
        }
        #[cfg(not(feature = "rustls"))]
        {
            Err(Error::new(
                ErrorKind::Unsupported,
                "no default TLS connector, enable the `rustls` feature or provide a TlsConnector",
            ))
        }
    }
}

#[cfg(feature = "native-tls")]
/// Provides instances of [`IntoTls`] using native-tls
pub struct NativeTlsConnector {
    connector: tcp_stream::NativeTlsConnector,
}

#[cfg(feature = "native-tls")]
impl From<tcp_stream::NativeTlsConnector> for TlsConnector {
    fn from(connector: tcp_stream::NativeTlsConnector) -> Self {
        TlsConnector::Native(NativeTlsConnector { connector })
    }
}

#[cfg(feature = "native-tls")]
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
            for cert in CertificateDer::pem_slice_iter(cert_chain.as_bytes()) {
                let cert = cert.map_err(|e| Error::new(ErrorKind::Other, e))?;
                builder.add_root_certificate(
                    Certificate::from_der(&cert)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?,
                );
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

#[cfg(feature = "rustls")]
/// Provides instances of [`IntoTls`] using rustls
pub struct RustlsConnector {
    connector: tcp_stream::RustlsConnector,
}

#[cfg(feature = "rustls")]
impl From<tcp_stream::RustlsConnector> for TlsConnector {
    fn from(connector: tcp_stream::RustlsConnector) -> Self {
        TlsConnector::Rustls(RustlsConnector { connector })
    }
}

#[cfg(feature = "rustls")]
impl RustlsConnector {
    pub fn new(use_native_certs: bool) -> Result<Self, Error> {
        let connector = if use_native_certs {
            tcp_stream::RustlsConnector::new_with_native_certs()?
        } else {
            tcp_stream::RustlsConnectorConfig::default().connector_with_no_client_auth()?
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

#[cfg(all(test, feature = "native-tls"))]
mod tests {
    use super::*;

    const CERT: &str = "\
";

    fn connector(cert_chain: &str) -> Result<NativeTlsConnector, Error> {
        NativeTlsConnector::new(
            TLSConfig {
                cert_chain: Some(cert_chain),
                ..Default::default()
            },
            false,
        )
    }

    #[test]
    fn native_connector_accepts_pem_cert_chain() {
        assert!(connector(CERT).is_ok());
    }

    #[test]
    fn native_connector_accepts_multiple_certs_and_ignores_other_items() {
        let key = "-----BEGIN PRIVATE KEY-----\nAQID\n-----END PRIVATE KEY-----\n";
        assert!(connector(&format!("{key}{CERT}{CERT}")).is_ok());
    }

    #[test]
    fn native_connector_accepts_chain_without_certs() {
        assert!(connector("").is_ok());
    }

    #[test]
    fn native_connector_rejects_malformed_pem() {
        let bad = "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n";
        assert!(connector(bad).is_err());
    }

    #[test]
    fn native_connector_rejects_invalid_der() {
        let bad = "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";
        assert!(connector(bad).is_err());
    }
}
