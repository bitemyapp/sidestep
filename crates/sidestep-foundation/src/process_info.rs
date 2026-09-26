//! `NSProcessInfo`: this process and the machine it runs on.
//!
//! The arguments, environment, host name and operating-system version are
//! read once, when the shared instance is made. The version comes from
//! `uname`'s kernel release (`6.8.0-45-generic` is 6.8.0), since that is
//! the version Linux has; activity and termination calls are accepted and
//! have no effect.

use std::ffi::CStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSDictionary, NSInteger, NSOperatingSystemVersion, NSString, NSUInteger};

sidestep_runtime::static_class!(pub(crate) NSPROCESSINFO, NSPROCESSINFO_META = "NSProcessInfo", || {
    let _ = NSProcessInfoImpl::class();
    crate::perform::install();
});

type OsVersion = NSOperatingSystemVersion;

pub(crate) struct InfoIvars {
    name: Mutex<String>,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
    host: String,
    version: OsVersion,
    version_string: String,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSProcessInfo"]
    #[ivars = InfoIvars]
    pub(crate) struct NSProcessInfoImpl;

    impl NSProcessInfoImpl {
        #[unsafe(method_id(processInfo))]
        fn process_info() -> Retained<Self> {
            shared()
        }

        #[unsafe(method_id(environment))]
        fn environment(&self) -> Retained<AnyObject> {
            let entries = &self.ivars().environment;
            let keys: Vec<Retained<NSString>> = entries.iter().map(|(k, _)| NSString::from_str(k)).collect();
            let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let values: Vec<Retained<NSString>> = entries.iter().map(|(_, v)| NSString::from_str(v)).collect();
            NSDictionary::from_retained_objects(&keys, &values).into()
        }

        #[unsafe(method_id(arguments))]
        fn arguments(&self) -> Retained<AnyObject> {
            let arguments: Vec<Retained<NSString>> =
                self.ivars().arguments.iter().map(|a| NSString::from_str(a)).collect();
            objc2_foundation::NSArray::from_retained_slice(&arguments).into()
        }

        #[unsafe(method_id(hostName))]
        fn host_name(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().host)
        }

        #[unsafe(method_id(processName))]
        fn process_name(&self) -> Retained<NSString> {
            NSString::from_str(&crate::thread::lock(&self.ivars().name))
        }

        #[unsafe(method(setProcessName:))]
        fn set_process_name(&self, name: &NSString) {
            *crate::thread::lock(&self.ivars().name) = name.to_string();
        }

        #[unsafe(method(processIdentifier))]
        fn process_identifier(&self) -> libc::c_int {
            std::process::id() as libc::c_int
        }

        #[unsafe(method_id(globallyUniqueString))]
        fn globally_unique_string(&self) -> Retained<NSString> {
            NSString::from_str(&globally_unique())
        }

        #[unsafe(method_id(operatingSystemVersionString))]
        fn operating_system_version_string(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().version_string)
        }

        #[unsafe(method(operatingSystemVersion))]
        fn operating_system_version(&self) -> OsVersion {
            self.ivars().version
        }

        #[unsafe(method(isOperatingSystemAtLeastVersion:))]
        fn is_at_least(&self, version: OsVersion) -> bool {
            let v = self.ivars().version;
            (v.majorVersion, v.minorVersion, v.patchVersion)
                >= (version.majorVersion, version.minorVersion, version.patchVersion)
        }

        #[unsafe(method(processorCount))]
        fn processor_count(&self) -> NSUInteger {
            // SAFETY: sysconf has no preconditions.
            let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) };
            if n > 0 { n as NSUInteger } else { 1 }
        }

        #[unsafe(method(activeProcessorCount))]
        fn active_processor_count(&self) -> NSUInteger {
            std::thread::available_parallelism().map_or(1, |n| n.get())
        }

        #[unsafe(method(physicalMemory))]
        fn physical_memory(&self) -> u64 {
            // SAFETY: sysconf has no preconditions.
            let (pages, size) = unsafe { (libc::sysconf(libc::_SC_PHYS_PAGES), libc::sysconf(libc::_SC_PAGESIZE)) };
            (pages.max(0) as u64) * (size.max(0) as u64)
        }

        #[unsafe(method(systemUptime))]
        fn system_uptime(&self) -> f64 {
            // Monotonic time leaves out suspension, as macOS's uptime does.
            let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            // SAFETY: a valid timespec to fill.
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
            ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
        }

        #[unsafe(method(disableSuddenTermination))]
        fn disable_sudden_termination(&self) {}

        #[unsafe(method(enableSuddenTermination))]
        fn enable_sudden_termination(&self) {}

        #[unsafe(method(disableAutomaticTermination:))]
        fn disable_automatic_termination(&self, _reason: &NSString) {}

        #[unsafe(method(enableAutomaticTermination:))]
        fn enable_automatic_termination(&self, _reason: &NSString) {}

        #[unsafe(method(automaticTerminationSupportEnabled))]
        fn automatic_termination_support_enabled(&self) -> bool {
            false
        }

        #[unsafe(method(setAutomaticTerminationSupportEnabled:))]
        fn set_automatic_termination_support_enabled(&self, _enabled: bool) {}

        #[unsafe(method_id(beginActivityWithOptions:reason:))]
        fn begin_activity(&self, _options: u64, _reason: &NSString) -> Retained<NSObject> {
            // A token for endActivity:, holding nothing.
            NSObject::new()
        }

        #[unsafe(method(endActivity:))]
        fn end_activity(&self, _activity: &AnyObject) {}

        #[unsafe(method(performActivityWithOptions:reason:usingBlock:))]
        fn perform_activity(&self, _options: u64, _reason: &NSString, block: &DynBlock<dyn Fn()>) {
            block.call(());
        }

        #[unsafe(method(performExpiringActivityWithReason:usingBlock:))]
        fn perform_expiring_activity(&self, _reason: &NSString, block: &DynBlock<dyn Fn(Bool)>) {
            // Runs on a background queue and is never asked to expire.
            let block = SendBlock(block.copy());
            crate::dispatch::global_async(crate::runloop::core::Work::boxed(move || {
                let block = block;
                block.0.call((Bool::NO,));
            }));
        }

        #[unsafe(method_id(userName))]
        fn user_name(&self) -> Retained<NSString> {
            NSString::from_str(&crate::path::user_name())
        }

        #[unsafe(method_id(fullUserName))]
        fn full_user_name(&self) -> Retained<NSString> {
            NSString::from_str(&crate::path::full_user_name())
        }

        #[unsafe(method(thermalState))]
        fn thermal_state(&self) -> NSInteger {
            // NSProcessInfoThermalStateNominal.
            0
        }

        #[unsafe(method(isLowPowerModeEnabled))]
        fn is_low_power_mode_enabled(&self) -> bool {
            false
        }

        #[unsafe(method(isMacCatalystApp))]
        fn is_mac_catalyst_app(&self) -> bool {
            false
        }

        #[unsafe(method(isiOSAppOnMac))]
        fn is_ios_app_on_mac(&self) -> bool {
            false
        }
    }

    unsafe impl NSObjectProtocol for NSProcessInfoImpl {}
);

