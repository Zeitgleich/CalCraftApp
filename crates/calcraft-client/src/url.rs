//! Minimal, strict `http(s)` URL handling.
//!
//! The client only ever talks to one origin (scheme, host, port), so it needs
//! far less than a general URL library: parsing absolute `http(s)` URLs,
//! resolving `DAV:href` references against a request URL (RFC 3986 section
//! 5.2) and a canonical path form so that hrefs from different responses can
//! be compared reliably even when servers percent-encode differently.
//!
//! Deliberately rejected: user info (`user:pass@host`), non-ASCII host names
//! (callers pass the punycode form), queries and non-`http(s)` schemes.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

use crate::error::Error;

/// URL scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    /// Plain HTTP. Only allowed for loopback hosts unless explicitly enabled.
    Http,
    /// HTTP over TLS.
    Https,
}

impl Scheme {
    /// The scheme as written in URLs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// An absolute `http` or `https` URL with a canonical path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Url {
    scheme: Scheme,
    /// Lower-case host; IPv6 literals keep their brackets.
    host: String,
    /// Explicit non-default port.
    port: Option<u16>,
    /// Canonical, percent-encoded absolute path (see [`canonical_path`]).
    path: String,
}

impl Url {
    /// Parses an absolute `http(s)` URL.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`] for anything that is not a plain absolute
    /// `http(s)` URL without user info and query.
    pub fn parse(input: &str) -> Result<Self, Error> {
        let input = input.trim();
        let invalid = || Error::InvalidUrl(truncate(input));
        let (scheme, rest) = if let Some(rest) = strip_prefix_ci(input, "https://") {
            (Scheme::Https, rest)
        } else if let Some(rest) = strip_prefix_ci(input, "http://") {
            (Scheme::Http, rest)
        } else {
            return Err(invalid());
        };
        let rest = rest.split('#').next().unwrap_or_default();
        if rest.contains('?') {
            return Err(invalid());
        }
        let (authority, path) = match rest.find('/') {
            Some(i) => rest.split_at(i),
            None => (rest, "/"),
        };
        let (host, port) = parse_authority(authority).ok_or_else(invalid)?;
        let port = port.filter(|p| *p != scheme.default_port());
        let path = canonical_path(path).ok_or_else(invalid)?;
        Ok(Self {
            scheme,
            host,
            port,
            path,
        })
    }

    /// Resolves a reference (for example a `DAV:href` or a `Location`
    /// header) against this URL.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`] if the reference is malformed, uses another
    /// scheme than `http(s)` or contains a query.
    pub fn join(&self, reference: &str) -> Result<Self, Error> {
        let reference = reference.trim();
        let invalid = || Error::InvalidUrl(truncate(reference));
        if reference.is_empty() {
            return Ok(self.clone());
        }
        if has_scheme(reference) {
            return Self::parse(reference);
        }
        if reference.starts_with("//") {
            return Self::parse(&format!("{}:{reference}", self.scheme.as_str()));
        }
        let reference = reference.split('#').next().unwrap_or_default();
        if reference.contains('?') {
            return Err(invalid());
        }
        let merged = if reference.starts_with('/') {
            reference.to_owned()
        } else if reference.is_empty() {
            self.path.clone()
        } else {
            let dir = &self.path[..=self.path.rfind('/').unwrap_or(0)];
            format!("{dir}{reference}")
        };
        let path = canonical_path(&merged).ok_or_else(invalid)?;
        Ok(Self {
            scheme: self.scheme,
            host: self.host.clone(),
            port: self.port,
            path,
        })
    }

    /// Returns a URL with the same origin and the given canonical path.
    pub(crate) fn with_path(&self, href: &Href) -> Self {
        Self {
            scheme: self.scheme,
            host: self.host.clone(),
            port: self.port,
            path: href.0.clone(),
        }
    }

    /// The scheme.
    #[must_use]
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// The lower-case host (IPv6 literals in brackets).
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The effective port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(self.scheme.default_port())
    }

    /// The canonical, percent-encoded path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The path as an [`Href`].
    #[must_use]
    pub fn href(&self) -> Href {
        Href(self.path.clone())
    }

    /// Whether `other` has the same scheme, host and port.
    #[must_use]
    pub fn same_origin(&self, other: &Self) -> bool {
        self.scheme == other.scheme && self.host == other.host && self.port() == other.port()
    }

    /// Whether the host is a loopback address or `localhost`.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        if self.host == "localhost" || self.host.ends_with(".localhost") {
            return true;
        }
        if let Some(v6) = self
            .host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
        {
            return v6.parse::<Ipv6Addr>().is_ok_and(|ip| ip.is_loopback());
        }
        self.host
            .parse::<Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
    }

    /// `scheme://host[:port]` without a path.
    #[must_use]
    pub fn origin(&self) -> String {
        match self.port {
            Some(port) => format!("{}://{}:{port}", self.scheme.as_str(), self.host),
            None => format!("{}://{}", self.scheme.as_str(), self.host),
        }
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.origin(), self.path)
    }
}

