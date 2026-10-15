//! AppleScript has no interpreter on Linux. Keep the source and report the
//! unsupported scripting component through the API's error dictionary.
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSDictionary, NSNumber, NSString};
sidestep_runtime::static_class!(pub(crate) SCRIPT, SCRIPT_META = "NSAppleScript", || { let _ = Script::class(); });
crate::constant_string!(NSAppleScriptErrorMessage = "NSAppleScriptErrorMessage");
crate::constant_string!(NSAppleScriptErrorNumber = "NSAppleScriptErrorNumber");
crate::constant_string!(NSAppleScriptErrorBriefMessage = "NSAppleScriptErrorBriefMessage");
crate::constant_string!(NSAppleScriptErrorRange = "NSAppleScriptErrorRange");
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSAppleScript"]
    #[ivars = Retained<NSString>]
    struct Script;
    impl Script {
        #[unsafe(method_id(initWithSource:))]
        fn init(this: Allocated<Self>, source: &NSString) -> Retained<Self> { let this = this.set_ivars(source.retain()); unsafe { msg_send![super(this), init] } }
        #[unsafe(method_id(source))]
        fn source(&self) -> Retained<NSString> { self.ivars().clone() }
        #[unsafe(method(isCompiled))]
        fn compiled(&self) -> bool { false }
        #[unsafe(method(compileAndReturnError:))]
        fn compile(&self, error: *mut *mut NSDictionary) -> bool { unavailable(error); false }
        #[unsafe(method_id(executeAndReturnError:))]
        fn execute(&self, error: *mut *mut NSDictionary) -> Option<Retained<AnyObject>> { unavailable(error); None }
    }
    unsafe impl NSObjectProtocol for Script {}
);
fn unavailable(out: *mut *mut NSDictionary) {
    if out.is_null() {
        return;
    }
    let keys = [NSString::from_str("NSAppleScriptErrorNumber"), NSString::from_str("NSAppleScriptErrorMessage")];
    let values: [Retained<AnyObject>; 2] =
        [NSNumber::numberWithInt(-4).into(), NSString::from_str("AppleScript is unavailable on this platform.").into()];
    let dict = NSDictionary::from_retained_objects(&keys.iter().map(|s| &**s).collect::<Vec<_>>(), &values);
    unsafe { out.write(Retained::autorelease_ptr(Retained::cast_unchecked(dict))) };
}
