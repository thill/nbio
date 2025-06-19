//! A non-blocking HTTP client
use std::{
    cell::RefCell,
    fmt::Debug,
    io::{self, Error, ErrorKind, Read},
    mem::swap,
    rc::Rc,
    str::FromStr,
    sync::Arc,
};

use hyperium_http::{
    header::{CONTENT_LENGTH, HOST, TRANSFER_ENCODING},
    Response,
};
use tcp_stream::OwnedTLSConfig;

use crate::{
    buffer::GrowableCircleBuf,
    dns::AddrResolver,
    frame::{DeserializeFrame, FrameDuplex, SerializeFrame, SizedFrame},
    tcp::TcpSession,
    tls::{NativeTlsConnector, TlsConnector},
    DriveOutcome, Flush, Publish, PublishOutcome, Receive, ReceiveOutcome, Session, SessionStatus,
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
/// Calling `connect(..)` or `request(..)` will return a [`HttpClientSession`], which encapsulates a [`FrameDuplex`] utilizing an HTTP [`FramingStrategy`].
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
/// returns [`SessionStatus::Connected`], at which point the session can send multiple [`hyperium_http::Request`] payloads and receive [`hyperium_http::Response`] payloads utilizing HTTP "keep-alive".
///
/// ## Example
///
/// ```no_run
/// use nbio::{Receive, ReceiveOutcome, Session};
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
///     if let ReceiveOutcome::Payload(r) = conn.receive().unwrap() {
///         // validate the response
///         println!("Response Body: {}", String::from_utf8_lossy(r.body()));
///         break;
///     }
/// }
/// ```
pub struct HttpClient {
    tls_connector: Option<Arc<TlsConnector>>,
    addr_resolver: Option<Arc<AddrResolver>>,
}
impl HttpClient {
    /// Create a new HttpClient
    pub fn new() -> Self {
        Self {
            tls_connector: None,
            addr_resolver: None,
        }
    }

    /// Set the [`AddrResolver`] to use to resolve DNS entries
    pub fn with_addr_resolver(mut self, addr_resolver: Arc<AddrResolver>) -> Self {
        self.addr_resolver = Some(addr_resolver);
        self
    }

    /// Set the [`TlsConnector`] to use for a TLS handshakes
    pub fn with_tls_connector(mut self, tls_connector: Arc<TlsConnector>) -> Self {
        self.tls_connector = Some(tls_connector);
        self
    }

    /// Create a native TLS connector with the given config
    pub fn with_tls_config(mut self, tls_config: OwnedTLSConfig) -> Result<Self, Error> {
        self.tls_connector = Some(Arc::new(TlsConnector::Native(NativeTlsConnector::new(
            tls_config.as_ref(),
            false,
        )?)));
        Ok(self)
    }

    /// Initiate a new HTTP connection that is ready for a new request.
    ///
    /// This will return a [`HttpClientSession`], which encapsulates a [`FrameDuplex`] utilizing an HTTP [`FramingStrategy`].
    /// The framing strategy utilizes Hyperium's [`http`] lib for [`hyperium_http::Request`] and [`hyperium_http::Response`] structs.
    ///
    /// You may create a [`PersistentHttpConnection`] from the returned [`HttpClientSession`] by calling [`From<HttpClientSession>`] on it,
    /// is useful for keep-alive request/response coordination across multiple concurrent pending requests.
    ///
    /// The returned [`HttpClientSession`] will have pre-setup TLS for `https` URLs, and will have pre-buffered the serialized request request.
    /// Before calling `read`/`write`, call [`Session::drive()`] to finish connecting and complete any pending TLS handshakes until [`Session::status`]
    /// returns [`SessionStatus::Connected`].
    ///
    /// For now, this only supports HTTP 1.x.
    pub fn connect(
        &mut self,
        host: &str,
        port: u16,
        scheme: Scheme,
    ) -> Result<HttpClientSession, io::Error> {
        let mut conn = TcpSession::connect(
            format!("{host}:{port}"),
            self.addr_resolver.as_ref().map(|x| Arc::clone(x)),
            self.tls_connector.as_ref().map(|x| Arc::clone(&x)),
        )?;
        if scheme == Scheme::Https {
            conn = conn.into_tls(&host)?;
        }
        Ok(HttpClientSession::new(FrameDuplex::new(
            conn,
            Http1ResponseDeserializer::new(),
            Http1RequestSerializer::new(),
            0,
        )))
    }

    /// Initiate a new HTTP connection that will send the given request.
    ///
    /// This will return a [`HttpClientSession`], which encapsulates a [`FrameDuplex`] utilizing an HTTP [`FramingStrategy`].
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
        let scheme = match request.uri().scheme_str() {
            None => Scheme::Http,
            Some("http") => Scheme::Http,
            Some("https") => Scheme::Https,
            _ => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "bad http uri scheme",
                ))
            }
        };
        let session = connect_stream(
            scheme,
            request.uri().host(),
            request.uri().port().map(|x| x.as_u16()),
            self.addr_resolver.as_ref().map(|x| Arc::clone(x)),
            self.tls_connector.as_ref().map(|x| Arc::clone(&x)),
        )?;
        let mut conn = HttpClientSession::new(FrameDuplex::new(
            session,
            Http1ResponseDeserializer::new(),
            Http1RequestSerializer::new(),
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
    scheme: Scheme,
    host: Option<&str>,
    port: Option<u16>,
    addr_resolver: Option<Arc<AddrResolver>>,
    tls_connector: Option<Arc<TlsConnector>>,
) -> Result<TcpSession, Error> {
    let host = match host {
        Some(x) => x.to_owned(),
        None => return Err(io::Error::new(ErrorKind::InvalidData, "missing host")),
    };
    let port = match port {
        Some(x) => x,
        None => scheme.default_port(),
    };
    let mut conn = TcpSession::connect(format!("{host}:{port}"), addr_resolver, tls_connector)?;
    if scheme == Scheme::Https {
        conn = conn
            .into_tls(&host)
            .map_err(|err| Error::new(ErrorKind::ConnectionRefused, err))?;
    }
    Ok(conn)
}

/// A [`Session`] created by the [`HttpClient`].
///
/// This encapsulates a [`FrameDuplex<TcpSession, Http1ResponseDeserializer, Http1RequestSerializer>`] and allows a single
/// [`hyperium_http::Request`] to be enqueued prior to a successful connection, supporting the [`HttpClient::request`] function.
pub struct HttpClientSession {
    session: FrameDuplex<TcpSession, Http1ResponseDeserializer, Http1RequestSerializer>,
    pending_initial_request: Option<HttpRequest>,
}
impl HttpClientSession {
    pub fn new(
        session: FrameDuplex<TcpSession, Http1ResponseDeserializer, Http1RequestSerializer>,
    ) -> Self {
        Self {
            session,
            pending_initial_request: None,
        }
    }
}
impl Session for HttpClientSession {
    fn status(&self) -> crate::SessionStatus {
        self.session.status()
    }

    fn drive(&mut self) -> Result<DriveOutcome, Error> {
        let mut result: crate::DriveOutcome = self.session.drive()?;
        if self.session.status() == SessionStatus::Established
            && self.pending_initial_request.is_some()
        {
            let wrote = match self.session.publish(
                self.pending_initial_request
                    .take()
                    .expect("checked pending_request"),
            )? {
                PublishOutcome::Published => true,
                PublishOutcome::Incomplete(x) => {
                    self.pending_initial_request = Some(x);
                    false
                }
            };
            if wrote {
                self.pending_initial_request = None;
                result = DriveOutcome::Active;
            }
        }
        Ok(result)
    }
}
impl Receive for HttpClientSession {
    type ReceivePayload<'a> = hyperium_http::Response<Vec<u8>>;

    fn receive<'a>(&'a mut self) -> Result<crate::ReceiveOutcome<Self::ReceivePayload<'a>>, Error> {
        self.drive()?;
        if self.pending_initial_request.is_none() && self.status() == SessionStatus::Established {
            // make the request/response model more straightforward by not requiring checks to `status()` before calling `read`.
            // `self.pending_initial_request.is_none()`: only do this when an initial request is pending, otherwise revert to default `read` behavior for persistent streams.
            self.session.receive()
        } else {
            Ok(crate::ReceiveOutcome::Idle)
        }
    }
}
impl Publish for HttpClientSession {
    type PublishPayload<'a> = HttpRequest;

