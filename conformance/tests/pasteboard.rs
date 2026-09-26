//! NSPasteboard and NSPasteboardItem beyond strings: types, their old
//! names, data, items, lazy owners and data providers, and writing and
//! reading objects.
//!
//! Only pasteboards made with `pasteboardWithUniqueName` are used, never
//! the general one, which is the clipboard of whoever runs the tests.
//! Apple's pasteboard adds conversions of its own to what's written (TIFF
//! after PNG, and so on), so types are checked for containment and
//! relative order, never as whole lists.
//!
//! NSData is made through the runtime rather than with `NSData::with_bytes`,
//! so this file links on Linux before Sidestep's Foundation has NSData;
//! until it does, the checks that need data are skipped.

use std::cell::RefCell;
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, NSObject, ProtocolObject};
use objc2::{AnyThread, ClassType, define_class, msg_send};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardItemDataProvider, NSPasteboardType,
    NSPasteboardTypeFileURL, NSPasteboardTypeHTML, NSPasteboardTypePDF, NSPasteboardTypePNG, NSPasteboardTypeRTF,
    NSPasteboardTypeString, NSPasteboardTypeTIFF, NSPasteboardTypeURL, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSObjectProtocol, NSString};

use sidestep as _;

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

/// NSData holding `bytes`, if Foundation has NSData.
fn data(bytes: &[u8]) -> Option<Retained<NSData>> {
    let class = AnyClass::get(c"NSData")?;
    // SAFETY: dataWithBytes:length: copies `length` bytes from the pointer.
    Some(unsafe { msg_send![class, dataWithBytes: bytes.as_ptr().cast::<c_void>(), length: bytes.len()] })
}

fn board() -> Retained<NSPasteboard> {
    NSPasteboard::pasteboardWithUniqueName()
}

fn types_of(types: Option<Retained<NSArray<NSPasteboardType>>>) -> Vec<String> {
    types.map(|t| t.iter().map(|t| t.to_string()).collect()).unwrap_or_default()
}

fn types_array(types: &[&NSPasteboardType]) -> Retained<NSArray<NSPasteboardType>> {
    NSArray::from_slice(types)
}

/// `needle` is in `haystack`, and every earlier needle before it.
fn in_order(haystack: &[String], needles: &[&str]) -> bool {
    let mut from = 0;
    for needle in needles {
        match haystack[from..].iter().position(|t| t == needle) {
            Some(at) => from += at + 1,
            None => return false,
        }
    }
    true
}

fn string_type() -> &'static NSPasteboardType {
    // SAFETY: AppKit's constants live as long as the program.
    unsafe { NSPasteboardTypeString }
}

#[test]
fn names_and_types_have_their_values() {
    use objc2_app_kit::*;
    // SAFETY: AppKit's constants live as long as the program.
    let names = unsafe {
        [
            (NSPasteboardTypeString, "public.utf8-plain-text"),
            (NSPasteboardTypePDF, "com.adobe.pdf"),
            (NSPasteboardTypeTIFF, "public.tiff"),
            (NSPasteboardTypePNG, "public.png"),
            (NSPasteboardTypeRTF, "public.rtf"),
            (NSPasteboardTypeRTFD, "com.apple.flat-rtfd"),
            (NSPasteboardTypeHTML, "public.html"),
            (NSPasteboardTypeTabularText, "public.utf8-tab-separated-values-text"),
            (NSPasteboardTypeFont, "com.apple.cocoa.pasteboard.character-formatting"),
            (NSPasteboardTypeRuler, "com.apple.cocoa.pasteboard.paragraph-formatting"),
            (NSPasteboardTypeColor, "com.apple.cocoa.pasteboard.color"),
            (NSPasteboardTypeSound, "com.apple.cocoa.pasteboard.sound"),
            (NSPasteboardTypeMultipleTextSelection, "com.apple.cocoa.pasteboard.multiple-text-selection"),
            (NSPasteboardTypeTextFinderOptions, "com.apple.cocoa.pasteboard.find-panel-search-options"),
            (NSPasteboardTypeURL, "public.url"),
            (NSPasteboardTypeFileURL, "public.file-url"),
            (NSPasteboardNameGeneral, "Apple CFPasteboard general"),
            (NSPasteboardNameFont, "Apple CFPasteboard font"),
            (NSPasteboardNameRuler, "Apple CFPasteboard ruler"),
            (NSPasteboardNameFind, "Apple CFPasteboard find"),
            (NSPasteboardNameDrag, "Apple CFPasteboard drag"),
        ]
    };
    for (name, value) in names {
        assert_eq!(name.to_string(), value);
    }
    #[allow(deprecated)]
    // SAFETY: as above.
    let legacy = unsafe {
        [
            (NSStringPboardType, "NSStringPboardType"),
            (NSFilenamesPboardType, "NSFilenamesPboardType"),
            (NSURLPboardType, "Apple URL pasteboard type"),
            (NSTIFFPboardType, "NeXT TIFF v4.0 pasteboard type"),
            (NSHTMLPboardType, "Apple HTML pasteboard type"),
        ]
    };
    for (name, value) in legacy {
        assert_eq!(name.to_string(), value);
    }
}

