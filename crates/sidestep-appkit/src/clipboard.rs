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
//! are read when asked for, waiting for as long as data keeps coming and
//! until [`READ_TIMEOUT`] passes without any: a large image that streams in
//! is read whole, and a client that never answers costs [`READ_TIMEOUT`],
//! once. That bound is documented on `NSPasteboard`.
//!
//! A counter shared by both threads plays `changeCount`: the main thread
//! bumps it when the program clears the pasteboard, and the render thread
//! when another client's selection brings something new. An offer that
//! only repeats the last one (same types, same text), as compositors send
//! on a change of focus, isn't news: what was read of the one before, and
//! the pasteboard items made of it, stay good. A write of ours is current
//! while the counter hasn't moved since.
//!
//! What another client offers is read once per change: answers are kept
//! until the next one, so a program reading a list of files item by item
//! (or twice) makes one read, and a type that didn't come in time isn't
//! waited for again. The drag pasteboard is served the same way from the
//! drag and drop offer under the pointer ([`Source::Drag`]): each drag that
//! enters a window is a change, its URL list comes read with it, and the
//! rest of its data is read only when the program asks for it, from that
//! drag's own offer (none once the drag is over).
//!
//! Data the program promised (`declareTypes:owner:`, item data providers)
//! is offered by type; when another client asks for it, the render thread
//! asks the main thread, which asks the owner (see
//! `pasteboard::provide_for_render`) and hands the bytes back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::protocol::ToRender;

/// How long the main thread waits for the selection's data before giving
/// up: for a read it asked for, since the read began or data last came; for
/// the text read ahead, since the selection arrived.
pub(crate) const READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Text (or a drag's URL list) longer than this isn't read ahead; it's
/// read if a program asks.
pub(crate) const READ_AHEAD_LIMIT: usize = 1 << 20;

/// The most a read takes: a longer selection reads as nothing.
pub(crate) const READ_LIMIT: usize = 256 << 20;

/// Types' data by MIME type, as offered to other clients. `None` is data
/// the program promised: the main thread makes it when a client asks.
pub(crate) struct Contents {
    pub items: Vec<(String, Option<Arc<[u8]>>)>,
}

impl Contents {
    /// The data offered as `mime`: `Some(None)` if it's promised.
    pub fn data(&self, mime: &str) -> Option<Option<Arc<[u8]>>> {
        self.items.iter().find(|(m, _)| m == mime).map(|(_, d)| d.clone())
    }
}

/// Which offer of another client's a pasteboard reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The selection: the general pasteboard.
    Selection,
    /// The drag and drop offer under the pointer: the drag pasteboard.
    Drag,
}

/// The MIME type that marks a selection as ours, so the render thread
/// doesn't read back what this process offers.
pub(crate) fn owner_mime() -> &'static str {
    static MIME: OnceLock<String> = OnceLock::new();
    MIME.get_or_init(|| format!("{};pid={}", crate::pasteboard_types::OWNER_PREFIX, std::process::id()))
}

/// The best text type among `mimes`.
pub(crate) fn text_mime(mimes: &[String]) -> Option<String> {
    crate::pasteboard_types::TEXT_MIMES.iter().find(|m| mimes.iter().any(|o| o == *m)).map(|m| m.to_string())
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
    /// For the drag pasteboard, the render thread's name for the drag, which
    /// reads are served from.
    drag: u64,
    mimes: Vec<String>,
    text: Text,
    /// The main thread's reads under way, by token. Answers for tokens not
    /// here (their reader gave up) go.
    reads: HashMap<u64, Reading>,
    /// What reads of the contents of change `cached_for` brought, by MIME
    /// type (`None`: nothing came, or not in time).
    cache: Vec<(String, Option<Arc<[u8]>>)>,
    cached_for: isize,
}

/// A read under way.
struct Reading {
    /// When it began, or data last came for it.
    active: Instant,
    /// The answer, once it comes: the data, or `None` if there's none.
    answer: Option<Option<Vec<u8>>>,
}

impl Foreign {
    fn new() -> Self {
        // Until the compositor tells us of a selection, there is no text.
        Foreign {
            offer: 0,
            drag: 0,
            mimes: Vec::new(),
            text: Text::Read(None),
            reads: HashMap::new(),
            cache: Vec::new(),
            cached_for: 0,
        }
    }

    /// The cache, emptied first if it's for another change than `generation`.
    fn cache_for(&mut self, generation: isize) -> &mut Vec<(String, Option<Arc<[u8]>>)> {
        if self.cached_for != generation {
            self.cache.clear();
            self.cached_for = generation;
        }
        &mut self.cache
    }
}

pub(crate) struct Shared {
    source: Source,
    /// `changeCount`.
    generation: AtomicIsize,
    /// The program has read the general pasteboard: reading ahead is worth
    /// it from now on.
    used: AtomicBool,
    foreign: Mutex<Foreign>,
    arrived: Condvar,
    next_token: AtomicU64,
}

