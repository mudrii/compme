use std::env;
use std::process;
use std::thread;
use std::time::Duration;

use platform::{OverlayPresenter, ScreenRect};
use platform_macos::{MacosOverlayDiagnostics, MacosOverlayPresenter};

fn main() {
    let duration = env::args()
        .nth(1)
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(4));
    let rect = ScreenRect {
        x: env_f64("COMPLETE_ME_OVERLAY_X").unwrap_or(520.0),
        y: env_f64("COMPLETE_ME_OVERLAY_Y").unwrap_or(240.0),
        w: env_f64("COMPLETE_ME_OVERLAY_W").unwrap_or(1.0),
        h: env_f64("COMPLETE_ME_OVERLAY_H").unwrap_or(18.0),
    };
    let text =
        env::var("COMPLETE_ME_OVERLAY_TEXT").unwrap_or_else(|_| "ghost completion text".into());
    let update_text = env::var("COMPLETE_ME_OVERLAY_UPDATE_TEXT")
        .unwrap_or_else(|_| "updated ghost completion text".into());
    let update_after = env::var("COMPLETE_ME_OVERLAY_UPDATE_AFTER_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(800));

    let mut presenter = match MacosOverlayPresenter::new() {
        Ok(presenter) => presenter,
        Err(err) => {
            eprintln!("failed to create overlay presenter: {err:?}");
            process::exit(2);
        }
    };

    if let Err(err) = presenter.show_ghost(rect, &text) {
        eprintln!("SHOW_ERROR {err:?}");
        process::exit(1);
    }
    println!("SHOW rect={rect:?} text={text:?} pid={}", process::id());
    let shown_diagnostics = presenter.diagnostics_for_acceptance();
    println!("SHOW_DIAG {shown_diagnostics:?}");
    if !shown_diagnostics_ok(shown_diagnostics) {
        eprintln!("SHOW_DIAG_ERROR {shown_diagnostics:?}");
        process::exit(1);
    }

    let update_after = update_after.min(duration);
    thread::sleep(update_after);
    if let Err(err) = presenter.update_ghost(&update_text) {
        eprintln!("UPDATE_ERROR {err:?}");
        process::exit(1);
    }
    println!("UPDATE text={update_text:?}");
    let update_diagnostics = presenter.diagnostics_for_acceptance();
    println!("UPDATE_DIAG {update_diagnostics:?}");
    if !shown_diagnostics_ok(update_diagnostics) {
        eprintln!("UPDATE_DIAG_ERROR {update_diagnostics:?}");
        process::exit(1);
    }

    thread::sleep(duration.saturating_sub(update_after));
    if let Err(err) = presenter.hide() {
        eprintln!("HIDE_ERROR {err:?}");
        process::exit(1);
    }
    println!("HIDE");
    let hidden_diagnostics = presenter.diagnostics_for_acceptance();
    println!("HIDE_DIAG {hidden_diagnostics:?}");
    if !hidden_diagnostics.has_panel || hidden_diagnostics.visible {
        eprintln!("HIDE_DIAG_ERROR {hidden_diagnostics:?}");
        process::exit(1);
    }
    println!(
        "SUMMARY shown=true updated=true hidden=true click_through={} nonactivating={} can_key={}",
        shown_diagnostics.ignores_mouse_events,
        shown_diagnostics.nonactivating_panel,
        shown_diagnostics.can_become_key_window
    );
}

fn env_f64(name: &str) -> Option<f64> {
    env::var(name).ok().and_then(|raw| raw.parse().ok())
}

fn shown_diagnostics_ok(diagnostics: MacosOverlayDiagnostics) -> bool {
    diagnostics.has_panel
        && diagnostics.visible
        && diagnostics.ignores_mouse_events
        && diagnostics.nonactivating_panel
        && !diagnostics.can_become_key_window
        && diagnostics.level == 101
}
