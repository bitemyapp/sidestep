//! What the panels and the workspace ask the desktop for, through
//! xdg-desktop-portal: files to open or save (`FileChooser`), URLs to open
//! (`OpenURI`), and files to show in the file manager (`FileManager1`).
//!
//! Nothing here runs on the main thread but the bookkeeping: D-Bus is
//! spoken, with `desktop`'s client, on threads of its own, and answers come
//! back to the main thread as tasks on its run loop
//! (`runloop::main().perform`, in the common modes, so a modal loop waiting
//! for one gets it): a request names a callback kept on the main thread,
//! which the task calls. No request waits on another:
//!
//! - each file chooser runs on a thread of its own (`sidestep-chooser`),
//!   with a connection of its own, for as long as the user takes;
//! - URLs and files to show go, one after another, to one thread
//!   (`sidestep-portal`), whose calls each end within a few seconds;
//! - programs it starts (`xdg-open`, which may run the opened program in
//!   the foreground) are waited for on threads of their own.
//!
//! A file chooser follows the portal's request protocol: the thread picks
//! the request's handle token, subscribes to the `Response` signal on the
//! request's object path, calls `OpenFile` or `SaveFile` with the panel's
//! options (`accept_label`, `modal`, `multiple`, `directory`,
//! `current_folder` as bytes ending in NUL, `current_name`, `filters` from
//! the allowed types), and waits for that request's response (another
//! request's is ignored): 0 with `uris`, 1 for cancelled. Without a portal
//! it runs `zenity` (or `kdialog`) if one is installed, and otherwise
//! answers that it failed. A chooser given up on (`abandon`, as the panel's
//! `cancel:` does) ends at once: its connection is shut, which makes the
//! portal close the dialog it shows for it, or its program is killed. Its
//! subscription goes with its connection.
//!
//! URLs other than files open through `OpenURI.OpenURI`; files (which the
//! portal wants as file descriptors) and anything the portal refuses open
//! with `xdg-open`. `FileManager1.ShowItems` shows files, else `xdg-open`
//! opens their folder. The URL schemes a program can open are the usual
//! ones and those the desktop names handlers for: in `mimeapps.list` files
//! and in the `mimeinfo.cache` of installed programs. The table is read
//! once, by whichever thread asks first (the portal thread, started when
//! the application finishes launching, usually has).
//!
//! The message building and reading are plain functions, tested below with
//! made-up replies: the container the tests run in has no portal.
//! `tests/panels.rs` runs the choosers' fallback end to end with a stand-in
//! `zenity`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use sidestep_foundation::runloop::{self, Mode};

use crate::desktop::{self, Bus, Reader, Writer};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const FILE_CHOOSER: &str = "org.freedesktop.portal.FileChooser";
const OPEN_URI: &str = "org.freedesktop.portal.OpenURI";
const REQUEST: &str = "org.freedesktop.portal.Request";
const FILE_MANAGER: &str = "org.freedesktop.FileManager1";
const FILE_MANAGER_PATH: &str = "/org/freedesktop/FileManager1";
/// How often a chooser's thread looks whether it was given up on while a
/// fallback program runs.
const POLL: Duration = Duration::from_millis(50);

/// A file chooser's question.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Choose {
    pub save: bool,
    pub title: String,
    pub accept: String,
    pub multiple: bool,
    pub directory: bool,
    /// A folder to start in, as a path.
    pub folder: Option<String>,
    /// The name to suggest, when saving.
    pub name: Option<String>,
    /// Named filters of glob patterns (`*.txt`).
    pub filters: Vec<(String, Vec<String>)>,
}

/// A file chooser's answer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Chosen {
    /// The files chosen, as URIs.
    Uris(Vec<String>),
    Cancelled,
    /// Nothing could ask: no portal and no fallback.
    Failed,
}

/// Work for the portal thread.
enum Job {
    /// Open a URI, then run `done`, if any, with whether it went.
    OpenUri(String, Option<Done>),
    ShowItems(Vec<String>),
    /// Run something off the main thread (a completion handler).
    Run(Box<dyn FnOnce() + Send>),
}

/// What runs on the portal thread once a URI was handed on.
pub(crate) type Done = Box<dyn FnOnce(bool) + Send>;

/// A callback waiting on the main thread for a file chooser's answer.
pub(crate) type Answer = Box<dyn FnOnce(Chosen)>;

