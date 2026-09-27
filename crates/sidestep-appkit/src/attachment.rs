//! Text attachments: `NSTextAttachment`, `NSTextAttachmentCell`, and
//! `+[NSAttributedString attributedStringWithAttachment:]`.
//!
//! An attachment is a U+FFFC with the attachment as its
//! `NSAttachmentAttributeName` value. Text layout (`text::layout`) lays each
//! one out as a box ([`metrics`]), and whoever draws the text draws the
//! attachment into the box ([`draw`]): string drawing, a layout manager
//! (through `-showAttachmentCell:inRect:characterIndex:`), TextKit 2's
//! fragments.
//!
//! What an attachment is laid out and drawn with, as on macOS (measured,
//! `conformance/tests/text_attachments.rs`):
//!
//! - An attachment with a cell (`attachmentCell`) is its cell's frame
//!   (`cellFrameForTextContainer:…`: the cell's baseline offset and size),
//!   at least a point wide, and draws through the cell. An attachment of
//!   nothing (no image, contents or file wrapper) has a cell of its own,
//!   of no size; one made with a file wrapper, a cell of the wrapper's
//!   image. An attachment with an image, or with contents, has none.
//! - Otherwise it's its bounds (`attachmentBoundsForTextContainer:…`): the
//!   bounds set, unless they are all zero, then the image's size (the
//!   image set, else one decoded from the contents or the file wrapper),
//!   else the 32-point square of a file's icon; and it draws its image
//!   (`imageForBounds:textContainer:characterIndex:`) into them, upright
//!   whichever way the view faces.
//!
//! Both are asked by message where a subclass overrides them, so
//! subclasses size and draw attachments their own way, and asked as AppKit
//! asks (measured): string drawing and TextKit 2 ask TextKit 2's methods
//! (`attachmentBoundsForAttributes:…`, `imageForBounds:attributes:…`) where
//! a subclass has them (before a cell, for the bounds), TextKit 1 its own.
//! They are told the text container (TextKit's; string drawing's given a
//! width, one of that width; none otherwise), a line fragment as wide as
//! the text's lines may be (AppKit's 40000 for string drawing given no
//! width) and as tall as the character's font's line, and the character's
//! index (or location). The glyph position is where the line starts (x 0,
//! and the baseline below the fragment's top, but for TextKit 1): text is
//! measured once for each set of attributes, before anything is placed,
//! where AppKit asks again with the pen's place. Text view attachments
//! (`NSTextAttachmentViewProvider`) aren't made: no attachment has a view
//! provider.
//!
//! An attachment may be measured on any thread (an attributed string drawn
//! off the main thread): its state is behind a lock, and the cell AppKit
//! makes for an attachment of nothing or of a file wrapper is made only
//! when asked for (`attachmentCell`) or to be shown by a layout manager on
//! the main thread; measured before that, it's its image's size.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{
    AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel,
};
use objc2_app_kit::{NSCell, NSCompositingOperation, NSImage, NSTextAttachment, NSTextAttachmentCell, NSView};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSCopying, NSData, NSDictionary, NSFileWrapper, NSPoint, NSRect, NSSize, NSString, NSUInteger, NSZone,
};

use crate::funnel::Funnel;
use crate::text::layout::Attachment;

sidestep_runtime::static_class!(pub NSTEXTATTACHMENT, NSTEXTATTACHMENT_META = "NSTextAttachment", || {
    let class = NSTextAttachmentImpl::class();
    ATTACHMENT_CELL.capture(class, sel!(attachmentCell));
    BOUNDS.capture(class, sel!(attachmentBoundsForTextContainer:proposedLineFragment:glyphPosition:characterIndex:));
    IMAGE.capture(class, sel!(imageForBounds:textContainer:characterIndex:));
    BOUNDS_TK2.capture(class, sel!(attachmentBoundsForAttributes:location:textContainer:proposedLineFragment:position:));
    IMAGE_TK2.capture(class, sel!(imageForBounds:attributes:location:textContainer:));
});

sidestep_runtime::static_class!(pub NSTEXTATTACHMENTCELL, NSTEXTATTACHMENTCELL_META = "NSTextAttachmentCell", || {
    let class = NSTextAttachmentCellImpl::class();
    CELL_FRAME.capture(class, sel!(cellFrameForTextContainer:proposedLineFragment:glyphPosition:characterIndex:));
});

static ATTACHMENT_CELL: Funnel = Funnel::new();
static BOUNDS: Funnel = Funnel::new();
static IMAGE: Funnel = Funnel::new();
static BOUNDS_TK2: Funnel = Funnel::new();
static IMAGE_TK2: Funnel = Funnel::new();
static CELL_FRAME: Funnel = Funnel::new();

/// The view provider classes registered by file type (class pointers).
static VIEW_PROVIDERS: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

/// The side of a file's icon, the image of an attachment of no image.
const ICON: f64 = 32.0;

/// What an attachment holds, behind its lock.
struct State {
    contents: Option<Retained<NSData>>,
    file_type: Option<Retained<NSString>>,
    image: Option<Retained<NSImage>>,
    bounds: CGRect,
    wrapper: Option<Retained<NSFileWrapper>>,
    /// The wrapper made of the contents or the image, when asked for.
    made_wrapper: Option<Retained<NSFileWrapper>>,
    /// The cell set, or made for a file wrapper or an attachment of
    /// nothing.
    cell: Option<Retained<AnyObject>>,
    /// The cell was made here, not set.
    own_cell: bool,
    /// Made with a file wrapper (`initWithFileWrapper:`), which gives the
    /// attachment a cell of the wrapper's image.
    legacy: bool,
    padding: f64,
    allows_view: bool,
    /// The image decoded from the contents or the wrapper, once.
    decoded: Option<Option<Retained<NSImage>>>,
}

