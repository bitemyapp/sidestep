//! `NSImageView` and `NSImageCell`: an image, scaled and aligned inside
//! an optional frame, and the cell that draws it.
//!
//! Geometry is AppKit's, measured by `conformance/tests/image_views.rs`:
//!
//! - The cell's `imageRectForBounds:` and `titleRectForBounds:` are the
//!   bounds; where the image goes is worked out as it draws. The frame
//!   style insets the drawing rect (`drawingRectForBounds:`): a photo
//!   frame 1 point at the top and left and 2 at the bottom and right (its
//!   shadow's side), a gray bezel 8 all round, a groove or button 2. An
//!   inset larger than the bounds collapses the rect to their middle.
//! - The image is scaled into the drawing rect (proportionally down, not
//!   at all, to fill it, or proportionally up or down), placed by the
//!   alignment, and its origin rounded to whole points, half-way values
//!   up; its size isn't rounded. A photo frame, groove and button are
//!   opaque. An image larger than the drawing rect and not scaled spills
//!   over the frame, clipped only by the view.
//! - The cell's own size is its frame's: nothing without one, 6 by 6
//!   points for a photo, 18 by 21 for a gray bezel, 4 by 4 for a groove or
//!   a button. The view's intrinsic size is the image's alignment rect
//!   without a frame (nothing without an image) and none at all with one,
//!   whose alignment rect is 3 points in on every side.
//! - An image cell holds only images: its type stays `NSNullCellType`,
//!   its object value is its image, and any other object value raises.
//!   It has no target, action or tag; the view keeps its own.
//!
//! Template images draw in a color, as AppKit tints them: the view's
//! `contentTintColor`, else the secondary label color; half as strong
//! (or the disabled text color) when disabled, and in the selected text
//! color on an emphasized background (a selected table row). Other images
//! draw at 40% when disabled.
//!
//! An image view takes images dropped on it when it's editable
//! (`isEditable`, enabled or not; it registers the image types for drags
//! whether it is or not, as AppKit does) and the drag's source allows a
//! copy, taking the image as the drop concludes; pasted, cut and deleted
//! too if it `allowsCutCopyPaste`. It sends its action when that changes
//! the image. It animates an animated image (a GIF's frames) if `animates`
//! says so, in a window or not, as AppKit does, until it drops the image,
//! stops animating or goes away.

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackgroundStyle, NSCell, NSCellType, NSColor, NSControl, NSDragOperation, NSDraggingInfo, NSEvent, NSImage,
    NSImageAlignment, NSImageCell, NSImageDynamicRange, NSImageFrameStyle, NSImageScaling, NSImageSymbolConfiguration,
    NSImageView, NSPasteboard, NSResponder, NSView,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSPoint, NSRect, NSSize, NSString, NSZone};

use super::cell::{self, NSCellImpl};
use super::control;
use crate::palette::System;
use crate::protocol::{Blend, Color};
use crate::theme::{self, parts};

// NSImageCell

pub(crate) struct ImageCellIvars {
    image: RefCell<Option<Retained<NSImage>>>,
    alignment: Cell<NSImageAlignment>,
    scaling: Cell<NSImageScaling>,
    frame_style: Cell<NSImageFrameStyle>,
}

impl ImageCellIvars {
    fn new(image: Option<&NSImage>) -> Self {
        ImageCellIvars {
            image: RefCell::new(image.map(|i| i.retain())),
            alignment: Cell::new(NSImageAlignment::AlignCenter),
            scaling: Cell::new(NSImageScaling::ScaleProportionallyDown),
            frame_style: Cell::new(NSImageFrameStyle::None),
        }
    }
}

