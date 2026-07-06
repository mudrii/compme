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
        x: env_f64("COMPME_OVERLAY_X").unwrap_or(520.0),
        y: env_f64("COMPME_OVERLAY_Y").unwrap_or(240.0),
        w: env_f64("COMPME_OVERLAY_W").unwrap_or(1.0),
        h: env_f64("COMPME_OVERLAY_H").unwrap_or(18.0),
    };
    let text = env::var("COMPME_OVERLAY_TEXT").unwrap_or_else(|_| "ghost completion text".into());
    let mode = env::var("COMPME_OVERLAY_MODE").unwrap_or_else(|_| "ghost".into());
    let update_text = env::var("COMPME_OVERLAY_UPDATE_TEXT")
        .unwrap_or_else(|_| "updated ghost completion text".into());
    let update_after = env::var("COMPME_OVERLAY_UPDATE_AFTER_MS")
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

    if mode == "correction" {
        if let Err(err) = presenter.show_correction(rect, &text) {
            eprintln!("CORRECTION_SHOW_ERROR {err:?}");
            process::exit(1);
        }
        println!(
            "CORRECTION_SHOW rect={rect:?} suggestion={text:?} pid={}",
            process::id()
        );
        let diagnostics = presenter.diagnostics_for_acceptance();
        println!("CORRECTION_SHOW_DIAG {diagnostics:?}");
        if !correction_diagnostics_ok(diagnostics, rect) {
            eprintln!("CORRECTION_SHOW_DIAG_ERROR {diagnostics:?}");
            process::exit(1);
        }

        thread::sleep(duration);
        if let Err(err) = presenter.hide() {
            eprintln!("HIDE_ERROR {err:?}");
            process::exit(1);
        }
        println!("HIDE");
        let hidden_diagnostics = presenter.diagnostics_for_acceptance();
        println!("HIDE_DIAG {hidden_diagnostics:?}");
        if !hidden_diagnostics.has_panel
            || hidden_diagnostics.visible
            || hidden_diagnostics.underline_visible
        {
            eprintln!("HIDE_DIAG_ERROR {hidden_diagnostics:?}");
            process::exit(1);
        }
        println!(
            "SUMMARY correction_shown=true hidden=true underline_visible_before=true click_through={} nonactivating={} can_key={}",
            diagnostics.ignores_mouse_events,
            diagnostics.nonactivating_panel,
            diagnostics.can_become_key_window
        );
        return;
    }

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
        && diagnostics.joins_all_spaces
        && diagnostics.fullscreen_auxiliary
        && diagnostics.level == 101
}

fn correction_diagnostics_ok(diagnostics: MacosOverlayDiagnostics, word_rect: ScreenRect) -> bool {
    let Some(panel) = diagnostics.panel_frame else {
        return false;
    };
    let Some(underline) = diagnostics.underline_frame else {
        return false;
    };
    shown_diagnostics_ok(diagnostics)
        && diagnostics.has_underline_panel
        && diagnostics.underline_visible
        && approx_eq(panel.x, word_rect.x)
        && approx_eq(underline.x, word_rect.x)
        && approx_eq(underline.w, word_rect.w.max(8.0))
        && approx_eq(underline.h, 2.0)
        && approx_eq(panel.y - underline.y, word_rect.h + 6.0)
        && panel.w >= word_rect.w
        && (20.0..=52.0).contains(&panel.h)
}

fn approx_eq(left: f64, right: f64) -> bool {
    (left - right).abs() <= 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word_rect() -> ScreenRect {
        ScreenRect {
            x: 1.0,
            y: 240.0,
            w: 12.0,
            h: 18.0,
        }
    }

    fn valid_shown_diagnostics() -> MacosOverlayDiagnostics {
        MacosOverlayDiagnostics {
            has_panel: true,
            visible: true,
            ignores_mouse_events: true,
            nonactivating_panel: true,
            can_become_key_window: false,
            level: 101,
            joins_all_spaces: true,
            fullscreen_auxiliary: true,
            panel_frame: Some(ScreenRect {
                x: 1.0,
                y: 26.0,
                w: 96.0,
                h: 26.0,
            }),
            has_underline_panel: true,
            underline_visible: true,
            underline_frame: Some(ScreenRect {
                x: 1.0,
                y: 2.0,
                w: 12.0,
                h: 2.0,
            }),
        }
    }

    #[test]
    fn shown_diagnostics_require_space_collection_behavior() {
        assert!(shown_diagnostics_ok(valid_shown_diagnostics()));

        let mut missing_spaces = valid_shown_diagnostics();
        missing_spaces.joins_all_spaces = false;
        assert!(!shown_diagnostics_ok(missing_spaces));

        let mut missing_fullscreen = valid_shown_diagnostics();
        missing_fullscreen.fullscreen_auxiliary = false;
        assert!(!shown_diagnostics_ok(missing_fullscreen));
    }

    #[test]
    fn correction_diagnostics_require_space_collection_behavior() {
        assert!(correction_diagnostics_ok(
            valid_shown_diagnostics(),
            word_rect()
        ));

        let mut missing_spaces = valid_shown_diagnostics();
        missing_spaces.joins_all_spaces = false;
        assert!(!correction_diagnostics_ok(missing_spaces, word_rect()));

        let mut missing_fullscreen = valid_shown_diagnostics();
        missing_fullscreen.fullscreen_auxiliary = false;
        assert!(!correction_diagnostics_ok(missing_fullscreen, word_rect()));
    }

    #[test]
    fn correction_diagnostics_rejects_misplaced_banner_or_underline() {
        assert!(correction_diagnostics_ok(
            valid_shown_diagnostics(),
            word_rect()
        ));

        let mut moved_banner = valid_shown_diagnostics();
        moved_banner.panel_frame.as_mut().unwrap().x += 10.0;
        assert!(!correction_diagnostics_ok(moved_banner, word_rect()));

        let mut floating_underline = valid_shown_diagnostics();
        floating_underline.underline_frame.as_mut().unwrap().y += 8.0;
        assert!(!correction_diagnostics_ok(floating_underline, word_rect()));

        let mut narrow_underline = valid_shown_diagnostics();
        narrow_underline.underline_frame.as_mut().unwrap().w = 4.0;
        assert!(!correction_diagnostics_ok(narrow_underline, word_rect()));
    }
}
