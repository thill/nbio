//! Provides a WebSocket [`Session`] that encapsulates an underlying [`TcpSession`]
use std::{
    borrow::Cow,
    collections::VecDeque,
    fmt::Debug,
    io::{Cursor, Error, ErrorKind},
};

use derived_from_tungstenite::MidClientHandshake;
use tcp_stream::{TLSConfig, TcpStream};
use tungstenite::{
    client::IntoClientRequest,
    error::ProtocolError,
    protocol::frame::{
        coding::{CloseCode, Control, Data, OpCode},
        FrameHeader,
    },
};

use crate::{
    frame::{DeserializeFrame, FrameDuplex, SerializeFrame, SizedFrame},
    http::Scheme,
    tcp::TcpSession,
    DriveOutcome, Flush, Publish, PublishOutcome, Receive, ReceiveOutcome, Session, SessionStatus,
};

use self::inner::{Frame, Payload};

pub type ClientRequest = tungstenite::handshake::client::Request;
pub type ClientResponse = tungstenite::handshake::client::Response;

/// Encapsulates a [`TcpSession`] in a [`FrameDuplex`] that serializes/deserializes a WebSocket [`Message`]
/// and provides WebSocket handshake functionality while [`SessionStatus::Establishing`] the session.
pub struct WebSocketSession {
    handshake: Option<PendingHandshake>,
    session: Option<FrameDuplex<TcpSession, WebSocketFrameDeserializer, WebSocketFrameSerializer>>,
    write_buffer_capacity: usize,
    upgrade_response: Option<ClientResponse>,
    automatic_pongs: bool,
    pong_queue: VecDeque<Vec<u8>>,
}
impl WebSocketSession {
    /// Connect as a WebSocket client. By default, the session will not automatically send pong responses.
    /// To enable automatic pong responses, use [`WebSocketSession::with_automatic_pongs`]
    pub fn connect<I: IntoClientRequest>(
        request: I,
        tls_config: Option<TLSConfig<'_, '_, '_>>,
    ) -> Result<Self, Error> {
        let request = request
            .into_client_request()
            .map_err(|err| Error::new(ErrorKind::InvalidData, err))?;
        let scheme = match request.uri().scheme_str() {
            None => Scheme::Http,
            Some("ws") => Scheme::Http,
            Some("wss") => Scheme::Https,
            x => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("invalid scheme {x:?}"),
                ))
            }
        };
        let stream = crate::http::connect_stream(
            scheme,
            request.uri().host(),
            request.uri().port().map(|x| x.as_u16()),
            tls_config.unwrap_or_default(),
        )?;
        Ok(Self {
            handshake: Some(PendingHandshake::StartClientHandshake(stream, request)),
            session: None,
            write_buffer_capacity: 4096,
            upgrade_response: None,
            automatic_pongs: false,
            pong_queue: VecDeque::new(),
        })
    }

    /// Set the flag that controls automatically sending pong responses
    pub fn with_automatic_pongs(mut self) -> Self {
        self.automatic_pongs = true;
        self
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

    /// After connecting as a client, this function can be used to get the initial HTTP response to the Upgrade request.
    /// When requested in any other circumstance, it will return `None`.
    pub fn upgrade_response<'a>(&'a self) -> Option<&'a ClientResponse> {
        self.upgrade_response.as_ref()
    }
}
impl Session for WebSocketSession {
    fn status(&self) -> SessionStatus {
        match &self.session {
            None => match &self.handshake {
                None => SessionStatus::Terminated,
                Some(_) => SessionStatus::Establishing,
            },
            Some(x) => x.status(),
        }
    }

