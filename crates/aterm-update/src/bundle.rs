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
/// [`plist_marks_dev_build`] for why the failure direction is open). Bounded like
/// every other small file the updater consults (`read_ledger_text`): a plist over
/// the cap is "unreadable", never unbounded work — the S12 `which_copy` report asks
/// this of the running bundle on every `--version` and About open.
pub(crate) fn is_dev_marked(app_root: &Path) -> bool {
    crate::read_ledger_text(&app_root.join("Contents/Info.plist"))
        .is_some_and(|text| plist_marks_dev_build(&text))
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

/// WHERE a running copy is installed from, and therefore what it can and cannot
/// do for the person using it. `resolve_from` answers the updater's question —
/// "may I replace this?" — as a bare `Option`, which is the right shape for the
/// updater and the wrong shape for a human: "no" is the same value whether the
/// copy is a dev build or a download the user never moved out of their Downloads
/// folder, and only one of those is something they can fix.
///
/// This is that same judgement, kept as a REASON. It is what a first-open doctor
/// reports, and it is deliberately pure — the classification is a function of the
/// path alone, so every case below is a unit test rather than a situation someone
/// has to reproduce by hand on a fresh Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPosture {
    /// A normal install. The bundle can be replaced in place, so self-update
    /// works and the shell shims can be written beside it.
    Installed,
    /// Running straight out of a mounted disk image (`/Volumes/…`) — the user
    /// double-clicked `aterm.app` inside the DMG window instead of dragging it
    /// to Applications. The volume is read-only and disappears on eject.
    MountedImage,
    /// Gatekeeper is running this copy from a randomized, read-only path (App
    /// Translocation), which is what happens to a quarantined app launched
    /// WITHOUT being moved first — typically unzipped and opened from
    /// `~/Downloads`. The copy the user can see is not the copy that is running.
    Translocated,
    /// Not a `…/<name>.app/Contents/MacOS/<exe>` layout at all: a `cargo run` or
    /// a `target/release` binary. Nothing is wrong; there is simply no bundle.
    NotABundle,
}

impl InstallPosture {
    /// Whether this copy can replace itself — the updater's question, answered
    /// from the same classification the human-facing text uses, so the two can
    /// never drift into disagreeing about the same install.
    #[must_use]
    pub fn can_update(self) -> bool {
        matches!(self, Self::Installed)
    }

    /// Whether moving the app to `/Applications` is what fixes it. False for a
    /// dev build, where nothing is broken and the advice would be wrong.
    #[must_use]
    pub fn wants_move_to_applications(self) -> bool {
        matches!(self, Self::MountedImage | Self::Translocated)
    }

    /// One line naming what is true, in the user's terms rather than the
    /// updater's. No trailing period: callers put this in a sentence.
    #[must_use]
    pub fn summary(self) -> &'static str {
        match self {
            Self::Installed => "aterm is installed and can keep itself up to date",
            Self::MountedImage => "aterm is running from the disk image, not from Applications",
            Self::Translocated => {
                "macOS is running aterm from a temporary read-only copy, because it was opened \
                 without being moved first"
            }
            Self::NotABundle => "aterm is running as a plain binary, not from an app bundle",
        }
    }

    /// What the person should DO, or `None` when there is nothing to fix. The
    /// two consequences are named because they are both invisible otherwise:
    /// the copy silently cannot update itself, and it silently does not put
    /// `aterm` on the PATH of a new shell.
    #[must_use]
    pub fn remedy(self) -> Option<&'static str> {
        match self {
            Self::Installed | Self::NotABundle => None,
            Self::MountedImage => Some(
                "Drag aterm to Applications and open it from there. Until then it cannot update \
                 itself, and it cannot add `aterm` to your PATH.",
            ),
            Self::Translocated => Some(
                "Move aterm into Applications and open it again. Until then it cannot update \
                 itself, and it cannot add `aterm` to your PATH.",
            ),
        }
    }
}

