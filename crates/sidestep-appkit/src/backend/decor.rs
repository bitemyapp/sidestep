//! Client-side decorations, for compositors that don't draw a title bar
//! (GNOME's among them): an Adwaita-like header bar with the title and the
//! close, maximize and minimize buttons, a shadow, and a border to resize
//! by.
//!
//! They're drawn here, on the render thread, with tiny-skia, rather than
//! by a crate such as sctk-adwaita: the header then renders at fractional
//! scales like the rest of the window, its title is set with the same text
//! drawing as the window's content (the main thread sends it as ops), and
//! nothing runs a subprocess to find a font.
//!
//! Each part is a subsurface of the window's own surface, outside it: the
//! header above, and four strips around the whole for the shadow and the
//! resize handles. The window's surface stays exactly the content, so the
//! content view's coordinates don't change, and the window geometry takes
//! in the header but not the shadow. Parts are synchronized subsurfaces:
//! what they show changes with the next commit of the window's surface.
//! After a resize or a new scale they're drawn only by the present that
//! brings the content drawn for it, so the two show together; changes the
//! content doesn't follow (focus, hover, the title) are committed at once.
//!
//! Pointer input on the parts stays on this thread: dragging the header
//! moves the window (xdg_toplevel.move), double-clicking it maximizes, the
//! right button opens the compositor's window menu, and the border resizes
//! (xdg_toplevel.resize) with the matching cursor. Maximized, tiled and
//! fullscreen windows lose the border and the rounded corners; fullscreen
//! ones lose the header too.
//!
//! The look follows GNOME's: light by default, dark with
//! `SIDESTEP_THEME=dark` or a `GTK_THEME` naming a dark variant.

use std::ops::Range;
use std::sync::atomic::{AtomicU8, Ordering};

use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::client::backend::ObjectId;
use smithay_client_toolkit::reexports::client::protocol::wl_region::WlRegion;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::reexports::client::protocol::wl_subsurface::WlSubsurface;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Proxy, QueueHandle};
use smithay_client_toolkit::reexports::csd_frame::WindowManagerCapabilities;
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::subcompositor::SubcompositorState;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PremultipliedColorU8, Stroke, Transform};

use super::{State, Win, as_pixels, px};
use crate::protocol::{Cursor, FromRender, Rect, Style, TitleText, WindowId, WindowState};
use crate::raster::{self, Canvas, Glyphs};

/// Header height, in points.
pub(crate) const HEADER: u32 = 46;
/// Room around the window for the shadow, in points.
const MARGIN: u32 = 20;
/// How far outside the window the border takes the pointer, and how far
/// along an edge from a corner a corner resize reaches.
const GRIP: u32 = 10;
const CORNER: f64 = 20.0;
/// Radius of the top corners.
const RADIUS: f32 = 12.0;
const BUTTON: f32 = 24.0;
const BUTTON_GAP: f32 = 10.0;
/// From the right edge to the last button, the same as from the top.
const BUTTON_INSET: f32 = (HEADER as f32 - BUTTON) / 2.0;
/// Point size of the title.
pub(crate) const TITLE_SIZE: f64 = 14.0;
/// Points over which a title too long for the header fades out.
const TITLE_FADE: f32 = 24.0;

/// A part of the decorations, as input sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Header,
    Top,
    Bottom,
    Left,
    Right,
}

const STRIPS: [Part; 4] = [Part::Top, Part::Bottom, Part::Left, Part::Right];

/// A button in the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Close,
    Maximize,
    Minimize,
}

/// A subsurface of the window's surface: a part of the decorations, or the
/// menu bar (`menubar`).
pub(super) struct Surface {
    pub(super) surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
    pub(super) mapped: bool,
}

impl Surface {
    pub(super) fn new(
        parent: &WlSurface,
        subcompositor: &SubcompositorState,
        viewporter: &WpViewporter,
        qh: &QueueHandle<State>,
    ) -> Self {
        let (subsurface, surface) = subcompositor.create_subsurface(parent.clone(), qh);
        let viewport = viewporter.get_viewport(&surface, qh, ());
        Surface { surface, subsurface, viewport, mapped: false }
    }

    pub(super) fn hide(&mut self) {
        if self.mapped {
            self.surface.attach(None, 0, 0);
            self.surface.commit();
            self.mapped = false;
        }
    }

    /// Show a new buffer of `size` pixels (premultiplied ARGB), which `draw`
    /// fills, at `at` points relative to the window's surface and `points`
    /// big.
    fn show(
        &mut self,
        pool: &mut SlotPool,
        size: (u32, u32),
        at: (i32, i32),
        points: (u32, u32),
        draw: impl FnOnce(&mut [u32]),
    ) {
        self.show_as(pool, size, wl_shm::Format::Argb8888, at, points, draw);
    }

