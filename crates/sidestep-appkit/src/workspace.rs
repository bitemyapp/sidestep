//! `NSWorkspace`, `NSRunningApplication` and `NSBeep`: the desktop around
//! the program.
//!
//! `NSWorkspace` opens URLs and shows files through xdg-desktop-portal
//! (`portal`, on its own thread: `openURL:` returns at once, NO when the
//! URL has no scheme, nothing handles it, or it names a file that isn't
//! there, as on macOS). `openURL:configuration:completionHandler:` takes an
//! `NSWorkspaceOpenConfiguration` (its settings are kept, with macOS's
//! defaults, but Linux has no use for them) and calls the handler later,
//! off the main thread, as AppKit does: with no application (Sidestep
//! can't name another program's) and, when nothing opens the URL,
//! `NSCocoaErrorDomain`'s 256 (260 for a missing file). It answers the accessibility
//! display options from the desktop's settings (less motion when GNOME's
//! animations are off, more contrast with its high-contrast setting), and
//! has a notification center of its own, as on macOS. Wayland shows a
//! program nothing of other programs, so the workspace knows only this
//! one: it is the front application while active, and the only one
//! running.
//!
//! `NSRunningApplication` describes this program (`currentApplication`):
//! its process, its executable's name and URL, its bundle identifier (the
//! main bundle's, else the desktop entry it was launched from, else none),
//! and the application's state.
//!
//! `NSBeep` makes no sound: the platform doesn't bind a bell.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::time::SystemTime;

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadMarker, Message, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy, NSImage, NSRunningApplication,
    NSWorkspace, NSWorkspaceOpenConfiguration,
};
use objc2_foundation::{
    NSArray, NSCopying, NSDate, NSDictionary, NSError, NSNotificationCenter, NSString, NSURL, NSZone,
};

sidestep_runtime::static_class!(pub NSWORKSPACE, NSWORKSPACE_META = "NSWorkspace", || {
    let _ = NSWorkspaceImpl::class();
});

sidestep_runtime::static_class!(
    pub NSWORKSPACEOPENCONFIGURATION,
    NSWORKSPACEOPENCONFIGURATION_META = "NSWorkspaceOpenConfiguration",
    || {
        let _ = NSWorkspaceOpenConfigurationImpl::class();
    }
);

sidestep_runtime::static_class!(pub NSRUNNINGAPPLICATION, NSRUNNINGAPPLICATION_META = "NSRunningApplication", || {
    let _ = NSRunningApplicationImpl::class();
});

sidestep_foundation::constant_string!(
    NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification =
        "NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification"
);

/// A pointer to an object that lives as long as the program.
struct Immortal(*const AnyObject);

// SAFETY: the objects kept this way are made once, never released, and
// their classes may be used from any thread.
unsafe impl Send for Immortal {}
// SAFETY: as above.
unsafe impl Sync for Immortal {}

impl Immortal {
    fn get<T: Message>(&self) -> Retained<T> {
        // SAFETY: the pointer is to a live object of type T, never
        // released (see `keep`).
        unsafe { Retained::retain(self.0.cast::<T>().cast_mut()) }.expect("an immortal object")
    }
}

/// Keep `object` for the program's lifetime.
fn keep<T: Message>(object: Retained<T>) -> Immortal {
    Immortal(Retained::into_raw(object).cast::<AnyObject>())
}

