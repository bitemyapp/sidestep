//! `NSLayoutManager`.

use std::cell::RefCell;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, define_class, msg_send};
use objc2_app_kit::{NSTextStorage, NSTextStorageEditActions};
use objc2_foundation::{NSInteger, NSRange};

sidestep_runtime::static_class!(pub(crate) NSLAYOUTMANAGER, NSLAYOUTMANAGER_META = "NSLayoutManager", || {
    let _ = NSLayoutManagerImpl::class();
});

pub(crate) struct Ivars {
    storage: RefCell<Weak<NSTextStorage>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSLayoutManager"]
    #[ivars = Ivars]
    pub(crate) struct NSLayoutManagerImpl;

    impl NSLayoutManagerImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(Ivars { storage: RefCell::new(Weak::default()) });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(textStorage))]
        fn text_storage(&self) -> Option<Retained<NSTextStorage>> {
            self.ivars().storage.borrow().load()
        }

        #[unsafe(method(setTextStorage:))]
        fn set_text_storage(&self, storage: Option<&NSTextStorage>) {
            *self.ivars().storage.borrow_mut() = storage.map_or_else(Weak::default, Weak::new);
        }

        #[unsafe(method(processEditingForTextStorage:edited:range:changeInLength:invalidatedRange:))]
        fn process_editing(
            &self,
            _storage: &NSTextStorage,
            _mask: NSTextStorageEditActions,
            _range: NSRange,
            _delta: NSInteger,
            _invalidated: NSRange,
        ) {
        }
    }

    unsafe impl NSObjectProtocol for NSLayoutManagerImpl {}
);
