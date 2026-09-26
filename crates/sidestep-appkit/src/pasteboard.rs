//! `NSPasteboard`. The general pasteboard is the system clipboard, carried
//! by the render thread (see [`crate::clipboard`]); the drag pasteboard
//! (`NSPasteboardNameDrag`) holds the drag over one of our windows (see
//! [`crate::drag`]); pasteboards made by name or with a unique name live in
//! this process only.
//!
//! A pasteboard holds items (`NSPasteboardItem`, see
//! [`crate::pasteboard_item`]). Its own `setString:forType:`,
//! `setData:forType:` and `setPropertyList:forType:` write to the first
//! item, and its reads take the first item that has the type, except text,
//! which is every item's text a line each, as on macOS. `types` lists every
//! item's types in the order written, each followed by its old name, and
//! `availableTypeFromArray:` finds a type or one of its kinds (see
//! [`crate::pasteboard_types`]). `declareTypes:owner:` and
//! `addTypes:owner:` promise types their owner provides when they're first
//! read, replacing what was there. Asking whether objects can be read
//! (`canReadObjectForClasses:options:`) looks only at the types: nothing is
//! read, asked for or made.
//!
//! What a program writes it can read back at once. What another program
//! copied is read ahead, as text, when it's copied, so `stringForType:`
//! doesn't wait; for other types, or if the text is still arriving, a read
//! waits while the data comes, and returns nil once
//! `clipboard::READ_TIMEOUT` (200 ms) passes without any, which is
//! remembered until the next copy. Another program's files (a URL list)
//! become an item each, read when the program first asks about the
//! pasteboard's items or types (a drag's come with the drag).
//!
//! Writes to the general pasteboard on the main thread are offered to other
//! programs once per turn of the event loop, so a copy that writes several
//! types makes one selection, not one per type; writes from other threads
//! are offered at once. Promised types are offered too, and their owner is
//! asked, on the main thread, when another program reads them; a promise
//! kept changes no type, so it isn't offered again.
//!
//! Pasteboards are safe to use from any thread, as on macOS: their state
//! is behind a mutex, never held while other code runs. Pasteboard objects
//! are never freed, as their items point back to them: `releaseGlobally`,
//! the only way AppKit frees one, lets go of its contents and its name.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, MessageReceiver, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadMarker, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem};
use objc2_foundation::{NSArray, NSData, NSString};

use crate::clipboard::{self, Contents, Source};
use crate::pasteboard_item::{
    self as item, BoardRef, NSPasteboardItemImpl, Object, Provider, Read, Value, imp as item_imp,
};
use crate::pasteboard_types::{self as types, FILE_URL, FILENAMES, OLD_URL, STRING, URL};

/// The general pasteboard's name, `NSPasteboardNameGeneral`.
const GENERAL: &str = "Apple CFPasteboard general";
/// The drag pasteboard's name, `NSPasteboardNameDrag`.
pub(crate) const DRAG: &str = "Apple CFPasteboard drag";

/// `NSNotFound`, which `indexOfPasteboardItem:` answers for items that
/// aren't there.
const NOT_FOUND: usize = isize::MAX as usize;

/// `NSPasteboardWritingPromised`.
const WRITING_PROMISED: usize = 1 << 9;

/// The general pasteboard changed on the main thread since it was last
/// offered.
static UNOFFERED: AtomicBool = AtomicBool::new(false);

/// Where a pasteboard's contents come from when this process didn't write
/// them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Only this process writes it.
    Local,
    /// Another program's offer, through the render thread, unless this
    /// process wrote it since.
    System(Source),
}

struct Held {
    /// For a local pasteboard, its `changeCount`. For the general and drag
    /// pasteboards, the change count the items belong to: once the shared
    /// count has moved on, another program's offer is current, and its
    /// items are made when first asked for.
    generation: isize,
    items: Vec<Retained<NSPasteboardItem>>,
    /// Counts changes to the items and their values, which `types` is
    /// cached against.
    version: u64,
    types: Option<(u64, Retained<NSArray<NSString>>)>,
}

impl Held {
    fn touch(&mut self) {
        self.version += 1;
    }
}

/// Any thread may message a pasteboard: the name is an immutable string
/// made here, and the rest is behind the mutex (items are made for any
/// thread to use too).
pub(crate) struct PasteboardIvars {
    name: Retained<NSString>,
    kind: Kind,
    held: Mutex<Held>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSPasteboard"]
    #[ivars = PasteboardIvars]
    pub(crate) struct NSPasteboardImpl;

    impl NSPasteboardImpl {
        #[unsafe(method_id(generalPasteboard))]
        fn general_pasteboard() -> Retained<NSPasteboard> {
            named(GENERAL)
        }

        #[unsafe(method_id(pasteboardWithName:))]
        fn pasteboard_with_name(name: &NSString) -> Retained<NSPasteboard> {
            named(&name.to_string())
        }

