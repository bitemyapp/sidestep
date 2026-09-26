//! A text editor window: an `NSTextView` in a scroll view, written only
//! against objc2-app-kit, so it runs on AppKit on macOS and on Sidestep on
//! Linux.
//!
//! SCENARIO: styled text with a selection and a find highlight (editor, the
//! default), text typed a character at a time (typing), a long document
//! for scrolling and editing (big), text blocks and a table, as a chat
//! transcript sets them (blocks), or the styled text typed into, and then
//! deleted from, so its paragraph wraps onto more lines and back and the
//! text below moves (reflow).
//! TEXTEDIT_QUIT_AFTER: seconds until the app terminates itself.

use std::cell::{Cell, OnceCell, RefCell};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions,
    NSBackgroundColorAttributeName, NSBackingStoreType, NSColor, NSFont, NSFontAttributeName, NSFontWeightBold,
    NSFontWeightRegular, NSForegroundColorAttributeName, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
    NSTextBlock, NSTextBlockLayer, NSTextBlockValueType, NSTextInputClient, NSTextTable, NSTextTableBlock, NSTextView,
    NSUnderlineStyleAttributeName, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSDictionary, NSMutableAttributedString, NSNotFound, NSNotification, NSNumber,
    NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSRectEdge, NSSize, NSString, NSTimer, ns_string,
};

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

type Attrs = Retained<NSDictionary<NSString, AnyObject>>;

fn attrs(pairs: &[(&NSString, &AnyObject)]) -> Attrs {
    let keys: Vec<&NSString> = pairs.iter().map(|p| p.0).collect();
    let values: Vec<&AnyObject> = pairs.iter().map(|p| p.1).collect();
    NSDictionary::from_slices(&keys, &values)
}

fn piece(text: &str, a: &Attrs) -> Retained<NSAttributedString> {
    unsafe { NSAttributedString::new_with_attributes(&NSString::from_str(text), a) }
}

/// The editor scenario's text: a heading, styled words, wrapped
/// paragraphs, a centered line.
fn styled_text() -> Retained<NSMutableAttributedString> {
    let body_font = NSFont::systemFontOfSize(15.0);
    let mono = unsafe { NSFont::monospacedSystemFontOfSize_weight(14.0, NSFontWeightRegular) };
    let bold = unsafe { NSFont::systemFontOfSize_weight(24.0, NSFontWeightBold) };
    let ink = NSColor::colorWithSRGBRed_green_blue_alpha(0.13, 0.13, 0.16, 1.0);
    let accent = NSColor::colorWithSRGBRed_green_blue_alpha(0.75, 0.20, 0.35, 1.0);
    let gold = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.90, 0.55, 1.0);
    let spaced = NSMutableParagraphStyle::new();
    spaced.setParagraphSpacing(10.0);
    let centered = NSMutableParagraphStyle::new();
    centered.setAlignment(objc2_app_kit::NSTextAlignment::Center);
    centered.setParagraphSpacing(10.0);
    let underline = NSNumber::new_isize(1);
    let (f, c, p) = unsafe { (NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName) };
    let heading = attrs(&[(f, &bold), (c, &accent), (p, &spaced)]);
    let body = attrs(&[(f, &body_font), (c, &ink), (p, &spaced)]);
    let code = attrs(&[(f, &mono), (c, &accent), (p, &spaced)]);
    let marked = attrs(&[(f, &body_font), (c, &ink), (p, &spaced), (unsafe { NSBackgroundColorAttributeName }, &gold)]);
    let lined =
        attrs(&[(f, &body_font), (c, &ink), (p, &spaced), (unsafe { NSUnderlineStyleAttributeName }, &underline)]);
    let middle = attrs(&[(f, &body_font), (c, &accent), (p, &centered)]);
    let out = NSMutableAttributedString::new();
    for (text, a) in [
        ("Sidestep text editing\n", &heading),
        ("This window is an ", &body),
        ("NSTextView", &code),
        (
            " in a scroll view: its text lives in an NSTextStorage, is laid out by an NSLayoutManager in an NSTextContainer, and is ",
            &body,
        ),
        ("edited", &marked),
        (" with the standard commands, ", &body),
        ("undo", &lined),
        (", the clipboard and input methods.\n", &body),
        (
            "Long paragraphs wrap at the container's width, which tracks the view's, so resizing the window lays the text out again. Each edit lays out only the paragraph it touches; the rest keeps its lines.\n",
            &body,
        ),
        ("Centered, in color\n", &middle),
        ("fn main() { println!(\"hello from objc2\"); }\n", &code),
        ("The end.", &body),
    ] {
        out.appendAttributedString(&piece(text, a));
    }
    out
}

