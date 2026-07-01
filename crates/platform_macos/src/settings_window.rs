//! The S2 settings window shell (A3 settings plan, tick 1: skeleton).
//!
//! Same contract as the tray: render-only AppKit glue, no policy. The run
//! loop opens it via a tray flag and polls visibility each heartbeat so the
//! activation-policy dance stays correct however the window closes.
//!
//! LSUIElement apps (our Info.plist) run as `Accessory`: a window shown
//! without promoting the activation policy to `Regular` never becomes key.
//! `set_visible(true)` promotes; the visibility POLL (not a window delegate)
//! detects any close — red button included — and demotes back to
//! `Accessory`, so no Dock icon is left stranded. AppKit/FFI glue: build-
//! and live-verified, not unit-tested (tray convention); the policy-edge
//! decision is the unit-tested pure part.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSButtonType,
    NSControlStateValueOn, NSEvent, NSFont, NSPopUpButton, NSResponder, NSSwitch, NSTabView,
    NSTabViewItem, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use platform::PlatformError;

/// A requested accept-key rebind: `(word, full)` as `(keycode, Carbon
/// modifier mask)` pairs, `None` = reset that role to its default. Slice 2's
/// recorder captures the modifier mask (`event.modifierFlags()`); a bare key
/// carries mask 0.
pub type RebindRequest = (Option<(i64, u32)>, Option<(i64, u32)>);

/// Which accept role a recorder field rebinds (recorder 5b slice 4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecorderRole {
    Word,
    Full,
}

/// What one captured keyDown does to a recording field. Pure half of the
/// KeyRecorderField; the AppKit subclass is the LOOK-verified consumer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecordDecision {
    /// Esc: leave recording, revert to the idle label. Esc is a fixed key
    /// AND the cancel gesture — cancel wins (match-arm ordering is the
    /// contract, pinned by test).
    Cancel,
    /// Down (the fixed cycle key): consumed silently, stay recording.
    RejectFixed,
    /// Captured key == the OTHER role's current key: would collide at
    /// `from_accept_keys` — stay recording, show "In use".
    RejectCollision,
    /// Park the request, exit recording.
    Accept,
}

/// Decide what a recording field does with a captured `(keycode, mask)`, given
/// the OTHER role's currently registered `(keycode, mask)`. Esc and Down stay
/// FIXED regardless of any held modifier — they are the cancel gesture and the
/// reserved cycle key, so Shift+Esc still cancels and Ctrl+Down is still the
/// silent reject. Collision is on the FULL `(keycode, mask)` identity (matching
/// `from_accept_keys_with_mods`): Tab and Shift+Tab are distinct, so capturing
/// Tab while the other role holds Shift+Tab is NOT a collision.
pub fn record_decision(keycode: i64, mask: u32, other_role: (i64, u32)) -> RecordDecision {
    match keycode {
        // The crate consts, not literals: if the FIXED key set ever changed,
        // literals here would silently stop rejecting it (review-c135).
        crate::KEYCODE_ESCAPE => RecordDecision::Cancel, // fixed + cancel
        crate::KEYCODE_DOWN => RecordDecision::RejectFixed, // fixed cycle key
        _ if (keycode, mask) == other_role => RecordDecision::RejectCollision,
        _ => RecordDecision::Accept,
    }
}

/// Build the BOTH-slots request for one captured key. `RebindRequest`'s
/// `None` means "reset to DEFAULT" (`from_accept_keys` default-fills), NOT
/// "keep current" — a bare-`None` partial request would silently clobber
/// the other role's prior rebind back to Tab/backtick, so the recorder
/// always carries the other role's CURRENT registered key explicitly.
pub fn rebind_request_for(
    role: RecorderRole,
    captured: (i64, u32),
    current: ((i64, u32), (i64, u32)),
) -> RebindRequest {
    match role {
        RecorderRole::Word => (Some(captured), Some(current.1)),
        RecorderRole::Full => (Some(current.0), Some(captured)),
    }
}

/// Human label for an accept keycode (Shortcuts pane + recorder idle text).
/// Single source — the run loop's `shortcuts_text` composes through this.
pub fn keycode_label(code: i64) -> String {
    // The fixed accept keys go through the crate consts (drift-safe); the rest
    // are standard macOS virtual keycodes, DISPLAY-ONLY, so literals are fine.
    let named = match code {
        c if c == crate::KEYCODE_TAB => "Tab",
        c if c == crate::KEYCODE_GRAVE => "` (backtick)",
        c if c == crate::KEYCODE_ESCAPE => "Esc",
        c if c == crate::KEYCODE_DOWN => "Down arrow",
        // Letters.
        0 => "A",
        11 => "B",
        8 => "C",
        2 => "D",
        14 => "E",
        3 => "F",
        5 => "G",
        4 => "H",
        34 => "I",
        38 => "J",
        40 => "K",
        37 => "L",
        46 => "M",
        45 => "N",
        31 => "O",
        35 => "P",
        12 => "Q",
        15 => "R",
        1 => "S",
        17 => "T",
        32 => "U",
        9 => "V",
        13 => "W",
        7 => "X",
        16 => "Y",
        6 => "Z",
        // Digits.
        29 => "0",
        18 => "1",
        19 => "2",
        20 => "3",
        21 => "4",
        23 => "5",
        22 => "6",
        26 => "7",
        28 => "8",
        25 => "9",
        // Function keys.
        122 => "F1",
        120 => "F2",
        99 => "F3",
        118 => "F4",
        96 => "F5",
        97 => "F6",
        98 => "F7",
        100 => "F8",
        101 => "F9",
        109 => "F10",
        103 => "F11",
        111 => "F12",
        // Common specials.
        49 => "Space",
        36 => "Return",
        51 => "Delete",
        117 => "Forward delete",
        // Punctuation / symbol keys (US ANSI layout) — the PHYSICAL key, not
        // its shifted glyph (display only, so the recorder can label a rebind
        // to e.g. Control+] as "⌃]" rather than "⌃key 30").
        27 => "-",
        24 => "=",
        33 => "[",
        30 => "]",
        42 => "\\",
        41 => ";",
        39 => "'",
        43 => ",",
        47 => ".",
        44 => "/",
        // Arrows (Down is handled above via KEYCODE_DOWN).
        123 => "Left arrow",
        124 => "Right arrow",
        126 => "Up arrow",
        _ => return format!("key {code}"),
    };
    named.to_string()
}

/// The accept-key label with a macOS modifier-glyph prefix (⌃⌥⇧⌘) for the
/// Shortcuts pane (modifier-combo slice 1b). A zero mask renders exactly like
/// [`keycode_label`], so bare keys are unchanged.
pub fn keycode_label_with_mods(code: i64, mask: u32) -> String {
    format!(
        "{}{}",
        crate::accept_key_modifier_glyphs(mask),
        keycode_label(code)
    )
}

/// What a recorder field renders + writes for one captured keyDown — the pure
/// composition of [`record_decision`], [`rebind_request_for`] and
/// [`keycode_label`] so the AppKit field's keyDown is thin glue (the field
/// itself is LOOK-verified, per this file's AppKit convention).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RecorderOutcome {
    /// Esc: stop recording; restore the label to the role's CURRENT key.
    Cancel { idle_label: String },
    /// A reserved fixed key (Down): consumed silently — stay recording, no change.
    RejectSilent,
    /// The captured key collides with the other role: stay recording, show `hint`.
    RejectCollision { hint: &'static str },
    /// Accept: write `request` (BOTH slots — clobber-safe), show `label`, stop.
    Accept {
        request: RebindRequest,
        label: String,
    },
}

/// Compose one captured `(keycode, mask)` keyDown into a render+write outcome
/// for `role`, given the (word, full) currently-registered `(keycode, mask)`
/// pairs (`effective_accept_keys_with_mods()`). The Accept arm always carries
/// BOTH slots so a partial request can never clobber the other role back to
/// default (the c134 trap), and now preserves the OTHER role's modifier mask
/// verbatim (audit-r2: a one-role rebind must not strip the other's mask).
/// Labels render through `keycode_label_with_mods`, so a modifier shows its
/// ⌃⌥⇧⌘ glyph.
pub fn recorder_outcome(
    role: RecorderRole,
    keycode: i64,
    mask: u32,
    current: ((i64, u32), (i64, u32)),
) -> RecorderOutcome {
    let (role_current, other_current) = match role {
        RecorderRole::Word => (current.0, current.1),
        RecorderRole::Full => (current.1, current.0),
    };
    match record_decision(keycode, mask, other_current) {
        RecordDecision::Cancel => RecorderOutcome::Cancel {
            idle_label: keycode_label_with_mods(role_current.0, role_current.1),
        },
        RecordDecision::RejectFixed => RecorderOutcome::RejectSilent,
        RecordDecision::RejectCollision => RecorderOutcome::RejectCollision {
            hint: "In use \u{2014} press another",
        },
        RecordDecision::Accept => RecorderOutcome::Accept {
            request: rebind_request_for(role, (keycode, mask), current),
            label: keycode_label_with_mods(keycode, mask),
        },
    }
}

/// A Personalization-pane edit, carried across the crate seam as PRIMITIVES so
/// `platform_macos` stays free of a `personalization`/`app` dependency (the
/// `apps_edit` "index crosses the seam" idiom). The run loop maps each variant
/// back onto the source `PersonalizationProfile`'s field and calls
/// `inference.set_profile` live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonalizationEdit {
    /// Global free-text instructions (`PersonalizationProfile::global_instructions`).
    GlobalInstructions(String),
    /// Sender display name (`PersonalizationProfile::sender.name`).
    SenderName(String),
    /// Sender email (`PersonalizationProfile::sender.email`).
    SenderEmail(String),
    /// Steering-strength stop, the popup row 0..=5 addressing `Strength::from_stop`
    /// (0 = Off). Carried as a `usize` like `emoji_skin_tone_index`.
    StrengthStop(usize),
}

