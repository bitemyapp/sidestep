//! Run-loop modes. A mode is a string, compared by value; the loop works
//! with small integers instead, interned once per process, so that mode
//! sets are bit masks and matching a timer or block to the running mode
//! costs a mask test.
//!
//! `kCFRunLoopCommonModes` is not a mode a loop runs in but the name of a
//! per-loop set (initially just the default mode). Items added "to the
//! common modes" are copied into every mode of that set, and into modes
//! added to it later; [`ModeSet`] holds the expanded set and the item keeps
//! a separate `common` flag, which is how CoreFoundation behaves: removing
//! an item from the default mode leaves it in the other common modes.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::NSString;
use sidestep_runtime::ObjectRef;

use crate::thread::lock;
use crate::{ConstStr, ConstantString};

static DEFAULT_NAME: ConstantString =
    ConstantString::new(&crate::CONSTANT_STRING_CLASS, ConstStr::new("kCFRunLoopDefaultMode\0"));
static COMMON_NAME: ConstantString =
    ConstantString::new(&crate::CONSTANT_STRING_CLASS, ConstStr::new("kCFRunLoopCommonModes\0"));

// Foundation's and CoreFoundation's names are the same objects, as on macOS.
#[unsafe(no_mangle)]
pub static NSDefaultRunLoopMode: ObjectRef = DEFAULT_NAME.object_ref();
#[unsafe(no_mangle)]
pub static NSRunLoopCommonModes: ObjectRef = COMMON_NAME.object_ref();
#[unsafe(no_mangle)]
pub static kCFRunLoopDefaultMode: ObjectRef = DEFAULT_NAME.object_ref();
#[unsafe(no_mangle)]
pub static kCFRunLoopCommonModes: ObjectRef = COMMON_NAME.object_ref();

