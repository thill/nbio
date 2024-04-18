//! [`FramingSession`] will "frame" messages using an underlying binary [`Session`].
//!
//! The [`FramingStrategy`] and [`FramingSession`] pattern provided in this module enable a convenient way to convert
//! data from an underlying binary streaming protocol back and forth with parsed events.

use std::{
    io::{Error, ErrorKind},
    mem::swap,
};

use crate::{internal::GrowableCircleBuf, ReadStatus, Session, WriteStatus};

/// # Framing Session
///
/// Encapsulates a binary streaming [`Session`], providing framing of underlying messages using the given [`FramingStrategy`].
/// The underlying [`Session::ReadData`] and [`Session::WriteData`] must be `[u8]`.
///
/// This effectively translates any `Session<ReadData=[u8], WriteData=[u8]>` to a `Session<ReadData=YourStruct, WriteData=YourStruct>`
/// provided an apporpriate [`FramingStrategy`].
///
/// ## Drive
///
/// It is imperative that the `drive` function be called regularly in this implementation.
/// This function is responsible for writing bytes from the internal write buffer to the underlying [`Session`].
///
/// ## Write Buffering
///
/// Frames passed into `write` are serialized using the [`FramingStrategy`] and are copied to an internal circular buffer.
/// The internal circular buffer is initialized with a capacity given to the `new` function.
///
/// If the entire frame is not able to fit in the remaining circular buffer space, none of the frame will be copied.
/// In this case, [`WriteStatus::Pending`] will be returned with a reference to the frame to be retried later.
///
/// The circular buffer will grow to fit frames that exceed the entire circular buffer capacity, but only when the buffer is empty.
/// This allows for any sized frame to be successfuly sent while avoiding the circular buffer growing the unreasonable sizes.
///
/// ## Read Buffering
///
/// Data is read from the underlying binary [`Session`] into an internal [`Vec<u8>`].
/// Each call to read will first check if the read buffer contains a full frame to be returned immediately.
/// The underlying [`Session`]'s `read' function will only be called when the read buffer is devoid of a full frame.
/// This avoids the buffer growing to an unreasonable size, as it will only ever grow to the size of a frame plus the next read size.
pub struct FramingSession<S, F> {
    session: S,
    framing_strategy: F,
    write_buffer: GrowableCircleBuf,
    read_buffer: Vec<u8>,
    read_advance: usize,
}
impl<S, F> FramingSession<S, F>
where
    S: for<'a> Session<ReadData<'a> = &'a [u8], WriteData<'a> = &'a [u8]> + 'static,
    F: FramingStrategy + 'static,
{
    /// Create a new [`FramingSession`]
    ///
    /// # Parameters
    /// - `session`: The underlying binary [`Session`]
    /// - `framing_strategy`: The strategy used to convert binary data back and forth to framed messages
    /// - `write_buffer_capacity`: The capacity, **in bytes**, of the underlying circular buffer that holds serialized write frames
    pub fn new(session: S, framing_strategy: F, write_buffer_capacity: usize) -> Self {
        Self {
            session,
            framing_strategy,
            write_buffer: GrowableCircleBuf::new(write_buffer_capacity)
                .unwrap_or_else(|_| GrowableCircleBuf::new(usize::MAX / 2).unwrap()),
            read_buffer: Vec::new(),
            read_advance: 0,
        }
    }
}
impl<S, F> Session for FramingSession<S, F>
where
    S: for<'a> Session<ReadData<'a> = &'a [u8], WriteData<'a> = &'a [u8]> + 'static,
    F: FramingStrategy + 'static,
{
    type ReadData<'a> = F::ReadFrame<'a>;
    type WriteData<'a> = F::WriteFrame<'a>;

    fn is_connected(&self) -> bool {
        self.session.is_connected()
    }

    fn drive(&mut self) -> Result<bool, std::io::Error> {
        self.session.drive()?;
        if self.write_buffer.is_empty() {
            return Ok(false);
        }
        let write_buffer = self.write_buffer.peek_read();
        let wrote_len = match self.session.write(write_buffer)? {
            WriteStatus::Success => write_buffer.len(),
            WriteStatus::Pending(pending) => write_buffer.len() - pending.len(),
        };
        self.write_buffer.advance_read(wrote_len)?;
        Ok(wrote_len > 0)
    }

    fn write<'a>(
        &mut self,
        frame: Self::WriteData<'a>,
    ) -> Result<WriteStatus<Self::WriteData<'a>>, Error> {
        if !self.session.is_connected() {
            return Err(Error::new(
                ErrorKind::NotConnected,
                "underlying session is not connected",
            ));
        }
        let data = self.framing_strategy.serialize_frame(&frame)?;
        if self.write_buffer.try_write(&data)? {
            Ok(WriteStatus::Success)
        } else {
            Ok(WriteStatus::Pending(frame))
        }
    }

    fn read<'a>(&'a mut self) -> Result<ReadStatus<Self::ReadData<'a>>, std::io::Error> {
        if self.read_advance != 0 {
            let mut new_buf = Vec::from(&self.read_buffer[self.read_advance..]);
            self.read_advance = 0;
            swap(&mut new_buf, &mut self.read_buffer);
        }
        // try deserializing before receiving to avoid growing the buffer forever in a slow consumer
        if self
            .framing_strategy
            .check_deserialize_frame(&self.read_buffer, false)?
        {
            let de = self.framing_strategy.deserialize_frame(&self.read_buffer)?;
            self.read_advance = de.size;
            return Ok(ReadStatus::Data(de.frame));
        }
        let data = match self.session.read()? {
            ReadStatus::Data(data) => data,
            ReadStatus::Buffered => return Ok(ReadStatus::Buffered),
            ReadStatus::None => return Ok(ReadStatus::None),
        };
        self.read_buffer.extend_from_slice(data);
        Ok(ReadStatus::Buffered)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        while !self.write_buffer.is_empty() {
            self.drive()?;
        }
        self.session.flush()
    }
}

