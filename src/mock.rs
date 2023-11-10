//! Mock sessions, most useful for testing

use std::{collections::VecDeque, io::Error};

use crate::{ReadStatus, Session, TlsSession, WriteStatus};

/// A mock session, using internal [`VecDeque`] instances to drive results returned on function calls.
///
/// Data passed to write is pushed to the internal public `write_queue`.
/// Data returned to read is popped from the internal public `read_result_queue`.
///
/// When the `write_result_queue` is empty, the write will return `Success` and be pushed to the internal `write_queue`.
/// When the `read_result_queue`, `connect_result_queue`, or `drive_result_queue` are empty, their respective function will return `None` or `false`.
pub struct MockSession<'a, R, W, WO>
where
    R: ?Sized,
    W: ?Sized + ToOwned<Owned = WO>,
{
    pub connected: bool,
    pub connect_result_queue: VecDeque<Result<bool, Error>>,
    pub drive_result_queue: VecDeque<Result<bool, Error>>,
    pub read_result_queue: VecDeque<Result<ReadStatus<'a, R>, Error>>,
    pub write_result_queue: VecDeque<Result<WriteStatus<'a, W>, Error>>,
    pub write_queue: VecDeque<WO>,
}
impl<'s, R, W, WO> MockSession<'s, R, W, WO>
where
    R: ?Sized,
    W: ?Sized + ToOwned<Owned = WO>,
{
    pub fn new() -> Self {
        Self {
            connected: true,
            connect_result_queue: VecDeque::new(),
            drive_result_queue: VecDeque::new(),
            read_result_queue: VecDeque::new(),
            write_result_queue: VecDeque::new(),
            write_queue: VecDeque::new(),
        }
    }
}
impl<'s, R, W, WO> Session for MockSession<'s, R, W, WO>
where
    R: ?Sized,
    W: ?Sized + ToOwned<Owned = WO>,
{
    type WriteData = W;
    type ReadData = R;

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn try_connect(&mut self) -> Result<bool, Error> {
        match self.connect_result_queue.pop_front() {
            Some(x) => x,
            None => Ok(false),
        }
    }

    fn drive(&mut self) -> Result<bool, Error> {
        match self.drive_result_queue.pop_front() {
            Some(x) => x,
            None => Ok(false),
        }
    }

    fn write<'a>(
        &mut self,
        data: &'a Self::WriteData,
    ) -> Result<crate::WriteStatus<'a, Self::WriteData>, Error> {
        match self.write_result_queue.pop_front() {
            Some(Ok(WriteStatus::Success)) | None => {
                self.write_queue.push_back(data.to_owned());
                Ok(WriteStatus::Success)
            }
            Some(Ok(WriteStatus::Pending(_))) => {
                panic!("MockSession does not support WriteStatus::Pending")
            }
            Some(Err(err)) => Err(err),
        }
    }

    fn read<'a>(&'a mut self) -> Result<crate::ReadStatus<'a, Self::ReadData>, Error> {
        match self.read_result_queue.pop_front() {
            None => Ok(ReadStatus::None),
            Some(x) => x,
        }
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), Error> {
        self.connected = false;
        Ok(())
    }
}

impl<'a, R, W, WO> TlsSession for MockSession<'a, R, W, WO>
where
    R: ?Sized,
    W: ?Sized + ToOwned<Owned = WO>,
{
    fn to_tls(
        &mut self,
        _domain: &str,
        _config: tcp_stream::TLSConfig<'_, '_, '_>,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn is_handshake_complete(&self) -> Result<bool, Error> {
        Ok(true)
    }
}

#[cfg(test)]
mod test {
    use std::io::{Error, ErrorKind};

    use crate::Session;

    use super::MockSession;

    #[test]
    fn test_mock_session() {
        let mut sess = MockSession::<'_, [u8], [u8], Vec<u8>>::new();

        // pop read result
        sess.read_result_queue
            .push_back(Err(Error::new(ErrorKind::BrokenPipe, "write test")));
        assert!(sess.read().is_err());
        assert!(sess.read().is_ok());

        // pop write result
        sess.write_result_queue
            .push_back(Err(Error::new(ErrorKind::BrokenPipe, "write test")));
        assert!(sess.write(&vec![0, 1]).is_err());
        assert!(sess.write(&vec![2, 3]).is_ok());

        // pop user write
        assert_eq!(sess.write_queue.pop_front().unwrap(), vec![2, 3])
    }
}
