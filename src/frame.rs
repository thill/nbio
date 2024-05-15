//! [`Receive`] and [`Publish`] implementations that serialize and deserialize payloads on underlying binary sessions.
//!
//! ## Implementations
//!
//! - [`FramePublisher`] will "frame" and publish payloads on an underlying binary [`Session`] that can [`Publish`].
//! - [`FrameReceiver`] will receive and deserialize from a binary [`Session`] that can [`Receive`].
//! - [`FrameDuplex`] acts as a combo [`FramePublisher`] and [`FrameReceiver`] for bi-directional sessions.
//!
//! The [`DeserializeFrame`] and [`SerializeFrame`] traits provided in this module enable a convenient way to convert
//! data from an underlying binary streaming protocol back and forth with parsed events.

use std::{
    fmt::Debug,
    io::{Error, ErrorKind},
    mem::swap,
};

use crate::{
    buffer::GrowableCircleBuf, DriveOutcome, Flush, Publish, PublishOutcome, Receive,
    ReceiveOutcome, Session, SessionStatus,
};

/// # FrameDuplex
///
/// Encapsulates a binary bi-directional streaming [`Session`], providing framing using the given [`SerializeFrame`] and [`DeserializeFrame`] traits.
/// The underlying [`Publish::PublishPayload`] and [`Receive::ReceivePayload`] must be `[u8]`.
///
/// This effectively translates any `Session<ReceivePayload=[u8], PublishPayload=[u8]>` to a `Session<ReceivePayload=YourStruct, PublishPayload=YourStruct>`
/// provided apporpriate [`SerializeFrame`] and [`DeserializeFrame`] impls.
///
/// ## Drive
///
/// It is imperative that the `drive` function be called regularly in this implementation.
/// This function is responsible for writing bytes from the internal write buffer to the underlying [`Session`].
///
/// ## Publish Buffering
///
/// Frames passed into `publish` are serialized using the [`SerializeFrame`] impl and are copied to an internal circular buffer.
/// The internal circular buffer is initialized with a capacity given to the `new` function.
///
/// If the entire frame is not able to fit in the remaining circular buffer space, none of the frame will be copied.
/// In this case, [`PublishOutcome::Incomplete`] will be returned with a reference to the frame to be retried later.
///
/// The circular buffer will grow to fit frames that exceed the entire circular buffer capacity, but only when the buffer is empty.
/// This allows for any sized frame to be successfuly published while avoiding the circular buffer growing the unreasonable sizes.
///
/// ## Receive Buffering
///
/// Data is received from the underlying binary [`Receive`] [`Session`] into an internal [`Vec<u8>`].
/// Each call to receive will first check if the read buffer contains a full frame to be returned immediately.
/// The underlying [`Receive::receive`] function will only be called when the read buffer is devoid of a full frame.
/// This avoids the buffer growing to an unreasonable size, as it will only ever grow to the size of a frame plus the next read size.
pub struct FrameDuplex<S, DF, SF> {
    session: S,
    deserialize_frame: DF,
    serialize_frame: SF,
    write_buffer: GrowableCircleBuf,
    read_buffer: Vec<u8>,
    read_advance: usize,
}
impl<S, DF, SF> FrameDuplex<S, DF, SF>
where
    S: for<'a> Publish<PublishPayload<'a> = &'a [u8]>
        + for<'a> Receive<ReceivePayload<'a> = &'a [u8]>
        + 'static,
    DF: DeserializeFrame + 'static,
    SF: SerializeFrame + 'static,
{
    /// Create a new [`FrameDuplex`]
    ///
    /// # Parameters
    /// - `session`: The underlying binary [`Session`]
    /// - `deserialize_frame`: The strategy used to convert binary data to framed messages
    /// - `serialize_frame`: The strategy used to convert framed messages to binary data
    /// - `write_buffer_capacity`: The capacity, **in bytes**, of the underlying circular buffer that holds serialized write frames
    pub fn new(
        session: S,
        deserialize_frame: DF,
        serialize_frame: SF,
        write_buffer_capacity: usize,
    ) -> Self {
        Self {
            session,
            deserialize_frame,
            serialize_frame,
            write_buffer: GrowableCircleBuf::new(write_buffer_capacity)
                .unwrap_or_else(|_| GrowableCircleBuf::new(usize::MAX / 2).unwrap()),
            read_buffer: Vec::new(),
            read_advance: 0,
        }
    }
}
impl<S, DF, SF> Session for FrameDuplex<S, DF, SF>
where
    S: for<'a> Publish<PublishPayload<'a> = &'a [u8]> + 'static,
    DF: DeserializeFrame + 'static,
    SF: SerializeFrame + 'static,
{
    fn status(&self) -> crate::SessionStatus {
        self.session.status()
    }

    fn close(&mut self) {
        self.session.close()
    }

    fn drive(&mut self) -> Result<DriveOutcome, std::io::Error> {
        let mut outcome = self.session.drive()?;
        if self.write_buffer.is_empty() {
            return Ok(outcome);
        }
        let write_buffer = self.write_buffer.peek_read();
        let wrote_len = match self.session.publish(write_buffer)? {
            PublishOutcome::Published => write_buffer.len(),
            PublishOutcome::Incomplete(pending) => write_buffer.len() - pending.len(),
        };
        self.write_buffer.advance_read(wrote_len)?;
        if wrote_len > 0 {
            outcome = DriveOutcome::Active;
        }
        Ok(outcome)
    }
}
impl<S, DF, SF> Publish for FrameDuplex<S, DF, SF>
where
    S: for<'a> Publish<PublishPayload<'a> = &'a [u8]> + 'static,
    DF: DeserializeFrame + 'static,
    SF: SerializeFrame + 'static,
{
    type PublishPayload<'a> = SF::SerializedFrame<'a>;
    fn publish<'a>(
        &mut self,
        frame: Self::PublishPayload<'a>,
    ) -> Result<PublishOutcome<Self::PublishPayload<'a>>, Error> {
        if self.session.status() != SessionStatus::Established {
            return Err(Error::new(
                ErrorKind::NotConnected,
                "underlying session is not established",
            ));
        }
        self.serialize_frame
            .serialize_frame(frame, &mut self.write_buffer)
    }
}
impl<S, DF, SF> Flush for FrameDuplex<S, DF, SF>
where
    S: for<'a> Publish<PublishPayload<'a> = &'a [u8]> + Flush + 'static,
    DF: DeserializeFrame + 'static,
    SF: SerializeFrame + 'static,
{
    fn flush(&mut self) -> Result<(), std::io::Error> {
        while !self.write_buffer.is_empty() {
            self.drive()?;
        }
        self.session.flush()
    }
}
impl<S, DF, SF> Receive for FrameDuplex<S, DF, SF>
where
    S: for<'a> Publish<PublishPayload<'a> = &'a [u8]>
        + for<'a> Receive<ReceivePayload<'a> = &'a [u8]>
        + 'static,
    DF: DeserializeFrame + 'static,
    SF: SerializeFrame + 'static,
{
    type ReceivePayload<'a> = DF::DeserializedFrame<'a>;
    fn receive<'a>(
        &'a mut self,
    ) -> Result<ReceiveOutcome<Self::ReceivePayload<'a>>, std::io::Error> {
        if self.read_advance != 0 {
            let mut new_buf = Vec::from(&self.read_buffer[self.read_advance..]);
            self.read_advance = 0;
            swap(&mut new_buf, &mut self.read_buffer);
        }
        // try deserializing before receiving to avoid growing the buffer forever in a slow consumer
        if self
            .deserialize_frame
            .check_deserialize_frame(&self.read_buffer, false)?
        {
            let de = self
                .deserialize_frame
                .deserialize_frame(&self.read_buffer)?;
            self.read_advance = de.size;
            return Ok(ReceiveOutcome::Payload(de.frame));
        }
        let data = match self.session.receive()? {
            ReceiveOutcome::Payload(data) => data,
            ReceiveOutcome::Buffered => return Ok(ReceiveOutcome::Buffered),
            ReceiveOutcome::Idle => return Ok(ReceiveOutcome::Idle),
        };
        self.read_buffer.extend_from_slice(data);
        Ok(ReceiveOutcome::Buffered)
    }
}
impl<S, DF, SF> Debug for FrameDuplex<S, DF, SF>
where
    S: for<'a> Session + 'static,
    DF: DeserializeFrame + 'static,
    SF: SerializeFrame + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameDuplex")
            .field("session", &self.session)
            .finish()
    }
}

