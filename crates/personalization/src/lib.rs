//! Prompt-based personalization (design spec §6): custom instructions
//! (global + per-app + per-domain), a 6-stop strength slider, and sender
//! identity, templated into a steering preamble that is prepended to the
//! completion prompt. Pure and dependency-free — no ML, no I/O.
//!
//! Scope (§15 D2/D15, Project Scope): the strength slider has **6 stops with
//! full reach for every user — no tier caps**. Cotypist's Free/Plus/Pro caps are
//! paywall artifacts we deliberately do not clone.

use std::collections::HashMap;

/// Delimiter fencing the free-text instruction block so user/domain text (which
/// can arrive from web-driven `setOverride` deep links — design spec §13) cannot
/// dissolve the surrounding directive frame.
const INSTRUCTION_FENCE: &str = "\"\"\"";

/// Upper bound on instruction characters folded into a single preamble. The
/// spec guides "a few hundred words"; this caps abuse/runaway config well above
/// that while keeping the prompt prefill short.
const MAX_INSTRUCTION_CHARS: usize = 2000;

/// Truncate to at most `max` characters on a char boundary (never mid-scalar).
fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// Cap to `MAX_INSTRUCTION_CHARS` then neutralize any `"""` fence inside. The
/// cap is applied BEFORE fence-escaping, so the returned block can run a few
/// chars longer than the cap (each escaped fence adds 2) — fine, since the cap
/// is a runaway-abuse guard, not an exact output-length contract.
fn instruction_block_text(s: &str) -> String {
    let mut text = truncate_chars(s, MAX_INSTRUCTION_CHARS).to_string();
    // Replace to a fixed point: a single pass over a run of 3k+2 quotes (e.g.
    // `"""""`) leaves the replacement's trailing quote adjacent to the leftover
    // pair, reconstructing a live fence. Each pass shortens the longest quote
    // run, so this terminates.
    while text.contains(INSTRUCTION_FENCE) {
        text = text.replace(INSTRUCTION_FENCE, "\" \" \"");
    }
    text
}

/// Personalization strength: a 6-stop slider from `Off` to `Max`. Only the
/// endpoints are labelled in the UI; the intermediate stops scale how forcefully
/// the custom instructions steer the completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Strength {
    Off,
    Stop1,
    Stop2,
    // Default to a middle stop: personalization on, balanced steer.
    #[default]
    Stop3,
    Stop4,
    Max,
}

impl Strength {
    /// The six stops in order, `Off` (0) … `Max` (5).
    pub const STOPS: [Strength; 6] = [
        Strength::Off,
        Strength::Stop1,
        Strength::Stop2,
        Strength::Stop3,
        Strength::Stop4,
        Strength::Max,
    ];

    /// Map a slider tick `0..=5` to a stop, clamping out-of-range values to the
    /// nearest endpoint. Full reach for every user — there is no tier cap.
    pub fn from_stop(stop: u8) -> Strength {
        let index = (stop as usize).min(Strength::STOPS.len() - 1);
        Strength::STOPS[index]
    }

    /// The directive phrase whose forcefulness scales with the stop. `Off` has
    /// no directive (personalization disabled).
    fn directive(self) -> &'static str {
        match self {
            Strength::Off => "",
            Strength::Stop1 => "You may consider the following preferences",
            Strength::Stop2 => "Take the following preferences into account",
            Strength::Stop3 => "Follow the following preferences",
            Strength::Stop4 => "Closely follow the following preferences",
            Strength::Max => "Strictly follow the following preferences above all else",
        }
    }
}

/// The user's name/email, fed into the prompt for signature/contact awareness.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SenderIdentity {
    pub name: String,
    pub email: String,
}

impl SenderIdentity {
    fn line(&self) -> Option<String> {
        if self.name.trim().is_empty() && self.email.trim().is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !self.name.trim().is_empty() {
            parts.push(format!("name {}", self.name.trim()));
        }
        if !self.email.trim().is_empty() {
            parts.push(format!("email {}", self.email.trim()));
        }
        Some(format!("The writer's {}.", parts.join(", ")))
    }
}