    fn drive(&mut self) -> Result<DriveOutcome, Error> {
        match &mut self.session {
            None => match self.handshake.take() {
                None => Err(Error::new(ErrorKind::NotConnected, "not connected")),
                Some(handshake) => {
                    let (result, handshake) = handshake.drive()?;
                    match handshake {
                        PendingHandshake::Complete(stream, mut tail, response) => {
                            self.upgrade_response = Some(response);
                            let mut session = FrameDuplex::new(
                                TcpSession::new(stream)?,
                                WebSocketFrameDeserializer::new(),
                                WebSocketFrameSerializer::new(),
                                self.write_buffer_capacity,
                            );
                            session.read_buffer_mut().extend_from_slice(&mut tail);
                            self.session = Some(session);
                        }
                        handshake => self.handshake = Some(handshake),
                    };
                    Ok(result)
                }
            },
            Some(session) => {
                let mut outcome = DriveOutcome::Idle;
                if let Some(payload) = self.pong_queue.front() {
                    if let PublishOutcome::Published =
                        session.publish(Message::Pong(payload.into()))?
                    {
                        self.pong_queue.pop_front();
                    }
                    outcome = DriveOutcome::Active;
                }
                if session.drive()? == DriveOutcome::Active {
                    outcome = DriveOutcome::Active;
                }
                Ok(outcome)
            }
        }
    }
}
impl Receive for WebSocketSession {
    type ReceivePayload<'a> = Message<'a>;

    fn receive<'a>(&'a mut self) -> Result<ReceiveOutcome<Self::ReceivePayload<'a>>, Error> {
        match &mut self.session {
            None => Err(Error::new(ErrorKind::NotConnected, "not connected")),
            Some(session) => {
                let message = session.receive()?;
                if self.automatic_pongs {
                    if let ReceiveOutcome::Payload(Message::Ping(payload)) = &message {
                        self.pong_queue.push_back(payload.to_vec());
                    }
                }
                Ok(message)
            }
        }
    }
}
impl Publish for WebSocketSession {
    type PublishPayload<'a> = Message<'a>;

