//! Application compatibility tiers (design spec §16 compatibility-parity table,
//! mirroring `cotypist.app/compatibility`). A pure classifier from a macOS bundle
//! id to a tier, plus the policy each tier implies. The live per-app verification
//! (that each app actually behaves as its tier claims) is environment-bound; this
//! crate is the deterministic core that drives gating.

/// How well an application is supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatTier {
    /// Inline suggestion + accept works directly.
    Works,
    /// Needs user setup first (e.g. Google Docs Accessibility / Text Metrics).
    SetupNeeded,
    /// Only a mirror window can render suggestions (Firefox/Zen).
    MirrorOnly,
    /// Partial support; suggestions may be limited (Slack).
    Partial,
    /// Only AI-chat/sidebar fields, never the editor pane (VS Code/Cursor/…).
    SidebarOnly,
    /// Explicitly unsupported — never suggest (Thunderbird/Pages/Ghostty/…).
    Unsupported,
    /// Not in the known table — treat conservatively but allow.
    Unknown,
}

impl CompatTier {
    /// Whether the engine should attempt completions in this app at all. The
    /// `Unsupported` tier hard-blocks; everything else may suggest (subject to the
    /// app's own field capabilities and the user's preferences). `SidebarOnly`
    /// needs an additional field-level check by callers before suggestions are
    /// safe in code editors.
    pub fn allows_suggestions(self) -> bool {
        !matches!(self, CompatTier::Unsupported)
    }

    /// Whether suggestions in this app should be restricted to AI-chat/sidebar
    /// fields (not the main editor pane).
    pub fn sidebar_only(self) -> bool {
        matches!(self, CompatTier::SidebarOnly)
    }
}

/// Whether a bundle id is a web browser. Google Docs and other web editors run
/// inside one, so a browser field that is not yet readable may need
/// Accessibility / Text-Metrics setup. (MirrorOnly browsers render via the
/// engine's existing popup-anchor fallback when inline caret geometry is absent.)
pub fn is_browser(bundle_id: &str) -> bool {
    // Exact ids first (the curated set)...
    if matches!(
        bundle_id,
        "com.apple.Safari"
            | "com.google.Chrome"
            | "com.microsoft.edgemac"
            | "com.brave.Browser"
            | "com.vivaldi.Vivaldi"
            | "company.thebrowser.Browser"
            | "company.thebrowser.dia"
            | "org.mozilla.firefox"
            | "org.mozilla.firefoxdeveloperedition"
            | "org.mozilla.nightly"
            | "app.zen-browser.zen"
    ) {
        return true;
    }
    // ...then dot-bounded families for variant builds (canary/beta/dev/
    // nightly, Chromium forks). The trailing '.' keeps lookalike prefixes
    // out: com.google.Chromecast must never trigger an AX URL read. Helper
    // bundles (com.google.Chrome.helper) DO match by design — harmless,
    // since helpers never own the focused AX field on macOS (the browser
    // process hosts the AX tree), and excluding them would need a deny-list
    // that drifts.
    [
        "com.google.Chrome.",
        "org.chromium.",
        "com.vivaldi.",
        "com.microsoft.edgemac.",
        "com.brave.Browser.",
    ]
    .iter()
    .any(|family| {
        bundle_id
            .strip_prefix(family)
            .is_some_and(|rest| !rest.is_empty())
    })
}

/// Whether the focused field needs Accessibility/Text-Metrics setup before inline
/// suggestions work (design spec §16 "Setup needed"): a browser or an explicitly
/// setup-needed app (Arc/Dia) whose field text is not yet readable — the case
/// Google Docs hits in Chrome until Accessibility mode is enabled.
pub fn needs_accessibility_setup(bundle_id: &str, readable_text: bool) -> bool {
    !readable_text
        && (is_browser(bundle_id)
            || matches!(compatibility_tier(bundle_id), CompatTier::SetupNeeded))
}

/// Shell command leaders that mark a terminal line as a command, not an
/// AI-agent natural-language prompt.
const SHELL_LEADERS: &[&str] = &[
    "cd", "ls", "git", "cat", "rm", "cp", "mv", "mkdir", "sudo", "brew", "npm", "cargo", "python",
    "pip", "ssh", "curl", "grep", "rg", "echo", "vim", "nano", "make", "docker", "kubectl", "npx",
    "pnpm",
];

const GO_SUBCOMMANDS: &[&str] = &[
    "bug",
    "build",
    "clean",
    "doc",
    "env",
    "fmt",
    "generate",
    "get",
    "help",
    "install",
    "list",
    "mod",
    "run",
    "telemetry",
    "test",
    "tool",
    "version",
    "vet",
    "work",
];