/// # FramePublisher
///
/// Encapsulates a binary streaming [`Session`], providing framing using the given [`SerializeFrame`] trait.
/// The underlying [`Publish::PublishPayload`] must be `[u8]`.
///
/// This effectively translates any `Publish<PublishPayload=[u8]>` to a `Publish<PublishPayload=YourStruct>`
/// provided an apporpriate [`SerializeFrame`] impls.
///
/// ## Drive
///
/// It is imperative that the `drive` function be called regularly in this implementation.
/// This function is responsible for writing bytes from the internal write buffer to the underlying [`Session`].
///
/// ## Publish Buffering
///
/// Frames passed into `publish` are serialized using the [`SerializeFrame`] impl and are copied to an internal circular buffer.
/// The internal circular buffer is initialized with a capacity given to the `new` function.
///
/// If the entire frame is not able to fit in the remaining circular buffer space, none of the frame will be copied.
/// In this case, [`PublishOutcome::Incomplete`] will be returned with a reference to the frame to be retried later.
///
/// The circular buffer will grow to fit frames that exceed the entire circular buffer capacity, but only when the buffer is empty.
/// This allows for any sized frame to be successfuly published while avoiding the circular buffer growing the unreasonable sizes.
pub struct FramePublisher<S, F> {
    session: S,
    framing_strategy: F,
    write_buffer: GrowableCircleBuf,
}
impl<S, F> FramePublisher<S, F>
where
    S: for<'a> Publish<PublishPayload<'a> = &'a [u8]> + 'static,
    F: SerializeFrame + 'static,
{
    /// Create a new [`FramePublisher`]
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
        }
    }
}
impl<S, F> Session for FramePublisher<S, F>
where
    S: for<'a> Publish<PublishPayload<'a> = &'a [u8]> + 'static,
    F: SerializeFrame + 'static,
{
    fn status(&self) -> crate::SessionStatus {
        self.session.status()
    }

    fn close(&mut self) {
        self.session.close()
    }

    fn drive(&mut self) -> Result<DriveOutcome, std::io::Error> {
        let mut outcome = self.session.drive()?;
        if self.write_buffer.is_empty() {
            return Ok(outcome);
        }
        let write_buffer = self.write_buffer.peek_read();
        let wrote_len = match self.session.publish(write_buffer)? {
            PublishOutcome::Published => write_buffer.len(),
            PublishOutcome::Incomplete(pending) => write_buffer.len() - pending.len(),
        };
        self.write_buffer.advance_read(wrote_len)?;
        if wrote_len > 0 {
            outcome = DriveOutcome::Active;
        }
        Ok(outcome)
    }
}
impl<S, F> Publish for FramePublisher<S, F>
where
    S: for<'a> Publish<PublishPayload<'a> = &'a [u8]> + 'static,
    F: SerializeFrame + 'static,
{
    type PublishPayload<'a> = F::SerializedFrame<'a>;
    fn publish<'a>(
        &mut self,
        frame: Self::PublishPayload<'a>,
    ) -> Result<PublishOutcome<Self::PublishPayload<'a>>, Error> {
        if self.session.status() != SessionStatus::Established {
            return Err(Error::new(
                ErrorKind::NotConnected,
                "underlying session is not established",
            ));
        }
        self.framing_strategy
            .serialize_frame(frame, &mut self.write_buffer)
    }
}
impl<S, F> Flush for FramePublisher<S, F>
where
    S: for<'a> Publish<PublishPayload<'a> = &'a [u8]> + Flush + 'static,
    F: SerializeFrame + 'static,
{
    fn flush(&mut self) -> Result<(), std::io::Error> {
        while !self.write_buffer.is_empty() {
            self.drive()?;
        }
        self.session.flush()
    }
}
impl<S, F> Debug for FramePublisher<S, F>
where
    S: for<'a> Session + 'static,
    F: SerializeFrame + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramePublisher")
            .field("session", &self.session)
            .finish()
    }
}