/// Settings-pane toggles, flipped by controls on the main thread and observed
/// by the run loop (the tray-flags pattern: render-only UI, policy outside).
#[derive(Clone)]
pub struct SettingsFlags {
    /// Master enabled flag — THE SAME Arc as TrayFlags.enabled (one atomic,
    /// two views: tray checkmark + this switch). The run loop's existing
    /// enabled-edge handles persist + ghost dismiss; tray and SIGUSR1 also
    /// write it, which is why switches refresh on every show.
    pub general_enabled: Arc<AtomicBool>,
    /// Labs: global mid-line completions (`COMPME_MIDLINE`). The run loop
    /// watches edges, persists, and re-applies the engine gate live.
    pub labs_midline: Arc<AtomicBool>,
    /// General: global typo autocorrect (`COMPME_AUTOCORRECT`). Same watcher
    /// pattern: the run loop persists and applies on the edge.
    pub general_autocorrect: Arc<AtomicBool>,
    /// General: trailing space after single-word accepts
    /// (`COMPME_TRAILING_SPACE`). Same watcher pattern.
    pub general_trailing_space: Arc<AtomicBool>,
    /// Context: include clipboard text as bounded prompt context
    /// (`COMPME_CLIPBOARD_CONTEXT`). The run loop watches/persists the edge.
    pub context_clipboard: Arc<AtomicBool>,
    /// Context: include screen OCR text as bounded prompt context
    /// (`COMPME_SCREEN_CONTEXT`). Turning it on persists for the next launch;
    /// turning it off also gates new OCR submissions in the current run.
    pub context_screen: Arc<AtomicBool>,
    /// Emoji: offer :shortcode completions (`COMPME_EMOJI`).
    pub emoji_enabled: Arc<AtomicBool>,
    /// Emoji: selected skin-tone popup row (`COMPME_EMOJI_SKIN_TONE`).
    /// The run loop maps the index to the app-side `SkinTone` enum and
    /// persists the config value.
    pub emoji_skin_tone_index: Arc<AtomicUsize>,
    /// Emoji: selected gender popup row (`COMPME_EMOJI_GENDER`). The run loop
    /// maps the index to the app-side `Gender` enum and persists the value.
    pub emoji_gender_index: Arc<AtomicUsize>,
    /// Statistics rows, composed by the run loop (`stats_pane_lines`) right
    /// before each show; the window only renders them (one label per line).
    pub stats_lines: Arc<Mutex<Vec<String>>>,
    /// Statistics: selected range-picker row (a `StatRange` index — 7/14/30
    /// days). The run loop reads it to choose the `daily_buckets` span before
    /// composing `stats_lines`. Default 0 (Last 7 days) keeps the legacy span.
    pub stat_range_index: Arc<AtomicUsize>,
    /// Statistics range-picker item titles, one per `StatRange::ALL` row in
    /// order (so the index addresses the enum). Composed app-side because the
    /// window can't see the `stats` crate (the `setup_model_menu_titles` seam).
    pub stat_range_titles: Vec<String>,
    /// Statistics: selected grouping-picker row (a `StatGrouping` index —
    /// Daily/Weekly). The run loop reads it to re-bucket the daily slices via
    /// `stats::group_buckets` before composing `stats_lines`. Default 0 (Daily)
    /// is the identity re-bucketing, so the legacy display is unchanged.
    pub stat_group_index: Arc<AtomicUsize>,
    /// Statistics grouping-picker item titles, one per `StatGrouping::ALL` row.
    pub stat_group_titles: Vec<String>,
    /// About text (version/license/no-telemetry/repo/credits), composed once
    /// at startup — static for the process lifetime, rendered verbatim.
    pub about_text: String,
    /// Setup rows (permission/model readiness), composed by the run loop
    /// right before each show; one label per line, refreshed like stats.
    pub setup_lines: Arc<Mutex<Vec<String>>>,
    /// Setup buttons (tray-flags pattern): the button stores true, the run
    /// loop consumes the edge and performs the privileged call on its side.
    pub setup_grant_ax: Arc<AtomicBool>,
    pub setup_request_screen: Arc<AtomicBool>,
    pub setup_reveal_model: Arc<AtomicBool>,
    /// Setup "Download Model" — the run loop spawns the worker for the
    /// `setup_model_index` target and logs progress.
    pub setup_download_model: Arc<AtomicBool>,
    /// Picker: the catalog index the Setup-tab popup selects as the download
    /// target. Default = the recommended index (set by the run loop), so the
    /// download is unchanged until the user picks another row. The run loop's
    /// download edge reads it via `model_picker::selected_catalog_entry`,
    /// which is total over an out-of-range value.
    pub setup_model_index: Arc<AtomicUsize>,
    /// Picker: the popup's item titles, one per catalog row in catalog order
    /// (so the selected index still addresses the catalog), each suffixed with
    /// its RAM-fit label (e.g. "qwen2.5-0.5b · fits"). Composed once by the
    /// run loop (model_catalog + the RAM probe are app-side; the window only
    /// renders the finished strings). `Exceeds` rows are blocked by the download
    /// edge.
    pub setup_model_menu_titles: Vec<String>,
    /// Apps rows (per-app recorded-input counts), composed by the run loop
    /// right before each show; refreshed like stats.
    pub apps_lines: Arc<Mutex<Vec<String>>>,
    /// Per-Apps-row resolved policy bits `[Enabled, TabDisabled, MidLine,
    /// Autocorrect]`, composed by the run loop alongside `apps_lines` (same
    /// order/cap) so each row's checkboxes open reflecting the saved per-app
    /// override instead of a hard-seeded OFF. Refreshed on every show like
    /// `apps_lines`. The bool order matches `APP_POLICY_FIELD_TITLES` / the
    /// `apps_edit` field-index encoding.
    pub apps_policy_bits: Arc<Mutex<Vec<[bool; APP_POLICY_FIELDS]>>>,
    /// A clicked Apps-row Delete button: the ROW INDEX (the run loop resolves
    /// it to an app id via apps_row_ids and performs the delete).
    pub apps_delete_row: Arc<Mutex<Option<usize>>>,
    /// A toggled Apps-row policy checkbox: `(row, field_index, on)`. The field
    /// is carried as a SMALL INDEX (0=Enabled, 1=TabDisabled, 2=MidLine,
    /// 3=Autocorrect) so this crate stays free of a `prefs` dependency — the
    /// run loop maps it to `prefs::AppPolicyField` and calls
    /// `set_app_policy_field` on the row's app id (apps_delete_row pattern; the
    /// "index crosses the seam" idiom of setup_model_index/stat_range_index).
    pub apps_edit: Arc<Mutex<Option<(usize, usize, bool)>>>,
    /// Shortcuts text (current bindings + how to change them). Behind a
    /// Mutex since recorder 5b: a live rebind recomposes it and the window
    /// refreshes the label on every show (stats_lines pattern).
    pub shortcuts_text: Arc<Mutex<String>>,
    /// A requested live rebind: (word, full) raw keycodes, `None` = default.
    /// The recorder UI (or a debug trigger) writes it; the run loop consumes
    /// the edge and runs the keymap-first/rearm-second/persist-last sequence
    /// (apps_delete_row pattern).
    pub shortcuts_rebind_request: Arc<Mutex<Option<RebindRequest>>>,
    /// The latest Personalization-pane edit (text-field commit or strength
    /// popup). Last-writer-wins per tick: the run loop consumes the edge,
    /// applies the variant to its source `PersonalizationProfile`, calls
    /// `inference.set_profile`, and persists (apps_edit pattern).
    pub personalization_edit: Arc<Mutex<Option<PersonalizationEdit>>>,
    /// Personalization initial values, composed by the run loop from the source
    /// profile so the pane's fields/popup reflect the current config on open
    /// (the about_text / setup_model_index pattern — render-only seed strings).
    pub personalization_instructions: Arc<Mutex<String>>,
    pub personalization_sender_name: Arc<Mutex<String>>,
    pub personalization_sender_email: Arc<Mutex<String>>,
    /// Strength popup: the pre-selected stop (0..=5, 0 = Off) and one title per
    /// stop in `Strength::STOPS` order. Titles cross the seam here because
    /// `platform_macos` can't see the `personalization` crate (the stat-range
    /// titles pattern); the selected index addresses `Strength::from_stop`.
    pub personalization_strength_index: Arc<AtomicUsize>,
    pub personalization_strength_titles: Vec<String>,
}

struct SettingsTargetIvars {
    flags: SettingsFlags,
}

define_class!(
    // SAFETY: a plain NSObject subclass used only as a control action target;
    // its methods read control state and flip atomics.
    #[unsafe(super = objc2_foundation::NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = SettingsTargetIvars]
    struct SettingsTarget;

    unsafe impl NSObjectProtocol for SettingsTarget {}

    impl SettingsTarget {
        #[unsafe(method(grantAccessibility:))]
        fn grant_accessibility(&self, _sender: Option<&NSButton>) {
            self.ivars().flags.setup_grant_ax.store(true, Ordering::Relaxed);
        }

        #[unsafe(method(requestScreenRecording:))]
        fn request_screen_recording(&self, _sender: Option<&NSButton>) {
            self.ivars()
                .flags
                .setup_request_screen
                .store(true, Ordering::Relaxed);
        }

        #[unsafe(method(revealModel:))]
        fn reveal_model(&self, _sender: Option<&NSButton>) {
            self.ivars()
                .flags
                .setup_reveal_model
                .store(true, Ordering::Relaxed);
        }

        #[unsafe(method(downloadModel:))]
        fn download_model(&self, _sender: Option<&NSButton>) {
            self.ivars()
                .flags
                .setup_download_model
                .store(true, Ordering::Relaxed);
        }

        #[unsafe(method(selectModel:))]
        fn select_model(&self, sender: Option<&NSPopUpButton>) {
            if let Some(popup) = sender {
                // indexOfSelectedItem is -1 only on an empty menu; the popup is
                // always populated, but clamp negatives to 0 defensively. The
                // run loop resolves this index through selected_catalog_entry,
                // which falls back to recommended on any out-of-range value.
                let index = popup.indexOfSelectedItem().max(0) as usize;
                self.ivars()
                    .flags
                    .setup_model_index
                    .store(index, Ordering::Relaxed);
            }
        }

        #[unsafe(method(selectStatRange:))]
        fn select_stat_range(&self, sender: Option<&NSPopUpButton>) {
            if let Some(popup) = sender {
                // indexOfSelectedItem is -1 only on an empty menu; clamp
                // defensively. The run loop resolves it via StatRange::from_index,
                // which is total over an out-of-range value.
                let index = popup.indexOfSelectedItem().max(0) as usize;
                self.ivars()
                    .flags
                    .stat_range_index
                    .store(index, Ordering::Relaxed);
            }
        }

        #[unsafe(method(selectStatGroup:))]
        fn select_stat_group(&self, sender: Option<&NSPopUpButton>) {
            if let Some(popup) = sender {
                // Resolved by StatGrouping::from_index (total over OOB).
                let index = popup.indexOfSelectedItem().max(0) as usize;
                self.ivars()
                    .flags
                    .stat_group_index
                    .store(index, Ordering::Relaxed);
            }
        }

        #[unsafe(method(deleteAppRow:))]
        fn delete_app_row(&self, sender: Option<&NSButton>) {
            if let Some(button) = sender {
                let row = button.tag().max(0) as usize;
                // Recover from a poisoned lock rather than silently dropping the
                // user's Delete click: the slot is a plain `Option<usize>` whose
                // bytes are valid even if some other holder panicked.
                let mut slot = self
                    .ivars()
                    .flags
                    .apps_delete_row
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *slot = Some(row);
            }
        }

        #[unsafe(method(editAppPolicy:))]
        fn edit_app_policy(&self, sender: Option<&NSButton>) {
            if let Some(checkbox) = sender {
                // The tag packs (row, field): `row * APP_POLICY_FIELDS + field`,
                // mirroring deleteAppRow's row-in-tag. The run loop unpacks it,
                // resolves row -> app id via apps_row_ids (the SAME cap/order),
                // maps the field index -> prefs::AppPolicyField, and writes.
                let packed = checkbox.tag().max(0) as usize;
                let row = packed / APP_POLICY_FIELDS;
                let field = packed % APP_POLICY_FIELDS;
                let on = checkbox.state() == NSControlStateValueOn;
                // Poison-recovery, like deleteAppRow: the slot is a plain
                // Option whose bytes stay valid even if a holder panicked.
                let mut slot = self
                    .ivars()
                    .flags
                    .apps_edit
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *slot = Some((row, field, on));
            }
        }

        #[unsafe(method(toggleEnabled:))]
        fn toggle_enabled(&self, sender: Option<&NSSwitch>) {
            if let Some(switch) = sender {
                let on = switch.state() == NSControlStateValueOn;
                self.ivars().flags.general_enabled.store(on, Ordering::Relaxed);
            }
        }

        #[unsafe(method(toggleTrailingSpace:))]
        fn toggle_trailing_space(&self, sender: Option<&NSSwitch>) {
            if let Some(switch) = sender {
                let on = switch.state() == NSControlStateValueOn;
                self.ivars()
                    .flags
                    .general_trailing_space
                    .store(on, Ordering::Relaxed);
            }
        }

        #[unsafe(method(toggleAutocorrect:))]
        fn toggle_autocorrect(&self, sender: Option<&NSSwitch>) {
            if let Some(switch) = sender {
                let on = switch.state() == NSControlStateValueOn;
                self.ivars()
                    .flags
                    .general_autocorrect
                    .store(on, Ordering::Relaxed);
            }
        }

        #[unsafe(method(toggleMidline:))]
        fn toggle_midline(&self, sender: Option<&NSSwitch>) {
            if let Some(switch) = sender {
                let on = switch.state() == NSControlStateValueOn;
                self.ivars().flags.labs_midline.store(on, Ordering::Relaxed);
            }
        }

        #[unsafe(method(toggleClipboardContext:))]
        fn toggle_clipboard_context(&self, sender: Option<&NSSwitch>) {
            if let Some(switch) = sender {
                let on = switch.state() == NSControlStateValueOn;
                self.ivars()
                    .flags
                    .context_clipboard
                    .store(on, Ordering::Relaxed);
            }
        }

        #[unsafe(method(toggleScreenContext:))]
        fn toggle_screen_context(&self, sender: Option<&NSSwitch>) {
            if let Some(switch) = sender {
                let on = switch.state() == NSControlStateValueOn;
                self.ivars()
                    .flags
                    .context_screen
                    .store(on, Ordering::Relaxed);
            }
        }

        #[unsafe(method(toggleEmoji:))]
        fn toggle_emoji(&self, sender: Option<&NSSwitch>) {
            if let Some(switch) = sender {
                let on = switch.state() == NSControlStateValueOn;
                self.ivars()
                    .flags
                    .emoji_enabled
                    .store(on, Ordering::Relaxed);
            }
        }

        #[unsafe(method(selectEmojiSkinTone:))]
        fn select_emoji_skin_tone(&self, sender: Option<&NSPopUpButton>) {
            if let Some(popup) = sender {
                let index = popup.indexOfSelectedItem().max(0) as usize;
                self.ivars()
                    .flags
                    .emoji_skin_tone_index
                    .store(index, Ordering::Relaxed);
            }
        }

        #[unsafe(method(selectEmojiGender:))]
        fn select_emoji_gender(&self, sender: Option<&NSPopUpButton>) {
            if let Some(popup) = sender {
                let index = popup.indexOfSelectedItem().max(0) as usize;
                self.ivars()
                    .flags
                    .emoji_gender_index
                    .store(index, Ordering::Relaxed);
            }
        }

        // Personalization pane: each control records one `PersonalizationEdit`
        // (last-writer-wins) for the run loop to apply live. Text fields fire on
        // commit (Enter / focus-loss); the strength popup on selection.
        #[unsafe(method(editGlobalInstructions:))]
        fn edit_global_instructions(&self, sender: Option<&NSTextField>) {
            if let Some(field) = sender {
                self.record_personalization_edit(PersonalizationEdit::GlobalInstructions(
                    field.stringValue().to_string(),
                ));
            }
        }

        #[unsafe(method(editSenderName:))]
        fn edit_sender_name(&self, sender: Option<&NSTextField>) {
            if let Some(field) = sender {
                self.record_personalization_edit(PersonalizationEdit::SenderName(
                    field.stringValue().to_string(),
                ));
            }
        }

        #[unsafe(method(editSenderEmail:))]
        fn edit_sender_email(&self, sender: Option<&NSTextField>) {
            if let Some(field) = sender {
                self.record_personalization_edit(PersonalizationEdit::SenderEmail(
                    field.stringValue().to_string(),
                ));
            }
        }

        #[unsafe(method(selectStrength:))]
        fn select_strength(&self, sender: Option<&NSPopUpButton>) {
            if let Some(popup) = sender {
                let index = popup.indexOfSelectedItem().max(0) as usize;
                self.record_personalization_edit(PersonalizationEdit::StrengthStop(index));
            }
        }
    }
);

