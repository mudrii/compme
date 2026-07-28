//! Live confirmation-dialog test (ROADMAP Phase 2.6). Linux only, and
//! `#[ignore]`d: it needs a display and a real `zenity`.
//!
//! The fake-helper test in [`super`] proves the argv we build and how exit codes
//! map. What it cannot prove is that *real* zenity accepts those options — a
//! renamed or unsupported flag would make every confirm fail closed forever, and
//! zenity reports that the same way it reports a missing display: exit 255. So
//! this test runs the production argv with `--timeout=1` appended and requires
//! exit 5 (the timeout), which is only reachable if zenity parsed every option and
//! actually displayed the dialog.
//!
//! ```sh
//! nix-shell -p zenity --run 'tools/acceptance/run-linux-atspi-session.sh \
//!   --run-in-session cargo test -p platform_linux -- --ignored --test-threads=1 zenity'
//! ```

use super::*;

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn zenity_accepts_the_argv_we_build_and_times_out_as_a_decline() {
    let prompt = ConfirmPrompt {
        title: "compme live test",
        message: "Nobody will answer this -- it must time out.",
        confirm_label: "Allow",
    };
    let mut args = ConfirmHelper::Zenity.args(&prompt);
    args.push("--timeout=1".to_string());

    let code = spawn_and_wait(ConfirmHelper::Zenity.program(), &args)
        .expect("zenity must be on PATH for this test");

    assert_eq!(
        code,
        Some(5),
        "expected zenity's timeout exit (5). 255 means it rejected an option or could not open \
         the display, and 1 would mean something answered for the user"
    );
    assert_eq!(
        ConfirmHelper::Zenity.outcome(code),
        ConfirmOutcome::Declined,
        "an unanswered dialog must never be a confirm"
    );
}
