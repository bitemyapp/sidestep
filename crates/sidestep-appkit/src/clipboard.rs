//! The system clipboard (the Wayland selection), between the main thread,
//! where `NSPasteboard` lives, and the render thread, which owns the
//! wl_data_device.
//!
//! Writing is asynchronous and never waits: the main thread keeps what it
//! wrote and hands the render thread a copy to offer, and the render thread
//! answers other clients' requests for it from its event loop.
//!
//! Reading can't be synchronous on Wayland: the data comes through a pipe
//! from whichever client owns the selection. So once the program has used
//! the general pasteboard, the render thread reads ahead: whenever another
//! client offers a selection (compositors offer the current one again each
//! time a window of ours gets the keyboard), it reads the text, which is
//! what programs read nearly always, up to [`READ_AHEAD_LIMIT`], and keeps
//! it here. `stringForType:` then answers at once. If the text is still on
//! its way, the main thread waits for it until [`READ_TIMEOUT`] after the
//! selection arrived, and no longer: a client that never answers costs one
//! wait, not one per call. Other types, and text too long to read ahead,
//! are read when asked for, waiting [`READ_TIMEOUT`] at most. That bound is
//! documented on `NSPasteboard`.
//!
//! A counter shared by both threads plays `changeCount`: the main thread
//! bumps it when the program clears the pasteboard, and the render thread
//! when another client's selection brings something new. An offer that
//! only repeats the last one (same types, same text) isn't news. A write of
//! ours is current while the counter hasn't moved since.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::protocol::ToRender;

/// How long the main thread waits for the selection's data before giving
/// up: after a read it asked for, or after the selection arrived for the
/// text read ahead.
pub(crate) const READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Text longer than this isn't read ahead; it's read if a program asks.
pub(crate) const READ_AHEAD_LIMIT: usize = 1 << 20;

/// The most a read takes: a longer selection reads as nothing.
pub(crate) const READ_LIMIT: usize = 256 << 20;

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

/// The best text type among `mimes`.
pub(crate) fn text_mime(mimes: &[String]) -> Option<String> {
    mime_types("public.utf8-plain-text").into_iter().find(|m| mimes.contains(m))
}

/// Where the text of another client's selection stands.
enum Text {
    /// Not read: nobody had asked when it came, or it's too long to read
    /// ahead.
    Unread,
    /// On its way, for readers to wait for until `deadline`. `before` is
    /// the last selection's text when the types are the same, to tell
    /// whether this one is news.
    Reading { offer: u64, deadline: Instant, before: Option<Option<String>> },
    /// Read: `None` if there's none, or it couldn't be read.
    Read(Option<String>),
}

/// What reading the text ahead came to.
pub(crate) enum ReadAhead {
    Text(Option<String>),
    TooLong,
    Failed,
}

/// What the render thread knows of a selection another client owns.
struct Foreign {
    /// Counts other clients' selections; a read ahead answers for one.
    offer: u64,
    mimes: Vec<String>,
    text: Text,
    /// The main thread's reads under way, by token, with the answer once
    /// it comes. Answers for tokens not here (their reader gave up) go.
    reads: HashMap<u64, Option<Option<Vec<u8>>>>,
}

pub(crate) struct Shared {
    /// `changeCount`.
    generation: AtomicIsize,
    /// The program has read the general pasteboard: reading ahead is worth
    /// it from now on.
    used: AtomicBool,
    foreign: Mutex<Foreign>,
    arrived: Condvar,
    next_token: AtomicU64,
}

pub(crate) fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        generation: AtomicIsize::new(1),
        used: AtomicBool::new(false),
        // Until the compositor tells us of a selection, there is no text.
        foreign: Mutex::new(Foreign { offer: 0, mimes: Vec::new(), text: Text::Read(None), reads: HashMap::new() }),
        arrived: Condvar::new(),
        next_token: AtomicU64::new(1),
    })
}

