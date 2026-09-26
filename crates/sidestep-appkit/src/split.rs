//! `NSSplitView`: arranged subviews side by side (vertical dividers) or
//! stacked (horizontal ones), with dividers the user drags.
//!
//! Frames are the state, as in AppKit: the views sit one after another
//! along the split, each divider the dividers' thickness after the view
//! before it. A hidden view is collapsed: it keeps its frame, takes no
//! room, and its dividers stay. `adjustSubviews` shares the room among the
//! shown views in proportion to their sizes; a resize shares the change the
//! same way unless the delegate's `splitView:resizeSubviewsWithOldSize:`
//! does it. `setPosition:ofDividerAtIndex:` moves one divider between the
//! views on either side, within the range the delegate allows, and
//! collapses a view pushed past its edge when the delegate lets it.
//!
//! Dividers are dragged with the mouse: `mouseDown:` finds the divider
//! under the pointer (its drawn rectangle, widened to at least five points,
//! as the delegate adjusts it), and `mouseDragged:` moves it; there is no
//! tracking loop of its own. Positions go to the user defaults under the
//! autosave name, in AppKit's format, and come back from there when the
//! name is set.

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSCursor, NSEvent, NSResponder, NSSplitView, NSSplitViewDelegate, NSSplitViewDividerStyle,
    NSView,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSNotification, NSNumber, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    NSUserDefaults,
};

use crate::views;

sidestep_runtime::static_class!(pub NSSPLITVIEW, NSSPLITVIEW_META = "NSSplitView", || {
    let _ = NSSplitViewImpl::class();
});

sidestep_foundation::constant_string!(
    NSSplitViewWillResizeSubviewsNotification = "NSSplitViewWillResizeSubviewsNotification"
);
sidestep_foundation::constant_string!(
    NSSplitViewDidResizeSubviewsNotification = "NSSplitViewDidResizeSubviewsNotification"
);

/// A thin divider's hit area grows to this many points.
const MIN_EFFECTIVE: f64 = 5.0;
const DEFAULT_HOLDING: f32 = 250.0;

pub(crate) struct SplitIvars {
    vertical: Cell<bool>,
    style: Cell<NSSplitViewDividerStyle>,
    autosave: RefCell<Option<Retained<NSString>>>,
    delegate: RefCell<Option<Weak<AnyObject>>>,
    /// By arranged index; missing ones are the default.
    holding: RefCell<Vec<f32>>,
    arranges_all: Cell<bool>,
    /// The arranged views when not all subviews are.
    arranged: RefCell<Vec<Retained<NSView>>>,
    /// The divider being dragged, and where in it the drag took hold.
    drag: Cell<Option<(usize, f64)>>,
}

impl Default for SplitIvars {
    fn default() -> Self {
        SplitIvars {
            vertical: Cell::new(false),
            style: Cell::new(NSSplitViewDividerStyle::Thick),
            autosave: RefCell::new(None),
            delegate: RefCell::new(None),
            holding: RefCell::default(),
            arranges_all: Cell::new(true),
            arranged: RefCell::default(),
            drag: Cell::new(None),
        }
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSplitView"]
    #[ivars = SplitIvars]
    pub(crate) struct NSSplitViewImpl;

    impl NSSplitViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(SplitIvars::default());
            // SAFETY: NSView's designated initializer.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(isVertical))]
        fn is_vertical(&self) -> bool {
            self.ivars().vertical.get()
        }

        #[unsafe(method(setVertical:))]
        fn set_vertical(&self, vertical: bool) {
            if self.ivars().vertical.replace(vertical) != vertical {
                self.as_view().setNeedsDisplay(true);
            }
        }

        #[unsafe(method(dividerStyle))]
        fn divider_style(&self) -> NSSplitViewDividerStyle {
            self.ivars().style.get()
        }

        #[unsafe(method(setDividerStyle:))]
        fn set_divider_style(&self, style: NSSplitViewDividerStyle) {
            self.ivars().style.set(style);
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(isPaneSplitter))]
        fn is_pane_splitter(&self) -> bool {
            self.ivars().style.get() == NSSplitViewDividerStyle::PaneSplitter
        }

        #[unsafe(method(setIsPaneSplitter:))]
        fn set_is_pane_splitter(&self, flag: bool) {
            let style = if flag { NSSplitViewDividerStyle::PaneSplitter } else { NSSplitViewDividerStyle::Thick };
            self.split().setDividerStyle(style);
        }

