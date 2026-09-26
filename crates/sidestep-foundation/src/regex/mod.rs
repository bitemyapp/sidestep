//! Regular expressions, for `NSRegularExpressionSearch` and
//! `NSRegularExpression`, on fancy-regex.
//!
//! Foundation's patterns are ICU's, which `translate` rewrites where
//! fancy-regex reads them differently. Replacement templates use ICU's `$n`
//! and `\` escapes.
//!
//! Matching runs on UTF-8. Text holding a lone surrogate is matched with
//! U+FFFD in its place, which takes the same three bytes and one UTF-16
//! unit, so match offsets carry over unchanged.
//!
//! A search covers a range of its string, as ICU's region does: matches lie
//! inside it, and by default `^` and `$` match at its ends (anchoring
//! bounds) and `\b` and look-ahead don't see past them (opaque bounds),
//! while look-behind sees what comes before, as on macOS. `walk` gives the
//! engine as much of the string as the pattern's constructs need to answer
//! as ICU would. The flags reported with each match (hit end, required
//! end) are worked out from the pattern's shape rather than recorded by
//! the engine, so they are approximate.
//!
//! `NSRegularExpression` and `NSTextCheckingResult` are defined here too.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::DynBlock;
use fancy_regex::{Regex, RegexBuilder};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSError, NSInteger, NSMatchingFlags, NSMatchingOptions, NSMutableString, NSRange, NSRegularExpression,
    NSRegularExpressionOptions, NSString, NSTextCheckingResult, NSTextCheckingType, NSUInteger, NSZone,
};

use crate::string::index::Text;
use crate::string::search::{NOT_FOUND, to_utf16, window};
use crate::string::view::view;
use crate::string::{inline, mutable, wtf8};

mod translate;

pub(crate) use translate::CASE_INSENSITIVE;
use translate::Traits;

/// `NSMatchingOptions`.
const REPORT_PROGRESS: usize = 1 << 0;
const REPORT_COMPLETION: usize = 1 << 1;
pub(crate) const MATCH_ANCHORED: usize = 1 << 2;
const TRANSPARENT_BOUNDS: usize = 1 << 3;
const WITHOUT_ANCHORING_BOUNDS: usize = 1 << 4;
/// `NSMatchingFlags`.
const FLAG_PROGRESS: usize = 1 << 0;
const FLAG_COMPLETED: usize = 1 << 1;
const FLAG_HIT_END: usize = 1 << 2;
const FLAG_REQUIRED_END: usize = 1 << 3;

/// A compiled pattern.
pub(crate) struct Pattern {
    re: Regex,
    traits: Traits,
}

impl Pattern {
    /// Compile an ICU pattern, or explain why it isn't one.
    pub(crate) fn new(pattern: &str, options: usize) -> Result<Pattern, String> {
        let t = translate::translate(pattern, options);
        RegexBuilder::new(&t.pattern)
            .backtrack_limit(10_000_000)
            .build()
            .map(|re| Pattern { re, traits: t.traits })
            .map_err(|e| e.to_string())
    }

    /// The number of capture groups, not counting the whole match.
    pub(crate) fn groups(&self) -> usize {
        self.re.captures_len() - 1
    }
}

/// The compiled pattern for `NSRegularExpressionSearch` with `pattern`
/// (WTF-8) and `flags`, or `None` if it isn't one, from a small cache per
/// thread: string searches name their pattern on every call, often the same
/// one in a loop, and compiling costs far more than matching a line.
pub(crate) fn cached(pattern: &[u8], flags: usize) -> Option<Rc<Pattern>> {
    type Entry = (Box<[u8]>, usize, Option<Rc<Pattern>>);
    thread_local! {
        static CACHE: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
    }
    let compile = || {
        let text = wtf8::to_str_lossy(pattern, wtf8::flags_of(pattern, true));
        Pattern::new(&text, flags).ok().map(Rc::new)
    };
    CACHE
        .try_with(|cache| {
            // Compiling calls nothing that searches, so the borrow is never
            // taken twice.
            let mut cache = cache.borrow_mut();
            if let Some(k) = cache.iter().position(|(p, f, _)| **p == *pattern && *f == flags) {
                let hit = cache.remove(k);
                let compiled = hit.2.clone();
                cache.insert(0, hit);
                return compiled;
            }
            let compiled = compile();
            cache.insert(0, (pattern.into(), flags, compiled.clone()));
            cache.truncate(16);
            compiled
        })
        // During the thread's teardown, without the cache.
        .unwrap_or_else(|_| compile())
}

