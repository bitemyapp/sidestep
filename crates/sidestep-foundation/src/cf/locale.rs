//! `CFLocale` over `NSLocale`: locales, their values (`NSLocale`'s
//! `-objectForKey:`), identifiers taken apart, put together and made
//! canonical, the ISO code lists, English display names, language
//! directions, and the old Mac OS and Windows locale codes — the lists,
//! names and codes as macOS gives them (`locale_data`).
//!
//! Canonical identifiers follow ICU's rules as far as they go without its
//! data: each subtag's case (language lower, script title, region upper),
//! the replaced language codes, keywords' names lowercased; not ICU's
//! likely-subtag additions and removals. Display names are English
//! whatever the display locale, as the rest of Sidestep's locale data is.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSDictionary, NSLocale, NSString};

use super::locale_data as data;

/// Export a constant string under Foundation's and CoreFoundation's
/// names, one object for both, as on macOS.
macro_rules! shared_keys {
    ($($object:ident: $ns:ident, $cf:ident = $value:literal;)*) => {$(
        static $object: crate::ConstantString =
            crate::ConstantString::new(&crate::CONSTANT_STRING_CLASS, crate::ConstStr::new(concat!($value, "\0")));
        #[unsafe(no_mangle)]
        pub static $ns: sidestep_runtime::ObjectRef = $object.object_ref();
        #[unsafe(no_mangle)]
        pub static $cf: sidestep_runtime::ObjectRef = $object.object_ref();
    )*};
}

shared_keys! {
    EXEMPLAR_CHARACTER_SET: NSLocaleExemplarCharacterSet, kCFLocaleExemplarCharacterSet = "kCFLocaleExemplarCharacterSetKey";
    CALENDAR: NSLocaleCalendar, kCFLocaleCalendar = "kCFLocaleCalendarKey";
    COLLATION_IDENTIFIER: NSLocaleCollationIdentifier, kCFLocaleCollationIdentifier = "collation";
    MEASUREMENT_SYSTEM: NSLocaleMeasurementSystem, kCFLocaleMeasurementSystem = "kCFLocaleMeasurementSystemKey";
    CURRENCY_SYMBOL: NSLocaleCurrencySymbol, kCFLocaleCurrencySymbol = "kCFLocaleCurrencySymbolKey";
    CURRENCY_CODE: NSLocaleCurrencyCode, kCFLocaleCurrencyCode = "currency";
    COLLATOR_IDENTIFIER: NSLocaleCollatorIdentifier, kCFLocaleCollatorIdentifier = "kCFLocaleCollatorIdentifierKey";
    QUOTATION_BEGIN: NSLocaleQuotationBeginDelimiterKey, kCFLocaleQuotationBeginDelimiterKey = "kCFLocaleQuotationBeginDelimiterKey";
    QUOTATION_END: NSLocaleQuotationEndDelimiterKey, kCFLocaleQuotationEndDelimiterKey = "kCFLocaleQuotationEndDelimiterKey";
    ALTERNATE_QUOTATION_BEGIN: NSLocaleAlternateQuotationBeginDelimiterKey,
        kCFLocaleAlternateQuotationBeginDelimiterKey = "kCFLocaleAlternateQuotationBeginDelimiterKey";
    ALTERNATE_QUOTATION_END: NSLocaleAlternateQuotationEndDelimiterKey,
        kCFLocaleAlternateQuotationEndDelimiterKey = "kCFLocaleAlternateQuotationEndDelimiterKey";
}

crate::constant_string!(kCFLocaleCalendarIdentifier = "calendar");

