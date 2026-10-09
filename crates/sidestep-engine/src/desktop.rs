//! The desktop's appearance preferences, from the settings portal.
//!
//! Desktops publish them under `org.freedesktop.appearance` through
//! xdg-desktop-portal's `org.freedesktop.portal.Settings`: `color-scheme`
//! (1: prefer dark, 2: prefer light, 0: no preference), `accent-color` (an
//! sRGB triple, out of range when unset) and `contrast` (1: high), and
//! signal changes. GNOME's portal also publishes its interface settings
//! under `org.gnome.desktop.interface`: the text scale, the interface and
//! monospaced fonts, the cursor size and whether to animate. A thread of
//! its own (`settings`) asks the session bus once and then waits for
//! changes, so neither the main thread nor the render thread ever waits
//! for D-Bus. Only the few messages this needs are spoken, with a minimal
//! client below.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::time::Duration;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const INTERFACE: &str = "org.gnome.desktop.interface";
/// The settings read, appearance first: a window's first frame waits for
/// those.
const KEYS: [(&str, &str); 8] = [
    (NAMESPACE, "color-scheme"),
    (NAMESPACE, "accent-color"),
    (NAMESPACE, "contrast"),
    (INTERFACE, "text-scaling-factor"),
    (INTERFACE, "font-name"),
    (INTERFACE, "monospace-font-name"),
    (INTERFACE, "cursor-size"),
    (INTERFACE, "enable-animations"),
];

/// A setting the portal told.
#[derive(Clone, Debug, PartialEq)]
pub enum Setting {
    Scheme(Scheme),
    /// sRGB red, green and blue; `None` when the desktop has no accent.
    Accent(Option<[f64; 3]>),
    Contrast(bool),
    /// How much larger than their size text is shown.
    TextScale(f64),
    /// Fonts as GNOME names them: a family, styles and a size in points
    /// (`Cantarell 11`).
    Font(String),
    MonospaceFont(String),
    CursorSize(i32),
    Animations(bool),
}
/// How long the first answer may take before the thread gives up.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(2);

/// A preference, as the portal numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    NoPreference,
    Dark,
    Light,
}

impl Scheme {
    fn from_portal(value: u32) -> Scheme {
        match value {
            1 => Scheme::Dark,
            2 => Scheme::Light,
            _ => Scheme::NoPreference,
        }
    }
}

/// Watch the settings on a thread of its own, calling `changed` with each
/// once known and at every change, and `done` when the first answers are
/// in (or there's no session bus or portal to ask).
pub fn watch(changed: impl Fn(Setting) + Send + 'static, done: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name("sidestep-settings".into()).spawn(move || {
        let mut done = Some(done);
        let _ = run(&changed, &mut done);
        if let Some(done) = done.take() {
            done();
        }
    });
}

/// The setting `key` of `namespace` holds the variant `r` is at.
fn setting(namespace: &str, key: &str, r: &mut Reader) -> Option<Setting> {
    Some(match (namespace, key, r.variant()?) {
        (NAMESPACE, "color-scheme", Value::U32(v)) => Setting::Scheme(Scheme::from_portal(v)),
        (NAMESPACE, "accent-color", Value::Rgb(c)) => {
            Setting::Accent(c.iter().all(|v| (0.0..=1.0).contains(v)).then_some(c))
        }
        (NAMESPACE, "contrast", Value::U32(v)) => Setting::Contrast(v == 1),
        (INTERFACE, "text-scaling-factor", Value::F64(v)) if v > 0.0 => Setting::TextScale(v),
        (INTERFACE, "font-name", Value::Str(s)) => Setting::Font(s),
        (INTERFACE, "monospace-font-name", Value::Str(s)) => Setting::MonospaceFont(s),
        (INTERFACE, "cursor-size", Value::I32(v)) => Setting::CursorSize(v),
        (INTERFACE, "enable-animations", Value::Bool(b)) => Setting::Animations(b),
        _ => return None,
    })
}

