#![allow(clippy::unwrap_used)]

mod common;

use calcraft_client::{CollectionKind, CollectionProps, Error, Href, Method};
use common::{BASE, MockTransport, block_on, client};

fn href(path: &str) -> Href {
    Href::parse(path).unwrap()
}

#[test]
fn calcraft_calendars() {
    let mock = MockTransport::new().reply(
        Method::Propfind,
        "/dav/calendars/alice/",
        207,
        include_str!("fixtures/calcraft/calendars.xml"),
    );
    let client = client(BASE, mock);
    let calendars = block_on(client.list_collections(&href("/dav/calendars/alice/"))).unwrap();
    assert_eq!(calendars.len(), 2, "home itself is skipped");

    let personal = &calendars[0];
    assert_eq!(personal.href, href("/dav/calendars/alice/default/"));
    assert_eq!(personal.kind, CollectionKind::Calendar);
    assert_eq!(personal.display_name.as_deref(), Some("Personal"));
    assert_eq!(personal.description, None, "404 propstat is ignored");
    assert_eq!(personal.color.as_deref(), Some("#336699"));
    assert_eq!(personal.supported_components, ["VEVENT", "VTODO"]);
    assert_eq!(personal.ctag.as_deref(), Some("7"));
    assert_eq!(
        personal.sync_token.as_deref(),
        Some("urn:calcraft:sync:3f2a:7")
    );
    assert!(personal.supports_sync);

    let work = &calendars[1];
    assert_eq!(work.display_name.as_deref(), Some("Work & Travel"));
    assert_eq!(work.description.as_deref(), Some("Office <Berlin>"));
    assert_eq!(work.color.as_deref(), Some("#FF8800CC"));

    let request = &client.transport().requests()[0];
    assert_eq!(request.header("depth"), Some("1"));
    for prop in [
        "resourcetype",
        "getctag",
        "sync-token",
        "calendar-color",
        "supported-calendar-component-set",
    ] {
        assert!(request.body.contains(prop), "{prop}");
    }
}

#[test]
fn calcraft_addressbooks() {
    let mock = MockTransport::new().reply(
        Method::Propfind,
        "/dav/addressbooks/alice/",
        207,
        include_str!("fixtures/calcraft/addressbooks.xml"),
    );
    let client = client(BASE, mock);
    let books = block_on(client.list_collections(&href("/dav/addressbooks/alice/"))).unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].kind, CollectionKind::AddressBook);
    assert_eq!(books[0].description.as_deref(), Some("Friends"));
    assert_eq!(books[0].color, None);
}

#[test]
fn nextcloud_calendars() {
    let home = href("/remote.php/dav/calendars/alice%40example.com/");
    let mock = MockTransport::new().reply(
        Method::Propfind,
        home.as_str(),
        207,
        include_str!("fixtures/sabre/calendars.xml"),
    );
    let client = client("https://cloud.example.com/", mock);
    let calendars = block_on(client.list_collections(&home)).unwrap();
    let names: Vec<_> = calendars
        .iter()
        .map(|c| c.display_name.as_deref().unwrap())
        .collect();
    // Inbox/outbox, subscriptions and the foreign-host entry are skipped.
    assert_eq!(names, ["Persönlich", "Team Planüng"]);
    let personal = &calendars[0];
    assert_eq!(
        personal.href.as_str(),
        "/remote.php/dav/calendars/alice@example.com/personal/"
    );
    assert_eq!(personal.supported_components, ["VEVENT", "VTODO"]);
    assert!(personal.supports_sync);
    assert_eq!(personal.color.as_deref(), Some("#0082C9"));
    let team = &calendars[1];
    assert_eq!(team.href.last_segment(), "Team Planüng");
    assert_eq!(team.color, None, "invalid colour is dropped");
    assert_eq!(
        team.description.as_deref(),
        Some("Shared \u{2014} read only")
    );
    assert!(team.supported_components.is_empty());
}

#[test]
fn radicale_collections() {
    let mock = MockTransport::new().reply(
        Method::Propfind,
        "/alice/",
        207,
        include_str!("fixtures/radicale/collections.xml"),
    );
    let client = client("https://radicale.example.com/", mock);
    let collections = block_on(client.list_collections(&href("/alice/"))).unwrap();
    assert_eq!(collections.len(), 2);
    assert_eq!(collections[0].kind, CollectionKind::Calendar);
    assert_eq!(collections[0].color.as_deref(), Some("#ff0000ff"));
    assert_eq!(
        collections[0].supported_components,
        ["VEVENT", "VJOURNAL", "VTODO"]
    );
    assert!(collections[0].supports_sync, "sync token implies support");
    assert_eq!(collections[1].kind, CollectionKind::AddressBook);
    assert_eq!(collections[1].description.as_deref(), Some("All contacts"));
    assert_eq!(collections[1].ctag.as_deref(), Some("\"ffee\""));
}

