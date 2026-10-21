#![allow(clippy::unwrap_used)]

mod common;

use calcraft_client::{ClientOptions, Credentials, DavClient, Error, Href, Method};
use common::{BASE, MockTransport, assert_same_origin, block_on, client, client_with};

const CC_PRINCIPAL: &str = include_str!("fixtures/calcraft/principal.xml");
const CC_HOMES: &str = include_str!("fixtures/calcraft/homes.xml");
const NC_PRINCIPAL: &str = include_str!("fixtures/sabre/principal.xml");
const NC_HOMES: &str = include_str!("fixtures/sabre/homes.xml");
const RAD_PRINCIPAL: &str = include_str!("fixtures/radicale/principal.xml");
const RAD_HOMES: &str = include_str!("fixtures/radicale/homes.xml");

fn href(path: &str) -> Href {
    Href::parse(path).unwrap()
}

#[test]
fn calcraft_well_known_flow() {
    // CalCraft answers /.well-known/* with 301 -> /dav/ for any method.
    let mock = MockTransport::new()
        .redirect(Method::Propfind, "/.well-known/caldav", 301, "/dav/")
        .reply(Method::Propfind, "/dav/", 207, CC_PRINCIPAL)
        .reply(Method::Propfind, "/dav/principals/alice/", 207, CC_HOMES);
    let client = client(BASE, mock);
    let discovery = block_on(client.discover()).unwrap();
    assert_eq!(
        discovery.context_url.to_string(),
        "https://cal.example.com/dav/"
    );
    assert_eq!(discovery.principal, href("/dav/principals/alice/"));
    assert_eq!(discovery.display_name.as_deref(), Some("Alice Example"));
    assert_eq!(discovery.calendar_homes, [href("/dav/calendars/alice/")]);
    assert_eq!(
        discovery.addressbook_homes,
        [href("/dav/addressbooks/alice/")]
    );

    let mock = client.transport();
    assert_eq!(mock.remaining(), 0);
    assert_same_origin(mock, "https://cal.example.com/");
    for request in mock.requests() {
        assert_eq!(request.header("depth"), Some("0"));
        assert!(request.body.contains("<d:propfind"));
    }
    assert!(mock.requests()[0].body.contains("current-user-principal"));
    assert!(mock.requests()[2].body.contains("calendar-home-set"));
}

#[test]
fn nextcloud_absolute_redirect_and_encoded_principal() {
    let mock = MockTransport::new()
        .redirect(
            Method::Propfind,
            "/.well-known/caldav",
            301,
            "https://cloud.example.com/remote.php/dav/",
        )
        .reply(Method::Propfind, "/remote.php/dav/", 207, NC_PRINCIPAL)
        .reply(
            Method::Propfind,
            "/remote.php/dav/principals/users/alice@example.com/",
            207,
            NC_HOMES,
        );
    let client = client("https://cloud.example.com", mock);
    let discovery = block_on(client.discover()).unwrap();
    assert_eq!(
        discovery.principal,
        href("/remote.php/dav/principals/users/alice@example.com/")
    );
    assert_eq!(
        discovery.calendar_homes,
        [href("/remote.php/dav/calendars/alice%40example.com/")]
    );
    assert_eq!(
        discovery.addressbook_homes[0].as_str(),
        "/remote.php/dav/addressbooks/users/alice@example.com/"
    );
    assert_same_origin(client.transport(), "https://cloud.example.com/");
}

#[test]
fn user_supplied_path_is_tried_first() {
    let mock = MockTransport::new()
        .reply(Method::Propfind, "/remote.php/dav", 207, NC_PRINCIPAL)
        .reply(
            Method::Propfind,
            // Canonical form: `%40` is decoded to `@`.
            "/remote.php/dav/principals/users/alice@example.com/",
            207,
            NC_HOMES,
        );
    let client = client("https://cloud.example.com/remote.php/dav", mock);
    let discovery = block_on(client.discover()).unwrap();
    assert_eq!(discovery.calendar_homes.len(), 1);
    assert_eq!(client.transport().requests().len(), 2);
}

#[test]
fn radicale_default_namespace_and_root_fallback() {
    // Radicale: no well-known handling here (404), DAV on the root.
    let mock = MockTransport::new()
        .reply_with(
            Method::Propfind,
            "/.well-known/caldav",
            404,
            &[("Content-Type", "text/plain")],
            "Not Found",
        )
        .reply(Method::Propfind, "/.well-known/carddav", 405, Vec::new())
        .reply(Method::Propfind, "/", 207, RAD_PRINCIPAL)
        .reply(Method::Propfind, "/alice/", 207, RAD_HOMES);
    let client = client("https://radicale.example.com/", mock);
    let discovery = block_on(client.discover()).unwrap();
    assert_eq!(discovery.principal, href("/alice/"));
    assert_eq!(discovery.calendar_homes, [href("/alice/")]);
    assert_eq!(discovery.addressbook_homes, [href("/alice/")]);
    assert_eq!(discovery.display_name, None);
}

#[test]
fn cross_origin_redirect_is_not_followed() {
    let mock = MockTransport::new().redirect(
        Method::Propfind,
        "/.well-known/caldav",
        301,
        "https://other.example.net/dav/",
    );
    let client = client(BASE, mock);
    match block_on(client.discover()) {
        Err(Error::CrossOriginRedirect(url)) => assert_eq!(url, "https://other.example.net/dav/"),
        other => panic!("{other:?}"),
    }
    assert_eq!(client.transport().requests().len(), 1);
}

