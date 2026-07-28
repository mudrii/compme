//! Pure geometry for the X11 ghost/correction overlay (ROADMAP Phase 2.5).
//!
//! Split out of the X11 code on purpose: this crate is compiled and tested on
//! the macOS and Windows CI lanes too, and headless pixel assertions cannot
//! catch a placement bug anyway. Everything that decides *where* a window goes —
//! the ghost box at the caret, the correction underline and banner around a word
//! rect, the on-screen clamp, and the pixel-coverage rectangles that give the
//! window its shape — lives here as a function over plain numbers, unit-tested
//! on every host.
//!
//! **There is no Y-flip.** AT-SPI reports `ATSPI_COORD_TYPE_SCREEN` extents with
//! a top-left origin and Y growing downwards, and X11 root coordinates are the
//! same, so the macOS `primary_height - y` flip (Cocoa is Y-up) has no analogue
//! here. Getting this wrong in the other direction — "every platform must need a
//! flip" — would put the ghost a screen-height away from the caret.

use platform::ScreenRect;

/// An X11 window box in root coordinates: signed position (the protocol's
/// `i16`-ranged x/y, widened to `i32` so arithmetic cannot wrap before the
/// clamp) and unsigned non-zero extent, because X11 refuses a zero-sized window
/// with `BadValue`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowBox {
    pub x: i32,
    pub y: i32,
    pub w: u16,
    pub h: u16,
}

/// The X screen's pixel size — `Screen::width_in_pixels`/`height_in_pixels`.
/// One root window spans every monitor on X11, which is what makes the clamp
/// below correct here and wrong on macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenSize {
    pub w: u16,
    pub h: u16,
}

/// How much wider than tall a rect may be and still be one character.
///
/// **Not the macOS threshold, deliberately.** The macOS presenter rejects
/// anything wider than 4pt because AX reports an actual caret *sliver*. AT-SPI's
/// `GetCharacterExtents` reports the character **cell** instead — the harness
/// measured `17,16,5,17` on the GTK fixture, so a 4px cap would classify every
/// real Linux caret as element bounds and pin the ghost to a default 18px box.
/// Aspect ratio is the signal that survives both: a character cell is never much
/// wider than its line is tall, while element bounds are a whole field wide.
const CARET_MAX_ASPECT: f64 = 3.0;
/// Generous: display-size fonts make genuinely tall caret lines, while element
/// bounds run to hundreds of pixels.
const CARET_MAX_H: f64 = 160.0;
/// Fallback box height when the rect is element bounds, not a caret.
const DEGENERATE_BOX_H: f64 = 18.0;
/// Padding between the box edge and the text, horizontally and vertically. 2px
/// each, matching the macOS label inset: a larger horizontal inset shows as a
/// visible gap between the typed word and the ghost.
const PAD: f64 = 2.0;
/// Widest overlay window. A ghost longer than this is clipped by the renderer
/// rather than growing a window across the screen.
const MAX_BOX_W: f64 = 720.0;
/// Correction underline thickness (the grammar-fix spec's "1-2px filled
/// underline").
pub const UNDERLINE_H: u16 = 2;
/// Gap between the word rect and the correction banner.
const BANNER_GAP: f64 = 4.0;
/// Banner width bounds. The floor keeps a one-word suggestion readable; the cap
/// stops a long suggestion from spanning the screen.
const BANNER_MIN_W: f64 = 24.0;
const BANNER_MAX_W: f64 = 480.0;

/// Ghost box height for an anchor, independent of the text. Split out because
/// the caller needs it *before* it can measure the text: the font size is
/// derived from the box height, and the width from the text measured at that
/// size.
pub fn ghost_box_height(anchor: ScreenRect) -> f64 {
    if is_element_bounds(anchor) {
        DEGENERATE_BOX_H
    } else {
        (finite_or(anchor.h, 0.0) + 2.0 * PAD).clamp(16.0, 48.0)
    }
}

