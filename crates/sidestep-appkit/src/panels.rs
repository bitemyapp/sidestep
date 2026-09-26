//! `NSSavePanel` and `NSOpenPanel`: the desktop's file chooser, through
//! xdg-desktop-portal (see `portal`).
//!
//! They are real panels, so programs may treat them as windows, but they
//! never get a surface of their own: the portal shows the dialog. The
//! defaults are macOS's (`conformance/tests/panels.rs`): a save panel is
//! titled Save with a Save prompt, names the file Untitled, starts in the
//! Documents folder and may create folders; an open panel is titled Open,
//! chooses one file, and has no URL until a choice.
//!
//! `runModal` asks the portal and runs a modal loop for the panel
//! (`runModalForWindow:`, so the panel is `NSApp.modalWindow` and the
//! program's other windows take no input, as on macOS; timers and drawing
//! go on) until the answer comes: `NSModalResponseOK` with `URL` or `URLs`
//! set, `NSModalResponseCancel`, or `NSModalResponseAbort` when there is no
//! portal (and no `zenity` or `kdialog` to fall back on).
//! `beginWithCompletionHandler:` and
//! `beginSheetModalForWindow:completionHandler:` ask the same way and call
//! the handler when the answer comes, without a loop of their own. While
//! the desktop's dialog is up the panel is visible, though it never shows
//! a surface of its own (ordering it front does nothing). `cancel:` gives
//! the dialog up and answers Cancel at once, as a click on the panel's
//! Cancel does on macOS; `ok:` answers OK for a save panel, with the name
//! and folder it has (an open panel has no selection of its own to take,
//! so there it does nothing). Allowed file types become the dialog's
//! filter: extensions as `*.ext`, and the common type identifiers as their
//! extensions.

// `allowedFileTypes` is deprecated, but programs still set it.
#![allow(deprecated)]

use std::cell::{Cell, RefCell};

use block2::DynBlock;
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSModalResponse, NSModalResponseAbort, NSModalResponseCancel, NSModalResponseOK,
    NSOpenPanel, NSPanel, NSResponder, NSSavePanel, NSView, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSCopying, NSPoint, NSRect, NSSize, NSString, NSURL};

use crate::portal::{self, Choose, Chosen};

sidestep_runtime::static_class!(pub NSSAVEPANEL, NSSAVEPANEL_META = "NSSavePanel", || {
    let _ = NSSavePanelImpl::class();
});

sidestep_runtime::static_class!(pub NSOPENPANEL, NSOPENPANEL_META = "NSOpenPanel", || {
    let _ = NSOpenPanelImpl::class();
});

pub(crate) struct SaveIvars {
    prompt: RefCell<Retained<NSString>>,
    name_label: RefCell<Retained<NSString>>,
    name: RefCell<Retained<NSString>>,
    message: RefCell<Retained<NSString>>,
    directory: RefCell<Option<Retained<NSURL>>>,
    /// The file chosen, for a save panel.
    chosen: RefCell<Option<Retained<NSURL>>>,
    allowed_types: RefCell<Option<Retained<NSArray<NSString>>>>,
    content_types: RefCell<Option<Retained<AnyObject>>>,
    current_type: RefCell<Option<Retained<AnyObject>>>,
    tag_names: RefCell<Option<Retained<AnyObject>>>,
    required_type: RefCell<Option<Retained<NSString>>>,
    others: Cell<bool>,
    create_directories: Cell<bool>,
    extension_hidden: Cell<bool>,
    can_hide_extension: Cell<bool>,
    packages: Cell<bool>,
    hidden_files: Cell<bool>,
    tag_field: Cell<bool>,
    content_types_shown: Cell<bool>,
    delegate: RefCell<Weak<AnyObject>>,
    accessory: RefCell<Option<Retained<NSView>>>,
    identifier: RefCell<Option<Retained<NSString>>>,
    /// The desktop's dialog asking for this panel (its request).
    pending: Cell<Option<u64>>,
}

