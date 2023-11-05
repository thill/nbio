//! Internal data structures

use std::io::{Error, ErrorKind, Write};

use circbuf::CircBuf;

pub struct GrowableCircleBuf {
    circbuf: CircBuf,
    one_time_buffer: Vec<u8>,
    one_time_offset: usize,
}
impl GrowableCircleBuf {
    pub fn new(capacity: usize) -> Result<Self, Error> {
        Ok(Self {
            circbuf: CircBuf::with_capacity(capacity)
                .map_err(|err| Error::new(ErrorKind::Other, err))?,
            one_time_buffer: Vec::new(),
            one_time_offset: 0,
        })
    }

    /// return true if unread data size is 0
    pub fn is_empty(&self) -> bool {
        self.circbuf.is_empty() && self.one_time_buffer.is_empty()
    }

    /// return size of unread data
    pub fn len(&self) -> usize {
        if self.one_time_buffer.is_empty() {
            self.circbuf.len()
        } else {
            self.one_time_buffer.len()
        }
    }

    /// return if data was written.
    /// data larger than the capacity will only write when the buffer is empty.
    pub fn try_write(&mut self, data: &Vec<&[u8]>) -> Result<bool, Error> {
        let total_data_len = data.iter().map(|x| x.len()).sum::<usize>();
        if !self.is_empty() && total_data_len >= self.circbuf.len() - self.len() {}

        if total_data_len > self.circbuf.cap() {
            // data will never fit in circle buf, try to use one-time buffer
            if self.is_empty() {
                // populate one-time buffer
                self.one_time_offset = 0;
                for d in data {
                    self.one_time_buffer.extend_from_slice(d);
                }
                return Ok(true);
            } else {
                // can only write to one-time buffer when circbuf is drained
                return Ok(false);
            }
        }

        if total_data_len > self.circbuf.avail() {
            // data will not fit in available space
            return Ok(false);
        }

        // write to cir
        for d in data {
            self.circbuf.write_all(d)?;
        }

        Ok(true)
    }

    /// peek at available bytes
    pub fn peek_read<'a>(&'a self) -> &'a [u8] {
        if self.one_time_buffer.is_empty() {
            let avail = self.circbuf.get_bytes();
            if avail[0].is_empty() {
                avail[1]
            } else {
                avail[0]
            }
        } else {
            &self.one_time_buffer
        }
    }

    /// advance bytes that were able to be consumed from read
    pub fn advance_read(&mut self, size: usize) -> Result<(), Error> {
        if self.one_time_buffer.is_empty() {
            self.circbuf
                .advance_read(size)
                .map_err(|x| Error::new(ErrorKind::Other, x))
        } else if size == self.one_time_buffer.len() {
            self.one_time_offset = 0;
            self.one_time_buffer = Vec::new();
            Ok(())
        } else if size < self.one_time_buffer.len() {
            self.one_time_offset += size;
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::InvalidData,
                "over-read one-time buffer",
            ))
        }
    }
}