/// The ghost window box: anchored at the caret's left edge, hugging the caret
/// line with `PAD` above and below.
///
/// `text_width_px` is the measured advance width of the ghost string at
/// [`font_px`] — passed in rather than estimated here so this stays pure while
/// the box still fits the glyphs the renderer will actually draw.
///
/// `x` is the anchor's left edge, matching the macOS presenter. Whether a caret
/// at end-of-text should instead start at `anchor.x + anchor.w` (the AT-SPI
/// read path falls back to the *last* character's box there) is a one-glyph
/// question only a live look can settle, exactly as the macOS placement was
/// calibrated — so this does not guess.
pub fn ghost_box(anchor: ScreenRect, text_width_px: f64, screen: ScreenSize) -> WindowBox {
    let h = ghost_box_height(anchor);
    // Element bounds: hug the inside top-left, where the field's text starts.
    // A real caret: lift by the padding so the glyphs sit on the typed line.
    let y = if is_element_bounds(anchor) {
        finite_or(anchor.y, 0.0)
    } else {
        finite_or(anchor.y, 0.0) - PAD
    };
    let w = (finite_or(text_width_px, 0.0) + 2.0 * PAD).clamp(1.0, MAX_BOX_W);
    clamp_onto_screen(finite_or(anchor.x, 0.0), y, w, h, screen)
}

/// Banner height for a word rect — the text-independent half, like
/// [`ghost_box_height`].
pub fn banner_box_height(word: ScreenRect) -> f64 {
    (finite_or(word.h, 0.0) + 8.0).clamp(20.0, 52.0)
}

/// The correction banner: a small filled box just **above** the word, showing
/// the suggestion.
///
/// A word on the first visible line has no room above it. The banner then flips
/// *below* the underline rather than being clamped onto the word — clamping
/// would cover the very text the underline is pointing at.
pub fn correction_banner_box(
    word: ScreenRect,
    text_width_px: f64,
    screen: ScreenSize,
) -> WindowBox {
    let h = banner_box_height(word);
    let w = (finite_or(text_width_px, 0.0) + 2.0 * PAD)
        .clamp(BANNER_MIN_W, BANNER_MAX_W)
        .max(finite_or(word.w, 0.0).min(BANNER_MAX_W));
    let above = finite_or(word.y, 0.0) - h - BANNER_GAP;
    let y = if above >= 0.0 {
        above
    } else {
        finite_or(word.y, 0.0) + finite_or(word.h, 0.0) + f64::from(UNDERLINE_H) + BANNER_GAP
    };
    clamp_onto_screen(finite_or(word.x, 0.0), y, w, h, screen)
}

/// The correction underline: a [`UNDERLINE_H`]-tall bar flush under the word
/// rect, as wide as the word.
pub fn correction_underline_box(word: ScreenRect, screen: ScreenSize) -> WindowBox {
    clamp_onto_screen(
        finite_or(word.x, 0.0),
        finite_or(word.y, 0.0) + finite_or(word.h, 0.0),
        finite_or(word.w, 0.0).max(8.0),
        f64::from(UNDERLINE_H),
        screen,
    )
}

/// Glyph size for a box height. The box hugs the caret line, so `height - 6`
/// tracks the field's visual text size instead of a fixed default — the same
/// rule as the macOS ghost label, clamped to a legible floor and a sane cap.
pub fn font_px(box_h: u16) -> f32 {
    (f64::from(box_h) - 6.0).clamp(9.0, 28.0) as f32
}

/// A rect too wide for its height, or taller than any text line, is the focused
/// element's bounds rather than a character at the caret.
fn is_element_bounds(rect: ScreenRect) -> bool {
    let w = finite_or(rect.w, 0.0);
    let h = finite_or(rect.h, 0.0);
    w > CARET_MAX_ASPECT * h.max(1.0) || h > CARET_MAX_H
}

/// Replace a non-finite coordinate with `fallback`. Rects come from another
/// process over D-Bus; a NaN would silently propagate through the clamp into a
/// `saturating as` cast and place the window at 0,0 with no diagnosis.
fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

/// Clamp a box into the root window.
///
/// Correct on X11 and wrong on macOS, which is why it lives here rather than
/// being ported: X11 lays *every* monitor out inside one root window whose
/// coordinate space is `0..width × 0..height`, so a clamp keeps the overlay on
/// a real display. In Cocoa's global space a display below the primary has
/// legitimately negative `y`, so the macOS presenter deliberately does not
/// clamp.
fn clamp_onto_screen(x: f64, y: f64, w: f64, h: f64, screen: ScreenSize) -> WindowBox {
    let sw = f64::from(screen.w).max(1.0);
    let sh = f64::from(screen.h).max(1.0);
    let w = w.clamp(1.0, sw);
    let h = h.clamp(1.0, sh);
    WindowBox {
        x: x.clamp(0.0, sw - w).round() as i32,
        y: y.clamp(0.0, sh - h).round() as i32,
        w: w.round().clamp(1.0, f64::from(u16::MAX)) as u16,
        h: h.round().clamp(1.0, f64::from(u16::MAX)) as u16,
    }
}

