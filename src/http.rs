//! A non-blocking HTTP 1.x client
use std::{
    io::{self, Error, ErrorKind, Read},
    mem::swap,
    str::FromStr,
};

use hyperium_http::header::{CONTENT_LENGTH, HOST, TRANSFER_ENCODING};
use tcp_stream::OwnedTLSConfig;

use crate::{
    frame::{DeserializedFrame, FramingSession, FramingStrategy},
    tcp::StreamingTcpSession,
    Session, TlsSession, WriteStatus,
};

pub type HttpClientSession = FramingSession<StreamingTcpSession, Http1FramingStrategy>;

/// A simple non-blocking HTTP 1.x client
///
/// Calling `request(..)` will return a [`HttpClientSession`], which is simply a [`FramingSession`] utilizing an HTTP [`FramingStrategy`].
/// The framing strategy utilizes Hyperium's [`http`] lib for [`http::Request`] and [`http::Response`] structs.
///
/// The returned [`HttpClientSession`] will have pre-setup TLS for `https` URLs, and will have pre-buffered the serialized request request.
/// Calling `drive(..)` and `read(..)` will perform the TLS handshake, flush the pending request, buffer the response, and return a deserialized [`http::Response`].
///
/// For now, this only supports HTTP 1.x.
///
/// Example:
/// ```no_run
/// use nbio::{Session, ReadStatus};
/// use nbio::http::HttpClient;
/// use nbio::hyperium_http::Request;
/// use nbio::tcp_stream::OwnedTLSConfig;
///
/// // create the client and make the request
/// let mut client = HttpClient::new(OwnedTLSConfig::default());
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
    /// Create a new HttpClient using the given TLS config
    pub fn new(tls_config: OwnedTLSConfig) -> Self {
        Self { tls_config }
    }

    /// Initiate a new HTTP connection and buffer the given request.
    ///
    /// This will return a [`HttpClientSession`], which is simply a [`FramingSession`] utilizing an HTTP [`FramingStrategy`].
    /// The framing strategy utilizes Hyperium's [`http`] lib for [`http::Request`] and [`http::Response`] structs.
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

        let https = match request.uri().scheme_str() {
            None => false,
            Some("http") => false,
            Some("https") => true,
            _ => return Err(io::Error::new(ErrorKind::InvalidData, "bad uri scheme")),
        };
        let host = match request.uri().host() {
            Some(x) => x.to_owned(),
            None => return Err(io::Error::new(ErrorKind::InvalidData, "missing host")),
        };
        let port = match request.uri().port() {
            Some(x) => x.as_u16(),
            None => {
                if https {
                    443
                } else {
                    80
                }
            }
        };

        // start connection
        let mut conn = FramingSession::new(
            StreamingTcpSession::connect(&format!("{}:{}", host, port))?.with_nonblocking(true)?,
            Http1FramingStrategy::new(),
            0,
        );
        if https {
            conn.to_tls(&host, self.tls_config.as_ref())?;
        }
        if let WriteStatus::Pending(_) = conn.write(&request)? {
            return Err(Error::new(
                ErrorKind::Other,
                "http payload should have buffered",
            ));
        }

        Ok(conn)
    }
}

/// Extensible public trait to support serializing a variety of body types.
pub trait IntoBody {
    fn into_body(self) -> Vec<u8>;
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
    serialized_request: Vec<u8>,
    deserialized_response: hyperium_http::Response<Vec<u8>>,
    deserialized_size: usize,
    body_info: Option<BodyInfo>,
}
impl Http1FramingStrategy {
    pub fn new() -> Self {
        Self {
            serialized_request: Vec::new(),
            deserialized_response: hyperium_http::Response::new(Vec::new()),
            deserialized_size: 0,
            body_info: None,
        }
    }
}
impl FramingStrategy for Http1FramingStrategy {
    type ReadFrame = hyperium_http::Response<Vec<u8>>;
    type WriteFrame = hyperium_http::Request<Vec<u8>>;

    fn check_deserialize_frame(&mut self, data: &[u8], eof: bool) -> Result<bool, Error> {
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
                    parsed_into_response(parsed, &mut self.deserialized_response)?;
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
                swap(self.deserialized_response.body_mut(), &mut body);
                // reset parsed body info for next response parsing iteration, allowing for use of keep-alive
                self.body_info = None;
                Ok(true)
            }
        }
    }

    fn deserialize_frame<'a>(
        &'a mut self,
        _data: &'a [u8],
    ) -> Result<crate::frame::DeserializedFrame<'a, Self::ReadFrame>, Error> {
        // return response that was deserialized in `check_deserialize_frame(..)`
        Ok(DeserializedFrame::new(
            &self.deserialized_response,
            self.deserialized_size,
        ))
    }

    fn serialize_frame<'a>(
        &'a mut self,
        request: &'a Self::WriteFrame,
    ) -> Result<Vec<&'a [u8]>, Error> {
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

        // construct HTTP/1.1 payload
        self.serialized_request = Vec::new();
        self.serialized_request
            .extend_from_slice(request.method().as_str().as_bytes());
        self.serialized_request.extend_from_slice(" ".as_bytes());
        self.serialized_request
            .extend_from_slice(request.uri().path().as_bytes());
        if let Some(query) = request.uri().query() {
            self.serialized_request.extend_from_slice("?".as_bytes());
            self.serialized_request.extend_from_slice(query.as_bytes());
        }
        self.serialized_request
            .extend_from_slice(format!(" {:?}", request.version()).as_bytes());
        self.serialized_request
            .extend_from_slice(LINE_BREAK.as_bytes());
        {
            // host header
            self.serialized_request
                .extend_from_slice(HOST.as_str().as_bytes());
            self.serialized_request.extend_from_slice(": ".as_bytes());
            self.serialized_request.extend_from_slice(host.as_bytes());
            self.serialized_request
                .extend_from_slice(LINE_BREAK.as_bytes());
        }
        for (n, v) in request.headers().iter() {
            // request headers
            self.serialized_request
                .extend_from_slice(n.as_str().as_bytes());
            self.serialized_request.extend_from_slice(": ".as_bytes());
            self.serialized_request.extend_from_slice(
                v.to_str()
                    .map_err(|_| {
                        Error::new(
                            ErrorKind::InvalidData,
                            format!("could not convert header '{}' to string", n.as_str()).as_str(),
                        )
                    })?
                    .as_bytes(),
            );
            self.serialized_request
                .extend_from_slice(LINE_BREAK.as_bytes());
        }
        if body.len() > 0 {
            // content length header
            self.serialized_request
                .extend_from_slice(CONTENT_LENGTH.as_str().as_bytes());
            self.serialized_request.extend_from_slice(": ".as_bytes());
            self.serialized_request
                .extend_from_slice(content_length.as_bytes());
            self.serialized_request
                .extend_from_slice(LINE_BREAK.as_bytes());
        }
        self.serialized_request
            .extend_from_slice(LINE_BREAK.as_bytes());
        self.serialized_request.extend_from_slice(body);

        // returned pending request
        Ok(vec![&self.serialized_request])
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
