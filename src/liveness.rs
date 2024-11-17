//! [`Session`] implementations that can provide or monitor "liveness" for an underlying session.

use std::{
    fmt::Debug,
    io::{Error, ErrorKind},
    time::{Duration, SystemTime},
};

use crate::{
    DriveOutcome, Flush, Publish, PublishOutcome, Receive, ReceiveOutcome, Session, SessionStatus,
};

/// Encapsulates an underlying [`Session`] that can [`Publish`], publishing a user-defined "heartbeat" message as a given interval.
///
/// Heartbeats are produced with a call to a user-defined [`FnMut`], allowing the user to populate a heartbeat with sequences, timestamps, etc.
/// Heartbeat messages are written on calls to [`Session::drive`] using an internal timestamp to schedule when to send heartbeats.
///
/// The provided [`FnMut`] heartbeat populator should accept `&mut S` as input, where `S` is the underlying session type.
/// The heartbeat populator allows the function to attempt to write a heartbeat directly to the underlying session.
/// The session should return true if the next heartbeat is to be scheduled, or false otherwise.
///
/// [`HeartbeatingSession`] will also implement [`Receive`] for underlying any underlying [`Session`] that also implements [`Receive`].
pub struct HeartbeatingSession<S> {
    session: S,
    interval: Duration,
    heartbeat_writer: Box<dyn FnMut(&mut S) -> Result<HeartbeatOutcome, Error> + Send + 'static>,
    next_heartbeat: SystemTime,
}
impl<S> HeartbeatingSession<S>
where
    S: Publish + 'static,
{
    /// Create a new `HeartbeatingSession`, using the given [`Session`], `interval`, and `heartbeat_writer`.
    pub fn new<F>(session: S, interval: Duration, heartbeat_writer: F) -> Self
    where
        F: for<'a> FnMut(&mut S) -> Result<HeartbeatOutcome, Error> + Send + 'static,
    {
        Self {
            session,
            interval,
            heartbeat_writer: Box::new(heartbeat_writer),
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
impl<S> Session for HeartbeatingSession<S>
where
    S: Publish + 'static,
{
    fn status(&self) -> SessionStatus {
        self.session.status()
    }

    fn drive(&mut self) -> Result<DriveOutcome, std::io::Error> {
        let now = SystemTime::now();
        if now >= self.next_heartbeat {
            if let HeartbeatOutcome::Sent | HeartbeatOutcome::Skipped =
                (self.heartbeat_writer)(&mut self.session)?
            {
                self.next_heartbeat = now + self.interval;
                self.session.drive()?;
                return Ok(DriveOutcome::Active);
            }
        }
        self.session.drive()
    }
}
impl<S> Publish for HeartbeatingSession<S>
where
    S: Publish + 'static,
{
    type PublishPayload<'a> = S::PublishPayload<'a>;

    fn publish<'a>(
        &mut self,
        payload: Self::PublishPayload<'a>,
    ) -> Result<crate::PublishOutcome<Self::PublishPayload<'a>>, std::io::Error> {
        self.session.publish(payload)
    }
}
impl<S> Receive for HeartbeatingSession<S>
where
    S: Receive + Publish + 'static,
{
    type ReceivePayload<'a> = S::ReceivePayload<'a>;
    fn receive<'a>(
        &'a mut self,
    ) -> Result<crate::ReceiveOutcome<Self::ReceivePayload<'a>>, std::io::Error> {
        self.session.receive()
    }
}
impl<S> Flush for HeartbeatingSession<S>
where
    S: Receive + Flush + 'static,
{
    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.session.flush()
    }
}
impl<S> Debug for HeartbeatingSession<S>
where
    S: Publish + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeartbeatingSession")
            .field("session", &self.session)
            .finish()
    }
}

/// Used by a [`HeartbeatingSession`] to indicate if a heartbeat was sent, skipped, or failed.
///
/// When a heartbeat was `Sent` or `Skipped`, the [`HeartbeatingSession`] will schedule the next heartbeat.
/// When a heartbeat `Failed`, the [`HeartbeatingSession`] will re-attempt the heartbeat on the next call to `drive`.
#[derive(Debug, Clone, Copy)]
pub enum HeartbeatOutcome {
    Sent,
    Skipped,
    Failed,
}

