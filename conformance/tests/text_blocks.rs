//! Text blocks and tables (NSTextBlock, NSTextTableBlock, NSTextTable):
//! their settings, and where a layout manager puts the paragraphs they
//! enclose, the rects it reports for them, and the ranges an attributed
//! string finds for them.
//!
//! Text is in the 12-point monospaced system font in a 300-point-wide
//! container with the default line fragment padding (5); positions are
//! checked against the layout manager's own line height, not numbers that
//! depend on a platform's fonts.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAttributedStringAppKitAdditions, NSColor, NSFont, NSFontAttributeName, NSFontWeightRegular, NSLayoutManager,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSTextBlock, NSTextBlockDimension, NSTextBlockLayer,
    NSTextBlockValueType, NSTextBlockVerticalAlignment, NSTextContainer, NSTextStorage, NSTextTable, NSTextTableBlock,
    NSTextTableLayoutAlgorithm,
};
use objc2_foundation::{
    NSArray, NSCopying, NSDictionary, NSNotFound, NSPoint, NSRange, NSRect, NSRectEdge, NSSize, NSString,
};

use sidestep as _;

const W: f64 = 300.0;
const PAD: f64 = 5.0;
const ABS: NSTextBlockValueType = NSTextBlockValueType::AbsoluteValueType;
const PCT: NSTextBlockValueType = NSTextBlockValueType::PercentageValueType;
const EDGES: [NSRectEdge; 4] = [NSRectEdge::MinX, NSRectEdge::MinY, NSRectEdge::MaxX, NSRectEdge::MaxY];

/// The font the tests lay out with, made one test at a time: on CI's
/// macOS runner, AppKit once gave no font to one of several test threads
/// asking for it at once.
fn font() -> Retained<NSFont> {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular })
}

fn storage(text: &str) -> Retained<NSTextStorage> {
    let f = font();
    let attrs =
        NSDictionary::<NSString, AnyObject>::from_slices(&[unsafe { NSFontAttributeName }], &[&*f as &AnyObject]);
    unsafe { objc2::msg_send![NSTextStorage::alloc(), initWithString: &*NSString::from_str(text), attributes: &*attrs] }
}

fn set_blocks(ts: &NSTextStorage, at: usize, len: usize, blocks: &[Retained<NSTextBlock>]) {
    let style = NSMutableParagraphStyle::new();
    style.setTextBlocks(&NSArray::from_retained_slice(blocks));
    unsafe { ts.addAttribute_value_range(NSParagraphStyleAttributeName, &style, NSRange::new(at, len)) };
}

/// A block with the same margin, border and padding on every edge.
fn block(margin: f64, border: f64, padding: f64, width: Option<(f64, NSTextBlockValueType)>) -> Retained<NSTextBlock> {
    let b = NSTextBlock::new();
    b.setWidth_type_forLayer(margin, ABS, NSTextBlockLayer::Margin);
    b.setWidth_type_forLayer(border, ABS, NSTextBlockLayer::Border);
    b.setWidth_type_forLayer(padding, ABS, NSTextBlockLayer::Padding);
    if let Some((w, t)) = width {
        b.setContentWidth_type(w, t);
    }
    b
}

fn cell(table: &NSTextTable, row: isize, column: isize, span: isize) -> Retained<NSTextTableBlock> {
    NSTextTableBlock::initWithTable_startingRow_rowSpan_startingColumn_columnSpan(
        NSTextTableBlock::alloc(),
        table,
        row,
        1,
        column,
        span,
    )
}

struct Kit {
    ts: Retained<NSTextStorage>,
    lm: Retained<NSLayoutManager>,
    tc: Retained<NSTextContainer>,
}

fn kit(ts: Retained<NSTextStorage>) -> Kit {
    let lm = NSLayoutManager::new();
    let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(W, 1.0e7));
    lm.addTextContainer(&tc);
    ts.addLayoutManager(&lm);
    // Rects of what isn't laid out yet may be empty: lay it all out.
    lm.ensureLayoutForTextContainer(&tc);
    Kit { ts, lm, tc }
}

impl Kit {
    /// The line fragment holding glyph `i`, and the glyphs it holds.
    fn fragment(&self, i: usize) -> (NSRect, (usize, usize)) {
        let mut r = NSRange::new(0, 0);
        let rect = unsafe { self.lm.lineFragmentRectForGlyphAtIndex_effectiveRange(i, &mut r) };
        (rect, (r.location, r.length))
    }

    fn frag(&self, i: usize) -> NSRect {
        self.fragment(i).0
    }