define_class!(
    #[unsafe(super(NSCell, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSImageCell"]
    #[ivars = ImageCellIvars]
    pub(crate) struct NSImageCellImpl;

    impl NSImageCellImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ImageCellIvars::new(None));
            // SAFETY: NSCell's initializer: a cell with no content.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initImageCell:))]
        fn init_image_cell(this: Allocated<Self>, image: Option<&NSImage>) -> Retained<Self> {
            let this = this.set_ivars(ImageCellIvars::new(image));
            // SAFETY: as above; the image is kept here, and the cell's type
            // stays null, as AppKit's image cells report.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let this = this.set_ivars(ImageCellIvars::new(None));
            // SAFETY: as above.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            drop(this);
            // A string is an object value an image cell won't take.
            not_an_image(string)
        }

        #[unsafe(method_id(image))]
        fn image(&self) -> Option<Retained<NSImage>> {
            self.ivars().image.borrow().clone()
        }

        #[unsafe(method(setImage:))]
        fn set_image(&self, image: Option<&NSImage>) {
            let same = match (&*self.ivars().image.borrow(), image) {
                (Some(a), Some(b)) => std::ptr::eq(&**a, b),
                (None, None) => true,
                _ => false,
            };
            if same {
                return;
            }
            let old = self.ivars().image.replace(image.map(|i| i.retain()));
            drop(old);
            cell::changed(self.base());
        }

        #[unsafe(method_id(objectValue))]
        fn object_value(&self) -> Option<Retained<AnyObject>> {
            self.ivars().image.borrow().clone().map(Retained::into_super).map(Retained::into_super)
        }

        #[unsafe(method(setObjectValue:))]
        fn set_object_value(&self, object: Option<&AnyObject>) {
            let image = match object {
                None => None,
                Some(o) => match o.downcast_ref::<NSImage>() {
                    Some(image) => Some(image),
                    None => not_an_image(o),
                },
            };
            // SAFETY: setImage: takes an image or nil.
            let _: () = unsafe { msg_send![self, setImage: image] };
        }

        #[unsafe(method_id(stringValue))]
        fn string_value(&self) -> Retained<NSString> {
            // An image's description, as AppKit gives it.
            let image = self.ivars().image.borrow().clone();
            // SAFETY: description takes nothing and returns a string.
            image.map_or_else(NSString::new, |i| unsafe { msg_send![&*i, description] })
        }

        #[unsafe(method(setStringValue:))]
        fn set_string_value(&self, string: &NSString) {
            not_an_image(string);
        }

        #[unsafe(method(imageAlignment))]
        fn image_alignment(&self) -> NSImageAlignment {
            self.ivars().alignment.get()
        }

        #[unsafe(method(setImageAlignment:))]
        fn set_image_alignment(&self, alignment: NSImageAlignment) {
            if self.ivars().alignment.replace(alignment) != alignment {
                cell::redraw(self.base());
            }
        }

        #[unsafe(method(imageScaling))]
        fn image_scaling(&self) -> NSImageScaling {
            self.ivars().scaling.get()
        }

        #[unsafe(method(setImageScaling:))]
        fn set_image_scaling(&self, scaling: NSImageScaling) {
            if self.ivars().scaling.replace(scaling) != scaling {
                cell::redraw(self.base());
            }
        }

        #[unsafe(method(imageFrameStyle))]
        fn image_frame_style(&self) -> NSImageFrameStyle {
            self.ivars().frame_style.get()
        }

        #[unsafe(method(setImageFrameStyle:))]
        fn set_image_frame_style(&self, style: NSImageFrameStyle) {
            if self.ivars().frame_style.replace(style) != style {
                // The frame changes the cell's size and the view's.
                cell::changed(self.base());
            }
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            matches!(
                self.ivars().frame_style.get(),
                NSImageFrameStyle::Photo | NSImageFrameStyle::Groove | NSImageFrameStyle::Button
            )
        }

        // Geometry.

        #[unsafe(method(cellSize))]
        fn cell_size(&self) -> NSSize {
            frame_size(self.ivars().frame_style.get())
        }

        #[unsafe(method(cellSizeForBounds:))]
        fn cell_size_for_bounds(&self, _bounds: NSRect) -> NSSize {
            frame_size(self.ivars().frame_style.get())
        }

        #[unsafe(method(imageRectForBounds:))]
        fn image_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            bounds
        }

        #[unsafe(method(titleRectForBounds:))]
        fn title_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            bounds
        }

        #[unsafe(method(drawingRectForBounds:))]
        fn drawing_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            let flipped = self.base().view().is_some_and(|v| v.isFlipped());
            drawing_rect(bounds, self.ivars().frame_style.get(), flipped)
        }

        // Drawing.

        #[unsafe(method(drawWithFrame:inView:))]
        fn draw_with_frame(&self, frame: NSRect, view: &NSView) {
            if !theme::paint::recording() {
                return;
            }
            let style = self.ivars().frame_style.get();
            if style != NSImageFrameStyle::None {
                let state = parts::State { disabled: !self.base().has(cell::Flags::ENABLED), ..parts::State::default() };
                parts::image_frame(theme::palette(), frame, frame_kind(style), parts::Axis { flipped: view.isFlipped() }, state);
            }
            // SAFETY: drawInteriorWithFrame:inView: takes a rect and a view.
            let _: () = unsafe { msg_send![self, drawInteriorWithFrame: frame, inView: view] };
        }

        #[unsafe(method(drawInteriorWithFrame:inView:))]
        fn draw_interior_with_frame(&self, frame: NSRect, view: &NSView) {
            let Some(image) = shown_image(self, view) else { return };
            // SAFETY: drawingRectForBounds: takes and returns a rect.
            let area: NSRect = unsafe { msg_send![self, drawingRectForBounds: frame] };
            let (scaling, alignment) = (self.ivars().scaling.get(), self.ivars().alignment.get());
            let r = image_rect(image.size(), area, scaling, alignment, view.isFlipped());
            let enabled = self.base().has(cell::Flags::ENABLED);
            let style = as_ns_cell(self).backgroundStyle();
            draw_image(&image, r, template_tint(&image, style, content_tint(view).as_deref(), enabled), enabled, 0.4);
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, zone: *mut NSZone) -> Retained<NSCell> {
            // SAFETY: NSCell's copyWithZone: returns a cell of this class.
            let copy: Retained<NSCell> = unsafe { msg_send![super(self), copyWithZone: zone] };
            // SAFETY: the copy is an instance of the receiver's class.
            let theirs = unsafe { &*(Retained::as_ptr(&copy).cast::<NSImageCellImpl>()) };
            let ivars = theirs.ivars();
            ivars.image.replace(self.ivars().image.borrow().clone());
            ivars.alignment.set(self.ivars().alignment.get());
            ivars.scaling.set(self.ivars().scaling.get());
            ivars.frame_style.set(self.ivars().frame_style.get());
            copy
        }
    }

    unsafe impl NSObjectProtocol for NSImageCellImpl {}
);

