//! Text editing at scale: an `NSTextView` (as `scrollableTextView` makes
//! it, not in a window) holding 11 MB of text in 200 000 lines, in the
//! 13-point monospaced system font. Run in release mode on macOS (AppKit)
//! and on Linux (Sidestep) to compare: `cargo run --release -p textbench`.
//!
//! Each figure is the median of seven runs (a fresh view each time, after
//! a warm-up run):
//!
//! - `setString`: replacing the text of a fresh view with 1 KB, 1 MB and
//!   the 11 MB string (the first two over 201 and 21 runs); then the same
//!   with the first screen laid out after it, and with the attributes read
//!   at the text's end (where a lazy storage fixes them only when asked).
//! - replacing all the text through the text storage, with
//!   `replaceCharactersInRange:withString:` and with
//!   `setAttributedString:` (an attributed string of two runs a line).
//! - first screen: laying out the top 600 points after that, what the first
//!   paint needs.
//! - full layout: laying out all of it (contiguous layout, AppKit's
//!   default, needs this before answering for text far down).
//! - keystroke: typing a character in the middle of the text and laying
//!   out the 600 points around the caret, as the next paint needs (p50 and
//!   p99 of 400 keystrokes per run; the p99 is the median of the runs'),
//!   with contiguous layout after the full layout, and with non-contiguous
//!   layout from a fresh view; and deleting backward, the same way.
//! - jump to the end: with non-contiguous layout, laying out the last
//!   screen of a fresh view.
//! - keystroke and reading the text: typing as above, then asking the
//!   view's string for its length and the caret's paragraph, as a delegate
//!   re-highlighting the edited paragraph does.
//! - select all: selecting all of the laid-out text, with the rect an input
//!   method asks for on each selection change.
//! - keystroke in a long paragraph: typing in the middle of the middle one
//!   of 40 paragraphs of 32 KB each (a paragraph is laid out again whole).
//! - keystroke in highlighted text: the text set through the storage with
//!   `setAttributedString:` (in the view's font, four highlighted words a
//!   line, nine runs), then typing in the middle with non-contiguous
//!   layout, as above: before its attributes are fixed (a lazy storage
//!   fixes them as layout reaches them), and after they all have been.
//! - rich text: the 1 MB text with four bold words a line (18 182
//!   paragraphs, nine runs each) written as RTF and as HTML, as copying it
//!   from a rich text view writes it, and each read back, as pasting reads
//!   it (the median of three runs for reading HTML, seven for the rest).
//!
//! What isn't measured: drawing (turning the laid-out lines into pixels),
//! which depends on the window system.

use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSAttributedStringDocumentFormats, NSDocumentTypeDocumentAttribute, NSDocumentTypeDocumentOption,
    NSFont, NSFontAttributeName, NSFontWeightRegular, NSHTMLTextDocumentType, NSLayoutManager, NSRTFTextDocumentType,
    NSScrollView, NSStandardKeyBindingResponding, NSTextContainer, NSTextInputClient, NSTextView,
};
use objc2_foundation::{
    NSAttributedString, NSData, NSDictionary, NSMutableAttributedString, NSNotFound, NSPoint, NSRange, NSRect, NSSize,
    NSString,
};

use sidestep as _;

/// Lines of text; TEXTBENCH_LINES sets another number.
fn lines() -> usize {
    std::env::var("TEXTBENCH_LINES").ok().and_then(|s| s.parse().ok()).unwrap_or(200_000)
}
const KEYS: usize = 400;
const SCREEN: f64 = 600.0;

/// Lines of 55 bytes: 200 000 of them are 11 MB.
fn text(lines: usize) -> String {
    let mut s = String::with_capacity(lines * 55);
    for i in 0..lines {
        s.push_str(&format!("{i:06} the quick brown fox jumps over the lazy dog {:03}\n", i % 997));
    }
    s
}

struct View {
    /// Kept: the text view's superviews go with it.
    _scroll: Retained<NSScrollView>,
    tv: Retained<NSTextView>,
    lm: Retained<NSLayoutManager>,
    tc: Retained<NSTextContainer>,
}

