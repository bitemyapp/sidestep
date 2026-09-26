//! `NSBox`: a titled border round a content view, a custom filled box, or
//! a separator line.
//!
//! The geometry is AppKit's (`conformance/tests/controls.rs`, `boxes`,
//! `custom_and_separator_boxes`, `box_sizing`), in the box's unflipped
//! coordinates, with the title `th` points tall (its font's line) and its
//! text plus 4 points each side wide, 7 points in from the left:
//!
//! - at or above the top, the title's top is the box's; the border ends
//!   `th - 2` points below it;
//! - below the top, the border is the bounds and the title hangs 2 points
//!   over the top;
//! - above the bottom, the title hangs 2 points under the bottom;
//! - at or below the bottom, the border starts `th - 2` points up;
//! - the content view sits inside the border by the margins (5 by
//!   default), and a custom box's line insets it one point more.
//!
//! A primary box draws as an Adwaita card with its title above; a custom
//! box draws exactly what it's given (fill, border, width, corner radius);
//! a separator is a line along its long side. A transparent box draws
//! nothing.

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSBorderType, NSBoxType, NSColor, NSFont, NSResponder, NSTitlePosition, NSView};
use objc2_foundation::{NSCopying, NSPoint, NSRect, NSSize, NSString};

use super::cell::Styled;
use crate::theme::{self, metrics, parts};
use crate::views;

pub(crate) struct BoxIvars {
    kind: Cell<NSBoxType>,
    position: Cell<NSTitlePosition>,
    title: RefCell<Retained<NSString>>,
    title_font: RefCell<Option<Retained<NSFont>>>,
    margins: Cell<NSSize>,
    content: RefCell<Option<Retained<NSView>>>,
    transparent: Cell<bool>,
    border_width: Cell<f64>,
    corner_radius: Cell<f64>,
    border_color: RefCell<Option<Retained<NSColor>>>,
    fill_color: RefCell<Option<Retained<NSColor>>>,
    border_type: Cell<NSBorderType>,
    title_cell: RefCell<Option<Retained<AnyObject>>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSBox"]
    #[ivars = BoxIvars]
    pub(crate) struct NSBoxImpl;

