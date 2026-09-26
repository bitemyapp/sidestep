//! The field editor: the one text view per window that text fields and
//! other controls edit in, and the API controls start and end editing
//! with.
//!
//! **Which editor.** `-[NSWindow fieldEditor:forObject:]` asks the
//! window's delegate (`windowWillReturnFieldEditor:toObject:`) first; else
//! a secure text field (or its cell) gets the window's secure editor,
//! which lays its text out as bullets, one per character, and won't copy
//! or cut it; anything else gets the window's shared editor. Both are made
//! when first asked for with `createFlag` and kept by the window.
//!
//! **A session.** [`begin`] puts the editor in the control, inside a plain
//! clipping view of its own over the control's text rect (not a clip view,
//! so it draws in the window's layer), set up as AppKit's is (as measured
//! on macOS): plain text, no background, a container 40 000 points wide
//! that doesn't track the view, so a long line scrolls sideways; the
//! control's text, font, color and alignment; the control as delegate;
//! and makes it the window's first responder. The editor's delegate calls
//! (`textShouldBeginEditing:`, `textDidBeginEditing:`, `textDidChange:`,
//! `textShouldEndEditing:`, `textDidEndEditing:` with `NSTextMovement`)
//! are what the control turns into `controlTextDid…` for its delegate;
//! commands go to the control's `textView:doCommandBySelector:` first,
//! where a text field asks its delegate's
//! `control:textView:doCommandBySelector:`.
//!
//! **Ending.** Return, Tab and Backtab end editing with that movement in
//! `textDidEndEditing:`'s notification (Escape doesn't: the control's
//! delegate may take it). After Return the control goes on editing with
//! all its text selected, as AppKit's fields do; after Tab and Backtab the
//! window selects the next or previous key view, if it can. The editor
//! leaving first responder (a click elsewhere) ends editing and the
//! session. `abortEditing` ends it without taking the text,
//! `validateEditing` takes the text into the control's cell, and
//! `-[NSWindow endEditingFor:]` makes the window first responder.
//!
//! A session holds its control weakly and its clipping view strongly. One
//! whose editor is no longer its window's first responder (the control
//! left the window, which takes first responder away without asking) or
//! whose control has gone is over: it is ended, quietly, when next come
//! across, and the editor forgets it was editing. Ending a session touches
//! the control only while it is alive: a view's going leaves its
//! subviews' links to it dangling, so the clipping view of a control that
//! has gone is dropped, never taken out of it.
//!
//! **For controls.** The functions `select_text`, `edit_on_click`,
//! `edit_with_frame`, `select_with_frame`, `end_editing`,
//! `current_editor`, `abort_editing` and `validate_editing` match the
//! hooks `controls::text_field` calls, taking the cell and control as
//! objects. They reach the cell by message (`stringValue`, `font`,
//! `textColor`, `alignment`, `titleRectForBounds:`,
//! `setUpFieldEditorAttributes:`, `setStringValue:`), so they need nothing
//! of the controls' classes' insides.

// Nothing in this crate starts a session yet: the controls' hooks
// (`controls::text_field`) call `select_text` and the rest once the two
// workstreams are wired together.

use std::cell::RefCell;

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSEvent, NSResponder, NSText, NSTextView, NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize, NSString};

use super::commands::movement;
use super::notify::responds;
use super::text_view::{NSTextViewImpl, as_impl, new_text_view};

/// Keys a window keeps its editors under, as associated objects.
static SHARED_KEY: u8 = 1;
static SECURE_KEY: u8 = 2;

/// The field editor's container: wide enough for any one line.
const LINE_WIDTH: f64 = 40_000.0;

/// An editing session: an editor in a control.
struct Session {
    editor: Weak<NSTextView>,
    control: Weak<NSView>,
    cell: Option<Weak<AnyObject>>,
    clip: Retained<NSView>,
    /// Begun, and its editor not yet made first responder.
    starting: std::cell::Cell<bool>,
}

