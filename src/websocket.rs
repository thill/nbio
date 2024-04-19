use std::{
    borrow::{Borrow, Cow},
    io::{Cursor, Error, ErrorKind},
    os::fd::FromRawFd,
};

use tcp_stream::{TLSConfig, TcpStream};
use tungstenite::{
    client::IntoClientRequest,
    error::ProtocolError,
    handshake::MidHandshake,
    protocol::{
        frame::{
            coding::{Control, Data, OpCode},
            FrameHeader,
        },
        CloseFrame,
    },
    ClientHandshake,
};

use crate::{
    frame::{DeserializedFrame, FramingSession, FramingStrategy},
    tcp::TcpSession,
    ConnectionStatus, ReadStatus, Session, WriteStatus,
};

pub type WsRequest = tungstenite::handshake::client::Request;
pub type UpgradeResponse = tungstenite::handshake::client::Response;

pub struct WebSocketSession {
    handshake: Option<WsHandshake>,
    session: Option<FramingSession<TcpSession, WebSocketFramingStrategy>>,
    write_buffer_capacity: usize,
    upgrade_response: Option<UpgradeResponse>,
}
impl WebSocketSession {
    /// Connect as a WebSocket client
    pub fn connect<I: IntoClientRequest>(
        request: I,
        tls_config: Option<TLSConfig<'_, '_, '_>>,
    ) -> Result<Self, Error> {
        let request = request
            .into_client_request()
            .map_err(|err| Error::new(ErrorKind::InvalidData, err))?;
        let stream = crate::http::connect_stream(
            request.uri().scheme().map(|x| x.as_ref()),
            request.uri().host(),
            request.uri().port().map(|x| x.as_u16()),
            tls_config.unwrap_or_default(),
        )?;
        Ok(Self {
            handshake: Some(WsHandshake::StartClientHandshake(stream, request)),
            session: None,
            write_buffer_capacity: 4096,
            upgrade_response: None,
        })
    }

    /// Set the underlying `write_buffer_capacity` to a non-default value. Defaults to `4096`.
    pub fn with_write_buffer_capacity(
        mut self,
        write_buffer_capacity: usize,
    ) -> Result<Self, Error> {
        if self.session.is_some() {
            return Err(Error::new(
                ErrorKind::Other,
                "write_buffer_capacity must be set before connection is established",
            ));
        }
        self.write_buffer_capacity = write_buffer_capacity;
        Ok(self)
    }
}
impl Session for WebSocketSession {
    type ReadData<'a> = WsMessage<'a>;
    type WriteData<'a> = WsMessage<'a>;

    fn status(&self) -> ConnectionStatus {
        match &self.session {
            None => match &self.handshake {
                None => ConnectionStatus::Closed,
                Some(_) => ConnectionStatus::Connecting,
            },
            Some(x) => x.status(),
        }
    }

    fn close(&mut self) {
        self.handshake = None;
        self.session = None;
    }

    fn flush(&mut self) -> Result<(), Error> {
        match &mut self.session {
            None => Err(Error::new(ErrorKind::NotConnected, "not connected")),
            Some(x) => x.flush(),
        }
    }

    fn drive(&mut self) -> Result<bool, Error> {
        match &mut self.session {
            None => match self.handshake.take() {
                None => Err(Error::new(ErrorKind::NotConnected, "not connected")),
                Some(handshake) => {
                    let (result, handshake) = handshake.drive()?;
                    match handshake {
                        WsHandshake::Complete(stream, response) => {
                            self.upgrade_response = Some(response);
                            self.session = Some(FramingSession::new(
                                TcpSession::new(stream)?,
                                WebSocketFramingStrategy::new(),
                                self.write_buffer_capacity,
                            ))
                        }
                        handshake => self.handshake = Some(handshake),
                    };
                    Ok(result)
                }
            },
            Some(x) => x.drive(),
        }
    }

    fn read<'a>(&'a mut self) -> Result<ReadStatus<Self::ReadData<'a>>, Error> {
        match &mut self.session {
            None => Err(Error::new(ErrorKind::NotConnected, "not connected")),
            Some(x) => match self.upgrade_response.take() {
                Some(x) => Ok(ReadStatus::Data(WsMessage::Connected(x))),
                None => x.read(),
            },
        }
    }

    fn write<'a>(
        &mut self,
        data: Self::WriteData<'a>,
    ) -> Result<WriteStatus<Self::WriteData<'a>>, Error> {
        match &mut self.session {
            None => Err(Error::new(ErrorKind::NotConnected, "not connected")),
            Some(x) => x.write(data),
        }
    }
}

