//! `NSPasteboardItem`: one thing on a pasteboard, with a value per type.
//!
//! A pasteboard is a list of items. Writing an object makes an item of it
//! (an `NSPasteboardItem` is written as itself), and the pasteboard's own
//! `setString:forType:` and the like write to its first item. Items are
//! live: one written to a pasteboard shows what's written to it later, and
//! once the pasteboard is cleared it's empty and takes nothing more. It
//! stays tied to that pasteboard, and writing it to another panics, as
//! AppKit raises. An item on a pasteboard finds the types asked for as the
//! pasteboard does (`availableTypeFromArray:`, a type or one of its
//! kinds); one on none finds nothing, as on macOS.
//!
//! A value is bytes, a string (kept as the string, its UTF-8 made when
//! something needs bytes), a property list (kept as an immutable copy made
//! all the way down: Sidestep doesn't serialize property lists yet), a
//! promise, or data another program offers. Promises come from `declareTypes:owner:` (the owner is asked
//! with `pasteboard:provideDataForType:`), from an item's data provider
//! (`pasteboard:item:provideDataForType:`), and from objects written with
//! the promised writing option (`pasteboardPropertyListForType:`); each
//! is asked when the type is first read, with no lock held, so it can
//! write the value back. Another program's data is read through the
//! clipboard's transport when first asked for, and kept.
//!
//! Items may be used from any thread, as AppKit allows: their values are
//! behind a mutex, which is never held while other code runs.

use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};

use crate::clipboard::{self, Source};
use crate::pasteboard_types::{self as types, STRING};

/// An object another thread may be handed: an owner or data provider the
/// program gave (AppKit calls them on whichever thread reads), or a
/// property list, copied all the way down when it was set (`frozen`), so
/// immutable.
pub(crate) struct Object(pub Retained<AnyObject>);

// SAFETY: see the type's documentation. A property list is kept as a copy
// made of new arrays and dictionaries holding immutable copies of their
// strings, numbers, data and dates, which Foundation lets threads share,
// and which nothing else holds to change; owners and providers are
// messaged from the reading thread as AppKit messages them.
unsafe impl Send for Object {}
// SAFETY: as for Send.
unsafe impl Sync for Object {}

impl Clone for Object {
    fn clone(&self) -> Self {
        Object(self.0.clone())
    }
}

/// Who makes a promised value.
#[derive(Clone)]
pub(crate) enum Provider {
    /// Declared with no owner: nothing will come.
    Nobody,
    /// A pasteboard owner (`pasteboard:provideDataForType:`).
    Owner(Object),
    /// An item's data provider (`pasteboard:item:provideDataForType:`).
    Item(Object),
    /// A written object (`pasteboardPropertyListForType:`).
    Writer(Object),
}

pub(crate) enum Value {
    Data(Arc<[u8]>),
    String(Retained<NSString>),
    PropertyList(Object),
    Promised(Provider),
    /// Another program's, as `mime` from `source`, for the contents of its
    /// change `generation` (read as nothing once it has changed).
    Foreign {
        mime: Arc<str>,
        source: Source,
        generation: isize,
    },
}

impl Value {
    /// The value, if it's there to read without asking anyone.
    fn as_read(&self) -> Option<Read> {
        match self {
            Value::Data(d) => Some(Read::Data(d.clone())),
            Value::String(s) => Some(Read::String(s.clone())),
            Value::PropertyList(p) => Some(Read::PropertyList(p.0.clone())),
            Value::Promised(_) | Value::Foreign { .. } => None,
        }
    }
}

/// A value as read, with no lock held.
pub(crate) enum Read {
    Data(Arc<[u8]>),
    String(Retained<NSString>),
    PropertyList(Retained<AnyObject>),
}

impl Read {
    /// As bytes: a string's UTF-8; a property list has none.
    pub(crate) fn bytes(&self) -> Option<Arc<[u8]>> {
        match self {
            Read::Data(d) => Some(d.clone()),
            Read::String(s) => Some(s.to_string().into_bytes().into()),
            Read::PropertyList(_) => None,
        }
    }

    /// As a string: bytes if they're UTF-8.
    pub(crate) fn string(&self) -> Option<Retained<NSString>> {
        match self {
            Read::Data(d) => std::str::from_utf8(d).ok().map(NSString::from_str),
            Read::String(s) => Some(s.clone()),
            Read::PropertyList(_) => None,
        }
    }

    /// As a property list: a string is one; bytes aren't read as one.
    pub(crate) fn property_list(&self) -> Option<Retained<AnyObject>> {
        match self {
            Read::Data(_) => None,
            Read::String(s) => Some(Retained::into_super(Retained::into_super(s.clone()))),
            Read::PropertyList(p) => Some(p.clone()),
        }
    }
}

