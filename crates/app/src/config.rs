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
/// clamp into `[min, max]`. Invalid config never fails startup.
pub fn parse_clamped<T>(raw: Option<String>, default: T, min: T, max: T) -> T
where
    T: FromStr + Ord + Copy,
{
    let value = raw
        .and_then(|s| s.trim().parse::<T>().ok())
        .unwrap_or(default);
    value.clamp(min, max)
}

/// Resolve the config file path: `COMPLETE_ME_CONFIG` override, else
/// `$HOME/Library/Application Support/complete-me/config.env`. `None` if neither
/// is available.
pub fn config_file_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("COMPLETE_ME_CONFIG") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    std::env::var("HOME").ok().map(|home| {
        PathBuf::from(home)
            .join("Library/Application Support/complete-me")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_pairs() {
        let pairs = parse_env_file("COMPLETE_ME_MAX_WORDS=12\nCOMPLETE_ME_DEBOUNCE_MS=80");
        assert_eq!(
            pairs,
            vec![
                ("COMPLETE_ME_MAX_WORDS".to_string(), "12".to_string()),
                ("COMPLETE_ME_DEBOUNCE_MS".to_string(), "80".to_string()),
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
        let path = dir.join(format!(
            "complete-me-test-config-{}.env",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "# comment\nCOMPLETE_ME_MAX_WORDS=15\nCOMPLETE_ME_PROMPT_MODE=raw\n",
        )
        .unwrap();
        let map = load_file_map(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            map.get("COMPLETE_ME_MAX_WORDS").map(String::as_str),
            Some("15")
        );
        assert_eq!(
            map.get("COMPLETE_ME_PROMPT_MODE").map(String::as_str),
            Some("raw")
        );
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn load_file_map_missing_file_is_empty() {
        let map = load_file_map(Path::new("/no/such/complete-me/config.env"));
        assert!(map.is_empty());
    }

    #[test]
    fn config_file_path_covers_all_branches() {
        // Save originals so the test is hermetic and leaves the process env
        // exactly as it found it.
        let saved_config = std::env::var("COMPLETE_ME_CONFIG").ok();
        let saved_home = std::env::var("HOME").ok();

        // Branch 1: COMPLETE_ME_CONFIG set non-empty -> returned verbatim.
        std::env::set_var("COMPLETE_ME_CONFIG", "/some/path");
        assert_eq!(config_file_path(), Some(PathBuf::from("/some/path")));

        // Branch 2: COMPLETE_ME_CONFIG empty + HOME set -> path under $HOME.
        std::env::set_var("COMPLETE_ME_CONFIG", "");
        std::env::set_var("HOME", "/h");
        let path = config_file_path().expect("HOME branch should yield a path");
        assert!(
            path.ends_with("complete-me/config.env"),
            "unexpected path: {path:?}"
        );
        assert!(
            path.starts_with("/h"),
            "path should be under HOME: {path:?}"
        );

        // Branch 3: neither var available -> None.
        std::env::remove_var("COMPLETE_ME_CONFIG");
        std::env::remove_var("HOME");
        assert_eq!(config_file_path(), None);

        // Restore originals.
        match saved_config {
            Some(v) => std::env::set_var("COMPLETE_ME_CONFIG", v),
            None => std::env::remove_var("COMPLETE_ME_CONFIG"),
        }
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