#[derive(Debug, Clone)]
pub enum WsMessage<'a> {
    Connected(UpgradeResponse),
    Text(Cow<'a, str>),
    Binary(Cow<'a, [u8]>),
    Ping(Cow<'a, [u8]>),
    Pong(Cow<'a, [u8]>),
    Close(Option<CloseFrame<'a>>),
    Frame(WsFrame<'a>),
}
impl<'a> From<tungstenite::Message> for WsMessage<'a> {
    fn from(value: tungstenite::Message) -> Self {
        match value {
            tungstenite::Message::Text(x) => WsMessage::Text(Cow::Owned(x)),
            tungstenite::Message::Binary(x) => WsMessage::Binary(Cow::Owned(x)),
            tungstenite::Message::Ping(x) => WsMessage::Ping(Cow::Owned(x)),
            tungstenite::Message::Pong(x) => WsMessage::Pong(Cow::Owned(x)),
            tungstenite::Message::Close(x) => WsMessage::Close(x),
            tungstenite::Message::Frame(_) => todo!(),
        }
    }
}
impl<'a> TryFrom<WsFrame<'a>> for WsMessage<'a> {
    type Error = Error;
    fn try_from(value: WsFrame<'a>) -> Result<Self, Self::Error> {
        match value.header.opcode {
            OpCode::Data(Data::Text) => Ok(WsMessage::Text(value.payload.into())),
            OpCode::Data(Data::Binary) => Ok(WsMessage::Binary(value.payload.into())),
            OpCode::Control(Control::Ping) => Ok(WsMessage::Ping(value.payload.into())),
            OpCode::Control(Control::Pong) => Ok(WsMessage::Pong(value.payload.into())),
            OpCode::Control(Control::Close) => Ok(WsMessage::Close(parse_close_frame(
                value.payload.as_bytes(),
            )?)),
            opcode => Err(Error::new(
                ErrorKind::Other,
                format!("unrecognized opcode {opcode:?}"),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WsFrame<'a> {
    pub header: FrameHeader,
    serialized_header: Vec<u8>,
    pub payload: WsPayload<'a>,
}

#[derive(Debug, Clone)]
pub enum WsPayload<'a> {
    Str(Cow<'a, str>),
    Bytes(Cow<'a, [u8]>),
}
impl<'a> WsPayload<'a> {
    pub fn as_bytes(&'a self) -> &'a [u8] {
        match self {
            Self::Str(x) => x.as_bytes(),
            Self::Bytes(x) => x.borrow(),
        }
    }
}
impl<'a> From<Cow<'a, str>> for WsPayload<'a> {
    fn from(value: Cow<'a, str>) -> Self {
        Self::Str(value)
    }
}
impl<'a> From<Cow<'a, [u8]>> for WsPayload<'a> {
    fn from(value: Cow<'a, [u8]>) -> Self {
        Self::Bytes(value)
    }
}
impl<'a> From<WsPayload<'a>> for Cow<'a, [u8]> {
    fn from(value: WsPayload<'a>) -> Self {
        match value {
            WsPayload::Str(x) => Cow::Owned(x.as_bytes().to_vec()),
            WsPayload::Bytes(x) => x,
        }
    }
}
impl<'a> From<WsPayload<'a>> for Cow<'a, str> {
    fn from(value: WsPayload<'a>) -> Self {
        match value {
            WsPayload::Str(x) => x,
            WsPayload::Bytes(x) => Cow::Owned(String::from_utf8_lossy(x.borrow()).to_string()),
        }
    }
}

struct AssembledDataFragments {
    opcode: Data,
    payload: Vec<u8>,
}
impl AssembledDataFragments {
    pub fn new(frames: Vec<(Data, &[u8])>) -> Result<Self, Error> {
        let opcode = match frames.get(0) {
            None => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "cannot assemble zero frames",
                ))
            }
            Some(&(x, _)) => x,
        };
        let mut payload = Vec::new();
        for (_, x) in frames.into_iter() {
            payload.append(&mut x.to_vec());
        }
        Ok(Self { opcode, payload })
    }
    pub fn append(&mut self, frames: Vec<(Data, &[u8])>) {
        for (_, x) in frames.into_iter() {
            self.payload.append(&mut x.to_vec());
        }
    }
}

pub struct WebSocketFramingStrategy {
    /// optionally used to assemble fragmented data frames
    fragments: Option<AssembledDataFragments>,
}
impl WebSocketFramingStrategy {
    pub fn new() -> Self {
        Self { fragments: None }
    }
}
impl Default for WebSocketFramingStrategy {
    fn default() -> Self {
        Self::new()
    }
}
impl FramingStrategy for WebSocketFramingStrategy {
    type ReadFrame<'a> = WsMessage<'a>;
    type WriteFrame<'a> = WsMessage<'a>;

    fn check_deserialize_frame(&mut self, data: &[u8], _eof: bool) -> Result<bool, Error> {
        let mut cursor = Cursor::new(data);
        loop {
            if let Some((header, payload_size)) = FrameHeader::parse(&mut cursor)
                .map_err(|err| Error::new(ErrorKind::InvalidData, err))?
            {
                if cursor.position() + payload_size <= data.len() as u64 {
                    if header.is_final {
                        // received at least one final frame
                        return Ok(true);
                    } else {
                        cursor.set_position(cursor.position() + payload_size);
                    }
                } else {
                    return Ok(false);
                }
            }
        }
    }

    fn deserialize_frame<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> Result<DeserializedFrame<Self::ReadFrame<'a>>, Error> {
        let mut data_fragments = Vec::new();
        let mut cursor = Cursor::new(data);
        loop {
            if let Some((header, payload_size)) = FrameHeader::parse(&mut cursor)
                .map_err(|err| Error::new(ErrorKind::InvalidData, err))?
            {
                let payload_start = cursor.position();
                let payload_end = payload_start + payload_size;
                if payload_end <= data.len() as u64 {
                    let payload = &data[payload_start as usize..payload_end as usize];
                    cursor.set_position(payload_end);
                    match header.opcode {
                        OpCode::Data(opcode) => {
                            if header.is_final {
                                // final, return assembled data fragments
                                if self.fragments.is_none() && data_fragments.is_empty() {
                                    // single fragment data, return it as-is
                                    return Ok(DeserializedFrame::new(
                                        WsFrame {
                                            header,
                                            serialized_header: Vec::new(),
                                            payload: Cow::Borrowed(payload).into(),
                                        }
                                        .try_into()?,
                                        payload_end as usize,
                                    ));
                                } else {
                                    // append to other fragments, return result
                                    data_fragments.push((opcode, payload));
                                    if self.fragments.is_none() {
                                        self.fragments =
                                            Some(AssembledDataFragments::new(data_fragments)?);
                                    } else {
                                        self.fragments
                                            .as_mut()
                                            .expect("partially assembled fragments")
                                            .append(data_fragments);
                                    }
                                    let assembled_fragments =
                                        self.fragments.take().expect("fully assembled fragments");
                                    let mut header = FrameHeader::default();
                                    header.opcode = OpCode::Data(assembled_fragments.opcode);
                                    return Ok(DeserializedFrame::new(
                                        WsFrame {
                                            header,
                                            serialized_header: Vec::new(),
                                            payload: Cow::Owned::<'a, [u8]>(
                                                assembled_fragments.payload,
                                            )
                                            .into(),
                                        }
                                        .try_into()?,
                                        payload_end as usize,
                                    ));
                                }
                            } else {
                                // not final, append data fragment
                                data_fragments.push((opcode, payload));
                            }
                        }
                        OpCode::Control(_) => {
                            if !header.is_final {
                                return Err(Error::new(
                                    ErrorKind::InvalidData,
                                    "WebSocket encounted fragmented control frame",
                                ));
                            }
                            if !data_fragments.is_empty() {
                                // add encountered data fragments to state
                                if self.fragments.is_none() {
                                    self.fragments =
                                        Some(AssembledDataFragments::new(data_fragments)?);
                                }
                            }
                            // return the control frame
                            return Ok(DeserializedFrame::new(
                                WsFrame {
                                    header,
                                    serialized_header: Vec::new(),
                                    payload: Cow::Borrowed(payload).into(),
                                }
                                .try_into()?,
                                payload_end as usize,
                            ));
                        }
                    }
                }
            }
        }
    }

    fn write_frame<'a>(
        &mut self,
        frame: Self::WriteFrame<'a>,
        buffer: &mut crate::buffer::GrowableCircleBuf,
    ) -> Result<crate::WriteStatus<Self::WriteFrame<'a>>, Error> {
        let frame = match frame {
            WsMessage::Connected(_) => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "cannot write WsMessage::Connected",
                ))
            }
            WsMessage::Text(payload) => serialize_frame(OpCode::Data(Data::Text), payload)?,
            WsMessage::Binary(payload) => serialize_frame(OpCode::Data(Data::Binary), payload)?,
            WsMessage::Ping(payload) => serialize_frame(OpCode::Control(Control::Ping), payload)?,
            WsMessage::Pong(payload) => serialize_frame(OpCode::Control(Control::Pong), payload)?,
            WsMessage::Close(close_frame) => serialize_frame(
                OpCode::Control(Control::Close),
                serialize_close_frame_payload(&close_frame)?,
            )?,
            WsMessage::Frame(frame) => frame,
        };
        if frame.serialized_header.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "frame is missing serialized header",
            ));
        }
        if buffer.try_write(&vec![
            frame.serialized_header.as_slice(),
            frame.payload.as_bytes(),
        ])? {
            Ok(WriteStatus::Success)
        } else {
            Ok(WriteStatus::Pending(WsMessage::Frame(frame)))
        }
    }
}