#[test]
fn a_fresh_board_is_empty() {
    let pb = board();
    assert_eq!(pb.changeCount(), 0);
    assert!(types_of(pb.types()).is_empty());
    assert_eq!(pb.pasteboardItems().map_or(0, |i| i.count()), 0);
    assert!(pb.stringForType(string_type()).is_none());
    // Writing needs no clearContents first, and doesn't change the count.
    assert!(pb.setString_forType(&s("before"), string_type()));
    assert_eq!(pb.stringForType(string_type()).unwrap().to_string(), "before");
    assert_eq!(pb.changeCount(), 0);
}

#[test]
fn change_counts() {
    let pb = board();
    assert_eq!(pb.clearContents(), 1);
    assert_eq!(pb.changeCount(), 1);
    assert!(types_of(pb.types()).is_empty());
    assert!(pb.setString_forType(&s("x"), string_type()));
    assert_eq!(pb.changeCount(), 1);
    if let Some(d) = data(b"\x89PNG") {
        // SAFETY: the constant lives as long as the program.
        assert!(pb.setData_forType(Some(&d), unsafe { NSPasteboardTypePNG }));
        assert_eq!(pb.changeCount(), 1);
    }
    // SAFETY: as above.
    let html = unsafe { NSPasteboardTypeHTML };
    // SAFETY: the owner is nil.
    assert_eq!(unsafe { pb.declareTypes_owner(&types_array(&[html]), None) }, 2);
    assert_eq!(pb.changeCount(), 2);
    // SAFETY: as above.
    assert_eq!(unsafe { pb.addTypes_owner(&types_array(&[string_type()]), None) }, 2);
    assert_eq!(pb.changeCount(), 2);
    assert_eq!(pb.prepareForNewContentsWithOptions(NSPasteboardContentsOptions::empty()), 3);
    assert_eq!(pb.changeCount(), 3);
}

#[test]
fn types_are_listed_in_the_order_written_with_their_old_names() {
    let pb = board();
    pb.clearContents();
    assert!(pb.setString_forType(&s("héllo"), string_type()));
    let custom = s("com.example.sidestep-custom");
    assert!(pb.setString_forType(&s("custom"), &custom));
    // SAFETY: the constant lives as long as the program.
    assert!(pb.setString_forType(&s("<b>h</b>"), unsafe { NSPasteboardTypeHTML }));
    let types = types_of(pb.types());
    assert!(
        in_order(
            &types,
            &["public.utf8-plain-text", "NSStringPboardType", "com.example.sidestep-custom", "public.html"]
        ),
        "{types:?}"
    );
    assert!(in_order(&types, &["public.html", "Apple HTML pasteboard type"]), "{types:?}");
    // A type written again keeps its place and takes the new value.
    assert!(pb.setString_forType(&s("again"), string_type()));
    assert_eq!(pb.stringForType(string_type()).unwrap().to_string(), "again");
    let again = types_of(pb.types());
    assert_eq!(again.iter().filter(|t| *t == "public.utf8-plain-text").count(), 1);
    assert!(in_order(&again, &["public.utf8-plain-text", "com.example.sidestep-custom"]), "{again:?}");
}

