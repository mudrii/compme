//! Override-redirect X11 ghost/correction overlay (ROADMAP Phase 2.5). Linux only.
//!
//! **Why `x11rb` and not Xlib/Xft/cairo.** Same reason `atspi_live` speaks D-Bus
//! instead of linking libatspi: a C library in the link line makes the compme
//! binary refuse to *start* on a host that lacks it, a hard failure where this
//! project requires fail-closed degradation. `x11rb` speaks the X11 wire protocol
//! in pure Rust (its only dependencies are `x11rb-protocol`, `rustix` and
//! `gethostname`), and `allow-unsafe-code` — the feature that would pull in
//! libxcb — is deliberately left off.
//!
//! **Why `fontdue` alone and not `tiny-skia` + `fontdue`.** The plan's §2.5 said
//! "smallest dep wins", and the overlay needs exactly two drawing primitives: a
//! filled rectangle and a glyph coverage blit. Both are a few lines over a
//! `Vec<u32>`, so a 2D canvas library would be paid for and unused. `fontdue`
//! brings `ttf-parser` + `hashbrown`; `tiny-skia` would add five more crates for
//! nothing.
//!
//! **How it degrades.** An ARGB (depth-32 TrueColor) visual is used when the
//! server offers one, so a compositing desktop blends the ghost properly. When it
//! does not — or when nothing is compositing, which is the case under the Xvfb
//! test harness — the window's SHAPE *bounding* region is set to the pixels the
//! renderer actually touched, so fully transparent areas are not part of the
//! window at all and the application below shows through regardless. The same
//! rectangles are computed either way, so one code path serves both.
//!
//! **What it never does.** It never takes focus (override-redirect windows are
//! unmanaged, and this code never calls `SetInputFocus`), never selects key or
//! button events, and sets an *empty* SHAPE input region so every click reaches
//! the application underneath.

use crate::overlay_font;
use crate::overlay_geometry::{
    banner_box_height, correction_banner_box, correction_underline_box, font_px, ghost_box,
    ghost_box_height, visible_rects, ScreenSize, WindowBox,
};
use fontdue::{Font, FontSettings};
use platform::{PlatformError, ScreenRect};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClipOrdering, ColormapAlloc, ConfigureWindowAux,
    ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, Gcontext, ImageFormat, PropMode,
    Rectangle, StackMode, VisualClass, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// `COMPME_FONT`: an explicit `.ttf`/`.otf` path that overrides discovery. The
/// escape hatch for a host whose fonts are somewhere the scan does not look
/// (a Nix build shell, a container with a single mounted font).
const FONT_ENV: &str = "COMPME_FONT";

/// Ghost glyph colour: mid grey at 75% alpha — visibly "not yet typed" against
/// both light and dark fields, matching the macOS ghost label's intent.
const GHOST_TEXT: u32 = premultiplied(191, 0x99, 0x99, 0x99);
/// Correction banner: a near-opaque dark plate with near-white text, so the
/// suggestion reads as a UI affordance rather than as field content.
const BANNER_BG: u32 = premultiplied(235, 0x20, 0x20, 0x20);
const BANNER_TEXT: u32 = premultiplied(255, 0xF0, 0xF0, 0xF0);
/// Correction underline: the macOS presenter's `colorWithWhite:0.45 alpha:0.9`.
const UNDERLINE: u32 = premultiplied(230, 0x73, 0x73, 0x73);

/// Pack a straight-alpha colour into the premultiplied ARGB the X server wants
/// for a 32-bit visual. `const` so the palette above is compile-time data.
const fn premultiplied(a: u8, r: u8, g: u8, b: u8) -> u32 {
    ((a as u32) << 24) | (scale(r, a) << 16) | (scale(g, a) << 8) | scale(b, a)
}

/// One channel scaled by alpha. A free function rather than a closure: closures
/// cannot be called from a `const fn`.
const fn scale(channel: u8, alpha: u8) -> u32 {
    (channel as u32 * alpha as u32) / 255
}

fn cannot_complete(what: &str, err: impl std::fmt::Display) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("platform_linux x11 overlay {what}: {err}"),
    }
}