        #[unsafe(method_id(pasteboardWithUniqueName))]
        fn pasteboard_with_unique_name() -> Retained<NSPasteboard> {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            named(&format!("Sidestep unique pasteboard {}-{n}", std::process::id()))
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Retained<NSString> {
            self.ivars().name.clone()
        }

        #[unsafe(method(changeCount))]
        fn change_count(&self) -> isize {
            self.count()
        }

        #[unsafe(method(clearContents))]
        fn clear_contents(&self) -> isize {
            self.clear(Vec::new())
        }

        #[unsafe(method(prepareForNewContentsWithOptions:))]
        fn prepare_for_new_contents_with_options(&self, _options: usize) -> isize {
            self.clear(Vec::new())
        }

        #[unsafe(method(declareTypes:owner:))]
        fn declare_types_owner(&self, kinds: &NSArray<NSString>, owner: Option<&AnyObject>) -> isize {
            self.clear(promises(kinds, owner))
        }

        /// The owner's promises replace what the types held, as on macOS.
        #[unsafe(method(addTypes:owner:))]
        fn add_types_owner(&self, kinds: &NSArray<NSString>, owner: Option<&AnyObject>) -> isize {
            let first = self.first_item();
            let first = item_imp(&first);
            for (kind, value) in promises(kinds, owner) {
                first.set(kind, value);
            }
            self.count()
        }

        #[unsafe(method_id(types))]
        fn types(&self) -> Option<Retained<NSArray<NSString>>> {
            Some(self.type_list())
        }

        #[unsafe(method_id(availableTypeFromArray:))]
        fn available_type_from_array(&self, wanted: &NSArray<NSString>) -> Option<Retained<NSString>> {
            types::first_available(&kinds_of(&self.items()), wanted)
        }

        #[unsafe(method(setData:forType:))]
        fn set_data_for_type(&self, data: Option<&NSData>, kind: &NSString) -> bool {
            let bytes = data.map_or_else(|| Arc::from(&[][..]), item::bytes_of);
            self.write(types::from_ns(kind), Value::Data(bytes))
        }

        #[unsafe(method(setString:forType:))]
        fn set_string_for_type(&self, string: &NSString, kind: &NSString) -> bool {
            self.write(types::from_ns(kind), item::string_value(string))
        }

        #[unsafe(method(setPropertyList:forType:))]
        fn set_property_list_for_type(&self, list: &AnyObject, kind: &NSString) -> bool {
            let kind = types::from_ns(kind);
            match strings_of(list).filter(|_| kind == FILENAMES) {
                // Paths become file URLs, an item each, as on macOS.
                Some(paths) => {
                    self.write_urls(paths.iter().map(|p| types::file_url(p)));
                    true
                }
                None => self.write(kind, item::property_list_value(list)),
            }
        }

        #[unsafe(method_id(dataForType:))]
        fn data_for_type(&self, kind: &NSString) -> Option<Retained<NSData>> {
            let bytes = self.read(&types::from_ns(kind)).and_then(|r| r.bytes());
            bytes.and_then(|b| item::data_object(&b))
        }

        #[unsafe(method_id(stringForType:))]
        fn string_for_type(&self, kind: &NSString) -> Option<Retained<NSString>> {
            self.read(&types::from_ns(kind)).and_then(|r| r.string())
        }

        #[unsafe(method_id(propertyListForType:))]
        fn property_list_for_type(&self, kind: &NSString) -> Option<Retained<AnyObject>> {
            self.property_list(&types::from_ns(kind))
        }

        #[unsafe(method_id(pasteboardItems))]
        fn pasteboard_items(&self) -> Option<Retained<NSArray<NSPasteboardItem>>> {
            Some(NSArray::from_retained_slice(&self.items()))
        }

        #[unsafe(method(indexOfPasteboardItem:))]
        fn index_of_pasteboard_item(&self, wanted: &NSPasteboardItem) -> usize {
            self.items().iter().position(|i| std::ptr::eq(&**i, wanted)).unwrap_or(NOT_FOUND)
        }

        #[unsafe(method(writeObjects:))]
        fn write_objects(&self, objects: &NSArray<AnyObject>) -> bool {
            let mut all = true;
            for object in objects.iter() {
                match self.item_for(&object) {
                    Some(Some(item)) => self.append(item),
                    // Nothing to write, as for an empty item.
                    Some(None) => {}
                    None => all = false,
                }
            }
            all
        }

        #[unsafe(method_id(readObjectsForClasses:options:))]
        fn read_objects_for_classes_options(
            &self,
            classes: &NSArray<AnyClass>,
            options: Option<&AnyObject>,
        ) -> Option<Retained<NSArray<AnyObject>>> {
            let objects: Vec<Retained<AnyObject>> =
                self.read_objects(classes, options).into_iter().map(|(_, o)| o).collect();
            Some(NSArray::from_retained_slice(&objects))
        }

        #[unsafe(method(canReadObjectForClasses:options:))]
        fn can_read_object_for_classes_options(&self, classes: &NSArray<AnyClass>, options: Option<&AnyObject>) -> bool {
            let readers = readers(classes, options, as_board(self));
            self.items().iter().any(|held| {
                let i = item_imp(held);
                readers.iter().any(|reader| reader.can_read(i))
            })
        }

        #[unsafe(method(canReadItemWithDataConformingToTypes:))]
        fn can_read_item_with_data_conforming_to_types(&self, wanted: &NSArray<NSString>) -> bool {
            let wanted: Vec<String> = wanted.iter().map(|w| types::from_ns(&w)).collect();
            kinds_of(&self.items()).iter().any(|k| wanted.iter().any(|w| types::conforms(k, w)))
        }

        /// A pasteboard of the program's own lets go of its contents and its
        /// name (a new one is made if the name is asked for again); the
        /// object itself stays, as items may point to it. The system's
        /// pasteboards stay as they are.
        #[unsafe(method(releaseGlobally))]
        fn release_globally(&self) {
            if self.ivars().kind != Kind::Local {
                return;
            }
            let mut held = self.lock();
            held.touch();
            let old = std::mem::take(&mut held.items);
            drop(held);
            retire(self.board(), old);
            unregister(&self.ivars().name.to_string(), self);
        }
    }