static SHARED: OnceLock<Immortal> = OnceLock::new();
static CENTER: OnceLock<Immortal> = OnceLock::new();
static CURRENT: OnceLock<Immortal> = OnceLock::new();
/// When the program started, as near as Sidestep can tell (its first look).
static LAUNCHED: OnceLock<SystemTime> = OnceLock::new();

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSWorkspace"]
    pub(crate) struct NSWorkspaceImpl;

    impl NSWorkspaceImpl {
        /// One for the program, made at the first call.
        #[unsafe(method_id(sharedWorkspace))]
        fn shared_workspace() -> Retained<NSWorkspace> {
            SHARED
                .get_or_init(|| {
                    // Answers can take a while: start asking early.
                    crate::portal::prewarm();
                    // SAFETY: NSObject's initializer.
                    let w: Retained<NSWorkspace> = unsafe { msg_send![NSWorkspace::alloc(), init] };
                    keep(w)
                })
                .get()
        }

        /// A notification center of the workspace's own.
        #[unsafe(method_id(notificationCenter))]
        fn notification_center(&self) -> Retained<NSNotificationCenter> {
            CENTER.get_or_init(|| keep(NSNotificationCenter::new())).get()
        }

        #[unsafe(method(openURL:))]
        fn open_url(&self, url: &NSURL) -> bool {
            open(url, None).is_ok()
        }

        /// As `openURL:`, the handler called later off the main thread.
        #[unsafe(method(openURL:configuration:completionHandler:))]
        fn open_url_configuration(
            &self,
            url: &NSURL,
            _configuration: &NSWorkspaceOpenConfiguration,
            handler: Option<&DynBlock<dyn Fn(*mut NSRunningApplication, *mut NSError)>>,
        ) {
            let handler = handler.map(|h| Handler(h.copy()));
            let done = handler.clone().map(|h| -> crate::portal::Done {
                Box::new(move |opened| h.call(if opened { Ok(()) } else { Err(256) }))
            });
            if let Err(code) = open(url, done)
                && let Some(h) = handler
            {
                crate::portal::later(Box::new(move || h.call(Err(code))));
            }
        }

        /// Files open with what the desktop has for them; NO for a file
        /// that isn't there.
        #[unsafe(method(openFile:))]
        fn open_file(&self, path: &NSString) -> bool {
            let path = path.to_string();
            std::path::Path::new(&path).exists() && crate::portal::open_uri(&crate::portal::file_uri(&path), None)
        }

        #[unsafe(method(activateFileViewerSelectingURLs:))]
        fn activate_file_viewer_selecting_urls(&self, urls: &NSArray<NSURL>) {
            let uris: Vec<String> = urls.iter().filter_map(|u| u.absoluteString()).map(|s| s.to_string()).collect();
            if !uris.is_empty() {
                crate::portal::show_items(uris);
            }
        }

        #[unsafe(method(selectFile:inFileViewerRootedAtPath:))]
        fn select_file(&self, path: Option<&NSString>, root: &NSString) -> bool {
            let target = path.filter(|p| p.length() > 0).unwrap_or(root);
            crate::portal::show_items(vec![crate::portal::file_uri(&target.to_string())]);
            true
        }

        #[unsafe(method(isFilePackageAtPath:))]
        fn is_file_package_at_path(&self, _path: &NSString) -> bool {
            false
        }

        #[unsafe(method_id(URLForApplicationToOpenURL:))]
        fn url_for_application_to_open_url(&self, _url: &NSURL) -> Option<Retained<NSURL>> {
            None
        }

        #[unsafe(method_id(fullPathForApplication:))]
        fn full_path_for_application(&self, _name: &NSString) -> Option<Retained<NSString>> {
            None
        }

        /// A folder's or a document's symbol.
        #[unsafe(method_id(iconForFile:))]
        fn icon_for_file(&self, path: &NSString) -> Retained<NSImage> {
            let folder = std::path::Path::new(&path.to_string()).is_dir();
            let name = NSString::from_str(if folder { "folder" } else { "doc" });
            NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, None).unwrap_or_default()
        }

        #[unsafe(method_id(frontmostApplication))]
        fn frontmost_application(&self) -> Option<Retained<NSRunningApplication>> {
            active().then(current)
        }

        #[unsafe(method_id(menuBarOwningApplication))]
        fn menu_bar_owning_application(&self) -> Option<Retained<NSRunningApplication>> {
            active().then(current)
        }

        #[unsafe(method_id(runningApplications))]
        fn running_applications(&self) -> Retained<NSArray<NSRunningApplication>> {
            NSArray::from_retained_slice(&[current()])
        }

        #[unsafe(method(accessibilityDisplayShouldReduceMotion))]
        fn reduce_motion(&self) -> bool {
            !crate::settings::interface().animations
        }

        #[unsafe(method(accessibilityDisplayShouldIncreaseContrast))]
        fn increase_contrast(&self) -> bool {
            use crate::palette::Look;
            matches!(crate::appearance::system().look(), Look::LightContrast | Look::DarkContrast)
        }

        #[unsafe(method(accessibilityDisplayShouldReduceTransparency))]
        fn reduce_transparency(&self) -> bool {
            false
        }

        #[unsafe(method(accessibilityDisplayShouldInvertColors))]
        fn invert_colors(&self) -> bool {
            false
        }

        #[unsafe(method(accessibilityDisplayShouldDifferentiateWithoutColor))]
        fn differentiate_without_color(&self) -> bool {
            false
        }

        #[unsafe(method(isVoiceOverEnabled))]
        fn is_voice_over_enabled(&self) -> bool {
            false
        }

        #[unsafe(method(isSwitchControlEnabled))]
        fn is_switch_control_enabled(&self) -> bool {
            false
        }
    }

    unsafe impl NSObjectProtocol for NSWorkspaceImpl {}
);

