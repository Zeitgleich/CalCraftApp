//! End-to-end test against a running CalCraft (or other) server.
//!
//! Skipped unless `CALCRAFT_TEST_URL` is set, for example:
//!
//! ```sh
//! CALCRAFT_TEST_URL=http://127.0.0.1:5232 CALCRAFT_TEST_USER=alice \
//! CALCRAFT_TEST_PASSWORD='...' cargo test -p calcraft-client --test live_calcraft -- --nocapture
//! ```
//!
//! The test creates and deletes its own calendar and address book.
#![cfg(feature = "hyper-transport")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use calcraft_client::{
    CollectionKind, CollectionProps, Credentials, DavClient, Error, HyperTransport, Precondition,
    SyncState,
};

fn event(uid: &str, summary: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//calcraft-client//live test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

#[test]
fn live_server_roundtrip() {
    let Ok(url) = std::env::var("CALCRAFT_TEST_URL") else {
        eprintln!("CALCRAFT_TEST_URL not set; skipping live test");
        return;
    };
    let user = std::env::var("CALCRAFT_TEST_USER").unwrap_or_else(|_| "alice".into());
    let password = std::env::var("CALCRAFT_TEST_PASSWORD").expect("CALCRAFT_TEST_PASSWORD");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let credentials = Credentials::basic(user, password).unwrap();
        let client = DavClient::new(&url, credentials, HyperTransport::new().unwrap()).unwrap();
        roundtrip(&client).await;
    });
}

async fn roundtrip(client: &DavClient<HyperTransport>) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();

    // Discovery.
    let discovery = client.discover().await.unwrap();
    eprintln!("discovered {discovery:?}");
    let calendar_home = discovery
        .calendar_homes
        .first()
        .expect("calendar home")
        .clone();
    let book_home = discovery
        .addressbook_homes
        .first()
        .expect("address book home")
        .clone();

    // Create a calendar and see it listed.
    let calendar = calendar_home.child(&format!("live-{stamp}"), true).unwrap();
    let props = CollectionProps {
        display_name: Some("Live & test".into()),
        color: Some("#336699".into()),
        description: Some("calcraft-client live test".into()),
        components: vec!["VEVENT".into()],
    };
    client.create_calendar(&calendar, &props).await.unwrap();
    let listed = client.list_collections(&calendar_home).await.unwrap();
    let created = listed.iter().find(|c| c.href == calendar).expect("listed");
    assert_eq!(created.display_name.as_deref(), Some("Live & test"));
    assert_eq!(created.color.as_deref(), Some("#336699"));
    assert_eq!(created.supported_components, ["VEVENT"]);
    eprintln!("created {created:?}");

    // Objects with preconditions.
    // CalCraft only accepts [a-z0-9._-] names; apps should use lower-case UIDs.
    let object = calendar.child("event-one.ics", false).unwrap();
    let etag1 = client
        .put_object(
            &object,
            CollectionKind::Calendar,
            &event("one", "One"),
            &Precondition::Create,
        )
        .await
        .unwrap()
        .expect("etag after create");
    let again = client
        .put_object(
            &object,
            CollectionKind::Calendar,
            &event("one", "One"),
            &Precondition::Create,
        )
        .await;
    assert!(
        matches!(again, Err(Error::PreconditionFailed(_))),
        "{again:?}"
    );

    // Initial sync and multiget.
    let empty = BTreeMap::new();
    let delta = client
        .sync(&calendar, &SyncState::default(), &empty)
        .await
        .unwrap();
    assert!(delta.full_resync);
    assert_eq!(delta.changed.len(), 1);
    assert_eq!(delta.changed[0].0, object);
    let hrefs: Vec<_> = delta.changed.iter().map(|(h, _)| h.clone()).collect();
    let fetched = client
        .multiget(&calendar, CollectionKind::Calendar, &hrefs)
        .await
        .unwrap();
    assert_eq!(fetched.objects.len(), 1);
    assert!(fetched.objects[0].data.contains("SUMMARY:One"));
    assert_eq!(fetched.objects[0].etag.as_deref(), Some(etag1.as_str()));

    // Update with the right and a stale etag.
    let etag2 = client
        .put_object(
            &object,
            CollectionKind::Calendar,
            &event("one", "One (edited)"),
            &Precondition::Update(etag1.clone()),
        )
        .await
        .unwrap()
        .expect("etag after update");
    let stale = client
        .put_object(
            &object,
            CollectionKind::Calendar,
            &event("one", "Lost update"),
            &Precondition::Update(etag1.clone()),
        )
        .await;
    assert!(
        matches!(stale, Err(Error::PreconditionFailed(_))),
        "{stale:?}"
    );

    // Incremental sync sees the change.
    let mut known = BTreeMap::new();
    known.insert(object.clone(), etag1.clone());
    let delta2 = client.sync(&calendar, &delta.state, &known).await.unwrap();
    assert!(!delta2.full_resync);
    assert_eq!(delta2.changed, [(object.clone(), Some(etag2.clone()))]);

    // Bogus token -> full resync.
    let bogus = SyncState {
        sync_token: Some("http://example.com/bogus/1".into()),
        ..SyncState::default()
    };
    let delta3 = client.sync(&calendar, &bogus, &known).await.unwrap();
    assert!(delta3.full_resync);

    // Delete with stale and right etag; sync reports the deletion.
    assert!(matches!(
        client.delete_object(&object, &etag1).await,
        Err(Error::PreconditionFailed(_))
    ));
    client.delete_object(&object, &etag2).await.unwrap();
    known.insert(object.clone(), etag2.clone());
    let delta4 = client.sync(&calendar, &delta2.state, &known).await.unwrap();
    assert_eq!(delta4.deleted, std::slice::from_ref(&object));

    // Ctag fallback path against the same server.
    let ctag_state = SyncState {
        use_ctag: true,
        ..SyncState::default()
    };
    let delta5 = client.sync(&calendar, &ctag_state, &known).await.unwrap();
    assert_eq!(delta5.deleted, std::slice::from_ref(&object));

    // Rename, then clean up.
    let rename = CollectionProps {
        display_name: Some("Renamed".into()),
        ..CollectionProps::default()
    };
    client
        .update_collection(&calendar, CollectionKind::Calendar, &rename)
        .await
        .unwrap();
    let listed = client.list_collections(&calendar_home).await.unwrap();
    assert!(
        listed
            .iter()
            .any(|c| c.href == calendar && c.display_name.as_deref() == Some("Renamed"))
    );
    client.delete_collection(&calendar).await.unwrap();

    // Address book via extended MKCOL.
    let book = book_home.child(&format!("live-{stamp}"), true).unwrap();
    client
        .create_addressbook(
            &book,
            &CollectionProps {
                display_name: Some("Live book".into()),
                ..CollectionProps::default()
            },
        )
        .await
        .unwrap();
    let card = book.child("carol.vcf", false).unwrap();
    let vcard = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:carol\r\nFN:Carol Contact\r\nEND:VCARD\r\n";
    client
        .put_object(
            &card,
            CollectionKind::AddressBook,
            vcard,
            &Precondition::Create,
        )
        .await
        .unwrap();
    let fetched = client
        .multiget(
            &book,
            CollectionKind::AddressBook,
            std::slice::from_ref(&card),
        )
        .await
        .unwrap();
    assert!(fetched.objects[0].data.contains("FN:Carol Contact"));
    let single = client.get_object(&card).await.unwrap();
    assert_eq!(single.data, vcard);
    client.delete_collection(&book).await.unwrap();
    eprintln!("live roundtrip ok");
}
