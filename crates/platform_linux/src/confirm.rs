//! Blocking modal confirmation for Linux (ROADMAP Phase 2.6), through `zenity`.
//!
//! **`kdialog` was removed after measurement, not chosen against.** Two things
//! were confirmed against kdialog 26.04.3 under Xvfb. First, `--warningyesno`
//! takes a *value*, so the `--` end-of-options separator this module used to pass
//! was consumed as the message text and the real message never reached the
//! dialog. Second — and the reason the fix is removal rather than
//! `--warningyesno=<message>` — kdialog has no equivalent of zenity's
//! `--default-cancel`: with the corrected argv, **Return still exits 0**, i.e.
//! confirms. That breaks the one invariant this module exists to hold, on the
//! prompts that gate an irreversible delete and a deep-link trust change. A
//! helper that cannot decline by default cannot implement this contract, so a
//! host without zenity now gets a reported failure naming what to install.
//!
//! There is no toolkit-neutral system dialog on Linux, and linking GTK would make
//! the binary refuse to *start* where GTK is absent — the same reason the AT-SPI
//! path speaks D-Bus instead of libatspi (see `crate::atspi_live`). A helper
//! process is spawned, so a host without it degrades to a reported failure
//! rather than a broken executable.
//!
//! The invariants this module exists to hold:
//! - **`Ok(true)` means an explicit confirm click, nothing else.** Only the
//!   helper's success exit code (0) maps to `true`; every other code, a signal,
//!   and every spawn failure map to decline-or-error.
//! - **The confirming button is not the default.** zenity gets
//!   `--default-cancel`, so Return and Escape both decline.
//! - **No shell, ever.** The title/message/label are `argv` elements, so shell
//!   metacharacters are inert; each is passed in `--option=value` form so a value
//!   beginning with `--` cannot be read as another option.
//! - **No helper, and no display at all, are errors** — never a silent confirm,
//!   and never a silent decline either. zenity exits 1 for both "user cancelled"
//!   and "could not open the display", so the display is pre-flighted before any
//!   spawn (see `session_display_problem`).
//!
//! `ExitStatus` cannot be constructed portably, so the seam these functions take
//! yields `Option<i32>` (`ExitStatus::code()`): the argv construction, the
//! helper-chain fallthrough, and the code→verdict mapping are then testable on
//! every host, including with a fake helper binary. Only `Command::status()`
//! itself is untested glue.

use platform::PlatformError;
use shell_flags::ConfirmPrompt;

/// The helpers, in preference order. One entry: see the module docs for why
/// `kdialog` cannot be in this list.
pub const HELPER_CHAIN: [ConfirmHelper; 1] = [ConfirmHelper::Zenity];

/// Label of the declining button. Fixed rather than taken from `ConfirmPrompt`,
/// which carries only the confirming label — matching the macOS alert, whose
/// first (default) button is always "Cancel".
pub const DECLINE_LABEL: &str = "Cancel";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmHelper {
    Zenity,
}

/// One helper's answer.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfirmOutcome {
    /// The confirming button was clicked.
    Confirmed,
    /// The user declined: cancel, close, Escape, or a dialog timeout.
    Declined,
    /// The helper did not collect an answer (bad options, no display, killed).
    /// Never a confirm, and the reason is carried so the caller can report which
    /// helpers it tried and how each failed.
    Failed(String),
}