    unsafe impl NSObjectProtocol for NSPasteboardImpl {}
);

impl NSPasteboardImpl {
    /// `types`: every item's types and their old names, made once per
    /// change.
    fn type_list(&self) -> Retained<NSArray<NSString>> {
        let (version, items) = self.items_at();
        if let Some((at, types)) = &self.lock().types
            && *at == version
        {
            return types.clone();
        }
        let kinds = kinds_of(&items);
        let mut listed: Vec<&str> = Vec::with_capacity(kinds.len() * 2);
        for kind in &kinds {
            let old = types::old_name(kind);
            let url = (kind == FILE_URL).then_some(OLD_URL);
            for name in std::iter::once(kind.as_str()).chain(old).chain(url) {
                if !listed.contains(&name) {
                    listed.push(name);
                }
            }
        }
        let strings: Vec<Retained<NSString>> = listed.iter().map(|k| NSString::from_str(k)).collect();
        let array = NSArray::from_retained_slice(&strings);
        let mut held = self.lock();
        // Kept only if nothing changed meanwhile.
        if held.version == version {
            held.types = Some((version, array.clone()));
        }
        array
    }

    /// `changeCount`.
    fn count(&self) -> isize {
        match self.ivars().kind {
            Kind::Local => self.lock().generation,
            Kind::System(source) => clipboard::shared_for(source).change_count(),
        }
    }

