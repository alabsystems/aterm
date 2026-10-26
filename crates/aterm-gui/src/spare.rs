// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Warm spare shell management ([`Spares`]).
//!
//! Keeps one pre-spawned shell waiting on a [`SpareFifo`] so plain new-tab
//! requests adopt it instead of forking:
//! - Parked: waits before startup files; can retarget its directory on release ([`AdoptCheck::verdict`]).
//! - Warm: runs startup files ahead of time once [`GateBook`] certifies the rc
//!   generation ([`rc_generation`], [`Reported`], [`escape_scan`], [`App::spare_warm_gates`]).
//!
//! Until adoption, wakes are swallowed ([`SpareLink::post`]) and host/control
//! effects only taint the spare ([`wire_spare_taint`], [`control_refusal`]).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_core::terminal::{StartKind, StartMark, Terminal};
use aterm_window::event_loop::EventLoopProxy;

use crate::{App, Session, Wake, WindowId};

/// Maximum wait for a spawned spare to report its parked mark.
pub(crate) const PARKED_DEADLINE: Duration = Duration::from_secs(1);
/// Maximum age of a parked spare before replacement.
pub(crate) const PARKED_MAX_AGE: Duration = Duration::from_secs(60 * 60);
/// Maximum age of a verified warm spare before discard.
pub(crate) const READY_MAX_AGE: Duration = Duration::from_secs(10 * 60);
/// Maximum time since user activity to allow warm promotion.
pub(crate) const PERSON_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Initial grace period after first window creation before booking spares.
pub(crate) const BOOT_GRACE: Duration = Duration::from_secs(1);
const SPARE_FORK_PARK_WAIT: Duration = Duration::from_millis(20);
/// Settle delay after a tab landing or input before booking the next spare.
pub(crate) const SETTLE: Duration = Duration::from_millis(150);
/// Input idle duration required before warm promotion.
pub(crate) const PROMOTE_IDLE: Duration = Duration::from_secs(1);
/// Folder stability duration required before warm promotion.
pub(crate) const PROMOTE_FOLDER_STABLE: Duration = Duration::from_secs(3);
/// Maximum parked spawns per rolling hour.
pub(crate) const PARKED_BUDGET: usize = 60;
/// Maximum warm spare discards per rolling hour.
pub(crate) const WARM_BUDGET: usize = 8;
const HOUR: Duration = Duration::from_secs(60 * 60);
/// Minimum interval between spare replacements.
pub(crate) const REPLACE_MIN: Duration = Duration::from_secs(2);
/// Delay before replacing a parked spare after the target folder or window moves.
pub(crate) const REPLACE_PARKED_AFTER: Duration = Duration::from_secs(1);
/// Delay before replacing a warm spare after the target folder or window moves.
pub(crate) const REPLACE_WARM_AFTER: Duration = Duration::from_secs(10);
/// Holdoff duration after a critical memory-pressure episode.
pub(crate) const PRESSURE_HOLD: Duration = Duration::from_secs(10 * 60);
const TOMBSTONES: usize = 32;
/// Consecutive clean visible runs required to open a generation's warm gate.
pub(crate) const CLEAN_STREAK: u32 = 3;
/// Maximum startup duration for a visible run to count as a clean sample.
pub(crate) const SAMPLE_WINDOW: Duration = Duration::from_secs(2);
const BACKOFF_FIRST: Duration = Duration::from_secs(60 * 60);
const BACKOFF_SECOND: Duration = Duration::from_secs(24 * 60 * 60);
const SCAN_EVERY: Duration = Duration::from_secs(2);
const SCAN_GRACE: Duration = Duration::from_millis(100);
const T_READY_MIN: Duration = Duration::from_secs(1);
const T_READY_MAX: Duration = Duration::from_secs(8);
const GATE_KEEP: usize = 16;
const FRESH_FOR: Duration = Duration::from_secs(2);
const GATE_FILE: &str = "spare-gate-v1.tsv";
const GATE_FILE_MAX: u64 = 128 * 1024;

/// Configured `spare_shell` mode (`auto`, `parked`, or `off`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpareMode {
    Auto,
    Parked,
    Off,
}

impl SpareMode {
    /// Accepted config spellings in `--validate-config` order.
    pub(crate) const SPELLINGS: [&'static str; 3] = ["auto", "parked", "off"];

    /// Parse a `spare_shell` value.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "parked" => Some(Self::Parked),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// Shell start mark and session state captured by the PTY reader.
#[derive(Clone, Debug)]
pub(crate) struct StartSeen {
    pub(crate) at: Instant,
    pub(crate) mark: StartMark,
    pub(crate) input: Option<aterm_session::sink::DrivenInput>,
    pub(crate) fg_pgrp: i32,
    pub(crate) image: Option<PathBuf>,
    pub(crate) alt_screen: bool,
}

/// Per-session startup and host-effect observations read by the spare gate.
#[derive(Default)]
pub(crate) struct Witness {
    pub(crate) parked_seen: OnceLock<Instant>,
    pub(crate) start_seen: OnceLock<StartSeen>,
    pub(crate) host_effects: AtomicU32,
    pub(crate) early_control: AtomicU32,
    pub(crate) askpass: AtomicU32,
    pub(crate) shell: OnceLock<(i32, Option<PathBuf>)>,
    shell_version: OnceLock<i32>,
    fifo_held: AtomicBool,
    fifo: Mutex<Option<SpareFifo>>,
    observed: AtomicBool,
    born: OnceLock<(&'static str, u128)>,
}

impl Witness {
    /// Record the spawned shell pid, executable path, and pid version.
    pub(crate) fn note_shell(&self, pid: i32, version: Option<i32>) {
        let _ = self.shell.set((pid, shell_image(pid)));
        if let Some(version) = version {
            let _ = self.shell_version.set(version);
        }
    }

    /// Hold `fifo` until the shell starts its first prompt or the session ends.
    pub(crate) fn hold_fifo(&self, fifo: SpareFifo) {
        *self.fifo.lock().unwrap_or_else(|p| p.into_inner()) = Some(fifo);
        self.fifo_held.store(true, Ordering::Release);
    }

    /// Write a protocol line to the held FIFO.
    pub(crate) fn tell(&self, line: &str) -> bool {
        self.fifo
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|f| f.tell(line).is_ok())
    }

    /// Take and release the held FIFO.
    pub(crate) fn take_fifo(&self) -> Option<SpareFifo> {
        self.fifo_held.store(false, Ordering::Release);
        self.fifo.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    /// Format `born=` and `spare_age_ms=` status fields.
    pub(crate) fn born_fields(&self) -> String {
        match self.born.get() {
            Some((tier, age)) => format!("born=spare-{tier} spare_age_ms={age}"),
            None => "born=cold".to_owned(),
        }
    }
}

/// Record parked/start marks and release the FIFO on first prompt after processing a PTY chunk.
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

/// Read `pid`'s kernel pid version (`p_idversion` on macOS).
pub(crate) fn shell_version(pid: i32) -> Option<i32> {
    #[cfg(target_os = "macos")]
    {
        (pid > 0)
            .then(|| bsdinfo_uniq(pid))
            .flatten()
            .map(|info| info.p_idversion)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

fn shell_image(pid: i32) -> Option<PathBuf> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        (pid > 0)
            .then(|| aterm_uds::process::read_process_path(pid))
            .flatten()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// Hold FIFO (`<control dir>/spare/<pid>-<birth>-<sid>`) and its `<sid>` symlink.
pub(crate) struct SpareFifo {
    path: PathBuf,
    link: Option<PathBuf>,
    file: std::fs::File,
}

impl SpareFifo {
    /// Create and open session `sid`'s FIFO and symlink.
    #[cfg(unix)]
    pub(crate) fn make(sid: &aterm_session::SessionId) -> std::io::Result<Self> {
        let dir = crate::control_auth::private_subdir(aterm_core::shell_integration::SPARE_DIR)
            .ok_or_else(|| std::io::Error::other("no private control dir"))?;
        let link = crate::control_auth::sid_file_in(&dir, sid)
            .ok_or_else(|| std::io::Error::other("not a minted sid"))?;
        let name = aterm_core::shell_integration::spare_fifo::name(sid.as_str());
        let mut fifo = Self::make_at(dir.join(&name))?;
        std::os::unix::fs::symlink(&name, &link)?;
        fifo.link = Some(link);
        Ok(fifo)
    }

    #[cfg(not(unix))]
    pub(crate) fn make(_sid: &aterm_session::SessionId) -> std::io::Result<Self> {
        Err(std::io::Error::other("no spare off unix"))
    }

    /// Create and open a FIFO at `path` without a symlink.
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

/// Bitflags recording pre-adoption host effects requested by a spare.
pub(crate) mod taint {
    pub(crate) const BELL: u32 = 1;
    pub(crate) const NOTIFY: u32 = 1 << 1;
    pub(crate) const CLIPBOARD: u32 = 1 << 2;
    pub(crate) const KITTY: u32 = 1 << 3;
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) const WINDOW_OP: u32 = 1 << 4;
    pub(crate) const CONTROL: u32 = 1 << 5;
    /// Askpass request from inside the spare (macOS only;
    /// [`askpass_refused`](super::askpass_refused)).
    #[cfg(target_os = "macos")]
    pub(crate) const ASKPASS: u32 = 1 << 6;
}

/// Lifecycle notifications sent from a spare's reader or worker thread to the main thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpareNote {
    Parked,
    AtStart,
    Gone,
    Verified(Verdict),
    Gate,
}

/// Outcome of verifying a warm spare at its hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Ok,
    Deterministic(&'static str),
    Backoff(&'static str),
}

/// Shared state between a spare's PTY reader and the main thread.
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

    /// Record a pre-adoption host effect ([`taint`]).
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

    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    /// Mark the spare as adopted into `window` so subsequent wakes are forwarded.
    pub(crate) fn go_live(&self, window: WindowId) {
        self.window.store(window.0, Ordering::Release);
        self.live.store(true, Ordering::Release);
    }

    fn send(&self, wake: Wake) -> bool {
        self.poster.as_ref().is_some_and(|post| post(wake))
    }

    /// Route a reader wake: forward if live, or convert to [`SpareNote`] while unowned.
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

    /// Build the [`crate::spawn::WakePost`] callback for this spare's reader.
    pub(crate) fn post(self: &Arc<Self>) -> crate::spawn::WakePost {
        let link = Arc::clone(self);
        Arc::new(move |wake| link.decide(wake))
    }
}

/// Spare link carried on [`crate::spawn::SessionFactory::spare`].
#[derive(Clone)]
pub(crate) struct SpareSpawn {
    pub(crate) link: Arc<SpareLink>,
}

/// Error returned when a spare's target working directory would fall back.
#[derive(Debug)]
pub(crate) struct FolderFallsBack;

impl std::fmt::Display for FolderFallsBack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the spare's folder would fall back")
    }
}

impl std::error::Error for FolderFallsBack {}

/// Install pre-adoption host callbacks on `term` that only taint `link`.
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

static SPARE_SIDS: std::sync::RwLock<Option<(aterm_session::SessionId, i32, Arc<SpareLink>)>> =
    std::sync::RwLock::new(None);

fn publish_spare_sid(entry: Option<(aterm_session::SessionId, i32, Arc<SpareLink>)>) {
    *SPARE_SIDS.write().unwrap_or_else(|p| p.into_inner()) = entry;
}

fn peer_inside(leader: i32) -> bool {
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

/// Taint the active spare if an unmatched control request targeted its session.
pub(crate) fn control_refusal(sid: Option<&aterm_session::SessionId>, local: Option<u64>) {
    let guard = SPARE_SIDS.read().unwrap_or_else(|p| p.into_inner());
    if let Some((spare_sid, pid, link)) = guard.as_ref()
        && (sid == Some(spare_sid) || (local == Some(link.id) && peer_inside(*pid)))
    {
        link.taint(taint::CONTROL);
    }
}

/// Count control requests originating from inside `witness`'s session before its start mark.
pub(crate) fn note_control(witness: &Witness) {
    if witness.start_seen.get().is_none()
        && let Some((pid, _)) = witness.shell.get()
        && peer_inside(*pid)
    {
        witness.early_control.fetch_add(1, Ordering::Relaxed);
    }
}

/// Refuse and taint askpass requests from an unowned spare, or count early requests on observed sessions.
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

static OBSERVED: Mutex<Vec<(i32, std::sync::Weak<Witness>)>> = Mutex::new(Vec::new());

/// Inputs captured on the main thread to compute an rc generation stamp.
#[derive(Clone, Debug)]
pub(crate) struct GenInputs {
    pub(crate) shell: aterm_core::shell_integration::ShellType,
    pub(crate) shell_path: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) launch_zdotdir: PathBuf,
    pub(crate) fingerprint: u64,
    pub(crate) containment: u8,
}

/// Startup directory and root paths reported by the most recent start mark.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reported {
    pub(crate) rc_dir: Option<PathBuf>,
    pub(crate) roots: Option<Vec<PathBuf>>,
}

fn roots_of(mark: &StartMark) -> Option<Vec<PathBuf>> {
    mark.roots
        .as_ref()
        .map(|roots| roots.iter().map(PathBuf::from).collect())
}

fn rc_paths(inputs: &GenInputs, reported: &Reported) -> Vec<PathBuf> {
    use aterm_core::shell_integration::ShellType;
    let z = reported.rc_dir.as_deref();
    let mut paths = Vec::new();
    match inputs.shell {
        ShellType::Zsh => {
            for f in ["/etc/zshenv", "/etc/zprofile", "/etc/zshrc", "/etc/zlogin"] {
                paths.push(PathBuf::from(f));
            }
            if cfg!(target_os = "linux") {
                for f in ["zshenv", "zprofile", "zshrc", "zlogin"] {
                    paths.push(Path::new("/etc/zsh").join(f));
                }
            }
            let h = &inputs.launch_zdotdir;
            paths.push(h.join(".zshenv"));
            let z = z.unwrap_or(h);
            for f in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
                paths.push(z.join(f));
            }
        }
        ShellType::Fish => {
            let mut folders: Vec<PathBuf> = Vec::new();
            if let Some((sysconf, vendor)) = reported.roots.as_deref().and_then(<[_]>::split_first)
            {
                paths.push(sysconf.join("config.fish"));
                folders.push(sysconf.join("conf.d"));
                folders.extend(vendor.iter().cloned());
            }
            let user = z.map_or_else(
                || {
                    std::env::var_os("XDG_CONFIG_HOME")
                        .filter(|x| !x.is_empty())
                        .map_or_else(|| inputs.home.join(".config"), PathBuf::from)
                        .join("fish")
                },
                Path::to_path_buf,
            );
            paths.push(user.join("config.fish"));
            folders.push(user.join("conf.d"));
            folders.push(user.join("themes"));
            for dir in folders {
                push_entries(&mut paths, dir);
            }
        }
        _ => {
            for f in ["/etc/profile", "/etc/bashrc"] {
                paths.push(PathBuf::from(f));
            }
            if cfg!(target_os = "linux") {
                paths.push(PathBuf::from("/etc/bash.bashrc"));
            }
            for f in [".bash_profile", ".bash_login", ".profile", ".bashrc"] {
                paths.push(inputs.home.join(f));
            }
        }
    }
    push_entries(&mut paths, inputs.home.join(".aterm").join("shell.d"));
    paths.push(PathBuf::from("/etc/paths"));
    push_entries(&mut paths, PathBuf::from("/etc/paths.d"));
    paths.push(inputs.shell_path.clone());
    paths
}