fn view(mtm: MainThreadMarker, non_contiguous: bool) -> View {
    let scroll = NSTextView::scrollableTextView(mtm);
    scroll.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(800.0, SCREEN)));
    let tv: Retained<NSTextView> = scroll.documentView().unwrap().downcast().unwrap();
    let font = unsafe { NSFont::monospacedSystemFontOfSize_weight(13.0, NSFontWeightRegular) };
    tv.setFont(Some(&font));
    let lm = unsafe { tv.layoutManager() }.unwrap();
    lm.setAllowsNonContiguousLayout(non_contiguous);
    let tc = unsafe { tv.textContainer() }.unwrap();
    View { _scroll: scroll, tv, lm, tc }
}

impl View {
    /// Lay out the screen of text from `y` down.
    fn screen_at(&self, y: f64) {
        let r = NSRect::new(NSPoint::new(0.0, y.max(0.0)), NSSize::new(800.0, SCREEN));
        self.lm.ensureLayoutForBoundingRect_inTextContainer(r, &self.tc);
    }

    /// Lay out the screen around the caret.
    fn caret_screen(&self) {
        let sel = self.tv.selectedRange();
        let caret = self.lm.boundingRectForGlyphRange_inTextContainer(NSRange::new(sel.location, 0), &self.tc);
        self.screen_at(caret.origin.y - SCREEN / 2.0);
    }

    fn type_char(&self) {
        let s = NSString::from_str("x");
        unsafe { self.tv.insertText_replacementRange(&s, NSRange::new(NSNotFound as usize, 0)) };
        self.caret_screen();
    }

    fn delete_backward(&self) {
        unsafe { self.tv.deleteBackward(None) };
        self.caret_screen();
    }

    /// Type, then read what a delegate reads of the text.
    fn type_and_read(&self) {
        self.type_char();
        let string = self.tv.string();
        let sel = self.tv.selectedRange();
        std::hint::black_box((string.length(), string.paragraphRangeForRange(NSRange::new(sel.location, 0))));
    }

    /// Select all the text, and find the rect an input method asks for.
    fn select_all(&self) {
        let len = self.tv.string().length();
        self.tv.setSelectedRange(NSRange::new(0, len));
        let mut actual = NSRange::new(0, 0);
        let r = unsafe { self.tv.firstRectForCharacterRange_actualRange(NSRange::new(0, len), &mut actual) };
        std::hint::black_box(r);
    }
}

/// The text's size as the figures name it.
fn size_name(bytes: usize) -> String {
    if bytes >= 500_000 { format!("{:.0} MB", bytes as f64 / 1e6) } else { format!("{:.0} KB", bytes as f64 / 1e3) }
}

/// The median time of `f` on a fresh view readied by `ready`, over `runs`
/// runs after a warm-up.
fn on_fresh_views(mtm: MainThreadMarker, runs: usize, ready: impl Fn(&View), f: impl Fn(&View)) -> f64 {
    let mut times = Vec::with_capacity(runs);
    for run in 0..=runs {
        objc2::rc::autoreleasepool(|_| {
            let v = view(mtm, false);
            ready(&v);
            let t = Instant::now();
            f(&v);
            let dt = ms(t.elapsed());
            drop(v);
            if run > 0 {
                times.push(dt);
            }
        });
    }
    median(times)
}

/// `text` with two runs a line: its number in an attribute of its own.
fn two_runs_a_line(text: &NSString) -> Retained<NSMutableAttributedString> {
    let m = NSMutableAttributedString::from_nsstring(text);
    let key = NSString::from_str("textbench.number");
    let value = NSString::from_str("number");
    let len = text.length();
    let mut at = 0;
    while at < len {
        let line = text.lineRangeForRange(NSRange::new(at, 0));
        unsafe { m.addAttribute_value_range(&key, &value, NSRange::new(at, 6.min(line.length))) };
        at = line.location + line.length.max(1);
    }
    m
}

