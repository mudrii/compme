//! Web-driven config deep-link parsing (design spec §8 / §16).
//!
//! Cotypist pushes compatibility fixes through `cotypist.app/{setPreference,
//! launchCotypist/setOverride}` URL-scheme deep links. We re-implement the
//! **safe, reversible, user-visible** subset: a `compme://setOverride` link
//! that enables/disables or excludes/includes completions for one app **or** one
//! domain.
//!
//! Security posture: **strict and fail-closed.** A deep link is an untrusted
//! input that any web page or app can fire, so the parser:
//! - accepts only the `compme` scheme and the `setOverride` command;
//! - accepts exactly one scope (`app` XOR `domain`) and exactly one action
//!   (`enabled` XOR `excluded`), both with sane values;
//! - rejects unknown commands, unknown query parameters, empty/oversized/
//!   malformed scopes, and any percent-encoding (the safe subset needs none) —
//!   anything outside the allow-list is an error, never silently ignored.
//!
//! It deliberately CANNOT set custom instructions, model paths, security
//! settings, or anything non-reversible — those require [`LinkTrust::Signed`].
//!
//! **Signing (A3):** [`parse_deep_link_with_trust`] verifies a trailing
//! `&sig=<128 hex>` Ed25519 signature over the exact URL bytes preceding
//! `&sig=` against a host-pinned [`TrustedKey`]. No canonicalization: the
//! signed payload is the byte prefix, so the signature must be the final
//! parameter and anything after it fails closed. With no trusted key
//! configured, signed links are rejected (fail-closed default-off).
//!
//! **Reversibility is NOT a full substitute for signing.** Because any page can
//! fire a deep link, an unsigned link can still nuisance-toggle a user's apps
//! (clickjacking / DoS by rapid toggling). Two host-layer requirements are
//! therefore mandatory, not optional: (1) the host MUST surface every applied
//! command to the user (the §16 "user-visible" requirement) and SHOULD allow
//! undo; (2) any future non-reversible command (custom instructions, model
//! override, security settings) MUST be gated on [`LinkTrust::Signed`] when it
//! is added here. The §16 web-config gate stays *partial* until the URL-scheme
//! event reception (FFI) and the host confirmation prompt land.

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
    /// Not a `compme://` URL.
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
    /// The `sig` value was not 128 hex chars (a 64-byte Ed25519 signature).
    MalformedSignature,
    /// `sig` was present but not the final query parameter (the signed payload
    /// must be the exact byte prefix, so the signature must come last).
    MisplacedSignature,
    /// A signed link arrived but no trusted key is configured (fail-closed).
    UntrustedSignature,
    /// The signature did not verify against the trusted key for this payload.
    InvalidSignature,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NotOurScheme => write!(f, "not a compme:// URL"),
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
            ParseError::MalformedSignature => {
                write!(f, "malformed signature (need 128 hex chars)")
            }
            ParseError::MisplacedSignature => {
                write!(f, "misplaced signature (`sig` must be the final parameter)")
            }
            ParseError::UntrustedSignature => {
                write!(f, "signed link but no trusted key configured")
            }
            ParseError::InvalidSignature => write!(f, "signature verification failed"),
        }
    }
}

impl std::error::Error for ParseError {}

const SCHEME: &str = "compme://";
/// Bundle ids / domains are short; cap to reject absurd inputs.
const MAX_SCOPE_LEN: usize = 253;

/// Parse and validate a `compme://setOverride?...` deep link. Returns the
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

/// An Ed25519 public key the host trusts to sign deep links (A3 §16 signing).
/// The host pins exactly one; links signed by anything else fail verification.
pub struct TrustedKey(ed25519_dalek::VerifyingKey);

impl TrustedKey {
    /// Decode a 64-hex-char (32-byte) Ed25519 public key. `None` on any
    /// malformation or a cryptographically invalid point — the host then has
    /// no trusted key and signed links are rejected (fail-closed).
    pub fn from_hex(raw: &str) -> Option<Self> {
        let bytes: [u8; 32] = parse_hex(raw.trim())?.try_into().ok()?;
        ed25519_dalek::VerifyingKey::from_bytes(&bytes)
            .ok()
            .map(Self)
    }
}

/// Whether a parsed link carried a verified signature. Today both levels can
/// only express the reversible [`OverrideCommand`] subset; any future
/// non-reversible command MUST require [`LinkTrust::Signed`] at the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkTrust {
    Unsigned,
    Signed,
}