// The keys NSLocale reads itself, under CoreFoundation's names.
#[unsafe(no_mangle)]
pub static kCFLocaleIdentifier: sidestep_runtime::ObjectRef = crate::locale::IDENTIFIER.object_ref();
#[unsafe(no_mangle)]
pub static kCFLocaleLanguageCode: sidestep_runtime::ObjectRef = crate::locale::LANGUAGE_CODE.object_ref();
#[unsafe(no_mangle)]
pub static kCFLocaleCountryCode: sidestep_runtime::ObjectRef = crate::locale::COUNTRY_CODE.object_ref();
#[unsafe(no_mangle)]
pub static kCFLocaleScriptCode: sidestep_runtime::ObjectRef = crate::locale::SCRIPT_CODE.object_ref();
#[unsafe(no_mangle)]
pub static kCFLocaleVariantCode: sidestep_runtime::ObjectRef = crate::locale::VARIANT_CODE.object_ref();
#[unsafe(no_mangle)]
pub static kCFLocaleDecimalSeparator: sidestep_runtime::ObjectRef = crate::locale::DECIMAL_SEPARATOR.object_ref();
#[unsafe(no_mangle)]
pub static kCFLocaleGroupingSeparator: sidestep_runtime::ObjectRef = crate::locale::GROUPING_SEPARATOR.object_ref();
#[unsafe(no_mangle)]
pub static kCFLocaleUsesMetricSystem: sidestep_runtime::ObjectRef = crate::locale::USES_METRIC_SYSTEM.object_ref();
use super::string::text;
use super::types::{CFTypeID, id, keep_named, object, owned};

type CFIndex = isize;

fn locale<'a>(cf: *const c_void) -> &'a NSLocale {
    // SAFETY: the callers' contracts: `cf` is a locale.
    unsafe { &*cf.cast::<NSLocale>() }
}

fn string_of(cf: *const c_void) -> Option<String> {
    // SAFETY: the callers' contracts: `cf` is null or a string.
    (!cf.is_null()).then(|| text(unsafe { object(cf) }).into_owned())
}

fn strings(list: &[&str]) -> *mut c_void {
    let strings: Vec<Retained<NSString>> = list.iter().map(|s| NSString::from_str(s)).collect();
    owned(NSArray::from_retained_slice(&strings))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleGetTypeID() -> CFTypeID {
    id::LOCALE
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleCopyCurrent() -> *mut c_void {
    owned(NSLocale::currentLocale())
}

/// The system locale, identifier "": one object for the process.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleGetSystem() -> *const c_void {
    static SYSTEM: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SYSTEM.get_or_init(|| Retained::into_raw(crate::locale::object("")) as usize) as *const c_void
}

/// A locale for an identifier, made canonical.
///
/// # Safety
///
/// `identifier` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFLocaleCreate(_alloc: *const c_void, identifier: *const c_void) -> *mut c_void {
    let Some(identifier) = string_of(identifier) else { return std::ptr::null_mut() };
    owned(crate::locale::object(&canonical(&identifier, false)))
}

/// The same locale: locales can't change.
///
/// # Safety
///
/// `cf` is null or a locale.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFLocaleCreateCopy(_alloc: *const c_void, cf: *const c_void) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    owned(objc2::Message::retain(locale(cf)))
}

/// # Safety
///
/// `cf` is a locale.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFLocaleGetIdentifier(cf: *const c_void) -> *const c_void {
    // SAFETY: per this function's contract.
    keep_named(unsafe { object(cf) }, "identifier", || Some(locale(cf).localeIdentifier().into()))
}

