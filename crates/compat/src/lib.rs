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
    /// app's own field capabilities and the user's preferences).
    pub fn allows_suggestions(self) -> bool {
        !matches!(self, CompatTier::Unsupported)
    }

    /// Whether suggestions in this app should be restricted to AI-chat/sidebar
    /// fields (not the main editor pane).
    pub fn sidebar_only(self) -> bool {
        matches!(self, CompatTier::SidebarOnly)
    }

    /// Where suggestions should be rendered for this tier. `MirrorOnly` apps
    /// (Firefox/Zen) cannot host an inline ghost, so they fall back to a mirror
    /// window; everything else renders inline (with the engine's own per-field
    /// popup fallback when caret geometry is missing).
    pub fn placement(self) -> Placement {
        match self {
            CompatTier::MirrorOnly => Placement::Mirror,
            _ => Placement::Inline,
        }
    }
}

/// How a suggestion is rendered for an app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Inline,
    Mirror,
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
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.len() < 3 {
        return false;
    }
    // A recognized shell command leader → treat as shell input, not a prompt.
    let first = words[0]
        .trim_start_matches(['$', '%', '>', '#'])
        .to_lowercase();
    if SHELL_LEADERS.contains(&first.as_str()) {
        return false;
    }
    // Require some lowercase-alphabetic prose so a bare path/flags line is skipped.
    line.chars().any(|c| c.is_ascii_lowercase())
}

/// Whether Google Docs needs Accessibility/Text-Metrics setup before inline
/// suggestions work (design spec §16 "Setup needed"): on a Google Docs domain
/// where the field text is not yet readable, onboarding should be surfaced.
pub fn google_docs_needs_setup(domain: Option<&str>, readable_text: bool) -> bool {
    domain.is_some_and(|d| d == "docs.google.com" || d.ends_with(".docs.google.com"))
        && !readable_text
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
    fn only_unsupported_blocks() {
        for app in [
            "com.apple.TextEdit",
            "company.thebrowser.Browser",
            "org.mozilla.firefox",
            "com.tinyspeck.slackmacgap",
            "com.microsoft.VSCode",
            "com.example.unknown",
        ] {
            assert!(
                compatibility_tier(app).allows_suggestions(),
                "{app} should allow suggestions"
            );
        }
    }

    #[test]
    fn sidebar_only_flag_is_set_only_for_code_editors() {
        assert!(compatibility_tier("com.microsoft.VSCode").sidebar_only());
        assert!(!compatibility_tier("com.apple.TextEdit").sidebar_only());
        assert!(!compatibility_tier("com.example.unknown").sidebar_only());
    }

    #[test]
    fn mirror_only_apps_get_mirror_placement() {
        assert_eq!(
            compatibility_tier("org.mozilla.firefox").placement(),
            Placement::Mirror
        );
        assert_eq!(
            compatibility_tier("com.apple.TextEdit").placement(),
            Placement::Inline
        );
    }

    #[test]
    fn terminal_activates_only_for_natural_language_prompts() {
        let term = "com.googlecode.iterm2";
        // Shell commands → no suggestions.
        assert!(!terminal_prompt_activates(term, "git commit -m wip"));
        assert!(!terminal_prompt_activates(term, "ls -la /tmp"));
        // Too few words → no.
        assert!(!terminal_prompt_activates(term, "hello there"));
        // Natural-language agent prompt → yes.
        assert!(terminal_prompt_activates(
            term,
            "please refactor the parser to"
        ));
        // Only the current line matters.
        assert!(terminal_prompt_activates(
            term,
            "git status\nplease summarize the diff for"
        ));
    }

    #[test]
    fn non_terminal_apps_are_never_restricted_by_the_prompt_heuristic() {
        assert!(terminal_prompt_activates("com.apple.TextEdit", "ls -la"));
        assert!(terminal_prompt_activates("com.apple.mail", "cd"));
    }

    #[test]
    fn google_docs_setup_detected_only_when_unreadable_on_docs_domain() {
        assert!(google_docs_needs_setup(Some("docs.google.com"), false));
        assert!(!google_docs_needs_setup(Some("docs.google.com"), true));
        assert!(!google_docs_needs_setup(Some("example.com"), false));
        assert!(!google_docs_needs_setup(None, false));
    }
}
