# Troubleshooting

Compme is fail-closed by design: when it cannot read a field safely, or a
policy says no, it shows nothing instead of risking wrong text. Most "no
suggestions" reports are one of the cases below.

**First diagnostic:** open the menu-bar icon → **Settings…** → **Setup** tab
and read the readiness checklist (Accessibility / Screen Recording / model
file). The rows re-probe each time the pane is shown, so a green checklist
means the app sees what it needs.

## No suggestions appear

- **Accessibility permission is not granted.** Without it Compme cannot read
  the focused field at all. Enable Compme under System Settings → Privacy &
  Security → Accessibility, then re-check the Setup-tab checklist. (Input
  Monitoring is not required by the production accept path.)
- **The field is secure, or Secure Input is on globally.** Password fields
  and any app that enables global Secure Input are always blocked — by
  design, with no override. Suggestions resume in normal fields.
- **The app or field is unsupported.** Compme classifies every focused
  field; unreadable or unwritable fields, and apps in the Unsupported
  compatibility tier, fail closed. Code editors and terminals are gated by
  their compatibility tier (terminal command lines are blocked, while
  natural-language AI prompts can be allowed), and SidebarOnly editors such
  as VS Code enable only conservative assistant/sidebar fields.
- **The app or domain is excluded, or suggestions are paused.** Per-app and
  per-browser-domain exclusions (`COMPME_EXCLUDED_APPS` /
  `COMPME_EXCLUDED_DOMAINS`, the Apps pane, or confirmed `compme://`
  override links) suppress suggestions there, as do the master Enabled
  switch (tray checkmark) and the global pause/snooze.
- **Only autocorrect seems missing.** The opt-in statistical autocorrect
  (`COMPME_FULL_AUTOCORRECT`) runs only in a conservative known-prose app
  allowlist or a positively classified assistant field; browsers, unknown
  apps, code editors, and code-like contexts fail closed.

## The model is not downloading or not generating

- **No model downloaded yet.** Inline completions need a local GGUF model:
  Setup tab → pick a catalog row → **Download**.
- **The model does not fit this machine.** Each catalog row carries a
  `fits` / `tight` / `exceeds` RAM verdict; on a machine below a model's
  minimum RAM the download is blocked and logged, and nothing is fetched.
- **The model is already on disk.** A dest-exists guard skips re-downloading
  an existing model; delete it first if you meant to fetch it again.

## Requirements mismatch

- macOS 14 (Sonoma) or later is required; the bundle sets
  `LSMinimumSystemVersion` 14.0.
- The published Homebrew cask and release pipeline are Apple silicon
  (arm64) only — Intel Macs are not supported by the published cask.

## Filing an issue

If none of the above explains it, open an issue at
<https://github.com/mudrii/compme/issues> with your macOS version, the app
where suggestions were missing, and what the Setup-tab checklist showed.
Report vulnerabilities privately through
[GitHub security advisories](https://github.com/mudrii/compme/security/advisories/new)
(see [SECURITY.md](../SECURITY.md)) — please do not open public issues for
security reports.