pub(crate) struct AttachmentIvars {
    /// Locked only to read or change what's held: never while a message
    /// is sent (objects taken out are released after it's unlocked).
    state: Mutex<State>,
}

impl AttachmentIvars {
    fn new() -> AttachmentIvars {
        AttachmentIvars {
            state: Mutex::new(State {
                contents: None,
                file_type: None,
                image: None,
                bounds: CGRect::ZERO,
                wrapper: None,
                made_wrapper: None,
                cell: None,
                own_cell: false,
                legacy: false,
                padding: 0.0,
                allows_view: true,
                decoded: None,
            }),
        }
    }

    fn with(f: impl FnOnce(&mut State)) -> AttachmentIvars {
        let ivars = AttachmentIvars::new();
        f(&mut ivars.state.lock().unwrap_or_else(|e| e.into_inner()));
        ivars
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. An attachment may
    // be measured and drawn on several threads at once (an immutable
    // attributed string's): its state is behind a lock.
    #[unsafe(super(NSObject))]
    #[name = "NSTextAttachment"]
    #[ivars = AttachmentIvars]
    pub(crate) struct NSTextAttachmentImpl;

    impl NSTextAttachmentImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(AttachmentIvars::new());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithData:ofType:))]
        fn init_with_data(this: Allocated<Self>, contents: Option<&NSData>, kind: Option<&NSString>) -> Retained<Self> {
            let ivars = AttachmentIvars::with(|st| {
                st.contents = contents.map(|c| c.retain());
                st.file_type = kind.map(|k| NSString::from_str(&k.to_string()));
            });
            let this = this.set_ivars(ivars);
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithFileWrapper:))]
        fn init_with_file_wrapper(this: Allocated<Self>, wrapper: Option<&NSFileWrapper>) -> Retained<Self> {
            let file_type = wrapper.and_then(type_of_wrapper).map(NSString::from_str);
            let ivars = AttachmentIvars::with(|st| {
                st.file_type = file_type;
                st.wrapper = wrapper.map(|w| w.retain());
                st.legacy = wrapper.is_some();
            });
            let this = this.set_ivars(ivars);
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(contents))]
        fn contents(&self) -> Option<Retained<NSData>> {
            self.state().contents.clone()
        }

        #[unsafe(method(setContents:))]
        fn set_contents(&self, contents: Option<&NSData>) {
            let old = std::mem::replace(&mut self.state().contents, contents.map(|c| c.retain()));
            let derived = self.forget_derived();
            drop((old, derived));
        }

        #[unsafe(method_id(fileType))]
        fn file_type(&self) -> Option<Retained<NSString>> {
            self.state().file_type.clone()
        }

        #[unsafe(method(setFileType:))]
        fn set_file_type(&self, kind: Option<&NSString>) {
            let kind = kind.map(|k| NSString::from_str(&k.to_string()));
            let old = std::mem::replace(&mut self.state().file_type, kind);
            let derived = self.forget_derived();
            drop((old, derived));
        }

        #[unsafe(method_id(image))]
        fn image(&self) -> Option<Retained<NSImage>> {
            self.state().image.clone()
        }

        #[unsafe(method(setImage:))]
        fn set_image(&self, image: Option<&NSImage>) {
            let old = {
                let mut st = self.state();
                (std::mem::replace(&mut st.image, image.map(|i| i.retain())), st.made_wrapper.take())
            };
            drop(old);
        }

        #[unsafe(method(bounds))]
        fn bounds(&self) -> CGRect {
            self.state().bounds
        }

        #[unsafe(method(setBounds:))]
        fn set_bounds(&self, bounds: CGRect) {
            self.state().bounds = bounds;
        }

        #[unsafe(method_id(fileWrapper))]
        fn file_wrapper(&self) -> Option<Retained<NSFileWrapper>> {
            self.wrapper()
        }

        #[unsafe(method(setFileWrapper:))]
        fn set_file_wrapper(&self, wrapper: Option<&NSFileWrapper>) {
            let kind = wrapper.and_then(type_of_wrapper).map(NSString::from_str);
            let old = {
                let mut st = self.state();
                let old_type = if kind.is_some() { std::mem::replace(&mut st.file_type, kind) } else { None };
                (std::mem::replace(&mut st.wrapper, wrapper.map(|w| w.retain())), old_type)
            };
            let derived = self.forget_derived();
            drop((old, derived));
        }

        #[unsafe(method_id(attachmentCell))]
        fn attachment_cell(&self) -> Option<Retained<AnyObject>> {
            self.cell()
        }

        #[unsafe(method(setAttachmentCell:))]
        fn set_attachment_cell(&self, cell: Option<&AnyObject>) {
            let old = {
                let mut st = self.state();
                st.own_cell = false;
                std::mem::replace(&mut st.cell, cell.map(|c| c.retain()))
            };
            drop(old);
            if let Some(cell) = cell
                && cell.class().responds_to(sel!(setAttachment:))
            {
                // SAFETY: a cell's setAttachment: takes an attachment.
                let _: () = unsafe { msg_send![cell, setAttachment: self] };
            }
        }

        #[unsafe(method(lineLayoutPadding))]
        fn line_layout_padding(&self) -> f64 {
            self.state().padding
        }

        #[unsafe(method(setLineLayoutPadding:))]
        fn set_line_layout_padding(&self, padding: f64) {
            self.state().padding = padding;
        }

        #[unsafe(method(allowsTextAttachmentView))]
        fn allows_text_attachment_view(&self) -> bool {
            self.state().allows_view
        }

        #[unsafe(method(setAllowsTextAttachmentView:))]
        fn set_allows_text_attachment_view(&self, allows: bool) {
            self.state().allows_view = allows;
        }

        #[unsafe(method(usesTextAttachmentView))]
        fn uses_text_attachment_view(&self) -> bool {
            self.state().allows_view
        }

        #[unsafe(method(textAttachmentViewProviderClassForFileType:))]
        fn view_provider_class(kind: &NSString) -> *const AnyClass {
            let providers = VIEW_PROVIDERS.lock().unwrap_or_else(|e| e.into_inner());
            providers.as_ref().and_then(|p| p.get(&kind.to_string())).map_or(std::ptr::null(), |&c| c as *const AnyClass)
        }

        #[unsafe(method(registerTextAttachmentViewProviderClass:forFileType:))]
        fn register_view_provider_class(class: &AnyClass, kind: &NSString) {
            let mut providers = VIEW_PROVIDERS.lock().unwrap_or_else(|e| e.into_inner());
            providers.get_or_insert_default().insert(kind.to_string(), class as *const AnyClass as usize);
        }

        /// Attachments aren't archived here (there is no
        /// `encodeWithCoder:`), so they don't claim secure coding.
        #[unsafe(method(supportsSecureCoding))]
        fn supports_secure_coding() -> bool {
            false
        }

        // NSTextAttachmentContainer.

        #[unsafe(method_id(imageForBounds:textContainer:characterIndex:))]
        fn image_for_bounds(
            &self,
            _bounds: CGRect,
            _container: Option<&AnyObject>,
            _index: NSUInteger,
        ) -> Option<Retained<NSImage>> {
            self.shown_image()
        }

        #[unsafe(method(attachmentBoundsForTextContainer:proposedLineFragment:glyphPosition:characterIndex:))]
        fn attachment_bounds(
            &self,
            _container: Option<&AnyObject>,
            _fragment: CGRect,
            _position: CGPoint,
            _index: NSUInteger,
        ) -> CGRect {
            self.default_bounds()
        }

        // NSTextAttachmentLayout (TextKit 2).

        #[unsafe(method_id(imageForBounds:attributes:location:textContainer:))]
        fn image_for_bounds_tk2(
            &self,
            _bounds: CGRect,
            _attributes: &AnyObject,
            _location: &AnyObject,
            _container: Option<&AnyObject>,
        ) -> Option<Retained<NSImage>> {
            self.shown_image()
        }

        #[unsafe(method(attachmentBoundsForAttributes:location:textContainer:proposedLineFragment:position:))]
        fn attachment_bounds_tk2(
            &self,
            _attributes: &AnyObject,
            _location: &AnyObject,
            _container: Option<&AnyObject>,
            _fragment: CGRect,
            _position: CGPoint,
        ) -> CGRect {
            self.default_bounds()
        }

        #[unsafe(method_id(viewProviderForParentView:location:textContainer:))]
        fn view_provider(
            &self,
            _parent: Option<&AnyObject>,
            _location: &AnyObject,
            _container: Option<&AnyObject>,
        ) -> Option<Retained<AnyObject>> {
            None
        }
    }

    unsafe impl NSObjectProtocol for NSTextAttachmentImpl {}
);

