//! `NSTextContainer`: the region a layout manager fills with lines, a
//! rectangle here (exclusion paths are kept, not used), with padding at
//! each end of every line.
//!
//! A change of size, padding, line limit or line breaking tells the layout
//! manager (`textContainerChangedGeometry:`), which lays the text out
//! again. A container tracking its text view's width is resized by the
//! view (`text_view`).

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, define_class, msg_send};
use objc2_app_kit::{NSLayoutManager, NSLineBreakMode, NSTextContainer, NSTextView, NSWritingDirection};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSUInteger};

sidestep_runtime::static_class!(pub(crate) NSTEXTCONTAINER, NSTEXTCONTAINER_META = "NSTextContainer", || {
    let _ = NSTextContainerImpl::class();
});

pub(crate) struct Ivars {
    size: Cell<NSSize>,
    padding: Cell<f64>,
    max_lines: Cell<usize>,
    line_break: Cell<NSLineBreakMode>,
    width_tracks: Cell<bool>,
    height_tracks: Cell<bool>,
    manager: RefCell<Weak<NSLayoutManager>>,
    view: RefCell<Weak<NSTextView>>,
    exclusion: RefCell<Option<Retained<AnyObject>>>,
}

/// The default: as good as unbounded both ways, as AppKit's
/// `initWithSize:` callers usually ask for.
const HUGE: f64 = 10_000_000.0;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextContainer"]
    #[ivars = Ivars]
    pub(crate) struct NSTextContainerImpl;

    impl NSTextContainerImpl {
        #[unsafe(method_id(initWithSize:))]
        fn init_with_size(this: Allocated<Self>, size: NSSize) -> Retained<Self> {
            let this = this.set_ivars(Ivars {
                size: Cell::new(size),
                padding: Cell::new(5.0),
                max_lines: Cell::new(0),
                line_break: Cell::new(NSLineBreakMode::ByWordWrapping),
                width_tracks: Cell::new(false),
                height_tracks: Cell::new(false),
                manager: RefCell::new(Weak::default()),
                view: RefCell::new(Weak::default()),
                exclusion: RefCell::new(None),
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithContainerSize:))]
        fn init_with_container_size(this: Allocated<Self>, size: NSSize) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initWithSize: size] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initWithSize: NSSize::new(HUGE, HUGE)] }
        }

        #[unsafe(method(size))]
        fn size(&self) -> NSSize {
            self.ivars().size.get()
        }

        #[unsafe(method(setSize:))]
        fn set_size(&self, size: NSSize) {
            self.resize(size);
        }

        #[unsafe(method(containerSize))]
        fn container_size(&self) -> NSSize {
            self.ivars().size.get()
        }

        #[unsafe(method(setContainerSize:))]
        fn set_container_size(&self, size: NSSize) {
            self.resize(size);
        }

        #[unsafe(method(lineFragmentPadding))]
        fn line_fragment_padding(&self) -> f64 {
            self.ivars().padding.get()
        }

        #[unsafe(method(setLineFragmentPadding:))]
        fn set_line_fragment_padding(&self, padding: f64) {
            if self.ivars().padding.replace(padding) != padding {
                self.changed();
            }
        }

        #[unsafe(method(maximumNumberOfLines))]
        fn maximum_number_of_lines(&self) -> NSUInteger {
            self.ivars().max_lines.get()
        }

        #[unsafe(method(setMaximumNumberOfLines:))]
        fn set_maximum_number_of_lines(&self, lines: NSUInteger) {
            if self.ivars().max_lines.replace(lines) != lines {
                self.changed();
            }
        }

        #[unsafe(method(lineBreakMode))]
        fn line_break_mode(&self) -> NSLineBreakMode {
            self.ivars().line_break.get()
        }

        #[unsafe(method(setLineBreakMode:))]
        fn set_line_break_mode(&self, mode: NSLineBreakMode) {
            if self.ivars().line_break.replace(mode) != mode {
                self.changed();
            }
        }

        #[unsafe(method(widthTracksTextView))]
        fn width_tracks_text_view(&self) -> bool {
            self.ivars().width_tracks.get()
        }

        #[unsafe(method(setWidthTracksTextView:))]
        fn set_width_tracks_text_view(&self, flag: bool) {
            self.ivars().width_tracks.set(flag);
        }

        #[unsafe(method(heightTracksTextView))]
        fn height_tracks_text_view(&self) -> bool {
            self.ivars().height_tracks.get()
        }

        #[unsafe(method(setHeightTracksTextView:))]
        fn set_height_tracks_text_view(&self, flag: bool) {
            self.ivars().height_tracks.set(flag);
        }

        #[unsafe(method(isSimpleRectangularTextContainer))]
        fn is_simple_rectangular(&self) -> bool {
            self.ivars().exclusion.borrow().as_ref().is_none_or(|a| {
                // SAFETY: the exclusion paths are an array.
                let n: NSUInteger = unsafe { msg_send![&**a, count] };
                n == 0
            })
        }

        #[unsafe(method_id(exclusionPaths))]
        fn exclusion_paths(&self) -> Retained<AnyObject> {
            match self.ivars().exclusion.borrow().as_ref() {
                Some(a) => a.clone(),
                None => Retained::into_super(Retained::into_super(NSArray::<AnyObject>::new())),
            }
        }

        #[unsafe(method(setExclusionPaths:))]
        fn set_exclusion_paths(&self, paths: &AnyObject) {
            // SAFETY: -copy of an array is an immutable array.
            let copy: Retained<AnyObject> = unsafe { msg_send![paths, copy] };
            *self.ivars().exclusion.borrow_mut() = Some(copy);
        }

        #[unsafe(method(lineFragmentRectForProposedRect:atIndex:writingDirection:remainingRect:))]
        fn line_fragment_rect_for_proposed(
            &self,
            proposed: NSRect,
            _index: NSUInteger,
            _direction: NSWritingDirection,
            remaining: *mut NSRect,
        ) -> NSRect {
            if !remaining.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *remaining = NSRect::ZERO };
            }
            clip_to(self.ivars().size.get(), proposed)
        }

        #[unsafe(method(containsPoint:))]
        fn contains_point(&self, p: NSPoint) -> bool {
            let s = self.ivars().size.get();
            p.x >= 0.0 && p.y >= 0.0 && p.x < s.width && p.y < s.height
        }

        #[unsafe(method_id(layoutManager))]
        fn layout_manager(&self) -> Option<Retained<NSLayoutManager>> {
            self.ivars().manager.borrow().load()
        }

        #[unsafe(method(setLayoutManager:))]
        fn set_layout_manager(&self, manager: Option<&NSLayoutManager>) {
            *self.ivars().manager.borrow_mut() = manager.map_or_else(Weak::default, Weak::new);
        }

        #[unsafe(method(replaceLayoutManager:))]
        fn replace_layout_manager(&self, manager: &NSLayoutManager) {
            let old = self.ivars().manager.borrow().load();
            let this: &NSTextContainer = self.as_container();
            if let Some(old) = old {
                let containers = old.textContainers();
                if let Some(i) = containers.iter().position(|c| std::ptr::eq(&*c, this)) {
                    old.removeTextContainerAtIndex(i);
                }
            }
            manager.addTextContainer(this);
        }

        #[unsafe(method_id(textView))]
        fn text_view(&self) -> Option<Retained<NSTextView>> {
            self.ivars().view.borrow().load()
        }

        #[unsafe(method(setTextView:))]
        fn set_text_view(&self, view: Option<&NSTextView>) {
            *self.ivars().view.borrow_mut() = view.map_or_else(Weak::default, Weak::new);
            // Loaded first: the manager (a subclass's, maybe) is sent the
            // message with nothing borrowed.
            let manager = self.ivars().manager.borrow().load();
            if let Some(m) = manager {
                m.textContainerChangedTextView(self.as_container());
            }
        }

        #[unsafe(method_id(textLayoutManager))]
        fn text_layout_manager(&self) -> Option<Retained<AnyObject>> {
            None
        }
    }

    unsafe impl NSObjectProtocol for NSTextContainerImpl {}
);

