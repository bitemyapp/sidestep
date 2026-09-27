//! TextKit 2 at scale: an `NSTextView` in TextKit 2 mode (as
//! `scrollableTextView` makes it, 800 × 600 points, not in a window)
//! holding 11 MB of text in 200 000 lines, in the 13-point monospaced
//! system font. Run in release mode on macOS (AppKit) and on Linux
//! (Sidestep) to compare: `cargo run --release -p textkit2bench`.
//!
//! Each figure is the median of seven runs, a fresh view each, after a
//! warm-up run:
//!
//! - `setString`: the 11 MB string into a fresh view.
//! - first layout: laying out the viewport of a fresh view after that
//!   (`layoutViewport`), what the first paint needs, and how many of the
//!   200 000 fragments it laid out.
//! - scroll step: scrolling 40 points (a wheel step) and laying out the
//!   viewport again, 400 steps down from the top (p50, p99, max).
//! - jump: scrolling to the middle of the text (as the view estimates it)
//!   and laying out the viewport there.
//! - keystroke: typing a character in the middle of what shows and laying
//!   out the viewport (p50, p99, max over 400).
//! - with every fragment made (enumerating them all, which scrolling
//!   through the text also does): `setFont:` on the whole text, and
//!   `setString:` of one character, which free them all; then the view
//!   with every fragment made again, freed (dropped and its autorelease
//!   pool drained).
//!
//! TEXTKIT2BENCH_LINES sets another number of lines.

use std::time::{Duration, Instant};

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSFont, NSFontWeightRegular, NSScrollView, NSTextInputClient, NSTextLayoutFragment,
    NSTextLayoutFragmentEnumerationOptions, NSTextLayoutManager, NSTextView,
};
use objc2_foundation::{NSNotFound, NSPoint, NSRange, NSRect, NSSize, NSString};

use sidestep as _;

const STEPS: usize = 400;
const SCREEN: f64 = 600.0;

fn lines() -> usize {
    std::env::var("TEXTKIT2BENCH_LINES").ok().and_then(|s| s.parse().ok()).unwrap_or(200_000)
}

/// Lines of 55 bytes: 200 000 of them are 11 MB.
fn text(lines: usize) -> String {
    let mut s = String::with_capacity(lines * 55);
    for i in 0..lines {
        s.push_str(&format!("{i:06} the quick brown fox jumps over the lazy dog {:03}\n", i % 997));
    }
    s
}

struct View {
    scroll: Retained<NSScrollView>,
    tv: Retained<NSTextView>,
    tlm: Retained<NSTextLayoutManager>,
}

fn view(mtm: MainThreadMarker) -> View {
    let scroll = NSTextView::scrollableTextView(mtm);
    scroll.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(800.0, SCREEN)));
    let tv: Retained<NSTextView> = scroll.documentView().unwrap().downcast().unwrap();
    let font = unsafe { NSFont::monospacedSystemFontOfSize_weight(13.0, NSFontWeightRegular) };
    tv.setFont(Some(&font));
    let tlm = tv.textLayoutManager().expect("a TextKit 2 view");
    View { scroll, tv, tlm }
}

impl View {
    fn layout_viewport(&self) {
        self.tlm.textViewportLayoutController().layoutViewport();
    }

    fn scroll_to(&self, y: f64) {
        let clip = self.scroll.contentView();
        clip.scrollToPoint(NSPoint::new(0.0, y));
        self.scroll.reflectScrolledClipView(&clip);
    }

    fn visible_y(&self) -> f64 {
        self.scroll.contentView().bounds().origin.y
    }

    /// Fragments laid out.
    fn laid(&self) -> usize {
        let n = std::cell::Cell::new(0);
        let block = block2::RcBlock::new(|f: std::ptr::NonNull<NSTextLayoutFragment>| -> objc2::runtime::Bool {
            if unsafe { f.as_ref() }.state().0 == 3 {
                n.set(n.get() + 1);
            }
            objc2::runtime::Bool::YES
        });
        self.tlm.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
            None,
            NSTextLayoutFragmentEnumerationOptions::None,
            &block,
        );
        n.get()
    }
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
struct Steps {
    p50: Vec<f64>,
    p99: Vec<f64>,
    max: Vec<f64>,
}

