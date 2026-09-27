//! Toll-free bridging's shared parts: `CFGetTypeID` and each bridged
//! type's type ID, `CFCopyDescription`, and the allocator constants.
//!
//! CoreFoundation objects are Foundation's objects here, so a type ID is
//! worked out from the object's class: the first class on its superclass
//! chain that has a CoreFoundation counterpart. Classes are compared by
//! name, which needs no link-time reference to classes (such as the
//! collections) that may not be built in. The ID values are Sidestep's
//! own; only their equality means anything.
//!
//! Allocators are ignored (everything is allocated the Objective-C way),
//! so the allocator constants are distinct immortal objects that nothing
//! looks inside.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{ClassType, define_class, msg_send};
use objc2_foundation::NSString;
use sidestep_runtime::{ObjectRef, StaticObject};

pub(crate) type CFTypeID = usize;

/// The type IDs, by kind.
pub(crate) mod id {
    use super::CFTypeID;
    pub(crate) const TYPE: CFTypeID = 1;
    pub(crate) const ALLOCATOR: CFTypeID = 2;
    pub(crate) const STRING: CFTypeID = 7;
    pub(crate) const DICTIONARY: CFTypeID = 18;
    pub(crate) const ARRAY: CFTypeID = 19;
    pub(crate) const DATA: CFTypeID = 20;
    pub(crate) const BOOLEAN: CFTypeID = 21;
    pub(crate) const NUMBER: CFTypeID = 22;
    pub(crate) const NULL: CFTypeID = 23;
    pub(crate) const SET: CFTypeID = 24;
    pub(crate) const URL: CFTypeID = 29;
    pub(crate) const ERROR: CFTypeID = 30;
    pub(crate) const ATTRIBUTED_STRING: CFTypeID = 31;
    pub(crate) const CHARACTER_SET: CFTypeID = 32;
    pub(crate) const DATE: CFTypeID = 42;
    pub(crate) const RUN_LOOP: CFTypeID = 43;
    pub(crate) const RUN_LOOP_TIMER: CFTypeID = 44;
    pub(crate) const RUN_LOOP_OBSERVER: CFTypeID = 45;
    pub(crate) const LOCALE: CFTypeID = 46;
    pub(crate) const TIME_ZONE: CFTypeID = 47;
    pub(crate) const RUN_LOOP_SOURCE: CFTypeID = 48;
    pub(crate) const CALENDAR: CFTypeID = 49;
    pub(crate) const BUNDLE: CFTypeID = 50;
    pub(crate) const READ_STREAM: CFTypeID = 51;
    pub(crate) const WRITE_STREAM: CFTypeID = 52;
    pub(crate) const FILE_SECURITY: CFTypeID = 53;
    pub(crate) const MACH_PORT: CFTypeID = 54;
    pub(crate) const MESSAGE_PORT: CFTypeID = 55;
    pub(crate) const PLUG_IN: CFTypeID = 56;
    // CoreGraphics' types, whose classes Sidestep's AppKit defines.
    pub const CG_COLOR_SPACE: CFTypeID = 101;
    pub const CG_COLOR: CFTypeID = 102;
    pub const CG_PATH: CFTypeID = 103;
    pub const CG_CONTEXT: CFTypeID = 104;
    pub const CG_IMAGE: CFTypeID = 105;
    pub const CG_GRADIENT: CFTypeID = 106;
    pub const CG_DATA_PROVIDER: CFTypeID = 107;
    pub const CG_DATA_CONSUMER: CFTypeID = 108;
    pub const CG_FONT: CFTypeID = 109;
    pub const CG_SHADING: CFTypeID = 110;
    pub const CG_FUNCTION: CFTypeID = 111;
    pub const CG_PATTERN: CFTypeID = 112;
    // ImageIO's, which Sidestep's AppKit defines too.
    pub const CG_IMAGE_SOURCE: CFTypeID = 113;
    pub const CG_IMAGE_DESTINATION: CFTypeID = 114;
    // CoreText's, likewise (a font is an `NSFont`, a font descriptor an
    // `NSFontDescriptor`, as on macOS).
    pub const CT_FONT: CFTypeID = 120;
    pub const CT_FONT_DESCRIPTOR: CFTypeID = 121;
    pub const CT_LINE: CFTypeID = 122;
    pub const CT_RUN: CFTypeID = 123;
    pub const CT_TYPESETTER: CFTypeID = 124;
    pub const CT_FRAMESETTER: CFTypeID = 125;
    pub const CT_FRAME: CFTypeID = 126;
    pub const CT_PARAGRAPH_STYLE: CFTypeID = 127;
    pub const CT_FONT_COLLECTION: CFTypeID = 128;
    pub const CT_GLYPH_INFO: CFTypeID = 129;
    pub const CT_RUN_DELEGATE: CFTypeID = 130;
    pub const CT_TEXT_TAB: CFTypeID = 131;
    pub const CT_RUBY_ANNOTATION: CFTypeID = 132;
}

