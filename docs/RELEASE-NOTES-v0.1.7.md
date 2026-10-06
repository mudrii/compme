# Compme v0.1.7 — release candidate

> Unpublished until the release workflow, cask finalization and post_verify
> complete; v0.1.6 remains the published release until then. Tagged under the
> owner's 2026-10-07 G11 decision: one of the 22 manual/live acceptance rows is
> closed and 21 remain open with recorded dispositions in docs/ACCEPTANCE.md.

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
- Settings confirmation dialogs activate and order Settings before running,
  so a deletion prompt no longer blocks an inactive Settings window unseen.
- Apps refreshes every row label after history erasure, so deleted apps no
  longer leave stale counts; long names truncate in the middle and keep the
  count visible, with the full row in a tooltip.
- The TextEdit acceptance harness now forwards its explicit trailing-space,
  screen-context and accept-key settings to the isolated product launch.

Windows remains a fail-closed scaffold. Linux remains experimental AT-SPI2/X11;
neither platform gains a supported published package in this macOS candidate.
