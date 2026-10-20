// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RE-KEY CHANNEL, host side (2026-09-24): how an adopted shell whose
//! shell-integration nonce did not survive a seamless update gets a new one.
//!
//! WHY (measured 2026-09-24 on the owner's Mac, 0.93.0): both live tabs read
//! `status integration=degraded` — the 0.91 parent of the 0.92 update dropped
//! their nonces (`git merge-base --is-ancestor 6656edb7c v0.91.0` → no), and
//! `spawn::authorize_adopted_shell_nonce` can only keep a lost nonce's
//! requirement standing. The shell fixed its nonce at source time, so every mark
//! it emits (`detail=`, blocks, exit codes, prompt navigation) stays dropped for
//! the tab's whole life; closing the tab — and the agent in it — was the only
//! cure.
//!
//! THE CHANNEL: at spawn the host exports `ATERM_REKEY_PATH` =
//! `<control dir>/rekey/<sid>` to every zsh/bash/fish it integrates; the script
//! captures and scrubs it and checks it at every prompt
//! (`__aterm_rekey_check`, one fork-free `[[ -f ]]`). The fact that a session HAS
//! the channel rides the handoff record (`SessionRecord::rekey`), because only a
//! shell spawned with the path — and so with this build's script, which the
//! content-addressed script folder guarantees it sourced — will ever read the
//! file. When a successor adopts such a session and finds its integration
//! degraded (no nonce carried, or no screen carried at all), it mints a fresh
//! nonce, writes it to that file ([`deliver`]: `0600`, renamed into place
//! whole, never through a symlink, in the `0700` control dir) and only then
//! authorizes it on the engine (`spawn::hydrate_adopted_engine`, the adoption's
//! one engine seam); the shell's next prompt picks it up and its marks resume,
//! signed with a key that never appeared in typed text, scrollback or history.
//!
//! `status integration=` reads `on` from the FIRST MARK signed with the new key,
//! not from the write (`Terminal::authorize_shell_integration_on_first_mark`):
//! a tab running a long program — the Claude tabs this was built for — reaches
//! its next prompt hours later, and until then its marks have not reached the
//! engine. Nor does a key the shell has not taken cross the next update: the
//! successor finds the session degraded again and writes a fresh one, which
//! replaces the untaken file. No expiry: the design's first sketch dropped an
//! unconsumed authorization after 10 s, which would have re-keyed exactly the
//! tabs that sit longest at a foreground program never.
//!
//! Under an OVERLAP handoff the write waits for the update's Commit
//! ([`Deferred`]): an adoption that is abandoned hands its sessions back to the
//! old process, and a key written for the abandoned candidate would have moved
//! the shell off the old process's key.
//!
//! A shell spawned BEFORE the channel has no hook, and the adoption writes
//! nothing for it: its record carries no `rekey`, so it stays `degraded` — its
//! marks dropped — rather than being reported healed. Nothing types a re-key
//! line into such a shell on its own: its foreground is usually an agent, and a
//! line typed there is a prompt.
//!
//! THE TYPED RE-KEY (2026-09-26) heals it anyway, at the one moment its SHELL
//! holds the terminal: the live agent upgrade, which has ended the agent and is
//! about to type its relaunch line at the shell's prompt (both degraded tabs
//! measured that day run Claude sessions). The sweep asks the window
//! (`@<sid> rekey shell=<pid>`, owner-only; [`typed::issue`]) — for a tab whose
//! integration is `degraded`, never a healthy one, and only while `<pid>` leads
//! the tab's foreground process group — and the window mints a key, writes it
//! with [`deliver`]'s discipline to a ONE-USE file `<control dir>/rekey/<sid>.<n>`
//! (never the channel's own `<sid>`), authorizes it as a key the shell has not
//! taken (`Terminal::authorize_shell_integration_rekey`: `on` only from its first
//! signed mark) and answers the PATH. The relaunch line reads the file into the
//! globals the old scripts sign with and removes it
//! (`aterm_shell_integration::typed_rekey`); the key never enters typed text.
//!
//! Unlike the channel's key, a typed one can be TAKEN BACK, and is whenever its
//! line never ran — a key no shell took must not stand where its file could
//! still hand it to someone else. The file decides ([`typed::settle`]): gone, the
//! shell read it and the key is kept; still there, the file is removed and the
//! authorization it replaced is restored. The sweep settles at once when it
//! types nothing after asking (`@<sid> rekey withdraw`); the window settles every
//! issued key itself [`typed::TTL`] after issuing it, and when the session
//! closes ([`forget_typed`]); a file left by a window that exited first is swept
//! by the next typed re-key of that session, and names a key no engine
//! authorizes (the carry never takes an untaken key across an update).
//!
//! THE TYPED UPGRADE (the LOADER / BODY split, 2026-09-26, `shell_body`): the
//! same line reaches a shell whose integration predates LOADERS — which no body
//! pointer can reach, and whose status reads `integration_rev=frozen` — and a
//! loader whose prompt hook never runs (2026-10-01, [`typed`]'s `behind`). For
//! a shell that does not sign THIS build's revision at the prompt it holds, the
//! one-use file also names this build's script folder and the session's body
//! pointer path (lines 2 and 3), and the line sources this build's loader
//! (`aterm_shell_integration::typed_rekey_with_loader`), which upgrades the
//! shell in place. Such a shell is issued the file HEALTHY too: its first line
//! is then the key it already signs with, so no key moves — not by this build's
//! line, nor by an older sweep's key-only one, which reads that line alone —
//! and nothing on the engine changes, so settling it touches only the file.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use aterm_session::SessionId;

