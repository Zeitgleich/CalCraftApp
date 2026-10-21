//! The HTTP transport abstraction.
//!
//! The library never opens sockets itself. Every request goes through an
//! [`HttpTransport`], so tests can replay recorded responses and the app can
//! choose its HTTP stack. A default implementation based on hyper and rustls
//! is available with the `hyper-transport` feature
//! ([`HyperTransport`](crate::HyperTransport)).
//!
//! Transports must not follow redirects (the client decides which redirects
//! are safe) and must stop reading a response body once it exceeds
//! [`Request::max_response_bytes`].

use std::fmt;
use std::future::Future;

use zeroize::Zeroizing;

use crate::url::Url;

/// HTTP methods used by CalDAV/CardDAV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `GET`
    Get,
    /// `PUT`
    Put,
    /// `DELETE`
    Delete,
    /// `PROPFIND` (RFC 4918)
    Propfind,
    /// `PROPPATCH` (RFC 4918)
    Proppatch,
    /// `REPORT` (RFC 3253)
    Report,
    /// `MKCALENDAR` (RFC 4791)
    Mkcalendar,
    /// `MKCOL` (RFC 4918, extended by RFC 5689)
    Mkcol,
}

impl Method {
    /// The method token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Propfind => "PROPFIND",
            Self::Proppatch => "PROPPATCH",
            Self::Report => "REPORT",
            Self::Mkcalendar => "MKCALENDAR",
            Self::Mkcol => "MKCOL",
        }
    }
}

/// A secret header value (the `Authorization` header). It is zeroized on
/// drop and never printed by `Debug`.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub(crate) fn new(value: Zeroizing<String>) -> Self {
        Self(value)
    }

    /// The secret value. Transports should mark the resulting header as
    /// sensitive and must never log it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// An HTTP request built by the client.
#[derive(Debug)]
pub struct Request {
    /// Method.
    pub method: Method,
    /// Absolute request URL (always on the account origin).
    pub url: Url,
    /// Additional headers with lower-case names. Values contain no control
    /// characters.
    pub headers: Vec<(&'static str, String)>,
    /// `Authorization` header value, if credentials are configured.
    pub authorization: Option<Secret>,
    /// Request body (may be empty).
    pub body: Vec<u8>,
    /// Transports must fail with [`TransportErrorKind::BodyTooLarge`] instead
    /// of buffering more than this many response body bytes.
    pub max_response_bytes: usize,
}

/// An HTTP response.
#[derive(Debug, Clone)]
pub struct Response {
    /// Status code.
    pub status: u16,
    /// Headers (names in any case; values that are not valid UTF-8 may be
    /// dropped by the transport).
    pub headers: Vec<(String, String)>,
    /// Complete body.
    pub body: Vec<u8>,
}

impl Response {
    /// The first header with this name (case-insensitive).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Kinds of transport failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransportErrorKind {
    /// DNS, TCP or TLS connection failure (including certificate errors).
    Connect,
    /// The request timed out.
    Timeout,
    /// The response body exceeded [`Request::max_response_bytes`].
    BodyTooLarge,
    /// The request was refused before sending (for example insecure HTTP).
    Refused,
    /// Any other failure.
    Other,
}

/// A transport failure. The message must not contain secrets.
#[derive(Debug, Clone)]
pub struct TransportError {
    /// What went wrong.
    pub kind: TransportErrorKind,
    /// Human-readable details.
    pub message: String,
}

impl TransportError {
    /// Creates a transport error.
    #[must_use]
    pub fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for TransportError {}

/// An asynchronous HTTP client.
///
/// Implementations must not follow redirects, must honour
/// [`Request::max_response_bytes`] and must not log the
/// [`Request::authorization`] value.
pub trait HttpTransport: Send + Sync {
    /// Sends a request and returns the complete response.
    fn send(
        &self,
        request: Request,
    ) -> impl Future<Output = Result<Response, TransportError>> + Send;
}