/// # Safety
///
/// `cf` is a locale; `key` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFLocaleGetValue(cf: *const c_void, key: *const c_void) -> *const c_void {
    let Some(name) = string_of(key) else { return std::ptr::null() };
    // SAFETY: per this function's contract; -objectForKey: takes a key.
    keep_named(unsafe { object(cf) }, &name, || unsafe { objc2::msg_send![locale(cf), objectForKey: object(key)] })
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleCopyAvailableLocaleIdentifiers() -> *mut c_void {
    strings(data::AVAILABLE)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleCopyISOLanguageCodes() -> *mut c_void {
    strings(data::ISO_LANGUAGES)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleCopyISOCountryCodes() -> *mut c_void {
    strings(data::ISO_COUNTRIES)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleCopyISOCurrencyCodes() -> *mut c_void {
    strings(data::ISO_CURRENCIES)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleCopyCommonISOCurrencyCodes() -> *mut c_void {
    strings(data::COMMON_CURRENCIES)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleCopyPreferredLanguages() -> *mut c_void {
    owned(NSLocale::preferredLanguages())
}

fn name_in(table: &[(&str, &'static str)], code: &str) -> Option<&'static str> {
    table.iter().find(|(c, _)| *c == code).map(|&(_, n)| n)
}

/// An identifier's English name: its language's, then its script,
/// region and variant in parentheses. Han scripts are "Simplified" and
/// "Traditional" there, and qualify Chinese and Cantonese directly
/// ("Chinese, Traditional (Taiwan)"), as on macOS.
fn identifier_name(identifier: &str) -> Option<String> {
    let p = crate::locale::parts(identifier);
    let code = p.language.to_ascii_lowercase();
    let mut language = name_in(data::LANGUAGE_NAMES, &code)?.to_string();
    let mut qualifiers = Vec::new();
    if let Some(script) = p.script {
        let han = match script {
            "Hans" => Some("Simplified"),
            "Hant" => Some("Traditional"),
            _ => None,
        };
        match han {
            Some(han) if code == "zh" || code == "yue" => language = format!("{language}, {han}"),
            Some(han) => qualifiers.push(han.to_string()),
            None => qualifiers.push(name_in(data::SCRIPT_NAMES, script)?.to_string()),
        }
    }
    if let Some(region) = p.region {
        qualifiers.push(name_in(data::COUNTRY_NAMES, &region.to_ascii_uppercase())?.to_string());
    }
    if let Some(variant) = p.variant {
        qualifiers.push(if variant.eq_ignore_ascii_case("POSIX") {
            "Computer".to_string()
        } else {
            variant.to_string()
        });
    }
    Some(if qualifiers.is_empty() { language.to_string() } else { format!("{language} ({})", qualifiers.join(", ")) })
}

/// A value's English name for a key: an identifier's, a language's,
/// region's, script's, currency's or calendar's; NULL for others.
///
/// # Safety
///
/// `display` is a locale; `key` and `value` null or strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFLocaleCopyDisplayNameForPropertyValue(
    _display: *const c_void,
    key: *const c_void,
    value: *const c_void,
) -> *mut c_void {
    let (Some(key), Some(value)) = (string_of(key), string_of(value)) else { return std::ptr::null_mut() };
    let name = match key.as_str() {
        "kCFLocaleIdentifierKey" => identifier_name(&value),
        "kCFLocaleLanguageCodeKey" => name_in(data::LANGUAGE_NAMES, &value).map(str::to_string),
        "kCFLocaleCountryCodeKey" => name_in(data::COUNTRY_NAMES, &value).map(str::to_string),
        "kCFLocaleScriptCodeKey" => name_in(data::SCRIPT_NAMES, &value).map(str::to_string),
        "currency" => name_in(data::CURRENCY_NAMES, &value).map(str::to_string),
        "kCFLocaleCurrencySymbolKey" => currency_symbol(&value),
        "calendar" => name_in(data::CALENDAR_NAMES, &value).map(str::to_string),
        _ => None,
    };
    name.map_or(std::ptr::null_mut(), |n| owned(NSString::from_str(&n)))
}

/// A currency's symbol in English: its own for those that have one, else
/// its code (none for `LSM`), as macOS gives them.
fn currency_symbol(code: &str) -> Option<String> {
    const SYMBOLS: [(&str, &str); 23] = [
        ("AUD", "A$"),
        ("BRL", "R$"),
        ("CAD", "CA$"),
        ("CNY", "CN¥"),
        ("EUR", "€"),
        ("GBP", "£"),
        ("HKD", "HK$"),
        ("ILS", "₪"),
        ("INR", "₹"),
        ("JPY", "¥"),
        ("KRW", "₩"),
        ("MXN", "MX$"),
        ("NZD", "NZ$"),
        ("PHP", "₱"),
        ("TWD", "NT$"),
        ("USD", "$"),
        ("VND", "₫"),
        ("XAF", "FCFA"),
        ("XCD", "EC$"),
        ("XCG", "Cg."),
        ("XOF", "F\u{202f}CFA"),
        ("XPF", "CFPF"),
        ("XXX", "¤"),
    ];
    if let Some((_, symbol)) = SYMBOLS.iter().find(|(c, _)| *c == code) {
        return Some(symbol.to_string());
    }
    (code != "LSM" && data::ISO_CURRENCIES.contains(&code)).then(|| code.to_string())
}

/// Language codes that were replaced, and their replacements.
const REPLACED: [(&str, &str); 5] = [("iw", "he"), ("in", "id"), ("ji", "yi"), ("no", "nb"), ("tl", "fil")];

fn is_script(subtag: &str) -> bool {
    subtag.len() == 4 && subtag.chars().all(|c| c.is_ascii_alphabetic())
}

fn is_region(subtag: &str) -> bool {
    (subtag.len() == 2 && subtag.chars().all(|c| c.is_ascii_alphabetic()))
        || (subtag.len() == 3 && subtag.chars().all(|c| c.is_ascii_digit()))
}

/// Leave out a script that goes without saying, as macOS does: the
/// language's default script (`sr_Cyrl_RS` is `sr_RS`, `en-Latn-US` the
/// language `en-US`), and in a locale identifier written with
/// underscores, simplified Han in China and Singapore and traditional Han
/// in Hong Kong, Macao and Taiwan (`zh_Hant_TW` is `zh_TW`, `yue_Hant_HK`
/// is `yue_HK`). A locale identifier keeps its script when a hyphen
/// separates it from the region (`zh_Hant-TW`) or the region isn't one
/// (`zh_Hans_CN.UTF-8`).
fn drop_default_script(pieces: &mut Vec<(Option<char>, &str)>, language: bool) {
    let [(_, lang), (_, script), ..] = pieces[..] else { return };
    if !is_script(script) {
        return;
    }
    let lang = lang.to_ascii_lowercase();
    let lang = REPLACED.iter().find(|(old, _)| *old == lang).map_or(lang.as_str(), |(_, new)| new);
    let script = script[..1].to_ascii_uppercase() + &script[1..].to_ascii_lowercase();
    let region = pieces.get(2).copied();
    let drop = if language {
        data::DEFAULT_SCRIPTS.iter().any(|(l, s)| *l == lang && *s == script)
    } else {
        let region = match region {
            None if pieces[1].0 == Some('_') => Some(""),
            None => None,
            Some((Some('_'), r)) if is_region(r) => Some(r),
            Some(_) => None,
        };
        region.is_some_and(|r| {
            let r = r.to_ascii_uppercase();
            data::DEFAULT_SCRIPTS.iter().any(|(l, s)| *l == lang && *s == script)
                || (script == "Hans" && matches!(r.as_str(), "CN" | "SG"))
                || (script == "Hant" && matches!(r.as_str(), "HK" | "MO" | "TW"))
        })
    };
    if drop {
        let removed = pieces.remove(1);
        // The region keeps the script's separator when there was nothing
        // between them: `zh-Hant_TW` is `zh_TW`, `sr_Cyrl` is `sr`.
        if let Some(next) = pieces.get_mut(1) {
            next.0 = if language { removed.0 } else { next.0 };
        }
    }
}

/// An identifier with each subtag's case made canonical and replaced
/// language codes replaced; `language` joins subtags with `-` (and ends
/// at an empty one), else the separators stay as given.
pub(crate) fn canonical(identifier: &str, language: bool) -> String {
    let (base, keywords) = match identifier.split_once('@') {
        Some((base, keywords)) => (base, Some(keywords)),
        None => (identifier, None),
    };
    // A language identifier has no encoding (`.UTF-8`).
    let base = if language { base.split('.').next().unwrap_or(base) } else { base };
    // The subtags, each with the separator before it.
    let mut pieces: Vec<(Option<char>, &str)> = Vec::new();
    let (mut separator, mut start) = (None, 0);
    for (i, c) in base.char_indices() {
        if c == '-' || c == '_' {
            pieces.push((separator, &base[start..i]));
            (separator, start) = (Some(c), i + 1);
        }
    }
    pieces.push((separator, &base[start..]));
    drop_default_script(&mut pieces, language);
    let mut out = String::with_capacity(identifier.len());
    // An extension (`-u-…`, `-x-…`) stays as it is.
    let mut verbatim = false;
    for (index, (separator, subtag)) in pieces.into_iter().enumerate() {
        if language && index > 0 && subtag.is_empty() {
            break;
        }
        let piece = if verbatim {
            subtag.to_string()
        } else if index == 0 {
            let lower = subtag.to_ascii_lowercase();
            REPLACED.iter().find(|(old, _)| *old == lower).map_or(lower, |(_, new)| new.to_string())
        } else if subtag.len() == 1 {
            verbatim = true;
            subtag.to_ascii_lowercase()
        } else if index == 1 && subtag.len() == 4 && subtag.chars().all(|c| c.is_ascii_alphabetic()) {
            subtag[..1].to_ascii_uppercase() + &subtag[1..].to_ascii_lowercase()
        } else {
            subtag.to_ascii_uppercase()
        };
        if let Some(separator) = separator {
            // A script joins its language with a hyphen.
            let script = index == 1 && is_script(subtag);
            out.push(if language || script { '-' } else { separator });
        }
        out.push_str(&piece);
    }
    if let Some(keywords) = keywords {
        out.push('@');
        let pairs: Vec<String> = keywords
            .split(';')
            .map(|pair| match pair.split_once('=') {
                Some((k, v)) => format!("{}={v}", k.to_ascii_lowercase()),
                None => pair.to_ascii_lowercase(),
            })
            .collect();
        out.push_str(&pairs.join(";"));
    }
    out
}

/// # Safety
///
/// `identifier` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFLocaleCreateCanonicalLanguageIdentifierFromString(
    _alloc: *const c_void,
    identifier: *const c_void,
) -> *mut c_void {
    let Some(identifier) = string_of(identifier) else { return std::ptr::null_mut() };
    owned(NSString::from_str(&canonical(&identifier, true)))
}

/// # Safety
///
/// `identifier` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFLocaleCreateCanonicalLocaleIdentifierFromString(
    _alloc: *const c_void,
    identifier: *const c_void,
) -> *mut c_void {
    let Some(identifier) = string_of(identifier) else { return std::ptr::null_mut() };
    owned(NSString::from_str(&canonical(&identifier, false)))
}

/// The identifier old Mac OS language and region codes name: the region's
/// if it has one, else the language's.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleCreateCanonicalLocaleIdentifierFromScriptManagerCodes(
    _alloc: *const c_void,
    language: i16,
    region: i16,
) -> *mut c_void {
    let found = data::SCRIPT_MANAGER_REGIONS
        .iter()
        .find(|(r, _)| *r == region)
        .or_else(|| data::SCRIPT_MANAGER_LANGUAGES.iter().find(|(l, _)| *l == language));
    found.map_or(std::ptr::null_mut(), |(_, id)| owned(NSString::from_str(id)))
}

/// # Safety
///
/// `identifier` is null or a locale identifier.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFLocaleCreateComponentsFromLocaleIdentifier(
    _alloc: *const c_void,
    identifier: *const c_void,
) -> *mut c_void {
    let Some(identifier) = string_of(identifier) else { return std::ptr::null_mut() };
    let mut pairs: Vec<(String, String)> = Vec::new();
    let (base, keywords) = identifier.split_once('@').unwrap_or((&identifier, ""));
    let p = crate::locale::parts(base);
    // In canonical case, as macOS gives them: the language lowercase, the
    // script titlecase, the region and variant uppercase, keywords' names
    // lowercase (their values as they are).
    if !p.language.is_empty() {
        pairs.push(("kCFLocaleLanguageCodeKey".into(), p.language.to_ascii_lowercase()));
    }
    let title = |s: &str| s[..1].to_ascii_uppercase() + &s[1..].to_ascii_lowercase();
    for (key, value) in [
        ("kCFLocaleScriptCodeKey", p.script.filter(|s| !s.is_empty()).map(title)),
        ("kCFLocaleCountryCodeKey", p.region.map(str::to_ascii_uppercase)),
        ("kCFLocaleVariantCodeKey", p.variant.map(str::to_ascii_uppercase)),
    ] {
        if let Some(value) = value {
            pairs.push((key.into(), value));
        }
    }
    for pair in keywords.split(';').filter(|p| !p.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            pairs.push((k.to_ascii_lowercase(), v.into()));
        }
    }
    let keys: Vec<Retained<NSString>> = pairs.iter().map(|(k, _)| NSString::from_str(k)).collect();
    let values: Vec<Retained<AnyObject>> = pairs.iter().map(|(_, v)| NSString::from_str(v).into()).collect();
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    owned(NSDictionary::from_retained_objects(&keys, &values))
}

/// # Safety
///
/// `components` is null or a dictionary of strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFLocaleCreateLocaleIdentifierFromComponents(
    _alloc: *const c_void,
    components: *const c_void,
) -> *mut c_void {
    if components.is_null() {
        return std::ptr::null_mut();
    }
    let mut parts: [Option<String>; 4] = Default::default();
    let mut keywords: Vec<(String, String)> = Vec::new();
    // SAFETY: per this function's contract.
    for (key, value) in crate::plist::dictionary_entries(unsafe { &*components.cast::<NSDictionary>() }) {
        let (key, value) = (text(&key).into_owned(), text(&value).into_owned());
        match key.as_str() {
            "kCFLocaleLanguageCodeKey" => parts[0] = Some(value),
            "kCFLocaleScriptCodeKey" => parts[1] = Some(value),
            "kCFLocaleCountryCodeKey" => parts[2] = Some(value),
            "kCFLocaleVariantCodeKey" => parts[3] = Some(value),
            _ => keywords.push((key, value)),
        }
    }
    let mut out: String = parts.iter().flatten().cloned().collect::<Vec<_>>().join("_");
    if !keywords.is_empty() {
        keywords.sort();
        out.push('@');
        out.push_str(&keywords.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";"));
    }
    owned(NSString::from_str(&out))
}

