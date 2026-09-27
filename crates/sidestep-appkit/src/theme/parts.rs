//! Painters for the parts controls are made of, in the coordinates of the
//! view being drawn. Each takes the rectangle AppKit's geometry gives the
//! part and the part's state, and draws Adwaita's version of it through
//! [`paint`](super::paint).
//!
//! Adwaita's rules, as applied here: buttons and fields are washes of the
//! foreground color with 6-point corners (pills and circles where the
//! shape asks); what is on, chosen or in progress is the accent; disabled
//! parts draw at half strength; the focus ring is the accent at half
//! strength, 2 points wide, just outside the part's shape.

use std::f64::consts::{PI, TAU};

use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::paint::{self, Radii, radii};
use super::palette::{Palette, dimmed, faded};
use crate::protocol::Color;

/// A part's state, as painters take it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct State {
    pub disabled: bool,
    /// Pressed (a cell's highlight).
    pub pressed: bool,
    /// On, checked or selected.
    pub on: bool,
    /// Mixed (a check box's third state).
    pub mixed: bool,
}

/// Corner radius of buttons, fields and cards.
pub(crate) const RADIUS: f64 = 6.0;

/// The outline a part's focus ring follows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Outline {
    RoundRect(NSRect, Radii),
    Ellipse(NSRect),
}

/// `c` at half strength when disabled.
fn state_color(c: Color, s: State) -> Color {
    if s.disabled { dimmed(c) } else { c }
}

fn inset(r: NSRect, dx: f64, dy: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(r.origin.x + dx, r.origin.y + dy),
        NSSize::new(r.size.width - 2.0 * dx, r.size.height - 2.0 * dy),
    )
}

fn center(r: NSRect) -> NSPoint {
    NSPoint::new(r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0)
}

/// A square of side `side` centered in `r`.
pub(crate) fn centered_square(r: NSRect, side: f64) -> NSRect {
    let c = center(r);
    NSRect::new(NSPoint::new(c.x - side / 2.0, c.y - side / 2.0), NSSize::new(side, side))
}

/// Which way up a view's y axis points, to place things visually.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Axis {
    pub flipped: bool,
}

impl Axis {
    /// `dy` points further down the screen from `y`.
    pub fn down(self, y: f64, dy: f64) -> f64 {
        if self.flipped { y + dy } else { y - dy }
    }

    /// The visual top edge of `r`.
    pub fn top(self, r: NSRect) -> f64 {
        if self.flipped { r.origin.y } else { r.origin.y + r.size.height }
    }
}

// Buttons

/// How a push-like bezel is colored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Emphasis {
    /// The usual wash.
    Normal,
    /// The default button (Return): the accent.
    Default,
    /// A destructive action: red.
    Destructive,
    /// A color the program set (`bezelColor`).
    Tinted(Color),
    /// The usual wash on an emphasized background (a selected row of the
    /// key window's focused table): light, as the text there is. macOS
    /// washes with white at about 13%, and 30% pressed
    /// (`conformance/tests/cell_backgrounds.rs`, `button_titles`).
    OnSelection,
}

/// A button's rounded bezel filling `r`.
pub(crate) fn button_bezel(p: &Palette, r: NSRect, radius: Radii, emphasis: Emphasis, s: State) {
    let fill = match emphasis {
        Emphasis::Normal if s.pressed || s.on => p.button_pressed,
        Emphasis::Normal => p.button,
        Emphasis::Default => p.accent,
        Emphasis::Destructive => p.destructive,
        Emphasis::Tinted(c) => c,
        Emphasis::OnSelection if s.pressed || s.on => faded(p.accent_text_on, 0.3),
        Emphasis::OnSelection => faded(p.accent_text_on, 0.13),
    };
    let washed = matches!(emphasis, Emphasis::Normal | Emphasis::OnSelection);
    let fill = if s.pressed && !washed { darker(fill) } else { fill };
    paint::fill_round_rect(r, radius, state_color(fill, s));
}

/// The text color on a button with `emphasis`.
pub(crate) fn button_text(p: &Palette, emphasis: Emphasis, s: State) -> Color {
    let c = match emphasis {
        Emphasis::Normal => p.label,
        _ => p.accent_text_on,
    };
    state_color(c, s)
}