/// # FrameReceiver
///
/// Encapsulates a binary streaming [`Session`], providing framing using the given [`DeserializeFrame`] trait.
/// The underlying [`Receive::ReceivePayload`] must be `[u8]`.
///
/// This effectively translates any `Receive<ReceivePayload=[u8]>` to a `Receive<ReceivePayload=YourStruct>`
/// provided an apporpriate [`DeserializeFrame`] impl.
///
/// ## Receive Buffering
///
/// Data is received from the underlying binary [`Receive`] [`Session`] into an internal [`Vec<u8>`].
/// Each call to receive will first check if the read buffer contains a full frame to be returned immediately.
/// The underlying [`Receive::receive`] function will only be called when the read buffer is devoid of a full frame.
/// This avoids the buffer growing to an unreasonable size, as it will only ever grow to the size of a frame plus the next read size.
pub struct FrameReceiver<S, F> {
    session: S,
    deserialize_frame: F,
    read_buffer: Vec<u8>,
    read_advance: usize,
}
impl<S, F> FrameReceiver<S, F>
where
    S: Session + 'static,
    F: DeserializeFrame + 'static,
{
    /// Create a new [`FrameReceiver`]
    ///
    /// # Parameters
    /// - `session`: The underlying binary [`Session`]
    /// - `deserialize_frame`: The strategy used to convert binary data back and forth to framed messages
    pub fn new(session: S, deserialize_frame: F) -> Self {
        Self {
            session,
            deserialize_frame,
            read_buffer: Vec::new(),
            read_advance: 0,
        }
    }
}
impl<S, F> Session for FrameReceiver<S, F>
where
    S: for<'a> Session + 'static,
    F: DeserializeFrame + 'static,
{
    fn status(&self) -> crate::SessionStatus {
        self.session.status()
    }

    fn close(&mut self) {
        self.session.close()
    }

    fn drive(&mut self) -> Result<DriveOutcome, std::io::Error> {
        self.session.drive()
    }
}
impl<S, F> Receive for FrameReceiver<S, F>
where
    S: for<'a> Receive<ReceivePayload<'a> = &'a [u8]> + 'static,
    F: DeserializeFrame + 'static,
{
    type ReceivePayload<'a> = F::DeserializedFrame<'a>;
    fn receive<'a>(
        &'a mut self,
    ) -> Result<ReceiveOutcome<Self::ReceivePayload<'a>>, std::io::Error> {
        if self.read_advance != 0 {
            let mut new_buf = Vec::from(&self.read_buffer[self.read_advance..]);
            self.read_advance = 0;
            swap(&mut new_buf, &mut self.read_buffer);
        }
        // try deserializing before receiving to avoid growing the buffer forever in a slow consumer
        if self
            .deserialize_frame
            .check_deserialize_frame(&self.read_buffer, false)?
        {
            let de = self
                .deserialize_frame
                .deserialize_frame(&self.read_buffer)?;
            self.read_advance = de.size;
            return Ok(ReceiveOutcome::Payload(de.frame));
        }
        let data = match self.session.receive()? {
            ReceiveOutcome::Payload(data) => data,
            ReceiveOutcome::Buffered => return Ok(ReceiveOutcome::Buffered),
            ReceiveOutcome::Idle => return Ok(ReceiveOutcome::Idle),
        };
        self.read_buffer.extend_from_slice(data);
        Ok(ReceiveOutcome::Buffered)
    }
}
impl<S, F> Debug for FrameReceiver<S, F>
where
    S: for<'a> Session + 'static,
    F: DeserializeFrame + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameReceiver")
            .field("session", &self.session)
            .finish()
    }
}

