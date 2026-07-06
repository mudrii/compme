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
    /// Per-app standalone grammar/spell-fix override (config/UI): `None` →
    /// inherit the global `COMPME_GRAMMAR_FIX`.
    pub grammar_fix: Option<bool>,
}

/// The editable per-app fields exposed as row checkboxes in the Settings "Apps"
/// pane. The AppKit layer translates one checkbox toggle into one of these; the
/// resolution getters (`mid_line_enabled`, `autocorrect_enabled`, `tab_disabled`,
/// `should_suggest`) consume the result live. `collect_inputs`/`thesaurus` are
/// driven by the tray and config, not this pane, so they are intentionally absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppPolicyField {
    Enabled,
    TabDisabled,
    MidLine,
    Autocorrect,
    GrammarFix,
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

    /// Effective standalone grammar/spell-fix state for `app`: the per-app
    /// override, else the caller's global default.
    pub fn grammar_fix_enabled(&self, app: Option<&str>, global_default: bool) -> bool {
        app.and_then(|app| self.per_app.get(app))
            .and_then(|policy| policy.grammar_fix)
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

    /// Whether typing-history collection is explicitly allowed for this app.
    /// Default allowed; per-app opt-out. This is only the collection toggle:
    /// callers that record broad monitored typing must also apply suggestion
    /// privacy gates through [`Self::monitored_collection_allowed`].
    pub fn collection_allowed(&self, app: Option<&str>) -> bool {
        app.and_then(|app| self.per_app.get(app))
            .and_then(|policy| policy.collect_inputs)
            .unwrap_or(true)
    }

    /// Whether broad monitored typing may be durably recorded for this app/domain
    /// at `now_ms`. This intentionally combines the explicit collection toggle
    /// with suggestion privacy gates so snoozed, disabled, excluded app, and
    /// excluded-domain contexts do not keep collecting background typing.
    pub fn monitored_collection_allowed(
        &self,
        app: Option<&str>,
        domain: Option<&str>,
        now_ms: u64,
    ) -> bool {
        self.collection_allowed(app) && self.should_suggest(app, domain, now_ms)
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

    /// Whether `app` is per-app snoozed at `now_ms` (auto-resumes after).
    pub fn is_app_snoozed(&self, app: &str, now_ms: u64) -> bool {
        self.app_snooze_until_ms
            .get(app)
            .is_some_and(|until| now_ms < *until)
    }

    /// Whether a snooze is currently active at `now_ms` (auto-resumes after).
    pub fn is_snoozed(&self, now_ms: u64) -> bool {
        self.snooze_until_ms.is_some_and(|until| now_ms < until)
    }

    /// Fully re-enable `app`: turn the per-app `enabled` policy ON *and* clear any
    /// hard-block exclude, so re-enable is a true inverse of exclude/disable.
    /// `excluded_apps` short-circuits `should_suggest` (checked before the per-app
    /// `enabled`), so a bare `enabled = Some(true)` on an app that was excluded via
    /// tray "Disable ▸ Always" would silently no-op — the checkbox/shortcut would
    /// read On while suggestions stay dead with no in-app recovery. Shared by
    /// `set_app_policy_field` (Settings "Apps" checkbox + toggle-app shortcut) and
    /// `apply_override`'s deep-link Enable arm so the invariant can't diverge.
    fn fully_enable_app(&mut self, app: &str) {
        self.excluded_apps.remove(app);
        self.per_app.entry(app.to_string()).or_default().enabled = Some(true);
    }

    /// Clear domain hard-blocks that would still cover `domain` after a user
    /// explicitly enables/includes it. The current domain model is a deny-list,
    /// not a true allow/deny precedence tree, so include must remove both exact
    /// and suffix-covering rules to avoid a successful-looking no-op.
    fn clear_domain_exclusions_covering(&mut self, domain: &str) {
        let domain = domain.to_ascii_lowercase();
        self.excluded_domains.retain(|rule| {
            !host_matches_domain_rule(&domain, rule) && !host_matches_domain_rule(rule, &domain)
        });
    }

    /// Set one editable field of an app's per-app override (Settings "Apps" pane).
    ///
    /// Creates the entry if absent, like `apply_override` — same in-memory model
    /// as the existing per-app delete. The tri-state `Option` fields take
    /// `Some(on)`; revert a field to "inherit the default" by deleting the row.
    ///
    /// Re-enabling (`Enabled`, `on == true`) routes through `fully_enable_app`, so
    /// it also clears a prior hard-block exclude. Disable is intentionally NOT the
    /// mirror image: `enabled = Some(false)` blocks softly, and a tray-set
    /// `excluded_apps` entry is a deliberately stronger state left untouched here.
    pub fn set_app_policy_field(&mut self, app: &str, field: AppPolicyField, on: bool) {
        if let (AppPolicyField::Enabled, true) = (field, on) {
            self.fully_enable_app(app);
            return;
        }
        let policy = self.per_app.entry(app.to_string()).or_default();
        match field {
            AppPolicyField::Enabled => policy.enabled = Some(on),
            AppPolicyField::TabDisabled => policy.tab_disabled = on,
            AppPolicyField::MidLine => policy.mid_line = Some(on),
            AppPolicyField::Autocorrect => policy.autocorrect = Some(on),
            AppPolicyField::GrammarFix => policy.grammar_fix = Some(on),
        }
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
                self.fully_enable_app(app);
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
                self.clear_domain_exclusions_covering(domain);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single shared decision table for the two independent
    /// `host_matches_domain_rule` matchers (`prefs` here, `personalization` in its
    /// own crate). The two modules document that they "must never drift apart";
    /// this table is duplicated verbatim into `personalization::tests`
    /// (`DOMAIN_MATCHER_SHARED_CASES`) so an edit to EITHER matcher that changes a
    /// decision fails one crate's test. The expected column is the decision BOTH
    /// matchers currently make (verified empirically).
    ///
    /// NOTE: these matchers take ALREADY-lowercased input (callers fold case
    /// upstream — `should_suggest` / `per_domain_instruction`). So `GOOGLE.COM`
    /// does NOT match here; both agree on that raw-matcher contract.
    const DOMAIN_MATCHER_SHARED_CASES: &[(&str, &str, bool)] = &[
        ("www.google.com", "google.com", true),
        ("evilgoogle.com", "google.com", false),
        ("google.com.evil.com", "google.com", false),
        ("google.com", "google.com", true),
        ("GOOGLE.COM", "google.com", false),
        ("", "google.com", false),
        ("google.com", "", false),
    ];

    #[test]
    fn domain_matcher_agrees_on_shared_case_table() {
        for &(host, rule, expected) in DOMAIN_MATCHER_SHARED_CASES {
            assert_eq!(
                host_matches_domain_rule(host, rule),
                expected,
                "prefs matcher disagrees on ({host:?}, {rule:?})"
            );
        }
    }

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
        // thesaurus mirrors the same inherit + per-app override contract
        // (round-2 audit: it lacked a direct test, unlike its two siblings).
        assert!(p.thesaurus_enabled(Some("com.apple.Safari"), true)); // inherit global on
        assert!(!p.thesaurus_enabled(Some("com.apple.Safari"), false)); // inherit global off
        p.per_app
            .entry("com.apple.Safari".into())
            .or_default()
            .thesaurus = Some(false);
        assert!(!p.thesaurus_enabled(Some("com.apple.Safari"), true)); // per-app off wins
        assert!(p.thesaurus_enabled(Some("unconfigured.app"), true)); // no override → global
    }

    #[test]
    fn grammar_fix_enabled_inherits_global_default_without_app() {
        let p = Prefs::default();
        assert!(p.grammar_fix_enabled(None, true));
        assert!(!p.grammar_fix_enabled(None, false));
        assert!(p.grammar_fix_enabled(Some("com.apple.TextEdit"), true));
        assert!(!p.grammar_fix_enabled(Some("com.apple.TextEdit"), false));
    }

    #[test]
    fn grammar_fix_enabled_respects_per_app_override() {
        let mut p = Prefs::default();
        p.per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .grammar_fix = Some(false);
        p.per_app
            .entry("com.apple.Safari".into())
            .or_default()
            .grammar_fix = Some(true);

        assert!(!p.grammar_fix_enabled(Some("com.apple.TextEdit"), true));
        assert!(p.grammar_fix_enabled(Some("com.apple.Safari"), false));
        assert!(p.grammar_fix_enabled(Some("com.apple.Notes"), true));
    }

    #[test]
    fn set_app_policy_field_writes_grammar_fix() {
        let mut p = Prefs::default();
        p.set_app_policy_field("com.apple.TextEdit", AppPolicyField::GrammarFix, false);
        assert_eq!(
            p.per_app
                .get("com.apple.TextEdit")
                .and_then(|policy| policy.grammar_fix),
            Some(false)
        );
        assert!(!p.grammar_fix_enabled(Some("com.apple.TextEdit"), true));
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
    fn monitored_collection_stops_when_suggestions_are_gated_by_snooze_or_exclusion() {
        // Pins the recording/suggestion seam: `collection_allowed` is the
        // explicit per-app opt-out, while `monitored_collection_allowed` is the
        // privacy policy for broad AllMonitored writes. A snoozed or excluded app
        // can still have collection toggled on, but durable monitored recording
        // must stop together with suggestions.
        let mut p = Prefs::default();

        // Snoozed app: explicit collection toggle remains on, monitored writes off.
        p.snooze_app("com.apple.TextEdit", 1_000, 60);
        assert!(!p.should_suggest(Some("com.apple.TextEdit"), None, 1_000));
        assert!(p.collection_allowed(Some("com.apple.TextEdit")));
        assert!(!p.monitored_collection_allowed(Some("com.apple.TextEdit"), None, 1_000));

        // Excluded app: explicit collection toggle remains on, monitored writes off.
        p.excluded_apps.insert("com.apple.Safari".into());
        assert!(!p.should_suggest(Some("com.apple.Safari"), None, 1_000));
        assert!(p.collection_allowed(Some("com.apple.Safari")));
        assert!(!p.monitored_collection_allowed(Some("com.apple.Safari"), None, 1_000));

        // The per-app collect_inputs opt-out also stops monitored writes and does
        // NOT re-enable suggestions for the excluded app.
        p.per_app
            .entry("com.apple.Safari".into())
            .or_default()
            .collect_inputs = Some(false);
        assert!(!p.collection_allowed(Some("com.apple.Safari")));
        assert!(!p.monitored_collection_allowed(Some("com.apple.Safari"), None, 1_000));
        assert!(!p.should_suggest(Some("com.apple.Safari"), None, 1_000));
    }

    #[test]
    fn monitored_collection_allowed_for_enabled_unsnoozed_app_with_collection_on() {
        // Positive direction: an enabled, un-snoozed, non-excluded app whose
        // collection toggle is on may durably record monitored typing.
        let mut p = Prefs::default();
        p.per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .collect_inputs = Some(true);
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1_000));
        assert!(p.collection_allowed(Some("com.apple.TextEdit")));
        assert!(p.monitored_collection_allowed(Some("com.apple.TextEdit"), None, 1_000));

        // Make the Some(true) override load-bearing: flipping ONLY collect_inputs
        // to Some(false) — same enabled/unsnoozed/non-excluded context, so
        // should_suggest stays true — must flip monitored_collection_allowed to
        // false. This pins that the collection toggle (not the suggestion gate)
        // is the deciding term here.
        p.per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .collect_inputs = Some(false);
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1_000));
        assert!(!p.collection_allowed(Some("com.apple.TextEdit")));
        assert!(!p.monitored_collection_allowed(Some("com.apple.TextEdit"), None, 1_000));
    }

    #[test]
    fn active_snooze_overrides_a_per_app_force_enable() {
        // Order pin: should_suggest checks the global snooze BEFORE consulting the
        // per-app `enabled` override. So even an explicit per-app force-enable
        // (Some(true)) cannot punch through an active snooze window.
        let mut p = Prefs::default();
        p.per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .enabled = Some(true);
        // Without a snooze the force-enable suggests.
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1_000));
        // Activate the global snooze; the per-app force-enable must NOT override it.
        p.snooze(1_000, 60);
        assert!(!p.should_suggest(Some("com.apple.TextEdit"), None, 1_000));
        // Auto-resume past the deadline re-honors the force-enable.
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1_000 + 60 * 60_000));
    }

    #[test]
    fn app_snooze_and_excluded_domain_override_a_per_app_force_enable() {
        // Order pins for the two remaining gates ahead of the per-app `enabled`
        // consult (the global-snooze arm is pinned above): a PER-APP snooze and
        // an excluded DOMAIN each short-circuit should_suggest before the
        // force-enable (Some(true)) is read, so neither can be punched through.

        // (a) Per-app snooze beats the force-enable, and auto-resumes.
        let mut p = Prefs::default();
        p.per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .enabled = Some(true);
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1_000));
        p.snooze_app("com.apple.TextEdit", 1_000, 60);
        assert!(!p.should_suggest(Some("com.apple.TextEdit"), None, 1_000));
        // Past the deadline the force-enable governs again.
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1_000 + 60 * 60_000));

        // (b) Excluded domain beats the force-enable (fresh prefs, no snooze).
        let mut p = Prefs::default();
        p.per_app
            .entry("com.apple.Safari".into())
            .or_default()
            .enabled = Some(true);
        p.excluded_domains.insert("bank.example.com".into());
        assert!(!p.should_suggest(Some("com.apple.Safari"), Some("bank.example.com"), 1_000));
        // A non-excluded domain in the same context still suggests.
        assert!(p.should_suggest(Some("com.apple.Safari"), Some("docs.example.com"), 1_000));
    }

    #[test]
    fn per_app_collect_inputs_false_blocks_collection_but_not_suggestions() {
        // The collection toggle is independent of the suggestion gate: a per-app
        // collect_inputs = Some(false) stops durable monitored recording
        // (monitored_collection_allowed) while leaving should_suggest true, since
        // the app is enabled, un-snoozed, and non-excluded.
        let mut p = Prefs::default();
        p.per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .collect_inputs = Some(false);
        // Suggestions still fire…
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1_000));
        // …but the monitored-collection resolver returns false.
        assert!(!p.collection_allowed(Some("com.apple.TextEdit")));
        assert!(!p.monitored_collection_allowed(Some("com.apple.TextEdit"), None, 1_000));
    }

    #[test]
    fn monitored_collection_blocked_for_excluded_domain() {
        // The domain hard-block flows through should_suggest into
        // monitored_collection_allowed: an excluded focus domain must stop durable
        // monitored recording, while a non-excluded domain (same app, collection
        // on) still allows it. Pins the domain term distinct from the app gates.
        let mut p = Prefs::default();
        p.excluded_domains.insert("secret.example.com".into());

        // Excluded domain → monitored recording blocked even though the app side
        // is fully permissive (collection on by default, not snoozed/excluded).
        assert!(!p.should_suggest(None, Some("secret.example.com"), 1_000));
        assert!(!p.monitored_collection_allowed(None, Some("secret.example.com"), 1_000));

        // A different, non-excluded domain in the same context is still allowed.
        assert!(p.should_suggest(None, Some("public.example.com"), 1_000));
        assert!(p.monitored_collection_allowed(None, Some("public.example.com"), 1_000));
    }

    #[test]
    fn app_snooze_until_relaunch_saturates() {
        let mut p = Prefs::default();
        // u64::MAX minutes saturates: effectively "until relaunch".
        p.snooze_app("com.googlecode.iterm2", 5_000, u64::MAX);
        assert!(!p.should_suggest(Some("com.googlecode.iterm2"), None, u64::MAX - 1));
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
    fn monitored_collection_stops_for_per_app_snoozed_app_only() {
        // Per-app snooze gate on the monitored-collection seam: only the GLOBAL
        // snooze arm is otherwise tested. Snoozing app A must stop A's monitored
        // recording while leaving a different app B (same context, collection on
        // by default) recording — the per-app snooze must not bleed across apps.
        let mut p = Prefs::default();
        p.snooze_app("com.apple.TextEdit", 1_000, 60);
        // A is per-app snoozed → monitored collection blocked.
        assert!(p.collection_allowed(Some("com.apple.TextEdit")));
        assert!(!p.monitored_collection_allowed(Some("com.apple.TextEdit"), None, 1_000));
        // B is not snoozed → monitored collection still allowed.
        assert!(p.monitored_collection_allowed(Some("com.apple.Safari"), None, 1_000));
    }

    #[test]
    fn excluded_domain_does_not_match_rule_as_non_boundary_suffix() {
        // Mirrors the personalization sibling's pinned negative so the two
        // subdomain matchers can't drift: a rule "google.com" must NOT match
        // "google.com.evil.com" — a different registrable domain that merely
        // contains the rule text without a dot boundary before it.
        let mut p = Prefs::default();
        p.excluded_domains.insert("google.com".into());
        assert!(
            p.should_suggest(None, Some("google.com.evil.com"), 0),
            "a domain that merely contains the rule must not be excluded"
        );
    }

    #[test]
    fn default_allows_suggestions() {
        let p = Prefs::default();
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1000));
    }

    #[test]
    fn default_blocks_suggestions_when_disabled() {
        // Privacy DENY path: with default_enabled=false and NO per-app/domain
        // override, NO exclusion, and NO snooze, should_suggest must fall through
        // to the global default (false) for both an app-scoped and a
        // domain-scoped call. Pins the off-by-default global gate.
        let p = Prefs {
            default_enabled: false,
            ..Default::default()
        };
        assert!(!p.should_suggest(Some("com.apple.TextEdit"), None, 1000));
        assert!(!p.should_suggest(None, Some("docs.example.com"), 1000));
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
    fn web_override_domain_enable_clears_a_parent_rule_that_blocks_a_subdomain() {
        // Excluding a parent (`google.com`) blocks every subdomain via the
        // dot-boundary matcher. Enabling a concrete subdomain must clear the
        // covering parent rule too; otherwise the deep link succeeds but
        // suggestions remain blocked.
        let mut p = Prefs::default();
        apply(
            &mut p,
            "compme://setOverride?domain=google.com&excluded=true",
        );
        assert!(!p.should_suggest(Some("com.apple.Safari"), Some("docs.google.com"), 0));
        apply(
            &mut p,
            "compme://setOverride?domain=docs.google.com&enabled=true",
        );
        assert!(!p.excluded_domains.contains("google.com"));
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
    fn snooze_deadline_is_exclusive_so_zero_minutes_never_snoozes() {
        // is_snoozed is `now < until` (exclusive). snooze(now, 0) sets
        // until == now, so the very instant it is set is already past the
        // window — a zero-minute snooze is a no-op, and the deadline tick
        // itself auto-resumes. Pins the `<` boundary so a `<=` mutant (which
        // would keep suggestions gated at and one tick past the deadline) dies.
        let mut p = Prefs::default();
        p.snooze(1_000, 0);
        assert!(!p.is_snoozed(1_000), "until == now must not be snoozed");
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 1_000));
        // A real window is gated strictly before, and resumes exactly at the deadline.
        p.snooze(1_000, 1); // until = 61_000
        assert!(p.is_snoozed(60_999));
        assert!(!p.is_snoozed(61_000), "deadline tick auto-resumes");
    }

    #[test]
    fn app_snooze_deadline_tick_auto_resumes_for_that_app() {
        // is_app_snoozed mirrors the global `now < until` exclusivity: the app is
        // gated strictly before the deadline and suggestable at the exact deadline
        // tick. Pins the per-app `<` boundary (the global one is pinned above).
        let mut p = Prefs::default();
        p.snooze_app("com.apple.TextEdit", 1_000, 1); // until = 61_000
        assert!(p.is_app_snoozed("com.apple.TextEdit", 60_999));
        assert!(!p.is_app_snoozed("com.apple.TextEdit", 61_000));
        assert!(p.should_suggest(Some("com.apple.TextEdit"), None, 61_000));
    }

    #[test]
    fn feature_overrides_with_no_app_key_fall_through_to_the_global_default() {
        // app=None can never hit a per_app override, so every feature resolver must
        // return the caller's global default verbatim. Pins the None arm of the
        // shared inherit pattern for all four feature toggles.
        let p = Prefs::default();
        assert!(p.autocorrect_enabled(None, true));
        assert!(!p.autocorrect_enabled(None, false));
        assert!(p.thesaurus_enabled(None, true));
        assert!(!p.thesaurus_enabled(None, false));
        assert!(p.mid_line_enabled(None, true));
        assert!(!p.mid_line_enabled(None, false));
        assert!(p.collection_allowed(None));
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

    #[test]
    fn set_app_policy_field_writes_back_each_editable_field() {
        // Drives the Settings "Apps" pane row checkboxes: each toggle writes one
        // per-app override and the resolution getters reflect it live.
        let mut p = Prefs::default(); // default_enabled = true
        let app = "com.tinyspeck.slackmacgap";

        // mid_line: turn OFF for this app (global default would say on).
        p.set_app_policy_field(app, AppPolicyField::MidLine, false);
        assert!(!p.mid_line_enabled(Some(app), true));
        assert!(p.mid_line_enabled(Some("other"), true)); // other apps unaffected

        // autocorrect: turn ON for this app (global default would say off).
        p.set_app_policy_field(app, AppPolicyField::Autocorrect, true);
        assert!(p.autocorrect_enabled(Some(app), false));

        // tab_disabled: enable for this app.
        p.set_app_policy_field(app, AppPolicyField::TabDisabled, true);
        assert!(p.tab_disabled(Some(app)));

        // enabled: turn OFF for this app -> no suggestions there, others still on.
        p.set_app_policy_field(app, AppPolicyField::Enabled, false);
        assert!(!p.should_suggest(Some(app), None, 0));
        assert!(p.should_suggest(Some("other"), None, 0));

        // A single shared entry holds all four edits (not four separate rows).
        assert_eq!(p.per_app.len(), 1);
    }

    #[test]
    fn set_app_policy_field_tri_state_then_delete_reverts_to_inherit() {
        // The doc contract: the tri-state Option fields distinguish an explicit
        // Some(false) (force-off, overriding the default) from None (inherit), and
        // "revert a field to inherit" is done by DELETING the per-app row — there
        // is no setter path back to None. Pin both halves so a future change to the
        // entry/getter inherit math can't silently regress it.
        // `Prefs::default()` already has `default_enabled = true` (suggestions ON).
        let mut p = Prefs::default();
        assert!(p.default_enabled);
        let app = "com.tinyspeck.slackmacgap";

        // Explicit Some(false) overrides the permissive global default...
        p.set_app_policy_field(app, AppPolicyField::Enabled, false);
        assert_eq!(p.per_app[app].enabled, Some(false));
        assert!(
            !p.should_suggest(Some(app), None, 0),
            "Some(false) must force-off even though default_enabled is true"
        );

        // ...and deleting the whole per-app row reverts the field to inherit, so
        // the global default governs again (the only revert-to-None path).
        p.per_app.remove(app);
        assert!(
            p.should_suggest(Some(app), None, 0),
            "after the row is deleted the app inherits default_enabled (true)"
        );

        // The same revert path applies to the other tri-state knobs: an explicit
        // Some(true) override differs from inherit, and delete returns to the
        // caller-supplied global default rather than leaving a stale Some.
        p.set_app_policy_field(app, AppPolicyField::Autocorrect, true);
        assert!(
            p.autocorrect_enabled(Some(app), false),
            "Some(true) overrides a false global default"
        );
        p.per_app.remove(app);
        assert!(
            !p.autocorrect_enabled(Some(app), false),
            "after delete, autocorrect inherits the false global default"
        );
    }

    #[test]
    fn set_app_policy_field_enable_clears_a_tray_exclude() {
        // The Settings "Apps" checkbox / toggle-app shortcut trap: an app hard-
        // blocked via tray "Disable ▸ Always" lands in excluded_apps, which
        // short-circuits should_suggest BEFORE the per-app `enabled` is consulted.
        // Toggling the checkbox back On must be a true re-enable — clear the
        // exclude too — or the checkbox reads On forever while suggestions stay
        // dead. Mirrors the deep-link apply_override Enable arm.
        let mut p = Prefs::default();
        p.excluded_apps.insert("com.foo.bar".into());
        assert!(!p.should_suggest(Some("com.foo.bar"), None, 0));
        p.set_app_policy_field("com.foo.bar", AppPolicyField::Enabled, true);
        assert!(
            !p.excluded_apps.contains("com.foo.bar"),
            "re-enable must clear the hard-block exclude"
        );
        assert_eq!(p.per_app["com.foo.bar"].enabled, Some(true));
        assert!(p.should_suggest(Some("com.foo.bar"), None, 0));
    }

    #[test]
    fn fully_enable_app_clears_the_exclude_but_preserves_other_per_app_fields() {
        // fully_enable_app uses `entry().or_default()` specifically so re-enabling an
        // app keeps its OTHER per-app settings (tab_disabled, autocorrect, …) intact;
        // it only flips `enabled` on and drops the hard-block exclude. Every existing
        // enable-clears-exclude test starts from an exclude-only row, so a regression
        // that overwrote the whole entry with a fresh `AppPolicy { enabled: Some(true),
        // ..default }` would clobber the siblings undetected. Pin the preservation
        // through the shared helper via both public entry points.
        for use_deep_link in [false, true] {
            let mut p = Prefs::default();
            // Pre-existing per-app config the user set earlier, plus a tray exclude.
            let app = "com.microsoft.VSCode";
            p.per_app.insert(
                app.into(),
                AppPolicy {
                    enabled: Some(false),
                    tab_disabled: true,
                    autocorrect: Some(false),
                    ..Default::default()
                },
            );
            p.excluded_apps.insert(app.into());

            if use_deep_link {
                apply(
                    &mut p,
                    "compme://setOverride?app=com.microsoft.VSCode&enabled=true",
                );
            } else {
                p.set_app_policy_field(app, AppPolicyField::Enabled, true);
            }

            // The dual-clear happened…
            assert!(
                !p.excluded_apps.contains(app),
                "re-enable clears the exclude ({use_deep_link})"
            );
            assert_eq!(p.per_app[app].enabled, Some(true), "{use_deep_link}");
            // …and the co-existing fields survived (not reset to Default).
            assert!(
                p.per_app[app].tab_disabled,
                "tab_disabled must be preserved on re-enable ({use_deep_link})"
            );
            assert_eq!(
                p.per_app[app].autocorrect,
                Some(false),
                "autocorrect override must be preserved on re-enable ({use_deep_link})"
            );
        }
    }

    #[test]
    fn set_app_policy_field_disable_does_not_add_to_excluded_apps() {
        // Disable is intentionally NOT the mirror of enable: it sets the soft
        // per-app `enabled = Some(false)` gate and must leave excluded_apps
        // untouched (tray-exclude is a deliberately stronger, separate state).
        let mut p = Prefs::default();
        p.set_app_policy_field("com.foo.bar", AppPolicyField::Enabled, false);
        assert_eq!(p.per_app["com.foo.bar"].enabled, Some(false));
        assert!(
            p.excluded_apps.is_empty(),
            "soft disable must not promote the app into the hard-block set"
        );
        assert!(!p.should_suggest(Some("com.foo.bar"), None, 0));
    }
}
