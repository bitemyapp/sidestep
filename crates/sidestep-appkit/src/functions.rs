//! AppKit's C functions that no class here needs: the application's entry
//! point, services (Linux has no Services menu), window depths, typed file
//! pasteboard types, the window list, tiled-rect and multipart-image
//! drawing, and the no-ops a compositor makes of screen updates and
//! animation effects. Values are as measured on macOS.

use std::ffi::{c_char, c_int, c_void};
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{ClassType, MainThreadMarker, msg_send};
use objc2_app_kit::{NSApplication, NSCompositingOperation, NSImage, NSRectFill, NSWindowDepth};
use objc2_foundation::{NSArray, NSInteger, NSIntersectionRect, NSPoint, NSRect, NSRectEdge, NSSize, NSString};

// The application.

/// Makes the application (the `NSPrincipalClass` of the main bundle's
/// `Info.plist`, else NSApplication), runs it, and exits when it stops.
///
/// # Safety
///
/// The arguments are the program's.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSApplicationMain(_argc: c_int, _argv: *mut *mut c_char) -> c_int {
    MainThreadMarker::new().expect("sidestep: NSApplicationMain runs on the main thread");
    let class = principal_class().unwrap_or_else(|| {
        crate::load_shell::<NSApplication>();
        NSApplication::class()
    });
    // SAFETY: +sharedApplication makes the application, of that class;
    // -run takes nothing.
    unsafe {
        let app: Retained<AnyObject> = msg_send![class, sharedApplication];
        let _: () = msg_send![&*app, run];
    }
    std::process::exit(0)
}

fn principal_class() -> Option<&'static AnyClass> {
    // SAFETY: +mainBundle returns a bundle; the key's value is a string or
    // anything else a plist holds.
    let name: Option<Retained<AnyObject>> = unsafe {
        let bundle: Retained<AnyObject> = msg_send![objc2_foundation::NSBundle::class(), mainBundle];
        msg_send![&*bundle, objectForInfoDictionaryKey: &*NSString::from_str("NSPrincipalClass")]
    };
    let name = name?.downcast::<NSString>().ok()?;
    AnyClass::get(&std::ffi::CString::new(name.to_string()).ok()?)
}

/// AppKit is always loaded: YES, with the application made.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSApplicationLoad() -> Bool {
    if let Some(mtm) = MainThreadMarker::new() {
        let _ = NSApplication::sharedApplication(mtm);
    }
    Bool::YES
}