fn run(changed: &dyn Fn(Setting), done: &mut Option<impl FnOnce()>) -> Option<()> {
    let mut bus = Bus::connect()?;
    bus.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "Hello", "", |_| {})?;
    for namespace in [NAMESPACE, INTERFACE] {
        let rule = format!(
            "type='signal',interface='{SETTINGS}',member='SettingChanged',path='{PORTAL_PATH}',arg0='{namespace}'"
        );
        bus.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", "s", |w| {
            w.string(&rule)
        })?;
    }
    for (i, (namespace, key)) in KEYS.into_iter().enumerate() {
        // ReadOne answers with the value; older portals only have Read,
        // which wraps it in one more variant.
        let args = |w: &mut Writer| {
            w.string(namespace);
            w.string(key);
        };
        let reply = match bus.call(PORTAL, PORTAL_PATH, SETTINGS, "ReadOne", "ss", args) {
            Some(reply) => Some(reply),
            None => bus.call(PORTAL, PORTAL_PATH, SETTINGS, "Read", "ss", args),
        };
        if let Some(value) = reply.and_then(|m| setting(namespace, key, &mut Reader::new(&m.body, m.big_endian))) {
            changed(value);
        }
        // The appearance is in: windows may show.
        if i == 2
            && let Some(done) = done.take()
        {
            done();
        }
    }
    bus.stream.set_read_timeout(None).ok()?;
    loop {
        let message = bus.read()?;
        if message.kind == SIGNAL && message.member.as_deref() == Some("SettingChanged") {
            let mut r = Reader::new(&message.body, message.big_endian);
            let (namespace, key) = (r.string()?, r.string()?);
            if let Some(value) = setting(namespace, key, &mut r) {
                changed(value);
            }
        }
    }
}

const METHOD_CALL: u8 = 1;
pub const METHOD_RETURN: u8 = 2;
pub const SIGNAL: u8 = 4;

/// A connection to the session bus. (`portal` speaks through it too.)
pub struct Bus {
    pub stream: UnixStream,
    reader: BufReader<UnixStream>,
    serial: u32,
}

/// A received message: what this client looks at.
pub struct Message {
    pub kind: u8,
    pub big_endian: bool,
    pub reply_serial: Option<u32>,
    pub member: Option<String>,
    /// The object path, which a signal comes from.
    pub path: Option<String>,
    pub body: Vec<u8>,
}

impl Bus {
    pub fn connect() -> Option<Bus> {
        let stream = connect_session_bus()?;
        stream.set_read_timeout(Some(ANSWER_TIMEOUT)).ok()?;
        stream.set_write_timeout(Some(ANSWER_TIMEOUT)).ok()?;
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut writer = stream.try_clone().ok()?;
        // The EXTERNAL mechanism: the server checks our uid on the socket.
        let uid = std::fs::metadata("/proc/self").ok()?.uid();
        let hex: String = uid.to_string().bytes().map(|b| format!("{b:02x}")).collect();
        writer.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes()).ok()?;
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        if !line.starts_with("OK ") {
            return None;
        }
        writer.write_all(b"BEGIN\r\n").ok()?;
        Some(Bus { stream, reader, serial: 0 })
    }

    /// Call a method and wait for its reply; None for an error reply.
    pub fn call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        signature: &str,
        args: impl FnOnce(&mut Writer),
    ) -> Option<Message> {
        self.serial += 1;
        let serial = self.serial;
        let bytes = method_call(serial, destination, path, interface, member, signature, args);
        self.stream.write_all(&bytes).ok()?;
        loop {
            let message = self.read()?;
            if message.reply_serial == Some(serial) {
                return (message.kind == METHOD_RETURN).then_some(message);
            }
        }
    }

    pub fn read(&mut self) -> Option<Message> {
        let mut fixed = [0u8; 16];
        self.reader.read_exact(&mut fixed).ok()?;
        let big_endian = match fixed[0] {
            b'l' => false,
            b'B' => true,
            _ => return None,
        };
        let word = |at: usize| {
            let b = [fixed[at], fixed[at + 1], fixed[at + 2], fixed[at + 3]];
            if big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
        };
        let (body_len, fields_len) = (word(4) as usize, word(12) as usize);
        // Bounded: nothing this client reads is anywhere near this.
        if body_len > 1 << 20 || fields_len > 1 << 16 {
            return None;
        }
        let mut rest = vec![0u8; align(16 + fields_len, 8) - 16 + body_len];
        self.reader.read_exact(&mut rest).ok()?;
        let mut header = fixed.to_vec();
        header.extend_from_slice(&rest[..fields_len]);
        let (reply_serial, member, path) = header_fields(&header, big_endian)?;
        let body = rest[align(16 + fields_len, 8) - 16..].to_vec();
        Some(Message { kind: fixed[1], big_endian, reply_serial, member, path, body })
    }
}