fn rgb(r: f64, g: f64, b: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
}

/// A block as wide as the text, with padding across and down.
fn padded_block(across: f64, down: f64) -> Retained<NSTextBlock> {
    let block = NSTextBlock::new();
    block.setContentWidth_type(100.0, NSTextBlockValueType::PercentageValueType);
    let abs = NSTextBlockValueType::AbsoluteValueType;
    for (edge, w) in
        [(NSRectEdge::MinX, across), (NSRectEdge::MaxX, across), (NSRectEdge::MinY, down), (NSRectEdge::MaxY, down)]
    {
        block.setWidth_type_forLayer_edge(w, abs, NSTextBlockLayer::Padding, edge);
    }
    block
}

/// The blocks scenario's text: a message in a tinted box with a rule on
/// its left, a code block inside it, a quotation, and a table whose
/// header is ruled above and below.
fn blocks_text() -> Retained<NSMutableAttributedString> {
    let abs = NSTextBlockValueType::AbsoluteValueType;
    let body_font = NSFont::systemFontOfSize(14.0);
    let mono = unsafe { NSFont::monospacedSystemFontOfSize_weight(13.0, NSFontWeightRegular) };
    let bold = unsafe { NSFont::systemFontOfSize_weight(14.0, NSFontWeightBold) };
    let ink = rgb(0.13, 0.13, 0.16);
    let (f, c, p) = unsafe { (NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName) };
    let style = |blocks: &[Retained<NSTextBlock>], spacing: f64| {
        let s = NSMutableParagraphStyle::new();
        s.setParagraphSpacing(spacing);
        if !blocks.is_empty() {
            s.setTextBlocks(&NSArray::from_retained_slice(blocks));
        }
        s
    };
    // The message: a tinted surface with a plum rule on the left.
    let message = padded_block(14.0, 9.0);
    message.setBackgroundColor(Some(&rgb(0.96, 0.95, 0.93)));
    message.setBorderColor(Some(&rgb(0.45, 0.25, 0.40)));
    message.setWidth_type_forLayer_edge(3.0, abs, NSTextBlockLayer::Border, NSRectEdge::MinX);
    message.setWidth_type_forLayer_edge(8.0, abs, NSTextBlockLayer::Margin, NSRectEdge::MaxY);
    // Code inside it: a lighter box with a hairline border.
    let code = padded_block(12.0, 7.0);
    code.setBackgroundColor(Some(&rgb(1.0, 1.0, 1.0)));
    code.setBorderColor(Some(&rgb(0.80, 0.78, 0.74)));
    code.setWidth_type_forLayer(1.0, abs, NSTextBlockLayer::Border);
    code.setWidth_type_forLayer_edge(6.0, abs, NSTextBlockLayer::Margin, NSRectEdge::MinY);
    // A quotation: a gold rule, indented.
    let quote = padded_block(12.0, 2.0);
    quote.setBorderColor(Some(&rgb(0.80, 0.62, 0.20)));
    quote.setWidth_type_forLayer_edge(3.0, abs, NSTextBlockLayer::Border, NSRectEdge::MinX);
    quote.setWidth_type_forLayer_edge(6.0, abs, NSTextBlockLayer::Margin, NSRectEdge::MaxY);
    let out = NSMutableAttributedString::new();
    let add = |text: &str, font: &NSFont, blocks: &[Retained<NSTextBlock>], spacing: f64| {
        let a = attrs(&[(f, font), (c, &ink), (p, &style(blocks, spacing))]);
        out.appendAttributedString(&piece(text, &a));
    };
    add("Text blocks and tables\n", &bold, &[], 8.0);
    let m = std::slice::from_ref(&message);
    add("A message sits in a block of its own: a tinted surface with a rule on its left.\n", &body_font, m, 6.0);
    add("It can hold code in a block inside it:\n", &body_font, m, 0.0);
    let mc = [message.clone(), code.clone()];
    add("fn main() {\n", &mono, &mc, 0.0);
    add("    println!(\"hello from a text block\");\n", &mono, &mc, 0.0);
    add("}\n", &mono, &mc, 0.0);
    add("And the message goes on after it.\n", &body_font, m, 0.0);
    add(
        "A quotation is a block with a rule, indented from the text around it.\n",
        &body_font,
        std::slice::from_ref(&quote),
        0.0,
    );
    // A table: the header ruled above and below, hairlines between rows.
    let table = NSTextTable::new();
    table.setNumberOfColumns(3);
    table.setContentWidth_type(100.0, NSTextBlockValueType::PercentageValueType);
    table.setCollapsesBorders(true);
    let rows = [
        ["Class", "What it is", "Lines"],
        ["NSTextBlock", "a box around paragraphs", "480"],
        ["NSTextTable", "columns of cells, laid out a row at a time", "260"],
    ];
    let header_rule = rgb(0.35, 0.33, 0.30);
    let rule = rgb(0.80, 0.78, 0.74);
    for (r, row) in rows.iter().enumerate() {
        for (col, text) in row.iter().enumerate() {
            let cell = NSTextTableBlock::initWithTable_startingRow_rowSpan_startingColumn_columnSpan(
                NSTextTableBlock::alloc(),
                &table,
                r as isize,
                1,
                col as isize,
                1,
            );
            let (top, bottom, color) = if r == 0 { (1.0, 1.0, &header_rule) } else { (0.0, 0.5, &rule) };
            cell.setBorderColor(Some(color));
            cell.setWidth_type_forLayer_edge(top, abs, NSTextBlockLayer::Border, NSRectEdge::MinY);
            cell.setWidth_type_forLayer_edge(bottom, abs, NSTextBlockLayer::Border, NSRectEdge::MaxY);
            for edge in [NSRectEdge::MinY, NSRectEdge::MaxY] {
                cell.setWidth_type_forLayer_edge(6.0, abs, NSTextBlockLayer::Padding, edge);
            }
            cell.setWidth_type_forLayer_edge(
                if col == 0 { 2.0 } else { 10.0 },
                abs,
                NSTextBlockLayer::Padding,
                NSRectEdge::MinX,
            );
            cell.setWidth_type_forLayer_edge(10.0, abs, NSTextBlockLayer::Padding, NSRectEdge::MaxX);
            let font = if r == 0 { &bold } else { &body_font };
            add(&format!("{text}\n"), font, &[Retained::into_super(cell)], 0.0);
        }
    }
    add("Text after the table.", &body_font, &[], 0.0);
    out
}