/// Whether a bundle id is a terminal emulator whose suggestions should only
/// activate for AI-agent natural-language prompts, not arbitrary shell input.
pub fn is_terminal(bundle_id: &str) -> bool {
    matches!(bundle_id, "com.apple.Terminal" | "com.googlecode.iterm2")
}

/// Heuristic for terminal AI-agent prompt activation (design spec §16): in a
/// terminal, only suggest when the current line looks like a natural-language
/// prompt (several words, lowercase prose) rather than a shell command. Outside
/// terminals this always returns true (no restriction).
pub fn terminal_prompt_activates(bundle_id: &str, left_context: &str) -> bool {
    if !is_terminal(bundle_id) {
        return true;
    }
    let line = left_context
        .rsplit('\n')
        .next()
        .unwrap_or(left_context)
        .trim();
    // Drop leading shell-prompt sigils ($ / % / > / #) that render as their own
    // tokens, so the real command word is inspected (not a bare sigil).
    let tokens: Vec<&str> = line
        .split_whitespace()
        .map(|word| word.trim_start_matches(['$', '%', '>', '#']))
        .filter(|word| !word.is_empty())
        .collect();
    if tokens.len() < 3 {
        return false;
    }
    // A recognized shell command leader → treat as shell input, not a prompt.
    if SHELL_LEADERS.contains(&tokens[0].to_lowercase().as_str()) {
        return false;
    }
    if is_go_command(&tokens) {
        return false;
    }
    // Executable path invocation → treat as shell input even when the path or
    // flags contain lowercase ASCII that would otherwise look prose-like.
    if looks_like_shell_path_invocation(tokens[0], &tokens[1..]) {
        return false;
    }
    // Require some lowercase-alphabetic prose so a bare path/flags line is skipped.
    line.chars().any(|c| c.is_ascii_lowercase())
}

fn looks_like_shell_path_invocation(first: &str, rest: &[&str]) -> bool {
    if !is_path_token(first) || is_single_segment_slash_command(first) {
        return false;
    }
    if is_executable_path_token(first) {
        return true;
    }
    rest.iter()
        .any(|token| token.starts_with('-') || is_path_token(token))
}

fn is_single_segment_slash_command(token: &str) -> bool {
    token.starts_with('/') && !token[1..].contains('/')
}

fn is_executable_path_token(token: &str) -> bool {
    if let Some(relative) = token
        .strip_prefix("./")
        .or_else(|| token.strip_prefix("../"))
    {
        return !relative.contains('/')
            || relative.starts_with("bin/")
            || relative.starts_with("scripts/");
    }
    if let Some(home_relative) = token.strip_prefix("~/") {
        return !home_relative.contains('/')
            || home_relative.starts_with("bin/")
            || home_relative.starts_with(".local/bin/");
    }
    if token.starts_with("/Applications/") {
        return token.contains("/Contents/MacOS/");
    }
    if is_users_executable_path(token) {
        return true;
    }
    token.starts_with("/bin/")
        || token.starts_with("/sbin/")
        || token.starts_with("/usr/")
        || token.starts_with("/opt/")
        || token.starts_with("/tmp/")
        || token.starts_with("/private/tmp/")
        || token.starts_with("/nix/store/")
}

fn is_users_executable_path(token: &str) -> bool {
    let Some(rest) = token.strip_prefix("/Users/") else {
        return false;
    };
    let Some((_, user_relative)) = rest.split_once('/') else {
        return false;
    };
    user_relative.starts_with("bin/") || user_relative.starts_with(".local/bin/")
}

fn is_go_command(tokens: &[&str]) -> bool {
    if !tokens[0].eq_ignore_ascii_case("go") {
        return false;
    }
    let Some(subcommand) = tokens.get(1).map(|token| token.to_lowercase()) else {
        return false;
    };
    if subcommand == "fix" {
        return tokens.iter().skip(2).any(|token| is_go_fix_target(token));
    }
    GO_SUBCOMMANDS.contains(&subcommand.as_str())
}

fn is_go_fix_target(token: &str) -> bool {
    token.starts_with('-')
        || is_path_token(token)
        || token == "."
        || token == "all"
        || token == "std"
        || token.contains("...")
        || token.contains('/')
}

fn is_path_token(token: &str) -> bool {
    token.starts_with('/')
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with("~/")
}