/// Open `url`, then run `done` (on the portal thread) with whether it
/// went; the Cocoa error code if it can't: 260 for a file that isn't there,
/// 256 for a URL nothing opens.
fn open(url: &NSURL, done: Option<crate::portal::Done>) -> Result<(), isize> {
    if url.isFileURL() {
        let there = url.path().is_some_and(|p| std::path::Path::new(&p.to_string()).exists());
        if !there {
            return Err(260);
        }
    }
    let uri = url.absoluteString().ok_or(256_isize)?;
    if crate::portal::open_uri(&uri.to_string(), done) { Ok(()) } else { Err(256) }
}

/// A completion handler, called off the main thread.
#[derive(Clone)]
struct Handler(RcBlock<dyn Fn(*mut NSRunningApplication, *mut NSError)>);

// SAFETY: AppKit calls these handlers on a queue of its own, so programs
// give ones that may run on any thread; blocks' reference counts are
// atomic.
unsafe impl Send for Handler {}

impl Handler {
    /// Call it with no application, and an error for `Err`.
    fn call(&self, result: Result<(), isize>) {
        let error = result.err().map(|code| {
            // SAFETY: a domain, a code and no user info.
            unsafe { NSError::errorWithDomain_code_userInfo(&NSString::from_str("NSCocoaErrorDomain"), code, None) }
        });
        let error = error.as_ref().map_or(std::ptr::null_mut(), |e| Retained::as_ptr(e).cast_mut());
        self.0.call((std::ptr::null_mut(), error));
    }
}

pub(crate) struct ConfigurationIvars {
    prompts: Cell<bool>,
    adds_to_recents: Cell<bool>,
    activates: Cell<bool>,
    hides: Cell<bool>,
    hides_others: Cell<bool>,
    for_printing: Cell<bool>,
    new_instance: Cell<bool>,
    substitution: Cell<bool>,
    universal_links: Cell<bool>,
    arguments: RefCell<Retained<NSArray<NSString>>>,
    environment: RefCell<Retained<NSDictionary<NSString, NSString>>>,
    apple_event: RefCell<Option<Retained<AnyObject>>>,
}

impl ConfigurationIvars {
    /// macOS's defaults (`conformance/tests/panels.rs`).
    fn new() -> Self {
        ConfigurationIvars {
            prompts: Cell::new(true),
            adds_to_recents: Cell::new(true),
            activates: Cell::new(true),
            hides: Cell::new(false),
            hides_others: Cell::new(false),
            for_printing: Cell::new(false),
            new_instance: Cell::new(false),
            substitution: Cell::new(true),
            universal_links: Cell::new(false),
            arguments: RefCell::new(NSArray::new()),
            environment: RefCell::new(NSDictionary::new()),
            apple_event: RefCell::new(None),
        }
    }
}