#[test]
fn mkcalendar_body() {
    let mock = MockTransport::new()
        .reply(
            Method::Mkcalendar,
            "/dav/calendars/alice/work/",
            201,
            Vec::new(),
        )
        .reply(
            Method::Mkcalendar,
            "/dav/calendars/alice/work/",
            405,
            Vec::new(),
        );
    let client = client(BASE, mock);
    let home = href("/dav/calendars/alice/");
    let target = home.child("work", true).unwrap();
    let props = CollectionProps {
        display_name: Some("Work & <Travel>".into()),
        color: Some("#336699".into()),
        description: Some("Job".into()),
        components: vec!["VEVENT".into()],
    };
    block_on(client.create_calendar(&target, &props)).unwrap();
    let request = &client.transport().requests()[0];
    assert!(request.body.contains("<c:mkcalendar "), "{}", request.body);
    assert!(
        request
            .body
            .contains("<d:displayname>Work &amp; &lt;Travel&gt;</d:displayname>")
    );
    assert!(
        request
            .body
            .contains("<ic:calendar-color>#336699</ic:calendar-color>")
    );
    assert!(
        request
            .body
            .contains("<c:calendar-description>Job</c:calendar-description>")
    );
    assert!(request.body.contains(r#"<c:comp name="VEVENT"/>"#));
    assert!(
        request
            .header("content-type")
            .unwrap()
            .starts_with("application/xml")
    );

    match block_on(client.create_calendar(&target, &props)) {
        Err(Error::Status { status: 405, .. }) => {}
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        block_on(client.create_calendar(&href("/dav/calendars/alice/x.ics"), &props)),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn extended_mkcol_for_addressbook() {
    let mock = MockTransport::new().reply(
        Method::Mkcol,
        "/dav/addressbooks/alice/team/",
        201,
        Vec::new(),
    );
    let client = client(BASE, mock);
    let props = CollectionProps {
        display_name: Some("Team".into()),
        description: Some("Colleagues".into()),
        ..CollectionProps::default()
    };
    block_on(client.create_addressbook(&href("/dav/addressbooks/alice/team/"), &props)).unwrap();
    let body = &client.transport().requests()[0].body;
    assert!(body.contains("<d:mkcol "));
    assert!(body.contains("<d:resourcetype><d:collection/><card:addressbook/></d:resourcetype>"));
    assert!(
        body.contains("<card:addressbook-description>Colleagues</card:addressbook-description>")
    );
    let colored = CollectionProps {
        color: Some("#123456".into()),
        ..CollectionProps::default()
    };
    assert!(matches!(
        block_on(client.create_addressbook(&href("/dav/addressbooks/alice/x/"), &colored)),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn proppatch_success_and_rejection() {
    let ok = r#"<d:multistatus xmlns:d="DAV:" xmlns:ic="http://apple.com/ns/ical/"><d:response><d:href>/dav/calendars/alice/work/</d:href><d:propstat><d:prop><d:displayname/><ic:calendar-color/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    let rejected = r#"<d:multistatus xmlns:d="DAV:" xmlns:ic="http://apple.com/ns/ical/"><d:response><d:href>/dav/calendars/alice/work/</d:href><d:propstat><d:prop><d:displayname/></d:prop><d:status>HTTP/1.1 409 Conflict</d:status></d:propstat><d:propstat><d:prop><ic:calendar-color/></d:prop><d:status>HTTP/1.1 424 Failed Dependency</d:status></d:propstat></d:response></d:multistatus>"#;
    let mock = MockTransport::new()
        .reply(Method::Proppatch, "/dav/calendars/alice/work/", 207, ok)
        .reply(
            Method::Proppatch,
            "/dav/calendars/alice/work/",
            207,
            rejected,
        );
    let client = client(BASE, mock);
    let target = href("/dav/calendars/alice/work/");
    let props = CollectionProps {
        display_name: Some("Job".into()),
        color: Some("#AABBCC".into()),
        ..CollectionProps::default()
    };
    block_on(client.update_collection(&target, CollectionKind::Calendar, &props)).unwrap();
    let body = &client.transport().requests()[0].body;
    assert!(body.contains("<d:propertyupdate "));
    assert!(body.contains("<d:set><d:prop><d:displayname>Job</d:displayname><ic:calendar-color>#AABBCC</ic:calendar-color></d:prop></d:set>"));
    match block_on(client.update_collection(&target, CollectionKind::Calendar, &props)) {
        Err(Error::Status {
            status: 409,
            condition,
            ..
        }) => assert_eq!(condition.as_deref(), Some("displayname")),
        other => panic!("{other:?}"),
    }
    // Nothing to change: no request.
    block_on(client.update_collection(
        &target,
        CollectionKind::Calendar,
        &CollectionProps::default(),
    ))
    .unwrap();
    assert_eq!(client.transport().requests().len(), 2);
}

#[test]
fn delete_collection() {
    let mock = MockTransport::new().reply(
        Method::Delete,
        "/dav/calendars/alice/temp/",
        204,
        Vec::new(),
    );
    let client = client(BASE, mock);
    block_on(client.delete_collection(&href("/dav/calendars/alice/temp/"))).unwrap();
    assert_eq!(client.transport().requests()[0].header("if-match"), None);
}
