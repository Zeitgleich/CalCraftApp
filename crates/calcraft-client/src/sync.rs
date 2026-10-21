//! Collection synchronisation: WebDAV `sync-collection` (RFC 6578) with a
//! fallback to `CS:getctag` plus an etag comparison over `PROPFIND Depth: 1`.

use std::collections::{BTreeMap, HashSet};

use crate::client::{DavClient, Depth};
use crate::error::Error;
use crate::transport::{HttpTransport, Method};
use crate::url::Href;
use crate::xml::{NS_CALSERVER, NS_DAV, XMLNS, escape};

/// Per-collection state to persist between sync runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncState {
    /// Last `DAV:sync-token`.
    pub sync_token: Option<String>,
    /// Last `CS:getctag` (fallback mode).
    pub ctag: Option<String>,
    /// Set once the server refused `sync-collection`; later runs go straight
    /// to the ctag/etag fallback.
    pub use_ctag: bool,
}

/// How a [`SyncDelta`] was computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMethod {
    /// `sync-collection` report.
    SyncCollection,
    /// ctag check plus etag comparison.
    Ctag,
}

/// Changes since the previous [`SyncState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncDelta {
    /// New or changed members with their new etag; fetch them with
    /// [`DavClient::multiget`].
    pub changed: Vec<(Href, Option<String>)>,
    /// Members that no longer exist.
    pub deleted: Vec<Href>,
    /// State to persist once the delta has been applied.
    pub state: SyncState,
    /// The complete member list was compared against `known` (first sync,
    /// rejected token or fallback mode). Local objects not on the server are
    /// in `deleted`.
    pub full_resync: bool,
    /// How the delta was computed.
    pub method: SyncMethod,
}

/// The raw result of [`DavClient::sync_collection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCollectionResult {
    /// Changed members and their etag.
    pub changed: Vec<(Href, Option<String>)>,
    /// Removed members.
    pub deleted: Vec<Href>,
    /// The new sync token.
    pub sync_token: String,
}

impl<T: HttpTransport> DavClient<T> {
    /// Runs `sync-collection` (sync-level 1) from `token` (`None` for an
    /// initial sync). Truncated results (a 507 entry for the collection) are
    /// continued with the returned token until complete, up to
    /// [`ClientOptions::max_sync_pages`](crate::ClientOptions) requests.
    ///
    /// # Errors
    /// [`Error::InvalidSyncToken`] if the server rejected the token, HTTP,
    /// transport and XML errors.
    pub async fn sync_collection(
        &self,
        collection: &Href,
        token: Option<&str>,
    ) -> Result<SyncCollectionResult, Error> {
        let url = self.url_for(collection);
        let mut token = token.map(str::to_owned);
        // href -> Some(etag) for changes, None for deletions; later pages win.
        let mut changes: BTreeMap<Href, Option<Option<String>>> = BTreeMap::new();
        for _ in 0..self.options().max_sync_pages.max(1) {
            let token_xml = match &token {
                Some(token) => format!("<d:sync-token>{}</d:sync-token>", escape(token)),
                None => "<d:sync-token/>".to_owned(),
            };
            let body = format!(
                r#"<?xml version="1.0" encoding="utf-8"?><d:sync-collection {XMLNS}>{token_xml}<d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
            );
            let multistatus = self
                .xml_request(Method::Report, &url, Some(Depth::Zero), body)
                .await?;
            let mut truncated = false;
            for response in &multistatus.responses {
                let status = response.effective_status();
                if &response.href == collection {
                    truncated |= status == 507;
                    continue;
                }
                if response.href.is_collection() {
                    continue;
                }
                match status {
                    404 | 410 => {
                        changes.insert(response.href.clone(), None);
                    }
                    200..=299 => {
                        let etag = response.prop_text(NS_DAV, "getetag");
                        changes.insert(response.href.clone(), Some(etag));
                    }
                    _ => {}
                }
                if changes.len() > self.options().max_sync_entries {
                    return Err(Error::InvalidResponse(
                        "sync-collection result exceeds max_sync_entries".into(),
                    ));
                }
            }
            let new_token = multistatus.sync_token.ok_or_else(|| {
                Error::InvalidResponse("sync-collection response without sync-token".into())
            })?;
            if !truncated {
                let mut result = SyncCollectionResult {
                    changed: Vec::new(),
                    deleted: Vec::new(),
                    sync_token: new_token,
                };
                for (href, change) in changes {
                    match change {
                        Some(etag) => result.changed.push((href, etag)),
                        None => result.deleted.push(href),
                    }
                }
                return Ok(result);
            }
            if token.as_deref() == Some(new_token.as_str()) {
                return Err(Error::InvalidResponse(
                    "truncated sync-collection result without progress".into(),
                ));
            }
            token = Some(new_token);
        }
        Err(Error::InvalidResponse(
            "sync-collection did not complete within max_sync_pages".into(),
        ))
    }