impl SettingsTarget {
    fn new(flags: SettingsFlags, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SettingsTargetIvars { flags });
        // SAFETY: NSObject's init signature is correct for this subclass.
        unsafe { objc2::msg_send![super(this), init] }
    }

    /// Park a Personalization edit for the run loop (last-writer-wins; the loop
    /// `take()`s it each tick). Poison-tolerant like the other flag writers.
    fn record_personalization_edit(&self, edit: PersonalizationEdit) {
        *self
            .ivars()
            .flags
            .personalization_edit
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(edit);
    }
}

struct KeyRecorderFieldIvars {
    role: RecorderRole,
    /// keyDown parks the captured (both-slots, clobber-safe) request here; the
    /// run loop consumes the edge (`apply_live_accept_keymap`).
    rebind_slot: Arc<Mutex<Option<RebindRequest>>>,
    /// Child label that renders the key text — a bare NSView has no
    /// `setStringValue`, so the visible string lives on this passive subview.
    label: Retained<NSTextField>,
}

define_class!(
    // SAFETY: a plain NSVIEW subclass used as an inline key recorder (recorder
    // 5b slice 4b). NSView, NOT NSTextField: a non-editable NSTextField that
    // takes first responder installs an NSTextView field editor whose
    // input-context setup spins the run loop and DEADLOCKS under our custom
    // CFRunLoop heartbeat — 2026-06-14 live finding: clicking the field HUNG the
    // app (force-quit only). A bare NSView has no text-input machinery. It sits
    // transparently OVER a sibling display label (same frame, added on top), so
    // the overlay is the hit-test winner and captures the click/keys while the
    // label renders the text. keyDown does NOT call super (the key is CAPTURED,
    // never typed); acceptsFirstResponder + mouseDown make it first responder on
    // click. The decision logic is the unit-tested `recorder_outcome`; the
    // AppKit shell is LIVE-verified (this file's convention).
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = KeyRecorderFieldIvars]
    struct KeyRecorderField;

    impl KeyRecorderField {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            // Grab first responder so the next keyDown lands here. NSView IS-A
            // NSResponder, so the upcast is a direct as_ref().
            if let Some(window) = self.window() {
                // Force the app active + this window key so the OS routes the
                // next keyDown HERE: under the custom CFRunLoop heartbeat a click
                // alone doesn't give compme keyboard focus.
                if let Some(mtm) = MainThreadMarker::new() {
                    NSApplication::sharedApplication(mtm).activate();
                }
                window.makeKeyWindow();
                let view: &NSView = self;
                let responder: &NSResponder = view.as_ref();
                let became = window.makeFirstResponder(Some(responder));
                // Recording feedback: the box shows it's armed. keyDown replaces
                // this with the captured key; Esc reverts to the current key.
                self.ivars()
                    .label
                    .setStringValue(&NSString::from_str("Press a key\u{2026}"));
                if crate::debug_enabled() {
                    eprintln!(
                        "compme: recorder mouseDown role={:?} first_responder={became} key_window={}",
                        self.ivars().role,
                        window.isKeyWindow()
                    );
                }
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            // u16 keyCode -> i64 (the crate's keycode currency). NO super call:
            // swallow the key so it is captured, never typed. The child label
            // renders the result. Slice 2: read the held modifiers too —
            // NSEvent reports them in high bits, mapped to the Carbon mask the
            // accept stack registers.
            let keycode = event.keyCode() as i64;
            let mask = crate::ns_modifier_flags_to_carbon_mask(event.modifierFlags().0 as u64);
            if crate::debug_enabled() {
                eprintln!(
                    "compme: recorder keyDown role={:?} keycode={keycode} mask={mask}",
                    self.ivars().role
                );
            }
            let label = &self.ivars().label;
            match recorder_outcome(
                self.ivars().role,
                keycode,
                mask,
                crate::effective_accept_keys_with_mods(),
            ) {
                RecorderOutcome::Accept { request, label: text } => {
                    // Recover from a poisoned lock rather than silently dropping
                    // the user's rebind: the slot holds a plain `Option<_>` whose
                    // bytes stay valid even if some other holder panicked.
                    let mut slot = self
                        .ivars()
                        .rebind_slot
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    *slot = Some(request);
                    label.setStringValue(&NSString::from_str(&text));
                }
                RecorderOutcome::Cancel { idle_label } => {
                    label.setStringValue(&NSString::from_str(&idle_label));
                }
                RecorderOutcome::RejectSilent => {}
                RecorderOutcome::RejectCollision { hint } => {
                    label.setStringValue(&NSString::from_str(hint));
                }
            }
        }

    }
);

impl KeyRecorderField {
    /// `label` is the sibling display field this overlay updates; the caller
    /// adds the label first, then this overlay on top of it (same frame).
    fn new(
        role: RecorderRole,
        rebind_slot: Arc<Mutex<Option<RebindRequest>>>,
        label: Retained<NSTextField>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(KeyRecorderFieldIvars {
            role,
            rebind_slot,
            label,
        });
        // set_ivars BEFORE init (the SettingsTarget pattern); NSView's
        // designated initializer is initWithFrame:.
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(160.0, 24.0));
        unsafe { objc2::msg_send![super(this), initWithFrame: frame] }
    }
}

/// Whether a visibility transition requires demoting the activation policy
/// back to `Accessory` (pure: the run loop feeds it the polled states).
pub fn policy_restore_needed(was_visible: bool, visible_now: bool) -> bool {
    was_visible && !visible_now
}

/// Whether an Apps-pane row names a deletable app (an "app \u{2014} N" count
/// row from `apps_pane_lines`) rather than a status message ("Input collection
/// is off" / "No recorded inputs yet") or an empty padding row. Drives per-row
/// Delete-button visibility so a Delete button never sits beside a non-app row
/// (2026-06-14 live finding: 8 Delete buttons showed against empty rows).
/// Couples to `apps_pane_lines`'s em-dash separator; pinned by test.
fn apps_row_is_deletable(line: &str) -> bool {
    line.contains(" \u{2014} ")
}

/// Hide the per-row policy checkboxes on non-deletable (status/empty) rows,
/// mirroring the Delete-button visibility rule. Checkboxes are stored row-major
/// (APP_POLICY_FIELDS per row), so checkbox flat-index `idx` belongs to row
/// `idx / APP_POLICY_FIELDS`.
fn refresh_apps_policy_checkbox_visibility(checkboxes: &[Retained<NSButton>], lines: &[String]) {
    for (idx, checkbox) in checkboxes.iter().enumerate() {
        let row = idx / APP_POLICY_FIELDS;
        checkbox.setHidden(!lines.get(row).is_some_and(|l| apps_row_is_deletable(l)));
    }
}

/// Re-seed the per-row policy checkbox CHECKED state from `bits` (composed by
/// the run loop alongside `apps_lines`, same order/cap). Checkboxes are stored
/// row-major (`APP_POLICY_FIELDS` per row), so flat index `idx` is row
/// `idx / APP_POLICY_FIELDS`, field `idx % APP_POLICY_FIELDS`. A row absent
/// from `bits` (status/empty rows) falls back to OFF — those rows are hidden
/// anyway by [`refresh_apps_policy_checkbox_visibility`].
fn refresh_apps_policy_checkbox_states(
    checkboxes: &[Retained<NSButton>],
    bits: &[[bool; APP_POLICY_FIELDS]],
) {
    for (idx, checkbox) in checkboxes.iter().enumerate() {
        let row = idx / APP_POLICY_FIELDS;
        let field = idx % APP_POLICY_FIELDS;
        let on = bits.get(row).is_some_and(|r| r[field]);
        checkbox.setState(if on {
            NSControlStateValueOn
        } else {
            objc2_app_kit::NSControlStateValueOff
        });
    }
}

fn setup_action_available(lines: &[String], label: &str, ready: bool) -> bool {
    let glyph = if ready { '\u{2713}' } else { '\u{2717}' };
    let expected = format!("{glyph} {label}");
    lines.iter().any(|line| line.as_str() == expected.as_str())
}

fn refresh_setup_action_buttons(buttons: &[Retained<NSButton>], lines: &[String]) {
    let available = [
        setup_action_available(lines, "Accessibility", false),
        setup_action_available(lines, "Screen Recording", false),
        setup_action_available(lines, "Model file", true),
        true,
    ];
    for (button, available) in buttons.iter().zip(available) {
        button.setHidden(!available);
        button.setEnabled(available);
    }
}

pub struct MacosSettingsWindow {
    window: Option<Retained<NSWindow>>,
    flags: SettingsFlags,
    // Keep the action target alive for the window's lifetime.
    target: Option<Retained<SettingsTarget>>,
    // Statistics row labels, refreshed from `flags.stats_lines` on every show
    // (the window is built once; data rows must not go stale on reopen).
    stats_labels: Vec<Retained<NSTextField>>,
    // Setup row labels, refreshed from `flags.setup_lines` the same way.
    setup_labels: Vec<Retained<NSTextField>>,
    // Setup action buttons, hidden/disabled from the same row actions as the
    // labels so unavailable prompts (notably Screen Recording when OCR is off)
    // cannot be clicked.
    setup_action_buttons: Vec<Retained<NSButton>>,
    // Apps row labels, refreshed from `flags.apps_lines` the same way.
    apps_labels: Vec<Retained<NSTextField>>,
    // Per-row Apps Delete buttons, hidden on every refresh for rows that are
    // not deletable app rows (status/empty rows) — see `apps_row_is_deletable`.
    apps_delete_buttons: Vec<Retained<NSButton>>,
    // Per-row Apps policy checkboxes (APP_POLICY_FIELDS per row, row-major),
    // hidden on the same non-deletable rows as the Delete buttons.
    apps_policy_checkboxes: Vec<Retained<NSButton>>,
    // General-tab switches, refreshed from their atomics on every show:
    // enabled has EXTERNAL writers (tray, SIGUSR1), so its rendered state
    // can go stale while the window is closed (c95 staleness class). The
    // others refresh too — harmless and uniform.
    switches: Vec<(Retained<NSSwitch>, Arc<AtomicBool>)>,
    shortcuts_label: Option<Retained<NSTextField>>,
    // Per-role recorder display boxes, refreshed to the effective keymap on
    // every show. The wrapping shortcuts label refreshes from flags.shortcuts_
    // text, but these bezeled boxes are separate AppKit objects — without this
    // they could disagree with the label after a rebind that happened via a
    // non-window path while the window was closed (the banked 4b residual).
    recorder_labels: Vec<(RecorderRole, Retained<NSTextField>)>,
    // Personalization text fields, re-seeded from flags.personalization_* on
    // every show so an out-of-window config reload is reflected on reopen (the
    // same staleness class the apps/stats/shortcuts data rows guard against).
    personalization_instructions_field: Option<Retained<NSTextField>>,
    personalization_name_field: Option<Retained<NSTextField>>,
    personalization_email_field: Option<Retained<NSTextField>>,
}

impl MacosSettingsWindow {
    pub fn new(flags: SettingsFlags) -> Self {
        // Lazy: the NSWindow is created on first show (main thread).
        Self {
            window: None,
            flags,
            target: None,
            stats_labels: Vec::new(),
            setup_labels: Vec::new(),
            setup_action_buttons: Vec::new(),
            apps_labels: Vec::new(),
            apps_delete_buttons: Vec::new(),
            apps_policy_checkboxes: Vec::new(),
            switches: Vec::new(),
            shortcuts_label: None,
            recorder_labels: Vec::new(),
            personalization_instructions_field: None,
            personalization_name_field: None,
            personalization_email_field: None,
        }
    }

