//! Internal data structures

use std::io::{Error, ErrorKind};

pub struct GrowableCircleBuf {
    capacity: usize,
    buffer: Vec<u8>,
    write_off: usize,
    read_off: usize,
}
impl GrowableCircleBuf {
    pub fn new(capacity: usize) -> Self {
        let mut buffer = Vec::new();
        buffer.resize(capacity, 0);
        Self {
            capacity,
            buffer,
            write_off: 0,
            read_off: 0,
        }
    }
    /// return true if unread data size is 0
    pub fn is_empty(&self) -> bool {
        self.read_off == self.write_off
    }
    /// return size of unread data
    pub fn len(&self) -> usize {
        if self.read_off == self.write_off {
            0
        } else if self.read_off < self.write_off {
            self.write_off - self.read_off
        } else {
            self.buffer.len() - (self.read_off - self.write_off)
        }
    }
    /// return if data was written.
    /// data larger than the capacity will only write when the buffer is empty.
    pub fn try_write(&mut self, data: &Vec<&[u8]>) -> Result<bool, Error> {
        self.check_wrap_offsets();

        let total_data_len = data.iter().map(|x| x.len()).sum::<usize>();
        if !self.is_empty() && total_data_len >= self.buffer.len() - self.len() {
            // buffer is not in a state to grow (not empty), and data will not fit in remaining buffer space
            return Ok(false);
        }

        // handle grown buffer
        if self.buffer.len() > self.capacity {
            if self.is_empty() {
                // empty, shrink back
                self.write_off = 0;
                self.read_off = 0;
                self.buffer.resize(self.capacity, 0);
            } else {
                // don't allow additional writes to a grown buffer
                return Ok(false);
            }
        }

        // if buffer is empty, always write at beginning
        if self.read_off == self.write_off {
            self.read_off = 0;
            self.write_off = 0;
            if total_data_len > self.capacity {
                // oversized data, grow for one-time oversized write
                self.buffer = Vec::new();
                for data in data {
                    self.buffer.extend_from_slice(data);
                }
                self.write_off = total_data_len;
            } else {
                // write to beginning of buffer
                for data in data {
                    self.buffer[self.write_off..self.write_off + data.len()].copy_from_slice(data);
                    self.write_off += data.len();
                }
            }
            return Ok(true);
        }

        // otherwise, write all data to buffer
        for data in data {
            if self.read_off > self.write_off {
                // insert data from write_off to read_off - 1
                if data.len() < self.read_off - self.write_off {
                    self.buffer[self.write_off..self.write_off + data.len()].copy_from_slice(data);
                    self.write_off += data.len();
                } else {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "circlebuf checked write will not fit in buffer",
                    ));
                }
            } else {
                // can try to insert data from write_off to end, and from beginning to read_off
                if data.len() <= (self.buffer.len() - self.write_off) {
                    // will fit from write_off to end
                    self.buffer[self.write_off..self.write_off + data.len()].copy_from_slice(data);
                    self.write_off += data.len();
                } else if data.len() < (self.buffer.len() - self.write_off) + self.read_off {
                    // write from write_off to end, and from beginning to read_off
                    let first_write_size = self.buffer.len() - self.write_off;
                    self.buffer[self.write_off..].copy_from_slice(&data[..first_write_size]);
                    self.buffer[0..data.len() - first_write_size]
                        .copy_from_slice(&data[first_write_size..]);
                    self.write_off = data.len() - first_write_size;
                } else {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "circlebuf checked write will not fit in buffer",
                    ));
                }
            }
        }
        Ok(true)
    }

    /// peek at available bytes
    pub fn peek_read<'a>(&'a self) -> &'a [u8] {
        // peek data
        if self.read_off == self.write_off {
            // read empty buffer
            &self.buffer[0..0]
        } else if self.read_off < self.write_off {
            // read from read_off to write_off
            &self.buffer[self.read_off..self.write_off]
        } else {
            // read from read_off to end
            &self.buffer[self.read_off..]
        }
    }

    /// advance bytes that were able to be consumed from read
    pub fn advance_read(&mut self, size: usize) {
        self.check_wrap_offsets();
        if self.read_off + size == self.buffer.len() {
            self.read_off = 0;
        } else if self.read_off + size < self.buffer.len() {
            self.read_off += size;
        } else {
            self.read_off = size - (self.buffer.len() - self.read_off);
        }
    }

    fn check_wrap_offsets(&mut self) {
        if self.read_off == self.write_off {
            // empty buffers should always write next data to beginning
            self.read_off = 0;
            self.write_off = 0;
        } else {
            if self.read_off == self.buffer.len() {
                self.read_off = 0;
            }
            if self.write_off == self.buffer.len() && self.read_off != 0 {
                self.write_off = 0;
            }
        }
    }
}
