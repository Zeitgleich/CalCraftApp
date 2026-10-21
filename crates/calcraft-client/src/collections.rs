//! Listing, creating and changing calendars and address books.

use crate::client::{DavClient, Depth, status_error};
use crate::error::Error;
use crate::transport::{HttpTransport, Method};
use crate::url::Href;
use crate::xml::{NS_APPLE_ICAL, NS_CALDAV, NS_CALSERVER, NS_CARDDAV, NS_DAV, XMLNS, escape};

/// What a collection contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CollectionKind {
    /// A CalDAV calendar (`text/calendar` objects).
    Calendar,
    /// A CardDAV address book (`text/vcard` objects).
    AddressBook,
}

impl CollectionKind {
    pub(crate) fn content_type(self) -> &'static str {
        match self {
            Self::Calendar => "text/calendar; charset=utf-8",
            Self::AddressBook => "text/vcard; charset=utf-8",
        }
    }
}

/// A calendar or address book in a home collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    /// Canonical href (ends with `/`).
    pub href: Href,
    /// Calendar or address book.
    pub kind: CollectionKind,
    /// `DAV:displayname`.
    pub display_name: Option<String>,
    /// `calendar-description` / `addressbook-description`.
    pub description: Option<String>,
    /// Apple `calendar-color`, only if it is a valid `#RRGGBB[AA]` value.
    pub color: Option<String>,
    /// Supported component names (`VEVENT`, `VTODO`, ...); empty when the
    /// server does not say (RFC 4791: then all are allowed).
    pub supported_components: Vec<String>,
    /// `CS:getctag` (changes whenever the contents change).
    pub ctag: Option<String>,
    /// `DAV:sync-token` (RFC 6578).
    pub sync_token: Option<String>,
    /// Whether `DAV:supported-report-set` lists `sync-collection`, or the
    /// collection has a sync token.
    pub supports_sync: bool,
}

/// Properties for creating or updating a collection. `None` fields are left
/// unchanged (update) or unset (create).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectionProps {
    /// Display name.
    pub display_name: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Colour as `#RRGGBB` or `#RRGGBBAA` (calendars only).
    pub color: Option<String>,
    /// Supported components for new calendars (for example `["VEVENT"]`).
    /// Ignored for updates and address books.
    pub components: Vec<String>,
}

const LIST_QUERY: &str = concat!(
    "<d:prop><d:resourcetype/><d:displayname/><cs:getctag/><d:sync-token/>",
    "<d:supported-report-set/><c:calendar-description/><card:addressbook-description/>",
    "<ic:calendar-color/><c:supported-calendar-component-set/></d:prop>"
);

const MAX_TEXT_CHARS: usize = 1024;