#[test]
fn old_names_read_and_write_the_new_types() {
    use objc2_app_kit::*;
    #[allow(deprecated)]
    // SAFETY: AppKit's constants live as long as the program.
    let pairs = unsafe {
        [
            (NSStringPboardType, NSPasteboardTypeString),
            (NSTIFFPboardType, NSPasteboardTypeTIFF),
            (NSRTFPboardType, NSPasteboardTypeRTF),
            (NSRTFDPboardType, NSPasteboardTypeRTFD),
            (NSHTMLPboardType, NSPasteboardTypeHTML),
            (NSPDFPboardType, NSPasteboardTypePDF),
            (NSTabularTextPboardType, NSPasteboardTypeTabularText),
            (NSFontPboardType, NSPasteboardTypeFont),
            (NSRulerPboardType, NSPasteboardTypeRuler),
            (NSColorPboardType, NSPasteboardTypeColor),
            (NSMultipleTextSelectionPboardType, NSPasteboardTypeMultipleTextSelection),
        ]
    };
    let png_old = s("Apple PNG pasteboard type");
    // SAFETY: as above.
    let pairs = pairs.into_iter().chain([(&*png_old, unsafe { NSPasteboardTypePNG })]);
    for (old, new) in pairs {
        let pb = board();
        pb.clearContents();
        assert!(pb.setString_forType(&s("new"), new));
        assert_eq!(pb.stringForType(old).map(|v| v.to_string()).as_deref(), Some("new"), "{old}");
        let types = types_of(pb.types());
        assert!(in_order(&types, &[&new.to_string(), &old.to_string()]), "{types:?}");
        pb.clearContents();
        assert!(pb.setString_forType(&s("old"), old));
        assert_eq!(pb.stringForType(new).map(|v| v.to_string()).as_deref(), Some("old"), "{old}");
    }
    // The old URL type holds a property list, not a URL.
    #[allow(deprecated)]
    // SAFETY: as above.
    let (old_url, url) = unsafe { (NSURLPboardType, NSPasteboardTypeURL) };
    let pb = board();
    pb.clearContents();
    assert!(pb.setString_forType(&s("https://example.com/"), old_url));
    assert!(pb.stringForType(url).is_none());
}

#[test]
fn available_types_follow_the_callers_order() {
    let pb = board();
    pb.clearContents();
    assert!(pb.setString_forType(&s("text"), string_type()));
    // SAFETY: the constants live as long as the program.
    let (tiff, png, pdf, html) =
        unsafe { (NSPasteboardTypeTIFF, NSPasteboardTypePNG, NSPasteboardTypePDF, NSPasteboardTypeHTML) };
    let found = pb.availableTypeFromArray(&types_array(&[tiff, html, string_type()]));
    assert_eq!(found.unwrap().to_string(), "public.utf8-plain-text");
    assert!(pb.availableTypeFromArray(&types_array(&[pdf, html])).is_none());
    assert!(pb.setString_forType(&s("<i>h</i>"), html));
    let found = pb.availableTypeFromArray(&types_array(&[string_type(), html]));
    assert_eq!(found.unwrap().to_string(), "public.utf8-plain-text");
    let found = pb.availableTypeFromArray(&types_array(&[html, string_type()]));
    assert_eq!(found.unwrap().to_string(), "public.html");
    #[allow(deprecated)]
    // SAFETY: as above.
    let old = unsafe { objc2_app_kit::NSStringPboardType };
    let found = pb.availableTypeFromArray(&types_array(&[png, old]));
    assert_eq!(found.unwrap().to_string(), "NSStringPboardType");
}

