//! Run loops, timers and CoreFoundation's run-loop functions, checked on
//! macOS and on Linux alike.
//!
//! The main thread's loop differs from other threads' (on macOS the main
//! dispatch queue counts as a source of its common modes), so this file has
//! its own `main` and runs some checks there and others on fresh threads.
//! Timing checks never allow a timer to fire early and leave generous room
//! for a slow machine on the late side.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRunLoop, CFRunLoopActivity, CFRunLoopMode, CFRunLoopObserver, CFRunLoopTimer,
    kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSObjectNSDelayedPerforming, NSObjectNSThreadPerformAdditions, NSRunLoop,
    NSRunLoopCommonModes, NSString, NSTimer,
};

use sidestep as _;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RunLoopTestTarget"]
    #[ivars = Cell<u32>]
    struct Target;

    impl Target {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            self.ivars().set(self.ivars().get() + 1);
        }
    }
);

static HITS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn hits() -> Vec<String> {
    std::mem::take(&mut *HITS.lock().unwrap())
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RunLoopTestPerformer"]
    struct Performer;

    impl Performer {
        #[unsafe(method(hit:))]
        fn hit(&self, argument: Option<&AnyObject>) {
            let argument = argument.map(|a| {
                let text: Retained<NSString> = unsafe { msg_send![a, description] };
                text.to_string()
            });
            let mode = NSRunLoop::currentRunLoop().currentMode().map(|m| m.to_string());
            let main = objc2::MainThreadMarker::new().is_some();
            HITS.lock().unwrap().push(format!("{argument:?} main={main} mode={mode:?}"));
        }

        /// Records "first", then asks for `hit:` on the main thread.
        #[unsafe(method(hitThenQueue:))]
        fn hit_then_queue(&self, _argument: Option<&AnyObject>) {
            HITS.lock().unwrap().push("first".into());
            unsafe { self.performSelectorOnMainThread_withObject_waitUntilDone(sel!(hit:), None, false) };
        }
    }
);

/// Counts its drops, and uses the run loop while being dropped, as
/// `-dealloc` methods cancelling their delayed performs do.
struct UsesTheLoopWhenDropped(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for UsesTheLoopWhenDropped {
    fn drop(&mut self) {
        let here = NSRunLoop::currentRunLoop();
        assert!(here.currentMode().is_none());
        unsafe { NSObject::cancelPreviousPerformRequestsWithTarget(&here) };
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RunLoopTestDroppingTarget"]
    #[ivars = UsesTheLoopWhenDropped]
    struct DroppingTarget;

    impl DroppingTarget {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {}
    }
);

fn performer() -> Retained<Performer> {
    unsafe { msg_send![Performer::alloc(), init] }
}

struct SendPerformer(Retained<Performer>);
unsafe impl Send for SendPerformer {}

fn target() -> Retained<Target> {
    let this = Target::alloc().set_ivars(Cell::new(0));
    unsafe { msg_send![super(this), init] }
}

fn wait_until(what: impl Fn() -> bool) {
    let start = Instant::now();
    while !what() {
        assert!(start.elapsed() < Duration::from_secs(5), "timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn retain_count(object: &AnyObject) -> usize {
    unsafe { msg_send![object, retainCount] }
}

fn default_mode() -> &'static NSString {
    unsafe { NSDefaultRunLoopMode }
}

fn common_modes() -> &'static NSString {
    unsafe { NSRunLoopCommonModes }
}

fn cf_default() -> Option<&'static CFRunLoopMode> {
    unsafe { kCFRunLoopDefaultMode }
}

fn cf_common() -> Option<&'static CFRunLoopMode> {
    unsafe { kCFRunLoopCommonModes }
}

/// A mode name as a CoreFoundation string (toll-free on both platforms).
fn cf(mode: &NSString) -> &CFRunLoopMode {
    unsafe { &*(mode as *const NSString).cast() }
}

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn date_in(seconds: f64) -> Retained<NSDate> {
    NSDate::dateWithTimeIntervalSinceNow(seconds)
}

fn ms(since: Instant) -> u128 {
    since.elapsed().as_millis()
}

fn block_timer(interval: f64, repeats: bool, f: impl Fn(&NSTimer) + 'static) -> Retained<NSTimer> {
    let block = RcBlock::new(move |t: NonNull<NSTimer>| f(unsafe { t.as_ref() }));
    unsafe { NSTimer::timerWithTimeInterval_repeats_block(interval, repeats, &block) }
}

fn counter() -> (Rc<Cell<u32>>, impl Fn(&NSTimer) + 'static) {
    let count = Rc::new(Cell::new(0));
    let c = count.clone();
    (count, move |_: &NSTimer| c.set(c.get() + 1))
}

fn schedule(timer: &NSTimer, mode: &NSString) {
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(timer, mode) };
}

/// A timer far in the future, to keep a mode from running out of work.
fn keep_alive(mode: &NSString) -> Retained<NSTimer> {
    let timer = block_timer(1000.0, false, |_| {});
    schedule(&timer, mode);
    timer
}

fn run_mode(mode: &NSString, seconds: f64) -> bool {
    NSRunLoop::currentRunLoop().runMode_beforeDate(mode, &date_in(seconds))
}

/// Run `mode` a slice at a time until `done` holds, for at most 5 s. What
/// is due now can still take a while on a loaded machine (CI's runners fire
/// timers tens of milliseconds late), so waits for something to happen use
/// this; checks that something did *not* happen keep a fixed window.
fn run_until(mode: &NSString, done: impl Fn() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(5), "timed out");
        run_mode(mode, 0.01);
    }
}

fn run_in_mode(mode: Option<&CFRunLoopMode>, seconds: f64, return_after_source: bool) -> i32 {
    CFRunLoop::run_in_mode(mode, seconds, return_after_source).0
}

const FINISHED: i32 = 1;
const STOPPED: i32 = 2;
const TIMED_OUT: i32 = 3;

/// Run `f` on a new thread, which has a fresh run loop.
fn on_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::spawn(f).join().unwrap();
}

fn observer(
    activities: usize,
    repeats: bool,
    order: isize,
    log: &Rc<RefCell<Vec<String>>>,
    tag: &'static str,
) -> objc2_core_foundation::CFRetained<CFRunLoopObserver> {
    let log = log.clone();
    let block = RcBlock::new(move |_: *mut CFRunLoopObserver, activity: CFRunLoopActivity| {
        log.borrow_mut().push(format!("{tag}:{}", activity.0));
    });
    unsafe { CFRunLoopObserver::with_handler(None, activities, repeats, order, Some(&block)) }.unwrap()
}

// ---------------------------------------------------------------------------

fn mode_names() {
    assert_eq!(default_mode().to_string(), "kCFRunLoopDefaultMode");
    assert_eq!(common_modes().to_string(), "kCFRunLoopCommonModes");
    // CoreFoundation's names are the very same objects.
    let kdef: &AnyObject = cf_default().unwrap().as_ref();
    let kcom: &AnyObject = cf_common().unwrap().as_ref();
    assert!(std::ptr::eq(kdef, (default_mode() as &NSString).as_ref()));
    assert!(std::ptr::eq(kcom, (common_modes() as &NSString).as_ref()));
}

fn loop_identity() {
    let main = NSRunLoop::mainRunLoop();
    assert!(std::ptr::eq(&*main, &*NSRunLoop::mainRunLoop()));
    assert!(std::ptr::eq(&*main, &*NSRunLoop::currentRunLoop()));
    assert!(main.currentMode().is_none());
    let cf_main = CFRunLoop::main().unwrap();
    assert!(std::ptr::eq(&*main.getCFRunLoop(), &*cf_main));
    assert!(std::ptr::eq(&*CFRunLoop::current().unwrap(), &*cf_main));
    let main_ptr = &*main as *const NSRunLoop as usize;
    on_thread(move || {
        let here = NSRunLoop::currentRunLoop();
        assert_eq!(&*NSRunLoop::mainRunLoop() as *const NSRunLoop as usize, main_ptr);
        assert_ne!(&*here as *const NSRunLoop as usize, main_ptr);
        assert!(std::ptr::eq(&*here, &*NSRunLoop::currentRunLoop()));
        assert!(std::ptr::eq(&*here.getCFRunLoop(), &*CFRunLoop::current().unwrap()));
        assert!(std::ptr::eq(&*NSRunLoop::mainRunLoop().getCFRunLoop(), &*CFRunLoop::main().unwrap()));
    });
}

