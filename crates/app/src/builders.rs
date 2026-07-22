//! Pure builders and parsers for config-driven state.
//!
//! The run loop supplies a key→value lookup (environment layered over the
//! config file); this module owns the parsing rules that turn raw values into
//! typed emoji prefs, suggestion preferences, and the personalization profile.
//! The model-download filesystem helpers live here too — path preparation,
//! GGUF validation, and downloaded-model discovery.

use std::collections::HashMap;
use std::path::PathBuf;

use emoji::{EmojiPrefs, Gender, SkinTone};
use personalization::{PersonalizationProfile, SenderIdentity, Strength};
use prefs::Prefs;

use crate::config;

pub(crate) fn emoji_config_enabled(lookup: &impl Fn(&str) -> Option<String>) -> bool {
    lookup("COMPME_EMOJI").is_some_and(|v| v == "1" || v == "true" || v == "on")
}

/// Parse emoji prefs (A2 §8/§16) independently of the enable gate so persisted
/// skin-tone/gender choices survive while Emoji completions are disabled.
pub(crate) fn build_emoji_prefs(lookup: &impl Fn(&str) -> Option<String>) -> EmojiPrefs {
    EmojiPrefs {
        skin_tone: parse_skin_tone(lookup("COMPME_EMOJI_SKIN_TONE")),
        gender: parse_gender(lookup("COMPME_EMOJI_GENDER")),
    }
}

pub(crate) fn parse_skin_tone(raw: Option<String>) -> SkinTone {
    match raw
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("light") => SkinTone::Light,
        Some("medium-light") | Some("medium_light") => SkinTone::MediumLight,
        Some("medium") => SkinTone::Medium,
        Some("medium-dark") | Some("medium_dark") => SkinTone::MediumDark,
        Some("dark") => SkinTone::Dark,
        _ => SkinTone::Default,
    }
}

pub(crate) const EMOJI_SKIN_TONE_VALUES: [(SkinTone, &str); 6] = [
    (SkinTone::Default, "default"),
    (SkinTone::Light, "light"),
    (SkinTone::MediumLight, "medium-light"),
    (SkinTone::Medium, "medium"),
    (SkinTone::MediumDark, "medium-dark"),
    (SkinTone::Dark, "dark"),
];

pub(crate) fn emoji_skin_tone_index(tone: SkinTone) -> usize {
    EMOJI_SKIN_TONE_VALUES
        .iter()
        .position(|(candidate, _)| *candidate == tone)
        .unwrap_or(0)
}

pub(crate) fn emoji_skin_tone_from_index(index: usize) -> SkinTone {
    EMOJI_SKIN_TONE_VALUES
        .get(index)
        .map(|(tone, _)| *tone)
        .unwrap_or_default()
}

pub(crate) fn emoji_skin_tone_value(tone: SkinTone) -> &'static str {
    EMOJI_SKIN_TONE_VALUES
        .iter()
        .find_map(|(candidate, value)| (*candidate == tone).then_some(*value))
        .unwrap_or("default")
}

pub(crate) fn parse_gender(raw: Option<String>) -> Gender {
    match raw
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("female") => Gender::Female,
        Some("male") => Gender::Male,
        _ => Gender::Neutral,
    }
}

/// Gender popup rows in menu order (index addresses this table); the second
/// element is the persisted `COMPME_EMOJI_GENDER` value `parse_gender` reads.
pub(crate) const EMOJI_GENDER_VALUES: [(Gender, &str); 3] = [
    (Gender::Neutral, "neutral"),
    (Gender::Female, "female"),
    (Gender::Male, "male"),
];

pub(crate) fn emoji_gender_index(gender: Gender) -> usize {
    EMOJI_GENDER_VALUES
        .iter()
        .position(|(candidate, _)| *candidate == gender)
        .unwrap_or(0)
}

pub(crate) fn emoji_gender_from_index(index: usize) -> Gender {
    EMOJI_GENDER_VALUES
        .get(index)
        .map(|(gender, _)| *gender)
        .unwrap_or_default()
}

pub(crate) fn emoji_gender_value(gender: Gender) -> &'static str {
    EMOJI_GENDER_VALUES
        .iter()
        .find_map(|(candidate, value)| (*candidate == gender).then_some(*value))
        .unwrap_or("neutral")
}

/// Whether the destination model file is already present and complete — a
/// non-empty `.gguf`, from the file's length (`None` = missing). A missing
/// file or a 0-byte stub (an interrupted finalize) is NOT present, so the
/// picker re-downloads rather than treating the stub as done. Guards a repeat
/// "Download" click from re-fetching and clobbering a good file.
fn model_present(dest_len: Option<u64>) -> bool {
    matches!(dest_len, Some(len) if len > 0)
}