/// Row-run rectangles covering every pixel of `pixels` whose alpha is non-zero.
///
/// This is what makes the overlay usable **without a compositor**: the window's
/// SHAPE bounding region is set to these rectangles, so fully transparent
/// pixels are simply not part of the window and the application shows through.
/// With a compositor the same rectangles are a no-op (they cover every pixel the
/// renderer touched) and the alpha channel does the blending.
///
/// `pixels` is row-major, `w * h` long; a short slice yields only the rows it
/// actually holds rather than panicking, because a renderer bug must not take
/// the process down.
pub fn visible_rects(pixels: &[u32], w: u16, h: u16) -> Vec<(i16, i16, u16, u16)> {
    let width = usize::from(w);
    let mut rects = Vec::new();
    if width == 0 {
        return rects;
    }
    for row in 0..usize::from(h) {
        let Some(line) = pixels.get(row * width..(row + 1) * width) else {
            break;
        };
        let mut run_start: Option<usize> = None;
        for (col, pixel) in line.iter().enumerate() {
            match (pixel >> 24, run_start) {
                (0, Some(start)) => {
                    rects.push(row_rect(start, col, row));
                    run_start = None;
                }
                (0, None) => {}
                (_, None) => run_start = Some(col),
                (_, Some(_)) => {}
            }
        }
        if let Some(start) = run_start {
            rects.push(row_rect(start, width, row));
        }
    }
    rects
}