impl NSImageCellImpl {
    fn base(&self) -> &NSCellImpl {
        cell::imp(as_ns_cell(self))
    }
}

fn as_ns_cell(cell: &NSImageCellImpl) -> &NSCell {
    // SAFETY: NSImageCell is a subclass of NSCell.
    unsafe { &*(cell as *const NSImageCellImpl).cast::<NSCell>() }
}

/// A cell as an image cell, if it is one.
pub(crate) fn as_image_cell(cell: &NSCell) -> Option<&NSImageCellImpl> {
    // SAFETY: NSImageCellImpl is the class NSImageCell names.
    unsafe { super::impl_of::<NSImageCell, NSImageCellImpl>(cell) }
}

/// An image cell's object value must be an image: raise as AppKit does.
fn not_an_image(object: &AnyObject) -> ! {
    let class = object.class().name().to_string_lossy();
    panic!("NSImageCell's object value must be an NSImage, not a \"{class}\".")
}

/// The size a frame style gives a cell (`image_views.rs`, `frame_styles`).
fn frame_size(style: NSImageFrameStyle) -> NSSize {
    match style {
        NSImageFrameStyle::Photo => NSSize::new(6.0, 6.0),
        NSImageFrameStyle::GrayBezel => NSSize::new(18.0, 21.0),
        NSImageFrameStyle::Groove | NSImageFrameStyle::Button => NSSize::new(4.0, 4.0),
        _ => NSSize::ZERO,
    }
}

/// A frame style's insets round the image: left, top, right and bottom as
/// seen, whichever way the view is flipped.
fn frame_insets(style: NSImageFrameStyle) -> [f64; 4] {
    match style {
        NSImageFrameStyle::Photo => [1.0, 1.0, 2.0, 2.0],
        NSImageFrameStyle::GrayBezel => [8.0; 4],
        NSImageFrameStyle::Groove | NSImageFrameStyle::Button => [2.0; 4],
        _ => [0.0; 4],
    }
}

fn frame_kind(style: NSImageFrameStyle) -> parts::ImageFrame {
    match style {
        NSImageFrameStyle::Photo => parts::ImageFrame::Photo,
        NSImageFrameStyle::GrayBezel => parts::ImageFrame::GrayBezel,
        NSImageFrameStyle::Groove => parts::ImageFrame::Groove,
        _ => parts::ImageFrame::Button,
    }
}