define_class!(
    /// How `openURL:configuration:completionHandler:` opens: settings kept
    /// for the program, which Linux has no use for.
    #[unsafe(super(NSObject))]
    #[name = "NSWorkspaceOpenConfiguration"]
    #[ivars = ConfigurationIvars]
    pub(crate) struct NSWorkspaceOpenConfigurationImpl;

    impl NSWorkspaceOpenConfigurationImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ConfigurationIvars::new());
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        /// A new one each time, as on macOS.
        #[unsafe(method_id(configuration))]
        fn configuration() -> Retained<NSWorkspaceOpenConfiguration> {
            NSWorkspaceOpenConfiguration::new()
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSWorkspaceOpenConfiguration> {
            let copy = NSWorkspaceOpenConfiguration::new();
            let (mine, theirs) = (self.ivars(), config_ivars(&copy));
            for (from, to) in [
                (&mine.prompts, &theirs.prompts),
                (&mine.adds_to_recents, &theirs.adds_to_recents),
                (&mine.activates, &theirs.activates),
                (&mine.hides, &theirs.hides),
                (&mine.hides_others, &theirs.hides_others),
                (&mine.for_printing, &theirs.for_printing),
                (&mine.new_instance, &theirs.new_instance),
                (&mine.substitution, &theirs.substitution),
                (&mine.universal_links, &theirs.universal_links),
            ] {
                to.set(from.get());
            }
            drop(theirs.arguments.replace(mine.arguments.borrow().clone()));
            drop(theirs.environment.replace(mine.environment.borrow().clone()));
            drop(theirs.apple_event.replace(mine.apple_event.borrow().clone()));
            copy
        }

        #[unsafe(method(promptsUserIfNeeded))]
        fn prompts_user_if_needed(&self) -> bool {
            self.ivars().prompts.get()
        }

        #[unsafe(method(setPromptsUserIfNeeded:))]
        fn set_prompts_user_if_needed(&self, flag: bool) {
            self.ivars().prompts.set(flag);
        }

        #[unsafe(method(addsToRecentItems))]
        fn adds_to_recent_items(&self) -> bool {
            self.ivars().adds_to_recents.get()
        }

        #[unsafe(method(setAddsToRecentItems:))]
        fn set_adds_to_recent_items(&self, flag: bool) {
            self.ivars().adds_to_recents.set(flag);
        }

        #[unsafe(method(activates))]
        fn activates(&self) -> bool {
            self.ivars().activates.get()
        }

        #[unsafe(method(setActivates:))]
        fn set_activates(&self, flag: bool) {
            self.ivars().activates.set(flag);
        }

        #[unsafe(method(hides))]
        fn hides(&self) -> bool {
            self.ivars().hides.get()
        }

        #[unsafe(method(setHides:))]
        fn set_hides(&self, flag: bool) {
            self.ivars().hides.set(flag);
        }

        #[unsafe(method(hidesOthers))]
        fn hides_others(&self) -> bool {
            self.ivars().hides_others.get()
        }

        #[unsafe(method(setHidesOthers:))]
        fn set_hides_others(&self, flag: bool) {
            self.ivars().hides_others.set(flag);
        }

        #[unsafe(method(isForPrinting))]
        fn is_for_printing(&self) -> bool {
            self.ivars().for_printing.get()
        }

        #[unsafe(method(setForPrinting:))]
        fn set_for_printing(&self, flag: bool) {
            self.ivars().for_printing.set(flag);
        }

        #[unsafe(method(createsNewApplicationInstance))]
        fn creates_new_application_instance(&self) -> bool {
            self.ivars().new_instance.get()
        }

        #[unsafe(method(setCreatesNewApplicationInstance:))]
        fn set_creates_new_application_instance(&self, flag: bool) {
            self.ivars().new_instance.set(flag);
        }

        #[unsafe(method(allowsRunningApplicationSubstitution))]
        fn allows_running_application_substitution(&self) -> bool {
            self.ivars().substitution.get()
        }

        #[unsafe(method(setAllowsRunningApplicationSubstitution:))]
        fn set_allows_running_application_substitution(&self, flag: bool) {
            self.ivars().substitution.set(flag);
        }

        #[unsafe(method(requiresUniversalLinks))]
        fn requires_universal_links(&self) -> bool {
            self.ivars().universal_links.get()
        }

        #[unsafe(method(setRequiresUniversalLinks:))]
        fn set_requires_universal_links(&self, flag: bool) {
            self.ivars().universal_links.set(flag);
        }

        #[unsafe(method_id(arguments))]
        fn arguments(&self) -> Retained<NSArray<NSString>> {
            self.ivars().arguments.borrow().clone()
        }

        #[unsafe(method(setArguments:))]
        fn set_arguments(&self, arguments: &NSArray<NSString>) {
            drop(self.ivars().arguments.replace(arguments.copy()));
        }

        #[unsafe(method_id(environment))]
        fn environment(&self) -> Retained<NSDictionary<NSString, NSString>> {
            self.ivars().environment.borrow().clone()
        }

        #[unsafe(method(setEnvironment:))]
        fn set_environment(&self, environment: &NSDictionary<NSString, NSString>) {
            drop(self.ivars().environment.replace(environment.copy()));
        }

        #[unsafe(method_id(appleEvent))]
        fn apple_event(&self) -> Option<Retained<AnyObject>> {
            self.ivars().apple_event.borrow().clone()
        }

        #[unsafe(method(setAppleEvent:))]
        fn set_apple_event(&self, event: Option<&AnyObject>) {
            drop(self.ivars().apple_event.replace(event.map(|e| e.retain())));
        }
    }

    unsafe impl NSObjectProtocol for NSWorkspaceOpenConfigurationImpl {}

    unsafe impl NSCopying for NSWorkspaceOpenConfigurationImpl {}
);