impl SaveIvars {
    fn new() -> Self {
        SaveIvars {
            prompt: RefCell::new(NSString::from_str("Save")),
            name_label: RefCell::new(NSString::from_str("Save As:")),
            name: RefCell::new(NSString::from_str("Untitled")),
            message: RefCell::new(NSString::new()),
            directory: RefCell::new(None),
            chosen: RefCell::new(None),
            allowed_types: RefCell::new(None),
            content_types: RefCell::new(None),
            current_type: RefCell::new(None),
            tag_names: RefCell::new(None),
            required_type: RefCell::new(None),
            others: Cell::new(false),
            create_directories: Cell::new(true),
            extension_hidden: Cell::new(true),
            can_hide_extension: Cell::new(false),
            packages: Cell::new(false),
            hidden_files: Cell::new(false),
            tag_field: Cell::new(true),
            content_types_shown: Cell::new(false),
            delegate: RefCell::new(Weak::default()),
            accessory: RefCell::new(None),
            identifier: RefCell::new(None),
            pending: Cell::new(None),
        }
    }
}

define_class!(
    #[unsafe(super(NSPanel, NSWindow, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSavePanel"]
    #[ivars = SaveIvars]
    pub(crate) struct NSSavePanelImpl;

    impl NSSavePanelImpl {
        #[unsafe(method_id(initWithContentRect:styleMask:backing:defer:))]
        fn init_with_content_rect(
            this: Allocated<Self>,
            rect: NSRect,
            style: NSWindowStyleMask,
            backing: NSBackingStoreType,
            defer: bool,
        ) -> Retained<Self> {
            let this = this.set_ivars(SaveIvars::new());
            // SAFETY: NSPanel's designated initializer.
            let this: Retained<Self> =
                unsafe { msg_send![super(this), initWithContentRect: rect, styleMask: style, backing: backing, defer: defer] };
            let window: &NSWindow = &this;
            window.setTitle(&NSString::from_str("Save"));
            window.setHidesOnDeactivate(false);
            this
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let frame = NSRect::new(NSPoint::ZERO, NSSize::new(600.0, 400.0));
            // SAFETY: the designated initializer.
            unsafe {
                msg_send![
                    this,
                    initWithContentRect: frame,
                    styleMask: NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable,
                    backing: NSBackingStoreType::Buffered,
                    defer: true
                ]
            }
        }

        #[unsafe(method_id(savePanel))]
        fn save_panel() -> Retained<NSSavePanel> {
            let mtm = MainThreadMarker::new().expect("sidestep: panels belong to the main thread");
            NSSavePanel::new(mtm)
        }

        /// The folder and the name, once there is a folder.
        #[unsafe(method_id(URL))]
        fn url(&self) -> Option<Retained<NSURL>> {
            save_url(self)
        }

        #[unsafe(method_id(identifier))]
        fn identifier(&self) -> Option<Retained<NSString>> {
            self.ivars().identifier.borrow().clone()
        }

        #[unsafe(method(setIdentifier:))]
        fn set_identifier(&self, identifier: Option<&NSString>) {
            let old = self.ivars().identifier.replace(identifier.map(|i| i.copy()));
            drop(old);
        }

        #[unsafe(method_id(directoryURL))]
        fn directory_url(&self) -> Option<Retained<NSURL>> {
            directory(self)
        }

        #[unsafe(method(setDirectoryURL:))]
        fn set_directory_url(&self, url: Option<&NSURL>) {
            let old = self.ivars().directory.replace(url.map(|u| u.retain()));
            drop(old);
        }

        #[unsafe(method_id(allowedContentTypes))]
        fn allowed_content_types(&self) -> Retained<AnyObject> {
            let set = self.ivars().content_types.borrow().clone();
            set.unwrap_or_else(|| Retained::into_super(Retained::into_super(NSArray::<AnyObject>::new())))
        }

        #[unsafe(method(setAllowedContentTypes:))]
        fn set_allowed_content_types(&self, types: &AnyObject) {
            let old = self.ivars().content_types.replace(Some(types.retain()));
            drop(old);
        }

        #[unsafe(method(allowsOtherFileTypes))]
        fn allows_other_file_types(&self) -> bool {
            self.ivars().others.get()
        }

        #[unsafe(method(setAllowsOtherFileTypes:))]
        fn set_allows_other_file_types(&self, flag: bool) {
            self.ivars().others.set(flag);
        }

        #[unsafe(method_id(currentContentType))]
        fn current_content_type(&self) -> Option<Retained<AnyObject>> {
            self.ivars().current_type.borrow().clone()
        }

        #[unsafe(method(setCurrentContentType:))]
        fn set_current_content_type(&self, kind: Option<&AnyObject>) {
            let old = self.ivars().current_type.replace(kind.map(|k| k.retain()));
            drop(old);
        }

        #[unsafe(method_id(accessoryView))]
        fn accessory_view(&self) -> Option<Retained<NSView>> {
            self.ivars().accessory.borrow().clone()
        }

        #[unsafe(method(setAccessoryView:))]
        fn set_accessory_view(&self, view: Option<&NSView>) {
            let old = self.ivars().accessory.replace(view.map(|v| v.retain()));
            drop(old);
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().load()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            let old = self.ivars().delegate.replace(delegate.map_or_else(Weak::default, Weak::new));
            drop(old);
        }

        /// The portal's dialog decides.
        #[unsafe(method(isExpanded))]
        fn is_expanded(&self) -> bool {
            false
        }

        #[unsafe(method(canCreateDirectories))]
        fn can_create_directories(&self) -> bool {
            self.ivars().create_directories.get()
        }

        #[unsafe(method(setCanCreateDirectories:))]
        fn set_can_create_directories(&self, flag: bool) {
            self.ivars().create_directories.set(flag);
        }
        #[unsafe(method(canSelectHiddenExtension))]
        fn can_select_hidden_extension(&self) -> bool {
            self.ivars().can_hide_extension.get()
        }

        #[unsafe(method(setCanSelectHiddenExtension:))]
        fn set_can_select_hidden_extension(&self, flag: bool) {
            self.ivars().can_hide_extension.set(flag);
        }
        #[unsafe(method(isExtensionHidden))]
        fn is_extension_hidden(&self) -> bool {
            self.ivars().extension_hidden.get()
        }

        #[unsafe(method(setExtensionHidden:))]
        fn set_extension_hidden(&self, flag: bool) {
            self.ivars().extension_hidden.set(flag);
        }
        #[unsafe(method(treatsFilePackagesAsDirectories))]
        fn treats_file_packages_as_directories(&self) -> bool {
            self.ivars().packages.get()
        }

        #[unsafe(method(setTreatsFilePackagesAsDirectories:))]
        fn set_treats_file_packages_as_directories(&self, flag: bool) {
            self.ivars().packages.set(flag);
        }
        #[unsafe(method_id(prompt))]
        fn prompt(&self) -> Retained<NSString> {
            self.ivars().prompt.borrow().clone()
        }

        #[unsafe(method(setPrompt:))]
        fn set_prompt(&self, value: Option<&NSString>) {
            let old = self.ivars().prompt.replace(value.map_or_else(NSString::new, |v| v.copy()));
            drop(old);
        }
        #[unsafe(method_id(nameFieldLabel))]
        fn name_field_label(&self) -> Retained<NSString> {
            self.ivars().name_label.borrow().clone()
        }

        #[unsafe(method(setNameFieldLabel:))]
        fn set_name_field_label(&self, value: Option<&NSString>) {
            let old = self.ivars().name_label.replace(value.map_or_else(NSString::new, |v| v.copy()));
            drop(old);
        }
        #[unsafe(method_id(nameFieldStringValue))]
        fn name_field_string_value(&self) -> Retained<NSString> {
            self.ivars().name.borrow().clone()
        }

        #[unsafe(method(setNameFieldStringValue:))]
        fn set_name_field_string_value(&self, value: Option<&NSString>) {
            let old = self.ivars().name.replace(value.map_or_else(NSString::new, |v| v.copy()));
            drop(old);
        }
        #[unsafe(method_id(message))]
        fn message(&self) -> Retained<NSString> {
            self.ivars().message.borrow().clone()
        }

        #[unsafe(method(setMessage:))]
        fn set_message(&self, value: Option<&NSString>) {
            let old = self.ivars().message.replace(value.map_or_else(NSString::new, |v| v.copy()));
            drop(old);
        }
        #[unsafe(method(showsHiddenFiles))]
        fn shows_hidden_files(&self) -> bool {
            self.ivars().hidden_files.get()
        }

        #[unsafe(method(setShowsHiddenFiles:))]
        fn set_shows_hidden_files(&self, flag: bool) {
            self.ivars().hidden_files.set(flag);
        }
        #[unsafe(method(showsTagField))]
        fn shows_tag_field(&self) -> bool {
            self.ivars().tag_field.get()
        }

        #[unsafe(method(setShowsTagField:))]
        fn set_shows_tag_field(&self, flag: bool) {
            self.ivars().tag_field.set(flag);
        }
        #[unsafe(method(showsContentTypes))]
        fn shows_content_types(&self) -> bool {
            self.ivars().content_types_shown.get()
        }

        #[unsafe(method(setShowsContentTypes:))]
        fn set_shows_content_types(&self, flag: bool) {
            self.ivars().content_types_shown.set(flag);
        }

        #[unsafe(method_id(tagNames))]
        fn tag_names(&self) -> Option<Retained<AnyObject>> {
            self.ivars().tag_names.borrow().clone()
        }

        #[unsafe(method(setTagNames:))]
        fn set_tag_names(&self, names: Option<&AnyObject>) {
            let old = self.ivars().tag_names.replace(names.map(|n| n.retain()));
            drop(old);
        }

        #[unsafe(method(validateVisibleColumns))]
        fn validate_visible_columns(&self) {}

        /// A save panel's OK, with the name and folder it has; an open
        /// panel has no selection of its own to take.
        #[unsafe(method(ok:))]
        fn ok(&self, _sender: Option<&AnyObject>) {
            if open_ivars(self).is_none() {
                give_up(self, Chosen::Uris(Vec::new()));
            }
        }

        /// Close the desktop's dialog and answer Cancel.
        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: Option<&AnyObject>) {
            give_up(self, Chosen::Cancelled);
        }

        /// While the desktop's dialog asks for it.
        #[unsafe(method(isVisible))]
        fn is_visible(&self) -> bool {
            self.ivars().pending.get().is_some()
        }

        // The desktop shows the dialog: the panel never shows itself.

        #[unsafe(method(orderFront:))]
        fn order_front(&self, _sender: Option<&AnyObject>) {}

        #[unsafe(method(makeKeyAndOrderFront:))]
        fn make_key_and_order_front(&self, _sender: Option<&AnyObject>) {}

        #[unsafe(method(orderFrontRegardless))]
        fn order_front_regardless(&self) {}

        #[unsafe(method(orderWindow:relativeTo:))]
        fn order_window(&self, _place: NSWindowOrderingMode, _other: isize) {}

        #[unsafe(method(runModal))]
        fn run_modal(&self) -> NSModalResponse {
            run_modal(as_panel(self))
        }

        #[unsafe(method(beginWithCompletionHandler:))]
        fn begin_with_completion_handler(&self, handler: &DynBlock<dyn Fn(NSModalResponse)>) {
            begin(as_panel(self), handler);
        }

        /// As `beginWithCompletionHandler:`: the portal's dialog stands
        /// over the window by itself.
        #[unsafe(method(beginSheetModalForWindow:completionHandler:))]
        fn begin_sheet_modal_for_window(&self, _window: &NSWindow, handler: &DynBlock<dyn Fn(NSModalResponse)>) {
            begin(as_panel(self), handler);
        }

        // What old versions of AppKit had.

        #[unsafe(method_id(filename))]
        fn filename(&self) -> Retained<NSString> {
            save_url(self).and_then(|u| u.path()).unwrap_or_default()
        }

        #[unsafe(method_id(directory))]
        fn directory_path(&self) -> Retained<NSString> {
            directory(self).and_then(|u| u.path()).unwrap_or_default()
        }

        #[unsafe(method(setDirectory:))]
        fn set_directory(&self, path: Option<&NSString>) {
            let url = path.map(|p| NSURL::fileURLWithPath_isDirectory(p, true));
            let old = self.ivars().directory.replace(url);
            drop(old);
        }

        #[unsafe(method_id(requiredFileType))]
        fn required_file_type(&self) -> Option<Retained<NSString>> {
            self.ivars().required_type.borrow().clone()
        }

        #[unsafe(method(setRequiredFileType:))]
        fn set_required_file_type(&self, kind: Option<&NSString>) {
            let old = self.ivars().required_type.replace(kind.map(|k| k.copy()));
            drop(old);
        }

        #[unsafe(method(runModalForDirectory:file:))]
        fn run_modal_for_directory(&self, directory: Option<&NSString>, file: Option<&NSString>) -> NSModalResponse {
            self.set_start(directory, file);
            run_modal(as_panel(self))
        }

        #[unsafe(method(selectText:))]
        fn select_text(&self, _sender: Option<&AnyObject>) {}

        #[unsafe(method_id(allowedFileTypes))]
        fn allowed_file_types(&self) -> Option<Retained<NSArray<NSString>>> {
            self.ivars().allowed_types.borrow().clone()
        }

        #[unsafe(method(setAllowedFileTypes:))]
        fn set_allowed_file_types(&self, types: Option<&NSArray<NSString>>) {
            let old = self.ivars().allowed_types.replace(types.map(|t| t.retain()));
            drop(old);
        }
    }
);

