//! `CFBundle` over `NSBundle`: bundles made once per directory, their
//! parts, `Info.plist` (localized by `InfoPlist.strings`), resources and
//! localized strings by localization, and their executables, loaded with
//! `dlopen` and searched with `dlsym`.
//!
//! As measured on macOS (`conformance/tests/cf_types.rs`): a bundle with a
//! `Contents` directory has its plug-ins, frameworks and support files
//! there, a flat one in itself; the version number packs `CFBundleVersion`
//! as `NumVersion` does; the executable's architectures are those its
//! header names (ELF here: `kCFBundleExecutableArchitectureARM64` for
//! AArch64, `…X86_64` for x86-64); bundles have no Carbon resource maps
//! (`-1`) and no plug-ins.

use std::collections::HashMap;
use std::ffi::{CString, c_void};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSBundle, NSNumber, NSString, NSURL};

use super::string::text;
use super::types::{CFTypeID, id, keep_named, object, owned};

crate::constant_string!(kCFBundleInfoDictionaryVersionKey = "CFBundleInfoDictionaryVersion");
crate::constant_string!(kCFBundleExecutableKey = "CFBundleExecutable");
crate::constant_string!(kCFBundleIdentifierKey = "CFBundleIdentifier");
crate::constant_string!(kCFBundleVersionKey = "CFBundleVersion");
crate::constant_string!(kCFBundleDevelopmentRegionKey = "CFBundleDevelopmentRegion");
crate::constant_string!(kCFBundleNameKey = "CFBundleName");
crate::constant_string!(kCFBundleLocalizationsKey = "CFBundleLocalizations");

type Boolean = u8;

fn bundle<'a>(cf: *const c_void) -> &'a NSBundle {
    // SAFETY: the callers' contracts: `cf` is a bundle.
    unsafe { &*cf.cast::<NSBundle>() }
}

fn string_of(cf: *const c_void) -> Option<String> {
    // SAFETY: the callers' contracts: `cf` is null or a string.
    (!cf.is_null()).then(|| text(unsafe { object(cf) }).into_owned())
}

fn strings_of(cf: *const c_void) -> Vec<String> {
    if cf.is_null() {
        return Vec::new();
    }
    // SAFETY: the callers' contracts: `cf` is an array of strings.
    unsafe { &*cf.cast::<NSArray<NSString>>() }.iter().map(|s| s.to_string()).collect()
}

fn path_of_url(cf: *const c_void) -> Option<PathBuf> {
    // SAFETY: the callers' contracts: `cf` is null or a URL.
    (!cf.is_null()).then(|| crate::url::file_path(unsafe { &*cf.cast::<NSURL>() })).flatten()
}

fn directory_url(path: &Path) -> *mut c_void {
    let text = format!("{}/", crate::path::without_slash(path).trim_end_matches('/'));
    crate::url::file_url(Path::new(&text)).map_or(std::ptr::null_mut(), owned)
}

fn file_url(path: &Path) -> *mut c_void {
    crate::url::file_url(path).map_or(std::ptr::null_mut(), owned)
}

fn strings(list: &[String]) -> *mut c_void {
    let strings: Vec<Retained<NSString>> = list.iter().map(|s| NSString::from_str(s)).collect();
    owned(NSArray::from_retained_slice(&strings))
}

fn urls(paths: &[PathBuf]) -> *mut c_void {
    let urls: Vec<Retained<NSURL>> = paths.iter().filter_map(|p| crate::url::file_url(p)).collect();
    owned(NSArray::from_retained_slice(&urls))
}

/// Where a bundle keeps what isn't a resource: `Contents`, or the bundle
/// itself.
fn support_directory(layout: &crate::bundle::Layout) -> PathBuf {
    let contents = layout.path.join("Contents");
    if contents.is_dir() { contents } else { layout.path.clone() }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFBundleGetTypeID() -> CFTypeID {
    id::BUNDLE
}

/// The main bundle, which lives for the rest of the process.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFBundleGetMainBundle() -> *const c_void {
    Retained::as_ptr(&NSBundle::mainBundle()).cast()
}

/// The bundle at a directory: one object per directory.
///
/// # Safety
///
/// `url` is null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCreate(_alloc: *const c_void, url: *const c_void) -> *mut c_void {
    path_of_url(url).and_then(|p| crate::bundle::at_path(&p)).map_or(std::ptr::null_mut(), owned)
}