    /// As `show`, the pixels in `format`.
    pub(super) fn show_as(
        &mut self,
        pool: &mut SlotPool,
        (pw, ph): (u32, u32),
        format: wl_shm::Format,
        at: (i32, i32),
        points: (u32, u32),
        draw: impl FnOnce(&mut [u32]),
    ) {
        let Ok((buffer, bytes)) = pool.create_buffer(pw as i32, ph as i32, pw as i32 * 4, format) else {
            return;
        };
        draw(&mut as_pixels(bytes)[..(pw * ph) as usize]);
        let _ = buffer.attach_to(&self.surface);
        self.surface.damage_buffer(0, 0, pw as i32, ph as i32);
        self.viewport.set_destination(points.0 as i32, points.1 as i32);
        self.subsurface.set_position(at.0, at.1);
        self.surface.commit();
        self.mapped = true;
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.viewport.destroy();
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

/// What the decorations are drawn around.
pub(crate) struct FrameInfo<'a> {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub state: WindowState,
    pub style: Style,
    pub title: &'a TitleText,
    /// The menu bar between the header and the content, in points.
    pub bar: u32,
}

/// What the parts depend on besides the title and the pointer.
fn drawn_key(f: &FrameInfo) -> (u32, u32, u64, WindowState, Style, u32) {
    (f.width, f.height, f.scale.to_bits(), f.state, f.style, f.bar)
}

pub(crate) struct Decor {
    header: Surface,
    strips: [Surface; 4],
    regions: Option<[Region; 4]>,
    /// Pointer input goes through: no part takes it.
    passthrough: bool,
    look: Look,
    header_dirty: bool,
    strips_dirty: bool,
    /// What was last drawn: content size, scale bits, state, style.
    drawn: Option<(u32, u32, u64, WindowState, Style, u32)>,
}

/// How the header looks, apart from the surfaces it's shown on.
struct Look {
    capabilities: WindowManagerCapabilities,
    hover: Option<Control>,
    pressed: Option<Control>,
    /// The title's coverage at the scale it was last drawn: width, height,
    /// scale, one byte a pixel.
    title_mask: Option<(u32, u32, f64, Vec<u8>)>,
    /// The width of the buttons, for the title to stay clear of.
    buttons_width: f32,
}

/// Colors, straight RGBA.
struct Theme {
    header: [f32; 4],
    header_backdrop: [f32; 4],
    fg: [f32; 4],
    separator: [f32; 4],
    shadow: f32,
    outline: f32,
}

fn theme() -> &'static Theme {
    static LIGHT: Theme = Theme {
        header: [0.922, 0.922, 0.922, 1.0],
        header_backdrop: [0.98, 0.98, 0.98, 1.0],
        fg: [0.0, 0.0, 0.0, 0.8],
        separator: [0.0, 0.0, 0.0, 0.1],
        shadow: 0.16,
        outline: 0.14,
    };
    static DARK: Theme = Theme {
        header: [0.188, 0.188, 0.188, 1.0],
        header_backdrop: [0.141, 0.141, 0.141, 1.0],
        fg: [1.0, 1.0, 1.0, 0.9],
        separator: [0.0, 0.0, 0.0, 0.36],
        shadow: 0.35,
        outline: 0.45,
    };
    let dark = match SCHEME.load(Ordering::Relaxed) {
        DARK_SCHEME => true,
        LIGHT_SCHEME => false,
        _ => explicit_theme().unwrap_or(false),
    };
    if dark { &DARK } else { &LIGHT }
}

/// The desktop's preference, once the settings portal has told it.
static SCHEME: AtomicU8 = AtomicU8::new(0);
const LIGHT_SCHEME: u8 = 1;
const DARK_SCHEME: u8 = 2;

/// Dark or light as the environment asks (`SIDESTEP_THEME=dark` or
/// `light`, or a dark `GTK_THEME`), which wins over the desktop's
/// preference.
pub(crate) fn explicit_theme() -> Option<bool> {
    static EXPLICIT: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    *EXPLICIT.get_or_init(|| match std::env::var("SIDESTEP_THEME").as_deref() {
        Ok("dark") => Some(true),
        Ok("light") => Some(false),
        _ => std::env::var("GTK_THEME").ok().filter(|t| t.to_lowercase().contains("dark")).map(|_| true),
    })
}

/// The desktop now prefers dark (or not): decorations drawn from now on
/// follow.
pub(crate) fn set_dark(dark: bool) {
    SCHEME.store(if dark { DARK_SCHEME } else { LIGHT_SCHEME }, Ordering::Relaxed);
}

impl Decor {
    pub fn new(
        parent: &WlSurface,
        compositor: &CompositorState,
        subcompositor: &SubcompositorState,
        viewporter: &WpViewporter,
        qh: &QueueHandle<State>,
    ) -> Self {
        let make = || Surface::new(parent, subcompositor, viewporter, qh);
        let header = make();
        let strips = [make(), make(), make(), make()];
        // The shadow beyond the grip doesn't take input: clicks there go to
        // whatever is behind the window.
        let regions = [(); 4].map(|_| Region::new(compositor).ok());
        let regions = if regions.iter().all(Option::is_some) { Some(regions.map(Option::unwrap)) } else { None };
        Decor {
            header,
            strips,
            regions,
            passthrough: false,
            look: Look::new(),
            header_dirty: true,
            strips_dirty: true,
            drawn: None,
        }
    }

    /// Take no pointer input, given an empty region, or take it again.
    pub fn set_passthrough(&mut self, empty: Option<&WlRegion>) {
        self.passthrough = empty.is_some();
        self.header.surface.set_input_region(empty);
        self.header_dirty = true;
        self.strips_dirty = true;
    }