/// `text` in the view's font with four words a line highlighted, as a
/// highlighter makes it: nine runs a line.
fn highlighted(text: &NSString) -> Retained<NSMutableAttributedString> {
    let font = unsafe { NSFont::monospacedSystemFontOfSize_weight(13.0, NSFontWeightRegular) };
    let attrs = NSDictionary::from_slices(&[unsafe { NSFontAttributeName }], &[&*font as &AnyObject]);
    let m = unsafe {
        NSMutableAttributedString::initWithString_attributes(NSMutableAttributedString::alloc(), text, Some(&attrs))
    };
    let key = NSString::from_str("textbench.highlight");
    let value = NSString::from_str("keyword");
    let len = text.length();
    let mut at = 0;
    while at < len {
        let line = text.lineRangeForRange(NSRange::new(at, 0));
        for k in 0..4 {
            if k * 12 + 5 < line.length {
                unsafe { m.addAttribute_value_range(&key, &value, NSRange::new(at + k * 12, 5)) };
            }
        }
        at = line.location + line.length.max(1);
    }
    m
}

/// `text` in the 13-point system font with four bold words a line: nine
/// runs a line that rich text formats write.
fn bold_words(text: &NSString) -> Retained<NSMutableAttributedString> {
    let font = NSFont::systemFontOfSize(13.0);
    let bold = NSFont::boldSystemFontOfSize(13.0);
    let attrs = NSDictionary::from_slices(&[unsafe { NSFontAttributeName }], &[&*font as &AnyObject]);
    let m = unsafe {
        NSMutableAttributedString::initWithString_attributes(NSMutableAttributedString::alloc(), text, Some(&attrs))
    };
    let len = text.length();
    let mut at = 0;
    while at < len {
        let line = text.lineRangeForRange(NSRange::new(at, 0));
        for k in 0..4 {
            if k * 12 + 5 < line.length {
                unsafe { m.addAttribute_value_range(NSFontAttributeName, &bold, NSRange::new(at + k * 12, 5)) };
            }
        }
        at = line.location + line.length.max(1);
    }
    m
}

/// The median time of `f` over `runs` runs after a warm-up.
fn timed<T>(runs: usize, f: impl Fn() -> T) -> f64 {
    let mut times = Vec::with_capacity(runs);
    for run in 0..=runs {
        objc2::rc::autoreleasepool(|_| {
            let t = Instant::now();
            std::hint::black_box(f());
            if run > 0 {
                times.push(ms(t.elapsed()));
            }
        });
    }
    median(times)
}

/// Writing `string` as `kind` (RTF or HTML) and reading it back.
fn rich_text(string: &NSAttributedString, kind: &NSString, read_runs: usize) -> (f64, f64) {
    let write = NSDictionary::from_slices(&[unsafe { NSDocumentTypeDocumentAttribute }], &[kind as &AnyObject]);
    let read = NSDictionary::from_slices(&[unsafe { NSDocumentTypeDocumentOption }], &[kind as &AnyObject]);
    let range = NSRange::new(0, string.length());
    let data =
        || -> Retained<NSData> { unsafe { string.dataFromRange_documentAttributes_error(range, &write) }.unwrap() };
    let t_write = timed(7, data);
    let bytes = data();
    let t_read = timed(read_runs, || unsafe {
        NSAttributedString::initWithData_options_documentAttributes_error(
            NSAttributedString::alloc(),
            &bytes,
            &read,
            None,
        )
        .unwrap()
    });
    (t_write, t_read)
}

