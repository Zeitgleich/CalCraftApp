#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use calcraft_client::{ClientOptions, Error, Href, Method, SyncMethod, SyncState};
use common::{BASE, MockTransport, block_on, client, client_with};

const CAL: &str = "/dav/calendars/alice/default/";

fn href(path: &str) -> Href {
    Href::parse(path).unwrap()
}

fn known(entries: &[(&str, &str)]) -> BTreeMap<Href, String> {
    entries
        .iter()
        .map(|(h, e)| (href(h), (*e).to_owned()))
        .collect()
}

fn error_403(body: &str) -> MockTransport {
    MockTransport::new().reply(Method::Report, CAL, 403, body)
}

#[test]
fn initial_sync_follows_truncation() {
    let mock = MockTransport::new()
        .reply(
            Method::Report,
            CAL,
            207,
            include_str!("fixtures/calcraft/sync_page1.xml"),
        )
        .reply(
            Method::Report,
            CAL,
            207,
            include_str!("fixtures/calcraft/sync_page2.xml"),
        );
    let client = client(BASE, mock);
    // A stale local object that no longer exists on the server.
    let local = known(&[("/dav/calendars/alice/default/stale.ics", "\"s\"")]);
    let delta = block_on(client.sync(&href(CAL), &SyncState::default(), &local)).unwrap();
    let changed: Vec<_> = delta
        .changed
        .iter()
        .map(|(h, _)| h.last_segment())
        .collect();
    assert_eq!(changed, ["e0.ics", "e1.ics", "e2.ics"]);
    assert_eq!(delta.changed[2].1.as_deref(), Some("\"e2-v1\""));
    assert_eq!(
        delta.deleted,
        [href("/dav/calendars/alice/default/stale.ics")]
    );
    assert!(delta.full_resync);
    assert_eq!(delta.method, SyncMethod::SyncCollection);
    assert_eq!(
        delta.state.sync_token.as_deref(),
        Some("urn:calcraft:sync:3f2a:3")
    );

    let requests = client.transport().requests();
    assert!(
        requests[0].body.contains("<d:sync-token/>"),
        "{}",
        requests[0].body
    );
    assert!(requests[0].body.contains("<d:sync-level>1</d:sync-level>"));
    assert!(
        requests[1]
            .body
            .contains("<d:sync-token>urn:calcraft:sync:3f2a:2</d:sync-token>")
    );
    assert_eq!(requests[0].header("depth"), Some("0"));
}

#[test]
fn incremental_sync_reports_changes_and_deletions() {
    let mock = MockTransport::new().reply(
        Method::Report,
        CAL,
        207,
        include_str!("fixtures/calcraft/sync_delta.xml"),
    );
    let client = client(BASE, mock);
    let state = SyncState {
        sync_token: Some("urn:calcraft:sync:3f2a:3".into()),
        ..SyncState::default()
    };
    // e1 is reported but its etag equals ours (e.g. our own PUT): skipped.
    let local = known(&[
        ("/dav/calendars/alice/default/e1.ics", "\"e1-v1\""),
        ("/dav/calendars/alice/default/gone.ics", "\"g\""),
        ("/dav/calendars/alice/default/kept.ics", "\"k\""),
    ]);
    let delta = block_on(client.sync(&href(CAL), &state, &local)).unwrap();
    assert_eq!(
        delta.changed,
        [(
            href("/dav/calendars/alice/default/new.ics"),
            Some("\"new-v1\"".into())
        )]
    );
    assert_eq!(
        delta.deleted,
        [href("/dav/calendars/alice/default/gone.ics")]
    );
    assert!(!delta.full_resync, "kept.ics must not be deleted");
    assert_eq!(
        delta.state.sync_token.as_deref(),
        Some("urn:calcraft:sync:3f2a:9")
    );
    assert!(
        client.transport().requests()[0]
            .body
            .contains("<d:sync-token>urn:calcraft:sync:3f2a:3</d:sync-token>")
    );
}