fn big_text() -> String {
    (0..20_000)
        .map(|i| format!("{i:05}  The quick brown fox jumps over the lazy dog, line {i} of a long document.\n"))
        .collect()
}

#[derive(Default)]
struct DelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    timers: RefCell<Vec<Retained<NSTimer>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextEditDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            self.open_window();
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate(&self, _sender: &NSApplication) -> bool {
            true
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars::default());
        unsafe { msg_send![super(this), init] }
    }

    fn every(&self, seconds: f64, repeats: bool, f: impl Fn() + 'static) {
        let block = RcBlock::new(move |_: NonNull<NSTimer>| f());
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, repeats, &block) };
        self.ivars().timers.borrow_mut().push(timer);
    }

    fn open_window(&self) {
        let mtm = self.mtm();
        let scenario = std::env::var("SCENARIO").unwrap_or_else(|_| "editor".into());
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(720.0, 520.0));
        let style_mask = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style_mask,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Sidestep text editor"));
        let scroll = NSTextView::scrollableTextView(mtm);
        scroll.setFrame(frame);
        scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        // The text view fills the scroll view, following its size.
        let tv: Retained<NSTextView> = scroll.documentView().unwrap().downcast().unwrap();
        tv.setTextContainerInset(NSSize::new(12.0, 12.0));
        tv.setAllowsUndo(true);
        let storage = unsafe { tv.textStorage() }.unwrap();
        match scenario.as_str() {
            "big" => {
                if let Some(lm) = unsafe { tv.layoutManager() } {
                    lm.setAllowsNonContiguousLayout(true);
                }
                let mono = unsafe { NSFont::monospacedSystemFontOfSize_weight(13.0, NSFontWeightRegular) };
                tv.setFont(Some(&mono));
                tv.setString(&NSString::from_str(&big_text()));
                tv.setSelectedRange(NSRange::new(0, 0));
            }
            "typing" => {
                tv.setFont(Some(&NSFont::systemFontOfSize(16.0)));
                let text: Vec<char> =
                    "Typed a character at a time, each an edit transaction with its own layout.".chars().collect();
                let at = Cell::new(0usize);
                let view = tv.clone();
                self.every(0.05, true, move || {
                    let i = at.get();
                    if i < text.len() {
                        let s = NSString::from_str(&text[i].to_string());
                        unsafe {
                            NSTextInputClient::insertText_replacementRange(
                                &*view,
                                &s,
                                NSRange::new(NSNotFound as usize, 0),
                            )
                        };
                        at.set(i + 1);
                    }
                });
            }
            "blocks" => {
                let m: &NSMutableAttributedString = &storage;
                m.setAttributedString(&blocks_text());
                tv.setSelectedRange(NSRange::new(0, 0));
            }
            "reflow" => {
                let m: &NSMutableAttributedString = &storage;
                m.setAttributedString(&styled_text());
                tv.setSelectedRange(NSRange::new(60, 0));
                let step = Cell::new(0usize);
                let view = tv.clone();
                self.every(0.02, true, move || {
                    let i = step.get();
                    step.set(i + 1);
                    // 120 words typed, then 60 of them deleted.
                    if i < 120 {
                        let s = NSString::from_str("word ");
                        unsafe {
                            NSTextInputClient::insertText_replacementRange(
                                &*view,
                                &s,
                                NSRange::new(NSNotFound as usize, 0),
                            )
                        };
                    } else if i < 180 {
                        for _ in 0..5 {
                            unsafe { NSTextInputClient::doCommandBySelector(&*view, objc2::sel!(deleteBackward:)) };
                        }
                    }
                });
            }
            _ => {
                let m: &NSMutableAttributedString = &storage;
                m.setAttributedString(&styled_text());
                tv.setSelectedRange(NSRange::new(22, 16));
                // A find highlight: a temporary attribute, kept by the layout
                // manager apart from the text.
                let text = storage.string().to_string();
                if let (Some(lm), Some(at)) = (unsafe { tv.layoutManager() }, text.find("Long paragraphs")) {
                    let green = NSColor::colorWithSRGBRed_green_blue_alpha(0.75, 0.93, 0.75, 1.0);
                    let at = text[..at].encode_utf16().count();
                    unsafe {
                        lm.addTemporaryAttribute_value_forCharacterRange(
                            NSBackgroundColorAttributeName,
                            &green,
                            NSRange::new(at, 15),
                        )
                    };
                }
            }
        }
        window.setContentView(Some(&scroll));
        window.makeFirstResponder(Some(&tv));
        if let Some(secs) = std::env::var("TEXTEDIT_QUIT_AFTER").ok().and_then(|s| s.parse::<f64>().ok()) {
            self.every(secs, false, || {
                NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None)
            });
        }
        window.makeKeyAndOrderFront(None);
        let _ = self.ivars().window.set(window);
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