/// `drawingRectForBounds:`: the bounds less the frame's insets, collapsed
/// to their middle along an axis the insets don't fit.
pub(crate) fn drawing_rect(bounds: NSRect, style: NSImageFrameStyle, flipped: bool) -> NSRect {
    let [left, top, right, bottom] = frame_insets(style);
    let below = if flipped { top } else { bottom };
    let (mut x, mut w) = (bounds.origin.x + left, bounds.size.width - left - right);
    let (mut y, mut h) = (bounds.origin.y + below, bounds.size.height - top - bottom);
    if w < 0.0 {
        (x, w) = (bounds.origin.x + bounds.size.width / 2.0, 0.0);
    }
    if h < 0.0 {
        (y, h) = (bounds.origin.y + bounds.size.height / 2.0, 0.0);
    }
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// Where an image of `size` goes in `area`: scaled, aligned, its origin
/// rounded to whole points (half-way up).
pub(crate) fn image_rect(
    size: NSSize,
    area: NSRect,
    scaling: NSImageScaling,
    alignment: NSImageAlignment,
    flipped: bool,
) -> NSRect {
    use NSImageAlignment as A;
    let s = super::scaled_image(size, area.size, scaling);
    let across = match alignment {
        A::AlignLeft | A::AlignTopLeft | A::AlignBottomLeft => 0.0,
        A::AlignRight | A::AlignTopRight | A::AlignBottomRight => 1.0,
        _ => 0.5,
    };
    // How far up from the bottom, as seen.
    let up = match alignment {
        A::AlignTop | A::AlignTopLeft | A::AlignTopRight => 1.0,
        A::AlignBottom | A::AlignBottomLeft | A::AlignBottomRight => 0.0,
        _ => 0.5,
    };
    let down = if flipped { 1.0 - up } else { up };
    let x = area.origin.x + across * (area.size.width - s.width);
    let y = area.origin.y + down * (area.size.height - s.height);
    NSRect::new(NSPoint::new(super::half_up(x), super::half_up(y)), s)
}

/// The color a template image draws in, or none for other images: white
/// on an emphasized background, else the tint (the secondary label color
/// without one), fainter when disabled.
pub(crate) fn template_tint(
    image: &NSImage,
    style: NSBackgroundStyle,
    tint: Option<&NSColor>,
    enabled: bool,
) -> Option<Color> {
    if !image.isTemplate() {
        return None;
    }
    let look = crate::appearance::current_look();
    if style == NSBackgroundStyle::Emphasized {
        return Some(crate::palette::get(System::AlternateSelectedControlText, look));
    }
    Some(match tint {
        Some(c) if enabled => theme::color_of(c),
        Some(c) => theme::palette::faded(theme::color_of(c), 0.5),
        None if enabled => crate::palette::get(System::SecondaryLabel, look),
        None => crate::palette::get(System::DisabledControlText, look),
    })
}

/// Draw `image` into `r` (upright whichever way the view is flipped):
/// tinted when `tint` is given, else at `dimmed` strength when disabled.
pub(crate) fn draw_image(image: &NSImage, r: NSRect, tint: Option<Color>, enabled: bool, dimmed: f64) {
    if r.size.width <= 0.0 || r.size.height <= 0.0 {
        return;
    }
    let fraction = if tint.is_none() && !enabled { dimmed } else { 1.0 };
    if tint.is_some() || !overrides_drawing(image) {
        crate::image::imp(image).draw(r, NSRect::ZERO, Blend::SourceOver, fraction, true, tint);
    } else {
        // An image class of the program's own draws its own way.
        let op = objc2_app_kit::NSCompositingOperation::SourceOver;
        // SAFETY: the method takes two rects, an operation, a fraction, a
        // flag and optional hints.
        unsafe {
            image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(r, NSRect::ZERO, op, fraction, true, None)
        };
    }
}

/// Whether `image`'s class draws itself its own way.
fn overrides_drawing(image: &NSImage) -> bool {
    let sel = sel!(drawInRect:fromRect:operation:fraction:respectFlipped:hints:);
    let ours = <NSImage as ClassType>::class().instance_method(sel).map(|m| m.implementation() as usize);
    ours != image.class().instance_method(sel).map(|m| m.implementation() as usize)
}

/// The tint the view showing a cell gives its template images, if it
/// has `contentTintColor`.
fn content_tint(view: &NSView) -> Option<Retained<NSColor>> {
    if let Some(v) = as_image_view(view) {
        return v.ivars().tint.borrow().clone();
    }
    if view.respondsToSelector(sel!(contentTintColor)) {
        // SAFETY: contentTintColor takes nothing and returns a color or nil.
        return unsafe { msg_send![view, contentTintColor] };
    }
    None
}

/// The image a cell shows in `view`: its own, configured with the view's
/// symbol configuration.
fn shown_image(cell: &NSImageCellImpl, view: &NSView) -> Option<Retained<NSImage>> {
    let image = cell.ivars().image.borrow().clone()?;
    match as_image_view(view) {
        Some(v) if is_its_cell(v, cell) => Some(v.effective(&image)),
        _ => Some(image),
    }
}

fn is_its_cell(view: &NSImageViewImpl, cell: &NSImageCellImpl) -> bool {
    let control: &NSControl = view.as_view_control();
    control.cell().is_some_and(|c| std::ptr::eq(&*c, as_ns_cell(cell)))
}

// NSImageView

pub(crate) struct ImageViewIvars {
    target: RefCell<Weak<AnyObject>>,
    action: Cell<Option<Sel>>,
    animates: Cell<bool>,
    cut_copy_paste: Cell<bool>,
    tint: RefCell<Option<Retained<NSColor>>>,
    symbols: RefCell<Option<Retained<NSImageSymbolConfiguration>>>,
    dynamic_range: Cell<NSImageDynamicRange>,
    /// The image as shown: the cell's with the symbol configuration
    /// applied.
    configured: SymbolCache,
    /// An animated image's frames, while the view shows them.
    animation: RefCell<Option<super::image_animation::Animation>>,
}

impl Default for ImageViewIvars {
    fn default() -> Self {
        ImageViewIvars {
            target: RefCell::new(Weak::default()),
            action: Cell::new(None),
            animates: Cell::new(true),
            cut_copy_paste: Cell::new(true),
            tint: RefCell::new(None),
            symbols: RefCell::new(None),
            dynamic_range: Cell::new(NSImageDynamicRange::Unspecified),
            configured: SymbolCache::default(),
            animation: RefCell::new(None),
        }
    }
}

/// The pasteboard types image views take in drags, as AppKit registers
/// them (under their old names).
const DRAG_TYPES: [&str; 5] = [
    "NeXT TIFF v4.0 pasteboard type",
    "Apple PDF pasteboard type",
    "Apple PNG pasteboard type",
    "public.jpeg",
    "NSFilenamesPboardType",
];

define_class!(
    #[unsafe(super(NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSImageView"]
    #[ivars = ImageViewIvars]
    pub(crate) struct NSImageViewImpl;

    impl NSImageViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(ImageViewIvars::default());
            // SAFETY: NSControl's designated initializer, which makes the
            // cell.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            let control: &NSControl = this.as_view_control();
            if let Some(c) = control.cell() {
                c.setRefusesFirstResponder(true);
            }
            let kinds: Vec<Retained<NSString>> = DRAG_TYPES.iter().map(|t| NSString::from_str(t)).collect();
            let kinds = NSArray::from_retained_slice(&kinds);
            // SAFETY: the types are pasteboard types.
            let _: () = unsafe { msg_send![&*this, registerForDraggedTypes: &*kinds] };
            this
        }

        // The image and how it's shown, the cell's.

        #[unsafe(method_id(image))]
        fn image(&self) -> Option<Retained<NSImage>> {
            // SAFETY: an image cell's image is an image.
            self.the_cell().and_then(|c| unsafe { msg_send![&*c, image] })
        }

        #[unsafe(method(setImage:))]
        fn set_image(&self, image: Option<&NSImage>) {
            if let Some(c) = self.the_cell() {
                // SAFETY: setImage: takes an image or nil.
                let _: () = unsafe { msg_send![&*c, setImage: image] };
            }
            self.image_changed();
        }

        #[unsafe(method(imageAlignment))]
        fn image_alignment(&self) -> NSImageAlignment {
            self.with_image_cell(|c| c.ivars().alignment.get()).unwrap_or(NSImageAlignment::AlignCenter)
        }

        #[unsafe(method(setImageAlignment:))]
        fn set_image_alignment(&self, alignment: NSImageAlignment) {
            // SAFETY: the cell takes image settings (`forward` checks).
            self.forward(|c| unsafe { msg_send![c, setImageAlignment: alignment] });
        }

        #[unsafe(method(imageScaling))]
        fn image_scaling(&self) -> NSImageScaling {
            self.with_image_cell(|c| c.ivars().scaling.get()).unwrap_or(NSImageScaling::ScaleProportionallyDown)
        }

        #[unsafe(method(setImageScaling:))]
        fn set_image_scaling(&self, scaling: NSImageScaling) {
            // SAFETY: the cell takes image settings (`forward` checks).
            self.forward(|c| unsafe { msg_send![c, setImageScaling: scaling] });
        }

        #[unsafe(method(imageFrameStyle))]
        fn image_frame_style(&self) -> NSImageFrameStyle {
            self.with_image_cell(|c| c.ivars().frame_style.get()).unwrap_or(NSImageFrameStyle::None)
        }

        #[unsafe(method(setImageFrameStyle:))]
        fn set_image_frame_style(&self, style: NSImageFrameStyle) {
            // SAFETY: the cell takes image settings (`forward` checks).
            self.forward(|c| unsafe { msg_send![c, setImageFrameStyle: style] });
        }

        #[unsafe(method(isEditable))]
        fn is_editable(&self) -> bool {
            self.the_cell().is_some_and(|c| c.isEditable())
        }

        #[unsafe(method(setEditable:))]
        fn set_editable(&self, flag: bool) {
            if let Some(c) = self.the_cell() {
                c.setEditable(flag);
            }
        }

        #[unsafe(method(animates))]
        fn animates(&self) -> bool {
            self.ivars().animates.get()
        }

        #[unsafe(method(setAnimates:))]
        fn set_animates(&self, flag: bool) {
            if self.ivars().animates.replace(flag) != flag {
                self.image_changed();
            }
        }

        #[unsafe(method(allowsCutCopyPaste))]
        fn allows_cut_copy_paste(&self) -> bool {
            self.ivars().cut_copy_paste.get()
        }

        #[unsafe(method(setAllowsCutCopyPaste:))]
        fn set_allows_cut_copy_paste(&self, flag: bool) {
            self.ivars().cut_copy_paste.set(flag);
        }

        #[unsafe(method_id(contentTintColor))]
        fn content_tint_color(&self) -> Option<Retained<NSColor>> {
            self.ivars().tint.borrow().clone()
        }

        #[unsafe(method(setContentTintColor:))]
        fn set_content_tint_color(&self, color: Option<&NSColor>) {
            let old = self.ivars().tint.replace(color.map(|c| c.retain()));
            if !super::button::same_color(old.as_deref(), color) {
                self.as_view().setNeedsDisplay(true);
            }
        }

        #[unsafe(method_id(symbolConfiguration))]
        fn symbol_configuration(&self) -> Option<Retained<NSImageSymbolConfiguration>> {
            self.ivars().symbols.borrow().clone()
        }

        #[unsafe(method(setSymbolConfiguration:))]
        fn set_symbol_configuration(&self, config: Option<&NSImageSymbolConfiguration>) {
            self.ivars().symbols.replace(config.map(|c| c.retain()));
            self.ivars().configured.clear();
            self.image_changed();
            self.as_view_control().invalidateIntrinsicContentSize();
        }

        #[unsafe(method(preferredImageDynamicRange))]
        fn preferred_image_dynamic_range(&self) -> NSImageDynamicRange {
            self.ivars().dynamic_range.get()
        }

        #[unsafe(method(setPreferredImageDynamicRange:))]
        fn set_preferred_image_dynamic_range(&self, range: NSImageDynamicRange) {
            self.ivars().dynamic_range.set(range);
        }

        // Every image draws in the standard range here.
        #[unsafe(method(imageDynamicRange))]
        fn image_dynamic_range(&self) -> NSImageDynamicRange {
            NSImageDynamicRange::Standard
        }

        #[unsafe(method(defaultPreferredImageDynamicRange))]
        fn default_preferred_image_dynamic_range() -> NSImageDynamicRange {
            NSImageDynamicRange::Unspecified
        }

        #[unsafe(method(setDefaultPreferredImageDynamicRange:))]
        fn set_default_preferred_image_dynamic_range(_range: NSImageDynamicRange) {}

        // Target and action: an image cell has neither, so the view keeps
        // them.

        #[unsafe(method_id(target))]
        fn target(&self) -> Option<Retained<AnyObject>> {
            self.ivars().target.borrow().load()
        }

        #[unsafe(method(setTarget:))]
        fn set_target(&self, target: Option<&AnyObject>) {
            self.ivars().target.replace(target.map_or_else(Weak::default, Weak::new));
        }

        #[unsafe(method(action))]
        fn action(&self) -> Option<Sel> {
            self.ivars().action.get()
        }

        #[unsafe(method(setAction:))]
        fn set_action(&self, action: Option<Sel>) {
            self.ivars().action.set(action);
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            self.the_cell().is_some_and(|c| c.isOpaque())
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            let control = self.as_view_control();
            control.isEditable() && control.isEnabled()
        }

        // Sizing.

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            control::cached_intrinsic(self.as_view_control(), || self.natural_size())
        }

        #[unsafe(method(sizeThatFits:))]
        fn size_that_fits(&self, size: NSSize) -> NSSize {
            let natural = self.natural_size();
            let axis = |v: f64, proposed: f64| if v == control::NO_METRIC { proposed } else { v };
            NSSize::new(axis(natural.width, size.width), axis(natural.height, size.height))
        }

        #[unsafe(method(alignmentRectInsets))]
        fn alignment_rect_insets(&self) -> NSEdgeInsets {
            self.insets()
        }

        // An image view doesn't track the mouse: a click goes on up the
        // responder chain (dragging an editable view's image out isn't
        // done).
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            // SAFETY: NSResponder's mouseDown: (not NSControl's, which
            // tracks) passes the event on; the receiver is an NSResponder.
            let _: () = unsafe { msg_send![super(self, <NSResponder as ClassType>::class()), mouseDown: event] };
        }

        // Editing: the keyboard, the edit menu and drags (`image_views.rs`,
        // `editing`, `edit_menu`, `drags`). Being editable is what counts,
        // enabled or not (only taking the keyboard needs both); deleting,
        // as cutting and pasting, needs `allowsCutCopyPaste` too.

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let deletes = event.charactersIgnoringModifiers().is_some_and(|c| {
                c.length() == 1 && matches!(c.characterAtIndex(0), 0x7F | 0x08 | 0xF728)
            });
            if deletes && self.edits() {
                self.clear_image();
            } else {
                // SAFETY: NSResponder's keyDown: passes the key on.
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }

        #[unsafe(method(delete:))]
        fn delete(&self, _sender: Option<&AnyObject>) {
            if self.edits() {
                self.clear_image();
            }
        }

        #[unsafe(method(cut:))]
        fn cut(&self, _sender: Option<&AnyObject>) {
            if self.edits() {
                self.copy_image();
                self.clear_image();
            }
        }

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            if self.ivars().cut_copy_paste.get() {
                self.copy_image();
            }
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            if !self.edits() {
                return;
            }
            if let Some(image) = crate::image::from_pasteboard(&NSPasteboard::generalPasteboard()) {
                self.take_image(Some(&image));
            }
        }

        #[unsafe(method(validateUserInterfaceItem:))]
        fn validate_user_interface_item(&self, item: &AnyObject) -> bool {
            // SAFETY: validated items answer action.
            let action: Option<Sel> = unsafe { msg_send![item, action] };
            self.validates(action)
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &AnyObject) -> bool {
            // SAFETY: menu items answer action.
            let action: Option<Sel> = unsafe { msg_send![item, action] };
            self.validates(action)
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            self.drag_operation(sender)
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            self.drag_operation(sender)
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {}

        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag_operation(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            true
        }

        // A drop succeeds if the source allows a copy, whatever the drag
        // holds; once it has, the view takes the drag's image, if it has
        // one. (A drag session gets here only after `draggingEntered:` took
        // the drag, so only while editable; called directly, AppKit's view
        // takes the image whatever it and the source allow.)
        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            copies(sender)
        }

        #[unsafe(method(concludeDragOperation:))]
        fn conclude_drag_operation(&self, sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
            let image = sender.and_then(|s| crate::image::from_pasteboard(&s.draggingPasteboard()));
            if let Some(image) = image {
                self.take_image(Some(&image));
            }
        }
    }

    unsafe impl NSObjectProtocol for NSImageViewImpl {}
);

