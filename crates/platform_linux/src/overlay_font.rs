//! Font discovery for the X11 overlay (ROADMAP Phase 2.5).
//!
//! **No fontconfig.** Linking the C library would make the compme binary refuse
//! to *start* on a host without it, the same hard failure the AT-SPI layer
//! rejected libatspi for. A directory scan finds the same fonts on every distro
//! layout this project targets, and a host with no font at all is reported as a
//! fail-closed overlay error instead of a panic.
//!
//! Kept out of the X11 module and free of any X11 type so the ranking, the
//! search-path rules, and the scan are unit-tested on the macOS and Windows
//! lanes that also build this crate.

use std::path::{Path, PathBuf};

/// How deep to walk each search root. Real layouts nest at most
/// `<root>/truetype/dejavu/DejaVuSans.ttf`; the bound is also the only thing
/// stopping a symlink cycle, because the scan deliberately *does* follow links
/// (see `collect_best`).
const MAX_DEPTH: usize = 4;

/// Fonts named here are preferred, best first: each is a widely installed
/// regular-weight sans-serif with broad coverage, and picking deterministically
/// keeps the overlay's look stable across hosts rather than "whatever the
/// directory order was".
const PREFERRED: [&str; 6] = [
    "dejavusans.ttf",
    "liberationsans-regular.ttf",
    "notosans-regular.ttf",
    "freesans.ttf",
    "arial.ttf",
    "cantarell-regular.otf",
];

/// Rank a font file by name: `Some(0)` is the best match, larger is worse,
/// `None` means "not usable".
///
/// Bold/italic/oblique/condensed faces are refused rather than ranked last: a
/// ghost rendered in Bold Italic reads as emphasis the user did not type. So are
/// mono/serif/symbol/emoji faces, which either mis-measure the ghost against the
/// field's proportional text or carry no Latin glyphs at all.
pub fn rank_candidate(file_name: &str) -> Option<u32> {
    let lower = file_name.to_ascii_lowercase();
    if !(lower.ends_with(".ttf") || lower.ends_with(".otf")) {
        return None;
    }
    let stem = lower.trim_end_matches(".ttf").trim_end_matches(".otf");
    // Substring matching on the stem, because the same face is spelled
    // `DejaVuSans-Bold`, `LiberationSans-Italic`, and `NotoSansMono` depending
    // on the family.
    for reject in [
        "bold",
        "italic",
        "oblique",
        "condensed",
        "mono",
        "serif",
        "symbol",
        "emoji",
        "math",
        "light",
        "thin",
        "black",
    ] {
        if stem.contains(reject) {
            return None;
        }
    }
    if let Some(index) = PREFERRED.iter().position(|name| *name == lower) {
        return Some(index as u32);
    }
    // Anything else that survived the rejections is a usable last resort — a
    // host with only one unfamiliar sans font still gets a readable ghost.
    Some(PREFERRED.len() as u32 + 1)
}

/// The directories to scan, best first, from environment values rather than
/// `std::env` so the rules are testable.
///
/// Order is deliberate: a user-installed font wins over a system one (that is
/// what the user chose), and `XDG_DATA_DIRS` comes before the hardcoded FHS
/// paths because it is how Nix, Flatpak, and Guix expose fonts at all. Relative
/// entries are dropped — the basedir spec requires absolute paths, and a
/// relative one would scan the process cwd. The absolute test is a leading `/`
/// on the string, never `Path::is_absolute`, which answers for the host that
/// *compiled* the code (this crate is built on the Windows lane too).
pub fn font_search_dirs(
    xdg_data_home: Option<&str>,
    xdg_data_dirs: Option<&str>,
    home: Option<&str>,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };

    match xdg_data_home.filter(|v| v.starts_with('/')) {
        Some(data_home) => push(Path::new(data_home).join("fonts")),
        None => {
            if let Some(home) = home.filter(|v| v.starts_with('/')) {
                push(Path::new(home).join(".local/share/fonts"));
            }
        }
    }
    if let Some(home) = home.filter(|v| v.starts_with('/')) {
        push(Path::new(home).join(".fonts"));
    }
    for entry in xdg_data_dirs.unwrap_or_default().split(':') {
        if entry.starts_with('/') {
            push(Path::new(entry).join("fonts"));
        }
    }
    for fixed in [
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        // NixOS exposes the configured system fonts here and nowhere else.
        "/run/current-system/sw/share/X11/fonts",
    ] {
        push(PathBuf::from(fixed));
    }
    dirs
}