/// Send a void request and **wait for the server's verdict**.
///
/// X11 reports errors for requests with no reply asynchronously, so the naive
/// version of this module returned `Ok(())` from `show_ghost` while the server had
/// rejected `CreateWindow` and nothing was on screen — a fail-*open* show, which
/// the [`platform::OverlayPresenter`] contract explicitly forbids ("the engine
/// assumes an emitted ghost is on screen"). It was caught by a live test that
/// could not find the window the presenter said it had created.
///
/// The cost is one round trip per request on a local socket, tens of microseconds
/// against a 300 ms suggestion budget — a price worth paying to never claim a
/// ghost that is not there.
fn checked<C: RequestConnection>(
    what: &str,
    request: Result<x11rb::cookie::VoidCookie<'_, C>, x11rb::errors::ConnectionError>,
) -> Result<(), PlatformError> {
    request
        .map_err(|err| cannot_complete(what, err))?
        .check()
        .map_err(|err| cannot_complete(what, err))
}

/// One override-redirect window plus the resources that belong to it. Two of
/// these exist at most: the text window (ghost, reused as the correction banner)
/// and the underline bar — the same two-panel shape as the macOS presenter.
struct OverlayWindow {
    id: Window,
    gc: Gcontext,
    /// The current background pixmap. Kept alive rather than freed right after
    /// `background_pixmap`: the protocol lets a client free it immediately, but
    /// holding it makes the ownership obvious and costs one resource id.
    pixmap: Option<u32>,
    box_: WindowBox,
    mapped: bool,
}

/// The live overlay: one X11 connection, one font, and the two windows.
///
/// Not `Send`/`Sync` by construction (the connection is owned here and driven
/// from one thread), which is exactly what [`platform::OverlayPresenter`]
/// requires.
pub struct X11Overlay {
    conn: RustConnection,
    root: Window,
    screen: ScreenSize,
    depth: u8,
    visual: u32,
    /// Whether the chosen visual carries an alpha channel. `false` means a
    /// compositor cannot blend us, and the SHAPE bounding region is doing the
    /// work instead.
    argb: bool,
    /// Only set for the ARGB visual: a window whose visual differs from the
    /// root's needs its own colormap or `CreateWindow` fails with `BadMatch`.
    colormap: Option<u32>,
    /// Byte order the server wants inside each 4-byte pixel.
    lsb_first: bool,
    /// Largest `PutImage` payload this connection accepts, so a wide window is
    /// uploaded in row bands instead of one over-long request.
    max_request_bytes: usize,
    window_type: Option<(u32, u32)>,
    font: Font,
    font_path: std::path::PathBuf,
    text: Option<OverlayWindow>,
    underline: Option<OverlayWindow>,
    /// The anchor of the ghost currently on screen, so `update_ghost` can
    /// re-render without re-anchoring. `None` means nothing is showing.
    last_anchor: Option<ScreenRect>,
}

impl std::fmt::Debug for X11Overlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Overlay")
            .field("screen", &self.screen)
            .field("depth", &self.depth)
            .field("argb", &self.argb)
            .field("font", &self.font_path)
            .field("text_window", &self.text.as_ref().map(|w| w.id))
            .field("underline_window", &self.underline.as_ref().map(|w| w.id))
            .finish()
    }
}

impl X11Overlay {
    /// Connect, choose a visual, prove SHAPE is available, and load a font.
    ///
    /// Every failure is one a legitimate host can have — no `DISPLAY`, a server
    /// without the SHAPE extension, a machine with no fonts installed — so each
    /// is reported with what was tried rather than retried or panicked on. The
    /// host reconciles a failed show (hide + retract the shown stat), so a clear
    /// error here is the contract, not a defect.
    pub fn open() -> Result<Self, PlatformError> {
        let (conn, screen_num) = x11rb::connect(None).map_err(|err| {
            cannot_complete(
                "connect",
                format!(
                    "{err} (DISPLAY={})",
                    std::env::var("DISPLAY").unwrap_or_else(|_| "<unset>".into())
                ),
            )
        })?;
        let setup = conn.setup();
        let screen = setup
            .roots
            .get(screen_num)
            .ok_or_else(|| cannot_complete("screen", format!("no screen {screen_num}")))?;
        let root = screen.root;
        let screen_size = ScreenSize {
            w: screen.width_in_pixels,
            h: screen.height_in_pixels,
        };
        let lsb_first = setup.image_byte_order == x11rb::protocol::xproto::ImageOrder::LSB_FIRST;
        let (depth, visual, argb) = pick_visual(screen);
        // `PutImage` payloads are serialized as one `u32` per pixel, so a server
        // whose ZPixmap format for this depth is not 32 bits per pixel would be
        // handed a buffer it reads as garbage. Refuse up front with a name for
        // the mismatch instead of drawing noise on a 16-bit screen.
        let bits_per_pixel = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == depth)
            .map(|format| format.bits_per_pixel)
            .ok_or_else(|| cannot_complete("pixmap format", format!("none for depth {depth}")))?;
        if bits_per_pixel != 32 {
            return Err(cannot_complete(
                "pixmap format",
                format!("depth {depth} is {bits_per_pixel} bits per pixel, not 32"),
            ));
        }

