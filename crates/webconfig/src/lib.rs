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
//! **Expiry (G14):** every signed link MUST carry `exp=<unix seconds>` inside
//! the signed prefix (before `&sig=`), and is rejected once that deadline
//! passes. `exp` is **required on signed links, optional on unsigned ones**:
//! a signature is a standing grant that a captured link would otherwise replay
//! forever, so the fail-closed rule belongs where the authority lives — an
//! unsigned link carries no authority at all (any page can mint one from
//! scratch), so a deadline on it protects nothing. Backward compatibility
//! costs nothing here: the only signer is the in-repo `sign_link` example,
//! which now stamps `exp` itself, and requiring it today means the future
//! non-reversible commands this gate exists for cannot ship without it. An
//! `exp` on an unsigned link is still honoured when present (never silently
//! ignored). The deadline is compared against an injected clock; only the
//! public entry points read the system clock.
//!
//! **Reversibility is NOT a full substitute for signing.** Because any page can
//! fire a deep link, an unsigned link can still nuisance-toggle a user's apps
//! (clickjacking / DoS by rapid toggling). Two host-layer requirements are
//! therefore mandatory, not optional: (1) the host MUST surface every applied
//! command to the user (the §16 "user-visible" requirement) and SHOULD allow
//! undo; (2) any future non-reversible command (custom instructions, model
//! override, security settings) MUST be gated on [`LinkTrust::Signed`] when it
//! is added here. The macOS host receives URL-scheme events and confirms every
//! parsed command before applying it.

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

/// Canonical spelling for a DNS host used by domain-scoped policy. DNS names
/// are case-insensitive, and one terminal dot denotes the absolute root label
/// without changing the host's identity.
pub fn normalize_domain(domain: &str) -> String {
    let mut normalized = domain.to_ascii_lowercase();
    if normalized.ends_with('.') {
        normalized.pop();
    }
    normalized
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
    /// The `exp` value was not plain unix seconds (ASCII digits fitting `u64`).
    MalformedExpiry(String),
    /// A signed link carried no `exp=` deadline (required on signed links).
    MissingExpiry,
    /// The link's `exp=` deadline has passed. Distinct from a malformed link
    /// and from an untrusted one: this link was well-formed and correctly
    /// signed, it is simply a replay of one whose authority has run out.
    ExpiredLink { exp: u64, now: u64 },
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
            ParseError::MalformedExpiry(value) => {
                write!(f, "malformed expiry (need unix seconds): {value}")
            }
            ParseError::MissingExpiry => {
                write!(f, "signed link without an `exp` deadline")
            }
            ParseError::ExpiredLink { exp, now } => {
                write!(f, "link expired (exp={exp}, now={now})")
            }
        }
    }
}

impl std::error::Error for ParseError {}

const SCHEME: &str = "compme://";
/// Bundle ids / domains are short; cap to reject absurd inputs.
const MAX_SCOPE_LEN: usize = 253;

/// Test shorthand: [`parse_deep_link_at`] against the system clock. Production
/// parses every link through [`parse_deep_link_with_trust`].
#[cfg(test)]
fn parse_deep_link(url: &str) -> Result<OverrideCommand, ParseError> {
    parse_deep_link_at(url, now_unix_secs())
}

/// Parse and validate an unsigned `compme://setOverride?...` deep link. Returns
/// the reversible command, or a specific [`ParseError`] — never a
/// partial/guessed result. An `exp=` deadline is optional here (an unsigned
/// link has no authority to expire) but is enforced when present. The clock is
/// injected so the expiry rule stays pure and deterministically testable: only
/// the public entry point reads the system clock, never the verifier below it.
fn parse_deep_link_at(url: &str, now_unix_secs: u64) -> Result<OverrideCommand, ParseError> {
    let (command, expiry) = parse_payload(url)?;
    check_expiry(expiry, now_unix_secs)?;
    Ok(command)
}

/// Parse the link body into the command plus its optional `exp=` deadline,
/// without judging that deadline — the caller decides whether a missing one is
/// allowed and compares it against its injected clock.
fn parse_payload(url: &str) -> Result<(OverrideCommand, Option<u64>), ParseError> {
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
    let mut expiry: Option<u64> = None;

    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| ParseError::MalformedParam(pair.to_string()))?;
        match key {
            "app" => set_once(&mut app, key, value.to_string())?,
            "domain" => set_once(&mut domain, key, value.to_string())?,
            "enabled" => set_once(&mut enabled, key, parse_bool(value)?)?,
            "excluded" => set_once(&mut excluded, key, parse_bool(value)?)?,
            "exp" => set_once(&mut expiry, key, parse_unix_seconds(value)?)?,
            other => return Err(ParseError::UnknownParam(other.to_string())),
        }
    }

    let scope = match (app, domain) {
        (Some(_), Some(_)) => return Err(ParseError::AmbiguousScope),
        (None, None) => return Err(ParseError::MissingScope),
        (Some(a), None) => Scope::App(valid_app_scope(a)?),
        (None, Some(d)) => Scope::Domain(valid_domain_scope(d)?),
    };

    let action = match (enabled, excluded) {
        (Some(_), Some(_)) => return Err(ParseError::AmbiguousAction),
        (None, None) => return Err(ParseError::MissingAction),
        (Some(true), None) => OverrideAction::Enable,
        (Some(false), None) => OverrideAction::Disable,
        (None, Some(true)) => OverrideAction::Exclude,
        (None, Some(false)) => OverrideAction::Include,
    };

    Ok((OverrideCommand { scope, action }, expiry))
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

