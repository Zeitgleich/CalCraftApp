//! The client core: origin policy, authentication and response handling.

use crate::auth::Credentials;
use crate::error::Error;
use crate::transport::{HttpTransport, Method, Request, Response};
use crate::url::{Href, Scheme, Url};
use crate::xml::{Multistatus, XmlLimits, parse_error_condition, parse_multistatus};

/// Tunable limits and policies. Start from [`ClientOptions::default`] and
/// change individual fields.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ClientOptions {
    /// Allow plain `http://` to non-loopback hosts. Off by default: Basic
    /// credentials would be sent in clear text.
    pub allow_insecure_http: bool,
    /// Maximum number of same-origin redirects followed during discovery.
    pub max_redirects: usize,
    /// Maximum size of an XML response body.
    pub max_xml_bytes: usize,
    /// Maximum XML nesting depth.
    pub max_xml_depth: usize,
    /// Maximum number of XML elements per response.
    pub max_xml_elements: usize,
    /// Maximum size of one calendar object or vCard.
    pub max_object_bytes: usize,
    /// Number of hrefs per `calendar-multiget` / `addressbook-multiget`.
    pub multiget_batch_size: usize,
    /// Maximum number of `sync-collection` continuation requests (507
    /// truncation) in one sync run.
    pub max_sync_pages: usize,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            allow_insecure_http: false,
            max_redirects: 5,
            max_xml_bytes: 32 * 1024 * 1024,
            max_xml_depth: 32,
            max_xml_elements: 500_000,
            max_object_bytes: 4 * 1024 * 1024,
            multiget_batch_size: 100,
            max_sync_pages: 1000,
        }
    }
}

/// `Depth` header values.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Depth {
    Zero,
    One,
}

impl Depth {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "0",
            Self::One => "1",
        }
    }
}

/// A CalDAV/CardDAV client bound to one account on one origin.
///
/// Every request goes to the origin (scheme, host and port) of the base URL;
/// URLs on any other origin are refused before anything is sent, so the
/// credentials cannot leak to another host, whatever the server returns.
pub struct DavClient<T> {
    transport: T,
    base: Url,
    credentials: Credentials,
    options: ClientOptions,
}

impl<T> std::fmt::Debug for DavClient<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DavClient")
            .field("base", &self.base.to_string())
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

impl<T: HttpTransport> DavClient<T> {
    /// Creates a client with default options.
    ///
    /// `base_url` is what the user entered, for example
    /// `https://cal.example.com` or `https://cloud.example.com/remote.php/dav/`.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`] or [`Error::InsecureUrl`].
    pub fn new(base_url: &str, credentials: Credentials, transport: T) -> Result<Self, Error> {
        Self::with_options(base_url, credentials, transport, ClientOptions::default())
    }

    /// Creates a client with custom options.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`] or [`Error::InsecureUrl`].
    pub fn with_options(
        base_url: &str,
        credentials: Credentials,
        transport: T,
        options: ClientOptions,
    ) -> Result<Self, Error> {
        let base = Url::parse(base_url)?;
        if base.scheme() == Scheme::Http && !base.is_loopback() && !options.allow_insecure_http {
            return Err(Error::InsecureUrl(base.to_string()));
        }
        Ok(Self {
            transport,
            base,
            credentials,
            options,
        })
    }

    /// The base URL.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// The absolute URL of an href on the account origin.
    #[must_use]
    pub fn url_for(&self, href: &Href) -> Url {
        self.base.with_path(href)
    }

    /// The transport.
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// The options.
    #[must_use]
    pub fn options(&self) -> &ClientOptions {
        &self.options
    }

    pub(crate) fn xml_limits(&self) -> XmlLimits {
        XmlLimits {
            max_bytes: self.options.max_xml_bytes,
            max_depth: self.options.max_xml_depth,
            max_elements: self.options.max_xml_elements,
        }
    }