/// Deserialize frames using buffer slices.
/// This is used by a [`FramingSession`] to read/write frames using a raw binary [`Session`].
pub trait DeserializeFrame {
    /// Type returned by `deserialize_frame`
    ///
    /// Examples:
    /// - Framed `[u8]` contents for streaming binary messages
    /// - JSON payload for streaming JSON messages
    /// - `HttpResponse` for an HttpClient connection
    type DeserializedFrame<'a>
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
    ) -> Result<SizedFrame<Self::DeserializedFrame<'a>>, Error>;
}

pub trait SerializeFrame {
    /// Type returned by `serialize_frame`
    ///
    /// Examples:
    /// - Framed `[u8]` contents for streaming binary messages
    /// - JSON payload streaming JSON messages
    /// - `HttpRequest` for an HttpClient connection
    type SerializedFrame<'a>
    where
        Self: 'a;

    /// Serialize and write the given frame to the given [`GrowableCircleBuf`], returning the appropriate PublishOutcome.
    ///
    /// The lifetime of the returned data is bound to `&self`.
    /// This allows the `FramingStrategy` to parse data to an internal field and return the reference.
    fn serialize_frame<'a>(
        &mut self,
        frame: Self::SerializedFrame<'a>,
        buffer: &mut GrowableCircleBuf,
    ) -> Result<PublishOutcome<Self::SerializedFrame<'a>>, Error>;
}

