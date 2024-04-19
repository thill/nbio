//! A non-blocking HTTP client
use std::{
    io::{self, Error, ErrorKind, Read},
    mem::swap,
    str::FromStr,
};

use hyperium_http::{
    header::{CONTENT_LENGTH, HOST, TRANSFER_ENCODING},
    Response,
};
use tcp_stream::{OwnedTLSConfig, TLSConfig, TcpStream};

use crate::{
    buffer::GrowableCircleBuf,
    frame::{DeserializedFrame, FramingSession, FramingStrategy},
    tcp::TcpSession,
    ConnectionStatus, Session, WriteStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    Http,
    Https,
}
impl Scheme {
    pub fn default_port(&self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// An HTTP Request, which can be represented by a [`hyperium_http::Request`] or a serialized HTTP payload.
///
/// [`HttpRequest::Serialized`] is utilized to prevent needing to serialize a payload more than one time in
/// the event it needs to be retried due to back-pressure.
pub enum HttpRequest {
    Request(hyperium_http::Request<Vec<u8>>),
    Serialized(Vec<u8>),
}
impl<I: IntoBody> From<hyperium_http::Request<I>> for HttpRequest {
    fn from(value: hyperium_http::Request<I>) -> Self {
        let (parts, body) = value.into_parts();
        HttpRequest::Request(hyperium_http::Request::from_parts(parts, body.into_body()))
    }
}

/// A simple non-blocking HTTP 1.x client
///
/// Calling `connect(..)` or `request(..)` will return a [`HttpClientSession`], which encapsulates a [`FramingSession`] utilizing an HTTP [`FramingStrategy`].
/// The framing strategy utilizes Hyperium's [`http`] lib for [`hyperium_http::Request`] and [`hyperium_http::Response`] structs.
///
/// The returned [`HttpClientSession`] will have pre-setup TLS for `https` URLs, and will pre-buffer the serialized request.
/// Calls to `drive(..)` will perform the TLS handshake and flush the pending request.
/// Calls to `read(..)` will buffer the response, and return a deserialized [`hyperium_http::Response`].
///
/// For now, this only supports HTTP 1.x.
///
/// ## Functions
///
/// [`HttpClient::request`] will open a [`HttpClientSession`] for the given [`hyperium_http::Request`], buffering the request, which can be driven and read to completion.
/// To open a connection without an immeidate pending [`hyperium_http::Request`], use [`HttpClient::connect`], which simply opens a persistent connection.
///
/// [`HttpClient::connect`] will open a persistent [`HttpClientSession`] to the given domain. This connection must call [`Session::drive()`] until [`Session::status`]
/// returns [`ConnectionStatus::Connected`], at which point the session can send multiple [`hyperium_http::Request`] payloads and receive [`hyperium_http::Response`] payloads utilizing HTTP "keep-alive".
///
/// ## Example
///
/// ```no_run
/// use nbio::{Session, ReadStatus};
/// use nbio::http::HttpClient;
/// use nbio::hyperium_http::Request;
/// use nbio::tcp_stream::OwnedTLSConfig;
///
/// // create the client and make the request
/// let mut client = HttpClient::new();
/// let mut conn = client
///     .request(Request::get("http://icanhazip.com").body(()).unwrap())
///     .unwrap();
///
/// // drive and read the conn until a full response is received
/// loop {
///     conn.drive().unwrap();
///     if let ReadStatus::Data(r) = conn.read().unwrap() {
///         // validate the response
///         println!("Response Body: {}", String::from_utf8_lossy(r.body()));
///         break;
///     }
/// }
/// ```
pub struct HttpClient {
    tls_config: OwnedTLSConfig,
}
impl HttpClient {
    /// Create a new HttpClient
    pub fn new() -> Self {
        Self {
            tls_config: OwnedTLSConfig::default(),
        }
    }

    /// Override the default TLS config
    pub fn with_tls_config(mut self, tls_config: OwnedTLSConfig) -> Self {
        self.tls_config = tls_config;
        self
    }

    /// Initiate a new HTTP connection that is ready for a new request.
    ///
    /// This will return a [`HttpClientSession`], which encapsulates a [`FramingSession`] utilizing an HTTP [`FramingStrategy`].
    /// The framing strategy utilizes Hyperium's [`http`] lib for [`hyperium_http::Request`] and [`hyperium_http::Response`] structs.
    ///
    /// The returned [`HttpClientSession`] will have pre-setup TLS for `https` URLs, and will have pre-buffered the serialized request request.
    /// Before calling `read`/`write`, call [`Session::drive()`] to finish connecting and complete any pending TLS handshakes until [`Session::status`]
    /// returns [`ConnectionStatus::Connected`].
    ///
    /// For now, this only supports HTTP 1.x.
    pub fn connect(
        &mut self,
        host: &str,
        port: u16,
        scheme: Scheme,
    ) -> Result<HttpClientSession, io::Error> {
        let mut conn = TcpSession::connect(format!("{host}:{port}"))?;
        if scheme == Scheme::Https {
            conn = conn.into_tls(&host, self.tls_config.as_ref())?;
        }
        Ok(HttpClientSession::new(FramingSession::new(
            conn,
            Http1FramingStrategy::new(),
            0,
        )))
    }

    /// Initiate a new HTTP connection that will send the given request.
    ///
    /// This will return a [`HttpClientSession`], which encapsulates a [`FramingSession`] utilizing an HTTP [`FramingStrategy`].
    /// The framing strategy utilizes Hyperium's [`http`] lib for [`hyperium_http::Request`] and [`hyperium_http::Response`] structs.
    ///
    /// The returned [`HttpClientSession`] will have pre-setup TLS for `https` URLs, and will have pre-buffered the serialized request request.
    /// Calling `drive(..)` and `read(..)` will perform the TLS handshake, flush the pending request, buffer the response, and return a deserialized [`http::Response`].
    ///
    /// For now, this only supports HTTP 1.x.
    pub fn request<I: IntoBody>(
        &mut self,
        request: hyperium_http::Request<I>,
    ) -> Result<HttpClientSession, io::Error> {
        let (parts, body) = request.into_parts();
        let request = hyperium_http::Request::from_parts(parts, body.into_body());
        let stream = connect_stream(
            request.uri().scheme_str(),
            request.uri().host(),
            request.uri().port().map(|x| x.as_u16()),
            self.tls_config.as_ref(),
        )?;
        let mut conn = HttpClientSession::new(FramingSession::new(
            TcpSession::new(stream)?,
            Http1FramingStrategy::new(),
            0,
        ));
        conn.pending_initial_request = Some(request.into());

        Ok(conn)
    }
}
impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn connect_stream(
    scheme: Option<&str>,
    host: Option<&str>,
    port: Option<u16>,
    tls_config: TLSConfig<'_, '_, '_>,
) -> Result<TcpStream, Error> {
    let scheme = match scheme {
        None => Scheme::Http,
        Some("http") => Scheme::Http,
        Some("https") => Scheme::Https,
        _ => return Err(io::Error::new(ErrorKind::InvalidData, "bad uri scheme")),
    };
    let host = match host {
        Some(x) => x.to_owned(),
        None => return Err(io::Error::new(ErrorKind::InvalidData, "missing host")),
    };
    let port = match port {
        Some(x) => x,
        None => scheme.default_port(),
    };
    let mut conn = TcpStream::connect(format!("{host}:{port}"))?;
    if scheme == Scheme::Https {
        conn = conn
            .into_tls(&host, tls_config)
            .map_err(|err| Error::new(ErrorKind::ConnectionRefused, err))?;
    }
    Ok(conn)
}

// pub type HttpClientSession = FramingSession<TcpSession, Http1FramingStrategy>;
/// A [`Session`] created by the [`HttpClient`].
///
/// This encapsulates a [`FramingSession<TcpSession, Http1FramingStrategy>`] and allows a single [`hyperium_http::Request`]
/// to be enqueued prior to a successful connection, supporting the [`HttpClient::request`] function.
pub struct HttpClientSession {
    session: FramingSession<TcpSession, Http1FramingStrategy>,
    pending_initial_request: Option<HttpRequest>,
}
impl HttpClientSession {
    pub fn new(session: FramingSession<TcpSession, Http1FramingStrategy>) -> Self {
        Self {
            session,
            pending_initial_request: None,
        }
    }
}
impl Session for HttpClientSession {
    type ReadData<'a> = hyperium_http::Response<Vec<u8>>;
    type WriteData<'a> = HttpRequest;

    fn status(&self) -> crate::ConnectionStatus {
        self.session.status()
    }

    fn close(&mut self) {
        self.session.close()
    }

    fn drive(&mut self) -> Result<bool, Error> {
        let mut result = self.session.drive()?;
        if self.session.status() == ConnectionStatus::Connected
            && self.pending_initial_request.is_some()
        {
            let wrote = match self.session.write(
                self.pending_initial_request
                    .take()
                    .expect("checked pending_request"),
            )? {
                WriteStatus::Success => true,
                WriteStatus::Pending(x) => {
                    self.pending_initial_request = Some(x);
                    false
                }
            };
            if wrote {
                self.pending_initial_request = None;
                result |= true;
            }
        }
        Ok(result)
    }

    fn write<'a>(
        &mut self,
        data: Self::WriteData<'a>,
    ) -> Result<WriteStatus<Self::WriteData<'a>>, Error> {
        self.session.write(data)
    }