/// The directory the re-key files live in: `<control dir>/rekey`, created
/// `0700`. `None` when there is no private control dir; the channel is then not
/// offered.
fn dir() -> Option<PathBuf> {
    crate::control_auth::private_subdir("rekey")
}

/// The re-key file of session `sid` in this user's control dir.
pub(crate) fn path_for(sid: &SessionId) -> Option<PathBuf> {
    crate::control_auth::sid_file_in(&dir()?, sid)
}

/// Write `key_hex` (a fresh 64-hex nonce) to `path` for the shell to take,
/// whole: the line and its newline go to a temp file beside it,
/// `.<name>.<pid>.<n>.tmp` (no script reads a dot name), created exclusively
/// (`O_EXCL`), `0600`, without following a symlink, then renamed over `path`.
/// The scripts' check reads and removes the file before it validates it, so a
/// file created at `path` and written after was, when a prompt checked in
/// between, consumed empty and the key or body never taken: 3 of about 37
/// loader-suite runs (8f317c9ba), and about 1% of in-place handoffs racing
/// fish 4.9.3's check under load (0 of about 19k renamed, 2026-10-02). `rename`
/// replaces a leftover at `path` (a stale key, or a planted symlink: the link,
/// not its target) in one step. A failed write leaves no temp file behind.
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
    // A crashed writer of an earlier process with this pid may have left it.
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

/// Remove session `sid`'s re-key file, if a key is still waiting there (the
/// session closed before its shell reached a prompt), and the one file a
/// bash writes beside it, `<sid>.trap` (its fork-free read of the DEBUG trap
/// its rc set, `aterm_shell_integration::DEBUG_TRAP_SUFFIX`). Best effort.
pub(crate) fn discard(sid: &SessionId) {
    if let Some(path) = path_for(sid) {
        let mut trap = path.clone().into_os_string();
        trap.push(aterm_core::shell_integration::DEBUG_TRAP_SUFFIX);
        let _ = std::fs::remove_file(trap);
        let _ = std::fs::remove_file(path);
    }
}

/// Re-key an adopted session whose marks are dropped, first half: mint a nonce
/// and hand it to the shell through `path`. The raw nonce comes back only when
/// the file was written, for the caller to authorize on the engine
/// (`spawn::hydrate_adopted_engine`), so a nonce the shell cannot receive is
/// never authorized. File work: call with no engine lock held. An adoption not
/// yet committed uses [`mint`] and [`Deferred`] instead.
#[cfg(any(unix, windows))]
pub(crate) fn hand_over(path: &Path, id: u64) -> Option<[u8; 32]> {
    let (pending, raw) = mint(path, id);
    pending.deliver().then_some(raw)
}