impl NSImageViewImpl {
    fn as_view(&self) -> &NSView {
        // SAFETY: NSImageView is a subclass of NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }

    fn as_view_control(&self) -> &NSImageView {
        // SAFETY: NSImageViewImpl is the class NSImageView names.
        unsafe { &*(self as *const Self).cast::<NSImageView>() }
    }

    fn the_cell(&self) -> Option<Retained<NSCell>> {
        let control: &NSControl = self.as_view_control();
        control.cell()
    }

    /// Run `f` on the cell, if it's an image cell, holding it meanwhile.
    fn with_image_cell<R>(&self, f: impl FnOnce(&NSImageCellImpl) -> R) -> Option<R> {
        let cell = self.the_cell()?;
        as_image_cell(&cell).map(f)
    }

    /// Send `f` to the cell, if it takes image settings.
    fn forward(&self, f: impl FnOnce(&NSCell)) {
        if let Some(c) = self.the_cell()
            && c.respondsToSelector(sel!(setImageScaling:))
        {
            f(&c);
        }
    }

    /// `image` as shown: with the symbol configuration applied.
    fn effective(&self, image: &NSImage) -> Retained<NSImage> {
        let config = self.ivars().symbols.borrow().clone();
        self.ivars().configured.shown(image, config.as_deref())
    }

    /// The image as shown, if there is one.
    fn shown(&self) -> Option<Retained<NSImage>> {
        let image = self.as_view_control().image()?;
        Some(self.effective(&image))
    }

    /// The intrinsic size: the shown image's alignment rect without a
    /// frame, none with one.
    fn natural_size(&self) -> NSSize {
        if self.as_view_control().imageFrameStyle() != NSImageFrameStyle::None {
            return NSSize::new(control::NO_METRIC, control::NO_METRIC);
        }
        self.shown().map_or(NSSize::ZERO, |i| i.alignmentRect().size)
    }

    /// The alignment rect's insets: 3 points with a frame, else the
    /// image's own.
    fn insets(&self) -> NSEdgeInsets {
        let zero = NSEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 };
        if self.as_view_control().imageFrameStyle() != NSImageFrameStyle::None {
            return NSEdgeInsets { top: 3.0, left: 3.0, bottom: 3.0, right: 3.0 };
        }
        let Some(image) = self.shown() else { return zero };
        let (size, a) = (image.size(), image.alignmentRect());
        NSEdgeInsets {
            top: size.height - (a.origin.y + a.size.height),
            left: a.origin.x,
            bottom: a.origin.y,
            right: size.width - (a.origin.x + a.size.width),
        }
    }