    fn read<'a>(&'a mut self) -> Result<crate::ReadStatus<Self::ReadData<'a>>, Error> {
        if self.pending_initial_request.is_none() && self.status() == ConnectionStatus::Connected {
            // make the request/response model more straightforward by not requiring checks to `status()` before calling `read`.
            // `self.pending_initial_request.is_none()`: only do this when an initial request is pending, otherwise revert to default `read` behavior for persistent streams.
            self.session.read()
        } else {
            Ok(crate::ReadStatus::None)
        }
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.session.flush()
    }
}

/// Extensible public trait to support serializing a variety of body types.
pub trait IntoBody {
    fn into_body(self) -> Vec<u8>;
}
impl IntoBody for String {
    fn into_body(self) -> Vec<u8> {
        self.into_bytes()
    }
}
impl IntoBody for &str {
    fn into_body(self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}
impl IntoBody for Vec<u8> {
    fn into_body(self) -> Vec<u8> {
        self
    }
}
impl IntoBody for &[u8] {
    fn into_body(self) -> Vec<u8> {
        self.to_vec()
    }
}
impl IntoBody for () {
    fn into_body(self) -> Vec<u8> {
        Vec::new()
    }
}

enum BodyType {
    ContentLength(usize),
    ChunkedTransfer,
    OnClose,
    None,
}

struct BodyInfo {
    offset: usize,
    ty: BodyType,
}
impl BodyInfo {
    pub fn new(offset: usize, ty: BodyType) -> Self {
        Self { offset, ty }
    }
}

/// A [`FramingStrategy`] for HTTP 1.x where [`FramingStrategy::WriteFrame`] is an [`http::Request`], and the [`FramingStrategy::ReadFrame`] is an [`http::Response`].
pub struct Http1FramingStrategy {
    deserialized_response: Option<hyperium_http::Response<Vec<u8>>>,
    deserialized_size: usize,
    body_info: Option<BodyInfo>,
}
impl Http1FramingStrategy {
    pub fn new() -> Self {
        Self {
            deserialized_response: None,
            deserialized_size: 0,
            body_info: None,
        }
    }
}
impl FramingStrategy for Http1FramingStrategy {
    type ReadFrame<'a> = hyperium_http::Response<Vec<u8>>;
    type WriteFrame<'a> = HttpRequest;

