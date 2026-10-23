// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-side shell integration body pointer and `status integration_rev=` reporting.

use std::path::{Path, PathBuf};

use aterm_core::terminal::ShellIntegrationPosture;
use aterm_session::SessionId;

/// The environment variable a fresh shell's loader reads its pointer path from.
pub(crate) const POINTER_VAR: &str = aterm_core::shell_integration::BODY_POINTER_VAR;

/// The directory the pointer files live in: `<control dir>/integration`,
/// created `0700`. `None` when there is no private control dir; no pointer is
/// offered then.
fn dir() -> Option<PathBuf> {
    crate::control_auth::private_subdir("integration")
}

/// The pointer file of session `sid` in this user's control dir.
pub(crate) fn path_for(sid: &SessionId) -> Option<PathBuf> {
    crate::control_auth::sid_file_in(&dir()?, sid)
}

/// Best-effort removal of session `sid`'s body pointer file.
pub(crate) fn discard(sid: &SessionId) {
    if let Some(path) = path_for(sid) {
        let _ = std::fs::remove_file(path);
    }
}

/// This build's body revision: its script folder's address.
pub(crate) fn own_rev() -> &'static str {
    aterm_core::shell_integration::script_set_address()
}

/// Installs or verifies this build's script folder and returns a pending pointer for `path`.
pub(crate) fn pending(
    path: &Path,
    id: u64,
    carried: Option<&str>,
) -> Option<crate::shell_rekey::Pending> {
    pending_with(
        path,
        id,
        carried,
        aterm_core::shell_integration::ensure_script_set,
    )
}

/// [`pending`] with the folder's installer injected (the product's is
/// `ensure_script_set`, which writes the user's cache; the tests' write none).
fn pending_with(
    path: &Path,
    id: u64,
    carried: Option<&str>,
    ensure: impl FnOnce() -> std::io::Result<PathBuf>,
) -> Option<crate::shell_rekey::Pending> {
    if carried == Some(own_rev()) {
        return None;
    }
    match ensure() {
        Ok(folder) if folder.ends_with(own_rev()) => {
            Some(crate::shell_rekey::Pending::body(path, own_rev(), id))
        }
        Ok(folder) => {
            aterm_log::warn!(
                "adopted session {id}: this build's shell-integration folder is not named by its \
                 address ({}); its shell keeps the integration it runs (status integration_rev=)",
                folder.display()
            );
            None
        }
        Err(e) => {
            aterm_log::warn!(
                "adopted session {id}: could not install this build's shell-integration scripts \
                 ({e}); its shell keeps the integration it runs (status integration_rev=)"
            );
            None
        }
    }
}

/// Points an adopted shell at this build's body now, or defers until update commit.
pub(crate) fn hand_over(
    path: &Path,
    id: u64,
    carried: Option<&str>,
    deferred: Option<&crate::shell_rekey::Deferred>,
) {
    let Some(pending) = pending(path, id, carried) else {
        return;
    };
    match deferred {
        Some(queue) => queue.hold(pending),
        None => {
            pending.deliver();
        }
    }
}

/// Returns the `status integration_rev=` word comparing the shell's revision against this build.
pub(crate) fn status_word(
    engine: Option<(Option<&str>, ShellIntegrationPosture)>,
    frozen: bool,
) -> String {
    status_word_against(engine, frozen, own_rev())
}

/// [`status_word`] against `own`, the revision the host ships.
fn status_word_against(
    engine: Option<(Option<&str>, ShellIntegrationPosture)>,
    frozen: bool,
    own: &str,
) -> String {
    let Some((rev, posture)) = engine else {
        return "-".to_string();
    };
    match rev {
        Some(rev) if rev == own => "current".to_string(),
        Some(rev) => format!("stale:{rev}"),
        None if frozen && posture != ShellIntegrationPosture::Off => "frozen".to_string(),
        None => "-".to_string(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const OWN: &str = "0123456789abcdef";
    const OLD: &str = "fedcba9876543210";

    /// Every arm of the word, from the facts `status` reads.
    #[test]
    fn the_status_word_says_which_body_the_shell_runs() {
        use ShellIntegrationPosture::{Degraded, Off, On};
        let word = |rev, posture, frozen| status_word_against(Some((rev, posture)), frozen, OWN);
        assert_eq!(word(Some(OWN), On, false), "current");
        assert_eq!(word(Some(OLD), On, false), format!("stale:{OLD}"));
        assert_eq!(word(Some(OWN), On, true), "current");
        assert_eq!(word(None, On, true), "frozen");
        assert_eq!(word(None, Degraded, true), "frozen");
        assert_eq!(word(None, Off, true), "-", "nothing integrated to freeze");
        assert_eq!(
            word(None, On, false),
            "-",
            "a loader shell before its first prompt"
        );
        assert_eq!(
            status_word_against(None, true, OWN),
            "-",
            "the lock was contended"
        );
    }

    /// A shell already on this build's revision is not pointed anywhere; any other is
    /// pointed only once this build's folder is verified (a failing installer or a folder
    /// not named by this build's address points nothing). The pointer is the address and a
    /// newline, `0600`.
    #[test]
    fn an_adopted_shell_is_pointed_at_this_builds_body_unless_it_runs_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::Builder::new()
            .prefix("body-pointer")
            .tempdir()
            .expect("scratch");
        let ours = scratch.path().join(own_rev());
        let installed = || Ok(ours.clone());
        let pointer = scratch.path().join("s-ab");
        assert!(pending_with(&pointer, 1, Some(own_rev()), installed).is_none());
        for carried in [None, Some(OLD)] {
            let pending = pending_with(&pointer, 1, carried, installed).expect("pointed");
            assert!(pending.deliver());
            assert_eq!(
                std::fs::read_to_string(&pointer).unwrap(),
                format!("{}\n", own_rev())
            );
            let mode = std::fs::metadata(&pointer).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            std::fs::remove_file(&pointer).unwrap();
        }
        let failed = || Err(std::io::Error::other("disk full"));
        assert!(pending_with(&pointer, 1, Some(OLD), failed).is_none());
        let foreign = || Ok(scratch.path().join(OLD));
        assert!(pending_with(&pointer, 1, Some(OLD), foreign).is_none());
        assert!(!pointer.exists(), "nothing was pointed");
    }
}
