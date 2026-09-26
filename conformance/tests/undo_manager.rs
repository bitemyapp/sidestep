//! NSUndoManager: groups and levels, grouping by run-loop turn, undo and
//! redo stacks, the three ways to register (target and selector, block,
//! invocation), action names and menu titles, disabling, and the
//! notifications around it all. Expected values are what macOS does.
//!
//! objc2 marks NSUndoManager as main-thread only, so this file has its
//! own `main`. The run loop runs between steps to end a turn, as an event
//! does.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSDate, NSNotification, NSNotificationCenter, NSObjectProtocol, NSRunLoop, NSString, NSUndoManager,
};

use sidestep as _;

#[derive(Default)]
struct Log {
    events: RefCell<Vec<String>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "UndoTestTarget"]
    #[ivars = Log]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(setValue:))]
        fn set_value(&self, v: Option<&AnyObject>) {
            let s = v.and_then(|v| v.downcast_ref::<NSString>()).map(|s| s.to_string()).unwrap_or_default();
            self.ivars().events.borrow_mut().push(format!("set {s}"));
        }

        #[unsafe(method(note:))]
        fn note(&self, n: &NSNotification) {
            let name = n.name().to_string();
            let name = name.trim_start_matches("NSUndoManager").trim_end_matches("Notification");
            self.ivars().events.borrow_mut().push(name.to_string());
        }
    }
);

fn s(t: &str) -> Retained<NSString> {
    NSString::from_str(t)
}

/// Let the run loop finish a turn.
fn turn() {
    NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.01));
}

fn target(mtm: MainThreadMarker) -> Retained<Target> {
    unsafe { msg_send![super(Target::alloc(mtm).set_ivars(Log::default())), init] }
}

fn register(um: &NSUndoManager, t: &Target, v: &str) {
    unsafe { um.registerUndoWithTarget_selector_object(t, sel!(setValue:), Some(&s(v))) };
}

/// What happened since the last call, checkpoints left out (macOS posts
/// them at more points than it documents).
fn events(t: &Target) -> Vec<String> {
    t.ivars().events.take().into_iter().filter(|e| e != "Checkpoint").collect()
}

/// A new manager whose notifications `t` logs.
fn manager(mtm: MainThreadMarker, t: &Target) -> Retained<NSUndoManager> {
    let um = NSUndoManager::new(mtm);
    unsafe { NSNotificationCenter::defaultCenter().addObserver_selector_name_object(t, sel!(note:), None, Some(&um)) };
    um
}

fn defaults(mtm: MainThreadMarker) {
    let um = NSUndoManager::new(mtm);
    assert!(um.groupsByEvent());
    assert_eq!(um.groupingLevel(), 0);
    assert_eq!(um.levelsOfUndo(), 0);
    assert!(!um.canUndo() && !um.canRedo());
    assert!(um.isUndoRegistrationEnabled());
    assert_eq!(um.undoActionName().to_string(), "");
    assert_eq!(um.undoMenuItemTitle().to_string(), "Undo");
    assert_eq!(um.redoMenuItemTitle().to_string(), "Redo");
    assert_eq!(um.undoMenuTitleForUndoActionName(&s("Cut")).to_string(), "Undo Cut");
    assert_eq!(um.redoMenuTitleForUndoActionName(&s("Cut")).to_string(), "Redo Cut");
    assert_eq!(um.runLoopModes().count(), 1);
    unsafe {
        assert_eq!(objc2_foundation::NSUndoManagerCheckpointNotification.to_string(), "NSUndoManagerCheckpointNotification");
        assert_eq!(
            objc2_foundation::NSUndoManagerDidCloseUndoGroupNotification.to_string(),
            "NSUndoManagerDidCloseUndoGroupNotification"
        );
    }
}

