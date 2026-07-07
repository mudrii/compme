//! Compile-time platform binding.
//!
//! This is the only app module allowed to name a `platform_*` crate. Shared
//! shell behavior goes through `platform::shell::ShellHost`; this module owns
//! construction and the macOS-only facades that do not have a portable
//! contract yet.

#![allow(unused_imports)]

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub use stub::*;
