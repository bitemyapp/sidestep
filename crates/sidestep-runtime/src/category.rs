//! Categories linked into the program: methods a framework crate adds to a
//! class defined elsewhere, such as Foundation's forwarding methods on
//! `NSObject` or AppKit's drawing methods on `NSString`.
//!
//! [`category!`](crate::category) puts an entry in the
//! `sidestep_categories` linker section, a sibling of `sidestep_classes`:
//! the name of the class, the category's name, and a function that adds
//! the methods through a [`Category`]. The runtime runs a class's
//! categories when the class is registered (`objc_registerClassPair`),
//! before it is marked loaded. Framework classes register the first time
//! anything uses them and other threads wait for that, so no message,
//! lookup or introspection can reach a class before its categories are
//! there, whichever class a program touches first; categories need no
//! constructor and cost nothing until their class is used.
//!
//! As on Apple's runtime, a category's method replaces a method of the
//! same name the class defines itself. Two categories adding the same
//! selector to one class is a mistake whose outcome depends on link order,
//! so debug builds panic on it; debug builds also check that a category's
//! method has the encoding of the method it overrides or replaces, as
//! objc2 does for subclasses.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fmt::Write;
use std::sync::LazyLock;

use objc2::encode::{Encode, EncodeArguments, EncodeReturn};
use objc2::runtime::{AnyClass, AnyObject, MethodImplementation};

use crate::Imp;
use crate::class::Class;
#[cfg(debug_assertions)]
use crate::class::find_method;
use crate::selector::Sel;

/// One entry of the program's list of categories. Not for direct use; see
/// [`category!`](crate::category).
#[doc(hidden)]
#[repr(C)]
pub struct LinkedCategory {
    /// The name of the class the category adds to.
    pub class: &'static str,
    /// The category's own name, for diagnostics.
    pub name: &'static str,
    pub attach: fn(&mut Category),
}

/// Every category linked into the program, read like the class list (see
/// `class::linked`): the linker defines `__start_sidestep_categories` and
/// `__stop_sidestep_categories` around the section.
fn linked() -> &'static [LinkedCategory] {
    unsafe extern "Rust" {
        #[link_name = "__start_sidestep_categories"]
        static START: LinkedCategory;
        #[link_name = "__stop_sidestep_categories"]
        static STOP: LinkedCategory;
    }
    let (start, stop) = (&raw const START, &raw const STOP);
    let len = (stop.addr() - start.addr()) / size_of::<LinkedCategory>();
    // SAFETY: the linker places the section's contents, entries written by
    // `category!` (all of this one type, whose size is a multiple of its
    // alignment), contiguously between the two symbols. The runtime's own
    // entry below guarantees the section exists.
    unsafe { std::slice::from_raw_parts(start, len) }
}

// The section needs an entry for its bounds to be defined in a program
// that links no framework: this one, which names no class.
crate::category!(""(SidestepNoCategory), |_| {});

/// Categories by the name of their class.
static BY_CLASS: LazyLock<HashMap<&'static [u8], Vec<&'static LinkedCategory>>> = LazyLock::new(|| {
    let mut map: HashMap<&'static [u8], Vec<&'static LinkedCategory>> = HashMap::new();
    for entry in linked().iter().filter(|e| !e.class.is_empty()) {
        map.entry(entry.class.as_bytes()).or_default().push(entry);
    }
    map
});

/// Run the categories of `cls`, a class being registered. Called holding
/// the class-loading lock, before the class is marked loaded.
pub(crate) fn attach(cls: &'static Class) {
    let Some(entries) = BY_CLASS.get(cls.name().to_bytes()) else { return };
    for entry in entries {
        (entry.attach)(&mut Category { class: cls, name: entry.name });
    }
}

/// A category being attached to its class: what its function adds methods
/// through.
pub struct Category {
    class: &'static Class,
    name: &'static str,
}