/// A canonical absolute path on the account's origin, as used in `DAV:href`.
///
/// Two hrefs that differ only in percent-encoding (for example `%40` vs `@`,
/// lower- vs upper-case hex) compare equal. Encoded slashes (`%2F`) are kept,
/// as they are not equivalent to a path separator.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Href(String);

impl Href {
    /// Parses an absolute path (`/dav/calendars/alice/`) into canonical form.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`] for relative or malformed paths.
    pub fn parse(path: &str) -> Result<Self, Error> {
        let path = path.trim();
        if !path.starts_with('/') || path.starts_with("//") || path.contains(['?', '#']) {
            return Err(Error::InvalidUrl(truncate(path)));
        }
        canonical_path(path)
            .map(Self)
            .ok_or_else(|| Error::InvalidUrl(truncate(path)))
    }

    /// The canonical percent-encoded path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this href names a collection (ends with `/`).
    #[must_use]
    pub fn is_collection(&self) -> bool {
        self.0.ends_with('/')
    }

    /// The href of a child resource. `name` is a single, unencoded path
    /// segment (it is percent-encoded here, including any `/`). With
    /// `collection` the result ends with `/`.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`] for empty names, `.` and `..`.
    pub fn child(&self, name: &str, collection: bool) -> Result<Self, Error> {
        if name.is_empty() || name == "." || name == ".." {
            return Err(Error::InvalidUrl(truncate(name)));
        }
        let mut path = self.0.clone();
        if !path.ends_with('/') {
            path.push('/');
        }
        for byte in name.bytes() {
            if is_unreserved(byte) || (byte != b'/' && is_path_safe(byte)) {
                path.push(char::from(byte));
            } else {
                push_encoded(&mut path, byte);
            }
        }
        if collection {
            path.push('/');
        }
        Ok(Self(path))
    }

    /// The last path segment, percent-decoded where it is valid UTF-8.
    #[must_use]
    pub fn last_segment(&self) -> String {
        let trimmed = self.0.trim_end_matches('/');
        let segment = &trimmed[trimmed.rfind('/').map_or(0, |i| i + 1)..];
        let bytes = percent_decode(segment).unwrap_or_default();
        String::from_utf8(bytes).unwrap_or_else(|_| segment.to_owned())
    }
}

impl fmt::Display for Href {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn strip_prefix_ci<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    let head = input.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &input[prefix.len()..])
}

fn has_scheme(reference: &str) -> bool {
    let Some(colon) = reference.find(':') else {
        return false;
    };
    let scheme = &reference[..colon];
    if reference[..colon].contains('/') {
        return false;
    }
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn parse_authority(authority: &str) -> Option<(String, Option<u16>)> {
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let end = rest.find(']')?;
        let literal = &rest[..end];
        literal.parse::<Ipv6Addr>().ok()?;
        let after = &rest[end + 1..];
        let port = match after {
            "" => None,
            _ => Some(after.strip_prefix(':')?),
        };
        (format!("[{}]", literal.to_ascii_lowercase()), port)
    } else {
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        let valid = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if !valid {
            return None;
        }
        (host.to_ascii_lowercase(), port)
    };
    let port = match port {
        None | Some("") => None,
        Some(digits) => {
            if !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            match digits.parse::<u16>().ok()? {
                0 => return None,
                p => Some(p),
            }
        }
    };
    Some((host, port))
}

const fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// Characters that may appear literally in a canonical path.
const fn is_path_safe(byte: u8) -> bool {
    is_unreserved(byte)
        || matches!(
            byte,
            b'!' | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'/'
        )
}

