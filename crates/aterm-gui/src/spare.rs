// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WARM SPARE SHELL (`docs/DESIGN-warm-spare-2026-10-16.md`).
//!
//! A new tab's prompt took ~37 ms from the request: the main thread's turn was
//! already under a millisecond, and the shell's own exec and startup files were
//! the rest. So aterm keeps ONE shell spawned ahead, before anyone asks, and a
//! plain Cmd-T adopts it instead of forking.
//!
//! Two tiers, one slot ([`Spares`], on the main thread):
//! - PARKED. The spare's wrapper waits at its first line, before any startup
//!   file (`aterm_shell_integration`'s parked hold), for one line on a FIFO
//!   aterm holds open ([`SpareFifo`]). Cmd-T says `go`, and the startup files
//!   run in view exactly as in a cold tab; aterm's spawn and the shell's exec
//!   and init are already done. No user code runs before anyone asked.
//! - WARM. Only for an rc generation a learned gate ([`GateBook`], on the
//!   `aterm-spare` thread) has seen run clean three times in view: the spare is
//!   told `run`, its startup files run unseen, and it waits again at the top of
//!   its first prompt hook, before any prompt work, clock or history-dependent
//!   expansion. Cmd-T says `go`, and the first prompt is drawn now, with
//!   history re-read when the file moved, `$SECONDS` carried on and the hooks
//!   the rc put ahead of aterm's run again.
//!
//! Until it is adopted a spare is nobody's session: never registered (no `ls`,
//! fabric, graph entry, restore journal, quit safety, keeper or primer), every
//! wake its reader posts swallowed ([`SpareLink::post`], with a backstop at the
//! top of `user_event`), its host effects only taint it ([`wire_spare_taint`]),
//! and a control request or an askpass from inside it is refused and taints it
//! ([`control_refusal`]). A spare whose shell shows anything the gate cannot
//! vouch for is killed and never adopted.
//!
//! Who never gets a spare: every ask that is not plain (`identity=`,
//! `observe=`/connected, operator, create-identity, `folder_of`), fish, pwsh,
//! `-e`, `integrate = false`, `temporal_recording`, held panes, and Linux and
//! Windows. They take today's cold path, unchanged. A spare exists only once a
//! person has been seen (a human gesture, or one of its windows coming to the
//! front), and is kept PARKED while a window is open; only promotion to WARM
//! waits for a person present now (seen in the last 10 minutes), so the first
//! Cmd-T after a long absence still adopts a parked spare. Wire-driven headless
//! instances, the test suites and agents' private instances see none unless
//! they set `spare_shell_needs_person = false`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_core::terminal::{StartKind, StartMark, Terminal};
use winit::event_loop::EventLoopProxy;

use crate::{App, Session, Wake, WindowId};

// ─── Numbers (design §0, §6, §7, §9) ───

/// A spawned spare must say it is parked within this; one that does not ran past
/// the wrapper's hold, with the rc possibly running unasked.
pub(crate) const PARKED_DEADLINE: Duration = Duration::from_secs(1);
/// A parked spare older than this is replaced.
pub(crate) const PARKED_MAX_AGE: Duration = Duration::from_secs(60 * 60);
/// A ready warm spare older than this (since it reached its hold) is discarded.
pub(crate) const READY_MAX_AGE: Duration = Duration::from_secs(10 * 60);
/// How recent a person's gesture must be for a spare to be promoted to WARM.
pub(crate) const PERSON_WINDOW: Duration = Duration::from_secs(10 * 60);
/// No spare before the first window has been up this long: never on the cold
/// start's path.
pub(crate) const BOOT_GRACE: Duration = Duration::from_secs(1);
/// A landing (adopted or cold) waits this before the next spare is booked.
pub(crate) const SETTLE: Duration = Duration::from_millis(150);
/// Promotion to WARM waits for this long without input.
pub(crate) const PROMOTE_IDLE: Duration = Duration::from_secs(1);
/// …and this long with the focused folder unchanged.
pub(crate) const PROMOTE_FOLDER_STABLE: Duration = Duration::from_secs(3);
/// At most this many parked spawns per rolling hour.
pub(crate) const PARKED_BUDGET: usize = 60;
/// At most this many warm spares discarded per rolling hour.
pub(crate) const WARM_BUDGET: usize = 8;
const HOUR: Duration = Duration::from_secs(60 * 60);
/// At least this between two replacements.
pub(crate) const REPLACE_MIN: Duration = Duration::from_secs(2);
/// A parked spare's folder or window moved: replaced once the new one holds this.
pub(crate) const REPLACE_PARKED_AFTER: Duration = Duration::from_secs(1);
/// A warm spare's: kept this long (the person may come back), then replaced.
pub(crate) const REPLACE_WARM_AFTER: Duration = Duration::from_secs(10);
/// No spare within this of a critical memory-pressure episode.
pub(crate) const PRESSURE_HOLD: Duration = Duration::from_secs(10 * 60);
/// Retired spare ids whose late wakes the backstop still swallows.
const TOMBSTONES: usize = 32;
/// Clean visible runs of a generation before it may run unseen.
pub(crate) const CLEAN_STREAK: u32 = 3;
/// A cold tab's start record must arrive within this to count clean.
pub(crate) const SAMPLE_WINDOW: Duration = Duration::from_secs(2);
/// A backoff failure's first and second ineligibility.
const BACKOFF_FIRST: Duration = Duration::from_secs(60 * 60);
const BACKOFF_SECOND: Duration = Duration::from_secs(24 * 60 * 60);
/// The escape scan runs at most this often.
const SCAN_EVERY: Duration = Duration::from_secs(2);
/// …and looks this far past the start mark.
const SCAN_GRACE: Duration = Duration::from_millis(100);
/// The warm deadline's bounds (`T_READY = clamp(4 × max rc_ms, 1 s, 8 s)`).
const T_READY_MIN: Duration = Duration::from_secs(1);
const T_READY_MAX: Duration = Duration::from_secs(8);
/// Generations the gate file keeps.
const GATE_KEEP: usize = 16;
/// A promotion writes `run` only on a stamp of the rc generation asked for
/// within this ([`Job::Freshen`]).
const FRESH_FOR: Duration = Duration::from_secs(2);
/// The gate file's name under the state dir.
const GATE_FILE: &str = "spare-gate-v1.tsv";
/// The persisted gate is a small cache, never an unbounded input.
const GATE_FILE_MAX: u64 = 128 * 1024;

// ─── Config ───

/// `spare_shell`: `auto` (parked, warm where the gate allows), `parked` (never
/// a startup file before Cmd-T), `off`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpareMode {
    Auto,
    Parked,
    Off,
}

impl SpareMode {
    /// The accepted spellings, in the order `--validate-config` lists them.
    pub(crate) const SPELLINGS: [&'static str; 3] = ["auto", "parked", "off"];

    /// `None` for an unknown spelling.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "parked" => Some(Self::Parked),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

// ─── What a session witnesses (every session's, on `SessionCtx`) ───

/// A shell's start record as its reader saw it arrive (design §4): the mark, and
/// the session's state at that instant.
#[derive(Clone, Debug)]
pub(crate) struct StartSeen {
    pub(crate) at: Instant,
    pub(crate) mark: StartMark,
    /// The PTY's driven input then (keys, pastes, control writes).
    pub(crate) input: Option<aterm_session::sink::DrivenInput>,
    /// The terminal's foreground process group then.
    pub(crate) fg_pgrp: i32,
    /// The shell's executable then.
    pub(crate) image: Option<PathBuf>,
    /// The alternate screen was up.
    pub(crate) alt_screen: bool,
}

/// What a session's reader, host callbacks and control seams witness that the
/// spare's gate reads (design §4, §5.2, §5.4). Every session has one; a spare's
/// is its [`SpareLink`]'s. Cheap: the reader pays two `Option::take`s per
/// batch, and the syscalls run once per session, when its start mark arrives.
#[derive(Default)]
pub(crate) struct Witness {
    pub(crate) parked_seen: OnceLock<Instant>,
    pub(crate) start_seen: OnceLock<StartSeen>,
    /// Host effects the real callbacks delivered (bell, notification, clipboard).
    pub(crate) host_effects: AtomicU32,
    /// Control requests that resolved to this session before its start mark.
    pub(crate) early_control: AtomicU32,
    /// Askpass requests from inside this session before its start mark.
    pub(crate) askpass: AtomicU32,
    /// The shell's pid and the executable it ran when spawned.
    pub(crate) shell: OnceLock<(i32, Option<PathBuf>)>,
    /// A spare's FIFO is still held: the reader closes it at the first prompt.
    fifo_held: AtomicBool,
    fifo: Mutex<Option<SpareFifo>>,
    /// The `aterm-spare` thread waits for this session's start record.
    observed: AtomicBool,
    /// How this session's shell started, when it was an adopted spare: its
    /// tier and its age at the adoption (`status born=`).
    born: OnceLock<(&'static str, u128)>,
}

impl Witness {
    /// Record the shell this session runs, at spawn.
    pub(crate) fn note_shell(&self, pid: i32) {
        let _ = self.shell.set((pid, shell_image(pid)));
    }

    /// Hold `fifo` until the shell's first prompt start (or the session's end).
    pub(crate) fn hold_fifo(&self, fifo: SpareFifo) {
        *self.fifo.lock().unwrap_or_else(|p| p.into_inner()) = Some(fifo);
        self.fifo_held.store(true, Ordering::Release);
    }

    /// Write one protocol line to the held FIFO; `false` when none is held or
    /// the write failed.
    pub(crate) fn tell(&self, line: &str) -> bool {
        self.fifo
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|f| f.tell(line).is_ok())
    }

    /// Let go of the FIFO: closed and unlinked as it drops.
    pub(crate) fn take_fifo(&self) -> Option<SpareFifo> {
        self.fifo_held.store(false, Ordering::Release);
        self.fifo.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    /// `status`'s `born=` (and `spare_age_ms=` for an adopted spare).
    pub(crate) fn born_fields(&self) -> String {
        match self.born.get() {
            Some((tier, age)) => format!("born=spare-{tier} spare_age_ms={age}"),
            None => "born=cold".to_owned(),
        }
    }
}

/// The reader's capture after each processed chunk, under the term lock it
/// already holds (design §4): a signed parked mark, a signed start record (with
/// the session's state at that instant), and, for an adopted spare, the first
/// prompt start, which proves the shell left every hold, so the FIFO goes.
#[cfg(unix)]
pub(crate) fn witness_after_process(
    w: &Witness,
    t: &mut Terminal,
    master: i32,
    input: impl FnOnce() -> Option<aterm_session::sink::DrivenInput>,
) {
    if t.take_spare_parked() {
        let _ = w.parked_seen.set(Instant::now());
    }
    if let Some(mark) = t.take_start_mark() {
        let pid = w.shell.get().map_or(-1, |(pid, _)| *pid);
        let seen = StartSeen {
            at: Instant::now(),
            mark,
            input: input(),
            fg_pgrp: crate::quit_safety::foreground_pgrp(master),
            image: shell_image(pid),
            alt_screen: t.modes().alternate_screen,
        };
        if w.start_seen.set(seen).is_ok() && w.observed.load(Ordering::Acquire) {
            let _ = send_job(Job::Marked);
        }
    }
    if w.fifo_held.load(Ordering::Acquire) && t.shell_prompt_started() {
        drop(w.take_fifo());
    }
}

/// `pid`'s executable, where the platform can say.
fn shell_image(pid: i32) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        (pid > 0)
            .then(|| aterm_uds::process::read_process_path(pid))
            .flatten()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

// ─── The FIFO (design §3.1) ───

/// A spare's hold FIFO, `<control dir>/spare/<pid>-<birth>-<sid>` (this
/// process's pid and start time: [`sweep_dead_fifos`] tells a crashed owner's
/// from a live one's by them), with the symlink `spare/<sid>` beside it, the
/// name the shell derives from its re-key path. 0600 in a 0700 folder, made
/// before the fork and held open read-write for the spare's life, so the
/// shell's open never blocks, a line written before the shell reads waits in
/// the pipe, and nothing aterm writes can become typed input. Dropped, its
/// names are unlinked and it is closed.
pub(crate) struct SpareFifo {
    path: PathBuf,
    /// The shell's name for it (`None`: a test's FIFO, opened at its path).
    link: Option<PathBuf>,
    file: std::fs::File,
}

/// `<pid>-<birth>`, this process's half of its FIFOs' names
/// ([`aterm_pty::process_birth`]; a birth it cannot read is `0`).
#[cfg(unix)]
fn fifo_owner() -> &'static str {
    static OWNER: OnceLock<String> = OnceLock::new();
    OWNER.get_or_init(|| {
        let pid = std::process::id();
        let birth = i32::try_from(pid)
            .ok()
            .and_then(aterm_pty::process_birth)
            .unwrap_or(0);
        format!("{pid}-{birth}")
    })
}

/// A FIFO name's owner and sid: `<pid>-<birth>-<sid>`, the sid a minted one.
#[cfg(unix)]
fn fifo_name_parts(name: &str) -> Option<(u32, u64, &str)> {
    let mut parts = name.splitn(3, '-');
    let pid = parts.next()?.parse().ok()?;
    let birth = parts.next()?.parse().ok()?;
    let sid = parts.next()?;
    crate::keeper_link::valid_sid(sid).then_some((pid, birth, sid))
}

impl SpareFifo {
    /// Make session `sid`'s FIFO and its link, and open it.
    #[cfg(unix)]
    pub(crate) fn make(sid: &aterm_session::SessionId) -> std::io::Result<Self> {
        let dir = crate::control_auth::private_subdir(aterm_core::shell_integration::SPARE_DIR)
            .ok_or_else(|| std::io::Error::other("no private control dir"))?;
        let link = crate::control_auth::sid_file_in(&dir, sid)
            .ok_or_else(|| std::io::Error::other("not a minted sid"))?;
        let name = format!("{}-{}", fifo_owner(), sid.as_str());
        let mut fifo = Self::make_at(dir.join(&name))?;
        // Relative, so it names the FIFO beside it wherever the folder is reached from.
        std::os::unix::fs::symlink(&name, &link)?;
        fifo.link = Some(link);
        Ok(fifo)
    }

    #[cfg(not(unix))]
    pub(crate) fn make(_sid: &aterm_session::SessionId) -> std::io::Result<Self> {
        Err(std::io::Error::other("no spare off unix"))
    }

    /// A FIFO at `path`, opened, with no link (a test's; [`Self::make`]'s).
    #[cfg(unix)]
    pub(crate) fn make_at(path: PathBuf) -> std::io::Result<Self> {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::other("NUL in the FIFO path"))?;
        // SAFETY: `c` is a NUL-terminated path that lives across the call.
        if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path);
        match file {
            Ok(file) => Ok(Self {
                path,
                link: None,
                file,
            }),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                Err(e)
            }
        }
    }

    /// Write `line` and its newline. The pipe is empty and the write tiny, so
    /// it never blocks (the descriptor is non-blocking regardless).
    fn tell(&self, line: &str) -> std::io::Result<()> {
        use std::io::Write as _;
        (&self.file).write_all(format!("{line}\n").as_bytes())
    }
}

impl Drop for SpareFifo {
    fn drop(&mut self) {
        if let Some(link) = &self.link {
            let _ = std::fs::remove_file(link);
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Release and remove the FIFOs a crashed aterm left in `dir` (design §3.1).
/// Each `<pid>-<birth>-<sid>` whose owner is gone (no process under the pid, or
/// another one born at another time) is opened for writing, which wakes a
/// spare still blocked opening it (one whose rc ignored the hangup); then its
/// link and itself are unlinked, and only then closed, so that shell reads
/// end-of-file, finds no FIFO and exits. A live owner's FIFO (this process's
/// too), and any entry that names no owner, is left. Once per process, on the
/// `aterm-spare` thread.
#[cfg(unix)]
pub(crate) fn sweep_dead_fifos(dir: &Path) {
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((pid, birth, sid)) = name.to_str().and_then(fifo_name_parts) else {
            continue;
        };
        if !entry.file_type().is_ok_and(|t| t.is_fifo()) || fifo_owner_alive(pid, birth) {
            continue;
        }
        let path = entry.path();
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path);
        let link = dir.join(sid);
        if std::fs::read_link(&link).is_ok_and(|to| to.as_os_str() == name.as_os_str()) {
            let _ = std::fs::remove_file(&link);
        }
        let _ = std::fs::remove_file(&path);
        drop(writer);
    }
}

/// Whether the process that named a FIFO `<pid>-<birth>-…` still runs: a live
/// pid whose start time is `birth`, or one whose start time cannot be read now
/// or could not be when the name was made (`0`, [`fifo_owner`]): both left
/// alone, as unproven.
#[cfg(unix)]
fn fifo_owner_alive(pid: u32, birth: u64) -> bool {
    aterm_uds::process::pid_alive(pid)
        && (birth == 0
            || i32::try_from(pid)
                .ok()
                .and_then(aterm_pty::process_birth)
                .is_none_or(|now| now == birth))
}

// ─── The link: a spare's reader, swallowed until adoption (design §5.3) ───

/// Taint kinds: what a spare's program asked of the host before anyone owned it.
pub(crate) mod taint {
    pub(crate) const BELL: u32 = 1;
    pub(crate) const NOTIFY: u32 = 1 << 1;
    pub(crate) const CLIPBOARD: u32 = 1 << 2;
    pub(crate) const KITTY: u32 = 1 << 3;
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) const WINDOW_OP: u32 = 1 << 4;
    pub(crate) const CONTROL: u32 = 1 << 5;
    /// Only macOS answers sudo's askpass ([`askpass_refused`]).
    #[cfg(target_os = "macos")]
    pub(crate) const ASKPASS: u32 = 1 << 6;
}

