//! Containers: split views, stack views, tab views, a table, a layout made
//! with anchors and scroll views, in one window. Written only against
//! objc2-app-kit: on macOS it runs on AppKit, on Linux on Sidestep.
//!
//! SCENARIO: split (three panes, the middle one split again), stack (rows
//! in each distribution), tabs (a tab view on its second page), table
//! (1000 rows in a scroll view, one selected), autolayout (a header,
//! sidebar, content and footer placed by constraints; the default), or
//! the scroll views:
//!
//! - overlay: a list with overlay scrollers, shown, and a badge floating
//!   over it;
//! - transparent: a scroll view drawing no background over a striped
//!   backdrop;
//! - nested: shelves of cards, each shelf a horizontal scroll view in a
//!   vertical one;
//! - hscroll: a wide timeline, scrolled sideways;
//! - fling: a long list flung up and down, as a touchpad would;
//! - stream: a log that grows by a line ten times a second, pinned to its
//!   end;
//! - idle: a list with a caret blinking in it.
//!
//! CONTAINERS_SCROLL: in the table and hscroll scenarios, scroll at 600
//! points a second; in the nested one, move the shelves up and down at
//! 300. CONTAINERS_QUIT_AFTER: seconds until the app
//! terminates itself. CONTAINERS_PNG=path: show nothing; lay the scenario
//! out in a window that is never shown, draw it into a bitmap with
//! `cacheDisplayInRect:toBitmapImageRep:` (at CONTAINERS_SCALE pixels a
//! point) and write it as a PNG, on macOS too: reference pictures taken
//! without the screen. CONTAINERS_BENCH=1: show nothing and time
//! scrolling a scroll view.

use std::cell::{OnceCell, RefCell};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSBezierPath, NSBitmapFormat, NSBitmapImageRep, NSColor, NSControlTextEditingDelegate, NSDeviceRGBColorSpace,
    NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSLayoutAttribute, NSLayoutConstraint, NSResponder,
    NSScrollView, NSSplitView, NSSplitViewDividerStyle, NSStackView, NSStackViewDistribution, NSStringDrawing,
    NSTabView, NSTabViewItem, NSTableCellView, NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate,
    NSTableViewStyle, NSUserInterfaceItemIdentification, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSIndexSet, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    NSTimer, ns_string,
};

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

mod scrolling;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn color(r: f64, g: f64, b: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
}

/// Label text: 13-point system font, near black.
fn attributes() -> Retained<NSDictionary<NSString, AnyObject>> {
    thread_local!(static ATTRS: OnceCell<Retained<NSDictionary<NSString, AnyObject>>> = const { OnceCell::new() });
    ATTRS.with(|a| {
        a.get_or_init(|| {
            let font = NSFont::systemFontOfSize(13.0);
            let fg = color(0.1, 0.1, 0.12);
            // SAFETY: the constants are defined by AppKit.
            let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
            let values: [&AnyObject; 2] = [&font, &fg];
            NSDictionary::from_slices(&keys, &values)
        })
        .clone()
    })
}

struct SwatchIvars {
    fill: Retained<NSColor>,
    label: Retained<NSString>,
    size: NSSize,
}

define_class!(
    /// A colored box with a label, and an intrinsic size when given one.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ContainersSwatch"]
    #[ivars = SwatchIvars]
    struct Swatch;

    impl Swatch {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            self.ivars().size
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            self.ivars().fill.setFill();
            NSBezierPath::fillRect(bounds);
            color(0.0, 0.0, 0.0).setFill();
            let edge = |r| NSBezierPath::fillRect(r);
            edge(rect(0.0, 0.0, bounds.size.width, 1.0));
            edge(rect(0.0, bounds.size.height - 1.0, bounds.size.width, 1.0));
            edge(rect(0.0, 0.0, 1.0, bounds.size.height));
            edge(rect(bounds.size.width - 1.0, 0.0, 1.0, bounds.size.height));
            // SAFETY: the attributes hold a font and a color.
            unsafe { self.ivars().label.drawAtPoint_withAttributes(NSPoint::new(6.0, 4.0), Some(&attributes())) };
        }
    }
);