fn push_entries(paths: &mut Vec<PathBuf>, dir: PathBuf) {
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        names.sort();
        paths.extend(names);
    }
    paths.push(dir);
}

/// Compute the non-zero metadata hash over shell startup files, hooks, binary, and factory state.
pub(crate) fn rc_generation(inputs: &GenInputs, reported: &Reported) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "aterm-spare-generation-v1".hash(&mut h);
    crate::config_watcher::path_feed_stamp(&rc_paths(inputs, reported)).hash(&mut h);
    aterm_core::shell_integration::script_set_address().hash(&mut h);
    inputs.fingerprint.hash(&mut h);
    inputs.containment.hash(&mut h);
    h.finish().max(1)
}

/// Persisted gate state for one rc generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GenRecord {
    pub(crate) hash: u64,
    pub(crate) streak: u32,
    pub(crate) ineligible_until: u64,
    pub(crate) failures: Vec<u64>,
    pub(crate) max_rc_ms: u32,
    pub(crate) reason: String,
    pub(crate) touched: u64,
}

/// Verdict on a visible shell startup run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sample {
    Clean { rc_ms: u32 },
    Unclean,
    Inconclusive,
}

/// Learned per-generation eligibility streaks, backoffs, and persistence.
#[derive(Default)]
pub(crate) struct GateBook {
    gens: Vec<GenRecord>,
    path: Option<PathBuf>,
    loaded: bool,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

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
    pub(crate) const fn at(path: Option<PathBuf>) -> Self {
        Self {
            gens: Vec::new(),
            path,
            loaded: false,
        }
    }

    fn load(&mut self) {
        if std::mem::replace(&mut self.loaded, true) {
            return;
        }
        let text = self.path.as_ref().and_then(|path| {
            crate::seamless::read_regular_capped(path, GATE_FILE_MAX)
                .and_then(|bytes| String::from_utf8(bytes).ok())
        });
        let Some(text) = text else { return };
        let Some(gens) = parse_gate_records(&text) else {
            return;
        };
        self.gens = gens;
    }

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
        let Ok(mut file) = options.open(&tmp) else {
            return;
        };
        use std::io::Write as _;
        let written = file.write_all(text.as_bytes());
        drop(file);
        if written.is_err() || std::fs::rename(&tmp, path).is_err() {
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

    /// Record a visible run sample for generation `hash`.
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

    /// Record a warm spare verification failure for generation `hash`.
    pub(crate) fn fail(&mut self, hash: u64, verdict: Verdict) {
        self.fail_at(hash, verdict, unix_now());
    }

    /// Record a failure for `hash` at unix second `now`.
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

    /// Whether generation `hash` is currently eligible to run unseen.
    pub(crate) fn eligible(&mut self, hash: u64) -> bool {
        self.eligible_at(hash, unix_now())
    }

    /// Whether generation `hash` is eligible at unix second `now`.
    pub(crate) fn eligible_at(&mut self, hash: u64, now: u64) -> bool {
        self.load();
        self.gens
            .iter()
            .find(|g| g.hash == hash)
            .is_some_and(|g| g.streak >= CLEAN_STREAK && now >= g.ineligible_until)
    }

    /// Metrics status string for generation `hash`.
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

    /// Warm readiness deadline (`T_READY`) for generation `hash`.
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

/// Outcome of an escape scan over processes started during a shell's rc run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scan {
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux")),
        expect(
            dead_code,
            reason = "only the macOS and Linux escape scans observe a run"
        )
    )]
    Clean,
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux")),
        expect(
            dead_code,
            reason = "only the macOS and Linux escape scans observe a run"
        )
    )]
    Escaped,
    Unknown,
    RateLimited,
}

#[cfg(target_os = "macos")]
use aterm_uds::process::{
    BsdInfoWithUniqId, bsdinfo_uniq, launchd_idversion, launched_before, uniq_identifier,
};

#[cfg(target_os = "macos")]
fn short_parent(pid: i32) -> Option<i32> {
    let info = aterm_uds::process::read_short_info(pid)?;
    i32::try_from(info.pbsi_ppid).ok()
}

#[cfg(target_os = "macos")]
fn exited(pid: i32) -> bool {
    let (rc, record, size) = process_table_record(pid);
    exited_observation(rc, &record, size)
}

#[cfg(target_os = "macos")]
fn born_before(pid: i32, when: std::time::SystemTime) -> bool {
    let (rc, record, size) = process_table_record(pid);
    born_before_observation(rc, &record, size, when)
}

