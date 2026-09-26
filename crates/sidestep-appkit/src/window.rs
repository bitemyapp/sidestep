//! `NSWindow`: routes input to views and runs the display pass.
//!
//! Invalidation collects damage per layer, in layer pixels. When the render
//! thread has shown the last frame, a display pass
//!
//! - places each scroll layer: where its viewport is and how far it has
//!   scrolled; the render thread moves tiles, nothing is redrawn;
//! - draws the tiles that come near the viewport and forgets far ones;
//! - redraws the damaged parts of each layer by calling `drawRect:` on the
//!   views there, which records drawing ops for the render thread;
//! - presents.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSClipView, NSEvent, NSEventType, NSResponder, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSCopying, NSPoint, NSRect, NSSize, NSString};

use crate::app;
use crate::graphics::{self, Xf};
use crate::protocol::{LayerId, Op, ROOT_LAYER, Rect, TILE_HEIGHT, ToRender, WindowId};
use crate::views::{self, NSViewImpl};

/// The window background, and what layers are cleared to before drawing.
const BACKGROUND: [f32; 4] = [0.925, 0.925, 0.925, 1.0];

pub(crate) struct WindowIvars {
    id: WindowId,
    title: RefCell<Retained<NSString>>,
    size: Cell<NSSize>,
    content: RefCell<Option<Retained<NSView>>>,
    first_responder: RefCell<Option<Retained<NSResponder>>>,
    released_when_closed: Cell<bool>,
    visible: Cell<bool>,
    configured: Cell<bool>,
    /// A frame was presented and the render thread hasn't shown it yet.
    frame_pending: Cell<bool>,
    needs_display: Cell<bool>,
    damage: RefCell<HashMap<LayerId, Vec<Rect>>>,
    clips: RefCell<Vec<Retained<NSView>>>,
    layers: RefCell<HashMap<LayerId, LayerState>>,
    /// The view that got the last mouse down, for the drags and up after it.
    mouse_view: RefCell<Option<Retained<NSView>>>,
}

#[derive(Default)]
struct LayerState {
    doc_width: u32,
    /// Tiles the render thread has drawn and keeps.
    valid: BTreeSet<u32>,
}