/// A chooser's answer waited for, and what gives it up.
struct Waiting {
    then: Answer,
    stop: Arc<Stop>,
}

/// What ends a chooser's wait early, shared with its thread.
#[derive(Default)]
struct Stop {
    stopped: AtomicBool,
    /// The chooser's connection, shut to end its wait.
    stream: Mutex<Option<UnixStream>>,
}

impl Stop {
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let stream = self.stream.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(stream) = stream {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Keep a clone of `stream` to shut; false if the chooser was already
    /// given up on.
    fn watch(&self, stream: &UnixStream) -> bool {
        let mut slot = self.stream.lock().unwrap_or_else(|e| e.into_inner());
        if self.stopped() {
            return false;
        }
        *slot = stream.try_clone().ok();
        true
    }
}

thread_local! {
    /// What waits for each file chooser's answer, on the main thread.
    static WAITING: RefCell<HashMap<u64, Waiting>> = RefCell::new(HashMap::new());
    static NEXT: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// The portal thread's inbox, once it runs.
static JOBS: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
/// URL schemes something handles, once read.
static SCHEMES: OnceLock<HashSet<String>> = OnceLock::new();

/// Start the portal thread, if it isn't running, so a first request finds
/// it ready (it reads the scheme table first).
pub(crate) fn prewarm() {
    let _ = jobs();
}

fn jobs() -> &'static Mutex<Sender<Job>> {
    JOBS.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new().name("sidestep-portal".into()).spawn(move || run(rx));
        Mutex::new(tx)
    })
}

/// Hand `job` to the portal thread; it comes back if the thread isn't
/// there.
fn send(job: Job) -> Result<(), Job> {
    jobs().lock().unwrap_or_else(|e| e.into_inner()).send(job).map_err(|e| e.0)
}

/// Ask the desktop for files; `then` gets the answer on the main thread,
/// unless the chooser is given up on first (see `abandon`). The request's
/// number.
pub(crate) fn choose(question: Choose, then: impl FnOnce(Chosen) + 'static) -> u64 {
    let id = NEXT.with(|n| n.replace(n.get() + 1));
    let stop = Arc::new(Stop::default());
    WAITING.with(|w| w.borrow_mut().insert(id, Waiting { then: Box::new(then), stop: stop.clone() }));
    let spawned = std::thread::Builder::new().name("sidestep-chooser".into()).spawn(move || {
        let chosen = ask(&question, &stop);
        if !stop.stopped() {
            answer(id, chosen);
        }
    });
    if spawned.is_err() {
        answer(id, Chosen::Failed);
    }
    id
}

/// Give up on chooser `id`: its dialog closes and it will never answer.
/// What waited for its answer, for the caller to answer instead (none if
/// it already has one).
pub(crate) fn abandon(id: u64) -> Option<Answer> {
    let waiting = WAITING.with(|w| w.borrow_mut().remove(&id))?;
    waiting.stop.stop();
    Some(waiting.then)
}

/// Open `uri` with what the desktop has for it, then run `done` (on the
/// portal thread) with whether it went; false if nothing handles its
/// scheme or the portal thread isn't there. The caller checks that a file
/// exists.
pub(crate) fn open_uri(uri: &str, done: Option<Done>) -> bool {
    let Some(scheme) = scheme_of(uri) else { return false };
    if !handled(&scheme) {
        return false;
    }
    send(Job::OpenUri(uri.to_owned(), done)).is_ok()
}

/// Show `uris` (files) in the file manager.
pub(crate) fn show_items(uris: Vec<String>) {
    let _ = send(Job::ShowItems(uris));
}

/// Run `f` off the main thread, after what was asked of the portal thread
/// before (a completion handler, as AppKit calls them on a queue of its
/// own).
pub(crate) fn later(f: Box<dyn FnOnce() + Send>) {
    if let Err(Job::Run(f)) = send(Job::Run(f)) {
        let _ = std::thread::Builder::new().name("sidestep-portal".into()).spawn(f);
    }
}

/// A URI's scheme, lowercased: letters, digits, `+`, `-` and `.` before a
/// colon, starting with a letter.
pub(crate) fn scheme_of(uri: &str) -> Option<String> {
    let (scheme, _) = uri.split_once(':')?;
    let ok = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    ok.then(|| scheme.to_ascii_lowercase())
}

