//! The string behind a mutable attributed string.
//!
//! `-[NSMutableAttributedString string]` is live on macOS: the object it
//! returns shows later edits. Sidestep's mutable attributed strings keep
//! their text in a `_SidestepAttributedText`, an NSMutableString subclass
//! that owns the buffer, and hand that very object out from `string` and
//! `mutableString`. Edits made through it go to the owner's
//! `replaceCharactersInRange:withString:`, so the runs stay in step. It
//! points back at its owner without retaining it; the owner clears the
//! pointer when it goes away, after which the text is a plain mutable
//! string.
//!
//! An attributed string of another class (an app's subclass implementing
//! the primitives itself) gets a `_SidestepAttributedStringProxy` from
//! `mutableString` instead: a mutable string that reads the owner's
//! `string` and sends every edit to the owner.

use std::cell::Cell;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSMutableString, NSRange, NSString, NSUInteger};

sidestep_runtime::static_class!(pub(crate) ATTRIBUTED_TEXT, ATTRIBUTED_TEXT_META = "_SidestepAttributedText", || {
    let _ = AttributedText::class();
});

pub(crate) struct TextIvars {
    /// The attributed string this is the text of, unretained; null once it
    /// is gone.
    owner: Cell<*const AnyObject>,
}

define_class!(
    #[unsafe(super(NSMutableString, NSString, NSObject))]
    #[name = "_SidestepAttributedText"]
    #[ivars = TextIvars]
    pub(crate) struct AttributedText;

    impl AttributedText {
        #[unsafe(method(replaceCharactersInRange:withString:))]
        fn replace_characters(&self, range: NSRange, string: &NSString) {
            let owner = self.ivars().owner.get();
            if owner.is_null() {
                crate::string::mutable::replace_buffer(self, range, string, "replaceCharactersInRange:withString:");
            } else {
                // SAFETY: the owner outlives the pointer (it clears it when
                // it goes away) and answers the primitive.
                let _: () = unsafe { msg_send![&*owner, replaceCharactersInRange: range, withString: string] };
            }
        }
    }

    unsafe impl NSObjectProtocol for AttributedText {}
);

/// A new backing text with the contents of `string`.
pub(crate) fn new_text(string: &NSString) -> Retained<NSString> {
    crate::load_shell(&ATTRIBUTED_TEXT);
    let this: Allocated<AttributedText> = AttributedText::alloc();
    let this = this.set_ivars(TextIvars { owner: Cell::new(std::ptr::null()) });
    // SAFETY: NSMutableString's initializer, which sets up the buffer.
    let text: Retained<AttributedText> = unsafe { msg_send![super(this), initWithString: string] };
    // SAFETY: an NSString subclass.
    unsafe { Retained::cast_unchecked(text) }
}

fn text_of(text: &NSString) -> Option<&AttributedText> {
    // SAFETY: every object starts with its class pointer.
    let class = unsafe { *(text as *const NSString).cast::<*const sidestep_runtime::Class>() };
    // SAFETY: checked the class.
    std::ptr::eq(class, &ATTRIBUTED_TEXT).then(|| unsafe { &*(text as *const NSString).cast::<AttributedText>() })
}

/// Point a backing text at its owner, or at nothing.
pub(crate) fn set_owner(text: &NSString, owner: *const AnyObject) {
    if let Some(t) = text_of(text) {
        t.ivars().owner.set(owner);
    }
}

pub(crate) struct ProxyIvars {
    owner: Retained<AnyObject>,
}

define_class!(
    #[unsafe(super(NSMutableString, NSString, NSObject))]
    #[name = "_SidestepAttributedStringProxy"]
    #[ivars = ProxyIvars]
    pub(crate) struct Proxy;

    impl Proxy {
        #[unsafe(method(length))]
        fn length(&self) -> NSUInteger {
            self.owner_string().length()
        }

        #[unsafe(method(characterAtIndex:))]
        fn character_at_index(&self, index: NSUInteger) -> u16 {
            self.owner_string().characterAtIndex(index)
        }

        #[unsafe(method(getCharacters:range:))]
        fn get_characters(&self, buffer: std::ptr::NonNull<u16>, range: NSRange) {
            // SAFETY: forwarded; the caller passes room for the range.
            unsafe { self.owner_string().getCharacters_range(buffer, range) }
        }

        #[unsafe(method(replaceCharactersInRange:withString:))]
        fn replace_characters(&self, range: NSRange, string: &NSString) {
            // SAFETY: the owner is an attributed string.
            let _: () = unsafe { msg_send![&*self.ivars().owner, replaceCharactersInRange: range, withString: string] };
        }
    }

    unsafe impl NSObjectProtocol for Proxy {}
);

impl Proxy {
    fn owner_string(&self) -> Retained<NSString> {
        // SAFETY: the owner is an attributed string.
        unsafe { msg_send![&*self.ivars().owner, string] }
    }
}

/// A live mutable string for an attributed string of another class.
pub(crate) fn proxy(owner: &AnyObject) -> Retained<NSMutableString> {
    let _ = NSMutableString::class();
    let this = Proxy::alloc().set_ivars(ProxyIvars { owner: owner.retain() });
    // SAFETY: NSMutableString's initializer.
    let proxy: Retained<Proxy> = unsafe { msg_send![super(this), init] };
    // SAFETY: an NSMutableString subclass.
    unsafe { Retained::cast_unchecked(proxy) }
}
