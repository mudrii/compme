//! Suggestion-gating preferences (design spec §8 / §16): per-app and per-domain
//! enable/exclude, per-app Tab-key disable, and a global pause/snooze. Pure: a
//! policy struct plus deterministic resolution against a caller-supplied clock
//! (`now_ms`), so every transition is unit-testable. Persistence and the
//! settings UI live elsewhere (config layer / A3).

use std::collections::{HashMap, HashSet};

const MS_PER_MINUTE: u64 = 60 * 1000;

/// Per-application override. Absent fields fall back to the global defaults.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AppPolicy {
    /// `None` → inherit `Prefs::default_enabled`; `Some(false)` → off in this app.
    pub enabled: Option<bool>,
    /// Pass a literal Tab in this app instead of treating it as Word-accept
    /// (Cotypist's per-app Tab disable).
    pub tab_disabled: bool,
}

/// Suggestion-gating preferences. `excluded_apps`/`excluded_domains` hard-block
/// (e.g. Finder-like or sensitive sites); `per_app` carries finer overrides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prefs {
    pub default_enabled: bool,
    pub excluded_apps: HashSet<String>,
    pub excluded_domains: HashSet<String>,
    pub per_app: HashMap<String, AppPolicy>,
    /// When set and in the future, suggestions are paused until this instant.
    pub snooze_until_ms: Option<u64>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            default_enabled: true,
            excluded_apps: HashSet::new(),
            excluded_domains: HashSet::new(),
            per_app: HashMap::new(),
            snooze_until_ms: None,
        }
    }
}

impl Prefs {
    /// Whether suggestions may fire for a focus context now. False if snoozed, if
    /// the app or domain is excluded, or if the per-app/global policy is off.
    pub fn should_suggest(&self, app: Option<&str>, domain: Option<&str>, now_ms: u64) -> bool {
        if self.is_snoozed(now_ms) {
            return false;
        }
        if let Some(app) = app {
            if self.excluded_apps.contains(app) {
                return false;
            }
        }
        if let Some(domain) = domain {
            if self.excluded_domains.contains(domain) {
                return false;
            }
        }
        app.and_then(|app| self.per_app.get(app))
            .and_then(|policy| policy.enabled)
            .unwrap_or(self.default_enabled)
    }

    /// Whether Tab should pass through literally (not map to Word-accept) for the
    /// focused app.
    pub fn tab_disabled(&self, app: Option<&str>) -> bool {
        app.and_then(|app| self.per_app.get(app))
            .is_some_and(|policy| policy.tab_disabled)
    }

    /// Pause suggestions for `minutes` from `now_ms` ("disable for N minutes").
    pub fn snooze(&mut self, now_ms: u64, minutes: u64) {
        self.snooze_until_ms = Some(now_ms.saturating_add(minutes.saturating_mul(MS_PER_MINUTE)));
    }

    /// Cancel any active snooze.
    pub fn clear_snooze(&mut self) {
        self.snooze_until_ms = None;
    }

    /// Whether a snooze is currently active at `now_ms` (auto-resumes after).
    pub fn is_snoozed(&self, now_ms: u64) -> bool {
        self.snooze_until_ms.is_some_and(|until| now_ms < until)
    }