/// A file an adopted shell will read at its next prompt, not written yet: a
/// re-key for its channel, or the body pointer that moves it onto this build's
/// integration (`shell_body`). Both wait for an overlap handoff's Commit
/// ([`Deferred`]).
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
        // The key never reaches a log or a panic message.
        f.debug_struct("Pending")
            .field("path", &self.path)
            .field("id", &self.id)
            .field("what", &self.what)
            .finish_non_exhaustive()
    }
}

impl Pending {
    /// The body pointer naming `rev` (a script folder's address) for the shell
    /// that reads `path` (`shell_body::pending`).
    pub(crate) fn body(path: &Path, rev: &str, id: u64) -> Self {
        Self {
            path: path.to_path_buf(),
            content: rev.to_owned(),
            id,
            what: What::Body,
        }
    }

    /// Write the file for the shell to take ([`deliver`]), saying either way.
    /// `true` when it was written.
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

/// Mint a fresh nonce for the shell reading `path`: the re-key to write and the
/// raw nonce to authorize on the engine.
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

/// The re-keys of an adoption not yet committed. A successor adopts its shells
/// before the outgoing process commits, and an attempt can still be abandoned
/// after that, when the old process takes every session back. A key file
/// written during adoption would then move the shell onto a key only the
/// abandoned candidate authorized (or overwrite the old process's own waiting
/// key). So an adoption under an overlap handoff authorizes its key on the
/// engine at once and holds the write here until Commit ([`Self::release`],
/// from the commit waiter, after the reader gate opens); an abandoned attempt
/// writes nothing. Body pointers (`shell_body`) wait here for the same reason.
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

    /// Hold `pending` for the Commit — or, once released, write it now.
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

    /// Whether [`Self::release`] has run: the incoming handoff COMMITTED, and
    /// its adopted shells are this process's. A test's view: a session's drop
    /// asks [`Self::withdraw`], which answers the same under one lock.
    #[cfg(all(test, unix))]
    pub(crate) fn released(&self) -> bool {
        self.held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_none()
    }

    /// Session `sid` closed. Before Commit it lets go of only what THIS
    /// process holds for it — the writes held here, which nothing will want
    /// once the session is gone — and answers `true`: the files at the sid's
    /// paths (`<control dir>/rekey/<sid>`, `<control dir>/integration/<sid>`)
    /// are then the PREDECESSOR's, which resumes the shell on a rollback and
    /// whose shell has not read them yet (round eight, findings 20 and 44).
    /// Once released it holds nothing and answers `false`: the session is
    /// this process's, and so are the files. One lock, so the answer and the
    /// withdrawal cannot straddle the Commit.
    pub(crate) fn withdraw(&self, sid: &SessionId) -> bool {
        let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
        let Some(queue) = held.as_mut() else {
            return false;
        };
        // Both files are named by the sid itself (`control_auth::sid_file_in`).
        queue
            .retain(|pending| pending.path.file_name() != Some(std::ffi::OsStr::new(sid.as_str())));
        true
    }

    /// THE COMMIT: write every held key; later ones are written at once.
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

// ─── The typed re-key ───

/// The session `sid` closed: a typed re-key still waiting for it is dropped
/// with its file ([`typed::forget`]; the engine goes too, so nothing is left to
/// take back).
pub(crate) fn forget_typed(sid: &SessionId) {
    #[cfg(unix)]
    typed::forget(sid);
    #[cfg(not(unix))]
    let _ = sid;
}

/// The typed re-key, the window's half (see the module docs): issued for a tab
/// the verb has proven its shell holds, settled by its file. POSIX only, like
/// the foreground proof it rests on.
#[cfg(unix)]
pub(crate) mod typed {
    // Imported here, not at the top of the file: this half is POSIX only, and a
    // Windows build would read them there as unused.
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use aterm_core::terminal::{ShellIntegrationPosture, Terminal};

    use super::{Path, PathBuf, SessionId};
    use super::{deliver, dir};

    /// How long a typed re-key's file waits for the line that reads it before
    /// the window settles it ([`settle`]). The relaunch line is typed within
    /// seconds or not at all (its fenced tries park at most half a minute), so
    /// this only bounds a key whose sweep died between asking and typing.
    pub(crate) const TTL: Duration = Duration::from_secs(120);