    /// The line height: the first line's fragment's (text starting with a
    /// plain paragraph).
    fn h(&self) -> f64 {
        self.frag(0).size.height
    }

    fn used(&self) -> NSRect {
        self.lm.ensureLayoutForTextContainer(&self.tc);
        self.lm.usedRectForTextContainer(&self.tc)
    }

    fn glyph_at(&self, x: f64, y: f64) -> usize {
        let mut f = 0.0;
        unsafe {
            self.lm.glyphIndexForPoint_inTextContainer_fractionOfDistanceThroughGlyph(
                NSPoint::new(x, y),
                &self.tc,
                &mut f,
            )
        }
    }
}

impl Drop for Kit {
    fn drop(&mut self) {
        self.ts.removeLayoutManager(&self.lm);
    }
}

#[track_caller]
fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() < 0.01, "{what}: {a} != {b}");
}

#[track_caller]
fn rect_is(r: NSRect, x: f64, y: f64, w: f64, h: f64, what: &str) {
    close(r.origin.x, x, &format!("{what} x"));
    close(r.origin.y, y, &format!("{what} y"));
    close(r.size.width, w, &format!("{what} width"));
    close(r.size.height, h, &format!("{what} height"));
}

#[test]
fn block_settings() {
    let b = NSTextBlock::new();
    assert_eq!(b.contentWidth(), 0.0);
    assert_eq!(b.contentWidthValueType(), ABS);
    assert_eq!(b.verticalAlignment(), NSTextBlockVerticalAlignment::TopAlignment);
    assert!(b.backgroundColor().is_none());
    for layer in [NSTextBlockLayer::Padding, NSTextBlockLayer::Border, NSTextBlockLayer::Margin] {
        for e in EDGES {
            assert_eq!(b.widthForLayer_edge(layer, e), 0.0);
            assert_eq!(b.widthValueTypeForLayer_edge(layer, e), ABS);
        }
    }
    for e in EDGES {
        assert!(b.borderColorForEdge(e).is_none());
    }
    for d in [0, 1, 2, 4, 5, 6] {
        assert_eq!(b.valueForDimension(NSTextBlockDimension(d)), 0.0);
    }
    // The content width is the width dimension.
    b.setContentWidth_type(100.0, PCT);
    assert_eq!(b.valueForDimension(NSTextBlockDimension::Width), 100.0);
    assert_eq!(b.valueTypeForDimension(NSTextBlockDimension::Width), PCT);
    b.setValue_type_forDimension(50.0, ABS, NSTextBlockDimension::Width);
    assert_eq!((b.contentWidth(), b.contentWidthValueType()), (50.0, ABS));
    b.setValue_type_forDimension(40.0, ABS, NSTextBlockDimension::MinimumHeight);
    assert_eq!(b.valueForDimension(NSTextBlockDimension::MinimumHeight), 40.0);
    // A layer's width for every edge at once, or one edge.
    b.setWidth_type_forLayer(3.0, PCT, NSTextBlockLayer::Border);
    for e in EDGES {
        assert_eq!(
            (
                b.widthForLayer_edge(NSTextBlockLayer::Border, e),
                b.widthValueTypeForLayer_edge(NSTextBlockLayer::Border, e)
            ),
            (3.0, PCT)
        );
    }
    b.setWidth_type_forLayer_edge(7.0, ABS, NSTextBlockLayer::Margin, NSRectEdge::MaxY);
    assert_eq!(b.widthForLayer_edge(NSTextBlockLayer::Margin, NSRectEdge::MaxY), 7.0);
    assert_eq!(b.widthForLayer_edge(NSTextBlockLayer::Margin, NSRectEdge::MinY), 0.0);
    // Border colors, all edges or one.
    let red = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
    let blue = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 1.0);
    b.setBorderColor(Some(&red));
    b.setBorderColor_forEdge(Some(&blue), NSRectEdge::MaxY);
    for e in [NSRectEdge::MinX, NSRectEdge::MinY, NSRectEdge::MaxX] {
        assert!(b.borderColorForEdge(e).is_some_and(|c| std::ptr::eq(&*c, &*red)));
    }
    assert!(b.borderColorForEdge(NSRectEdge::MaxY).is_some_and(|c| std::ptr::eq(&*c, &*blue)));
    b.setBackgroundColor(Some(&blue));
    assert!(b.backgroundColor().is_some_and(|c| std::ptr::eq(&*c, &*blue)));
    b.setVerticalAlignment(NSTextBlockVerticalAlignment::MiddleAlignment);
    assert_eq!(b.verticalAlignment(), NSTextBlockVerticalAlignment::MiddleAlignment);
    // Blocks are equal only to themselves; a copy is another block.
    let copy = b.copy();
    assert!(!std::ptr::eq(&*copy, &*b));
    assert_ne!(copy, b);
    assert_ne!(NSTextBlock::new(), NSTextBlock::new());
    // Paragraph styles hold blocks as given, equal when the blocks are the
    // same.
    let (b1, b2) = (NSTextBlock::new(), NSTextBlock::new());
    let (p1, p2) = (NSMutableParagraphStyle::new(), NSMutableParagraphStyle::new());
    assert_eq!(p1.textBlocks().count(), 0);
    p1.setTextBlocks(&NSArray::from_retained_slice(std::slice::from_ref(&b1)));
    p2.setTextBlocks(&NSArray::from_retained_slice(std::slice::from_ref(&b2)));
    assert_ne!(p1, p2);
    p2.setTextBlocks(&NSArray::from_retained_slice(std::slice::from_ref(&b1)));
    assert_eq!(p1, p2);
    assert!(std::ptr::eq(&*p1.textBlocks().objectAtIndex(0), &*b1));
    // Tables and cells.
    let t = NSTextTable::new();
    assert_eq!(t.numberOfColumns(), 0);
    assert_eq!(t.layoutAlgorithm(), NSTextTableLayoutAlgorithm::AutomaticLayoutAlgorithm);
    assert!(!t.collapsesBorders());
    assert!(!t.hidesEmptyCells());
    t.setNumberOfColumns(3);
    t.setLayoutAlgorithm(NSTextTableLayoutAlgorithm::FixedLayoutAlgorithm);
    t.setCollapsesBorders(true);
    t.setHidesEmptyCells(true);
    assert_eq!(t.numberOfColumns(), 3);
    assert_eq!(t.layoutAlgorithm(), NSTextTableLayoutAlgorithm::FixedLayoutAlgorithm);
    assert!(t.collapsesBorders() && t.hidesEmptyCells());
    let c = NSTextTableBlock::initWithTable_startingRow_rowSpan_startingColumn_columnSpan(
        NSTextTableBlock::alloc(),
        &t,
        1,
        2,
        0,
        1,
    );
    assert_eq!((c.startingRow(), c.rowSpan(), c.startingColumn(), c.columnSpan()), (1, 2, 0, 1));
    assert!(std::ptr::eq(&*c.table(), &*t));
    assert_eq!(t.numberOfColumns(), 3);
    assert!(!std::ptr::eq(&*c.copy(), &*c));
}