/// The bundles in a directory: its subdirectories with the extension, or
/// with any that are bundles (have an `Info.plist`).
///
/// # Safety
///
/// `url` is null or a URL; `kind` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCreateBundlesFromDirectory(
    _alloc: *const c_void,
    url: *const c_void,
    kind: *const c_void,
) -> *mut c_void {
    let Some(dir) = path_of_url(url) else { return std::ptr::null_mut() };
    let kind = string_of(kind).map(|k| k.trim_start_matches('.').to_string());
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    found.sort();
    let bundles: Vec<Retained<NSBundle>> = found
        .into_iter()
        .filter(|p| p.is_dir())
        .filter(|p| match &kind {
            Some(kind) if !kind.is_empty() => p.extension().is_some_and(|e| e == kind.as_str()),
            _ => p.join("Contents/Info.plist").exists() || p.join("Info.plist").exists(),
        })
        .filter_map(|p| crate::bundle::at_path(&p))
        .collect();
    owned(NSArray::from_retained_slice(&bundles))
}

/// # Safety
///
/// `identifier` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetBundleWithIdentifier(identifier: *const c_void) -> *const c_void {
    let Some(identifier) = string_of(identifier) else { return std::ptr::null() };
    crate::bundle::all()
        .into_iter()
        .find(|b| b.bundleIdentifier().is_some_and(|i| i.to_string() == identifier))
        // Bundles live for the rest of the process.
        .map_or(std::ptr::null(), |b| Retained::as_ptr(&b).cast())
}