    fn publish<'a>(
        &mut self,
        data: Self::PublishPayload<'a>,
    ) -> Result<PublishOutcome<Self::PublishPayload<'a>>, Error> {
        self.session.publish(data)
    }
}
impl Flush for HttpClientSession {
    fn flush(&mut self) -> Result<(), Error> {
        self.session.flush()
    }
}
impl Debug for HttpClientSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClientSession")
            .field("session", &self.session)
            .finish()
    }
}

struct PersistentHttpSessionContext {
    session: HttpClientSession,
    active_request_id: u64,
    next_request_id: u64,
    closed: bool,
}

/// Encapsulates a [`HttpClientSession`], enabling multiple requests to be queued and used on a single session.
///
/// It ensures only one request can be in flight at a time, and will buffer the request until the connection is established.
/// This utilizes HTTP 1.x keep-alive headers to attempt to re-use the same connection for multiple requests.
///
/// Requests are started by calling [`PersistentHttpConnection::request`], which will return a [`PendingHttpResponse`].
/// These pending http response will coordinate with one another to send requests and drive responses in the order the function was called.
///
/// An HTTP server may elect to close an http session at any time.
/// When this occurs, a new [`PersistentHttpConnection`] must be created from the original [`HttpClient`].
pub struct PersistentHttpConnection {
    context: Rc<RefCell<PersistentHttpSessionContext>>,
}