impl NSSavePanelImpl {
    /// The start folder and name the old `…ForDirectory:file:` methods take.
    fn set_start(&self, directory: Option<&NSString>, file: Option<&NSString>) {
        if let Some(dir) = directory {
            let old = self.ivars().directory.replace(Some(NSURL::fileURLWithPath_isDirectory(dir, true)));
            drop(old);
        }
        if let Some(file) = file {
            let old = self.ivars().name.replace(file.copy());
            drop(old);
        }
    }
}

fn save_ivars<T>(panel: &T) -> &SaveIvars {
    // SAFETY: the macros are used only in NSSavePanelImpl, whose instances
    // (and its subclasses') start with its ivars.
    unsafe { &*(panel as *const T).cast::<NSSavePanelImpl>() }.ivars()
}

fn as_panel(panel: &NSSavePanelImpl) -> &NSSavePanel {
    // SAFETY: NSSavePanelImpl is the class NSSavePanel names.
    unsafe { &*(panel as *const NSSavePanelImpl).cast::<NSSavePanel>() }
}

fn imp(panel: &NSSavePanel) -> &NSSavePanelImpl {
    // SAFETY: every NSSavePanel is an NSSavePanelImpl.
    unsafe { &*(panel as *const NSSavePanel).cast::<NSSavePanelImpl>() }
}