    /// Why a typed re-key was not issued: the verb's `ERR rekey <words>`.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Refusal {
        /// `integration=on`: the shell's marks already verify, and a re-key would
        /// move a working shell off its key.
        Healthy,
        /// `integration=off`: no nonce is required here, so there is nothing to heal.
        Off,
        /// The re-key channel's own key waits in `<sid>` for this shell's next
        /// prompt, and a typed one would race it.
        Pending,
        /// No private control dir to put the file in, or a sid that names none.
        NoDir,
        /// The file could not be written; nothing was authorized.
        Write(std::io::ErrorKind),
        /// No expiry could be started to settle it later, so it was taken back
        /// at once.
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

    /// How a typed re-key was settled ([`settle`]): the verb's `OK rekey <word>`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Settled {
        /// No typed re-key of this session was waiting.
        None,
        /// Its file was gone: the shell read the key, which is kept (still `degraded`
        /// until the first mark signed with it).
        Taken,
        /// Its file was still there: removed, and the authorization the key
        /// replaced restored.
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

    /// THE TYPED UPGRADE (the LOADER / BODY split, 2026-09-26): what a shell
    /// that is BEHIND ([`behind`]) is handed beside its key — this build's
    /// script folder and the session's body pointer path, lines 2 and 3 of the
    /// one-use file — so the relaunch line
    /// (`aterm_shell_integration::typed_rekey_with_loader`) sources this build's
    /// loader and the shell reads body pointers from then on. [`issue`] builds it
    /// only for such a shell.
    #[derive(Clone, Debug)]
    pub(crate) struct Upgrade {
        /// This build's installed script folder (`ensure_script_set`).
        pub(crate) folder: PathBuf,
        /// The session's body pointer file (`shell_body::path_for`).
        pub(crate) pointer: PathBuf,
    }

    /// What an issue writes, decided from the engine under one lock.
    enum Plan {
        /// A DEGRADED shell: a fresh key, authorized as one it has not taken
        /// (and the upgrade's lines, when it is behind).
        Heal,
        /// A HEALTHY shell that is behind: the key it already signs with — never
        /// moved — and the upgrade's lines. An older sweep's key-only line reads
        /// the same key from the first line, so it moves nothing either.
        UpgradeOnly([u8; 32]),
    }

    /// The typed re-keys issued and not yet settled, by session: at most one
    /// each. A leaf lock, never held across an engine lock or a file operation.
    static ISSUED: Mutex<BTreeMap<String, Issued>> = Mutex::new(BTreeMap::new());

    /// This process's typed re-key numbers: each file's name is its own.
    static SERIAL: AtomicU64 = AtomicU64::new(1);

    /// One issue at a time. [`issue_in`] is a sequence (settle the previous key,
    /// sweep, write, authorize, record); two interleaved issues of one tab each
    /// settle nothing and the later record overwrites the earlier, leaving a
    /// live key in an unsettled file. Held across the whole issue, before the
    /// engine lock and [`ISSUED`]; nothing that holds either takes it. A settle
    /// needs no turn: it takes its record out under [`ISSUED`] first, so a settle
    /// and an issue interleave safely.
    static ISSUING: Mutex<()> = Mutex::new(());

    /// Whether the shell of `engine` is BEHIND: the last revision it SIGNED is
    /// not this build's. The verb asks only while the shell holds its prompt,
    /// and a loader whose prompt hook runs signs the body it runs before every
    /// prompt — this build's, once a successor has pointed it here. So a shell
    /// that signs none runs an integration from before loaders, or a loader
    /// whose hook never runs: a fish from before the prompt event (2026-10-01)
    /// under a prompt its config.fish defines (`starship init fish | source`)
    /// lost the trampoline, the only place it read a pointer or signed, for the
    /// shell's life. One that signs an OLDER build's is such a shell that signed
    /// once — its own typed upgrade, by a build from before the event — and has
    /// taken no pointer since. No pointer reaches either; this build's loader,
    /// sourced in place, does. Read from the engine, never the registry's
    /// loader flag, which says a loader was spawned, not that it runs (review
    /// finding 2026-10-01). A loader that runs, at a prompt drawn before this
    /// build pointed it here, reads behind too, and the upgrade in place gives
    /// it at once the body its next prompt would have taken.
    fn behind(engine: &Terminal) -> bool {
        engine.shell_integration_rev() != Some(crate::shell_body::own_rev())
    }

