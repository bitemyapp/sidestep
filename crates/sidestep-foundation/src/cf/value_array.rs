//! CoreFoundation arrays of values that needn't be objects: those made
//! with callbacks other than `kCFTypeArrayCallBacks` (or none), and the
//! arrays of `CFRange`s `CFStringCreateArrayWithFindResults` returns. Their
//! values are the caller's pointers, retained, released, described and
//! compared through the callbacks the array was made with, as
//! CoreFoundation does (a NULL callback: nothing, or pointer identity).
//!
//! They are `NSArray` and `NSMutableArray` subclasses, so `CFGetTypeID`,
//! retain and release treat them as arrays. They answer the messages
//! CoreFoundation's functions send (`-count`, `-objectAtIndex:`, copying,
//! equality, hashing, description) without treating a value as an object;
//! the array functions that compare, move or add values reach them
//! through [`of`] instead of Foundation's methods, which would.

use std::ffi::c_void;
use std::sync::{Arc, Mutex, MutexGuard};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSArray, NSMutableArray, NSObject, NSString, NSZone};

use super::collections::ValueCallBacks;
use super::string::CFRange;

type Boolean = u8;
type Retain = unsafe extern "C-unwind" fn(*const c_void, *const c_void) -> *const c_void;
type Release = unsafe extern "C-unwind" fn(*const c_void, *const c_void);
type CopyDescription = unsafe extern "C-unwind" fn(*const c_void) -> *mut c_void;
type Equal = unsafe extern "C-unwind" fn(*const c_void, *const c_void) -> Boolean;

/// An array's callbacks, copied from the `CFArrayCallBacks` it was made
/// with.
#[derive(Clone, Copy, Default)]
pub(crate) struct Callbacks {
    retain: Option<Retain>,
    release: Option<Release>,
    describe: Option<CopyDescription>,
    equal: Option<Equal>,
}

impl Callbacks {
    /// The callbacks `callbacks` points to; none for NULL.
    ///
    /// # Safety
    ///
    /// `callbacks` is null or points to a `CFArrayCallBacks`.
    pub(crate) unsafe fn from(callbacks: *const ValueCallBacks) -> Callbacks {
        // SAFETY: per this function's contract.
        match unsafe { callbacks.as_ref() } {
            None => Callbacks::default(),
            Some(c) => Callbacks { retain: c.retain, release: c.release, describe: c.copy_description, equal: c.equal },
        }
    }

    /// The value to store for `value`: what the retain callback returns.
    fn retain(&self, value: *const c_void) -> *const c_void {
        match self.retain {
            // SAFETY: the array's callback, with the default allocator.
            Some(retain) => unsafe { retain(std::ptr::null(), value) },
            None => value,
        }
    }

    fn release(&self, value: *const c_void) {
        if let Some(release) = self.release {
            // SAFETY: as above.
            unsafe { release(std::ptr::null(), value) };
        }
    }

    /// Whether two values are equal: the same pointer, or equal by the
    /// equal callback.
    pub(crate) fn equal(&self, a: *const c_void, b: *const c_void) -> bool {
        // SAFETY: the array's callback, with values of the array.
        a == b || self.equal.is_some_and(|equal| unsafe { equal(a, b) } != 0)
    }

    /// The equal callback's address, which arrays compared must share.
    fn equality(&self) -> Option<usize> {
        self.equal.map(|f| f as usize)
    }

    fn describe(&self, value: *const c_void) -> String {
        // SAFETY: as above; the callback returns a string the caller owns.
        let made = self.describe.map(|describe| unsafe { describe(value) }).filter(|d| !d.is_null());
        match made {
            // SAFETY: a +1 string, taken over.
            Some(text) => {
                unsafe { Retained::<NSString>::from_raw(text.cast()) }.map(|t| t.to_string()).unwrap_or_default()
            }
            None => format!("<{value:p}>"),
        }
    }
}

/// An array's values and callbacks.
pub(crate) struct Values {
    items: Mutex<Vec<*const c_void>>,
    callbacks: Callbacks,
}

