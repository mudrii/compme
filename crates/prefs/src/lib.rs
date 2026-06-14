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
    /// Per-app typing-history collection override (tray "Input Collection in
    /// <app>"): `None` → inherit the default (allowed — the global opt-IN is
    /// the memory `StorageMode`); `Some(false)` → never record from this app.
    pub collect_inputs: Option<bool>,
    /// Per-app mid-line completions override (config
    /// `COMPME_MIDLINE_ON_APPS`/`COMPME_MIDLINE_OFF_APPS`): `None` →
    /// inherit the global `COMPME_MIDLINE`. The resolved value is applied LIVE
    /// to the engine's mid-line trigger gate via `Engine::set_allow_mid_word`,
    /// re-applied on every focus change and on the Labs-switch edge (run_loop)
    /// — see `mid_line_enabled`. (The original build-time `with_trigger_gates`
    /// bake is now just the startup default; the runtime setter overrides it.)
    pub mid_line: Option<bool>,
    /// Per-app autocorrect override (config
    /// `COMPME_AUTOCORRECT_ON_APPS`/`COMPME_AUTOCORRECT_OFF_APPS`): `None` →
    /// inherit the global `COMPME_AUTOCORRECT`.
    pub autocorrect: Option<bool>,
    /// Per-app thesaurus override (config
    /// `COMPME_THESAURUS_ON_APPS`/`COMPME_THESAURUS_OFF_APPS`): `None` →
    /// inherit the global `COMPME_THESAURUS`.
    pub thesaurus: Option<bool>,
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
    /// Per-app timed pauses (tray "Disable Completions in <app>" — the
    /// Cotypist-style submenu). `u64::MAX` = until relaunch (session-only,
    /// like the global snooze; the permanent arm is `excluded_apps`).
    pub app_snooze_until_ms: HashMap<String, u64>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            default_enabled: true,
            excluded_apps: HashSet::new(),
            excluded_domains: HashSet::new(),
            per_app: HashMap::new(),
            snooze_until_ms: None,
            app_snooze_until_ms: HashMap::new(),
        }
    }
}