/// Returns the parsed and total deserialized size frame for [`FramingStrategy`] `deserialize_frame`.
pub struct SizedFrame<T> {
    pub frame: T,
    pub size: usize,
}
impl<T> SizedFrame<T> {
    pub fn new(frame: T, size: usize) -> Self {
        Self { frame, size }
    }
}

/// A zero-copy binary [`SerializeFrame`] implementation that adds a little-endian u64 length to the beginning of the data.
pub struct U64FrameSerializer {
    header: [u8; 8],
}
impl U64FrameSerializer {
    pub fn new() -> Self {
        Self { header: [0; 8] }
    }
}
impl SerializeFrame for U64FrameSerializer {
    type SerializedFrame<'a> = &'a [u8];

    fn serialize_frame<'a>(
        &mut self,
        data: Self::SerializedFrame<'a>,
        write_buffer: &mut GrowableCircleBuf,
    ) -> Result<PublishOutcome<Self::SerializedFrame<'a>>, Error> {
        let len = u64::try_from(data.len())
            .map_err(|_| Error::new(ErrorKind::InvalidData, "frame to serialize exceeds u64"))?;
        self.header.copy_from_slice(&len.to_le_bytes());
        if write_buffer.try_write(&vec![self.header.as_slice(), data])? {
            Ok(PublishOutcome::Published)
        } else {
            Ok(PublishOutcome::Incomplete(data))
        }
    }
}

/// A zero-copy binary [`DeserializeFrame`] implementation that utilizes a little-endian u64 length at the beginning of the data.
pub struct U64FrameDeserializer {}
impl U64FrameDeserializer {
    pub fn new() -> Self {
        Self {}
    }
}
impl DeserializeFrame for U64FrameDeserializer {
    type DeserializedFrame<'a> = &'a [u8];

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
    ) -> Result<SizedFrame<Self::DeserializedFrame<'a>>, Error> {
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
            Ok(SizedFrame::new(&data[8..][..ulen], 8 + ulen))
        } else {
            Err(Error::new(
                ErrorKind::InvalidData,
                "cannot deserialize partial frame",
            ))
        }
    }
}
