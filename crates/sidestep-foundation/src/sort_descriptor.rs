//! `NSSortDescriptor`, and sorting collections by descriptors
//! (`-sortedArrayUsingDescriptors:`, `-sortUsingDescriptors:`).
//!
//! A descriptor names a key path, a direction and how to compare: a
//! selector the values answer (`compare:` unless another is given) or a
//! comparator block. Sorting by several descriptors compares by the first
//! and falls back to the next on a tie; the sort is stable.
//!
//! Sorting reads each element's values once, before any comparison, rather
//! than twice per comparison: key paths may cost a message per step, and a
//! sort makes `n log n` comparisons. Values that are Sidestep's own numbers
//! compare without messages under the default `compare:`.
//!
//! Key paths are resolved as key-value coding does for the classes that
//! matter here, without Foundation's full key-value coding (which Sidestep
//! doesn't have): each step sends `-valueForKey:` to objects that answer it
//! (dictionaries, which look the key up), and otherwise calls the object's
//! getter for the key (`-getKey`, `-key`, `-isKey` or `-_key`), wrapping a
//! number or `BOOL` it returns in an `NSNumber`. `self` is the object
//! itself. A step that finds no getter sends `-valueForUndefinedKey:` to an
//! object that answers it, and otherwise fails as Foundation's does. How a
//! step reads a class's objects is found once per class a sort meets, and
//! each step's getter selectors are registered when the descriptor is made.
//!
//! A nil key compares the elements themselves. Nil values sort before all
//! others, and `NSNull` counts as nil, as in Foundation: wherever it is
//! met along a key path, and as the element itself. Nil values are never
//! handed to the comparator or the selector.

use std::cmp::Ordering;
use std::ffi::{CString, c_void};
use std::ptr::{self, NonNull};

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Imp, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_foundation::{NSArray, NSComparator, NSInteger, NSNumber, NSSortDescriptor, NSString, NSUInteger, NSZone};

use crate::dictionary;
use crate::number::{self, Number};
use crate::util::{self, is_exactly};

sidestep_runtime::static_class!(pub(crate) NSSORTDESCRIPTOR, NSSORTDESCRIPTOR_META = "NSSortDescriptor", || {
    let _ = NSSortDescriptorImpl::class();
});

/// A comparator block, its result read as the `NSInteger` it is in the
/// block ABI: a block may return any value, which Rust's
/// `NSComparisonResult` enum can't hold.
type Comparator = RcBlock<dyn Fn(NonNull<AnyObject>, NonNull<AnyObject>) -> NSInteger>;

/// How a descriptor compares two values.
#[derive(Clone)]
enum How {
    /// `[a selector b]`.
    Selector(Sel),
    Block(Comparator),
    /// Only from `-init`: no way to compare, so every pair ties.
    Nothing,
}

/// A step of a key path: its key, as text and as the string `-valueForKey:`
/// takes, and the selectors of the getters key-value coding looks for, in
/// the order it looks (`getKey`, `key`, `isKey`, `_key`).
#[derive(Clone)]
struct Step {
    text: String,
    key: Retained<NSString>,
    getters: [Option<Sel>; 4],
}

impl Step {
    fn new(text: &str) -> Step {
        let mut chars = text.chars();
        let capitalized: String = chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default();
        let names = [format!("get{capitalized}"), text.to_owned(), format!("is{capitalized}"), format!("_{text}")];
        let getters = names.map(|name| CString::new(name).ok().map(|name| Sel::register(&name)));
        Step { text: text.to_owned(), key: NSString::from_str(text), getters }
    }
}

/// How a step reads the value of the objects of one class.
#[derive(Clone, Copy)]
enum Access {
    /// `-valueForKey:`, which the class answers.
    ValueForKey,
    /// The object itself, for the key `self`.
    Itself,
    /// A getter taking no arguments, with the type code of its result.
    Getter(Sel, Imp, u8),
    /// No getter: `-valueForUndefinedKey:`, which the class answers.
    Undefined,
    /// Nothing: the step fails as Foundation's `-valueForUndefinedKey:`.
    Fails,
}