#[test]
fn block_rects_on_their_own() {
    let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(W, 1.0e7));
    let at = |x, y| NSPoint::new(x, y);
    let r = |x, y, w, h| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
    let chars = NSRange::new(0, 5);
    // Margin 4, border 2, padding 10: 16 on each edge.
    let half = block(4.0, 2.0, 10.0, Some((50.0, PCT)));
    // Inside the rect's left layers and the point's top ones, half the
    // rect's width, down to its bottom.
    let l = half.rectForLayoutAtPoint_inRect_textContainer_characterRange(
        at(7.0, 15.0),
        r(5.0, 3.0, 200.0, 100.0),
        &tc,
        chars,
    );
    rect_is(l, 21.0, 31.0, 100.0, 72.0, "layout rect");
    let l = half.rectForLayoutAtPoint_inRect_textContainer_characterRange(
        at(0.0, 15.0),
        r(0.0, 0.0, 300.0, 1.0e7),
        &tc,
        chars,
    );
    rect_is(l, 16.0, 31.0, 150.0, 1.0e7 - 31.0, "layout rect in the container");
    // No wider than fits.
    let wide = block(4.0, 2.0, 10.0, Some((1000.0, ABS)));
    let l = wide.rectForLayoutAtPoint_inRect_textContainer_characterRange(
        at(0.0, 15.0),
        r(5.0, 0.0, 290.0, 1.0e7),
        &tc,
        chars,
    );
    assert_eq!(l.size.width, 258.0);
    // The bounds are the content with the layers around it.
    let content = r(21.0, 31.0, 50.0, 30.0);
    let b = half.boundsRectForContentRect_inRect_textContainer_characterRange(
        content,
        r(5.0, 0.0, 290.0, 1.0e7),
        &tc,
        chars,
    );
    rect_is(b, 5.0, 15.0, 82.0, 62.0, "bounds rect");
    // Percentages are of the rect's width, down too.
    let pct = NSTextBlock::new();
    pct.setWidth_type_forLayer(10.0, PCT, NSTextBlockLayer::Padding);
    pct.setContentWidth_type(100.0, PCT);
    let l = pct.rectForLayoutAtPoint_inRect_textContainer_characterRange(
        at(0.0, 0.0),
        r(0.0, 0.0, 200.0, 1000.0),
        &tc,
        chars,
    );
    rect_is(l, 20.0, 20.0, 160.0, 980.0, "percentage layers");
}

