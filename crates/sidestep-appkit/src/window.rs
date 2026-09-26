//! `NSWindow`: routes input to views, keeps the window's state, and runs
//! the display pass.
//!
//! Input: mouse downs go to the view under the pointer, which becomes the
//! first responder if it accepts; drags and ups go to the view that got the
//! down; keys and modifier changes go to the first responder, which is the
//! window itself until a view takes over, and unhandled ones travel up the
//! responder chain.
//!
//! State: Wayland decides a window's size, whether it's maximized or
//! fullscreen, and which window has the keyboard. The render thread reports
//! each change and this module turns them into AppKit's state (`frame`,
//! `isZoomed`, `isKeyWindow`, `backingScaleFactor`) and delegate calls.
//! Requests (`zoom:`, `setContentSize:`, `makeKeyWindow`) are asks the
//! compositor may decline. Wayland doesn't tell a window where it is, so
//! `frame` keeps the origin it was given.
//!
//! Invalidation collects damage per layer, in layer points. When the render
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

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, MessageReceiver, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSClipView, NSColor, NSCursor, NSEvent, NSEventMask, NSEventPhase, NSEventType, NSFont,
    NSFontAttributeName, NSForegroundColorAttributeName, NSResponder, NSStringDrawing, NSView, NSWindow,
    NSWindowAnimationBehavior, NSWindowButton, NSWindowCollectionBehavior, NSWindowOcclusionState,
    NSWindowOrderingMode, NSWindowSharingType, NSWindowStyleMask, NSWindowTabbingMode, NSWindowTitleVisibility,
};
use objc2_foundation::{NSCopying, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use sidestep_foundation::notification;

use crate::app;
use crate::graphics::{self, Xf};
use crate::protocol::{
    Button, Cursor, LayerId, Modifiers, Op, PopupPlacement, ROOT_LAYER, Rect, ScrollPhase, SizeLimits, Style,
    TILE_HEIGHT, TitleText, ToRender, WindowId, WindowRequest, WindowState,
};

/// Where a scroll is in a touchpad gesture, the speed an ending one
/// leaves, and whether the device's direction is reversed.
pub(crate) type Gesture = (ScrollPhase, (f64, f64), bool);
use crate::tracking;
use crate::views::{self, NSViewImpl};

/// The window background, and what layers are cleared to before drawing.
const BACKGROUND: [f32; 4] = [0.925, 0.925, 0.925, 1.0];

thread_local!(static AUTOMATIC_TABBING: Cell<bool> = const { Cell::new(true) });

/// What a window's views are drawn over: its background color, if one was
/// set.
fn background(window: &NSWindowImpl) -> [f32; 4] {
    match &window.ivars().settings.borrow().background {
        Some(c) => [c.redComponent(), c.greenComponent(), c.blueComponent(), c.alphaComponent()].map(|v| v as f32),
        None => BACKGROUND,
    }
}

/// Height of the title bar Sidestep draws, when it draws one.
const HEADER: f64 = crate::backend::HEADER as f64;

/// No size limit, as AppKit reports one.
const UNLIMITED: f64 = f32::MAX as f64;

pub(crate) struct WindowIvars {
    /// `windowNumber`, for the window's life.
    number: WindowId,
    /// What the render thread calls the window while it's on screen: a new
    /// id each time it's shown, so messages about an earlier showing still
    /// on their way find no window.
    showing: Cell<WindowId>,
    title: RefCell<Retained<NSString>>,
    /// The title bar needs the title set again.
    title_dirty: Cell<bool>,
    style: Cell<NSWindowStyleMask>,
    /// The origin `frame` reports.
    origin: Cell<NSPoint>,
    /// The content size, in points.
    size: Cell<NSSize>,
    /// Height of the title bar drawn around the content, once configured.
    titlebar: Cell<Option<f64>>,
    scale: Cell<f64>,
    state: Cell<WindowState>,
    content_min: Cell<NSSize>,
    content_max: Cell<NSSize>,
    content: RefCell<Option<Retained<NSView>>>,
    /// None means the window itself.
    first_responder: RefCell<Option<Retained<NSResponder>>>,
    /// Weak, as AppKit's is.
    delegate: RefCell<Option<Weak<AnyObject>>>,
    released_when_closed: Cell<bool>,
    visible: Cell<bool>,
    key: Cell<bool>,
    main: Cell<bool>,
    miniaturized: Cell<bool>,
    accepts_mouse_moved: Cell<bool>,
    /// Shown as a popup of another window rather than as a toplevel.
    popup: Cell<Option<PopupPlacement>>,
    /// The parent, which holds this window among its children.
    parent: Cell<Option<NonNull<NSWindow>>>,
    children: RefCell<Vec<Retained<NSWindow>>>,
    configured: Cell<bool>,
    /// A frame was presented and the render thread hasn't shown it yet.
    frame_pending: Cell<bool>,
    needs_display: Cell<bool>,
    damage: RefCell<HashMap<LayerId, Vec<Rect>>>,
    clips: RefCell<Vec<Retained<NSView>>>,
    layers: RefCell<HashMap<LayerId, LayerState>>,
    /// The view that got the last mouse down, for the drags and up after
    /// it, and the button that went down.
    mouse_view: RefCell<Option<(Retained<NSView>, Button)>>,
    /// Where the pointer last was, in window coordinates.
    pointer: Cell<Option<NSPoint>>,
    settings: RefCell<Settings>,
    tracking: RefCell<tracking::WindowTracking>,
    /// What the render thread was last told about input methods: whether
    /// the first responder takes their text, and where its caret is.
    text_input: Cell<(bool, Option<Rect>)>,
    /// A pinch under way has turned: rotate events are being sent.
    rotating: Cell<bool>,
    /// The window this one belongs over, for the compositor to keep it
    /// there.
    transient: RefCell<Option<Weak<NSWindow>>>,
}

/// Settings a window keeps for programs that set them. Only the background
/// color changes what shows; the rest are the compositor's business on
/// Wayland (levels, shadows, tabbing) or wait for a use.
struct Settings {
    background: Option<Retained<NSColor>>,
    opaque: bool,
    has_shadow: bool,
    alpha: f64,
    title_visibility: NSWindowTitleVisibility,
    titlebar_transparent: bool,
    movable_by_background: bool,
    level: isize,
    collection: NSWindowCollectionBehavior,
    tabbing: NSWindowTabbingMode,
    ignores_mouse: bool,
    autosave_name: Retained<NSString>,
    /// Weak, as AppKit's is.
    initial_first_responder: Option<Weak<NSView>>,
    movable: bool,
    edited: bool,
    subtitle: Retained<NSString>,
    hides_on_deactivate: bool,
    restorable: bool,
    excluded_from_windows_menu: bool,
    can_hide: bool,
    tabbing_identifier: Retained<NSString>,
    preserves_content: bool,
    animation_behavior: NSWindowAnimationBehavior,
    sharing_type: NSWindowSharingType,
    concurrent_drawing: bool,
    displays_when_profile_changes: bool,
    recalculates_key_view_loop: bool,
    /// None: the title.
    miniwindow_title: Option<Retained<NSString>>,
    represented_filename: Retained<NSString>,
    resize_increments: NSSize,
    content_resize_increments: NSSize,
    aspect_ratio: NSSize,
    content_aspect_ratio: NSSize,
    /// The window has been on screen, and the initial first responder had
    /// its turn.
    shown: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            background: None,
            opaque: true,
            has_shadow: true,
            alpha: 1.0,
            title_visibility: NSWindowTitleVisibility::Visible,
            titlebar_transparent: false,
            movable_by_background: false,
            level: 0,
            collection: NSWindowCollectionBehavior::Default,
            tabbing: NSWindowTabbingMode::Automatic,
            ignores_mouse: false,
            autosave_name: NSString::new(),
            initial_first_responder: None,
            movable: true,
            edited: false,
            subtitle: NSString::new(),
            hides_on_deactivate: false,
            restorable: true,
            excluded_from_windows_menu: false,
            can_hide: true,
            tabbing_identifier: NSString::new(),
            preserves_content: true,
            animation_behavior: NSWindowAnimationBehavior::Default,
            sharing_type: NSWindowSharingType::ReadOnly,
            concurrent_drawing: true,
            displays_when_profile_changes: false,
            recalculates_key_view_loop: false,
            miniwindow_title: None,
            represented_filename: NSString::new(),
            resize_increments: NSSize::new(1.0, 1.0),
            content_resize_increments: NSSize::new(1.0, 1.0),
            aspect_ratio: NSSize::new(0.0, 0.0),
            content_aspect_ratio: NSSize::new(0.0, 0.0),
            shown: false,
        }
    }
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
            style: NSWindowStyleMask,
            _backing: NSBackingStoreType,
            _defer: bool,
        ) -> Retained<Self> {
            app::load_shells();
            static NEXT_NUMBER: AtomicU32 = AtomicU32::new(1);
            let this = this.set_ivars(WindowIvars {
                number: NEXT_NUMBER.fetch_add(1, Ordering::Relaxed),
                showing: Cell::new(0),
                title: RefCell::new(NSString::new()),
                title_dirty: Cell::new(true),
                style: Cell::new(style),
                origin: Cell::new(rect.origin),
                size: Cell::new(rect.size),
                titlebar: Cell::new(None),
                scale: Cell::new(1.0),
                state: Cell::new(WindowState::default()),
                content_min: Cell::new(NSSize::ZERO),
                content_max: Cell::new(NSSize::new(UNLIMITED, UNLIMITED)),
                content: RefCell::new(None),
                first_responder: RefCell::new(None),
                delegate: RefCell::new(None),
                released_when_closed: Cell::new(true),
                visible: Cell::new(false),
                key: Cell::new(false),
                main: Cell::new(false),
                miniaturized: Cell::new(false),
                accepts_mouse_moved: Cell::new(false),
                popup: Cell::new(None),
                parent: Cell::new(None),
                children: RefCell::new(Vec::new()),
                configured: Cell::new(false),
                frame_pending: Cell::new(false),
                needs_display: Cell::new(false),
                damage: RefCell::new(HashMap::new()),
                clips: RefCell::new(Vec::new()),
                layers: RefCell::new(HashMap::new()),
                mouse_view: RefCell::new(None),
                pointer: Cell::new(None),
                settings: RefCell::new(Settings::default()),
                tracking: RefCell::default(),
                text_input: Cell::new((false, None)),
                rotating: Cell::new(false),
                transient: RefCell::new(None),
            });
            // SAFETY: NSResponder's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            app::window_made(as_window(&this));
            this
        }

        #[unsafe(method_id(backgroundColor))]
        fn background_color(&self) -> Retained<NSColor> {
            let set = self.ivars().settings.borrow().background.clone();
            set.unwrap_or_else(NSColor::windowBackgroundColor)
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: Option<&NSColor>) {
            self.ivars().settings.borrow_mut().background = color.map(|c| c.retain());
            self.damage_all();
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            self.ivars().settings.borrow().opaque
        }

        #[unsafe(method(setOpaque:))]
        fn set_opaque(&self, flag: bool) {
            self.ivars().settings.borrow_mut().opaque = flag;
        }

        #[unsafe(method(hasShadow))]
        fn has_shadow(&self) -> bool {
            self.ivars().settings.borrow().has_shadow
        }

        #[unsafe(method(setHasShadow:))]
        fn set_has_shadow(&self, flag: bool) {
            self.ivars().settings.borrow_mut().has_shadow = flag;
        }

        #[unsafe(method(invalidateShadow))]
        fn invalidate_shadow(&self) {}

        #[unsafe(method(alphaValue))]
        fn alpha_value(&self) -> f64 {
            self.ivars().settings.borrow().alpha
        }

        #[unsafe(method(setAlphaValue:))]
        fn set_alpha_value(&self, alpha: f64) {
            self.ivars().settings.borrow_mut().alpha = alpha;
        }

        #[unsafe(method(titleVisibility))]
        fn title_visibility(&self) -> NSWindowTitleVisibility {
            self.ivars().settings.borrow().title_visibility
        }

        #[unsafe(method(setTitleVisibility:))]
        fn set_title_visibility(&self, visibility: NSWindowTitleVisibility) {
            self.ivars().settings.borrow_mut().title_visibility = visibility;
            self.ivars().title_dirty.set(true);
            self.ivars().needs_display.set(true);
        }

        #[unsafe(method(titlebarAppearsTransparent))]
        fn titlebar_appears_transparent(&self) -> bool {
            self.ivars().settings.borrow().titlebar_transparent
        }

        #[unsafe(method(setTitlebarAppearsTransparent:))]
        fn set_titlebar_appears_transparent(&self, flag: bool) {
            self.ivars().settings.borrow_mut().titlebar_transparent = flag;
        }

        #[unsafe(method(isMovableByWindowBackground))]
        fn is_movable_by_window_background(&self) -> bool {
            self.ivars().settings.borrow().movable_by_background
        }

        #[unsafe(method(setMovableByWindowBackground:))]
        fn set_movable_by_window_background(&self, flag: bool) {
            self.ivars().settings.borrow_mut().movable_by_background = flag;
        }

        #[unsafe(method(level))]
        fn level(&self) -> isize {
            self.ivars().settings.borrow().level
        }

        #[unsafe(method(setLevel:))]
        fn set_level(&self, level: isize) {
            self.ivars().settings.borrow_mut().level = level;
        }

        #[unsafe(method(collectionBehavior))]
        fn collection_behavior(&self) -> NSWindowCollectionBehavior {
            self.ivars().settings.borrow().collection
        }

        #[unsafe(method(setCollectionBehavior:))]
        fn set_collection_behavior(&self, behavior: NSWindowCollectionBehavior) {
            self.ivars().settings.borrow_mut().collection = behavior;
        }

        #[unsafe(method(tabbingMode))]
        fn tabbing_mode(&self) -> NSWindowTabbingMode {
            self.ivars().settings.borrow().tabbing
        }

        #[unsafe(method(setTabbingMode:))]
        fn set_tabbing_mode(&self, mode: NSWindowTabbingMode) {
            self.ivars().settings.borrow_mut().tabbing = mode;
        }

        #[unsafe(method(ignoresMouseEvents))]
        fn ignores_mouse_events(&self) -> bool {
            self.ivars().settings.borrow().ignores_mouse
        }

        #[unsafe(method(setIgnoresMouseEvents:))]
        fn set_ignores_mouse_events(&self, flag: bool) {
            let was = std::mem::replace(&mut self.ivars().settings.borrow_mut().ignores_mouse, flag);
            if was != flag {
                self.send_style();
            }
        }

        #[unsafe(method_id(frameAutosaveName))]
        fn frame_autosave_name(&self) -> Retained<NSString> {
            self.ivars().settings.borrow().autosave_name.clone()
        }

        #[unsafe(method(setFrameAutosaveName:))]
        fn set_frame_autosave_name(&self, name: &NSString) -> bool {
            self.ivars().settings.borrow_mut().autosave_name = name.copy();
            true
        }

        #[unsafe(method(setFrameUsingName:))]
        fn set_frame_using_name(&self, _name: &NSString) -> bool {
            // Nothing is saved: Wayland places windows itself.
            false
        }

        #[unsafe(method(saveFrameUsingName:))]
        fn save_frame_using_name(&self, _name: &NSString) {}

        #[unsafe(method_id(initialFirstResponder))]
        fn initial_first_responder(&self) -> Option<Retained<NSView>> {
            self.ivars().settings.borrow().initial_first_responder.as_ref().and_then(Weak::load)
        }

        #[unsafe(method(setInitialFirstResponder:))]
        fn set_initial_first_responder(&self, view: Option<&NSView>) {
            let view = view.map(Weak::new);
            let old = std::mem::replace(&mut self.ivars().settings.borrow_mut().initial_first_responder, view);
            // Dropped outside the borrow.
            drop(old);
        }

        #[unsafe(method(isMovable))]
        fn is_movable(&self) -> bool {
            self.ivars().settings.borrow().movable
        }

        #[unsafe(method(setMovable:))]
        fn set_movable(&self, flag: bool) {
            let was = std::mem::replace(&mut self.ivars().settings.borrow_mut().movable, flag);
            if was != flag {
                self.send_style();
            }
        }

        #[unsafe(method(isDocumentEdited))]
        fn is_document_edited(&self) -> bool {
            self.ivars().settings.borrow().edited
        }

        #[unsafe(method(setDocumentEdited:))]
        fn set_document_edited(&self, flag: bool) {
            self.ivars().settings.borrow_mut().edited = flag;
        }

        #[unsafe(method_id(subtitle))]
        fn subtitle(&self) -> Retained<NSString> {
            self.ivars().settings.borrow().subtitle.clone()
        }

        #[unsafe(method(setSubtitle:))]
        fn set_subtitle(&self, subtitle: &NSString) {
            self.ivars().settings.borrow_mut().subtitle = subtitle.copy();
        }

        #[unsafe(method(hidesOnDeactivate))]
        fn hides_on_deactivate(&self) -> bool {
            self.ivars().settings.borrow().hides_on_deactivate
        }

        #[unsafe(method(setHidesOnDeactivate:))]
        fn set_hides_on_deactivate(&self, flag: bool) {
            self.ivars().settings.borrow_mut().hides_on_deactivate = flag;
        }

        #[unsafe(method(isRestorable))]
        fn is_restorable(&self) -> bool {
            self.ivars().settings.borrow().restorable
        }

        #[unsafe(method(setRestorable:))]
        fn set_restorable(&self, flag: bool) {
            self.ivars().settings.borrow_mut().restorable = flag;
        }

        #[unsafe(method(isExcludedFromWindowsMenu))]
        fn is_excluded_from_windows_menu(&self) -> bool {
            self.ivars().settings.borrow().excluded_from_windows_menu
        }

        #[unsafe(method(setExcludedFromWindowsMenu:))]
        fn set_excluded_from_windows_menu(&self, flag: bool) {
            self.ivars().settings.borrow_mut().excluded_from_windows_menu = flag;
        }

        #[unsafe(method(canHide))]
        fn can_hide(&self) -> bool {
            self.ivars().settings.borrow().can_hide
        }

        #[unsafe(method(setCanHide:))]
        fn set_can_hide(&self, flag: bool) {
            self.ivars().settings.borrow_mut().can_hide = flag;
        }

        #[unsafe(method_id(tabbingIdentifier))]
        fn tabbing_identifier(&self) -> Retained<NSString> {
            self.ivars().settings.borrow().tabbing_identifier.clone()
        }

        #[unsafe(method(setTabbingIdentifier:))]
        fn set_tabbing_identifier(&self, identifier: &NSString) {
            self.ivars().settings.borrow_mut().tabbing_identifier = identifier.copy();
        }

        #[unsafe(method(allowsAutomaticWindowTabbing))]
        fn allows_automatic_window_tabbing() -> bool {
            AUTOMATIC_TABBING.with(Cell::get)
        }

        #[unsafe(method(setAllowsAutomaticWindowTabbing:))]
        fn set_allows_automatic_window_tabbing(flag: bool) {
            AUTOMATIC_TABBING.with(|t| t.set(flag));
        }

        #[unsafe(method(preservesContentDuringLiveResize))]
        fn preserves_content_during_live_resize(&self) -> bool {
            self.ivars().settings.borrow().preserves_content
        }

        #[unsafe(method(setPreservesContentDuringLiveResize:))]
        fn set_preserves_content_during_live_resize(&self, flag: bool) {
            self.ivars().settings.borrow_mut().preserves_content = flag;
        }

        #[unsafe(method(inLiveResize))]
        fn in_live_resize(&self) -> bool {
            false
        }

        #[unsafe(method(animationBehavior))]
        fn animation_behavior(&self) -> NSWindowAnimationBehavior {
            self.ivars().settings.borrow().animation_behavior
        }

        #[unsafe(method(setAnimationBehavior:))]
        fn set_animation_behavior(&self, behavior: NSWindowAnimationBehavior) {
            self.ivars().settings.borrow_mut().animation_behavior = behavior;
        }

        #[unsafe(method(sharingType))]
        fn sharing_type(&self) -> NSWindowSharingType {
            self.ivars().settings.borrow().sharing_type
        }

        #[unsafe(method(setSharingType:))]
        fn set_sharing_type(&self, kind: NSWindowSharingType) {
            self.ivars().settings.borrow_mut().sharing_type = kind;
        }

        #[unsafe(method(allowsConcurrentViewDrawing))]
        fn allows_concurrent_view_drawing(&self) -> bool {
            self.ivars().settings.borrow().concurrent_drawing
        }

        #[unsafe(method(setAllowsConcurrentViewDrawing:))]
        fn set_allows_concurrent_view_drawing(&self, flag: bool) {
            self.ivars().settings.borrow_mut().concurrent_drawing = flag;
        }

        #[unsafe(method(displaysWhenScreenProfileChanges))]
        fn displays_when_screen_profile_changes(&self) -> bool {
            self.ivars().settings.borrow().displays_when_profile_changes
        }

        #[unsafe(method(setDisplaysWhenScreenProfileChanges:))]
        fn set_displays_when_screen_profile_changes(&self, flag: bool) {
            self.ivars().settings.borrow_mut().displays_when_profile_changes = flag;
        }

        #[unsafe(method(autorecalculatesKeyViewLoop))]
        fn autorecalculates_key_view_loop(&self) -> bool {
            self.ivars().settings.borrow().recalculates_key_view_loop
        }

        #[unsafe(method(setAutorecalculatesKeyViewLoop:))]
        fn set_autorecalculates_key_view_loop(&self, flag: bool) {
            self.ivars().settings.borrow_mut().recalculates_key_view_loop = flag;
        }

        #[unsafe(method_id(miniwindowTitle))]
        fn miniwindow_title(&self) -> Retained<NSString> {
            let set = self.ivars().settings.borrow().miniwindow_title.clone();
            set.unwrap_or_else(|| self.ivars().title.borrow().clone())
        }

        /// Nil goes back to the title.
        #[unsafe(method(setMiniwindowTitle:))]
        fn set_miniwindow_title(&self, title: Option<&NSString>) {
            self.ivars().settings.borrow_mut().miniwindow_title = title.map(|t| t.copy());
        }

        #[unsafe(method_id(representedFilename))]
        fn represented_filename(&self) -> Retained<NSString> {
            self.ivars().settings.borrow().represented_filename.clone()
        }

        #[unsafe(method(setRepresentedFilename:))]
        fn set_represented_filename(&self, name: &NSString) {
            self.ivars().settings.borrow_mut().represented_filename = name.copy();
        }

        #[unsafe(method(resizeIncrements))]
        fn resize_increments(&self) -> NSSize {
            self.ivars().settings.borrow().resize_increments
        }

        // Frame and content increments exclude each other, as AppKit's do.
        #[unsafe(method(setResizeIncrements:))]
        fn set_resize_increments(&self, size: NSSize) {
            let mut settings = self.ivars().settings.borrow_mut();
            settings.resize_increments = size;
            settings.content_resize_increments = NSSize::new(1.0, 1.0);
        }

        #[unsafe(method(contentResizeIncrements))]
        fn content_resize_increments(&self) -> NSSize {
            self.ivars().settings.borrow().content_resize_increments
        }

        #[unsafe(method(setContentResizeIncrements:))]
        fn set_content_resize_increments(&self, size: NSSize) {
            let mut settings = self.ivars().settings.borrow_mut();
            settings.content_resize_increments = size;
            settings.resize_increments = NSSize::new(1.0, 1.0);
        }

        #[unsafe(method(aspectRatio))]
        fn aspect_ratio(&self) -> NSSize {
            self.ivars().settings.borrow().aspect_ratio
        }

        #[unsafe(method(setAspectRatio:))]
        fn set_aspect_ratio(&self, size: NSSize) {
            self.ivars().settings.borrow_mut().aspect_ratio = size;
        }

        #[unsafe(method(contentAspectRatio))]
        fn content_aspect_ratio(&self) -> NSSize {
            self.ivars().settings.borrow().content_aspect_ratio
        }

        #[unsafe(method(setContentAspectRatio:))]
        fn set_content_aspect_ratio(&self, size: NSSize) {
            self.ivars().settings.borrow_mut().content_aspect_ratio = size;
        }

        #[unsafe(method(isZoomable))]
        fn is_zoomable(&self) -> bool {
            self.ivars().style.get().contains(NSWindowStyleMask::Resizable)
        }

        #[unsafe(method(isResizable))]
        fn is_resizable(&self) -> bool {
            self.ivars().style.get().contains(NSWindowStyleMask::Resizable)
        }

        #[unsafe(method(isMiniaturizable))]
        fn is_miniaturizable(&self) -> bool {
            self.ivars().style.get().contains(NSWindowStyleMask::Miniaturizable)
        }

        #[unsafe(method(hasCloseBox))]
        fn has_close_box(&self) -> bool {
            self.ivars().style.get().contains(NSWindowStyleMask::Closable)
        }

        #[unsafe(method(hasTitleBar))]
        fn has_title_bar(&self) -> bool {
            self.ivars().style.get().contains(NSWindowStyleMask::Titled)
        }

        #[unsafe(method(isFloatingPanel))]
        fn is_floating_panel(&self) -> bool {
            false
        }

        #[unsafe(method(isModalPanel))]
        fn is_modal_panel(&self) -> bool {
            false
        }

        #[unsafe(method(orderedIndex))]
        fn ordered_index(&self) -> isize {
            0
        }

        // Wayland doesn't let clients stack their windows.
        #[unsafe(method(setOrderedIndex:))]
        fn set_ordered_index(&self, _index: isize) {}

        #[unsafe(method(setIsMiniaturized:))]
        fn set_is_miniaturized(&self, flag: bool) {
            if flag {
                as_window(self).miniaturize(None);
            } else {
                as_window(self).deminiaturize(None);
            }
        }

        #[unsafe(method(setIsZoomed:))]
        fn set_is_zoomed(&self, flag: bool) {
            if as_window(self).isZoomed() != flag {
                as_window(self).zoom(None);
            }
        }

        #[unsafe(method(worksWhenModal))]
        fn works_when_modal(&self) -> bool {
            false
        }

        #[unsafe(method(occlusionState))]
        fn occlusion_state(&self) -> NSWindowOcclusionState {
            let ivars = self.ivars();
            let shown = ivars.visible.get() && !ivars.miniaturized.get() && !ivars.state.get().suspended;
            if shown { NSWindowOcclusionState::Visible } else { NSWindowOcclusionState(0) }
        }

        #[unsafe(method(setIsVisible:))]
        fn set_is_visible(&self, flag: bool) {
            if flag {
                order_front(self);
            } else {
                order_out(self);
            }
        }

        #[unsafe(method(update))]
        fn update(&self) {}

        // Sidestep draws its own title bar buttons, which aren't views.
        #[unsafe(method_id(standardWindowButton:))]
        fn standard_window_button(&self, _button: NSWindowButton) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method_id(screen))]
        fn screen(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method_id(windowController))]
        fn window_controller(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method_id(attachedSheet))]
        fn attached_sheet(&self) -> Option<Retained<NSWindow>> {
            None
        }

        #[unsafe(method_id(sheetParent))]
        fn sheet_parent(&self) -> Option<Retained<NSWindow>> {
            None
        }

        #[unsafe(method(isSheet))]
        fn is_sheet(&self) -> bool {
            false
        }

        /// Dragging from a mouse-down in the content moves the window, as
        /// windows with title bars of their own do.
        #[unsafe(method(performWindowDragWithEvent:))]
        fn perform_window_drag_with_event(&self, _event: &NSEvent) {
            self.request(WindowRequest::Move);
        }

        #[unsafe(method_id(nextEventMatchingMask:))]
        fn next_event_matching_mask(&self, mask: NSEventMask) -> Option<Retained<NSEvent>> {
            app::next_event(mask, None, true, false)
        }

        #[unsafe(method_id(nextEventMatchingMask:untilDate:inMode:dequeue:))]
        fn next_event_matching_mask_until(
            &self,
            mask: NSEventMask,
            until: Option<&AnyObject>,
            mode: &NSString,
            dequeue: bool,
        ) -> Option<Retained<NSEvent>> {
            app::next_event(mask, app::deadline(until), dequeue, app::default_mode(mode))
        }

        #[unsafe(method(postEvent:atStart:))]
        fn post_event(&self, event: &NSEvent, at_start: bool) {
            app::post_event(event, at_start);
        }

        #[unsafe(method(discardEventsMatchingMask:beforeEvent:))]
        fn discard_events_matching_mask(&self, mask: NSEventMask, last: Option<&NSEvent>) {
            app::discard_events(mask, last);
        }

        #[unsafe(method(isOnActiveSpace))]
        fn is_on_active_space(&self) -> bool {
            true
        }

        #[unsafe(method(setFrame:display:animate:))]
        fn set_frame_display_animate(&self, frame: NSRect, display: bool, _animate: bool) {
            as_window(self).setFrame_display(frame, display);
        }

        #[unsafe(method(setFrameTopLeftPoint:))]
        fn set_frame_top_left_point(&self, point: NSPoint) {
            let height = as_window(self).frame().size.height;
            self.ivars().origin.set(NSPoint::new(point.x, point.y - height));
        }

        #[unsafe(method(cascadeTopLeftFromPoint:))]
        fn cascade_top_left_from_point(&self, point: NSPoint) -> NSPoint {
            // Wayland places windows: the point stays where it is.
            point
        }

        #[unsafe(method(orderBack:))]
        fn order_back(&self, _sender: Option<&AnyObject>) {
            order_front(self);
        }

        #[unsafe(method(orderWindow:relativeTo:))]
        fn order_window_relative_to(&self, place: NSWindowOrderingMode, _other: isize) {
            if place == NSWindowOrderingMode::Out {
                order_out(self);
            } else {
                order_front(self);
            }
        }

        #[unsafe(method(viewsNeedDisplay))]
        fn views_need_display(&self) -> bool {
            self.ivars().needs_display.get()
        }

        #[unsafe(method(setViewsNeedDisplay:))]
        fn set_views_need_display(&self, flag: bool) {
            if flag {
                self.ivars().needs_display.set(true);
            }
        }

        #[unsafe(method(displayIfNeeded))]
        fn display_if_needed(&self) {
            // Displays run once the last frame is shown, from the event loop.
        }

        #[unsafe(method(frameRectForContentRect:styleMask:))]
        fn class_frame_rect_for_content_rect(rect: NSRect, style: NSWindowStyleMask) -> NSRect {
            grow(rect, predicted_titlebar(style))
        }

        #[unsafe(method(contentRectForFrameRect:styleMask:))]
        fn class_content_rect_for_frame_rect(rect: NSRect, style: NSWindowStyleMask) -> NSRect {
            grow(rect, -predicted_titlebar(style))
        }

        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> {
            self.ivars().title.borrow().clone()
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: &NSString) {
            self.ivars().title.replace(title.copy());
            self.ivars().title_dirty.set(true);
            self.ivars().needs_display.set(true);
        }

        #[unsafe(method(styleMask))]
        fn style_mask(&self) -> NSWindowStyleMask {
            let mut style = self.ivars().style.get();
            if self.ivars().state.get().fullscreen {
                style |= NSWindowStyleMask::FullScreen;
            }
            style
        }

        #[unsafe(method(setStyleMask:))]
        fn set_style_mask(&self, style: NSWindowStyleMask) {
            self.ivars().style.set(style & !NSWindowStyleMask::FullScreen);
            if self.ivars().visible.get() {
                self.send_style();
                self.send_limits();
            }
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.delegate_object()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            self.ivars().delegate.replace(delegate.map(Weak::new));
        }

        #[unsafe(method(windowNumber))]
        fn window_number(&self) -> isize {
            self.number()
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
            grow(self.content_rect(), self.titlebar())
        }

        #[unsafe(method(contentLayoutRect))]
        fn content_layout_rect(&self) -> NSRect {
            NSRect::new(NSPoint::ZERO, self.ivars().size.get())
        }

        #[unsafe(method(frameRectForContentRect:))]
        fn frame_rect_for_content_rect(&self, rect: NSRect) -> NSRect {
            grow(rect, self.titlebar())
        }

        #[unsafe(method(contentRectForFrameRect:))]
        fn content_rect_for_frame_rect(&self, rect: NSRect) -> NSRect {
            grow(rect, -self.titlebar())
        }

        #[unsafe(method(setContentSize:))]
        fn set_content_size(&self, size: NSSize) {
            self.resize_to(size);
        }

        #[unsafe(method(setFrame:display:))]
        fn set_frame_display(&self, frame: NSRect, _display: bool) {
            let content = grow(frame, -self.titlebar());
            self.ivars().origin.set(content.origin);
            self.resize_to(content.size);
        }

        #[unsafe(method(setFrameOrigin:))]
        fn set_frame_origin(&self, origin: NSPoint) {
            self.ivars().origin.set(origin);
        }

        #[unsafe(method(minSize))]
        fn min_size(&self) -> NSSize {
            grow(NSRect::new(NSPoint::ZERO, self.ivars().content_min.get()), self.titlebar()).size
        }

        #[unsafe(method(setMinSize:))]
        fn set_min_size(&self, size: NSSize) {
            let content = grow(NSRect::new(NSPoint::ZERO, size), -self.titlebar()).size;
            self.ivars().content_min.set(content);
            self.send_limits();
        }

        #[unsafe(method(maxSize))]
        fn max_size(&self) -> NSSize {
            grow(NSRect::new(NSPoint::ZERO, self.ivars().content_max.get()), self.titlebar()).size
        }

        #[unsafe(method(setMaxSize:))]
        fn set_max_size(&self, size: NSSize) {
            let content = grow(NSRect::new(NSPoint::ZERO, size), -self.titlebar()).size;
            self.ivars().content_max.set(content);
            self.send_limits();
        }

        #[unsafe(method(contentMinSize))]
        fn content_min_size(&self) -> NSSize {
            self.ivars().content_min.get()
        }

        #[unsafe(method(setContentMinSize:))]
        fn set_content_min_size(&self, size: NSSize) {
            self.ivars().content_min.set(size);
            self.send_limits();
        }

        #[unsafe(method(contentMaxSize))]
        fn content_max_size(&self) -> NSSize {
            self.ivars().content_max.get()
        }

        #[unsafe(method(setContentMaxSize:))]
        fn set_content_max_size(&self, size: NSSize) {
            self.ivars().content_max.set(size);
            self.send_limits();
        }

        #[unsafe(method(backingScaleFactor))]
        fn backing_scale_factor(&self) -> f64 {
            self.ivars().scale.get()
        }

        #[unsafe(method(convertRectToBacking:))]
        fn convert_rect_to_backing(&self, r: NSRect) -> NSRect {
            let s = self.ivars().scale.get();
            NSRect::new(
                NSPoint::new(r.origin.x * s, r.origin.y * s),
                NSSize::new(r.size.width * s, r.size.height * s),
            )
        }

        #[unsafe(method(convertRectFromBacking:))]
        fn convert_rect_from_backing(&self, r: NSRect) -> NSRect {
            let s = self.ivars().scale.get();
            NSRect::new(
                NSPoint::new(r.origin.x / s, r.origin.y / s),
                NSSize::new(r.size.width / s, r.size.height / s),
            )
        }

        #[unsafe(method(convertPointToBacking:))]
        fn convert_point_to_backing(&self, p: NSPoint) -> NSPoint {
            let s = self.ivars().scale.get();
            NSPoint::new(p.x * s, p.y * s)
        }

        #[unsafe(method(convertPointFromBacking:))]
        fn convert_point_from_backing(&self, p: NSPoint) -> NSPoint {
            let s = self.ivars().scale.get();
            NSPoint::new(p.x / s, p.y / s)
        }

        #[unsafe(method(convertRectToScreen:))]
        fn convert_rect_to_screen(&self, r: NSRect) -> NSRect {
            let o = self.ivars().origin.get();
            NSRect::new(NSPoint::new(r.origin.x + o.x, r.origin.y + o.y), r.size)
        }

        #[unsafe(method(convertRectFromScreen:))]
        fn convert_rect_from_screen(&self, r: NSRect) -> NSRect {
            let o = self.ivars().origin.get();
            NSRect::new(NSPoint::new(r.origin.x - o.x, r.origin.y - o.y), r.size)
        }

        #[unsafe(method(convertPointToScreen:))]
        fn convert_point_to_screen(&self, p: NSPoint) -> NSPoint {
            let o = self.ivars().origin.get();
            NSPoint::new(p.x + o.x, p.y + o.y)
        }

        #[unsafe(method(convertPointFromScreen:))]
        fn convert_point_from_screen(&self, p: NSPoint) -> NSPoint {
            let o = self.ivars().origin.get();
            NSPoint::new(p.x - o.x, p.y - o.y)
        }

        #[unsafe(method(addChildWindow:ordered:))]
        fn add_child_window(&self, child: &NSWindow, _place: NSWindowOrderingMode) {
            add_child(self, child);
        }

        #[unsafe(method(removeChildWindow:))]
        fn remove_child_window(&self, child: &NSWindow) {
            remove_child(self, child);
        }

        /// Nil until Foundation has `NSArray` to answer with.
        #[unsafe(method_id(childWindows))]
        fn child_windows(&self) -> Option<Retained<AnyObject>> {
            let children = self.ivars().children.borrow().clone();
            objc2::runtime::AnyClass::get(c"NSArray").map(|_| app::array_of(&children))
        }

        #[unsafe(method_id(parentWindow))]
        fn parent_window(&self) -> Option<Retained<NSWindow>> {
            // SAFETY: a parent holds its children, and clears their links
            // before it goes.
            self.ivars().parent.get().map(|p| unsafe { p.as_ref() }.retain())
        }

        #[unsafe(method(setParentWindow:))]
        fn set_parent_window(&self, parent: Option<&NSWindow>) {
            // SAFETY: as above.
            if let Some(old) = self.ivars().parent.get().map(|p| unsafe { p.as_ref() }.retain()) {
                remove_child(imp(&old), as_window(self));
            }
            if let Some(parent) = parent {
                add_child(imp(parent), as_window(self));
            }
        }

        #[unsafe(method(makeKeyAndOrderFront:))]
        fn make_key_and_order_front(&self, _sender: Option<&AnyObject>) {
            if self.ivars().visible.get() {
                self.request(WindowRequest::Activate);
            }
            order_front(self);
        }

        #[unsafe(method(orderFront:))]
        fn order_front(&self, _sender: Option<&AnyObject>) {
            order_front(self);
        }

        #[unsafe(method(orderFrontRegardless))]
        fn order_front_regardless(&self) {
            order_front(self);
        }

        #[unsafe(method(orderOut:))]
        fn order_out(&self, _sender: Option<&AnyObject>) {
            order_out(self);
        }

        /// The delegate hears of it even for a window that isn't on screen,
        /// which a program may close to tear it down.
        #[unsafe(method(close))]
        fn close(&self) {
            notify(self, sel!(windowWillClose:), "NSWindowWillCloseNotification");
            if self.ivars().visible.get() {
                order_out(self);
                app::window_closed();
            }
        }

        /// The close button's click (and the compositor's close request,
        /// its equivalent): nothing for a window without a close button.
        #[unsafe(method(performClose:))]
        fn perform_close(&self, _sender: Option<&AnyObject>) {
            if self.ivars().style.get().contains(NSWindowStyleMask::Closable) && should_close(self) {
                as_window(self).close();
            }
        }

        /// Only a window on screen can be miniaturized.
        #[unsafe(method(miniaturize:))]
        fn miniaturize(&self, _sender: Option<&AnyObject>) {
            if !self.ivars().visible.get() || self.ivars().miniaturized.get() {
                return;
            }
            notify(self, sel!(windowWillMiniaturize:), "NSWindowWillMiniaturizeNotification");
            self.request(WindowRequest::Minimize);
            self.ivars().miniaturized.set(true);
            notify(self, sel!(windowDidMiniaturize:), "NSWindowDidMiniaturizeNotification");
        }

        #[unsafe(method(performMiniaturize:))]
        fn perform_miniaturize(&self, sender: Option<&AnyObject>) {
            if self.ivars().style.get().contains(NSWindowStyleMask::Miniaturizable) {
                as_window(self).miniaturize(sender);
            }
        }

        #[unsafe(method(deminiaturize:))]
        fn deminiaturize(&self, _sender: Option<&AnyObject>) {
            self.request(WindowRequest::Activate);
            self.deminiaturized();
        }

        #[unsafe(method(isMiniaturized))]
        fn is_miniaturized(&self) -> bool {
            self.ivars().miniaturized.get()
        }

        #[unsafe(method(zoom:))]
        fn zoom(&self, _sender: Option<&AnyObject>) {
            self.request(WindowRequest::Maximize(!self.ivars().state.get().maximized));
        }

        #[unsafe(method(performZoom:))]
        fn perform_zoom(&self, sender: Option<&AnyObject>) {
            if self.ivars().style.get().contains(NSWindowStyleMask::Resizable) {
                as_window(self).zoom(sender);
            }
        }

        #[unsafe(method(isZoomed))]
        fn is_zoomed(&self) -> bool {
            self.ivars().state.get().maximized
        }

        #[unsafe(method(toggleFullScreen:))]
        fn toggle_full_screen(&self, _sender: Option<&AnyObject>) {
            self.request(WindowRequest::Fullscreen(!self.ivars().state.get().fullscreen));
        }

        #[unsafe(method(center))]
        fn center(&self) {}

        #[unsafe(method(isVisible))]
        fn is_visible(&self) -> bool {
            self.ivars().visible.get()
        }

        #[unsafe(method(areCursorRectsEnabled))]
        fn are_cursor_rects_enabled(&self) -> bool {
            self.tracking().borrow().cursor_rects_enabled()
        }

        #[unsafe(method(enableCursorRects))]
        fn enable_cursor_rects(&self) {
            self.tracking().borrow_mut().enable_cursor_rects(true);
        }

        #[unsafe(method(disableCursorRects))]
        fn disable_cursor_rects(&self) {
            self.tracking().borrow_mut().enable_cursor_rects(false);
        }

        #[unsafe(method(invalidateCursorRectsForView:))]
        fn invalidate_cursor_rects_for_view(&self, view: &NSView) {
            self.tracking().borrow_mut().invalidate_cursor_rects(Some(views::imp(view)));
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.tracking().borrow_mut().invalidate_cursor_rects(None);
            tracking::update(self, None);
        }

        #[unsafe(method(discardCursorRects))]
        fn discard_cursor_rects(&self) {
            if let Some(content) = self.content() {
                discard_all_cursor_rects(&content);
            }
        }

        // The end of the chain for views that don't set a cursor of their
        // own: the arrow.
        #[unsafe(method(cursorUpdate:))]
        fn cursor_update(&self, _event: &NSEvent) {
            NSCursor::arrowCursor().set();
        }

        #[unsafe(method(isKeyWindow))]
        fn is_key_window(&self) -> bool {
            self.ivars().key.get()
        }

        #[unsafe(method(isMainWindow))]
        fn is_main_window(&self) -> bool {
            self.ivars().main.get()
        }

        /// A title bar makes a window able to take the keyboard; borderless
        /// windows that want it override this, as on macOS.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            self.ivars().style.get().contains(NSWindowStyleMask::Titled)
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            self.ivars().visible.get() && self.ivars().style.get().contains(NSWindowStyleMask::Titled)
        }

        #[unsafe(method(makeKeyWindow))]
        fn make_key_window(&self) {
            if self.ivars().visible.get() && !self.ivars().key.get() {
                self.request(WindowRequest::Activate);
            }
        }

        #[unsafe(method(makeMainWindow))]
        fn make_main_window(&self) {
            as_window(self).makeKeyWindow();
        }

        #[unsafe(method(becomeKeyWindow))]
        fn become_key_window(&self) {
            notify(self, sel!(windowDidBecomeKey:), "NSWindowDidBecomeKeyNotification");
        }

        #[unsafe(method(resignKeyWindow))]
        fn resign_key_window(&self) {
            notify(self, sel!(windowDidResignKey:), "NSWindowDidResignKeyNotification");
        }

        #[unsafe(method(becomeMainWindow))]
        fn become_main_window(&self) {
            notify(self, sel!(windowDidBecomeMain:), "NSWindowDidBecomeMainNotification");
        }

        #[unsafe(method(resignMainWindow))]
        fn resign_main_window(&self) {
            notify(self, sel!(windowDidResignMain:), "NSWindowDidResignMainNotification");
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

        #[unsafe(method(acceptsMouseMovedEvents))]
        fn accepts_mouse_moved_events(&self) -> bool {
            self.ivars().accepts_mouse_moved.get()
        }

        #[unsafe(method(setAcceptsMouseMovedEvents:))]
        fn set_accepts_mouse_moved_events(&self, flag: bool) {
            self.ivars().accepts_mouse_moved.set(flag);
        }

        #[unsafe(method(mouseLocationOutsideOfEventStream))]
        fn mouse_location_outside_of_event_stream(&self) -> NSPoint {
            self.ivars().pointer.get().unwrap_or(NSPoint::ZERO)
        }

        #[unsafe(method_id(firstResponder))]
        fn first_responder(&self) -> Option<Retained<NSResponder>> {
            match self.ivars().first_responder.borrow().clone() {
                Some(responder) => Some(responder),
                None => Some(Retained::into_super(self.retain())),
            }
        }

        #[unsafe(method(makeFirstResponder:))]
        fn make_first_responder(&self, responder: Option<&NSResponder>) -> bool {
            make_first_responder(self, responder)
        }

        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            send_event(self, event);
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            let content = self.ivars().content.borrow().clone();
            content.is_some_and(|c| c.performKeyEquivalent(event))
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
            // Also clears the view's link to the window as its next
            // responder.
            views::set_window(views::imp(&content), None);
        }
        for child in self.children.get_mut().drain(..) {
            imp(&child).ivars().parent.set(None);
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

/// `r` with `dh` points added to its height, as a frame is to its content.
fn grow(r: NSRect, dh: f64) -> NSRect {
    NSRect::new(r.origin, NSSize::new(r.size.width, (r.size.height + dh).max(0.0)))
}

/// The title bar height a window with `style` is expected to get before
/// the compositor says: Sidestep's own title bar only when decorations are
/// forced to the client, as the compositor draws them otherwise.
fn predicted_titlebar(style: NSWindowStyleMask) -> f64 {
    if style.contains(NSWindowStyleMask::Titled) && crate::backend::force_client_decorations() { HEADER } else { 0.0 }
}

fn to_style(style: NSWindowStyleMask) -> Style {
    Style {
        titled: style.contains(NSWindowStyleMask::Titled),
        closable: style.contains(NSWindowStyleMask::Closable),
        miniaturizable: style.contains(NSWindowStyleMask::Miniaturizable),
        resizable: style.contains(NSWindowStyleMask::Resizable),
        movable: true,
        passthrough: false,
    }
}

/// Points as whole points for the render thread; 0 for no limit.
fn limit(size: NSSize) -> (u32, u32) {
    let side = |v: f64| if v >= UNLIMITED / 2.0 { 0 } else { v.ceil().clamp(0.0, 1e6) as u32 };
    (side(size.width), side(size.height))
}

/// Tell the delegate, if it listens, with a notification from the window.
fn notify(window: &NSWindowImpl, selector: Sel, name: &str) {
    let Some(delegate) = window.delegate_object() else { return };
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: selector] };
    if !responds {
        return;
    }
    let note = notification(&NSString::from_str(name), Some(window));
    // SAFETY: NSWindowDelegate's notification methods take the notification
    // and return nothing.
    unsafe { MessageReceiver::send_message::<_, ()>(&*delegate, selector, (&*note,)) }
}