/// The bundles made so far, in an array kept until more are made.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFBundleGetAllBundles() -> *const c_void {
    static ARRAYS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
    let all = crate::bundle::all();
    let mut arrays = crate::thread::lock(&ARRAYS);
    if let Some(&(count, array)) = arrays.last()
        && count == all.len()
    {
        return array as *const c_void;
    }
    // Arrays handed out before stay alive: callers don't own them.
    let array = Retained::into_raw(NSArray::from_retained_slice(&all)) as usize;
    arrays.push((all.len(), array));
    array as *const c_void
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyBundleURL(cf: *const c_void) -> *mut c_void {
    directory_url(&crate::bundle::layout(bundle(cf)).path)
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyExecutableURL(cf: *const c_void) -> *mut c_void {
    match executable(bundle(cf)) {
        Some(path) => file_url(&path),
        None => std::ptr::null_mut(),
    }
}

/// A bundle's executable, if it is there.
fn executable(b: &NSBundle) -> Option<PathBuf> {
    crate::bundle::layout(b).executable.clone().filter(|p| p.is_file())
}

/// An executable of the bundle's by name: in `MacOS` (or the bundle itself
/// when flat), if it is there.
///
/// # Safety
///
/// `cf` is a bundle; `name` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyAuxiliaryExecutableURL(
    cf: *const c_void,
    name: *const c_void,
) -> *mut c_void {
    let Some(name) = string_of(name) else { return std::ptr::null_mut() };
    let layout = crate::bundle::layout(bundle(cf));
    let support = support_directory(layout);
    let dir = if support != layout.path { support.join("MacOS") } else { support };
    let path = dir.join(name);
    if path.is_file() { file_url(&path) } else { std::ptr::null_mut() }
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyResourcesDirectoryURL(cf: *const c_void) -> *mut c_void {
    directory_url(&crate::bundle::layout(bundle(cf)).resources)
}

macro_rules! support_urls {
    ($($name:ident = $dir:literal,)*) => {$(
        /// # Safety
        ///
        /// `cf` is a bundle.
        #[unsafe(no_mangle)]
        pub unsafe extern "C-unwind" fn $name(cf: *const c_void) -> *mut c_void {
            directory_url(&support_directory(crate::bundle::layout(bundle(cf))).join($dir))
        }
    )*};
}

support_urls! {
    CFBundleCopyBuiltInPlugInsURL = "PlugIns",
    CFBundleCopyPrivateFrameworksURL = "Frameworks",
    CFBundleCopySharedFrameworksURL = "SharedFrameworks",
    CFBundleCopySharedSupportURL = "SharedSupport",
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopySupportFilesDirectoryURL(cf: *const c_void) -> *mut c_void {
    directory_url(&support_directory(crate::bundle::layout(bundle(cf))))
}

fn info_text(b: &NSBundle, key: &str) -> Option<String> {
    crate::bundle::info(b)?.get(key)?.as_string().map(str::to_string)
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetIdentifier(cf: *const c_void) -> *const c_void {
    let b = bundle(cf);
    keep_named(b, "CFBundleIdentifier", || info_text(b, "CFBundleIdentifier").map(|s| NSString::from_str(&s).into()))
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetDevelopmentRegion(cf: *const c_void) -> *const c_void {
    let b = bundle(cf);
    keep_named(b, "CFBundleDevelopmentRegion", || {
        info_text(b, "CFBundleDevelopmentRegion").map(|s| NSString::from_str(&s).into())
    })
}

/// The info dictionary, which the bundle keeps.
///
/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetInfoDictionary(cf: *const c_void) -> *const c_void {
    let b = bundle(cf);
    keep_named(b, "info dictionary", || b.infoDictionary().map(Into::into))
}

/// The bundle's `InfoPlist.strings` for its preferred localization.
///
/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetLocalInfoDictionary(cf: *const c_void) -> *const c_void {
    let b = bundle(cf);
    keep_named(b, "local info dictionary", || {
        let table = crate::bundle::strings_table(b, Some("InfoPlist"), None);
        let keys: Vec<Retained<NSString>> = table.keys().map(|k| NSString::from_str(k)).collect();
        let values: Vec<Retained<AnyObject>> = table.values().map(|v| NSString::from_str(v).into()).collect();
        let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        Some(objc2_foundation::NSDictionary::from_retained_objects(&keys, &values).into())
    })
}

/// An info dictionary value, localized where `InfoPlist.strings` has it.
///
/// # Safety
///
/// `cf` is a bundle; `key` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetValueForInfoDictionaryKey(
    cf: *const c_void,
    key: *const c_void,
) -> *const c_void {
    let Some(name) = string_of(key) else { return std::ptr::null() };
    let b = bundle(cf);
    keep_named(b, &format!("value {name}"), || {
        if let Some(local) = crate::bundle::strings_table(b, Some("InfoPlist"), None).remove(&name) {
            return Some(NSString::from_str(&local).into());
        }
        crate::bundle::info(b)?.get(&name).and_then(crate::plist::to_object)
    })
}

/// # Safety
///
/// `url` is null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyInfoDictionaryForURL(url: *const c_void) -> *mut c_void {
    let Some(path) = path_of_url(url) else { return std::ptr::null_mut() };
    let info = if path.is_dir() { crate::bundle::directory_layout(&path).info } else { path };
    read_info(&info)
}

/// # Safety
///
/// `url` is null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyInfoDictionaryInDirectory(url: *const c_void) -> *mut c_void {
    let Some(path) = path_of_url(url) else { return std::ptr::null_mut() };
    read_info(&crate::bundle::directory_layout(&path).info)
}

fn read_info(path: &Path) -> *mut c_void {
    let Ok(bytes) = std::fs::read(path) else { return std::ptr::null_mut() };
    match crate::plist::parse(&bytes) {
        Some((value @ plist::Value::Dictionary(_), _)) => {
            crate::plist::to_object(&value).map_or(std::ptr::null_mut(), owned)
        }
        _ => std::ptr::null_mut(),
    }
}

/// `CFBundleVersion` packed as a `NumVersion`: the major version in two
/// BCD digits, the minor and bug-fix versions a digit each, the stage
/// (development, alpha, beta, final) and the prerelease revision; 0 for a
/// version that doesn't fit.
fn number_version(text: &str) -> u32 {
    let (numbers, stage_part) = match text.find(|c: char| c.is_ascii_alphabetic()) {
        Some(at) => text.split_at(at),
        None => (text, ""),
    };
    let parts: Vec<&str> = numbers.split('.').collect();
    let number =
        |i: usize| parts.get(i).map_or(Some(0), |p| if p.is_empty() { Some(0) } else { p.parse::<u32>().ok() });
    let (Some(major), Some(minor), Some(bug)) = (number(0), number(1), number(2)) else { return 0 };
    if parts.len() > 3 || major > 99 || minor > 9 || bug > 9 {
        return 0;
    }
    let letters = stage_part.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let (stage, revision) = match &stage_part[..stage_part.len() - letters.len()] {
        "" => (0x80, 0),
        "d" => (0x20, letters.parse::<u32>().unwrap_or(0)),
        "a" => (0x40, letters.parse::<u32>().unwrap_or(0)),
        "b" => (0x60, letters.parse::<u32>().unwrap_or(0)),
        "fc" => (0x80, letters.parse::<u32>().unwrap_or(0)),
        _ => return 0,
    };
    if revision > 255 {
        return 0;
    }
    ((major / 10) << 28) | ((major % 10) << 24) | (minor << 20) | (bug << 16) | (stage << 8) | revision
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetVersionNumber(cf: *const c_void) -> u32 {
    info_text(bundle(cf), "CFBundleVersion").map_or(0, |v| number_version(&v))
}

/// A four-character code, `????` for none.
fn four_cc(text: Option<String>) -> u32 {
    match text.as_deref().map(str::as_bytes) {
        Some(&[a, b, c, d]) => u32::from_be_bytes([a, b, c, d]),
        _ => u32::from_be_bytes(*b"????"),
    }
}

/// # Safety
///
/// `cf` is a bundle; `kind` and `creator` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetPackageInfo(cf: *const c_void, kind: *mut u32, creator: *mut u32) {
    let b = bundle(cf);
    // SAFETY: per this function's contract.
    unsafe { write_package(kind, creator, info_text(b, "CFBundlePackageType"), info_text(b, "CFBundleSignature")) };
}

/// # Safety
///
/// `kind` and `creator` null or writable.
unsafe fn write_package(kind: *mut u32, creator: *mut u32, k: Option<String>, c: Option<String>) {
    // SAFETY: per this function's contract.
    unsafe {
        if !kind.is_null() {
            kind.write(four_cc(k));
        }
        if !creator.is_null() {
            creator.write(four_cc(c));
        }
    }
}

/// # Safety
///
/// `url` is null or a URL; `kind` and `creator` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetPackageInfoInDirectory(
    url: *const c_void,
    kind: *mut u32,
    creator: *mut u32,
) -> Boolean {
    let Some(path) = path_of_url(url).filter(|p| p.is_dir()) else { return 0 };
    let info = std::fs::read(crate::bundle::directory_layout(&path).info)
        .ok()
        .and_then(|bytes| crate::plist::parse(&bytes))
        .and_then(|(v, _)| v.into_dictionary());
    let Some(info) = info else { return 0 };
    let text = |k: &str| info.get(k).and_then(|v| v.as_string()).map(str::to_string);
    // SAFETY: per this function's contract.
    unsafe { write_package(kind, creator, text("CFBundlePackageType"), text("CFBundleSignature")) };
    1
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyBundleLocalizations(cf: *const c_void) -> *mut c_void {
    strings(&crate::bundle::localizations_in(&crate::bundle::layout(bundle(cf)).resources))
}

/// # Safety
///
/// `url` is null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyLocalizationsForURL(url: *const c_void) -> *mut c_void {
    let Some(path) = path_of_url(url) else { return std::ptr::null_mut() };
    strings(&crate::bundle::localizations_in(&crate::bundle::directory_layout(&path).resources))
}