impl Shared {
    pub fn change_count(&self) -> isize {
        self.used.store(true, Ordering::Relaxed);
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

    /// Another client offers a selection with `mimes`, or cleared it (no
    /// types). Returns the offer whose text to read ahead, if it should be.
    pub fn foreign_offered(&self, mimes: Vec<String>, has_text: bool) -> Option<u64> {
        let mut f = self.lock();
        f.offer += 1;
        let read_ahead = has_text && self.used.load(Ordering::Relaxed);
        if read_ahead {
            // The generation moves once the text shows whether this is news.
            let before = match &f.text {
                _ if f.mimes != mimes => None,
                Text::Read(text) => Some(text.clone()),
                // Offered again before its text came: compare with what
                // was there before that.
                Text::Reading { before, .. } => before.clone(),
                Text::Unread => None,
            };
            f.text = Text::Reading { offer: f.offer, deadline: Instant::now() + READ_TIMEOUT, before };
        } else {
            let news = !(mimes.is_empty() && f.mimes.is_empty());
            f.text = if has_text { Text::Unread } else { Text::Read(None) };
            if news {
                self.generation.fetch_add(1, Ordering::AcqRel);
            }
        }
        f.mimes = mimes;
        let offer = f.offer;
        drop(f);
        self.arrived.notify_all();
        read_ahead.then_some(offer)
    }

    /// The text of `offer` was read ahead, or couldn't be.
    pub fn text_arrived(&self, offer: u64, outcome: ReadAhead) {
        let mut f = self.lock();
        let Text::Reading { offer: reading, before, .. } = &mut f.text else { return };
        if *reading != offer {
            return;
        }
        let before = before.take();
        let (text, news) = match outcome {
            ReadAhead::Text(text) => {
                let news = before.as_ref() != Some(&text);
                (Text::Read(text), news)
            }
            ReadAhead::TooLong => (Text::Unread, true),
            ReadAhead::Failed => (Text::Read(None), true),
        };
        f.text = text;
        if news {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        drop(f);
        self.arrived.notify_all();
    }

    /// The answer to the main thread's read `token`.
    pub fn read_arrived(&self, token: u64, data: Option<Vec<u8>>) {
        if let Some(slot) = self.lock().reads.get_mut(&token) {
            *slot = Some(data);
        }
        self.arrived.notify_all();
    }

    // The main thread's side.

    /// Offer `contents` as the selection, or clear it.
    pub fn offer(&self, contents: Option<Contents>) {
        crate::app::send_if_running(ToRender::SetSelection { contents: contents.map(Arc::new) });
    }

    /// The MIME types the foreign selection offers.
    pub fn foreign_mimes(&self) -> Vec<String> {
        self.used.store(true, Ordering::Relaxed);
        self.lock().mimes.clone()
    }

    /// The foreign selection's text: read ahead, or on its way until its
    /// deadline, or read now (see [`read_foreign`](Self::read_foreign)).
    pub fn foreign_text(&self) -> Option<String> {
        self.used.store(true, Ordering::Relaxed);
        let mut f = self.lock();
        loop {
            match &f.text {
                Text::Read(text) => return text.clone(),
                Text::Reading { deadline, .. } => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return None;
                    }
                    f = self.arrived.wait_timeout(f, left).unwrap_or_else(|e| e.into_inner()).0;
                }
                Text::Unread => {
                    let (offer, mime) = (f.offer, text_mime(&f.mimes)?);
                    drop(f);
                    let text = self.read_foreign(&mime).map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
                    // Kept for the selection it was read from, even if the
                    // read failed: asking again would only wait again.
                    let mut f = self.lock();
                    if f.offer == offer && matches!(f.text, Text::Unread) {
                        f.text = Text::Read(text.clone());
                    }
                    return text;
                }
            }
        }
    }

    /// Read the foreign selection as `mime`, waiting for [`READ_TIMEOUT`]
    /// at most.
    pub fn read_foreign(&self, mime: &str) -> Option<Vec<u8>> {
        self.used.store(true, Ordering::Relaxed);
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        self.lock().reads.insert(token, None);
        crate::app::send_if_running(ToRender::ReadSelection { mime: mime.to_owned(), token });
        let deadline = Instant::now() + READ_TIMEOUT;
        let mut f = self.lock();
        loop {
            if let Some(Some(_)) = f.reads.get(&token) {
                return f.reads.remove(&token).flatten().flatten();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // A late answer has nowhere to go.
                f.reads.remove(&token);
                return None;
            }
            f = self.arrived.wait_timeout(f, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Shared {
        Shared {
            generation: AtomicIsize::new(1),
            used: AtomicBool::new(true),
            foreign: Mutex::new(Foreign { offer: 0, mimes: Vec::new(), text: Text::Read(None), reads: HashMap::new() }),
            arrived: Condvar::new(),
            next_token: AtomicU64::new(1),
        }
    }

    fn text() -> Vec<String> {
        vec!["text/plain;charset=utf-8".to_string()]
    }

    #[test]
    fn a_repeated_offer_isnt_news() {
        let s = fresh();
        let offer = s.foreign_offered(text(), true).expect("read ahead");
        s.text_arrived(offer, ReadAhead::Text(Some("hi".into())));
        let count = s.change_count();
        assert_eq!(count, 2);
        // The compositor offers the same selection again, on focus.
        let again = s.foreign_offered(text(), true).expect("read ahead");
        s.text_arrived(again, ReadAhead::Text(Some("hi".into())));
        assert_eq!(s.change_count(), count);
        assert_eq!(s.foreign_text().as_deref(), Some("hi"));
        // New text is.
        let other = s.foreign_offered(text(), true).expect("read ahead");
        s.text_arrived(other, ReadAhead::Text(Some("ho".into())));
        assert_eq!(s.change_count(), count + 1);
    }

    #[test]
    fn a_client_that_never_answers_costs_one_wait() {
        let s = fresh();
        s.foreign_offered(text(), true).expect("read ahead");
        let start = Instant::now();
        assert_eq!(s.foreign_text(), None);
        assert!(start.elapsed() >= READ_TIMEOUT / 2);
        // The deadline has passed: no more waiting.
        let again = Instant::now();
        assert_eq!(s.foreign_text(), None);
        assert!(again.elapsed() < READ_TIMEOUT / 4);
        // An answer for an older offer is ignored.
        s.text_arrived(0, ReadAhead::Text(Some("late".into())));
        assert_eq!(s.foreign_text(), None);
    }

    #[test]
    fn late_answers_to_abandoned_reads_go() {
        let s = fresh();
        s.read_arrived(42, Some(vec![1, 2, 3]));
        assert!(s.lock().reads.is_empty());
    }
}
