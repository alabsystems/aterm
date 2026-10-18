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
/// install. Stamped by `targo --unverified ship dev-app` (its `--no-pin` omits it).
///
/// Layout alone cannot tell a dev build from a release, and the update gate is a
/// BUILD NUMBER comparison (`github.rs`) against a dev build's HEAD-committer-epoch
/// number, so without this mark a background check would silently replace an older
/// commit's build under test.
///
/// It lives in `Info.plist` because that file is codesign-sealed: editing it to fake
/// or strip the mark invalidates the signature, so the mark is tamper-evident.
pub const DEV_BUILD_KEY: &str = "<key>ATermDevBuild</key>";

/// Whether an `Info.plist`'s text marks a dev build. Pure, so the contract is
/// unit-testable without a bundle on disk.
///
/// Fails **open** (not a dev build) on anything unclear — absent key, empty or
/// non-`true` value, binary/unreadable plist: a corrupt plist must never silently
/// switch a normal install's updates off.
#[must_use]
pub fn plist_marks_dev_build(text: &str) -> bool {
    crate::manifest::xml_plist_string(text, DEV_BUILD_KEY)
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

/// Read the dev mark off a bundle on disk. Unreadable ⇒ false (see
/// [`plist_marks_dev_build`] for why the failure direction is open). Bounded
/// (`read_ledger_text`): `which_copy` asks this on every `--version` and About open.
pub(crate) fn is_dev_marked(app_root: &Path) -> bool {
    crate::read_ledger_text(&app_root.join("Contents/Info.plist"))
        .is_some_and(|text| plist_marks_dev_build(&text))
}

/// The `Info.plist` key by which a bundle DECLARES it can take a CHUNKED
/// rendezvous grant (`aterm-gui`'s `handoff_rendezvous`, `ATRZ2G`): the handoff
/// descriptors of more sessions than one `SCM_RIGHTS` message carries, sent over
/// several. Its value is the capability's token, [`HANDOFF_GRANT_CHUNKS_TOKEN`].
///
/// A DECLARED CAPABILITY, NOT A POLICY KNOB: read only after the candidate's
/// signature, sealed identity and authorization verified (the handoff pre-verify),
/// so it is sealed evidence of what the candidate's code does, never a way to relax
/// a check. Anything but the exact token reads as "not declared"; a read the system
/// refused FOR NOW is no answer at all ([`declares_grant_chunks`]).
pub const HANDOFF_GRANT_CHUNKS_KEY: &str = "<key>ATermHandoffGrantChunks</key>";

/// The value [`HANDOFF_GRANT_CHUNKS_KEY`] carries in a bundle that reads chunked
/// grants, and the token the parent publishes in the successor's
/// `ATERM_HANDOFF_GRANT_CAPS`.
pub const HANDOFF_GRANT_CHUNKS_TOKEN: &str = "chunks1";

/// Whether an `Info.plist`'s text declares the chunked grant. Pure; fails closed
/// (not declared) on anything but the exact token.
#[must_use]
pub fn plist_declares_grant_chunks(text: &str) -> bool {
    crate::manifest::xml_plist_string(text, HANDOFF_GRANT_CHUNKS_KEY)
        .is_some_and(|value| value == HANDOFF_GRANT_CHUNKS_TOKEN)
}

/// Read the chunked-grant declaration off a bundle on disk. Called only on a
/// bundle whose signature has just been verified.
///
/// `Err` is a read the system refused FOR NOW ([`grant_chunks_judged`]): the
/// caller refuses the pre-verification as a passing condition rather than cache
/// "not declared" with the candidate's pass.
pub(crate) fn declares_grant_chunks(app_root: &Path) -> Result<bool, String> {
    let path = app_root.join("Contents/Info.plist");
    grant_chunks_judged(&path, crate::read_ledger_text_checked(&path))
}

/// What the read of the `Info.plist` at `path` says about the chunked grant.
///
/// A MOMENT IS NOT A VERDICT: a read the system refused just then (`EMFILE`, `EIO`,
/// `ENOMEM`: [`aterm_update_core::handoff_policy::is_momentary_read_error`]) never
/// reached the bytes, and caching it as "not declared" with the pass would deny a
/// capable candidate the chunked grant for the cache's life. `Err` names it; an
/// error the same file would give again (`EACCES`, a FIFO, over the cap, not UTF-8)
/// stays "not declared".
pub(crate) fn grant_chunks_judged(
    path: &Path,
    read: std::io::Result<Option<String>>,
) -> Result<bool, String> {
    match read {
        Ok(text) => Ok(text.is_some_and(|text| plist_declares_grant_chunks(&text))),
        Err(error) if aterm_update_core::handoff_policy::is_momentary_read_error(&error) => {
            Err(format!("{}: unreadable: {error}", path.display()))
        }
        Err(_) => Ok(false),
    }
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
    // Deliberate symlink-shim resolution (a `/usr/local/bin/aterm` launcher names
    // the real bundle executable), not an identity comparison.
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    resolve_from(&exe)
}

pub use crate::install_posture::InstallPosture;

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

/// What probing the bundle's PARENT for a write answered ([`probe_parent_write`]).
#[derive(Debug, PartialEq, Eq)]
pub enum ParentWrite {
    /// An entry was created there: the swap's renames can land.
    Writable,
    /// This user may not write there — `EACCES`/`EPERM` (an admin-owned or
    /// MDM-managed `/Applications`), `EROFS` (a read-only volume) — or the error
    /// is one that answers the same way every time. A person's to clear.
    NotWritable(String),
    /// The folder may well be writable, but the write failed for a reason of this
    /// MOMENT: a full disk (`ENOSPC`), a quota (`EDQUOT`), an I/O error (`EIO`), an
    /// interrupted call, or leftover probe names past every fresh one tried.
    /// Retried, never a person's.
    Passing(String),
}

/// How many fresh probe names [`probe_parent_write`] tries past a name that is
/// already taken (a probe a killed process left behind, a sibling mid-probe).
const PROBE_NAMES: u32 = 8;

/// Whether we can replace the bundle in place: the swap operates in the bundle's
/// **parent** directory, so that parent must be writable. Probe it directly by
/// creating + removing a temp entry (more accurate than an `access()` mode guess,
/// which misreads ACL- and MDM-managed `/Applications`).
///
/// ONLY A REFUSED WRITE IS "NOT WRITABLE": otherwise the pre-park gate would tell a
/// person to move the app over a full disk or a leftover probe. A taken name is
/// probed again under a fresh one; error kinds are split by [`classify_probe_error`].
pub fn probe_parent_write(app_root: &Path) -> ParentWrite {
    let Some(parent) = app_root.parent() else {
        return ParentWrite::NotWritable(format!("{} has no parent folder", app_root.display()));
    };
    let pid = std::process::id();
    let mut last_taken = None;
    for attempt in 0..=PROBE_NAMES {
        let name = if attempt == 0 {
            format!(".aterm-update-probe-{pid}")
        } else {
            format!(".aterm-update-probe-{pid}-{attempt}")
        };
        let probe = parent.join(name);
        match std::fs::create_dir(&probe) {
            Ok(()) => {
                let _ = std::fs::remove_dir(&probe);
                return ParentWrite::Writable;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last_taken = Some(probe);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return classify_probe_error(parent, &error),
        }
    }
    ParentWrite::Passing(format!(
        "every probe name in {} was already taken (last {})",
        parent.display(),
        last_taken.map_or_else(String::new, |probe| probe.display().to_string())
    ))
}

/// The verdict a failed probe `create_dir` in `parent` carries. A PERMISSION
/// answer (`EACCES`, `EPERM`, `EROFS`) is the person's; a MOMENT (`ENOSPC`,
/// `EDQUOT`, `EIO`, `EINTR`, and the process-table/descriptor/memory moments
/// [`crate::verify::io_error_is_passing`] names) is passing. Anything else is not
/// writable, since it answers the same way on every try.
pub(crate) fn classify_probe_error(parent: &Path, error: &std::io::Error) -> ParentWrite {
    let message = format!("create a probe in {}: {error}", parent.display());
    let passing = matches!(
        error.raw_os_error(),
        Some(libc::ENOSPC | libc::EDQUOT | libc::EIO | libc::EINTR)
    ) || crate::verify::io_error_is_passing(error);
    if passing {
        ParentWrite::Passing(message)
    } else {
        ParentWrite::NotWritable(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a refused write is the person's; a full disk, a quota or an I/O error
    /// is a moment.
    #[test]
    fn a_probe_error_is_the_person_s_only_when_the_write_was_refused() {
        let parent = Path::new("/Applications");
        for errno in [libc::EACCES, libc::EPERM, libc::EROFS] {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(
                matches!(
                    classify_probe_error(parent, &error),
                    ParentWrite::NotWritable(_)
                ),
                "{error}"
            );
        }
        for errno in [
            libc::ENOSPC,
            libc::EDQUOT,
            libc::EIO,
            libc::EINTR,
            libc::EMFILE,
        ] {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(
                matches!(
                    classify_probe_error(parent, &error),
                    ParentWrite::Passing(_)
                ),
                "{error}"
            );
        }
    }

    /// A probe a killed launch left behind is not "not writable": the probe takes
    /// a fresh name. And a folder this user cannot write still says so.
    #[test]
    fn a_leftover_probe_name_is_probed_again_under_a_fresh_one() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "aterm-update-probe-parent-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let app = root.join("aterm.app");
        std::fs::create_dir_all(&app).unwrap();
        let leftover = root.join(format!(".aterm-update-probe-{}", std::process::id()));
        std::fs::create_dir(&leftover).unwrap();
        assert_eq!(probe_parent_write(&app), ParentWrite::Writable);
        assert!(leftover.is_dir(), "the leftover is not ours to remove");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        let refused = probe_parent_write(&app);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            matches!(refused, ParentWrite::NotWritable(_)),
            "{refused:?}"
        );
    }

    /// The doctor and the updater must never disagree about the same install:
    /// whatever `resolve_from` refuses to act on is exactly what `posture_from`
    /// reports as unable to update, and only the two fixable states advise a move.
    /// Translocation outranks the volume marker (a translocated path is
    /// randomized), and an install on an external disk is refused too, which keeps
    /// an ejected volume out of the boot trial's ownership rule.
    #[test]
    fn posture_agrees_with_what_the_updater_will_act_on() {
        let b = resolve_from(Path::new("/Applications/aterm.app/Contents/MacOS/aterm")).unwrap();
        assert_eq!(b.app_root, Path::new("/Applications/aterm.app"));
        assert_eq!(
            b.exe,
            Path::new("/Applications/aterm.app/Contents/MacOS/aterm")
        );
        for (p, want) in [
            (
                "/Applications/aterm.app/Contents/MacOS/aterm",
                InstallPosture::Installed,
            ),
            (
                "/Users//ana/Applications/aterm.app/Contents/MacOS/aterm",
                InstallPosture::Installed,
            ),
            (
                "/Volumes/aterm 0.67.0/aterm.app/Contents/MacOS/aterm",
                InstallPosture::MountedImage,
            ),
            (
                "/Volumes/Ext/Applications/aterm.app/Contents/MacOS/aterm",
                InstallPosture::MountedImage,
            ),
            (
                "/private/var/folders/zz/AppTranslocation/ABC/d/aterm.app/Contents/MacOS/aterm",
                InstallPosture::Translocated,
            ),
            (
                "/Volumes/x/private/var/AppTranslocation/ABC/d/aterm.app/Contents/MacOS/aterm",
                InstallPosture::Translocated,
            ),
            (
                "/Users//ana/aterm/target/release/aterm",
                InstallPosture::NotABundle,
            ),
            ("/usr/local/bin/aterm", InstallPosture::NotABundle),
            ("/tmp/aterm", InstallPosture::NotABundle),
        ] {
            let path = Path::new(p);
            assert_eq!(posture_from(path), want, "{p}");
            assert_eq!(
                resolve_from(path).is_some(),
                want == InstallPosture::Installed,
                "the updater and the doctor disagree about {p}"
            );
            assert_eq!(
                want.remedy().is_some(),
                matches!(
                    want,
                    InstallPosture::MountedImage | InstallPosture::Translocated
                ),
                "{p}"
            );
        }
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
        // The shape `ship dev-app` actually emits (newline + tab), and case.
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
        // A later key's string must not bind to this one (the hazard
        // `xml_plist_string` guards for release identity).
        assert!(!plist_marks_dev_build(
            "<plist><dict><key>ATermDevBuild</key><key>Other</key><string>true</string></dict></plist>"
        ));
    }

    #[test]
    fn the_chunked_grant_is_declared_only_by_its_exact_token() {
        let plist = |body: &str| {
            format!(
                "<plist><dict><key>CFBundleVersion</key><string>7</string>{body}</dict></plist>"
            )
        };
        assert!(plist_declares_grant_chunks(&plist(
            "<key>ATermHandoffGrantChunks</key>\n\t<string>chunks1</string>"
        )));
        for undeclared in [
            "",
            "<key>ATermHandoffGrantChunks</key><string></string>",
            "<key>ATermHandoffGrantChunks</key><string>chunks2</string>",
            "<key>ATermHandoffGrantChunks</key><string>true</string>",
            "<key>ATermHandoffGrantChunks</key><key>Other</key><string>chunks1</string>",
        ] {
            assert!(
                !plist_declares_grant_chunks(&plist(undeclared)),
                "{undeclared:?} declares nothing"
            );
        }
        assert!(!plist_declares_grant_chunks("\u{0}bplist00 binary garbage"));
    }

    /// THE SHIPPED TEMPLATE DECLARES IT: this build reads chunked grants, so the
    /// bundle it ships in says so, and the build before it may hand this one more
    /// sessions than one descriptor message carries.
    #[test]
    fn the_shipped_info_plist_declares_the_chunked_grant() {
        let template = include_str!("../../../apps/aterm-mac/Info.plist");
        assert!(plist_declares_grant_chunks(template));
    }
}
