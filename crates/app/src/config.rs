//! User-editable configuration file, layered under environment variables.
//!
//! The file is dotenv-style `KEY=VALUE`. Its parsed contents are merged into the
//! same lookup the environment uses, so there is one config code path
//! (`run_loop::Config::from_lookup`): env wins over file wins over default.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const ESCAPED_VALUE_PREFIX: &str = "__COMPME_ESCAPED__:";

/// Parse dotenv-style `KEY=VALUE` content into ordered pairs.
///
/// Rules: blank lines and `#` comment lines are ignored; surrounding whitespace
/// on the key and value is trimmed; the first `=` splits key from value (so
/// values may contain `=`); lines without `=`, or with an empty key, are skipped.
pub fn parse_env_file(contents: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        pairs.push((key.to_string(), decode_env_value(value.trim())));
    }
    pairs
}

fn encode_env_value(value: &str) -> String {
    if !value
        .chars()
        .any(|ch| matches!(ch, '\\' | '\n' | '\r' | '\t'))
        && !value.starts_with(ESCAPED_VALUE_PREFIX)
    {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    out.push_str(ESCAPED_VALUE_PREFIX);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

fn decode_env_value(value: &str) -> String {
    let Some(value) = value.strip_prefix(ESCAPED_VALUE_PREFIX) else {
        return value.to_string();
    };
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Parse a value, falling back to `default` when absent or unparseable, then
/// clamp into `[min, max]`. Invalid config never fails startup. Requires
/// `min <= max` (std `clamp` panics otherwise); every call site uses literal
/// `0..max` bounds, and the debug assert documents the invariant.
pub fn parse_clamped<T>(raw: Option<String>, default: T, min: T, max: T) -> T
where
    T: FromStr + Ord + Copy,
{
    debug_assert!(min <= max, "parse_clamped requires min <= max");
    let value = raw
        .and_then(|s| s.trim().parse::<T>().ok())
        .unwrap_or(default);
    value.clamp(min, max)
}

/// Resolve the lifetime-stats file path: sibling of the config file
/// (`stats.env` next to `config.env`, honoring a `COMPME_CONFIG` override's
/// directory). `None` when no config home is resolvable.
pub fn stats_file_path() -> Option<PathBuf> {
    config_file_path().map(|p| p.with_file_name("stats.env"))
}

/// Resolve the config file path: `COMPME_CONFIG` override, else the per-OS
/// config home (`~/Library/Application Support` on macOS, XDG on Linux,
/// `%APPDATA%` on Windows). `None` if the required env var is unavailable.
pub fn config_file_path() -> Option<PathBuf> {
    config_file_path_from(&|key| std::env::var(key).ok())
}

/// Lookup-injected core of [`config_file_path`] (the `Config::from_lookup`
/// pattern): testable without mutating the process environment — `set_var`
/// races parallel tests and is `unsafe` under edition 2024.
fn config_file_path_from(lookup: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    config_file_path_for(lookup, std::env::consts::OS)
}

/// Per-OS config home. `os` is `std::env::consts::OS` in production; injected
/// so every branch is testable on any host.
fn config_file_path_for(lookup: &impl Fn(&str) -> Option<String>, os: &str) -> Option<PathBuf> {
    if let Some(path) = lookup("COMPME_CONFIG") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    // Empty env vars are treated as unset (a relative "" base would write config
    // under the process cwd), matching the COMPME_CONFIG/XDG_CONFIG_HOME guards.
    let dir = match os {
        "windows" => PathBuf::from(lookup("APPDATA").filter(|v| !v.is_empty())?).join("compme"),
        "macos" => PathBuf::from(lookup("HOME").filter(|v| !v.is_empty())?)
            .join("Library/Application Support/compme"),
        _ => match lookup("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(xdg) => PathBuf::from(xdg).join("compme"),
            None => PathBuf::from(lookup("HOME").filter(|v| !v.is_empty())?).join(".config/compme"),
        },
    };
    Some(dir.join("config.env"))
}

/// Load and parse the config file into a map. Missing/unreadable file → empty
/// map (config is optional).
pub fn load_file_map(path: &Path) -> HashMap<String, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => parse_env_file(&contents).into_iter().collect(),
        Err(_) => HashMap::new(),
    }
}

/// Rewrite `contents` so `key` holds `value`, preserving everything else:
/// comments, blank lines, unknown keys, and line order all survive untouched.
/// Every line bearing `key` is rewritten (the read side is last-wins, so
/// leaving a stale duplicate would shadow the update); a missing key is
/// appended as a final `key=value` line.
pub fn update_env_file_contents(contents: &str, key: &str, value: &str) -> String {
    let mut found = false;
    let encoded_value = encode_env_value(value);
    let mut lines: Vec<String> = contents
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if !trimmed.starts_with('#') {
                if let Some((k, _)) = trimmed.split_once('=') {
                    if k.trim() == key {
                        found = true;
                        return format!("{key}={encoded_value}");
                    }
                }
            }
            line.to_string()
        })
        .collect();
    if !found {
        lines.push(format!("{key}={encoded_value}"));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Persist one `key=value` into the config file at `path`, preserving all other
/// content (comments, unknown keys, order). The write is atomic — a temp file
/// in the same directory is renamed over the target — so a crash mid-write can
/// never leave a truncated config. Parent directories are created on first use.
///
/// Fail-closed on read: only a MISSING file is treated as empty. Any other
/// read error (permissions, IO) aborts the persist — rewriting a config we
/// could not read would replace the user's whole file with one key.
///
/// Concurrency: callers are the single-threaded run loop, so writes never
/// interleave. A concurrent hand-edit of the file races last-writer-wins
/// within the microseconds between read and rename — accepted for a settings
/// file toggled a few times a day.
pub fn persist_setting(path: &Path, key: &str, value: &str) -> std::io::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let updated = update_env_file_contents(&existing, key, value);
    atomic_write(path, &updated)
}