const TEXT: &str = "Before\nInside one\nInside two\nAfter";

#[test]
fn a_block_insets_its_paragraphs() {
    // A full-width block with margin 4, border 2 and padding 10 around
    // "Inside one\nInside two\n".
    let b = block(4.0, 2.0, 10.0, Some((100.0, PCT)));
    let ts = storage(TEXT);
    set_blocks(&ts, 7, 22, std::slice::from_ref(&b));
    let k = kit(ts);
    let h = k.h();
    // Fragments start inside the block's layers and end inside them; the
    // content is the container less its padding, less the layers.
    let (f, r) = k.fragment(7);
    assert_eq!(r, (7, 11));
    rect_is(f, 16.0, h + 16.0, W - 32.0, h, "first fragment in the block");
    rect_is(k.frag(18), 16.0, 2.0 * h + 16.0, W - 32.0, h, "second");
    // After the block: below its bottom layers.
    rect_is(k.frag(29), 0.0, 3.0 * h + 32.0, W, h, "after the block");
    // The used rect takes in the block's bounds.
    rect_is(k.used(), 0.0, 0.0, W - PAD, 4.0 * h + 32.0, "used rect");
    // The block's layout rect (to the container's bottom) and bounds.
    let range = NSRange::new(7, 22);
    let layout = k.lm.layoutRectForTextBlock_glyphRange(&b, range);
    rect_is(layout, 21.0, h + 16.0, W - 42.0, 1.0e7 - h - 16.0, "layout rect");
    let bounds = k.lm.boundsRectForTextBlock_glyphRange(&b, range);
    rect_is(bounds, PAD, h, W - 10.0, 2.0 * h + 32.0, "bounds");
    let mut eff = NSRange::new(0, 0);
    let at = unsafe { k.lm.layoutRectForTextBlock_atIndex_effectiveRange(&b, 20, &mut eff) };
    assert_eq!((eff.location, eff.length), (7, 22));
    rect_is(at, 21.0, h + 16.0, W - 42.0, 1.0e7 - h - 16.0, "layout rect at an index");
    // Outside the block: nothing.
    let at = unsafe { k.lm.boundsRectForTextBlock_atIndex_effectiveRange(&b, 2, &mut eff) };
    assert_eq!(at, NSRect::ZERO);
    assert_eq!(eff.location, NSNotFound as usize);
    // Hits go to the block's text.
    assert_eq!(k.glyph_at(16.0 + PAD + 1.0, h + 16.0 + 1.0), 7);
    // The attributed string finds the block's paragraphs.
    let found = k.ts.rangeOfTextBlock_atIndex(&b, 9);
    assert_eq!((found.location, found.length), (7, 22));
    assert_eq!(k.ts.rangeOfTextBlock_atIndex(&b, 2).location, NSNotFound as usize);
}

#[test]
fn content_widths() {
    let lay = |width: (f64, NSTextBlockValueType)| {
        let ts = storage(TEXT);
        set_blocks(&ts, 7, 22, &[block(4.0, 2.0, 10.0, Some(width))]);
        let k = kit(ts);
        (k.frag(7), k.used())
    };
    // Absolute: that wide, with the line fragment padding at each end.
    let (f, used) = lay((100.0, ABS));
    assert_eq!(f.size.width, 100.0 + 2.0 * PAD);
    assert_eq!(used.size.width, 16.0 + PAD + 100.0 + 16.0);
    // A percentage of the container's width less its padding.
    let (f, _) = lay((50.0, PCT));
    assert_eq!(f.size.width, 0.5 * (W - 2.0 * PAD) + 2.0 * PAD);
}