/// Of the localizations, the one the user prefers.
///
/// # Safety
///
/// `localizations` is null or an array of strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyPreferredLocalizationsFromArray(
    localizations: *const c_void,
) -> *mut c_void {
    let available = strings_of(localizations);
    let chosen = crate::bundle::preferred_of(&available, &crate::bundle::preferred_languages());
    strings(&chosen.into_iter().collect::<Vec<_>>())
}

/// Of the localizations, the one the preferences (the user's, for none)
/// ask for first.
///
/// # Safety
///
/// Both are null or arrays of strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyLocalizationsForPreferences(
    localizations: *const c_void,
    preferences: *const c_void,
) -> *mut c_void {
    let available = strings_of(localizations);
    let preferences =
        if preferences.is_null() { crate::bundle::preferred_languages() } else { strings_of(preferences) };
    let chosen = crate::bundle::preferred_of(&available, &preferences);
    strings(&chosen.into_iter().collect::<Vec<_>>())
}

/// # Safety
///
/// `cf` is a bundle; the others null or strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyLocalizedString(
    cf: *const c_void,
    key: *const c_void,
    value: *const c_void,
    table: *const c_void,
) -> *mut c_void {
    let Some(key) = string_of(key) else { return std::ptr::null_mut() };
    let (value, table) = (string_of(value), string_of(table));
    let found = crate::bundle::localized_string(bundle(cf), &key, value.as_deref(), table.as_deref(), None);
    owned(NSString::from_str(&found))
}