    fn publish<'a>(
        &mut self,
        data: Self::PublishPayload<'a>,
    ) -> Result<PublishOutcome<Self::PublishPayload<'a>>, Error> {
        match &mut self.session {
            None => Err(Error::new(ErrorKind::NotConnected, "not connected")),
            Some(x) => x.publish(data),
        }
    }
}
impl Flush for WebSocketSession {
    fn flush(&mut self) -> Result<(), Error> {
        match &mut self.session {
            None => Err(Error::new(ErrorKind::NotConnected, "not connected")),
            Some(x) => x.flush(),
        }
    }
}
impl Debug for WebSocketSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocketSession")
            .field("session", &self.session)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message<'a> {
    Text(Cow<'a, str>),
    Binary(Cow<'a, [u8]>),
    Ping(Cow<'a, [u8]>),
    Pong(Cow<'a, [u8]>),
    Close(Option<CloseFrame<'a>>),
    Frame(Frame<'a>),
}
impl<'a> Message<'a> {
    pub fn into_owned<'b>(self) -> Message<'b> {
        match self {
            Self::Text(x) => Message::Text(Cow::Owned(x.into_owned())),
            Self::Binary(x) => Message::Binary(Cow::Owned(x.into_owned())),
            Self::Ping(x) => Message::Ping(Cow::Owned(x.into_owned())),
            Self::Pong(x) => Message::Pong(Cow::Owned(x.into_owned())),
            Self::Close(x) => Message::Close(x.map(|x| x.into_owned())),
            Self::Frame(x) => Message::Frame(x.into_owned()),
        }
    }
}
impl<'a> TryFrom<Message<'a>> for Frame<'a> {
    type Error = Error;
    fn try_from(value: Message<'a>) -> Result<Self, Self::Error> {
        #[inline]
        fn apply_mask(buf: &mut [u8], mask: &[u8; 4]) {
            for (i, byte) in buf.iter_mut().enumerate() {
                *byte ^= mask[i & 3];
            }
        }

        let (opcode, payload) = match value {
            Message::Frame(x) => return Ok(x),
            Message::Text(x) => (OpCode::Data(Data::Text), x.into()),
            Message::Binary(x) => (OpCode::Data(Data::Binary), x.into()),
            Message::Ping(x) => (OpCode::Control(Control::Ping), x.into()),
            Message::Pong(x) => (OpCode::Control(Control::Pong), x.into()),
            Message::Close(x) => {
                let mut payload = Vec::new();
                if let Some(x) = x {
                    payload.append(&mut u16::to_be_bytes(x.code.into()).into());
                    payload.append(&mut x.reason.as_bytes().to_vec())
                };
                (
                    OpCode::Control(Control::Close),
                    Cow::<'a, [u8]>::Owned(payload).into(),
                )
            }
        };

        let mask = rand::random();
        let mut payload = match payload {
            Payload::Bytes(x) => x.into_owned(),
            Payload::Str(x) => x.into_owned().into_bytes(),
        };
        apply_mask(&mut payload, &mask);

        let mut header = FrameHeader::default();
        header.opcode = opcode;
        header.mask = Some(mask);

        let mut serialized_header = Vec::new();
        header
            .format(payload.len() as u64, &mut serialized_header)
            .map_err(|err| Error::new(ErrorKind::Other, err))?;
        Ok(Frame {
            header,
            payload: Payload::Bytes(payload.into()),
            serialized_header: Some(serialized_header),
        })
    }
}
impl<'a> From<tungstenite::Message> for Message<'a> {
    fn from(value: tungstenite::Message) -> Self {
        match value {
            tungstenite::Message::Text(x) => Message::Text(Cow::Owned(x)),
            tungstenite::Message::Binary(x) => Message::Binary(Cow::Owned(x)),
            tungstenite::Message::Ping(x) => Message::Ping(Cow::Owned(x)),
            tungstenite::Message::Pong(x) => Message::Pong(Cow::Owned(x)),
            tungstenite::Message::Close(x) => Message::Close(x.map(|x| x.into())),
            tungstenite::Message::Frame(_) => todo!(),
        }
    }
}
impl<'a> TryFrom<Frame<'a>> for Message<'a> {
    type Error = Error;
    fn try_from(value: Frame<'a>) -> Result<Self, Self::Error> {
        match value.header.opcode {
            OpCode::Data(Data::Text) => Ok(Message::Text(value.payload.into())),
            OpCode::Data(Data::Binary) => Ok(Message::Binary(value.payload.into())),
            OpCode::Control(Control::Ping) => Ok(Message::Ping(value.payload.into())),
            OpCode::Control(Control::Pong) => Ok(Message::Pong(value.payload.into())),
            OpCode::Control(Control::Close) => {
                Ok(Message::Close(parse_close_frame(value.payload.as_bytes())?))
            }
            opcode => Err(Error::new(
                ErrorKind::Other,
                format!("unrecognized opcode {opcode:?}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CloseFrame<'a> {
    pub code: CloseCode,
    pub reason: Cow<'a, str>,
}
impl<'a> CloseFrame<'a> {
    pub fn into_owned<'b>(self) -> CloseFrame<'b> {
        CloseFrame {
            code: self.code,
            reason: Cow::Owned(self.reason.into_owned()),
        }
    }
}
impl<'a> From<tungstenite::protocol::CloseFrame<'a>> for CloseFrame<'a> {
    fn from(value: tungstenite::protocol::CloseFrame<'a>) -> Self {
        Self {
            code: value.code,
            reason: value.reason,
        }
    }
}

pub(crate) mod inner {
    use std::borrow::{Borrow, Cow};

    use tungstenite::protocol::frame::FrameHeader;

    #[derive(Debug, Clone, PartialEq)]
    pub struct Frame<'a> {
        pub header: FrameHeader,
        pub payload: Payload<'a>,
        pub serialized_header: Option<Vec<u8>>,
    }
    impl<'a> Frame<'a> {
        pub fn into_owned<'b>(self) -> Frame<'b> {
            Frame {
                header: self.header,
                payload: self.payload.into_owned(),
                serialized_header: self.serialized_header,
            }
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum Payload<'a> {
        Str(Cow<'a, str>),
        Bytes(Cow<'a, [u8]>),
    }
    impl<'a> Payload<'a> {
        pub fn as_bytes(&'a self) -> &'a [u8] {
            match self {
                Self::Str(x) => x.as_bytes(),
                Self::Bytes(x) => x.borrow(),
            }
        }
        pub fn into_owned<'b>(self) -> Payload<'b> {
            match self {
                Self::Str(x) => Payload::Str(Cow::Owned(x.into_owned())),
                Self::Bytes(x) => Payload::Bytes(Cow::Owned(x.into_owned())),
            }
        }
    }
    impl<'a> From<Cow<'a, str>> for Payload<'a> {
        fn from(value: Cow<'a, str>) -> Self {
            Self::Str(value)
        }
    }
    impl<'a> From<Cow<'a, [u8]>> for Payload<'a> {
        fn from(value: Cow<'a, [u8]>) -> Self {
            Self::Bytes(value)
        }
    }
    impl<'a> From<Payload<'a>> for Cow<'a, [u8]> {
        fn from(value: Payload<'a>) -> Self {
            match value {
                Payload::Str(x) => Cow::Owned(x.as_bytes().to_vec()),
                Payload::Bytes(x) => x,
            }
        }
    }
    impl<'a> From<Payload<'a>> for Cow<'a, str> {
        fn from(value: Payload<'a>) -> Self {
            match value {
                Payload::Str(x) => x,
                Payload::Bytes(x) => Cow::Owned(String::from_utf8_lossy(x.borrow()).to_string()),
            }
        }
    }
}

struct FragmentBuffer {
    opcode: Data,
    payload: Vec<u8>,
}
impl FragmentBuffer {
    pub fn new(opcode: Data) -> Self {
        Self {
            opcode,
            payload: Vec::new(),
        }
    }
    pub fn append(&mut self, frame: &[u8]) {
        self.payload.append(&mut frame.to_vec());
    }
}

pub struct WebSocketFrameDeserializer {
    /// optionally used to assemble fragmented data frames
    fragments: Option<FragmentBuffer>,
}
impl WebSocketFrameDeserializer {
    pub fn new() -> Self {
        Self { fragments: None }
    }
}
impl Default for WebSocketFrameDeserializer {
    fn default() -> Self {
        Self::new()
    }
}
impl DeserializeFrame for WebSocketFrameDeserializer {
    type DeserializedFrame<'a> = Message<'a>;

    fn check_deserialize_frame(&mut self, data: &[u8], _eof: bool) -> Result<bool, Error> {
        let mut cursor = Cursor::new(data);
        loop {
            match FrameHeader::parse(&mut cursor)
                .map_err(|err| Error::new(ErrorKind::InvalidData, err))?
            {
                None => return Ok(false),
                Some((header, payload_size)) => {
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
    }

    fn deserialize_frame<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> Result<SizedFrame<Self::DeserializedFrame<'a>>, Error> {
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
                                if self.fragments.is_none() {
                                    // single fragment data, return borrowed result
                                    return Ok(SizedFrame::new(
                                        Frame {
                                            header,
                                            serialized_header: None,
                                            payload: Cow::Borrowed(payload).into(),
                                        }
                                        .try_into()?,
                                        payload_end as usize,
                                    ));
                                } else {
                                    // append to other fragments, return owned result
                                    let mut fragments = self
                                        .fragments
                                        .take()
                                        .unwrap_or_else(|| FragmentBuffer::new(opcode));
                                    fragments.append(data);
                                    let mut header = FrameHeader::default();
                                    header.opcode = OpCode::Data(fragments.opcode);
                                    return Ok(SizedFrame::new(
                                        Frame {
                                            header,
                                            serialized_header: None,
                                            payload: Cow::Owned::<'a, [u8]>(fragments.payload)
                                                .into(),
                                        }
                                        .try_into()?,
                                        payload_end as usize,
                                    ));
                                }
                            } else {
                                // not final, append pending data fragment to state
                                let mut fragments = self
                                    .fragments
                                    .take()
                                    .unwrap_or_else(|| FragmentBuffer::new(opcode));
                                fragments.append(payload);
                                self.fragments = Some(fragments);
                            }
                        }
                        OpCode::Control(_) => {
                            if !header.is_final {
                                return Err(Error::new(
                                    ErrorKind::InvalidData,
                                    "WebSocket encounted fragmented control frame",
                                ));
                            }
                            // return the control frame
                            return Ok(SizedFrame::new(
                                Frame {
                                    header,
                                    serialized_header: None,
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
}

pub struct WebSocketFrameSerializer {}
impl WebSocketFrameSerializer {
    pub fn new() -> Self {
        Self {}
    }
}
impl Default for WebSocketFrameSerializer {
    fn default() -> Self {
        Self::new()
    }
}
impl SerializeFrame for WebSocketFrameSerializer {
    type SerializedFrame<'a> = Message<'a>;
    fn serialize_frame<'a>(
        &mut self,
        frame: Self::SerializedFrame<'a>,
        buffer: &mut crate::buffer::GrowableCircleBuf,
    ) -> Result<crate::PublishOutcome<Self::SerializedFrame<'a>>, Error> {
        let frame: Frame<'a> = frame.try_into()?;
        let serialized_header = match &frame.serialized_header {
            Some(x) => x,
            None => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "frame is missing serialized header",
                ))
            }
        };
        if buffer.try_write(&vec![
            serialized_header.as_slice(),
            frame.payload.as_bytes(),
        ])? {
            Ok(PublishOutcome::Published)
        } else {
            Ok(PublishOutcome::Incomplete(Message::Frame(frame)))
        }
    }
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

enum PendingHandshake {
    StartClientHandshake(TcpStream, ClientRequest),
    MidClientHandshake(MidClientHandshake),
    Complete(TcpStream, Vec<u8>, ClientResponse),
}
impl PendingHandshake {
    /// drive the connection handshake
    pub fn drive(self) -> Result<(DriveOutcome, Self), Error> {
        match self {
            Self::StartClientHandshake(stream, request) => {
                let mid = MidClientHandshake::start(stream, request)
                    .map_err(|err| Error::new(ErrorKind::Other, err))?;
                Ok((DriveOutcome::Active, Self::MidClientHandshake(mid)))
            }
            Self::MidClientHandshake(handshake) => match handshake
                .drive()
                .map_err(|err| Error::new(ErrorKind::Other, err))?
            {
                MidHandshakeOutcome::InProgress(mid, outcome) => {
                    Ok((outcome, Self::MidClientHandshake(mid)))
                }
                MidHandshakeOutcome::Complete(stream, tail, response) => {
                    Ok((DriveOutcome::Active, Self::Complete(stream, tail, response)))
                }
            },
            Self::Complete(stream, tail, response) => {
                Ok((DriveOutcome::Active, Self::Complete(stream, tail, response)))
            }
        }
    }
}

enum MidHandshakeOutcome {
    InProgress(MidClientHandshake, DriveOutcome),
    Complete(TcpStream, Vec<u8>, tungstenite::handshake::client::Response),
}

/// derived from code in the tungstenite crate to facilitate handshakes
mod derived_from_tungstenite {
    use tcp_stream::TcpStream;
    use tungstenite::{
        error::ProtocolError,
        handshake::{
            client::{self, generate_request, Response},
            derive_accept_key,
            machine::{HandshakeMachine, RoundResult, StageResult},
            ProcessingResult,
        },
        http::StatusCode,
        Error,
    };

    use crate::DriveOutcome;

    use super::MidHandshakeOutcome;

    pub struct MidClientHandshake {
        accept_key: String,
        machine: HandshakeMachine<TcpStream>,
    }
    impl MidClientHandshake {
        pub fn start(stream: TcpStream, request: client::Request) -> Result<Self, Error> {
            if request.method() != tungstenite::http::Method::GET {
                return Err(Error::Protocol(ProtocolError::WrongHttpMethod));
            }
            if request.version() < tungstenite::http::Version::HTTP_11 {
                return Err(Error::Protocol(ProtocolError::WrongHttpVersion));
            }
            let _ = tungstenite::client::uri_mode(request.uri())?;
            let (request, key) = generate_request(request)?;
            let machine = HandshakeMachine::start_write(stream, request);
            let accept_key = derive_accept_key(key.as_ref());
            Ok(MidClientHandshake {
                accept_key,
                machine,
            })
        }

        pub fn drive(self) -> Result<MidHandshakeOutcome, Error> {
            let accept_key = self.accept_key;
            match self.machine.single_round()? {
                RoundResult::WouldBlock(machine) | RoundResult::Incomplete(machine) => {
                    return Ok(MidHandshakeOutcome::InProgress(
                        Self {
                            accept_key,
                            machine,
                        },
                        DriveOutcome::Idle,
                    ))
                }
                RoundResult::StageFinished(s) => match Self::stage_finished(&accept_key, s)? {
                    tungstenite::handshake::ProcessingResult::Continue(machine) => {
                        return Ok(MidHandshakeOutcome::InProgress(
                            Self {
                                accept_key,
                                machine,
                            },
                            DriveOutcome::Idle,
                        ))
                    }
                    tungstenite::handshake::ProcessingResult::Done((stream, tail, response)) => {
                        return Ok(MidHandshakeOutcome::Complete(stream, tail, response))
                    }
                },
            }
        }

        fn stage_finished(
            accept_key: &str,
            finish: StageResult<Response, TcpStream>,
        ) -> Result<ProcessingResult<TcpStream, (TcpStream, Vec<u8>, Response)>, tungstenite::Error>
        {
            Ok(match finish {
                StageResult::DoneWriting(stream) => {
                    ProcessingResult::Continue(HandshakeMachine::start_read(stream))
                }
                StageResult::DoneReading {
                    stream,
                    result,
                    tail,
                } => {
                    let result = match Self::verify_response(accept_key, result) {
                        Ok(r) => r,
                        Err(Error::Http(mut e)) => {
                            *e.body_mut() = Some(tail);
                            return Err(Error::Http(e));
                        }
                        Err(e) => return Err(e),
                    };
                    ProcessingResult::Done((stream, tail, result))
                }
            })
        }

        fn verify_response(accept_key: &str, response: Response) -> Result<Response, Error> {
            if response.status() != StatusCode::SWITCHING_PROTOCOLS {
                return Err(Error::Http(response));
            }
            let headers = response.headers();
            if !headers
                .get("Upgrade")
                .and_then(|h| h.to_str().ok())
                .map(|h| h.eq_ignore_ascii_case("websocket"))
                .unwrap_or(false)
            {
                return Err(Error::Protocol(
                    ProtocolError::MissingUpgradeWebSocketHeader,
                ));
            }
            if !headers
                .get("Connection")
                .and_then(|h| h.to_str().ok())
                .map(|h| h.eq_ignore_ascii_case("Upgrade"))
                .unwrap_or(false)
            {
                return Err(Error::Protocol(
                    ProtocolError::MissingConnectionUpgradeHeader,
                ));
            }
            if !headers
                .get("Sec-WebSocket-Accept")
                .map(|h| h == accept_key)
                .unwrap_or(false)
            {
                return Err(Error::Protocol(
                    ProtocolError::SecWebSocketAcceptKeyMismatch,
                ));
            }
            Ok(response)
        }
    }
}