#[test]
fn edges_and_percentages() {
    // A rule on the left, more padding across than down, a margin below.
    let a = NSTextBlock::new();
    let set = |w, layer, edge| a.setWidth_type_forLayer_edge(w, ABS, layer, edge);
    set(1.0, NSTextBlockLayer::Border, NSRectEdge::MinX);
    set(9.0, NSTextBlockLayer::Padding, NSRectEdge::MinY);
    set(7.0, NSTextBlockLayer::Padding, NSRectEdge::MaxY);
    set(14.0, NSTextBlockLayer::Padding, NSRectEdge::MinX);
    set(12.0, NSTextBlockLayer::Padding, NSRectEdge::MaxX);
    set(3.0, NSTextBlockLayer::Margin, NSRectEdge::MinY);
    set(4.0, NSTextBlockLayer::Margin, NSRectEdge::MaxY);
    a.setContentWidth_type(100.0, PCT);
    let ts = storage(TEXT);
    set_blocks(&ts, 7, 22, &[a]);
    let k = kit(ts);
    let h = k.h();
    rect_is(k.frag(7), 15.0, h + 12.0, W - 27.0, h, "inside");
    close(k.frag(29).origin.y, 3.0 * h + 12.0 + 11.0, "after");
    // Percentage padding, of the container's width less its padding, down
    // as well as across.
    let p = NSTextBlock::new();
    p.setWidth_type_forLayer(10.0, PCT, NSTextBlockLayer::Padding);
    p.setContentWidth_type(50.0, PCT);
    let ts = storage(TEXT);
    set_blocks(&ts, 7, 22, &[p]);
    let k = kit(ts);
    let inset = 0.1 * (W - 2.0 * PAD);
    let f = k.frag(7);
    close(f.origin.x, inset, "percentage inset across");
    close(f.origin.y, h + inset, "percentage inset down");
}

#[test]
fn nested_and_neighbouring_blocks() {
    let outer = block(0.0, 1.0, 5.0, Some((100.0, PCT)));
    let inner = block(2.0, 0.0, 8.0, Some((100.0, PCT)));
    // Both around the same paragraphs: the tops add up, and the text goes
    // on below the lower bottom.
    let ts = storage(TEXT);
    set_blocks(&ts, 7, 22, &[outer.clone(), inner.clone()]);
    let k = kit(ts);
    let h = k.h();
    rect_is(k.frag(7), 16.0, h + 16.0, W - 32.0, h, "in both");
    close(k.frag(29).origin.y, 3.0 * h + 16.0 + 10.0, "after both");
    let inner_bounds = k.lm.boundsRectForTextBlock_glyphRange(&inner, NSRange::new(7, 22));
    rect_is(inner_bounds, 11.0, h + 6.0, W - 22.0, 2.0 * h + 20.0, "inner bounds");
    let layout = k.lm.layoutRectForTextBlock_glyphRange(&outer, NSRange::new(7, 22));
    rect_is(layout, 11.0, h + 6.0, W - 22.0, 1.0e7 - h - 6.0, "outer layout rect");
    drop(k);
    // The inner block around the middle paragraph only.
    let ts = storage("Before\nOne\nTwo\nThree\nAfter");
    set_blocks(&ts, 7, 4, std::slice::from_ref(&outer));
    set_blocks(&ts, 11, 4, &[outer.clone(), inner.clone()]);
    set_blocks(&ts, 15, 6, std::slice::from_ref(&outer));
    let k = kit(ts);
    rect_is(k.frag(7), 6.0, h + 6.0, W - 12.0, h, "outer only");
    rect_is(k.frag(11), 16.0, 2.0 * h + 16.0, W - 32.0, h, "inner");
    rect_is(k.frag(15), 6.0, 3.0 * h + 26.0, W - 12.0, h, "outer again");
    close(k.frag(21).origin.y, 4.0 * h + 32.0, "after");
    let bounds = k.lm.boundsRectForTextBlock_glyphRange(&outer, NSRange::new(7, 14));
    rect_is(bounds, PAD, h, W - 10.0, 3.0 * h + 32.0, "outer bounds");
    let found = k.ts.rangeOfTextBlock_atIndex(&inner, 12);
    assert_eq!((found.location, found.length), (11, 4));
    drop(k);
    // Two blocks one after the other: each has its own layers.
    let b2 = block(3.0, 0.0, 6.0, Some((100.0, PCT)));
    let ts = storage(TEXT);
    set_blocks(&ts, 7, 11, std::slice::from_ref(&outer));
    set_blocks(&ts, 18, 11, std::slice::from_ref(&b2));
    let k = kit(ts);
    rect_is(k.frag(18), 9.0, 2.0 * h + 12.0 + 9.0, W - 18.0, h, "second block");
    close(k.frag(29).origin.y, 3.0 * h + 12.0 + 18.0, "after");
    drop(k);
    // One block around two paragraphs apart is in two places.
    let ts = storage("Before\nOne\nTwo\nThree\nAfter");
    set_blocks(&ts, 7, 4, std::slice::from_ref(&b2));
    set_blocks(&ts, 15, 6, std::slice::from_ref(&b2));
    let k = kit(ts);
    rect_is(k.frag(11), 0.0, 2.0 * h + 18.0, W, h, "between");
    rect_is(k.frag(15), 9.0, 3.0 * h + 27.0, W - 18.0, h, "second part");
    let found = k.ts.rangeOfTextBlock_atIndex(&b2, 16);
    assert_eq!((found.location, found.length), (15, 6));
    let bounds = unsafe { k.lm.boundsRectForTextBlock_atIndex_effectiveRange(&b2, 16, std::ptr::null_mut()) };
    rect_is(bounds, PAD, 3.0 * h + 18.0, W - 10.0, h + 18.0, "second part's bounds");
}