fn swatch(mtm: MainThreadMarker, label: &str, fill: Retained<NSColor>, size: NSSize) -> Retained<NSView> {
    let this = Swatch::alloc(mtm).set_ivars(SwatchIvars { fill, label: NSString::from_str(label), size });
    let view: Retained<Swatch> =
        // SAFETY: the superclass's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, size.width.max(0.0), size.height.max(0.0))] };
    Retained::into_super(view)
}

/// A swatch without an intrinsic size.
fn pane(mtm: MainThreadMarker, label: &str, fill: Retained<NSColor>) -> Retained<NSView> {
    swatch(mtm, label, fill, NSSize::new(-1.0, -1.0))
}

fn sizable() -> NSAutoresizingMaskOptions {
    NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable
}

// The table's cells and data.

define_class!(
    /// A cell that draws its object value.
    #[unsafe(super(NSTableCellView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ContainersCell"]
    struct TextCell;

    impl TextCell {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            // SAFETY: a cell view's objectValue returns an object or nil.
            let value: Option<Retained<AnyObject>> = unsafe { msg_send![self, objectValue] };
            if let Some(text) = value.and_then(|v| v.downcast::<NSString>().ok()) {
                // SAFETY: the attributes hold a font and a color.
                unsafe { text.drawAtPoint_withAttributes(NSPoint::new(2.0, 3.0), Some(&attributes())) };
            }
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ContainersTableSource"]
    struct Source;

    unsafe impl NSObjectProtocol for Source {}

    unsafe impl NSTableViewDataSource for Source {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn rows(&self, _table: &NSTableView) -> isize {
            1000
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn value(
            &self,
            _table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: isize,
        ) -> Option<Retained<AnyObject>> {
            let id = column.map(|c| c.identifier().to_string()).unwrap_or_default();
            let text = match id.as_str() {
                "name" => format!("Item {row}"),
                "size" => format!("{} KB", (row * 37) % 1000),
                _ => format!("{} days ago", row % 30),
            };
            Some(Retained::into_super(Retained::into_super(NSString::from_str(&text))))
        }
    }

    unsafe impl NSControlTextEditingDelegate for Source {}

    unsafe impl NSTableViewDelegate for Source {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view(&self, table: &NSTableView, column: Option<&NSTableColumn>, _row: isize) -> Option<Retained<NSView>> {
            let id = column.map(|c| c.identifier()).unwrap_or_else(|| NSString::from_str("cell"));
            // SAFETY: there is no owner to connect outlets to.
            let reused = unsafe { table.makeViewWithIdentifier_owner(&id, None) };
            Some(reused.unwrap_or_else(|| {
                // SAFETY: the class's initializer.
                let cell: Retained<TextCell> =
                    unsafe { msg_send![TextCell::alloc(MainThreadMarker::from(table)), initWithFrame: NSRect::ZERO] };
                let view: Retained<NSView> = Retained::into_super(Retained::into_super(cell));
                view.setIdentifier(Some(&id));
                view
            }))
        }
    }
);

// The scenarios.

fn split(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
    let outer = NSSplitView::initWithFrame(NSSplitView::alloc(mtm), frame);
    outer.setVertical(true);
    outer.setDividerStyle(NSSplitViewDividerStyle::Thin);
    let inner = NSSplitView::initWithFrame(NSSplitView::alloc(mtm), rect(0.0, 0.0, 400.0, frame.size.height));
    inner.setDividerStyle(NSSplitViewDividerStyle::Thick);
    inner.addSubview(&pane(mtm, "Top", color(0.80, 0.90, 1.00)));
    inner.addSubview(&pane(mtm, "Bottom", color(0.85, 0.95, 0.85)));
    inner.adjustSubviews();
    outer.addSubview(&pane(mtm, "Sidebar", color(0.93, 0.90, 0.98)));
    outer.addSubview(&inner);
    outer.addSubview(&pane(mtm, "Inspector", color(1.00, 0.93, 0.85)));
    outer.adjustSubviews();
    outer.setPosition_ofDividerAtIndex(200.0, 0);
    outer.setPosition_ofDividerAtIndex(frame.size.width - 220.0, 1);
    inner.setPosition_ofDividerAtIndex(frame.size.height * 0.6, 0);
    Retained::into_super(outer)
}

fn stack(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
    let column = NSStackView::new(mtm);
    column.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    column.setAlignment(NSLayoutAttribute::Leading);
    column.setSpacing(12.0);
    column.setFrame(frame);
    column.setAutoresizingMask(sizable());
    let distributions = [
        ("gravity", NSStackViewDistribution::GravityAreas),
        ("fill", NSStackViewDistribution::Fill),
        ("equally", NSStackViewDistribution::FillEqually),
        ("proportional", NSStackViewDistribution::FillProportionally),
        ("spacing", NSStackViewDistribution::EqualSpacing),
        ("centering", NSStackViewDistribution::EqualCentering),
    ];
    for (i, (name, distribution)) in distributions.into_iter().enumerate() {
        let row = NSStackView::new(mtm);
        row.setDistribution(distribution);
        let tint = 0.75 + 0.04 * i as f64;
        for (j, width) in [70.0, 120.0, 90.0].into_iter().enumerate() {
            let label = if j == 0 { name.to_string() } else { format!("{width}") };
            row.addArrangedSubview(&swatch(
                mtm,
                &label,
                color(tint, 0.85, 1.0 - 0.1 * j as f64),
                NSSize::new(width, 40.0),
            ));
        }
        column.addArrangedSubview(&row);
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[row
            .widthAnchor()
            .constraintEqualToAnchor_constant(&column.widthAnchor(), -40.0)]));
    }
    column.setEdgeInsets(objc2_foundation::NSEdgeInsets { top: 20.0, left: 20.0, bottom: 20.0, right: 20.0 });
    Retained::into_super(column)
}

