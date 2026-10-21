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

/// The `Info.plist` key marking a bundle as a local dev build the updater must not
/// replace. `<string>true</string>`; any other value, or absence, is a normal
/// install. Stamped by `targo --unverified ship dev-app` (its `--no-pin` omits it).
///
/// Layout cannot tell a dev build from a release, and the update gate compares build
/// numbers against a dev build's HEAD-committer-epoch number, so without the mark a
/// background check would replace a build under test. `Info.plist` is codesign-sealed,
/// so faking or stripping the mark breaks the signature.
pub const DEV_BUILD_KEY: &str = "<key>ATermDevBuild</key>";

/// Whether an `Info.plist`'s text marks a dev build.
///
/// Fails open (not a dev build) on anything unclear (absent key, non-`true` value, binary
/// or unreadable plist): a corrupt plist must never switch a normal install's updates off.
#[must_use]
pub fn plist_marks_dev_build(text: &str) -> bool {
    crate::manifest::xml_plist_string(text, DEV_BUILD_KEY)
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

/// Read the dev mark off a bundle on disk; unreadable is false. Bounded
/// (`read_ledger_text`): `which_copy` asks on every `--version` and About open.
pub(crate) fn is_dev_marked(app_root: &Path) -> bool {
    crate::read_ledger_text(&app_root.join("Contents/Info.plist"))
        .is_some_and(|text| plist_marks_dev_build(&text))
}

/// The `Info.plist` key by which a bundle declares it takes a chunked rendezvous grant
/// (`aterm-gui`'s `handoff_rendezvous`, `ATRZ2G`): the descriptors of more sessions than
/// one `SCM_RIGHTS` message carries, sent over several. Its value is
/// [`HANDOFF_GRANT_CHUNKS_TOKEN`].
///
/// A declared capability, not a policy knob: read only after the candidate's signature,
/// sealed identity and authorization verified, so it never relaxes a check. Anything but
/// the exact token is "not declared".
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

/// Read the chunked-grant declaration off a bundle whose signature was just verified.
/// `Err` is a momentary read refusal ([`grant_chunks_judged`]): the caller fails the
/// pre-verification as passing rather than cache "not declared".
pub(crate) fn declares_grant_chunks(app_root: &Path) -> Result<bool, String> {
    let path = app_root.join("Contents/Info.plist");
    grant_chunks_judged(&path, crate::read_ledger_text_checked(&path))
}

/// What the read of the `Info.plist` at `path` says about the chunked grant.
///
/// A momentary refusal ([`aterm_update_core::handoff_policy::is_momentary_read_error`]:
/// `EMFILE`, `EIO`, `ENOMEM`) never reached the bytes, so it is `Err`, not a "not declared"
/// cached with the pass for the cache's life. An error the same file would give again
/// (`EACCES`, a FIFO, over the cap, not UTF-8) is "not declared".
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

/// Resolve the installed bundle the updater may replace, or `None` when it must not act:
///
/// * not the `…/<name>.app/Contents/MacOS/<exe>` layout (a `target/` binary): nothing to
///   swap;
/// * an App Translocation path (`…/AppTranslocation/…`): a read-only, randomized copy;
/// * under `/Volumes/…`: running from a mounted DMG (read-only);
/// * a bundle carrying [`DEV_BUILD_KEY`]: a local build installed in place.
///
/// Paths that only observe the bundle, or finalize a swap a previous run performed, use
/// [`resolve_layout`].
pub fn resolve() -> Option<Bundle> {
    let bundle = resolve_layout()?;
    // A dev-marked bundle rejoins the channel only by a deliberate `tools/install.sh`.
    if is_dev_marked(&bundle.app_root) {
        return None;
    }
    Some(bundle)
}

/// Resolve the bundle by layout alone, ignoring the dev mark.
///
/// For callers that must keep working on a dev-marked install: observation (provenance and
/// trial facts) and finalization (boot-health confirmation, crash-loop rollback), which
/// complete a swap a previous run made; gating those would strand an armed trial if the
/// bundle were marked between the swap and the next launch. Anything that stages or
/// applies goes through [`resolve`].
pub fn resolve_layout() -> Option<Bundle> {
    // Canonicalized so a `/usr/local/bin/aterm` symlink names the real bundle executable.
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    resolve_from(&exe)
}

pub use crate::install_posture::InstallPosture;

/// Classify a running executable's path: the pure core of the first-open doctor.
///
/// Translocation is tested first: a translocated DMG launch matches both markers, and its
/// randomized path is the more specific truth. The dev mark is not consulted: a dev build
/// is a healthy install to the person reading the report, and [`resolve`] gates on it.
#[must_use]
pub fn posture_from(exe: &Path) -> InstallPosture {
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

/// Pure core of [`resolve_layout`], testable with a synthetic path.
pub fn resolve_from(exe: &Path) -> Option<Bundle> {
    // Never swap a translocated or mounted-image launch. Lossy is fine: the string is
    // only matched against ASCII markers, never used as a path.
    let s = exe.to_string_lossy();
    if s.contains("/AppTranslocation/") || s.starts_with("/Volumes/") {
        return None;
    }
    layout_of(exe)
}

/// The `…/<X>.app/Contents/MacOS/<exe>` shape alone, with no translocation, volume or
/// dev-mark gate. For naming the bundle a process runs from (`which_copy`: a DMG launch
/// still names `/Volumes/aterm/aterm.app`), never for acting on it.
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

/// What probing the bundle's parent for a write answered ([`probe_parent_write`]).
#[derive(Debug, PartialEq, Eq)]
pub enum ParentWrite {
    /// An entry was created there: the swap's renames can land.
    Writable,
    /// `EACCES`/`EPERM` (an admin-owned or MDM-managed `/Applications`), `EROFS`, or an
    /// error that answers the same every time. A person's to clear.
    NotWritable(String),
    /// A failure of the moment: `ENOSPC`, `EDQUOT`, `EIO`, an interrupted call, or every
    /// fresh probe name already taken. Retried, never a person's.
    Passing(String),
}

/// How many fresh probe names [`probe_parent_write`] tries past a name that is
/// already taken (a probe a killed process left behind, a sibling mid-probe).
const PROBE_NAMES: u32 = 8;

/// Whether the swap's renames can land in the bundle's parent, probed by creating and
/// removing an entry (an `access()` check misreads ACL- and MDM-managed `/Applications`).
///
/// Only a refused write is `NotWritable`, so the pre-park gate never tells a person to
/// move the app over a full disk or a leftover probe; a taken name is retried fresh.
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

/// The verdict of a failed probe `create_dir` in `parent`: `ENOSPC`, `EDQUOT`, `EIO`,
/// `EINTR` and [`crate::verify::io_error_is_passing`]'s errors are passing; anything else
/// (`EACCES`, `EPERM`, `EROFS`, …) answers the same on every try and is not writable.
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

    /// A leftover probe is retried under a fresh name; an unwritable folder still says so.
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
        // Everything unclear fails open: a normal install keeps updating.
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

    /// This build reads chunked grants, so the bundle it ships in declares them.
    #[test]
    fn the_shipped_info_plist_declares_the_chunked_grant() {
        let template = include_str!("../../../apps/aterm-mac/Info.plist");
        assert!(plist_declares_grant_chunks(template));
    }
}
