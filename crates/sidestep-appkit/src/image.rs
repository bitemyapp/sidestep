//! `NSImage`: a picture of some size in points, drawn from whichever of its
//! representations suits where it's drawn.
//!
//! An image holds representations (`image_rep`, `bitmap`): bitmaps decoded
//! from files lazily, bitmaps made in memory, and drawing handlers. Its
//! size is its own: set (`initWithSize:`, `setSize:`), or else taken from
//! the first representation it's given, and kept when that representation
//! changes size, as AppKit keeps it. Setting it changes how large the
//! image draws, not its representations. A copy has copies of the
//! representations.
//!
//! Drawing picks a representation the way AppKit does: the smallest bitmap
//! whose pixels cover the destination in device pixels (a 1× and a 2×
//! bitmap: the 2× one on a 2× display), else the largest; a drawing
//! handler covers any size. `drawInRect:` and the `respectFlipped:` form
//! draw the image upright in a flipped context; the older
//! `drawInRect:fromRect:operation:fraction:` and `drawAtPoint:…` don't.
//! Template images draw their own colors here; views that tint them
//! (`NSImageView`, buttons) pass a tint to the representation.
//!
//! `setName:` registers an image under a name for `imageNamed:`, which
//! then looks for an image file of that name beside the program. The
//! registry is per thread: an image's state isn't shared between threads,
//! so each thread asking for a file by name gets its own image of it.
//! `lockFocus` makes a bitmap the image's only representation and draws
//! into it, keeping what the image showed.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBitmapImageRep, NSColor, NSCompositingOperation, NSCustomImageRep, NSGraphicsContext, NSImage, NSImageCacheMode,
    NSImageRep, NSImageResizingMode, NSImageSymbolConfiguration, NSTIFFCompression,
};
use objc2_foundation::{NSArray, NSCopying, NSDictionary, NSEdgeInsets, NSPoint, NSRect, NSSize, NSString, NSZone};

use crate::image_rep::{draw_rep_tinted, rep_imp};
use crate::protocol::{Blend, Color};

sidestep_runtime::static_class!(pub NSIMAGE, NSIMAGE_META = "NSImage", || {
    let _ = NSImageImpl::class();
});

// Hint keys, with AppKit's values.
sidestep_foundation::constant_string!(NSImageHintCTM = "NSImageHintCTM");
sidestep_foundation::constant_string!(NSImageHintInterpolation = "NSImageHintInterpolation");
sidestep_foundation::constant_string!(
    NSImageHintUserInterfaceLayoutDirection = "NSImageHintUserInterfaceLayoutDirection"
);

pub(crate) struct ImageIvars {
    /// Set by the program; otherwise the first representation's.
    size: Cell<Option<NSSize>>,
    reps: RefCell<Vec<Retained<NSImageRep>>>,
    name: RefCell<Option<Retained<NSString>>>,
    template: Cell<bool>,
    flipped: Cell<bool>,
    background: RefCell<Option<Retained<NSColor>>>,
    alignment: Cell<Option<NSRect>>,
    cap_insets: Cell<NSEdgeInsets>,
    resizing: Cell<NSImageResizingMode>,
    cache_mode: Cell<NSImageCacheMode>,
    description: RefCell<Option<Retained<NSString>>>,
    /// The bitmap `lockFocus` draws into, until `unlockFocus`.
    focus: RefCell<Option<Retained<NSBitmapImageRep>>>,
    /// What symbol a symbol image is (`symbols`).
    symbol: RefCell<Option<crate::symbols::Symbol>>,
}