        #[unsafe(method(dividerThickness))]
        fn divider_thickness(&self) -> f64 {
            match self.ivars().style.get() {
                NSSplitViewDividerStyle::Thin => 1.0,
                NSSplitViewDividerStyle::PaneSplitter => 10.0,
                _ => 9.0,
            }
        }

        #[unsafe(method_id(dividerColor))]
        fn divider_color(&self) -> Retained<NSColor> {
            let white = if self.ivars().style.get() == NSSplitViewDividerStyle::Thin { 0.8 } else { 0.9 };
            NSColor::colorWithWhite_alpha(white, 1.0)
        }

        #[unsafe(method(drawDividerInRect:))]
        fn draw_divider_in_rect(&self, rect: NSRect) {
            self.split().dividerColor().setFill();
            NSBezierPath::fillRect(rect);
            if self.ivars().style.get() == NSSplitViewDividerStyle::Thick {
                // A dimple in the middle.
                let dot = NSRect::new(
                    NSPoint::new(rect.origin.x + rect.size.width / 2.0 - 2.0, rect.origin.y + rect.size.height / 2.0 - 2.0),
                    NSSize::new(4.0, 4.0),
                );
                NSColor::colorWithWhite_alpha(0.6, 1.0).setFill();
                NSBezierPath::fillRect(dot);
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            for i in 0..self.dividers() {
                if !self.hides_divider(i) {
                    self.split().drawDividerInRect(self.divider_rect(i));
                }
            }
        }

        #[unsafe(method_id(autosaveName))]
        fn autosave_name(&self) -> Option<Retained<NSString>> {
            self.ivars().autosave.borrow().clone()
        }

        #[unsafe(method(setAutosaveName:))]
        fn set_autosave_name(&self, name: Option<&NSString>) {
            let old = self.ivars().autosave.replace(name.map(objc2_foundation::NSCopying::copy));
            drop(old);
            self.restore();
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<ProtocolObject<dyn NSSplitViewDelegate>>> {
            // SAFETY: only delegates are stored.
            self.delegate_object().map(|d| unsafe { Retained::cast_unchecked(d) })
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&ProtocolObject<dyn NSSplitViewDelegate>>) {
            self.ivars().delegate.replace(delegate.map(|d| Weak::new(d.as_ref())));
        }

        #[unsafe(method(adjustSubviews))]
        fn adjust_subviews(&self) {
            self.adjust();
        }

        #[unsafe(method(isSubviewCollapsed:))]
        fn is_subview_collapsed(&self, view: &NSView) -> bool {
            views::superview_of(views::imp(view)).is_some_and(|s| std::ptr::eq(views::as_view(s), self.as_view()))
                && view.isHidden()
        }

        #[unsafe(method(minPossiblePositionOfDividerAtIndex:))]
        fn min_possible_position(&self, index: isize) -> f64 {
            self.min_position(index)
        }

        #[unsafe(method(maxPossiblePositionOfDividerAtIndex:))]
        fn max_possible_position(&self, index: isize) -> f64 {
            self.max_position(index)
        }

        #[unsafe(method(setPosition:ofDividerAtIndex:))]
        fn set_position(&self, position: f64, index: isize) {
            self.move_divider(position, index, false);
        }

        #[unsafe(method(holdingPriorityForSubviewAtIndex:))]
        fn holding_priority(&self, index: isize) -> f32 {
            let holding = self.ivars().holding.borrow();
            usize::try_from(index).ok().and_then(|i| holding.get(i).copied()).unwrap_or(DEFAULT_HOLDING)
        }

        #[unsafe(method(setHoldingPriority:forSubviewAtIndex:))]
        fn set_holding_priority(&self, priority: f32, index: isize) {
            // Only arranged views have one.
            let Some(i) = usize::try_from(index).ok().filter(|&i| i < self.arranged().len()) else { return };
            let mut holding = self.ivars().holding.borrow_mut();
            if holding.len() <= i {
                holding.resize(i + 1, DEFAULT_HOLDING);
            }
            holding[i] = priority;
        }

        #[unsafe(method(arrangesAllSubviews))]
        fn arranges_all_subviews(&self) -> bool {
            self.ivars().arranges_all.get()
        }

        #[unsafe(method(setArrangesAllSubviews:))]
        fn set_arranges_all_subviews(&self, flag: bool) {
            if self.ivars().arranges_all.replace(flag) != flag && !flag {
                // What was arranged stays arranged.
                let all: Vec<Retained<NSView>> = views::subviews(views::imp(self.as_view())).to_vec();
                self.ivars().arranged.replace(all);
            }
        }

        #[unsafe(method_id(arrangedSubviews))]
        fn arranged_subviews(&self) -> Retained<NSArray<NSView>> {
            NSArray::from_retained_slice(&self.arranged())
        }

        #[unsafe(method(addArrangedSubview:))]
        fn add_arranged_subview(&self, view: &NSView) {
            let at = self.arranged().len();
            self.insert_arranged(view, at);
        }

        #[unsafe(method(insertArrangedSubview:atIndex:))]
        fn insert_arranged_subview(&self, view: &NSView, index: isize) {
            self.insert_arranged(view, index.max(0) as usize);
        }

        #[unsafe(method(removeArrangedSubview:))]
        fn remove_arranged_subview(&self, view: &NSView) {
            if self.ivars().arranges_all.get() {
                view.removeFromSuperview();
            } else {
                self.forget(view);
            }
        }

        #[unsafe(method(willRemoveSubview:))]
        fn will_remove_subview(&self, view: &NSView) {
            self.forget(view);
            // SAFETY: NSView's willRemoveSubview: takes the subview.
            unsafe { msg_send![super(self), willRemoveSubview: view] }
        }

        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize_subviews_with_old_size(&self, old: NSSize) {
            self.resize(old);
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            let sup = views::superview_of(views::imp(self.as_view())).map(views::as_view);
            let local = self.as_view().convertPoint_fromView(point, sup);
            if !self.as_view().isHidden() && self.divider_at(local).is_some() {
                Some(self.as_view().retain())
            } else {
                // SAFETY: NSView's hitTest: takes a point and returns a view.
                unsafe { msg_send![super(self), hitTest: point] }
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.as_view().convertPoint_fromView(event.locationInWindow(), None);
            match self.divider_at(p) {
                Some(i) => {
                    let at = self.along(p) - self.divider_position(i);
                    self.ivars().drag.set(Some((i, at)));
                }
                // SAFETY: NSResponder's mouseDown: takes the event.
                None => unsafe { msg_send![super(self), mouseDown: event] },
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            match self.ivars().drag.get() {
                Some((i, at)) => {
                    let p = self.as_view().convertPoint_fromView(event.locationInWindow(), None);
                    self.move_divider(self.along(p) - at, i as isize, true);
                }
                // SAFETY: NSResponder's mouseDragged: takes the event.
                None => unsafe { msg_send![super(self), mouseDragged: event] },
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            if self.ivars().drag.take().is_none() {
                // SAFETY: NSResponder's mouseUp: takes the event.
                unsafe { msg_send![super(self), mouseUp: event] }
            }
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            #[allow(deprecated)] // What Sidestep's cursors provide.
            let cursor = if self.ivars().vertical.get() {
                NSCursor::resizeLeftRightCursor()
            } else {
                NSCursor::resizeUpDownCursor()
            };
            for i in 0..self.dividers() {
                if !self.hides_divider(i) {
                    self.as_view().addCursorRect_cursor(self.effective_rect(i), &cursor);
                }
            }
        }
    }

    unsafe impl NSObjectProtocol for NSSplitViewImpl {}
);

impl NSSplitViewImpl {
    fn as_view(&self) -> &NSView {
        // SAFETY: an NSSplitView is an NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }

    fn split(&self) -> &NSSplitView {
        // SAFETY: NSSplitView is this class.
        unsafe { &*(self as *const Self).cast::<NSSplitView>() }
    }

    fn delegate_object(&self) -> Option<Retained<AnyObject>> {
        self.ivars().delegate.borrow().as_ref().and_then(Weak::load)
    }

    /// The delegate, if it answers `selector`.
    fn delegate_for(&self, selector: Sel) -> Option<Retained<AnyObject>> {
        self.delegate_object().filter(|d| d.class().responds_to(selector))
    }

    fn arranged(&self) -> Vec<Retained<NSView>> {
        if self.ivars().arranges_all.get() {
            views::subviews(views::imp(self.as_view())).to_vec()
        } else {
            self.ivars().arranged.borrow().clone()
        }
    }

    fn insert_arranged(&self, view: &NSView, at: usize) {
        let own =
            views::superview_of(views::imp(view)).is_some_and(|s| std::ptr::eq(views::as_view(s), self.as_view()));
        if self.ivars().arranges_all.get() {
            let subviews = views::subviews(views::imp(self.as_view()));
            match subviews.get(at) {
                Some(next) if !std::ptr::eq(&**next, view) => self.as_view().addSubview_positioned_relativeTo(
                    view,
                    objc2_app_kit::NSWindowOrderingMode::Below,
                    Some(next),
                ),
                _ if !own => self.as_view().addSubview(view),
                _ => {}
            }
            return;
        }
        self.forget(view);
        {
            let mut arranged = self.ivars().arranged.borrow_mut();
            let at = at.min(arranged.len());
            arranged.insert(at, view.retain());
        }
        if !own {
            self.as_view().addSubview(view);
        }
    }

    fn forget(&self, view: &NSView) {
        let removed = {
            let mut arranged = self.ivars().arranged.borrow_mut();
            arranged.iter().position(|v| std::ptr::eq(&**v, view)).map(|i| arranged.remove(i))
        };
        drop(removed);
    }

    fn thickness(&self) -> f64 {
        self.split().dividerThickness()
    }

    fn dividers(&self) -> usize {
        self.arranged().len().saturating_sub(1)
    }

    /// A point's coordinate along the split.
    fn along(&self, p: NSPoint) -> f64 {
        if self.ivars().vertical.get() { p.x } else { p.y }
    }

    /// A view's start and size along the split.
    fn extent(&self, view: &NSView) -> (f64, f64) {
        let f = view.frame();
        if self.ivars().vertical.get() { (f.origin.x, f.size.width) } else { (f.origin.y, f.size.height) }
    }

    /// The length of the split, across its views.
    fn length(&self) -> f64 {
        let b = self.as_view().bounds();
        if self.ivars().vertical.get() { b.size.width } else { b.size.height }
    }

    /// Where divider `i` starts: after the last view before it that shows.
    fn divider_position(&self, i: usize) -> f64 {
        let arranged = self.arranged();
        let t = self.thickness();
        // Collapsed views take no room: go back to one that shows.
        let mut pos = 0.0;
        for (k, view) in arranged.iter().enumerate().take(i + 1) {
            if view.isHidden() {
                if k > 0 {
                    pos += t;
                }
            } else {
                let (start, size) = self.extent(view);
                pos = start + size;
            }
        }
        pos
    }

    fn divider_rect(&self, i: usize) -> NSRect {
        let b = self.as_view().bounds();
        let pos = self.divider_position(i);
        let t = self.thickness();
        if self.ivars().vertical.get() {
            NSRect::new(NSPoint::new(pos, b.origin.y), NSSize::new(t, b.size.height))
        } else {
            NSRect::new(NSPoint::new(b.origin.x, pos), NSSize::new(b.size.width, t))
        }
    }

    fn hides_divider(&self, i: usize) -> bool {
        self.delegate_for(sel!(splitView:shouldHideDividerAtIndex:)).is_some_and(|d| {
            // SAFETY: the delegate method takes the split view and an index
            // and returns BOOL.
            unsafe { msg_send![&*d, splitView: self.split(), shouldHideDividerAtIndex: i as isize] }
        })
    }

    /// Where divider `i` takes the mouse: its drawn rectangle, at least
    /// five points thick, as the delegate adjusts it.
    fn effective_rect(&self, i: usize) -> NSRect {
        let drawn = self.divider_rect(i);
        let t = self.thickness();
        let grow = ((MIN_EFFECTIVE - t) / 2.0).max(0.0);
        let mut rect = if self.ivars().vertical.get() {
            NSRect::new(
                NSPoint::new(drawn.origin.x - grow, drawn.origin.y),
                NSSize::new(t + 2.0 * grow, drawn.size.height),
            )
        } else {
            NSRect::new(
                NSPoint::new(drawn.origin.x, drawn.origin.y - grow),
                NSSize::new(drawn.size.width, t + 2.0 * grow),
            )
        };
        if let Some(d) = self.delegate_for(sel!(splitView:effectiveRect:forDrawnRect:ofDividerAtIndex:)) {
            // SAFETY: the delegate method takes the split view, two rects
            // and an index, and returns a rect.
            rect = unsafe {
                msg_send![&*d, splitView: self.split(), effectiveRect: rect, forDrawnRect: drawn, ofDividerAtIndex: i as isize]
            };
        }
        rect
    }

    /// The divider whose effective or additional rectangle holds `p`.
    fn divider_at(&self, p: NSPoint) -> Option<usize> {
        let inside = |r: NSRect| {
            p.x >= r.origin.x
                && p.y >= r.origin.y
                && p.x < r.origin.x + r.size.width
                && p.y < r.origin.y + r.size.height
        };
        let extra = self.delegate_for(sel!(splitView:additionalEffectiveRectOfDividerAtIndex:));
        (0..self.dividers()).find(|&i| {
            if self.hides_divider(i) {
                return false;
            }
            inside(self.effective_rect(i))
                || extra.as_ref().is_some_and(|d| {
                    // SAFETY: the delegate method takes the split view and an
                    // index and returns a rect.
                    let r: NSRect = unsafe {
                        msg_send![&**d, splitView: self.split(), additionalEffectiveRectOfDividerAtIndex: i as isize]
                    };
                    inside(r)
                })
        })
    }

    fn min_position(&self, index: isize) -> f64 {
        let arranged = self.arranged();
        match usize::try_from(index).ok().filter(|&i| i + 1 < arranged.len()) {
            Some(i) if arranged[i].isHidden() => self.divider_position(i),
            Some(i) => self.extent(&arranged[i]).0,
            None => -1.0,
        }
    }

    fn max_position(&self, index: isize) -> f64 {
        let arranged = self.arranged();
        let t = self.thickness();
        match usize::try_from(index).ok().filter(|&i| i + 1 < arranged.len()) {
            Some(i) if arranged[i + 1].isHidden() => self.divider_position(i + 1) - t,
            Some(i) => {
                let (start, size) = self.extent(&arranged[i + 1]);
                start + size - t
            }
            None => -1.0,
        }
    }

    /// Put `view` at `start` along the split with `size`, filling the
    /// split across.
    fn place(&self, view: &NSView, start: f64, size: f64) {
        let b = self.as_view().bounds();
        let size = size.max(0.0);
        let frame = if self.ivars().vertical.get() {
            NSRect::new(NSPoint::new(start, 0.0), NSSize::new(size, b.size.height))
        } else {
            NSRect::new(NSPoint::new(0.0, start), NSSize::new(b.size.width, size))
        };
        if view.frame() != frame {
            view.setFrame(frame);
        }
    }

    /// The backing scale, for rounding edges to pixels.
    fn scale(&self) -> f64 {
        views::window_of(views::imp(self.as_view())).map_or(1.0, |w| w.as_window().backingScaleFactor())
    }

    /// Lay the shown views out with the given sizes, one after another.
    fn lay_out(&self, arranged: &[Retained<NSView>], sizes: &[f64]) {
        let t = self.thickness();
        let scale = self.scale();
        let round = |v: f64| (v * scale).round() / scale;
        let mut pos = 0.0;
        for (k, (view, size)) in arranged.iter().zip(sizes).enumerate() {
            if k > 0 {
                pos += t;
            }
            if view.isHidden() {
                continue;
            }
            let (start, end) = (round(pos), round(pos + size));
            self.place(view, start, end - start);
            pos += size;
        }
    }

    fn should_adjust(&self, view: &NSView) -> bool {
        self.delegate_for(sel!(splitView:shouldAdjustSizeOfSubview:)).is_none_or(|d| {
            // SAFETY: the delegate method takes the split view and a view and
            // returns BOOL.
            unsafe { msg_send![&*d, splitView: self.split(), shouldAdjustSizeOfSubview: view] }
        })
    }

    /// Share `total` among the shown views in proportion to their sizes,
    /// leaving those the delegate holds as they are.
    fn share(&self, arranged: &[Retained<NSView>], total: f64) -> Vec<f64> {
        let current: Vec<f64> = arranged.iter().map(|v| if v.isHidden() { 0.0 } else { self.extent(v).1 }).collect();
        let fixed: Vec<bool> = arranged.iter().map(|v| v.isHidden() || !self.should_adjust(v)).collect();
        let held: f64 = current.iter().zip(&fixed).filter(|(_, f)| **f).map(|(c, _)| c).sum();
        let flexible: f64 = current.iter().zip(&fixed).filter(|(_, f)| !**f).map(|(c, _)| c).sum();
        let count = fixed.iter().filter(|f| !**f).count();
        let room = (total - held).max(0.0);
        current
            .iter()
            .zip(&fixed)
            .map(|(&c, &f)| {
                if f {
                    c
                } else if flexible > 0.0 {
                    room * c / flexible
                } else {
                    room / count as f64
                }
            })
            .collect()
    }

    fn adjust(&self) {
        let arranged = self.arranged();
        if arranged.is_empty() {
            return;
        }
        let room = self.length() - self.thickness() * (arranged.len() - 1) as f64;
        let sizes = self.share(&arranged, room);
        self.lay_out(&arranged, &sizes);
        self.save();
    }

    fn resize(&self, old: NSSize) {
        if let Some(d) = self.delegate_for(sel!(splitView:resizeSubviewsWithOldSize:)) {
            // SAFETY: the delegate method takes the split view and a size.
            let _: () = unsafe { msg_send![&*d, splitView: self.split(), resizeSubviewsWithOldSize: old] };
            return;
        }
        let arranged = self.arranged();
        if arranged.is_empty() {
            return;
        }
        let dividers: Vec<usize> = (0..arranged.len() - 1).collect();
        for &i in &dividers {
            self.announce(true, i, None);
        }
        let room = self.length() - self.thickness() * (arranged.len() - 1) as f64;
        let sizes = self.share(&arranged, room);
        self.lay_out(&arranged, &sizes);
        for &i in &dividers {
            self.announce(false, i, None);
        }
        self.save();
    }

    /// `setPosition:ofDividerAtIndex:`, or a drag (`user`).
    fn move_divider(&self, position: f64, index: isize, user: bool) {
        let arranged = self.arranged();
        let Some(i) = usize::try_from(index).ok().filter(|&i| i + 1 < arranged.len()) else { return };
        let split = self.split();
        let mut min = self.min_position(index);
        let mut max = self.max_position(index);
        if let Some(d) = self.delegate_for(sel!(splitView:constrainMinCoordinate:ofSubviewAt:)) {
            // SAFETY: the delegate method takes the split view, a coordinate
            // and an index, and returns a coordinate.
            min = unsafe { msg_send![&*d, splitView: split, constrainMinCoordinate: min, ofSubviewAt: index] };
        }
        if let Some(d) = self.delegate_for(sel!(splitView:constrainMaxCoordinate:ofSubviewAt:)) {
            // SAFETY: as above.
            max = unsafe { msg_send![&*d, splitView: split, constrainMaxCoordinate: max, ofSubviewAt: index] };
        }
        let (before, after) = (&arranged[i], &arranged[i + 1]);
        let can_collapse = |view: &NSView| {
            self.delegate_for(sel!(splitView:canCollapseSubview:)).is_some_and(|d| {
                // SAFETY: the delegate method takes the split view and a view
                // and returns BOOL.
                unsafe { msg_send![&*d, splitView: split, canCollapseSubview: view] }
            })
        };
        let t = self.thickness();
        // Where the view before the divider starts and the one after ends.
        let start = self.min_position(index);
        let end = self.max_position(index) + t;
        let position = if position > max && can_collapse(after) {
            // Push the view after out of sight.
            self.announce(true, i, Some(user));
            after.setHidden(true);
            if !before.isHidden() {
                self.place(before, start, end - start - t);
            }
            self.announce(false, i, Some(user));
            self.save();
            return;
        } else if position < min && can_collapse(before) {
            self.announce(true, i, Some(user));
            before.setHidden(true);
            if !after.isHidden() {
                self.place(after, start + t, end - start - t);
            }
            self.announce(false, i, Some(user));
            self.save();
            return;
        } else {
            let position = position.min(max).max(min);
            match self.delegate_for(sel!(splitView:constrainSplitPosition:ofSubviewAt:)) {
                // SAFETY: as for the other constraining methods.
                Some(d) => unsafe {
                    msg_send![&*d, splitView: split, constrainSplitPosition: position, ofSubviewAt: index]
                },
                None => position,
            }
        };
        self.announce(true, i, Some(user));
        let scale = self.scale();
        let position = (position * scale).round() / scale;
        // A collapsed view comes back when its divider moves off the edge.
        if before.isHidden() && position > start {
            before.setHidden(false);
        }
        if after.isHidden() && position + t < end {
            after.setHidden(false);
        }
        if !before.isHidden() {
            self.place(before, start, position - start);
        }
        if !after.isHidden() {
            self.place(after, position + t, end - position - t);
        }
        self.as_view().setNeedsDisplay(true);
        self.announce(false, i, Some(user));
        self.save();
    }

    /// Post `NSSplitViewWill…` or `Did…ResizeSubviewsNotification`, and
    /// tell the delegate. `user` says whether the user moved the divider,
    /// when one was moved.
    fn announce(&self, will: bool, divider: usize, user: Option<bool>) {
        // SAFETY: the names are constants this module exports.
        let name: &NSString = unsafe {
            if will {
                objc2_app_kit::NSSplitViewWillResizeSubviewsNotification
            } else {
                objc2_app_kit::NSSplitViewDidResizeSubviewsNotification
            }
        };
        let selector = if will { sel!(splitViewWillResizeSubviews:) } else { sel!(splitViewDidResizeSubviews:) };
        let delegate = self.delegate_for(selector);
        if delegate.is_none() && !sidestep_foundation::notification_center::has_observers(name) {
            return;
        }
        let index = NSNumber::new_isize(divider as isize);
        let info = match user {
            Some(user) => {
                let user = NSNumber::new_bool(user);
                let keys =
                    [&*NSString::from_str("NSSplitViewDividerIndex"), &*NSString::from_str("NSSplitViewUserResizeKey")];
                let values: [&AnyObject; 2] = [&index, &user];
                NSDictionary::from_slices(&keys, &values)
            }
            None => {
                let keys = [&*NSString::from_str("NSSplitViewDividerIndex")];
                let values: [&AnyObject; 1] = [&index];
                NSDictionary::from_slices(&keys, &values)
            }
        };
        let this: &AnyObject = self.as_view();
        // SAFETY: a dictionary of strings to objects is a dictionary of
        // objects, as user info is.
        let info = unsafe { &*Retained::as_ptr(&info).cast::<NSDictionary>() };
        // SAFETY: as above.
        let note = unsafe { NSNotification::notificationWithName_object_userInfo(name, Some(this), Some(info)) };
        if let Some(d) = delegate {
            // SAFETY: the delegate's notification methods take the
            // notification.
            let _: () = unsafe { objc2::runtime::MessageReceiver::send_message(&*d, selector, (&*note,)) };
        }
        sidestep_foundation::notification_center::default_center().postNotification(&note);
    }

    fn defaults_key(&self) -> Option<Retained<NSString>> {
        let name = self.ivars().autosave.borrow().clone()?;
        (name.length() > 0).then(|| NSString::from_str(&format!("NSSplitView Subview Frames {name}")))
    }

    /// Keep the arranged views' frames under the autosave name.
    fn save(&self) {
        let Some(key) = self.defaults_key() else { return };
        let frames: Vec<Retained<NSString>> = self
            .arranged()
            .iter()
            .map(|v| {
                let f = v.frame();
                let hidden = if v.isHidden() { "YES" } else { "NO" };
                NSString::from_str(&format!(
                    "{:.6}, {:.6}, {:.6}, {:.6}, {hidden}, NO",
                    f.origin.x, f.origin.y, f.size.width, f.size.height
                ))
            })
            .collect();
        let array = NSArray::from_retained_slice(&frames);
        // SAFETY: an array of strings is a property list.
        unsafe { NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&array), &key) };
    }

    /// Take the arranged views' frames from the autosave name, if saved
    /// for as many views.
    fn restore(&self) {
        let Some(key) = self.defaults_key() else { return };
        let Some(saved) = NSUserDefaults::standardUserDefaults().arrayForKey(&key) else { return };
        let arranged = self.arranged();
        if saved.count() != arranged.len() {
            return;
        }
        for (view, entry) in arranged.iter().zip(saved.iter()) {
            let Some(text) = entry.downcast_ref::<NSString>() else { return };
            let text = text.to_string();
            let fields: Vec<&str> = text.split(',').map(str::trim).collect();
            let number = |i: usize| fields.get(i).and_then(|f| f.parse::<f64>().ok());
            let (Some(x), Some(y), Some(w), Some(h)) = (number(0), number(1), number(2), number(3)) else { return };
            view.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)));
            view.setHidden(fields.get(4) == Some(&"YES"));
        }
    }
}