define_class!(
    #[unsafe(super(NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSWindow"]
    #[ivars = WindowIvars]
    pub(crate) struct NSWindowImpl;

    impl NSWindowImpl {
        #[unsafe(method_id(initWithContentRect:styleMask:backing:defer:))]
        fn init(
            this: Allocated<Self>,
            rect: NSRect,
            _style: NSWindowStyleMask,
            _backing: NSBackingStoreType,
            _defer: bool,
        ) -> Retained<Self> {
            app::load_shells();
            static NEXT_ID: AtomicU32 = AtomicU32::new(1);
            let this = this.set_ivars(WindowIvars {
                id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                title: RefCell::new(NSString::new()),
                size: Cell::new(rect.size),
                content: RefCell::new(None),
                first_responder: RefCell::new(None),
                released_when_closed: Cell::new(true),
                visible: Cell::new(false),
                configured: Cell::new(false),
                frame_pending: Cell::new(false),
                needs_display: Cell::new(false),
                damage: RefCell::new(HashMap::new()),
                clips: RefCell::new(Vec::new()),
                layers: RefCell::new(HashMap::new()),
                mouse_view: RefCell::new(None),
            });
            // SAFETY: NSResponder's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> {
            self.ivars().title.borrow().clone()
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: &NSString) {
            self.ivars().title.replace(title.copy());
            if self.ivars().visible.get() {
                app::send(ToRender::SetTitle { window: self.ivars().id, title: title.to_string() });
            }
        }

        #[unsafe(method_id(contentView))]
        fn content_view(&self) -> Option<Retained<NSView>> {
            self.ivars().content.borrow().clone()
        }

        #[unsafe(method(setContentView:))]
        fn set_content_view(&self, view: Option<&NSView>) {
            set_content_view(self, view);
        }

        #[unsafe(method(frame))]
        fn frame(&self) -> NSRect {
            NSRect::new(NSPoint::ZERO, self.ivars().size.get())
        }

        #[unsafe(method(contentLayoutRect))]
        fn content_layout_rect(&self) -> NSRect {
            NSRect::new(NSPoint::ZERO, self.ivars().size.get())
        }

        #[unsafe(method(makeKeyAndOrderFront:))]
        fn make_key_and_order_front(&self, _sender: Option<&AnyObject>) {
            order_front(self);
        }

        #[unsafe(method(orderFront:))]
        fn order_front(&self, _sender: Option<&AnyObject>) {
            order_front(self);
        }

        #[unsafe(method(orderOut:))]
        fn order_out(&self, _sender: Option<&AnyObject>) {
            order_out(self);
        }

        #[unsafe(method(close))]
        fn close(&self) {
            order_out(self);
            app::window_closed();
        }

        #[unsafe(method(performClose:))]
        fn perform_close(&self, _sender: Option<&AnyObject>) {
            as_window(self).close();
        }

        #[unsafe(method(center))]
        fn center(&self) {}

        #[unsafe(method(isVisible))]
        fn is_visible(&self) -> bool {
            self.ivars().visible.get()
        }

        #[unsafe(method(isKeyWindow))]
        fn is_key_window(&self) -> bool {
            self.ivars().visible.get()
        }

        #[unsafe(method(isReleasedWhenClosed))]
        fn is_released_when_closed(&self) -> bool {
            self.ivars().released_when_closed.get()
        }

        #[unsafe(method(setReleasedWhenClosed:))]
        fn set_released_when_closed(&self, flag: bool) {
            // Only recorded: a window is never released on its own, which
            // at worst leaks one the program forgot.
            self.ivars().released_when_closed.set(flag);
        }

        #[unsafe(method_id(firstResponder))]
        fn first_responder(&self) -> Option<Retained<NSResponder>> {
            self.ivars().first_responder.borrow().clone()
        }

        #[unsafe(method(makeFirstResponder:))]
        fn make_first_responder(&self, responder: Option<&NSResponder>) -> bool {
            make_first_responder(self, responder)
        }

        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            send_event(self, event);
        }

        #[unsafe(method(display))]
        fn display(&self) {
            self.ivars().needs_display.set(true);
        }
    }

    unsafe impl NSObjectProtocol for NSWindowImpl {}
);

impl Drop for WindowIvars {
    fn drop(&mut self) {
        if let Some(content) = self.content.get_mut().take() {
            views::set_window(views::imp(&content), None);
        }
    }
}

pub(crate) fn imp(window: &NSWindow) -> &NSWindowImpl {
    // SAFETY: NSWindow is NSWindowImpl's class; subclasses share its layout.
    unsafe { &*(window as *const NSWindow).cast::<NSWindowImpl>() }
}

fn as_window(window: &NSWindowImpl) -> &NSWindow {
    // SAFETY: as in `imp`.
    unsafe { &*(window as *const NSWindowImpl).cast::<NSWindow>() }
}

impl NSWindowImpl {
    pub(crate) fn id(&self) -> WindowId {
        self.ivars().id
    }

    pub(crate) fn content_height(&self) -> f64 {
        self.ivars().size.get().height
    }

    pub(crate) fn is_content_view(&self, view: &NSViewImpl) -> bool {
        self.ivars().content.borrow().as_ref().is_some_and(|c| std::ptr::eq(views::imp(c), view))
    }

    /// Damage part of a layer, in its pixels.
    pub(crate) fn invalidate(&self, layer: LayerId, rect: Rect) {
        self.ivars().damage.borrow_mut().entry(layer).or_default().push(rect);
        self.ivars().needs_display.set(true);
    }

    /// A scroll layer moved or resized: place it again at the next frame.
    pub(crate) fn layers_moved(&self) {
        self.ivars().needs_display.set(true);
    }

    pub(crate) fn add_clip(&self, clip: &NSView) {
        self.ivars().clips.borrow_mut().push(clip.retain());
        self.ivars().needs_display.set(true);
    }