#[test]
fn paragraph_spacing_in_a_block() {
    let b = block(3.0, 0.0, 6.0, Some((100.0, PCT)));
    let ts = storage(TEXT);
    let style = NSMutableParagraphStyle::new();
    style.setTextBlocks(&NSArray::from_retained_slice(std::slice::from_ref(&b)));
    style.setParagraphSpacing(6.0);
    style.setParagraphSpacingBefore(3.0);
    unsafe { ts.addAttribute_value_range(NSParagraphStyleAttributeName, &style, NSRange::new(7, 22)) };
    let k = kit(ts);
    let h = k.h();
    // The fragments take in the spacing, as outside blocks.
    rect_is(k.frag(7), 9.0, h + 9.0, W - 18.0, h + 9.0, "first");
    rect_is(k.frag(18), 9.0, 2.0 * h + 18.0, W - 18.0, h + 9.0, "second");
    // The block ends below the last line, before its spacing after; the
    // text goes on below whichever is lower.
    let bounds = k.lm.boundsRectForTextBlock_glyphRange(&b, NSRange::new(7, 22));
    rect_is(bounds, PAD, h, W - 10.0, 2.0 * h + 30.0, "bounds");
    close(k.frag(29).origin.y, 3.0 * h + 30.0, "after");
}

#[test]
fn a_block_first_and_last() {
    let b = block(3.0, 0.0, 6.0, Some((100.0, PCT)));
    let ts = storage("One\nTwo");
    set_blocks(&ts, 0, 4, std::slice::from_ref(&b));
    let k = kit(ts);
    let h = k.frag(4).size.height;
    rect_is(k.frag(0), 9.0, 9.0, W - 18.0, h, "first paragraph");
    close(k.frag(4).origin.y, h + 18.0, "after");
    drop(k);
    // At the text's end: the used rect takes in its bottom layers; after a
    // final separator the extra line fragment is outside it.
    let ts = storage("Before\nInside");
    set_blocks(&ts, 7, 6, std::slice::from_ref(&b));
    let k = kit(ts);
    let h = k.h();
    close(k.used().size.height, 2.0 * h + 18.0, "used height");
    drop(k);
    let ts = storage("Before\nInside\n");
    set_blocks(&ts, 7, 7, std::slice::from_ref(&b));
    let k = kit(ts);
    let extra = k.lm.extraLineFragmentRect();
    rect_is(extra, 0.0, 2.0 * h + 18.0, W, h, "extra line fragment");
}

/// A table of two columns (cells with border 1 and padding 4) of "a1",
/// "b1…", "a2" and "b2", after "Before".
fn two_by_two(table: &NSTextTable, b1: &str, a2: &str) -> (Kit, Vec<Retained<NSTextTableBlock>>) {
    let cells = ["a1\n".to_string(), format!("{b1}\n"), format!("{a2}\n"), "b2\n".to_string()];
    let ts = storage(&format!("Before\n{}After", cells.concat()));
    let mut at = 7;
    let mut blocks = Vec::new();
    for (k, text) in cells.iter().enumerate() {
        let c = cell(table, (k / 2) as isize, (k % 2) as isize, 1);
        c.setWidth_type_forLayer(1.0, ABS, NSTextBlockLayer::Border);
        c.setWidth_type_forLayer(4.0, ABS, NSTextBlockLayer::Padding);
        let len = text.encode_utf16().count();
        set_blocks(&ts, at, len, &[Retained::into_super(c.clone())]);
        at += len;
        blocks.push(c);
    }
    (kit(ts), blocks)
}

