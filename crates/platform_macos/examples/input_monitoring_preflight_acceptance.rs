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

#[derive(Debug, PartialEq, Eq)]
struct CliOutput {
    stdout: String,
    stderr: String,
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

fn render_output(granted: bool, decision: &ModeDecision) -> CliOutput {
    let mut stdout = format!("INPUT_MONITORING granted={granted}\n");
    if let Some(revoked) = decision.revoked_summary {
        stdout.push_str(&format!("SUMMARY input_monitoring_revoked={revoked}\n"));
    }
    let stderr = if decision.usage_error {
        "usage: input_monitoring_preflight_acceptance [print|revoked]\n".to_string()
    } else {
        String::new()
    };
    CliOutput {
        stdout,
        stderr,
        exit_code: decision.exit_code,
    }
}

fn main() {
    let mode = env::args().nth(1).unwrap_or_else(|| "print".into());
    let granted = unsafe { CGPreflightListenEventAccess() };
    let decision = decide_mode(&mode, granted);

    let output = render_output(granted, &decision);
    print!("{}", output.stdout);
    eprint!("{}", output.stderr);
    if output.exit_code != 0 {
        process::exit(output.exit_code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_mode_reports_permission_without_summary_or_exit() {
        let decision = decide_mode("print", true);
        assert_eq!(
            render_output(true, &decision),
            CliOutput {
                stdout: "INPUT_MONITORING granted=true\n".to_string(),
                stderr: String::new(),
                exit_code: 0,
            }
        );
    }

    #[test]
    fn revoked_mode_passes_when_preflight_is_not_granted() {
        let decision = decide_mode("revoked", false);
        assert_eq!(
            render_output(false, &decision),
            CliOutput {
                stdout: "INPUT_MONITORING granted=false\nSUMMARY input_monitoring_revoked=true\n"
                    .to_string(),
                stderr: String::new(),
                exit_code: 0,
            }
        );
    }

    #[test]
    fn revoked_mode_fails_when_preflight_is_granted() {
        let decision = decide_mode("revoked", true);
        assert_eq!(
            render_output(true, &decision),
            CliOutput {
                stdout: "INPUT_MONITORING granted=true\nSUMMARY input_monitoring_revoked=false\n"
                    .to_string(),
                stderr: String::new(),
                exit_code: 1,
            }
        );
    }

    #[test]
    fn invalid_mode_reports_usage_and_exit_2() {
        let decision = decide_mode("other", false);
        assert_eq!(
            render_output(false, &decision),
            CliOutput {
                stdout: "INPUT_MONITORING granted=false\n".to_string(),
                stderr: "usage: input_monitoring_preflight_acceptance [print|revoked]\n"
                    .to_string(),
                exit_code: 2,
            }
        );
    }
}
