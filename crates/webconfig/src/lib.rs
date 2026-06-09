//! Web-driven config deep-link parsing (design spec §8 / §16).
//!
//! Cotypist pushes compatibility fixes through `cotypist.app/{setPreference,
//! launchCotypist/setOverride}` URL-scheme deep links. We re-implement the
//! **safe, reversible, user-visible** subset: a `complete-me://setOverride` link
//! that enables/disables or excludes/includes completions for one app **or** one
//! domain.
//!
//! Security posture: **strict and fail-closed.** A deep link is an untrusted
//! input that any web page or app can fire, so the parser:
//! - accepts only the `complete-me` scheme and the `setOverride` command;
//! - accepts exactly one scope (`app` XOR `domain`) and exactly one action
//!   (`enabled` XOR `excluded`), both with sane values;
//! - rejects unknown commands, unknown query parameters, empty/oversized/
//!   malformed scopes, and any percent-encoding (the safe subset needs none) —
//!   anything outside the allow-list is an error, never silently ignored.
//!
//! It deliberately CANNOT set custom instructions, model paths, security
//! settings, or anything non-reversible — those would need cryptographic signing
//! and an explicit trust origin (an A3 item).
//!
//! **Reversibility is NOT a full substitute for signing.** Because any page can
//! fire a deep link, an unsigned link can still nuisance-toggle a user's apps
//! (clickjacking / DoS by rapid toggling). Two host-layer requirements are
//! therefore mandatory, not optional: (1) the host MUST surface every applied
//! command to the user (the §16 "user-visible" requirement) and SHOULD allow
//! undo; (2) any future non-reversible command (custom instructions, model
//! override, security settings) MUST require a signature + trusted origin before
//! it is added here (A3). The §16 web-config gate stays *partial* until signing
//! lands.

/// What a parsed, validated deep link asks us to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverrideCommand {
    pub scope: Scope,
    pub action: OverrideAction,
}

/// Which target the override applies to. Exactly one per command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    App(String),
    Domain(String),
}

/// The reversible per-scope action. Exactly one per command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverrideAction {
    /// Enable completions for the scope (`enabled=true`).
    Enable,
    /// Disable completions for the scope (`enabled=false`).
    Disable,
    /// Add the scope to the exclude list (`excluded=true`).
    Exclude,
    /// Remove the scope from the exclude list (`excluded=false`).
    Include,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Not a `complete-me://` URL.
    NotOurScheme,
    /// Host/command is not a supported one (only `setOverride`).
    UnknownCommand(String),
    /// A query parameter outside the allow-list was present (fail-closed).
    UnknownParam(String),
    /// A query parameter (carried name) appeared more than once.
    DuplicateParam(String),
    /// A query token had no `=` separator.
    MalformedParam(String),
    /// Neither `app` nor `domain` was supplied.
    MissingScope,
    /// Both `app` and `domain` were supplied (must be exactly one).
    AmbiguousScope,
    /// The scope value was empty, too long, or contained illegal characters.
    InvalidScope,
    /// Neither `enabled` nor `excluded` was supplied.
    MissingAction,
    /// Both action params were supplied (must be exactly one).
    AmbiguousAction,
    /// An action value was not a boolean.
    InvalidValue(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NotOurScheme => write!(f, "not a complete-me:// URL"),
            ParseError::UnknownCommand(cmd) => write!(f, "unknown command: {cmd}"),
            ParseError::UnknownParam(name) => write!(f, "unknown query parameter: {name}"),
            ParseError::DuplicateParam(name) => write!(f, "duplicate query parameter: {name}"),
            ParseError::MalformedParam(token) => {
                write!(f, "malformed query parameter (no `=`): {token}")
            }
            ParseError::MissingScope => write!(f, "missing scope (need `app` or `domain`)"),
            ParseError::AmbiguousScope => {
                write!(f, "ambiguous scope (only one of `app` or `domain` allowed)")
            }
            ParseError::InvalidScope => {
                write!(f, "invalid scope (empty, too long, or illegal characters)")
            }
            ParseError::MissingAction => {
                write!(f, "missing action (need `enabled` or `excluded`)")
            }
            ParseError::AmbiguousAction => {
                write!(
                    f,
                    "ambiguous action (only one of `enabled` or `excluded` allowed)"
                )
            }
            ParseError::InvalidValue(value) => write!(f, "invalid boolean value: {value}"),
        }
    }
}

impl std::error::Error for ParseError {}

const SCHEME: &str = "complete-me://";
/// Bundle ids / domains are short; cap to reject absurd inputs.
const MAX_SCOPE_LEN: usize = 253;

