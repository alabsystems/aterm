// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The re-key channel, host side.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use aterm_session::SessionId;

/// The directory the re-key files live in: `<control dir>/rekey`, created
/// `0700`.
fn dir() -> Option<PathBuf> {
    crate::control_auth::private_subdir("rekey")
}

/// The re-key file of session `sid` in this user's control dir.
pub(crate) fn path_for(sid: &SessionId) -> Option<PathBuf> {
    crate::control_auth::sid_file_in(&dir()?, sid)
}

/// Write `key_hex` (a 64-hex nonce, or a body address) to `path` for the shell to take, whole.
pub(crate) fn deliver(path: &Path, key_hex: &str) -> std::io::Result<()> {
    static TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let temp = path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&temp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let written = options
        .open(&temp)
        .and_then(|mut file| file.write_all(format!("{key_hex}\n").as_bytes()))
        .and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Remove session `sid`'s pending re-key file and any companion `<sid>.trap` file
/// (`aterm_shell_integration::DEBUG_TRAP_SUFFIX`).
pub(crate) fn discard(sid: &SessionId) {
    if let Some(path) = path_for(sid) {
        let mut trap = path.clone().into_os_string();
        trap.push(aterm_core::shell_integration::DEBUG_TRAP_SUFFIX);
        let _ = std::fs::remove_file(trap);
        let _ = std::fs::remove_file(path);
    }
}

/// Re-key an adopted session whose marks are dropped, first half.
#[cfg(any(unix, windows))]
pub(crate) fn hand_over(path: &Path, id: u64) -> Option<[u8; 32]> {
    let (pending, raw) = mint(path, id);
    pending.deliver().then_some(raw)
}

/// A file an adopted shell will read at its next prompt, not written yet.
pub(crate) struct Pending {
    path: PathBuf,
    /// A 64-hex key ([`What::Rekey`]) or a 16-hex folder address ([`What::Body`]).
    content: String,
    id: u64,
    what: What,
}

/// What a [`Pending`] hands the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum What {
    Rekey,
    Body,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending")
            .field("path", &self.path)
            .field("id", &self.id)
            .field("what", &self.what)
            .finish_non_exhaustive()
    }
}

impl Pending {
    /// Body pointer naming `rev` (a script folder's address) for the shell reading `path`.
    pub(crate) fn body(path: &Path, rev: &str, id: u64) -> Self {
        Self {
            path: path.to_path_buf(),
            content: rev.to_owned(),
            id,
            what: What::Body,
        }
    }

    /// Write the file for the shell to take ([`deliver`]), saying either way.
    pub(crate) fn deliver(self) -> bool {
        let id = self.id;
        match (deliver(&self.path, &self.content), self.what) {
            (Ok(()), What::Rekey) => {
                aterm_log::info!(
                    "adopted session {id}: its shell-integration nonce did not cross the update; \
                     re-keyed through its channel — its marks resume at its next prompt"
                );
                true
            }
            (Ok(()), What::Body) => {
                aterm_log::info!(
                    "adopted session {id}: pointed its shell at this build's shell integration \
                     ({}); it takes it at its next prompt (status integration_rev=)",
                    self.content
                );
                true
            }
            (Err(e), What::Rekey) => {
                aterm_log::warn!(
                    "adopted session {id}: could not write its re-key file ({e}); its OSC \
                     133/633 marks stay dropped (status integration=degraded)"
                );
                false
            }
            (Err(e), What::Body) => {
                aterm_log::warn!(
                    "adopted session {id}: could not point its shell at this build's shell \
                     integration ({e}); it keeps the one it runs (status integration_rev=)"
                );
                false
            }
        }
    }
}

/// Mint a fresh nonce for the shell reading `path`.
#[cfg(any(unix, windows))]
pub(crate) fn mint(path: &Path, id: u64) -> (Pending, [u8; 32]) {
    let (raw, key_hex) = aterm_core::shell_integration::generate_nonce().into_parts();
    let pending = Pending {
        path: path.to_path_buf(),
        content: key_hex,
        id,
        what: What::Rekey,
    };
    (pending, raw)
}

/// The writes of an adoption not yet committed.
#[derive(Debug)]
pub(crate) struct Deferred {
    /// `Some`: holding until Commit. `None`: released; a key handed over from
    /// now on is written at once.
    held: std::sync::Mutex<Option<Vec<Pending>>>,
}