impl ConfirmHelper {
    pub fn program(self) -> &'static str {
        match self {
            Self::Zenity => "zenity",
        }
    }

    /// The exact `argv` (after the program name) for `prompt`.
    ///
    /// zenity: `--question` with `--default-cancel`, which gives the Cancel
    /// button focus so Return declines, and `--no-markup`, so a message
    /// containing `<` or `&` is shown literally instead of being parsed as Pango
    /// markup (or failing to render at all).
    ///
    pub fn args(self, prompt: &ConfirmPrompt<'_>) -> Vec<String> {
        // Every field is a glued `--option=value` element, so a title, message, or
        // label that begins with `--` stays a value instead of being parsed as an
        // option. No positional arguments at all: the previous kdialog form needed
        // one, guarded it with a bare `--`, and lost the message to an option that
        // takes a value (see the module docs).
        match self {
            Self::Zenity => vec![
                "--question".to_string(),
                "--no-markup".to_string(),
                "--default-cancel".to_string(),
                format!("--title={}", prompt.title),
                format!("--text={}", prompt.message),
                format!("--ok-label={}", prompt.confirm_label),
                format!("--cancel-label={DECLINE_LABEL}"),
            ],
        }
    }

    /// Map the helper's exit code to a verdict. `None` is a signal death.
    ///
    /// zenity: 0 confirm, 1 cancel/close/Escape, 5 `--timeout` expiry, anything
    /// else (255 on a bad option) is a failure. Measured on zenity 4.2.2.
    pub fn outcome(self, code: Option<i32>) -> ConfirmOutcome {
        match (self, code) {
            (_, Some(0)) => ConfirmOutcome::Confirmed,
            (Self::Zenity, Some(1 | 5)) => ConfirmOutcome::Declined,
            (_, Some(other)) => ConfirmOutcome::Failed(format!(
                "{} exited with {other}, which is not an answer",
                self.program()
            )),
            (_, None) => {
                ConfirmOutcome::Failed(format!("{} was killed by a signal", self.program()))
            }
        }
    }
}

/// `Some(reason)` when the session cannot show any GUI dialog at all.
///
/// **This exists because zenity's exit code cannot express it.** Measured on
/// zenity 4.2.2: with no `DISPLAY`, and with a `DISPLAY` pointing at a dead
/// server, zenity exits **1** — the same code as the user clicking Cancel. Left
/// to the exit code alone, a headless session turns "delete everything?" into a
/// silent `Ok(false)`: the user clicks Delete, nothing happens, nothing is
/// logged. stderr cannot break the tie either — a *genuine* Cancel under Xvfb
/// emitted 134 bytes of libEGL warnings, so "wrote to stderr" is not a failure
/// signal.
///
/// Checking the environment first is the part that *is* decidable, so it is
/// pure and tested on every host. A `DISPLAY` that is set but broken still
/// reads as a decline; that residual is documented rather than guessed at,
/// because distinguishing it means matching localized GTK warning text.
pub fn session_display_problem(display: Option<&str>, wayland: Option<&str>) -> Option<String> {
    let present = |value: Option<&str>| value.is_some_and(|value| !value.is_empty());
    if present(display) || present(wayland) {
        return None;
    }
    Some("neither DISPLAY nor WAYLAND_DISPLAY is set, so no dialog could be shown".to_string())
}

/// Walk the helper chain with `run`, returning the first real answer.
///
/// `display_problem` short-circuits the whole chain: when the session has no
/// display there is nothing to spawn, and reporting that is the only way it does
/// not masquerade as a decline (see [`session_display_problem`]).
///
/// A helper that cannot be spawned (not installed) or that fails to collect an
/// answer is skipped and the next one tried; when none answers, the error names
/// every helper and why it failed, so an operator does not have to guess which
/// package is missing. Only an explicit confirm returns `Ok(true)`.
pub fn confirm_with(
    prompt: &ConfirmPrompt<'_>,
    display_problem: Option<String>,
    mut run: impl FnMut(&str, &[String]) -> std::io::Result<Option<i32>>,
) -> Result<bool, PlatformError> {
    if let Some(reason) = display_problem {
        return Err(PlatformError::CannotComplete { reason });
    }
    let mut failures = Vec::new();
    for helper in HELPER_CHAIN {
        let args = helper.args(prompt);
        match run(helper.program(), &args) {
            Ok(code) => match helper.outcome(code) {
                ConfirmOutcome::Confirmed => return Ok(true),
                ConfirmOutcome::Declined => return Ok(false),
                ConfirmOutcome::Failed(reason) => failures.push(reason),
            },
            Err(err) => failures.push(format!("{}: {err}", helper.program())),
        }
    }
    Err(PlatformError::CannotComplete {
        reason: format!(
            "no usable confirmation dialog (install zenity): {}",
            failures.join("; ")
        ),
    })
}