    /// Show the window (creating it on first use) and promote the activation
    /// policy so it can become key. Main-thread only.
    pub fn show(&mut self) -> Result<(), PlatformError> {
        let mtm = main_thread()?;
        if self.window.is_none() {
            let target = SettingsTarget::new(self.flags.clone(), mtm);
            let built = build_window(mtm, &target, &self.flags);
            self.window = Some(built.window);
            self.stats_labels = built.stats_labels;
            self.setup_labels = built.setup_labels;
            self.setup_action_buttons = built.setup_action_buttons;
            self.apps_labels = built.apps_labels;
            self.apps_delete_buttons = built.apps_delete_buttons;
            self.apps_policy_checkboxes = built.apps_policy_checkboxes;
            self.switches = built.switches;
            self.shortcuts_label = Some(built.shortcuts_label);
            self.recorder_labels = built.recorder_labels;
            self.personalization_instructions_field = built.personalization_instructions_field;
            self.personalization_name_field = built.personalization_name_field;
            self.personalization_email_field = built.personalization_email_field;
            self.target = Some(target);
        }
        // Refresh data rows on EVERY show — the lazily built window is reused
        // across opens, so stale strings would otherwise survive a reopen.
        if let Ok(lines) = self.flags.stats_lines.lock() {
            for (label, line) in self.stats_labels.iter().zip(lines.iter()) {
                label.setStringValue(&NSString::from_str(line));
            }
        }
        if let Ok(lines) = self.flags.setup_lines.lock() {
            for (label, line) in self.setup_labels.iter().zip(lines.iter()) {
                label.setStringValue(&NSString::from_str(line));
            }
            refresh_setup_action_buttons(&self.setup_action_buttons, &lines);
        }
        if let Ok(lines) = self.flags.apps_lines.lock() {
            for (label, line) in self.apps_labels.iter().zip(lines.iter()) {
                label.setStringValue(&NSString::from_str(line));
            }
            for (i, button) in self.apps_delete_buttons.iter().enumerate() {
                button.setHidden(!lines.get(i).is_some_and(|l| apps_row_is_deletable(l)));
            }
            refresh_apps_policy_checkbox_visibility(&self.apps_policy_checkboxes, &lines);
        }
        // Per-row policy checkboxes re-seed from the run-loop-published bits —
        // a per-app override edited via the web UI / config reload while the
        // window was closed would otherwise leave the checkboxes stale.
        if let Ok(bits) = self.flags.apps_policy_bits.lock() {
            refresh_apps_policy_checkbox_states(&self.apps_policy_checkboxes, &bits);
        }
        // Personalization fields re-seed from their mutexes — a config reload
        // while the window was closed updates flags.personalization_* and the
        // fields would otherwise show the build-time values (c95 staleness).
        if let Some(field) = &self.personalization_instructions_field {
            if let Ok(text) = self.flags.personalization_instructions.lock() {
                field.setStringValue(&NSString::from_str(&text));
            }
        }
        if let Some(field) = &self.personalization_name_field {
            if let Ok(text) = self.flags.personalization_sender_name.lock() {
                field.setStringValue(&NSString::from_str(&text));
            }
        }
        if let Some(field) = &self.personalization_email_field {
            if let Ok(text) = self.flags.personalization_sender_email.lock() {
                field.setStringValue(&NSString::from_str(&text));
            }
        }
        // Shortcuts text re-reads its mutex — a live rebind (recorder 5b)
        // recomposes it while the window is closed.
        if let (Some(label), Ok(text)) = (&self.shortcuts_label, self.flags.shortcuts_text.lock()) {
            label.setStringValue(&NSString::from_str(&text));
        }
        // Recorder boxes re-sync to the effective keymap so they never disagree
        // with the wrapping label above after an out-of-window rebind. The
        // in-window case needs no live cross-box update: keyDown self-updates
        // the active box, and a rebind carries the OTHER role's key through
        // verbatim (rebind_request_for) — so the sibling box never goes stale.
        if !self.recorder_labels.is_empty() {
            let (word, full) = crate::effective_accept_keys_with_mods();
            for (role, label) in &self.recorder_labels {
                let (code, mask) = match role {
                    RecorderRole::Word => word,
                    RecorderRole::Full => full,
                };
                label.setStringValue(&NSString::from_str(&keycode_label_with_mods(code, mask)));
            }
        }
        // Switches re-sync from their atomics — enabled can be flipped by
        // the tray or SIGUSR1 while this window is closed.
        for (switch, atomic) in &self.switches {
            switch.setState(if atomic.load(Ordering::Relaxed) {
                NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
        }
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        // Bring the app forward FIRST, then force the window above other apps'
        // windows. `activate()` alone is cooperative on modern macOS and can
        // leave an accessory app's window BEHIND the previously-active app
        // (2026-06-14 live finding); `orderFrontRegardless` overrides that.
        app.activate();
        if let Some(window) = &self.window {
            window.makeKeyAndOrderFront(None);
            window.orderFrontRegardless();
        }
        Ok(())
    }

    /// Re-render the Setup rows from `flags.setup_lines` while the window
    /// stays open (the visible-only poll edge; show() covers the open edge).
    pub fn refresh_setup_labels(&self) {
        if let Ok(lines) = self.flags.setup_lines.lock() {
            for (label, line) in self.setup_labels.iter().zip(lines.iter()) {
                label.setStringValue(&NSString::from_str(line));
            }
            refresh_setup_action_buttons(&self.setup_action_buttons, &lines);
        }
    }

    /// Re-render the Shortcuts text from `flags.shortcuts_text` after a
    /// live rebind (the run loop recomposes, then calls this — the slice-4
    /// recorder lives INSIDE this window, so the window is open at exactly
    /// the moment the text changes; show() covers the reopen edge).
    pub fn refresh_shortcuts_label(&self) {
        if let (Some(label), Ok(text)) = (&self.shortcuts_label, self.flags.shortcuts_text.lock()) {
            label.setStringValue(&NSString::from_str(&text));
        }
    }

    /// Re-render the Apps rows from `flags.apps_lines` after a delete (the
    /// run loop recomposes, then calls this; show() covers the open edge).
    pub fn refresh_apps_labels(&self) {
        if let Ok(lines) = self.flags.apps_lines.lock() {
            for (label, line) in self.apps_labels.iter().zip(lines.iter()) {
                label.setStringValue(&NSString::from_str(line));
            }
            for (i, button) in self.apps_delete_buttons.iter().enumerate() {
                button.setHidden(!lines.get(i).is_some_and(|l| apps_row_is_deletable(l)));
            }
            refresh_apps_policy_checkbox_visibility(&self.apps_policy_checkboxes, &lines);
        }
        // The app set may have shifted (a delete reindexes rows), so re-seed
        // the checkbox states from the freshly published bits, mirroring show().
        if let Ok(bits) = self.flags.apps_policy_bits.lock() {
            refresh_apps_policy_checkbox_states(&self.apps_policy_checkboxes, &bits);
        }
    }

    /// Whether the window is visible to the app — TRUE while miniaturized
    /// (AppKit `isVisible` semantics). That is deliberate for the policy
    /// dance: a minimized window needs the Dock (its tile is the restore
    /// path), so the activation policy must stay `Regular` until the window
    /// actually closes. Main-thread only.
    pub fn is_visible(&self) -> bool {
        self.window.as_ref().is_some_and(|w| w.isVisible())
    }

    /// Demote the activation policy back to `Accessory` (after the window
    /// closed — however it closed). Main-thread only.
    pub fn restore_accessory_policy(&self) -> Result<(), PlatformError> {
        let mtm = main_thread()?;
        NSApplication::sharedApplication(mtm)
            .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        Ok(())
    }
}

fn main_thread() -> Result<MainThreadMarker, PlatformError> {
    MainThreadMarker::new().ok_or_else(|| PlatformError::CannotComplete {
        reason: "settings window requires the main thread".into(),
    })
}

/// One control's frame in a settings pane's local (bottom-left origin)
/// coordinates. Extracted so a pane's layout can be geometry-checked
/// deterministically: the pane can't be built off the test harness's non-main
/// thread (AppKit is main-thread-only), so overlap/bounds correctness — the
/// layout half of visual validation — is proven by test here, not only by eye.
#[derive(Clone, Copy)]
struct PaneRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl PaneRect {
    fn ns(self) -> NSRect {
        NSRect::new(NSPoint::new(self.x, self.y), NSSize::new(self.w, self.h))
    }

    /// Half-open rectangle intersection: shared edges (a row sitting exactly
    /// above another, or a label butting a field) do NOT count as overlap.
    #[cfg(test)]
    fn intersects(self, o: PaneRect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

/// Personalization pane control frames — the single source of truth consumed by
/// `build_window` AND `personalization_pane_layout_has_no_overlaps_within_budget`.
/// The multi-line instructions field (`GI_FIELD`, ~5–6 lines) pushed the
/// sender/strength rows down into the pane's lower space; the test proves the
/// stack stays collision-free and inside the ~500×350 pane budget so a future
/// layout edit can't silently overlap controls (only a real Mac shows rendering,
/// but geometry is verified here).
mod pers_layout {
    use super::PaneRect;
    pub const GI_LABEL: PaneRect = PaneRect {
        x: 20.0,
        y: 300.0,
        w: 440.0,
        h: 20.0,
    };
    pub const GI_FIELD: PaneRect = PaneRect {
        x: 20.0,
        y: 170.0,
        w: 460.0,
        h: 124.0,
    };
    pub const NAME_LABEL: PaneRect = PaneRect {
        x: 20.0,
        y: 142.0,
        w: 120.0,
        h: 22.0,
    };
    pub const NAME_FIELD: PaneRect = PaneRect {
        x: 145.0,
        y: 140.0,
        w: 335.0,
        h: 24.0,
    };
    pub const EMAIL_LABEL: PaneRect = PaneRect {
        x: 20.0,
        y: 107.0,
        w: 120.0,
        h: 22.0,
    };
    pub const EMAIL_FIELD: PaneRect = PaneRect {
        x: 145.0,
        y: 105.0,
        w: 335.0,
        h: 24.0,
    };
    pub const STRENGTH_LABEL: PaneRect = PaneRect {
        x: 20.0,
        y: 67.0,
        w: 140.0,
        h: 22.0,
    };
    pub const STRENGTH_POPUP: PaneRect = PaneRect {
        x: 165.0,
        y: 64.0,
        w: 220.0,
        h: 26.0,
    };
    /// Every control, for the geometry test.
    #[cfg(test)]
    pub const ALL: [(&str, PaneRect); 8] = [
        ("gi_label", GI_LABEL),
        ("gi_field", GI_FIELD),
        ("name_label", NAME_LABEL),
        ("name_field", NAME_FIELD),
        ("email_label", EMAIL_LABEL),
        ("email_field", EMAIL_FIELD),
        ("strength_label", STRENGTH_LABEL),
        ("strength_popup", STRENGTH_POPUP),
    ];
    /// Pane content budget the layout is designed against (tab-content inset of
    /// the 680×420 window). A control extending past this would clip on screen.
    #[cfg(test)]
    pub const BUDGET_W: f64 = 500.0;
    #[cfg(test)]
    pub const BUDGET_H: f64 = 350.0;
}

/// Apps pane layout — one COMPACT line per recorded app: name, four title-less
/// policy checkboxes as columns (labelled by a header row + per-checkbox
/// tooltips), and a Delete button, so N apps stack at one row-step with no
/// overlap. Shared by `build_window` and the geometry test. Replaces a
/// 2-line-per-row layout whose 26px step drew each row's checkboxes over the
/// next row's name (28 collisions — see `apps_pane_grid_has_no_overlaps...`).
mod apps_layout {
    use super::{PaneRect, APP_POLICY_FIELDS};
    /// "Recorded inputs by app" title.
    pub const HEADER: PaneRect = PaneRect {
        x: 20.0,
        y: 300.0,
        w: 300.0,
        h: 24.0,
    };
    /// Column-header row, just above the first data row.
    pub const COL_HEADER_Y: f64 = 278.0;
    pub const COL_HEADER_H: f64 = 16.0;
    pub const NAME_HEADER: PaneRect = PaneRect {
        x: 20.0,
        y: COL_HEADER_Y,
        w: 150.0,
        h: COL_HEADER_H,
    };
    /// First data-row baseline; each row steps down by `ROW_STEP`.
    pub const ROW_BASE_Y: f64 = 250.0;
    pub const ROW_STEP: f64 = 26.0;
    const NAME_X: f64 = 20.0;
    const NAME_W: f64 = 150.0;
    const NAME_H: f64 = 20.0;
    /// Left edge of each checkbox column (header label sits at the same x).
    pub const COL_X: [f64; APP_POLICY_FIELDS] = [180.0, 232.0, 284.0, 336.0];
    const COL_W: f64 = 44.0;
    const CB_H: f64 = 18.0;
    const DELETE_X: f64 = 400.0;
    const DELETE_W: f64 = 70.0;
    const DELETE_H: f64 = 20.0;

    fn row_y(row: usize) -> f64 {
        ROW_BASE_Y - row as f64 * ROW_STEP
    }
    pub fn name_rect(row: usize) -> PaneRect {
        PaneRect {
            x: NAME_X,
            y: row_y(row),
            w: NAME_W,
            h: NAME_H,
        }
    }
    pub fn checkbox_rect(row: usize, field: usize) -> PaneRect {
        PaneRect {
            x: COL_X[field],
            y: row_y(row),
            w: COL_W,
            h: CB_H,
        }
    }
    pub fn delete_rect(row: usize) -> PaneRect {
        PaneRect {
            x: DELETE_X,
            y: row_y(row),
            w: DELETE_W,
            h: DELETE_H,
        }
    }
    pub fn col_header_rect(field: usize) -> PaneRect {
        PaneRect {
            x: COL_X[field],
            y: COL_HEADER_Y,
            w: COL_W,
            h: COL_HEADER_H,
        }
    }
}

fn build_window(
    mtm: MainThreadMarker,
    target: &Retained<SettingsTarget>,
    flags: &SettingsFlags,
) -> BuiltWindow {
    // 680 wide so all 9 tabs fit without truncating (was 520, which overflowed
    // once the Personalization pane brought the count to 9). Resizable so the
    // user can widen further if their tab labels need it.
    let frame = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(680.0, 420.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    // SAFETY: standard NSWindow init; releasedWhenClosed defaults are managed
    // by the Retained wrapper (we keep ownership and hide instead of free).
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("Compme Settings"));
    window.center();
    let mut stats_labels: Vec<Retained<NSTextField>> = Vec::new();
    let mut setup_labels: Vec<Retained<NSTextField>> = Vec::new();
    let mut setup_action_buttons: Vec<Retained<NSButton>> = Vec::new();
    let mut apps_labels: Vec<Retained<NSTextField>> = Vec::new();
    let mut apps_delete_buttons: Vec<Retained<NSButton>> = Vec::new();
    let mut apps_policy_checkboxes: Vec<Retained<NSButton>> = Vec::new();
    let mut switches: Vec<(Retained<NSSwitch>, Arc<AtomicBool>)> = Vec::new();
    // Personalization text fields, kept so show() can re-seed them from
    // flags.personalization_* after an out-of-window config reload (the same
    // staleness class the data-row labels guard against). Assigned once in the
    // unconditional Personalization block below (deferred init — no None seed).
    let personalization_instructions_field: Option<Retained<NSTextField>>;
    let personalization_name_field: Option<Retained<NSTextField>>;
    let personalization_email_field: Option<Retained<NSTextField>>;

    // Tab layout (c105): one NSTabView as the content view, one tab per
    // pane_titles() entry. Tab content is ~500x350; per-pane coordinates are
    // local to their tab view, so panes never collide with each other again
    // (the c103/c104 overlap class is structurally gone).
    let tabs = NSTabView::new(mtm);
    let pane_views: Vec<Retained<NSView>> = pane_titles()
        .iter()
        .map(|title| {
            let view = NSView::new(mtm);
            let item = NSTabViewItem::new();
            item.setLabel(&NSString::from_str(title));
            item.setView(Some(&view));
            tabs.addTabViewItem(&item);
            view
        })
        .collect();

    // Setup tab: readiness rows (permissions, model file). Strings come
    // from the run loop via flags.setup_lines; show() refreshes them on
    // every open. Grant/Request/Reveal buttons are the next slice.
    {
        let setup = &pane_views[0];
        let header = NSTextField::labelWithString(&NSString::from_str("Setup checklist"), mtm);
        header.setFrame(NSRect::new(
            NSPoint::new(20.0, 300.0),
            NSSize::new(300.0, 24.0),
        ));
        setup.addSubview(&header);
        let initial: Vec<String> = flags
            .setup_lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for row in 0..SETUP_ROWS {
            let text = initial.get(row).map(String::as_str).unwrap_or("");
            let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            label.setFrame(NSRect::new(
                NSPoint::new(20.0, 270.0 - row as f64 * 26.0),
                NSSize::new(440.0, 20.0),
            ));
            setup.addSubview(&label);
            setup_labels.push(label);
        }

        // Model picker: the download target. Item titles come from
        // flags.setup_model_menu_titles (model_catalog is app-side); the selected
        // index lands in flags.setup_model_index, which the run loop's download
        // edge reads. Built once and pre-selected from the current index — the
        // catalog is static and only this popup writes the index, so there is
        // no external writer to refresh-on-show against.
        {
            let picker_label =
                NSTextField::labelWithString(&NSString::from_str("Model to download:"), mtm);
            picker_label.setFrame(NSRect::new(
                NSPoint::new(20.0, 185.0),
                NSSize::new(140.0, 22.0),
            ));
            setup.addSubview(&picker_label);

            let popup = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                NSRect::new(NSPoint::new(165.0, 182.0), NSSize::new(300.0, 26.0)),
                false,
            );
            for title in &flags.setup_model_menu_titles {
                popup.addItemWithTitle(&NSString::from_str(title));
            }
            let selected = flags.setup_model_index.load(Ordering::Relaxed);
            if selected < flags.setup_model_menu_titles.len() {
                popup.selectItemAtIndex(selected as isize);
            }
            // SAFETY: target outlives the window (held by MacosSettingsWindow);
            // setTarget/setAction are the standard control-wiring calls.
            unsafe {
                let any: &AnyObject = target.as_ref();
                popup.setTarget(Some(any));
                popup.setAction(Some(sel!(selectModel:)));
            }
            setup.addSubview(&popup);
        }

        // Action buttons (always present; each is a harmless no-op when its
        // item is already ready). The privileged calls happen in the run
        // loop — buttons only set flags.
        let buttons: [(&str, objc2::runtime::Sel); 4] = [
            ("Grant Accessibility\u{2026}", sel!(grantAccessibility:)),
            (
                "Request Screen Recording\u{2026}",
                sel!(requestScreenRecording:),
            ),
            ("Reveal Model in Finder", sel!(revealModel:)),
            ("Download Model", sel!(downloadModel:)),
        ];
        for (i, (title, action)) in buttons.into_iter().enumerate() {
            // SAFETY: target outlives the window (held by MacosSettingsWindow).
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(title),
                    Some({
                        let any: &AnyObject = target.as_ref();
                        any
                    }),
                    Some(action),
                    mtm,
                )
            };
            button.setFrame(NSRect::new(
                NSPoint::new(20.0, 150.0 - i as f64 * 36.0),
                NSSize::new(230.0, 28.0),
            ));
            setup.addSubview(&button);
            setup_action_buttons.push(button);
        }
        refresh_setup_action_buttons(&setup_action_buttons, &initial);
    }

    // General tab: a uniform stack of global toggles (40px step), each two
    // views of one atomic — see SettingsFlags. Same table+loop idiom as the
    // Context tab below. Push order (enabled, midline, autocorrect, trailing)
    // is the refresh order.
    {
        let general = &pane_views[1];
        let rows: [(&str, &Arc<AtomicBool>, objc2::runtime::Sel); 4] = [
            (
                "Enable completions",
                &flags.general_enabled,
                sel!(toggleEnabled:),
            ),
            (
                "Mid-line completions (show even with text after the cursor)",
                &flags.labs_midline,
                sel!(toggleMidline:),
            ),
            (
                "Autocorrect typos (offer the fix as you type)",
                &flags.general_autocorrect,
                sel!(toggleAutocorrect:),
            ),
            (
                "Trailing space after single-word completions",
                &flags.general_trailing_space,
                sel!(toggleTrailingSpace:),
            ),
        ];
        for (row, (title, flag, action)) in rows.into_iter().enumerate() {
            let label = NSTextField::labelWithString(&NSString::from_str(title), mtm);
            label.setFrame(NSRect::new(
                NSPoint::new(20.0, 340.0 - row as f64 * 40.0),
                NSSize::new(400.0, 20.0),
            ));
            general.addSubview(&label);

            let switch = NSSwitch::new(mtm);
            switch.setFrame(NSRect::new(
                NSPoint::new(420.0, 336.0 - row as f64 * 40.0),
                NSSize::new(60.0, 26.0),
            ));
            switch.setState(if flag.load(Ordering::Relaxed) {
                objc2_app_kit::NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
            // SAFETY: target outlives the window (held by MacosSettingsWindow).
            unsafe {
                switch.setTarget(Some({
                    let any: &AnyObject = target.as_ref();
                    any
                }));
                switch.setAction(Some(action));
            }
            general.addSubview(&switch);
            switches.push((switch, Arc::clone(flag)));
        }
    }

    // Personalization tab: the three steering knobs (roadmap item 5) — global
    // instructions, sender identity (name/email), and steering strength. Each
    // control records a `PersonalizationEdit` into flags.personalization_edit;
    // the run loop applies it to its source profile, calls
    // `inference.set_profile` LIVE, and persists. Initial values come from the
    // run loop via flags.personalization_* so the pane reflects current config.
    // Text fields fire their action on Enter / focus-loss; the popup on select.
    {
        let pers = &pane_views[2];

        let gi_label = NSTextField::labelWithString(
            &NSString::from_str("Global instructions (steer every suggestion):"),
            mtm,
        );
        gi_label.setFrame(pers_layout::GI_LABEL.ns());
        pers.addSubview(&gi_label);

        // Editable multi-line NSTextField. labelWithString builds a non-editable
        // label, so we construct a plain field and turn editing on. Multi-line:
        // `setUsesSingleLineMode(false)` + a word-wrapping non-scrolling cell lets
        // the user enter wrapped, multi-paragraph steering text. Return still fires
        // the action (commit) through the field editor and Option-Return inserts a
        // newline, so the tested target/action commit path is preserved — no
        // delegate needed. (ponytail: NSTextView-in-NSScrollView is the richer
        // widget, but it needs a novel delegate + visual LOOK; this wrapping field
        // delivers multi-line entry with zero new FFI surface. LOOK still pending.)
        let gi_field = NSTextField::new(mtm);
        gi_field.setFrame(pers_layout::GI_FIELD.ns());
        gi_field.setStringValue(&NSString::from_str(
            &flags
                .personalization_instructions
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        ));
        gi_field.setEditable(true);
        gi_field.setSelectable(true);
        gi_field.setUsesSingleLineMode(false);
        if let Some(cell) = gi_field.cell() {
            cell.setWraps(true);
            cell.setScrollable(false);
        }
        // SAFETY: target outlives the window (held by MacosSettingsWindow);
        // setTarget/setAction are the standard control-wiring calls.
        unsafe {
            let any: &AnyObject = target.as_ref();
            gi_field.setTarget(Some(any));
            gi_field.setAction(Some(sel!(editGlobalInstructions:)));
        }
        pers.addSubview(&gi_field);

        // Sender name + email rows (templated into the prompt as the writer's
        // identity). Two single-line editable fields, same wiring.
        let name_label = NSTextField::labelWithString(&NSString::from_str("Your name:"), mtm);
        name_label.setFrame(pers_layout::NAME_LABEL.ns());
        pers.addSubview(&name_label);
        let name_field = NSTextField::new(mtm);
        name_field.setFrame(pers_layout::NAME_FIELD.ns());
        name_field.setStringValue(&NSString::from_str(
            &flags
                .personalization_sender_name
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        ));
        name_field.setEditable(true);
        name_field.setSelectable(true);
        // SAFETY: see gi_field above.
        unsafe {
            let any: &AnyObject = target.as_ref();
            name_field.setTarget(Some(any));
            name_field.setAction(Some(sel!(editSenderName:)));
        }
        pers.addSubview(&name_field);

        let email_label = NSTextField::labelWithString(&NSString::from_str("Your email:"), mtm);
        email_label.setFrame(pers_layout::EMAIL_LABEL.ns());
        pers.addSubview(&email_label);
        let email_field = NSTextField::new(mtm);
        email_field.setFrame(pers_layout::EMAIL_FIELD.ns());
        email_field.setStringValue(&NSString::from_str(
            &flags
                .personalization_sender_email
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        ));
        email_field.setEditable(true);
        email_field.setSelectable(true);
        // SAFETY: see gi_field above.
        unsafe {
            let any: &AnyObject = target.as_ref();
            email_field.setTarget(Some(any));
            email_field.setAction(Some(sel!(editSenderEmail:)));
        }
        pers.addSubview(&email_field);

        // Strength popup: one row per stop, addressed by index 0..=5 (0 = Off),
        // mapped run-loop-side via Strength::from_stop — the seam carries only
        // the usize, like the emoji skin-tone popup. Titles cross the seam in
        // flags.personalization_strength_titles (Strength lives in the
        // `personalization` crate, invisible here; the stat-range pattern).
        let strength_label =
            NSTextField::labelWithString(&NSString::from_str("Steering strength:"), mtm);
        strength_label.setFrame(pers_layout::STRENGTH_LABEL.ns());
        pers.addSubview(&strength_label);
        let strength_popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            pers_layout::STRENGTH_POPUP.ns(),
            false,
        );
        for title in &flags.personalization_strength_titles {
            strength_popup.addItemWithTitle(&NSString::from_str(title));
        }
        let selected = flags.personalization_strength_index.load(Ordering::Relaxed);
        if selected < flags.personalization_strength_titles.len() {
            strength_popup.selectItemAtIndex(selected as isize);
        }
        // SAFETY: see gi_field above.
        unsafe {
            let any: &AnyObject = target.as_ref();
            strength_popup.setTarget(Some(any));
            strength_popup.setAction(Some(sel!(selectStrength:)));
        }
        pers.addSubview(&strength_popup);
        // Keep the text fields so show() can re-seed them after an
        // out-of-window config reload (strength re-syncs via its atomic above).
        personalization_instructions_field = Some(gi_field);
        personalization_name_field = Some(name_field);
        personalization_email_field = Some(email_field);
    }

    // Apps tab: per-app recorded-input counts (encrypted memory store).
    // Strings come from the run loop via flags.apps_lines; refreshed on
    // every show like the other data tabs.
    {
        let apps = &pane_views[3];
        let header =
            NSTextField::labelWithString(&NSString::from_str("Recorded inputs by app"), mtm);
        header.setFrame(apps_layout::HEADER.ns());
        apps.addSubview(&header);

        // Column-header row: "App" over the names + a short title over each policy
        // column. The bare checkboxes below carry the full title as a tooltip.
        let name_header = NSTextField::labelWithString(&NSString::from_str("App"), mtm);
        name_header.setFrame(apps_layout::NAME_HEADER.ns());
        apps.addSubview(&name_header);
        for (field, title) in APP_POLICY_COLUMN_HEADERS.iter().enumerate() {
            let col = NSTextField::labelWithString(&NSString::from_str(title), mtm);
            col.setFrame(apps_layout::col_header_rect(field).ns());
            apps.addSubview(&col);
        }

        let initial: Vec<String> = flags
            .apps_lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // Per-row policy bits, composed by the run loop in the SAME order/cap as
        // apps_lines, so each row's checkboxes open reflecting the saved per-app
        // override. Applied via refresh_apps_policy_checkbox_states below (and on
        // every show()/refresh_apps_labels, like the labels/visibility).
        let initial_bits: Vec<[bool; APP_POLICY_FIELDS]> = flags
            .apps_policy_bits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for row in 0..APPS_ROWS {
            let text = initial.get(row).map(String::as_str).unwrap_or("");
            let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            label.setFrame(apps_layout::name_rect(row).ns());
            apps.addSubview(&label);
            apps_labels.push(label);

            // Per-row Delete: the tag carries the row index; the run loop
            // resolves it against apps_row_ids (same cap/order as the
            // lines) and deletes that app's history.
            // SAFETY: target outlives the window (held by MacosSettingsWindow).
            let delete = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str("Delete"),
                    Some({
                        let any: &AnyObject = target.as_ref();
                        any
                    }),
                    Some(sel!(deleteAppRow:)),
                    mtm,
                )
            };
            delete.setTag(row as isize);
            delete.setFrame(apps_layout::delete_rect(row).ns());
            // Hidden unless this row names a deletable app — refreshed on every
            // show()/refresh_apps_labels as the app list changes.
            delete.setHidden(!apps_row_is_deletable(text));
            apps.addSubview(&delete);
            apps_delete_buttons.push(delete);

            // Per-row policy checkboxes (Enabled / Tab / Mid-line / Autocorrect),
            // mirroring the Delete button: each carries a packed tag
            // (row * APP_POLICY_FIELDS + field) and shares ONE action method
            // (editAppPolicy:). Hidden on non-deletable (status/empty) rows for
            // the same reason Delete is. The run loop unpacks the tag, resolves
            // the row to an app id, and writes via prefs::set_app_policy_field.
            let deletable = apps_row_is_deletable(text);
            // The resolved per-app policy lives in `prefs` (run-loop side), which
            // this crate intentionally cannot see (apps_lines/index-seam pattern).
            // The run loop publishes the per-row bits via flags.apps_policy_bits;
            // refresh_apps_policy_checkbox_states (below) seeds the checked state.
            for (field, full_title) in APP_POLICY_FIELD_TITLES.iter().enumerate() {
                // Title-less checkbox — the column header + tooltip name it, so all
                // four toggles fit one compact line beside the app name.
                // SAFETY: target outlives the window (held by MacosSettingsWindow).
                let checkbox = unsafe {
                    NSButton::buttonWithTitle_target_action(
                        &NSString::from_str(""),
                        Some({
                            let any: &AnyObject = target.as_ref();
                            any
                        }),
                        Some(sel!(editAppPolicy:)),
                        mtm,
                    )
                };
                checkbox.setButtonType(NSButtonType::Switch);
                checkbox.setTag((row * APP_POLICY_FIELDS + field) as isize);
                checkbox.setFrame(apps_layout::checkbox_rect(row, field).ns());
                checkbox.setToolTip(Some(&NSString::from_str(full_title)));
                checkbox.setHidden(!deletable);
                apps.addSubview(&checkbox);
                apps_policy_checkboxes.push(checkbox);
            }
        }
        refresh_apps_policy_checkbox_states(&apps_policy_checkboxes, &initial_bits);
    }

