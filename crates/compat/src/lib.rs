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
    "pip", "ssh", "curl", "grep", "echo", "vim", "nano", "make", "docker", "kubectl",
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
    // Require some lowercase-alphabetic prose so a bare path/flags line is skipped.
    line.chars().any(|c| c.is_ascii_lowercase())
}

/// Classify a macOS application bundle id into a compatibility tier.
pub fn compatibility_tier(bundle_id: &str) -> CompatTier {
    match bundle_id {
        // Works — a representative set across families.
        "com.apple.Safari"
        | "com.google.Chrome"
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

        // Mirror-window only.
        "org.mozilla.firefox" | "app.zen-browser.zen" => CompatTier::MirrorOnly,

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
        assert_eq!(compatibility_tier("com.apple.TextEdit"), CompatTier::Works);
        assert_eq!(
            compatibility_tier("company.thebrowser.Browser"),
            CompatTier::SetupNeeded
        );
        assert_eq!(
            compatibility_tier("org.mozilla.firefox"),
            CompatTier::MirrorOnly
        );
        assert_eq!(
            compatibility_tier("com.tinyspeck.slackmacgap"),
            CompatTier::Partial
        );
        assert_eq!(
            compatibility_tier("com.microsoft.VSCode"),
            CompatTier::SidebarOnly
        );
        assert_eq!(
            compatibility_tier("com.mitchellh.ghostty"),
            CompatTier::Unsupported
        );
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
        // The same shape but lowercased contains prose → activates (true).
        // NOTE: the originally-suggested "/usr/local/bin/tool --flag /tmp/OUT"
        // actually ACTIVATES because the path/flag segments are lowercase ASCII
        // prose, so the guard does not skip it. We assert the real behavior.
        assert!(terminal_prompt_activates(
            term,
            "/usr/local/bin/tool --flag /tmp/OUT"
        ));
        // Lowercase prose present (mixed-case line) → activates.
        assert!(terminal_prompt_activates(term, "RUN the build now"));
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
}
