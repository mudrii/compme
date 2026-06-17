use std::env;
use std::process;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
}

fn main() {
    let mode = env::args().nth(1).unwrap_or_else(|| "print".into());
    let granted = unsafe { CGPreflightListenEventAccess() };
    println!("INPUT_MONITORING granted={granted}");

    match mode.as_str() {
        "print" => {}
        "revoked" => {
            let revoked = !granted;
            println!("SUMMARY input_monitoring_revoked={revoked}");
            if !revoked {
                process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: input_monitoring_preflight_acceptance [print|revoked]");
            process::exit(2);
        }
    }
}
