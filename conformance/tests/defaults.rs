//! `NSUserDefaults`, checked on macOS and on Linux: the typed getters'
//! coercions, registration, removal, suites, the change notification, URL
//! values and domains. Every check uses a suite of its own, which it
//! removes afterwards.
//!
//! On Linux the defaults live under `$XDG_CONFIG_HOME`, which this test
//! points at a scratch directory before anything reads it, and it also
//! checks the files: the exact XML written by `synchronize`, the write
//! behind it, and the flush at exit (in a child process, which also shows
//! the argument domain).

use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, msg_send};
use objc2_foundation::{
    NSData, NSDate, NSDictionary, NSNotification, NSNotificationCenter, NSString, NSURL, NSUserDefaults,
};

use sidestep as _;

fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn suite_name(test: &str) -> String {
    format!("org.sidestep.defaults-test.{}.{test}", std::process::id())
}

fn suite(test: &str) -> Retained<NSUserDefaults> {
    NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&ns(&suite_name(test)))).unwrap()
}

fn done(defaults: &NSUserDefaults, test: &str) {
    defaults.removePersistentDomainForName(&ns(&suite_name(test)));
}

fn text(value: Option<Retained<NSString>>) -> Option<String> {
    value.map(|v| v.to_string())
}

fn dictionary(entries: &[(&str, Retained<AnyObject>)]) -> Retained<NSDictionary<NSString, AnyObject>> {
    let keys: Vec<Retained<NSString>> = entries.iter().map(|(k, _)| ns(k)).collect();
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<Retained<AnyObject>> = entries.iter().map(|(_, v)| v.clone()).collect();
    NSDictionary::from_retained_objects(&keys, &values)
}

fn coercions() {
    // SAFETY: the setters take property lists, which these are.
    unsafe {
        let d = suite("coercions");
        for (value, int, double, boolean) in [
            ("42", 42, 42.0, false),
            ("abc", 0, 0.0, false),
            ("3.7", 0, 3.7, false),
            ("YES", 0, 0.0, true),
            ("yes", 0, 0.0, true),
            ("true", 0, 0.0, true),
            ("1", 1, 1.0, true),
            ("NO", 0, 0.0, false),
            ("2", 2, 2.0, false),
            (" 5 ", 0, 5.0, false),
            ("Y", 0, 0.0, false),
            ("-3", -3, -3.0, false),
            ("1e3", 0, 1000.0, false),
            ("0x10", 0, 0.0, false),
        ] {
            let key = ns("k");
            d.setObject_forKey(Some(&ns(value)), &key);
            assert_eq!(d.integerForKey(&key), int, "integerForKey of {value:?}");
            assert_eq!(d.doubleForKey(&key), double, "doubleForKey of {value:?}");
            assert_eq!(d.floatForKey(&key), double as f32, "floatForKey of {value:?}");
            assert_eq!(d.boolForKey(&key), boolean, "boolForKey of {value:?}");
            assert_eq!(text(d.stringForKey(&key)).as_deref(), Some(value));
            assert!(d.dataForKey(&key).is_none());
            assert!(d.dictionaryForKey(&key).is_none());
        }
        d.setBool_forKey(true, &ns("bool"));
        assert!(d.boolForKey(&ns("bool")));
        assert_eq!(d.integerForKey(&ns("bool")), 1);
        assert_eq!(text(d.stringForKey(&ns("bool"))).as_deref(), Some("1"));
        d.setInteger_forKey(7, &ns("int"));
        assert!(d.boolForKey(&ns("int")));
        assert_eq!(d.doubleForKey(&ns("int")), 7.0);
        assert_eq!(text(d.stringForKey(&ns("int"))).as_deref(), Some("7"));
        d.setDouble_forKey(-2.9, &ns("double"));
        assert_eq!(d.integerForKey(&ns("double")), -2, "truncated");
        assert!(d.boolForKey(&ns("double")));
        assert_eq!(text(d.stringForKey(&ns("double"))).as_deref(), Some("-2.9"));
        d.setFloat_forKey(0.5, &ns("float"));
        assert_eq!(d.floatForKey(&ns("float")), 0.5);
        assert_eq!(d.integerForKey(&ns("float")), 0);
        let missing = ns("missing");
        assert!(d.objectForKey(&missing).is_none());
        assert_eq!((d.integerForKey(&missing), d.doubleForKey(&missing), d.boolForKey(&missing)), (0, 0.0, false));
        assert!(d.stringForKey(&missing).is_none());

        d.setObject_forKey(Some(&NSData::with_bytes(b"xy")), &ns("data"));
        assert_eq!(d.dataForKey(&ns("data")).unwrap().to_vec(), b"xy");
        assert!(d.stringForKey(&ns("data")).is_none());
        d.setObject_forKey(Some(&dictionary(&[("k", ns("v").into())])), &ns("dict"));
        assert_eq!(d.dictionaryForKey(&ns("dict")).unwrap().count(), 1);
        d.setObject_forKey(Some(&NSDate::dateWithTimeIntervalSinceReferenceDate(5.0)), &ns("date"));
        let date = d.objectForKey(&ns("date")).unwrap().downcast::<NSDate>().unwrap();
        assert_eq!(date.timeIntervalSinceReferenceDate(), 5.0);
        assert_eq!(d.integerForKey(&ns("date")), 0);
        d.setObject_forKey(None, &ns("data"));
        assert!(d.objectForKey(&ns("data")).is_none(), "setting nil removes");
        done(&d, "coercions");
    }
}