/// CoreGraphics', ImageIO's and CoreText's types: their classes (defined
/// by Sidestep's AppKit, where they live: Sidestep-private names, and
/// `NSFont` and `NSFontDescriptor`, which CoreText's fonts and descriptors
/// are) and the names `CFCopyTypeIDDescription` gives them.
const FRAMEWORK_TYPES: &[(&str, CFTypeID, &str)] = &[
    ("_SidestepCGColorSpace", id::CG_COLOR_SPACE, "CGColorSpace"),
    ("_SidestepCGColor", id::CG_COLOR, "CGColor"),
    ("_SidestepCGPath", id::CG_PATH, "CGPath"),
    ("_SidestepCGContext", id::CG_CONTEXT, "CGContext"),
    ("_SidestepCGImage", id::CG_IMAGE, "CGImage"),
    ("_SidestepCGGradient", id::CG_GRADIENT, "CGGradient"),
    ("_SidestepCGDataProvider", id::CG_DATA_PROVIDER, "CGDataProvider"),
    ("_SidestepCGDataConsumer", id::CG_DATA_CONSUMER, "CGDataConsumer"),
    ("_SidestepCGFont", id::CG_FONT, "CGFont"),
    ("_SidestepCGShading", id::CG_SHADING, "CGShading"),
    ("_SidestepCGFunction", id::CG_FUNCTION, "CGFunction"),
    ("_SidestepCGPattern", id::CG_PATTERN, "CGPattern"),
    ("_SidestepCGImageSource", id::CG_IMAGE_SOURCE, "CGImageSource"),
    ("_SidestepCGImageDestination", id::CG_IMAGE_DESTINATION, "CGImageDestination"),
    ("NSFont", id::CT_FONT, "CTFont"),
    ("NSFontDescriptor", id::CT_FONT_DESCRIPTOR, "CTFontDescriptor"),
    ("_SidestepCTLine", id::CT_LINE, "CTLine"),
    ("_SidestepCTRun", id::CT_RUN, "CTRun"),
    ("_SidestepCTTypesetter", id::CT_TYPESETTER, "CTTypesetter"),
    ("_SidestepCTFramesetter", id::CT_FRAMESETTER, "CTFramesetter"),
    ("_SidestepCTFrame", id::CT_FRAME, "CTFrame"),
    ("_SidestepCTParagraphStyle", id::CT_PARAGRAPH_STYLE, "CTParagraphStyle"),
    ("_SidestepCTFontCollection", id::CT_FONT_COLLECTION, "CTFontCollection"),
    ("_SidestepCTGlyphInfo", id::CT_GLYPH_INFO, "CTGlyphInfo"),
    ("_SidestepCTRunDelegate", id::CT_RUN_DELEGATE, "CTRunDelegate"),
    ("_SidestepCTTextTab", id::CT_TEXT_TAB, "CTTextTab"),
    ("_SidestepCTRubyAnnotation", id::CT_RUBY_ANNOTATION, "CTRubyAnnotation"),
];

