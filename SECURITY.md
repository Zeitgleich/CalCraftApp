# Security Policy

Security is the top priority of CalCraft and its app. Thank you for helping
keep it safe.

## Reporting a vulnerability

**Do not open a public issue.** Report vulnerabilities privately via
[GitHub Security Advisories](https://github.com/Zeitgleich/CalCraftApp/security/advisories/new).

Please include:

- affected version or commit
- description and impact
- steps to reproduce or a proof of concept

We aim to acknowledge reports within 72 hours and to provide a fix or mitigation
plan within 14 days. We credit reporters unless they prefer otherwise.

## Supported versions

Until 1.0, only the latest release receives security fixes.

## Security practices

- `unsafe_code = "forbid"` in all crates
- Dependency checks with `cargo-deny` on every change
- Tests for hostile server responses (oversized bodies, deep nesting, DTDs,
  malformed XML, foreign hrefs, redirect loops)
- Credentials stay on the configured origin, are zeroized on drop and are
  never logged