/// Atomically replace `path` with `contents`: a temp file in the same directory
/// is renamed over the target, so a crash mid-write prevents a torn/partial
/// config. This is NOT a full power-loss durability guarantee — there is no
/// fsync of the temp file before rename nor of the parent directory, so on
/// power loss the rename can become durable while the data blocks are not.
/// Acceptable for a settings file toggled a few times a day; the tradeoff is
/// intentional. Parent directories are created on first use.
fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "config path has no parent",
        )
    })?;
    // Owner-only dir (on first creation only, so a deliberate later chmod
    // sticks) and file, matching the hardening memory::open applies to the
    // same app-support tree — config.env holds no secret today, but the
    // permission shouldn't need revisiting if one is ever added.
    // Read existence BEFORE create_dir_all; only the unix arm consumes it,
    // so gate the binding too or non-unix clippy -D warnings rejects it.
    #[cfg(any(unix, windows))]
    let created_dir = !dir.exists();
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    if created_dir {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    // Windows analog of the 0700 tightening: owner-only DACL with inheritance,
    // so the temp file below (and every config file) inherits owner-only.
    #[cfg(windows)]
    if created_dir {
        platform_windows::win_host::harden_owner_only(dir)?;
    }
    let temp = dir.join(format!(
        ".{}.tmp.{}",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("config"),
        std::process::id()
    ));
    std::fs::write(&temp, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temp, path)
}

/// Whether any non-comment line in `contents` assigns `key`.
fn config_has_key(contents: &str, key: &str) -> bool {
    contents.lines().any(|line| {
        let trimmed = line.trim();
        !trimmed.starts_with('#')
            && trimmed
                .split_once('=')
                .is_some_and(|(k, _)| k.trim() == key)
    })
}

/// Drop every line assigning `key` from `contents`, preserving comments, blank
/// lines, unknown keys, and order — the inverse of `update_env_file_contents`.
pub fn remove_env_file_key(contents: &str, key: &str) -> String {
    let kept: Vec<&str> = contents
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.starts_with('#') || trimmed.split_once('=').is_none_or(|(k, _)| k.trim() != key)
        })
        .collect();
    if kept.is_empty() {
        return String::new();
    }
    let mut out = kept.join("\n");
    out.push('\n');
    out
}

/// Remove `key` from the config at `path` entirely (no blank `key=` left behind),
/// preserving all other content. A MISSING file or ABSENT key is a no-op — never
/// a rewrite, so clearing an already-clear setting does not churn the file. Other
/// read errors abort (the fail-closed read contract `persist_setting` documents).
pub fn remove_setting(path: &Path, key: &str) -> std::io::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if !config_has_key(&existing, key) {
        return Ok(());
    }
    let updated = remove_env_file_key(&existing, key);
    atomic_write(path, &updated)
}