/// The selection's state.
pub(crate) fn shared() -> &'static Shared {
    shared_for(Source::Selection)
}

pub(crate) fn shared_for(source: Source) -> &'static Shared {
    static SELECTION: OnceLock<Shared> = OnceLock::new();
    static DRAG: OnceLock<Shared> = OnceLock::new();
    let cell = match source {
        Source::Selection => &SELECTION,
        Source::Drag => &DRAG,
    };
    cell.get_or_init(|| Shared::new(source))
}

impl Shared {
    fn new(source: Source) -> Self {
        Shared {
            source,
            generation: AtomicIsize::new(1),
            used: AtomicBool::new(false),
            foreign: Mutex::new(Foreign::new()),
            arrived: Condvar::new(),
            next_token: AtomicU64::new(1),
        }
    }

    pub fn change_count(&self) -> isize {
        self.used.store(true, Ordering::Relaxed);
        self.current()
    }

    /// The change count, without taking it as a sign the program reads the
    /// pasteboard.
    fn current(&self) -> isize {
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

    /// Drag `drag` entered one of our windows offering `mimes`, and its URL
    /// list, if it has one, as read with it: always a change, as each drag
    /// is new. Returns the new change count.
    pub fn drag_offered(&self, drag: u64, mimes: Vec<String>, urls: Option<(String, Arc<[u8]>)>) -> isize {
        let mut f = self.lock();
        f.offer += 1;
        f.drag = drag;
        f.text = Text::Unread;
        f.mimes = mimes;
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let cache = f.cache_for(generation);
        cache.extend(urls.map(|(mime, data)| (mime, Some(data))));
        drop(f);
        self.arrived.notify_all();
        generation
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

    /// Data is coming for the main thread's read `token`: it waits on.
    pub fn read_progressed(&self, token: u64) {
        if let Some(reading) = self.lock().reads.get_mut(&token) {
            reading.active = Instant::now();
        }
    }

    /// The answer to the main thread's read `token`.
    pub fn read_arrived(&self, token: u64, data: Option<Vec<u8>>) {
        if let Some(reading) = self.lock().reads.get_mut(&token) {
            reading.answer = Some(data);
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

    /// Read the foreign selection as `mime` (see
    /// [`read_offer`](Self::read_offer)).
    pub fn read_foreign(&self, mime: &str) -> Option<Vec<u8>> {
        self.read_offer(mime, None).map(|data| data.to_vec())
    }

    /// Whether change `generation` is still current: data read now is its.
    pub fn is_current(&self, generation: isize) -> bool {
        self.current() == generation
    }

    /// Read `mime` for the contents of change `from` (none: the current
    /// ones), or nothing if the pasteboard has changed since. Waits while
    /// data keeps coming, and gives up after [`READ_TIMEOUT`] without any.
    /// Each type is read once per change: what came, or that nothing came
    /// in time, is kept, so a client that never answers costs one wait.
    pub fn read_offer(&self, mime: &str, from: Option<isize>) -> Option<Arc<[u8]>> {
        self.used.store(true, Ordering::Relaxed);
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let (generation, drag) = {
            let mut f = self.lock();
            let generation = self.current();
            if from.is_some_and(|g| g != generation) {
                return None;
            }
            if let Some((_, data)) = f.cache_for(generation).iter().find(|(m, _)| m == mime) {
                return data.clone();
            }
            f.reads.insert(token, Reading { active: Instant::now(), answer: None });
            (generation, f.drag)
        };
        let source = self.source;
        crate::app::send_if_running(ToRender::ReadSelection { mime: mime.to_owned(), token, source, drag });
        let mut f = self.lock();
        loop {
            let reading = f.reads.get(&token)?;
            if reading.answer.is_some() {
                let data: Option<Arc<[u8]>> = f.reads.remove(&token).and_then(|r| r.answer).flatten().map(Into::into);
                self.remember(&mut f, generation, mime, data.clone());
                return data;
            }
            let left = (reading.active + READ_TIMEOUT).saturating_duration_since(Instant::now());
            if left.is_zero() {
                // A late answer has nowhere to go, and asking again would
                // only wait again.
                f.reads.remove(&token);
                self.remember(&mut f, generation, mime, None);
                return None;
            }
            f = self.arrived.wait_timeout(f, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Keep what a read of change `generation` brought, if that's still the
    /// current one.
    fn remember(&self, f: &mut Foreign, generation: isize, mime: &str, data: Option<Arc<[u8]>>) {
        if !self.is_current(generation) {
            return;
        }
        let cache = f.cache_for(generation);
        if !cache.iter().any(|(m, _)| m == mime) {
            cache.push((mime.to_owned(), data));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Shared {
        let s = Shared::new(Source::Selection);
        s.used.store(true, Ordering::Relaxed);
        s
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
    fn each_drag_is_a_change() {
        let s = Shared::new(Source::Drag);
        let count = s.change_count();
        s.drag_offered(1, text(), None);
        let urls: Arc<[u8]> = Arc::from(&b"file:///tmp/a\r\n"[..]);
        let generation = s.drag_offered(2, text(), Some(("text/uri-list".into(), urls.clone())));
        assert_eq!(s.change_count(), count + 2);
        assert_eq!(generation, count + 2);
        assert_eq!(s.foreign_mimes(), text());
        // The URL list came with the drag: reading it doesn't wait.
        let start = Instant::now();
        assert_eq!(s.read_offer("text/uri-list", Some(generation)), Some(urls));
        assert!(start.elapsed() < READ_TIMEOUT / 4);
        // Nor does reading an earlier drag's data: there's none.
        assert_eq!(s.read_offer("text/uri-list", Some(generation - 1)), None);
        assert!(start.elapsed() < READ_TIMEOUT / 4);
    }

    /// A thread answering the next read `s` asks for with `data`.
    fn answer_next(s: &std::sync::Arc<Shared>, data: &'static [u8]) -> std::thread::JoinHandle<()> {
        let token = s.next_token.load(Ordering::Relaxed);
        let s = s.clone();
        std::thread::spawn(move || {
            while !s.lock().reads.contains_key(&token) {
                std::thread::yield_now();
            }
            s.read_arrived(token, Some(data.to_vec()));
        })
    }

    #[test]
    fn a_repeated_offer_keeps_what_was_read() {
        let s = std::sync::Arc::new(fresh());
        let mimes = vec!["text/plain;charset=utf-8".to_string(), "text/html".to_string()];
        let offer = s.foreign_offered(mimes.clone(), true).expect("read ahead");
        s.text_arrived(offer, ReadAhead::Text(Some("hi".into())));
        // Items made now belong to this change.
        let generation = s.change_count();
        let feeder = answer_next(&s, b"<b>hi</b>");
        assert_eq!(s.read_offer("text/html", Some(generation)).as_deref(), Some(&b"<b>hi</b>"[..]));
        feeder.join().unwrap();
        // The compositor offers the same selection again, as when a window of
        // ours gets the keyboard: the items stay good, and what was read of
        // it is read again at once.
        let again = s.foreign_offered(mimes.clone(), true).expect("read ahead");
        s.text_arrived(again, ReadAhead::Text(Some("hi".into())));
        assert!(s.is_current(generation));
        let start = Instant::now();
        assert_eq!(s.read_offer("text/html", Some(generation)).as_deref(), Some(&b"<b>hi</b>"[..]));
        assert!(start.elapsed() < READ_TIMEOUT / 4);
        assert_eq!(s.foreign_text().as_deref(), Some("hi"));
        // New contents are a new change: the old items read nothing.
        let other = s.foreign_offered(mimes, true).expect("read ahead");
        s.text_arrived(other, ReadAhead::Text(Some("ho".into())));
        assert!(!s.is_current(generation));
        assert_eq!(s.read_offer("text/html", Some(generation)), None);
    }

    #[test]
    fn a_type_that_never_comes_costs_one_wait() {
        let s = fresh();
        let start = Instant::now();
        assert_eq!(s.read_offer("image/png", None), None);
        assert!(start.elapsed() >= READ_TIMEOUT);
        // Remembered for this change: no second wait.
        let again = Instant::now();
        assert_eq!(s.read_offer("image/png", None), None);
        assert!(again.elapsed() < READ_TIMEOUT / 4);
        // A new change asks again.
        s.foreign_offered(vec!["image/png".into()], false);
        let fresh_start = Instant::now();
        assert_eq!(s.read_offer("image/png", None), None);
        assert!(fresh_start.elapsed() >= READ_TIMEOUT);
    }

    #[test]
    fn late_answers_to_abandoned_reads_go() {
        let s = fresh();
        s.read_arrived(42, Some(vec![1, 2, 3]));
        s.read_progressed(42);
        assert!(s.lock().reads.is_empty());
    }

    #[test]
    fn reads_wait_while_data_comes() {
        let s = std::sync::Arc::new(fresh());
        // The first token read_offer takes.
        let token = s.next_token.load(Ordering::Relaxed);
        let feeder = {
            let s = s.clone();
            std::thread::spawn(move || {
                // Data comes for three timeouts, a little at a time.
                let until = Instant::now() + READ_TIMEOUT * 3;
                while Instant::now() < until {
                    std::thread::sleep(READ_TIMEOUT / 4);
                    s.read_progressed(token);
                }
                s.read_arrived(token, Some(b"all of it".to_vec()));
            })
        };
        let start = Instant::now();
        assert_eq!(s.read_offer("image/png", None).as_deref(), Some(&b"all of it"[..]));
        assert!(start.elapsed() >= READ_TIMEOUT * 3);
        feeder.join().unwrap();
        // Read once per change.
        assert_eq!(s.read_offer("image/png", None).as_deref(), Some(&b"all of it"[..]));
        // Without data, it gives up.
        let start = Instant::now();
        assert_eq!(s.read_offer("text/html", None), None);
        assert!(start.elapsed() >= READ_TIMEOUT && start.elapsed() < READ_TIMEOUT * 3);
    }
}
