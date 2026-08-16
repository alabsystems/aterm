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

/// The `Info.plist` key marking a bundle as a LOCAL DEV BUILD the updater must not
/// replace. `<string>true</string>`; any other value, or absence, is a normal
/// install. Stamped by `tools/dev-app.sh` (its `--no-pin` omits it).
///
/// Layout alone cannot tell a dev build from a release: [`resolve_from`] checks only
/// the `.app` path shape, so a locally built binary installed to `/Applications` is
/// indistinguishable from a cut one. The gap is not theoretical — the update gate is
/// a BUILD NUMBER comparison (`github.rs`), and a dev build's number is HEAD's
/// committer epoch, so a build from an older commit sits below the channel head and
/// the next background check silently replaces the build you are testing, with no
/// prompt. This key is the explicit signal.
///
/// It lives in `Info.plist` because that file is codesign-sealed: editing it to fake
/// or strip the mark invalidates the signature (verified: `codesign --verify` then
/// reports "invalid Info.plist (plist or signature have been modified)"), so the
/// mark is tamper-evident rather than advisory.
pub const DEV_BUILD_KEY: &str = "<key>ATermDevBuild</key>";

/// Whether an `Info.plist`'s text marks a dev build. Pure, so the contract is
/// unit-testable without a bundle on disk.
///
/// Fails **open** (not a dev build) on anything unclear — absent key, empty or
/// non-`true` value, binary/unreadable plist. The mark only ever ADDS inertness to a
/// bundle that explicitly carries it; a corrupt plist must never silently switch a
/// normal install's updates off, which is the silent-stranding failure class this
/// crate exists to avoid.
#[must_use]
pub fn plist_marks_dev_build(text: &str) -> bool {
    crate::manifest::xml_plist_string(text, DEV_BUILD_KEY)
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

/// Read the dev mark off a bundle on disk. Unreadable ⇒ false (see
/// [`plist_marks_dev_build`] for why the failure direction is open).
fn is_dev_marked(app_root: &Path) -> bool {
    std::fs::read_to_string(app_root.join("Contents/Info.plist"))
        .is_ok_and(|text| plist_marks_dev_build(&text))
}

/// Resolve the installed bundle the updater **may replace**, or `None` when it must
/// not act:
///
/// * not the `…/<name>.app/Contents/MacOS/<exe>` layout (dev build, `cargo run`,
///   `target/release` binary) — there is nothing to swap;
/// * an **App-Translocation** path (`…/AppTranslocation/…`) — a read-only,
///   randomized ephemeral copy; swapping it would be wrong and would fail;
/// * a path under `/Volumes/…` — running directly from a mounted DMG (read-only);
/// * a bundle carrying [`DEV_BUILD_KEY`] — a local build installed in place.
///
/// Paths that only OBSERVE the installed bundle, or FINALIZE a swap a previous run
/// already performed, must use [`resolve_layout`] instead.
pub fn resolve() -> Option<Bundle> {
    let bundle = resolve_layout()?;
    // Dev-marked: inert for every ACQUIRING path, exactly like a `target/` binary.
    // Rejoining the channel is then a deliberate act (`tools/install.sh`), never
    // something a background check decides on the operator's behalf.
    if is_dev_marked(&bundle.app_root) {
        return None;
    }
    Some(bundle)
}

/// Resolve the bundle by LAYOUT alone, ignoring the dev mark.
///
/// For the two kinds of caller that must keep working on a dev-marked install:
///
/// * **observation** — reporting provenance/trial facts about the bundle, which are
///   no less true for a local build;
/// * **finalization** — boot-health confirmation and crash-loop rollback, which
///   install nothing but complete a swap a previous run already made. Gating those
///   would strand an armed trial (post-swap, pre-confirmation) with no way to
///   confirm or revert if a bundle were marked between the swap and the next launch.
///
/// Anything that STAGES or APPLIES goes through [`resolve`].
pub fn resolve_layout() -> Option<Bundle> {
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
    fn dev_mark_reads_only_an_explicit_true() {
        let plist = |body: &str| {
            format!(
                "<plist><dict><key>CFBundleVersion</key><string>7</string>{body}</dict></plist>"
            )
        };
        assert!(plist_marks_dev_build(&plist(
            "<key>ATermDevBuild</key><string>true</string>"
        )));
        // The shape `tools/dev-app.sh` actually emits (newline + tab), and case.
        assert!(plist_marks_dev_build(&plist(
            "<key>ATermDevBuild</key>\n\t<string>TRUE</string>"
        )));
        // Everything unclear fails OPEN — a normal install keeps updating.
        assert!(!plist_marks_dev_build(&plist("")));
        assert!(!plist_marks_dev_build(&plist(
            "<key>ATermDevBuild</key><string>false</string>"
        )));
        assert!(!plist_marks_dev_build(&plist(
            "<key>ATermDevBuild</key><string></string>"
        )));
        assert!(!plist_marks_dev_build("\u{0}bplist00 binary garbage"));
    }

    #[test]
    fn dev_mark_cannot_be_lent_by_an_intervening_key() {
        // Same hazard `xml_plist_string` guards for release identity: a later key's
        // string must not bind to this one.
        assert!(!plist_marks_dev_build(
            "<plist><dict><key>ATermDevBuild</key><key>Other</key><string>true</string></dict></plist>"
        ));
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