impl NSTextContainerImpl {
    fn as_container(&self) -> &NSTextContainer {
        // SAFETY: NSTextContainer is this class.
        unsafe { &*(self as *const Self).cast::<NSTextContainer>() }
    }

    fn resize(&self, size: NSSize) {
        if self.ivars().size.replace(size) != size {
            self.changed();
        }
    }

    /// Tell the layout manager the geometry changed.
    fn changed(&self) {
        let manager = self.ivars().manager.borrow().load();
        if let Some(m) = manager {
            m.textContainerChangedGeometry(self.as_container());
        }
    }
}

fn clip_to(size: NSSize, r: NSRect) -> NSRect {
    let x0 = r.origin.x.max(0.0);
    let x1 = (r.origin.x + r.size.width).min(size.width);
    let y0 = r.origin.y.max(0.0);
    let y1 = (r.origin.y + r.size.height).min(size.height);
    if x1 <= x0 || y1 <= y0 {
        return NSRect::ZERO;
    }
    NSRect::new(NSPoint::new(x0, y0), NSSize::new(x1 - x0, y1 - y0))
}

/// What the layout manager reads of a container, read once per layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    pub size: NSSize,
    pub padding: f64,
    pub max_lines: usize,
    pub line_break: NSLineBreakMode,
}

impl Geometry {
    pub const DEFAULT: Geometry = Geometry {
        size: NSSize::new(HUGE, HUGE),
        padding: 5.0,
        max_lines: 0,
        line_break: NSLineBreakMode::ByWordWrapping,
    };
}

/// A container's geometry: read from the instance variables when the
/// container is Sidestep's own class or a subclass that keeps the
/// accessors, else through messages.
pub(crate) fn geometry(c: &NSTextContainer) -> Geometry {
    if let Some(iv) = own(c) {
        return Geometry {
            size: iv.size.get(),
            padding: iv.padding.get(),
            max_lines: iv.max_lines.get(),
            line_break: iv.line_break.get(),
        };
    }
    Geometry {
        size: c.size(),
        padding: c.lineFragmentPadding(),
        max_lines: c.maximumNumberOfLines(),
        line_break: c.lineBreakMode(),
    }
}

/// The container's instance variables, if its class is exactly this one.
pub(crate) fn own(c: &NSTextContainer) -> Option<&Ivars> {
    let class = c.class();
    // SAFETY: an instance of exactly this class.
    std::ptr::eq(class, NSTextContainerImpl::class())
        .then(|| unsafe { &*(c as *const NSTextContainer).cast::<NSTextContainerImpl>() }.ivars())
}