impl Deferred {
    /// A queue holding every key until [`Self::release`].
    pub(crate) const fn holding() -> Self {
        Self {
            held: std::sync::Mutex::new(Some(Vec::new())),
        }
    }

    /// Hold `pending` for the Commit.
    pub(crate) fn hold(&self, pending: Pending) {
        let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
        match held.as_mut() {
            Some(queue) => queue.push(pending),
            None => {
                drop(held);
                pending.deliver();
            }
        }
    }

    /// Whether [`Self::release`] has run (the incoming handoff committed).
    #[cfg(all(test, unix))]
    pub(crate) fn released(&self) -> bool {
        self.held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_none()
    }

    /// Session `sid` closed.
    pub(crate) fn withdraw(&self, sid: &SessionId) -> bool {
        let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
        let Some(queue) = held.as_mut() else {
            return false;
        };
        queue
            .retain(|pending| pending.path.file_name() != Some(std::ffi::OsStr::new(sid.as_str())));
        true
    }

    /// The Commit: write every held file.
    #[cfg(unix)]
    pub(crate) fn release(&self) {
        let queue = self
            .held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .unwrap_or_default();
        for pending in queue {
            pending.deliver();
        }
    }
}

/// Session `sid` closed.
pub(crate) fn forget_typed(sid: &SessionId) {
    #[cfg(unix)]
    typed::forget(sid);
    #[cfg(not(unix))]
    let _ = sid;
}

/// The typed re-key, the window's half.
#[cfg(unix)]
pub(crate) mod typed {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use aterm_core::terminal::{ShellIntegrationPosture, Terminal};

    use super::{Path, PathBuf, SessionId};
    use super::{deliver, dir};

    /// How long a typed re-key file waits before the window settles it ([`settle`]).
    pub(crate) const TTL: Duration = Duration::from_secs(120);