#[test]
fn data_and_strings_convert_as_utf8() {
    let pb = board();
    pb.clearContents();
    assert!(pb.setString_forType(&s("héllo"), string_type()));
    let Some(png_bytes) = data(b"\x89PNG\r\n") else { return };
    let text = pb.dataForType(string_type()).expect("a string reads as data");
    assert_eq!(text.to_vec(), "héllo".as_bytes());
    // SAFETY: the constants live as long as the program.
    let (png, html) = unsafe { (NSPasteboardTypePNG, NSPasteboardTypeHTML) };
    assert!(pb.setData_forType(Some(&png_bytes), png));
    assert_eq!(pb.dataForType(png).unwrap().to_vec(), b"\x89PNG\r\n");
    assert!(pb.stringForType(png).is_none());
    assert!(in_order(&types_of(pb.types()), &["public.utf8-plain-text", "public.png"]));
    // Data read as a string, when it's UTF-8.
    assert!(pb.setData_forType(Some(&data("<p>ü</p>".as_bytes()).unwrap()), html));
    assert_eq!(pb.stringForType(html).unwrap().to_string(), "<p>ü</p>");
}

#[test]
fn declared_types_read_nothing_until_set() {
    let pb = board();
    // SAFETY: the constants live as long as the program.
    let (html, pdf, rtf) = unsafe { (NSPasteboardTypeHTML, NSPasteboardTypePDF, NSPasteboardTypeRTF) };
    // SAFETY: the owner is nil.
    unsafe { pb.declareTypes_owner(&types_array(&[html, string_type()]), None) };
    let types = types_of(pb.types());
    assert!(in_order(&types, &["public.html", "public.utf8-plain-text"]), "{types:?}");
    assert!(pb.stringForType(string_type()).is_none());
    assert!(pb.setString_forType(&s("<b>x</b>"), html));
    assert_eq!(pb.stringForType(html).unwrap().to_string(), "<b>x</b>");
    // Undeclared types can still be written, after the declared ones.
    assert!(pb.setString_forType(&s("pdf"), pdf));
    assert!(in_order(&types_of(pb.types()), &["public.html", "public.utf8-plain-text", "com.adobe.pdf"]));
    // SAFETY: the owner is nil.
    unsafe { pb.addTypes_owner(&types_array(&[rtf]), None) };
    assert!(types_of(pb.types()).iter().any(|t| t == "public.rtf"));
    assert!(pb.stringForType(rtf).is_none());
}

thread_local!(static ASKED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn asked() -> Vec<String> {
    ASKED.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

define_class!(
    // Provides the types it's asked for when they're read.
    #[unsafe(super(NSObject))]
    #[name = "ConformanceLazyOwner"]
    struct LazyOwner;

    impl LazyOwner {
        #[unsafe(method(pasteboard:provideDataForType:))]
        fn provide(&self, board: &NSPasteboard, kind: &NSPasteboardType) {
            ASKED.with(|a| a.borrow_mut().push(kind.to_string()));
            board.setString_forType(&s(&format!("provided {kind}")), kind);
        }
    }

    unsafe impl NSObjectProtocol for LazyOwner {}
);

impl LazyOwner {
    fn new() -> Retained<Self> {
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(Self::alloc().set_ivars(())), init] }
    }
}

#[test]
fn owners_provide_declared_types_when_read() {
    let pb = board();
    let owner = LazyOwner::new();
    // SAFETY: the constant lives as long as the program; the owner answers
    // pasteboard:provideDataForType:.
    let html = unsafe { NSPasteboardTypeHTML };
    unsafe { pb.declareTypes_owner(&types_array(&[string_type(), html]), Some(&owner)) };
    assert!(asked().is_empty());
    assert_eq!(pb.stringForType(string_type()).unwrap().to_string(), "provided public.utf8-plain-text");
    assert_eq!(asked(), ["public.utf8-plain-text"]);
    // Asked once: what it provided stays.
    assert_eq!(pb.stringForType(string_type()).unwrap().to_string(), "provided public.utf8-plain-text");
    assert!(asked().is_empty());
    assert_eq!(pb.stringForType(html).unwrap().to_string(), "provided public.html");
    assert_eq!(asked(), ["public.html"]);
}

define_class!(
    // Provides an item's types when they're read.
    #[unsafe(super(NSObject))]
    #[name = "ConformanceItemProvider"]
    struct Provider;

    unsafe impl NSObjectProtocol for Provider {}

    unsafe impl NSPasteboardItemDataProvider for Provider {
        #[unsafe(method(pasteboard:item:provideDataForType:))]
        fn provide(&self, _board: Option<&NSPasteboard>, item: &NSPasteboardItem, kind: &NSPasteboardType) {
            ASKED.with(|a| a.borrow_mut().push(kind.to_string()));
            item.setString_forType(&s("from the provider"), kind);
        }
    }
);