/// What a spare's reader tells the main thread before adoption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpareNote {
    /// It waits at the parked hold.
    Parked,
    /// Its start record arrived.
    AtStart,
    /// Its PTY closed.
    Gone,
    /// The `aterm-spare` thread's verdict on a warm spare at its hold.
    Verified(Verdict),
    /// The learned gate changed (`id` is 0).
    Gate,
}

/// A verification's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Ok,
    /// Ineligible until the generation changes.
    Deterministic(&'static str),
    /// Backoff: an hour, then a day.
    Backoff(&'static str),
}

/// The state a spare's reader and the main thread share: whether it is live
/// (adopted), which window owns it then, and what its reader reported before.
pub(crate) struct SpareLink {
    pub(crate) id: u64,
    pub(crate) witness: Arc<Witness>,
    live: AtomicBool,
    window: AtomicU64,
    reader_started: AtomicBool,
    exited: AtomicBool,
    parked_noted: AtomicBool,
    start_noted: AtomicBool,
    taint: AtomicU32,
    /// Where its notes and (once live) its wakes go: the event loop's proxy.
    poster: Option<crate::spawn::WakePost>,
}

impl SpareLink {
    pub(crate) fn new(id: u64, window: WindowId, poster: Option<crate::spawn::WakePost>) -> Self {
        Self {
            id,
            witness: Arc::default(),
            live: AtomicBool::new(false),
            window: AtomicU64::new(window.0),
            reader_started: AtomicBool::new(false),
            exited: AtomicBool::new(false),
            parked_noted: AtomicBool::new(false),
            start_noted: AtomicBool::new(false),
            taint: AtomicU32::new(0),
            poster,
        }
    }

    /// Record what the program asked of the host (a [`taint`] kind).
    pub(crate) fn taint(&self, kind: u32) {
        self.taint.fetch_or(kind, Ordering::AcqRel);
    }

    pub(crate) fn tainted(&self) -> u32 {
        self.taint.load(Ordering::Acquire)
    }

    pub(crate) fn exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    pub(crate) fn reader_started(&self) -> bool {
        self.reader_started.load(Ordering::Acquire)
    }

    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    /// Adopted into `window`: every later wake is the session's.
    pub(crate) fn go_live(&self, window: WindowId) {
        self.window.store(window.0, Ordering::Release);
        self.live.store(true, Ordering::Release);
    }

    fn send(&self, wake: Wake) -> bool {
        self.poster.as_ref().is_some_and(|post| post(wake))
    }

    /// Decide one wake the reader posts (design §5.3). Live: forwarded, its
    /// window rewritten to the owner's. Not live: `Output` returns `false`
    /// (the documented headless path, which clears the latch) after noting a
    /// parked or start mark once each; `Ready` is remembered; `Exit` becomes
    /// [`SpareNote::Gone`] and never reaches `user_event` (an `Exit` revokes a
    /// pending update); anything else is dropped.
    pub(crate) fn decide(&self, wake: Wake) -> bool {
        if self.live.load(Ordering::Acquire) {
            let owner = WindowId(self.window.load(Ordering::Acquire));
            let wake = match wake {
                Wake::Output { session, .. } => Wake::Output {
                    session,
                    window: owner,
                },
                Wake::Ready { session, .. } => Wake::Ready {
                    session,
                    window: owner,
                },
                Wake::Exit { session, .. } => Wake::Exit {
                    session,
                    window: owner,
                },
                other => other,
            };
            return self.send(wake);
        }
        match wake {
            Wake::Output { .. } => {
                if self.witness.parked_seen.get().is_some()
                    && !self.parked_noted.swap(true, Ordering::AcqRel)
                {
                    self.send(Wake::SpareNote {
                        id: self.id,
                        note: SpareNote::Parked,
                    });
                }
                if self.witness.start_seen.get().is_some()
                    && !self.start_noted.swap(true, Ordering::AcqRel)
                {
                    self.send(Wake::SpareNote {
                        id: self.id,
                        note: SpareNote::AtStart,
                    });
                }
                false
            }
            Wake::Ready { .. } => {
                self.reader_started.store(true, Ordering::Release);
                true
            }
            Wake::Exit { .. } => {
                self.exited.store(true, Ordering::Release);
                self.send(Wake::SpareNote {
                    id: self.id,
                    note: SpareNote::Gone,
                });
                true
            }
            _ => true,
        }
    }

    /// The reader's post for this spare.
    pub(crate) fn post(self: &Arc<Self>) -> crate::spawn::WakePost {
        let link = Arc::clone(self);
        Arc::new(move |wake| link.decide(wake))
    }
}

/// What a spare's factory copy carries ([`crate::spawn::SessionFactory::spare`]).
#[derive(Clone)]
pub(crate) struct SpareSpawn {
    pub(crate) link: Arc<SpareLink>,
}

/// A spare's host callbacks (design §5.2 step 4): the bell, notifications,
/// clipboard, kitty file reads and window operations only taint it, and the
/// clipboard query is revoked, so the reader never prefetches the pasteboard.
/// Adoption replaces them with the cold path's own (`spawn::arm_host_reach`).
pub(crate) fn wire_spare_taint(term: &Arc<Mutex<Terminal>>, link: &Arc<SpareLink>) {
    use aterm_core::terminal::ClipboardAccess;
    let mut t = crate::term_lock(term);
    t.revoke_clipboard_access(ClipboardAccess::Query);
    let l = Arc::clone(link);
    t.set_bell_callback(move || l.taint(taint::BELL));
    let l = Arc::clone(link);
    t.set_notification_callback(move |_| l.taint(taint::NOTIFY));
    let l = Arc::clone(link);
    t.set_advanced_notification_callback(move |_| l.taint(taint::NOTIFY));
    let l = Arc::clone(link);
    t.set_clipboard_callback(move |_| {
        l.taint(taint::CLIPBOARD);
        None
    });
    let l = Arc::clone(link);
    t.set_kitty_file_resolver(move |_, _| {
        l.taint(taint::KITTY);
        None
    });
    #[cfg(target_os = "linux")]
    {
        let l = Arc::clone(link);
        t.set_window_callback(move |_| {
            l.taint(taint::WINDOW_OP);
            None
        });
    }
}

// ─── Control and askpass (design §5.4) ───

/// The live spare's sid and shell pid, consulted only on a request's
/// not-found path and by askpass: live sessions pay nothing.
static SPARE_SIDS: std::sync::RwLock<Option<(aterm_session::SessionId, i32, Arc<SpareLink>)>> =
    std::sync::RwLock::new(None);

fn publish_spare_sid(entry: Option<(aterm_session::SessionId, i32, Arc<SpareLink>)>) {
    *SPARE_SIDS.write().unwrap_or_else(|p| p.into_inner()) = entry;
}

/// Whether the client this thread serves runs inside the session led by
/// `leader` (its `getsid`).
fn peer_inside(leader: i32) -> bool {
    // Read the actual writer now, under the serving lane's scoped direct
    // connection. Binary headers and handed-off subscriptions use the same
    // context; inherited bridge descriptors establish no local pid lineage.
    let peer = crate::control::serving_peer_pid().and_then(|pid| i32::try_from(pid).ok());
    #[cfg(unix)]
    {
        // SAFETY: `getsid` reads the process table; any pid is a valid argument.
        leader > 0 && peer.is_some_and(|pid| unsafe { libc::getsid(pid) } == leader)
    }
    #[cfg(not(unix))]
    {
        let _ = (peer, leader);
        false
    }
}

/// A control request named no registered session: when it named the spare —
/// its sid, which nobody outside it knows, or its reserved local id from a
/// client inside it — the spare is tainted. The caller answers `ERR no such
/// session` either way.
pub(crate) fn control_refusal(sid: Option<&aterm_session::SessionId>, local: Option<u64>) {
    let guard = SPARE_SIDS.read().unwrap_or_else(|p| p.into_inner());
    if let Some((spare_sid, pid, link)) = guard.as_ref()
        && (sid == Some(spare_sid) || (local == Some(link.id) && peer_inside(*pid)))
    {
        link.taint(taint::CONTROL);
    }
}

/// A control request resolved to a registered session: one from a client
/// inside it, before its start mark, is its rc's and is counted against its
/// generation. A driver polling the new tab from outside counts nothing.
pub(crate) fn note_control(witness: &Witness) {
    if witness.start_seen.get().is_none()
        && let Some((pid, _)) = witness.shell.get()
        && peer_inside(*pid)
    {
        witness.early_control.fetch_add(1, Ordering::Relaxed);
    }
}

/// An askpass request whose invoker's session is led by `invoker_sid_leader`
/// (`getsid` of the invoker): refused when that is the spare (and the spare is
/// tainted); counted for an observed session before its start mark. The askpass
/// lane is macOS's (`control_askpass`).
#[cfg(target_os = "macos")]
pub(crate) fn askpass_refused(invoker_pid: i32) -> bool {
    #[cfg(unix)]
    // SAFETY: `getsid` reads the process table; any pid is a valid argument.
    let leader = unsafe { libc::getsid(invoker_pid) };
    #[cfg(not(unix))]
    let leader = invoker_pid;
    if leader <= 0 {
        return false;
    }
    {
        let guard = SPARE_SIDS.read().unwrap_or_else(|p| p.into_inner());
        if let Some((_, pid, link)) = guard.as_ref()
            && *pid == leader
        {
            link.taint(taint::ASKPASS);
            return true;
        }
    }
    let observed = OBSERVED.lock().unwrap_or_else(|p| p.into_inner());
    for (pid, w) in observed.iter() {
        if *pid == leader
            && let Some(w) = w.upgrade()
            && w.start_seen.get().is_none()
        {
            w.askpass.fetch_add(1, Ordering::Relaxed);
        }
    }
    false
}

/// Sessions whose start record the gate waits for, by shell pid (askpass's
/// lookup). Pruned as observations are evaluated.
static OBSERVED: Mutex<Vec<(i32, std::sync::Weak<Witness>)>> = Mutex::new(Vec::new());

// ─── The rc generation (design §7.1) ───

/// What an rc generation is computed from, captured on the main thread (cheap)
/// and stamped off it (`$HOME` may be a network volume).
#[derive(Clone, Debug)]
pub(crate) struct GenInputs {
    pub(crate) shell: aterm_core::shell_integration::ShellType,
    /// The resolved shell binary.
    pub(crate) shell_path: PathBuf,
    /// `$HOME`.
    pub(crate) home: PathBuf,
    /// The launch environment's ZDOTDIR, else `$HOME`.
    pub(crate) launch_zdotdir: PathBuf,
    /// [`crate::spawn::SessionFactory::spawn_fingerprint`].
    pub(crate) fingerprint: u64,
    /// The containment mode's level.
    pub(crate) containment: u8,
}

/// The files a shell of `inputs.shell` reads at start, for a final rc folder
/// `z` (the last reported `AtermStartRc`, else the launch ZDOTDIR or HOME).
fn rc_paths(inputs: &GenInputs, z: Option<&Path>) -> Vec<PathBuf> {
    use aterm_core::shell_integration::ShellType;
    let mut paths = Vec::new();
    match inputs.shell {
        ShellType::Zsh => {
            for f in ["/etc/zshenv", "/etc/zprofile", "/etc/zshrc", "/etc/zlogin"] {
                paths.push(PathBuf::from(f));
            }
            let h = &inputs.launch_zdotdir;
            paths.push(h.join(".zshenv"));
            let z = z.unwrap_or(h);
            for f in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
                paths.push(z.join(f));
            }
        }
        _ => {
            for f in ["/etc/profile", "/etc/bashrc"] {
                paths.push(PathBuf::from(f));
            }
            for f in [".bash_profile", ".bash_login", ".profile", ".bashrc"] {
                paths.push(inputs.home.join(f));
            }
        }
    }
    let shell_d = inputs.home.join(".aterm").join("shell.d");
    if let Ok(entries) = std::fs::read_dir(&shell_d) {
        let mut names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        names.sort();
        paths.extend(names);
    }
    paths.push(shell_d);
    paths.push(inputs.shell_path.clone());
    paths
}

/// The rc generation: a metadata stamp over every startup file, each shell.d
/// entry, the shell binary, the script set, the factory and the containment
/// mode (design §7.1). HISTFILE is not in it: the release re-reads history.
pub(crate) fn rc_generation(inputs: &GenInputs, z: Option<&Path>) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "aterm-spare-generation-v1".hash(&mut h);
    crate::config_watcher::path_feed_stamp(&rc_paths(inputs, z)).hash(&mut h);
    aterm_core::shell_integration::script_set_address().hash(&mut h);
    inputs.fingerprint.hash(&mut h);
    inputs.containment.hash(&mut h);
    // Never 0: 0 means "no generation" in the mirror.
    h.finish().max(1)
}

// ─── The learned gate (design §7.1), on the aterm-spare thread ───

/// One generation's record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GenRecord {
    pub(crate) hash: u64,
    pub(crate) streak: u32,
    /// Unix seconds; `u64::MAX` until the generation changes; 0 none.
    pub(crate) ineligible_until: u64,
    /// Unix seconds of backoff failures in the last day.
    pub(crate) failures: Vec<u64>,
    /// The longest qualifying rc run, in ms.
    pub(crate) max_rc_ms: u32,
    /// The deterministic reason, when `ineligible_until` is `u64::MAX`.
    pub(crate) reason: String,
    /// Last touched (unix seconds), for keeping the newest [`GATE_KEEP`].
    pub(crate) touched: u64,
}

/// A visible run's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sample {
    Clean { rc_ms: u32 },
    Unclean,
    Inconclusive,
}

/// The per-generation gate: streaks, backoffs, persistence.
#[derive(Default)]
pub(crate) struct GateBook {
    gens: Vec<GenRecord>,
    path: Option<PathBuf>,
    loaded: bool,
}

/// Unix seconds now.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Parse one complete persisted snapshot, or discard all of its eligibility.
fn parse_gate_records(text: &str) -> Option<Vec<GenRecord>> {
    let mut gens: Vec<GenRecord> = Vec::new();
    for line in text.lines() {
        if gens.len() >= GATE_KEEP {
            return None;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let [hash, streak, until, failures, max_rc, touched, reason] = f.as_slice() else {
            return None;
        };
        let hash = u64::from_str_radix(hash, 16).ok()?;
        if gens.iter().any(|g| g.hash == hash) || reason.chars().any(char::is_control) {
            return None;
        }
        let failures = if failures.is_empty() {
            Vec::new()
        } else {
            failures
                .split(',')
                .map(str::parse)
                .collect::<Result<Vec<u64>, _>>()
                .ok()?
        };
        gens.push(GenRecord {
            hash,
            streak: streak.parse().ok()?,
            ineligible_until: until.parse().ok()?,
            failures,
            max_rc_ms: max_rc.parse().ok()?,
            reason: (*reason).to_owned(),
            touched: touched.parse().ok()?,
        });
    }
    Some(gens)
}

impl GateBook {
    pub(crate) fn at(path: Option<PathBuf>) -> Self {
        Self {
            gens: Vec::new(),
            path,
            loaded: false,
        }
    }

    /// Read the file once, lazily, on the first decision.
    fn load(&mut self) {
        if std::mem::replace(&mut self.loaded, true) {
            return;
        }
        let text = self.path.as_ref().and_then(|path| {
            crate::seamless::read_regular_capped(path, GATE_FILE_MAX)
                .and_then(|bytes| String::from_utf8(bytes).ok())
        });
        let Some(text) = text else { return };
        // A malformed cache is unknown, never partial evidence for running rc files unseen.
        let Some(gens) = parse_gate_records(&text) else {
            return;
        };
        self.gens = gens;
    }

    /// Write the file (temp + rename), keeping the newest [`GATE_KEEP`].
    fn persist(&mut self) {
        self.gens.sort_by_key(|g| std::cmp::Reverse(g.touched));
        self.gens.truncate(GATE_KEEP);
        let Some(path) = &self.path else { return };
        let mut text = String::new();
        for g in &self.gens {
            let failures: Vec<String> = g.failures.iter().map(u64::to_string).collect();
            let reason: String = g.reason.chars().filter(|c| !c.is_control()).collect();
            text.push_str(&format!(
                "{:016x}\t{}\t{}\t{}\t{}\t{}\t{reason}\n",
                g.hash,
                g.streak,
                g.ineligible_until,
                failures.join(","),
                g.max_rc_ms,
                g.touched
            ));
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        // A collision is not ours to truncate, follow, or remove.
        let Ok(mut file) = options.open(&tmp) else {
            return;
        };
        use std::io::Write as _;
        let written = file.write_all(text.as_bytes());
        drop(file);
        if written.is_err() || std::fs::rename(&tmp, path).is_err() {
            // Only this successful exclusive creation established ownership.
            let _ = std::fs::remove_file(&tmp);
        }
    }

    fn entry(&mut self, hash: u64) -> &mut GenRecord {
        self.load();
        let now = unix_now();
        let at = match self.gens.iter().position(|g| g.hash == hash) {
            Some(at) => at,
            None => {
                self.gens.push(GenRecord {
                    hash,
                    streak: 0,
                    ineligible_until: 0,
                    failures: Vec::new(),
                    max_rc_ms: 0,
                    reason: String::new(),
                    touched: now,
                });
                self.gens.len() - 1
            }
        };
        let g = &mut self.gens[at];
        g.touched = now;
        g
    }

    /// Fold one visible run of generation `hash` in.
    pub(crate) fn sample(&mut self, hash: u64, sample: Sample) {
        let g = self.entry(hash);
        match sample {
            Sample::Clean { rc_ms } => {
                g.streak = g.streak.saturating_add(1);
                g.max_rc_ms = g.max_rc_ms.max(rc_ms);
            }
            Sample::Unclean => g.streak = 0,
            Sample::Inconclusive => {}
        }
        self.persist();
    }

    /// A spare of generation `hash` failed.
    pub(crate) fn fail(&mut self, hash: u64, verdict: Verdict) {
        self.fail_at(hash, verdict, unix_now());
    }

    /// [`Self::fail`] at unix second `now`.
    pub(crate) fn fail_at(&mut self, hash: u64, verdict: Verdict, now: u64) {
        let g = self.entry(hash);
        g.streak = 0;
        match verdict {
            Verdict::Ok => {}
            Verdict::Deterministic(reason) => {
                g.ineligible_until = u64::MAX;
                reason.clone_into(&mut g.reason);
            }
            Verdict::Backoff(reason) => {
                g.failures
                    .retain(|t| now.saturating_sub(*t) < BACKOFF_SECOND.as_secs());
                let hold = if g.failures.is_empty() {
                    BACKOFF_FIRST
                } else {
                    BACKOFF_SECOND
                };
                g.failures.push(now);
                g.ineligible_until = now.saturating_add(hold.as_secs());
                reason.clone_into(&mut g.reason);
            }
        }
        self.persist();
    }

    /// Whether generation `hash` may run unseen now.
    pub(crate) fn eligible(&mut self, hash: u64) -> bool {
        self.eligible_at(hash, unix_now())
    }

    /// [`Self::eligible`] at unix second `now`.
    pub(crate) fn eligible_at(&mut self, hash: u64, now: u64) -> bool {
        self.load();
        self.gens
            .iter()
            .find(|g| g.hash == hash)
            .is_some_and(|g| g.streak >= CLEAN_STREAK && now >= g.ineligible_until)
    }

    /// The metrics word for `hash` (design §10).
    pub(crate) fn report(&mut self, hash: u64) -> String {
        self.load();
        let now = unix_now();
        match self.gens.iter().find(|g| g.hash == hash) {
            None => format!("learning 0/{CLEAN_STREAK}"),
            Some(g) if g.ineligible_until == u64::MAX => format!("ineligible:{}", g.reason),
            Some(g) if now < g.ineligible_until => "backoff".to_owned(),
            Some(g) if g.streak >= CLEAN_STREAK => "eligible".to_owned(),
            Some(g) => format!("learning {}/{CLEAN_STREAK}", g.streak),
        }
    }

    /// `T_READY` for `hash`: four times its longest qualifying rc run, in
    /// [1 s, 8 s].
    pub(crate) fn t_ready(&mut self, hash: u64) -> Duration {
        self.load();
        let rc = self
            .gens
            .iter()
            .find(|g| g.hash == hash)
            .map_or(0, |g| g.max_rc_ms);
        Duration::from_millis(u64::from(rc).saturating_mul(4)).clamp(T_READY_MIN, T_READY_MAX)
    }
}

// ─── The escape scan (design §7.1) ───

/// A scan's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scan {
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "only the macOS escape scan observes a run")
    )]
    Clean,
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "only the macOS escape scan observes a run")
    )]
    Escaped,
    /// The kernel did not provide a complete, stable observation.
    Unknown,
    RateLimited,
}