    // Context tab: prompt-context sources. Clipboard applies live; screen OCR is
    // a persisted opt-in that can be disabled immediately but starts its worker
    // on the next launch.
    {
        let context = &pane_views[4];
        let rows: [(&str, &Arc<AtomicBool>, objc2::runtime::Sel); 2] = [
            (
                "Clipboard context",
                &flags.context_clipboard,
                sel!(toggleClipboardContext:),
            ),
            (
                "Screen OCR context (restart to enable)",
                &flags.context_screen,
                sel!(toggleScreenContext:),
            ),
        ];
        for (row, (title, flag, action)) in rows.into_iter().enumerate() {
            let label = NSTextField::labelWithString(&NSString::from_str(title), mtm);
            label.setFrame(NSRect::new(
                NSPoint::new(20.0, 320.0 - row as f64 * 40.0),
                NSSize::new(380.0, 20.0),
            ));
            context.addSubview(&label);

            let switch = NSSwitch::new(mtm);
            switch.setFrame(NSRect::new(
                NSPoint::new(420.0, 316.0 - row as f64 * 40.0),
                NSSize::new(60.0, 26.0),
            ));
            switch.setState(if flag.load(Ordering::Relaxed) {
                objc2_app_kit::NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
            // SAFETY: target outlives the window (held by MacosSettingsWindow).
            unsafe {
                switch.setTarget(Some({
                    let any: &AnyObject = target.as_ref();
                    any
                }));
                switch.setAction(Some(action));
            }
            context.addSubview(&switch);
            switches.push((switch, Arc::clone(flag)));
        }
    }

    // Emoji tab: enable switch plus skin-tone popup. The run loop owns policy
    // and persistence; the window only writes atomics.
    {
        let emoji = &pane_views[5];
        let label = NSTextField::labelWithString(
            &NSString::from_str("Emoji shortcode completions (:smile)"),
            mtm,
        );
        label.setFrame(NSRect::new(
            NSPoint::new(20.0, 320.0),
            NSSize::new(380.0, 20.0),
        ));
        emoji.addSubview(&label);

        let switch = NSSwitch::new(mtm);
        switch.setFrame(NSRect::new(
            NSPoint::new(420.0, 316.0),
            NSSize::new(60.0, 26.0),
        ));
        switch.setState(if flags.emoji_enabled.load(Ordering::Relaxed) {
            objc2_app_kit::NSControlStateValueOn
        } else {
            objc2_app_kit::NSControlStateValueOff
        });
        // SAFETY: target outlives the window (held by MacosSettingsWindow).
        unsafe {
            switch.setTarget(Some({
                let any: &AnyObject = target.as_ref();
                any
            }));
            switch.setAction(Some(sel!(toggleEmoji:)));
        }
        emoji.addSubview(&switch);
        switches.push((switch, Arc::clone(&flags.emoji_enabled)));

        let tone_label = NSTextField::labelWithString(&NSString::from_str("Skin tone"), mtm);
        tone_label.setFrame(NSRect::new(
            NSPoint::new(20.0, 280.0),
            NSSize::new(160.0, 20.0),
        ));
        emoji.addSubview(&tone_label);

        let tone_popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::new(220.0, 276.0), NSSize::new(180.0, 26.0)),
            false,
        );
        for title in [
            "Default",
            "Light",
            "Medium light",
            "Medium",
            "Medium dark",
            "Dark",
        ] {
            tone_popup.addItemWithTitle(&NSString::from_str(title));
        }
        let selected = flags.emoji_skin_tone_index.load(Ordering::Relaxed);
        if selected < 6 {
            tone_popup.selectItemAtIndex(selected as isize);
        }
        // SAFETY: target outlives the window (held by MacosSettingsWindow).
        unsafe {
            tone_popup.setTarget(Some({
                let any: &AnyObject = target.as_ref();
                any
            }));
            tone_popup.setAction(Some(sel!(selectEmojiSkinTone:)));
        }
        emoji.addSubview(&tone_popup);

        let gender_label = NSTextField::labelWithString(&NSString::from_str("Gender"), mtm);
        gender_label.setFrame(NSRect::new(
            NSPoint::new(20.0, 244.0),
            NSSize::new(160.0, 20.0),
        ));
        emoji.addSubview(&gender_label);

        let gender_popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::new(220.0, 240.0), NSSize::new(180.0, 26.0)),
            false,
        );
        // Order mirrors the app-side EMOJI_GENDER_VALUES table (index addresses it).
        for title in ["Neutral", "Female", "Male"] {
            gender_popup.addItemWithTitle(&NSString::from_str(title));
        }
        let selected = flags.emoji_gender_index.load(Ordering::Relaxed);
        if selected < 3 {
            gender_popup.selectItemAtIndex(selected as isize);
        }
        // SAFETY: target outlives the window (held by MacosSettingsWindow).
        unsafe {
            gender_popup.setTarget(Some({
                let any: &AnyObject = target.as_ref();
                any
            }));
            gender_popup.setAction(Some(sel!(selectEmojiGender:)));
        }
        emoji.addSubview(&gender_popup);
    }

    let shortcuts_label = {
        let shortcuts_view = &pane_views[6];
        let initial = flags
            .shortcuts_text
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default();
        let text = NSTextField::wrappingLabelWithString(&NSString::from_str(&initial), mtm);
        text.setFrame(NSRect::new(
            NSPoint::new(20.0, 160.0),
            NSSize::new(460.0, 170.0),
        ));
        text.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        text.setEditable(false);
        shortcuts_view.addSubview(&text);
        text
    };

    // Recorder fields (recorder 5b slice 4b): click a field, then press a key
    // to rebind that accept role live. Both write flags.shortcuts_rebind_request;
    // the run loop consumes the edge (set keymap -> rearm -> persist). Rows sit
    // below the effective-bindings text (y=160..330). LOOK-verified.
    let mut recorder_labels: Vec<(RecorderRole, Retained<NSTextField>)> = Vec::new();
    {
        let shortcuts_view = &pane_views[6];
        let (word, full) = crate::effective_accept_keys_with_mods();
        for (role, label_text, y) in [
            (RecorderRole::Word, "Accept word:", 116.0),
            (RecorderRole::Full, "Accept full:", 80.0),
        ] {
            let row_label = NSTextField::labelWithString(&NSString::from_str(label_text), mtm);
            row_label.setFrame(NSRect::new(NSPoint::new(20.0, y), NSSize::new(110.0, 22.0)));
            shortcuts_view.addSubview(&row_label);

            // Display field showing the role's current key (bezeled box), with
            // its ⌃⌥⇧⌘ glyph prefix if a modifier is bound (slice 2).
            let (code, mask) = match role {
                RecorderRole::Word => word,
                RecorderRole::Full => full,
            };
            let key_label = NSTextField::labelWithString(
                &NSString::from_str(&keycode_label_with_mods(code, mask)),
                mtm,
            );
            key_label.setBezeled(true);
            key_label.setDrawsBackground(true);
            key_label.setEditable(false);
            key_label.setSelectable(false); // selectable => field editor => hang
            let box_frame = NSRect::new(NSPoint::new(140.0, y - 4.0), NSSize::new(160.0, 24.0));
            key_label.setFrame(box_frame);
            shortcuts_view.addSubview(&key_label);

            // Transparent recorder overlay ON TOP of the display field.
            let recorder = KeyRecorderField::new(
                role,
                Arc::clone(&flags.shortcuts_rebind_request),
                key_label.clone(),
                mtm,
            );
            recorder.setFrame(box_frame);
            shortcuts_view.addSubview(&recorder);
            // Keep a handle so show() can re-sync the box to the effective key.
            recorder_labels.push((role, key_label));
        }
    }

    // Statistics tab: header + STATS_ROWS data rows. Row strings come from
    // the run loop via flags.stats_lines; show() refreshes them on every
    // open. Monospaced font keeps sparkline glyphs column-aligned.
    {
        let stats = &pane_views[7];
        let stats_header =
            NSTextField::labelWithString(&NSString::from_str("This session + lifetime"), mtm);
        // Width 220 (not 300) so the header clears the Range picker's label at
        // x=300; the string is ~150pt at this size so it isn't clipped.
        stats_header.setFrame(NSRect::new(
            NSPoint::new(20.0, 300.0),
            NSSize::new(220.0, 24.0),
        ));
        stats.addSubview(&stats_header);

        // Range + grouping pickers (Tier 3.3): select the trailing span and the
        // daily/weekly bucketing the run loop renders the rows over. Bare,
        // self-describing popups ("Last 7 days" / "Daily") on the header row —
        // range x=250..358, grouping x=364..472, both clearing the 220-wide
        // header (x=20..240) and sitting at y=297, above the data rows (y<=270).
        {
            let range_popup = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                NSRect::new(NSPoint::new(250.0, 297.0), NSSize::new(108.0, 26.0)),
                false,
            );
            for title in &flags.stat_range_titles {
                range_popup.addItemWithTitle(&NSString::from_str(title));
            }
            let selected = flags.stat_range_index.load(Ordering::Relaxed);
            if selected < flags.stat_range_titles.len() {
                range_popup.selectItemAtIndex(selected as isize);
            }
            // SAFETY: target outlives the window (held by MacosSettingsWindow);
            // setTarget/setAction are the standard control-wiring calls.
            unsafe {
                let any: &AnyObject = target.as_ref();
                range_popup.setTarget(Some(any));
                range_popup.setAction(Some(sel!(selectStatRange:)));
            }
            stats.addSubview(&range_popup);

            let group_popup = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                NSRect::new(NSPoint::new(364.0, 297.0), NSSize::new(108.0, 26.0)),
                false,
            );
            for title in &flags.stat_group_titles {
                group_popup.addItemWithTitle(&NSString::from_str(title));
            }
            let selected = flags.stat_group_index.load(Ordering::Relaxed);
            if selected < flags.stat_group_titles.len() {
                group_popup.selectItemAtIndex(selected as isize);
            }
            // SAFETY: as above — target outlives the window.
            unsafe {
                let any: &AnyObject = target.as_ref();
                group_popup.setTarget(Some(any));
                group_popup.setAction(Some(sel!(selectStatGroup:)));
            }
            stats.addSubview(&group_popup);
        }

        let initial: Vec<String> = flags
            .stats_lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // SAFETY: NSFontWeightRegular is a constant extern static.
        let mono = NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe {
            objc2_app_kit::NSFontWeightRegular
        });
        for row in 0..STATS_ROWS {
            let text = initial.get(row).map(String::as_str).unwrap_or("");
            let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            label.setFont(Some(&mono));
            label.setFrame(NSRect::new(
                NSPoint::new(20.0, 270.0 - row as f64 * 26.0),
                NSSize::new(440.0, 20.0),
            ));
            stats.addSubview(&label);
            stats_labels.push(label);
        }
    }

    // About tab: static for the process lifetime, so build-once is fine
    // here (unlike the Statistics rows above).
    {
        let about_view = &pane_views[8];
        let about =
            NSTextField::wrappingLabelWithString(&NSString::from_str(&flags.about_text), mtm);
        about.setFrame(NSRect::new(
            NSPoint::new(20.0, 160.0),
            NSSize::new(460.0, 170.0),
        ));
        // Display-only: selectable (lets the user copy the repo URL), not
        // editable. 11pt so the wrapped credits block keeps headroom
        // (review-c101).
        about.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        about.setEditable(false);
        about_view.addSubview(&about);
    }
    window.setContentView(Some(&tabs));
    let _ = &pane_views; // pane views are retained by their tab items
                         // Keep the instance alive across closes: AppKit's default releases a
                         // window on close, which would dangle our Retained pointer.
                         // SAFETY: documented NSWindow property setter.
    unsafe { window.setReleasedWhenClosed(false) };
    BuiltWindow {
        window,
        stats_labels,
        setup_labels,
        setup_action_buttons,
        apps_labels,
        apps_delete_buttons,
        apps_policy_checkboxes,
        switches,
        shortcuts_label,
        recorder_labels,
        personalization_instructions_field,
        personalization_name_field,
        personalization_email_field,
    }
}