/// The folder set, else the Documents folder (the home folder without
/// one).
fn directory(panel: &NSSavePanelImpl) -> Option<Retained<NSURL>> {
    if let Some(set) = panel.ivars().directory.borrow().clone() {
        return Some(set);
    }
    let home = std::env::var("HOME").ok()?;
    let documents = format!("{home}/Documents");
    let path = if std::path::Path::new(&documents).is_dir() { documents } else { home };
    Some(NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&path), true))
}

/// A save panel's URL: the file chosen, else the folder and the name.
fn save_url(panel: &NSSavePanelImpl) -> Option<Retained<NSURL>> {
    if let Some(chosen) = panel.ivars().chosen.borrow().clone() {
        return Some(chosen);
    }
    if let Some(open) = open_ivars(panel) {
        return open.urls.borrow().first().cloned();
    }
    let dir = directory(panel)?;
    let name = panel.ivars().name.borrow().clone();
    let path = dir.path()?.to_string();
    Some(NSURL::fileURLWithPath_isDirectory(
        &NSString::from_str(&format!("{}/{name}", path.trim_end_matches('/'))),
        false,
    ))
}

/// A type the program allows, as file name patterns.
fn patterns_of(kind: &str) -> Vec<String> {
    let known: &[(&str, &[&str])] = &[
        ("public.plain-text", &["txt", "text"]),
        ("public.text", &["txt", "text"]),
        ("public.utf8-plain-text", &["txt"]),
        ("public.html", &["html", "htm"]),
        ("public.rtf", &["rtf"]),
        ("public.json", &["json"]),
        ("public.xml", &["xml"]),
        ("public.png", &["png"]),
        ("public.jpeg", &["jpg", "jpeg"]),
        ("public.tiff", &["tif", "tiff"]),
        ("com.compuserve.gif", &["gif"]),
        ("public.image", &["png", "jpg", "jpeg", "gif", "webp", "tif", "tiff", "bmp"]),
        ("com.adobe.pdf", &["pdf"]),
        ("public.comma-separated-values-text", &["csv"]),
        ("public.source-code", &["rs", "c", "h", "swift", "py", "js"]),
        ("public.zip-archive", &["zip"]),
    ];
    let kind = kind.trim_start_matches('.');
    match known.iter().find(|(uti, _)| *uti == kind) {
        Some((_, exts)) => exts.iter().map(|e| format!("*.{e}")).collect(),
        // Anything else with a dot is an identifier Sidestep can't place;
        // an extension otherwise.
        None if kind.contains('.') => Vec::new(),
        None => vec![format!("*.{kind}")],
    }
}