/// Serialize and deserialize frames using buffer slices.
/// This is used by a [`FramingSession`] to read/write frames using a raw binary [`Session`].
pub trait FramingStrategy {
    /// Type returned by `deserialize_frame`
    ///
    /// Examples:
    /// - Framed `[u8]` contents for streaming binary messages
    /// - JSON payload for streaming JSON messages
    /// - `HttpResponse` for an HttpClient connection
    type ReadFrame<'a>
    where
        Self: 'a;

    /// Type returned by `serialize_frame`
    ///
    /// Examples:
    /// - Framed `[u8]` contents for streaming binary messages
    /// - JSON payload streaming JSON messages
    /// - `HttpRequest` for an HttpClient connection
    type WriteFrame<'a>
    where
        Self: 'a;

    /// Returns if the given buffer contains a full frame starting at offset=0.
    ///
    /// This is used to determine if the buffer can be passed to the `deserialize_frame` function.
    /// This function is guaranteed to be called before `deserialize_frame(..)`.
    /// The `deserialize_frame(..)` function will only be called if this function returns `Ok(true)`.
    ///
    /// This function is guaranteed to be called repeatedly with the same set of growing data until `Ok(true)` or an `Err` is returned.
    /// This means that this function is able to cache any partially parsed frame information, as the next call to this function will
    /// always be for the same frame.
    fn check_deserialize_frame(&mut self, data: &[u8], eof: bool) -> Result<bool, Error>;

    /// Deserializes the given buffer into a message frame, returning the deserialized frame and serialized frame length.
    ///
    /// The lifetime of the returned data is bound to `&self` and the input `&data`.
    /// This allows for both internal `FramingStrategy`` buffering or zero-copy semantics.
    ///
    /// The data may contain extra data beyond the frame.
    /// The size returned in the [`DeserializedFrame`] struct is used to advance a read buffer.
    fn deserialize_frame<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> Result<DeserializedFrame<Self::ReadFrame<'a>>, Error>;

    /// Serialize the given frame, returning a `Vec<&[u8]>` representing the serialized frame.
    ///
    /// A `Vec<&[u8]> is used instead of a single `&[u8]` to allow for zero-copy of simple framing protocol.
    /// For example, [`U64FramingStrategy`]
    ///
    /// The lifetime of the returned data is bound to `&self`.
    /// This allows the `FramingStrategy` to parse data to an internal field and return the reference.
    fn serialize_frame<'a, 'b>(
        &'a mut self,
        data: &'a Self::WriteFrame<'b>,
    ) -> Result<Vec<&'a [u8]>, Error>;
}

/// Returns the parsed and total deserialized size frame for [`FramingStrategy`] `deserialize_frame`.
pub struct DeserializedFrame<T> {
    pub frame: T,
    pub size: usize,
}
impl<T> DeserializedFrame<T> {
    pub fn new(frame: T, size: usize) -> Self {
        Self { frame, size }
    }
}

/// A zero-copy binary [`FramingStrategy`] that adds a little-endian u64 length to the beginning of the data.
pub struct U64FramingStrategy {
    header: [u8; 8],
}
impl U64FramingStrategy {
    pub fn new() -> Self {
        Self { header: [0; 8] }
    }
}
impl FramingStrategy for U64FramingStrategy {
    type ReadFrame<'a> = &'a [u8];
    type WriteFrame<'a> = &'a [u8];
    fn serialize_frame<'a, 'b>(
        &'a mut self,
        data: &'a Self::ReadFrame<'b>,
    ) -> Result<Vec<&'a [u8]>, Error> {
        let len = u64::try_from(data.len())
            .map_err(|_| Error::new(ErrorKind::InvalidData, "frame to serialize exceeds u64"))?;
        self.header.copy_from_slice(&len.to_le_bytes());
        let mut buffers = Vec::new();
        buffers.push(self.header.as_slice());
        buffers.push(data);
        Ok(buffers)
    }

    fn check_deserialize_frame(&mut self, data: &[u8], _eof: bool) -> Result<bool, Error> {
        if data.len() < 8 {
            return Ok(false);
        }
        let len = u64::from_le_bytes(
            data[..8]
                .try_into()
                .expect("expected 8 byte slice to be 8 bytes long"),
        );
        let ulen = usize::try_from(len).map_err(|_| {
            Error::new(ErrorKind::InvalidData, "frame to deserialize exceeds usize")
        })?;
        Ok(data.len() - 8 >= ulen)
    }

    fn deserialize_frame<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> Result<DeserializedFrame<Self::WriteFrame<'a>>, Error> {
        if data.len() < 8 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "cannot deserialize partial frame",
            ));
        }
        let len = u64::from_le_bytes(
            data[..8]
                .try_into()
                .expect("expected 8 byte slice to be 8 bytes long"),
        );
        let ulen = usize::try_from(len).map_err(|_| {
            Error::new(ErrorKind::InvalidData, "frame to deserialize exceeds usize")
        })?;
        if data.len() - 8 >= ulen {
            Ok(DeserializedFrame::new(&data[8..][..ulen], 8 + ulen))
        } else {
            Err(Error::new(
                ErrorKind::InvalidData,
                "cannot deserialize partial frame",
            ))
        }
    }
}