fn tabs(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
    let tab = NSTabView::initWithFrame(NSTabView::alloc(mtm), frame);
    tab.setAutoresizingMask(sizable());
    for (name, fill) in
        [("General", color(0.9, 0.9, 1.0)), ("Layout", color(0.9, 1.0, 0.9)), ("Advanced", color(1.0, 0.95, 0.9))]
    {
        let item = NSTabViewItem::new();
        item.setLabel(&NSString::from_str(name));
        let page = pane(mtm, &format!("The {name} page"), fill);
        page.setAutoresizingMask(sizable());
        item.setView(Some(&page));
        tab.addTabViewItem(&item);
    }
    tab.selectTabViewItemAtIndex(1);
    Retained::into_super(tab)
}

fn table(mtm: MainThreadMarker, frame: NSRect, source: &Source) -> Retained<NSView> {
    let t = NSTableView::initWithFrame(NSTableView::alloc(mtm), frame);
    t.setStyle(NSTableViewStyle::Plain);
    t.setHeaderView(None);
    t.setUsesAlternatingRowBackgroundColors(true);
    for (id, width) in [("name", 260.0), ("size", 160.0), ("date", 200.0)] {
        let c = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &NSString::from_str(id));
        c.setWidth(width);
        t.addTableColumn(&c);
    }
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(Some(ProtocolObject::from_ref(source))) };
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDelegate(Some(ProtocolObject::from_ref(source))) };
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    scroll.setAutoresizingMask(sizable());
    scroll.setHasVerticalScroller(true);
    scroll.setDocumentView(Some(&t));
    t.reloadData();
    t.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(5), false);
    Retained::into_super(scroll)
}

