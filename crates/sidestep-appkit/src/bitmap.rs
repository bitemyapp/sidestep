//! `NSBitmapImageRep`: pixels in memory, in the layouts AppKit describes
//! with samples per pixel, bits per sample, planes and a bitmap format.
//!
//! A rep keeps its pixels one of three ways: a buffer of its own (rows
//! `bytesPerRow` apart, rounded up to 32 bytes as AppKit does, zeroed),
//! planes the caller owns, or an encoded file not decoded yet. Decoding is
//! lazy: `initWithData:` reads only the header (`codec`), and the pixels
//! are decoded when something asks for them (`bitmapData`, `colorAtX:y:`)
//! or, for drawing into a window, on the render thread (`raster::images`).
//!
//! Drawing a rep hands the rasterizer an `ImageData` snapshot: premultiplied
//! RGBA shared through an `Arc`. The snapshot is made again only when the
//! pixels may have changed: every way to write them (`bitmapData`,
//! `getBitmapDataPlanes:`, `setColor:atX:y:`, `setPixel:atX:y:`, drawing
//! into the rep through a context) counts a new generation. Pixels written
//! behind the rep's back (into caller-owned planes, or through a pointer
//! kept from before) show once something counts a generation, as in
//! AppKit, which caches what it drew too.
//!
//! Graphics contexts draw straight into a rep's pixels when they're 8-bit
//! RGBA with premultiplied alpha last, tiny-skia's own format, and 4-byte
//! aligned. Other layouts AppKit draws into (gray, gray and alpha, RGB in
//! four samples, alpha first, 16-bit and floating-point samples) are drawn
//! through a scratch copy in that format: each op unpacks the rep, draws,
//! and packs back the pixels it changed, so pixels it didn't touch keep
//! every bit of their precision.
//!
//! Sizes are checked: a layout whose bytes wouldn't fit in memory (a file
//! claiming 2³¹ × 2³¹ pixels, say) makes no rep, as in AppKit, and pixel
//! pointers are made only inside the pixels. Pointers handed to the program
//! (`bitmapData`) come straight from the buffer's allocation, never
//! through a reference to all of it, so drawing into the rep later doesn't
//! invalidate them.

use std::cell::{Cell, RefCell};
use std::ffi::c_uchar;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSColor,
    NSColorRenderingIntent, NSColorSpace, NSImageRep, NSTIFFCompression,
};
use objc2_foundation::{NSArray, NSDictionary, NSInteger, NSSize, NSString, NSUInteger};

use objc2_foundation::{NSCopying, NSZone};

use crate::color::Space;
use crate::image_rep::{RepIvars, rep_ivars};
use crate::protocol::Op;
use crate::raster::images::{ImageData, Pixels};

// Property keys, with AppKit's values.
sidestep_foundation::constant_string!(NSImageCompressionMethod = "NSImageCompressionMethod");
sidestep_foundation::constant_string!(NSImageCompressionFactor = "NSImageCompressionFactor");
sidestep_foundation::constant_string!(NSImageDitherTransparency = "NSImageDitherTransparency");
sidestep_foundation::constant_string!(NSImageRGBColorTable = "NSImageRGBColorTable");
sidestep_foundation::constant_string!(NSImageInterlaced = "NSImageInterlaced");
sidestep_foundation::constant_string!(NSImageColorSyncProfileData = "NSImageColorSyncProfileData");
sidestep_foundation::constant_string!(NSImageFrameCount = "NSImageFrameCount");
sidestep_foundation::constant_string!(NSImageCurrentFrame = "NSImageCurrentFrame");
sidestep_foundation::constant_string!(NSImageCurrentFrameDuration = "NSImageCurrentFrameDuration");
sidestep_foundation::constant_string!(NSImageLoopCount = "NSImageLoopCount");
sidestep_foundation::constant_string!(NSImageGamma = "NSImageGamma");
sidestep_foundation::constant_string!(NSImageProgressive = "NSImageProgressive");
sidestep_foundation::constant_string!(NSImageEXIFData = "NSImageEXIFData");
sidestep_foundation::constant_string!(NSImageIPTCData = "NSImageIPTCData");
sidestep_foundation::constant_string!(NSImageFallbackBackgroundColor = "NSImageFallbackBackgroundColor");

/// How a rep lays out its pixels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layout {
    pub width: usize,
    pub height: usize,
    pub bps: usize,
    pub spp: usize,
    pub alpha: bool,
    pub planar: bool,
    pub format: NSBitmapFormat,
    pub bpr: usize,
    pub bpp: usize,
}

impl Layout {
    fn planes(&self) -> usize {
        if self.planar { self.spp } else { 1 }
    }

    /// Bytes a plane holds. (Every layout a rep has passed [`sizes`].)
    fn plane_bytes(&self) -> usize {
        self.bpr * self.height
    }

    /// Bytes a plane holds and all of them, if they fit in memory.
    fn sizes(&self) -> Option<(usize, usize)> {
        let plane = self.bpr.checked_mul(self.height)?;
        let all = plane.checked_mul(self.planes())?;
        (all <= isize::MAX as usize).then_some((plane, all))
    }

    /// Straight in a context's format: 8-bit RGBA, alpha last and
    /// premultiplied, meshed.
    pub fn is_canvas(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.bps == 8
            && self.spp == 4
            && self.alpha
            && !self.planar
            && self.bpp == 32
            && self.bpr.is_multiple_of(4)
            && !self.format.intersects(
                NSBitmapFormat::AlphaFirst
                    | NSBitmapFormat::AlphaNonpremultiplied
                    | NSBitmapFormat::FloatingPointSamples,
            )
    }

    /// How a context draws into this layout, if it can: meshed samples of
    /// 8 or 16 bits or 32-bit floats, in native byte order, gray or RGB (in
    /// four samples) with premultiplied alpha or none, as AppKit's
    /// contexts take them.
    fn drawn_as(&self) -> Option<Samples> {
        let foreign = if cfg!(target_endian = "little") {
            NSBitmapFormat::SixteenBitBigEndian | NSBitmapFormat::ThirtyTwoBitBigEndian
        } else {
            NSBitmapFormat::SixteenBitLittleEndian | NSBitmapFormat::ThirtyTwoBitLittleEndian
        };
        if self.width == 0
            || self.height == 0
            || self.planar
            || self.format.intersects(NSBitmapFormat::AlphaNonpremultiplied | foreign)
        {
            return None;
        }
        let float = self.format.contains(NSBitmapFormat::FloatingPointSamples);
        let kind = match (self.bps, float) {
            (8, false) => Sample::U8,
            (16, false) => Sample::U16,
            (32, true) => Sample::F32,
            _ => return None,
        };
        let colors = self.spp.checked_sub(usize::from(self.alpha))?;
        let slots = match colors {
            1 => self.spp,
            3 => 4,
            _ => return None,
        };
        if self.bpp != slots * self.bps {
            return None;
        }
        let alpha = self.alpha.then(|| if self.format.contains(NSBitmapFormat::AlphaFirst) { 0 } else { colors });
        let first = usize::from(alpha == Some(0));
        Some(Samples { kind, gray: colors == 1, first, alpha })
    }
}