    /// Put the image on the general pasteboard.
    fn copy_image(&self) {
        let Some(image) = self.as_view_control().image() else { return };
        let board = NSPasteboard::generalPasteboard();
        board.clearContents();
        let objects = NSArray::from_retained_slice(&[Retained::into_super(Retained::into_super(image))]);
        // SAFETY: images write themselves to pasteboards.
        let _: bool = unsafe { msg_send![&*board, writeObjects: &*objects] };
    }

    fn editable(&self) -> bool {
        self.as_view_control().isEditable()
    }

    /// Whether the user may cut, paste or delete the image.
    fn edits(&self) -> bool {
        self.editable() && self.ivars().cut_copy_paste.get()
    }

    /// The image changed, or whether it animates: forget what was made for
    /// the old one, start or stop animating, redraw.
    fn image_changed(&self) {
        let image = self.as_view_control().image();
        self.ivars().configured.keep_only(image.as_deref());
        let animation = match image.as_deref() {
            Some(image) if self.ivars().animates.get() => {
                super::image_animation::Animation::follow(self.ivars().animation.take(), image, self.as_view())
            }
            _ => None,
        };
        let old = self.ivars().animation.replace(animation);
        drop(old);
        self.as_view().setNeedsDisplay(true);
    }

    /// Take `image` as the user's edit: show it and send the action.
    fn take_image(&self, image: Option<&NSImage>) {
        let control = self.as_view_control();
        control.setImage(image);
        let target = self.ivars().target.borrow().load();
        control::send_action(self.as_view(), self.ivars().action.get(), target.as_deref());
    }

