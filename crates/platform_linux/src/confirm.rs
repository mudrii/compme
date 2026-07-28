//! Blocking modal confirmation for Linux (ROADMAP Phase 2.6): `zenity`, falling
//! back to `kdialog`.
//!
//! There is no toolkit-neutral system dialog on Linux, and linking GTK would make
//! the binary refuse to *start* where GTK is absent — the same reason the AT-SPI
//! path speaks D-Bus instead of libatspi (see `crate::atspi_live`). A helper
//! process is spawned, so a host with neither helper degrades to a reported
//! failure rather than a broken executable.
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
//! - **Neither helper present is an error**, never a silent confirm.
//!
//! `ExitStatus` cannot be constructed portably, so the seam these functions take
//! yields `Option<i32>` (`ExitStatus::code()`): the argv construction, the
//! helper-chain fallthrough, and the code→verdict mapping are then testable on
//! every host, including with a fake helper binary. Only `Command::status()`
//! itself is untested glue.

use platform::PlatformError;
use shell_flags::ConfirmPrompt;

/// The helpers, in preference order.
pub const HELPER_CHAIN: [ConfirmHelper; 2] = [ConfirmHelper::Zenity, ConfirmHelper::Kdialog];

/// Label of the declining button. Fixed rather than taken from `ConfirmPrompt`,
/// which carries only the confirming label — matching the macOS alert, whose
/// first (default) button is always "Cancel".
pub const DECLINE_LABEL: &str = "Cancel";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmHelper {
    Zenity,
    Kdialog,
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
            Self::Kdialog => "kdialog",
        }
    }

    /// The exact `argv` (after the program name) for `prompt`.
    ///
    /// zenity: `--question` with `--default-cancel`, which gives the Cancel
    /// button focus so Return declines, and `--no-markup`, so a message
    /// containing `<` or `&` is shown literally instead of being parsed as Pango
    /// markup (or failing to render at all).
    ///
    /// kdialog: `--warningyesno`, whose KMessageBox warning styling makes the
    /// declining button the default one; `--yes-label`/`--no-label` relabel the
    /// buttons. kdialog has no explicit default-button switch, which is why it is
    /// the fallback rather than the first choice.
    pub fn args(self, prompt: &ConfirmPrompt<'_>) -> Vec<String> {
        // `--option=value` (glued) form throughout: a title, message, or label
        // that begins with `--` stays a value instead of being parsed as an
        // option. kdialog's positional text is the one exception, so it is
        // guarded by the `--` end-of-options separator.
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
            Self::Kdialog => vec![
                format!("--title={}", prompt.title),
                format!("--yes-label={}", prompt.confirm_label),
                format!("--no-label={DECLINE_LABEL}"),
                "--warningyesno".to_string(),
                "--".to_string(),
                prompt.message.to_string(),
            ],
        }
    }

    /// Map the helper's exit code to a verdict. `None` is a signal death.
    ///
    /// zenity: 0 confirm, 1 cancel/close/Escape, 5 `--timeout` expiry, anything
    /// else (255 on a GTK/display failure) is a failure.
    /// kdialog: 0 yes, 1 no, 2 usage/error, and Qt exits non-zero when it cannot
    /// open a display.
    pub fn outcome(self, code: Option<i32>) -> ConfirmOutcome {
        match (self, code) {
            (_, Some(0)) => ConfirmOutcome::Confirmed,
            (Self::Zenity, Some(1 | 5)) | (Self::Kdialog, Some(1)) => ConfirmOutcome::Declined,
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

/// Walk the helper chain with `run`, returning the first real answer.
///
/// A helper that cannot be spawned (not installed) or that fails to collect an
/// answer is skipped and the next one tried; when none answers, the error names
/// every helper and why it failed, so an operator does not have to guess which
/// package is missing. Only an explicit confirm returns `Ok(true)`.
pub fn confirm_with(
    prompt: &ConfirmPrompt<'_>,
    mut run: impl FnMut(&str, &[String]) -> std::io::Result<Option<i32>>,
) -> Result<bool, PlatformError> {
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
            "no usable confirmation dialog (install zenity or kdialog): {}",
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
    fn kdialog_argv_uses_the_warning_variant_and_ends_options_before_the_text() {
        let args = ConfirmHelper::Kdialog.args(&prompt("T", "M", "Delete"));
        assert_eq!(
            args,
            vec![
                "--title=T",
                "--yes-label=Delete",
                "--no-label=Cancel",
                "--warningyesno",
                "--",
                "M",
            ]
        );
    }

    #[test]
    fn hostile_prompt_text_stays_one_argv_element_per_field() {
        // A message that is shell metacharacters, starts with `--`, and contains
        // newlines and quotes: there is no shell, and the glued `--option=value`
        // form means none of it can be read as an option. The `--` separator
        // covers kdialog's positional text.
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
    fn the_chain_tries_kdialog_when_zenity_is_absent() {
        let mut tried = Vec::new();
        let confirmed = confirm_with(&prompt("t", "m", "c"), |program, args| {
            tried.push(program.to_string());
            match program {
                "zenity" => Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "No such file or directory",
                )),
                _ => {
                    assert!(args.contains(&"--warningyesno".to_string()));
                    Ok(Some(0))
                }
            }
        })
        .unwrap();

        assert!(confirmed);
        assert_eq!(tried, vec!["zenity", "kdialog"]);
    }

    #[test]
    fn a_declining_helper_short_circuits_the_chain() {
        // A real answer must not be second-guessed by another dialog.
        let mut tried = Vec::new();
        let confirmed = confirm_with(&prompt("t", "m", "c"), |program, _| {
            tried.push(program.to_string());
            Ok(Some(1))
        })
        .unwrap();

        assert!(!confirmed);
        assert_eq!(tried, vec!["zenity"], "the first answer wins");
    }

    #[test]
    fn no_helper_at_all_fails_closed_and_names_both() {
        let result = confirm_with(&prompt("t", "m", "c"), |program, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{program} missing"),
            ))
        });

        let Err(PlatformError::CannotComplete { reason }) = result else {
            panic!("a host with no dialog helper must fail closed, got {result:?}");
        };
        assert!(
            reason.contains("zenity") && reason.contains("kdialog"),
            "{reason}"
        );
        assert!(
            reason.contains("install"),
            "the error should say what to install: {reason}"
        );
    }

    #[test]
    fn a_broken_helper_falls_through_but_never_confirms() {
        // zenity present but unable to open a display (255) must not be read as
        // an answer; kdialog is tried, and if it also fails the result is an
        // error — never `Ok(true)`.
        let mut tried = Vec::new();
        let result = confirm_with(&prompt("t", "m", "c"), |program, _| {
            tried.push(program.to_string());
            Ok(Some(255))
        });

        assert_eq!(tried, vec!["zenity", "kdialog"]);
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
            let result = confirm_with(&p, |program, args| {
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