    /// Our surfaces, for input.
    pub fn surfaces(&self) -> impl Iterator<Item = (ObjectId, Part)> + '_ {
        std::iter::once((self.header.surface.id(), Part::Header))
            .chain(self.strips.iter().zip(STRIPS).map(|(s, p)| (s.surface.id(), p)))
    }

    /// Height of the header, in points: none in fullscreen.
    pub fn titlebar(&self, state: &WindowState) -> u32 {
        if state.fullscreen { 0 } else { HEADER }
    }

    /// Something shown changed since the parts were last drawn for a
    /// window as `f` describes it.
    pub fn needs_draw(&self, f: &FrameInfo) -> bool {
        self.header_dirty || self.strips_dirty || self.drawn != Some(drawn_key(f))
    }

    pub fn title_changed(&mut self) {
        self.look.title_mask = None;
        self.header_dirty = true;
    }

    pub fn scale_changed(&mut self) {
        self.look.title_mask = None;
        self.header_dirty = true;
        self.strips_dirty = true;
    }

    /// The theme changed: draw everything again.
    pub fn restyle(&mut self) {
        self.header_dirty = true;
        self.strips_dirty = true;
    }

    pub fn set_capabilities(&mut self, capabilities: WindowManagerCapabilities) {
        if capabilities != self.look.capabilities {
            self.look.capabilities = capabilities;
            self.header_dirty = true;
        }
    }

    /// Draw what changed and place every part, for a window `f.width` ×
    /// `f.height` points of content.
    pub fn draw(&mut self, f: &FrameInfo, pool: &mut SlotPool, glyphs: &mut Glyphs) {
        let now = drawn_key(f);
        if self.drawn != Some(now) {
            let resized = self.drawn.is_none_or(|d| (d.0, d.1, d.2, d.5) != (now.0, now.1, now.2, now.5));
            self.header_dirty = true;
            self.strips_dirty |= resized || self.drawn.is_none_or(|d| d.3 != now.3);
            self.drawn = Some(now);
        }
        if self.header_dirty {
            self.header_dirty = false;
            if f.state.fullscreen {
                self.header.hide();
            } else if let Some(pm) = self.look.render_header(f, glyphs) {
                let size = (pm.width(), pm.height());
                let top = -((HEADER + f.bar) as i32);
                self.header.show(pool, size, (0, top), (f.width, HEADER), |dst| {
                    for (d, p) in dst.iter_mut().zip(pm.pixels()) {
                        *d = u32::from_be_bytes([p.alpha(), p.red(), p.green(), p.blue()]);
                    }
                });
            }
        }
        if self.strips_dirty {
            self.strips_dirty = false;
            let floating = !(f.state.maximized || f.state.fullscreen || f.state.tiled);
            if floating {
                self.draw_strips(f, pool);
            } else {
                for s in &mut self.strips {
                    s.hide();
                }
            }
        }
    }

    fn draw_strips(&mut self, f: &FrameInfo, pool: &mut SlotPool) {
        let shade = Shadow::new(f, theme());
        let m = MARGIN as i32;
        for (i, place) in strip_places(f.width, f.height, f.bar).into_iter().enumerate() {
            let size = (px(place.size.0, f.scale), px(place.size.1, f.scale));
            if let Some(regions) = &self.regions {
                let region = &regions[i];
                region.subtract(-1_000_000, -1_000_000, 2_000_000, 2_000_000);
                let (g, (sw, sh)) = (GRIP as i32, (place.size.0 as i32, place.size.1 as i32));
                match place.part {
                    _ if self.passthrough => {}
                    Part::Top => region.add(m - g, sh - g, sw - 2 * (m - g), g),
                    Part::Bottom => region.add(m - g, 0, sw - 2 * (m - g), g),
                    Part::Left => region.add(sw - g, 0, g, sh),
                    Part::Right => region.add(0, 0, g, sh),
                    Part::Header => {}
                }
                self.strips[i].surface.set_input_region(Some(region.wl_region()));
            }
            self.strips[i].show(pool, size, place.at, place.size, |dst| {
                strip_pixels(&shade, &place, size, f.scale, dst);
            });
        }
    }
}

impl Look {
    fn new() -> Self {
        Look {
            capabilities: WindowManagerCapabilities::all(),
            hover: None,
            pressed: None,
            title_mask: None,
            buttons_width: 0.0,
        }
    }

    /// The header's buttons, right to left, with their centers.
    fn controls(&self, style: &Style, width: u32) -> Vec<(Control, f32)> {
        let mut controls = Vec::new();
        if style.closable {
            controls.push(Control::Close);
        }
        if style.resizable && self.capabilities.contains(WindowManagerCapabilities::MAXIMIZE) {
            controls.push(Control::Maximize);
        }
        if style.miniaturizable && self.capabilities.contains(WindowManagerCapabilities::MINIMIZE) {
            controls.push(Control::Minimize);
        }
        let mut x = width as f32 - BUTTON_INSET - BUTTON / 2.0;
        controls
            .into_iter()
            .map(|c| {
                let at = (c, x);
                x -= BUTTON + BUTTON_GAP;
                at
            })
            .collect()
    }

    fn control_at(&self, style: &Style, width: u32, x: f64, y: f64) -> Option<Control> {
        let cy = HEADER as f64 / 2.0;
        self.controls(style, width)
            .into_iter()
            .find_map(|(c, cx)| ((x - cx as f64).hypot(y - cy) <= BUTTON as f64 / 2.0 + 1.0).then_some(c))
    }

