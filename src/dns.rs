//! Name Resolver Trait and default impl

use std::{
    io::{Error, ErrorKind},
    net::{SocketAddr, ToSocketAddrs},
};

pub(crate) const STD_NAME_RESOLVER_PROVIDER: StdNameResolverProvider = StdNameResolverProvider;

/// Provides instances of [`NameResolver`] for a given string
pub trait NameResolverProvider {
    fn start(&self, s: String) -> Box<dyn NameResolver>;
}

/// Polls a [`NameResolutionOutcome`] to completion.
pub trait NameResolver: Send + Sync {
    fn poll(&mut self) -> Result<NameResolutionOutcome, Error>;
}
/// [`NameResolver`] outcome that can be polled to completion.
pub enum NameResolutionOutcome {
    Idle,
    Active,
    Resolved(Vec<SocketAddr>),
}

/// Provides instances of [`StdNameResolver`]
pub struct StdNameResolverProvider;
impl NameResolverProvider for StdNameResolverProvider {
    fn start(&self, s: String) -> Box<dyn NameResolver> {
        Box::new(StdNameResolver { s: Some(s) })
    }
}

/// Uses [`ToSocketAddrs`], which may block
pub struct StdNameResolver {
    s: Option<String>,
}
impl NameResolver for StdNameResolver {
    fn poll(&mut self) -> Result<NameResolutionOutcome, Error> {
        match self.s.take() {
            Some(x) => x
                .to_socket_addrs()
                .map(|x| NameResolutionOutcome::Resolved(x.collect())),
            None => Err(Error::new(ErrorKind::Other, "done!")),
        }
    }
}