        // SHAPE carries the empty input region that makes the overlay
        // click-through. Without it every click over the ghost would be
        // swallowed instead of reaching the app, which is worse than showing no
        // ghost at all — so this fails closed rather than degrading.
        conn.extension_information(shape::X11_EXTENSION_NAME)
            .map_err(|err| cannot_complete("SHAPE query", err))?
            .ok_or_else(|| {
                cannot_complete(
                    "SHAPE",
                    "extension absent: an overlay without an empty input region \
                     would swallow clicks meant for the application",
                )
            })?;

        let max_request_bytes = conn.maximum_request_bytes();
        let colormap = if argb {
            let cm = conn
                .generate_id()
                .map_err(|err| cannot_complete("colormap id", err))?;
            checked(
                "create_colormap",
                conn.create_colormap(ColormapAlloc::NONE, cm, root, visual),
            )?;
            Some(cm)
        } else {
            None
        };
        let window_type = intern_window_type(&conn);
        let (font_path, font) = load_font()?;

        Ok(Self {
            conn,
            root,
            screen: screen_size,
            depth,
            visual,
            argb,
            colormap,
            lsb_first,
            max_request_bytes,
            window_type,
            font,
            font_path,
            text: None,
            underline: None,
            last_anchor: None,
        })
    }

    /// The text window's id, once one exists. The single test seam: it is what
    /// lets the live tests prove `update_ghost` re-renders the *same* window
    /// instead of leaking a new one per keystroke, and query the server directly
    /// for override-redirect, map state, geometry, focus and input shape.
    pub fn text_window_id(&self) -> Option<u32> {
        self.text.as_ref().map(|w| w.id)
    }

    /// The underline window's id, once a correction has been shown.
    pub fn underline_window_id(&self) -> Option<u32> {
        self.underline.as_ref().map(|w| w.id)
    }

    /// The font the overlay is drawing with, for diagnostics.
    pub fn font_path(&self) -> &std::path::Path {
        &self.font_path
    }

    pub fn show_ghost(&mut self, anchor: ScreenRect, text: &str) -> Result<(), PlatformError> {
        let px = font_px(round_u16(ghost_box_height(anchor)));
        let width = self.measure(text, px);
        let box_ = ghost_box(anchor, width, self.screen);
        let canvas = self.render_ghost(box_, text, px);
        // Only record the anchor once the window is up: a stale `Some(anchor)`
        // would let `update_ghost` claim a ghost is showing when the show failed.
        self.present(WindowSlot::Text, box_, &canvas)?;
        self.last_anchor = Some(anchor);
        self.withdraw(WindowSlot::Underline)?;
        self.flush()
    }

    pub fn show_correction(
        &mut self,
        word: ScreenRect,
        suggestion: &str,
    ) -> Result<(), PlatformError> {
        let px = font_px(round_u16(banner_box_height(word)));
        let width = self.measure(suggestion, px);
        let banner = correction_banner_box(word, width, self.screen);
        let underline = correction_underline_box(word, self.screen);
        let banner_canvas = self.render_banner(banner, suggestion, px);
        let underline_canvas = Canvas::filled(underline.w, underline.h, UNDERLINE);
        self.present(WindowSlot::Text, banner, &banner_canvas)?;
        self.last_anchor = Some(word);
        self.present(WindowSlot::Underline, underline, &underline_canvas)?;
        self.flush()
    }

    pub fn update_ghost(&mut self, text: &str) -> Result<(), PlatformError> {
        // All-or-nothing: bind the anchor *and* the existing window before
        // touching either, so a half-shown overlay cannot end up resized while
        // still displaying the previous text.
        let (Some(anchor), Some(_)) = (self.last_anchor, self.text.as_ref()) else {
            return Err(PlatformError::CannotComplete {
                reason: "platform_linux x11 overlay: cannot update a hidden ghost".into(),
            });
        };
        self.show_ghost(anchor, text)
    }

    /// Unmap both windows. Idempotent by construction: an absent window and an
    /// already-unmapped one are both nothing to do.
    pub fn hide(&mut self) -> Result<(), PlatformError> {
        self.withdraw(WindowSlot::Text)?;
        self.withdraw(WindowSlot::Underline)?;
        self.last_anchor = None;
        self.flush()
    }

    fn measure(&self, text: &str, px: f32) -> f64 {
        text.chars()
            .map(|ch| f64::from(self.font.metrics(ch, px).advance_width))
            .sum()
    }

    fn render_ghost(&self, box_: WindowBox, text: &str, px: f32) -> Canvas {
        // Fully transparent behind the glyphs: the ghost sits *over* the user's
        // field, so a filled plate would hide the text it is extending.
        let mut canvas = Canvas::filled(box_.w, box_.h, 0);
        self.draw_text(&mut canvas, text, px, GHOST_TEXT);
        canvas
    }

    fn render_banner(&self, box_: WindowBox, text: &str, px: f32) -> Canvas {
        let mut canvas = Canvas::filled(box_.w, box_.h, BANNER_BG);
        self.draw_text(&mut canvas, text, px, BANNER_TEXT);
        canvas
    }

    /// Rasterize `text` into `canvas`, left-aligned with the module's padding and
    /// sitting on the font's own baseline. Glyphs past the right edge are
    /// dropped rather than wrapped: the box was sized from the same measurement,
    /// so this only triggers for a string clamped by `MAX_BOX_W`.
    fn draw_text(&self, canvas: &mut Canvas, text: &str, px: f32, colour: u32) {
        let ascent = self
            .font
            .horizontal_line_metrics(px)
            .map_or(f64::from(px), |m| f64::from(m.ascent));
        // Centre the line box vertically, then sit the glyphs on its baseline.
        let leading = (f64::from(canvas.h) - f64::from(px)).max(0.0) / 2.0;
        let baseline = (leading + ascent).round() as i32;
        let mut pen = 2.0_f64;
        for ch in text.chars() {
            let (metrics, coverage) = self.font.rasterize(ch, px);
            let left = (pen + f64::from(metrics.xmin)).round() as i32;
            let top = baseline - (metrics.height as i32 + metrics.ymin);
            for (row, line) in coverage.chunks(metrics.width.max(1)).enumerate() {
                for (col, alpha) in line.iter().enumerate() {
                    canvas.blend(left + col as i32, top + row as i32, *alpha, colour);
                }
            }
            pen += f64::from(metrics.advance_width);
            if pen > f64::from(canvas.w) {
                break;
            }
        }
    }

    /// Create-or-reuse the slot's window, move/resize it to `box_`, upload
    /// `canvas`, and map it topmost.
    fn present(
        &mut self,
        slot: WindowSlot,
        box_: WindowBox,
        canvas: &Canvas,
    ) -> Result<(), PlatformError> {
        let existing = self.slot(slot).take();
        let mut window = match existing {
            Some(window) => window,
            None => self.create_window(box_)?,
        };
        if window.box_ != box_ {
            checked(
                "configure_window",
                self.conn.configure_window(
                    window.id,
                    &ConfigureWindowAux::new()
                        .x(box_.x)
                        .y(box_.y)
                        .width(u32::from(box_.w))
                        .height(u32::from(box_.h)),
                ),
            )?;
            window.box_ = box_;
        }
        self.upload(&mut window, canvas)?;
        // Restack on every show: override-redirect windows are unmanaged, so
        // nothing else keeps this above a window that was raised meanwhile.
        checked(
            "restack",
            self.conn.configure_window(
                window.id,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            ),
        )?;
        if !window.mapped {
            checked("map_window", self.conn.map_window(window.id))?;
            window.mapped = true;
        }
        *self.slot(slot) = Some(window);
        Ok(())
    }

    fn withdraw(&mut self, slot: WindowSlot) -> Result<(), PlatformError> {
        let Some(window) = self.slot(slot).as_mut() else {
            return Ok(());
        };
        if window.mapped {
            let id = window.id;
            window.mapped = false;
            checked("unmap_window", self.conn.unmap_window(id))?;
        }
        Ok(())
    }

    fn slot(&mut self, slot: WindowSlot) -> &mut Option<OverlayWindow> {
        match slot {
            WindowSlot::Text => &mut self.text,
            WindowSlot::Underline => &mut self.underline,
        }
    }

    fn create_window(&self, box_: WindowBox) -> Result<OverlayWindow, PlatformError> {
        let id = self
            .conn
            .generate_id()
            .map_err(|err| cannot_complete("window id", err))?;
        let mut aux = CreateWindowAux::new()
            // The whole point: unmanaged by the window manager, so it is never
            // decorated, never reparented, and never handed the input focus.
            .override_redirect(1)
            // No event mask at all. Selecting nothing means the server never
            // considers this window a candidate for key or button delivery, and
            // there is no event loop here to drain.
            .event_mask(EventMask::NO_EVENT)
            // Explicit for a depth that differs from the parent's: X11 would
            // otherwise inherit the root's pixel values and fail with BadMatch.
            .background_pixel(0)
            .border_pixel(0);
        if let Some(colormap) = self.colormap {
            aux = aux.colormap(colormap);
        }
        checked(
            "create_window",
            self.conn.create_window(
                self.depth,
                id,
                self.root,
                clamp_i16(box_.x),
                clamp_i16(box_.y),
                box_.w,
                box_.h,
                0,
                WindowClass::INPUT_OUTPUT,
                self.visual,
                &aux,
            ),
        )?;

        // Empty input region: every click, scroll and drag over the overlay goes
        // to the application underneath. This is not cosmetic — without it the
        // ghost would eat the user's clicks on their own text field.
        checked(
            "empty input shape",
            self.conn.shape_rectangles(
                shape::SO::SET,
                shape::SK::INPUT,
                ClipOrdering::UNSORTED,
                id,
                0,
                0,
                &[],
            ),
        )?;

        // Advisory for compositors (no shadow, no animation on a tooltip).
        // Override-redirect means no WM reads it, so a failure to intern the
        // atoms is not worth failing the overlay for.
        if let Some((property, value)) = self.window_type {
            checked(
                "window type",
                self.conn.change_property32(
                    PropMode::REPLACE,
                    id,
                    property,
                    AtomEnum::ATOM,
                    &[value],
                ),
            )?;
        }

        let gc = self
            .conn
            .generate_id()
            .map_err(|err| cannot_complete("gc id", err))?;
        checked(
            "create_gc",
            self.conn.create_gc(gc, id, &CreateGCAux::new()),
        )?;

        Ok(OverlayWindow {
            id,
            gc,
            pixmap: None,
            box_,
            mapped: false,
        })
    }

    /// Upload `canvas` as the window's background pixmap and shape.
    ///
    /// The background pixmap, rather than drawing into the window, is what makes
    /// the overlay survive expose events with no event loop: the server repaints
    /// from it by itself. `clear_area` forces that repaint for the content
    /// already on screen.
    fn upload(&self, window: &mut OverlayWindow, canvas: &Canvas) -> Result<(), PlatformError> {
        let pixmap = self
            .conn
            .generate_id()
            .map_err(|err| cannot_complete("pixmap id", err))?;
        checked(
            "create_pixmap",
            self.conn
                .create_pixmap(self.depth, pixmap, window.id, canvas.w, canvas.h),
        )?;

        // Row bands, so a wide window cannot exceed the connection's maximum
        // request length. 32 bytes covers the PutImage header with room to spare.
        let row_bytes = usize::from(canvas.w) * 4;
        let rows_per_request = self
            .max_request_bytes
            .saturating_sub(32)
            .checked_div(row_bytes.max(1))
            .unwrap_or(1)
            .max(1);
        let bytes = canvas.to_x11_bytes(self.lsb_first);
        for band in 0..usize::from(canvas.h).div_ceil(rows_per_request) {
            let first = band * rows_per_request;
            let rows = rows_per_request.min(usize::from(canvas.h) - first);
            let slice = &bytes[first * row_bytes..(first + rows) * row_bytes];
            checked(
                "put_image",
                self.conn.put_image(
                    ImageFormat::Z_PIXMAP,
                    pixmap,
                    window.gc,
                    canvas.w,
                    rows as u16,
                    0,
                    clamp_i16(first as i32),
                    0,
                    self.depth,
                    slice,
                ),
            )?;
        }

        checked(
            "background_pixmap",
            self.conn.change_window_attributes(
                window.id,
                &ChangeWindowAttributesAux::new().background_pixmap(pixmap),
            ),
        )?;
        checked(
            "clear_area",
            self.conn
                .clear_area(false, window.id, 0, 0, canvas.w, canvas.h),
        )?;

        // The bounding shape is what gives transparency on a server with no ARGB
        // visual and on a desktop with no compositor: untouched pixels stop
        // being part of the window.
        let rects: Vec<Rectangle> = visible_rects(&canvas.pixels, canvas.w, canvas.h)
            .into_iter()
            .map(|(x, y, width, height)| Rectangle {
                x,
                y,
                width,
                height,
            })
            .collect();
        checked(
            "bounding shape",
            self.conn.shape_rectangles(
                shape::SO::SET,
                shape::SK::BOUNDING,
                ClipOrdering::YX_BANDED,
                window.id,
                0,
                0,
                &rects,
            ),
        )?;

        if let Some(previous) = window.pixmap.replace(pixmap) {
            checked("free_pixmap", self.conn.free_pixmap(previous))?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), PlatformError> {
        self.conn
            .flush()
            .map_err(|err| cannot_complete("flush", err))
    }
}