    /// The header, in pixels at the frame's scale.
    fn render_header(&mut self, f: &FrameInfo, glyphs: &mut Glyphs) -> Option<Pixmap> {
        let t = theme();
        let s = f.scale as f32;
        let (pw, ph) = (px(f.width, f.scale), px(HEADER, f.scale));
        let mut pm = Pixmap::new(pw.max(1), ph.max(1))?;
        let square = f.state.maximized || f.state.tiled;
        let radius = if square { 0.0 } else { RADIUS };
        let xf = Transform::from_scale(s, s);
        let (w, h) = (f.width as f32, HEADER as f32);

        // The shadow shows in the rounded corners' cutouts.
        if radius > 0.0 {
            let shade = Shadow::new(f, t);
            let data = pm.pixels_mut();
            let corner = ((radius * s).ceil() as u32).min(pw);
            for y in 0..corner.min(ph) {
                for x in (0..corner).chain(pw.saturating_sub(corner)..pw) {
                    let (px_, py) = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s - h - f.bar as f32);
                    let a = shade.alpha(px_ as f64, py as f64);
                    if a > 0.0 {
                        let a8 = (a * 255.0 + 0.5) as u8;
                        data[(y * pw + x) as usize] =
                            PremultipliedColorU8::from_rgba(0, 0, 0, a8).expect("black is premultiplied");
                    }
                }
            }
        }

        let active = f.state.activated;
        let mut paint = Paint { anti_alias: true, ..Paint::default() };
        let bg = if active { t.header } else { t.header_backdrop };
        paint.set_color_rgba8(to8(bg[0]), to8(bg[1]), to8(bg[2]), to8(bg[3]));
        if let Some(path) = top_rounded_rect(w, h, radius) {
            pm.fill_path(&path, &paint, FillRule::Winding, xf, None);
        }
        // The separator along the bottom edge.
        paint.set_color_rgba8(to8(t.separator[0]), to8(t.separator[1]), to8(t.separator[2]), to8(t.separator[3]));
        if let Some(r) = tiny_skia::Rect::from_xywh(0.0, h - 1.0 / s, w, 1.0 / s) {
            pm.fill_rect(r, &paint, xf, None);
        }

        // Buttons.
        let fg = t.fg;
        let dim = if active { 1.0 } else { 0.55 };
        let controls = self.controls(&f.style, f.width);
        self.buttons_width = controls.last().map_or(0.0, |(_, cx)| w - cx + BUTTON / 2.0 + BUTTON_INSET);
        for (control, cx) in controls {
            let cy = h / 2.0;
            let level = if self.pressed == Some(control) {
                0.3
            } else if self.hover == Some(control) {
                0.18
            } else {
                0.1
            };
            paint.set_color_rgba8(to8(fg[0]), to8(fg[1]), to8(fg[2]), to8(level * dim));
            if let Some(circle) = PathBuilder::from_circle(cx, cy, BUTTON / 2.0) {
                pm.fill_path(&circle, &paint, FillRule::Winding, xf, None);
            }
            paint.set_color_rgba8(to8(fg[0]), to8(fg[1]), to8(fg[2]), to8(fg[3] * dim));
            let stroke = Stroke { width: 1.5, ..Stroke::default() };
            let mut pb = PathBuilder::new();
            match control {
                Control::Close => {
                    let d = 3.5;
                    pb.move_to(cx - d, cy - d);
                    pb.line_to(cx + d, cy + d);
                    pb.move_to(cx + d, cy - d);
                    pb.line_to(cx - d, cy + d);
                }
                Control::Maximize if f.state.maximized => {
                    // Restore: two overlapping squares.
                    pb.push_rect(tiny_skia::Rect::from_xywh(cx - 4.0, cy - 2.0, 6.0, 6.0).expect("rect"));
                    pb.move_to(cx - 2.0, cy - 4.0);
                    pb.line_to(cx + 4.0, cy - 4.0);
                    pb.line_to(cx + 4.0, cy + 2.0);
                }
                Control::Maximize => {
                    pb.push_rect(tiny_skia::Rect::from_xywh(cx - 4.0, cy - 4.0, 8.0, 8.0).expect("rect"));
                }
                Control::Minimize => {
                    pb.move_to(cx - 4.0, cy + 3.5);
                    pb.line_to(cx + 4.0, cy + 3.5);
                }
            }
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &paint, &stroke, xf, None);
            }
        }

        self.draw_title(&mut pm, f, glyphs, fg, dim);
        Some(pm)
    }

    /// The title, centered in the window if it fits there, else in the room
    /// the buttons leave, and cut off where it doesn't fit.
    fn draw_title(&mut self, pm: &mut Pixmap, f: &FrameInfo, glyphs: &mut Glyphs, fg: [f32; 4], dim: f32) {
        let s = f.scale;
        let title = f.title;
        if title.ops.is_empty() || title.width <= 0.0 {
            return;
        }
        if self.title_mask.as_ref().is_none_or(|m| m.2 != s) {
            self.title_mask = Some(title_mask(title, s, glyphs));
        }
        let Some((mw, mh, _, mask)) = &self.title_mask else { return };
        let w = f.width as f32;
        let room = (self.buttons_width + 8.0).min(w / 2.0);
        let x = if title.width <= w - 2.0 * room {
            (w - title.width) / 2.0
        } else {
            (room / 2.0).max((w - room - title.width) / 2.0)
        };
        let right = w - room;
        let y = (HEADER as f32 - title.height) / 2.0;
        let (x0, y0) = ((x * s as f32).round() as i64, (y * s as f32).round() as i64);
        let (pw, ph) = (pm.width() as i64, pm.height() as i64);
        let clip = ((right * s as f32).floor() as i64).min(pw);
        // A title too long for its room fades out before the buttons.
        let overflows = x + title.width > right;
        let fade = if overflows { (TITLE_FADE * s as f32).max(1.0) } else { 0.0 };
        let color = [fg[0], fg[1], fg[2]];
        let alpha = fg[3] * dim;
        let data = pm.pixels_mut();
        for my in 0..*mh as i64 {
            let py = y0 + my;
            if py < 0 || py >= ph {
                continue;
            }
            for mx in 0..*mw as i64 {
                let px_ = x0 + mx;
                if px_ < 0 || px_ >= clip {
                    continue;
                }
                let mut cov = mask[(my * *mw as i64 + mx) as usize] as f32 / 255.0 * alpha;
                if overflows {
                    cov *= ((clip - px_) as f32 / fade).min(1.0);
                }
                if cov <= 0.0 {
                    continue;
                }
                let d = &mut data[(py * pw + px_) as usize];
                *d = over(*d, color, cov);
            }
        }
    }
}

