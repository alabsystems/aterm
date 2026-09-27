// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BODY POINTER, host side (the LOADER / BODY split, 2026-09-26): how a
//! shell that is ALREADY RUNNING comes to run this build's integration, and how
//! `status integration_rev=` says which integration each shell runs.
//!
//! WHY (gap 20 of the 2026-09-24 audit): a shell's integration script was fixed
//! at spawn. Only its nonce (`shell_rekey`) and its PATH order were live, so any
//! fix to the marks or other integration behaviour reached only new tabs, and
//! the long-lived agent tabs (2 and 14 days old on the owner's Mac) kept the old
//! script with no status word flagging them.
//!
//! THE SPLIT (aterm-shell-integration, "THE LOADER" in the zsh script): each
//! zsh/bash/fish script is a LOADER around one BODY. The body is also written
//! alone into the script folder, which is named by the SHA-256 of everything in
//! it, so a folder's 16-hex address names one body exactly. At spawn the host
//! exports [`POINTER_VAR`] = `<control dir>/integration/<sid>` to every
//! zsh/bash/fish it integrates; the loader captures and scrubs it and checks the
//! file at every prompt (one fork-free `[[ -f ]]`). A successor that adopts such
//! a shell across an update writes ITS OWN folder's address there
//! ([`hand_over`]: the folder installed first, then the file `0600`, created
//! exclusively, never through a symlink, in the `0700` control dir — the re-key
//! channel's discipline, `shell_rekey::deliver`) — and the shell sources that
//! folder's body at its next prompt. The loader takes a body ONLY from
//! `<root>/<address>/`, `<root>` being the folder it was itself loaded from, and
//! only a 16-hex address; nothing a peer can write names a file it sources.
//!
//! Under an OVERLAP handoff the write waits for the update's Commit
//! (`shell_rekey::Deferred`, which holds re-keys for the same reason): an
//! abandoned attempt hands its sessions back to the old process, and a pointer
//! written for the candidate would have moved the shell onto the candidate's
//! body.
//!
//! WHAT THE HOST SEES: the body signs `633;P;AtermIntegration=<address>` before
//! every 133;A; the engine keeps the last SIGNED one
//! (`Terminal::shell_integration_rev`) and the seamless carry takes it across an
//! update. [`status_word`] reads it against this build's address.
//!
//! A shell spawned BEFORE loaders has no pointer to read and signs no revision.
//! Nothing reaches it by itself — its record says so ([`SessionRecord::loader`]
//! absent) and `status` says `frozen` — until the live agent upgrade types its
//! relaunch line there: `shell_rekey::typed` then also hands it this build's
//! folder and its pointer path, and the line sources the loader
//! (`aterm_shell_integration::typed_rekey_with_loader`).
//!
//! [`SessionRecord::loader`]: crate::session_store::SessionRecord::loader

use std::path::{Path, PathBuf};

use aterm_core::terminal::ShellIntegrationPosture;
use aterm_session::SessionId;

/// The environment variable a fresh shell's loader reads its pointer path from.
pub(crate) const POINTER_VAR: &str = aterm_core::shell_integration::BODY_POINTER_VAR;

/// The directory the pointer files live in: `<control dir>/integration`,
/// created `0700` (the control dir's own discipline). `None` when there is no
/// private control dir to put it in — no pointer is offered then.
fn dir() -> Option<PathBuf> {
    let dir = crate::control_auth::socket_dir()?.join("integration");
    crate::control_auth::ensure_private_dir(&dir).ok()?;
    Some(dir)
}

/// The pointer file of session `sid`, in `dir`. The sid is server-minted
/// (`s-<hex>`); anything else is refused, so no carried value can steer a write
/// out of the directory.
fn path_in(dir: &Path, sid: &SessionId) -> Option<PathBuf> {
    let hex = sid.as_str().strip_prefix("s-")?;
    (!hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| dir.join(sid.as_str()))
}

/// The pointer file of session `sid` in this user's control dir.
pub(crate) fn path_for(sid: &SessionId) -> Option<PathBuf> {
    path_in(&dir()?, sid)
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

/// POINT an adopted shell at this build's body, first half: this build's
/// script folder installed (or verified byte for byte), and the pointer that
/// names it, ready to write to `path` — or `None`, and why in the log, when the
/// folder cannot be made sure of (a shell is never pointed at a folder that is
/// not this build's). `carried` is the revision the shell last signed, carried
/// across the update: a shell that already runs this build's body needs no
/// pointer. File work, so the caller holds no engine lock.
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

/// [`pending`], written now — or, under an overlap handoff, held for the
/// update's Commit (`deferred`).
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
/// - `current` — the shell's last SIGNED revision is this build's.
/// - `stale:<rev>` — it signed another build's (in practice an older one: a
///   successor points an adopted shell at its own body, which the shell takes at
///   its next prompt, and this word then reads `current`).
/// - `frozen` — a shell whose integration predates loaders: adopted from a record
///   with no loader, signing marks (`integration=on`/`degraded`) but no revision.
///   Nothing reaches it by itself; a new tab does, and so does the live agent
///   upgrade's relaunch line.
/// - `-` — no revision to say: no integrated shell, one that has not reached its
///   first prompt yet, or the terminal lock was contended (`engine` is `None`).
///
/// `engine` is the shell's signed revision and its integration posture, read
/// under the one terminal guard `status` takes.
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
        // A signed revision wins over the carried mark: a frozen shell that took
        // the typed upgrade signs one.
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

    #[test]
    fn a_pointer_file_is_named_only_for_a_server_minted_sid() {
        let dir = Path::new("/c/integration");
        let sid = SessionId::new("s-09035e075d6ee9388965".to_string());
        assert_eq!(path_in(dir, &sid), Some(dir.join("s-09035e075d6ee9388965")));
        for bad in ["../x", "s-", "s-zz", "s-12/../../etc", "abc"] {
            assert_eq!(
                path_in(dir, &SessionId::new(bad.to_string())),
                None,
                "{bad}"
            );
        }
    }

    /// A shell that already signs this build's revision is not pointed anywhere;
    /// any other — an older build's, or none carried — is pointed at this
    /// build's folder, ONLY once that folder is made sure of: an installer that
    /// fails, or hands back a folder not named by this build's address, points
    /// nothing. The pointer lands as the re-key channel writes a key: the
    /// address and its newline, `0600`.
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
