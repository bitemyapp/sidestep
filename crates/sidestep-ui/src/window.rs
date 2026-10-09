//! Windows: what a program asks for ([`WindowOptions`]) and what it can do
//! with a window it has ([`Window`]).
//!
//! A window's size, state and keyboard focus belong to the compositor:
//! a program asks (`request_size`, `set_maximized`, `activate`) and the
//! window follows what the compositor decides, which the handler hears
//! as [`WindowEvent`](crate::WindowEvent)s. Where the compositor leaves
//! decorations to the program (GNOME), the render thread draws a title
//! bar and a resize border around the content; the content keeps its
//! coordinates, from its own top left.
//!
//! A window may be hidden and shown again ([`Cx::hide_window`],
//! [`Cx::show_window`](crate::Cx::show_window)): it keeps its id, title,
//! style and size, and each showing is a new window to the render thread
//! (a *showing*, with an id of its own), so what it still had to say
//! about an earlier one finds nothing.
//!
//! [`Cx::hide_window`]: crate::Cx::hide_window

use kurbo::{Point, Rect, Size};
use sidestep_engine::protocol::{self, PopupLayout, PopupPlacement, SizeLimits, Style, ToRender, WindowRequest};
use smithay_client_toolkit::reexports::calloop::channel::Sender;

use crate::color::Color;
use crate::event::WindowState;

/// A pointer's appearance, named as cursor themes (and
/// wp_cursor_shape_v1) name them.
pub use sidestep_engine::protocol::Cursor;

/// A window, as long as it's open. Ids aren't reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub(crate) u32);

/// What a window shows where nothing is drawn: every pass starts by
/// clearing what it redraws to it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Background {
    /// The desktop's window background, light or dark as it is.
    #[default]
    System,
    Color(Color),
    /// Nothing: the window is see-through where it draws nothing.
    Transparent,
}

/// A corner of a rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    fn protocol(self) -> protocol::Corner {
        match self {
            Corner::TopLeft => protocol::Corner::TopLeft,
            Corner::TopRight => protocol::Corner::TopRight,
            Corner::BottomLeft => protocol::Corner::BottomLeft,
            Corner::BottomRight => protocol::Corner::BottomRight,
        }
    }
}

/// Where a popup opens against its anchor, as the compositor places it
/// (xdg_positioner): from the anchor rectangle's `anchor` corner, growing
/// toward `gravity`, moved by `offset` points; and what the compositor may
/// do where it wouldn't fit on the output: flip it to the anchor's other
/// side, slide it along the edge, or shrink it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PopupPosition {
    pub anchor: Corner,
    pub gravity: Corner,
    pub offset: (i32, i32),
    pub flip_x: bool,
    pub flip_y: bool,
    pub slide_x: bool,
    pub slide_y: bool,
    pub resize_x: bool,
    pub resize_y: bool,
}

impl PopupPosition {
    /// Below the anchor, its left edges lined up, flipping above it where
    /// there's no room below and sliding along the output's edges: a menu
    /// bar's menu, a completion list. The default.
    pub const BELOW: PopupPosition = PopupPosition {
        anchor: Corner::BottomLeft,
        gravity: Corner::BottomRight,
        offset: (0, 0),
        flip_x: false,
        flip_y: true,
        slide_x: true,
        slide_y: true,
        resize_x: false,
        resize_y: false,
    };

    /// Its top left at the anchor's top left, giving way at the edges and
    /// shrinking to fit: a context menu at a point (a 1 × 1 anchor).
    pub const AT_POINT: PopupPosition =
        PopupPosition { anchor: Corner::TopLeft, resize_y: true, ..PopupPosition::BELOW };

    /// To the anchor's right, its top at the anchor's top, flipping to the
    /// left where there's no room: a submenu beside its item.
    pub const BESIDE: PopupPosition =
        PopupPosition { anchor: Corner::TopRight, flip_x: true, flip_y: false, slide_x: false, ..PopupPosition::BELOW };

    pub fn offset(mut self, x: i32, y: i32) -> PopupPosition {
        self.offset = (x, y);
        self
    }