fn serialize_frame<'a, I: Into<WsPayload<'a>>>(
    opcode: OpCode,
    payload: I,
) -> Result<WsFrame<'a>, Error> {
    let payload = payload.into();
    let mut header = FrameHeader::default();
    header.opcode = opcode;
    let mut serialized_header = Vec::new();
    header
        .format(payload.as_bytes().len() as u64, &mut serialized_header)
        .map_err(|err| Error::new(ErrorKind::Other, err))?;
    Ok(WsFrame {
        header,
        serialized_header,
        payload: payload.into(),
    })
}

fn serialize_close_frame_payload<'a, 'b>(
    close_frame: &Option<CloseFrame<'a>>,
) -> Result<Cow<'b, [u8]>, Error> {
    let mut payload = Vec::new();
    if let Some(x) = close_frame {
        payload.append(&mut u16::to_be_bytes(x.code.into()).into());
        payload.append(&mut x.reason.as_bytes().to_vec())
    };
    Ok(Cow::Owned(payload))
}

fn parse_close_frame<'a>(payload: &[u8]) -> Result<Option<CloseFrame<'a>>, Error> {
    match payload.len() {
        0 => Ok(None),
        1 => Err(Error::new(
            ErrorKind::InvalidData,
            ProtocolError::InvalidCloseSequence,
        )),
        _ => {
            let code = u16::from_be_bytes([payload[0], payload[1]]).into();
            let text = String::from_utf8_lossy(&payload[2..]).to_string();
            Ok(Some(CloseFrame {
                code,
                reason: Cow::Owned(text),
            }))
        }
    }
}