fn current_mode_inside_callouts() {
    on_thread(|| {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let timer = block_timer(0.01, false, move |_| {
            log.borrow_mut().push(NSRunLoop::currentRunLoop().currentMode().map(|m| m.to_string()));
        });
        schedule(&timer, &s("ModeA"));
        run_mode(&s("ModeA"), 1.0);
        assert_eq!(*seen.borrow(), [Some("ModeA".to_string())]);
        assert!(NSRunLoop::currentRunLoop().currentMode().is_none());
    });
}

fn empty_modes_return_at_once() {
    on_thread(|| {
        let rl = NSRunLoop::currentRunLoop();
        let start = Instant::now();
        assert!(!rl.runMode_beforeDate(default_mode(), &NSDate::distantFuture()));
        assert_eq!(run_in_mode(cf_default(), 1.0, false), FINISHED);
        rl.runUntilDate(&date_in(1.0));
        assert!(rl.limitDateForMode(default_mode()).is_none());
        assert!(ms(start) < 500, "an empty mode doesn't wait");
    });
}

fn main_loop_is_never_empty_in_common_modes() {
    // The main queue counts as a source of the main loop's common modes.
    let start = Instant::now();
    assert_eq!(run_in_mode(cf_default(), 0.1, false), TIMED_OUT);
    assert!(ms(start) >= 95);
    assert!(run_mode(default_mode(), 0.05));
    assert_eq!(run_in_mode(cf_default(), 0.0, false), TIMED_OUT);
    let limit = NSRunLoop::currentRunLoop().limitDateForMode(default_mode()).unwrap();
    assert!(limit.isEqualToDate(&NSDate::distantFuture()));
    // Other modes are empty.
    let start = Instant::now();
    assert_eq!(run_in_mode(Some(cf(&s("MainCustomMode"))), 0.5, false), FINISHED);
    assert!(!run_mode(&s("MainCustomMode"), 0.5));
    assert!(ms(start) < 250);
}

fn run_mode_returns() {
    // On the main thread a timer doesn't end a run: it goes on to its date.
    let (count, tick) = counter();
    let timer = block_timer(0.05, false, tick);
    schedule(&timer, default_mode());
    let start = Instant::now();
    assert!(run_mode(default_mode(), 0.3));
    assert!(ms(start) >= 290);
    assert_eq!(count.get(), 1);

    on_thread(|| {
        // Elsewhere, a one-shot timer ends the run by emptying the mode.
        let (count, tick) = counter();
        schedule(&block_timer(0.05, false, tick), default_mode());
        let start = Instant::now();
        assert!(run_mode(default_mode(), 1.0));
        let elapsed = ms(start);
        assert!((45..800).contains(&elapsed), "{elapsed} ms");
        assert_eq!(count.get(), 1);

        schedule(&block_timer(0.05, false, |_| {}), default_mode());
        assert_eq!(run_in_mode(cf_default(), 1.0, true), FINISHED);

        // A repeating timer doesn't end it, even when asked to return after
        // a source: timers aren't sources.
        let (count, tick) = counter();
        let timer = block_timer(0.05, true, tick);
        schedule(&timer, default_mode());
        let start = Instant::now();
        assert!(run_mode(default_mode(), 0.3));
        assert!(ms(start) >= 290);
        // A loaded runner drops fires, never adds them: only the upper
        // bound is exact.
        assert!((1..=6).contains(&count.get()), "{} fires", count.get());
        timer.invalidate();

        // Neither does a block.
        let ran = Rc::new(Cell::new(false));
        let r = ran.clone();
        let block = RcBlock::new(move || r.set(true));
        unsafe { NSRunLoop::currentRunLoop().performBlock(&block) };
        let start = Instant::now();
        assert!(run_mode(default_mode(), 0.2));
        assert!(ran.get());
        assert!(ms(start) >= 190);
        let block = RcBlock::new(|| {});
        unsafe { CFRunLoop::current().unwrap().perform_block(Some(cf_default().unwrap().as_ref()), Some(&block)) };
        assert_eq!(run_in_mode(cf_default(), 0.1, true), TIMED_OUT);
    });
}

fn mode_filtering() {
    on_thread(|| {
        let (in_default, tick) = counter();
        let default_timer = block_timer(0.01, true, tick);
        schedule(&default_timer, default_mode());
        let (in_common, tick) = counter();
        let common_timer = block_timer(0.01, true, tick);
        schedule(&common_timer, common_modes());

        // Off the main thread the common set is just the default mode.
        let tracking = s("NSEventTrackingRunLoopMode");
        assert!(!run_mode(&tracking, 0.05));
        assert_eq!((in_default.get(), in_common.get()), (0, 0));

        let keep = keep_alive(&tracking);
        run_mode(&tracking, 0.05);
        assert_eq!((in_default.get(), in_common.get()), (0, 0));

        // A mode added to the common set gets the common timer, not the
        // default one.
        CFRunLoop::current().unwrap().add_common_mode(Some(cf(&tracking)));
        run_until(&tracking, || in_common.get() >= 1);
        assert_eq!(in_default.get(), 0);
        run_until(default_mode(), || in_default.get() >= 1);

        // A custom mode gets only what was added to it.
        let (in_custom, tick) = counter();
        let custom = s("CustomMode");
        let custom_timer = block_timer(0.01, true, tick);
        schedule(&custom_timer, &custom);
        let before = (in_default.get(), in_common.get());
        run_until(&custom, || in_custom.get() >= 1);
        assert_eq!((in_default.get(), in_common.get()), before);
        for t in [default_timer, common_timer, custom_timer, keep] {
            t.invalidate();
        }
    });
}

fn timer_phase_after_a_stall() {
    on_thread(|| {
        let start = Instant::now();
        let fires = Rc::new(RefCell::new(Vec::new()));
        let log = fires.clone();
        let timer = block_timer(0.1, true, move |_| {
            log.borrow_mut().push(start.elapsed().as_millis());
            if log.borrow().len() == 1 {
                std::thread::sleep(Duration::from_millis(350));
            }
        });
        schedule(&timer, default_mode());
        NSRunLoop::currentRunLoop().runUntilDate(&date_in(0.95));
        timer.invalidate();
        let fires = fires.borrow();
        // First at 100 ms; the stall until about 450 ms drops the fires due
        // meanwhile instead of bursting them, and the timer goes on in phase:
        // 100, 500, 600, 700, 800, 900 on an idle machine, where bursting
        // would add three more. A loaded CI runner fires tens of
        // milliseconds late and then on time again (Apple's gave
        // [191, 696, 714] there), and may drop fires, but never adds any, so
        // the count's upper bound is what's pinned.
        // The run may end late on a loaded machine (a CI runner's went on
        // to fire at 1025 ms): count what fired within its 950 ms.
        let within = fires.iter().filter(|&&at| at < 950).count();
        assert!(fires.len() >= 2 && within <= 6, "{fires:?}");
        assert!(fires[0] >= 100, "{fires:?}");
        assert!(fires[1] >= fires[0] + 350, "nothing fires during the stall: {fires:?}");
    });
}