    /// Delete the image as the user's edit; with none, nothing happens.
    fn clear_image(&self) {
        if self.as_view_control().image().is_some() {
            self.take_image(None);
        }
    }

    /// Whether the edit menu's `action` applies (`image_views.rs`,
    /// `edit_menu`); actions it doesn't know are left enabled.
    fn validates(&self, action: Option<Sel>) -> bool {
        let Some(action) = action else { return false };
        let has_image = self.as_view_control().image().is_some();
        let ccp = self.ivars().cut_copy_paste.get();
        if action == sel!(copy:) {
            has_image && ccp
        } else if action == sel!(cut:) || action == sel!(delete:) {
            has_image && self.edits()
        } else if action == sel!(paste:) {
            self.edits() && crate::image::pasteboard_has_image(&NSPasteboard::generalPasteboard())
        } else {
            true
        }
    }

    /// What a drag over the view may do: copy its image in, when the view
    /// is editable, the drag has an image and its source allows a copy
    /// (a drag from the view itself too).
    fn drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
        if !self.editable() || !copies(sender) || !crate::image::pasteboard_has_image(&sender.draggingPasteboard()) {
            return NSDragOperation::None;
        }
        NSDragOperation::Copy
    }

    /// Run `f` on the view's animation, if it has one.
    pub(crate) fn with_animation<R>(&self, f: impl FnOnce(&mut super::image_animation::Animation) -> R) -> Option<R> {
        self.ivars().animation.borrow_mut().as_mut().map(f)
    }
}

/// Whether a drag's source allows a copy (or leaves it to the
/// destination).
fn copies(sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
    sender.draggingSourceOperationMask().intersects(NSDragOperation::Copy | NSDragOperation::Generic)
}

