//! Name Resolver Trait and default impl

use std::{
    fmt::Debug,
    io::{Error, ErrorKind},
    net::{SocketAddr, ToSocketAddrs},
};

/// A [`ResolveAddr`] that can handle multiple impls, including dynamic dispatch.
pub enum AddrResolver {
    ToSocketAddrs(ToSocketAddrResolver),
    Dyn(Box<dyn ResolveAddr<IntoAddr = Box<dyn IntoAddr>>>),
}

impl ResolveAddr for AddrResolver {
    type IntoAddr = AnyIntoAddr;
    fn resolve_addr(&self, s: String) -> Result<Self::IntoAddr, Error> {
        match self {
            Self::ToSocketAddrs(x) => Ok(AnyIntoAddr::ToSocketAddrIntoAddr(x.resolve_addr(s)?)),
            Self::Dyn(x) => Ok(AnyIntoAddr::Dyn(Box::new(x.resolve_addr(s)?))),
        }
    }
}

impl Debug for AddrResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToSocketAddrs(_) => write!(f, "ToSocketAddrs"),
            Self::Dyn(_) => write!(f, "Dyn"),
        }
    }
}

impl Default for AddrResolver {
    fn default() -> Self {
        Self::ToSocketAddrs(ToSocketAddrResolver)
    }
}

/// Provides instances of [`AddrResolver`] for a given string
pub trait ResolveAddr {
    type IntoAddr: IntoAddr;
    fn resolve_addr(&self, s: String) -> Result<Self::IntoAddr, Error>;
}

/// [`IntoAddr`] outcome that can be polled to completion.
pub enum IntoAddrOutcome {
    Idle,
    Active,
    Finished(Vec<SocketAddr>),
}

/// A request to resolve an address that can be polled to completion.
pub trait IntoAddr {
    fn poll(&mut self) -> Result<IntoAddrOutcome, Error>;
}

impl IntoAddr for Box<dyn IntoAddr> {
    fn poll(&mut self) -> Result<IntoAddrOutcome, Error> {
        self.as_mut().poll()
    }
}

/// A [`IntoAddr`] for [`AddrResolver`], which is an enum that can handle multiple impls.
pub enum AnyIntoAddr {
    ToSocketAddrIntoAddr(ToSocketAddrIntoAddr),
    Dyn(Box<dyn IntoAddr>),
}

impl IntoAddr for AnyIntoAddr {
    fn poll(&mut self) -> Result<IntoAddrOutcome, Error> {
        match self {
            Self::ToSocketAddrIntoAddr(x) => x.poll(),
            Self::Dyn(x) => x.poll(),
        }
    }
}

/// Provides instances of [`StdAddrResolver`]
pub struct ToSocketAddrResolver;
impl ResolveAddr for ToSocketAddrResolver {
    type IntoAddr = ToSocketAddrIntoAddr;
    fn resolve_addr(&self, s: String) -> Result<Self::IntoAddr, Error> {
        Ok(ToSocketAddrIntoAddr { s: Some(s) })
    }
}

/// Uses [`ToSocketAddrs`], which may block for a moment
pub struct ToSocketAddrIntoAddr {
    s: Option<String>,
}
impl IntoAddr for ToSocketAddrIntoAddr {
    fn poll(&mut self) -> Result<IntoAddrOutcome, Error> {
        match self.s.take() {
            Some(x) => x
                .to_socket_addrs()
                .map(|x| IntoAddrOutcome::Finished(x.collect())),
            None => Err(Error::new(ErrorKind::Other, "done!")),
        }
    }
}