/// A pop-up (or pull-down) button: a push bezel with a downward chevron
/// centered in the `arrow_width` points at its trailing end, where the
/// title stops. `NSPopUpButtonCell` draws with it.
#[allow(dead_code)] // Until NSPopUpButtonCell lands (the menus work).
pub(crate) fn pop_up(p: &Palette, r: NSRect, axis: Axis, arrow_width: f64, s: State) {
    button_bezel(p, r, radii(RADIUS), Emphasis::Normal, s);
    let arrow = NSRect::new(
        NSPoint::new(r.origin.x + r.size.width - arrow_width, r.origin.y),
        NSSize::new(arrow_width, r.size.height),
    );
    chevron(arrow, axis, 8.0, false, state_color(p.label, s));
}

/// A pressed accent: darkened a little.
fn darker(c: Color) -> Color {
    [c[0] * 0.85, c[1] * 0.85, c[2] * 0.85, c[3]]
}

/// A check box: a rounded square, filled with the accent and a check mark
/// (or a dash when mixed) when on.
pub(crate) fn check_box(p: &Palette, r: NSRect, axis: Axis, s: State) {
    let side = r.size.width.min(r.size.height);
    let square = centered_square(r, side);
    let radius = radii((side / 4.0).round());
    if s.on || s.mixed {
        let fill = if s.pressed { darker(p.accent) } else { p.accent };
        paint::fill_round_rect(square, radius, state_color(fill, s));
        let ink = state_color(p.accent_text_on, s);
        let (x, w) = (square.origin.x, side);
        let at = |fx: f64, fy: f64| NSPoint::new(x + fx * w, axis.down(axis.top(square), fy * w));
        if s.mixed {
            paint::stroke_polyline(&[at(0.25, 0.5), at(0.75, 0.5)], side / 8.0, ink);
        } else {
            paint::stroke_polyline(&[at(0.24, 0.52), at(0.42, 0.7), at(0.76, 0.3)], side / 8.0, ink);
        }
    } else {
        let wash = if s.pressed { p.button_pressed } else { p.button };
        paint::fill_round_rect(square, radius, state_color(wash, s));
        paint::stroke_round_rect(square, radius, 1.5, state_color(p.outline, s));
    }
}

/// A radio button: a circle, filled with the accent and a dot when on.
pub(crate) fn radio(p: &Palette, r: NSRect, s: State) {
    let side = r.size.width.min(r.size.height);
    let circle = centered_square(r, side);
    if s.on {
        let fill = if s.pressed { darker(p.accent) } else { p.accent };
        paint::fill_ellipse(circle, state_color(fill, s));
        paint::fill_ellipse(centered_square(circle, (side * 0.375).round()), state_color(p.accent_text_on, s));
    } else {
        let wash = if s.pressed { p.button_pressed } else { p.button };
        paint::fill_ellipse(circle, state_color(wash, s));
        paint::stroke_ellipse(circle, 1.5, state_color(p.outline, s));
    }
}

/// A switch: a pill track, the accent when on, with a round knob at its
/// end; `position` runs from 0 (off) to 1 (on), for a knob being dragged.
pub(crate) fn switch(p: &Palette, r: NSRect, position: f64, s: State) {
    let h = r.size.height.min(r.size.width / 1.6);
    let w = (h * 1.75).min(r.size.width);
    let c = center(r);
    let track = NSRect::new(NSPoint::new(c.x - w / 2.0, c.y - h / 2.0), NSSize::new(w, h));
    let on = position >= 0.5;
    let fill = if on { p.accent } else { p.trough };
    paint::fill_round_rect(track, radii(h / 2.0), state_color(fill, s));
    let knob = h - 6.0;
    let x = track.origin.x + 3.0 + position.clamp(0.0, 1.0) * (w - 6.0 - knob);
    let k = NSRect::new(NSPoint::new(x, track.origin.y + 3.0), NSSize::new(knob, knob));
    paint::fill_ellipse(k, state_color(if s.pressed { darker(p.knob) } else { p.knob }, s));
}

