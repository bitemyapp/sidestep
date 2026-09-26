//! `NSPasteboard`. The general pasteboard is the system clipboard, carried
//! by the render thread (see [`crate::clipboard`]); pasteboards made by
//! name or with a unique name live in this process only.
//!
//! What a program writes it can read back at once. What another program
//! copied is read ahead, as text, when it's copied, so `stringForType:`
//! doesn't wait; for other types, or if the text is still arriving,
//! `stringForType:` waits for at most `clipboard::READ_TIMEOUT` (200 ms)
//! and returns nil if the data hasn't come by then.
//!
//! Pasteboards are safe to use from any thread, as on macOS: their state
//! is behind a mutex, and named pasteboards (the general one included) are
//! never freed, as `releaseGlobally` is the only way AppKit frees them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSCopying, NSString};

use crate::clipboard::{self, Contents, mime_types, shared};

/// The general pasteboard's name, `NSPasteboardNameGeneral`.
const GENERAL: &str = "Apple CFPasteboard general";

struct Written {
    /// Type and string, in the order written.
    items: Vec<(String, Retained<NSString>)>,
    /// The change count the writes belong to.
    generation: isize,
}

/// Any thread may message a pasteboard: the name is an immutable string
/// made here, and the rest is behind the mutex.
pub(crate) struct PasteboardIvars {
    name: Retained<NSString>,
    general: bool,
    written: Mutex<Written>,
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
            if self.ivars().general {
                shared().change_count()
            } else {
                self.lock().generation
            }
        }

        #[unsafe(method(clearContents))]
        fn clear_contents(&self) -> isize {
            let mut written = self.lock();
            written.items.clear();
            written.generation = if self.ivars().general { shared().bump() } else { written.generation + 1 };
            let generation = written.generation;
            drop(written);
            if self.ivars().general {
                shared().offer(None);
            }
            generation
        }

        #[unsafe(method(setString:forType:))]
        fn set_string_for_type(&self, string: &NSString, kind: &NSString) -> bool {
            let general = self.ivars().general;
            let mut written = self.lock();
            if general && written.generation != shared().change_count() {
                // Another program copied since this one last wrote: start
                // over, as AppKit expects a clearContents first.
                written.items.clear();
                written.generation = shared().bump();
            }
            let kind = kind.to_string();
            written.items.retain(|(k, _)| *k != kind);
            written.items.push((kind, string.copy()));
            let contents = general.then(|| offered(&written.items));
            drop(written);
            if let Some(contents) = contents {
                shared().offer(Some(contents));
            }
            true
        }

        #[unsafe(method_id(stringForType:))]
        fn string_for_type(&self, kind: &NSString) -> Option<Retained<NSString>> {
            string_for_type(self, &kind.to_string())
        }

        #[unsafe(method(releaseGlobally))]
        fn release_globally(&self) {}
    }

    unsafe impl NSObjectProtocol for NSPasteboardImpl {}
);

impl NSPasteboardImpl {
    fn lock(&self) -> std::sync::MutexGuard<'_, Written> {
        self.ivars().written.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn string_for_type(pasteboard: &NSPasteboardImpl, kind: &str) -> Option<Retained<NSString>> {
    let written = pasteboard.lock();
    if !pasteboard.ivars().general || written.generation == shared().change_count() {
        return written.items.iter().find(|(k, _)| k == kind).map(|(_, s)| s.clone());
    }
    drop(written);
    foreign_string(kind).map(|s| NSString::from_str(&s))
}

/// The pasteboard with this name, made the first time it's asked for.
fn named(name: &str) -> Retained<NSPasteboard> {
    // Pointers to pasteboards that are never freed: the registry keeps a
    // reference to each.
    static REGISTRY: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    let mut registry = REGISTRY.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let ptr = *registry.entry(name.to_owned()).or_insert_with(|| {
        let this = NSPasteboardImpl::alloc().set_ivars(PasteboardIvars {
            name: NSString::from_str(name),
            general: name == GENERAL,
            // The general pasteboard's writes are current while the change
            // count hasn't moved since them; before any, there are none.
            written: Mutex::new(Written { items: Vec::new(), generation: if name == GENERAL { -1 } else { 1 } }),
        });
        // SAFETY: NSObject's designated initializer.
        let pasteboard: Retained<NSPasteboardImpl> = unsafe { msg_send![super(this), init] };
        Retained::into_raw(pasteboard) as usize
    });
    // SAFETY: the registry holds a reference to every pasteboard it names,
    // so this one is alive, and NSPasteboardImpl is the class NSPasteboard
    // names.
    unsafe { Retained::retain(ptr as *mut NSPasteboard) }.expect("registered pasteboards aren't null")
}

/// Our strings as the system selection offers them.
fn offered(items: &[(String, Retained<NSString>)]) -> Contents {
    let mut contents = Contents { items: Vec::new() };
    for (kind, string) in items {
        let bytes: Arc<[u8]> = string.to_string().into_bytes().into();
        for mime in mime_types(kind) {
            if contents.data(&mime).is_none() {
                contents.items.push((mime, bytes.clone()));
            }
        }
    }
    contents
}

/// Another program's selection as a string of pasteboard type `kind`.
fn foreign_string(kind: &str) -> Option<String> {
    let s = shared();
    let offered = s.foreign_mimes();
    let wanted = mime_types(kind);
    let mime = wanted.iter().find(|m| offered.contains(m))?;
    if kind == "public.utf8-plain-text" {
        // Read ahead when it was copied.
        return s.foreign_text();
    }
    s.read_foreign(mime).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// So the timeout's documentation stays next to its value.
const _: () = assert!(clipboard::READ_TIMEOUT.as_millis() == 200);