impl Values {
    /// The values, locked. No callback runs while the guard is held, so a
    /// callback may use the array.
    fn items(&self) -> MutexGuard<'_, Vec<*const c_void>> {
        crate::thread::lock(&self.items)
    }

    pub(crate) fn callbacks(&self) -> Callbacks {
        self.callbacks
    }

    /// The values, copied.
    pub(crate) fn snapshot(&self) -> Vec<*const c_void> {
        self.items().clone()
    }

    pub(crate) fn count(&self) -> usize {
        self.items().len()
    }

    fn at(&self, index: usize) -> *const c_void {
        let items = self.items();
        match items.get(index) {
            Some(&value) => value,
            None => {
                let count = items.len();
                drop(items);
                panic!("-[__NSCFArray objectAtIndex:]: index ({index}) beyond bounds ({count})")
            }
        }
    }

    /// Replace the values in `range` with `with`, retaining those and
    /// releasing the ones replaced, as CoreFoundation does. The range lies
    /// in the array.
    pub(crate) fn replace(&self, range: std::ops::Range<usize>, with: &[*const c_void]) {
        let added: Vec<*const c_void> = with.iter().map(|&v| self.callbacks.retain(v)).collect();
        let removed: Vec<*const c_void> = {
            let mut items = self.items();
            let end = range.end.min(items.len());
            let start = range.start.min(end);
            items.splice(start..end, added).collect()
        };
        for value in removed {
            self.callbacks.release(value);
        }
    }

    pub(crate) fn exchange(&self, a: usize, b: usize) {
        let mut items = self.items();
        let count = items.len();
        if a >= count || b >= count {
            drop(items);
            panic!(
                "-[__NSCFArray exchangeObjectAtIndex:withObjectAtIndex:]: index ({}) beyond bounds ({count})",
                a.max(b)
            );
        }
        items.swap(a, b);
    }

    pub(crate) fn check_insert(&self, index: usize) {
        let count = self.count();
        if index > count {
            panic!("-[__NSCFArray insertObject:atIndex:]: index ({index}) beyond bounds ({count})");
        }
    }

    pub(crate) fn check_index(&self, method: &str, index: usize) {
        let count = self.count();
        if index >= count {
            panic!("-[__NSCFArray {method}]: index ({index}) beyond bounds ({count})");
        }
    }

    /// Whether these values equal `other`'s, as `CFEqual` compares arrays:
    /// the same count and, when there are values, the same equal callback
    /// and values equal by it.
    fn equals(&self, other: &AnyObject) -> bool {
        let mine = self.snapshot();
        let (theirs, equality) = match of(other) {
            Some(values) => (values.snapshot(), values.callbacks.equality()),
            None => {
                // SAFETY: -isKindOfClass: takes a class.
                let array: bool = unsafe { msg_send![other, isKindOfClass: NSArray::<AnyObject>::class()] };
                if !array {
                    return false;
                }
                // SAFETY: an array's count.
                let count: usize = unsafe { msg_send![other, count] };
                if count != mine.len() {
                    return false;
                }
                if count == 0 {
                    return true;
                }
                // An array of objects: CoreFoundation's object callbacks.
                let objects = super::collections::object_equality();
                if self.callbacks.equality() != Some(objects) {
                    return false;
                }
                let theirs = (0..count)
                    // SAFETY: indices below the count.
                    .map(|i| unsafe { msg_send![other, objectAtIndex: i] })
                    .map(|o: *const AnyObject| o.cast::<c_void>())
                    .collect();
                (theirs, Some(objects))
            }
        };
        if mine.len() != theirs.len() {
            return false;
        }
        if mine.is_empty() {
            return true;
        }
        self.callbacks.equality() == equality && mine.iter().zip(&theirs).all(|(&a, &b)| self.callbacks.equal(a, b))
    }

    fn description(&self, this: &AnyObject, mutable: bool) -> String {
        let values = self.snapshot();
        let mut out = format!(
            "<CFArray {:p} [{:p}]>{{type = {}, count = {}, values = (",
            this,
            super::types::system_default_allocator(),
            if mutable { "mutable-small" } else { "immutable" },
            values.len()
        );
        if !values.is_empty() {
            out.push('\n');
            for (i, &value) in values.iter().enumerate() {
                out.push_str(&format!("\t{i} : {}\n", self.callbacks.describe(value)));
            }
        }
        out.push_str(")}");
        out
    }
}