    pub(crate) fn remove_clip(&self, clip: &NSViewImpl) {
        let layer = views::layer_id(clip);
        self.ivars().clips.borrow_mut().retain(|c| !std::ptr::eq(views::imp(c), clip));
        self.ivars().damage.borrow_mut().remove(&layer);
        if self.ivars().layers.borrow_mut().remove(&layer).is_some() && self.ivars().visible.get() {
            // Hide it: an empty viewport shows no tiles.
            app::send(ToRender::ScrollLayer {
                window: self.ivars().id,
                layer,
                viewport: Rect::default(),
                offset: 0.0,
                doc_width: 0,
            });
        }
        self.ivars().needs_display.set(true);
    }

    fn damage_all(&self) {
        let size = self.ivars().size.get();
        let mut damage = self.ivars().damage.borrow_mut();
        damage.clear();
        damage.insert(ROOT_LAYER, vec![Rect::new(0.0, 0.0, size.width as f32, size.height as f32).round_out()]);
        self.ivars().needs_display.set(true);
    }

    /// The window's size from the compositor, first and after resizes.
    pub(crate) fn configure(&self, width: u32, height: u32) {
        let ivars = self.ivars();
        ivars.configured.set(true);
        ivars.frame_pending.set(false);
        ivars.size.set(NSSize::new(width as f64, height as f64));
        let content = ivars.content.borrow().clone();
        if let Some(content) = content {
            content.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(width as f64, height as f64)));
        }
        self.damage_all();
    }

    pub(crate) fn frame_done(&self) {
        self.ivars().frame_pending.set(false);
    }

    /// Mouse input from the render thread, in surface pixels (top left).
    pub(crate) fn pointer(&self, kind: NSEventType, x: f64, y: f64, button: isize, delta_y: f64) {
        let location = NSPoint::new(x, self.content_height() - y);
        let event = crate::event::mouse_event(kind, location, as_window(self), button, delta_y);
        as_window(self).sendEvent(&event);
    }

    pub(crate) fn wants_drags(&self) -> bool {
        self.ivars().mouse_view.borrow().is_some()
    }
}

fn set_content_view(window: &NSWindowImpl, view: Option<&NSView>) {
    let old = window.ivars().content.replace(view.map(|v| v.retain()));
    if let Some(old) = old {
        // SAFETY: the window no longer holds the view.
        unsafe { old.setNextResponder(None) };
        views::set_window(views::imp(&old), None);
    }
    if let Some(view) = view {
        let size = window.ivars().size.get();
        view.setFrame(NSRect::new(NSPoint::ZERO, size));
        // SAFETY: the window owns its content view, so outlives the link.
        unsafe { view.setNextResponder(Some(window)) };
        views::set_window(views::imp(view), Some(NonNull::from(as_window(window))));
    }
    window.damage_all();
}

fn order_front(window: &NSWindowImpl) {
    let ivars = window.ivars();
    if ivars.visible.get() {
        return;
    }
    ivars.visible.set(true);
    let size = ivars.size.get();
    app::add_window(as_window(window));
    app::send(ToRender::CreateWindow {
        window: ivars.id,
        width: size.width.round().max(1.0) as u32,
        height: size.height.round().max(1.0) as u32,
        title: ivars.title.borrow().to_string(),
    });
}

fn order_out(window: &NSWindowImpl) {
    let ivars = window.ivars();
    if !ivars.visible.get() {
        return;
    }
    ivars.visible.set(false);
    ivars.configured.set(false);
    ivars.frame_pending.set(false);
    for layer in ivars.layers.borrow_mut().values_mut() {
        layer.valid.clear();
    }
    app::send(ToRender::CloseWindow { window: ivars.id });
    app::remove_window(as_window(window));
}

fn make_first_responder(window: &NSWindowImpl, responder: Option<&NSResponder>) -> bool {
    let current = window.ivars().first_responder.borrow().clone();
    if let Some(current) = &current {
        if responder.is_some_and(|r| std::ptr::eq(r, &**current)) {
            return true;
        }
        if !current.resignFirstResponder() {
            return false;
        }
    }
    let accepted = responder.is_none_or(|r| r.becomeFirstResponder());
    window.ivars().first_responder.replace(if accepted { responder.map(|r| r.retain()) } else { None });
    accepted
}