/// Whether something opens URLs with `scheme`: the usual schemes, and those
/// the desktop names handlers for.
fn handled(scheme: &str) -> bool {
    const USUAL: [&str; 5] = ["http", "https", "mailto", "file", "ftp"];
    USUAL.contains(&scheme) || SCHEMES.get_or_init(read_schemes).contains(scheme)
}

/// Hand an answer to what waits for it, on the main thread.
fn answer(id: u64, chosen: Chosen) {
    runloop::main().perform(&[Mode::COMMON], move || {
        let waiting = WAITING.with(|w| w.borrow_mut().remove(&id));
        if let Some(waiting) = waiting {
            (waiting.then)(chosen);
        }
    });
}

// The portal thread.

fn run(rx: mpsc::Receiver<Job>) {
    let _ = SCHEMES.get_or_init(read_schemes);
    let mut bus: Option<(Bus, String)> = None;
    while let Ok(job) = rx.recv() {
        if bus.is_none() && !matches!(job, Job::Run(_)) {
            bus = connect();
        }
        match job {
            Job::OpenUri(uri, done) => {
                let file = uri.starts_with("file:");
                let opened = (!file && bus.as_mut().is_some_and(|(b, _)| call_open_uri(b, &uri))) || xdg_open(&uri);
                if let Some(done) = done {
                    done(opened);
                }
            }
            Job::ShowItems(uris) => {
                let shown = bus.as_mut().is_some_and(|(b, _)| call_show_items(b, &uris));
                if !shown && let Some(first) = uris.first() {
                    xdg_open(&parent_of(first));
                }
            }
            Job::Run(f) => f(),
        }
    }
}

/// A connection, and our unique name on the bus.
fn connect() -> Option<(Bus, String)> {
    let mut bus = Bus::connect()?;
    let hello =
        bus.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "Hello", "", |_| {})?;
    let name = Reader::new(&hello.body, hello.big_endian).string()?.to_owned();
    Some((bus, name))
}

// A chooser's thread.

/// Ask the portal, else a program, unless given up on meanwhile.
fn ask(question: &Choose, stop: &Stop) -> Chosen {
    if let Some(chosen) = choose_with_portal(question, stop) {
        return chosen;
    }
    if stop.stopped() {
        return Chosen::Cancelled;
    }
    choose_with_program(question, stop).unwrap_or(Chosen::Failed)
}

/// The object path of a request made with `token` by the connection named
/// `unique` (`:1.42` becomes `1_42`), as the portal makes it.
pub(crate) fn request_path(unique: &str, token: &str) -> String {
    let sender = unique.trim_start_matches(':').replace('.', "_");
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

/// Ask the portal's file chooser, on a connection of the chooser's own;
/// None without a portal. Given up on, the connection is shut, which ends
/// the wait (and the dialog).
fn choose_with_portal(question: &Choose, stop: &Stop) -> Option<Chosen> {
    let (mut bus, unique) = connect()?;
    if !stop.watch(&bus.stream) {
        return Some(Chosen::Cancelled);
    }
    let token = "sidestep0";
    let path = request_path(&unique, token);
    subscribe(&mut bus, &path)?;
    let member = if question.save { "SaveFile" } else { "OpenFile" };
    let reply =
        bus.call(PORTAL, PORTAL_PATH, FILE_CHOOSER, member, "ssa{sv}", |w| file_chooser_args(w, question, token))?;
    // An old portal may put the request elsewhere: listen there too.
    let handle = Reader::new(&reply.body, reply.big_endian).string()?.to_owned();
    if handle != path {
        subscribe(&mut bus, &handle)?;
    }
    // The dialog takes as long as the user does.
    bus.stream.set_read_timeout(None).ok()?;
    loop {
        let Some(message) = bus.read() else {
            // The connection went: given up on, or the portal went away.
            return Some(if stop.stopped() { Chosen::Cancelled } else { Chosen::Failed });
        };
        let ours = message.path.as_deref().is_some_and(|p| p == path || p == handle);
        if is_response(message.kind, message.member.as_deref(), ours) {
            return parse_response(&message.body, message.big_endian);
        }
    }
}

/// Whether a message is our request's `Response`.
fn is_response(kind: u8, member: Option<&str>, ours: bool) -> bool {
    kind == desktop::SIGNAL && member == Some("Response") && ours
}

fn subscribe(bus: &mut Bus, path: &str) -> Option<()> {
    let rule = format!("type='signal',interface='{REQUEST}',member='Response',path='{path}'");
    bus.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", "s", |w| {
        w.string(&rule)
    })?;
    Some(())
}

