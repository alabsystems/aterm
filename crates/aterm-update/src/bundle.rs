// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolving the installed `.app` bundle this process is running from, and the
//! gates that make the updater a strict no-op outside a real, writable, installed
//! bundle.

use std::path::{Path, PathBuf};

/// A resolved installed bundle: the `.app` root and the executable inside it.
pub struct Bundle {
    /// `…/aterm.app`.
    pub app_root: PathBuf,
    /// `…/aterm.app/Contents/MacOS/aterm` (the re-exec target).
    pub exe: PathBuf,
}

/// Resolve the installed bundle from the running executable, or `None` when the
/// updater must not act:
///
/// * not the `…/<name>.app/Contents/MacOS/<exe>` layout (dev build, `cargo run`,
///   `target/release` binary) — there is nothing to swap;
/// * an **App-Translocation** path (`…/AppTranslocation/…`) — a read-only,
///   randomized ephemeral copy; swapping it would be wrong and would fail;
/// * a path under `/Volumes/…` — running directly from a mounted DMG (read-only).
///
/// `current_exe()` is canonicalized first so a symlink launcher (e.g. a
/// `/usr/local/bin/aterm` shim) resolves to the real bundle executable.
pub fn resolve() -> Option<Bundle> {
    // Canonicalization here is deliberate symlink-shim resolution (a
    // `/usr/local/bin/aterm` launcher resolves to the real bundle executable),
    // not an identity comparison.
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    resolve_from(&exe)
}

/// Pure core of [`resolve`], split out so it is unit-testable with a synthetic
/// path (no real filesystem / `current_exe`).
pub fn resolve_from(exe: &Path) -> Option<Bundle> {
    // Bail on translocated / mounted-image launches: never swap those. The lossy
    // string is only substring-MATCHED against ASCII markers, never used as a
    // path, so lossy is fine by design.
    let s = exe.to_string_lossy();
    if s.contains("/AppTranslocation/") || s.starts_with("/Volumes/") {
        return None;
    }
    // Require exactly  …/<X>.app/Contents/MacOS/<exe>.
    let macos = exe.parent()?; // …/Contents/MacOS
    if macos.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?; // …/Contents
    if contents.file_name()?.to_str()? != "Contents" {
        return None;
    }
    let app_root = contents.parent()?; // …/<X>.app
    if app_root.extension()?.to_str()? != "app" {
        return None;
    }
    Some(Bundle {
        app_root: app_root.to_path_buf(),
        exe: exe.to_path_buf(),
    })
}

/// Whether we can replace the bundle in place: the swap operates in the bundle's
/// **parent** directory, so that parent must be writable. Probe it directly by
/// creating + removing a temp entry (more accurate than an `access()` mode guess,
/// which misreads ACL- and MDM-managed `/Applications`).
pub fn parent_writable(app_root: &Path) -> bool {
    let Some(parent) = app_root.parent() else {
        return false;
    };
    let probe = parent.join(format!(".aterm-update-probe-{}", std::process::id()));
    match std::fs::create_dir(&probe) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_canonical_app_layout() {
        let b = resolve_from(Path::new("/Applications/aterm.app/Contents/MacOS/aterm")).unwrap();
        assert_eq!(b.app_root, Path::new("/Applications/aterm.app"));
        assert_eq!(
            b.exe,
            Path::new("/Applications/aterm.app/Contents/MacOS/aterm")
        );
    }

    #[test]
    fn rejects_dev_and_target_paths() {
        assert!(resolve_from(Path::new("/Users//x/aterm/target/release/aterm-gui")).is_none());
        assert!(resolve_from(Path::new("/tmp/aterm")).is_none());
    }

    #[test]
    fn rejects_translocated_and_mounted() {
        assert!(
            resolve_from(Path::new(
                "/private/var/folders/zz/AppTranslocation/ABC/d/aterm.app/Contents/MacOS/aterm"
            ))
            .is_none()
        );
        assert!(
            resolve_from(Path::new(
                "/Volumes/aterm 0.2.0/aterm.app/Contents/MacOS/aterm"
            ))
            .is_none()
        );
    }
}
