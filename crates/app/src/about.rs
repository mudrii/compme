//! About-pane content (A3 settings, c91 design / c98 verification).
//!
//! Pure: everything is compile-time or static — version from the crate,
//! license/repo/acks as constants, and the project's no-telemetry guarantee
//! quoted from the README. The window renders the string verbatim.

/// The About pane's full text. One source of truth for version, license,
/// the no-telemetry statement, the repo link, and dependency credits.
pub fn about_text() -> String {
    const REPO: &str = "https://github.com/mudrii/compme";
    // README.md's guarantee, quoted verbatim (the deliberate replacement for
    // a telemetry toggle — there is nothing to toggle).
    const TELEMETRY: &str = "All inference is local (llama.cpp), with no proprietary telemetry.";
    // Credits list only what this build links: the Apple stack is macOS-only.
    #[cfg(target_os = "macos")]
    const ACKS: &str = "llama-cpp-2, encoding_rs, rusqlite, aes-gcm, getrandom, \
                        accessibility-sys, core-foundation, core-graphics, objc2, \
                        objc2-app-kit, objc2-foundation, objc2-service-management, \
                        security-framework, regex, libc";
    #[cfg(all(unix, not(target_os = "macos")))]
    const ACKS: &str = "llama-cpp-2, encoding_rs, rusqlite, aes-gcm, getrandom, \
                        regex, libc";
    #[cfg(windows)]
    const ACKS: &str = "llama-cpp-2, encoding_rs, rusqlite, aes-gcm, getrandom, \
                        regex, windows";
    format!(
        "Compme v{}\nApache-2.0 \u{2014} Copyright 2026 Compme contributors\n{}\n{}\n\nBuilt with: {}",
        env!("CARGO_PKG_VERSION"),
        TELEMETRY,
        REPO,
        ACKS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_text_states_version_license_telemetry_repo_and_acks() {
        let text = about_text();
        // Version comes from the crate, not a hand-maintained string.
        assert!(text.contains(&format!("Compme v{}", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("Apache-2.0"));
        // The README's local-only guarantee, stated verbatim in the pane.
        assert!(text.contains("All inference is local (llama.cpp), with no proprietary telemetry."));
        assert!(text.contains("https://github.com/mudrii/compme"));
        // Credits: the inference and crypto cornerstones must be named.
        assert!(text.contains("llama-cpp-2"));
        assert!(text.contains("aes-gcm"));
        // The Apple stack is credited only in macOS builds.
        #[cfg(target_os = "macos")]
        assert!(text.contains("objc2"));
    }
}