/// Escape WTF-8 text so it matches itself as a pattern. The characters
/// escaped are ASCII, which never occurs inside another character's bytes.
pub(crate) fn escape_pattern(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 8);
    for &b in s {
        if b"\\^$.|?*+()[{}/".contains(&b) {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

/// Escape WTF-8 text so it expands to itself as a template.
pub(crate) fn escape_template(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 4);
    for &b in s {
        if b == b'\\' || b == b'$' {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

/// A new string with WTF-8 `bytes`.
fn string_of(bytes: &[u8]) -> Retained<NSString> {
    inline::new(bytes, wtf8::utf16_len(bytes), wtf8::flags_of(bytes, true))
}

/// A caller's string's WTF-8 text, copied.
fn bytes_of(s: &NSString) -> Vec<u8> {
    view(s).text().bytes.to_vec()
}

/// One match: the byte range in the text of the whole match and of each
/// group, `None` for groups that took no part.
pub(crate) type Found = Vec<Option<(usize, usize)>>;

/// What `walk` reports.
pub(crate) enum Step<'a> {
    /// A match, and the flags to report with it.
    Match(&'a Found, usize),
    /// The search moved past a position where nothing matched.
    Progress,
}

/// Walk the matches of `p` in the UTF-16 `range` of `t` under matching
/// `options`, in order, as NSRegularExpression enumerates them: `f` gets
/// each match and, with `NSMatchingReportProgress`, each position the
/// search moves on from, and returns whether to go on. Returns the flags a
/// completion report carries, or `None` if `f` stopped the walk.
pub(crate) fn walk(
    p: &Pattern,
    t: &Text,
    range: NSRange,
    options: usize,
    mut f: impl FnMut(Step) -> bool,
) -> Option<usize> {
    let (from, to) = window(t, range.location, range.length);
    let tr = p.traits;
    let anchoring = options & WITHOUT_ANCHORING_BOUNDS == 0;
    let transparent = options & TRANSPARENT_BOUNDS != 0;
    // The engine sees the text from the range's start when anchors, or
    // (with opaque bounds) word boundaries, must see it as the start;
    // otherwise from the string's, which look-behind may read, as ICU's
    // does.
    let start = if tr.anchors_start {
        if anchoring { from } else { 0 }
    } else if tr.word_bounds {
        if transparent { 0 } else { from }
    } else if tr.looks_behind {
        0
    } else {
        from
    };
    // It sees the text up to the range's end, where matches must stop,
    // unless an anchor must not match there (without anchoring bounds) or
    // look-ahead or a word boundary must see past it (with transparent
    // bounds); then matches running past it are left out.
    let end =
        if (tr.anchors_end && !anchoring) || (transparent && (tr.word_bounds || tr.looks_ahead) && !tr.anchors_end) {
            t.bytes.len()
        } else {
            to
        };
    let text = wtf8::to_str_lossy(&t.bytes[start..end], t.flags);
    let limit = to - start;
    let anchored = options & MATCH_ANCHORED != 0;
    let progress = options & REPORT_PROGRESS != 0 && !anchored;
    let mut pos = from - start;
    let mut found = Found::new();
    loop {
        let caps = (pos <= limit).then(|| p.re.captures_from_pos(&text, pos).ok().flatten()).flatten();
        let whole =
            caps.as_ref().and_then(|c| c.get(0)).filter(|m| m.end() <= limit && (!anchored || m.start() == pos));
        let Some(m) = whole else {
            // Moving on from each position but the last, as ICU reports.
            if progress && pos < limit {
                for _ in 1..text[pos..limit].chars().count() {
                    if !f(Step::Progress) {
                        return None;
                    }
                }
            }
            let hit_end = pos >= limit || !(tr.anchored || anchored);
            return Some(FLAG_COMPLETED | if hit_end { FLAG_HIT_END } else { 0 });
        };
        if progress {
            for _ in text[pos..m.start()].chars() {
                if !f(Step::Progress) {
                    return None;
                }
            }
        }
        let caps = caps.as_ref().expect("a match");
        found.clear();
        found.extend((0..caps.len()).map(|g| caps.get(g).map(|g| (start + g.start(), start + g.end()))));
        let flags = if m.end() == limit {
            (if tr.open_end { FLAG_HIT_END } else { 0 }) | if tr.anchors_end { FLAG_REQUIRED_END } else { 0 }
        } else {
            0
        };
        if !f(Step::Match(&found, flags)) {
            return None;
        }
        pos = if m.end() > m.start() {
            m.end()
        } else {
            m.end() + text[m.end()..].chars().next().map_or(1, char::len_utf8)
        };
    }
}

/// The matches `walk` finds, stopping when `f` returns false.
pub(crate) fn each_found(p: &Pattern, t: &Text, range: NSRange, options: usize, mut f: impl FnMut(&Found) -> bool) {
    walk(p, t, range, options & !REPORT_PROGRESS, |step| match step {
        Step::Match(found, _) => f(found),
        Step::Progress => true,
    });
}

/// A match's ranges in UTF-16.
fn ranges(t: &Text, found: &Found) -> Vec<NSRange> {
    found
        .iter()
        .map(|g| {
            g.map_or(NOT_FOUND, |(s, e)| {
                let (loc, len) = to_utf16(t, s, e);
                NSRange::new(loc, len)
            })
        })
        .collect()
}

/// Expand a template (WTF-8) for a match of `bytes`, appending to `out`:
/// `$n` is group n (as many digits as name a group), `\x` is `x` itself.
pub(crate) fn expand(template: &[u8], found: &Found, bytes: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < template.len() {
        match template[i] {
            b'\\' => {
                // The next character, whatever it is, stands for itself.
                if let Some(&next) = template.get(i + 1) {
                    let w = wtf8::width(next);
                    wtf8::push(out, &template[i + 1..(i + 1 + w).min(template.len())]);
                    i += 1 + w;
                } else {
                    i += 1;
                }
            }
            b'$' if template.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                i += 1;
                let mut n = 0usize;
                while let Some(d) = template.get(i).filter(|d| d.is_ascii_digit()) {
                    let next = n * 10 + usize::from(d - b'0');
                    if next >= found.len() && n != 0 {
                        break;
                    }
                    n = next;
                    i += 1;
                }
                if let Some(&Some((s, e))) = found.get(n) {
                    wtf8::push(out, &bytes[s..e]);
                }
            }
            _ => {
                let w = wtf8::width(template[i]).min(template.len() - i);
                wtf8::push(out, &template[i..i + w]);
                i += w;
            }
        }
    }
}

/// `expand` for a match given as UTF-16 ranges of `t`.
fn expand_ranges(template: &[u8], ranges: &[NSRange], t: &Text, out: &mut Vec<u8>) {
    let found: Found = ranges
        .iter()
        .map(|r| {
            (r.location != NOT_FOUND.location && r.end() <= t.utf16_len).then(|| {
                let (s, e) = t.range(r.location, r.length);
                // Whole characters: a range splitting a pair keeps neither
                // half.
                (if s.low { s.byte + 4 } else { s.byte }, e.byte.max(s.byte))
            })
        })
        .collect();
    expand(template, &found, t.bytes, out);
}

sidestep_runtime::static_class!(pub(crate) NSREGULAREXPRESSION, NSREGULAREXPRESSION_META = "NSRegularExpression", || {
    let _ = NSRegularExpressionImpl::class();
});

sidestep_runtime::static_class!(pub(crate) NSTEXTCHECKINGRESULT, NSTEXTCHECKINGRESULT_META = "NSTextCheckingResult", || {
    let _ = NSTextCheckingResultImpl::class();
});

pub(crate) struct RegexIvars {
    pattern: Retained<NSString>,
    options: usize,
    compiled: Pattern,
}

/// The user-info key an invalid pattern's error holds it under.
static INVALID_VALUE: crate::ConstantString =
    crate::ConstantString::new(&crate::CONSTANT_STRING_CLASS, crate::ConstStr::new("NSInvalidValue\0"));

/// The error for an invalid pattern.
fn invalid_pattern_error(pattern: &NSString) -> Retained<NSError> {
    // NSFormattingError, as macOS reports it.
    crate::error::cocoa(2048, &[(&INVALID_VALUE, pattern.retain().into())])
}

/// A new regular expression, or nil with `error` set.
fn compile(
    this: Allocated<NSRegularExpressionImpl>,
    pattern: &NSString,
    options: usize,
    error: *mut *mut AnyObject,
) -> Option<Retained<NSRegularExpressionImpl>> {
    let text = {
        let v = view(pattern);
        let t = v.text();
        wtf8::to_str_lossy(t.bytes, t.flags).into_owned()
    };
    match Pattern::new(&text, options) {
        Ok(compiled) => {
            // SAFETY: -copy of a string is an immutable string.
            let pattern: Retained<NSString> = unsafe { msg_send![pattern, copy] };
            let this = this.set_ivars(RegexIvars { pattern, options, compiled });
            // SAFETY: NSObject's initializer.
            Some(unsafe { msg_send![super(this), init] })
        }
        Err(_) => {
            // SAFETY: the caller passes null or room for an error.
            unsafe { crate::error::set(error.cast(), invalid_pattern_error(pattern)) };
            None
        }
    }
}

fn result(regex: &NSRegularExpressionImpl, ranges: Vec<NSRange>) -> Retained<NSTextCheckingResult> {
    // Loading the shell first makes this class the registered one.
    crate::load_shell(&NSTEXTCHECKINGRESULT);
    let this = NSTextCheckingResultImpl::alloc().set_ivars(ResultIvars { ranges, regex: Some(regex.retain()) });
    // SAFETY: NSObject's initializer; the class is NSTextCheckingResult.
    unsafe { Retained::cast_unchecked::<NSTextCheckingResult>(msg_send![super(this), init]) }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSRegularExpression"]
    #[ivars = RegexIvars]
    pub(crate) struct NSRegularExpressionImpl;

    impl NSRegularExpressionImpl {
        #[unsafe(method_id(initWithPattern:options:error:))]
        fn init_with_pattern(
            this: Allocated<Self>,
            pattern: &NSString,
            options: NSRegularExpressionOptions,
            error: *mut *mut AnyObject,
        ) -> Option<Retained<Self>> {
            compile(this, pattern, options.0, error)
        }

        #[unsafe(method_id(regularExpressionWithPattern:options:error:))]
        fn with_pattern(pattern: &NSString, options: NSRegularExpressionOptions, error: *mut *mut AnyObject) -> Option<Retained<Self>> {
            compile(Self::alloc(), pattern, options.0, error)
        }

        #[unsafe(method_id(escapedPatternForString:))]
        fn escaped_pattern(string: &NSString) -> Retained<NSString> {
            string_of(&escape_pattern(view(string).text().bytes))
        }

        #[unsafe(method_id(escapedTemplateForString:))]
        fn escaped_template(string: &NSString) -> Retained<NSString> {
            string_of(&escape_template(view(string).text().bytes))
        }

        #[unsafe(method_id(pattern))]
        fn pattern(&self) -> Retained<NSString> {
            self.ivars().pattern.clone()
        }

        #[unsafe(method(options))]
        fn options(&self) -> NSRegularExpressionOptions {
            NSRegularExpressionOptions(self.ivars().options)
        }

        #[unsafe(method(numberOfCaptureGroups))]
        fn number_of_capture_groups(&self) -> NSUInteger {
            self.ivars().compiled.groups()
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.retain()
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            match other.and_then(|o| o.downcast_ref::<NSRegularExpression>()) {
                Some(o) => {
                    // SAFETY: every NSRegularExpression is an instance of
                    // this class or a subclass of it.
                    let o = unsafe { &*(o as *const NSRegularExpression).cast::<Self>() };
                    let (a, b) = (self.ivars(), o.ivars());
                    a.options == b.options && a.pattern.isEqualToString(&b.pattern)
                }
                None => false,
            }
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            self.ivars().pattern.hash() ^ self.ivars().options
        }

        #[unsafe(method(enumerateMatchesInString:options:range:usingBlock:))]
        fn enumerate_matches(
            &self,
            string: &NSString,
            options: NSMatchingOptions,
            range: NSRange,
            block: &DynBlock<dyn Fn(*mut NSTextCheckingResult, NSMatchingFlags, NonNull<Bool>)>,
        ) {
            // A mutable string is copied first, so the block may edit it.
            let completed = mutable::with_text(string, |t| {
                crate::string::check_range("enumerateMatchesInString:options:range:usingBlock:", range, t.utf16_len);
                walk(&self.ivars().compiled, t, range, options.0, |step| {
                    let (r, flags) = match step {
                        Step::Match(found, flags) => (Some(result(self, ranges(t, found))), flags),
                        Step::Progress => (None, FLAG_PROGRESS),
                    };
                    let ptr = r.as_ref().map_or(std::ptr::null_mut(), |r| Retained::as_ptr(r).cast_mut());
                    let mut stop = Bool::NO;
                    block.call((ptr, NSMatchingFlags(flags), NonNull::from(&mut stop)));
                    !stop.as_bool()
                })
            });
            if let Some(flags) = completed
                && options.0 & REPORT_COMPLETION != 0
            {
                let mut stop = Bool::NO;
                block.call((std::ptr::null_mut(), NSMatchingFlags(flags), NonNull::from(&mut stop)));
            }
        }

        #[unsafe(method_id(matchesInString:options:range:))]
        fn matches_in_string(&self, string: &NSString, options: NSMatchingOptions, range: NSRange) -> Retained<AnyObject> {
            let results: Vec<Retained<NSTextCheckingResult>> = self
                .all(string, options.0, range, "matchesInString:options:range:")
                .into_iter()
                .map(|m| result(self, m))
                .collect();
            Retained::into_super(Retained::into_super(crate::string::paths::new_array(&results)))
        }

        #[unsafe(method(numberOfMatchesInString:options:range:))]
        fn number_of_matches(&self, string: &NSString, options: NSMatchingOptions, range: NSRange) -> NSUInteger {
            let mut count = 0;
            self.walk(string, options.0, range, "numberOfMatchesInString:options:range:", |_, step| {
                count += usize::from(matches!(step, Step::Match(..)));
                true
            });
            count
        }

        #[unsafe(method_id(firstMatchInString:options:range:))]
        fn first_match(&self, string: &NSString, options: NSMatchingOptions, range: NSRange) -> Option<Retained<NSTextCheckingResult>> {
            self.first(string, options.0, range, "firstMatchInString:options:range:").map(|m| result(self, m))
        }

        #[unsafe(method(rangeOfFirstMatchInString:options:range:))]
        fn range_of_first_match(&self, string: &NSString, options: NSMatchingOptions, range: NSRange) -> NSRange {
            self.first(string, options.0, range, "rangeOfFirstMatchInString:options:range:").map_or(NOT_FOUND, |m| m[0])
        }

        #[unsafe(method_id(stringByReplacingMatchesInString:options:range:withTemplate:))]
        fn replacing_matches(&self, string: &NSString, options: NSMatchingOptions, range: NSRange, template: &NSString) -> Retained<NSString> {
            let template = bytes_of(template);
            let v = view(string);
            let t = v.text();
            crate::string::check_range("stringByReplacingMatchesInString:options:range:withTemplate:", range, t.utf16_len);
            let mut out = Vec::with_capacity(t.bytes.len());
            let mut last = 0;
            each_found(&self.ivars().compiled, &t, range, options.0, |found| {
                let (s, e) = found[0].expect("the whole match");
                wtf8::push(&mut out, &t.bytes[last..s]);
                expand(&template, found, t.bytes, &mut out);
                last = e;
                true
            });
            wtf8::push(&mut out, &t.bytes[last..]);
            string_of(&out)
        }

        #[unsafe(method(replaceMatchesInString:options:range:withTemplate:))]
        fn replace_matches(&self, string: &NSMutableString, options: NSMatchingOptions, range: NSRange, template: &NSString) -> NSUInteger {
            let template = bytes_of(template);
            let replacements: Vec<(NSRange, Retained<NSString>)> = {
                let v = view(string);
                let t = v.text();
                crate::string::check_range("replaceMatchesInString:options:range:withTemplate:", range, t.utf16_len);
                let mut out = Vec::new();
                let mut edits = Vec::new();
                each_found(&self.ivars().compiled, &t, range, options.0, |found| {
                    out.clear();
                    expand(&template, found, t.bytes, &mut out);
                    let (s, e) = found[0].expect("the whole match");
                    let (loc, len) = to_utf16(&t, s, e);
                    edits.push((NSRange::new(loc, len), string_of(&out)));
                    true
                });
                edits
            };
            // Replace from the end so earlier ranges stay valid.
            for (r, with) in replacements.iter().rev() {
                string.replaceCharactersInRange_withString(*r, with);
            }
            replacements.len()
        }

        #[unsafe(method_id(replacementStringForResult:inString:offset:template:))]
        fn replacement_string(&self, result: &NSTextCheckingResult, string: &NSString, offset: NSInteger, template: &NSString) -> Retained<NSString> {
            let count = result.numberOfRanges();
            let groups: Vec<NSRange> = (0..count)
                .map(|k| {
                    let r = result.rangeAtIndex(k);
                    if r.location == NOT_FOUND.location { r } else { NSRange::new(r.location.wrapping_add_signed(offset), r.length) }
                })
                .collect();
            let template = bytes_of(template);
            let mut out = Vec::new();
            expand_ranges(&template, &groups, &view(string).text(), &mut out);
            string_of(&out)
        }
    }

    unsafe impl NSObjectProtocol for NSRegularExpressionImpl {}
);

impl NSRegularExpressionImpl {
    /// `walk` over `string`, with `f` getting the text too.
    fn walk(
        &self,
        string: &NSString,
        options: usize,
        range: NSRange,
        method: &str,
        mut f: impl FnMut(&Text, Step) -> bool,
    ) -> Option<usize> {
        let v = view(string);
        let t = v.text();
        crate::string::check_range(method, range, t.utf16_len);
        walk(&self.ivars().compiled, &t, range, options, |step| f(&t, step))
    }

    fn all(&self, string: &NSString, options: usize, range: NSRange, method: &str) -> Vec<Vec<NSRange>> {
        let mut out = Vec::new();
        self.walk(string, options & !REPORT_PROGRESS, range, method, |t, step| {
            if let Step::Match(found, _) = step {
                out.push(ranges(t, found));
            }
            true
        });
        out
    }

    fn first(&self, string: &NSString, options: usize, range: NSRange, method: &str) -> Option<Vec<NSRange>> {
        let mut out = None;
        self.walk(string, options & !REPORT_PROGRESS, range, method, |t, step| match step {
            Step::Match(found, _) => {
                out = Some(ranges(t, found));
                false
            }
            Step::Progress => true,
        });
        out
    }
}

pub(crate) struct ResultIvars {
    ranges: Vec<NSRange>,
    regex: Option<Retained<NSRegularExpressionImpl>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextCheckingResult"]
    #[ivars = ResultIvars]
    pub(crate) struct NSTextCheckingResultImpl;

    impl NSTextCheckingResultImpl {
        #[unsafe(method_id(regularExpressionCheckingResultWithRanges:count:regularExpression:))]
        fn with_ranges(ranges: *mut NSRange, count: NSUInteger, regex: &NSRegularExpression) -> Retained<Self> {
            let ranges = if count == 0 {
                Vec::new()
            } else {
                // SAFETY: the caller passes `count` ranges.
                unsafe { std::slice::from_raw_parts(ranges, count) }.to_vec()
            };
            // SAFETY: an NSRegularExpression is Sidestep's class.
            let regex: &NSRegularExpressionImpl = unsafe { &*(regex as *const NSRegularExpression).cast() };
            let this = Self::alloc().set_ivars(ResultIvars { ranges, regex: Some(regex.retain()) });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(resultType))]
        fn result_type(&self) -> NSTextCheckingType {
            NSTextCheckingType::RegularExpression
        }

        #[unsafe(method(range))]
        fn range(&self) -> NSRange {
            self.ivars().ranges.first().copied().unwrap_or(NOT_FOUND)
        }

        #[unsafe(method(numberOfRanges))]
        fn number_of_ranges(&self) -> NSUInteger {
            self.ivars().ranges.len()
        }

        #[unsafe(method(rangeAtIndex:))]
        fn range_at_index(&self, index: NSUInteger) -> NSRange {
            let ranges = &self.ivars().ranges;
            match ranges.get(index) {
                Some(r) => *r,
                None => panic!("-[NSTextCheckingResult rangeAtIndex:]: index {index} out of bounds; {} ranges", ranges.len()),
            }
        }

        #[unsafe(method(rangeWithName:))]
        fn range_with_name(&self, name: &NSString) -> NSRange {
            let iv = self.ivars();
            let name = crate::string::with_str(name, str::to_owned);
            iv.regex
                .as_ref()
                .and_then(|r| r.ivars().compiled.re.capture_names().position(|n| n == Some(name.as_str())))
                .and_then(|k| iv.ranges.get(k).copied())
                .unwrap_or(NOT_FOUND)
        }

        #[unsafe(method_id(regularExpression))]
        fn regular_expression(&self) -> Option<Retained<NSRegularExpression>> {
            // SAFETY: NSRegularExpressionImpl is the class registered as
            // NSRegularExpression.
            self.ivars().regex.as_ref().map(|r| unsafe { Retained::cast_unchecked(r.clone()) })
        }

        #[unsafe(method_id(resultByAdjustingRangesWithOffset:))]
        fn adjusted(&self, offset: NSInteger) -> Retained<Self> {
            let iv = self.ivars();
            let ranges = iv
                .ranges
                .iter()
                .map(|r| if r.location == NOT_FOUND.location { *r } else { NSRange::new(r.location.wrapping_add_signed(offset), r.length) })
                .collect();
            let this = Self::alloc().set_ivars(ResultIvars { ranges, regex: iv.regex.clone() });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSTextCheckingResultImpl {}
);

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Text<'_> {
        Text::plain(s.as_bytes(), s.encode_utf16().count(), wtf8::flags_of(s.as_bytes(), false))
    }

    fn all(pattern: &str, options: usize, s: &str) -> Vec<(usize, usize)> {
        let p = Pattern::new(pattern, options).unwrap();
        let t = text(s);
        let mut out = Vec::new();
        each_found(&p, &t, NSRange::new(0, t.utf16_len), 0, |found| {
            out.push(found[0].unwrap());
            true
        });
        out
    }

    #[test]
    fn icu_anchors_and_dots() {
        assert_eq!(all("[ \t]+$", 0, "abc  \t"), [(3, 6)]);
        assert_eq!(all("[ \t]+$", 0, "abc  \t\n"), [(3, 6)]);
        assert_eq!(all("[ \t]+$", 0, "a  \nb"), []);
        assert_eq!(all("a.b", 0, "a\nb"), []);
        assert_eq!(all("a.b", 0, "a\u{2028}b"), []);
        assert_eq!(all("a.b", 0, "a\u{b}b"), []);
        assert_eq!(all(r"\Qa.b\E", 0, "axb a.b"), [(4, 7)]);
        assert_eq!(all("[]a]+", 0, "x]a"), [(1, 3)]);
        assert_eq!(all("$", 0, "a\r\n"), [(1, 1), (3, 3)]);
        assert_eq!(all("$", translate::ANCHORS_MATCH_LINES, "\r\u{b}"), [(0, 0), (1, 1), (2, 2)]);
        assert_eq!(all("^", translate::ANCHORS_MATCH_LINES, "\n\n"), [(0, 0), (1, 1)]);
        assert_eq!(all("(?m)^b$", 0, "a\r\nb\r\nc"), [(3, 4)]);
    }

    #[test]
    fn templates() {
        let s = "John Smith";
        let found: Found = vec![Some((0, 10)), Some((0, 4)), Some((5, 10))];
        let expand_str = |template: &str| {
            let mut out = Vec::new();
            expand(template.as_bytes(), &found, s.as_bytes(), &mut out);
            String::from_utf8(out).unwrap()
        };
        assert_eq!(expand_str("$2, $1"), "Smith, John");
        assert_eq!(expand_str(r"\$$1"), "$John");
        assert_eq!(expand_str("$0!"), "John Smith!");
        assert_eq!(expand_str("$12"), "John2");
    }
}