/// Trimmed text without control characters, at most `MAX_TEXT_CHARS`.
pub(crate) fn clean_text(text: Option<String>) -> Option<String> {
    let text = text?;
    let cleaned: String = text
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT_CHARS)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn valid_color(color: &str) -> bool {
    let Some(hex) = color.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_component(name: &str) -> bool {
    !name.is_empty() && name.len() <= 32 && name.bytes().all(|b| b.is_ascii_uppercase())
}

impl<T: HttpTransport> DavClient<T> {
    /// Lists the calendars and address books in a home collection
    /// (`PROPFIND Depth: 1`). Other resources (scheduling inbox/outbox,
    /// subscriptions, plain folders) are skipped.
    ///
    /// # Errors
    /// HTTP, transport and XML errors.
    pub async fn list_collections(&self, home: &Href) -> Result<Vec<Collection>, Error> {
        let url = self.url_for(home);
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:propfind {XMLNS}>{LIST_QUERY}</d:propfind>"#
        );
        let multistatus = self
            .xml_request(Method::Propfind, &url, Some(Depth::One), body)
            .await?;
        let mut collections = Vec::new();
        for response in &multistatus.responses {
            if &response.href == home {
                continue;
            }
            let Some(resource_type) = response.prop(NS_DAV, "resourcetype") else {
                continue;
            };
            let kind = if resource_type.child(NS_CALDAV, "calendar").is_some() {
                CollectionKind::Calendar
            } else if resource_type.child(NS_CARDDAV, "addressbook").is_some() {
                CollectionKind::AddressBook
            } else {
                continue;
            };
            let description = match kind {
                CollectionKind::Calendar => response.prop_text(NS_CALDAV, "calendar-description"),
                CollectionKind::AddressBook => {
                    response.prop_text(NS_CARDDAV, "addressbook-description")
                }
            };
            let supported_components = response
                .prop(NS_CALDAV, "supported-calendar-component-set")
                .map(|set| {
                    set.children_named(NS_CALDAV, "comp")
                        .filter_map(|c| c.attr("name"))
                        .map(str::to_ascii_uppercase)
                        .filter(|n| valid_component(n))
                        .collect()
                })
                .unwrap_or_default();
            let sync_token = response.prop_text(NS_DAV, "sync-token");
            let reports_sync = response
                .prop(NS_DAV, "supported-report-set")
                .is_some_and(|set| {
                    set.children_named(NS_DAV, "supported-report").any(|r| {
                        r.child(NS_DAV, "report")
                            .is_some_and(|r| r.child(NS_DAV, "sync-collection").is_some())
                    })
                });
            collections.push(Collection {
                href: response.href.clone(),
                kind,
                display_name: clean_text(response.prop_text(NS_DAV, "displayname")),
                description: clean_text(description),
                color: response
                    .prop_text(NS_APPLE_ICAL, "calendar-color")
                    .filter(|c| valid_color(c)),
                supported_components,
                ctag: response.prop_text(NS_CALSERVER, "getctag"),
                supports_sync: reports_sync || sync_token.is_some(),
                sync_token,
            });
        }
        Ok(collections)
    }

    /// Creates a calendar with `MKCALENDAR` (RFC 4791 section 5.3.1).
    ///
    /// # Errors
    /// [`Error::InvalidInput`] for invalid properties, HTTP errors (405 if
    /// the collection already exists).
    pub async fn create_calendar(&self, href: &Href, props: &CollectionProps) -> Result<(), Error> {
        let prop = prop_xml(CollectionKind::Calendar, props, true)?;
        let set = if prop.is_empty() {
            String::new()
        } else {
            format!("<d:set><d:prop>{prop}</d:prop></d:set>")
        };
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><c:mkcalendar {XMLNS}>{set}</c:mkcalendar>"#
        );
        self.create(Method::Mkcalendar, href, body).await
    }

    /// Creates an address book with extended `MKCOL` (RFC 5689).
    ///
    /// # Errors
    /// [`Error::InvalidInput`] for invalid properties, HTTP errors.
    pub async fn create_addressbook(
        &self,
        href: &Href,
        props: &CollectionProps,
    ) -> Result<(), Error> {
        let prop = prop_xml(CollectionKind::AddressBook, props, true)?;
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:mkcol {XMLNS}><d:set><d:prop><d:resourcetype><d:collection/><card:addressbook/></d:resourcetype>{prop}</d:prop></d:set></d:mkcol>"#
        );
        self.create(Method::Mkcol, href, body).await
    }

    async fn create(&self, method: Method, href: &Href, body: String) -> Result<(), Error> {
        if !href.is_collection() {
            return Err(Error::InvalidInput(
                "collection href must end with '/'".into(),
            ));
        }
        let url = self.url_for(href);
        let response = self
            .send(
                method,
                &url,
                vec![("content-type", "application/xml; charset=utf-8".to_owned())],
                body.into_bytes(),
                self.options().max_xml_bytes,
            )
            .await?;
        match response.status {
            200 | 201 | 204 => Ok(()),
            _ => Err(status_error(&response, &url)),
        }
    }

    /// Changes display name, description and/or colour with `PROPPATCH`.
    ///
    /// # Errors
    /// [`Error::Status`] with the rejected property name as `condition` if
    /// the server refused a property; HTTP and transport errors.
    pub async fn update_collection(
        &self,
        href: &Href,
        kind: CollectionKind,
        props: &CollectionProps,
    ) -> Result<(), Error> {
        let prop = prop_xml(kind, props, false)?;
        if prop.is_empty() {
            return Ok(());
        }
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:propertyupdate {XMLNS}><d:set><d:prop>{prop}</d:prop></d:set></d:propertyupdate>"#
        );
        let url = self.url_for(href);
        let multistatus = self
            .xml_request(Method::Proppatch, &url, None, body)
            .await?;
        for response in &multistatus.responses {
            if let Some(status) = response.status.filter(|s| !(200..300).contains(s)) {
                return Err(Error::Status {
                    status,
                    url: url.to_string(),
                    condition: None,
                });
            }
            for propstat in &response.propstats {
                if !(200..300).contains(&propstat.status) {
                    return Err(Error::Status {
                        status: propstat.status,
                        url: url.to_string(),
                        condition: propstat.props.first().map(|p| p.name.clone()),
                    });
                }
            }
        }
        Ok(())
    }

    /// Deletes a collection and everything in it.
    ///
    /// # Errors
    /// HTTP and transport errors.
    pub async fn delete_collection(&self, href: &Href) -> Result<(), Error> {
        if !href.is_collection() {
            return Err(Error::InvalidInput(
                "collection href must end with '/'".into(),
            ));
        }
        self.delete_resource(href, None).await
    }
}

