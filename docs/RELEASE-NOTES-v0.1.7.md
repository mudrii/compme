# Compme v0.1.7 — release candidate

> Unpublished candidate. v0.1.6 remains the supported, published release.
> The acceptance ledger and ROADMAP must be closed before tagging; publication,
> signing, notarization, cask finalization and post_verify have not run for 0.1.7.

This candidate includes the post-v0.1.6 correctness and privacy repairs.

- Card redaction covers Unicode decimal digits, adjacent valid card windows
  and overlapping matches. Encrypted typing-memory records are scrubbed before
  storage and re-redacted when older records are read, without rewriting the
  ciphertext or migrating the database.
- The macOS Carbon registry marshals native registration and teardown through
  the main thread, with rollback and stale-handler guards. Physical keyboard
  acceptance remains required.
- Accessibility safety polling tracks focus and caret independently; field
  reads and exact replacements fail closed when identity, selection or text
  changes.
- Memory deletion clears the corresponding live history buffers, and
  read/delete-only opens reuse an existing OS key-store entry.
- Missing-model startup retains present dynamic-loader path variables while
  excluding inherited completion settings.
- Release preparation keeps candidate and published versions distinct.
  Verified cask finalization updates the published documentation boundaries
  alongside the artifact version and checksum.
- The TextEdit acceptance harness now forwards its explicit trailing-space,
  screen-context and accept-key settings to the isolated product launch.

Windows remains a fail-closed scaffold. Linux remains experimental AT-SPI2/X11;
neither platform gains a supported published package in this macOS candidate.
