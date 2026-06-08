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

    /// The slider position `0..=5`.
    pub fn stop(self) -> u8 {
        match self {
            Strength::Off => 0,
            Strength::Stop1 => 1,
            Strength::Stop2 => 2,
            Strength::Stop3 => 3,
            Strength::Stop4 => 4,
            Strength::Max => 5,
        }
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
    fn is_empty(&self) -> bool {
        self.name.trim().is_empty() && self.email.trim().is_empty()
    }

    fn line(&self) -> Option<String> {
        if self.is_empty() {
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
            if let Some(text) = self.per_domain.get(domain) {
                let text = text.trim();
                if !text.is_empty() {
                    sections.push(text);
                }
            }
        }
        sections.join("\n")
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
            out.push_str(truncate_chars(&instructions, MAX_INSTRUCTION_CHARS));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> PersonalizationProfile {
        PersonalizationProfile {
            global_instructions: "Write concisely.".into(),
            strength: Strength::Stop3,
            ..Default::default()
        }
    }

    #[test]
    fn from_stop_clamps_out_of_range() {
        assert_eq!(Strength::from_stop(0), Strength::Off);
        assert_eq!(Strength::from_stop(5), Strength::Max);
        assert_eq!(Strength::from_stop(99), Strength::Max);
    }

    #[test]
    fn all_six_stops_are_distinct() {
        let stops: Vec<u8> = Strength::STOPS.iter().map(|s| s.stop()).collect();
        assert_eq!(stops, vec![0, 1, 2, 3, 4, 5]);
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
