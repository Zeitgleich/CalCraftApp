//! `DAV:multistatus` (RFC 4918 section 13) and `DAV:error` bodies.

use crate::error::Error;
use crate::url::{Href, Url};

use super::{Element, NS_CALDAV, NS_CARDDAV, NS_DAV, XmlLimits, parse};

/// A parsed multistatus body. Hrefs are resolved against the request URL;
/// responses whose href is malformed or on another origin are dropped and
/// only counted, so nothing the server says can make the client talk to a
/// different host.
#[derive(Debug)]
pub(crate) struct Multistatus {
    pub responses: Vec<ResponseItem>,
    /// Top-level `DAV:sync-token` (RFC 6578).
    pub sync_token: Option<String>,
    /// Number of responses dropped because of a foreign or invalid href
    /// (diagnostics; checked by tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub rejected_hrefs: usize,
}

#[derive(Debug)]
pub(crate) struct ResponseItem {
    pub href: Href,
    /// Response-level status (`href, status` form).
    pub status: Option<u16>,
    pub propstats: Vec<PropStat>,
}

#[derive(Debug)]
pub(crate) struct PropStat {
    pub status: u16,
    pub props: Vec<Element>,
}

impl ResponseItem {
    /// A property from a successful propstat.
    pub fn prop(&self, ns: &str, name: &str) -> Option<&Element> {
        self.propstats
            .iter()
            .filter(|p| (200..300).contains(&p.status))
            .flat_map(|p| p.props.iter())
            .find(|e| e.is(ns, name))
    }

    /// Text value of a successful property.
    pub fn prop_text(&self, ns: &str, name: &str) -> Option<String> {
        self.prop(ns, name).and_then(Element::text_value)
    }

    /// The effective status: the response-level status; otherwise 404 when
    /// every propstat is 404 (sabre reports missing multiget hrefs that
    /// way), else 200.
    pub fn effective_status(&self) -> u16 {
        match self.status {
            Some(status) => status,
            None if !self.propstats.is_empty()
                && self.propstats.iter().all(|p| p.status == 404) =>
            {
                404
            }
            None => 200,
        }
    }
}

fn parse_status(line: &str) -> Option<u16> {
    let mut parts = line.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    let code = parts.next()?;
    if code.len() != 3 {
        return None;
    }
    code.parse().ok().filter(|c| (100..600).contains(c))
}

/// Parses a multistatus document for a request to `request_url`.
pub(crate) fn parse_multistatus(
    body: &str,
    request_url: &Url,
    limits: &XmlLimits,
) -> Result<Multistatus, Error> {
    let root = parse(body, limits)?;
    if !root.is(NS_DAV, "multistatus") {
        return Err(Error::InvalidResponse(
            "expected a DAV:multistatus document".into(),
        ));
    }
    let mut responses = Vec::new();
    let mut rejected_hrefs = 0;
    for response in root.children_named(NS_DAV, "response") {
        let status = match response.child(NS_DAV, "status") {
            Some(status) => Some(parse_status(&status.text).ok_or_else(|| {
                Error::InvalidResponse("malformed status line in multistatus".into())
            })?),
            None => None,
        };
        let mut propstats = Vec::new();
        for propstat in response.children_named(NS_DAV, "propstat") {
            // A propstat without a parsable status is treated as failed.
            let status = propstat
                .child(NS_DAV, "status")
                .and_then(|s| parse_status(&s.text))
                .unwrap_or(500);
            let props = propstat
                .child(NS_DAV, "prop")
                .map(|p| p.children.clone())
                .unwrap_or_default();
            propstats.push(PropStat { status, props });
        }
        // RFC 4918 allows several hrefs only in the `href*, status` form.
        let hrefs: Vec<&Element> = response.children_named(NS_DAV, "href").collect();
        if hrefs.is_empty() || (hrefs.len() > 1 && status.is_none()) {
            return Err(Error::InvalidResponse(
                "multistatus response without a valid href".into(),
            ));
        }
        for href in hrefs {
            match request_url.join(&href.text) {
                Ok(url) if url.same_origin(request_url) => responses.push(ResponseItem {
                    href: url.href(),
                    status,
                    propstats: clone_propstats(&propstats),
                }),
                _ => rejected_hrefs += 1,
            }
        }
    }
    let sync_token = root
        .child(NS_DAV, "sync-token")
        .and_then(Element::text_value);
    Ok(Multistatus {
        responses,
        sync_token,
        rejected_hrefs,
    })
}

fn clone_propstats(propstats: &[PropStat]) -> Vec<PropStat> {
    propstats
        .iter()
        .map(|p| PropStat {
            status: p.status,
            props: p.props.clone(),
        })
        .collect()
}

/// The local name of the first precondition in a `DAV:error` body.
pub(crate) fn parse_error_condition(body: &str) -> Option<String> {
    let limits = XmlLimits {
        max_bytes: 64 * 1024,
        max_depth: 8,
        max_elements: 256,
    };
    let root = parse(body, &limits).ok()?;
    if !root.is(NS_DAV, "error") {
        return None;
    }
    // Prefer standard conditions over vendor details (sabre puts its
    // `s:exception` and `s:message` elements first).
    let standard = |c: &&Element| [NS_DAV, NS_CALDAV, NS_CARDDAV].contains(&c.ns.as_str());
    root.children
        .iter()
        .find(standard)
        .or_else(|| root.children.first())
        .map(|c| c.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines() {
        assert_eq!(parse_status("HTTP/1.1 200 OK"), Some(200));
        assert_eq!(parse_status(" HTTP/1.1 404 Not Found\n"), Some(404));
        assert_eq!(parse_status("HTTP/2 507"), Some(507));
        assert_eq!(parse_status("200 OK"), None);
        assert_eq!(parse_status("HTTP/1.1 99999 X"), None);
    }

    #[test]
    fn foreign_hrefs_are_dropped() {
        let url = Url::parse("https://cal.example.com/dav/cal/").unwrap();
        let body = r#"<d:multistatus xmlns:d="DAV:">
            <d:response><d:href>https://evil.example/steal</d:href><d:status>HTTP/1.1 200 OK</d:status></d:response>
            <d:response><d:href>//evil.example/x</d:href><d:status>HTTP/1.1 200 OK</d:status></d:response>
            <d:response><d:href>http://cal.example.com/dav/cal/a.ics</d:href><d:status>HTTP/1.1 200 OK</d:status></d:response>
            <d:response><d:href>bad%zz</d:href><d:status>HTTP/1.1 200 OK</d:status></d:response>
            <d:response><d:href>https://CAL.example.com:443/dav/cal/b.ics</d:href><d:status>HTTP/1.1 200 OK</d:status></d:response>
            <d:response><d:href>c.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
        </d:multistatus>"#;
        let ms = parse_multistatus(body, &url, &XmlLimits::default()).unwrap();
        let hrefs: Vec<&str> = ms.responses.iter().map(|r| r.href.as_str()).collect();
        assert_eq!(hrefs, ["/dav/cal/b.ics", "/dav/cal/c.ics"]);
        assert_eq!(ms.rejected_hrefs, 4);
        assert_eq!(ms.responses[1].effective_status(), 404);
    }

    #[test]
    fn error_conditions() {
        assert_eq!(
            parse_error_condition(
                r#"<?xml version="1.0"?><D:error xmlns:D="DAV:"><D:valid-sync-token/></D:error>"#
            )
            .as_deref(),
            Some("valid-sync-token")
        );
        assert_eq!(parse_error_condition("<html>nope</html>"), None);
    }
}
