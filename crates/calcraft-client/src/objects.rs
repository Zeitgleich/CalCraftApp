//! Fetching and writing calendar objects and vCards.
//!
//! Object bodies are opaque text here: they are only checked to be UTF-8 and
//! within [`ClientOptions::max_object_bytes`](crate::ClientOptions).

use std::collections::HashSet;

use crate::client::{DavClient, quoted_etag, status_error, valid_etag};
use crate::collections::CollectionKind;
use crate::error::Error;
use crate::transport::{HttpTransport, Method};
use crate::url::Href;
use crate::xml::{NS_CALDAV, NS_CARDDAV, NS_DAV, XMLNS, escape};

/// A calendar object resource or vCard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DavObject {
    /// Canonical href.
    pub href: Href,
    /// Entity tag, if the server returned one.
    pub etag: Option<String>,
    /// iCalendar or vCard text.
    pub data: String,
}

/// The result of [`DavClient::multiget`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MultigetResult {
    /// Objects returned with data.
    pub objects: Vec<DavObject>,
    /// Requested hrefs the server reported as missing (or did not mention).
    pub missing: Vec<Href>,
    /// Hrefs whose data was missing, too large or otherwise unusable.
    pub rejected: Vec<Href>,
}

/// Precondition for [`DavClient::put_object`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// Create only (`If-None-Match: *`); fails if the resource exists.
    Create,
    /// Update only if unchanged (`If-Match: <etag>`).
    Update(String),
}

impl<T: HttpTransport> DavClient<T> {
    /// Fetches objects with `calendar-multiget` (RFC 4791 section 7.9) or
    /// `addressbook-multiget` (RFC 6352 section 8.7), in batches of
    /// [`ClientOptions::multiget_batch_size`](crate::ClientOptions).
    ///
    /// Only hrefs that were requested are accepted from the response.
    ///
    /// # Errors
    /// HTTP, transport and XML errors.
    pub async fn multiget(
        &self,
        collection: &Href,
        kind: CollectionKind,
        hrefs: &[Href],
    ) -> Result<MultigetResult, Error> {
        let mut unique = Vec::new();
        let mut seen = HashSet::new();
        for href in hrefs {
            if seen.insert(href) {
                unique.push(href.clone());
            }
        }
        let url = self.url_for(collection);
        let (report, data_ns, data_name) = match kind {
            CollectionKind::Calendar => ("c:calendar-multiget", NS_CALDAV, "calendar-data"),
            CollectionKind::AddressBook => {
                ("card:addressbook-multiget", NS_CARDDAV, "address-data")
            }
        };
        let data_element = match kind {
            CollectionKind::Calendar => "<c:calendar-data/>",
            CollectionKind::AddressBook => "<card:address-data/>",
        };
        let mut result = MultigetResult::default();
        for batch in unique.chunks(self.options().multiget_batch_size.max(1)) {
            let mut body = format!(
                r#"<?xml version="1.0" encoding="utf-8"?><{report} {XMLNS}><d:prop><d:getetag/>{data_element}</d:prop>"#
            );
            for href in batch {
                body.push_str(&format!("<d:href>{}</d:href>", escape(href.as_str())));
            }
            body.push_str(&format!("</{report}>"));
            let multistatus = self.xml_request(Method::Report, &url, None, body).await?;
            let requested: HashSet<&Href> = batch.iter().collect();
            let mut answered = HashSet::new();
            for response in &multistatus.responses {
                if !requested.contains(&response.href) || !answered.insert(response.href.clone()) {
                    continue;
                }
                let status = response.effective_status();
                if status == 404 || status == 410 {
                    result.missing.push(response.href.clone());
                    continue;
                }
                let data = response
                    .prop(data_ns, data_name)
                    .map(|d| d.text.clone())
                    .filter(|d| !d.trim().is_empty());
                match data {
                    Some(data)
                        if (200..300).contains(&status)
                            && data.len() <= self.options().max_object_bytes =>
                    {
                        result.objects.push(DavObject {
                            href: response.href.clone(),
                            etag: response.prop_text(NS_DAV, "getetag"),
                            data,
                        });
                    }
                    _ => result.rejected.push(response.href.clone()),
                }
            }
            for href in batch {
                if !answered.contains(href) {
                    result.missing.push(href.clone());
                }
            }
        }
        Ok(result)
    }

