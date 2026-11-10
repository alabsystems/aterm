// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolving the installed `.app` bundle this process is running from.

use std::path::{Path, PathBuf};

#[path = "bundle_capabilities.rs"]
mod capabilities;

/// A resolved installed bundle: the `.app` root and the executable inside it.
pub struct Bundle {
    /// `…/aterm.app`.
    pub app_root: PathBuf,
    /// `…/aterm.app/Contents/MacOS/aterm` (the re-exec target).
    pub exe: PathBuf,
}

/// The `Info.plist` key marking a bundle as a local dev build the updater must not replace.
pub const DEV_BUILD_KEY: &str = "<key>ATermDevBuild</key>";

/// Whether an `Info.plist`'s text marks a dev build.
#[must_use]
pub fn plist_marks_dev_build(text: &str) -> bool {
    crate::manifest::xml_plist_string(text, DEV_BUILD_KEY)
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

/// Read the dev mark off a bundle on disk; unreadable is false.
pub(crate) fn is_dev_marked(app_root: &Path) -> bool {
    crate::read_ledger_text(&app_root.join("Contents/Info.plist"))
        .is_some_and(|text| plist_marks_dev_build(&text))
}

/// The `Info.plist` key by which a bundle declares it takes a chunked rendezvous grant.
pub const HANDOFF_GRANT_CHUNKS_KEY: &str = "<key>ATermHandoffGrantChunks</key>";

/// The value [`HANDOFF_GRANT_CHUNKS_KEY`] carries in a bundle that reads chunked grants.
pub const HANDOFF_GRANT_CHUNKS_TOKEN: &str = "chunks1";

/// Whether an `Info.plist`'s text declares the chunked grant.
#[must_use]
pub fn plist_declares_grant_chunks(text: &str) -> bool {
    Capabilities::from_plist(text).grant_chunks
}

/// The `Info.plist` key by which a bundle declares it takes a sessionless rendezvous grant.
pub const HANDOFF_GRANT_EMPTY_KEY: &str = "<key>ATermHandoffGrantEmpty</key>";

/// The value [`HANDOFF_GRANT_EMPTY_KEY`] carries in a bundle that reads sessionless grants.
pub const HANDOFF_GRANT_EMPTY_TOKEN: &str = "empty1";

/// Whether an `Info.plist`'s text declares the sessionless grant.
#[must_use]
pub fn plist_declares_grant_empty(text: &str) -> bool {
    Capabilities::from_plist(text).grant_empty
}

/// The `Info.plist` key by which a bundle declares it rebuilds a window that held only an empty
/// Workers list from its row in a handoff layout.
pub const HANDOFF_GRANT_DESK_KEY: &str = "<key>ATermHandoffGrantDesk</key>";

/// The value [`HANDOFF_GRANT_DESK_KEY`] carries in a bundle that rebuilds such a window.
pub const HANDOFF_GRANT_DESK_TOKEN: &str = "desk1";

/// Whether an `Info.plist`'s text declares the desk grant.
#[must_use]
pub fn plist_declares_grant_desk(text: &str) -> bool {
    Capabilities::from_plist(text).grant_desk
}

/// The `Info.plist` key declaring complete-recipient journal recovery support.
pub const NATIVE_RECIPIENT_RECOVERY_KEY: &str = "<key>ATermNativeRecipientRecovery</key>";

/// Reads ANG5 histories and recovers them only with complete recipient retirement evidence.
pub const NATIVE_RECIPIENT_RECOVERY_TOKEN: &str = "recipients5";

/// Whether an `Info.plist` declares complete-recipient recovery, not writer admission.
#[must_use]
pub fn plist_declares_recipient_recovery(text: &str) -> bool {
    Capabilities::from_plist(text).recipient_recovery
}

/// Declarations from one immutable read of a candidate's sealed `Info.plist`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Capabilities {
    pub grant_chunks: bool,
    pub grant_empty: bool,
    pub grant_desk: bool,
    pub recipient_recovery: bool,
}

impl Capabilities {
    /// Read only after authenticating the candidate bundle.
    pub(crate) fn read(app_root: &Path) -> Result<Self, String> {
        let path = app_root.join("Contents/Info.plist");
        Self::judge(&path, crate::read_ledger_text_checked(&path))
    }

    /// An absent or permanently unreadable plist declares nothing. A transient error
    /// refuses the entire snapshot, rather than returning partially read capabilities.
    pub(crate) fn judge(
        path: &Path,
        read: std::io::Result<Option<String>>,
    ) -> Result<Self, String> {
        match read {
            Ok(Some(text)) => Ok(Self::from_plist(&text)),
            Err(error) if aterm_update_core::handoff_policy::is_momentary_read_error(&error) => {
                Err(format!("{}: unreadable: {error}", path.display()))
            }
            Ok(None) | Err(_) => Ok(Self::default()),
        }
    }
}

/// Resolve the installed bundle the updater may replace, or `None` when it must not act:
///
/// * not the `…/<name>.app/Contents/MacOS/<exe>` layout (a `target/` binary): nothing to
///   swap;
/// * an App Translocation path (`…/AppTranslocation/…`): a read-only, randomized copy;
/// * under `/Volumes/…`: running from a mounted DMG (read-only);
/// * a bundle carrying [`DEV_BUILD_KEY`]: a local build installed in place.
pub fn resolve() -> Option<Bundle> {
    let bundle = resolve_layout()?;
    if is_dev_marked(&bundle.app_root) {
        return None;
    }
    Some(bundle)
}

/// Resolve the bundle by layout alone, ignoring the dev mark.
pub fn resolve_layout() -> Option<Bundle> {
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    resolve_from(&exe)
}