/// The full personalization profile. `per_app` is keyed by application id
/// (bundle id) and `per_domain` by website domain; both **supplement** the
/// global instructions rather than replacing them (design spec §6).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PersonalizationProfile {
    pub global_instructions: String,
    pub per_app: HashMap<String, String>,
    pub per_domain: HashMap<String, String>,
    pub sender: SenderIdentity,
    pub strength: Strength,
}

impl PersonalizationProfile {
    /// Merge the instructions that apply to a focus context, in steering order:
    /// global, then the per-app supplement, then the per-domain supplement. Empty
    /// sections are skipped; surrounding whitespace is trimmed.
    pub fn resolve_instructions(&self, app: Option<&str>, domain: Option<&str>) -> String {
        let mut sections: Vec<&str> = Vec::new();
        let global = self.global_instructions.trim();
        if !global.is_empty() {
            sections.push(global);
        }
        if let Some(app) = app {
            if let Some(text) = self.per_app.get(app) {
                let text = text.trim();
                if !text.is_empty() {
                    sections.push(text);
                }
            }
        }
        if let Some(domain) = domain {
            if let Some(text) = self.per_domain_instruction(domain) {
                let text = text.trim();
                if !text.is_empty() {
                    sections.push(text);
                }
            }
        }
        sections.join("\n")
    }

    /// Resolve the per-domain instruction for `host`, matching the exact host or
    /// the most-specific parent-domain rule on a dot boundary (a `google.com`
    /// rule applies to `www.google.com`, but never to `evilgoogle.com`). This
    /// mirrors the subdomain-aware matching `prefs` uses for domain exclusions,
    /// so a user who configures both surfaces sees consistent scoping. The
    /// longest matching rule wins, making the choice deterministic.
    ///
    /// `host` is folded to lowercase here so the lookup is self-contained — like
    /// `prefs`, it does not depend on every caller pre-lowercasing. Keys are
    /// expected to be lowercased at insertion (the run loop does this for
    /// config-sourced domains).
    ///
    /// The `max_by_key(rule.len())` tie-break over an unordered `HashMap` is
    /// deterministic because the matching rules can never collide on length: every
    /// rule that matches `host` is one of its dot-boundary suffixes (or `host`
    /// itself), and two distinct suffixes of the same string always have distinct
    /// lengths. So the maximum is unique — no two equal-length rules can both
    /// match one host, and the HashMap iteration order is irrelevant.
    fn per_domain_instruction(&self, host: &str) -> Option<&String> {
        let host = host.to_ascii_lowercase();
        self.per_domain
            .iter()
            .filter(|(rule, _)| host_matches_domain_rule(&host, rule))
            .max_by_key(|(rule, _)| rule.len())
            .map(|(_, text)| text)
    }

