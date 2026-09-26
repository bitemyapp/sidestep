//! `NSImageRep` and `NSCustomImageRep`, and drawing representations.
//!
//! A representation draws by recording an `Op::Image` naming a snapshot of
//! its pixels (`bitmap::image_data`), placed by the current transform.
//! `drawInRect:` and the `respectFlipped:` form turn the image the right
//! way up in a flipped context; the older
//! `drawInRect:fromRect:operation:fraction:` doesn't, as in AppKit.
//!
//! A custom rep (a drawing handler, `imageWithSize:flipped:drawingHandler:`)
//! is drawn by running its handler into a bitmap context as many pixels as
//! the destination covers, and drawing that bitmap. As in AppKit, what the
//! handler drew is kept: drawing it again the same size in device pixels
//! reuses it, and another size (another scale) or another appearance calls
//! the handler again.
//!
//! A bitmap drawn in a window is cached on the render thread until the
//! bitmap goes away. The keys of those that went away are sent once a turn
//! of the main loop, after the display pass ([`send_forgotten`]): sent at
//! once, a key could reach the render thread before the paint that draws
//! it (a bitmap made and dropped within `drawRect:`), and the paint would
//! cache it again for good.

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageRep, NSCompositingOperation, NSCustomImageRep, NSImageInterpolation, NSImageRep,
};
use objc2_foundation::{NSArray, NSCopying, NSDictionary, NSInteger, NSPoint, NSRect, NSSize, NSString, NSZone};

use crate::appearance;
use crate::protocol::{Blend, Color, Op, Quality, Rect};

sidestep_runtime::static_class!(pub NSIMAGEREP, NSIMAGEREP_META = "NSImageRep", || {
    let _ = NSImageRepImpl::class();
});

sidestep_runtime::static_class!(pub NSBITMAPIMAGEREP, NSBITMAPIMAGEREP_META = "NSBitmapImageRep", || {
    let _ = crate::bitmap::NSBitmapImageRepImpl::class();
});

sidestep_runtime::static_class!(pub NSCUSTOMIMAGEREP, NSCUSTOMIMAGEREP_META = "NSCustomImageRep", || {
    let _ = NSCustomImageRepImpl::class();
});

/// What every representation reports.
pub(crate) struct RepIvars {
    pub size: Cell<NSSize>,
    pub pixels: Cell<(NSInteger, NSInteger)>,
    pub bps: Cell<NSInteger>,
    pub alpha: Cell<bool>,
    pub opaque: Cell<bool>,
    pub space_name: RefCell<Retained<NSString>>,
}

impl Default for RepIvars {
    fn default() -> Self {
        RepIvars {
            size: Cell::new(NSSize::ZERO),
            pixels: Cell::new((0, 0)),
            bps: Cell::new(0),
            alpha: Cell::new(false),
            opaque: Cell::new(false),
            space_name: RefCell::new(NSString::new()),
        }
    }
}

impl RepIvars {
    /// Take on what `other` reports, for a copy.
    pub fn copy_from(&self, other: &RepIvars) {
        self.size.set(other.size.get());
        self.pixels.set(other.pixels.get());
        self.bps.set(other.bps.get());
        self.alpha.set(other.alpha.get());
        self.opaque.set(other.opaque.get());
        *self.space_name.borrow_mut() = other.space_name.borrow().clone();
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements.
    #[unsafe(super(NSObject))]
    #[name = "NSImageRep"]
    #[ivars = RepIvars]
    pub(crate) struct NSImageRepImpl;

    impl NSImageRepImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(RepIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(draw))]
        fn draw(&self) -> bool {
            let size = self.ivars().size.get();
            draw_rep(self.as_rep(), NSRect::ZERO, NSRect::new(NSPoint::ZERO, size), Blend::SourceOver, 1.0, false)
        }

        #[unsafe(method(drawAtPoint:))]
        fn draw_at_point(&self, p: NSPoint) -> bool {
            let size = self.ivars().size.get();
            draw_rep(self.as_rep(), NSRect::ZERO, NSRect::new(p, size), Blend::SourceOver, 1.0, false)
        }

        #[unsafe(method(drawInRect:))]
        fn draw_in_rect(&self, r: NSRect) -> bool {
            draw_rep(self.as_rep(), NSRect::ZERO, r, Blend::SourceOver, 1.0, false)
        }