#[derive(Clone, Copy)]
enum WindowSlot {
    Text,
    Underline,
}

/// A premultiplied-ARGB pixel buffer, row-major.
struct Canvas {
    w: u16,
    h: u16,
    pixels: Vec<u32>,
}

impl Canvas {
    fn filled(w: u16, h: u16, colour: u32) -> Self {
        Self {
            w,
            h,
            pixels: vec![colour; usize::from(w) * usize::from(h)],
        }
    }

    /// Source-over one glyph pixel. `coverage` is fontdue's 8-bit alpha mask and
    /// `colour` is already premultiplied, so the blend is the plain
    /// `src*a + dst*(1-a)` with `src` scaled by the coverage.
    fn blend(&mut self, x: i32, y: i32, coverage: u8, colour: u32) {
        if coverage == 0 || x < 0 || y < 0 || x >= i32::from(self.w) || y >= i32::from(self.h) {
            return;
        }
        let index = y as usize * usize::from(self.w) + x as usize;
        let src_a = ((colour >> 24) & 0xFF) * u32::from(coverage) / 255;
        if src_a == 0 {
            return;
        }
        let mut out = src_a << 24;
        for shift in [16, 8, 0] {
            let src = ((colour >> shift) & 0xFF) * u32::from(coverage) / 255;
            let dst = (self.pixels[index] >> shift) & 0xFF;
            out |= (src + dst * (255 - src_a) / 255).min(0xFF) << shift;
        }
        self.pixels[index] = out;
    }

