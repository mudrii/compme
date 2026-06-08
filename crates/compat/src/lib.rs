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
}