/// The class a step last met and how it read that class's objects: a
/// sort's elements are nearly always of one class, so a step is resolved
/// once per sort.
type Found = Option<(*const AnyClass, Access)>;

#[derive(Clone)]
pub(crate) struct DescriptorIvars {
    key: Option<Retained<NSString>>,
    /// The key path's steps; empty for no key.
    path: Box<[Step]>,
    ascending: bool,
    how: How,
}

impl Default for DescriptorIvars {
    fn default() -> Self {
        DescriptorIvars { key: None, path: Box::default(), ascending: false, how: How::Nothing }
    }
}

impl DescriptorIvars {
    fn new(key: Option<&NSString>, ascending: bool, how: How) -> Self {
        // An empty key is no key, as in Foundation.
        let key = key.filter(|k| k.length() > 0).map(|k| {
            // SAFETY: an NSString copied is an NSString.
            unsafe { Retained::cast_unchecked::<NSString>(util::copy_key(k)) }
        });
        let path = key.as_ref().map(|k| k.to_string().split('.').map(Step::new).collect()).unwrap_or_default();
        DescriptorIvars { key, path, ascending, how }
    }

    /// The value this descriptor compares for `obj`. `found` remembers, for
    /// as many steps as it has room, how the step read the last class it
    /// met.
    fn value(&self, obj: &AnyObject, found: &mut [Found]) -> Option<Retained<AnyObject>> {
        let Some((first, rest)) = self.path.split_first() else { return Some(obj.retain()) };
        let mut spare = None;
        let mut value = value_for_key(obj, first, found.first_mut().unwrap_or(&mut spare))?;
        for (i, step) in rest.iter().enumerate() {
            let mut spare = None;
            value = value_for_key(&value, step, found.get_mut(i + 1).unwrap_or(&mut spare))?;
        }
        Some(value)
    }

    /// Whether this descriptor compares with `compare:`, which Sidestep's
    /// own numbers answer without a message.
    fn compares(&self) -> bool {
        matches!(self.how, How::Selector(sel) if sel == sel!(compare:))
    }

    fn direct(&self, order: Ordering) -> Ordering {
        if self.ascending { order } else { order.reverse() }
    }

    /// The order of two values, ascending or descending as the descriptor
    /// says. `NSNull` is nil.
    fn compare(&self, a: Option<&AnyObject>, b: Option<&AnyObject>) -> Ordering {
        let order = match (a.filter(|a| !is_null(a)), b.filter(|b| !is_null(b))) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(a), Some(b)) => match &self.how {
                How::Selector(sel) => compare_by(*sel, a, b),
                How::Block(block) => block.call((NonNull::from(a), NonNull::from(b))).cmp(&0),
                How::Nothing => Ordering::Equal,
            },
        };
        self.direct(order)
    }
}

/// `[a selector b]` as an ordering. Sidestep's own numbers compare without
/// a message under `compare:`.
fn compare_by(selector: Sel, a: &AnyObject, b: &AnyObject) -> Ordering {
    if selector == sel!(compare:)
        && let (Some(x), Some(y)) = (number::fast_value(a), number::fast_value(b))
    {
        return x.compare(&y);
    }
    let a = ptr::from_ref(a).cast_mut();
    let b = ptr::from_ref(b).cast_mut();
    crate::array::selector_order(selector)(a, b)
}

/// One of a sort's descriptors, read once for the sort.
enum Descriptor<'a> {
    Own(&'a DescriptorIvars),
    /// Any other object, asked `-compareObject:toObject:` for each pair.
    Foreign(&'a AnyObject),
}

impl Descriptor<'_> {
    fn of(obj: &AnyObject) -> Descriptor<'_> {
        if is_exactly(obj, &NSSORTDESCRIPTOR) {
            // SAFETY: an instance of exactly NSSortDescriptorImpl.
            Descriptor::Own(unsafe { &*(obj as *const AnyObject).cast::<NSSortDescriptorImpl>() }.ivars())
        } else {
            Descriptor::Foreign(obj)
        }
    }
}