impl From<HttpClientSession> for PersistentHttpConnection {
    fn from(session: HttpClientSession) -> Self {
        Self {
            context: Rc::new(RefCell::new(PersistentHttpSessionContext {
                session,
                active_request_id: 0,
                next_request_id: 0,
                closed: false,
            })),
        }
    }
}

impl Debug for PersistentHttpConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentHttpConnection").finish()
    }
}

impl Session for PersistentHttpConnection {
    fn status(&self) -> crate::SessionStatus {
        self.context.borrow().session.status()
    }

    fn drive(&mut self) -> Result<DriveOutcome, Error> {
        self.context.borrow_mut().session.drive()
    }
}

impl PersistentHttpConnection {
    pub fn request<I: IntoBody>(
        &mut self,
        mut request: hyperium_http::Request<I>,
    ) -> Result<PendingHttpResponse, Error> {
        // add the keep-alive header to the request
        request
            .headers_mut()
            .insert("Connection", "keep-alive".parse().unwrap());

        // check that the connection was not set to close by the server on the last request
        let mut context = self.context.borrow_mut();
        if context.closed {
            return Err(Error::new(
                ErrorKind::ConnectionRefused,
                "connection closed by server",
            ));
        }

        // assign the next available request id
        let request_id = context.next_request_id;
        context.next_request_id += 1;
        drop(context);

        // return the pending http response
        Ok(PendingHttpResponse {
            context: Rc::clone(&self.context),
            pending_request: Some(request.into()),
            request_id,
        })
    }
}

/// A pending or active [`HttpRequest`] for a [`PersistentHttpSession`], which can be polled to completion.
pub struct PendingHttpResponse {
    context: Rc<RefCell<PersistentHttpSessionContext>>,
    pending_request: Option<HttpRequest>,
    request_id: u64,
}
impl Debug for PendingHttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingHttpResponse")
            .field("request_id", &self.request_id)
            .finish()
    }
}
impl Session for PendingHttpResponse {
    fn status(&self) -> crate::SessionStatus {
        self.context.borrow().session.status()
    }

    fn drive(&mut self) -> Result<DriveOutcome, Error> {
        let mut context = self.context.borrow_mut();
        if context.active_request_id == self.request_id {
            // is the active request, drive the session
            context.session.drive()
        } else {
            // not the active request, do not drive the session
            Ok(DriveOutcome::Idle)
        }
    }
}
impl Receive for PendingHttpResponse {
    type ReceivePayload<'a> = hyperium_http::Response<Vec<u8>>;