/// What to ask the portal, from the panel's settings.
fn question(panel: &NSSavePanel) -> Choose {
    let p = imp(panel);
    let window: &NSWindow = panel;
    let open = open_ivars(p);
    let mut patterns = Vec::new();
    if let Some(types) = p.ivars().allowed_types.borrow().as_ref() {
        for t in types.iter() {
            patterns.extend(patterns_of(&t.to_string()));
        }
    }
    let filters = if patterns.is_empty() { Vec::new() } else { vec![("Supported files".to_owned(), patterns)] };
    let (multiple, directory) = match open {
        Some(o) => (o.multiple.get(), o.directories.get() && !o.files.get()),
        None => (false, false),
    };
    Choose {
        save: open.is_none(),
        title: window.title().to_string(),
        accept: p.ivars().prompt.borrow().to_string(),
        multiple,
        directory,
        folder: directory_of(p),
        name: open.is_none().then(|| p.ivars().name.borrow().to_string()),
        filters,
    }
}

fn directory_of(panel: &NSSavePanelImpl) -> Option<String> {
    directory(panel).and_then(|u| u.path()).map(|p| p.to_string())
}

/// Take the portal's answer into the panel; the response it comes to.
fn take(panel: &NSSavePanel, chosen: Chosen) -> NSModalResponse {
    let p = imp(panel);
    match chosen {
        Chosen::Uris(uris) => {
            let urls: Vec<Retained<NSURL>> =
                uris.iter().filter_map(|u| NSURL::URLWithString(&NSString::from_str(u))).collect();
            match open_ivars(p) {
                Some(open) => {
                    let old = open.urls.replace(urls);
                    drop(old);
                }
                None => {
                    let first = urls.into_iter().next();
                    if let Some(path) = first.as_ref().and_then(|u| u.path()) {
                        let path = path.to_string();
                        if let Some((dir, name)) = path.rsplit_once('/') {
                            let dir = if dir.is_empty() { "/" } else { dir };
                            let old = p
                                .ivars()
                                .directory
                                .replace(Some(NSURL::fileURLWithPath_isDirectory(&NSString::from_str(dir), true)));
                            drop(old);
                            drop(p.ivars().name.replace(NSString::from_str(name)));
                        }
                    }
                    drop(p.ivars().chosen.replace(first));
                }
            }
            NSModalResponseOK
        }
        Chosen::Cancelled => NSModalResponseCancel,
        Chosen::Failed => NSModalResponseAbort,
    }
}