/// A disclosure triangle: a chevron pointing right, or down when open.
pub(crate) fn disclosure(p: &Palette, r: NSRect, axis: Axis, open: bool, s: State) {
    let side = r.size.width.min(r.size.height);
    let sq = centered_square(r, side);
    let c = center(sq);
    let d = side * 0.2;
    let ink = state_color(if s.pressed { p.secondary_label } else { p.label }, s);
    let pts = if open {
        [
            NSPoint::new(c.x - 1.6 * d, axis.down(c.y, -0.8 * d)),
            NSPoint::new(c.x, axis.down(c.y, 0.8 * d)),
            NSPoint::new(c.x + 1.6 * d, axis.down(c.y, -0.8 * d)),
        ]
    } else {
        [
            NSPoint::new(c.x - 0.8 * d, axis.down(c.y, -1.6 * d)),
            NSPoint::new(c.x + 0.8 * d, c.y),
            NSPoint::new(c.x - 0.8 * d, axis.down(c.y, 1.6 * d)),
        ]
    };
    paint::stroke_polyline(&pts, 1.5, ink);
}

/// A downward (or upward) chevron centered in `r`, `size` points across,
/// for pop-up buttons, steppers and push-disclosure buttons.
pub(crate) fn chevron(r: NSRect, axis: Axis, size: f64, up: bool, color: Color) {
    let c = center(r);
    let (dx, dy) = (size / 2.0, size / 4.0);
    let dy = if up { -dy } else { dy };
    let pts = [
        NSPoint::new(c.x - dx, axis.down(c.y, -dy)),
        NSPoint::new(c.x, axis.down(c.y, dy)),
        NSPoint::new(c.x + dx, axis.down(c.y, -dy)),
    ];
    paint::stroke_polyline(&pts, 1.5, color);
}

/// A help button: a circle with a question mark, drawn as a hook and a
/// dot.
pub(crate) fn help(p: &Palette, r: NSRect, axis: Axis, s: State) {
    let side = r.size.width.min(r.size.height);
    let circle = centered_square(r, side);
    let wash = if s.pressed { p.button_pressed } else { p.button };
    paint::fill_ellipse(circle, state_color(wash, s));
    let c = center(circle);
    let u = side / 24.0;
    let ink = state_color(p.label, s);
    // The hook: an arc over the top, then down to the middle.
    let top = NSPoint::new(c.x, axis.down(c.y, -2.5 * u));
    paint::stroke_arc(top, 3.5 * u, -PI / 2.0 - 0.3, PI + 0.9, 2.0 * u, ink);
    paint::stroke_polyline(
        &[
            NSPoint::new(c.x + 1.6 * u, axis.down(c.y, 0.3 * u)),
            NSPoint::new(c.x, axis.down(c.y, 1.8 * u)),
            NSPoint::new(c.x, axis.down(c.y, 3.0 * u)),
        ],
        2.0 * u,
        ink,
    );
    paint::fill_ellipse(
        centered_square(
            NSRect::new(NSPoint::new(c.x - 1.3 * u, axis.down(c.y, 5.8 * u) - 1.3 * u), NSSize::new(2.6 * u, 2.6 * u)),
            2.6 * u,
        ),
        ink,
    );
}

// Fields and boxes

/// A text field's bezel: a rounded wash, with the view's background under
/// it when the field draws one.
pub(crate) fn entry(p: &Palette, r: NSRect, background: Option<Color>, s: State) {
    if let Some(bg) = background {
        paint::fill_round_rect(r, radii(RADIUS), bg);
    }
    paint::fill_round_rect(r, radii(RADIUS), state_color(p.entry, s));
}

/// A bordered (not bezeled) field's square line.
pub(crate) fn plain_border(p: &Palette, r: NSRect, s: State) {
    paint::stroke_round_rect(r, radii(0.0), 1.0, state_color(p.outline, s));
}

/// The frames an image view draws round its image (`NSImageFrameStyle`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageFrame {
    /// A print on paper: the view color, a hairline edge and a shadow
    /// below and to the right, where AppKit's frame is two points thick.
    Photo,
    /// A rounded wash.
    GrayBezel,
    /// A line cut into the surface: a dark edge with a light one inside.
    Groove,
    /// A button's bezel.
    Button,
}