/// A block moved to another thread. Blocks passed to Foundation are
/// copied to the heap and may be called from any thread.
struct SendBlock(block2::RcBlock<dyn Fn(Bool)>);

// SAFETY: see above.
unsafe impl Send for SendBlock {}

fn shared() -> Retained<NSProcessInfoImpl> {
    static SHARED: OnceLock<usize> = OnceLock::new();
    let ptr = *SHARED.get_or_init(|| {
        let (version, version_string) = os_version();
        let ivars = InfoIvars {
            name: Mutex::new(crate::path::process_name()),
            arguments: std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect(),
            environment: std::env::vars_os()
                .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
                .collect(),
            host: host_name(),
            version,
            version_string,
        };
        let this = NSProcessInfoImpl::alloc().set_ivars(ivars);
        // SAFETY: NSObject's designated initializer. The shared instance
        // lives for the rest of the process.
        let this: Retained<NSProcessInfoImpl> = unsafe { msg_send![super(this), init] };
        Retained::into_raw(this) as usize
    });
    // SAFETY: the leaked shared instance.
    unsafe { Retained::retain(ptr as *mut NSProcessInfoImpl) }.expect("the shared process info")
}

fn host_name() -> String {
    let mut buffer = [0 as libc::c_char; 256];
    // SAFETY: the buffer is writable; gethostname NUL-terminates what fits.
    if unsafe { libc::gethostname(buffer.as_mut_ptr(), buffer.len() - 1) } != 0 {
        return "localhost".into();
    }
    // SAFETY: NUL-terminated above.
    unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy().into_owned()
}

/// The kernel's version as three numbers, and the text around it.
fn os_version() -> (OsVersion, String) {
    // SAFETY: zeroed is a valid utsname; uname fills it.
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::uname(&mut uts) } != 0 {
        return (version(0, 0, 0), "Version 0.0.0".into());
    }
    // SAFETY: uname NUL-terminates its fields.
    let field = |f: &[libc::c_char]| unsafe { CStr::from_ptr(f.as_ptr()) }.to_string_lossy().into_owned();
    let release = field(&uts.release);
    let version = parse_release(&release);
    let text = format!(
        "Version {}.{}.{} ({} {release})",
        version.majorVersion,
        version.minorVersion,
        version.patchVersion,
        field(&uts.sysname)
    );
    (version, text)
}

fn parse_release(release: &str) -> OsVersion {
    let mut numbers = release
        .split(|c: char| !c.is_ascii_digit())
        .take_while(|s| !s.is_empty())
        .map(|s| s.parse::<NSInteger>().unwrap_or(0));
    version(numbers.next().unwrap_or(0), numbers.next().unwrap_or(0), numbers.next().unwrap_or(0))
}

fn version(major: NSInteger, minor: NSInteger, patch: NSInteger) -> OsVersion {
    OsVersion { majorVersion: major, minorVersion: minor, patchVersion: patch }
}

/// `UUID-PID-COUNTER`, unique across hosts and processes.
fn globally_unique() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let uuid = crate::uuid::format(&crate::uuid::random());
    format!("{uuid}-{}-{n:016X}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases() {
        assert_eq!(parse_release("6.8.0-45-generic"), version(6, 8, 0));
        assert_eq!(parse_release("5.15.167.4-microsoft-standard-WSL2"), version(5, 15, 167));
        assert_eq!(parse_release("6.10"), version(6, 10, 0));
        let a = globally_unique();
        assert_ne!(a, globally_unique());
        assert_eq!(a.len(), 36 + 1 + std::process::id().to_string().len() + 1 + 16);
    }
}