/// # Safety
///
/// As [`CFBundleCopyLocalizedString`]; `localizations` null or an array of
/// strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyLocalizedStringForLocalizations(
    cf: *const c_void,
    key: *const c_void,
    value: *const c_void,
    table: *const c_void,
    localizations: *const c_void,
) -> *mut c_void {
    let Some(key) = string_of(key) else { return std::ptr::null_mut() };
    let b = bundle(cf);
    let has = crate::bundle::localizations_in(&crate::bundle::layout(b).resources);
    let wanted = strings_of(localizations);
    let localization = wanted.iter().find(|w| has.contains(w)).cloned();
    let (value, table) = (string_of(value), string_of(table));
    let found = crate::bundle::localized_string(b, &key, value.as_deref(), table.as_deref(), localization.as_deref());
    owned(NSString::from_str(&found))
}

/// # Safety
///
/// `cf` is a bundle; the others null or strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyResourceURL(
    cf: *const c_void,
    name: *const c_void,
    kind: *const c_void,
    directory: *const c_void,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    unsafe { CFBundleCopyResourceURLForLocalization(cf, name, kind, directory, std::ptr::null()) }
}

/// # Safety
///
/// `cf` is a bundle; the others null or strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyResourceURLForLocalization(
    cf: *const c_void,
    name: *const c_void,
    kind: *const c_void,
    directory: *const c_void,
    localization: *const c_void,
) -> *mut c_void {
    let (name, kind, directory, localization) =
        (string_of(name), string_of(kind), string_of(directory), string_of(localization));
    let found = crate::bundle::find_resource(
        bundle(cf),
        name.as_deref(),
        kind.as_deref(),
        directory.as_deref(),
        localization.as_deref(),
    );
    found.map_or(std::ptr::null_mut(), |p| file_url(&p))
}

/// # Safety
///
/// `cf` is a bundle; the others null or strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyResourceURLsOfType(
    cf: *const c_void,
    kind: *const c_void,
    directory: *const c_void,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    unsafe { CFBundleCopyResourceURLsOfTypeForLocalization(cf, kind, directory, std::ptr::null()) }
}

/// # Safety
///
/// `cf` is a bundle; the others null or strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyResourceURLsOfTypeForLocalization(
    cf: *const c_void,
    kind: *const c_void,
    directory: *const c_void,
    localization: *const c_void,
) -> *mut c_void {
    let (kind, directory, localization) = (string_of(kind), string_of(directory), string_of(localization));
    urls(&crate::bundle::find_resources(bundle(cf), kind.as_deref(), directory.as_deref(), localization.as_deref()))
}

/// # Safety
///
/// `url` is null or a URL; the others null or strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyResourceURLInDirectory(
    url: *const c_void,
    name: *const c_void,
    kind: *const c_void,
    directory: *const c_void,
) -> *mut c_void {
    let Some(b) = path_of_url(url).and_then(|p| crate::bundle::at_path(&p)) else { return std::ptr::null_mut() };
    // SAFETY: per this function's contract.
    unsafe { CFBundleCopyResourceURL(Retained::as_ptr(&b).cast(), name, kind, directory) }
}

/// # Safety
///
/// `url` is null or a URL; the others null or strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyResourceURLsOfTypeInDirectory(
    url: *const c_void,
    kind: *const c_void,
    directory: *const c_void,
) -> *mut c_void {
    let Some(b) = path_of_url(url).and_then(|p| crate::bundle::at_path(&p)) else { return std::ptr::null_mut() };
    // SAFETY: per this function's contract.
    unsafe { CFBundleCopyResourceURLsOfType(Retained::as_ptr(&b).cast(), kind, directory) }
}

// Executables.