/// A constant string as the `NSString` it is.
pub(crate) fn constant(string: &'static ConstantString) -> &'static NSString {
    // SAFETY: constant strings are immortal instances of an NSString
    // subclass.
    unsafe { &*string.as_object().cast_const().cast::<NSString>() }
}

/// Export constant strings under Objective-C symbol names, each also
/// reachable from Rust as a `ConstantString` static.
macro_rules! exported_strings {
    ($($(#[$m:meta])* $symbol:ident, $vis:vis $object:ident = $value:literal;)*) => {$(
        $vis static $object: $crate::ConstantString =
            $crate::ConstantString::new(&$crate::CONSTANT_STRING_CLASS, $crate::ConstStr::new(concat!($value, "\0")));
        $(#[$m])*
        #[unsafe(no_mangle)]
        pub static $symbol: sidestep_runtime::ObjectRef = $object.object_ref();
    )*};
}
pub(crate) use exported_strings;

/// A run-loop mode, interned: equal names give equal modes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Mode(pub(crate) u32);

/// A mode's name, kept alive for the rest of the process.
struct Name(*const NSString);

// SAFETY: interned names are immutable strings that are never released.
unsafe impl Send for Name {}

struct Interner {
    ids: HashMap<Box<str>, u32>,
    names: Vec<Name>,
}

static INTERNER: LazyLock<Mutex<Interner>> = LazyLock::new(|| {
    let names =
        vec![Name(DEFAULT_NAME.as_object().cast_const().cast()), Name(COMMON_NAME.as_object().cast_const().cast())];
    let ids = HashMap::from([("kCFRunLoopDefaultMode".into(), 0), ("kCFRunLoopCommonModes".into(), 1)]);
    Mutex::new(Interner { ids, names })
});

impl Mode {
    /// `NSDefaultRunLoopMode`.
    pub const DEFAULT: Mode = Mode(0);
    /// `NSRunLoopCommonModes`: registering in it means every mode of the
    /// loop's common set. A loop never runs in it.
    pub const COMMON: Mode = Mode(1);

    /// The mode with this name.
    pub fn named(name: &str) -> Mode {
        let mut interner = lock(&INTERNER);
        if let Some(&id) = interner.ids.get(name) {
            return Mode(id);
        }
        let id = u32::try_from(interner.names.len()).expect("sidestep: too many run-loop modes");
        // Interned names live as long as the process, like the modes a
        // loop remembers.
        let object = Retained::into_raw(NSString::from_str(name));
        interner.names.push(Name(object));
        interner.ids.insert(name.into(), id);
        Mode(id)
    }

    /// The mode a string names.
    pub fn from_ns(name: &NSString) -> Mode {
        let ptr: *const NSString = name;
        if ptr == DEFAULT_NAME.as_object().cast_const().cast() {
            return Mode::DEFAULT;
        }
        if ptr == COMMON_NAME.as_object().cast_const().cast() {
            return Mode::COMMON;
        }
        match crate::string::fast_parts(name) {
            Some((text, _)) => Mode::named(text),
            None => Mode::named(&name.to_string()),
        }
    }

    /// The mode an Objective-C object names, if it is a string.
    pub(crate) fn from_object(object: &AnyObject) -> Option<Mode> {
        object.downcast_ref::<NSString>().map(Mode::from_ns)
    }

    /// The mode's name, which lives as long as the process.
    pub fn name(self) -> &'static NSString {
        let ptr = lock(&INTERNER).names[self.0 as usize].0;
        // SAFETY: interned names are never released.
        unsafe { &*ptr }
    }
}

/// A set of modes: a bit per mode below 64, a list for the rest (which only
/// programs with unusually many modes reach).
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct ModeSet {
    bits: u64,
    more: Vec<u32>,
}

impl ModeSet {
    pub(crate) fn of(mode: Mode) -> ModeSet {
        let mut set = ModeSet::default();
        set.insert(mode);
        set
    }

    pub(crate) fn insert(&mut self, mode: Mode) {
        match mode.0 {
            id @ 0..64 => self.bits |= 1 << id,
            id => {
                if !self.more.contains(&id) {
                    self.more.push(id);
                }
            }
        }
    }

    pub(crate) fn remove(&mut self, mode: Mode) {
        match mode.0 {
            id @ 0..64 => self.bits &= !(1 << id),
            id => self.more.retain(|&m| m != id),
        }
    }

    #[inline]
    pub(crate) fn contains(&self, mode: Mode) -> bool {
        match mode.0 {
            id @ 0..64 => self.bits & (1 << id) != 0,
            id => self.more.contains(&id),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.bits == 0 && self.more.is_empty()
    }

    pub(crate) fn extend(&mut self, other: &ModeSet) {
        self.bits |= other.bits;
        for &id in &other.more {
            self.insert(Mode(id));
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = Mode> + '_ {
        (0..64u32).filter(|id| self.bits & (1 << id) != 0).chain(self.more.iter().copied()).map(Mode)
    }

    pub(crate) fn subtract(&mut self, other: &ModeSet) {
        self.bits &= !other.bits;
        self.more.retain(|id| !other.more.contains(id));
    }
}

/// Where an item is registered: the modes it is in, and whether it was
/// added to the common modes (and so joins modes added to that set later).
#[derive(Clone, Default, Debug)]
pub(crate) struct Registration {
    pub(crate) modes: ModeSet,
    pub(crate) common: bool,
}

impl Registration {
    /// Add `mode`, expanding the common pseudo-mode with `common_set`.
    pub(crate) fn add(&mut self, mode: Mode, common_set: &ModeSet) {
        if mode == Mode::COMMON {
            self.common = true;
            self.modes.extend(common_set);
        } else {
            self.modes.insert(mode);
        }
    }

    /// Remove `mode`; removing the common pseudo-mode removes every mode of
    /// the common set.
    pub(crate) fn remove(&mut self, mode: Mode, common_set: &ModeSet) {
        if mode == Mode::COMMON {
            self.common = false;
            self.modes.subtract(common_set);
        } else {
            self.modes.remove(mode);
        }
    }

    pub(crate) fn contains(&self, mode: Mode) -> bool {
        if mode == Mode::COMMON { self.common } else { self.modes.contains(mode) }
    }
}