    fn check_deserialize_frame(&mut self, data: &[u8], eof: bool) -> Result<bool, Error> {
        if self.deserialized_response.is_none() {
            self.deserialized_response = Some(Response::new(Vec::new()));
        }
        let deserialized_response = self
            .deserialized_response
            .as_mut()
            .expect("checked deserialized_response value");

        let header_count: usize = count_max_headers(data);
        let mut headers = Vec::new();
        headers.resize(header_count, httparse::EMPTY_HEADER);

        // if they have not already been parsed, attempt to parse the response headers and find the body type.
        if self.body_info.is_none() {
            let mut parsed = httparse::Response::new(&mut headers);
            match parsed.parse(data).map_err(|err| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("http response parse failed: {err:?}").as_str(),
                )
            })? {
                httparse::Status::Complete(size) => {
                    // determine the body type
                    if parse_is_chunked(&parsed.headers) {
                        self.body_info = Some(BodyInfo::new(size, BodyType::ChunkedTransfer));
                    } else if let Some(content_length) = parse_content_length(&parsed.headers)? {
                        self.body_info =
                            Some(BodyInfo::new(size, BodyType::ContentLength(content_length)));
                    } else if parsed.version.is_none() || parsed.version == Some(1) {
                        self.body_info = Some(BodyInfo::new(size, BodyType::OnClose));
                    } else {
                        self.body_info = Some(BodyInfo::new(size, BodyType::None));
                    }
                    // parse into cached deserialized_response
                    parsed_into_response(parsed, deserialized_response)?;
                }
                httparse::Status::Partial => return Ok(false),
            }
        }