    /// Computes the changes of a collection since `state`.
    ///
    /// `known` maps the hrefs stored locally to their etags; it is used to
    /// detect deletions on full resyncs and to skip objects whose etag did
    /// not change. Uses `sync-collection` when possible, restarts with a full
    /// sync when the token is rejected, and falls back to ctag + etag
    /// comparison for servers without `sync-collection`.
    ///
    /// # Errors
    /// HTTP, transport and XML errors.
    pub async fn sync(
        &self,
        collection: &Href,
        state: &SyncState,
        known: &BTreeMap<Href, String>,
    ) -> Result<SyncDelta, Error> {
        if !state.use_ctag {
            let attempt = match self
                .sync_collection(collection, state.sync_token.as_deref())
                .await
            {
                Ok(result) => Ok((result, state.sync_token.is_none())),
                Err(Error::InvalidSyncToken(_)) if state.sync_token.is_some() => self
                    .sync_collection(collection, None)
                    .await
                    .map(|result| (result, true)),
                Err(error) => Err(error),
            };
            match attempt {
                Ok((result, full)) => {
                    let mut deleted = result.deleted;
                    if full {
                        let present: HashSet<&Href> =
                            result.changed.iter().map(|(href, _)| href).collect();
                        deleted = known
                            .keys()
                            .filter(|href| !present.contains(href))
                            .cloned()
                            .collect();
                    }
                    return Ok(SyncDelta {
                        changed: drop_unchanged(result.changed, known),
                        deleted,
                        state: SyncState {
                            sync_token: Some(result.sync_token),
                            ctag: state.ctag.clone(),
                            use_ctag: false,
                        },
                        full_resync: full,
                        method: SyncMethod::SyncCollection,
                    });
                }
                Err(error) => match fallback(&error) {
                    Some(permanent) => {
                        return self.sync_by_ctag(collection, state, known, permanent).await;
                    }
                    None => return Err(error),
                },
            }
        }
        self.sync_by_ctag(collection, state, known, true).await
    }

    async fn sync_by_ctag(
        &self,
        collection: &Href,
        state: &SyncState,
        known: &BTreeMap<Href, String>,
        permanent: bool,
    ) -> Result<SyncDelta, Error> {
        let url = self.url_for(collection);
        let ctag_query = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:propfind {XMLNS}><d:prop><cs:getctag/></d:prop></d:propfind>"#
        );
        let multistatus = self
            .xml_request(Method::Propfind, &url, Some(Depth::Zero), ctag_query)
            .await?;
        let ctag = multistatus
            .responses
            .iter()
            .find(|r| &r.href == collection)
            .and_then(|r| r.prop_text(NS_CALSERVER, "getctag"));
        // A temporary fallback keeps the old token so the next run can try
        // sync-collection again (already applied changes are filtered by
        // etag).
        let new_state = SyncState {
            sync_token: if permanent {
                None
            } else {
                state.sync_token.clone()
            },
            ctag: ctag.clone(),
            use_ctag: permanent,
        };
        if ctag.is_some() && ctag == state.ctag {
            return Ok(SyncDelta {
                changed: Vec::new(),
                deleted: Vec::new(),
                state: new_state,
                full_resync: false,
                method: SyncMethod::Ctag,
            });
        }
        let etag_query = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:propfind {XMLNS}><d:prop><d:resourcetype/><d:getetag/></d:prop></d:propfind>"#
        );
        let multistatus = self
            .xml_request(Method::Propfind, &url, Some(Depth::One), etag_query)
            .await?;
        let mut members = Vec::new();
        for response in &multistatus.responses {
            let is_collection = response
                .prop(NS_DAV, "resourcetype")
                .is_some_and(|t| t.child(NS_DAV, "collection").is_some());
            if &response.href == collection
                || response.href.is_collection()
                || is_collection
                || !(200..300).contains(&response.effective_status())
            {
                continue;
            }
            members.push((response.href.clone(), response.prop_text(NS_DAV, "getetag")));
        }
        let present: HashSet<&Href> = members.iter().map(|(href, _)| href).collect();
        let deleted = known
            .keys()
            .filter(|href| !present.contains(href))
            .cloned()
            .collect();
        Ok(SyncDelta {
            changed: drop_unchanged(members, known),
            deleted,
            state: new_state,
            full_resync: true,
            method: SyncMethod::Ctag,
        })
    }
}

fn drop_unchanged(
    changed: Vec<(Href, Option<String>)>,
    known: &BTreeMap<Href, String>,
) -> Vec<(Href, Option<String>)> {
    changed
        .into_iter()
        .filter(|(href, etag)| etag.is_none() || known.get(href) != etag.as_ref())
        .collect()
}

/// Whether an error from `sync-collection` should trigger the ctag
/// fallback: `Some(true)` for clear "not supported" signals (HTTP 501 or a
/// `DAV:supported-report` precondition), which make the fallback permanent;
/// `Some(false)` for other client errors, falling back for this run only;
/// `None` for errors that must be reported.
fn fallback(error: &Error) -> Option<bool> {
    match error {
        Error::Status { status: 501, .. } => Some(true),
        Error::Status {
            condition: Some(condition),
            ..
        } if condition == "supported-report" => Some(true),
        Error::Status {
            status: 400 | 403 | 405 | 409 | 415 | 422,
            ..
        } => Some(false),
        _ => None,
    }
}
