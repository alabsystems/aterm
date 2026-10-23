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
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
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

/// Remove session `sid`'s pending re-key file and any companion `<sid>.trap` file (`aterm_shell_integration::DEBUG_TRAP_SUFFIX`).
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
    /// The body pointer naming `rev` (a script folder's address) for the shell that reads `path` (`shell_body::pending`).
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
    pub(crate) fn holding() -> Self {
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

    /// How long a typed re-key's file waits for the line that reads it before the window settles it ([`settle`]).
    pub(crate) const TTL: Duration = Duration::from_secs(120);

    /// Why a typed re-key was not issued.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Refusal {
        /// `integration=on`.
        Healthy,
        /// `integration=off`.
        Off,
        /// The re-key channel's own key waits in `<sid>` for this shell's next prompt, and a typed one would race it.
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
        pub(crate) fn word(self) -> &'static str {
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

    /// One issue at a time.
    static ISSUING: Mutex<()> = Mutex::new(());

    /// Whether the shell of `engine` is behind: the last revision it signed is not this
    /// build's.
    fn behind(engine: &Terminal) -> bool {
        engine.shell_integration_rev() != Some(crate::shell_body::own_rev())
    }

    /// What `term`'s shell may be issued, and whether the upgrade's lines go with it
    /// (`upgrading` and the shell is [`behind`]): a heal when degraded, an upgrade-only file
    /// when healthy and upgrading, else why not.
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

    /// [`issue_in`] with the typed upgrade's lines for a shell that is [`behind`].
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

    /// Settle the typed re-key waiting for session `sid` (the verb's `rekey withdraw`) by its file.
    pub(crate) fn settle(term: &Mutex<Terminal>, sid: &SessionId) -> Settled {
        settle_issued(term, sid, None)
    }

    /// [`settle`] for the key numbered `serial` only (`None`: whichever is waiting), the expiry's form, which must never settle a later key.
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
mod tests {
    use super::*;
    use aterm_core::terminal::{ShellIntegrationPosture, Terminal};
    use std::sync::Mutex;

    fn scratch(tag: &str) -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix(tag)
            .tempdir()
            .expect("scratch")
    }

    /// The key lands 0600 with its newline, replacing a stale key, and a planted
    /// symlink at the path is replaced without writing through it.
    #[test]
    fn delivery_is_private_exclusive_and_never_through_a_symlink() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("rk-deliver");
        let names = || {
            let mut names: Vec<String> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        let path = dir.path().join("s-ab");
        std::fs::write(&path, "stale\n").unwrap();
        deliver(&path, "aa").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "aa\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(names(), ["s-ab"]);

        let victim = dir.path().join("victim");
        std::fs::write(&victim, "original").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        deliver(&path, "bb").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_file()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "bb\n");

        let occupied = dir.path().join("s-cd");
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("x"), "").unwrap();
        assert!(deliver(&occupied, "cc").is_err());
        assert_eq!(names(), ["s-ab", "s-cd", "victim"]);
    }

    /// A command cycle (prompt, command line, start) signed with `id`
    /// (`";id=<hex>"`, or `""` for none); `status detail=` reads the running
    /// command from it, so it shows whether marks reach the engine.
    fn cycle(id: &str) -> String {
        format!(
            "\x1b]133;A{id}\x07$ \x1b]633;E;claude --resume{id}\x07\x1b]133;B{id}\x07\
             claude --resume\n\x1b]133;C{id}\x07"
        )
    }

    /// The carry of a parent that lost the shell's nonce: the requirement
    /// crosses in `modes`, the nonce does not.
    fn carry_without_nonce() -> aterm_core::terminal::TerminalCheckpoint {
        let mut parent = Terminal::new(24, 80);
        parent.set_require_shell_integration_nonce(true);
        let carry = parent.checkpoint_carry(0).expect("Ground");
        assert_eq!(carry.shell_integration_nonce, None);
        carry
    }

    /// The key the adoption wrote, as the shell's `__aterm_rekey_check` takes it.
    fn key_in(path: &Path) -> String {
        let key = std::fs::read_to_string(path).expect("the key file");
        let key = key.trim_end().to_string();
        assert_eq!(key.len(), 64, "{key}");
        key
    }

    /// A session with the channel, adopted with its nonce requirement and no nonce (or no
    /// screen), gets a key in its file from `spawn::hydrate_adopted_engine`.
    #[test]
    fn the_adoption_rekeys_a_degraded_shell_that_has_the_channel() {
        let dir = scratch("rk-heal");
        for (label, carry) in [
            ("carried screen", Some(carry_without_nonce())),
            ("no screen", None),
        ] {
            let path = dir.path().join("s-ab");
            let term = std::sync::Mutex::new(Terminal::new(24, 80));
            crate::spawn::hydrate_adopted_engine(
                &term,
                carry.as_ref(),
                None,
                Some(&path),
                None,
                1,
                &mut crate::handoff_history::AdoptedHistory::default(),
            );
            let key = key_in(&path);
            let mut engine = term.lock().unwrap();
            assert_eq!(
                engine.shell_integration_posture(),
                ShellIntegrationPosture::Degraded,
                "{label}: handed over, not yet taken"
            );
            engine.process(cycle("").as_bytes());
            engine.process(cycle(&format!(";id={}", "1".repeat(64))).as_bytes());
            assert_eq!(
                crate::session_status::executing_detail(&engine),
                None,
                "{label}: unsigned and old-key marks stay dropped"
            );
            assert_eq!(
                engine.shell_integration_posture(),
                ShellIntegrationPosture::Degraded,
                "{label}"
            );
            engine.process(cycle(&format!(";id={key}")).as_bytes());
            assert_eq!(
                crate::session_status::executing_detail(&engine).as_deref(),
                Some("claude"),
                "{label}"
            );
            assert_eq!(
                engine.shell_integration_posture(),
                ShellIntegrationPosture::On,
                "{label}: the first signed mark is the shell taking the key"
            );
            drop(engine);
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Under an overlap handoff the old key stays in the file through an abandoned
    /// attempt, and the Commit writes the new one, which the engine (authorized at
    /// adoption) verifies.
    #[test]
    fn an_uncommitted_adoption_holds_its_key_until_the_commit() {
        let dir = scratch("rk-deferred");
        let path = dir.path().join("s-ab");
        let old_key = "7".repeat(64);
        deliver(&path, &old_key).expect("the old process's own waiting key");
        let term = std::sync::Mutex::new(Terminal::new(24, 80));
        let queue = Deferred::holding();
        crate::spawn::hydrate_adopted_engine(
            &term,
            Some(&carry_without_nonce()),
            None,
            Some(&path),
            Some(&queue),
            1,
            &mut crate::handoff_history::AdoptedHistory::default(),
        );
        assert_eq!(key_in(&path), old_key, "held: the old key still waits");
        drop(queue);
        assert_eq!(key_in(&path), old_key, "abandoned: nothing was written");
        let term = std::sync::Mutex::new(Terminal::new(24, 80));
        let queue = Deferred::holding();
        crate::spawn::hydrate_adopted_engine(
            &term,
            Some(&carry_without_nonce()),
            None,
            Some(&path),
            Some(&queue),
            1,
            &mut crate::handoff_history::AdoptedHistory::default(),
        );
        assert_eq!(key_in(&path), old_key, "held until the Commit");
        queue.release();
        let key = key_in(&path);
        assert_ne!(key, old_key, "the Commit wrote the successor's key");
        let mut engine = term.lock().unwrap();
        engine.process(cycle(&format!(";id={key}")).as_bytes());
        assert_eq!(
            crate::session_status::executing_detail(&engine).as_deref(),
            Some("claude"),
            "authorized at adoption, taken after the Commit"
        );
        drop(engine);
        let later = dir.path().join("s-cd");
        queue.hold(mint(&later, 2).0);
        assert!(later.exists(), "released: written at once");
    }

    /// Negative controls: a shell without the channel gets no file and stays
    /// `degraded`; a channel whose file cannot be written authorizes nothing and
    /// stays `degraded`; a session whose nonce did cross is `on` with nothing
    /// written.
    #[test]
    fn the_adoption_writes_no_key_where_none_is_wanted_or_deliverable() {
        let dir = scratch("rk-controls");
        let path = dir.path().join("s-ab");

        let term = std::sync::Mutex::new(Terminal::new(24, 80));
        crate::spawn::hydrate_adopted_engine(
            &term,
            Some(&carry_without_nonce()),
            None,
            None,
            None,
            1,
            &mut crate::handoff_history::AdoptedHistory::default(),
        );
        assert!(!path.exists());
        assert_eq!(
            term.lock().unwrap().shell_integration_posture(),
            ShellIntegrationPosture::Degraded,
            "no channel: degraded, and says so"
        );

        let unreachable = dir.path().join("missing-dir").join("s-ab");
        let term = std::sync::Mutex::new(Terminal::new(24, 80));
        crate::spawn::hydrate_adopted_engine(
            &term,
            Some(&carry_without_nonce()),
            None,
            Some(&unreachable),
            None,
            1,
            &mut crate::handoff_history::AdoptedHistory::default(),
        );
        assert_eq!(
            term.lock().unwrap().shell_integration_posture(),
            ShellIntegrationPosture::Degraded,
            "no key delivered, so none authorized"
        );

        let mut parent = Terminal::new(24, 80);
        parent.set_require_shell_integration_nonce(true);
        parent.authorize_shell_integration([7; 32]);
        let carried = parent.checkpoint_carry(0).expect("Ground");
        let term = std::sync::Mutex::new(Terminal::new(24, 80));
        crate::spawn::hydrate_adopted_engine(
            &term,
            Some(&carried),
            None,
            Some(&path),
            None,
            1,
            &mut crate::handoff_history::AdoptedHistory::default(),
        );
        assert!(!path.exists(), "a carried nonce needs no re-key");
        assert_eq!(
            term.lock().unwrap().shell_integration_posture(),
            ShellIntegrationPosture::On
        );
    }

    /// An adopted shell whose nonce did not cross the update, from a build with
    /// no re-key channel: `integration=degraded`.
    fn lost_nonce_engine() -> Mutex<Terminal> {
        let mut t = Terminal::new(24, 80);
        t.restore_checkpoint(&carry_without_nonce());
        assert_eq!(
            t.shell_integration_posture(),
            ShellIntegrationPosture::Degraded
        );
        Mutex::new(t)
    }

    fn sid(n: u32) -> SessionId {
        // Distinct per test: the issued-key table is process-wide.
        SessionId::new(format!("s-7e{n:04x}"))
    }

    /// A degraded tab gets a private one-use `<sid>.<n>` file beside the channel's `<sid>`,
    /// holding a fresh key.
    #[test]
    fn a_typed_rekey_heals_a_degraded_tab_from_its_first_signed_mark() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("rk-typed");
        let (term, sid) = (lost_nonce_engine(), sid(1));
        let (path, serial) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
        assert_eq!(path, dir.path().join(format!("{}.{serial}", sid.as_str())));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let key = key_in(&path);
        let mut engine = term.lock().unwrap();
        assert_eq!(
            engine.shell_integration_posture(),
            ShellIntegrationPosture::Degraded,
            "issued, not yet taken"
        );
        engine.process(b"\x1b]133;D;0\x07");
        engine.process(cycle("").as_bytes());
        engine.process(cycle(&format!(";id={}", "1".repeat(64))).as_bytes());
        assert_eq!(crate::session_status::executing_detail(&engine), None);
        assert_eq!(
            engine.shell_integration_posture(),
            ShellIntegrationPosture::Degraded
        );
        engine.process(cycle(&format!(";id={key}")).as_bytes());
        assert_eq!(
            crate::session_status::executing_detail(&engine).as_deref(),
            Some("claude")
        );
        assert_eq!(
            engine.shell_integration_posture(),
            ShellIntegrationPosture::On
        );
        drop(engine);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            typed::settle_issued(&term, &sid, None),
            typed::Settled::Taken
        );
    }

    /// A healthy tab is refused (a re-key would move a working shell off its
    /// key), as are one with no integration and one whose channel key already
    /// waits for its next prompt, each with nothing written.
    #[test]
    fn a_typed_rekey_is_refused_where_nothing_is_lost() {
        let dir = scratch("rk-typed-refused");
        let written = || std::fs::read_dir(dir.path()).unwrap().count();

        let mut healthy = Terminal::new(24, 80);
        healthy.set_require_shell_integration_nonce(true);
        healthy.authorize_shell_integration([9; 32]);
        let healthy = Mutex::new(healthy);
        assert_eq!(
            typed::issue_in(dir.path(), &healthy, &sid(2)).err(),
            Some(typed::Refusal::Healthy)
        );
        let off = Mutex::new(Terminal::new(24, 80));
        assert_eq!(
            typed::issue_in(dir.path(), &off, &sid(2)).err(),
            Some(typed::Refusal::Off)
        );
        assert_eq!(written(), 0, "nothing written for either");

        let term = lost_nonce_engine();
        deliver(&dir.path().join(sid(2).as_str()), &"5".repeat(64)).unwrap();
        assert_eq!(
            typed::issue_in(dir.path(), &term, &sid(2)).err(),
            Some(typed::Refusal::Pending)
        );
        assert_eq!(written(), 1, "the channel's key alone");
        assert_eq!(
            term.lock().unwrap().shell_integration_posture(),
            ShellIntegrationPosture::Degraded
        );
        assert_eq!(
            typed::issue_in(dir.path(), &term, &SessionId::new("../x")).err(),
            Some(typed::Refusal::NoDir),
            "a sid naming no file of ours"
        );
    }

    /// A healthy shell that is behind (signing no revision, or an older build's) is issued
    /// the key it already signs with plus this build's folder and pointer path; nothing on
    /// the engine changes.
    #[test]
    fn a_shell_behind_this_build_is_issued_the_upgrade_healthy_or_not() {
        use aterm_core::shell_integration::hex_encode;
        let dir = scratch("rk-typed-upgrade");
        let up = typed::Upgrade {
            folder: PathBuf::from("/c/shell-integration/0123456789abcdef"),
            pointer: PathBuf::from("/c/integration/s-04"),
        };
        let key = [9u8; 32];
        let healthy = |signed: Option<&str>| {
            let mut t = Terminal::new(24, 80);
            t.set_require_shell_integration_nonce(true);
            t.authorize_shell_integration(key);
            if let Some(rev) = signed {
                t.process(
                    format!(
                        "\x1b]633;P;AtermIntegration={rev};id={}\x07",
                        hex_encode(&key)
                    )
                    .as_bytes(),
                );
            }
            Mutex::new(t)
        };
        let lines =
            |key: &str| format!("{key}\n{}\n{}\n", up.folder.display(), up.pointer.display());
        let in_use = |term: &Mutex<Terminal>| {
            let engine = term.lock().unwrap();
            (
                engine.shell_integration_posture(),
                engine.shell_integration_nonce_in_use(),
            )
        };

        for signed in [None, Some("fedcba9876543210")] {
            let term = healthy(signed);
            for read in [false, true] {
                let (path, _) = typed::issue_upgrading_in(dir.path(), &term, &sid(40), Some(&up))
                    .unwrap_or_else(|no| panic!("{signed:?}: {no:?}"));
                assert_eq!(
                    std::fs::read_to_string(&path).unwrap(),
                    lines(&hex_encode(&key)),
                    "{signed:?}"
                );
                assert_eq!(in_use(&term), (ShellIntegrationPosture::On, Some(key)));
                if read {
                    std::fs::remove_file(&path).unwrap();
                }
                assert_eq!(
                    typed::settle(&term, &sid(40)),
                    if read {
                        typed::Settled::Taken
                    } else {
                        typed::Settled::Withdrawn
                    }
                );
                assert!(!path.exists());
                assert_eq!(
                    in_use(&term),
                    (ShellIntegrationPosture::On, Some(key)),
                    "the key the shell signs with never moved"
                );
            }
        }

        let written = || std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(
            typed::issue_upgrading_in(
                dir.path(),
                &healthy(Some(crate::shell_body::own_rev())),
                &sid(41),
                Some(&up)
            )
            .err(),
            Some(typed::Refusal::Healthy),
            "a shell that signs this build's revision runs this build's body"
        );
        assert_eq!(
            typed::issue_in(dir.path(), &healthy(None), &sid(41)).err(),
            Some(typed::Refusal::Healthy),
            "no upgrade to hand: the key-only form never touches a healthy tab"
        );
        assert_eq!(written(), 0, "nothing written for either");

        let term = lost_nonce_engine();
        let (path, _) =
            typed::issue_upgrading_in(dir.path(), &term, &sid(42), Some(&up)).expect("issued");
        let text = std::fs::read_to_string(&path).unwrap();
        let fresh = text.lines().next().unwrap().to_string();
        assert_eq!(fresh.len(), 64);
        assert_eq!(text, lines(&fresh), "a fresh key, then the two lines");
        term.lock()
            .unwrap()
            .process(cycle(&format!(";id={fresh}")).as_bytes());
        assert_eq!(
            term.lock().unwrap().shell_integration_posture(),
            ShellIntegrationPosture::On,
            "the fresh key verifies the shell's marks"
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(typed::settle(&term, &sid(42)), typed::Settled::Taken);
    }

    /// A key whose line never ran is taken back with its file, so a mark signed with it is
    /// dropped.
    #[test]
    fn a_typed_rekey_whose_line_never_ran_is_taken_back() {
        let dir = scratch("rk-typed-withdraw");
        let (term, sid) = (lost_nonce_engine(), sid(3));
        let (first, first_serial) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
        let first_key = key_in(&first);
        let (second, second_serial) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
        assert!(!first.exists(), "the unread first key's file went with it");
        assert_ne!(key_in(&second), first_key);
        assert_eq!(
            typed::settle_issued(&term, &sid, Some(first_serial)),
            typed::Settled::None
        );
        assert!(second.exists());
        let second_key = key_in(&second);
        assert_eq!(
            typed::settle_issued(&term, &sid, Some(second_serial)),
            typed::Settled::Withdrawn
        );
        assert!(!second.exists(), "the one-use file does not linger");
        let mut engine = term.lock().unwrap();
        for key in [&first_key, &second_key] {
            let dropped = engine.shell_integration_dropped_count();
            engine.process(format!("\x1b]133;D;0;id={key}\x07").as_bytes());
            assert_eq!(
                engine.shell_integration_dropped_count(),
                dropped + 1,
                "a withdrawn key verifies nothing"
            );
        }
        assert_eq!(
            engine.shell_integration_posture(),
            ShellIntegrationPosture::Degraded
        );
        drop(engine);
        assert_eq!(typed::settle(&term, &sid), typed::Settled::None, "once");

        let (read, _) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
        let key = key_in(&read);
        std::fs::remove_file(&read).unwrap();
        assert_eq!(typed::settle(&term, &sid), typed::Settled::Taken);
        let mut engine = term.lock().unwrap();
        engine.process(cycle(&format!(";id={key}")).as_bytes());
        assert_eq!(
            engine.shell_integration_posture(),
            ShellIntegrationPosture::On
        );
        drop(engine);

        let term = lost_nonce_engine();
        let (waiting, _) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
        forget_typed(&sid);
        assert!(!waiting.exists());
        assert_eq!(typed::settle(&term, &sid), typed::Settled::None);
    }

    /// Leftovers of a window that exited first are swept by the next typed
    /// re-key of the same session (a stale `<sid>.<n>` file, and a symlink at
    /// such a name, removed without writing its target); the channel's own file
    /// and another session's are left alone.
    #[test]
    fn a_typed_rekey_sweeps_what_a_dead_window_left_and_nothing_else() {
        let dir = scratch("rk-typed-sweep");
        let (term, sid) = (lost_nonce_engine(), sid(4));
        let stale = dir
            .path()
            .join(format!("{}.{}", sid.as_str(), u64::MAX - 1));
        std::fs::write(&stale, "stale\n").unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "original").unwrap();
        let planted = dir.path().join(format!("{}.{}", sid.as_str(), u64::MAX));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        let other = dir.path().join(format!("{}.9", sid_of_other().as_str()));
        std::fs::write(&other, "theirs\n").unwrap();
        let (path, _) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
        assert!(!stale.exists() && std::fs::symlink_metadata(&planted).is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
        assert!(
            other.exists(),
            "another session's file is not ours to sweep"
        );
        assert!(path.exists());
        forget_typed(&sid);
    }

    fn sid_of_other() -> SessionId {
        sid(0xffff)
    }

    /// Two issues of one tab at once leave nothing standing: both are issued
    /// (the second settling the first), and once the waiting one is settled no
    /// file holds a key the engine would verify.
    #[test]
    fn two_issues_at_once_leave_no_key_in_a_file() {
        use std::sync::{Arc, Barrier};
        let dir = scratch("rk-typed-race");
        let sid = sid(5);
        let (mut left, mut live) = (0, 0);
        for _ in 0..50 {
            let term = Arc::new(lost_nonce_engine());
            let together = Arc::new(Barrier::new(2));
            let issue = || {
                let (term, together) = (Arc::clone(&term), Arc::clone(&together));
                let (dir, sid) = (dir.path().to_path_buf(), sid.clone());
                std::thread::spawn(move || {
                    together.wait();
                    typed::issue_in(&dir, &term, &sid).is_ok()
                })
            };
            let (a, b) = (issue(), issue());
            assert!(a.join().unwrap() && b.join().unwrap(), "both issued");
            assert_eq!(typed::settle(&term, &sid), typed::Settled::Withdrawn);
            for file in std::fs::read_dir(dir.path()).unwrap().flatten() {
                left += 1;
                let key = key_in(&file.path());
                let mut engine = term.lock().unwrap();
                let dropped = engine.shell_integration_dropped_count();
                engine.process(format!("\x1b]133;A;id={key}\x07").as_bytes());
                if engine.shell_integration_dropped_count() == dropped {
                    live += 1;
                }
                drop(engine);
                std::fs::remove_file(file.path()).unwrap();
            }
        }
        assert_eq!((left, live), (0, 0), "(files left, live keys in them)");
    }
}
