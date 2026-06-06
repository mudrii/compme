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
}
