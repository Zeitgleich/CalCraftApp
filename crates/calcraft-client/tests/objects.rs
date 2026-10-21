#![allow(clippy::unwrap_used)]

mod common;

use calcraft_client::{ClientOptions, CollectionKind, Error, Href, Method, Precondition};
use common::{BASE, MockTransport, block_on, client, client_with};

const CAL: &str = "/dav/calendars/alice/default/";

fn href(path: &str) -> Href {
    Href::parse(path).unwrap()
}

#[test]
fn calcraft_multiget() {
    let mock = MockTransport::new().reply(
        Method::Report,
        CAL,
        207,
        include_str!("fixtures/calcraft/multiget.xml"),
    );
    let client = client(BASE, mock);
    let hrefs = [
        href("/dav/calendars/alice/default/mine.ics"),
        href("/dav/calendars/alice/default/missing.ics"),
        href("/dav/calendars/alice/default/unmentioned.ics"),
        href("/dav/calendars/alice/default/mine.ics"),
    ];
    let result = block_on(client.multiget(&href(CAL), CollectionKind::Calendar, &hrefs)).unwrap();
    assert_eq!(result.objects.len(), 1);
    let object = &result.objects[0];
    assert_eq!(object.href, hrefs[0]);
    assert_eq!(object.etag.as_deref(), Some("\"m-1\""));
    assert!(
        object
            .data
            .starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n")
    );
    assert!(object.data.contains("SUMMARY:Mine & yours\r\n"));
    // The response for Bob's object was not requested and is ignored.
    assert!(!result.objects.iter().any(|o| o.data.contains("Bob secret")));
    assert_eq!(result.missing, [hrefs[1].clone(), hrefs[2].clone()]);
    assert!(result.rejected.is_empty());

    let request = &client.transport().requests()[0];
    assert!(request.body.contains("<c:calendar-multiget "));
    assert!(request.body.contains("<c:calendar-data/>"));
    assert_eq!(
        request.body.matches("<d:href>").count(),
        3,
        "duplicates collapsed"
    );
    assert_eq!(request.header("depth"), None);
}

#[test]
fn multiget_batches() {
    let empty = r#"<d:multistatus xmlns:d="DAV:"/>"#;
    let mock = MockTransport::new()
        .reply(Method::Report, CAL, 207, empty)
        .reply(Method::Report, CAL, 207, empty)
        .reply(Method::Report, CAL, 207, empty);
    let mut options = ClientOptions::default();
    options.multiget_batch_size = 2;
    let client = client_with(BASE, mock, options);
    let hrefs: Vec<Href> = (0..5).map(|i| href(&format!("{CAL}e{i}.ics"))).collect();
    let result = block_on(client.multiget(&href(CAL), CollectionKind::Calendar, &hrefs)).unwrap();
    assert_eq!(result.missing.len(), 5);
    let counts: Vec<usize> = client
        .transport()
        .requests()
        .iter()
        .map(|r| r.body.matches("<d:href>").count())
        .collect();
    assert_eq!(counts, [2, 2, 1]);
}

#[test]
fn nextcloud_multiget_matches_encoded_hrefs() {
    let collection = href("/remote.php/dav/calendars/alice@example.com/personal/");
    let mock = MockTransport::new().reply(
        Method::Report,
        collection.as_str(),
        207,
        include_str!("fixtures/sabre/multiget.xml"),
    );
    let client = client("https://cloud.example.com/", mock);
    let notes = collection.child("Meeting Notes.ics", false).unwrap();
    let old = collection.child("old.ics", false).unwrap();
    let result = block_on(client.multiget(
        &collection,
        CollectionKind::Calendar,
        &[notes.clone(), old.clone()],
    ))
    .unwrap();
    assert_eq!(result.objects.len(), 1);
    assert_eq!(result.objects[0].href, notes);
    assert!(result.objects[0].data.contains("SUMMARY:Notes <draft>\r\n"));
    assert_eq!(result.missing, [old]);
    let body = &client.transport().requests()[0].body;
    assert!(body.contains("<d:href>/remote.php/dav/calendars/alice@example.com/personal/Meeting%20Notes.ics</d:href>"), "{body}");
}

#[test]
fn radicale_addressbook_multiget() {
    let book = href("/alice/contacts/");
    let mock = MockTransport::new().reply(
        Method::Report,
        "/alice/contacts/",
        207,
        include_str!("fixtures/radicale/addressbook_multiget.xml"),
    );
    let client = client("https://radicale.example.com/", mock);
    let card = href("/alice/contacts/b2c9.vcf");
    let result = block_on(client.multiget(
        &book,
        CollectionKind::AddressBook,
        std::slice::from_ref(&card),
    ))
    .unwrap();
    assert_eq!(result.objects[0].href, card);
    assert!(result.objects[0].data.contains("FN:Carol Contact"));
    let body = &client.transport().requests()[0].body;
    assert!(body.contains("<card:addressbook-multiget "));
    assert!(body.contains("<card:address-data/>"));
}

#[test]
fn oversized_object_in_multiget_is_rejected() {
    let big = "X".repeat(2000);
    let body = format!(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>{CAL}big.ics</d:href><d:propstat><d:prop><d:getetag>"b"</d:getetag><c:calendar-data>{big}</c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>{CAL}empty.ics</d:href><d:propstat><d:prop><d:getetag>"e"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
    );
    let mock = MockTransport::new().reply(Method::Report, CAL, 207, body);
    let mut options = ClientOptions::default();
    options.max_object_bytes = 1000;
    let client = client_with(BASE, mock, options);
    let hrefs = [
        href(&format!("{CAL}big.ics")),
        href(&format!("{CAL}empty.ics")),
    ];
    let result = block_on(client.multiget(&href(CAL), CollectionKind::Calendar, &hrefs)).unwrap();
    assert!(result.objects.is_empty());
    assert_eq!(result.rejected, hrefs);
}