// Services: Linux has no Services menu. An item counts as shown, as on
// macOS for items it doesn't know; none can be performed.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSShowsServicesMenuItem(_item: &NSString) -> Bool {
    Bool::YES
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSSetShowsServicesMenuItem(_item: &NSString, _enabled: Bool) -> NSInteger {
    0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSUpdateDynamicServices() {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSPerformService(_item: &NSString, _pboard: Option<&AnyObject>) -> Bool {
    Bool::NO
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSRegisterServicesProvider(_provider: Option<&AnyObject>, _name: &NSString) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSUnregisterServicesProvider(_name: &NSString) {}

// Window depths: a depth is 0x100 × a color space number (1 white, 2
// RGB) plus the bits per sample.

/// The depths windows come in, zero-terminated.
static DEPTHS: [NSWindowDepth; 6] = [
    NSWindowDepth(0x108),
    NSWindowDepth(0x204),
    NSWindowDepth(0x208),
    NSWindowDepth(0x210),
    NSWindowDepth(0x220),
    NSWindowDepth(0),
];

/// How many components a color space's colors have, by its name.
fn components(space: &str) -> NSInteger {
    match space {
        "NSDeviceRGBColorSpace" | "NSCalibratedRGBColorSpace" => 3,
        "NSDeviceWhiteColorSpace"
        | "NSCalibratedWhiteColorSpace"
        | "NSDeviceBlackColorSpace"
        | "NSCalibratedBlackColorSpace" => 1,
        "NSDeviceCMYKColorSpace" => 4,
        _ => 0,
    }
}

/// The deepest window depth close to what's asked: gray spaces get 8-bit
/// gray, the rest RGB of the sample size; exact unless the space is CMYK,
/// which windows don't have.
///
/// # Safety
///
/// `exact` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSBestDepth(
    space: &NSString,
    bps: NSInteger,
    _bpp: NSInteger,
    _planar: Bool,
    exact: *mut Bool,
) -> NSWindowDepth {
    let name = space.to_string();
    let depth = match (components(&name), bps) {
        (1, _) => 0x108,
        (_, ..=8) => 0x208,
        (_, ..=16) => 0x210,
        _ => 0x220,
    };
    if !exact.is_null() {
        // SAFETY: the caller's storage.
        unsafe { exact.write(Bool::new(name != "NSDeviceCMYKColorSpace")) };
    }
    NSWindowDepth(depth)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSPlanarFromDepth(depth: NSWindowDepth) -> Bool {
    // As macOS answers: its 8-bit gray depth, alone.
    Bool::new(depth.0 == 0x108)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSColorSpaceFromDepth(depth: NSWindowDepth) -> *mut NSString {
    let name = match depth.0 >> 8 {
        0 => "NSCalibratedBlackColorSpace",
        1 => "NSCalibratedWhiteColorSpace",
        2 => "NSCalibratedRGBColorSpace",
        _ => return std::ptr::null_mut(),
    };
    Retained::autorelease_return(NSString::from_str(name))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSBitsPerSampleFromDepth(depth: NSWindowDepth) -> NSInteger {
    (depth.0 & 0xff) as NSInteger
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSBitsPerPixelFromDepth(depth: NSWindowDepth) -> NSInteger {
    let bps = (depth.0 & 0xff) as NSInteger;
    match depth.0 >> 8 {
        1 if bps == 8 => 8,
        2 if bps <= 8 => 3 * bps,
        2 => 4 * bps,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSNumberOfColorComponents(space: &NSString) -> NSInteger {
    components(&space.to_string())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSAvailableWindowDepths() -> NonNull<NSWindowDepth> {
    NonNull::from(&DEPTHS[0])
}

// Typed file pasteboard types. Despite their names, the two `Create`
// functions return autoreleased strings the caller doesn't own, as
// measured on macOS (objc2-app-kit 0.3.2's bindings take ownership, which
// over-releases on macOS too).

const FILENAMES: &str = "NSTypedFilenamesPboardType:";
const FILE_CONTENTS: &str = "NXTypedFileContentsPboardType:";

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSCreateFilenamePboardType(file_type: &NSString) -> *mut NSString {
    Retained::autorelease_return(NSString::from_str(&format!("{FILENAMES}{file_type}")))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSCreateFileContentsPboardType(file_type: &NSString) -> *mut NSString {
    Retained::autorelease_return(NSString::from_str(&format!("{FILE_CONTENTS}{file_type}")))
}

fn file_type(pboard_type: &str) -> Option<&str> {
    pboard_type.strip_prefix(FILENAMES).or_else(|| pboard_type.strip_prefix(FILE_CONTENTS))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSGetFileType(pboard_type: &NSString) -> *mut NSString {
    match file_type(&pboard_type.to_string()) {
        Some(t) => Retained::autorelease_return(NSString::from_str(t)),
        None => std::ptr::null_mut(),
    }
}

/// The file types of the typed ones among `types`, sorted; nil if none.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSGetFileTypes(types: &NSArray<NSString>) -> *mut NSArray<NSString> {
    let mut found: Vec<String> = types.iter().filter_map(|t| file_type(&t.to_string()).map(String::from)).collect();
    if found.is_empty() {
        return std::ptr::null_mut();
    }
    found.sort();
    found.dedup();
    let strings: Vec<Retained<NSString>> = found.iter().map(|t| NSString::from_str(t)).collect();
    Retained::autorelease_return(NSArray::from_retained_slice(&strings))
}

// The window list: the program's windows on screen, front first.

fn window_numbers() -> Vec<NSInteger> {
    crate::app::on_screen().iter().rev().map(|w| w.windowNumber()).collect()
}

/// # Safety
///
/// `count` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSCountWindows(count: NonNull<NSInteger>) {
    // SAFETY: the caller's storage.
    unsafe { count.write(window_numbers().len() as NSInteger) };
}

/// # Safety
///
/// `list` has room for `size` numbers.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSWindowList(size: NSInteger, list: NonNull<NSInteger>) {
    for (i, n) in window_numbers().into_iter().take(size.max(0) as usize).enumerate() {
        // SAFETY: inside the caller's `size` slots.
        unsafe { list.add(i).write(n) };
    }
}

/// # Safety
///
/// `count` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSCountWindowsForContext(_context: NSInteger, count: NonNull<NSInteger>) {
    // SAFETY: forwarded contract.
    unsafe { NSCountWindows(count) }
}

/// # Safety
///
/// `list` has room for `size` numbers.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSWindowListForContext(_context: NSInteger, size: NSInteger, list: NonNull<NSInteger>) {
    // SAFETY: forwarded contract.
    unsafe { NSWindowList(size, list) }
}

// What a compositor makes of the rest.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSDisableScreenUpdates() {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSEnableScreenUpdates() {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSSetFocusRingStyle(_placement: NSInteger) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSCopyBits(_gstate: NSInteger, _rect: NSRect, _to: NSPoint) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSReleaseAlertPanel(_panel: Option<&AnyObject>) {}

/// Window server memory isn't Sidestep's to know: nothing is reported.
///
/// # Safety
///
/// The pointers are writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSGetWindowServerMemory(
    _context: NSInteger,
    virtual_memory: NonNull<NSInteger>,
    backing: NonNull<NSInteger>,
    dump: NonNull<*mut NSString>,
) -> NSInteger {
    // SAFETY: the caller's storage.
    unsafe {
        virtual_memory.write(0);
        backing.write(0);
        dump.write(Retained::autorelease_return(NSString::new()));
    }
    0
}

/// No effect is shown (the compositor animates windows, not points); the
/// delegate hears at once that it ended.
///
/// # Safety
///
/// `selector` is a method of `delegate` taking the context pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSShowAnimationEffect(
    _effect: NSInteger,
    _center: NSPoint,
    _size: NSSize,
    delegate: Option<&AnyObject>,
    selector: Option<Sel>,
    context: *mut c_void,
) {
    if let (Some(delegate), Some(selector)) = (delegate, selector) {
        // SAFETY: per this function's contract.
        unsafe { objc2::runtime::MessageReceiver::send_message::<_, ()>(delegate, selector, (context,)) };
    }
}

// Drawing.

/// Fill a one-point strip off each of `sides` in turn, with the matching
/// color, clipped to `clip`; what's left of `bounds`.
fn tiled(
    bounds: NSRect,
    clip: NSRect,
    count: usize,
    side: impl Fn(usize) -> NSRectEdge,
    fill: impl Fn(usize),
) -> NSRect {
    let mut rest = bounds;
    for i in 0..count {
        let mut slice = NSRect::ZERO;
        let mut remainder = NSRect::ZERO;
        // SAFETY: the out pointers are locals.
        unsafe {
            objc2_foundation::NSDivideRect(rest, NonNull::from(&mut slice), NonNull::from(&mut remainder), 1.0, side(i))
        };
        rest = remainder;
        let visible = NSIntersectionRect(slice, clip);
        if visible.size.width > 0.0 && visible.size.height > 0.0 {
            fill(i);
            NSRectFill(visible);
        }
    }
    rest
}

/// # Safety
///
/// `sides` and `grays` hold `count` values each.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSDrawTiledRects(
    bounds: NSRect,
    clip: NSRect,
    sides: NonNull<NSRectEdge>,
    grays: NonNull<f64>,
    count: NSInteger,
) -> NSRect {
    let count = count.max(0) as usize;
    tiled(
        bounds,
        clip,
        count,
        // SAFETY: inside the caller's arrays.
        |i| unsafe { *sides.add(i).as_ptr() },
        |i| objc2_app_kit::NSColor::colorWithWhite_alpha(unsafe { *grays.add(i).as_ptr() }, 1.0).set(),
    )
}

/// # Safety
///
/// `sides` and `colors` hold `count` values each.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSDrawColorTiledRects(
    bounds: NSRect,
    clip: NSRect,
    sides: NonNull<NSRectEdge>,
    colors: NonNull<NonNull<objc2_app_kit::NSColor>>,
    count: NSInteger,
) -> NSRect {
    let count = count.max(0) as usize;
    tiled(
        bounds,
        clip,
        count,
        // SAFETY: inside the caller's arrays.
        |i| unsafe { *sides.add(i).as_ptr() },
        |i| unsafe { (*colors.add(i).as_ptr()).as_ref() }.set(),
    )
}

fn draw(image: Option<&NSImage>, rect: NSRect, op: NSCompositingOperation, alpha: f64, flipped: bool) {
    let Some(image) = image else { return };
    if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
        return;
    }
    // SAFETY: no hints; the rest are plain values.
    unsafe {
        image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(rect, NSRect::ZERO, op, alpha, flipped, None)
    };
}

/// Caps at their own length at each end of `frame`, the center stretched
/// between them.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C-unwind" fn NSDrawThreePartImage(
    frame: NSRect,
    start: Option<&NSImage>,
    center: Option<&NSImage>,
    end: Option<&NSImage>,
    vertical: Bool,
    op: NSCompositingOperation,
    alpha: f64,
    flipped: Bool,
) {
    let size = |i: Option<&NSImage>| i.map_or(NSSize::ZERO, |i| i.size());
    let (o, s) = (frame.origin, frame.size);
    let flipped = flipped.as_bool();
    if vertical.as_bool() {
        let (a, b) = (size(start).height, size(end).height);
        // The start is at the top: the frame's minimum y when flipped.
        let (first, last) = if flipped { (o.y, o.y + s.height - b) } else { (o.y + s.height - a, o.y) };
        draw(start, NSRect::new(NSPoint::new(o.x, first), NSSize::new(s.width, a)), op, alpha, flipped);
        draw(end, NSRect::new(NSPoint::new(o.x, last), NSSize::new(s.width, b)), op, alpha, flipped);
        let middle_y = if flipped { o.y + a } else { o.y + b };
        draw(
            center,
            NSRect::new(NSPoint::new(o.x, middle_y), NSSize::new(s.width, s.height - a - b)),
            op,
            alpha,
            flipped,
        );
    } else {
        let (a, b) = (size(start).width, size(end).width);
        draw(start, NSRect::new(o, NSSize::new(a, s.height)), op, alpha, flipped);
        draw(end, NSRect::new(NSPoint::new(o.x + s.width - b, o.y), NSSize::new(b, s.height)), op, alpha, flipped);
        draw(
            center,
            NSRect::new(NSPoint::new(o.x + a, o.y), NSSize::new(s.width - a - b, s.height)),
            op,
            alpha,
            flipped,
        );
    }
}

