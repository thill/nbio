//! Utilities that use or encapsulate a [`Session`]

use std::{
    io::{Error, ErrorKind},
    time::{Duration, SystemTime},
};

use crate::{ReadStatus, Session, WriteStatus};

/// Encapsulates an underlying session, writing a user-defined "heartbeat" message as a given interval.
///
/// Heartbeats are produced with a call to a user-defined [`FnMut`], allowing the user to populate a heartbeat with sequences, timestamps, etc.
/// Heartbeat messages are written on calls to `drive()` using an internal timestamp to schedule when to send heartbeats.
///
/// The provided [`FnMut`] heartbeat populator should accept `&mut S` as input, where `S` is the underlying session type.
/// The heartbeat populator allows the function to attempt to write a heartbeat directly to the underlying session.
/// The session should return true if the next heartbeat is to be scheduled, or false otherwise.
pub struct HeartbeatingSession<S, F> {
    session: S,
    interval: Duration,
    heartbeat_writer: F,
    next_heartbeat: SystemTime,
}
impl<S, F> HeartbeatingSession<S, F>
where
    S: Session + 'static,
    F: for<'a> FnMut(&mut S) -> Result<HeartbeatResult, Error> + 'static,
{
    /// Create a new `HeartbeatingSession`, using the given [`Session`], `interval`, and `heartbeat_writer`.
    pub fn new(session: S, interval: Duration, heartbeat_writer: F) -> Self {
        Self {
            session,
            interval,
            heartbeat_writer,
            next_heartbeat: SystemTime::now() + interval,
        }
    }
    /// Get the underlying session
    pub fn session<'a>(&'a mut self) -> &'a S {
        &self.session
    }
    /// Get the mutable underlying session
    pub fn session_mut<'a>(&'a mut self) -> &'a mut S {
        &mut self.session
    }
}
impl<S, F> Session for HeartbeatingSession<S, F>
where
    S: Session + 'static,
    F: for<'a> FnMut(&mut S) -> Result<HeartbeatResult, Error> + 'static,
{
    type ReadData<'a> = S::ReadData<'a>;
    type WriteData<'a> = S::WriteData<'a>;

    fn is_connected(&self) -> bool {
        self.session.is_connected()
    }

    fn drive(&mut self) -> Result<bool, std::io::Error> {
        let now = SystemTime::now();
        if now >= self.next_heartbeat {
            if let HeartbeatResult::Sent | HeartbeatResult::Skipped =
                (self.heartbeat_writer)(&mut self.session)?
            {
                self.next_heartbeat = now + self.interval;
                self.session.drive()?;
                return Ok(true);
            }
        }
        self.session.drive()
    }

    fn write<'a>(
        &mut self,
        data: &'a Self::WriteData<'a>,
    ) -> Result<crate::WriteStatus<'a, Self::WriteData<'a>>, std::io::Error> {
        self.session.write(data)
    }

    fn read<'a>(&'a mut self) -> Result<crate::ReadStatus<'a, Self::ReadData<'a>>, std::io::Error> {
        self.session.read()
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.session.flush()
    }
}

/// Used by a [`HeartbeatingSession`] to indicate if a heartbeat was sent, skipped, or failed.
///
/// When a heartbeat was `Sent` or `Skipped`, the [`HeartbeatingSession`] will schedule the next heartbeat.
/// When a heartbeat `Failed`, the [`HeartbeatingSession`] will re-attempt the heartbeat on the next call to `drive`.
#[derive(Debug, Clone, Copy)]
pub enum HeartbeatResult {
    Sent,
    Skipped,
    Failed,
}