/// `OpenFile` and `SaveFile`'s arguments: the parent window (none: we
/// can't export one), the title, and the options.
pub(crate) fn file_chooser_args(w: &mut Writer, q: &Choose, token: &str) {
    w.string("");
    w.string(&q.title);
    dict(w, |d| {
        entry(d, "handle_token", "s", |w| w.string(token));
        entry(d, "modal", "b", |w| w.u32(1));
        if !q.accept.is_empty() {
            entry(d, "accept_label", "s", |w| w.string(&q.accept));
        }
        if !q.save {
            entry(d, "multiple", "b", |w| w.u32(u32::from(q.multiple)));
            entry(d, "directory", "b", |w| w.u32(u32::from(q.directory)));
        }
        if let Some(name) = &q.name {
            entry(d, "current_name", "s", |w| w.string(name));
        }
        if let Some(folder) = &q.folder {
            // Bytes ending in NUL.
            entry(d, "current_folder", "ay", |w| {
                w.u32(folder.len() as u32 + 1);
                w.buf.extend_from_slice(folder.as_bytes());
                w.buf.push(0);
            });
        }
        if !q.filters.is_empty() {
            entry(d, "filters", "a(sa(us))", |w| {
                array(w, 8, |w| {
                    for (name, patterns) in &q.filters {
                        w.pad(8);
                        w.string(name);
                        array(w, 8, |w| {
                            for pattern in patterns {
                                w.pad(8);
                                // 0: a glob pattern.
                                w.u32(0);
                                w.string(pattern);
                            }
                        });
                    }
                });
            });
        }
    });
}

/// An array: its length, then its elements from their alignment.
fn array(w: &mut Writer, align: usize, elements: impl FnOnce(&mut Writer)) {
    w.u32(0);
    let at = w.buf.len() - 4;
    w.pad(align);
    let start = w.buf.len();
    elements(w);
    let len = (w.buf.len() - start) as u32;
    w.buf[at..at + 4].copy_from_slice(&len.to_le_bytes());
}

/// An `a{sv}` dictionary.
fn dict(w: &mut Writer, entries: impl FnOnce(&mut Writer)) {
    array(w, 8, entries);
}

/// A dictionary entry: the key, then a variant of type `signature`.
fn entry(w: &mut Writer, key: &str, signature: &str, value: impl FnOnce(&mut Writer)) {
    w.pad(8);
    w.string(key);
    w.signature(signature);
    value(w);
}

/// A `Response` signal's body: the code and results (`uris` among them).
pub(crate) fn parse_response(body: &[u8], big_endian: bool) -> Option<Chosen> {
    let mut r = Reader::new(body, big_endian);
    let code = r.u32()?;
    if code != 0 {
        return Some(Chosen::Cancelled);
    }
    let len = r.u32()? as usize;
    r.pad(8);
    let end = r.at + len;
    let mut uris = Vec::new();
    while r.at < end {
        r.pad(8);
        let key = r.string()?;
        let signature = r.signature()?;
        if key == "uris" && signature == "as" {
            let n = r.u32()? as usize;
            let stop = r.at + n;
            while r.at < stop {
                uris.push(r.string()?.to_owned());
            }
        } else {
            skip(&mut r, signature)?;
        }
    }
    Some(Chosen::Uris(uris))
}

/// Read past one value of type `signature`.
fn skip(r: &mut Reader, signature: &str) -> Option<()> {
    let mut sig = signature.as_bytes();
    while !sig.is_empty() {
        sig = skip_one(r, sig)?;
    }
    Some(())
}