fn to8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// `color` at `alpha` coverage over a premultiplied pixel.
fn over(d: PremultipliedColorU8, color: [f32; 3], alpha: f32) -> PremultipliedColorU8 {
    let inv = 1.0 - alpha;
    let mix = |dst: u8, src: f32| (dst as f32 * inv + src * alpha * 255.0 + 0.5) as u8;
    let a = mix(d.alpha(), 1.0);
    let (r, g, b) = (mix(d.red(), color[0]), mix(d.green(), color[1]), mix(d.blue(), color[2]));
    PremultipliedColorU8::from_rgba(r.min(a), g.min(a), b.min(a), a).expect("clamped to alpha")
}

/// A rectangle with its top corners rounded.
fn top_rounded_rect(w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    if r <= 0.0 {
        pb.push_rect(tiny_skia::Rect::from_xywh(0.0, 0.0, w, h)?);
        return pb.finish();
    }
    // Circular arcs as cubic Béziers.
    let k = r * 0.552_284_8;
    pb.move_to(0.0, h);
    pb.line_to(0.0, r);
    pb.cubic_to(0.0, r - k, r - k, 0.0, r, 0.0);
    pb.line_to(w - r, 0.0);
    pb.cubic_to(w - r + k, 0.0, w, r - k, w, r);
    pb.line_to(w, h);
    pb.close();
    pb.finish()
}

/// Rasterize the title's ops into a coverage mask at `scale`.
fn title_mask(title: &TitleText, scale: f64, glyphs: &mut Glyphs) -> (u32, u32, f64, Vec<u8>) {
    let (w, h) = (px(title.width.ceil() as u32, scale).max(1), px(title.height.ceil() as u32, scale).max(1));
    let mut px_ = vec![0u32; (w * h) as usize];
    let mut canvas = Canvas::new(&mut px_, w, h, 0.0, scale as f32);
    let all = Rect::new(0.0, 0.0, title.width.ceil(), title.height.ceil());
    raster::paint(&mut canvas, glyphs, &[all], &title.ops);
    // The title is set in white on nothing: its alpha is the coverage.
    let mask = px_.iter().map(|p| raster::channels(*p)[3]).collect();
    (w, h, scale, mask)
}

/// Where a strip of the border goes: position and size in points,
/// relative to the window's surface.
struct StripPlace {
    part: Part,
    at: (i32, i32),
    size: (u32, u32),
}

fn strip_places(width: u32, height: u32, bar: u32) -> [StripPlace; 4] {
    let (m, h) = (MARGIN, height + HEADER + bar);
    let (mi, top) = (MARGIN as i32, -((HEADER + bar) as i32));
    [
        StripPlace { part: Part::Top, at: (-mi, top - mi), size: (width + 2 * m, m) },
        StripPlace { part: Part::Bottom, at: (-mi, height as i32), size: (width + 2 * m, m) },
        StripPlace { part: Part::Left, at: (-mi, top), size: (m, h) },
        StripPlace { part: Part::Right, at: (width as i32, top), size: (m, h) },
    ]
}