/// Spawn `program` with `args`, wait for it, and report its exit code. The
/// blocking wait is the point: [`platform::shell::ShellHost::confirm`] is a
/// blocking modal confirm.
pub fn spawn_and_wait(program: &str, args: &[String]) -> std::io::Result<Option<i32>> {
    Ok(std::process::Command::new(program)
        .args(args)
        .status()?
        .code())
}

/// Live zenity test. In a sibling file (a `#[path]` module) rather than inline,
/// matching how `run_loop` and `platform_macos` keep their tests — see the repo
/// brief's "Where tests live".
#[cfg(all(test, target_os = "linux"))]
#[path = "confirm_live_tests.rs"]
mod live_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt<'a>(title: &'a str, message: &'a str, confirm_label: &'a str) -> ConfirmPrompt<'a> {
        ConfirmPrompt {
            title,
            message,
            confirm_label,
        }
    }

    #[test]
    fn zenity_argv_makes_cancel_the_default_and_disables_markup() {
        let args = ConfirmHelper::Zenity.args(&prompt("Allow link?", "Open <b>x</b>?", "Allow"));
        assert_eq!(
            args,
            vec![
                "--question",
                "--no-markup",
                "--default-cancel",
                "--title=Allow link?",
                "--text=Open <b>x</b>?",
                "--ok-label=Allow",
                "--cancel-label=Cancel",
            ]
        );
        // The contract's core: Return must decline, so --default-cancel is not
        // optional, and the confirming label must never become the OK *default*.
        assert!(args.contains(&"--default-cancel".to_string()));
    }

    #[test]
    fn the_helper_chain_contains_only_helpers_that_can_decline_by_default() {
        // kdialog was in this chain and had to come out. Measured against kdialog
        // 26.04.3: `--warningyesno` takes a value, so the `--` separator became
        // the message text and the real message vanished; and with the corrected
        // `--warningyesno=<message>` form Return *still* exits 0, because kdialog
        // has no `--default-cancel`. A helper whose default button confirms cannot
        // implement `ShellHost::confirm`, whose contract is that Return declines.
        //
        // This test is the guard against re-adding one: every helper in the chain
        // must pass an argument that makes the declining button the default.
        for helper in HELPER_CHAIN {
            let args = helper.args(&prompt("T", "M", "Delete"));
            assert!(
                args.iter().any(|a| a == "--default-cancel"),
                "{} has no default-decline argument, so Return would confirm",
                helper.program()
            );
        }
    }

    #[test]
    fn hostile_prompt_text_stays_one_argv_element_per_field() {
        // A message that is shell metacharacters, starts with `--`, and contains
        // newlines and quotes: there is no shell, and the glued `--option=value`
        // form means none of it can be read as an option. There are no positional
        // arguments left to guard.
        let nasty = "--yes-label=Pwn; rm -rf ~ && echo \"$(id)\"\n`whoami`";
        // Prefixes this module chooses itself; anything else starting with `--`
        // would be an option the prompt text smuggled in.
        const OURS: [&str; 5] = [
            "--title=",
            "--text=",
            "--ok-label=",
            "--yes-label=",
            "--no-label=",
        ];
        for helper in HELPER_CHAIN {
            let args = helper.args(&prompt(nasty, nasty, nasty));
            let carrying: Vec<(usize, &String)> = args
                .iter()
                .enumerate()
                .filter(|(_, a)| a.contains("rm -rf"))
                .collect();
            assert_eq!(
                carrying.len(),
                3,
                "{helper:?} must pass title/message/label through verbatim, once each: {args:?}"
            );
            for (i, arg) in carrying {
                let is_ours = OURS.iter().any(|p| arg.starts_with(p));
                let is_positional_after_separator = i > 0 && args[i - 1] == "--";
                assert!(
                    is_ours || is_positional_after_separator,
                    "{helper:?} argv[{i}] = {arg:?} could be parsed as an option"
                );
                if is_positional_after_separator {
                    assert_eq!(arg, nasty, "positional text must be verbatim");
                }
            }
        }
    }

    #[test]
    fn only_exit_zero_confirms() {
        for helper in HELPER_CHAIN {
            assert_eq!(helper.outcome(Some(0)), ConfirmOutcome::Confirmed);
            assert_eq!(helper.outcome(Some(1)), ConfirmOutcome::Declined);
            // Anything unrecognized is a failure, never a confirm: 255 is
            // zenity's "could not open the display", 2 is kdialog's usage error.
            for code in [2, 3, 5, 127, 255, -1] {
                let outcome = helper.outcome(Some(code));
                if helper == ConfirmHelper::Zenity && code == 5 {
                    // zenity's --timeout expiry: nobody answered, so decline.
                    assert_eq!(outcome, ConfirmOutcome::Declined);
                    continue;
                }
                assert!(
                    matches!(outcome, ConfirmOutcome::Failed(_)),
                    "{helper:?} code {code} must not be an answer: {outcome:?}"
                );
            }
            assert!(matches!(helper.outcome(None), ConfirmOutcome::Failed(_)));
        }
    }

    #[test]
    fn an_absent_zenity_is_an_error_naming_it_and_never_a_confirm() {
        // With kdialog out of the chain there is nothing to fall through to, and
        // the important half is what that does *not* do: a host without zenity must
        // get a reported failure that says what to install, never a silent
        // `Ok(true)` and never a silent `Ok(false)` that looks like the user
        // declining something they were never shown.
        let mut tried = Vec::new();
        let error = confirm_with(&prompt("t", "m", "c"), None, |program, _| {
            tried.push(program.to_string());
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No such file or directory",
            ))
        })
        .expect_err("no usable helper must be an error");

        assert_eq!(tried, vec!["zenity"]);
        let PlatformError::CannotComplete { reason } = error else {
            panic!("expected CannotComplete, got {error:?}");
        };
        assert!(
            reason.contains("zenity"),
            "the error must name the helper to install: {reason}"
        );
    }

    #[test]
    fn a_declining_helper_short_circuits_the_chain() {
        // A real answer must not be second-guessed by another dialog.
        let mut tried = Vec::new();
        let confirmed = confirm_with(&prompt("t", "m", "c"), None, |program, _| {
            tried.push(program.to_string());
            Ok(Some(1))
        })
        .unwrap();

        assert!(!confirmed);
        assert_eq!(tried, vec!["zenity"], "the first answer wins");
    }

    #[test]
    fn no_helper_at_all_fails_closed_and_names_what_to_install() {
        let result = confirm_with(&prompt("t", "m", "c"), None, |program, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{program} missing"),
            ))
        });

        let Err(PlatformError::CannotComplete { reason }) = result else {
            panic!("a host with no dialog helper must fail closed, got {result:?}");
        };
        assert!(reason.contains("zenity"), "{reason}");
        // kdialog cannot implement this contract (module docs), so telling an
        // operator to install it is advice that cannot work.
        assert!(
            !reason.contains("kdialog"),
            "the remediation must not name a helper that cannot decline by default: {reason}"
        );
        assert!(
            reason.contains("install"),
            "the error should say what to install: {reason}"
        );
    }

    #[test]
    fn a_session_with_no_display_is_reported_and_never_read_as_a_decline() {
        // The bug this guards: zenity exits 1 both for "user clicked Cancel" and
        // for "could not open the display" (measured, 4.2.2). Without the
        // pre-flight, a headless run of the delete-everything prompt returns
        // Ok(false) — indistinguishable from the user declining, and silent.
        assert_eq!(session_display_problem(Some(":0"), None), None);
        assert_eq!(session_display_problem(None, Some("wayland-0")), None);
        assert_eq!(session_display_problem(Some(""), Some("wayland-1")), None);

        let reason = session_display_problem(None, None).expect("no display must be a problem");
        assert!(reason.contains("DISPLAY"), "{reason}");
        // Empty is not set: an exported-but-empty DISPLAY reaches no server.
        assert!(session_display_problem(Some(""), Some("")).is_some());
        assert!(session_display_problem(Some(""), None).is_some());
    }

    #[test]
    fn a_display_problem_short_circuits_before_any_helper_is_spawned() {
        let mut spawned = Vec::new();
        let result = confirm_with(
            &prompt("t", "m", "c"),
            Some("no display here".to_string()),
            |program, _| {
                spawned.push(program.to_string());
                Ok(Some(0)) // would confirm — must never be reached
            },
        );

        assert!(
            spawned.is_empty(),
            "nothing may be spawned without a display: {spawned:?}"
        );
        let Err(PlatformError::CannotComplete { reason }) = result else {
            panic!("expected a reported failure, got {result:?}");
        };
        assert_eq!(reason, "no display here");
    }

    #[test]
    fn a_broken_helper_falls_through_but_never_confirms() {
        // A helper that ran but did not collect an answer (255 on a bad option)
        // must not be read as one. With a single-helper chain that means a
        // reported error — never `Ok(true)`, and never `Ok(false)` either, because
        // "the dialog failed" is not "the user declined".
        let mut tried = Vec::new();
        let result = confirm_with(&prompt("t", "m", "c"), None, |program, _| {
            tried.push(program.to_string());
            Ok(Some(255))
        });

        assert_eq!(tried, vec!["zenity"]);
        let Err(PlatformError::CannotComplete { reason }) = result else {
            panic!("expected fail-closed, got {result:?}");
        };
        assert!(
            reason.contains("255"),
            "the error should carry the exit code: {reason}"
        );
    }

    /// A fake helper binary on disk proves the argv we build survives a real
    /// `Command` spawn and that its exit code maps as expected — the success
    /// path, without a display. Unix-only because it writes a `#!/bin/sh` script;
    /// the pure argv/verdict tests above cover the Windows lane.
    #[cfg(unix)]
    #[test]
    fn a_fake_helper_records_our_argv_and_its_exit_code_decides() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "compme-linux-confirm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("argv");
        let fake = dir.join("zenity");

        for (exit, expected) in [(0, Some(true)), (1, Some(false)), (255, None)] {
            // The fake records each argument on its own line, so an argument
            // that got split or re-parsed is visible.
            std::fs::write(
                &fake,
                format!(
                    "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > \"$1\"\nexit {exit}\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

            let p = prompt("Delete memory?", "Erase 12 entries -- forever?", "Erase");
            let result = confirm_with(&p, None, |program, args| {
                if program != "zenity" {
                    // The 255 case falls through to kdialog; this host has none.
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "no kdialog in this test",
                    ));
                }
                // Absolute path, so the test needs no PATH mutation (this
                // crate's tests run in parallel).
                let mut argv = vec![log.to_string_lossy().to_string()];
                argv.extend(args.iter().cloned());
                spawn_and_wait(&fake.to_string_lossy(), &argv)
            });

            let recorded: Vec<String> = std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .skip(1) // the log path this test prepended
                .map(str::to_string)
                .collect();
            assert_eq!(
                recorded,
                ConfirmHelper::Zenity.args(&p),
                "the helper must receive exactly the argv we built"
            );
            assert!(
                recorded.contains(&"--text=Erase 12 entries -- forever?".to_string()),
                "a message containing `--` must stay inside its own argument: {recorded:?}"
            );
            match expected {
                Some(answer) => assert_eq!(result, Ok(answer), "exit {exit}"),
                None => assert!(result.is_err(), "exit {exit} must not be an answer"),
            }
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