/// Ask, and run a modal loop for the panel until the answer comes (or the
/// program ends it: the dialog is then given up).
fn run_modal(panel: &NSSavePanel) -> NSModalResponse {
    let mtm = MainThreadMarker::from(panel);
    let answered = panel.retain();
    let id = portal::choose(question(panel), move |chosen| {
        imp(&answered).ivars().pending.set(None);
        let code = take(&answered, chosen);
        crate::modal::stop_window(&answered, code);
    });
    imp(panel).ivars().pending.set(Some(id));
    let code = NSApplication::sharedApplication(mtm).runModalForWindow(panel);
    if imp(panel).ivars().pending.take().is_some() {
        drop(portal::abandon(id));
    }
    code
}

/// Ask, and call `handler` with the response when the answer comes.
fn begin(panel: &NSSavePanel, handler: &DynBlock<dyn Fn(NSModalResponse)>) {
    let handler = handler.copy();
    let answered = panel.retain();
    let id = portal::choose(question(panel), move |chosen| {
        imp(&answered).ivars().pending.set(None);
        let code = take(&answered, chosen);
        handler.call((code,));
    });
    imp(panel).ivars().pending.set(Some(id));
}

/// Give the desktop's dialog up, answering `chosen` in its place.
fn give_up(panel: &NSSavePanelImpl, chosen: Chosen) {
    let Some(id) = panel.ivars().pending.take() else { return };
    if let Some(then) = portal::abandon(id) {
        then(chosen);
    }
}