#[cfg(any(target_vendor = "apple", feature = "collections"))]
fn numbers_and_arrays() {
    // SAFETY: the setters take property lists, which these are.
    unsafe {
        use objc2_foundation::{NSArray, NSNumber};
        let d = suite("numbers");
        d.setObject_forKey(Some(&NSNumber::new_f64(2.9)), &ns("real"));
        assert_eq!(d.integerForKey(&ns("real")), 2);
        assert_eq!(text(d.stringForKey(&ns("real"))).as_deref(), Some("2.9"));
        d.setInteger_forKey(7, &ns("int"));
        let number = d.objectForKey(&ns("int")).unwrap().downcast::<NSNumber>().unwrap();
        assert_eq!(number.integerValue(), 7);
        d.setObject_forKey(Some(&NSArray::from_retained_slice(&[ns("a"), ns("b")])), &ns("strings"));
        assert_eq!(d.arrayForKey(&ns("strings")).unwrap().count(), 2);
        assert_eq!(d.stringArrayForKey(&ns("strings")).unwrap().count(), 2);
        d.setObject_forKey(Some(&NSArray::from_retained_slice(&[NSNumber::new_i64(1)])), &ns("numbers"));
        assert_eq!(d.arrayForKey(&ns("numbers")).unwrap().count(), 1);
        assert!(d.stringArrayForKey(&ns("numbers")).is_none(), "not all strings");
        assert!(d.arrayForKey(&ns("int")).is_none());
        done(&d, "numbers");
    }
}