    /// Why a typed re-key was not issued.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Refusal {
        /// `integration=on`.
        Healthy,
        /// `integration=off`.
        Off,
        /// The re-key channel's own key waits in `<sid>` for this shell's next prompt.
        Pending,
        /// No private control dir to put the file in, or a sid that names none.
        NoDir,
        /// The file could not be written.
        Write(std::io::ErrorKind),
        /// No expiry could be started to settle it later, so it was taken back at once.
        NoExpiry(std::io::ErrorKind),
    }

    impl Refusal {
        /// The reply line.
        pub(crate) fn reply(&self) -> String {
            match self {
                Refusal::Healthy => {
                    "ERR rekey integration=on (a working key is never moved)\n".into()
                }
                Refusal::Off => "ERR rekey integration=off (nothing to heal)\n".into(),
                Refusal::Pending => {
                    "ERR rekey pending (the shell's own channel key waits for its next prompt)\n"
                        .into()
                }
                Refusal::NoDir => "ERR rekey no private control dir\n".into(),
                Refusal::Write(kind) => {
                    format!("ERR rekey could not write the key file ({kind})\n")
                }
                Refusal::NoExpiry(kind) => {
                    format!("ERR rekey could not start its expiry ({kind}); taken back\n")
                }
            }
        }
    }

    /// How a typed re-key was settled ([`settle`]).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Settled {
        /// No typed re-key of this session was waiting.
        None,
        /// Its file was gone.
        Taken,
        /// Its file was still there.
        Withdrawn,
    }

    impl Settled {
        /// The reply word.
        pub(crate) const fn word(self) -> &'static str {
            match self {
                Settled::None => "none",
                Settled::Taken => "taken",
                Settled::Withdrawn => "withdrawn",
            }
        }
    }

    /// One issued typed re-key: its file, and the key to settle it by (`None`
    /// for an upgrade of a healthy shell, which authorized nothing).
    struct Issued {
        serial: u64,
        path: PathBuf,
        nonce: Option<[u8; 32]>,
    }

    /// The typed upgrade.
    #[derive(Clone, Debug)]
    pub(crate) struct Upgrade {
        /// This build's installed script folder (`ensure_script_set`).
        pub(crate) folder: PathBuf,
        /// The session's body pointer file (`shell_body::path_for`).
        pub(crate) pointer: PathBuf,
    }

    /// What an issue writes, decided from the engine under one lock.
    enum Plan {
        /// A degraded shell: a fresh key, authorized as one it has not taken (and the
        /// upgrade's lines, when it is behind).
        Heal,
        /// A healthy shell that is behind: the key it already signs with, never moved, and
        /// the upgrade's lines.
        UpgradeOnly([u8; 32]),
    }

    /// The typed re-keys issued and not yet settled, by session: at most one
    /// each.
    static ISSUED: Mutex<BTreeMap<String, Issued>> = Mutex::new(BTreeMap::new());

    /// This process's typed re-key numbers: each file's name is its own.
    static SERIAL: AtomicU64 = AtomicU64::new(1);

    /// Serializes `issue_upgrading_in` so concurrent issues on one tab settle the previous key
    /// before recording the new one.
    static ISSUING: Mutex<()> = Mutex::new(());

    /// Whether the shell of `engine` is behind: the last revision it signed is not this
    /// build's.
    fn behind(engine: &Terminal) -> bool {
        engine.shell_integration_rev() != Some(crate::shell_body::own_rev())
    }

    /// Decide what `term`'s shell may be issued and whether the upgrade's lines go with it.
    fn plan(term: &Mutex<Terminal>, upgrading: bool) -> Result<(Plan, bool), Refusal> {
        let engine = crate::term_lock(term);
        let upgrading = upgrading && behind(&engine);
        match engine.shell_integration_posture() {
            ShellIntegrationPosture::Degraded => Ok((Plan::Heal, upgrading)),
            ShellIntegrationPosture::On if upgrading => engine
                .shell_integration_nonce_in_use()
                .map(|kept| (Plan::UpgradeOnly(kept), true))
                .ok_or(Refusal::Healthy),
            ShellIntegrationPosture::On => Err(Refusal::Healthy),
            ShellIntegrationPosture::Off => Err(Refusal::Off),
        }
    }

    /// Issue a typed re-key for session `sid`, whose engine is `term`.
    pub(crate) fn issue(
        term: &Arc<Mutex<Terminal>>,
        sid: &SessionId,
        upgrade: impl FnOnce() -> Option<Upgrade>,
    ) -> Result<PathBuf, Refusal> {
        let (_, upgrading) = plan(term, true)?;
        let upgrade = upgrading.then(upgrade).flatten();
        let dir = dir().ok_or(Refusal::NoDir)?;
        let (path, serial) = issue_upgrading_in(&dir, term, sid, upgrade.as_ref())?;
        let (engine, session) = (Arc::clone(term), sid.clone());
        let expiry = std::thread::Builder::new()
            .name("rekey-expiry".into())
            .spawn(move || {
                std::thread::sleep(TTL);
                settle_issued(&engine, &session, Some(serial));
            });
        if let Err(e) = expiry {
            settle_issued(term, sid, Some(serial));
            return Err(Refusal::NoExpiry(e.kind()));
        }
        Ok(path)
    }

    /// [`issue`] into `dir`, without the expiry and without an upgrade.
    #[cfg(test)]
    pub(crate) fn issue_in(
        dir: &Path,
        term: &Mutex<Terminal>,
        sid: &SessionId,
    ) -> Result<(PathBuf, u64), Refusal> {
        issue_upgrading_in(dir, term, sid, None)
    }

    /// `issue_in` with the typed upgrade's lines for a shell that is [`behind`].
    pub(crate) fn issue_upgrading_in(
        dir: &Path,
        term: &Mutex<Terminal>,
        sid: &SessionId,
        upgrade: Option<&Upgrade>,
    ) -> Result<(PathBuf, u64), Refusal> {
        let channel = crate::control_auth::sid_file_in(dir, sid).ok_or(Refusal::NoDir)?;
        let _one = ISSUING.lock().unwrap_or_else(|p| p.into_inner());
        settle_issued(term, sid, None);
        let (planned, upgrading) = plan(term, upgrade.is_some())?;
        if channel.exists() {
            return Err(Refusal::Pending);
        }
        sweep_leftovers(dir, sid);
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{}.{serial}", sid.as_str()));
        let (nonce, key_hex, kept) = match planned {
            Plan::Heal => {
                let (raw, hex) = aterm_core::shell_integration::generate_nonce().into_parts();
                (Some(raw), hex, None)
            }
            Plan::UpgradeOnly(in_use) => (
                None,
                aterm_core::shell_integration::hex_encode(&in_use),
                Some(in_use),
            ),
        };
        let content = match upgrade.filter(|_| upgrading) {
            Some(up) => format!(
                "{key_hex}\n{}\n{}",
                up.folder.display(),
                up.pointer.display()
            ),
            None => key_hex,
        };
        deliver(&path, &content).map_err(|e| Refusal::Write(e.kind()))?;
        let refused = {
            let mut engine = crate::term_lock(term);
            match (engine.shell_integration_posture(), nonce, kept) {
                (ShellIntegrationPosture::Degraded, Some(nonce), _) => {
                    engine.authorize_shell_integration_rekey(nonce);
                    None
                }
                (ShellIntegrationPosture::On, None, Some(kept))
                    if engine.shell_integration_nonce_in_use() == Some(kept) =>
                {
                    None
                }
                _ => Some(Refusal::Healthy),
            }
        };
        if let Some(no) = refused {
            let _ = std::fs::remove_file(&path);
            return Err(no);
        }
        {
            let mut issued = ISSUED.lock().unwrap_or_else(|p| p.into_inner());
            issued.insert(
                sid.as_str().to_owned(),
                Issued {
                    serial,
                    path: path.clone(),
                    nonce,
                },
            );
        }
        match (nonce.is_some(), upgrading) {
            (true, false) => aterm_log::info!(
                "session {}: typed re-key issued (its integration was degraded); the relaunch \
                 line typed at its shell's prompt reads it",
                sid.as_str()
            ),
            (true, true) => aterm_log::info!(
                "session {}: typed re-key and integration upgrade issued (its integration was \
                 degraded, and its shell signs no revision of this build's); the relaunch line \
                 typed at its shell's prompt reads them",
                sid.as_str()
            ),
            _ => aterm_log::info!(
                "session {}: typed integration upgrade issued (its shell signs no revision of \
                 this build's); the relaunch line typed at its shell's prompt sources this \
                 build's loader",
                sid.as_str()
            ),
        }
        Ok((path, serial))
    }

    /// Remove `<sid>.<n>` files no live key names: left by a window that exited
    /// before its expiry settled them.
    fn sweep_leftovers(dir: &Path, sid: &SessionId) {
        let prefix = format!("{}.", sid.as_str());
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_str().is_some_and(|n| {
                n.strip_prefix(&prefix).is_some_and(|serial| {
                    !serial.is_empty() && serial.bytes().all(|b| b.is_ascii_digit())
                })
            }) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Settle the typed re-key waiting for session `sid` (`rekey withdraw`) by its file.
    pub(crate) fn settle(term: &Mutex<Terminal>, sid: &SessionId) -> Settled {
        settle_issued(term, sid, None)
    }

    /// [`settle`] for the key numbered `serial` only (`None` settles whichever is waiting).
    pub(crate) fn settle_issued(
        term: &Mutex<Terminal>,
        sid: &SessionId,
        serial: Option<u64>,
    ) -> Settled {
        let issued = {
            let mut issued = ISSUED.lock().unwrap_or_else(|p| p.into_inner());
            match issued.get(sid.as_str()) {
                Some(i) if serial.is_none_or(|s| s == i.serial) => issued.remove(sid.as_str()),
                _ => None,
            }
        };
        let Some(issued) = issued else {
            return Settled::None;
        };
        let unread = match std::fs::remove_file(&issued.path) {
            Ok(()) => true,
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        };
        if let Some(nonce) = issued.nonce {
            let mut engine = crate::term_lock(term);
            if unread {
                engine.withdraw_shell_integration_rekey(&nonce);
            } else {
                engine.settle_shell_integration_rekey(&nonce);
            }
        }
        if unread {
            aterm_log::info!(
                "session {}: typed re-key taken back — its line never read it",
                sid.as_str()
            );
            Settled::Withdrawn
        } else {
            Settled::Taken
        }
    }

    /// Session `sid` closed.
    pub(crate) fn forget(sid: &SessionId) {
        let issued = {
            let mut issued = ISSUED.lock().unwrap_or_else(|p| p.into_inner());
            issued.remove(sid.as_str())
        };
        if let Some(issued) = issued {
            let _ = std::fs::remove_file(issued.path);
        }
    }
}

#[cfg(all(test, unix))]
#[path = "shell_rekey_tests.rs"]
mod tests;