/// `proc_pidinfo(PROC_PIDT_BSDINFOWITHUNIQID)`'s record (`<sys/proc_info.h>`):
/// the BSD info and the unique ids XNU keeps for a process and its parent.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct BsdInfoWithUniqId {
    pbsd: libc::proc_bsdinfo,
    p_uuid: [u8; 16],
    p_uniqueid: u64,
    p_puniqueid: u64,
    p_idversion: i32,
    p_orig_ppidversion: i32,
    p_reserve2: u64,
    p_reserve3: u64,
}

#[cfg(target_os = "macos")]
const PROC_PIDT_BSDINFOWITHUNIQID: libc::c_int = 18;

/// launchd's pid version (`PROC_PIDUNIQIDENTIFIERINFO` of pid 1, which any
/// process may read): every process launchd itself starts names it as its
/// original parent's (`p_orig_ppidversion`), and an orphan launchd adopts
/// keeps its own original parent's through the adoption and any later exec
/// (measured on Darwin 27: Spotlight's `mdworker_shared` and `xpcproxy` name
/// launchd's, 1026; an orphan that execs once adopted names its launcher's,
/// though its `p_puniqueid` then reads as launchd's). `None` when unreadable.
#[cfg(target_os = "macos")]
fn launchd_idversion() -> Option<i32> {
    /// `<sys/proc_info.h>`'s `proc_uniqidentifierinfo`.
    #[repr(C)]
    struct UniqIdentifierInfo {
        p_uuid: [u8; 16],
        p_uniqueid: u64,
        p_puniqueid: u64,
        p_idversion: i32,
        p_orig_ppidversion: i32,
        p_reserve2: u64,
        p_reserve3: u64,
    }
    const PROC_PIDUNIQIDENTIFIERINFO: libc::c_int = 17;
    let mut info = std::mem::MaybeUninit::<UniqIdentifierInfo>::zeroed();
    let size = std::mem::size_of::<UniqIdentifierInfo>() as libc::c_int;
    // SAFETY: `info` is `size` writable bytes; the kernel writes at most that
    // many and returns how many it wrote.
    let n = unsafe {
        libc::proc_pidinfo(
            1,
            PROC_PIDUNIQIDENTIFIERINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    // SAFETY: zero-initialised plain data, and a full record was written.
    (n == size).then(|| unsafe { info.assume_init() }.p_idversion)
}

#[cfg(target_os = "macos")]
fn bsdinfo_uniq(pid: i32) -> Option<BsdInfoWithUniqId> {
    let mut info = std::mem::MaybeUninit::<BsdInfoWithUniqId>::zeroed();
    let size = std::mem::size_of::<BsdInfoWithUniqId>() as libc::c_int;
    // SAFETY: `info` is `size` writable bytes; the kernel writes at most that
    // many and returns how many it wrote.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDT_BSDINFOWITHUNIQID,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    // SAFETY: zero-initialised plain data, and a full record was written.
    (n == size).then(|| unsafe { info.assume_init() })
}

/// Whether `pid` positively reads as a zombie (`SZOMB`). Beginning exit
/// does not establish resource teardown. An empty process-table response
/// remains uncertain rather than proving the absence of resource holders.
#[cfg(target_os = "macos")]
fn exited(pid: i32) -> bool {
    let (rc, record, size) = process_table_record(pid);
    exited_observation(rc, &record, size)
}

/// Whether `pid`'s process-table record says it was born before `when`. One
/// older than the shell is no process of its run, which only a process started
/// after the shell can be (design §7.1), whatever its exit state: the record
/// outlives the `proc_pidinfo` one through an exit, so it settles a process
/// wedged mid-exit (measured on m3: a `bash` in `E` and its `sleep` in `U` for
/// 18 days, which left every scan `Unknown` and the warm spare cold for good).
/// An absent or short record says nothing.
#[cfg(target_os = "macos")]
fn born_before(pid: i32, when: std::time::SystemTime) -> bool {
    let (rc, record, size) = process_table_record(pid);
    born_before_observation(rc, &record, size, when)
}

/// The process table's record of `pid`: `sysctl`'s code, the record, and how
/// many bytes of it the kernel wrote.
#[cfg(target_os = "macos")]
fn process_table_record(pid: i32) -> (libc::c_int, [u8; 1024], usize) {
    // `KERN_PROC`, `KERN_PROC_PID` (`<sys/sysctl.h>`) and the LP64
    // `kinfo_proc` response, which the workspace's libc does not export.
    const KERN_PROC: libc::c_int = 14;
    const KERN_PROC_PID: libc::c_int = 1;
    let mut mib = [libc::CTL_KERN, KERN_PROC, KERN_PROC_PID, pid];
    let mut record = [0u8; 1024];
    let mut size: libc::size_t = record.len();
    // SAFETY: `record` holds `size` writable bytes and `size` says so; the
    // kernel writes at most that many and updates `size` to what it wrote.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            record.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc, record, size)
}

/// Interpret the process-table response's start time (`kp_proc.p_starttime`,
/// the LP64 record's first field: a `timeval`, `tv_sec` an i64 then `tv_usec` an
/// i32) without performing a process query.
#[cfg(target_os = "macos")]
fn born_before_observation(
    rc: libc::c_int,
    record: &[u8],
    size: usize,
    when: std::time::SystemTime,
) -> bool {
    const P_STAT: usize = 36;
    if rc != 0 || size > record.len() || size <= P_STAT {
        return false;
    }
    let (Some(sec), Some(usec)) = (record.get(..8), record.get(8..12)) else {
        return false;
    };
    let (Ok(sec), Ok(usec)) = (<[u8; 8]>::try_from(sec), <[u8; 4]>::try_from(usec)) else {
        return false;
    };
    let (Ok(sec), Ok(usec)) = (
        u64::try_from(i64::from_ne_bytes(sec)),
        u64::try_from(i32::from_ne_bytes(usec)),
    ) else {
        return false;
    };
    std::time::UNIX_EPOCH + Duration::from_secs(sec) + Duration::from_micros(usec) < when
}

/// Interpret the process-table response without performing a process query.
#[cfg(target_os = "macos")]
fn exited_observation(rc: libc::c_int, record: &[u8], size: usize) -> bool {
    const P_STAT: usize = 36;
    if rc != 0 || size > record.len() || size <= P_STAT {
        return false;
    }
    u32::from(record[P_STAT]) == libc::SZOMB
}

// `<sys/proc_info.h>` / `<libproc.h>`: the uid-filtered listing returns
// bytes, unlike proc_listallpids' pid count. Other users' protected processes
// cannot turn a complete observation of our uid into an unreadable one.
#[cfg(target_os = "macos")]
const PROC_UID_ONLY: u32 = 4;
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn proc_listpids(
        kind: u32,
        typeinfo: u32,
        buffer: *mut libc::c_void,
        bytes: libc::c_int,
    ) -> libc::c_int;
}

/// The escape scan (design §7.1): processes of our uid started between `s_pid`'s
/// start and `until` that either (a) were reparented to launchd and are younger
/// than the shell, or (b) still descend from the shell but left its session — and whose environment names session `sid`
/// (`ATERM_PARENT_SESSION_ID`, which every process a session starts inherits)
/// or cannot be read. Such a process outlives the tab it was started in: `eval
/// $(ssh-agent)`, a tmux server, a `setsid`/`nohup` daemon.
#[cfg(target_os = "macos")]
pub(crate) fn escape_scan(
    s_pid: i32,
    sid: &str,
    from: std::time::SystemTime,
    until: std::time::SystemTime,
) -> Scan {
    rescan_while_unknown(SCAN_ATTEMPTS, || {
        escape_scan_among(s_pid, sid, from, until, uid_pids, bsdinfo_uniq)
    })
}

/// Whole-uid listings an [`escape_scan`] takes before it settles for
/// `Unknown`.
#[cfg(target_os = "macos")]
const SCAN_ATTEMPTS: usize = 3;

/// The first verdict of up to `attempts` scans that is not `Unknown`. A
/// process reaped between a listing and its read is settled inside the scan
/// ([`escape_scan_among`]); a scan still incomplete (the shell or a listing
/// unreadable for a moment) is taken again. A later scan judges the same
/// window, so it is as true a verdict, and a found escape still wins the scan
/// that found it.
#[cfg(target_os = "macos")]
fn rescan_while_unknown(attempts: usize, mut scan: impl FnMut() -> Scan) -> Scan {
    for _ in 0..attempts {
        match scan() {
            Scan::Unknown => {}
            verdict => return verdict,
        }
    }
    Scan::Unknown
}

/// Every pid of `uid`, or `None` when the listing cannot be whole (a full
/// buffer may have omitted a process born after the size query).
#[cfg(target_os = "macos")]
fn uid_pids(uid: u32) -> Option<Vec<i32>> {
    const PID_BYTES: usize = std::mem::size_of::<i32>();
    const MAX_SCAN_PIDS: usize = 1 << 20;
    // SAFETY: null asks for the required byte capacity only, filtered by uid.
    let need = unsafe { proc_listpids(PROC_UID_ONLY, uid, std::ptr::null_mut(), 0) };
    let room = usize::try_from(need)
        .ok()
        .filter(|n| *n > 0 && *n % PID_BYTES == 0)
        .and_then(|n| (n / PID_BYTES).checked_add(64))
        .filter(|n| *n <= MAX_SCAN_PIDS)?;
    let mut pids = vec![0i32; room];
    let bytes = libc::c_int::try_from(room * PID_BYTES).ok()?;
    // SAFETY: `pids` provides `bytes` writable bytes of i32 slots.
    let got = unsafe { proc_listpids(PROC_UID_ONLY, uid, pids.as_mut_ptr().cast(), bytes) };
    let got = usize::try_from(got)
        .ok()
        .filter(|n| *n > 0 && *n % PID_BYTES == 0 && *n < bytes as usize)?;
    pids.truncate(got / PID_BYTES);
    Some(pids)
}

/// Later listings an [`escape_scan_among`] takes to settle the pids its first
/// listing named but could not read, before it calls the scan incomplete. The
/// first is taken at once; the pause before each after it doubles from
/// [`SCAN_SETTLE_PAUSE`], 511 ms in all, for a process that has begun to exit
/// to finish.
#[cfg(target_os = "macos")]
const SCAN_SETTLE_LISTINGS: usize = 10;

/// The pause before the second settling listing.
#[cfg(target_os = "macos")]
const SCAN_SETTLE_PAUSE: Duration = Duration::from_millis(1);

/// [`escape_scan`] over the pids `list` names for our uid, each read by
/// `read_pid`: the host's whole uid and [`bsdinfo_uniq`] in production; in a
/// test, a fixture's own processes, so a verdict about one process tree is
/// never another test's process exiting mid-listing, and the record a kernel
/// race leaves (a process read before its parent's exit was done).
///
/// Only the first listing's pids are judged. One it names that cannot be
/// read and is not positively a zombie is settled by a later listing (up to
/// [`SCAN_SETTLE_LISTINGS`]): a listing holds every live process and every zombie
/// of the uid (measured on Darwin 27: an unreaped child is listed, a reaped
/// one is not), so a pid a later listing no longer names was reaped and
/// holds nothing, and one it still names is read again. A pid that stays
/// unreadable leaves the scan incomplete (`Unknown`). A busy host's churn
/// reaps a few of a scan's pids before they are read (measured: up to 19 of
/// 620 under 12 fork-exec loops at load 30) and every fresh whole scan
/// meets new ones; the first listing's own settle in one later listing,
/// except a process still exiting (`P_WEXIT`, read as nothing yet not a
/// zombie), which a later listing names until its exit is done (measured
/// under six loops of 200 MB processes exiting, at load 41-50: 62 of 136
/// scans took 2-4 later listings; none was left unsettled).
///
/// A pid the scan drops as exited (a zombie, or reaped) can be the parent a
/// record read before its exit was done still names: XNU hands an exiting
/// process's children to launchd before it is a zombie, so such a record is
/// stale (a daemon whose launcher was exiting at the read would be judged
/// still parented, and its escape unseen). Every record that names a dropped
/// parent is read again; one that cannot be read again as the same process,
/// or still names a dropped parent, leaves the scan incomplete.
#[cfg(target_os = "macos")]
fn escape_scan_among(
    s_pid: i32,
    sid: &str,
    from: std::time::SystemTime,
    until: std::time::SystemTime,
    list: impl FnMut(u32) -> Option<Vec<i32>>,
    read_pid: impl FnMut(i32) -> Option<BsdInfoWithUniqId>,
) -> Scan {
    escape_scan_with_owner(s_pid, sid, from, until, list, read_pid, |pid, sid| {
        u32::try_from(pid)
            .ok()
            .and_then(atpkg::caller_shell::process_args)
            .map(|args| {
                args.env.is_empty()
                    || args.env_var(aterm_types::domain::ENV_PARENT_SESSION_ID) == Some(sid)
            })
    })
}

/// The same scan with the environment's ownership answer supplied. `None` is
/// unreadable, not evidence the process exited. The production wrapper above
/// supplies the ordinary process-args lookup.
#[cfg(target_os = "macos")]
fn escape_scan_with_owner(
    s_pid: i32,
    sid: &str,
    from: std::time::SystemTime,
    until: std::time::SystemTime,
    mut list: impl FnMut(u32) -> Option<Vec<i32>>,
    mut read_pid: impl FnMut(i32) -> Option<BsdInfoWithUniqId>,
    mut read_owner: impl FnMut(i32, &str) -> Option<bool>,
) -> Scan {
    if s_pid <= 1 {
        return Scan::Unknown;
    }
    // SAFETY: `getuid` is a side-effect-free getter.
    let uid = unsafe { libc::getuid() };
    let mut read = |p: i32| {
        read_pid(p).filter(|i| p > 0 && i.pbsd.pbi_pid == p as u32 && i.pbsd.pbi_uid == uid)
    };
    let Some(shell) = read(s_pid) else {
        return Scan::Unknown;
    };
    let shell_start = std::time::UNIX_EPOCH
        + Duration::from_secs(shell.pbsd.pbi_start_tvsec)
        + Duration::from_micros(shell.pbsd.pbi_start_tvusec);
    let start = shell_start.max(from);
    let Some(pids) = list(uid) else {
        return Scan::Unknown;
    };
    let mut infos = Vec::with_capacity(pids.len());
    let mut unsettled = Vec::new();
    let mut older = Vec::new();
    for &p in &pids {
        match read(p) {
            Some(info) => infos.push((p, info)),
            // A positively observed zombie can be skipped.
            None if exited(p) => {}
            // Born before the shell: none of the run's, live, exiting or a
            // zombie, and no parent a record of the run can name.
            None if born_before(p, shell_start) => older.push(p),
            // Beginning exit or an empty process-table response is not yet
            // an exit: a later listing settles it.
            None => unsettled.push(p),
        }
    }
    let mut pause = Duration::ZERO;
    for _ in 0..SCAN_SETTLE_LISTINGS {
        if unsettled.is_empty() {
            break;
        }
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
        pause = pause.saturating_mul(2).max(SCAN_SETTLE_PAUSE);
        let Some(listed) = list(uid) else {
            break;
        };
        unsettled.retain(|&p| {
            if !listed.contains(&p) {
                return false;
            }
            match read(p) {
                Some(info) => {
                    infos.push((p, info));
                    false
                }
                None => !exited(p),
            }
        });
    }
    // The first listing's pids the scan dropped as exited (positively zombies,
    // or reaped), and every record naming one as its parent, which was read
    // before that parent's exit handed it to launchd: read it again.
    let held: std::collections::HashSet<i32> = infos.iter().map(|(p, _)| *p).collect();
    let gone: std::collections::HashSet<i32> = pids
        .into_iter()
        .filter(|p| !held.contains(p) && !unsettled.contains(p) && !older.contains(p))
        .collect();
    let mut stale = false;
    for (p, info) in &mut infos {
        if !gone.contains(&(info.pbsd.pbi_ppid as i32)) {
            continue;
        }
        match read(*p) {
            Some(fresh)
                if fresh.p_uniqueid == info.p_uniqueid
                    && !gone.contains(&(fresh.pbsd.pbi_ppid as i32)) =>
            {
                *info = fresh;
            }
            _ => stale = true,
        }
    }
    // Keep known positives despite unrelated churn: a pid still unsettled, or
    // a record that cannot be read again, makes an otherwise clean scan
    // incomplete, never an escape unseen.
    let mut incomplete = stale || !unsettled.is_empty();
    if !infos
        .iter()
        .any(|(p, i)| *p == s_pid && i.p_uniqueid == shell.p_uniqueid)
    {
        incomplete = true;
    }
    let parent_of = |pid: i32| -> Option<i32> {
        infos
            .iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, i)| i.pbsd.pbi_ppid as i32)
    };
    let launchd = launchd_idversion();
    // The run's processes are in the shell's session or in one a process of
    // the run made (`setsid`): a session is entered only by birth from one of
    // its members, so each member descends from its leader. A session with a
    // live member born before the shell is no session of the run, and an
    // orphan in one is the busy host's (the pid naming a live session is
    // never handed out again, measured over two pid wraps, so the id is that
    // session's). Read only when the shell leads its own session, as a tab's
    // does, and only once an orphan outside it needs it.
    // SAFETY: `getsid` reads the process table.
    let leads_session = unsafe { libc::getsid(s_pid) } == s_pid;
    let mut older_sessions: Option<std::collections::HashSet<i32>> = None;
    // Whether `leader`, the live leader of its session, descends from a process
    // born before the shell without passing through the shell. The run's own
    // `setsid` leader is under the shell, or launchd's once its parent is gone
    // (Darwin has no subreaper), so a session led from outside is a stranger's
    // (another terminal's tab, born after this shell). Each parent must be
    // older than its child: a pid reused since its record was read ends the
    // walk with no verdict, as does a parent this uid's listing did not read.
    let led_from_outside = |leader: i32| -> bool {
        // SAFETY: `getsid` reads the process table.
        if unsafe { libc::getsid(leader) } != leader {
            return false;
        }
        let record = |pid: i32| infos.iter().find(|(p, _)| *p == pid).map(|(_, i)| i);
        let (mut at, mut child) = match record(leader) {
            Some(info) => (leader, info),
            None => return false,
        };
        for _ in 0..64 {
            if at == s_pid {
                return false;
            }
            if child.p_uniqueid < shell.p_uniqueid {
                return true;
            }
            let up = child.pbsd.pbi_ppid as i32;
            match record(up) {
                Some(parent) if up > 1 && parent.p_uniqueid < child.p_uniqueid => {
                    (at, child) = (up, parent);
                }
                _ => return false,
            }
        }
        false
    };
    let mut escaped = false;
    for (pid, info) in &infos {
        if *pid == s_pid {
            continue;
        }
        let started = std::time::UNIX_EPOCH
            + Duration::from_secs(info.pbsd.pbi_start_tvsec)
            + Duration::from_micros(info.pbsd.pbi_start_tvusec);
        if started < start || started > until {
            continue;
        }
        // Adopted by launchd and younger than the shell. One launchd started
        // itself (a Spotlight worker indexing a busy host's files, an XPC
        // service) was never anyone's orphan; any other's original parent is
        // not launchd's ([`launchd_idversion`]). (Which one it was is not
        // read: XNU re-points an adopted orphan's `p_puniqueid` at launchd's
        // when it next execs, so the environment check below says whose.)
        let reparented = info.pbsd.pbi_ppid == 1
            && info.p_uniqueid > shell.p_uniqueid
            && launchd.is_none_or(|v| info.p_orig_ppidversion != v);
        let attached = {
            let mut at = *pid;
            let mut attached = false;
            for _ in 0..64 {
                match parent_of(at) {
                    Some(p) if p == s_pid => {
                        attached = true;
                        break;
                    }
                    Some(p) if p > 1 => at = p,
                    _ => break,
                }
            }
            attached
        };
        if !(reparented || attached) {
            continue;
        }
        // Its session, and below its environment, are read after its record:
        // one gone since then (a busy host's, or the shell's own short-lived
        // child) holds nothing and is no escape, and its pid may already be
        // another's, so each read is confirmed against the record's birth.
        // SAFETY: `getsid` reads the process table.
        let session = unsafe { libc::getsid(*pid) };
        match read(*pid) {
            Some(now) if now.p_uniqueid == info.p_uniqueid => {}
            Some(_) => continue, // A positively observed replacement: this birth is gone.
            None => {
                if !exited(*pid) && !list(uid).is_some_and(|pids| !pids.contains(pid)) {
                    incomplete = true;
                }
                continue;
            }
        }
        if session < 0 {
            // Present, yet its session unreadable: no verdict on it.
            incomplete = true;
            continue;
        }
        let left_session = attached && session != s_pid;
        if !(reparented || left_session) {
            continue;
        }
        let in_session = session == s_pid;
        // An orphan in a session older than the shell (a busy host's
        // background job, adopted in the run's window), or in a younger one
        // led from outside the run (another tab's), is not the run's.
        if reparented && leads_session && !in_session && session > 0 && session != *pid {
            let older = older_sessions.get_or_insert_with(|| {
                infos
                    .iter()
                    .filter(|(p, i)| *p > 1 && i.p_uniqueid < shell.p_uniqueid)
                    .filter_map(|(p, i)| {
                        // SAFETY: `getsid` reads the process table.
                        let s = unsafe { libc::getsid(*p) };
                        (s > 0 && read(*p).is_some_and(|now| now.p_uniqueid == i.p_uniqueid))
                            .then_some(s)
                    })
                    .collect()
            });
            if older.contains(&session) || led_from_outside(session) {
                continue;
            }
        }
        // Whose it is: one still in the shell's session or its descendant line
        // is the shell's; else its environment says (an empty one is a withheld
        // one: Darwin 27 hands another process's `KERN_PROCARGS2` back without
        // its environment, so a daemon there counts against the run — the
        // window, the run alone, and the older-session rule above keep
        // strangers out).
        let names_sid = in_session
            || left_session
            || match read_owner(*pid, sid) {
                Some(owned) => owned,
                // Unreadable: the run's while the same birth is observed.
                // A missing identity is uncertain unless a positive zombie or
                // complete fresh listing settles that the old process is gone.
                None => match read(*pid) {
                    Some(now) => now.p_uniqueid == info.p_uniqueid,
                    None => {
                        if !exited(*pid) && !list(uid).is_some_and(|pids| !pids.contains(pid)) {
                            incomplete = true;
                        }
                        false
                    }
                },
            };
        if names_sid {
            escaped = true;
            break;
        }
    }
    if !read(s_pid).is_some_and(|now| now.p_uniqueid == shell.p_uniqueid) {
        return Scan::Unknown;
    }
    if escaped {
        Scan::Escaped
    } else if incomplete {
        Scan::Unknown
    } else {
        Scan::Clean
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn escape_scan(
    _s_pid: i32,
    _sid: &str,
    _from: std::time::SystemTime,
    _until: std::time::SystemTime,
) -> Scan {
    Scan::Unknown
}

/// SIGKILL every process in `pid`'s session (design §9: a warm spare's retire,
/// so a hung rc child dies and no user trap runs), the shell last. macOS:
/// the process table; elsewhere the shell's group.
pub(crate) fn kill_session(pid: i32) {
    if pid <= 1 {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: a null buffer asks for the count only.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if let Ok(count) = usize::try_from(count) {
            let mut pids = vec![0i32; count + 64];
            let bytes = (pids.len() * std::mem::size_of::<i32>()) as libc::c_int;
            // SAFETY: `pids` is `bytes` writable bytes.
            let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
            pids.truncate(usize::try_from(n).unwrap_or(0).min(pids.len()));
            for p in pids {
                // SAFETY: `getsid`/`kill` on a pid; a stale pid errs harmlessly,
                // and only members of the spare's own session are signalled.
                if p > 1 && p != pid && unsafe { libc::getsid(p) } == pid {
                    unsafe { libc::kill(p, libc::SIGKILL) };
                }
            }
        }
    }
    #[cfg(unix)]
    // SAFETY: the spare's shell leads its own group (forkpty); its pid is ours
    // and unreaped (the caller holds the session).
    unsafe {
        libc::killpg(pid, libc::SIGKILL);
    }
}

// ─── The aterm-spare thread (design §5.1) ───

/// A spare's spawn, as the thread runs it.
pub(crate) struct SpawnJob {
    pub(crate) id: u64,
    pub(crate) window: WindowId,
    pub(crate) rows: u16,
    pub(crate) cols: u16,
    pub(crate) cell_px: Option<(u16, u16)>,
    pub(crate) factory: crate::spawn::SessionFactory,
    pub(crate) proxy: EventLoopProxy<Wake>,
    pub(crate) cwd: Option<String>,
}

/// A spawned spare, back on the main thread ([`Wake::SpareSpawned`]).
pub(crate) struct SpareSpawned {
    pub(crate) id: u64,
    pub(crate) session: std::io::Result<Session>,
}

impl std::fmt::Debug for SpareSpawned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpareSpawned")
            .field("id", &self.id)
            .field("spawned", &self.session.as_ref().map(|_| ()))
            .finish()
    }
}

