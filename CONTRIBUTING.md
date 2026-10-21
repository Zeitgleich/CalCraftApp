# Contributing to CalCraftApp

Thanks for your interest in contributing!

## Developer Certificate of Origin

All commits must be signed off (`git commit -s`), certifying the
[Developer Certificate of Origin](https://developercertificate.org/).

## Before opening a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p calcraft-client --all-targets --no-default-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

Optionally run the end-to-end test against a CalCraft server (see
`crates/calcraft-client/tests/live_calcraft.rs`):

```sh
CALCRAFT_TEST_URL=http://127.0.0.1:5232 CALCRAFT_TEST_USER=alice \
CALCRAFT_TEST_PASSWORD='...' cargo test -p calcraft-client --test live_calcraft -- --nocapture
```

## Guidelines

- No `unsafe` code.
- New dependencies need a short justification in the pull request; keep the
  dependency tree small.
- Parsers and protocol handlers need tests for malformed and hostile input.
- Never log credentials or object contents.
- Security issues: see [SECURITY.md](SECURITY.md), never in public issues.

The project license has not been decided yet (see README.md); contributions
will be licensed under the license chosen before the first release.
