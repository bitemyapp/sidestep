//! Text editing at scale: an `NSTextView` (as `scrollableTextView` makes
//! it, not in a window) holding 10 MB of text in 200 000 lines, in the
//! 13-point monospaced system font. Run in release mode on macOS (AppKit)
//! and on Linux (Sidestep) to compare: `cargo run --release -p textbench`.
//!
//! Each figure is the median of seven runs (a fresh view each time, after
//! a warm-up run):
//!
//! - `setString`: replacing the view's text with the 10 MB string.
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
//!
//! What isn't measured: drawing (turning the laid-out lines into pixels),
//! which depends on the window system.

use std::time::{Duration, Instant};

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSFont, NSFontWeightRegular, NSLayoutManager, NSScrollView, NSStandardKeyBindingResponding,
    NSTextContainer, NSTextInputClient, NSTextView,
};
use objc2_foundation::{NSNotFound, NSPoint, NSRange, NSRect, NSSize, NSString};

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
    let text = text(lines);
    println!("{} bytes, {} lines", text.len(), lines);
    let ns = NSString::from_str(&text);
    let middle = ns.length() / 2;
    let (mut set, mut first, mut full, mut end) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut typed, mut deleted, mut typed_nc, mut deleted_nc) =
        (Keys::default(), Keys::default(), Keys::default(), Keys::default());
    let (mut read, mut long) = (Keys::default(), Keys::default());
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
        if warm {
            continue;
        }
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
    println!("{:<34} {:>9.3} ms", "setString (10 MB)", median(set));
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
}