#[test]
fn items_hold_types_and_values() {
    let item = NSPasteboardItem::new();
    assert!(item.types().is_empty());
    assert!(item.setString_forType(&s("one"), string_type()));
    // SAFETY: the constant lives as long as the program.
    let html = unsafe { NSPasteboardTypeHTML };
    assert!(item.setString_forType(&s("<b>two</b>"), html));
    let types: Vec<String> = item.types().iter().map(|t| t.to_string()).collect();
    assert!(in_order(&types, &["public.utf8-plain-text", "public.html"]), "{types:?}");
    assert_eq!(item.stringForType(string_type()).unwrap().to_string(), "one");
    // SAFETY: as above.
    assert!(item.stringForType(unsafe { NSPasteboardTypePDF }).is_none());
    if let Some(d) = data(b"bytes") {
        // SAFETY: as above.
        let (png, rtf) = unsafe { (NSPasteboardTypePNG, NSPasteboardTypeRTF) };
        assert!(item.setData_forType(&d, png));
        assert_eq!(item.dataForType(png).unwrap().to_vec(), b"bytes");
        assert_eq!(item.dataForType(string_type()).unwrap().to_vec(), b"one");
        // Data reads as a string when it's UTF-8.
        assert_eq!(item.stringForType(png).unwrap().to_string(), "bytes");
        assert!(item.setData_forType(&data(&[0xff, 0xfe, 0x41]).unwrap(), rtf));
        assert!(item.stringForType(rtf).is_none());
    }
    // On a pasteboard, it answers which types it has.
    let pb = board();
    pb.clearContents();
    let objects: Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
        NSArray::from_retained_slice(&[ProtocolObject::from_retained(item.clone())]);
    assert!(pb.writeObjects(&objects));
    let found = item.availableTypeFromArray(&types_array(&[html, string_type()]));
    assert_eq!(found.unwrap().to_string(), "public.html");
}

#[test]
fn items_are_live_until_their_board_is_cleared() {
    let pb = board();
    pb.clearContents();
    let item = NSPasteboardItem::new();
    assert!(item.setString_forType(&s("kept"), string_type()));
    let objects: Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
        NSArray::from_retained_slice(&[ProtocolObject::from_retained(item.clone())]);
    assert!(pb.writeObjects(&objects));
    // The board hands back the item written, and shows what's written to it.
    let items = pb.pasteboardItems().expect("items");
    assert!(std::ptr::eq(&*items.objectAtIndex(0), &*item));
    // SAFETY: the constant lives as long as the program.
    let tabular = unsafe { objc2_app_kit::NSPasteboardTypeTabularText };
    assert!(item.setString_forType(&s("a\tb"), tabular));
    assert_eq!(pb.stringForType(tabular).unwrap().to_string(), "a\tb");
    // Its own writes show on its item.
    // SAFETY: as above.
    let html = unsafe { NSPasteboardTypeHTML };
    assert!(pb.setString_forType(&s("<i>"), html));
    assert!(item.types().iter().any(|t| t.to_string() == "public.html"));
    // Cleared, the item is empty.
    pb.clearContents();
    assert!(item.types().is_empty());
    assert!(item.stringForType(string_type()).is_none());
}

#[test]
fn types_conform() {
    let pb = board();
    pb.clearContents();
    // SAFETY: the constants live as long as the program.
    let (html, file_url, url) = unsafe { (NSPasteboardTypeHTML, NSPasteboardTypeFileURL, NSPasteboardTypeURL) };
    assert!(pb.setString_forType(&s("<b>"), html));
    let can = |kinds: &[&str]| {
        let kinds: Vec<Retained<NSString>> = kinds.iter().map(|k| s(k)).collect();
        pb.canReadItemWithDataConformingToTypes(&NSArray::from_retained_slice(&kinds))
    };
    assert!(can(&["public.html"]));
    assert!(can(&["public.text"]));
    assert!(can(&["public.data"]));
    assert!(!can(&["public.image"]));
    assert!(!can(&["public.utf8-plain-text"]));
    pb.clearContents();
    assert!(pb.setString_forType(&s("file:///tmp/x"), file_url));
    assert!(can(&["public.url"]));
    assert!(!can(&["public.text"]));
    // Reading takes the exact type.
    assert!(pb.stringForType(url).is_none());
    if let Some(d) = data(b"png") {
        pb.clearContents();
        // SAFETY: as above.
        assert!(pb.setData_forType(Some(&d), unsafe { NSPasteboardTypePNG }));
        assert!(can(&["public.image"]));
        assert!(!can(&["public.text"]));
    }
}