    fn layout(self) -> PopupLayout {
        PopupLayout {
            corner: self.anchor.protocol(),
            gravity: self.gravity.protocol(),
            offset: self.offset,
            flip_x: self.flip_x,
            flip_y: self.flip_y,
            slide_x: self.slide_x,
            slide_y: self.slide_y,
            resize_x: self.resize_x,
            resize_y: self.resize_y,
        }
    }
}

impl Default for PopupPosition {
    fn default() -> PopupPosition {
        PopupPosition::BELOW
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    Toplevel { parent: Option<WindowId> },
    Popup { parent: WindowId, anchor: Rect, grab: bool, position: PopupPosition },
}

/// What a window is asked to be when it opens.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowOptions {
    title: String,
    size: Size,
    min_size: Option<Size>,
    max_size: Option<Size>,
    decorated: bool,
    resizable: bool,
    closable: bool,
    minimizable: bool,
    passthrough: bool,
    close_on_request: bool,
    visible: bool,
    background: Background,
    kind: Kind,
}

impl WindowOptions {
    /// A titled window of 640 by 480 points, which the user can move,
    /// resize, minimize and close.
    pub fn new(title: impl Into<String>) -> WindowOptions {
        WindowOptions {
            title: title.into(),
            size: Size::new(640.0, 480.0),
            min_size: None,
            max_size: None,
            decorated: true,
            resizable: true,
            closable: true,
            minimizable: true,
            passthrough: false,
            close_on_request: true,
            visible: true,
            background: Background::System,
            kind: Kind::Toplevel { parent: None },
        }
    }

    /// A popup (a menu, a tooltip, a completion list): a borderless window
    /// over `parent`, opening below `anchor` (a rectangle of the parent's
    /// content) where it fits, as the compositor places it (see
    /// [`popup_position`](WindowOptions::popup_position) for elsewhere).
    /// With `grab` it takes the keyboard and pointer while open, as menus
    /// do, and the compositor dismisses it at a click elsewhere.
    pub fn popup(parent: WindowId, anchor: Rect, size: Size, grab: bool) -> WindowOptions {
        WindowOptions {
            size,
            decorated: false,
            resizable: false,
            close_on_request: true,
            kind: Kind::Popup { parent, anchor, grab, position: PopupPosition::BELOW },
            ..WindowOptions::new("")
        }
    }

    /// Where a popup opens against its anchor. (Nothing for a toplevel.)
    pub fn popup_position(mut self, position: PopupPosition) -> Self {
        if let Kind::Popup { position: p, .. } = &mut self.kind {
            *p = position;
        }
        self
    }

    /// The content's size, in points.
    pub fn size(mut self, width: f64, height: f64) -> Self {
        self.size = Size::new(width, height);
        self
    }

    pub fn min_size(mut self, width: f64, height: f64) -> Self {
        self.min_size = Some(Size::new(width, height));
        self
    }

    pub fn max_size(mut self, width: f64, height: f64) -> Self {
        self.max_size = Some(Size::new(width, height));
        self
    }

    /// With a title bar (the default); without, nothing is drawn around
    /// the content and the compositor is asked to draw nothing either.
    pub fn decorated(mut self, decorated: bool) -> Self {
        self.decorated = decorated;
        self
    }

    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    pub fn minimizable(mut self, minimizable: bool) -> Self {
        self.minimizable = minimizable;
        self
    }

    /// Let pointer input through the window to whatever is behind it.
    pub fn passthrough(mut self, passthrough: bool) -> Self {
        self.passthrough = passthrough;
        self
    }

    /// Close the window when the user asks to (the default), after the
    /// handler hears [`CloseRequested`](crate::WindowEvent::CloseRequested);
    /// without, the handler decides (`Cx::close_window`).
    pub fn close_on_request(mut self, close: bool) -> Self {
        self.close_on_request = close;
        self
    }

    /// Open shown (the default), or hidden until
    /// [`Cx::show_window`](crate::Cx::show_window).
    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn background(mut self, background: Background) -> Self {
        self.background = background;
        self
    }

    /// Keep the window over `parent` (a dialog's document window).
    pub fn transient_for(mut self, parent: WindowId) -> Self {
        if let Kind::Toplevel { parent: p } = &mut self.kind {
            *p = Some(parent);
        }
        self
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.visible
    }

