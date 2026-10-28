//! CoreServices' public C ABI on Linux, backed by filesystem notifications
//! and Sidestep's Foundation and dispatch implementations.
#![cfg(not(target_vendor = "apple"))]

mod events;
mod metadata;
