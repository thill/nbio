//! Utilities that use or encapsulate a [`Session`]

use std::{
    io::{Error, ErrorKind},
    time::{Duration, SystemTime},
};

use tcp_stream::TLSConfig;

use crate::{ReadStatus, Session, TlsSession, WriteStatus};

/// Encapsulates an underlying session, writing a user-defined "heartbeat" message as a given interval.
///
/// Heartbeats are produced with a call to a user-defined [`FnMut`], allowing the user to populate a heartbeat with sequences, timestamps, etc.
/// Heartbeat messages are written on calls to `drive()` using an internal timestamp to schedule when to send heartbeats.
///
/// The provided [`FnMut`] heartbeat populator should accept `&mut dyn Session` as input, allowing the function to optionally write a heartbeat directly to the underlying session.
/// The session should return true if the next heartbeat is to be scheduled, or false if the
pub struct HeartbeatingSession<S, F>
where
    S: Session,
    F: FnMut(
        &mut dyn Session<ReadData = S::ReadData, WriteData = S::WriteData>,
    ) -> Result<HeartbeatResult, Error>,
{
    session: S,
    interval: Duration,
    heartbeat_writer: F,
    next_heartbeat: SystemTime,
}
impl<S, F> HeartbeatingSession<S, F>
where
    S: Session,
    F: FnMut(
        &mut dyn Session<ReadData = S::ReadData, WriteData = S::WriteData>,
    ) -> Result<HeartbeatResult, Error>,
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
}
impl<S, F> Session for HeartbeatingSession<S, F>
where
    S: Session,
    F: FnMut(
        &mut dyn Session<ReadData = S::ReadData, WriteData = S::WriteData>,
    ) -> Result<HeartbeatResult, Error>,
{
    type ReadData = S::ReadData;
    type WriteData = S::WriteData;

    fn is_connected(&self) -> bool {
        self.session.is_connected()
    }

    fn try_connect(&mut self) -> Result<bool, std::io::Error> {
        self.session.try_connect()
    }

    fn drive(&mut self) -> Result<bool, std::io::Error> {
        let now = SystemTime::now();
        if now >= self.next_heartbeat {
            if let HeartbeatResult::Sent | HeartbeatResult::Skipped =
                (self.heartbeat_writer)(&mut NoWriteSession::new(&mut self.session))?
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
        data: &'a Self::WriteData,
    ) -> Result<crate::WriteStatus<'a, Self::WriteData>, std::io::Error> {
        self.session.write(data)
    }

    fn read<'a>(&'a mut self) -> Result<crate::ReadStatus<'a, Self::ReadData>, std::io::Error> {
        self.session.read()
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.session.flush()
    }

    fn close(&mut self) -> Result<(), std::io::Error> {
        self.session.close()
    }
}
impl<S, F> TlsSession for HeartbeatingSession<S, F>
where
    S: TlsSession,
    F: FnMut(
        &mut dyn Session<ReadData = S::ReadData, WriteData = S::WriteData>,
    ) -> Result<HeartbeatResult, Error>,
{
    fn to_tls(&mut self, domain: &str, config: TLSConfig<'_, '_, '_>) -> Result<(), Error> {
        self.session.to_tls(domain, config)
    }

    fn is_handshake_complete(&self) -> Result<bool, Error> {
        self.session.is_handshake_complete()
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

/// Internal, used to ensure a user-provided function may not `read`.
struct NoWriteSession<'s, S: Session> {
    session: &'s mut S,
}
impl<'s, S: Session> NoWriteSession<'s, S> {
    fn new(session: &'s mut S) -> Self {
        Self { session }
    }
}
impl<'s, S: Session> Session for NoWriteSession<'s, S> {
    type ReadData = S::ReadData;
    type WriteData = S::WriteData;

    fn is_connected(&self) -> bool {
        self.session.is_connected()
    }

    fn try_connect(&mut self) -> Result<bool, Error> {
        self.session.try_connect()
    }

    fn drive(&mut self) -> Result<bool, Error> {
        self.session.drive()
    }

    fn write<'a>(
        &mut self,
        data: &'a Self::WriteData,
    ) -> Result<WriteStatus<'a, Self::WriteData>, Error> {
        self.session.write(data)
    }

    fn read<'a>(&'a mut self) -> Result<ReadStatus<'a, Self::ReadData>, Error> {
        Err(Error::new(
            ErrorKind::PermissionDenied,
            "heartbeat function may not read",
        ))
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.session.flush()
    }

    fn close(&mut self) -> Result<(), Error> {
        self.session.close()
    }
}

