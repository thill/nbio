/// nbio trait implementations for crossbeam channel structs
///
/// This module does not provide any new struct implementations.
/// Rather, it implements `Publish` and `Receive` for the existing crossbeam `Sender` and `Receiver` structs.
use std::io;

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError};

use crate::{
    DriveOutcome, Publish, PublishOutcome, Receive, ReceiveOutcome, Session, SessionStatus,
};

impl<T: 'static> Session for Sender<T> {
    fn status(&self) -> SessionStatus {
        SessionStatus::Established
    }
    fn drive(&mut self) -> Result<DriveOutcome, io::Error> {
        Ok(DriveOutcome::Idle)
    }
}
impl<T: 'static> Publish for Sender<T> {
    type PublishPayload<'a> = T where
        Self: 'a;
    fn publish<'a>(
        &mut self,
        payload: Self::PublishPayload<'a>,
    ) -> Result<PublishOutcome<Self::PublishPayload<'a>>, io::Error> {
        // status is always `Established`, so when there is no receiver, report as `Published`
        match self.try_send(payload) {
            Ok(()) => Ok(PublishOutcome::Published),
            Err(TrySendError::Disconnected(_)) => Ok(PublishOutcome::Published),
            Err(TrySendError::Full(x)) => Ok(PublishOutcome::Incomplete(x)),
        }
    }
}

impl<T: 'static> Session for Receiver<T> {
    fn status(&self) -> SessionStatus {
        SessionStatus::Established
    }
    fn drive(&mut self) -> Result<DriveOutcome, io::Error> {
        Ok(DriveOutcome::Idle)
    }
}
impl<T: 'static> Receive for Receiver<T> {
    type ReceivePayload<'a> = T
        where
            Self: 'a;
    fn receive<'a>(&'a mut self) -> Result<ReceiveOutcome<Self::ReceivePayload<'a>>, io::Error> {
        // status is always `Established`, so when there is no sender, report as `Idle`
        match self.try_recv() {
            Ok(x) => Ok(ReceiveOutcome::Payload(x)),
            Err(TryRecvError::Empty) => Ok(ReceiveOutcome::Idle),
            Err(TryRecvError::Disconnected) => Ok(ReceiveOutcome::Idle),
        }
    }
}