/// Everything `build_window` hands back: the window plus the data-row label
/// handles each tab needs refreshed on show.
struct BuiltWindow {
    window: Retained<NSWindow>,
    stats_labels: Vec<Retained<NSTextField>>,
    setup_labels: Vec<Retained<NSTextField>>,
    setup_action_buttons: Vec<Retained<NSButton>>,
    apps_labels: Vec<Retained<NSTextField>>,
    apps_delete_buttons: Vec<Retained<NSButton>>,
    apps_policy_checkboxes: Vec<Retained<NSButton>>,
    switches: Vec<(Retained<NSSwitch>, Arc<AtomicBool>)>,
    shortcuts_label: Retained<NSTextField>,
    recorder_labels: Vec<(RecorderRole, Retained<NSTextField>)>,
    personalization_instructions_field: Option<Retained<NSTextField>>,
    personalization_name_field: Option<Retained<NSTextField>>,
    personalization_email_field: Option<Retained<NSTextField>>,
}

/// Max Setup row count (accessibility / screen recording / model file).
/// Public for the same reason as [`APPS_ROWS`]: the run loop's composer
/// pins against this count instead of a drifting literal.
pub const SETUP_ROWS: usize = 3;

/// Max Apps rows (top apps by recorded-input count, plus status lines).
/// Public: the run loop's line composer caps to this same number
/// (review-c108 — a drifting duplicate would silently waste label slots).
pub const APPS_ROWS: usize = 8;