#[test]
fn invalid_token_triggers_full_resync() {
    for error_body in [
        include_str!("fixtures/calcraft/invalid_sync_token.xml"),
        include_str!("fixtures/sabre/invalid_sync_token.xml"),
    ] {
        let mock = error_403(error_body).reply(
            Method::Report,
            CAL,
            207,
            include_str!("fixtures/calcraft/sync_page2.xml"),
        );
        let client = client(BASE, mock);
        let state = SyncState {
            sync_token: Some("urn:calcraft:sync:old:1".into()),
            ..SyncState::default()
        };
        let local = known(&[
            ("/dav/calendars/alice/default/e2.ics", "\"e2-v1\""),
            ("/dav/calendars/alice/default/e9.ics", "\"x\""),
        ]);
        let delta = block_on(client.sync(&href(CAL), &state, &local)).unwrap();
        assert!(delta.full_resync);
        assert!(delta.changed.is_empty(), "e2 unchanged");
        assert_eq!(delta.deleted, [href("/dav/calendars/alice/default/e9.ics")]);
        let requests = client.transport().requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].body.contains("<d:sync-token/>"));
    }
}

#[test]
fn low_level_sync_reports_invalid_token() {
    let client = client(
        BASE,
        error_403(include_str!("fixtures/calcraft/invalid_sync_token.xml")),
    );
    assert!(matches!(
        block_on(client.sync_collection(&href(CAL), Some("t"))),
        Err(Error::InvalidSyncToken(_))
    ));
}

#[test]
fn truncation_without_progress_is_an_error() {
    let mock = MockTransport::new()
        .reply(
            Method::Report,
            CAL,
            207,
            include_str!("fixtures/calcraft/sync_page1.xml"),
        )
        .reply(
            Method::Report,
            CAL,
            207,
            include_str!("fixtures/calcraft/sync_page1.xml"),
        );
    let client = client(BASE, mock);
    assert!(matches!(
        block_on(client.sync_collection(&href(CAL), None)),
        Err(Error::InvalidResponse(_))
    ));
}

#[test]
fn page_limit_is_enforced() {
    let page = |n: usize| {
        format!(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>{CAL}</d:href><d:status>HTTP/1.1 507 Insufficient Storage</d:status></d:response><d:sync-token>t{n}</d:sync-token></d:multistatus>"#
        )
    };
    let mut mock = MockTransport::new();
    for n in 0..3 {
        mock = mock.reply(Method::Report, CAL, 207, page(n));
    }
    let mut options = ClientOptions::default();
    options.max_sync_pages = 3;
    let client = client_with(BASE, mock, options);
    assert!(matches!(
        block_on(client.sync_collection(&href(CAL), None)),
        Err(Error::InvalidResponse(_))
    ));
}

#[test]
fn missing_sync_token_is_an_error() {
    let mock = MockTransport::new().reply(
        Method::Report,
        CAL,
        207,
        r#"<d:multistatus xmlns:d="DAV:"/>"#,
    );
    let client = client(BASE, mock);
    assert!(matches!(
        block_on(client.sync_collection(&href(CAL), None)),
        Err(Error::InvalidResponse(_))
    ));
}

#[test]
fn nextcloud_sync_with_encoded_hrefs() {
    let collection = href("/remote.php/dav/calendars/alice@example.com/personal/");
    let mock = MockTransport::new().reply(
        Method::Report,
        collection.as_str(),
        207,
        include_str!("fixtures/sabre/sync.xml"),
    );
    let client = client("https://cloud.example.com/", mock);
    let state = SyncState {
        sync_token: Some("http://sabre.io/ns/sync/42".into()),
        ..SyncState::default()
    };
    let delta = block_on(client.sync(&collection, &state, &BTreeMap::new())).unwrap();
    // The entry pointing to another host is dropped.
    assert_eq!(delta.changed.len(), 1);
    assert_eq!(
        delta.changed[0].0,
        collection.child("Meeting Notes.ics", false).unwrap()
    );
    assert_eq!(delta.deleted, [collection.child("old.ics", false).unwrap()]);
    assert_eq!(
        delta.state.sync_token.as_deref(),
        Some("http://sabre.io/ns/sync/43")
    );
}