/// What a descriptor compares, for each element of a sort.
enum Column {
    /// The elements themselves (a descriptor without a key).
    Elements,
    Values(Vec<Option<Retained<AnyObject>>>),
    /// Values that are all Sidestep's own numbers, compared by `compare:`:
    /// read once, compared without messages.
    Numbers(Vec<Number>),
}

impl Column {
    fn of(descriptor: &DescriptorIvars, items: &[Retained<AnyObject>]) -> Column {
        let numbers = |values: &mut dyn Iterator<Item = Option<&AnyObject>>| -> Option<Vec<Number>> {
            values.map(|v| v.and_then(number::fast_value)).collect()
        };
        if descriptor.path.is_empty() {
            let numbers = if descriptor.compares() { numbers(&mut items.iter().map(|o| Some(&**o))) } else { None };
            return numbers.map_or(Column::Elements, Column::Numbers);
        }
        let mut found = vec![None; descriptor.path.len()];
        let values: Vec<Option<Retained<AnyObject>>> = items.iter().map(|o| descriptor.value(o, &mut found)).collect();
        let numbers = if descriptor.compares() { numbers(&mut values.iter().map(|v| v.as_deref())) } else { None };
        match numbers {
            Some(numbers) => Column::Numbers(numbers),
            None => Column::Values(values),
        }
    }
}

/// The positions of `items` in the order `descriptors` sort them, stably.
/// Each element's values are read once, up front.
pub(crate) fn positions(descriptors: &NSArray, items: &[Retained<AnyObject>]) -> Vec<usize> {
    let descriptors = descriptors.to_vec();
    let descriptors: Vec<Descriptor> = descriptors.iter().map(|d| Descriptor::of(d)).collect();
    let columns: Vec<Option<Column>> = descriptors
        .iter()
        .map(|d| match d {
            Descriptor::Own(d) => Some(Column::of(d, items)),
            Descriptor::Foreign(_) => None,
        })
        .collect();
    let mut order: Vec<usize> = (0..items.len()).collect();
    crate::array::sort_stable(&mut order, &mut |a, b| {
        for (descriptor, column) in descriptors.iter().zip(&columns) {
            let verdict = match (descriptor, column) {
                (Descriptor::Own(d), Some(Column::Numbers(numbers))) => d.direct(numbers[a].compare(&numbers[b])),
                (Descriptor::Own(d), Some(Column::Values(values))) => {
                    d.compare(values[a].as_deref(), values[b].as_deref())
                }
                (Descriptor::Own(d), _) => d.compare(Some(&items[a]), Some(&items[b])),
                (Descriptor::Foreign(d), _) => {
                    // SAFETY: -compareObject:toObject: takes two objects and
                    // returns NSComparisonResult, an NSInteger.
                    let result: NSInteger = unsafe { msg_send![*d, compareObject: &*items[a], toObject: &*items[b]] };
                    result.cmp(&0)
                }
            };
            if verdict != Ordering::Equal {
                return verdict;
            }
        }
        Ordering::Equal
    });
    order
}

/// `items` in the order `descriptors` give, as a new immutable array.
pub(crate) fn sorted(descriptors: &NSArray, items: &[Retained<AnyObject>]) -> Retained<NSArray> {
    let order = positions(descriptors, items);
    crate::array::make(order.into_iter().map(|i| items[i].clone()).collect())
}

/// Fail as Foundation's `-valueForUndefinedKey:` does.
#[cold]
pub(crate) fn undefined_key(obj: &AnyObject, key: &str) -> ! {
    let class = obj.class().name().to_string_lossy();
    panic!(
        "[<{class} {:p}> valueForUndefinedKey:]: this class is not key value coding-compliant for the key {key}.",
        ptr::from_ref(obj)
    );
}

/// Whether `obj` is `NSNull`, which sorts as nil.
#[inline]
fn is_null(obj: &AnyObject) -> bool {
    is_exactly(obj, &crate::NSNULL)
}