fn prop_xml(kind: CollectionKind, props: &CollectionProps, create: bool) -> Result<String, Error> {
    let mut xml = String::new();
    if let Some(name) = &props.display_name {
        xml.push_str(&format!("<d:displayname>{}</d:displayname>", escape(name)));
    }
    if let Some(description) = &props.description {
        let element = match kind {
            CollectionKind::Calendar => "c:calendar-description",
            CollectionKind::AddressBook => "card:addressbook-description",
        };
        xml.push_str(&format!("<{element}>{}</{element}>", escape(description)));
    }
    if let Some(color) = &props.color {
        if kind != CollectionKind::Calendar || !valid_color(color) {
            return Err(Error::InvalidInput(
                "colour must be #RRGGBB or #RRGGBBAA on a calendar".into(),
            ));
        }
        xml.push_str(&format!("<ic:calendar-color>{color}</ic:calendar-color>"));
    }
    if create && kind == CollectionKind::Calendar && !props.components.is_empty() {
        xml.push_str("<c:supported-calendar-component-set>");
        for component in &props.components {
            if !valid_component(component) {
                return Err(Error::InvalidInput("invalid component name".into()));
            }
            xml.push_str(&format!(r#"<c:comp name="{component}"/>"#));
        }
        xml.push_str("</c:supported-calendar-component-set>");
    }
    Ok(xml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_xml_is_escaped_and_validated() {
        let props = CollectionProps {
            display_name: Some("Work & <Travel>".into()),
            color: Some("#336699".into()),
            components: vec!["VEVENT".into()],
            ..CollectionProps::default()
        };
        let xml = prop_xml(CollectionKind::Calendar, &props, true).unwrap();
        assert!(xml.contains("<d:displayname>Work &amp; &lt;Travel&gt;</d:displayname>"));
        assert!(xml.contains(r#"<c:comp name="VEVENT"/>"#));
        let bad = CollectionProps {
            color: Some("red;}</ic:calendar-color>".into()),
            ..CollectionProps::default()
        };
        assert!(prop_xml(CollectionKind::Calendar, &bad, false).is_err());
        let bad = CollectionProps {
            components: vec![r#"VEVENT"/><x"#.into()],
            ..CollectionProps::default()
        };
        assert!(prop_xml(CollectionKind::Calendar, &bad, true).is_err());
    }

    #[test]
    fn text_cleaning() {
        assert_eq!(
            clean_text(Some("  a\u{7}b \n".into())).as_deref(),
            Some("ab")
        );
        assert_eq!(clean_text(Some("   ".into())), None);
        assert_eq!(
            clean_text(Some("x".repeat(5000))).map(|s| s.len()),
            Some(MAX_TEXT_CHARS)
        );
    }
}
