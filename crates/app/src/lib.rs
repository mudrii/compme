//! `app` is the host binary crate: every module is declared in `main.rs`
//! and compiled into the `compme` binary target. This implicit lib target
//! is intentionally empty — integration tests (`tests/`) drive the built
//! binary, and nothing in the workspace depends on `app` as a library.