impl Drop for Values {
    fn drop(&mut self) {
        let items = std::mem::take(self.items.get_mut().unwrap_or_else(|e| e.into_inner()));
        for value in items {
            self.callbacks.release(value);
        }
    }
}

/// Make an array of `values` (already retained for it) with `callbacks`.
pub(crate) fn make(values: Vec<*const c_void>, callbacks: Callbacks, mutable: bool) -> Retained<AnyObject> {
    let values = Values { items: Mutex::new(values), callbacks };
    // SAFETY: NSArray's -init, on arrays whose storage is their ivars;
    // either is an object.
    unsafe {
        if mutable {
            let this = MutableValueArray::alloc().set_ivars(values);
            let made: Retained<MutableValueArray> = msg_send![super(this), init];
            Retained::cast_unchecked(made)
        } else {
            let this = ValueArray::alloc().set_ivars(values);
            let made: Retained<ValueArray> = msg_send![super(this), init];
            Retained::cast_unchecked(made)
        }
    }
}

/// Make an array of `values`, retaining each with `callbacks`.
pub(crate) fn create(values: &[*const c_void], callbacks: Callbacks, mutable: bool) -> Retained<AnyObject> {
    make(values.iter().map(|&v| callbacks.retain(v)).collect(), callbacks, mutable)
}

/// The values of `cf` if it is one of these arrays.
pub(crate) fn of(cf: &AnyObject) -> Option<&Values> {
    let class = cf.class();
    if std::ptr::eq(class, ValueArray::class()) {
        // SAFETY: an instance of exactly this class.
        Some(unsafe { &*(cf as *const AnyObject).cast::<ValueArray>() }.ivars())
    } else if std::ptr::eq(class, MutableValueArray::class()) {
        // SAFETY: as above.
        Some(unsafe { &*(cf as *const AnyObject).cast::<MutableValueArray>() }.ivars())
    } else {
        None
    }
}

/// [`of`] for a CoreFoundation pointer.
///
/// # Safety
///
/// `cf` is a live object.
pub(crate) unsafe fn of_cf<'a>(cf: *const c_void) -> Option<&'a Values> {
    // SAFETY: per this function's contract.
    of(unsafe { &*cf.cast::<AnyObject>() })
}

/// `CFEqual` of two objects when either is one of these arrays.
///
/// # Safety
///
/// Both are live objects.
pub(crate) unsafe fn cf_equal(a: *const c_void, b: *const c_void) -> Option<bool> {
    // SAFETY: per this function's contract.
    let (a, b) = unsafe { (&*a.cast::<AnyObject>(), &*b.cast::<AnyObject>()) };
    match (of(a), of(b)) {
        (Some(values), _) => Some(values.equals(b)),
        (None, Some(values)) => Some(values.equals(a)),
        (None, None) => None,
    }
}

/// The array `CFStringCreateArrayWithFindResults` returns: a mutable array
/// (as macOS's is) of pointers to `ranges`, which live as long as an array
/// holds them.
pub(crate) fn of_ranges(ranges: Vec<CFRange>) -> Retained<AnyObject> {
    let values = ranges.into_iter().map(|r| Arc::into_raw(Arc::new(r)).cast::<c_void>()).collect();
    let callbacks = Callbacks {
        retain: Some(range_retain),
        release: Some(range_release),
        describe: Some(range_description),
        equal: Some(range_equal),
    };
    make(values, callbacks, true)
}

unsafe extern "C-unwind" fn range_retain(_alloc: *const c_void, value: *const c_void) -> *const c_void {
    // SAFETY: the values of a find-results array are its ranges, each an
    // `Arc` the arrays holding it count.
    unsafe { Arc::increment_strong_count(value.cast::<CFRange>()) };
    value
}

unsafe extern "C-unwind" fn range_release(_alloc: *const c_void, value: *const c_void) {
    // SAFETY: as above.
    unsafe { Arc::decrement_strong_count(value.cast::<CFRange>()) };
}

unsafe extern "C-unwind" fn range_description(value: *const c_void) -> *mut c_void {
    // SAFETY: a range.
    let range = unsafe { &*value.cast::<CFRange>() };
    super::types::owned(NSString::from_str(&format!("{{{}, {}}}", range.location, range.length)))
}