/// `[obj valueForKey:key]`, as the start of this module describes; `found`
/// is how the step last read a class.
fn value_for_key(obj: &AnyObject, step: &Step, found: &mut Found) -> Option<Retained<AnyObject>> {
    // Foundation reads a key of NSNull as NSNull, which sorts as nil.
    if is_null(obj) {
        return None;
    }
    if let Some(value) = dictionary::own_value_for_key(obj, &step.key, &step.text) {
        return value;
    }
    let class = obj.class();
    let access = match *found {
        Some((seen, access)) if ptr::eq(seen, class) => access,
        _ => {
            let access = if class.responds_to(sel!(valueForKey:)) {
                Access::ValueForKey
            } else if step.text == "self" {
                Access::Itself
            } else {
                accessor(class, step)
            };
            *found = Some((class, access));
            access
        }
    };
    read(obj, step, access)
}

/// How NSObject's key-value coding reads `step` of `class`'s objects: its
/// getter, or else `-valueForUndefinedKey:`.
fn accessor(class: &AnyClass, step: &Step) -> Access {
    for sel in step.getters.into_iter().flatten() {
        let Some(method) = class.instance_method(sel) else { continue };
        if method.arguments_count() != 2 {
            continue;
        }
        // Qualifiers (const, in, out and so on) come before the type.
        let kind = method.return_type().to_bytes().iter().copied().find(|c| !b"rnNoORV".contains(c));
        match kind {
            Some(
                kind
                @ (b'@' | b'c' | b'C' | b'B' | b's' | b'S' | b'i' | b'I' | b'l' | b'L' | b'q' | b'Q' | b'f' | b'd'),
            ) => {
                return Access::Getter(sel, method.implementation(), kind);
            }
            _ => continue,
        }
    }
    if class.responds_to(sel!(valueForUndefinedKey:)) { Access::Undefined } else { Access::Fails }
}

/// The value `access` reads from `obj` for `step`.
fn read(obj: &AnyObject, step: &Step, access: Access) -> Option<Retained<AnyObject>> {
    match access {
        // SAFETY: -valueForKey: takes a string and returns an object or nil.
        Access::ValueForKey => unsafe { msg_send![obj, valueForKey: &*step.key] },
        Access::Itself => Some(obj.retain()),
        Access::Getter(sel, imp, kind) => call_getter(obj, sel, imp, kind),
        // SAFETY: -valueForUndefinedKey: takes a string and returns an
        // object or nil.
        Access::Undefined => unsafe { msg_send![obj, valueForUndefinedKey: &*step.key] },
        Access::Fails => undefined_key(obj, &step.text),
    }
}

/// Call the getter `imp` of `obj`, whose result has the type code `kind`,
/// wrapping a number in an `NSNumber`.
fn call_getter(obj: &AnyObject, sel: Sel, imp: Imp, kind: u8) -> Option<Retained<AnyObject>> {
    let receiver = ptr::from_ref(obj).cast_mut();
    /// Call the getter as a function returning `$t`.
    macro_rules! call {
        ($t:ty) => {{
            // SAFETY: `accessor` found `imp` as the implementation of a
            // method of `obj`'s class taking no arguments, whose encoding
            // says it returns `$t`.
            let f: unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> $t = unsafe { std::mem::transmute(imp) };
            unsafe { f(receiver, sel) }
        }};
    }
    let number: Retained<NSNumber> = match kind {
        b'@' => {
            let value: *mut AnyObject = call!(*mut AnyObject);
            // SAFETY: a getter returns an object it keeps alive (or an
            // autoreleased one), or nil.
            return unsafe { Retained::retain(value) };
        }
        b'c' => NSNumber::new_i8(call!(i8)),
        b'C' => NSNumber::new_u8(call!(u8)),
        b'B' => NSNumber::new_bool(call!(bool)),
        b's' => NSNumber::new_i16(call!(i16)),
        b'S' => NSNumber::new_u16(call!(u16)),
        b'i' => NSNumber::new_i32(call!(i32)),
        b'I' => NSNumber::new_u32(call!(u32)),
        b'l' => NSNumber::new_isize(call!(isize)),
        b'L' => NSNumber::new_usize(call!(usize)),
        b'q' => NSNumber::new_i64(call!(i64)),
        b'Q' => NSNumber::new_u64(call!(u64)),
        b'f' => NSNumber::new_f32(call!(f32)),
        b'd' => NSNumber::new_f64(call!(f64)),
        _ => unreachable!("`accessor` returns only these kinds"),
    };
    Some(util::upcast(number))
}

