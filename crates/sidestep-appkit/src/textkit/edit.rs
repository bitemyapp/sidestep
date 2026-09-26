//! Edits a user makes in a text view, one transaction each, in AppKit's
//! order (as `conformance/tests/text_view.rs` records it on macOS):
//!
//! 1. `shouldChangeTextInRange:replacementString:`, which first begins
//!    editing if the view isn't yet (`textShouldBeginEditing:`, then
//!    `NSTextDidBeginEditingNotification`), then asks the delegate's
//!    `textView:shouldChangeTextInRange:replacementString:`, and then
//!    registers the undo, as AppKit does there: so a program's own
//!    `shouldChangeTextInRange:…`, edit and `didChangeText` is undoable
//!    too;
//! 2. the text storage edit, the new text in the typing attributes;
//! 3. the selection update (`textView:willChangeSelection…` and
//!    `NSTextViewDidChangeSelectionNotification`);
//! 4. `didChangeText` (`NSTextDidChangeNotification`), after which a view
//!    that isn't its window's first responder ends editing at once
//!    (`textShouldEndEditing:`, `NSTextDidEndEditingNotification`);
//! 5. one frame size change, and scrolling the selection into view.
//!
//! **Undo.** Typing and backward deletion coalesce into one undo action,
//! a run, while each edit ends where the run's text ends (a deletion that
//! reaches back past the run's start takes what it deletes into the run),
//! until the selection moves some other way, `breakUndoCoalescing`, or
//! the manager lets go of the run's action (`removeAllActions`). A
//! composition an input method commits joins the run it follows. Undoing
//! an edit puts back what it replaced and selects it; redoing puts the
//! edit's text back with the insertion point after it (an attribute
//! change selects its range both ways). Typing, backward deletion and the
//! insertion commands are named "Typing", pastes "Paste", cuts "Cut", and
//! the rest go unnamed, as on macOS. Nothing is registered, or named, while
//! the manager's registration is disabled.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{Message, msg_send};
use objc2_app_kit::{NSTextStorage, NSTextView};
use objc2_foundation::{NSAttributedString, NSMutableAttributedString, NSRange, NSString, NSUndoManager};

use super::text_view::NSTextViewImpl;

/// What an edit is, for undo's action name and whether it coalesces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Typing,
    Delete,
    Paste,
    Cut,
    Other,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Typing | Kind::Delete => "Typing",
            Kind::Paste => "Paste",
            Kind::Cut => "Cut",
            Kind::Other => "",
        }
    }

    fn coalesces(self) -> bool {
        matches!(self, Kind::Typing | Kind::Delete)
    }
}

/// How the next `shouldChangeTextInRange:…` registers undo: as the edit a
/// command is making, not at all (an input method's composition on its
/// way), or, for a program's own call, as an unnamed edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UndoAs {
    Program,
    Kind(Kind),
    Quiet,
}

/// An undo action a run of typing keeps extending: what the run put in
/// (its range now) and what it took out.
pub(crate) struct Typing {
    pub range: NSRange,
    pub replaced: Retained<NSAttributedString>,
    pub kind: Kind,
    /// Only attributes changed: undo and redo both select the range.
    pub attributes: bool,
    /// Whether the undo manager still holds the run's action: cleared when
    /// it lets go (the action's block owns an [`Alive`]).
    alive: Rc<Cell<bool>>,
}

