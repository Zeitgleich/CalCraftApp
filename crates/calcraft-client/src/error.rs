//! Error types.
//!
//! No error message ever contains credentials: URLs cannot carry user info
//! (rejected while parsing) and the `Authorization` header is never echoed.

use std::fmt;

use crate::transport::TransportError;

/// Errors returned by the client.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A URL or href could not be parsed or is not allowed.
    InvalidUrl(String),
    /// Plain `http://` to a non-loopback host without
    /// [`ClientOptions::allow_insecure_http`](crate::ClientOptions::allow_insecure_http).
    InsecureUrl(String),
    /// A URL outside the account's origin. The request was not sent, so
    /// credentials never leave the configured origin.
    CrossOrigin(String),
    /// A redirect pointed to another origin. It was not followed; the caller
    /// may ask the user and start over with that URL.
    CrossOriginRedirect(String),
    /// More redirects than allowed, or a redirect loop.
    TooManyRedirects(String),
    /// Service discovery did not find a `current-user-principal`.
    DiscoveryFailed(String),
    /// Invalid credentials (HTTP 401).
    Unauthorized(String),
    /// The resource does not exist (HTTP 404 or 410).
    NotFound(String),
    /// `If-Match` / `If-None-Match` failed (HTTP 412): the resource was
    /// changed or created concurrently. Fetch it again and merge.
    PreconditionFailed(String),
    /// The server rejected the sync token (`DAV:valid-sync-token`); a full
    /// resynchronisation is required.
    InvalidSyncToken(String),
    /// Any other unexpected HTTP status. `condition` is the local name of the
    /// first precondition element in a `DAV:error` body, if any.
    Status {
        /// HTTP status code.
        status: u16,
        /// Request URL.
        url: String,
        /// Precondition/postcondition name such as `supported-report`.
        condition: Option<String>,
    },
    /// A response body exceeded the configured limit.
    BodyTooLarge(usize),
    /// Malformed or hostile XML (including DTDs and limit violations).
    Xml(String),
    /// A response that is well-formed but not what the protocol requires.
    InvalidResponse(String),
    /// An object body is not valid UTF-8 or exceeds the size limit.
    InvalidObject(String),
    /// Invalid input from the caller (for example a user name with `:`).
    InvalidInput(String),
    /// The HTTP transport failed.
    Transport(TransportError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(url) => write!(f, "invalid URL: {url}"),
            Self::InsecureUrl(url) => write!(f, "refusing unencrypted HTTP to {url}"),
            Self::CrossOrigin(url) => {
                write!(f, "refusing request outside the account origin: {url}")
            }
            Self::CrossOriginRedirect(url) => {
                write!(f, "redirect to another origin not followed: {url}")
            }
            Self::TooManyRedirects(url) => {
                write!(f, "too many redirects or redirect loop at {url}")
            }
            Self::DiscoveryFailed(url) => write!(f, "no CalDAV/CardDAV service found at {url}"),
            Self::Unauthorized(url) => write!(f, "authentication failed for {url}"),
            Self::NotFound(url) => write!(f, "not found: {url}"),
            Self::PreconditionFailed(url) => write!(f, "resource was modified concurrently: {url}"),
            Self::InvalidSyncToken(url) => {
                write!(f, "sync token rejected, full resync needed: {url}")
            }
            Self::Status {
                status,
                url,
                condition: Some(condition),
            } => write!(f, "HTTP {status} ({condition}) for {url}"),
            Self::Status { status, url, .. } => write!(f, "HTTP {status} for {url}"),
            Self::BodyTooLarge(limit) => write!(f, "response body exceeds {limit} bytes"),
            Self::Xml(message) => write!(f, "invalid XML: {message}"),
            Self::InvalidResponse(message) => write!(f, "invalid response: {message}"),
            Self::InvalidObject(message) => write!(f, "invalid object: {message}"),
            Self::InvalidInput(message) => write!(f, "invalid input: {message}"),
            Self::Transport(error) => write!(f, "transport error: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}

impl From<TransportError> for Error {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}