/// An image view's frame filling `r`.
pub(crate) fn image_frame(p: &Palette, r: NSRect, frame: ImageFrame, axis: Axis, s: State) {
    match frame {
        ImageFrame::Photo => {
            // The shadow along the bottom and right edges, then the print,
            // a point in from them.
            paint::fill_rect(r, state_color(p.card_border, s));
            let top = axis.top(r);
            let print = NSRect::new(
                NSPoint::new(r.origin.x, if axis.flipped { top } else { top - (r.size.height - 1.0) }),
                NSSize::new(r.size.width - 1.0, r.size.height - 1.0),
            );
            paint::fill_rect(print, p.view);
            paint::stroke_round_rect(print, radii(0.0), 1.0, state_color(p.card_border, s));
        }
        ImageFrame::GrayBezel => {
            paint::fill_round_rect(inset(r, 1.0, 1.0), radii(RADIUS + 2.0), state_color(p.button, s));
        }
        ImageFrame::Groove => {
            paint::stroke_round_rect(inset(r, 1.0, 1.0), radii(RADIUS / 2.0), 1.0, state_color(p.knob, s));
            paint::stroke_round_rect(r, radii(RADIUS / 2.0), 1.0, state_color(p.separator, s));
        }
        ImageFrame::Button => button_bezel(p, r, radii(RADIUS), Emphasis::Normal, s),
    }
}

/// A card: a filled rounded rectangle with a hairline edge.
pub(crate) fn card(p: &Palette, r: NSRect) {
    paint::fill_round_rect(r, radii(RADIUS * 2.0), p.card);
    paint::stroke_round_rect(r, radii(RADIUS * 2.0), 1.0, p.card_border);
}

/// A separator line along the long side of `r`, one point thick.
pub(crate) fn separator(p: &Palette, r: NSRect) {
    let line = if r.size.width >= r.size.height {
        NSRect::new(
            NSPoint::new(r.origin.x, (r.origin.y + r.size.height / 2.0 - 0.5).floor()),
            NSSize::new(r.size.width, 1.0),
        )
    } else {
        NSRect::new(
            NSPoint::new((r.origin.x + r.size.width / 2.0 - 0.5).floor(), r.origin.y),
            NSSize::new(1.0, r.size.height),
        )
    };
    paint::fill_rect(line, p.separator);
}

// Indicators

/// A progress bar: a pill trough, filled with the accent from the left for
/// `fraction` (0 to 1).
pub(crate) fn progress_bar(p: &Palette, r: NSRect, fraction: f64, s: State) {
    let bar = bar_rect(r);
    let radius = radii(bar.size.height / 2.0);
    paint::fill_round_rect(bar, radius, state_color(p.trough, s));
    let w = (bar.size.width * fraction.clamp(0.0, 1.0)).max(if fraction > 0.0 { bar.size.height } else { 0.0 });
    if w > 0.0 {
        paint::fill_round_rect(
            NSRect::new(bar.origin, NSSize::new(w, bar.size.height)),
            radius,
            state_color(p.accent, s),
        );
    }
}

/// An indeterminate bar: the trough with a pulse sliding across it,
/// `phase` running from 0 to 1 once a cycle.
pub(crate) fn progress_pulse(p: &Palette, r: NSRect, phase: f64, s: State) {
    let bar = bar_rect(r);
    let radius = radii(bar.size.height / 2.0);
    paint::fill_round_rect(bar, radius, state_color(p.trough, s));
    let pulse = (bar.size.width * 0.3).max(bar.size.height);
    // Back and forth, easing at the ends.
    let t = (1.0 - (phase * TAU).cos()) / 2.0;
    let x = bar.origin.x + t * (bar.size.width - pulse);
    paint::fill_round_rect(
        NSRect::new(NSPoint::new(x, bar.origin.y), NSSize::new(pulse, bar.size.height)),
        radius,
        state_color(p.accent, s),
    );
}

/// The thin bar a progress indicator draws in its frame: 6 points (4 for
/// small ones), centered.
fn bar_rect(r: NSRect) -> NSRect {
    let h = if r.size.height >= 16.0 { 6.0 } else { 4.0 };
    NSRect::new(
        NSPoint::new(r.origin.x, r.origin.y + ((r.size.height - h) / 2.0).round()),
        NSSize::new(r.size.width, h),
    )
}

