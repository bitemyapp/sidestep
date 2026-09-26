//! `NSUndoManager`: stacks of undo and redo groups.
//!
//! A group is a list of actions, each a target and selector with an
//! object, a block, a recorded invocation, or a nested group. Actions go
//! into the innermost open group; closing the outermost pushes it onto the
//! undo stack (onto the redo stack while undoing), keeping at most
//! `levelsOfUndo` groups when that is set. Undoing a group runs its
//! actions last first inside a new group, which collects what they
//! register and becomes the redo group; redoing does the reverse. A new
//! action registered outside an undo or redo clears the redo stack.
//! Targets aren't retained, as in Foundation (an invocation's arguments
//! are, its target isn't): an action whose target has gone is skipped.
//! While registration is disabled, grouping and naming do nothing either.
//!
//! Grouping by event follows what macOS does
//! (`conformance/tests/undo_manager.rs`): with `groupsByEvent`, the first
//! registration, `beginUndoGrouping` or `setActionName:` of a run-loop
//! turn opens an outer group first, and a run-loop observer (in
//! `runLoopModes`, before the loop waits or when a run ends, at
//! `NSUndoCloseGroupingRunLoopOrdering`) closes it. `undo` closes it
//! itself when it is the only group open.
//!
//! `prepareWithInvocationTarget:` returns an `NSProxy` that records the
//! next message it is sent as an invocation, through Foundation's
//! forwarding, and registers it.
//!
//! No `RefCell` borrow is held while an action runs, a notification is
//! posted or an action is dropped: each may call back into the manager
//! (dropping an action releases what it holds, whose `-dealloc` often
//! sends `removeAllActionsWithTarget:`).

use std::cell::{Cell, RefCell};

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, PartialInit, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSArray, NSInteger, NSInvocation, NSMethodSignature, NSProxy, NSString, NSUInteger};

use crate::runloop::{self, Activity, Mode, ObserverId};

sidestep_runtime::static_class!(pub NSUNDOMANAGER, NSUNDOMANAGER_META = "NSUndoManager", || {
    let _ = NSUndoManagerImpl::class();
});

crate::constant_string!(NSUndoManagerCheckpointNotification = "NSUndoManagerCheckpointNotification");
crate::constant_string!(NSUndoManagerWillUndoChangeNotification = "NSUndoManagerWillUndoChangeNotification");
crate::constant_string!(NSUndoManagerWillRedoChangeNotification = "NSUndoManagerWillRedoChangeNotification");
crate::constant_string!(NSUndoManagerDidUndoChangeNotification = "NSUndoManagerDidUndoChangeNotification");
crate::constant_string!(NSUndoManagerDidRedoChangeNotification = "NSUndoManagerDidRedoChangeNotification");
crate::constant_string!(NSUndoManagerDidOpenUndoGroupNotification = "NSUndoManagerDidOpenUndoGroupNotification");
crate::constant_string!(NSUndoManagerWillCloseUndoGroupNotification = "NSUndoManagerWillCloseUndoGroupNotification");
crate::constant_string!(NSUndoManagerDidCloseUndoGroupNotification = "NSUndoManagerDidCloseUndoGroupNotification");
crate::constant_string!(NSUndoManagerGroupIsDiscardableKey = "NSUndoManagerGroupIsDiscardableKey");

/// The notifications a manager posts, with itself as the object.
#[derive(Clone, Copy)]
enum Note {
    Checkpoint,
    WillUndo,
    WillRedo,
    DidUndo,
    DidRedo,
    DidOpen,
    WillClose,
    DidClose,
}

/// Where macOS closes the event's group, among a run loop's observers.
const CLOSE_GROUPING_ORDER: isize = 350_000;

enum Action {
    Selector { target: Weak<AnyObject>, target_ptr: usize, selector: Sel, object: Option<Retained<AnyObject>> },
    Block { target: Weak<AnyObject>, target_ptr: usize, block: RcBlock<dyn Fn(std::ptr::NonNull<AnyObject>)> },
    Invocation { target: Weak<AnyObject>, target_ptr: usize, invocation: Retained<NSInvocation> },
    Group(Group),
}