fn registration_and_suites() {
    // SAFETY: the setters take property lists, which these are.
    unsafe {
        let d = suite("registration");
        let registered = format!("registered-{}", std::process::id());
        d.registerDefaults(&dictionary(&[(&registered, ns("regval").into()), ("overridden", ns("regabc").into())]));
        assert_eq!(text(d.stringForKey(&ns(&registered))).as_deref(), Some("regval"));
        d.setObject_forKey(Some(&ns("abc")), &ns("overridden"));
        assert_eq!(text(d.stringForKey(&ns("overridden"))).as_deref(), Some("abc"));
        d.removeObjectForKey(&ns("overridden"));
        assert_eq!(
            text(d.stringForKey(&ns("overridden"))).as_deref(),
            Some("regabc"),
            "removal falls back to registration"
        );
        d.setObject_forKey(Some(&ns("set")), &ns("shared"));

        let other = suite("registration");
        assert_eq!(text(other.stringForKey(&ns("shared"))).as_deref(), Some("set"), "instances share a suite's domain");
        assert_eq!(text(other.stringForKey(&ns(&registered))).as_deref(), Some("regval"));
        let standard = NSUserDefaults::standardUserDefaults();
        assert!(std::ptr::eq(&*standard, &*NSUserDefaults::standardUserDefaults()));
        assert_eq!(
            text(standard.stringForKey(&ns(&registered))).as_deref(),
            Some("regval"),
            "registration is process-wide"
        );
        assert!(standard.stringForKey(&ns("shared")).is_none(), "the suite isn't in the standard search list");
        standard.addSuiteNamed(&ns(&suite_name("registration")));
        assert_eq!(text(standard.stringForKey(&ns("shared"))).as_deref(), Some("set"));
        standard.removeSuiteNamed(&ns(&suite_name("registration")));
        assert!(standard.stringForKey(&ns("shared")).is_none());

        let everything = d.dictionaryRepresentation();
        assert!(everything.objectForKey(&ns(&registered)).is_some());
        assert!(everything.objectForKey(&ns("shared")).is_some());
        assert!(!d.objectIsForcedForKey(&ns("shared")));
        assert!(NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&ns("NSGlobalDomain"))).is_none());
        assert!(d.synchronize());
        done(&d, "registration");
        assert!(other.stringForKey(&ns("shared")).is_none(), "removing the domain removes its values");
    }
}

fn domains() {
    // SAFETY: the setters take property lists, which these are.
    unsafe {
        let d = suite("domains");
        let name = format!("{}.other", suite_name("domains"));
        d.setPersistentDomain_forName(&dictionary(&[("pk", ns("pv").into())]), &ns(&name));
        let domain = d.persistentDomainForName(&ns(&name)).unwrap();
        assert_eq!(domain.count(), 1);
        assert_eq!(
            domain.objectForKey(&ns("pk")).map(|v| v.downcast::<NSString>().unwrap().to_string()).as_deref(),
            Some("pv")
        );
        d.removePersistentDomainForName(&ns(&name));
        assert!(d.persistentDomainForName(&ns(&name)).is_none());

        d.setVolatileDomain_forName(&dictionary(&[("vk", ns("vv").into())]), &ns("SidestepVolatile"));
        assert_eq!(d.volatileDomainForName(&ns("SidestepVolatile")).count(), 1);
        assert!(d.stringForKey(&ns("vk")).is_none(), "volatile domains aren't searched");
        d.removeVolatileDomainForName(&ns("SidestepVolatile"));
        done(&d, "domains");
    }
}

fn change_notification() {
    // SAFETY: the setters take property lists, which these are.
    unsafe {
        let d = suite("notification");
        let posts = Rc::new(Cell::new(0));
        let expected: *const NSUserDefaults = &*d;
        let block = RcBlock::new({
            let posts = posts.clone();
            move |note: NonNull<NSNotification>| {
                let note = note.as_ref();
                let object = note.object().map(|o| Retained::as_ptr(&o).cast::<NSUserDefaults>());
                if object == Some(expected) {
                    posts.set(posts.get() + 1);
                }
            }
        });
        let center = NSNotificationCenter::defaultCenter();
        let name = objc2_foundation::NSUserDefaultsDidChangeNotification;
        assert_eq!(name.to_string(), "NSUserDefaultsDidChangeNotification");
        let token = center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block);
        d.setObject_forKey(Some(&ns("v")), &ns("k"));
        assert!(posts.get() >= 1, "posted before the setter returns, on this thread");
        let before = posts.get();
        d.removeObjectForKey(&ns("k"));
        assert!(posts.get() > before);
        center.removeObserver(&*(Retained::as_ptr(&token).cast::<AnyObject>()));
        done(&d, "notification");
    }
}