/// A spinner: an arc turning once a second, over a faint ring; `phase`
/// runs from 0 to 1 once a turn.
pub(crate) fn spinner(p: &Palette, r: NSRect, phase: f64, s: State) {
    let side = r.size.width.min(r.size.height);
    let width = (side / 8.0).max(1.5);
    let radius = side / 2.0 - width / 2.0 - side * 0.06;
    let c = center(r);
    paint::stroke_ellipse(
        centered_square(r, 2.0 * (radius + width / 2.0)),
        width,
        state_color(faded(p.label, 0.15), s),
    );
    // The arc grows and shrinks as it turns.
    let sweep = PI * (0.6 + 0.5 * (1.0 - (phase * TAU * 2.0).cos()) / 2.0);
    paint::stroke_arc(c, radius, phase * TAU, sweep, width, state_color(p.label, s));
}

// Segments, steppers, sliders

/// A segmented control's trough: the whole control, a rounded wash.
pub(crate) fn segment_trough(p: &Palette, r: NSRect, s: State) {
    paint::fill_round_rect(r, radii(RADIUS), state_color(p.button, s));
}

/// One segment of a segmented control: raised (a card-like fill) when
/// selected, darker while pressed.
pub(crate) fn segment(p: &Palette, r: NSRect, radius: Radii, selected: bool, tint: Option<Color>, s: State) {
    if selected {
        let fill = tint.unwrap_or(p.accent);
        paint::fill_round_rect(
            inset(r, 2.0, 2.0),
            radius.map(|v| (v - 2.0).max(0.0)),
            state_color(if s.pressed { darker(fill) } else { fill }, s),
        );
    } else if s.pressed {
        paint::fill_round_rect(inset(r, 2.0, 2.0), radius.map(|v| (v - 2.0).max(0.0)), state_color(p.button, s));
    }
}

/// A separated segment: its own pill.
pub(crate) fn separated_segment(p: &Palette, r: NSRect, selected: bool, tint: Option<Color>, s: State) {
    let fill = if selected {
        tint.unwrap_or(p.accent)
    } else if s.pressed {
        p.button_pressed
    } else {
        p.button
    };
    paint::fill_round_rect(r, radii(RADIUS), state_color(fill, s));
}

/// A stepper: two stacked halves, each with a chevron; `pressed` says
/// which half is held (true: the upper one).
pub(crate) fn stepper(p: &Palette, r: NSRect, axis: Axis, pressed: Option<bool>, s: State) {
    let w = r.size.width.min(20.0);
    let body =
        NSRect::new(NSPoint::new(r.origin.x + (r.size.width - w) / 2.0, r.origin.y), NSSize::new(w, r.size.height));
    paint::fill_round_rect(body, radii(RADIUS), state_color(p.button, s));
    let half = body.size.height / 2.0;
    let top_half = NSRect::new(
        NSPoint::new(body.origin.x, if axis.flipped { body.origin.y } else { body.origin.y + half }),
        NSSize::new(w, half),
    );
    let bottom_half = NSRect::new(
        NSPoint::new(body.origin.x, if axis.flipped { body.origin.y + half } else { body.origin.y }),
        NSSize::new(w, half),
    );
    if let Some(upper) = pressed {
        let held = if upper { top_half } else { bottom_half };
        let radius = if upper { [RADIUS, RADIUS, 0.0, 0.0] } else { [0.0, 0.0, RADIUS, RADIUS] };
        paint::fill_round_rect(held, radius, state_color(p.button_pressed, s));
    }
    let ink = state_color(p.label, s);
    chevron(top_half, axis, 7.0, true, ink);
    chevron(bottom_half, axis, 7.0, false, ink);
}