/// What an attachment is shown as (see the module).
enum Shown {
    /// Its cell (set, made, or a subclass's).
    Cell(Retained<AnyObject>),
    /// The cell it would make for itself, not made yet: one of this image
    /// (a file wrapper's), or of none (an attachment of nothing).
    OwnCell(Option<Retained<NSImage>>),
    /// Its bounds and image.
    Bounds,
}

impl NSTextAttachmentImpl {
    fn state(&self) -> MutexGuard<'_, State> {
        self.ivars().state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drop what was worked out of the contents or the wrapper; handed
    /// back to be released after the lock.
    #[must_use]
    fn forget_derived(&self) -> impl Sized {
        let mut st = self.state();
        let cell = if st.own_cell {
            st.own_cell = false;
            st.cell.take()
        } else {
            None
        };
        (st.decoded.take(), st.made_wrapper.take(), cell)
    }

    /// Whether the attachment makes its own cell (see the module), and
    /// whether it's one of a file wrapper's image; `None` if it has a cell
    /// already or shows its bounds.
    fn own_cell_kind(st: &State) -> Option<bool> {
        if st.image.is_some() || (st.contents.is_some() && !st.legacy) || st.cell.is_some() {
            return None;
        }
        let empty = st.contents.is_none() && st.wrapper.is_none();
        (st.legacy || empty).then_some(st.legacy)
    }

    /// What it's shown as, without making a cell (a subclass's
    /// `attachmentCell` is asked).
    fn shown(&self) -> Shown {
        if ATTACHMENT_CELL.overridden(self.as_object(), sel!(attachmentCell)) {
            // SAFETY: attachmentCell returns a cell or nil.
            let cell: Option<Retained<AnyObject>> = unsafe { msg_send![self, attachmentCell] };
            return cell.map_or(Shown::Bounds, Shown::Cell);
        }
        let (cell, own) = {
            let st = self.state();
            if st.image.is_some() || (st.contents.is_some() && !st.legacy) {
                return Shown::Bounds;
            }
            (st.cell.clone(), Self::own_cell_kind(&st))
        };
        match (cell, own) {
            (Some(cell), _) => Shown::Cell(cell),
            (None, Some(legacy)) => Shown::OwnCell(if legacy { self.decoded_image().or_else(icon) } else { None }),
            (None, None) => Shown::Bounds,
        }
    }