enum WsHandshake {
    StartClientHandshake(TcpStream, WsRequest),
    MidClientHandshake(MidHandshake<ClientHandshake<TcpStream>>),
    Complete(TcpStream, UpgradeResponse),
}
impl WsHandshake {
    /// drive the connection handshake
    pub fn drive(self) -> Result<(bool, Self), Error> {
        match self {
            Self::StartClientHandshake(stream, request) => {
                let mid = ClientHandshake::start(stream, request, None)
                    .map_err(|err| Error::new(ErrorKind::ConnectionAborted, err))?;
                Ok((true, Self::MidClientHandshake(mid)))
            }
            Self::MidClientHandshake(handshake) => match handshake.handshake() {
                Ok((mut ws, response)) => {
                    // TODO: avoid unsafe and need to swap with extra work doing more of the handshake process ourselves
                    let mut stream = unsafe { TcpStream::from_raw_fd(0) };
                    std::mem::swap(ws.get_mut(), &mut stream);
                    Ok((true, Self::Complete(stream, response)))
                }
                Err(err) => match err {
                    tungstenite::HandshakeError::Interrupted(mid) => {
                        Ok((false, Self::MidClientHandshake(mid)))
                    }
                    tungstenite::HandshakeError::Failure(err) => {
                        Err(Error::new(ErrorKind::ConnectionAborted, err))
                    }
                },
            },
            Self::Complete(s, r) => Ok((false, Self::Complete(s, r))),
        }
    }
}
