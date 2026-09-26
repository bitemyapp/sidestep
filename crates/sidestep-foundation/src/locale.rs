//! `NSLocale`: an identifier and what it says.
//!
//! The current locale comes from the POSIX environment: `$LC_ALL`, then
//! `$LC_TIME`, then `$LANG`, with the encoding and modifier dropped
//! (`fr_FR.UTF-8@euro` is `fr_FR`) and `C` or `POSIX` meaning
//! `en_US_POSIX`. Identifiers given to `localeWithLocaleIdentifier:` are
//! kept as given, as on macOS. Formatting data is English: Sidestep's
//! date formatter only has English symbols.

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSLocale, NSString, NSUInteger, NSZone};

use crate::runloop::modes::exported_strings;

sidestep_runtime::static_class!(pub(crate) NSLOCALE, NSLOCALE_META = "NSLocale", || {
    let _ = NSLocaleImpl::class();
    crate::perform::install();
});

exported_strings! {
    NSLocaleIdentifier, pub(crate) IDENTIFIER = "kCFLocaleIdentifierKey";
    NSLocaleLanguageCode, pub(crate) LANGUAGE_CODE = "kCFLocaleLanguageCodeKey";
    NSLocaleCountryCode, pub(crate) COUNTRY_CODE = "kCFLocaleCountryCodeKey";
    NSLocaleScriptCode, pub(crate) SCRIPT_CODE = "kCFLocaleScriptCodeKey";
    NSLocaleVariantCode, pub(crate) VARIANT_CODE = "kCFLocaleVariantCodeKey";
    NSLocaleDecimalSeparator, pub(crate) DECIMAL_SEPARATOR = "kCFLocaleDecimalSeparatorKey";
    NSLocaleGroupingSeparator, pub(crate) GROUPING_SEPARATOR = "kCFLocaleGroupingSeparatorKey";
    NSLocaleUsesMetricSystem, pub(crate) USES_METRIC_SYSTEM = "kCFLocaleUsesMetricSystemKey";
    NSCurrentLocaleDidChangeNotification, CURRENT_LOCALE_DID_CHANGE = "kCFLocaleCurrentLocaleDidChangeNotification";
}

/// The parts of an identifier: language, script, region, variant.
pub(crate) struct Parts<'a> {
    pub(crate) language: &'a str,
    pub(crate) script: Option<&'a str>,
    pub(crate) region: Option<&'a str>,
    pub(crate) variant: Option<&'a str>,
}

pub(crate) fn parts(identifier: &str) -> Parts<'_> {
    let base = identifier.split(['.', '@']).next().unwrap_or("");
    let mut pieces = base.split(['_', '-']);
    let language = pieces.next().unwrap_or("");
    let mut script = None;
    let mut region = None;
    let mut variant = None;
    for piece in pieces {
        if script.is_none() && region.is_none() && piece.len() == 4 && piece.chars().all(|c| c.is_ascii_alphabetic()) {
            script = Some(piece);
        } else if region.is_none() && (piece.len() == 2 || piece.len() == 3) {
            region = Some(piece);
        } else if !piece.is_empty() {
            variant = Some(piece);
        }
    }
    Parts { language, script, region, variant }
}