    /// Build the steering preamble prepended to the completion prompt. Returns an
    /// empty string when strength is `Off`, or when there are no instructions and
    /// no sender identity to steer with.
    pub fn build_preamble(&self, app: Option<&str>, domain: Option<&str>) -> String {
        if self.strength == Strength::Off {
            return String::new();
        }
        let instructions = self.resolve_instructions(app, domain);
        let sender_line = self.sender.line();
        if instructions.is_empty() && sender_line.is_none() {
            return String::new();
        }

        let mut out = String::new();
        // The directive only introduces actual instructions; a sender-only
        // preamble must not promise "preferences" that aren't there.
        if !instructions.is_empty() {
            out.push_str(self.strength.directive());
            out.push_str(":\n");
            out.push_str(INSTRUCTION_FENCE);
            out.push('\n');
            out.push_str(&instruction_block_text(&instructions));
            out.push('\n');
            out.push_str(INSTRUCTION_FENCE);
            out.push('\n');
        }
        if let Some(line) = sender_line {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

/// True when `host` is matched by a domain `rule`: either an exact match or a
/// subdomain on a dot boundary (`www.google.com` matches `google.com`, but
/// `evilgoogle.com` does not). Kept in sync with the prefs domain-exclusion
/// matcher so per-domain steering and per-domain exclusions scope alike.
fn host_matches_domain_rule(host: &str, rule: &str) -> bool {
    if host == rule {
        return true;
    }
    host.strip_suffix(rule)
        .is_some_and(|prefix| prefix.ends_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single shared decision table for the two independent
    /// `host_matches_domain_rule` matchers (`personalization` here, `prefs` in its
    /// own crate). Both modules document that they "must never drift apart"; this
    /// table is duplicated verbatim into `prefs::tests` so an edit to EITHER
    /// matcher that changes any decision fails one crate's test. The expected
    /// column is the decision BOTH matchers currently make (verified empirically),
    /// so the table is the contract, not either implementation.
    ///
    /// NOTE: these matchers take ALREADY-lowercased input (callers fold case
    /// upstream — `per_domain_instruction` / `should_suggest`). So a mixed-case
    /// host like `GOOGLE.COM` does NOT match here: that is the raw-matcher
    /// contract, and both agree on it.
    pub(crate) const DOMAIN_MATCHER_SHARED_CASES: &[(&str, &str, bool)] = &[
        ("www.google.com", "google.com", true),
        ("evilgoogle.com", "google.com", false),
        ("google.com.evil.com", "google.com", false),
        ("google.com", "google.com", true),
        // Case: raw matcher does not fold case (callers do), so this misses.
        ("GOOGLE.COM", "google.com", false),
        // Empty host never matches a non-empty rule.
        ("", "google.com", false),
        // Empty rule: both agree it does not match a non-empty host.
        ("google.com", "", false),
    ];

    #[test]
    fn domain_matcher_agrees_on_shared_case_table() {
        for &(host, rule, expected) in DOMAIN_MATCHER_SHARED_CASES {
            assert_eq!(
                host_matches_domain_rule(host, rule),
                expected,
                "personalization matcher disagrees on ({host:?}, {rule:?})"
            );
        }
    }

    #[test]
    fn truncate_chars_keeps_exactly_max_and_cuts_on_a_char_boundary() {
        // Boundary cases the over-cap tests skip: len == max (nth(max) is None →
        // unchanged) and len == max+1 (nth(max) is Some → cut to exactly max).
        // Multibyte input proves the cut lands on a char boundary, never
        // mid-scalar (which would panic on the slice).
        assert_eq!(truncate_chars("abc", 3), "abc"); // len == max → unchanged
        assert_eq!(truncate_chars("abcd", 3), "abc"); // len == max+1 → cut to max
        assert_eq!(truncate_chars("aé", 2), "aé"); // 2 chars == max → unchanged
        assert_eq!(truncate_chars("aéx", 2), "aé"); // cut after é, on a boundary
    }

    fn profile() -> PersonalizationProfile {
        PersonalizationProfile {
            global_instructions: "Write concisely.".into(),
            strength: Strength::Stop3,
            ..Default::default()
        }
    }

    #[test]
    fn each_stop_maps_to_its_exact_directive_phrase() {
        // Pin the EXACT directive literal for every stop. Off has no directive;
        // the five active stops scale forcefulness. A reworded phrase (or two
        // stops collapsing to the same wording) would break the steer contract
        // these literals encode, so assert each verbatim.
        assert_eq!(Strength::Off.directive(), "");
        assert_eq!(
            Strength::Stop1.directive(),
            "You may consider the following preferences"
        );
        assert_eq!(
            Strength::Stop2.directive(),
            "Take the following preferences into account"
        );
        assert_eq!(
            Strength::Stop3.directive(),
            "Follow the following preferences"
        );
        assert_eq!(
            Strength::Stop4.directive(),
            "Closely follow the following preferences"
        );
        assert_eq!(
            Strength::Max.directive(),
            "Strictly follow the following preferences above all else"
        );
    }

    #[test]
    fn per_app_lookup_is_case_sensitive_unlike_domain() {
        // per_app is a plain HashMap::get keyed by bundle id — an exact,
        // case-SENSITIVE match. per_domain, by contrast, folds the host to
        // lowercase before matching. Pin both halves of that contrast so the
        // two lookups never drift into the same casing policy.
        let mut p = profile();
        p.per_app
            .insert("com.apple.Mail".into(), "Be formal.".into());
        p.per_domain.insert("google.com".into(), "Be terse.".into());

        // Exact-case per-app key matches and supplements the global.
        let exact = p.resolve_instructions(Some("com.apple.Mail"), None);
        assert!(exact.contains("Be formal."), "exact-case app key matches");

        // Wrong-case per-app key does NOT match → falls back to global only.
        assert_eq!(
            p.resolve_instructions(Some("com.apple.mail"), None),
            "Write concisely.",
            "case-folded app key misses, leaving only the global"
        );

        // Contrast: a wrong-case domain still matches because the host is folded.
        assert!(
            p.resolve_instructions(None, Some("GOOGLE.COM"))
                .contains("Be terse."),
            "domain lookup is case-insensitive, unlike per-app"
        );
    }

    #[test]
    fn default_strength_is_the_middle_stop_so_personalization_is_on_by_default() {
        // The `#[default]` on Strength is Stop3 (not Off): a freshly-constructed
        // profile has personalization ON at a balanced steer. This is a product
        // decision (§6/§16 — "Default to a middle stop"), not incidental enum
        // ordering. Pin both the enum default and its observable consequence — a
        // default-strength profile carrying a global instruction produces a
        // non-empty, Stop3-directive-bearing preamble; if the default silently
        // moved to Off the preamble would be empty, and a move to any other stop
        // would change the directive wording. Every other test sets `strength`
        // explicitly, so this default was otherwise unpinned.
        assert_eq!(Strength::default(), Strength::Stop3);

        let p = PersonalizationProfile {
            global_instructions: "Write concisely.".into(),
            ..Default::default() // strength defaults to Stop3
        };
        let preamble = p.build_preamble(None, None);
        assert!(
            !preamble.is_empty(),
            "default strength must steer (not Off): {preamble:?}"
        );
        assert!(
            preamble.contains("Follow the following preferences"),
            "default steer must be the Stop3 directive: {preamble:?}"
        );
    }

    #[test]
    fn from_stop_clamps_out_of_range() {
        assert_eq!(Strength::from_stop(0), Strength::Off);
        assert_eq!(Strength::from_stop(5), Strength::Max);
        assert_eq!(Strength::from_stop(99), Strength::Max);
    }

    #[test]
    fn all_six_stops_are_distinct() {
        let stops: Vec<Strength> = (0u8..6).map(Strength::from_stop).collect();
        assert_eq!(stops, Strength::STOPS.to_vec());
    }

    #[test]
    fn off_yields_empty_preamble() {
        let mut p = profile();
        p.strength = Strength::Off;
        assert_eq!(p.build_preamble(None, None), "");
    }

    #[test]
    fn no_instructions_and_no_sender_yields_empty_preamble() {
        let p = PersonalizationProfile {
            strength: Strength::Max,
            ..Default::default()
        };
        assert_eq!(p.build_preamble(None, None), "");
    }

    #[test]
    fn higher_strength_produces_a_stronger_directive() {
        let mut p = profile();
        p.strength = Strength::Stop1;
        let gentle = p.build_preamble(None, None);
        p.strength = Strength::Max;
        let max = p.build_preamble(None, None);

        // Both carry the instruction text, but the directive differs and the Max
        // directive is observably stronger.
        assert!(gentle.contains("Write concisely."));
        assert!(max.contains("Write concisely."));
        assert_ne!(gentle, max);
        assert!(max.contains("Strictly"));
        assert!(!gentle.contains("Strictly"));
    }

    #[test]
    fn strength_stops_are_monotonic_full_reach_and_roundtrip() {
        // The slider is a strictly-increasing 6-stop scale 0..=5 with no tier cap
        // (§6 / §16: full reach for every user). Pin the ordering contract and the
        // tick↔stop round-trip so an intermediate stop can't silently reorder.
        for (i, stop) in Strength::STOPS.iter().enumerate() {
            assert_eq!(Strength::from_stop(i as u8), *stop);
        }
        // Every stop above Off yields a non-empty, distinct directive (the steer
        // is observable and never collapses two stops to the same forcefulness).
        let directives: Vec<&str> = Strength::STOPS
            .iter()
            .filter(|s| **s != Strength::Off)
            .map(|s| s.directive())
            .collect();
        assert!(directives.iter().all(|d| !d.is_empty()));
        for i in 0..directives.len() {
            for j in (i + 1)..directives.len() {
                assert_ne!(directives[i], directives[j]);
            }
        }
    }

    #[test]
    fn resolve_merges_global_then_app_then_domain() {
        let mut p = profile();
        p.per_app
            .insert("com.apple.TextEdit".into(), "Use plain text.".into());
        p.per_domain
            .insert("docs.google.com".into(), "Match the doc tone.".into());

        let merged = p.resolve_instructions(Some("com.apple.TextEdit"), Some("docs.google.com"));
        assert_eq!(
            merged,
            "Write concisely.\nUse plain text.\nMatch the doc tone."
        );
    }

    #[test]
    fn resolve_merges_global_then_domain_when_no_app_context() {
        // The browser-focus path (§6/§13 setOverride): a per-domain override with
        // NO per-app context. Pins that the per_domain branch fires under
        // app=None and supplements global in global→domain order — every other
        // domain-merge test also passes a Some(app), so this path was unpinned.
        let mut p = profile();
        p.per_domain
            .insert("docs.google.com".into(), "Match the doc tone.".into());
        assert_eq!(
            p.resolve_instructions(None, Some("docs.google.com")),
            "Write concisely.\nMatch the doc tone."
        );
    }

    #[test]
    fn resolve_uses_domain_alone_when_global_is_empty() {
        // Domain section as the sole/leading section: empty global, no app — the
        // `sections.join` path with per_domain at index 0 (today's tests always
        // have a non-empty global leading).
        let mut p = PersonalizationProfile {
            strength: Strength::Stop3,
            ..Default::default()
        };
        p.per_domain
            .insert("example.com".into(), "Be terse.".into());
        assert_eq!(
            p.resolve_instructions(None, Some("example.com")),
            "Be terse."
        );
        // An unmatched domain with empty global yields nothing.
        assert_eq!(p.resolve_instructions(None, Some("other.com")), "");
    }

    #[test]
    fn resolve_matches_subdomain_against_parent_domain_rule() {
        // A `google.com` rule applies to its subdomains (dot-boundary match),
        // mirroring prefs domain exclusions, but never to a look-alike host.
        let mut p = PersonalizationProfile {
            strength: Strength::Stop3,
            ..Default::default()
        };
        p.per_domain.insert("google.com".into(), "Be terse.".into());
        assert_eq!(
            p.resolve_instructions(None, Some("google.com")),
            "Be terse."
        );
        assert_eq!(
            p.resolve_instructions(None, Some("www.google.com")),
            "Be terse."
        );
        assert_eq!(
            p.resolve_instructions(None, Some("docs.google.com")),
            "Be terse."
        );
        // Look-alike host must NOT match on a non-dot boundary.
        assert_eq!(p.resolve_instructions(None, Some("evilgoogle.com")), "");
        // Nor when the rule appears as a non-boundary suffix substring inside a
        // different registrable domain (the classic `google.com.evil.com`
        // over-match). `prefs::host_matches_domain_rule` pins this exact
        // negative; personalization's independent matcher must agree so the two
        // never drift apart.
        assert_eq!(
            p.resolve_instructions(None, Some("google.com.evil.com")),
            ""
        );
    }

    #[test]
    fn resolve_folds_host_case_so_lookup_is_self_contained() {
        // The matcher lowercases the host itself (like prefs), so a mixed-case
        // host still resolves its lowercased rule even if a caller forgot to
        // pre-lowercase — guarding the steering-scope contract against drift.
        let mut p = PersonalizationProfile {
            strength: Strength::Stop3,
            ..Default::default()
        };
        p.per_domain.insert("google.com".into(), "Be terse.".into());
        assert_eq!(
            p.resolve_instructions(None, Some("WWW.Google.COM")),
            "Be terse."
        );
        assert_eq!(
            p.resolve_instructions(None, Some("Google.com")),
            "Be terse."
        );
    }

    #[test]
    fn resolve_prefers_most_specific_domain_rule() {
        // When both a parent and a more-specific rule match, the longest
        // (most specific) rule wins deterministically.
        let mut p = PersonalizationProfile {
            strength: Strength::Stop3,
            ..Default::default()
        };
        p.per_domain
            .insert("google.com".into(), "Parent rule.".into());
        p.per_domain
            .insert("docs.google.com".into(), "Doc rule.".into());
        assert_eq!(
            p.resolve_instructions(None, Some("docs.google.com")),
            "Doc rule."
        );
        assert_eq!(
            p.resolve_instructions(None, Some("mail.google.com")),
            "Parent rule."
        );
    }

    #[test]
    fn resolve_skips_whitespace_only_per_app_and_per_domain_sections() {
        // The inner trim-to-empty guard drops a per-app or per-domain section whose
        // text is whitespace-only, so a blank section is never pushed and never
        // joined in — the resolved output is exactly the non-empty global, with no
        // dangling blank line or duplicated separator.
        let mut p = profile();
        p.per_app.insert("app".into(), "   ".into());
        p.per_domain.insert("ex.com".into(), "\n\t".into());

        let resolved = p.resolve_instructions(Some("app"), Some("ex.com"));
        assert_eq!(
            resolved, "Write concisely.",
            "whitespace-only sections are skipped, leaving only the global: {resolved:?}"
        );
        // No blank/duplicated section leaked in via the join separator.
        assert!(!resolved.contains("\n\n"));
        assert!(!resolved.ends_with('\n'));
    }

    #[test]
    fn per_app_supplements_rather_than_replaces_global() {
        let mut p = profile();
        p.per_app
            .insert("com.apple.mail".into(), "Be formal.".into());

        let merged = p.resolve_instructions(Some("com.apple.mail"), None);
        assert!(merged.contains("Write concisely."));
        assert!(merged.contains("Be formal."));
    }

    #[test]
    fn unmatched_app_falls_back_to_global_only() {
        let mut p = profile();
        p.per_app
            .insert("com.apple.mail".into(), "Be formal.".into());

        assert_eq!(
            p.resolve_instructions(Some("com.other.app"), None),
            "Write concisely."
        );
    }

    #[test]
    fn sender_identity_is_included_in_the_preamble() {
        let mut p = profile();
        p.sender = SenderIdentity {
            name: "Ada".into(),
            email: "ada@example.com".into(),
        };
        let preamble = p.build_preamble(None, None);
        assert!(preamble.contains("Ada"));
        assert!(preamble.contains("ada@example.com"));
    }

    #[test]
    fn sender_with_only_name_still_renders() {
        let sender = SenderIdentity {
            name: "Grace".into(),
            email: "  ".into(),
        };
        assert_eq!(sender.line(), Some("The writer's name Grace.".into()));
    }

    #[test]
    fn sender_with_only_email_still_renders() {
        let sender = SenderIdentity {
            name: "  ".into(),
            email: "grace@example.com".into(),
        };
        assert_eq!(
            sender.line(),
            Some("The writer's email grace@example.com.".into())
        );
    }

    #[test]
    fn sender_only_preamble_has_no_dangling_preferences_directive() {
        // No instructions, only a sender → the preamble must not promise
        // "preferences" that aren't there (review finding A).
        let p = PersonalizationProfile {
            strength: Strength::Max,
            sender: SenderIdentity {
                name: "Ada".into(),
                email: String::new(),
            },
            ..Default::default()
        };
        let preamble = p.build_preamble(None, None);
        assert!(preamble.contains("Ada"), "sender still rendered");
        assert!(
            !preamble.to_lowercase().contains("preferences"),
            "no preferences directive when there are no instructions: {preamble:?}"
        );
    }

    #[test]
    fn every_stop_produces_a_distinct_preamble() {
        // Each of the 6 slider positions must steer observably differently
        // (review finding D): pin pairwise distinctness, not just endpoints.
        let mut p = profile();
        let preambles: Vec<String> = Strength::STOPS
            .iter()
            .map(|&s| {
                p.strength = s;
                p.build_preamble(None, None)
            })
            .collect();
        for i in 0..preambles.len() {
            for j in (i + 1)..preambles.len() {
                assert_ne!(
                    preambles[i], preambles[j],
                    "stops {i} and {j} collapsed to identical preambles"
                );
            }
        }
    }

    #[test]
    fn build_preamble_renders_all_three_sections_in_global_app_domain_order() {
        // resolve_instructions is tested for the three-way merge, but the
        // assembled build_preamble (directive + fenced block) was never pinned
        // with all three sections present at once. Prove the fenced block carries
        // global, per-app and per-domain text, in global→app→domain order, for a
        // single focus context.
        let mut p = profile();
        p.per_app
            .insert("com.apple.TextEdit".into(), "Use plain text.".into());
        p.per_domain
            .insert("docs.google.com".into(), "Match the doc tone.".into());

        let preamble = p.build_preamble(Some("com.apple.TextEdit"), Some("docs.google.com"));

        let global_at = preamble.find("Write concisely.").expect("global present");
        let app_at = preamble.find("Use plain text.").expect("per-app present");
        let domain_at = preamble
            .find("Match the doc tone.")
            .expect("per-domain present");
        assert!(
            global_at < app_at && app_at < domain_at,
            "sections must be global→app→domain ordered: {preamble:?}"
        );
        // All three live inside the fenced instruction block (open+close fence).
        assert!(
            preamble.matches(INSTRUCTION_FENCE).count() >= 2,
            "all three sections fenced together: {preamble:?}"
        );
    }

    #[test]
    fn instructions_are_fenced_in_the_preamble() {
        // Free-text user/domain instructions are fenced so they cannot dissolve
        // the directive frame (review finding C — per-domain text can arrive from
        // web-driven setOverride deep links).
        let p = profile();
        let preamble = p.build_preamble(None, None);
        assert!(
            preamble.matches(INSTRUCTION_FENCE).count() >= 2,
            "instructions wrapped in an open+close fence: {preamble:?}"
        );
    }

    #[test]
    fn embedded_instruction_fences_are_neutralized() {
        let mut p = profile();
        p.global_instructions = "Prefer \"\"\" break out \"\"\" wording.".into();
        let preamble = p.build_preamble(None, None);

        assert_eq!(
            preamble.matches(INSTRUCTION_FENCE).count(),
            2,
            "only the wrapper fences should remain: {preamble:?}"
        );
        assert!(preamble.contains("\" \" \" break out \" \" \""));
    }

    #[test]
    fn consecutive_quote_runs_cannot_reconstruct_a_fence() {
        // A run of 3k+2 quotes (5, 8, ...) defeats a single-pass replace: the
        // replacement's trailing quote plus the leftover pair splice a live
        // fence back together. The fixed-point loop must neutralize every run.
        for n in 4..=9 {
            let mut p = profile();
            p.global_instructions = format!("break {} out", "\"".repeat(n));
            let preamble = p.build_preamble(None, None);
            assert_eq!(
                preamble.matches(INSTRUCTION_FENCE).count(),
                2,
                "quote run of {n} reconstructed a fence: {preamble:?}"
            );
        }
    }

    #[test]
    fn overlong_multibyte_instructions_truncate_on_a_char_boundary() {
        // Multibyte prose ending in a fence-like sequence, well over the cap. The
        // truncation must (a) not panic mid-scalar, (b) keep the output within the
        // documented cap, and (c) yield a valid UTF-8 String (proving char-boundary
        // safety: a byte-index split through a multibyte char would have panicked).
        let mut p = profile();
        // "日本語" is 3 chars / 9 bytes each repeat; 1000 repeats = 3000 chars,
        // ending in the """ fence convention.
        p.global_instructions = format!("{}{}", "日本語".repeat(1000), INSTRUCTION_FENCE);
        let preamble = p.build_preamble(None, None);

        // (a) reaching here means no panic.
        // (b) bounded by the instruction cap (plus directive/fence overhead, same
        // slack the sibling cap test uses).
        assert!(
            preamble.chars().count() <= MAX_INSTRUCTION_CHARS + 200,
            "preamble bounded by the instruction cap, got {} chars",
            preamble.chars().count()
        );
        // (c) valid UTF-8 with no replacement char from a mid-scalar split.
        assert!(!preamble.contains('\u{FFFD}'));
        assert!(preamble.contains("日本語"));
        // The wrapper fences are still present and the embedded fence neutralized
        // (if any survived truncation, it would have been rewritten).
        assert!(preamble.matches(INSTRUCTION_FENCE).count() >= 2);
    }

    #[test]
    fn overlong_instructions_are_capped() {
        let mut p = profile();
        p.global_instructions = "word ".repeat(2000); // ~10k chars
        let preamble = p.build_preamble(None, None);
        assert!(
            preamble.chars().count() <= MAX_INSTRUCTION_CHARS + 200,
            "preamble bounded by the instruction cap, got {} chars",
            preamble.chars().count()
        );
    }
}