fn config_ivars(configuration: &NSWorkspaceOpenConfiguration) -> &ConfigurationIvars {
    // SAFETY: every NSWorkspaceOpenConfiguration is an
    // NSWorkspaceOpenConfigurationImpl.
    unsafe { &*(configuration as *const NSWorkspaceOpenConfiguration).cast::<NSWorkspaceOpenConfigurationImpl>() }
        .ivars()
}

fn bundle_identifier() -> Option<Retained<NSString>> {
    // SAFETY: +mainBundle returns a bundle; -bundleIdentifier a string or
    // nil.
    let from_bundle: Option<Retained<NSString>> = unsafe {
        let bundle: Retained<AnyObject> = msg_send![objc2_foundation::NSBundle::class(), mainBundle];
        msg_send![&*bundle, bundleIdentifier]
    };
    from_bundle.or_else(|| {
        // GLib's launchers name the entry a program was started from, and
        // the process they started: children inherit both, and aren't it.
        let pid = std::env::var("GIO_LAUNCHED_DESKTOP_FILE_PID").ok()?;
        if pid.trim().parse::<u32>().ok()? != std::process::id() {
            return None;
        }
        let entry = std::env::var_os("GIO_LAUNCHED_DESKTOP_FILE")?;
        let name = std::path::Path::new(&entry).file_name()?.to_str()?.strip_suffix(".desktop")?.to_owned();
        (!name.is_empty()).then(|| NSString::from_str(&name))
    })
}

/// Whether the application is active (on the main thread; elsewhere, not
/// knowable without it).
fn active() -> bool {
    MainThreadMarker::new().is_some() && crate::app::is_active()
}

fn current() -> Retained<NSRunningApplication> {
    CURRENT
        .get_or_init(|| {
            LAUNCHED.get_or_init(SystemTime::now);
            // SAFETY: NSObject's initializer.
            let r: Retained<NSRunningApplication> = unsafe { msg_send![NSRunningApplication::alloc(), init] };
            keep(r)
        })
        .get()
}

fn executable() -> Option<std::path::PathBuf> {
    std::env::current_exe().ok()
}