    fn style(&self) -> Style {
        Style {
            titled: self.decorated,
            closable: self.closable,
            miniaturizable: self.minimizable,
            resizable: self.resizable,
            movable: true,
            passthrough: self.passthrough,
        }
    }

    fn limits(&self) -> SizeLimits {
        let points =
            |s: Option<Size>| s.map_or((0, 0), |s| (s.width.round().max(0.0) as u32, s.height.round().max(0.0) as u32));
        SizeLimits { min: points(self.min_size), max: points(self.max_size) }
    }
}

/// What the toolkit keeps of an open window.
pub(crate) struct WindowData {
    pub id: WindowId,
    /// The render thread's id for the current showing, while shown, and
    /// whether it was ever shown (its first showing has its own number).
    pub showing: Option<u32>,
    pub shown_before: bool,
    pub title: String,
    pub style: Style,
    pub limits: SizeLimits,
    pub background: Background,
    pub close_on_request: bool,
    pub kind: Kind,
    pub size: Size,
    pub scale: f64,
    pub state: WindowState,
    /// The height of the title bar the render thread draws, 0 for none.
    pub titlebar: u32,
    pub configured: bool,
    /// A frame was presented and the render thread hasn't shown it yet.
    pub frame_pending: bool,
    /// What the next pass redraws, in points of the content.
    pub damage: Vec<protocol::Rect>,
    /// The title goes to the render thread at the next pass.
    pub title_dirty: bool,
    pub focused: bool,
    /// The pointer is over the content.
    pub pointer_inside: bool,
    pub outputs: Vec<u32>,
    pub cursor: Cursor,
    pub text_input: (bool, Option<Rect>),
}

/// The most rectangles a window's damage keeps before they become their
/// union.
const MAX_DAMAGE: usize = 16;

impl WindowData {
    /// What the toolkit keeps of a window, not yet shown.
    pub fn new(id: WindowId, options: &WindowOptions) -> WindowData {
        WindowData {
            id,
            showing: None,
            shown_before: false,
            title: options.title.clone(),
            style: options.style(),
            limits: options.limits(),
            background: options.background,
            close_on_request: options.close_on_request,
            kind: options.kind.clone(),
            size: options.size,
            scale: 1.0,
            state: WindowState::default(),
            titlebar: 0,
            configured: false,
            frame_pending: false,
            damage: Vec::new(),
            title_dirty: true,
            focused: false,
            pointer_inside: false,
            outputs: Vec::new(),
            cursor: Cursor::Default,
            text_input: (false, None),
        }
    }

    /// The window a popup is part of.
    pub fn popup_of(&self) -> Option<WindowId> {
        match self.kind {
            Kind::Popup { parent, .. } => Some(parent),
            Kind::Toplevel { .. } => None,
        }
    }

    /// The window a toplevel is kept over.
    pub fn transient_for(&self) -> Option<WindowId> {
        match self.kind {
            Kind::Toplevel { parent } => parent,
            Kind::Popup { .. } => None,
        }
    }

    /// Show the window on the render thread as showing `showing`; `parent`
    /// is the showing of the window it's a popup of or kept over, if that
    /// is shown.
    pub fn show(&mut self, showing: u32, parent: Option<u32>, tx: &Sender<ToRender>) {
        let popup = match self.kind {
            Kind::Popup { anchor, grab, position, .. } => Some(PopupPlacement {
                parent: parent.unwrap_or(0),
                anchor: protocol::Rect::new(anchor.x0 as f32, anchor.y0 as f32, anchor.x1 as f32, anchor.y1 as f32),
                below: true,
                grab,
                layout: (position != PopupPosition::BELOW).then(|| position.layout()),
            }),
            Kind::Toplevel { .. } => None,
        };
        send(
            tx,
            ToRender::CreateWindow {
                window: showing,
                width: self.size.width.round().max(1.0) as u32,
                height: self.size.height.round().max(1.0) as u32,
                title: self.title.clone(),
                style: self.style,
                limits: self.limits,
                popup,
                sheet_of: None,
            },
        );
        if let (Kind::Toplevel { parent: Some(_) }, Some(parent)) = (&self.kind, parent) {
            send(tx, ToRender::SetParent { window: showing, parent: Some(parent) });
        }
        // A new showing knows nothing of the pointer's appearance or of
        // input methods.
        if self.cursor != Cursor::Default {
            send(tx, ToRender::SetCursor { window: showing, cursor: self.cursor });
        }
        self.text_input = (false, None);
        self.showing = Some(showing);
        self.configured = false;
        self.frame_pending = false;
        self.damage.clear();
        self.title_dirty = true;
        self.titlebar = 0;
        self.outputs.clear();
    }

