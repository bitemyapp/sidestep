//! The font manager: font files and `CGFont`s registered for the process.
//! Each face is laid out as a family of its own (`text::fonts::register_data`)
//! and made findable by its PostScript and full names (`CTFontCreateWithName`,
//! `+[NSFont fontWithName:size:]`). Registering a file's faces a second time
//! fails with `kCTFontManagerErrorAlreadyRegistered`, and unregistering
//! faces that aren't with `kCTFontManagerErrorNotRegistered`; a `CGFont`
//! registered again succeeds, and unregistering one that isn't fails with
//! code -1 (measured on macOS). Descriptors made of a file's data don't
//! register it. Scopes other than the process's aren't kept apart, and
//! nothing is downloaded.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::DefinedClass;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSFontDescriptor;
use objc2_core_foundation::{
    CFArray, CFBundle, CFComparisonResult, CFData, CFError, CFIndex, CFRunLoopSource, CFString, CFURL,
};
use objc2_core_graphics::CGFont;
use objc2_core_text::{CTFontDescriptor, CTFontManagerAutoActivationSetting, CTFontManagerScope};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSMutableArray, NSString, NSURL};

use super::descriptor::descriptor;
use super::{ns_string, owned};
use crate::font::{self as nsfont};
use crate::text::fonts::{self, Design, Family, FontSpec};

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerCopyAvailableFontFamilyNames() -> Option<NonNull<CFArray>> {
    let names: Vec<Retained<NSString>> = fonts::family_names().iter().map(|n| NSString::from_str(n)).collect();
    Some(owned(NSArray::from_retained_slice(&names)))
}

/// Each family's regular face's PostScript name, and those of the fonts
/// registered by name.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerCopyAvailablePostScriptNames() -> Option<NonNull<CFArray>> {
    let mut names: Vec<String> = fonts::family_names()
        .iter()
        .map(|family| {
            let spec =
                FontSpec { family: Family::Named(family.as_str().into()), ..FontSpec::system(Design::Default, 12.0) };
            fonts::resolve(&spec).postscript_name.to_string()
        })
        .collect();
    names.extend(fonts::registered_names().into_iter().map(|(n, _)| n));
    names.sort();
    names.dedup();
    let names: Vec<Retained<NSString>> = names.iter().map(|n| NSString::from_str(n)).collect();
    Some(owned(NSArray::from_retained_slice(&names)))
}

/// The files the system's fonts come from.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerCopyAvailableFontURLs() -> Option<NonNull<CFArray>> {
    let paths: Vec<String> = crate::text::with_ctx(|ctx| {
        let ids: Vec<_> = ctx.fcx.collection.family_names().map(String::from).collect();
        let mut paths = Vec::new();
        for name in ids {
            let Some(family) = ctx.fcx.collection.family_by_name(&name) else { continue };
            for font in family.fonts() {
                if let parley::fontique::SourceKind::Path(path) = &font.source().kind {
                    paths.push(path.to_string_lossy().into_owned());
                }
            }
        }
        paths
    });
    let mut paths = paths;
    paths.sort();
    paths.dedup();
    let urls: Vec<Retained<NSURL>> = paths.iter().map(|p| NSURL::fileURLWithPath(&NSString::from_str(p))).collect();
    Some(owned(NSArray::from_retained_slice(&urls)))
}

/// Family names in order, case aside.
///
/// # Safety
///
/// `a` and `b` are strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontManagerCompareFontFamilyNames(
    a: NonNull<c_void>,
    b: NonNull<c_void>,
    _context: *mut c_void,
) -> CFComparisonResult {
    // SAFETY: as the caller promises.
    let (a, b) = unsafe { (&*a.as_ptr().cast::<NSString>(), &*b.as_ptr().cast::<NSString>()) };
    let (a, b) = (a.to_string().to_lowercase(), b.to_string().to_lowercase());
    CFComparisonResult(a.cmp(&b) as CFIndex)
}

/// What becomes of a font file's faces: made and remembered, made only for
/// a look at them, or only those made before.
#[derive(Clone, Copy, PartialEq)]
enum Faces {
    Register,
    Look,
    Known,
}

/// The faces of a font file's bytes.
fn faces_of(bytes: Vec<u8>, how: Faces) -> Vec<fonts::DataFamily> {
    let blob = parley::fontique::Blob::new(std::sync::Arc::new(bytes));
    let count = skrifa::raw::FileRef::new(blob.data()).map_or(0, |f| f.fonts().count());
    (0..count as u32)
        .filter_map(|i| {
            let font = parley::FontData::new(blob.clone(), i);
            match how {
                Faces::Register => fonts::register_data(&font),
                Faces::Look => fonts::peek_data(&font),
                Faces::Known => fonts::data_face(&font),
            }
        })
        .collect()
}