fn timer_details() {
    on_thread(|| {
        assert_eq!(block_timer(5.0, false, |_| {}).timeInterval(), 0.0);
        assert_eq!(block_timer(5.0, true, |_| {}).timeInterval(), 5.0);
        assert_eq!(block_timer(0.0, true, |_| {}).timeInterval(), 0.0001);
        assert_eq!(block_timer(-1.0, true, |_| {}).timeInterval(), 0.0001);

        let timer = block_timer(5.0, false, |_| {});
        assert_eq!(timer.tolerance(), 0.0);
        timer.setTolerance(0.5);
        assert_eq!(timer.tolerance(), 0.5);
        timer.setTolerance(-1.0);
        assert_eq!(timer.tolerance(), 0.0);
        assert!(timer.isValid());
        let ahead = timer.fireDate().timeIntervalSinceNow();
        assert!((4.9..=5.0).contains(&ahead), "{ahead}");

        // -fire on a one-shot timer fires and invalidates it; the fire date
        // then reads as the reference date.
        let (count, tick) = counter();
        let timer = block_timer(5.0, false, tick);
        timer.fire();
        assert_eq!(count.get(), 1);
        assert!(!timer.isValid());
        assert_eq!(timer.fireDate().timeIntervalSinceReferenceDate(), 0.0);
        timer.fire();
        assert_eq!(count.get(), 1, "an invalid timer has no callout");

        // On a repeating timer it leaves the fire date alone.
        let timer = block_timer(5.0, true, |_| {});
        let before = timer.fireDate();
        timer.fire();
        assert!(before.isEqualToDate(&timer.fireDate()));
        assert!(timer.isValid());
        timer.invalidate();

        let initialized = unsafe {
            NSTimer::initWithFireDate_interval_repeats_block(
                NSTimer::alloc(),
                &date_in(100.0),
                0.5,
                false,
                &RcBlock::new(|_| {}),
            )
        };
        assert_eq!(initialized.timeInterval(), 0.0);
        assert!((99.0..=100.0).contains(&initialized.fireDate().timeIntervalSinceNow()));
        let initialized = unsafe {
            NSTimer::initWithFireDate_interval_repeats_block(
                NSTimer::alloc(),
                &date_in(100.0),
                -0.5,
                true,
                &RcBlock::new(|_| {}),
            )
        };
        assert_eq!(initialized.timeInterval(), 0.0001);

        let target = target();
        let info = NSObject::new();
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                1.0,
                &target,
                sel!(tick:),
                Some(&info),
                false,
            )
        };
        assert!(std::ptr::eq(&*timer.userInfo().unwrap(), (*info).as_ref() as &AnyObject));
        assert!(block_timer(1.0, false, |_| {}).userInfo().is_none());
    });
}

fn timer_scheduling() {
    on_thread(|| {
        // Invalidating inside the callout stops the repeats.
        let count = Rc::new(Cell::new(0));
        let c = count.clone();
        let timer = block_timer(0.01, true, move |t| {
            c.set(c.get() + 1);
            t.invalidate();
        });
        schedule(&timer, default_mode());
        run_until(default_mode(), || count.get() >= 1);
        run_mode(default_mode(), 0.05);
        assert_eq!(count.get(), 1);

        // A fire date in the past fires on the next pass.
        let (count, tick) = counter();
        let timer = block_timer(10.0, false, tick);
        schedule(&timer, default_mode());
        timer.setFireDate(&date_in(-5.0));
        let start = Instant::now();
        run_mode(default_mode(), 1.0);
        assert_eq!(count.get(), 1);
        assert!(ms(start) < 500);

        // The distant future pauses a repeating timer; fire dates are
        // clamped to CoreFoundation's latest date.
        let (count, tick) = counter();
        let timer = block_timer(0.01, true, tick);
        schedule(&timer, default_mode());
        timer.setFireDate(&NSDate::distantFuture());
        assert_eq!(timer.fireDate().timeIntervalSinceReferenceDate(), 4_039_289_856.0);
        let start = Instant::now();
        assert!(run_mode(default_mode(), 0.1));
        assert!(ms(start) >= 95);
        assert_eq!(count.get(), 0);
        timer.setFireDate(&date_in(0.0));
        run_until(default_mode(), || count.get() >= 1);
        timer.invalidate();

        // A timer in two modes fires once per fire date.
        let (count, tick) = counter();
        let timer = block_timer(0.05, true, tick);
        schedule(&timer, default_mode());
        schedule(&timer, &s("Other"));
        NSRunLoop::currentRunLoop().runUntilDate(&date_in(0.275));
        // At most one per fire date (4 or 5 here on an idle machine; a
        // loaded runner drops some, never adds them).
        assert!((1..=5).contains(&count.get()), "{} fires", count.get());
        timer.invalidate();

        // A past fire date on a repeating timer fires at once, then every
        // interval from when the timer was made: the old date's phase is
        // not kept.
        let (count, tick) = counter();
        let block = RcBlock::new(move |t: NonNull<NSTimer>| tick(unsafe { t.as_ref() }));
        let made = Instant::now();
        let timer = unsafe {
            NSTimer::initWithFireDate_interval_repeats_block(NSTimer::alloc(), &date_in(-10.25), 1.0, true, &block)
        };
        assert!((-10.3..=-10.2).contains(&timer.fireDate().timeIntervalSinceNow()));
        schedule(&timer, default_mode());
        run_until(default_mode(), || count.get() >= 1);
        assert_eq!(count.get(), 1);
        let next = timer.fireDate().timeIntervalSinceNow() + made.elapsed().as_secs_f64();
        assert!((0.95..1.01).contains(&next), "{next}");
        timer.invalidate();

        // Invalid timers aren't scheduled.
        let (count, tick) = counter();
        let timer = block_timer(0.01, false, tick);
        timer.invalidate();
        schedule(&timer, default_mode());
        assert!(!run_mode(default_mode(), 0.05));
        assert_eq!(count.get(), 0);

        // The loop keeps a scheduled timer alive.
        let (count, tick) = counter();
        schedule(&block_timer(0.01, false, tick), default_mode());
        run_until(default_mode(), || count.get() >= 1);
        assert_eq!(count.get(), 1);
    });
}

fn one_shot_timer_rearmed_in_its_callout() {
    on_thread(|| {
        let count = Rc::new(Cell::new(0));
        let c = count.clone();
        let timer = block_timer(0.01, false, move |t| {
            c.set(c.get() + 1);
            if c.get() == 1 {
                t.setFireDate(&date_in(0.01));
            }
        });
        schedule(&timer, default_mode());
        let keep = keep_alive(default_mode());
        run_until(default_mode(), || count.get() >= 1);
        run_mode(default_mode(), 0.1);
        assert_eq!(count.get(), 1);
        assert!(!timer.isValid());
        keep.invalidate();
    });
}

fn timers_retain_target_and_user_info() {
    on_thread(|| {
        let target = target();
        let info = NSObject::new();
        let (t0, i0) = (retain_count(&target), retain_count(&info));
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                0.01,
                &target,
                sel!(tick:),
                Some(&info),
                true,
            )
        };
        assert_eq!((retain_count(&target), retain_count(&info)), (t0 + 1, i0 + 1));
        let before = retain_count(&timer);
        schedule(&timer, default_mode());
        assert_eq!(retain_count(&timer), before + 1, "the loop retains a scheduled timer");
        run_until(default_mode(), || target.ivars().get() >= 1);
        timer.invalidate();
        assert_eq!((retain_count(&target), retain_count(&info)), (t0, i0));
        assert_eq!(retain_count(&timer), before);

        // A one-shot timer lets go of them once it has fired.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.01,
                &target,
                sel!(tick:),
                Some(&info),
                false,
            )
        };
        assert_eq!(retain_count(&target), t0 + 1);
        run_until(default_mode(), || !timer.isValid());
        assert_eq!((retain_count(&target), retain_count(&info)), (t0, i0));
    });
}