    /// `propertyListForType:`: the value, or for the two old URL types, what
    /// the pasteboard's URLs make.
    fn property_list(&self, kind: &str) -> Option<Retained<AnyObject>> {
        let strings = |strings: Vec<Retained<NSString>>| {
            let array = NSArray::from_retained_slice(&strings);
            Retained::into_super(Retained::into_super(array))
        };
        match kind {
            // The paths of the file URLs.
            FILENAMES => {
                let paths: Vec<Retained<NSString>> = self
                    .urls()
                    .iter()
                    .filter_map(|url| types::file_path(url))
                    .map(|p| NSString::from_str(&p))
                    .collect();
                (!paths.is_empty()).then(|| strings(paths))
            }
            // The first URL, with an empty title.
            OLD_URL => {
                let url = self.urls().into_iter().next()?;
                Some(strings(vec![NSString::from_str(&url), NSString::new()]))
            }
            kind => self.read(kind)?.property_list(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Held> {
        self.ivars().held.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn board(&self) -> BoardRef {
        BoardRef::new(as_board(self))
    }

    /// The items, made from another program's offer first if that's what
    /// the pasteboard holds now.
    fn items(&self) -> Vec<Retained<NSPasteboardItem>> {
        self.items_at().1
    }

    /// The items, and the version of the contents they're from.
    fn items_at(&self) -> (u64, Vec<Retained<NSPasteboardItem>>) {
        let Kind::System(source) = self.ivars().kind else {
            let held = self.lock();
            return (held.version, held.items.clone());
        };
        let shared = clipboard::shared_for(source);
        loop {
            let count = shared.change_count();
            let held = self.lock();
            if held.generation == count {
                return (held.version, held.items.clone());
            }
            drop(held);
            // Made with no lock held: it may read the offer.
            let fresh = foreign_items(source, self.board(), count);
            let mut held = self.lock();
            if shared.change_count() != count || held.generation == count {
                // Something changed meanwhile: look again.
                continue;
            }
            held.generation = count;
            held.touch();
            let old = std::mem::replace(&mut held.items, fresh);
            let (version, items) = (held.version, held.items.clone());
            drop(held);
            retire(self.board(), old);
            return (version, items);
        }
    }

    /// The contents, for writing: another program's offer gives way to a
    /// new change of our own first, as a program is expected to have
    /// cleared the pasteboard before writing.
    fn lock_for_writing(&self) -> MutexGuard<'_, Held> {
        let mut held = self.lock();
        if let Kind::System(source) = self.ivars().kind {
            let shared = clipboard::shared_for(source);
            if held.generation != shared.change_count() {
                held.generation = shared.bump();
                held.touch();
                let old = std::mem::take(&mut held.items);
                drop(held);
                retire(self.board(), old);
                held = self.lock();
            }
        }
        held
    }

    /// Start new contents: a new change, and `first` as the first item's
    /// entries if there are any. Returns the new change count.
    fn clear(&self, first: Vec<(String, Value)>) -> isize {
        let fresh = (!first.is_empty()).then(|| item::item_with(first, self.board()));
        let mut held = self.lock();
        held.generation = match self.ivars().kind {
            Kind::Local => held.generation + 1,
            Kind::System(source) => clipboard::shared_for(source).bump(),
        };
        let generation = held.generation;
        held.touch();
        let old = std::mem::replace(&mut held.items, fresh.into_iter().collect());
        drop(held);
        retire(self.board(), old);
        self.changed();
        generation
    }

    /// The first item, made if there's none, for the pasteboard's own
    /// writes.
    fn first_item(&self) -> Retained<NSPasteboardItem> {
        let mut held = self.lock_for_writing();
        if let Some(first) = held.items.first() {
            return first.clone();
        }
        let first = item::item_with(Vec::new(), self.board());
        held.items.push(first.clone());
        held.touch();
        first
    }

    fn write(&self, kind: String, value: Value) -> bool {
        let first = self.first_item();
        // Setting tells the pasteboard it changed (`item_changed`).
        item_imp(&first).set(kind, value)
    }

    /// URLs as the first item's and new ones'.
    fn write_urls(&self, urls: impl Iterator<Item = String>) {
        for (i, url) in urls.enumerate() {
            let kind = types::url_kind(&url);
            let value = Value::String(NSString::from_str(&url));
            if i == 0 {
                self.write(kind.to_owned(), value);
            } else {
                self.append(item::item_with(vec![(kind.to_owned(), value)], self.board()));
            }
        }
    }

    fn append(&self, item: Retained<NSPasteboardItem>) {
        let mut held = self.lock_for_writing();
        held.items.push(item);
        held.touch();
        drop(held);
        self.changed();
    }

    /// A type's value: text from every item that has some, a line each;
    /// anything else from the first item that has it. Another program's
    /// contents that change while they're read are read again, once.
    fn read(&self, kind: &str) -> Option<Read> {
        let Kind::System(source) = self.ivars().kind else { return self.read_items(kind) };
        let shared = clipboard::shared_for(source);
        let count = shared.change_count();
        match self.read_items(kind) {
            None if shared.change_count() != count => self.read_items(kind),
            read => read,
        }
    }

    fn read_items(&self, kind: &str) -> Option<Read> {
        let items = self.items();
        let mut having = items.iter().map(|i| item_imp(i)).filter(|i| i.has(kind));
        let first = having.next()?;
        let rest: Vec<&NSPasteboardItemImpl> = if kind == STRING { having.collect() } else { Vec::new() };
        if rest.is_empty() {
            return first.read(kind);
        }
        let lines: Vec<String> =
            std::iter::once(first).chain(rest).filter_map(|i| i.read(kind)?.string()).map(|s| s.to_string()).collect();
        (!lines.is_empty()).then(|| Read::String(NSString::from_str(&lines.join("\n"))))
    }

    /// Every item's URL (a file URL or another), in order.
    fn urls(&self) -> Vec<String> {
        self.items()
            .iter()
            .filter_map(|i| {
                let i = item_imp(i);
                let kind = [FILE_URL, URL].into_iter().find(|k| i.has(k))?;
                Some(i.read(kind)?.string()?.to_string())
            })
            .collect()
    }

    /// The item `writeObjects:` makes of `object`: `Some(None)` when there's
    /// nothing to write, `None` when the object can't be written.
    fn item_for(&self, object: &AnyObject) -> Option<Option<Retained<NSPasteboardItem>>> {
        let board = self.board();
        if let Some(existing) = object.downcast_ref::<NSPasteboardItem>() {
            let mut state = item_imp(existing).lock();
            if state.board.is_some() {
                drop(state);
                panic!(
                    "-[NSPasteboard writeObjects:]: Cannot write pasteboard item {existing:?}.  It is already \
                     associated with another pasteboard."
                );
            }
            state.board = Some(board);
            let empty = state.entries.is_empty();
            drop(state);
            return Some((!empty).then(|| existing.retain()));
        }
        let one = |kind: &str, value: Value| Some(Some(item::item_with(vec![(kind.to_owned(), value)], board)));
        if let Some(string) = object.downcast_ref::<NSString>() {
            return one(STRING, item::string_value(string));
        }
        if item::is_kind_of(object, c"NSURL") {
            // SAFETY: NSURL's absoluteString and isFileURL take nothing.
            let (string, file): (Option<Retained<NSString>>, bool) =
                unsafe { (msg_send![object, absoluteString], msg_send![object, isFileURL]) };
            let Some(string) = string else { return Some(None) };
            return one(if file { FILE_URL } else { URL }, item::string_value(&string));
        }
        if !item::writes_itself(object) {
            return None;
        }
        // SAFETY: NSPasteboardWriting's methods: the types for a pasteboard,
        // the options for a type, and the value of a type.
        let kinds: Retained<NSArray<NSString>> =
            unsafe { msg_send![object, writableTypesForPasteboard: as_board(self)] };
        let asks_options = item::responds(object, sel!(writingOptionsForType:pasteboard:));
        let mut entries = Vec::new();
        for kind in kinds.iter() {
            let promised = asks_options && {
                // SAFETY: as above.
                let options: usize =
                    unsafe { msg_send![object, writingOptionsForType: &*kind, pasteboard: as_board(self)] };
                options & WRITING_PROMISED != 0
            };
            let value = if promised {
                Value::Promised(Provider::Writer(Object(object.retain())))
            } else {
                // SAFETY: as above.
                let list: Option<Retained<AnyObject>> =
                    unsafe { msg_send![object, pasteboardPropertyListForType: &*kind] };
                match list {
                    Some(list) => item::value_of_property_list(&list),
                    None => continue,
                }
            };
            entries.push((types::from_ns(&kind), value));
        }
        Some((!entries.is_empty()).then(|| item::item_with(entries, board)))
    }

    /// `readObjectsForClasses:options:`: for each item, the first of the
    /// classes that can be made of it, with the item's index.
    fn read_objects(
        &self,
        classes: &NSArray<AnyClass>,
        options: Option<&AnyObject>,
    ) -> Vec<(usize, Retained<AnyObject>)> {
        let readers = readers(classes, options, as_board(self));
        let mut found = Vec::new();
        for (index, held) in self.items().into_iter().enumerate() {
            let object = readers.iter().find_map(|reader| reader.read(&held, as_board(self)));
            found.extend(object.map(|o| (index, o)));
        }
        found
    }

    /// The general pasteboard changed: offer it at the end of the main
    /// thread's turn, or now from another thread.
    fn changed(&self) {
        if self.ivars().kind != Kind::System(Source::Selection) {
            return;
        }
        if MainThreadMarker::new().is_some() {
            UNOFFERED.store(true, Ordering::Relaxed);
        } else {
            self.offer();
        }
    }

    /// Offer what the program wrote as the system selection, or clear it.
    fn offer(&self) {
        let held = self.lock();
        if held.generation != clipboard::shared().change_count() {
            // Another program's selection is current.
            return;
        }
        let items = held.items.clone();
        drop(held);
        let offered = offered(&items);
        clipboard::shared().offer((!offered.items.is_empty()).then_some(offered));
    }
}

fn as_board(board: &NSPasteboardImpl) -> &NSPasteboard {
    // SAFETY: NSPasteboardImpl is the class NSPasteboard names.
    unsafe { &*(board as *const NSPasteboardImpl).cast::<NSPasteboard>() }
}

fn board_imp(board: &NSPasteboard) -> &NSPasteboardImpl {
    // SAFETY: every pasteboard is an NSPasteboardImpl.
    unsafe { &*(board as *const NSPasteboard).cast::<NSPasteboardImpl>() }
}

/// Whether `class` is `other` or a subclass of it.
fn inherits(class: &AnyClass, other: &AnyClass) -> bool {
    let mut current = Some(class);
    while let Some(c) = current {
        if std::ptr::eq(c, other) {
            return true;
        }
        current = c.superclass();
    }
    false
}

/// For each of `board`'s items, the first of `classes` that can be made of
/// it, with the item's index, as `readObjectsForClasses:options:` reads
/// them (for the dragging info's `enumerateDraggingItems…`).
pub(crate) fn objects_by_item(
    board: &NSPasteboard,
    classes: &NSArray<AnyClass>,
    options: Option<&AnyObject>,
) -> Vec<(usize, Retained<AnyObject>)> {
    board_imp(board).read_objects(classes, options)
}

/// An item's value changed: its pasteboard's types may have, and unless
/// only a promise was kept (`offer` false), it's due to be offered again.
pub(crate) fn item_changed(board: &NSPasteboard, offer: bool) {
    let board = board_imp(board);
    board.lock().touch();
    if offer {
        board.changed();
    }
}

/// Items that left `board`: they're emptied, and whoever promised their
/// values hears that the pasteboard is done with them.
fn retire(board: BoardRef, items: Vec<Retained<NSPasteboardItem>>) {
    let mut told: Vec<(Retained<AnyObject>, objc2::runtime::Sel)> = Vec::new();
    for held in &items {
        let entries = {
            let mut state = item_imp(held).lock();
            state.retired = true;
            std::mem::take(&mut state.entries)
        };
        for (_, value) in &entries {
            let (object, selector) = match value {
                Value::Promised(Provider::Owner(o)) => (&o.0, sel!(pasteboardChangedOwner:)),
                Value::Promised(Provider::Item(o)) => (&o.0, sel!(pasteboardFinishedWithDataProvider:)),
                _ => continue,
            };
            if !told.iter().any(|(t, _)| std::ptr::eq(&**t, &**object)) {
                told.push((object.clone(), selector));
            }
        }
        // Released with no lock held.
        drop(entries);
    }
    for (object, selector) in told {
        if item::responds(&object, selector) {
            // SAFETY: both take the pasteboard and return nothing.
            unsafe { MessageReceiver::send_message::<_, ()>(&*object, selector, (board.get(),)) };
        }
    }
}

/// The strings of an array of strings.
fn strings_of(list: &AnyObject) -> Option<Vec<String>> {
    let array = list.downcast_ref::<NSArray>()?;
    array.iter().map(|o| o.downcast_ref::<NSString>().map(|s| s.to_string())).collect()
}

/// The promises `declareTypes:owner:` and `addTypes:owner:` make.
fn promises(kinds: &NSArray<NSString>, owner: Option<&AnyObject>) -> Vec<(String, Value)> {
    let provider = owner.map_or(Provider::Nobody, |o| Provider::Owner(Object(o.retain())));
    let mut entries: Vec<(String, Value)> = Vec::new();
    for kind in kinds.iter() {
        let kind = types::from_ns(&kind);
        if !entries.iter().any(|(k, _)| *k == kind) {
            entries.push((kind, Value::Promised(provider.clone())));
        }
    }
    entries
}

/// Every item's types, each once, in the order written.
fn kinds_of(items: &[Retained<NSPasteboardItem>]) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    for held in items {
        for (kind, _) in &item_imp(held).lock().entries {
            if !kinds.contains(kind) {
                kinds.push(kind.clone());
            }
        }
    }
    kinds
}

/// How `readObjectsForClasses:options:` makes objects of one class.
enum Reader {
    /// The items themselves.
    Item,
    Text,
    /// URLs, only file URLs if so asked.
    Url(Retained<AnyClass>, bool),
    /// A class of the program's (NSPasteboardReading): the types it reads,
    /// as the pasteboard has them, and as asked for.
    Readable(Retained<AnyClass>, Vec<(String, Retained<NSString>)>),
}

/// NSPasteboardReadingOptions: the value is handed over as a string, a
/// property list, or (without either, as it is by default) as data.
const READING_AS_STRING: usize = 1 << 0;
const READING_AS_PROPERTY_LIST: usize = 1 << 1;
const READING_AS_KEYED_ARCHIVE: usize = 1 << 2;

/// A reader for each class that has a way to be read, asking each class of
/// the program's for its types once.
fn readers(classes: &NSArray<AnyClass>, options: Option<&AnyObject>, board: &NSPasteboard) -> Vec<Reader> {
    let file_urls_only = options.is_some_and(|options| {
        let key = NSString::from_str("NSPasteboardURLReadingFileURLsOnlyKey");
        // SAFETY: the options are a dictionary: objectForKey: returns an
        // object or nil, here a number.
        let value: Option<Retained<AnyObject>> = unsafe { msg_send![options, objectForKey: &*key] };
        // SAFETY: as above.
        value.is_some_and(|v| unsafe { msg_send![&*v, boolValue] })
    });
    let (item_class, string_class) = (<NSPasteboardItem as ClassType>::class(), NSString::class());
    let url_class = AnyClass::get(c"NSURL");
    classes
        .iter()
        .filter_map(|class| {
            let is = |other: &AnyClass| inherits(&class, other);
            if is(item_class) {
                return Some(Reader::Item);
            }
            if is(string_class) {
                return Some(Reader::Text);
            }
            if url_class.is_some_and(is) {
                return Some(Reader::Url(class, file_urls_only));
            }
            if !class.metaclass().responds_to(sel!(readableTypesForPasteboard:)) {
                return None;
            }
            // SAFETY: NSPasteboardReading's class method takes the pasteboard
            // and returns the types.
            let readable: Retained<NSArray<NSString>> =
                unsafe { msg_send![&*class, readableTypesForPasteboard: board] };
            let kinds = readable.iter().map(|k| (types::from_ns(&k), k)).collect();
            Some(Reader::Readable(class, kinds))
        })
        .collect()
}

impl Reader {
    /// Whether an object can be made of `held`, going by its types alone.
    fn can_read(&self, held: &NSPasteboardItemImpl) -> bool {
        match self {
            Reader::Item => true,
            Reader::Text => held.has(STRING),
            Reader::Url(_, files_only) => held.has(FILE_URL) || (!files_only && held.has(URL)),
            Reader::Readable(_, kinds) => kinds.iter().any(|(k, _)| held.has(k)),
        }
    }

    /// The object made of `held`, if one can be.
    fn read(&self, held: &Retained<NSPasteboardItem>, board: &NSPasteboard) -> Option<Retained<AnyObject>> {
        let i = item_imp(held);
        match self {
            Reader::Item => Some(Retained::into_super(Retained::into_super(held.clone()))),
            Reader::Text => {
                let string = i.read(STRING)?.string()?;
                Some(Retained::into_super(Retained::into_super(string)))
            }
            Reader::Url(class, files_only) => {
                let kinds: &[&str] = if *files_only { &[FILE_URL] } else { &[FILE_URL, URL] };
                let string = kinds.iter().find(|k| i.has(k)).and_then(|k| i.read(k)?.string())?;
                // SAFETY: URLWithString: takes a string and returns a URL
                // or nil.
                unsafe { msg_send![&**class, URLWithString: &*string] }
            }
            Reader::Readable(class, kinds) => {
                let (kind, name) = kinds.iter().find(|(k, _)| i.has(k))?;
                let options: usize = if class.metaclass().responds_to(sel!(readingOptionsForType:pasteboard:)) {
                    // SAFETY: NSPasteboardReading's optional class method
                    // takes the type and the pasteboard.
                    unsafe { msg_send![&**class, readingOptionsForType: &**name, pasteboard: board] }
                } else {
                    0
                };
                let read = i.read(kind)?;
                let list: Retained<AnyObject> = if options & READING_AS_STRING != 0 {
                    Retained::into_super(Retained::into_super(read.string()?))
                } else if options & READING_AS_PROPERTY_LIST != 0 {
                    read.property_list()?
                } else if options & READING_AS_KEYED_ARCHIVE != 0 {
                    // Sidestep has no keyed archives.
                    return None;
                } else {
                    Retained::into_super(Retained::into_super(item::data_object(&read.bytes()?)?))
                };
                // SAFETY: +alloc makes an instance to initialize, and
                // readers implement initWithPasteboardPropertyList:ofType:,
                // an init method taking the value and the type.
                unsafe {
                    let this: Allocated<AnyObject> = msg_send![&**class, alloc];
                    msg_send![this, initWithPasteboardPropertyList: &*list, ofType: &**name]
                }
            }
        }
    }
}

/// Offer what the main thread wrote to the general pasteboard since the
/// last time, if anything. Called once per turn of the event loop.
pub(crate) fn offer_changes() {
    if UNOFFERED.swap(false, Ordering::Relaxed) {
        board_imp(&named(GENERAL)).offer();
    }
}

/// Another program asked for a promised type of ours as `mime`: ask the
/// owner, on the main thread, and hand the bytes to the render thread.
pub(crate) fn provide_for_render(mime: &str, token: u64) {
    let general = named(GENERAL);
    let general = board_imp(&general);
    let current = general.lock().generation == clipboard::shared().change_count();
    let data = if current { bytes_for_mime(general, mime) } else { None };
    crate::app::send_if_running(crate::protocol::ToRender::SelectionData { token, data });
}

/// The bytes offered as `mime`, asking for promised values.
fn bytes_for_mime(board: &NSPasteboardImpl, mime: &str) -> Option<Arc<[u8]>> {
    if mime == types::URI_LIST || mime == types::GNOME_FILES_MIME {
        let urls = board.urls();
        let urls = urls.iter().map(String::as_str);
        let text = if mime == types::URI_LIST {
            types::uri_list(urls)
        } else {
            types::gnome_copied_files(urls.filter(|u| types::is_file_uri(u)))
        };
        return Some(text.into_bytes().into());
    }
    board.read(&types::type_for_mime(mime)?)?.bytes()
}

/// A value as it can be offered without running other code: `None` for
/// what isn't there or can't travel, `Some(None)` for a promise.
fn peek(held: &NSPasteboardItemImpl, kind: &str) -> Option<Option<Arc<[u8]>>> {
    let state = held.lock();
    let (_, value) = state.entries.iter().find(|(k, _)| k == kind)?;
    match value {
        Value::Data(d) => Some(Some(d.clone())),
        Value::String(s) => Some(Some(s.to_string().into_bytes().into())),
        Value::Promised(_) => Some(None),
        Value::PropertyList(_) | Value::Foreign { .. } => None,
    }
}

/// Our items as the system selection offers them: each type under its MIME
/// types, text joined as `read` joins it, and every item's URL in one list.
fn offered(items: &[Retained<NSPasteboardItem>]) -> Contents {
    let mut contents = Contents { items: Vec::new() };
    let mut add = |mime: &str, data: Option<Arc<[u8]>>| {
        if contents.data(mime).is_none() {
            contents.items.push((mime.to_owned(), data));
        }
    };
    for kind in kinds_of(items) {
        if kind == FILE_URL || kind == URL {
            continue;
        }
        let values: Vec<Option<Arc<[u8]>>> = items.iter().filter_map(|i| peek(item_imp(i), &kind)).collect();
        let data = match values.as_slice() {
            [] => continue,
            [first, ..] if kind != STRING || values.len() == 1 => first.clone(),
            many => many.iter().cloned().collect::<Option<Vec<_>>>().map(|lines| {
                let lines: Vec<&[u8]> = lines.iter().map(|l| &**l).collect();
                Arc::from(lines.join(&b'\n'))
            }),
        };
        for mime in types::mimes_for(&kind) {
            add(&mime, data.clone());
        }
    }
    let urls: Vec<Option<Arc<[u8]>>> =
        items.iter().filter_map(|i| [FILE_URL, URL].into_iter().find_map(|k| peek(item_imp(i), k))).collect();
    if !urls.is_empty() {
        match urls.into_iter().collect::<Option<Vec<_>>>() {
            Some(urls) => {
                let urls: Vec<String> = urls.iter().map(|u| String::from_utf8_lossy(u).into_owned()).collect();
                add(types::URI_LIST, Some(types::uri_list(urls.iter().map(String::as_str)).into_bytes().into()));
                let files: Vec<&str> = urls.iter().map(String::as_str).filter(|u| types::is_file_uri(u)).collect();
                if !files.is_empty() {
                    add(types::GNOME_FILES_MIME, Some(types::gnome_copied_files(files).into_bytes().into()));
                }
            }
            None => add(types::URI_LIST, None),
        }
    }
    contents
}

/// Items for another program's offer, change `generation`: one with every
/// type it offers, read when first asked for, and its URLs (a URL list, read
/// now, or with the drag) an item each, the first sharing the first item.
fn foreign_items(source: Source, board: BoardRef, generation: isize) -> Vec<Retained<NSPasteboardItem>> {
    let shared = clipboard::shared_for(source);
    let mimes = shared.foreign_mimes();
    let urls = match types::url_mime(&mimes) {
        Some(mime) => {
            shared.read_offer(mime, Some(generation)).map(|d| types::parse_urls(mime, &d)).unwrap_or_default()
        }
        None => Vec::new(),
    };
    let url_entry = |url: &str| (types::url_kind(url).to_owned(), Value::String(NSString::from_str(url)));
    let mut first: Vec<(String, Value)> = urls.first().map(|u| url_entry(u)).into_iter().collect();
    for mime in &mimes {
        let Some(kind) = types::type_for_mime(mime) else { continue };
        if !first.iter().any(|(k, _)| *k == kind) {
            first.push((kind.into_owned(), Value::Foreign { mime: mime.as_str().into(), source, generation }));
        }
    }
    let mut items = Vec::new();
    if !first.is_empty() {
        items.push(item::item_with(first, board));
    }
    items.extend(urls.iter().skip(1).map(|url| item::item_with(vec![url_entry(url)], board)));
    items
}

/// Every pasteboard with a name, by name. They're never freed: the registry
/// keeps a reference to each (a pointer, as pasteboards may be used from
/// any thread), which it gives up, unreleased, if the name is let go.
fn registry() -> MutexGuard<'static, HashMap<String, usize>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// `releaseGlobally`: forget `board`'s name, if it's still `board`'s.
fn unregister(name: &str, board: &NSPasteboardImpl) {
    let mut registry = registry();
    if registry.get(name).is_some_and(|&ptr| std::ptr::eq(ptr as *const NSPasteboardImpl, board)) {
        registry.remove(name);
    }
}

/// The pasteboard with this name, made the first time it's asked for.
pub(crate) fn named(name: &str) -> Retained<NSPasteboard> {
    let mut registry = registry();
    let found = registry.get(name).copied();
    let ptr = found.unwrap_or_else(|| {
        crate::load_shell::<NSPasteboard>();
        let kind = match name {
            GENERAL => Kind::System(Source::Selection),
            DRAG => Kind::System(Source::Drag),
            _ => Kind::Local,
        };
        // A system pasteboard's items belong to no change until they're
        // first asked for; a local one starts at 0, as on macOS.
        let generation = if kind == Kind::Local { 0 } else { -1 };
        let this = NSPasteboardImpl::alloc().set_ivars(PasteboardIvars {
            name: NSString::from_str(name),
            kind,
            held: Mutex::new(Held { generation, items: Vec::new(), version: 0, types: None }),
        });
        // SAFETY: NSObject's designated initializer.
        let pasteboard: Retained<NSPasteboardImpl> = unsafe { msg_send![super(this), init] };
        let ptr = Retained::into_raw(pasteboard) as usize;
        registry.insert(name.to_owned(), ptr);
        ptr
    });
    // SAFETY: the registry holds a reference to every pasteboard it names,
    // so this one is alive, and NSPasteboardImpl is the class NSPasteboard
    // names.
    unsafe { Retained::retain(ptr as *mut NSPasteboard) }.expect("registered pasteboards aren't null")
}

/// So the timeout's documentation stays next to its value.
const _: () = assert!(clipboard::READ_TIMEOUT.as_millis() == 200);
