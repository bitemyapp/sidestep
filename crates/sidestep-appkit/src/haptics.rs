//! `NSHapticFeedbackManager`: its `+defaultPerformer`, one object for the
//! program's life, as on macOS. Linux desktops have no force-feedback
//! trackpads to drive, so performing a pattern does nothing.

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send};

sidestep_runtime::static_class!(pub NSHAPTICFEEDBACKMANAGER, NSHAPTICFEEDBACKMANAGER_META = "NSHapticFeedbackManager", || {
    let _ = NSHapticFeedbackManagerImpl::class();
});

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSHapticFeedbackManager"]
    pub(crate) struct NSHapticFeedbackManagerImpl;

    impl NSHapticFeedbackManagerImpl {
        #[unsafe(method_id(defaultPerformer))]
        fn default_performer() -> Retained<NSObject> {
            thread_local! {
                static PERFORMER: Retained<NSObject> = {
                    let mtm = MainThreadMarker::new().expect("sidestep: haptic feedback belongs to the main thread");
                    // SAFETY: NSObject's initializer.
                    let performer: Retained<PerformerImpl> = unsafe { msg_send![PerformerImpl::alloc(mtm), init] };
                    performer.into_super()
                };
            }
            PERFORMER.with(Clone::clone)
        }
    }
);

define_class!(
    /// The performer: `NSHapticFeedbackPerformer`'s one method, which does
    /// nothing here. Private, so made here only, never looked up by name.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "_SidestepHapticFeedbackPerformer"]
    struct PerformerImpl;

    impl PerformerImpl {
        #[unsafe(method(performFeedbackPattern:performanceTime:))]
        fn perform_feedback_pattern(&self, _pattern: isize, _time: usize) {}
    }

    unsafe impl NSObjectProtocol for PerformerImpl {}
);