fn perform_block_modes_and_order() {
    on_thread(|| {
        let rl = CFRunLoop::current().unwrap();
        let other = s("PerformOther");
        rl.add_common_mode(Some(cf(&other)));
        let order = Rc::new(RefCell::new(Vec::new()));
        let modes: [(i32, &CFRunLoopMode); 4] =
            [(1, cf_common().unwrap()), (2, cf_default().unwrap()), (3, cf(&other)), (4, cf_common().unwrap())];
        for (i, mode) in modes {
            let o = order.clone();
            let block = RcBlock::new(move || o.borrow_mut().push(i));
            unsafe { rl.perform_block(Some(mode.as_ref()), Some(&block)) };
        }
        let keep = keep_alive(&other);
        run_until(&other, || order.borrow().len() >= 3);
        assert_eq!(*order.borrow(), [1, 3, 4]);
        run_until(default_mode(), || order.borrow().len() >= 4);
        assert_eq!(*order.borrow(), [1, 3, 4, 2]);
        keep.invalidate();

        // -performBlock: runs in the default mode only.
        let ran = Rc::new(Cell::new(false));
        let r = ran.clone();
        let block = RcBlock::new(move || r.set(true));
        unsafe { NSRunLoop::currentRunLoop().performBlock(&block) };
        let keep = keep_alive(&s("Elsewhere"));
        run_mode(&s("Elsewhere"), 0.02);
        assert!(!ran.get());
        run_until(default_mode(), || ran.get());
        keep.invalidate();
    });
}

#[cfg(any(target_vendor = "apple", feature = "collections"))]
fn perform_in_modes() {
    use objc2_foundation::NSArray;
    on_thread(|| {
        let log = Rc::new(RefCell::new(Vec::new()));
        let l = log.clone();
        let block = RcBlock::new(move || {
            l.borrow_mut().push(NSRunLoop::currentRunLoop().currentMode().unwrap().to_string());
        });
        let modes = NSArray::from_retained_slice(&[s("InModesA"), s("InModesB")]);
        unsafe { NSRunLoop::currentRunLoop().performInModes_block(&modes, &block) };
        let keep = keep_alive(&s("InModesB"));
        run_until(&s("InModesB"), || !log.borrow().is_empty());
        assert_eq!(*log.borrow(), ["InModesB"]);
        keep.invalidate();
        // An array of modes works for CFRunLoopPerformBlock too.
        let l = log.clone();
        let block = RcBlock::new(move || l.borrow_mut().push("cf".to_string()));
        let modes = NSArray::from_retained_slice(&[s("InModesC")]);
        let modes: &AnyObject = modes.as_ref();
        let modes: &objc2_core_foundation::CFType = unsafe { &*(modes as *const AnyObject).cast() };
        unsafe { CFRunLoop::current().unwrap().perform_block(Some(modes), Some(&block)) };
        let keep = keep_alive(&s("InModesC"));
        run_until(&s("InModesC"), || log.borrow().len() >= 2);
        assert_eq!(*log.borrow(), ["InModesB", "cf"]);
        keep.invalidate();
    });
}

#[cfg(not(any(target_vendor = "apple", feature = "collections")))]
fn perform_in_modes() {}

fn perform_block_from_another_thread_waits_for_a_wake_up() {
    on_thread(|| {
        struct Loop(*const CFRunLoop);
        unsafe impl Send for Loop {}
        let here = Loop(&*CFRunLoop::current().unwrap());
        let timer = block_timer(0.3, false, |_| {});
        schedule(&timer, default_mode());
        let start = Instant::now();
        let ran_at = Arc::new(Mutex::new(None));
        let at = ran_at.clone();
        let other = std::thread::spawn(move || {
            let here = here;
            std::thread::sleep(Duration::from_millis(50));
            let block = RcBlock::new(move || *at.lock().unwrap() = Some(start.elapsed().as_millis()));
            unsafe { (*here.0).perform_block(Some(cf_default().unwrap().as_ref()), Some(&block)) };
        });
        assert_eq!(run_in_mode(cf_default(), 1.0, true), FINISHED);
        other.join().unwrap();
        // CFRunLoopPerformBlock doesn't wake the loop: the block ran when
        // the timer did.
        let ran_at = ran_at.lock().unwrap().unwrap();
        assert!(ran_at >= 290, "{ran_at} ms");
    });
}

fn run_results() {
    on_thread(|| {
        let keep = keep_alive(default_mode());
        let start = Instant::now();
        assert_eq!(run_in_mode(cf_default(), 0.1, false), TIMED_OUT);
        assert!(ms(start) >= 95);

        schedule(&block_timer(0.02, false, |_| CFRunLoop::current().unwrap().stop()), default_mode());
        let start = Instant::now();
        assert_eq!(run_in_mode(cf_default(), 1.0, false), STOPPED);
        assert!(ms(start) < 500);

        // A stop while the loop isn't running is dropped.
        CFRunLoop::current().unwrap().stop();
        assert_eq!(run_in_mode(cf_default(), 0.05, false), TIMED_OUT);

        let block = RcBlock::new(|| CFRunLoop::current().unwrap().stop());
        unsafe { CFRunLoop::current().unwrap().perform_block(Some(cf_default().unwrap().as_ref()), Some(&block)) };
        let start = Instant::now();
        assert_eq!(run_in_mode(cf_default(), 1.0, false), STOPPED);
        assert!(ms(start) < 500);

        // -runUntilDate: carries on after a stop.
        let count = Rc::new(Cell::new(0));
        let c = count.clone();
        let timer = block_timer(0.02, true, move |t| {
            c.set(c.get() + 1);
            CFRunLoop::current().unwrap().stop();
            if c.get() == 3 {
                t.invalidate();
            }
        });
        schedule(&timer, default_mode());
        keep.invalidate();
        NSRunLoop::currentRunLoop().runUntilDate(&date_in(1.0));
        assert_eq!(count.get(), 3);

        // CFRunLoopRun returns when the mode empties or on a stop.
        schedule(&block_timer(0.02, false, |_| {}), default_mode());
        CFRunLoop::run();
        let keep = keep_alive(default_mode());
        schedule(&block_timer(0.02, false, |_| CFRunLoop::current().unwrap().stop()), default_mode());
        CFRunLoop::run();
        keep.invalidate();
    });
}