fn urls() {
    // SAFETY: the setters take property lists, which these are.
    unsafe {
        let d = suite("urls");
        let home = objc2_foundation::NSHomeDirectory().to_string();
        let http = NSURL::URLWithString(&ns("http://example.com/a%20b")).unwrap();
        d.setURL_forKey(Some(&http), &ns("http"));
        assert!(d.objectForKey(&ns("http")).unwrap().downcast::<NSData>().is_ok(), "other URLs are archived");
        assert_eq!(
            d.URLForKey(&ns("http")).and_then(|u| text(u.absoluteString())).as_deref(),
            Some("http://example.com/a%20b")
        );
        d.setURL_forKey(
            Some(&NSURL::fileURLWithPath_isDirectory(&ns(&format!("{home}/Documents/x.txt")), false)),
            &ns("file"),
        );
        assert_eq!(
            text(d.stringForKey(&ns("file"))).as_deref(),
            Some("~/Documents/x.txt"),
            "file URLs are abbreviated paths"
        );
        assert_eq!(d.URLForKey(&ns("file")).and_then(|u| text(u.path())), Some(format!("{home}/Documents/x.txt")));
        d.setURL_forKey(Some(&NSURL::fileURLWithPath_isDirectory(&ns("/tmp/y"), false)), &ns("tmp"));
        assert_eq!(text(d.stringForKey(&ns("tmp"))).as_deref(), Some("/tmp/y"));
        d.setObject_forKey(Some(&ns("~/Library/foo")), &ns("tilde"));
        assert_eq!(d.URLForKey(&ns("tilde")).and_then(|u| text(u.path())), Some(format!("{home}/Library/foo")));
        d.setObject_forKey(Some(&ns("/abs/path")), &ns("abs"));
        assert_eq!(d.URLForKey(&ns("abs")).and_then(|u| text(u.absoluteString())).as_deref(), Some("file:///abs/path"));
        d.setObject_forKey(Some(&ns("42")), &ns("relative"));
        let cwd = std::env::current_dir().unwrap();
        let relative = d.URLForKey(&ns("relative")).unwrap();
        assert!(relative.isFileURL());
        assert_eq!(
            std::path::PathBuf::from(text(relative.path()).unwrap()).file_name(),
            cwd.join("42").file_name(),
            "a relative path is taken from the current directory"
        );
        d.setURL_forKey(None, &ns("tmp"));
        assert!(d.objectForKey(&ns("tmp")).is_none());
        done(&d, "urls");
    }
}

/// Linux: what `synchronize` writes, and the write behind it.
#[cfg(target_os = "linux")]
fn files(config: &std::path::Path) {
    // SAFETY: as above.
    unsafe {
        let d = suite("files");
        let file = config.join(suite_name("files")).join("defaults.plist");
        d.setObject_forKey(Some(&ns("last")), &ns("zeta"));
        d.setObject_forKey(Some(&dictionary(&[])), &ns("alpha"));
        d.setObject_forKey(Some(&ns("<&>")), &ns("esc"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !file.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(file.exists(), "written behind, without synchronize");
        d.setBool_forKey(false, &ns("mid"));
        d.setInteger_forKey(-7, &ns("neg"));
        d.setDouble_forKey(0.1, &ns("real"));
        assert!(d.synchronize());
        let written = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            written,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>alpha</key>\n\t<dict/>\n\t<key>esc</key>\n\t<string>&lt;&amp;&gt;</string>\n\t<key>mid</key>\n\t<false/>\n\t<key>neg</key>\n\t<integer>-7</integer>\n\t<key>real</key>\n\t<real>0.10000000000000001</real>\n\t<key>zeta</key>\n\t<string>last</string>\n</dict>\n</plist>\n"
        );
        done(&d, "files");
        assert!(d.synchronize());
        assert!(!file.exists(), "a removed domain's file goes");
    }
}

/// The child process: reads the argument domain and sets a value that only
/// the exit flush can save.
fn child(suite: &str) -> ! {
    // SAFETY: as above.
    unsafe {
        let standard = NSUserDefaults::standardUserDefaults();
        assert_eq!(text(standard.stringForKey(&ns("SidestepArgument"))).as_deref(), Some("from the command line"));
        let d = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&ns(suite))).unwrap();
        d.setObject_forKey(Some(&ns("saved at exit")), &ns("k"));
        d.setObject_forKey(Some(&ns("from the suite")), &ns("SidestepArgument"));
        assert_eq!(
            text(d.stringForKey(&ns("SidestepArgument"))).as_deref(),
            Some("from the command line"),
            "the argument domain comes first"
        );
        std::process::exit(0)
    }
}

