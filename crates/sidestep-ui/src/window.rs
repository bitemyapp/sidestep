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

use kurbo::{Point, Rect, Size};
use sidestep_engine::protocol::{self, PopupPlacement, SizeLimits, Style, ToRender, WindowRequest};
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

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Toplevel { parent: Option<WindowId> },
    Popup { parent: WindowId, anchor: Rect, grab: bool },
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
            background: Background::System,
            kind: Kind::Toplevel { parent: None },
        }
    }

    /// A popup (a menu, a tooltip, a completion list): a borderless window
    /// over `parent`, opening below `anchor` (a rectangle of the parent's
    /// content) where it fits, as the compositor places it. With `grab` it
    /// takes the keyboard and pointer while open, as menus do, and the
    /// compositor dismisses it at a click elsewhere.
    pub fn popup(parent: WindowId, anchor: Rect, size: Size, grab: bool) -> WindowOptions {
        WindowOptions {
            size,
            decorated: false,
            resizable: false,
            close_on_request: true,
            kind: Kind::Popup { parent, anchor, grab },
            ..WindowOptions::new("")
        }
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
    pub title: String,
    pub style: Style,
    pub limits: SizeLimits,
    pub background: Background,
    pub close_on_request: bool,
    /// The window a popup is part of.
    pub popup_of: Option<WindowId>,
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
    pub outputs: Vec<u32>,
    pub cursor: Cursor,
    pub text_input: (bool, Option<Rect>),
}

/// The most rectangles a window's damage keeps before they become their
/// union.
const MAX_DAMAGE: usize = 16;

impl WindowData {
    /// Open a window on the render thread.
    pub fn open(id: WindowId, options: &WindowOptions, tx: &Sender<ToRender>) -> WindowData {
        let popup = match options.kind {
            Kind::Popup { parent, anchor, grab } => Some(PopupPlacement {
                parent: parent.0,
                anchor: protocol::Rect::new(anchor.x0 as f32, anchor.y0 as f32, anchor.x1 as f32, anchor.y1 as f32),
                below: true,
                grab,
                layout: None,
            }),
            Kind::Toplevel { .. } => None,
        };
        let style = options.style();
        let limits = options.limits();
        send(
            tx,
            ToRender::CreateWindow {
                window: id.0,
                width: options.size.width.round().max(1.0) as u32,
                height: options.size.height.round().max(1.0) as u32,
                title: options.title.clone(),
                style,
                limits,
                popup,
                sheet_of: None,
            },
        );
        if let Kind::Toplevel { parent: Some(parent) } = options.kind {
            send(tx, ToRender::SetParent { window: id.0, parent: Some(parent.0) });
        }
        WindowData {
            id,
            title: options.title.clone(),
            style,
            limits,
            background: options.background,
            close_on_request: options.close_on_request,
            popup_of: match options.kind {
                Kind::Popup { parent, .. } => Some(parent),
                Kind::Toplevel { .. } => None,
            },
            size: options.size,
            scale: 1.0,
            state: WindowState::default(),
            titlebar: 0,
            configured: false,
            frame_pending: false,
            damage: Vec::new(),
            title_dirty: true,
            focused: false,
            outputs: Vec::new(),
            cursor: Cursor::Default,
            text_input: (false, None),
        }
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
        self.configured && !self.frame_pending && (!self.damage.is_empty() || self.title_dirty)
    }
}

pub(crate) fn send(tx: &Sender<ToRender>, msg: ToRender) {
    sidestep_engine::backend::null::sending();
    let _ = tx.send(msg);
}

/// An open window, to ask things of: from [`Cx::window`](crate::Cx::window).
pub struct Window<'a> {
    pub(crate) data: &'a mut WindowData,
    pub(crate) tx: &'a Sender<ToRender>,
}

impl Window<'_> {
    pub fn id(&self) -> WindowId {
        self.data.id
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
            send(self.tx, ToRender::SetCursor { window: self.data.id.0, cursor });
        }
    }

    pub fn set_min_size(&mut self, size: Option<Size>) {
        self.data.limits.min = size.map_or((0, 0), |s| (s.width.round() as u32, s.height.round() as u32));
        send(self.tx, ToRender::SetSizeLimits { window: self.data.id.0, limits: self.data.limits });
    }

    pub fn set_max_size(&mut self, size: Option<Size>) {
        self.data.limits.max = size.map_or((0, 0), |s| (s.width.round() as u32, s.height.round() as u32));
        send(self.tx, ToRender::SetSizeLimits { window: self.data.id.0, limits: self.data.limits });
    }

    pub fn set_resizable(&mut self, resizable: bool) {
        self.data.style.resizable = resizable;
        send(self.tx, ToRender::SetStyle { window: self.data.id.0, style: self.data.style });
    }

    fn request(&mut self, request: WindowRequest) {
        send(self.tx, ToRender::Request { window: self.data.id.0, request });
    }

    /// Ask the compositor for a new content size, in points.
    pub fn request_size(&mut self, size: Size) {
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
        send(self.tx, ToRender::TextInput { window: self.data.id.0, wanted, caret });
    }

    /// The program dropped the text being composed: the input method
    /// starts over.
    pub fn reset_text_input(&mut self) {
        send(self.tx, ToRender::ResetTextInput { window: self.data.id.0 });
    }

    /// Where the window's content is in its own coordinates: (0, 0) to its
    /// size.
    pub fn bounds(&self) -> Rect {
        Rect::from_origin_size(Point::ZERO, self.data.size)
    }
}