#[test]
fn put_create_and_update() {
    let uri = format!("{CAL}meeting.ics");
    let mock = MockTransport::new()
        .reply_with(Method::Put, &uri, 201, &[("ETag", "\"v1\"")], Vec::new())
        .reply(Method::Put, &uri, 412, Vec::new())
        .reply_with(Method::Put, &uri, 204, &[("ETag", "\"v2\"")], Vec::new())
        .reply(Method::Put, &uri, 204, Vec::new());
    let client = client(BASE, mock);
    let target = href(&uri);
    let data = "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n";

    let etag = block_on(client.put_object(
        &target,
        CollectionKind::Calendar,
        data,
        &Precondition::Create,
    ))
    .unwrap();
    assert_eq!(etag.as_deref(), Some("\"v1\""));
    let conflict = block_on(client.put_object(
        &target,
        CollectionKind::Calendar,
        data,
        &Precondition::Update("\"stale\"".into()),
    ));
    assert!(matches!(conflict, Err(Error::PreconditionFailed(_))));
    let etag = block_on(client.put_object(
        &target,
        CollectionKind::Calendar,
        data,
        &Precondition::Update("v1".into()),
    ))
    .unwrap();
    assert_eq!(etag.as_deref(), Some("\"v2\""));
    // Server rewrote the data and sent no ETag.
    let etag = block_on(client.put_object(
        &target,
        CollectionKind::Calendar,
        data,
        &Precondition::Update("\"v2\"".into()),
    ))
    .unwrap();
    assert_eq!(etag, None);

    let requests = client.transport().requests();
    assert_eq!(requests[0].header("if-none-match"), Some("*"));
    assert_eq!(requests[0].header("if-match"), None);
    assert_eq!(
        requests[0].header("content-type"),
        Some("text/calendar; charset=utf-8")
    );
    assert_eq!(requests[0].body, data);
    assert_eq!(requests[1].header("if-match"), Some("\"stale\""));
    assert_eq!(
        requests[2].header("if-match"),
        Some("\"v1\""),
        "unquoted etag is quoted"
    );
}

#[test]
fn put_vcard_and_limits() {
    let mock = MockTransport::new().reply(
        Method::Put,
        "/dav/addressbooks/alice/default/c.vcf",
        201,
        Vec::new(),
    );
    let mut options = ClientOptions::default();
    options.max_object_bytes = 64;
    let client = client_with(BASE, mock, options);
    let target = href("/dav/addressbooks/alice/default/c.vcf");
    block_on(client.put_object(
        &target,
        CollectionKind::AddressBook,
        "BEGIN:VCARD\r\nEND:VCARD\r\n",
        &Precondition::Create,
    ))
    .unwrap();
    assert_eq!(
        client.transport().requests()[0].header("content-type"),
        Some("text/vcard; charset=utf-8")
    );
    let huge = "x".repeat(65);
    assert!(matches!(
        block_on(client.put_object(
            &target,
            CollectionKind::AddressBook,
            &huge,
            &Precondition::Create
        )),
        Err(Error::InvalidObject(_))
    ));
    assert!(matches!(
        block_on(client.put_object(
            &href("/dav/x/"),
            CollectionKind::AddressBook,
            "x",
            &Precondition::Create
        )),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn header_injection_via_etag_is_refused() {
    let client = client(BASE, MockTransport::new());
    let target = href(&format!("{CAL}x.ics"));
    for evil in ["\"a\"\r\nX-Evil: 1", "\"a\"\nAuthorization: x", "a\"b"] {
        let result = block_on(client.put_object(
            &target,
            CollectionKind::Calendar,
            "x",
            &Precondition::Update(evil.into()),
        ));
        assert!(matches!(result, Err(Error::InvalidInput(_))), "{evil}");
        assert!(matches!(
            block_on(client.delete_object(&target, evil)),
            Err(Error::InvalidInput(_))
        ));
    }
    assert!(client.transport().requests().is_empty());
}

#[test]
fn delete_with_if_match() {
    let uri = format!("{CAL}gone.ics");
    let mock = MockTransport::new()
        .reply(Method::Delete, &uri, 412, Vec::new())
        .reply(Method::Delete, &uri, 204, Vec::new())
        .reply(Method::Delete, &uri, 404, Vec::new());
    let client = client(BASE, mock);
    let target = href(&uri);
    assert!(matches!(
        block_on(client.delete_object(&target, "\"wrong\"")),
        Err(Error::PreconditionFailed(_))
    ));
    block_on(client.delete_object(&target, "\"right\"")).unwrap();
    assert!(matches!(
        block_on(client.delete_object(&target, "\"right\"")),
        Err(Error::NotFound(_))
    ));
    assert_eq!(
        client.transport().requests()[1].header("if-match"),
        Some("\"right\"")
    );
}

#[test]
fn get_object_checks_utf8() {
    let uri = format!("{CAL}a.ics");
    let mock = MockTransport::new()
        .reply_with(
            Method::Get,
            &uri,
            200,
            &[("ETag", "\"g1\""), ("Content-Type", "text/calendar")],
            "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n",
        )
        .reply_with(
            Method::Get,
            &uri,
            200,
            &[("Content-Type", "text/calendar")],
            vec![0xff, 0xfe, b'x'],
        );
    let client = client(BASE, mock);
    let object = block_on(client.get_object(&href(&uri))).unwrap();
    assert_eq!(object.etag.as_deref(), Some("\"g1\""));
    assert!(matches!(
        block_on(client.get_object(&href(&uri))),
        Err(Error::InvalidObject(_))
    ));
}