#[cfg(target_os = "macos")]
fn process_table_record(pid: i32) -> (libc::c_int, [u8; 1024], usize) {
    const KERN_PROC: libc::c_int = 14;
    const KERN_PROC_PID: libc::c_int = 1;
    let mut mib = [libc::CTL_KERN, KERN_PROC, KERN_PROC_PID, pid];
    let mut record = [0u8; 1024];
    let mut size: libc::size_t = record.len();
    // SAFETY: `record` holds `size` writable bytes and `size` reports capacity.
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

#[cfg(target_os = "macos")]
fn exited_observation(rc: libc::c_int, record: &[u8], size: usize) -> bool {
    const P_STAT: usize = 36;
    if rc != 0 || size > record.len() || size <= P_STAT {
        return false;
    }
    u32::from(record[P_STAT]) == libc::SZOMB
}

/// Scan for background processes or daemons escaped from `s_pid`'s startup window.
#[cfg(target_os = "macos")]
pub(crate) fn escape_scan(
    s_pid: i32,
    shell_version: Option<i32>,
    sid: &str,
    from: std::time::SystemTime,
    until: std::time::SystemTime,
) -> Scan {
    rescan_while_unknown(SCAN_ATTEMPTS, || {
        escape_scan_since(
            s_pid,
            shell_version,
            sid,
            from,
            until,
            uid_pids,
            bsdinfo_uniq,
        )
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
const SCAN_ATTEMPTS: usize = 3;

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rescan_while_unknown(attempts: usize, mut scan: impl FnMut() -> Scan) -> Scan {
    for _ in 0..attempts {
        match scan() {
            Scan::Unknown => {}
            verdict => return verdict,
        }
    }
    Scan::Unknown
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn shell_unknown(s_pid: i32, why: &str) -> Scan {
    aterm_log::info!("spare gate: scan unknown: the run's shell {s_pid} {why}");
    Scan::Unknown
}

/// `uid`'s live pids ([`aterm_uds::holders::pids_of_uid`]), or `None` when they cannot all be listed.
#[cfg(target_os = "macos")]
fn uid_pids(uid: u32) -> Option<Vec<i32>> {
    let pids = aterm_uds::holders::pids_of_uid(uid).ok()?;
    pids.into_iter().map(|p| i32::try_from(p).ok()).collect()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
const SCAN_SETTLE_LISTINGS: usize = 10;

#[cfg(any(target_os = "macos", target_os = "linux"))]
const SCAN_SETTLE_PAUSE: Duration = Duration::from_millis(1);

#[cfg(all(target_os = "macos", test))]
fn escape_scan_among(
    s_pid: i32,
    sid: &str,
    from: std::time::SystemTime,
    until: std::time::SystemTime,
    list: impl FnMut(u32) -> Option<Vec<i32>>,
    read_pid: impl FnMut(i32) -> Option<BsdInfoWithUniqId>,
) -> Scan {
    escape_scan_since(s_pid, None, sid, from, until, list, read_pid)
}

#[cfg(target_os = "macos")]
fn escape_scan_since(
    s_pid: i32,
    shell_version: Option<i32>,
    sid: &str,
    from: std::time::SystemTime,
    until: std::time::SystemTime,
    list: impl FnMut(u32) -> Option<Vec<i32>>,
    read_pid: impl FnMut(i32) -> Option<BsdInfoWithUniqId>,
) -> Scan {
    escape_scan_with_owner(
        s_pid,
        shell_version,
        sid,
        from,
        until,
        list,
        read_pid,
        environment_names,
        stdio_dead,
    )
}

#[cfg(target_os = "macos")]
fn environment_names(pid: i32, sid: &str) -> Option<bool> {
    u32::try_from(pid)
        .ok()
        .and_then(atpkg::caller_shell::process_args)
        .map(|args| {
            args.env.is_empty()
                || args.env_var(aterm_types::domain::ENV_PARENT_SESSION_ID) == Some(sid)
        })
}

#[cfg(target_os = "macos")]
fn stdio_dead(pid: i32) -> Option<bool> {
    u32::try_from(pid)
        .ok()
        .and_then(aterm_uds::holders::stdio_on_dead_vnode)
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeaderLine {
    Outside,
    Run,
    Unknown,
}

#[cfg(target_os = "macos")]
const PROC_FLAG_CONTROLT: u32 = 0x80;

#[cfg(target_os = "macos")]
#[allow(
    clippy::too_many_arguments,
    reason = "the scan's own inputs plus its four injected host reads"
)]
fn escape_scan_with_owner(
    s_pid: i32,
    shell_version: Option<i32>,
    sid: &str,
    from: std::time::SystemTime,
    until: std::time::SystemTime,
    mut list: impl FnMut(u32) -> Option<Vec<i32>>,
    mut read_pid: impl FnMut(i32) -> Option<BsdInfoWithUniqId>,
    mut read_owner: impl FnMut(i32, &str) -> Option<bool>,
    mut read_stdio_dead: impl FnMut(i32) -> Option<bool>,
) -> Scan {
    if s_pid <= 1 {
        return shell_unknown(s_pid, "unread");
    }
    // SAFETY: `getuid` is a side-effect-free getter.
    let uid = unsafe { libc::getuid() };
    let mut read = |p: i32| {
        read_pid(p).filter(|i| p > 0 && i.pbsd.pbi_pid == p as u32 && i.pbsd.pbi_uid == uid)
    };
    let Some(shell) = read(s_pid) else {
        return shell_unknown(s_pid, "unread");
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
            None if exited(p) => {}
            None if born_before(p, shell_start) => older.push(p),
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
    let held: std::collections::HashSet<i32> = infos.iter().map(|(p, _)| *p).collect();
    let gone: std::collections::HashSet<i32> = pids
        .iter()
        .copied()
        .filter(|p| !held.contains(p) && !unsettled.contains(p) && !older.contains(p))
        .collect();
    let mut stale = false;
    let naming_gone: Vec<usize> = (0..infos.len())
        .filter(|&k| gone.contains(&(infos[k].1.pbsd.pbi_ppid as i32)))
        .collect();
    for k in naming_gone {
        let (p, info) = infos[k];
        match read(p) {
            Some(fresh)
                if fresh.p_uniqueid == info.p_uniqueid
                    && !gone.contains(&(fresh.pbsd.pbi_ppid as i32)) =>
            {
                infos[k].1 = fresh;
            }
            None if (exited(p) || list(uid).is_some_and(|pids| !pids.contains(&p)))
                && !orphans_live(&infos, p, &mut read, &mut list, uid, None) => {}
            _ => stale = true,
        }
    }
    let mut incomplete = stale || !unsettled.is_empty();
    let shell_listed = infos
        .iter()
        .any(|(p, i)| *p == s_pid && i.p_uniqueid == shell.p_uniqueid);
    let parent_of = |pid: i32| -> Option<i32> {
        infos
            .iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, i)| i.pbsd.pbi_ppid as i32)
    };
    let launchd = launchd_idversion();
    // SAFETY: `getsid` reads the process table.
    let leads_session = unsafe { libc::getsid(s_pid) } == s_pid;
    let mut older_sessions: Option<std::collections::HashSet<i32>> = None;
    let leader_line = |leader: i32| -> LeaderLine {
        // SAFETY: `getsid` reads the process table.
        if unsafe { libc::getsid(leader) } != leader {
            return LeaderLine::Unknown;
        }
        let record = |pid: i32| infos.iter().find(|(p, _)| *p == pid).map(|(_, i)| i);
        let (mut at, mut child) = match record(leader) {
            Some(info) => (leader, info),
            None => return LeaderLine::Unknown,
        };
        for _ in 0..64 {
            if at == s_pid {
                return LeaderLine::Run;
            }
            if child.p_uniqueid < shell.p_uniqueid {
                return LeaderLine::Outside;
            }
            let up = child.pbsd.pbi_ppid as i32;
            match record(up) {
                Some(parent) if up > 1 && parent.p_uniqueid < child.p_uniqueid => {
                    (at, child) = (up, parent);
                }
                _ => return LeaderLine::Unknown,
            }
        }
        LeaderLine::Unknown
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
        let run_orphan = |uniqueid: u64, launcher: i32| {
            uniqueid > shell.p_uniqueid
                && launchd.is_none_or(|v| launcher != v)
                && !shell_version.is_some_and(|v| launched_before(launcher, v))
        };
        let mut reparented =
            info.pbsd.pbi_ppid == 1 && run_orphan(info.p_uniqueid, info.p_orig_ppidversion);
        let attached = {
            let mut at = *pid;
            let mut attached = false;
            for _ in 0..64 {
                let foreign = parent_of(at).is_none();
                let up = if foreign {
                    short_parent(at)
                } else {
                    parent_of(at)
                };
                match up {
                    Some(p) if p == s_pid => {
                        attached = true;
                        break;
                    }
                    Some(p) if p > 1 => at = p,
                    Some(1) if foreign => {
                        reparented |= uniq_identifier(at)
                            .is_some_and(|f| run_orphan(f.p_uniqueid, f.p_orig_ppidversion));
                        break;
                    }
                    _ => break,
                }
            }
            attached
        };
        if !(reparented || attached) {
            continue;
        }
        // SAFETY: `getsid` reads the process table.
        let session = unsafe { libc::getsid(*pid) };
        match read(*pid) {
            Some(now)
                if now.p_uniqueid == info.p_uniqueid && now.pbsd.pbi_ppid != info.pbsd.pbi_ppid =>
            {
                incomplete = true;
                continue;
            }
            Some(now) if now.p_uniqueid == info.p_uniqueid => {}
            Some(_) => {
                let older = leads_session.then(|| {
                    &*older_sessions.get_or_insert_with(|| {
                        older_sessions_of(&infos, &mut read, shell.p_uniqueid)
                    })
                });
                incomplete |= orphans_live(&infos, *pid, &mut read, &mut list, uid, older);
                continue;
            }
            None => {
                let older = leads_session.then(|| {
                    &*older_sessions.get_or_insert_with(|| {
                        older_sessions_of(&infos, &mut read, shell.p_uniqueid)
                    })
                });
                if orphans_live(&infos, *pid, &mut read, &mut list, uid, older)
                    || (!exited(*pid) && !list(uid).is_some_and(|pids| !pids.contains(pid)))
                {
                    incomplete = true;
                }
                continue;
            }
        }
        if session < 0 {
            incomplete = true;
            continue;
        }
        let left_session = attached && session != s_pid;
        if !(reparented || left_session) {
            continue;
        }
        let in_session = session == s_pid;
        let mut stdio = "-";
        if reparented && leads_session && !in_session && session > 0 {
            let line = leader_line(session);
            if session != *pid {
                let older = older_sessions
                    .get_or_insert_with(|| older_sessions_of(&infos, &mut read, shell.p_uniqueid));
                if older.contains(&session) || line == LeaderLine::Outside {
                    continue;
                }
            }
            if !attached && line != LeaderLine::Run {
                if info.pbsd.pbi_flags & PROC_FLAG_CONTROLT != 0 {
                    continue;
                }
                stdio = match read_stdio_dead(*pid) {
                    Some(true)
                        if read(*pid).is_some_and(|now| now.p_uniqueid == info.p_uniqueid) =>
                    {
                        "dead"
                    }
                    Some(false) => "live",
                    _ => "unread",
                };
            }
        }
        let names_sid = in_session
            || left_session
            || match read_owner(*pid, sid) {
                Some(owned) => owned,
                None => match read(*pid) {
                    Some(now) => now.p_uniqueid == info.p_uniqueid,
                    None => {
                        let older = leads_session.then(|| {
                            &*older_sessions.get_or_insert_with(|| {
                                older_sessions_of(&infos, &mut read, shell.p_uniqueid)
                            })
                        });
                        if orphans_live(&infos, *pid, &mut read, &mut list, uid, older)
                            || (!exited(*pid) && !list(uid).is_some_and(|pids| !pids.contains(pid)))
                        {
                            incomplete = true;
                        }
                        false
                    }
                },
            };
        if names_sid {
            let comm: Vec<u8> = info
                .pbsd
                .pbi_comm
                .iter()
                .take_while(|c| **c != 0)
                .map(|c| *c as u8)
                .collect();
            aterm_log::info!(
                "spare gate: escape: pid={pid} ppid={} session={session} \
                 in_session={in_session} reparented={reparented} attached={attached} \
                 flags={:#x} stdio={stdio} comm={:?}",
                info.pbsd.pbi_ppid,
                info.pbsd.pbi_flags,
                String::from_utf8_lossy(&comm),
            );
            escaped = true;
            break;
        }
    }
    if !read(s_pid).is_some_and(|now| now.p_uniqueid == shell.p_uniqueid) {
        return shell_unknown(s_pid, "gone");
    }
    if escaped {
        Scan::Escaped
    } else if !shell_listed {
        shell_unknown(s_pid, "unlisted")
    } else if incomplete {
        Scan::Unknown
    } else {
        Scan::Clean
    }
}

#[cfg(target_os = "macos")]
fn older_sessions_of(
    infos: &[(i32, BsdInfoWithUniqId)],
    read: &mut impl FnMut(i32) -> Option<BsdInfoWithUniqId>,
    shell_uniqueid: u64,
) -> std::collections::HashSet<i32> {
    infos
        .iter()
        .filter(|(p, i)| *p > 1 && i.p_uniqueid < shell_uniqueid)
        .filter_map(|(p, i)| {
            // SAFETY: `getsid` reads the process table.
            let s = unsafe { libc::getsid(*p) };
            (s > 0 && read(*p).is_some_and(|now| now.p_uniqueid == i.p_uniqueid)).then_some(s)
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn orphans_live(
    infos: &[(i32, BsdInfoWithUniqId)],
    gone: i32,
    read: &mut impl FnMut(i32) -> Option<BsdInfoWithUniqId>,
    list: &mut impl FnMut(u32) -> Option<Vec<i32>>,
    uid: u32,
    older: Option<&std::collections::HashSet<i32>>,
) -> bool {
    let mut below = vec![gone];
    let mut seen = std::collections::HashSet::new();
    while let Some(parent) = below.pop() {
        if !seen.insert(parent) {
            continue;
        }
        if seen.len() > 256 {
            return true;
        }
        for (p, info) in infos
            .iter()
            .filter(|(_, i)| i.pbsd.pbi_ppid as i32 == parent)
        {
            match read(*p) {
                Some(now) if now.p_uniqueid == info.p_uniqueid => {
                    // SAFETY: `getsid` reads the process table.
                    let session = unsafe { libc::getsid(*p) };
                    if !older.is_some_and(|older| session > 0 && older.contains(&session)) {
                        return true;
                    }
                }
                None if !exited(*p) && !list(uid).is_some_and(|pids| !pids.contains(p)) => {
                    return true;
                }
                _ => below.push(*p),
            }
        }
    }
    false
}

/// Process state parsed from `/proc/<pid>/stat` and `/proc/<pid>` ownership.
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcRecord {
    pub(crate) ppid: i32,
    pub(crate) session: i32,
    pub(crate) start: u64,
    pub(crate) ended: bool,
    pub(crate) exiting: bool,
    pub(crate) threads: u32,
    pub(crate) uid: u32,
}

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
fn parse_proc_stat(stat: &str, pid: i32, uid: u32) -> Option<ProcRecord> {
    const PF_EXITING: u64 = 0x4;
    let open = stat.find(" (")?;
    if stat.get(..open)?.trim().parse::<i32>().ok()? != pid {
        return None;
    }
    let close = stat.rfind(')')?;
    if close < open {
        return None;
    }
    let fields: Vec<&str> = stat.get(close + 1..)?.split_ascii_whitespace().collect();
    let state = fields.first()?.chars().next()?;
    Some(ProcRecord {
        ppid: fields.get(1)?.parse().ok()?,
        session: fields.get(3)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
        ended: matches!(state, 'Z' | 'X' | 'x'),
        exiting: fields.get(6)?.parse::<u64>().ok()? & PF_EXITING != 0,
        threads: fields.get(17)?.parse().ok()?,
        uid,
    })
}

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
impl ProcRecord {
    fn holds_nothing(&self) -> bool {
        (self.ended || self.exiting) && self.threads <= 1
    }
}

/// Process-table interface used by the Linux escape scan.
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
pub(crate) trait ProcTable {
    fn list(&mut self) -> Option<Vec<i32>>;
    fn read(&mut self, pid: i32) -> std::io::Result<Option<ProcRecord>>;
    fn owner(&mut self, pid: i32, sid: &str) -> Option<bool>;
    fn pause(&mut self, pause: Duration);
}

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
fn listed_proc_pids(
    entries: impl IntoIterator<Item = std::io::Result<std::ffi::OsString>>,
) -> Option<Vec<i32>> {
    let mut pids = Vec::new();
    for entry in entries {
        let name = entry.ok()?;
        if let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok())
            && pid > 0
        {
            pids.push(pid);
        }
    }
    Some(pids)
}

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
fn read_proc_record(
    pid: i32,
    stat: impl FnOnce() -> std::io::Result<String>,
    uid: impl FnOnce() -> std::io::Result<u32>,
) -> std::io::Result<Option<ProcRecord>> {
    let stat = match stat() {
        Ok(stat) => stat,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let uid = match uid() {
        Ok(uid) => uid,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    parse_proc_stat(&stat, pid, uid)
        .map(Some)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid proc record"))
}

#[cfg(target_os = "linux")]
struct Procfs;

#[cfg(target_os = "linux")]
impl ProcTable for Procfs {
    fn list(&mut self) -> Option<Vec<i32>> {
        let entries = std::fs::read_dir("/proc").ok()?;
        listed_proc_pids(entries.map(|entry| entry.map(|e| e.file_name())))
    }

    fn read(&mut self, pid: i32) -> std::io::Result<Option<ProcRecord>> {
        use std::os::unix::fs::MetadataExt as _;
        read_proc_record(
            pid,
            || std::fs::read_to_string(format!("/proc/{pid}/stat")),
            || std::fs::metadata(format!("/proc/{pid}")).map(|m| m.uid()),
        )
    }

    fn owner(&mut self, pid: i32, sid: &str) -> Option<bool> {
        u32::try_from(pid)
            .ok()
            .and_then(atpkg::caller_shell::process_args)
            .map(|args| {
                args.env.is_empty()
                    || args.env_var(aterm_types::domain::ENV_PARENT_SESSION_ID) == Some(sid)
            })
    }

    fn pause(&mut self, pause: Duration) {
        std::thread::sleep(pause);
    }
}

/// Scan `/proc` for background processes or daemons escaped from `s_pid`'s startup window.
#[cfg(target_os = "linux")]
pub(crate) fn escape_scan(
    s_pid: i32,
    _shell_version: Option<i32>,
    sid: &str,
    from: std::time::SystemTime,
    until: std::time::SystemTime,
) -> Scan {
    let Some(window) = boot_tick_window(from, until) else {
        return Scan::Unknown;
    };
    let mut table = Procfs;
    let Ok(parent) = i32::try_from(std::os::unix::process::parent_id()) else {
        return Scan::Unknown;
    };
    let Some(reapers) = reapers(&mut table, parent) else {
        return Scan::Unknown;
    };
    // SAFETY: `geteuid` is a side-effect-free getter.
    let uid = unsafe { libc::geteuid() };
    rescan_while_unknown(SCAN_ATTEMPTS, || {
        linux_escape_scan(&mut table, s_pid, sid, window, &reapers, uid)
    })
}

#[cfg(target_os = "linux")]
#[cfg_attr(
    target_pointer_width = "64",
    expect(
        clippy::useless_conversion,
        reason = "c_ulong is u64 on LP64 and u32 on 32-bit"
    )
)]
fn boot_tick_window(
    from: std::time::SystemTime,
    until: std::time::SystemTime,
) -> Option<(u64, u64)> {
    unsafe extern "C" {
        fn getauxval(kind: libc::c_ulong) -> libc::c_ulong;
    }
    const AT_CLKTCK: libc::c_ulong = 17;
    // SAFETY: side-effect-free getter.
    let per_second = u64::from(unsafe { getauxval(AT_CLKTCK) });
    if per_second == 0 {
        return None;
    }
    let aterm_uds::process::BootClock(boot) = aterm_uds::process::boot_clock_now()?;
    let wall = std::time::SystemTime::now();
    let tick_of = |t: std::time::SystemTime| -> Option<u64> {
        let ns = match wall.duration_since(t) {
            Ok(back) => boot.saturating_sub(u64::try_from(back.as_nanos()).ok()?),
            Err(ahead) => boot.saturating_add(u64::try_from(ahead.duration().as_nanos()).ok()?),
        };
        u64::try_from(u128::from(ns) * u128::from(per_second) / 1_000_000_000).ok()
    };
    Some((tick_of(from)?, tick_of(until)?))
}

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
fn reapers(table: &mut impl ProcTable, mut at: i32) -> Option<Vec<i32>> {
    let mut reapers = vec![1];
    for _ in 0..64 {
        if at == 1 {
            return Some(reapers);
        }
        if at <= 0 || reapers.contains(&at) {
            return None;
        }
        reapers.push(at);
        at = table.read(at).ok()??.ppid;
    }
    None
}

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
struct LinuxProcs {
    ours: std::collections::HashMap<i32, ProcRecord>,
    others: std::collections::HashMap<i32, ProcRecord>,
    gone: std::collections::HashSet<i32>,
}

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
impl LinuxProcs {
    fn put(&mut self, pid: i32, r: ProcRecord, uid: u32) {
        if r.uid == uid {
            self.others.remove(&pid);
            self.ours.insert(pid, r);
        } else {
            self.ours.remove(&pid);
            self.others.insert(pid, r);
        }
    }

    fn get(&self, pid: i32) -> Option<ProcRecord> {
        self.ours
            .get(&pid)
            .or_else(|| self.others.get(&pid))
            .copied()
    }

    fn drop_gone(&mut self, pid: i32) {
        self.ours.remove(&pid);
        self.others.remove(&pid);
        self.gone.insert(pid);
    }

    fn leaving(&self, r: &ProcRecord) -> bool {
        self.gone.contains(&r.ppid) || self.get(r.ppid).is_some_and(|p| p.ended || p.exiting)
    }

    fn run_lines(&self, session: i32) -> impl Iterator<Item = (&i32, &ProcRecord)> {
        self.ours.iter().chain(
            self.others
                .iter()
                .filter(move |(_, r)| r.session == session),
        )
    }

    fn settle(
        &mut self,
        table: &mut impl ProcTable,
        shell_start: u64,
        session: i32,
        uid: u32,
    ) -> Result<bool, ()> {
        let stale_now = |procs: &Self| -> Vec<i32> {
            procs
                .run_lines(session)
                .filter(|(_, r)| r.start >= shell_start && !r.holds_nothing() && procs.leaving(r))
                .map(|(p, _)| *p)
                .collect()
        };
        let mut pause = Duration::ZERO;
        let mut stale = stale_now(self);
        for _ in 0..SCAN_SETTLE_LISTINGS {
            if stale.is_empty() {
                break;
            }
            if !pause.is_zero() {
                table.pause(pause);
            }
            pause = pause.saturating_mul(2).max(SCAN_SETTLE_PAUSE);
            for p in stale {
                let Some(old) = self.get(p) else {
                    continue;
                };
                match table.read(old.ppid) {
                    Ok(Some(parent)) => self.put(old.ppid, parent, uid),
                    Ok(None) => self.drop_gone(old.ppid),
                    Err(_) => return Err(()),
                }
                match table.read(p) {
                    Ok(Some(fresh)) if fresh.start == old.start => self.put(p, fresh, uid),
                    Ok(_) => self.drop_gone(p),
                    Err(_) => return Err(()),
                }
            }
            stale = stale_now(self);
        }
        Ok(stale.is_empty())
    }

    fn orphans_live(&self, table: &mut impl ProcTable, gone: i32) -> Result<bool, ()> {
        let mut below = vec![gone];
        let mut seen = std::collections::HashSet::new();
        while let Some(parent) = below.pop() {
            if !seen.insert(parent) {
                continue;
            }
            if seen.len() > 256 {
                return Ok(true);
            }
            for (&p, r) in self.ours.iter().filter(|(_, r)| r.ppid == parent) {
                match table.read(p) {
                    Ok(Some(now)) if now.start == r.start && !now.holds_nothing() => {
                        return Ok(true);
                    }
                    Ok(_) => below.push(p),
                    Err(_) => return Err(()),
                }
            }
        }
        Ok(false)
    }
}

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
fn linux_escape_scan(
    table: &mut impl ProcTable,
    s_pid: i32,
    sid: &str,
    window: (u64, u64),
    reapers: &[i32],
    uid: u32,
) -> Scan {
    use std::collections::{HashMap, HashSet};
    if s_pid <= 1 {
        return shell_unknown(s_pid, "unread");
    }
    let Ok(Some(shell)) = table.read(s_pid) else {
        return shell_unknown(s_pid, "unread");
    };
    if shell.uid != uid || shell.ended {
        return shell_unknown(s_pid, "unread");
    }
    let Some(pids) = table.list() else {
        return Scan::Unknown;
    };
    let mut procs = LinuxProcs {
        ours: HashMap::with_capacity(pids.len()),
        others: HashMap::new(),
        gone: HashSet::new(),
    };
    for p in pids {
        match table.read(p) {
            Ok(Some(r)) => procs.put(p, r, uid),
            Ok(None) => {
                procs.gone.insert(p);
            }
            Err(_) => return Scan::Unknown,
        }
    }
    let in_window = |r: &ProcRecord| r.start >= window.0.max(shell.start) && r.start <= window.1;
    let Ok(mut settled) = procs.settle(table, shell.start, s_pid, uid) else {
        return Scan::Unknown;
    };
    if settled {
        let parents: HashSet<i32> = procs
            .run_lines(s_pid)
            .filter(|(p, r)| **p != s_pid && in_window(r) && !r.holds_nothing())
            .map(|(_, r)| r.ppid)
            .filter(|&p| p > 1 && p != s_pid && !reapers.contains(&p) && procs.get(p).is_some())
            .collect();
        for p in parents {
            match table.read(p) {
                Ok(Some(now)) if procs.get(p).is_some_and(|was| was.start == now.start) => {
                    procs.put(p, now, uid);
                }
                Ok(_) => procs.drop_gone(p),
                Err(_) => return Scan::Unknown,
            }
        }
        let Ok(again) = procs.settle(table, shell.start, s_pid, uid) else {
            return Scan::Unknown;
        };
        settled = again;
    }
    let mut incomplete = !settled;
    let walk = |pid: i32| -> (bool, bool, bool) {
        let mut at = pid;
        for _ in 0..64 {
            let Some(r) = procs.get(at) else {
                return (false, false, false);
            };
            let foreign = !procs.ours.contains_key(&at);
            match r.ppid {
                p if p == s_pid => return (true, false, false),
                p if foreign && reapers.contains(&p) => {
                    let orphan = in_window(&r) && !r.holds_nothing();
                    return (false, orphan, orphan && r.session != s_pid);
                }
                p if p > 1 => at = p,
                _ => return (false, false, false),
            }
        }
        (false, false, false)
    };
    let mut escaped = false;
    for (&pid, r) in &procs.ours {
        if pid == s_pid || r.holds_nothing() {
            continue;
        }
        if !in_window(r) {
            continue;
        }
        let (under, below_orphan, named_only) = walk(pid);
        let reparented = !under && (reapers.contains(&r.ppid) || below_orphan);
        let named_only = named_only && !reapers.contains(&r.ppid);
        let left_session = under && r.session != s_pid;
        if !(reparented || left_session) {
            continue;
        }
        let older_session = if !(left_session || r.session == s_pid)
            && shell.session == s_pid
            && r.session > 1
            && r.session != pid
        {
            match table.read(r.session) {
                Ok(Some(leader)) => !leader.ended && leader.start < shell.start,
                Ok(None) => false,
                Err(_) => return Scan::Unknown,
            }
        } else {
            false
        };
        let owned = if left_session || r.session == s_pid {
            true
        } else if older_session {
            false
        } else {
            match table.owner(pid, sid) {
                Some(owned) => owned,
                None if named_only => false,
                None => match table.read(pid) {
                    Ok(Some(now)) if now.start == r.start && !now.holds_nothing() => true,
                    Ok(_) => {
                        match procs.orphans_live(table, pid) {
                            Ok(live) => incomplete |= live,
                            Err(()) => return Scan::Unknown,
                        }
                        false
                    }
                    Err(_) => return Scan::Unknown,
                },
            }
        };
        if owned {
            aterm_log::info!(
                "spare gate: escape: pid={pid} ppid={} session={} in_session={} \
                 reparented={reparented} left_session={left_session} start={}",
                r.ppid,
                r.session,
                r.session == s_pid,
                r.start,
            );
            escaped = true;
            break;
        }
    }
    if !matches!(table.read(s_pid), Ok(Some(now)) if now.start == shell.start && !now.ended) {
        return shell_unknown(s_pid, "gone");
    }
    if escaped {
        Scan::Escaped
    } else if incomplete {
        Scan::Unknown
    } else {
        Scan::Clean
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn escape_scan(
    _s_pid: i32,
    _shell_version: Option<i32>,
    _sid: &str,
    _from: std::time::SystemTime,
    _until: std::time::SystemTime,
) -> Scan {
    Scan::Unknown
}

/// Send `SIGKILL` to every process in `pid`'s session, ending with `pid`.
#[cfg(unix)]
pub(crate) fn kill_session(pid: i32) {
    if pid <= 1 {
        return;
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    for p in host_pids() {
        // SAFETY: `getsid`/`kill` on a pid; only members of the spare's session are signalled.
        if p > 1 && p != pid && unsafe { libc::getsid(p) } == pid {
            unsafe { libc::kill(p, libc::SIGKILL) };
        }
    }
    // SAFETY: the spare's shell leads its own group and `pid` is unreaped.
    unsafe {
        libc::killpg(pid, libc::SIGKILL);
    }
}

/// No-op off Unix.
#[cfg(not(unix))]
pub(crate) fn kill_session(_pid: i32) {}

#[cfg(target_os = "macos")]
fn host_pids() -> Vec<i32> {
    // SAFETY: a null buffer asks for the count only.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    let mut pids = vec![0i32; count + 64];
    let bytes = (pids.len() * std::mem::size_of::<i32>()) as libc::c_int;
    // SAFETY: `pids` is `bytes` writable bytes.
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(n).unwrap_or(0).min(pids.len()));
    pids
}

#[cfg(target_os = "linux")]
fn host_pids() -> Vec<i32> {
    Procfs.list().unwrap_or_default()
}

/// Background job to spawn a spare shell on the `aterm-spare` thread.
pub(crate) struct SpawnJob {
    pub(crate) id: u64,
    pub(crate) window: WindowId,
    pub(crate) rows: u16,
    pub(crate) cols: u16,
    pub(crate) cell_px: Option<(u16, u16)>,
    pub(crate) factory: crate::spawn::SessionFactory,
    pub(crate) proxy: EventLoopProxy<Wake>,
    pub(crate) cwd: Option<String>,
    pub(crate) shell: aterm_core::shell_integration::ShellType,
}

/// Result of a [`SpawnJob`] returned to the main thread.
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

/// Visible rc run queued for evaluation by the `aterm-spare` thread.
pub(crate) struct Observation {
    pub(crate) witness: Arc<Witness>,
    pub(crate) sid: String,
    pub(crate) started_at: Instant,
    pub(crate) input0: Option<aterm_session::sink::DrivenInput>,
    pub(crate) host_effects0: u32,
    pub(crate) early_control0: u32,
    pub(crate) inputs: GenInputs,
}

/// Request to verify a warm spare waiting at its hold.
pub(crate) struct VerifyJob {
    pub(crate) id: u64,
    pub(crate) witness: Arc<Witness>,
    pub(crate) sid: String,
    pub(crate) inputs: GenInputs,
    pub(crate) generation: u64,
    pub(crate) run_at: Instant,
    pub(crate) reply: crate::spawn::WakePost,
}

/// Work item for the `aterm-spare` worker thread.
pub(crate) enum Job {
    Spawn(Box<SpawnJob>),
    Observe(Box<Observation>),
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only a unix reader witnesses a start")
    )]
    Marked,
    Verify(Box<VerifyJob>),
    Fail(u64, Verdict),
    Restamp(Box<GenInputs>, Option<EventLoopProxy<Wake>>),
    Freshen(u64),
    Retire(Box<Session>, bool),
    Sweep,
    Stop(Sender<()>),
}

/// Atomic mirror of gate state shared between the `aterm-spare` thread and main thread.
#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) generation: AtomicU64,
    pub(crate) fresh: AtomicU64,
    pub(crate) eligible: AtomicBool,
    pub(crate) pre_hold: AtomicU8,
    pub(crate) t_ready_ms: AtomicU64,
    pub(crate) report: Mutex<String>,
    pub(crate) reported: Mutex<Reported>,
    pub(crate) rc_ms: Mutex<VecDeque<u32>>,
}

/// Bitflags for [`Shared::pre_hold`].
pub(crate) const PRE_HOLD_ZSH: u8 = 1;
pub(crate) const PRE_HOLD_BASH: u8 = 1 << 1;
const PRE_HOLD_UNREAD: u8 = u8::MAX;

const fn pre_hold_bit(shell: aterm_core::shell_integration::ShellType) -> u8 {
    use aterm_core::shell_integration::ShellType;
    match shell {
        ShellType::Zsh => PRE_HOLD_ZSH,
        ShellType::Bash => PRE_HOLD_BASH,
        _ => 0,
    }
}

fn read_pre_hold(exists: impl Fn(&str) -> bool) -> u8 {
    use aterm_core::shell_integration::{ShellType, pre_hold_files};
    [ShellType::Zsh, ShellType::Bash]
        .into_iter()
        .filter(|&shell| pre_hold_files(shell).iter().any(|f| exists(f)))
        .fold(0, |bits, shell| bits | pre_hold_bit(shell))
}

fn parked_spawn_runs_unseen(
    shared: &Shared,
    shell: aterm_core::shell_integration::ShellType,
    exists: impl Fn(&str) -> bool,
) -> bool {
    let bits = read_pre_hold(exists);
    shared.pre_hold.store(bits, Ordering::Release);
    aterm_core::shell_integration::has_parked_hold(shell)
        && bits & pre_hold_bit(shell) != 0
        && !shared.eligible.load(Ordering::Acquire)
}

impl Shared {
    /// Initial mirror state before pre-hold system rc files are checked.
    pub(crate) fn unread() -> Self {
        Self {
            pre_hold: AtomicU8::new(PRE_HOLD_UNREAD),
            ..Self::default()
        }
    }

    /// Whether `shell` has a system rc file that runs before its parked hold.
    pub(crate) fn ran_before_hold(&self, shell: aterm_core::shell_integration::ShellType) -> bool {
        self.pre_hold.load(Ordering::Acquire) & pre_hold_bit(shell) != 0
    }

    /// Compute the rc generation for `inputs` using the last reported roots.
    pub(crate) fn stamp(&self, inputs: &GenInputs) -> u64 {
        let reported = self
            .reported
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        rc_generation(inputs, &reported)
    }
}

struct Thread {
    tx: Mutex<Option<Sender<Job>>>,
}

static THREAD: OnceLock<Option<Thread>> = OnceLock::new();
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
fn debug_eligible() -> bool {
    aterm_types::dev_seam!("ATERM_DEBUG_SPARE_ELIGIBLE").is_some_and(|v| v == "1")
}

fn debug_present() -> bool {
    aterm_types::dev_seam!("ATERM_DEBUG_SPARE_PRESENT").is_some_and(|v| v == "1")
}

fn debug_ms(name: &'static str) -> Option<Duration> {
    let raw = match name {
        "T_READY" => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_TREADY_MS"),
        "IDLE" => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_IDLE_MS"),
        "PERSON" => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_PERSON_MS"),
        "READY_AGE" => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_READY_AGE_MS"),
        "SCAN" => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_SCAN_MS"),
        "PARKED" => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_PARKED_MS"),
        _ => aterm_types::dev_seam!("ATERM_DEBUG_SPARE_SETTLE_MS"),
    }?;
    raw.to_str()?.parse().ok().map(Duration::from_millis)
}

fn person_window() -> Duration {
    debug_ms("PERSON").unwrap_or(PERSON_WINDOW)
}

fn ready_max_age() -> Duration {
    debug_ms("READY_AGE").unwrap_or(READY_MAX_AGE)
}

fn scan_every() -> Duration {
    debug_ms("SCAN").unwrap_or(SCAN_EVERY)
}

fn parked_deadline() -> Duration {
    debug_ms("PARKED").unwrap_or(PARKED_DEADLINE)
}

/// Process-wide [`Shared`] gate mirror.
pub(crate) fn shared() -> Arc<Shared> {
    Arc::clone(SHARED.get_or_init(|| Arc::new(Shared::unread())))
}

/// Enqueue `job` on the `aterm-spare` thread, spawning the thread on first use.
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

/// Shut down and join the `aterm-spare` thread.
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

struct Pending {
    obs: Box<Observation>,
    stamp: u64,
    not_before: Option<Instant>,
}

fn worker(rx: Receiver<Job>) {
    crate::qos::set_self(crate::qos::Role::Background);
    let shared = shared();
    shared
        .pre_hold
        .store(read_pre_hold(|f| Path::new(f).exists()), Ordering::Release);
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
        let changed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
            None | Some(Job::Marked) | Some(Job::Stop(_)) => false,
            Some(Job::Spawn(job)) => {
                run_spawn(*job);
                false
            }
            Some(Job::Observe(obs)) => {
                let stamp = shared.stamp(&obs.inputs);
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
                inputs = Some(*new_inputs);
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
                    aterm_core::shell_integration::spare_fifo::sweep_dead(
                        &dir.join(aterm_core::shell_integration::SPARE_DIR),
                    );
                }
                false
            }
        }))
        .unwrap_or(false);
        let mut changed = changed;
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

fn mirror(
    book: &mut GateBook,
    inputs: &GenInputs,
    shared: &Shared,
    proxy: Option<&EventLoopProxy<Wake>>,
) {
    let generation = shared.stamp(inputs);
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

fn evaluate(
    p: Pending,
    book: &mut GateBook,
    last_scan: &mut Option<Instant>,
    shared: &Shared,
) -> Option<Pending> {
    if let Some(at) = *last_scan
        && p.obs.witness.start_seen.get().is_some()
    {
        let free = at + scan_every();
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
        aterm_log::info!(
            "spare gate: unclean run: failed=start: no start record within {} ms",
            SAMPLE_WINDOW.as_millis()
        );
        aterm_log::info!("spare gate: generation {stamp:016x} saw a visible run: Unclean");
        book.sample(stamp, Sample::Unclean);
        return None;
    };
    {
        let mut reported = shared.reported.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(dir) = &seen.mark.rc_dir
            && reported.rc_dir.as_deref() != Some(Path::new(dir))
        {
            reported.rc_dir = Some(PathBuf::from(dir));
        }
        reported.roots = roots_of(&seen.mark);
    }
    let now = shared.stamp(&obs.inputs);
    if now != stamp {
        aterm_log::info!(
            "spare gate: generation {stamp:016x} became {now:016x} before its visible run was judged; the run is not counted"
        );
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

fn judge(obs: &Observation, seen: &StartSeen, last_scan: &mut Option<Instant>) -> Sample {
    let w = &obs.witness;
    if seen
        .input
        .zip(obs.input0)
        .is_none_or(|(at, input0)| at != input0)
    {
        aterm_log::info!("spare gate: inconclusive run: input moved before its start record");
        return Sample::Inconclusive;
    }
    let (pid, image0) = w.shell.get().cloned().unwrap_or((-1, None));
    let rc = seen.at.saturating_duration_since(obs.started_at);
    let flags_clean = seen
        .mark
        .flags
        .is_some_and(|f| f.sched == 0 && f.jobs.unwrap_or(0) == 0 && !f.periodic);
    let roots_named = obs.inputs.shell != aterm_core::shell_integration::ShellType::Fish
        || seen.mark.roots.is_some();
    let failed: Vec<&str> = [
        (seen.mark.kind == StartKind::Cold, "kind"),
        (rc <= SAMPLE_WINDOW, "slow"),
        (flags_clean, "flags"),
        (roots_named, "roots"),
        (pid > 1 && seen.fg_pgrp == pid, "fg"),
        (
            image0
                .as_ref()
                .is_some_and(|image| seen.image.as_ref() == Some(image)),
            "image",
        ),
        (!seen.alt_screen, "alt"),
        (
            w.host_effects.load(Ordering::Acquire) == obs.host_effects0,
            "host",
        ),
        (
            w.early_control.load(Ordering::Acquire) == obs.early_control0,
            "control",
        ),
        (w.askpass.load(Ordering::Acquire) == 0, "askpass"),
    ]
    .into_iter()
    .filter_map(|(kept, rule)| (!kept).then_some(rule))
    .collect();
    if !failed.is_empty() {
        aterm_log::info!(
            "spare gate: unclean run: failed={} kind={:?} rc_ms={} flags={:?} roots={roots_named} \
             fg={}/{pid} image_same={} alt={} host={}/{} control={}/{} askpass={}",
            failed.join(","),
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
    let shell_version = w.shell_version.get().copied();
    match rate_limited_scan(
        pid,
        shell_version,
        &obs.sid,
        obs.started_at,
        seen.at,
        last_scan,
        true,
    ) {
        Scan::Clean => Sample::Clean {
            rc_ms: u32::try_from(rc.as_millis()).unwrap_or(u32::MAX),
        },
        Scan::Escaped => Sample::Unclean,
        scan @ (Scan::Unknown | Scan::RateLimited) => {
            aterm_log::info!("spare gate: inconclusive run: scan {scan:?}");
            Sample::Inconclusive
        }
    }
}

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

fn rate_limited_scan(
    pid: i32,
    shell_version: Option<i32>,
    sid: &str,
    run_at: Instant,
    mark_at: Instant,
    last_scan: &mut Option<Instant>,
    spaced: bool,
) -> Scan {
    rate_limited(mark_at, last_scan, spaced, || {
        escape_scan(
            pid,
            shell_version,
            sid,
            wall_of(run_at),
            wall_of(mark_at) + SCAN_GRACE,
        )
    })
}

fn rate_limited(
    mark_at: Instant,
    last_scan: &mut Option<Instant>,
    spaced: bool,
    scan: impl FnOnce() -> Scan,
) -> Scan {
    crate::qos::sleep_until(mark_at + SCAN_GRACE);
    let now = Instant::now();
    if spaced && last_scan.is_some_and(|at| now.saturating_duration_since(at) < scan_every()) {
        return Scan::RateLimited;
    }
    *last_scan = Some(now);
    scan()
}

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
    let reported = shared
        .reported
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if job.inputs.shell == aterm_core::shell_integration::ShellType::Fish
        && roots_of(&seen.mark).is_none_or(|roots| reported.roots.as_ref() != Some(&roots))
    {
        return Verdict::Backoff("roots");
    }
    if rc_generation(&job.inputs, &reported) != job.generation {
        return Verdict::Backoff("stale");
    }
    let shell_version = w.shell_version.get().copied();
    match rate_limited_scan(
        pid,
        shell_version,
        &job.sid,
        job.run_at,
        seen.at,
        last_scan,
        false,
    ) {
        Scan::Clean => Verdict::Ok,
        Scan::Escaped => Verdict::Backoff("escaped"),
        Scan::Unknown | Scan::RateLimited => Verdict::Backoff("scan-unknown"),
    }
}

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
        shell,
    } = job;
    let session = if parked_spawn_runs_unseen(&shared(), shell, |f| Path::new(f).exists()) {
        Err(std::io::Error::other(
            "a system file now runs before the parked hold",
        ))
    } else {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::watchdog::forks_wait_for_park(SPARE_FORK_PARK_WAIT, || {
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
            })
        }))
        .unwrap_or_else(|_| Err(std::io::Error::other("the spare's spawn panicked")))
    };
    let done = Wake::SpareSpawned(Box::new(SpareSpawned { id, session }));
    if let Err(aterm_window::event_loop::EventLoopClosed(Wake::SpareSpawned(done))) =
        proxy.send_event(done)
        && let Ok(session) = done.session
    {
        drop(session);
    }
}

impl Wake {
    /// Local session id reported by a session-internal wake, if any.
    pub(crate) const fn local_session(&self) -> Option<u64> {
        match self {
            Wake::Output { session, .. } => Some(*session),
            Wake::QueuedKeyDelivered { session, .. } => Some(*session),
            Wake::MetaChanged { session, .. } => Some(*session),
            Wake::TaskChanged { session } => Some(*session),
            Wake::InputWatch { session, .. } => Some(*session),
            Wake::ClaudeFooter { session, .. } => Some(*session),
            Wake::SessionLogMoved { session, .. } => Some(*session),
            Wake::ProgramResolved { session, .. } => Some(*session),
            Wake::Ready { session, .. } => Some(*session),
            Wake::Exit { session, .. } => Some(*session),
            Wake::Bell { session, .. } => Some(*session),
            #[cfg(target_os = "linux")]
            Wake::WindowOp { session, .. } => Some(*session),
            Wake::ProtectedRead { session, .. } => Some(*session),
            Wake::LeaseChanged { .. }
            | Wake::CrashJournalOwed
            | Wake::FabricChanged { .. }
            | Wake::Typed { .. }
            | Wake::Driven { .. }
            | Wake::Image(..)
            | Wake::Video { .. }
            | Wake::VideoStatus { .. }
            | Wake::VideoStop { .. }
            | Wake::ReadChrome { .. }
            | Wake::ReadPanes { .. }
            | Wake::ReadPrivacy { .. }
            | Wake::WarmupResult
            | Wake::ClaimantRetired
            | Wake::ConsentObserver
            | Wake::ArmConsentWait { .. }
            | Wake::ConsentCensusReady
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
            | Wake::Vitals
            | Wake::DiskWalk
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
            | Wake::ControlTextScale { .. }
            | Wake::LoginNotice { .. }
            | Wake::CardSeed { .. }
            | Wake::Aos { .. }
            | Wake::Rewind { .. }
            | Wake::RewindDone(..)
            | Wake::AosWorked(..)
            | Wake::AosExport { .. }
            | Wake::PersonStandIn { .. }
            | Wake::ControlSecurityPosture { .. }
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
            | Wake::PaneRegridOwed { .. }
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
            | Wake::WorkerLaunch { .. }
            | Wake::HwKeyTarget { .. }
            | Wake::DocumentAdmitted(..)
            | Wake::EditRequest(..)
            | Wake::EditCancel { .. }
            | Wake::EditAwait(..)
            | Wake::EditAwaitWithdraw { .. }
            | Wake::CliTab(..)
            | Wake::MenuAction { .. }
            | Wake::ConfirmedClose { .. }
            | Wake::AnswerCloseConfirm { .. }
            | Wake::SiblingScan(..)
            | Wake::FleetRead
            | Wake::FabricRead
            | Wake::PillAct { .. }
            | Wake::CreateWindow
            | Wake::TabWindowCommand(_)
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
            | Wake::OpenFactMenu { .. }
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
            | Wake::PkgCToolchain(..)
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
            #[cfg(target_os = "macos")]
            Wake::UpdateLaunchUnregistered { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::OwnLaunchRecord(..) => None,
            #[cfg(all(test, unix))]
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
            #[cfg(target_os = "macos")]
            Wake::SystemColorsChanged => None,
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
            Wake::StatusAction { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::StatusMenuOpening { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::SiblingRaised { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::NoticeVoice => None,
            #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
            Wake::NoticeClicked { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::NativeTerminateRequested { .. } => None,
            #[cfg(unix)]
            Wake::LifelineCut => None,
            #[cfg(unix)]
            Wake::ExitSignal { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::TabContextMenuOpening { .. } => None,
            #[cfg(any(target_os = "macos", all(test, unix)))]
            Wake::NativeTabCommand { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::TabMenuConnection { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::TabMenuUpgrade { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::TabMenuRewind { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::TabMenuSessionAction { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::BeginSessionRename { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::CommitSessionRename { .. } => None,
            #[cfg(target_os = "macos")]
            Wake::CancelSessionRename { .. } => None,
            Wake::WorkstationSetup(_)
            | Wake::DeveloperToolsInstall(_, _)
            | Wake::InitialSetupStatus { .. }
            | Wake::InitialSetupApplyResolved => None,
        }
    }
}

/// Lifecycle state of the spare slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotState {
    Booked,
    Spawned,
    Parked,
    Warming,
    Verifying,
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

    /// Whether startup files have run (or are running) unseen in this state.
    pub(crate) const fn warm(self) -> bool {
        matches!(self, Self::Warming | Self::Verifying | Self::Ready)
    }
}

/// Terminal grid dimensions `(rows, cols, cell_size)`.
pub(crate) type Grid = (u16, u16, Option<(u16, u16)>);

/// Active spare shell slot.
pub(crate) struct Slot {
    pub(crate) id: u64,
    pub(crate) state: SlotState,
    pub(crate) link: Arc<SpareLink>,
    pub(crate) session: Option<Session>,
    pub(crate) cwd: Option<String>,
    pub(crate) grid: Grid,
    pub(crate) window: WindowId,
    pub(crate) fingerprint: u64,
    pub(crate) colors: u64,
    pub(crate) booked: Instant,
    pub(crate) since: Instant,
    pub(crate) generation: u64,
    pub(crate) t_ready: Duration,
    pub(crate) inputs: GenInputs,
}

/// Adopted spare in flight between `try_adopt_spare` and pool installation.
pub(crate) struct Transit {
    pub(crate) id: u64,
    pub(crate) link: Arc<SpareLink>,
    pub(crate) tier: SlotState,
    pub(crate) grid: Grid,
    pub(crate) age: Duration,
    pub(crate) rc_ms: Option<u64>,
    pub(crate) inputs: GenInputs,
    pub(crate) miss: Arc<OnceLock<&'static str>>,
    pub(crate) retarget: Arc<OnceLock<String>>,
}

/// Pre-handoff validation check executed by the spawner off the main thread.
pub(crate) struct AdoptCheck {
    pub(crate) cwd: Option<String>,
    pub(crate) generation: Option<(GenInputs, u64)>,
    pub(crate) link: Arc<SpareLink>,
    pub(crate) kill: bool,
    pub(crate) retarget: bool,
}

impl AdoptCheck {
    /// Validate that the spare can land in `want` (or `default`), returning an optional retarget directory.
    pub(crate) fn verdict(
        &self,
        session: &Session,
        want: Option<&str>,
        default: Option<&str>,
    ) -> Result<Option<String>, &'static str> {
        if let Some((inputs, generation)) = &self.generation
            && shared().stamp(inputs) != *generation
        {
            return Err("stale");
        }
        let want = want.or(default);
        let start = crate::spawn_folder::start_folder(want);
        if start.fell_back.is_some() {
            return Err("folder");
        }
        let retarget = if want == self.cwd.as_deref().or(default) {
            None
        } else if self.retarget {
            let dir = start.cwd.as_deref().ok_or("folder")?;
            Some(crate::spawn_folder::retarget_folder(dir).ok_or("folder")?)
        } else {
            return Err("folder");
        };
        if self.link.exited() || session.pty_closed_at.get().is_some() {
            return Err("process");
        }
        Ok(retarget)
    }
}

/// Platform probe returning whether power or thermal constraints block warm promotion.
pub(crate) type PowerProbe = fn() -> bool;

/// Whether the host is in Low Power Mode or serious thermal state.
pub(crate) fn power_constrained() -> bool {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::{class, sel};
        // SAFETY: `+processInfo` is an immortal singleton; both getters are thread-safe scalar properties.
        unsafe {
            let info =
                aterm_objc::send::send_id(class(c"NSProcessInfo").as_id(), sel!(processInfo));
            if info.is_null() {
                return false;
            }
            let low = aterm_objc::send::send_bool(info, sel!(isLowPowerModeEnabled));
            let thermal = aterm_objc::send::send_isize(info, sel!(thermalState));
            low || thermal >= 2
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Main-thread warm spare controller state on [`App`].
pub(crate) struct Spares {
    pub(crate) slot: Option<Slot>,
    tombstones: VecDeque<u64>,
    pub(crate) transit: Vec<Transit>,
    parked_spawns: VecDeque<Instant>,
    warm_discards: VecDeque<Instant>,
    last_replace: Option<Instant>,
    person: std::cell::Cell<Option<Instant>>,
    tick_at: std::cell::Cell<Option<Instant>>,
    pub(crate) adopted: [u32; 3],
    misses: Vec<(&'static str, u32)>,
    discards: Vec<(&'static str, u32)>,
    pressure_at: Option<Instant>,
    pass_active: bool,
    wanted: Option<(WindowId, Option<String>, Instant)>,
    wants_prompt: std::cell::Cell<Option<u64>>,
    boot_at: Option<Instant>,
    /// Shell/script fingerprint disabled after a parked spare ran past its hold.
    pub(crate) parked_failed: Option<u64>,
    /// Target folder that failed with [`FolderFallsBack`], barred until the target moves.
    pub(crate) folder_failed: Option<Option<String>>,
    pub(crate) shared: Arc<Shared>,
    pub(crate) power: PowerProbe,
    restamped: Option<u64>,
    freshen: Option<(u64, Instant)>,
    freshen_seq: u64,
    boot_observed: bool,
    swept: bool,
    /// Captured [`Job`] queue for headless unit tests.
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
            folder_failed: None,
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

fn count_within(log: &VecDeque<Instant>, now: Instant, window: Duration) -> usize {
    log.iter()
        .filter(|at| now.saturating_duration_since(**at) < window)
        .count()
}

fn prune(log: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while log
        .front()
        .is_some_and(|at| now.saturating_duration_since(*at) >= window)
    {
        log.pop_front();
    }
}

/// Decision from [`App::spare_book_check`] or [`App::spare_promote_check`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BookCheck {
    Go {
        window: WindowId,
        cwd: Option<String>,
    },
    Wait(Instant),
    NotAtPrompt(Option<u64>),
    No(&'static str),
}

impl Spares {
    /// Whether the event-loop backstop should swallow a wake for session `id`.
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

    /// Send `job` to the worker thread (or capture it in tests).
    pub(crate) fn send(&mut self, job: Job) -> Result<(), Job> {
        #[cfg(test)]
        if let Some(captured) = self.captured.as_mut() {
            captured.push(job);
            return Ok(());
        }
        send_job(job)
    }

    /// Record `id` in the tombstone ring so late wakes are swallowed.
    pub(crate) fn tombstone_public(&mut self, id: u64) {
        self.tombstone(id);
    }

    /// Arm the next spare tick at or before `at`.
    pub(crate) fn arm(&self, at: Instant) {
        let next = self.tick_at.get().map_or(at, |t| t.min(at));
        self.tick_at.set(Some(next));
    }

    /// Record user activity at `now` and schedule a tick after [`SETTLE`].
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

    /// Notify that session `id` produced output while booking awaited its first prompt.
    pub(crate) fn note_output(&self, id: u64, now: Instant) {
        if self.wants_prompt.get() == Some(id) {
            self.wants_prompt.set(None);
            self.arm(now + debug_ms("SETTLE").unwrap_or(SETTLE));
        }
    }

    pub(crate) fn note_miss(&mut self, reason: &'static str) {
        bump(&mut self.misses, reason);
    }

    /// Publish spare state to the shared metrics snapshot.
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

struct MetricsSnap {
    state: &'static str,
    booked: Option<Instant>,
    rest: String,
}

static METRICS: Mutex<Option<MetricsSnap>> = Mutex::new(None);

/// Emit spare metrics fields to `s`.
pub(crate) fn emit_metrics(s: &mut dyn crate::metrics::wire::FieldSink) {
    let snap = METRICS.lock().unwrap_or_else(|e| e.into_inner());
    let (state, age, rest) = match snap.as_ref() {
        Some(m) => (
            m.state,
            m.booked.map_or(0, |at| at.elapsed().as_millis()),
            m.rest.clone(),
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
    s.label("spare", state);
    s.count("spare_age_ms", u64::try_from(age).unwrap_or(u64::MAX));
    s.count("spare_ptys", crate::metrics::spare_ptys() as u64);
    s.label("spare_gate", &report.replace(' ', "_"));
    for (key, value) in rest.split_whitespace().filter_map(|t| t.split_once('=')) {
        match value.parse::<u64>() {
            Ok(n) => s.count(key, n),
            Err(_) => s.label(key, value),
        }
    }
    s.opt_count("spare_rc_ms_p50", rc_p50.map(u64::from), "-");
}

fn parked_key(inputs: &GenInputs) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    aterm_core::shell_integration::script_set_address().hash(&mut h);
    inputs.shell_path.hash(&mut h);
    h.finish()
}

fn shell_binary(override_shell: Option<&str>) -> PathBuf {
    match override_shell.filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => std::env::var_os("SHELL").map_or_else(|| PathBuf::from("/bin/sh"), PathBuf::from),
    }
}

impl App {
    /// Configured [`SpareMode`].
    pub(crate) fn spare_mode(&self) -> SpareMode {
        if cfg!(not(any(target_os = "macos", target_os = "linux"))) {
            return SpareMode::Off;
        }
        self.config.spare_shell_mode()
    }

    /// Capture [`GenInputs`] for the current session factory.
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

    fn spare_factory_ok(&self) -> bool {
        let f = &self.session_factory;
        f.exec_command.is_none()
            && f.integrate
            && !f.temporal_recording
            && aterm_core::shell_integration::has_spare_hold(crate::spawn::detect_spawn_shell(
                f.shell_override.as_deref(),
            ))
    }

    /// Next spare tick deadline, running `spare_tick` first if due at `now`.
    pub(crate) fn spare_deadline(&mut self, now: Instant) -> Option<Instant> {
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

    /// Publish spare metrics for the current mode.
    pub(crate) fn publish_spare(&self) {
        self.spares.publish(self.spare_mode(), Instant::now());
    }

    /// Advance spare booking, promotion, replacement, and aging on the main thread.
    pub(crate) fn spare_tick(&mut self, now: Instant) {
        if cfg!(all(
            any(target_os = "macos", target_os = "linux"),
            not(test)
        )) && !std::mem::replace(&mut self.spares.swept, true)
        {
            let _ = self.spares.send(Job::Sweep);
        }
        if cfg!(not(test)) {
            crate::spawn_prep::warm();
        }
        let mode = self.spare_mode();
        if mode == SpareMode::Off || self.proxy.is_none() {
            if self.spares.slot.is_some() {
                self.retire_spare("off", None);
            }
            return;
        }
        self.spares.boot_at.get_or_insert(now);
        let inputs = self.spare_gen_inputs();
        let key = inputs.fingerprint ^ parked_key(&inputs);
        if self.spares.restamped != Some(key) {
            self.spares.restamped = Some(key);
            let _ = self
                .spares
                .send(Job::Restamp(Box::new(inputs.clone()), self.proxy.clone()));
        }
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

    fn spare_age(&mut self, now: Instant, mode: SpareMode) {
        let Some(slot) = &self.spares.slot else {
            return;
        };
        let age = now.saturating_duration_since(slot.since);
        let (state, t_ready) = (slot.state, slot.t_ready);
        let parked_by = parked_deadline();
        match state {
            SlotState::Spawned if age >= parked_by => {
                aterm_log::warn!("spare: no parked mark within {parked_by:?}; spares off");
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
                SlotState::Spawned => Some(slot.since + parked_by),
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

    fn spare_wanted(&self) -> Option<(WindowId, Option<String>)> {
        let front = self.frontmost_window?;
        if !self.windows.contains_key(&front) {
            return None;
        }
        Some((
            front,
            self.focused_pane_folder(front)
                .map(|folder| folder.reported),
        ))
    }

    fn spare_follow(&mut self, now: Instant) {
        let wanted = self.spare_wanted();
        let moved = match (&self.spares.wanted, &wanted) {
            (Some((w, c, _)), Some((nw, nc))) => w != nw || c != nc,
            (None, None) => false,
            _ => true,
        };
        if moved {
            self.spares.wanted = wanted.clone().map(|(w, c)| (w, c, now));
            self.spares.folder_failed = None;
        }
        let Some(slot) = &self.spares.slot else {
            return;
        };
        if matches!(slot.state, SlotState::Booked | SlotState::Spawned) {
            return;
        }
        let since = self.spares.wanted.as_ref().map_or(now, |(_, _, at)| *at);
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

    /// Check whether a spare can be booked at `now`.
    pub(crate) fn spare_book_check(&self, now: Instant, inputs: &GenInputs) -> BookCheck {
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
        if self.spares.shared.ran_before_hold(inputs.shell)
            && !self.spares.shared.eligible.load(Ordering::Acquire)
        {
            return BookCheck::No("system-rc");
        }
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
            return BookCheck::No("window");
        };
        if self.spares.folder_failed.as_ref() == Some(&cwd) {
            return BookCheck::No("folder");
        }
        let focused = self.focused_session_id(window);
        let at_prompt = focused
            .and_then(|id| self.pool.get(id))
            .is_some_and(|s| crate::term_lock(&s.term).shell_prompt_started());
        if !at_prompt {
            return BookCheck::NotAtPrompt(focused);
        }
        if !aterm_core::shell_integration::has_parked_hold(inputs.shell)
            && let Some(no) = self.spare_warm_gates(now, now)
        {
            return no;
        }
        BookCheck::Go { window, cwd }
    }

    fn spare_book(&mut self, now: Instant, inputs: &GenInputs) {
        let (window, cwd) = match self.spare_book_check(now, inputs) {
            BookCheck::Go { window, cwd } => (window, cwd),
            BookCheck::Wait(at) => {
                self.spares.arm(at);
                return;
            }
            BookCheck::NotAtPrompt(focused) => {
                self.spares.wants_prompt.set(focused);
                return;
            }
            BookCheck::No(_) => return,
        };
        let warm_only = !aterm_core::shell_integration::has_parked_hold(inputs.shell);
        if warm_only && !self.spare_fresh(now) {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let (generation, t_ready) = if warm_only {
            let shared = &self.spares.shared;
            let t_ready = Duration::from_millis(shared.t_ready_ms.load(Ordering::Acquire))
                .clamp(T_READY_MIN, T_READY_MAX);
            (
                shared.generation.load(Ordering::Acquire),
                debug_ms("T_READY").unwrap_or(t_ready),
            )
        } else {
            (0, T_READY_MIN)
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
            generation,
            t_ready,
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
            shell: inputs.shell,
        }));
        if self.spares.send(job).is_err() {
            self.spares.slot = None;
            self.spares.tombstone(id);
        }
    }

    /// Check whether a parked spare can be promoted to warm at `now`.
    pub(crate) fn spare_promote_check(&self, now: Instant) -> BookCheck {
        let Some(slot) = &self.spares.slot else {
            return BookCheck::No("none");
        };
        if slot.state != SlotState::Parked {
            return BookCheck::No("not-parked");
        }
        if let Some(no) = self.spare_warm_gates(now, slot.since) {
            return no;
        }
        BookCheck::Go {
            window: slot.window,
            cwd: slot.cwd.clone(),
        }
    }

    /// Check common gates required before running startup files unseen.
    pub(crate) fn spare_warm_gates(&self, now: Instant, since: Instant) -> Option<BookCheck> {
        if self.spare_mode() != SpareMode::Auto {
            return Some(BookCheck::No("mode"));
        }
        if self.config.spare_shell_needs_person_or_default()
            && self
                .spares
                .person
                .get()
                .is_none_or(|at| now.saturating_duration_since(at) >= person_window())
        {
            return Some(BookCheck::No("no-person"));
        }
        let shared = &self.spares.shared;
        if shared.generation.load(Ordering::Acquire) == 0
            || !shared.eligible.load(Ordering::Acquire)
        {
            return Some(BookCheck::No("gate"));
        }
        let idle = debug_ms("IDLE").unwrap_or(PROMOTE_IDLE);
        let stable = debug_ms("IDLE").unwrap_or(PROMOTE_FOLDER_STABLE);
        let mut not_before = since;
        if let Some(at) = self.spares.person.get() {
            not_before = not_before.max(at + idle);
        }
        if let Some((_, _, at)) = &self.spares.wanted {
            not_before = not_before.max(*at + stable);
        }
        if now < not_before {
            return Some(BookCheck::Wait(not_before));
        }
        if count_within(&self.spares.warm_discards, now, HOUR) >= WARM_BUDGET {
            return Some(BookCheck::No("budget"));
        }
        if self.spares.pass_active {
            return Some(BookCheck::No("pass"));
        }
        if self.update_handoff_in_flight() {
            return Some(BookCheck::No("update"));
        }
        if (self.spares.power)() {
            return Some(BookCheck::No("power"));
        }
        None
    }

    fn spare_promote(&mut self, now: Instant) {
        match self.spare_promote_check(now) {
            BookCheck::Go { .. } => {}
            BookCheck::Wait(at) => {
                self.spares.arm(at);
                return;
            }
            BookCheck::NotAtPrompt(_) | BookCheck::No(_) => return,
        }
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

    fn spare_fresh(&mut self, now: Instant) -> bool {
        let answered = self.spares.shared.fresh.load(Ordering::Acquire);
        if let Some((n, at)) = self.spares.freshen {
            let young = now.saturating_duration_since(at) < FRESH_FOR;
            if young && answered == n {
                self.spares.freshen = None;
                return true;
            }
            if young {
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

    /// Handle [`Wake::SpareSpawned`] completion on the main thread.
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
                let ran_past = slot.link.witness.start_seen.get().is_some();
                let warm_only = !aterm_core::shell_integration::has_parked_hold(slot.inputs.shell);
                slot.session = Some(session);
                slot.state = if warm_only {
                    SlotState::Warming
                } else if parked {
                    SlotState::Parked
                } else {
                    SlotState::Spawned
                };
                slot.since = if warm_only { slot.booked } else { now };
                if warm_only {
                    self.spares.arm(now);
                    if ran_past {
                        self.on_spare_note(id, SpareNote::AtStart);
                    }
                    return;
                }
                if ran_past {
                    aterm_log::warn!("spare: a parked spare signed a start record; spares off");
                    self.spares.parked_failed = Some(parked_key(&slot.inputs));
                    self.retire_spare("ran_past_hold", None);
                    return;
                }
                self.spares.arm(now);
            }
            Ok(session) => {
                self.spares.retire_session(session, false);
            }
            Err(e) if matches => {
                aterm_log::info!("spare: spawn failed: {e}");
                let slot = self.spares.slot.take();
                self.spares.tombstone(id);
                if e.get_ref().is_some_and(|e| e.is::<FolderFallsBack>())
                    && let Some(slot) = slot
                {
                    if let Some(at) = self
                        .spares
                        .parked_spawns
                        .iter()
                        .rposition(|at| *at == slot.booked)
                    {
                        self.spares.parked_spawns.remove(at);
                    }
                    self.spares.folder_failed = Some(slot.cwd);
                    bump(&mut self.spares.discards, "folder");
                } else {
                    bump(&mut self.spares.discards, "spawn");
                }
            }
            Err(_) => {}
        }
    }

    /// Handle [`Wake::SpareNote`] events on the main thread.
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
                            None => Arc::new(|_| false),
                        };
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

    /// Swallow wakes for unowned spares at the top of `user_event`.
    pub(crate) fn on_spare_backstop(&mut self, ev: &Wake) {
        if let Wake::Output { session, .. } = ev
            && let Some(slot) = &self.spares.slot
            && slot.id == *session
            && let Some(s) = &slot.session
        {
            s.output_wake_pending.store(0, Ordering::Relaxed);
        }
    }

    /// Retire the current spare slot, recording `reason` and optional gate `failure`.
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

    /// Retire any active spare and stop the worker thread on application exit.
    pub(crate) fn shutdown_spares(&mut self) {
        if self.spares.slot.is_some() {
            self.retire_spare("quit", None);
        }
        stop_thread();
    }

    /// Retire the spare and hold off booking after critical memory pressure.
    #[cfg(target_os = "macos")]
    pub(crate) fn note_spare_pressure(&mut self) {
        self.spares.pressure_at = Some(Instant::now());
        self.retire_spare("pressure", None);
    }

    /// Update `pass_active` and retire any warm spare when an atpkg pass starts.
    pub(crate) fn note_spare_pass(&mut self, active: bool) {
        let was = std::mem::replace(&mut self.spares.pass_active, active);
        if active && !was && self.spares.slot.as_ref().is_some_and(|s| s.state.warm()) {
            self.retire_spare("pass", None);
        }
        if was && !active {
            self.spares.arm(Instant::now());
        }
    }

    /// Reconcile spare state after a configuration reload.
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

    /// Retire any warm spare after terminal colors or appearance change.
    pub(crate) fn spare_colors_changed(&mut self) {
        if self.spares.slot.as_ref().is_some_and(|s| s.state.warm()) {
            self.retire_spare("colors", None);
        }
    }

    /// Schedule a spare check after a tab lands or window focus changes.
    pub(crate) fn spare_poke(&self) {
        let settle = debug_ms("SETTLE").unwrap_or(SETTLE);
        self.spares.arm(Instant::now() + settle);
    }

    /// Attempt to adopt the spare for a new tab in `window` at `grid`.
    pub(crate) fn try_adopt_spare(
        &mut self,
        window: WindowId,
        asked_from: Option<WindowId>,
        grid: Grid,
        ask: &crate::tab_spawn::SpawnAsk,
    ) -> Result<(Slot, Session), &'static str> {
        let now = Instant::now();
        if self.spare_mode() == SpareMode::Off {
            return Err("off");
        }
        if !matches!(ask.admission, crate::tab_spawn::SpawnAdmission::Plain)
            || ask.exec_command.is_some()
            || ask.observe.is_some()
            || ask.folder_of.is_some()
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
        match slot.state {
            SlotState::Parked | SlotState::Warming | SlotState::Verifying | SlotState::Ready => {}
            SlotState::Booked | SlotState::Spawned => return Err("booked"),
        }
        let warm = slot.state.warm();
        if fingerprint != slot.fingerprint {
            return Err("factory");
        }
        if warm && colors != slot.colors {
            return Err("colors");
        }
        let here = match (&ask.cwd, focused) {
            (Some(cwd), _) => Some(cwd) == slot.cwd.as_ref(),
            (None, Some(f)) => Some(&f.reported) == slot.cwd.as_ref(),
            (None, None) => slot.cwd.is_none(),
        };
        if !here && !(slot.state == SlotState::Parked && self.spare_retargets(slot.inputs.shell)) {
            return Err("folder");
        }
        if warm && grid != slot.grid && !self.spare_blank(slot) {
            return Err("grid");
        }
        let age = now.saturating_duration_since(slot.since);
        let too_old = match slot.state {
            SlotState::Parked => age >= PARKED_MAX_AGE,
            SlotState::Warming => age >= slot.t_ready,
            _ => age >= ready_max_age(),
        };
        if too_old {
            return Err("age");
        }
        if warm && generation != slot.generation {
            return Err("stale");
        }
        if warm && !eligible {
            return Err("gate");
        }
        if slot.link.tainted() != 0 {
            return Err("taint");
        }
        if slot.link.exited()
            || slot
                .session
                .as_ref()
                .is_none_or(|s| s.pty_closed_at.get().is_some())
        {
            return Err("process");
        }
        if in_flight {
            return Err("update");
        }
        let _ = window;
        let mut slot = self.spares.slot.take().ok_or("none")?;
        let session = slot.session.take().ok_or("none")?;
        publish_spare_sid(None);
        Ok((slot, session))
    }

    /// Whether a parked spare of `shell` can `cd` to a new folder on release.
    pub(crate) fn spare_retargets(&self, shell: aterm_core::shell_integration::ShellType) -> bool {
        aterm_core::shell_integration::has_parked_hold(shell)
            && !self.spares.shared.ran_before_hold(shell)
    }

    fn spare_blank(&self, slot: &Slot) -> bool {
        slot.session.as_ref().is_some_and(|s| {
            let t = crate::term_lock(&s.term);
            t.grid().scrollback_lines() == 0
                && (0..usize::from(t.grid().rows()))
                    .all(|r| t.row_text(r).is_none_or(|l| l.trim().is_empty()))
        })
    }

    /// Finalize adoption of spare `id` into `window` and release its hold FIFO.
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
        crate::spawn::prime_agents_if_due(crate::spawn::PrimeCue::SpareAdopted);
        let told = match transit.retarget.get() {
            Some(dir) => transit.link.witness.tell(&format!("go {dir}")),
            None => transit.link.witness.tell("go"),
        };
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
                .record_row(
                    crate::session_timeline_view::SPARE_ADOPTED,
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
        if transit.tier == SlotState::Parked {
            self.observe_spare_run(id, transit.inputs);
        }
        self.spare_poke();
        self.publish_spare();
    }

    /// Wire host callbacks, census, keeper, and recording timeline on an adopted spare.
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
                &session.ctx,
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

    /// Register a visible shell startup in session `id` for gate observation.
    pub(crate) fn observe_spare_run(&mut self, id: u64, inputs: GenInputs) {
        if self.spare_mode() != SpareMode::Auto {
            return;
        }
        let Some(s) = self.pool.get(id) else {
            return;
        };
        if !aterm_core::shell_integration::has_spare_hold(inputs.shell) || !s.rekey_channel {
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
            host_effects0: 0,
            early_control0: 0,
            inputs,
        };
        let _ = self.spares.send(Job::Observe(Box::new(obs)));
    }
}

impl Spares {
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

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod command_override_tests {
    use super::*;

    #[test]
    fn an_explicit_command_leaves_the_spare_for_the_next_ordinary_shell() {
        let mut app = App::headless_for_test();
        app.session_factory.shell_override = Some("/bin/zsh".into());
        let window = WindowId(0);
        let grid = app.spawn_grid(window);
        let id = app.next_session_id;
        app.next_session_id += 1;
        let cwd = app
            .focused_pane_folder(window)
            .map(|folder| folder.reported);
        app.spares.slot = Some(super::tests::parked_slot_for(&app, id, cwd.as_deref()));
        let ask = crate::tab_spawn::SpawnAsk {
            exec_command: Some(vec!["/bin/sh".into()]),
            ..crate::tab_spawn::SpawnAsk::default()
        };
        assert_eq!(
            app.try_adopt_spare(window, Some(window), grid, &ask)
                .map(|_| ()),
            Err("ask")
        );
        assert_eq!(app.spares.slot.as_ref().unwrap().id, id);
        let ordinary = crate::tab_spawn::SpawnAsk::default();
        let (taken, _) = app
            .try_adopt_spare(window, Some(window), grid, &ordinary)
            .expect("the same eligible spare remains available to an ordinary shell");
        assert_eq!(taken.id, id);
        assert!(app.spares.slot.is_none());
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "spare_conformance.rs"]
mod spare_conformance;
