//! Hostile and broken server responses.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use calcraft_client::{
    ClientOptions, CollectionKind, Error, Href, Method, SyncState, TransportErrorKind,
};
use common::{BASE, MockTransport, PASSWORD, block_on, client, client_with};

const HOME: &str = "/dav/calendars/alice/";
const CAL: &str = "/dav/calendars/alice/default/";

fn href(path: &str) -> Href {
    Href::parse(path).unwrap()
}

fn list(body: impl Into<Vec<u8>>) -> Result<Vec<calcraft_client::Collection>, Error> {
    let mock = MockTransport::new().reply(Method::Propfind, HOME, 207, body);
    let client = client(BASE, mock);
    block_on(client.list_collections(&href(HOME)))
}

#[test]
fn huge_bodies_are_refused() {
    let mut options = ClientOptions::default();
    options.max_xml_bytes = 4096;
    let huge = format!(
        r#"<d:multistatus xmlns:d="DAV:">{}</d:multistatus>"#,
        "<d:response/>".repeat(1000)
    );
    // A well-behaved transport stops reading.
    let mock = MockTransport::new().reply(Method::Propfind, HOME, 207, huge.clone());
    let client = client_with(BASE, mock, options.clone());
    match block_on(client.list_collections(&href(HOME))) {
        Err(Error::Transport(e)) => assert_eq!(e.kind, TransportErrorKind::BodyTooLarge),
        other => panic!("{other:?}"),
    }
    // A broken transport that ignores the limit is caught by the client.
    let mut mock = MockTransport::new().reply(Method::Propfind, HOME, 207, huge);
    mock.ignore_limit = true;
    let client = client_with(BASE, mock, options);
    assert!(matches!(
        block_on(client.list_collections(&href(HOME))),
        Err(Error::BodyTooLarge(4096))
    ));
}

#[test]
fn deep_nesting_is_refused() {
    let deep = format!(
        r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/x/</d:href><d:propstat><d:prop>{}{}</d:prop></d:propstat></d:response></d:multistatus>"#,
        "<d:a>".repeat(10_000),
        "</d:a>".repeat(10_000)
    );
    assert!(matches!(list(deep), Err(Error::Xml(_))));
}

#[test]
fn element_count_is_limited() {
    let mut options = ClientOptions::default();
    options.max_xml_elements = 1000;
    let many = format!(
        r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/x/</d:href><d:propstat><d:prop><d:displayname>{}</d:displayname></d:prop></d:propstat></d:response></d:multistatus>"#,
        "<d:x/>".repeat(5000)
    );
    let mock = MockTransport::new().reply(Method::Propfind, HOME, 207, many);
    let client = client_with(BASE, mock, options);
    assert!(matches!(
        block_on(client.list_collections(&href(HOME))),
        Err(Error::Xml(_))
    ));
}

#[test]
fn doctype_and_entities_are_refused() {
    let billion_laughs = r#"<?xml version="1.0"?>
<!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">]>
<d:multistatus xmlns:d="DAV:"><d:response><d:href>&lol2;</d:href></d:response></d:multistatus>"#;
    let xxe = r#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY xxe SYSTEM "file:///etc/passwd">]><d:multistatus xmlns:d="DAV:"><d:response><d:href>/&xxe;</d:href></d:response></d:multistatus>"#;
    let undeclared = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/&nbsp;</d:href></d:response></d:multistatus>"#;
    for body in [billion_laughs, xxe, undeclared] {
        assert!(matches!(list(body), Err(Error::Xml(_))), "{body}");
    }
}