/// How a context's drawing is stored in a layout that isn't its own.
#[derive(Clone, Copy, Debug)]
struct Samples {
    kind: Sample,
    gray: bool,
    /// The sample the colors start at.
    first: usize,
    /// The alpha's sample, if there's alpha.
    alpha: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
enum Sample {
    U8,
    U16,
    F32,
}

/// Where a rep's pixels are.
enum Storage {
    /// Its own, 4-byte aligned: `bpr × height` bytes per plane, planes one
    /// after another.
    Owned(Vec<u32>),
    /// The caller's planes.
    Borrowed([*mut u8; 5]),
    /// A file, decoded on demand, turned upright or not.
    Encoded(Arc<[u8]>, bool),
}

static KEYS: AtomicU64 = AtomicU64::new(1);

pub(crate) struct BitmapIvars {
    layout: RefCell<Layout>,
    space: Cell<Space>,
    storage: RefCell<Storage>,
    key: u64,
    generation: Cell<u64>,
    /// The last snapshot drawn, and the generation it shows.
    snapshot: RefCell<Option<Arc<ImageData>>>,
    /// Drawn in a window, so the render thread may have it cached.
    recorded: Cell<bool>,
    properties: RefCell<Vec<(Retained<NSString>, Retained<AnyObject>)>>,
    compression: Cell<(NSTIFFCompression, f32)>,
    /// An animated GIF's frames and which one shows.
    frames: RefCell<Option<Box<Frames>>>,
}

/// An animated file's frames (`NSImageFrameCount` and its kin): which
/// one shows, and the decoder that brings the others, one at a time, as
/// they're asked for (made when the first is).
struct Frames {
    file: Arc<[u8]>,
    info: crate::codec::Frames,
    current: usize,
    decoder: Option<crate::codec::GifFrames>,
}

impl Clone for Frames {
    /// A copy shows the same frame, and decodes its own when told another.
    fn clone(&self) -> Self {
        Frames { file: self.file.clone(), info: self.info.clone(), current: self.current, decoder: None }
    }
}

impl BitmapIvars {
    fn new(layout: Layout, space: Space, storage: Storage) -> Self {
        BitmapIvars {
            layout: RefCell::new(layout),
            space: Cell::new(space),
            storage: RefCell::new(storage),
            key: KEYS.fetch_add(1, Ordering::Relaxed),
            generation: Cell::new(0),
            snapshot: RefCell::new(None),
            recorded: Cell::new(false),
            properties: RefCell::new(Vec::new()),
            compression: Cell::new((NSTIFFCompression::None, 0.0)),
            frames: RefCell::new(None),
        }
    }
}

/// `n` zeroed words, or `None` if the memory can't be had.
fn zeroed_words(n: usize) -> Option<Vec<u32>> {
    if n == 0 {
        return Some(Vec::new());
    }
    let layout = std::alloc::Layout::array::<u32>(n).ok()?;
    // SAFETY: the layout's size isn't zero.
    let p = unsafe { std::alloc::alloc_zeroed(layout) };
    if p.is_null() {
        return None;
    }
    // SAFETY: `p` is the global allocator's, allocated for exactly `n`
    // u32s with their alignment, all zero, which is a valid u32.
    Some(unsafe { Vec::from_raw_parts(p.cast::<u32>(), n, n) })
}

define_class!(
    // SAFETY: NSImageRep has no subclassing requirements; a rep is used by
    // one thread at a time.
    #[unsafe(super(NSImageRep, objc2::runtime::NSObject))]
    #[name = "NSBitmapImageRep"]
    #[ivars = BitmapIvars]
    pub(crate) struct NSBitmapImageRepImpl;

    impl NSBitmapImageRepImpl {
        #[unsafe(method_id(initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bytesPerRow:bitsPerPixel:))]
        #[allow(clippy::too_many_arguments)]
        fn init_planes(
            this: Allocated<Self>,
            planes: *mut *mut c_uchar,
            width: NSInteger,
            height: NSInteger,
            bps: NSInteger,
            spp: NSInteger,
            alpha: bool,
            planar: bool,
            space: &NSString,
            bpr: NSInteger,
            bpp: NSInteger,
        ) -> Option<Retained<Self>> {
            init_with(this, planes, (width, height, bps, spp), alpha, planar, space, NSBitmapFormat::empty(), bpr, bpp)
        }

        #[unsafe(method_id(initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bitmapFormat:bytesPerRow:bitsPerPixel:))]
        #[allow(clippy::too_many_arguments)]
        fn init_planes_format(
            this: Allocated<Self>,
            planes: *mut *mut c_uchar,
            width: NSInteger,
            height: NSInteger,
            bps: NSInteger,
            spp: NSInteger,
            alpha: bool,
            planar: bool,
            space: &NSString,
            format: NSBitmapFormat,
            bpr: NSInteger,
            bpp: NSInteger,
        ) -> Option<Retained<Self>> {
            init_with(this, planes, (width, height, bps, spp), alpha, planar, space, format, bpr, bpp)
        }

        #[unsafe(method_id(initWithData:))]
        fn init_with_data(this: Allocated<Self>, data: &AnyObject) -> Option<Retained<Self>> {
            init_encoded(this, crate::image_rep::data_bytes(data).unwrap_or_else(|| Arc::from(&[][..])), true).filter(|r| has_pixels(r))
        }

        #[unsafe(method_id(imageRepWithData:))]
        fn image_rep_with_data(data: &AnyObject) -> Option<Retained<NSBitmapImageRep>> {
            crate::image_rep::data_bytes(data).and_then(|b| from_file_bytes(b, true))
        }

        #[unsafe(method_id(imageRepsWithData:))]
        fn image_reps_with_data(data: &AnyObject) -> Retained<NSArray<NSImageRep>> {
            let reps: Vec<Retained<NSImageRep>> = crate::image_rep::data_bytes(data)
                .and_then(|b| from_file_bytes(b, true))
                .map(Retained::into_super)
                .into_iter()
                .collect();
            NSArray::from_retained_slice(&reps)
        }

        #[unsafe(method_id(imageTypes))]
        fn image_types() -> Retained<NSArray<NSString>> {
            crate::image_rep::type_list()
        }

        #[unsafe(method_id(imageUnfilteredTypes))]
        fn image_unfiltered_types() -> Retained<NSArray<NSString>> {
            crate::image_rep::type_list()
        }

        #[unsafe(method(bitmapData))]
        fn bitmap_data(&self) -> *mut c_uchar {
            self.touch();
            self.plane(0)
        }

        #[unsafe(method(getBitmapDataPlanes:))]
        fn get_bitmap_data_planes(&self, out: NonNull<*mut c_uchar>) {
            self.touch();
            let n = self.ivars().layout.borrow().planes();
            for i in 0..5 {
                let p = if i < n { self.plane(i) } else { std::ptr::null_mut() };
                // SAFETY: the caller passes room for five plane pointers.
                unsafe { *out.as_ptr().add(i) = p };
            }
        }

        #[unsafe(method(isPlanar))]
        fn is_planar(&self) -> bool {
            self.ivars().layout.borrow().planar
        }

        #[unsafe(method(samplesPerPixel))]
        fn samples_per_pixel(&self) -> NSInteger {
            self.ivars().layout.borrow().spp as NSInteger
        }

        #[unsafe(method(bitsPerPixel))]
        fn bits_per_pixel(&self) -> NSInteger {
            self.ivars().layout.borrow().bpp as NSInteger
        }

        #[unsafe(method(bytesPerRow))]
        fn bytes_per_row(&self) -> NSInteger {
            self.ivars().layout.borrow().bpr as NSInteger
        }

        #[unsafe(method(bytesPerPlane))]
        fn bytes_per_plane(&self) -> NSInteger {
            self.ivars().layout.borrow().plane_bytes() as NSInteger
        }

        #[unsafe(method(numberOfPlanes))]
        fn number_of_planes(&self) -> NSInteger {
            self.ivars().layout.borrow().planes() as NSInteger
        }

        #[unsafe(method(bitmapFormat))]
        fn bitmap_format(&self) -> NSBitmapFormat {
            self.ivars().layout.borrow().format
        }

        #[unsafe(method_id(colorSpace))]
        fn color_space(&self) -> Retained<NSColorSpace> {
            crate::color::space(self.ivars().space.get())
        }

        #[unsafe(method_id(colorAtX:y:))]
        fn color_at(&self, x: NSInteger, y: NSInteger) -> Option<Retained<NSColor>> {
            self.color(x, y)
        }

        #[unsafe(method(setColor:atX:y:))]
        fn set_color(&self, color: &NSColor, x: NSInteger, y: NSInteger) {
            let c = crate::color::resolve(color);
            let a = c[3].clamp(0.0, 1.0) as f64;
            self.write(x, y, [c[0] as f64 * a, c[1] as f64 * a, c[2] as f64 * a, a]);
        }

        #[unsafe(method(getPixel:atX:y:))]
        fn get_pixel(&self, p: NonNull<NSUInteger>, x: NSInteger, y: NSInteger) {
            let Some(samples) = self.samples(x, y) else { return };
            for (i, s) in samples.iter().enumerate() {
                // SAFETY: the caller passes room for samplesPerPixel values.
                unsafe { *p.as_ptr().add(i) = *s };
            }
        }

        #[unsafe(method(setPixel:atX:y:))]
        fn set_pixel(&self, p: NonNull<NSUInteger>, x: NSInteger, y: NSInteger) {
            let spp = self.ivars().layout.borrow().spp;
            // SAFETY: the caller passes samplesPerPixel values.
            let samples: Vec<NSUInteger> = (0..spp).map(|i| unsafe { *p.as_ptr().add(i) }).collect();
            self.put_samples(x, y, &samples);
        }

        #[unsafe(method_id(bitmapImageRepByRetaggingWithColorSpace:))]
        fn retagged(&self, space: &NSColorSpace) -> Option<Retained<NSBitmapImageRep>> {
            self.copy_in(crate::color::space_of(space))
        }

        #[unsafe(method_id(bitmapImageRepByConvertingToColorSpace:renderingIntent:))]
        fn converted(&self, space: &NSColorSpace, _intent: NSColorRenderingIntent) -> Option<Retained<NSBitmapImageRep>> {
            // No color management: the same pixels, called the new space's.
            self.copy_in(crate::color::space_of(space))
        }

        #[unsafe(method_id(representationUsingType:properties:))]
        fn representation(
            &self,
            kind: NSBitmapImageFileType,
            properties: &NSDictionary<NSBitmapImageRepPropertyKey, AnyObject>,
        ) -> Option<Retained<AnyObject>> {
            self.encoded(kind, crate::image_rep::number_for(properties, "NSImageCompressionFactor"))
        }

        #[unsafe(method_id(representationOfImageRepsInArray:usingType:properties:))]
        fn representation_of(
            reps: &NSArray<NSImageRep>,
            kind: NSBitmapImageFileType,
            properties: &NSDictionary<NSBitmapImageRepPropertyKey, AnyObject>,
        ) -> Option<Retained<AnyObject>> {
            let quality = crate::image_rep::number_for(properties, "NSImageCompressionFactor");
            reps.iter().find_map(|r| r.downcast::<NSBitmapImageRep>().ok()).and_then(|r| imp(&r).encoded(kind, quality))
        }

        #[unsafe(method_id(TIFFRepresentation))]
        fn tiff_representation(&self) -> Option<Retained<AnyObject>> {
            self.encoded(NSBitmapImageFileType::TIFF, None)
        }

        #[unsafe(method_id(TIFFRepresentationUsingCompression:factor:))]
        fn tiff_representation_using(&self, _comp: NSTIFFCompression, _factor: f32) -> Option<Retained<AnyObject>> {
            self.encoded(NSBitmapImageFileType::TIFF, None)
        }

        #[unsafe(method_id(TIFFRepresentationOfImageRepsInArray:))]
        fn tiff_of(reps: &NSArray<NSImageRep>) -> Option<Retained<AnyObject>> {
            reps.iter()
                .find_map(|r| r.downcast::<NSBitmapImageRep>().ok())
                .and_then(|r| imp(&r).encoded(NSBitmapImageFileType::TIFF, None))
        }

        #[unsafe(method(getCompression:factor:))]
        fn get_compression(&self, comp: *mut NSTIFFCompression, factor: *mut f32) {
            let (c, f) = self.ivars().compression.get();
            // SAFETY: each pointer is null or writable.
            unsafe {
                if !comp.is_null() {
                    *comp = c;
                }
                if !factor.is_null() {
                    *factor = f;
                }
            }
        }

        #[unsafe(method(setCompression:factor:))]
        fn set_compression(&self, comp: NSTIFFCompression, factor: f32) {
            self.ivars().compression.set((comp, factor));
        }

        #[unsafe(method_id(valueForProperty:))]
        fn value_for_property(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            let key = key.to_string();
            self.frame_property(&key).or_else(|| {
                self.ivars().properties.borrow().iter().find(|(k, _)| k.to_string() == key).map(|(_, v)| v.clone())
            })
        }

        #[unsafe(method(setProperty:withValue:))]
        fn set_property(&self, key: &NSString, value: Option<&AnyObject>) {
            if !self.set_frame_property(key, value) {
                self.store_property(key, value);
            }
        }

        #[unsafe(method_id(initForIncrementalLoad))]
        fn init_for_incremental_load(this: Allocated<Self>) -> Retained<Self> {
            init_encoded(this, Arc::from(&[][..]), false).expect("an empty rep")
        }

        #[unsafe(method(incrementalLoadFromData:complete:))]
        fn incremental_load(&self, data: &AnyObject, complete: bool) -> NSInteger {
            self.load_incrementally(data, complete)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSBitmapImageRep> {
            self.duplicate()
        }
    }

    unsafe impl NSObjectProtocol for NSBitmapImageRepImpl {}

    unsafe impl NSCopying for NSBitmapImageRepImpl {}
);

impl Drop for NSBitmapImageRepImpl {
    fn drop(&mut self) {
        crate::image_rep::forget(self.ivars().key, self.ivars().recorded.get());
    }
}

/// `rep` was drawn in a window: the render thread keeps it cached until
/// it goes.
pub(crate) fn mark_recorded(rep: &NSBitmapImageRep) {
    imp(rep).ivars().recorded.set(true);
}

pub(crate) fn imp(rep: &NSBitmapImageRep) -> &NSBitmapImageRepImpl {
    // SAFETY: every NSBitmapImageRep is an NSBitmapImageRepImpl.
    unsafe { &*(rep as *const NSBitmapImageRep).cast::<NSBitmapImageRepImpl>() }
}

/// AppKit's row length when none is given: whole pixels, rounded up to 32
/// bytes (`None` if that many don't fit in memory).
fn default_bpr(width: usize, bpp: usize) -> Option<usize> {
    width.checked_mul(bpp)?.div_ceil(8).div_ceil(32).checked_mul(32)
}

#[allow(clippy::too_many_arguments)]
fn init_with(
    this: Allocated<NSBitmapImageRepImpl>,
    planes: *mut *mut c_uchar,
    (width, height, bps, spp): (NSInteger, NSInteger, NSInteger, NSInteger),
    alpha: bool,
    planar: bool,
    space: &NSString,
    format: NSBitmapFormat,
    bpr: NSInteger,
    bpp: NSInteger,
) -> Option<Retained<NSBitmapImageRepImpl>> {
    let (width, height) = (usize::try_from(width).ok()?, usize::try_from(height).ok()?);
    let (bps, spp) = (usize::try_from(bps).ok()?, usize::try_from(spp).ok()?);
    if width == 0 || height == 0 || !matches!(bps, 1 | 2 | 4 | 8 | 12 | 16 | 32) || !(1..=5).contains(&spp) {
        return None;
    }
    let meshed_bpp = if planar { bps } else { bps * spp };
    let bpp = match usize::try_from(bpp).ok()? {
        0 => meshed_bpp,
        b if b >= meshed_bpp => b,
        _ => return None,
    };
    let row_bits = width.checked_mul(bpp)?;
    let bpr = match usize::try_from(bpr).ok()? {
        0 => default_bpr(width, bpp)?,
        b if b.checked_mul(8)? >= row_bits => b,
        _ => return None,
    };
    let layout = Layout { width, height, bps, spp, alpha, planar, format, bpr, bpp };
    // AppKit makes no rep whose bytes don't fit in memory.
    let (_, all) = layout.sizes()?;
    let storage = if planes.is_null() || {
        // SAFETY: a non-null `planes` has at least one entry.
        unsafe { *planes }.is_null()
    } {
        Storage::Owned(zeroed_words(all.div_ceil(4))?)
    } else {
        let mut p = [std::ptr::null_mut(); 5];
        for (i, slot) in p.iter_mut().enumerate().take(layout.planes()) {
            // SAFETY: the caller passes one pointer per plane.
            *slot = unsafe { *planes.add(i) };
        }
        Storage::Borrowed(p)
    };
    let name = space.to_string();
    let space_kind = space_for_name(&name, spp - usize::from(alpha));
    let this = this.set_ivars(BitmapIvars::new(layout, space_kind, storage));
    // SAFETY: NSImageRep's designated initializer.
    let this: Retained<NSBitmapImageRepImpl> = unsafe { msg_send![super(this), init] };
    set_rep(&this, width, height, bps, alpha, &name);
    Some(this)
}

fn space_for_name(name: &str, colors: usize) -> Space {
    match (name, colors) {
        ("NSCalibratedRGBColorSpace", _) => Space::GenericRgb,
        ("NSCalibratedWhiteColorSpace" | "NSCalibratedBlackColorSpace", _) => Space::GenericGray,
        ("NSDeviceWhiteColorSpace" | "NSDeviceBlackColorSpace", _) => Space::DeviceGray,
        ("NSDeviceCMYKColorSpace", _) => Space::DeviceCmyk,
        (_, 1) => Space::DeviceGray,
        _ => Space::DeviceRgb,
    }
}

/// Fill in what the rep reports as an `NSImageRep`.
fn set_rep(rep: &NSBitmapImageRepImpl, width: usize, height: usize, bps: usize, alpha: bool, space: &str) {
    let ivars: &RepIvars = rep_ivars(rep);
    ivars.size.set(NSSize::new(width as f64, height as f64));
    ivars.pixels.set((width as NSInteger, height as NSInteger));
    ivars.bps.set(bps as NSInteger);
    ivars.alpha.set(alpha);
    ivars.opaque.set(true);
    *ivars.space_name.borrow_mut() = NSString::from_str(space);
}

/// A rep for an encoded file, sized from its header, `None` if the header
/// isn't one the codecs read.
fn init_encoded(
    this: Allocated<NSBitmapImageRepImpl>,
    bytes: Arc<[u8]>,
    orient: bool,
) -> Option<Retained<NSBitmapImageRepImpl>> {
    let header = if bytes.is_empty() { None } else { Some(crate::codec::header(&bytes)?) };
    let layout = Layout {
        width: 0,
        height: 0,
        bps: 8,
        spp: 4,
        alpha: true,
        planar: false,
        format: NSBitmapFormat::empty(),
        bpr: 0,
        bpp: 32,
    };
    let this = this.set_ivars(BitmapIvars::new(layout, Space::Srgb, Storage::Owned(Vec::new())));
    // SAFETY: NSImageRep's designated initializer.
    let this: Retained<NSBitmapImageRepImpl> = unsafe { msg_send![super(this), init] };
    if let Some(header) = header
        && !this.adopt_file(bytes, &header, orient)
    {
        return None;
    }
    Some(this)
}

/// A new rep for a file's bytes.
pub(crate) fn from_file_bytes(bytes: Arc<[u8]>, orient: bool) -> Option<Retained<NSBitmapImageRep>> {
    crate::load_shell::<NSBitmapImageRep>();
    let this = init_encoded(NSBitmapImageRepImpl::alloc(), bytes, orient)?;
    // SAFETY: NSBitmapImageRepImpl is the class NSBitmapImageRep names.
    Some(unsafe { Retained::cast_unchecked(this) })
}

/// Whether a rep has a size (an empty file makes none).
fn has_pixels(rep: &NSBitmapImageRepImpl) -> bool {
    rep.ivars().layout.borrow().width > 0
}

impl NSBitmapImageRepImpl {
    fn color(&self, x: NSInteger, y: NSInteger) -> Option<Retained<NSColor>> {
        let [r, g, b, a] = self.read(x, y)?;
        let space = crate::color::space(self.ivars().space.get());
        let gray = self.ivars().layout.borrow().spp - usize::from(self.ivars().layout.borrow().alpha) < 3;
        // Unpremultiplied, in the rep's space.
        let un = |v: f64| if a > 0.0 { (v / a).min(1.0) } else { 0.0 };
        let comps: Vec<f64> = if gray { vec![un(r), a] } else { vec![un(r), un(g), un(b), a] };
        // SAFETY: the components match the space's count plus alpha.
        Some(unsafe {
            NSColor::colorWithColorSpace_components_count(
                &space,
                NonNull::new(comps.as_ptr().cast_mut()).expect("components"),
                comps.len() as NSInteger,
            )
        })
    }

    /// The pixels as an image file of `kind`, in an `NSData`.
    fn encoded(&self, kind: NSBitmapImageFileType, quality: Option<f64>) -> Option<Retained<AnyObject>> {
        let (w, h, rgba) = self.straight_rgba()?;
        let bytes = crate::codec::encode(kind, w, h, &rgba, quality)?;
        crate::image_rep::make_data(&bytes)
    }

    fn load_incrementally(&self, data: &AnyObject, complete: bool) -> NSInteger {
        // Status values: completed −6, still reading the header −2,
        // unreadable −4.
        if !complete {
            return -2;
        }
        let Some(bytes) = crate::image_rep::data_bytes(data) else { return -4 };
        let Some(header) = crate::codec::header(&bytes) else { return -4 };
        if !self.adopt_file(bytes, &header, true) {
            return -4;
        }
        -6
    }

    /// Take the file's size, layout and density from its header; the
    /// pixels come when asked for. The layout is AppKit's for decoded
    /// files: 8-bit samples four bytes a pixel, rows unpadded, alpha last
    /// and unpremultiplied when the file has it; a file turned upright is
    /// redrawn, which gives it premultiplied alpha. False (and nothing
    /// taken) if its pixels wouldn't fit in memory.
    fn adopt_file(&self, bytes: Arc<[u8]>, header: &crate::codec::Header, orient: bool) -> bool {
        let turned = orient && header.orientation.turns();
        let (w, h) = if orient && header.orientation.swaps() {
            (header.height, header.width)
        } else {
            (header.width, header.height)
        };
        let (w, h) = (w as usize, h as usize);
        let alpha = header.alpha || turned;
        let format =
            if header.alpha && !turned { NSBitmapFormat::AlphaNonpremultiplied } else { NSBitmapFormat::empty() };
        let Some(bpr) = w.checked_mul(4) else { return false };
        let layout = Layout {
            width: w,
            height: h,
            bps: 8,
            spp: if alpha { 4 } else { 3 },
            alpha,
            planar: false,
            format,
            bpr,
            bpp: 32,
        };
        if layout.sizes().is_none() {
            return false;
        }
        *self.ivars().layout.borrow_mut() = layout;
        *self.ivars().frames.borrow_mut() = crate::codec::gif_frames(&bytes)
            .map(|info| Box::new(Frames { file: bytes.clone(), info, current: 0, decoder: None }));
        *self.ivars().storage.borrow_mut() = Storage::Encoded(bytes, orient);
        self.ivars().space.set(Space::Srgb);
        set_rep(self, w, h, 8, alpha, "NSCalibratedRGBColorSpace");
        rep_ivars(self).opaque.set(!alpha);
        let (dx, dy) = header.dpi;
        let (dx, dy) = if orient && header.orientation.swaps() { (dy, dx) } else { (dx, dy) };
        rep_ivars(self).size.set(NSSize::new(w as f64 * 72.0 / dx, h as f64 * 72.0 / dy));
        self.bump();
        true
    }

    fn bump(&self) {
        self.ivars().generation.set(self.ivars().generation.get() + 1);
    }

    fn store_property(&self, key: &NSString, value: Option<&AnyObject>) {
        let mut props = self.ivars().properties.borrow_mut();
        let name = key.to_string();
        props.retain(|(k, _)| k.to_string() != name);
        if let Some(v) = value {
            props.push((NSString::from_str(&name), v.retain()));
        }
    }

    /// `setProperty:withValue:` for the current frame of an animated file:
    /// true if it was that.
    fn set_frame_property(&self, key: &NSString, value: Option<&AnyObject>) -> bool {
        if key.to_string() != "NSImageCurrentFrame" || self.ivars().frames.borrow().is_none() {
            return false;
        }
        // SAFETY: the frame is a number.
        if let Some(frame) = value.map(|v| -> isize { unsafe { msg_send![v, integerValue] } }) {
            self.show_frame(frame);
        }
        true
    }

    /// An animated file's `NSImageFrameCount`, `NSImageCurrentFrame`,
    /// `NSImageCurrentFrameDuration` or `NSImageLoopCount`, as numbers.
    fn frame_property(&self, key: &str) -> Option<Retained<AnyObject>> {
        use objc2_foundation::NSNumber;
        let frames = self.ivars().frames.borrow();
        let f = frames.as_ref()?;
        let number = match key {
            "NSImageFrameCount" => NSNumber::new_isize(f.info.delays.len() as isize),
            "NSImageCurrentFrame" => NSNumber::new_isize(f.current as isize),
            // Single precision, as AppKit keeps it.
            "NSImageCurrentFrameDuration" => NSNumber::new_f32(f.info.delays[f.current] as f32),
            "NSImageLoopCount" => NSNumber::new_isize(f.info.loops as isize),
            _ => return None,
        };
        Some(Retained::into_super(Retained::into_super(Retained::into_super(number))))
    }

    /// Show frame `i` of an animated file: its pixels, decoded now (the
    /// next frame alone, as an animation asks), become the rep's, in the
    /// rep's layout. A frame it doesn't have is ignored.
    fn show_frame(&self, i: isize) {
        let frame = {
            let mut frames = self.ivars().frames.borrow_mut();
            let Some(f) = frames.as_mut() else { return };
            if i < 0 || i as usize >= f.info.delays.len() || i as usize == f.current {
                return;
            }
            let file = f.file.clone();
            let decoder = f.decoder.get_or_insert_with(|| crate::codec::GifFrames::new(file));
            let Some(frame) = decoder.frame(i as usize) else { return };
            f.current = i as usize;
            frame
        };
        self.decode();
        let layout = self.ivars().layout.borrow().clone();
        let premultiply = layout.alpha && !layout.format.contains(NSBitmapFormat::AlphaNonpremultiplied);
        if let Storage::Owned(buf) = &mut *self.ivars().storage.borrow_mut() {
            let bytes = crate::raster::as_bytes(buf);
            for y in 0..layout.height {
                let src = frame.get(y * layout.width * 4..(y + 1) * layout.width * 4);
                let dst = bytes.get_mut(y * layout.bpr..y * layout.bpr + layout.width * 4);
                let (Some(src), Some(dst)) = (src, dst) else { break };
                dst.copy_from_slice(src);
                if premultiply {
                    for p in dst.as_chunks_mut::<4>().0 {
                        let a = u32::from(p[3]);
                        for c in &mut p[..3] {
                            *c = ((u32::from(*c) * a + 127) / 255) as u8;
                        }
                    }
                }
            }
        }
        self.bump();
    }

    /// The pixels may be written: decode them if they're still a file, and
    /// count a new generation.
    fn touch(&self) {
        self.decode();
        self.bump();
    }

    /// Replace a file with its pixels, in the layout the header promised.
    /// (Without the memory for them, it stays a file, with no pixels.)
    fn decode(&self) {
        let (file, upright) = match &*self.ivars().storage.borrow() {
            Storage::Encoded(f, upright) => (f.clone(), *upright),
            _ => return,
        };
        let layout = self.ivars().layout.borrow().clone();
        let premultiply = layout.alpha && !layout.format.contains(NSBitmapFormat::AlphaNonpremultiplied);
        let Some(mut buf) = zeroed_words(layout.plane_bytes().div_ceil(4)) else { return };
        let decoded = if premultiply {
            crate::codec::decode(&file, upright)
        } else {
            crate::codec::decode_straight(&file, upright)
        };
        if let Some(d) = decoded {
            let bytes = crate::raster::as_bytes(&mut buf);
            let (w, h) = (layout.width.min(d.width as usize), layout.height.min(d.height as usize));
            for y in 0..h {
                let src = &d.rgba[y * d.width as usize * 4..][..w * 4];
                let dst = &mut bytes[y * layout.bpr..][..w * 4];
                // An RGB pixel's fourth byte is padding, left opaque as
                // AppKit leaves it.
                dst.copy_from_slice(src);
            }
        }
        *self.ivars().storage.borrow_mut() = Storage::Owned(buf);
    }

    /// Where plane `i` starts: in the rep's buffer (null past its end), or
    /// the caller's. The pointer comes from the buffer's allocation with no
    /// reference to the whole buffer made on the way (`Vec::as_mut_ptr`
    /// promises that), so pointers handed out stay valid while the rep
    /// draws into the buffer later.
    fn plane(&self, i: usize) -> *mut u8 {
        let plane_bytes = self.ivars().layout.borrow().plane_bytes();
        match &mut *self.ivars().storage.borrow_mut() {
            Storage::Owned(buf) => match i.checked_mul(plane_bytes) {
                Some(at) if at < buf.len() * 4 || (at == 0 && buf.is_empty()) => {
                    buf.as_mut_ptr().cast::<u8>().wrapping_add(at)
                }
                _ => std::ptr::null_mut(),
            },
            Storage::Borrowed(p) => p.get(i).copied().unwrap_or(std::ptr::null_mut()),
            Storage::Encoded(..) => std::ptr::null_mut(),
        }
    }

    /// Bytes in the rep's own buffer, if it has one.
    fn owned_len(&self) -> Option<usize> {
        match &*self.ivars().storage.borrow() {
            Storage::Owned(buf) => Some(buf.len() * 4),
            _ => None,
        }
    }

    /// The bytes of plane `i` of row `y`, from `x` on; null unless the whole
    /// pixel is in the rep's buffer (the caller's planes are taken to hold
    /// what the layout says, as AppKit takes them).
    fn at(&self, i: usize, x: usize, y: usize) -> *mut u8 {
        let l = self.ivars().layout.borrow().clone();
        if x >= l.width || y >= l.height || i >= l.planes() {
            return std::ptr::null_mut();
        }
        let p = self.plane(i);
        if p.is_null() {
            return p;
        }
        // Inside the layout, which fits in memory, so this can't overflow.
        let off = y * l.bpr + x * l.bpp / 8;
        if let Some(len) = self.owned_len()
            && i * l.plane_bytes() + off + l.bpp.div_ceil(8) > len
        {
            return std::ptr::null_mut();
        }
        p.wrapping_add(off)
    }

    /// The samples of pixel (x, y), top-left origin, each scaled to its bit
    /// depth.
    fn samples(&self, x: NSInteger, y: NSInteger) -> Option<Vec<NSUInteger>> {
        self.decode();
        let l = self.ivars().layout.borrow().clone();
        let (x, y) = (usize::try_from(x).ok()?, usize::try_from(y).ok()?);
        if x >= l.width || y >= l.height || !matches!(l.bps, 8 | 16) {
            return None;
        }
        let bytes = l.bps / 8;
        let mut out = Vec::with_capacity(l.spp);
        for s in 0..l.spp {
            let p = if l.planar { self.at(s, x, y) } else { self.at(0, x, y).wrapping_add(s * bytes) };
            if p.is_null() {
                return None;
            }
            // SAFETY: inside the pixel's bytes.
            let v = unsafe {
                if bytes == 1 { NSUInteger::from(*p) } else { NSUInteger::from(p.cast::<u16>().read_unaligned()) }
            };
            out.push(v);
        }
        Some(out)
    }

    fn put_samples(&self, x: NSInteger, y: NSInteger, samples: &[NSUInteger]) {
        self.touch();
        let l = self.ivars().layout.borrow().clone();
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else { return };
        if x >= l.width || y >= l.height || !matches!(l.bps, 8 | 16) {
            return;
        }
        let bytes = l.bps / 8;
        for (s, v) in samples.iter().enumerate().take(l.spp) {
            let p = if l.planar { self.at(s, x, y) } else { self.at(0, x, y).wrapping_add(s * bytes) };
            if p.is_null() {
                return;
            }
            // SAFETY: inside the pixel's bytes.
            unsafe {
                if bytes == 1 {
                    *p = *v as u8;
                } else {
                    p.cast::<u16>().write_unaligned(*v as u16);
                }
            }
        }
    }

    /// Pixel (x, y) as premultiplied RGBA, 0 to 1.
    fn read(&self, x: NSInteger, y: NSInteger) -> Option<[f64; 4]> {
        let s = self.samples(x, y)?;
        let l = self.ivars().layout.borrow().clone();
        let max = ((1u64 << l.bps) - 1) as f64;
        let v: Vec<f64> = s.iter().map(|&v| v as f64 / max).collect();
        let colors = l.spp - usize::from(l.alpha);
        let (c, a) = if l.format.contains(NSBitmapFormat::AlphaFirst) && l.alpha {
            (&v[1..], v[0])
        } else {
            (&v[..colors], if l.alpha { v[colors] } else { 1.0 })
        };
        let premul = |x: f64| if l.format.contains(NSBitmapFormat::AlphaNonpremultiplied) { x * a } else { x };
        Some(match colors {
            1 | 2 => [premul(c[0]), premul(c[0]), premul(c[0]), a],
            _ => [premul(c[0]), premul(c[1]), premul(c[2]), a],
        })
    }

    /// Write premultiplied RGBA (0 to 1) to pixel (x, y).
    fn write(&self, x: NSInteger, y: NSInteger, rgba: [f64; 4]) {
        let l = self.ivars().layout.borrow().clone();
        let max = ((1u64 << l.bps.min(16)) - 1) as f64;
        let a = rgba[3];
        let un = |v: f64| {
            if l.format.contains(NSBitmapFormat::AlphaNonpremultiplied) { if a > 0.0 { v / a } else { 0.0 } } else { v }
        };
        let q = |v: f64| (v.clamp(0.0, 1.0) * max).round() as NSUInteger;
        let colors = l.spp - usize::from(l.alpha);
        let mut c: Vec<NSUInteger> = if colors < 3 {
            // Gray is the color's luminance, taken unpremultiplied.
            let gray = if a > 0.0 { crate::color::gray_of([rgba[0] / a, rgba[1] / a, rgba[2] / a]) * a } else { 0.0 };
            vec![q(un(gray))]
        } else {
            vec![q(un(rgba[0])), q(un(rgba[1])), q(un(rgba[2]))]
        };
        if l.alpha {
            if l.format.contains(NSBitmapFormat::AlphaFirst) {
                c.insert(0, q(a));
            } else {
                c.push(q(a));
            }
        }
        self.put_samples(x, y, &c);
    }

    /// A copy of this rep (pixels and all) in another space of the same
    /// kind.
    fn copy_in(&self, space: Space) -> Option<Retained<NSBitmapImageRep>> {
        self.decode();
        let l = self.ivars().layout.borrow().clone();
        let colors = l.spp - usize::from(l.alpha);
        let fits = match colors {
            1 => matches!(
                space,
                Space::GenericGray | Space::Gamma22Gray | Space::ExtendedGamma22Gray | Space::DeviceGray
            ),
            3 => !matches!(
                space,
                Space::GenericGray
                    | Space::Gamma22Gray
                    | Space::ExtendedGamma22Gray
                    | Space::DeviceGray
                    | Space::GenericCmyk
                    | Space::DeviceCmyk
            ),
            4 => matches!(space, Space::GenericCmyk | Space::DeviceCmyk),
            _ => false,
        };
        if !fits {
            return None;
        }
        let buf = self.pixels_copy()?;
        let size = rep_ivars(self).size.get();
        let name = rep_ivars(self).space_name.borrow().to_string();
        crate::load_shell::<NSBitmapImageRep>();
        let this = NSBitmapImageRepImpl::alloc().set_ivars(BitmapIvars::new(l.clone(), space, Storage::Owned(buf)));
        // SAFETY: NSImageRep's designated initializer.
        let this: Retained<NSBitmapImageRepImpl> = unsafe { msg_send![super(this), init] };
        set_rep(&this, l.width, l.height, l.bps, l.alpha, &name);
        rep_ivars(&*this).size.set(size);
        // SAFETY: NSBitmapImageRepImpl is the class NSBitmapImageRep names.
        Some(unsafe { Retained::cast_unchecked(this) })
    }

    /// A buffer of the rep's own holding a copy of its pixels (planes one
    /// after another); zeros for a file not decoded.
    fn pixels_copy(&self) -> Option<Vec<u32>> {
        let l = self.ivars().layout.borrow().clone();
        let (plane, len) = l.sizes()?;
        let mut buf = zeroed_words(len.div_ceil(4))?;
        let dst = buf.as_mut_ptr().cast::<u8>();
        match &*self.ivars().storage.borrow() {
            Storage::Owned(src) => {
                let n = len.min(src.len() * 4);
                // SAFETY: both buffers hold at least `n` bytes, and they're
                // different allocations.
                unsafe { std::ptr::copy_nonoverlapping(src.as_ptr().cast::<u8>(), dst, n) };
            }
            Storage::Borrowed(p) => {
                for (i, &src) in p.iter().enumerate().take(l.planes()) {
                    if !src.is_null() {
                        // SAFETY: the caller's planes hold bytesPerPlane
                        // bytes each; the copy has room for every plane.
                        unsafe { std::ptr::copy_nonoverlapping(src, dst.add(i * plane), plane) };
                    }
                }
            }
            Storage::Encoded(..) => {}
        }
        Some(buf)
    }

    /// `copyWithZone:`: the same layout, properties and size in points,
    /// with pixels of its own (a file not yet decoded stays one, shared).
    fn duplicate(&self) -> Retained<NSBitmapImageRep> {
        let l = self.ivars().layout.borrow().clone();
        let encoded = match &*self.ivars().storage.borrow() {
            Storage::Encoded(file, upright) => Some(Storage::Encoded(file.clone(), *upright)),
            _ => None,
        };
        let storage = encoded.unwrap_or_else(|| Storage::Owned(self.pixels_copy().unwrap_or_default()));
        let ivars = BitmapIvars::new(l, self.ivars().space.get(), storage);
        *ivars.properties.borrow_mut() = self.ivars().properties.borrow().clone();
        ivars.compression.set(self.ivars().compression.get());
        *ivars.frames.borrow_mut() = self.ivars().frames.borrow().clone();
        crate::load_shell::<NSBitmapImageRep>();
        let this = NSBitmapImageRepImpl::alloc().set_ivars(ivars);
        // SAFETY: NSImageRep's designated initializer.
        let this: Retained<NSBitmapImageRepImpl> = unsafe { msg_send![super(this), init] };
        rep_ivars(&*this).copy_from(rep_ivars(self));
        // SAFETY: NSBitmapImageRepImpl is the class NSBitmapImageRep names.
        unsafe { Retained::cast_unchecked(this) }
    }

    /// The pixels as premultiplied RGBA rows, `width × 4` bytes apart.
    fn premultiplied_rgba(&self) -> Option<Vec<u8>> {
        self.decode();
        let l = self.ivars().layout.borrow().clone();
        let mut out = vec![0u8; l.width.checked_mul(l.height)?.checked_mul(4)?];
        if l.is_canvas() {
            let base = self.plane(0);
            if base.is_null() {
                return None;
            }
            for y in 0..l.height {
                // SAFETY: row `y` holds `width × 4` bytes at `y × bpr`.
                let row = unsafe { std::slice::from_raw_parts(base.add(y * l.bpr), l.width * 4) };
                out[y * l.width * 4..][..l.width * 4].copy_from_slice(row);
            }
            return Some(out);
        }
        // 8-bit samples four bytes a pixel, meshed, alpha (if any) last: a
        // decoded file's layout.
        if l.bps == 8
            && l.bpp == 32
            && !l.planar
            && matches!(l.spp, 3 | 4)
            && !l.format.contains(NSBitmapFormat::AlphaFirst)
        {
            let base = self.plane(0);
            if base.is_null() {
                return None;
            }
            let straight = l.format.contains(NSBitmapFormat::AlphaNonpremultiplied);
            for y in 0..l.height {
                // SAFETY: row `y` holds `width × 4` bytes at `y × bpr`.
                let row = unsafe { std::slice::from_raw_parts(base.add(y * l.bpr), l.width * 4) };
                let out = &mut out[y * l.width * 4..][..l.width * 4];
                for (d, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(row.as_chunks::<4>().0) {
                    let a = if l.alpha { s[3] } else { 255 };
                    let c = |v: u8| if straight { ((u32::from(v) * u32::from(a) + 127) / 255) as u8 } else { v.min(a) };
                    *d = [c(s[0]), c(s[1]), c(s[2]), a];
                }
            }
            return Some(out);
        }
        for y in 0..l.height {
            for x in 0..l.width {
                let p = self.read(x as NSInteger, y as NSInteger).unwrap_or([0.0; 4]);
                let at = (y * l.width + x) * 4;
                for (i, v) in p.iter().enumerate() {
                    out[at + i] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
        Some(out)
    }

    /// Width, height and straight (unpremultiplied) RGBA, for encoders.
    fn straight_rgba(&self) -> Option<(u32, u32, Vec<u8>)> {
        let l = self.ivars().layout.borrow().clone();
        let mut px = self.premultiplied_rgba()?;
        for p in px.as_chunks_mut::<4>().0 {
            let a = u32::from(p[3]);
            if a != 0 && a != 255 {
                for c in &mut p[..3] {
                    *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        Some((l.width as u32, l.height as u32, px))
    }
}

/// What the rasterizer draws of `rep`: a snapshot of its pixels, made again
/// only after they may have changed.
pub(crate) fn image_data(rep: &NSBitmapImageRep) -> Option<Arc<ImageData>> {
    let this = imp(rep);
    let generation = this.ivars().generation.get();
    if let Some(s) = this.ivars().snapshot.borrow().as_ref()
        && s.generation == generation
    {
        return Some(s.clone());
    }
    let l = this.ivars().layout.borrow().clone();
    let file = match &*this.ivars().storage.borrow() {
        Storage::Encoded(file, upright) => Some((file.clone(), *upright)),
        _ => None,
    };
    let pixels = match file {
        Some((file, upright)) => Pixels::Encoded(file, upright),
        None => Pixels::Rgba(Arc::from(this.premultiplied_rgba()?)),
    };
    let data = Arc::new(ImageData {
        key: this.ivars().key,
        generation,
        width: l.width as u32,
        height: l.height as u32,
        pixels,
    });
    *this.ivars().snapshot.borrow_mut() = Some(data.clone());
    Some(data)
}

/// The rep's size in pixels and points, if a context can draw into it.
pub(crate) fn drawable(rep: &NSBitmapImageRep) -> Option<((usize, usize), NSSize)> {
    let this = imp(rep);
    this.decode();
    let l = this.ivars().layout.borrow().clone();
    l.drawn_as()?;
    if this.plane(0).is_null() {
        return None;
    }
    let size = rep_ivars(this).size.get();
    Some(((l.width, l.height), size))
}

thread_local! {
    static GLYPHS: RefCell<crate::raster::Glyphs> = RefCell::default();
    /// The pixels of a rep not in a context's format, while they're drawn:
    /// as unpacked, and as drawn.
    static SCRATCH: RefCell<(Vec<u32>, Vec<u32>)> = RefCell::default();
}

/// Draw `ops` into the rep's pixels, on this thread.
pub(crate) fn rasterize(rep: &NSBitmapImageRep, ops: &[Op]) {
    let this = imp(rep);
    this.decode();
    let l = this.ivars().layout.borrow().clone();
    let Some(samples) = l.drawn_as() else { return };
    let base = this.plane(0);
    if base.is_null() {
        return;
    }
    let size = rep_ivars(this).size.get();
    let scale = if size.width > 0.0 { (l.width as f64 / size.width) as f32 } else { 1.0 };
    let damage = crate::protocol::Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
    let paint = |px: &mut [u32], stride: usize| {
        let mut canvas = crate::raster::Canvas::new(px, l.width as u32, l.height as u32, 0.0, scale);
        canvas.stride = stride;
        GLYPHS.with(|g| {
            let mut g = g.borrow_mut();
            crate::raster::paint(&mut canvas, &mut g, &[damage], ops);
        });
    };
    if l.is_canvas() && base.cast::<u32>().is_aligned() {
        // In place: the rep's own buffer, or the caller's when aligned.
        let stride = l.bpr / 4;
        let len = stride * (l.height - 1) + l.width;
        // SAFETY: plane 0 holds bytesPerRow × pixelsHigh bytes (the rep's
        // own buffer is that long; the caller's planes are, as
        // initWithBitmapDataPlanes: requires), which cover `len` u32s; it's
        // 4-byte aligned, as checked; and nothing else touches it while
        // the ops draw, this thread using the rep.
        let px = unsafe { std::slice::from_raw_parts_mut(base.cast::<u32>(), len) };
        paint(px, stride);
    } else {
        SCRATCH.with(|scratch| {
            let mut fresh = <(Vec<u32>, Vec<u32>)>::default();
            let mut held = scratch.try_borrow_mut();
            let (before, px) = match held.as_deref_mut() {
                Ok(s) => (&mut s.0, &mut s.1),
                Err(_) => (&mut fresh.0, &mut fresh.1),
            };
            before.clear();
            before.resize(l.width * l.height, 0);
            // SAFETY: plane 0 holds the layout's rows, as above.
            unsafe { unpack(&l, samples, base, before) };
            px.clear();
            px.extend_from_slice(before);
            paint(px, l.width);
            // SAFETY: as above.
            unsafe { pack(&l, samples, px, before, base) };
        });
    }
    this.bump();
}

/// Read a rep's pixels (`samples` in layout `l` at `base`) as premultiplied
/// RGBA canvas pixels, `l.width` a row.
///
/// # Safety
///
/// `base` points at `l.bpr × l.height` readable bytes.
unsafe fn unpack(l: &Layout, samples: Samples, base: *const u8, out: &mut [u32]) {
    let bytes = l.bpp / 8;
    for (y, row) in out.chunks_exact_mut(l.width).enumerate() {
        for (x, p) in row.iter_mut().enumerate() {
            // SAFETY: pixel (x, y) is inside the rows, as the caller
            // promises.
            let px = unsafe { base.add(y * l.bpr + x * bytes) };
            // SAFETY: sample `i` is inside the pixel.
            let get = |i: usize| unsafe { read_sample(px, samples.kind, i) };
            let a = samples.alpha.map_or(255, get);
            let c = |i: usize| get(samples.first + i).min(a);
            *p = if samples.gray {
                let g = c(0);
                u32::from_ne_bytes([g, g, g, a])
            } else {
                u32::from_ne_bytes([c(0), c(1), c(2), a])
            };
        }
    }
}

/// Write back the canvas pixels of `px` that differ from `before` into the
/// rep's pixels at `base`: the others keep their bits.
///
/// # Safety
///
/// `base` points at `l.bpr × l.height` writable bytes.
unsafe fn pack(l: &Layout, samples: Samples, px: &[u32], before: &[u32], base: *mut u8) {
    let bytes = l.bpp / 8;
    for (i, (&p, _)) in px.iter().zip(before).enumerate().filter(|(_, (p, b))| p != b) {
        let (x, y) = (i % l.width, i / l.width);
        let [r, g, b, a] = p.to_ne_bytes();
        // SAFETY: pixel (x, y) is inside the rows, as the caller promises.
        let out = unsafe { base.add(y * l.bpr + x * bytes) };
        // SAFETY: sample `k` is inside the pixel.
        let put = |k: usize, v: u8| unsafe { write_sample(out, samples.kind, k, v) };
        if samples.gray {
            put(samples.first, gray8([r, g, b, a]));
        } else {
            put(samples.first, r);
            put(samples.first + 1, g);
            put(samples.first + 2, b);
        }
        match samples.alpha {
            Some(k) => put(k, a),
            // RGB's fourth sample is padding, opaque as AppKit leaves it.
            None if !samples.gray => put(3, 255),
            None => {}
        }
    }
}

/// Sample `i` of the pixel at `px`, as 8 bits.
///
/// # Safety
///
/// The sample is inside readable memory.
unsafe fn read_sample(px: *const u8, kind: Sample, i: usize) -> u8 {
    // SAFETY: as the caller promises.
    unsafe {
        match kind {
            Sample::U8 => *px.add(i),
            Sample::U16 => ((u32::from(px.cast::<u16>().add(i).read_unaligned()) * 255 + 32767) / 65535) as u8,
            Sample::F32 => (px.cast::<f32>().add(i).read_unaligned().clamp(0.0, 1.0) * 255.0).round() as u8,
        }
    }
}

/// Store 8-bit `v` as sample `i` of the pixel at `px`.
///
/// # Safety
///
/// The sample is inside writable memory.
unsafe fn write_sample(px: *mut u8, kind: Sample, i: usize, v: u8) {
    // SAFETY: as the caller promises.
    unsafe {
        match kind {
            Sample::U8 => *px.add(i) = v,
            Sample::U16 => px.cast::<u16>().add(i).write_unaligned(u16::from(v) * 257),
            Sample::F32 => px.cast::<f32>().add(i).write_unaligned(f32::from(v) / 255.0),
        }
    }
}

/// A premultiplied canvas pixel's gray, premultiplied: its luminance in
/// linear light, encoded again (see `color::gray_of`).
fn gray8([r, g, b, a]: [u8; 4]) -> u8 {
    if r == g && g == b {
        return r;
    }
    if a == 0 {
        return 0;
    }
    let un = |c: u8| f64::from(c) / f64::from(a);
    let gray = crate::color::gray_of([un(r), un(g), un(b)]);
    (gray.clamp(0.0, 1.0) * f64::from(a)).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_up_to_32_bytes() {
        assert_eq!(default_bpr(4, 32), Some(32));
        assert_eq!(default_bpr(9, 32), Some(64));
        assert_eq!(default_bpr(100, 32), Some(416));
        assert_eq!(default_bpr(1000, 24), Some(3008));
        assert_eq!(default_bpr(33, 8), Some(64));
        assert_eq!(default_bpr(usize::MAX / 8, 32), None);
    }

    fn layout(width: usize, height: usize, bps: usize, spp: usize, format: NSBitmapFormat, bpp: usize) -> Layout {
        let alpha = spp == 2 || spp == 4;
        let bpr = default_bpr(width, bpp).unwrap();
        Layout { width, height, bps, spp, alpha, planar: false, format, bpr, bpp }
    }

    #[test]
    fn sizes_beyond_memory_are_refused() {
        let huge = Layout { bpr: 1 << 33, ..layout(1 << 31, 1 << 31, 8, 4, NSBitmapFormat::empty(), 32) };
        assert_eq!(huge.sizes(), None, "2^64 bytes wraps to 0 unchecked");
        assert_eq!(layout(3, 2, 8, 4, NSBitmapFormat::empty(), 32).sizes(), Some((64, 64)));
        // A file claiming 2^31 × 2^31 pixels makes no rep.
        crate::load_shell::<NSBitmapImageRep>();
        let tiff = include_bytes!("../../../conformance/tests/fixtures/huge.tiff");
        assert!(from_file_bytes(Arc::from(&tiff[..]), true).is_none());
        // Nor does such a size asked for.
        // SAFETY: NULL planes make the rep allocate; the name is a string.
        let rep: Option<Retained<NSBitmapImageRep>> = unsafe {
            msg_send![
                NSBitmapImageRep::alloc(),
                initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>(),
                pixelsWide: 1isize << 31,
                pixelsHigh: 1isize << 31,
                bitsPerSample: 8isize,
                samplesPerPixel: 4isize,
                hasAlpha: true,
                isPlanar: false,
                colorSpaceName: &*NSString::from_str("NSDeviceRGBColorSpace"),
                bytesPerRow: 0isize,
                bitsPerPixel: 0isize
            ]
        };
        assert!(rep.is_none());
    }

    #[test]
    fn contexts_draw_into_the_layouts_appkit_takes() {
        let drawn = |bps, spp, format, bpp| layout(4, 4, bps, spp, format, bpp).drawn_as().is_some();
        assert!(drawn(8, 4, NSBitmapFormat::empty(), 32));
        assert!(drawn(8, 1, NSBitmapFormat::empty(), 8), "gray");
        assert!(drawn(8, 2, NSBitmapFormat::empty(), 16), "gray and alpha");
        assert!(drawn(8, 3, NSBitmapFormat::empty(), 32), "RGBX");
        assert!(drawn(8, 4, NSBitmapFormat::AlphaFirst, 32), "ARGB");
        assert!(drawn(16, 4, NSBitmapFormat::empty(), 64), "16-bit");
        assert!(drawn(32, 4, NSBitmapFormat::FloatingPointSamples, 128), "float");
        assert!(!drawn(8, 3, NSBitmapFormat::empty(), 24), "24-bit RGB");
        assert!(!drawn(8, 4, NSBitmapFormat::AlphaNonpremultiplied, 32), "straight alpha");
        assert!(!drawn(16, 4, NSBitmapFormat::FloatingPointSamples, 64), "half floats");
        assert!(layout(0, 0, 8, 4, NSBitmapFormat::empty(), 32).drawn_as().is_none(), "no pixels");
        assert!(!Layout { bpr: 0, ..layout(0, 0, 8, 4, NSBitmapFormat::empty(), 32) }.is_canvas());
    }

    /// 200 small fills into a 1024 × 1024 bitmap on the program's own
    /// memory, and on the rep's, through a bitmap context.
    #[test]
    #[ignore = "a benchmark; run in release mode"]
    fn timing_fills_into_caller_planes() {
        crate::load_shell::<NSBitmapImageRep>();
        let mut mine = vec![0u32; 1024 * 1024];
        let mut planes = [mine.as_mut_ptr().cast::<u8>()];
        for (what, planes) in [("caller's planes", planes.as_mut_ptr()), ("the rep's own", std::ptr::null_mut())] {
            // SAFETY: the planes are null or 1024 rows of 4096 bytes.
            let rep: Retained<NSBitmapImageRep> = unsafe {
                msg_send![
                    NSBitmapImageRep::alloc(),
                    initWithBitmapDataPlanes: planes,
                    pixelsWide: 1024isize,
                    pixelsHigh: 1024isize,
                    bitsPerSample: 8isize,
                    samplesPerPixel: 4isize,
                    hasAlpha: true,
                    isPlanar: false,
                    colorSpaceName: &*NSString::from_str("NSDeviceRGBColorSpace"),
                    bytesPerRow: 4096isize,
                    bitsPerPixel: 32isize
                ]
            };
            let ctx = crate::context::bitmap_context(&rep).expect("a context");
            crate::context::begin_current(ctx);
            let ms = crate::backend::median(|| {
                for k in 0..200 {
                    let r = objc2_foundation::NSRect::new(
                        objc2_foundation::NSPoint::new((k % 20) as f64 * 50.0, (k / 20) as f64 * 100.0),
                        NSSize::new(40.0, 20.0),
                    );
                    crate::context::with_state(|st| {
                        st.fill_rect(r, [1.0, 0.0, 0.0, 1.0], crate::protocol::Blend::Copy)
                    });
                }
            });
            crate::context::end_current();
            println!("200 fills into a 1024 × 1024 bitmap on {what}: {ms:.2} ms");
        }
    }

    #[test]
    fn unpacking_and_packing_keep_what_isnt_drawn() {
        // 16-bit gray and alpha, 2 × 1: an odd value no 8-bit round trip
        // keeps, and a pixel drawn over.
        let l = layout(2, 1, 16, 2, NSBitmapFormat::empty(), 32);
        let samples = l.drawn_as().unwrap();
        let mut px: Vec<u16> = vec![0x1234, 0xffff, 0x0101, 0xffff];
        px.resize(l.bpr / 2, 0);
        let base = px.as_mut_ptr().cast::<u8>();
        let mut before = vec![0u32; 2];
        // SAFETY: `px` holds the layout's one row.
        unsafe { unpack(&l, samples, base, &mut before) };
        assert_eq!(before[0].to_ne_bytes(), [0x12, 0x12, 0x12, 0xff]);
        let mut after = before.clone();
        after[1] = u32::from_ne_bytes([255, 255, 255, 255]);
        // SAFETY: as above.
        unsafe { pack(&l, samples, &after, &before, base) };
        assert_eq!(px[..4], [0x1234, 0xffff, 0xffff, 0xffff]);
        // Gray is luminance in linear light: red is a middle gray.
        assert!((122..=132).contains(&gray8([255, 0, 0, 255])), "{}", gray8([255, 0, 0, 255]));
        assert_eq!(gray8([77, 77, 77, 128]), 77);
    }
}