fn read_url(url: &CFURL) -> Option<Vec<u8>> {
    // SAFETY: a CFURL is an NSURL here.
    let url: &NSURL = unsafe { &*(url as *const CFURL).cast() };
    std::fs::read(url.to_file_path()?).ok()
}

/// A descriptor of a file's face, named by its PostScript name (read from
/// the file: the face needn't be laid out).
fn descriptor_of(family: fonts::DataFamily) -> Retained<NSFontDescriptor> {
    let name = NSString::from_str(&fonts::data_name(&family.0));
    let spec = FontSpec::data(family, 0.0);
    // SAFETY: the constant is this crate's own.
    let key = unsafe { objc2_app_kit::NSFontNameAttribute };
    nsfont::make_descriptor(spec, Some(NSDictionary::from_slices(&[key], &[&*name as &AnyObject])))
}

fn descriptors(faces: Vec<fonts::DataFamily>) -> Retained<NSArray<NSFontDescriptor>> {
    let all: Vec<Retained<NSFontDescriptor>> = faces.into_iter().map(descriptor_of).collect();
    NSArray::from_retained_slice(&all)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerCreateFontDescriptorsFromURL(url: Option<&CFURL>) -> Option<NonNull<CFArray>> {
    let faces = faces_of(read_url(url?)?, Faces::Look);
    (!faces.is_empty()).then(|| owned(descriptors(faces)))
}

fn data_bytes(data: &CFData) -> Vec<u8> {
    // SAFETY: a CFData is an NSData here.
    unsafe { &*(data as *const CFData).cast::<objc2_foundation::NSData>() }.to_vec()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerCreateFontDescriptorFromData(
    data: Option<&CFData>,
) -> Option<NonNull<CTFontDescriptor>> {
    let face = faces_of(data_bytes(data?), Faces::Look).into_iter().next()?;
    Some(descriptor(descriptor_of(face)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerCreateFontDescriptorsFromData(data: Option<&CFData>) -> Option<NonNull<CFArray>> {
    Some(owned(descriptors(faces_of(data_bytes(data?), Faces::Look))))
}

/// What registering a face by name found.
#[derive(Clone, Copy, PartialEq)]
enum Registered {
    /// It's registered now.
    New,
    /// It was already.
    Again,
    /// Another face has its name.
    Taken,
}

/// Make `face` findable by name.
fn register_by_name(face: &fonts::DataFamily) -> Registered {
    let name = fonts::data_name(&face.0);
    match fonts::registered_names().into_iter().find(|(n, _)| *n == name) {
        Some((_, f)) if f == *face => Registered::Again,
        Some(_) => Registered::Taken,
        None => {
            fonts::register_named(face, &[&name, &fonts::data_full_name(&face.0)]);
            Registered::New
        }
    }
}

/// The font manager's error codes (`CTFontManagerError`), and the one it
/// gives for a `CGFont` that isn't registered (measured on macOS).
const FILE_NOT_FOUND: CFIndex = 101;
const UNRECOGNIZED_FORMAT: CFIndex = 103;
const ALREADY_REGISTERED: CFIndex = 105;
const NOT_REGISTERED: CFIndex = 201;
const GRAPHICS_FONT_NOT_REGISTERED: CFIndex = -1;

/// Hand the caller an error in the font manager's domain (+1, theirs to
/// release), if it asks for one.
///
/// # Safety
///
/// `error` is null or valid to write an error pointer through.
unsafe fn fail(error: *mut *mut CFError, code: CFIndex) -> bool {
    if !error.is_null() {
        // SAFETY: the constant is this crate's own.
        let domain = ns_string(unsafe { objc2_core_text::kCTFontManagerErrorDomain });
        // SAFETY: no user info.
        let made = unsafe { NSError::errorWithDomain_code_userInfo(domain, code, None) };
        // SAFETY: as the caller promises; an NSError is a CFError here.
        unsafe { error.write(Retained::into_raw(made).cast()) };
    }
    false
}

/// # Safety
///
/// `error` is null or valid to write an error pointer through; it gets
/// one if registering fails.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontManagerRegisterFontsForURL(
    url: Option<&CFURL>,
    _scope: CTFontManagerScope,
    error: *mut *mut CFError,
) -> bool {
    // SAFETY: as the caller promises.
    unsafe { crate::coregraphics::store(error, std::ptr::null_mut()) };
    let Some(bytes) = url.and_then(read_url) else {
        // SAFETY: as the caller promises.
        return unsafe { fail(error, FILE_NOT_FOUND) };
    };
    let faces = faces_of(bytes, Faces::Register);
    if faces.is_empty() {
        // SAFETY: as the caller promises.
        return unsafe { fail(error, UNRECOGNIZED_FORMAT) };
    }
    // Every face is registered, whatever an earlier one did.
    let registered: Vec<Registered> = faces.iter().map(register_by_name).collect();
    if registered.iter().all(|&r| r == Registered::New) {
        return true;
    }
    // SAFETY: as the caller promises.
    unsafe { fail(error, ALREADY_REGISTERED) }
}

/// # Safety
///
/// As `CTFontManagerRegisterFontsForURL`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontManagerUnregisterFontsForURL(
    url: Option<&CFURL>,
    _scope: CTFontManagerScope,
    error: *mut *mut CFError,
) -> bool {
    // SAFETY: as the caller promises.
    unsafe { crate::coregraphics::store(error, std::ptr::null_mut()) };
    let faces = url.and_then(read_url).map(|b| faces_of(b, Faces::Known)).unwrap_or_default();
    let unregistered: Vec<bool> = faces.iter().map(fonts::unregister_named).collect();
    if unregistered.contains(&true) {
        return true;
    }
    // SAFETY: as the caller promises.
    unsafe { fail(error, NOT_REGISTERED) }
}

/// # Safety
///
/// As `CTFontManagerRegisterFontsForURL`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontManagerRegisterGraphicsFont(
    font: Option<&CGFont>,
    error: *mut *mut CFError,
) -> bool {
    // SAFETY: as the caller promises.
    unsafe { crate::coregraphics::store(error, std::ptr::null_mut()) };
    let face = font.and_then(|f| fonts::register_data(&crate::coregraphics::font::font_imp(f).ivars().data));
    // Registering a font again, or one whose name another has, succeeds
    // (measured on macOS); the name keeps finding the first.
    match face.map(|f| register_by_name(&f)) {
        Some(_) => true,
        // SAFETY: as the caller promises.
        None => unsafe { fail(error, UNRECOGNIZED_FORMAT) },
    }
}

/// # Safety
///
/// As `CTFontManagerRegisterFontsForURL`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontManagerUnregisterGraphicsFont(
    font: Option<&CGFont>,
    error: *mut *mut CFError,
) -> bool {
    // SAFETY: as the caller promises.
    unsafe { crate::coregraphics::store(error, std::ptr::null_mut()) };
    let face = font.and_then(|f| fonts::data_face(&crate::coregraphics::font::font_imp(f).ivars().data));
    if face.is_some_and(|f| fonts::unregister_named(&f)) {
        return true;
    }
    // SAFETY: as the caller promises.
    unsafe { fail(error, GRAPHICS_FONT_NOT_REGISTERED) }
}

fn urls_of(array: &CFArray) -> Vec<Retained<AnyObject>> {
    // SAFETY: a CFArray is an NSArray here.
    nsfont::array_items(unsafe { &*(array as *const CFArray).cast::<AnyObject>() })
}

fn register_urls(urls: &CFArray, register: bool) -> bool {
    let mut ok = true;
    for url in urls_of(urls) {
        // SAFETY: the array holds URLs, which are CFURLs here.
        let url: &CFURL = unsafe { &*Retained::as_ptr(&url).cast() };
        // SAFETY: no error is asked for.
        ok &= unsafe {
            if register {
                CTFontManagerRegisterFontsForURL(Some(url), CTFontManagerScope::Process, std::ptr::null_mut())
            } else {
                CTFontManagerUnregisterFontsForURL(Some(url), CTFontManagerScope::Process, std::ptr::null_mut())
            }
        };
    }
    ok
}

/// # Safety
///
/// `errors` is null or valid to write an array pointer through (it gets
/// none).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontManagerRegisterFontsForURLs(
    urls: Option<&CFArray>,
    _scope: CTFontManagerScope,
    errors: *mut *const CFArray,
) -> bool {
    // SAFETY: as the caller promises.
    unsafe { crate::coregraphics::store(errors, std::ptr::null()) };
    urls.is_some_and(|u| register_urls(u, true))
}

