//! Apple's control geometry: the sizes and insets a program can observe,
//! measured on macOS by the conformance tests named beside each. Sizes
//! that depend on text are these constants plus the text's own measured
//! size, so they hold with whatever fonts a system has.
//!
//! Arrays indexed by control size run regular, small, mini, large
//! ([`size_index`]).

use objc2_app_kit::NSControlSize;

/// An index into the per-control-size arrays: regular, small, mini,
/// large (extra large counts as large).
pub(crate) fn size_index(size: NSControlSize) -> usize {
    match size {
        NSControlSize::Small => 1,
        NSControlSize::Mini => 2,
        NSControlSize::Large | NSControlSize::ExtraLarge => 3,
        _ => 0,
    }
}

/// What a cell with no content measures (`controls.rs`, `cell_defaults`).
pub(crate) const UNBOUNDED_CELL: f64 = 40000.0;

/// Text cells pad their text by this much on each side, left and right
/// (`controls.rs`, `text_cells`).
pub(crate) const TEXT_PADDING: f64 = 2.0;
/// A bordered text cell's inset all round (`text_cells`).
pub(crate) const BORDER_INSET: f64 = 2.0;
/// A bezeled text cell's inset all round (`text_cells`).
pub(crate) const BEZEL_INSET: f64 = 3.0;

/// The system font size for each control size.
pub(crate) const FONT_SIZE: [f64; 4] = [13.0, 11.0, 9.0, 13.0];

// Push buttons (`controls.rs`, `push_buttons`).

/// A push button's height, and the room it adds round its title.
pub(crate) const PUSH_HEIGHT: [f64; 4] = [24.0, 20.0, 16.0, 28.0];
/// The width a push button without a title gives its content.
pub(crate) const PUSH_EMPTY_CONTENT: f64 = 10.0;

// Check boxes and radio buttons (`controls.rs`, `check_boxes`).

/// The box (or circle) of a check box or radio button.
pub(crate) const CHECK_BOX: [f64; 4] = [16.0, 14.0, 12.0, 18.0];
/// Between the box and the title.
pub(crate) const CHECK_GAP: [f64; 4] = [6.0, 4.0, 4.0, 6.0];
/// The height the box is centered by: the box's own, except a mini box,
/// which sits a point lower (`check_box_rects`).
pub(crate) const CHECK_CENTER: [f64; 4] = [16.0, 14.0, 11.0, 18.0];

// Other bezels, from `buttons_of_every_bezel`, at every control size.

/// A disclosure triangle's square.
pub(crate) const DISCLOSURE: f64 = 13.0;
/// A help button's (and a push-disclosure button's) square.
pub(crate) const HELP: [f64; 4] = [24.0, 20.0, 16.0, 28.0];
/// Flexible push (and glass) and circular buttons keep their content this
/// far in from their top and bottom edges (a circle from its sides too),
/// and are their title's height plus twice this.
pub(crate) const ROUND_INSET: [f64; 4] = [4.0, 3.0, 1.0, 6.0];
/// A flexible or circular button without a title: its height, and a
/// circle's width.
pub(crate) const ROUND_EMPTY: [f64; 4] = [18.0, 16.0, 12.0, 22.0];
/// A textured square button: the room across its title (its drawing rect
/// is in by half that each side), and its height.
pub(crate) const TEXTURED_PAD: [f64; 4] = [8.0, 6.0, 6.0, 8.0];
pub(crate) const TEXTURED_HEIGHT: [f64; 4] = [20.0, 14.0, 11.0, 20.0];
/// A toolbar button: the room across its title, and its height.
pub(crate) const TOOLBAR_PAD: [f64; 4] = [14.0, 12.0, 10.0, 14.0];
pub(crate) const TOOLBAR_HEIGHT: [f64; 4] = [20.0, 16.0, 13.0, 20.0];
/// What these bezels' `cellSize` adds to their intrinsic size (their
/// frames reach past their alignment rects): textured, toolbar, small
/// square.
pub(crate) const TEXTURED_FRAME: (f64, f64) = (4.0, 5.0);
pub(crate) const TOOLBAR_FRAME: (f64, f64) = (2.0, 3.0);
pub(crate) const SMALL_SQUARE_FRAME: (f64, f64) = (0.0, 2.0);

// Text fields (`controls.rs`, `text_fields`).

/// A bezeled field's inset round its title rect, and the height it adds.
pub(crate) const FIELD_BEZEL_INSET: f64 = 4.0;
/// A bordered field adds this to the text's height.
pub(crate) const FIELD_BORDER_HEIGHT: f64 = 4.0;

// Boxes (`controls.rs`, `boxes`).

/// The default content view margins.
pub(crate) const BOX_MARGIN: f64 = 5.0;
/// Where a box's title starts from the left edge.
pub(crate) const BOX_TITLE_X: f64 = 7.0;
/// The title rect is the title's text plus this.
pub(crate) const BOX_TITLE_PAD: f64 = 8.0;
/// A custom box's line insets the content by this beyond the margins.
pub(crate) const BOX_CUSTOM_BORDER: f64 = 1.0;

// Indicators (`controls.rs`, `progress_indicators`).

/// A bar's height.
pub(crate) const BAR_HEIGHT: [f64; 4] = [20.0, 12.0, 12.0, 20.0];
/// A spinner's side.
pub(crate) const SPINNER: [f64; 4] = [32.0, 16.0, 10.0, 32.0];

// Segmented controls (`controls.rs`, `segmented_controls`).

pub(crate) const SEGMENT_HEIGHT: [f64; 4] = [24.0, 20.0, 16.0, 28.0];
/// The room a segment adds round its label (in the 13-point system font,
/// whatever the control size).
pub(crate) const SEGMENT_PADDING: [f64; 4] = [20.0, 18.0, 14.0, 24.0];
/// A segment with neither label nor width set.
pub(crate) const SEGMENT_EMPTY: f64 = 24.0;
/// Between two segments.
pub(crate) const SEGMENT_DIVIDER: f64 = 1.0;

// Steppers, sliders, switches (`controls.rs`, `steppers_sliders_switches`).

pub(crate) const STEPPER: (f64, f64) = (20.0, 26.0);
/// A stepper's autorepeat: the first repeat and the time between the rest.
pub(crate) const STEPPER_REPEAT: (f64, f64) = (0.5, 0.1);
/// A linear slider's thickness across its track.
pub(crate) const SLIDER: [f64; 4] = [16.0, 14.0, 12.0, 20.0];
/// A slider knob's length along the track.
pub(crate) const KNOB: [f64; 4] = [20.0, 18.0, 16.0, 24.0];
/// The bar a slider's knob runs along, across the track.
pub(crate) const SLIDER_BAR: f64 = 6.0;
pub(crate) const SWITCH: (f64, f64) = (54.0, 24.0);