/// Read past the first complete type of `sig`; the rest of it.
fn skip_one<'s>(r: &mut Reader, sig: &'s [u8]) -> Option<&'s [u8]> {
    let (&c, rest) = sig.split_first()?;
    match c {
        b'y' => {
            r.byte()?;
            Some(rest)
        }
        b'b' | b'u' | b'i' => {
            r.u32()?;
            Some(rest)
        }
        b'n' | b'q' => {
            r.pad(2);
            r.at += 2;
            Some(rest)
        }
        b'x' | b't' | b'd' => {
            r.pad(8);
            r.at += 8;
            Some(rest)
        }
        b's' | b'o' => {
            r.string()?;
            Some(rest)
        }
        b'g' => {
            r.signature()?;
            Some(rest)
        }
        b'v' => {
            let inner = r.signature()?.to_owned();
            skip(r, &inner)?;
            Some(rest)
        }
        b'a' => {
            let len = r.u32()? as usize;
            let element = complete_type(rest)?;
            r.pad(alignment(element[0]));
            r.at = r.at.checked_add(len)?;
            (r.at <= r.buf.len()).then_some(&rest[element.len()..])
        }
        b'(' | b'{' => {
            r.pad(8);
            let inner = complete_type(sig)?;
            let mut body = &inner[1..inner.len() - 1];
            while !body.is_empty() {
                body = skip_one(r, body)?;
            }
            Some(&sig[inner.len()..])
        }
        _ => None,
    }
}

/// The first complete type at the start of `sig`.
fn complete_type(sig: &[u8]) -> Option<&[u8]> {
    match *sig.first()? {
        b'a' => complete_type(&sig[1..]).map(|t| &sig[..t.len() + 1]),
        open @ (b'(' | b'{') => {
            let close = if open == b'(' { b')' } else { b'}' };
            let mut depth = 0;
            for (i, &c) in sig.iter().enumerate() {
                if c == open {
                    depth += 1;
                } else if c == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&sig[..=i]);
                    }
                }
            }
            None
        }
        _ => Some(&sig[..1]),
    }
}

fn alignment(c: u8) -> usize {
    match c {
        b'y' | b'g' | b'v' => 1,
        b'n' | b'q' => 2,
        b'x' | b't' | b'd' | b'(' | b'{' => 8,
        _ => 4,
    }
}

fn call_open_uri(bus: &mut Bus, uri: &str) -> bool {
    bus.call(PORTAL, PORTAL_PATH, OPEN_URI, "OpenURI", "ssa{sv}", |w| {
        w.string("");
        w.string(uri);
        dict(w, |_| {});
    })
    .is_some()
}

fn call_show_items(bus: &mut Bus, uris: &[String]) -> bool {
    bus.call(FILE_MANAGER, FILE_MANAGER_PATH, FILE_MANAGER, "ShowItems", "ass", |w| {
        array(w, 4, |w| {
            for uri in uris {
                w.string(uri);
            }
        });
        w.string("");
    })
    .is_some()
}

/// The folder a file URI is in, as a URI.
fn parent_of(uri: &str) -> String {
    match uri.trim_end_matches('/').rsplit_once('/') {
        Some((dir, _)) if dir.len() > "file://".len() => dir.to_owned(),
        _ => uri.to_owned(),
    }
}

/// Start `xdg-open` on `uri`; whether it started. It is waited for on a
/// thread of its own, as it may run the program it opens in the foreground
/// until that exits.
fn xdg_open(uri: &str) -> bool {
    let Ok(mut child) = Command::new("xdg-open").arg(uri).stdin(Stdio::null()).spawn() else { return false };
    let reaped = std::thread::Builder::new().name("sidestep-xdg-open".into()).spawn(move || {
        let _ = child.wait();
    });
    // Without a thread, it is left to exit on its own.
    drop(reaped);
    true
}

/// Without a portal: `zenity`, else `kdialog`, if installed; killed if the
/// chooser is given up on.
fn choose_with_program(q: &Choose, stop: &Stop) -> Option<Chosen> {
    for (program, args) in [("zenity", zenity_args(q)), ("kdialog", kdialog_args(q))] {
        let spawned =
            Command::new(program).args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
        let Ok(child) = spawned else { continue };
        return Some(wait_for_program(child, stop));
    }
    None
}

/// Wait for a chooser program's answer: the paths it prints, one a line.
fn wait_for_program(mut child: Child, stop: &Stop) -> Chosen {
    // Its output is read as it comes, so a long one can't fill the pipe.
    let output = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = out.read_to_string(&mut text);
            text
        })
    });
    let status = loop {
        if stop.stopped() {
            let _ = child.kill();
            let _ = child.wait();
            return Chosen::Cancelled;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(POLL),
            Err(_) => return Chosen::Failed,
        }
    };
    let text = output.and_then(|o| o.join().ok()).unwrap_or_default();
    if !status.success() {
        return Chosen::Cancelled;
    }
    Chosen::Uris(text.lines().filter(|l| !l.is_empty()).map(file_uri).collect())
}