/// Encapsulates an underlying session, returning an error when a liveness check fails.
///
/// A liveness timestamp is tracked internally and is reset based on the given [`LivenessStrategy`].
///
/// When the configured duration elapses without liveness being reset, the `drive` function will return an `Err` or [`ErrorKind::TimedOut`].
pub struct LivenessSession<S: Session> {
    session: S,
    timeout: Duration,
    strategy: LivenessStrategy,
    liveness: SystemTime,
}
impl<S: Session> LivenessSession<S> {
    /// Create a new `LivenessSession`, using the given [`Session`], [`LivenessStrategy`], and `timeout`.
    pub fn new(session: S, timeout: Duration, strategy: LivenessStrategy) -> Self {
        Self {
            session,
            timeout,
            strategy,
            liveness: SystemTime::now() + timeout,
        }
    }
}
impl<S: Session> Session for LivenessSession<S> {
    type ReadData = S::ReadData;
    type WriteData = S::WriteData;

    fn is_connected(&self) -> bool {
        self.session.is_connected()
    }

    fn try_connect(&mut self) -> Result<bool, std::io::Error> {
        if self.session.try_connect()? {
            if self.strategy.connect {
                self.liveness = SystemTime::now() + self.timeout;
            }
            Ok(true)
        } else {
            Ok(false)
        }
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
        data: &'a Self::WriteData,
    ) -> Result<crate::WriteStatus<'a, Self::WriteData>, std::io::Error> {
        let r = self.session.write(data)?;
        if let WriteStatus::Success = r {
            if self.strategy.write {
                self.liveness = SystemTime::now() + self.timeout;
            }
        }
        Ok(r)
    }

    fn read<'a>(&'a mut self) -> Result<crate::ReadStatus<'a, Self::ReadData>, std::io::Error> {
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

    fn close(&mut self) -> Result<(), std::io::Error> {
        self.session.close()
    }
}
impl<S: TlsSession> TlsSession for LivenessSession<S> {
    fn to_tls(&mut self, domain: &str, config: TLSConfig<'_, '_, '_>) -> Result<(), Error> {
        self.session.to_tls(domain, config)
    }

    fn is_handshake_complete(&self) -> Result<bool, Error> {
        self.session.is_handshake_complete()
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

#[cfg(test)]
mod test {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, SystemTime},
    };

    use crate::{mock::MockSession, ReadStatus, Session};

    use super::{HeartbeatResult, HeartbeatingSession, LivenessSession, LivenessStrategy};

    #[test]
    fn test_heartbeat() {
        let seq = AtomicUsize::new(0);
        let mut sess = HeartbeatingSession::new(
            MockSession::new(),
            Duration::from_millis(50),
            move |s: &mut dyn Session<WriteData = usize, ReadData = usize>| {
                s.write(&seq.fetch_add(1, Ordering::AcqRel))?;
                Ok(HeartbeatResult::Sent)
            },
        );

        // drive for more than 3 intervals
        let end = SystemTime::now() + Duration::from_millis(151);
        while SystemTime::now() < end {
            sess.drive().unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        sess.drive().unwrap();

        // assert at least 3 and at most 5 heartbeats received
        assert_eq!(0, sess.session.write_queue.pop_front().unwrap());
        assert_eq!(1, sess.session.write_queue.pop_front().unwrap());
        assert_eq!(2, sess.session.write_queue.pop_front().unwrap());
        assert!(sess.session.write_queue.len() <= 2);
    }

    #[test]
    fn test_liveness() {
        let mut sess = LivenessSession::new(
            MockSession::new(),
            Duration::from_millis(50),
            LivenessStrategy::default(),
        );

        // send and drive for longer than interval
        let end = SystemTime::now() + Duration::from_millis(100);
        while SystemTime::now() < end {
            sess.session
                .read_result_queue
                .push_back(Ok(ReadStatus::Data(&(0 as usize))));
            sess.read().unwrap();
            sess.write(&(0 as usize)).unwrap();
            assert!(sess.session.write_queue.pop_front().is_some());
            sess.drive().unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }

        // assert drive does not report liveness error
        while sess.drive().unwrap() {}
        assert!(sess.drive().is_ok());

        // drive for longer than interval
        let end = SystemTime::now() + Duration::from_millis(100);
        while SystemTime::now() < end {
            sess.drive().ok();
            std::thread::sleep(Duration::from_millis(1));
        }

        // assert drive reports liveness error
        assert!(sess.drive().is_err());
    }
}