    /// Apply a validated web-driven-config override (design spec §8/§16). The
    /// command has already passed `webconfig`'s strict fail-closed parsing, so
    /// this only maps the reversible action onto the policy store:
    /// - App enable is a full "allow": it sets the per-app `enabled` policy on
    ///   AND clears any hard-block exclude, so `Enable` is a true inverse of
    ///   `Exclude`/`Disable` (otherwise a deep-link enable would silently no-op
    ///   on an excluded app, since `excluded_apps` short-circuits `should_suggest`).
    /// - App disable sets the per-app `enabled` policy off (soft).
    /// - App exclude/include adds to / removes from the hard-block app set.
    /// - Domain has no per-domain policy struct, so enable/include un-excludes
    ///   the domain and disable/exclude adds it to the domain hard-block set.
    pub fn apply_override(&mut self, command: &webconfig::OverrideCommand) {
        use webconfig::{OverrideAction::*, Scope};
        match (&command.scope, command.action) {
            (Scope::App(app), Enable) => {
                self.excluded_apps.remove(app);
                self.per_app.entry(app.clone()).or_default().enabled = Some(true);
            }
            (Scope::App(app), Disable) => {
                self.per_app.entry(app.clone()).or_default().enabled = Some(false);
            }
            (Scope::App(app), Exclude) => {
                self.excluded_apps.insert(app.clone());
            }
            (Scope::App(app), Include) => {
                self.excluded_apps.remove(app);
            }
            (Scope::Domain(domain), Disable | Exclude) => {
                self.excluded_domains.insert(domain.clone());
            }
            (Scope::Domain(domain), Enable | Include) => {
                self.excluded_domains.remove(domain);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_allows_suggestions() {
        let p = Prefs::default();
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1000));
    }

    /// Parse a deep link and apply it — the end-to-end web-config path (§16).
    fn apply(prefs: &mut Prefs, url: &str) {
        let cmd = webconfig::parse_deep_link(url).expect("valid deep link");
        prefs.apply_override(&cmd);
    }

    #[test]
    fn web_override_disables_then_re_enables_an_app() {
        let mut p = Prefs::default();
        apply(
            &mut p,
            "complete-me://setOverride?app=com.foo.bar&enabled=false",
        );
        assert!(!p.should_suggest(Some("com.foo.bar"), None, 0));
        apply(
            &mut p,
            "complete-me://setOverride?app=com.foo.bar&enabled=true",
        );
        assert!(p.should_suggest(Some("com.foo.bar"), None, 0));
    }

    #[test]
    fn web_override_excludes_then_includes_an_app() {
        let mut p = Prefs::default();
        apply(
            &mut p,
            "complete-me://setOverride?app=com.foo.bar&excluded=true",
        );
        assert!(p.excluded_apps.contains("com.foo.bar"));
        assert!(!p.should_suggest(Some("com.foo.bar"), None, 0));
        apply(
            &mut p,
            "complete-me://setOverride?app=com.foo.bar&excluded=false",
        );
        assert!(!p.excluded_apps.contains("com.foo.bar"));
        assert!(p.should_suggest(Some("com.foo.bar"), None, 0));
    }

    #[test]
    fn web_override_excludes_then_includes_a_domain() {
        let mut p = Prefs::default();
        apply(
            &mut p,
            "complete-me://setOverride?domain=docs.google.com&excluded=true",
        );
        assert!(!p.should_suggest(Some("com.apple.Safari"), Some("docs.google.com"), 0));
        // Domain enable un-excludes (no per-domain policy struct exists).
        apply(
            &mut p,
            "complete-me://setOverride?domain=docs.google.com&enabled=true",
        );
        assert!(p.should_suggest(Some("com.apple.Safari"), Some("docs.google.com"), 0));
    }

    #[test]
    fn web_override_app_enable_clears_a_prior_exclude() {
        // Enable must be a true allow: an excluded app becomes suggestable again
        // (excluded_apps short-circuits should_suggest, so a bare per-app enable
        // would otherwise silently no-op).
        let mut p = Prefs::default();
        p.excluded_apps.insert("com.foo.bar".into());
        apply(
            &mut p,
            "complete-me://setOverride?app=com.foo.bar&enabled=true",
        );
        assert!(!p.excluded_apps.contains("com.foo.bar"));
        assert_eq!(p.per_app["com.foo.bar"].enabled, Some(true));
        assert!(p.should_suggest(Some("com.foo.bar"), None, 0));
    }

    #[test]
    fn web_override_disable_sets_explicit_per_app_state() {
        let mut p = Prefs::default();
        apply(
            &mut p,
            "complete-me://setOverride?app=com.foo.bar&enabled=false",
        );
        assert_eq!(p.per_app["com.foo.bar"].enabled, Some(false));
    }

    #[test]
    fn web_override_domain_disable_excludes_and_include_unexcludes() {
        let mut p = Prefs::default();
        // The other half of the domain matrix: enabled=false → exclude.
        apply(
            &mut p,
            "complete-me://setOverride?domain=evil.example&enabled=false",
        );
        assert!(p.excluded_domains.contains("evil.example"));
        // excluded=false → include (un-exclude).
        apply(
            &mut p,
            "complete-me://setOverride?domain=evil.example&excluded=false",
        );
        assert!(!p.excluded_domains.contains("evil.example"));
    }

    #[test]
    fn web_override_app_include_on_unexcluded_app_is_a_harmless_noop() {
        let mut p = Prefs::default();
        apply(
            &mut p,
            "complete-me://setOverride?app=com.foo.bar&excluded=false",
        );
        assert!(p.excluded_apps.is_empty());
        assert!(p.should_suggest(Some("com.foo.bar"), None, 0));
    }

    #[test]
    fn excluded_app_blocks_suggestions() {
        let mut p = Prefs::default();
        p.excluded_apps.insert("com.apple.Finder".into());
        assert!(!p.should_suggest(Some("com.apple.Finder"), None, 1000));
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1000));
    }

    #[test]
    fn excluded_domain_blocks_suggestions() {
        let mut p = Prefs::default();
        p.excluded_domains.insert("bank.example.com".into());
        assert!(!p.should_suggest(Some("com.apple.Safari"), Some("bank.example.com"), 1000));
        assert!(p.should_suggest(Some("com.apple.Safari"), Some("docs.example.com"), 1000));
    }

    #[test]
    fn per_app_disable_overrides_global_default() {
        let mut p = Prefs::default();
        p.per_app.insert(
            "com.tinyspeck.slackmacgap".into(),
            AppPolicy {
                enabled: Some(false),
                ..Default::default()
            },
        );
        assert!(!p.should_suggest(Some("com.tinyspeck.slackmacgap"), None, 1000));
    }

    #[test]
    fn per_app_enable_overrides_global_disabled_default() {
        let mut p = Prefs {
            default_enabled: false,
            ..Default::default()
        };
        p.per_app.insert(
            "com.apple.TextEdit".into(),
            AppPolicy {
                enabled: Some(true),
                ..Default::default()
            },
        );
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1000));
        assert!(!p.should_suggest(Some("com.other.app"), None, 1000));
    }

    #[test]
    fn snooze_blocks_then_auto_resumes() {
        let mut p = Prefs::default();
        p.snooze(10_000, 5); // 5 minutes from t=10s
        assert!(p.is_snoozed(10_000));
        assert!(!p.should_suggest(Some("com.apple.TextEdit"), None, 100_000));
        // 5 min = 300_000 ms later → resumed.
        assert!(!p.is_snoozed(10_000 + 300_000));
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 10_000 + 300_000));
    }

    #[test]
    fn clear_snooze_resumes_immediately() {
        let mut p = Prefs::default();
        p.snooze(0, 10);
        p.clear_snooze();
        assert!(!p.is_snoozed(1000));
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1000));
    }

    #[test]
    fn tab_disabled_is_per_app() {
        let mut p = Prefs::default();
        p.per_app.insert(
            "com.microsoft.VSCode".into(),
            AppPolicy {
                tab_disabled: true,
                ..Default::default()
            },
        );
        assert!(p.tab_disabled(Some("com.microsoft.VSCode")));
        assert!(!p.tab_disabled(Some("com.apple.TextEdit")));
        assert!(!p.tab_disabled(None));
    }
}