/// Corners at their own size, edges stretched along them, the center
/// stretched to fill.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C-unwind" fn NSDrawNinePartImage(
    frame: NSRect,
    top_left: Option<&NSImage>,
    top: Option<&NSImage>,
    top_right: Option<&NSImage>,
    left: Option<&NSImage>,
    center: Option<&NSImage>,
    right: Option<&NSImage>,
    bottom_left: Option<&NSImage>,
    bottom: Option<&NSImage>,
    bottom_right: Option<&NSImage>,
    op: NSCompositingOperation,
    alpha: f64,
    flipped: Bool,
) {
    let size = |i: Option<&NSImage>| i.map_or(NSSize::ZERO, |i| i.size());
    let flipped = flipped.as_bool();
    let (o, s) = (frame.origin, frame.size);
    let (tl, br) = (size(top_left), size(bottom_right));
    let (x0, x1, x2) = (o.x, o.x + tl.width, o.x + s.width - br.width);
    // Rows from the top: the frame's minimum y when flipped.
    let (top_h, bottom_h) = (tl.height, br.height);
    let (top_y, middle_y, bottom_y) = if flipped {
        (o.y, o.y + top_h, o.y + s.height - bottom_h)
    } else {
        (o.y + s.height - top_h, o.y + bottom_h, o.y)
    };
    let middle_h = s.height - top_h - bottom_h;
    let widths = [tl.width, x2 - x1, br.width];
    let xs = [x0, x1, x2];
    let rows = [
        (top_y, top_h, [top_left, top, top_right]),
        (middle_y, middle_h, [left, center, right]),
        (bottom_y, bottom_h, [bottom_left, bottom, bottom_right]),
    ];
    for (y, h, images) in rows {
        for (i, image) in images.into_iter().enumerate() {
            draw(image, NSRect::new(NSPoint::new(xs[i], y), NSSize::new(widths[i], h)), op, alpha, flipped);
        }
    }
}

/// `NSNativeShortGlyphPacking` (the one packing left): each glyph as a
/// 16-bit number in the machine's order, then two zero bytes, as macOS
/// writes it; the length is the bytes written. Other packings write
/// nothing.
///
/// # Safety
///
/// `glyphs` holds `count` glyphs and `packed` has room for twice as many
/// bytes and two more.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSConvertGlyphsToPackedGlyphs(
    glyphs: NonNull<u32>,
    count: NSInteger,
    packing: NSInteger,
    packed: NonNull<c_char>,
) -> NSInteger {
    const NATIVE_SHORT: NSInteger = 5;
    if packing != NATIVE_SHORT {
        return 0;
    }
    let count = count.max(0) as usize;
    let out = packed.as_ptr().cast::<u8>();
    for i in 0..count {
        // SAFETY: inside the caller's buffers.
        unsafe {
            let bytes = (*glyphs.add(i).as_ptr() as u16).to_ne_bytes();
            out.add(2 * i).copy_from_nonoverlapping(bytes.as_ptr(), 2);
        }
    }
    // SAFETY: as above.
    unsafe { out.add(2 * count).write_bytes(0, 2) };
    (2 * count + 2) as NSInteger
}