unsafe extern "C-unwind" fn range_equal(a: *const c_void, b: *const c_void) -> Boolean {
    // SAFETY: ranges.
    let (a, b) = unsafe { (&*a.cast::<CFRange>(), &*b.cast::<CFRange>()) };
    u8::from(a.location == b.location && a.length == b.length)
}

define_class!(
    /// An immutable CoreFoundation array of values that needn't be objects.
    #[unsafe(super(NSArray, NSObject))]
    #[name = "_SidestepCFValueArray"]
    #[ivars = Values]
    struct ValueArray;

    impl ValueArray {
        #[unsafe(method(count))]
        fn count(&self) -> usize {
            self.ivars().count()
        }

        #[unsafe(method(objectAtIndex:))]
        fn object_at(&self, index: usize) -> *mut AnyObject {
            self.ivars().at(index).cast_mut().cast()
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<AnyObject> {
            let values = self.ivars();
            create(&values.snapshot(), values.callbacks, false)
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<AnyObject> {
            let values = self.ivars();
            create(&values.snapshot(), values.callbacks, true)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| self.ivars().equals(other))
        }

        #[unsafe(method(isEqualToArray:))]
        fn is_equal_to_array(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| self.ivars().equals(other))
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            self.ivars().count()
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().description(self.as_ref(), false))
        }

        #[unsafe(method_id(descriptionWithLocale:))]
        fn description_with_locale(&self, _locale: Option<&AnyObject>) -> Retained<NSString> {
            NSString::from_str(&self.ivars().description(self.as_ref(), false))
        }
    }
);

define_class!(
    /// A mutable CoreFoundation array of values that needn't be objects.
    #[unsafe(super(NSMutableArray, NSArray, NSObject))]
    #[name = "_SidestepCFMutableValueArray"]
    #[ivars = Values]
    struct MutableValueArray;

    impl MutableValueArray {
        #[unsafe(method(count))]
        fn count(&self) -> usize {
            self.ivars().count()
        }

        #[unsafe(method(objectAtIndex:))]
        fn object_at(&self, index: usize) -> *mut AnyObject {
            self.ivars().at(index).cast_mut().cast()
        }

        #[unsafe(method(insertObject:atIndex:))]
        fn insert(&self, value: *mut AnyObject, index: usize) {
            self.ivars().check_insert(index);
            self.ivars().replace(index..index, &[value.cast_const().cast()]);
        }

        #[unsafe(method(addObject:))]
        fn add(&self, value: *mut AnyObject) {
            let end = self.ivars().count();
            self.ivars().replace(end..end, &[value.cast_const().cast()]);
        }

        #[unsafe(method(removeObjectAtIndex:))]
        fn remove(&self, index: usize) {
            self.ivars().check_index("removeObjectAtIndex:", index);
            self.ivars().replace(index..index + 1, &[]);
        }

        #[unsafe(method(removeLastObject))]
        fn remove_last(&self) {
            let count = self.ivars().count();
            if count > 0 {
                self.ivars().replace(count - 1..count, &[]);
            }
        }

        #[unsafe(method(replaceObjectAtIndex:withObject:))]
        fn replace(&self, index: usize, value: *mut AnyObject) {
            self.ivars().check_index("replaceObjectAtIndex:withObject:", index);
            self.ivars().replace(index..index + 1, &[value.cast_const().cast()]);
        }

        #[unsafe(method(removeAllObjects))]
        fn remove_all(&self) {
            let count = self.ivars().count();
            self.ivars().replace(0..count, &[]);
        }

        #[unsafe(method(exchangeObjectAtIndex:withObjectAtIndex:))]
        fn exchange(&self, a: usize, b: usize) {
            self.ivars().exchange(a, b);
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<AnyObject> {
            let values = self.ivars();
            create(&values.snapshot(), values.callbacks, false)
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<AnyObject> {
            let values = self.ivars();
            create(&values.snapshot(), values.callbacks, true)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| self.ivars().equals(other))
        }

        #[unsafe(method(isEqualToArray:))]
        fn is_equal_to_array(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| self.ivars().equals(other))
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            self.ivars().count()
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().description(self.as_ref(), true))
        }

        #[unsafe(method_id(descriptionWithLocale:))]
        fn description_with_locale(&self, _locale: Option<&AnyObject>) -> Retained<NSString> {
            NSString::from_str(&self.ivars().description(self.as_ref(), true))
        }
    }
);
