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

// macOS test builds also compile the stub as a named module so run-loop unit
// tests can inject stub shell pieces (the run() startup seam); the glob
// re-export stays non-macOS-only so stub names never collide with the real
// bindings above.
#[cfg(any(test, not(target_os = "macos")))]
pub mod stub;
#[cfg(not(target_os = "macos"))]
pub use stub::*;
