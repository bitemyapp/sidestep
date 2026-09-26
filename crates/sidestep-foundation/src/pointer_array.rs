//! `NSPointerArray`: an ordered list of pointers, held as its pointer
//! functions say (see `pointer_table.rs`), which may be null.
//!
//! The pointers are [`Item`]s in a vector, in a [`Guarded`] cell as the other
//! collections keep theirs. A weak item whose object deallocates reads as
//! null from then on and stays in its place until `-compact` removes it,
//! with the nulls stored explicitly. Pointers handed out
//! (`-pointerAtIndex:`, fast enumeration) are a strong item's object as it
//! is, a weak item's loaded and autoreleased, so it stays alive for the
//! caller.

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSFastEnumerationState, NSPointerArray, NSPointerFunctions, NSPointerFunctionsOptions, NSUInteger, NSZone,
};

use crate::array;
use crate::enumerator::Mutations;
use crate::guarded::Guarded;
use crate::pointer_table::{self, Functions, Item};
use crate::util;

sidestep_runtime::static_class!(pub(crate) NSPOINTERARRAY, NSPOINTERARRAY_META = "NSPointerArray", || {
    let _ = NSPointerArrayImpl::class();
});

const NAME: &str = "NSPointerArray";

pub(crate) struct PointerArrayIvars {
    functions: Functions,
    items: Guarded<Vec<Item>>,
    mutations: Mutations,
}

impl PointerArrayIvars {
    fn new(functions: Functions, items: Vec<Item>) -> Self {
        PointerArrayIvars { functions, items: Guarded::new(items), mutations: Mutations::default() }
    }
}

/// `pointer` held as `functions` say; null stays null whatever they say.
///
/// # Safety
/// As for `Item::new`, unless `pointer` is null.
unsafe fn hold(functions: Functions, pointer: *mut c_void) -> Item {
    if pointer.is_null() {
        Item::Raw(ptr::null_mut())
    } else {
        // SAFETY: guaranteed by the caller.
        unsafe { Item::new(functions, pointer) }
    }
}

fn make(functions: Functions) -> Retained<NSPointerArray> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSPointerArray::alloc();
    // SAFETY: NSPointerArray's class is NSPointerArrayImpl.
    let this = unsafe { std::mem::transmute::<Allocated<NSPointerArray>, Allocated<NSPointerArrayImpl>>(this) };
    // SAFETY: NSPointerArrayImpl is the class registered as NSPointerArray.
    unsafe { Retained::cast_unchecked(init(this, functions, Vec::new())) }
}