#[test]
fn writing_items_makes_one_item_each() {
    let pb = board();
    pb.clearContents();
    let (a, b) = (NSPasteboardItem::new(), NSPasteboardItem::new());
    // SAFETY: the constant lives as long as the program.
    let file_url = unsafe { NSPasteboardTypeFileURL };
    assert!(a.setString_forType(&s("file:///tmp/a"), file_url));
    assert!(b.setString_forType(&s("file:///tmp/b"), file_url));
    let objects: Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
        NSArray::from_retained_slice(&[ProtocolObject::from_retained(a), ProtocolObject::from_retained(b)]);
    assert!(pb.writeObjects(&objects));
    let items = pb.pasteboardItems().expect("items");
    assert_eq!(items.count(), 2);
    assert_eq!(pb.indexOfPasteboardItem(&NSPasteboardItem::new()), isize::MAX as usize);
    for (i, expected) in ["file:///tmp/a", "file:///tmp/b"].iter().enumerate() {
        let item = items.objectAtIndex(i);
        let types: Vec<String> = item.types().iter().map(|t| t.to_string()).collect();
        assert_eq!(types, ["public.file-url"]);
        assert_eq!(item.stringForType(file_url).unwrap().to_string(), *expected);
        assert_eq!(pb.indexOfPasteboardItem(&item), i);
    }
    // The board reads its first item.
    assert_eq!(pb.stringForType(file_url).unwrap().to_string(), "file:///tmp/a");
    assert!(types_of(pb.types()).iter().any(|t| t == "public.file-url"));
    let found = pb.availableTypeFromArray(&types_array(&[string_type(), file_url]));
    assert_eq!(found.unwrap().to_string(), "public.file-url");
    assert!(pb.canReadItemWithDataConformingToTypes(&NSArray::from_slice(&[file_url])));
    assert!(!pb.canReadItemWithDataConformingToTypes(&NSArray::from_slice(&[string_type()])));
    // Writing after clearing starts over.
    pb.clearContents();
    assert_eq!(pb.pasteboardItems().map_or(0, |i| i.count()), 0);
}

#[test]
fn board_writes_go_to_the_first_item() {
    let pb = board();
    pb.clearContents();
    assert!(pb.setString_forType(&s("text"), string_type()));
    // SAFETY: the constant lives as long as the program.
    assert!(pb.setString_forType(&s("<b>t</b>"), unsafe { NSPasteboardTypeHTML }));
    let items = pb.pasteboardItems().expect("items");
    assert_eq!(items.count(), 1);
    let item = items.objectAtIndex(0);
    assert_eq!(item.stringForType(string_type()).unwrap().to_string(), "text");
    let types: Vec<String> = item.types().iter().map(|t| t.to_string()).collect();
    assert!(in_order(&types, &["public.utf8-plain-text", "public.html"]), "{types:?}");
}