    /// `attachmentCell`: none with an image or contents; the cell set;
    /// else one made for a file wrapper's image or, for an attachment of
    /// nothing, an empty one, kept (see the module). Made on the thread
    /// that asks for it.
    fn cell(&self) -> Option<Retained<AnyObject>> {
        let legacy = {
            let st = self.state();
            if st.image.is_some() || (st.contents.is_some() && !st.legacy) {
                return None;
            }
            if let Some(cell) = st.cell.clone() {
                return Some(cell);
            }
            Self::own_cell_kind(&st)?
        };
        let image = if legacy { self.decoded_image().or_else(icon) } else { None };
        crate::load_shell::<objc2_app_kit::NSTextAttachmentCell>();
        let cell: Retained<NSTextAttachmentCellImpl> = match image {
            // SAFETY: NSCell's initializers.
            Some(image) => unsafe { msg_send![NSTextAttachmentCellImpl::class_alloc(), initImageCell: &*image] },
            None => unsafe { msg_send![NSTextAttachmentCellImpl::class_alloc(), init] },
        };
        *cell.ivars().attachment.borrow_mut() = Some(Weak::from_retained(&self.retain()));
        // SAFETY: a cell is an object.
        let cell: Retained<AnyObject> = unsafe { Retained::cast_unchecked(cell) };
        let mut st = self.state();
        // Another thread may have made one meanwhile: that one stays.
        if let Some(made) = st.cell.clone() {
            return Some(made);
        }
        st.cell = Some(cell.clone());
        st.own_cell = true;
        Some(cell)
    }

    /// The image the contents or the file wrapper hold, decoded once.
    fn decoded_image(&self) -> Option<Retained<NSImage>> {
        let (contents, wrapper) = {
            let st = self.state();
            if let Some(known) = st.decoded.clone() {
                return known;
            }
            (st.contents.clone(), st.wrapper.clone())
        };
        let data =
            contents.or_else(|| wrapper.and_then(|w| w.isRegularFile().then(|| w.regularFileContents()).flatten()));
        let image = data.and_then(|d| NSImage::initWithData(NSImage::alloc(), &d)).filter(|i| {
            let s = i.size();
            s.width > 0.0 && s.height > 0.0
        });
        let mut st = self.state();
        st.decoded.get_or_insert(image).clone()
    }

    /// The image drawn: the image set, else the contents' or wrapper's,
    /// else a file's icon.
    fn shown_image(&self) -> Option<Retained<NSImage>> {
        let image = self.state().image.clone();
        image.or_else(|| self.decoded_image()).or_else(icon)
    }

    /// `attachmentBoundsForTextContainer:…`: the bounds set, unless all
    /// zero, else the image's size, else an icon's.
    fn default_bounds(&self) -> CGRect {
        let (b, image) = {
            let st = self.state();
            (st.bounds, st.image.clone())
        };
        if b != CGRect::ZERO {
            return b;
        }
        let size = image.or_else(|| self.decoded_image()).map(|i| i.size());
        CGRect::new(CGPoint::ZERO, size.unwrap_or(NSSize::new(ICON, ICON)))
    }

    /// `fileWrapper`: the wrapper set, else one made of the contents
    /// (`Attachment.<extension>`) or of the image as TIFF
    /// (`Attachment.tiff`), made once.
    fn wrapper(&self) -> Option<Retained<NSFileWrapper>> {
        let (contents, kind, image) = {
            let st = self.state();
            if let Some(w) = st.wrapper.clone().or_else(|| st.made_wrapper.clone()) {
                return Some(w);
            }
            (st.contents.clone(), st.file_type.clone(), st.image.clone())
        };
        let (data, name) = if let Some(contents) = contents {
            let kind = kind.as_ref().map(|t| t.to_string()).unwrap_or_default();
            let name = extension_of_type(&kind).map_or_else(|| "Attachment".to_owned(), |e| format!("Attachment.{e}"));
            (contents, name)
        } else {
            (image?.TIFFRepresentation()?, "Attachment.tiff".to_owned())
        };
        let wrapper = NSFileWrapper::initRegularFileWithContents(NSFileWrapper::alloc(), &data);
        wrapper.setPreferredFilename(Some(&NSString::from_str(&name)));
        let mut st = self.state();
        Some(st.made_wrapper.get_or_insert(wrapper).clone())
    }
}

/// A file's icon: the image of an attachment with nothing to show (a
/// document symbol, Sidestep's own drawing).
fn icon() -> Option<Retained<NSImage>> {
    let image = crate::symbols::image("doc", None, crate::symbols::empty_config())?;
    image.setSize(NSSize::new(ICON, ICON));
    Some(image)
}

/// The type identifier a file name's extension names, for the types
/// attachments know.
fn type_of_extension(ext: &str) -> Option<&'static str> {
    if let Some(kind) = crate::imageio::format::Kind::of_extension(ext) {
        return Some(kind.uti());
    }
    Some(match ext.to_ascii_lowercase().as_str() {
        "txt" | "text" => "public.plain-text",
        "rtf" => "public.rtf",
        "rtfd" => "com.apple.rtfd",
        "html" | "htm" => "public.html",
        "pdf" => "com.adobe.pdf",
        _ => return None,
    })
}

/// The extension a file of a type identifier takes.
fn extension_of_type(uti: &str) -> Option<&'static str> {
    if let Some(kind) = crate::imageio::format::Kind::of_uti(uti) {
        return Some(kind.extension());
    }
    Some(match uti {
        "public.plain-text" | "public.utf8-plain-text" | "public.text" => "txt",
        "public.rtf" => "rtf",
        "com.apple.rtfd" => "rtfd",
        "public.html" => "html",
        "com.adobe.pdf" => "pdf",
        _ => return None,
    })
}

/// The type of a wrapper's file, by its name's extension.
fn type_of_wrapper(wrapper: &NSFileWrapper) -> Option<&'static str> {
    let name = wrapper.preferredFilename().or_else(|| wrapper.filename())?.to_string();
    let (_, ext) = name.rsplit_once('.')?;
    type_of_extension(ext)
}

// The cell.

pub(crate) struct CellIvars {
    /// The attachment it belongs to (not kept: the attachment keeps it).
    attachment: RefCell<Option<Weak<NSTextAttachmentImpl>>>,
}