/// Encapsulates an underlying [`Session`], returning an error when a liveness check fails.
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
    S: Receive + 'static,
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
    S: Receive + 'static,
{
    fn status(&self) -> crate::SessionStatus {
        self.session.status()
    }

    fn drive(&mut self) -> Result<DriveOutcome, std::io::Error> {
        match self.session.drive()? {
            DriveOutcome::Active => {
                if self.strategy.drive {
                    self.liveness = SystemTime::now() + self.timeout;
                }
                Ok(DriveOutcome::Active)
            }
            DriveOutcome::Idle => {
                if SystemTime::now() > self.liveness {
                    Err(Error::new(ErrorKind::TimedOut, "liveness check"))
                } else {
                    Ok(DriveOutcome::Idle)
                }
            }
        }
    }
}
impl<S> Receive for LivenessSession<S>
where
    S: Receive + 'static,
{
    type ReceivePayload<'a> = S::ReceivePayload<'a>;

    fn receive<'a>(
        &'a mut self,
    ) -> Result<crate::ReceiveOutcome<Self::ReceivePayload<'a>>, std::io::Error> {
        let r = self.session.receive()?;
        if let ReceiveOutcome::Payload(_) | ReceiveOutcome::Buffered = r {
            if self.strategy.receive {
                self.liveness = SystemTime::now() + self.timeout;
            }
        }
        Ok(r)
    }
}
impl<S> Publish for LivenessSession<S>
where
    S: Publish + Receive + 'static,
{
    type PublishPayload<'a> = S::PublishPayload<'a>;
    fn publish<'a>(
        &mut self,
        payload: Self::PublishPayload<'a>,
    ) -> Result<crate::PublishOutcome<Self::PublishPayload<'a>>, std::io::Error> {
        let r = self.session.publish(payload)?;
        if let PublishOutcome::Published = r {
            if self.strategy.publish {
                self.liveness = SystemTime::now() + self.timeout;
            }
        }
        Ok(r)
    }
}
impl<S> Flush for LivenessSession<S>
where
    S: Flush + Receive + 'static,
{
    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.session.flush()
    }
}
impl<S> Debug for LivenessSession<S>
where
    S: Receive + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LivenessSession")
            .field("session", &self.session)
            .finish()
    }
}

/// Used by [`LivenessSession`] to determine when to reset the `liveness` timestamp.
///
/// Checks:
/// - `receive`: resets when a call to `receive` results in [`ReceiveOutcome::Payload`] or [`ReceiveOutcome::Buffered`]
/// - `publish`: resets when a call to `publish` results in [`PublishOutcome::Published`]
/// - `drive`: resets when a call to `drive` results in [`DriveOutcome::Active`]
///
/// The [`Default`] impl enables all liveness checks, whereas `new()` will disable all liveness checks.
///
/// A builder pattern is provided to enable a combination of checks with one line of code:
/// ```no_run
/// use nbio::liveness::LivenessStrategy;
/// let strat = LivenessStrategy::new().with_receive(true).with_publish(true);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct LivenessStrategy {
    drive: bool,
    receive: bool,
    publish: bool,
}
impl LivenessStrategy {
    pub fn new() -> Self {
        Self {
            drive: false,
            receive: false,
            publish: false,
        }
    }

    pub fn is_drive(&mut self) -> bool {
        self.drive
    }
    pub fn is_receive(&mut self) -> bool {
        self.receive
    }
    pub fn is_publish(&mut self) -> bool {
        self.publish
    }

    pub fn set_drive(&mut self, drive: bool) {
        self.drive = drive;
    }
    pub fn set_receive(&mut self, receive: bool) {
        self.receive = receive;
    }
    pub fn set_publish(&mut self, publish: bool) {
        self.publish = publish;
    }

    pub fn with_drive(mut self, drive: bool) -> Self {
        self.drive = drive;
        self
    }
    pub fn with_receive(mut self, receive: bool) -> Self {
        self.receive = receive;
        self
    }
    pub fn with_publish(mut self, publish: bool) -> Self {
        self.publish = publish;
        self
    }
}
impl Default for LivenessStrategy {
    fn default() -> Self {
        Self {
            drive: true,
            receive: true,
            publish: true,
        }
    }
}
