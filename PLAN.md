# CalCraftApp plan

| Milestone | Scope |
| --- | --- |
| **C1** | `calcraft-client`: CalDAV/CardDAV protocol library (discovery, collections, sync-collection with ctag fallback, multiget, conditional PUT/DELETE, MKCALENDAR/MKCOL/PROPPATCH), pluggable HTTP transport with a hyper + rustls default, hostile-response tests and a live test against CalCraft. |
| **C2** | Leptos UI components (month/week/day views, event and contact editors) and the Tauri 2 desktop shell; account setup via discovery; credentials in the OS keychain. |
| **C3** | Offline cache (local store per account), background sync with conflict handling (412 -> refetch and merge), iCalendar/vCard parsing and recurrence expansion, local reminders/notifications. |
| **C4** | Signed and notarised installers for macOS, Windows and Linux; auto-update with signature verification; crash-free release process. |
| **C5** | Mobile (Tauri mobile for iOS and Android): platform keychains, background refresh, push-free sync strategy, App Store/Play Store distribution. |

## C1 follow-ups

- Decide the license (MPL-2.0 or MIT/Apache-2.0) before the first release.
- Consider `rustls-platform-verifier` (OS trust store) for C2/C5 instead of
  the bundled Mozilla roots plus user-added CAs.
- Fuzz the multistatus parser (`cargo fuzz`), like the CalCraft server does.
- Run the live test in CI against a CalCraft build.