/// A strip's shadow in premultiplied ARGB, `size` pixels. Along the edges,
/// away from the corners, the shadow depends only on the distance from
/// the edge, so it's worked out once a row (above and below the window) or
/// once a column (at its sides) and copied.
fn strip_pixels(shade: &Shadow, place: &StripPlace, (pw, ph): (u32, u32), scale: f64, dst: &mut [u32]) {
    let (x0, y0) = (place.at.0 as f64, place.at.1 as f64);
    let point = |px_: u32, py: u32| (x0 + (px_ as f64 + 0.5) / scale, y0 + (py as f64 + 0.5) / scale);
    let pixel = |px_: u32, py: u32| {
        let (x, y) = point(px_, py);
        ((shade.alpha(x, y) * 255.0 + 0.5) as u32) << 24
    };
    let (flat_x, flat_y) = shade.flat();
    let pw_ = pw as usize;
    match place.part {
        Part::Top | Part::Bottom => {
            for py in 0..ph {
                let row = &mut dst[py as usize * pw_..(py as usize + 1) * pw_];
                let mut flat = None;
                for px_ in 0..pw {
                    row[px_ as usize] = if flat_x.contains(&point(px_, py).0) {
                        *flat.get_or_insert_with(|| pixel(px_, py))
                    } else {
                        pixel(px_, py)
                    };
                }
            }
        }
        _ => {
            let mut profile: Option<Vec<u32>> = None;
            for py in 0..ph {
                let row = &mut dst[py as usize * pw_..(py as usize + 1) * pw_];
                if flat_y.contains(&point(0, py).1) {
                    let profile = profile.get_or_insert_with(|| (0..pw).map(|px_| pixel(px_, py)).collect());
                    row.copy_from_slice(profile);
                } else {
                    for px_ in 0..pw {
                        row[px_ as usize] = pixel(px_, py);
                    }
                }
            }
        }
    }
}

/// The window's shadow and outline, around the header and content with
/// the header's top corners rounded.
struct Shadow {
    /// The window, in points relative to its surface.
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    radius: f64,
    outline: f64,
    /// The shadow's alpha by distance from the window, in steps of
    /// 1 / [`SHADOW_STEPS`] point: a table, as every pixel of the border
    /// looks it up.
    falloff: Vec<f32>,
}

const SHADOW_STEPS: f64 = 8.0;
/// How far the shadow is cast down.
const SHADOW_DROP: f64 = 2.0;

impl Shadow {
    fn new(f: &FrameInfo, t: &Theme) -> Self {
        let square = f.state.maximized || f.state.tiled;
        let strength = if f.state.activated { t.shadow as f64 } else { t.shadow as f64 * 0.5 };
        let sigma = 6.0;
        let steps = ((MARGIN as f64 + SHADOW_DROP + 2.0) * SHADOW_STEPS) as usize;
        let falloff = (0..steps)
            .map(|i| {
                let d = i as f64 / SHADOW_STEPS;
                (strength * (-(d * d) / (2.0 * sigma * sigma)).exp()) as f32
            })
            .collect();
        Shadow {
            x0: 0.0,
            y0: -((HEADER + f.bar) as f64),
            x1: f.width as f64,
            y1: f.height as f64,
            radius: if square { 0.0 } else { RADIUS as f64 },
            outline: t.outline as f64,
            falloff,
        }
    }

    /// Where the shadow depends on one coordinate only: `x` between the
    /// rounded corners (above and below the window), `y` below them and
    /// above the bottom edge (beside it), the drop included.
    fn flat(&self) -> (Range<f64>, Range<f64>) {
        let r = self.radius;
        (self.x0 + r..self.x1 - r, self.y0 + r + SHADOW_DROP..self.y1)
    }

    /// Distance from the window's outline, positive outside, for a window
    /// moved down by `dy`.
    fn distance(&self, x: f64, y: f64, dy: f64) -> f64 {
        let (y0, y1) = (self.y0 + dy, self.y1 + dy);
        let r = self.radius;
        // Only the top corners are rounded.
        if y < y0 + r && (x < self.x0 + r || x > self.x1 - r) {
            let cx = if x < self.x0 + r { self.x0 + r } else { self.x1 - r };
            return (x - cx).hypot(y - (y0 + r)) - r;
        }
        let dx = (self.x0 - x).max(x - self.x1);
        let dyy = (y0 - y).max(y - y1);
        if dx > 0.0 && dyy > 0.0 { dx.hypot(dyy) } else { dx.max(dyy) }
    }

    /// Premultiplied alpha of the shadow and outline at a point (points).
    fn alpha(&self, x: f64, y: f64) -> f32 {
        let d = self.distance(x, y, 0.0);
        if d < 0.0 {
            return 0.0;
        }
        // A 1-point outline, then a soft shadow cast slightly downward.
        let outline = if d < 1.0 { self.outline * (1.0 - d).clamp(0.0, 1.0) } else { 0.0 };
        let ds = self.distance(x, y, SHADOW_DROP).max(0.0);
        let shadow = self.falloff.get((ds * SHADOW_STEPS) as usize).copied().unwrap_or(0.0) as f64;
        (outline + shadow * (1.0 - outline)).min(1.0) as f32
    }
}

// Input.

fn with_decor<R>(state: &mut State, window: WindowId, f: impl FnOnce(&mut Decor, &Win) -> R) -> Option<R> {
    let win = state.windows.get_mut(&window)?;
    let mut decor = win.decor.take()?;
    let r = f(&mut decor, win);
    win.decor = Some(decor);
    Some(r)
}

pub(super) fn pointer_moved(state: &mut State, window: WindowId, part: Part, x: f64, y: f64) {
    let changed = with_decor(state, window, |d, win| {
        let hover = if part == Part::Header { d.look.control_at(&win.style, win.width, x, y) } else { None };
        let changed = hover != d.look.hover;
        d.look.hover = hover;
        d.header_dirty |= changed;
        changed
    });
    if changed == Some(true) {
        state.refresh_decor(window);
    }
}