    /// What `term`'s shell may be issued, and whether the upgrade's lines go
    /// with it — `upgrading` (an upgrade is at hand) and the shell is
    /// [`behind`]: a heal when its integration is degraded; an upgrade-only
    /// file when it is healthy and both hold; else why not.
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

    /// ISSUE a typed re-key for session `sid`, whose engine is `term`: the one-use
    /// file's path, or why not. The caller has already proven the tab's shell holds
    /// its terminal (the verb's foreground check). Settled [`TTL`] from now
    /// by the window itself unless the sweep settles it first. `upgrade` builds
    /// the typed upgrade's lines — file work, run with no lock held — and is
    /// called only for a shell that is [`behind`].
    pub(crate) fn issue(
        term: &Arc<Mutex<Terminal>>,
        sid: &SessionId,
        upgrade: impl FnOnce() -> Option<Upgrade>,
    ) -> Result<PathBuf, Refusal> {
        // A healthy shell that runs this build's body, or an unintegrated tab, is
        // refused before anything is touched.
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
            // Nothing would take it back later, so it is taken back now.
            settle_issued(term, sid, Some(serial));
            return Err(Refusal::NoExpiry(e.kind()));
        }
        Ok(path)
    }

    /// [`issue`] into `dir`, without the expiry and without an upgrade: the
    /// file's path and the key's serial (the tests' and the conformance's form).
    #[cfg(test)]
    pub(crate) fn issue_in(
        dir: &Path,
        term: &Mutex<Terminal>,
        sid: &SessionId,
    ) -> Result<(PathBuf, u64), Refusal> {
        issue_upgrading_in(dir, term, sid, None)
    }