/// Owned by a registered action's block: its drop tells the run its
/// action has gone.
struct Alive(Rc<Cell<bool>>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// Replace `range` with `text` as the user does, in a transaction. The new
/// text gets the typing attributes. False if the view or its delegate
/// refused.
pub(crate) fn user_replace(view: &NSTextViewImpl, range: NSRange, text: &NSString, kind: Kind) -> bool {
    replace(view, range, text, UndoAs::Kind(kind))
}

/// [`user_replace`] registering no undo: an input method's composition
/// on its way (see `input_client`).
pub(crate) fn user_replace_quietly(view: &NSTextViewImpl, range: NSRange, text: &NSString) -> bool {
    replace(view, range, text, UndoAs::Quiet)
}

fn replace(view: &NSTextViewImpl, range: NSRange, text: &NSString, undo: UndoAs) -> bool {
    let tv = view.as_text_view();
    view.set_undo_as(undo);
    // SAFETY: the method's own types; a subclass may override it.
    let ok: bool = unsafe { msg_send![tv, shouldChangeTextInRange: range, replacementString: Some(text)] };
    view.set_undo_as(UndoAs::Program);
    if !ok {
        return false;
    }
    let Some(storage) = view.storage() else { return false };
    let typing = view.typing_attributes_dict();
    // SAFETY: the typing attributes are an attribute dictionary.
    let new = unsafe { NSAttributedString::new_with_attributes(text, &typing) };
    let added = text.length();
    view.begin_transaction();
    storage.beginEditing();
    let m: &NSMutableAttributedString = &storage;
    m.replaceCharactersInRange_withAttributedString(range, &new);
    storage.endEditing();
    view.set_selection_internal(NSRange::new(range.location + added, 0), true);
    // SAFETY: didChangeText takes nothing; a subclass may override it.
    let _: () = unsafe { msg_send![tv, didChangeText] };
    view.end_transaction();
    true
}

/// The manager `view`'s edits register with now, if they register at all.
fn manager(view: &NSTextViewImpl) -> Option<Retained<NSUndoManager>> {
    if !view.allows_undo() {
        return None;
    }
    let um = view.text_undo_manager()?;
    let ok = !um.isUndoing() && !um.isRedoing() && um.isUndoRegistrationEnabled();
    ok.then_some(um)
}

/// `shouldChangeTextInRange:replacementString:` said yes: register the
/// undo of replacing `range` with `string` (none for an attribute
/// change), extending the run of typing it continues.
pub(crate) fn register_change(view: &NSTextViewImpl, range: NSRange, string: Option<&NSString>) {
    let kind = match view.undo_as() {
        UndoAs::Quiet => return,
        UndoAs::Kind(k) => k,
        UndoAs::Program => Kind::Other,
    };
    let Some(um) = manager(view) else {
        view.set_coalescing(None);
        return;
    };
    let Some(storage) = view.storage() else { return };
    if range.location.checked_add(range.length).is_none_or(|end| end > storage.length()) {
        // The edit itself will fail.
        return;
    }
    let attributes = string.is_none();
    let added = string.map_or(range.length, |s| s.length());
    if kind.coalesces()
        && !attributes
        && let Some(run) = view.coalescing()
        && run.borrow().alive.get()
        && extend(&mut run.borrow_mut(), &storage, range, added)
    {
        return;
    }
    let replaced = storage.attributedSubstringFromRange(range);
    let run = Rc::new(RefCell::new(Typing {
        range: NSRange::new(range.location, added),
        replaced,
        kind,
        attributes,
        alive: Rc::new(Cell::new(true)),
    }));
    register(&um, view, run.clone());
    view.set_coalescing(kind.coalesces().then_some(run));
}

/// Take an edit of `range` into `added` units into the run `t`, if it
/// ends where the run's text ends: typing there, or deleting back from
/// there.
fn extend(t: &mut Typing, storage: &NSTextStorage, range: NSRange, added: usize) -> bool {
    let end = t.range.location + t.range.length;
    if range.location + range.length != end {
        return false;
    }
    if range.length == 0 {
        t.range.length += added;
        return true;
    }
    if added != 0 {
        // Text replaced by other text: an action of its own.
        return false;
    }
    if range.location >= t.range.location {
        t.range.length -= range.length;
        return true;
    }
    // Deleting back past the run's start: what goes before it is what the
    // run replaced too.
    let before = storage.attributedSubstringFromRange(NSRange::new(range.location, t.range.location - range.location));
    let joined = NSMutableAttributedString::from_attributed_nsstring(&before);
    joined.appendAttributedString(&t.replaced);
    t.replaced = Retained::into_super(joined);
    t.range = NSRange::new(range.location, 0);
    true
}

/// Register the undo of a composition committed as `len` units at
/// `location`, which replaced `original`: it joins the run of typing it
/// follows, or starts one.
pub(crate) fn register_composition(
    view: &NSTextViewImpl,
    location: usize,
    len: usize,
    original: Retained<NSAttributedString>,
) {
    let Some(um) = manager(view) else {
        view.set_coalescing(None);
        return;
    };
    if original.length() == 0
        && let Some(run) = view.coalescing()
        && run.borrow().alive.get()
    {
        let mut t = run.borrow_mut();
        if t.range.location + t.range.length == location {
            t.range.length += len;
            return;
        }
    }
    let run = Rc::new(RefCell::new(Typing {
        range: NSRange::new(location, len),
        replaced: original,
        kind: Kind::Typing,
        attributes: false,
        alive: Rc::new(Cell::new(true)),
    }));
    register(&um, view, run.clone());
    view.set_coalescing(Some(run));
}

/// Register `run`'s undo with `um`, named for its kind.
fn register(um: &NSUndoManager, view: &NSTextViewImpl, run: Rc<RefCell<Typing>>) {
    let name = run.borrow().kind.name();
    let alive = Alive(run.borrow().alive.clone());
    let weak: Weak<NSTextViewImpl> = Weak::from_retained(&view.retain());
    let block = RcBlock::new(move |_target: std::ptr::NonNull<AnyObject>| {
        let _held = &alive;
        if let Some(view) = weak.load() {
            undo_run(&view, &run.borrow());
        }
    });
    let target: &NSObject = view.as_text_view();
    // SAFETY: the handler takes the target; the view keeps nothing the block
    // needs alive except through the weak reference.
    unsafe { um.registerUndoWithTarget_handler(target, &block) };
    if !name.is_empty() {
        um.setActionName(&NSString::from_str(name));
    }
}

/// Undo (or redo) an edit: put back what it replaced, register the
/// reverse, and select what came back (on redo, the insertion point goes
/// after it).
fn undo_run(view: &NSTextViewImpl, t: &Typing) {
    let Some(storage) = view.storage() else { return };
    let len = storage.length();
    let location = t.range.location.min(len);
    let range = NSRange::new(location, t.range.length.min(len - location));
    let current = storage.attributedSubstringFromRange(range);
    view.set_coalescing(None);
    view.discard_marked_text();
    let um = view.text_undo_manager();
    let redoing = um.as_ref().is_some_and(|um| um.isRedoing());
    if let Some(um) = &um {
        let reverse = Rc::new(RefCell::new(Typing {
            range: NSRange::new(range.location, t.replaced.length()),
            replaced: current,
            kind: t.kind,
            attributes: t.attributes,
            alive: Rc::new(Cell::new(true)),
        }));
        register(um, view, reverse);
    }
    view.begin_transaction();
    storage.beginEditing();
    let m: &NSMutableAttributedString = &storage;
    m.replaceCharactersInRange_withAttributedString(range, &t.replaced);
    storage.endEditing();
    let back = t.replaced.length();
    let select = if t.attributes || (!redoing && back > 0) {
        NSRange::new(range.location, back)
    } else {
        NSRange::new(range.location + back, 0)
    };
    view.set_selection_internal(select, true);
    // SAFETY: didChangeText takes nothing.
    let _: () = unsafe { msg_send![view.as_text_view(), didChangeText] };
    view.end_transaction();
}

/// Whether `view` is its window's first responder.
pub(crate) fn is_first_responder(view: &NSTextView) -> bool {
    let Some(window) = view.window() else { return false };
    window
        .firstResponder()
        .is_some_and(|r| std::ptr::eq(Retained::as_ptr(&r).cast::<AnyObject>(), (view as *const NSTextView).cast()))
}