pub(super) fn pointer_left(state: &mut State, window: WindowId, _part: Part) {
    let changed = with_decor(state, window, |d, _| {
        let changed = d.look.hover.is_some() || d.look.pressed.is_some();
        (d.look.hover, d.look.pressed) = (None, None);
        d.header_dirty |= changed;
        changed
    });
    if changed == Some(true) {
        state.refresh_decor(window);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pointer_button(
    state: &mut State,
    window: WindowId,
    part: Part,
    (x, y): (f64, f64),
    code: u32,
    pressed: bool,
    clicks: u32,
    seat: &WlSeat,
    serial: u32,
) {
    const LEFT: u32 = 0x110;
    const RIGHT: u32 = 0x111;
    let Some(win) = state.windows.get_mut(&window) else { return };
    let (style, width, win_state) = (win.style, win.width, win.state);
    let Some(toplevel) = win.toplevel().cloned() else { return };
    let Some(d) = win.decor.as_mut() else { return };
    if part != Part::Header {
        if pressed && code == LEFT && style.resizable {
            toplevel.resize(seat, serial, edge(part, width, win.height + win.bar_height(), x, y));
        }
        return;
    }
    let look = &mut d.look;
    let control = look.control_at(&style, width, x, y);
    match (code, pressed) {
        (LEFT, true) => {
            if control.is_some() {
                look.pressed = control;
                d.header_dirty = true;
            } else if clicks == 2 {
                if style.resizable && look.capabilities.contains(WindowManagerCapabilities::MAXIMIZE) {
                    toggle_maximized(&toplevel, &win_state);
                }
            } else if style.movable {
                toplevel.move_(seat, serial);
            }
        }
        (LEFT, false) => {
            let was = look.pressed.take();
            d.header_dirty |= was.is_some();
            if was.is_some() && was == control {
                match control {
                    Some(Control::Close) => {
                        let _ = state.to_main.send(FromRender::CloseRequested { window });
                    }
                    Some(Control::Maximize) => toggle_maximized(&toplevel, &win_state),
                    Some(Control::Minimize) => toplevel.set_minimized(),
                    None => {}
                }
            }
        }
        (RIGHT, true) if control.is_none() && look.capabilities.contains(WindowManagerCapabilities::WINDOW_MENU) => {
            toplevel.show_window_menu(seat, serial, (x as i32, y as i32));
        }
        _ => {}
    }
    state.refresh_decor(window);
}

fn toggle_maximized(toplevel: &smithay_client_toolkit::shell::xdg::window::Window, state: &WindowState) {
    if state.maximized {
        toplevel.unset_maximized();
    } else {
        toplevel.set_maximized();
    }
}

/// Where in the window a point on a strip is: frame coordinates in points
/// from the top left of the header.
fn frame_point(part: Part, width: u32, height: u32, x: f64, y: f64) -> (f64, f64) {
    let m = MARGIN as f64;
    let (w, h) = (width as f64, (height + HEADER) as f64);
    match part {
        Part::Top => (x - m, y - m),
        Part::Bottom => (x - m, h + y),
        Part::Left => (x - m, y),
        Part::Right => (w + x, y),
        Part::Header => (x, y),
    }
}

fn edge(part: Part, width: u32, height: u32, x: f64, y: f64) -> ResizeEdge {
    let (fx, fy) = frame_point(part, width, height, x, y);
    let (w, h) = (width as f64, (height + HEADER) as f64);
    let (left, right) = (fx < CORNER, fx >= w - CORNER);
    let (top, bottom) = (fy < CORNER, fy >= h - CORNER);
    match part {
        Part::Top | Part::Bottom => {
            let top_side = part == Part::Top;
            match (left, right, top_side) {
                (true, _, true) => ResizeEdge::TopLeft,
                (_, true, true) => ResizeEdge::TopRight,
                (_, _, true) => ResizeEdge::Top,
                (true, _, false) => ResizeEdge::BottomLeft,
                (_, true, false) => ResizeEdge::BottomRight,
                _ => ResizeEdge::Bottom,
            }
        }
        Part::Left if top => ResizeEdge::TopLeft,
        Part::Left if bottom => ResizeEdge::BottomLeft,
        Part::Left => ResizeEdge::Left,
        Part::Right if top => ResizeEdge::TopRight,
        Part::Right if bottom => ResizeEdge::BottomRight,
        Part::Right => ResizeEdge::Right,
        Part::Header => ResizeEdge::None,
    }
}

/// The cursor over a part of the decorations.
pub(super) fn cursor(win: &Win, part: Part, x: f64, y: f64) -> Cursor {
    if part == Part::Header || !win.style.resizable {
        return Cursor::Default;
    }
    // The frame takes in the menu bar as well as the content.
    match edge(part, win.width, win.height + win.bar_height(), x, y) {
        ResizeEdge::Top => Cursor::NResize,
        ResizeEdge::Bottom => Cursor::SResize,
        ResizeEdge::Left => Cursor::WResize,
        ResizeEdge::Right => Cursor::EResize,
        ResizeEdge::TopLeft => Cursor::NwResize,
        ResizeEdge::TopRight => Cursor::NeResize,
        ResizeEdge::BottomLeft => Cursor::SwResize,
        ResizeEdge::BottomRight => Cursor::SeResize,
        _ => Cursor::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::super::median;
    use super::*;

    fn frame(title: &TitleText, width: u32, height: u32, scale: f64) -> FrameInfo<'_> {
        let style = Style {
            titled: true,
            closable: true,
            miniaturizable: true,
            resizable: true,
            movable: true,
            passthrough: false,
        };
        let state = WindowState { activated: true, ..Default::default() };
        FrameInfo { width, height, scale, state, style, title, bar: 0 }
    }

    #[test]
    fn edges_and_corners() {
        // A 400 × 300 window: the frame is 400 × 346 with the header.
        let e = |part, x, y| edge(part, 400, 300, x, y);
        assert_eq!(e(Part::Top, 200.0, 15.0), ResizeEdge::Top);
        assert_eq!(e(Part::Top, 5.0, 15.0), ResizeEdge::TopLeft);
        assert_eq!(e(Part::Top, 435.0, 15.0), ResizeEdge::TopRight);
        assert_eq!(e(Part::Bottom, 200.0, 2.0), ResizeEdge::Bottom);
        assert_eq!(e(Part::Bottom, 25.0, 2.0), ResizeEdge::BottomLeft);
        assert_eq!(e(Part::Left, 15.0, 100.0), ResizeEdge::Left);
        assert_eq!(e(Part::Left, 15.0, 5.0), ResizeEdge::TopLeft);
        assert_eq!(e(Part::Right, 2.0, 340.0), ResizeEdge::BottomRight);
        assert_eq!(e(Part::Right, 2.0, 100.0), ResizeEdge::Right);
    }

    #[test]
    fn shadow_falls_off_outside_only() {
        let title = TitleText::default();
        let s = Shadow::new(&frame(&title, 400, 300, 1.0), theme());
        assert_eq!(s.alpha(200.0, 100.0), 0.0, "inside the content");
        assert_eq!(s.alpha(200.0, -30.0), 0.0, "inside the header");
        let near = s.alpha(-2.0, 100.0);
        let far = s.alpha(-15.0, 100.0);
        assert!(near > far && far >= 0.0, "{near} {far}");
        // The rounded corner's cutout has shadow.
        assert!(s.alpha(0.5, -(HEADER as f64) + 0.5) > 0.0);
    }

    #[test]
    fn strips_copy_only_where_the_shadow_is_flat() {
        // Every pixel as computed on its own equals the strip drawn with
        // rows and columns copied.
        let title = TitleText::default();
        for scale in [1.0, 1.5, 2.0] {
            let f = frame(&title, 300, 200, scale);
            let shade = Shadow::new(&f, theme());
            for place in strip_places(f.width, f.height, f.bar) {
                let size = (px(place.size.0, scale), px(place.size.1, scale));
                let mut fast = vec![0u32; (size.0 * size.1) as usize];
                strip_pixels(&shade, &place, size, scale, &mut fast);
                for py in 0..size.1 {
                    for px_ in 0..size.0 {
                        let x = place.at.0 as f64 + (px_ as f64 + 0.5) / scale;
                        let y = place.at.1 as f64 + (py as f64 + 0.5) / scale;
                        let slow = ((shade.alpha(x, y) * 255.0 + 0.5) as u32) << 24;
                        assert_eq!(fast[(py * size.0 + px_) as usize], slow, "{:?} at {px_},{py}", place.part);
                    }
                }
            }
        }
    }

    #[test]
    fn buttons_follow_style_and_capabilities() {
        let mut look = Look::new();
        let all = Style {
            titled: true,
            closable: true,
            miniaturizable: true,
            resizable: true,
            movable: true,
            passthrough: false,
        };
        let names = |look: &Look, style: &Style| look.controls(style, 400).into_iter().map(|c| c.0).collect::<Vec<_>>();
        assert_eq!(names(&look, &all), [Control::Close, Control::Maximize, Control::Minimize]);
        look.capabilities = WindowManagerCapabilities::MAXIMIZE;
        assert_eq!(names(&look, &all), [Control::Close, Control::Maximize]);
        let fixed = Style { resizable: false, ..all };
        assert_eq!(names(&look, &fixed), [Control::Close]);
        // The close button sits as far from the right edge as from the top.
        let (_, cx) = look.controls(&all, 400)[0];
        assert_eq!(400.0 - cx - BUTTON / 2.0, BUTTON_INSET);
        assert_eq!(look.control_at(&all, 400, cx as f64, HEADER as f64 / 2.0), Some(Control::Close));
        assert_eq!(look.control_at(&all, 400, 200.0, 20.0), None);
    }

    /// `cargo test --release -p sidestep-appkit decor_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn decor_timing() {
        let title = TitleText::default();
        for scale in [1.0, 2.0] {
            let f = frame(&title, 1280, 800, scale);
            let mut look = Look::new();
            let mut glyphs = Glyphs::default();
            let header = median(|| drop(look.render_header(&f, &mut glyphs)));
            let shade = Shadow::new(&f, theme());
            let strips = median(|| {
                for place in strip_places(f.width, f.height, f.bar) {
                    let size = (px(place.size.0, scale), px(place.size.1, scale));
                    let mut dst = vec![0u32; (size.0 * size.1) as usize];
                    strip_pixels(&shade, &place, size, scale, &mut dst);
                }
            });
            println!("1280 × 800 at {scale}x: header {header:.2} ms, shadow and border {strips:.2} ms");
        }
    }
}
