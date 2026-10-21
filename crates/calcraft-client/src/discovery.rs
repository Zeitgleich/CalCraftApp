//! Service discovery (RFC 6764 section 6, RFC 5397, RFC 4791 section 6.2.1,
//! RFC 6352 section 7.1.1).

use std::collections::HashSet;

use crate::client::{DavClient, Depth, status_error};
use crate::error::Error;
use crate::transport::{HttpTransport, Method};
use crate::url::{Href, Url};
use crate::xml::{Element, NS_CALDAV, NS_CARDDAV, NS_DAV, XMLNS};

/// The result of [`DavClient::discover`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    /// The URL that answered with the principal (after redirects).
    pub context_url: Url,
    /// The authenticated user's principal.
    pub principal: Href,
    /// The principal's display name.
    pub display_name: Option<String>,
    /// `CALDAV:calendar-home-set` (empty if the server has no CalDAV).
    pub calendar_homes: Vec<Href>,
    /// `CARDDAV:addressbook-home-set` (empty if the server has no CardDAV).
    pub addressbook_homes: Vec<Href>,
}

const PRINCIPAL_QUERY: &str = "<d:prop><d:current-user-principal/></d:prop>";
const HOME_QUERY: &str =
    "<d:prop><c:calendar-home-set/><card:addressbook-home-set/><d:displayname/></d:prop>";

fn propfind_body(prop: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?><d:propfind {XMLNS}>{prop}</d:propfind>"#)
}

impl<T: HttpTransport> DavClient<T> {
    /// Finds the principal and the calendar/address book home sets.
    ///
    /// Tries, in order: the base URL itself (when it has a path),
    /// `/.well-known/caldav`, `/.well-known/carddav` and `/`. Same-origin
    /// redirects are followed up to
    /// [`ClientOptions::max_redirects`](crate::ClientOptions::max_redirects);
    /// redirects to other origins stop discovery
    /// with [`Error::CrossOriginRedirect`] so the app can ask the user.
    ///
    /// # Errors
    /// [`Error::Unauthorized`], [`Error::CrossOriginRedirect`],
    /// [`Error::TooManyRedirects`], [`Error::DiscoveryFailed`], transport and
    /// XML errors.
    pub async fn discover(&self) -> Result<Discovery, Error> {
        let base = self.base_url().clone();
        let mut candidates = Vec::new();
        if base.path() != "/" {
            candidates.push(base.clone());
        }
        for path in ["/.well-known/caldav", "/.well-known/carddav", "/"] {
            candidates.push(base.join(path)?);
        }
        for candidate in candidates {
            match self.find_principal(&candidate).await {
                Ok(Some((context_url, principal))) => {
                    return self.load_principal(context_url, principal).await;
                }
                Ok(None) => {}
                Err(
                    error @ (Error::Unauthorized(_)
                    | Error::CrossOriginRedirect(_)
                    | Error::TooManyRedirects(_)
                    | Error::CrossOrigin(_)
                    | Error::Transport(_)),
                ) => return Err(error),
                // Not a DAV endpoint (404, 405, HTML error page, ...).
                Err(_) => {}
            }
        }
        Err(Error::DiscoveryFailed(base.to_string()))
    }

    /// PROPFIND `current-user-principal`, following same-origin redirects.
    async fn find_principal(&self, start: &Url) -> Result<Option<(Url, Href)>, Error> {
        let mut url = start.clone();
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(url.clone()) || visited.len() > self.options().max_redirects + 1 {
                return Err(Error::TooManyRedirects(url.to_string()));
            }
            let response = self
                .send(
                    Method::Propfind,
                    &url,
                    vec![
                        ("content-type", "application/xml; charset=utf-8".to_owned()),
                        ("depth", Depth::Zero.as_str().to_owned()),
                    ],
                    propfind_body(PRINCIPAL_QUERY).into_bytes(),
                    self.options().max_xml_bytes,
                )
                .await?;
            match response.status {
                301 | 302 | 303 | 307 | 308 => {
                    let location = response.header("location").ok_or_else(|| {
                        Error::InvalidResponse("redirect without Location".into())
                    })?;
                    let next = url.join(location)?;
                    if !next.same_origin(self.base_url()) {
                        return Err(Error::CrossOriginRedirect(next.to_string()));
                    }
                    url = next;
                }
                207 => {
                    let multistatus = self.parse_multistatus(&response, &url)?;
                    let principal = multistatus
                        .responses
                        .iter()
                        .find_map(|r| r.prop(NS_DAV, "current-user-principal"))
                        .map(|prop| self.prop_hrefs(&url, prop))
                        .transpose()?
                        .and_then(|hrefs| hrefs.into_iter().next());
                    return Ok(principal.map(|p| (url, p)));
                }
                _ => return Err(status_error(&response, &url)),
            }
        }
    }

    async fn load_principal(&self, context_url: Url, principal: Href) -> Result<Discovery, Error> {
        let url = self.url_for(&principal);
        let multistatus = self
            .xml_request(
                Method::Propfind,
                &url,
                Some(Depth::Zero),
                propfind_body(HOME_QUERY),
            )
            .await?;
        let mut discovery = Discovery {
            context_url,
            principal,
            display_name: None,
            calendar_homes: Vec::new(),
            addressbook_homes: Vec::new(),
        };
        for response in &multistatus.responses {
            if let Some(prop) = response.prop(NS_CALDAV, "calendar-home-set") {
                discovery.calendar_homes = self.prop_hrefs(&url, prop)?;
            }
            if let Some(prop) = response.prop(NS_CARDDAV, "addressbook-home-set") {
                discovery.addressbook_homes = self.prop_hrefs(&url, prop)?;
            }
            if discovery.display_name.is_none() {
                discovery.display_name = response.prop_text(NS_DAV, "displayname");
            }
        }
        Ok(discovery)
    }

    /// The `DAV:href` children of a property on the account origin.
    /// Foreign or malformed hrefs are dropped; an error is returned only if
    /// the property had hrefs and none of them is usable.
    pub(crate) fn prop_hrefs(&self, request_url: &Url, prop: &Element) -> Result<Vec<Href>, Error> {
        let mut hrefs = Vec::new();
        let mut first_rejected = None;
        for href in prop.children_named(NS_DAV, "href") {
            let error = match request_url.join(&href.text) {
                Ok(url) if url.same_origin(self.base_url()) => {
                    let href = url.href();
                    if !hrefs.contains(&href) {
                        hrefs.push(href);
                    }
                    continue;
                }
                Ok(url) => Error::CrossOrigin(url.to_string()),
                Err(error) => error,
            };
            first_rejected.get_or_insert(error);
        }
        match first_rejected {
            Some(error) if hrefs.is_empty() => Err(error),
            _ => Ok(hrefs),
        }
    }
}