        #[unsafe(method(drawInRect:fromRect:operation:fraction:respectFlipped:hints:))]
        fn draw_in_rect_full(
            &self,
            dst: NSRect,
            src: NSRect,
            op: NSCompositingOperation,
            fraction: f64,
            respect: bool,
            _hints: Option<&AnyObject>,
        ) -> bool {
            draw_rep(self.as_rep(), src, dst, Blend::from_raw(op.0), fraction, respect)
        }

        #[unsafe(method(size))]
        fn size(&self) -> NSSize {
            self.ivars().size.get()
        }

        #[unsafe(method(setSize:))]
        fn set_size(&self, size: NSSize) {
            self.ivars().size.set(size);
        }

        #[unsafe(method(hasAlpha))]
        fn has_alpha(&self) -> bool {
            self.ivars().alpha.get()
        }

        #[unsafe(method(setAlpha:))]
        fn set_alpha(&self, flag: bool) {
            self.ivars().alpha.set(flag);
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            self.ivars().opaque.get()
        }

        #[unsafe(method(setOpaque:))]
        fn set_opaque(&self, flag: bool) {
            self.ivars().opaque.set(flag);
        }

        #[unsafe(method_id(colorSpaceName))]
        fn color_space_name(&self) -> Retained<NSString> {
            self.ivars().space_name.borrow().clone()
        }

        #[unsafe(method(setColorSpaceName:))]
        fn set_color_space_name(&self, name: &NSString) {
            *self.ivars().space_name.borrow_mut() = NSString::from_str(&name.to_string());
        }

        #[unsafe(method(bitsPerSample))]
        fn bits_per_sample(&self) -> NSInteger {
            self.ivars().bps.get()
        }

        #[unsafe(method(setBitsPerSample:))]
        fn set_bits_per_sample(&self, v: NSInteger) {
            self.ivars().bps.set(v);
        }

        #[unsafe(method(pixelsWide))]
        fn pixels_wide(&self) -> NSInteger {
            self.ivars().pixels.get().0
        }

        #[unsafe(method(setPixelsWide:))]
        fn set_pixels_wide(&self, v: NSInteger) {
            let (_, h) = self.ivars().pixels.get();
            self.ivars().pixels.set((v, h));
        }

        #[unsafe(method(pixelsHigh))]
        fn pixels_high(&self) -> NSInteger {
            self.ivars().pixels.get().1
        }

        #[unsafe(method(setPixelsHigh:))]
        fn set_pixels_high(&self, v: NSInteger) {
            let (w, _) = self.ivars().pixels.get();
            self.ivars().pixels.set((w, v));
        }

        #[unsafe(method_id(imageRepWithContentsOfFile:))]
        fn image_rep_with_contents_of_file(path: &NSString) -> Option<Retained<NSImageRep>> {
            rep_from_file(&path.to_string())
        }

        #[unsafe(method_id(imageRepsWithContentsOfFile:))]
        fn image_reps_with_contents_of_file(path: &NSString) -> Option<Retained<NSArray<NSImageRep>>> {
            rep_from_file(&path.to_string()).map(|rep| NSArray::from_retained_slice(&[rep]))
        }

        #[unsafe(method_id(imageRepWithContentsOfURL:))]
        fn image_rep_with_contents_of_url(url: &AnyObject) -> Option<Retained<NSImageRep>> {
            url_path(url).and_then(|path| rep_from_file(&path.to_string()))
        }

        #[unsafe(method(canInitWithData:))]
        fn can_init_with_data(data: &AnyObject) -> bool {
            data_is_image(data)
        }