/// `+imageViewWithImage:`: a view of the class it's sent to (a subclass
/// makes one of its own), with no frame, showing `image`. A
/// `define_class!` class method doesn't see its receiver, so it goes in by
/// hand, as a category.
unsafe extern "C-unwind" fn image_view_with_image(class: &AnyClass, _cmd: Sel, image: &NSImage) -> *mut NSImageView {
    // SAFETY: the receiver is NSImageView or a subclass: alloc and
    // initWithFrame: make one of it.
    let view: Retained<NSImageView> = unsafe {
        let allocated: Allocated<NSImageView> = msg_send![class, alloc];
        msg_send![allocated, initWithFrame: NSRect::ZERO]
    };
    view.setImage(Some(image));
    Retained::autorelease_return(view)
}

sidestep_runtime::category!("NSImageView"(SidestepImageViewFactory), |category| {
    // SAFETY: the function takes the class, the selector and an image, and
    // returns an autoreleased image view, as a class method.
    unsafe {
        category.add_class_method(
            sel!(imageViewWithImage:),
            image_view_with_image as unsafe extern "C-unwind" fn(_, _, _) -> _,
        )
    };
});

/// A symbol image made with a symbol configuration, kept with the image
/// it was made of, so image views and buttons make it once per image and
/// configuration.
#[derive(Default)]
pub(crate) struct SymbolCache(RefCell<Option<(Retained<NSImage>, Retained<NSImage>)>>);

impl SymbolCache {
    /// `image` made with `config`, or as it is without one.
    pub(crate) fn shown(&self, image: &NSImage, config: Option<&NSImageSymbolConfiguration>) -> Retained<NSImage> {
        let Some(config) = config else { return image.retain() };
        if let Some((of, made)) = &*self.0.borrow()
            && std::ptr::eq(&**of, image)
        {
            return made.clone();
        }
        let made = image.imageWithSymbolConfiguration(config).unwrap_or_else(|| image.retain());
        let old = self.0.replace(Some((image.retain(), made.clone())));
        drop(old);
        made
    }

    /// Forget what was made (the configuration changed).
    pub(crate) fn clear(&self) {
        let old = self.0.take();
        drop(old);
    }

    /// Forget what was made of any image but `image`.
    pub(crate) fn keep_only(&self, image: Option<&NSImage>) {
        let stale = self.0.borrow().as_ref().is_some_and(|(of, _)| image.is_none_or(|i| !std::ptr::eq(&**of, i)));
        if stale {
            self.clear();
        }
    }
}

/// `view` as an image view, if it is one.
pub(crate) fn as_image_view(view: &NSView) -> Option<&NSImageViewImpl> {
    // SAFETY: NSImageViewImpl is the class NSImageView names.
    unsafe { super::impl_of::<NSImageView, NSImageViewImpl>(view) }
}

/// Whether a cell is an image cell showing images in a view: accessibility
/// gives such cells the image role.
pub(crate) fn is_image_cell(cell: &NSCell) -> bool {
    as_image_cell(cell).is_some() && cell.r#type() == NSCellType::NullCellType
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    #[test]
    fn frames_inset_the_image_as_seen() {
        let b = rect(0.0, 0.0, 100.0, 50.0);
        // The photo frame's shadow is at the bottom, whichever way y runs.
        assert_eq!(drawing_rect(b, NSImageFrameStyle::Photo, false), rect(1.0, 2.0, 97.0, 47.0));
        assert_eq!(drawing_rect(b, NSImageFrameStyle::Photo, true), rect(1.0, 1.0, 97.0, 47.0));
        // Insets the bounds can't hold collapse to their middle.
        assert_eq!(
            drawing_rect(rect(0.0, 0.0, 10.0, 6.0), NSImageFrameStyle::GrayBezel, false),
            rect(5.0, 3.0, 0.0, 0.0)
        );
    }

    #[test]
    fn images_land_by_alignment_either_way_up() {
        use NSImageAlignment as A;
        let area = rect(0.0, 0.0, 100.0, 50.0);
        let size = NSSize::new(21.0, 11.0);
        let place = |a, flipped| image_rect(size, area, NSImageScaling::ScaleProportionallyDown, a, flipped);
        // Half-way origins round up.
        assert_eq!(place(A::AlignCenter, false), rect(40.0, 20.0, 21.0, 11.0));
        // The top is up the screen: y's high end unflipped, 0 flipped.
        assert_eq!(place(A::AlignTopLeft, false), rect(0.0, 39.0, 21.0, 11.0));
        assert_eq!(place(A::AlignTopLeft, true), rect(0.0, 0.0, 21.0, 11.0));
        assert_eq!(place(A::AlignBottomRight, true), rect(79.0, 39.0, 21.0, 11.0));
        // Scaling up keeps the size unrounded.
        let up = image_rect(size, area, NSImageScaling::ScaleProportionallyUpOrDown, A::AlignCenter, false);
        assert_eq!((up.origin.x, up.size.height), (2.0, 50.0));
        assert!((up.size.width - 21.0 * 50.0 / 11.0).abs() < 1e-9);
    }
}