    fn receive<'a>(&'a mut self) -> Result<ReceiveOutcome<Self::ReceivePayload<'a>>, Error> {
        let mut context = self.context.borrow_mut();
        // first check if the active request is this request, if not, return idle
        if context.active_request_id != self.request_id {
            return Ok(ReceiveOutcome::Idle);
        }

        // then try to send the request if necessary
        if let Some(request) = self.pending_request.take() {
            match context.session.publish(request)? {
                PublishOutcome::Incomplete(request) => {
                    self.pending_request = Some(request);
                    return Ok(ReceiveOutcome::Idle);
                }
                PublishOutcome::Published => {
                    return Ok(ReceiveOutcome::Active);
                }
            }
        }

        // otherwise, attempt to receive the response
        match context.session.receive()? {
            ReceiveOutcome::Payload(response) => {
                // done! advance the active request id, return the response
                context.active_request_id += 1;
                // check if the keep-alive header is set to close, this will fail the next request early without needing to attempt and fail when interactive with the wire
                if response
                    .headers()
                    .get("Connection")
                    .map(|x| x.to_str().unwrap())
                    == Some("close")
                {
                    context.closed = true;
                }
                Ok(ReceiveOutcome::Payload(response))
            }
            ReceiveOutcome::Active => Ok(ReceiveOutcome::Active),
            ReceiveOutcome::Idle => Ok(ReceiveOutcome::Idle),
        }
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

/// A [`DeserializeFrame`] impl for HTTP 1.x where [`DeserializeFrame::DeserializedFrame`] is an [`http::Response`].
pub struct Http1ResponseDeserializer {
    deserialized_response: Option<hyperium_http::Response<Vec<u8>>>,
    deserialized_size: usize,
    body_info: Option<BodyInfo>,
}
impl Http1ResponseDeserializer {
    pub fn new() -> Self {
        Self {
            deserialized_response: None,
            deserialized_size: 0,
            body_info: None,
        }
    }
}
impl DeserializeFrame for Http1ResponseDeserializer {
    type DeserializedFrame<'a> = hyperium_http::Response<Vec<u8>>;

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
        let (parsed_body, total_size) = match &self.body_info {
            None => (None, 0),
            Some(body_info) => {
                match body_info.ty {
                    BodyType::ChunkedTransfer => {
                        // TODO: determine better way to see if all chunks have been received
                        // TODO: cache offset of last read chunk to avoid re-parsing entire body every time
                        if body_info.offset < data.len() && ends_with_ascii(data, "\r\n\r\n") {
                            let mut body = Vec::new();
                            let mut decoder =
                                chunked_transfer::Decoder::new(&data[body_info.offset..]);
                            decoder.read_to_end(&mut body)?;
                            let body_len = body.len();
                            match decoder.remaining_chunks_size() {
                                None => (Some(body), body_info.offset + body_len),
                                Some(_) => (None, 0),
                            }
                        } else {
                            (None, 0)
                        }
                    }
                    BodyType::ContentLength(content_length) => {
                        // read to given length
                        let total_length = body_info.offset + content_length;
                        if data.len() >= total_length {
                            (
                                Some(data[body_info.offset..total_length].to_vec()),
                                total_length,
                            )
                        } else {
                            (None, 0)
                        }
                    }
                    BodyType::OnClose => {
                        if eof {
                            (Some(data[body_info.offset..].to_vec()), data.len())
                        } else {
                            (None, 0)
                        }
                    }
                    BodyType::None => (Some(Vec::new()), body_info.offset),
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
                self.deserialized_size = total_size;
                Ok(true)
            }
        }
    }

    fn deserialize_frame<'a>(
        &'a mut self,
        _data: &'a [u8],
    ) -> Result<crate::frame::SizedFrame<Self::DeserializedFrame<'a>>, Error> {
        // return response that was deserialized in `check_deserialize_frame(..)`
        Ok(SizedFrame::new(
            self.deserialized_response
                .take()
                .ok_or_else(|| Error::new(ErrorKind::Other, "no deserialized frame"))?,
            self.deserialized_size,
        ))
    }
}

/// A [`SerializeFrame`] impl for HTTP 1.x where [`SerializeFrame::SerializedFrame`] is an [`HttpRequest`].
pub struct Http1RequestSerializer {}
impl Http1RequestSerializer {
    pub fn new() -> Self {
        Self {}
    }
}
impl SerializeFrame for Http1RequestSerializer {
    type SerializedFrame<'a> = HttpRequest;

    fn serialize_frame<'a>(
        &mut self,
        request: Self::SerializedFrame<'a>,
        buffer: &mut GrowableCircleBuf,
    ) -> Result<PublishOutcome<Self::SerializedFrame<'a>>, Error> {
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
            Ok(PublishOutcome::Published)
        } else {
            Ok(PublishOutcome::Incomplete(HttpRequest::Serialized(
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