/// Ask the delegate, then the window itself, whether to close.
fn should_close(window: &NSWindowImpl) -> bool {
    let this = as_window(window);
    let delegate = window.delegate_object();
    let asked: &AnyObject = match &delegate {
        Some(d) => d,
        None => this,
    };
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    let responds: bool = unsafe { msg_send![asked, respondsToSelector: sel!(windowShouldClose:)] };
    // SAFETY: windowShouldClose: takes the window and returns BOOL.
    !responds || unsafe { msg_send![asked, windowShouldClose: this] }
}

impl NSWindowImpl {
    /// What the render thread calls the window in its current (or next)
    /// showing.
    pub(crate) fn id(&self) -> WindowId {
        self.ivars().showing.get()
    }

    /// `windowNumber`.
    pub(crate) fn number(&self) -> isize {
        self.ivars().number as isize
    }

    pub(crate) fn delegate_object(&self) -> Option<Retained<AnyObject>> {
        self.ivars().delegate.borrow().as_ref().and_then(Weak::load)
    }

    /// Belong over `parent` (or no window): compositors keep such windows
    /// above their parent and place them over it.
    pub(crate) fn set_transient(&self, parent: Option<&NSWindow>) {
        let same = match (self.transient(), parent) {
            (Some(a), Some(b)) => std::ptr::eq(&*a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        let old = self.ivars().transient.replace(parent.map(Weak::new));
        drop(old);
        if self.ivars().visible.get() {
            app::send(ToRender::SetParent { window: self.id(), parent: self.transient_showing() });
        }
    }

    pub(crate) fn transient(&self) -> Option<Retained<NSWindow>> {
        self.ivars().transient.borrow().as_ref().and_then(Weak::load)
    }

    /// The window this one belongs over, if it's on screen, as the render
    /// thread knows it.
    fn transient_showing(&self) -> Option<WindowId> {
        self.transient().filter(|p| imp(p).ivars().visible.get()).map(|p| imp(&p).id())
    }

    pub(crate) fn content_height(&self) -> f64 {
        self.ivars().size.get().height
    }

    /// Where the content's bottom left is on the screen.
    pub(crate) fn content_origin(&self) -> NSPoint {
        self.ivars().origin.get()
    }

    /// The first responder, when it isn't the window itself.
    pub(crate) fn first_responder_object(&self) -> Option<Retained<AnyObject>> {
        let first = self.ivars().first_responder.borrow().clone();
        first.map(Retained::into_super).map(Retained::into_super)
    }

    pub(crate) fn text_input_state(&self) -> &Cell<(bool, Option<Rect>)> {
        &self.ivars().text_input
    }

    /// The style the render thread works with: the style mask, and the
    /// settings that change what the user can do with the window.
    fn render_style(&self) -> Style {
        let settings = self.ivars().settings.borrow();
        Style { movable: settings.movable, passthrough: settings.ignores_mouse, ..to_style(self.ivars().style.get()) }
    }

    fn send_style(&self) {
        if self.ivars().visible.get() {
            app::send(ToRender::SetStyle { window: self.id(), style: self.render_style() });
        }
    }

    fn content_rect(&self) -> NSRect {
        NSRect::new(self.ivars().origin.get(), self.ivars().size.get())
    }

    /// Height of the title bar around the content: what the render thread
    /// reported, or what it's expected to be.
    fn titlebar(&self) -> f64 {
        self.ivars().titlebar.get().unwrap_or_else(|| predicted_titlebar(self.ivars().style.get()))
    }

    fn request(&self, request: WindowRequest) {
        if self.ivars().visible.get() {
            app::send(ToRender::Request { window: self.id(), request });
        }
    }

    fn limits(&self) -> SizeLimits {
        SizeLimits { min: limit(self.ivars().content_min.get()), max: limit(self.ivars().content_max.get()) }
    }

    fn send_limits(&self) {
        if self.ivars().visible.get() {
            app::send(ToRender::SetSizeLimits { window: self.id(), limits: self.limits() });
        }
    }

    fn resize_to(&self, size: NSSize) {
        let ivars = self.ivars();
        if ivars.visible.get() {
            let (w, h) = (size.width.round().max(1.0) as u32, size.height.round().max(1.0) as u32);
            app::send(ToRender::Request { window: self.id(), request: WindowRequest::Resize(w, h) });
        } else {
            ivars.size.set(size);
            let content = ivars.content.borrow().clone();
            if let Some(content) = content {
                content.setFrame(NSRect::new(NSPoint::ZERO, size));
            }
        }
    }

    fn deminiaturized(&self) {
        if self.ivars().miniaturized.replace(false) {
            notify(self, sel!(windowDidDeminiaturize:), "NSWindowDidDeminiaturizeNotification");
        }
    }

    pub(crate) fn is_content_view(&self, view: &NSViewImpl) -> bool {
        self.ivars().content.borrow().as_ref().is_some_and(|c| std::ptr::eq(views::imp(c), view))
    }

    /// Damage part of a layer, in its points.
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

    /// A view left the window: it's no longer the first responder, without
    /// being asked, nor does it get the rest of a click.
    pub(crate) fn view_left(&self, view: &NSViewImpl) {
        let is_it = |r: &NSResponder| std::ptr::eq((r as *const NSResponder).cast::<NSViewImpl>(), view);
        let first = self.ivars().first_responder.borrow().as_ref().is_some_and(|r| is_it(r));
        if first {
            // Dropped outside the borrow: releasing may run arbitrary code.
            let gone = self.ivars().first_responder.take();
            drop(gone);
        }
        let clicked = self.ivars().mouse_view.borrow().as_ref().is_some_and(|(v, _)| is_it(v));
        if clicked {
            let gone = self.ivars().mouse_view.take();
            drop(gone);
        }
        tracking::view_left(self, view);
    }

    pub(crate) fn tracking(&self) -> &RefCell<tracking::WindowTracking> {
        &self.ivars().tracking
    }

    pub(crate) fn as_window(&self) -> &NSWindow {
        as_window(self)
    }

    /// Where the pointer is in the window, if it's over it.
    pub(crate) fn pointer(&self) -> Option<NSPoint> {
        self.ivars().pointer.get()
    }

    /// A mouse button is held down.
    pub(crate) fn dragging(&self) -> bool {
        crate::event::pressed_buttons() != 0
    }

    pub(crate) fn is_key(&self) -> bool {
        self.ivars().key.get()
    }

    pub(crate) fn is_first_responder(&self, view: &NSViewImpl) -> bool {
        let first = self.ivars().first_responder.borrow();
        first.as_ref().is_some_and(|r| std::ptr::eq((&**r as *const NSResponder).cast::<NSViewImpl>(), view))
    }

    pub(crate) fn content(&self) -> Option<Retained<NSView>> {
        self.ivars().content.borrow().clone()
    }

    pub(crate) fn remove_clip(&self, clip: &NSViewImpl) {
        let layer = views::layer_id(clip);
        self.ivars().clips.borrow_mut().retain(|c| !std::ptr::eq(views::imp(c), clip));
        self.ivars().damage.borrow_mut().remove(&layer);
        if self.ivars().layers.borrow_mut().remove(&layer).is_some() && self.ivars().visible.get() {
            // Hide it: an empty viewport shows no tiles.
            app::send(ToRender::ScrollLayer {
                window: self.id(),
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

    /// The window's size, scale and state from the compositor, first and
    /// after every change.
    pub(crate) fn configure(&self, width: u32, height: u32, scale: f64, titlebar: u32, state: WindowState) {
        let ivars = self.ivars();
        let first = !ivars.configured.replace(true);
        let size = NSSize::new(width as f64, height as f64);
        let resized = ivars.size.replace(size) != size;
        let rescaled = ivars.scale.replace(scale) != scale;
        let titlebar = titlebar as f64;
        if ivars.titlebar.replace(Some(titlebar)) != Some(titlebar) && titlebar > 0.0 {
            ivars.title_dirty.set(true);
        }
        let before = ivars.state.replace(state);
        if resized || first {
            let content = ivars.content.borrow().clone();
            if let Some(content) = content {
                content.setFrame(NSRect::new(NSPoint::ZERO, size));
            }
        }
        if rescaled {
            // The render thread dropped every tile drawn at the old scale.
            for layer in ivars.layers.borrow_mut().values_mut() {
                layer.valid.clear();
            }
        }
        if first || resized || rescaled {
            // The render thread made a new canvas: draw it all, now.
            ivars.frame_pending.set(false);
            self.damage_all();
        }
        if resized && !first {
            notify(self, sel!(windowDidResize:), "NSWindowDidResizeNotification");
        }
        if rescaled && !first {
            notify(self, sel!(windowDidChangeBackingProperties:), "NSWindowDidChangeBackingPropertiesNotification");
        }
        if state.fullscreen != before.fullscreen {
            let (selector, name) = if state.fullscreen {
                (sel!(windowDidEnterFullScreen:), "NSWindowDidEnterFullScreenNotification")
            } else {
                (sel!(windowDidExitFullScreen:), "NSWindowDidExitFullScreenNotification")
            };
            notify(self, selector, name);
        }
        if state.suspended != before.suspended {
            notify(self, sel!(windowDidChangeOcclusionState:), "NSWindowDidChangeOcclusionStateNotification");
        }
        if state.activated {
            self.deminiaturized();
        }
    }

    pub(crate) fn frame_done(&self) {
        self.ivars().frame_pending.set(false);
    }

    /// The window became the key window, or stopped being it (see
    /// `app::focus_moved`, which decides).
    pub(crate) fn set_key(&self, key: bool) {
        if self.ivars().key.replace(key) == key {
            return;
        }
        self.tracking().borrow_mut().recheck();
        crate::inputcontext::update(self);
        let this = as_window(self);
        if key {
            self.deminiaturized();
            this.becomeKeyWindow();
        } else {
            this.resignKeyWindow();
        }
    }

    /// The window became the main window, or stopped being it.
    pub(crate) fn set_main(&self, main: bool) {
        if self.ivars().main.replace(main) == main {
            return;
        }
        let this = as_window(self);
        if main {
            this.becomeMainWindow();
        } else {
            this.resignMainWindow();
        }
    }

    pub(crate) fn is_main(&self) -> bool {
        self.ivars().main.get()
    }

    /// Where the pointer is, from the render thread's points from the top
    /// left of the content.
    fn location(&self, x: f64, y: f64) -> NSPoint {
        NSPoint::new(x, self.content_height() - y)
    }

    /// A button pressed or released at `x`, `y` (points from the top left).
    pub(crate) fn button(&self, button: Button, pressed: bool, x: f64, y: f64, clicks: u32, modifiers: Modifiers) {
        let (number, kind) = match (button, pressed) {
            (Button::Left, true) => (0, NSEventType::LeftMouseDown),
            (Button::Left, false) => (0, NSEventType::LeftMouseUp),
            (Button::Right, true) => (1, NSEventType::RightMouseDown),
            (Button::Right, false) => (1, NSEventType::RightMouseUp),
            (Button::Other(n), true) => (n as isize, NSEventType::OtherMouseDown),
            (Button::Other(n), false) => (n as isize, NSEventType::OtherMouseUp),
        };
        crate::event::set_button_down(number, pressed);
        if pressed {
            crate::momentum::stop();
        }
        let location = self.location(x, y);
        self.pointer_at(location);
        let event = crate::event::mouse_event(kind, location, as_window(self), number, clicks as isize, modifiers);
        app::dispatch(&event);
        if !pressed {
            // What changed during a drag shows once it ends.
            tracking::update(self, None);
        }
    }

    fn pointer_at(&self, location: NSPoint) {
        self.ivars().pointer.set(Some(location));
        let origin = self.ivars().origin.get();
        crate::event::set_mouse_location(NSPoint::new(origin.x + location.x, origin.y + location.y));
    }

    /// The pointer moved: a drag if a button is down, else a move for
    /// windows that accept them.
    pub(crate) fn motion(&self, x: f64, y: f64, modifiers: Modifiers) {
        let location = self.location(x, y);
        self.pointer_at(location);
        // Drags follow the buttons input says are held, which stay right
        // even when a nested loop takes the mouse-up.
        let held = crate::event::pressed_buttons();
        let (kind, number) = if held & 1 != 0 {
            (NSEventType::LeftMouseDragged, 0)
        } else if held & 2 != 0 {
            (NSEventType::RightMouseDragged, 1)
        } else if held != 0 {
            (NSEventType::OtherMouseDragged, held.trailing_zeros() as isize)
        } else {
            (NSEventType::MouseMoved, 0)
        };
        let event = crate::event::mouse_event(kind, location, as_window(self), number, 0, modifiers);
        let moved = kind == NSEventType::MouseMoved;
        // Tracking areas hear of the move first, as AppKit's do.
        tracking::update(self, moved.then_some(&*event));
        if !moved || self.ivars().accepts_mouse_moved.get() {
            app::dispatch(&event);
        }
    }

    /// A scroll by `delta` at `x`, `y`; a touchpad gesture that ends fast
    /// enough coasts on (see `momentum`).
    pub(crate) fn scroll(
        &self,
        at: (f64, f64),
        delta: (f64, f64),
        wheel: bool,
        modifiers: Modifiers,
        gesture: Gesture,
    ) {
        let (phase, velocity, inverted) = gesture;
        let location = self.location(at.0, at.1);
        self.pointer_at(location);
        if phase != ScrollPhase::Ended && phase != ScrollPhase::Changed {
            crate::momentum::stop();
        }
        let phase = match phase {
            ScrollPhase::None => NSEventPhase::None,
            ScrollPhase::Began => NSEventPhase::Began,
            ScrollPhase::Changed => NSEventPhase::Changed,
            ScrollPhase::Ended => NSEventPhase::Ended,
        };
        let phases = (phase, NSEventPhase::None);
        let event = crate::event::scroll_event(location, as_window(self), delta, wheel, modifiers, phases, inverted);
        app::dispatch(&event);
        if phase == NSEventPhase::Ended {
            crate::momentum::start(as_window(self), location, velocity, modifiers, inverted);
        }
    }

    /// A pinch: magnify events throughout, and rotate events from when the
    /// fingers first turn.
    pub(crate) fn pinch(
        &self,
        at: (f64, f64),
        phase: ScrollPhase,
        magnification: f64,
        rotation: f64,
        modifiers: Modifiers,
    ) {
        let location = self.location(at.0, at.1);
        self.pointer_at(location);
        let this = as_window(self);
        let magnify = match phase {
            ScrollPhase::Began => NSEventPhase::Began,
            ScrollPhase::Ended | ScrollPhase::None => NSEventPhase::Ended,
            ScrollPhase::Changed => NSEventPhase::Changed,
        };
        let event =
            crate::event::gesture_event(NSEventType::Magnify, location, this, magnify, magnification, modifiers);
        app::dispatch(&event);
        let rotating = self.ivars().rotating.get();
        let rotate = match magnify {
            NSEventPhase::Changed if rotation != 0.0 && !rotating => Some(NSEventPhase::Began),
            NSEventPhase::Changed if rotating => Some(NSEventPhase::Changed),
            NSEventPhase::Ended if rotating => Some(NSEventPhase::Ended),
            _ => None,
        };
        if let Some(phase) = rotate {
            self.ivars().rotating.set(phase != NSEventPhase::Ended);
            let amount = if phase == NSEventPhase::Ended { 0.0 } else { rotation };
            let event = crate::event::gesture_event(NSEventType::Rotate, location, this, phase, amount, modifiers);
            app::dispatch(&event);
        }
    }

    pub(crate) fn pointer_left(&self) {
        self.ivars().pointer.set(None);
        // The pointer can't leave while a button holds it, unless the
        // compositor took it over (a window move): the press is over.
        if crate::event::pressed_buttons() != 0 {
            crate::event::release_all_buttons();
            let gone = self.ivars().mouse_view.take();
            drop(gone);
        }
        tracking::update(self, None);
    }

    /// Show the pointer as `cursor` over this window's content.
    pub(crate) fn set_cursor(&self, cursor: Cursor) {
        if self.ivars().visible.get() {
            app::send(ToRender::SetCursor { window: self.id(), cursor });
        }
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
    ivars.title_dirty.set(true);
    // The render thread's new window knows nothing of input methods yet.
    ivars.text_input.set((false, None));
    if ivars.popup.get().is_none() {
        ivars.popup.set(child_placement(window));
    }
    let size = ivars.size.get();
    // A new identity for the render thread: what it still has to say about
    // an earlier showing is about surfaces that are gone.
    static NEXT_SHOWING: AtomicU32 = AtomicU32::new(1);
    ivars.showing.set(NEXT_SHOWING.fetch_add(1, Ordering::Relaxed));
    app::add_window(as_window(window));
    app::send(ToRender::CreateWindow {
        window: window.id(),
        width: size.width.round().max(1.0) as u32,
        height: size.height.round().max(1.0) as u32,
        title: ivars.title.borrow().to_string(),
        style: window.render_style(),
        limits: window.limits(),
        popup: ivars.popup.get(),
    });
    let cursor = app::cursor();
    if cursor != Cursor::Default {
        window.set_cursor(cursor);
    }
    if let Some(parent) = window.transient_showing() {
        app::send(ToRender::SetParent { window: window.id(), parent: Some(parent) });
    }
    // The first time on screen, the initial first responder takes over from
    // the window, if it's still in it.
    let initial = {
        let mut settings = ivars.settings.borrow_mut();
        let first = !std::mem::replace(&mut settings.shown, true);
        if first { settings.initial_first_responder.as_ref().and_then(Weak::load) } else { None }
    };
    let unclaimed = ivars.first_responder.borrow().is_none();
    if let Some(view) = initial
        && unclaimed
        && views::window_of(views::imp(&view)).is_some_and(|w| std::ptr::eq(w, window))
    {
        make_first_responder(window, Some(&view));
    }
}

fn order_out(window: &NSWindowImpl) {
    let ivars = window.ivars();
    if !ivars.visible.get() {
        return;
    }
    // Children go first: a popup can't outlive its parent.
    let children = ivars.children.borrow().clone();
    for child in children {
        order_out(imp(&child));
    }
    app::window_hidden(as_window(window));
    ivars.visible.set(false);
    ivars.configured.set(false);
    ivars.frame_pending.set(false);
    ivars.popup.set(None);
    ivars.mouse_view.replace(None);
    for layer in ivars.layers.borrow_mut().values_mut() {
        layer.valid.clear();
    }
    app::send(ToRender::CloseWindow { window: window.id() });
    app::remove_window(as_window(window));
}

fn add_child(parent: &NSWindowImpl, child: &NSWindow) {
    let c = imp(child);
    // SAFETY: as in `parentWindow`.
    if let Some(old) = c.ivars().parent.get().map(|p| unsafe { p.as_ref() }.retain()) {
        remove_child(imp(&old), child);
    }
    parent.ivars().children.borrow_mut().push(child.retain());
    c.ivars().parent.set(Some(NonNull::from(as_window(parent))));
    if c.ivars().style.get().contains(NSWindowStyleMask::Titled) {
        c.set_transient(Some(as_window(parent)));
    }
    // A borderless child already on screen moves into its parent.
    if c.ivars().visible.get() && child_placement(c).is_some() {
        order_out(c);
        order_front(c);
    }
}

fn remove_child(parent: &NSWindowImpl, child: &NSWindow) {
    let removed: Vec<_> = {
        let mut children = parent.ivars().children.borrow_mut();
        let (gone, kept) = children.drain(..).partition(|c| std::ptr::eq(&**c, child));
        *children = kept;
        gone
    };
    if !removed.is_empty() {
        imp(child).ivars().parent.set(None);
        if imp(child).transient().is_some_and(|t| std::ptr::eq(&*t, as_window(parent))) {
            imp(child).set_transient(None);
        }
    }
    // Dropped outside the borrow: releasing may run arbitrary code.
    drop(removed);
}

/// Where a borderless child window goes on Wayland, which places windows
/// itself: a popup of its parent, where its frame is relative to the
/// parent's (both in AppKit's screen coordinates, which Sidestep keeps as
/// given since Wayland doesn't say where windows are).
fn child_placement(window: &NSWindowImpl) -> Option<PopupPlacement> {
    let ivars = window.ivars();
    if ivars.style.get().contains(NSWindowStyleMask::Titled) {
        return None;
    }
    // SAFETY: as in `parentWindow`.
    let parent = imp(unsafe { ivars.parent.get()?.as_ref() });
    if !parent.ivars().visible.get() {
        return None;
    }
    let content = parent.content_rect();
    let frame = window.content_rect();
    let x = (frame.origin.x - content.origin.x) as f32;
    let top = ((content.origin.y + content.size.height) - (frame.origin.y + frame.size.height)) as f32;
    Some(PopupPlacement {
        parent: parent.id(),
        anchor: Rect::new(x, top, x + 1.0, top + 1.0),
        below: false,
        grab: false,
    })
}

/// Show `window` as a popup of `parent`: an xdg_popup opening below
/// `anchor`, a rectangle in `parent`'s window coordinates, and taking the
/// pointer and keyboard grab if `grab` (as menus do; tooltips don't). The
/// compositor keeps it on screen and dismisses it when the user clicks
/// elsewhere, which orders it out. The infrastructure for menus and
/// tooltips; nothing uses it yet.
#[allow(dead_code)]
pub(crate) fn show_as_popup(window: &NSWindow, parent: &NSWindow, anchor: NSRect, grab: bool) {
    let (w, parent) = (imp(window), imp(parent));
    let h = parent.content_height();
    let top = h - (anchor.origin.y + anchor.size.height);
    let rect = Rect::new(
        anchor.origin.x as f32,
        top as f32,
        (anchor.origin.x + anchor.size.width) as f32,
        (top + anchor.size.height) as f32,
    );
    w.ivars().popup.set(Some(PopupPlacement { parent: parent.id(), anchor: rect, below: true, grab }));
    order_front(w);
}

fn discard_all_cursor_rects(view: &NSView) {
    view.discardCursorRects();
    for sub in views::subviews(views::imp(view)) {
        discard_all_cursor_rects(&sub);
    }
}

fn make_first_responder(window: &NSWindowImpl, responder: Option<&NSResponder>) -> bool {
    let this: &NSResponder = as_window(window);
    // The window itself stands for "no view".
    let responder = responder.filter(|r| !std::ptr::eq(*r, this));
    let current = window.ivars().first_responder.borrow().clone();
    let same = match (&current, responder) {
        (Some(c), Some(r)) => std::ptr::eq(&**c, r),
        (None, None) => true,
        _ => false,
    };
    if same {
        return true;
    }
    if let Some(current) = &current
        && !current.resignFirstResponder()
    {
        return false;
    }
    // A view outside this window can't take its keys: the window does.
    let foreign = responder
        .and_then(|r| r.downcast_ref::<NSView>())
        .is_some_and(|v| views::window_of(views::imp(v)).is_none_or(|w| !std::ptr::eq(w, window)));
    let responder = if foreign { None } else { responder };
    let accepted = responder.is_none_or(|r| r.becomeFirstResponder());
    let old = window.ivars().first_responder.replace(if accepted { responder.map(|r| r.retain()) } else { None });
    drop(old);
    window.tracking().borrow_mut().recheck();
    crate::inputcontext::update(window);
    accepted
}

/// The first responder, the window itself standing in for none.
fn first_responder(window: &NSWindowImpl) -> Retained<NSResponder> {
    as_window(window).firstResponder().expect("a window is at least its own first responder")
}

fn send_event(window: &NSWindowImpl, event: &NSEvent) {
    let kind = event.r#type();
    match kind {
        NSEventType::KeyDown => first_responder(window).keyDown(event),
        NSEventType::KeyUp => first_responder(window).keyUp(event),
        NSEventType::FlagsChanged => first_responder(window).flagsChanged(event),
        NSEventType::LeftMouseDown | NSEventType::RightMouseDown | NSEventType::OtherMouseDown => {
            let content = window.ivars().content.borrow().clone();
            let Some(view) = content.and_then(|c| c.hitTest(event.locationInWindow())) else { return };
            if view.acceptsFirstResponder() {
                as_window(window).makeFirstResponder(Some(&view));
            }
            let button = match kind {
                NSEventType::LeftMouseDown => Button::Left,
                NSEventType::RightMouseDown => Button::Right,
                _ => Button::Other(event.buttonNumber().clamp(2, 31) as u8),
            };
            // The first button down owns the drag. A press with no other
            // button held starts over, even if a nested loop took the last
            // mouse-up.
            let others = crate::event::pressed_buttons() & !(1 << event.buttonNumber().clamp(0, 31)) != 0;
            let owner = window.ivars().mouse_view.borrow().is_none() || !others;
            if owner {
                window.ivars().mouse_view.replace(Some((view.clone(), button)));
            }
            match kind {
                NSEventType::LeftMouseDown => view.mouseDown(event),
                NSEventType::RightMouseDown => view.rightMouseDown(event),
                _ => view.otherMouseDown(event),
            }
        }
        NSEventType::LeftMouseUp | NSEventType::RightMouseUp | NSEventType::OtherMouseUp => {
            let held = window.ivars().mouse_view.borrow().clone();
            let Some((view, owner)) = held else { return };
            let released = match kind {
                NSEventType::LeftMouseUp => Button::Left,
                NSEventType::RightMouseUp => Button::Right,
                _ => Button::Other(event.buttonNumber().clamp(2, 31) as u8),
            };
            if released == owner {
                window.ivars().mouse_view.replace(None);
            }
            match kind {
                NSEventType::LeftMouseUp => view.mouseUp(event),
                NSEventType::RightMouseUp => view.rightMouseUp(event),
                _ => view.otherMouseUp(event),
            }
        }
        NSEventType::LeftMouseDragged | NSEventType::RightMouseDragged | NSEventType::OtherMouseDragged => {
            let held = window.ivars().mouse_view.borrow().clone();
            let Some((view, _)) = held else { return };
            match kind {
                NSEventType::LeftMouseDragged => view.mouseDragged(event),
                NSEventType::RightMouseDragged => view.rightMouseDragged(event),
                _ => view.otherMouseDragged(event),
            }
        }
        NSEventType::MouseMoved => first_responder(window).mouseMoved(event),
        NSEventType::ScrollWheel
        | NSEventType::Magnify
        | NSEventType::Rotate
        | NSEventType::Swipe
        | NSEventType::SmartMagnify => {
            // To the view under the pointer.
            let content = window.ivars().content.borrow().clone();
            if let Some(view) = content.and_then(|c| c.hitTest(event.locationInWindow())) {
                match kind {
                    NSEventType::ScrollWheel => view.scrollWheel(event),
                    NSEventType::Magnify => view.magnifyWithEvent(event),
                    NSEventType::Rotate => view.rotateWithEvent(event),
                    NSEventType::Swipe => view.swipeWithEvent(event),
                    _ => view.smartMagnifyWithEvent(event),
                }
            }
        }
        _ => {}
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
    let id = window.id();
    if ivars.title_dirty.replace(false) {
        send_title(window);
    }

    let clips = ivars.clips.borrow().clone();
    for clip in &clips {
        update_scroll_layer(window, views::imp(clip));
    }

    let damage = ivars.damage.borrow_mut().remove(&ROOT_LAYER).unwrap_or_default();
    let content = ivars.content.borrow().clone();
    let color = background(window);
    for area in coalesce(damage) {
        graphics::begin_recording();
        graphics::push(Op::Fill { rect: area, color });
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

/// Send the title, set as drawing ops for the title bar when Sidestep
/// draws one. Done in the display pass, where no view is recording.
fn send_title(window: &NSWindowImpl) {
    let title = window.ivars().title.borrow().clone();
    let shown = window.ivars().settings.borrow().title_visibility == NSWindowTitleVisibility::Visible;
    let text = if window.titlebar() > 0.0 && shown { set_title_text(&title) } else { TitleText::default() };
    app::send(ToRender::SetTitle { window: window.id(), title: title.to_string(), text });
}

/// The title in the title bar's font, in white, from the top left.
fn set_title_text(title: &NSString) -> TitleText {
    if title.length() == 0 {
        return TitleText::default();
    }
    let font = NSFont::boldSystemFontOfSize(crate::backend::TITLE_SIZE);
    let white = NSColor::whiteColor();
    // SAFETY: the attribute names are constants this crate exports.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [&font, &white];
    let attributes = NSDictionary::from_slices(&keys, &values);
    // SAFETY: the dictionary maps attribute names to their values.
    let size = unsafe { title.sizeWithAttributes(Some(&attributes)) };
    let (width, height) = (size.width.ceil() as f32, size.height.ceil() as f32);
    graphics::begin_recording();
    // Top-left origin, y down, like a flipped view.
    graphics::set_view(Xf::IDENTITY, Rect::new(0.0, 0.0, width, height));
    // SAFETY: as above.
    unsafe { title.drawAtPoint_withAttributes(NSPoint::ZERO, Some(&attributes)) };
    TitleText { ops: graphics::end_recording(), width, height }
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

    app::send(ToRender::ScrollLayer { window: window.id(), layer: id, viewport, offset, doc_width });
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
        app::send(ToRender::DropTiles { window: window.id(), layer: id, tiles: far });
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

    let color = background(window);
    for area in areas {
        graphics::begin_recording();
        graphics::push(Op::Fill { rect: area, color });
        record(doc, doc_xf, doc_rect, area);
        let ops = graphics::end_recording();
        app::send(ToRender::Paint { window: window.id(), layer: id, rects: vec![area], ops });
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