#[test]
fn a_table_lays_its_rows_out_across() {
    for algorithm in
        [NSTextTableLayoutAlgorithm::AutomaticLayoutAlgorithm, NSTextTableLayoutAlgorithm::FixedLayoutAlgorithm]
    {
        let table = NSTextTable::new();
        table.setNumberOfColumns(2);
        table.setLayoutAlgorithm(algorithm);
        let (k, cells) = two_by_two(&table, "b1 longer text", "a2");
        let h = k.h();
        let column = (W - 2.0 * PAD) / 2.0;
        // Side by side, each column's cell inside its layers (5 each side).
        rect_is(k.frag(7), PAD, h + 5.0, column, h, "a1");
        rect_is(k.frag(10), PAD + column, h + 5.0, column, h, "b1");
        rect_is(k.frag(25), PAD, 2.0 * h + 15.0, column, h, "a2");
        rect_is(k.frag(28), PAD + column, 2.0 * h + 15.0, column, h, "b2");
        close(k.frag(31).origin.y, 3.0 * h + 20.0, "after");
        let bounds = k.lm.boundsRectForTextBlock_glyphRange(&cells[1], NSRange::new(10, 15));
        rect_is(bounds, PAD + column, h, column, h + 10.0, "b1's bounds");
        let t = k.ts.rangeOfTextTable_atIndex(&table, 9);
        assert_eq!((t.location, t.length), (7, 24));
        let c = k.ts.rangeOfTextBlock_atIndex(&cells[0], 7);
        assert_eq!((c.location, c.length), (7, 3));
    }
}

#[test]
fn table_rows_are_as_tall_as_their_tallest_cell() {
    let table = NSTextTable::new();
    table.setNumberOfColumns(2);
    let long = "b1 is a much longer piece of text that wraps";
    let (k, cells) = two_by_two(&table, long, "a2\nmore");
    let h = k.h();
    let column = (W - 2.0 * PAD) / 2.0;
    let b1 = 10;
    let a1_bounds = k.lm.boundsRectForTextBlock_glyphRange(&cells[0], NSRange::new(7, 3));
    // b1 wraps in its column; its row is as tall as its lines.
    let lines = {
        let mut n = 0;
        let mut i = b1;
        while i < b1 + long.len() + 1 {
            let (_, r) = k.fragment(i);
            i = r.0 + r.1;
            n += 1;
        }
        n
    };
    assert!(lines > 1, "b1 wraps");
    let row = lines as f64 * h + 10.0;
    rect_is(a1_bounds, PAD, h, column, row, "a1's bounds run the row");
    // The second row: a cell of two paragraphs, stacked.
    let a2 = b1 + long.len() + 1;
    rect_is(k.frag(a2), PAD, h + row + 5.0, column, h, "a2");
    rect_is(k.frag(a2 + 3), PAD, 2.0 * h + row + 5.0, column, h, "more");
    rect_is(k.frag(a2 + 8), PAD + column, h + row + 5.0, column, h, "b2");
    close(k.frag(a2 + 11).origin.y, 3.0 * h + row + 10.0, "after");
    // Hit testing goes by column, and down by line.
    let y = h + 6.0;
    assert_eq!(k.glyph_at(20.0, y), 8);
    assert_eq!(k.glyph_at(PAD + column + 10.0, y), b1);
    let second_line = k.fragment(b1).1;
    assert_eq!(k.glyph_at(PAD + column + 10.0, y + h), second_line.0 + second_line.1);
}