/// `kCFBundleExecutableArchitecture…` for an ELF file's machine.
fn architecture(path: &Path) -> Option<i32> {
    let mut header = [0u8; 20];
    let mut file = std::fs::File::open(path).ok()?;
    std::io::Read::read_exact(&mut file, &mut header).ok()?;
    if &header[..4] != b"\x7fELF" {
        return None;
    }
    let machine = if header[5] == 2 {
        u16::from_be_bytes([header[18], header[19]])
    } else {
        u16::from_le_bytes([header[18], header[19]])
    };
    Some(match machine {
        183 => 0x0100_000c,
        62 => 0x0100_0007,
        3 => 0x7,
        40 => 12,
        20 => 18,
        21 => 0x0100_0012,
        _ => return None,
    })
}

fn architectures(path: Option<PathBuf>) -> *mut c_void {
    let Some(arch) = path.as_deref().and_then(architecture) else { return std::ptr::null_mut() };
    owned(NSArray::from_retained_slice(&[NSNumber::new_i32(arch)]))
}

/// The architecture this process runs.
fn own_architecture() -> i32 {
    if cfg!(target_arch = "aarch64") { 0x0100_000c } else { 0x0100_0007 }
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyExecutableArchitectures(cf: *const c_void) -> *mut c_void {
    architectures(executable(bundle(cf)))
}

/// # Safety
///
/// `url` is null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleCopyExecutableArchitecturesForURL(url: *const c_void) -> *mut c_void {
    let Some(path) = path_of_url(url) else { return std::ptr::null_mut() };
    let executable = if path.is_dir() { crate::bundle::directory_layout(&path).executable } else { Some(path) };
    architectures(executable)
}

fn loadable(path: Option<&Path>) -> bool {
    path.and_then(architecture) == Some(own_architecture())
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleIsExecutableLoadable(cf: *const c_void) -> Boolean {
    u8::from(loadable(executable(bundle(cf)).as_deref()))
}

/// # Safety
///
/// `url` is null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleIsExecutableLoadableForURL(url: *const c_void) -> Boolean {
    let Some(path) = path_of_url(url) else { return 0 };
    let executable = if path.is_dir() { crate::bundle::directory_layout(&path).executable } else { Some(path) };
    u8::from(loadable(executable.as_deref()))
}

/// The executables loaded with `dlopen`, by path, and their handles.
static LOADED: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

fn is_main(b: &NSBundle) -> bool {
    std::ptr::eq(b, &*NSBundle::mainBundle())
}

/// The handle to search a bundle's symbols in: the process's for the main
/// bundle, else the bundle's executable, loaded if it isn't.
fn handle(b: &NSBundle, load: bool) -> Result<*mut c_void, isize> {
    if is_main(b) {
        return Ok(libc::RTLD_DEFAULT);
    }
    let Some(path) = executable(b) else { return Err(crate::error::code::FILE_NO_SUCH_FILE) };
    let mut loaded = crate::thread::lock(&LOADED);
    let loaded = loaded.get_or_insert_with(HashMap::new);
    if let Some(&h) = loaded.get(&path) {
        return Ok(h as *mut c_void);
    }
    if !load {
        return Err(0);
    }
    if !loadable(Some(&path)) {
        // `NSExecutableArchitectureMismatchError`.
        return Err(3585);
    }
    let Ok(c) = CString::new(path.to_string_lossy().into_owned()) else { return Err(3587) };
    // SAFETY: a NUL-terminated path; the handle stays until unloaded.
    let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if h.is_null() {
        // `NSExecutableLoadError`.
        return Err(3587);
    }
    loaded.insert(path, h as usize);
    Ok(h)
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleIsExecutableLoaded(cf: *const c_void) -> Boolean {
    u8::from(handle(bundle(cf), false).is_ok())
}

/// # Safety
///
/// `cf` is a bundle; `error` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleLoadExecutableAndReturnError(
    cf: *const c_void,
    error: *mut *mut c_void,
) -> Boolean {
    match handle(bundle(cf), true) {
        Ok(_) => 1,
        Err(code) => {
            if !error.is_null() {
                // SAFETY: per this function's contract; a +1 error.
                unsafe { error.write(Retained::into_raw(crate::error::cocoa(code, &[])).cast()) };
            }
            0
        }
    }
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleLoadExecutable(cf: *const c_void) -> Boolean {
    // SAFETY: per this function's contract.
    unsafe { CFBundleLoadExecutableAndReturnError(cf, std::ptr::null_mut()) }
}

/// Whether the executable would load: it is there and for this machine.
///
/// # Safety
///
/// `cf` is a bundle; `error` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundlePreflightExecutable(cf: *const c_void, error: *mut *mut c_void) -> Boolean {
    let b = bundle(cf);
    if is_main(b) {
        return 1;
    }
    let code = match executable(b) {
        None => crate::error::code::FILE_NO_SUCH_FILE,
        Some(path) if !loadable(Some(&path)) => 3585,
        Some(_) => return 1,
    };
    if !error.is_null() {
        // SAFETY: per this function's contract; a +1 error.
        unsafe { error.write(Retained::into_raw(crate::error::cocoa(code, &[])).cast()) };
    }
    0
}

