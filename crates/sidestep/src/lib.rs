//! Depend on this crate and objc2 programs build for Linux as well as macOS.
//!
//! On Apple targets it is empty. Elsewhere it links in Sidestep's Objective-C
//! runtime and framework implementations.

#[cfg(not(target_vendor = "apple"))]
pub use sidestep_foundation as foundation;
#[cfg(not(target_vendor = "apple"))]
pub use sidestep_runtime as runtime;