/// The best-ranked font file under any of `dirs`, or `None` when none holds one.
///
/// Ties are broken by path so the choice is stable across runs on one host
/// (directory iteration order is not).
pub fn find_font_file(dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut best: Option<(u32, PathBuf)> = None;
    for dir in dirs {
        collect_best(dir, 0, &mut best);
        // An earlier directory outranks a later one only when it found an
        // equally good face; keep scanning so a user font that is merely
        // "usable" does not beat the system DejaVu.
    }
    best.map(|(_, path)| path)
}

fn collect_best(dir: &Path, depth: usize, best: &mut Option<(u32, PathBuf)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // `metadata` FOLLOWS symlinks, unlike `DirEntry::file_type`. That is
        // load-bearing, not incidental: Nix profiles and `share/fonts` trees are
        // symlink farms — the first version of this scan used `file_type` and
        // found nothing at all in `<dejavu-store-path>/share/fonts`, where every
        // entry is a link. A link cycle is bounded by MAX_DEPTH above, and a
        // broken link is an `Err` that is simply skipped.
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => collect_best(&path, depth + 1, best),
            Ok(meta) if meta.is_file() => {
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(rank) = rank_candidate(name) else {
                    continue;
                };
                let better = match best {
                    None => true,
                    Some((best_rank, best_path)) => {
                        rank < *best_rank || (rank == *best_rank && path < *best_path)
                    }
                };
                if better {
                    *best = Some((rank, path));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_faces_outrank_unknown_ones_and_styled_faces_are_refused() {
        assert_eq!(rank_candidate("DejaVuSans.ttf"), Some(0));
        assert!(rank_candidate("LiberationSans-Regular.ttf") > Some(0));
        assert!(rank_candidate("DejaVuSans.ttf") < rank_candidate("Whatever.ttf"));
        // Case-insensitive: the same face ships capitalized differently per
        // distro.
        assert_eq!(rank_candidate("dejavusans.TTF"), Some(0));
        // A ghost in Bold Italic reads as emphasis the user did not type, and a
        // mono/serif face mis-measures against the field's proportional text.
        for refused in [
            "DejaVuSans-Bold.ttf",
            "DejaVuSans-Oblique.ttf",
            "DejaVuSansCondensed.ttf",
            "DejaVuSansMono.ttf",
            "DejaVuSerif.ttf",
            "NotoColorEmoji.ttf",
            "DejaVuMathTeXGyre.ttf",
            "DejaVuSans-ExtraLight.ttf",
            "Roboto-Thin.ttf",
            "Inter-Black.ttf",
        ] {
            assert_eq!(rank_candidate(refused), None, "{refused}");
        }
        // Not a font file at all.
        for refused in ["fonts.dir", "DejaVuSans.pcf.gz", "README", "sans.ttf.bak"] {
            assert_eq!(rank_candidate(refused), None, "{refused}");
        }
    }

    #[test]
    fn search_dirs_prefer_user_then_xdg_then_fhs_and_drop_relative_entries() {
        let dirs = font_search_dirs(
            Some("/home/u/.share"),
            Some("/nix/store/a/share:relative/share:/usr/share"),
            Some("/home/u"),
        );
        let shown: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        assert_eq!(
            shown,
            vec![
                "/home/u/.share/fonts",
                "/home/u/.fonts",
                "/nix/store/a/share/fonts",
                "/usr/share/fonts",
                "/usr/local/share/fonts",
                "/run/current-system/sw/share/X11/fonts",
            ],
            "a relative XDG_DATA_DIRS entry must be dropped, and /usr/share \
             must not appear twice"
        );

        // No XDG_DATA_HOME falls back to the spec's default under $HOME. The
        // absolute/relative verdict is POSIX and must not follow the build host:
        // this same assertion runs on the Windows lane, where
        // `Path::is_absolute("/home/u")` is false.
        let dirs = font_search_dirs(None, None, Some("/home/u"));
        assert!(dirs.contains(&PathBuf::from("/home/u/.local/share/fonts")));
        assert!(dirs.contains(&PathBuf::from("/usr/share/fonts")));
        let dirs = font_search_dirs(Some(r"C:\fonts"), None, Some(r"C:\users\u"));
        assert!(
            !dirs.iter().any(|d| d.starts_with("C:")),
            "a Windows-shaped path is not a POSIX absolute path: {dirs:?}"
        );
        // A host with no HOME still searches the system paths rather than
        // returning nothing.
        assert!(!font_search_dirs(None, None, None).is_empty());
    }

    #[test]
    fn the_scan_finds_the_best_face_in_a_nested_layout() {
        let root = std::env::temp_dir().join(format!(
            "compme-linux-font-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let nested = root.join("truetype/dejavu");
        std::fs::create_dir_all(&nested).unwrap();
        // The real Debian layout: the regular face sits two directories down,
        // next to faces that must be refused.
        for name in [
            "DejaVuSans-Bold.ttf",
            "DejaVuSansMono.ttf",
            "DejaVuSans.ttf",
            "fonts.dir",
        ] {
            std::fs::write(nested.join(name), []).unwrap();
        }
        std::fs::write(root.join("Unknown.ttf"), []).unwrap();

        assert_eq!(
            find_font_file(std::slice::from_ref(&root)),
            Some(nested.join("DejaVuSans.ttf")),
            "the preferred face must win over an unranked one nearer the root"
        );
        // Nothing usable is `None`, which is what makes the overlay report a
        // diagnosable error instead of panicking on a fontless host.
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(find_font_file(&[empty]), None);
        assert_eq!(find_font_file(&[root.join("missing")]), None);
        assert_eq!(find_font_file(&[]), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_scan_is_depth_bounded_and_stable() {
        let root = std::env::temp_dir().join(format!(
            "compme-linux-font-depth-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let deep = root.join("a/b/c/d/e/f");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("DejaVuSans.ttf"), []).unwrap();
        assert_eq!(
            find_font_file(std::slice::from_ref(&root)),
            None,
            "a font past MAX_DEPTH must not be found: the bound is what keeps a \
             pathological tree from hanging the overlay"
        );

        // Two equally ranked faces resolve to the same one every run; directory
        // iteration order does not.
        let flat = root.join("flat");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join("Zeta.ttf"), []).unwrap();
        std::fs::write(flat.join("Alpha.ttf"), []).unwrap();
        assert_eq!(
            find_font_file(std::slice::from_ref(&flat)),
            Some(flat.join("Alpha.ttf"))
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// `#[cfg(unix)]`, not `#[cfg(target_os = "linux")]`: the macOS lane is unix
    /// too, so this still runs there — only the Windows lane, which has no
    /// `std::os::unix`, skips it.
    #[cfg(unix)]
    #[test]
    fn the_scan_follows_symlinks_because_font_trees_are_symlink_farms() {
        // The exact shape that made the first version of this scan find nothing
        // on the NixOS host: `<store-path>/share/fonts/truetype` and every file
        // under it are links, and `DirEntry::file_type` reports a symlink as
        // neither a directory nor a file, so both branches were skipped.
        let root = std::env::temp_dir().join(format!(
            "compme-linux-font-links-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let real = root.join("real/truetype");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("DejaVuSans.ttf"), []).unwrap();

        let farm = root.join("farm");
        std::fs::create_dir_all(&farm).unwrap();
        // A linked directory...
        std::os::unix::fs::symlink(root.join("real"), farm.join("linked")).unwrap();
        assert_eq!(
            find_font_file(std::slice::from_ref(&farm)),
            Some(farm.join("linked/truetype/DejaVuSans.ttf"))
        );
        // ...and a linked file.
        let files = root.join("files");
        std::fs::create_dir_all(&files).unwrap();
        std::os::unix::fs::symlink(real.join("DejaVuSans.ttf"), files.join("DejaVuSans.ttf"))
            .unwrap();
        assert_eq!(
            find_font_file(std::slice::from_ref(&files)),
            Some(files.join("DejaVuSans.ttf"))
        );
        // A broken link is skipped rather than reported as a usable font, which
        // would only fail later inside the rasterizer.
        let broken = root.join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::os::unix::fs::symlink(root.join("gone"), broken.join("DejaVuSans.ttf")).unwrap();
        assert_eq!(find_font_file(std::slice::from_ref(&broken)), None);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