#[test]
fn malformed_xml_is_refused() {
    let cases: [&[u8]; 8] = [
        b"<d:multistatus xmlns:d=\"DAV:\"><d:response>",
        b"<d:multistatus xmlns:d=\"DAV:\"></d:response></d:multistatus>",
        b"<x:multistatus xmlns:d=\"DAV:\"/>",
        b"not xml at all",
        b"",
        b"<d:multistatus xmlns:d=\"DAV:\"/><d:multistatus xmlns:d=\"DAV:\"/>",
        b"<d:multistatus xmlns:d=\"DAV:\">\xff\xfe</d:multistatus>",
        b"<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/a/</d:href><d:status>garbage</d:status></d:response></d:multistatus>",
    ];
    for body in cases {
        let result = list(body.to_vec());
        assert!(
            matches!(result, Err(Error::Xml(_) | Error::InvalidResponse(_))),
            "{:?}: {result:?}",
            String::from_utf8_lossy(body)
        );
    }
    // Well-formed, but not a multistatus.
    assert!(matches!(
        list(r#"<d:error xmlns:d="DAV:"/>"#),
        Err(Error::InvalidResponse(_))
    ));
    // Response without href.
    assert!(matches!(
        list(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:status>HTTP/1.1 200 OK</d:status></d:response></d:multistatus>"#
        ),
        Err(Error::InvalidResponse(_))
    ));
}

#[test]
fn foreign_hrefs_never_become_requests() {
    // Every response href points elsewhere; nothing is listed.
    let body = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
        <d:response><d:href>https://evil.example.org/dav/calendars/alice/x/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
        <d:response><d:href>//evil.example.org/y/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
        <d:response><d:href>http://cal.example.com/z/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
        <d:response><d:href>file:///etc/passwd</d:href><d:status>HTTP/1.1 200 OK</d:status></d:response>
    </d:multistatus>"#;
    assert!(list(body).unwrap().is_empty());

    // Path traversal stays on the origin and is normalised.
    let traversal = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/dav/calendars/alice/../../../bob/cal/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    let listed = list(traversal).unwrap();
    assert_eq!(listed[0].href.as_str(), "/bob/cal/");

    // Foreign sync entries are dropped as well.
    let sync = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>https://evil.example.org/a.ics</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:sync-token>t1</d:sync-token></d:multistatus>"#;
    let mock = MockTransport::new().reply(Method::Report, CAL, 207, sync);
    let client = client(BASE, mock);
    let delta = block_on(client.sync(&href(CAL), &SyncState::default(), &BTreeMap::new())).unwrap();
    assert!(delta.changed.is_empty());
}

#[test]
fn multiget_ignores_unrequested_and_foreign_hrefs() {
    let body = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
      <d:response><d:href>https://evil.example.org/dav/calendars/alice/default/a.ics</d:href><d:propstat><d:prop><d:getetag>"evil"</d:getetag><c:calendar-data>EVIL</c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
      <d:response><d:href>/dav/calendars/alice/default/injected.ics</d:href><d:propstat><d:prop><d:getetag>"i"</d:getetag><c:calendar-data>INJECTED</c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
      <d:response><d:href>/dav/calendars/alice/default/a.ics</d:href><d:propstat><d:prop><d:getetag>"good"</d:getetag><c:calendar-data>GOOD</c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
      <d:response><d:href>/dav/calendars/alice/default/a.ics</d:href><d:propstat><d:prop><d:getetag>"dup"</d:getetag><c:calendar-data>DUPLICATE</c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
    </d:multistatus>"#;
    let mock = MockTransport::new().reply(Method::Report, CAL, 207, body);
    let client = client(BASE, mock);
    let wanted = href("/dav/calendars/alice/default/a.ics");
    let result = block_on(client.multiget(
        &href(CAL),
        CollectionKind::Calendar,
        std::slice::from_ref(&wanted),
    ))
    .unwrap();
    assert_eq!(result.objects.len(), 1);
    assert_eq!(result.objects[0].data, "GOOD");
}

#[test]
fn html_error_pages_do_not_confuse_errors() {
    let mock = MockTransport::new().reply_with(
        Method::Report,
        CAL,
        403,
        &[("Content-Type", "text/html")],
        "<html><body><d:error xmlns:d=\"DAV:\"><d:valid-sync-token/></d:error></body></html>",
    );
    let client = client(BASE, mock);
    match block_on(client.sync_collection(&href(CAL), Some("t"))) {
        Err(Error::Status {
            status: 403,
            condition: None,
            ..
        }) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn secrets_are_not_printed() {
    let mock = MockTransport::new().reply(Method::Propfind, HOME, 401, Vec::new());
    let client = client(BASE, mock);
    let error = block_on(client.list_collections(&href(HOME))).unwrap_err();
    let request = &client.transport().requests()[0];
    let printed = format!("{client:?} {error} {error:?}");
    let encoded = request.authorization.clone().unwrap();
    assert!(!printed.contains(PASSWORD), "{printed}");
    assert!(!printed.contains(&encoded["Basic ".len()..]), "{printed}");
    assert!(matches!(error, Error::Unauthorized(_)));
}

#[test]
fn utf8_bom_and_whitespace_are_tolerated() {
    let body = "\u{FEFF}<?xml version=\"1.0\"?>\n<d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">\n <d:response>\n  <d:href>\n   /dav/calendars/alice/x/\n  </d:href>\n  <d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>\n </d:response>\n</d:multistatus>\n";
    let listed = list(body).unwrap();
    assert_eq!(listed[0].href.as_str(), "/dav/calendars/alice/x/");
}

#[test]
fn many_hrefs_sharing_a_propstat_are_rejected() {
    // ~5,000 hrefs times a 5,000-element propstat would be ~25M copies if
    // the propstat were duplicated per href; RFC 4918 forbids the form.
    let hrefs: String = (0..5000)
        .map(|i| format!("<d:href>/x/{i}/</d:href>"))
        .collect();
    let props = "<d:x/>".repeat(5000);
    let body = format!(
        r#"<d:multistatus xmlns:d="DAV:"><d:response>{hrefs}<d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
    );
    let started = std::time::Instant::now();
    assert!(matches!(list(body), Err(Error::InvalidResponse(_))));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}