/// Linux: the child process of `exit_during_a_write`, which changes a big
/// domain and exits `delay` milliseconds later, while the writer thread may
/// be writing it.
#[cfg(target_os = "linux")]
fn late_child(suite: &str, delay: u64) -> ! {
    // SAFETY: as above.
    unsafe {
        let d = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&ns(suite))).unwrap();
        d.setObject_forKey(Some(&ns(&"x".repeat(4 << 20))), &ns("big"));
        d.setObject_forKey(Some(&ns("written")), &ns("k"));
    }
    std::thread::sleep(std::time::Duration::from_millis(delay));
    std::process::exit(0)
}

/// Linux: a process that exits while its defaults are being written still
/// leaves them complete on disk, with no temporary file behind.
#[cfg(target_os = "linux")]
fn exit_during_a_write(config: &std::path::Path) {
    let name = suite_name("late");
    let dir = config.join(&name);
    for delay in [0, 40, 50, 55, 60, 70, 90] {
        let _ = std::fs::remove_dir_all(&dir);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .env("SIDESTEP_DEFAULTS_LATE_CHILD", &name)
            .env("SIDESTEP_DEFAULTS_DELAY", delay.to_string())
            .status()
            .unwrap();
        assert!(status.success());
        let written = std::fs::read_to_string(dir.join("defaults.plist")).expect("written by exit");
        assert!(written.contains("<string>written</string>") && written.ends_with("</plist>\n"), "delay {delay}");
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(entries, [std::ffi::OsString::from("defaults.plist")], "delay {delay}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

fn arguments_and_exit(#[allow(unused)] config: Option<&std::path::Path>) {
    let name = suite_name("child");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .env("SIDESTEP_DEFAULTS_CHILD", &name)
        .args(["-SidestepArgument", "from the command line"])
        .status()
        .unwrap();
    assert!(status.success(), "the child's checks pass");
    #[cfg(target_os = "linux")]
    {
        let file = config.unwrap().join(&name).join("defaults.plist");
        let written = std::fs::read_to_string(&file).expect("flushed at exit");
        assert!(written.contains("<string>saved at exit</string>"), "{written}");
    }
    let d = suite("child");
    d.removePersistentDomainForName(&ns(&name));
    let _: bool = unsafe { msg_send![&*d, synchronize] };
}

fn main() {
    if let Ok(suite) = std::env::var("SIDESTEP_DEFAULTS_CHILD") {
        child(&suite);
    }
    #[cfg(target_os = "linux")]
    if let Ok(suite) = std::env::var("SIDESTEP_DEFAULTS_LATE_CHILD") {
        let delay = std::env::var("SIDESTEP_DEFAULTS_DELAY").unwrap().parse().unwrap();
        late_child(&suite, delay);
    }
    #[cfg(target_os = "linux")]
    let config = {
        let dir = std::env::temp_dir().join(format!("sidestep-defaults-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: nothing else runs yet; the defaults read this on first use.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
        dir
    };
    let checks: &[(&str, fn())] = &[
        ("coercions", coercions),
        #[cfg(any(target_vendor = "apple", feature = "collections"))]
        ("numbers_and_arrays", numbers_and_arrays),
        ("registration_and_suites", registration_and_suites),
        ("domains", domains),
        ("change_notification", change_notification),
        ("urls", urls),
    ];
    for (name, check) in checks {
        check();
        println!("test {name} ... ok");
    }
    #[cfg(target_os = "linux")]
    {
        files(&config);
        println!("test files ... ok");
        exit_during_a_write(&config);
        println!("test exit_during_a_write ... ok");
        arguments_and_exit(Some(&config));
        std::fs::remove_dir_all(&config).unwrap();
    }
    #[cfg(not(target_os = "linux"))]
    arguments_and_exit(None);
    println!("test arguments_and_exit ... ok");
}