/// Holds the single-instance file lock for the process lifetime; the kernel
/// releases it on ANY exit (crash included). Dropping releases explicitly.
#[derive(Debug)]
pub struct InstanceLock {
    _file: std::fs::File,
}

/// Why the instance lock was not acquired — the caller's message must not
/// claim "another instance" when the truth is an IO/permissions failure
/// (review finding: misleading diagnostics send users down the wrong path).
#[derive(Debug, PartialEq, Eq)]
pub enum InstanceLockError {
    /// Another live process holds the lock.
    Held,
    /// The lock could not even be attempted (permissions, unwritable dir…).
    Io(String),
}

/// Try to become THE compme instance: an exclusive, non-blocking OS file lock
/// on `path` (parent dirs created). `std::fs::File::try_lock` is
/// `flock(LOCK_EX | LOCK_NB)` on unix and `LockFileEx` on Windows. Launch-method-
/// agnostic: a LaunchServices-spawned copy and a direct-exec'd binary contend
/// on the same file (the c92 finding — two instances would double AX observers
/// and hotkey registrations).
///
/// On [`InstanceLockError::Io`] the app fails closed before installing AX
/// observers or hotkeys. Running unguarded can double-observe private context
/// and double-insert completions.
pub fn try_acquire_instance_lock(path: &Path) -> Result<InstanceLock, InstanceLockError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| InstanceLockError::Io(e.to_string()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| InstanceLockError::Io(e.to_string()))?;
    match file.try_lock() {
        Ok(()) => Ok(InstanceLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(InstanceLockError::Held),
        Err(std::fs::TryLockError::Error(err)) => Err(InstanceLockError::Io(err.to_string())),
    }
}