/// Encapsulates an underlying session, returning an error when a liveness check fails.
///
/// A liveness timestamp is tracked internally and is reset based on the given [`LivenessStrategy`].
///
/// When the configured duration elapses without liveness being reset, the `drive` function will return an `Err` or [`ErrorKind::TimedOut`].
pub struct LivenessSession<S> {
    session: S,
    timeout: Duration,
    strategy: LivenessStrategy,
    liveness: SystemTime,
}
impl<S> LivenessSession<S>
where
    S: Session + 'static,
{
    /// Create a new `LivenessSession`, using the given [`Session`], [`LivenessStrategy`], and `timeout`.
    pub fn new(session: S, timeout: Duration, strategy: LivenessStrategy) -> Self {
        Self {
            session,
            timeout,
            strategy,
            liveness: SystemTime::now() + timeout,
        }
    }
    /// Get the underlying session
    pub fn session<'a>(&'a mut self) -> &'a S {
        &self.session
    }
    /// Get the mutable underlying session
    pub fn session_mut<'a>(&'a mut self) -> &'a mut S {
        &mut self.session
    }
}
impl<S> Session for LivenessSession<S>
where
    S: Session + 'static,
{
    type ReadData<'a> = S::ReadData<'a>;
    type WriteData<'a> = S::WriteData<'a>;

    fn is_connected(&self) -> bool {
        self.session.is_connected()
    }

    fn drive(&mut self) -> Result<bool, std::io::Error> {
        if self.session.drive()? {
            if self.strategy.drive {
                self.liveness = SystemTime::now() + self.timeout;
            }
            Ok(true)
        } else if SystemTime::now() > self.liveness {
            Err(Error::new(ErrorKind::TimedOut, "liveness check"))
        } else {
            Ok(false)
        }
    }

    fn write<'a>(
        &mut self,
        data: &'a Self::WriteData<'a>,
    ) -> Result<crate::WriteStatus<'a, Self::WriteData<'a>>, std::io::Error> {
        let r = self.session.write(data)?;
        if let WriteStatus::Success = r {
            if self.strategy.write {
                self.liveness = SystemTime::now() + self.timeout;
            }
        }
        Ok(r)
    }

    fn read<'a>(&'a mut self) -> Result<crate::ReadStatus<'a, Self::ReadData<'a>>, std::io::Error> {
        let r = self.session.read()?;
        if let ReadStatus::Data(_) | ReadStatus::Buffered = r {
            if self.strategy.read {
                self.liveness = SystemTime::now() + self.timeout;
            }
        }
        Ok(r)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.session.flush()
    }
}

/// Used by [`LivenessSession`] to determine when to reset the `liveness` timestamp.
///
/// Checks:
/// - `read`: resets when a call to `read` results in [`ReadStatus::Data`] or [`ReadStatus::Buffered`]
/// - `write`: resets when a call to `write` results in [`WriteStatus::Success`]
/// - `drive`: resets when a call to `drive` results in `true`
/// - `connect`: resets when a call to `try_connect` results in `true`
///
/// The [`Default`] impl enables all liveness checks, whereas `new()` will disable all liveness checks.
///
/// A builder pattern is provided to enable a combination of checks with one line of code:
/// ```
/// use nbio::util::LivenessStrategy;
/// let strat = LivenessStrategy::new().with_read(true).with_write(true);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct LivenessStrategy {
    connect: bool,
    drive: bool,
    read: bool,
    write: bool,
}
impl LivenessStrategy {
    pub fn new() -> Self {
        Self {
            connect: false,
            drive: false,
            read: false,
            write: false,
        }
    }

    pub fn is_connect(&mut self) -> bool {
        self.connect
    }
    pub fn is_drive(&mut self) -> bool {
        self.drive
    }
    pub fn is_read(&mut self) -> bool {
        self.read
    }
    pub fn is_write(&mut self) -> bool {
        self.write
    }

    pub fn set_connect(&mut self, connect: bool) {
        self.connect = connect;
    }
    pub fn set_drive(&mut self, drive: bool) {
        self.drive = drive;
    }
    pub fn set_read(&mut self, read: bool) {
        self.read = read;
    }
    pub fn set_write(&mut self, write: bool) {
        self.write = write;
    }

    pub fn with_connect(mut self, connect: bool) -> Self {
        self.connect = connect;
        self
    }
    pub fn with_drive(mut self, drive: bool) -> Self {
        self.drive = drive;
        self
    }
    pub fn with_read(mut self, read: bool) -> Self {
        self.read = read;
        self
    }
    pub fn with_write(mut self, write: bool) -> Self {
        self.write = write;
        self
    }
}
impl Default for LivenessStrategy {
    fn default() -> Self {
        Self {
            connect: true,
            drive: true,
            read: true,
            write: true,
        }
    }
}