    /// Sends one request to `url`, which must be on the account origin.
    pub(crate) async fn send(
        &self,
        method: Method,
        url: &Url,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
        max_response_bytes: usize,
    ) -> Result<Response, Error> {
        if !url.same_origin(&self.base) {
            return Err(Error::CrossOrigin(url.to_string()));
        }
        if let Some((name, _)) = headers.iter().find(|(_, v)| !header_safe(v)) {
            return Err(Error::InvalidInput(format!(
                "unsafe value for header {name}"
            )));
        }
        let request = Request {
            method,
            url: url.clone(),
            headers,
            authorization: Some(self.credentials.authorization()),
            body,
            max_response_bytes,
        };
        let response = self.transport.send(request).await?;
        if response.body.len() > max_response_bytes {
            return Err(Error::BodyTooLarge(max_response_bytes));
        }
        Ok(response)
    }

    /// Sends an XML request and expects a `207 Multi-Status`.
    pub(crate) async fn xml_request(
        &self,
        method: Method,
        url: &Url,
        depth: Option<Depth>,
        body: String,
    ) -> Result<Multistatus, Error> {
        let mut headers = vec![("content-type", "application/xml; charset=utf-8".to_owned())];
        if let Some(depth) = depth {
            headers.push(("depth", depth.as_str().to_owned()));
        }
        let response = self
            .send(
                method,
                url,
                headers,
                body.into_bytes(),
                self.options.max_xml_bytes,
            )
            .await?;
        if response.status != 207 {
            return Err(status_error(&response, url));
        }
        self.parse_multistatus(&response, url)
    }

    pub(crate) fn parse_multistatus(
        &self,
        response: &Response,
        url: &Url,
    ) -> Result<Multistatus, Error> {
        let text = body_text(&response.body)?;
        parse_multistatus(text, url, &self.xml_limits())
    }
}

/// The response body as UTF-8 text without BOM.
pub(crate) fn body_text(body: &[u8]) -> Result<&str, Error> {
    let text =
        std::str::from_utf8(body).map_err(|_| Error::Xml("response is not valid UTF-8".into()))?;
    Ok(text.strip_prefix('\u{FEFF}').unwrap_or(text))
}

/// Maps an unexpected status to an error.
pub(crate) fn status_error(response: &Response, url: &Url) -> Error {
    let url = url.to_string();
    match response.status {
        401 => Error::Unauthorized(url),
        404 | 410 => Error::NotFound(url),
        412 => Error::PreconditionFailed(url),
        status => {
            let is_xml = response
                .header("content-type")
                .is_none_or(|ct| ct.to_ascii_lowercase().contains("xml"));
            let condition = if is_xml {
                std::str::from_utf8(&response.body)
                    .ok()
                    .and_then(parse_error_condition)
            } else {
                None
            };
            if condition.as_deref() == Some("valid-sync-token") {
                return Error::InvalidSyncToken(url);
            }
            Error::Status {
                status,
                url,
                condition,
            }
        }
    }
}

/// Printable ASCII only: no CR/LF header injection, no control characters.
fn header_safe(value: &str) -> bool {
    value.bytes().all(|b| (0x20..0x7F).contains(&b))
}

/// Formats an entity tag for `If-Match`, adding quotes if a server returned
/// it unquoted.
pub(crate) fn quoted_etag(etag: &str) -> Result<String, Error> {
    let etag = etag.trim();
    let quoted = etag.starts_with('"') || etag.starts_with("W/\"");
    if etag.is_empty() || !header_safe(etag) || (!quoted && etag.contains('"')) {
        return Err(Error::InvalidInput("malformed entity tag".into()));
    }
    Ok(if quoted {
        etag.to_owned()
    } else {
        format!("\"{etag}\"")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_quoting() {
        assert_eq!(quoted_etag("\"abc\"").unwrap(), "\"abc\"");
        assert_eq!(quoted_etag("W/\"abc\"").unwrap(), "W/\"abc\"");
        assert_eq!(quoted_etag("abc").unwrap(), "\"abc\"");
        assert!(quoted_etag("a\"b").is_err());
        assert!(quoted_etag("\"a\"\r\nX-Evil: 1").is_err());
        assert!(quoted_etag("").is_err());
    }
}