fn push_encoded(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('%');
    out.push(char::from(HEX[usize::from(byte >> 4)]));
    out.push(char::from(HEX[usize::from(byte & 0xF)]));
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = hex_value(*bytes.get(i + 1)?)?;
            let lo = hex_value(*bytes.get(i + 2)?)?;
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Canonicalises an absolute path: removes dot segments, decodes every
/// percent-escape except `%2F`, and re-encodes every byte outside the
/// path-safe set with upper-case hex. Returns `None` for malformed escapes.
fn canonical_path(path: &str) -> Option<String> {
    let path = if path.is_empty() { "/" } else { path };
    if !path.starts_with('/') {
        return None;
    }
    let mut segments: Vec<String> = Vec::new();
    let raw_segments: Vec<&str> = path[1..].split('/').collect();
    let last = raw_segments.len() - 1;
    for (index, raw) in raw_segments.iter().enumerate() {
        let mut segment = String::with_capacity(raw.len());
        let bytes = raw.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let byte = if bytes[i] == b'%' {
                let hi = hex_value(*bytes.get(i + 1)?)?;
                let lo = hex_value(*bytes.get(i + 2)?)?;
                i += 3;
                hi << 4 | lo
            } else {
                i += 1;
                bytes[i - 1]
            };
            if byte != b'/' && is_path_safe(byte) {
                segment.push(char::from(byte));
            } else {
                push_encoded(&mut segment, byte);
            }
        }
        match segment.as_str() {
            "." => {
                if index == last {
                    segments.push(String::new());
                }
            }
            ".." => {
                segments.pop();
                if index == last {
                    segments.push(String::new());
                }
            }
            _ => segments.push(segment),
        }
    }
    let mut out = String::with_capacity(path.len());
    for segment in &segments {
        out.push('/');
        out.push_str(segment);
    }
    if out.is_empty() {
        out.push('/');
    }
    Some(out)
}

fn truncate(input: &str) -> String {
    const MAX: usize = 200;
    let mut end = input.len().min(MAX);
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    input[..end]
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalises() {
        let url = Url::parse("HTTPS://Cal.Example.COM:443/dav/").unwrap();
        assert_eq!(url.to_string(), "https://cal.example.com/dav/");
        assert_eq!(url.port(), 443);
        let url = Url::parse("http://127.0.0.1:5232").unwrap();
        assert_eq!(url.to_string(), "http://127.0.0.1:5232/");
        assert!(url.is_loopback());
        let v6 = Url::parse("http://[::1]:8080/x").unwrap();
        assert_eq!(v6.host(), "[::1]");
        assert!(v6.is_loopback());
        assert!(!Url::parse("https://example.com/").unwrap().is_loopback());
    }

    #[test]
    fn rejects_dangerous_urls() {
        for bad in [
            "ftp://example.com/",
            "https://user:pass@example.com/",
            "https://@example.com/",
            "https://exa mple.com/",
            "https://exämple.com/",
            "https://example.com:0/",
            "https://example.com:99999/",
            "https://example.com/?q=1",
            "https://example.com/%zz",
            "javascript:alert(1)",
            "https:///path",
            "https://[::1/",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn joins_references() {
        let base = Url::parse("https://example.com/dav/calendars/alice/").unwrap();
        assert_eq!(
            base.join("default/").unwrap().path(),
            "/dav/calendars/alice/default/"
        );
        assert_eq!(base.join("/other/").unwrap().path(), "/other/");
        assert_eq!(base.join("../bob/").unwrap().path(), "/dav/calendars/bob/");
        assert_eq!(base.join("/a/b/../../../..").unwrap().path(), "/");
        let other = base.join("https://evil.example/x").unwrap();
        assert!(!other.same_origin(&base));
        let net = base.join("//evil.example/x").unwrap();
        assert!(!net.same_origin(&base));
        assert!(base.join("mailto:a@b").is_err());
        assert!(base.join("x?y").is_err());
        assert_eq!(
            base.join("x#frag").unwrap().path(),
            "/dav/calendars/alice/x"
        );
    }

    #[test]
    fn canonical_percent_encoding() {
        let a = Href::parse("/dav/a%40b.com/caf%c3%a9/x%2fy/").unwrap();
        let b = Href::parse("/dav/a@b.com/café/x%2Fy/").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "/dav/a@b.com/caf%C3%A9/x%2Fy/");
        let spaced = Href::parse("/dav/My Calendar/").unwrap();
        assert_eq!(spaced.as_str(), "/dav/My%20Calendar/");
        assert_eq!(spaced.last_segment(), "My Calendar");
        assert!(Href::parse("relative/").is_err());
        assert!(Href::parse("//host/x").is_err());
    }

    #[test]
    fn child_segments_are_encoded() {
        let home = Href::parse("/dav/calendars/alice/").unwrap();
        assert_eq!(
            home.child("work", true).unwrap().as_str(),
            "/dav/calendars/alice/work/"
        );
        assert_eq!(
            home.child("a/b?#%.ics", false).unwrap().as_str(),
            "/dav/calendars/alice/a%2Fb%3F%23%25.ics"
        );
        assert!(home.child("..", true).is_err());
        assert!(home.child("", false).is_err());
    }
}