    /// Take the window off the screen.
    pub fn hide(&mut self, tx: &Sender<ToRender>) {
        if let Some(showing) = self.showing.take() {
            send(tx, ToRender::CloseWindow { window: showing });
        }
        self.configured = false;
        self.frame_pending = false;
        self.damage.clear();
        self.focused = false;
        self.pointer_inside = false;
    }

    pub fn bounds(&self) -> protocol::Rect {
        protocol::Rect::new(0.0, 0.0, self.size.width as f32, self.size.height as f32)
    }

    /// Redraw `r` (content points) at the next pass.
    pub fn invalidate(&mut self, r: protocol::Rect) {
        let r = r.round_out().intersect(&self.bounds());
        if r.is_empty() || self.damage.iter().any(|d| d.intersect(&r) == r) {
            return;
        }
        self.damage.retain(|d| r.intersect(d) != *d);
        self.damage.push(r);
        if self.damage.len() > MAX_DAMAGE {
            let all = self.damage.iter().fold(r, |a, d| a.union(d));
            self.damage = vec![all];
        }
    }

    pub fn invalidate_all(&mut self) {
        self.damage = vec![self.bounds()];
    }

    /// Whether the next pass has anything to send.
    pub fn wants_pass(&self) -> bool {
        self.showing.is_some()
            && self.configured
            && !self.frame_pending
            && (!self.damage.is_empty() || self.title_dirty)
    }
}

pub(crate) fn send(tx: &Sender<ToRender>, msg: ToRender) {
    sidestep_engine::backend::null::sending();
    let _ = tx.send(msg);
}

/// An open window, to ask things of: from [`Cx::window`](crate::Cx::window).
/// What it asks of the compositor applies while the window is shown;
/// asked of a hidden window, it waits for the next showing where it can
/// (the title, size limits, the cursor) and is dropped otherwise.
pub struct Window<'a> {
    pub(crate) data: &'a mut WindowData,
    pub(crate) tx: &'a Sender<ToRender>,
}