#[test]
fn radicale_sync() {
    let book = href("/alice/contacts/");
    let mock = MockTransport::new().reply(
        Method::Report,
        "/alice/contacts/",
        207,
        include_str!("fixtures/radicale/sync.xml"),
    );
    let client = client("https://radicale.example.com/", mock);
    let state = SyncState {
        sync_token: Some("http://radicale.org/ns/sync/ffee".into()),
        ..SyncState::default()
    };
    let delta = block_on(client.sync(&book, &state, &BTreeMap::new())).unwrap();
    assert_eq!(
        delta.changed,
        [(href("/alice/contacts/b2c9.vcf"), Some("\"8c1f\"".into()))]
    );
    assert_eq!(
        delta.state.sync_token.as_deref(),
        Some("http://radicale.org/ns/sync/ffef")
    );
}

const CTAG: &str = r#"<d:multistatus xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/"><d:response><d:href>/dav/calendars/alice/default/</d:href><d:propstat><d:prop><cs:getctag>ctag-2</cs:getctag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
const ETAGS: &str = r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/dav/calendars/alice/default/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype><D:getetag/></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response><D:response><D:href>/dav/calendars/alice/default/a.ics</D:href><D:propstat><D:prop><D:resourcetype/><D:getetag>"a2"</D:getetag></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response><D:response><D:href>/dav/calendars/alice/default/b.ics</D:href><D:propstat><D:prop><D:resourcetype/><D:getetag>"b1"</D:getetag></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response><D:response><D:href>/dav/calendars/alice/default/sub/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#;

#[test]
fn fallback_to_ctag_and_etags() {
    for (status, body) in [
        (
            403,
            r#"<d:error xmlns:d="DAV:"><d:supported-report/></d:error>"#,
        ),
        (415, include_str!("fixtures/sabre/report_not_supported.xml")),
        (501, ""),
        (400, ""),
    ] {
        let mock = MockTransport::new()
            .reply(Method::Report, CAL, status, body)
            .reply(Method::Propfind, CAL, 207, CTAG)
            .reply(Method::Propfind, CAL, 207, ETAGS);
        let client = client(BASE, mock);
        let state = SyncState {
            ctag: Some("ctag-1".into()),
            ..SyncState::default()
        };
        let local = known(&[
            ("/dav/calendars/alice/default/a.ics", "\"a1\""),
            ("/dav/calendars/alice/default/b.ics", "\"b1\""),
            ("/dav/calendars/alice/default/c.ics", "\"c1\""),
        ]);
        let delta = block_on(client.sync(&href(CAL), &state, &local)).unwrap();
        assert_eq!(delta.method, SyncMethod::Ctag, "{status}");
        assert_eq!(
            delta.changed,
            [(
                href("/dav/calendars/alice/default/a.ics"),
                Some("\"a2\"".into())
            )]
        );
        assert_eq!(delta.deleted, [href("/dav/calendars/alice/default/c.ics")]);
        assert!(delta.full_resync);
        assert!(delta.state.use_ctag);
        assert_eq!(delta.state.ctag.as_deref(), Some("ctag-2"));
        let requests = client.transport().requests();
        assert_eq!(requests[1].header("depth"), Some("0"));
        assert_eq!(requests[2].header("depth"), Some("1"));
    }
}

#[test]
fn unchanged_ctag_skips_listing() {
    let mock = MockTransport::new().reply(Method::Propfind, CAL, 207, CTAG);
    let client = client(BASE, mock);
    let state = SyncState {
        ctag: Some("ctag-2".into()),
        use_ctag: true,
        ..SyncState::default()
    };
    let delta = block_on(client.sync(
        &href(CAL),
        &state,
        &known(&[("/dav/calendars/alice/default/a.ics", "\"a\"")]),
    ))
    .unwrap();
    assert!(delta.changed.is_empty() && delta.deleted.is_empty());
    assert!(!delta.full_resync);
    assert_eq!(client.transport().requests().len(), 1);
}

#[test]
fn other_errors_are_not_masked() {
    let mock = MockTransport::new().reply(Method::Report, CAL, 404, Vec::new());
    let client = client(BASE, mock);
    assert!(matches!(
        block_on(client.sync(&href(CAL), &SyncState::default(), &BTreeMap::new())),
        Err(Error::NotFound(_))
    ));
    let mock = MockTransport::new().reply(Method::Report, CAL, 401, Vec::new());
    let client = common::client(BASE, mock);
    assert!(matches!(
        block_on(client.sync(&href(CAL), &SyncState::default(), &BTreeMap::new())),
        Err(Error::Unauthorized(_))
    ));
}
