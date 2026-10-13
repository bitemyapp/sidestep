//! Depend on this crate and objc2 programs build for Linux as well as macOS.
//!
//! On Apple targets it is empty. Elsewhere it links in Sidestep's Objective-C
//! runtime and framework implementations.

#[cfg(all(not(target_vendor = "apple"), feature = "appkit"))]
pub use sidestep_appkit as appkit;
#[cfg(not(target_vendor = "apple"))]
pub use sidestep_core_services as core_services;
#[cfg(not(target_vendor = "apple"))]
pub use sidestep_foundation as foundation;
#[cfg(not(target_vendor = "apple"))]
pub use sidestep_runtime as runtime;
#[cfg(not(target_vendor = "apple"))]
pub use sidestep_uniform_type_identifiers as uniform_type_identifiers;