fn autolayout(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
    let root = pane(mtm, "", color(0.97, 0.97, 0.97));
    root.setFrame(frame);
    root.setAutoresizingMask(sizable());
    let header = pane(mtm, "Header: pinned to the top, 48 points tall", color(0.30, 0.45, 0.75));
    let sidebar = pane(mtm, "Sidebar: 180 wide", color(0.90, 0.88, 0.96));
    let content = pane(mtm, "Content: fills the rest", color(1.0, 1.0, 1.0));
    let footer = swatch(mtm, "Footer: centered, its own size", color(0.95, 0.85, 0.70), NSSize::new(260.0, 28.0));
    for v in [&header, &sidebar, &content, &footer] {
        v.setTranslatesAutoresizingMaskIntoConstraints(false);
        root.addSubview(v);
    }
    let pad = 12.0;
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        header.topAnchor().constraintEqualToAnchor(&root.topAnchor()),
        header.leadingAnchor().constraintEqualToAnchor(&root.leadingAnchor()),
        header.trailingAnchor().constraintEqualToAnchor(&root.trailingAnchor()),
        header.heightAnchor().constraintEqualToConstant(48.0),
        sidebar.topAnchor().constraintEqualToAnchor_constant(&header.bottomAnchor(), pad),
        sidebar.leadingAnchor().constraintEqualToAnchor_constant(&root.leadingAnchor(), pad),
        sidebar.widthAnchor().constraintEqualToConstant(180.0),
        sidebar.bottomAnchor().constraintEqualToAnchor_constant(&footer.topAnchor(), -pad),
        content.topAnchor().constraintEqualToAnchor(&sidebar.topAnchor()),
        content.leadingAnchor().constraintEqualToAnchor_constant(&sidebar.trailingAnchor(), pad),
        content.trailingAnchor().constraintEqualToAnchor_constant(&root.trailingAnchor(), -pad),
        content.bottomAnchor().constraintEqualToAnchor(&sidebar.bottomAnchor()),
        footer.centerXAnchor().constraintEqualToAnchor(&root.centerXAnchor()),
        footer.bottomAnchor().constraintEqualToAnchor_constant(&root.bottomAnchor(), -pad),
    ]));
    root
}