impl Category {
    /// The class the category adds to. It is being registered: its own
    /// methods and its superclass are in place, but it must not be sent
    /// messages yet.
    pub fn class(&self) -> &'static AnyClass {
        // SAFETY: the runtime's classes are objc2's `AnyClass`.
        unsafe { &*(self.class as *const Class).cast::<AnyClass>() }
    }

    /// The category's name, as `category!` gave it.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Add an instance method, with the encoding objc2 gives `func`'s
    /// types, as `ClassBuilder::add_method` does.
    ///
    /// # Safety
    /// `func` must be callable as the method `sel` names on instances of
    /// the class, with the receiver as its first argument.
    pub unsafe fn add_method<F: MethodImplementation>(&mut self, sel: objc2::runtime::Sel, func: F) {
        let types = encoding::<F>(sel);
        self.add(self.class, raw(sel), imp_of(func), &types);
    }

    /// Add a class method.
    ///
    /// # Safety
    /// As for [`add_method`](Self::add_method), with the class object as
    /// the receiver.
    pub unsafe fn add_class_method<F: MethodImplementation<Callee = AnyClass>>(
        &mut self,
        sel: objc2::runtime::Sel,
        func: F,
    ) {
        let types = encoding::<F>(sel);
        self.add(self.class.metaclass(), raw(sel), imp_of(func), &types);
    }

    /// Add every method `helper` defines itself (not those it inherits),
    /// instance methods as instance methods and class methods as class
    /// methods, with their encodings. This lets a category be written as a
    /// `define_class!` type with no instance variables, a subclass of the
    /// class's superclass or of `NSObject`, whose methods treat their
    /// receiver as an instance of the class.
    ///
    /// # Safety
    /// Each of `helper`'s methods must be callable with a receiver of the
    /// category's class (or the class object, for class methods).
    pub unsafe fn add_methods_of(&mut self, helper: &AnyClass) {
        // SAFETY: objc2's classes are the runtime's.
        let helper = unsafe { &*(helper as *const AnyClass).cast::<Class>() };
        for (from, to) in [(helper, self.class), (helper.metaclass(), self.class.metaclass())] {
            let methods: Vec<_> = from.rt().methods.read().unwrap().order.clone();
            for method in methods {
                // SAFETY: methods are never freed.
                let method = unsafe { method.get() };
                self.add(to, method.sel(), method.imp(), method.types());
            }
        }
    }

    fn add(&self, target: &'static Class, sel: Sel, imp: Imp, types: &CStr) {
        #[cfg(debug_assertions)]
        self.check(target, sel, types);
        let own = target.rt().methods.read().unwrap().by_sel.get(&(sel as usize)).copied();
        match own {
            // SAFETY: methods are never freed; the implementation replaces
            // one of the same encoding.
            Some(method) => unsafe {
                crate::method::method_setImplementation(method.0, imp);
            },
            None => {
                crate::method::add_method(target, sel, imp, types);
            }
        }
    }

    /// Debug builds: the selector isn't added to this class by another
    /// category, and the method has the encoding of any it overrides.
    #[cfg(debug_assertions)]
    fn check(&self, target: &'static Class, sel: Sel, types: &CStr) {
        claim(target, sel, self.name);
        if let Some(existing) = find_method(target, sel)
            && !existing.types().is_empty()
            && !types.is_empty()
            && existing.types() != types
        {
            // SAFETY: a selector from the runtime.
            let name = unsafe { crate::selector::name(sel) };
            panic!(
                "sidestep: category {} gives {}[{} {}] the encoding {types:?}, where the method it replaces has {:?}",
                self.name,
                if target.is_meta() { '+' } else { '-' },
                target.instance_class().name().to_string_lossy(),
                name.to_string_lossy(),
                existing.types(),
            );
        }
    }
}

/// Debug builds: record that category `name` adds `sel` to `target`, and
/// panic if another category already did.
#[cfg(debug_assertions)]
fn claim(target: &'static Class, sel: Sel, name: &'static str) {
    use std::sync::Mutex;
    static CLAIMED: Mutex<Option<HashMap<(usize, usize), &'static str>>> = Mutex::new(None);
    let mut claimed = crate::util::lock(&CLAIMED);
    let key = (target as *const Class as usize, sel as usize);
    if let Some(other) = claimed.get_or_insert_default().insert(key, name)
        && other != name
    {
        drop(claimed);
        // SAFETY: a selector from the runtime.
        let sel = unsafe { crate::selector::name(sel) };
        panic!(
            "sidestep: categories {other} and {name} both add {}[{} {}]",
            if target.is_meta() { '+' } else { '-' },
            target.instance_class().name().to_string_lossy(),
            sel.to_string_lossy(),
        );
    }
}

/// The type encoding objc2 gives a method implemented by `F`, as
/// `ClassBuilder` writes it: the return type, the receiver, the selector,
/// then the arguments.
fn encoding<F: MethodImplementation>(sel: objc2::runtime::Sel) -> CString {
    let args = F::Arguments::ENCODINGS;
    debug_assert_eq!(
        sel.name().to_bytes().iter().filter(|&&b| b == b':').count(),
        args.len(),
        "sidestep: selector {sel} doesn't take as many arguments as its implementation"
    );
    let mut types =
        format!("{}{}{}", F::Return::ENCODING_RETURN, <*mut AnyObject>::ENCODING, objc2::runtime::Sel::ENCODING);
    for arg in args {
        write!(types, "{arg}").expect("formatting to a string");
    }
    CString::new(types).expect("encodings have no NULs")
}

/// objc2's `Sel` is a transparent wrapper around the runtime's selector
/// pointer.
fn raw(sel: objc2::runtime::Sel) -> Sel {
    // SAFETY: same representation.
    unsafe { std::mem::transmute::<objc2::runtime::Sel, Sel>(sel) }
}

