use objc2::MainThreadMarker;
use objc2_foundation::NSString;

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

fn main() {
    let greeting = NSString::from_str("hello from objc2");
    println!("{greeting}, main thread: {}", MainThreadMarker::new().is_some());
}