        #[unsafe(method(imageRepClassForData:))]
        fn image_rep_class_for_data(data: &AnyObject) -> Option<&'static AnyClass> {
            data_is_image(data).then(<NSBitmapImageRep as ClassType>::class)
        }

        // The abstract class reads nothing; `NSBitmapImageRep` lists what
        // it reads.
        #[unsafe(method_id(imageTypes))]
        fn image_types() -> Retained<NSArray<NSString>> {
            NSArray::new()
        }

        #[unsafe(method_id(imageUnfilteredTypes))]
        fn image_unfiltered_types() -> Retained<NSArray<NSString>> {
            NSArray::new()
        }

        #[unsafe(method(registerImageRepClass:))]
        fn register_image_rep_class(class: &AnyClass) {
            let mut classes = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
            let at = class as *const AnyClass as usize;
            if !classes.contains(&at) {
                classes.push(at);
            }
        }

        #[unsafe(method(unregisterImageRepClass:))]
        fn unregister_image_rep_class(class: &AnyClass) {
            let at = class as *const AnyClass as usize;
            REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).retain(|c| *c != at);
        }

        #[unsafe(method_id(registeredImageRepClasses))]
        fn registered_image_rep_classes() -> Retained<NSArray<AnyObject>> {
            let mut classes: Vec<&AnyClass> = vec![<NSBitmapImageRep as ClassType>::class(), <NSCustomImageRep as ClassType>::class()];
            for &c in REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                // SAFETY: registered classes live for the program.
                classes.push(unsafe { &*(c as *const AnyClass) });
            }
            let objects: Vec<&AnyObject> = classes.iter().map(|c| {
                // SAFETY: a class is an object.
                unsafe { &*(*c as *const AnyClass).cast::<AnyObject>() }
            }).collect();
            NSArray::from_slice(&objects)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSImageRep> {
            let copy = Self::alloc().set_ivars(RepIvars::default());
            // SAFETY: NSObject's designated initializer.
            let copy: Retained<Self> = unsafe { msg_send![super(copy), init] };
            copy.ivars().copy_from(self.ivars());
            // SAFETY: NSImageRepImpl is the class NSImageRep names.
            unsafe { Retained::cast_unchecked(copy) }
        }
    }

    unsafe impl NSObjectProtocol for NSImageRepImpl {}

    unsafe impl NSCopying for NSImageRepImpl {}
);

static REGISTERED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn rep_from_file(path: &str) -> Option<Retained<NSImageRep>> {
    crate::bitmap::from_file_bytes(read_file(path)?, true).map(Retained::into_super)
}

fn data_is_image(data: &AnyObject) -> bool {
    data_bytes(data).is_some_and(|b| crate::codec::header(&b).is_some())
}

/// What every representation reports, of any subclass's instance.
pub(crate) fn rep_ivars<T>(this: &T) -> &RepIvars {
    // SAFETY: only called with NSImageRep subclass instances, which start
    // with NSImageRep's layout and ivars.
    unsafe { &*(this as *const T).cast::<NSImageRepImpl>() }.ivars()
}

/// The uniform type identifiers of the files the codecs read.
pub(crate) fn type_list() -> Retained<NSArray<NSString>> {
    let types: Vec<Retained<NSString>> = [
        "public.png",
        "public.jpeg",
        "com.compuserve.gif",
        "org.webmproject.webp",
        "com.microsoft.bmp",
        "public.tiff",
        "com.microsoft.ico",
    ]
    .iter()
    .map(|t| NSString::from_str(t))
    .collect();
    NSArray::from_retained_slice(&types)
}

impl NSImageRepImpl {
    fn as_rep(&self) -> &NSImageRep {
        // SAFETY: NSImageRepImpl is the class NSImageRep names.
        unsafe { &*(self as *const Self).cast::<NSImageRep>() }
    }
}

pub(crate) fn rep_imp(rep: &NSImageRep) -> &NSImageRepImpl {
    // SAFETY: every NSImageRep is an NSImageRepImpl.
    unsafe { &*(rep as *const NSImageRep).cast::<NSImageRepImpl>() }
}

// NSCustomImageRep.

type Handler = RcBlock<dyn Fn(NSRect) -> Bool>;

pub(crate) struct CustomIvars {
    flipped: bool,
    handler: Option<Handler>,
    draw_selector: Option<Sel>,
    delegate: RefCell<Option<objc2::rc::Weak<AnyObject>>>,
    /// What the handler drew, per pixel size and appearance.
    cache: RefCell<Vec<(CacheKey, Retained<NSBitmapImageRep>)>>,
}

#[derive(Clone, Copy, PartialEq)]
struct CacheKey {
    pixels: (usize, usize),
    appearance: appearance::Id,
}