/// A visible rc run the gate learns from: a cold tab's, or a parked spare's
/// adoption (design §7.1).
pub(crate) struct Observation {
    pub(crate) witness: Arc<Witness>,
    pub(crate) sid: String,
    pub(crate) started_at: Instant,
    pub(crate) input0: Option<aterm_session::sink::DrivenInput>,
    pub(crate) host_effects0: u32,
    pub(crate) early_control0: u32,
    pub(crate) inputs: GenInputs,
}

/// A warm spare at its hold, for the thread to verify.
pub(crate) struct VerifyJob {
    pub(crate) id: u64,
    pub(crate) witness: Arc<Witness>,
    pub(crate) sid: String,
    pub(crate) inputs: GenInputs,
    pub(crate) generation: u64,
    /// When it was told `run`: its rc's run starts here.
    pub(crate) run_at: Instant,
    /// Where the verdict goes: the event loop.
    pub(crate) reply: crate::spawn::WakePost,
}

/// The thread's work.
pub(crate) enum Job {
    Spawn(Box<SpawnJob>),
    Observe(Box<Observation>),
    /// An observed session's start record arrived.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only a unix reader witnesses a start")
    )]
    Marked,
    Verify(Box<VerifyJob>),
    /// A spare of `generation` failed.
    Fail(u64, Verdict),
    /// Recompute the current generation (the factory or the shell changed).
    Restamp(GenInputs, Option<EventLoopProxy<Wake>>),
    /// Stamp the current generation from the disk now and mirror the gate for
    /// it, then answer: [`Shared::fresh`] takes the number and a gate note
    /// runs the main thread's tick. The mirror is otherwise only as fresh as
    /// its last stamp (an observation, a failure, a restamp), and a startup
    /// file edited since would run unseen and unproven: a promotion writes
    /// `run` only on a stamp taken after it asked.
    Freshen(u64),
    /// Drop a retired spare here, off the main thread; `kill` first SIGKILLs
    /// its whole session (a warm spare, whose rc may hold a child).
    Retire(Box<Session>, bool),
    /// Release the FIFOs a crashed aterm left ([`sweep_dead_fifos`]).
    Sweep,
    Stop(Sender<()>),
}

/// What the main thread reads of the gate, mirrored by the thread.
#[derive(Default)]
pub(crate) struct Shared {
    /// The current generation (0 before the first stamp).
    pub(crate) generation: AtomicU64,
    /// The last [`Job::Freshen`] answered: `generation` and `eligible` were
    /// stamped from the disk after it was asked.
    pub(crate) fresh: AtomicU64,
    /// It may run unseen.
    pub(crate) eligible: AtomicBool,
    /// `/etc/zshenv` exists: a parked zsh spare runs it before its hold, so
    /// parking needs the gate too.
    pub(crate) system_zshenv: AtomicBool,
    /// `T_READY` for the current generation, ms.
    pub(crate) t_ready_ms: AtomicU64,
    /// The metrics word.
    pub(crate) report: Mutex<String>,
    /// The last reported rc folder (zsh's final ZDOTDIR).
    pub(crate) rc_dir: Mutex<Option<PathBuf>>,
    /// Recent qualifying rc durations (ms), for `spare_rc_ms_p50`.
    pub(crate) rc_ms: Mutex<VecDeque<u32>>,
}

struct Thread {
    tx: Mutex<Option<Sender<Job>>>,
}

static THREAD: OnceLock<Option<Thread>> = OnceLock::new();
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
/// The dev seam `ATERM_DEBUG_SPARE_ELIGIBLE=1`: skip learning.
fn debug_eligible() -> bool {
    aterm_types::dev_seam!("ATERM_DEBUG_SPARE_ELIGIBLE").is_some_and(|v| v == "1")
}

/// The dev seam `ATERM_DEBUG_SPARE_PRESENT=1`: a headless instance takes its
/// launch as a person's arrival, as a windowed launch's first focus is, so a
/// measurement can stand in for the person.
fn debug_present() -> bool {
    aterm_types::dev_seam!("ATERM_DEBUG_SPARE_PRESENT").is_some_and(|v| v == "1")
}

/// A dev-seam duration in ms.
fn debug_ms(name: &'static str) -> Option<Duration> {
    let raw = match name {
        "T_READY" => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_TREADY_MS"),
        "IDLE" => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_IDLE_MS"),
        "PERSON" => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_PERSON_MS"),
        "READY_AGE" => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_READY_AGE_MS"),
        _ => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_SETTLE_MS"),
    }?;
    raw.to_str()?.parse().ok().map(Duration::from_millis)
}

/// [`PERSON_WINDOW`] (its dev seam shortens it for a measurement of a return).
fn person_window() -> Duration {
    debug_ms("PERSON").unwrap_or(PERSON_WINDOW)
}

/// [`READY_MAX_AGE`] (its dev seam, likewise).
fn ready_max_age() -> Duration {
    debug_ms("READY_AGE").unwrap_or(READY_MAX_AGE)
}

/// The mirror the thread keeps.
pub(crate) fn shared() -> Arc<Shared> {
    Arc::clone(SHARED.get_or_init(Arc::default))
}

/// Hand `job` to the thread, starting it on first use. `Err` gives it back
/// when no thread could start.
pub(crate) fn send_job(job: Job) -> Result<(), Job> {
    let thread = THREAD.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("aterm-spare".into())
            .spawn(move || worker(rx))
            .ok()
            .map(|_| Thread {
                tx: Mutex::new(Some(tx)),
            })
    });
    match thread {
        Some(t) => match t.tx.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            Some(tx) => tx.send(job).map_err(|e| e.0),
            None => Err(job),
        },
        None => Err(job),
    }
}

/// Stop the thread and wait for it (the final exit).
pub(crate) fn stop_thread() {
    let Some(Some(thread)) = THREAD.get() else {
        return;
    };
    let (done_tx, done_rx) = channel();
    let sent = thread
        .tx
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .is_some_and(|tx| tx.send(Job::Stop(done_tx)).is_ok());
    if sent {
        let _ = done_rx.recv_timeout(Duration::from_secs(5));
    }
}

/// One pending observation on the thread.
struct Pending {
    obs: Box<Observation>,
    stamp: u64,
    /// The escape scan was busy: not before this (the scan's spacing).
    not_before: Option<Instant>,
}