/// With grouping by event, registering opens a group that the end of the
/// run loop's turn closes; undo closes it itself.
fn groups_by_event(mtm: MainThreadMarker) {
    let t = target(mtm);
    let um = manager(mtm, &t);
    register(&um, &t, "a");
    assert_eq!(um.groupingLevel(), 1);
    assert!(um.canUndo());
    assert_eq!(events(&t), ["DidOpenUndoGroup"]);
    register(&um, &t, "b");
    assert_eq!(um.groupingLevel(), 1, "one group a turn");
    turn();
    assert_eq!(um.groupingLevel(), 0);
    assert_eq!(events(&t), ["WillCloseUndoGroup", "DidCloseUndoGroup"]);
    assert_eq!(um.undoCount(), 1);
    um.undo();
    assert_eq!(events(&t), ["WillUndoChange", "set b", "set a", "DidUndoChange"]);
    assert!(!um.canUndo());
    // Undo in the same turn closes the group first.
    register(&um, &t, "c");
    um.undo();
    assert_eq!(um.groupingLevel(), 0);
    assert_eq!(events(&t), ["DidOpenUndoGroup", "WillCloseUndoGroup", "DidCloseUndoGroup", "WillUndoChange", "set c", "DidUndoChange"]);
    // An explicit group opens inside the turn's group.
    um.beginUndoGrouping();
    assert_eq!(um.groupingLevel(), 2);
    register(&um, &t, "d");
    um.endUndoGrouping();
    assert_eq!(um.groupingLevel(), 1);
    turn();
    assert_eq!(um.groupingLevel(), 0);
    assert_eq!(um.undoCount(), 1);
    events(&t);
    um.undo();
    assert_eq!(events(&t), ["WillUndoChange", "set d", "DidUndoChange"]);
}

/// Without grouping by event, groups are what the program makes; nested
/// groups undo with their outer group, or alone with undoNestedGroup.
fn explicit_groups(mtm: MainThreadMarker) {
    let t = target(mtm);
    let um = manager(mtm, &t);
    um.setGroupsByEvent(false);
    um.beginUndoGrouping();
    register(&um, &t, "1");
    register(&um, &t, "2");
    um.beginUndoGrouping();
    register(&um, &t, "3");
    um.endUndoGrouping();
    um.endUndoGrouping();
    assert_eq!(um.groupingLevel(), 0);
    assert_eq!(
        events(&t),
        ["DidOpenUndoGroup", "DidOpenUndoGroup", "WillCloseUndoGroup", "DidCloseUndoGroup", "WillCloseUndoGroup", "DidCloseUndoGroup"]
    );
    um.undo();
    assert_eq!(events(&t), ["WillUndoChange", "set 3", "set 2", "set 1", "DidUndoChange"]);
    // An empty group is kept.
    um.beginUndoGrouping();
    um.endUndoGrouping();
    assert!(um.canUndo());
    assert_eq!(um.undoCount(), 1);
    um.removeAllActions();
    assert!(!um.canUndo());
    // undoNestedGroup takes the innermost closed group back.
    um.beginUndoGrouping();
    register(&um, &t, "outer");
    um.beginUndoGrouping();
    register(&um, &t, "inner");
    um.endUndoGrouping();
    events(&t);
    um.undoNestedGroup();
    assert_eq!(events(&t), ["WillUndoChange", "set inner", "DidUndoChange"]);
    assert_eq!(um.groupingLevel(), 1);
    um.endUndoGrouping();
    // removeAllActionsWithTarget: takes the target's actions away.
    unsafe { um.removeAllActionsWithTarget(&t) };
    assert!(!um.canUndo());
}