impl Action {
    fn targets(&self, ptr: usize) -> bool {
        match self {
            Action::Selector { target_ptr, .. }
            | Action::Block { target_ptr, .. }
            | Action::Invocation { target_ptr, .. } => *target_ptr == ptr,
            Action::Group(_) => false,
        }
    }
}

#[derive(Default)]
struct Group {
    actions: Vec<Action>,
    name: String,
    discardable: bool,
    info: Vec<(Retained<NSString>, Retained<AnyObject>)>,
}

impl Group {
    /// Move every action for the target at `ptr` into `out`, from nested
    /// groups too, and nested groups left empty.
    fn remove_target(&mut self, ptr: usize, out: &mut Vec<Action>) {
        for a in std::mem::take(&mut self.actions) {
            match a {
                Action::Group(mut g) => {
                    g.remove_target(ptr, out);
                    if g.actions.is_empty() {
                        out.push(Action::Group(g));
                    } else {
                        self.actions.push(Action::Group(g));
                    }
                }
                a if a.targets(ptr) => out.push(a),
                a => self.actions.push(a),
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Undoing,
    Redoing,
}

pub(crate) struct Ivars {
    undo: RefCell<Vec<Group>>,
    redo: RefCell<Vec<Group>>,
    /// The open groups, outermost first.
    open: RefCell<Vec<Group>>,
    disabled: Cell<usize>,
    groups_by_event: Cell<bool>,
    levels: Cell<usize>,
    phase: Cell<Phase>,
    /// A group this turn of the run loop opened by itself, to be closed
    /// when the turn ends.
    event_group: Cell<bool>,
    observer: Cell<Option<(ObserverId, runloop::RunLoop)>>,
    modes: RefCell<Retained<NSArray<NSString>>>,
}

impl Drop for Ivars {
    fn drop(&mut self) {
        if let Some((id, rl)) = self.observer.take()
            && rl.is_current()
        {
            rl.remove_observer(id);
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSUndoManager"]
    #[ivars = Ivars]
    pub(crate) struct NSUndoManagerImpl;

    impl NSUndoManagerImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the constant is this crate's string.
            let default = unsafe { objc2_foundation::NSDefaultRunLoopMode };
            let modes = NSArray::from_slice(&[default]);
            let this = this.set_ivars(Ivars {
                undo: RefCell::default(),
                redo: RefCell::default(),
                open: RefCell::default(),
                disabled: Cell::new(0),
                groups_by_event: Cell::new(true),
                levels: Cell::new(0),
                phase: Cell::new(Phase::Idle),
                event_group: Cell::new(false),
                observer: Cell::new(None),
                modes: RefCell::new(modes),
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        // Grouping.

        #[unsafe(method(beginUndoGrouping))]
        fn begin_undo_grouping(&self) {
            if self.ivars().disabled.get() > 0 {
                return;
            }
            self.open_event_group();
            self.begin();
        }

        #[unsafe(method(endUndoGrouping))]
        fn end_undo_grouping(&self) {
            if self.ivars().disabled.get() > 0 {
                return;
            }
            self.end();
        }

        #[unsafe(method(groupingLevel))]
        fn grouping_level(&self) -> NSInteger {
            self.ivars().open.borrow().len() as NSInteger
        }

        #[unsafe(method(disableUndoRegistration))]
        fn disable_undo_registration(&self) {
            let d = &self.ivars().disabled;
            d.set(d.get() + 1);
        }

        #[unsafe(method(enableUndoRegistration))]
        fn enable_undo_registration(&self) {
            let d = &self.ivars().disabled;
            d.set(d.get().saturating_sub(1));
        }

        #[unsafe(method(isUndoRegistrationEnabled))]
        fn is_undo_registration_enabled(&self) -> bool {
            self.ivars().disabled.get() == 0
        }

        #[unsafe(method(groupsByEvent))]
        fn groups_by_event(&self) -> bool {
            self.ivars().groups_by_event.get()
        }

        #[unsafe(method(setGroupsByEvent:))]
        fn set_groups_by_event(&self, flag: bool) {
            self.ivars().groups_by_event.set(flag);
        }

        #[unsafe(method(levelsOfUndo))]
        fn levels_of_undo(&self) -> NSUInteger {
            self.ivars().levels.get()
        }

        #[unsafe(method(setLevelsOfUndo:))]
        fn set_levels_of_undo(&self, levels: NSUInteger) {
            self.ivars().levels.set(levels);
            let dropped = (
                trim(&mut self.ivars().undo.borrow_mut(), levels),
                trim(&mut self.ivars().redo.borrow_mut(), levels),
            );
            drop(dropped);
        }

        #[unsafe(method_id(runLoopModes))]
        fn run_loop_modes(&self) -> Retained<NSArray<NSString>> {
            self.ivars().modes.borrow().clone()
        }

        #[unsafe(method(setRunLoopModes:))]
        fn set_run_loop_modes(&self, modes: &NSArray<NSString>) {
            // SAFETY: -copy of an array is an immutable array of the same
            // elements.
            let copy: Retained<NSArray<NSString>> = unsafe { msg_send![modes, copy] };
            *self.ivars().modes.borrow_mut() = copy;
            // Observe again in the new modes.
            if let Some((id, rl)) = self.ivars().observer.take()
                && rl.is_current()
            {
                rl.remove_observer(id);
            }
            if self.ivars().event_group.get() {
                self.observe();
            }
        }

        // Undoing and redoing.

        #[unsafe(method(undo))]
        fn undo(&self) {
            self.close_for_undo("undo");
            self.perform_top(Phase::Undoing);
        }

        #[unsafe(method(redo))]
        fn redo(&self) {
            self.close_for_undo("redo");
            self.perform_top(Phase::Redoing);
        }

        #[unsafe(method(undoNestedGroup))]
        fn undo_nested_group(&self) {
            let nested = {
                let mut open = self.ivars().open.borrow_mut();
                match open.last_mut() {
                    None => None,
                    Some(g) => match g.actions.pop() {
                        Some(Action::Group(nested)) => Some(nested),
                        Some(other) => {
                            g.actions.push(other);
                            drop(open);
                            panic!("-[NSUndoManager undoNestedGroup]: an action was registered since the last group closed");
                        }
                        None => None,
                    },
                }
            };
            match nested {
                Some(group) => self.perform(group, Phase::Undoing),
                None => self.perform_top(Phase::Undoing),
            }
        }

        #[unsafe(method(canUndo))]
        fn can_undo(&self) -> bool {
            !self.ivars().undo.borrow().is_empty() || !self.ivars().open.borrow().is_empty()
        }

        #[unsafe(method(canRedo))]
        fn can_redo(&self) -> bool {
            !self.ivars().redo.borrow().is_empty()
        }

        #[unsafe(method(undoCount))]
        fn undo_count(&self) -> NSUInteger {
            self.ivars().undo.borrow().len()
        }

        #[unsafe(method(redoCount))]
        fn redo_count(&self) -> NSUInteger {
            self.ivars().redo.borrow().len()
        }

        #[unsafe(method(isUndoing))]
        fn is_undoing(&self) -> bool {
            self.ivars().phase.get() == Phase::Undoing
        }

        #[unsafe(method(isRedoing))]
        fn is_redoing(&self) -> bool {
            self.ivars().phase.get() == Phase::Redoing
        }

        /// Everything goes, the open groups too (the turn's included), as
        /// on macOS: nothing is left to undo, and nothing is open.
        #[unsafe(method(removeAllActions))]
        fn remove_all_actions(&self) {
            let iv = self.ivars();
            // Dropped once the borrows end: releasing an action's object
            // may run code.
            let gone = (iv.undo.take(), iv.redo.take(), iv.open.take());
            iv.event_group.set(false);
            iv.disabled.set(0);
            drop(gone);
        }

        #[unsafe(method(removeAllActionsWithTarget:))]
        fn remove_all_actions_with_target(&self, target: &AnyObject) {
            let ptr = target as *const AnyObject as usize;
            let iv = self.ivars();
            // What goes is dropped once no borrow is held.
            let mut removed: Vec<Action> = Vec::new();
            let mut emptied: Vec<Group> = Vec::new();
            for stack in [&iv.undo, &iv.redo] {
                let mut stack = stack.borrow_mut();
                for mut g in std::mem::take(&mut *stack) {
                    g.remove_target(ptr, &mut removed);
                    if g.actions.is_empty() {
                        emptied.push(g);
                    } else {
                        stack.push(g);
                    }
                }
            }
            for g in iv.open.borrow_mut().iter_mut() {
                g.remove_target(ptr, &mut removed);
            }
            drop((removed, emptied));
        }

        // Registering.

        #[unsafe(method(registerUndoWithTarget:selector:object:))]
        fn register_selector(&self, target: &AnyObject, selector: Sel, object: Option<&AnyObject>) {
            use objc2::Message;
            self.register(Action::Selector {
                target: Weak::new(target),
                target_ptr: target as *const AnyObject as usize,
                selector,
                object: object.map(|o| o.retain()),
            });
        }

        #[unsafe(method(registerUndoWithTarget:handler:))]
        fn register_handler(&self, target: &AnyObject, handler: &DynBlock<dyn Fn(std::ptr::NonNull<AnyObject>)>) {
            self.register(Action::Block {
                target: Weak::new(target),
                target_ptr: target as *const AnyObject as usize,
                block: handler.copy(),
            });
        }

        #[unsafe(method_id(prepareWithInvocationTarget:))]
        fn prepare_with_invocation_target(&self, target: &AnyObject) -> Retained<AnyObject> {
            new_proxy(self, target)
        }

        // Names and user info.

        #[unsafe(method(setActionName:))]
        fn set_action_name(&self, name: &NSString) {
            if self.ivars().disabled.get() > 0 {
                return;
            }
            let name = name.to_string();
            self.with_current_group(|g| g.name = name);
        }

        #[unsafe(method_id(undoActionName))]
        fn undo_action_name(&self) -> Retained<NSString> {
            NSString::from_str(&self.undo_name())
        }

        #[unsafe(method_id(redoActionName))]
        fn redo_action_name(&self) -> Retained<NSString> {
            let name = self.ivars().redo.borrow().last().map(|g| g.name.clone()).unwrap_or_default();
            NSString::from_str(&name)
        }

        #[unsafe(method_id(undoMenuItemTitle))]
        fn undo_menu_item_title(&self) -> Retained<NSString> {
            NSString::from_str(&menu_title("Undo", &self.undo_name()))
        }

        #[unsafe(method_id(redoMenuItemTitle))]
        fn redo_menu_item_title(&self) -> Retained<NSString> {
            let name = self.ivars().redo.borrow().last().map(|g| g.name.clone()).unwrap_or_default();
            NSString::from_str(&menu_title("Redo", &name))
        }

        #[unsafe(method_id(undoMenuTitleForUndoActionName:))]
        fn undo_menu_title_for(&self, name: &NSString) -> Retained<NSString> {
            NSString::from_str(&format!("Undo {name}"))
        }

        #[unsafe(method_id(redoMenuTitleForUndoActionName:))]
        fn redo_menu_title_for(&self, name: &NSString) -> Retained<NSString> {
            NSString::from_str(&format!("Redo {name}"))
        }

        #[unsafe(method(setActionIsDiscardable:))]
        fn set_action_is_discardable(&self, flag: bool) {
            if self.ivars().disabled.get() > 0 {
                return;
            }
            self.with_current_group(|g| g.discardable = flag);
        }

        #[unsafe(method(undoActionIsDiscardable))]
        fn undo_action_is_discardable(&self) -> bool {
            self.ivars().undo.borrow().last().is_some_and(|g| g.discardable)
        }

        #[unsafe(method(redoActionIsDiscardable))]
        fn redo_action_is_discardable(&self) -> bool {
            self.ivars().redo.borrow().last().is_some_and(|g| g.discardable)
        }

        #[unsafe(method(setActionUserInfoValue:forKey:))]
        fn set_action_user_info_value(&self, value: Option<&AnyObject>, key: &NSString) {
            use objc2::Message;
            if self.ivars().disabled.get() > 0 {
                return;
            }
            let (key, value) = (key.copy_string(), value.map(|v| v.retain()));
            self.with_current_group(move |g| {
                g.info.retain(|(k, _)| !k.isEqualToString(&key));
                if let Some(v) = value {
                    g.info.push((key, v));
                }
            });
        }

        #[unsafe(method_id(undoActionUserInfoValueForKey:))]
        fn undo_action_user_info_value(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            let undo = self.ivars().undo.borrow();
            undo.last().and_then(|g| g.info.iter().find(|(k, _)| k.isEqualToString(key)).map(|(_, v)| v.clone()))
        }

        #[unsafe(method_id(redoActionUserInfoValueForKey:))]
        fn redo_action_user_info_value(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            let redo = self.ivars().redo.borrow();
            redo.last().and_then(|g| g.info.iter().find(|(k, _)| k.isEqualToString(key)).map(|(_, v)| v.clone()))
        }
    }

    unsafe impl NSObjectProtocol for NSUndoManagerImpl {}
);

trait CopyString {
    fn copy_string(&self) -> Retained<NSString>;
}

impl CopyString for NSString {
    fn copy_string(&self) -> Retained<NSString> {
        // SAFETY: -copy of a string is an immutable string.
        unsafe { msg_send![self, copy] }
    }
}

/// "Undo" alone for an unnamed action, else "Undo Name".
fn menu_title(verb: &str, name: &str) -> String {
    if name.is_empty() { verb.to_string() } else { format!("{verb} {name}") }
}

/// Keep at most `levels` groups (all for 0): the oldest are taken out,
/// for the caller to drop once its borrow ends.
#[must_use]
fn trim(stack: &mut Vec<Group>, levels: usize) -> Vec<Group> {
    if levels > 0 && stack.len() > levels {
        let excess = stack.len() - levels;
        return stack.drain(..excess).collect();
    }
    Vec::new()
}

impl NSUndoManagerImpl {
    fn post(&self, name: Note) {
        // SAFETY: the names are constant strings this module exports.
        let name: &NSString = unsafe {
            match name {
                Note::Checkpoint => objc2_foundation::NSUndoManagerCheckpointNotification,
                Note::WillUndo => objc2_foundation::NSUndoManagerWillUndoChangeNotification,
                Note::WillRedo => objc2_foundation::NSUndoManagerWillRedoChangeNotification,
                Note::DidUndo => objc2_foundation::NSUndoManagerDidUndoChangeNotification,
                Note::DidRedo => objc2_foundation::NSUndoManagerDidRedoChangeNotification,
                Note::DidOpen => objc2_foundation::NSUndoManagerDidOpenUndoGroupNotification,
                Note::WillClose => objc2_foundation::NSUndoManagerWillCloseUndoGroupNotification,
                Note::DidClose => objc2_foundation::NSUndoManagerDidCloseUndoGroupNotification,
            }
        };
        let object: &AnyObject = self;
        crate::notification_center::post(name, Some(object), None);
    }

    /// With grouping by event, open the turn's group if it has none yet.
    fn open_event_group(&self) {
        let iv = self.ivars();
        if !iv.groups_by_event.get() || iv.event_group.get() || iv.phase.get() != Phase::Idle {
            return;
        }
        if !iv.open.borrow().is_empty() {
            return;
        }
        iv.event_group.set(true);
        self.observe();
        self.begin();
    }

    /// Close the event's group when the run loop's turn ends.
    fn observe(&self) {
        let iv = self.ivars();
        let current = iv.observer.take();
        if let Some(o) = current {
            iv.observer.set(Some(o));
            return;
        }
        let rl = runloop::current();
        let modes: Vec<Mode> = iv.modes.borrow().iter().map(|m| Mode::from_ns(&m)).collect();
        let weak = Weak::from_retained(&self.retain_self());
        let id = rl.add_observer(&modes, Activity::BEFORE_WAITING | Activity::EXIT, CLOSE_GROUPING_ORDER, move |_| {
            if let Some(this) = weak.load() {
                this.close_event_group();
            }
        });
        iv.observer.set(Some((id, rl)));
    }

    fn retain_self(&self) -> Retained<Self> {
        use objc2::Message;
        self.retain()
    }

    fn close_event_group(&self) {
        let iv = self.ivars();
        if !iv.event_group.replace(false) {
            return;
        }
        if iv.open.borrow().len() == 1 {
            self.end();
        }
    }

    /// Open a group; `notify` for the grouping notifications (the group
    /// an undo or redo collects into goes without them, as on macOS).
    fn begin_with(&self, notify: bool) {
        self.ivars().open.borrow_mut().push(Group::default());
        if notify {
            self.post(Note::DidOpen);
            self.post(Note::Checkpoint);
        }
    }

    fn begin(&self) {
        self.begin_with(true);
    }

    fn end(&self) {
        self.end_with(true);
    }

    fn end_with(&self, notify: bool) {
        if self.ivars().open.borrow().is_empty() {
            panic!("-[NSUndoManager endUndoGrouping]: endUndoGrouping without beginUndoGrouping");
        }
        if notify {
            self.post(Note::Checkpoint);
            self.post(Note::WillClose);
        }
        let iv = self.ivars();
        let group = iv.open.borrow_mut().pop().expect("checked above");
        let finished = {
            let mut open = iv.open.borrow_mut();
            match open.last_mut() {
                Some(outer) => {
                    outer.actions.push(Action::Group(group));
                    None
                }
                None => Some(group),
            }
        };
        if let Some(group) = finished {
            match iv.phase.get() {
                // What an undo registers is for redoing; an undo that
                // registered nothing leaves nothing to redo.
                Phase::Undoing if group.actions.is_empty() => {}
                Phase::Undoing => self.push(&iv.redo, group),
                _ => self.push(&iv.undo, group),
            }
        }
        if notify {
            self.post(Note::DidClose);
        }
    }

    fn push(&self, stack: &RefCell<Vec<Group>>, group: Group) {
        let dropped = {
            let mut stack = stack.borrow_mut();
            stack.push(group);
            trim(&mut stack, self.ivars().levels.get())
        };
        drop(dropped);
    }

    /// Add an action to the innermost open group, opening the event's group
    /// first if need be.
    fn register(&self, action: Action) {
        let iv = self.ivars();
        if iv.disabled.get() > 0 {
            return;
        }
        self.open_event_group();
        if iv.open.borrow().is_empty() {
            panic!("-[NSUndoManager registerUndoWithTarget:…]: must begin a group before registering undo");
        }
        let cleared = if iv.phase.get() == Phase::Idle { iv.redo.take() } else { Vec::new() };
        iv.open.borrow_mut().last_mut().expect("checked above").actions.push(action);
        drop(cleared);
    }

    /// Run `f` on the outermost open group, which becomes the next undo or
    /// redo group, opening the event's group first if need be.
    fn with_current_group(&self, f: impl FnOnce(&mut Group)) {
        self.open_event_group();
        if let Some(g) = self.ivars().open.borrow_mut().first_mut() {
            f(g);
        }
    }

    fn undo_name(&self) -> String {
        let iv = self.ivars();
        if let Some(g) = iv.open.borrow().first() {
            return g.name.clone();
        }
        iv.undo.borrow().last().map(|g| g.name.clone()).unwrap_or_default()
    }

    /// Before undoing or redoing: close the event's group, the only group
    /// that may be open then.
    fn close_for_undo(&self, what: &str) {
        let iv = self.ivars();
        let level = iv.open.borrow().len();
        if level == 0 {
            return;
        }
        if level == 1 && iv.groups_by_event.get() {
            iv.event_group.set(false);
            self.end();
            return;
        }
        panic!("-[NSUndoManager {what}]: {what} was called with too many nested undo groups");
    }

    /// Undo (or redo) the top group of the undo (or redo) stack.
    fn perform_top(&self, phase: Phase) {
        let iv = self.ivars();
        let stack = if phase == Phase::Undoing { &iv.undo } else { &iv.redo };
        let Some(group) = stack.borrow_mut().pop() else { return };
        self.perform(group, phase);
    }

    /// Run a group's actions, last first, inside a group that collects
    /// what they register, named as the group was.
    fn perform(&self, group: Group, phase: Phase) {
        let iv = self.ivars();
        self.post(Note::Checkpoint);
        let (will, did) =
            if phase == Phase::Undoing { (Note::WillUndo, Note::DidUndo) } else { (Note::WillRedo, Note::DidRedo) };
        self.post(will);
        let previous = iv.phase.replace(phase);
        self.begin_with(false);
        if let Some(g) = iv.open.borrow_mut().last_mut() {
            g.name.clone_from(&group.name);
            g.discardable = group.discardable;
        }
        run_actions(group.actions);
        self.end_with(false);
        iv.phase.set(previous);
        self.post(Note::Checkpoint);
        self.post(did);
    }
}

/// Run actions last first, nested groups the same way.
fn run_actions(actions: Vec<Action>) {
    for action in actions.into_iter().rev() {
        match action {
            Action::Selector { target, selector, object, .. } => {
                if let Some(target) = target.load() {
                    // SAFETY: an undo selector takes one object argument
                    // and returns nothing, as Foundation requires.
                    unsafe {
                        objc2::runtime::MessageReceiver::send_message::<_, ()>(&*target, selector, (object.as_deref(),))
                    };
                }
            }
            Action::Block { target, block, .. } => {
                if let Some(target) = target.load() {
                    block.call((std::ptr::NonNull::from(&*target),));
                }
            }
            Action::Invocation { target, invocation, .. } => {
                if let Some(target) = target.load() {
                    // SAFETY: the invocation was recorded from a message to
                    // the target with that message's signature, and holds its
                    // arguments; it goes back to holding no target after.
                    unsafe {
                        invocation.setTarget(Some(&target));
                        invocation.invoke();
                        invocation.setTarget(None);
                    }
                }
            }
            Action::Group(g) => run_actions(g.actions),
        }
    }
}

// prepareWithInvocationTarget:'s proxy.

pub(crate) struct ProxyIvars {
    manager: Weak<NSUndoManagerImpl>,
    target: Weak<AnyObject>,
    target_ptr: usize,
}

define_class!(
    #[unsafe(super(NSProxy))]
    #[name = "_SidestepUndoProxy"]
    #[ivars = ProxyIvars]
    struct UndoProxy;

    impl UndoProxy {
        #[unsafe(method_id(methodSignatureForSelector:))]
        fn method_signature(&self, sel: Sel) -> Option<Retained<NSMethodSignature>> {
            let target = self.ivars().target.load();
            // SAFETY: every object answers methodSignatureForSelector:.
            target.and_then(|t| unsafe { msg_send![&*t, methodSignatureForSelector: sel] })
        }

        #[unsafe(method(forwardInvocation:))]
        fn forward_invocation(&self, invocation: &NSInvocation) {
            let iv = self.ivars();
            let (Some(manager), Some(target)) = (iv.manager.load(), iv.target.load()) else { return };
            // SAFETY: retaining the arguments keeps them for the undo; the
            // target (the proxy, as sent) is taken out first, so neither it
            // nor the real target is retained: the target is set again
            // when the action runs.
            unsafe {
                invocation.setTarget(None);
                invocation.retainArguments();
            }
            use objc2::Message;
            manager.register(Action::Invocation {
                target: Weak::from_retained(&target),
                target_ptr: iv.target_ptr,
                invocation: invocation.retain(),
            });
        }
    }
);

/// A proxy recording messages for `target` into `manager`. NSProxy has no
/// `-init` (it would be forwarded), so the proxy is used as allocated.
fn new_proxy(manager: &NSUndoManagerImpl, target: &AnyObject) -> Retained<AnyObject> {
    let ivars = ProxyIvars {
        manager: Weak::from_retained(&manager.retain_self()),
        target: Weak::new(target),
        target_ptr: target as *const AnyObject as usize,
    };
    let mut this: PartialInit<UndoProxy> = UndoProxy::alloc().set_ivars(ivars);
    let ptr = PartialInit::as_mut_ptr(&mut this);
    std::mem::forget(this);
    // SAFETY: an allocated proxy with its instance variables set; the
    // allocation's reference is handed over.
    let proxy = unsafe { Retained::from_raw(ptr) }.expect("an allocated proxy");
    // SAFETY: a proxy is an object.
    unsafe { Retained::cast_unchecked(proxy) }
}