#[test]
fn writing_strings() {
    let pb = board();
    pb.clearContents();
    let objects: Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> = NSArray::from_retained_slice(&[
        ProtocolObject::from_retained(s("first")),
        ProtocolObject::from_retained(s("second")),
    ]);
    assert!(pb.writeObjects(&objects));
    let items = pb.pasteboardItems().expect("items");
    assert_eq!(items.count(), 2);
    assert_eq!(items.objectAtIndex(1).stringForType(string_type()).unwrap().to_string(), "second");
    // The board's text is every item's, a line each.
    assert_eq!(pb.stringForType(string_type()).unwrap().to_string(), "first\nsecond");
    if data(b"").is_some() {
        assert_eq!(pb.dataForType(string_type()).unwrap().to_vec(), b"first\nsecond");
    }
    // Other types come from the first item that has them.
    // SAFETY: the constant lives as long as the program.
    let html = unsafe { NSPasteboardTypeHTML };
    assert!(items.objectAtIndex(1).setString_forType(&s("<b>2</b>"), html));
    assert_eq!(pb.stringForType(html).unwrap().to_string(), "<b>2</b>");
    // Read back as strings.
    let classes = NSArray::from_slice(&[NSString::class()]);
    // SAFETY: NSString reads from pasteboards; no options.
    let read = unsafe { pb.readObjectsForClasses_options(&classes, None) }.expect("objects");
    let read: Vec<String> =
        (0..read.count()).map(|i| read.objectAtIndex(i).downcast::<NSString>().unwrap().to_string()).collect();
    assert_eq!(read, ["first", "second"]);
    // SAFETY: as above.
    assert!(unsafe { pb.canReadObjectForClasses_options(&classes, None) });
}

#[test]
fn item_data_providers_are_asked_when_read() {
    let pb = board();
    pb.clearContents();
    let item = NSPasteboardItem::new();
    // SAFETY: NSObject's designated initializer.
    let provider: Retained<Provider> = unsafe { msg_send![super(Provider::alloc().set_ivars(())), init] };
    // SAFETY: the constant lives as long as the program.
    let html = unsafe { NSPasteboardTypeHTML };
    assert!(item.setDataProvider_forTypes(ProtocolObject::from_ref(&*provider), &types_array(&[html])));
    let objects: Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
        NSArray::from_retained_slice(&[ProtocolObject::from_retained(item)]);
    assert!(pb.writeObjects(&objects));
    asked();
    assert!(types_of(pb.types()).iter().any(|t| t == "public.html"));
    assert!(asked().is_empty());
    assert_eq!(pb.stringForType(html).unwrap().to_string(), "from the provider");
    assert_eq!(asked(), ["public.html"]);
}

#[test]
fn property_lists() {
    let pb = board();
    pb.clearContents();
    let kind = s("com.example.sidestep-list");
    let list = NSArray::from_retained_slice(&[s("a"), s("b")]);
    // SAFETY: an array of strings is a property list.
    assert!(unsafe { pb.setPropertyList_forType(&list, &kind) });
    let back = pb.propertyListForType(&kind).expect("a property list");
    let back = back.downcast::<NSArray>().expect("an array");
    let back: Vec<String> =
        (0..back.count()).map(|i| back.objectAtIndex(i).downcast::<NSString>().unwrap().to_string()).collect();
    assert_eq!(back, ["a", "b"]);
    assert!(pb.stringForType(&kind).is_none());
    // A string reads as a property list.
    assert!(pb.setString_forType(&s("plain"), string_type()));
    let back = pb.propertyListForType(string_type()).expect("a property list");
    assert_eq!(back.downcast::<NSString>().unwrap().to_string(), "plain");
}

fn strings(list: Retained<objc2::runtime::AnyObject>) -> Vec<String> {
    let array = list.downcast::<NSArray>().expect("an array");
    (0..array.count()).map(|i| array.objectAtIndex(i).downcast::<NSString>().unwrap().to_string()).collect()
}

