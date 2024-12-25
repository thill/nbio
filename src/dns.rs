//! Name Resolver Trait and default impl

use std::{
    io::{Error, ErrorKind},
    net::{SocketAddr, ToSocketAddrs},
};

pub(crate) const STD_NAME_RESOLVER_PROVIDER: StdAddrResolverProvider = StdAddrResolverProvider;

/// Provides instances of [`AddrResolver`] for a given string
pub trait AddrResolverProvider {
    fn start(&self, s: String) -> Box<dyn AddrResolver>;
}

/// Polls a [`AddrResolutionOutcome`] to completion.
pub trait AddrResolver: Send + Sync {
    fn poll(&mut self) -> Result<AddrResolutionOutcome, Error>;
}

/// [`AddrResolver`] outcome that can be polled to completion.
pub enum AddrResolutionOutcome {
    Idle,
    Active,
    Resolved(Vec<SocketAddr>),
}

/// Provides instances of [`StdAddrResolver`]
pub struct StdAddrResolverProvider;
impl AddrResolverProvider for StdAddrResolverProvider {
    fn start(&self, s: String) -> Box<dyn AddrResolver> {
        Box::new(StdAddrResolver { s: Some(s) })
    }
}

/// Uses [`ToSocketAddrs`], which may block
pub struct StdAddrResolver {
    s: Option<String>,
}
impl AddrResolver for StdAddrResolver {
    fn poll(&mut self) -> Result<AddrResolutionOutcome, Error> {
        match self.s.take() {
            Some(x) => x
                .to_socket_addrs()
                .map(|x| AddrResolutionOutcome::Resolved(x.collect())),
            None => Err(Error::new(ErrorKind::Other, "done!")),
        }
    }
}