#[derive(Default)]
struct DelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    source: OnceCell<Retained<Source>>,
    timers: RefCell<Vec<Retained<NSTimer>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ContainersDelegate"]
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
        // SAFETY: the superclass's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    fn open_window(&self) {
        let mtm = self.mtm();
        let scenario = std::env::var("SCENARIO").unwrap_or_else(|_| "autolayout".into());
        let frame = rect(0.0, 0.0, 900.0, 600.0);
        let png = std::env::var("CONTAINERS_PNG").ok();
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        // SAFETY: a plain window, never shown.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: Rust owns the window, so closing it mustn't release it.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Sidestep containers"));
        let content = match scenario.as_str() {
            "split" => split(mtm, frame),
            "stack" => stack(mtm, frame),
            "tabs" => tabs(mtm, frame),
            "table" => {
                // SAFETY: the class's initializer.
                let source: Retained<Source> = unsafe { msg_send![Source::alloc(mtm), init] };
                let view = table(mtm, frame, &source);
                let _ = self.ivars().source.set(source);
                if std::env::var_os("CONTAINERS_SCROLL").is_some() {
                    // SAFETY: the table scenario's view is its scroll view.
                    let scroll: Retained<NSScrollView> = unsafe { Retained::cast_unchecked(view.clone()) };
                    let start = std::time::Instant::now();
                    let block = RcBlock::new(move |_: NonNull<NSTimer>| {
                        let clip = scroll.contentView();
                        let y = (start.elapsed().as_secs_f64() * 600.0) % 20000.0;
                        clip.scrollToPoint(NSPoint::new(0.0, y));
                        scroll.reflectScrolledClipView(&clip);
                    });
                    let timer =
                        // SAFETY: the block takes the timer and returns nothing.
                        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / 60.0, true, &block) };
                    self.ivars().timers.borrow_mut().push(timer);
                }
                view
            }
            "overlay" => scrolling::overlay(mtm, frame, &mut self.ivars().timers.borrow_mut()),
            "transparent" => scrolling::transparent(mtm, frame),
            "nested" => scrolling::nested(mtm, frame, &mut self.ivars().timers.borrow_mut()),
            "hscroll" => scrolling::hscroll(mtm, frame, &mut self.ivars().timers.borrow_mut()),
            "fling" => scrolling::fling(mtm, frame, &mut self.ivars().timers.borrow_mut()),
            "stream" => scrolling::stream(mtm, frame, &mut self.ivars().timers.borrow_mut()),
            "idle" => scrolling::idle(mtm, frame, &mut self.ivars().timers.borrow_mut()),
            _ => autolayout(mtm, frame),
        };
        window.setContentView(Some(&content));
        if scenario == "table" {
            // The table has the focus, so its selection shows strongly.
            // SAFETY: the table scenario's view is its scroll view.
            let scroll: &NSScrollView = unsafe { &*Retained::as_ptr(&content).cast() };
            let table = scroll.documentView();
            window.makeFirstResponder(table.as_deref().map(|t| &**t));
        }

        if let Some(secs) = std::env::var("CONTAINERS_QUIT_AFTER").ok().and_then(|s| s.parse::<f64>().ok()) {
            let block = RcBlock::new(move |_: NonNull<NSTimer>| {
                NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None);
            });
            // SAFETY: the block takes the timer and returns nothing.
            let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, false, &block) };
            self.ivars().timers.borrow_mut().push(timer);
        }
        if let Some(path) = png {
            // Laid out in the window, which is never shown.
            write_png(&content, &path);
            let _ = self.ivars().window.set(window);
            return;
        }
        window.makeKeyAndOrderFront(None);
        let _ = self.ivars().window.set(window);
    }
}

/// Draw `view`, laid out, into a bitmap and write it to `path` as a PNG.
fn write_png(view: &NSView, path: &str) {
    view.layoutSubtreeIfNeeded();
    let scale: f64 = std::env::var("CONTAINERS_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let b = view.bounds();
    let (w, h) = ((b.size.width * scale) as usize, (b.size.height * scale) as usize);
    // SAFETY: NULL planes make the rep allocate its own; the color space
    // name is AppKit's constant.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            <NSBitmapImageRep as objc2::AnyThread>::alloc(),
            std::ptr::null_mut(),
            w as isize,
            h as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::empty(),
            0,
            32,
        )
    }
    .expect("a bitmap");
    rep.setSize(b.size);
    view.cacheDisplayInRect_toBitmapImageRep(b, &rep);
    // The premultiplied pixels, unpremultiplied for PNG.
    let (data, row) = (rep.bitmapData(), rep.bytesPerRow() as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            // SAFETY: inside the bitmap's rows.
            let [r, g, b, a] = unsafe { std::ptr::read(data.add(y * row + x * 4).cast::<[u8; 4]>()) };
            let un = |c: u8| {
                if a == 0 { 0 } else { ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8 }
            };
            rgba.extend_from_slice(&[un(r), un(g), un(b), a]);
        }
    }
    let image = image::RgbaImage::from_raw(w as u32, h as u32, rgba).expect("pixels");
    image.save(path).expect("the PNG written");
    println!("wrote {path} ({w} x {h})");
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    if std::env::var_os("CONTAINERS_BENCH").is_some() {
        app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        // SAFETY: a plain window, never shown.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 600.0, 400.0),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                true,
            )
        };
        // SAFETY: Rust owns the window, so closing it mustn't release it.
        unsafe { window.setReleasedWhenClosed(false) };
        scrolling::bench(mtm, &window);
        return;
    }
    if std::env::var_os("CONTAINERS_PNG").is_some() {
        // No window shown and the application never runs or activates.
        app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        Delegate::new(mtm).open_window();
        return;
    }
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