fn observer_sequence_and_order() {
    on_thread(|| {
        let rl = CFRunLoop::current().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let all = observer(0x0FFF_FFFF, true, 0, &log, "all");
        rl.add_observer(Some(&all), cf_default());
        let l = log.clone();
        schedule(&block_timer(0.02, false, move |_| l.borrow_mut().push("timer".into())), default_mode());
        assert_eq!(run_in_mode(cf_default(), 1.0, false), FINISHED);
        assert_eq!(*log.borrow(), ["all:1", "all:2", "all:4", "all:32", "all:64", "timer", "all:128"]);
        log.borrow_mut().clear();

        // With nothing but an observer the mode is empty: no callouts.
        assert_eq!(run_in_mode(cf_default(), 0.1, false), FINISHED);
        assert!(log.borrow().is_empty());

        // A poll doesn't sleep, so no before- and after-waiting.
        let keep = keep_alive(default_mode());
        assert_eq!(run_in_mode(cf_default(), 0.0, false), TIMED_OUT);
        assert_eq!(*log.borrow(), ["all:1", "all:2", "all:4", "all:128"]);
        log.borrow_mut().clear();

        let l = log.clone();
        let block = RcBlock::new(move || l.borrow_mut().push("block".into()));
        unsafe { rl.perform_block(Some(cf_default().unwrap().as_ref()), Some(&block)) };
        assert_eq!(run_in_mode(cf_default(), 0.05, false), TIMED_OUT);
        assert_eq!(*log.borrow(), ["all:1", "all:2", "all:4", "block", "all:32", "all:64", "all:128"]);
        rl.remove_observer(Some(&all), cf_default());
        log.borrow_mut().clear();

        // Ascending order, registration order among equals.
        let mut keep_observers = Vec::new();
        for (order, tag) in [(5, "5a"), (-3, "-3"), (5, "5b"), (0, "0"), (isize::MAX, "max"), (5, "5c")] {
            let o = observer(32, true, order, &log, tag);
            rl.add_observer(Some(&o), cf_default());
            keep_observers.push(o);
        }
        run_in_mode(cf_default(), 0.01, true);
        assert_eq!(*log.borrow(), ["-3:32", "0:32", "5a:32", "5b:32", "5c:32", "max:32"]);
        for o in &keep_observers {
            rl.remove_observer(Some(o), cf_default());
        }
        log.borrow_mut().clear();

        // A one-shot observer fires once and is then invalid.
        let once = observer(32, false, 0, &log, "once");
        rl.add_observer(Some(&once), cf_default());
        let repeating = block_timer(0.02, true, |_| {});
        schedule(&repeating, default_mode());
        run_in_mode(cf_default(), 0.1, false);
        assert_eq!(*log.borrow(), ["once:32"]);
        assert!(!once.is_valid());
        assert!(!rl.contains_observer(Some(&once), cf_default()));
        log.borrow_mut().clear();

        // Invalidating inside the callout stops further calls.
        let l = log.clone();
        let block = RcBlock::new(move |o: *mut CFRunLoopObserver, a: CFRunLoopActivity| {
            l.borrow_mut().push(format!("inv:{}", a.0));
            unsafe { (*o).invalidate() };
        });
        let o = unsafe { CFRunLoopObserver::with_handler(None, 32, true, 0, Some(&block)) }.unwrap();
        rl.add_observer(Some(&o), cf_default());
        assert!(rl.contains_observer(Some(&o), cf_default()));
        run_in_mode(cf_default(), 0.1, false);
        assert_eq!(*log.borrow(), ["inv:32"]);
        assert!(!rl.contains_observer(Some(&o), cf_default()));
        repeating.invalidate();
        keep.invalidate();
    });
}

fn nested_runs() {
    on_thread(|| {
        let count = Rc::new(Cell::new(0));
        let nested_fires = Rc::new(Cell::new(None));
        let (c, n) = (count.clone(), nested_fires.clone());
        let timer = block_timer(0.02, true, move |_| {
            c.set(c.get() + 1);
            if n.get().is_none() {
                n.set(Some(0));
                let before = c.get();
                assert_eq!(NSRunLoop::currentRunLoop().currentMode().unwrap().to_string(), "kCFRunLoopDefaultMode");
                assert!(!run_mode(&s("NestedEmpty"), 0.01));
                run_mode(default_mode(), 0.1);
                // A timer doesn't fire inside its own callout.
                n.set(Some(c.get() - before));
                assert_eq!(NSRunLoop::currentRunLoop().currentMode().unwrap().to_string(), "kCFRunLoopDefaultMode");
            }
        });
        schedule(&timer, default_mode());
        let keep = keep_alive(default_mode());
        run_mode(default_mode(), 0.2);
        assert_eq!(nested_fires.get(), Some(0));
        assert!(count.get() >= 1);
        timer.invalidate();

        // A stop inside a nested run ends only that run.
        let log = Rc::new(RefCell::new(Vec::new()));
        let l = log.clone();
        let timer = block_timer(0.01, false, move |_| {
            let l2 = l.clone();
            let inner = block_timer(0.01, false, move |_| {
                l2.borrow_mut().push("stop");
                CFRunLoop::current().unwrap().stop();
            });
            schedule(&inner, default_mode());
            let result = run_in_mode(cf_default(), 1.0, false);
            l.borrow_mut().push(if result == STOPPED { "inner stopped" } else { "inner not stopped" });
        });
        schedule(&timer, default_mode());
        let start = Instant::now();
        assert_eq!(run_in_mode(cf_default(), 0.3, false), TIMED_OUT);
        assert!(ms(start) >= 290);
        assert_eq!(*log.borrow(), ["stop", "inner stopped"]);

        // Entry and exit observers see nested runs too.
        let rl = CFRunLoop::current().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let entry_exit = observer(1 | 128, true, 0, &log, "o");
        rl.add_observer(Some(&entry_exit), cf_common());
        schedule(
            &block_timer(0.01, false, |_| {
                run_in_mode(cf_default(), 0.01, false);
            }),
            default_mode(),
        );
        run_in_mode(cf_default(), 0.1, false);
        assert_eq!(*log.borrow(), ["o:1", "o:1", "o:128", "o:128"]);
        rl.remove_observer(Some(&entry_exit), cf_common());
        keep.invalidate();
    });
}

fn common_mode_registrations() {
    on_thread(|| {
        let rl = CFRunLoop::current().unwrap();
        let timer = block_timer(1.0, false, |_| {});
        let t: &CFRunLoopTimer = (*timer).as_ref();
        let x = s("CommonX");
        rl.add_timer(Some(t), cf_common());
        assert!(rl.contains_timer(Some(t), cf_common()));
        assert!(rl.contains_timer(Some(t), cf_default()));
        assert!(!rl.contains_timer(Some(t), Some(cf(&x))));
        rl.add_common_mode(Some(cf(&x)));
        assert!(rl.contains_timer(Some(t), Some(cf(&x))));
        // Removing it from one common mode leaves it in the others.
        rl.remove_timer(Some(t), cf_default());
        assert!(!rl.contains_timer(Some(t), cf_default()));
        assert!(rl.contains_timer(Some(t), cf_common()));
        assert!(rl.contains_timer(Some(t), Some(cf(&x))));
        // Removing it from the common modes takes it out of all of them.
        rl.remove_timer(Some(t), cf_common());
        assert!(!rl.contains_timer(Some(t), cf_common()));
        assert!(!rl.contains_timer(Some(t), Some(cf(&x))));
        assert!(timer.isValid(), "removing doesn't invalidate");
        timer.invalidate();
    });
}

fn limit_date_and_next_fire_date() {
    on_thread(|| {
        let rl = NSRunLoop::currentRunLoop();
        let cf_rl = CFRunLoop::current().unwrap();
        let keep = keep_alive(default_mode());
        let (count, tick) = counter();
        schedule(&block_timer(0.01, false, tick), default_mode());
        // Well past the fire date: CI's runners have made Apple's loop treat
        // a timer 10 ms overdue as not due yet.
        std::thread::sleep(Duration::from_millis(100));
        // One pass, firing what is due, then the next timer's date.
        let limit = rl.limitDateForMode(default_mode()).unwrap();
        assert_eq!(count.get(), 1);
        assert!(limit.isEqualToDate(&keep.fireDate()));

        let soon = block_timer(3.0, false, |_| {});
        schedule(&soon, default_mode());
        let now = CFAbsoluteTimeGetCurrent();
        let next = cf_rl.next_timer_fire_date(cf_default()) - now;
        assert!((2.9..=3.0).contains(&next), "{next}");
        assert_eq!(cf_rl.next_timer_fire_date(Some(cf(&s("NoTimers")))), 0.0);
        soon.invalidate();
        keep.invalidate();
    });
}