#[test]
fn downgrade_redirect_is_not_followed() {
    for location in [
        "http://cal.example.com/dav/",
        "https://cal.example.com:8443/dav/",
    ] {
        let mock =
            MockTransport::new().redirect(Method::Propfind, "/.well-known/caldav", 302, location);
        let client = client(BASE, mock);
        assert!(
            matches!(
                block_on(client.discover()),
                Err(Error::CrossOriginRedirect(_))
            ),
            "{location}"
        );
    }
}

#[test]
fn redirect_loops_and_chains_are_bounded() {
    let mock = MockTransport::new()
        .redirect(Method::Propfind, "/.well-known/caldav", 301, "/a/")
        .redirect(Method::Propfind, "/a/", 307, "/b/")
        .redirect(Method::Propfind, "/b/", 308, "/a/");
    let client = client(BASE, mock);
    assert!(matches!(
        block_on(client.discover()),
        Err(Error::TooManyRedirects(_))
    ));
    assert_eq!(client.transport().requests().len(), 3);

    let mut mock =
        MockTransport::new().redirect(Method::Propfind, "/.well-known/caldav", 301, "/r0/");
    for i in 0..10 {
        mock = mock.redirect(
            Method::Propfind,
            &format!("/r{i}/"),
            301,
            &format!("/r{}/", i + 1),
        );
    }
    let mut options = ClientOptions::default();
    options.max_redirects = 3;
    let client = client_with(BASE, mock, options);
    assert!(matches!(
        block_on(client.discover()),
        Err(Error::TooManyRedirects(_))
    ));
    assert_eq!(client.transport().requests().len(), 4);
}

#[test]
fn redirect_without_location_falls_through() {
    let mock = MockTransport::new()
        .reply(Method::Propfind, "/.well-known/caldav", 301, Vec::new())
        .reply(Method::Propfind, "/.well-known/carddav", 404, Vec::new())
        .reply(Method::Propfind, "/", 207, RAD_PRINCIPAL)
        .reply(Method::Propfind, "/alice/", 207, RAD_HOMES);
    let client = client(BASE, mock);
    assert!(block_on(client.discover()).is_ok());
}

#[test]
fn unauthorized_stops_discovery() {
    let mock = MockTransport::new().reply_with(
        Method::Propfind,
        "/.well-known/caldav",
        401,
        &[("WWW-Authenticate", "Basic realm=\"CalCraft\"")],
        Vec::new(),
    );
    let client = client(BASE, mock);
    assert!(matches!(
        block_on(client.discover()),
        Err(Error::Unauthorized(_))
    ));
}

#[test]
fn nothing_found() {
    let mock = MockTransport::new()
        .reply(Method::Propfind, "/.well-known/caldav", 404, Vec::new())
        .reply(Method::Propfind, "/.well-known/carddav", 404, Vec::new())
        .reply_with(
            Method::Propfind,
            "/",
            200,
            &[("Content-Type", "text/html")],
            "<html>Welcome to nginx</html>",
        );
    let client = client(BASE, mock);
    assert!(matches!(
        block_on(client.discover()),
        Err(Error::DiscoveryFailed(_))
    ));
}

#[test]
fn cross_origin_home_set_is_refused() {
    // iCloud-style: home set on a different host.
    let principal = r#"<multistatus xmlns="DAV:"><response><href>https://cal.example.com/</href><propstat><prop><current-user-principal><href>https://cal.example.com/123456/principal/</href></current-user-principal></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;
    let homes = r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><response><href>/123456/principal/</href><propstat><prop><C:calendar-home-set><href xmlns="DAV:">https://p42-caldav.example.org:443/123456/calendars/</href></C:calendar-home-set></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;
    let mock = MockTransport::new()
        .reply(Method::Propfind, "/.well-known/caldav", 207, principal)
        .reply(Method::Propfind, "/123456/principal/", 207, homes);
    let client = client(BASE, mock);
    match block_on(client.discover()) {
        Err(Error::CrossOrigin(url)) => {
            assert_eq!(url, "https://p42-caldav.example.org/123456/calendars/");
        }
        other => panic!("{other:?}"),
    }
    assert_same_origin(client.transport(), "https://cal.example.com/");
}

#[test]
fn cross_origin_principal_is_refused() {
    let principal = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/</d:href><d:propstat><d:prop><d:current-user-principal><d:href>https://evil.example.org/p/</d:href></d:current-user-principal></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    let mock = MockTransport::new().reply(Method::Propfind, "/.well-known/caldav", 207, principal);
    let client = client(BASE, mock);
    assert!(matches!(
        block_on(client.discover()),
        Err(Error::CrossOrigin(_))
    ));
    assert_eq!(client.transport().requests().len(), 1);
}

#[test]
fn https_is_required_except_for_loopback() {
    let creds = || Credentials::basic("alice", "pw").unwrap();
    assert!(matches!(
        DavClient::new("http://cal.example.com/", creds(), MockTransport::new()),
        Err(Error::InsecureUrl(_))
    ));
    for loopback in [
        "http://localhost:5232/",
        "http://127.0.0.1:5232/",
        "http://[::1]:5232/",
    ] {
        assert!(
            DavClient::new(loopback, creds(), MockTransport::new()).is_ok(),
            "{loopback}"
        );
    }
    let mut options = ClientOptions::default();
    options.allow_insecure_http = true;
    assert!(
        DavClient::with_options(
            "http://cal.example.com/",
            creds(),
            MockTransport::new(),
            options
        )
        .is_ok()
    );
    assert!(matches!(
        DavClient::new(
            "https://alice:pw@cal.example.com/",
            creds(),
            MockTransport::new()
        ),
        Err(Error::InvalidUrl(_))
    ));
}