impl Steps {
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

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let _app = NSApplication::sharedApplication(mtm);
    let lines = lines();
    let text = text(lines);
    println!("{} bytes, {} lines", text.len(), lines);
    let ns = NSString::from_str(&text);
    let (mut set, mut first, mut jump, mut laid) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut font, mut reset, mut free) = (Vec::new(), Vec::new(), Vec::new());
    let (mut scroll, mut keys) = (Steps::default(), Steps::default());
    let mut height = 0.0;
    let other = unsafe { NSFont::monospacedSystemFontOfSize_weight(14.0, NSFontWeightRegular) };
    let one = NSString::from_str("x");
    for run in 0..8 {
        let freed_at = std::cell::Cell::new(Instant::now());
        let (t_set, t_first, n_laid, t_jump, s, k, t_font, t_reset) = objc2::rc::autoreleasepool(|_| {
            let v = view(mtm);
            let t = Instant::now();
            v.tv.setString(&ns);
            let t_set = ms(t.elapsed());
            let t = Instant::now();
            v.layout_viewport();
            let t_first = ms(t.elapsed());
            let n_laid = v.laid();
            height = v.tv.frame().size.height;
            let s: Vec<f64> = (0..STEPS)
                .map(|_| {
                    let y = v.visible_y() + 40.0;
                    let t = Instant::now();
                    v.scroll_to(y);
                    v.layout_viewport();
                    ms(t.elapsed())
                })
                .collect();
            let t = Instant::now();
            v.scroll_to(v.tv.frame().size.height / 2.0);
            v.layout_viewport();
            let t_jump = ms(t.elapsed());
            // Typing in the middle of what shows.
            let at = v.tv.characterIndexForInsertionAtPoint(NSPoint::new(100.0, v.visible_y() + SCREEN / 2.0));
            v.tv.setSelectedRange(NSRange::new(at, 0));
            let x = NSString::from_str("x");
            let k: Vec<f64> = (0..STEPS)
                .map(|_| {
                    let t = Instant::now();
                    unsafe { v.tv.insertText_replacementRange(&x, NSRange::new(NSNotFound as usize, 0)) };
                    v.layout_viewport();
                    ms(t.elapsed())
                })
                .collect();
            // Every fragment made, then all of them freed by a new font and a
            // new text.
            v.laid();
            let t = Instant::now();
            v.tv.setFont(Some(&other));
            v.layout_viewport();
            let t_font = ms(t.elapsed());
            v.laid();
            let t = Instant::now();
            v.tv.setString(&one);
            v.layout_viewport();
            let t_reset = ms(t.elapsed());
            // And again, to be freed with the view.
            v.tv.setString(&ns);
            v.layout_viewport();
            v.laid();
            freed_at.set(Instant::now());
            drop(v);
            (t_set, t_first, n_laid, t_jump, s, k, t_font, t_reset)
        });
        let t_free = ms(freed_at.get().elapsed());
        if run == 0 {
            continue;
        }
        font.push(t_font);
        reset.push(t_reset);
        free.push(t_free);
        set.push(t_set);
        first.push(t_first);
        jump.push(t_jump);
        laid.push(n_laid as f64);
        scroll.add(&s);
        keys.add(&k);
    }
    println!("{:<34} {:>9.3} ms", "setString", median(set));
    println!("{:<34} {:>9.3} ms", "first viewport layout", median(first));
    println!("{:<34} {:>9.0}", "fragments laid out by it", median(laid));
    println!("{:<34} {:>9.0} pt", "view height (estimated)", height);
    scroll.report("scroll step and layout");
    println!("{:<34} {:>9.3} ms", "jump to the middle and layout", median(jump));
    keys.report("keystroke and layout");
    println!("{:<34} {:>9.3} ms", "setFont, every fragment made", median(font));
    println!("{:<34} {:>9.3} ms", "setString, every fragment made", median(reset));
    println!("{:<34} {:>9.3} ms", "free the view, every fragment made", median(free));
}