pub use crate::install_posture::InstallPosture;

/// Classify a running executable's path: the pure core of the first-open doctor.
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
    let s = exe.to_string_lossy();
    if s.contains("/AppTranslocation/") || s.starts_with("/Volumes/") {
        return None;
    }
    layout_of(exe)
}

/// The `…/<X>.app/Contents/MacOS/<exe>` shape alone.
pub(crate) fn layout_of(exe: &Path) -> Option<Bundle> {
    let macos = exe.parent()?;
    if macos.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()?.to_str()? != "Contents" {
        return None;
    }
    let app_root = contents.parent()?;
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
    /// `EACCES`/`EPERM`, `EROFS`, or an error that answers the same every time.
    NotWritable(String),
    /// A failure of the moment.
    Passing(String),
}

/// How many fresh probe names [`probe_parent_write`] tries past a name that is
/// already taken (a probe a killed process left behind, a sibling mid-probe).
const PROBE_NAMES: u32 = 8;

/// Whether the swap's renames can land in the bundle's parent.
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

/// The verdict of a failed probe `create_dir` in `parent`.
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
        let root = crate::paths::scratch_dir("probe-parent");
        let app = root.join("aterm.app");
        std::fs::create_dir_all(&app).unwrap();
        let leftover = root.join(format!(".aterm-update-probe-{}", std::process::id()));
        std::fs::create_dir(&leftover).unwrap();
        assert_eq!(probe_parent_write(&app), ParentWrite::Writable);
        assert!(leftover.is_dir(), "the leftover is not ours to remove");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        let refused = probe_parent_write(&app);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            matches!(refused, ParentWrite::NotWritable(_)),
            "{refused:?}"
        );
    }

    /// The doctor and the updater must never disagree about the same install:
    /// whatever `resolve_from` refuses to act on is exactly what `posture_from`
    /// reports as unable to update, and only the two fixable states advise a move.
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
        assert!(plist_marks_dev_build(&plist(
            "<key>ATermDevBuild</key>\n\t<string>TRUE</string>"
        )));
        assert!(!plist_marks_dev_build(&plist("")));
        assert!(!plist_marks_dev_build(&plist(
            "<key>ATermDevBuild</key><string>false</string>"
        )));
        assert!(!plist_marks_dev_build(&plist(
            "<key>ATermDevBuild</key><string></string>"
        )));
        assert!(!plist_marks_dev_build("\u{0}bplist00 binary garbage"));
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

    #[test]
    fn the_sessionless_grant_is_declared_only_by_its_exact_token() {
        let plist = |body: &str| {
            format!(
                "<plist><dict><key>CFBundleVersion</key><string>7</string>{body}</dict></plist>"
            )
        };
        assert!(plist_declares_grant_empty(&plist(
            "<key>ATermHandoffGrantEmpty</key>\n\t<string>empty1</string>"
        )));
        for undeclared in [
            "",
            "<key>ATermHandoffGrantEmpty</key><string></string>",
            "<key>ATermHandoffGrantEmpty</key><string>empty2</string>",
            "<key>ATermHandoffGrantEmpty</key><string>true</string>",
            "<key>ATermHandoffGrantEmpty</key><key>Other</key><string>empty1</string>",
            "<key>ATermHandoffGrantChunks</key><string>chunks1</string>",
        ] {
            assert!(
                !plist_declares_grant_empty(&plist(undeclared)),
                "{undeclared:?} declares nothing"
            );
        }
        assert!(!plist_declares_grant_empty("\u{0}bplist00 binary garbage"));
        let both = plist(
            "<key>ATermHandoffGrantChunks</key><string>chunks1</string>\
             <key>ATermHandoffGrantEmpty</key><string>empty1</string>",
        );
        assert!(plist_declares_grant_empty(&both) && plist_declares_grant_chunks(&both));
    }

    /// This build reads sessionless grants, so the bundle it ships in declares them.
    #[test]
    fn the_shipped_info_plist_declares_the_sessionless_grant() {
        let template = include_str!("../../../apps/aterm-mac/Info.plist");
        assert!(plist_declares_grant_empty(template));
    }

    #[test]
    fn the_desk_grant_is_declared_only_by_its_exact_token() {
        let plist = |body: &str| {
            format!(
                "<plist><dict><key>CFBundleVersion</key><string>7</string>{body}</dict></plist>"
            )
        };
        assert!(plist_declares_grant_desk(&plist(
            "<key>ATermHandoffGrantDesk</key>\n\t<string>desk1</string>"
        )));
        for undeclared in [
            "",
            "<key>ATermHandoffGrantDesk</key><string></string>",
            "<key>ATermHandoffGrantDesk</key><string>desk2</string>",
            "<key>ATermHandoffGrantDesk</key><string>true</string>",
            "<key>ATermHandoffGrantDesk</key><key>Other</key><string>desk1</string>",
            "<key>ATermHandoffGrantEmpty</key><string>empty1</string>",
            "<key>ATermHandoffGrantEmpty</key><string>desk1</string>",
        ] {
            assert!(
                !plist_declares_grant_desk(&plist(undeclared)),
                "{undeclared:?} declares nothing"
            );
        }
        assert!(!plist_declares_grant_desk("\u{0}bplist00 binary garbage"));
    }

    /// This build rebuilds a lone Workers list, so the bundle it ships in declares it.
    #[test]
    fn the_shipped_info_plist_declares_the_desk_grant() {
        let template = include_str!("../../../apps/aterm-mac/Info.plist");
        assert!(plist_declares_grant_desk(template));
    }
}