#[test]
fn table_widths() {
    // A table with its own layers, three columns.
    let table = NSTextTable::new();
    table.setNumberOfColumns(3);
    table.setWidth_type_forLayer(3.0, ABS, NSTextBlockLayer::Padding);
    table.setWidth_type_forLayer(1.0, ABS, NSTextBlockLayer::Border);
    table.setContentWidth_type(100.0, PCT);
    let ts = storage("Before\nx\ny\nz\nAfter");
    for c in 0..3 {
        set_blocks(&ts, 7 + 2 * c, 2, &[Retained::into_super(cell(&table, 0, c as isize, 1))]);
    }
    let k = kit(ts);
    let h = k.h();
    let column = (W - 2.0 * PAD - 8.0) / 3.0;
    for c in 0..3 {
        rect_is(k.frag(7 + 2 * c), 4.0 + c as f64 * column, h + 4.0, column + 2.0 * PAD, h, "cell");
    }
    drop(k);
    // A cell spanning both columns, then a row of two.
    let table = NSTextTable::new();
    table.setNumberOfColumns(2);
    let ts = storage("Before\nwide\nl\nr\nAfter");
    set_blocks(&ts, 7, 5, &[Retained::into_super(cell(&table, 0, 0, 2))]);
    set_blocks(&ts, 12, 2, &[Retained::into_super(cell(&table, 1, 0, 1))]);
    set_blocks(&ts, 14, 2, &[Retained::into_super(cell(&table, 1, 1, 1))]);
    let k = kit(ts);
    let column = (W - 2.0 * PAD) / 2.0;
    rect_is(k.frag(7), 0.0, h, W, h, "spanning cell");
    rect_is(k.frag(12), 0.0, 2.0 * h, column + 2.0 * PAD, h, "left");
    rect_is(k.frag(14), column, 2.0 * h, column + 2.0 * PAD, h, "right");
    drop(k);
    // A cell with a content width sets its column's; the rest share what
    // is left.
    let table = NSTextTable::new();
    table.setNumberOfColumns(2);
    let ts = storage("Before\na\nb\nAfter");
    let first = cell(&table, 0, 0, 1);
    first.setContentWidth_type(50.0, ABS);
    set_blocks(&ts, 7, 2, &[Retained::into_super(first)]);
    set_blocks(&ts, 9, 2, &[Retained::into_super(cell(&table, 0, 1, 1))]);
    let k = kit(ts);
    rect_is(k.frag(7), 0.0, h, 50.0 + 2.0 * PAD, h, "set width");
    rect_is(k.frag(9), 50.0, h, W - 2.0 * PAD - 50.0 + 2.0 * PAD, h, "the rest");
    drop(k);
    // A table in a block: inside the block's layers.
    let table = NSTextTable::new();
    table.setNumberOfColumns(2);
    let enclosing = block(0.0, 1.0, 10.0, Some((100.0, PCT)));
    let ts = storage("Before\nL\nR\nAfter");
    for (c, at) in [(0, 7), (1, 9)] {
        set_blocks(&ts, at, 2, &[enclosing.clone(), Retained::into_super(cell(&table, 0, c, 1))]);
    }
    let k = kit(ts);
    let column = (W - 2.0 * PAD - 22.0) / 2.0;
    rect_is(k.frag(7), 11.0, h + 11.0, column + 2.0 * PAD, h, "left in the block");
    rect_is(k.frag(9), 11.0 + column, h + 11.0, column + 2.0 * PAD, h, "right in the block");
    close(k.frag(11).origin.y, 2.0 * h + 22.0, "after the block");
}

#[test]
fn collapsed_borders_share_a_width() {
    let pitch = |collapse: bool| {
        let table = NSTextTable::new();
        table.setNumberOfColumns(2);
        table.setCollapsesBorders(collapse);
        table.setContentWidth_type(100.0, PCT);
        let ts = storage("Before\na1\nb1\na2\nb2\nAfter");
        for k in 0..4 {
            let c = cell(&table, (k / 2) as isize, (k % 2) as isize, 1);
            c.setWidth_type_forLayer(4.0, ABS, NSTextBlockLayer::Border);
            set_blocks(&ts, 7 + 3 * k, 3, &[Retained::into_super(c)]);
        }
        let k = kit(ts);
        let h = k.h();
        (k.frag(13).origin.y - k.frag(7).origin.y - h, k.frag(10).origin.x - k.frag(7).origin.x)
    };
    // Between rows: one border when they collapse, two when not; the
    // columns stay as wide.
    let (open, open_across) = pitch(false);
    let (shared, shared_across) = pitch(true);
    close(open, 8.0, "separate borders");
    close(shared, 4.0, "collapsed borders");
    close(open_across, shared_across, "columns");
}

#[test]
fn editing_in_a_table_keeps_rows_together() {
    let table = NSTextTable::new();
    table.setNumberOfColumns(2);
    let (k, _) = two_by_two(&table, "b1", "a2");
    let h = k.h();
    // Lengthen a1 until it wraps: its row grows, and b1 stays at its top.
    let long = " and a great deal more text so that it wraps";
    k.ts.replaceCharactersInRange_withString(NSRange::new(9, 0), &NSString::from_str(long));
    let b1 = 10 + long.len();
    close(k.frag(b1).origin.y, h + 5.0, "b1 stays at the row's top");
    let (_, first) = k.fragment(7);
    assert!(first.1 < 3 + long.len(), "a1 wraps");
    let a2 = b1 + 3;
    let a1_lines = {
        let (mut n, mut i) = (0, 7);
        while i < b1 {
            let (_, r) = k.fragment(i);
            i = r.0 + r.1;
            n += 1;
        }
        n
    };
    close(k.frag(a2).origin.y, h + a1_lines as f64 * h + 15.0, "the next row moves down");
}
