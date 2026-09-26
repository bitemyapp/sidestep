//! dispatch2 links `-ldispatch` on Linux. Sidestep defines libdispatch's
//! symbols in Rust (`src/dispatch`), so the library only has to exist: an
//! empty archive in our output directory satisfies the linker, and Cargo
//! passes the search path on to every crate that depends on us, as
//! `sidestep-runtime`'s build script does for libobjc.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    fs::write(out.join("libdispatch.a"), b"!<arch>\n").expect("write stub archive");
    println!("cargo::rustc-link-search=native={}", out.display());
}