impl Window<'_> {
    pub fn id(&self) -> WindowId {
        self.data.id
    }

    /// Whether the window is on the screen (not hidden).
    pub fn is_visible(&self) -> bool {
        self.data.showing.is_some()
    }

    /// The content's size, in points.
    pub fn size(&self) -> Size {
        self.data.size
    }

    /// Device pixels per point.
    pub fn scale(&self) -> f64 {
        self.data.scale
    }

    pub fn state(&self) -> WindowState {
        self.data.state
    }

    /// Whether the window has the keyboard.
    pub fn is_focused(&self) -> bool {
        self.data.focused
    }

    pub fn title(&self) -> &str {
        &self.data.title
    }

    /// The outputs (screens, by their [`Output::id`](crate::Output)) the
    /// window is on, in the order it entered them.
    pub fn outputs(&self) -> &[u32] {
        &self.data.outputs
    }

    pub fn set_title(&mut self, title: impl Into<String>) {
        let title = title.into();
        if title != self.data.title {
            self.data.title = title;
            self.data.title_dirty = true;
        }
    }

    /// Draw the whole window again at the next pass.
    pub fn redraw(&mut self) {
        self.data.invalidate_all();
    }

    /// Draw `rect` (content points) again at the next pass.
    pub fn invalidate(&mut self, rect: Rect) {
        self.data.invalidate(protocol::Rect::new(rect.x0 as f32, rect.y0 as f32, rect.x1 as f32, rect.y1 as f32));
    }

    /// The pointer's appearance over the window's content.
    pub fn set_cursor(&mut self, cursor: Cursor) {
        if self.data.cursor != cursor {
            self.data.cursor = cursor;
            if let Some(showing) = self.data.showing {
                send(self.tx, ToRender::SetCursor { window: showing, cursor });
            }
        }
    }

    pub fn set_min_size(&mut self, size: Option<Size>) {
        self.data.limits.min = size.map_or((0, 0), |s| (s.width.round() as u32, s.height.round() as u32));
        if let Some(showing) = self.data.showing {
            send(self.tx, ToRender::SetSizeLimits { window: showing, limits: self.data.limits });
        }
    }

    pub fn set_max_size(&mut self, size: Option<Size>) {
        self.data.limits.max = size.map_or((0, 0), |s| (s.width.round() as u32, s.height.round() as u32));
        if let Some(showing) = self.data.showing {
            send(self.tx, ToRender::SetSizeLimits { window: showing, limits: self.data.limits });
        }
    }

    pub fn set_resizable(&mut self, resizable: bool) {
        self.data.style.resizable = resizable;
        if let Some(showing) = self.data.showing {
            send(self.tx, ToRender::SetStyle { window: showing, style: self.data.style });
        }
    }

    fn request(&mut self, request: WindowRequest) {
        if let Some(showing) = self.data.showing {
            send(self.tx, ToRender::Request { window: showing, request });
        }
    }

    /// Ask the compositor for a new content size, in points. A hidden
    /// window shows at it next time.
    pub fn request_size(&mut self, size: Size) {
        if self.data.showing.is_none() {
            self.data.size = Size::new(size.width.round().max(1.0), size.height.round().max(1.0));
        }
        self.request(WindowRequest::Resize(size.width.round().max(1.0) as u32, size.height.round().max(1.0) as u32));
    }

    pub fn minimize(&mut self) {
        self.request(WindowRequest::Minimize);
    }

    pub fn set_maximized(&mut self, maximized: bool) {
        self.request(WindowRequest::Maximize(maximized));
    }

    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        self.request(WindowRequest::Fullscreen(fullscreen));
    }

    /// Ask for the keyboard (xdg-activation): compositors grant it to a
    /// window the user just used, and may only mark others as wanting
    /// attention.
    pub fn activate(&mut self) {
        self.request(WindowRequest::Activate);
    }

    /// Move the window with the pointer, from the button press under way:
    /// for a window that draws its own title bar or moves by its
    /// background.
    pub fn begin_move(&mut self) {
        self.request(WindowRequest::Move);
    }

    /// Take text from input methods (or stop), with the caret where a
    /// candidate window should go, in content points. What they type
    /// arrives as [`Ime`](crate::Ime) events; keys still arrive as keys.
    pub fn set_text_input(&mut self, wanted: bool, caret: Option<Rect>) {
        if self.data.text_input == (wanted, caret) {
            return;
        }
        self.data.text_input = (wanted, caret);
        let caret = caret.map(|r| protocol::Rect::new(r.x0 as f32, r.y0 as f32, r.x1 as f32, r.y1 as f32));
        if let Some(showing) = self.data.showing {
            send(self.tx, ToRender::TextInput { window: showing, wanted, caret });
        }
    }

    /// The program dropped the text being composed: the input method
    /// starts over.
    pub fn reset_text_input(&mut self) {
        if let Some(showing) = self.data.showing {
            send(self.tx, ToRender::ResetTextInput { window: showing });
        }
    }

    /// Where the window's content is in its own coordinates: (0, 0) to its
    /// size.
    pub fn bounds(&self) -> Rect {
        Rect::from_origin_size(Point::ZERO, self.data.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_positions_name_xdg_placements() {
        let below = PopupPosition::BELOW.layout();
        assert_eq!((below.corner, below.gravity), (protocol::Corner::BottomLeft, protocol::Corner::BottomRight));
        assert!(below.flip_y && below.slide_x && !below.resize_y);
        let beside = PopupPosition::BESIDE.offset(-1, -4).layout();
        assert_eq!(beside.corner, protocol::Corner::TopRight);
        assert!(beside.flip_x && !beside.flip_y && !beside.slide_x);
        assert_eq!(beside.offset, (-1, -4));
        assert!(PopupPosition::AT_POINT.layout().resize_y);
    }
}
