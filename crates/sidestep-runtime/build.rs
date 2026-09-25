//! objc2's GNUstep mode links `-lobjc`, `-lgnustep-base` and `-lgnustep-gui`.
//! Sidestep defines those symbols in Rust, so the libraries only have to
//! exist: empty archives in our output directory satisfy the linker, and
//! Cargo passes the search path on to every crate that depends on us.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    for name in ["objc", "gnustep-base", "gnustep-gui"] {
        fs::write(out.join(format!("lib{name}.a")), b"!<arch>\n").expect("write stub archive");
    }
    println!("cargo::rustc-link-search=native={}", out.display());
}