/// A linear slider's track, filled with the accent up to `knob_center`
/// (a coordinate along the track), and its tick marks.
pub(crate) fn slider_track(
    p: &Palette,
    bar: NSRect,
    vertical: bool,
    filled_to: Option<f64>,
    fill: Option<Color>,
    s: State,
) {
    let r = radii(if vertical { bar.size.width } else { bar.size.height } / 2.0);
    paint::fill_round_rect(bar, r, state_color(p.trough, s));
    if let Some(to) = filled_to {
        let part = if vertical {
            NSRect::new(bar.origin, NSSize::new(bar.size.width, (to - bar.origin.y).max(0.0)))
        } else {
            NSRect::new(bar.origin, NSSize::new((to - bar.origin.x).max(0.0), bar.size.height))
        };
        paint::fill_round_rect(part, r, state_color(fill.unwrap_or(p.accent), s));
    }
}

/// A tick mark, on the track's accent fill or off it.
pub(crate) fn tick(p: &Palette, r: NSRect, on_fill: bool, s: State) {
    let color = if on_fill { faded(p.accent_text_on, 0.8) } else { p.secondary_label };
    paint::fill_rect(r, state_color(color, s));
}

/// A slider's round knob in `r`.
pub(crate) fn knob(p: &Palette, r: NSRect, s: State) {
    let side = r.size.width.min(r.size.height);
    let circle = centered_square(r, side);
    paint::fill_ellipse(circle, state_color(p.knob_border, s));
    paint::fill_ellipse(inset(circle, 1.0, 1.0), state_color(if s.pressed { darker(p.knob) } else { p.knob }, s));
}

// Focus

/// The focus ring round `outline`: the accent at half strength, 2 points
/// wide, just outside.
pub(crate) fn focus_ring(p: &Palette, outline: Outline) {
    match outline {
        Outline::RoundRect(r, radius) => paint::stroke_round_rect(
            inset(r, -2.0, -2.0),
            radius.map(|v| if v > 0.0 { v + 2.0 } else { 2.0 }),
            2.0,
            p.focus_ring,
        ),
        Outline::Ellipse(r) => paint::stroke_ellipse(inset(r, -2.0, -2.0), 2.0, p.focus_ring),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{Xf, begin_recording, end_recording, set_view};
    use crate::protocol::{Op, Rect};

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    fn record(f: impl FnOnce()) -> Vec<Op> {
        begin_recording();
        set_view(Xf::IDENTITY, Rect::new(0.0, 0.0, 200.0, 200.0));
        f();
        end_recording()
    }

    fn colors(ops: &[Op]) -> Vec<Color> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Fill { color, .. } | Op::FillPath { paint: crate::protocol::Paint::Solid(color), .. } => {
                    Some(*color)
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn disabled_parts_draw_at_half_strength() {
        let p = &super::super::palette::LIGHT;
        let on = State { on: true, ..State::default() };
        let enabled = colors(&record(|| check_box(p, rect(0.0, 0.0, 16.0, 16.0), Axis { flipped: true }, on)));
        let disabled = colors(&record(|| {
            check_box(p, rect(0.0, 0.0, 16.0, 16.0), Axis { flipped: true }, State { disabled: true, ..on })
        }));
        assert_eq!(enabled.len(), disabled.len());
        for (a, b) in enabled.iter().zip(&disabled) {
            assert_eq!(a[3] * 0.5, b[3]);
        }
        // Checked boxes are the accent.
        assert_eq!(enabled[0], p.accent);
    }

    #[test]
    fn default_and_destructive_buttons_stand_out() {
        let p = &super::super::palette::DARK;
        let fill =
            |e| colors(&record(|| button_bezel(p, rect(0.0, 0.0, 60.0, 24.0), radii(RADIUS), e, State::default())))[0];
        assert_eq!(fill(Emphasis::Default), p.accent);
        assert_eq!(fill(Emphasis::Destructive), p.destructive);
        assert_eq!(fill(Emphasis::Normal), p.button);
    }

    #[test]
    fn focus_rings_sit_outside_the_part() {
        let p = &super::super::palette::LIGHT;
        let ops = record(|| focus_ring(p, Outline::RoundRect(rect(10.0, 10.0, 40.0, 20.0), radii(RADIUS))));
        let Some(Op::FillPath { path, paint: crate::protocol::Paint::Solid(color), .. }) = ops.first() else {
            panic!("a path")
        };
        assert_eq!(*color, p.focus_ring);
        let bounds = path.bounds();
        assert_eq!((bounds.left(), bounds.right()), (8.0, 52.0));
    }
}