fn cf_timers() {
    on_thread(|| {
        let rl = CFRunLoop::current().unwrap();
        let count = Rc::new(Cell::new(0));
        let c = count.clone();
        let block = RcBlock::new(move |_: *mut CFRunLoopTimer| c.set(c.get() + 1));
        let now = CFAbsoluteTimeGetCurrent();
        let timer = unsafe { CFRunLoopTimer::with_handler(None, now + 0.02, 0.0, 0, 0, Some(&block)) }.unwrap();
        assert_eq!(timer.interval(), 0.0);
        assert!(!timer.does_repeat());
        assert!((timer.next_fire_date() - now - 0.02).abs() < 1e-6);
        rl.add_timer(Some(&timer), cf_default());
        assert_eq!(run_in_mode(cf_default(), 1.0, false), FINISHED);
        assert_eq!(count.get(), 1);
        assert!(!timer.is_valid());
        assert_eq!(timer.next_fire_date(), 0.0);

        // Non-positive intervals don't repeat; far dates are clamped, past
        // ones aren't.
        let timer = unsafe { CFRunLoopTimer::with_handler(None, now - 5.0, -1.0, 0, 7, Some(&block)) }.unwrap();
        assert_eq!((timer.interval(), timer.does_repeat(), timer.order()), (0.0, false, 7));
        let timer = unsafe { CFRunLoopTimer::with_handler(None, 1e20, 1.0, 0, 0, Some(&block)) }.unwrap();
        assert_eq!(timer.next_fire_date(), 4_039_289_856.0);
        assert!(timer.does_repeat());
        timer.set_next_fire_date(-1e20);
        assert_eq!(timer.next_fire_date(), -1e20);
        // The same object is an NSTimer.
        let ns: &NSTimer = (*timer).as_ref();
        assert_eq!(ns.timeInterval(), 1.0);
        timer.invalidate();
    });
}

/// A CoreFoundation timer's context outlives the timer's callout, even when
/// the callout invalidates the timer, which releases the timer's own
/// reference to it.
fn cf_timer_context_outlives_its_callout() {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicIsize, Ordering::SeqCst};
    static REFS: AtomicIsize = AtomicIsize::new(0);
    static IN_CALLOUT: AtomicIsize = AtomicIsize::new(-1);
    unsafe extern "C-unwind" fn retain(info: *const c_void) -> *const c_void {
        REFS.fetch_add(1, SeqCst);
        info
    }
    unsafe extern "C-unwind" fn release(_info: *const c_void) {
        REFS.fetch_sub(1, SeqCst);
    }
    unsafe extern "C-unwind" fn callout(timer: *mut CFRunLoopTimer, _info: *mut c_void) {
        unsafe { &*timer }.invalidate();
        IN_CALLOUT.store(REFS.load(SeqCst), SeqCst);
    }
    on_thread(|| {
        let mut context = objc2_core_foundation::CFRunLoopTimerContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: Some(retain),
            release: Some(release),
            copyDescription: None,
        };
        let now = CFAbsoluteTimeGetCurrent();
        let timer = unsafe { CFRunLoopTimer::new(None, now, 1.0, 0, 0, Some(callout), &mut context) }.unwrap();
        assert_eq!(REFS.load(SeqCst), 1, "the timer keeps a reference");
        CFRunLoop::current().unwrap().add_timer(Some(&timer), cf_default());
        assert_eq!(run_in_mode(cf_default(), 1.0, false), FINISHED);
        assert!(IN_CALLOUT.load(SeqCst) >= 1, "the context was released while its callout ran");
        assert_eq!(REFS.load(SeqCst), 0, "every reference is given back");
    });
}

/// The timer constructors and run methods not used elsewhere here: a
/// scheduled block timer, `-initWithFireDate:interval:target:…`,
/// `-acceptInputForMode:beforeDate:` and `-run`.
fn timer_and_run_entry_points() {
    on_thread(|| {
        let (count, tick) = counter();
        let block = RcBlock::new(move |t: NonNull<NSTimer>| tick(unsafe { t.as_ref() }));
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.01, true, &block) };
        assert!(timer.isValid());
        assert!(run_mode(default_mode(), 0.035));
        assert!(count.get() >= 1, "scheduled in the default mode");
        timer.invalidate();

        // A timer made for a date: it fires then, not before, and holds its
        // target and user info until it is invalidated.
        let target = target();
        let info = NSObject::new();
        let (t0, i0) = (retain_count(&target), retain_count(&info));
        let start = Instant::now();
        let timer = unsafe {
            NSTimer::initWithFireDate_interval_target_selector_userInfo_repeats(
                NSTimer::alloc(),
                &date_in(0.02),
                0.01,
                &target,
                sel!(tick:),
                Some(&info),
                true,
            )
        };
        assert_eq!((retain_count(&target), retain_count(&info)), (t0 + 1, i0 + 1));
        let info_object: &AnyObject = &info;
        // In a pool: Sidestep's runtime doesn't elide returned autoreleases.
        objc2::rc::autoreleasepool(|_| {
            assert!(timer.userInfo().is_some_and(|u| std::ptr::eq(&*u, info_object)));
        });
        schedule(&timer, default_mode());
        while target.ivars().get() == 0 {
            assert!(start.elapsed() < Duration::from_secs(5));
            run_mode(default_mode(), 0.1);
        }
        assert!(ms(start) >= 19, "fired early, at {} ms", ms(start));
        timer.invalidate();
        assert_eq!((retain_count(&target), retain_count(&info)), (t0, i0));

        // acceptInputForMode:beforeDate: fires a due timer and returns by
        // the date.
        let (count, tick) = counter();
        let timer = block_timer(0.01, false, tick);
        schedule(&timer, default_mode());
        let keep = keep_alive(default_mode());
        let start = Instant::now();
        NSRunLoop::currentRunLoop().acceptInputForMode_beforeDate(default_mode(), &date_in(0.1));
        assert_eq!(count.get(), 1);
        assert!(ms(start) >= 9 && start.elapsed() < Duration::from_secs(2));
        keep.invalidate();
    });

    // -run returns once nothing is left in the default mode: here, after
    // the only timer, a one-shot, has fired.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (count, tick) = counter();
        let timer = block_timer(0.02, false, tick);
        schedule(&timer, default_mode());
        let start = Instant::now();
        NSRunLoop::currentRunLoop().run();
        tx.send((count.get(), ms(start))).unwrap();
    });
    let (fired, took) = rx.recv_timeout(Duration::from_secs(10)).expect("-run returned");
    assert_eq!(fired, 1);
    assert!(took >= 19, "{took} ms");
}

fn other_threads_see_and_stop_a_loop() {
    on_thread(|| {
        struct Loop(*const CFRunLoop);
        unsafe impl Send for Loop {}
        let rl = CFRunLoop::current().unwrap();
        assert!(!rl.is_waiting());
        let here = Loop(&*rl);
        let seen = Arc::new(Mutex::new(None));
        let s2 = seen.clone();
        let other = std::thread::spawn(move || {
            let here = here;
            let rl = unsafe { &*here.0 };
            // Until the loop sleeps (a loaded machine may take a while to
            // get it there).
            let start = Instant::now();
            while !rl.is_waiting() && start.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(5));
            }
            let mode =
                rl.current_mode().map(|m| unsafe { &*(&*m as *const CFRunLoopMode).cast::<NSString>() }.to_string());
            *s2.lock().unwrap() = Some((rl.is_waiting(), mode));
            rl.stop();
        });
        let keep = keep_alive(&s("Sleepy"));
        assert_eq!(run_in_mode(Some(cf(&s("Sleepy"))), 2.0, false), STOPPED);
        other.join().unwrap();
        assert_eq!(*seen.lock().unwrap(), Some((true, Some("Sleepy".to_string()))));
        keep.invalidate();
    });
}

fn timer_added_to_the_main_loop_from_another_thread() {
    let fired_on_main = Arc::new(Mutex::new(None));
    let f = fired_on_main.clone();
    on_thread(move || {
        let block = RcBlock::new(move |_: *mut CFRunLoopTimer| {
            *f.lock().unwrap() = Some(objc2::MainThreadMarker::new().is_some());
        });
        let timer =
            unsafe { CFRunLoopTimer::with_handler(None, CFAbsoluteTimeGetCurrent() + 0.02, 0.0, 0, 0, Some(&block)) }
                .unwrap();
        CFRunLoop::main().unwrap().add_timer(Some(&timer), cf_default());
    });
    run_until(default_mode(), || fired_on_main.lock().unwrap().is_some());
    assert_eq!(*fired_on_main.lock().unwrap(), Some(true));
}