/// The implementation `func` is: `MethodImplementation` is implemented
/// only for function pointers.
fn imp_of<F: MethodImplementation>(func: F) -> Imp {
    const { assert!(size_of::<F>() == size_of::<Imp>()) };
    // SAFETY: a function pointer of the same size, only ever called with
    // the method's real signature.
    unsafe { std::mem::transmute_copy::<F, Imp>(&func) }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, Sel};
    use objc2::{ClassType, msg_send, sel};

    extern "C-unwind" fn doubled(_: &AnyObject, _: Sel, x: i64) -> i64 {
        x * 2
    }

    extern "C-unwind" fn class_answer(_: &AnyClass, _: Sel) -> i64 {
        42
    }

    extern "C-unwind" fn replaced(_: &AnyObject, _: Sel) -> i64 {
        2
    }

    extern "C-unwind" fn original(_: &AnyObject, _: Sel) -> i64 {
        1
    }

    static ATTACHED: AtomicUsize = AtomicUsize::new(0);

    crate::category!("SidestepUnitCategoryTarget"(SidestepUnitAdditions), |category| {
        ATTACHED.fetch_add(1, Ordering::SeqCst);
        assert_eq!(category.class().name(), c"SidestepUnitCategoryTarget");
        // SAFETY: the signatures match the selectors.
        unsafe {
            category.add_method(sel!(sidestepDoubled:), doubled as extern "C-unwind" fn(_, _, _) -> _);
            category.add_class_method(sel!(sidestepClassAnswer), class_answer as extern "C-unwind" fn(_, _) -> _);
            category.add_method(sel!(sidestepWhich), replaced as extern "C-unwind" fn(_, _) -> _);
        }
    });

    /// A category's methods are there as soon as its class is registered,
    /// and replace the class's own.
    #[test]
    fn categories_attach_when_their_class_registers() {
        assert_eq!(ATTACHED.load(Ordering::SeqCst), 0);
        let mut builder = ClassBuilder::new(c"SidestepUnitCategoryTarget", NSObject::class()).unwrap();
        // SAFETY: the signature matches the selector.
        unsafe { builder.add_method(sel!(sidestepWhich), original as extern "C-unwind" fn(_, _) -> _) };
        let cls = builder.register();
        assert_eq!(ATTACHED.load(Ordering::SeqCst), 1);
        assert!(cls.instance_method(sel!(sidestepDoubled:)).is_some());
        let obj: objc2::rc::Retained<NSObject> = unsafe { msg_send![cls, new] };
        let got: i64 = unsafe { msg_send![&*obj, sidestepDoubled: 21i64] };
        assert_eq!(got, 42);
        let got: i64 = unsafe { msg_send![cls, sidestepClassAnswer] };
        assert_eq!(got, 42);
        let got: i64 = unsafe { msg_send![&*obj, sidestepWhich] };
        assert_eq!(got, 2);
    }

    extern "C-unwind" fn helper_value(_: &AnyObject, _: Sel) -> i64 {
        7
    }

    extern "C-unwind" fn helper_class_value(_: &AnyClass, _: Sel) -> i64 {
        8
    }

    crate::category!("SidestepUnitAdoptTarget"(SidestepUnitAdopted), |category| {
        // A helper class holding the methods, as a `define_class!` type
        // would, defined when the category attaches.
        let mut helper = ClassBuilder::new(c"SidestepUnitAdoptHelper", NSObject::class()).unwrap();
        // SAFETY: the signatures match the selectors.
        unsafe {
            helper.add_method(sel!(sidestepHelperValue), helper_value as extern "C-unwind" fn(_, _) -> _);
            helper.add_class_method(
                sel!(sidestepHelperClassValue),
                helper_class_value as extern "C-unwind" fn(_, _) -> _,
            );
            category.add_methods_of(helper.register());
        }
    });

    /// A category can take its methods, instance and class, from a helper
    /// class.
    #[test]
    fn categories_adopt_a_helper_class_methods() {
        let cls = ClassBuilder::new(c"SidestepUnitAdoptTarget", NSObject::class()).unwrap().register();
        let obj: objc2::rc::Retained<NSObject> = unsafe { msg_send![cls, new] };
        let got: i64 = unsafe { msg_send![&*obj, sidestepHelperValue] };
        assert_eq!(got, 7);
        let got: i64 = unsafe { msg_send![cls, sidestepHelperClassValue] };
        assert_eq!(got, 8);
        // Only the helper's own methods: nothing it inherits.
        assert_eq!(cls.instance_methods().len(), 1);
        assert_eq!(cls.metaclass().instance_methods().len(), 1);
    }

    /// Two categories adding one selector to one class panic in debug
    /// builds.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "categories SidestepA and SidestepB both add -[NSObject sidestepTwice]")]
    fn duplicate_selectors_panic() {
        // SAFETY: objc2's classes are the runtime's.
        let cls = unsafe { &*(NSObject::class() as *const AnyClass).cast::<crate::class::Class>() };
        let sel = crate::selector::register(c"sidestepTwice");
        super::claim(cls, sel, "SidestepA");
        super::claim(cls, sel, "SidestepA");
        super::claim(cls, sel, "SidestepB");
    }
}
