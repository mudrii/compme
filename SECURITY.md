# Security Policy

## Reporting a vulnerability

Report privately via
[GitHub security advisories](https://github.com/mudrii/compme/security/advisories/new).
Please do not open public issues for security reports. You should receive a
response within 7 days.

## Supported versions

Only the latest published release receives security fixes.

## Scope notes

Compme is local-first: all inference runs on-device and the app makes no
telemetry connections (enforced by `tools/release/check-privacy-policy.sh` in
CI). Reports about the redaction layer, the encrypted typing memory, the
signed `compme://` deep-link scheme, secure-field handling, or the release
pipeline's signing/notarization chain are especially welcome.
