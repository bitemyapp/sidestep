//! Case mapping, normalization and folding.
//!
//! Case mapping is Unicode's full mapping from ICU4X (`ß` uppercases to
//! `SS`, `İ` lowercases to `i` + U+0307) with no locale tailoring: the
//! localized and `…WithLocale:` forms take the same path until Sidestep has
//! an NSLocale. `capitalizedString` titlecases the first letter of each word
//! and lowercases the rest; a word is a run of letters, marks and
//! apostrophes, so digits and other punctuation start new words.
//!
//! Lone surrogates pass through every mapping unchanged.

use icu_casemap::CaseMapperBorrowed;
use icu_casemap::options::TitlecaseOptions;
use icu_locale_core::LanguageIdentifier;
use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use icu_properties::props::GeneralCategoryGroup;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{ClassType, define_class};
use objc2_foundation::{NSString, NSStringCompareOptions};

use super::fold::{self, Piece, general_category};
use super::view::view;
use super::{inline, wtf8};

/// Apply `f` to each UTF-8 stretch of a string, keeping lone surrogates.
fn map(obj: &AnyObject, f: impl Fn(&str) -> String) -> Retained<NSString> {
    let v = view(obj);
    let t = v.text();
    let mut out = Vec::with_capacity(t.bytes.len());
    fold::for_each_piece(t.bytes, |piece| match piece {
        Piece::Str(s) => wtf8::push(&mut out, f(s).as_bytes()),
        Piece::Surrogate(u) => {
            let mut b = Vec::with_capacity(3);
            wtf8::encode(u, &mut b);
            wtf8::push(&mut out, &b);
        }
    });
    if out == t.bytes {
        return super::search::keep(obj, &v);
    }
    let flags = wtf8::flags_of(&out, true);
    inline::new(&out, wtf8::utf16_len(&out), flags)
}

fn root() -> LanguageIdentifier {
    LanguageIdentifier::UNKNOWN
}

pub(crate) fn uppercase(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_uppercase();
    }
    CaseMapperBorrowed::new().uppercase_to_string(s, &root()).into_owned()
}

pub(crate) fn lowercase(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    CaseMapperBorrowed::new().lowercase_to_string(s, &root()).into_owned()
}

/// Whether `c` continues a word for `capitalizedString`.
fn in_word(c: char) -> bool {
    matches!(c, '\'' | '\u{2019}')
        || GeneralCategoryGroup::Letter.union(GeneralCategoryGroup::Mark).contains(general_category(u32::from(c)))
}

pub(crate) fn capitalize(s: &str) -> String {
    let cm = CaseMapperBorrowed::new();
    let mut out = String::with_capacity(s.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if !word.is_empty() {
            out.push_str(&cm.titlecase_segment_with_only_case_data_to_string(
                word,
                &root(),
                TitlecaseOptions::default(),
            ));
            word.clear();
        }
    };
    for c in s.chars() {
        if in_word(c) {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// `stringByFoldingWithOptions:locale:`: the folds asked for, recomposed.
fn fold_with(s: &str, options: usize) -> String {
    let folded =
        fold::fold_str(s, options & (fold::CASE_INSENSITIVE | fold::DIACRITIC_INSENSITIVE | fold::WIDTH_INSENSITIVE));
    fold::compose(&folded)
}

fn this(obj: &Helper) -> &AnyObject {
    obj
}

define_class!(
    // NSString's case and normalization methods, copied onto NSString when
    // it loads.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepStringCase"]
    pub(crate) struct Helper;

    impl Helper {
        #[unsafe(method_id(uppercaseString))]
        fn uppercase_string(&self) -> Retained<NSString> {
            map(this(self), uppercase)
        }

        #[unsafe(method_id(lowercaseString))]
        fn lowercase_string(&self) -> Retained<NSString> {
            map(this(self), lowercase)
        }

        #[unsafe(method_id(capitalizedString))]
        fn capitalized_string(&self) -> Retained<NSString> {
            map(this(self), capitalize)
        }

        #[unsafe(method_id(localizedUppercaseString))]
        fn localized_uppercase_string(&self) -> Retained<NSString> {
            map(this(self), uppercase)
        }

        #[unsafe(method_id(localizedLowercaseString))]
        fn localized_lowercase_string(&self) -> Retained<NSString> {
            map(this(self), lowercase)
        }

        #[unsafe(method_id(localizedCapitalizedString))]
        fn localized_capitalized_string(&self) -> Retained<NSString> {
            map(this(self), capitalize)
        }

        #[unsafe(method_id(uppercaseStringWithLocale:))]
        fn uppercase_with_locale(&self, _locale: Option<&AnyObject>) -> Retained<NSString> {
            map(this(self), uppercase)
        }

        #[unsafe(method_id(lowercaseStringWithLocale:))]
        fn lowercase_with_locale(&self, _locale: Option<&AnyObject>) -> Retained<NSString> {
            map(this(self), lowercase)
        }

        #[unsafe(method_id(capitalizedStringWithLocale:))]
        fn capitalized_with_locale(&self, _locale: Option<&AnyObject>) -> Retained<NSString> {
            map(this(self), capitalize)
        }

        #[unsafe(method_id(decomposedStringWithCanonicalMapping))]
        fn nfd(&self) -> Retained<NSString> {
            map(this(self), |s| DecomposingNormalizerBorrowed::new_nfd().normalize(s).into_owned())
        }

        #[unsafe(method_id(precomposedStringWithCanonicalMapping))]
        fn nfc(&self) -> Retained<NSString> {
            map(this(self), |s| ComposingNormalizerBorrowed::new_nfc().normalize(s).into_owned())
        }

        #[unsafe(method_id(decomposedStringWithCompatibilityMapping))]
        fn nfkd(&self) -> Retained<NSString> {
            map(this(self), |s| DecomposingNormalizerBorrowed::new_nfkd().normalize(s).into_owned())
        }

        #[unsafe(method_id(precomposedStringWithCompatibilityMapping))]
        fn nfkc(&self) -> Retained<NSString> {
            map(this(self), |s| ComposingNormalizerBorrowed::new_nfkc().normalize(s).into_owned())
        }

        #[unsafe(method_id(stringByFoldingWithOptions:locale:))]
        fn folding(&self, options: NSStringCompareOptions, _locale: Option<&AnyObject>) -> Retained<NSString> {
            map(this(self), |s| fold_with(s, options.0))
        }
    }
);

/// Add the case methods to NSString.
pub(crate) fn install(target: &AnyClass) {
    super::install::copy_methods(Helper::class(), target, false);
}