pub(crate) fn zenity_args(q: &Choose) -> Vec<String> {
    let mut args = vec!["--file-selection".to_owned(), format!("--title={}", q.title)];
    if q.save {
        args.push("--save".into());
        args.push("--confirm-overwrite".into());
    }
    if q.multiple {
        args.push("--multiple".into());
        args.push("--separator=\n".into());
    }
    if q.directory {
        args.push("--directory".into());
    }
    match (&q.folder, &q.name) {
        (Some(folder), Some(name)) => args.push(format!("--filename={folder}/{name}")),
        (Some(folder), None) => args.push(format!("--filename={folder}/")),
        (None, Some(name)) => args.push(format!("--filename={name}")),
        (None, None) => {}
    }
    for (name, patterns) in &q.filters {
        args.push(format!("--file-filter={name} | {}", patterns.join(" ")));
    }
    args
}

fn kdialog_args(q: &Choose) -> Vec<String> {
    let start = q.folder.clone().unwrap_or_else(|| ".".into());
    let filter = q.filters.iter().flat_map(|(_, p)| p.iter().cloned()).collect::<Vec<_>>().join(" ");
    let mut args = vec!["--title".to_owned(), q.title.clone()];
    if q.directory {
        args.push("--getexistingdirectory".into());
        args.push(start);
    } else if q.save {
        args.push("--getsavefilename".into());
        args.push(q.name.as_ref().map_or(start.clone(), |n| format!("{start}/{n}")));
        args.push(filter);
    } else {
        if q.multiple {
            args.push("--multiple".into());
            args.push("--separate-output".into());
        }
        args.push("--getopenfilename".into());
        args.push(start);
        args.push(filter);
    }
    args
}

/// A path as a `file://` URI, its bytes escaped where URIs need it.
pub(crate) fn file_uri(path: &str) -> String {
    let mut out = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The URL schemes the desktop names handlers for
/// (`x-scheme-handler/<scheme>=`): in the `mimeapps.list` files of the XDG
/// config and data folders, and in the `mimeinfo.cache` of the programs
/// installed in the data folders (which lists what their `.desktop` files
/// say they open).
fn read_schemes() -> HashSet<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let config = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
    let data = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    let config_dirs = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
    let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let mut files = vec![format!("{config}/mimeapps.list")];
    files.extend(config_dirs.split(':').map(|d| format!("{d}/mimeapps.list")));
    for dir in std::iter::once(data.as_str()).chain(data_dirs.split(':')) {
        for name in ["mimeapps.list", "defaults.list", "mimeinfo.cache"] {
            files.push(format!("{dir}/applications/{name}"));
        }
    }
    let mut schemes = HashSet::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        schemes.extend(schemes_in(&text));
    }
    schemes
}