/// Build the personalization profile from config (A2 §6). The global
/// instructions key steers every request; optional per-app/per-domain target
/// lists activate supplemental value keys without delimiter-parsing free text.
pub(crate) fn build_personalization(
    lookup: &impl Fn(&str) -> Option<String>,
) -> PersonalizationProfile {
    // Case-handling asymmetry is intentional: per-app keys are kept verbatim
    // (`|app| app.to_string()`) because bundle ids are case-stable identifiers
    // matched against the verbatim `request.field.app`, while per-domain keys are
    // lowercased to match the lowercased host from `domain_from_url`. Do NOT
    // "normalize per_app too" — that would break bundle-id keying.
    let mut profile = PersonalizationProfile {
        global_instructions: lookup("COMPME_INSTRUCTIONS").unwrap_or_default(),
        per_app: instruction_map_from_config(
            lookup,
            "COMPME_INSTRUCTIONS_APPS",
            "COMPME_INSTRUCTIONS_APP_",
            |app| app.to_string(),
        ),
        per_domain: instruction_map_from_config(
            lookup,
            "COMPME_INSTRUCTIONS_DOMAINS",
            "COMPME_INSTRUCTIONS_DOMAIN_",
            webconfig::normalize_domain,
        ),
        sender: SenderIdentity {
            name: lookup("COMPME_SENDER_NAME").unwrap_or_default(),
            email: lookup("COMPME_SENDER_EMAIL").unwrap_or_default(),
        },
        ..Default::default()
    };
    if let Some(stop) = lookup("COMPME_STRENGTH").and_then(|raw| raw.parse::<u8>().ok()) {
        profile.strength = Strength::from_stop(stop);
    }
    profile
}

fn instruction_map_from_config(
    lookup: &impl Fn(&str) -> Option<String>,
    list_key: &str,
    value_prefix: &str,
    normalize_target: impl Fn(&str) -> String,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let targets: Vec<String> = comma_list(lookup(list_key))
        .into_iter()
        .map(|target| normalize_target(&target))
        .collect();
    let mut key_counts = HashMap::new();
    for target in &targets {
        let value_key = format!("{value_prefix}{}", config_target_key_suffix(target));
        *key_counts.entry(value_key).or_insert(0usize) += 1;
    }
    for target in targets {
        let value_key = format!("{value_prefix}{}", config_target_key_suffix(&target));
        if key_counts.get(&value_key) != Some(&1) {
            continue;
        }
        let Some(value) = lookup(&value_key) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        map.insert(target, value.to_string());
    }
    map
}