/// Number of editable per-app policy fields rendered as checkboxes on each
/// Apps row, in tag-encoding order: 0=Enabled, 1=TabDisabled, 2=MidLine,
/// 3=Autocorrect. Mirrors `prefs::AppPolicyField`'s variant order; the run
/// loop maps the index back. A checkbox's tag is `row * APP_POLICY_FIELDS +
/// field`, so this is the modulus the run loop unpacks against.
pub const APP_POLICY_FIELDS: usize = 4;

/// Checkbox titles for the per-app policy fields, indexed the same as the tag
/// encoding (and `prefs::AppPolicyField`'s variant order). `TabDisabled` reads
/// as "Tab key" so the checked state means "Tab is a literal Tab here".
const APP_POLICY_FIELD_TITLES: [&str; APP_POLICY_FIELDS] =
    ["Enabled", "Tab key", "Mid-line", "Autocorrect"];

/// Short column headers for the compact one-line Apps grid. The bare checkboxes
/// carry the full [`APP_POLICY_FIELD_TITLES`] as tooltips; these label the
/// columns in the header row so the toggles are self-explanatory.
const APP_POLICY_COLUMN_HEADERS: [&str; APP_POLICY_FIELDS] = ["On", "Tab", "Mid", "AC"];

/// Fixed Statistics row count (shown / accepted / words / lifetime).
/// Public for the same reason as [`APPS_ROWS`]: the run loop's composer
/// pins against this count instead of a drifting literal.
pub const STATS_ROWS: usize = 4;

/// Number of settings tabs.
pub const PANE_COUNT: usize = 9;