pub(crate) fn schemes_in(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("x-scheme-handler/"))
        .filter_map(|l| {
            let (scheme, apps) = l.split_once('=')?;
            (!apps.trim().trim_matches(';').is_empty()).then(|| scheme.trim().to_ascii_lowercase())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_dict_keys(body: &[u8]) -> Vec<(String, String)> {
        // Past the parent window and the title.
        let mut r = Reader::new(body, false);
        r.string().unwrap();
        r.string().unwrap();
        let len = r.u32().unwrap() as usize;
        r.pad(8);
        let end = r.at + len;
        let mut keys = Vec::new();
        while r.at < end {
            r.pad(8);
            let key = r.string().unwrap().to_owned();
            let sig = r.signature().unwrap().to_owned();
            skip(&mut r, &sig).unwrap();
            keys.push((key, sig));
        }
        assert_eq!(r.at, body.len());
        keys
    }

    #[test]
    fn open_file_options_follow_the_panel() {
        let q = Choose {
            title: "Open".into(),
            accept: "Choose".into(),
            multiple: true,
            folder: Some("/home/u/docs".into()),
            filters: vec![("Text".into(), vec!["*.txt".into(), "*.md".into()])],
            ..Choose::default()
        };
        let mut w = Writer::default();
        file_chooser_args(&mut w, &q, "sidestep3");
        let keys = read_dict_keys(&w.buf);
        let names: Vec<&str> = keys.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            ["handle_token", "modal", "accept_label", "multiple", "directory", "current_folder", "filters"]
        );
        assert!(keys.contains(&("filters".into(), "a(sa(us))".into())));
        assert!(keys.contains(&("current_folder".into(), "ay".into())));
        // The folder's bytes end in NUL.
        let at = w.buf.windows(13).position(|s| s == b"/home/u/docs\0").expect("the folder");
        assert!(at > 0);
    }

    #[test]
    fn save_file_options_name_the_file() {
        let q = Choose { save: true, title: "Save".into(), name: Some("Untitled.txt".into()), ..Choose::default() };
        let mut w = Writer::default();
        file_chooser_args(&mut w, &q, "t");
        let keys: Vec<String> = read_dict_keys(&w.buf).into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["handle_token", "modal", "current_name"]);
    }

    /// A made-up `Response`: code 0 and results with `uris` among other
    /// entries.
    #[test]
    fn responses_give_their_uris() {
        let mut w = Writer::default();
        w.u32(0);
        dict(&mut w, |d| {
            entry(d, "current_filter", "(sa(us))", |w| {
                w.pad(8);
                w.string("Text");
                array(w, 8, |w| {
                    w.pad(8);
                    w.u32(0);
                    w.string("*.txt");
                });
            });
            entry(d, "uris", "as", |w| {
                array(w, 4, |w| {
                    w.string("file:///tmp/a.txt");
                    w.string("file:///tmp/b%20c.txt");
                });
            });
            entry(d, "writable", "b", |w| w.u32(1));
        });
        let chosen = parse_response(&w.buf, false).unwrap();
        assert_eq!(chosen, Chosen::Uris(vec!["file:///tmp/a.txt".into(), "file:///tmp/b%20c.txt".into()]));
        let mut cancelled = Writer::default();
        cancelled.u32(1);
        dict(&mut cancelled, |_| {});
        assert_eq!(parse_response(&cancelled.buf, false), Some(Chosen::Cancelled));
    }

    /// Only our request's `Response` answers the chooser: the same signal on
    /// another request's path (an `OpenURI` answered late) doesn't.
    #[test]
    fn only_our_response_answers() {
        assert!(is_response(desktop::SIGNAL, Some("Response"), true));
        assert!(!is_response(desktop::SIGNAL, Some("Response"), false));
        assert!(!is_response(desktop::SIGNAL, Some("Other"), true));
        assert!(!is_response(desktop::METHOD_RETURN, Some("Response"), true));
    }

    #[test]
    fn request_paths_escape_the_unique_name() {
        assert_eq!(request_path(":1.42", "sidestep0"), "/org/freedesktop/portal/desktop/request/1_42/sidestep0");
    }

    #[test]
    fn schemes_and_uris() {
        assert_eq!(scheme_of("HTTPS://example.com").as_deref(), Some("https"));
        assert_eq!(scheme_of("x-app+y.z:do"), Some("x-app+y.z".into()));
        assert_eq!(scheme_of("no scheme"), None);
        assert_eq!(scheme_of("1http://x"), None);
        let list = "[Default Applications]\nx-scheme-handler/slack=slack.desktop\nx-scheme-handler/empty=\ntext/plain=gedit.desktop\n";
        assert_eq!(schemes_in(list), ["slack"]);
        // mimeinfo.cache lists every program for a type, ending in ';'.
        let cache = "[MIME Cache]\nx-scheme-handler/zoommtg=Zoom.desktop;\nx-scheme-handler/none=;\n";
        assert_eq!(schemes_in(cache), ["zoommtg"]);
        assert_eq!(file_uri("/tmp/a b/ü.txt"), "file:///tmp/a%20b/%C3%BC.txt");
        assert_eq!(parent_of("file:///tmp/a/b.txt"), "file:///tmp/a");
    }

    #[test]
    fn zenity_is_asked_what_the_panel_asks() {
        let q = Choose {
            save: true,
            title: "Save".into(),
            folder: Some("/tmp".into()),
            name: Some("x.txt".into()),
            filters: vec![("Text".into(), vec!["*.txt".into()])],
            ..Choose::default()
        };
        let args = zenity_args(&q);
        assert!(args.contains(&"--save".into()));
        assert!(args.contains(&"--filename=/tmp/x.txt".into()));
        assert!(args.contains(&"--file-filter=Text | *.txt".into()));
    }
}