/// One horizontal run as an X11 rectangle. Coordinates are window-relative and
/// bounded by the window size, so the `as` casts cannot lose information for any
/// window this module produces (`w`/`h` are `u16`).
fn row_rect(start: usize, end: usize, row: usize) -> (i16, i16, u16, u16) {
    (
        start.min(i16::MAX as usize) as i16,
        row.min(i16::MAX as usize) as i16,
        (end - start).min(u16::MAX as usize) as u16,
        1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Xvfb screen the AT-SPI harness runs, so the numbers in these tests
    /// are the numbers the live tests see.
    const SCREEN: ScreenSize = ScreenSize { w: 1280, h: 1024 };

    /// The real per-character extents the harness measured on the GTK fixture
    /// (`17,16,5,17` — recorded in ROADMAP §1.1).
    fn fixture_caret() -> ScreenRect {
        ScreenRect {
            x: 17.0,
            y: 16.0,
            w: 5.0,
            h: 17.0,
        }
    }

    #[test]
    fn ghost_hugs_the_caret_line_with_no_y_flip() {
        // The property that would break silently: X11 root coordinates and
        // AT-SPI screen coordinates share a top-left origin, so the ghost's top
        // must be *at* the caret (minus the 2px pad), not a screen-height away
        // from it. A ported Cocoa flip would put y near 1024 - 16 = 1008.
        let ghost = ghost_box(fixture_caret(), 60.0, SCREEN);
        assert_eq!(ghost.x, 17, "the ghost starts at the caret's left edge");
        assert_eq!(ghost.y, 14, "caret top 16 lifted by the 2px pad");
        assert_eq!(ghost.h, 21, "caret height 17 plus 2px above and below");
        assert_eq!(ghost.w, 64, "measured 60px of text plus 2px each side");
    }

    #[test]
    fn ghost_width_follows_the_measured_text_and_is_capped() {
        let caret = fixture_caret();
        // Sized to content rather than to a fixed floor: an empty ghost is a
        // 1px-wide window, not a 240px transparent slab over the field.
        assert_eq!(ghost_box(caret, 0.0, SCREEN).w, 4);
        assert_eq!(ghost_box(caret, 100.0, SCREEN).w, 104);
        // Capped, so a runaway suggestion cannot open a screen-wide window; the
        // renderer clips the text to the box.
        assert_eq!(ghost_box(caret, 5_000.0, SCREEN).w, 720);
    }

    #[test]
    fn an_element_bounds_rect_is_not_treated_as_a_caret() {
        // The live Chrome failure: the app answers the caret query with the
        // element's bounds (1799x1225 there). Hugging that "line" would make a
        // 1225px-tall window; hug the inside top-left with a default line box
        // instead, which is guaranteed to be inside something visible.
        let bounds = ScreenRect {
            x: 40.0,
            y: 100.0,
            w: 1799.0,
            h: 1225.0,
        };
        let ghost = ghost_box(bounds, 60.0, SCREEN);
        assert_eq!(ghost.h, 18, "default line box, not the element height");
        assert_eq!(ghost.y, 100, "no pad lift: hug the element's top edge");
        assert_eq!(ghost_box_height(bounds), 18.0);

        // The AT-SPI-specific calibration: `GetCharacterExtents` returns the
        // character *cell*, not a caret sliver, so a 5px-wide rect at a 17px line
        // is a real caret. The macOS 4pt width cap would call this element bounds
        // and pin every Linux ghost to the default 18px box — which is exactly
        // what the harness's measured `17,16,5,17` caught.
        assert!(ghost_box_height(fixture_caret()) > DEGENERATE_BOX_H);
        // A tall-but-thin rect is still a caret (a heading line)...
        let heading = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 12.0,
            h: 80.0,
        };
        assert_eq!(ghost_box_height(heading), 48.0, "clamped, but line-derived");
        // ...and a single-line entry's bounds are caught by aspect ratio even
        // though they are only one line tall, which no absolute width cap that
        // still admits a wide glyph could do.
        let entry_bounds = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 20.0,
        };
        assert_eq!(ghost_box_height(entry_bounds), 18.0);
        // A wide glyph (a full-width CJK cell) is still one character.
        let wide_glyph = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 34.0,
            h: 17.0,
        };
        assert_eq!(ghost_box_height(wide_glyph), 21.0);
        // A zero-height rect cannot divide by zero into "always a caret".
        let flat = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 40.0,
            h: 0.0,
        };
        assert_eq!(ghost_box_height(flat), 18.0);
    }

    #[test]
    fn the_overlay_is_clamped_into_the_root_window() {
        // Unlike Cocoa, every X11 monitor lives inside one root window, so a box
        // pushed past the right or bottom edge belongs back on screen rather
        // than on a hypothetical display beyond it.
        let far_right = ScreenRect {
            x: 1270.0,
            y: 1020.0,
            w: 2.0,
            h: 16.0,
        };
        let ghost = ghost_box(far_right, 300.0, SCREEN);
        assert_eq!(ghost.x + i32::from(ghost.w), 1280);
        assert_eq!(ghost.y + i32::from(ghost.h), 1024);
        // Negative input (a rect from a monitor arrangement we mis-read) is
        // pulled to the origin instead of creating an invisible window.
        let negative = ScreenRect {
            x: -500.0,
            y: -500.0,
            w: 2.0,
            h: 16.0,
        };
        let ghost = ghost_box(negative, 60.0, SCREEN);
        assert_eq!((ghost.x, ghost.y), (0, 0));
        // A box wider than the screen is shrunk to the screen, never left
        // wider (X11 would accept it and the text would be unreachable).
        let tiny_screen = ScreenSize { w: 100, h: 40 };
        let ghost = ghost_box(fixture_caret(), 400.0, tiny_screen);
        assert_eq!((ghost.x, ghost.w), (0, 100));
        assert!(i32::from(ghost.h) <= 40);
    }

    #[test]
    fn a_non_finite_rect_yields_a_usable_box_instead_of_garbage() {
        // Rects arrive from another process over D-Bus. NaN survives `clamp`,
        // and `NaN as i32` is a silent 0 — so sanitize where it is diagnosable.
        let nan = ScreenRect {
            x: f64::NAN,
            y: f64::INFINITY,
            w: f64::NAN,
            h: f64::NEG_INFINITY,
        };
        let ghost = ghost_box(nan, f64::NAN, SCREEN);
        assert_eq!((ghost.x, ghost.y), (0, 0));
        assert!(ghost.w >= 1 && ghost.h >= 1, "never a zero-sized window");
        let underline = correction_underline_box(nan, SCREEN);
        assert!(underline.w >= 1 && underline.h >= 1);
        let banner = correction_banner_box(nan, f64::NAN, SCREEN);
        assert!(banner.w >= 1 && banner.h >= 1);
    }

    #[test]
    fn correction_underline_sits_directly_under_the_word() {
        let word = ScreenRect {
            x: 17.0,
            y: 200.0,
            w: 24.0,
            h: 17.0,
        };
        let underline = correction_underline_box(word, SCREEN);
        assert_eq!(underline.x, 17);
        assert_eq!(underline.y, 217, "flush under the word box's bottom edge");
        assert_eq!((underline.w, underline.h), (24, UNDERLINE_H));
        // A degenerate word rect still gets a visible underline rather than a
        // 0-width window (BadValue from X11).
        let sliver = ScreenRect { w: 0.0, ..word };
        assert_eq!(correction_underline_box(sliver, SCREEN).w, 8);
    }

    #[test]
    fn correction_banner_sits_above_the_word_and_flips_below_when_it_cannot() {
        let word = ScreenRect {
            x: 17.0,
            y: 200.0,
            w: 24.0,
            h: 17.0,
        };
        let banner = correction_banner_box(word, 40.0, SCREEN);
        assert_eq!(banner.h, 25, "word height 17 plus 8");
        assert_eq!(banner.y, 171, "200 - 25 - 4px gap");
        assert!(
            i32::from(banner.h) + banner.y <= 200,
            "the banner must not overlap the word it describes"
        );
        assert_eq!(banner.x, 17);

        // First-line word: there is no room above, so flip below the underline
        // rather than clamp onto the word and hide it.
        let first_line = ScreenRect { y: 4.0, ..word };
        let flipped = correction_banner_box(first_line, 40.0, SCREEN);
        assert_eq!(
            flipped.y,
            4 + 17 + i32::from(UNDERLINE_H) + 4,
            "below the underline"
        );
        assert!(flipped.y > 4 + 17, "clear of the word rect");
    }

    #[test]
    fn banner_width_covers_the_word_but_stays_bounded() {
        let word = ScreenRect {
            x: 0.0,
            y: 300.0,
            w: 200.0,
            h: 17.0,
        };
        // Never narrower than the word it labels...
        assert_eq!(correction_banner_box(word, 10.0, SCREEN).w, 200);
        // ...never narrower than the readable floor for a short suggestion...
        let narrow = ScreenRect { w: 4.0, ..word };
        assert_eq!(correction_banner_box(narrow, 1.0, SCREEN).w, 24);
        // ...and capped even when both the word and the text are huge.
        let huge = ScreenRect { w: 1200.0, ..word };
        assert_eq!(correction_banner_box(huge, 4_000.0, SCREEN).w, 480);
    }

    #[test]
    fn font_size_tracks_the_box_height() {
        // A 17px caret line → 21px box → 15px glyphs, i.e. the field's own text
        // size rather than a fixed default.
        assert_eq!(font_px(21), 15.0);
        // Floors and caps so a degenerate box stays legible and a clamped-48
        // box does not draw absurd glyphs.
        assert_eq!(font_px(1), 9.0);
        assert_eq!(font_px(48), 28.0);
    }

    #[test]
    fn visible_rects_cover_exactly_the_non_transparent_runs() {
        // 4x2, alpha in the top byte: row 0 has one run of 2 starting at x=1,
        // row 1 has two runs (x=0 and x=3).
        let pixels = [
            0x0000_0000,
            0xFF00_0000,
            0x8000_0000,
            0x0000_0000,
            0xFF11_2233,
            0x0000_0000,
            0x0000_0000,
            0x01FF_FFFF,
        ];
        assert_eq!(
            visible_rects(&pixels, 4, 2),
            vec![(1, 0, 2, 1), (0, 1, 1, 1), (3, 1, 1, 1)]
        );
        // A fully opaque buffer is one run per row — with a compositor this is
        // the no-op shape that lets the alpha channel do the blending.
        assert_eq!(
            visible_rects(&[0xFF00_0000; 6], 3, 2),
            vec![(0, 0, 3, 1), (0, 1, 3, 1)]
        );
        // Fully transparent: no rectangles, so nothing of the window is
        // clickable *or* visible — the honest result for an empty render.
        assert!(visible_rects(&[0; 6], 3, 2).is_empty());
        // A short buffer stops at the rows it holds instead of panicking: a
        // renderer bug must not take the process down.
        assert_eq!(visible_rects(&[0xFF00_0000; 3], 3, 9), vec![(0, 0, 3, 1)]);
        assert!(visible_rects(&[0xFF00_0000; 3], 0, 3).is_empty());
    }
}