define_class!(
    #[unsafe(super(NSCell, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTextAttachmentCell"]
    #[ivars = CellIvars]
    pub(crate) struct NSTextAttachmentCellImpl;

    impl NSTextAttachmentCellImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(CellIvars { attachment: RefCell::new(None) });
            // SAFETY: NSCell's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initImageCell:))]
        fn init_image_cell(this: Allocated<Self>, image: Option<&AnyObject>) -> Retained<Self> {
            let this = this.set_ivars(CellIvars { attachment: RefCell::new(None) });
            // SAFETY: NSCell's initializer.
            unsafe { msg_send![super(this), initImageCell: image] }
        }

        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let this = this.set_ivars(CellIvars { attachment: RefCell::new(None) });
            // SAFETY: NSCell's initializer.
            unsafe { msg_send![super(this), initTextCell: string] }
        }

        /// Its image's size, or none.
        #[unsafe(method(cellSize))]
        fn cell_size(&self) -> NSSize {
            // SAFETY: NSCell's image getter.
            let image: Option<Retained<NSImage>> = unsafe { msg_send![self, image] };
            image.map_or(NSSize::ZERO, |i| i.size())
        }

        #[unsafe(method(cellBaselineOffset))]
        fn cell_baseline_offset(&self) -> NSPoint {
            NSPoint::ZERO
        }

        #[unsafe(method_id(attachment))]
        fn attachment(&self) -> Option<Retained<NSTextAttachmentImpl>> {
            self.ivars().attachment.borrow().as_ref().and_then(Weak::load)
        }

        #[unsafe(method(setAttachment:))]
        fn set_attachment(&self, attachment: Option<&AnyObject>) {
            let weak = attachment.and_then(attachment_of).map(|a| Weak::from_retained(&a.retain()));
            *self.ivars().attachment.borrow_mut() = weak;
        }

        /// Its image, drawn to fill the frame, upright.
        #[unsafe(method(drawWithFrame:inView:))]
        fn draw_with_frame(&self, frame: NSRect, _view: Option<&NSView>) {
            // SAFETY: NSCell's image getter.
            let image: Option<Retained<NSImage>> = unsafe { msg_send![self, image] };
            if let Some(image) = image {
                draw_image(&image, frame);
            }
        }

        #[unsafe(method(drawWithFrame:inView:characterIndex:))]
        fn draw_with_frame_index(&self, frame: NSRect, view: Option<&NSView>, _index: NSUInteger) {
            // SAFETY: drawWithFrame:inView: takes a rect and a view or nil.
            let _: () = unsafe { msg_send![self, drawWithFrame: frame, inView: view] };
        }

        #[unsafe(method(drawWithFrame:inView:characterIndex:layoutManager:))]
        fn draw_with_frame_manager(&self, frame: NSRect, view: Option<&NSView>, index: NSUInteger, _manager: &AnyObject) {
            // SAFETY: as above, with an index.
            let _: () = unsafe { msg_send![self, drawWithFrame: frame, inView: view, characterIndex: index] };
        }

        #[unsafe(method(highlight:withFrame:inView:))]
        fn highlight(&self, _flag: bool, _frame: NSRect, _view: Option<&NSView>) {}

        #[unsafe(method(wantsToTrackMouse))]
        fn wants_to_track_mouse(&self) -> bool {
            true
        }

        #[unsafe(method(wantsToTrackMouseForEvent:inRect:ofView:atCharacterIndex:))]
        fn wants_to_track_mouse_for_event(
            &self,
            _event: &AnyObject,
            _frame: NSRect,
            _view: Option<&NSView>,
            _index: NSUInteger,
        ) -> bool {
            // SAFETY: wantsToTrackMouse takes nothing.
            unsafe { msg_send![self, wantsToTrackMouse] }
        }

        /// Tracking ends at once: attachment cells do nothing with the
        /// mouse by themselves.
        #[unsafe(method(trackMouse:inRect:ofView:untilMouseUp:))]
        fn track_mouse(&self, _event: &AnyObject, _frame: NSRect, _view: Option<&NSView>, _until_up: bool) -> bool {
            false
        }

        #[unsafe(method(trackMouse:inRect:ofView:atCharacterIndex:untilMouseUp:))]
        fn track_mouse_at_index(
            &self,
            event: &AnyObject,
            frame: NSRect,
            view: Option<&NSView>,
            _index: NSUInteger,
            until_up: bool,
        ) -> bool {
            // SAFETY: as trackMouse:inRect:ofView:untilMouseUp: takes them.
            unsafe { msg_send![self, trackMouse: event, inRect: frame, ofView: view, untilMouseUp: until_up] }
        }

        /// Its baseline offset and size.
        #[unsafe(method(cellFrameForTextContainer:proposedLineFragment:glyphPosition:characterIndex:))]
        fn cell_frame(&self, _container: Option<&AnyObject>, _fragment: NSRect, _position: NSPoint, _index: NSUInteger) -> NSRect {
            frame_of(self)
        }

        #[unsafe(method_id(copyWithZone:))]
        /// NSCell's copy, knowing the same attachment (as on macOS).
        fn copy_with_zone(&self, zone: *mut NSZone) -> Retained<NSCell> {
            // SAFETY: NSCell's copyWithZone: returns a cell of the
            // receiver's class.
            let copy: Retained<NSCell> = unsafe { msg_send![super(self), copyWithZone: zone] };
            if let Some(theirs) = copy.downcast_ref::<NSTextAttachmentCellImpl>() {
                *theirs.ivars().attachment.borrow_mut() = self.ivars().attachment.borrow().clone();
            }
            copy
        }
    }
);

