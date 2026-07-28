//! Live reveal test (ROADMAP Phase 2.6). Linux only, and `#[ignore]`d: it needs a
//! session bus on which the well-known `org.freedesktop.FileManager1` name is
//! claimable.
//!
//! What it proves that a unit test cannot: the `ShowItems` call is **shaped
//! correctly on the wire**. A wrong argument signature or method name reaches a
//! real file manager as `UnknownMethod`/`InvalidArgs`, which this adapter would
//! quietly turn into the `xdg-open` fallback — i.e. reveal would "work" while
//! never selecting the file, on every desktop, and nothing would say so. Standing
//! up a fake file manager on the harness's private bus catches that here.
//!
//! ```sh
//! tools/acceptance/run-linux-atspi-session.sh --run-in-session \
//!   cargo test -p platform_linux -- --ignored --test-threads=1 reveal
//! ```

use std::sync::mpsc::Sender;
use std::sync::Mutex;

use super::*;

/// A stand-in for Nautilus/Dolphin/Thunar: it records the call and returns.
struct FakeFileManager {
    calls: Mutex<Sender<(Vec<String>, String)>>,
}

#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl FakeFileManager {
    /// `ShowItems(URIs: as, StartupId: s)`. The name and both argument types must
    /// match the real interface, or the adapter's call will not dispatch here.
    fn show_items(&self, uris: Vec<String>, startup_id: String) {
        let _ = self.calls.lock().unwrap().send((uris, startup_id));
    }
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn reveal_sends_show_items_with_a_percent_encoded_file_uri() {
    let (tx, rx) = std::sync::mpsc::channel();
    // Owning the well-known name is what makes the adapter's call reach us; on a
    // shared bus this would fail, which is why the harness's private bus is
    // required.
    let _service = zbus::blocking::connection::Builder::session()
        .expect("the harness must provide a session bus")
        .name(FILE_MANAGER1_NAME)
        .expect("FileManager1 name must be claimable on this bus")
        .serve_at(
            FILE_MANAGER1_PATH,
            FakeFileManager {
                calls: Mutex::new(tx),
            },
        )
        .expect("serve FileManager1")
        .build()
        .expect("build the fake file-manager connection");

    // A space and a `#` in the name: both must arrive percent-encoded, or the file
    // manager selects nothing.
    let path = std::path::Path::new("/tmp/compme reveal#probe.gguf");
    reveal(path).expect("ShowItems must succeed when a file manager answers");

    let (uris, startup_id) = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the fake file manager must have received ShowItems");
    assert_eq!(uris, vec!["file:///tmp/compme%20reveal%23probe.gguf"]);
    assert_eq!(startup_id, "", "no launch context to forward");
}