/// # Safety
///
/// `cf` is a bundle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleUnloadExecutable(cf: *const c_void) {
    let b = bundle(cf);
    if is_main(b) {
        return;
    }
    let Some(path) = executable(b) else { return };
    let removed = crate::thread::lock(&LOADED).as_mut().and_then(|l| l.remove(&path));
    if let Some(h) = removed {
        // SAFETY: a handle from dlopen, closed once.
        unsafe { libc::dlclose(h as *mut c_void) };
    }
}

/// A symbol of the bundle's executable (loaded if it isn't), or NULL.
fn symbol(b: &NSBundle, name: &str) -> *mut c_void {
    let Ok(h) = handle(b, true) else { return std::ptr::null_mut() };
    let Ok(c) = CString::new(name) else { return std::ptr::null_mut() };
    // SAFETY: a handle and a NUL-terminated name.
    unsafe { libc::dlsym(h, c.as_ptr()) }
}

/// # Safety
///
/// `cf` is a bundle; `name` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetFunctionPointerForName(
    cf: *const c_void,
    name: *const c_void,
) -> *mut c_void {
    string_of(name).map_or(std::ptr::null_mut(), |n| symbol(bundle(cf), &n))
}

/// # Safety
///
/// `cf` is a bundle; `name` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetDataPointerForName(cf: *const c_void, name: *const c_void) -> *mut c_void {
    string_of(name).map_or(std::ptr::null_mut(), |n| symbol(bundle(cf), &n))
}

/// # Safety
///
/// `cf` is a bundle; `names` null or an array of strings; `table` null or
/// with room for its count.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetFunctionPointersForNames(
    cf: *const c_void,
    names: *const c_void,
    table: *mut *mut c_void,
) {
    if table.is_null() {
        return;
    }
    for (i, name) in strings_of(names).iter().enumerate() {
        // SAFETY: per this function's contract.
        unsafe { table.add(i).write(symbol(bundle(cf), name)) };
    }
}

/// # Safety
///
/// As [`CFBundleGetFunctionPointersForNames`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleGetDataPointersForNames(
    cf: *const c_void,
    names: *const c_void,
    table: *mut *mut c_void,
) {
    // SAFETY: per this function's contract.
    unsafe { CFBundleGetFunctionPointersForNames(cf, names, table) };
}

/// NULL: Sidestep has no plug-ins.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFBundleGetPlugIn(_cf: *const c_void) -> *const c_void {
    std::ptr::null()
}

/// -1: no Carbon resource map, as macOS answers today.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFBundleOpenBundleResourceMap(_cf: *const c_void) -> i32 {
    -1
}

/// `resFNotFound` (-193), both references -1.
///
/// # Safety
///
/// Both references are null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBundleOpenBundleResourceFiles(
    _cf: *const c_void,
    refs: *mut i32,
    localized: *mut i32,
) -> i32 {
    // SAFETY: per this function's contract.
    unsafe {
        if !refs.is_null() {
            refs.write(-1);
        }
        if !localized.is_null() {
            localized.write(-1);
        }
    }
    -193
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFBundleCloseBundleResourceMap(_cf: *const c_void, _refs: i32) {}

#[cfg(test)]
mod tests {
    use super::number_version;

    #[test]
    fn version_numbers() {
        assert_eq!(number_version("1.2.3"), 0x0123_8000);
        assert_eq!(number_version("10.4"), 0x1040_8000);
        assert_eq!(number_version("1.0b2"), 0x0100_6002);
        assert_eq!(number_version("x"), 0);
        assert_eq!(number_version("1.2.3.4"), 0);
    }
}
