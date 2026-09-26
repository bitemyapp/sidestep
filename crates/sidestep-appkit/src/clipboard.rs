//! The system clipboard (the Wayland selection), between the main thread,
//! where `NSPasteboard` lives, and the render thread, which owns the
//! wl_data_device.
//!
//! Writing is asynchronous and never waits: the main thread keeps what it
//! wrote and hands the render thread a copy to offer, and the render thread
//! answers other clients' requests for it from its event loop.
//!
//! Reading can't be synchronous on Wayland: the data comes through a pipe
//! from whichever client owns the selection. So the render thread reads
//! ahead: whenever the selection changes hands (and whenever a window of
//! ours gets the keyboard, when compositors send the current selection),
//! it reads the text, which is what programs read nearly always, and keeps
//! it here. `stringForType:` then answers at once. If the text is still on
//! its way, or a program asks for another type, the main thread waits for
//! it, but for [`READ_TIMEOUT`] at most, a bound documented on
//! `NSPasteboard`.
//!
//! A counter shared by both threads plays `changeCount`: the render thread
//! bumps it when another client takes the selection, the main thread when
//! the program clears the pasteboard. A write of ours is current while
//! the counter hasn't moved since.

use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::protocol::ToRender;

/// How long the main thread waits for the selection's data before giving
/// up, when it wasn't read ahead of time.
pub(crate) const READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Types' data by MIME type, as offered to other clients.
pub(crate) struct Contents {
    pub items: Vec<(String, Arc<[u8]>)>,
}

impl Contents {
    pub fn data(&self, mime: &str) -> Option<Arc<[u8]>> {
        self.items.iter().find(|(m, _)| m == mime).map(|(_, d)| d.clone())
    }
}

/// The MIME type that marks a selection as ours, so the render thread
/// doesn't read back what this process offers.
pub(crate) fn owner_mime() -> &'static str {
    static MIME: OnceLock<String> = OnceLock::new();
    MIME.get_or_init(|| format!("application/x-sidestep-owner;pid={}", std::process::id()))
}

/// The MIME types a pasteboard type is offered and read as, best first.
pub(crate) fn mime_types(pasteboard_type: &str) -> Vec<String> {
    let known: &[&str] = match pasteboard_type {
        "public.utf8-plain-text" => &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"],
        "public.html" => &["text/html"],
        "public.rtf" => &["text/rtf", "application/rtf"],
        "public.png" => &["image/png"],
        "public.tiff" => &["image/tiff"],
        "public.file-url" => &["text/uri-list"],
        "public.url" => &["text/x-moz-url", "text/uri-list"],
        other if other.contains('/') => return vec![other.to_owned()],
        _ => &[],
    };
    known.iter().map(|m| m.to_string()).collect()
}

/// What the render thread knows of a selection another client owns.
struct Foreign {
    /// The generation it arrived in.
    generation: isize,
    mimes: Vec<String>,
    /// Its text, once read: `Some(None)` if it has none.
    text: Option<Option<String>>,
    /// Answers to reads, by token.
    reads: HashMap<u64, Option<Vec<u8>>>,
}

impl Default for Foreign {
    fn default() -> Self {
        // Until the compositor tells us of a selection, there is no text.
        Foreign { generation: 0, mimes: Vec::new(), text: Some(None), reads: HashMap::new() }
    }
}

pub(crate) struct Shared {
    /// `changeCount`.
    generation: AtomicIsize,
    foreign: Mutex<Foreign>,
    arrived: Condvar,
    next_token: AtomicU64,
}

pub(crate) fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        generation: AtomicIsize::new(1),
        foreign: Mutex::new(Foreign::default()),
        arrived: Condvar::new(),
        next_token: AtomicU64::new(1),
    })
}

impl Shared {
    pub fn change_count(&self) -> isize {
        self.generation.load(Ordering::Acquire)
    }

    /// The program cleared the pasteboard: a new generation, its own.
    pub fn bump(&self) -> isize {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Foreign> {
        self.foreign.lock().unwrap_or_else(|e| e.into_inner())
    }

    // The render thread's side.

    /// Another client took the selection (or cleared it: no types).
    /// Returns the new generation, which the text read ahead belongs to.
    pub fn foreign_changed(&self, mimes: Vec<String>, reading_text: bool) -> isize {
        let mut f = self.lock();
        f.mimes = mimes;
        f.text = if reading_text { None } else { Some(None) };
        f.reads.clear();
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        f.generation = generation;
        drop(f);
        self.arrived.notify_all();
        generation
    }

    /// The text of generation `generation`'s selection arrived.
    pub fn text_arrived(&self, generation: isize, text: Option<String>) {
        let mut f = self.lock();
        if f.generation == generation {
            f.text = Some(text);
        }
        drop(f);
        self.arrived.notify_all();
    }

    pub fn read_arrived(&self, token: u64, data: Option<Vec<u8>>) {
        self.lock().reads.insert(token, data);
        self.arrived.notify_all();
    }

    // The main thread's side.

    /// Offer `contents` as the selection, or clear it.
    pub fn offer(&self, contents: Option<Contents>) {
        crate::app::send_if_running(ToRender::SetSelection { contents: contents.map(Arc::new) });
    }

    /// The MIME types the foreign selection offers.
    pub fn foreign_mimes(&self) -> Vec<String> {
        self.lock().mimes.clone()
    }

    /// The foreign selection's text, waiting for [`READ_TIMEOUT`] at most.
    pub fn foreign_text(&self) -> Option<String> {
        let deadline = Instant::now() + READ_TIMEOUT;
        let mut f = self.lock();
        loop {
            if let Some(text) = &f.text {
                return text.clone();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            f = self.arrived.wait_timeout(f, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Read the foreign selection as `mime`, waiting for [`READ_TIMEOUT`]
    /// at most.
    pub fn read_foreign(&self, mime: &str) -> Option<Vec<u8>> {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        crate::app::send_if_running(ToRender::ReadSelection { mime: mime.to_owned(), token });
        let deadline = Instant::now() + READ_TIMEOUT;
        let mut f = self.lock();
        loop {
            if let Some(data) = f.reads.remove(&token) {
                return data;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            f = self.arrived.wait_timeout(f, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}
