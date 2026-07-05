use std::env;
use std::process;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
}

#[derive(Debug, PartialEq, Eq)]
struct ModeDecision {
    revoked_summary: Option<bool>,
    usage_error: bool,
    exit_code: i32,
}

fn decide_mode(mode: &str, granted: bool) -> ModeDecision {
    match mode {
        "print" => ModeDecision {
            revoked_summary: None,
            usage_error: false,
            exit_code: 0,
        },
        "revoked" => {
            let revoked = !granted;
            ModeDecision {
                revoked_summary: Some(revoked),
                usage_error: false,
                exit_code: if revoked { 0 } else { 1 },
            }
        }
        _ => ModeDecision {
            revoked_summary: None,
            usage_error: true,
            exit_code: 2,
        },
    }
}

fn main() {
    let mode = env::args().nth(1).unwrap_or_else(|| "print".into());
    let granted = unsafe { CGPreflightListenEventAccess() };
    println!("INPUT_MONITORING granted={granted}");

    let decision = decide_mode(&mode, granted);
    if let Some(revoked) = decision.revoked_summary {
        println!("SUMMARY input_monitoring_revoked={revoked}");
    }
    if decision.usage_error {
        eprintln!("usage: input_monitoring_preflight_acceptance [print|revoked]");
    }
    if decision.exit_code != 0 {
        process::exit(decision.exit_code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_mode_reports_permission_without_summary_or_exit() {
        assert_eq!(
            decide_mode("print", true),
            ModeDecision {
                revoked_summary: None,
                usage_error: false,
                exit_code: 0,
            }
        );
    }

    #[test]
    fn revoked_mode_passes_when_preflight_is_not_granted() {
        assert_eq!(
            decide_mode("revoked", false),
            ModeDecision {
                revoked_summary: Some(true),
                usage_error: false,
                exit_code: 0,
            }
        );
    }

    #[test]
    fn revoked_mode_fails_when_preflight_is_granted() {
        assert_eq!(
            decide_mode("revoked", true),
            ModeDecision {
                revoked_summary: Some(false),
                usage_error: false,
                exit_code: 1,
            }
        );
    }

    #[test]
    fn invalid_mode_reports_usage_and_exit_2() {
        assert_eq!(
            decide_mode("other", false),
            ModeDecision {
                revoked_summary: None,
                usage_error: true,
                exit_code: 2,
            }
        );
    }
}