/// A pasteboard, which is never freed (see `pasteboard`), as items point
/// back to it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct BoardRef(NonNull<NSPasteboard>);

// SAFETY: pasteboards live as long as the program and may be used from any
// thread.
unsafe impl Send for BoardRef {}
// SAFETY: as for Send.
unsafe impl Sync for BoardRef {}

impl BoardRef {
    pub(crate) fn new(board: &NSPasteboard) -> Self {
        BoardRef(NonNull::from(board))
    }

    pub(crate) fn get(&self) -> &'static NSPasteboard {
        // SAFETY: pasteboards are never freed.
        unsafe { self.0.as_ref() }
    }
}

#[derive(Default)]
pub(crate) struct State {
    /// Types and their values, in the order written.
    pub entries: Vec<(String, Value)>,
    /// The pasteboard it was written to, which it stays tied to.
    pub board: Option<BoardRef>,
    /// The pasteboard was cleared since: the item is empty for good.
    pub retired: bool,
}

pub(crate) struct ItemIvars {
    state: Mutex<State>,
}

sidestep_runtime::static_class!(pub NSPASTEBOARDITEM, NSPASTEBOARDITEM_META = "NSPasteboardItem", || {
    let _ = NSPasteboardItemImpl::class();
});

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSPasteboardItem"]
    #[ivars = ItemIvars]
    pub(crate) struct NSPasteboardItemImpl;

    impl NSPasteboardItemImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ItemIvars { state: Mutex::new(State::default()) });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(types))]
        fn types(&self) -> Retained<NSArray<NSString>> {
            self.type_list()
        }

        /// Only an item on a pasteboard finds anything, as on macOS.
        #[unsafe(method_id(availableTypeFromArray:))]
        fn available_type_from_array(&self, wanted: &NSArray<NSString>) -> Option<Retained<NSString>> {
            self.available(wanted)
        }

        #[unsafe(method(setDataProvider:forTypes:))]
        fn set_data_provider_for_types(&self, provider: &AnyObject, kinds: &NSArray<NSString>) -> bool {
            let provider = Object(provider.retain());
            kinds.iter().all(|kind| self.set(types::from_ns(&kind), Value::Promised(Provider::Item(provider.clone()))))
        }

        #[unsafe(method(setData:forType:))]
        fn set_data_for_type(&self, data: &NSData, kind: &NSString) -> bool {
            self.set(types::from_ns(kind), Value::Data(bytes_of(data)))
        }

        #[unsafe(method(setString:forType:))]
        fn set_string_for_type(&self, string: &NSString, kind: &NSString) -> bool {
            self.set(types::from_ns(kind), string_value(string))
        }

        #[unsafe(method(setPropertyList:forType:))]
        fn set_property_list_for_type(&self, list: &AnyObject, kind: &NSString) -> bool {
            self.set(types::from_ns(kind), property_list_value(list))
        }

        #[unsafe(method_id(dataForType:))]
        fn data_for_type(&self, kind: &NSString) -> Option<Retained<NSData>> {
            let bytes = self.read(&types::from_ns(kind)).and_then(|r| r.bytes());
            bytes.map(|b| NSData::with_bytes(&b))
        }

        #[unsafe(method_id(stringForType:))]
        fn string_for_type(&self, kind: &NSString) -> Option<Retained<NSString>> {
            self.read(&types::from_ns(kind)).and_then(|r| r.string())
        }

        #[unsafe(method_id(propertyListForType:))]
        fn property_list_for_type(&self, kind: &NSString) -> Option<Retained<AnyObject>> {
            self.read(&types::from_ns(kind)).and_then(|r| r.property_list())
        }

        // NSPasteboardWriting: an item writes its own types.
        #[unsafe(method_id(writableTypesForPasteboard:))]
        fn writable_types_for_pasteboard(&self, _board: &NSPasteboard) -> Retained<NSArray<NSString>> {
            self.type_list()
        }

        #[unsafe(method_id(pasteboardPropertyListForType:))]
        fn pasteboard_property_list_for_type(&self, kind: &NSString) -> Option<Retained<AnyObject>> {
            match self.read(&types::from_ns(kind)) {
                Some(Read::Data(d)) => Some(Retained::into_super(Retained::into_super(NSData::with_bytes(&d)))),
                other => other.and_then(|r| r.property_list()),
            }
        }
    }

    unsafe impl NSObjectProtocol for NSPasteboardItemImpl {}
);

