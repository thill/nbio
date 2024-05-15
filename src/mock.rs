//! Mock sessions, most useful for testing

use std::{
    collections::VecDeque,
    fmt::Debug,
    io::{Error, ErrorKind},
};

use crate::{
    DriveOutcome, Publish, PublishOutcome, Receive, ReceiveOutcome, Session, SessionStatus,
};

/// A mock session, using internal [`VecDeque`] instances to drive results returned on function calls.
///
/// Data passed to [`MockSession::publish`] is pushed to the internal public `publish_queue`.
/// Data returned from' [`MockSession::receive`] is popped from the internal public `receive_queue`.
///
/// When the `publish_queue` is empty, the publish will return [`PublishOutcome::Success`] and be pushed to the internal `publish_queue`.
/// When the `receive_queue`, `connect_result_queue`, or `drive_result_queue` are empty, their respective function will return `None` or `false`.
pub struct MockSession<R, W> {
    pub status: SessionStatus,
    pub drive_result_queue: VecDeque<Result<DriveOutcome, Error>>,
    pub receive_queue: VecDeque<R>,
    pub publish_queue: VecDeque<W>,
}
impl<R, W> MockSession<R, W>
where
    R: 'static,
    W: 'static,
{
    pub fn new() -> Self {
        Self {
            status: SessionStatus::Established,
            drive_result_queue: VecDeque::new(),
            receive_queue: VecDeque::new(),
            publish_queue: VecDeque::new(),
        }
    }
}
impl<R, W> Session for MockSession<R, W>
where
    R: 'static,
    W: 'static,
{
    fn status(&self) -> SessionStatus {
        self.status
    }

    fn close(&mut self) {
        self.status = SessionStatus::Terminated
    }

    fn drive(&mut self) -> Result<DriveOutcome, Error> {
        if self.status == SessionStatus::Terminated {
            return Err(Error::new(ErrorKind::NotConnected, "terminated"));
        }
        match self.drive_result_queue.pop_front() {
            Some(x) => x,
            None => Ok(DriveOutcome::Idle),
        }
    }
}
impl<R, W> Publish for MockSession<R, W>
where
    R: 'static,
    W: 'static,
{
    type PublishPayload<'a> = W;
    fn publish<'a>(
        &mut self,
        payload: Self::PublishPayload<'a>,
    ) -> Result<crate::PublishOutcome<Self::PublishPayload<'a>>, Error> {
        if self.status != SessionStatus::Established {
            return Err(Error::new(ErrorKind::NotConnected, "not established"));
        }
        self.publish_queue.push_back(payload);
        Ok(PublishOutcome::Published)
    }
}
impl<R, W> Receive for MockSession<R, W>
where
    R: 'static,
    W: 'static,
{
    type ReceivePayload<'a> = R;
    fn receive<'a>(&'a mut self) -> Result<ReceiveOutcome<Self::ReceivePayload<'a>>, Error> {
        if self.status != SessionStatus::Established {
            return Err(Error::new(ErrorKind::NotConnected, "not established"));
        }
        match self.receive_queue.pop_front() {
            None => Ok(ReceiveOutcome::Idle),
            Some(x) => Ok(ReceiveOutcome::Payload(x)),
        }
    }
}
impl<R, W> Debug for MockSession<R, W>
where
    R: 'static,
    W: 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MockSession")
    }
}

#[cfg(test)]
mod test {
    use crate::{Publish, Receive, ReceiveOutcome};

    use super::MockSession;

    #[test]
    fn test_mock_session() {
        let mut sess = MockSession::<&[u8], &[u8]>::new();

        // pop read result
        sess.receive_queue.push_back("hello, reader!".as_bytes());
        if let ReceiveOutcome::Payload(x) = sess.receive().unwrap() {
            assert_eq!(x, "hello, reader!".as_bytes());
        } else {
            panic!("Data");
        }

        // pop write result
        assert!(sess.publish(&[0, 1]).is_ok());
        assert!(sess.publish(&[2, 3]).is_ok());

        // pop user write
        assert_eq!(sess.publish_queue.pop_front().unwrap(), vec![0, 1]);
        assert_eq!(sess.publish_queue.pop_front().unwrap(), vec![2, 3]);
    }
}
