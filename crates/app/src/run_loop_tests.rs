//! Unit tests for `run_loop`, split out of the module file
//! (2026-07-25 audit, F16) so the production surface is visible in `wc -l`.
//! Same module path as before (`mod tests` inside the parent module), so
//! `use super::*` and every test name are unchanged.

use super::*;

#[test]
#[ignore = "subprocess helper: deliberately terminates with A32's hard-exit code"]
fn inference_timeout_exit_guard_subprocess_helper() {
    let mut guard = InferenceTimeoutExitGuard::new();
    guard.arm();
}

#[test]
fn inference_timeout_exit_guard_uses_no_cleanup_termination() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "run_loop::tests::inference_timeout_exit_guard_subprocess_helper",
        ])
        .status()
        .expect("launch hard-exit helper");

    assert_eq!(
        status.code(),
        Some(INFERENCE_SHUTDOWN_TIMEOUT_EXIT_CODE),
        "armed last-drop guard must bypass ordinary process exit"
    );
}

#[test]
#[ignore = "subprocess helper: watchdog deliberately terminates with A32's hard-exit code"]
fn inference_timeout_watchdog_subprocess_helper() {
    arm_inference_shutdown_watchdog();
    std::thread::sleep(Duration::from_secs(2));
    panic!("shutdown watchdog failed to terminate the helper");
}

#[test]
fn inference_timeout_watchdog_bounds_stuck_cleanup() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "run_loop::tests::inference_timeout_watchdog_subprocess_helper",
        ])
        .status()
        .expect("launch shutdown-watchdog helper");

    assert_eq!(
        status.code(),
        Some(INFERENCE_SHUTDOWN_TIMEOUT_EXIT_CODE),
        "watchdog must hard-exit when later cleanup remains stuck"
    );
}
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

static SHORTCUT_BINDINGS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct ShortcutBindingsGuard {
    previous: crate::shell::ShortcutBindings,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl ShortcutBindingsGuard {
    fn reset() -> Self {
        let lock = SHORTCUT_BINDINGS_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = crate::shell::effective_shortcut_bindings();
        crate::shell::set_shortcut_bindings_from_config(None, None, None, None);
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for ShortcutBindingsGuard {
    fn drop(&mut self) {
        crate::shell::set_shortcut_bindings(self.previous);
    }
}

/// Build a lookup closure from a list of key/value pairs.
fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |key: &str| map.get(key).cloned()
}

#[test]
fn personalization_edit_rejoins_each_knob_onto_the_profile_and_persist_pair() {
    // Each PersonalizationEdit variant the Settings pane records must land on
    // the right profile field AND return the matching (env_key, value) so the
    // run loop persists what it applied. Covers the three steering knobs.
    use crate::shell::PersonalizationEdit as E;
    let mut profile = PersonalizationProfile::default();

    let (key, val) =
        apply_personalization_edit(&mut profile, E::GlobalInstructions("Be terse.".into()));
    assert_eq!(profile.global_instructions, "Be terse.");
    assert_eq!((key, val.as_str()), ("COMPME_INSTRUCTIONS", "Be terse."));

    let (key, val) = apply_personalization_edit(&mut profile, E::SenderName("Ada".into()));
    assert_eq!(profile.sender.name, "Ada");
    assert_eq!((key, val.as_str()), ("COMPME_SENDER_NAME", "Ada"));

    let (key, val) = apply_personalization_edit(&mut profile, E::SenderEmail("ada@x.io".into()));
    assert_eq!(profile.sender.email, "ada@x.io");
    assert_eq!((key, val.as_str()), ("COMPME_SENDER_EMAIL", "ada@x.io"));

    // Strength stop addresses STOPS by index and round-trips through
    // from_stop; the persisted value is the stop number, matching how
    // build_personalization parses COMPME_STRENGTH.
    let (key, val) = apply_personalization_edit(&mut profile, E::StrengthStop(5));
    assert_eq!(profile.strength, Strength::from_stop(5));
    assert_eq!((key, val.as_str()), ("COMPME_STRENGTH", "5"));
    assert_eq!(personalization_strength_index(profile.strength), 5);

    // Out-of-range stop is total (clamped via from_stop), never panics.
    let (_key, val) = apply_personalization_edit(&mut profile, E::StrengthStop(9_999));
    assert_eq!(profile.strength, Strength::from_stop(255));
    assert_eq!(val, "255");

    // Titles cover every stop in order, so the popup index always addresses
    // a real Strength.
    assert_eq!(
        personalization_strength_titles().len(),
        Strength::STOPS.len()
    );
}

#[test]
fn live_personalization_edit_updates_worker_profile_and_persists_value() {
    use crate::shell::PersonalizationEdit as E;
    let mut profile = PersonalizationProfile::default();
    let applied_profile = RefCell::new(None);
    let persisted = RefCell::new(None);

    let (key, value, persist_result) = apply_live_personalization_edit(
        &mut profile,
        E::GlobalInstructions("Use short direct completions.".into()),
        |profile| *applied_profile.borrow_mut() = Some(profile),
        |key, value| {
            *persisted.borrow_mut() = Some((key, value.to_string()));
            Ok(())
        },
    );

    assert!(persist_result.is_ok());
    assert_eq!(key, "COMPME_INSTRUCTIONS");
    assert_eq!(value, "Use short direct completions.");
    assert_eq!(profile.global_instructions, "Use short direct completions.");
    assert_eq!(
        applied_profile
            .borrow()
            .as_ref()
            .unwrap()
            .global_instructions,
        "Use short direct completions."
    );
    assert_eq!(
        persisted.into_inner(),
        Some((
            "COMPME_INSTRUCTIONS",
            "Use short direct completions.".to_string()
        ))
    );
}

fn test_screen_context() -> ScreenContext {
    ScreenContext {
        field: host_field("screen-field"),
        generation: 1,
        snapshot: 2,
        text: "screen text".into(),
    }
}

#[test]
fn context_toggle_off_clears_clipboard_screen_worker_and_wait() {
    let clipboard_cell = Mutex::new(Some("clipboard text".to_string()));
    apply_clipboard_context_edge(false, &clipboard_cell);
    assert_eq!(*clipboard_cell.lock().unwrap(), None);

    let mut config_screen_context = false;
    let ui_flag = AtomicBool::new(false);
    let screen_cell = Mutex::new(Some(test_screen_context()));
    let mut screen_ocr = Some("worker");
    let wait_ms = RefCell::new(Vec::new());

    let edge = apply_screen_context_edge(
        false,
        ScreenContextToggleState {
            config_screen_context: &mut config_screen_context,
            ui_flag: &ui_flag,
            screen_cell: &screen_cell,
            screen_ocr: &mut screen_ocr,
        },
        |ms| wait_ms.borrow_mut().push(ms),
        || true,
        || Ok("new-worker"),
    );

    assert_eq!(edge, ScreenContextEdge::Disabled);
    assert_eq!(*screen_cell.lock().unwrap(), None);
    assert_eq!(screen_ocr, None);
    assert_eq!(wait_ms.into_inner(), vec![0]);
}

#[test]
fn screen_context_enable_reverts_false_when_permission_denied_or_spawn_fails() {
    let denied_flag = AtomicBool::new(true);
    let denied_cell = Mutex::new(None);
    let mut denied_context = true;
    let mut denied_ocr: Option<&str> = Some("old-worker");
    let denied_wait = RefCell::new(Vec::new());

    let denied = apply_screen_context_edge(
        true,
        ScreenContextToggleState {
            config_screen_context: &mut denied_context,
            ui_flag: &denied_flag,
            screen_cell: &denied_cell,
            screen_ocr: &mut denied_ocr,
        },
        |ms| denied_wait.borrow_mut().push(ms),
        || false,
        || Ok("new-worker"),
    );
    let denied_persist_value = denied_context;

    assert_eq!(denied, ScreenContextEdge::RevertedDenied);
    assert!(!denied_context);
    assert!(!denied_persist_value);
    assert!(!denied_flag.load(Ordering::Relaxed));
    assert_eq!(denied_ocr, None);
    assert_eq!(denied_wait.into_inner(), vec![0]);

    let failed_flag = AtomicBool::new(true);
    let failed_cell = Mutex::new(None);
    let mut failed_context = true;
    let mut failed_ocr: Option<&str> = Some("old-worker");
    let failed_wait = RefCell::new(Vec::new());

    let failed = apply_screen_context_edge(
        true,
        ScreenContextToggleState {
            config_screen_context: &mut failed_context,
            ui_flag: &failed_flag,
            screen_cell: &failed_cell,
            screen_ocr: &mut failed_ocr,
        },
        |ms| failed_wait.borrow_mut().push(ms),
        || true,
        || Err("spawn failed".to_string()),
    );
    let failed_persist_value = failed_context;

    assert_eq!(failed, ScreenContextEdge::RevertedSpawnFailed);
    assert!(!failed_context);
    assert!(!failed_persist_value);
    assert!(!failed_flag.load(Ordering::Relaxed));
    assert_eq!(failed_ocr, None);
    assert_eq!(failed_wait.into_inner(), vec![0]);
}

#[test]
fn screen_context_enable_starts_worker_and_sets_wait() {
    let ui_flag = AtomicBool::new(true);
    let screen_cell = Mutex::new(None);
    let mut config_screen_context = true;
    let mut screen_ocr: Option<&str> = None;
    let wait_ms = RefCell::new(Vec::new());

    let edge = apply_screen_context_edge(
        true,
        ScreenContextToggleState {
            config_screen_context: &mut config_screen_context,
            ui_flag: &ui_flag,
            screen_cell: &screen_cell,
            screen_ocr: &mut screen_ocr,
        },
        |ms| wait_ms.borrow_mut().push(ms),
        || true,
        || Ok("new-worker"),
    );

    assert_eq!(edge, ScreenContextEdge::Enabled);
    assert!(config_screen_context);
    assert!(ui_flag.load(Ordering::Relaxed));
    assert_eq!(*screen_cell.lock().unwrap(), None);
    assert_eq!(screen_ocr, Some("new-worker"));
    assert_eq!(wait_ms.into_inner(), vec![SCREEN_CONTEXT_WAIT_MS]);
}

#[test]
fn request_log_does_not_emit_prompt_text() {
    let request = CompletionRequest {
        generation: 42,
        field: field_with_app("com.apple.TextEdit"),
        domain: None,
        snapshot: 42,
        prompt: "secret prompt with ada@example.com".into(),
        max_tokens: 24,
        kind: RequestKind::Completion,
    };
    let prefs = Prefs::default();
    let line = request_log_line(
        &request,
        Some("com.apple.TextEdit"),
        None,
        &prefs,
        1_000,
        Some("ada@example.com"),
        false,
    );
    assert!(
        line.contains("request gen=42 prompt_chars=34 app=com.apple.TextEdit"),
        "request logs should expose only prompt length and gate metadata: {line}"
    );
    assert!(line.contains("app_allows=true"));
    assert!(line.contains("terminal_ok=true"));
    assert!(line.contains("domain_ready=true"));
    assert!(line.contains("prefs_ok=true"));
    assert!(line.contains("prompt_marker=true"));
    assert!(!line.contains("secret"));
    assert!(!line.contains("ada@example.com"));
    assert!(!line.contains("prompt with"));
}

#[test]
fn web_override_persisted_keys_round_trip_through_build_prefs() {
    let mut prefs = Prefs::default();
    for url in [
        "compme://setOverride?domain=Docs.Google.com&excluded=true",
        "compme://setOverride?app=com.foo.disabled&enabled=false",
        "compme://setOverride?app=com.foo.enabled&enabled=true",
        "compme://setOverride?app=com.foo.excluded&excluded=true",
    ] {
        handle_deep_link(url, None, &mut prefs, |_| true).expect("valid deep link applies");
    }

    let dir = std::env::temp_dir().join(format!(
        "compme-web-override-persist-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("config.env");
    persist_web_override_prefs(&path, &prefs);

    let map = config::load_file_map(&path).expect("reload persisted prefs");
    assert_eq!(
        map.get("COMPME_EXCLUDED_DOMAINS"),
        Some(&"docs.google.com".to_string())
    );
    assert_eq!(
        map.get("COMPME_DISABLED_APPS"),
        Some(&"com.foo.disabled".to_string())
    );
    assert_eq!(
        map.get("COMPME_ENABLED_APPS"),
        Some(&"com.foo.enabled".to_string())
    );
    assert_eq!(
        map.get("COMPME_EXCLUDED_APPS"),
        Some(&"com.foo.excluded".to_string())
    );

    let reloaded = build_prefs(&|key| map.get(key).cloned());
    assert_eq!(reloaded.per_app["com.foo.disabled"].enabled, Some(false));
    assert_eq!(reloaded.per_app["com.foo.enabled"].enabled, Some(true));
    assert!(!reloaded.should_suggest(None, Some("docs.google.com"), 0));
    assert!(!reloaded.should_suggest(Some("com.foo.disabled"), None, 0));
    assert!(reloaded.should_suggest(Some("com.foo.enabled"), None, 0));
    assert!(!reloaded.should_suggest(Some("com.foo.excluded"), None, 0));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn web_override_persist_round_trips_all_five_overrides_on_one_app() {
    // A single app carrying ALL FIVE editable per-app overrides at once —
    // enabled + tab_disabled + mid_line + autocorrect + grammar_fix — must survive
    // persist_web_override_prefs -> build_prefs with every field intact and
    // INDEPENDENT. The existing feature-only round-trip pins mid_line/
    // autocorrect/grammar_fix/tab_disabled but deliberately keeps `enabled == None` to
    // prove independence; no test rounds the `enabled` key alongside the
    // four feature keys on the SAME app. Because each field serializes to a
    // *separate* comma-list key, a regression where the enabled write (or its
    // reload) clobbered or dropped a co-resident feature override — or vice
    // versa — would pass every existing round-trip yet corrupt an app that a
    // user configured fully in the Apps pane.
    use prefs::AppPolicyField::*;
    let app = "com.foo.allfive";
    let mut prefs = Prefs::default();
    prefs.set_app_policy_field(app, Enabled, false); // -> COMPME_DISABLED_APPS
    prefs.set_app_policy_field(app, TabDisabled, true); // -> COMPME_TAB_DISABLED_APPS
    prefs.set_app_policy_field(app, MidLine, true); // -> COMPME_MIDLINE_ON_APPS
    prefs.set_app_policy_field(app, Autocorrect, false); // -> COMPME_AUTOCORRECT_OFF_APPS
    prefs.set_app_policy_field(app, GrammarFix, true); // -> COMPME_GRAMMAR_FIX_ON_APPS

    let dir = std::env::temp_dir().join(format!(
        "compme-web-override-allfour-persist-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("config.env");
    persist_web_override_prefs(&path, &prefs);

    let map = config::load_file_map(&path).expect("reload persisted prefs");
    // Each field lands in its own key for this one app; the disabled/enabled
    // split is pinned so a polarity flip is caught.
    assert_eq!(map.get("COMPME_DISABLED_APPS"), Some(&app.to_string()));
    assert_eq!(map.get("COMPME_ENABLED_APPS"), None);
    assert_eq!(map.get("COMPME_TAB_DISABLED_APPS"), Some(&app.to_string()));
    assert_eq!(map.get("COMPME_MIDLINE_ON_APPS"), Some(&app.to_string()));
    assert_eq!(
        map.get("COMPME_AUTOCORRECT_OFF_APPS"),
        Some(&app.to_string())
    );
    assert_eq!(
        map.get("COMPME_GRAMMAR_FIX_ON_APPS"),
        Some(&app.to_string())
    );

    let reloaded = build_prefs(&|key| map.get(key).cloned());
    let p = &reloaded.per_app[app];
    assert_eq!(p.enabled, Some(false), "enabled override lost");
    assert!(p.tab_disabled, "tab_disabled override lost");
    assert_eq!(p.mid_line, Some(true), "mid_line override lost");
    assert_eq!(p.autocorrect, Some(false), "autocorrect override lost");
    assert_eq!(p.grammar_fix, Some(true), "grammar_fix override lost");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn web_override_persist_round_trips_midline_autocorrect_tab_disabled_per_app() {
    // r2 HIGH-2: the Apps-pane feature overrides (mid_line / autocorrect /
    // tab_disabled) must survive persist_web_override_prefs -> build_prefs
    // EXACTLY and INDEPENDENTLY of the enabled / excluded keys. These three
    // fields are set via set_app_policy_field (not deep links), so the
    // existing enabled/excluded round-trip test never exercised them — the
    // _ON/_OFF/_TAB_DISABLED comma-list serialization was untested end to end.
    let mut prefs = Prefs::default();
    // An app carrying ONLY feature overrides (no enabled/excluded override),
    // so we prove the feature keys round-trip on their own.
    prefs.set_app_policy_field("com.foo.feat", prefs::AppPolicyField::MidLine, true);
    prefs.set_app_policy_field("com.foo.feat", prefs::AppPolicyField::Autocorrect, false);
    prefs.set_app_policy_field("com.foo.feat", prefs::AppPolicyField::GrammarFix, true);
    prefs.set_app_policy_field("com.foo.feat", prefs::AppPolicyField::TabDisabled, true);
    // A second app exercising the opposite mid_line/autocorrect polarity so
    // the _ON vs _OFF list split is pinned, not just "non-default present".
    prefs.set_app_policy_field("com.bar.feat", prefs::AppPolicyField::MidLine, false);
    prefs.set_app_policy_field("com.bar.feat", prefs::AppPolicyField::Autocorrect, true);
    prefs.set_app_policy_field("com.bar.feat", prefs::AppPolicyField::GrammarFix, false);

    let dir = std::env::temp_dir().join(format!(
        "compme-web-override-feature-persist-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("config.env");
    persist_web_override_prefs(&path, &prefs);

    let map = config::load_file_map(&path).expect("reload persisted prefs");
    // The polarity-split comma lists are written verbatim.
    assert_eq!(
        map.get("COMPME_MIDLINE_ON_APPS"),
        Some(&"com.foo.feat".to_string())
    );
    assert_eq!(
        map.get("COMPME_MIDLINE_OFF_APPS"),
        Some(&"com.bar.feat".to_string())
    );
    assert_eq!(
        map.get("COMPME_AUTOCORRECT_ON_APPS"),
        Some(&"com.bar.feat".to_string())
    );
    assert_eq!(
        map.get("COMPME_AUTOCORRECT_OFF_APPS"),
        Some(&"com.foo.feat".to_string())
    );
    assert_eq!(
        map.get("COMPME_GRAMMAR_FIX_ON_APPS"),
        Some(&"com.foo.feat".to_string())
    );
    assert_eq!(
        map.get("COMPME_GRAMMAR_FIX_OFF_APPS"),
        Some(&"com.bar.feat".to_string())
    );
    assert_eq!(
        map.get("COMPME_TAB_DISABLED_APPS"),
        Some(&"com.foo.feat".to_string())
    );

    let reloaded = build_prefs(&|key| map.get(key).cloned());
    let foo = &reloaded.per_app["com.foo.feat"];
    assert_eq!(foo.mid_line, Some(true));
    assert_eq!(foo.autocorrect, Some(false));
    assert_eq!(foo.grammar_fix, Some(true));
    assert!(foo.tab_disabled);
    // Independence: the feature-only app never gained an enabled override.
    assert_eq!(foo.enabled, None);
    let bar = &reloaded.per_app["com.bar.feat"];
    assert_eq!(bar.mid_line, Some(false));
    assert_eq!(bar.autocorrect, Some(true));
    assert_eq!(bar.grammar_fix, Some(false));
    assert!(!bar.tab_disabled);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn log_squelch_logs_changes_and_resumes_after_reset() {
    let mut squelch = LogSquelch::default();
    // First occurrence logs; identical repeats are squelched.
    assert!(squelch.should_log("UnsupportedField"));
    assert!(!squelch.should_log("UnsupportedField"));
    assert!(!squelch.should_log("UnsupportedField"));
    // A DIFFERENT error logs (state changed).
    assert!(squelch.should_log("StaleField"));
    assert!(!squelch.should_log("StaleField"));
    // A successful read resets: the next error is a new episode.
    squelch.reset();
    assert!(squelch.should_log("StaleField"));
}

#[test]
fn statistics_pane_composition_is_exactly_stats_rows_deep() {
    // The window builds STATS_ROWS labels and zips them with these
    // lines; a composition that stopped matching would silently leave a
    // stale label (review-c103). Pin against the REAL const — a literal
    // here goes stale silently when the pane grows a row.
    let mut lines = stats_pane_lines(&[stats::DayBucket::default()]);
    lines.push(lifetime_line(&stats::PersistedStats::default()));
    assert_eq!(lines.len(), crate::shell::STATS_ROWS);
}

#[test]
fn stats_flush_due_boundaries() {
    assert!(stats_flush_due(None, 10_000), "never flushed: due now");
    let last = Some(100_000);
    assert!(
        !stats_flush_due(last, 100_000 + STATS_FLUSH_INTERVAL_MS - 1),
        "inside the interval"
    );
    assert!(
        stats_flush_due(last, 100_000 + STATS_FLUSH_INTERVAL_MS),
        "interval elapsed"
    );
    assert!(
        !stats_flush_due(last, 99_999),
        "clock-skew saturates, not due"
    );
}

fn flush_temp_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("cm-flush-{tag}-{}", std::process::id()))
}

#[test]
fn lifetime_flush_is_idempotent() {
    // baseline + grow-only session totals, overwritten in place: the
    // SAME state must produce byte-identical files no matter how many
    // times it flushes (periodic + shutdown share this writer).
    let dir = flush_temp_path("idem");
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("stats.env");
    let base = stats::PersistedStats {
        shown: 10,
        accepted: 4,
        dismissed: 2,
        superseded: 1,
        words: 9,
    };
    let mut usage = stats::Stats::new();
    usage.record(1_000, stats::Outcome::Shown);
    usage.record(1_000, stats::Outcome::Accepted { words: 2 });
    let session = usage.session_totals();

    persist_lifetime_stats(Some(&path), &base, session).expect("first flush");
    let first = std::fs::read(&path).expect("file written");
    persist_lifetime_stats(Some(&path), &base, session).expect("second flush");
    let second = std::fs::read(&path).expect("file rewritten");
    assert_eq!(first, second, "same state → identical bytes");
    assert_eq!(
        String::from_utf8(first).unwrap(),
        stats::render_stats_file(&base.merged(session.counts, session.words))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lifetime_flush_then_final_flush_never_double_counts() {
    // The shutdown flush after N periodic flushes must yield EXACTLY
    // base + final-session-totals — a re-read of the file (the old
    // shutdown shape) would re-add the session every time.
    let dir = flush_temp_path("nodouble");
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("stats.env");
    let base = stats::PersistedStats {
        shown: 100,
        accepted: 50,
        dismissed: 10,
        superseded: 5,
        words: 200,
    };
    let mut usage = stats::Stats::new();
    usage.record(1_000, stats::Outcome::Accepted { words: 3 });
    persist_lifetime_stats(Some(&path), &base, usage.session_totals()).expect("periodic");
    // Session grows, then the final (shutdown) flush.
    usage.record(2_000, stats::Outcome::Accepted { words: 4 });
    persist_lifetime_stats(Some(&path), &base, usage.session_totals()).expect("final");

    let on_disk = stats::parse_stats_file(&std::fs::read_to_string(&path).unwrap());
    assert_eq!(on_disk.accepted, 52, "base 50 + 2 accepts, counted once");
    assert_eq!(on_disk.words, 207, "base 200 + 7 words, counted once");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lifetime_flush_skips_without_path_and_errors_cleanly_on_unwritable_dest() {
    // No stats path (no HOME/COMPME_CONFIG) → quiet no-op success. This is
    // the only TRUE fail-soft case: there is nowhere to write, so success.
    assert!(
        persist_lifetime_stats(None, &stats::PersistedStats::default(), Default::default()).is_ok()
    );
    // Unwritable destination (parent is a regular FILE) → Err (NOT
    // soft-swallowed), no panic, and nothing is written at the target.
    let blocker = flush_temp_path("blocked");
    std::fs::write(&blocker, b"i am a file").unwrap();
    let path = blocker.join("stats.env");
    assert!(persist_lifetime_stats(
        Some(&path),
        &stats::PersistedStats::default(),
        Default::default()
    )
    .is_err());
    assert!(
        !path.exists(),
        "a failed flush must leave nothing at the destination"
    );
    let _ = std::fs::remove_file(&blocker);
}

#[test]
fn lifetime_flush_creates_a_nested_missing_parent_and_leaves_no_temp() {
    // The stats home may be several levels deep and not yet exist (first run
    // before any dir is created). `create_dir_all(parent)` must build the
    // whole chain — `create_dir` alone would error on the missing
    // grandparent. After a clean flush the only file present is the target:
    // the `.env.tmp` scratch must have been renamed away, never left behind.
    let root = flush_temp_path("nested");
    let _ = std::fs::remove_dir_all(&root);
    let path = root.join("a").join("b").join("stats.env");
    assert!(
        !path.parent().unwrap().exists(),
        "parent chain absent up front"
    );

    persist_lifetime_stats(
        Some(&path),
        &stats::PersistedStats::default(),
        Default::default(),
    )
    .expect("flush into a missing nested parent");

    assert!(path.exists(), "the target file must exist after the flush");
    let leftovers: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .expect("dir readable")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        vec!["stats.env".to_string()],
        "no `.env.tmp` scratch may linger beside the renamed target"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn lifetime_flush_creates_owner_only_parent_and_stats_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = flush_temp_path("private-perms");
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("stats.env");

    persist_lifetime_stats(
        Some(&path),
        &stats::PersistedStats::default(),
        Default::default(),
    )
    .expect("fresh stats flush");

    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700, "fresh stats parent is owner-only");
    assert_eq!(mode(&path), 0o600, "stats file is owner-only");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn lifetime_flush_preserves_custom_parent_and_ignores_legacy_temp_symlink() {
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::PermissionsExt;

    let dir = flush_temp_path("custom-perms");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = dir.join("stats.env");
    let tmp = path.with_extension("env.tmp");
    let victim = dir.join("victim.txt");
    std::fs::write(&victim, b"PRECIOUS").unwrap();
    symlink(&victim, &tmp).unwrap();

    persist_lifetime_stats(
        Some(&path),
        &stats::PersistedStats::default(),
        Default::default(),
    )
    .expect("stats flush in custom parent");

    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o755, "custom parent mode is preserved");
    assert_eq!(mode(&path), 0o600, "renamed temp and final are owner-only");
    assert_eq!(std::fs::read(&victim).unwrap(), b"PRECIOUS");
    assert!(
        tmp.is_symlink(),
        "legacy fixed temp is never opened or replaced"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn setup_poll_fires_only_while_visible_and_spaced() {
    // The Setup tab re-probes permissions on a 480ms cadence, but ONLY
    // while the window is visible — hidden windows must cost nothing.
    assert!(!setup_poll_due(false, None, 10_000), "hidden: never");
    assert!(setup_poll_due(true, None, 10_000), "first visible poll");
    assert!(
        !setup_poll_due(true, Some(10_000), 10_479),
        "inside the interval"
    );
    assert!(
        setup_poll_due(true, Some(10_000), 10_480),
        "interval elapsed"
    );
    assert!(
        !setup_poll_due(false, Some(10_000), 99_999),
        "hidden again: never, regardless of elapsed"
    );
}

#[test]
fn settings_flags_share_the_tray_enabled_atomic() {
    // The Enabled switch and the tray toggle are TWO VIEWS of ONE
    // atomic — sharing the Arc is what keeps them in sync (banked
    // c115 design). Pin identity, not just equal values.
    let config = Config::from_lookup(lookup(&[
        ("COMPME_MIDLINE", "1"),
        ("COMPME_AUTOCORRECT", "1"),
        ("COMPME_FULL_AUTOCORRECT", "1"),
        ("COMPME_THESAURUS_SELECTION", "1"),
        ("COMPME_TRAILING_SPACE", "1"),
        ("COMPME_CROSS_APP_PREVIOUS_INPUTS", "1"),
        ("COMPME_CLIPBOARD_CONTEXT", "0"),
        ("COMPME_SCREEN_CONTEXT", "1"),
        ("COMPME_EMOJI", "1"),
        ("COMPME_EMOJI_SKIN_TONE", "dark"),
        ("COMPME_EMOJI_GENDER", "female"),
        ("COMPME_INSTRUCTIONS", "Keep completions terse."),
        ("COMPME_SENDER_NAME", "Ada"),
        ("COMPME_SENDER_EMAIL", "ada@example.com"),
        ("COMPME_STRENGTH", "4"),
    ]));
    let tray_enabled = Arc::new(AtomicBool::new(true));
    let flags = build_settings_flags(&config, Arc::clone(&tray_enabled), false, 16);
    assert!(Arc::ptr_eq(&flags.general_enabled, &tray_enabled));
    assert!(!flags.general_launch_at_login.load(Ordering::Relaxed));
    assert_eq!(
        flags.labs_midline.load(Ordering::Relaxed),
        config.allow_mid_word
    );
    assert!(flags.general_autocorrect.load(Ordering::Relaxed) == config.autocorrect);
    assert_eq!(
        flags.general_full_autocorrect.load(Ordering::Relaxed),
        config.full_autocorrect
    );
    assert_eq!(
        flags.general_thesaurus_selection.load(Ordering::Relaxed),
        config.thesaurus_selection
    );
    assert_eq!(
        flags.general_trailing_space.load(Ordering::Relaxed),
        config.trailing_space
    );
    assert_eq!(
        flags
            .context_cross_app_previous_inputs
            .load(Ordering::Relaxed),
        config.cross_app_previous_inputs
    );
    assert!(flags.context_clipboard.load(Ordering::Relaxed) == config.clipboard_context);
    assert!(flags.context_screen.load(Ordering::Relaxed) == config.screen_context);
    assert_eq!(
        flags.emoji_enabled.load(Ordering::Relaxed),
        config.emoji.is_some()
    );
    assert_eq!(
        flags.emoji_skin_tone_index.load(Ordering::Relaxed),
        emoji_skin_tone_index(config.emoji_prefs.skin_tone)
    );
    assert_eq!(
        flags.emoji_gender_index.load(Ordering::Relaxed),
        emoji_gender_index(config.emoji_prefs.gender)
    );
    assert_eq!(
        flags.setup_model_index.load(Ordering::Relaxed),
        crate::model_picker::recommended_index()
    );
    let expected_titles = crate::model_picker::model_menu_titles(16);
    assert!(!flags.setup_model_menu_titles.is_empty());
    assert_eq!(flags.setup_model_menu_titles, expected_titles);
    assert_eq!(
        *flags.personalization_instructions.lock().unwrap(),
        config.personalization.global_instructions
    );
    assert_eq!(
        *flags.personalization_sender_name.lock().unwrap(),
        config.personalization.sender.name
    );
    assert_eq!(
        *flags.personalization_sender_email.lock().unwrap(),
        config.personalization.sender.email
    );
    assert_eq!(
        flags.personalization_strength_index.load(Ordering::Relaxed),
        personalization_strength_index(config.personalization.strength)
    );
    assert_eq!(
        flags.personalization_strength_titles,
        personalization_strength_titles()
    );
}

// macOS-only: key NAMES come from the macOS arm's keycode label table;
// the scaffold arms render numerically until real adapters bring their
// own key naming (ROADMAP 1.1).
#[cfg(target_os = "macos")]
#[test]
fn shortcuts_text_names_known_keycodes_and_falls_back_numerically() {
    // Shortcuts tab (persist-only slice): current bindings by NAME for
    // the known codes, numeric fallback for exotic rebinds, fixed rows
    // for the non-rebindable keys, and the how-to-change note.
    let text = shortcuts_text((48, 0), (50, 0), None);
    assert!(text.contains("Accept word: Tab"));
    assert!(text.contains("Accept full: ` (backtick)"));
    assert!(text.contains("Grammar accept: Unbound"));
    assert!(text.contains("Dismiss: Esc"));
    assert!(text.contains("Cycle candidates: Down arrow"));
    assert!(text.contains("COMPME_ACCEPT_WORD_KEY"));
    assert!(text.contains("COMPME_GRAMMAR_ACCEPT_KEY"));
    assert!(text.contains("relaunch"));

    let custom = shortcuts_text((125, 0), (200, 0), Some((96, 0)));
    assert!(custom.contains("Accept word: Down arrow"));
    assert!(custom.contains("Accept full: key 200")); // unnamed code → generic
    assert!(custom.contains("Grammar accept: F5"));

    // Modifier masks render as glyph-prefixed labels (slice 1b label half):
    // 512 = Carbon shiftKey ⇧, 4096 = controlKey ⌃.
    let combo = shortcuts_text((48, 512), (50, 4096), Some((96, 512)));
    assert!(combo.contains("Accept word: \u{21e7}Tab"), "{combo}");
    assert!(
        combo.contains("Accept full: \u{2303}` (backtick)"),
        "{combo}"
    );
    assert!(combo.contains("Grammar accept: \u{21e7}F5"), "{combo}");
}

#[test]
fn domain_from_url_extracts_lowercased_host_without_port() {
    // The per-domain extractor's pure half (audit c121: domain gating
    // was promised but dead — the gates now consume a domain and this
    // is the URL→domain step the AX extractor will feed).
    assert_eq!(
        domain_from_url("https://Sub.Example.COM:8443/path?q=1"),
        Some("sub.example.com".to_string())
    );
    assert_eq!(
        domain_from_url("http://docs.google.com/document/d/x"),
        Some("docs.google.com".to_string())
    );
    assert_eq!(domain_from_url("not a url"), None);
    assert_eq!(domain_from_url("file:///etc/hosts"), None);
    assert_eq!(domain_from_url("https://"), None);
    // Userinfo stripping is SECURITY-relevant: `user:pw@host` must resolve
    // to the real host (after the last `@`), never the userinfo. For
    // `evil.com@bank.example` the host that per-domain rules must gate is
    // bank.example — taking the userinfo (or .next() instead of
    // .next_back()) would defeat the exclusion.
    assert_eq!(
        domain_from_url("https://user:pw@Bank.Example/account"),
        Some("bank.example".to_string())
    );
    assert_eq!(
        domain_from_url("https://evil.com@bank.example/"),
        Some("bank.example".to_string())
    );
    // Host with no path still resolves.
    assert_eq!(
        domain_from_url("https://example.com"),
        Some("example.com".to_string())
    );
    // Absolute DNS names may carry one terminal root-label dot. Domain
    // rules use their ordinary spelling, so the extractor must normalize
    // that equivalent host form before caching or matching exclusions.
    assert_eq!(
        domain_from_url("https://Bank.Example./account"),
        Some("bank.example".to_string())
    );
    assert_eq!(
        domain_from_url("https://login.bank.example./"),
        Some("login.bank.example".to_string())
    );
}

#[test]
fn comma_list_trims_and_drops_empties_and_sorted_join_orders() {
    // Pinned in isolation (previously only exercised via build_prefs
    // round-trips, which would not localize a "keep empties" regression):
    // surrounding whitespace is trimmed, empty/whitespace-only entries are
    // dropped (including a doubled `,,`), and None yields an empty list.
    assert_eq!(comma_list(Some(" a , ,b ,".into())), vec!["a", "b"]);
    assert_eq!(comma_list(Some("x,,y".into())), vec!["x", "y"]);
    assert_eq!(comma_list(Some("   ".into())), Vec::<String>::new());
    assert_eq!(comma_list(None), Vec::<String>::new());
    // sorted_join is stable-sorted for a deterministic file diff.
    assert_eq!(sorted_join(["c", "a", "b"].into_iter()), "a,b,c");
    assert_eq!(sorted_join(std::iter::empty()), "");
}

#[test]
fn suggestion_gates_honor_an_excluded_domain_when_present() {
    // End-to-end through the shared gate (audit top-5 missing test):
    // the domain parameter must actually block — and None must not.
    let mut prefs = Prefs::default();
    prefs.excluded_domains.insert("blocked.example".to_string());
    assert!(suggestion_gates_pass(
        Some("com.apple.Safari"),
        "hello",
        Some("ok.example"),
        &prefs,
        0
    ));
    assert!(!suggestion_gates_pass(
        Some("com.apple.Safari"),
        "hello",
        Some("blocked.example"),
        &prefs,
        0
    ));
    assert!(suggestion_gates_pass(
        Some("com.apple.Safari"),
        "hello",
        None,
        &prefs,
        0
    ));
    let fqdn = domain_from_url("https://login.blocked.example./").expect("valid absolute DNS host");
    assert!(!suggestion_gates_pass(
        Some("com.apple.Safari"),
        "hello",
        Some(&fqdn),
        &prefs,
        0
    ));
}

#[test]
fn domain_miss_notice_fires_at_threshold_not_before() {
    let mut notice = DomainMissNotice::default();
    for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD - 1 {
        assert_eq!(notice.observe(true, false), None);
    }
    let msg = notice.observe(true, false).expect("fires at the threshold");
    assert!(msg.contains(&DOMAIN_MISS_NOTICE_THRESHOLD.to_string()));
    assert!(msg.contains("COMPME_DEBUG"));
}

#[test]
fn domain_miss_notice_success_resets_the_streak() {
    let mut notice = DomainMissNotice::default();
    for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD - 1 {
        assert_eq!(notice.observe(true, false), None);
    }
    assert_eq!(notice.observe(true, true), None, "success resets");
    for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD - 1 {
        assert_eq!(notice.observe(true, false), None, "fresh streak");
    }
    assert!(notice.observe(true, false).is_some());
}

#[test]
fn domain_miss_notice_is_one_shot_per_process() {
    let mut notice = DomainMissNotice::default();
    for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD {
        let _ = notice.observe(true, false);
    }
    // Keep missing, even across a reset + fresh streak: never re-fires.
    assert_eq!(notice.observe(true, true), None);
    for _ in 0..3 * DOMAIN_MISS_NOTICE_THRESHOLD {
        assert_eq!(notice.observe(true, false), None);
    }
}

#[test]
fn domain_miss_notice_never_fires_without_rules() {
    let mut notice = DomainMissNotice::default();
    for _ in 0..3 * DOMAIN_MISS_NOTICE_THRESHOLD {
        assert_eq!(notice.observe(false, false), None);
    }
}

#[test]
fn domain_miss_notice_rules_removed_mid_streak_suppress_then_restore_fires() {
    // The mirror of the mid-streak test (reachable via deep-link Domain
    // Enable removing the last rule): crossing the threshold while rules
    // are ABSENT stays silent; restoring rules fires on the next miss.
    let mut notice = DomainMissNotice::default();
    for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD - 1 {
        assert_eq!(notice.observe(true, false), None);
    }
    // Rules removed exactly at the would-fire miss: suppressed.
    assert_eq!(notice.observe(false, false), None);
    // Rules restored: the accumulated streak fires immediately.
    assert!(notice.observe(true, false).is_some());
}

#[test]
fn domain_miss_notice_rules_added_mid_streak_fire_immediately() {
    // The streak counts even while rules are empty (detection genuinely
    // HAS been failing); the first miss after rules appear fires.
    let mut notice = DomainMissNotice::default();
    for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD + 5 {
        assert_eq!(notice.observe(false, false), None);
    }
    assert!(notice.observe(true, false).is_some());
}

#[test]
fn domain_cache_entry_pairs_browser_app_with_extracted_host() {
    // The Focus-arm decision: a browser app + a real page URL caches
    // (app key, HOST) — the full URL is dropped at extraction (privacy
    // boundary; path/query never leave the expression).
    assert_eq!(
        domain_cache_entry(
            Some("com.apple.Safari"),
            Some("https://docs.google.com/document/d/abc?tab=1")
        ),
        Some((
            "com.apple.Safari".to_string(),
            "docs.google.com".to_string()
        ))
    );
    // Non-browser app: never caches, even with a URL-shaped value.
    assert_eq!(
        domain_cache_entry(Some("com.apple.TextEdit"), Some("https://x.example/")),
        None
    );
    // Browser but no URL resolved (AX miss): fail-open.
    assert_eq!(domain_cache_entry(Some("com.google.Chrome"), None), None);
    // Browser but a non-URL value (omnibox search text shape): the
    // extractor rejects it — no bogus host.
    assert_eq!(
        domain_cache_entry(Some("com.google.Chrome"), Some("how to cook rice")),
        None
    );
    // No app key: nothing to attribute the host to.
    assert_eq!(domain_cache_entry(None, Some("https://x.example/")), None);
}

#[test]
fn apply_global_disable_maps_arms_to_snooze_or_persistent_off() {
    // Global submenu (a3 build item 1, the half the 06-10 annotation
    // overclaimed): Hour/UntilRelaunch ride the snooze machinery like
    // the per-app arms; Always asks the caller to flip the persistent
    // enabled flag (true return) — the existing edge persists it.
    let mut prefs = Prefs::default();
    assert!(!apply_global_disable(DisableArm::Hour, &mut prefs, 1_000));
    assert!(prefs.is_snoozed(1_000 + 59 * 60 * 1000));
    assert!(!prefs.is_snoozed(1_000 + 61 * 60 * 1000));

    let mut prefs = Prefs::default();
    assert!(!apply_global_disable(
        DisableArm::UntilRelaunch,
        &mut prefs,
        1_000
    ));
    assert!(prefs.is_snoozed(u64::MAX - 1), "holds for the process life");

    let mut prefs = Prefs::default();
    assert!(apply_global_disable(DisableArm::Always, &mut prefs, 1_000));
    assert!(!prefs.is_snoozed(2_000), "Always is not a snooze");
}

#[test]
fn switch_edge_fires_once_per_change_and_tracks_current() {
    // The watcher contract (audit c121): apply+persist exactly once per
    // edge, never per heartbeat.
    let flag = AtomicBool::new(false);
    let mut current = false;
    assert_eq!(switch_edge(&flag, &mut current), None, "no change: quiet");
    flag.store(true, Ordering::Relaxed);
    assert_eq!(switch_edge(&flag, &mut current), Some(true), "edge fires");
    assert!(current, "current tracks the new state");
    assert_eq!(switch_edge(&flag, &mut current), None, "same state: quiet");
    flag.store(false, Ordering::Relaxed);
    assert_eq!(switch_edge(&flag, &mut current), Some(false));
}

#[test]
fn general_autocorrect_settings_edge_applies_live_and_persists_once() {
    let flag = AtomicBool::new(true);
    let mut current = false;
    let persisted = RefCell::new(Vec::new());
    let dismissed = RefCell::new(Vec::new());

    assert_eq!(
        apply_autocorrect_settings_edge(
            &flag,
            &mut current,
            |on| persisted.borrow_mut().push(on),
            |on| dismissed.borrow_mut().push(on),
        ),
        Some(true)
    );
    assert!(current);
    assert_eq!(persisted.borrow().as_slice(), &[true]);
    assert_eq!(dismissed.borrow().as_slice(), &[] as &[bool]);
    assert_eq!(
        apply_autocorrect_settings_edge(
            &flag,
            &mut current,
            |on| persisted.borrow_mut().push(on),
            |on| dismissed.borrow_mut().push(on),
        ),
        None
    );
    assert_eq!(persisted.borrow().as_slice(), &[true]);
    assert_eq!(dismissed.borrow().as_slice(), &[] as &[bool]);

    flag.store(false, Ordering::Relaxed);
    assert_eq!(
        apply_autocorrect_settings_edge(
            &flag,
            &mut current,
            |on| persisted.borrow_mut().push(on),
            |on| dismissed.borrow_mut().push(on),
        ),
        Some(false)
    );
    assert!(!current);
    assert_eq!(persisted.borrow().as_slice(), &[true, false]);
    assert_eq!(dismissed.borrow().as_slice(), &[false]);
}

#[test]
fn trailing_space_settings_edge_sets_engine_and_persists_once() {
    let flag = AtomicBool::new(false);
    let mut current = true;
    let live = RefCell::new(Vec::new());
    let persisted = RefCell::new(Vec::new());

    assert_eq!(
        apply_trailing_space_settings_edge(
            &flag,
            &mut current,
            |on| live.borrow_mut().push(on),
            |on| persisted.borrow_mut().push(on),
        ),
        Some(false)
    );
    assert!(!current);
    assert_eq!(live.borrow().as_slice(), &[false]);
    assert_eq!(persisted.borrow().as_slice(), &[false]);
}

#[test]
fn midline_settings_edge_applies_effective_app_policy_and_persists_global_default() {
    let flag = AtomicBool::new(true);
    let mut global = false;
    let mut prefs = Prefs::default();
    prefs.set_app_policy_field("com.override", prefs::AppPolicyField::MidLine, false);
    let live = RefCell::new(Vec::new());
    let persisted = RefCell::new(Vec::new());

    assert_eq!(
        apply_midline_settings_edge(
            &flag,
            &mut global,
            &prefs,
            Some("com.override"),
            |on| live.borrow_mut().push(on),
            |on| persisted.borrow_mut().push(on),
        ),
        Some(true)
    );

    assert!(global);
    assert_eq!(live.borrow().as_slice(), &[false]);
    assert_eq!(persisted.borrow().as_slice(), &[true]);
}

#[test]
fn drain_settings_edges_is_quiet_when_no_flag_moved() {
    // The watcher contract carries over to the drain: no flag movement, no
    // commands, no mirror writes — the heartbeat stays a no-op.
    let config = Config::from_lookup(lookup(&[]));
    let mut settings = SettingsState::new(
        config.allow_mid_word,
        config.emoji.is_some(),
        config.emoji_prefs,
        emoji_skin_tone_index(config.emoji_prefs.skin_tone),
        emoji_gender_index(config.emoji_prefs.gender),
        false,
    );
    let flags = build_settings_flags(&config, Arc::new(AtomicBool::new(false)), false, 16);
    let prefs = Prefs::default();

    assert!(drain_settings_edges(&flags, &mut { config }, &mut settings, &prefs, None).is_empty());
}

#[test]
fn drain_settings_edges_flips_pure_switch_mirrors_and_emits_commands_in_order() {
    // Every pure switch: the drain owns the Config/SettingsState mirror AND
    // emits one command carrying the new value, in the watcher order the
    // loop applied them (so apply replays the same sequence).
    let config = Config::from_lookup(lookup(&[]));
    let mut settings = SettingsState::new(
        config.allow_mid_word,
        config.emoji.is_some(),
        config.emoji_prefs,
        emoji_skin_tone_index(config.emoji_prefs.skin_tone),
        emoji_gender_index(config.emoji_prefs.gender),
        false,
    );
    let flags = build_settings_flags(&config, Arc::new(AtomicBool::new(false)), false, 16);
    flags.general_autocorrect.store(true, Ordering::Relaxed);
    flags
        .general_full_autocorrect
        .store(true, Ordering::Relaxed);
    flags
        .general_thesaurus_selection
        .store(true, Ordering::Relaxed);
    flags.general_trailing_space.store(true, Ordering::Relaxed);
    flags.labs_midline.store(true, Ordering::Relaxed);
    flags
        .context_cross_app_previous_inputs
        .store(true, Ordering::Relaxed);
    flags.context_clipboard.store(true, Ordering::Relaxed);
    let prefs = Prefs::default();
    let mut config = config;

    let commands = drain_settings_edges(&flags, &mut config, &mut settings, &prefs, None);

    assert_eq!(
        commands,
        vec![
            SettingsCommand::Autocorrect { on: true },
            SettingsCommand::FullAutocorrect { on: true },
            SettingsCommand::ThesaurusSelection { on: true },
            SettingsCommand::TrailingSpace { on: true },
            SettingsCommand::Midline {
                on: true,
                allow: true,
            },
            SettingsCommand::CrossAppPreviousInputs { on: true },
            SettingsCommand::ClipboardContext { on: true },
        ]
    );
    // Mirrors moved with the flags.
    assert!(config.autocorrect);
    assert!(config.full_autocorrect);
    assert!(config.thesaurus_selection);
    assert!(config.trailing_space);
    assert!(settings.global_mid_word);
    assert!(config.cross_app_previous_inputs);
    assert!(config.clipboard_context);
    // And a second drain over the same state is quiet.
    assert!(drain_settings_edges(&flags, &mut config, &mut settings, &prefs, None).is_empty());
}

#[test]
fn drain_settings_edges_midline_command_carries_the_resolved_app_gate() {
    // The engine effect for mid-line is NOT the raw global: it is the
    // per-app-resolved gate (prefs override + focused app), so the command
    // carries `allow` computed at drain time.
    let config = Config::from_lookup(lookup(&[]));
    let mut settings = SettingsState::new(
        false,
        config.emoji.is_some(),
        config.emoji_prefs,
        emoji_skin_tone_index(config.emoji_prefs.skin_tone),
        emoji_gender_index(config.emoji_prefs.gender),
        false,
    );
    let flags = build_settings_flags(&config, Arc::new(AtomicBool::new(false)), false, 16);
    flags.labs_midline.store(true, Ordering::Relaxed);
    let mut prefs = Prefs::default();
    prefs.set_app_policy_field("com.override", prefs::AppPolicyField::MidLine, false);
    let mut config = config;

    let commands = drain_settings_edges(
        &flags,
        &mut config,
        &mut settings,
        &prefs,
        Some("com.override"),
    );

    assert_eq!(
        commands,
        vec![SettingsCommand::Midline {
            on: true,
            allow: false,
        }]
    );
    assert!(settings.global_mid_word, "the global default still flips");
}

#[test]
fn drain_settings_edges_defers_the_os_backed_mirrors_to_apply() {
    // Launch-at-login and screen context are the two edges the spec analysis
    // flagged as non-pure: their mirrors may only move on a SUCCESSFUL
    // OS apply, and a rejection must restore the UI atomic. The drain
    // therefore emits the command WITHOUT consuming the edge; apply updates
    // the mirror (or reverts the flag) in the same heartbeat.
    let config = Config::from_lookup(lookup(&[]));
    let mut settings = SettingsState::new(
        config.allow_mid_word,
        config.emoji.is_some(),
        config.emoji_prefs,
        emoji_skin_tone_index(config.emoji_prefs.skin_tone),
        emoji_gender_index(config.emoji_prefs.gender),
        false,
    );
    let flags = build_settings_flags(&config, Arc::new(AtomicBool::new(false)), false, 16);
    flags.general_launch_at_login.store(true, Ordering::Relaxed);
    flags.context_screen.store(true, Ordering::Relaxed);
    let prefs = Prefs::default();
    let mut config = config;

    let commands = drain_settings_edges(&flags, &mut config, &mut settings, &prefs, None);

    assert_eq!(
        commands,
        vec![
            SettingsCommand::LaunchAtLogin { desired: true },
            SettingsCommand::ScreenContext { desired: true },
        ]
    );
    assert!(!settings.current_launch_at_login, "apply owns this mirror");
    assert!(!config.screen_context, "apply owns this mirror too");
    // Because the edge was not consumed, a drain without an intervening
    // apply re-emits — the loop must always run the two halves together.
    assert_eq!(
        drain_settings_edges(&flags, &mut config, &mut settings, &prefs, None),
        vec![
            SettingsCommand::LaunchAtLogin { desired: true },
            SettingsCommand::ScreenContext { desired: true },
        ]
    );
}

#[test]
fn drain_settings_edges_emoji_switch_preserves_prefs_and_reports_popup_values() {
    // Emoji off/on cycles keep the parsed payload (SettingsState.emoji_prefs)
    // so re-enabling restores the user's tone/gender; the popup watchers
    // clamp out-of-range atomics and report the persisted value string.
    let mut config = Config::from_lookup(lookup(&[]));
    config.emoji_prefs = EmojiPrefs {
        skin_tone: SkinTone::Medium,
        gender: Gender::Female,
    };
    config.emoji = Some(config.emoji_prefs);
    let mut settings = SettingsState::new(
        false,
        true,
        config.emoji_prefs,
        emoji_skin_tone_index(SkinTone::Medium),
        emoji_gender_index(Gender::Female),
        false,
    );
    let flags = build_settings_flags(&config, Arc::new(AtomicBool::new(false)), false, 16);
    let prefs = Prefs::default();

    // Off: payload moves out of config.emoji but survives in the state.
    flags.emoji_enabled.store(false, Ordering::Relaxed);
    let commands = drain_settings_edges(&flags, &mut config, &mut settings, &prefs, None);
    assert_eq!(commands, vec![SettingsCommand::EmojiSwitch { on: false }]);
    assert_eq!(config.emoji, None);
    assert_eq!(settings.emoji_prefs.skin_tone, SkinTone::Medium);
    assert_eq!(settings.emoji_prefs.gender, Gender::Female);

    // Back on: the saved payload is restored into config.emoji.
    flags.emoji_enabled.store(true, Ordering::Relaxed);
    let commands = drain_settings_edges(&flags, &mut config, &mut settings, &prefs, None);
    assert_eq!(commands, vec![SettingsCommand::EmojiSwitch { on: true }]);
    assert_eq!(config.emoji, Some(settings.emoji_prefs));

    // Skin tone popup: an out-of-range atomic clamps to the last index and
    // the command carries the persisted value string.
    flags.emoji_skin_tone_index.store(99, Ordering::Relaxed);
    let commands = drain_settings_edges(&flags, &mut config, &mut settings, &prefs, None);
    assert_eq!(commands.len(), 1);
    let SettingsCommand::EmojiSkinTone { value } = &commands[0] else {
        panic!("expected a skin-tone command");
    };
    assert_eq!(*value, emoji_skin_tone_value(SkinTone::Dark));
    assert_eq!(
        settings.emoji_skin_tone_index,
        EMOJI_SKIN_TONE_VALUES.len() - 1,
        "clamped to the last valid index"
    );

    // Gender popup mirrors the same contract (index 2 = Male; the initial
    // Female payload sits at index 1, so 1 would be a no-op edge).
    flags.emoji_gender_index.store(2, Ordering::Relaxed);
    let commands = drain_settings_edges(&flags, &mut config, &mut settings, &prefs, None);
    assert_eq!(commands.len(), 1);
    let SettingsCommand::EmojiGender { value } = &commands[0] else {
        panic!("expected a gender command");
    };
    assert_eq!(*value, emoji_gender_value(Gender::Male));
    assert_eq!(config.emoji.as_ref().map(|p| p.gender), Some(Gender::Male));
}

#[test]
fn apply_settings_commands_persists_switches_and_dismisses_on_off_edges() {
    // The apply half (plan item 4a): persists fire in command order with
    // their exact keys, and ONLY off-edges retract the visible suggestion.
    let mut engine = phase_engine();
    let mut suggestion = phase_pending_suggestion();
    let config = Config::from_lookup(lookup(&[]));
    let mut settings = phase_settings_state(Vec::new());
    let shell: Arc<dyn ShellHost> = Arc::new(RecordingShell {
        trusted: true,
        log: startup_log(),
    });
    let settings_flags = build_settings_flags(&config, Arc::new(AtomicBool::new(false)), false, 16);
    let mut settings_window = crate::shell::SettingsWindow::new(settings_flags.clone());
    let cross_app = AtomicBool::new(false);
    let previous_inputs = PreviousInputs::default();
    let clipboard_cell = Mutex::new(Some("clip".to_string()));
    let screen_cell = Mutex::new(None);
    let mut screen_ocr = None;
    let waits = RefCell::new(Vec::new());
    let recomposed = RefCell::new(Vec::new());
    let persisted = RefCell::new(Vec::new());
    let persisted_values = RefCell::new(Vec::new());

    apply_settings_commands(
        &[
            SettingsCommand::Autocorrect { on: false },
            SettingsCommand::TrailingSpace { on: true },
            SettingsCommand::Midline {
                on: true,
                allow: false,
            },
            SettingsCommand::CrossAppPreviousInputs { on: false },
            SettingsCommand::ClipboardContext { on: false },
            SettingsCommand::EmojiSwitch { on: false },
            SettingsCommand::EmojiSkinTone { value: "dark" },
        ],
        SettingsApplyCtx {
            engine: &mut engine,
            suggestion: &mut suggestion,
            shell: &shell,
            settings_window: &mut settings_window,
            config: &mut { config },
            settings: &mut settings,
            launch_login_flag: settings_flags.general_launch_at_login.as_ref(),
            screen_flag: settings_flags.context_screen.as_ref(),
            cross_app_previous_inputs: &cross_app,
            previous_inputs: &previous_inputs,
            clipboard_cell: &clipboard_cell,
            screen_cell: &screen_cell,
            screen_ocr: &mut screen_ocr,
            set_screen_wait_ms: &|ms| waits.borrow_mut().push(ms),
            spawn_screen_ocr: &|| Err("not spawned in this test".to_string()),
            recompose_setup_lines: &|_config| recomposed.borrow_mut().push(()),
            persist_switch: &|key, _label, on| persisted.borrow_mut().push((key.to_string(), on)),
            persist_value: &|key, _label, value| {
                persisted_values
                    .borrow_mut()
                    .push((key.to_string(), value.to_string()))
            },
        },
    );

    assert_eq!(
        persisted.borrow().as_slice(),
        [
            ("COMPME_AUTOCORRECT".to_string(), false),
            ("COMPME_TRAILING_SPACE".to_string(), true),
            ("COMPME_MIDLINE".to_string(), true),
            ("COMPME_CROSS_APP_PREVIOUS_INPUTS".to_string(), false),
            ("COMPME_CLIPBOARD_CONTEXT".to_string(), false),
            ("COMPME_EMOJI".to_string(), false),
        ]
    );
    assert_eq!(
        persisted_values.borrow().as_slice(),
        [("COMPME_EMOJI_SKIN_TONE".to_string(), "dark".to_string())]
    );
    // The off-edges (autocorrect first) cleared the pending suggestion.
    assert!(
        suggestion.latest.take().is_none(),
        "an off-edge must retract the visible suggestion"
    );
    // Cross-app mirror dropped and the clipboard cell was cleared.
    assert!(!cross_app.load(Ordering::Relaxed));
    assert!(clipboard_cell.lock().unwrap().is_none());
    // No screen edge ran: no waits, no recompose.
    assert!(waits.borrow().is_empty());
    assert!(recomposed.borrow().is_empty());
}

#[test]
fn apply_settings_commands_launch_at_login_rejection_restores_the_ui_atomic() {
    // The OS-backed edge (spec analysis: launch-at-login is NOT pure): a
    // rejected shell mutation restores the UI atomic, leaves the
    // SettingsState mirror untouched, and persists nothing.
    struct RejectingShell;
    impl ShellHost for RejectingShell {
        fn pump_events(&self, _heartbeat: Duration) {}
        fn accessibility_trusted(&self) -> bool {
            true
        }
        fn prompt_accessibility_trust(&self) -> bool {
            true
        }
        fn physical_memory_bytes(&self) -> u64 {
            0
        }
        fn open_url(&self, _url: &str) -> Result<(), PlatformError> {
            Ok(())
        }
        fn open_permission_settings(&self) -> Result<(), PlatformError> {
            Ok(())
        }
        fn reveal_file(&self, _path: &Path) -> Result<(), PlatformError> {
            Ok(())
        }
        fn set_launch_at_login(&self, _enabled: bool) -> Result<(), PlatformError> {
            Err(PlatformError::CannotComplete {
                reason: "rejected by test shell".into(),
            })
        }
        fn confirm(&self, _prompt: &shell_flags::ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
            Ok(false)
        }
        fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
            Ok([0; 32])
        }
    }

    let mut engine = phase_engine();
    let mut suggestion = SuggestionState::new();
    let config = Config::from_lookup(lookup(&[]));
    let mut settings = phase_settings_state(Vec::new());
    let shell: Arc<dyn ShellHost> = Arc::new(RejectingShell);
    let settings_flags = build_settings_flags(&config, Arc::new(AtomicBool::new(false)), false, 16);
    settings_flags
        .general_launch_at_login
        .store(true, Ordering::Relaxed);
    let mut settings_window = crate::shell::SettingsWindow::new(settings_flags.clone());
    let cross_app = AtomicBool::new(false);
    let previous_inputs = PreviousInputs::default();
    let clipboard_cell = Mutex::new(None);
    let screen_cell = Mutex::new(None);
    let mut screen_ocr = None;
    let persisted = RefCell::new(Vec::new());

    apply_settings_commands(
        &[SettingsCommand::LaunchAtLogin { desired: true }],
        SettingsApplyCtx {
            engine: &mut engine,
            suggestion: &mut suggestion,
            shell: &shell,
            settings_window: &mut settings_window,
            config: &mut { config },
            settings: &mut settings,
            launch_login_flag: settings_flags.general_launch_at_login.as_ref(),
            screen_flag: settings_flags.context_screen.as_ref(),
            cross_app_previous_inputs: &cross_app,
            previous_inputs: &previous_inputs,
            clipboard_cell: &clipboard_cell,
            screen_cell: &screen_cell,
            screen_ocr: &mut screen_ocr,
            set_screen_wait_ms: &|_| {},
            spawn_screen_ocr: &|| Err("not spawned in this test".to_string()),
            recompose_setup_lines: &|_| {},
            persist_switch: &|key, _label, on| persisted.borrow_mut().push((key.to_string(), on)),
            persist_value: &|_key, _label, _value| {},
        },
    );

    assert!(
        persisted.borrow().is_empty(),
        "a rejected OS mutation persists nothing"
    );
    assert!(
        !settings_flags
            .general_launch_at_login
            .load(Ordering::Relaxed),
        "the UI atomic is restored to the applied truth"
    );
    assert!(!settings.current_launch_at_login, "the mirror never moved");
}

#[test]
fn apply_settings_commands_screen_context_denial_reverts_flag_and_mirror() {
    // The second OS-backed edge: Screen Recording denied -> the UI atomic
    // AND the config mirror revert, the wait drops to zero, the Setup pane
    // recomposes, and the persisted value is the RESOLVED (false) state.
    let mut engine = phase_engine();
    let mut suggestion = SuggestionState::new();
    let mut config = Config::from_lookup(lookup(&[]));
    let mut settings = phase_settings_state(Vec::new());
    // RecordingShell's screen_capture_permission default is false (denied).
    let shell: Arc<dyn ShellHost> = Arc::new(RecordingShell {
        trusted: true,
        log: startup_log(),
    });
    let settings_flags = build_settings_flags(&config, Arc::new(AtomicBool::new(false)), false, 16);
    settings_flags.context_screen.store(true, Ordering::Relaxed);
    let mut settings_window = crate::shell::SettingsWindow::new(settings_flags.clone());
    let cross_app = AtomicBool::new(false);
    let previous_inputs = PreviousInputs::default();
    let clipboard_cell = Mutex::new(None);
    let screen_cell = Mutex::new(None);
    let mut screen_ocr = None;
    let waits = RefCell::new(Vec::new());
    let recomposed = RefCell::new(Vec::new());
    let persisted = RefCell::new(Vec::new());

    apply_settings_commands(
        &[SettingsCommand::ScreenContext { desired: true }],
        SettingsApplyCtx {
            engine: &mut engine,
            suggestion: &mut suggestion,
            shell: &shell,
            settings_window: &mut settings_window,
            config: &mut config,
            settings: &mut settings,
            launch_login_flag: settings_flags.general_launch_at_login.as_ref(),
            screen_flag: settings_flags.context_screen.as_ref(),
            cross_app_previous_inputs: &cross_app,
            previous_inputs: &previous_inputs,
            clipboard_cell: &clipboard_cell,
            screen_cell: &screen_cell,
            screen_ocr: &mut screen_ocr,
            set_screen_wait_ms: &|ms| waits.borrow_mut().push(ms),
            spawn_screen_ocr: &|| Err("not spawned in this test".to_string()),
            recompose_setup_lines: &|_config| recomposed.borrow_mut().push(()),
            persist_switch: &|key, _label, on| persisted.borrow_mut().push((key.to_string(), on)),
            persist_value: &|_key, _label, _value| {},
        },
    );

    assert!(
        !settings_flags.context_screen.load(Ordering::Relaxed),
        "denied grant restores the UI atomic"
    );
    assert!(!config.screen_context, "and the config mirror");
    assert_eq!(waits.borrow().as_slice(), &[0u64]);
    assert_eq!(
        recomposed.borrow().len(),
        1,
        "the Setup pane recomposed once"
    );
    assert_eq!(
        persisted.borrow().as_slice(),
        [("COMPME_SCREEN_CONTEXT".to_string(), false)],
        "the persisted value is the resolved state, not the request"
    );
}

#[test]
fn handle_control_event_toggle_global_flips_flag_clears_monitored_and_retracts() {
    // 4b routing pin: the Shortcut arm through HostEventCtx. Disabling via
    // the global shortcut flips TrayFlags.enabled atomically, clears the
    // monitored state for the policy transition, and retracts the visible
    // suggestion; re-enabling flips back without touching the rest.
    let mut engine = phase_engine();
    let mut suggestion = phase_pending_suggestion();
    let mut focus = FocusContext::new();
    let mut prefs = Prefs::default();
    let config = Config::from_lookup(lookup(&[]));
    let flags = phase_tray_flags();
    let field = FieldHandle {
        app: "pid:42".into(),
        pid: Some(42),
        element_id: "ax:0x123".into(),
        generation: 1,
    };
    let mut monitored = phase_monitored_with_buffer(&field);
    let mut usage = UsageStats::default();
    let shell: Arc<dyn ShellHost> = Arc::new(RecordingShell {
        trusted: true,
        log: startup_log(),
    });
    let adapter = Arc::new(FakeAdapter::allow_all(startup_log()));
    let mut manual_grammar_request = None;
    let now_ms = 1_000u64;
    let wall_ms = 1_000u64;
    let mut ctx = HostEventCtx {
        engine: &mut engine,
        suggestion: &mut suggestion,
        focus: &mut focus,
        prefs: &mut prefs,
        config: &config,
        flags: &flags,
        monitored: &mut monitored,
        usage: &mut usage,
        shell: &shell,
        adapter: &adapter,
        now_ms,
        wall_ms,
        manual_grammar_request: &mut manual_grammar_request,
    };

    handle_control_event(
        &mut ctx,
        HostControlEvent::Shortcut(ShortcutAction::ToggleGlobal),
    );

    assert!(
        !ctx.flags.enabled.load(Ordering::Relaxed),
        "the shortcut flips the shared enable atomic"
    );
    assert!(
        ctx.monitored.monitored_buffers.is_empty(),
        "the policy transition clears monitored state"
    );
    assert!(
        ctx.suggestion.latest.take().is_none(),
        "disabling retracts the visible suggestion"
    );

    handle_control_event(
        &mut ctx,
        HostControlEvent::Shortcut(ShortcutAction::ToggleGlobal),
    );
    assert!(
        ctx.flags.enabled.load(Ordering::Relaxed),
        "re-enabling flips back"
    );
}

#[test]
fn handle_control_event_grammar_check_without_field_stays_disarmed() {
    // 4b routing pin: GrammarCheck with no focused field routes to the
    // NoField outcome — nothing armed, nothing retracted.
    let mut engine = phase_engine();
    let mut suggestion = phase_pending_suggestion();
    let mut focus = FocusContext::new();
    let mut prefs = Prefs::default();
    let config = Config::from_lookup(lookup(&[]));
    let flags = phase_tray_flags();
    let mut monitored = MonitoredInput::default();
    let mut usage = UsageStats::default();
    let shell: Arc<dyn ShellHost> = Arc::new(RecordingShell {
        trusted: true,
        log: startup_log(),
    });
    let adapter = Arc::new(FakeAdapter::allow_all(startup_log()));
    let mut manual_grammar_request = None;
    let mut ctx = HostEventCtx {
        engine: &mut engine,
        suggestion: &mut suggestion,
        focus: &mut focus,
        prefs: &mut prefs,
        config: &config,
        flags: &flags,
        monitored: &mut monitored,
        usage: &mut usage,
        shell: &shell,
        adapter: &adapter,
        now_ms: 1_000,
        wall_ms: 1_000,
        manual_grammar_request: &mut manual_grammar_request,
    };

    handle_control_event(
        &mut ctx,
        HostControlEvent::Shortcut(ShortcutAction::GrammarCheck),
    );

    assert!(
        ctx.manual_grammar_request.is_none(),
        "no field → no armed grammar request"
    );
    assert!(
        ctx.suggestion.latest.take().is_some(),
        "the pending suggestion is untouched"
    );
}

#[test]
fn handle_control_event_routes_dismiss_cycle_and_force_activate() {
    // 4b routing pin: the light arms route through the same ctx — over an
    // engine with nothing held, all three complete and leave the pending
    // request slot untouched (ForceActivate re-shows nothing, Dismiss
    // suppresses nothing, Cycle rotates nothing).
    let mut engine = phase_engine();
    let mut suggestion = phase_pending_suggestion();
    let mut focus = FocusContext::new();
    let mut prefs = Prefs::default();
    let config = Config::from_lookup(lookup(&[]));
    let flags = phase_tray_flags();
    let mut monitored = MonitoredInput::default();
    let mut usage = UsageStats::default();
    let shell: Arc<dyn ShellHost> = Arc::new(RecordingShell {
        trusted: true,
        log: startup_log(),
    });
    let adapter = Arc::new(FakeAdapter::allow_all(startup_log()));
    let mut manual_grammar_request = None;
    let mut ctx = HostEventCtx {
        engine: &mut engine,
        suggestion: &mut suggestion,
        focus: &mut focus,
        prefs: &mut prefs,
        config: &config,
        flags: &flags,
        monitored: &mut monitored,
        usage: &mut usage,
        shell: &shell,
        adapter: &adapter,
        now_ms: 1_000,
        wall_ms: 1_000,
        manual_grammar_request: &mut manual_grammar_request,
    };

    handle_control_event(
        &mut ctx,
        HostControlEvent::Shortcut(ShortcutAction::ForceActivate),
    );
    handle_control_event(&mut ctx, HostControlEvent::Dismiss);
    handle_control_event(&mut ctx, HostControlEvent::Cycle);

    assert!(
        ctx.suggestion.latest.take().is_some(),
        "nothing held → none of the three routes replaces the pending request"
    );
}

#[test]
fn delete_app_row_resolves_against_ids_and_recomposes_together() {
    // The irreversible path (audit c121, top missing test): row index →
    // app id resolution uses the SAME cap/order as the rendered lines,
    // out-of-range clicks no-op, and lines+ids recompose as one unit so
    // a follow-up click can't hit the wrong app.
    use memory::{MemoryStore, StaticKey, StorageMode};
    let store =
        MemoryStore::open_in_memory(&StaticKey([7u8; 32]), StorageMode::AcceptedOnly).unwrap();
    store.remember("com.a.alpha", "x").unwrap();
    store.remember("com.a.alpha", "y").unwrap();
    store.remember("com.b.beta", "z").unwrap();
    let (lines, ids) = compose_apps_rows(Some(&store));
    assert_eq!(
        ids,
        vec!["com.a.alpha".to_string(), "com.b.beta".to_string()]
    );
    assert_eq!(lines.len(), 2);

    // Out-of-range (stale) click: nothing deleted.
    assert!(delete_app_row_and_recompose(&store, &ids, 5).is_none());
    assert_eq!(store.count().unwrap(), 3);

    // Row 0 deletes alpha; recomposed pair stays aligned.
    let (lines2, ids2) = delete_app_row_and_recompose(&store, &ids, 0).unwrap();
    assert_eq!(ids2, vec!["com.b.beta".to_string()]);
    assert_eq!(lines2.len(), 1);
    assert!(lines2[0].contains("com.b.beta"));
    assert_eq!(store.count().unwrap(), 1);
}

#[test]
fn apps_row_ids_align_with_the_rendered_lines() {
    // Delete buttons carry a row index; resolution back to an app id
    // must use the SAME cap and order as the rendered lines, or a click
    // deletes the wrong app's history.
    let many: Vec<(String, u64)> = (0..20).map(|i| (format!("app{i:02}"), 20 - i)).collect();
    let ids = apps_row_ids(&many);
    assert_eq!(ids.len(), crate::shell::APPS_ROWS);
    assert_eq!(ids[0], "app00");
    assert_eq!(ids.len(), apps_pane_lines(&many, true).len());
    // Status lines carry no deletable rows.
    assert!(apps_row_ids(&[]).is_empty());
}

#[test]
fn apps_policy_field_index_maps_in_checkbox_order() {
    // The Apps-row checkbox tag packs (row, field); the field index must
    // map back to the SAME AppPolicyField order the AppKit layer renders
    // (APP_POLICY_FIELD_TITLES), or a toggle writes the wrong field. The
    // index count is pinned to crate::shell::APP_POLICY_FIELDS so a
    // drifting duplicate can't silently desync the two sides.
    use prefs::AppPolicyField::*;
    assert_eq!(apps_policy_field_from_index(0), Some(Enabled));
    assert_eq!(apps_policy_field_from_index(1), Some(TabDisabled));
    assert_eq!(apps_policy_field_from_index(2), Some(MidLine));
    assert_eq!(apps_policy_field_from_index(3), Some(Autocorrect));
    assert_eq!(apps_policy_field_from_index(4), Some(GrammarFix));
    // One past the last field is out of range (stale/garbled click no-ops).
    assert_eq!(
        apps_policy_field_from_index(crate::shell::APP_POLICY_FIELDS),
        None
    );
    // Every valid index resolves — the map covers all rendered checkboxes.
    for i in 0..crate::shell::APP_POLICY_FIELDS {
        assert!(apps_policy_field_from_index(i).is_some());
    }
}

#[test]
fn apps_policy_bits_resolve_per_app_overrides_in_checkbox_order() {
    // The Apps-pane checkboxes seed from these bits; each row must reflect
    // the saved per-app override (not a hard-seeded OFF), in the SAME
    // [Enabled, TabDisabled, MidLine, Autocorrect, GrammarFix] order the checkboxes use.
    use prefs::AppPolicyField::*;
    let mut prefs = prefs::Prefs {
        default_enabled: false,
        ..Default::default()
    };
    // "explicit" overrides every field ON; "inherit" has no override and so
    // falls back to defaults (default_enabled=false, globals passed below).
    prefs.set_app_policy_field("com.explicit", Enabled, true);
    prefs.set_app_policy_field("com.explicit", TabDisabled, true);
    prefs.set_app_policy_field("com.explicit", MidLine, true);
    prefs.set_app_policy_field("com.explicit", Autocorrect, true);
    prefs.set_app_policy_field("com.explicit", GrammarFix, false);
    let ids = vec!["com.explicit".to_string(), "com.inherit".to_string()];

    let bits = compose_apps_policy_bits(&prefs, &ids, false, true, true);

    assert_eq!(bits.len(), ids.len(), "one entry per row, same order/cap");
    // Explicit overrides win on every field.
    assert_eq!(bits[0], [true, true, true, true, false]);
    // Inherit: Enabled falls to default_enabled (false), TabDisabled default
    // off, MidLine to the global (false), Autocorrect and GrammarFix to globals (true).
    assert_eq!(bits[1], [false, false, false, true, true]);
}

#[test]
fn apps_pane_lines_render_counts_or_status() {
    // Apps tab: top apps by recorded-input count; honest status lines
    // when collection is off or nothing is recorded yet.
    assert_eq!(
        apps_pane_lines(&[], false),
        vec!["Input collection is off".to_string()]
    );
    assert_eq!(
        apps_pane_lines(&[], true),
        vec!["No recorded inputs yet".to_string()]
    );
    let counts = vec![
        ("com.apple.TextEdit".to_string(), 12),
        ("com.google.Chrome".to_string(), 3),
    ];
    assert_eq!(
        apps_pane_lines(&counts, true),
        vec![
            "com.apple.TextEdit \u{2014} 12".to_string(),
            "com.google.Chrome \u{2014} 3".to_string(),
        ]
    );
    // Capped at the window's row count (shared const, review-c108).
    let many: Vec<(String, u64)> = (0..20).map(|i| (format!("app{i:02}"), 20 - i)).collect();
    assert_eq!(apps_pane_lines(&many, true).len(), crate::shell::APPS_ROWS);
}

#[test]
fn setup_pane_composition_respects_the_row_limit() {
    // The window builds SETUP_ROWS labels; zip-truncation would
    // silently hide overflow rows (review-c106, c103 precedent). Pin
    // against the REAL const, not a drifting literal. Exact equality:
    // `<=` would miss a dropped row (blank line in the pane).
    let rows = crate::setup_state::setup_rows(crate::setup_state::SetupChecks {
        ax_trusted: true,
        ax_relaunch_required: false,
        screen_context_enabled: true,
        screen_recording: true,
        model_ready: true,
    });
    assert_eq!(rows.len(), crate::shell::SETUP_ROWS);
}

#[test]
fn setup_row_line_renders_readiness_glyphs() {
    // Setup tab rows: check mark when ready, cross when not.
    let ready = crate::setup_state::SetupRow {
        label: "Accessibility",
        ready: true,
        action: None,
    };
    let missing = crate::setup_state::SetupRow {
        label: "Model file",
        ready: false,
        action: None,
    };
    assert_eq!(setup_row_line(&ready), "\u{2713} Accessibility");
    assert_eq!(setup_row_line(&missing), "\u{2717} Model file");
}

#[test]
fn setup_lines_from_checks_renders_relaunch_required_after_accessibility_grant() {
    let lines = setup_lines_from_checks(crate::setup_state::SetupChecks {
        ax_trusted: true,
        ax_relaunch_required: true,
        screen_context_enabled: false,
        screen_recording: false,
        model_ready: true,
    });
    assert_eq!(
        lines,
        vec![
            "\u{2717} Relaunch app".to_string(),
            "\u{2713} Model file".to_string()
        ]
    );
}

#[test]
fn startup_key_bindings_apply_global_shortcuts_from_config() {
    let _guard = ShortcutBindingsGuard::reset();
    let config = Config::from_lookup(lookup(&[
        ("COMPME_FORCE_ACTIVATE_KEY", "cmd+96"),
        ("COMPME_TOGGLE_APP_KEY", "option+96"),
        ("COMPME_TOGGLE_GLOBAL_KEY", "shift+96"),
        ("COMPME_GRAMMAR_CHECK_KEY", "control+96"),
    ]));

    apply_startup_key_bindings(&config);

    let bindings = crate::shell::effective_shortcut_bindings();
    assert_eq!(bindings.force_activate, Some((96, 256)));
    assert_eq!(bindings.toggle_app, Some((96, 2048)));
    assert_eq!(bindings.toggle_global, Some((96, 512)));
    assert_eq!(bindings.grammar_check, Some((96, 4096)));
}

#[test]
fn accept_subscription_observes_startup_shortcuts_before_installing() {
    let _guard = ShortcutBindingsGuard::reset();
    let config = Config::from_lookup(lookup(&[
        ("COMPME_FORCE_ACTIVATE_KEY", "cmd+96"),
        ("COMPME_TOGGLE_APP_KEY", "option+96"),
        ("COMPME_TOGGLE_GLOBAL_KEY", "shift+96"),
        ("COMPME_GRAMMAR_CHECK_KEY", "control+96"),
    ]));
    let observed = RefCell::new(None);

    apply_startup_key_bindings(&config);
    let (sub, subscription_state) = subscribe_accept_with_preconfigured_bindings(true, || {
        *observed.borrow_mut() = Some(crate::shell::effective_shortcut_bindings());
        Ok(noop_accept_subscription())
    })
    .expect("subscription setup succeeds");

    assert_eq!(subscription_state, AccessibilitySubscriptions::Ready);
    drop(sub);
    let observed = observed.into_inner().expect("subscribe closure ran");
    assert_eq!(observed.force_activate, Some((96, 256)));
    assert_eq!(observed.toggle_app, Some((96, 2048)));
    assert_eq!(observed.toggle_global, Some((96, 512)));
    assert_eq!(observed.grammar_check, Some((96, 4096)));
}

#[test]
fn lifetime_line_formats_persisted_plus_session_totals() {
    // Statistics pane 4th row: lifetime totals (stats.env base merged
    // with the live session) — words and accepted only, no sparkline.
    let merged = stats::PersistedStats {
        shown: 100,
        accepted: 42,
        dismissed: 5,
        superseded: 3,
        words: 337,
    };
    assert_eq!(
        lifetime_line(&merged),
        "Lifetime 337 words \u{b7} 42 accepted"
    );
}

#[test]
fn session_usage_snapshot_uses_the_stats_wall_clock_window() {
    // Usage events are recorded with epoch milliseconds. Shutdown must query
    // the same wall-clock domain; using process-elapsed milliseconds would
    // drop every current-session event from the 30-day stats window.
    let wall_ms = 1_800_000_000_000;
    let mut usage = stats::Stats::default();
    usage.record(wall_ms, stats::Outcome::Shown);
    usage.record(wall_ms, stats::Outcome::Accepted { words: 3 });
    usage.record_latency(wall_ms, 42);

    let snapshot = session_usage_snapshot(&usage, wall_ms + 1);
    assert_eq!(snapshot.counts.shown, 1);
    assert_eq!(snapshot.counts.accepted, 1);
    assert_eq!(snapshot.words, 3);
    assert_eq!(snapshot.latency_avg, Some(42));
    assert_eq!(snapshot.latency_p95, Some(42));

    let later_wall_ms = wall_ms + 1_000;
    let later_snapshot = session_usage_snapshot(&usage, later_wall_ms);
    assert_eq!(later_snapshot, snapshot);
}

#[test]
fn dismiss_records_a_dismissed_outcome_only_while_a_ghost_is_visible() {
    // The HostEvent::Dismiss arm feeds record_dismissal with
    // engine.has_visible_suggestion(). A bare Esc with nothing showing (the
    // audit gap) is not a verdict on any suggestion, so it must leave the
    // stats untouched — counting it would skew the dismissed totals.
    let wall_ms = 1_800_000_000_000;
    let mut usage = stats::Stats::new();

    record_dismissal(&mut usage, wall_ms, false);
    assert_eq!(
        usage.counts(wall_ms).dismissed,
        0,
        "Esc with no visible ghost must not record a Dismissed outcome"
    );
    assert_eq!(usage.session_totals().counts.dismissed, 0);

    // Esc while a ghost IS visible is the real dismissal: exactly one
    // Dismissed outcome, and no other outcome kind rides along.
    record_dismissal(&mut usage, wall_ms, true);
    let counts = usage.counts(wall_ms);
    assert_eq!(counts.dismissed, 1);
    assert_eq!(
        (counts.shown, counts.accepted, counts.superseded),
        (0, 0, 0),
        "a dismissal must record nothing but the Dismissed outcome"
    );
    assert_eq!(usage.session_totals().counts.dismissed, 1);
}

#[test]
fn stats_pane_lines_render_one_sparkline_row_per_metric() {
    // Statistics pane T2: three fixed rows (shown/accepted/words), each
    // label-padded with a per-day sparkline and the span total.
    let mk = |shown: usize, accepted: usize, words: usize| stats::DayBucket {
        counts: stats::Counts {
            shown,
            accepted,
            dismissed: 0,
            superseded: 0,
        },
        words,
    };
    let buckets = [mk(0, 0, 0), mk(2, 1, 2), mk(4, 1, 5)];
    assert_eq!(
        stats_pane_lines(&buckets),
        vec![
            "Shown    \u{2581}\u{2585}\u{2588}  6",
            "Accepted \u{2581}\u{2588}\u{2588}  2",
            "Words    \u{2581}\u{2584}\u{2588}  7",
        ]
    );
}

#[test]
fn stats_range_group_indices_select_window_and_bucket_rows() {
    let now = 1_800_000_000_000;
    let mut usage = stats::Stats::default();
    usage.record(now - 13 * stats::DAY_MS, stats::Outcome::Shown);
    usage.record(
        now - 12 * stats::DAY_MS,
        stats::Outcome::Accepted { words: 2 },
    );
    usage.record(now - 2 * stats::DAY_MS, stats::Outcome::Shown);
    usage.record(now - stats::DAY_MS, stats::Outcome::Shown);
    usage.record(now, stats::Outcome::Accepted { words: 5 });

    assert_eq!(
        compose_stats_lines(&usage, now, 1, 1),
        vec![
            "Shown    \u{2585}\u{2588}  3",
            "Accepted \u{2588}\u{2588}  2",
            "Words    \u{2584}\u{2588}  7",
        ]
    );
}

#[test]
fn env_shadow_warnings_name_only_set_switch_keys() {
    // A set env var silently overrides the file a Settings switch writes
    // (env-over-file layering) — warn at startup per shadowed key.
    let warnings = env_shadow_warnings(|key| key == "COMPME_AUTOCORRECT");
    assert_eq!(
        warnings,
        vec![
            "COMPME_AUTOCORRECT is set in the environment \u{2014} Settings changes \
                 persist to config.env but the environment wins at relaunch"
                .to_string()
        ]
    );
    assert!(env_shadow_warnings(|_| false).is_empty());
    let every_warning = env_shadow_warnings(|_| true);
    for key in [
        "COMPME_ENABLED",
        "COMPME_MIDLINE",
        "COMPME_AUTOCORRECT",
        "COMPME_GRAMMAR_FIX",
        "COMPME_TRAILING_SPACE",
        "COMPME_CLIPBOARD_CONTEXT",
        "COMPME_SCREEN_CONTEXT",
        "COMPME_INSTRUCTIONS",
        "COMPME_SENDER_NAME",
        "COMPME_SENDER_EMAIL",
        "COMPME_STRENGTH",
        "COMPME_EMOJI",
        "COMPME_EMOJI_SKIN_TONE",
        "COMPME_EMOJI_GENDER",
        "COMPME_NO_COLLECT_APPS",
        "COMPME_EXCLUDED_APPS",
        "COMPME_EXCLUDED_DOMAINS",
        "COMPME_ENABLED_APPS",
        "COMPME_DISABLED_APPS",
        "COMPME_MIDLINE_ON_APPS",
        "COMPME_MIDLINE_OFF_APPS",
        "COMPME_AUTOCORRECT_ON_APPS",
        "COMPME_AUTOCORRECT_OFF_APPS",
        "COMPME_GRAMMAR_FIX_ON_APPS",
        "COMPME_GRAMMAR_FIX_OFF_APPS",
        "COMPME_THESAURUS_ON_APPS",
        "COMPME_THESAURUS_OFF_APPS",
        "COMPME_TAB_DISABLED_APPS",
        "COMPME_LICENSE_ACCEPTED",
        "COMPME_ACCEPT_WORD_KEY",
        "COMPME_ACCEPT_FULL_KEY",
        "COMPME_GRAMMAR_ACCEPT_KEY",
        "COMPME_GRAMMAR_CHECK_KEY",
    ] {
        assert!(
            every_warning.iter().any(|warning| warning.starts_with(key)),
            "{key} must warn when env shadows persisted config"
        );
    }
    assert_eq!(every_warning.len(), 36);
}

#[test]
fn startup_env_shadow_notice_lines_keep_runtime_prefix_and_unset_keys_quiet() {
    let notices = startup_env_shadow_notice_lines(|key| key == "COMPME_ACCEPT_WORD_KEY");
    assert_eq!(
        notices,
        vec![
            "compme: COMPME_ACCEPT_WORD_KEY is set in the environment \u{2014} Settings \
                 changes persist to config.env but the environment wins at relaunch"
                .to_string()
        ]
    );
    assert!(startup_env_shadow_notice_lines(|_| false).is_empty());
}

#[test]
fn force_activate_parses_documented_key_and_legacy_alias() {
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_FORCE_ACTIVATE_KEY", "ctrl+49")]))
            .force_activate_key
            .as_deref(),
        Some("ctrl+49")
    );
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_FORCE_ACTIVATE", "shift+49")]))
            .force_activate_key
            .as_deref(),
        Some("shift+49")
    );
    let config = Config::from_lookup(lookup(&[
        ("COMPME_FORCE_ACTIVATE_KEY", "ctrl+49"),
        ("COMPME_FORCE_ACTIVATE", "shift+49"),
    ]));
    assert_eq!(
        config.force_activate_key.as_deref(),
        Some("ctrl+49"),
        "documented key spelling wins over the legacy alias"
    );
    let bindings = crate::shell::ShortcutBindings::from_config(
        config.force_activate_key.as_deref(),
        None,
        None,
        None,
    );
    assert_eq!(
        bindings.force_activate,
        crate::shell::parse_accept_key("ctrl+49")
    );
}

#[test]
fn config_parses_grammar_check_and_grammar_accept_keys() {
    let config = Config::from_lookup(lookup(&[
        ("COMPME_GRAMMAR_CHECK_KEY", "cmd+shift+96"),
        ("COMPME_GRAMMAR_ACCEPT_KEY", "ctrl+96"),
    ]));
    assert_eq!(config.grammar_check_key.as_deref(), Some("cmd+shift+96"));
    assert_eq!(
        config.grammar_accept_key,
        crate::shell::parse_accept_key("ctrl+96")
    );
}

#[test]
fn config_parses_toggle_shortcut_keys_and_maps_them_to_their_own_bindings() {
    let config = Config::from_lookup(lookup(&[
        ("COMPME_TOGGLE_APP_KEY", "ctrl+48"),
        ("COMPME_TOGGLE_GLOBAL_KEY", "shift+50"),
    ]));
    assert_eq!(config.toggle_app_key.as_deref(), Some("ctrl+48"));
    assert_eq!(config.toggle_global_key.as_deref(), Some("shift+50"));
    // Empty strings must fall through to None (the .filter guard), not
    // survive as bound-but-unparseable chords.
    let empty = Config::from_lookup(lookup(&[
        ("COMPME_TOGGLE_APP_KEY", ""),
        ("COMPME_TOGGLE_GLOBAL_KEY", ""),
    ]));
    assert!(empty.toggle_app_key.is_none());
    assert!(empty.toggle_global_key.is_none());
    // Thread the keys exactly as run() does (force_activate, toggle_app,
    // toggle_global, grammar_check): distinct chords so a positional swap
    // between the two toggle slots fails.
    let bindings = crate::shell::ShortcutBindings::from_config(
        None,
        config.toggle_app_key.as_deref(),
        config.toggle_global_key.as_deref(),
        None,
    );
    assert_eq!(
        bindings.toggle_app,
        crate::shell::parse_accept_key("ctrl+48")
    );
    assert_eq!(
        bindings.toggle_global,
        crate::shell::parse_accept_key("shift+50")
    );
}

#[test]
fn env_shadow_warns_when_emoji_gender_env_shadows_persisted_setting() {
    let warnings = env_shadow_warnings(|key| key == "COMPME_EMOJI_GENDER");
    assert_eq!(
        warnings,
        vec![
            "COMPME_EMOJI_GENDER is set in the environment \u{2014} Settings changes \
                 persist to config.env but the environment wins at relaunch"
                .to_string()
        ]
    );
}

#[test]
fn trailing_space_persist_value_round_trips_through_the_parser() {
    assert!(
        Config::from_lookup(lookup(&[("COMPME_TRAILING_SPACE", switch_value(true))]))
            .trailing_space
    );
    assert!(
        !Config::from_lookup(lookup(&[("COMPME_TRAILING_SPACE", switch_value(false))]))
            .trailing_space
    );
}

#[test]
fn autocorrect_persist_value_round_trips_through_the_parser() {
    // The General-tab Autocorrect switch persists switch_value(flag);
    // the launch parser must read it back to the same bool, both ways.
    assert!(Config::from_lookup(lookup(&[("COMPME_AUTOCORRECT", switch_value(true))])).autocorrect);
    assert!(
        !Config::from_lookup(lookup(&[("COMPME_AUTOCORRECT", switch_value(false))])).autocorrect
    );
}

#[test]
fn full_autocorrect_persist_value_round_trips_through_the_parser() {
    assert!(
        Config::from_lookup(lookup(&[("COMPME_FULL_AUTOCORRECT", switch_value(true),)]))
            .full_autocorrect
    );
    assert!(
        !Config::from_lookup(lookup(&[("COMPME_FULL_AUTOCORRECT", switch_value(false),)]))
            .full_autocorrect
    );
}

#[test]
fn cross_app_previous_inputs_persist_value_round_trips_through_the_parser() {
    assert!(
        Config::from_lookup(lookup(&[(
            "COMPME_CROSS_APP_PREVIOUS_INPUTS",
            switch_value(true),
        )]))
        .cross_app_previous_inputs
    );
    assert!(
        !Config::from_lookup(lookup(&[(
            "COMPME_CROSS_APP_PREVIOUS_INPUTS",
            switch_value(false),
        )]))
        .cross_app_previous_inputs
    );
}

#[test]
fn thesaurus_selection_persist_value_round_trips_through_the_parser() {
    assert!(
        Config::from_lookup(lookup(&[(
            "COMPME_THESAURUS_SELECTION",
            switch_value(true),
        )]))
        .thesaurus_selection
    );
    assert!(
        !Config::from_lookup(lookup(&[(
            "COMPME_THESAURUS_SELECTION",
            switch_value(false),
        )]))
        .thesaurus_selection
    );
}

#[test]
fn midline_persist_value_round_trips_through_the_parser() {
    // The Labs-pane watcher persists switch_value(flag); the launch-time
    // parser must read it back to the same bool, both ways.
    assert!(Config::from_lookup(lookup(&[("COMPME_MIDLINE", switch_value(true))])).allow_mid_word);
    assert!(
        !Config::from_lookup(lookup(&[("COMPME_MIDLINE", switch_value(false))])).allow_mid_word
    );
}

#[test]
fn emoji_persist_value_round_trips_through_the_parser() {
    assert!(
        Config::from_lookup(lookup(&[("COMPME_EMOJI", switch_value(true))]))
            .emoji
            .is_some()
    );
    assert!(
        Config::from_lookup(lookup(&[("COMPME_EMOJI", switch_value(false))]))
            .emoji
            .is_none()
    );
    assert_eq!(
        Config::from_lookup(lookup(&[
            ("COMPME_EMOJI", "1"),
            ("COMPME_EMOJI_SKIN_TONE", "medium-light"),
        ]))
        .emoji
        .unwrap()
        .skin_tone,
        SkinTone::MediumLight
    );
}

#[test]
fn emoji_toggle_preserves_custom_prefs_within_the_session() {
    let mut config_emoji = Some(EmojiPrefs {
        skin_tone: SkinTone::MediumDark,
        gender: Gender::Female,
    });
    let mut saved = config_emoji.unwrap();

    apply_emoji_enabled(&mut config_emoji, &mut saved, false);
    assert!(config_emoji.is_none());

    apply_emoji_enabled(&mut config_emoji, &mut saved, true);
    assert_eq!(
        config_emoji,
        Some(EmojiPrefs {
            skin_tone: SkinTone::MediumDark,
            gender: Gender::Female,
        })
    );
}

#[test]
fn emoji_switch_edge_applies_config_and_persists_only_on_change() {
    let flag = AtomicBool::new(true);
    let mut current = true;
    let mut config_emoji = Some(EmojiPrefs {
        skin_tone: SkinTone::MediumDark,
        gender: Gender::Female,
    });
    let mut saved = config_emoji.unwrap();
    let mut persisted = Vec::new();

    assert_eq!(
        handle_emoji_switch_edge(&flag, &mut current, &mut config_emoji, &mut saved, |on| {
            persisted.push(on)
        },),
        None
    );
    assert_eq!(persisted, Vec::<bool>::new());

    flag.store(false, Ordering::Relaxed);
    assert_eq!(
        handle_emoji_switch_edge(&flag, &mut current, &mut config_emoji, &mut saved, |on| {
            persisted.push(on)
        },),
        Some(false)
    );
    assert!(config_emoji.is_none());
    assert_eq!(persisted, vec![false]);

    flag.store(true, Ordering::Relaxed);
    assert_eq!(
        handle_emoji_switch_edge(&flag, &mut current, &mut config_emoji, &mut saved, |on| {
            persisted.push(on)
        },),
        Some(true)
    );
    assert_eq!(
        config_emoji,
        Some(EmojiPrefs {
            skin_tone: SkinTone::MediumDark,
            gender: Gender::Female,
        })
    );
    assert_eq!(persisted, vec![false, true]);
}

#[test]
fn disabled_emoji_preserves_persisted_skin_tone_for_later_enable() {
    let config = Config::from_lookup(lookup(&[
        ("COMPME_EMOJI", "0"),
        ("COMPME_EMOJI_SKIN_TONE", "dark"),
        ("COMPME_EMOJI_GENDER", "female"),
    ]));
    assert_eq!(config.emoji, None);
    assert_eq!(
        config.emoji_prefs,
        EmojiPrefs {
            skin_tone: SkinTone::Dark,
            gender: Gender::Female,
        }
    );

    let flags = build_settings_flags(&config, Arc::new(AtomicBool::new(true)), false, 16);
    assert_eq!(
        flags.emoji_skin_tone_index.load(Ordering::Relaxed),
        emoji_skin_tone_index(SkinTone::Dark)
    );

    let enabled_flag = AtomicBool::new(true);
    let mut enabled = false;
    let mut config_emoji = config.emoji;
    let mut saved = config.emoji_prefs;
    handle_emoji_switch_edge(
        &enabled_flag,
        &mut enabled,
        &mut config_emoji,
        &mut saved,
        |_| {},
    );
    assert_eq!(
        config_emoji,
        Some(EmojiPrefs {
            skin_tone: SkinTone::Dark,
            gender: Gender::Female,
        })
    );
}

#[test]
fn emoji_skin_tone_edge_applies_config_and_persists_only_on_change() {
    let index = AtomicUsize::new(emoji_skin_tone_index(SkinTone::MediumDark));
    let mut current = emoji_skin_tone_index(SkinTone::MediumDark);
    let mut config_emoji = Some(EmojiPrefs {
        skin_tone: SkinTone::MediumDark,
        gender: Gender::Female,
    });
    let mut saved = config_emoji.unwrap();
    let mut persisted = Vec::new();

    assert_eq!(
        handle_emoji_skin_tone_change(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
        ),
        None
    );
    assert_eq!(persisted, Vec::<String>::new());

    index.store(emoji_skin_tone_index(SkinTone::Light), Ordering::Relaxed);
    assert_eq!(
        handle_emoji_skin_tone_change(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
        ),
        Some(SkinTone::Light)
    );
    assert_eq!(
        config_emoji,
        Some(EmojiPrefs {
            skin_tone: SkinTone::Light,
            gender: Gender::Female,
        })
    );
    assert_eq!(saved.skin_tone, SkinTone::Light);
    assert_eq!(persisted, vec!["light"]);
}

#[test]
fn emoji_skin_tone_change_persists_saved_prefs_while_emoji_disabled() {
    // config_emoji=None (Emoji disabled). Every other emoji test passes Some,
    // so moving `saved_prefs.skin_tone = tone` inside the `if let Some(prefs)`
    // block would drop persistence here yet stay green. The saved prefs must
    // update so re-enabling Emoji restores the chosen tone.
    let index = AtomicUsize::new(emoji_skin_tone_index(SkinTone::Light));
    let mut current = emoji_skin_tone_index(SkinTone::MediumDark);
    let mut config_emoji: Option<EmojiPrefs> = None;
    let mut saved = EmojiPrefs {
        skin_tone: SkinTone::MediumDark,
        gender: Gender::Female,
    };
    let mut persisted = Vec::new();
    assert_eq!(
        handle_emoji_skin_tone_change(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
        ),
        Some(SkinTone::Light)
    );
    assert!(config_emoji.is_none(), "Emoji must stay disabled");
    assert_eq!(saved.skin_tone, SkinTone::Light);
    assert_eq!(persisted, vec!["light"]);
}

#[test]
fn emoji_gender_change_persists_saved_prefs_while_emoji_disabled() {
    // Same disabled-state persistence contract for gender.
    let index = AtomicUsize::new(emoji_gender_index(Gender::Male));
    let mut current = emoji_gender_index(Gender::Female);
    let mut config_emoji: Option<EmojiPrefs> = None;
    let mut saved = EmojiPrefs {
        skin_tone: SkinTone::MediumDark,
        gender: Gender::Female,
    };
    let mut persisted = Vec::new();
    let out = handle_emoji_gender_change(
        &index,
        &mut current,
        &mut config_emoji,
        &mut saved,
        |value| persisted.push(value.to_string()),
    );
    assert_eq!(out, Some(Gender::Male));
    assert!(config_emoji.is_none(), "Emoji must stay disabled");
    assert_eq!(saved.gender, Gender::Male);
    assert_eq!(
        persisted,
        vec![emoji_gender_value(Gender::Male).to_string()]
    );
}

#[test]
fn enqueue_deep_link_bounds_queue_fifo_and_rejects_oversize() {
    // No direct test drove enqueue_deep_link's cap/oversize branches (only the
    // handle_deep_link caller was tested). Pin FIFO evict-oldest at the cap and
    // the oversize reject.
    let mut q: Vec<String> = Vec::new();
    for i in 0..MAX_DEEP_LINK_QUEUE {
        assert!(enqueue_deep_link(&mut q, format!("compme://{i}")));
    }
    assert_eq!(q.len(), MAX_DEEP_LINK_QUEUE);
    // Full queue: accept the new url, evict the OLDEST (FIFO), stay at cap.
    assert!(enqueue_deep_link(&mut q, "compme://new".into()));
    assert_eq!(q.len(), MAX_DEEP_LINK_QUEUE);
    assert_eq!(q[0], "compme://1", "oldest (compme://0) must be evicted");
    assert_eq!(q[MAX_DEEP_LINK_QUEUE - 1], "compme://new");
    // Oversize url rejected; queue untouched.
    let big = "x".repeat(MAX_DEEP_LINK_URL_CHARS + 1);
    assert!(!enqueue_deep_link(&mut q, big));
    assert_eq!(q.len(), MAX_DEEP_LINK_QUEUE);
    assert_eq!(q[MAX_DEEP_LINK_QUEUE - 1], "compme://new");
}

#[test]
fn emoji_gender_edge_applies_config_and_persists_only_on_change() {
    let index = AtomicUsize::new(emoji_gender_index(Gender::Female));
    let mut current = emoji_gender_index(Gender::Female);
    let mut config_emoji = Some(EmojiPrefs {
        skin_tone: SkinTone::Medium,
        gender: Gender::Female,
    });
    let mut saved = config_emoji.unwrap();
    let mut persisted = Vec::new();

    // No change → None, nothing persisted.
    assert_eq!(
        handle_emoji_gender_change(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
        ),
        None
    );
    assert_eq!(persisted, Vec::<String>::new());

    // Change to Male → applies to config + saved, persists "male".
    index.store(emoji_gender_index(Gender::Male), Ordering::Relaxed);
    assert_eq!(
        handle_emoji_gender_change(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
        ),
        Some(Gender::Male)
    );
    assert_eq!(
        config_emoji,
        Some(EmojiPrefs {
            skin_tone: SkinTone::Medium,
            gender: Gender::Male,
        })
    );
    assert_eq!(saved.gender, Gender::Male);
    assert_eq!(persisted, vec!["male"]);

    // Index↔value round-trip for every variant; OOB clamps to the default.
    for g in [Gender::Neutral, Gender::Female, Gender::Male] {
        assert_eq!(emoji_gender_from_index(emoji_gender_index(g)), g);
    }
    assert_eq!(emoji_gender_from_index(99), Gender::Neutral);
    assert_eq!(emoji_gender_value(Gender::Neutral), "neutral");
    assert_eq!(parse_gender(Some("male".into())), Gender::Male);
}

#[test]
fn handle_emoji_skin_tone_change_clamps_out_of_range_atomic_to_last_tone() {
    // A bogus atomic index (e.g. 99) must clamp to the last addressable tone
    // via `.min(EMOJI_SKIN_TONE_VALUES.len() - 1)` — not panic or fall back to
    // the default. The last entry is `SkinTone::Dark` ("dark").
    let index = AtomicUsize::new(99);
    let mut current = emoji_skin_tone_index(SkinTone::Default);
    let mut config_emoji = Some(EmojiPrefs::default());
    let mut saved_prefs = EmojiPrefs::default();
    let mut persisted: Option<&'static str> = None;
    let result = handle_emoji_skin_tone_change(
        &index,
        &mut current,
        &mut config_emoji,
        &mut saved_prefs,
        |value| persisted = Some(value),
    );
    assert_eq!(result, Some(SkinTone::Dark));
    assert_eq!(current, emoji_skin_tone_index(SkinTone::Dark));
    assert_eq!(saved_prefs.skin_tone, SkinTone::Dark);
    assert_eq!(config_emoji.unwrap().skin_tone, SkinTone::Dark);
    assert_eq!(persisted, Some("dark"));
}

#[test]
fn handle_emoji_gender_change_clamps_out_of_range_atomic_to_last_gender() {
    // Gender twin of the skin-tone clamp test: a bogus atomic index clamps to
    // the last gender via `.min(EMOJI_GENDER_VALUES.len() - 1)`. The last entry
    // is `Gender::Male` ("male").
    let index = AtomicUsize::new(99);
    let mut current = emoji_gender_index(Gender::Neutral);
    let mut config_emoji = Some(EmojiPrefs::default());
    let mut saved_prefs = EmojiPrefs::default();
    let mut persisted: Option<&'static str> = None;
    let result = handle_emoji_gender_change(
        &index,
        &mut current,
        &mut config_emoji,
        &mut saved_prefs,
        |value| persisted = Some(value),
    );
    assert_eq!(result, Some(Gender::Male));
    assert_eq!(current, emoji_gender_index(Gender::Male));
    assert_eq!(saved_prefs.gender, Gender::Male);
    assert_eq!(config_emoji.unwrap().gender, Gender::Male);
    assert_eq!(persisted, Some("male"));
}

#[test]
fn emoji_gender_edge_invalidates_stale_visible_suggestion() {
    let index = AtomicUsize::new(emoji_gender_index(Gender::Male));
    let mut current = emoji_gender_index(Gender::Neutral);
    let mut config_emoji = Some(EmojiPrefs::default());
    let mut saved = EmojiPrefs::default();
    let mut persisted = Vec::new();
    let mut invalidations = 0;

    assert_eq!(
        handle_emoji_gender_change_with_invalidation(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
            || invalidations += 1,
        ),
        Some(Gender::Male)
    );
    assert_eq!(persisted, vec!["male"]);
    assert_eq!(invalidations, 1);
}

#[test]
fn emoji_skin_tone_edge_invalidates_stale_visible_suggestion() {
    let index = AtomicUsize::new(emoji_skin_tone_index(SkinTone::Dark));
    let mut current = emoji_skin_tone_index(SkinTone::Default);
    let mut config_emoji = Some(EmojiPrefs::default());
    let mut saved = EmojiPrefs::default();
    let mut persisted = Vec::new();
    let mut invalidations = 0;

    assert_eq!(
        handle_emoji_skin_tone_change_with_invalidation(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
            || invalidations += 1,
        ),
        Some(SkinTone::Dark)
    );
    assert_eq!(persisted, vec!["dark"]);
    assert_eq!(invalidations, 1);
}

#[test]
fn emoji_edges_with_unchanged_index_never_invalidate_the_visible_suggestion() {
    // A no-op popup edit (same index as `current`) must NOT clear the
    // showing suggestion: `handle_*_change` short-circuits to `None`, so the
    // `_with_invalidation` wrapper never runs `invalidate_visible_suggestion`.
    // A changed index proves the invalidation path is still wired. Mirrors
    // `emoji_{gender,skin_tone}_edge_invalidates_stale_visible_suggestion`.

    // --- Skin tone: same index → no invalidation, no persist, None. ---
    let index = AtomicUsize::new(emoji_skin_tone_index(SkinTone::Medium));
    let mut current = emoji_skin_tone_index(SkinTone::Medium);
    let mut config_emoji = Some(EmojiPrefs::default());
    let mut saved = EmojiPrefs::default();
    let mut persisted = Vec::new();
    let mut invalidations = 0;

    assert_eq!(
        handle_emoji_skin_tone_change_with_invalidation(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
            || invalidations += 1,
        ),
        None,
        "unchanged skin-tone index is a no-op edit"
    );
    assert_eq!(persisted, Vec::<String>::new());
    assert_eq!(
        invalidations, 0,
        "a no-op skin-tone edit must not clear the showing suggestion"
    );

    // A genuine change DOES invalidate, proving the path is still wired.
    index.store(emoji_skin_tone_index(SkinTone::Dark), Ordering::Relaxed);
    assert_eq!(
        handle_emoji_skin_tone_change_with_invalidation(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
            || invalidations += 1,
        ),
        Some(SkinTone::Dark)
    );
    assert_eq!(invalidations, 1);

    // --- Gender: same index → no invalidation, no persist, None. ---
    let index = AtomicUsize::new(emoji_gender_index(Gender::Female));
    let mut current = emoji_gender_index(Gender::Female);
    let mut config_emoji = Some(EmojiPrefs::default());
    let mut saved = EmojiPrefs::default();
    let mut persisted = Vec::new();
    let mut invalidations = 0;

    assert_eq!(
        handle_emoji_gender_change_with_invalidation(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
            || invalidations += 1,
        ),
        None,
        "unchanged gender index is a no-op edit"
    );
    assert_eq!(persisted, Vec::<String>::new());
    assert_eq!(
        invalidations, 0,
        "a no-op gender edit must not clear the showing suggestion"
    );

    // A genuine change DOES invalidate.
    index.store(emoji_gender_index(Gender::Male), Ordering::Relaxed);
    assert_eq!(
        handle_emoji_gender_change_with_invalidation(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
            || invalidations += 1,
        ),
        Some(Gender::Male)
    );
    assert_eq!(invalidations, 1);
}

#[test]
fn snooze_duration_matches_the_rendered_wording() {
    // AppStatus::render_line says "Snoozed for up to 1 hour" (a &'static
    // str). If SNOOZE_MINUTES ever changes, that wording must follow.
    assert_eq!(
        SNOOZE_MINUTES, 60,
        "update AppStatus::render_line's 'up to 1 hour' wording (status.rs) \
             together with SNOOZE_MINUTES"
    );
    assert!(AppStatus::Ready.render_line(true).contains("1 hour"));
}

#[test]
fn deep_links_apply_overrides_and_fail_closed() {
    let mut prefs = Prefs::default();
    // A valid unsigned exclude applies and names the action.
    let summary = handle_deep_link(
        "compme://setOverride?app=com.apple.TextEdit&excluded=true",
        None,
        &mut prefs,
        |_| true,
    )
    .expect("valid link applies");
    assert!(summary.contains("com.apple.TextEdit"), "{summary}");
    assert!(prefs.excluded_apps.contains("com.apple.TextEdit"));
    // Garbage fails with the parser's message, prefs untouched.
    let err = handle_deep_link("compme://setEverything?x=1", None, &mut prefs, |_| true)
        .expect_err("unknown command must fail");
    assert!(err.contains("unknown command"), "{err}");
    // A signed link without a configured trusted key fails closed.
    let err = handle_deep_link(
        &format!(
            "compme://setOverride?app=com.apple.TextEdit&enabled=true&sig={}",
            "ab".repeat(64)
        ),
        None,
        &mut prefs,
        |_| true,
    )
    .expect_err("signed link without a key must fail");
    assert!(err.contains("no trusted key"), "{err}");
}

#[test]
fn only_a_verified_signed_deep_link_reaches_confirmation_and_mutates_prefs() {
    // RFC 8032-compatible deterministic fixture: private seed [7; 32].
    // Keeping the public key, payload, and signature as literals makes the
    // expected trust decision independent of the production signer/parser.
    let trusted = webconfig::TrustedKey::from_hex(
        "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
    )
    .expect("fixture public key");
    let signed = concat!(
        "compme://setOverride?app=com.apple.TextEdit&excluded=true",
        "&sig=721848ed25850b98440cdb91f5077077b8f1077446be885c3b8c6b3c3a2a986f",
        "8884b34489c675afdc344af112d58251f8df40098903d97a861605baa667a005",
    );
    let mut prefs = Prefs::default();
    let confirmations = RefCell::new(Vec::new());

    let summary = handle_deep_link(signed, Some(&trusted), &mut prefs, |decision| {
        confirmations.borrow_mut().push(decision.clone());
        true
    })
    .expect("a valid signed link should reach host confirmation and apply");

    assert_eq!(
        confirmations.into_inner(),
        vec![webconfig::PromptDecision {
            scope: "com.apple.TextEdit".to_string(),
            action: "Exclude".to_string(),
            trust: "signed link, verified".to_string(),
        }],
    );
    assert!(summary.contains("Signed link"), "{summary}");
    assert!(prefs.excluded_apps.contains("com.apple.TextEdit"));

    // Changing the signed scope without resigning must fail before the host
    // prompts or mutates the already-established policy.
    let before = prefs.clone();
    let tampered = signed.replace("com.apple.TextEdit", "com.apple.Mail");
    let prompted = Cell::new(false);
    let err = handle_deep_link(&tampered, Some(&trusted), &mut prefs, |_| {
        prompted.set(true);
        true
    })
    .expect_err("tampered payload must fail closed");
    assert_eq!(err, "signature verification failed");
    assert!(
        !prompted.get(),
        "unverified payload must not reach confirmation"
    );
    assert_eq!(prefs, before, "unverified payload must not mutate policy");
}

#[test]
fn accept_key_config_parses_keycodes_and_rejects_junk() {
    // Raw macOS virtual keycodes (the future shortcuts-pane recorder
    // emits keycodes too); junk → None → default bindings.
    let config = Config::from_lookup(lookup(&[
        ("COMPME_ACCEPT_WORD_KEY", "122"),
        ("COMPME_ACCEPT_FULL_KEY", "120"),
    ]));
    assert_eq!(config.accept_word_key, Some((122, 0)));
    assert_eq!(config.accept_full_key, Some((120, 0)));
    let junk = Config::from_lookup(lookup(&[("COMPME_ACCEPT_WORD_KEY", "tab")]));
    assert_eq!(junk.accept_word_key, None);
    assert_eq!(Config::from_lookup(lookup(&[])).accept_word_key, None);
    // Modifier combos parse into (keycode, Carbon mask): shift=512, ctrl=4096
    // (slice 1b — Shift+Tab etc. configurable via the persisted string).
    let combo = Config::from_lookup(lookup(&[
        ("COMPME_ACCEPT_WORD_KEY", "shift+48"),
        ("COMPME_ACCEPT_FULL_KEY", "ctrl+shift+50"),
    ]));
    assert_eq!(combo.accept_word_key, Some((48, 512)));
    assert_eq!(combo.accept_full_key, Some((50, 512 | 4096)));
}

#[test]
fn emoji_and_memory_config_synonyms_map_to_the_right_variant() {
    // The enum/synonym parse arms each fall to a SAFE default on no-match, so
    // a dropped/typo'd arm is silent — pin every documented synonym + the
    // trim/case handling + the default fallback.
    assert_eq!(parse_gender(Some("male".into())), Gender::Male);
    assert_eq!(parse_gender(Some(" Female ".into())), Gender::Female);
    assert_eq!(parse_gender(Some("nonbinary".into())), Gender::Neutral);

    assert_eq!(parse_skin_tone(Some("light".into())), SkinTone::Light);
    assert_eq!(parse_skin_tone(Some("medium".into())), SkinTone::Medium);
    assert_eq!(
        parse_skin_tone(Some("medium_light".into())),
        SkinTone::MediumLight
    );
    assert_eq!(
        parse_skin_tone(Some("medium_dark".into())),
        SkinTone::MediumDark
    );
    assert_eq!(parse_skin_tone(Some("dark".into())), SkinTone::Dark);
    assert_eq!(parse_skin_tone(Some("bogus".into())), SkinTone::Default);

    // The third storage-mode alias `all_monitored` (siblings `all`/`monitored`
    // are already pinned elsewhere).
    assert_eq!(
        parse_storage_mode(Some("all_monitored".into())),
        memory::StorageMode::AllMonitored
    );

    // Tri-state truthy/falsy synonyms + trim/case + unrecognized → None.
    for v in ["1", "true", "on", "yes", " YES "] {
        assert_eq!(parse_tri_state(Some(v.into())), Some(true), "{v}");
    }
    for v in ["0", "false", "off", "no", " No "] {
        assert_eq!(parse_tri_state(Some(v.into())), Some(false), "{v}");
    }
    assert_eq!(parse_tri_state(Some("maybe".into())), None);
    assert_eq!(parse_tri_state(None), None);
}

#[test]
fn a_declined_prompt_rejects_the_link_and_leaves_prefs_untouched() {
    let mut prefs = Prefs::default();
    let err = handle_deep_link(
        "compme://setOverride?app=com.apple.TextEdit&excluded=true",
        None,
        &mut prefs,
        |_| false, // user clicks Cancel
    )
    .expect_err("declined prompt must reject");
    assert!(err.contains("declined"), "{err}");
    assert!(prefs.excluded_apps.is_empty(), "prefs must be untouched");
}

#[test]
fn launch_at_login_applies_only_when_the_key_is_explicitly_set() {
    // Absent: leave the user's Login Items setting alone.
    assert_eq!(Config::from_lookup(lookup(&[])).launch_at_login, None);
    // Explicit true/false apply.
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_LAUNCH_AT_LOGIN", "true")])).launch_at_login,
        Some(true)
    );
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_LAUNCH_AT_LOGIN", "0")])).launch_at_login,
        Some(false)
    );
    // Junk fails safe to leave-alone, NOT to a register/unregister.
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_LAUNCH_AT_LOGIN", "maybe")])).launch_at_login,
        None
    );
}

struct LaunchAtLoginHost {
    result: Result<(), PlatformError>,
    calls: Mutex<Vec<bool>>,
}

impl platform::shell::ShellHost for LaunchAtLoginHost {
    fn pump_events(&self, _heartbeat: Duration) {}
    fn physical_memory_bytes(&self) -> u64 {
        1
    }
    fn open_url(&self, _url: &str) -> Result<(), PlatformError> {
        Ok(())
    }
    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        Ok(())
    }
    fn reveal_file(&self, _path: &std::path::Path) -> Result<(), PlatformError> {
        Ok(())
    }
    fn set_launch_at_login(&self, enabled: bool) -> Result<(), PlatformError> {
        self.calls.lock().unwrap().push(enabled);
        self.result.clone()
    }
    fn confirm(&self, _prompt: &shell_flags::ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
        Ok(false)
    }
    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
        Err(PlatformError::UnsupportedField {
            reason: "test".into(),
        })
    }
}

#[test]
fn launch_at_login_settings_edge_applies_then_persists() {
    let flag = AtomicBool::new(true);
    let mut current = false;
    let host = LaunchAtLoginHost {
        result: Ok(()),
        calls: Mutex::new(Vec::new()),
    };
    let persisted = RefCell::new(Vec::new());

    assert_eq!(
        apply_launch_at_login_settings_edge(&flag, &mut current, &host, |on| {
            persisted.borrow_mut().push(on)
        }),
        Ok(Some(true))
    );
    assert!(current);
    assert_eq!(host.calls.lock().unwrap().as_slice(), &[true]);
    assert_eq!(persisted.borrow().as_slice(), &[true]);
    assert_eq!(
        apply_launch_at_login_settings_edge(&flag, &mut current, &host, |_| unreachable!()),
        Ok(None)
    );

    flag.store(false, Ordering::Relaxed);
    assert_eq!(
        apply_launch_at_login_settings_edge(&flag, &mut current, &host, |on| {
            persisted.borrow_mut().push(on)
        }),
        Ok(Some(false))
    );
    assert!(!current);
    assert_eq!(host.calls.lock().unwrap().as_slice(), &[true, false]);
    assert_eq!(persisted.borrow().as_slice(), &[true, false]);
}

#[test]
fn launch_at_login_settings_edge_restores_state_and_does_not_persist_on_failure() {
    let flag = AtomicBool::new(true);
    let mut current = false;
    let host = LaunchAtLoginHost {
        result: Err(PlatformError::CannotComplete {
            reason: "registration denied".into(),
        }),
        calls: Mutex::new(Vec::new()),
    };
    let persisted = RefCell::new(Vec::new());

    assert!(
        apply_launch_at_login_settings_edge(&flag, &mut current, &host, |on| {
            persisted.borrow_mut().push(on)
        })
        .is_err()
    );
    assert!(!current);
    assert!(!flag.load(Ordering::Relaxed));
    assert_eq!(host.calls.lock().unwrap().as_slice(), &[true]);
    assert!(persisted.borrow().is_empty());
}

#[test]
fn per_app_autocorrect_override_gates_the_replacement_offer() {
    // Global autocorrect ON, per-app OFF: the typo fix must not offer in
    // that app, while emoji (an unrelated feature) still does elsewhere.
    let config = Config::from_lookup(lookup(&[
        ("COMPME_AUTOCORRECT", "1"),
        ("COMPME_AUTOCORRECT_OFF_APPS", "com.quiet.app"),
    ]));
    // `teh` is a known typo; in the opted-out app no offer fires…
    assert_eq!(
        replacement_decision(
            "teh",
            &config,
            &config.prefs,
            Some("com.quiet.app"),
            None,
            true,
            0
        ),
        None
    );
    // …but the same input in another app offers the fix.
    assert!(replacement_decision(
        "teh",
        &config,
        &config.prefs,
        Some("com.other.app"),
        None,
        true,
        0
    )
    .is_some());
}

#[test]
fn per_app_thesaurus_override_survives_persistence_and_gates_the_offer() {
    let configured = Config::from_lookup(lookup(&[
        ("COMPME_THESAURUS", "0"),
        (
            "COMPME_THESAURUS_ON_APPS",
            "com.example.writer,com.example.conflict",
        ),
        ("COMPME_THESAURUS_OFF_APPS", "com.example.conflict"),
    ]));
    let dir = std::env::temp_dir().join(format!(
        "compme-per-app-thesaurus-persist-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("config.env");

    // Any prefs persistence edge rewrites every per-app policy category.
    // The config-only thesaurus override must survive that rewrite too.
    persist_web_override_prefs(&path, &configured.prefs);
    let map = config::load_file_map(&path).expect("reload persisted prefs");
    let reloaded = Config::from_lookup(|key| {
        (key == "COMPME_THESAURUS")
            .then(|| "0".to_string())
            .or_else(|| map.get(key).cloned())
    });

    assert_eq!(
        replacement_decision(
            "happy",
            &reloaded,
            &reloaded.prefs,
            Some("com.example.writer"),
            None,
            true,
            0,
        ),
        Some((
            vec![
                "glad".to_string(),
                "joyful".to_string(),
                "cheerful".to_string(),
                "content".to_string(),
                "pleased".to_string(),
                "delighted".to_string(),
            ],
            5,
        )),
        "the opted-in app should get the observable synonym candidates",
    );
    assert_eq!(
        replacement_decision(
            "happy",
            &reloaded,
            &reloaded.prefs,
            Some("com.example.other"),
            None,
            true,
            0,
        ),
        None,
        "an unconfigured app should inherit the global off state",
    );
    assert_eq!(
        replacement_decision(
            "happy",
            &reloaded,
            &reloaded.prefs,
            Some("com.example.conflict"),
            None,
            true,
            0,
        ),
        None,
        "the explicit per-app off list should win a conflicting on entry",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toggling_app_collection_flips_state_and_serializes_stably() {
    let mut prefs = Prefs::default();
    // First toggle: disable.
    assert!(!toggle_app_collection(&mut prefs, "com.apple.TextEdit"));
    assert!(!prefs.collection_allowed(Some("com.apple.TextEdit")));
    // Stable sorted persistence value.
    assert!(!toggle_app_collection(&mut prefs, "com.apple.Finder"));
    assert_eq!(
        no_collect_apps_value(&prefs),
        "com.apple.Finder,com.apple.TextEdit"
    );
    // Second toggle: re-enable; value shrinks.
    assert!(toggle_app_collection(&mut prefs, "com.apple.Finder"));
    assert_eq!(no_collect_apps_value(&prefs), "com.apple.TextEdit");
}

#[test]
fn collection_disabled_skips_both_recording_sinks() {
    // Per-app "Input Collection off" must silence BOTH sinks: the
    // previous-inputs context and the encrypted memory store.
    let previous = PreviousInputs::default();
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([3u8; 32]),
        memory::StorageMode::AcceptedOnly,
    )
    .expect("store");
    let field = FieldHandle {
        app: "com.apple.TextEdit".into(),
        pid: Some(42),
        element_id: "ax:1".into(),
        generation: 1,
    };

    record_full_accept(
        AcceptAction::Full,
        &field,
        "hello world",
        AcceptRecording {
            context_max_chars: 100,
            cross_app_previous_inputs: false,
            previous_inputs: &previous,
            memory: Some(&store),
            collection_allowed: false,
        },
    );
    assert_eq!(store.count().expect("count"), 0, "memory must not record");
    assert!(
        previous.recent("com.apple.TextEdit").is_empty(),
        "previous-inputs must not record"
    );

    // Sanity: allowed -> both record.
    record_full_accept(
        AcceptAction::Full,
        &field,
        "hello world",
        AcceptRecording {
            context_max_chars: 100,
            cross_app_previous_inputs: false,
            previous_inputs: &previous,
            memory: Some(&store),
            collection_allowed: true,
        },
    );
    assert_eq!(store.count().expect("count"), 1);
    assert!(!previous.recent("com.apple.TextEdit").is_empty());
}

#[test]
fn failed_accept_records_no_context_memory_or_accept_stats() {
    let previous = PreviousInputs::default();
    let store = accepted_store();
    let mut tracker = FieldTracker::new();
    let mut usage = stats::Stats::new();
    let prefs = Prefs::default();
    let preview = (
        field_with_app("com.apple.TextEdit"),
        "never inserted secret".to_string(),
        0usize,
    );

    apply_accept_side_effects(
        false,
        AcceptSideEffects {
            action: AcceptAction::Full,
            preview: Some(&preview),
            correction_preview: None,
            range_preview: None,
            wall_ms: 10_000,
            context_max_chars: 160,
            cross_app_previous_inputs: false,
            previous_inputs: &previous,
            memory: Some(&store),
            prefs: &prefs,
            tracker: &mut tracker,
            usage: &mut usage,
        },
    );

    assert_eq!(store.count().unwrap(), 0);
    assert!(previous.recent("com.apple.TextEdit").is_empty());
    let totals = usage.session_totals();
    assert_eq!(totals.counts.accepted, 0);
    assert_eq!(totals.words, 0);
}

#[test]
fn accept_commit_status_distinguishes_cleanup_from_insert_failure() {
    let requests: Vec<CompletionRequest> = Vec::new();
    assert!(accept_mutation_committed(&Ok(requests)));
    assert!(accept_mutation_committed::<Vec<CompletionRequest>>(&Err(
        engine::AcceptError {
            error: PlatformError::Timeout,
            committed: true,
        }
    )));
    assert!(!accept_mutation_committed::<Vec<CompletionRequest>>(&Err(
        engine::AcceptError {
            error: PlatformError::StaleField,
            committed: false,
        }
    )));
}

#[test]
fn committed_cleanup_failure_records_context_memory_and_accept_stats_once() {
    let previous = PreviousInputs::default();
    let store = accepted_store();
    let mut tracker = FieldTracker::new();
    let mut usage = stats::Stats::new();
    let prefs = Prefs::default();
    let preview = (
        field_with_app("com.apple.TextEdit"),
        "accepted words".to_string(),
        0usize,
    );

    let cleanup_failure: Result<Vec<CompletionRequest>, engine::AcceptError> =
        Err(engine::AcceptError {
            error: PlatformError::Timeout,
            committed: true,
        });
    apply_accept_side_effects(
        accept_mutation_committed(&cleanup_failure),
        AcceptSideEffects {
            action: AcceptAction::Full,
            preview: Some(&preview),
            correction_preview: None,
            range_preview: None,
            wall_ms: 10_000,
            context_max_chars: 160,
            cross_app_previous_inputs: false,
            previous_inputs: &previous,
            memory: Some(&store),
            prefs: &prefs,
            tracker: &mut tracker,
            usage: &mut usage,
        },
    );

    assert_eq!(store.count().unwrap(), 1);
    assert_eq!(previous.recent("com.apple.TextEdit").len(), 1);
    let totals = usage.session_totals();
    assert_eq!(totals.counts.accepted, 1);
    assert_eq!(totals.words, 2);
}

#[test]
fn replacement_accept_absorbs_the_delete_then_insert_echo() {
    // A REPLACEMENT accept (`replace_left > 0`, e.g. an emoji `:smile`→😄
    // swap) routes to `apply_self_replace`, which deletes the typed token
    // before inserting. The tracker baseline must end delete-then-inserted so
    // the field's own AX readback is absorbed as a caret move — not mistaken
    // for fresh typing. The two existing accept tests use `replace_left == 0`
    // (append-only), leaving this branch unexercised.
    let previous = PreviousInputs::default();
    let store = accepted_store();
    let mut tracker = FieldTracker::new();
    let mut usage = stats::Stats::new();
    let prefs = Prefs::default();
    let field = field_with_app("com.apple.TextEdit");

    // Seed a baseline of "x:smile" (caret at 7) so the replace branch has a
    // baseline to delete-then-insert against.
    tracker.observe(
        &field,
        &text_context(&field, "x:smile"),
        TriggerPolicy::Automatic,
        0,
    );

    let preview = (field.clone(), "😄".to_string(), 6usize);
    apply_accept_side_effects(
        true,
        AcceptSideEffects {
            action: AcceptAction::Full,
            preview: Some(&preview),
            correction_preview: None,
            range_preview: None,
            wall_ms: 10_000,
            context_max_chars: 160,
            cross_app_previous_inputs: false,
            previous_inputs: &previous,
            memory: Some(&store),
            prefs: &prefs,
            tracker: &mut tracker,
            usage: &mut usage,
        },
    );

    // The replace deleted ":smile" and inserted "😄": the baseline now reads
    // "x😄" (caret at 2). The field's matching readback must absorb as a pure
    // caret move with no spurious echo armed.
    let observed = tracker.observe(
        &field,
        &text_context(&field, "x😄"),
        TriggerPolicy::Automatic,
        1,
    );
    assert_eq!(
        observed,
        Observation::CaretMoved {
            field: field.clone(),
            caret: 2,
        },
        "replacement accept must leave the baseline delete-then-inserted"
    );
    // Sanity: the accept still recorded its stats and sinks.
    assert_eq!(store.count().unwrap(), 1);
    assert_eq!(usage.session_totals().counts.accepted, 1);
}

#[test]
fn correction_accept_absorbs_exact_range_echo_and_records_stats() {
    let previous = PreviousInputs::default();
    let store = accepted_store();
    let mut tracker = FieldTracker::new();
    let mut usage = stats::Stats::new();
    let prefs = Prefs::default();
    let field = field_with_app("com.apple.TextEdit");
    tracker.observe(
        &field,
        &text_context(&field, "I saw teh"),
        TriggerPolicy::Automatic,
        0,
    );
    let correction = (
        field.clone(),
        "the".to_string(),
        CorrectionRange { start: 6, end: 9 },
    );

    apply_accept_side_effects(
        true,
        AcceptSideEffects {
            action: AcceptAction::Correction,
            preview: None,
            correction_preview: Some(&correction),
            range_preview: None,
            wall_ms: 10_000,
            context_max_chars: 160,
            cross_app_previous_inputs: false,
            previous_inputs: &previous,
            memory: Some(&store),
            prefs: &prefs,
            tracker: &mut tracker,
            usage: &mut usage,
        },
    );

    let observed = tracker.observe(
        &field,
        &text_context(&field, "I saw the"),
        TriggerPolicy::Automatic,
        1,
    );
    assert_eq!(
        observed,
        Observation::CaretMoved {
            field: field.clone(),
            caret: 9,
        }
    );
    let totals = usage.session_totals();
    assert_eq!(totals.counts.accepted, 1);
    assert_eq!(totals.words, 1);
    assert_eq!(
        store.count().unwrap(),
        0,
        "corrections are not full accepts"
    );
    assert!(previous.recent("com.apple.TextEdit").is_empty());
}

#[test]
fn selection_replacement_absorbs_exact_range_echo_and_records_stats() {
    let previous = PreviousInputs::default();
    let store = accepted_store();
    let mut tracker = FieldTracker::new();
    let mut usage = stats::Stats::new();
    let prefs = Prefs::default();
    let field = field_with_app("com.apple.TextEdit");
    tracker.observe(
        &field,
        &text_context(&field, "I am happy"),
        TriggerPolicy::Automatic,
        0,
    );
    let replacement = (
        field.clone(),
        "glad".to_string(),
        CorrectionRange { start: 5, end: 10 },
    );

    apply_accept_side_effects(
        true,
        AcceptSideEffects {
            action: AcceptAction::Full,
            preview: None,
            correction_preview: None,
            range_preview: Some(&replacement),
            wall_ms: 10_000,
            context_max_chars: 160,
            cross_app_previous_inputs: false,
            previous_inputs: &previous,
            memory: Some(&store),
            prefs: &prefs,
            tracker: &mut tracker,
            usage: &mut usage,
        },
    );

    let observed = tracker.observe(
        &field,
        &text_context(&field, "I am glad"),
        TriggerPolicy::Automatic,
        1,
    );
    assert_eq!(
        observed,
        Observation::CaretMoved {
            field: field.clone(),
            caret: 9,
        }
    );
    let totals = usage.session_totals();
    assert_eq!(totals.counts.accepted, 1);
    assert_eq!(totals.words, 1);
    assert_eq!(
        store.count().unwrap(),
        0,
        "selection replacements are not full completion accepts"
    );
    assert!(previous.recent("com.apple.TextEdit").is_empty());
}

#[test]
fn correction_accept_absorbs_app_normalized_readback_as_caret_move() {
    // The app normalized the landed correction on write (here it
    // autocapitalized "the" to "The"), so the next AX readback differs from
    // the text the correction intended. It is still the accept's own echo —
    // not new typing — so it must absorb as a caret move. Before the
    // one-shot resync, the tracker seeded the INTENDED "the" and the
    // normalized "The" readback diffed into a synthetic same-length change,
    // arming a spurious request and routing "The" into monitored memory.
    let previous = PreviousInputs::default();
    let store = accepted_store();
    let mut tracker = FieldTracker::new();
    let mut usage = stats::Stats::new();
    let prefs = Prefs::default();
    let field = field_with_app("com.apple.TextEdit");
    tracker.observe(
        &field,
        &text_context(&field, "I saw teh"),
        TriggerPolicy::Automatic,
        0,
    );
    let correction = (
        field.clone(),
        "the".to_string(),
        CorrectionRange { start: 6, end: 9 },
    );

    apply_accept_side_effects(
        true,
        AcceptSideEffects {
            action: AcceptAction::Correction,
            preview: None,
            correction_preview: Some(&correction),
            range_preview: None,
            wall_ms: 10_000,
            context_max_chars: 160,
            cross_app_previous_inputs: false,
            previous_inputs: &previous,
            memory: Some(&store),
            prefs: &prefs,
            tracker: &mut tracker,
            usage: &mut usage,
        },
    );

    // Readback is the app-normalized form ("The"), NOT the intended "the".
    let observed = tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, "I saw The"),
        TriggerPolicy::Automatic,
        1,
    );
    match observed {
        Observation::CaretMoved { caret, .. } => assert_eq!(caret, 9),
        Observation::Typed(change) => panic!(
            "normalized correction echo must absorb as a caret move, not \
                 synthesize typing (inserted_text={:?})",
            change.inserted_text
        ),
    }
}

#[test]
fn completion_outcome_log_line_never_includes_candidate_text() {
    let line = completion_outcome_log_line(7, &["secret phrase".into(), "other".into()]);

    assert!(line.contains("gen=7"));
    assert!(line.contains("candidate_count=2"));
    assert!(line.contains("candidate_lengths=[13, 5]"));
    assert!(
        !line.contains("secret phrase"),
        "diagnostics must not emit raw completion text"
    );
}

#[test]
fn replacement_debug_log_line_redacts_left_context() {
    let secret = "sk-abcdEFGH0123456789abcdEFGH0123";
    let line =
        replacement_debug_log_line(&format!("token {secret}"), true, false, false, true, "None");

    assert!(line.contains("left="));
    assert!(!line.contains(secret));
    assert!(line.contains("[redacted-secret]"));
}

#[test]
fn blocked_request_log_line_reports_gate_metadata_without_prompt_text() {
    let prefs = Prefs::default();
    let request = req_with_prompt("git status && print-secret");
    let line = request_log_line(
        &request,
        Some("com.apple.Terminal"),
        None,
        &prefs,
        1_000,
        Some("print-secret"),
        true,
    );

    assert!(line.contains("request blocked"));
    assert!(line.contains("prompt_chars=26"));
    assert!(line.contains("app=com.apple.Terminal"));
    assert!(line.contains("terminal_ok=false"));
    assert!(line.contains("prompt_marker=true"));
    assert!(!line.contains("git status"));
    assert!(!line.contains("print-secret"));
}

#[test]
fn app_disable_arms_map_to_the_right_prefs_mutation() {
    let mut prefs = Prefs::default();
    // Hour: per-app snooze for 60 minutes, auto-resuming.
    apply_app_disable(DisableArm::Hour, "com.apple.TextEdit", &mut prefs, 1_000);
    assert!(prefs.is_app_snoozed("com.apple.TextEdit", 1_000 + 59 * 60_000));
    assert!(!prefs.is_app_snoozed("com.apple.TextEdit", 1_000 + 60 * 60_000));
    // UntilRelaunch: saturated deadline, session-only.
    apply_app_disable(
        DisableArm::UntilRelaunch,
        "com.apple.Safari",
        &mut prefs,
        1_000,
    );
    assert!(prefs.is_app_snoozed("com.apple.Safari", u64::MAX - 1));
    // Always: hard exclude (persisted by the caller).
    apply_app_disable(
        DisableArm::Always,
        "com.googlecode.iterm2",
        &mut prefs,
        1_000,
    );
    assert!(prefs.excluded_apps.contains("com.googlecode.iterm2"));
    // Excluded-apps persistence value: stable comma-joined sorted list,
    // round-trippable through the COMPME_EXCLUDED_APPS parser.
    prefs.excluded_apps.insert("com.apple.Finder".into());
    assert_eq!(
        excluded_apps_value(&prefs),
        "com.apple.Finder,com.googlecode.iterm2"
    );
}

#[test]
fn snooze_request_snoozes_for_an_hour_and_is_consumed() {
    let mut prefs = Prefs::default();
    // Not requested → untouched.
    assert!(!apply_snooze_request(false, &mut prefs, 1_000));
    assert!(!prefs.is_snoozed(1_000));
    // Requested → snoozed for exactly SNOOZE_MINUTES from now.
    assert!(apply_snooze_request(true, &mut prefs, 1_000));
    assert!(prefs.is_snoozed(1_000));
    assert!(prefs.is_snoozed(1_000 + 59 * 60 * 1_000));
    assert!(!prefs.is_snoozed(1_000 + 60 * 60 * 1_000));
}

#[test]
fn config_enabled_reads_compme_enabled_and_defaults_on() {
    // The global tray-toggle state, persisted on toggle and read back at
    // launch. Distinct from COMPME_DEFAULT_ENABLED (the per-app
    // suggestion-policy default in prefs).
    assert!(Config::from_lookup(lookup(&[])).enabled);
    assert!(Config::from_lookup(lookup(&[("COMPME_ENABLED", "true")])).enabled);
    assert!(!Config::from_lookup(lookup(&[("COMPME_ENABLED", "false")])).enabled);
    assert!(!Config::from_lookup(lookup(&[("COMPME_ENABLED", "0")])).enabled);
}

#[test]
fn clipboard_and_screen_context_flags_default_off() {
    let off = Config::from_lookup(lookup(&[]));
    assert!(!off.clipboard_context);
    assert!(!off.screen_context);
    assert!(!off.diag_context);
    assert_eq!(off.acceptance_prompt_marker, None);
    let on = Config::from_lookup(lookup(&[
        ("COMPME_CLIPBOARD_CONTEXT", "1"),
        ("COMPME_SCREEN_CONTEXT", "true"),
        ("COMPME_DIAG_CONTEXT", "true"),
        ("COMPME_ACCEPTANCE_PROMPT_MARKER", "run marker"),
    ]));
    assert!(on.clipboard_context);
    assert!(on.screen_context);
    assert!(on.diag_context);
    assert_eq!(on.acceptance_prompt_marker.as_deref(), Some("run marker"));
}

#[test]
fn clipboard_diagnostic_reports_marker_without_raw_text() {
    let line = clipboard_diagnostic_line(
        Some("CLIPBOARD-CONTEXT-MARKER"),
        Some("CLIPBOARD-CONTEXT-MARKER"),
    );
    assert_eq!(line, "Some(chars=24 marker=true)");
    assert!(
        !line.contains("CLIPBOARD-CONTEXT-MARKER"),
        "diagnostic leaked marker text: {line:?}"
    );
    assert_eq!(
        clipboard_diagnostic_line(
            Some("other 24 character text"),
            Some("CLIPBOARD-CONTEXT-MARKER")
        ),
        "Some(chars=23 marker=false)"
    );
    assert_eq!(
        clipboard_diagnostic_line(None, Some("CLIPBOARD-CONTEXT-MARKER")),
        "None"
    );
}

#[test]
fn unsupported_apps_are_gated_out() {
    assert!(!app_allows_suggestions(Some("com.mitchellh.ghostty")));
    assert!(app_allows_suggestions(Some("com.apple.TextEdit")));
    // Unresolved app → fail-open (field capabilities still gate).
    assert!(app_allows_suggestions(None));
}

#[test]
fn sidebar_only_apps_require_a_positive_assistant_field() {
    assert!(!app_allows_suggestions(Some("com.microsoft.VSCode")));
    assert!(!app_allows_suggestions(Some(
        "com.todesktop.230313mzl4w4u92"
    )));
    assert!(!app_allows_suggestions(Some("com.exafunction.windsurf")));
    assert!(app_allows_suggestions_for_field(SuggestionApp {
        app_key: Some("com.microsoft.VSCode"),
        assistant_field: true,
    }));
    assert!(!app_allows_suggestions_for_field(SuggestionApp {
        app_key: Some("com.microsoft.VSCode"),
        assistant_field: false,
    }));
}

#[test]
fn suggestion_gates_apply_to_local_replacements_too() {
    // The local replacement offer (emoji/typo/UK) shares this gate, so it is
    // suppressed exactly where a model completion would be.
    let prefs = Prefs::default();
    assert!(suggestion_gates_pass(
        Some("com.apple.TextEdit"),
        "color",
        None,
        &prefs,
        0
    ));
    // Sidebar-only app → blocked.
    assert!(!suggestion_gates_pass(
        Some("com.microsoft.VSCode"),
        "color",
        None,
        &prefs,
        0
    ));
    assert!(suggestion_gates_pass_for_field(
        SuggestionApp {
            app_key: Some("com.microsoft.VSCode"),
            assistant_field: true,
        },
        "color",
        None,
        &prefs,
        0
    ));
    // Terminal with a shell-command line → blocked (not a natural-language prompt).
    assert!(!suggestion_gates_pass(
        Some("com.googlecode.iterm2"),
        "git status && ls -la",
        None,
        &prefs,
        0
    ));
}

#[test]
fn live_keymap_apply_orders_set_rearm_persist_and_reverts_on_failure() {
    // Recorder 5b sequencing contract (banked c131 design): keymap
    // write FIRST (an old hotkey firing mid-swap reads the new map —
    // role-safe), re-arm SECOND, persist ONLY after the re-arm
    // succeeded. On re-arm failure the map REVERTS so
    // effective_accept_keys()/the Shortcuts pane keep telling the
    // registered truth (the c123 desync class).
    let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let l1 = std::rc::Rc::clone(&log);
    let l2 = std::rc::Rc::clone(&log);
    let l3 = std::rc::Rc::clone(&log);
    let ok = apply_live_accept_keymap(
        Some((35, 0)),
        Some((38, 0)),
        Some((96, 0)),
        |w, f, g| {
            l1.borrow_mut().push(format!("set:{w:?},{f:?},{g:?}"));
            Ok(())
        },
        || {
            l2.borrow_mut().push("rearm".into());
            Ok(())
        },
        |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
        || ((35, 0), (38, 0), Some((96, 0))),
    );
    assert!(ok.is_ok());
    assert_eq!(
        *log.borrow(),
        vec![
            "set:Some((35, 0)),Some((38, 0)),Some((96, 0))".to_string(),
            "rearm".to_string(),
            "persist:(35, 0),(38, 0),Some((96, 0))".to_string(),
        ]
    );

    // Failure path: set → rearm Err → REVERT set, no persist.
    let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let l1 = std::rc::Rc::clone(&log);
    let l2 = std::rc::Rc::clone(&log);
    let l3 = std::rc::Rc::clone(&log);
    let rearm_calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let calls = std::rc::Rc::clone(&rearm_calls);
    let err = apply_live_accept_keymap(
        Some((35, 0)),
        Some((38, 0)),
        Some((96, 0)),
        |w, f, g| {
            l1.borrow_mut().push(format!("set:{w:?},{f:?},{g:?}"));
            Ok(())
        },
        || {
            let call = calls.get() + 1;
            calls.set(call);
            l2.borrow_mut().push("rearm".into());
            if call == 1 {
                Err(PlatformError::Timeout)
            } else {
                Ok(())
            }
        },
        |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
        || ((48, 0), (50, 0), Some((96, 512))), // the pre-swap registered truth
    );
    assert!(err.is_err());
    assert_eq!(
        *log.borrow(),
        vec![
            "set:Some((35, 0)),Some((38, 0)),Some((96, 0))".to_string(),
            "rearm".to_string(),
            "set:Some((48, 0)),Some((50, 0)),Some((96, 512))".to_string(), // revert (masks intact)
            "rearm".to_string(), // restore the old consumer tap
        ],
        "restore the previous keymap/tap and do not persist after a failed re-arm"
    );

    // Failure path where the REVERT set_map ALSO fails: the function still
    // returns the re-arm error (never the revert error) and never persists.
    // The revert failure is logged rather than swallowed silently — the
    // keymap/registration desync would otherwise be invisible.
    let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let calls = std::cell::Cell::new(0u32);
    let l2 = std::rc::Rc::clone(&log);
    let l3 = std::rc::Rc::clone(&log);
    let revert_fails = apply_live_accept_keymap(
        Some((35, 0)),
        Some((38, 0)),
        Some((96, 0)),
        |w, f, _g| {
            // First call (the forward set) succeeds; the second (the
            // revert) fails.
            if calls.get() == 0 {
                calls.set(1);
                Ok(())
            } else {
                Err(crate::shell::KeymapError::Collision(
                    w.or(f).map(|(k, _)| k).unwrap_or(0),
                ))
            }
        },
        || {
            l2.borrow_mut().push("rearm".into());
            Err(PlatformError::Timeout)
        },
        |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
        || ((48, 0), (50, 0), None),
    );
    assert!(
        matches!(&revert_fails, Err(e) if e.starts_with("re-arm failed")),
        "the re-arm error is returned even when the revert also fails"
    );
    assert!(
        !log.borrow().iter().any(|l| l.starts_with("persist:")),
        "a failed re-arm never persists, even if the revert fails too"
    );

    // Partial rebind (word=None keeps the default): persist receives the
    // DEFAULTS-RESOLVED registered pair from effective(), not the raw
    // request args — pins the explicit-beats-absent persist choice.
    let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let l1 = std::rc::Rc::clone(&log);
    let l2 = std::rc::Rc::clone(&log);
    let l3 = std::rc::Rc::clone(&log);
    let partial = apply_live_accept_keymap(
        None,
        Some((38, 0)),
        None,
        |w, f, g| {
            l1.borrow_mut().push(format!("set:{w:?},{f:?},{g:?}"));
            Ok(())
        },
        || {
            l2.borrow_mut().push("rearm".into());
            Ok(())
        },
        |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
        || ((48, 0), (38, 0), None), // post-resolution: default word stays 48
    );
    assert!(partial.is_ok());
    assert_eq!(
        log.borrow().last().unwrap(),
        "persist:(48, 0),(38, 0),None",
        "persist writes the RESOLVED pair, not the raw request"
    );

    // Invalid map (collision) fails BEFORE any rearm/persist.
    let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let l2 = std::rc::Rc::clone(&log);
    let l3 = std::rc::Rc::clone(&log);
    let invalid = apply_live_accept_keymap(
        Some((53, 0)),
        None,
        None,
        |_, _, _| Err(crate::shell::KeymapError::Collision(53)),
        || {
            l2.borrow_mut().push("rearm".into());
            Ok(())
        },
        |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
        || ((48, 0), (50, 0), None),
    );
    assert!(invalid.is_err());
    assert!(
        log.borrow().is_empty(),
        "rejected map never rearms/persists"
    );
}

#[test]
fn live_rebind_sets_and_persists_the_recorder_resolved_masks_verbatim() {
    // Slice 2: the recorder now supplies fully-resolved (keycode, mask)
    // pairs for BOTH roles, so apply_live_accept_keymap sets them as-is —
    // no mask reconstruction. word=Shift+48 (the unchanged role, carried
    // through by the recorder with its Shift mask intact — the audit-r2
    // preservation, now done upstream in recorder_outcome) and a freshly
    // captured bare full key (50) both reach set_map untouched, and persist
    // receives the same resolved pair (round-trips via format_accept_key).
    let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let l1 = std::rc::Rc::clone(&log);
    let l3 = std::rc::Rc::clone(&log);
    const SHIFT: u32 = 512; // Carbon shiftKey mask used by the macOS keymap facade.
                            // A stateful registered map so effective() reflects what set_map last
                            // wrote — this lets the PERSIST leg be asserted (persist reads the
                            // resolved registered pair via effective(), exactly as the real run
                            // loop does). Starts at the pre-rebind truth (word=Shift+48, full=60).
                            // Typed literals let inference name the type (no complex annotation).
    let registered = std::rc::Rc::new(std::cell::RefCell::new((
        (48_i64, SHIFT),
        (60_i64, 0_u32),
        Some((96_i64, SHIFT)),
    )));
    let r_set = std::rc::Rc::clone(&registered);
    let r_eff = std::rc::Rc::clone(&registered);
    let applied = apply_live_accept_keymap(
        Some((48, SHIFT)), // word: unchanged Shift+48, mask carried by the recorder
        Some((50, 0)),     // full: newly captured bare key
        Some((96, SHIFT)), // grammar accept: existing masked key preserved
        move |w, f, g| {
            // Mirror set_accept_keymap_from_config_with_mods: a None slot
            // default-fills (Tab/backtick); here both are explicit.
            *r_set.borrow_mut() = (w.unwrap_or((48, 0)), f.unwrap_or((50, 0)), g);
            l1.borrow_mut().push(format!("set:{w:?},{f:?},{g:?}"));
            Ok(())
        },
        || Ok(()),
        |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
        move || *r_eff.borrow(),
    );
    assert!(applied.is_ok());
    assert_eq!(
        log.borrow()[0],
        format!("set:Some((48, {SHIFT})),Some((50, 0)),Some((96, {SHIFT}))"),
        "the recorder-resolved masks reach set_map verbatim — Shift+48 kept, full bare"
    );
    assert_eq!(
        log.borrow().last().unwrap(),
        &format!("persist:(48, {SHIFT}),(50, 0),Some((96, {SHIFT}))"),
        "persist receives the resolved registered pair — the Shift mask survives to disk"
    );
}

#[test]
fn live_rebind_failure_rearms_the_previous_keymap_after_revert() {
    let registered = std::rc::Rc::new(std::cell::RefCell::new((
        (48_i64, 512_u32),
        (50_i64, 0_u32),
        Some((96_i64, 512_u32)),
    )));
    let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let rearm_calls = std::rc::Rc::new(std::cell::Cell::new(0));

    let r_set = std::rc::Rc::clone(&registered);
    let l_set = std::rc::Rc::clone(&log);
    let l_rearm = std::rc::Rc::clone(&log);
    let calls = std::rc::Rc::clone(&rearm_calls);
    let r_eff = std::rc::Rc::clone(&registered);
    let l_persist = std::rc::Rc::clone(&log);
    let applied = apply_live_accept_keymap(
        Some((60, 0)),
        Some((61, 0)),
        Some((62, 0)),
        move |w, f, g| {
            *r_set.borrow_mut() = (w.unwrap_or((48, 0)), f.unwrap_or((50, 0)), g);
            l_set
                .borrow_mut()
                .push(format!("set:{:?}", *r_set.borrow()));
            Ok(())
        },
        move || {
            let call = calls.get() + 1;
            calls.set(call);
            l_rearm.borrow_mut().push(format!("rearm:{call}"));
            if call == 1 {
                Err(PlatformError::Timeout)
            } else {
                Ok(())
            }
        },
        move |w, f, g| {
            l_persist
                .borrow_mut()
                .push(format!("persist:{w:?},{f:?},{g:?}"))
        },
        move || *r_eff.borrow(),
    );

    assert!(applied.is_err());
    assert_eq!(
        log.borrow().as_slice(),
        [
            "set:((60, 0), (61, 0), Some((62, 0)))",
            "rearm:1",
            "set:((48, 512), (50, 0), Some((96, 512)))",
            "rearm:2",
        ],
        "failure restores the old map and re-arms against it without persisting"
    );
    assert_eq!(rearm_calls.get(), 2);
    assert_eq!(
        *registered.borrow(),
        ((48, 512), (50, 0), Some((96, 512))),
        "effective keymap reports the previous registered truth"
    );
}

#[test]
fn grammar_accept_rebind_persists_compme_grammar_accept_key() {
    let persisted: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let sink = std::rc::Rc::clone(&persisted);
    let ok = apply_live_accept_keymap(
        Some((48, 0)),
        Some((50, 0)),
        Some((96, 512)),
        |_, _, _| Ok(()),
        || Ok(()),
        move |w, f, g| {
            for (key, value) in [
                ("COMPME_ACCEPT_WORD_KEY", Some(w)),
                ("COMPME_ACCEPT_FULL_KEY", Some(f)),
                ("COMPME_GRAMMAR_ACCEPT_KEY", g),
            ] {
                match value {
                    Some((code, mask)) => sink.borrow_mut().push(format!(
                        "{key}={}",
                        crate::shell::format_accept_key(code, mask)
                    )),
                    None => sink.borrow_mut().push(format!("remove:{key}")),
                }
            }
        },
        || ((48, 0), (50, 0), Some((96, 512))),
    );
    assert!(ok.is_ok());
    assert!(persisted
        .borrow()
        .contains(&"COMPME_GRAMMAR_ACCEPT_KEY=shift+96".to_string()));
}

#[test]
fn cached_domain_guards_on_the_app_it_was_read_under() {
    let cache = Some(("com.apple.Safari".to_string(), "docs.example".to_string()));
    // Same app → the cached host applies.
    assert_eq!(
        cached_domain(&cache, Some("com.apple.Safari")),
        Some("docs.example")
    );
    // The request resolved to a DIFFERENT app than the focus that
    // populated the cache → never cross-attribute a domain.
    assert_eq!(cached_domain(&cache, Some("com.google.Chrome")), None);
    assert_eq!(cached_domain(&cache, None), None);
    assert_eq!(cached_domain(&None, Some("com.apple.Safari")), None);
}

#[test]
fn typing_domain_refreshes_browser_cache_when_a_domain_consumer_is_enabled() {
    let mut cache = Some((
        "com.apple.Safari".to_string(),
        "allowed.example".to_string(),
    ));
    assert_eq!(
        typing_domain(&mut cache, Some("com.apple.Safari"), false, None),
        Some("allowed.example".into())
    );

    assert_eq!(
        typing_domain(
            &mut cache,
            Some("com.apple.Safari"),
            true,
            Some("https://blocked.example/private")
        ),
        Some("blocked.example".into())
    );
    assert_eq!(
        typing_domain(&mut cache, Some("com.apple.Safari"), true, None),
        None
    );
}

#[test]
fn domain_observation_enabled_fires_on_either_consumer() {
    // Domain reads are an OR of the two per-domain consumers: excluded-domain
    // rules and per-domain steering instructions. Pin each disjunct so neither
    // consumer silently stops requesting browser-domain detection.
    let empty_profile = PersonalizationProfile::default();
    let empty_prefs = Prefs::default();

    // Both empty → no consumer wants domains.
    assert!(!domain_observation_enabled(&empty_prefs, &empty_profile));

    // Only excluded_domains non-empty → enabled.
    let mut prefs_only = Prefs::default();
    prefs_only.excluded_domains.insert("bank.example".into());
    assert!(domain_observation_enabled(&prefs_only, &empty_profile));

    // Only per_domain steering non-empty → enabled.
    let mut profile_only = PersonalizationProfile::default();
    profile_only
        .per_domain
        .insert("docs.google.com".into(), "Match the doc tone.".into());
    assert!(domain_observation_enabled(&empty_prefs, &profile_only));
}

#[test]
fn submit_gate_blocks_an_excluded_domain() {
    // The per-domain rules' submit-side consumer: with a domain present,
    // an excluded host blocks the request in an otherwise-allowed app.
    let mut prefs = Prefs::default();
    prefs.excluded_domains.insert("bank.example".into());
    assert!(!request_passes_submit_gates(
        &req_with_prompt("Dear team"),
        Some("com.apple.Safari"),
        Some("bank.example"),
        &prefs,
        0
    ));
    assert!(request_passes_submit_gates(
        &req_with_prompt("Dear team"),
        Some("com.apple.Safari"),
        Some("other.example"),
        &prefs,
        0
    ));
    // Browser domain rules configured but no fresh domain resolved:
    // fail closed on model submit so a missed URL read cannot bypass an
    // excluded-domain rule.
    assert!(!request_passes_submit_gates(
        &req_with_prompt("Dear team"),
        Some("com.apple.Safari"),
        None,
        &prefs,
        0
    ));
}

#[test]
fn submit_gate_uses_grammar_left_context_not_empty_prompt() {
    let prefs = Prefs::default();
    let request = grammar_req_with_left_ctx("Dear team teh");
    assert_eq!(request.prompt, "");
    assert!(request_passes_submit_gates(
        &request,
        Some("com.apple.Terminal"),
        None,
        &prefs,
        0
    ));
}

#[test]
fn submit_gate_blocks_a_subdomain_of_an_excluded_domain() {
    // Privacy-critical subdomain consumer: an excluded `bank.example` rule
    // must also block a request typed on the subdomain `login.bank.example`
    // (dot-boundary match), through both the model submit gate and the
    // local-replacement gate. A look-alike host on a non-dot boundary stays
    // allowed.
    let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
    let mut prefs = Prefs::default();
    prefs.excluded_domains.insert("bank.example".into());
    let app = Some("com.apple.Safari");

    // Submit gate: subdomain blocked.
    assert!(!request_passes_submit_gates(
        &req_with_prompt("Dear team"),
        app,
        Some("login.bank.example"),
        &prefs,
        0
    ));
    // Submit gate: look-alike on a non-dot boundary is NOT blocked.
    assert!(request_passes_submit_gates(
        &req_with_prompt("Dear team"),
        app,
        Some("notbank.example"),
        &prefs,
        0
    ));

    // Replacement gate: the same subdomain rule blocks the local path too.
    assert!(replacement_decision(
        "hi :smile",
        &config,
        &prefs,
        app,
        Some("login.bank.example"),
        true,
        0
    )
    .is_none());
    assert!(replacement_decision(
        "hi :smile",
        &config,
        &prefs,
        app,
        Some("notbank.example"),
        true,
        0
    )
    .is_some());
}

#[test]
fn web_override_persist_removes_emptied_keys_instead_of_blanking() {
    // Clearing the last entry in a category must REMOVE the key from
    // config.env — not leave the prior value stale (a naive skip) and not
    // write a blank `KEY=` (which occupies the env-over-file layer and
    // clutters the file). review-2026-06-13.
    let dir =
        std::env::temp_dir().join(format!("compme-weboverride-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("config.env");

    let mut prefs = Prefs::default();
    prefs.excluded_domains.insert("old.example.com".into());
    persist_web_override_prefs(&path, &prefs);
    let after_set = std::fs::read_to_string(&path).expect("read after set");
    assert!(
        after_set.contains("COMPME_EXCLUDED_DOMAINS=old.example.com"),
        "a populated category must persist its value: {after_set:?}"
    );

    // Clear every category and re-persist.
    persist_web_override_prefs(&path, &Prefs::default());
    let after_clear = std::fs::read_to_string(&path).expect("read after clear");
    assert!(
        !after_clear.contains("COMPME_EXCLUDED_DOMAINS"),
        "emptied key must be removed, not left stale or blanked: {after_clear:?}"
    );
    assert!(
        !after_clear.contains("COMPME_ENABLED_APPS="),
        "an empty category must never be written as a blank key: {after_clear:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn replacement_decision_blocks_an_excluded_domain() {
    // The local-replacement path consumes the same domain gate.
    let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
    let mut prefs = Prefs::default();
    prefs.excluded_domains.insert("bank.example".into());
    let app = Some("com.apple.Safari");
    assert!(replacement_decision(
        "hi :smile",
        &config,
        &prefs,
        app,
        Some("bank.example"),
        true,
        0
    )
    .is_none());
    assert!(replacement_decision(
        "hi :smile",
        &config,
        &prefs,
        app,
        Some("other.example"),
        true,
        0
    )
    .is_some());
}

#[test]
fn submit_gate_combines_app_terminal_and_preference_policy() {
    let prefs = Prefs::default();
    assert!(request_passes_submit_gates(
        &req_with_prompt("Dear team"),
        Some("com.apple.TextEdit"),
        None,
        &prefs,
        0
    ));
    assert!(!request_passes_submit_gates(
        &req_with_prompt("Dear team"),
        Some("com.mitchellh.ghostty"),
        None,
        &prefs,
        0
    ));
    assert!(!request_passes_submit_gates(
        &req_with_prompt("Dear team"),
        Some("com.microsoft.VSCode"),
        None,
        &prefs,
        0
    ));
    assert!(!request_passes_submit_gates(
        &req_with_prompt("git status && ls -la"),
        Some("com.googlecode.iterm2"),
        None,
        &prefs,
        0
    ));
    assert!(request_passes_submit_gates(
        &req_with_prompt("please summarize the recent changes"),
        Some("com.googlecode.iterm2"),
        None,
        &prefs,
        0
    ));

    let excluded = build_prefs(&lookup(&[("COMPME_EXCLUDED_APPS", "com.apple.TextEdit")]));
    assert!(!request_passes_submit_gates(
        &req_with_prompt("Dear team"),
        Some("com.apple.TextEdit"),
        None,
        &excluded,
        0
    ));
}

#[test]
fn submit_gate_blocks_while_snoozed_then_auto_resumes() {
    // Snooze must gate suggestions through the *integration* submit gate, not
    // only the standalone prefs unit — and auto-resume after the window
    // (A2 §16 pause/snooze).
    let mut prefs = Prefs::default();
    prefs.snooze(1_000, 5); // paused until t = 1_000 + 5*60_000 = 301_000 ms
    let req = req_with_prompt("Dear team");
    let app = Some("com.apple.TextEdit");
    // Blocked at the start of and midway through the window.
    assert!(!request_passes_submit_gates(&req, app, None, &prefs, 1_000));
    assert!(!request_passes_submit_gates(
        &req, app, None, &prefs, 61_000
    ));
    // Auto-resumes once the window elapses.
    assert!(request_passes_submit_gates(
        &req, app, None, &prefs, 301_001
    ));
}

#[test]
fn submit_gate_uses_resolved_bundle_id_not_volatile_field_app() {
    let volatile = CompletionRequest {
        field: FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: "f".into(),
            generation: 1,
        },
        ..req_with_prompt("Dear team")
    };

    let sidebar_key = resolve_app_key(volatile.field.pid, |pid| {
        (pid == 42).then(|| "com.microsoft.VSCode".to_string())
    });
    assert!(!request_passes_submit_gates(
        &volatile,
        sidebar_key.as_deref(),
        None,
        &Prefs::default(),
        0
    ));

    let textedit_key = resolve_app_key(volatile.field.pid, |pid| {
        (pid == 42).then(|| "com.apple.TextEdit".to_string())
    });
    let excluded = build_prefs(&lookup(&[("COMPME_EXCLUDED_APPS", "com.apple.TextEdit")]));
    assert!(!request_passes_submit_gates(
        &volatile,
        textedit_key.as_deref(),
        None,
        &excluded,
        0
    ));

    // Unresolved pid fails open and does not treat the volatile `pid:42`
    // field app as a preference key.
    let unresolved = resolve_app_key(volatile.field.pid, |_| None);
    assert!(request_passes_submit_gates(
        &volatile,
        unresolved.as_deref(),
        None,
        &build_prefs(&lookup(&[("COMPME_EXCLUDED_APPS", "pid:42")])),
        0
    ));
}

#[test]
fn context_max_chars_parsing_is_off_by_default_and_fail_safe() {
    assert_eq!(parse_context_max_chars(None), 0);
    assert_eq!(parse_context_max_chars(Some("off".into())), 0);
    assert_eq!(parse_context_max_chars(Some("0".into())), 0);
    assert_eq!(parse_context_max_chars(Some("150".into())), 150);
    assert_eq!(
        parse_context_max_chars(Some("true".into())),
        DEFAULT_CONTEXT_MAX_CHARS
    );
    assert_eq!(parse_context_max_chars(Some("99999".into())), 2000); // clamped
}

#[test]
fn context_max_chars_treats_falsy_words_and_blank_values_as_off() {
    // Explicit falsy words and an empty/whitespace-only value all mean off
    // (0); a plain number is taken verbatim; non-numeric junk falls back to
    // the default bound rather than disabling context.
    assert_eq!(parse_context_max_chars(Some("false".into())), 0);
    assert_eq!(parse_context_max_chars(Some("no".into())), 0);
    assert_eq!(parse_context_max_chars(Some("".into())), 0);
    assert_eq!(parse_context_max_chars(Some("   ".into())), 0);
    assert_eq!(parse_context_max_chars(Some("200".into())), 200);
    assert_eq!(
        parse_context_max_chars(Some("junk".into())),
        DEFAULT_CONTEXT_MAX_CHARS
    );
}

#[test]
fn resolve_app_key_maps_pid_to_bundle_id() {
    let resolver = |pid: i32| (pid == 42).then(|| "com.apple.TextEdit".to_string());
    assert_eq!(
        resolve_app_key(Some(42), resolver),
        Some("com.apple.TextEdit".into())
    );
    // Unresolvable pid or absent pid → None (fail-open gating).
    assert_eq!(resolve_app_key(Some(99), resolver), None);
    assert_eq!(resolve_app_key(None, resolver), None);
}

#[test]
fn resolve_app_key_returns_none_for_pid_above_i32_range() {
    // A u32 pid larger than i32::MAX can't be a real macOS pid; `i32::try_from`
    // fails so the resolver must never be called and gating fails open (None),
    // rather than panicking or wrapping to a negative pid.
    let resolver = |_pid: i32| -> Option<String> {
        panic!("resolver must not be called for an out-of-range pid");
    };
    let too_big = (i32::MAX as u32) + 1;
    assert_eq!(resolve_app_key(Some(too_big), resolver), None);
    assert_eq!(resolve_app_key(Some(u32::MAX), resolver), None);
}

#[test]
fn effective_app_key_falls_back_to_canonical_field_app() {
    let stable = FieldHandle {
        app: "com.apple.TextEdit".into(),
        pid: Some(42),
        element_id: "f".into(),
        generation: 1,
    };
    assert_eq!(
        effective_app_key(&stable, |_| None),
        Some("com.apple.TextEdit".into()),
        "a transient pid lookup miss must keep the already-canonical app key"
    );

    let volatile = FieldHandle {
        app: "pid:42".into(),
        ..stable
    };
    assert_eq!(
        effective_app_key(&volatile, |_| None),
        None,
        "a volatile pid:N app still fails open when no resolver can identify it"
    );
}

#[test]
fn effective_app_key_blocks_submit_with_canonical_fallback() {
    let field = FieldHandle {
        app: "com.apple.TextEdit".into(),
        pid: Some(42),
        element_id: "f".into(),
        generation: 1,
    };
    let request = CompletionRequest {
        field: field.clone(),
        ..req_with_prompt("Dear team")
    };
    let mut prefs = Prefs::default();
    prefs.excluded_apps.insert("com.apple.TextEdit".into());
    let app_key = effective_app_key(&field, |_| None);

    assert!(!request_passes_submit_gates(
        &request,
        app_key.as_deref(),
        None,
        &prefs,
        1_000
    ));
}

#[test]
fn current_app_actions_use_canonical_fallback_when_pid_lookup_fails() {
    let field = FieldHandle {
        app: "com.apple.TextEdit".into(),
        pid: Some(42),
        element_id: "f".into(),
        generation: 1,
    };
    let app = effective_app_key(&field, |_| None).expect("canonical fallback");

    let mut prefs = Prefs::default();
    assert!(!toggle_app_collection(&mut prefs, &app));
    assert_eq!(no_collect_apps_value(&prefs), "com.apple.TextEdit");

    apply_app_disable(DisableArm::Always, &app, &mut prefs, 1_000);
    assert_eq!(excluded_apps_value(&prefs), "com.apple.TextEdit");
}

#[test]
fn memory_storage_mode_defaults_off_and_parses_modes() {
    use memory::StorageMode;
    // Unset, falsy, and unknown all stay Off (opt-in §16 default).
    assert_eq!(parse_storage_mode(None), StorageMode::Off);
    assert_eq!(parse_storage_mode(Some("off".into())), StorageMode::Off);
    assert_eq!(
        parse_storage_mode(Some("nonsense".into())),
        StorageMode::Off
    );
    // Accepted-only synonyms.
    assert_eq!(
        parse_storage_mode(Some("accepted".into())),
        StorageMode::AcceptedOnly
    );
    assert_eq!(
        parse_storage_mode(Some("  TRUE ".into())),
        StorageMode::AcceptedOnly
    );
    // All-monitored synonyms.
    assert_eq!(
        parse_storage_mode(Some("all".into())),
        StorageMode::AllMonitored
    );
    assert_eq!(
        parse_storage_mode(Some("monitored".into())),
        StorageMode::AllMonitored
    );
}

#[test]
fn hex_key_parses_64_chars_and_rejects_bad_input() {
    let hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    let key = parse_hex_key(hex).expect("valid 64-hex key");
    assert_eq!(key[0], 0x00);
    assert_eq!(key[1], 0x11);
    assert_eq!(key[31], 0xff);
    // Wrong length and non-hex digits fail closed (store stays disabled).
    assert!(parse_hex_key("deadbeef").is_none());
    assert!(parse_hex_key(&"z".repeat(64)).is_none());
}

#[test]
fn memory_disabled_without_key_or_path_even_when_mode_set() {
    // Mode on but no key/path → no store (fail-closed, logged).
    let cfg = MemoryConfig {
        mode: memory::StorageMode::AcceptedOnly,
        path: None,
        key: None,
    };
    assert!(open_memory_store(&cfg, || None).is_none());
    // Off mode is always disabled regardless of key/path.
    let cfg_off = MemoryConfig {
        mode: memory::StorageMode::Off,
        path: Some(PathBuf::from("/tmp/should-not-open.db")),
        key: Some([7u8; 32]),
    };
    assert!(open_memory_store(&cfg_off, || None).is_none());
}

#[test]
fn memory_file_hardening_failure_drops_store_and_stops_at_the_failed_sidecar() {
    struct DropProbe<'a>(&'a Cell<bool>);
    impl Drop for DropProbe<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    let dropped = Cell::new(false);
    let path = PathBuf::from("/tmp/compme-memory.db");
    let attempted = RefCell::new(Vec::new());

    let result = retain_memory_store_if_hardened(
        DropProbe(&dropped),
        &path,
        |_| Ok(true),
        |candidate| {
            attempted.borrow_mut().push(candidate.to_path_buf());
            if candidate.to_string_lossy().ends_with("-wal") {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "cannot harden wal",
                ))
            } else {
                Ok(())
            }
        },
    );

    assert!(result.is_err());
    assert!(
        dropped.get(),
        "a hardening failure must drop the live store"
    );
    assert_eq!(
        attempted.into_inner(),
        vec![
            path.clone(),
            PathBuf::from("/tmp/compme-memory.db-journal"),
            PathBuf::from("/tmp/compme-memory.db-wal"),
        ],
        "hardening stops immediately at the first failed owned file"
    );
}

#[test]
fn memory_file_probe_error_drops_store_without_attempting_hardening() {
    struct DropProbe<'a>(&'a Cell<bool>);
    impl Drop for DropProbe<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    let dropped = Cell::new(false);
    let hardened = Cell::new(false);
    let result = retain_memory_store_if_hardened(
        DropProbe(&dropped),
        std::path::Path::new("/tmp/compme-memory.db"),
        |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "metadata denied",
            ))
        },
        |_| {
            hardened.set(true);
            Ok(())
        },
    );

    assert!(result.is_err());
    assert!(dropped.get(), "a probe error must drop the live store");
    assert!(
        !hardened.get(),
        "hardening must not proceed after the existence probe fails"
    );
}

#[test]
fn memory_parent_posture_gate_fails_closed_without_mutating_preexisting_parent() {
    let parent = PathBuf::from("/tmp/custom-memory-parent");
    let ensured = Cell::new(false);
    let mut key = [0x5a; 32];

    let result = ensure_memory_parent_posture_with(
        &parent,
        &mut key,
        |_| {
            ensured.set(true);
            Ok(())
        },
        |_| Ok(false),
    );

    assert!(ensured.get());
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        key, [0; 32],
        "the rejected pre-open key copy must be scrubbed"
    );
}

#[test]
fn memory_parent_posture_gate_propagates_readback_errors() {
    let mut key = [0x5a; 32];
    let result = ensure_memory_parent_posture_with(
        std::path::Path::new("/tmp/unreadable-memory-parent"),
        &mut key,
        |_| Ok(()),
        |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "readback denied",
            ))
        },
    );

    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        key, [0; 32],
        "a posture probe error must scrub the key copy"
    );
}

#[test]
fn memory_parent_posture_gate_keeps_key_for_a_verified_store_open() {
    let mut key = [0x5a; 32];
    ensure_memory_parent_posture_with(
        std::path::Path::new("/tmp/private-memory-parent"),
        &mut key,
        |_| Ok(()),
        |_| Ok(true),
    )
    .expect("verified parent");

    assert_eq!(key, [0x5a; 32]);
}

fn private_memory_test_path(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("compme-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("memory.db")
}

fn remove_private_memory_test_dir(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}

#[test]
fn memory_opens_with_the_keychain_fallback_key_when_env_key_is_missing() {
    let path = private_memory_test_path("keychain-fallback");
    let cfg = MemoryConfig {
        mode: memory::StorageMode::AcceptedOnly,
        path: Some(path.clone()),
        key: None,
    };

    let store = open_memory_store(&cfg, || Some([7u8; 32]));
    assert!(
        store.is_some(),
        "a keychain-provided key must open the store when the env key is absent"
    );
    drop(store);
    remove_private_memory_test_dir(&path);
}

#[test]
fn an_explicit_env_key_takes_precedence_over_the_keychain() {
    // The keychain must not even be consulted: an explicit
    // COMPME_MEMORY_KEY is the operator's override (and the
    // fail-closed path when the keychain is unavailable).
    let path = private_memory_test_path("env-key-precedence");
    let cfg = MemoryConfig {
        mode: memory::StorageMode::AcceptedOnly,
        path: Some(path.clone()),
        key: Some([7u8; 32]),
    };

    let store = open_memory_store(&cfg, || panic!("keychain consulted despite env key"));
    assert!(store.is_some());
    drop(store);
    remove_private_memory_test_dir(&path);
}

#[test]
fn configured_all_monitored_store_persists_redacted_inserted_deltas_only() {
    let path = private_memory_test_path("all-monitored-configured");
    let path_str = path.to_string_lossy().into_owned();
    let key = "1111111111111111111111111111111111111111111111111111111111111111";
    let cfg = build_memory_config(&|name| match name {
        "COMPME_MEMORY" => Some("all".into()),
        "COMPME_MEMORY_PATH" => Some(path_str.clone()),
        "COMPME_MEMORY_KEY" => Some(key.into()),
        _ => None,
    });
    let store = open_memory_store(&cfg, || panic!("keychain consulted despite env key"))
        .expect("configured all-monitored store opens");
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(
        &field,
        "pre-existing alice@example.com",
        "pre-existing alice@example.com typed bob@example.com ",
    );
    let prefs = Prefs::default();
    queue_and_flush_monitored(&change, &store, &prefs, true, false);
    assert_eq!(
        store.recent("com.apple.TextEdit", 10).unwrap(),
        vec![" typed [redacted-email] "]
    );
    drop(store);

    let reopened = open_memory_store(&cfg, || panic!("keychain consulted despite env key"))
        .expect("configured all-monitored store reopens");
    assert_eq!(
        reopened.recent("com.apple.TextEdit", 10).unwrap(),
        vec![" typed [redacted-email] "]
    );
    drop(reopened);
    let raw = std::fs::read(&path).expect("memory db is readable");
    for needle in [
        b"bob@example.com".as_slice(),
        b"[redacted-email]".as_slice(),
    ] {
        assert!(
            !raw.windows(needle.len()).any(|window| window == needle),
            "monitored text must be encrypted on disk, including redacted form"
        );
    }
    remove_private_memory_test_dir(&path);
}

#[test]
fn the_keychain_is_not_consulted_when_memory_is_off_or_path_is_missing() {
    let cfg_off = MemoryConfig {
        mode: memory::StorageMode::Off,
        path: Some(PathBuf::from("/tmp/should-not-open.db")),
        key: None,
    };
    assert!(open_memory_store(&cfg_off, || panic!("keychain consulted while Off")).is_none());
    // No path → no store to encrypt; creating a keychain key would be a
    // side effect with no purpose.
    let cfg_no_path = MemoryConfig {
        mode: memory::StorageMode::AcceptedOnly,
        path: None,
        key: None,
    };
    assert!(
        open_memory_store(&cfg_no_path, || panic!("keychain consulted without a path")).is_none()
    );
}

#[test]
fn latency_sample_computes_elapsed_and_prunes_older_generations() {
    let mut submit = HashMap::new();
    submit.insert(1u64, 100u64);
    submit.insert(2u64, 150u64);
    submit.insert(3u64, 220u64);
    // Outcome for gen 2 at t=210 → latency 60ms; gen 1 (older) is pruned, gen
    // 3 (newer, still pending) is kept.
    assert_eq!(latency_sample(&mut submit, 2, 210), Some(60));
    assert!(!submit.contains_key(&1));
    assert!(!submit.contains_key(&2));
    assert!(submit.contains_key(&3));
}

#[test]
fn latency_sample_is_none_for_an_untracked_generation() {
    let mut submit = HashMap::new();
    submit.insert(5u64, 100u64);
    // A coalesced-away or already-pruned generation has no submit time.
    assert_eq!(latency_sample(&mut submit, 9, 200), None);
    // The unrelated entry is untouched.
    assert!(submit.contains_key(&5));
    // Degenerate form of the same path: a fully empty map.
    let mut empty: HashMap<u64, u64> = HashMap::new();
    assert_eq!(latency_sample(&mut empty, 1, 100), None);
    assert!(empty.is_empty());
}

#[test]
fn latency_sample_saturates_rather_than_overflowing() {
    let mut submit = HashMap::new();
    submit.insert(1u64, 0u64);
    // An implausibly large elapsed value clamps to u32::MAX, never panics.
    assert_eq!(latency_sample(&mut submit, 1, u64::MAX), Some(u32::MAX));
}

#[test]
fn latency_sample_prunes_all_lower_generations_in_one_call() {
    let mut submit = HashMap::new();
    for (gen, at) in [(1u64, 100u64), (2, 110), (3, 120)] {
        submit.insert(gen, at);
    }
    // Outcome for the newest pending gen prunes every entry at or below it.
    assert_eq!(latency_sample(&mut submit, 3, 200), Some(80));
    assert!(submit.is_empty());
}

#[test]
fn latency_sample_returns_zero_when_outcome_same_ms_as_submit() {
    // A completion returned within the same heartbeat reads as 0 ms — the
    // true measured value at run-loop resolution, not None.
    let mut submit = HashMap::new();
    submit.insert(1u64, 100u64);
    assert_eq!(latency_sample(&mut submit, 1, 100), Some(0));
}

#[test]
fn latency_sample_supports_repeated_calls_against_a_shared_map() {
    // The runtime pattern: one persistent map, sampled per outcome.
    let mut submit = HashMap::new();
    submit.insert(2u64, 100u64);
    submit.insert(3u64, 130u64);
    assert_eq!(latency_sample(&mut submit, 2, 150), Some(50));
    assert_eq!(latency_sample(&mut submit, 3, 200), Some(70));
    assert!(submit.is_empty());
}

fn request_for_submit_tracking(generation: u64) -> CompletionRequest {
    CompletionRequest {
        generation,
        field: FieldHandle {
            app: "com.apple.TextEdit".into(),
            pid: Some(7),
            element_id: "ax:field".into(),
            generation: 1,
        },
        domain: None,
        snapshot: generation,
        prompt: "hello world".into(),
        max_tokens: 24,
        kind: RequestKind::Completion,
    }
}

fn request_log_context_for_submit_tracking() -> RequestLogContext {
    RequestLogContext {
        app_key: Some("com.apple.TextEdit".into()),
        assistant_field: false,
        domain: None,
        prefs: Prefs::default(),
        acceptance_prompt_marker: Some("hello".into()),
    }
}

#[test]
fn submit_tracking_records_only_accepted_requests() {
    let mut submit_times = HashMap::new();
    let mut log_context = request_log_context_for_submit_tracking();
    log_context.domain = Some("docs.google.com".into());

    let log_line = submit_request_and_track(
        &mut submit_times,
        request_for_submit_tracking(7),
        123,
        log_context,
        |request| {
            assert_eq!(request.generation, 7);
            assert_eq!(request.prompt, "hello world");
            assert_eq!(request.domain.as_deref(), Some("docs.google.com"));
            true
        },
    );

    assert!(log_line.contains("request gen=7"));
    assert!(log_line.contains("app=com.apple.TextEdit"));
    assert!(!log_line.contains("inference submit failed"));
    assert_eq!(submit_times.get(&7), Some(&123));
}

#[test]
fn submit_tracking_does_not_record_rejected_requests() {
    let mut submit_times = HashMap::new();

    let mut submitted_generation = None;
    let log_line = submit_request_and_track(
        &mut submit_times,
        request_for_submit_tracking(7),
        123,
        request_log_context_for_submit_tracking(),
        |request| {
            submitted_generation = Some(request.generation);
            false
        },
    );

    assert_eq!(log_line, "compme: inference submit failed gen=7");
    assert!(!log_line.contains("request gen="));
    assert_eq!(submitted_generation, Some(7));
    assert!(!submit_times.contains_key(&7));
    assert_eq!(
        latency_sample(&mut submit_times, 7, 200),
        None,
        "rejected worker submissions must not create phantom latency samples"
    );
}

#[test]
fn submit_tracking_does_not_overwrite_a_domain_already_on_the_request() {
    // `submit_request_and_track` only fills `request.domain` from
    // `log_context.domain` when the request's own domain is None. A domain
    // already resolved onto the request (e.g. the active tab's host) must win
    // over the log context's — never get clobbered.
    let mut submit_times = HashMap::new();
    let request = CompletionRequest {
        domain: Some("a.com".into()),
        ..request_for_submit_tracking(7)
    };
    let mut log_context = request_log_context_for_submit_tracking();
    log_context.domain = Some("b.com".into());

    submit_request_and_track(&mut submit_times, request, 123, log_context, |request| {
        assert_eq!(
            request.domain.as_deref(),
            Some("a.com"),
            "the request's pre-existing domain must be preserved"
        );
        true
    });
}

#[test]
fn auxiliary_context_is_prepared_before_submitting_the_request() {
    let clipboard_cell = Arc::new(Mutex::new(None));
    let order = RefCell::new(Vec::new());
    let mut submit_times = HashMap::new();
    let mut log_context = request_log_context_for_submit_tracking();
    log_context.domain = Some("docs.google.com".into());
    let request = CompletionRequest {
        generation: 17,
        snapshot: 42,
        ..request_for_submit_tracking(17)
    };

    let (clipboard_diag, submit_line) = submit_request_with_auxiliary_context(
        request,
        SubmitRequestContext {
            submit_times: &mut submit_times,
            now_ms: 321,
            log_context,
        },
        AuxiliarySubmitContext {
            clipboard_enabled: true,
            diag_context: true,
            diag_clipboard_marker: Some("copied marker"),
            clipboard_cell: &clipboard_cell,
            screen_enabled: true,
        },
        || {
            order.borrow_mut().push("clipboard");
            Some("copied marker".into())
        },
        |_| {
            order.borrow_mut().push("caret");
            rect(9.0)
        },
        |submission| {
            order.borrow_mut().push("screen");
            assert_eq!(submission.generation, 17);
            assert_eq!(submission.snapshot, 42);
            assert_eq!(submission.caret_rect.unwrap().x, 9.0);
        },
        |request| {
            order.borrow_mut().push("submit");
            assert_eq!(request.generation, 17);
            assert_eq!(request.domain.as_deref(), Some("docs.google.com"));
            true
        },
    );

    assert_eq!(
        *order.borrow(),
        vec!["clipboard", "caret", "screen", "submit"]
    );
    assert_eq!(
        *clipboard_cell.lock().unwrap(),
        Some("copied marker".to_string())
    );
    assert_eq!(
        clipboard_diag.as_deref(),
        Some("Some(chars=13 marker=true)")
    );
    assert!(submit_line.contains("request gen=17"));
    assert_eq!(submit_times.get(&17), Some(&321));
}

#[test]
fn auxiliary_context_off_clears_stale_clipboard_and_skips_screen_submission() {
    let clipboard_cell = Arc::new(Mutex::new(Some("stale clipboard".into())));
    let mut submit_times = HashMap::new();

    let (clipboard_diag, submit_line) = submit_request_with_auxiliary_context(
        request_for_submit_tracking(18),
        SubmitRequestContext {
            submit_times: &mut submit_times,
            now_ms: 444,
            log_context: request_log_context_for_submit_tracking(),
        },
        AuxiliarySubmitContext {
            clipboard_enabled: false,
            diag_context: true,
            diag_clipboard_marker: Some("marker"),
            clipboard_cell: &clipboard_cell,
            screen_enabled: false,
        },
        || panic!("clipboard must not be read when context is disabled"),
        |_| panic!("caret must not be read when screen context is disabled"),
        |_| panic!("screen OCR must not be submitted when disabled"),
        |request| {
            assert_eq!(request.generation, 18);
            true
        },
    );

    assert_eq!(clipboard_diag, None);
    assert_eq!(*clipboard_cell.lock().unwrap(), None);
    assert!(submit_line.contains("request gen=18"));
    assert_eq!(submit_times.get(&18), Some(&444));
}

#[test]
fn clipboard_enabled_but_empty_clears_the_stale_cell() {
    // Clipboard context is ENABLED but the OS read returns None (empty
    // clipboard). The cell must be CLEARED to None — never left holding a
    // prior value, which would leak a stale secret into the next prompt. The
    // `*_off_*` test above covers the disabled branch; this pins the
    // enabled-but-empty path.
    let clipboard_cell = Arc::new(Mutex::new(Some("old secret".into())));
    let mut submit_times = HashMap::new();

    let (clipboard_diag, submit_line) = submit_request_with_auxiliary_context(
        request_for_submit_tracking(19),
        SubmitRequestContext {
            submit_times: &mut submit_times,
            now_ms: 555,
            log_context: request_log_context_for_submit_tracking(),
        },
        AuxiliarySubmitContext {
            clipboard_enabled: true,
            diag_context: false,
            diag_clipboard_marker: None,
            clipboard_cell: &clipboard_cell,
            screen_enabled: false,
        },
        || None,
        |_| panic!("screen disabled"),
        |_| panic!("screen disabled"),
        |request| {
            assert_eq!(request.generation, 19);
            true
        },
    );

    assert_eq!(clipboard_diag, None);
    assert_eq!(
        *clipboard_cell.lock().unwrap(),
        None,
        "an empty clipboard read must clear the stale cell"
    );
    assert!(submit_line.contains("request gen=19"));
    assert_eq!(submit_times.get(&19), Some(&555));
}

#[test]
fn submit_path_redacts_clipboard_before_storing_in_cell() {
    // The submit path (submit_request_with_auxiliary_context) is the ONLY
    // place clipboard text is redacted before it lands in the cell the
    // inference worker reads into the model prompt. The clipboard routinely
    // holds passwords/cards/emails, so a regression dropping redaction::redact
    // here would silently leak raw secrets into the prompt. Pin it: a
    // secret-bearing clipboard must be stored already redacted.
    let clipboard_cell = Arc::new(Mutex::new(None));
    let mut submit_times = HashMap::new();
    let raw_secret = "sk-abcdEFGH0123456789abcdEFGH0123";

    submit_request_with_auxiliary_context(
        request_for_submit_tracking(21),
        SubmitRequestContext {
            submit_times: &mut submit_times,
            now_ms: 500,
            log_context: request_log_context_for_submit_tracking(),
        },
        AuxiliarySubmitContext {
            clipboard_enabled: true,
            diag_context: false,
            diag_clipboard_marker: None,
            clipboard_cell: &clipboard_cell,
            screen_enabled: false,
        },
        || Some(format!("paste {raw_secret} now")),
        |_| panic!("screen disabled"),
        |_| panic!("screen disabled"),
        |_| true,
    );

    let stored = clipboard_cell.lock().unwrap().clone().expect("cell set");
    assert!(
        stored.contains("[redacted-secret]"),
        "clipboard stored redacted: {stored:?}"
    );
    assert!(
        !stored.contains(raw_secret),
        "raw secret must not reach the prompt cell: {stored:?}"
    );
}

#[test]
fn screen_ocr_submission_preserves_request_stamp_and_caret_rect() {
    let request = CompletionRequest {
        generation: 17,
        snapshot: 42,
        ..request_for_submit_tracking(17)
    };
    let submission = ScreenOcrSubmission::from_request(&request, rect(9.0));

    assert_eq!(submission.field, request.field);
    assert_eq!(submission.generation, 17);
    assert_eq!(submission.snapshot, 42);
    assert_eq!(submission.caret_rect.unwrap().x, 9.0);
}

fn field_with_app(app: &str) -> FieldHandle {
    FieldHandle {
        app: app.into(),
        pid: Some(7),
        element_id: "ax:field".into(),
        generation: 1,
    }
}

#[test]
fn buffered_monitored_text_drops_orphaned_prior_generation_buffer() {
    // A field's generation bumps (element replaced) without a Focus event
    // clearing the map. The old-generation Collecting buffer must not linger:
    // the fresh handle prunes its same-logical-field stale sibling so
    // monitored_buffers can't accumulate dead keys within one session.
    let mut buffers: HashMap<FieldHandle, MonitoredBuffer> = HashMap::new();
    let gen1 = field_with_app("com.apple.TextEdit");
    let mut gen2 = field_with_app("com.apple.TextEdit");
    gen2.generation = 2; // same app/pid/element_id, replaced element

    // Mid-word collection on gen1 leaves a Collecting buffer (no boundary).
    assert_eq!(buffered_monitored_text(&mut buffers, &gen1, "ab"), None);
    assert_eq!(buffers.len(), 1);

    // First keystroke on gen2 evicts the orphaned gen1 buffer.
    assert_eq!(buffered_monitored_text(&mut buffers, &gen2, "cd"), None);
    assert_eq!(buffers.len(), 1, "stale gen1 buffer pruned: {buffers:?}");
    assert!(buffers.contains_key(&gen2));
    assert!(!buffers.contains_key(&gen1));

    // An UNRELATED field is left untouched by the prune.
    let other = field_with_app("com.apple.Notes");
    assert_eq!(buffered_monitored_text(&mut buffers, &other, "ef"), None);
    assert_eq!(buffers.len(), 2);
    assert!(buffers.contains_key(&gen2));
    assert!(buffers.contains_key(&other));
}

fn text_context(field: &FieldHandle, left: &str) -> platform::TextContext {
    platform::TextContext {
        left: left.into(),
        right: String::new(),
        left_scalars: left.chars().count(),
        selection: None,
        selected_text: None,
        caret: left.chars().count(),
        source: platform::ContextSource::Accessibility,
        field_id: field.clone(),
        offset_encoding: platform::OffsetEncoding::UnicodeScalars,
    }
}

fn text_context_with_right(field: &FieldHandle, left: &str, right: &str) -> platform::TextContext {
    platform::TextContext {
        left: left.into(),
        right: right.into(),
        left_scalars: left.chars().count(),
        selection: None,
        selected_text: None,
        caret: left.chars().count(),
        source: platform::ContextSource::Accessibility,
        field_id: field.clone(),
        offset_encoding: platform::OffsetEncoding::Utf16CodeUnits,
    }
}

fn writable_axset_caps() -> Capabilities {
    Capabilities {
        readable_text: true,
        readable_caret: true,
        writable: true,
        assistant_field: false,
        secure: false,
        security_state: SecurityState::Normal,
        toolkit: Toolkit::AppKit,
        multiline: true,
        insert_strategy: InsertStrategy::AxSet,
        accept_intercept: KeyInterceptMode::CgEventTap,
        overlay_at_caret: OverlayPlacement::NativePanel,
        coords_global_screen: true,
    }
}

fn grammar_gate<'a>(
    config: &'a Config,
    prefs: &'a Prefs,
    app_key: Option<&'a str>,
    domain: Option<&'a str>,
    enabled: bool,
    caps: &'a Capabilities,
    now_ms: u64,
) -> GrammarRequestGate<'a> {
    GrammarRequestGate {
        config,
        prefs,
        app_key,
        domain,
        enabled,
        caps,
        now_ms,
    }
}

#[test]
fn grammar_trigger_dispatches_word_at_caret_scalar_range() {
    let field = host_field("grammar");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let request = grammar_fix_request(
        &field,
        &text_context_with_right(&field, "😀 teh", ""),
        grammar_gate(
            &config,
            &config.prefs,
            Some("TextEdit"),
            None,
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .expect("request");

    assert_eq!(request.generation, field.generation);
    assert_eq!(request.prompt, "");
    match request.kind {
        RequestKind::GrammarFix {
            word,
            left_ctx,
            correction_range,
        } => {
            assert_eq!(word, "teh");
            assert_eq!(left_ctx, "😀 teh");
            assert_eq!(correction_range, CorrectionRange { start: 2, end: 5 });
        }
        RequestKind::Completion => panic!("expected grammar request"),
    }
}

#[test]
fn grammar_request_bounds_left_context_to_a_caret_adjacent_tail() {
    // The AX field value is unbounded input; the prompt context must be a
    // bounded tail while correction_range stays in full-field coordinates.
    let field = host_field("grammar-long");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let long_left = format!("{} teh", "word ".repeat(400).trim_end());
    let request = grammar_fix_request(
        &field,
        &text_context_with_right(&field, &long_left, ""),
        grammar_gate(
            &config,
            &config.prefs,
            Some("TextEdit"),
            None,
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .expect("request");

    match request.kind {
        RequestKind::GrammarFix {
            word,
            left_ctx,
            correction_range,
        } => {
            assert_eq!(word, "teh");
            assert!(
                left_ctx.chars().count() <= GRAMMAR_LEFT_CTX_CHARS,
                "left_ctx not bounded: {} chars",
                left_ctx.chars().count()
            );
            assert!(left_ctx.ends_with("teh"), "tail must stay caret-adjacent");
            // Range still addresses the full field value, not the tail.
            let start = long_left.chars().count() - 3;
            assert_eq!(
                correction_range,
                CorrectionRange {
                    start,
                    end: start + 3
                }
            );
        }
        RequestKind::Completion => panic!("expected grammar request"),
    }
}

#[test]
fn grammar_trigger_dispatches_midword_whole_word_range() {
    let field = host_field("grammar-mid");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let request = grammar_fix_request(
        &field,
        &text_context_with_right(&field, "te", "h later"),
        grammar_gate(
            &config,
            &config.prefs,
            Some("TextEdit"),
            None,
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .expect("request");

    match request.kind {
        RequestKind::GrammarFix {
            word,
            correction_range,
            ..
        } => {
            assert_eq!(word, "teh");
            assert_eq!(correction_range, CorrectionRange { start: 0, end: 3 });
        }
        RequestKind::Completion => panic!("expected grammar request"),
    }
}

#[test]
fn grammar_trigger_rejects_an_overlong_word_before_inference() {
    let field = host_field("grammar-overlong");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let long_word = "x".repeat(GRAMMAR_WORD_MAX_CHARS + 1);

    assert!(
        grammar_fix_request(
            &field,
            &text_context_with_right(&field, &long_word, ""),
            grammar_gate(
                &config,
                &config.prefs,
                Some("TextEdit"),
                None,
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_none(),
        "unbounded AX words must not become grammar-model prompts"
    );
}

#[test]
fn grammar_detection_blocks_without_fresh_browser_domain_when_domain_rules_exist() {
    let field = field_with_app("com.google.Chrome");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let mut prefs = config.prefs.clone();
    prefs.excluded_domains.insert("docs.example.com".into());

    assert!(grammar_fix_request(
        &field,
        &text_context_with_right(&field, "teh", ""),
        grammar_gate(
            &config,
            &prefs,
            Some("com.google.Chrome"),
            None,
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .is_none());
    assert!(grammar_fix_request(
        &field,
        &text_context_with_right(&field, "teh", ""),
        grammar_gate(
            &config,
            &prefs,
            Some("com.google.Chrome"),
            Some("docs.example.com"),
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .is_none());
    assert!(grammar_fix_request(
        &field,
        &text_context_with_right(&field, "teh", ""),
        grammar_gate(
            &config,
            &prefs,
            Some("com.google.Chrome"),
            Some("public.example.com"),
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .is_some());
}

#[test]
fn grammar_detection_refresh_drops_stale_allowed_browser_domain() {
    let field = field_with_app("com.google.Chrome");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let mut prefs = config.prefs.clone();
    prefs.excluded_domains.insert("blocked.example".into());
    let mut cache = Some((
        "com.google.Chrome".to_string(),
        "allowed.example".to_string(),
    ));

    let refreshed_domain = typing_domain(&mut cache, Some("com.google.Chrome"), true, None);

    assert_eq!(refreshed_domain, None);
    assert!(grammar_fix_request(
        &field,
        &text_context_with_right(&field, "teh", ""),
        grammar_gate(
            &config,
            &prefs,
            Some("com.google.Chrome"),
            refreshed_domain.as_deref(),
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .is_none());
}

#[test]
fn grammar_detection_refresh_reads_current_browser_url_before_gating() {
    let field = field_with_app("com.google.Chrome");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let mut prefs = config.prefs.clone();
    prefs.excluded_domains.insert("blocked.example".into());
    let calls = std::cell::Cell::new(0);
    let mut cache = Some((
        "com.google.Chrome".to_string(),
        "allowed.example".to_string(),
    ));

    let refreshed_domain =
        typing_domain_for_current_field(&mut cache, Some("com.google.Chrome"), true, || {
            calls.set(calls.get() + 1);
            Some("https://blocked.example/doc".to_string())
        });

    assert_eq!(calls.get(), 1);
    assert_eq!(refreshed_domain.as_deref(), Some("blocked.example"));
    assert!(grammar_fix_request(
        &field,
        &text_context_with_right(&field, "teh", ""),
        grammar_gate(
            &config,
            &prefs,
            Some("com.google.Chrome"),
            refreshed_domain.as_deref(),
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .is_none());
}

#[test]
fn grammar_detection_refresh_allows_current_allowed_browser_url() {
    let field = field_with_app("com.google.Chrome");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let mut prefs = config.prefs.clone();
    prefs.excluded_domains.insert("blocked.example".into());
    let calls = std::cell::Cell::new(0);
    let mut cache = Some((
        "com.google.Chrome".to_string(),
        "blocked.example".to_string(),
    ));

    let refreshed_domain =
        typing_domain_for_current_field(&mut cache, Some("com.google.Chrome"), true, || {
            calls.set(calls.get() + 1);
            Some("https://allowed.example/doc".to_string())
        });

    assert_eq!(calls.get(), 1);
    assert_eq!(refreshed_domain.as_deref(), Some("allowed.example"));
    assert!(grammar_fix_request(
        &field,
        &text_context_with_right(&field, "teh", ""),
        grammar_gate(
            &config,
            &prefs,
            Some("com.google.Chrome"),
            refreshed_domain.as_deref(),
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .is_some());
}

#[test]
fn manual_grammar_request_uses_fresh_browser_url_before_gating() {
    let field = field_with_app("com.google.Chrome");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let mut prefs = config.prefs.clone();
    prefs.excluded_domains.insert("blocked.example".into());
    let calls = std::cell::Cell::new(0);
    let mut cache = Some((
        "com.google.Chrome".to_string(),
        "allowed.example".to_string(),
    ));

    let request = manual_grammar_request_for_current_field(
        ManualGrammarRequestInputs {
            field: &field,
            ctx: &text_context_with_right(&field, "teh", ""),
            caps: &writable_axset_caps(),
            config: &config,
            prefs: &prefs,
            app_key: Some("com.google.Chrome"),
            enabled: true,
            now_ms: 0,
        },
        &mut cache,
        || {
            calls.set(calls.get() + 1);
            Some("https://blocked.example/doc".to_string())
        },
    );

    assert_eq!(calls.get(), 1);
    assert!(
            request.is_none(),
            "manual grammar shortcut must not arm from stale allowed cache after the current URL is excluded"
        );
}

#[test]
fn grammar_detection_respects_enable_per_app_snooze_and_axset() {
    let field = host_field("grammar-gates");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let ctx = text_context_with_right(&field, "teh", "");
    assert!(grammar_fix_request(
        &field,
        &ctx,
        grammar_gate(
            &config,
            &config.prefs,
            Some("TextEdit"),
            None,
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .is_some());

    assert!(grammar_fix_request(
        &field,
        &ctx,
        grammar_gate(
            &config,
            &config.prefs,
            Some("TextEdit"),
            None,
            false,
            &writable_axset_caps(),
            0,
        ),
    )
    .is_none());

    let mut prefs = config.prefs.clone();
    prefs.set_app_policy_field("TextEdit", prefs::AppPolicyField::GrammarFix, false);
    assert!(grammar_fix_request(
        &field,
        &ctx,
        grammar_gate(
            &config,
            &prefs,
            Some("TextEdit"),
            None,
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .is_none());

    let mut prefs = config.prefs.clone();
    prefs.snooze_app("TextEdit", 0, 60);
    assert!(grammar_fix_request(
        &field,
        &ctx,
        grammar_gate(
            &config,
            &prefs,
            Some("TextEdit"),
            None,
            true,
            &writable_axset_caps(),
            1,
        ),
    )
    .is_none());

    let mut caps = writable_axset_caps();
    caps.insert_strategy = InsertStrategy::SyntheticKeys;
    assert!(grammar_fix_request(
        &field,
        &ctx,
        grammar_gate(
            &config,
            &config.prefs,
            Some("TextEdit"),
            None,
            true,
            &caps,
            0,
        ),
    )
    .is_none());
}

#[test]
fn grammar_detection_allows_per_app_on_override_when_global_default_is_off() {
    let field = host_field("grammar-app-override");
    let config = Config::from_lookup(lookup(&[]));
    let ctx = text_context_with_right(&field, "teh", "");

    assert!(
        grammar_fix_request(
            &field,
            &ctx,
            grammar_gate(
                &config,
                &config.prefs,
                Some("TextEdit"),
                None,
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_none(),
        "global grammar off with no app override must block"
    );

    let mut prefs = config.prefs.clone();
    prefs.set_app_policy_field("TextEdit", prefs::AppPolicyField::GrammarFix, true);

    assert!(
            grammar_fix_request(
                &field,
                &ctx,
                grammar_gate(
                    &config,
                    &prefs,
                    Some("TextEdit"),
                    None,
                    true,
                    &writable_axset_caps(),
                    0,
                ),
            )
            .is_some(),
            "Apps-pane grammar override must enable the focused app even when the global default is off"
        );
}

#[test]
fn grammar_pre_read_policy_blocks_disabled_paths_before_ax_text() {
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let app = Some("TextEdit");
    let mut cache = None;
    assert!(grammar_pre_read_policy_passes(
        &config,
        &config.prefs,
        app,
        true,
        0,
        &mut cache,
        || None,
    ));

    let mut cache = None;
    assert!(!grammar_pre_read_policy_passes(
        &config,
        &config.prefs,
        app,
        false,
        0,
        &mut cache,
        || None,
    ));

    let mut prefs = config.prefs.clone();
    prefs.set_app_policy_field("TextEdit", prefs::AppPolicyField::GrammarFix, false);
    let mut cache = None;
    assert!(!grammar_pre_read_policy_passes(
        &config,
        &prefs,
        app,
        true,
        0,
        &mut cache,
        || None,
    ));

    let mut prefs = config.prefs.clone();
    prefs.snooze_app("TextEdit", 0, 60);
    let mut cache = None;
    assert!(!grammar_pre_read_policy_passes(
        &config,
        &prefs,
        app,
        true,
        1,
        &mut cache,
        || None,
    ));

    let mut prefs = config.prefs.clone();
    prefs.excluded_domains.insert("blocked.example".into());
    let url_reads = std::cell::Cell::new(0);
    let mut cache = Some((
        "com.google.Chrome".to_string(),
        "allowed.example".to_string(),
    ));
    assert!(!grammar_pre_read_policy_passes(
        &config,
        &prefs,
        Some("com.google.Chrome"),
        true,
        0,
        &mut cache,
        || {
            url_reads.set(url_reads.get() + 1);
            Some("https://blocked.example/doc".to_string())
        },
    ));
    assert_eq!(url_reads.get(), 1);
}

fn grammar_shortcut_probe(
    config: &Config,
    prefs: &Prefs,
    enabled: bool,
    app: &str,
    now_ms: u64,
    cached_domain_entry: Option<(String, String)>,
    focused_url: Option<&str>,
) -> (GrammarCheckShortcutOutcome, usize, usize, usize) {
    let field = field_with_app(app);
    let mut cache = cached_domain_entry;
    let read_count = std::cell::Cell::new(0);
    let caps_count = std::cell::Cell::new(0);
    let url_count = std::cell::Cell::new(0);
    let outcome = handle_grammar_check_shortcut(GrammarCheckShortcutArgs {
        current_field: Some(field.clone()),
        config,
        prefs,
        enabled,
        now_ms,
        last_domain: &mut cache,
        resolve_app_key: |field: FieldHandle| Some(field.app.clone()),
        focused_page_url: |_: FieldHandle| {
            url_count.set(url_count.get() + 1);
            focused_url.map(str::to_string)
        },
        read_context: |field: FieldHandle| {
            read_count.set(read_count.get() + 1);
            Ok(text_context_with_right(&field, "teh", ""))
        },
        capabilities: |_: FieldHandle| {
            caps_count.set(caps_count.get() + 1);
            Ok(writable_axset_caps())
        },
        arm_manual_grammar_request: |_: FieldHandle| Some((77, 88)),
    });
    (outcome, read_count.get(), caps_count.get(), url_count.get())
}

#[test]
fn grammar_check_shortcut_blocks_policy_before_read_context() {
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));

    let (_, reads, caps, urls) =
        grammar_shortcut_probe(&config, &config.prefs, false, "TextEdit", 0, None, None);
    assert_eq!((reads, caps, urls), (0, 0, 0));

    let mut prefs = config.prefs.clone();
    prefs.set_app_policy_field("TextEdit", prefs::AppPolicyField::GrammarFix, false);
    let (_, reads, caps, urls) =
        grammar_shortcut_probe(&config, &prefs, true, "TextEdit", 0, None, None);
    assert_eq!((reads, caps, urls), (0, 0, 0));

    let mut prefs = config.prefs.clone();
    prefs.snooze_app("TextEdit", 0, 60);
    let (_, reads, caps, urls) =
        grammar_shortcut_probe(&config, &prefs, true, "TextEdit", 1, None, None);
    assert_eq!((reads, caps, urls), (0, 0, 0));

    let mut prefs = config.prefs.clone();
    prefs.excluded_domains.insert("blocked.example".into());
    let (_, reads, caps, urls) = grammar_shortcut_probe(
        &config,
        &prefs,
        true,
        "com.google.Chrome",
        0,
        Some((
            "com.google.Chrome".to_string(),
            "allowed.example".to_string(),
        )),
        Some("https://blocked.example/doc"),
    );
    assert_eq!((reads, caps, urls), (0, 0, 1));

    let (outcome, reads, caps, urls) =
        grammar_shortcut_probe(&config, &config.prefs, true, "TextEdit", 0, None, None);
    assert_eq!((reads, caps, urls), (1, 1, 0));
    match outcome {
        GrammarCheckShortcutOutcome::Armed(request) => {
            assert_eq!(request.generation, 77);
            assert_eq!(request.snapshot, 88);
            assert!(matches!(request.kind, RequestKind::GrammarFix { .. }));
        }
        other => panic!("expected armed grammar request, got {other:?}"),
    }
}

#[test]
fn grammar_check_shortcut_surfaces_read_context_error_without_capability_or_arm() {
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let field = field_with_app("TextEdit");
    let mut cache = None;
    let caps_count = std::cell::Cell::new(0);
    let arm_count = std::cell::Cell::new(0);

    let outcome = handle_grammar_check_shortcut(GrammarCheckShortcutArgs {
        current_field: Some(field),
        config: &config,
        prefs: &config.prefs,
        enabled: true,
        now_ms: 0,
        last_domain: &mut cache,
        resolve_app_key: |field: FieldHandle| Some(field.app.clone()),
        focused_page_url: |_: FieldHandle| None,
        read_context: |_| Err(PlatformError::Timeout),
        capabilities: |_| {
            caps_count.set(caps_count.get() + 1);
            Ok(writable_axset_caps())
        },
        arm_manual_grammar_request: |_| {
            arm_count.set(arm_count.get() + 1);
            Some((77, 88))
        },
    });

    assert!(matches!(
        outcome,
        GrammarCheckShortcutOutcome::ReadContextError(PlatformError::Timeout)
    ));
    assert_eq!(caps_count.get(), 0);
    assert_eq!(arm_count.get(), 0);
}

#[test]
fn caret_event_with_failing_context_read_still_establishes_the_current_field() {
    // The HostEvent::Caret arm routes through establish_caret_field_then_read:
    // the host's reported field becomes current BEFORE the context read is
    // attempted. Keeping the prior field on a read failure (the audit gap)
    // would leave shortcuts — e.g. the grammar check, which resolves against
    // focus.current_field — targeting stale focus after a transient AX error.
    let stale = field_with_app("com.apple.Notes");
    let reported = field_with_app("com.apple.TextEdit");
    let mut current_field = Some(stale);

    let result = establish_caret_field_then_read(&mut current_field, &reported, || {
        Err(PlatformError::Timeout)
    });

    assert_eq!(result.err(), Some(PlatformError::Timeout));
    assert_eq!(
        current_field.as_ref(),
        Some(&reported),
        "an unreadable caret event must still retarget the current field"
    );

    // The readable path establishes the field the same way and hands the
    // context through untouched.
    let next = field_with_app("com.google.Chrome");
    let result = establish_caret_field_then_read(&mut current_field, &next, || {
        Ok(text_context(&next, "hello"))
    });

    assert_eq!(result.map(|ctx| ctx.left), Ok("hello".to_string()));
    assert_eq!(current_field.as_ref(), Some(&next));
}

#[test]
fn grammar_check_shortcut_surfaces_capabilities_error_without_arm() {
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let field = field_with_app("TextEdit");
    let mut cache = None;
    let read_count = std::cell::Cell::new(0);
    let arm_count = std::cell::Cell::new(0);

    let outcome = handle_grammar_check_shortcut(GrammarCheckShortcutArgs {
        current_field: Some(field.clone()),
        config: &config,
        prefs: &config.prefs,
        enabled: true,
        now_ms: 0,
        last_domain: &mut cache,
        resolve_app_key: |field: FieldHandle| Some(field.app.clone()),
        focused_page_url: |_: FieldHandle| None,
        read_context: |field: FieldHandle| {
            read_count.set(read_count.get() + 1);
            Ok(text_context_with_right(&field, "teh", ""))
        },
        capabilities: |_| Err(PlatformError::Timeout),
        arm_manual_grammar_request: |_| {
            arm_count.set(arm_count.get() + 1);
            Some((77, 88))
        },
    });

    assert!(matches!(
        outcome,
        GrammarCheckShortcutOutcome::CapabilitiesError(PlatformError::Timeout)
    ));
    assert_eq!(read_count.get(), 1);
    assert_eq!(arm_count.get(), 0);
}

#[test]
fn grammar_detection_rejects_non_empty_selection() {
    let field = host_field("grammar-selection");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let mut ctx = text_context_with_right(&field, "teh", "");
    ctx.selection = Some(platform::TextRange { start: 0, end: 1 });

    assert!(grammar_fix_request(
        &field,
        &ctx,
        grammar_gate(
            &config,
            &config.prefs,
            Some("TextEdit"),
            None,
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .is_none());
}

#[test]
fn grammar_detection_allows_collapsed_selection() {
    // AX providers may report the caret as an empty selection range rather
    // than None: a collapsed range (start == end) is no selection and must
    // not block grammar fix. Pins the `start != range.end` conjunct.
    let field = host_field("grammar-collapsed-selection");
    let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
    let mut ctx = text_context_with_right(&field, "teh", "");
    ctx.selection = Some(platform::TextRange { start: 3, end: 3 });

    assert!(grammar_fix_request(
        &field,
        &ctx,
        grammar_gate(
            &config,
            &config.prefs,
            Some("TextEdit"),
            None,
            true,
            &writable_axset_caps(),
            0,
        ),
    )
    .is_some());
}

fn typed_change_after_baseline(
    field: &FieldHandle,
    baseline: &str,
    next: &str,
) -> engine::TextChange {
    let mut tracker = FieldTracker::new();
    let _ = tracker.observe_with_inserted_text(
        field,
        &text_context(field, baseline),
        TriggerPolicy::Automatic,
        1,
    );
    match tracker.observe_with_inserted_text(
        field,
        &text_context(field, next),
        TriggerPolicy::Automatic,
        2,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    }
}

fn accepted_store() -> memory::MemoryStore {
    memory::MemoryStore::open_in_memory(
        &memory::StaticKey([3u8; 32]),
        memory::StorageMode::AcceptedOnly,
    )
    .expect("open in-memory store")
}

fn queue_and_flush_monitored(
    change: &engine::TextChange,
    store: &memory::MemoryStore,
    prefs: &Prefs,
    enabled: bool,
    secure: bool,
) {
    queue_and_flush_monitored_for_app(change, store, prefs, enabled, secure, None);
}

fn queue_and_flush_monitored_for_app(
    change: &engine::TextChange,
    store: &memory::MemoryStore,
    prefs: &Prefs,
    enabled: bool,
    secure: bool,
    domain: Option<&str>,
) {
    let mut pending = Vec::new();
    let mut buffers = HashMap::new();
    enqueue_monitored_change(
        &mut pending,
        change,
        Some(change.field.app.clone()),
        domain.map(str::to_owned),
    );
    flush_monitored_changes(
        &mut pending,
        &mut buffers,
        Some(store),
        prefs,
        monitored_policy(enabled, secure, true, 1_000),
    );
}

fn queue_and_flush_monitored_with_buffers(
    change: &engine::TextChange,
    buffers: &mut HashMap<FieldHandle, MonitoredBuffer>,
    store: &memory::MemoryStore,
    prefs: &Prefs,
    enabled: bool,
    secure: bool,
) {
    let mut pending = Vec::new();
    enqueue_monitored_change(&mut pending, change, Some(change.field.app.clone()), None);
    flush_monitored_changes(
        &mut pending,
        buffers,
        Some(store),
        prefs,
        monitored_policy(enabled, secure, true, 1_000),
    );
}

fn monitored_policy(enabled: bool, secure: bool, trusted: bool, now_ms: u64) -> MonitoredPolicy {
    MonitoredPolicy {
        enabled,
        secure,
        trusted,
        now_ms,
    }
}

fn assert_policy_transition_clears_buffered_monitored_text() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([13u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let prefs = Prefs::default();
    let mut tracker = FieldTracker::new();
    let _ = tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, ""),
        TriggerPolicy::Automatic,
        1,
    );
    let partial = match tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, "secret"),
        TriggerPolicy::Automatic,
        2,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };
    let mut buffers = HashMap::new();
    queue_and_flush_monitored_with_buffers(&partial, &mut buffers, &store, &prefs, true, false);
    assert_eq!(store.count().unwrap(), 0);
    assert!(!buffers.is_empty());

    let mut pending = Vec::new();
    enqueue_monitored_change(
        &mut pending,
        &partial,
        Some("com.apple.TextEdit".into()),
        None,
    );
    assert!(!pending.is_empty());
    clear_monitored_state_for_policy_transition(&mut pending, &mut buffers);
    assert!(pending.is_empty());
    assert!(buffers.is_empty());
    let boundary = match tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, "secret "),
        TriggerPolicy::Automatic,
        3,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };
    queue_and_flush_monitored_with_buffers(&boundary, &mut buffers, &store, &prefs, true, false);
    assert_eq!(store.recent("com.apple.TextEdit", 10).unwrap(), vec![" "]);
}

#[test]
fn full_accept_records_to_both_sinks_under_a_resolved_bundle_id() {
    let prev = PreviousInputs::default();
    let store = accepted_store();
    record_full_accept(
        AcceptAction::Full,
        &field_with_app("com.apple.TextEdit"),
        "the quick brown fox",
        AcceptRecording {
            context_max_chars: 160,
            cross_app_previous_inputs: false,
            previous_inputs: &prev,
            memory: Some(&store),
            collection_allowed: true,
        },
    );
    assert_eq!(store.count().unwrap(), 1);
    assert_eq!(prev.recent("com.apple.TextEdit").len(), 1);
}

#[test]
fn full_accept_collects_cross_app_history_only_while_the_opt_in_is_on() {
    let prev = PreviousInputs::default();
    let field = field_with_app("com.apple.TextEdit");
    record_full_accept(
        AcceptAction::Full,
        &field,
        "private while off",
        AcceptRecording {
            context_max_chars: 160,
            cross_app_previous_inputs: false,
            previous_inputs: &prev,
            memory: None,
            collection_allowed: true,
        },
    );
    assert!(prev.recent_for_scope("com.apple.TextEdit", true).is_empty());

    record_full_accept(
        AcceptAction::Full,
        &field,
        "shared while on",
        AcceptRecording {
            context_max_chars: 160,
            cross_app_previous_inputs: true,
            previous_inputs: &prev,
            memory: None,
            collection_allowed: true,
        },
    );
    assert_eq!(
        prev.recent_for_scope("com.apple.TextEdit", true),
        vec!["shared while on"]
    );
    assert_eq!(
        prev.recent("com.apple.TextEdit"),
        vec!["shared while on", "private while off"]
    );
}

#[test]
fn word_accept_records_nothing() {
    let prev = PreviousInputs::default();
    let store = accepted_store();
    record_full_accept(
        AcceptAction::Word,
        &field_with_app("com.apple.TextEdit"),
        "fox",
        AcceptRecording {
            context_max_chars: 160,
            cross_app_previous_inputs: false,
            previous_inputs: &prev,
            memory: Some(&store),
            collection_allowed: true,
        },
    );
    assert_eq!(store.count().unwrap(), 0);
    assert!(prev.recent("com.apple.TextEdit").is_empty());
}

#[test]
fn full_accept_under_a_volatile_pid_key_records_nothing() {
    let prev = PreviousInputs::default();
    let store = accepted_store();
    record_full_accept(
        AcceptAction::Full,
        &field_with_app("pid:42"),
        "ignored",
        AcceptRecording {
            context_max_chars: 160,
            cross_app_previous_inputs: false,
            previous_inputs: &prev,
            memory: Some(&store),
            collection_allowed: true,
        },
    );
    assert_eq!(store.count().unwrap(), 0);
    assert!(prev.recent("pid:42").is_empty());
}

#[test]
fn full_accept_with_context_disabled_still_records_to_memory() {
    // context_max_chars == 0 disables the previous-input ring, but the
    // encrypted store (its own opt-in) still records.
    let prev = PreviousInputs::default();
    let store = accepted_store();
    record_full_accept(
        AcceptAction::Full,
        &field_with_app("com.apple.TextEdit"),
        "remembered",
        AcceptRecording {
            context_max_chars: 0,
            cross_app_previous_inputs: false,
            previous_inputs: &prev,
            memory: Some(&store),
            collection_allowed: true,
        },
    );
    assert_eq!(store.count().unwrap(), 1);
    assert!(prev.recent("com.apple.TextEdit").is_empty());
}

#[test]
fn all_monitored_records_typed_field_text_after_established_baseline() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([4u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let mut tracker = FieldTracker::new();
    let first = match tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, "pre-existing draft"),
        TriggerPolicy::Automatic,
        1,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("first non-empty snapshot is typed"),
    };
    let prefs = Prefs::default();
    queue_and_flush_monitored(&first, &store, &prefs, true, false);
    assert_eq!(
        store.count().unwrap(),
        0,
        "baseline snapshot is not user typing"
    );

    let second = match tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, "pre-existing draft! "),
        TriggerPolicy::Automatic,
        2,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("second snapshot changed text"),
    };
    queue_and_flush_monitored(&second, &store, &prefs, true, false);
    assert_eq!(store.recent("com.apple.TextEdit", 10).unwrap(), vec!["! "]);
}

#[test]
fn all_monitored_records_first_typed_text_after_empty_baseline() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([7u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(&field, "", "h ");
    let prefs = Prefs::default();
    queue_and_flush_monitored(&change, &store, &prefs, true, false);
    assert_eq!(store.recent("com.apple.TextEdit", 10).unwrap(), vec!["h "]);
}

#[test]
fn all_monitored_buffers_char_by_char_text_until_redactable_boundary() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([11u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let prefs = Prefs::default();
    let mut tracker = FieldTracker::new();
    let _ = tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, ""),
        TriggerPolicy::Automatic,
        1,
    );
    let mut buffers = HashMap::new();
    for (idx, value) in [
        "a",
        "ad",
        "ada",
        "ada@",
        "ada@e",
        "ada@ex",
        "ada@example",
        "ada@example.",
        "ada@example.com",
    ]
    .into_iter()
    .enumerate()
    {
        let change = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, value),
            TriggerPolicy::Automatic,
            (idx + 2) as u64,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        queue_and_flush_monitored_with_buffers(&change, &mut buffers, &store, &prefs, true, false);
        assert_eq!(store.count().unwrap(), 0);
    }

    let change = match tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, "ada@example.com "),
        TriggerPolicy::Automatic,
        99,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };
    queue_and_flush_monitored_with_buffers(&change, &mut buffers, &store, &prefs, true, false);
    assert_eq!(
        store.recent("com.apple.TextEdit", 10).unwrap(),
        vec!["[redacted-email] "]
    );
}

#[test]
fn accepted_only_does_not_record_monitored_typing() {
    let store = accepted_store();
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
    let prefs = Prefs::default();
    queue_and_flush_monitored(&change, &store, &prefs, true, false);
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn all_monitored_browser_domains_use_fresh_cached_domain_rules() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([14u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.Safari");
    let mut prefs = Prefs::default();
    prefs.excluded_domains.insert("sensitive.example".into());

    let allowed = typed_change_after_baseline(&field, "", "allowed browser text ");
    queue_and_flush_monitored_for_app(&allowed, &store, &prefs, true, false, Some("other.example"));
    assert_eq!(
        store.recent("com.apple.Safari", 10).unwrap(),
        vec!["allowed browser text "]
    );

    let blocked = typed_change_after_baseline(&field, "", "blocked browser text ");
    queue_and_flush_monitored_for_app(
        &blocked,
        &store,
        &prefs,
        true,
        false,
        Some("docs.sensitive.example"),
    );
    assert_eq!(
        store.recent("com.apple.Safari", 10).unwrap(),
        vec!["allowed browser text "]
    );
}

#[test]
fn monitored_typing_honors_collection_privacy_gates() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([5u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
    let mut prefs = Prefs::default();
    prefs
        .per_app
        .entry("com.apple.TextEdit".into())
        .or_default()
        .collect_inputs = Some(false);
    queue_and_flush_monitored(&change, &store, &prefs, true, false);
    assert_eq!(store.count().unwrap(), 0);

    let volatile = field_with_app("pid:42");
    let change = typed_change_after_baseline(&volatile, "", "ordinary typed text ");
    let prefs = Prefs::default();
    queue_and_flush_monitored(&change, &store, &prefs, true, false);
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn monitored_typing_honors_disabled_and_excluded_app_blocks() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([18u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(&field, "", "ordinary typed text ");

    let mut disabled = Prefs::default();
    disabled
        .per_app
        .entry("com.apple.TextEdit".into())
        .or_default()
        .enabled = Some(false);
    queue_and_flush_monitored(&change, &store, &disabled, true, false);
    assert_eq!(store.count().unwrap(), 0);

    let mut excluded = Prefs::default();
    excluded.excluded_apps.insert("com.apple.TextEdit".into());
    queue_and_flush_monitored(&change, &store, &excluded, true, false);
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn monitored_typing_uses_field_app_fallback_when_app_key_missing() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([28u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(&field, "", "ordinary typed text ");

    let mut excluded = Prefs::default();
    excluded.excluded_apps.insert("com.apple.TextEdit".into());
    let mut pending = Vec::new();
    enqueue_monitored_change(&mut pending, &change, None, None);
    assert_eq!(
        pending[0].app_key.as_deref(),
        Some("com.apple.TextEdit"),
        "stable field app must be used when pid resolution missed"
    );
    flush_monitored_changes(
        &mut pending,
        &mut HashMap::new(),
        Some(&store),
        &excluded,
        monitored_policy(true, false, true, 1_000),
    );
    assert_eq!(store.count().unwrap(), 0);

    let mut snoozed = Prefs::default();
    snoozed.snooze_app("com.apple.TextEdit", 1_000, 60);
    let mut pending = Vec::new();
    enqueue_monitored_change(&mut pending, &change, None, None);
    flush_monitored_changes(
        &mut pending,
        &mut HashMap::new(),
        Some(&store),
        &snoozed,
        monitored_policy(true, false, true, 1_001),
    );
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn collection_off_drops_partial_monitored_buffer_before_reenable() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([12u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let mut tracker = FieldTracker::new();
    let _ = tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, ""),
        TriggerPolicy::Automatic,
        1,
    );
    let partial = match tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, "secret"),
        TriggerPolicy::Automatic,
        2,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };

    let mut pending = Vec::new();
    let mut buffers = HashMap::new();
    enqueue_monitored_change(
        &mut pending,
        &partial,
        Some("com.apple.TextEdit".into()),
        None,
    );
    let mut prefs = Prefs::default();
    prefs
        .per_app
        .entry("com.apple.TextEdit".into())
        .or_default()
        .collect_inputs = Some(false);
    flush_monitored_changes(
        &mut pending,
        &mut buffers,
        Some(&store),
        &prefs,
        monitored_policy(true, false, true, 1_000),
    );
    assert!(buffers.is_empty());

    let boundary = match tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, "secret "),
        TriggerPolicy::Automatic,
        3,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };
    let mut pending = Vec::new();
    enqueue_monitored_change(
        &mut pending,
        &boundary,
        Some("com.apple.TextEdit".into()),
        None,
    );
    flush_monitored_changes(
        &mut pending,
        &mut buffers,
        Some(&store),
        &Prefs::default(),
        monitored_policy(true, false, true, 1_001),
    );
    assert_eq!(store.recent("com.apple.TextEdit", 10).unwrap(), vec![" "]);
}

#[test]
fn oversized_monitored_insert_persists_no_user_text() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([21u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let oversized = format!("{} ", "x".repeat(MAX_MONITORED_BUFFER_CHARS + 1));
    let change = typed_change_after_baseline(&field, "", &oversized);
    queue_and_flush_monitored(&change, &store, &Prefs::default(), true, false);

    assert_eq!(store.count().unwrap(), 0);
    assert_eq!(
        store.recent("com.apple.TextEdit", 10).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn oversized_monitored_insert_with_boundary_clears_partial_buffer() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([15u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let prefs = Prefs::default();
    let mut tracker = FieldTracker::new();
    let _ = tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, ""),
        TriggerPolicy::Automatic,
        1,
    );
    let partial = match tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, "secret"),
        TriggerPolicy::Automatic,
        2,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };
    let mut buffers = HashMap::new();
    queue_and_flush_monitored_with_buffers(&partial, &mut buffers, &store, &prefs, true, false);
    assert!(!buffers.is_empty());

    let oversized = format!("secret{} ", "x".repeat(MAX_MONITORED_BUFFER_CHARS + 1));
    let change = match tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, &oversized),
        TriggerPolicy::Automatic,
        3,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };
    queue_and_flush_monitored_with_buffers(&change, &mut buffers, &store, &prefs, true, false);
    assert!(buffers.is_empty());

    let boundary = format!("{oversized} ");
    let change = match tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, &boundary),
        TriggerPolicy::Automatic,
        4,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };
    queue_and_flush_monitored_with_buffers(&change, &mut buffers, &store, &prefs, true, false);
    assert_eq!(store.recent("com.apple.TextEdit", 10).unwrap(), vec![" "]);
}

#[test]
fn monitored_overflow_drops_until_next_boundary() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([14u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let prefs = Prefs::default();
    let mut tracker = FieldTracker::new();
    let _ = tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, ""),
        TriggerPolicy::Automatic,
        1,
    );
    let mut buffers = HashMap::new();
    let mut value = String::new();
    for idx in 0..=MAX_MONITORED_BUFFER_CHARS {
        value.push('x');
        let change = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, &value),
            TriggerPolicy::Automatic,
            (idx + 2) as u64,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        queue_and_flush_monitored_with_buffers(&change, &mut buffers, &store, &prefs, true, false);
    }
    assert_eq!(
        buffers.get(&field),
        Some(&MonitoredBuffer::DroppedUntilBoundary)
    );

    value.push(' ');
    let change = match tracker.observe_with_inserted_text(
        &field,
        &text_context(&field, &value),
        TriggerPolicy::Automatic,
        999,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };
    queue_and_flush_monitored_with_buffers(&change, &mut buffers, &store, &prefs, true, false);
    assert!(buffers.is_empty());
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn browser_domain_rule_without_fresh_domain_blocks_replacement_offer() {
    let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
    let mut prefs = Prefs::default();
    prefs.excluded_domains.insert("bank.example".into());
    let app = Some("com.apple.Safari");
    let decision = if browser_domain_fresh_enough_for_rules(app, None, &prefs) {
        replacement_decision("hi :smile", &config, &prefs, app, None, true, 0)
    } else {
        None
    };
    assert!(decision.is_none());
}

#[test]
fn policy_transition_drops_partial_monitored_buffer_before_reuse() {
    assert_policy_transition_clears_buffered_monitored_text();
}

#[test]
fn monitored_typing_stops_when_context_collection_is_blocked() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([6u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
    let prefs = Prefs::default();
    queue_and_flush_monitored(&change, &store, &prefs, true, true);
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn monitored_typing_uses_fresh_browser_url_before_persisting() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([20u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.google.Chrome");
    let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
    let mut prefs = Prefs::default();
    prefs.excluded_domains.insert("blocked.example".into());
    let mut cache = Some((
        "com.google.Chrome".to_string(),
        "allowed.example".to_string(),
    ));
    let calls = std::cell::Cell::new(0);
    let mut pending = Vec::new();
    let mut buffers = HashMap::new();

    let domain = enqueue_monitored_change_for_current_domain(
        &mut pending,
        &mut cache,
        &change,
        Some("com.google.Chrome".to_string()),
        true,
        || {
            calls.set(calls.get() + 1);
            Some("https://blocked.example/doc".to_string())
        },
    );
    flush_monitored_changes(
        &mut pending,
        &mut buffers,
        Some(&store),
        &prefs,
        monitored_policy(true, false, true, 1_000),
    );

    assert_eq!(calls.get(), 1);
    assert_eq!(domain.as_deref(), Some("blocked.example"));
    assert_eq!(store.count().unwrap(), 0);
    assert!(buffers.is_empty());
}

#[test]
fn secure_policy_clears_buffered_monitored_text_without_boundary() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([16u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let mut buffers = HashMap::from([(
        field.clone(),
        MonitoredBuffer::Collecting("partial secret".into()),
    )]);
    let mut pending = Vec::new();

    flush_monitored_changes(
        &mut pending,
        &mut buffers,
        Some(&store),
        &Prefs::default(),
        monitored_policy(true, true, true, 1_001),
    );

    assert!(pending.is_empty());
    assert!(buffers.is_empty());
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn monitored_flush_rechecks_secure_input_before_persisting_pending_text() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([17u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
    let mut pending = Vec::new();
    enqueue_monitored_change(
        &mut pending,
        &change,
        Some("com.apple.TextEdit".into()),
        None,
    );
    let mut buffers = HashMap::new();
    let mut secure = false;
    let mut last_secure_poll_ms = None;
    let mut probe_called = false;

    flush_monitored_changes_after_secure_recheck(
        &mut pending,
        &mut buffers,
        Some(&store),
        &Prefs::default(),
        MonitoredFlushState {
            secure: &mut secure,
            last_secure_poll_ms: &mut last_secure_poll_ms,
        },
        MonitoredFlushRuntime {
            monitored_memory_active: true,
            enabled: true,
            trusted: true,
            now_ms: 1_001,
        },
        || {
            probe_called = true;
            true
        },
    );

    assert!(probe_called);
    assert!(secure);
    assert_eq!(last_secure_poll_ms, Some(1_001));
    assert!(pending.is_empty());
    assert!(buffers.is_empty());
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn monitored_flush_persists_when_secure_recheck_clears() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([19u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
    let mut pending = Vec::new();
    enqueue_monitored_change(
        &mut pending,
        &change,
        Some("com.apple.TextEdit".into()),
        None,
    );
    let mut buffers = HashMap::new();
    let mut secure = true;
    let mut last_secure_poll_ms = None;
    let mut probe_called = false;

    flush_monitored_changes_after_secure_recheck(
        &mut pending,
        &mut buffers,
        Some(&store),
        &Prefs::default(),
        MonitoredFlushState {
            secure: &mut secure,
            last_secure_poll_ms: &mut last_secure_poll_ms,
        },
        MonitoredFlushRuntime {
            monitored_memory_active: true,
            enabled: true,
            trusted: true,
            now_ms: 1_002,
        },
        || {
            probe_called = true;
            false
        },
    );

    assert!(probe_called);
    assert!(!secure);
    assert_eq!(last_secure_poll_ms, Some(1_002));
    assert!(pending.is_empty());
    assert!(buffers.is_empty());
    assert_eq!(
        store.recent("com.apple.TextEdit", 10).unwrap(),
        vec!["ordinary typed text "]
    );
}

#[test]
fn monitored_flush_blocks_relaunch_required_effective_untrusted_runtime() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([25u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
    let mut pending = Vec::new();
    enqueue_monitored_change(
        &mut pending,
        &change,
        Some("com.apple.TextEdit".into()),
        None,
    );
    let mut buffers = HashMap::new();
    let mut secure = true;
    let mut last_secure_poll_ms = None;
    let mut probe_called = false;

    flush_monitored_changes_after_secure_recheck(
        &mut pending,
        &mut buffers,
        Some(&store),
        &Prefs::default(),
        MonitoredFlushState {
            secure: &mut secure,
            last_secure_poll_ms: &mut last_secure_poll_ms,
        },
        MonitoredFlushRuntime {
            monitored_memory_active: true,
            enabled: true,
            trusted: runtime_trusted(true, AccessibilitySubscriptions::RelaunchRequired),
            now_ms: 1_004,
        },
        || {
            probe_called = true;
            false
        },
    );

    assert!(probe_called);
    assert!(!secure);
    assert_eq!(last_secure_poll_ms, Some(1_004));
    assert!(pending.is_empty());
    assert!(buffers.is_empty());
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn monitored_flush_skips_secure_recheck_without_pending_work() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([20u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let mut pending = Vec::new();
    let mut buffers = HashMap::new();
    let mut secure = false;
    let mut last_secure_poll_ms = None;

    flush_monitored_changes_after_secure_recheck(
        &mut pending,
        &mut buffers,
        Some(&store),
        &Prefs::default(),
        MonitoredFlushState {
            secure: &mut secure,
            last_secure_poll_ms: &mut last_secure_poll_ms,
        },
        MonitoredFlushRuntime {
            monitored_memory_active: true,
            enabled: true,
            trusted: true,
            now_ms: 1_003,
        },
        || panic!("secure probe must not run without pending monitored work"),
    );

    assert!(!secure);
    assert_eq!(last_secure_poll_ms, None);
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn monitored_flush_rechecks_secure_input_for_buffered_work() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([22u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let mut pending = Vec::new();
    let mut buffers = HashMap::from([(
        field.clone(),
        MonitoredBuffer::Collecting("partial secret".into()),
    )]);
    let mut secure = false;
    let mut last_secure_poll_ms = None;
    let mut probe_called = false;

    flush_monitored_changes_after_secure_recheck(
        &mut pending,
        &mut buffers,
        Some(&store),
        &Prefs::default(),
        MonitoredFlushState {
            secure: &mut secure,
            last_secure_poll_ms: &mut last_secure_poll_ms,
        },
        MonitoredFlushRuntime {
            monitored_memory_active: true,
            enabled: true,
            trusted: true,
            now_ms: 1_004,
        },
        || {
            probe_called = true;
            true
        },
    );

    assert!(probe_called);
    assert!(secure);
    assert_eq!(last_secure_poll_ms, Some(1_004));
    assert!(pending.is_empty());
    assert!(buffers.is_empty());
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn monitored_buffers_are_isolated_per_same_app_field() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([21u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field_a = field_with_app("com.apple.TextEdit");
    let mut field_b = field_with_app("com.apple.TextEdit");
    field_b.element_id = "ax:other-field".into();
    field_b.generation = 2;
    let prefs = Prefs::default();
    let mut tracker_a = FieldTracker::new();
    let mut tracker_b = FieldTracker::new();
    let _ = tracker_a.observe_with_inserted_text(
        &field_a,
        &text_context(&field_a, ""),
        TriggerPolicy::Automatic,
        1,
    );
    let _ = tracker_b.observe_with_inserted_text(
        &field_b,
        &text_context(&field_b, ""),
        TriggerPolicy::Automatic,
        1,
    );
    let partial_a = match tracker_a.observe_with_inserted_text(
        &field_a,
        &text_context(&field_a, "secret"),
        TriggerPolicy::Automatic,
        2,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };
    let partial_b = match tracker_b.observe_with_inserted_text(
        &field_b,
        &text_context(&field_b, "note"),
        TriggerPolicy::Automatic,
        2,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };
    let mut buffers = HashMap::new();
    queue_and_flush_monitored_with_buffers(&partial_a, &mut buffers, &store, &prefs, true, false);
    queue_and_flush_monitored_with_buffers(&partial_b, &mut buffers, &store, &prefs, true, false);
    assert_eq!(store.count().unwrap(), 0);
    assert!(buffers.contains_key(&field_a));
    assert!(buffers.contains_key(&field_b));

    let boundary_b = match tracker_b.observe_with_inserted_text(
        &field_b,
        &text_context(&field_b, "note "),
        TriggerPolicy::Automatic,
        3,
    ) {
        Observation::Typed(change) => change,
        Observation::CaretMoved { .. } => panic!("expected typed change"),
    };
    queue_and_flush_monitored_with_buffers(&boundary_b, &mut buffers, &store, &prefs, true, false);

    assert_eq!(
        store.recent("com.apple.TextEdit", 10).unwrap(),
        vec!["note "]
    );
    assert!(buffers.contains_key(&field_a));
    assert!(!buffers.contains_key(&field_b));
}

#[test]
fn monitored_write_failure_drains_boundary_without_replay() {
    let field = field_with_app("com.apple.TextEdit");
    let prefs = Prefs::default();
    let mut pending = Vec::new();
    let mut buffers = HashMap::new();
    let first = typed_change_after_baseline(&field, "", "first ");
    enqueue_monitored_change(
        &mut pending,
        &first,
        Some("com.apple.TextEdit".into()),
        None,
    );
    let mut attempts = Vec::new();

    flush_monitored_changes_with_monitor(
        &mut pending,
        &mut buffers,
        &prefs,
        monitored_policy(true, false, true, 1_001),
        |field, text| {
            attempts.push((field.app.clone(), text.to_string()));
            Err(memory::MemoryError::Db("forced failure".into()))
        },
    );

    assert_eq!(
        attempts,
        vec![("com.apple.TextEdit".into(), "first ".into())]
    );
    assert!(pending.is_empty());
    assert!(buffers.is_empty());

    let next = typed_change_after_baseline(&field, "first ", "first next ");
    enqueue_monitored_change(&mut pending, &next, Some("com.apple.TextEdit".into()), None);
    flush_monitored_changes_with_monitor(
        &mut pending,
        &mut buffers,
        &prefs,
        monitored_policy(true, false, true, 1_002),
        |field, text| {
            attempts.push((field.app.clone(), text.to_string()));
            Ok(())
        },
    );

    assert_eq!(
        attempts,
        vec![
            ("com.apple.TextEdit".into(), "first ".into()),
            ("com.apple.TextEdit".into(), "next ".into()),
        ]
    );
    assert!(pending.is_empty());
    assert!(buffers.is_empty());
}

#[test]
fn queued_monitored_typing_uses_policy_after_queueing() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([8u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
    let mut pending = Vec::new();
    enqueue_monitored_change(
        &mut pending,
        &change,
        Some("com.apple.TextEdit".into()),
        None,
    );

    let mut prefs = Prefs::default();
    prefs.snooze(1_000, 5);
    let mut buffers = HashMap::new();
    flush_monitored_changes(
        &mut pending,
        &mut buffers,
        Some(&store),
        &prefs,
        monitored_policy(true, false, true, 1_001),
    );
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn queued_monitored_typing_uses_field_app_when_app_key_is_absent() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([9u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.apple.TextEdit");
    let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
    let mut pending = Vec::new();
    enqueue_monitored_change(&mut pending, &change, None, None);

    let mut prefs = Prefs::default();
    prefs
        .per_app
        .entry("com.apple.TextEdit".into())
        .or_default()
        .collect_inputs = Some(false);
    let mut buffers = HashMap::new();
    flush_monitored_changes(
        &mut pending,
        &mut buffers,
        Some(&store),
        &prefs,
        monitored_policy(true, false, true, 1_001),
    );
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn secure_field_blocks_suggestion_offer() {
    // Privacy-critical: secure input (password field / global secure input)
    // must block the collection/suggestion gate BEFORE any other gate gets a
    // say — even when trusted + enabled + an allowed app + an unexcluded
    // domain would otherwise all pass. `!policy.secure` is the first
    // conjunct, so a single secure=true forces false. The edge-detector enum
    // is tested elsewhere; this pins the GATE behavior.
    let prefs = Prefs::default();
    // Everything else green: trusted, enabled, allowed app, no exclusions.
    assert!(
        monitored_collection_gates_pass(
            Some("com.apple.TextEdit"),
            None,
            &prefs,
            monitored_policy(true, false, true, 1_000),
            true,
        ),
        "baseline must pass so the only difference below is `secure`"
    );
    // Flip ONLY secure: the gate must now refuse, proving secure short-circuits
    // ahead of the other (still-green) gates.
    assert!(
        !monitored_collection_gates_pass(
            Some("com.apple.TextEdit"),
            None,
            &prefs,
            monitored_policy(true, true, true, 1_000),
            true,
        ),
        "secure input must block the suggestion/collection gate regardless of \
             every other gate being green"
    );
}

#[test]
fn excluded_apps_gate_blocks_suggestions() {
    // The per-app exclude path (App Settings / tray "Disable in this app"):
    // an excluded app must fail `suggestion_gates_pass` while a sibling app
    // still passes. Today only the domain-exclusion path has a gate test;
    // this pins the APP exclusion through the same shared gate.
    let mut prefs = Prefs::default();
    prefs.excluded_apps.insert("com.apple.Finder".into());
    assert!(
        !suggestion_gates_pass(Some("com.apple.Finder"), "hello there", None, &prefs, 0),
        "an excluded app must be blocked by the suggestion gate"
    );
    assert!(
        suggestion_gates_pass(Some("com.apple.TextEdit"), "hello there", None, &prefs, 0),
        "a non-excluded app must still pass"
    );
}

#[test]
fn tray_disabled_blocks_suggestions_regardless_of_prefs() {
    // The tray Enable toggle is a hard master switch: with `enabled=false`,
    // the suggestion gate stack must withhold offers even when prefs would
    // happily allow the app (default prefs: no exclusions, no snooze) AND a
    // real offer exists. We assert through BOTH seams the run loop uses:
    // - `suggestion_gates_pass` is prefs-only and STILL passes (proving the
    //   pref layer would allow it), so the block must come from `enabled`;
    // - `replacement_decision`, which carries the `enabled` flag, returns
    //   None once the tray is off and Some when it is on (same inputs).
    let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
    let allowed = Some("com.apple.TextEdit");
    assert!(
        suggestion_gates_pass(allowed, "hi :smile", None, &config.prefs, 0),
        "default prefs allow this app — so the block below is the tray switch, not prefs"
    );
    assert!(
        replacement_decision("hi :smile", &config, &config.prefs, allowed, None, true, 0).is_some(),
        "baseline: tray enabled + allowed app + a shortcode offers"
    );
    assert!(
        replacement_decision("hi :smile", &config, &config.prefs, allowed, None, false, 0)
            .is_none(),
        "the tray master switch (enabled=false) must block offers regardless of prefs"
    );
}

#[test]
fn monitored_collection_gates_match_suggestion_privacy_blocks() {
    let mut prefs = Prefs::default();
    assert!(!monitored_collection_gates_pass(
        Some("com.apple.TextEdit"),
        None,
        &prefs,
        monitored_policy(false, false, true, 1_000),
        true,
    ));
    assert!(!monitored_collection_gates_pass(
        Some("com.apple.TextEdit"),
        None,
        &prefs,
        monitored_policy(true, true, true, 1_000),
        true,
    ));
    assert!(!monitored_collection_gates_pass(
        Some("com.apple.TextEdit"),
        None,
        &prefs,
        monitored_policy(true, false, false, 1_000),
        true,
    ));

    prefs.excluded_apps.insert("com.apple.TextEdit".into());
    assert!(!monitored_collection_gates_pass(
        Some("com.apple.TextEdit"),
        None,
        &prefs,
        monitored_policy(true, false, true, 1_000),
        true,
    ));
    prefs.excluded_apps.clear();

    prefs.excluded_domains.insert("sensitive.example".into());
    assert!(!monitored_collection_gates_pass(
        Some("com.apple.Safari"),
        Some("docs.sensitive.example"),
        &prefs,
        monitored_policy(true, false, true, 1_000),
        true,
    ));
    assert!(!monitored_collection_gates_pass(
        Some("com.apple.Safari"),
        None,
        &prefs,
        monitored_policy(true, false, true, 1_000),
        true,
    ));
    assert!(monitored_collection_gates_pass(
        Some("com.apple.Safari"),
        Some("other.example"),
        &prefs,
        monitored_policy(true, false, true, 1_000),
        true,
    ));
    // Every gate open EXCEPT terminal_ok: a shell-history-style field in a
    // terminal must block monitored collection on its own, pinning the
    // `&& terminal_ok` conjunct. Same inputs as the passing case above but
    // with terminal_ok=false.
    assert!(!monitored_collection_gates_pass(
        Some("com.apple.Safari"),
        Some("other.example"),
        &prefs,
        monitored_policy(true, false, true, 1_000),
        false,
    ));
    prefs.excluded_domains.clear();

    prefs.snooze(1_000, 5);
    assert!(!monitored_collection_gates_pass(
        Some("com.apple.TextEdit"),
        None,
        &prefs,
        monitored_policy(true, false, true, 1_001),
        true,
    ));
}

#[test]
fn queued_monitored_typing_preserves_terminal_compatibility_without_prompt() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([10u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.googlecode.iterm2");
    let change = typed_change_after_baseline(&field, "", "git status && ls -la ");
    let mut pending = Vec::new();
    enqueue_monitored_change(
        &mut pending,
        &change,
        Some("com.googlecode.iterm2".into()),
        None,
    );
    assert!(!pending[0].terminal_ok);

    let prefs = Prefs::default();
    let mut buffers = HashMap::new();
    flush_monitored_changes(
        &mut pending,
        &mut buffers,
        Some(&store),
        &prefs,
        monitored_policy(true, false, true, 1_001),
    );
    assert_eq!(store.count().unwrap(), 0);

    let prompt = typed_change_after_baseline(&field, "", "please summarize the diff for ");
    let mut pending = Vec::new();
    enqueue_monitored_change(
        &mut pending,
        &prompt,
        Some("com.googlecode.iterm2".into()),
        None,
    );
    assert!(pending[0].terminal_ok);
    flush_monitored_changes(
        &mut pending,
        &mut buffers,
        Some(&store),
        &prefs,
        monitored_policy(true, false, true, 1_002),
    );
    assert_eq!(
        store.recent("com.googlecode.iterm2", 10).unwrap(),
        vec!["please summarize the diff for "]
    );
}

#[test]
fn queued_monitored_typing_uses_field_app_for_terminal_policy_when_app_key_missing() {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([29u8; 32]),
        memory::StorageMode::AllMonitored,
    )
    .expect("open in-memory store");
    let field = field_with_app("com.googlecode.iterm2");
    let change = typed_change_after_baseline(&field, "", "git status && ls -la ");
    let mut pending = Vec::new();
    enqueue_monitored_change(&mut pending, &change, None, None);
    assert_eq!(pending[0].app_key.as_deref(), Some("com.googlecode.iterm2"));
    assert!(
        !pending[0].terminal_ok,
        "terminal command text must not fail open when pid resolution misses"
    );

    flush_monitored_changes(
        &mut pending,
        &mut HashMap::new(),
        Some(&store),
        &Prefs::default(),
        monitored_policy(true, false, true, 1_000),
    );
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn accept_word_count_is_at_least_one() {
    assert_eq!(accept_word_count("the quick brown fox"), 4);
    assert_eq!(accept_word_count("solo"), 1);
    assert_eq!(accept_word_count("   "), 1); // whitespace-only still counts as one
    assert_eq!(accept_word_count(""), 1);
}

#[test]
fn mirror_mode_only_for_mirror_only_apps() {
    assert!(mirror_mode_for(Some("org.mozilla.firefox")));
    assert!(!mirror_mode_for(Some("com.apple.TextEdit")));
    assert!(!mirror_mode_for(None)); // unresolved app → inline
}

#[test]
fn stat_outcome_maps_engine_events() {
    assert_eq!(
        stat_outcome(engine::StatEvent::Shown),
        stats::Outcome::Shown
    );
    assert_eq!(
        stat_outcome(engine::StatEvent::Superseded),
        stats::Outcome::Superseded
    );
}

#[test]
fn canonicalize_field_app_replaces_volatile_pid_app_with_bundle_id() {
    let field = FieldHandle {
        app: "pid:42".into(),
        pid: Some(42),
        element_id: "ax:field".into(),
        generation: 7,
    };
    let (canonical, app_key) = canonicalize_field_app(field, |pid| {
        (pid == 42).then(|| "com.apple.TextEdit".into())
    });

    assert_eq!(app_key.as_deref(), Some("com.apple.TextEdit"));
    assert_eq!(canonical.app, "com.apple.TextEdit");
    assert_eq!(canonical.pid, Some(42));
    assert_eq!(canonical.element_id, "ax:field");
}

#[test]
fn canonicalize_field_app_returns_stable_fallback_key_on_resolver_miss() {
    let field = FieldHandle {
        app: "com.apple.TextEdit".into(),
        pid: Some(42),
        element_id: "ax:field".into(),
        generation: 7,
    };
    let (canonical, app_key) = canonicalize_field_app(field, |_| None);

    assert_eq!(app_key.as_deref(), Some("com.apple.TextEdit"));
    assert_eq!(canonical.app, "com.apple.TextEdit");
}

#[test]
fn previous_inputs_record_and_read_with_canonical_bundle_id() {
    let field = FieldHandle {
        app: "pid:42".into(),
        pid: Some(42),
        element_id: "ax:field".into(),
        generation: 7,
    };
    let (canonical, _) = canonicalize_field_app(field, |pid| {
        (pid == 42).then(|| "com.apple.TextEdit".into())
    });
    let previous_inputs = PreviousInputs::default();
    previous_inputs.record(&canonical.app, "accepted completion".into());
    let worker_context = WorkerContext {
        previous_inputs,
        max_chars: 200,
        ..WorkerContext::default()
    };
    let matching_request = engine::CompletionRequest {
        generation: 7,
        field: canonical.clone(),
        domain: None,
        snapshot: 7,
        prompt: "now".into(),
        max_tokens: 8,
        kind: RequestKind::Completion,
    };
    let volatile_request = engine::CompletionRequest {
        generation: 7,
        field: FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: "ax:field".into(),
            generation: 7,
        },
        domain: None,
        snapshot: 7,
        prompt: "now".into(),
        max_tokens: 8,
        kind: RequestKind::Completion,
    };

    assert!(worker_context
        .block_for(&matching_request)
        .contains("accepted completion"));
    assert!(!worker_context
        .block_for(&volatile_request)
        .contains("accepted completion"));
}

#[test]
fn empty_environment_uses_defaults() {
    let config = Config::from_lookup(lookup(&[]));
    assert_eq!(config.acceptance_pid, None);
    assert_eq!(config.stub_completion, None);
    assert_eq!(config.model_path, PathBuf::from(DEFAULT_MODEL));
    assert_eq!(config.prompt_mode, PromptMode::Terse);
    assert_eq!(config.run_ms, None);
    assert_eq!(config.debounce_ms, DEFAULT_DEBOUNCE_MS);
    assert_eq!(config.max_words, DEFAULT_MAX_WORDS);
    assert_eq!(config.max_tokens, DEFAULT_MAX_TOKENS);
    assert_eq!(config.heartbeat_ms, DEFAULT_HEARTBEAT_MS);
    assert_eq!(config.min_context_chars, DEFAULT_MIN_CONTEXT_CHARS);
    assert!(!config.allow_mid_word); // conservative default: mid-word suppressed
    assert!(!config.grammar_fix);
    assert!(!config.diag_coords);
}

#[test]
fn min_context_parses_and_clamps() {
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_MIN_CONTEXT", "5")])).min_context_chars,
        5
    );
    // over max → clamps to 100
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_MIN_CONTEXT", "999")])).min_context_chars,
        100
    );
    // unparseable → default
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_MIN_CONTEXT", "lots")])).min_context_chars,
        DEFAULT_MIN_CONTEXT_CHARS
    );
}

#[test]
fn midline_opt_in_by_one_or_true() {
    assert!(Config::from_lookup(lookup(&[("COMPME_MIDLINE", "1")])).allow_mid_word);
    assert!(Config::from_lookup(lookup(&[("COMPME_MIDLINE", "true")])).allow_mid_word);
    assert!(!Config::from_lookup(lookup(&[("COMPME_MIDLINE", "no")])).allow_mid_word);
}

#[test]
fn trailing_space_opt_in_by_one_or_true_and_off_by_default() {
    assert!(Config::from_lookup(lookup(&[("COMPME_TRAILING_SPACE", "1")])).trailing_space);
    assert!(Config::from_lookup(lookup(&[("COMPME_TRAILING_SPACE", "true")])).trailing_space);
    assert!(!Config::from_lookup(lookup(&[("COMPME_TRAILING_SPACE", "no")])).trailing_space);
    // Off by default when the key is absent (byte-identical accept behavior).
    assert!(!Config::from_lookup(lookup(&[])).trailing_space);
}

#[test]
fn emoji_config_off_by_default_and_parses_prefs_when_enabled() {
    // Absent / falsy → disabled (None).
    assert!(Config::from_lookup(lookup(&[])).emoji.is_none());
    assert!(Config::from_lookup(lookup(&[("COMPME_EMOJI", "no")]))
        .emoji
        .is_none());
    // Enabled → Some with default prefs.
    let on = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]))
        .emoji
        .expect("enabled");
    assert_eq!(on, EmojiPrefs::default());
    // Skin tone + gender parsed.
    let custom = Config::from_lookup(lookup(&[
        ("COMPME_EMOJI", "on"),
        ("COMPME_EMOJI_SKIN_TONE", "medium-dark"),
        ("COMPME_EMOJI_GENDER", "female"),
    ]))
    .emoji
    .expect("enabled");
    assert_eq!(custom.skin_tone, SkinTone::MediumDark);
    assert_eq!(custom.gender, Gender::Female);
}

#[test]
fn emoji_offer_gated_by_enable_and_shortcode() {
    let prefs = Some(EmojiPrefs::default());
    // Enabled + a trailing :shortcode → offers (glyph, chars-to-replace).
    let (glyph, replace_left) = emoji_offer("hi :smile", &prefs).expect("offer");
    assert!(!glyph.is_empty());
    assert_eq!(replace_left, 6); // ":smile"
                                 // Enabled but no shortcode → no offer.
    assert!(emoji_offer("hello world", &prefs).is_none());
    // Disabled (None) → never offers, even with a shortcode.
    assert!(emoji_offer("hi :smile", &None).is_none());
}

#[test]
fn trailing_word_extracts_the_word_at_the_caret() {
    assert_eq!(trailing_word("I teh"), Some("teh"));
    assert_eq!(trailing_word("color"), Some("color"));
    assert_eq!(trailing_word("café"), Some("café")); // multibyte
    assert_eq!(trailing_word("x:smile"), Some("smile")); // ':' is a boundary
    assert_eq!(trailing_word("done "), None); // trailing space = boundary
    assert_eq!(trailing_word("a1b"), Some("b")); // digit is a boundary
    assert_eq!(trailing_word(""), None);
}

#[test]
fn full_autocorrect_off_or_code_context_never_calls_the_os_checker() {
    let off = Config::from_lookup(lookup(&[]));
    let called = Cell::new(false);
    assert!(full_autocorrect_decision(
        "I teh",
        &off,
        &off.prefs,
        FullAutocorrectGate {
            app: SuggestionApp {
                app_key: Some("com.apple.TextEdit"),
                assistant_field: false,
            },
            domain: None,
            enabled: true,
            now_ms: 0,
        },
        |_| {
            called.set(true);
            Ok(Some("the".into()))
        },
    )
    .is_none());
    assert!(!called.get());

    let on = Config::from_lookup(lookup(&[("COMPME_FULL_AUTOCORRECT", "1")]));
    for (app_key, assistant_field, left) in [
        (Some("com.apple.dt.Xcode"), false, "I teh"),
        (Some("com.apple.Safari"), false, "I teh"),
        (Some("com.example.Writer"), false, "I teh"),
        (Some("com.apple.TextEdit"), false, "let value = teh"),
    ] {
        let called = Cell::new(false);
        assert!(full_autocorrect_decision(
            left,
            &on,
            &on.prefs,
            FullAutocorrectGate {
                app: SuggestionApp {
                    app_key,
                    assistant_field,
                },
                domain: None,
                enabled: true,
                now_ms: 0,
            },
            |_| {
                called.set(true);
                Ok(Some("the".into()))
            },
        )
        .is_none());
        assert!(!called.get());
    }
}

#[test]
fn full_autocorrect_offers_one_atomic_word_in_prose_and_assistant_fields() {
    let config = Config::from_lookup(lookup(&[("COMPME_FULL_AUTOCORRECT", "1")]));
    for app in [
        SuggestionApp {
            app_key: Some("com.apple.TextEdit"),
            assistant_field: false,
        },
        SuggestionApp {
            app_key: Some("com.microsoft.VSCode"),
            assistant_field: true,
        },
    ] {
        assert_eq!(
            full_autocorrect_decision(
                "I teh",
                &config,
                &config.prefs,
                FullAutocorrectGate {
                    app,
                    domain: None,
                    enabled: true,
                    now_ms: 0,
                },
                |word| {
                    assert_eq!(word, "teh");
                    Ok(Some("the".into()))
                },
            ),
            Some((vec!["the".into()], 3))
        );
    }
}

#[test]
fn full_autocorrect_rejects_identical_multiword_and_disabled_app_results() {
    let config = Config::from_lookup(lookup(&[("COMPME_FULL_AUTOCORRECT", "1")]));
    let app = SuggestionApp {
        app_key: Some("com.apple.TextEdit"),
        assistant_field: false,
    };
    for correction in ["teh", "two words", ""] {
        assert!(full_autocorrect_decision(
            "I teh",
            &config,
            &config.prefs,
            FullAutocorrectGate {
                app,
                domain: None,
                enabled: true,
                now_ms: 0,
            },
            |_| Ok(Some(correction.into())),
        )
        .is_none());
    }

    let prefs = build_prefs(&lookup(&[(
        "COMPME_AUTOCORRECT_OFF_APPS",
        "com.apple.TextEdit",
    )]));
    assert!(full_autocorrect_decision(
        "I teh",
        &config,
        &prefs,
        FullAutocorrectGate {
            app,
            domain: None,
            enabled: true,
            now_ms: 0,
        },
        |_| Ok(Some("the".into())),
    )
    .is_none());
}

#[test]
fn autocorrect_and_british_off_by_default() {
    let config = Config::from_lookup(lookup(&[]));
    assert!(!config.autocorrect);
    assert!(!config.full_autocorrect);
    assert!(!config.grammar_fix);
    assert!(!config.british_english);
    assert!(!config.thesaurus);
    assert!(!config.thesaurus_selection);
    // Off → no word-based offer even on a known typo / americanism.
    assert!(replacement_offer("teh", &config, config.autocorrect, config.thesaurus).is_none());
    assert!(replacement_offer("color", &config, config.autocorrect, config.thesaurus).is_none());
}

#[test]
fn replacement_offer_fires_for_enabled_word_features() {
    let ac = Config::from_lookup(lookup(&[("COMPME_AUTOCORRECT", "1")]));
    assert_eq!(
        replacement_offer("I teh", &ac, ac.autocorrect, ac.thesaurus),
        Some((vec!["the".into()], 3))
    );
    // A correctly-spelled word never offers.
    assert!(replacement_offer("the", &ac, ac.autocorrect, ac.thesaurus).is_none());

    let uk = Config::from_lookup(lookup(&[("COMPME_BRITISH_ENGLISH", "on")]));
    assert_eq!(
        replacement_offer("color", &uk, uk.autocorrect, uk.thesaurus),
        Some((vec!["colour".into()], 5))
    );
    assert!(replacement_offer("colour", &uk, uk.autocorrect, uk.thesaurus).is_none());
}

#[test]
fn replacement_offer_prioritizes_emoji_then_word_features() {
    // Emoji shortcode wins over the word-based features when all are enabled.
    let all = Config::from_lookup(lookup(&[
        ("COMPME_EMOJI", "1"),
        ("COMPME_AUTOCORRECT", "1"),
        ("COMPME_BRITISH_ENGLISH", "1"),
        ("COMPME_THESAURUS", "1"),
    ]));
    let (candidates, replace_left) =
        replacement_offer("teh :smile", &all, all.autocorrect, all.thesaurus).expect("emoji wins");
    assert_eq!(candidates[0], "😄"); // emoji wins
    assert_eq!(replace_left, 6); // ":smile", not the word "teh"
}

#[test]
fn replacement_offer_falls_through_autocorrect_to_localize() {
    // With BOTH word features on: a US spelling is not a typo, so it
    // must fall THROUGH autocorrect (None) to the UK fix; a typo takes
    // the autocorrect branch first.
    let both = Config::from_lookup(lookup(&[
        ("COMPME_AUTOCORRECT", "1"),
        ("COMPME_BRITISH_ENGLISH", "1"),
    ]));
    assert_eq!(
        replacement_offer("color", &both, both.autocorrect, both.thesaurus),
        Some((vec!["colour".into()], 5))
    );
    assert_eq!(
        replacement_offer("teh", &both, both.autocorrect, both.thesaurus),
        Some((vec!["the".into()], 3))
    );
}

#[test]
fn grammar_capitalizes_standalone_i_under_the_autocorrect_gate() {
    // "i" -> "I" is a grammar fix wired into replacement_offer behind the
    // autocorrect toggle (so it stays off in code fields like typo-fixing).
    let on = Config::from_lookup(lookup(&[("COMPME_AUTOCORRECT", "1")]));
    // A lone lowercase pronoun is offered as capital "I", replacing 1 char.
    assert_eq!(
        replacement_offer("i", &on, on.autocorrect, on.thesaurus),
        Some((vec!["I".into()], 1))
    );
    // Words that merely start with "i" are untouched (no false fix).
    assert_eq!(
        replacement_offer("in", &on, on.autocorrect, on.thesaurus),
        None
    );
    assert_eq!(
        replacement_offer("idea", &on, on.autocorrect, on.thesaurus),
        None
    );
    // Contraction limitation pinned: `trailing_word` tokenizes on the
    // apostrophe (it takes only alphabetic chars), so "i'm" reaches the
    // pipeline as "m" and no grammar fix fires — even though
    // grammar::capitalize_pronoun("i'm") itself returns "I'm". Capitalizing
    // contractions would need the caret-token model to include apostrophes.
    assert_eq!(
        replacement_offer("i'm", &on, on.autocorrect, on.thesaurus),
        None
    );
    // Gated off: autocorrect disabled -> no grammar fix either.
    let off = Config::from_lookup(lookup(&[("COMPME_AUTOCORRECT", "0")]));
    assert_eq!(
        replacement_offer("i", &off, off.autocorrect, off.thesaurus),
        None
    );
}

#[test]
fn thesaurus_offer_fires_for_enabled_feature() {
    let th = Config::from_lookup(lookup(&[("COMPME_THESAURUS", "1")]));
    let (syns, word_len) =
        replacement_offer("I am happy", &th, th.autocorrect, th.thesaurus).expect("offer");
    assert!(syns.contains(&"glad".to_string()));
    assert!(!syns.contains(&"happy".to_string()));
    assert_eq!(word_len, 5); // "happy"
}

#[test]
fn selection_thesaurus_offers_synonyms_for_one_selected_word() {
    let config = Config::from_lookup(lookup(&[("COMPME_THESAURUS_SELECTION", "1")]));
    let field = host_field("selection-thesaurus");
    let mut ctx = text_context_with_right(&field, "I am ", " today");
    ctx.selection = Some(platform::TextRange { start: 5, end: 10 });
    ctx.selected_text = Some("happy".into());
    let decision = selection_thesaurus_decision(
        &ctx,
        SelectionThesaurusGate {
            config: &config,
            prefs: &config.prefs,
            app: SuggestionApp {
                app_key: Some("com.apple.TextEdit"),
                assistant_field: false,
            },
            domain: None,
            enabled: true,
            caps: &writable_axset_caps(),
            now_ms: 0,
        },
    )
    .expect("selected word has synonyms");
    assert_eq!(decision.0, "happy");
    assert!(decision.1.contains(&"glad".to_string()));
    assert_eq!(decision.2, CorrectionRange { start: 5, end: 10 });
}

#[test]
fn selection_thesaurus_reaches_the_offer_after_a_selection_only_caret_event() {
    let config = Config::from_lookup(lookup(&[("COMPME_THESAURUS_SELECTION", "1")]));
    let field = host_field("selection-thesaurus-integration");
    let mut tracker = FieldTracker::new();
    tracker.observe(
        &field,
        &text_context_with_right(&field, "I am happy today", ""),
        TriggerPolicy::Automatic,
        0,
    );

    let mut selected = text_context_with_right(&field, "I am ", " today");
    selected.selection = Some(platform::TextRange { start: 5, end: 10 });
    selected.selected_text = Some("happy".into());
    assert_eq!(
        tracker.observe(&field, &selected, TriggerPolicy::Automatic, 1),
        Observation::CaretMoved {
            field: field.clone(),
            caret: 5,
        }
    );

    let decision = selection_thesaurus_decision(
        &selected,
        SelectionThesaurusGate {
            config: &config,
            prefs: &config.prefs,
            app: SuggestionApp {
                app_key: Some("com.apple.TextEdit"),
                assistant_field: false,
            },
            domain: None,
            enabled: true,
            caps: &writable_axset_caps(),
            now_ms: 1,
        },
    )
    .expect("selection-only caret event should offer synonyms");
    assert_eq!(decision.0, "happy");
    assert_eq!(decision.2, CorrectionRange { start: 5, end: 10 });
}

#[test]
fn first_selected_snapshot_can_offer_even_when_the_tracker_calls_it_typed() {
    let config = Config::from_lookup(lookup(&[("COMPME_THESAURUS_SELECTION", "1")]));
    let field = host_field("selection-thesaurus-first-snapshot");
    let mut selected = text_context_with_right(&field, "I am ", " today");
    selected.selection = Some(platform::TextRange { start: 5, end: 10 });
    selected.selected_text = Some("happy".into());

    let mut tracker = FieldTracker::new();
    assert!(matches!(
        tracker.observe(&field, &selected, TriggerPolicy::Automatic, 0),
        Observation::Typed(_)
    ));
    assert!(
        selection_thesaurus_decision(
            &selected,
            SelectionThesaurusGate {
                config: &config,
                prefs: &config.prefs,
                app: SuggestionApp {
                    app_key: Some("com.apple.TextEdit"),
                    assistant_field: false,
                },
                domain: None,
                enabled: true,
                caps: &writable_axset_caps(),
                now_ms: 0,
            },
        )
        .is_some(),
        "selection evaluation runs after either tracker observation kind"
    );
}

#[test]
fn selection_thesaurus_rejects_unknown_multiword_and_non_atomic_fields() {
    let config = Config::from_lookup(lookup(&[("COMPME_THESAURUS_SELECTION", "1")]));
    let field = host_field("selection-thesaurus-closed");
    let mut ctx = text_context_with_right(&field, "", "");
    ctx.selection = Some(platform::TextRange { start: 0, end: 9 });
    ctx.selected_text = Some("two words".into());
    let atomic = writable_axset_caps();
    assert!(selection_thesaurus_decision(
        &ctx,
        SelectionThesaurusGate {
            config: &config,
            prefs: &config.prefs,
            app: SuggestionApp {
                app_key: Some("com.apple.TextEdit"),
                assistant_field: false,
            },
            domain: None,
            enabled: true,
            caps: &atomic,
            now_ms: 0,
        },
    )
    .is_none());

    ctx.selected_text = Some("happy".into());
    let mut non_atomic = writable_axset_caps();
    non_atomic.insert_strategy = InsertStrategy::SyntheticKeys;
    assert!(selection_thesaurus_decision(
        &ctx,
        SelectionThesaurusGate {
            config: &config,
            prefs: &config.prefs,
            app: SuggestionApp {
                app_key: Some("com.apple.TextEdit"),
                assistant_field: false,
            },
            domain: None,
            enabled: true,
            caps: &non_atomic,
            now_ms: 0,
        },
    )
    .is_none());

    ctx.selection = None;
    ctx.selected_text = None;
    assert!(selection_thesaurus_decision(
        &ctx,
        SelectionThesaurusGate {
            config: &config,
            prefs: &config.prefs,
            app: SuggestionApp {
                app_key: Some("com.apple.TextEdit"),
                assistant_field: false,
            },
            domain: None,
            enabled: true,
            caps: &atomic,
            now_ms: 0,
        },
    )
    .is_none());
}

#[test]
fn replacement_decision_honors_snooze_and_auto_resumes() {
    // The fn's own contract: a local offer must not show while the model
    // is snoozed. Snooze lives in runtime-mutated prefs, passed
    // separately from config — this is the local path's own gate test.
    let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
    let mut prefs = Prefs::default();
    prefs.snooze(1_000, 60);
    let app = Some("com.apple.TextEdit");
    assert!(replacement_decision("hi :smile", &config, &prefs, app, None, true, 2_000).is_none());
    // 60 minutes later the snooze expired → offers again.
    let after = 1_000 + 60 * 60_000;
    assert!(replacement_decision("hi :smile", &config, &prefs, app, None, true, after).is_some());
}

#[test]
fn replacement_decision_uses_canonical_fallback_on_resolver_miss() {
    let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
    let mut prefs = Prefs::default();
    prefs.excluded_apps.insert("com.apple.TextEdit".into());
    let field = FieldHandle {
        app: "com.apple.TextEdit".into(),
        pid: Some(42),
        element_id: "ax:field".into(),
        generation: 7,
    };
    let (_, app_key) = canonicalize_field_app(field, |_| None);

    assert_eq!(app_key.as_deref(), Some("com.apple.TextEdit"));
    assert!(
        replacement_decision(
            "hi :smile",
            &config,
            &prefs,
            app_key.as_deref(),
            None,
            true,
            0
        )
        .is_none(),
        "local replacements must not fail open when pid resolution misses"
    );
}

#[test]
fn per_app_autocorrect_on_list_overrides_a_global_off() {
    // COMPME_AUTOCORRECT_ON_APPS: the positive override loop — a typo'd
    // key string in that parse would silently kill the feature.
    let prefs = build_prefs(&lookup(&[("COMPME_AUTOCORRECT_ON_APPS", "com.a.one")]));
    assert!(prefs.autocorrect_enabled(Some("com.a.one"), false));
    assert!(!prefs.autocorrect_enabled(Some("com.other"), false));
}

#[test]
fn grammar_fix_config_and_per_app_lists_parse() {
    let config = Config::from_lookup(lookup(&[
        ("COMPME_GRAMMAR_FIX", "on"),
        ("COMPME_GRAMMAR_ACCEPT_KEY", "ctrl+96"),
        ("COMPME_GRAMMAR_CHECK_KEY", "shift+96"),
    ]));
    assert!(config.grammar_fix);
    assert_eq!(
        config.grammar_accept_key,
        crate::shell::parse_accept_key("ctrl+96")
    );
    assert_eq!(config.grammar_check_key.as_deref(), Some("shift+96"));

    let prefs = build_prefs(&lookup(&[
        ("COMPME_GRAMMAR_FIX_ON_APPS", "com.a.one"),
        ("COMPME_GRAMMAR_FIX_OFF_APPS", "com.a.two"),
    ]));
    assert!(prefs.grammar_fix_enabled(Some("com.a.one"), false));
    assert!(!prefs.grammar_fix_enabled(Some("com.a.two"), true));
    assert!(!prefs.grammar_fix_enabled(Some("com.other"), false));
}

#[test]
fn trusted_key_parses_valid_hex_and_fails_closed_otherwise() {
    // COMPME_TRUSTED_KEY gates whether signed deep links can EVER apply;
    // the lookup→from_hex wiring is the app's security posture switch.
    // (from_hex validates a real Ed25519 point — this is the basepoint.)
    let valid = "5866666666666666666666666666666666666666666666666666666666666666";
    let with_key = Config::from_lookup(lookup(&[("COMPME_TRUSTED_KEY", valid)]));
    assert!(with_key.trusted_key.is_some());
    let junk = Config::from_lookup(lookup(&[("COMPME_TRUSTED_KEY", "not-hex")]));
    assert!(junk.trusted_key.is_none(), "malformed key fails closed");
    let absent = Config::from_lookup(lookup(&[]));
    assert!(
        absent.trusted_key.is_none(),
        "default: signed links rejected"
    );
}

#[test]
fn context_bound_lifts_zero_only_when_an_auxiliary_source_is_active() {
    // clipboard/screen context with max_chars == 0 would be a silent
    // no-op (the worker's block builder returns "" at bound 0).
    assert_eq!(
        context_bound_chars(true, false, 0),
        DEFAULT_CONTEXT_MAX_CHARS
    );
    assert_eq!(
        context_bound_chars(false, true, 0),
        DEFAULT_CONTEXT_MAX_CHARS
    );
    assert_eq!(
        context_bound_chars(false, false, 0),
        0,
        "nothing enabled stays off"
    );
    assert_eq!(
        context_bound_chars(true, true, 50),
        50,
        "explicit bound wins"
    );
}

#[test]
fn settings_context_bound_supports_late_clipboard_enable() {
    // Clipboard can now be enabled from Settings after launch; the inference
    // worker therefore needs a positive bound even when context env vars
    // were off at startup.
    assert_eq!(settings_context_bound_chars(0), DEFAULT_CONTEXT_MAX_CHARS);
    assert_eq!(settings_context_bound_chars(120), 120);
}

#[test]
fn parse_license_accepted_round_trips_and_normalizes() {
    // None → empty set; messy hand-edited values trim and drop empties;
    // serialize (via record_license_acceptance) is sorted + deduped, so
    // parse(serialize(parse(x))) == parse(x).
    assert!(parse_license_accepted(None).is_empty());
    let parsed = parse_license_accepted(Some(" b , ,a ".into()));
    assert_eq!(
        parsed.iter().cloned().collect::<Vec<_>>(),
        vec!["a".to_string(), "b".to_string()]
    );
    let mut set = parsed.clone();
    let serialized = record_license_acceptance(&mut set, "a"); // duplicate
    assert_eq!(serialized, "a,b", "sorted, deduped, unchanged by re-accept");
    assert_eq!(parse_license_accepted(Some(serialized)), parsed);
}

#[test]
fn record_license_acceptance_inserts_new_models() {
    let mut set = std::collections::BTreeSet::new();
    assert_eq!(
        record_license_acceptance(&mut set, "gemma-2-2b-q4_k_m"),
        "gemma-2-2b-q4_k_m"
    );
    assert_eq!(
        record_license_acceptance(&mut set, "llama-3.2-1b-q4_k_m"),
        "gemma-2-2b-q4_k_m,llama-3.2-1b-q4_k_m"
    );
    assert!(set.contains("gemma-2-2b-q4_k_m"));
}

#[test]
fn config_parses_license_accepted_from_lookup() {
    let config = Config::from_lookup(lookup(&[("COMPME_LICENSE_ACCEPTED", "x-model,y-model")]));
    assert!(config.license_accepted.contains("x-model"));
    assert!(config.license_accepted.contains("y-model"));
    assert!(Config::from_lookup(lookup(&[])).license_accepted.is_empty());
}

#[test]
fn catalog_download_request_threads_the_entry_hash_to_the_verifier() {
    // The consume edge previously hardcoded expected_sha256: None — a
    // pinned catalog hash would have been silently ignored. The request
    // builder must carry the entry's hash so verify-before-rename
    // engages the moment a hash lands in the catalog.
    let entry = model_catalog::ModelEntry {
        name: "test-model",
        url: "https://example.invalid/m.gguf",
        size_mb: 1,
        min_ram_gb: 1,
        license: model_catalog::License::Apache2,
        expected_sha256: Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
    };
    let status = std::sync::Arc::new(model_fetch::DownloadStatus::default());
    let request = catalog_download_request(
        &entry,
        PathBuf::from("/tmp/m.gguf"),
        std::sync::Arc::clone(&status),
    );
    assert_eq!(
        request.expected_sha256.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert_eq!(request.url, entry.url);
    assert_eq!(request.dest, PathBuf::from("/tmp/m.gguf"));
    assert_eq!(request.max_bytes, Some(1024 * 1024));
    // The SAME status block must ride along — a helper constructing a
    // fresh one would silently break progress polling.
    assert!(std::sync::Arc::ptr_eq(&request.status, &status));

    // Unpinned entry → no verification requested (downloader skips).
    let unpinned = model_catalog::ModelEntry {
        expected_sha256: None,
        ..entry
    };
    let status = std::sync::Arc::new(model_fetch::DownloadStatus::default());
    let request = catalog_download_request(&unpinned, PathBuf::from("/tmp/m.gguf"), status);
    assert_eq!(request.expected_sha256, None);
}

#[test]
fn download_idle_blocks_only_an_in_flight_download() {
    use model_fetch::{DownloadState, DownloadStatus};
    // The latent one-shot bug (found by the picker design audit): the
    // old `model_download_status.is_none()` guard never reset, so after
    // ONE download — even a Failed one — every later request was
    // silently swallowed for the process lifetime. Idle/Running block
    // (in flight); Done/Failed re-allow (retry and re-download work).
    assert!(download_idle(None), "no download yet");
    let status = DownloadStatus::default(); // state: Idle (queued)
    assert!(!download_idle(Some(&status)), "queued blocks");
    *status.state.lock().unwrap() = DownloadState::Running;
    assert!(!download_idle(Some(&status)), "running blocks");
    *status.state.lock().unwrap() = DownloadState::Done("/tmp/m.gguf".into());
    assert!(download_idle(Some(&status)), "done re-allows");
    *status.state.lock().unwrap() = DownloadState::Failed("boom".into());
    assert!(download_idle(Some(&status)), "failed re-allows retry");
}

#[test]
fn model_download_requeues_existing_file_when_hash_mismatches() {
    const EXPECTED_HASH: &str = "3aa927ba0345110f5880efe4a064beafcd9b37d4652c0293ca266654223ebf1f";
    let entry = model_catalog::ModelEntry {
        name: "test-model",
        url: "https://example.invalid/m.gguf",
        size_mb: 1,
        min_ram_gb: 1,
        license: model_catalog::License::Apache2,
        expected_sha256: Some(EXPECTED_HASH),
    };
    let dest = std::env::temp_dir().join(format!(
        "compme-existing-hash-mismatch-{}.gguf",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&dest);
    std::fs::write(&dest, b"wrong model bytes").unwrap();
    assert_eq!(
        model_download_dest_present(&dest, Some(EXPECTED_HASH)),
        Ok(false),
        "the helper must hash nonempty pinned files before trusting them"
    );
    let mut downloader = Some(());
    let mut status = None;
    let mut logged = 7;
    let requested_hash = std::cell::RefCell::new(None::<String>);

    let result = start_model_download_edge(ModelDownloadEdge {
        entry: &entry,
        dest: &dest,
        downloader: &mut downloader,
        model_download_status: &mut status,
        model_download_logged: &mut logged,
        prepare: |_: &std::path::Path| Ok(()),
        existing_model: model_download_dest_present,
        spawn: || Ok(()),
        request: |_: &(), request: model_fetch::DownloadRequest| {
            *requested_hash.borrow_mut() = request.expected_sha256;
            true
        },
    });
    let _ = std::fs::remove_file(&dest);

    assert_eq!(
        result,
        DownloadStartResult::Queued,
        "a nonempty file with the wrong hash must be re-downloaded"
    );
    assert_eq!(requested_hash.borrow().as_deref(), entry.expected_sha256);
    assert!(
        status.is_some(),
        "queued re-download must expose a fresh status block"
    );
    assert_eq!(logged, 0);
    std::fs::write(&dest, b"expected model bytes").unwrap();
    assert_eq!(
        model_download_dest_present(&dest, Some(EXPECTED_HASH)),
        Ok(true),
        "a matching pinned model may skip the download"
    );
    let _ = std::fs::remove_file(&dest);
}

#[test]
fn model_download_skips_existing_file_when_hash_matches() {
    const EXPECTED_HASH: &str = "de516b3d3641c9011fbf3cea3198c39f339fd92066b124279b69949640b171a5";
    let entry = model_catalog::ModelEntry {
        name: "test-model",
        url: "https://example.invalid/m.gguf",
        size_mb: 1,
        min_ram_gb: 1,
        license: model_catalog::License::Apache2,
        expected_sha256: Some(EXPECTED_HASH),
    };
    let dest = std::env::temp_dir().join(format!(
        "compme-existing-hash-match-{}.gguf",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&dest);
    std::fs::write(&dest, b"matching model bytes").unwrap();
    let mut downloader = Some(());
    let original_status = std::sync::Arc::new(model_fetch::DownloadStatus::default());
    *original_status.state.lock().unwrap() =
        model_fetch::DownloadState::Done("/tmp/previous.gguf".into());
    let original_ptr = std::sync::Arc::as_ptr(&original_status);
    let mut status = Some(original_status);
    let mut logged = 2;
    let requested = std::cell::Cell::new(false);

    let result = start_model_download_edge(ModelDownloadEdge {
        entry: &entry,
        dest: &dest,
        downloader: &mut downloader,
        model_download_status: &mut status,
        model_download_logged: &mut logged,
        prepare: |_: &std::path::Path| Ok(()),
        existing_model: model_download_dest_present,
        spawn: || Ok(()),
        request: |_: &(), _request: model_fetch::DownloadRequest| {
            requested.set(true);
            true
        },
    });
    let _ = std::fs::remove_file(&dest);

    assert_eq!(result, DownloadStartResult::AlreadyPresent);
    assert!(!requested.get(), "matching existing model skips enqueue");
    assert_eq!(
        status.as_ref().map(std::sync::Arc::as_ptr),
        Some(original_ptr),
        "skip path must not replace the tracked status block"
    );
    assert_eq!(logged, 2);
}

#[test]
fn model_download_busy_does_not_replace_tracked_status() {
    let entry = model_catalog::recommended().expect("catalog has a recommended model");
    let dest = std::path::PathBuf::from("/tmp/compme-model.gguf");
    let mut downloader = Some(());
    let original_status = std::sync::Arc::new(model_fetch::DownloadStatus::default());
    *original_status.state.lock().unwrap() =
        model_fetch::DownloadState::Failed("previous failure".into());
    let original_ptr = std::sync::Arc::as_ptr(&original_status);
    let mut status = Some(original_status);
    let mut logged = 2;

    let result = start_model_download_edge(ModelDownloadEdge {
        entry,
        dest: &dest,
        downloader: &mut downloader,
        model_download_status: &mut status,
        model_download_logged: &mut logged,
        prepare: |_: &std::path::Path| Ok(()),
        existing_model: |_: &std::path::Path, _: Option<&str>| Ok(false),
        spawn: || Ok(()),
        request: |_: &(), _request: model_fetch::DownloadRequest| false,
    });

    assert_eq!(result, DownloadStartResult::Busy);
    assert_eq!(
        status.as_ref().map(std::sync::Arc::as_ptr),
        Some(original_ptr),
        "dropped requests must not expose a fresh idle status"
    );
    assert_eq!(logged, 2);
}

#[test]
fn model_download_click_blocks_below_min_ram_before_prompt_or_enqueue() {
    let entry = model_catalog::recommended().expect("catalog has a recommended model");
    let mut accepted = std::collections::BTreeSet::new();
    let prompted = std::cell::Cell::new(false);

    let decision = model_download_click_decision(
        crate::model_picker::recommended_index(),
        entry.min_ram_gb.saturating_sub(1),
        &mut accepted,
        |_, _, _| {
            prompted.set(true);
            true
        },
    )
    .expect("catalog has an entry");

    match decision {
        ModelDownloadClickDecision::BlockedByRam(message) => {
            assert!(message.contains(entry.name));
        }
        other => panic!("expected RAM block, got {other:?}"),
    }
    assert!(
        !prompted.get(),
        "RAM block must happen before license prompt"
    );
    assert!(accepted.is_empty());
}

#[test]
fn model_download_click_declines_license_without_recording_acceptance() {
    let encumbered_index = model_catalog::catalog()
        .iter()
        .position(|entry| entry.license.needs_acceptance())
        .expect("catalog has an encumbered entry");
    let encumbered = &model_catalog::catalog()[encumbered_index];
    let mut accepted = std::collections::BTreeSet::new();

    let decision = model_download_click_decision(
        encumbered_index,
        encumbered.min_ram_gb,
        &mut accepted,
        |model, license_name, terms_url| {
            assert_eq!(model, encumbered.name);
            assert_eq!(license_name, encumbered.license.display_name());
            assert_eq!(terms_url, encumbered.license.terms_url());
            false
        },
    )
    .expect("catalog has an entry");

    assert_eq!(
        decision,
        ModelDownloadClickDecision::LicenseDeclined {
            model: encumbered.name
        }
    );
    assert!(
        accepted.is_empty(),
        "declining a license must not persist acceptance"
    );
}

#[test]
fn model_download_click_accepts_license_and_returns_persist_value() {
    let encumbered_index = model_catalog::catalog()
        .iter()
        .position(|entry| entry.license.needs_acceptance())
        .expect("catalog has an encumbered entry");
    let encumbered = &model_catalog::catalog()[encumbered_index];
    let mut accepted = std::collections::BTreeSet::new();

    let decision = model_download_click_decision(
        encumbered_index,
        encumbered.min_ram_gb,
        &mut accepted,
        |model, license_name, terms_url| {
            assert_eq!(model, encumbered.name);
            assert_eq!(license_name, encumbered.license.display_name());
            assert_eq!(terms_url, encumbered.license.terms_url());
            true
        },
    )
    .expect("catalog has an entry");

    match decision {
        ModelDownloadClickDecision::Ready {
            entry,
            accepted_license: Some(accepted_license),
        } => {
            assert_eq!(entry.name, encumbered.name);
            assert_eq!(accepted_license.model, encumbered.name);
            assert_eq!(
                accepted_license.license_name,
                encumbered.license.display_name()
            );
            assert_eq!(accepted_license.value, encumbered.name);
        }
        other => panic!("expected accepted license ready decision, got {other:?}"),
    }
    assert!(accepted.contains(encumbered.name));
}

#[test]
fn model_download_click_skips_prompt_for_already_accepted_license() {
    let encumbered_index = model_catalog::catalog()
        .iter()
        .position(|entry| entry.license.needs_acceptance())
        .expect("catalog has an encumbered entry");
    let encumbered = &model_catalog::catalog()[encumbered_index];
    // Seed the accepted set so the download gate proceeds without prompting.
    let mut accepted = std::collections::BTreeSet::new();
    accepted.insert(encumbered.name.to_string());

    let decision = model_download_click_decision(
        encumbered_index,
        encumbered.min_ram_gb,
        &mut accepted,
        |_, _, _| panic!("already-accepted license must not re-prompt"),
    )
    .expect("catalog has an entry");

    match decision {
        ModelDownloadClickDecision::Ready {
            entry,
            accepted_license: None,
        } => assert_eq!(entry.name, encumbered.name),
        other => panic!("expected ready decision without new acceptance, got {other:?}"),
    }
    // Re-download of an already-licensed model leaves the set unchanged.
    assert!(accepted.contains(encumbered.name));
}

#[test]
fn model_download_click_uses_selected_index_and_oob_falls_back_to_recommended() {
    let selected_index = model_catalog::catalog()
        .iter()
        .position(|entry| {
            entry.license == model_catalog::License::Apache2
                && Some(entry.name) != model_catalog::recommended().map(|e| e.name)
        })
        .expect("catalog has a non-default unencumbered entry");
    let selected = &model_catalog::catalog()[selected_index];
    let mut accepted = std::collections::BTreeSet::new();

    let selected_decision = model_download_click_decision(
        selected_index,
        selected.min_ram_gb,
        &mut accepted,
        |_, _, _| panic!("unencumbered selected model must not prompt"),
    )
    .expect("catalog has an entry");
    match selected_decision {
        ModelDownloadClickDecision::Ready {
            entry,
            accepted_license: None,
        } => assert_eq!(entry.name, selected.name),
        other => panic!("expected selected entry to be ready, got {other:?}"),
    }

    let recommended = model_catalog::recommended().expect("catalog has a recommended model");
    let fallback_decision = model_download_click_decision(
        usize::MAX,
        recommended.min_ram_gb,
        &mut accepted,
        |_, _, _| panic!("recommended model must not prompt"),
    )
    .expect("fallback catalog entry");
    match fallback_decision {
        ModelDownloadClickDecision::Ready {
            entry,
            accepted_license: None,
        } => assert_eq!(entry.name, recommended.name),
        other => panic!("expected OOB fallback to recommended, got {other:?}"),
    }
}

#[test]
fn model_download_status_line_surfaces_progress_and_outcome_but_stays_quiet_when_ready() {
    use model_fetch::{DownloadState, DownloadStatus};
    use std::sync::atomic::Ordering;

    let status = std::sync::Arc::new(DownloadStatus::default());
    let set = |state: DownloadState, done: u64, total: u64| {
        *status.state.lock().unwrap() = state;
        status.downloaded.store(done, Ordering::Relaxed);
        status.total.store(total, Ordering::Relaxed);
    };

    // Model already loaded → never nag, whatever the download state says.
    set(DownloadState::Failed("ignored".into()), 0, 0);
    assert_eq!(model_download_status_line(Some(&status), true), None);
    // No download and idle → nothing to show.
    assert_eq!(model_download_status_line(None, false), None);
    set(DownloadState::Idle, 0, 0);
    assert_eq!(model_download_status_line(Some(&status), false), None);

    // Running: percent when total known, byte count when unknown (0 total).
    set(
        DownloadState::Running,
        512 * 1024 * 1024,
        1024 * 1024 * 1024,
    );
    assert!(model_download_status_line(Some(&status), false)
        .unwrap()
        .contains("50%"));
    set(DownloadState::Running, 3 * 1024 * 1024, 0);
    assert!(model_download_status_line(Some(&status), false)
        .unwrap()
        .contains("3 MB"));

    // Terminal states are the whole point — the user must see them.
    set(DownloadState::Done("/tmp/m.gguf".into()), 0, 0);
    assert!(model_download_status_line(Some(&status), false)
        .unwrap()
        .contains("relaunch"));
    set(DownloadState::Failed("http error: status 404".into()), 0, 0);
    assert!(model_download_status_line(Some(&status), false)
        .unwrap()
        .contains("404"));
}

#[test]
fn download_log_transitions_log_each_stage_exactly_once() {
    use model_fetch::DownloadState;
    // Running logs once, never repeats.
    let (logged, line) = download_log_transition(&DownloadState::Running, 0);
    assert_eq!(logged, 1);
    assert!(line.unwrap().contains("running"));
    assert_eq!(
        download_log_transition(&DownloadState::Running, 1),
        (1, None)
    );
    // Done logs where the model landed — the only user-visible signal —
    // even when it skipped Running.
    let done = DownloadState::Done("/tmp/m.gguf".into());
    let (logged, line) = download_log_transition(&done, 0);
    assert_eq!(logged, 2);
    assert!(line.unwrap().contains("/tmp/m.gguf"));
    assert_eq!(
        download_log_transition(&done, 2),
        (2, None),
        "terminal logs once"
    );
    // Failed is terminal too.
    let failed = DownloadState::Failed("boom".into());
    let (logged, line) = download_log_transition(&failed, 1);
    assert_eq!(logged, 2);
    assert!(line.unwrap().contains("boom"));
    assert_eq!(download_log_transition(&failed, 2), (2, None));
    // Idle never logs.
    assert_eq!(download_log_transition(&DownloadState::Idle, 0), (0, None));
}

#[test]
fn download_log_transition_emits_path_hint_on_running_to_done() {
    // The normal sequence is Running (logged 0->1) then Done (logged 1->2).
    // The Done guard is `logged < 2`, so reaching Done with logged == 1 must
    // still emit the destination path — the only signal of where the model
    // landed. A mutant narrowing the guard to `logged == 0` would drop the
    // line on this real path, leaving the user with no destination.
    let done = model_fetch::DownloadState::Done("/tmp/m.gguf".into());
    let (logged, line) = download_log_transition(&done, 1);
    assert_eq!(logged, 2);
    assert!(line.unwrap().contains("/tmp/m.gguf"));
}

#[test]
fn model_download_prepare_failure_does_not_spawn_or_enqueue() {
    let entry = model_catalog::recommended().expect("catalog has a recommended model");
    let dest = std::path::PathBuf::from("/tmp/compme-model.gguf");
    let mut downloader: Option<()> = None;
    let mut status = Some(std::sync::Arc::new(model_fetch::DownloadStatus::default()));
    let previous_status = status.as_ref().map(std::sync::Arc::as_ptr).unwrap();
    let mut logged = 7;
    let metadata_checked = std::cell::Cell::new(false);
    let spawned = std::cell::Cell::new(false);
    let requested = std::cell::Cell::new(false);

    let result = start_model_download_edge(ModelDownloadEdge {
        entry,
        dest: &dest,
        downloader: &mut downloader,
        model_download_status: &mut status,
        model_download_logged: &mut logged,
        prepare: |_: &std::path::Path| Err("no model directory".into()),
        existing_model: |_: &std::path::Path, _: Option<&str>| {
            metadata_checked.set(true);
            Ok(false)
        },
        spawn: || {
            spawned.set(true);
            Ok(())
        },
        request: |_: &(), _| {
            requested.set(true);
            true
        },
    });

    assert_eq!(
        result,
        DownloadStartResult::PreparedFailed("no model directory".into())
    );
    assert!(
        !metadata_checked.get(),
        "metadata must not run after prep fails"
    );
    assert!(!spawned.get(), "downloader must not spawn after prep fails");
    assert!(
        !requested.get(),
        "request must not enqueue after prep fails"
    );
    assert_eq!(
        status.as_ref().map(std::sync::Arc::as_ptr),
        Some(previous_status)
    );
    assert_eq!(logged, 7);
}

#[test]
fn model_download_spawn_failure_does_not_enqueue_or_mark_running() {
    let entry = model_catalog::recommended().expect("catalog has a recommended model");
    let dest = std::path::PathBuf::from("/tmp/compme-model.gguf");
    let mut downloader: Option<()> = None;
    let mut status = None;
    let mut logged = 7;
    let requested = std::cell::Cell::new(false);

    let result = start_model_download_edge(ModelDownloadEdge {
        entry,
        dest: &dest,
        downloader: &mut downloader,
        model_download_status: &mut status,
        model_download_logged: &mut logged,
        prepare: |_: &std::path::Path| Ok(()),
        existing_model: |_: &std::path::Path, _: Option<&str>| Ok(false),
        spawn: || Err("thread unavailable".into()),
        request: |_: &(), _| {
            requested.set(true);
            true
        },
    });

    assert_eq!(
        result,
        DownloadStartResult::SpawnFailed("thread unavailable".into())
    );
    assert!(
        !requested.get(),
        "request must not enqueue without a downloader"
    );
    assert!(status.is_none(), "failed spawn must not set running status");
    assert_eq!(logged, 7);
}

#[test]
fn replacement_decision_combines_gate_and_offer() {
    let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
    let allowed = Some("com.apple.TextEdit");
    // Enabled (tray) + allowed app + a shortcode → offers.
    assert!(
        replacement_decision("hi :smile", &config, &config.prefs, allowed, None, true, 0).is_some()
    );
    // Tray-disabled → no offer even with a match.
    assert!(
        replacement_decision("hi :smile", &config, &config.prefs, allowed, None, false, 0)
            .is_none()
    );
    // Sidebar-only / blocked app → no offer even when enabled.
    assert!(replacement_decision(
        "hi :smile",
        &config,
        &config.prefs,
        Some("com.microsoft.VSCode"),
        None,
        true,
        0
    )
    .is_none());
    // No matching token → no offer.
    assert!(replacement_decision(
        "hello world",
        &config,
        &config.prefs,
        allowed,
        None,
        true,
        0
    )
    .is_none());
}

#[test]
fn numeric_knobs_parse_and_clamp() {
    let config = Config::from_lookup(lookup(&[
        ("COMPME_DEBOUNCE_MS", "60"),
        ("COMPME_MAX_WORDS", "999"),    // over max → clamps to 50
        ("COMPME_MAX_TOKENS", "0"),     // under min → clamps to 1
        ("COMPME_HEARTBEAT_MS", "500"), // over max → clamps to 100
    ]));
    assert_eq!(config.debounce_ms, 60);
    assert_eq!(config.max_words, 50);
    assert_eq!(config.max_tokens, 1);
    assert_eq!(config.heartbeat_ms, 100);
}

#[test]
fn numeric_knobs_fall_back_to_defaults_when_unparseable() {
    let config = Config::from_lookup(lookup(&[
        ("COMPME_DEBOUNCE_MS", "fast"),
        ("COMPME_MAX_WORDS", "many"),
        ("COMPME_MAX_TOKENS", "lots"),
        ("COMPME_HEARTBEAT_MS", "soon"),
    ]));
    assert_eq!(config.debounce_ms, DEFAULT_DEBOUNCE_MS);
    assert_eq!(config.max_words, DEFAULT_MAX_WORDS);
    assert_eq!(config.max_tokens, DEFAULT_MAX_TOKENS);
    assert_eq!(config.heartbeat_ms, DEFAULT_HEARTBEAT_MS);
}

#[test]
fn candidate_count_parses_and_clamps() {
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_CANDIDATES", "3")])).candidates,
        3
    );
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_CANDIDATES", "0")])).candidates,
        1
    );
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_CANDIDATES", "99")])).candidates,
        5
    );
    assert_eq!(
        Config::from_lookup(lookup(&[("COMPME_CANDIDATES", "many")])).candidates,
        DEFAULT_CANDIDATES
    );
}

#[test]
fn diag_coords_enabled_by_one_or_true() {
    assert!(Config::from_lookup(lookup(&[("COMPME_DIAG_COORDS", "1")])).diag_coords);
    assert!(Config::from_lookup(lookup(&[("COMPME_DIAG_COORDS", "true")])).diag_coords);
    assert!(!Config::from_lookup(lookup(&[("COMPME_DIAG_COORDS", "no")])).diag_coords);
}

#[test]
fn valid_pid_and_run_ms_parse() {
    let config = Config::from_lookup(lookup(&[
        ("COMPME_ACCEPTANCE_PID", "8273"),
        ("COMPME_RUN_MS", "4000"),
    ]));
    assert_eq!(config.acceptance_pid, Some(8273));
    assert_eq!(config.run_ms, Some(4000));
}

#[test]
fn unparseable_pid_and_run_ms_fall_back_to_none() {
    let config = Config::from_lookup(lookup(&[
        ("COMPME_ACCEPTANCE_PID", "not-a-number"),
        ("COMPME_RUN_MS", "later"),
    ]));
    assert_eq!(config.acceptance_pid, None);
    assert_eq!(config.run_ms, None);
}

#[test]
fn empty_stub_completion_is_treated_as_unset() {
    let config = Config::from_lookup(lookup(&[("COMPME_STUB_COMPLETION", "")]));
    assert_eq!(config.stub_completion, None);
}

#[test]
fn non_empty_stub_completion_is_kept() {
    let config = Config::from_lookup(lookup(&[("COMPME_STUB_COMPLETION", " jumps")]));
    assert_eq!(config.stub_completion.as_deref(), Some(" jumps"));
}

#[test]
fn model_path_override_wins_over_default() {
    let config = Config::from_lookup(lookup(&[("COMPME_MODEL_PATH", "/models/x.gguf")]));
    assert_eq!(config.model_path, PathBuf::from("/models/x.gguf"));
}

#[test]
fn prompt_mode_raw_is_parsed() {
    let config = Config::from_lookup(lookup(&[("COMPME_PROMPT_MODE", "raw")]));
    assert_eq!(config.prompt_mode, PromptMode::Raw);
}

#[test]
fn only_unavailable_statuses_drop_pending_requests() {
    assert!(!status_drops_pending_requests(AppStatus::Loading));
    assert!(!status_drops_pending_requests(AppStatus::Ready));
    assert!(status_drops_pending_requests(AppStatus::Disabled));
    assert!(status_drops_pending_requests(AppStatus::Blocked(
        BlockReason::Permission
    )));
    assert!(status_drops_pending_requests(AppStatus::Blocked(
        BlockReason::AccessibilityUnavailable
    )));
    assert!(status_drops_pending_requests(AppStatus::Blocked(
        BlockReason::SecureInput
    )));
    assert!(status_drops_pending_requests(AppStatus::Blocked(
        BlockReason::ModelUnavailable
    )));
}

#[test]
fn failed_inference_worker_surfaces_model_unavailable_instead_of_loading() {
    assert_eq!(
        derive_status(
            true,
            AccessibilitySubscriptions::Ready,
            false,
            effective_model_available(true, true),
            false,
            true,
        ),
        AppStatus::Blocked(BlockReason::ModelUnavailable)
    );
}

#[test]
fn manual_grammar_request_drops_under_loading_unlike_pending_completions() {
    // The one-shot GrammarCheck shortcut arms `manual_grammar_request`,
    // consumed only inside the `suggestions_allowed()` arm; every other
    // status takes the drop-with-log `else if` so the key press is never
    // silently discarded. The `suggestions_allowed` truth table alone is
    // owned by status.rs (`only_ready_allows_suggestions`) — asserting it
    // again here would be a duplicate. The behavior *this* branch encodes,
    // pinned nowhere else, is the DIVERGENCE at Loading: a manual grammar
    // request is dropped there (suggestions not allowed) even though a
    // pending *completion* request is preserved (`status_drops_pending_
    // requests` is false for Loading). If the two predicates were ever
    // realigned at Loading, the grammar drop branch would silently change
    // meaning; this guards that coupling.
    // Ready: grammar submitted, pending completions kept.
    assert!(AppStatus::Ready.suggestions_allowed());
    assert!(!status_drops_pending_requests(AppStatus::Ready));
    // Loading: grammar request dropped, pending completion preserved.
    assert!(!AppStatus::Loading.suggestions_allowed());
    assert!(!status_drops_pending_requests(AppStatus::Loading));
    // Hard-blocked / disabled: grammar request and pending completions both go.
    for status in [
        AppStatus::Disabled,
        AppStatus::Blocked(BlockReason::Permission),
        AppStatus::Blocked(BlockReason::AccessibilityUnavailable),
        AppStatus::Blocked(BlockReason::RelaunchRequired),
        AppStatus::Blocked(BlockReason::SecureInput),
        AppStatus::Blocked(BlockReason::ModelUnavailable),
    ] {
        assert!(!status.suggestions_allowed(), "{status:?}");
        assert!(status_drops_pending_requests(status), "{status:?}");
    }
}

#[test]
fn subscription_error_degrades_only_for_missing_accessibility_or_untrusted_startup() {
    assert_eq!(
        subscription_error_action(
            false,
            &PlatformError::CannotComplete {
                reason: "AX down".into()
            }
        ),
        SubscriptionErrorAction::NoopUntilPermission
    );
    assert_eq!(
        subscription_error_action(
            true,
            &PlatformError::PermissionMissing {
                permission: "Accessibility".into()
            }
        ),
        SubscriptionErrorAction::NoopUntilPermission
    );
    // Fatal must carry the underlying error context — run() interpolates
    // this message verbatim into the operator-facing startup failure, so a
    // blank/constant payload would silently strip the diagnostic.
    match subscription_error_action(
        true,
        &PlatformError::CannotComplete {
            reason: "AX down".into(),
        },
    ) {
        SubscriptionErrorAction::Fatal(m) => {
            assert!(m.contains("CannotComplete") && m.contains("AX down"), "{m}")
        }
        other => panic!("expected Fatal, got {other:?}"),
    }
    match subscription_error_action(true, &PlatformError::Timeout) {
        SubscriptionErrorAction::Fatal(m) => assert!(m.contains("Timeout"), "{m}"),
        other => panic!("expected Fatal, got {other:?}"),
    }
}

#[test]
fn unavailable_accessibility_service_degrades_without_masquerading_as_permission() {
    assert_eq!(
        subscription_error_action(
            true,
            &PlatformError::AccessibilityUnavailable {
                reason: "AT-SPI accessibility session unavailable (no bus)".into(),
            },
        ),
        SubscriptionErrorAction::Unavailable(
            "AT-SPI accessibility session unavailable (no bus)".into(),
        ),
    );

    match subscription_error_action(
        true,
        &PlatformError::UnsupportedField {
            reason: "subscription contract bug".into(),
        },
    ) {
        SubscriptionErrorAction::Fatal(message) => {
            assert!(message.contains("UnsupportedField"), "{message}");
        }
        other => panic!("trusted unsupported subscription must stay fatal, got {other:?}"),
    }
}

#[test]
fn degraded_startup_subscriptions_keep_runtime_permission_blocked() {
    assert!(runtime_trusted(true, AccessibilitySubscriptions::Ready));
    assert!(!runtime_trusted(false, AccessibilitySubscriptions::Ready));
    assert!(!runtime_trusted(
        true,
        AccessibilitySubscriptions::RelaunchRequired
    ));
    assert!(!runtime_trusted(
        false,
        AccessibilitySubscriptions::Unavailable
    ));
}

#[test]
fn secure_input_subscription_error_is_fatal_when_trusted() {
    // A SecureInput error at subscription time is NOT the missing-permission
    // degrade path: when the app is already trusted it is a fatal startup
    // condition, and the Fatal payload must carry the variant so run()'s
    // operator-facing message names what failed. (Untrusted still degrades
    // to NoopUntilPermission, like every non-permission error.)
    let secure = PlatformError::SecureInput {
        state: SecurityState::SecureInputEnabled,
    };
    match subscription_error_action(true, &secure) {
        SubscriptionErrorAction::Fatal(m) => assert!(m.contains("SecureInput"), "{m}"),
        other => panic!("expected Fatal, got {other:?}"),
    }
    assert_eq!(
        subscription_error_action(false, &secure),
        SubscriptionErrorAction::NoopUntilPermission
    );
}

#[test]
fn screen_recording_requested_only_when_context_on_and_permission_missing() {
    assert!(should_request_screen_recording(true, false));
    assert!(!should_request_screen_recording(true, true));
    assert!(!should_request_screen_recording(false, false));
    assert!(!should_request_screen_recording(false, true));
}

#[test]
fn instance_lock_io_failure_fails_closed_before_startup_side_effects() {
    let side_effects = std::cell::Cell::new(0);
    let result: Result<Option<()>, String> = instance_lock_startup_gate(
        Some(std::path::PathBuf::from("/tmp/compme.lock")),
        |_| Err(config::InstanceLockError::Io("permission denied".into())),
        || side_effects.set(side_effects.get() + 1),
    );
    assert!(matches!(
        result,
        Err(message) if message.contains("permission denied")
    ));
    assert_eq!(
        side_effects.get(),
        0,
        "startup side effects must not run after lock IO failure"
    );
    assert!(matches!(
        instance_startup_decision(Some(config::InstanceLockError::Io("permission denied".into()))),
        InstanceStartupDecision::Fail(message) if message.contains("permission denied")
    ));
}

#[test]
fn missing_instance_lock_path_fails_closed_before_startup_side_effects() {
    let side_effects = std::cell::Cell::new(0);
    let result: Result<Option<()>, String> = instance_lock_startup_gate(
        None::<std::path::PathBuf>,
        |_| Ok(()),
        || side_effects.set(side_effects.get() + 1),
    );
    assert!(matches!(
        result,
        Err(message) if message.contains("instance lock")
    ));
    assert_eq!(
        side_effects.get(),
        0,
        "startup side effects must not run without an instance lock path"
    );
    assert!(matches!(
        instance_startup_decision(None),
        InstanceStartupDecision::Fail(message) if message.contains("instance lock")
    ));
}

#[test]
fn acquiring_the_instance_lock_runs_startup_side_effects_once_and_proceeds() {
    // The proceed path: a clean acquire returns Ok(Some(lock)) AND runs the
    // startup side effects exactly once (installing AX observers etc.). A
    // regression that swallowed a successful acquire, or ran the side effects
    // zero/twice, would slip past the fail-closed tests alone.
    let side_effects = std::cell::Cell::new(0);
    let result = instance_lock_startup_gate(
        Some(std::path::PathBuf::from("/tmp/compme.lock")),
        |_| Ok("held-lock"),
        || side_effects.set(side_effects.get() + 1),
    );
    assert!(matches!(result, Ok(Some("held-lock"))));
    assert_eq!(
        side_effects.get(),
        1,
        "startup side effects must run exactly once after a clean acquire"
    );
}

#[test]
fn a_duplicate_instance_exits_gracefully_without_startup_side_effects() {
    // The graceful-duplicate path: `Held` maps to ExitOk, so the gate returns
    // Ok(None) (caller exits 0, not an error) and the startup side effects
    // never run — a second launch must not install observers or touch state.
    let side_effects = std::cell::Cell::new(0);
    let result: Result<Option<()>, String> = instance_lock_startup_gate(
        Some(std::path::PathBuf::from("/tmp/compme.lock")),
        |_| Err(config::InstanceLockError::Held),
        || side_effects.set(side_effects.get() + 1),
    );
    assert!(matches!(result, Ok(None)));
    assert_eq!(
        side_effects.get(),
        0,
        "a duplicate instance must not run startup side effects"
    );
    assert!(matches!(
        instance_startup_decision(Some(config::InstanceLockError::Held)),
        InstanceStartupDecision::ExitOk(message) if message.contains("already running")
    ));
}

#[test]
fn secure_input_caps_are_non_interactive_and_secure() {
    let caps = secure_input_caps();
    assert!(!caps.readable_text);
    assert!(!caps.readable_caret);
    assert!(!caps.writable);
    assert!(caps.secure);
    assert_eq!(caps.security_state, SecurityState::SecureInputEnabled);
    assert_eq!(caps.insert_strategy, InsertStrategy::None);
    assert_eq!(caps.accept_intercept, KeyInterceptMode::None);
    assert_eq!(caps.overlay_at_caret, OverlayPlacement::None);
}

fn host_field(id: &str) -> FieldHandle {
    FieldHandle {
        app: "TextEdit".into(),
        pid: Some(7),
        element_id: id.into(),
        generation: 1,
    }
}

fn rect(x: f64) -> Option<ScreenRect> {
    Some(ScreenRect {
        x,
        y: 0.0,
        w: 1.0,
        h: 14.0,
    })
}

fn req(generation: u64) -> CompletionRequest {
    CompletionRequest {
        generation,
        field: host_field("f"),
        domain: None,
        snapshot: generation,
        prompt: "p".into(),
        max_tokens: 8,
        kind: RequestKind::Completion,
    }
}

fn req_with_prompt(prompt: &str) -> CompletionRequest {
    CompletionRequest {
        prompt: prompt.into(),
        ..req(1)
    }
}

fn grammar_req_with_left_ctx(left_ctx: &str) -> CompletionRequest {
    CompletionRequest {
        prompt: String::new(),
        kind: RequestKind::GrammarFix {
            word: "teh".into(),
            left_ctx: left_ctx.into(),
            correction_range: CorrectionRange { start: 0, end: 3 },
        },
        ..req(1)
    }
}

#[test]
fn log_err_passes_through_ok_requests() {
    let out = log_err("x", Ok(vec![req(1), req(2)]));
    assert_eq!(out.len(), 2);
}

#[test]
fn log_err_swallows_errors_into_empty_vec() {
    // The "one failed effect never kills the loop" guarantee: an Err becomes
    // an empty request list (logged), not a propagated failure.
    let out = log_err("x", Err(PlatformError::Timeout));
    assert!(out.is_empty());
}

#[test]
fn offer_all_keeps_newest_request() {
    let mut latest = LatestRequest::new();
    offer_all(&mut latest, vec![req(1), req(3), req(2)]);
    // Newest-by-generation wins regardless of arrival order…
    assert_eq!(latest.take().unwrap().generation, 3);

    // …and offering an OLDER generation afterward must NOT re-populate the
    // slot (the `>=` guard in LatestRequest::offer): a late stale request
    // can never resurrect a request the loop already moved past.
    offer_all(&mut latest, vec![req(3)]);
    offer_all(&mut latest, vec![req(1)]);
    assert_eq!(
        latest.take().unwrap().generation,
        3,
        "an older generation must not overwrite the retained newest"
    );
}

#[test]
fn grammar_check_shortcut_blocked_after_read_clears_pending_completion() {
    let mut latest = LatestRequest::new();
    latest.offer(req(9));
    let mut manual = Some(req(1));

    apply_grammar_shortcut_pending_effect(
        &mut latest,
        &mut manual,
        &GrammarCheckShortcutOutcome::BlockedAfterRead,
    );

    assert!(latest.take().is_none());
    assert!(manual.is_none());
}

#[test]
fn grammar_check_shortcut_not_armed_clears_pending_completion() {
    let mut latest = LatestRequest::new();
    latest.offer(req(9));
    let mut manual = None;

    apply_grammar_shortcut_pending_effect(
        &mut latest,
        &mut manual,
        &GrammarCheckShortcutOutcome::NotArmed,
    );

    assert!(latest.take().is_none());
    assert!(manual.is_none());
}

#[test]
fn grammar_check_shortcut_later_failed_press_drops_stale_manual_request() {
    let mut latest = LatestRequest::new();
    let mut manual = None;

    apply_grammar_shortcut_pending_effect(
        &mut latest,
        &mut manual,
        &GrammarCheckShortcutOutcome::Armed(req(4)),
    );
    apply_grammar_shortcut_pending_effect(
        &mut latest,
        &mut manual,
        &GrammarCheckShortcutOutcome::BlockedAfterRead,
    );

    assert!(latest.take().is_none());
    assert!(manual.is_none());

    apply_grammar_shortcut_pending_effect(
        &mut latest,
        &mut manual,
        &GrammarCheckShortcutOutcome::Armed(req(5)),
    );
    apply_grammar_shortcut_pending_effect(
        &mut latest,
        &mut manual,
        &GrammarCheckShortcutOutcome::NotArmed,
    );

    assert!(latest.take().is_none());
    assert!(manual.is_none());
}

#[test]
fn grammar_check_shortcut_non_armed_error_drops_stale_manual_without_completion_clear() {
    let mut latest = LatestRequest::new();
    latest.offer(req(9));
    let mut manual = Some(req(4));

    apply_grammar_shortcut_pending_effect(
        &mut latest,
        &mut manual,
        &GrammarCheckShortcutOutcome::ReadContextError(PlatformError::Timeout),
    );

    assert_eq!(latest.take().unwrap().generation, 9);
    assert!(manual.is_none());
}

#[test]
fn secure_edge_detects_enter() {
    assert_eq!(secure_edge(false, true, true), SecureEdge::Enter);
    assert_eq!(secure_edge(false, true, false), SecureEdge::Enter);
}

#[test]
fn secure_edge_clears_only_when_trusted() {
    assert_eq!(secure_edge(true, false, true), SecureEdge::ClearRehydrate);
    // Cleared but Accessibility not (yet) trusted → stay blocked, no rehydrate.
    assert_eq!(secure_edge(true, false, false), SecureEdge::None);
}

#[test]
fn secure_edge_none_when_unchanged() {
    assert_eq!(secure_edge(false, false, true), SecureEdge::None);
    assert_eq!(secure_edge(true, true, true), SecureEdge::None);
}

#[test]
fn dismiss_only_on_enabled_to_disabled_edge() {
    assert!(should_dismiss_on_disable(true, false));
    assert!(!should_dismiss_on_disable(false, false)); // already disabled
    assert!(!should_dismiss_on_disable(false, true)); // re-enabling
    assert!(!should_dismiss_on_disable(true, true)); // still enabled
}

#[test]
fn toggle_app_dismisses_only_when_app_was_enabled() {
    // ToggleApp disables exactly when the app was enabled pre-toggle, so the
    // dismiss seam fires on that branch only. Both arms pinned so inverting
    // the production guard is caught (the per-app retraction has no global
    // edge for should_dismiss_on_disable to cover).
    assert!(toggle_app_dismisses(true)); // was enabled -> toggle disables -> dismiss
    assert!(!toggle_app_dismisses(false)); // was disabled -> toggle enables -> keep
}

#[test]
fn app_enabled_baseline_reads_override_then_default() {
    // The value ToggleApp inverts: per-app `enabled` override wins; absent an
    // override it falls back to `default_enabled` (NOT should_suggest, so
    // snooze/exclude don't enter here).
    let mut prefs = Prefs {
        default_enabled: true,
        ..Default::default()
    };
    // No override -> default.
    assert!(app_enabled_baseline(&prefs, "com.none"));
    prefs.default_enabled = false;
    assert!(!app_enabled_baseline(&prefs, "com.none"));
    // Override beats default in both directions.
    prefs.per_app.entry("com.on".into()).or_default().enabled = Some(true);
    assert!(app_enabled_baseline(&prefs, "com.on")); // override true vs default false
    prefs.default_enabled = true;
    prefs.per_app.entry("com.off".into()).or_default().enabled = Some(false);
    assert!(!app_enabled_baseline(&prefs, "com.off")); // override false vs default true
}

#[test]
fn toggle_app_inverts_override_and_converges_across_baselines() {
    // review finding E + r1/r2: ToggleApp inverts the PER-APP enabled override
    // (driven by app_enabled_baseline), writing the inverse as an explicit
    // override. It must CONVERGE — one toggle flips the live state, a second
    // restores it — from every starting baseline (None/default true, None/
    // default false, Some(true), Some(false)). The toggle dispatch reads the
    // baseline then writes `set_app_policy_field(Enabled, !baseline)`; this
    // mirrors that core through the same two public helpers.
    let app = "com.toggle.app";
    let one_toggle = |prefs: &mut Prefs| {
        let next = !app_enabled_baseline(prefs, app);
        prefs.set_app_policy_field(app, prefs::AppPolicyField::Enabled, next);
        next
    };
    for (default_enabled, seed) in [
        (true, None),
        (false, None),
        (true, Some(true)),
        (false, Some(false)),
        (true, Some(false)),
        (false, Some(true)),
    ] {
        let mut prefs = Prefs {
            default_enabled,
            ..Default::default()
        };
        if let Some(v) = seed {
            prefs.per_app.entry(app.into()).or_default().enabled = Some(v);
        }
        let start = app_enabled_baseline(&prefs, app);
        // One toggle flips the effective enabled state and pins an override.
        let after_first = one_toggle(&mut prefs);
        assert_eq!(
            after_first, !start,
            "first toggle must flip (seed {seed:?})"
        );
        assert_eq!(prefs.per_app[app].enabled, Some(!start));
        assert_eq!(app_enabled_baseline(&prefs, app), !start);
        // Second toggle converges back — no drift, regardless of baseline.
        let after_second = one_toggle(&mut prefs);
        assert_eq!(
            after_second, start,
            "second toggle must converge (seed {seed:?})"
        );
        assert_eq!(app_enabled_baseline(&prefs, app), start);
    }
}

#[test]
fn apps_edit_dismisses_only_focused_app_on_enable_off_edge() {
    use prefs::AppPolicyField::*;
    // Gap 3: editing the FOCUSED app's Enabled->off dismisses; editing a
    // DIFFERENT app's row does not; and only the Enabled->off edge fires.
    // Focused app == "com.a".
    assert!(apps_edit_dismisses_focused(
        Enabled,
        false,
        Some("com.a"),
        "com.a"
    ));
    // Different app edited while focused on com.a -> no dismiss.
    assert!(!apps_edit_dismisses_focused(
        Enabled,
        false,
        Some("com.a"),
        "com.b"
    ));
    // Enabling (on=true) the focused app does not dismiss.
    assert!(!apps_edit_dismisses_focused(
        Enabled,
        true,
        Some("com.a"),
        "com.a"
    ));
    // Disabling GrammarFix for the focused app also dismisses, because an
    // already visible correction would otherwise remain acceptable.
    assert!(apps_edit_dismisses_focused(
        GrammarFix,
        false,
        Some("com.a"),
        "com.a"
    ));
    // Feature-off edges that can stale an existing visible suggestion also dismiss.
    assert!(!apps_edit_dismisses_focused(
        TabDisabled,
        false,
        Some("com.a"),
        "com.a"
    ));
    // Enabling Tab suppression for the focused app must also retract the
    // visible suggestion, otherwise the already armed bare-Tab binding can
    // still accept it until the next focus/show cycle.
    assert!(apps_edit_dismisses_focused(
        TabDisabled,
        true,
        Some("com.a"),
        "com.a"
    ));
    assert!(apps_edit_dismisses_focused(
        MidLine,
        false,
        Some("com.a"),
        "com.a"
    ));
    assert!(apps_edit_dismisses_focused(
        Autocorrect,
        false,
        Some("com.a"),
        "com.a"
    ));
    // No focused app at all -> nothing to dismiss.
    assert!(!apps_edit_dismisses_focused(Enabled, false, None, "com.a"));
}

#[test]
fn toggle_app_dismisses_iff_focused_app_was_enabled_before_toggle() {
    // The ToggleApp shortcut flips a PER-APP override and must retract an
    // on-screen ghost ONLY when the toggle DISABLES the focused app. Unlike
    // ToggleGlobal/SIGUSR1, it never touches the global `enabled` atomic, so
    // the tick reconciliation (should_dismiss_on_disable over the global
    // edge) can NOT cover it — the production arm's
    // `if toggle_app_dismisses(current) { latest.clear(); on_dismiss() }`
    // seam is the only retraction, with `current =
    // app_enabled_baseline(&prefs, app)` read BEFORE the override write.
    // Round 1's convergence test pinned the override write but never this
    // dismiss guard; inverting the seam (leave a ghost on disable, dismiss
    // on enable) passes every round-1 test. This drives the dispatch core
    // through the same three production helpers.
    let app = "com.toggle.dismiss";
    let toggle_decides_dismiss = |prefs: &mut Prefs| -> bool {
        let current = app_enabled_baseline(prefs, app);
        prefs.set_app_policy_field(app, prefs::AppPolicyField::Enabled, !current);
        toggle_app_dismisses(current) // the run loop's dismiss guard
    };
    for (default_enabled, seed) in [
        (true, None),
        (false, None),
        (true, Some(true)),
        (false, Some(false)),
        (true, Some(false)),
        (false, Some(true)),
    ] {
        let mut prefs = Prefs {
            default_enabled,
            ..Default::default()
        };
        if let Some(v) = seed {
            prefs.per_app.entry(app.into()).or_default().enabled = Some(v);
        }
        let was_enabled = app_enabled_baseline(&prefs, app);
        // Dismiss fires iff the app was enabled before (the toggle disables
        // it); when it was already disabled, the toggle re-enables and there
        // is nothing on screen to retract.
        assert_eq!(
            toggle_decides_dismiss(&mut prefs),
            was_enabled,
            "dismiss decision must equal pre-toggle enabled (seed {seed:?})"
        );
        // And the toggle still flipped the live state (guards against a
        // mutation that returns the right dismiss bool but skips the write).
        assert_eq!(app_enabled_baseline(&prefs, app), !was_enabled);
    }
}

#[test]
fn coalesce_empty_drain_is_empty() {
    assert_eq!(coalesce_caret_reads(vec![]), vec![]);
}

#[test]
fn coalesce_keeps_a_lone_caret() {
    let events = vec![HostEvent::Caret(host_field("a"), rect(1.0))];
    assert_eq!(coalesce_caret_reads(events.clone()), events);
}

#[test]
fn coalesce_collapses_adjacent_same_field_carets_to_the_last() {
    let events = vec![
        HostEvent::Caret(host_field("a"), rect(1.0)),
        HostEvent::Caret(host_field("a"), rect(2.0)),
        HostEvent::Caret(host_field("a"), rect(3.0)),
    ];
    // Only the newest read survives, carrying the latest rect.
    assert_eq!(
        coalesce_caret_reads(events),
        vec![HostEvent::Caret(host_field("a"), rect(3.0))]
    );
}

#[test]
fn coalesce_keeps_carets_for_different_fields() {
    let events = vec![
        HostEvent::Caret(host_field("a"), rect(1.0)),
        HostEvent::Caret(host_field("b"), rect(2.0)),
    ];
    assert_eq!(coalesce_caret_reads(events.clone()), events);
}

#[test]
fn coalesce_does_not_cross_a_focus_event() {
    // Focus changes engine state, so the caret before it must still be read.
    let events = vec![
        HostEvent::Caret(host_field("a"), rect(1.0)),
        HostEvent::Focus(host_field("a")),
        HostEvent::Caret(host_field("a"), rect(2.0)),
    ];
    assert_eq!(coalesce_caret_reads(events.clone()), events);
}

#[test]
fn coalesce_does_not_cross_an_accept_event() {
    let events = vec![
        HostEvent::Caret(host_field("a"), rect(1.0)),
        HostEvent::Accept(AcceptAction::Full),
        HostEvent::Caret(host_field("a"), rect(2.0)),
    ];
    assert_eq!(coalesce_caret_reads(events.clone()), events);
}

#[test]
fn coalesce_passes_non_caret_events_through() {
    let events = vec![
        HostEvent::Focus(host_field("a")),
        HostEvent::Accept(AcceptAction::Word),
        HostEvent::Shortcut(ShortcutAction::GrammarCheck),
        HostEvent::Accept(AcceptAction::Correction),
    ];
    assert_eq!(coalesce_caret_reads(events.clone()), events);
}

#[test]
fn focus_caret_accept_and_dismiss_clear_pending_requests() {
    assert!(host_event_invalidates_pending_request(&HostEvent::Focus(
        host_field("a")
    )));
    assert!(host_event_invalidates_pending_request(&HostEvent::Caret(
        host_field("a"),
        None
    )));
    assert!(host_event_invalidates_pending_request(&HostEvent::Accept(
        AcceptAction::Full
    )));
    assert!(host_event_invalidates_pending_request(&HostEvent::Accept(
        AcceptAction::Correction
    )));
    assert!(host_event_invalidates_pending_request(&HostEvent::Dismiss));
    assert!(!host_event_invalidates_pending_request(&HostEvent::Cycle));
    assert!(!host_event_invalidates_pending_request(
        &HostEvent::Shortcut(ShortcutAction::GrammarCheck)
    ));
}

#[test]
fn host_event_queue_drops_old_caret_to_preserve_control_event() {
    let mut queue = VecDeque::new();
    for i in 0..MAX_HOST_EVENT_QUEUE {
        assert!(enqueue_host_event(
            &mut queue,
            HostEvent::Caret(host_field(&format!("field-{i}")), rect(i as f64))
        ));
    }

    assert!(enqueue_host_event(
        &mut queue,
        HostEvent::Accept(AcceptAction::Full)
    ));

    assert_eq!(queue.len(), MAX_HOST_EVENT_QUEUE);
    assert!(queue
        .iter()
        .any(|event| matches!(event, HostEvent::Accept(AcceptAction::Full))));
    assert!(!queue
        .iter()
        .any(|event| matches!(event, HostEvent::Caret(field, _) if field.element_id == "field-0")));
}

#[test]
fn host_event_queue_drops_old_focus_to_preserve_control_event() {
    // Focus events are backpressure-droppable too (a superseded focus is as
    // stale as a superseded caret). A full queue of Focus events must yield
    // the oldest one to admit a control event, not refuse it.
    let mut queue = VecDeque::new();
    for i in 0..MAX_HOST_EVENT_QUEUE {
        assert!(enqueue_host_event(
            &mut queue,
            HostEvent::Focus(host_field(&format!("field-{i}")))
        ));
    }

    assert!(enqueue_host_event(
        &mut queue,
        HostEvent::Accept(AcceptAction::Full)
    ));

    assert_eq!(queue.len(), MAX_HOST_EVENT_QUEUE);
    assert!(queue
        .iter()
        .any(|event| matches!(event, HostEvent::Accept(AcceptAction::Full))));
    assert!(!queue
        .iter()
        .any(|event| matches!(event, HostEvent::Focus(field) if field.element_id == "field-0")));
}

#[test]
fn host_event_queue_never_orphans_caret_when_dropping_focus() {
    let mut queue = VecDeque::new();
    assert!(enqueue_host_event(
        &mut queue,
        HostEvent::Focus(host_field("dependent"))
    ));
    assert!(enqueue_host_event(
        &mut queue,
        HostEvent::Caret(host_field("dependent"), rect(1.0))
    ));
    for i in 2..MAX_HOST_EVENT_QUEUE {
        assert!(enqueue_host_event(
            &mut queue,
            HostEvent::Caret(host_field(&format!("field-{i}")), rect(i as f64))
        ));
    }

    assert!(enqueue_host_event(
        &mut queue,
        HostEvent::Accept(AcceptAction::Full)
    ));

    let focus = queue.iter().position(
        |event| matches!(event, HostEvent::Focus(field) if field.element_id == "dependent"),
    );
    let caret = queue.iter().position(
        |event| matches!(event, HostEvent::Caret(field, _) if field.element_id == "dependent"),
    );
    assert!(
        caret.is_none() || focus.is_some_and(|focus| focus < caret.unwrap()),
        "a retained caret must keep its preceding focus boundary"
    );
}

#[test]
fn host_event_queue_keeps_carets_that_follow_a_refocus_when_dropping_focus() {
    // Dropping the oldest Focus(A) must prune only the carets that depended on
    // it — those queued before A's next Focus. A later re-Focus(A) starts a new
    // dependency chain, and its carets must survive intact; the old `retain`
    // dropped every Caret(A) in the queue, orphaning the second chain's focus.
    let mut queue = VecDeque::new();
    assert!(enqueue_host_event(
        &mut queue,
        HostEvent::Focus(host_field("a"))
    ));
    assert!(enqueue_host_event(
        &mut queue,
        HostEvent::Caret(host_field("a"), rect(1.0))
    ));
    assert!(enqueue_host_event(
        &mut queue,
        HostEvent::Focus(host_field("b"))
    ));
    assert!(enqueue_host_event(
        &mut queue,
        HostEvent::Focus(host_field("a"))
    ));
    assert!(enqueue_host_event(
        &mut queue,
        HostEvent::Caret(host_field("a"), rect(2.0))
    ));
    for i in 5..MAX_HOST_EVENT_QUEUE {
        assert!(enqueue_host_event(
            &mut queue,
            HostEvent::Caret(host_field(&format!("field-{i}")), rect(i as f64))
        ));
    }

    assert!(enqueue_host_event(
        &mut queue,
        HostEvent::Accept(AcceptAction::Full)
    ));

    let a_events: Vec<&HostEvent> = queue
        .iter()
        .filter(|event| match event {
            HostEvent::Focus(field) | HostEvent::Caret(field, _) => field.element_id == "a",
            _ => false,
        })
        .collect();
    // The first Focus(a) and its Caret(a) at rect 1.0 are gone; the re-focus
    // and its Caret(a) at rect 2.0 remain, in order.
    assert_eq!(a_events.len(), 2, "{a_events:?}");
    assert!(matches!(a_events[0], HostEvent::Focus(_)));
    assert!(matches!(a_events[1], HostEvent::Caret(_, r) if *r == rect(2.0)));
    assert!(queue
        .iter()
        .any(|event| matches!(event, HostEvent::Focus(field) if field.element_id == "b")));
}

#[test]
fn host_event_queue_refuses_when_only_control_events_remain() {
    let mut queue = VecDeque::new();
    for _ in 0..MAX_HOST_EVENT_QUEUE {
        assert!(enqueue_host_event(
            &mut queue,
            HostEvent::Accept(AcceptAction::Full)
        ));
    }

    assert!(!enqueue_host_event(&mut queue, HostEvent::Dismiss));
    assert_eq!(queue.len(), MAX_HOST_EVENT_QUEUE);
}

#[test]
fn host_event_drain_reports_backlog() {
    let queue = Mutex::new(VecDeque::new());
    for i in 0..(MAX_HOST_EVENTS_PER_TICK + 1) {
        assert!(push_host_event(
            &queue,
            HostEvent::Caret(host_field(&format!("field-{i}")), rect(i as f64))
        ));
    }

    let drained = drain_host_events(&queue);

    assert_eq!(drained.events.len(), MAX_HOST_EVENTS_PER_TICK);
    assert!(drained.backlog_remaining);
}

#[test]
fn grammar_check_shortcut_routes_to_detection() {
    assert_eq!(
        host_event_route(&HostEvent::Shortcut(ShortcutAction::GrammarCheck)),
        HostEventRoute::ManualGrammarDetection
    );
    assert_eq!(
        host_event_route(&HostEvent::Shortcut(ShortcutAction::ForceActivate)),
        HostEventRoute::Normal
    );
}

#[test]
fn grammar_accept_action_routes_to_accept_correction_not_full() {
    assert_eq!(
        host_event_route(&HostEvent::Accept(AcceptAction::Correction)),
        HostEventRoute::AcceptCorrection
    );
    assert_eq!(
        host_event_route(&HostEvent::Accept(AcceptAction::Full)),
        HostEventRoute::Normal
    );
    assert_eq!(
        host_event_route(&HostEvent::Accept(AcceptAction::Word)),
        HostEventRoute::Normal
    );
}

#[test]
fn coalesce_collapses_only_within_runs() {
    // a,a -> last a ; then b ; then a,a -> last a. Two runs collapse
    // independently around the intervening different-field caret.
    let events = vec![
        HostEvent::Caret(host_field("a"), rect(1.0)),
        HostEvent::Caret(host_field("a"), rect(2.0)),
        HostEvent::Caret(host_field("b"), rect(3.0)),
        HostEvent::Caret(host_field("a"), rect(4.0)),
        HostEvent::Caret(host_field("a"), rect(5.0)),
    ];
    assert_eq!(
        coalesce_caret_reads(events),
        vec![
            HostEvent::Caret(host_field("a"), rect(2.0)),
            HostEvent::Caret(host_field("b"), rect(3.0)),
            HostEvent::Caret(host_field("a"), rect(5.0)),
        ]
    );
}

// — run() startup seam: recording fakes + the ordering/failure contracts. —

/// Ordered record of every factory/seam call `startup()` makes, so the
/// startup sequence (the c92 wiring contract) is pinned by observation
/// rather than by re-reading the code. Blind window: work that bypasses
/// the factories and recorded seams — deep-link install, launch-at-login,
/// memory open, clipboard/screen cells, screen-OCR, and the inference
/// spawn between "subscribe-accept" and "tray" — is not observed here.
type StartupLog = Arc<Mutex<Vec<&'static str>>>;

fn startup_log() -> StartupLog {
    Arc::new(Mutex::new(Vec::new()))
}

fn log_push(log: &StartupLog, step: &'static str) {
    log.lock().unwrap().push(step);
}

fn log_steps(log: &StartupLog) -> Vec<&'static str> {
    log.lock().unwrap().clone()
}

/// Shell double with a settable Accessibility grant; the two permission
/// probes are recorded so the permission step's place in the sequence is
/// observable.
struct RecordingShell {
    trusted: bool,
    log: StartupLog,
}

impl ShellHost for RecordingShell {
    fn pump_events(&self, _heartbeat: Duration) {}
    fn accessibility_trusted(&self) -> bool {
        log_push(&self.log, "permissions");
        self.trusted
    }
    fn prompt_accessibility_trust(&self) -> bool {
        log_push(&self.log, "permission-prompt");
        self.trusted
    }
    fn physical_memory_bytes(&self) -> u64 {
        0
    }
    fn open_url(&self, _url: &str) -> Result<(), PlatformError> {
        Ok(())
    }
    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        Ok(())
    }
    fn reveal_file(&self, _path: &Path) -> Result<(), PlatformError> {
        Ok(())
    }
    fn set_launch_at_login(&self, _enabled: bool) -> Result<(), PlatformError> {
        Ok(())
    }
    fn confirm(&self, _prompt: &shell_flags::ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
        Ok(false)
    }
    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
        Ok([0; 32])
    }
}

/// Adapter double: the three subscriptions are recorded and can be
/// programmed to fail; every other method is inert (`Err(StaleField)`).
struct FakeAdapter {
    log: StartupLog,
    focus_error: Option<PlatformError>,
    caret_error: Option<PlatformError>,
    accept_error: Option<PlatformError>,
}

impl FakeAdapter {
    fn allow_all(log: StartupLog) -> Self {
        Self {
            log,
            focus_error: None,
            caret_error: None,
            accept_error: None,
        }
    }

    fn failing(log: StartupLog, err: PlatformError) -> Self {
        Self {
            log,
            focus_error: Some(err.clone()),
            caret_error: Some(err.clone()),
            accept_error: Some(err),
        }
    }
}

impl PlatformAdapter for FakeAdapter {
    fn environment(&self) -> platform::Environment {
        platform::Environment {
            os: platform::OperatingSystem::Macos,
            version: "test".into(),
        }
    }
    fn subscribe_focus(&self, _cb: platform::FocusCallback) -> Result<Subscription, PlatformError> {
        log_push(&self.log, "subscribe-focus");
        match &self.focus_error {
            Some(err) => Err(err.clone()),
            None => Ok(Subscription::new(1)),
        }
    }
    fn subscribe_caret(&self, _cb: platform::CaretCallback) -> Result<Subscription, PlatformError> {
        log_push(&self.log, "subscribe-caret");
        match &self.caret_error {
            Some(err) => Err(err.clone()),
            None => Ok(Subscription::new(2)),
        }
    }
    fn subscribe_accept(
        &self,
        _cb: platform::AcceptCallback,
    ) -> Result<AcceptSubscription, PlatformError> {
        log_push(&self.log, "subscribe-accept");
        match &self.accept_error {
            Some(err) => Err(err.clone()),
            None => Ok(AcceptSubscription::new(
                Subscription::new(3),
                |_| Ok(()),
                |_| Ok(()),
                |_| Ok(()),
            )),
        }
    }
    fn front_app(&self) -> Option<platform::AppId> {
        None
    }
    fn capabilities(&self, _field: &FieldHandle) -> Result<Capabilities, PlatformError> {
        Err(PlatformError::StaleField)
    }
    fn read_context(&self, _field: &FieldHandle) -> Result<TextContext, PlatformError> {
        Err(PlatformError::StaleField)
    }
    fn caret_rect(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        Err(PlatformError::StaleField)
    }
    fn text_range_rect(
        &self,
        _field: &FieldHandle,
        _range: CorrectionRange,
    ) -> Result<Option<ScreenRect>, PlatformError> {
        Err(PlatformError::StaleField)
    }
    fn insert(
        &self,
        _field: &FieldHandle,
        _text: &str,
        _strategy: InsertStrategy,
    ) -> Result<platform::Inserted, PlatformError> {
        Err(PlatformError::StaleField)
    }
    fn insert_replacing(
        &self,
        _field: &FieldHandle,
        _text: &str,
        _replace_left: usize,
        _strategy: InsertStrategy,
    ) -> Result<platform::Inserted, PlatformError> {
        Err(PlatformError::StaleField)
    }
    fn insert_replacing_range(
        &self,
        _field: &FieldHandle,
        _expected_text: &str,
        _text: &str,
        _range: CorrectionRange,
        _strategy: InsertStrategy,
    ) -> Result<platform::Inserted, PlatformError> {
        Err(PlatformError::StaleField)
    }
}

/// Inert overlay double — the engine is constructed but never driven by
/// these tests, so its methods only need to exist.
struct FakeOverlay;

impl OverlayPresenter for FakeOverlay {
    fn show_ghost(&mut self, _rect: ScreenRect, _text: &str) -> Result<(), PlatformError> {
        Ok(())
    }
    fn update_ghost(&mut self, _text: &str) -> Result<(), PlatformError> {
        Ok(())
    }
    fn hide(&mut self) -> Result<(), PlatformError> {
        Ok(())
    }
}

/// In-memory startup config: stub completion (the model "loads" without
/// file I/O, so startup reaches inference spawn and never touches the
/// app-support model-adoption path); every other knob at its default.
fn startup_test_config() -> Config {
    Config::from_lookup(|key| {
        if key == "COMPME_STUB_COMPLETION" {
            Some(" test".to_string())
        } else {
            None
        }
    })
}

/// Hermetic temp dir for the instance lock, per the file's temp-dir
/// convention; callers `remove_dir_all` at the end.
fn startup_test_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("compme-startup-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Recording factories for a hermetic `startup()`: a REAL instance lock on
/// a temp path (so the flock gate runs for real), the given config/shell/
/// adapter/overlay behavior, no-op signal handlers, and the widened stub
/// shell's (unsupported) tray. One-shot values ride in slots because the
/// factory trait objects are `Fn`, not `FnMut`.
fn recording_factories(
    log: &StartupLog,
    dir: &Path,
    shell: RecordingShell,
    adapter: Result<FakeAdapter, PlatformError>,
    overlay: Result<FakeOverlay, PlatformError>,
    config: Result<Config, StartupError>,
) -> RunFactories<FakeAdapter, FakeOverlay> {
    let lock_path = dir.join("instance.lock");
    RunFactories {
        instance_lock_path: {
            let log = Arc::clone(log);
            Box::new(move || {
                log_push(&log, "instance-lock");
                Some(lock_path.clone())
            })
        },
        try_acquire_instance_lock: Box::new(config::try_acquire_instance_lock),
        load_config: {
            let log = Arc::clone(log);
            let slot = Mutex::new(Some(config));
            Box::new(move || {
                log_push(&log, "config");
                slot.lock()
                    .unwrap()
                    .take()
                    .expect("load_config called twice")
            })
        },
        install_signal_handlers: {
            let log = Arc::clone(log);
            Box::new(move || log_push(&log, "signals"))
        },
        make_shell: {
            let log = Arc::clone(log);
            let slot = Mutex::new(Some(shell));
            Box::new(move || {
                log_push(&log, "shell");
                Arc::new(
                    slot.lock()
                        .unwrap()
                        .take()
                        .expect("make_shell called twice"),
                ) as Arc<dyn ShellHost>
            })
        },
        make_adapter: {
            let log = Arc::clone(log);
            let slot = Mutex::new(Some(adapter));
            Box::new(move |_acceptance_pid| {
                log_push(&log, "adapter");
                slot.lock()
                    .unwrap()
                    .take()
                    .expect("make_adapter called twice")
            })
        },
        make_overlay: {
            let log = Arc::clone(log);
            let slot = Mutex::new(Some(overlay));
            Box::new(move || {
                log_push(&log, "overlay");
                slot.lock()
                    .unwrap()
                    .take()
                    .expect("make_overlay called twice")
            })
        },
        make_tray: {
            let log = Arc::clone(log);
            Box::new(move |flags| {
                log_push(&log, "tray");
                crate::shell::stub::make_tray(flags)
            })
        },
    }
}

#[test]
fn startup_orders_lock_config_signals_permissions_before_platform() {
    // The c92 class: nothing that touches the platform (AX observers,
    // hotkeys, engine) may run before the instance lock, config, signal
    // handlers, and the permission check have. The sequence IS the
    // contract, so pin it exactly — every adjacent pair is an invariant a
    // wiring regression could silently swap while compiling green.
    let log = startup_log();
    let dir = startup_test_dir("order");
    let factories = recording_factories(
        &log,
        &dir,
        RecordingShell {
            trusted: true,
            log: Arc::clone(&log),
        },
        Ok(FakeAdapter::allow_all(Arc::clone(&log))),
        Ok(FakeOverlay),
        Ok(startup_test_config()),
    );

    let ctx = startup(&factories)
        .expect("startup succeeds")
        .expect("first instance proceeds");

    assert_eq!(
            log_steps(&log),
            vec![
                "instance-lock",
                "config",
                "signals",
                "shell",
                "permissions",
                "adapter",
                "overlay",
                "subscribe-focus",
                "subscribe-caret",
                "subscribe-accept",
                "tray",
            ],
            "startup order regressed — lock → config → signals → permissions → platform is the c92 contract"
        );
    assert!(
        ctx.model_available,
        "stub completion means a model is ready"
    );
    assert!(
        ctx.accessibility_subscriptions == AccessibilitySubscriptions::Ready,
        "all subscriptions installed against a trusted shell"
    );
    assert!(ctx.tray.is_none(), "stub tray is unsupported → headless");
    drop(ctx);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(target_os = "linux")]
fn startup_applies_accept_bindings_before_adapter_construction() {
    crate::shell::set_accept_keymap_from_config_with_mods(None, None, None).unwrap();
    let log = startup_log();
    let dir = startup_test_dir("bindings-before-adapter");
    let config = Config::from_lookup(lookup(&[
        ("COMPME_STUB_COMPLETION", " test"),
        ("COMPME_ACCEPT_WORD_KEY", "36"),
        ("COMPME_ACCEPT_FULL_KEY", "48"),
    ]));
    let mut factories = recording_factories(
        &log,
        &dir,
        RecordingShell {
            trusted: true,
            log: Arc::clone(&log),
        },
        Ok(FakeAdapter::allow_all(Arc::clone(&log))),
        Ok(FakeOverlay),
        Ok(config),
    );
    factories.make_adapter = {
        let log = Arc::clone(&log);
        Box::new(move |_acceptance_pid| {
            log_push(&log, "adapter");
            assert_eq!(
                crate::shell::effective_accept_keys_with_mods_and_grammar(),
                ((36, 0), (48, 0), None),
                "the installability probe must see persisted bindings"
            );
            Ok(FakeAdapter::allow_all(Arc::clone(&log)))
        })
    };

    let ctx = startup(&factories).expect("startup").expect("context");
    drop(ctx);
    crate::shell::set_accept_keymap_from_config_with_mods(None, None, None).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn startup_config_failure_aborts_before_platform_construction() {
    // Fail-closed config: an existing-but-unreadable config file aborts
    // startup (complements the binary-level app/tests/config_startup.rs,
    // which exercises the real unreadable file). This pins the abort
    // POINT: after the instance lock, before any platform construction.
    let log = startup_log();
    let dir = startup_test_dir("config");
    let factories = recording_factories(
        &log,
        &dir,
        RecordingShell {
            trusted: true,
            log: Arc::clone(&log),
        },
        Ok(FakeAdapter::allow_all(Arc::clone(&log))),
        Ok(FakeOverlay),
        Err("failed to read config /private/tmp/x/config.env: permission denied".to_string()),
    );

    let err = startup(&factories)
        .err()
        .expect("config failure aborts startup");

    assert!(
        err.contains("failed to read config"),
        "unexpected error: {err}"
    );
    assert_eq!(
        log_steps(&log),
        vec!["instance-lock", "config"],
        "no shell/adapter/overlay/tray may be constructed after a config failure"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn startup_degraded_subscriptions_surface_requires_relaunch() {
    // AX permission missing → every subscription degrades to no-op
    // ("grant it, then relaunch"). Startup still completes, but the
    // requires-relaunch state must surface: the run reflects Blocked
    // instead of silently running deaf.
    let log = startup_log();
    let dir = startup_test_dir("degraded");
    let factories = recording_factories(
        &log,
        &dir,
        RecordingShell {
            trusted: false,
            log: Arc::clone(&log),
        },
        Ok(FakeAdapter::failing(
            Arc::clone(&log),
            PlatformError::PermissionMissing {
                permission: "Accessibility".to_string(),
            },
        )),
        Ok(FakeOverlay),
        Ok(startup_test_config()),
    );

    let ctx = startup(&factories)
        .expect("degraded subscriptions are non-fatal")
        .expect("startup completes");

    assert!(
        ctx.accessibility_subscriptions == AccessibilitySubscriptions::RelaunchRequired,
        "degraded focus/caret/accept subscriptions must surface as requires-relaunch"
    );
    assert!(
        !runtime_trusted(true, ctx.accessibility_subscriptions),
        "requires-relaunch keeps the run Blocked even after permission is granted"
    );
    // The permission prompt fired, all three subscriptions were attempted,
    // and startup still ran to completion (the tray is built last).
    assert_eq!(
        log_steps(&log),
        vec![
            "instance-lock",
            "config",
            "signals",
            "shell",
            "permissions",
            "permission-prompt",
            "adapter",
            "overlay",
            "subscribe-focus",
            "subscribe-caret",
            "subscribe-accept",
            "tray",
        ]
    );
    drop(ctx);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn startup_survives_missing_linux_accessibility_service_without_permission_prompt() {
    let log = startup_log();
    let dir = startup_test_dir("accessibility-unavailable");
    let factories = recording_factories(
        &log,
        &dir,
        RecordingShell {
            trusted: true,
            log: Arc::clone(&log),
        },
        Ok(FakeAdapter::failing(
            Arc::clone(&log),
            PlatformError::AccessibilityUnavailable {
                reason: "AT-SPI accessibility session unavailable (no bus)".to_string(),
            },
        )),
        Ok(FakeOverlay),
        Ok(startup_test_config()),
    );

    let ctx = startup(&factories)
        .expect("an unavailable AT-SPI service is non-fatal")
        .expect("startup remains alive with inert subscriptions");

    assert_eq!(
        ctx.accessibility_subscriptions,
        AccessibilitySubscriptions::Unavailable
    );
    assert_eq!(
        derive_status(
            true,
            ctx.accessibility_subscriptions,
            false,
            true,
            true,
            true,
        ),
        AppStatus::Blocked(BlockReason::AccessibilityUnavailable),
        "service absence must not masquerade as a relaunchable permission transition",
    );
    assert_eq!(
        log_steps(&log),
        vec![
            "instance-lock",
            "config",
            "signals",
            "shell",
            "permissions",
            "adapter",
            "overlay",
            "subscribe-focus",
            "subscribe-caret",
            "subscribe-accept",
            "tray",
        ],
        "service absence must not masquerade as missing permission or abort startup",
    );
    drop(ctx);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn startup_instance_lock_collision_exits_before_touching_adapter() {
    // Second instance: the real flock is already held (by this test, on a
    // temp path — same-process flocks contend because each open() is a
    // separate description). The gate takes the clean-exit arm without
    // constructing a single platform object: the fast unit version of
    // bundle-smoke's COMPME_ACCEPTANCE_PID=444 double-launch check.
    let log = startup_log();
    let dir = startup_test_dir("collision");
    let held =
        config::try_acquire_instance_lock(&dir.join("instance.lock")).expect("test holds the lock");
    let factories = recording_factories(
        &log,
        &dir,
        RecordingShell {
            trusted: true,
            log: Arc::clone(&log),
        },
        Ok(FakeAdapter::allow_all(Arc::clone(&log))),
        Ok(FakeOverlay),
        Ok(startup_test_config()),
    );

    let outcome = startup(&factories).expect("lock contention is a clean exit, not an error");

    assert!(
        outcome.is_none(),
        "second instance exits without a run context"
    );
    assert_eq!(
        log_steps(&log),
        vec!["instance-lock"],
        "config/signals/shell/adapter must not run behind a held instance lock"
    );
    drop(held);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn startup_adapter_permission_failure_stops_before_engine() {
    // AX-permission-negative adapter init: make_adapter fails closed on
    // the missing grant → startup aborts at "adapter init" before the
    // overlay exists, so the engine is never constructed.
    let log = startup_log();
    let dir = startup_test_dir("adapter-denied");
    let factories = recording_factories(
        &log,
        &dir,
        RecordingShell {
            trusted: false,
            log: Arc::clone(&log),
        },
        Err(PlatformError::PermissionMissing {
            permission: "Accessibility".to_string(),
        }),
        Ok(FakeOverlay),
        Ok(startup_test_config()),
    );

    let err = startup(&factories)
        .err()
        .expect("adapter init failure aborts startup");

    assert!(err.starts_with("adapter init: "), "unexpected error: {err}");
    assert_eq!(
        log_steps(&log),
        vec![
            "instance-lock",
            "config",
            "signals",
            "shell",
            "permissions",
            "permission-prompt",
            "adapter",
        ],
        "overlay/engine/subscriptions/tray must not run after adapter init fails"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn startup_overlay_failure_stops_before_engine() {
    // Overlay-init twin of the adapter-permission arm above: make_overlay
    // fails → startup aborts at "overlay init" after the adapter exists
    // but before the engine is constructed, so no subscriptions or tray.
    let log = startup_log();
    let dir = startup_test_dir("overlay-init");
    let factories = recording_factories(
        &log,
        &dir,
        RecordingShell {
            trusted: true,
            log: Arc::clone(&log),
        },
        Ok(FakeAdapter::allow_all(Arc::clone(&log))),
        Err(PlatformError::CannotComplete {
            reason: "overlay unavailable".to_string(),
        }),
        Ok(startup_test_config()),
    );

    let err = startup(&factories)
        .err()
        .expect("overlay init failure aborts startup");

    assert!(err.starts_with("overlay init: "), "unexpected error: {err}");
    assert_eq!(
        log_steps(&log),
        vec![
            "instance-lock",
            "config",
            "signals",
            "shell",
            "permissions",
            "adapter",
            "overlay",
        ],
        "engine/subscriptions/tray must not run after overlay init fails"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// — heartbeat phases: each `*_phase` fn's consume-edge / mutate / persist
//   contract, driven through the same fakes as the startup seam. —

/// Point `config::config_file_path()` (and therefore the app-support models
/// dir) at a hermetic temp dir for one test. `COMPME_CONFIG` is restored on
/// drop (unwind included) so no later test — and never the developer's real
/// `config.env` — sees a phase's persist. Process-env mutation is safe here
/// only because the `app` lane is pinned to `--test-threads=1`.
/// Serialises every test that owns a `PhaseConfigHome`: the phases read
/// `COMPME_CONFIG` from the process environment, and the Windows/Linux CI lanes
/// run this crate's tests in parallel (only the macOS lane passes
/// `--test-threads=1`), so two phase tests racing on `set_var` would persist
/// into each other's temp home. The guard is held for the test's lifetime.
static PHASE_ENV_LOCK: Mutex<()> = Mutex::new(());

struct PhaseConfigHome {
    dir: PathBuf,
    previous: Option<std::ffi::OsString>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl PhaseConfigHome {
    fn new(tag: &str) -> Self {
        let serial = PHASE_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!("compme-phase-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let previous = std::env::var_os("COMPME_CONFIG");
        std::env::set_var("COMPME_CONFIG", dir.join("config.env"));
        Self {
            dir,
            previous,
            _serial: serial,
        }
    }

    fn config_path(&self) -> PathBuf {
        self.dir.join("config.env")
    }

    fn models_dir(&self) -> PathBuf {
        self.dir.join("models")
    }

    /// The persisted key/value map; an absent file reads as empty.
    fn persisted(&self) -> HashMap<String, String> {
        config::load_file_map(&self.config_path()).expect("config.env readable")
    }
}

impl Drop for PhaseConfigHome {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var("COMPME_CONFIG", value),
            None => std::env::remove_var("COMPME_CONFIG"),
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Shell double for the phases: a programmable confirm answer, a pid → bundle
/// id map (the frontmost-app resolver), a screen-grant state, and a call log
/// so "which privileged call ran" is observable.
struct PhaseShell {
    confirm_answer: bool,
    screen_granted: bool,
    bundle_ids: HashMap<i32, String>,
    calls: Mutex<Vec<String>>,
}

impl PhaseShell {
    fn new() -> Self {
        Self {
            confirm_answer: false,
            screen_granted: false,
            bundle_ids: HashMap::new(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn confirming(mut self, answer: bool) -> Self {
        self.confirm_answer = answer;
        self
    }

    fn with_app(mut self, pid: i32, app: &str) -> Self {
        self.bundle_ids.insert(pid, app.to_string());
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl ShellHost for PhaseShell {
    fn pump_events(&self, _heartbeat: Duration) {}
    fn prompt_accessibility_trust(&self) -> bool {
        self.record("prompt-ax".into());
        true
    }
    fn screen_capture_permission(&self) -> bool {
        self.screen_granted
    }
    fn request_screen_capture_permission(&self) -> bool {
        self.record("request-screen".into());
        true
    }
    fn physical_memory_bytes(&self) -> u64 {
        0
    }
    fn bundle_id_for_pid(&self, pid: i32) -> Option<String> {
        self.bundle_ids.get(&pid).cloned()
    }
    fn open_url(&self, _url: &str) -> Result<(), PlatformError> {
        Ok(())
    }
    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        Ok(())
    }
    fn reveal_file(&self, path: &Path) -> Result<(), PlatformError> {
        self.record(format!("reveal:{}", path.display()));
        Ok(())
    }
    fn set_launch_at_login(&self, _enabled: bool) -> Result<(), PlatformError> {
        Ok(())
    }
    fn confirm(&self, prompt: &shell_flags::ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
        self.record(format!("confirm:{}", prompt.title));
        Ok(self.confirm_answer)
    }
    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
        Ok([0; 32])
    }
}

fn phase_shell(shell: PhaseShell) -> (Arc<PhaseShell>, Arc<dyn ShellHost>) {
    let shell = Arc::new(shell);
    let host: Arc<dyn ShellHost> = Arc::clone(&shell) as Arc<dyn ShellHost>;
    (shell, host)
}

fn phase_settings_flags(config: &Config) -> crate::shell::SettingsFlags {
    build_settings_flags(config, Arc::new(AtomicBool::new(true)), false, 8)
}

fn phase_tray_flags() -> TrayFlags {
    TrayFlags {
        enabled: Arc::new(AtomicBool::new(true)),
        quit: Arc::new(AtomicBool::new(false)),
        open_settings: Arc::new(AtomicBool::new(false)),
        snooze_requested: Arc::new(AtomicBool::new(false)),
        global_disable: Arc::new(Mutex::new(None)),
        open_settings_window: Arc::new(AtomicBool::new(false)),
        check_updates: Arc::new(AtomicBool::new(false)),
        visit_website: Arc::new(AtomicBool::new(false)),
        contact_support: Arc::new(AtomicBool::new(false)),
        collection_toggle: Arc::new(AtomicBool::new(false)),
        app_disable: Arc::new(Mutex::new(None)),
    }
}

/// An engine over the inert fakes: the dismiss edge is observed through the
/// `suggestion.latest` slot the phases clear alongside it.
fn phase_engine() -> Engine<SharedAdapter<FakeAdapter>, FakeOverlay> {
    Engine::new(
        SharedAdapter::new(Arc::new(FakeAdapter::allow_all(startup_log()))),
        FakeOverlay,
        0,
        8,
        32,
    )
}

fn phase_settings_state(apps_ids: Vec<String>) -> SettingsState {
    let mut settings = SettingsState::new(false, false, EmojiPrefs::default(), 0, 0, false);
    settings.apps_ids = apps_ids;
    settings
}

/// A `FocusContext` on `field`, plus the resolver map entry for its pid.
fn phase_focus(field: &FieldHandle) -> FocusContext {
    let mut focus = FocusContext::new();
    focus.current_field = Some(field.clone());
    focus
}

fn phase_pending_suggestion() -> SuggestionState {
    let mut suggestion = SuggestionState::new();
    suggestion.latest.offer(request_for_submit_tracking(1));
    suggestion
}

fn phase_monitored_with_buffer(field: &FieldHandle) -> MonitoredInput {
    let mut monitored = MonitoredInput::default();
    monitored
        .monitored_buffers
        .insert(field.clone(), MonitoredBuffer::Collecting("partial".into()));
    monitored
}

// model_download_phase

#[test]
fn model_download_phase_without_click_or_status_is_a_no_op() {
    let home = PhaseConfigHome::new("dl-noop");
    let mut config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let (_shell, host) = phase_shell(PhaseShell::new());
    let mut downloader = None;
    let mut download = DownloadState::default();

    model_download_phase(
        &flags,
        &host,
        &mut config,
        64,
        &mut downloader,
        &mut download,
    );

    assert!(downloader.is_none(), "no click → no downloader spawned");
    assert!(download.model_download_status.is_none());
    assert_eq!(download.model_download_logged, 0);
    assert!(!home.config_path().exists(), "nothing persisted");
}

#[test]
fn model_download_phase_ram_block_consumes_the_click_without_starting() {
    // The recommended entry needs ≥ 2 GiB; 0 GiB available takes the
    // BlockedByRam arm: the click edge is consumed, no downloader is
    // spawned, no status is armed, and nothing (license or path) persists.
    let home = PhaseConfigHome::new("dl-ram");
    let mut config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let (shell, host) = phase_shell(PhaseShell::new().confirming(true));
    flags.setup_download_model.store(true, Ordering::Relaxed);
    let mut downloader = None;
    let mut download = DownloadState::default();

    model_download_phase(
        &flags,
        &host,
        &mut config,
        0,
        &mut downloader,
        &mut download,
    );

    assert!(
        !flags.setup_download_model.load(Ordering::Relaxed),
        "click edge consumed"
    );
    assert!(
        downloader.is_none(),
        "blocked click must not spawn a downloader"
    );
    assert!(download.model_download_status.is_none());
    assert!(config.license_accepted.is_empty());
    assert!(
        shell.calls().is_empty(),
        "no license prompt before the RAM gate"
    );
    assert!(!home.config_path().exists());
}

#[test]
fn model_download_phase_click_while_running_is_consumed_and_only_logs_progress() {
    // A second click during an in-flight download is swallowed by the
    // download_idle gate (edge consumed, nothing restarted); the progress
    // cursor still advances 0 → 1 on the Running transition.
    let home = PhaseConfigHome::new("dl-running");
    let mut config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let (_shell, host) = phase_shell(PhaseShell::new());
    flags.setup_download_model.store(true, Ordering::Relaxed);
    let status = Arc::new(model_fetch::DownloadStatus::default());
    *status.state.lock().unwrap() = model_fetch::DownloadState::Running;
    let mut downloader = None;
    let mut download = DownloadState {
        model_download_status: Some(Arc::clone(&status)),
        model_download_logged: 0,
    };

    model_download_phase(
        &flags,
        &host,
        &mut config,
        64,
        &mut downloader,
        &mut download,
    );

    assert!(!flags.setup_download_model.load(Ordering::Relaxed));
    assert!(downloader.is_none(), "busy download must not spawn another");
    assert!(
        download
            .model_download_status
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(s, &status)),
        "the in-flight status is kept"
    );
    assert_eq!(download.model_download_logged, 1, "Running logged once");
    assert!(!home.config_path().exists());
}

#[test]
fn model_download_phase_done_edge_persists_model_path_exactly_once() {
    // The Done transition auto-wires COMPME_MODEL_PATH to the downloaded
    // file; a later heartbeat on the same Done state must not re-persist.
    let home = PhaseConfigHome::new("dl-done");
    let mut config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let (_shell, host) = phase_shell(PhaseShell::new());
    let done_path = home.models_dir().join("m.gguf");
    let status = Arc::new(model_fetch::DownloadStatus::default());
    *status.state.lock().unwrap() = model_fetch::DownloadState::Done(done_path.clone());
    let mut downloader = None;
    let mut download = DownloadState {
        model_download_status: Some(status),
        model_download_logged: 0,
    };

    model_download_phase(
        &flags,
        &host,
        &mut config,
        64,
        &mut downloader,
        &mut download,
    );

    assert_eq!(download.model_download_logged, 2, "terminal state logged");
    assert_eq!(
        home.persisted().get("COMPME_MODEL_PATH"),
        Some(&done_path.to_string_lossy().to_string())
    );

    // Same Done state, second heartbeat: cursor stays at 2 and the key is
    // not rewritten (remove it and check it stays gone).
    config::remove_setting(&home.config_path(), "COMPME_MODEL_PATH").unwrap();
    model_download_phase(
        &flags,
        &host,
        &mut config,
        64,
        &mut downloader,
        &mut download,
    );
    assert_eq!(download.model_download_logged, 2);
    assert!(
        !home.persisted().contains_key("COMPME_MODEL_PATH"),
        "Done edge fires once per download"
    );
}

// drain_deep_links_phase

#[test]
fn drain_deep_links_phase_applies_a_confirmed_link_and_persists_overrides() {
    let home = PhaseConfigHome::new("dl-link-ok");
    let config = startup_test_config();
    let (shell, host) = phase_shell(PhaseShell::new().confirming(true));
    let deep_links = Mutex::new(vec![
        "compme://setOverride?app=com.apple.TextEdit&excluded=true".to_string(),
    ]);
    let mut prefs = Prefs::default();
    let field = field_with_app("com.apple.TextEdit");
    let mut monitored = phase_monitored_with_buffer(&field);
    let mut suggestion = phase_pending_suggestion();
    let mut engine = phase_engine();

    drain_deep_links_phase(
        &deep_links,
        &host,
        &config,
        &mut prefs,
        &mut monitored,
        &mut suggestion,
        &mut engine,
    );

    assert!(deep_links.lock().unwrap().is_empty(), "queue drained");
    assert_eq!(
        shell.calls(),
        vec!["confirm:Allow configuration change?"],
        "the §16 confirmation ran once"
    );
    assert!(prefs.excluded_apps.contains("com.apple.TextEdit"));
    assert_eq!(
        home.persisted()
            .get("COMPME_EXCLUDED_APPS")
            .map(String::as_str),
        Some("com.apple.TextEdit"),
        "applied override persisted"
    );
    assert!(
        monitored.monitored_buffers.is_empty(),
        "policy transition clears monitored capture"
    );
    assert!(
        suggestion.latest.take().is_none(),
        "dismiss edge clears the pending suggestion"
    );
}

#[test]
fn drain_deep_links_phase_declined_or_malformed_links_change_nothing() {
    // Declined: the prompt ran, prefs/config/suggestion untouched. Malformed:
    // rejected by the parser BEFORE any prompt. Both drain the queue.
    let home = PhaseConfigHome::new("dl-link-declined");
    let config = startup_test_config();
    let (shell, host) = phase_shell(PhaseShell::new().confirming(false));
    let deep_links = Mutex::new(vec![
        "compme://setOverride?app=com.apple.TextEdit&excluded=true".to_string(),
        "compme://setEverything?x=1".to_string(),
    ]);
    let mut prefs = Prefs::default();
    let mut monitored = MonitoredInput::default();
    let mut suggestion = phase_pending_suggestion();
    let mut engine = phase_engine();

    drain_deep_links_phase(
        &deep_links,
        &host,
        &config,
        &mut prefs,
        &mut monitored,
        &mut suggestion,
        &mut engine,
    );

    assert!(deep_links.lock().unwrap().is_empty(), "queue drained");
    assert_eq!(
        shell.calls(),
        vec!["confirm:Allow configuration change?"],
        "only the well-formed link reaches the prompt"
    );
    assert_eq!(prefs, Prefs::default(), "declined link leaves prefs alone");
    assert!(!home.config_path().exists(), "nothing persisted");
    assert!(
        suggestion.latest.take().is_some(),
        "no override applied → no dismiss edge"
    );
}

// setup_pane_actions_phase

#[test]
fn setup_pane_actions_phase_consumes_grant_ax_and_screen_edges() {
    let _home = PhaseConfigHome::new("setup-edges");
    let mut config = startup_test_config();
    let flags = phase_settings_flags(&config);

    // Screen context OFF: the request edge is consumed but no OS call runs.
    let (shell, host) = phase_shell(PhaseShell::new());
    flags.setup_grant_ax.store(true, Ordering::Relaxed);
    flags.setup_request_screen.store(true, Ordering::Relaxed);
    setup_pane_actions_phase(&flags, &host, &config);
    assert!(!flags.setup_grant_ax.load(Ordering::Relaxed));
    assert!(!flags.setup_request_screen.load(Ordering::Relaxed));
    assert_eq!(shell.calls(), vec!["prompt-ax"]);

    // Screen context ON and not yet granted: the request runs.
    config.screen_context = true;
    let (shell, host) = phase_shell(PhaseShell::new());
    flags.setup_request_screen.store(true, Ordering::Relaxed);
    setup_pane_actions_phase(&flags, &host, &config);
    assert_eq!(shell.calls(), vec!["request-screen"]);

    // Already granted: nothing to request.
    let (shell, host) = phase_shell(PhaseShell {
        screen_granted: true,
        ..PhaseShell::new()
    });
    flags.setup_request_screen.store(true, Ordering::Relaxed);
    setup_pane_actions_phase(&flags, &host, &config);
    assert!(shell.calls().is_empty());
    assert!(!flags.setup_request_screen.load(Ordering::Relaxed));
}

#[test]
fn setup_pane_actions_phase_reveals_the_models_dir_after_creating_it() {
    let home = PhaseConfigHome::new("setup-reveal");
    let config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let (shell, host) = phase_shell(PhaseShell::new());
    flags.setup_reveal_models_dir.store(true, Ordering::Relaxed);
    assert!(!home.models_dir().exists());

    setup_pane_actions_phase(&flags, &host, &config);

    assert!(!flags.setup_reveal_models_dir.load(Ordering::Relaxed));
    assert!(home.models_dir().is_dir(), "created before reveal");
    assert_eq!(
        shell.calls(),
        vec![format!("reveal:{}", home.models_dir().display())]
    );
}

#[test]
fn setup_pane_actions_phase_chosen_model_is_validated_before_persist() {
    let home = PhaseConfigHome::new("setup-byom");
    let config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let (_shell, host) = phase_shell(PhaseShell::new());

    // A non-GGUF pick is rejected: slot consumed, nothing persisted.
    let bad = home.dir.join("bad.gguf");
    std::fs::write(&bad, b"NOPE1234").unwrap();
    *flags.setup_choose_model.lock().unwrap() = Some(bad);
    setup_pane_actions_phase(&flags, &host, &config);
    assert!(flags.setup_choose_model.lock().unwrap().is_none());
    assert!(
        !home.config_path().exists(),
        "rejected model never persists"
    );

    // A GGUF-magic pick points COMPME_MODEL_PATH at it in place.
    let good = home.dir.join("good.gguf");
    std::fs::write(&good, b"GGUF\x03\x00\x00\x00rest").unwrap();
    *flags.setup_choose_model.lock().unwrap() = Some(good.clone());
    setup_pane_actions_phase(&flags, &host, &config);
    assert!(flags.setup_choose_model.lock().unwrap().is_none());
    assert_eq!(
        home.persisted().get("COMPME_MODEL_PATH"),
        Some(&good.to_string_lossy().to_string())
    );
}

// apps_row_delete_phase

fn phase_memory_with_two_apps() -> (memory::MemoryStore, Vec<String>) {
    let store = memory::MemoryStore::open_in_memory(
        &memory::StaticKey([5u8; 32]),
        memory::StorageMode::AcceptedOnly,
    )
    .expect("store");
    store.remember("com.a", "alpha").unwrap();
    store.remember("com.b", "beta").unwrap();
    let (_, ids) = compose_apps_rows(Some(&store));
    assert_eq!(ids.len(), 2);
    (store, ids)
}

#[test]
fn apps_row_delete_phase_confirmed_deletes_the_row_and_recomposes() {
    let _home = PhaseConfigHome::new("apps-delete-ok");
    let config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let (shell, host) = phase_shell(PhaseShell::new().confirming(true));
    let (store, ids) = phase_memory_with_two_apps();
    let row = ids.iter().position(|a| a == "com.a").unwrap();
    let survivor = ids[1 - row].clone();
    let memory = Some(store);
    let mut settings = phase_settings_state(ids);
    let prefs = Prefs::default();
    let window = crate::shell::SettingsWindow::new(flags.clone());
    *flags.apps_delete_row.lock().unwrap() = Some(row);

    apps_row_delete_phase(
        &flags,
        &host,
        &memory,
        &mut settings,
        &prefs,
        &config,
        &window,
    );

    assert!(
        flags.apps_delete_row.lock().unwrap().is_none(),
        "edge consumed"
    );
    assert_eq!(shell.calls(), vec!["confirm:Delete recorded inputs?"]);
    let counts = memory.as_ref().unwrap().count_by_app().unwrap();
    assert_eq!(counts, vec![(survivor.clone(), 1)], "only com.a erased");
    assert_eq!(settings.apps_ids, vec![survivor.clone()], "rows recomposed");
    assert!(
        flags
            .apps_lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains(&survivor)),
        "apps_lines republished"
    );
    assert_eq!(
        flags.apps_policy_bits.lock().unwrap().len(),
        1,
        "policy bits re-seeded in the new row order"
    );
}

#[test]
fn apps_row_delete_phase_cancel_keeps_every_row() {
    let _home = PhaseConfigHome::new("apps-delete-cancel");
    let config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let (shell, host) = phase_shell(PhaseShell::new().confirming(false));
    let (store, ids) = phase_memory_with_two_apps();
    let memory = Some(store);
    let mut settings = phase_settings_state(ids.clone());
    let prefs = Prefs::default();
    let window = crate::shell::SettingsWindow::new(flags.clone());
    *flags.apps_delete_row.lock().unwrap() = Some(0);

    apps_row_delete_phase(
        &flags,
        &host,
        &memory,
        &mut settings,
        &prefs,
        &config,
        &window,
    );

    assert!(flags.apps_delete_row.lock().unwrap().is_none());
    assert_eq!(shell.calls(), vec!["confirm:Delete recorded inputs?"]);
    assert_eq!(
        memory.as_ref().unwrap().count().unwrap(),
        2,
        "nothing erased"
    );
    assert_eq!(settings.apps_ids, ids, "rows unchanged");
    assert!(flags.apps_lines.lock().unwrap().is_empty(), "no re-render");
}

#[test]
fn apps_row_delete_phase_stale_row_or_missing_store_never_prompts() {
    let _home = PhaseConfigHome::new("apps-delete-stale");
    let config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let prefs = Prefs::default();
    let window = crate::shell::SettingsWindow::new(flags.clone());

    // Out-of-range row against a live store.
    let (shell, host) = phase_shell(PhaseShell::new().confirming(true));
    let (store, ids) = phase_memory_with_two_apps();
    let memory = Some(store);
    let mut settings = phase_settings_state(ids.clone());
    *flags.apps_delete_row.lock().unwrap() = Some(99);
    apps_row_delete_phase(
        &flags,
        &host,
        &memory,
        &mut settings,
        &prefs,
        &config,
        &window,
    );
    assert!(
        flags.apps_delete_row.lock().unwrap().is_none(),
        "edge consumed"
    );
    assert!(shell.calls().is_empty(), "stale click never prompts");
    assert_eq!(memory.as_ref().unwrap().count().unwrap(), 2);

    // Valid row but memory is off.
    let (shell, host) = phase_shell(PhaseShell::new().confirming(true));
    let no_memory: Option<memory::MemoryStore> = None;
    *flags.apps_delete_row.lock().unwrap() = Some(0);
    apps_row_delete_phase(
        &flags,
        &host,
        &no_memory,
        &mut settings,
        &prefs,
        &config,
        &window,
    );
    assert!(flags.apps_delete_row.lock().unwrap().is_none());
    assert!(shell.calls().is_empty());
    assert_eq!(settings.apps_ids, ids);
}

// apps_row_policy_edit_phase

#[test]
fn apps_row_policy_edit_phase_writes_the_override_and_persists() {
    // Editing an UNFOCUSED app's row: the per-app override lands in prefs
    // and the web-override file, but the focused field's suggestion stays.
    let home = PhaseConfigHome::new("apps-edit-persist");
    let config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let (_shell, host) = phase_shell(PhaseShell::new().with_app(7, "com.focused"));
    let settings = phase_settings_state(vec!["com.a".into()]);
    let mut prefs = Prefs::default();
    let focus = phase_focus(&field_with_app("com.focused"));
    let mut suggestion = phase_pending_suggestion();
    let mut engine = phase_engine();
    *flags.apps_edit.lock().unwrap() = Some((0, 0, false));

    apps_row_policy_edit_phase(
        &flags,
        &settings,
        &mut prefs,
        &focus,
        &host,
        &mut suggestion,
        &mut engine,
    );

    assert!(flags.apps_edit.lock().unwrap().is_none(), "edge consumed");
    assert_eq!(
        prefs.per_app.get("com.a").and_then(|p| p.enabled),
        Some(false),
        "Enabled=false written as a per-app override"
    );
    assert_eq!(
        home.persisted()
            .get("COMPME_DISABLED_APPS")
            .map(String::as_str),
        Some("com.a")
    );
    assert!(
        suggestion.latest.take().is_some(),
        "another app's row never dismisses the focused ghost"
    );
}

#[test]
fn apps_row_policy_edit_phase_disabling_the_focused_app_dismisses() {
    let _home = PhaseConfigHome::new("apps-edit-dismiss");
    let config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let (_shell, host) = phase_shell(PhaseShell::new().with_app(7, "com.a"));
    let settings = phase_settings_state(vec!["com.a".into()]);
    let mut prefs = Prefs::default();
    let focus = phase_focus(&field_with_app("com.a"));
    let mut engine = phase_engine();

    // Disable (Enabled → false) on the focused app: dismiss fires.
    let mut suggestion = phase_pending_suggestion();
    *flags.apps_edit.lock().unwrap() = Some((0, 0, false));
    apps_row_policy_edit_phase(
        &flags,
        &settings,
        &mut prefs,
        &focus,
        &host,
        &mut suggestion,
        &mut engine,
    );
    assert!(suggestion.latest.take().is_none(), "dismiss edge fired");

    // Re-enable on the focused app: no dismiss.
    let mut suggestion = phase_pending_suggestion();
    *flags.apps_edit.lock().unwrap() = Some((0, 0, true));
    apps_row_policy_edit_phase(
        &flags,
        &settings,
        &mut prefs,
        &focus,
        &host,
        &mut suggestion,
        &mut engine,
    );
    assert!(
        suggestion.latest.take().is_some(),
        "enabling never dismisses"
    );
    assert_ne!(
        prefs.per_app.get("com.a").and_then(|p| p.enabled),
        Some(false)
    );
}

#[test]
fn apps_row_policy_edit_phase_ignores_stale_row_or_field() {
    let home = PhaseConfigHome::new("apps-edit-stale");
    let config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let (_shell, host) = phase_shell(PhaseShell::new());
    let settings = phase_settings_state(vec!["com.a".into()]);
    let mut prefs = Prefs::default();
    let focus = FocusContext::new();
    let mut suggestion = SuggestionState::new();
    let mut engine = phase_engine();

    for stale in [(5, 0, false), (0, 9, false)] {
        *flags.apps_edit.lock().unwrap() = Some(stale);
        apps_row_policy_edit_phase(
            &flags,
            &settings,
            &mut prefs,
            &focus,
            &host,
            &mut suggestion,
            &mut engine,
        );
        assert!(flags.apps_edit.lock().unwrap().is_none(), "edge consumed");
    }
    assert_eq!(prefs, Prefs::default(), "stale edits never touch prefs");
    assert!(!home.config_path().exists(), "nothing persisted");
}

// personalization_edits_phase

#[test]
fn personalization_edits_phase_applies_persists_and_mirrors_every_knob() {
    let home = PhaseConfigHome::new("pers-edit");
    let mut config = startup_test_config();
    let flags = phase_settings_flags(&config);
    let window = crate::shell::SettingsWindow::new(flags.clone());
    let inference = InferenceHandle::unavailable();
    use crate::shell::PersonalizationEdit as E;
    *flags.personalization_edit.lock().unwrap() = vec![
        E::GlobalInstructions("be brief".into()),
        E::SenderName("Ann".into()),
        E::SenderEmail("ann@example.com".into()),
        E::StrengthStop(personalization_strength_index(Strength::Max)),
    ];

    personalization_edits_phase(&window, &flags, &mut config, &inference);

    assert!(
        flags.personalization_edit.lock().unwrap().is_empty(),
        "edit queue drained"
    );
    // Source profile updated (survives restart via persist).
    assert_eq!(config.personalization.global_instructions, "be brief");
    assert_eq!(config.personalization.sender.name, "Ann");
    assert_eq!(config.personalization.sender.email, "ann@example.com");
    assert_eq!(config.personalization.strength, Strength::Max);
    // Persisted under the launch-parser keys.
    let persisted = home.persisted();
    assert_eq!(
        persisted.get("COMPME_INSTRUCTIONS").map(String::as_str),
        Some("be brief")
    );
    assert_eq!(
        persisted.get("COMPME_SENDER_NAME").map(String::as_str),
        Some("Ann")
    );
    assert_eq!(
        persisted.get("COMPME_SENDER_EMAIL").map(String::as_str),
        Some("ann@example.com")
    );
    assert_eq!(
        persisted.get("COMPME_STRENGTH").map(String::as_str),
        Some(
            personalization_strength_index(Strength::Max)
                .to_string()
                .as_str()
        )
    );
    // Flag mirrors re-seeded from the applied profile.
    assert_eq!(
        *flags.personalization_instructions.lock().unwrap(),
        "be brief"
    );
    assert_eq!(*flags.personalization_sender_name.lock().unwrap(), "Ann");
    assert_eq!(
        *flags.personalization_sender_email.lock().unwrap(),
        "ann@example.com"
    );
    assert_eq!(
        flags.personalization_strength_index.load(Ordering::Relaxed),
        personalization_strength_index(Strength::Max)
    );
}

#[test]
fn personalization_edits_phase_without_edits_changes_nothing() {
    let home = PhaseConfigHome::new("pers-noop");
    let mut config = startup_test_config();
    let before = config.personalization.clone();
    let flags = phase_settings_flags(&config);
    let window = crate::shell::SettingsWindow::new(flags.clone());
    let inference = InferenceHandle::unavailable();

    personalization_edits_phase(&window, &flags, &mut config, &inference);

    assert_eq!(config.personalization, before);
    assert!(!home.config_path().exists(), "nothing persisted");
}

// tray_collection_toggle_phase

#[test]
fn tray_collection_toggle_phase_flips_collection_and_round_trips_the_no_collect_key() {
    let home = PhaseConfigHome::new("tray-collect");
    let flags = phase_tray_flags();
    let (_shell, host) = phase_shell(PhaseShell::new().with_app(7, "com.a"));
    let field = field_with_app("com.a");
    let focus = phase_focus(&field);
    let mut prefs = Prefs::default();

    // First toggle: collection OFF, key written, capture buffers cleared.
    let mut monitored = phase_monitored_with_buffer(&field);
    flags.collection_toggle.store(true, Ordering::Relaxed);
    tray_collection_toggle_phase(&flags, &host, &focus, &mut prefs, &mut monitored);
    assert!(
        !flags.collection_toggle.load(Ordering::Relaxed),
        "edge consumed"
    );
    assert_eq!(
        prefs.per_app.get("com.a").and_then(|p| p.collect_inputs),
        Some(false)
    );
    assert_eq!(
        home.persisted()
            .get("COMPME_NO_COLLECT_APPS")
            .map(String::as_str),
        Some("com.a")
    );
    assert!(monitored.monitored_buffers.is_empty());

    // Second toggle: back to inherit, and the emptied key is REMOVED (not
    // written blank, which would shadow the env layer).
    flags.collection_toggle.store(true, Ordering::Relaxed);
    tray_collection_toggle_phase(&flags, &host, &focus, &mut prefs, &mut monitored);
    assert_eq!(
        prefs.per_app.get("com.a").and_then(|p| p.collect_inputs),
        None
    );
    assert!(
        !home.persisted().contains_key("COMPME_NO_COLLECT_APPS"),
        "re-enabling the last app clears the key"
    );
}

#[test]
fn tray_collection_toggle_phase_without_a_focused_app_only_consumes_the_edge() {
    let home = PhaseConfigHome::new("tray-collect-nofocus");
    let flags = phase_tray_flags();
    let (_shell, host) = phase_shell(PhaseShell::new());
    let focus = FocusContext::new();
    let mut prefs = Prefs::default();
    let mut monitored = phase_monitored_with_buffer(&field_with_app("com.a"));
    flags.collection_toggle.store(true, Ordering::Relaxed);

    tray_collection_toggle_phase(&flags, &host, &focus, &mut prefs, &mut monitored);

    assert!(!flags.collection_toggle.load(Ordering::Relaxed));
    assert_eq!(prefs, Prefs::default());
    assert!(!home.config_path().exists());
    assert!(
        monitored.monitored_buffers.is_empty(),
        "capture is cleared before app resolution"
    );
}

// tray_app_disable_phase

#[test]
fn tray_app_disable_phase_always_excludes_persists_and_dismisses() {
    let home = PhaseConfigHome::new("tray-disable-always");
    let flags = phase_tray_flags();
    let (_shell, host) = phase_shell(PhaseShell::new().with_app(7, "com.a"));
    let field = field_with_app("com.a");
    let focus = phase_focus(&field);
    let mut prefs = Prefs::default();
    let mut monitored = phase_monitored_with_buffer(&field);
    let mut suggestion = phase_pending_suggestion();
    let mut engine = phase_engine();
    *flags.app_disable.lock().unwrap() = Some(DisableArm::Always);

    tray_app_disable_phase(
        &flags,
        &host,
        &focus,
        &mut prefs,
        &mut monitored,
        &mut suggestion,
        &mut engine,
        1_000,
    );

    assert!(flags.app_disable.lock().unwrap().is_none(), "arm consumed");
    assert!(prefs.excluded_apps.contains("com.a"));
    assert!(
        prefs.app_snooze_until_ms.is_empty(),
        "Always is not a snooze"
    );
    assert_eq!(
        home.persisted()
            .get("COMPME_EXCLUDED_APPS")
            .map(String::as_str),
        Some("com.a")
    );
    assert!(monitored.monitored_buffers.is_empty());
    assert!(suggestion.latest.take().is_none(), "dismiss edge fired");
}

#[test]
fn tray_app_disable_phase_hour_snoozes_the_app_without_persisting() {
    let home = PhaseConfigHome::new("tray-disable-hour");
    let flags = phase_tray_flags();
    let (_shell, host) = phase_shell(PhaseShell::new().with_app(7, "com.a"));
    let field = field_with_app("com.a");
    let focus = phase_focus(&field);
    let mut prefs = Prefs::default();
    let mut monitored = MonitoredInput::default();
    let mut suggestion = phase_pending_suggestion();
    let mut engine = phase_engine();
    let now_ms = 5_000;
    *flags.app_disable.lock().unwrap() = Some(DisableArm::Hour);

    tray_app_disable_phase(
        &flags,
        &host,
        &focus,
        &mut prefs,
        &mut monitored,
        &mut suggestion,
        &mut engine,
        now_ms,
    );

    assert!(flags.app_disable.lock().unwrap().is_none());
    assert_eq!(
        prefs.app_snooze_until_ms.get("com.a").copied(),
        Some(now_ms + SNOOZE_MINUTES * 60 * 1000)
    );
    assert!(prefs.excluded_apps.is_empty(), "Hour never excludes");
    assert!(!home.config_path().exists(), "timed arms are session-only");
    assert!(
        !prefs.should_suggest(Some("com.a"), None, now_ms),
        "suggestions paused in the app"
    );
    assert!(suggestion.latest.take().is_none(), "dismiss edge fired");
}

#[test]
fn tray_app_disable_phase_without_a_focused_app_only_consumes_the_arm() {
    let home = PhaseConfigHome::new("tray-disable-nofocus");
    let flags = phase_tray_flags();
    let (_shell, host) = phase_shell(PhaseShell::new());
    let focus = FocusContext::new();
    let mut prefs = Prefs::default();
    let mut monitored = MonitoredInput::default();
    let mut suggestion = phase_pending_suggestion();
    let mut engine = phase_engine();
    *flags.app_disable.lock().unwrap() = Some(DisableArm::Always);

    tray_app_disable_phase(
        &flags,
        &host,
        &focus,
        &mut prefs,
        &mut monitored,
        &mut suggestion,
        &mut engine,
        1_000,
    );

    assert!(flags.app_disable.lock().unwrap().is_none(), "arm consumed");
    assert_eq!(prefs, Prefs::default());
    assert!(!home.config_path().exists());
    assert!(
        suggestion.latest.take().is_some(),
        "no app resolved → no policy change → no dismiss"
    );
}