fn delayed_performs() {
    let target = performer();
    let argument = s("an argument long enough not to be tagged");
    let counts = (retain_count(&target), retain_count(&argument));
    unsafe { target.performSelector_withObject_afterDelay(sel!(hit:), Some(&argument), 0.0) };
    assert!(hits().is_empty(), "never synchronous");
    assert_eq!((retain_count(&target), retain_count(&argument)), (counts.0 + 1, counts.1 + 1));
    // Only in the default mode.
    assert!(!run_mode(&s("NSEventTrackingRunLoopMode"), 0.05));
    assert!(hits().is_empty());
    run_until(default_mode(), || !HITS.lock().unwrap().is_empty());
    assert_eq!(
        hits(),
        ["Some(\"an argument long enough not to be tagged\") main=true mode=Some(\"kCFRunLoopDefaultMode\")"]
    );
    assert_eq!((retain_count(&target), retain_count(&argument)), counts);

    // Cancelling matches the argument with -isEqual:; nil only matches nil.
    let (a, equal_a) = (s("an equal argument, long enough"), s("an equal argument, long enough"));
    unsafe { target.performSelector_withObject_afterDelay(sel!(hit:), Some(&a), 0.0) };
    unsafe { NSObject::cancelPreviousPerformRequestsWithTarget_selector_object(&target, sel!(hit:), Some(&equal_a)) };
    assert_eq!(retain_count(&target), counts.0, "cancelling releases the target");
    unsafe { target.performSelector_withObject_afterDelay(sel!(hit:), None, 0.0) };
    unsafe { NSObject::cancelPreviousPerformRequestsWithTarget_selector_object(&target, sel!(hit:), Some(&a)) };
    run_until(default_mode(), || !HITS.lock().unwrap().is_empty());
    assert_eq!(hits(), ["None main=true mode=Some(\"kCFRunLoopDefaultMode\")"]);
    unsafe { target.performSelector_withObject_afterDelay(sel!(hit:), Some(&a), 0.0) };
    unsafe { NSObject::cancelPreviousPerformRequestsWithTarget_selector_object(&target, sel!(hit:), None) };
    run_until(default_mode(), || !HITS.lock().unwrap().is_empty());
    run_mode(default_mode(), 0.05);
    assert_eq!(hits().len(), 1);
    unsafe { target.performSelector_withObject_afterDelay(sel!(hit:), Some(&a), 0.0) };
    unsafe { target.performSelector_withObject_afterDelay(sel!(hit:), None, 0.0) };
    unsafe { NSObject::cancelPreviousPerformRequestsWithTarget(&target) };
    run_mode(default_mode(), 0.05);
    assert!(hits().is_empty());
}

#[cfg(any(target_vendor = "apple", feature = "collections"))]
fn delayed_performs_in_modes() {
    use objc2_foundation::NSArray;
    let target = performer();
    let modes = NSArray::from_retained_slice(&[s("DelayedMode")]);
    unsafe { target.performSelector_withObject_afterDelay_inModes(sel!(hit:), None, 0.0, &modes) };
    run_mode(default_mode(), 0.05);
    assert!(hits().is_empty());
    run_until(&s("DelayedMode"), || !HITS.lock().unwrap().is_empty());
    assert_eq!(hits(), ["None main=true mode=Some(\"DelayedMode\")"]);

    let modes = NSArray::from_retained_slice(&[s("OnlyThisMode")]);
    unsafe { target.performSelectorOnMainThread_withObject_waitUntilDone_modes(sel!(hit:), None, false, Some(&modes)) };
    run_mode(default_mode(), 0.05);
    assert!(hits().is_empty());
    let keep = keep_alive(&s("OnlyThisMode"));
    run_until(&s("OnlyThisMode"), || !HITS.lock().unwrap().is_empty());
    assert_eq!(hits(), ["None main=true mode=Some(\"OnlyThisMode\")"]);
    keep.invalidate();
}

#[cfg(not(any(target_vendor = "apple", feature = "collections")))]
fn delayed_performs_in_modes() {}

fn performs_on_the_main_thread() {
    let target = performer();
    // Waiting on the main thread sends it at once, outside any run.
    unsafe { target.performSelectorOnMainThread_withObject_waitUntilDone(sel!(hit:), None, true) };
    assert_eq!(hits(), ["None main=true mode=None"]);

    // Not waiting: a later pass sends it, and that ends the run.
    let argument = s("an argument long enough not to be tagged");
    let counts = (retain_count(&target), retain_count(&argument));
    unsafe { target.performSelectorOnMainThread_withObject_waitUntilDone(sel!(hit:), Some(&argument), false) };
    assert!(hits().is_empty());
    assert_eq!((retain_count(&target), retain_count(&argument)), (counts.0 + 1, counts.1 + 1));
    let start = Instant::now();
    assert!(run_mode(default_mode(), 1.0));
    assert!(ms(start) < 500);
    assert_eq!(hits().len(), 1);
    assert_eq!((retain_count(&target), retain_count(&argument)), counts);

    // From another thread, waiting: sent while the main thread runs a mode
    // of the common set, and the caller waits for it.
    let modal = s("PerformModalMode");
    CFRunLoop::main().unwrap().add_common_mode(Some(cf(&modal)));
    let sender = SendPerformer(target.clone());
    let other = std::thread::spawn(move || {
        let sender = sender;
        unsafe { sender.0.performSelectorOnMainThread_withObject_waitUntilDone(sel!(hit:), None, true) };
        HITS.lock().unwrap().push("returned".into());
    });
    std::thread::sleep(Duration::from_millis(50));
    let keep = keep_alive(&modal);
    let start = Instant::now();
    while !other.is_finished() && start.elapsed() < Duration::from_secs(5) {
        run_mode(&modal, 0.05);
    }
    other.join().unwrap();
    assert_eq!(hits(), ["None main=true mode=Some(\"PerformModalMode\")", "returned"]);
    keep.invalidate();
}

fn performs_on_other_threads() {
    let target = performer();
    let thread = unsafe {
        objc2_foundation::NSThread::initWithBlock(
            objc2_foundation::NSThread::alloc(),
            &RcBlock::new(|| {
                let keep = keep_alive(default_mode());
                NSRunLoop::currentRunLoop().runUntilDate(&date_in(0.5));
                keep.invalidate();
            }),
        )
    };
    thread.start();
    std::thread::sleep(Duration::from_millis(50));
    unsafe { target.performSelector_onThread_withObject_waitUntilDone(sel!(hit:), &thread, None, true) };
    assert_eq!(hits(), ["None main=false mode=Some(\"kCFRunLoopDefaultMode\")"]);

    unsafe { target.performSelectorInBackground_withObject(sel!(hit:), None) };
    let start = Instant::now();
    while HITS.lock().unwrap().is_empty() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(hits(), ["None main=false mode=None"]);
}

/// A worker's `NSRunLoop`, handed to another thread, answers
/// `-getCFRunLoop` there, and the result stops the worker's run.
fn another_threads_loop_objects() {
    struct SendLoop(Retained<NSRunLoop>);
    unsafe impl Send for SendLoop {}
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let keep = keep_alive(&s("WorkerMode"));
        tx.send(SendLoop(NSRunLoop::currentRunLoop())).unwrap();
        let result = run_in_mode(Some(cf(&s("WorkerMode"))), 5.0, false);
        keep.invalidate();
        result
    });
    let worker_loop = rx.recv().unwrap();
    let cf_loop = worker_loop.0.getCFRunLoop();
    wait_until(|| cf_loop.is_waiting());
    cf_loop.wake_up();
    cf_loop.stop();
    assert_eq!(worker.join().unwrap(), STOPPED);
}