/// 40 paragraphs of 32 KB: words, no line breaks.
fn long_paragraphs() -> String {
    let words = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor ";
    let para: String = words.repeat(32 * 1024 / words.len());
    (0..40).map(|_| format!("{para}\n")).collect()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn percentile(v: &[f64], p: f64) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

#[derive(Default)]
struct Keys {
    p50: Vec<f64>,
    p99: Vec<f64>,
    max: Vec<f64>,
}

impl Keys {
    fn add(&mut self, times: &[f64]) {
        self.p50.push(percentile(times, 0.5));
        self.p99.push(percentile(times, 0.99));
        self.max.push(percentile(times, 1.0));
    }

    fn report(self, name: &str) {
        println!(
            "{name:<34} p50 {:>8.3} ms   p99 {:>8.3} ms   max {:>8.3} ms",
            median(self.p50),
            median(self.p99),
            median(self.max)
        );
    }
}

fn keystrokes(v: &View, f: impl Fn(&View)) -> Vec<f64> {
    (0..KEYS)
        .map(|_| {
            let t = Instant::now();
            f(v);
            ms(t.elapsed())
        })
        .collect()
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let _app = NSApplication::sharedApplication(mtm);
    let lines = lines();
    let small = NSString::from_str(&text(19));
    let mid = NSString::from_str(&text(18_182));
    let text = text(lines);
    println!("{} bytes, {} lines", text.len(), lines);
    let ns = NSString::from_str(&text);
    let middle = ns.length() / 2;
    let (mut set, mut first, mut full, mut end) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut typed, mut deleted, mut typed_nc, mut deleted_nc) =
        (Keys::default(), Keys::default(), Keys::default(), Keys::default());
    let (mut read, mut long) = (Keys::default(), Keys::default());
    let (mut lit_lazy, mut lit_fixed) = (Keys::default(), Keys::default());
    let lit = highlighted(&ns);
    let mut select = Vec::new();
    let long_text = NSString::from_str(&long_paragraphs());
    for run in 0..8 {
        let warm = run == 0;
        // Contiguous layout: set the text, the first screen, all of it, then
        // type in the middle.
        let v = view(mtm, false);
        let t = Instant::now();
        v.tv.setString(&ns);
        let t_set = ms(t.elapsed());
        let t = Instant::now();
        v.screen_at(0.0);
        let t_first = ms(t.elapsed());
        let t = Instant::now();
        v.lm.ensureLayoutForTextContainer(&v.tc);
        let t_full = ms(t.elapsed());
        v.tv.setSelectedRange(NSRange::new(middle, 0));
        v.caret_screen();
        let k = keystrokes(&v, View::type_char);
        let d = keystrokes(&v, View::delete_backward);
        let r = keystrokes(&v, View::type_and_read);
        let t = Instant::now();
        v.select_all();
        let t_select = ms(t.elapsed());
        drop(v);
        // A long paragraph: typing in the middle of it.
        let v = view(mtm, false);
        v.tv.setString(&long_text);
        v.lm.ensureLayoutForTextContainer(&v.tc);
        v.tv.setSelectedRange(NSRange::new(long_text.length() / 2, 0));
        v.caret_screen();
        let l = keystrokes(&v, View::type_char);
        drop(v);
        // Non-contiguous layout: the last screen of a fresh view, then type
        // in the middle.
        let v = view(mtm, true);
        v.tv.setString(&ns);
        let t = Instant::now();
        v.tv.setSelectedRange(NSRange::new(ns.length(), 0));
        v.caret_screen();
        let t_end = ms(t.elapsed());
        v.tv.setSelectedRange(NSRange::new(middle, 0));
        v.caret_screen();
        let knc = keystrokes(&v, View::type_char);
        let dnc = keystrokes(&v, View::delete_backward);
        drop(v);
        // Highlighted text, typed into before and after its attributes are
        // all fixed.
        let mut lit_keys = Vec::new();
        for fix_first in [false, true] {
            let v = view(mtm, true);
            let storage = unsafe { v.tv.textStorage() }.unwrap();
            let m: &NSMutableAttributedString = &storage;
            let a: &NSAttributedString = &lit;
            m.setAttributedString(a);
            if fix_first {
                storage.ensureAttributesAreFixedInRange(NSRange::new(0, storage.length()));
            }
            v.tv.setSelectedRange(NSRange::new(middle, 0));
            v.caret_screen();
            lit_keys.push(keystrokes(&v, View::type_char));
            drop(v);
        }
        if warm {
            continue;
        }
        lit_lazy.add(&lit_keys[0]);
        lit_fixed.add(&lit_keys[1]);
        set.push(t_set);
        first.push(t_first);
        full.push(t_full);
        end.push(t_end);
        typed.add(&k);
        deleted.add(&d);
        typed_nc.add(&knc);
        deleted_nc.add(&dnc);
        read.add(&r);
        long.add(&l);
        select.push(t_select);
    }
    // setString at other sizes, and what laziness puts off until later.
    let size = size_name(text.len());
    let nothing = |_: &View| {};
    let t_small = on_fresh_views(mtm, 201, nothing, |v| v.tv.setString(&small));
    let t_mid = on_fresh_views(mtm, 21, nothing, |v| v.tv.setString(&mid));
    let t_set_first = on_fresh_views(mtm, 7, nothing, |v| {
        v.tv.setString(&ns);
        v.screen_at(0.0);
    });
    let t_set_attrs = on_fresh_views(mtm, 7, nothing, |v| {
        v.tv.setString(&ns);
        let storage = unsafe { v.tv.textStorage() }.unwrap();
        let mut r = NSRange::new(0, 0);
        let d = unsafe { storage.attributesAtIndex_effectiveRange(storage.length() - 1, &mut r) };
        std::hint::black_box((d, r));
    });
    let short = |v: &View| v.tv.setString(&small);
    let t_replace = on_fresh_views(mtm, 7, short, |v| {
        let storage = unsafe { v.tv.textStorage() }.unwrap();
        let m: &NSMutableAttributedString = &storage;
        m.replaceCharactersInRange_withString(NSRange::new(0, storage.length()), &ns);
    });
    let attributed = two_runs_a_line(&ns);
    let t_attributed = on_fresh_views(mtm, 7, short, |v| {
        let storage = unsafe { v.tv.textStorage() }.unwrap();
        let m: &NSMutableAttributedString = &storage;
        let a: &NSAttributedString = &attributed;
        m.setAttributedString(a);
    });
    println!("{:<34} {:>9.3} ms", format!("setString ({})", size_name(small.length())), t_small);
    println!("{:<34} {:>9.3} ms", format!("setString ({})", size_name(mid.length())), t_mid);
    println!("{:<34} {:>9.3} ms", format!("setString ({size})"), median(set));
    println!("{:<34} {:>9.3} ms", "setString and first screen", t_set_first);
    println!("{:<34} {:>9.3} ms", "setString, attributes at the end", t_set_attrs);
    println!("{:<34} {:>9.3} ms", "storage: replace all", t_replace);
    println!("{:<34} {:>9.3} ms", "storage: setAttributedString", t_attributed);
    println!("{:<34} {:>9.3} ms", "first screen", median(first));
    println!("{:<34} {:>9.3} ms", "full layout", median(full));
    println!("{:<34} {:>9.3} ms", "jump to the end (non-contiguous)", median(end));
    typed.report("keystroke (contiguous)");
    deleted.report("delete backward (contiguous)");
    typed_nc.report("keystroke (non-contiguous)");
    deleted_nc.report("delete backward (non-contiguous)");
    read.report("keystroke and reading the text");
    println!("{:<34} {:>9.3} ms", "select all", median(select));
    long.report("keystroke in a 32 KB paragraph");
    lit_lazy.report("keystroke, highlighted, to fix");
    lit_fixed.report("keystroke, highlighted, fixed");
    let rich = bold_words(&mid);
    let mid_size = size_name(mid.length());
    for (name, kind, read_runs) in
        [("RTF", unsafe { NSRTFTextDocumentType }, 7), ("HTML", unsafe { NSHTMLTextDocumentType }, 3)]
    {
        let (write, read) = rich_text(&rich, kind, read_runs);
        println!("{:<34} {:>9.3} ms", format!("rich text: {mid_size} as {name}"), write);
        println!("{:<34} {:>9.3} ms", format!("rich text: {mid_size} of {name} read"), read);
    }
}
