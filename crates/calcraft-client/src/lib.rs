//! A security-first CalDAV/CardDAV client library.
//!
//! `calcraft-client` speaks the standard protocols, so it works with
//! [CalCraft](https://github.com/Zeitgleich/CalCraft) as well as other
//! servers (Radicale, Baïkal/sabre/dav, Nextcloud, ...):
//!
//! - service discovery ([`DavClient::discover`]): RFC 6764 well-known URLs,
//!   `current-user-principal`, calendar and address book home sets;
//! - collections ([`DavClient::list_collections`], `MKCALENDAR`, extended
//!   `MKCOL`, `PROPPATCH`);
//! - synchronisation ([`DavClient::sync`]): `sync-collection` (RFC 6578)
//!   including truncated results and invalid tokens, with a ctag/etag
//!   fallback;
//! - objects ([`DavClient::multiget`], [`DavClient::put_object`],
//!   [`DavClient::delete_object`]) with optimistic concurrency via etags.
//!
//! # Security model
//!
//! - A client is bound to one origin. Requests to any other origin are
//!   refused before sending ([`Error::CrossOrigin`]); hrefs in responses
//!   pointing elsewhere are dropped; cross-origin redirects are not followed.
//! - HTTPS is required except for loopback hosts, unless explicitly allowed.
//! - Credentials are zeroized on drop, redacted in `Debug` and never logged
//!   (the library does not log at all).
//! - XML is parsed with hard limits (size, depth, element count); DTDs and
//!   unknown entities are rejected.
//! - Object data is treated as opaque UTF-8 text with a size limit.
//!
//! # Transport
//!
//! All I/O goes through the [`HttpTransport`] trait. The `hyper-transport`
//! feature (default) provides [`HyperTransport`] (hyper + rustls).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod auth;
mod client;
mod collections;
mod discovery;
mod error;
#[cfg(feature = "hyper-transport")]
mod hyper_transport;
mod objects;
mod sync;
mod transport;
mod url;
mod xml;

pub use auth::Credentials;
pub use client::{ClientOptions, DavClient};
pub use collections::{Collection, CollectionKind, CollectionProps};
pub use discovery::Discovery;
pub use error::Error;
#[cfg(feature = "hyper-transport")]
pub use hyper_transport::{HyperTransport, HyperTransportBuilder};
pub use objects::{DavObject, MultigetResult, Precondition};
pub use sync::{SyncCollectionResult, SyncDelta, SyncMethod, SyncState};
pub use transport::{
    HttpTransport, Method, Request, Response, Secret, TransportError, TransportErrorKind,
};
pub use url::{Href, Scheme, Url};