/// Parse and validate a `compme://setOverride?...` deep link — the reversible
/// command, or a specific [`ParseError`], never a partial/guessed result —
/// signature-aware: a trailing
/// `&sig=<128 hex>` parameter is split off and verified (Ed25519, over the
/// exact URL bytes preceding `&sig=`) against the host's trusted key before
/// the payload is parsed. A verified payload must also carry an `exp=` unix
/// deadline it has not yet reached, so a captured signed link stops working
/// (G14). Unsigned links still parse (the reversible subset needs no
/// signature, and no deadline) and are labeled [`LinkTrust::Unsigned`].
pub fn parse_deep_link_with_trust(
    url: &str,
    trusted: Option<&TrustedKey>,
) -> Result<(OverrideCommand, LinkTrust), ParseError> {
    parse_deep_link_with_trust_at(url, trusted, now_unix_secs())
}

/// [`parse_deep_link_with_trust`] with the clock injected. A signed link must
/// carry an `exp=` deadline inside the signed prefix (module docs) and is
/// rejected once `now_unix_secs` reaches it, so a captured link stops
/// verifying instead of replaying forever (G14).
fn parse_deep_link_with_trust_at(
    url: &str,
    trusted: Option<&TrustedKey>,
    now_unix_secs: u64,
) -> Result<(OverrideCommand, LinkTrust), ParseError> {
    match split_trailing_signature(url)? {
        None => {
            parse_deep_link_at(url, now_unix_secs).map(|command| (command, LinkTrust::Unsigned))
        }
        Some((payload, signature)) => {
            let key = trusted.ok_or(ParseError::UntrustedSignature)?;
            key.0
                .verify_strict(payload.as_bytes(), &signature)
                .map_err(|_| ParseError::InvalidSignature)?;
            // Parse first (a signed-but-malformed payload must still surface
            // its own parse error), then apply the signed-link expiry rule.
            let (command, expiry) = parse_payload(payload)?;
            if expiry.is_none() {
                return Err(ParseError::MissingExpiry);
            }
            check_expiry(expiry, now_unix_secs)?;
            Ok((command, LinkTrust::Signed))
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

/// The §16 mandatory host confirmation for a parsed deep link: every link must
/// be confirmed by the user before applying. Pure — the host's UI layer renders
/// the prompt from these human-readable pieces; tests inject the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptDecision {
    pub scope: String,
    pub action: String,
    pub trust: String,
}

/// Decide the confirmation requirement for a parsed link. Today EVERY link
/// prompts — reversible-but-unsigned links can still nuisance-toggle (module
/// docs), and no silent-eligible command class exists yet.
pub fn prompt_decision_for_link(command: &OverrideCommand, trust: LinkTrust) -> PromptDecision {
    let scope = match &command.scope {
        Scope::App(app) => app.clone(),
        Scope::Domain(domain) => domain.clone(),
    };
    PromptDecision {
        scope,
        action: format!("{:?}", command.action),
        trust: match trust {
            LinkTrust::Unsigned => "unsigned link".to_string(),
            LinkTrust::Signed => "signed link, verified".to_string(),
        },
    }
}

/// Wall-clock unix seconds, read only at the public API edge. A clock before
/// the epoch saturates to `u64::MAX`, expiring every deadline — the fail-closed
/// direction, where returning 0 would make every captured link look fresh.
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(u64::MAX)
}

/// Reject a link whose `exp` deadline has been reached. `now == exp` is already
/// expired (the deadline is the first instant the link is dead, as with a JWT
/// `exp`). `None` means no deadline was supplied; the caller has already
/// decided whether that is allowed for this trust level.
fn check_expiry(expiry: Option<u64>, now_unix_secs: u64) -> Result<(), ParseError> {
    match expiry {
        Some(exp) if now_unix_secs >= exp => Err(ParseError::ExpiredLink {
            exp,
            now: now_unix_secs,
        }),
        _ => Ok(()),
    }
}

/// Plain unix seconds: ASCII digits only, fitting in `u64`. The digit check is
/// ours rather than `from_str`'s because `u64::from_str` accepts a leading `+`
/// (the same leniency `parse_hex` guards against below), and `+1` must be
/// malformed, not a deadline in 1970.
fn parse_unix_seconds(value: &str) -> Result<u64, ParseError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParseError::MalformedExpiry(value.to_string()));
    }
    value
        .parse::<u64>()
        .map_err(|_| ParseError::MalformedExpiry(value.to_string()))
}