/// NSObject's `-valueForKey:` for `key`, which reads the object's own
/// properties: its getter, else `-valueForUndefinedKey:`. For
/// `-[NSDictionary valueForKey:]` with a key starting with `@`.
pub(crate) fn property(obj: &AnyObject, key: &str) -> Option<Retained<AnyObject>> {
    let step = Step::new(key);
    read(obj, &step, accessor(obj.class(), &step))
}

/// Whether `other` is a descriptor with the same key, direction and way of
/// comparing as `ours`.
fn equal_descriptors(ours: &NSSortDescriptorImpl, other: &AnyObject) -> bool {
    if ptr::eq(ptr::from_ref(ours).cast(), other) {
        return true;
    }
    if !is_exactly(other, &NSSORTDESCRIPTOR) {
        return false;
    }
    // SAFETY: an instance of exactly NSSortDescriptorImpl.
    let theirs = unsafe { &*(other as *const AnyObject).cast::<NSSortDescriptorImpl>() }.ivars();
    let ours = ours.ivars();
    let same_key = match (&ours.key, &theirs.key) {
        (None, None) => true,
        (Some(a), Some(b)) => util::equal(a, b),
        _ => false,
    };
    let same_how = match (&ours.how, &theirs.how) {
        (How::Selector(a), How::Selector(b)) => a == b,
        (How::Block(a), How::Block(b)) => ptr::eq::<DynBlock<_>>(&**a, &**b),
        (How::Nothing, How::Nothing) => true,
        _ => false,
    };
    same_key && same_how && ours.ascending == theirs.ascending
}

fn make(ivars: DescriptorIvars) -> Retained<NSSortDescriptor> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSSortDescriptor::alloc();
    // SAFETY: NSSortDescriptor's class is NSSortDescriptorImpl.
    let this = unsafe { std::mem::transmute::<Allocated<NSSortDescriptor>, Allocated<NSSortDescriptorImpl>>(this) };
    // SAFETY: NSSortDescriptorImpl is the class registered as
    // NSSortDescriptor.
    unsafe { Retained::cast_unchecked(init(this, ivars)) }
}