    impl NSBoxImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(BoxIvars {
                kind: Cell::new(NSBoxType::Primary),
                position: Cell::new(NSTitlePosition::AtTop),
                title: RefCell::new(NSString::from_str("Title")),
                title_font: RefCell::new(None),
                margins: Cell::new(NSSize::new(metrics::BOX_MARGIN, metrics::BOX_MARGIN)),
                content: RefCell::new(None),
                transparent: Cell::new(false),
                border_width: Cell::new(1.0),
                corner_radius: Cell::new(0.0),
                border_color: RefCell::new(None),
                fill_color: RefCell::new(None),
                border_type: Cell::new(NSBorderType::GrooveBorder),
                title_cell: RefCell::new(None),
            });
            // SAFETY: NSView's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            let content = NSView::initWithFrame(NSView::alloc(this.mtm()), NSRect::ZERO);
            put_content(&this, Some(&content));
            this
        }

        #[unsafe(method(boxType))]
        fn box_type(&self) -> NSBoxType {
            self.ivars().kind.get()
        }

        #[unsafe(method(setBoxType:))]
        fn set_box_type(&self, kind: NSBoxType) {
            if self.ivars().kind.replace(kind) == kind {
                return;
            }
            if kind == NSBoxType::Separator {
                // A separator has no content.
                put_content(self, None);
            } else if self.ivars().content.borrow().is_none() {
                let content = NSView::initWithFrame(NSView::alloc(self.mtm()), NSRect::ZERO);
                put_content(self, Some(&content));
            }
            self.changed();
        }

        #[unsafe(method(titlePosition))]
        fn title_position(&self) -> NSTitlePosition {
            self.ivars().position.get()
        }

        #[unsafe(method(setTitlePosition:))]
        fn set_title_position(&self, position: NSTitlePosition) {
            self.ivars().position.set(position);
            self.changed();
        }

        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> {
            self.ivars().title.borrow().clone()
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: &NSString) {
            self.ivars().title.replace(title.copy());
            self.changed();
        }

        #[unsafe(method(setTitleWithMnemonic:))]
        fn set_title_with_mnemonic(&self, title: Option<&NSString>) {
            let text = title.map(|t| t.to_string().replacen('&', "", 1)).unwrap_or_default();
            self.ivars().title.replace(NSString::from_str(&text));
            self.changed();
        }

        #[unsafe(method_id(titleFont))]
        fn title_font(&self) -> Retained<NSFont> {
            title_font(self)
        }

        #[unsafe(method(setTitleFont:))]
        fn set_title_font(&self, font: &NSFont) {
            self.ivars().title_font.replace(Some(font.retain()));
            self.changed();
        }

        #[unsafe(method_id(titleCell))]
        fn title_cell(&self) -> Retained<AnyObject> {
            title_cell(self)
        }

        #[unsafe(method(borderRect))]
        fn border_rect(&self) -> NSRect {
            layout(self).border
        }

        #[unsafe(method(titleRect))]
        fn title_rect(&self) -> NSRect {
            layout(self).title
        }

        #[unsafe(method(contentViewMargins))]
        fn content_view_margins(&self) -> NSSize {
            self.ivars().margins.get()
        }

        #[unsafe(method(setContentViewMargins:))]
        fn set_content_view_margins(&self, margins: NSSize) {
            self.ivars().margins.set(margins);
            self.changed();
        }

        #[unsafe(method_id(contentView))]
        fn content_view(&self) -> Option<Retained<NSView>> {
            self.ivars().content.borrow().clone()
        }

        #[unsafe(method(setContentView:))]
        fn set_content_view(&self, view: Option<&NSView>) {
            put_content(self, view);
            self.changed();
        }

        #[unsafe(method(isTransparent))]
        fn is_transparent(&self) -> bool {
            self.ivars().transparent.get()
        }

        #[unsafe(method(setTransparent:))]
        fn set_transparent(&self, flag: bool) {
            self.ivars().transparent.set(flag);
            self.redraw();
        }

        #[unsafe(method(borderWidth))]
        fn border_width(&self) -> f64 {
            self.ivars().border_width.get()
        }

        #[unsafe(method(setBorderWidth:))]
        fn set_border_width(&self, width: f64) {
            self.ivars().border_width.set(width);
            self.redraw();
        }

        #[unsafe(method(cornerRadius))]
        fn corner_radius(&self) -> f64 {
            self.ivars().corner_radius.get()
        }

        #[unsafe(method(setCornerRadius:))]
        fn set_corner_radius(&self, radius: f64) {
            self.ivars().corner_radius.set(radius);
            self.redraw();
        }

        #[unsafe(method_id(borderColor))]
        fn border_color(&self) -> Retained<NSColor> {
            let set = self.ivars().border_color.borrow().clone();
            set.unwrap_or_else(|| theme::system_color(sel!(secondaryLabelColor), |p| p.secondary_label))
        }

        #[unsafe(method(setBorderColor:))]
        fn set_border_color(&self, color: &NSColor) {
            self.ivars().border_color.replace(Some(color.retain()));
            self.redraw();
        }

        #[unsafe(method_id(fillColor))]
        fn fill_color(&self) -> Retained<NSColor> {
            let set = self.ivars().fill_color.borrow().clone();
            set.unwrap_or_else(|| theme::system_color(sel!(clearColor), |_| [0.0; 4]))
        }

        #[unsafe(method(setFillColor:))]
        fn set_fill_color(&self, color: &NSColor) {
            self.ivars().fill_color.replace(Some(color.retain()));
            self.redraw();
        }

        #[unsafe(method(borderType))]
        fn border_type(&self) -> NSBorderType {
            self.ivars().border_type.get()
        }

        #[unsafe(method(setBorderType:))]
        fn set_border_type(&self, kind: NSBorderType) {
            self.ivars().border_type.set(kind);
            self.changed();
        }

        #[unsafe(method(setFrameFromContentFrame:))]
        fn set_frame_from_content_frame(&self, content: NSRect) {
            let (left, bottom, right, top) = insets(self);
            let frame = NSRect::new(
                NSPoint::new(content.origin.x - left, content.origin.y - bottom),
                NSSize::new(content.size.width + left + right, content.size.height + bottom + top),
            );
            self.as_view().setFrame(frame);
            self.changed();
        }

        #[unsafe(method(sizeToFit))]
        fn size_to_fit(&self) {
            size_to_fit(self);
        }

        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize_subviews_with_old_size(&self, _old: NSSize) {
            place_content(self);
        }

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            let none = super::control::NO_METRIC;
            if self.ivars().kind.get() != NSBoxType::Separator {
                return NSSize::new(none, none);
            }
            let size = views::frame(views::imp(self.as_view())).size;
            if size.width >= size.height { NSSize::new(none, 1.0) } else { NSSize::new(1.0, none) }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw(self);
        }
    }

    unsafe impl NSObjectProtocol for NSBoxImpl {}
);

