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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_allows_suggestions() {
        let p = Prefs::default();
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1000));
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