/// Classify a running executable's path — the pure core of the first-open
/// doctor, testable with a synthetic path exactly like [`resolve_from`].
///
/// ORDER MATTERS. A translocated copy of a DMG launch can match both markers,
/// and translocation is the more specific truth (the path is randomized, so the
/// user cannot even find the copy that is running); it is therefore tested
/// first. Everything else that is bundle-shaped is `Installed` — the dev mark
/// is deliberately NOT consulted here, because a dev build is a healthy install
/// as far as the person looking at the report is concerned, and [`resolve`]
/// already gates acquisition on it separately.
#[must_use]
pub fn posture_from(exe: &Path) -> InstallPosture {
    // Lossy is fine and deliberate, exactly as in `resolve_from`: the string is
    // only substring-matched against ASCII markers, never used as a path.
    let s = exe.to_string_lossy();
    if s.contains("/AppTranslocation/") {
        return InstallPosture::Translocated;
    }
    if s.starts_with("/Volumes/") {
        return InstallPosture::MountedImage;
    }
    if layout_of(exe).is_none() {
        return InstallPosture::NotABundle;
    }
    InstallPosture::Installed
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
    layout_of(exe)
}

/// The `…/<X>.app/Contents/MacOS/<exe>` SHAPE alone — no translocation, volume or
/// dev-mark gate. For NAMING the bundle a process runs from (`which_copy`: a DMG
/// launch is still "running: /Volumes/aterm/aterm.app"), never for acting on it —
/// every acquiring path goes through [`resolve`] / [`resolve_from`].
pub(crate) fn layout_of(exe: &Path) -> Option<Bundle> {
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
    fn posture_names_the_reason_not_just_the_refusal() {
        let installed = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        let dmg = Path::new("/Volumes/aterm 0.67.0/aterm.app/Contents/MacOS/aterm");
        let transloc = Path::new(
            "/private/var/folders/zz/AppTranslocation/ABC/d/aterm.app/Contents/MacOS/aterm",
        );
        let bare = Path::new("/Users//ana/aterm/target/release/aterm");

        assert_eq!(posture_from(installed), InstallPosture::Installed);
        assert_eq!(posture_from(dmg), InstallPosture::MountedImage);
        assert_eq!(posture_from(transloc), InstallPosture::Translocated);
        assert_eq!(posture_from(bare), InstallPosture::NotABundle);
    }

    /// The doctor and the updater must never disagree about the same install:
    /// whatever `resolve_from` refuses to act on is exactly what `posture_from`
    /// reports as unable to update. (A dev-MARKED bundle is the one deliberate
    /// exception and is not path-classifiable, so it is not covered here — see
    /// `posture_from`'s note.)
    #[test]
    fn posture_agrees_with_what_the_updater_will_act_on() {
        for p in [
            "/Applications/aterm.app/Contents/MacOS/aterm",
            "/Users//ana/Applications/aterm.app/Contents/MacOS/aterm",
            "/Volumes/aterm 0.67.0/aterm.app/Contents/MacOS/aterm",
            "/private/var/folders/zz/AppTranslocation/ABC/d/aterm.app/Contents/MacOS/aterm",
            "/Users//ana/aterm/target/release/aterm",
            "/usr/local/bin/aterm",
        ] {
            let path = Path::new(p);
            assert_eq!(
                resolve_from(path).is_some(),
                posture_from(path).can_update(),
                "the updater and the doctor disagree about {p}"
            );
        }
    }

    /// Only the two fixable states advise a move, and both name the two
    /// consequences that are otherwise invisible.
    #[test]
    fn only_fixable_states_advise_a_move() {
        assert!(InstallPosture::MountedImage.wants_move_to_applications());
        assert!(InstallPosture::Translocated.wants_move_to_applications());
        assert!(!InstallPosture::Installed.wants_move_to_applications());
        assert!(!InstallPosture::NotABundle.wants_move_to_applications());

        assert!(InstallPosture::Installed.remedy().is_none());
        assert!(InstallPosture::NotABundle.remedy().is_none());
        for p in [InstallPosture::MountedImage, InstallPosture::Translocated] {
            let remedy = p.remedy().expect("a fixable state must say what to do");
            assert!(remedy.contains("Applications"), "{remedy}");
            assert!(remedy.contains("update itself"), "{remedy}");
            assert!(remedy.contains("PATH"), "{remedy}");
            assert!(
                !p.summary().ends_with('.'),
                "summary is a clause: {}",
                p.summary()
            );
        }
    }

    /// Translocation wins over the volume marker: a translocated path is
    /// randomized, so "drag it from the disk image" would name a copy the user
    /// cannot find.
    #[test]
    fn translocation_outranks_the_volume_marker() {
        assert_eq!(
            posture_from(Path::new(
                "/Volumes/x/private/var/AppTranslocation/ABC/d/aterm.app/Contents/MacOS/aterm"
            )),
            InstallPosture::Translocated
        );
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