fn send_event(window: &NSWindowImpl, event: &NSEvent) {
    let kind = event.r#type();
    let down = kind == NSEventType::LeftMouseDown
        || kind == NSEventType::RightMouseDown
        || kind == NSEventType::OtherMouseDown;
    let content = window.ivars().content.borrow().clone();
    if down {
        let Some(view) = content.and_then(|c| c.hitTest(event.locationInWindow())) else { return };
        if view.acceptsFirstResponder() {
            as_window(window).makeFirstResponder(Some(&view));
        }
        window.ivars().mouse_view.replace(Some(view.clone()));
        if kind == NSEventType::LeftMouseDown {
            view.mouseDown(event);
        } else if kind == NSEventType::RightMouseDown {
            view.rightMouseDown(event);
        } else {
            view.otherMouseDown(event);
        }
    } else if kind == NSEventType::LeftMouseUp || kind == NSEventType::RightMouseUp || kind == NSEventType::OtherMouseUp
    {
        let Some(view) = window.ivars().mouse_view.take() else { return };
        if kind == NSEventType::LeftMouseUp {
            view.mouseUp(event);
        } else if kind == NSEventType::RightMouseUp {
            view.rightMouseUp(event);
        } else {
            view.otherMouseUp(event);
        }
    } else if kind == NSEventType::LeftMouseDragged {
        let view = window.ivars().mouse_view.borrow().clone();
        if let Some(view) = view {
            view.mouseDragged(event);
        }
    } else if kind == NSEventType::ScrollWheel {
        if let Some(view) = content.and_then(|c| c.hitTest(event.locationInWindow())) {
            view.scrollWheel(event);
        }
    }
}

// The display pass.

/// Run a display pass if the window has something to show and the render
/// thread is ready for it.
pub(crate) fn display_if_needed(window: &NSWindowImpl) {
    let ivars = window.ivars();
    if !ivars.visible.get() || !ivars.configured.get() || ivars.frame_pending.get() || !ivars.needs_display.get() {
        return;
    }
    ivars.needs_display.set(false);
    graphics::install_string_drawing();
    let id = ivars.id;

    let clips = ivars.clips.borrow().clone();
    for clip in &clips {
        update_scroll_layer(window, views::imp(clip));
    }

    let damage = ivars.damage.borrow_mut().remove(&ROOT_LAYER).unwrap_or_default();
    let content = ivars.content.borrow().clone();
    for area in coalesce(damage) {
        graphics::begin_recording();
        graphics::push(Op::Fill { rect: area, color: BACKGROUND });
        if let Some(content) = &content {
            let root = views::imp(content);
            let xf = views::root_xf(root, ROOT_LAYER, window.content_height());
            let size = ivars.size.get();
            let all = Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
            record(root, xf, all, area);
        }
        let ops = graphics::end_recording();
        app::send(ToRender::Paint { window: id, layer: ROOT_LAYER, rects: vec![area], ops });
    }
    // Damage to layers that no longer exist.
    ivars.damage.borrow_mut().clear();

    app::send(ToRender::Present { window: id });
    ivars.frame_pending.set(true);
}

/// Record `view` and its subviews for `area` of a layer. `xf` maps the view
/// to the layer; `clip` is where its ancestors let it draw.
fn record(view: &NSViewImpl, xf: Xf, clip: Rect, area: Rect) {
    let visible = clip.intersect(&xf.rect(views::bounds(view)));
    let target = visible.intersect(&area);
    if target.is_empty() {
        return;
    }
    graphics::set_view(xf, target);
    // SAFETY: drawRect: takes an NSRect.
    unsafe { msg_send![view, drawRect: xf.inverse_rect(target)] }
    if views::is_clip(view) {
        // The document has a layer of its own.
        return;
    }
    let flipped = views::is_flipped(view);
    for sub in views::subviews(view) {
        let sub = views::imp(&sub);
        if views::is_hidden(sub) {
            continue;
        }
        let sub_xf = views::step(sub, flipped, views::frame(sub)).then(&xf);
        record(sub, sub_xf, visible, area);
    }
}