/// The application, when the program made it and this is the main thread.
fn application() -> Option<Retained<NSApplication>> {
    MainThreadMarker::new()?;
    crate::app::existing()
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSRunningApplication"]
    pub(crate) struct NSRunningApplicationImpl;

    impl NSRunningApplicationImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(());
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(currentApplication))]
        fn current_application() -> Retained<NSRunningApplication> {
            current()
        }

        #[unsafe(method_id(runningApplicationWithProcessIdentifier:))]
        fn running_application_with_process_identifier(pid: i32) -> Option<Retained<NSRunningApplication>> {
            (pid == std::process::id() as i32).then(current)
        }

        #[unsafe(method_id(runningApplicationsWithBundleIdentifier:))]
        fn running_applications_with_bundle_identifier(_id: &NSString) -> Retained<NSArray<NSRunningApplication>> {
            NSArray::new()
        }

        #[unsafe(method(terminateAutomaticallyTerminableApplications))]
        fn terminate_automatically_terminable_applications() {}

        #[unsafe(method(isTerminated))]
        fn is_terminated(&self) -> bool {
            false
        }

        #[unsafe(method(isFinishedLaunching))]
        fn is_finished_launching(&self) -> bool {
            application().is_some_and(|a| a.isRunning())
        }

        #[unsafe(method(isHidden))]
        fn is_hidden(&self) -> bool {
            application().is_some_and(|a| a.isHidden())
        }

        #[unsafe(method(isActive))]
        fn is_active(&self) -> bool {
            active()
        }

        #[unsafe(method(ownsMenuBar))]
        fn owns_menu_bar(&self) -> bool {
            active()
        }

        #[unsafe(method(activationPolicy))]
        fn activation_policy(&self) -> NSApplicationActivationPolicy {
            application().map_or(NSApplicationActivationPolicy::Regular, |a| a.activationPolicy())
        }

        /// The executable's name, the program having no bundle.
        #[unsafe(method_id(localizedName))]
        fn localized_name(&self) -> Option<Retained<NSString>> {
            executable().and_then(|exe| exe.file_name().map(|n| NSString::from_str(&n.to_string_lossy())))
        }

        /// The main bundle's identifier (`CFBundleIdentifier` in its
        /// `Info.plist`); else, for a program the desktop launched from a
        /// desktop entry, that entry's id; else nil, as for a bare
        /// executable on macOS.
        #[unsafe(method_id(bundleIdentifier))]
        fn bundle_identifier(&self) -> Option<Retained<NSString>> {
            bundle_identifier()
        }

        #[unsafe(method_id(bundleURL))]
        fn bundle_url(&self) -> Option<Retained<NSURL>> {
            None
        }

        #[unsafe(method_id(executableURL))]
        fn executable_url(&self) -> Option<Retained<NSURL>> {
            executable().map(|exe| NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&exe.to_string_lossy()), false))
        }

        #[unsafe(method(processIdentifier))]
        fn process_identifier(&self) -> i32 {
            std::process::id() as i32
        }

        #[unsafe(method_id(launchDate))]
        fn launch_date(&self) -> Option<Retained<NSDate>> {
            let at = LAUNCHED.get_or_init(SystemTime::now);
            let since = at.duration_since(SystemTime::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
            Some(NSDate::dateWithTimeIntervalSince1970(since))
        }

        #[unsafe(method_id(icon))]
        fn icon(&self) -> Option<Retained<NSImage>> {
            None
        }

        /// `NSBundleExecutableArchitectureX86_64` or `…ARM64`.
        #[unsafe(method(executableArchitecture))]
        fn executable_architecture(&self) -> isize {
            if cfg!(target_arch = "aarch64") { 0x0100_000c } else { 0x0100_0007 }
        }

        #[unsafe(method(hide))]
        fn hide(&self) -> bool {
            application().is_some_and(|a| {
                a.hide(None);
                true
            })
        }

        #[unsafe(method(unhide))]
        fn unhide(&self) -> bool {
            application().is_some_and(|a| {
                a.unhide(None);
                true
            })
        }

        #[unsafe(method(activateWithOptions:))]
        fn activate_with_options(&self, _options: NSApplicationActivationOptions) -> bool {
            application().is_some_and(|a| {
                a.activate();
                true
            })
        }

        #[unsafe(method(activateFromApplication:options:))]
        fn activate_from_application(&self, _from: &AnyObject, _options: NSApplicationActivationOptions) -> bool {
            application().is_some_and(|a| {
                a.activate();
                true
            })
        }

        #[unsafe(method(terminate))]
        fn terminate(&self) -> bool {
            application().is_some_and(|a| {
                a.terminate(None);
                true
            })
        }

        #[unsafe(method(forceTerminate))]
        fn force_terminate(&self) -> bool {
            std::process::exit(0)
        }
    }

    unsafe impl NSObjectProtocol for NSRunningApplicationImpl {}
);

/// `NSBeep`: no sound, as nothing binds a bell.
#[unsafe(no_mangle)]
pub extern "C" fn NSBeep() {}