        // use parsed BodyInfo to check if entire body has been received
        let parsed_body = match &self.body_info {
            None => None,
            Some(body_info) => {
                match body_info.ty {
                    BodyType::ChunkedTransfer => {
                        // TODO: determine better way to see if all chunks have been received, deserializer does not support this
                        // TODO: cache offset of last read chunk to avoid re-parsing entire body every time
                        if body_info.offset < data.len() && ends_with_ascii(data, "\r\n\r\n") {
                            let mut body = Vec::new();
                            let mut decoder =
                                chunked_transfer::Decoder::new(&data[body_info.offset..]);
                            decoder.read_to_end(&mut body)?;
                            match decoder.remaining_chunks_size() {
                                None => Some(body),
                                Some(_) => None,
                            }
                        } else {
                            None
                        }
                    }
                    BodyType::ContentLength(content_length) => {
                        // read to given length
                        let total_length = body_info.offset + content_length;
                        if data.len() >= total_length {
                            Some(data[body_info.offset..total_length].to_vec())
                        } else {
                            None
                        }
                    }
                    BodyType::OnClose => {
                        if eof {
                            Some(data[body_info.offset..].to_vec())
                        } else {
                            None
                        }
                    }
                    BodyType::None => Some(Vec::new()),
                }
            }
        };

        // if entire body has been received, insert into deserialized_response and return true
        match parsed_body {
            None => {
                if eof {
                    Err(Error::new(
                        ErrorKind::UnexpectedEof,
                        "http connection terminated before receiving full response",
                    ))
                } else {
                    Ok(false)
                }
            }
            Some(mut body) => {
                swap(deserialized_response.body_mut(), &mut body);
                // reset parsed body info for next response parsing iteration, allowing for use of keep-alive
                self.body_info = None;
                Ok(true)
            }
        }
    }

    fn deserialize_frame<'a>(
        &'a mut self,
        _data: &'a [u8],
    ) -> Result<crate::frame::DeserializedFrame<Self::ReadFrame<'a>>, Error> {
        // return response that was deserialized in `check_deserialize_frame(..)`
        Ok(DeserializedFrame::new(
            self.deserialized_response
                .take()
                .ok_or_else(|| Error::new(ErrorKind::Other, "no deserialized frame"))?,
            self.deserialized_size,
        ))
    }

    fn write_frame<'a>(
        &mut self,
        request: Self::WriteFrame<'a>,
        buffer: &mut GrowableCircleBuf,
    ) -> Result<WriteStatus<Self::WriteFrame<'a>>, Error> {
        let serialized_request = match request {
            HttpRequest::Request(request) => {
                // check version
                match request.version() {
                    hyperium_http::Version::HTTP_10 | hyperium_http::Version::HTTP_11 => {}
                    version => {
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            format!("unsupported http request version {version:?}").as_str(),
                        ))
                    }
                }

                // parse uri
                let host = match request.uri().host() {
                    Some(x) => x.to_owned(),
                    None => return Err(io::Error::new(ErrorKind::InvalidData, "missing host")),
                };

                // calculate content-length
                let body = request.body();
                let content_length = body.len().to_string();

                // construct HTTP/1.x payload
                let mut serialized_request = Vec::new();
                serialized_request.extend_from_slice(request.method().as_str().as_bytes());
                serialized_request.extend_from_slice(" ".as_bytes());
                serialized_request.extend_from_slice(request.uri().path().as_bytes());
                if let Some(query) = request.uri().query() {
                    serialized_request.extend_from_slice("?".as_bytes());
                    serialized_request.extend_from_slice(query.as_bytes());
                }
                serialized_request
                    .extend_from_slice(format!(" {:?}", request.version()).as_bytes());
                serialized_request.extend_from_slice(LINE_BREAK.as_bytes());
                {
                    // host header
                    serialized_request.extend_from_slice(HOST.as_str().as_bytes());
                    serialized_request.extend_from_slice(": ".as_bytes());
                    serialized_request.extend_from_slice(host.as_bytes());
                    serialized_request.extend_from_slice(LINE_BREAK.as_bytes());
                }
                for (n, v) in request.headers().iter() {
                    // request headers
                    serialized_request.extend_from_slice(n.as_str().as_bytes());
                    serialized_request.extend_from_slice(": ".as_bytes());
                    serialized_request.extend_from_slice(
                        v.to_str()
                            .map_err(|_| {
                                Error::new(
                                    ErrorKind::InvalidData,
                                    format!("could not convert header '{}' to string", n.as_str())
                                        .as_str(),
                                )
                            })?
                            .as_bytes(),
                    );
                    serialized_request.extend_from_slice(LINE_BREAK.as_bytes());
                }
                if body.len() > 0 {
                    // content length header
                    serialized_request.extend_from_slice(CONTENT_LENGTH.as_str().as_bytes());
                    serialized_request.extend_from_slice(": ".as_bytes());
                    serialized_request.extend_from_slice(content_length.as_bytes());
                    serialized_request.extend_from_slice(LINE_BREAK.as_bytes());
                }
                serialized_request.extend_from_slice(LINE_BREAK.as_bytes());
                serialized_request.extend_from_slice(body);
                serialized_request
            }
            HttpRequest::Serialized(serialized) => serialized,
        };

        // returned pending request
        if buffer.try_write(&vec![&serialized_request])? {
            Ok(WriteStatus::Success)
        } else {
            Ok(WriteStatus::Pending(HttpRequest::Serialized(
                serialized_request,
            )))
        }
    }
}

