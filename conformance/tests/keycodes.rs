//! The table Sidestep uses for NSEvent keyCode on Linux, checked against
//! macOS: every row that macOS can be asked about must match what Apple's
//! frameworks report for that virtual key code. On Linux this checks the
//! table's consistency.

include!("../../crates/sidestep-appkit/src/keycode_table.rs");

/// The PC keys macOS treats as F13, F14 and F15 share those keys' codes.
const SHARED: [u16; 3] = [105, 107, 113];

#[test]
fn table_is_consistent() {
    let mut evdev: Vec<u16> = KEY_CODES.iter().map(|k| k.0).collect();
    let sorted = evdev.windows(2).all(|w| w[0] < w[1]);
    evdev.dedup();
    assert!(sorted && evdev.len() == KEY_CODES.len(), "evdev codes are sorted and unique");
    for (i, a) in KEY_CODES.iter().enumerate() {
        assert!(a.1 < 128, "macOS virtual key codes are 7-bit");
        let again = KEY_CODES[i + 1..].iter().filter(|b| b.1 == a.1).count();
        assert!(again == 0 || SHARED.contains(&a.1), "macOS code {} appears twice", a.1);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CString, c_void};
    use std::ptr::null;

    use objc2::encode::{Encode, Encoding, RefEncode};
    use objc2::rc::Retained;
    use objc2::{ClassType, msg_send};
    use objc2_app_kit::NSEvent;

    use super::{KEY_CODES, Seen};

    type CFTypeRef = *const c_void;

    /// A CGEventRef, encoded as the method expects.
    #[repr(transparent)]
    struct CGEventRef(CFTypeRef);
    // SAFETY: a pointer to the opaque __CGEvent struct.
    unsafe impl Encode for CGEventRef {
        const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("__CGEvent", &[]));
    }
    // SAFETY: as above.
    unsafe impl RefEncode for CGEventRef {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const i8, encoding: u32) -> CFTypeRef;
        fn CFDictionaryCreate(
            alloc: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFTypeRef;
        fn CFArrayGetCount(array: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
        fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateKeyboardEvent(source: CFTypeRef, key: u16, down: bool) -> CFTypeRef;
        fn CGEventGetType(event: CFTypeRef) -> u32;
        fn CGEventGetFlags(event: CFTypeRef) -> u64;
    }

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        static kTISPropertyInputSourceID: CFTypeRef;
        static kTISPropertyUnicodeKeyLayoutData: CFTypeRef;
        fn TISCreateInputSourceList(properties: CFTypeRef, include_all: bool) -> CFTypeRef;
        fn TISGetInputSourceProperty(source: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn LMGetKbdType() -> u8;
        fn UCKeyTranslate(
            layout: *const u8,
            key: u16,
            action: u16,
            modifiers: u32,
            keyboard_type: u32,
            options: u32,
            dead_key_state: *mut u32,
            max_length: usize,
            length: *mut usize,
            chars: *mut u16,
        ) -> i32;
    }

    /// The US keyboard layout's translation data.
    fn us_layout() -> *const u8 {
        let id = CString::new("com.apple.keylayout.US").unwrap();
        // SAFETY: CoreFoundation and Text Input Sources calls with valid
        // arguments; the objects are leaked for the test's duration.
        unsafe {
            let key = kTISPropertyInputSourceID;
            let value = CFStringCreateWithCString(null(), id.as_ptr(), 0x0800_0100);
            let filter = CFDictionaryCreate(
                null(),
                &key,
                &value,
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let sources = TISCreateInputSourceList(filter, true);
            assert!(CFArrayGetCount(sources) > 0, "the US layout is installed");
            let data = TISGetInputSourceProperty(CFArrayGetValueAtIndex(sources, 0), kTISPropertyUnicodeKeyLayoutData);
            CFDataGetBytePtr(data)
        }
    }

    /// What the US layout types for a key with no modifiers.
    fn us_chars(layout: *const u8, code: u16) -> String {
        let mut dead = 0u32;
        let mut chars = [0u16; 8];
        let mut len = 0usize;
        // kUCKeyActionDisplay, no modifiers, no dead keys.
        // SAFETY: a valid layout and output buffer.
        let status = unsafe {
            UCKeyTranslate(layout, code, 3, 0, LMGetKbdType() as u32, 1, &mut dead, 8, &mut len, chars.as_mut_ptr())
        };
        assert_eq!(status, 0);
        String::from_utf16(&chars[..len]).unwrap()
    }

    #[test]
    fn observed_rows_match_macos() {
        const FLAGS_CHANGED: u32 = 12;
        const NUMERIC_PAD: u64 = 0x20_0000;
        // Flags that say which keys are down; the rest describe the event.
        const KEY_FLAGS: u64 = 0x00ff_ffff;
        let layout = us_layout();
        let mut checked = 0;
        for &(evdev, code, ref seen) in KEY_CODES {
            // SAFETY: creates a synthetic event, which isn't posted.
            let event = unsafe { CGEventCreateKeyboardEvent(null(), code, true) };
            let (kind, flags) = unsafe { (CGEventGetType(event), CGEventGetFlags(event) & KEY_FLAGS) };
            let what = format!("evdev {evdev} -> macOS {code}");
            match seen {
                Seen::Char(text) => {
                    assert_eq!(us_chars(layout, code), *text, "{what}");
                    assert_eq!(flags & NUMERIC_PAD, 0, "{what} is outside the keypad");
                }
                Seen::Keypad(text) => {
                    assert_eq!(us_chars(layout, code), *text, "{what}");
                    assert_ne!(flags & NUMERIC_PAD, 0, "{what} is on the keypad");
                }
                Seen::Function(ch) => {
                    // SAFETY: eventWithCGEvent: takes a CGEventRef.
                    let ns: Option<Retained<NSEvent>> =
                        unsafe { msg_send![NSEvent::class(), eventWithCGEvent: CGEventRef(event)] };
                    let chars = ns.and_then(|e| e.charactersIgnoringModifiers()).expect("characters").to_string();
                    assert_eq!(chars.encode_utf16().collect::<Vec<_>>(), [*ch], "{what}");
                }
                Seen::Modifier(expected) => {
                    assert_eq!(kind, FLAGS_CHANGED, "{what} is a modifier");
                    assert_eq!(flags, *expected, "{what}");
                }
                Seen::Conventional => continue,
            }
            checked += 1;
        }
        assert!(checked > 100, "{checked} rows checked");
    }
}