define_class!(
    // SAFETY: NSImageRep has no subclassing requirements.
    #[unsafe(super(NSImageRep, NSObject))]
    #[name = "NSCustomImageRep"]
    #[ivars = CustomIvars]
    pub(crate) struct NSCustomImageRepImpl;

    impl NSCustomImageRepImpl {
        #[unsafe(method_id(initWithSize:flipped:drawingHandler:))]
        fn init_with_handler(
            this: Allocated<Self>,
            size: NSSize,
            flipped: bool,
            handler: &DynBlock<dyn Fn(NSRect) -> Bool>,
        ) -> Retained<Self> {
            let this = this.set_ivars(CustomIvars {
                flipped,
                handler: Some(handler.copy()),
                draw_selector: None,
                delegate: RefCell::new(None),
                cache: RefCell::new(Vec::new()),
            });
            // SAFETY: NSImageRep's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            rep_ivars(&*this).size.set(size);
            rep_ivars(&*this).alpha.set(true);
            this
        }

        #[unsafe(method_id(initWithDrawSelector:delegate:))]
        fn init_with_selector(this: Allocated<Self>, selector: Sel, delegate: &AnyObject) -> Retained<Self> {
            let this = this.set_ivars(CustomIvars {
                flipped: false,
                handler: None,
                draw_selector: Some(selector),
                delegate: RefCell::new(Some(objc2::rc::Weak::new(delegate))),
                cache: RefCell::new(Vec::new()),
            });
            // SAFETY: NSImageRep's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(drawingHandler))]
        fn drawing_handler(&self) -> *mut DynBlock<dyn Fn(NSRect) -> Bool> {
            self.ivars().handler.as_ref().map_or(std::ptr::null_mut(), RcBlock::as_ptr)
        }

        #[unsafe(method(drawSelector))]
        fn draw_selector(&self) -> Option<Sel> {
            self.ivars().draw_selector
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().as_ref().and_then(|w| w.load())
        }

        #[unsafe(method(draw))]
        fn draw(&self) -> bool {
            let size = rep_ivars(self).size.get();
            draw_rep(self.as_rep(), NSRect::ZERO, NSRect::new(NSPoint::ZERO, size), Blend::SourceOver, 1.0, false)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSCustomImageRep> {
            // The same drawing; what it drew is drawn again as needed.
            let copy = Self::alloc().set_ivars(CustomIvars {
                flipped: self.ivars().flipped,
                handler: self.ivars().handler.clone(),
                draw_selector: self.ivars().draw_selector,
                delegate: RefCell::new(self.ivars().delegate.borrow().clone()),
                cache: RefCell::new(Vec::new()),
            });
            // SAFETY: NSImageRep's designated initializer.
            let copy: Retained<Self> = unsafe { msg_send![super(copy), init] };
            rep_ivars(&*copy).copy_from(rep_ivars(self));
            // SAFETY: NSCustomImageRepImpl is the class NSCustomImageRep
            // names.
            unsafe { Retained::cast_unchecked(copy) }
        }
    }

    unsafe impl NSObjectProtocol for NSCustomImageRepImpl {}
);

impl NSCustomImageRepImpl {
    fn as_rep(&self) -> &NSImageRep {
        // SAFETY: NSCustomImageRepImpl is an NSImageRep.
        unsafe { &*(self as *const Self).cast::<NSImageRep>() }
    }

    /// Run the drawing (handler or delegate) into a bitmap of `pixels`,
    /// the rep's size in points, cached per pixel size and appearance.
    pub(crate) fn rendered(&self, pixels: (usize, usize)) -> Option<Retained<NSBitmapImageRep>> {
        let key = CacheKey { pixels, appearance: appearance::current() };
        if let Some((_, rep)) = self.ivars().cache.borrow().iter().find(|(k, _)| *k == key) {
            return Some(rep.clone());
        }
        let size = rep_ivars(self).size.get();
        let rep = new_bitmap(pixels.0, pixels.1, size)?;
        let rect = NSRect::new(NSPoint::ZERO, size);
        render_into(&rep, self.ivars().flipped, || match (&self.ivars().handler, self.ivars().draw_selector) {
            (Some(h), _) => {
                h.call((rect,));
            }
            (None, Some(sel)) => {
                let delegate = self.ivars().delegate.borrow().as_ref().and_then(|w| w.load());
                if let Some(d) = delegate {
                    // SAFETY: the draw selector takes the rep.
                    let _: () = unsafe { objc2::runtime::MessageReceiver::send_message(&*d, sel, (self.as_rep(),)) };
                }
            }
            _ => {}
        });
        let mut cache = self.ivars().cache.borrow_mut();
        if cache.len() >= 4 {
            cache.remove(0);
        }
        cache.push((key, rep.clone()));
        Some(rep)
    }
}

/// A new transparent 8-bit RGBA bitmap, `size` points.
pub(crate) fn new_bitmap(w: usize, h: usize, size: NSSize) -> Option<Retained<NSBitmapImageRep>> {
    crate::load_shell::<NSBitmapImageRep>();
    // SAFETY: NULL planes make the rep allocate; the color space name is a
    // constant.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            w.max(1) as NSInteger,
            h.max(1) as NSInteger,
            8,
            4,
            true,
            false,
            &NSString::from_str("NSCalibratedRGBColorSpace"),
            NSBitmapFormat::empty(),
            0,
            32,
        )
    }?;
    rep.setSize(size);
    Some(rep)
}