/// Class names with CoreFoundation counterparts.
const BRIDGED: &[(&str, CFTypeID)] = &[
    ("NSString", id::STRING),
    ("NSAttributedString", id::ATTRIBUTED_STRING),
    ("NSDictionary", id::DICTIONARY),
    ("NSArray", id::ARRAY),
    ("NSSet", id::SET),
    ("NSData", id::DATA),
    ("NSNumber", id::NUMBER),
    ("NSNull", id::NULL),
    ("NSURL", id::URL),
    ("NSError", id::ERROR),
    ("NSDate", id::DATE),
    ("NSCharacterSet", id::CHARACTER_SET),
    ("NSLocale", id::LOCALE),
    ("NSTimeZone", id::TIME_ZONE),
    ("NSTimer", id::RUN_LOOP_TIMER),
    ("_SidestepCFRunLoop", id::RUN_LOOP),
    ("NSBundle", id::BUNDLE),
    ("_SidestepCFCalendar", id::CALENDAR),
    ("_SidestepCFReadStream", id::READ_STREAM),
    ("_SidestepCFWriteStream", id::WRITE_STREAM),
    ("_SidestepCFFileSecurity", id::FILE_SECURITY),
    ("_SidestepRunLoopObserver", id::RUN_LOOP_OBSERVER),
    ("_SidestepCFAllocator", id::ALLOCATOR),
];

/// The type ID of an object's class.
pub(crate) fn type_of(object: &AnyObject) -> CFTypeID {
    let mut class: Option<&AnyClass> = Some(object.class());
    while let Some(c) = class {
        let name = c.name().to_bytes();
        if let Some(&(_, id)) = BRIDGED.iter().find(|(n, _)| n.as_bytes() == name) {
            if id == id::NUMBER && is_boolean(object) {
                return id::BOOLEAN;
            }
            return id;
        }
        if let Some(&(_, id, _)) = FRAMEWORK_TYPES.iter().find(|(n, ..)| n.as_bytes() == name) {
            return id;
        }
        class = c.superclass();
    }
    id::TYPE
}

/// A boolean: one of the two constants `+numberWithBool:` hands out
/// (`kCFBooleanTrue` and `kCFBooleanFalse`). A number made from a `char`
/// is a number, as on macOS.
pub(crate) fn is_boolean(number: &AnyObject) -> bool {
    crate::number::is_boolean(number)
}

/// # Safety
///
/// `cf` is a live object.
pub(crate) unsafe fn object<'a>(cf: *const c_void) -> &'a AnyObject {
    // SAFETY: guaranteed by the caller.
    unsafe { &*cf.cast::<AnyObject>() }
}

/// A +1 reference, as the Create and Copy functions return.
pub(crate) fn owned<T: objc2::Message>(object: Retained<T>) -> *mut c_void {
    Retained::into_raw(object).cast()
}

/// Keep `value` alive as long as `owner`, for the Get functions, whose
/// results the caller doesn't own.
pub(crate) fn owned_by(owner: &AnyObject, key: &'static u8, value: Retained<AnyObject>) -> *const c_void {
    let ptr = Retained::as_ptr(&value).cast::<c_void>();
    // SAFETY: retaining associations keep `value` for `owner`'s lifetime;
    // the key is a static address.
    unsafe {
        objc2::ffi::objc_setAssociatedObject(
            (owner as *const AnyObject).cast_mut(),
            (key as *const u8).cast(),
            Retained::as_ptr(&value).cast_mut(),
            objc2::ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        )
    };
    ptr
}