/// Parse and validate a `complete-me://setOverride?...` deep link. Returns the
/// reversible command, or a specific [`ParseError`] — never a partial/guessed
/// result.
pub fn parse_deep_link(url: &str) -> Result<OverrideCommand, ParseError> {
    let rest = url.strip_prefix(SCHEME).ok_or(ParseError::NotOurScheme)?;

    // Split `command?query` (an absent `?` means no params at all).
    let (command, query) = match rest.split_once('?') {
        Some((c, q)) => (c, q),
        None => (rest, ""),
    };
    // Tolerate a trailing slash on the command host (`setOverride/?...`).
    let command = command.trim_end_matches('/');
    if command != "setOverride" {
        return Err(ParseError::UnknownCommand(command.to_string()));
    }

    let mut app: Option<String> = None;
    let mut domain: Option<String> = None;
    let mut enabled: Option<bool> = None;
    let mut excluded: Option<bool> = None;

    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| ParseError::MalformedParam(pair.to_string()))?;
        match key {
            "app" => set_once(&mut app, key, value.to_string())?,
            "domain" => set_once(&mut domain, key, value.to_string())?,
            "enabled" => set_once(&mut enabled, key, parse_bool(value)?)?,
            "excluded" => set_once(&mut excluded, key, parse_bool(value)?)?,
            other => return Err(ParseError::UnknownParam(other.to_string())),
        }
    }

    let scope = match (app, domain) {
        (Some(_), Some(_)) => return Err(ParseError::AmbiguousScope),
        (None, None) => return Err(ParseError::MissingScope),
        (Some(a), None) => Scope::App(valid_scope(a)?),
        (None, Some(d)) => Scope::Domain(valid_scope(d)?),
    };

    let action = match (enabled, excluded) {
        (Some(_), Some(_)) => return Err(ParseError::AmbiguousAction),
        (None, None) => return Err(ParseError::MissingAction),
        (Some(true), None) => OverrideAction::Enable,
        (Some(false), None) => OverrideAction::Disable,
        (None, Some(true)) => OverrideAction::Exclude,
        (None, Some(false)) => OverrideAction::Include,
    };

    Ok(OverrideCommand { scope, action })
}

/// Set an option exactly once; a second assignment for the same `key` is a
/// duplicate-param error carrying that key (so the host can name it).
fn set_once<T>(slot: &mut Option<T>, key: &str, value: T) -> Result<(), ParseError> {
    if slot.is_some() {
        return Err(ParseError::DuplicateParam(key.to_string()));
    }
    *slot = Some(value);
    Ok(())
}

/// Strictly `true` or `false` — no numeric or case variants. Anything else is an
/// error (fail-closed; a signed broader command set is an A3 item).
fn parse_bool(value: &str) -> Result<bool, ParseError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(ParseError::InvalidValue(other.to_string())),
    }
}