/// Like [`parse_deep_link`], but signature-aware: a trailing
/// `&sig=<128 hex>` parameter is split off and verified (Ed25519, over the
/// exact URL bytes preceding `&sig=`) against the host's trusted key before
/// the payload is parsed. Unsigned links still parse (the reversible subset
/// needs no signature) and are labeled [`LinkTrust::Unsigned`].
pub fn parse_deep_link_with_trust(
    url: &str,
    trusted: Option<&TrustedKey>,
) -> Result<(OverrideCommand, LinkTrust), ParseError> {
    match split_trailing_signature(url)? {
        None => parse_deep_link(url).map(|command| (command, LinkTrust::Unsigned)),
        Some((payload, signature)) => {
            let key = trusted.ok_or(ParseError::UntrustedSignature)?;
            key.0
                .verify_strict(payload.as_bytes(), &signature)
                .map_err(|_| ParseError::InvalidSignature)?;
            parse_deep_link(payload).map(|command| (command, LinkTrust::Signed))
        }
    }
}

/// Split a trailing `&sig=<hex>` off the URL. The signature MUST be the final
/// parameter (the signed payload is the exact byte prefix before `&sig=`, so
/// anything after the value would be unsigned, attacker-appendable bytes).
/// A first `&sig=` followed by more parameters — including a second `sig` —
/// therefore fails closed as misplaced.
fn split_trailing_signature(
    url: &str,
) -> Result<Option<(&str, ed25519_dalek::Signature)>, ParseError> {
    let Some(index) = url.find("&sig=") else {
        return Ok(None);
    };
    let payload = &url[..index];
    let value = &url[index + "&sig=".len()..];
    if value.contains('&') {
        return Err(ParseError::MisplacedSignature);
    }
    let bytes: [u8; 64] = parse_hex(value)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(ParseError::MalformedSignature)?;
    Ok(Some((
        payload,
        ed25519_dalek::Signature::from_bytes(&bytes),
    )))
}

/// Whether a parsed deep link may apply silently or must be confirmed by the
/// user first (the §16 mandatory host confirmation). Pure — the host's UI
/// layer renders the prompt; tests inject the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptDecision {
    /// Show a confirmation prompt before applying; the fields are
    /// human-readable pieces for the prompt text.
    PromptRequired {
        scope: String,
        action: String,
        trust: String,
    },
    /// Reserved for future signed non-reversible commands; unreachable today
    /// (every current command is reversible and prompts).
    ApplySilently,
}

/// Decide the confirmation requirement for a parsed link. Today EVERY link
/// prompts — reversible-but-unsigned links can still nuisance-toggle (module
/// docs), and no silent-eligible command class exists yet.
pub fn prompt_decision_for_link(command: &OverrideCommand, trust: LinkTrust) -> PromptDecision {
    let scope = match &command.scope {
        Scope::App(app) => app.clone(),
        Scope::Domain(domain) => domain.clone(),
    };
    PromptDecision::PromptRequired {
        scope,
        action: format!("{:?}", command.action),
        trust: match trust {
            LinkTrust::Unsigned => "unsigned link".to_string(),
            LinkTrust::Signed => "signed link, verified".to_string(),
        },
    }
}

