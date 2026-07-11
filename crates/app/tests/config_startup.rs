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

    let output = Command::new(env!("CARGO_BIN_EXE_compme"))
        .env_clear()
        .env("COMPME_CONFIG", &config_path)
        .env("COMPME_RUN_MS", "1")
        .output()
        .expect("launch compme");

    assert!(!output.status.success(), "startup unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to read config"),
        "missing fail-closed diagnostic: {stderr}"
    );
    assert!(
        stderr.contains(&config_path.display().to_string()),
        "diagnostic omitted config path: {stderr}"
    );
}