thread_local! {
    static SESSIONS: RefCell<Vec<Session>> = const { RefCell::new(Vec::new()) };
}

define_class!(
    /// The plain view that clips a field editor to its control's text
    /// rect.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "_SidestepFieldEditorClip"]
    struct EditorClip;

    impl EditorClip {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepWindowFieldEditor"]
    struct WindowFieldEditor;

    impl WindowFieldEditor {
        #[unsafe(method_id(fieldEditor:forObject:))]
        fn field_editor_for_object(&self, create: bool, object: Option<&AnyObject>) -> Option<Retained<NSText>> {
            // SAFETY: installed on NSWindow, so the receiver is one.
            let window = unsafe { &*(self as *const Self).cast::<NSWindow>() };
            field_editor(window, create, object)
        }

        #[unsafe(method(endEditingFor:))]
        fn end_editing_for(&self, object: Option<&AnyObject>) {
            // SAFETY: as above.
            let window = unsafe { &*(self as *const Self).cast::<NSWindow>() };
            end_editing_for(window, object);
        }
    }
);

sidestep_runtime::category!("NSWindow"(SidestepFieldEditor), |category| {
    // SAFETY: the helper's methods treat their receiver as a window.
    unsafe { category.add_methods_of(WindowFieldEditor::class()) };
});

fn key(k: &'static u8) -> *const std::ffi::c_void {
    (k as *const u8).cast()
}

fn associated(window: &NSWindow, k: &'static u8) -> Option<Retained<NSTextView>> {
    let obj = (window as *const NSWindow).cast::<AnyObject>();
    // SAFETY: the window is an object and the key a static address.
    let found = unsafe { objc2::ffi::objc_getAssociatedObject(obj, key(k)) };
    // SAFETY: only text views are associated under these keys, and the
    // window keeps them.
    unsafe { Retained::retain(found.cast::<NSTextView>().cast_mut()) }
}

fn associate(window: &NSWindow, k: &'static u8, editor: &NSTextView) {
    let obj = (window as *const NSWindow).cast::<AnyObject>().cast_mut();
    // SAFETY: as above; the window retains the editor.
    unsafe {
        objc2::ffi::objc_setAssociatedObject(
            obj,
            key(k),
            (editor as *const NSTextView).cast::<AnyObject>().cast_mut(),
            objc2::ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
    }
}

/// Whether `object` is a secure text field, or a secure field's cell.
fn is_secure(object: &AnyObject) -> bool {
    let mut class = Some(object.class());
    while let Some(c) = class {
        let name = c.name().to_str().unwrap_or("");
        if name == "NSSecureTextField" || name == "NSSecureTextFieldCell" {
            return true;
        }
        class = c.superclass();
    }
    false
}

/// `-[NSWindow fieldEditor:forObject:]`.
pub(crate) fn field_editor(window: &NSWindow, create: bool, object: Option<&AnyObject>) -> Option<Retained<NSText>> {
    if let Some(delegate) = window.delegate() {
        let sel = sel!(windowWillReturnFieldEditor:toObject:);
        if responds(delegate.as_ref(), sel) {
            // SAFETY: the delegate method takes the window and an object or
            // nil, and returns an object or nil.
            let theirs: Option<Retained<AnyObject>> =
                unsafe { msg_send![&*delegate, windowWillReturnFieldEditor: window, toObject: object] };
            if let Some(t) = theirs.and_then(|t| t.downcast::<NSText>().ok()) {
                return Some(t);
            }
        }
    }
    let secure = object.is_some_and(is_secure);
    let k = if secure { &SECURE_KEY } else { &SHARED_KEY };
    if let Some(e) = associated(window, k) {
        return Some(Retained::into_super(e));
    }
    if !create {
        return None;
    }
    let editor = new_text_view(MainThreadMarker::from(window), NSRect::ZERO);
    editor.setFieldEditor(true);
    if secure && let Some(v) = as_impl(&editor) {
        v.set_secure(true);
    }
    associate(window, k, &editor);
    Some(Retained::into_super(editor))
}

/// Start editing `control`'s text (its cell's, if it has one) with
/// `editor`, over `frame` (in the control's points), with `delegate`
/// hearing the editor, `selection` selected (all for none), and the editor
/// first responder.
pub(crate) fn begin(
    editor: &NSTextView,
    control: &NSView,
    cell: Option<&AnyObject>,
    frame: NSRect,
    delegate: Option<&AnyObject>,
    selection: Option<NSRange>,
) {
    prune();
    finish(editor);
    let mtm = MainThreadMarker::from(control);
    let clip: Retained<EditorClip> = {
        let this = EditorClip::alloc(mtm).set_ivars(());
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    };
    // SAFETY: EditorClip is an NSView subclass.
    let clip: Retained<NSView> = unsafe { Retained::cast_unchecked(clip) };
    control.addSubview(&clip);
    editor.setFrame(NSRect::new(NSPoint::ZERO, frame.size));
    editor.setMinSize(frame.size);
    editor.setMaxSize(NSSize::new(LINE_WIDTH, frame.size.height.max(LINE_WIDTH)));
    editor.setHorizontallyResizable(true);
    editor.setVerticallyResizable(true);
    editor.setRichText(false);
    editor.setDrawsBackground(false);
    editor.setEditable(true);
    editor.setSelectable(true);
    // SAFETY: textContainer takes nothing.
    if let Some(c) = unsafe { editor.textContainer() } {
        c.setWidthTracksTextView(false);
        c.setHeightTracksTextView(false);
        c.setSize(NSSize::new(LINE_WIDTH, 10_000_000.0));
    }
    let source = cell.unwrap_or(control);
    if let Some(font) = get::<objc2_app_kit::NSFont>(source, sel!(font)) {
        editor.setFont(Some(&font));
    }
    if let Some(color) = get::<objc2_app_kit::NSColor>(source, sel!(textColor)) {
        editor.setTextColor(Some(&color));
    }
    if responds(source, sel!(alignment)) {
        // SAFETY: alignment takes nothing and returns an NSTextAlignment.
        let a: objc2_app_kit::NSTextAlignment = unsafe { msg_send![source, alignment] };
        editor.setAlignment(a);
    }
    let text = get::<NSString>(source, sel!(stringValue)).unwrap_or_default();
    editor.setString(&text);
    clip.addSubview(editor);
    if let Some(cell) = cell
        && responds(cell, sel!(setUpFieldEditorAttributes:))
    {
        // SAFETY: the cell's method takes the editor and returns it.
        let _: Option<Retained<AnyObject>> = unsafe { msg_send![cell, setUpFieldEditorAttributes: editor] };
    }
    // SAFETY: setDelegate: takes any object as the delegate, weakly.
    let _: () = unsafe { msg_send![editor, setDelegate: delegate] };
    let len = text.length();
    let sel = selection.map_or(NSRange::new(0, len), |r| {
        let loc = r.location.min(len);
        NSRange::new(loc, r.length.min(len - loc))
    });
    editor.setSelectedRange(sel);
    SESSIONS.with(|s| {
        s.borrow_mut().push(Session {
            editor: Weak::new(editor),
            control: Weak::new(control),
            cell: cell.map(Weak::new),
            clip,
            starting: std::cell::Cell::new(true),
        })
    });
    if let Some(window) = control.window() {
        window.makeFirstResponder(Some(editor));
    }
    SESSIONS.with(|s| {
        for x in s.borrow().iter() {
            if x.editor.load().is_some_and(|e| std::ptr::eq(&*e, editor)) {
                x.starting.set(false);
            }
        }
    });
    editor.scrollRangeToVisible(sel);
}

/// Take `editor` out of the control it edits in, if it is in one.
pub(crate) fn finish(editor: &NSTextView) {
    let session = SESSIONS.with(|s| {
        let mut s = s.borrow_mut();
        let at = s.iter().position(|x| x.editor.load().is_some_and(|e| std::ptr::eq(&*e, editor)));
        at.map(|i| s.remove(i))
    });
    if let Some(session) = session {
        end_session(editor, session);
    }
}

/// Take the editor out of a session's clipping view, and that out of the
/// control only if the control is still alive; the editor forgets it was
/// editing.
fn end_session(editor: &NSTextView, session: Session) {
    editor.setDelegate(None);
    // SAFETY: superview takes nothing.
    let in_clip = unsafe { editor.superview() }.is_some_and(|s| std::ptr::eq(&*s, &*session.clip));
    if in_clip {
        // The clip view is alive (the session holds it), so this touches
        // nothing that may have gone.
        editor.removeFromSuperview();
    }
    if session.control.load().is_some() {
        session.clip.removeFromSuperview();
    }
    if let Some(v) = as_impl(editor) {
        v.reset_editing();
    }
    // A clip view whose control has gone is dropped here without being
    // taken out of it: its link to the control is never followed again.
    drop(session);
}

/// Whether a session is still on: its control alive and its editor its
/// window's first responder (or about to be).
fn is_live(x: &Session) -> bool {
    let (Some(editor), Some(_)) = (x.editor.load(), x.control.load()) else { return false };
    if x.starting.get() {
        return true;
    }
    let Some(window) = editor.window() else { return false };
    window
        .firstResponder()
        .is_some_and(|r| std::ptr::eq(Retained::as_ptr(&r).cast::<AnyObject>(), Retained::as_ptr(&editor).cast()))
}

/// End the sessions that are over (see the module's notes).
fn prune() {
    // Looked at with the list let go: finding out sends messages.
    let all: Vec<Session> = SESSIONS.with(|s| std::mem::take(&mut *s.borrow_mut()));
    let (live, over): (Vec<Session>, Vec<Session>) = all.into_iter().partition(is_live);
    SESSIONS.with(|s| {
        let mut s = s.borrow_mut();
        let newer = std::mem::replace(&mut *s, live);
        s.extend(newer);
    });
    for session in over {
        match session.editor.load() {
            Some(editor) => end_session(&editor, session),
            None => drop(session),
        }
    }
}

/// The editor and cell of the session `control` edits in, if any (one
/// that is over is ended first).
fn session_of(control: &AnyObject) -> Option<(Retained<NSTextView>, Option<Retained<AnyObject>>)> {
    prune();
    SESSIONS.with(|s| {
        s.borrow().iter().find_map(|x| {
            let c = x.control.load()?;
            if !std::ptr::eq(Retained::as_ptr(&c).cast::<AnyObject>(), control) {
                return None;
            }
            Some((x.editor.load()?, x.cell.as_ref().and_then(Weak::load)))
        })
    })
}

/// The control `editor` edits in.
fn control_of(editor: &NSTextView) -> Option<Retained<NSView>> {
    SESSIONS.with(|s| {
        s.borrow().iter().find_map(|x| {
            let e = x.editor.load()?;
            std::ptr::eq(&*e, editor).then(|| x.control.load()).flatten()
        })
    })
}

/// An object property read by message, if the object has it.
fn get<T: Message>(object: &AnyObject, sel: Sel) -> Option<Retained<T>> {
    if !responds(object, sel) {
        return None;
    }
    // SAFETY: the getters asked for (font, textColor, stringValue, cell)
    // take nothing and return an object or nil, of the type asked for.
    let value: *mut AnyObject = unsafe { objc2::runtime::MessageReceiver::send_message(object, sel, ()) };
    // SAFETY: as above; retained here from the getter's autoreleased
    // result.
    unsafe { Retained::retain(value.cast::<T>()) }
}

/// Before committed text goes into an editor: nothing to do yet.
pub(crate) fn before_insert(_v: &NSTextViewImpl) {}

/// Return, Tab or Backtab in a field editor: end editing with that
/// movement, then go on as AppKit does.
pub(crate) fn end_with_movement(v: &NSTextViewImpl, movement: isize) {
    let editor = v.as_text_view();
    let control = control_of(editor);
    if !v.end_editing_forced(movement) {
        return;
    }
    let Some(control) = control else { return };
    let still = control_of(editor).is_some_and(|c| std::ptr::eq(&*c, &*control));
    if !still {
        return;
    }
    match movement {
        movement::RETURN => {
            // Editing goes on, the text selected.
            let len = v.text_length();
            editor.setSelectedRange(NSRange::new(0, len));
        }
        movement::TAB | movement::BACKTAB => {
            let Some(window) = control.window() else { return };
            let sel = if movement == movement::TAB {
                sel!(selectKeyViewFollowingView:)
            } else {
                sel!(selectKeyViewPrecedingView:)
            };
            if responds(&window, sel) {
                // SAFETY: the key-view methods take a view.
                unsafe { objc2::runtime::MessageReceiver::send_message::<_, ()>(&*window, sel, (&*control,)) };
            }
        }
        _ => {}
    }
}

/// The field editor stopped being first responder: its session ends.
pub(crate) fn resigned(editor: &NSTextView) {
    finish(editor);
}

/// `-[NSWindow endEditingFor:]`: the window takes first responder from the
/// field editor.
fn end_editing_for(window: &NSWindow, object: Option<&AnyObject>) {
    let first = window.firstResponder();
    let editor = first.and_then(|r| r.downcast::<NSTextView>().ok()).filter(|t| t.isFieldEditor());
    let Some(editor) = editor else { return };
    if let Some(object) = object {
        let is = |o: Option<Retained<AnyObject>>| o.is_some_and(|o| std::ptr::eq(&*o, object));
        // SAFETY: a view is an object.
        let control = control_of(&editor).map(|c| unsafe { Retained::cast_unchecked::<AnyObject>(c) });
        // SAFETY: delegate takes nothing and returns an object or nil.
        let delegate: Option<Retained<AnyObject>> = unsafe { msg_send![&*editor, delegate] };
        let edits = is(control) || is(delegate) || std::ptr::eq(Retained::as_ptr(&editor).cast::<AnyObject>(), object);
        if !edits {
            return;
        }
    }
    if !window.makeFirstResponder(Some(window)) {
        // The editor wouldn't let go: take it out anyway.
        finish(&editor);
    }
}

// The hooks controls call.

/// `-[NSTextField selectText:]`, and a field becoming first responder:
/// editing with all its text selected.
pub(crate) fn select_text(control: &AnyObject, _sender: Option<&AnyObject>) {
    start(control, Some(NSRange::new(0, usize::MAX >> 1)));
}

/// A click in a selectable field: editing, with the click passed on to
/// place the insertion point.
pub(crate) fn edit_on_click(control: &AnyObject, event: &NSEvent) {
    if let Some(editor) = start(control, None) {
        editor.mouseDown(event);
    }
}

/// Start editing `control` in its window's field editor over its cell's
/// text rect (or just in its session's, if it is editing).
fn start(control: &AnyObject, selection: Option<NSRange>) -> Option<Retained<NSTextView>> {
    let view = control.downcast_ref::<NSView>()?;
    let window = view.window()?;
    if let Some((editor, _)) = session_of(control) {
        if let Some(sel) = selection {
            let len = editor.string().length();
            let loc = sel.location.min(len);
            editor.setSelectedRange(NSRange::new(loc, sel.length.min(len - loc)));
        }
        return Some(editor);
    }
    let editor = field_editor(&window, true, Some(control))?.downcast::<NSTextView>().ok()?;
    let cell = get::<AnyObject>(control, sel!(cell));
    let bounds = view.bounds();
    let frame = match &cell {
        Some(c) if responds(c, sel!(titleRectForBounds:)) => {
            // SAFETY: titleRectForBounds: takes and returns a rect.
            unsafe { msg_send![&**c, titleRectForBounds: bounds] }
        }
        _ => inset(bounds, 2.0),
    };
    let delegate: &AnyObject = view;
    begin(&editor, view, cell.as_deref(), frame, Some(delegate), selection);
    Some(editor)
}

fn inset(r: NSRect, by: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(r.origin.x + by, r.origin.y + by),
        NSSize::new((r.size.width - 2.0 * by).max(0.0), (r.size.height - 2.0 * by).max(0.0)),
    )
}

/// `-[NSCell editWithFrame:inView:editor:delegate:event:]`.
pub(crate) fn edit_with_frame(
    cell: &AnyObject,
    frame: NSRect,
    view: &NSView,
    editor: &AnyObject,
    delegate: Option<&AnyObject>,
    event: Option<&NSEvent>,
) {
    let Some(editor) = editor.downcast_ref::<NSTextView>() else { return };
    begin(editor, view, Some(cell), frame, delegate, None);
    if let Some(event) = event {
        editor.mouseDown(event);
    }
}

/// `-[NSCell selectWithFrame:inView:editor:delegate:start:length:]`.
pub(crate) fn select_with_frame(
    cell: &AnyObject,
    frame: NSRect,
    view: &NSView,
    editor: &AnyObject,
    delegate: Option<&AnyObject>,
    start: isize,
    length: isize,
) {
    let Some(editor) = editor.downcast_ref::<NSTextView>() else { return };
    let sel = NSRange::new(start.max(0) as usize, length.max(0) as usize);
    begin(editor, view, Some(cell), frame, delegate, Some(sel));
}

/// `-[NSCell endEditing:]`: the editor leaves the control.
pub(crate) fn end_editing(_cell: &AnyObject, editor: &AnyObject) {
    if let Some(editor) = editor.downcast_ref::<NSTextView>() {
        finish(editor);
    }
}

/// `-[NSControl currentEditor]`.
pub(crate) fn current_editor(control: &AnyObject) -> Option<Retained<AnyObject>> {
    // SAFETY: a text view is an object.
    session_of(control).map(|(e, _)| unsafe { Retained::cast_unchecked::<AnyObject>(e) })
}

/// `-[NSControl abortEditing]`: end editing without taking the text.
pub(crate) fn abort_editing(control: &AnyObject) -> bool {
    let Some((editor, _)) = session_of(control) else { return false };
    let window = editor.window();
    finish(&editor);
    if let Some(w) = window
        && w.firstResponder()
            .is_some_and(|r| std::ptr::eq(Retained::as_ptr(&r).cast::<AnyObject>(), Retained::as_ptr(&editor).cast()))
    {
        w.makeFirstResponder(Some(&w));
    }
    true
}

/// `-[NSControl validateEditing]`: the editor's text into the cell (or the
/// control).
pub(crate) fn validate_editing(control: &AnyObject) {
    let Some((editor, cell)) = session_of(control) else { return };
    let text = editor.string();
    let target: &AnyObject = cell.as_deref().unwrap_or(control);
    if responds(target, sel!(setStringValue:)) {
        // SAFETY: setStringValue: takes a string.
        let _: () = unsafe { msg_send![target, setStringValue: &*text] };
    }
}

/// Hooks for Linux tests of editing sessions (`tests/field_editor.rs`),
/// until the controls' own methods call the functions above.
#[doc(hidden)]
pub mod testing {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;

    /// `-[NSTextField selectText:]`: editing `control` in its window's
    /// field editor, all its text selected.
    pub fn select_text(control: &AnyObject) {
        super::select_text(control, None);
    }

    /// `-[NSControl currentEditor]`.
    pub fn current_editor(control: &AnyObject) -> Option<Retained<AnyObject>> {
        super::current_editor(control)
    }

    /// `-[NSControl abortEditing]`.
    pub fn abort_editing(control: &AnyObject) -> bool {
        super::abort_editing(control)
    }
}