/// The conventional lock path next to the config file.
pub fn instance_lock_path() -> Option<PathBuf> {
    config_file_path().map(|config| config.with_file_name("instance.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_pairs() {
        let pairs = parse_env_file("COMPME_MAX_WORDS=12\nCOMPME_DEBOUNCE_MS=80");
        assert_eq!(
            pairs,
            vec![
                ("COMPME_MAX_WORDS".to_string(), "12".to_string()),
                ("COMPME_DEBOUNCE_MS".to_string(), "80".to_string()),
            ]
        );
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let pairs = parse_env_file("# a comment\n\n  \nKEY=value\n# trailing");
        assert_eq!(pairs, vec![("KEY".to_string(), "value".to_string())]);
    }

    #[test]
    fn trims_whitespace_around_key_and_value() {
        let pairs = parse_env_file("  KEY  =   value with spaces  ");
        assert_eq!(
            pairs,
            vec![("KEY".to_string(), "value with spaces".to_string())]
        );
    }

    #[test]
    fn first_equals_splits_so_value_may_contain_equals() {
        let pairs = parse_env_file("MODEL=/path/to=model.gguf");
        assert_eq!(
            pairs,
            vec![("MODEL".to_string(), "/path/to=model.gguf".to_string())]
        );
    }

    #[test]
    fn parses_explicitly_escaped_multiline_values() {
        let pairs = parse_env_file(
            "COMPME_INSTRUCTIONS=__COMPME_ESCAPED__:first line\\nsecond line\\tindented\\\\tail",
        );
        assert_eq!(
            pairs,
            vec![(
                "COMPME_INSTRUCTIONS".to_string(),
                "first line\nsecond line\tindented\\tail".to_string()
            )]
        );
    }

    #[test]
    fn leaves_legacy_backslash_values_literal() {
        let pairs = parse_env_file("MODEL=C:\\new\\tmodel.gguf");
        assert_eq!(
            pairs,
            vec![("MODEL".to_string(), "C:\\new\\tmodel.gguf".to_string())]
        );
    }

    #[test]
    fn escaped_prefix_value_round_trips_without_being_stripped() {
        let value = "__COMPME_ESCAPED__:literal";
        let updated = update_env_file_contents("", "COMPME_INSTRUCTIONS", value);
        assert_eq!(
            parse_env_file(&updated),
            vec![("COMPME_INSTRUCTIONS".to_string(), value.to_string())]
        );
    }

    #[test]
    fn skips_lines_without_equals_or_empty_key() {
        let pairs = parse_env_file("no equals here\n=novalue\nGOOD=1");
        assert_eq!(pairs, vec![("GOOD".to_string(), "1".to_string())]);
    }

    #[test]
    fn update_replaces_the_value_in_place_preserving_everything_else() {
        let existing =
            "# compme config\nCOMPME_MAX_WORDS=12\n\nCOMPME_ENABLED=true\n# trailing comment\n";
        assert_eq!(
            update_env_file_contents(existing, "COMPME_ENABLED", "false"),
            "# compme config\nCOMPME_MAX_WORDS=12\n\nCOMPME_ENABLED=false\n# trailing comment\n"
        );
    }

    #[test]
    fn update_appends_a_missing_key_as_a_final_line() {
        // With and without a trailing newline on the existing contents.
        assert_eq!(update_env_file_contents("A=1\n", "B", "2"), "A=1\nB=2\n");
        assert_eq!(update_env_file_contents("A=1", "B", "2"), "A=1\nB=2\n");
        assert_eq!(update_env_file_contents("", "B", "2"), "B=2\n");
    }

    #[test]
    fn update_rewrites_every_duplicate_of_the_key() {
        // The read side is last-wins (HashMap collect), so a stale duplicate
        // left behind would silently shadow the update.
        assert_eq!(
            update_env_file_contents("K=old1\nOTHER=x\nK=old2\n", "K", "new"),
            "K=new\nOTHER=x\nK=new\n"
        );
    }

    #[test]
    fn update_escapes_multiline_values_so_they_reload_as_one_setting() {
        let updated = update_env_file_contents(
            "COMPME_INSTRUCTIONS=old\n",
            "COMPME_INSTRUCTIONS",
            "first line\nsecond line\\tail",
        );
        assert_eq!(
            updated,
            "COMPME_INSTRUCTIONS=__COMPME_ESCAPED__:first line\\nsecond line\\\\tail\n"
        );
        assert_eq!(
            parse_env_file(&updated),
            vec![(
                "COMPME_INSTRUCTIONS".to_string(),
                "first line\nsecond line\\tail".to_string()
            )]
        );
    }

    #[test]
    fn update_leaves_comments_mentioning_the_key_untouched() {
        assert_eq!(
            update_env_file_contents("# set K=1 to enable\nK=0\n", "K", "1"),
            "# set K=1 to enable\nK=1\n"
        );
    }

    #[test]
    fn update_matches_a_key_padded_with_whitespace() {
        // parse_env_file trims the key, so `  K  = v` IS key K on the read
        // side; the updater must agree or the stale padded line would win.
        assert_eq!(
            update_env_file_contents("  K  = old\n", "K", "new"),
            "K=new\n"
        );
    }

    #[test]
    fn single_instance_lock_excludes_a_second_holder_until_released() {
        let dir =
            std::env::temp_dir().join(format!("compme-instance-lock-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("instance.lock");

        let first = try_acquire_instance_lock(&path).expect("first acquire");
        // flock conflicts apply across separate open file descriptions even
        // within one process, so a second acquire must report HELD…
        assert_eq!(
            try_acquire_instance_lock(&path).unwrap_err(),
            InstanceLockError::Held,
            "second instance must be refused as Held"
        );
        // …and succeed after the first holder drops (kernel releases the
        // lock — crash-safe, unlike pid files).
        drop(first);
        assert!(try_acquire_instance_lock(&path).is_ok());
        // An IO failure must NOT masquerade as Held (misleading diagnostics):
        // a lock path whose parent chain crosses a FILE fails create_dir_all
        // on every OS (/dev/null tricks are unix-only; this runs on the
        // Windows CI gate too).
        let blocker = dir.join("blocker-file");
        std::fs::write(&blocker, b"x").expect("write blocker file");
        let bad = blocker.join("cannot-exist").join("instance.lock");
        assert!(matches!(
            try_acquire_instance_lock(&bad),
            Err(InstanceLockError::Io(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_setting_creates_updates_and_survives_reload() {
        let dir = std::env::temp_dir().join(format!("compme-persist-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("config.env");

        // Missing file + parent dirs → created with just the key.
        persist_setting(&path, "COMPME_ENABLED", "false").expect("create");
        assert_eq!(
            load_file_map(&path).get("COMPME_ENABLED"),
            Some(&"false".to_string())
        );

        // Add unrelated content, then update: both survive, value replaced.
        std::fs::write(
            &path,
            "# keep me\nCOMPME_MAX_WORDS=12\nCOMPME_ENABLED=false\n",
        )
        .expect("seed");
        persist_setting(&path, "COMPME_ENABLED", "true").expect("update");
        let contents = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(
            contents,
            "# keep me\nCOMPME_MAX_WORDS=12\nCOMPME_ENABLED=true\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_env_file_key_drops_only_the_target_line() {
        // Comments, blank lines, unknown keys, and order all survive; every line
        // bearing the key is dropped (read side is last-wins, so a leftover
        // duplicate would resurrect the value).
        let contents = "# head\nA=1\nB=2\nA=3\n\nC=4\n";
        assert_eq!(remove_env_file_key(contents, "A"), "# head\nB=2\n\nC=4\n");
        // Removing the only key yields an empty file (no lone trailing newline).
        assert_eq!(remove_env_file_key("ONLY=x\n", "ONLY"), "");
        // Absent key leaves content byte-identical (modulo final-newline norm).
        assert_eq!(remove_env_file_key("B=2\n", "A"), "B=2\n");
    }

    #[test]
    fn remove_setting_clears_key_and_no_ops_when_absent_or_missing() {
        let dir = std::env::temp_dir().join(format!("compme-remove-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.env");

        // Missing file → no-op, no file created.
        remove_setting(&path, "COMPME_EXCLUDED_DOMAINS").expect("missing is no-op");
        assert!(
            !path.exists(),
            "removing from a missing file must not create one"
        );

        // Seed two keys; removing one leaves the other untouched and drops the line.
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            &path,
            "COMPME_EXCLUDED_DOMAINS=bank.example\nCOMPME_ENABLED=true\n",
        )
        .expect("seed");
        remove_setting(&path, "COMPME_EXCLUDED_DOMAINS").expect("remove");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            "COMPME_ENABLED=true\n",
            "the emptied key must be removed, not blanked, and siblings preserved"
        );

        // Removing an absent key is a no-op that does not rewrite (content stable).
        remove_setting(&path, "COMPME_EXCLUDED_DOMAINS").expect("absent is no-op");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            "COMPME_ENABLED=true\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_normalizes_crlf_input_to_lf() {
        // `str::lines` strips the `\r` of CRLF pairs; the rewrite joins with
        // plain `\n`. Documented (not accidental): the parser treats both the
        // same, so normalizing on first persist is harmless.
        assert_eq!(
            update_env_file_contents("K=old\r\nOTHER=x\r\n", "K", "new"),
            "K=new\nOTHER=x\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn persist_setting_never_rewrites_a_config_it_could_not_read() {
        // An unreadable-but-existing config must FAIL the persist, not be
        // silently replaced by a one-line file (that would destroy every other
        // setting the user has).
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("compme-unreadable-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.env");
        std::fs::write(&path, "PRECIOUS=keep\n").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");

        let result = persist_setting(&path, "COMPME_ENABLED", "false");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("unchmod");
        assert!(
            result.is_err(),
            "an unreadable config must fail the persist"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            "PRECIOUS=keep\n",
            "the original content must survive untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn atomic_write_leaves_target_intact_on_failed_rename() {
        // Core crash-safety contract: atomic_write never truncates or partially
        // writes the live target. A pre-existing config must be BYTE-IDENTICAL
        // after a write that fails before the rename lands — the temp file is the
        // only thing that can be left behind, never a torn target. We force the
        // failure by making the parent directory read-only so the scratch-file
        // write cannot happen; the target is opened/renamed only AFTER that, so a
        // regression that wrote in place (the non-atomic path this guards against)
        // would corrupt PRECIOUS here.
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("compme-atomic-intact-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.env");
        let original = "PRECIOUS=keep\nCOMPME_MAX_WORDS=12\n";
        std::fs::write(&path, original).expect("seed target");

        // Read-only parent: create_dir_all on the existing dir is a no-op, but the
        // scratch write (and thus the rename) cannot proceed.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("chmod ro");

        let result = atomic_write(&path, "ONLY=one\n");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod rw");
        assert!(result.is_err(), "a write that cannot complete must error");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            original,
            "the pre-existing target must survive byte-identical — no truncation/partial write"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn atomic_write_creates_owner_only_dir_and_file() {
        // Fresh-install hardening: the first write must leave the app-support
        // dir 0700 and config.env 0600, matching memory::open's convention. A
        // pre-existing dir keeps its permissions (pinned directly by
        // atomic_write_preserves_perms_of_a_pre_existing_dir below).
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("compme-atomic-perms-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.env");

        atomic_write(&path, "K=v\n").expect("write");

        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700, "created dir is owner-only");
        assert_eq!(mode(&path), 0o600, "config file is owner-only");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn atomic_write_preserves_perms_of_a_pre_existing_dir() {
        // A successful write into a dir that ALREADY exists must not re-chmod it —
        // the `created_dir` guard only tightens perms on a dir atomic_write itself
        // created. Without the guard, every settings write would clobber a user's
        // relaxed (e.g. 0755) config-dir permissions back to 0700.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("compme-preexist-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        atomic_write(&dir.join("config.env"), "K=v\n").expect("write");

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "pre-existing dir perms must be preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_rejects_a_parentless_path() {
        // An empty path has no parent component (`Path::parent` → None; a
        // single-component relative path like "config" yields Some("") instead),
        // so it must error InvalidInput rather than write to the cwd. Pins the
        // no-parent guard atomic_write shares with persist_setting/remove_setting.
        let err = atomic_write(Path::new(""), "K=v\n").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn temp_name_derives_from_target_so_stats_and_config_never_share_a_scratch() {
        // The scratch file is `.{target_file_name}.tmp.{pid}`, derived from the
        // TARGET — so config.env and stats.env in one dir get DIFFERENT scratch
        // paths and a write to one can never collide with the other's temp. We
        // observe the scratch name directly: pointing atomic_write at a target
        // that is itself a directory makes the final rename fail (file-over-dir),
        // stranding the scratch so its name is inspectable. A shared/fixed temp
        // name (the pre-fix behavior) would strand `.config.tmp.*` for both.
        let dir = std::env::temp_dir().join(format!("cm-cfg-temp-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        // Make each target a directory so the rename-over fails and the scratch
        // is left behind for inspection.
        std::fs::create_dir(dir.join("stats.env")).expect("stats.env dir");

        assert!(
            atomic_write(&dir.join("stats.env"), "STATS=v\n").is_err(),
            "rename over a directory must fail, stranding the scratch"
        );

        let scratch: Vec<String> = std::fs::read_dir(&dir)
            .expect("dir readable")
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.') && n.contains(".tmp."))
            .collect();
        assert_eq!(scratch.len(), 1, "exactly one scratch left");
        assert!(
            scratch[0].starts_with(".stats.env.tmp."),
            "scratch must derive from the target name `stats.env`, got {:?}",
            scratch[0]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clamped_uses_default_when_absent() {
        assert_eq!(parse_clamped::<u64>(None, 120, 0, 5000), 120);
    }

    #[test]
    fn clamped_uses_default_when_unparseable() {
        assert_eq!(parse_clamped::<u64>(Some("abc".into()), 120, 0, 5000), 120);
    }

    #[test]
    fn clamped_parses_valid_value() {
        assert_eq!(parse_clamped::<usize>(Some("12".into()), 8, 1, 50), 12);
    }

    #[test]
    fn clamped_enforces_bounds() {
        assert_eq!(parse_clamped::<usize>(Some("999".into()), 8, 1, 50), 50);
        assert_eq!(parse_clamped::<usize>(Some("0".into()), 8, 1, 50), 1);
    }

    #[test]
    fn load_file_map_reads_and_parses_a_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("compme-test-config-{}.env", std::process::id()));
        std::fs::write(
            &path,
            "# comment\nCOMPME_MAX_WORDS=15\nCOMPME_PROMPT_MODE=raw\n",
        )
        .unwrap();
        let map = load_file_map(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(map.get("COMPME_MAX_WORDS").map(String::as_str), Some("15"));
        assert_eq!(
            map.get("COMPME_PROMPT_MODE").map(String::as_str),
            Some("raw")
        );
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn load_file_map_missing_file_is_empty() {
        let map = load_file_map(Path::new("/no/such/compme/config.env"));
        assert!(map.is_empty());
    }

    #[test]
    fn config_path_macos_uses_application_support() {
        let lookup = |key: &str| match key {
            "HOME" => Some("/Users/u".to_string()),
            _ => None,
        };
        assert_eq!(
            config_file_path_for(&lookup, "macos").unwrap(),
            PathBuf::from("/Users/u/Library/Application Support/compme/config.env")
        );
    }

    #[test]
    fn config_path_linux_prefers_xdg_config_home() {
        let lookup = |key: &str| match key {
            "XDG_CONFIG_HOME" => Some("/home/u/.cfg".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(
            config_file_path_for(&lookup, "linux").unwrap(),
            PathBuf::from("/home/u/.cfg/compme/config.env")
        );
    }

    #[test]
    fn config_path_linux_falls_back_to_dot_config() {
        let lookup = |key: &str| match key {
            "XDG_CONFIG_HOME" => Some(String::new()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(
            config_file_path_for(&lookup, "linux").unwrap(),
            PathBuf::from("/home/u/.config/compme/config.env")
        );
    }

    #[test]
    fn config_path_windows_uses_appdata() {
        let lookup = |key: &str| match key {
            "APPDATA" => Some(r"C:\Users\u\AppData\Roaming".to_string()),
            _ => None,
        };
        assert_eq!(
            config_file_path_for(&lookup, "windows").unwrap(),
            PathBuf::from(r"C:\Users\u\AppData\Roaming")
                .join("compme")
                .join("config.env")
        );
    }

    #[test]
    fn config_override_wins_on_every_os() {
        let lookup = |key: &str| match key {
            "COMPME_CONFIG" => Some("/tmp/override.env".to_string()),
            _ => None,
        };
        for os in ["macos", "linux", "windows"] {
            assert_eq!(
                config_file_path_for(&lookup, os).unwrap(),
                PathBuf::from("/tmp/override.env")
            );
        }
    }

    #[test]
    fn config_path_none_without_required_home() {
        let lookup = |_: &str| None;
        for os in ["macos", "linux", "windows"] {
            assert!(config_file_path_for(&lookup, os).is_none());
        }
    }

    #[test]
    fn config_path_none_when_home_env_is_empty() {
        // An empty HOME/APPDATA must be treated as unset, not as a relative ""
        // base that writes config under the process cwd.
        let empty = |key: &str| match key {
            "HOME" | "APPDATA" => Some(String::new()),
            _ => None,
        };
        for os in ["macos", "linux", "windows"] {
            assert!(config_file_path_for(&empty, os).is_none(), "os={os}");
        }
        // Empty XDG_CONFIG_HOME must fall back to (also-empty) HOME → None.
        let empty_xdg = |key: &str| match key {
            "XDG_CONFIG_HOME" | "HOME" => Some(String::new()),
            _ => None,
        };
        assert!(config_file_path_for(&empty_xdg, "linux").is_none());
    }

    #[test]
    fn config_file_path_covers_all_branches() {
        // Drives the lookup-injected core — mutating the process env here
        // (`set_var`/`remove_var`) raced parallel tests and is `unsafe`
        // under edition 2024.
        let env = |pairs: &'static [(&str, &str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            }
        };

        // Branch 1: COMPME_CONFIG set non-empty -> returned verbatim.
        assert_eq!(
            config_file_path_from(&env(&[("COMPME_CONFIG", "/some/path"), ("HOME", "/h")])),
            Some(PathBuf::from("/some/path"))
        );

        // Branch 2: COMPME_CONFIG empty + per-OS home var set -> path under
        // it on EVERY host OS (this test runs on the Windows/Linux CI gates
        // too; the per-OS branch specifics are pinned by the
        // config_file_path_for tests below).
        let path = config_file_path_from(&env(&[
            ("COMPME_CONFIG", ""),
            ("HOME", "/h"),
            ("APPDATA", "/h"),
        ]))
        .expect("home branch should yield a path");
        assert!(
            path.ends_with("compme/config.env"),
            "unexpected path: {path:?}"
        );
        assert!(
            path.starts_with("/h"),
            "path should be under HOME: {path:?}"
        );

        // Branch 3: neither var available -> None.
        assert_eq!(config_file_path_from(&env(&[])), None);
    }
}
