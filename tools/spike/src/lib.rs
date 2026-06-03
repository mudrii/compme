//! Spike logic, unit-tested behind seams. Real FFI lives in src/bin/*.

pub mod geometry {
    /// A rectangle in screen coordinates.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct ScreenRect { pub x: f64, pub y: f64, pub w: f64, pub h: f64 }

    /// True if a caret rect is plausibly a caret (not empty, not the whole container).
    pub fn usable_caret_rect(w: f64, h: f64) -> bool {
        h > 0.0 && h < 200.0 && w < 2000.0
    }

    /// Convert an AX rect (top-left origin, y grows downward) to a Cocoa window
    /// origin (bottom-left origin, y grows upward) given the primary screen height.
    pub fn ax_to_cocoa_origin(screen_h: f64, r: ScreenRect) -> (f64, f64) {
        (r.x, screen_h - r.y - r.h)
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::geometry::*;

    #[test]
    fn usable_rejects_zero_height() { assert!(!usable_caret_rect(10.0, 0.0)); }

    #[test]
    fn usable_rejects_container_width() { assert!(!usable_caret_rect(2500.0, 18.0)); }

    #[test]
    fn usable_rejects_tall_rect() { assert!(!usable_caret_rect(10.0, 250.0)); }

    #[test]
    fn usable_accepts_normal_caret() { assert!(usable_caret_rect(2.0, 18.0)); }

    #[test]
    fn ax_to_cocoa_flips_y_from_top_left() {
        let r = ScreenRect { x: 50.0, y: 100.0, w: 2.0, h: 20.0 };
        let (x, y) = ax_to_cocoa_origin(1000.0, r);
        assert_eq!(x, 50.0);
        assert_eq!(y, 880.0); // 1000 - 100 - 20
    }
}