/// Run `draw` with a context on `rep` current (flipped if asked), then put
/// the old context back.
pub(crate) fn render_into(rep: &NSBitmapImageRep, flipped: bool, draw: impl FnOnce()) {
    let Some(ctx) = crate::context::bitmap_context(rep) else { return };
    if flipped {
        crate::context::flip(&ctx);
    }
    crate::context::begin_current(ctx);
    draw();
    crate::context::end_current();
}

/// Record `rep` drawn from `src` (image points; empty for all of it) into
/// `dst` (user space). Returns whether there was a context to draw in.
pub(crate) fn draw_rep(
    rep: &NSImageRep,
    src: NSRect,
    dst: NSRect,
    blend: Blend,
    fraction: f64,
    respect_flipped: bool,
) -> bool {
    draw_rep_tinted(rep, src, dst, blend, fraction, (respect_flipped, false), None)
}

/// [`draw_rep`], tinted (a template's alpha filled with `tint`) and, with
/// `flipped.1`, upside down: an image whose own coordinates run down
/// (`-[NSImage setFlipped:]`), whose `src` counts from its top.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_rep_tinted(
    rep: &NSImageRep,
    src: NSRect,
    dst: NSRect,
    blend: Blend,
    fraction: f64,
    (respect_flipped, image_flipped): (bool, bool),
    tint: Option<Color>,
) -> bool {
    let Some(scale) = crate::context::with_state(|st| device_scale(st)) else { return false };
    let size = rep_imp(rep).ivars().size.get();
    // The pixels: a bitmap's own, or a custom rep's drawing at the size it
    // will show.
    let (bitmap, from_custom) = match rep.downcast_ref::<NSBitmapImageRep>() {
        Some(b) => (b.retain(), false),
        None => {
            let Some(custom) = rep.downcast_ref::<NSCustomImageRep>() else { return false };
            // SAFETY: every NSCustomImageRep is an NSCustomImageRepImpl.
            let custom = unsafe { &*(custom as *const NSCustomImageRep).cast::<NSCustomImageRepImpl>() };
            let pixels = |points: f64| (points.abs() * scale).ceil().max(1.0) as usize;
            let Some(b) = custom.rendered((pixels(dst.size.width), pixels(dst.size.height))) else { return false };
            (b, true)
        }
    };
    let Some(image) = crate::bitmap::image_data(&bitmap) else { return false };
    let (pw, ph) = (image.width as f64, image.height as f64);
    // `src` in the image's points (origin at the bottom left) to pixels
    // (origin at the top left).
    let (sx, sy) = if from_custom || size.width <= 0.0 || size.height <= 0.0 {
        (pw / size.width.max(1e-9), ph / size.height.max(1e-9))
    } else {
        (pw / size.width, ph / size.height)
    };
    let src_px = if src.size.width <= 0.0 || src.size.height <= 0.0 {
        Rect::new(0.0, 0.0, pw as f32, ph as f32)
    } else {
        let x0 = src.origin.x * sx;
        let top = if image_flipped { src.origin.y } else { size.height - (src.origin.y + src.size.height) };
        let y0 = top * sy;
        Rect::new(x0 as f32, y0 as f32, (x0 + src.size.width * sx) as f32, (y0 + src.size.height * sy) as f32)
    };
    let recorded = crate::context::with_state(|st| {
        // Where the image's top row goes: the rectangle's top, unless the
        // context is flipped and that's respected, or the image is.
        let top_at_min = (st.flipped && respect_flipped) != image_flipped;
        let (x0, x1) = (dst.origin.x as f32, (dst.origin.x + dst.size.width) as f32);
        let (lo, hi) = (dst.origin.y as f32, (dst.origin.y + dst.size.height) as f32);
        let dst = if top_at_min { Rect::new(x0, lo, x1, hi) } else { Rect::new(x0, hi, x1, lo) };
        let quality = match st.gs.interpolation {
            NSImageInterpolation::None => Quality::None,
            NSImageInterpolation::Low => Quality::Low,
            NSImageInterpolation::High => Quality::High,
            _ => Quality::Medium,
        };
        let mut draw = st.gs.draw();
        draw.blend = blend;
        st.push(Op::Image {
            image: image.clone(),
            src: src_px,
            dst,
            alpha: fraction.clamp(0.0, 1.0) as f32,
            quality,
            tint,
            draw,
        });
        matches!(st.target, crate::context::Target::Record)
    });
    if recorded == Some(true) {
        crate::bitmap::mark_recorded(&bitmap);
    }
    true
}