/// What an undo registers is for redoing; a redo's registrations are for
/// undoing again; a new registration clears the redo stack. Names carry
/// over.
fn undo_and_redo(mtm: MainThreadMarker) {
    let t = target(mtm);
    let um = manager(mtm, &t);
    let seen = t.clone();
    let inner = um.clone();
    let block = RcBlock::new(move |obj: NonNull<AnyObject>| {
        assert!(std::ptr::eq(obj.as_ptr(), Retained::as_ptr(&seen).cast_mut().cast()));
        let state = format!("block undoing={} redoing={} level={}", inner.isUndoing(), inner.isRedoing(), inner.groupingLevel());
        seen.ivars().events.borrow_mut().push(state);
        register(&inner, &seen, "inverse");
    });
    unsafe { um.registerUndoWithTarget_handler(&t, &block) };
    um.setActionName(&s("Paste"));
    assert_eq!(um.undoActionName().to_string(), "Paste");
    assert_eq!(um.undoMenuItemTitle().to_string(), "Undo Paste");
    turn();
    events(&t);
    um.undo();
    assert!(!um.isUndoing());
    assert_eq!(events(&t), ["WillUndoChange", "block undoing=true redoing=false level=1", "DidUndoChange"]);
    assert!(um.canRedo());
    assert_eq!(um.redoActionName().to_string(), "Paste");
    assert_eq!(um.redoMenuItemTitle().to_string(), "Redo Paste");
    um.redo();
    assert_eq!(events(&t), ["WillRedoChange", "set inverse", "DidRedoChange"]);
    assert!(!um.canRedo());
    assert!(um.canUndo());
    assert_eq!(um.undoActionName().to_string(), "Paste");
    // A new action clears what could be redone.
    let um = manager(mtm, &t);
    um.setGroupsByEvent(false);
    let (seen, inner) = (t.clone(), um.clone());
    let block = RcBlock::new(move |_: NonNull<AnyObject>| register(&inner, &seen, "inverse"));
    um.beginUndoGrouping();
    unsafe { um.registerUndoWithTarget_handler(&t, &block) };
    um.endUndoGrouping();
    um.undo();
    assert!(um.canRedo());
    um.beginUndoGrouping();
    register(&um, &t, "fresh");
    assert!(!um.canRedo());
    um.endUndoGrouping();
    assert_eq!((um.undoCount(), um.redoCount()), (1, 0));
}

/// levelsOfUndo keeps the newest groups.
fn levels(mtm: MainThreadMarker) {
    let t = target(mtm);
    let um = manager(mtm, &t);
    um.setGroupsByEvent(false);
    um.setLevelsOfUndo(2);
    for i in 0..4 {
        um.beginUndoGrouping();
        register(&um, &t, &i.to_string());
        um.endUndoGrouping();
    }
    assert_eq!(um.undoCount(), 2);
    events(&t);
    while um.canUndo() {
        um.undo();
    }
    assert_eq!(events(&t), ["WillUndoChange", "set 3", "DidUndoChange", "WillUndoChange", "set 2", "DidUndoChange"]);
}

/// Disabling nests, and registrations meanwhile are dropped.
fn disabling(mtm: MainThreadMarker) {
    let t = target(mtm);
    let um = manager(mtm, &t);
    um.disableUndoRegistration();
    um.disableUndoRegistration();
    register(&um, &t, "x");
    assert!(!um.canUndo());
    assert_eq!(um.groupingLevel(), 0);
    um.enableUndoRegistration();
    assert!(!um.isUndoRegistrationEnabled());
    um.enableUndoRegistration();
    assert!(um.isUndoRegistrationEnabled());
}

/// prepareWithInvocationTarget: records the message sent to what it
/// returns. The message goes through objc_msgSend, since objc2 checks in
/// debug builds that the receiver has the method, and the proxy forwards
/// it.
fn invocations(mtm: MainThreadMarker) {
    let t = target(mtm);
    let um = manager(mtm, &t);
    um.setGroupsByEvent(false);
    um.beginUndoGrouping();
    let proxy = unsafe { um.prepareWithInvocationTarget(&t) };
    let send: unsafe extern "C-unwind" fn() = objc2::ffi::objc_msgSend;
    let set: unsafe extern "C-unwind" fn(&AnyObject, Sel, Option<&AnyObject>) = unsafe { std::mem::transmute(send) };
    unsafe { set(&proxy, sel!(setValue:), Some(&s("recorded"))) };
    um.endUndoGrouping();
    assert!(events(&t).iter().all(|e| !e.starts_with("set")), "recording doesn't send");
    um.undo();
    assert_eq!(events(&t), ["WillUndoChange", "set recorded", "DidUndoChange"]);
}

fn main() {
    let mtm = MainThreadMarker::new().expect("the test's main runs on the main thread");
    defaults(mtm);
    groups_by_event(mtm);
    explicit_groups(mtm);
    undo_and_redo(mtm);
    levels(mtm);
    disabling(mtm);
    invocations(mtm);
    println!("undo_manager: ok");
}