/// The identifier a Windows locale code names; a sublanguage it doesn't
/// know names its language's.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleCreateLocaleIdentifierFromWindowsLocaleCode(
    _alloc: *const c_void,
    code: u32,
) -> *mut c_void {
    let code = code & 0xffff;
    let find = |c: u32| data::WINDOWS_CODES.iter().find(|(k, _)| *k == c).map(|&(_, id)| id);
    find(code).or_else(|| find(code & 0x3ff)).map_or(std::ptr::null_mut(), |id| owned(NSString::from_str(id)))
}

/// A locale identifier's Windows code: its own, its without a script, or
/// its language's; 0 for none.
///
/// # Safety
///
/// `identifier` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFLocaleGetWindowsLocaleCodeFromLocaleIdentifier(identifier: *const c_void) -> u32 {
    let Some(identifier) = string_of(identifier) else { return 0 };
    let find = |id: &str| data::WINDOWS_CODES.iter().find(|(_, i)| *i == id).map(|&(c, _)| c);
    let p = crate::locale::parts(&identifier);
    let without_script = match p.region {
        Some(region) => format!("{}_{region}", p.language),
        None => p.language.to_string(),
    };
    find(&identifier).or_else(|| find(&without_script)).or_else(|| find(p.language)).unwrap_or(0)
}