// NSOpenPanel

pub(crate) struct OpenIvars {
    urls: RefCell<Vec<Retained<NSURL>>>,
    resolves_aliases: Cell<bool>,
    directories: Cell<bool>,
    multiple: Cell<bool>,
    files: Cell<bool>,
    ubiquitous_conflicts: Cell<bool>,
    ubiquitous_downloads: Cell<bool>,
    accessory_disclosed: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSSavePanel, NSPanel, NSWindow, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSOpenPanel"]
    #[ivars = OpenIvars]
    pub(crate) struct NSOpenPanelImpl;

    impl NSOpenPanelImpl {
        #[unsafe(method_id(initWithContentRect:styleMask:backing:defer:))]
        fn init_with_content_rect(
            this: Allocated<Self>,
            rect: NSRect,
            style: NSWindowStyleMask,
            backing: NSBackingStoreType,
            defer: bool,
        ) -> Retained<Self> {
            let this = this.set_ivars(OpenIvars {
                urls: RefCell::new(Vec::new()),
                resolves_aliases: Cell::new(true),
                directories: Cell::new(false),
                multiple: Cell::new(false),
                files: Cell::new(true),
                ubiquitous_conflicts: Cell::new(false),
                ubiquitous_downloads: Cell::new(false),
                accessory_disclosed: Cell::new(false),
            });
            // SAFETY: NSSavePanel's designated initializer.
            let this: Retained<Self> =
                unsafe { msg_send![super(this), initWithContentRect: rect, styleMask: style, backing: backing, defer: defer] };
            let window: &NSWindow = &this;
            window.setTitle(&NSString::from_str("Open"));
            let save = save_ivars(&*this);
            drop(save.prompt.replace(NSString::from_str("Open")));
            save.create_directories.set(false);
            this
        }

        #[unsafe(method_id(openPanel))]
        fn open_panel() -> Retained<NSOpenPanel> {
            let mtm = MainThreadMarker::new().expect("sidestep: panels belong to the main thread");
            NSOpenPanel::new(mtm)
        }

        #[unsafe(method_id(URLs))]
        fn urls(&self) -> Retained<NSArray<NSURL>> {
            NSArray::from_retained_slice(&self.ivars().urls.borrow())
        }

        #[unsafe(method(resolvesAliases))]
        fn resolves_aliases(&self) -> bool {
            self.ivars().resolves_aliases.get()
        }

        #[unsafe(method(setResolvesAliases:))]
        fn set_resolves_aliases(&self, flag: bool) {
            self.ivars().resolves_aliases.set(flag);
        }

        #[unsafe(method(canChooseDirectories))]
        fn can_choose_directories(&self) -> bool {
            self.ivars().directories.get()
        }

        #[unsafe(method(setCanChooseDirectories:))]
        fn set_can_choose_directories(&self, flag: bool) {
            self.ivars().directories.set(flag);
        }