/// Any item as the implementation class.
pub(crate) fn imp(item: &NSPasteboardItem) -> &NSPasteboardItemImpl {
    // SAFETY: NSPasteboardItem is NSPasteboardItemImpl's class, and
    // subclasses share its layout.
    unsafe { &*(item as *const NSPasteboardItem).cast::<NSPasteboardItemImpl>() }
}

/// A new, empty item.
pub(crate) fn new_item() -> Retained<NSPasteboardItem> {
    crate::load_shell::<NSPasteboardItem>();
    NSPasteboardItem::new()
}

/// A new item holding `entries`, tied to `board`.
pub(crate) fn item_with(entries: Vec<(String, Value)>, board: BoardRef) -> Retained<NSPasteboardItem> {
    let item = new_item();
    {
        let mut state = imp(&item).lock();
        state.entries = entries;
        state.board = Some(board);
    }
    item
}

impl NSPasteboardItemImpl {
    fn available(&self, wanted: &NSArray<NSString>) -> Option<Retained<NSString>> {
        let kinds: Vec<String> = {
            let state = self.lock();
            state.board?;
            state.entries.iter().map(|(k, _)| k.clone()).collect()
        };
        types::first_available(&kinds, wanted)
    }

    fn type_list(&self) -> Retained<NSArray<NSString>> {
        let kinds: Vec<Retained<NSString>> = self.lock().entries.iter().map(|(k, _)| NSString::from_str(k)).collect();
        NSArray::from_retained_slice(&kinds)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.ivars().state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Set a type's value, keeping its place if it had one, and tell the
    /// pasteboard it's on; false, and nothing set, once that pasteboard was
    /// cleared.
    pub(crate) fn set(&self, kind: String, value: Value) -> bool {
        let mut state = self.lock();
        if state.retired {
            return false;
        }
        let old = match state.entries.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, v)) => Some(std::mem::replace(v, value)),
            None => {
                state.entries.push((kind, value));
                None
            }
        };
        let board = state.board;
        drop(state);
        // A promise kept (by whoever made it, or the program writing that
        // type after all) changes no type, so there's nothing new to offer
        // other programs: they're answered from here when they ask.
        let kept = matches!(old, Some(Value::Promised(_)));
        // Released with no lock held: it may be the last reference to an
        // object whose dealloc runs code.
        drop(old);
        if let Some(board) = board {
            crate::pasteboard::item_changed(board.get(), !kept);
        }
        true
    }

    /// Whether the item has a value (or a promise) for `kind`.
    pub(crate) fn has(&self, kind: &str) -> bool {
        self.lock().entries.iter().any(|(k, _)| k == kind)
    }

    /// The value of `kind`, asking whoever promised it, or reading it from
    /// the program that offers it, the first time.
    pub(crate) fn read(&self, kind: &str) -> Option<Read> {
        let (board, pending) = {
            let state = self.lock();
            let board = state.board;
            let (_, value) = state.entries.iter().find(|(k, _)| k == kind)?;
            if let Some(read) = value.as_read() {
                return Some(read);
            }
            match value {
                Value::Promised(provider) => (board, Pending::Promise(provider.clone())),
                Value::Foreign { mime, source, generation } => {
                    (board, Pending::Foreign(mime.clone(), *source, *generation))
                }
                _ => return None,
            }
        };
        match pending {
            Pending::Promise(provider) => {
                self.ask(provider, kind, board);
                // Whatever the provider wrote, if it wrote anything.
                let state = self.lock();
                let (_, value) = state.entries.iter().find(|(k, _)| k == kind)?;
                value.as_read()
            }
            Pending::Foreign(mime, source, generation) => {
                let shared = clipboard::shared_for(source);
                let data = if kind == STRING && source == Source::Selection {
                    // Read ahead when it was offered.
                    let text = shared.foreign_text().map(|t| Arc::<[u8]>::from(t.into_bytes()));
                    text.filter(|_| shared.is_current(generation))
                } else {
                    shared.read_offer(&mime, Some(generation))
                };
                let data = data?;
                let mut state = self.lock();
                if let Some((_, value @ Value::Foreign { .. })) = state.entries.iter_mut().find(|(k, _)| k == kind) {
                    *value = Value::Data(data.clone());
                }
                Some(Read::Data(data))
            }
        }
    }

    /// Ask `provider` for `kind`, with no lock held; what it gives is
    /// written to the item.
    fn ask(&self, provider: Provider, kind: &str, board: Option<BoardRef>) {
        let kind_ns = NSString::from_str(kind);
        match provider {
            Provider::Nobody => {}
            Provider::Owner(owner) => {
                let Some(board) = board else { return };
                // SAFETY: owners implement pasteboard:provideDataForType:,
                // which takes the pasteboard and the type.
                let _: () = unsafe { msg_send![&*owner.0, pasteboard: board.get(), provideDataForType: &*kind_ns] };
            }
            Provider::Item(provider) => {
                let board = board.map(|b| b.get());
                // SAFETY: data providers implement
                // pasteboard:item:provideDataForType:.
                let _: () = unsafe {
                    msg_send![&*provider.0, pasteboard: board, item: as_item(self), provideDataForType: &*kind_ns]
                };
            }
            Provider::Writer(object) => {
                // SAFETY: pasteboard writers implement
                // pasteboardPropertyListForType:, returning an object or nil.
                let list: Option<Retained<AnyObject>> =
                    unsafe { msg_send![&*object.0, pasteboardPropertyListForType: &*kind_ns] };
                if let Some(list) = list {
                    self.set(kind.to_owned(), value_of_property_list(&list));
                }
            }
        }
    }
}