/// Right to left (2) for the languages written so, else left to right (1).
///
/// # Safety
///
/// `language` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFLocaleGetLanguageCharacterDirection(language: *const c_void) -> CFIndex {
    const RIGHT_TO_LEFT: [&str; 18] = [
        "ar", "ars", "ckb", "dv", "fa", "he", "ks", "lrc", "mid", "mzn", "nqo", "ps", "rhg", "sd", "syr", "ug", "ur",
        "yi",
    ];
    let code = string_of(language).unwrap_or_default();
    let base = crate::locale::parts(&code).language.to_ascii_lowercase();
    if RIGHT_TO_LEFT.contains(&base.as_str()) { 2 } else { 1 }
}

/// Top to bottom (3), for every language, as on macOS.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFLocaleGetLanguageLineDirection(_language: *const c_void) -> CFIndex {
    3
}

/// The value of one of the keys `NSLocale` doesn't answer itself, for
/// `-objectForKey:`: `None` for keys not handled here.
pub(crate) fn extra_value(locale: &NSLocale, identifier: &str, key: &str) -> Option<Option<Retained<AnyObject>>> {
    let p = crate::locale::parts(identifier);
    let keyword = |name: &str| {
        identifier.split_once('@').and_then(|(_, k)| {
            k.split(';')
                .find_map(|pair| pair.split_once('=').filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v))
        })
    };
    let region = p.region.map(str::to_ascii_uppercase);
    let region_data = region.as_deref().and_then(|r| data::REGIONS.iter().find(|entry| entry.0 == r));
    let text = |s: &str| Some(Some(NSString::from_str(s).into()));
    let language = p.language.to_ascii_lowercase();
    let quote = |i: usize| {
        let marks = data::QUOTES.iter().find(|(l, _)| *l == language).map_or(["“", "”", "‘", "’"], |(_, q)| *q);
        text(marks[i])
    };
    match key {
        "calendar" => text(keyword("calendar").unwrap_or("gregorian")),
        // A calendar Sidestep doesn't have (Buddhist, Hebrew, …): a
        // Gregorian one rather than none (see docs/abi.md).
        "kCFLocaleCalendarKey" => Some(
            super::calendar::for_locale(locale, keyword("calendar").unwrap_or("gregorian"))
                .or_else(|| super::calendar::for_locale(locale, "gregorian")),
        ),
        "collation" => text(keyword("collation").unwrap_or("standard")),
        "kCFLocaleCollatorIdentifierKey" => text(identifier),
        "kCFLocaleUsesMetricSystemKey" => {
            let metric = region_data.is_none_or(|r| r.3 == "Metric") && !identifier.is_empty();
            Some(Some(objc2_foundation::NSNumber::new_bool(metric).into()))
        }
        "kCFLocaleMeasurementSystemKey" => match (region_data, identifier.is_empty()) {
            (_, true) => text("U.S."),
            (Some(r), _) => text(r.3),
            (None, _) => text("Metric"),
        },
        // A `currency` keyword's code, uppercase, and its symbol (in
        // English) over the region's.
        "currency" => match (keyword("currency"), region_data) {
            (Some(code), _) => text(&code.to_ascii_uppercase()),
            (None, Some(r)) if !r.1.is_empty() => text(r.1),
            _ => Some(None),
        },
        "kCFLocaleCurrencySymbolKey" => match (keyword("currency"), region_data) {
            (Some(code), _) => {
                text(&currency_symbol(&code.to_ascii_uppercase()).unwrap_or_else(|| code.to_ascii_uppercase()))
            }
            (None, Some(r)) if !r.2.is_empty() => text(r.2),
            _ => text("\u{a4}"),
        },
        "kCFLocaleQuotationBeginDelimiterKey" => quote(0),
        "kCFLocaleQuotationEndDelimiterKey" => quote(1),
        "kCFLocaleAlternateQuotationBeginDelimiterKey" => quote(2),
        "kCFLocaleAlternateQuotationEndDelimiterKey" => quote(3),
        "kCFLocaleExemplarCharacterSetKey" => Some(Some(exemplars(&p).into())),
        _ => None,
    }
}