/// Tab titles in display order (Cotypist order) — Setup first, About last;
/// new panes insert between, never around.
pub fn pane_titles() -> [&'static str; PANE_COUNT] {
    [
        "Setup",
        "General",
        "Personalization",
        "Apps",
        "Context",
        "Emoji",
        "Shortcuts",
        "Statistics",
        "About",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personalization_pane_layout_has_no_overlaps_within_budget() {
        // Deterministic layout check for the Personalization pane — the pane can't
        // be built off the test harness's non-main thread, so this proves the
        // "visual" property that matters most (no control overlaps another, and
        // every control stays inside the ~500x350 pane budget) without a running
        // window server. Regression guard for the multi-line GI_FIELD growth that
        // pushed the sender/strength rows down: a future edit that overlaps a row
        // or overflows the pane fails here instead of only showing up on a Mac.
        let all = pers_layout::ALL;

        // No two controls intersect (same-row label+field are x-disjoint at the
        // 140->145 gutter; rows are vertically disjoint).
        for (i, (na, a)) in all.iter().enumerate() {
            for (nb, b) in all.iter().skip(i + 1) {
                assert!(
                    !a.intersects(*b),
                    "{na} overlaps {nb}: {na}=({},{},{},{}) {nb}=({},{},{},{})",
                    a.x,
                    a.y,
                    a.w,
                    a.h,
                    b.x,
                    b.y,
                    b.w,
                    b.h
                );
            }
        }

        // Every control is on-pane (non-negative origin, right/top within budget).
        for (name, r) in all.iter() {
            assert!(r.x >= 0.0 && r.y >= 0.0, "{name} has a negative origin");
            assert!(
                r.x + r.w <= pers_layout::BUDGET_W,
                "{name} overflows pane width ({} > {})",
                r.x + r.w,
                pers_layout::BUDGET_W
            );
            assert!(
                r.y + r.h <= pers_layout::BUDGET_H,
                "{name} overflows pane height ({} > {})",
                r.y + r.h,
                pers_layout::BUDGET_H
            );
        }
    }

    #[test]
    fn apps_pane_grid_has_no_overlaps_within_budget() {
        // Regression for the old 2-line-per-row Apps layout whose 26px step drew
        // each row's four policy checkboxes over the NEXT row's app name (28
        // collisions with 8 rows — the geometry check that surfaced this). The
        // compact one-line grid must keep every control of every row collision-
        // free and inside the ~500x350 pane budget.
        let mut all: Vec<(&'static str, PaneRect)> = vec![
            ("header", apps_layout::HEADER),
            ("name_header", apps_layout::NAME_HEADER),
        ];
        for f in 0..APP_POLICY_FIELDS {
            all.push(("col_header", apps_layout::col_header_rect(f)));
        }
        for row in 0..APPS_ROWS {
            all.push(("name", apps_layout::name_rect(row)));
            all.push(("delete", apps_layout::delete_rect(row)));
            for f in 0..APP_POLICY_FIELDS {
                all.push(("checkbox", apps_layout::checkbox_rect(row, f)));
            }
        }

        for (i, (na, a)) in all.iter().enumerate() {
            for (nb, b) in all.iter().skip(i + 1) {
                assert!(
                    !a.intersects(*b),
                    "{na} overlaps {nb}: ({},{},{},{}) vs ({},{},{},{})",
                    a.x,
                    a.y,
                    a.w,
                    a.h,
                    b.x,
                    b.y,
                    b.w,
                    b.h
                );
            }
        }
        for (name, r) in &all {
            assert!(r.x >= 0.0 && r.y >= 0.0, "{name} has a negative origin");
            assert!(
                r.x + r.w <= pers_layout::BUDGET_W,
                "{name} overflows pane width ({} > {})",
                r.x + r.w,
                pers_layout::BUDGET_W
            );
            assert!(
                r.y + r.h <= pers_layout::BUDGET_H,
                "{name} overflows pane height ({} > {})",
                r.y + r.h,
                pers_layout::BUDGET_H
            );
        }
    }

    #[test]
    fn apps_row_is_deletable_only_for_count_rows() {
        // Drives per-row Delete-button visibility: only "app \u{2014} N" count
        // rows (apps_pane_lines) get a Delete button; status messages and empty
        // padding rows must NOT (2026-06-14 finding: buttons on empty rows).
        assert!(apps_row_is_deletable("com.apple.Safari \u{2014} 5"));
        assert!(apps_row_is_deletable("org.mozilla.firefox \u{2014} 1"));
        assert!(!apps_row_is_deletable("Input collection is off"));
        assert!(!apps_row_is_deletable("No recorded inputs yet"));
        assert!(!apps_row_is_deletable(""));
    }

    #[test]
    fn setup_action_available_matches_exact_setup_row_label() {
        let lines = vec!["\u{2717} Accessibility helper".to_string()];
        assert!(!setup_action_available(&lines, "Accessibility", false));

        let exact_missing = vec!["\u{2717} Accessibility".to_string()];
        assert!(setup_action_available(
            &exact_missing,
            "Accessibility",
            false
        ));

        let exact_ready = vec!["\u{2713} Model file".to_string()];
        assert!(setup_action_available(&exact_ready, "Model file", true));
        assert!(!setup_action_available(&exact_ready, "Model", true));

        let ready_permission = vec!["\u{2713} Accessibility".to_string()];
        assert!(!setup_action_available(
            &ready_permission,
            "Accessibility",
            false
        ));

        let missing_model = vec!["\u{2717} Model file".to_string()];
        assert!(!setup_action_available(&missing_model, "Model file", true));
    }

    #[test]
    fn pane_titles_are_fixed_and_ordered() {
        // Tab order is part of the settings UX contract (Cotypist order):
        // Setup first, About last. New panes insert between, never around.
        assert_eq!(
            pane_titles().as_slice(),
            &[
                "Setup",
                "General",
                "Personalization",
                "Apps",
                "Context",
                "Emoji",
                "Shortcuts",
                "Statistics",
                "About"
            ]
        );
        assert_eq!(pane_titles().len(), PANE_COUNT);
    }

    #[test]
    fn apps_policy_tag_packs_and_unpacks_as_an_exact_inverse() {
        // Each per-row policy checkbox carries a packed tag
        // `row * APP_POLICY_FIELDS + field` (editAppPolicy:), which the refresh
        // helpers and the run loop unpack as `tag / APP_POLICY_FIELDS` (row) and
        // `tag % APP_POLICY_FIELDS` (field). The two must be exact inverses over
        // every seeded checkbox, INCLUDING the last row (a base mismatch or a
        // `/`<->`%` swap would mis-route the highest rows silently).
        for row in 0..APPS_ROWS {
            for field in 0..APP_POLICY_FIELDS {
                let tag = row * APP_POLICY_FIELDS + field;
                assert_eq!(tag / APP_POLICY_FIELDS, row, "row of tag {tag}");
                assert_eq!(tag % APP_POLICY_FIELDS, field, "field of tag {tag}");
            }
        }

        // The pack is a bijection: the row-major sweep visits 0..ROWS*FIELDS once
        // each with no gaps or collisions, so two distinct (row, field) cells can
        // never share a tag (which would alias two checkboxes onto one app slot).
        let mut tags: Vec<usize> = (0..APPS_ROWS)
            .flat_map(|row| {
                (0..APP_POLICY_FIELDS).map(move |field| row * APP_POLICY_FIELDS + field)
            })
            .collect();
        let count = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), count, "tags must be unique across all cells");
        assert_eq!(tags, (0..APPS_ROWS * APP_POLICY_FIELDS).collect::<Vec<_>>());
    }

    #[test]
    fn record_decision_esc_cancels_even_over_collision() {
        // Esc is BOTH a fixed key and the cancel gesture — cancel wins, even
        // when Esc would also collide with the other role (impossible today,
        // pinned anyway: the match arm ordering is the contract).
        assert_eq!(record_decision(53, 0, (53, 0)), RecordDecision::Cancel);
        assert_eq!(record_decision(53, 0, (48, 0)), RecordDecision::Cancel);
        // Esc stays the cancel gesture even with a modifier held (slice 2):
        // you can't bind Shift+Esc — Esc still cancels recording.
        assert_eq!(
            record_decision(53, crate::CARBON_SHIFT_KEY, (48, 0)),
            RecordDecision::Cancel
        );
    }

    #[test]
    fn record_decision_rejects_fixed_down_silently() {
        assert_eq!(
            record_decision(125, 0, (48, 0)),
            RecordDecision::RejectFixed
        );
        // Down stays the reserved cycle key even with a modifier held (slice 2).
        assert_eq!(
            record_decision(125, crate::CARBON_CONTROL_KEY, (48, 0)),
            RecordDecision::RejectFixed
        );
    }

    #[test]
    fn record_decision_rejects_the_other_roles_key() {
        // Capturing the OTHER role's EXACT (keycode, mask) would collide at
        // from_accept_keys_with_mods — reject in the field, stay recording.
        assert_eq!(
            record_decision(48, 0, (48, 0)),
            RecordDecision::RejectCollision
        );
        assert_eq!(
            record_decision(48, crate::CARBON_SHIFT_KEY, (48, crate::CARBON_SHIFT_KEY)),
            RecordDecision::RejectCollision
        );
    }

    #[test]
    fn record_decision_same_keycode_different_mask_is_not_a_collision() {
        // Slice 2: collision is the FULL (keycode, mask) identity, matching
        // from_accept_keys_with_mods. Tab (48,0) and Shift+Tab (48,SHIFT) are
        // distinct bindings that coexist — capturing one while the other role
        // holds the same keycode under a DIFFERENT mask must ACCEPT, not reject.
        assert_eq!(
            record_decision(48, 0, (48, crate::CARBON_SHIFT_KEY)),
            RecordDecision::Accept
        );
        assert_eq!(
            record_decision(48, crate::CARBON_SHIFT_KEY, (48, 0)),
            RecordDecision::Accept
        );
    }

    #[test]
    fn record_decision_accepts_normal_keys_including_own_current() {
        assert_eq!(record_decision(122, 0, (50, 0)), RecordDecision::Accept); // F1
                                                                              // Re-recording the role's OWN current key is a harmless no-op rebind.
        assert_eq!(record_decision(48, 0, (50, 0)), RecordDecision::Accept);
    }

    #[test]
    fn recorder_outcome_accept_writes_both_slots_and_labels_the_captured_key() {
        // Recording WORD with current (word=48, full=50), capture 122 (F1): the
        // request carries BOTH slots (full stays 50 — clobber-safe) and the
        // label is the captured key's.
        assert_eq!(
            recorder_outcome(RecorderRole::Word, 122, 0, ((48, 0), (50, 0))),
            RecorderOutcome::Accept {
                request: (Some((122, 0)), Some((50, 0))),
                label: keycode_label_with_mods(122, 0),
            }
        );
        // Recording FULL keeps word's current in slot 0.
        assert_eq!(
            recorder_outcome(RecorderRole::Full, 122, 0, ((48, 0), (50, 0))),
            RecorderOutcome::Accept {
                request: (Some((48, 0)), Some((122, 0))),
                label: keycode_label_with_mods(122, 0),
            }
        );
    }

    #[test]
    fn recorder_outcome_captures_the_modifier_mask_and_glyph_labels_it() {
        // Slice 2 core: capturing Shift+F1 while WORD records lands a MASKED
        // request (122, SHIFT) and a glyph-prefixed label; FULL's current is
        // carried through. This is the whole point of the slice — a bare
        // recorder would drop the Shift and bind plain F1.
        let outcome = recorder_outcome(
            RecorderRole::Word,
            122,
            crate::CARBON_SHIFT_KEY,
            ((48, 0), (50, 0)),
        );
        assert_eq!(
            outcome,
            RecorderOutcome::Accept {
                request: (Some((122, crate::CARBON_SHIFT_KEY)), Some((50, 0))),
                label: keycode_label_with_mods(122, crate::CARBON_SHIFT_KEY),
            }
        );
        // The label actually carries the ⇧ glyph (not just the bare key name).
        assert!(
            keycode_label_with_mods(122, crate::CARBON_SHIFT_KEY).starts_with('\u{21e7}'),
            "the captured Shift modifier prefixes the label with ⇧"
        );
    }

    #[test]
    fn recorder_outcome_preserves_the_other_roles_mask_when_rebinding_one_role() {
        // Audit-r2 at its true source: rebinding WORD must not strip a modifier
        // off FULL. FULL is currently Shift+backtick (50, SHIFT); capturing a
        // bare key for WORD carries FULL's (50, SHIFT) through verbatim, so the
        // downstream apply_live_accept_keymap can set it without reconstruction.
        assert_eq!(
            recorder_outcome(
                RecorderRole::Word,
                122,
                0,
                ((48, 0), (50, crate::CARBON_SHIFT_KEY))
            ),
            RecorderOutcome::Accept {
                request: (Some((122, 0)), Some((50, crate::CARBON_SHIFT_KEY))),
                label: keycode_label_with_mods(122, 0),
            }
        );
    }

    #[test]
    fn recorder_outcome_full_role_carries_words_masked_current_into_slot_zero() {
        // The Full-role symmetry of the c134 wrong-slot-clobber trap: rebinding
        // FULL must not strip WORD back to default. WORD currently holds a masked
        // binding (48, SHIFT); capturing a masked key (122, SHIFT) for FULL must
        // carry WORD's (48, SHIFT) through verbatim in slot 0 (the tuple-swap arm
        // of rebind_request_for) and land FULL's masked capture in slot 1. A
        // regression that wrote the captured key to slot 0 (Word's), or dropped
        // Word's mask, would clobber the wrong role — exactly what c134 traps.
        assert_eq!(
            recorder_outcome(
                RecorderRole::Full,
                122,
                crate::CARBON_SHIFT_KEY,
                ((48, crate::CARBON_SHIFT_KEY), (50, 0))
            ),
            RecorderOutcome::Accept {
                request: (
                    Some((48, crate::CARBON_SHIFT_KEY)),
                    Some((122, crate::CARBON_SHIFT_KEY))
                ),
                label: keycode_label_with_mods(122, crate::CARBON_SHIFT_KEY),
            }
        );
        // The label carries the ⇧ glyph for the captured FULL key.
        assert!(
            keycode_label_with_mods(122, crate::CARBON_SHIFT_KEY).starts_with('\u{21e7}'),
            "the captured Shift modifier prefixes the Full label with ⇧"
        );
    }

    #[test]
    fn recorder_outcome_esc_cancels_and_reverts_to_the_roles_current_label() {
        // Esc reverts to the role's OWN current key label (with its glyph), not
        // the other role.
        assert_eq!(
            recorder_outcome(
                RecorderRole::Full,
                crate::KEYCODE_ESCAPE,
                0,
                ((48, 0), (50, crate::CARBON_SHIFT_KEY))
            ),
            RecorderOutcome::Cancel {
                idle_label: keycode_label_with_mods(50, crate::CARBON_SHIFT_KEY)
            }
        );
        assert_eq!(
            recorder_outcome(
                RecorderRole::Word,
                crate::KEYCODE_ESCAPE,
                0,
                ((48, 0), (50, 0))
            ),
            RecorderOutcome::Cancel {
                idle_label: keycode_label_with_mods(48, 0)
            }
        );
    }

    #[test]
    fn recorder_outcome_down_is_rejected_silently_and_collision_shows_a_hint() {
        assert_eq!(
            recorder_outcome(
                RecorderRole::Word,
                crate::KEYCODE_DOWN,
                0,
                ((48, 0), (50, 0))
            ),
            RecorderOutcome::RejectSilent
        );
        // Recording WORD, capture (50,0) == full's current → collision, no write.
        assert!(matches!(
            recorder_outcome(RecorderRole::Word, 50, 0, ((48, 0), (50, 0))),
            RecorderOutcome::RejectCollision { .. }
        ));
    }

    #[test]
    fn rebind_request_carries_the_other_roles_current_key() {
        // THE clobber trap: RebindRequest None = DEFAULT (from_accept_keys
        // default-fills), NOT "keep current" — a bare-None partial request
        // would reset the other role's prior rebind back to Tab/backtick.
        // The recorder therefore always sends BOTH slots.
        assert_eq!(
            rebind_request_for(RecorderRole::Word, (122, 0), ((48, 0), (99, 0))),
            (Some((122, 0)), Some((99, 0)))
        );
        assert_eq!(
            rebind_request_for(RecorderRole::Full, (122, 0), ((99, 0), (50, 0))),
            (Some((99, 0)), Some((122, 0)))
        );
        // The other role's MASK is carried verbatim, not dropped (audit-r2).
        assert_eq!(
            rebind_request_for(
                RecorderRole::Word,
                (122, crate::CARBON_SHIFT_KEY),
                ((48, 0), (99, crate::CARBON_CONTROL_KEY))
            ),
            (
                Some((122, crate::CARBON_SHIFT_KEY)),
                Some((99, crate::CARBON_CONTROL_KEY))
            )
        );
    }

    #[test]
    fn keycode_label_names_known_keys_and_falls_back() {
        assert_eq!(keycode_label(48), "Tab");
        assert_eq!(keycode_label(50), "` (backtick)");
        assert_eq!(keycode_label(53), "Esc");
        assert_eq!(keycode_label(125), "Down arrow");
        assert_eq!(keycode_label(96), "F5");
        assert_eq!(keycode_label(12), "Q");
        assert_eq!(keycode_label(20), "3");
        assert_eq!(keycode_label(49), "Space");
        assert_eq!(keycode_label(7), "X");
        // Punctuation / symbol keys (US ANSI) — the LOOK gap: a rebind to
        // Control+] used to render "⌃key 30" instead of "⌃]".
        assert_eq!(keycode_label(30), "]");
        assert_eq!(keycode_label(42), "\\");
        assert_eq!(keycode_label(33), "[");
        assert_eq!(keycode_label(27), "-");
        assert_eq!(keycode_label(41), ";");
        assert_eq!(keycode_label(44), "/");
        // Arrows (Down is named above via KEYCODE_DOWN).
        assert_eq!(keycode_label(123), "Left arrow");
        assert_eq!(keycode_label(126), "Up arrow");
        // Unknown keycode → a readable generic, never a crash.
        assert_eq!(keycode_label(200), "key 200");
    }

    #[test]
    fn keycode_label_with_mods_prefixes_modifier_glyphs() {
        // No modifiers → identical to the bare label (back-compat display).
        assert_eq!(keycode_label_with_mods(48, 0), "Tab");
        // A single modifier prepends its macOS glyph.
        assert_eq!(
            keycode_label_with_mods(48, crate::CARBON_SHIFT_KEY),
            "\u{21e7}Tab"
        );
        // All four render in the canonical macOS order ⌃⌥⇧⌘ regardless of the
        // bitwise OR order they were combined in.
        let all = crate::CARBON_CMD_KEY
            | crate::CARBON_SHIFT_KEY
            | crate::CARBON_OPTION_KEY
            | crate::CARBON_CONTROL_KEY;
        assert_eq!(
            keycode_label_with_mods(48, all),
            "\u{2303}\u{2325}\u{21e7}\u{2318}Tab"
        );
        // Unknown keycode still falls back to the generic, with the prefix.
        assert_eq!(
            keycode_label_with_mods(200, crate::CARBON_CONTROL_KEY),
            "\u{2303}key 200"
        );
    }

    #[test]
    fn policy_restores_only_on_the_visible_to_hidden_edge() {
        assert!(policy_restore_needed(true, false), "close edge demotes");
        assert!(
            !policy_restore_needed(true, true),
            "still open: keep Regular"
        );
        assert!(!policy_restore_needed(false, false), "never opened: no-op");
        assert!(
            !policy_restore_needed(false, true),
            "open edge is show()'s job"
        );
    }
}