enum Pending {
    Promise(Provider),
    Foreign(Arc<str>, Source, isize),
}

pub(crate) fn as_item(item: &NSPasteboardItemImpl) -> &NSPasteboardItem {
    // SAFETY: as in `imp`.
    unsafe { &*(item as *const NSPasteboardItemImpl).cast::<NSPasteboardItem>() }
}

pub(crate) fn string_value(string: &NSString) -> Value {
    // Strings may be mutable; the item keeps an immutable copy.
    // SAFETY: -copy returns an immutable string, retained.
    let copy: Retained<NSString> = unsafe { msg_send![string, copy] };
    Value::String(copy)
}

pub(crate) fn property_list_value(list: &AnyObject) -> Value {
    Value::PropertyList(Object(frozen(list)))
}

/// A property list as the pasteboard keeps it, as macOS keeps a snapshot:
/// arrays and dictionaries made anew all the way down, holding immutable
/// copies of the rest (strings, numbers, data, dates), so what the program
/// changes later, or on another thread, isn't what's on the pasteboard.
fn frozen(list: &AnyObject) -> Retained<AnyObject> {
    if let Some(array) = list.downcast_ref::<NSArray>() {
        let items: Vec<Retained<AnyObject>> = array.iter().map(|item| frozen(&item)).collect();
        return Retained::into_super(Retained::into_super(NSArray::from_retained_slice(&items)));
    }
    if let Some(dictionary) = list.downcast_ref::<NSDictionary>() {
        let (keys, values) = dictionary.to_vecs();
        // A property list's keys are strings (the new dictionary copies
        // them); other keys make no property list, and the dictionary is
        // copied as it is.
        let keys: Option<Vec<Retained<NSString>>> = keys.into_iter().map(|k| k.downcast::<NSString>().ok()).collect();
        if let Some(keys) = keys {
            let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let values: Vec<Retained<AnyObject>> = values.iter().map(|v| frozen(v)).collect();
            let copy = NSDictionary::<NSString, AnyObject>::from_retained_objects(&keys, &values);
            return Retained::into_super(Retained::into_super(copy));
        }
    }
    if responds(list, sel!(copyWithZone:)) {
        // SAFETY: the object copies itself: -copy returns an immutable
        // copy (or the object itself, when it's immutable), retained.
        unsafe { msg_send![list, copy] }
    } else {
        list.retain()
    }
}

/// What an object returned from `pasteboardPropertyListForType:` becomes:
/// a string, bytes, or another property list.
pub(crate) fn value_of_property_list(list: &AnyObject) -> Value {
    if let Some(string) = list.downcast_ref::<NSString>() {
        return string_value(string);
    }
    if let Some(data) = list.downcast_ref::<NSData>() {
        return Value::Data(bytes_of(data));
    }
    property_list_value(list)
}

/// An NSData's bytes.
pub(crate) fn bytes_of(data: &NSData) -> Arc<[u8]> {
    // SAFETY: the bytes are copied before anything else runs.
    Arc::from(unsafe { sidestep_foundation::data::bytes(data) })
}

/// Whether `object` responds to `selector`.
pub(crate) fn responds(object: &AnyObject, selector: objc2::runtime::Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![object, respondsToSelector: selector] }
}

/// The selectors of the pasteboard writing protocol, for `writeObjects:`.
pub(crate) fn writes_itself(object: &AnyObject) -> bool {
    responds(object, sel!(writableTypesForPasteboard:)) && responds(object, sel!(pasteboardPropertyListForType:))
}