/// The session bus's socket, from `DBUS_SESSION_BUS_ADDRESS` or the usual
/// place in the runtime directory.
fn connect_session_bus() -> Option<UnixStream> {
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().or_else(|| {
        let dir = std::env::var("XDG_RUNTIME_DIR").ok()?;
        Some(format!("unix:path={dir}/bus"))
    })?;
    for candidate in address.split(';') {
        let Some(params) = candidate.strip_prefix("unix:") else { continue };
        for param in params.split(',') {
            if let Some(path) = param.strip_prefix("path=") {
                if let Ok(stream) = UnixStream::connect(unescape(path)) {
                    return Some(stream);
                }
            } else if let Some(name) = param.strip_prefix("abstract=") {
                use std::os::linux::net::SocketAddrExt;
                let addr = std::os::unix::net::SocketAddr::from_abstract_name(unescape(name).as_bytes()).ok()?;
                if let Ok(stream) = UnixStream::connect_addr(&addr) {
                    return Some(stream);
                }
            }
        }
    }
    None
}

/// D-Bus addresses escape bytes as `%xx`.
fn unescape(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn align(n: usize, to: usize) -> usize {
    n.div_ceil(to) * to
}

/// Writes little-endian D-Bus values, aligned from the message's start.
#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn pad(&mut self, to: usize) {
        self.buf.resize(align(self.buf.len(), to), 0);
    }

    pub fn byte(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u32(&mut self, v: u32) {
        self.pad(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    pub fn signature(&mut self, s: &str) {
        self.byte(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// A header field: its code and a variant of type `kind` ('s', 'o',
    /// 'g').
    fn field(&mut self, code: u8, kind: char, value: &str) {
        self.pad(8);
        self.byte(code);
        self.signature(&kind.to_string());
        if kind == 'g' { self.signature(value) } else { self.string(value) }
    }
}

fn method_call(
    serial: u32,
    destination: &str,
    path: &str,
    interface: &str,
    member: &str,
    signature: &str,
    args: impl FnOnce(&mut Writer),
) -> Vec<u8> {
    let mut body = Writer::default();
    args(&mut body);
    let mut w = Writer::default();
    w.byte(b'l');
    w.byte(METHOD_CALL);
    w.byte(0);
    w.byte(1);
    w.u32(body.buf.len() as u32);
    w.u32(serial);
    // The header fields, an array of (byte, variant): its length, then
    // the structs from an 8-byte boundary.
    w.u32(0);
    let length_at = w.buf.len() - 4;
    w.pad(8);
    let start = w.buf.len();
    w.field(1, 'o', path);
    w.field(2, 's', interface);
    w.field(3, 's', member);
    w.field(6, 's', destination);
    if !signature.is_empty() {
        w.field(8, 'g', signature);
    }
    let fields = (w.buf.len() - start) as u32;
    w.buf[length_at..length_at + 4].copy_from_slice(&fields.to_le_bytes());
    w.pad(8);
    w.buf.extend_from_slice(&body.buf);
    w.buf
}

/// Reads D-Bus values from a body or header, aligned from its start.
pub struct Reader<'a> {
    pub buf: &'a [u8],
    pub at: usize,
    big_endian: bool,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8], big_endian: bool) -> Self {
        Reader { buf, at: 0, big_endian }
    }

    pub fn pad(&mut self, to: usize) {
        self.at = align(self.at, to);
    }

    pub fn byte(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.at)?;
        self.at += 1;
        Some(b)
    }

    pub fn u32(&mut self) -> Option<u32> {
        self.pad(4);
        let b: [u8; 4] = self.buf.get(self.at..self.at + 4)?.try_into().ok()?;
        self.at += 4;
        Some(if self.big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    }

    pub fn string(&mut self) -> Option<&'a str> {
        let len = self.u32()? as usize;
        let s = std::str::from_utf8(self.buf.get(self.at..self.at + len)?).ok()?;
        self.at += len + 1;
        Some(s)
    }

    pub fn signature(&mut self) -> Option<&'a str> {
        let len = self.byte()? as usize;
        let s = std::str::from_utf8(self.buf.get(self.at..self.at + len)?).ok()?;
        self.at += len + 1;
        Some(s)
    }

    /// A variant holding a u32, possibly inside more variants.
    #[cfg(test)]
    fn variant_u32(&mut self) -> Option<u32> {
        match self.signature()? {
            "u" => self.u32(),
            "v" => self.variant_u32(),
            _ => None,
        }
    }

    fn f64(&mut self) -> Option<f64> {
        self.pad(8);
        let b: [u8; 8] = self.buf.get(self.at..self.at + 8)?.try_into().ok()?;
        self.at += 8;
        Some(f64::from_bits(if self.big_endian { u64::from_be_bytes(b) } else { u64::from_le_bytes(b) }))
    }

    /// A variant holding one of the values settings have, possibly inside
    /// more variants.
    fn variant(&mut self) -> Option<Value> {
        Some(match self.signature()? {
            "u" => Value::U32(self.u32()?),
            "i" => Value::I32(self.u32()? as i32),
            "b" => Value::Bool(self.u32()? != 0),
            "d" => Value::F64(self.f64()?),
            "s" => Value::Str(self.string()?.to_owned()),
            "(ddd)" => Value::Rgb([self.f64()?, self.f64()?, self.f64()?]),
            "v" => self.variant()?,
            _ => return None,
        })
    }
}