/// A bundle id or domain: non-empty, bounded, and limited to the unreserved
/// characters real identifiers use. Rejects spaces, percent-encoding, path or
/// query metacharacters — so an attacker can't smuggle a second command or a
/// control sequence through the scope.
fn valid_scope(s: String) -> Result<String, ParseError> {
    // Charset first: only ASCII unreserved identifier characters. This both
    // blocks smuggling and guarantees one byte per character, so the length cap
    // below is unambiguously a character count.
    if s.is_empty()
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(ParseError::InvalidScope);
    }
    // Charset above guarantees ASCII, so byte length == character count.
    if s.len() > MAX_SCOPE_LEN {
        return Err(ParseError::InvalidScope);
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_app_enable() {
        assert_eq!(
            parse_deep_link("complete-me://setOverride?app=com.apple.TextEdit&enabled=true"),
            Ok(OverrideCommand {
                scope: Scope::App("com.apple.TextEdit".into()),
                action: OverrideAction::Enable,
            })
        );
    }

    #[test]
    fn parses_each_action() {
        let cases = [
            ("enabled=true", OverrideAction::Enable),
            ("enabled=false", OverrideAction::Disable),
            ("excluded=true", OverrideAction::Exclude),
            ("excluded=false", OverrideAction::Include),
        ];
        for (param, expected) in cases {
            let url = format!("complete-me://setOverride?app=com.foo.bar&{param}");
            assert_eq!(parse_deep_link(&url).unwrap().action, expected);
        }
    }

    #[test]
    fn parses_domain_scope() {
        assert_eq!(
            parse_deep_link("complete-me://setOverride?domain=docs.google.com&excluded=true"),
            Ok(OverrideCommand {
                scope: Scope::Domain("docs.google.com".into()),
                action: OverrideAction::Exclude,
            })
        );
    }

    #[test]
    fn param_order_is_irrelevant() {
        assert_eq!(
            parse_deep_link("complete-me://setOverride?enabled=false&app=com.foo.bar"),
            Ok(OverrideCommand {
                scope: Scope::App("com.foo.bar".into()),
                action: OverrideAction::Disable,
            })
        );
    }

    #[test]
    fn trailing_slash_on_command_is_tolerated() {
        assert!(parse_deep_link("complete-me://setOverride/?app=com.foo.bar&enabled=true").is_ok());
    }

    #[test]
    fn wrong_scheme_is_rejected() {
        assert_eq!(
            parse_deep_link("cotypist://setOverride?app=x&enabled=true"),
            Err(ParseError::NotOurScheme)
        );
        assert_eq!(
            parse_deep_link("https://complete-me/setOverride"),
            Err(ParseError::NotOurScheme)
        );
    }

    #[test]
    fn unknown_command_is_rejected() {
        // setPreference is intentionally NOT in the safe subset yet (needs signing).
        match parse_deep_link("complete-me://setPreference?app=x&enabled=true") {
            Err(ParseError::UnknownCommand(c)) => assert_eq!(c, "setPreference"),
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn unknown_param_fails_closed() {
        // A smuggled instruction/model param must be rejected, not ignored.
        match parse_deep_link("complete-me://setOverride?app=x&enabled=true&instructions=be+evil") {
            Err(ParseError::UnknownParam(p)) => assert_eq!(p, "instructions"),
            other => panic!("expected UnknownParam, got {other:?}"),
        }
    }

    #[test]
    fn missing_and_ambiguous_scope_are_errors() {
        assert_eq!(
            parse_deep_link("complete-me://setOverride?enabled=true"),
            Err(ParseError::MissingScope)
        );
        assert_eq!(
            parse_deep_link("complete-me://setOverride?app=x&domain=y&enabled=true"),
            Err(ParseError::AmbiguousScope)
        );
    }

    #[test]
    fn missing_and_ambiguous_action_are_errors() {
        assert_eq!(
            parse_deep_link("complete-me://setOverride?app=x"),
            Err(ParseError::MissingAction)
        );
        assert_eq!(
            parse_deep_link("complete-me://setOverride?app=x&enabled=true&excluded=true"),
            Err(ParseError::AmbiguousAction)
        );
    }

    #[test]
    fn invalid_action_value_is_rejected() {
        match parse_deep_link("complete-me://setOverride?app=x&enabled=maybe") {
            Err(ParseError::InvalidValue(v)) => assert_eq!(v, "maybe"),
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn malformed_scope_is_rejected() {
        // Empty, illegal chars, percent-encoding, and oversized all fail closed.
        for bad in [
            "complete-me://setOverride?app=&enabled=true",
            "complete-me://setOverride?app=com foo&enabled=true",
            "complete-me://setOverride?app=a%2Fb&enabled=true",
            "complete-me://setOverride?app=a/b&enabled=true",
        ] {
            assert_eq!(parse_deep_link(bad), Err(ParseError::InvalidScope), "{bad}");
        }
        let huge = "x".repeat(MAX_SCOPE_LEN + 1);
        assert_eq!(
            parse_deep_link(&format!(
                "complete-me://setOverride?app={huge}&enabled=true"
            )),
            Err(ParseError::InvalidScope)
        );
    }

    #[test]
    fn duplicate_param_is_rejected_with_its_name() {
        assert_eq!(
            parse_deep_link("complete-me://setOverride?app=x&app=y&enabled=true"),
            Err(ParseError::DuplicateParam("app".into()))
        );
    }

    #[test]
    fn param_without_equals_is_rejected() {
        assert_eq!(
            parse_deep_link("complete-me://setOverride?app=x&enabled=true&flag"),
            Err(ParseError::MalformedParam("flag".into()))
        );
    }

    #[test]
    fn command_and_keys_are_case_sensitive() {
        // Lowercase command and capitalized keys fail closed (no aliasing).
        assert_eq!(
            parse_deep_link("complete-me://setoverride?app=x&enabled=true"),
            Err(ParseError::UnknownCommand("setoverride".into()))
        );
        assert_eq!(
            parse_deep_link("complete-me://setOverride?App=x&enabled=true"),
            Err(ParseError::UnknownParam("App".into()))
        );
    }

    #[test]
    fn boolean_values_are_strict_true_false_only() {
        for bad in [
            "enabled=1",
            "enabled=0",
            "enabled=True",
            "enabled=2",
            "enabled=",
        ] {
            let url = format!("complete-me://setOverride?app=x&{bad}");
            assert!(
                matches!(parse_deep_link(&url), Err(ParseError::InvalidValue(_))),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn empty_query_and_bare_command_and_leading_amp_are_handled() {
        // Empty query after `?`, no `?` at all → MissingScope (no panic).
        assert_eq!(
            parse_deep_link("complete-me://setOverride?"),
            Err(ParseError::MissingScope)
        );
        assert_eq!(
            parse_deep_link("complete-me://setOverride"),
            Err(ParseError::MissingScope)
        );
        // A leading empty `&`-pair is ignored, not treated as a malformed param.
        assert!(parse_deep_link("complete-me://setOverride?&app=x&enabled=true").is_ok());
    }

    #[test]
    fn scope_charset_and_length_boundary() {
        // Dot, dash, underscore all accepted.
        assert!(
            parse_deep_link("complete-me://setOverride?app=com.foo_bar-baz9&enabled=true").is_ok()
        );
        // Exactly MAX_SCOPE_LEN passes; +1 fails (covered elsewhere).
        let max = "a".repeat(MAX_SCOPE_LEN);
        assert!(
            parse_deep_link(&format!("complete-me://setOverride?app={max}&enabled=true")).is_ok()
        );
    }
}