fn config_target_key_suffix(target: &str) -> String {
    target
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

pub(crate) fn comma_list(raw: Option<String>) -> Vec<String> {
    raw.map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// Parse a fail-safe boolean: only explicit falsy values disable; anything else
/// (incl. unrecognized strings) keeps the safe default so a typo never silently
/// turns the whole product off.
///
/// Shared by two distinct keys on purpose — `COMPME_ENABLED` (the global
/// tray-toggle state, persisted on toggle) and `COMPME_DEFAULT_ENABLED`
/// (the per-app suggestion-policy default in prefs). Both want the same
/// fail-safe-on parse; their SEMANTICS stay separate.
pub(crate) fn parse_enabled_default(raw: Option<String>) -> bool {
    match raw {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => true,
    }
}

/// Build suggestion-gating preferences from config (A2 §8). Comma-separated
/// lists carry app/domain hard excludes, explicit per-app enable/disable policy,
/// and per-app feature overrides.
pub(crate) fn build_prefs(lookup: &impl Fn(&str) -> Option<String>) -> Prefs {
    let excluded_apps = comma_list(lookup("COMPME_EXCLUDED_APPS"))
        .into_iter()
        .collect();
    let mut prefs = Prefs {
        default_enabled: parse_enabled_default(lookup("COMPME_DEFAULT_ENABLED")),
        excluded_apps,
        ..Default::default()
    };
    for domain in comma_list(lookup("COMPME_EXCLUDED_DOMAINS")) {
        prefs
            .excluded_domains
            .insert(webconfig::normalize_domain(&domain));
    }
    for app in comma_list(lookup("COMPME_ENABLED_APPS")) {
        prefs.per_app.entry(app).or_default().enabled = Some(true);
    }
    for app in comma_list(lookup("COMPME_DISABLED_APPS")) {
        prefs.per_app.entry(app).or_default().enabled = Some(false);
    }
    // Per-app typing-history opt-outs (tray "Input Collection in <app>"),
    // mirroring the COMPME_EXCLUDED_APPS comma-list format.
    if let Some(raw) = lookup("COMPME_NO_COLLECT_APPS") {
        for app in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            prefs
                .per_app
                .entry(app.to_string())
                .or_default()
                .collect_inputs = Some(false);
        }
    }
    // Per-app feature overrides (App Settings pane): _ON/_OFF comma lists per
    // feature; ON parses first so a conflicting OFF (parsed second) WINS.
    for app in comma_list(lookup("COMPME_MIDLINE_ON_APPS")) {
        prefs.per_app.entry(app).or_default().mid_line = Some(true);
    }
    for app in comma_list(lookup("COMPME_MIDLINE_OFF_APPS")) {
        prefs.per_app.entry(app).or_default().mid_line = Some(false);
    }
    for app in comma_list(lookup("COMPME_AUTOCORRECT_ON_APPS")) {
        prefs.per_app.entry(app).or_default().autocorrect = Some(true);
    }
    for app in comma_list(lookup("COMPME_AUTOCORRECT_OFF_APPS")) {
        prefs.per_app.entry(app).or_default().autocorrect = Some(false);
    }
    for app in comma_list(lookup("COMPME_GRAMMAR_FIX_ON_APPS")) {
        prefs.per_app.entry(app).or_default().grammar_fix = Some(true);
    }
    for app in comma_list(lookup("COMPME_GRAMMAR_FIX_OFF_APPS")) {
        prefs.per_app.entry(app).or_default().grammar_fix = Some(false);
    }
    for app in comma_list(lookup("COMPME_THESAURUS_ON_APPS")) {
        prefs.per_app.entry(app).or_default().thesaurus = Some(true);
    }
    for app in comma_list(lookup("COMPME_THESAURUS_OFF_APPS")) {
        prefs.per_app.entry(app).or_default().thesaurus = Some(false);
    }
    for app in comma_list(lookup("COMPME_TAB_DISABLED_APPS")) {
        prefs.per_app.entry(app).or_default().tab_disabled = true;
    }
    prefs
}

/// Resolve one config key with env-over-file precedence: the environment value
/// wins, falling back to the file value, else `None` (so `from_lookup` applies
/// the default). Extracted so the precedence direction is unit-testable without
/// mutating the process environment.
pub(crate) fn layered(env_value: Option<String>, file_value: Option<String>) -> Option<String> {
    env_value.or(file_value)
}

pub(crate) fn prepare_model_download_dest(dest: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create model directory {}: {err}",
                parent.display()
            )
        })?;
    }
    Ok(())
}

fn model_download_dest_len(dest: &std::path::Path) -> Option<u64> {
    std::fs::metadata(dest).ok().map(|m| m.len())
}

/// Validate a bring-your-own-model file: a readable, non-empty `.gguf` whose
/// header carries the GGUF magic. Checked at the trust boundary (the file
/// panel) so a bad pick fails at the click, not deep in the model loader after
/// a relaunch. Returns a human-readable reason on rejection.
pub(crate) fn validate_gguf_model(path: &std::path::Path) -> Result<(), String> {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("gguf"))
        != Some(true)
    {
        return Err(format!("{} is not a .gguf file", path.display()));
    }
    let mut file = std::fs::File::open(path)
        .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    let mut magic = [0u8; 4];
    // read_exact on a <4-byte (e.g. empty/partial) file errors, so this also
    // rejects empty stubs.
    std::io::Read::read_exact(&mut file, &mut magic)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    if &magic != b"GGUF" {
        return Err(format!(
            "{} is not a GGUF model (bad header)",
            path.display()
        ));
    }
    Ok(())
}

/// The app-support models directory (sibling of the config file): where
/// `Download Model` writes GGUFs. `None` when no config home resolves.
pub(crate) fn app_support_models_dir() -> Option<PathBuf> {
    config::models_dir_path()
}

/// Create the models directory, then reveal that exact filesystem path.
/// Injection keeps the UI action testable without opening Finder; a creation
/// failure returns before the reveal callback can run.
pub(crate) fn show_models_folder_with(
    path: &std::path::Path,
    create_dir: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    reveal_dir: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    create_dir(path)?;
    reveal_dir(path)
}