/// A setting's value.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    U32(u32),
    I32(i32),
    Bool(bool),
    F64(f64),
    Str(String),
    Rgb([f64; 3]),
}

/// The reply serial, member and path among a header's fields.
type Fields = (Option<u32>, Option<String>, Option<String>);

fn header_fields(header: &[u8], big_endian: bool) -> Option<Fields> {
    let mut r = Reader::new(header, big_endian);
    r.at = 12;
    let len = r.u32()? as usize;
    r.pad(8);
    let end = r.at + len;
    let (mut reply_serial, mut member, mut path) = (None, None, None);
    while r.at < end {
        r.pad(8);
        let code = r.byte()?;
        let kind = r.signature()?;
        match (code, kind) {
            (5, "u") => reply_serial = Some(r.u32()?),
            (3, "s") => member = Some(r.string()?.to_owned()),
            (1, "o") => path = Some(r.string()?.to_owned()),
            (_, "s" | "o") => {
                r.string()?;
            }
            (_, "g") => {
                r.signature()?;
            }
            (_, "u") => {
                r.u32()?;
            }
            _ => return None,
        }
    }
    Some((reply_serial, member, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_calls_are_laid_out_as_the_specification_says() {
        let bytes = method_call(7, "d.x", "/p", "i.x", "M", "s", |w| w.string("ab"));
        // Fixed part: little-endian, a method call, no flags, version 1,
        // an 7-byte body (length, "ab", NUL), serial 7.
        assert_eq!(&bytes[..4], b"l\x01\x00\x01");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 7);
        let fields = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        // The path field: code 1, signature "o", then the path.
        assert_eq!(&bytes[16..24], b"\x01\x01o\x00\x02\x00\x00\x00");
        assert_eq!(&bytes[24..27], b"/p\x00");
        // The body starts at the next 8-byte boundary after the fields.
        let body = align(16 + fields, 8);
        assert_eq!(bytes.len(), body + 7);
        assert_eq!(&bytes[body..], b"\x02\x00\x00\x00ab\x00");
        // And the header reads back.
        let (reply, member, path) = header_fields(&bytes[..16 + fields], false).unwrap();
        assert_eq!((reply, member.as_deref(), path.as_deref()), (None, Some("M"), Some("/p")));
    }

    #[test]
    fn variants_read_through_nesting() {
        // A variant holding a u32 1 (ReadOne's answer).
        let one = b"\x01u\x00\x00\x01\x00\x00\x00";
        assert_eq!(Reader::new(one, false).variant_u32(), Some(1));
        // A variant holding that variant (Read's answer): the inner
        // signature follows at once, its value aligned from the start.
        let nested = b"\x01v\x00\x01u\x00\x00\x00\x02\x00\x00\x00";
        assert_eq!(Reader::new(nested, false).variant_u32(), Some(2));
        let big = b"\x01u\x00\x00\x00\x00\x00\x01";
        assert_eq!(Reader::new(big, true).variant_u32(), Some(1));
        assert_eq!(Reader::new(b"\x01s\x00", false).variant_u32(), None);
    }

    #[test]
    fn signals_carry_the_setting() {
        let mut w = Writer::default();
        w.string(NAMESPACE);
        w.string("color-scheme");
        w.signature("u");
        w.u32(2);
        let mut r = Reader::new(&w.buf, false);
        assert_eq!(r.string(), Some(NAMESPACE));
        assert_eq!(r.string(), Some("color-scheme"));
        assert_eq!(r.variant_u32().map(Scheme::from_portal), Some(Scheme::Light));
    }

    #[test]
    fn accents_and_contrast_decode() {
        let mut w = Writer::default();
        w.signature("(ddd)");
        for v in [0.2f64, 0.4, 0.6] {
            w.pad(8);
            w.buf.extend_from_slice(&v.to_le_bytes());
        }
        let got = setting(NAMESPACE, "accent-color", &mut Reader::new(&w.buf, false));
        assert_eq!(got, Some(Setting::Accent(Some([0.2, 0.4, 0.6]))));
        // Out of range: no accent.
        let mut w = Writer::default();
        w.signature("(ddd)");
        for v in [-1.0f64, -1.0, -1.0] {
            w.pad(8);
            w.buf.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(setting(NAMESPACE, "accent-color", &mut Reader::new(&w.buf, false)), Some(Setting::Accent(None)));
        let one = b"\x01u\x00\x00\x01\x00\x00\x00";
        assert_eq!(setting(NAMESPACE, "contrast", &mut Reader::new(one, false)), Some(Setting::Contrast(true)));
    }

    #[test]
    fn interface_settings_decode() {
        let mut w = Writer::default();
        w.signature("d");
        w.pad(8);
        w.buf.extend_from_slice(&1.25f64.to_le_bytes());
        assert_eq!(
            setting(INTERFACE, "text-scaling-factor", &mut Reader::new(&w.buf, false)),
            Some(Setting::TextScale(1.25))
        );
        let mut w = Writer::default();
        w.signature("v");
        w.signature("s");
        w.string("Cantarell 11");
        assert_eq!(
            setting(INTERFACE, "font-name", &mut Reader::new(&w.buf, false)),
            Some(Setting::Font("Cantarell 11".into()))
        );
        let b = b"\x01b\x00\x00\x00\x00\x00\x00";
        assert_eq!(
            setting(INTERFACE, "enable-animations", &mut Reader::new(b, false)),
            Some(Setting::Animations(false))
        );
        let i = b"\x01i\x00\x00\x18\x00\x00\x00";
        assert_eq!(setting(INTERFACE, "cursor-size", &mut Reader::new(i, false)), Some(Setting::CursorSize(24)));
        // A key of the wrong type, or another namespace's, is nothing.
        assert_eq!(setting(NAMESPACE, "cursor-size", &mut Reader::new(i, false)), None);
    }

    #[test]
    fn addresses_unescape() {
        assert_eq!(unescape("/run/user/1000/bus"), "/run/user/1000/bus");
        assert_eq!(unescape("/tmp/a%2cb"), "/tmp/a,b");
        assert_eq!(unescape("50%"), "50%");
    }
}