    /// Serialize for `PutImage` with `ZPixmap` at 32 bits per pixel. The server's
    /// `image_byte_order` decides the byte order inside each pixel; getting it
    /// backwards swaps red and blue, which is the classic silent X11 colour bug.
    fn to_x11_bytes(&self, lsb_first: bool) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.pixels.len() * 4);
        for pixel in &self.pixels {
            if lsb_first {
                bytes.extend_from_slice(&pixel.to_le_bytes());
            } else {
                bytes.extend_from_slice(&pixel.to_be_bytes());
            }
        }
        bytes
    }
}

/// Prefer a depth-32 TrueColor visual (an alpha channel a compositor can blend),
/// else fall back to the root's own visual and report that alpha is unavailable.
fn pick_visual(screen: &x11rb::protocol::xproto::Screen) -> (u8, u32, bool) {
    let argb = screen
        .allowed_depths
        .iter()
        .filter(|depth| depth.depth == 32)
        .flat_map(|depth| depth.visuals.iter())
        .find(|visual| visual.class == VisualClass::TRUE_COLOR);
    match argb {
        Some(visual) => (32, visual.visual_id, true),
        None => (screen.root_depth, screen.root_visual, false),
    }
}

/// `(_NET_WM_WINDOW_TYPE, _NET_WM_WINDOW_TYPE_TOOLTIP)`, or `None` when either
/// atom cannot be interned — advisory only, so it must not fail the overlay.
fn intern_window_type(conn: &RustConnection) -> Option<(u32, u32)> {
    let property = conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE").ok()?;
    let value = conn
        .intern_atom(false, b"_NET_WM_WINDOW_TYPE_TOOLTIP")
        .ok()?;
    Some((property.reply().ok()?.atom, value.reply().ok()?.atom))
}

