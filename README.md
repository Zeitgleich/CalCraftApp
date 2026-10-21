# CalCraftApp

Cross-platform calendar and contacts client for
[CalCraft](https://github.com/Zeitgleich/CalCraft), the security-first,
self-hosted CalDAV/CardDAV server. The app will be built with Tauri 2 and
Leptos and speaks standard CalDAV/CardDAV, so it also works with other servers
(Radicale, Baïkal/sabre, Nextcloud, ...).

## Status

Milestone **C1** (protocol library) is in progress. There is no UI yet. See
[PLAN.md](PLAN.md) for the roadmap.

## Workspace

| Crate | Purpose |
| --- | --- |
| [`crates/calcraft-client`](crates/calcraft-client) | CalDAV/CardDAV protocol library |

## Architecture of `calcraft-client`

```
DavClient<T: HttpTransport>
 ├─ discovery    RFC 6764 well-known, current-user-principal, home sets
 ├─ collections  PROPFIND listing, MKCALENDAR, extended MKCOL, PROPPATCH
 ├─ sync         sync-collection (RFC 6578, 507 continuation, token reset)
 │               with ctag + etag fallback
 ├─ objects      calendar-/addressbook-multiget, PUT/DELETE with If-Match /
 │               If-None-Match, 412 -> Error::PreconditionFailed
 ├─ xml          request escaping; hardened multistatus parser (quick-xml):
 │               size/depth/element limits, no DTDs or custom entities,
 │               namespace-resolved (arbitrary prefixes)
 ├─ url          strict http(s) URLs, RFC 3986 reference resolution,
 │               canonical hrefs (percent-encoding normalised)
 └─ transport    HttpTransport trait; HyperTransport (feature
                 `hyper-transport`, default): hyper 1 + rustls (ring)
```

Security properties:

- **One origin per client.** Requests outside the configured scheme/host/port
  are refused before sending, foreign hrefs in responses are dropped, and
  cross-origin redirects are not followed, so credentials cannot leak to
  other hosts.
- **HTTPS required** except for loopback hosts, unless explicitly allowed.
- **Credentials** (HTTP Basic with app passwords) are supplied by the caller,
  zeroized on drop (`zeroize`), redacted in `Debug`, and never logged; the
  library has no logging at all.
- **Hard limits** on response sizes, XML depth and element count, and object
  size. Object bodies (iCalendar/vCard) are treated as opaque UTF-8 text.
- No `unsafe` code.

### Dependencies

Without the default transport (`--no-default-features`) the library depends
only on `quick-xml` (+ `memchr`) and `zeroize`. The `hyper-transport` feature
adds hyper, hyper-util, hyper-rustls, rustls (ring provider), webpki-roots,
tokio, http and http-body-util (39 crates in total on macOS).

### Example

```rust,ignore
use calcraft_client::{Credentials, DavClient, HyperTransport, SyncState};

let credentials = Credentials::basic("alice", app_password)?;
let client = DavClient::new("https://cal.example.com", credentials, HyperTransport::new()?)?;
let discovery = client.discover().await?;
for home in &discovery.calendar_homes {
    for calendar in client.list_collections(home).await? {
        let delta = client.sync(&calendar.href, &SyncState::default(), &Default::default()).await?;
        let hrefs: Vec<_> = delta.changed.into_iter().map(|(href, _)| href).collect();
        let objects = client.multiget(&calendar.href, calendar.kind, &hrefs).await?;
    }
}
```

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the live test against a real
CalCraft server and [SECURITY.md](SECURITY.md) for reporting vulnerabilities.

## License

License: to be decided (MPL-2.0 or MIT/Apache-2.0). The app is meant to be
distributable through app stores, which rules out the AGPL used by the
CalCraft server. No CalCraft server code is included in this repository.
