use std::process::Command;

struct TempTree(std::path::PathBuf);

impl TempTree {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "compme-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir(&path).expect("create isolated temp root");
        Self(path)
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn startup_fails_closed_and_names_an_unreadable_config_path() {
    let temp = TempTree::new("config-startup");
    let config_path = temp.0.join("config.env");
    std::fs::create_dir(&config_path).expect("directory is deterministically unreadable as text");

    let mut command = Command::new(env!("CARGO_BIN_EXE_compme"));
    command
        .env_clear()
        .env("COMPME_CONFIG", &config_path)
        .env("COMPME_RUN_MS", "1");
    // `env_clear` is what makes this test meaningful — it proves the config path
    // comes from COMPME_CONFIG and nothing else. But it also strips the dynamic
    // loader's search path, and on a non-FHS host (NixOS, or any Nix/Guix build
    // env) the child then dies at load time with "libstdc++.so.6: cannot open
    // shared object file" before `main` runs. Forwarding only the loader
    // variables cannot influence which config file is read, so the proposition
    // under test is unchanged.
    for loader_var in [
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
    ] {
        if let Ok(value) = std::env::var(loader_var) {
            command.env(loader_var, value);
        }
    }
    let output = command.output().expect("launch compme");

    assert!(!output.status.success(), "startup unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Distinguish "the binary refused the config" from "the binary never ran":
    // without this, a loader failure also satisfies the exit-code assertion
    // above and the real cause is buried in a "missing diagnostic" message.
    assert!(
        !stderr.contains("error while loading shared libraries")
            && !stderr.contains("image not found"),
        "compme could not start, so this test proved nothing about config handling: {stderr}"
    );
    assert!(
        stderr.contains("failed to read config"),
        "missing fail-closed diagnostic: {stderr}"
    );
    assert!(
        stderr.contains(&config_path.display().to_string()),
        "diagnostic omitted config path: {stderr}"
    );
}

// A successful macOS launch initializes AppKit, Accessibility, and Carbon;
// that is a native acceptance test rather than a portable config regression.
#[cfg(not(target_os = "macos"))]
#[test]
fn startup_accepts_a_basename_config_path() {
    let temp = TempTree::new("basename-config-startup");
    std::fs::write(temp.0.join("config.env"), "COMPME_ENABLED=false\n")
        .expect("seed basename config");

    let mut command = Command::new(env!("CARGO_BIN_EXE_compme"));
    command
        .current_dir(&temp.0)
        .env_clear()
        .env("COMPME_CONFIG", "config.env")
        .env("COMPME_RUN_MS", "1");
    for loader_var in [
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
    ] {
        if let Ok(value) = std::env::var(loader_var) {
            command.env(loader_var, value);
        }
    }
    let output = command.output().expect("launch compme");

    assert!(
        output.status.success(),
        "basename COMPME_CONFIG rejected at startup: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
