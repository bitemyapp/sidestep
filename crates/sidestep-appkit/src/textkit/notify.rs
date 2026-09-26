//! What text views tell the world: the `NSText` and `NSTextView`
//! notifications, each posted on the default center and handed to the
//! delegate's matching method (as AppKit's delegate hears them), and the
//! delegate's questions.
//!
//! Notifications are posted without making an object when nobody
//! observes (`notification_center::post`); the delegate gets one of its
//! own only when it has the method.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, MessageReceiver, Sel};
use objc2::{msg_send, sel};
use objc2_foundation::{NSDictionary, NSNotification, NSString};

sidestep_foundation::constant_string!(NSTextDidBeginEditingNotification = "NSTextDidBeginEditingNotification");
sidestep_foundation::constant_string!(NSTextDidEndEditingNotification = "NSTextDidEndEditingNotification");
sidestep_foundation::constant_string!(NSTextDidChangeNotification = "NSTextDidChangeNotification");
sidestep_foundation::constant_string!(NSTextMovementUserInfoKey = "NSTextMovement");
sidestep_foundation::constant_string!(
    NSTextViewDidChangeSelectionNotification = "NSTextViewDidChangeSelectionNotification"
);
sidestep_foundation::constant_string!(
    NSTextViewDidChangeTypingAttributesNotification = "NSTextViewDidChangeTypingAttributesNotification"
);
sidestep_foundation::constant_string!(
    NSTextViewWillChangeNotifyingTextViewNotification = "NSTextViewWillChangeNotifyingTextViewNotification"
);
sidestep_foundation::constant_string!(
    NSTextViewWillSwitchToNSLayoutManagerNotification = "NSTextViewWillSwitchToNSLayoutManagerNotification"
);
sidestep_foundation::constant_string!(
    NSTextViewDidSwitchToNSLayoutManagerNotification = "NSTextViewDidSwitchToNSLayoutManagerNotification"
);

/// The notifications a text view posts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Note {
    BeginEditing,
    EndEditing,
    Change,
    ChangeSelection,
    ChangeTypingAttributes,
}

impl Note {
    fn name(self) -> &'static NSString {
        // SAFETY: the names are constant strings this module exports.
        unsafe {
            match self {
                Note::BeginEditing => objc2_app_kit::NSTextDidBeginEditingNotification,
                Note::EndEditing => objc2_app_kit::NSTextDidEndEditingNotification,
                Note::Change => objc2_app_kit::NSTextDidChangeNotification,
                Note::ChangeSelection => objc2_app_kit::NSTextViewDidChangeSelectionNotification,
                Note::ChangeTypingAttributes => objc2_app_kit::NSTextViewDidChangeTypingAttributesNotification,
            }
        }
    }

    /// The delegate method that hears it.
    fn delegate_selector(self) -> Sel {
        match self {
            Note::BeginEditing => sel!(textDidBeginEditing:),
            Note::EndEditing => sel!(textDidEndEditing:),
            Note::Change => sel!(textDidChange:),
            Note::ChangeSelection => sel!(textViewDidChangeSelection:),
            Note::ChangeTypingAttributes => sel!(textViewDidChangeTypingAttributes:),
        }
    }
}

pub(crate) fn responds(obj: &AnyObject, sel: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector.
    unsafe { msg_send![obj, respondsToSelector: sel] }
}

/// Post `note` from `view`, with `info`, and tell `delegate`.
pub(crate) fn post(
    view: &AnyObject,
    delegate: Option<&AnyObject>,
    note: Note,
    info: Option<&NSDictionary<NSString, AnyObject>>,
) {
    let info_object: Option<&AnyObject> = info.map(|d| d.as_ref());
    sidestep_foundation::notification_center::post(note.name(), Some(view), info_object);
    let Some(delegate) = delegate else { return };
    let sel = note.delegate_selector();
    if responds(delegate, sel) {
        // SAFETY: an attribute-free dictionary of strings to objects is an
        // NSDictionary of objects.
        let info = info.map(|d| unsafe { &*(d as *const NSDictionary<NSString, AnyObject>).cast::<NSDictionary>() });
        // SAFETY: a name, an object and a dictionary, as it takes.
        let n: Retained<NSNotification> =
            unsafe { NSNotification::notificationWithName_object_userInfo(note.name(), Some(view), info) };
        // SAFETY: the delegate methods take the notification and return
        // nothing.
        unsafe { MessageReceiver::send_message::<_, ()>(delegate, sel, (&*n,)) };
    }
}

/// Ask `delegate` a yes-or-no question taking the view (`textShould…:`),
/// yes if it doesn't answer.
pub(crate) fn ask(delegate: Option<&AnyObject>, sel: Sel, view: &AnyObject) -> bool {
    let Some(delegate) = delegate else { return true };
    if !responds(delegate, sel) {
        return true;
    }
    // SAFETY: the delegate methods take the text object and return BOOL.
    let answer: objc2::runtime::Bool = unsafe { MessageReceiver::send_message(delegate, sel, (view,)) };
    answer.as_bool()
}
