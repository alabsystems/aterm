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

/// Reads ANG5 histories in the legacy issuer journal, not independent custody.
pub const NATIVE_LEGACY_RECIPIENT_RECOVERY_TOKEN: &str = "recipients5";

/// Reads independently enrolled and bound ANG5 histories and requires recipient retirement.
pub const NATIVE_RECIPIENT_RECOVERY_TOKEN: &str = "bound-recipients5";

/// The recipient-history protocol an `Info.plist` declares, not writer admission.
#[must_use]
pub fn plist_recipient_recovery(text: &str) -> super::NativeRecipientRecovery {
    Capabilities::from_plist(text).recipient_recovery
}

/// Declarations from one immutable read of a candidate's sealed `Info.plist`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Capabilities {
    pub grant_chunks: bool,
    pub grant_empty: bool,
    pub grant_desk: bool,
    pub recipient_recovery: super::NativeRecipientRecovery,
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
    if write_error_is_passing(error) {
        ParentWrite::Passing(message)
    } else {
        ParentWrite::NotWritable(message)
    }
}

/// Whether a native write failed because of temporary volume or process resources.
pub(crate) fn write_error_is_passing(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENOSPC | libc::EDQUOT | libc::EIO | libc::EINTR)
    ) || crate::verify::io_error_is_passing(error)
}

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;