/// Decode a hex string into bytes; `None` on odd length or a non-hex digit.
fn parse_hex(raw: &str) -> Option<Vec<u8>> {
    if !raw.len().is_multiple_of(2) {
        return None;
    }
    // Reject every non-hex byte OURSELVES before decoding. `from_str_radix`
    // accepts an optional leading `+`, so a pair like "+a" would decode to 0x0a
    // and let a signature or trusted key that is not hex at all through as
    // well-formed — harmless today (the bytes are wrong, so verification fails)
    // but it contradicts this module's fail-closed contract and would report
    // `InvalidSignature` where `MalformedSignature` is the truth.
    if !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
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

/// Shared lexical scope check: non-empty, bounded, and limited to the
/// unreserved characters real identifiers use. Rejects spaces,
/// percent-encoding, path or query metacharacters — so an attacker can't
/// smuggle a second command or a control sequence through the scope.
fn valid_scope_lexical(s: &str) -> Result<(), ParseError> {
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
    Ok(())
}

fn valid_app_scope(s: String) -> Result<String, ParseError> {
    valid_scope_lexical(&s)?;
    Ok(s)
}

fn valid_domain_scope(s: String) -> Result<String, ParseError> {
    let s = normalize_domain(&s);
    valid_scope_lexical(&s)?;

    let labels: Vec<&str> = s.split('.').collect();
    if labels.len() < 2 {
        return Err(ParseError::InvalidScope);
    }

    for label in &labels {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || label.contains('_')
        {
            return Err(ParseError::InvalidScope);
        }
    }

    let tld = labels.last().expect("at least two labels");
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(ParseError::InvalidScope);
    }

    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_normalization_folds_case_and_one_dns_root_dot() {
        assert_eq!(normalize_domain("Bank.Example."), "bank.example");
        assert_eq!(normalize_domain("BANK.EXAMPLE"), "bank.example");
        assert_eq!(normalize_domain("example.com.."), "example.com.");
        assert_eq!(normalize_domain("."), "");
    }

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
    fn domain_scope_canonicalizes_case_and_a_dns_root_dot() {
        assert_eq!(
            parse_deep_link("compme://setOverride?domain=Bank.Example.&excluded=true"),
            Ok(OverrideCommand {
                scope: Scope::Domain("bank.example".into()),
                action: OverrideAction::Exclude,
            })
        );
    }

    #[test]
    fn domain_with_two_char_tld_is_accepted() {
        // A two-character TLD is the shortest legal TLD: pins the `tld.len() < 2`
        // lower bound so a `<= 2` mutant (which would reject `example.io`) fails.
        assert_eq!(
            parse_deep_link("compme://setOverride?domain=example.io&enabled=true"),
            Ok(OverrideCommand {
                scope: Scope::Domain("example.io".into()),
                action: OverrideAction::Enable,
            })
        );
    }

    #[test]
    fn domain_labels_enforce_the_dns_63_byte_limit() {
        let longest_valid = format!("{}.example.com", "a".repeat(63));
        assert!(parse_deep_link(&format!(
            "compme://setOverride?domain={longest_valid}&enabled=true"
        ))
        .is_ok());

        let oversized_label = format!("{}.example.com", "a".repeat(64));
        assert_eq!(
            parse_deep_link(&format!(
                "compme://setOverride?domain={oversized_label}&enabled=true"
            )),
            Err(ParseError::InvalidScope)
        );
    }

    #[test]
    fn domain_scope_rejects_overly_broad_or_malformed_hosts() {
        for bad in [
            "com",
            "localhost",
            ".example.com",
            "example..com",
            "bad_domain.com",
            "-example.com",
            "example-.com",
            "example.c",
            "example.123",
        ] {
            let url = format!("compme://setOverride?domain={bad}&excluded=true");
            assert_eq!(
                parse_deep_link(&url),
                Err(ParseError::InvalidScope),
                "{bad}"
            );
        }
    }

    #[test]
    fn app_scope_still_accepts_bundle_identifier_underscores() {
        assert_eq!(
            parse_deep_link("compme://setOverride?app=com.example.My_App&enabled=false"),
            Ok(OverrideCommand {
                scope: Scope::App("com.example.My_App".into()),
                action: OverrideAction::Disable,
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
    fn oversized_domain_scope_is_rejected() {
        // A domain host that is structurally valid (multiple non-empty labels,
        // no `_`, no leading/trailing `-`, alphabetic 2+ char TLD) but whose total
        // length exceeds MAX_SCOPE_LEN is rejected with InvalidScope — the same
        // length cap (in valid_scope_lexical, applied before the domain structural
        // checks) that bounds app scopes also bounds domain scopes.
        let long_label = "a".repeat(MAX_SCOPE_LEN);
        let host = format!("{long_label}.example.com");
        assert!(
            host.len() > MAX_SCOPE_LEN,
            "host must exceed the length cap"
        );
        assert_eq!(
            parse_deep_link(&format!("compme://setOverride?domain={host}&excluded=true")),
            Err(ParseError::InvalidScope)
        );
    }

    #[test]
    fn scope_rejects_query_fragment_metacharacters_and_control_chars() {
        // The lexical allow-list (alphanumeric + `.` `-` `_`) must reject URL
        // structural metacharacters that could smuggle a second command or
        // parameter, and any control/whitespace characters. `&`/`=` are query
        // separators consumed before scope validation, so they're exercised at
        // the parser level (a trailing fragment, an extra `=`); the rest are
        // tested directly against the scope value.
        for bad in [
            "?",      // query start
            "#",      // fragment start
            ";",      // path/param separator
            ":",      // scheme/port separator
            "@",      // userinfo
            "/",      // path separator (regression with the existing case)
            "\\",     // backslash
            "*",      // glob
            "<",      // angle brackets
            ">",      //
            "\u{0}",  // NUL control char
            "\n",     // newline control char
            "\t",     // tab control char
            "\u{7f}", // DEL control char
            " ",      // space
        ] {
            let url = format!("compme://setOverride?app=com.foo{bad}bar&enabled=true");
            assert_eq!(
                parse_deep_link(&url),
                Err(ParseError::InvalidScope),
                "scope containing {bad:?} must be rejected"
            );
        }
        // `=` inside the value is consumed as the param separator: `app=a=b`
        // splits to key `app`, value `a=b` — `=` is then an illegal scope char.
        assert_eq!(
            parse_deep_link("compme://setOverride?app=a=b&enabled=true"),
            Err(ParseError::InvalidScope),
            "an `=` smuggled into the scope value must be rejected"
        );
        // A trailing `#fragment` rides on the last value and is rejected as an
        // illegal action value (not silently dropped).
        match parse_deep_link("compme://setOverride?app=com.foo.bar&enabled=true#x") {
            Err(ParseError::InvalidValue(v)) => assert_eq!(
                v, "true#x",
                "the fragment must ride on the echoed value, not be silently dropped"
            ),
            other => panic!("a trailing fragment must not be silently accepted, got {other:?}"),
        }
    }

    #[test]
    fn scope_rejects_non_ascii_confusable_homoglyphs() {
        // Homograph-attack guard: the charset allow-list is
        // `is_ascii_alphanumeric()`, NOT Unicode `is_alphanumeric()`, so a scope
        // carrying a non-ASCII letter that merely LOOKS like an ASCII one (a
        // confusable / homoglyph) must fail closed as InvalidScope — never be
        // accepted as a different-bytes look-alike of a legitimate app/domain.
        // The existing charset test pins ASCII metacharacters and control bytes;
        // this pins the distinct non-ASCII *alphabetic* direction. A future widen
        // to `is_alphanumeric()` (which returns true for these) would silently
        // open homograph spoofing of the scope and is exactly what this kills.
        for confusable in [
            "\u{0430}pple",   // Cyrillic 'а' (U+0430) for ASCII 'a' in "apple"
            "g\u{043e}ogle",  // Cyrillic 'о' (U+043E) for ASCII 'o' in "google"
            "\u{0440}aypal",  // Cyrillic 'р' (U+0440) for ASCII 'p' in "paypal"
            "ex\u{0430}mple", // Cyrillic 'а' inside a domain-shaped label
            "caf\u{e9}",      // Latin-1 'é' — a real letter, still non-ASCII
            "\u{ff41}pp",     // fullwidth 'ａ' (U+FF41) homoglyph of ASCII 'a'
        ] {
            // App scope: any non-ASCII letter is rejected outright.
            let app_url = format!("compme://setOverride?app={confusable}&enabled=true");
            assert_eq!(
                parse_deep_link(&app_url),
                Err(ParseError::InvalidScope),
                "non-ASCII confusable app scope {confusable:?} must be rejected"
            );
            // Domain scope: the same lexical gate runs first, so a confusable
            // host is rejected before any structural domain check.
            let domain_url = format!("compme://setOverride?domain={confusable}.com&excluded=true");
            assert_eq!(
                parse_deep_link(&domain_url),
                Err(ParseError::InvalidScope),
                "non-ASCII confusable domain scope {confusable:?}.com must be rejected"
            );
        }
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
            let expected = bad
                .strip_prefix("enabled=")
                .expect("test inputs start with enabled=");
            match parse_deep_link(&url) {
                Err(ParseError::InvalidValue(v)) => {
                    assert_eq!(v, expected, "{bad} must be rejected with its echoed value")
                }
                other => panic!("{bad} must be rejected, got {other:?}"),
            }
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
        let PromptDecision {
            scope,
            action,
            trust,
        } = decision;
        assert_eq!(scope, "com.apple.Mail");
        assert!(action.contains("Exclude"), "{action}");
        assert!(trust.contains("unsigned"), "{trust}");
        // Signed links ALSO prompt today (no non-reversible commands exist;
        // the silent path is reserved and unreachable until they do).
        let signed = prompt_decision_for_link(&command, LinkTrust::Signed);
        // "unsigned link" also contains the substring "signed", so match on the
        // exact label to actually distinguish a Signed link from an Unsigned one.
        assert_eq!(signed.trust, "signed link, verified");
    }

    #[test]
    fn prompt_decision_names_domain_scope_command() {
        // Mirrors the App-scope prompt test for the Domain arm of
        // prompt_decision_for_link: the domain string must flow through to the
        // prompt's scope field, and action/trust must render the same way.
        let command = OverrideCommand {
            scope: Scope::Domain("docs.google.com".into()),
            action: OverrideAction::Disable,
        };
        let decision = prompt_decision_for_link(&command, LinkTrust::Unsigned);
        let PromptDecision {
            scope,
            action,
            trust,
        } = decision;
        assert_eq!(scope, "docs.google.com");
        assert!(action.contains("Disable"), "{action}");
        assert!(trust.contains("unsigned"), "{trust}");
        // Signed domain links ALSO prompt today (silent path still unreachable).
        let signed = prompt_decision_for_link(&command, LinkTrust::Signed);
        // Exact label, not `contains("signed")` — "unsigned link" contains that
        // substring too and would not distinguish the two trust levels.
        assert!(matches!(
            signed,
            PromptDecision { scope, trust, .. }
                if scope == "docs.google.com" && trust == "signed link, verified"
        ));
    }

    #[test]
    fn prompt_decision_names_every_action_across_both_scopes() {
        // Extends the single-case prompt tests: the action label must render
        // distinctly for ALL FOUR actions, and the scope string must flow
        // through for BOTH scope kinds (App and Domain). A bug that mislabeled
        // one action (e.g. Enable→Disable) or dropped one scope arm would pass
        // the existing one-case tests but fail here.
        let scopes = [
            ("com.apple.Mail", Scope::App("com.apple.Mail".into())),
            ("docs.google.com", Scope::Domain("docs.google.com".into())),
        ];
        let actions = [
            (OverrideAction::Enable, "Enable"),
            (OverrideAction::Disable, "Disable"),
            (OverrideAction::Exclude, "Exclude"),
            (OverrideAction::Include, "Include"),
        ];
        for (scope_str, scope) in scopes {
            for (action, action_label) in actions {
                let command = OverrideCommand {
                    scope: scope.clone(),
                    action,
                };
                let PromptDecision {
                    scope: got_scope,
                    action: got_action,
                    trust,
                } = prompt_decision_for_link(&command, LinkTrust::Unsigned);
                assert_eq!(got_scope, scope_str, "scope for {action_label}");
                assert_eq!(
                    got_action, action_label,
                    "action label for {scope_str}/{action_label}"
                );
                assert!(trust.contains("unsigned"), "{trust}");
            }
        }
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

    /// A deterministic "now" for the injected clock (2027-01-15, unix seconds).
    const TEST_NOW: u64 = 1_800_000_000;

    /// Sign `payload` with an `exp=` deadline appended INSIDE the signed
    /// prefix — the only shape a signed link may take (module docs).
    fn signed_url_expiring_at(payload: &str, exp: u64) -> String {
        signed_url(&format!("{payload}&exp={exp}"))
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
    fn an_unsigned_link_still_parses_when_a_trusted_key_is_configured() {
        // Contract: a configured trust key gates the SIGNED path only; an
        // unsigned link (no `sig=`) must still parse and surface as Unsigned so
        // the caller can apply its own unsigned-link policy. Pin the branch with
        // `Some(key)` present — the sibling test only drives it with `None`, so a
        // refactor that consulted `trusted` before the signature check (rejecting
        // all unsigned links whenever a key is pinned) would pass unnoticed.
        assert_eq!(
            parse_deep_link_with_trust(
                "compme://setOverride?app=com.apple.Mail&enabled=true",
                Some(&test_trusted_key()),
            ),
            Ok((
                OverrideCommand {
                    scope: Scope::App("com.apple.Mail".into()),
                    action: OverrideAction::Enable,
                },
                LinkTrust::Unsigned,
            ))
        );
    }

    #[test]
    fn a_validly_signed_link_parses_as_signed_trust() {
        // A signed link carries its `exp=` inside the signed prefix; here it is
        // still in the future for the injected clock.
        let url = signed_url_expiring_at(
            "compme://setOverride?app=com.apple.TextEdit&excluded=true",
            TEST_NOW + 60,
        );
        assert_eq!(
            parse_deep_link_with_trust_at(&url, Some(&test_trusted_key()), TEST_NOW),
            Ok((
                OverrideCommand {
                    scope: Scope::App("com.apple.TextEdit".into()),
                    action: OverrideAction::Exclude,
                },
                LinkTrust::Signed,
            ))
        );
    }

    // ---- link expiry (G14) ----

    #[test]
    fn a_signed_link_is_rejected_once_its_expiry_has_passed() {
        // A captured link must stop verifying. The deadline instant itself is
        // already dead (`now == exp`), as with a JWT `exp` — pinning the `>=`
        // so a `>` mutant (one free second) fails.
        let payload = "compme://setOverride?app=com.apple.TextEdit&enabled=true";
        for exp in [TEST_NOW - 1, TEST_NOW] {
            let url = signed_url_expiring_at(payload, exp);
            assert_eq!(
                parse_deep_link_with_trust_at(&url, Some(&test_trusted_key()), TEST_NOW),
                Err(ParseError::ExpiredLink { exp, now: TEST_NOW }),
                "exp={exp} must be expired at now={TEST_NOW}"
            );
        }
        // The expiry failure is its OWN error: the link is neither malformed
        // nor untrusted, so a host can say "this link has expired" rather than
        // "this link is broken" or "we do not trust this signer".
        let expired = signed_url_expiring_at(payload, TEST_NOW - 1);
        let err = parse_deep_link_with_trust_at(&expired, Some(&test_trusted_key()), TEST_NOW)
            .expect_err("expired");
        assert_ne!(err, ParseError::MalformedExpiry((TEST_NOW - 1).to_string()));
        assert_ne!(err, ParseError::MissingExpiry);
        assert_ne!(err, ParseError::UntrustedSignature);
        assert_ne!(err, ParseError::InvalidSignature);
        assert!(err.to_string().contains("expired"), "{err}");
    }

    #[test]
    fn a_signed_link_before_its_expiry_is_accepted() {
        // One second before the deadline is the tightest accepted case; pins
        // the other side of the `>=` boundary above.
        let url = signed_url_expiring_at(
            "compme://setOverride?domain=docs.google.com&excluded=true",
            TEST_NOW + 1,
        );
        assert_eq!(
            parse_deep_link_with_trust_at(&url, Some(&test_trusted_key()), TEST_NOW),
            Ok((
                OverrideCommand {
                    scope: Scope::Domain("docs.google.com".into()),
                    action: OverrideAction::Exclude,
                },
                LinkTrust::Signed,
            ))
        );
    }

    #[test]
    fn a_signed_link_without_an_expiry_is_rejected() {
        // The required-vs-optional decision, pinned: `exp` is REQUIRED on a
        // signed link. A signature is a standing grant, so the fail-closed
        // posture applies to it; the only signer is the in-repo `sign_link`
        // example (which stamps `exp`), so nothing already-signed regresses,
        // and a future non-reversible command cannot ship an unexpiring link
        // by forgetting to add the parameter.
        let url = signed_url("compme://setOverride?app=com.apple.TextEdit&enabled=true");
        assert_eq!(
            parse_deep_link_with_trust_at(&url, Some(&test_trusted_key()), TEST_NOW),
            Err(ParseError::MissingExpiry)
        );
    }

    #[test]
    fn tampering_with_the_expiry_after_signing_fails_verification() {
        // `exp` lives INSIDE the signed prefix, so extending it is exactly as
        // detectable as retargeting the app: the bytes change, the signature
        // does not cover them any more. If this ever returns Ok, the deadline
        // has been moved outside the signature and G14 is back.
        let key = test_trusted_key();
        let payload = "compme://setOverride?app=com.apple.TextEdit&enabled=true";
        let url = signed_url_expiring_at(payload, TEST_NOW - 1);
        let extended = url.replace(
            &format!("exp={}", TEST_NOW - 1),
            &format!("exp={}", TEST_NOW + 3600),
        );
        assert_ne!(extended, url, "the replacement must have changed the URL");
        assert_eq!(
            parse_deep_link_with_trust_at(&extended, Some(&key), TEST_NOW),
            Err(ParseError::InvalidSignature),
            "reviving an expired link by editing `exp` must fail verification"
        );
        // ...and shortening a live link's deadline is equally unsigned.
        let live = signed_url_expiring_at(payload, TEST_NOW + 3600);
        let shortened = live.replace(
            &format!("exp={}", TEST_NOW + 3600),
            &format!("exp={}", TEST_NOW - 1),
        );
        assert_eq!(
            parse_deep_link_with_trust_at(&shortened, Some(&key), TEST_NOW),
            Err(ParseError::InvalidSignature)
        );
    }

    #[test]
    fn a_malformed_expiry_value_is_rejected_as_malformed() {
        // Distinct from an expired deadline: these never parse as a time at
        // all. `+1` is the `u64::from_str` leniency the crate rejects for hex
        // too — it must not decode to a 1970 deadline.
        let key = test_trusted_key();
        for bad in [
            "",                     // empty
            "abc",                  // not a number
            "+1",                   // from_str accepts a leading `+`; we must not
            "-1",                   // negative
            "12.5",                 // fractional
            "1e9",                  // exponent notation
            " 1",                   // leading space
            "18446744073709551616", // u64::MAX + 1 (overflow)
        ] {
            // Sign the malformed value itself (editing it afterwards would
            // fail on the signature instead, hiding what is under test).
            let url = signed_url(&format!(
                "compme://setOverride?app=com.apple.TextEdit&enabled=true&exp={bad}"
            ));
            assert_eq!(
                parse_deep_link_with_trust_at(&url, Some(&key), TEST_NOW),
                Err(ParseError::MalformedExpiry(bad.to_string())),
                "exp value {bad:?} must be rejected as malformed"
            );
            // Same verdict on the unsigned parser, which shares the payload
            // parser (the malformed value is never silently dropped).
            assert_eq!(
                parse_deep_link(&format!(
                    "compme://setOverride?app=com.apple.TextEdit&enabled=true&exp={bad}"
                )),
                Err(ParseError::MalformedExpiry(bad.to_string())),
                "unsigned exp value {bad:?} must be rejected as malformed"
            );
        }
        // A duplicate `exp` is still a duplicate param, not a last-one-wins.
        assert_eq!(
            parse_deep_link("compme://setOverride?app=com.foo.bar&enabled=true&exp=1&exp=2"),
            Err(ParseError::DuplicateParam("exp".into()))
        );
    }

    #[test]
    fn an_unsigned_link_may_omit_an_expiry_but_never_outlives_one() {
        // `exp` is OPTIONAL on unsigned links: they carry no authority to
        // expire (any page can mint one from scratch), so demanding a deadline
        // there would reject honest links for no security gain. When one IS
        // present it is still honoured — never parsed and ignored.
        let live = "compme://setOverride?app=com.foo.bar&enabled=true";
        assert_eq!(
            parse_deep_link_with_trust_at(live, Some(&test_trusted_key()), TEST_NOW),
            Ok((
                OverrideCommand {
                    scope: Scope::App("com.foo.bar".into()),
                    action: OverrideAction::Enable,
                },
                LinkTrust::Unsigned,
            ))
        );
        assert!(parse_deep_link_at(&format!("{live}&exp={}", TEST_NOW + 1), TEST_NOW).is_ok());
        assert_eq!(
            parse_deep_link_at(&format!("{live}&exp={}", TEST_NOW - 1), TEST_NOW),
            Err(ParseError::ExpiredLink {
                exp: TEST_NOW - 1,
                now: TEST_NOW
            })
        );
    }

    #[test]
    fn the_public_entry_points_read_the_system_clock() {
        // The clock-injecting functions are private, so pin that the public
        // wrappers actually feed them wall-clock seconds: a hardcoded 0 (or a
        // monotonic uptime) would make a 1970 deadline look fresh.
        let key = test_trusted_key();
        let payload = "compme://setOverride?app=com.foo.bar&enabled=true";
        let long_expired = signed_url_expiring_at(payload, 1);
        match parse_deep_link_with_trust(&long_expired, Some(&key)) {
            Err(ParseError::ExpiredLink { exp: 1, now }) => {
                assert!(now > 1_700_000_000, "clock looks wrong: {now}");
            }
            other => panic!("a 1970 deadline must be expired, got {other:?}"),
        }
        // Same for the unsigned entry point (matched on the variant, not on a
        // second clock read, which could tick between the two calls).
        assert!(
            matches!(
                parse_deep_link(&format!("{payload}&exp=1")),
                Err(ParseError::ExpiredLink { exp: 1, .. })
            ),
            "parse_deep_link must expire a 1970 deadline too"
        );
        // A deadline in the year 2200 is still live on the same clock.
        let far_future = signed_url_expiring_at(payload, 7_258_118_400);
        assert_eq!(
            parse_deep_link_with_trust(&far_future, Some(&key)).map(|(_, trust)| trust),
            Ok(LinkTrust::Signed)
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
    fn a_tampered_signature_value_fails_verification() {
        // Companion to the tampered-PAYLOAD test: keep the signed payload byte
        // for byte, but flip a byte INSIDE a still-well-formed (128-hex, 64-byte)
        // signature. It is the right length and lexically valid hex, so it sails
        // past the MalformedSignature guard and reaches verify_strict — which
        // must reject it as InvalidSignature, never accept it. Pins the actual
        // cryptographic check, distinct from the length/charset rejection.
        use ed25519_dalek::Signer;
        let payload = "compme://setOverride?app=com.apple.TextEdit&enabled=true";
        let sig = test_signer().sign(payload.as_bytes());
        let mut bytes = sig.to_bytes();
        bytes[0] ^= 0x01; // flip one bit; length and hex-validity unchanged.
        let url = format!("{payload}&sig={}", encode_hex(&bytes));
        // Sanity: the tampered sig is still a well-formed 128-hex value, so the
        // failure below is the crypto check, not the malformed-value guard.
        assert_eq!(url[url.find("&sig=").unwrap() + 5..].len(), 128);
        assert_eq!(
            parse_deep_link_with_trust(&url, Some(&test_trusted_key())),
            Err(ParseError::InvalidSignature),
            "a well-formed but bit-flipped signature must fail verification"
        );
    }

    #[test]
    fn a_tampered_scope_fails_verification() {
        // Companion to the action-tamper test above: flipping the *scope*
        // (the targeted app/domain) after signing must also fail closed. The
        // signature covers the whole byte prefix, so swapping which app a
        // signed link targets — or switching an App scope to a Domain — has to
        // require a fresh signature, never silently re-target.
        let url = signed_url("compme://setOverride?app=com.apple.TextEdit&enabled=true");
        // (a) same scope kind, different target app.
        let retargeted = url.replace("com.apple.TextEdit", "com.evil.Keylogger");
        assert_eq!(
            parse_deep_link_with_trust(&retargeted, Some(&test_trusted_key())),
            Err(ParseError::InvalidSignature),
            "retargeting the signed app must fail verification"
        );
        // (b) flip the scope key itself: app → domain (valid domain so the
        // failure is the signature, not a parse error masking it).
        let domain = signed_url("compme://setOverride?app=example.com&enabled=true");
        let flipped = domain.replace("app=example.com", "domain=example.com");
        assert_eq!(
            parse_deep_link_with_trust(&flipped, Some(&test_trusted_key())),
            Err(ParseError::InvalidSignature),
            "flipping the scope kind (app→domain) must fail verification"
        );
    }

    #[test]
    fn split_trailing_signature_hands_the_verifier_the_exact_payload_prefix() {
        // The split point is security-critical: the verifier must receive
        // exactly the bytes before `&sig=` (everything signed) and nothing
        // after. Assert the boundary precisely against the private splitter.
        let payload = "compme://setOverride?app=com.apple.TextEdit&enabled=true";
        let url = signed_url(payload);
        let split = url.find("&sig=").expect("signed url has a sig param");
        let (got_payload, signature) = split_trailing_signature(&url)
            .expect("well-formed sig splits cleanly")
            .expect("a sig param is present");
        // The payload is the byte prefix up to (not including) `&sig=`.
        assert_eq!(got_payload, payload);
        assert_eq!(got_payload, &url[..split]);
        assert_eq!(got_payload.len(), split);
        // And the recovered signature is the one we appended (round-trips).
        use ed25519_dalek::Signer;
        let expected = test_signer().sign(payload.as_bytes());
        assert_eq!(signature, expected);
        // No `&sig=` at all → no split (the unsigned path).
        assert_eq!(split_trailing_signature(payload), Ok(None));
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
            &"a".repeat(127), // odd length (parse_hex odd-length guard)
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
    fn sign_bearing_hex_is_malformed_not_merely_unverifiable() {
        // `u8::from_str_radix` accepts a leading `+`, so "+1" decodes to 0x01
        // and a 128-char run of it used to parse as a well-formed signature
        // that then failed verification. It is not hex, so it must be rejected
        // before any crypto runs.
        let payload = "compme://setOverride?app=com.apple.TextEdit&enabled=true";
        let plus_sig = "+1".repeat(64);
        assert_eq!(plus_sig.len(), 128, "same length as a real hex signature");
        assert_eq!(
            parse_deep_link_with_trust(
                &format!("{payload}&sig={plus_sig}"),
                Some(&test_trusted_key()),
            ),
            Err(ParseError::MalformedSignature),
        );
    }

    #[test]
    fn trusted_key_rejects_sign_bearing_hex() {
        // Same leniency on the trust root: a key that is not hex must not be
        // silently adopted in a different spelling.
        assert!(TrustedKey::from_hex(&"+1".repeat(32)).is_none());
        // ...while a real 64-char hex key still decodes.
        assert!(TrustedKey::from_hex(&encode_hex(test_trusted_key().0.as_bytes())).is_some());
    }

    #[test]
    fn a_signature_that_is_not_the_final_parameter_is_misplaced() {
        // Anything after the sig value would be unsigned, attacker-appendable
        // bytes — including a second sig.
        let url = signed_url("compme://setOverride?app=com.apple.TextEdit&enabled=true");
        // `&exp=` is in the list because an expiry appended AFTER the signature
        // would be unsigned: the deadline only means something inside the
        // signed prefix, so a trailing one must fail closed, not extend a link.
        for appended in ["&excluded=true", "&sig=00", "&x=1", "&exp=9999999999"] {
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

    #[test]
    fn trusted_key_from_hex_trims_surrounding_whitespace() {
        // from_hex trims before decoding (line ~210): a key pasted from a
        // config file or shell arrives with a trailing newline / padding
        // spaces, and it must parse to the SAME key as the bare hex — not
        // fail closed on the padding.
        let hex = encode_hex(test_signer().verifying_key().as_bytes());
        let bare = TrustedKey::from_hex(&hex).expect("bare hex parses");
        let padded = TrustedKey::from_hex(&format!("  {hex}\n")).expect("padded hex parses");
        assert_eq!(
            padded.0.to_bytes(),
            bare.0.to_bytes(),
            "whitespace-padded hex must decode to the identical key"
        );
    }

    #[test]
    fn display_messages_carry_the_offending_token() {
        // The host surfaces these Display strings to the user, so each
        // value-carrying variant must echo the offending token verbatim. Pins the
        // Display arms against a refactor that drops the interpolated value.
        assert!(ParseError::UnknownParam("instructions".into())
            .to_string()
            .contains("instructions"));
        assert!(ParseError::InvalidValue("maybe".into())
            .to_string()
            .contains("maybe"));
        assert!(ParseError::UnknownCommand("setPreference".into())
            .to_string()
            .contains("setPreference"));
    }

    #[test]
    fn from_hex_rejects_well_formed_hex_that_is_not_a_valid_point() {
        // 64 hex chars decode cleanly to 32 bytes (parse_hex + try_into both
        // succeed), but these bytes encode a compressed point whose y-coordinate
        // does not decompress to a curve point — VerifyingKey::from_bytes rejects
        // it, so from_hex fails closed despite lexically well-formed hex of the
        // right length. (NB: the all-zero point IS accepted by ed25519-dalek
        // 3.x's from_bytes — a small-order point only verify_strict rejects — so
        // this pins the from_bytes decompression failure, not a small-order one.)
        let well_formed_but_not_a_point = format!("{}ff", "00".repeat(31));
        assert_eq!(well_formed_but_not_a_point.len(), 64);
        assert!(TrustedKey::from_hex(&well_formed_but_not_a_point).is_none());
    }

    #[test]
    fn signed_link_rejects_scope_tampering_after_signature() {
        let original = signed_url("compme://setOverride?app=com.apple.TextEdit&enabled=true");
        let cases = [
            original.replace("app=com.apple.TextEdit", "app=com.apple.Notes"),
            original.replace("enabled=true", "enabled=false"),
            original.replace("setOverride?app=", "setOverride?domain="),
        ];

        for tampered in cases {
            assert_eq!(
                parse_deep_link_with_trust(&tampered, Some(&test_trusted_key())),
                Err(ParseError::InvalidSignature)
            );
        }
    }

    #[test]
    fn verify_strict_rejects_low_order_key_that_plain_verify_would_accept() {
        // parse_deep_link_with_trust verifies with `verify_strict` (lib.rs
        // ~L240), NOT `verify`. The distinction is load-bearing and this is the
        // ONLY test that pins it: every other signed-link test uses a proper key
        // + honest signature, so a silent `verify_strict`→`verify` downgrade
        // would sail through them. This test FAILS if that downgrade happens.
        //
        // It uses a sourced, behavior-distinguishing CCTV test vector (the same
        // corpus ed25519-dalek 3.x uses in tests/validation_criteria.rs):
        //   C2SP/CCTV ed25519vectors @ commit 5ea85644bd03..., vector #3
        //   flags: low_order_A, low_order_component_A, low_order_component_R
        //   key  = 00..00 (the all-zero / low-order point A)
        //   msg  = "ed25519vectors 3"
        //   sig  = 36684ea9.. (a genuine signature, NOT all-zero)
        // ed25519-dalek's own criteria allow `low_order_A` for verify() but NOT
        // for verify_strict() (VERIFY_ALLOWED_EDGECASES vs
        // VERIFY_STRICT_ALLOWED_EDGECASES). Empirically against the pinned
        // ed25519-dalek 3.0.0: verify() ACCEPTS this triple, verify_strict()
        // REJECTS it. So plain `verify` would return Ok(_) here while
        // `verify_strict` returns Err → InvalidSignature.
        //
        // The signed payload must equal the vector's message byte-for-byte, so
        // the deep-link prefix before `&sig=` IS that message. verify_strict runs
        // before parse_deep_link, so the verifier sees "ed25519vectors 3" and
        // rejects it; the payload never needs to be a parseable command.
        const VECTOR_KEY_HEX: &str =
            "0000000000000000000000000000000000000000000000000000000000000000";
        const VECTOR_MSG: &str = "ed25519vectors 3";
        const VECTOR_SIG_HEX: &str = concat!(
            "36684ea91032ba5b1dbab2d02f4debc74c3327f2b3802e2e4d371aa42b12b56b",
            "05ba9a796274d80437afa36f1236563f2f3b0aa84cecddc3d20914615ba4fe02",
        );

        let key = TrustedKey::from_hex(VECTOR_KEY_HEX)
            .expect("low-order point decodes (it must reach the verifier, not fail at key decode)");
        let url = format!("{VECTOR_MSG}&sig={VECTOR_SIG_HEX}");
        assert_eq!(url[url.find("&sig=").unwrap() + 5..].len(), 128);
        assert_eq!(
            parse_deep_link_with_trust(&url, Some(&key)),
            Err(ParseError::InvalidSignature),
            "verify_strict must reject this low-order-A vector; plain `verify` would ACCEPT it, \
             so this assertion fails if the production call is downgraded to verify()"
        );
    }
}