/// The font to draw with: `COMPME_FONT` when set, else the best face the scan
/// finds. A missing font is a fail-closed error naming every directory tried —
/// never a panic, and never a silent blank overlay.
fn load_font() -> Result<(std::path::PathBuf, Font), PlatformError> {
    if let Some(explicit) = std::env::var_os(FONT_ENV) {
        let path = std::path::PathBuf::from(explicit);
        // An operator who set the variable wants *that* font; silently scanning
        // past a typo would hide the mistake.
        let bytes = std::fs::read(&path)
            .map_err(|err| cannot_complete(&format!("{FONT_ENV}={}", path.display()), err))?;
        return Ok((path.clone(), parse_font(&path, bytes)?));
    }
    let dirs = overlay_font::font_search_dirs(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("XDG_DATA_DIRS").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    );
    let path = overlay_font::find_font_file(&dirs).ok_or_else(|| {
        cannot_complete(
            "font",
            format!(
                "no usable .ttf/.otf found in {}; set {FONT_ENV} to a font path",
                dirs.iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })?;
    let bytes =
        std::fs::read(&path).map_err(|err| cannot_complete(&path.display().to_string(), err))?;
    let font = parse_font(&path, bytes)?;
    Ok((path, font))
}

fn parse_font(path: &std::path::Path, bytes: Vec<u8>) -> Result<Font, PlatformError> {
    Font::from_bytes(bytes, FontSettings::default())
        .map_err(|err| cannot_complete(&format!("font {}", path.display()), err))
}

fn round_u16(value: f64) -> u16 {
    value.round().clamp(1.0, f64::from(u16::MAX)) as u16
}

/// X11 window positions are `i16`. The geometry module already clamped into the
/// root, so this only guards against a screen larger than `i16::MAX`.
fn clamp_i16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplied_scales_colour_by_alpha() {
        assert_eq!(premultiplied(255, 0x12, 0x34, 0x56), 0xFF12_3456);
        // Half alpha halves each channel, so the blend below never has to
        // divide again — the classic source of over-bright overlay text.
        assert_eq!(premultiplied(128, 0xFF, 0xFF, 0xFF), 0x8080_8080);
        assert_eq!(premultiplied(0, 0xFF, 0xFF, 0xFF), 0);
    }

    #[test]
    fn blending_a_glyph_pixel_respects_coverage_and_bounds() {
        let mut canvas = Canvas::filled(2, 2, 0);
        // Zero coverage writes nothing; full coverage writes the colour as-is.
        canvas.blend(0, 0, 0, 0xFFFF_FFFF);
        assert_eq!(canvas.pixels[0], 0);
        canvas.blend(0, 0, 255, 0xFFFF_FFFF);
        assert_eq!(canvas.pixels[0], 0xFFFF_FFFF);
        // Partial coverage over an empty pixel scales alpha and colour together.
        let mut canvas = Canvas::filled(1, 1, 0);
        canvas.blend(0, 0, 128, 0xFFFF_FFFF);
        assert_eq!(canvas.pixels[0], 0x8080_8080);
        // Out-of-bounds is dropped, not wrapped into the next row — a glyph
        // whose bitmap overhangs the box must not smear across the window.
        let mut canvas = Canvas::filled(2, 2, 0);
        for (x, y) in [(-1, 0), (0, -1), (2, 0), (0, 2), (99, 99)] {
            canvas.blend(x, y, 255, 0xFFFF_FFFF);
        }
        assert!(canvas.pixels.iter().all(|p| *p == 0));
    }

    #[test]
    fn pixels_are_serialized_in_the_servers_byte_order() {
        // Getting this backwards swaps red and blue: a silent, plausible-looking
        // colour bug that no headless assertion would otherwise catch.
        let canvas = Canvas {
            w: 1,
            h: 1,
            pixels: vec![0xAA11_2233],
        };
        assert_eq!(canvas.to_x11_bytes(true), vec![0x33, 0x22, 0x11, 0xAA]);
        assert_eq!(canvas.to_x11_bytes(false), vec![0xAA, 0x11, 0x22, 0x33]);
        // Every pixel is four bytes, so a band of rows is a whole number of
        // scanlines and PutImage needs no scanline padding.
        assert_eq!(Canvas::filled(3, 2, 0).to_x11_bytes(true).len(), 24);
    }
}