fn init(this: Allocated<NSPointerArrayImpl>, functions: Functions, items: Vec<Item>) -> Retained<NSPointerArrayImpl> {
    let this = this.set_ivars(PointerArrayIvars::new(functions, items));
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn options(options: NSPointerFunctionsOptions) -> Functions {
    Functions::from_options(options, NAME, "initWithOptions:")
}

/// Fail as Foundation does for an index past the end.
#[cold]
#[track_caller]
fn beyond(method: &str, what: &str, index: usize, count: usize) -> ! {
    panic!("*** -[{NAME} {method}]: attempt to {what} pointer at index {index} beyond bounds {count}");
}

impl NSPointerArrayImpl {
    fn obj(&self) -> *const AnyObject {
        ptr::from_ref(self).cast()
    }

    /// The items for changing. Items taken out must be dropped after the
    /// reference's last use: releasing may run code.
    ///
    /// # Safety
    /// As for `Guarded::write`.
    #[allow(clippy::mut_from_ref)]
    unsafe fn items(&self) -> &mut Vec<Item> {
        // SAFETY: guaranteed by the caller.
        unsafe { self.ivars().items.write(NAME, self.obj()) }
    }

    fn changed(&self) {
        self.ivars().mutations.bump();
    }

    fn len(&self) -> usize {
        // SAFETY: reading the length runs no other code.
        unsafe { self.ivars().items.peek() }.len()
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSPointerArray"]
    #[ivars = PointerArrayIvars]
    pub(crate) struct NSPointerArrayImpl;

    impl NSPointerArrayImpl {
        #[unsafe(method_id(pointerArrayWithOptions:))]
        fn with_options(o: NSPointerFunctionsOptions) -> Retained<NSPointerArray> {
            make(options(o))
        }

        #[unsafe(method_id(pointerArrayWithPointerFunctions:))]
        fn with_pointer_functions(functions: &NSPointerFunctions) -> Retained<NSPointerArray> {
            make(pointer_table::functions_of(functions))
        }

        #[unsafe(method_id(strongObjectsPointerArray))]
        fn strong_objects() -> Retained<NSPointerArray> {
            make(Functions::STRONG)
        }

        #[unsafe(method_id(weakObjectsPointerArray))]
        fn weak_objects() -> Retained<NSPointerArray> {
            make(Functions::WEAK)
        }

        #[unsafe(method_id(pointerArrayWithStrongObjects))]
        fn old_strong_objects() -> Retained<AnyObject> {
            util::upcast(make(Functions::STRONG))
        }

        #[unsafe(method_id(pointerArrayWithWeakObjects))]
        fn old_weak_objects() -> Retained<AnyObject> {
            util::upcast(make(Functions::WEAK))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init(this, Functions::STRONG, Vec::new())
        }

        #[unsafe(method_id(initWithOptions:))]
        fn init_with_options(this: Allocated<Self>, o: NSPointerFunctionsOptions) -> Retained<Self> {
            init(this, options(o), Vec::new())
        }

        #[unsafe(method_id(initWithPointerFunctions:))]
        fn init_with_pointer_functions(this: Allocated<Self>, functions: &NSPointerFunctions) -> Retained<Self> {
            init(this, pointer_table::functions_of(functions), Vec::new())
        }

        #[unsafe(method_id(pointerFunctions))]
        fn pointer_functions(&self) -> Retained<NSPointerFunctions> {
            pointer_table::make_pointer_functions(self.ivars().functions)
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.len()
        }

        #[unsafe(method(pointerAtIndex:))]
        fn pointer_at_index(&self, index: NSUInteger) -> *mut c_void {
            let items = self.ivars().items.read();
            match items.get(index) {
                Some(item) => item.handed_out(),
                None => beyond("pointerAtIndex:", "access", index, items.len()),
            }
        }

        #[unsafe(method(addPointer:))]
        fn add_pointer(&self, pointer: *mut c_void) {
            // Held before the items are taken: copying may run code.
            // SAFETY: callers pass pointers of the array's kind.
            let item = unsafe { hold(self.ivars().functions, pointer) };
            // SAFETY: only the items change.
            unsafe { self.items() }.push(item);
            self.changed();
        }

        #[unsafe(method(insertPointer:atIndex:))]
        fn insert_pointer(&self, pointer: *mut c_void, index: NSUInteger) {
            let count = self.len();
            if index > count {
                beyond("insertPointer:atIndex:", "insert", index, count);
            }
            // SAFETY: as in -addPointer:.
            let item = unsafe { hold(self.ivars().functions, pointer) };
            // SAFETY: only the items change.
            unsafe { self.items() }.insert(index, item);
            self.changed();
        }

        #[unsafe(method(removePointerAtIndex:))]
        fn remove_pointer(&self, index: NSUInteger) {
            let count = self.len();
            if index >= count {
                beyond("removePointerAtIndex:", "remove", index, count);
            }
            // SAFETY: only the items change; the item is released after.
            let removed = unsafe { self.items() }.remove(index);
            self.changed();
            drop(removed);
        }

        #[unsafe(method(replacePointerAtIndex:withPointer:))]
        fn replace_pointer(&self, index: NSUInteger, pointer: *mut c_void) {
            let count = self.len();
            if index >= count {
                beyond("replacePointerAtIndex:withPointer:", "replace", index, count);
            }
            // SAFETY: as in -addPointer:.
            let item = unsafe { hold(self.ivars().functions, pointer) };
            // SAFETY: only the items change; the old item is released after.
            let old = std::mem::replace(&mut unsafe { self.items() }[index], item);
            self.changed();
            drop(old);
        }

        /// Removes the nulls, weak references whose objects went included.
        #[unsafe(method(compact))]
        fn compact(&self) {
            // SAFETY: only the items change; what is removed is released
            // after.
            let items = unsafe { self.items() };
            let (kept, removed): (Vec<Item>, Vec<Item>) =
                std::mem::take(items).into_iter().partition(|item| !item.pointer().is_null());
            *items = kept;
            self.changed();
            drop(removed);
        }

        /// Grows with nulls, or drops pointers from the end.
        #[unsafe(method(setCount:))]
        fn set_count(&self, count: NSUInteger) {
            // SAFETY: only the items change; what is removed is released
            // after.
            let items = unsafe { self.items() };
            let removed = if count < items.len() { items.split_off(count) } else { Vec::new() };
            items.resize_with(count, || Item::Raw(ptr::null_mut()));
            self.changed();
            drop(removed);
        }

        /// The objects, nulls left out.
        #[unsafe(method_id(allObjects))]
        fn all_objects(&self) -> Retained<NSArray> {
            let loaded: Vec<Option<Retained<AnyObject>>> = self.ivars().items.read().iter().map(Item::load).collect();
            array::make(loaded.into_iter().flatten().collect())
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSPointerArray> {
            let ivars = self.ivars();
            let mut loaded = Vec::new();
            let items: Vec<Item> = ivars
                .items
                .read()
                .iter()
                .map(|item| item.duplicate(&mut loaded).unwrap_or(Item::Raw(ptr::null_mut())))
                .collect();
            drop(loaded);
            let copy = NSPointerArray::alloc();
            // SAFETY: as in `make`.
            let copy = unsafe { std::mem::transmute::<Allocated<NSPointerArray>, Allocated<NSPointerArrayImpl>>(copy) };
            // SAFETY: NSPointerArrayImpl is the class registered as
            // NSPointerArray.
            unsafe { Retained::cast_unchecked(init(copy, ivars.functions, items)) }
        }

        /// Every pointer, nulls included.
        #[unsafe(method(countByEnumeratingWithState:objects:count:))]
        fn count_by_enumerating(
            &self,
            state: NonNull<NSFastEnumerationState>,
            buffer: NonNull<*mut AnyObject>,
            len: NSUInteger,
        ) -> NSUInteger {
            let ivars = self.ivars();
            let items = ivars.items.read();
            // SAFETY: the caller passes a valid state and room for `len`
            // pointers. Weak items are handed out autoreleased, which keeps
            // them alive for the loop.
            unsafe {
                crate::enumerator::batch(state, buffer, len, ivars.mutations.as_ptr(), |i| {
                    items.get(i).map(|item| item.handed_out().cast())
                })
            }
        }
    }

    unsafe impl NSObjectProtocol for NSPointerArrayImpl {}
);