fn worker(rx: Receiver<Job>) {
    crate::qos::set_self(crate::qos::Role::Background);
    let shared = shared();
    shared
        .system_zshenv
        .store(Path::new("/etc/zshenv").exists(), Ordering::Release);
    let mut book = GateBook::at(aterm_types::dirs::state_dir().map(|d| d.join(GATE_FILE)));
    let mut pending: Vec<Pending> = Vec::new();
    let mut last_scan: Option<Instant> = None;
    let mut inputs: Option<GenInputs> = None;
    let mut proxy: Option<EventLoopProxy<Wake>> = None;
    loop {
        let deadline = pending.iter().map(due).min();
        let job = match deadline {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(job) => Some(job),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(job) => Some(job),
                Err(_) => break,
            },
        };
        if let Some(Job::Stop(done)) = job {
            let _ = done.send(());
            break;
        }
        // A panic is a failed job, never a dead thread.
        let changed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
            None | Some(Job::Marked) | Some(Job::Stop(_)) => false,
            Some(Job::Spawn(job)) => {
                run_spawn(*job);
                false
            }
            Some(Job::Observe(obs)) => {
                let z = shared
                    .rc_dir
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                let stamp = rc_generation(&obs.inputs, z.as_deref());
                pending.push(Pending {
                    obs,
                    stamp,
                    not_before: None,
                });
                false
            }
            Some(Job::Verify(job)) => {
                let verdict = verify(&job, &mut last_scan, &shared);
                aterm_log::info!("spare {}: verified at its hold: {verdict:?}", job.id);
                let _ = (job.reply)(Wake::SpareNote {
                    id: job.id,
                    note: SpareNote::Verified(verdict),
                });
                false
            }
            Some(Job::Fail(generation, verdict)) => {
                book.fail(generation, verdict);
                true
            }
            Some(Job::Restamp(new_inputs, new_proxy)) => {
                inputs = Some(new_inputs);
                if new_proxy.is_some() {
                    proxy = new_proxy;
                }
                true
            }
            Some(Job::Freshen(n)) => {
                if let Some(inputs) = &inputs {
                    mirror(&mut book, inputs, &shared, None);
                }
                shared.fresh.store(n, Ordering::Release);
                if let Some(proxy) = &proxy {
                    let _ = proxy.send_event(Wake::SpareNote {
                        id: 0,
                        note: SpareNote::Gate,
                    });
                }
                false
            }
            Some(Job::Retire(session, kill)) => {
                if kill {
                    kill_session(session.pid);
                }
                drop(session);
                false
            }
            Some(Job::Sweep) => {
                #[cfg(unix)]
                if let Some(dir) = crate::control_auth::socket_dir() {
                    sweep_dead_fifos(&dir.join(aterm_core::shell_integration::SPARE_DIR));
                }
                false
            }
        }))
        .unwrap_or(false);
        let mut changed = changed;
        // Evaluate every due observation.
        let now = Instant::now();
        let mut i = 0;
        let mut deferred = Vec::new();
        while i < pending.len() {
            if due(&pending[i]) <= now {
                let p = pending.swap_remove(i);
                let again = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    evaluate(p, &mut book, &mut last_scan, &shared)
                }))
                .unwrap_or(None);
                deferred.extend(again);
                changed = true;
            } else {
                i += 1;
            }
        }
        pending.extend(deferred);
        if changed && let Some(inputs) = &inputs {
            mirror(&mut book, inputs, &shared, proxy.as_ref());
        }
    }
}

/// When an observation is evaluated: shortly after its start mark, or at the
/// end of the sample window.
fn due(p: &Pending) -> Instant {
    let at = due_at(&p.obs);
    p.not_before.map_or(at, |nb| at.max(nb))
}

fn due_at(obs: &Observation) -> Instant {
    match obs.witness.start_seen.get() {
        Some(seen) => seen.at + SCAN_GRACE,
        None => obs.started_at + SAMPLE_WINDOW,
    }
}

/// Recompute the current generation and mirror the gate's view of it; tell the
/// main thread when eligibility moved.
fn mirror(
    book: &mut GateBook,
    inputs: &GenInputs,
    shared: &Shared,
    proxy: Option<&EventLoopProxy<Wake>>,
) {
    let z = shared
        .rc_dir
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let generation = rc_generation(inputs, z.as_deref());
    let eligible = debug_eligible() || book.eligible(generation);
    let report = if debug_eligible() {
        "eligible".to_owned()
    } else {
        book.report(generation)
    };
    let t_ready = debug_ms("T_READY").unwrap_or_else(|| book.t_ready(generation));
    shared
        .t_ready_ms
        .store(t_ready.as_millis() as u64, Ordering::Release);
    *shared.report.lock().unwrap_or_else(|p| p.into_inner()) = report;
    let old_gen = shared.generation.swap(generation, Ordering::AcqRel);
    let was = shared.eligible.swap(eligible, Ordering::AcqRel);
    if (was != eligible || old_gen != generation)
        && let Some(proxy) = proxy
    {
        let _ = proxy.send_event(Wake::SpareNote {
            id: 0,
            note: SpareNote::Gate,
        });
    }
}

/// Judge one visible run (design §7.1) and fold it into the gate.
/// Judge one visible run and fold it in; a run whose escape scan must wait for
/// the scan's spacing comes back to be judged then (two tabs opened within the
/// spacing each still teach the gate).
fn evaluate(
    p: Pending,
    book: &mut GateBook,
    last_scan: &mut Option<Instant>,
    shared: &Shared,
) -> Option<Pending> {
    if let Some(at) = *last_scan
        && p.obs.witness.start_seen.get().is_some()
    {
        let free = at + SCAN_EVERY;
        if Instant::now() < free {
            return Some(Pending {
                not_before: Some(free),
                ..p
            });
        }
    }
    let Pending { obs, stamp, .. } = p;
    OBSERVED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(_, w)| w.upgrade().is_some_and(|w| !Arc::ptr_eq(&w, &obs.witness)));
    let w = &obs.witness;
    let Some(seen) = w.start_seen.get() else {
        book.sample(stamp, Sample::Unclean);
        return None;
    };
    if let Some(dir) = &seen.mark.rc_dir {
        let mut rc_dir = shared.rc_dir.lock().unwrap_or_else(|e| e.into_inner());
        if rc_dir.as_deref() != Some(Path::new(dir)) {
            *rc_dir = Some(PathBuf::from(dir));
        }
    }
    // The generation must not have moved under the run; else nothing is learned.
    let z = shared
        .rc_dir
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if rc_generation(&obs.inputs, z.as_deref()) != stamp {
        return None;
    }
    let sample = judge(&obs, seen, last_scan);
    aterm_log::info!("spare gate: generation {stamp:016x} saw a visible run: {sample:?}");
    if let Sample::Clean { rc_ms } = sample {
        let mut recent = shared.rc_ms.lock().unwrap_or_else(|e| e.into_inner());
        recent.push_back(rc_ms);
        while recent.len() > 32 {
            recent.pop_front();
        }
    }
    book.sample(stamp, sample);
    None
}

/// The verdict on one visible run.
fn judge(obs: &Observation, seen: &StartSeen, last_scan: &mut Option<Instant>) -> Sample {
    let w = &obs.witness;
    if seen
        .input
        .zip(obs.input0)
        .is_none_or(|(at, input0)| at != input0)
    {
        return Sample::Inconclusive;
    }
    let (pid, image0) = w.shell.get().cloned().unwrap_or((-1, None));
    let rc = seen.at.saturating_duration_since(obs.started_at);
    let flags_clean = seen
        .mark
        .flags
        .is_some_and(|f| f.sched == 0 && f.jobs.unwrap_or(0) == 0 && !f.periodic);
    let clean = seen.mark.kind == StartKind::Cold
        && rc <= SAMPLE_WINDOW
        && flags_clean
        && pid > 1
        && seen.fg_pgrp == pid
        && image0
            .as_ref()
            .is_some_and(|image| seen.image.as_ref() == Some(image))
        && !seen.alt_screen
        && w.host_effects.load(Ordering::Acquire) == obs.host_effects0
        && w.early_control.load(Ordering::Acquire) == obs.early_control0
        && w.askpass.load(Ordering::Acquire) == 0;
    if !clean {
        aterm_log::info!(
            "spare gate: unclean run: kind={:?} rc_ms={} flags={:?} fg={}/{pid} image_same={} alt={} \
             host={}/{} control={}/{} askpass={}",
            seen.mark.kind,
            rc.as_millis(),
            seen.mark.flags,
            seen.fg_pgrp,
            seen.image == image0,
            seen.alt_screen,
            w.host_effects.load(Ordering::Acquire),
            obs.host_effects0,
            w.early_control.load(Ordering::Acquire),
            obs.early_control0,
            w.askpass.load(Ordering::Acquire),
        );
        return Sample::Unclean;
    }
    match rate_limited_scan(pid, &obs.sid, obs.started_at, seen.at, last_scan) {
        Scan::Clean => Sample::Clean {
            rc_ms: u32::try_from(rc.as_millis()).unwrap_or(u32::MAX),
        },
        Scan::Escaped => Sample::Unclean,
        Scan::Unknown | Scan::RateLimited => Sample::Inconclusive,
    }
}

/// The escape scan, at most once per [`SCAN_EVERY`].
/// The wall-clock reading of the monotonic instant `at`.
fn wall_of(at: Instant) -> std::time::SystemTime {
    let now = Instant::now();
    let wall = std::time::SystemTime::now();
    if at <= now {
        wall.checked_sub(now.saturating_duration_since(at))
            .unwrap_or(wall)
    } else {
        wall + at.saturating_duration_since(now)
    }
}

/// The escape scan over a run that started at `run_at` and signed its start
/// record at `mark_at`, at most once per [`SCAN_EVERY`].
fn rate_limited_scan(
    pid: i32,
    sid: &str,
    run_at: Instant,
    mark_at: Instant,
    last_scan: &mut Option<Instant>,
) -> Scan {
    let now = Instant::now();
    if last_scan.is_some_and(|at| now.saturating_duration_since(at) < SCAN_EVERY) {
        return Scan::RateLimited;
    }
    *last_scan = Some(now);
    escape_scan(pid, sid, wall_of(run_at), wall_of(mark_at) + SCAN_GRACE)
}

/// Verify a warm spare at its hold (design §6, Verifying).
fn verify(job: &VerifyJob, last_scan: &mut Option<Instant>, shared: &Shared) -> Verdict {
    let w = &job.witness;
    let Some(seen) = w.start_seen.get() else {
        return Verdict::Backoff("no-mark");
    };
    if seen.mark.kind != StartKind::Spare {
        return Verdict::Deterministic("ran-past-hold");
    }
    let Some(f) = seen.mark.flags else {
        return Verdict::Backoff("no-flags");
    };
    if f.sched != 0 || f.jobs.unwrap_or(0) != 0 || f.periodic {
        return Verdict::Deterministic(if f.sched != 0 {
            "sched"
        } else if f.periodic {
            "periodic"
        } else {
            "jobs"
        });
    }
    let (pid, image0) = w.shell.get().cloned().unwrap_or((-1, None));
    if pid <= 1
        || seen.fg_pgrp != pid
        || !image0
            .as_ref()
            .is_some_and(|image| seen.image.as_ref() == Some(image))
    {
        return Verdict::Backoff("not-the-shell");
    }
    if seen.alt_screen {
        return Verdict::Backoff("alt-screen");
    }
    let z = shared
        .rc_dir
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if rc_generation(&job.inputs, z.as_deref()) != job.generation {
        return Verdict::Backoff("stale");
    }
    // A scan that is rate-limited waits for its turn: a verification is rare.
    if let Some(at) = *last_scan {
        let wait = SCAN_EVERY.saturating_sub(at.elapsed());
        std::thread::sleep(wait);
    }
    match rate_limited_scan(pid, &job.sid, job.run_at, seen.at, last_scan) {
        Scan::Clean => Verdict::Ok,
        Scan::Escaped => Verdict::Backoff("escaped"),
        Scan::Unknown | Scan::RateLimited => Verdict::Backoff("scan-unknown"),
    }
}

/// Spawn one spare on this thread and post it back. At this thread's own band:
/// an exec'd child's priority does not follow the thread that forked it
/// (measured: forks from a utility and a user-interactive thread both run at
/// priority 31 after exec), and nobody waits for a spare's fork.
fn run_spawn(job: SpawnJob) {
    let SpawnJob {
        id,
        window,
        rows,
        cols,
        cell_px,
        factory,
        proxy,
        cwd,
    } = job;
    let session = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::spawn::spawn_session(
            id,
            window,
            rows,
            cols,
            cell_px,
            &factory,
            &proxy,
            cwd.as_deref(),
            None,
            None,
            None,
        )
    }))
    .unwrap_or_else(|_| Err(std::io::Error::other("the spare's spawn panicked")));
    let done = Wake::SpareSpawned(Box::new(SpareSpawned { id, session }));
    if let Err(winit::event_loop::EventLoopClosed(Wake::SpareSpawned(done))) =
        proxy.send_event(done)
        && let Ok(session) = done.session
    {
        // No loop to take it: hang it up here.
        drop(session);
    }
}

// ─── The backstop's classification (design §5.3) ───