impl Default for ImageIvars {
    fn default() -> Self {
        ImageIvars {
            size: Cell::new(None),
            reps: RefCell::new(Vec::new()),
            name: RefCell::new(None),
            template: Cell::new(false),
            flipped: Cell::new(false),
            background: RefCell::new(None),
            alignment: Cell::new(None),
            cap_insets: Cell::new(NSEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 }),
            resizing: Cell::new(NSImageResizingMode::Stretch),
            cache_mode: Cell::new(NSImageCacheMode::Default),
            description: RefCell::new(None),
            focus: RefCell::new(None),
            symbol: RefCell::new(None),
        }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; an image is used by
    // one thread at a time.
    #[unsafe(super(NSObject))]
    #[name = "NSImage"]
    #[ivars = ImageIvars]
    pub(crate) struct NSImageImpl;

    // Making images.
    impl NSImageImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ImageIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithSize:))]
        fn init_with_size(this: Allocated<Self>, size: NSSize) -> Retained<Self> {
            let this = this.set_ivars(ImageIvars { size: Cell::new(Some(size)), ..Default::default() });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithData:))]
        fn init_with_data(this: Allocated<Self>, data: &AnyObject) -> Option<Retained<Self>> {
            with_rep(this, rep_of_data(data, true))
        }

        #[unsafe(method_id(initWithDataIgnoringOrientation:))]
        fn init_with_data_ignoring_orientation(this: Allocated<Self>, data: &AnyObject) -> Option<Retained<Self>> {
            with_rep(this, rep_of_data(data, false))
        }

        #[unsafe(method_id(initWithContentsOfFile:))]
        fn init_with_contents_of_file(this: Allocated<Self>, path: &NSString) -> Option<Retained<Self>> {
            with_rep(this, rep_at(&path.to_string()))
        }

        #[unsafe(method_id(initWithContentsOfURL:))]
        fn init_with_contents_of_url(this: Allocated<Self>, url: &AnyObject) -> Option<Retained<Self>> {
            with_rep(this, crate::image_rep::url_path(url).and_then(|p| rep_at(&p.to_string())))
        }

        // By reference, an image exists even for a file that doesn't: it
        // just isn't valid.
        #[unsafe(method_id(initByReferencingFile:))]
        fn init_by_referencing_file(this: Allocated<Self>, path: &NSString) -> Option<Retained<Self>> {
            Some(referencing(this, rep_at(&path.to_string())))
        }

        #[unsafe(method_id(initByReferencingURL:))]
        fn init_by_referencing_url(this: Allocated<Self>, url: &AnyObject) -> Retained<Self> {
            let rep = crate::image_rep::url_path(url).and_then(|p| rep_at(&p.to_string()));
            referencing(this, rep)
        }

        #[unsafe(method_id(initWithPasteboard:))]
        fn init_with_pasteboard(_this: Allocated<Self>, _pasteboard: &AnyObject) -> Option<Retained<Self>> {
            // The pasteboard carries strings only, so far.
            None
        }

        #[unsafe(method(canInitWithPasteboard:))]
        fn can_init_with_pasteboard(_pasteboard: &AnyObject) -> bool {
            false
        }

        #[unsafe(method_id(imageWithSize:flipped:drawingHandler:))]
        fn with_handler(size: NSSize, flipped: bool, handler: &DynBlock<dyn Fn(NSRect) -> Bool>) -> Retained<NSImage> {
            let rep = NSCustomImageRep::initWithSize_flipped_drawingHandler(NSCustomImageRep::alloc(), size, flipped, handler);
            let image = new_image(Some(size));
            imp(&image).add_rep(Retained::into_super(rep));
            image
        }

        #[unsafe(method_id(imageNamed:))]
        fn image_named(name: &NSString) -> Option<Retained<NSImage>> {
            named(&name.to_string())
        }

        #[unsafe(method_id(imageWithSystemSymbolName:accessibilityDescription:))]
        fn with_system_symbol(name: &NSString, description: Option<&NSString>) -> Option<Retained<NSImage>> {
            crate::symbols::image(&name.to_string(), description, crate::symbols::empty_config())
        }

        // Variable values (how much of a symbol is filled in) aren't
        // drawn: the symbol is.
        #[unsafe(method_id(imageWithSystemSymbolName:variableValue:accessibilityDescription:))]
        fn with_system_symbol_value(name: &NSString, _value: f64, description: Option<&NSString>) -> Option<Retained<NSImage>> {
            crate::symbols::image(&name.to_string(), description, crate::symbols::empty_config())
        }

        // Symbols of the program's own, from its asset catalog, which
        // Linux programs don't have.
        #[unsafe(method_id(imageWithSymbolName:variableValue:))]
        fn with_symbol(_name: &NSString, _value: f64) -> Option<Retained<NSImage>> {
            None
        }

        #[unsafe(method_id(imageWithSymbolName:bundle:variableValue:))]
        fn with_symbol_in_bundle(_name: &NSString, _bundle: Option<&AnyObject>, _value: f64) -> Option<Retained<NSImage>> {
            None
        }

        #[unsafe(method_id(imageTypes))]
        fn image_types() -> Retained<NSArray<NSString>> {
            crate::image_rep::type_list()
        }

        #[unsafe(method_id(imageUnfilteredTypes))]
        fn image_unfiltered_types() -> Retained<NSArray<NSString>> {
            crate::image_rep::type_list()
        }
    }

    // What an image is.
    impl NSImageImpl {
        #[unsafe(method(size))]
        fn size(&self) -> NSSize {
            self.image_size()
        }

        #[unsafe(method(setSize:))]
        fn set_size(&self, size: NSSize) {
            self.ivars().size.set(Some(size));
        }

        #[unsafe(method(isValid))]
        fn is_valid(&self) -> bool {
            let size = self.image_size();
            !self.ivars().reps.borrow().is_empty() || (size.width > 0.0 && size.height > 0.0)
        }

        #[unsafe(method(setName:))]
        fn set_name(&self, name: Option<&NSString>) -> bool {
            set_name(self, name.map(|n| n.to_string()))
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Option<Retained<NSString>> {
            self.ivars().name.borrow().clone()
        }

        #[unsafe(method(isTemplate))]
        fn is_template(&self) -> bool {
            self.ivars().template.get()
        }

        #[unsafe(method(setTemplate:))]
        fn set_template(&self, flag: bool) {
            self.ivars().template.set(flag);
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            self.ivars().flipped.get()
        }

        #[unsafe(method(setFlipped:))]
        fn set_flipped(&self, flag: bool) {
            self.ivars().flipped.set(flag);
        }

        #[unsafe(method_id(backgroundColor))]
        fn background_color(&self) -> Retained<NSColor> {
            self.ivars().background.borrow().clone().unwrap_or_else(NSColor::clearColor)
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: &NSColor) {
            *self.ivars().background.borrow_mut() = Some(color.retain());
        }

        #[unsafe(method(alignmentRect))]
        fn alignment_rect(&self) -> NSRect {
            self.ivars().alignment.get().unwrap_or(NSRect::new(NSPoint::ZERO, self.image_size()))
        }

        #[unsafe(method(setAlignmentRect:))]
        fn set_alignment_rect(&self, rect: NSRect) {
            self.ivars().alignment.set(Some(rect));
        }

        #[unsafe(method(capInsets))]
        fn cap_insets(&self) -> NSEdgeInsets {
            self.ivars().cap_insets.get()
        }

        #[unsafe(method(setCapInsets:))]
        fn set_cap_insets(&self, insets: NSEdgeInsets) {
            self.ivars().cap_insets.set(insets);
        }

        #[unsafe(method(resizingMode))]
        fn resizing_mode(&self) -> NSImageResizingMode {
            self.ivars().resizing.get()
        }

        #[unsafe(method(setResizingMode:))]
        fn set_resizing_mode(&self, mode: NSImageResizingMode) {
            self.ivars().resizing.set(mode);
        }

        #[unsafe(method(cacheMode))]
        fn cache_mode(&self) -> NSImageCacheMode {
            self.ivars().cache_mode.get()
        }

        #[unsafe(method(setCacheMode:))]
        fn set_cache_mode(&self, mode: NSImageCacheMode) {
            self.ivars().cache_mode.set(mode);
        }

        #[unsafe(method_id(accessibilityDescription))]
        fn accessibility_description(&self) -> Option<Retained<NSString>> {
            self.ivars().description.borrow().clone()
        }

        #[unsafe(method(setAccessibilityDescription:))]
        fn set_accessibility_description(&self, description: Option<&NSString>) {
            *self.ivars().description.borrow_mut() = description.map(|d| NSString::from_str(&d.to_string()));
        }

        #[unsafe(method(recache))]
        fn recache(&self) {}

        #[unsafe(method_id(imageWithSymbolConfiguration:))]
        fn with_symbol_configuration(&self, config: &NSImageSymbolConfiguration) -> Option<Retained<NSImage>> {
            // Only symbols change; other images are what they are.
            let symbol = self.ivars().symbol.borrow().clone();
            match symbol {
                Some(symbol) => crate::symbols::reconfigured(&symbol, config),
                None => Some(self.as_image().retain()),
            }
        }

        #[unsafe(method_id(symbolConfiguration))]
        fn symbol_configuration(&self) -> Retained<NSImageSymbolConfiguration> {
            let symbol = self.ivars().symbol.borrow().clone();
            symbol.map_or_else(crate::symbols::empty_config, |s| s.config().clone())
        }

        #[unsafe(method(recommendedLayerContentsScale:))]
        fn recommended_layer_contents_scale(&self, preferred: f64) -> f64 {
            preferred
        }
    }

    // Representations.
    impl NSImageImpl {
        #[unsafe(method_id(representations))]
        fn representations(&self) -> Retained<NSArray<NSImageRep>> {
            NSArray::from_retained_slice(&self.reps())
        }

        #[unsafe(method(addRepresentation:))]
        fn add_representation(&self, rep: &NSImageRep) {
            self.add_rep(rep.retain());
        }

        #[unsafe(method(addRepresentations:))]
        fn add_representations(&self, reps: &NSArray<NSImageRep>) {
            for rep in reps.to_vec() {
                self.add_rep(rep);
            }
        }

        #[unsafe(method(removeRepresentation:))]
        fn remove_representation(&self, rep: &NSImageRep) {
            self.ivars().reps.borrow_mut().retain(|r| !std::ptr::eq(&**r, rep));
        }

        #[unsafe(method_id(bestRepresentationForRect:context:hints:))]
        fn best_representation(
            &self,
            rect: NSRect,
            context: Option<&NSGraphicsContext>,
            _hints: Option<&NSDictionary<NSString, AnyObject>>,
        ) -> Option<Retained<NSImageRep>> {
            let scale = context
                .and_then(|c| crate::context::with_state_of(c, |st| crate::image_rep::device_scale(st)))
                .unwrap_or(1.0);
            self.best_rep(rect.size, scale)
        }

        #[unsafe(method_id(TIFFRepresentation))]
        fn tiff_representation(&self) -> Option<Retained<AnyObject>> {
            self.tiff()
        }

        #[unsafe(method_id(TIFFRepresentationUsingCompression:factor:))]
        fn tiff_representation_using(&self, _comp: NSTIFFCompression, _factor: f32) -> Option<Retained<AnyObject>> {
            self.tiff()
        }
    }

    // Drawing.
    impl NSImageImpl {
        #[unsafe(method(drawInRect:))]
        fn draw_in_rect(&self, rect: NSRect) {
            self.draw(rect, NSRect::ZERO, Blend::SourceOver, 1.0, true, None);
        }

        #[unsafe(method(drawInRect:fromRect:operation:fraction:))]
        fn draw_in_rect_from(&self, rect: NSRect, from: NSRect, op: NSCompositingOperation, fraction: f64) {
            self.draw(rect, from, Blend::from_raw(op.0), fraction, false, None);
        }

        #[unsafe(method(drawInRect:fromRect:operation:fraction:respectFlipped:hints:))]
        fn draw_in_rect_hints(
            &self,
            rect: NSRect,
            from: NSRect,
            op: NSCompositingOperation,
            fraction: f64,
            respect: bool,
            _hints: Option<&NSDictionary<NSString, AnyObject>>,
        ) {
            self.draw(rect, from, Blend::from_raw(op.0), fraction, respect, None);
        }

        #[unsafe(method(drawAtPoint:fromRect:operation:fraction:))]
        fn draw_at_point(&self, point: NSPoint, from: NSRect, op: NSCompositingOperation, fraction: f64) {
            let size = if from.size.width > 0.0 && from.size.height > 0.0 { from.size } else { self.image_size() };
            self.draw(NSRect::new(point, size), from, Blend::from_raw(op.0), fraction, false, None);
        }

        #[unsafe(method(drawRepresentation:inRect:))]
        fn draw_representation(&self, rep: &NSImageRep, rect: NSRect) -> bool {
            crate::image_rep::draw_rep(rep, NSRect::ZERO, rect, Blend::SourceOver, 1.0, false)
        }

        #[unsafe(method(lockFocus))]
        fn lock_focus(&self) {
            self.focus(self.ivars().flipped.get());
        }

        #[unsafe(method(lockFocusFlipped:))]
        fn lock_focus_flipped(&self, flipped: bool) {
            self.focus(flipped);
        }

        #[unsafe(method(unlockFocus))]
        fn unlock_focus(&self) {
            let Some(rep) = self.ivars().focus.borrow_mut().take() else { return };
            crate::context::end_current();
            *self.ivars().reps.borrow_mut() = vec![Retained::into_super(rep)];
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSImage> {
            // The copy has copies of the representations, as AppKit's
            // does, and no name.
            let copy = new_image(self.ivars().size.get());
            let c = imp(&copy).ivars();
            let reps: Vec<Retained<NSImageRep>> = self.reps().iter().map(|r| r.copy()).collect();
            *c.reps.borrow_mut() = reps;
            c.template.set(self.ivars().template.get());
            c.flipped.set(self.ivars().flipped.get());
            c.alignment.set(self.ivars().alignment.get());
            c.cap_insets.set(self.ivars().cap_insets.get());
            c.resizing.set(self.ivars().resizing.get());
            *c.background.borrow_mut() = self.ivars().background.borrow().clone();
            *c.symbol.borrow_mut() = self.ivars().symbol.borrow().clone();
            *c.description.borrow_mut() = self.ivars().description.borrow().clone();
            copy
        }
    }

    unsafe impl NSObjectProtocol for NSImageImpl {}

    unsafe impl NSCopying for NSImageImpl {}
);

impl Drop for NSImageImpl {
    fn drop(&mut self) {
        // An image dropped between lockFocus and unlockFocus.
        if self.ivars().focus.borrow_mut().take().is_some() {
            crate::context::end_current();
        }
    }
}

/// Make `image` the symbol image `symbol`.
pub(crate) fn set_symbol(image: &NSImage, symbol: crate::symbols::Symbol) {
    *imp(image).ivars().symbol.borrow_mut() = Some(symbol);
}

pub(crate) fn imp(image: &NSImage) -> &NSImageImpl {
    // SAFETY: every NSImage is an NSImageImpl.
    unsafe { &*(image as *const NSImage).cast::<NSImageImpl>() }
}

fn new_image(size: Option<NSSize>) -> Retained<NSImage> {
    crate::load_shell::<NSImage>();
    let this = NSImageImpl::alloc().set_ivars(ImageIvars { size: Cell::new(size), ..Default::default() });
    // SAFETY: NSObject's designated initializer.
    let this: Retained<NSImageImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSImageImpl is the class NSImage names.
    unsafe { Retained::cast_unchecked(this) }
}

/// An image of one representation, its size the representation's; none
/// without one.
fn with_rep(this: Allocated<NSImageImpl>, rep: Option<Retained<NSBitmapImageRep>>) -> Option<Retained<NSImageImpl>> {
    rep.map(|rep| referencing(this, Some(rep)))
}

/// A bitmap of an image file's bytes, turned upright or not.
fn rep_of_data(data: &AnyObject, upright: bool) -> Option<Retained<NSBitmapImageRep>> {
    crate::bitmap::from_file_bytes(crate::image_rep::data_bytes(data)?, upright)
}

fn referencing(this: Allocated<NSImageImpl>, rep: Option<Retained<NSBitmapImageRep>>) -> Retained<NSImageImpl> {
    let this = this.set_ivars(ImageIvars::default());
    // SAFETY: NSObject's designated initializer.
    let this: Retained<NSImageImpl> = unsafe { msg_send![super(this), init] };
    if let Some(rep) = rep {
        this.add_rep(Retained::into_super(rep));
    }
    this
}

/// A bitmap of the image file at `path`, if it is one.
fn rep_at(path: &str) -> Option<Retained<NSBitmapImageRep>> {
    crate::bitmap::from_file_bytes(crate::image_rep::read_file(path)?, true)
}

impl NSImageImpl {
    fn as_image(&self) -> &NSImage {
        // SAFETY: NSImageImpl is the class NSImage names.
        unsafe { &*(self as *const Self).cast::<NSImage>() }
    }

    /// The representations, copied out: nothing stays borrowed while
    /// they're sent messages.
    fn reps(&self) -> Vec<Retained<NSImageRep>> {
        self.ivars().reps.borrow().clone()
    }

    /// Add a representation; an image with no size yet takes its size.
    fn add_rep(&self, rep: Retained<NSImageRep>) {
        if self.ivars().size.get().is_none() {
            self.ivars().size.set(Some(rep.size()));
        }
        self.ivars().reps.borrow_mut().push(rep);
    }

    fn image_size(&self) -> NSSize {
        self.ivars().size.get().unwrap_or(NSSize::ZERO)
    }

    /// The representation to draw `size` points at `scale` device pixels a
    /// point: the smallest whose pixels cover it, else the largest. A
    /// drawing handler covers any size.
    fn best_rep(&self, size: NSSize, scale: f64) -> Option<Retained<NSImageRep>> {
        let reps = self.reps();
        let need = |points: f64| points.abs() * scale - 0.01;
        let (nw, nh) = (need(size.width), need(size.height));
        let pixels = |r: &NSImageRep| -> (f64, f64) {
            if r.downcast_ref::<NSBitmapImageRep>().is_some() {
                let (w, h) = rep_imp(r).ivars().pixels.get();
                (w as f64, h as f64)
            } else {
                (f64::INFINITY, f64::INFINITY)
            }
        };
        let area = |r: &NSImageRep| {
            let (w, h) = pixels(r);
            w * h
        };
        let covering = reps.iter().filter(|r| {
            let (w, h) = pixels(r);
            w >= nw && h >= nh
        });
        let best = covering.min_by(|a, b| area(a).total_cmp(&area(b)));
        best.or_else(|| reps.iter().max_by(|a, b| area(a).total_cmp(&area(b)))).cloned()
    }

    /// Draw `from` (image points; empty for all of it) into `rect`.
    pub(crate) fn draw(
        &self,
        rect: NSRect,
        from: NSRect,
        blend: Blend,
        fraction: f64,
        respect: bool,
        tint: Option<Color>,
    ) {
        let size = self.image_size();
        if size.width <= 0.0 || size.height <= 0.0 {
            return;
        }
        let Some(scale) = crate::context::with_state(|st| crate::image_rep::device_scale(st)) else { return };
        let Some(rep) = self.best_rep(rect.size, scale) else { return };
        let from =
            if from.size.width > 0.0 && from.size.height > 0.0 { from } else { NSRect::new(NSPoint::ZERO, size) };
        // Image points to the representation's.
        let rs = rep.size();
        let (fx, fy) = (rs.width / size.width, rs.height / size.height);
        let src = NSRect::new(
            NSPoint::new(from.origin.x * fx, from.origin.y * fy),
            NSSize::new(from.size.width * fx, from.size.height * fy),
        );
        draw_rep_tinted(&rep, src, rect, blend, fraction, (respect, self.ivars().flipped.get()), tint);
    }

    /// The image as a TIFF file: its first bitmap, else its drawing at one
    /// pixel a point.
    fn tiff(&self) -> Option<Retained<AnyObject>> {
        let size = self.image_size();
        let rep = self.best_rep(size, 1.0)?;
        let bitmap = match rep.downcast::<NSBitmapImageRep>() {
            Ok(b) => b,
            Err(rep) => {
                let pixels = |v: f64| v.ceil().max(1.0) as usize;
                let b = crate::image_rep::new_bitmap(pixels(size.width), pixels(size.height), size)?;
                crate::image_rep::render_into(&b, false, || {
                    crate::image_rep::draw_rep(
                        &rep,
                        NSRect::ZERO,
                        NSRect::new(NSPoint::ZERO, size),
                        Blend::Copy,
                        1.0,
                        false,
                    );
                });
                b
            }
        };
        // SAFETY: TIFFRepresentation takes nothing and returns an NSData.
        unsafe { msg_send![&*bitmap, TIFFRepresentation] }
    }

    /// `lockFocus`: a bitmap of the image's size at the display's scale
    /// becomes where drawing goes, holding what the image showed.
    fn focus(&self, flipped: bool) {
        if self.ivars().focus.borrow().is_some() {
            return;
        }
        let size = self.image_size();
        let scale = display_scale();
        let pixels = |v: f64| (v * scale).ceil().max(1.0) as usize;
        let Some(rep) = crate::image_rep::new_bitmap(pixels(size.width), pixels(size.height), size) else { return };
        if !self.ivars().reps.borrow().is_empty() {
            crate::image_rep::render_into(&rep, false, || {
                self.draw(NSRect::new(NSPoint::ZERO, size), NSRect::ZERO, Blend::Copy, 1.0, false, None);
            });
        }
        let Some(ctx) = crate::context::bitmap_context(&rep) else { return };
        if flipped {
            crate::context::flip(&ctx);
        }
        crate::context::begin_current(ctx);
        *self.ivars().focus.borrow_mut() = Some(rep);
    }
}

/// Pixels per point on the display: the largest of the windows' backing
/// scales (1 with none).
fn display_scale() -> f64 {
    crate::app::windows_for_appearance()
        .iter()
        .map(|w| crate::window::backing_scale(crate::window::imp(w)))
        .fold(1.0, f64::max)
}

// Named images: `setName:` and `imageNamed:`.

thread_local! {
    /// This thread's registered images by name. Images aren't shared
    /// between threads (their state isn't synchronized), so neither is the
    /// registry: a name registered on one thread is found only there.
    static NAMED: RefCell<HashMap<String, Retained<NSImage>>> = RefCell::new(HashMap::new());
}

fn set_name(image: &NSImageImpl, name: Option<String>) -> bool {
    let this = image.as_image();
    let taken = NAMED.with(|named| {
        let mut named = named.borrow_mut();
        if let Some(name) = &name
            && named.get(name).is_some_and(|other| !std::ptr::eq(&**other, this))
        {
            return true;
        }
        // Drop the image's old registration.
        named.retain(|_, other| !std::ptr::eq(&**other, this));
        if let Some(name) = &name {
            named.insert(name.clone(), this.retain());
        }
        false
    });
    if !taken {
        *image.ivars().name.borrow_mut() = name.as_deref().map(NSString::from_str);
    }
    !taken
}

/// The image registered as `name` on this thread, else an image file of
/// that name found beside the program (registered under it from then on).
fn named(name: &str) -> Option<Retained<NSImage>> {
    if let Some(image) = NAMED.with(|named| named.borrow().get(name).cloned()) {
        return Some(image);
    }
    let file = resource(name)?;
    let rep = rep_at(&file.to_string_lossy())?;
    let image = new_image(None);
    imp(&image).add_rep(Retained::into_super(rep));
    set_name(imp(&image), Some(name.to_owned()));
    Some(image)
}

/// An image file called `name` (with or without an extension) in the
/// program's directory or a `Resources` directory beside or above it.
/// The directories are listed once, the first time a name is looked for:
/// looking a name up (or not finding it, every frame) touches no files.
fn resource(name: &str) -> Option<PathBuf> {
    // Each file's name, the first directory it's in, and its path.
    static FILES: OnceLock<HashMap<String, (usize, PathBuf)>> = OnceLock::new();
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let files = FILES.get_or_init(|| {
        let mut files = HashMap::new();
        let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)) else {
            return files;
        };
        for (at, dir) in [dir.clone(), dir.join("Resources"), dir.join("../Resources")].into_iter().enumerate() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                if entry.path().is_file()
                    && let Ok(file) = entry.file_name().into_string()
                {
                    files.entry(file).or_insert_with(|| (at, entry.path()));
                }
            }
        }
        files
    });
    // The first directory with one, then the first extension.
    let extensions = ["", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".tiff", ".tif", ".bmp", ".ico"];
    extensions.iter().filter_map(|e| files.get(&format!("{name}{e}"))).min_by_key(|(at, _)| *at).map(|(_, p)| p.clone())
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;

    use objc2::runtime::AnyObject;
    use objc2::{AnyThread, DefinedClass, define_class, msg_send};

    use super::*;

    // A stand-in for NSData, which Foundation doesn't have yet: images
    // read data through `length` and `bytes`.
    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "SidestepTestData"]
        #[ivars = Vec<u8>]
        struct Data;

        impl Data {
            #[unsafe(method(length))]
            fn length(&self) -> usize {
                self.ivars().len()
            }

            #[unsafe(method(bytes))]
            fn bytes(&self) -> *const c_void {
                self.ivars().as_ptr().cast()
            }
        }
    );

    fn data(bytes: &[u8]) -> Retained<Data> {
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(Data::alloc().set_ivars(bytes.to_vec())), init] }
    }

    fn image_with(data: &AnyObject, upright: bool) -> Option<Retained<NSImage>> {
        crate::load_shell::<NSImage>();
        // SAFETY: both initializers take data and return an image or nil.
        unsafe {
            if upright {
                msg_send![NSImage::alloc(), initWithData: data]
            } else {
                msg_send![NSImage::alloc(), initWithDataIgnoringOrientation: data]
            }
        }
    }

    #[test]
    fn data_is_turned_upright_unless_asked_not_to() {
        let jpeg = data(include_bytes!("../../../conformance/tests/fixtures/orientation-6.jpg"));
        assert_eq!(image_with(&jpeg, true).expect("a JPEG").size(), NSSize::new(2.0, 4.0));
        let sideways = image_with(&jpeg, false).expect("a JPEG");
        assert_eq!(sideways.size(), NSSize::new(4.0, 2.0));
        let rep = sideways.representations().objectAtIndex(0).downcast::<NSBitmapImageRep>().expect("a bitmap");
        assert_eq!((rep.samplesPerPixel(), rep.hasAlpha()), (3, false));
        assert!(image_with(&data(b"no image here"), true).is_none());
    }
}