/// Whether `host` is `rule` or a subdomain of it, matched on a dot boundary.
/// Both must already be lowercased. `google.com` matches `google.com` and
/// `www.google.com`, but never `notgoogle.com`/`evilgoogle.com` (no dot before
/// the rule) or `google.com.evil.com` (a different registrable domain that
/// merely contains the rule).
fn host_matches_domain_rule(host: &str, rule: &str) -> bool {
    host == rule
        || (host.len() > rule.len()
            && host.ends_with(rule)
            && host.as_bytes()[host.len() - rule.len() - 1] == b'.')
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
            if self.is_app_snoozed(app, now_ms) {
                return false;
            }
        }
        if let Some(domain) = domain {
            // Fold case at the lookup to match the lowercase-normalized inserts
            // (apply_override / build_prefs). Symmetric with the write seam so a
            // mixed-case host from any source still matches a stored rule (c136).
            // A rule covers its subdomains (dot-boundary suffix), so "google.com"
            // blocks www.google.com too (2026-06-14 live finding).
            let host = domain.to_ascii_lowercase();
            if self
                .excluded_domains
                .iter()
                .any(|rule| host_matches_domain_rule(&host, rule))
            {
                return false;
            }
        }
        app.and_then(|app| self.per_app.get(app))
            .and_then(|policy| policy.enabled)
            .unwrap_or(self.default_enabled)
    }

    /// Effective autocorrect state for `app`: the per-app override, else the
    /// caller's global default (globals stay in the app config — prefs only
    /// stores overrides).
    pub fn autocorrect_enabled(&self, app: Option<&str>, global_default: bool) -> bool {
        app.and_then(|app| self.per_app.get(app))
            .and_then(|policy| policy.autocorrect)
            .unwrap_or(global_default)
    }

    /// Effective thesaurus state for `app`: the per-app override, else the
    /// caller's global default.
    pub fn thesaurus_enabled(&self, app: Option<&str>, global_default: bool) -> bool {
        app.and_then(|app| self.per_app.get(app))
            .and_then(|policy| policy.thesaurus)
            .unwrap_or(global_default)
    }

    /// Effective mid-line state for `app` (same inherit pattern). The run loop
    /// applies this to the engine live via `Engine::set_allow_mid_word` on focus
    /// and Labs-switch edges — no longer build-baked. See `AppPolicy::mid_line`.
    pub fn mid_line_enabled(&self, app: Option<&str>, global_default: bool) -> bool {
        app.and_then(|app| self.per_app.get(app))
            .and_then(|policy| policy.mid_line)
            .unwrap_or(global_default)
    }

    /// Whether typing-history collection (previous-inputs context + encrypted
    /// memory) may record from this app. Default allowed; per-app opt-out.
    pub fn collection_allowed(&self, app: Option<&str>) -> bool {
        app.and_then(|app| self.per_app.get(app))
            .and_then(|policy| policy.collect_inputs)
            .unwrap_or(true)
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

    /// Pause suggestions in ONE app for `minutes` from `now_ms` (the tray
    /// per-app disable). `u64::MAX` minutes saturates to "until relaunch".
    pub fn snooze_app(&mut self, app: &str, now_ms: u64, minutes: u64) {
        self.app_snooze_until_ms.insert(
            app.to_string(),
            now_ms.saturating_add(minutes.saturating_mul(MS_PER_MINUTE)),
        );
    }

    /// Cancel a per-app snooze (re-enable before the deadline).
    pub fn clear_app_snooze(&mut self, app: &str) {
        self.app_snooze_until_ms.remove(app);
    }

    /// Whether `app` is per-app snoozed at `now_ms` (auto-resumes after).
    pub fn is_app_snoozed(&self, app: &str, now_ms: u64) -> bool {
        self.app_snooze_until_ms
            .get(app)
            .is_some_and(|until| now_ms < *until)
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
                // Lowercase at the write seam: the detection side lowercases
                // extracted hosts (domain_from_url), so a mixed-case rule
                // would NEVER match — permanently inert, and invisible to
                // the miss notice since detection itself succeeds
                // (audit-tests-c135).
                self.excluded_domains.insert(domain.to_ascii_lowercase());
            }
            (Scope::Domain(domain), Enable | Include) => {
                self.excluded_domains.remove(&domain.to_ascii_lowercase());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_app_feature_overrides_inherit_the_global_default() {
        let mut p = Prefs::default();
        // No override → the caller's global default decides.
        assert!(p.autocorrect_enabled(Some("com.apple.TextEdit"), true));
        assert!(!p.autocorrect_enabled(Some("com.apple.TextEdit"), false));
        assert!(p.mid_line_enabled(None, true));
        // Per-app off wins over a global on…
        p.per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .autocorrect = Some(false);
        assert!(!p.autocorrect_enabled(Some("com.apple.TextEdit"), true));
        // …and per-app on wins over a global off.
        p.per_app
            .entry("com.googlecode.iterm2".into())
            .or_default()
            .mid_line = Some(true);
        assert!(p.mid_line_enabled(Some("com.googlecode.iterm2"), false));
    }

    #[test]
    fn collection_allowed_defaults_on_and_honors_the_per_app_override() {
        let mut p = Prefs::default();
        // Default: collection allowed everywhere (opt-out model, the global
        // opt-IN is the memory StorageMode).
        assert!(p.collection_allowed(Some("com.apple.TextEdit")));
        assert!(p.collection_allowed(None));
        // Per-app off.
        p.per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .collect_inputs = Some(false);
        assert!(!p.collection_allowed(Some("com.apple.TextEdit")));
        // Other apps unaffected.
        assert!(p.collection_allowed(Some("com.apple.Safari")));
        // Explicit Some(true) re-allows.
        p.per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .collect_inputs = Some(true);
        assert!(p.collection_allowed(Some("com.apple.TextEdit")));
    }

    #[test]
    fn app_snooze_until_relaunch_saturates_and_clear_reenables() {
        let mut p = Prefs::default();
        // u64::MAX minutes saturates: effectively "until relaunch".
        p.snooze_app("com.googlecode.iterm2", 5_000, u64::MAX);
        assert!(!p.should_suggest(Some("com.googlecode.iterm2"), None, u64::MAX - 1));
        // Clearing re-enables before any deadline.
        p.clear_app_snooze("com.googlecode.iterm2");
        assert!(p.should_suggest(Some("com.googlecode.iterm2"), None, 5_000));
        // A hard exclude still dominates regardless of snooze state.
        p.excluded_apps.insert("com.googlecode.iterm2".into());
        assert!(!p.should_suggest(Some("com.googlecode.iterm2"), None, 5_000));
    }

    #[test]
    fn app_snooze_gates_only_that_app_and_auto_resumes() {
        let mut p = Prefs::default();
        p.snooze_app("com.apple.TextEdit", 1_000, 60);
        // The snoozed app is gated…
        assert!(!p.should_suggest(Some("com.apple.TextEdit"), None, 1_000));
        assert!(!p.should_suggest(Some("com.apple.TextEdit"), None, 1_000 + 59 * 60_000));
        // …other apps are not…
        assert!(p.should_suggest(Some("com.apple.Safari"), None, 1_000));
        // …and it auto-resumes at the deadline.
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1_000 + 60 * 60_000));
    }

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
        apply(&mut p, "compme://setOverride?app=com.foo.bar&enabled=false");
        assert!(!p.should_suggest(Some("com.foo.bar"), None, 0));
        apply(&mut p, "compme://setOverride?app=com.foo.bar&enabled=true");
        assert!(p.should_suggest(Some("com.foo.bar"), None, 0));
    }

    #[test]
    fn web_override_excludes_then_includes_an_app() {
        let mut p = Prefs::default();
        apply(&mut p, "compme://setOverride?app=com.foo.bar&excluded=true");
        assert!(p.excluded_apps.contains("com.foo.bar"));
        assert!(!p.should_suggest(Some("com.foo.bar"), None, 0));
        apply(
            &mut p,
            "compme://setOverride?app=com.foo.bar&excluded=false",
        );
        assert!(!p.excluded_apps.contains("com.foo.bar"));
        assert!(p.should_suggest(Some("com.foo.bar"), None, 0));
    }

    #[test]
    fn web_override_excludes_then_includes_a_domain() {
        let mut p = Prefs::default();
        apply(
            &mut p,
            "compme://setOverride?domain=docs.google.com&excluded=true",
        );
        assert!(!p.should_suggest(Some("com.apple.Safari"), Some("docs.google.com"), 0));
        // Domain enable un-excludes (no per-domain policy struct exists).
        apply(
            &mut p,
            "compme://setOverride?domain=docs.google.com&enabled=true",
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
        apply(&mut p, "compme://setOverride?app=com.foo.bar&enabled=true");
        assert!(!p.excluded_apps.contains("com.foo.bar"));
        assert_eq!(p.per_app["com.foo.bar"].enabled, Some(true));
        assert!(p.should_suggest(Some("com.foo.bar"), None, 0));
    }

    #[test]
    fn web_override_disable_sets_explicit_per_app_state() {
        let mut p = Prefs::default();
        apply(&mut p, "compme://setOverride?app=com.foo.bar&enabled=false");
        assert_eq!(p.per_app["com.foo.bar"].enabled, Some(false));
    }

    #[test]
    fn web_override_domain_disable_excludes_and_include_unexcludes() {
        let mut p = Prefs::default();
        // The other half of the domain matrix: enabled=false → exclude.
        apply(
            &mut p,
            "compme://setOverride?domain=evil.example&enabled=false",
        );
        assert!(p.excluded_domains.contains("evil.example"));
        // excluded=false → include (un-exclude).
        apply(
            &mut p,
            "compme://setOverride?domain=evil.example&excluded=false",
        );
        assert!(!p.excluded_domains.contains("evil.example"));
    }

    #[test]
    fn web_override_domain_rules_normalize_case_to_match_detected_hosts() {
        // The detection side lowercases (domain_from_url); a rule stored
        // with mixed case would NEVER match a detected host — permanently
        // inert, and invisible to the miss notice (detection itself works,
        // so the streak resets). Normalize at the single write seam.
        let mut p = Prefs::default();
        apply(
            &mut p,
            "compme://setOverride?domain=Docs.Google.com&enabled=false",
        );
        assert!(
            !p.should_suggest(None, Some("docs.google.com"), 0),
            "mixed-case rule must block the lowercased detected host"
        );
        // Removal normalizes the same way (different case than the insert).
        apply(
            &mut p,
            "compme://setOverride?domain=DOCS.google.COM&excluded=false",
        );
        assert!(p.should_suggest(None, Some("docs.google.com"), 0));
    }

    #[test]
    fn web_override_app_include_on_unexcluded_app_is_a_harmless_noop() {
        let mut p = Prefs::default();
        apply(
            &mut p,
            "compme://setOverride?app=com.foo.bar&excluded=false",
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
    fn global_snooze_saturates_instead_of_wrapping() {
        // The parallel arm of the pinned snooze_app saturation: u64::MAX
        // minutes must clamp to "forever", never wrap past now (a wrap
        // would UN-snooze — the failure direction that matters).
        let mut p = Prefs::default();
        p.snooze(5_000, u64::MAX);
        assert!(p.is_snoozed(u64::MAX - 1));
    }

    #[test]
    fn excluded_domain_blocks_suggestions_without_an_app_key() {
        // Browser context can yield a domain with no resolvable app key;
        // the domain exclusion must hold on its own.
        let mut p = Prefs::default();
        p.excluded_domains.insert("bank.example".into());
        assert!(!p.should_suggest(None, Some("bank.example"), 0));
        assert!(p.should_suggest(None, Some("other.example"), 0));
    }

    #[test]
    fn excluded_domain_lookup_is_case_insensitive() {
        // Inserts already lowercase (apply_override + build_prefs normalize),
        // but the LOOKUP must also fold case so a mixed-case host from any
        // future domain source still matches a stored rule. Same bug class as
        // c136, one seam upstream: insert normalized, lookup must too.
        let mut p = Prefs::default();
        p.excluded_domains.insert("bank.example.com".into());
        assert!(
            !p.should_suggest(Some("com.apple.Safari"), Some("Bank.Example.COM"), 1000),
            "a mixed-case host must still match a lowercase exclusion rule"
        );
    }

    #[test]
    fn excluded_domain_matches_subdomains_on_a_dot_boundary() {
        // A rule for the registrable domain must cover its subdomains: a user
        // who excludes "google.com" expects www.google.com / mail.google.com
        // too (2026-06-14 live finding — www.google.com slipped a "google.com"
        // rule). But a DIFFERENT domain that merely CONTAINS the rule must not
        // match (no over-blocking; the security-relevant direction).
        let mut p = Prefs::default();
        p.excluded_domains.insert("google.com".into());
        // Exact host + subdomains are blocked.
        assert!(!p.should_suggest(Some("com.apple.Safari"), Some("google.com"), 0));
        assert!(!p.should_suggest(Some("com.apple.Safari"), Some("www.google.com"), 0));
        assert!(!p.should_suggest(Some("com.apple.Safari"), Some("mail.google.com"), 0));
        // A different registrable domain is NOT blocked, even if it ends with
        // the rule text without a dot boundary, or contains it as a label.
        assert!(p.should_suggest(Some("com.apple.Safari"), Some("notgoogle.com"), 0));
        assert!(p.should_suggest(Some("com.apple.Safari"), Some("evilgoogle.com"), 0));
        assert!(p.should_suggest(Some("com.apple.Safari"), Some("google.com.evil.com"), 0));
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