/// # Safety
///
/// As `CTFontManagerRegisterFontsForURLs`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontManagerUnregisterFontsForURLs(
    urls: Option<&CFArray>,
    _scope: CTFontManagerScope,
    errors: *mut *const CFArray,
) -> bool {
    // SAFETY: as the caller promises.
    unsafe { crate::coregraphics::store(errors, std::ptr::null()) };
    urls.is_some_and(|u| register_urls(u, false))
}

type Handler = block2::DynBlock<dyn Fn(NonNull<CFArray>, bool) -> bool>;

/// Tell a registration handler it's done, with no errors.
fn done(handler: Option<&Handler>) {
    if let Some(handler) = handler {
        let errors = NSArray::<AnyObject>::new();
        let errors = NonNull::from(&*errors).cast::<CFArray>();
        // SAFETY: the handler's function takes the block, the errors and
        // whether registration is done, and returns whether to go on.
        unsafe {
            let f: unsafe extern "C-unwind" fn(&Handler, NonNull<CFArray>, bool) -> bool =
                std::mem::transmute(super::block_function(handler));
            f(handler, errors, true);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerRegisterFontURLs(
    urls: Option<&CFArray>,
    _scope: CTFontManagerScope,
    _enabled: bool,
    handler: Option<&Handler>,
) {
    if let Some(urls) = urls {
        register_urls(urls, true);
    }
    done(handler);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerUnregisterFontURLs(
    urls: Option<&CFArray>,
    _scope: CTFontManagerScope,
    handler: Option<&Handler>,
) {
    if let Some(urls) = urls {
        register_urls(urls, false);
    }
    done(handler);
}

/// The data faces descriptors in `array` describe.
fn data_faces(array: &CFArray) -> Vec<fonts::DataFamily> {
    urls_of(array)
        .into_iter()
        .filter_map(|d| d.downcast::<NSFontDescriptor>().ok())
        .filter_map(|d| match nsfont::descriptor_spec(&d).family {
            Family::Data(face) => Some(face),
            _ => None,
        })
        .collect()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerRegisterFontDescriptors(
    descriptors: Option<&CFArray>,
    _scope: CTFontManagerScope,
    _enabled: bool,
    handler: Option<&Handler>,
) {
    for face in descriptors.map(data_faces).unwrap_or_default() {
        register_by_name(&face);
    }
    done(handler);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerUnregisterFontDescriptors(
    descriptors: Option<&CFArray>,
    _scope: CTFontManagerScope,
    handler: Option<&Handler>,
) {
    for face in descriptors.map(data_faces).unwrap_or_default() {
        fonts::unregister_named(&face);
    }
    done(handler);
}

/// Font assets belong to app bundles on Apple platforms: none are found.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerRegisterFontsWithAssetNames(
    _names: Option<&CFArray>,
    _bundle: Option<&CFBundle>,
    _scope: CTFontManagerScope,
    _enabled: bool,
    handler: Option<&Handler>,
) {
    done(handler);
}

/// Registered fonts are always enabled.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerEnableFontDescriptors(_descriptors: Option<&CFArray>, _enable: bool) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerGetScopeForURL(url: Option<&CFURL>) -> CTFontManagerScope {
    let Some(bytes) = url.and_then(read_url) else { return CTFontManagerScope::None };
    let registered = fonts::registered_names();
    let known = faces_of(bytes, Faces::Known).iter().any(|f| registered.iter().any(|(_, r)| r == f));
    if known { CTFontManagerScope::Process } else { CTFontManagerScope::None }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerCopyRegisteredFontDescriptors(
    _scope: CTFontManagerScope,
    _enabled: bool,
) -> Option<NonNull<CFArray>> {
    let mut faces: Vec<fonts::DataFamily> = Vec::new();
    for (_, face) in fonts::registered_names() {
        if !faces.contains(&face) {
            faces.push(face);
        }
    }
    Some(owned(descriptors(faces)))
}

/// Fonts are all here or not at all: the handler gets the descriptors no
/// font matches.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerRequestFonts(
    descriptors: Option<&CFArray>,
    completion: Option<&block2::DynBlock<dyn Fn(NonNull<CFArray>)>>,
) {
    let unresolved = NSMutableArray::<AnyObject>::new();
    for d in descriptors.map(urls_of).unwrap_or_default() {
        if let Ok(desc) = d.clone().downcast::<NSFontDescriptor>()
            && !nsfont::descriptor_spec(&desc).missing
        {
            continue;
        }
        unresolved.addObject(&d);
    }
    if let Some(completion) = completion {
        completion.call((NonNull::from(&*unresolved).cast::<CFArray>(),));
    }
}

/// Font requests from other processes belong to macOS's font server.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerCreateFontRequestRunLoopSource(
    _order: CFIndex,
    _callback: *const c_void,
) -> Option<NonNull<CFRunLoopSource>> {
    None
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerIsSupportedFont(url: Option<&CFURL>) -> bool {
    url.and_then(read_url)
        .is_some_and(|bytes| skrifa::raw::FileRef::new(&bytes).is_ok_and(|f| f.fonts().next().is_some()))
}

static AUTO_ACTIVATION: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerSetAutoActivationSetting(
    _bundle: Option<&CFString>,
    setting: CTFontManagerAutoActivationSetting,
) {
    AUTO_ACTIVATION.store(setting.0 as isize, std::sync::atomic::Ordering::Relaxed);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontManagerGetAutoActivationSetting(
    _bundle: Option<&CFString>,
) -> CTFontManagerAutoActivationSetting {
    CTFontManagerAutoActivationSetting(AUTO_ACTIVATION.load(std::sync::atomic::Ordering::Relaxed) as _)
}