/// The most-recently-modified valid `*.gguf` in `dir`, if any. Downloads
/// land in this dir, but the loader otherwise only consults COMPME_MODEL_PATH
/// (env/config file) and the DEFAULT_MODEL fallback — a repo-relative dev path
/// absent from a shipped `.app`. So a model the user already downloaded (this
/// build or an older one) would sit unused and the Setup row stay ✗, with a
/// re-download click reporting only "already present". Newest wins so the
/// latest download is adopted; unreadable, empty, and bad-magic files are skipped.
// ponytail: newest-by-mtime, not the picker's selection — the Download button
// persists the exact selected model; this is only the zero-click fallback.
fn discover_downloaded_model(dir: &std::path::Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if validate_gguf_model(&path).is_err() {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || meta.len() == 0 {
            continue;
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        if newest
            .as_ref()
            .is_none_or(|(newest_mtime, _)| mtime > *newest_mtime)
        {
            newest = Some((mtime, path));
        }
    }
    newest.map(|(_, path)| path)
}

pub(crate) fn downloaded_model_to_adopt(
    stub_completion: Option<&str>,
    configured_path: &std::path::Path,
    models_dir: Option<&std::path::Path>,
) -> Option<PathBuf> {
    if stub_completion.is_some() || validate_gguf_model(configured_path).is_ok() {
        return None;
    }
    models_dir.and_then(discover_downloaded_model)
}

pub(crate) fn model_download_dest_present(
    dest: &std::path::Path,
    expected_sha256: Option<&str>,
) -> Result<bool, String> {
    if !model_present(model_download_dest_len(dest)) {
        return Ok(false);
    }
    let Some(expected) = expected_sha256 else {
        return Ok(true);
    };
    let file = std::fs::File::open(dest)
        .map_err(|err| format!("failed to read existing model {}: {err}", dest.display()))?;
    let actual = model_fetch::read_sha256_hex(std::io::BufReader::new(file))
        .map_err(|err| format!("failed to hash existing model {}: {err}", dest.display()))?;
    Ok(actual == expected.to_ascii_lowercase())
}

pub(crate) fn model_download_ram_block_message(
    entry: &model_catalog::ModelEntry,
    available_ram_gb: u32,
) -> Option<String> {
    (!model_catalog::offerable_by_ram(entry, available_ram_gb)).then(|| {
        format!(
            "download of {} blocked — requires at least {} GiB RAM (available: {} GiB)",
            entry.name, entry.min_ram_gb, available_ram_gb
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// Build a lookup closure from a list of key/value pairs.
    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn personalization_built_from_config_keys() {
        let profile = build_personalization(&lookup(&[
            ("COMPME_INSTRUCTIONS", "Be terse."),
            ("COMPME_STRENGTH", "5"),
            ("COMPME_SENDER_NAME", "Ada"),
        ]));
        assert_eq!(profile.strength, Strength::Max);
        let preamble = profile.build_preamble(Some("com.apple.TextEdit"), None);
        assert!(preamble.contains("Be terse."));
        assert!(preamble.contains("Ada"));
    }

    #[test]
    fn sender_email_is_parsed_into_profile_and_templated_into_preamble() {
        // COMPME_SENDER_EMAIL flows into profile.sender.email and is templated
        // into the steering preamble (the sender line) so the model can address
        // the writer correctly.
        let profile = build_personalization(&lookup(&[
            ("COMPME_INSTRUCTIONS", "Be terse."),
            ("COMPME_SENDER_EMAIL", "ada@example.com"),
        ]));
        assert_eq!(profile.sender.email, "ada@example.com");
        let preamble = profile.build_preamble(Some("com.apple.TextEdit"), None);
        assert!(
            preamble.contains("ada@example.com"),
            "sender email must appear in the built preamble: {preamble:?}"
        );
    }

    #[test]
    fn strength_falls_back_to_default_when_compme_strength_is_unparseable() {
        // COMPME_STRENGTH is present but cannot parse as u8: a non-numeric value
        // and a numeric value that overflows u8 both leave the default stop in
        // place (parse fails => the `if let Some` branch is skipped).
        let default_strength = PersonalizationProfile::default().strength;

        let non_numeric = build_personalization(&lookup(&[("COMPME_STRENGTH", "abc")]));
        assert_eq!(non_numeric.strength, default_strength);

        let overflows_u8 = build_personalization(&lookup(&[("COMPME_STRENGTH", "999")]));
        assert_eq!(overflows_u8.strength, default_strength);
    }

    #[test]
    fn instruction_suffix_folds_non_ascii_to_underscore() {
        // Every non-ASCII-alphanumeric char (including a multi-byte unicode char)
        // folds to a single '_'; ASCII alphanumerics are uppercased. For "café"
        // the 'é' becomes '_', yielding "CAF_".
        assert_eq!(config_target_key_suffix("café"), "CAF_");
    }

    #[test]
    fn personalization_built_from_per_app_and_domain_config_keys() {
        let profile = build_personalization(&lookup(&[
            ("COMPME_INSTRUCTIONS", "Be terse."),
            (
                "COMPME_INSTRUCTIONS_APPS",
                "com.apple.TextEdit, com.apple.Notes, com.missing.App",
            ),
            (
                "COMPME_INSTRUCTIONS_APP_COM_APPLE_TEXTEDIT",
                "Use a plain-text tone.",
            ),
            (
                "COMPME_INSTRUCTIONS_APP_COM_APPLE_NOTES",
                "Prefer note bullets.",
            ),
            (
                "COMPME_INSTRUCTIONS_DOMAINS",
                "Docs.Google.com, mail.example",
            ),
            (
                "COMPME_INSTRUCTIONS_DOMAIN_DOCS_GOOGLE_COM",
                "Prefer document context.",
            ),
        ]));

        assert_eq!(
            profile.per_app.get("com.apple.TextEdit"),
            Some(&"Use a plain-text tone.".to_string())
        );
        assert_eq!(
            profile.per_app.get("com.apple.Notes"),
            Some(&"Prefer note bullets.".to_string())
        );
        assert!(
            !profile.per_app.contains_key("com.missing.App"),
            "listed apps without instruction values should not create empty entries"
        );
        assert_eq!(
            profile.per_domain.get("docs.google.com"),
            Some(&"Prefer document context.".to_string())
        );
        assert!(
            !profile.per_domain.contains_key("mail.example"),
            "listed domains without instruction values should not create empty entries"
        );

        let preamble = profile.build_preamble(Some("com.apple.TextEdit"), Some("docs.google.com"));
        assert!(preamble.contains("Be terse."));
        assert!(preamble.contains("Use a plain-text tone."));
        assert!(preamble.contains("Prefer document context."));
        assert!(!preamble.contains("Prefer note bullets."));
    }

    #[test]
    fn personalization_per_domain_steers_a_subdomain_through_the_assembled_profile() {
        // End-to-end app wiring of the round-1 subdomain matcher: a `google.com`
        // root-dotted rule from config must steer `www.google.com` (both rule and
        // host are canonicalized and matched on a dot boundary), but never a
        // look-alike `evilgoogle.com`. This pins the app-level normalization seam, not just the
        // personalization crate's resolve_instructions.
        let profile = build_personalization(&lookup(&[
            ("COMPME_STRENGTH", "5"),
            ("COMPME_INSTRUCTIONS_DOMAINS", "Google.com."),
            (
                "COMPME_INSTRUCTIONS_DOMAIN_GOOGLE_COM",
                "Prefer search-friendly phrasing.",
            ),
        ]));

        // Config domain was lowercased into the profile key.
        assert_eq!(
            profile.per_domain.get("google.com"),
            Some(&"Prefer search-friendly phrasing.".to_string())
        );

        // A subdomain of the rule is steered.
        let on_subdomain = profile.build_preamble(None, Some("www.google.com"));
        assert!(
            on_subdomain.contains("Prefer search-friendly phrasing."),
            "subdomain www.google.com should match the google.com rule: {on_subdomain:?}"
        );

        // A look-alike host on a non-dot boundary is NOT steered.
        let on_lookalike = profile.build_preamble(None, Some("evilgoogle.com"));
        assert!(
            !on_lookalike.contains("Prefer search-friendly phrasing."),
            "evilgoogle.com must not match the google.com rule: {on_lookalike:?}"
        );
    }

    #[test]
    fn personalization_rejects_duplicate_canonical_domain_targets() {
        let profile = build_personalization(&lookup(&[
            ("COMPME_INSTRUCTIONS_DOMAINS", "Bank.Example,bank.example."),
            (
                "COMPME_INSTRUCTIONS_DOMAIN_BANK_EXAMPLE",
                "Must not resolve ambiguously.",
            ),
        ]));
        assert!(profile.per_domain.is_empty());
    }

    #[test]
    fn personalization_skips_ambiguous_per_target_instruction_keys() {
        let profile = build_personalization(&lookup(&[
            (
                "COMPME_INSTRUCTIONS_APPS",
                "com.example.Editor, com-example-Editor",
            ),
            (
                "COMPME_INSTRUCTIONS_APP_COM_EXAMPLE_EDITOR",
                "Use editor-specific style.",
            ),
            (
                "COMPME_INSTRUCTIONS_DOMAINS",
                "docs.google.com, docs-google-com",
            ),
            (
                "COMPME_INSTRUCTIONS_DOMAIN_DOCS_GOOGLE_COM",
                "Use docs-specific style.",
            ),
        ]));

        assert!(
            profile.per_app.is_empty(),
            "colliding app suffixes must not apply one value to multiple apps"
        );
        assert!(
            profile.per_domain.is_empty(),
            "colliding domain suffixes must not apply one value to multiple domains"
        );
    }

    #[test]
    fn personalization_skips_blank_per_target_instruction_values() {
        // A listed target whose value KEY is present but blank/whitespace-only
        // is a present-but-empty instruction: `instruction_map_from_config`
        // trims it, sees it empty, and skips it — no empty entry is stored.
        // (`com.missing.App` confirms the absent-key path still skips too.)
        let profile = build_personalization(&lookup(&[
            (
                "COMPME_INSTRUCTIONS_APPS",
                "com.apple.TextEdit, com.apple.Notes, com.missing.App",
            ),
            // Present but whitespace-only → must be skipped.
            ("COMPME_INSTRUCTIONS_APP_COM_APPLE_TEXTEDIT", "   "),
            // A real value confirms the non-blank path still stores.
            (
                "COMPME_INSTRUCTIONS_APP_COM_APPLE_NOTES",
                "Prefer note bullets.",
            ),
        ]));

        assert!(
            !profile.per_app.contains_key("com.apple.TextEdit"),
            "a whitespace-only instruction value must not be stored as an empty instruction"
        );
        assert!(
            !profile.per_app.contains_key("com.missing.App"),
            "a listed target with no value key stays absent"
        );
        assert_eq!(
            profile.per_app.get("com.apple.Notes"),
            Some(&"Prefer note bullets.".to_string()),
            "a non-blank value is still stored alongside the skipped blank one"
        );
    }

    #[test]
    fn personalization_defaults_to_no_steer_when_keys_absent() {
        let profile = build_personalization(&lookup(&[]));
        assert_eq!(profile.build_preamble(Some("com.apple.TextEdit"), None), "");
    }

    #[test]
    fn prefs_built_from_excluded_apps_list() {
        let prefs = build_prefs(&lookup(&[(
            "COMPME_EXCLUDED_APPS",
            "com.apple.Finder, com.tinyspeck.slackmacgap",
        )]));
        assert!(!prefs.should_suggest(Some("com.apple.Finder"), None, 0));
        assert!(!prefs.should_suggest(Some("com.tinyspeck.slackmacgap"), None, 0));
        assert!(prefs.should_suggest(Some("com.apple.TextEdit"), None, 0));
    }

    #[test]
    fn prefs_builds_web_override_policy_from_config_lists() {
        let prefs = build_prefs(&lookup(&[
            ("COMPME_EXCLUDED_DOMAINS", "Docs.Google.com, bank.example."),
            (
                "COMPME_ENABLED_APPS",
                "com.example.enabled, com.example.conflict",
            ),
            (
                "COMPME_DISABLED_APPS",
                "com.example.disabled, com.example.conflict",
            ),
        ]));

        assert!(prefs.excluded_domains.contains("docs.google.com"));
        assert!(prefs.excluded_domains.contains("bank.example"));
        assert_eq!(prefs.per_app["com.example.enabled"].enabled, Some(true));
        assert_eq!(prefs.per_app["com.example.disabled"].enabled, Some(false));
        assert_eq!(
            prefs.per_app["com.example.conflict"].enabled,
            Some(false),
            "disabled list is parsed after enabled list so off wins conflicts"
        );
    }

    #[test]
    fn skin_tone_index_round_trips_all_variants_and_clamps_oob() {
        // The skin-tone popup addresses `EMOJI_SKIN_TONE_VALUES` by index, so
        // `emoji_skin_tone_from_index` must invert `emoji_skin_tone_index` for
        // every variant. An out-of-range index clamps to the documented default
        // (`SkinTone::default()` == `SkinTone::Default`), mirroring the gender
        // round-trip above.
        for tone in [
            SkinTone::Default,
            SkinTone::Light,
            SkinTone::MediumLight,
            SkinTone::Medium,
            SkinTone::MediumDark,
            SkinTone::Dark,
        ] {
            assert_eq!(
                emoji_skin_tone_from_index(emoji_skin_tone_index(tone)),
                tone,
                "index round-trip must be lossless for {tone:?}"
            );
        }
        assert_eq!(emoji_skin_tone_from_index(99), SkinTone::Default);
    }

    #[test]
    fn per_app_feature_lists_parse_with_off_winning_conflicts() {
        let prefs = build_prefs(&lookup(&[
            ("COMPME_MIDLINE_ON_APPS", "com.a.one, com.a.both"),
            ("COMPME_MIDLINE_OFF_APPS", "com.a.both"),
            ("COMPME_AUTOCORRECT_OFF_APPS", "com.a.two"),
        ]));
        assert!(prefs.mid_line_enabled(Some("com.a.one"), false), "ON list");
        assert!(
            !prefs.mid_line_enabled(Some("com.a.both"), true),
            "OFF wins the conflict"
        );
        assert!(!prefs.autocorrect_enabled(Some("com.a.two"), true));
        // (Write-back serializers land with the settings-pane watcher — their
        // first production caller; the c22 no-unused-fns rule.)
    }

    #[test]
    fn no_collect_apps_env_parses_into_per_app_collect_overrides() {
        let prefs = build_prefs(&lookup(&[(
            "COMPME_NO_COLLECT_APPS",
            "com.apple.TextEdit, com.googlecode.iterm2",
        )]));
        assert!(!prefs.collection_allowed(Some("com.apple.TextEdit")));
        assert!(!prefs.collection_allowed(Some("com.googlecode.iterm2")));
        assert!(prefs.collection_allowed(Some("com.apple.Safari")));
    }

    #[test]
    fn prefs_default_enabled_fails_safe() {
        // Absent or unrecognized → enabled (a typo never silently kills the app);
        // only explicit falsy values disable.
        assert!(build_prefs(&lookup(&[])).default_enabled);
        assert!(build_prefs(&lookup(&[("COMPME_DEFAULT_ENABLED", "yes")])).default_enabled);
        assert!(build_prefs(&lookup(&[("COMPME_DEFAULT_ENABLED", "True")])).default_enabled);
        assert!(!build_prefs(&lookup(&[("COMPME_DEFAULT_ENABLED", "0")])).default_enabled);
        assert!(!build_prefs(&lookup(&[("COMPME_DEFAULT_ENABLED", "off")])).default_enabled);
    }

    #[test]
    fn layered_lookup_prefers_env_then_file_then_none() {
        // env wins over file (the P1 "env > file > default" precedence).
        assert_eq!(
            layered(Some("env".into()), Some("file".into())),
            Some("env".into())
        );
        // file is the fallback when env is absent.
        assert_eq!(layered(None, Some("file".into())), Some("file".into()));
        // neither present → None, so `from_lookup` applies the built-in default.
        assert_eq!(layered(None, None), None);
    }

    #[test]
    fn model_present_only_for_a_nonempty_existing_file() {
        // The dest-exists guard: a complete .gguf already on disk skips the
        // re-download (avoid clobber + wasted bandwidth on a repeat click).
        assert!(model_present(Some(1)), "a 1-byte+ file is present");
        assert!(model_present(Some(500_000_000)), "a real model is present");
        // A missing file OR a 0-byte stub (an interrupted finalize) is NOT
        // present — re-download rather than treat the stub as done.
        assert!(!model_present(None), "missing file → re-download");
        assert!(!model_present(Some(0)), "0-byte stub → re-download");
    }

    #[test]
    fn model_download_ram_block_message_blocks_only_below_minimum() {
        let entry = model_catalog::recommended().expect("catalog has a recommended model");
        assert!(
            model_download_ram_block_message(entry, entry.min_ram_gb.saturating_sub(1))
                .expect("below minimum is blocked")
                .contains(entry.name)
        );
        assert_eq!(
            model_download_ram_block_message(entry, entry.min_ram_gb),
            None,
            "tight-at-minimum models are allowed with a picker warning"
        );
    }

    #[test]
    fn validate_gguf_model_accepts_gguf_magic_and_rejects_the_rest() {
        let dir = std::env::temp_dir().join(format!("cm-byom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Wrong extension → rejected before any read.
        let txt = dir.join("model.bin");
        std::fs::write(&txt, b"GGUFxxxx").unwrap();
        assert!(
            validate_gguf_model(&txt).is_err(),
            "non-.gguf must be rejected"
        );

        // .gguf extension but wrong magic → rejected.
        let bad = dir.join("bad.gguf");
        std::fs::write(&bad, b"NOPEyyyy").unwrap();
        assert!(
            validate_gguf_model(&bad).is_err(),
            "bad magic must be rejected"
        );

        // Empty .gguf → rejected (read_exact of 4 bytes fails).
        let empty = dir.join("empty.gguf");
        std::fs::write(&empty, b"").unwrap();
        assert!(
            validate_gguf_model(&empty).is_err(),
            "empty must be rejected"
        );

        // Missing file → rejected.
        assert!(validate_gguf_model(&dir.join("nope.gguf")).is_err());

        // Real GGUF magic + uppercase extension → accepted.
        let good = dir.join("model.GGUF");
        std::fs::write(&good, b"GGUF\x03\x00\x00\x00rest").unwrap();
        assert!(
            validate_gguf_model(&good).is_ok(),
            "a GGUF-magic .gguf (case-insensitive ext) must be accepted"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn show_models_folder_creates_then_reveals_the_exact_directory() {
        let path = PathBuf::from("/tmp/compme models/exact");
        let calls = RefCell::new(Vec::new());

        show_models_folder_with(
            &path,
            |created| {
                calls.borrow_mut().push(("create", created.to_path_buf()));
                Ok(())
            },
            |revealed| {
                calls.borrow_mut().push(("reveal", revealed.to_path_buf()));
                Ok(())
            },
        )
        .expect("create + reveal");

        assert_eq!(
            calls.into_inner(),
            vec![("create", path.clone()), ("reveal", path)]
        );
    }

    #[test]
    fn show_models_folder_propagates_create_failure_without_revealing() {
        let path = PathBuf::from("/tmp/compme models/blocked");
        let revealed = Cell::new(false);

        let err = show_models_folder_with(
            &path,
            |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "blocked",
                ))
            },
            |_| {
                revealed.set(true);
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(!revealed.get(), "create failure must stop before reveal");
    }

    #[test]
    fn discover_downloaded_model_picks_newest_valid_gguf() {
        use std::time::{Duration, SystemTime};
        let dir = std::env::temp_dir().join(format!("cm-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Empty dir → nothing to adopt.
        assert_eq!(discover_downloaded_model(&dir), None);

        // A non-gguf file, an empty stub, and bad GGUF magic are ignored.
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join("partial.gguf"), b"").unwrap();
        std::fs::write(dir.join("corrupt.gguf"), b"NOPE").unwrap();
        assert_eq!(
            discover_downloaded_model(&dir),
            None,
            "non-gguf, empty, and malformed candidates must be skipped"
        );

        // Two valid models: the one with the newer mtime wins, regardless of name.
        let older = dir.join("qwen2.5-0.5b-q4_k_m.gguf");
        let newer = dir.join("gemma-2-2b-q4_k_m.gguf");
        std::fs::write(&older, b"GGUF-old").unwrap();
        std::fs::write(&newer, b"GGUF-new").unwrap();
        let base = SystemTime::now();
        set_mtime(&older, base - Duration::from_secs(60));
        set_mtime(&newer, base);
        assert_eq!(
            discover_downloaded_model(&dir).as_deref(),
            Some(newer.as_path()),
            "the most recently modified gguf must win"
        );

        // Touch the older one newer → it now wins (proves mtime, not name/order).
        set_mtime(&older, base + Duration::from_secs(60));
        assert_eq!(
            discover_downloaded_model(&dir).as_deref(),
            Some(older.as_path())
        );

        // A newer malformed file must never poison automatic adoption.
        let corrupt = dir.join("newest-corrupt.gguf");
        std::fs::write(&corrupt, b"NOPE-newest").unwrap();
        set_mtime(&corrupt, base + Duration::from_secs(120));
        assert_eq!(
            discover_downloaded_model(&dir).as_deref(),
            Some(older.as_path()),
            "newest malformed candidate must be skipped in favor of a valid model"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn downloaded_model_adoption_uses_configured_file_validity() {
        let dir = std::env::temp_dir().join(format!("cm-adopt-validity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let models = dir.join("models");
        std::fs::create_dir_all(&models).unwrap();
        let downloaded = models.join("downloaded.gguf");
        std::fs::write(&downloaded, b"GGUF-valid").unwrap();

        let configured = dir.join("configured.gguf");
        std::fs::write(&configured, b"GGUF-valid").unwrap();
        assert_eq!(
            downloaded_model_to_adopt(None, &configured, Some(&models)),
            None,
            "a valid configured model always wins"
        );

        for invalid in [
            dir.join("missing.gguf"),
            dir.join("empty.gguf"),
            dir.join("bad-header.gguf"),
            dir.join("directory.gguf"),
        ] {
            if invalid.file_name().unwrap() == "empty.gguf" {
                std::fs::write(&invalid, b"").unwrap();
            } else if invalid.file_name().unwrap() == "bad-header.gguf" {
                std::fs::write(&invalid, b"NOPE").unwrap();
            } else if invalid.file_name().unwrap() == "directory.gguf" {
                std::fs::create_dir(&invalid).unwrap();
            }
            assert_eq!(
                downloaded_model_to_adopt(None, &invalid, Some(&models)).as_deref(),
                Some(downloaded.as_path()),
                "invalid configured path {} must fall back",
                invalid.display()
            );
        }
        assert_eq!(
            downloaded_model_to_adopt(Some("stub"), &dir.join("missing.gguf"), Some(&models)),
            None,
            "stub mode never scans downloaded models"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Set a file's mtime with stdlib only (File::set_modified, stable 1.75).
    fn set_mtime(path: &std::path::Path, when: std::time::SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[test]
    fn model_download_dest_parent_failure_is_reported() {
        let file_parent = std::env::temp_dir().join(format!(
            "compme-download-parent-blocker-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&file_parent);
        std::fs::write(&file_parent, b"not a directory").unwrap();
        let dest = file_parent.join("model.gguf");
        let result = prepare_model_download_dest(&dest);
        let _ = std::fs::remove_file(&file_parent);
        assert!(
            result.is_err(),
            "download preparation must report parent creation failures"
        );
    }
}