#[test]
fn file_names_are_the_file_urls_paths() {
    #[allow(deprecated)]
    // SAFETY: AppKit's constants live as long as the program.
    let (filenames, old_url, file_url) =
        unsafe { (objc2_app_kit::NSFilenamesPboardType, objc2_app_kit::NSURLPboardType, NSPasteboardTypeFileURL) };
    let pb = board();
    pb.clearContents();
    let (a, b) = (NSPasteboardItem::new(), NSPasteboardItem::new());
    assert!(a.setString_forType(&s("file:///tmp/a%20b"), file_url));
    assert!(b.setString_forType(&s("file:///tmp/c"), file_url));
    let objects: Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
        NSArray::from_retained_slice(&[ProtocolObject::from_retained(a), ProtocolObject::from_retained(b)]);
    assert!(pb.writeObjects(&objects));
    assert!(in_order(&types_of(pb.types()), &["public.file-url", "NSFilenamesPboardType"]));
    assert_eq!(strings(pb.propertyListForType(filenames).expect("paths")), ["/tmp/a b", "/tmp/c"]);
    assert_eq!(strings(pb.propertyListForType(old_url).expect("a URL")), ["file:///tmp/a%20b", ""]);
    assert_eq!(pb.availableTypeFromArray(&types_array(&[filenames])).unwrap().to_string(), "NSFilenamesPboardType");
    // Paths written by their old name become file URLs, an item each.
    let pb = board();
    pb.clearContents();
    let paths = NSArray::from_retained_slice(&[s("/tmp/a b"), s("/tmp/c")]);
    // SAFETY: an array of strings is a property list.
    assert!(unsafe { pb.setPropertyList_forType(&paths, filenames) });
    let items = pb.pasteboardItems().expect("items");
    assert_eq!(items.count(), 2);
    assert_eq!(items.objectAtIndex(0).stringForType(file_url).unwrap().to_string(), "file:///tmp/a%20b");
    assert_eq!(items.objectAtIndex(1).stringForType(file_url).unwrap().to_string(), "file:///tmp/c");
}

#[test]
fn boards_by_name_are_shared() {
    let name = s("org.sidestep.conformance.named");
    let a = NSPasteboard::pasteboardWithName(&name);
    let b = NSPasteboard::pasteboardWithName(&name);
    assert!(std::ptr::eq(&*a, &*b));
    a.clearContents();
    assert!(a.setString_forType(&s("shared"), string_type()));
    assert_eq!(b.stringForType(string_type()).unwrap().to_string(), "shared");
    assert_eq!(a.changeCount(), b.changeCount());
    let _: () = unsafe { msg_send![&*a, releaseGlobally] };
}

/// Microseconds a run of `f` takes, the median of seven.
fn median_us(mut f: impl FnMut()) -> f64 {
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = std::time::Instant::now();
            f();
            start.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    runs[3]
}

/// What a copy and a paste cost on a pasteboard of this process's, for
/// comparing Apple's with Sidestep's: `cargo test --release -p
/// sidestep-conformance --test pasteboard timing -- --ignored --nocapture`.
#[test]
#[ignore]
fn timing() {
    let pb = board();
    // SAFETY: the constants live as long as the program.
    let (html, png, file_url) = unsafe { (NSPasteboardTypeHTML, NSPasteboardTypePNG, NSPasteboardTypeFileURL) };
    let text = s("a line of text to copy and paste");
    let markup = s("<p>a line of <b>text</b></p>");
    const N: usize = 1000;
    let copy = median_us(|| {
        for _ in 0..N {
            pb.clearContents();
            pb.setString_forType(&text, string_type());
            pb.setString_forType(&markup, html);
        }
    });
    let paste = median_us(|| {
        for _ in 0..N {
            std::hint::black_box(pb.stringForType(string_type()));
        }
    });
    let wanted = types_array(&[file_url, png, html, string_type()]);
    let available = median_us(|| {
        for _ in 0..N {
            std::hint::black_box(pb.availableTypeFromArray(&wanted));
        }
    });
    let types = median_us(|| {
        for _ in 0..N {
            std::hint::black_box(pb.types());
        }
    });
    let urls: Vec<Retained<NSString>> = (0..100).map(|i| s(&format!("file:///tmp/file{i}"))).collect();
    let files = median_us(|| {
        pb.clearContents();
        // Items belong to one pasteboard for good, so each run makes its own.
        let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = urls
            .iter()
            .map(|url| {
                let item = NSPasteboardItem::new();
                item.setString_forType(url, file_url);
                ProtocolObject::from_retained(item)
            })
            .collect();
        pb.writeObjects(&NSArray::from_retained_slice(&objects));
        for item in pb.pasteboardItems().unwrap().iter() {
            std::hint::black_box(item.stringForType(file_url));
        }
    });
    println!("copy (clear, text, HTML): {:.2} µs", copy / N as f64);
    println!("paste (stringForType:): {:.3} µs", paste / N as f64);
    println!("availableTypeFromArray: (4 types): {:.3} µs", available / N as f64);
    println!("types: {:.3} µs", types / N as f64);
    println!("100 file items written and read: {files:.1} µs");
}