fn init(this: Allocated<NSSortDescriptorImpl>, ivars: DescriptorIvars) -> Retained<NSSortDescriptorImpl> {
    let this = this.set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// A comparator block, copied.
fn comparator(block: NSComparator) -> How {
    // SAFETY: the caller passes a valid block, or null; NSComparisonResult
    // is an NSInteger in the block ABI, so viewing the result as one is the
    // same call.
    match unsafe {
        RcBlock::copy(block.cast::<DynBlock<dyn Fn(NonNull<AnyObject>, NonNull<AnyObject>) -> NSInteger>>())
    } {
        Some(block) => How::Block(block),
        None => How::Nothing,
    }
}

/// A selector to compare by; nil means `compare:`, as in Foundation.
fn selector_or_compare(selector: Option<Sel>) -> How {
    How::Selector(selector.unwrap_or(sel!(compare:)))
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSSortDescriptor"]
    #[ivars = DescriptorIvars]
    pub(crate) struct NSSortDescriptorImpl;

    impl NSSortDescriptorImpl {
        #[unsafe(method_id(sortDescriptorWithKey:ascending:))]
        fn with_key(key: Option<&NSString>, ascending: bool) -> Retained<NSSortDescriptor> {
            make(DescriptorIvars::new(key, ascending, How::Selector(sel!(compare:))))
        }

        #[unsafe(method_id(sortDescriptorWithKey:ascending:selector:))]
        fn with_key_selector(key: Option<&NSString>, ascending: bool, selector: Option<Sel>) -> Retained<NSSortDescriptor> {
            make(DescriptorIvars::new(key, ascending, selector_or_compare(selector)))
        }

        #[unsafe(method_id(sortDescriptorWithKey:ascending:comparator:))]
        fn with_key_comparator(key: Option<&NSString>, ascending: bool, block: NSComparator) -> Retained<NSSortDescriptor> {
            make(DescriptorIvars::new(key, ascending, comparator(block)))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init(this, DescriptorIvars::default())
        }

        #[unsafe(method_id(initWithKey:ascending:))]
        fn init_with_key(this: Allocated<Self>, key: Option<&NSString>, ascending: bool) -> Retained<Self> {
            init(this, DescriptorIvars::new(key, ascending, How::Selector(sel!(compare:))))
        }

        #[unsafe(method_id(initWithKey:ascending:selector:))]
        fn init_with_key_selector(this: Allocated<Self>, key: Option<&NSString>, ascending: bool, selector: Option<Sel>) -> Retained<Self> {
            init(this, DescriptorIvars::new(key, ascending, selector_or_compare(selector)))
        }

        #[unsafe(method_id(initWithKey:ascending:comparator:))]
        fn init_with_key_comparator(this: Allocated<Self>, key: Option<&NSString>, ascending: bool, block: NSComparator) -> Retained<Self> {
            init(this, DescriptorIvars::new(key, ascending, comparator(block)))
        }

        #[unsafe(method_id(key))]
        fn key(&self) -> Option<Retained<NSString>> {
            self.ivars().key.clone()
        }

        #[unsafe(method(ascending))]
        fn ascending(&self) -> bool {
            self.ivars().ascending
        }

        #[unsafe(method(selector))]
        fn selector(&self) -> Option<Sel> {
            match self.ivars().how {
                How::Selector(sel) => Some(sel),
                _ => None,
            }
        }

        /// The block, which the descriptor keeps alive; null for a
        /// descriptor comparing by selector.
        #[unsafe(method(comparator))]
        fn comparator(&self) -> NSComparator {
            match &self.ivars().how {
                How::Block(block) => ptr::from_ref::<DynBlock<_>>(block).cast_mut().cast(),
                _ => ptr::null_mut(),
            }
        }

        /// Sidestep's descriptors always evaluate.
        #[unsafe(method(allowEvaluation))]
        fn allow_evaluation(&self) {}

        #[unsafe(method(compareObject:toObject:))]
        fn compare_object(&self, a: &AnyObject, b: &AnyObject) -> NSInteger {
            let ivars = self.ivars();
            let mut found = [None; 4];
            let (x, y) = (ivars.value(a, &mut found), ivars.value(b, &mut found));
            ivars.compare(x.as_deref(), y.as_deref()) as NSInteger
        }

        #[unsafe(method_id(reversedSortDescriptor))]
        fn reversed_sort_descriptor(&self) -> Retained<AnyObject> {
            let mut ivars = self.ivars().clone();
            ivars.ascending = !ivars.ascending;
            util::upcast(make(ivars))
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<AnyObject> {
            // Immutable: a copy is the same object.
            util::upcast(self.retain())
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| equal_descriptors(self, other))
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            let ivars = self.ivars();
            let key: NSUInteger = ivars.key.as_ref().map_or(0, |k| k.hash());
            key ^ NSUInteger::from(ivars.ascending)
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let ivars = self.ivars();
            let key = ivars.key.as_ref().map(|k| k.to_string()).unwrap_or_default();
            let direction = if ivars.ascending { "ascending" } else { "descending" };
            let how = match &ivars.how {
                How::Selector(sel) => sel.name().to_string_lossy().into_owned(),
                How::Block(block) => format!("BLOCK({:p})", ptr::from_ref::<DynBlock<_>>(block).cast::<c_void>()),
                How::Nothing => "(null)".to_owned(),
            };
            NSString::from_str(&format!("({key}, {direction}, NO, {how})"))
        }
    }

    unsafe impl NSObjectProtocol for NSSortDescriptorImpl {}
);