/// The letters a locale's language is written with: its script's or
/// region's where those differ, else its language's; English's for a
/// language macOS has none for, and none for the system locale.
fn exemplars(p: &crate::locale::Parts<'_>) -> Retained<objc2_foundation::NSCharacterSet> {
    use super::locale_exemplars::{EXEMPLARS, IMPLIED};
    let find = |table: &'static [(&'static str, &'static str)], key: &str| {
        table.binary_search_by(|(k, _)| (*k).cmp(key)).ok().map(|i| table[i].1)
    };
    let language = p.language.to_ascii_lowercase();
    let script = p.script.map(|s| s[..1].to_ascii_uppercase() + &s[1..].to_ascii_lowercase());
    let region = p.region.map(str::to_ascii_uppercase);
    let mut keys = Vec::new();
    if let Some(script) = &script {
        if let Some(region) = &region {
            keys.push(format!("{language}_{script}_{region}"));
        }
        keys.push(format!("{language}_{script}"));
    } else if let Some(region) = &region {
        let key = format!("{language}_{region}");
        keys.extend(find(IMPLIED, &key).map(str::to_string));
        keys.push(key);
    }
    keys.push(language.clone());
    let ranges = match keys.iter().find_map(|k| find(EXEMPLARS, k)) {
        Some(ranges) => ranges,
        None if language.is_empty() => "",
        None => "41-5a,61-7a",
    };
    let set = objc2_foundation::NSMutableCharacterSet::new();
    for range in ranges.split(',').filter(|r| !r.is_empty()) {
        let (first, last) = range.split_once('-').unwrap_or((range, range));
        let (Ok(first), Ok(last)) = (usize::from_str_radix(first, 16), usize::from_str_radix(last, 16)) else {
            continue;
        };
        set.addCharactersInRange(objc2_foundation::NSRange::new(first, last - first + 1));
    }
    Retained::into_super(set)
}

