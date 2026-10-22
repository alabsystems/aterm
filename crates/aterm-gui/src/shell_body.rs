// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The body pointer, host side: how an already-running shell comes to run this build's
//! integration, and how `status integration_rev=` says which integration each shell runs.
//!
//! Each zsh/bash/fish integration script is a loader around one body
//! (aterm-shell-integration). The body is also written alone into the script folder, named
//! by a SHA-256 over its contents, so a folder's 16-hex address names one body. At spawn the
//! host exports [`POINTER_VAR`] = `<control dir>/integration/<sid>`; the loader captures and
//! scrubs it and checks the file at every prompt (one fork-free `[[ -f ]]`). A successor
//! that adopts the shell across an update writes its own folder's address there
//! ([`hand_over`]: the folder installed first, then the file `0600`, renamed into place
//! whole, never through a symlink, in the `0700` control dir, as `shell_rekey::deliver`
//! does), and the shell sources that body at its next prompt. The loader takes a body only
//! from `<root>/<address>/`, `<root>` being its own folder, and only for a 16-hex address,
//! so nothing a peer can write names a file it sources.
//!
//! Under an overlap handoff the write waits for the update's Commit
//! (`shell_rekey::Deferred`): an abandoned attempt hands its sessions back to the old
//! process, and a pointer written for the candidate would move the shell onto its body.
//!
//! The body signs `633;P;AtermIntegration=<address>` before every 133;A; the engine keeps
//! the last signed one (`Terminal::shell_integration_rev`), the seamless carry takes it
//! across an update, and [`status_word`] reads it against this build's address.
//!
//! A shell spawned before loaders has no pointer and signs no revision: its record says so
//! ([`SessionRecord::loader`] false) and `status` says `frozen`, until the live agent
//! upgrade types its relaunch line, to which `shell_rekey::typed` also hands this build's
//! folder and pointer path so the line sources the loader
//! (`aterm_shell_integration::typed_rekey_with_loader`). The same line reaches a fish loader
//! whose prompt hook never runs (one that predates the fish prompt event, under a prompt
//! its config.fish defines): it signs no revision though its record has a loader, so
//! `typed` asks the revision the engine holds, never the record.
//!
//! [`SessionRecord::loader`]: crate::session_store::SessionRecord::loader

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

/// Remove session `sid`'s pointer, if one still waits there (the session closed
/// before its shell reached a prompt). Best effort.
pub(crate) fn discard(sid: &SessionId) {
    if let Some(path) = path_for(sid) {
        let _ = std::fs::remove_file(path);
    }
}

/// This build's body revision: its script folder's address.
pub(crate) fn own_rev() -> &'static str {
    aterm_core::shell_integration::script_set_address()
}

/// First half of pointing an adopted shell at this build's body: install (or verify byte
/// for byte) this build's script folder and return the pointer naming it, ready to write
/// to `path`. `None` when the shell already signed this build's revision (`carried`) or
/// the folder cannot be verified (logged). Does file I/O: the caller holds no engine lock.
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

/// [`pending`], written now or, under an overlap handoff, held for the update's Commit
/// (`deferred`).
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

/// `status integration_rev=`: which integration body the session's shell runs,
/// against the one this build ships.
///
/// - `current`: the shell's last signed revision is this build's.
/// - `stale:<rev>`: it signed another build's (a successor points an adopted shell at its
///   own body, which the shell takes at its next prompt).
/// - `frozen`: the integration predates loaders (adopted from a record with no loader,
///   signing marks but no revision). Only a new tab or the live agent upgrade reaches it.
/// - `-`: no integrated shell, no first prompt yet, or the terminal lock was contended
///   (`engine` is `None`).
///
/// `engine` is the shell's signed revision and integration posture, read under the one
/// terminal guard `status` takes.
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
        // A signed revision wins over the frozen mark: a frozen shell that took the typed
        // upgrade signs one.
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