/// The locale the environment names.
pub(crate) fn current_identifier() -> String {
    let value = ["LC_ALL", "LC_TIME", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    let base = value.split(['.', '@']).next().unwrap_or("");
    match base {
        "" => "en_US".into(),
        "C" | "POSIX" => "en_US_POSIX".into(),
        base => base.replace('-', "_"),
    }
}

pub(crate) struct LocaleIvars {
    identifier: String,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSLocale"]
    #[ivars = LocaleIvars]
    pub(crate) struct NSLocaleImpl;

    impl NSLocaleImpl {
        #[unsafe(method_id(localeWithLocaleIdentifier:))]
        fn with_identifier(identifier: &NSString) -> Retained<Self> {
            make(identifier.to_string())
        }

        #[unsafe(method_id(initWithLocaleIdentifier:))]
        fn init_with_identifier(this: Allocated<Self>, identifier: &NSString) -> Retained<Self> {
            init(this, identifier.to_string())
        }

        #[unsafe(method_id(init))]
        fn init_empty(this: Allocated<Self>) -> Retained<Self> {
            init(this, String::new())
        }

        #[unsafe(method_id(currentLocale))]
        fn current_locale() -> Retained<Self> {
            make(current_identifier())
        }

        #[unsafe(method_id(autoupdatingCurrentLocale))]
        fn autoupdating_current_locale() -> Retained<Self> {
            make(current_identifier())
        }

        #[unsafe(method_id(systemLocale))]
        fn system_locale() -> Retained<Self> {
            make(String::new())
        }

        #[unsafe(method_id(preferredLanguages))]
        fn preferred_languages() -> Retained<AnyObject> {
            let id = current_identifier();
            let p = parts(&id);
            let language = match p.region {
                Some(region) => format!("{}-{region}", p.language),
                None => p.language.to_string(),
            };
            objc2_foundation::NSArray::from_retained_slice(&[NSString::from_str(&language)]).into()
        }

        #[unsafe(method_id(canonicalLocaleIdentifierFromString:))]
        fn canonical_locale_identifier(string: &NSString) -> Retained<NSString> {
            let text = string.to_string();
            let p = parts(&text);
            let mut out = p.language.to_ascii_lowercase();
            if let Some(script) = p.script {
                out.push('_');
                out.push_str(&script[..1].to_ascii_uppercase());
                out.push_str(&script[1..].to_ascii_lowercase());
            }
            if let Some(region) = p.region {
                out.push('_');
                out.push_str(&region.to_ascii_uppercase());
            }
            if let Some(variant) = p.variant {
                out.push('_');
                out.push_str(&variant.to_ascii_uppercase());
            }
            NSString::from_str(&out)
        }

        #[unsafe(method_id(localeIdentifier))]
        fn locale_identifier(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().identifier)
        }

        #[unsafe(method_id(languageCode))]
        fn language_code(&self) -> Retained<NSString> {
            NSString::from_str(parts(&self.ivars().identifier).language)
        }

        #[unsafe(method_id(languageIdentifier))]
        fn language_identifier(&self) -> Retained<NSString> {
            let p = parts(&self.ivars().identifier);
            let mut out = p.language.to_string();
            if let Some(script) = p.script {
                out.push('-');
                out.push_str(script);
            }
            NSString::from_str(&out)
        }

        #[unsafe(method_id(countryCode))]
        fn country_code(&self) -> Option<Retained<NSString>> {
            parts(&self.ivars().identifier).region.map(NSString::from_str)
        }

        #[unsafe(method_id(regionCode))]
        fn region_code(&self) -> Option<Retained<NSString>> {
            parts(&self.ivars().identifier).region.map(NSString::from_str)
        }

        #[unsafe(method_id(scriptCode))]
        fn script_code(&self) -> Option<Retained<NSString>> {
            parts(&self.ivars().identifier).script.map(NSString::from_str)
        }

        #[unsafe(method_id(variantCode))]
        fn variant_code(&self) -> Option<Retained<NSString>> {
            parts(&self.ivars().identifier).variant.map(NSString::from_str)
        }

        #[unsafe(method_id(calendarIdentifier))]
        fn calendar_identifier(&self) -> Retained<NSString> {
            NSString::from_str("gregorian")
        }

        #[unsafe(method(usesMetricSystem))]
        fn uses_metric_system(&self) -> bool {
            !matches!(parts(&self.ivars().identifier).region, Some("US" | "LR" | "MM"))
        }

        #[unsafe(method_id(decimalSeparator))]
        fn decimal_separator(&self) -> Retained<NSString> {
            NSString::from_str(separators(&self.ivars().identifier).0)
        }

        #[unsafe(method_id(groupingSeparator))]
        fn grouping_separator(&self) -> Retained<NSString> {
            NSString::from_str(separators(&self.ivars().identifier).1)
        }

        #[unsafe(method_id(quotationBeginDelimiter))]
        fn quotation_begin_delimiter(&self) -> Retained<NSString> {
            NSString::from_str("\u{201c}")
        }

        #[unsafe(method_id(quotationEndDelimiter))]
        fn quotation_end_delimiter(&self) -> Retained<NSString> {
            NSString::from_str("\u{201d}")
        }

        #[unsafe(method_id(objectForKey:))]
        fn object_for_key(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            self.value_for(&key.to_string())
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSLocale>()).is_some_and(|o| identifier_of(o) == self.ivars().identifier)
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            crate::string::hash_str(&self.ivars().identifier)
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&format!("{} (fixed)", self.ivars().identifier))
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSLocaleImpl {}
);

impl NSLocaleImpl {
    fn value_for(&self, key: &str) -> Option<Retained<AnyObject>> {
        let id = &self.ivars().identifier;
        let p = parts(id);
        let text = |s: &str| Some(NSString::from_str(s).into());
        match key {
            "kCFLocaleIdentifierKey" => text(id),
            "kCFLocaleLanguageCodeKey" => text(p.language),
            "kCFLocaleCountryCodeKey" => p.region.and_then(text),
            "kCFLocaleScriptCodeKey" => p.script.and_then(text),
            "kCFLocaleVariantCodeKey" => p.variant.and_then(text),
            "kCFLocaleDecimalSeparatorKey" => text(separators(id).0),
            "kCFLocaleGroupingSeparatorKey" => text(separators(id).1),
            _ => None,
        }
    }
}

/// Decimal and grouping separators, for the languages that differ from
/// English.
fn separators(identifier: &str) -> (&'static str, &'static str) {
    match parts(identifier).language {
        "de" | "es" | "it" | "nl" | "pt" | "da" | "id" | "tr" => (",", "."),
        "fr" | "ru" | "pl" | "cs" | "sv" | "fi" | "nb" | "uk" => (",", "\u{a0}"),
        _ => (".", ","),
    }
}

/// A locale's identifier.
pub(crate) fn identifier_of(locale: &NSLocale) -> &str {
    // SAFETY: every NSLocale is an instance of this class.
    &unsafe { &*(locale as *const NSLocale).cast::<NSLocaleImpl>() }.ivars().identifier
}

fn init(this: Allocated<NSLocaleImpl>, identifier: String) -> Retained<NSLocaleImpl> {
    let this = this.set_ivars(LocaleIvars { identifier });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn make(identifier: String) -> Retained<NSLocaleImpl> {
    // SAFETY: +alloc through the binding loads the class.
    let this: Allocated<NSLocaleImpl> = unsafe { msg_send![NSLocale::class(), alloc] };
    init(this, identifier)
}

/// A locale object for Rust code.
pub(crate) fn object(identifier: &str) -> Retained<NSLocale> {
    // SAFETY: NSLocaleImpl is the class NSLocale names.
    unsafe { Retained::cast_unchecked(make(identifier.to_string())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_parts() {
        let p = parts("zh_Hant_TW");
        assert_eq!((p.language, p.script, p.region), ("zh", Some("Hant"), Some("TW")));
        let p = parts("fr_FR.UTF-8@euro");
        assert_eq!((p.language, p.region, p.variant), ("fr", Some("FR"), None));
        let p = parts("en_US_POSIX");
        assert_eq!((p.language, p.region, p.variant), ("en", Some("US"), Some("POSIX")));
        assert_eq!(parts("en-GB").region, Some("GB"));
    }
}