impl NSTextAttachmentCellImpl {
    /// An uninitialized instance, for the attachment to make its cell.
    fn class_alloc() -> Allocated<Self> {
        // SAFETY: +alloc on the class. A cell belongs to the main thread;
        // an attachment makes its own only when a program asks for it
        // (`attachmentCell`, on the thread it asks on, as AppKit would) or
        // to be shown by a layout manager on the main thread.
        unsafe { msg_send![Self::class(), alloc] }
    }
}

/// A cell's frame from its baseline offset and size, by message.
fn frame_of(cell: &AnyObject) -> NSRect {
    // SAFETY: an attachment cell answers cellSize and cellBaselineOffset.
    let (size, offset): (NSSize, NSPoint) = unsafe { (msg_send![cell, cellSize], msg_send![cell, cellBaselineOffset]) };
    NSRect::new(offset, size)
}

/// Draw `image` filling `rect`, upright whichever way the context faces.
fn draw_image(image: &NSImage, rect: NSRect) {
    // SAFETY: NSImage's drawing method, with no hints.
    let _: () = unsafe {
        msg_send![
            image,
            drawInRect: rect,
            fromRect: NSRect::ZERO,
            operation: NSCompositingOperation::SourceOver,
            fraction: 1.0f64,
            respectFlipped: true,
            hints: std::ptr::null::<NSDictionary<NSString, AnyObject>>()
        ]
    };
}

/// The attachment an attribute's value is, if it's one.
fn attachment_of(value: &AnyObject) -> Option<&NSTextAttachmentImpl> {
    // Asked through AppKit's class, which loads the class's shell first.
    let attachment = value.downcast_ref::<NSTextAttachment>()?;
    // SAFETY: every NSTextAttachment is an NSTextAttachmentImpl.
    Some(unsafe { &*(attachment as *const NSTextAttachment).cast::<NSTextAttachmentImpl>() })
}

impl NSTextAttachmentImpl {
    fn as_object(&self) -> &AnyObject {
        // SAFETY: an object.
        unsafe { &*(self as *const Self).cast::<AnyObject>() }
    }
}

/// Which of AppKit's layouts lays attachments out: each asks them its own
/// things (see the module).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Engine {
    Drawing,
    TextKit1,
    TextKit2,
}

/// Where text with attachments is laid out or drawn: what the methods a
/// subclass overrides are told (see the module).
#[derive(Clone)]
pub(crate) struct Setting {
    pub engine: Engine,
    /// The text container, if the text has one.
    pub container: Option<Retained<AnyObject>>,
    /// How wide the proposed line fragments are.
    pub width: f64,
    /// String drawing given a width: a container of that width is made
    /// for the methods that take one.
    pub sized: bool,
}

/// The width of the line fragments string drawing proposes when given no
/// width (measured).
const UNBOUNDED_WIDTH: f64 = 40000.0;

impl Setting {
    /// String drawing, given a width or not.
    pub fn drawing(width: Option<f64>) -> Setting {
        let width = width.filter(|w| w.is_finite() && *w > 0.0);
        Setting {
            engine: Engine::Drawing,
            container: None,
            width: width.unwrap_or(UNBOUNDED_WIDTH),
            sized: width.is_some(),
        }
    }

    /// TextKit's layout in `container`, its lines `width` wide.
    pub fn textkit(engine: Engine, container: Option<Retained<AnyObject>>, width: f64) -> Setting {
        let width = if width.is_finite() { width } else { UNBOUNDED_WIDTH };
        Setting { engine, container, width, sized: false }
    }

    /// The container to tell a method: TextKit's, or one made for string
    /// drawing given a width.
    fn container(&self) -> Option<Retained<AnyObject>> {
        if !self.sized || self.container.is_some() {
            return self.container.clone();
        }
        crate::load_shell::<objc2_app_kit::NSTextContainer>();
        let class = objc2_app_kit::NSTextContainer::class();
        // SAFETY: NSTextContainer's +alloc and initializer; a container
        // is plain data.
        Some(unsafe {
            let this: Allocated<AnyObject> = msg_send![class, alloc];
            msg_send![this, initWithSize: NSSize::new(self.width, 1e7)]
        })
    }
}