#[cfg(test)]
mod tests {
    use super::canonical;

    #[test]
    fn canonical_identifiers() {
        let cases = [
            ("en-us", "en-US", "en-US"),
            ("EN_us", "en-US", "en_US"),
            ("zh-hant-tw", "zh-Hant-TW", "zh-Hant-TW"),
            ("iw", "he", "he"),
            ("no", "nb", "nb"),
            ("en_US@calendar=japanese", "en-US@calendar=japanese", "en_US@calendar=japanese"),
            ("EN@CALENDAR=JAPANESE", "en@calendar=JAPANESE", "en@calendar=JAPANESE"),
            ("de-DE-u-co-phonebk", "de-DE-u-co-phonebk", "de-DE-u-co-phonebk"),
            ("es-419", "es-419", "es-419"),
            ("en_us_posix", "en-US-POSIX", "en_US_POSIX"),
            ("", "", ""),
            ("zh_Hant_TW", "zh-Hant-TW", "zh_TW"),
            ("zh-Hant_TW", "zh-Hant-TW", "zh_TW"),
            ("zh_Hant-TW", "zh-Hant-TW", "zh-Hant-TW"),
            ("zh_Hans", "zh-Hans", "zh-Hans"),
            ("zh_Hani", "zh", "zh"),
            ("sr_Cyrl_RS", "sr-RS", "sr_RS"),
            ("sr_Latn_RS", "sr-Latn-RS", "sr-Latn_RS"),
            ("sr_Cyrl", "sr", "sr"),
            ("sr_Cyrl_RS@calendar=japanese", "sr-RS@calendar=japanese", "sr_RS@calendar=japanese"),
            ("en_Latn_CN", "en-CN", "en_CN"),
            ("en_Hans_CN", "en-Hans-CN", "en_CN"),
            ("ko_Kore_KR", "ko-Kore-KR", "ko-Kore_KR"),
            ("ja_Jpan_JP", "ja-JP", "ja_JP"),
            ("yue_Hant_HK", "yue-Hant-HK", "yue_HK"),
            ("zh_Hans_CN.UTF-8", "zh-Hans-CN", "zh-Hans_CN.UTF-8"),
            ("en_Latn_US_POSIX", "en-US-POSIX", "en_US_POSIX"),
        ];
        for (input, language, locale) in cases {
            assert_eq!(canonical(input, true), language, "{input}");
            assert_eq!(canonical(input, false), locale, "{input}");
        }
    }
}
