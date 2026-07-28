# CLI Coding-Tool Keybindings — Qwen Code · Kimi CLI · Codex CLI · Claude Code

> GitHub-flavored markdown. Sources fetched 2026-07-20: Claude Code `code.claude.com/docs/en/interactive-mode`; Qwen `docs/users/reference/keyboard-shortcuts.md`; Kimi `docs/en/reference/keyboard.md`; Codex `codex-rs/tui/src/keymap.rs` (`built_in_defaults`) + `developers.openai.com/codex/config-reference`.
> Bindings vary by version and terminal. On macOS, `Alt/Option` shortcuts need "Option as Meta" configured. Codex's map is fully rebindable via `[tui.keymap.*]` in `~/.codex/config.toml` (defaults shown). "Kimi Code" = MoonshotAI **kimi-cli**.

| Keystroke | Qwen Code | Kimi CLI | Codex CLI | Claude Code |
|---|---|---|---|---|
| `Ctrl+C` | Cancel request + clear input; ×2 exits | Clear input / interrupt running op | Interrupt/clear; ×2 exits | Interrupt; 1st clears input, 2nd exits |
| `Ctrl+D` | Exit if empty (×2 confirm); else delete char right | Exit (empty input) | Delete char right; exit if empty | Exit (×2 ~800ms); else delete char right |
| `Ctrl+Z` | — | — | — | Suspend to shell (Unix; `fg` resumes) |
| `Esc` | Close dialogs/suggestions | Close completion / skip question | Interrupt turn (stop response); cancel dialog | Interrupt Claude / close dialog |
| `Esc Esc` | Clear input draft | — | — (edit-prev is configurable) | Clear draft, or open rewind menu (if empty) |
| `Enter` | Submit | Submit; queue while streaming; confirm | Submit (composer); accept (list) | Submit |
| `Shift+Enter` | Newline | — | Newline | Newline (native in many terminals) |
| `Ctrl+J` | Newline | Newline | Newline; list-down | Newline (works in any terminal) |
| `Alt+Enter` | — | Newline | Newline | — (`Option+Enter` after Meta setup) |
| `Tab` | Autocomplete suggestion | Switch question tab | Queue message (composer) | Autocomplete (commands/files) |
| `Shift+Tab` | Cycle approval mode (plan→default→auto-edit→auto→yolo) | Toggle plan mode | — | Cycle permission mode (default→acceptEdits→plan→auto) |
| `Ctrl+X` | Open input in external editor | Toggle agent/shell mode | — | `Ctrl+X Ctrl+E` editor; `Ctrl+X Ctrl+K` stop subagents |
| `Ctrl+L` | Clear screen | — | Clear terminal | Redraw screen |
| `Ctrl+R` | Reverse history search | — | History search (previous) | Reverse history search |
| `Ctrl+S` | Stash/restore input; if empty: print full output | Steer — inject input into running turn | History search (next) | Stash/restore prompt |
| `Ctrl+O` | Toggle full transcript view | Edit in external editor | Copy | Toggle transcript viewer |
| `Ctrl+T` | Toggle tool descriptions | — | Open transcript | Toggle task checklist |
| `Ctrl+B` | Background running shell; (in input) cursor-left | — | Cursor-left (editor); page-up (pager) | Background running task |
| `Ctrl+G` | Show IDE context | — | Open external editor | Open text editor (alt: `Ctrl+X Ctrl+E`) |
| `Ctrl+A` | Cursor → line start | — | Line start (editor); open full approval | Cursor → line start |
| `Ctrl+E` | Cursor → line end | Expand full approval content | Line end (editor) | Cursor → line end |
| `Ctrl+K` | Delete to end of line | — | Kill to line end; list-up | Delete to end of line |
| `Ctrl+U` | Delete to line start | — | Kill to line start; half-page-up (pager) | Delete to line start |
| `Ctrl+W` | Delete word left | — | Delete word backward | Delete previous word |
| `Ctrl+Y` | Retry last failed request | — | Yank (paste killed text) | Paste deleted text |
| `Ctrl+V` | Paste (image→reference); `Alt+V` on Win | Paste (images/video) | — (terminal paste) | Paste image (`Cmd+V`/`Alt+V`) |
| `Alt+B` / `Alt+F` | Word left / right | — | Word left / right | Word back / forward (needs Meta) |
| `Up`/`Down` | Row nav, then history | Navigate options; `↑` recalls queued msg | Cursor/history; `Shift`=reasoning | Cursor/history; `Left/Right` cycle tabs |
| `Shift+Up/Down` | Scroll history (virtualized-buffer mode) | — | Inc/dec reasoning effort | — |
| `Alt+,` / `Alt+.` | — | — | Dec/inc reasoning effort | — |
| `Alt+Up` / `Shift+Left` | — | — | Edit queued message | — |
| `Alt+M` | Toggle markdown render (rich/raw) | — | — | Cycle permission mode (Windows) |
| `Alt+R` | — | — | Toggle raw-output mode | — |
| `/` | Slash command | Slash-command completion | Slash command | Command or skill |
| `@` | File reference | File-path completion | File mention | File mention |
| `!` | Toggle shell mode (empty input) | — (use `Ctrl+X`) | — | Shell mode (run command) |
| `?` | Toggle shortcuts help (empty input) | — | Toggle shortcuts | Toggle shortcut help (empty input) |
| `1–9` / `1–5` | Select item by number (menus) | `1–4` approval (`4`=decline+feedback); `1–5` question | — | — |
| `y`/`a`/`p`/`d`/`n` | — | — | Approve / session / prefix / deny / decline (`Esc`/`n`) | — |

## Notable divergences

- **Mode switching:** Qwen & Claude cycle modes with `Shift+Tab`; Kimi uses `Shift+Tab` for plan-only and `Ctrl+X` to flip agent↔shell; Codex has no default mode-toggle key (use `/approvals`, `/model`).
- **Mid-turn steering:** only Kimi has it (`Ctrl+S` injects immediately); the others queue messages for after the turn.
- **`Ctrl+Y`:** Qwen = retry last failed request; Codex/Claude = yank (paste killed text).
- **`Ctrl+O`:** Qwen/Claude = transcript; Kimi = external editor; Codex = copy.
- **External editor:** Qwen `Ctrl+X` · Kimi `Ctrl+O` · Codex `Ctrl+G` · Claude `Ctrl+G` (or `Ctrl+X Ctrl+E`).
- **Approvals:** Codex uses letter keys (`y/a/p/d/n`); Kimi uses number keys (`1–4`); Qwen/Claude use number + arrows.
- **Reasoning effort:** Codex `Shift+↑/↓` or `Alt+,/.`; not bound by default in the others.