thread_local! {
    /// The setting text is being laid out in on this thread.
    static SETTING: RefCell<Option<Setting>> = const { RefCell::new(None) };
    /// The index of the character whose attributes are being worked out.
    static INDEX: Cell<Option<usize>> = const { Cell::new(None) };
    /// A subclass's method was sent a made-up index (see `noting_guesses`).
    static GUESSED: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` (which works out attributes, through `string_drawing::attrs_of`)
/// with attachments laid out in `setting`.
pub(crate) fn in_setting<R>(setting: Setting, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Setting>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let old = self.0.take();
            SETTING.with(|s| *s.borrow_mut() = old);
        }
    }
    let _restore = Restore(SETTING.with(|s| s.borrow_mut().replace(setting)));
    f()
}

/// Run `f` for the attributes of the character at `index` (the first with
/// them), which is what the methods of a subclass are told.
pub(crate) fn at_index<R>(index: usize, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<usize>);
    impl Drop for Restore {
        fn drop(&mut self) {
            INDEX.set(self.0);
        }
    }
    let _restore = Restore(INDEX.replace(Some(index)));
    f()
}

/// Run `f`, saying whether a subclass's method was sent an attachment's
/// character index made up (0: `f` wasn't run in `at_index`), so that
/// what it answered can be asked again when the index is known.
pub(crate) fn noting_guesses<R>(f: impl FnOnce() -> R) -> (R, bool) {
    let outer = GUESSED.replace(false);
    let r = f();
    let guessed = GUESSED.get();
    GUESSED.set(outer || guessed);
    (r, guessed)
}

fn setting() -> Setting {
    SETTING.with(|s| s.borrow().clone()).unwrap_or_else(|| Setting::drawing(None))
}

/// The character's index for a subclass's method.
fn index_told() -> usize {
    INDEX.get().unwrap_or_else(|| {
        GUESSED.set(true);
        0
    })
}

/// A text location for TextKit 2's methods.
fn location_told(index: usize) -> Retained<AnyObject> {
    crate::textkit2::location::location(index)
}

type Attributes = NSDictionary<NSString, AnyObject>;

/// The box of a cell's frame: at least a point wide, as on macOS.
fn cell_box(frame: NSRect) -> Attachment {
    let width = if frame.size.width > 0.0 { frame.size.width } else { 1.0 };
    Attachment { width: width as f32, height: frame.size.height.max(0.0) as f32, y: frame.origin.y as f32 }
}

fn bounds_box(b: CGRect) -> Attachment {
    Attachment { width: b.size.width.max(0.0) as f32, height: b.size.height.max(0.0) as f32, y: b.origin.y as f32 }
}

/// The box text lays an `NSAttachmentAttributeName` value out as, if it's
/// an attachment (see the module), in the current setting; `attributes`
/// are the character's, and `ascent` and `line` its font's ascent and line
/// height (whole points), for what a subclass is told.
pub(crate) fn metrics(
    value: &AnyObject,
    attributes: Option<&Attributes>,
    ascent: f64,
    line: f64,
) -> Option<Attachment> {
    let attachment = attachment_of(value)?;
    let setting = setting();
    let fragment = CGRect::new(CGPoint::ZERO, CGSize::new(setting.width, line));
    let position = if setting.engine == Engine::TextKit1 { CGPoint::ZERO } else { CGPoint::new(0.0, ascent) };
    let tk2 = sel!(attachmentBoundsForAttributes:location:textContainer:proposedLineFragment:position:);
    if setting.engine != Engine::TextKit1 && BOUNDS_TK2.overridden(attachment.as_object(), tk2) {
        let location = location_told(index_told());
        let container = setting.container();
        let empty;
        let attributes = match attributes {
            Some(a) => a,
            None => {
                empty = NSDictionary::new();
                &empty
            }
        };
        // SAFETY: the layout protocol's method: attributes, a location, a
        // container or nil, a rect and a point.
        let b: CGRect = unsafe {
            msg_send![
                attachment,
                attachmentBoundsForAttributes: attributes,
                location: &*location,
                textContainer: container.as_deref(),
                proposedLineFragment: fragment,
                position: position
            ]
        };
        return Some(bounds_box(b));
    }
    match attachment.shown() {
        Shown::Cell(cell) => {
            let sel = sel!(cellFrameForTextContainer:proposedLineFragment:glyphPosition:characterIndex:);
            let ours = cell.downcast_ref::<NSTextAttachmentCell>().is_some();
            let overridden = !ours || CELL_FRAME.overridden(&cell, sel);
            let frame: NSRect = if overridden && cell.class().responds_to(sel) {
                let index = index_told();
                let container = setting.container();
                // SAFETY: the cell protocol's method: a container (nil, as
                // AppKit's string drawing sends without one), a rect, a
                // point and an index.
                unsafe {
                    msg_send![
                        &*cell,
                        cellFrameForTextContainer: container.as_deref(),
                        proposedLineFragment: fragment,
                        glyphPosition: position,
                        characterIndex: index
                    ]
                }
            } else {
                frame_of(&cell)
            };
            Some(cell_box(frame))
        }
        // The cell it would make: its image's size, and no offset.
        Shown::OwnCell(image) => Some(cell_box(NSRect::new(NSPoint::ZERO, image.map_or(NSSize::ZERO, |i| i.size())))),
        Shown::Bounds => {
            let sel = sel!(attachmentBoundsForTextContainer:proposedLineFragment:glyphPosition:characterIndex:);
            let b: CGRect = if BOUNDS.overridden(attachment.as_object(), sel) {
                let index = index_told();
                let container = setting.container();
                // SAFETY: the container protocol's method: a container or
                // nil, a rect, a point and an index.
                unsafe {
                    msg_send![
                        attachment,
                        attachmentBoundsForTextContainer: container.as_deref(),
                        proposedLineFragment: fragment,
                        glyphPosition: position,
                        characterIndex: index
                    ]
                }
            } else {
                attachment.default_bounds()
            };
            Some(bounds_box(b))
        }
    }
}

/// An attachment's place when it's drawn: its box in the current user
/// space, its character's index and attributes, and the view drawn.
pub(crate) struct Drawn<'a> {
    pub rect: NSRect,
    pub index: usize,
    pub attributes: Option<&'a Attributes>,
    pub view: Option<&'a NSView>,
}

/// Draw the attachment `value` (an `NSAttachmentAttributeName` value) at
/// `at`, laid out in `setting`: through `manager`'s
/// `showAttachmentCell:inRect:characterIndex:` for a layout manager's.
pub(crate) fn draw(value: &AnyObject, at: &Drawn<'_>, setting: &Setting, manager: Option<&AnyObject>) {
    let Some(attachment) = attachment_of(value) else { return };
    let cell = match attachment.shown() {
        Shown::Cell(cell) => Some(cell),
        // A layout manager is shown the cell, made for it on the main
        // thread (cells belong there); elsewhere it's drawn as it would
        // draw itself.
        Shown::OwnCell(image) => match manager.filter(|_| MainThreadMarker::new().is_some()) {
            Some(_) => attachment.cell(),
            None => {
                if let Some(image) = image {
                    draw_image(&image, at.rect);
                }
                return;
            }
        },
        Shown::Bounds => None,
    };
    if let Some(cell) = cell {
        match manager {
            Some(manager) => {
                // SAFETY: the layout manager's method: a cell, a rect and an
                // index.
                let _: () = unsafe {
                    msg_send![manager, showAttachmentCell: &*cell, inRect: at.rect, characterIndex: at.index]
                };
            }
            None => {
                let sel = sel!(drawWithFrame:inView:characterIndex:);
                if cell.class().responds_to(sel) {
                    // SAFETY: the cell protocol's method.
                    let _: () =
                        unsafe { msg_send![&*cell, drawWithFrame: at.rect, inView: at.view, characterIndex: at.index] };
                } else {
                    // SAFETY: the cell protocol's method.
                    let _: () = unsafe { msg_send![&*cell, drawWithFrame: at.rect, inView: at.view] };
                }
            }
        }
        return;
    }
    let object = attachment.as_object();
    let tk2 = sel!(imageForBounds:attributes:location:textContainer:);
    let image: Option<Retained<NSImage>> = if setting.engine != Engine::TextKit1 && IMAGE_TK2.overridden(object, tk2) {
        let location = location_told(at.index);
        let container = setting.container();
        let empty;
        let attributes = match at.attributes {
            Some(a) => a,
            None => {
                empty = NSDictionary::new();
                &empty
            }
        };
        // SAFETY: the layout protocol's method: a rect, attributes, a
        // location and a container or nil.
        unsafe {
            msg_send![
                attachment,
                imageForBounds: at.rect,
                attributes: attributes,
                location: &*location,
                textContainer: container.as_deref()
            ]
        }
    } else if IMAGE.overridden(object, sel!(imageForBounds:textContainer:characterIndex:)) {
        let container = setting.container();
        // SAFETY: the container protocol's method: a rect, a container or
        // nil and an index.
        unsafe {
            msg_send![attachment, imageForBounds: at.rect, textContainer: container.as_deref(), characterIndex: at.index]
        }
    } else {
        attachment.shown_image()
    };
    if let Some(image) = image {
        draw_image(&image, at.rect);
    }
}

/// `-[NSLayoutManager showAttachmentCell:inRect:characterIndex:]`: the
/// cell draws itself, told its text view and the layout manager.
pub(crate) fn show_cell(manager: &AnyObject, cell: &AnyObject, rect: NSRect, index: usize, view: Option<&NSView>) {
    let sel = sel!(drawWithFrame:inView:characterIndex:layoutManager:);
    if cell.class().responds_to(sel) {
        // SAFETY: the cell protocol's method.
        let _: () = unsafe {
            msg_send![cell, drawWithFrame: rect, inView: view, characterIndex: index, layoutManager: manager]
        };
    } else {
        // SAFETY: the cell protocol's method.
        let _: () = unsafe { msg_send![cell, drawWithFrame: rect, inView: view] };
    }
}

// +[NSAttributedString attributedStringWithAttachment:] and its kin.

extern "C-unwind" fn with_attachment(class: &AnyClass, _sel: Sel, attachment: &AnyObject) -> *mut AnyObject {
    make_with_attachment(class, attachment, None)
}

extern "C-unwind" fn with_attachment_attributes(
    class: &AnyClass,
    _sel: Sel,
    attachment: &AnyObject,
    attributes: Option<&NSDictionary<NSString, AnyObject>>,
) -> *mut AnyObject {
    make_with_attachment(class, attachment, attributes)
}

/// A new, autoreleased attributed string of `class`: a U+FFFC with the
/// attachment (and `attributes`).
fn make_with_attachment(
    class: &AnyClass,
    attachment: &AnyObject,
    attributes: Option<&NSDictionary<NSString, AnyObject>>,
) -> *mut AnyObject {
    // SAFETY: the key is a constant this crate exports.
    let key = unsafe { objc2_app_kit::NSAttachmentAttributeName };
    let mut keys: Vec<Retained<NSString>> = Vec::new();
    let mut values: Vec<Retained<AnyObject>> = Vec::new();
    if let Some(a) = attributes {
        let (k, v) = a.to_vecs();
        keys.extend(k);
        values.extend(v);
    }
    match keys.iter().position(|k| **k == *key) {
        Some(i) => values[i] = attachment.retain(),
        None => {
            keys.push(key.copy());
            values.push(attachment.retain());
        }
    }
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let dict = NSDictionary::from_retained_objects(&keys, &values);
    let text = NSString::from_str("\u{FFFC}");
    // SAFETY: +alloc on an attributed string class, then its initializer.
    let string: Retained<AnyObject> = unsafe {
        let this: Allocated<AnyObject> = msg_send![class, alloc];
        msg_send![this, initWithString: &*text, attributes: &*dict]
    };
    Retained::autorelease_return(string)
}

/// `-[NSMutableAttributedString updateAttachmentsFromPath:]`: the
/// attachments keep what they hold (their files aren't read again).
extern "C-unwind" fn update_attachments_from_path(_this: &AnyObject, _sel: Sel, _path: &NSString) {}

sidestep_runtime::category!("NSAttributedString"(NSAttributedStringAttachmentConveniences), |category| {
    // SAFETY: class methods of an attributed string class, with the
    // encodings their signatures give.
    unsafe {
        category.add_class_method(
            sel!(attributedStringWithAttachment:),
            with_attachment as extern "C-unwind" fn(_, _, _) -> _,
        );
        category.add_class_method(
            sel!(attributedStringWithAttachment:attributes:),
            with_attachment_attributes as extern "C-unwind" fn(_, _, _, _) -> _,
        );
    }
});

sidestep_runtime::category!("NSMutableAttributedString"(NSMutableAttributedStringAttachmentConveniences), |category| {
    // SAFETY: an instance method of a mutable attributed string.
    unsafe {
        category.add_method(
            sel!(updateAttachmentsFromPath:),
            update_attachments_from_path as extern "C-unwind" fn(_, _, _),
        );
    }
});