/// Device pixels per user-space unit, roughly, where drawing goes now.
pub(crate) fn device_scale(st: &crate::context::ContextState) -> f64 {
    let [a, b, c, d, _, _] = st.gs.ctm.as_coeffs();
    let ctm = (a.hypot(b)).max(c.hypot(d));
    ctm * st.pixels_per_point()
}

// Files and data.

/// The bytes of an `NSData` (or anything answering `bytes` and `length`).
pub(crate) fn data_bytes(data: &AnyObject) -> Option<Arc<[u8]>> {
    // SAFETY: NSData answers -length and -bytes.
    let len: usize = unsafe { msg_send![data, length] };
    if len == 0 {
        return None;
    }
    // SAFETY: as above; -bytes points at `length` bytes.
    let bytes: *const std::ffi::c_void = unsafe { msg_send![data, bytes] };
    if bytes.is_null() {
        return None;
    }
    // SAFETY: as above.
    Some(Arc::from(unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), len) }))
}

/// A new `NSData` holding `bytes`, when Foundation has the class.
pub(crate) fn make_data(bytes: &[u8]) -> Option<Retained<AnyObject>> {
    // Looked up by name: NSData belongs to Foundation's side, and a build
    // without it has none to make.
    let class = AnyClass::get(c"NSData")?;
    // SAFETY: +dataWithBytes:length: copies the bytes into a new NSData.
    unsafe { msg_send![class, dataWithBytes: bytes.as_ptr().cast::<std::ffi::c_void>(), length: bytes.len()] }
}

/// A number property of a dictionary, by key name.
pub(crate) fn number_for(dict: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<f64> {
    let value = dict.objectForKey(&NSString::from_str(key))?;
    // SAFETY: numbers answer -doubleValue.
    Some(unsafe { msg_send![&*value, doubleValue] })
}

pub(crate) fn read_file(path: &str) -> Option<Arc<[u8]>> {
    std::fs::read(path).ok().map(Arc::from)
}

/// The file path of a file URL.
pub(crate) fn url_path(url: &AnyObject) -> Option<Retained<NSString>> {
    // SAFETY: NSURL answers -path.
    unsafe { msg_send![url, path] }
}

/// Keys of bitmaps drawn in windows that went away, for the render
/// thread's cache to forget.
static FORGOTTEN: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// A representation with this key is gone: this thread's cache forgets it
/// now, and the render thread's, if it was drawn there (`recorded`), after
/// the main loop's turn ([`send_forgotten`]).
pub(crate) fn forget(key: u64, recorded: bool) {
    crate::raster::images::forget(&[key]);
    if recorded {
        FORGOTTEN.lock().unwrap_or_else(|e| e.into_inner()).push(key);
    }
}

/// Once a turn of the main loop, after the display pass has sent its
/// paints: tell the render thread which bitmaps went away, in one message.
pub(crate) fn send_forgotten() {
    let keys = std::mem::take(&mut *FORGOTTEN.lock().unwrap_or_else(|e| e.into_inner()));
    if !keys.is_empty() {
        crate::app::send_if_running(crate::protocol::ToRender::ForgetImages { keys });
    }
}