fn update_scroll_layer(window: &NSWindowImpl, clip: &NSViewImpl) {
    let ivars = window.ivars();
    let id = views::layer_id(clip);
    // SAFETY: every clip view is an NSClipView.
    let clip_view = unsafe { &*(clip as *const NSViewImpl).cast::<NSClipView>() };
    let (Some(document), Some(p)) = (clip_view.documentView(), views::placement(clip)) else { return };
    if p.layer != ROOT_LAYER {
        // Scroll views inside scroll views aren't supported yet.
        return;
    }
    let doc = views::imp(&document);
    let full = p.xf.rect(views::bounds(clip)).round_out();
    let viewport = full.intersect(&p.clip);
    let size = views::frame(doc).size;
    let doc_width = size.width.ceil().max(0.0) as u32;
    let doc_height = size.height.ceil().max(0.0) as u32;

    // Where the clip view's top edge falls in the document's layer.
    let doc_xf = views::root_xf(doc, id, 0.0);
    let clip_to_layer = views::step(doc, views::is_flipped(clip), views::frame(doc)).inverse().then(&doc_xf);
    let shown = clip_to_layer.rect(views::bounds(clip));
    let offset = (shown.y0 + (viewport.y0 - full.y0)).round();

    app::send(ToRender::ScrollLayer { window: ivars.id, layer: id, viewport, offset, doc_width });
    if viewport.is_empty() || doc_width == 0 || doc_height == 0 {
        return;
    }

    let mut layers = ivars.layers.borrow_mut();
    let state = layers.entry(id).or_default();
    if state.doc_width != doc_width {
        state.valid.clear();
        state.doc_width = doc_width;
    }
    let tile = TILE_HEIGHT as f32;
    let last_tile = doc_height.div_ceil(TILE_HEIGHT) - 1;
    let height = viewport.y1 - viewport.y0;
    let first = ((offset - tile) / tile).floor().max(0.0) as u32;
    let last = (((offset + height + tile) / tile).floor().max(0.0) as u32).min(last_tile);

    // Forget tiles well away from the viewport.
    let keep = first.saturating_sub(1)..=last + 1;
    let far: Vec<u32> = state.valid.iter().copied().filter(|i| !keep.contains(i)).collect();
    if !far.is_empty() {
        for i in &far {
            state.valid.remove(i);
        }
        app::send(ToRender::DropTiles { window: ivars.id, layer: id, tiles: far });
    }

    let doc_rect = Rect::new(0.0, 0.0, doc_width as f32, doc_height as f32);
    let tile_rect = |i: u32| Rect::new(0.0, (i * TILE_HEIGHT) as f32, doc_width as f32, ((i + 1) * TILE_HEIGHT) as f32);
    let mut areas = Vec::new();
    for i in first..=last {
        if state.valid.insert(i) {
            areas.push(tile_rect(i).intersect(&doc_rect));
        }
    }
    let damage = ivars.damage.borrow_mut().remove(&id).unwrap_or_default();
    for r in coalesce(damage) {
        for &i in &state.valid {
            let part = r.intersect(&tile_rect(i));
            if !part.is_empty() && !areas.iter().any(|a: &Rect| a.intersect(&part) == part) {
                areas.push(part);
            }
        }
    }
    drop(layers);

    for area in areas {
        graphics::begin_recording();
        graphics::push(Op::Fill { rect: area, color: BACKGROUND });
        record(doc, doc_xf, doc_rect, area);
        let ops = graphics::end_recording();
        app::send(ToRender::Paint { window: ivars.id, layer: id, rects: vec![area], ops });
    }
}

/// Merge damage rectangles that overlap or nearly touch, so each area is
/// drawn once.
fn coalesce(mut rects: Vec<Rect>) -> Vec<Rect> {
    rects.retain(|r| !r.is_empty());
    let area = |r: &Rect| (r.x1 - r.x0) * (r.y1 - r.y0);
    let mut merged = true;
    while merged && rects.len() > 1 {
        merged = false;
        'outer: for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let u = rects[i].union(&rects[j]);
                // Merge when the union wastes little over drawing both.
                if area(&u) <= (area(&rects[i]) + area(&rects[j])) * 1.25 + 64.0 {
                    rects[i] = u;
                    rects.swap_remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
    }
    if rects.len() > 16 {
        let all = rects.iter().skip(1).fold(rects[0], |a, r| a.union(r));
        rects = vec![all];
    }
    rects
}