impl NSBoxImpl {
    fn as_view(&self) -> &NSView {
        // SAFETY: NSBox is a subclass of NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }

    fn mtm(&self) -> MainThreadMarker {
        MainThreadMarker::from(self)
    }

    /// Something the layout depends on changed: move the content, redraw.
    fn changed(&self) {
        place_content(self);
        self.redraw();
    }

    fn redraw(&self) {
        self.as_view().setNeedsDisplay(true);
    }
}

/// The cell showing the title, made when first asked for.
fn title_cell(b: &NSBoxImpl) -> Retained<AnyObject> {
    if let Some(cell) = b.ivars().title_cell.borrow().clone() {
        return cell;
    }
    // Made outside the borrow: making it sends messages.
    let title = b.ivars().title.borrow().clone();
    let cell = objc2_app_kit::NSTextFieldCell::initTextCell(objc2_app_kit::NSTextFieldCell::alloc(b.mtm()), &title);
    let cell: Retained<AnyObject> =
        Retained::into_super(Retained::into_super(Retained::into_super(Retained::into_super(cell))));
    b.ivars().title_cell.replace(Some(cell.clone()));
    cell
}

/// Where the parts of a box go, in its bounds.
struct Layout {
    border: NSRect,
    title: NSRect,
    content: NSRect,
}

fn title_font(b: &NSBoxImpl) -> Retained<NSFont> {
    let set = b.ivars().title_font.borrow().clone();
    set.unwrap_or_else(|| NSFont::systemFontOfSize(11.0))
}

/// The title's text, styled, in `color`.
fn styled_title(b: &NSBoxImpl, color: crate::protocol::Color) -> Styled {
    let font = title_font(b);
    let mut a = attrs_for(&font);
    a.color = color;
    Styled::plain(b.ivars().title.borrow().to_string(), a)
}

fn attrs_for(font: &NSFont) -> crate::text::layout::Attrs {
    let mut a = crate::text::layout::Attrs::new(crate::font::text_font(font));
    a.paragraph.line_break = crate::text::layout::LineBreak::Clip;
    a
}

/// The title's size: its text plus 4 points each side, its font's line
/// tall.
fn title_size(b: &NSBoxImpl) -> NSSize {
    let size = styled_title(b, [0.0; 4]).size(None);
    NSSize::new(size.width + metrics::BOX_TITLE_PAD, size.height)
}

/// Whether the box shows its title: primary boxes with a position.
fn titled(b: &NSBoxImpl) -> bool {
    b.ivars().kind.get() == NSBoxType::Primary && b.ivars().position.get() != NSTitlePosition::NoTitle
}

fn layout(b: &NSBoxImpl) -> Layout {
    let bounds = views::bounds(views::imp(b.as_view()));
    let (w, h) = (bounds.size.width, bounds.size.height);
    let m = b.ivars().margins.get();
    let custom_line =
        b.ivars().kind.get() == NSBoxType::Custom && b.ivars().border_type.get() != NSBorderType::NoBorder;
    let (mx, my) = if custom_line {
        (m.width + metrics::BOX_CUSTOM_BORDER, m.height + metrics::BOX_CUSTOM_BORDER)
    } else {
        (m.width, m.height)
    };
    let full = NSRect::new(NSPoint::ZERO, NSSize::new(w, h));
    let inset =
        |r: NSRect| NSRect::new(NSPoint::new(mx, r.origin.y + my), NSSize::new(w - 2.0 * mx, r.size.height - 2.0 * my));
    if !titled(b) {
        return Layout { border: full, title: NSRect::ZERO, content: inset(full) };
    }
    let t = title_size(b);
    let th = t.height;
    let title_at = |y: f64| NSRect::new(NSPoint::new(metrics::BOX_TITLE_X, y), t);
    let content_between =
        |bottom: f64, top: f64| NSRect::new(NSPoint::new(mx, bottom), NSSize::new(w - 2.0 * mx, top - bottom));
    match b.ivars().position.get() {
        NSTitlePosition::BelowTop => {
            Layout { border: full, title: title_at(h - th + 2.0), content: content_between(my, h - th + 4.0) }
        }
        NSTitlePosition::AboveBottom => {
            Layout { border: full, title: title_at(-2.0), content: content_between(th - 4.0, h - my) }
        }
        NSTitlePosition::AtBottom | NSTitlePosition::BelowBottom => {
            let border = NSRect::new(NSPoint::new(0.0, th - 2.0), NSSize::new(w, h - (th - 2.0)));
            Layout { border, title: title_at(0.0), content: content_between(th + 3.0, h - my) }
        }
        _ => {
            let border = NSRect::new(NSPoint::ZERO, NSSize::new(w, h - (th - 2.0)));
            Layout { border, title: title_at(h - th), content: inset(border) }
        }
    }
}

/// The distance from the frame to the content on each side: left, bottom,
/// right, top.
fn insets(b: &NSBoxImpl) -> (f64, f64, f64, f64) {
    let l = layout(b);
    let size = views::frame(views::imp(b.as_view())).size;
    let c = l.content;
    (c.origin.x, c.origin.y, size.width - c.origin.x - c.size.width, size.height - c.origin.y - c.size.height)
}

/// Make `view` the content view (none for a separator), in the content
/// rect.
fn put_content(b: &NSBoxImpl, view: Option<&NSView>) {
    let old = b.ivars().content.replace(view.map(|v| v.retain()));
    if let Some(old) = &old
        && view.is_none_or(|v| !std::ptr::eq(&**old, v))
    {
        old.removeFromSuperview();
    }
    if let Some(view) = view {
        b.as_view().addSubview(view);
    }
    drop(old);
    place_content(b);
}

fn place_content(b: &NSBoxImpl) {
    let content = b.ivars().content.borrow().clone();
    if let Some(content) = content {
        content.setFrame(layout(b).content);
    }
}

/// `sizeToFit`: shrink the content round its subviews (which move to its
/// origin) and the box round the content, moving the box by as much as the
/// subviews moved; a titled box stays as wide as its title and 20 points.
fn size_to_fit(b: &NSBoxImpl) {
    let content = b.ivars().content.borrow().clone();
    let view = b.as_view();
    let frame = views::frame(views::imp(view));
    let (left, bottom, right, top) = insets(b);
    let mut union: Option<NSRect> = None;
    if let Some(content) = &content {
        for sub in views::subviews(views::imp(content)) {
            let f = views::frame(views::imp(&sub));
            union = Some(match union {
                None => f,
                Some(u) => {
                    let x0 = u.origin.x.min(f.origin.x);
                    let y0 = u.origin.y.min(f.origin.y);
                    let x1 = (u.origin.x + u.size.width).max(f.origin.x + f.size.width);
                    let y1 = (u.origin.y + u.size.height).max(f.origin.y + f.size.height);
                    NSRect::new(NSPoint::new(x0, y0), NSSize::new(x1 - x0, y1 - y0))
                }
            });
        }
    }
    let Some(union) = union else {
        let margins = b.ivars().margins.get();
        let size =
            if titled(b) || margins != NSSize::ZERO { NSSize::new(left + right, bottom + top) } else { NSSize::ZERO };
        view.setFrame(NSRect::new(frame.origin, size));
        b.changed();
        return;
    };
    if let Some(content) = &content {
        for sub in views::subviews(views::imp(content)) {
            let f = views::frame(views::imp(&sub));
            sub.setFrameOrigin(NSPoint::new(f.origin.x - union.origin.x, f.origin.y - union.origin.y));
        }
    }
    let mut width = union.size.width + left + right;
    if titled(b) {
        width = width.max(title_size(b).width + 20.0);
    }
    let size = NSSize::new(width, union.size.height + bottom + top);
    let origin = NSPoint::new(frame.origin.x + union.origin.x, frame.origin.y + union.origin.y);
    view.setFrame(NSRect::new(origin, size));
    b.changed();
}

fn draw(b: &NSBoxImpl) {
    if !theme::paint::recording() || b.ivars().transparent.get() {
        return;
    }
    let p = theme::palette();
    let l = layout(b);
    match b.ivars().kind.get() {
        NSBoxType::Separator => parts::separator(p, l.border),
        NSBoxType::Custom => {
            let radius = theme::paint::radii(b.ivars().corner_radius.get());
            if let Some(fill) = b.ivars().fill_color.borrow().as_ref() {
                theme::paint::fill_round_rect(l.border, radius, theme::color_of(fill));
            }
            if b.ivars().border_type.get() != NSBorderType::NoBorder {
                let color = b.ivars().border_color.borrow().as_ref().map_or(p.secondary_label, |c| theme::color_of(c));
                theme::paint::stroke_round_rect(l.border, radius, b.ivars().border_width.get(), color);
            }
        }
        _ => {
            parts::card(p, l.border);
            if titled(b) {
                let r = NSRect::new(
                    NSPoint::new(l.title.origin.x + metrics::BOX_TITLE_PAD / 2.0, l.title.origin.y),
                    NSSize::new(l.title.size.width - metrics::BOX_TITLE_PAD, l.title.size.height),
                );
                // The title never draws outside its rect.
                theme::paint::with_clip(l.title, || styled_title(b, p.secondary_label).draw(r));
            }
        }
    }
}
