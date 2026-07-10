# Security Policy

## Reporting a vulnerability

Report privately via
[GitHub security advisories](https://github.com/mudrii/compme/security/advisories/new).
Please do not open public issues for security reports. You should receive a
response within 7 days.

## Supported versions

Only the latest published release receives security fixes. The current
supported release is `v0.1.4`; earlier releases are unsupported.

## Scope notes

Compme is local-first: inference runs on-device and the app does not send
analytics or telemetry. It does have explicit network-facing actions, including
model downloads and opening release/documentation URLs. The CI policy check
`tools/release/check-privacy-policy.sh` rejects known telemetry dependencies and
hosts, and requires every referenced network host to be reviewed; it is a
defense-in-depth static check, not proof of all runtime traffic.
CI and stable-tag validation also run the exact-SHA-pinned RustSec audit action
against `Cargo.lock`; local release-readiness proof uses `cargo audit`.

Reports about the redaction layer, the opt-in encrypted typing memory,
`compme://` signature verification and mandatory confirmation, secure-field
handling, or the release pipeline's signing/notarization chain are especially
welcome. Unsigned deep links are limited to the reversible command subset and
still require host confirmation; signed links are verified only when a trusted
public key is configured.
