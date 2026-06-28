//! User-editable configuration file, layered under environment variables.
//!
//! The file is dotenv-style `KEY=VALUE`. Its parsed contents are merged into the
//! same lookup the environment uses, so there is one config code path
//! (`run_loop::Config::from_lookup`): env wins over file wins over default.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

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
        pairs.push((key.to_string(), value.trim().to_string()));
    }
    pairs
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

/// Resolve the config file path: `COMPME_CONFIG` override, else
/// `$HOME/Library/Application Support/compme/config.env`. `None` if neither
/// is available.
pub fn config_file_path() -> Option<PathBuf> {
    config_file_path_from(&|key| std::env::var(key).ok())
}

/// Lookup-injected core of [`config_file_path`] (the `Config::from_lookup`
/// pattern): testable without mutating the process environment — `set_var`
/// races parallel tests and is `unsafe` under edition 2024.
fn config_file_path_from(lookup: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(path) = lookup("COMPME_CONFIG") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    lookup("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library/Application Support/compme")
            .join("config.env")
    })
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
    let mut lines: Vec<String> = contents
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if !trimmed.starts_with('#') {
                if let Some((k, _)) = trimmed.split_once('=') {
                    if k.trim() == key {
                        found = true;
                        return format!("{key}={value}");
                    }
                }
            }
            line.to_string()
        })
        .collect();
    if !found {
        lines.push(format!("{key}={value}"));
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
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(
        ".{}.tmp.{}",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("config"),
        std::process::id()
    ));
    std::fs::write(&temp, contents)?;
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

/// Holds the single-instance flock for the process lifetime; the kernel
/// releases it on ANY exit (crash included) — the reason this is flock and
/// not a pid file. Dropping releases explicitly.
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

/// Try to become THE compme instance: `flock(LOCK_EX | LOCK_NB)` on `path`
/// (parent dirs created). Launch-method-agnostic: a LaunchServices-spawned
/// copy and a direct-exec'd binary contend on the same file (the c92 finding
/// — two instances would double AX observers and hotkey registrations).
///
/// On [`InstanceLockError::Io`] the app fails closed before installing AX
/// observers or hotkeys. Running unguarded can double-observe private context
/// and double-insert completions.
pub fn try_acquire_instance_lock(path: &Path) -> Result<InstanceLock, InstanceLockError> {
    use std::os::unix::io::AsRawFd;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| InstanceLockError::Io(e.to_string()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| InstanceLockError::Io(e.to_string()))?;
    // SAFETY: flock on an owned, open fd; no memory contracts involved.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(InstanceLock { _file: file })
    } else {
        Err(instance_lock_error_from(std::io::Error::last_os_error()))
    }
}

fn instance_lock_error_from(err: std::io::Error) -> InstanceLockError {
    if err.kind() == std::io::ErrorKind::WouldBlock
        || err.raw_os_error() == Some(libc::EWOULDBLOCK)
        || err.raw_os_error() == Some(libc::EAGAIN)
    {
        InstanceLockError::Held
    } else {
        InstanceLockError::Io(err.to_string())
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
        // a lock path whose parent is an unwritable pseudo-dir errors as Io.
        let bad = Path::new("/dev/null/cannot/exist/instance.lock");
        assert!(matches!(
            try_acquire_instance_lock(bad),
            Err(InstanceLockError::Io(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn instance_lock_error_classifies_only_wouldblock_as_held() {
        assert_eq!(
            instance_lock_error_from(std::io::Error::from_raw_os_error(libc::EWOULDBLOCK)),
            InstanceLockError::Held
        );
        // The kind-based WouldBlock variant (no raw os errno) is also Held.
        assert_eq!(
            instance_lock_error_from(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
            InstanceLockError::Held
        );
        // EAGAIN aliases EWOULDBLOCK on Linux but is a distinct errno on some
        // platforms; it must also classify as Held.
        assert_eq!(
            instance_lock_error_from(std::io::Error::from_raw_os_error(libc::EAGAIN)),
            InstanceLockError::Held
        );
        assert!(matches!(
            instance_lock_error_from(std::io::Error::from_raw_os_error(libc::EACCES)),
            InstanceLockError::Io(_)
        ));
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

        // Branch 2: COMPME_CONFIG empty + HOME set -> path under $HOME.
        let path = config_file_path_from(&env(&[("COMPME_CONFIG", ""), ("HOME", "/h")]))
            .expect("HOME branch should yield a path");
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