/// A request for a thread made right after `-start` waits for the thread's
/// loop, waiting for it or not.
fn performs_right_after_start() {
    for wait in [false, true] {
        let target = performer();
        let block = RcBlock::new(|| {
            let keep = keep_alive(default_mode());
            NSRunLoop::currentRunLoop().runUntilDate(&date_in(0.3));
            keep.invalidate();
        });
        let thread = unsafe { objc2_foundation::NSThread::initWithBlock(objc2_foundation::NSThread::alloc(), &block) };
        thread.start();
        unsafe { target.performSelector_onThread_withObject_waitUntilDone(sel!(hit:), &thread, None, wait) };
        wait_until(|| !HITS.lock().unwrap().is_empty());
        assert_eq!(hits(), ["None main=false mode=Some(\"kCFRunLoopDefaultMode\")"]);
        wait_until(|| thread.isFinished());
    }
}

/// Requests waiting for a loop all run in one pass of its perform source;
/// one made while they run waits for the next.
fn performs_in_bursts() {
    let target = performer();
    for i in 0..10 {
        let argument = s(&format!("request {i}"));
        unsafe { target.performSelectorOnMainThread_withObject_waitUntilDone(sel!(hit:), Some(&argument), false) };
    }
    unsafe { target.performSelectorOnMainThread_withObject_waitUntilDone(sel!(hitThenQueue:), None, false) };
    assert!(run_mode(default_mode(), 1.0));
    let first = hits();
    assert_eq!(first.len(), 11);
    assert_eq!(first[0], "Some(\"request 0\") main=true mode=Some(\"kCFRunLoopDefaultMode\")");
    assert_eq!(first[9], "Some(\"request 9\") main=true mode=Some(\"kCFRunLoopDefaultMode\")");
    assert_eq!(first[10], "first");
    assert!(run_mode(default_mode(), 1.0));
    assert_eq!(hits(), ["None main=true mode=Some(\"kCFRunLoopDefaultMode\")"]);

    // From another thread, too.
    let sender = SendPerformer(target.clone());
    on_thread(move || {
        let sender = sender;
        for _ in 0..5 {
            unsafe { sender.0.performSelectorOnMainThread_withObject_waitUntilDone(sel!(hit:), None, false) };
        }
    });
    assert!(run_mode(default_mode(), 1.0));
    assert_eq!(hits().len(), 5);
}

/// A request for a thread that ends before running its loop is dropped,
/// with its target and argument. A caller waiting for one isn't left
/// hanging: macOS raises, Sidestep panics, with the same reason (checked
/// in a child process, which that ends).
fn performs_for_threads_that_end() {
    let target = performer();
    let argument = s("an argument long enough not to be tagged");
    let counts = (retain_count(&target), retain_count(&argument));
    let block = RcBlock::new(|| std::thread::sleep(Duration::from_millis(100)));
    let thread = unsafe { objc2_foundation::NSThread::initWithBlock(objc2_foundation::NSThread::alloc(), &block) };
    thread.start();
    unsafe { target.performSelector_onThread_withObject_waitUntilDone(sel!(hit:), &thread, Some(&argument), false) };
    wait_until(|| thread.isFinished());
    wait_until(|| (retain_count(&target), retain_count(&argument)) == counts);
    assert!(hits().is_empty());

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .env("SIDESTEP_RUNLOOP_CHILD", "wait-for-exiting-thread")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait_with_output().unwrap()).unwrap());
    let output = rx.recv_timeout(Duration::from_secs(20)).expect("the waiting caller hung");
    assert!(!output.status.success());
    // On macOS the exception meets Rust frames, which abort without its
    // reason (NSDestinationInvalidException, with the text below).
    if cfg!(not(target_vendor = "apple")) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = "-[RunLoopTestPerformer performSelector:onThread:withObject:waitUntilDone:modes:]: target thread \
                      exited while waiting for the perform";
        assert!(stderr.contains(reason), "{stderr}");
    }
}

/// The child process of `performs_for_threads_that_end`.
fn wait_for_exiting_thread() {
    let target = performer();
    let block = RcBlock::new(|| std::thread::sleep(Duration::from_millis(200)));
    let thread = unsafe { objc2_foundation::NSThread::initWithBlock(objc2_foundation::NSThread::alloc(), &block) };
    thread.start();
    std::thread::sleep(Duration::from_millis(50));
    unsafe { target.performSelector_onThread_withObject_waitUntilDone(sel!(hit:), &thread, None, true) };
    println!("returned");
}

/// A thread's loop ends with the thread, releasing its timers, and what
/// that releases may use the run loop again.
fn loops_end_with_their_threads() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    fn schedule_and_leave(dropped: Arc<AtomicUsize>) {
        let this = DroppingTarget::alloc().set_ivars(UsesTheLoopWhenDropped(dropped));
        let target: Retained<DroppingTarget> = unsafe { msg_send![super(this), init] };
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                100.0,
                &target,
                sel!(tick:),
                None,
                false,
            )
        };
        drop((timer, target));
    }
    let dropped = Arc::new(AtomicUsize::new(0));
    let d = dropped.clone();
    std::thread::spawn(move || schedule_and_leave(d)).join().unwrap();
    wait_until(|| dropped.load(SeqCst) == 1);

    let d = dropped.clone();
    let block = RcBlock::new(move || schedule_and_leave(d.clone()));
    let thread = unsafe { objc2_foundation::NSThread::initWithBlock(objc2_foundation::NSThread::alloc(), &block) };
    thread.start();
    wait_until(|| thread.isFinished() && dropped.load(SeqCst) == 2);
}

type Test = (&'static str, fn());

fn main() {
    let tests: &[Test] = &[
        ("mode_names", mode_names),
        ("loop_identity", loop_identity),
        ("current_mode_inside_callouts", current_mode_inside_callouts),
        ("empty_modes_return_at_once", empty_modes_return_at_once),
        ("main_loop_is_never_empty_in_common_modes", main_loop_is_never_empty_in_common_modes),
        ("run_mode_returns", run_mode_returns),
        ("mode_filtering", mode_filtering),
        ("timer_phase_after_a_stall", timer_phase_after_a_stall),
        ("timer_details", timer_details),
        ("timer_scheduling", timer_scheduling),
        ("one_shot_timer_rearmed_in_its_callout", one_shot_timer_rearmed_in_its_callout),
        ("timers_retain_target_and_user_info", timers_retain_target_and_user_info),
        ("perform_block_modes_and_order", perform_block_modes_and_order),
        ("perform_in_modes", perform_in_modes),
        (
            "perform_block_from_another_thread_waits_for_a_wake_up",
            perform_block_from_another_thread_waits_for_a_wake_up,
        ),
        ("run_results", run_results),
        ("observer_sequence_and_order", observer_sequence_and_order),
        ("nested_runs", nested_runs),
        ("common_mode_registrations", common_mode_registrations),
        ("limit_date_and_next_fire_date", limit_date_and_next_fire_date),
        ("cf_timers", cf_timers),
        ("cf_timer_context_outlives_its_callout", cf_timer_context_outlives_its_callout),
        ("timer_and_run_entry_points", timer_and_run_entry_points),
        ("other_threads_see_and_stop_a_loop", other_threads_see_and_stop_a_loop),
        ("timer_added_to_the_main_loop_from_another_thread", timer_added_to_the_main_loop_from_another_thread),
        ("delayed_performs", delayed_performs),
        ("delayed_performs_in_modes", delayed_performs_in_modes),
        ("performs_on_the_main_thread", performs_on_the_main_thread),
        ("performs_on_other_threads", performs_on_other_threads),
        ("another_threads_loop_objects", another_threads_loop_objects),
        ("performs_right_after_start", performs_right_after_start),
        ("performs_in_bursts", performs_in_bursts),
        ("performs_for_threads_that_end", performs_for_threads_that_end),
        ("loops_end_with_their_threads", loops_end_with_their_threads),
    ];
    if std::env::var("SIDESTEP_RUNLOOP_CHILD").is_ok_and(|c| c == "wait-for-exiting-thread") {
        wait_for_exiting_thread();
        return;
    }
    assert!(objc2::MainThreadMarker::new().is_some(), "runs on the main thread");
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test());
        println!("test {name} ... ok");
    }
}