/// Keep a value alive as long as `owner`, once per name, for the Get
/// functions of objects whose values don't change: the first one kept is
/// the one handed out again. They are kept in a dictionary associated with
/// the owner. `make` runs without the lock (it may be any code, even code
/// that comes back here); if another thread keeps a value for the name
/// meanwhile, that one is handed out and this one dropped.
pub(crate) fn keep_named(
    owner: &AnyObject,
    name: &str,
    make: impl FnOnce() -> Option<Retained<AnyObject>>,
) -> *const c_void {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static KEY: u8 = 0;
    type Kept = objc2_foundation::NSMutableDictionary<NSString, AnyObject>;
    let name = NSString::from_str(name);
    let key = (&KEY as *const u8).cast();
    let kept = {
        let _guard = crate::thread::lock(&LOCK);
        // SAFETY: a live object and a static key.
        let found = unsafe { objc2::ffi::objc_getAssociatedObject(owner, key) };
        if found.is_null() {
            let kept = Kept::new();
            // SAFETY: a retaining association keeps the dictionary for the
            // owner's life.
            unsafe {
                objc2::ffi::objc_setAssociatedObject(
                    (owner as *const AnyObject).cast_mut(),
                    key,
                    Retained::as_ptr(&kept).cast_mut().cast(),
                    objc2::ffi::OBJC_ASSOCIATION_RETAIN,
                )
            };
            kept
        } else {
            // SAFETY: only this function associates objects under the key.
            unsafe { Retained::retain(found.cast::<Kept>().cast_mut()) }.expect("non-null")
        }
    };
    // Looked up and set under the lock, as other threads change the
    // dictionary; a value found stays in it, so its pointer outlives the
    // `Retained` let go of here.
    let lookup = |kept: &Kept| {
        let _guard = crate::thread::lock(&LOCK);
        kept.objectForKey(&name).map(|value| Retained::as_ptr(&value).cast::<c_void>())
    };
    if let Some(found) = lookup(&kept) {
        return found;
    }
    let Some(value) = make() else { return std::ptr::null() };
    let kept_now = {
        let _guard = crate::thread::lock(&LOCK);
        match kept.objectForKey(&name) {
            Some(first) => Retained::as_ptr(&first).cast(),
            None => {
                // SAFETY: a string key and an object value.
                unsafe { kept.setObject_forKey(&value, objc2::runtime::ProtocolObject::from_ref(&*name)) };
                Retained::as_ptr(&value).cast()
            }
        }
    };
    // Ours, if another's was kept first, goes after the lock.
    drop(value);
    kept_now
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFGetTypeID(cf: *const c_void) -> CFTypeID {
    if cf.is_null() {
        return 0;
    }
    // SAFETY: the caller passes a live object.
    type_of(unsafe { object(cf) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCopyDescription(cf: *const c_void) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller passes a live object; -description returns a string.
    let text: Retained<NSString> = unsafe { msg_send![object(cf), description] };
    owned(text)
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCopyTypeIDDescription(type_id: CFTypeID) -> *mut c_void {
    let name = match type_id {
        id::TYPE => "CFType",
        id::ALLOCATOR => "CFAllocator",
        id::BOOLEAN => "CFBoolean",
        id::RUN_LOOP => "CFRunLoop",
        id::RUN_LOOP_TIMER => "CFRunLoopTimer",
        id::RUN_LOOP_OBSERVER => "CFRunLoopObserver",
        id::RUN_LOOP_SOURCE => "CFRunLoopSource",
        id::CALENDAR => "CFCalendar",
        id::READ_STREAM => "CFReadStream",
        id::WRITE_STREAM => "CFWriteStream",
        id::FILE_SECURITY => "CFFileSecurity",
        id::MACH_PORT => "CFMachPort",
        id::MESSAGE_PORT => "CFMessagePort",
        id::PLUG_IN => "CFPlugIn",
        id if FRAMEWORK_TYPES.iter().any(|t| t.1 == id) => {
            let name = FRAMEWORK_TYPES.iter().find(|t| t.1 == id).map_or("", |t| t.2);
            return owned(NSString::from_str(name));
        }
        _ => BRIDGED.iter().find(|(_, id)| *id == type_id).map_or("", |(n, _)| n.strip_prefix("NS").unwrap_or(n)),
    };
    let name = if name.starts_with("CF") || name.is_empty() { name.to_string() } else { format!("CF{name}") };
    owned(NSString::from_str(&name))
}

/// Print a description to stderr: a string's text, other objects'
/// descriptions, with characters past ASCII as `\u` escapes of their
/// UTF-16 units, as CoreFoundation prints them.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFShow(cf: *const c_void) {
    if cf.is_null() {
        eprintln!("(null)");
        return;
    }
    // SAFETY: the caller passes a live object.
    let object = unsafe { object(cf) };
    let text = match object.downcast_ref::<NSString>() {
        Some(string) => string.to_string(),
        // SAFETY: -description returns a string.
        None => {
            let description: Option<Retained<NSString>> = unsafe { msg_send![object, description] };
            description.map_or(String::new(), |d| d.to_string())
        }
    };
    let mut out = String::with_capacity(text.len());
    for unit in text.encode_utf16() {
        match char::from_u32(u32::from(unit)) {
            Some(c) if c.is_ascii() => out.push(c),
            _ => out.push_str(&format!("\\u{unit:04x}")),
        }
    }
    eprintln!("{out}");
}

/// Print a string's particulars to stdout, as CoreFoundation's debugging
/// aid does: its length, whether its text fits in eight bits, whether it
/// can change, and where it is.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFShowStr(cf: *const c_void) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    if cf.is_null() {
        let _ = writeln!(out, "(null)");
        return;
    }
    // SAFETY: the caller passes a string.
    let string = unsafe { object(cf) };
    let text = super::string::text(string);
    let length: usize = text.encode_utf16().count();
    let eight_bit = text.is_ascii();
    // SAFETY: strings answer -isKindOfClass:.
    let mutable: bool = unsafe { msg_send![string, isKindOfClass: objc2_foundation::NSMutableString::class()] };
    let _ = writeln!(out);
    let _ = writeln!(out, "Length {length}");
    let _ = writeln!(out, "IsEightBit {}", u8::from(eight_bit));
    let _ = writeln!(out, "HasLengthByte 0");
    let _ = writeln!(out, "HasNullByte {}", u8::from(eight_bit));
    let _ = writeln!(out, "InlineContents {}", u8::from(!mutable && length > 0));
    let _ = writeln!(out, "Allocator SystemDefault");
    let _ = writeln!(out, "Mutable {}", u8::from(mutable));
    if mutable {
        let _ = writeln!(out, "CurrentCapacity {length}");
        let _ = writeln!(out, "DesiredCapacity {length}");
    }
    let _ = writeln!(out, "Contents {cf:p}");
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFGetAllocator(_cf: *const c_void) -> *const c_void {
    std::ptr::null()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFAllocatorGetTypeID() -> CFTypeID {
    id::ALLOCATOR
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFAllocatorGetDefault() -> *const c_void {
    std::ptr::null()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFNullGetTypeID() -> CFTypeID {
    id::NULL
}

sidestep_runtime::static_class!(pub(crate) CF_ALLOCATOR_CLASS, CF_ALLOCATOR_META = "_SidestepCFAllocator", || {
    let _ = AllocatorImpl::class();
});

define_class!(
    /// The class of the allocator constants.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCFAllocator"]
    struct AllocatorImpl;
);

static SYSTEM_DEFAULT: StaticObject<()> = StaticObject::new(&CF_ALLOCATOR_CLASS, ());
static MALLOC: StaticObject<()> = StaticObject::new(&CF_ALLOCATOR_CLASS, ());
static MALLOC_ZONE: StaticObject<()> = StaticObject::new(&CF_ALLOCATOR_CLASS, ());
static NULL_ALLOCATOR: StaticObject<()> = StaticObject::new(&CF_ALLOCATOR_CLASS, ());
static USE_CONTEXT: StaticObject<()> = StaticObject::new(&CF_ALLOCATOR_CLASS, ());

/// `NULL`: the default allocator.
#[unsafe(no_mangle)]
pub static kCFAllocatorDefault: usize = 0;
#[unsafe(no_mangle)]
pub static kCFAllocatorSystemDefault: ObjectRef = SYSTEM_DEFAULT.object_ref();
#[unsafe(no_mangle)]
pub static kCFAllocatorMalloc: ObjectRef = MALLOC.object_ref();
#[unsafe(no_mangle)]
pub static kCFAllocatorMallocZone: ObjectRef = MALLOC_ZONE.object_ref();
#[unsafe(no_mangle)]
pub static kCFAllocatorNull: ObjectRef = NULL_ALLOCATOR.object_ref();
#[unsafe(no_mangle)]
pub static kCFAllocatorUseContext: ObjectRef = USE_CONTEXT.object_ref();

/// `kCFAllocatorSystemDefault`'s address, which descriptions show.
pub(crate) fn system_default_allocator() -> *const c_void {
    SYSTEM_DEFAULT.as_object().cast_const().cast()
}

/// Whether an allocator is `kCFAllocatorNull`, which frees nothing.
pub(crate) fn is_null_allocator(allocator: *const c_void) -> bool {
    std::ptr::eq(allocator.cast::<u8>(), NULL_ALLOCATOR.as_object().cast_const().cast::<u8>())
}