impl Wake {
    /// The local session a wake REPORTS about, from that session's own reader or
    /// host plumbing (output, readiness, exit, a bell, an input watch, its meta,
    /// footer or program): what the warm spare's backstop at the top of
    /// `user_event` swallows for a spare nobody owns yet (`spare`). A request
    /// that names a session (a control verb, a menu row) is `None`: its handler
    /// resolves the name against the registry, where a spare is not. Exhaustive,
    /// with no `_` arm, so a new variant must be classified here.
    pub(crate) fn local_session(&self) -> Option<u64> {
        match self {
            Wake::Output { session, .. } => Some(*session),
            Wake::QueuedKeyDelivered { session, .. } => Some(*session),
            Wake::MetaChanged { session, .. } => Some(*session),
            Wake::InputWatch { session, .. } => Some(*session),
            Wake::ClaudeFooter { session, .. } => Some(*session),
            Wake::ProgramResolved { session, .. } => Some(*session),
            Wake::Ready { session, .. } => Some(*session),
            Wake::Exit { session, .. } => Some(*session),
            Wake::Bell { session, .. } => Some(*session),
            #[cfg(target_os = "linux")]
            Wake::WindowOp { session, .. } => Some(*session),
            Wake::ProtectedRead { session, .. } => Some(*session),
            Wake::LeaseChanged { .. }
            | Wake::FabricChanged { .. }
            | Wake::Control
            | Wake::Video { .. }
            | Wake::VideoStatus { .. }
            | Wake::VideoStop { .. }
            | Wake::ReadChrome { .. }
            | Wake::ReadPanes { .. }
            | Wake::ReadPrivacy { .. }
            | Wake::WarmupResult
            | Wake::ClaimantRetired
            | Wake::ConsentObserver
            | Wake::ConsentProbeReady
            | Wake::DebuggerProbeReady
            | Wake::DevToolsProbeReady
            | Wake::DebuggerAccessResult
            | Wake::DevToolsHandOver
            | Wake::GpuSurfaceReady { .. }
            | Wake::MacosAccessCardDecided { .. }
            | Wake::ReadDims { .. }
            | Wake::ReadAppStatus { .. }
            | Wake::ReadMessages { .. }
            | Wake::ReadUpdateControl { .. }
            | Wake::UpdateCheckCompleted
            | Wake::UpdateCheckerPhase
            | Wake::ConfigReloadObserved(..)
            | Wake::ConfigWatchStatus(..)
            | Wake::HarnessPolicyObserved(..)
            | Wake::ThemeCatalogChanged(..)
            | Wake::NativeConfigFinished { .. }
            | Wake::NativeConfigReconciled { .. }
            | Wake::NativeConfigExternalPrepared { .. }
            | Wake::NativeConfigDiagnosticsFinished(..)
            | Wake::FontCatalogPrepared(..)
            | Wake::NativeUpdateFinished { .. }
            | Wake::NativePackagesFinished { .. }
            | Wake::NativeMachineStateFinished { .. }
            | Wake::HarnessStatsCollected { .. }
            | Wake::NativeUpdateReconcileFinished { .. }
            | Wake::NativeUpdateWorkerDrained
            | Wake::NativeDocumentQueueSlotAvailable
            | Wake::NativeBootHealthConfirmationFinished { .. }
            | Wake::NativeActivationSuperseded(..)
            | Wake::ControlPrepared
            | Wake::NativeDocumentSaved { .. }
            | Wake::NativeDocumentJournaled { .. }
            | Wake::NativeDocumentJournalRewritten { .. }
            | Wake::NativeDocumentJournalReseated { .. }
            | Wake::OsAppearance(..)
            | Wake::ControlAppearance { .. }
            | Wake::UpdateHealth { .. }
            | Wake::DevBuildStanding { .. }
            | Wake::DevBuildWrites(..)
            | Wake::UpdateStaged { .. }
            | Wake::SetSettingsField { .. }
            | Wake::Hop { .. }
            | Wake::InvokeMenuAction { .. }
            | Wake::RainControl { .. }
            | Wake::ThemeControl { .. }
            | Wake::FxControl { .. }
            | Wake::StreakStatus { .. }
            | Wake::ToneStatus { .. }
            | Wake::TrailAdmissions { .. }
            | Wake::TrailStatus { .. }
            | Wake::KittyCollection { .. }
            | Wake::KittyWear { .. }
            | Wake::TypingMomentum { .. }
            | Wake::ReadSessionStatus { .. }
            | Wake::ReadSessionStatuses { .. }
            | Wake::Story { .. }
            | Wake::ReadSessionWindows { .. }
            | Wake::SpawnSession { .. }
            | Wake::SpawnConnectedSession { .. }
            | Wake::TabSpawned(..)
            | Wake::SpareSpawned(..)
            | Wake::SpareNote { .. }
            | Wake::CloseSession { .. }
            | Wake::RaiseSession { .. }
            | Wake::ConnectionsChanged
            | Wake::SettingsOverlay { .. }
            | Wake::SettingsShowSection { .. }
            | Wake::Input { .. }
            | Wake::CursorMoveLicenseClear { .. }
            | Wake::HwKeyTarget { .. }
            | Wake::DocumentAdmitted(..)
            | Wake::FabricCli(..)
            | Wake::MenuAction { .. }
            | Wake::ConfirmedClose { .. }
            | Wake::AnswerCloseConfirm { .. }
            | Wake::FleetInstances(..)
            | Wake::CreateWindow
            | Wake::DetachActiveTab
            | Wake::ViewActiveSessionInNewWindow
            | Wake::TabMenuAction { .. }
            | Wake::TabCmd { .. }
            | Wake::TabCmdAimed { .. }
            | Wake::SetDragHover { .. }
            | Wake::FindCmd { .. }
            | Wake::PaneFocus { .. }
            | Wake::PointerCmd { .. }
            | Wake::CaptureWindow { .. }
            | Wake::CaptureAuxWindow { .. }
            | Wake::ReadAuxControls { .. }
            | Wake::InspectApp { .. }
            | Wake::ActApp { .. }
            | Wake::OpenApp { .. }
            | Wake::FrontControlSurface { .. }
            | Wake::OpenAuxWindow { .. }
            | Wake::PkgSeed { .. }
            | Wake::AgentUpgrade { .. }
            | Wake::AgentUpgradeWord { .. }
            | Wake::HostNote(..)
            | Wake::HarnessWait { .. }
            | Wake::HarnessFact { .. }
            | Wake::WallMoved
            | Wake::UpdateProgress(..)
            | Wake::UpdateCheckAsked { .. }
            | Wake::PkgSeedStarted { .. }
            | Wake::PkgFirstRunAhead { .. }
            | Wake::PkgSeedUnusable { .. }
            | Wake::PkgSeedFailed { .. }
            | Wake::PkgSeedDone { .. }
            | Wake::PkgSeedNothing { .. }
            | Wake::PkgLockWaiting { .. }
            | Wake::PkgLockAcquired { .. }
            | Wake::PkgLockTimedOut { .. }
            | Wake::PkgManagedCurrent(..)
            | Wake::PkgMachineSettings(..)
            | Wake::PkgForeign(..)
            | Wake::PkgMachineState(..)
            | Wake::PkgMachineRecordMissing
            | Wake::PkgGroupAborted { .. }
            | Wake::PkgMachineRefused(..)
            | Wake::AppNotice { .. }
            | Wake::Notice { .. }
            | Wake::DevToolsHandOverAsked { .. }
            | Wake::PkgProgress { .. }
            | Wake::PkgPassEnded { .. }
            | Wake::PkgPersonPass(..)
            | Wake::SemanticFontReady
            | Wake::FontFamiliesListed(..)
            | Wake::AiLooked { .. }
            | Wake::ReadHarnessPolicy { .. } => None,
            #[cfg(unix)]
            Wake::Snapshot => None,
            #[cfg(target_os = "macos")]
            Wake::MemoryPressure { .. } => None,
            #[cfg(test)]
            Wake::ConfigReload => None,
            #[cfg(unix)]
            Wake::UpdateHandoffFinished(..) => None,
            #[cfg(unix)]
            Wake::ActivateCommittedHandoff { .. } => None,
            #[cfg(unix)]
            Wake::HandoffHistorySettled(..) => None,
            #[cfg(any(target_os = "macos", all(test, unix)))]
            Wake::UpdateHandoffAwaitingPark { .. } => None,
            #[cfg(any(target_os = "macos", all(test, unix)))]
            Wake::UpdateHandoffStandingDown { .. } => None,
            #[cfg(any(target_os = "macos", windows))]
            Wake::ReduceMotionChanged => None,
            #[cfg(any(target_os = "macos", windows))]
            Wake::ApplyStagedUpdate => None,
            #[cfg(a11y_tree)]
            Wake::Accessibility(..) => None,
            #[cfg(target_os = "linux")]
            Wake::PasteReady { .. } => None,
            #[cfg(target_os = "linux")]
            Wake::FindPasteReady { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::Askpass(..) => None,
            #[cfg(target_os = "macos")]
            Wake::AskpassAnswered { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::AskpassWithdraw { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::PasteConfirmed { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::OperatorAction { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::OperatorMenuOpening => None,
            #[cfg(target_os = "macos")]
            Wake::NativeTerminateRequested { .. } => None,
            #[cfg(unix)]
            Wake::LifelineCut => None,
            #[cfg(target_os = "macos")]
            Wake::SelectTab { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::CloseTab { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::TabContextMenuOpening { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::TabMenuConnection { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::TabMenuUpgrade { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::ConnDragBegin { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::ConnDragTo { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::ConnDragDrop { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::ConnDragCancel { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::BeginSessionRename { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::CommitSessionRename { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::CancelSessionRename { .. } => None,
        }
    }
}

// ─── The slot, on the main thread (design §6) ───

/// Where a spare is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotState {
    /// The spawn is on the `aterm-spare` thread.
    Booked,
    /// Spawned; its parked mark has not arrived.
    Spawned,
    /// Waiting at the parked hold: no startup file has run.
    Parked,
    /// Told `run`: its startup files run unseen.
    Warming,
    /// At its warm hold; the thread is verifying it.
    Verifying,
    /// Verified at its warm hold.
    Ready,
}

impl SlotState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Booked | Self::Spawned => "booked",
            Self::Parked => "parked",
            Self::Warming | Self::Verifying => "warming",
            Self::Ready => "ready",
        }
    }

    /// A tier whose startup files ran (or run) unseen.
    pub(crate) const fn warm(self) -> bool {
        matches!(self, Self::Warming | Self::Verifying | Self::Ready)
    }
}

/// The grid a spare is spawned at: rows, cols and the cell box.
pub(crate) type Grid = (u16, u16, Option<(u16, u16)>);

/// The one spare.
pub(crate) struct Slot {
    pub(crate) id: u64,
    pub(crate) state: SlotState,
    pub(crate) link: Arc<SpareLink>,
    pub(crate) session: Option<Session>,
    /// The folder it was spawned in (`None`: the factory's).
    pub(crate) cwd: Option<String>,
    pub(crate) grid: Grid,
    /// The window it was booked for.
    pub(crate) window: WindowId,
    pub(crate) fingerprint: u64,
    /// The engine-config generation its factory copy carried (the config
    /// carries the reported scheme, so the generation moves with it).
    pub(crate) colors: u64,
    pub(crate) booked: Instant,
    /// When it entered its state.
    pub(crate) since: Instant,
    /// The generation the gate opened for (warm tiers).
    pub(crate) generation: u64,
    pub(crate) t_ready: Duration,
    pub(crate) inputs: GenInputs,
}

/// An adopted spare between Cmd-T and its landing.
pub(crate) struct Transit {
    pub(crate) id: u64,
    pub(crate) link: Arc<SpareLink>,
    pub(crate) tier: SlotState,
    /// The grid it was spawned at: its landing re-sizes its pane only when the
    /// window's differs now.
    pub(crate) grid: Grid,
    pub(crate) age: Duration,
    pub(crate) rc_ms: Option<u64>,
    pub(crate) inputs: GenInputs,
    /// Why the spawner did not hand it over, when it did not.
    pub(crate) miss: Arc<OnceLock<&'static str>>,
}

/// What the spawner checks before it hands an adopted spare over (design §8
/// step 2), off the main thread.
pub(crate) struct AdoptCheck {
    pub(crate) cwd: Option<String>,
    /// `Some((inputs, generation))` for a warm tier: the rc generation must not
    /// have moved.
    pub(crate) generation: Option<(GenInputs, u64)>,
    pub(crate) link: Arc<SpareLink>,
    /// A warm tier dies with its whole session when the check fails.
    pub(crate) kill: bool,
}

impl AdoptCheck {
    /// `None` when the spare may be adopted, else the miss reason.
    pub(crate) fn miss(&self, session: &Session) -> Option<&'static str> {
        if let Some((inputs, generation)) = &self.generation {
            let z = shared()
                .rc_dir
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if rc_generation(inputs, z.as_deref()) != *generation {
                return Some("stale");
            }
        }
        if crate::spawn_folder::start_folder(self.cwd.as_deref())
            .fell_back
            .is_some()
        {
            return Some("folder");
        }
        if self.link.exited() || session.pty_closed_at.get().is_some() {
            return Some("process");
        }
        None
    }
}

/// The platform's say on running an rc unseen: Low Power Mode or a serious
/// thermal state blocks a warm promotion. Injected, with an inert test arm.
pub(crate) type PowerProbe = fn() -> bool;

/// Whether the machine is in Low Power Mode or thermally serious (macOS
/// `NSProcessInfo`; elsewhere never).
pub(crate) fn power_constrained() -> bool {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::{class, sel};
        // SAFETY: `+processInfo` is an immortal singleton; both getters are
        // plain scalar properties, documented thread-safe.
        unsafe {
            let info =
                aterm_objc::send::send_id(class(c"NSProcessInfo").as_id(), sel!(processInfo));
            if info.is_null() {
                return false;
            }
            let low = aterm_objc::send::send_bool(info, sel!(isLowPowerModeEnabled));
            // NSProcessInfoThermalStateSerious = 2.
            let thermal = aterm_objc::send::send_isize(info, sel!(thermalState));
            low || thermal >= 2
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The App's spare state.
pub(crate) struct Spares {
    pub(crate) slot: Option<Slot>,
    /// Retired spare ids whose late wakes are swallowed.
    tombstones: VecDeque<u64>,
    pub(crate) transit: Vec<Transit>,
    parked_spawns: VecDeque<Instant>,
    warm_discards: VecDeque<Instant>,
    last_replace: Option<Instant>,
    /// The last person's gesture (`App::note_person_input`).
    person: std::cell::Cell<Option<Instant>>,
    /// The next tick, if armed.
    tick_at: std::cell::Cell<Option<Instant>>,
    pub(crate) adopted: [u32; 3],
    misses: Vec<(&'static str, u32)>,
    discards: Vec<(&'static str, u32)>,
    pressure_at: Option<Instant>,
    pass_active: bool,
    /// Where a spare should be (window, folder) and since when.
    wanted: Option<(WindowId, Option<String>, Instant)>,
    /// A booking waits for this session's first prompt: its next output wake
    /// arms the tick.
    wants_prompt: std::cell::Cell<Option<u64>>,
    boot_at: Option<Instant>,
    /// A parked spare that ran past its hold: no spare again until the script
    /// set or the shell changes (the key names both).
    pub(crate) parked_failed: Option<u64>,
    pub(crate) shared: Arc<Shared>,
    pub(crate) power: PowerProbe,
    restamped: Option<u64>,
    /// The [`Job::Freshen`] a promotion waits for: its number and when it was
    /// asked.
    freshen: Option<(u64, Instant)>,
    freshen_seq: u64,
    /// The boot session's visible run was handed to the gate.
    boot_observed: bool,
    /// A crashed run's FIFOs were handed to the thread to release.
    swept: bool,
    /// A test's capture of the jobs the App sends the `aterm-spare` thread, and
    /// its stand-in for the event loop (verdicts and notes come back through
    /// the test). `None` in every run.
    #[cfg(test)]
    pub(crate) captured: Option<Vec<Job>>,
}

impl Default for Spares {
    fn default() -> Self {
        Self {
            slot: None,
            tombstones: VecDeque::new(),
            transit: Vec::new(),
            parked_spawns: VecDeque::new(),
            warm_discards: VecDeque::new(),
            last_replace: None,
            person: std::cell::Cell::new(None),
            tick_at: std::cell::Cell::new(None),
            adopted: [0; 3],
            misses: Vec::new(),
            discards: Vec::new(),
            pressure_at: None,
            pass_active: false,
            wanted: None,
            wants_prompt: std::cell::Cell::new(None),
            boot_at: None,
            parked_failed: None,
            // A test's App keeps a mirror of its own; a run shares the thread's.
            shared: if cfg!(test) { Arc::default() } else { shared() },
            power: power_constrained,
            restamped: None,
            freshen: None,
            freshen_seq: 0,
            boot_observed: false,
            swept: false,
            #[cfg(test)]
            captured: None,
        }
    }
}

fn bump(counts: &mut Vec<(&'static str, u32)>, reason: &'static str) {
    match counts.iter_mut().find(|(r, _)| *r == reason) {
        Some((_, n)) => *n = n.saturating_add(1),
        None => counts.push((reason, 1)),
    }
}

/// The entries of `log` younger than `window`.
fn count_within(log: &VecDeque<Instant>, now: Instant, window: Duration) -> usize {
    log.iter()
        .filter(|at| now.saturating_duration_since(**at) < window)
        .count()
}

/// Drop the entries of `log` older than `window`.
fn prune(log: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while log
        .front()
        .is_some_and(|at| now.saturating_duration_since(*at) >= window)
    {
        log.pop_front();
    }
}

/// A booking's or a promotion's verdict.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BookCheck {
    /// Go: the window and folder.
    Go {
        window: WindowId,
        cwd: Option<String>,
    },
    /// Not before this instant.
    Wait(Instant),
    /// The front window's focused session (if any) has not reached its prompt.
    NotAtPrompt(Option<u64>),
    /// Not now, for this reason; a trigger looks again.
    No(&'static str),
}

impl Spares {
    /// Whether the backstop swallows a wake naming local id `id`: the slot's,
    /// one in transit, or a retired spare's.
    pub(crate) fn swallows(&self, id: u64) -> bool {
        self.slot.as_ref().is_some_and(|s| s.id == id)
            || self.transit.iter().any(|t| t.id == id)
            || self.tombstones.contains(&id)
    }

    fn tombstone(&mut self, id: u64) {
        if self.tombstones.len() >= TOMBSTONES {
            self.tombstones.pop_front();
        }
        self.tombstones.push_back(id);
    }

    /// Send `job` to the `aterm-spare` thread (a test captures it instead).
    pub(crate) fn send(&mut self, job: Job) -> Result<(), Job> {
        #[cfg(test)]
        if let Some(captured) = self.captured.as_mut() {
            captured.push(job);
            return Ok(());
        }
        send_job(job)
    }

    /// An adopted spare that never went live: its late wakes are swallowed.
    pub(crate) fn tombstone_public(&mut self, id: u64) {
        self.tombstone(id);
    }

    /// Arm the tick no later than `at`.
    pub(crate) fn arm(&self, at: Instant) {
        let next = self.tick_at.get().map_or(at, |t| t.min(at));
        self.tick_at.set(Some(next));
    }

    /// A person's gesture, or a window coming to the front: stamped, and the
    /// tick armed once the input settles.
    pub(crate) fn note_person(&self, now: Instant) {
        let first = self
            .person
            .get()
            .is_none_or(|at| now.saturating_duration_since(at) >= person_window());
        self.person.set(Some(now));
        let settle = debug_ms("SETTLE").unwrap_or(SETTLE);
        if first
            || self.slot.is_none()
            || self
                .slot
                .as_ref()
                .is_some_and(|s| s.state == SlotState::Parked)
        {
            self.arm(now + settle);
        }
    }

    /// Session `id` produced output: a booking that waited for its first
    /// prompt looks again.
    pub(crate) fn note_output(&self, id: u64, now: Instant) {
        if self.wants_prompt.get() == Some(id) {
            self.wants_prompt.set(None);
            self.arm(now + debug_ms("SETTLE").unwrap_or(SETTLE));
        }
    }

    pub(crate) fn note_miss(&mut self, reason: &'static str) {
        bump(&mut self.misses, reason);
    }

    /// Publish what `metrics` prints of the spare (design §10): the control
    /// thread formats it ([`metrics_fields_text`]).
    pub(crate) fn publish(&self, mode: SpareMode, now: Instant) {
        let state = if mode == SpareMode::Off {
            "off"
        } else {
            self.slot.as_ref().map_or("absent", |s| s.state.as_str())
        };
        let list = |counts: &[(&'static str, u32)]| -> String {
            if counts.is_empty() {
                "-".to_owned()
            } else {
                counts
                    .iter()
                    .map(|(r, n)| format!("{r}:{n}"))
                    .collect::<Vec<_>>()
                    .join(",")
            }
        };
        let parked_left =
            PARKED_BUDGET.saturating_sub(count_within(&self.parked_spawns, now, HOUR));
        let warm_left = WARM_BUDGET.saturating_sub(count_within(&self.warm_discards, now, HOUR));
        let snap = MetricsSnap {
            state,
            booked: self.slot.as_ref().map(|s| s.booked),
            rest: format!(
                "spare_adopted=parked:{},warming:{},ready:{} spare_miss={} spare_discard={} \
                 spare_budget=warm:{warm_left}/{WARM_BUDGET},parked:{parked_left}/{PARKED_BUDGET}",
                self.adopted[0],
                self.adopted[1],
                self.adopted[2],
                list(&self.misses),
                list(&self.discards),
            ),
        };
        *METRICS.lock().unwrap_or_else(|e| e.into_inner()) = Some(snap);
    }
}

/// What the main thread last published for `metrics`.
struct MetricsSnap {
    state: &'static str,
    booked: Option<Instant>,
    rest: String,
}

static METRICS: Mutex<Option<MetricsSnap>> = Mutex::new(None);

/// The spare's `metrics` fields (design §10), leading space included:
/// `spare=` (off/absent/booked/parked/warming/ready), its age, the spare PTYs,
/// the gate's word, the adoptions per tier, misses and discards per reason,
/// the budgets left and the median qualifying rc run.
pub(crate) fn metrics_fields_text() -> String {
    let snap = METRICS.lock().unwrap_or_else(|e| e.into_inner());
    let (state, age, rest) = match snap.as_ref() {
        Some(s) => (
            s.state,
            s.booked.map_or(0, |at| at.elapsed().as_millis()),
            s.rest.clone(),
        ),
        None => (
            "absent",
            0,
            format!(
                "spare_adopted=parked:0,warming:0,ready:0 spare_miss=- spare_discard=- \
                 spare_budget=warm:{WARM_BUDGET}/{WARM_BUDGET},parked:{PARKED_BUDGET}/{PARKED_BUDGET}"
            ),
        ),
    };
    drop(snap);
    let shared = shared();
    let report = shared
        .report
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let report = if report.is_empty() {
        format!("learning 0/{CLEAN_STREAK}")
    } else {
        report
    };
    let rc_p50 = {
        let recent = shared.rc_ms.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<u32> = recent.iter().copied().collect();
        v.sort_unstable();
        v.get(v.len() / 2).copied()
    };
    format!(
        " spare={state} spare_age_ms={age} spare_ptys={} spare_gate={} {rest} spare_rc_ms_p50={}",
        crate::metrics::spare_ptys(),
        report.replace(' ', "_"),
        rc_p50.map_or_else(|| "-".to_owned(), |v| v.to_string()),
    )
}

/// Field-for-field JSON twin of [`metrics_fields_text`], with a leading comma:
/// each `key=value` token, a number where its value is one, else a string.
pub(crate) fn metrics_fields_json() -> String {
    metrics_fields_text()
        .split_whitespace()
        .filter_map(|token| token.split_once('='))
        .map(|(key, value)| {
            if value.parse::<u64>().is_ok() {
                format!(",\"{key}\":{value}")
            } else {
                format!(",\"{key}\":\"{}\"", aterm_control::wire::json_escape(value))
            }
        })
        .collect()
}

/// The deterministic-failure key: the script set and the shell binary.
fn parked_key(inputs: &GenInputs) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    aterm_core::shell_integration::script_set_address().hash(&mut h);
    inputs.shell_path.hash(&mut h);
    h.finish()
}

/// The resolved shell binary for a factory's override (`$SHELL`, else
/// `/bin/sh`, as `aterm_pty` resolves it).
fn shell_binary(override_shell: Option<&str>) -> PathBuf {
    match override_shell.filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => std::env::var_os("SHELL").map_or_else(|| PathBuf::from("/bin/sh"), PathBuf::from),
    }
}

impl App {
    /// The configured mode (`spare_shell`).
    pub(crate) fn spare_mode(&self) -> SpareMode {
        if cfg!(not(target_os = "macos")) {
            return SpareMode::Off;
        }
        self.config.spare_shell_mode()
    }

    /// What this factory's rc generation is computed from (cheap; no stat).
    pub(crate) fn spare_gen_inputs(&self) -> GenInputs {
        let f = &self.session_factory;
        let home = aterm_types::dirs::home_dir().unwrap_or_default();
        GenInputs {
            shell: crate::spawn::detect_spawn_shell(f.shell_override.as_deref()),
            shell_path: shell_binary(f.shell_override.as_deref()),
            launch_zdotdir: std::env::var_os("ZDOTDIR")
                .filter(|z| !z.is_empty())
                .map_or_else(|| home.clone(), PathBuf::from),
            home,
            fingerprint: f.spawn_fingerprint(),
            containment: aterm_containment::mode_or_containment().level(),
        }
    }

    /// Whether this factory's shells could ever be spares: integrated zsh/bash
    /// with no `-e` and no temporal recording.
    fn spare_factory_ok(&self) -> bool {
        let f = &self.session_factory;
        f.exec_command.is_none()
            && f.integrate
            && !f.temporal_recording
            && aterm_core::shell_integration::has_spare_hold(crate::spawn::detect_spawn_shell(
                f.shell_override.as_deref(),
            ))
    }

    /// The tick's next instant, running it first when it is due (`about_to_wait`).
    pub(crate) fn spare_deadline(&mut self, now: Instant) -> Option<Instant> {
        // The first turn: the first tick waits out the boot grace.
        if self.spares.boot_at.is_none() {
            self.spares.boot_at = Some(now);
            self.spares.arm(now + BOOT_GRACE);
            if debug_present() {
                self.spares.note_person(now);
            }
        }
        if self.spares.tick_at.get().is_some_and(|at| at <= now) {
            self.spares.tick_at.set(None);
            self.spare_tick(now);
            self.publish_spare();
        }
        self.spares.tick_at.get()
    }

    /// Publish the spare's `metrics` fields.
    pub(crate) fn publish_spare(&self) {
        self.spares.publish(self.spare_mode(), Instant::now());
    }

    /// Book, promote, replace and age the spare (design §7.2, §9). Main thread,
    /// no filesystem access.
    pub(crate) fn spare_tick(&mut self, now: Instant) {
        // A crashed run's spares are released once a launch, whatever the mode
        // (never a test's: the sweep reads the real control dir).
        if cfg!(all(target_os = "macos", not(test)))
            && !std::mem::replace(&mut self.spares.swept, true)
        {
            let _ = self.spares.send(Job::Sweep);
        }
        let mode = self.spare_mode();
        if mode == SpareMode::Off || self.proxy.is_none() {
            if self.spares.slot.is_some() {
                self.retire_spare("off", None);
            }
            return;
        }
        self.spares.boot_at.get_or_insert(now);
        // The gate learns this factory's generation (once per change).
        let inputs = self.spare_gen_inputs();
        let key = inputs.fingerprint ^ parked_key(&inputs);
        if self.spares.restamped != Some(key) {
            self.spares.restamped = Some(key);
            let _ = self
                .spares
                .send(Job::Restamp(inputs.clone(), self.proxy.clone()));
        }
        // The boot session's startup files ran in view before the first tick:
        // a visible run the gate learns from like a cold tab's.
        if !std::mem::replace(&mut self.spares.boot_observed, true) && mode == SpareMode::Auto {
            self.observe_spare_run(0, inputs.clone());
        }
        self.spare_age(now, mode);
        self.spare_follow(now);
        if self.spares.slot.is_none() {
            self.spare_book(now, &inputs);
        }
        if mode == SpareMode::Auto {
            self.spare_promote(now);
        }
    }

    /// The slot's own deadlines (design §6, §9).
    fn spare_age(&mut self, now: Instant, mode: SpareMode) {
        let Some(slot) = &self.spares.slot else {
            return;
        };
        let age = now.saturating_duration_since(slot.since);
        let (state, t_ready) = (slot.state, slot.t_ready);
        match state {
            SlotState::Spawned if age >= PARKED_DEADLINE => {
                // The wrapper did not hold: the rc may be running unasked.
                aterm_log::warn!("spare: no parked mark within {PARKED_DEADLINE:?}; spares off");
                self.spares.parked_failed = Some(parked_key(&slot.inputs));
                self.retire_spare("no_park", None);
            }
            SlotState::Parked if age >= PARKED_MAX_AGE => self.retire_spare("age", None),
            SlotState::Warming | SlotState::Verifying if mode == SpareMode::Parked => {
                self.retire_spare("parked", None);
            }
            SlotState::Ready if mode == SpareMode::Parked => self.retire_spare("parked", None),
            SlotState::Warming if age >= t_ready => {
                self.retire_spare("rc_timeout", Some(Verdict::Backoff("rc_timeout")));
            }
            SlotState::Ready if age >= ready_max_age() => self.retire_spare("age", None),
            _ => {}
        }
        if let Some(slot) = &self.spares.slot {
            let at = match slot.state {
                SlotState::Spawned => Some(slot.since + PARKED_DEADLINE),
                SlotState::Parked => Some(slot.since + PARKED_MAX_AGE),
                SlotState::Warming => Some(slot.since + slot.t_ready),
                SlotState::Ready => Some(slot.since + ready_max_age()),
                SlotState::Booked | SlotState::Verifying => None,
            };
            if let Some(at) = at {
                self.spares.arm(at);
            }
        }
    }

    /// Where a spare should be now: the front window and its focused pane's
    /// folder, or `None` when that pane is held (no spare then).
    fn spare_wanted(&self) -> Option<(WindowId, Option<String>)> {
        let front = self.frontmost_window?;
        if !self.windows.contains_key(&front) {
            return None;
        }
        match self.focused_pane_folder(front) {
            Some(folder) if folder.held => None,
            Some(folder) => Some((front, Some(folder.reported))),
            None => Some((front, None)),
        }
    }

    /// Replace a spare whose window or folder moved, once the new one holds
    /// (design §9): 1 s for a parked spare, 10 s for a warm one; a warm spare's
    /// grid change, after 2 s.
    fn spare_follow(&mut self, now: Instant) {
        let wanted = self.spare_wanted();
        let moved = match (&self.spares.wanted, &wanted) {
            (Some((w, c, _)), Some((nw, nc))) => w != nw || c != nc,
            (None, None) => false,
            _ => true,
        };
        if moved {
            self.spares.wanted = wanted.clone().map(|(w, c)| (w, c, now));
        }
        let Some(slot) = &self.spares.slot else {
            return;
        };
        if matches!(slot.state, SlotState::Booked | SlotState::Spawned) {
            return;
        }
        let since = self.spares.wanted.as_ref().map_or(now, |(_, _, at)| *at);
        // One spare per process, re-keyed to the front window: only a folder
        // that moved (or a held pane) misplaces it; a warm one's grid is judged
        // in the window it would land in.
        let misplaced = match &wanted {
            Some((_, c)) => *c != slot.cwd,
            None => true,
        };
        if let Some((w, _)) = &wanted
            && !misplaced
            && *w != slot.window
            && let Some(slot) = self.spares.slot.as_mut()
        {
            slot.window = *w;
        }
        let Some(slot) = &self.spares.slot else {
            return;
        };
        let regrid = slot.state.warm()
            && self
                .windows
                .contains_key(&slot.window)
                .then(|| self.spawn_grid(slot.window))
                .is_some_and(|g| g != slot.grid);
        let after = if slot.state.warm() {
            REPLACE_WARM_AFTER
        } else {
            REPLACE_PARKED_AFTER
        };
        let after = if regrid && !misplaced {
            REPLACE_MIN
        } else {
            after
        };
        if !(misplaced || regrid) {
            return;
        }
        let ready_at = since + after;
        let min_at = self.spares.last_replace.map_or(now, |at| at + REPLACE_MIN);
        let at = ready_at.max(min_at);
        if now >= at {
            self.spares.last_replace = Some(now);
            self.retire_spare(if regrid { "grid" } else { "moved" }, None);
            self.spares.arm(now);
        } else {
            self.spares.arm(at);
        }
    }

    /// Whether a parked spare may be booked now (design §7.2): where it goes,
    /// when to look again, or why not. The event loop it needs is the tick's
    /// and the booking's own check, so a test reaches every gate here.
    pub(crate) fn spare_book_check(&self, now: Instant, inputs: &GenInputs) -> BookCheck {
        // A person seen once is enough: a PARKED spare runs nothing of theirs,
        // so it waits through an absence for the Cmd-T that ends it. Only WARM
        // asks for a person now ([`Self::spare_promote_check`]).
        if self.config.spare_shell_needs_person_or_default() && self.spares.person.get().is_none() {
            return BookCheck::No("no-person");
        }
        if !self.spare_factory_ok() {
            return BookCheck::No("factory");
        }
        if self.pending_restore.is_some() {
            return BookCheck::No("restore");
        }
        if self.update_handoff_in_flight() {
            return BookCheck::No("update");
        }
        if self.spares.parked_failed == Some(parked_key(inputs)) {
            return BookCheck::No("parked-failed");
        }
        if let Some(at) = self.spares.pressure_at
            && now.saturating_duration_since(at) < PRESSURE_HOLD
        {
            return BookCheck::Wait(at + PRESSURE_HOLD);
        }
        // A zsh spare runs /etc/zshenv before its hold: parking it needs the gate.
        if inputs.shell == aterm_core::shell_integration::ShellType::Zsh
            && self.spares.shared.system_zshenv.load(Ordering::Acquire)
            && !self.spares.shared.eligible.load(Ordering::Acquire)
        {
            return BookCheck::No("system-zshenv");
        }
        // Time gates: the boot grace, the replacement spacing, input settled, the
        // hourly budget.
        let settle = debug_ms("SETTLE").unwrap_or(SETTLE);
        let mut not_before = self.spares.boot_at.map_or(now, |at| at + BOOT_GRACE);
        if let Some(at) = self.spares.last_replace {
            not_before = not_before.max(at + REPLACE_MIN);
        }
        if let Some(at) = self.spares.person.get() {
            not_before = not_before.max(at + settle);
        }
        if count_within(&self.spares.parked_spawns, now, HOUR) >= PARKED_BUDGET
            && let Some(oldest) = self.spares.parked_spawns.front()
        {
            not_before = not_before.max(*oldest + HOUR);
        }
        if now < not_before {
            return BookCheck::Wait(not_before);
        }
        let Some((window, cwd)) = self.spare_wanted() else {
            return BookCheck::No("held");
        };
        // The front window's focused session has reached its prompt.
        let focused = self.focused_session_id(window);
        let at_prompt = focused
            .and_then(|id| self.pool.get(id))
            .is_some_and(|s| crate::term_lock(&s.term).shell_prompt_started());
        if !at_prompt {
            return BookCheck::NotAtPrompt(focused);
        }
        BookCheck::Go { window, cwd }
    }

    /// Book a parked spare when every condition holds.
    fn spare_book(&mut self, now: Instant, inputs: &GenInputs) {
        let (window, cwd) = match self.spare_book_check(now, inputs) {
            BookCheck::Go { window, cwd } => (window, cwd),
            BookCheck::Wait(at) => {
                self.spares.arm(at);
                return;
            }
            BookCheck::NotAtPrompt(focused) => {
                // Its next output looks again.
                self.spares.wants_prompt.set(focused);
                return;
            }
            BookCheck::No(_) => return,
        };
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let id = self.next_session_id;
        self.next_session_id += 1;
        let grid = self.spawn_grid(window);
        let link = Arc::new(SpareLink::new(
            id,
            window,
            Some(crate::spawn::post_to(&proxy)),
        ));
        let mut factory = self.session_factory.clone();
        factory.reader_after_install = false;
        factory.spare = Some(SpareSpawn {
            link: Arc::clone(&link),
        });
        let colors = self.engine_color_fanouts;
        let (rows, cols, cell_px) = grid;
        prune(&mut self.spares.parked_spawns, now, HOUR);
        self.spares.parked_spawns.push_back(now);
        self.spares.slot = Some(Slot {
            id,
            state: SlotState::Booked,
            link,
            session: None,
            cwd: cwd.clone(),
            grid,
            window,
            fingerprint: inputs.fingerprint,
            colors,
            booked: now,
            since: now,
            generation: 0,
            t_ready: T_READY_MIN,
            inputs: inputs.clone(),
        });
        let job = Job::Spawn(Box::new(SpawnJob {
            id,
            window,
            rows,
            cols,
            cell_px,
            factory,
            proxy,
            cwd,
        }));
        if self.spares.send(job).is_err() {
            self.spares.slot = None;
            self.spares.tombstone(id);
        }
    }

    /// Whether the parked spare may be told `run` now (design §7.2).
    pub(crate) fn spare_promote_check(&self, now: Instant) -> BookCheck {
        let Some(slot) = &self.spares.slot else {
            return BookCheck::No("none");
        };
        if slot.state != SlotState::Parked {
            return BookCheck::No("not-parked");
        }
        if self.spare_mode() != SpareMode::Auto {
            return BookCheck::No("mode");
        }
        // The startup files run unseen only while a person is here (a gesture,
        // or a window coming to the front, within [`PERSON_WINDOW`]); their
        // next one arms the tick again ([`Spares::note_person`]).
        if self.config.spare_shell_needs_person_or_default()
            && self
                .spares
                .person
                .get()
                .is_none_or(|at| now.saturating_duration_since(at) >= person_window())
        {
            return BookCheck::No("no-person");
        }
        let shared = &self.spares.shared;
        if shared.generation.load(Ordering::Acquire) == 0
            || !shared.eligible.load(Ordering::Acquire)
        {
            return BookCheck::No("gate");
        }
        let idle = debug_ms("IDLE").unwrap_or(PROMOTE_IDLE);
        let stable = debug_ms("IDLE").unwrap_or(PROMOTE_FOLDER_STABLE);
        let mut not_before = slot.since;
        if let Some(at) = self.spares.person.get() {
            not_before = not_before.max(at + idle);
        }
        if let Some((_, _, at)) = &self.spares.wanted {
            not_before = not_before.max(*at + stable);
        }
        if now < not_before {
            return BookCheck::Wait(not_before);
        }
        if count_within(&self.spares.warm_discards, now, HOUR) >= WARM_BUDGET {
            return BookCheck::No("budget");
        }
        if self.spares.pass_active {
            return BookCheck::No("pass");
        }
        if self.update_handoff_in_flight() {
            return BookCheck::No("update");
        }
        if (self.spares.power)() {
            return BookCheck::No("power");
        }
        BookCheck::Go {
            window: slot.window,
            cwd: slot.cwd.clone(),
        }
    }

    /// Promote a parked spare to WARM: write `run`.
    fn spare_promote(&mut self, now: Instant) {
        match self.spare_promote_check(now) {
            BookCheck::Go { .. } => {}
            BookCheck::Wait(at) => {
                self.spares.arm(at);
                return;
            }
            BookCheck::NotAtPrompt(_) | BookCheck::No(_) => return,
        }
        // The check read the gate's mirror, which is only as fresh as its last
        // stamp: `run` waits for one taken after this asked, so a startup file
        // edited since the gate opened never runs unseen (a stamp that finds a
        // new generation closes the gate before the check passes again).
        if !self.spare_fresh(now) {
            return;
        }
        let shared = &self.spares.shared;
        let generation = shared.generation.load(Ordering::Acquire);
        let t_ready = Duration::from_millis(shared.t_ready_ms.load(Ordering::Acquire))
            .clamp(T_READY_MIN, T_READY_MAX);
        let t_ready = debug_ms("T_READY").unwrap_or(t_ready);
        let Some(slot) = self.spares.slot.as_mut() else {
            return;
        };
        if !slot.link.witness.tell("run") {
            self.retire_spare("fifo", None);
            return;
        }
        slot.state = SlotState::Warming;
        slot.since = now;
        slot.generation = generation;
        slot.t_ready = t_ready;
        self.spares.arm(now + t_ready);
    }

    /// Whether the gate's mirror was stamped after the last [`Job::Freshen`]
    /// this asked, within [`FRESH_FOR`] of the ask. Else one is asked (one at
    /// a time), and its answer's gate note runs the tick again.
    fn spare_fresh(&mut self, now: Instant) -> bool {
        let answered = self.spares.shared.fresh.load(Ordering::Acquire);
        if let Some((n, at)) = self.spares.freshen {
            let young = now.saturating_duration_since(at) < FRESH_FOR;
            if young && answered == n {
                self.spares.freshen = None;
                return true;
            }
            if young {
                // Asked, not answered yet: the answer's note looks again.
                self.spares.arm(at + FRESH_FOR);
                return false;
            }
        }
        self.spares.freshen_seq += 1;
        let n = self.spares.freshen_seq;
        if self.spares.send(Job::Freshen(n)).is_ok() {
            self.spares.freshen = Some((n, now));
            self.spares.arm(now + FRESH_FOR);
        } else {
            self.spares.freshen = None;
        }
        false
    }

    /// [`Wake::SpareSpawned`].
    pub(crate) fn on_spare_spawned(&mut self, done: SpareSpawned) {
        let SpareSpawned { id, session } = done;
        let now = Instant::now();
        let matches = self
            .spares
            .slot
            .as_ref()
            .is_some_and(|s| s.id == id && s.state == SlotState::Booked);
        match session {
            Ok(session) if matches => {
                let Some(slot) = self.spares.slot.as_mut() else {
                    return;
                };
                publish_spare_sid(Some((
                    session.ctx.self_id.clone(),
                    session.pid,
                    Arc::clone(&slot.link),
                )));
                let parked = slot.link.witness.parked_seen.get().is_some();
                // A start record that came in before this completion (its note
                // found the slot still Booked and was dropped): the shell ran
                // past its hold, as `on_spare_note` judges one seen later.
                let ran_past = slot.link.witness.start_seen.get().is_some();
                slot.session = Some(session);
                slot.state = if parked {
                    SlotState::Parked
                } else {
                    SlotState::Spawned
                };
                slot.since = now;
                if ran_past {
                    aterm_log::warn!("spare: a parked spare signed a start record; spares off");
                    self.spares.parked_failed = Some(parked_key(&slot.inputs));
                    self.retire_spare("ran_past_hold", None);
                    return;
                }
                self.spares.arm(now);
            }
            Ok(session) => {
                // Retired while it was spawning.
                self.spares.retire_session(session, false);
            }
            Err(e) if matches => {
                aterm_log::info!("spare: spawn failed: {e}");
                self.spares.slot = None;
                self.spares.tombstone(id);
                bump(&mut self.spares.discards, "spawn");
            }
            Err(_) => {}
        }
    }

    /// [`Wake::SpareNote`].
    pub(crate) fn on_spare_note(&mut self, id: u64, note: SpareNote) {
        let now = Instant::now();
        if note == SpareNote::Gate {
            self.spares.arm(now);
            return;
        }
        let Some(slot) = self.spares.slot.as_mut() else {
            return;
        };
        if slot.id != id {
            return;
        }
        match note {
            SpareNote::Gate => {}
            SpareNote::Parked => {
                if slot.state == SlotState::Spawned {
                    slot.state = SlotState::Parked;
                    slot.since = now;
                    self.spares.arm(now);
                }
            }
            SpareNote::AtStart => {
                let kind = slot.link.witness.start_seen.get().map(|s| s.mark.kind);
                match (slot.state, kind) {
                    (SlotState::Warming, Some(StartKind::Spare)) => {
                        let reply: crate::spawn::WakePost = match &self.proxy {
                            Some(proxy) => crate::spawn::post_to(proxy),
                            // No loop (a test): the verdict comes back through it.
                            None => Arc::new(|_| false),
                        };
                        // Its rc ran from the `run`, when it entered Warming.
                        let run_at = slot.since;
                        slot.state = SlotState::Verifying;
                        slot.since = now;
                        let job = Job::Verify(Box::new(VerifyJob {
                            id,
                            witness: Arc::clone(&slot.link.witness),
                            sid: slot
                                .session
                                .as_ref()
                                .map(|s| s.ctx.self_id.as_str().to_owned())
                                .unwrap_or_default(),
                            inputs: slot.inputs.clone(),
                            generation: slot.generation,
                            run_at,
                            reply,
                        }));
                        if self.spares.send(job).is_err() {
                            self.retire_spare("thread", None);
                        }
                    }
                    // A start record from a shell that should still be held: it
                    // ran past a hold.
                    (SlotState::Spawned | SlotState::Parked, _) => {
                        aterm_log::warn!("spare: a parked spare signed a start record; spares off");
                        self.spares.parked_failed = Some(parked_key(&slot.inputs));
                        self.retire_spare("ran_past_hold", None);
                    }
                    (SlotState::Warming, _) => {
                        self.retire_spare(
                            "ran_past_hold",
                            Some(Verdict::Deterministic("ran-past-hold")),
                        );
                    }
                    _ => {}
                }
            }
            SpareNote::Gone => {
                let failure = slot.state.warm().then_some(Verdict::Backoff("gone"));
                self.retire_spare("gone", failure);
            }
            SpareNote::Verified(verdict) => {
                if slot.state != SlotState::Verifying {
                    return;
                }
                let tainted = slot.link.tainted();
                let verdict = match verdict {
                    Verdict::Ok if tainted & taint::CONTROL != 0 => {
                        Verdict::Deterministic("control")
                    }
                    Verdict::Ok if tainted != 0 => Verdict::Backoff("host_effect"),
                    v => v,
                };
                if verdict == Verdict::Ok {
                    slot.state = SlotState::Ready;
                    slot.since = now;
                    self.spares.arm(now + ready_max_age());
                } else {
                    let reason = match verdict {
                        Verdict::Deterministic(r) | Verdict::Backoff(r) => r,
                        Verdict::Ok => "verify",
                    };
                    self.retire_spare(reason, Some(verdict));
                }
            }
        }
    }

    /// The backstop at the top of `user_event`: a wake naming a spare that
    /// slipped past its reader's post (design §5.3). An output wake clears that
    /// session's latch; everything else is dropped.
    pub(crate) fn on_spare_backstop(&mut self, ev: &Wake) {
        if let Wake::Output { session, .. } = ev
            && let Some(slot) = &self.spares.slot
            && slot.id == *session
            && let Some(s) = &slot.session
        {
            s.output_wake_pending.store(0, Ordering::Relaxed);
        }
    }

    /// Retire the spare: `reason` counted, a warm tier's discard budgeted, the
    /// gate told of a `failure`, the session dropped on the `aterm-spare`
    /// thread (a warm tier's whole session SIGKILLed first).
    pub(crate) fn retire_spare(&mut self, reason: &'static str, failure: Option<Verdict>) {
        let Some(slot) = self.spares.slot.take() else {
            return;
        };
        let now = Instant::now();
        self.spares.tombstone(slot.id);
        publish_spare_sid(None);
        bump(&mut self.spares.discards, reason);
        if slot.state.warm() {
            prune(&mut self.spares.warm_discards, now, HOUR);
            self.spares.warm_discards.push_back(now);
        }
        if let Some(verdict) = failure {
            let generation = if slot.generation != 0 {
                slot.generation
            } else {
                self.spares.shared.generation.load(Ordering::Acquire)
            };
            let _ = self.spares.send(Job::Fail(generation, verdict));
        }
        if let Some(session) = slot.session {
            let kill = slot.state.warm() && slot.state != SlotState::Ready;
            self.spares.retire_session(session, kill);
        }
        self.spares.arm(now);
        self.publish_spare();
    }

    /// The final exit: every spare retired and the thread joined, before the
    /// final reap and the keeper's BYE (`mem::forget(app)` runs no RAII).
    pub(crate) fn shutdown_spares(&mut self) {
        if self.spares.slot.is_some() {
            self.retire_spare("quit", None);
        }
        stop_thread();
    }

    /// Critical memory pressure: retire the spare and hold off for 10 minutes.
    /// Its one caller is the macOS-only `Wake::MemoryPressure` arm.
    #[cfg(target_os = "macos")]
    pub(crate) fn note_spare_pressure(&mut self) {
        self.spares.pressure_at = Some(Instant::now());
        self.retire_spare("pressure", None);
    }

    /// An atpkg pass started or ended: a warm spare's rc ran before it.
    pub(crate) fn note_spare_pass(&mut self, active: bool) {
        let was = std::mem::replace(&mut self.spares.pass_active, active);
        if active && !was && self.spares.slot.as_ref().is_some_and(|s| s.state.warm()) {
            self.retire_spare("pass", None);
        }
        if was && !active {
            self.spares.arm(Instant::now());
        }
    }

    /// The config was reloaded: a spare spawned by another factory goes, and a
    /// mode change is applied now.
    pub(crate) fn spare_config_changed(&mut self) {
        let fingerprint = self.session_factory.spawn_fingerprint();
        let stale = self
            .spares
            .slot
            .as_ref()
            .is_some_and(|s| s.fingerprint != fingerprint);
        if stale {
            self.retire_spare("factory", None);
        }
        self.spares.arm(Instant::now());
    }

    /// The colors or appearance changed: a warm spare's rc may have read them.
    pub(crate) fn spare_colors_changed(&mut self) {
        if self.spares.slot.as_ref().is_some_and(|s| s.state.warm()) {
            self.retire_spare("colors", None);
        }
    }

    /// A landing (adopted or cold) or a focus change: the next spare is booked
    /// once things settle.
    pub(crate) fn spare_poke(&self) {
        let settle = debug_ms("SETTLE").unwrap_or(SETTLE);
        self.spares.arm(Instant::now() + settle);
    }

    /// Adoption's gates G1–G10 (design §7.3) for a spawn landing at `place` in
    /// `window` at `grid`, asked for as `ask` says. `Ok` takes the spare out of
    /// the slot; `Err` names the miss.
    pub(crate) fn try_adopt_spare(
        &mut self,
        window: WindowId,
        asked_from: Option<WindowId>,
        grid: Grid,
        ask: &crate::tab_spawn::SpawnAsk,
    ) -> Result<(Slot, Session), &'static str> {
        let now = Instant::now();
        // G1
        if self.spare_mode() == SpareMode::Off {
            return Err("off");
        }
        // G2
        if !matches!(ask.admission, crate::tab_spawn::SpawnAdmission::Plain)
            || ask.identity.is_some()
            || ask.identity_prep == crate::tab_spawn::IdentityPrep::Create
            || ask.observe.is_some()
            || ask.folder_of.is_some()
            // Its own theme (`spawn theme=`): a warm spare's rc read the
            // instance's colours, and the newborn's must be on its engine
            // before its reader runs.
            || ask.theme.is_some()
        {
            return Err("ask");
        }
        let fingerprint = self.session_factory.spawn_fingerprint();
        let colors = self.engine_color_fanouts;
        let focused = asked_from.and_then(|w| self.focused_pane_folder(w));
        let in_flight = self.update_handoff_in_flight();
        let generation = self.spares.shared.generation.load(Ordering::Acquire);
        let eligible = self.spares.shared.eligible.load(Ordering::Acquire);
        let Some(slot) = &self.spares.slot else {
            return Err("none");
        };
        // G3
        match slot.state {
            SlotState::Parked | SlotState::Warming | SlotState::Ready => {}
            SlotState::Booked | SlotState::Spawned | SlotState::Verifying => return Err("booked"),
        }
        let warm = slot.state.warm();
        // G4
        if fingerprint != slot.fingerprint {
            return Err("factory");
        }
        if warm && colors != slot.colors {
            return Err("colors");
        }
        // G5
        match (&ask.cwd, focused) {
            (Some(cwd), _) if Some(cwd) != slot.cwd.as_ref() => return Err("folder"),
            (Some(_), _) => {}
            (None, Some(f)) if f.held => return Err("held"),
            (None, Some(f)) if Some(&f.reported) != slot.cwd.as_ref() => return Err("folder"),
            (None, None) if slot.cwd.is_some() => return Err("folder"),
            (None, _) => {}
        }
        // G6
        if warm && grid != slot.grid && !self.spare_blank(slot) {
            return Err("grid");
        }
        // G7
        let age = now.saturating_duration_since(slot.since);
        let too_old = match slot.state {
            SlotState::Parked => age >= PARKED_MAX_AGE,
            SlotState::Warming => age >= slot.t_ready,
            _ => age >= ready_max_age(),
        };
        if too_old {
            return Err("age");
        }
        // G8
        if warm && generation != slot.generation {
            return Err("stale");
        }
        if warm && !eligible {
            return Err("gate");
        }
        if slot.link.tainted() != 0 {
            return Err("taint");
        }
        // G9
        if slot.link.exited()
            || slot
                .session
                .as_ref()
                .is_none_or(|s| s.pty_closed_at.get().is_some())
        {
            return Err("process");
        }
        // G10
        if in_flight {
            return Err("update");
        }
        let _ = window;
        let mut slot = self.spares.slot.take().ok_or("none")?;
        let session = slot.session.take().ok_or("none")?;
        publish_spare_sid(None);
        Ok((slot, session))
    }

    /// Whether a warm spare's screen is blank (primary grid, no scrollback):
    /// it may land at any grid.
    fn spare_blank(&self, slot: &Slot) -> bool {
        slot.session.as_ref().is_some_and(|s| {
            let t = crate::term_lock(&s.term);
            t.grid().scrollback_lines() == 0
                && (0..usize::from(t.grid().rows()))
                    .all(|r| t.row_text(r).is_none_or(|l| l.trim().is_empty()))
        })
    }

    /// An adopted spare landed in the pool (design §8 step 4): the session goes
    /// live in `window`, its reader's `Ready` is replayed, the primer runs, and
    /// the shell is told `go`. The session's own wiring (host reach, census,
    /// latches, start, cast) was made as it landed
    /// ([`Self::prepare_adopted_spare`]).
    pub(crate) fn adopt_live(&mut self, id: u64, window: WindowId) {
        let Some(at) = self.spares.transit.iter().position(|t| t.id == id) else {
            return;
        };
        let transit = self.spares.transit.remove(at);
        let _ = transit
            .link
            .witness
            .born
            .set((transit.tier.as_str(), transit.age.as_millis()));
        transit.link.go_live(window);
        // A write that put bytes in its input queue while it was nobody's (an
        // rc's query answered by its reader) fired the input watch into the
        // swallowing post, and the watch fires once until its probe re-arms
        // it: armed again now, and woken when input waits, as an attach does
        // (`input_stall::watch_input`), so the tab's stall watch is a cold
        // tab's from here.
        if let Some(s) = self.pool.get(id) {
            s.ctx.sink.rearm_input_hook();
            if s.ctx.sink.input_backlog().is_some_and(|b| b.unread() > 0) {
                s.ctx.sink.wake_input_hook();
            }
        }
        if transit.link.reader_started() {
            self.store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .mark_alive(id);
        }
        crate::spawn::prime_agents_if_due();
        let told = transit.link.witness.tell("go");
        let tier = match transit.tier {
            SlotState::Parked => 0,
            SlotState::Warming | SlotState::Verifying => 1,
            _ => 2,
        };
        self.spares.adopted[tier] += 1;
        if let Some(s) = self.pool.get(id) {
            if s.pty_closed_at.get().is_some()
                && let Some(proxy) = &self.proxy
            {
                let _ = proxy.send_event(Wake::Exit {
                    session: id,
                    window,
                });
            }
            s.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record(
                    "spare adopted",
                    format!(
                        "tier={} age_ms={} rc_ms={}",
                        transit.tier.as_str(),
                        transit.age.as_millis(),
                        transit
                            .rc_ms
                            .map_or_else(|| "-".to_owned(), |ms| ms.to_string())
                    ),
                );
        }
        if !told {
            aterm_log::warn!("spare {id}: the release could not be written");
        }
        // A parked spare's startup files run now, in view: a visible run the
        // gate learns from, started at the release.
        if transit.tier == SlotState::Parked {
            self.observe_spare_run(id, transit.inputs);
        }
        self.spare_poke();
        self.publish_spare();
    }

    /// The session-side half of an adoption, on the owned session before it is
    /// installed: the cold path's host wiring, the PTY census, fresh latches,
    /// the shell's start, the keeper and the cast's timeline (design §8 step 4).
    pub(crate) fn prepare_adopted_spare(&self, session: &mut Session, window: WindowId) {
        let reach = crate::spawn::session_host_reach();
        if let Some(proxy) = &self.proxy {
            crate::spawn::arm_host_reach(
                &session.term,
                reach,
                &self.session_factory,
                proxy,
                session.id,
                window,
                &session.ctx.witness,
            );
        }
        session._pty_census = crate::metrics::PtyCensus::counted();
        session.output_wake_pending.store(0, Ordering::Relaxed);
        session.last_output_ns.store(0, Ordering::Relaxed);
        session.output_request_booked_ns.store(0, Ordering::Relaxed);
        let now = Instant::now();
        session.shell_start = Some(crate::spawn::ShellStart {
            at: now,
            name: crate::spawn::interactive_shell_name(
                self.session_factory.shell_override.as_deref(),
            ),
            input: session.ctx.sink.driven_input(),
        });
        session
            .ctx
            .cast
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .adopt_spare(now);
        crate::keeper_link::register(session, None);
    }

    /// Tell the gate about a visible rc run that starts now in session `id`: a
    /// cold tab's, or a parked spare's release (design §7.1).
    pub(crate) fn observe_spare_run(&mut self, id: u64, inputs: GenInputs) {
        if self.spare_mode() != SpareMode::Auto {
            return;
        }
        let Some(s) = self.pool.get(id) else {
            return;
        };
        if !aterm_core::shell_integration::has_spare_hold(inputs.shell)
            || !s.rekey_channel
            || s.identity.is_some()
        {
            return;
        }
        let w = &s.ctx.witness;
        w.observed.store(true, Ordering::Release);
        OBSERVED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((s.pid, Arc::downgrade(w)));
        let started_at = s.shell_start.as_ref().map_or_else(Instant::now, |st| st.at);
        let input0 = s.shell_start.as_ref().map(|st| st.input);
        let obs = Observation {
            witness: Arc::clone(w),
            sid: s.ctx.self_id.as_str().to_owned(),
            started_at,
            input0,
            // A fresh session's counters start at zero, and a spare's moved
            // nothing before its release (its effects only tainted it).
            host_effects0: 0,
            early_control0: 0,
            inputs,
        };
        let _ = self.spares.send(Job::Observe(Box::new(obs)));
    }
}

impl Spares {
    /// Drop a retired spare's session on the `aterm-spare` thread (here when it
    /// has none), `kill` first SIGKILLing its whole session.
    fn retire_session(&mut self, session: Session, kill: bool) {
        if let Err(Job::Retire(session, kill)) = self.send(Job::Retire(Box::new(session), kill)) {
            if kill {
                kill_session(session.pid);
            }
            drop(session);
        }
    }
}

#[cfg(test)]
#[path = "spare_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "spare_conformance.rs"]
mod spare_conformance;