fn parsed_into_response(
    parsed: httparse::Response,
    resp: &mut http::Response<Vec<u8>>,
) -> Result<(), Error> {
    // status code
    if let Some(code) = parsed.code {
        let status_code = hyperium_http::StatusCode::try_from(code).map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                format!("response invalid status code '{code}'").as_str(),
            )
        })?;
        *resp.status_mut() = status_code;
    }
    // version
    if let Some(version) = parsed.version {
        *resp.version_mut() = match version {
            0 => hyperium_http::Version::HTTP_10,
            1 => hyperium_http::Version::HTTP_11,
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("response invalid version '{version}'").as_str(),
                ))
            }
        };
    }
    // headers
    for h in parsed.headers.iter() {
        let name = http::HeaderName::from_str(h.name).map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                format!("response invalid header name '{}'", h.name).as_str(),
            )
        })?;
        let value = http::HeaderValue::from_bytes(h.value).map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                format!("response invalid header value '{:?}'", h.value).as_str(),
            )
        })?;
        resp.headers_mut().insert(name, value);
    }
    Ok(())
}

const LINE_BREAK: &str = "\r\n";

fn count_max_headers(payload: &[u8]) -> usize {
    if payload.is_empty() {
        return 0;
    }
    let mut count = 0;
    for i in 0..payload.len() - 1 {
        if payload[i] == b'\r' && payload[i + 1] == b'\n' {
            count += 1;
        }
    }
    count
}

fn ends_with_ascii(buf: &[u8], ends_with: &str) -> bool {
    if buf.len() < ends_with.len() {
        return false;
    }
    let ends_with = ends_with.as_bytes();
    for i in 0..ends_with.len() {
        if buf[buf.len() - i - 1] != ends_with[ends_with.len() - i - 1] {
            return false;
        }
    }
    true
}

fn parse_is_chunked(headers: &[httparse::Header]) -> bool {
    match find_header(&headers, TRANSFER_ENCODING.as_str()) {
        Some(v) => String::from_utf8_lossy(v).eq_ignore_ascii_case("chunked"),
        None => false,
    }
}

fn parse_content_length(headers: &[httparse::Header]) -> Result<Option<usize>, Error> {
    if let Some(v) = find_header(headers, CONTENT_LENGTH.as_str()) {
        let v = String::from_utf8_lossy(v);
        return Ok(Some(v.parse().map_err(|_| {
            Error::new(ErrorKind::InvalidData, "content-length not a number")
        })?));
    }
    Ok(None)
}

fn find_header<'a>(headers: &'a [httparse::Header], name: &str) -> Option<&'a [u8]> {
    for h in headers.iter() {
        if h.name.eq_ignore_ascii_case(name) {
            return Some(h.value);
        }
    }
    None
}
