//! Mock sessions, most useful for testing

use std::{collections::VecDeque, io::Error};

use crate::{ReadStatus, Session, WriteStatus};

/// A mock session, using internal [`VecDeque`] instances to drive results returned on function calls.
///
/// Data passed to write is pushed to the internal public `write_queue`.
/// Data returned to read is popped from the internal public `read_result_queue`.
///
/// When the `write_result_queue` is empty, the write will return `Success` and be pushed to the internal `write_queue`.
/// When the `read_result_queue`, `connect_result_queue`, or `drive_result_queue` are empty, their respective function will return `None` or `false`.
pub struct MockSession<R, W>
where
    R: ?Sized + ToOwned + 'static,
    W: ?Sized + ToOwned + 'static,
    R::Owned: AsRef<R>,
{
    pub connected: bool,
    pub drive_result_queue: VecDeque<Result<bool, Error>>,
    pub read_queue: VecDeque<R::Owned>,
    pub write_queue: VecDeque<W::Owned>,
    last_read: Option<R::Owned>,
}
impl<R, W> MockSession<R, W>
where
    R: ?Sized + ToOwned + 'static,
    W: ?Sized + ToOwned + 'static,
    R::Owned: AsRef<R>,
{
    pub fn new() -> Self {
        Self {
            connected: true,
            drive_result_queue: VecDeque::new(),
            read_queue: VecDeque::new(),
            write_queue: VecDeque::new(),
            last_read: None,
        }
    }
}
impl<R, W> Session for MockSession<R, W>
where
    R: ?Sized + ToOwned + 'static,
    W: ?Sized + ToOwned + 'static,
    R::Owned: AsRef<R>,
{
    type ReadData<'a> = R;
    type WriteData<'a> = W;

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn drive(&mut self) -> Result<bool, Error> {
        match self.drive_result_queue.pop_front() {
            Some(x) => x,
            None => Ok(false),
        }
    }

    fn write<'a>(
        &mut self,
        data: &'a Self::WriteData<'a>,
    ) -> Result<crate::WriteStatus<'a, Self::WriteData<'a>>, Error> {
        self.write_queue.push_back(data.to_owned());
        Ok(WriteStatus::Success)
    }

    fn read<'a>(&'a mut self) -> Result<ReadStatus<'a, Self::ReadData<'a>>, Error> {
        match self.read_queue.pop_front() {
            None => Ok(ReadStatus::None),
            Some(x) => {
                self.last_read = Some(x);
                Ok(ReadStatus::Data(self.last_read.as_ref().unwrap().as_ref()))
            }
        }
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use crate::{ReadStatus, Session};

    use super::MockSession;

    #[test]
    fn test_mock_session() {
        let mut sess = MockSession::<[u8], [u8]>::new();

        // pop read result
        sess.read_queue
            .push_back("hello, reader!".as_bytes().to_vec());
        if let ReadStatus::Data(x) = sess.read().unwrap() {
            assert_eq!(x, "hello, reader!".as_bytes());
        } else {
            panic!("Data");
        }

        // pop write result
        assert!(sess.write(&vec![0, 1]).is_ok());
        assert!(sess.write(&vec![2, 3]).is_ok());

        // pop user write
        assert_eq!(sess.write_queue.pop_front().unwrap(), vec![0, 1]);
        assert_eq!(sess.write_queue.pop_front().unwrap(), vec![2, 3]);
    }
}