    /// [`issue_in`] with the typed upgrade's lines, for a shell that is
    /// [`behind`]: written after the key for a degraded shell, and — the key it
    /// already signs with in front — for a healthy one. A shell that signs this
    /// build's revision is handed none (a degraded one, its key alone).
    pub(crate) fn issue_upgrading_in(
        dir: &Path,
        term: &Mutex<Terminal>,
        sid: &SessionId,
        upgrade: Option<&Upgrade>,
    ) -> Result<(PathBuf, u64), Refusal> {
        let channel = crate::control_auth::sid_file_in(dir, sid).ok_or(Refusal::NoDir)?;
        let _one = ISSUING.lock().unwrap_or_else(|p| p.into_inner());
        // An earlier typed re-key of this session is settled first, by its file,
        // so the authorization a new one replaces is the one the shell really has.
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
        // Written before it is authorized: a key the shell cannot receive is
        // never authorized.
        deliver(&path, &content).map_err(|e| Refusal::Write(e.kind()))?;
        let refused = {
            let mut engine = crate::term_lock(term);
            match (engine.shell_integration_posture(), nonce, kept) {
                (ShellIntegrationPosture::Degraded, Some(nonce), _) => {
                    engine.authorize_shell_integration_rekey(nonce);
                    None
                }
                // The upgrade names the key in use: still the one in use.
                (ShellIntegrationPosture::On, None, Some(kept))
                    if engine.shell_integration_nonce_in_use() == Some(kept) =>
                {
                    None
                }
                _ => Some(Refusal::Healthy),
            }
        };
        if let Some(no) = refused {
            // A mark verified between the look and the write (healthy now), or
            // the key an upgrade named moved: nothing is issued.
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
    /// before its expiry settled them. Never the channel's own `<sid>`.
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

    /// Settle the typed re-key waiting for session `sid` (the verb's `rekey
    /// withdraw`) by its file: gone, the key is kept; still there, the file is
    /// removed and the key taken back.
    pub(crate) fn settle(term: &Mutex<Terminal>, sid: &SessionId) -> Settled {
        settle_issued(term, sid, None)
    }

    /// [`settle`] for the key numbered `serial` only (`None`: whichever is
    /// waiting), the expiry's form, which must never settle a later key. An
    /// upgrade-only issue authorized nothing, so only its file is settled.
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
        // The file decides. Removed now: it was still waiting, so the line never ran.
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

    /// The session `sid` closed: its waiting typed re-key's file goes with it (the
    /// engine goes too, so nothing is left to take back).
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
    /// symlink at the path is replaced without writing through it. Neither a
    /// delivery nor a failed one leaves its temp file in the directory.
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

        // A directory at the path refuses the rename: an error, and no temp.
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

    /// Healed through the adoption itself: a session with the channel, adopted
    /// from a carry with its nonce requirement and no nonce, comes out of
    /// `spawn::hydrate_adopted_engine` with a key in its file. Marks signed with
    /// the old key or none stay dropped; the new key's cycle lands. The posture
    /// reads `degraded` until that first mark, then `on`. The same holds for an
    /// adoption that carried no screen.
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

    /// An adoption not committed writes no key: under an overlap handoff the
    /// old key stays in the file through an abandoned attempt, and the Commit
    /// writes the new one, which the engine (authorized at adoption) verifies.
    /// Without the queue the key is written at once
    /// (`the_adoption_rekeys_a_degraded_shell_that_has_the_channel`).
    #[test]
    fn an_uncommitted_adoption_holds_its_key_until_the_commit() {
        let dir = scratch("rk-deferred");
        let path = dir.path().join("s-ab");
        let old_key = "7".repeat(64);
        deliver(&path, &old_key).expect("the old process's own waiting key");
        // Abandoned: the queue is dropped unreleased (the candidate exits).
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
        // Committed: the release writes the key the engine already verifies.
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
        // After the Commit a key is written at once.
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

    // ─── The typed re-key ───

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

    /// The heal, window side: a degraded tab gets a private one-use file named
    /// for its session and number, beside (never over) the channel's `<sid>`,
    /// holding a fresh key the engine verifies. Until the first mark signed with
    /// it the tab reads `degraded`, and marks signed with the old key or none (a
    /// forged `133;D`) stay dropped. The first signed cycle lands: `detail=`
    /// names the program, `integration=on`.
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
        // The line removed the file; the window then finds it read.
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

    /// THE TYPED UPGRADE (the LOADER / BODY split, 2026-09-26): a HEALTHY shell
    /// that is BEHIND — it signs no revision (from before loaders, or a loader
    /// whose prompt hook never runs), or an OLDER build's (2026-10-01) — is
    /// issued the upgrade: the key it already signs with (never moved), this
    /// build's folder and its pointer path; nothing on the engine changes, and
    /// settling it touches only the file, either way. A DEGRADED one gets a
    /// fresh key with the same two lines, authorized as a typed re-key as
    /// before. A shell that signs THIS build's revision runs this build's body,
    /// so a healthy one is refused with nothing written; so is a healthy one
    /// with no upgrade to hand it.
    #[test]
    fn a_shell_behind_this_build_is_issued_the_upgrade_healthy_or_not() {
        use aterm_core::shell_integration::hex_encode;
        // Sids no other test of this module uses: the issued keys are one
        // process-wide map, and the tests run in parallel.
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

    /// A key whose line never ran leaves nothing standing: settled with its file
    /// present, the file is removed and the engine returns to the lost nonce, so
    /// a mark signed with the key is dropped and the tab reads `degraded`. The
    /// expiry settles only its own key; a second key over an unread first
    /// supersedes it; a closing session takes its file along. Negative control:
    /// a key whose file was read is kept (`Taken`) and verifies marks.
    #[test]
    fn a_typed_rekey_whose_line_never_ran_is_taken_back() {
        let dir = scratch("rk-typed-withdraw");
        let (term, sid) = (lost_nonce_engine(), sid(3));
        let (first, first_serial) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
        let first_key = key_in(&first);
        // A second key over the unread first: the first is taken back, its file gone.
        let (second, second_serial) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
        assert!(!first.exists(), "the unread first key's file went with it");
        assert_ne!(key_in(&second), first_key);
        // The first key's expiry does not settle the second.
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

        // Negative control: read, so kept.
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

        // A session that closes takes its waiting file with it.
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
        // Serials no issue of this process draws: the counter is process-wide,
        // and a low serial could be a live key issued by a parallel test.
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
        for _ in 0..400 {
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