    /// Fetches one object with `GET`.
    ///
    /// # Errors
    /// [`Error::NotFound`], [`Error::InvalidObject`] for non-UTF-8 data,
    /// [`Error::BodyTooLarge`], HTTP and transport errors.
    pub async fn get_object(&self, href: &Href) -> Result<DavObject, Error> {
        let url = self.url_for(href);
        let response = self
            .send(
                Method::Get,
                &url,
                Vec::new(),
                Vec::new(),
                self.options().max_object_bytes,
            )
            .await?;
        if response.status != 200 {
            return Err(status_error(&response, &url));
        }
        let etag = response
            .header("etag")
            .map(str::to_owned)
            .filter(|e| valid_etag(e));
        let data = String::from_utf8(response.body)
            .map_err(|_| Error::InvalidObject(format!("{url} is not valid UTF-8")))?;
        Ok(DavObject {
            href: href.clone(),
            etag,
            data,
        })
    }

    /// Creates or updates an object with `PUT` and the given precondition.
    /// Returns the new entity tag if the server sent one (servers that
    /// rewrite the data may omit it; fetch the object again in that case).
    ///
    /// # Errors
    /// [`Error::PreconditionFailed`] on a conflict (HTTP 412),
    /// [`Error::WeakEtag`] for `Update` with a weak tag (nothing is sent),
    /// [`Error::InvalidObject`] if `data` exceeds the size limit, HTTP and
    /// transport errors.
    pub async fn put_object(
        &self,
        href: &Href,
        kind: CollectionKind,
        data: &str,
        precondition: &Precondition,
    ) -> Result<Option<String>, Error> {
        if href.is_collection() {
            return Err(Error::InvalidInput(
                "object href must not end with '/'".into(),
            ));
        }
        if data.len() > self.options().max_object_bytes {
            return Err(Error::InvalidObject(format!(
                "object exceeds {} bytes",
                self.options().max_object_bytes
            )));
        }
        let condition = match precondition {
            Precondition::Create => ("if-none-match", "*".to_owned()),
            Precondition::Update(etag) => ("if-match", quoted_etag(etag)?),
        };
        let url = self.url_for(href);
        let response = self
            .send(
                Method::Put,
                &url,
                vec![("content-type", kind.content_type().to_owned()), condition],
                data.as_bytes().to_vec(),
                self.options().max_xml_bytes,
            )
            .await?;
        match response.status {
            200 | 201 | 204 => Ok(response
                .header("etag")
                .map(str::to_owned)
                .filter(|e| valid_etag(e))),
            _ => Err(status_error(&response, &url)),
        }
    }

    /// Deletes an object only if it is unchanged (`If-Match`).
    ///
    /// # Errors
    /// [`Error::PreconditionFailed`] if it was changed, [`Error::WeakEtag`]
    /// for a weak tag (nothing is sent), [`Error::NotFound`],
    /// HTTP and transport errors.
    pub async fn delete_object(&self, href: &Href, etag: &str) -> Result<(), Error> {
        if href.is_collection() {
            return Err(Error::InvalidInput(
                "object href must not end with '/'".into(),
            ));
        }
        self.delete_resource(href, Some(etag)).await
    }

    pub(crate) async fn delete_resource(
        &self,
        href: &Href,
        etag: Option<&str>,
    ) -> Result<(), Error> {
        let mut headers = Vec::new();
        if let Some(etag) = etag {
            headers.push(("if-match", quoted_etag(etag)?));
        }
        let url = self.url_for(href);
        let response = self
            .send(
                Method::Delete,
                &url,
                headers,
                Vec::new(),
                self.options().max_xml_bytes,
            )
            .await?;
        match response.status {
            200 | 202 | 204 => Ok(()),
            _ => Err(status_error(&response, &url)),
        }
    }
}