/// Classify a macOS application bundle id into a compatibility tier.
pub fn compatibility_tier(bundle_id: &str) -> CompatTier {
    match bundle_id {
        // Works — a representative set across families. The curated Chromium
        // browsers (Brave/Edge/Vivaldi) classify like Chrome — they share its
        // Blink engine and were previously fail-open Unknown despite being
        // listed in `is_browser`, so they emitted no compat guidance.
        "com.apple.Safari"
        | "com.google.Chrome"
        | "com.brave.Browser"
        | "com.microsoft.edgemac"
        | "com.vivaldi.Vivaldi"
        | "com.apple.mail"
        | "com.microsoft.Word"
        | "com.apple.TextEdit"
        | "com.apple.Notes"
        | "notion.id"
        | "md.obsidian"
        | "com.apple.MobileSMS"
        | "com.apple.Terminal"
        | "com.googlecode.iterm2" => CompatTier::Works,

        // Setup needed — Arc/Dia need Text Metrics / a launch flag for inline.
        "company.thebrowser.Browser" | "company.thebrowser.dia" => CompatTier::SetupNeeded,

        // Mirror-window only. All Gecko/Firefox-family builds share the same
        // lack of inline caret geometry, so the Developer Edition and Nightly
        // variants must classify like the stable build (they were fail-open
        // Unknown before — `is_browser` lists them but the tier arm didn't).
        "org.mozilla.firefox"
        | "org.mozilla.firefoxdeveloperedition"
        | "org.mozilla.nightly"
        | "app.zen-browser.zen" => CompatTier::MirrorOnly,

        // Partial.
        "com.tinyspeck.slackmacgap" => CompatTier::Partial,

        // Sidebar/AI-chat only (code editors — editor pane stays disabled).
        "com.microsoft.VSCode" | "com.todesktop.230313mzl4w4u92" | "com.exafunction.windsurf" => {
            CompatTier::SidebarOnly
        }

        // Explicitly unsupported.
        "org.mozilla.thunderbird"
        | "com.apple.iWork.Pages"
        | "com.literatureandlatte.scrivener3"
        | "com.microsoft.onenote.mac"
        | "com.barebones.bbedit"
        | "com.sublimetext.3"
        | "com.sublimetext.4"
        | "com.mitchellh.ghostty"
        | "dev.warp.Warp-Stable" => CompatTier::Unsupported,

        _ => CompatTier::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_apps_map_to_their_tiers() {
        for (bundle, tier) in [
            ("com.apple.Safari", CompatTier::Works),
            ("com.google.Chrome", CompatTier::Works),
            ("com.apple.mail", CompatTier::Works),
            ("com.microsoft.Word", CompatTier::Works),
            ("com.apple.TextEdit", CompatTier::Works),
            ("com.apple.Notes", CompatTier::Works),
            ("notion.id", CompatTier::Works),
            ("md.obsidian", CompatTier::Works),
            ("com.apple.MobileSMS", CompatTier::Works),
            ("com.apple.Terminal", CompatTier::Works),
            ("com.googlecode.iterm2", CompatTier::Works),
            ("com.brave.Browser", CompatTier::Works),
            ("com.microsoft.edgemac", CompatTier::Works),
            ("com.vivaldi.Vivaldi", CompatTier::Works),
            ("company.thebrowser.Browser", CompatTier::SetupNeeded),
            ("company.thebrowser.dia", CompatTier::SetupNeeded),
            ("org.mozilla.firefox", CompatTier::MirrorOnly),
            (
                "org.mozilla.firefoxdeveloperedition",
                CompatTier::MirrorOnly,
            ),
            ("org.mozilla.nightly", CompatTier::MirrorOnly),
            ("app.zen-browser.zen", CompatTier::MirrorOnly),
            ("com.tinyspeck.slackmacgap", CompatTier::Partial),
            ("com.microsoft.VSCode", CompatTier::SidebarOnly),
            ("com.todesktop.230313mzl4w4u92", CompatTier::SidebarOnly),
            ("com.exafunction.windsurf", CompatTier::SidebarOnly),
            ("org.mozilla.thunderbird", CompatTier::Unsupported),
            ("com.apple.iWork.Pages", CompatTier::Unsupported),
            ("com.literatureandlatte.scrivener3", CompatTier::Unsupported),
            ("com.microsoft.onenote.mac", CompatTier::Unsupported),
            ("com.barebones.bbedit", CompatTier::Unsupported),
            ("com.sublimetext.3", CompatTier::Unsupported),
            ("com.sublimetext.4", CompatTier::Unsupported),
            ("com.mitchellh.ghostty", CompatTier::Unsupported),
            ("dev.warp.Warp-Stable", CompatTier::Unsupported),
        ] {
            assert_eq!(compatibility_tier(bundle), tier, "{bundle}");
        }
    }

    #[test]
    fn browser_families_match_variants_but_not_lookalikes() {
        // The domain-detection pre-gate: Chromium-family variants and
        // siblings count as browsers...
        for browser in [
            "com.apple.Safari",
            "com.google.Chrome",
            "com.google.Chrome.canary",
            "com.google.Chrome.beta",
            "org.chromium.Chromium",
            "com.microsoft.edgemac",
            "com.microsoft.edgemac.Beta",
            "com.brave.Browser",
            "com.brave.Browser.nightly",
            "com.vivaldi.Vivaldi",
            "com.vivaldi.Vivaldi.snapshot",
            "company.thebrowser.Browser",
            "company.thebrowser.dia",
            "org.mozilla.firefox",
            "org.mozilla.firefoxdeveloperedition",
            "org.mozilla.nightly",
            "app.zen-browser.zen",
            // In-family helper bundles match BY DESIGN (see is_browser):
            // helpers never own the focused AX field, and a deny-list of
            // helper suffixes would drift.
            "com.google.Chrome.helper",
        ] {
            assert!(is_browser(browser), "{browser} should be a browser");
        }
        // ...but the prefix match is dot-bounded: a lookalike bundle id
        // sharing the prefix string must NOT trigger AX URL reads.
        for not_browser in [
            "com.google.Chromecast",
            "com.apple.TextEdit",
            "com.brave.BrowserHelper.weird", // outside the dotted family
            "org.chromium",                  // bare prefix without a segment
        ] {
            assert!(!is_browser(not_browser), "{not_browser} is not a browser");
        }
    }

    #[test]
    fn unknown_app_is_unknown_and_allowed() {
        let tier = compatibility_tier("com.example.SomethingNew");
        assert_eq!(tier, CompatTier::Unknown);
        assert!(tier.allows_suggestions());
    }

    #[test]
    fn unsupported_blocks_suggestions() {
        assert!(!compatibility_tier("com.mitchellh.ghostty").allows_suggestions());
        assert!(!compatibility_tier("com.apple.iWork.Pages").allows_suggestions());
    }

    #[test]
    fn unsupported_is_the_only_global_block() {
        for app in [
            "com.apple.TextEdit",
            "company.thebrowser.Browser",
            "org.mozilla.firefox",
            "com.tinyspeck.slackmacgap",
            "com.example.unknown",
        ] {
            assert!(
                compatibility_tier(app).allows_suggestions(),
                "{app} should allow suggestions"
            );
        }
        assert!(compatibility_tier("com.microsoft.VSCode").allows_suggestions());
    }

    #[test]
    fn sidebar_only_flag_is_set_only_for_code_editors() {
        assert!(compatibility_tier("com.microsoft.VSCode").sidebar_only());
        assert!(!compatibility_tier("com.apple.TextEdit").sidebar_only());
        assert!(!compatibility_tier("com.example.unknown").sidebar_only());
    }

    #[test]
    fn terminal_activates_only_for_natural_language_prompts() {
        let term = "com.googlecode.iterm2";
        // Shell commands → no suggestions.
        assert!(!terminal_prompt_activates(term, "git commit -m wip"));
        assert!(!terminal_prompt_activates(term, "ls -la /tmp"));
        // Shell commands behind a prompt sigil → still no suggestions (review #1).
        assert!(!terminal_prompt_activates(term, "$ git push origin main"));
        assert!(!terminal_prompt_activates(term, "% npm install left-pad"));
        assert!(!terminal_prompt_activates(term, "> cargo build --release"));
        // Too few words → no.
        assert!(!terminal_prompt_activates(term, "hello there"));
        // Natural-language agent prompt → yes.
        assert!(terminal_prompt_activates(
            term,
            "please refactor the parser to"
        ));
        // ...even behind a prompt sigil.
        assert!(terminal_prompt_activates(
            term,
            "$ please summarize the recent changes"
        ));
        // Only the current line matters.
        assert!(terminal_prompt_activates(
            term,
            "git status\nplease summarize the diff for"
        ));
    }

    #[test]
    fn terminal_activates_for_minimal_three_word_prompt() {
        // Exactly three lowercase-prose tokens with a non-shell leader is the
        // smallest accepted prompt: pins the `tokens.len() < 3` lower bound so a
        // `<= 3` mutant (which would reject this legit prompt) is caught.
        assert!(terminal_prompt_activates(
            "com.googlecode.iterm2",
            "summarize the diff"
        ));
    }

    #[test]
    fn terminal_skips_shell_leaders_case_insensitively() {
        // The shell-leader gate lowercases tokens[0] before matching SHELL_LEADERS
        // (line ~157), so a mixed/upper-case leader is still recognized as shell
        // input and does NOT activate. Existing leader tests only use lowercase or
        // sigil-prefixed leaders, leaving the case-folding path uncovered.
        let term = "com.googlecode.iterm2";
        assert!(
            !terminal_prompt_activates(term, "Git push origin main"),
            "a mixed-case shell leader is still shell input"
        );
        assert!(
            !terminal_prompt_activates(term, "GIT commit -m wip"),
            "an upper-case shell leader is still shell input"
        );
    }

    #[test]
    fn terminal_skips_go_commands_case_insensitively() {
        // is_go_command compares the `go` leader with eq_ignore_ascii_case and
        // lowercases the subcommand (lines ~227-230), so case variants of `go`
        // and its subcommands are still treated as shell input. Existing go
        // coverage only uses all-lowercase forms.
        let term = "com.googlecode.iterm2";
        assert!(
            !terminal_prompt_activates(term, "GO test ./..."),
            "an upper-case `go` leader is still a go command"
        );
        assert!(
            !terminal_prompt_activates(term, "go FIX ./..."),
            "an upper-case go subcommand is still a go command"
        );
    }

    #[test]
    fn apple_terminal_is_gated_like_iterm() {
        // is_terminal classifies com.apple.Terminal alongside iTerm2 (line ~130),
        // so the same prompt heuristic applies: shell input is skipped, but a
        // natural-language prompt activates. Existing terminal tests only exercise
        // iTerm2's bundle id.
        let term = "com.apple.Terminal";
        assert!(
            !terminal_prompt_activates(term, "git commit -m wip"),
            "a shell command does not activate in Apple Terminal"
        );
        assert!(
            terminal_prompt_activates(term, "please refactor the parser to"),
            "a natural-language prompt activates in Apple Terminal"
        );
    }

    #[test]
    fn accessibility_setup_for_unreadable_mirror_only_browser() {
        // Firefox is a MirrorOnly browser and is listed in is_browser, so
        // needs_accessibility_setup keys purely on browser+unreadable: true when
        // the field text is not readable, false once it is. Existing coverage uses
        // Chrome (Works) and Arc (SetupNeeded), not a MirrorOnly/Firefox-family id.
        assert!(needs_accessibility_setup("org.mozilla.firefox", false));
        assert!(!needs_accessibility_setup("org.mozilla.firefox", true));
    }

    #[test]
    fn accessibility_setup_is_false_for_non_browser_non_setup_app() {
        // needs_accessibility_setup only fires for browsers or SetupNeeded apps. A
        // plain editor (Works) or an Unknown app never needs it, even with
        // unreadable field text — the `&&` short-circuits on the second term. Pins
        // that the gate is not just `!readable_text`. Existing coverage only shows
        // the true side (browser/Arc) and the browser readable=true flip.
        assert!(!needs_accessibility_setup("com.apple.TextEdit", false));
        assert!(!needs_accessibility_setup("com.example.unknown", false));
        // ...and a SetupNeeded app flips back to false once its text is readable,
        // proving the readable_text term still gates the SetupNeeded branch.
        assert!(needs_accessibility_setup(
            "company.thebrowser.Browser",
            false
        ));
        assert!(!needs_accessibility_setup(
            "company.thebrowser.Browser",
            true
        ));
    }

    #[test]
    fn non_terminal_app_always_activates_regardless_of_line() {
        // terminal_prompt_activates is a no-op outside terminals: a non-terminal
        // bundle id returns true even for input that WOULD be rejected as shell
        // command in a terminal (a known shell leader, too few words, empty). Pins
        // the early `!is_terminal` true-return so the heuristic never leaks into
        // non-terminal apps.
        let editor = "com.apple.TextEdit";
        assert!(terminal_prompt_activates(editor, "git commit -m wip"));
        assert!(terminal_prompt_activates(editor, "hi"));
        assert!(terminal_prompt_activates(editor, ""));
    }

    #[test]
    fn terminal_skips_common_lowercase_shell_commands() {
        let term = "com.googlecode.iterm2";
        for command in [
            "npx ctx7 latest",
            "pnpm add react",
            "go test ./...",
            "go fix ./...",
            "go fix example.com/x",
            "go fix example.com/acme/widget",
            "go fix all",
            "go fix std",
            "go fix .",
            "go fix net/http",
            "go fix cmd/go",
            "go bug report",
            "go help test",
            "go help buildconstraint",
            "go telemetry on",
            "rg foo crates",
        ] {
            assert!(
                !terminal_prompt_activates(term, command),
                "{command} should be treated as shell input"
            );
        }
    }

    #[test]
    fn terminal_keeps_command_like_natural_language_prompts_active() {
        let term = "com.googlecode.iterm2";
        for prompt in [
            "go fix the failing tests",
            "/review the current tracked diff",
            "/graphify --update current repo",
            "/review --all current diff",
            "/Users/mudrii/src/compme has failing tests",
            "/tmp has failing tests",
            "/Applications/MyTool.app crashes on launch",
            "~/src/compme has failing tests",
            "./crates/compat has failing tests",
        ] {
            assert!(
                terminal_prompt_activates(term, prompt),
                "{prompt} should be treated as an AI-agent prompt"
            );
        }
    }

    #[test]
    fn terminal_empty_or_whitespace_line_does_not_activate() {
        // A freshly-cleared agent line (caret on an empty prompt) yields zero
        // tokens → the `< 3` branch returns false, so nothing fires on a blank
        // line. Existing tests only cover the 2-token `< 3` case ("hello there").
        let term = "com.googlecode.iterm2";
        assert!(!terminal_prompt_activates(term, ""));
        assert!(!terminal_prompt_activates(term, "   "));
        // The current (last) line is blank even though a prior line had tokens.
        assert!(!terminal_prompt_activates(term, "git status\n   "));
    }

    #[test]
    fn terminal_does_not_activate_for_prompt_without_lowercase() {
        // The final gate (line ~169) requires at least one ASCII-lowercase letter
        // for the line to count as natural-language prose. A >=3-token line that
        // clears the leader/go/path checks but contains NO ascii-lowercase letter
        // therefore returns false. Covers the all-uppercase prose case (distinct
        // from RUN BUILD NOW above) and the no-letter-at-all (digit/punct) case the
        // existing tests never exercise.
        let term = "com.googlecode.iterm2";
        assert!(
            !terminal_prompt_activates(term, "PLEASE REFACTOR PARSER"),
            "an all-uppercase >=3-token line has no lowercase prose"
        );
        assert!(
            !terminal_prompt_activates(term, "123 456 789"),
            "a digit-only >=3-token line has no lowercase prose"
        );
    }

    #[test]
    fn terminal_skips_uppercase_or_pathy_lines_with_no_prose() {
        let term = "com.googlecode.iterm2";
        // >=3 tokens, leader not a known shell command, and no lowercase prose at
        // all → the no-lowercase-prose `false` branch (line ~110).
        assert!(!terminal_prompt_activates(term, "RUN BUILD NOW"));
        // An all-uppercase pathy/flags line likewise has no lowercase prose → false.
        assert!(!terminal_prompt_activates(
            term,
            "/USR/LOCAL/BIN/TOOL --FLAG /TMP/OUT"
        ));
        // The same shell shape lowercased is still a command, not prose.
        assert!(!terminal_prompt_activates(
            term,
            "/usr/local/bin/tool --flag /tmp/OUT"
        ));
        assert!(!terminal_prompt_activates(term, "./script --dry-run now"));
        assert!(!terminal_prompt_activates(
            term,
            "./scripts/tool input output"
        ));
        assert!(!terminal_prompt_activates(term, "../bin/tool input output"));
        assert!(!terminal_prompt_activates(term, "~/bin/tool input output"));
        assert!(!terminal_prompt_activates(
            term,
            "/Users/mudrii/.local/bin/tool input output"
        ));
        assert!(!terminal_prompt_activates(
            term,
            "/usr/local/bin/tool input output"
        ));
        assert!(!terminal_prompt_activates(term, "/tmp/tool input output"));
        assert!(!terminal_prompt_activates(
            term,
            "/Applications/MyTool.app/Contents/MacOS/tool input output"
        ));
        assert!(!terminal_prompt_activates(
            term,
            "/private/tmp/tool input output"
        ));
        assert!(!terminal_prompt_activates(
            term,
            "/nix/store/hash-tool/bin/tool input output"
        ));
        assert!(!terminal_prompt_activates(term, "./script run now"));
        // Lowercase prose present (mixed-case line) → activates.
        assert!(terminal_prompt_activates(term, "RUN the build now"));
    }

    #[test]
    fn deep_relative_path_not_under_bin_or_scripts_is_not_an_executable_invocation() {
        // is_executable_path_token's ./ (and ../) arm only treats a SINGLE-segment
        // relative path or one under bin/ or scripts/ as executable; any other
        // deep relative path (e.g. ./foo/bar) falls through. With a prose-only
        // tail (no flags, no path tokens) looks_like_shell_path_invocation is
        // false, so classification rests on the lowercase-prose rule — and the
        // line activates as an AI-agent prompt rather than being skipped as a
        // shell invocation.
        let term = "com.googlecode.iterm2";
        assert!(
            terminal_prompt_activates(term, "./foo/bar runs now"),
            "a deep relative path not under bin/ or scripts/ is not an executable path"
        );
        // Contrast: the SAME tail under bin/ IS an executable path → shell input,
        // so it does NOT activate. This isolates the executable-path branch as the
        // only differentiator.
        assert!(
            !terminal_prompt_activates(term, "./bin/tool runs now"),
            "the same line under bin/ is an executable-path invocation"
        );
    }

    #[test]
    fn terminal_path_arg_tail_marks_line_as_shell_input() {
        // looks_like_shell_path_invocation: when the FIRST token is a path token
        // but NOT an executable path (`./foo/bar` strips to `foo/bar`, which
        // contains `/` and is not under bin/ or scripts/ → not executable), the
        // function falls through to the tail heuristic
        // `rest.iter().any(|t| t.starts_with('-') || is_path_token(t))`. A later
        // path-token arg (`/tmp/out`) therefore activates that branch → the line
        // is classified as shell input, so it does NOT activate as a prompt.
        let term = "com.googlecode.iterm2";
        assert!(
            !terminal_prompt_activates(term, "./foo/bar input /tmp/out"),
            "a non-executable leading path with a later path-token arg is shell input"
        );
        // Contrast: identical leading path, but a prose-only tail (no flag, no
        // path token) leaves the tail heuristic false → not shell input, so
        // classification rests on the lowercase-prose rule and the line activates.
        assert!(
            terminal_prompt_activates(term, "./foo/bar input output"),
            "the same leading path with a prose-only tail is not shell input"
        );
    }

    #[test]
    fn applications_path_without_contents_macos_is_not_an_executable_invocation() {
        // is_executable_path_token's /Applications/ arm requires /Contents/MacOS/
        // — a /Applications/Foo.app/bar path WITHOUT it is not executable. With a
        // prose-only tail it falls through to the lowercase-prose rule and the
        // line activates as a prompt.
        let term = "com.googlecode.iterm2";
        assert!(
            terminal_prompt_activates(term, "/Applications/Foo.app/bar runs now"),
            "an /Applications path without /Contents/MacOS/ is not an executable path"
        );
        // Contrast: WITH /Contents/MacOS/ it is an executable path → shell input,
        // so it does NOT activate (mirrors the existing positive case).
        assert!(
            !terminal_prompt_activates(term, "/Applications/Foo.app/Contents/MacOS/bar runs now"),
            "the /Contents/MacOS/ form is an executable-path invocation"
        );
    }

    #[test]
    fn non_terminal_apps_are_never_restricted_by_the_prompt_heuristic() {
        assert!(terminal_prompt_activates("com.apple.TextEdit", "ls -la"));
        assert!(terminal_prompt_activates("com.apple.mail", "cd"));
    }

    #[test]
    fn accessibility_setup_detected_for_unreadable_browsers_and_setup_apps() {
        // Google Docs runs in Chrome (Works tier) — keyed on browser+unreadable.
        assert!(needs_accessibility_setup("com.google.Chrome", false));
        assert!(!needs_accessibility_setup("com.google.Chrome", true));
        // Arc/Dia (SetupNeeded) when unreadable.
        assert!(needs_accessibility_setup(
            "company.thebrowser.Browser",
            false
        ));
        // Plain native apps never need this setup.
        assert!(!needs_accessibility_setup("com.apple.TextEdit", false));
    }

    #[test]
    fn is_browser_recognizes_web_browsers() {
        assert!(is_browser("com.google.Chrome"));
        assert!(is_browser("org.mozilla.firefox"));
        assert!(!is_browser("com.apple.TextEdit"));
    }

    #[test]
    fn accessibility_setup_not_needed_for_unknown_non_browser() {
        // An Unknown-tier, non-browser bundle id exercises the false arm of the
        // is_browser gate in needs_accessibility_setup (line ~91): even with
        // unreadable field text, the result is false because it is neither a
        // browser nor a SetupNeeded app. Existing coverage uses TextEdit (a known
        // Works non-browser) at line ~721, not an Unknown-tier id.
        assert_eq!(
            compatibility_tier("com.example.SomethingNew"),
            CompatTier::Unknown
        );
        assert!(!is_browser("com.example.SomethingNew"));
        assert!(!needs_accessibility_setup(
            "com.example.SomethingNew",
            false
        ));
    }

    #[test]
    fn is_browser_false_for_bare_family_prefix_with_empty_suffix() {
        // The dot-bounded family check (is_browser, line ~83) requires a NON-empty
        // suffix after the family prefix: strip_prefix succeeds but yields "", so
        // the `!rest.is_empty()` guard rejects it. A bundle id that is exactly a
        // family prefix is therefore not a browser (it is neither an exact-id match
        // nor a real variant build).
        assert!(!is_browser("org.chromium."));
        assert!(!is_browser("com.brave.Browser."));
    }

    #[test]
    fn is_browser_false_for_empty_bundle_id() {
        // The empty bundle id is neither an exact match nor a non-empty family
        // suffix: strip_prefix(family) on "" yields None for every family, so the
        // any(...) is false (line ~47). Guards against an empty id slipping
        // through the dot-bounded family check.
        assert!(!is_browser(""));
    }

    #[test]
    fn is_browser_rejects_same_segment_lookalike_without_dot_boundary() {
        // The family match is dot-bounded (is_browser strips a trailing-'.'
        // family prefix), so a lookalike that shares the SAME bundle SEGMENT but
        // continues it without a dot boundary must NOT match. These are the
        // tightest false-positive cases: each begins with a real family/exact id
        // string but extends the final segment (no dot), so an unbounded
        // prefix/contains match would wrongly flag them as browsers and trigger
        // AX URL reads in a non-browser app.
        assert!(!is_browser("org.chromiumfoo")); // not "org.chromium." (no dot)
        assert!(!is_browser("com.brave.BrowserX")); // not exact "com.brave.Browser" nor "...Browser."
        assert!(!is_browser("com.google.Chromewide")); // not exact "com.google.Chrome" nor "...Chrome."
        assert!(!is_browser("com.microsoft.edgemacfoo")); // not exact "com.microsoft.edgemac" nor "...edgemac."
    }

    #[test]
    fn every_curated_browser_has_a_concrete_tier() {
        // Fail-open invariant the module comments say shipped broken once: a
        // bundle id that is_browser recognizes must NOT classify as Unknown.
        // Curated Chromium/Gecko browsers were previously listed in is_browser
        // yet fell through compatibility_tier to Unknown (fail-open), so they
        // emitted no compat guidance. Loop every curated browser id (the exact
        // set from is_browser, plus representative family-variant builds) and
        // assert is_browser ⇒ tier != Unknown.
        let curated = [
            // Exact-id curated set from is_browser.
            "com.apple.Safari",
            "com.google.Chrome",
            "com.microsoft.edgemac",
            "com.brave.Browser",
            "com.vivaldi.Vivaldi",
            "company.thebrowser.Browser",
            "company.thebrowser.dia",
            "org.mozilla.firefox",
            "org.mozilla.firefoxdeveloperedition",
            "org.mozilla.nightly",
            "app.zen-browser.zen",
        ];
        for b in curated {
            assert!(is_browser(b), "{b} should be a curated browser");
            assert_ne!(
                compatibility_tier(b),
                CompatTier::Unknown,
                "{b} is a curated browser but classifies as Unknown (fail-open regression)"
            );
        }
    }

    #[test]
    fn terminal_heuristic_does_not_apply_to_unlisted_terminals() {
        // terminal_prompt_activates gates on is_terminal, which whitelists ONLY
        // com.apple.Terminal and com.googlecode.iterm2 (lib.rs ~L130). Ghostty
        // is NOT whitelisted as a terminal (it is an Unsupported app, blocked
        // upstream by tier), so the prompt heuristic must treat it like any
        // non-terminal app: the early `!is_terminal` true-return fires and a
        // shell-command line that WOULD be rejected inside a real terminal still
        // returns true here. This pins that the heuristic never leaks to
        // terminals outside the explicit whitelist.
        assert!(
            terminal_prompt_activates("com.mitchellh.ghostty", "git commit -m wip"),
            "the terminal prompt heuristic must not gate an unlisted terminal (Ghostty)"
        );
    }
}