/// Decode a hex string into bytes; `None` on odd length or a non-hex digit.
fn parse_hex(raw: &str) -> Option<Vec<u8>> {
    if !raw.len().is_multiple_of(2) {
        return None;
    }
    (0..raw.len() / 2)
        .map(|i| u8::from_str_radix(raw.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
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
            parse_deep_link("compme://setOverride?app=com.apple.TextEdit&enabled=true"),
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
            let url = format!("compme://setOverride?app=com.foo.bar&{param}");
            assert_eq!(parse_deep_link(&url).unwrap().action, expected);
        }
    }

    #[test]
    fn parses_domain_scope() {
        assert_eq!(
            parse_deep_link("compme://setOverride?domain=docs.google.com&excluded=true"),
            Ok(OverrideCommand {
                scope: Scope::Domain("docs.google.com".into()),
                action: OverrideAction::Exclude,
            })
        );
    }

    #[test]
    fn param_order_is_irrelevant() {
        assert_eq!(
            parse_deep_link("compme://setOverride?enabled=false&app=com.foo.bar"),
            Ok(OverrideCommand {
                scope: Scope::App("com.foo.bar".into()),
                action: OverrideAction::Disable,
            })
        );
    }

    #[test]
    fn trailing_slash_on_command_is_tolerated() {
        // Tolerating the slash must still parse the CORRECT scope+action — not
        // merely "no error" (a bug mapping Enable→Disable would pass `.is_ok()`).
        assert_eq!(
            parse_deep_link("compme://setOverride/?app=com.foo.bar&enabled=true"),
            Ok(OverrideCommand {
                scope: Scope::App("com.foo.bar".into()),
                action: OverrideAction::Enable,
            })
        );
    }

    #[test]
    fn wrong_scheme_is_rejected() {
        assert_eq!(
            parse_deep_link("cotypist://setOverride?app=x&enabled=true"),
            Err(ParseError::NotOurScheme)
        );
        assert_eq!(
            parse_deep_link("https://compme/setOverride"),
            Err(ParseError::NotOurScheme)
        );
    }

    #[test]
    fn unknown_command_is_rejected() {
        // setPreference is intentionally NOT in the safe subset yet (needs signing).
        match parse_deep_link("compme://setPreference?app=x&enabled=true") {
            Err(ParseError::UnknownCommand(c)) => assert_eq!(c, "setPreference"),
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn unknown_param_fails_closed() {
        // A smuggled instruction/model param must be rejected, not ignored.
        match parse_deep_link("compme://setOverride?app=x&enabled=true&instructions=be+evil") {
            Err(ParseError::UnknownParam(p)) => assert_eq!(p, "instructions"),
            other => panic!("expected UnknownParam, got {other:?}"),
        }
    }

    #[test]
    fn missing_and_ambiguous_scope_are_errors() {
        assert_eq!(
            parse_deep_link("compme://setOverride?enabled=true"),
            Err(ParseError::MissingScope)
        );
        assert_eq!(
            parse_deep_link("compme://setOverride?app=x&domain=y&enabled=true"),
            Err(ParseError::AmbiguousScope)
        );
    }

    #[test]
    fn missing_and_ambiguous_action_are_errors() {
        assert_eq!(
            parse_deep_link("compme://setOverride?app=x"),
            Err(ParseError::MissingAction)
        );
        assert_eq!(
            parse_deep_link("compme://setOverride?app=x&enabled=true&excluded=true"),
            Err(ParseError::AmbiguousAction)
        );
    }

    #[test]
    fn invalid_action_value_is_rejected() {
        match parse_deep_link("compme://setOverride?app=x&enabled=maybe") {
            Err(ParseError::InvalidValue(v)) => assert_eq!(v, "maybe"),
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn malformed_scope_is_rejected() {
        // Empty, illegal chars, percent-encoding, and oversized all fail closed.
        for bad in [
            "compme://setOverride?app=&enabled=true",
            "compme://setOverride?app=com foo&enabled=true",
            "compme://setOverride?app=a%2Fb&enabled=true",
            "compme://setOverride?app=a/b&enabled=true",
        ] {
            assert_eq!(parse_deep_link(bad), Err(ParseError::InvalidScope), "{bad}");
        }
        let huge = "x".repeat(MAX_SCOPE_LEN + 1);
        assert_eq!(
            parse_deep_link(&format!("compme://setOverride?app={huge}&enabled=true")),
            Err(ParseError::InvalidScope)
        );
    }

    #[test]
    fn duplicate_param_is_rejected_with_its_name() {
        assert_eq!(
            parse_deep_link("compme://setOverride?app=x&app=y&enabled=true"),
            Err(ParseError::DuplicateParam("app".into()))
        );
    }

    #[test]
    fn param_without_equals_is_rejected() {
        assert_eq!(
            parse_deep_link("compme://setOverride?app=x&enabled=true&flag"),
            Err(ParseError::MalformedParam("flag".into()))
        );
    }

    #[test]
    fn command_and_keys_are_case_sensitive() {
        // Lowercase command and capitalized keys fail closed (no aliasing).
        assert_eq!(
            parse_deep_link("compme://setoverride?app=x&enabled=true"),
            Err(ParseError::UnknownCommand("setoverride".into()))
        );
        assert_eq!(
            parse_deep_link("compme://setOverride?App=x&enabled=true"),
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
            let url = format!("compme://setOverride?app=x&{bad}");
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
            parse_deep_link("compme://setOverride?"),
            Err(ParseError::MissingScope)
        );
        assert_eq!(
            parse_deep_link("compme://setOverride"),
            Err(ParseError::MissingScope)
        );
        // A leading empty `&`-pair is ignored, not treated as a malformed param
        // — and the surviving params still parse to the correct command.
        assert_eq!(
            parse_deep_link("compme://setOverride?&app=x&enabled=true"),
            Ok(OverrideCommand {
                scope: Scope::App("x".into()),
                action: OverrideAction::Enable,
            })
        );
    }

    #[test]
    fn scope_charset_and_length_boundary() {
        // Dot, dash, underscore, digit all accepted AND preserved verbatim in
        // the scope (not silently stripped).
        assert_eq!(
            parse_deep_link("compme://setOverride?app=com.foo_bar-baz9&enabled=true"),
            Ok(OverrideCommand {
                scope: Scope::App("com.foo_bar-baz9".into()),
                action: OverrideAction::Enable,
            })
        );
        // Exactly MAX_SCOPE_LEN passes AND the scope survives at full length —
        // an off-by-one truncation at the boundary would shorten it and still
        // be `.is_ok()`.
        let max = "a".repeat(MAX_SCOPE_LEN);
        match parse_deep_link(&format!("compme://setOverride?app={max}&enabled=true")) {
            Ok(OverrideCommand {
                scope: Scope::App(s),
                ..
            }) => assert_eq!(
                s.len(),
                MAX_SCOPE_LEN,
                "max-length scope must survive intact"
            ),
            other => panic!("expected Ok App scope at MAX_SCOPE_LEN, got {other:?}"),
        }
    }

    #[test]
    fn prompt_decision_requires_confirmation_and_names_the_command() {
        let command = OverrideCommand {
            scope: Scope::App("com.apple.Mail".into()),
            action: OverrideAction::Exclude,
        };
        // Unsigned reversible link: prompt, with human-readable pieces.
        let decision = prompt_decision_for_link(&command, LinkTrust::Unsigned);
        let PromptDecision::PromptRequired {
            scope,
            action,
            trust,
        } = decision
        else {
            panic!("unsigned links must prompt");
        };
        assert_eq!(scope, "com.apple.Mail");
        assert!(action.contains("Exclude"), "{action}");
        assert!(trust.contains("unsigned"), "{trust}");
        // Signed links ALSO prompt today (no non-reversible commands exist;
        // the silent path is reserved and unreachable until they do).
        let signed = prompt_decision_for_link(&command, LinkTrust::Signed);
        assert!(
            matches!(signed, PromptDecision::PromptRequired { trust, .. } if trust.contains("signed"))
        );
    }

    // ---- signed links ----

    /// Deterministic test keypair: the signer the host trusts.
    fn test_signer() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
    }

    fn test_trusted_key() -> TrustedKey {
        let hex = encode_hex(test_signer().verifying_key().as_bytes());
        TrustedKey::from_hex(&hex).expect("valid test key")
    }

    fn encode_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Sign `payload` with the test signer and append the trailing sig param.
    fn signed_url(payload: &str) -> String {
        use ed25519_dalek::Signer;
        let sig = test_signer().sign(payload.as_bytes());
        format!("{payload}&sig={}", encode_hex(&sig.to_bytes()))
    }

    #[test]
    fn an_unsigned_link_parses_as_unsigned_trust() {
        assert_eq!(
            parse_deep_link_with_trust(
                "compme://setOverride?app=com.apple.TextEdit&enabled=true",
                None,
            ),
            Ok((
                OverrideCommand {
                    scope: Scope::App("com.apple.TextEdit".into()),
                    action: OverrideAction::Enable,
                },
                LinkTrust::Unsigned,
            ))
        );
    }

    #[test]
    fn a_validly_signed_link_parses_as_signed_trust() {
        let url = signed_url("compme://setOverride?app=com.apple.TextEdit&excluded=true");
        assert_eq!(
            parse_deep_link_with_trust(&url, Some(&test_trusted_key())),
            Ok((
                OverrideCommand {
                    scope: Scope::App("com.apple.TextEdit".into()),
                    action: OverrideAction::Exclude,
                },
                LinkTrust::Signed,
            ))
        );
    }

    #[test]
    fn a_signed_link_without_a_trusted_key_fails_closed() {
        let url = signed_url("compme://setOverride?app=com.apple.TextEdit&enabled=true");
        assert_eq!(
            parse_deep_link_with_trust(&url, None),
            Err(ParseError::UntrustedSignature)
        );
    }

    #[test]
    fn a_tampered_payload_fails_verification() {
        let url = signed_url("compme://setOverride?app=com.apple.TextEdit&enabled=true");
        // Flip the payload after signing: enable → disable.
        let tampered = url.replace("enabled=true", "enabled=false");
        assert_eq!(
            parse_deep_link_with_trust(&tampered, Some(&test_trusted_key())),
            Err(ParseError::InvalidSignature)
        );
    }

    #[test]
    fn a_valid_signature_over_a_malformed_payload_surfaces_the_parse_error() {
        // Verify-THEN-parse ordering: when a payload is correctly signed by
        // the trusted key but is itself malformed, the caller must see the
        // PARSE error (so the host can tell the user *what* was wrong with the
        // link), NOT a signature error. A reversed order (parse-then-verify,
        // or short-circuiting parse failures into InvalidSignature) would hide
        // the real defect behind a misleading "signature failed".
        //
        // (a) a genuinely-signed but empty scope → InvalidScope, not a sig error.
        let url = signed_url("compme://setOverride?app=&enabled=true");
        assert_eq!(
            parse_deep_link_with_trust(&url, Some(&test_trusted_key())),
            Err(ParseError::InvalidScope),
            "a verified signature over an invalid scope must surface InvalidScope"
        );

        // (b) a signed but unknown command → UnknownCommand, surfacing the
        // command name — again the parse error, not the signature layer.
        let url = signed_url("compme://setPreference?app=com.foo.bar&enabled=true");
        assert_eq!(
            parse_deep_link_with_trust(&url, Some(&test_trusted_key())),
            Err(ParseError::UnknownCommand("setPreference".into())),
            "a verified signature over an unknown command must surface UnknownCommand"
        );
    }

    #[test]
    fn a_signature_from_an_untrusted_signer_fails_verification() {
        use ed25519_dalek::Signer;
        let payload = "compme://setOverride?app=com.apple.TextEdit&enabled=true";
        let rogue = ed25519_dalek::SigningKey::from_bytes(&[8u8; 32]);
        let sig = rogue.sign(payload.as_bytes());
        let url = format!("{payload}&sig={}", encode_hex(&sig.to_bytes()));
        assert_eq!(
            parse_deep_link_with_trust(&url, Some(&test_trusted_key())),
            Err(ParseError::InvalidSignature)
        );
    }

    #[test]
    fn a_malformed_signature_value_is_rejected_before_verification() {
        let payload = "compme://setOverride?app=com.apple.TextEdit&enabled=true";
        for bad in [
            "",               // empty
            "deadbeef",       // too short
            &"z".repeat(128), // non-hex
            &"ab".repeat(63), // wrong length (126 chars)
            &"ab".repeat(65), // wrong length (130 chars)
        ] {
            assert_eq!(
                parse_deep_link_with_trust(
                    &format!("{payload}&sig={bad}"),
                    Some(&test_trusted_key()),
                ),
                Err(ParseError::MalformedSignature),
                "sig value {bad:?} must be rejected as malformed"
            );
        }
    }

    #[test]
    fn a_signature_that_is_not_the_final_parameter_is_misplaced() {
        // Anything after the sig value would be unsigned, attacker-appendable
        // bytes — including a second sig.
        let url = signed_url("compme://setOverride?app=com.apple.TextEdit&enabled=true");
        for appended in ["&excluded=true", "&sig=00", "&x=1"] {
            assert_eq!(
                parse_deep_link_with_trust(&format!("{url}{appended}"), Some(&test_trusted_key()),),
                Err(ParseError::MisplacedSignature),
                "appending {appended:?} after the signature must fail closed"
            );
        }
    }

    #[test]
    fn a_sig_as_the_first_parameter_is_an_unknown_param_not_a_signature() {
        // Only a trailing `&sig=` is the signature envelope; `?sig=` means the
        // payload has no parameters before it, which the safe subset never
        // produces — it falls through to the strict parser and fails closed.
        assert_eq!(
            parse_deep_link_with_trust("compme://setOverride?sig=00", Some(&test_trusted_key()),),
            Err(ParseError::UnknownParam("sig".into()))
        );
    }

    #[test]
    fn the_unsigned_parser_still_rejects_sig_as_unknown() {
        // Regression pin: the pre-signing API never silently accepts a signed
        // link (it would drop the signature semantics on the floor).
        assert_eq!(
            parse_deep_link("compme://setOverride?app=com.apple.TextEdit&enabled=true&sig=00",),
            Err(ParseError::UnknownParam("sig".into()))
        );
    }

    #[test]
    fn trusted_key_from_hex_rejects_malformed_input() {
        assert!(TrustedKey::from_hex("").is_none());
        assert!(TrustedKey::from_hex("deadbeef").is_none()); // too short
        assert!(TrustedKey::from_hex(&"z".repeat(64)).is_none()); // non-hex
        assert!(TrustedKey::from_hex(&"ab".repeat(33)).is_none()); // too long
                                                                   // A valid key round-trips.
        let hex = encode_hex(test_signer().verifying_key().as_bytes());
        assert!(TrustedKey::from_hex(&hex).is_some());
    }
}