        #[unsafe(method(allowsMultipleSelection))]
        fn allows_multiple_selection(&self) -> bool {
            self.ivars().multiple.get()
        }

        #[unsafe(method(setAllowsMultipleSelection:))]
        fn set_allows_multiple_selection(&self, flag: bool) {
            self.ivars().multiple.set(flag);
        }

        #[unsafe(method(canChooseFiles))]
        fn can_choose_files(&self) -> bool {
            self.ivars().files.get()
        }

        #[unsafe(method(setCanChooseFiles:))]
        fn set_can_choose_files(&self, flag: bool) {
            self.ivars().files.set(flag);
        }

        #[unsafe(method(canResolveUbiquitousConflicts))]
        fn can_resolve_ubiquitous_conflicts(&self) -> bool {
            self.ivars().ubiquitous_conflicts.get()
        }

        #[unsafe(method(setCanResolveUbiquitousConflicts:))]
        fn set_can_resolve_ubiquitous_conflicts(&self, flag: bool) {
            self.ivars().ubiquitous_conflicts.set(flag);
        }

        #[unsafe(method(canDownloadUbiquitousContents))]
        fn can_download_ubiquitous_contents(&self) -> bool {
            self.ivars().ubiquitous_downloads.get()
        }

        #[unsafe(method(setCanDownloadUbiquitousContents:))]
        fn set_can_download_ubiquitous_contents(&self, flag: bool) {
            self.ivars().ubiquitous_downloads.set(flag);
        }

        #[unsafe(method(isAccessoryViewDisclosed))]
        fn is_accessory_view_disclosed(&self) -> bool {
            self.ivars().accessory_disclosed.get()
        }

        #[unsafe(method(setAccessoryViewDisclosed:))]
        fn set_accessory_view_disclosed(&self, flag: bool) {
            self.ivars().accessory_disclosed.set(flag);
        }

        // What old versions of AppKit had.

        #[unsafe(method_id(filenames))]
        fn filenames(&self) -> Retained<NSArray<NSString>> {
            let paths: Vec<Retained<NSString>> = self.ivars().urls.borrow().iter().filter_map(|u| u.path()).collect();
            NSArray::from_retained_slice(&paths)
        }

        #[unsafe(method(runModalForTypes:))]
        fn run_modal_for_types(&self, types: Option<&NSArray<NSString>>) -> NSModalResponse {
            as_save(self).setAllowedFileTypes(types);
            run_modal(as_save(self))
        }

        #[unsafe(method(runModalForDirectory:file:types:))]
        fn run_modal_for_directory_file_types(
            &self,
            directory: Option<&NSString>,
            file: Option<&NSString>,
            types: Option<&NSArray<NSString>>,
        ) -> NSModalResponse {
            imp(as_save(self)).set_start(directory, file);
            as_save(self).setAllowedFileTypes(types);
            run_modal(as_save(self))
        }
    }
);

fn as_save(panel: &NSOpenPanelImpl) -> &NSSavePanel {
    // SAFETY: an open panel is a save panel.
    unsafe { &*(panel as *const NSOpenPanelImpl).cast::<NSSavePanel>() }
}

/// An open panel's own settings, if `panel` is one.
fn open_ivars(panel: &NSSavePanelImpl) -> Option<&OpenIvars> {
    // SAFETY: NSOpenPanelImpl is the class NSOpenPanel names.
    unsafe { crate::controls::impl_of::<NSOpenPanel, NSOpenPanelImpl>(panel) }.map(|o| o.ivars())
}

#[cfg(test)]
mod tests {
    use super::patterns_of;

    #[test]
    fn allowed_types_become_patterns() {
        assert_eq!(patterns_of("txt"), ["*.txt"]);
        assert_eq!(patterns_of(".md"), ["*.md"]);
        assert_eq!(patterns_of("public.jpeg"), ["*.jpg", "*.jpeg"]);
        assert_eq!(patterns_of("com.adobe.pdf"), ["*.pdf"]);
        // An identifier Sidestep can't place filters nothing.
        assert!(patterns_of("com.example.private").is_empty());
    }
}
