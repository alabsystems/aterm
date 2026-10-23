// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Foreground process input-stall detection, PTY backlog probing, and stall actions.
use aterm_agent::harness::resume::{AfterRestart, RELAUNCH_WORDS};
use aterm_session::input_backlog::{self, Contention, Gate, InputBacklog, InputWord, Liveness};
use aterm_session::sink::SinkWriter;
use aterm_types::keyboard::KeyboardMode;
use std::time::{Duration, Instant};

use aterm_types::control_verbs::{Input, input_of};

use crate::SessionCtx;
use crate::input::InputEvent;
use crate::menu::MenuAction;

/// [`writes_input`] of `verb`'s class, as a test asks it by name.
#[cfg(test)]
pub(crate) fn is_input_writing(verb: &str, rest: &str) -> bool {
    writes_input(input_of(verb), verb, rest.as_bytes())
}

/// Whether a request of a `class` verb writes INPUT BYTES to a session's PTY.
pub(crate) fn writes_input(class: Input, verb: &str, args: &[u8]) -> bool {
    class.contains(Input::WRITES)
        || (verb == "invoke"
            && std::str::from_utf8(args)
                .ok()
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(MenuAction::from_invoke_name)
                .is_some_and(MenuAction::writes_pty_input))
}

/// What the unread-input gate does with one input-writing verb.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    Open,
    Warn(String),
    Refuse(String),
}

impl Admission {
    /// The refusal line, if this is one.
    pub(crate) fn into_refusal(self) -> Option<String> {
        match self {
            Self::Refuse(line) => Some(line),
            Self::Open | Self::Warn(_) => None,
        }
    }
}

/// What an input-writing `verb` meets against `ctx`'s session.
pub(crate) fn admission(ctx: &SessionCtx, verb: &str, rest: &str, unread_ok: bool) -> Admission {
    gate(ctx, input_of(verb), verb, rest.as_bytes(), unread_ok)
}

/// [`admission`] for a request whose verb's class is known.
pub(crate) fn gate(
    ctx: &SessionCtx,
    class: Input,
    verb: &str,
    args: &[u8],
    unread_ok: bool,
) -> Admission {
    if unread_ok || !writes_input(class, verb, args) {
        return Admission::Open;
    }
    let Some(backlog) = ctx.sink.input_backlog() else {
        return Admission::Open;
    };
    let signals = || {
        written_bytes(verb, args, ctx.modes.keyboard_mode())
            .is_some_and(|bytes| backlog.signals_on_write(&bytes))
    };
    let published = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .agent()
        .input
        .clone();
    if ctx.sink.discards() != 0
        && let Some((fact, held)) = published
            .as_ref()
            .and_then(|fact| held_now(fact, &ctx.sink).map(|held| (fact, held)))
    {
        return if signals() {
            Admission::Open
        } else {
            let restart = offered_verb(ctx, fact.restart.leader);
            Admission::Refuse(held_refusal_text(
                &backlog,
                held,
                fact.restart.survived,
                restart.as_deref(),
            ))
        };
    }
    if backlog.queued == 0 {
        return Admission::Open;
    }
    let stopped = fg_stopped(ctx.sink.master());
    let stall_published = published
        .as_ref()
        .is_some_and(|fact| fact.word == InputWord::Stalled);
    let decided = input_backlog::gate(&backlog, stopped, stall_published);
    if decided == Gate::Open || signals() {
        return Admission::Open;
    }
    let _ = ctx.sink.wake_input_hook();
    if decided == Gate::Warn {
        return Admission::Warn(warning_text(&backlog));
    }
    let interrupt =
        written_bytes(verb, args, KeyboardMode::default()).as_deref() == Some(b"\x03".as_slice());
    let liveness = if published.is_some() {
        Liveness::Published
    } else {
        Liveness::Unobserved
    };
    let word = input_backlog::classify(Some(&backlog), stopped, liveness);
    let restart = (word == InputWord::Stalled)
        .then(|| offered_verb(ctx, published.as_ref().and_then(|fact| fact.restart.leader)))
        .flatten();
    Admission::Refuse(refusal_text(&backlog, word, interrupt, restart.as_deref()))
}

fn offered_verb(ctx: &SessionCtx, leader: Option<i32>) -> Option<String> {
    crate::restart_act::offered(ctx, leader, crate::metrics::now_us())
        .map(|offer| aterm_agent::harness::resume::restart_command(None, Some(offer.pid)))
}

/// The refusal an input-writing `verb` answers against `ctx`'s session, or `None` when it may write.
pub(crate) fn refusal(ctx: &SessionCtx, verb: &str, rest: &str, unread_ok: bool) -> Option<String> {
    admission(ctx, verb, rest, unread_ok).into_refusal()
}

/// The fields a warned write adds to its `OK`.
pub(crate) fn warning_text(b: &InputBacklog) -> String {
    format!(
        " warn=input-unread input_bytes={} input_wait_ms={}",
        b.unread(),
        b.wait.as_millis()
    )
}

/// `resp` with a warned write's fields ([`warning_text`]) at the end of its first line.
pub(crate) fn with_warning(resp: String, warning: Option<&str>) -> String {
    let Some(warning) = warning else {
        return resp;
    };
    let first = resp.split('\n').next().unwrap_or("");
    if !first.starts_with("OK")
        || first
            .split_whitespace()
            .any(|w| w == "skipped" || w == "dup=1")
    {
        return resp;
    }
    let at = first.len();
    let mut out = String::with_capacity(resp.len() + warning.len());
    out.push_str(&resp[..at]);
    out.push_str(warning);
    out.push_str(&resp[at..]);
    out
}

fn written_bytes(verb: &str, args: &[u8], mode: KeyboardMode) -> Option<Vec<u8>> {
    if verb == "feed-bin" {
        return Some(args.to_vec());
    }
    let rest = std::str::from_utf8(args).ok()?;
    let event = match verb {
        "key" => crate::control::parse_key(rest)?,
        "ctrl" => crate::control::parse_ctrl(rest)?,
        "feed" => return crate::control::feed_bytes(rest).ok(),
        "send" => return Some(crate::control::send_bytes(rest)),
        _ => return None,
    };
    let InputEvent::Key {
        key,
        mods,
        base_layout,
        layout_text,
        event_type,
    } = event
    else {
        return None;
    };
    Some(aterm_types::keyboard::encode_key_with_text(
        &key,
        mods,
        mode,
        event_type,
        base_layout,
        layout_text,
    ))
}

/// The refusal line for one reading and its word.
pub(crate) fn refusal_text(
    b: &InputBacklog,
    word: InputWord,
    interrupt: bool,
    restart: Option<&str>,
) -> String {
    let wait_ms = b.wait.as_millis();
    let queued = crate::presence::fmt_dur(b.wait);
    let why = match word {
        InputWord::Stopped => "the program is stopped; resume it: signal cont".to_string(),
        _ if interrupt => {
            format!("the program has not read input queued {queued} ago; interrupt it: signal int")
        }
        InputWord::Stalled => format!(
            "the program has not read input queued {queued} ago; restart it: {}",
            restart.unwrap_or("signal term")
        ),
        _ => format!("the program has not read input queued {queued} ago; retry in a moment"),
    };
    format!(
        "ERR busy input-unread bytes={} wait_ms={wait_ms} input={} ({why})\n",
        b.unread(),
        word.as_str()
    )
}

/// The refusal line while a stall is held through a drop aterm made ([`Restart`]).
pub(crate) fn held_refusal_text(
    b: &InputBacklog,
    held: Held,
    survived: bool,
    restart: Option<&str>,
) -> String {
    let why = match (held, restart) {
        (Held::Restart, Some(restart)) if survived => {
            format!("the program is still running after its restart signal; restart it: {restart}")
        }
        (Held::Restart, None) if survived => {
            "the program is still running after its restart signal; end it: signal kill".to_string()
        }
        (Held::Restart, _) => {
            "the program has not ended since its restart signal; retry in a moment".to_string()
        }
        (Held::Flush, restart) => format!(
            "the program has read no input since reset flush dropped its queue; restart it: {}",
            restart.unwrap_or("signal term")
        ),
    };
    format!(
        "ERR busy input-unread bytes={} wait_ms={} input={} ({why})\n",
        b.unread(),
        b.wait.as_millis(),
        InputWord::Stalled.as_str()
    )
}

/// Whether a `turn` re-press would only queue another Enter behind input the program has not read yet.
pub(crate) fn repress_would_queue(sink: &SinkWriter) -> bool {
    sink.input_backlog().is_some_and(|b| b.unread() > 0)
}

/// Before `signal term|kill|hup|quit` reaches the PTY's foreground job.
#[cfg(unix)]
pub(crate) fn discard_before_signal(
    master: i32,
    sink: &SinkWriter,
    sig: libc::c_int,
) -> Option<usize> {
    if !is_restart_signal(sig) {
        return None;
    }
    let echo = aterm_pty::tty_echo(master)?;
    let unread = sink.input_backlog()?;
    if echo.canonical && !fg_stopped(master) {
        return None;
    }
    let dropped = sink.discard_unread_input(aterm_session::sink::Discard::Restart);
    if unread.queued == 0 && unread.spilled == Some(0) {
        return dropped.filter(|n| *n > 0);
    }
    dropped
}

/// After `signal term|kill|hup|quit` has been sent.
#[cfg(unix)]
pub(crate) fn after_signal(sink: &SinkWriter, sig: libc::c_int) {
    if is_restart_signal(sig) {
        let _ = sink.wake_input_hook();
    }
}

/// Whether `sig` is a restart signal.
#[cfg(unix)]
pub(crate) fn is_restart_signal(sig: libc::c_int) -> bool {
    matches!(
        sig,
        libc::SIGTERM | libc::SIGKILL | libc::SIGHUP | libc::SIGQUIT
    )
}

/// Whether the PTY `master`'s foreground job is stopped (`SIGSTOP`/`SIGTSTP`).
pub(crate) fn fg_stopped(master: i32) -> bool {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let pgrp = crate::quit_safety::foreground_pgrp(master);
        pgrp > 0 && pid_stopped(pgrp) == Some(true)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = master;
        false
    }
}

/// The PTY `master`'s foreground group leader's resident set, in MiB, or `None` when unknown.
pub(crate) fn fg_rss_mb(master: i32) -> Option<u64> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let pgrp = crate::quit_safety::foreground_pgrp(master);
        if pgrp <= 0 {
            return None;
        }
        pid_rss_mb(pgrp)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = master;
        None
    }
}

/// The PTY `master`'s foreground group leader and its total CPU time in nanoseconds ([`pid_cpu_ns`]), or `None` when unknown.
pub(crate) fn fg_cpu(master: i32) -> Option<(i32, u64)> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let pgrp = crate::quit_safety::foreground_pgrp(master);
        if pgrp <= 0 {
            return None;
        }
        pid_cpu_ns(pgrp).map(|ns| (pgrp, ns))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = master;
        None
    }
}

/// `pid`'s stop state from its [`crate::quit_safety::pid_bsdinfo`] record, which must also belong to this uid.
#[cfg(target_os = "macos")]
pub(crate) fn pid_stopped(pid: libc::pid_t) -> Option<bool> {
    const SSTOP: u32 = 4;
    if pid <= 1 {
        return None;
    }
    let info = crate::quit_safety::pid_bsdinfo(pid)?;
    // SAFETY: `geteuid` is a side-effect-free libc getter.
    let ours = unsafe { libc::geteuid() };
    if info.pbi_uid != ours {
        return None;
    }
    Some(info.pbi_status == SSTOP)
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct ProcTaskInfo {
    _virtual_size: u64,
    resident_size: u64,
    total_user: u64,
    total_system: u64,
    _threads_times: [u64; 2],
    _counters: [i32; 12],
}

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<ProcTaskInfo>() == 96);

/// `pid`'s resident set in MiB from `proc_pidinfo(PROC_PIDTASKINFO)`, read only when the kernel wrote exactly one whole record.
#[cfg(target_os = "macos")]
pub(crate) fn pid_rss_mb(pid: libc::pid_t) -> Option<u64> {
    pid_task_info(pid).map(|info| info.resident_size / (1024 * 1024))
}

/// `pid`'s total CPU time, user plus system, in nanoseconds.
#[cfg(target_os = "macos")]
pub(crate) fn pid_cpu_ns(pid: libc::pid_t) -> Option<u64> {
    let (numer, denom) = aterm_sysprobe::procs::kernel_timebase()?;
    let info = pid_task_info(pid)?;
    let ticks = u128::from(info.total_user) + u128::from(info.total_system);
    u64::try_from(ticks * u128::from(numer) / u128::from(denom)).ok()
}

#[cfg(target_os = "macos")]
fn pid_task_info(pid: libc::pid_t) -> Option<ProcTaskInfo> {
    const PROC_PIDTASKINFO: libc::c_int = 4;
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<ProcTaskInfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<ProcTaskInfo>()).ok()?;
    // SAFETY: `info` points to `size` writable bytes for `PROC_PIDTASKINFO`.
    let read =
        unsafe { libc::proc_pidinfo(pid, PROC_PIDTASKINFO, 0, info.as_mut_ptr().cast(), size) };
    if read != size {
        return None;
    }
    // SAFETY: `proc_pidinfo` initialized the full `proc_taskinfo` record of plain integers.
    Some(unsafe { info.assume_init() })
}

#[cfg(target_os = "linux")]
fn linux_proc_stat_after_comm(pid: libc::pid_t) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    if pid <= 1 {
        return None;
    }
    let path = format!("/proc/{pid}/stat");
    let meta = std::fs::metadata(&path).ok()?;
    // SAFETY: `geteuid` is a side-effect-free libc getter.
    let ours = unsafe { libc::geteuid() };
    if meta.uid() != ours {
        return None;
    }
    let stat = std::fs::read_to_string(path).ok()?;
    let close_paren = stat.rfind(')')?;
    Some(stat.get(close_paren + 1..)?.trim_start().to_string())
}

#[cfg(target_os = "linux")]
pub(crate) fn pid_stopped(pid: libc::pid_t) -> Option<bool> {
    let tail = linux_proc_stat_after_comm(pid)?;
    let state = tail.split_whitespace().next()?;
    Some(matches!(state, "T" | "t"))
}

#[cfg(target_os = "linux")]
pub(crate) fn pid_rss_mb(pid: libc::pid_t) -> Option<u64> {
    let tail = linux_proc_stat_after_comm(pid)?;
    let rss_pages: u64 = tail.split_whitespace().nth(21)?.parse().ok()?;
    static PAGE_SIZE: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let page_size = (*PAGE_SIZE.get_or_init(|| {
        // SAFETY: `sysconf(_SC_PAGESIZE)` is a thread-safe POSIX query.
        let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        u64::try_from(v).ok().filter(|&b| b > 0)
    }))?;
    Some(rss_pages.saturating_mul(page_size) / (1024 * 1024))
}

#[cfg(target_os = "linux")]
pub(crate) fn pid_cpu_ns(pid: libc::pid_t) -> Option<u64> {
    static CLK_TCK: std::sync::OnceLock<Option<u128>> = std::sync::OnceLock::new();
    let hz = (*CLK_TCK.get_or_init(|| {
        // SAFETY: `sysconf(_SC_CLK_TCK)` is a thread-safe POSIX query.
        let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        (v > 0).then_some(v as u128)
    }))?;
    let tail = linux_proc_stat_after_comm(pid)?;
    let mut fields = tail.split_whitespace();
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    let ticks = u128::from(utime) + u128::from(stime);
    u64::try_from(ticks.saturating_mul(1_000_000_000) / hz).ok()
}

/// The attention owner the server writes a stall under (`meta attention_owner= aterm`).
pub(crate) const SERVER_ATTENTION_OWNER: &str = "aterm";

/// The cadence of a probe that has nothing sooner to wait for.
pub(crate) const RECHECK: Duration = Duration::from_secs(5);

/// The least time between two probes of one session asked for by a wake (the input hook, the session's own output) rather than by its deadline.
pub(crate) const PROBE_MIN_GAP: Duration = Duration::from_millis(250);

/// The least wall time a CPU reading of the foreground leader is compared across.
pub(crate) const SPIN_WINDOW: Duration = Duration::from_secs(1);

/// What one probe saw of the program beside its input queue, and whether aterm itself has dropped its input since.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Activity {
    pub(crate) cpu: Option<(i32, u64)>,
    pub(crate) last_output: Option<Instant>,
    pub(crate) discards: u64,
    pub(crate) restarts: u64,
    pub(crate) read_since_discard: bool,
    pub(crate) contention: Contention,
}

/// How contended this machine's CPUs are now.
pub(crate) fn machine_contention() -> Contention {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn getloadavg(loadavg: *mut f64, nelem: libc::c_int) -> libc::c_int;
        }
        let mut load = [0f64; 2];
        // SAFETY: `load` is a writable 2-element `f64` array passed to `getloadavg`.
        let n = unsafe { getloadavg(load.as_mut_ptr(), 2) };
        if n < 1 {
            return Contention::default();
        }
        let worst = if n >= 2 {
            load[0].max(load[1])
        } else {
            load[0]
        };
        let cpus = std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get);
        Contention::of_load(worst, cpus)
    }
    #[cfg(not(unix))]
    {
        Contention::default()
    }
}

/// Grace period after sending a restart signal before reporting a stall.
pub(crate) const RESTART_GRACE: Duration = Duration::from_secs(5);

/// What a published stall keeps so that a restart is not taken for a recovery.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Restart {
    pub(crate) leader: Option<i32>,
    pub(crate) discards: u64,
    pub(crate) restarts: u64,
    pub(crate) survived: bool,
}

impl Restart {
    /// Whether aterm has dropped the queue of a stall published with this record.
    pub(crate) fn dropped(&self, discards: u64) -> bool {
        self.leader.is_some() && discards != self.discards
    }

    /// Return whether a drop in the stall window was caused by a session restart.
    pub(crate) fn restarted(&self, restarts: u64) -> bool {
        self.leader.is_some() && restarts != self.restarts
    }
}

/// Which drop a published stall is held through now, for the refusal's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Held {
    Restart,
    Flush,
}

fn held_now(fact: &InputStallFact, sink: &SinkWriter) -> Option<Held> {
    let restarted = fact.restart.restarted(sink.restart_discards());
    if !fact.restart.dropped(sink.discards()) {
        return None;
    }
    if sink.read_since_discard() {
        let _ = sink.wake_input_hook();
        return None;
    }
    (fg_cpu(sink.master()).map(|(pid, _)| pid) == fact.restart.leader).then_some(if restarted {
        Held::Restart
    } else {
        Held::Flush
    })
}

fn spin_window(
    mark: Option<(i32, Instant, u64)>,
    spinning: bool,
    cpu: Option<(i32, u64)>,
    contention: Contention,
    now: Instant,
) -> (Option<(i32, Instant, u64)>, bool) {
    let Some((pid, ns)) = cpu else {
        return (None, false);
    };
    match mark {
        Some((leader, at, burned_at)) if leader == pid => {
            let wall = now.saturating_duration_since(at);
            if wall < SPIN_WINDOW {
                return (mark, spinning);
            }
            let wall_ns = u64::try_from(wall.as_nanos()).unwrap_or(u64::MAX);
            let burned = ns.saturating_sub(burned_at);
            (
                Some((pid, now, ns)),
                burned.saturating_mul(2) >= contention.fair_share_ns(wall_ns),
            )
        }
        _ => (Some((pid, now, ns)), false),
    }
}

/// One session's published input stall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputStallFact {
    pub(crate) word: InputWord,
    pub(crate) since: Instant,
    pub(crate) since_unix: Option<i64>,
    pub(crate) bytes: usize,
    pub(crate) stopped: bool,
    pub(crate) rss_mb: Option<u64>,
    pub(crate) restart: Restart,
    pub(crate) act: Option<StallAct>,
}

/// The explicit restart a published stall's words name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StallAct {
    pub(crate) cmd: String,
    pub(crate) promised: bool,
}

/// What one probe did to a session's published stall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Transition {
    None,
    Entered(InputStallFact),
    Left,
    Updated(InputStallFact),
}

#[derive(Clone, Debug)]
struct InputWatch {
    next_probe: Instant,
    last_probe: Instant,
    published: Option<InputStallFact>,
    cpu_mark: Option<(i32, Instant, u64)>,
    spinning: bool,
    dropped: Option<(Instant, bool)>,
    restarted: Option<Instant>,
}

/// Every session's input watch.
#[derive(Debug, Default)]
pub(crate) struct InputWatches {
    watches: std::collections::HashMap<u64, InputWatch>,
}

impl InputWatches {
    /// Fold one PTY backlog probe for session `id` and update stall state.
    pub(crate) fn step(
        &mut self,
        id: u64,
        backlog: Option<&InputBacklog>,
        stopped: bool,
        activity: Activity,
        rss: impl FnOnce() -> Option<u64>,
        now: Instant,
    ) -> (Transition, Option<Instant>) {
        let prior = self.watches.remove(&id);
        let (cpu_mark, spinning) = spin_window(
            prior.as_ref().and_then(|w| w.cpu_mark),
            prior.as_ref().is_some_and(|w| w.spinning),
            activity.cpu,
            activity.contention,
            now,
        );
        let dropped_before = prior.as_ref().and_then(|w| w.dropped);
        let restarted_before = prior.as_ref().and_then(|w| w.restarted);
        let mut was = prior.and_then(|w| w.published);
        let wait = backlog.map_or(Duration::ZERO, |b| b.wait);
        let since = now.checked_sub(wait).unwrap_or(now);
        let leader = activity.cpu.map(|(pid, _)| pid);
        let hold = was
            .as_ref()
            .filter(|fact| {
                fact.restart.dropped(activity.discards)
                    && !matches!(
                        input_backlog::classify(backlog, stopped, Liveness::Published),
                        InputWord::Stopped | InputWord::Unmeasured
                    )
            })
            .map(|fact| {
                let (at, spun) = dropped_before.unwrap_or((now, spinning));
                let stopped_spinning =
                    spun && !spinning && !backlog.is_some_and(input_backlog::lagging);
                let alive = leader != fact.restart.leader
                    || activity.last_output.is_some_and(|out| out > at)
                    || activity.read_since_discard
                    || stopped_spinning;
                (!alive).then_some((at, spun))
            });
        let recovered = matches!(hold, Some(None));
        let dropped = hold.flatten();
        let restarted = dropped
            .filter(|_| {
                was.as_ref()
                    .is_some_and(|fact| fact.restart.restarted(activity.restarts))
            })
            .map(|_| restarted_before.unwrap_or(now));
        if recovered {
            was = None;
        }
        let liveness = if was.is_some() {
            Liveness::Published
        } else {
            Liveness::Watched {
                drew: activity.last_output.is_some_and(|at| at > since),
                spinning,
                contention: activity.contention,
            }
        };
        let word = if dropped.is_some() {
            InputWord::Stalled
        } else {
            input_backlog::classify(backlog, stopped, liveness)
        };
        let survived = restarted.is_some_and(|at| now >= at + RESTART_GRACE);
        let next_probe = match word {
            InputWord::Unmeasured | InputWord::Clear => None,
            InputWord::Pending => Some(
                Some(since + activity.contention.stretch(input_backlog::STALL_AFTER))
                    .filter(|at| *at > now)
                    .unwrap_or(now + RECHECK),
            ),
            InputWord::Typeahead | InputWord::Stalled | InputWord::Stopped => Some(
                restarted
                    .filter(|_| !survived)
                    .map_or(now + RECHECK, |at| (at + RESTART_GRACE).min(now + RECHECK)),
            ),
        };
        let stalled = matches!(word, InputWord::Stalled | InputWord::Stopped);
        let (transition, published) = match (was, stalled) {
            (None, false) if recovered => (Transition::Left, None),
            (None, false) => (Transition::None, None),
            (Some(_), false) => (Transition::Left, None),
            (None, true) => {
                let fact = InputStallFact {
                    word,
                    since,
                    since_unix: None,
                    bytes: backlog.map_or(0, InputBacklog::unread),
                    stopped: word == InputWord::Stopped,
                    rss_mb: rss(),
                    restart: Restart {
                        leader,
                        discards: activity.discards,
                        restarts: activity.restarts,
                        survived: false,
                    },
                    act: None,
                };
                (Transition::Entered(fact.clone()), Some(fact))
            }
            (Some(old), true) => {
                let fact = InputStallFact {
                    word,
                    stopped: word == InputWord::Stopped,
                    restart: Restart {
                        survived,
                        ..old.restart
                    },
                    ..old.clone()
                };
                if fact == old {
                    (Transition::None, Some(old))
                } else {
                    (Transition::Updated(fact.clone()), Some(fact))
                }
            }
        };
        if let Some(next_probe) = next_probe {
            self.watches.insert(
                id,
                InputWatch {
                    next_probe,
                    last_probe: now,
                    published,
                    cpu_mark,
                    spinning,
                    dropped,
                    restarted,
                },
            );
        }
        (transition, next_probe)
    }

    /// Session `id`'s published stall.
    pub(crate) fn published(&self, id: u64) -> Option<&InputStallFact> {
        self.watches.get(&id).and_then(|w| w.published.as_ref())
    }

    /// A watch publishing `fact` for session `id`, as the probe that entered it leaves one, for a test of what reads the publication.
    #[cfg(test)]
    pub(crate) fn publish_for_test(&mut self, id: u64, fact: InputStallFact, now: Instant) {
        self.watches.insert(
            id,
            InputWatch {
                next_probe: now + RECHECK,
                last_probe: now,
                published: Some(fact),
                cpu_mark: None,
                spinning: false,
                dropped: None,
                restarted: None,
            },
        );
    }

    /// Give session `id`'s published stall what its publication settled.
    pub(crate) fn settle_publication(
        &mut self,
        id: u64,
        act: Option<StallAct>,
        since_unix: Option<i64>,
    ) {
        if let Some(fact) = self.watches.get_mut(&id).and_then(|w| w.published.as_mut()) {
            fact.act = act;
            fact.since_unix = since_unix;
        }
    }

    /// Whether session `id` is being watched (its output wakes probe it).
    pub(crate) fn armed(&self, id: u64) -> bool {
        self.watches.contains_key(&id)
    }

    /// Whether the hook wake of session `id` brought only the terminal's own answers to its program's queries, which start no watch.
    pub(crate) fn answers_only(&self, id: u64, driven: bool) -> bool {
        !driven && !self.watches.contains_key(&id)
    }

    /// Whether a wake-driven probe of `id` must wait.
    pub(crate) fn throttled(&self, id: u64, now: Instant) -> bool {
        self.watches
            .get(&id)
            .is_some_and(|w| now < w.last_probe + PROBE_MIN_GAP)
    }

    /// A throttled wake is deferred, never dropped.
    pub(crate) fn defer(&mut self, id: u64) {
        if let Some(w) = self.watches.get_mut(&id) {
            w.next_probe = w.next_probe.min(w.last_probe + PROBE_MIN_GAP);
        }
    }

    /// The sessions whose deadline has come.
    pub(crate) fn due(&self, now: Instant) -> Vec<u64> {
        self.watches
            .iter()
            .filter(|(_, w)| w.next_probe <= now)
            .map(|(id, _)| *id)
            .collect()
    }

    /// The earliest deadline, for the event loop (`None` = arm nothing).
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        self.watches.values().map(|w| w.next_probe).min()
    }

    /// Stop watching `id`.
    pub(crate) fn forget(&mut self, id: u64) -> Option<InputStallFact> {
        self.watches.remove(&id).and_then(|w| w.published)
    }
}

/// One probe of a session's input for [`InputWatches::step`].
pub(crate) fn probe(
    sink: &SinkWriter,
    published: bool,
    last_output: Option<Instant>,
) -> (Option<InputBacklog>, bool, Activity) {
    let backlog = sink.input_backlog();
    let master = sink.master();
    let unread = backlog.is_some_and(|b| b.unread() > 0);
    let stopped = unread && fg_stopped(master);
    let restarts = sink.restart_discards();
    let discards = sink.discards();
    let looks = (unread || published) && !stopped;
    let activity = Activity {
        cpu: looks.then(|| fg_cpu(master)).flatten(),
        last_output,
        discards,
        restarts,
        read_since_discard: published && discards != 0 && sink.read_since_discard(),
        contention: if looks {
            machine_contention()
        } else {
            Contention::default()
        },
    };
    (backlog, stopped, activity)
}

/// Give `sink` its input watch.
pub(crate) fn watch_input(sink: &SinkWriter, wake: impl Fn() + Send + Sync + 'static) -> bool {
    if aterm_pty::input_queue_len(sink.master()).is_none() {
        return false;
    }
    sink.install_input_hook(wake);
    sink.input_backlog().is_some_and(|b| b.unread() > 0) && sink.wake_input_hook()
}

/// The four `status` fields, after `integration=`.
pub(crate) fn status_fields(
    backlog: Option<&InputBacklog>,
    word: InputWord,
    rss_mb: Option<u64>,
) -> String {
    let dash = || "-".to_string();
    format!(
        "input={} input_bytes={} input_wait_ms={} fg_rss_mb={}",
        word.as_str(),
        backlog.map_or_else(dash, |b| b.unread().to_string()),
        backlog.map_or_else(dash, |b| b.wait.as_millis().to_string()),
        rss_mb.map_or_else(dash, |mb| mb.to_string()),
    )
}

/// `status`'s four fields for `sink`, probed live on the call.
pub(crate) fn status_input(
    sink: &SinkWriter,
    published: Option<&InputStallFact>,
    watched: bool,
) -> String {
    let backlog = sink.input_backlog();
    if !watched && backlog.is_some_and(|b| b.unread() > 0) {
        let _ = sink.wake_input_hook();
    }
    let stopped = backlog.is_some_and(|b| b.unread() > 0) && fg_stopped(sink.master());
    let liveness = if published.is_some() {
        Liveness::Published
    } else {
        Liveness::Unobserved
    };
    let word = match input_backlog::classify(backlog.as_ref(), stopped, liveness) {
        word @ (InputWord::Stopped | InputWord::Unmeasured) => word,
        word => {
            if published.is_some_and(|fact| held_now(fact, sink).is_some()) {
                InputWord::Stalled
            } else {
                word
            }
        }
    };
    let rss = matches!(word, InputWord::Stalled | InputWord::Stopped)
        .then(|| fg_rss_mb(sink.master()))
        .flatten();
    status_fields(backlog.as_ref(), word, rss)
}

/// The local wall-clock `HH:MM` of the Unix second `since_unix`, under the zone's offset from UTC.
pub(crate) fn since_hhmm(since_unix: i64, offset_s: Option<i64>) -> Option<String> {
    let local = since_unix.saturating_add(offset_s?).rem_euclid(86_400);
    Some(format!("{:02}:{:02}", local / 3600, (local % 3600) / 60))
}

/// The Unix second at which `since` fell.
pub(crate) fn since_unix_of(since: Instant) -> Option<i64> {
    let ago = Instant::now().saturating_duration_since(since);
    let wall = aterm_time::since_epoch()?;
    i64::try_from(wall.checked_sub(ago)?.as_secs()).ok()
}

/// The server's attention line for a stall, at most [`crate::session_timeline::META_ATTENTION_KEYED_MAX`] bytes once stored.
pub(crate) fn attention_text(
    fact: &InputStallFact,
    program: Option<&str>,
    after: &AfterRestart,
    sid: &str,
    clock: Option<&str>,
    waited: Duration,
) -> String {
    let prog = program.filter(|p| !p.is_empty()).unwrap_or("the program");
    let when = clock.map_or_else(
        || format!("for {}", crate::presence::fmt_dur(waited)),
        |hhmm| format!("since {hhmm}"),
    );
    if fact.stopped {
        return format!(
            "{prog} is stopped with input queued {when} \u{2014} resume it: aterm ctl @{sid} \
             signal cont"
        );
    }
    let rss = fact.rss_mb.map(|mb| {
        if mb < 1024 {
            // A small program reads `rss 1 MB`, never `rss 0.0 GB`.
            return format!("rss {mb} MB");
        }
        let tenths = (mb * 10 + 512) / 1024;
        format!("rss {}.{} GB", tenths / 10, tenths % 10)
    });
    let explicit = fact.act.as_ref().map(|act| act.cmd.as_str());
    let head = |rss: Option<&str>| {
        if fact.restart.survived {
            let rss = rss.map_or_else(String::new, |rss| format!(" ({rss})"));
            let remedy = explicit.map_or_else(
                || format!("end it: aterm ctl @{sid} signal kill"),
                |cmd| format!("restart it: {cmd}"),
            );
            format!(
                "{prog} is still running after its restart signal, not reading input {when}{rss} \
                 \u{2014} {remedy}"
            )
        } else {
            let rss = rss.map_or_else(String::new, |rss| format!(", {rss}"));
            let cmd =
                explicit.map_or_else(|| format!("aterm ctl @{sid} signal term"), str::to_string);
            format!(
                "{prog} is frozen: not reading input {when} ({} B queued{rss}) \u{2014} restart \
                 it: {cmd}",
                fact.bytes
            )
        }
    };
    let tails: Vec<String> = match after {
        AfterRestart::Unknown | AfterRestart::Act => Vec::new(),
        AfterRestart::Relaunch => vec![format!("; {RELAUNCH_WORDS}")],
        AfterRestart::Command(line) => std::iter::once(line.clone())
            .chain(aterm_agent::harness::resume::shorter(line))
            .map(|cmd| format!(", then {cmd}"))
            .collect(),
    };
    [rss.as_deref(), None]
        .into_iter()
        .flat_map(|rss| tails.iter().map(move |tail| (rss, tail)))
        .map(|(rss, tail)| format!("{}{tail}", head(rss)))
        .find(|text| text.len() <= crate::session_timeline::META_ATTENTION_KEYED_MAX)
        .unwrap_or_else(|| head(rss.as_deref()))
}

/// What follows the restart of the stalled program in session `tl` ([`attention_text`]'s `<after>`).
pub(crate) fn after_restart(
    tl: &crate::session_timeline::SessionTimeline,
    act: Option<&StallAct>,
) -> AfterRestart {
    if let Some(act) = act {
        return if act.promised {
            AfterRestart::Relaunch
        } else {
            AfterRestart::Act
        };
    }
    tl.claude_footer()
        .and_then(|facts| facts.resume.clone())
        .map_or(AfterRestart::Unknown, AfterRestart::Command)
}

fn act_of(ctx: &SessionCtx, fact: &InputStallFact) -> Option<StallAct> {
    if fact.stopped {
        return None;
    }
    let offer = crate::restart_act::offered(ctx, fact.restart.leader, crate::metrics::now_us())?;
    Some(StallAct {
        cmd: aterm_agent::harness::resume::restart_command(
            Some(ctx.self_id.as_str()),
            Some(offer.pid),
        ),
        promised: offer.promised,
    })
}

/// Why the host's hand is not the one on session `ctx` at `now_us` (`None`: it is).
pub(crate) fn host_hand_refusal(ctx: &crate::SessionCtx, now_us: u64) -> Option<&'static str> {
    let own = crate::harness_host::holder();
    let claimed = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .live_claim(now_us)
        .is_some_and(|claim| claim.holder == own || claim.proven.is_some());
    if !claimed {
        return Some("not-supervised");
    }
    if ctx.fabric.hold().is_some() {
        return Some("halted");
    }
    let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
    match lease.as_ref() {
        Some(crate::Lease::Turn {
            driver: Some(_), ..
        }) => Some("driver-turn"),
        Some(crate::Lease::Drive { expires_us, .. }) if *expires_us > now_us => {
            Some("driver-lease")
        }
        _ => None,
    }
}

/// [`attention_text`] at `now`, with the zone read here.
pub(crate) fn attention_now(
    fact: &InputStallFact,
    program: Option<&str>,
    after: &AfterRestart,
    sid: &str,
    now: Instant,
) -> String {
    let clock = fact
        .since_unix
        .and_then(|since_unix| since_hhmm(since_unix, crate::presence::local_offset_s()));
    attention_text(
        fact,
        program,
        after,
        sid,
        clock.as_deref(),
        now.saturating_duration_since(fact.since),
    )
}

/// The identity of one stall episode, for the menu herald's notification key.
pub(crate) fn episode_key(fact: &InputStallFact) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    fact.since.hash(&mut h);
    h.finish()
}

/// Format the stall summary row for the session status menu.
pub(crate) fn menu_row(
    fact: &InputStallFact,
    program: Option<&str>,
    after: &AfterRestart,
    sid: &str,
    now: Instant,
) -> crate::status_item::InputStallRow {
    crate::status_item::InputStallRow {
        text: attention_now(fact, program, after, sid, now),
        key: episode_key(fact),
        stopped: fact.stopped,
    }
}

impl crate::App {
    /// Probe the input watches that are owed a probe and publish what moved.
    pub(crate) fn observe_input_stalls(&mut self, now: Instant, only: Option<u64>) -> Vec<u64> {
        let ids = match only {
            Some(id) if self.session_status.inputs.throttled(id, now) => {
                self.session_status.inputs.defer(id);
                return Vec::new();
            }
            Some(id) => {
                // marked what this takes.
                if let Some(session) = self.pool.get(id) {
                    session.ctx.sink.rearm_input_hook();
                    let driven = session.ctx.sink.take_input_hook_driven();
                    if self.session_status.inputs.answers_only(id, driven) {
                        return Vec::new();
                    }
                }
                vec![id]
            }
            None => self.session_status.inputs.due(now),
        };
        let mut moved = Vec::new();
        for id in ids {
            let Some(session) = self.pool.get(id) else {
                let _ = self.session_status.inputs.forget(id);
                continue;
            };
            let ctx = session.ctx.clone();
            let output_ns = session
                .latest_output_activity_ns
                .load(std::sync::atomic::Ordering::Acquire);
            ctx.sink.rearm_input_hook();
            let _ = ctx.sink.take_input_hook_driven();
            let (backlog, stopped, activity) = probe(
                &ctx.sink,
                self.session_status.inputs.published(id).is_some(),
                (output_ns != 0).then(|| self.lat_epoch + Duration::from_nanos(output_ns)),
            );
            let master = ctx.sink.master();
            let (transition, _) = self.session_status.inputs.step(
                id,
                backlog.as_ref(),
                stopped,
                activity,
                || fg_rss_mb(master),
                now,
            );
            let fact = match transition {
                Transition::None => match self.session_status.inputs.published(id) {
                    Some(fact) if act_of(&ctx, fact) != fact.act => Some(fact.clone()),
                    _ => continue,
                },
                Transition::Entered(fact) | Transition::Updated(fact) => Some(fact),
                Transition::Left => None,
            };
            self.publish_input_stall(id, &ctx, fact, now);
            moved.push(id);
        }
        moved
    }

    /// Stop watching a session whose PTY ended, unpublishing a stall it still shows.
    pub(crate) fn retire_input_watch(&mut self, session: u64) {
        if self.session_status.forget_input(session).is_none() {
            return;
        }
        if let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) {
            self.publish_input_stall(session, &ctx, None, Instant::now());
        }
    }

    /// A `Wake::InputWatch` the pre-Commit gate swallowed (`App::drops_wake_before_commit`) releases its sink's input hook.
    pub(crate) fn release_input_wake_dropped_before_commit(&self, ev: &crate::Wake) {
        if let crate::Wake::InputWatch { session } = ev
            && let Some(s) = self.pool.get(*session)
        {
            s.ctx.sink.rearm_input_hook();
        }
    }

    /// At Commit, probe every adopted session's input (`handoff_local_id` set) once, as its attach wake would have.
    #[cfg(unix)]
    pub(crate) fn rewatch_adopted_input(&mut self, now: Instant) -> Vec<u64> {
        let adopted: Vec<u64> = self
            .pool
            .iter()
            .filter(|s| s.handoff_local_id.is_some())
            .map(|s| s.id)
            .collect();
        adopted
            .into_iter()
            .flat_map(|id| self.observe_input_stalls(now, Some(id)))
            .collect()
    }

    fn publish_input_stall(
        &mut self,
        id: u64,
        ctx: &std::sync::Arc<SessionCtx>,
        fact: Option<InputStallFact>,
        now: Instant,
    ) {
        let fact = fact.map(|fact| InputStallFact {
            act: act_of(ctx, &fact),
            since_unix: fact.since_unix.or_else(|| since_unix_of(fact.since)),
            ..fact
        });
        if let Some(fact) = &fact {
            self.session_status
                .inputs
                .settle_publication(id, fact.act.clone(), fact.since_unix);
        }
        let (agent_moved, text, wake) = {
            let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let text = fact.as_ref().map(|fact| {
                attention_now(
                    fact,
                    tl.agent().program.as_deref(),
                    &after_restart(&tl, fact.act.as_ref()),
                    ctx.self_id.as_str(),
                    now,
                )
            });
            let moved = tl.set_input(fact);
            (moved, text, tl.slog_wake())
        };
        self.session_status.slog_wakes.note(id, wake);
        if agent_moved {
            // The A/B book (Settings ▸ Harness): a stall moves the word too.
            self.note_harness_phase(id);
        }
        let attention_moved =
            crate::session_timeline::write_server_attention(ctx, SERVER_ATTENTION_OWNER, text);
        if attention_moved {
            self.refresh_meta_dependent_chrome(id);
            self.mark_status_dirty();
        }
        self.refresh_presence_session(id);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use super::*;

    #[cfg(target_os = "macos")]
    const TICK: Duration = Duration::from_millis(1100);

    /// Publish a `stalled` fact for `ctx`'s session.
    #[cfg(target_os = "macos")]
    pub(crate) fn publish_stall(ctx: &SessionCtx) {
        ctx.sink
            .backdate_input_for_testing(input_backlog::STALL_AFTER + Duration::from_secs(2));
        let _ = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_input(Some(stalled(Instant::now())));
    }

    fn stalled(since: Instant) -> InputStallFact {
        InputStallFact {
            word: InputWord::Stalled,
            since,
            since_unix: None,
            bytes: 1,
            stopped: false,
            rss_mb: None,
            restart: Restart::default(),
            act: None,
        }
    }

    fn claude_in_front(ctx: &SessionCtx, pid: i32, word: &'static str) {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(pid);
        tl.set_program(pid, Some("claude".into()));
        tl.publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    }

    #[test]
    fn a_warning_rides_the_first_line_of_a_reply_that_wrote() {
        let spilled = InputBacklog {
            spilled: Some(4),
            ..backlog(3, Duration::from_millis(1500), false)
        };
        let w = warning_text(&spilled);
        assert_eq!(w, " warn=input-unread input_bytes=7 input_wait_ms=1500");
        assert_eq!(
            with_warning("OK seq=5\n".to_string(), Some(&w)),
            format!("OK seq=5{w}\n")
        );
        let turn = "OK 2 turn submitted=1 status=settled seq=9 id=3 dur_ms=40 hash=00ff\na\nb\n";
        assert_eq!(
            with_warning(turn.to_string(), Some(&w)),
            format!(
                "OK 2 turn submitted=1 status=settled seq=9 id=3 dur_ms=40 hash=00ff{w}\na\nb\n"
            )
        );
        for unchanged in [
            "ERR busy input-unread bytes=1\n",
            "OK skipped seq=4\n",
            "OK 0 turn skipped reason=changed submitted=0 seq=4 id=1\n",
            "OK dup=1\n",
        ] {
            assert_eq!(with_warning(unchanged.to_string(), Some(&w)), unchanged);
        }
        assert_eq!(with_warning("OK seq=5\n".to_string(), None), "OK seq=5\n");
    }

    fn backlog(queued: usize, wait: Duration, canonical: bool) -> InputBacklog {
        InputBacklog {
            queued,
            spilled: Some(0),
            wait,
            canonical,
            signals: None,
            output_backlog: Some(0),
        }
    }

    /// A real pty pair with a raw slave nobody reads.
    #[cfg(target_os = "macos")]
    pub(crate) fn raw_pty_pair() -> (i32, i32) {
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: zeroed `termios` is valid and filled by `tcgetattr` on the open slave fd.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `slave` is open and `t` is a valid out-param.
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0, "tcgetattr");
        // SAFETY: `t` is an initialized termios.
        unsafe { libc::cfmakeraw(&mut t) };
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        // SAFETY: `t` is a valid termios for the open slave.
        assert_eq!(
            unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) },
            0,
            "tcsetattr"
        );
        aterm_pty::set_nonblocking(master, true).expect("nonblocking master");
        (master, slave)
    }

    /// Everything the slave has queued, read in one `read`, as a program reads keys queued together.
    #[cfg(target_os = "macos")]
    pub(crate) fn slave_read_all(slave: i32) -> Vec<u8> {
        let mut buf = [0u8; 256];
        // SAFETY: `slave` is open and `buf` is a writable buffer of its length.
        let n = unsafe { libc::read(slave, buf.as_mut_ptr().cast(), buf.len()) };
        assert!(n >= 0, "read(slave)");
        buf[..usize::try_from(n).unwrap_or(0)].to_vec()
    }

    #[test]
    fn invoke_writes_input_only_for_an_action_that_writes_input() {
        assert!(is_input_writing("invoke", "Paste"));
        assert!(is_input_writing("invoke", "Paste extra"));
        for name in [
            "SelectAll",
            "Copy",
            "NewTab",
            "OpenPalette",
            "NoSuchAction",
            "",
        ] {
            assert!(!is_input_writing("invoke", name), "invoke {name}");
        }
        assert!(is_input_writing("send", ""));
        assert!(!is_input_writing("status", ""));
        assert!(
            !is_input_writing("signal", "term"),
            "a remedy writes no input"
        );
    }

    #[test]
    fn refusal_text_is_one_busy_line_with_the_facts_and_the_remedy() {
        let spilled = InputBacklog {
            spilled: Some(5),
            ..backlog(2, Duration::from_secs(700), false)
        };
        let empty = backlog(0, Duration::ZERO, false);
        let act = Some("restart pid=4242 if-stalled");
        for (line, head, tail) in [
            (
                refusal_text(&spilled, InputWord::Stalled, false, act),
                "bytes=7 wait_ms=700000 input=stalled",
                "restart pid=4242 if-stalled)",
            ),
            (
                refusal_text(&spilled, InputWord::Stopped, false, None),
                "bytes=7 wait_ms=700000 input=stopped",
                "signal cont)",
            ),
            (
                held_refusal_text(&empty, Held::Restart, true, act),
                "bytes=0 wait_ms=0 input=stalled",
                "restart pid=4242 if-stalled)",
            ),
        ] {
            assert!(
                line.starts_with(&format!("ERR busy input-unread {head} (")),
                "{line}"
            );
            assert!(
                line.ends_with(&format!("{tail}\n")) && line.matches('\n').count() == 1,
                "{line}"
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn pid_stopped_reads_a_stopped_child_and_its_resume() {
        use std::time::Instant;
        let mut child = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = libc::pid_t::try_from(child.id()).expect("pid");
        let until = |want: bool| {
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                if pid_stopped(pid) == Some(want) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            false
        };
        assert!(until(false), "a running child is not stopped");
        // SAFETY: `pid` is this test's own live child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        assert!(until(true), "SIGSTOP reads as stopped");
        // SAFETY: as above.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
        assert!(until(false), "SIGCONT reads as running again");
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(pid_stopped(0), None);
        assert_eq!(pid_stopped(-5), None);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_slow_read_is_warned_and_only_a_published_stall_refuses() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (master, slave) = raw_pty_pair();
        let ctx = crate::session_store::test_handle_on(9, master).ctx;
        ctx.sink.note_master_nonblocking(true);
        let wakes = std::sync::Arc::new(AtomicUsize::new(0));
        {
            let wakes = wakes.clone();
            ctx.sink.install_input_hook(move || {
                wakes.fetch_add(1, Ordering::SeqCst);
            });
        }
        ctx.sink
            .write_frame_nonparking(b"\r")
            .expect("the human's Enter");
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "the Enter landed");
        ctx.sink.rearm_input_hook();
        assert_eq!(admission(&ctx, "key", "down", false), Admission::Open);
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "a pass wakes nothing");
        ctx.sink.backdate_input_for_testing(TICK);

        let Admission::Warn(warning) = admission(&ctx, "key", "down", false) else {
            panic!("a slow read is warned, not refused");
        };
        assert!(
            warning.starts_with(" warn=input-unread input_bytes=1 input_wait_ms="),
            "{warning}"
        );
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            2,
            "the warning woke the watch"
        );
        assert_eq!(admission(&ctx, "key", "down", true), Admission::Open);

        publish_stall(&ctx);
        let refused = refusal(&ctx, "key", "down", false).expect("refused");
        assert!(
            refused.starts_with("ERR busy input-unread bytes=1 "),
            "{refused}"
        );
        assert!(refused.contains(" input=stalled "), "{refused}");
        assert!(
            refused.ends_with("; restart it: signal term)\n"),
            "{refused}"
        );
        assert!(refusal(&ctx, "invoke", "Paste", false).is_some());
        assert_eq!(refusal(&ctx, "key", "down", true), None);
        for verb in ["signal", "resize", "close", "reset"] {
            assert_eq!(refusal(&ctx, verb, "term", false), None, "{verb}");
        }
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            2,
            "no probe between them: no second wake"
        );
        assert_eq!(slave_read_all(slave), b"\r");
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    fn written_bytes_are_the_bytes_the_seam_writes() {
        let legacy = KeyboardMode::empty();
        for (verb, rest, want) in [
            ("key", "ctrl+c", &b"\x03"[..]),
            ("key", "c mods=ctrl", b"\x03"),
            ("key", "ctrl+\\", b"\x1c"),
            ("key", "ctrl+z", b"\x1a"),
            ("ctrl", "c", b"\x03"),
            ("ctrl", "C", b"\x03"),
            ("ctrl", "z", b"\x1a"),
            ("feed", "03", b"\x03"),
            ("send", "\x03", b"\x03"),
            ("key", "down", b"\x1b[B"),
            ("key", "enter", b"\r"),
            ("feed-bin", "\x03", b"\x03"),
            ("feed-bin", "03", b"03"),
        ] {
            assert_eq!(
                written_bytes(verb, rest.as_bytes(), legacy).as_deref(),
                Some(want),
                "{verb} {rest:?}"
            );
        }
        let kitty =
            written_bytes("key", b"ctrl+c", KeyboardMode::DISAMBIGUATE_ESC_CODES).expect("encodes");
        assert_ne!(kitty, b"\x03", "the kitty protocol reports ctrl+c as a key");
        for (verb, rest) in [
            ("key", "no-such-key"),
            ("ctrl", "cc"),
            ("feed", "zz"),
            ("paste", "x"),
            ("mouse", "click 1 1"),
            ("turn", "x"),
            ("paste-bin", "\x03"),
        ] {
            assert_eq!(
                written_bytes(verb, rest.as_bytes(), legacy),
                None,
                "{verb} {rest}"
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_signal_character_into_a_cbreak_program_is_not_refused() {
        let (master, slave) = raw_pty_pair();
        // SAFETY: all-zeros is a valid termios, overwritten by `tcgetattr`.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `slave` is this test's live pty slave; `t` is an out-param.
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0);
        t.c_lflag |= libc::ISIG;
        // SAFETY: `slave` is live; `t` is derived from its own termios.
        assert_eq!(unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) }, 0);
        let ctx = crate::session_store::test_handle_on(9, master).ctx;
        ctx.sink.note_master_nonblocking(true);
        ctx.sink
            .write_frame_nonparking(b"q")
            .expect("a human's key");
        publish_stall(&ctx);

        assert!(
            refusal(&ctx, "key", "down", false).is_some(),
            "a key still queues behind the stale `q`"
        );
        for (verb, rest) in [
            ("key", "ctrl+c"),
            ("key", "ctrl+z"),
            ("key", "ctrl+\\"),
            ("ctrl", "c"),
            ("feed", "03"),
        ] {
            assert_eq!(
                admission(&ctx, verb, rest, false),
                Admission::Open,
                "{verb} {rest}"
            );
        }
        ctx.sink.write_frame_nonparking(b"\x03").expect("^C");
        assert_eq!(
            ctx.sink.input_backlog().map(|b| b.queued),
            Some(0),
            "the ^C was a signal and flushed the `q`: nothing was read behind it"
        );
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);

        let (master, slave) = raw_pty_pair();
        let ctx = crate::session_store::test_handle_on(9, master).ctx;
        ctx.sink.note_master_nonblocking(true);
        ctx.sink
            .write_frame_nonparking(b"q")
            .expect("a human's key");
        publish_stall(&ctx);
        let refused = refusal(&ctx, "key", "ctrl+c", false).expect("raw ^C is input");
        assert!(refused.contains("signal int"), "{refused}");
        assert!(refusal(&ctx, "feed", "03", false).is_some());
        assert_eq!(slave_read_all(slave), b"q", "nothing queued behind it");
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_yielding_turn_asks_the_gate_again_before_its_first_byte() {
        use std::cell::Cell;
        use std::time::Instant;

        let (master, slave) = raw_pty_pair();
        let h = crate::session_store::test_handle_on(9, master);
        h.ctx.sink.note_master_nonblocking(true);
        let store = crate::session_store::new_store();
        store.write().unwrap().register(h.clone());
        let turn = |start: f32, publish: bool, request: &str| -> String {
            let reads = Cell::new(0u32);
            let momentum = || {
                reads.set(reads.get() + 1);
                if publish && reads.get() == 2 {
                    publish_stall(&h.ctx);
                }
                Ok((if reads.get() == 1 { start } else { 0.0 }, Instant::now()))
            };
            let paste = |text: &str| h.ctx.sink.write_frame_nonparking(text.as_bytes()).is_ok();
            let press = |_: &str| h.ctx.sink.write_frame_nonparking(b"\r").is_ok();
            let key = |_: crate::input::InputEvent| false;
            crate::control::cmd_turn(
                &h.term,
                &store,
                h.local_id,
                request,
                &h.ctx,
                &crate::control::TurnIo {
                    paste: &paste,
                    press: &press,
                    key: &key,
                    momentum: &momentum,
                    driver: None,
                    by: crate::turn_ledger::TurnBy::Ctl,
                    actor: aterm_slog::Actor::Ctl,
                    cause: None,
                    mark: None,
                },
            )
        };
        let request = "yield=0.20 timeout=5000 idle=1 submit_window=150 presses=1 -- hi";

        h.ctx
            .sink
            .write_frame_nonparking(b"\r")
            .expect("the human's Enter");
        assert_eq!(
            admission(&h.ctx, "turn", "", false),
            Admission::Open,
            "the turn arrives while the Enter is young: the dispatch lets it in"
        );
        let reply = turn(0.201, false, request);
        assert!(reply.starts_with("OK "), "{reply}");
        assert_eq!(slave_read_all(slave), b"\rhi\r", "typed behind a slow read");

        h.ctx
            .sink
            .write_frame_nonparking(b"\r")
            .expect("the human's Enter");
        assert_eq!(admission(&h.ctx, "turn", "", false), Admission::Open);
        let reply = turn(0.201, true, request);
        assert!(
            reply.starts_with("ERR busy input-unread bytes=1 "),
            "{reply}"
        );
        assert!(reply.contains(" input=stalled "), "{reply}");
        assert_eq!(slave_read_all(slave), b"\r", "nothing typed behind it");
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    fn reading(queued: usize, wait_s: u64, output: usize) -> InputBacklog {
        InputBacklog {
            output_backlog: Some(output),
            ..backlog(queued, Duration::from_secs(wait_s), false)
        }
    }

    fn spin(burned: Duration) -> Activity {
        Activity {
            cpu: Some((4242, u64::try_from(burned.as_nanos()).expect("small"))),
            last_output: None,
            discards: 0,
            restarts: 0,
            read_since_discard: false,
            contention: Contention::default(),
        }
    }

    fn primed(w: &mut InputWatches, id: u64, now: Instant) {
        let at = now.checked_sub(RECHECK).expect("uptime");
        let _ = w.step(
            id,
            Some(&reading(1, 0, 0)),
            false,
            spin(Duration::ZERO),
            || None,
            at,
        );
    }

    #[test]
    fn step_stretches_the_stall_by_the_machines_contention() {
        let loaded = Contention::of_load(150.0, 18);
        let t0 = Instant::now();
        let stretched = Some(t0 + Duration::from_millis(83_330));
        let under = |burned: Duration| Activity {
            contention: loaded,
            ..spin(burned)
        };
        let mut w = InputWatches::default();
        let (t, next) = w.step(
            1,
            Some(&reading(1, 0, 0)),
            false,
            under(Duration::ZERO),
            || None,
            t0,
        );
        assert_eq!((t, next), (Transition::None, stretched));
        let at = Duration::from_secs(12);
        let (t, next) = w.step(
            1,
            Some(&reading(1, 12, 0)),
            false,
            under(at),
            || None,
            t0 + at,
        );
        assert_eq!((t, next), (Transition::None, stretched), "slow, not frozen");
    }

    #[test]
    fn step_stopped_enters_at_once_and_updates_the_episode() {
        let mut w = InputWatches::default();
        let t0 = Instant::now();
        let (t, _) = w.step(
            1,
            Some(&reading(1, 0, 0)),
            true,
            Activity::default(),
            || Some(7),
            t0,
        );
        let Transition::Entered(stop) = t else {
            panic!("entered: {t:?}")
        };
        assert_eq!(stop.word, InputWord::Stopped);
        assert!(stop.stopped);

        let t1 = t0 + Duration::from_secs(11);
        let (t, _) = w.step(
            1,
            Some(&reading(1, 11, 0)),
            false,
            Activity::default(),
            || panic!(),
            t1,
        );
        let Transition::Updated(stall) = t else {
            panic!("updated: {t:?}")
        };
        assert_eq!(stall.word, InputWord::Stalled);
        assert!(!stall.stopped);
        assert_eq!((stall.since, stall.rss_mb), (stop.since, Some(7)));
    }

    #[test]
    fn step_leaves_on_clear_on_young_input_and_on_no_reading() {
        let t0 = Instant::now();
        let entered = |w: &mut InputWatches| {
            primed(w, 1, t0);
            let (t, _) = w.step(
                1,
                Some(&reading(1, 10, 0)),
                false,
                spin(RECHECK),
                || None,
                t0,
            );
            assert!(matches!(t, Transition::Entered(_)), "{t:?}");
        };
        let t1 = t0 + RECHECK;

        let mut w = InputWatches::default();
        entered(&mut w);
        let (t, next) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            Activity::default(),
            || panic!(),
            t1,
        );
        assert_eq!((t, next), (Transition::Left, None));
        assert!(!w.armed(1));

        let mut w = InputWatches::default();
        entered(&mut w);
        let (t, next) = w.step(
            1,
            Some(&reading(1, 2, 0)),
            false,
            Activity::default(),
            || panic!(),
            t1,
        );
        assert_eq!(t, Transition::Left);
        assert_eq!(next, Some(t1 + Duration::from_secs(8)));
        assert!(w.armed(1) && w.published(1).is_none());

        let mut w = InputWatches::default();
        entered(&mut w);
        let (t, _) = w.step(1, None, false, Activity::default(), || panic!(), t1);
        assert_eq!(t, Transition::Left);
        assert!(!w.armed(1));
    }

    #[test]
    fn step_holds_a_stall_whose_queue_aterm_dropped_until_it_draws() {
        let t0 = Instant::now();
        let entered = |w: &mut InputWatches| {
            primed(w, 1, t0);
            let (t, _) = w.step(
                1,
                Some(&reading(1, 10, 0)),
                false,
                spin(RECHECK),
                || None,
                t0,
            );
            assert!(matches!(t, Transition::Entered(_)), "{t:?}");
        };
        let after_drop = |discards: u64, out: Option<Instant>, now: Instant| Activity {
            discards,
            restarts: discards,
            last_output: out,
            ..spin(now.duration_since(t0.checked_sub(RECHECK).expect("uptime")))
        };
        let t1 = t0 + Duration::from_secs(1);

        let mut w = InputWatches::default();
        entered(&mut w);
        let (t, _) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after_drop(0, None, t1),
            || panic!(),
            t1,
        );
        assert_eq!(t, Transition::Left, "a read clears it");

        let mut w = InputWatches::default();
        entered(&mut w);
        let (t, next) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after_drop(1, None, t1),
            || panic!("rss is read at entry only"),
            t1,
        );
        assert_eq!((t, next), (Transition::None, Some(t1 + RESTART_GRACE)));
        let held = w.published(1).expect("held").clone();
        assert_eq!(held.word, InputWord::Stalled);
        assert!(!held.restart.survived);
        let almost = t1 + RESTART_GRACE - Duration::from_millis(1);
        let (t, next) = w.step(
            1,
            Some(&reading(18, 0, 0)),
            false,
            after_drop(1, None, almost),
            || panic!(),
            almost,
        );
        assert_eq!((t, next), (Transition::None, Some(t1 + RESTART_GRACE)));
        let at = t1 + RESTART_GRACE;
        let (t, next) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after_drop(1, None, at),
            || panic!(),
            at,
        );
        let Transition::Updated(fact) = t else {
            panic!("{t:?}")
        };
        assert!(fact.restart.survived, "{fact:?}");
        assert_eq!((fact.word, fact.since), (InputWord::Stalled, held.since));
        assert_eq!(next, Some(at + RECHECK));

        let mut w = InputWatches::default();
        entered(&mut w);
        let _ = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after_drop(1, Some(t1 - Duration::from_millis(1)), t1),
            || None,
            t1,
        );
        assert!(w.published(1).is_some(), "output before the drop");
        let (t, _) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after_drop(
                1,
                Some(t1 + Duration::from_millis(100)),
                t1 + Duration::from_millis(300),
            ),
            || panic!(),
            t1 + Duration::from_millis(300),
        );
        assert_eq!(t, Transition::Left, "it drew after the drop");
        let mut w = InputWatches::default();
        let quiet = input_backlog::QUIET_STALL_AFTER.as_secs();
        let (t, _) = w.step(
            1,
            Some(&reading(1, quiet, 0)),
            false,
            Activity::default(),
            || None,
            t0,
        );
        let Transition::Entered(fact) = t else {
            panic!("{t:?}")
        };
        assert_eq!(fact.restart.leader, None);
        let (t, _) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after_drop(1, None, t1),
            || panic!(),
            t1,
        );
        assert_eq!(t, Transition::Left);
    }

    #[test]
    fn step_holds_a_flushed_stall_but_never_calls_it_survived() {
        let t0 = Instant::now();
        let base = t0.checked_sub(RECHECK).expect("uptime");
        let after = |discards: u64, restarts: u64, now: Instant| Activity {
            discards,
            restarts,
            ..spin(now.duration_since(base))
        };
        let t1 = t0 + Duration::from_secs(1);
        let grace = t1 + RESTART_GRACE;
        let mut w = InputWatches::default();
        primed(&mut w, 1, t0);
        let (t, _) = w.step(
            1,
            Some(&reading(1, 10, 0)),
            false,
            spin(RECHECK),
            || None,
            t0,
        );
        assert!(matches!(t, Transition::Entered(_)), "{t:?}");

        let (t, next) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after(1, 0, t1),
            || panic!("rss is read at entry only"),
            t1,
        );
        assert_eq!((t, next), (Transition::None, Some(t1 + RECHECK)));
        let held = w.published(1).expect("held: a drop is not a read").clone();
        assert!(!held.restart.survived);
        let (t, next) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after(1, 0, grace),
            || panic!(),
            grace,
        );
        assert_eq!((t, next), (Transition::None, Some(grace + RECHECK)));
        assert_eq!(w.published(1), Some(&held), "never survived");

        let t2 = grace + Duration::from_secs(1);
        let (t, next) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after(2, 1, t2),
            || panic!(),
            t2,
        );
        assert_eq!((t, next), (Transition::None, Some(t2 + RESTART_GRACE)));
        let almost = t2 + RESTART_GRACE - Duration::from_millis(1);
        let (t, _) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after(2, 1, almost),
            || panic!(),
            almost,
        );
        assert_eq!(t, Transition::None, "the grace counts from the signal");
        let at = t2 + RESTART_GRACE;
        let (t, _) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after(2, 1, at),
            || panic!(),
            at,
        );
        let Transition::Updated(fact) = t else {
            panic!("{t:?}")
        };
        assert!(fact.restart.survived);
        assert_eq!(fact.since, held.since, "one episode throughout");
    }

    #[test]
    fn step_lets_a_held_stall_go_when_its_program_reads_or_stops_spinning() {
        let t0 = Instant::now();
        let base = t0.checked_sub(RECHECK).expect("uptime");
        let after_drop = |burned: Duration, read: bool| Activity {
            discards: 1,
            restarts: 1,
            read_since_discard: read,
            ..spin(burned)
        };
        let core = |now: Instant| now.duration_since(base);
        let t1 = t0 + Duration::from_secs(1);
        let t2 = t1 + Duration::from_secs(2);
        let held = |w: &mut InputWatches| {
            primed(w, 1, t0);
            let (t, _) = w.step(
                1,
                Some(&reading(1, 10, 0)),
                false,
                spin(RECHECK),
                || None,
                t0,
            );
            assert!(matches!(t, Transition::Entered(_)), "{t:?}");
            let (t, _) = w.step(
                1,
                Some(&reading(0, 0, 0)),
                false,
                after_drop(core(t1), false),
                || panic!(),
                t1,
            );
            assert_eq!(t, Transition::None, "held");
        };

        let mut w = InputWatches::default();
        held(&mut w);
        let (t, next) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after_drop(core(t1), false),
            || panic!(),
            t2,
        );
        assert_eq!((t, next), (Transition::Left, None), "it stopped spinning");
        let mut w = InputWatches::default();
        held(&mut w);
        let (t, _) = w.step(
            1,
            Some(&reading(1, 1, 0)),
            false,
            after_drop(core(t1), false),
            || panic!(),
            t2,
        );
        assert_eq!(t, Transition::None, "asleep and not reading");
        assert!(w.published(1).is_some());

        let asleep = |discards: u64, read: bool| Activity {
            cpu: Some((4242, 0)),
            last_output: None,
            discards,
            restarts: discards,
            read_since_discard: read,
            contention: Contention::default(),
        };
        let quiet = input_backlog::QUIET_STALL_AFTER.as_secs();
        let mut w = InputWatches::default();
        let _ = w.step(
            1,
            Some(&reading(1, 0, 0)),
            false,
            asleep(0, false),
            || None,
            base,
        );
        let (t, _) = w.step(
            1,
            Some(&reading(1, quiet, 0)),
            false,
            asleep(0, false),
            || None,
            t0,
        );
        assert!(matches!(t, Transition::Entered(_)), "{t:?}");
        for at in [t1, t2] {
            let (t, _) = w.step(
                1,
                Some(&reading(0, 0, 0)),
                false,
                asleep(1, false),
                || panic!(),
                at,
            );
            assert_eq!(t, Transition::None, "a sleeper stays held");
        }
        let (t, _) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            asleep(1, true),
            || panic!(),
            t2 + Duration::from_secs(1),
        );
        assert_eq!(t, Transition::Left, "a sleeper that reads is let go");

        let mut w = InputWatches::default();
        held(&mut w);
        let t3 = t1 + Duration::from_secs(13);
        let (t, _) = w.step(
            1,
            Some(&reading(1, 12, 0)),
            false,
            after_drop(core(t3), true),
            || Some(9),
            t3,
        );
        let Transition::Entered(fact) = t else {
            panic!("{t:?}")
        };
        assert_eq!(fact.since, t3 - Duration::from_secs(12));
        assert_eq!(fact.rss_mb, Some(9));
        assert_eq!(
            fact.restart,
            Restart {
                leader: Some(4242),
                discards: 1,
                restarts: 1,
                survived: false,
            }
        );
    }

    #[test]
    fn step_enters_only_a_program_seen_spinning_and_silent() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(12);
        let asleep = |burned_ms: u64| Activity {
            cpu: Some((4242, burned_ms * 1_000_000)),
            last_output: None,
            discards: 0,
            restarts: 0,
            read_since_discard: false,
            contention: Contention::default(),
        };
        let probe = |first: Activity, second: Activity, wait_s: u64| {
            let mut w = InputWatches::default();
            let _ = w.step(1, Some(&reading(1, 0, 0)), false, first, || None, t0);
            let (t, next) = w.step(1, Some(&reading(1, wait_s, 0)), false, second, || None, t1);
            (t, next, w)
        };

        let (t, next, w) = probe(asleep(0), asleep(3), 12);
        assert_eq!(t, Transition::None, "asleep: not published");
        assert_eq!(next, Some(t1 + RECHECK), "still watched");
        assert!(w.published(1).is_none());
        let quiet = input_backlog::QUIET_STALL_AFTER.as_secs();
        let (t, _, _) = probe(asleep(0), asleep(3), quiet);
        assert!(matches!(t, Transition::Entered(_)), "{t:?}");

        let drawing = |cpu: Activity| Activity {
            last_output: Some(t1 - Duration::from_millis(100)),
            ..cpu
        };
        for cpu in [asleep(40), spin(Duration::from_secs(12))] {
            let (t, _, _) = probe(spin(Duration::ZERO), drawing(cpu), 3600);
            assert_eq!(t, Transition::None, "a program that draws is alive");
        }
        let stale = Activity {
            last_output: Some(t1 - Duration::from_secs(13)),
            ..spin(Duration::from_secs(12))
        };
        let (t, _, _) = probe(spin(Duration::ZERO), stale, 12);
        assert!(matches!(t, Transition::Entered(_)), "{t:?}");

        let (t, _, _) = probe(spin(Duration::ZERO), spin(Duration::from_secs(12)), 12);
        assert!(matches!(t, Transition::Entered(_)), "{t:?}");
    }

    #[test]
    fn spin_window_judges_half_a_core_over_a_second_or_more() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mark = Some((7, t0, 1_000));
        let at = |d: Duration| t0 + d;
        let ns = |d: Duration| 1_000 + u64::try_from(d.as_nanos()).expect("small");
        let idle = Contention::default();
        assert_eq!(
            spin_window(mark, false, Some((7, ns(ms(1000)))), idle, at(ms(2000))),
            (Some((7, at(ms(2000)), ns(ms(1000)))), true)
        );
        assert!(
            !spin_window(mark, true, Some((7, ns(ms(999)))), idle, at(ms(2000))).1,
            "999 ms of CPU across two seconds is under half a core"
        );
        assert_eq!(
            spin_window(mark, true, Some((7, ns(ms(0)))), idle, at(ms(999))),
            (mark, true)
        );
        assert_eq!(
            spin_window(mark, true, Some((8, 5)), idle, at(ms(5000))),
            (Some((8, at(ms(5000)), 5)), false)
        );
        assert_eq!(
            spin_window(mark, true, None, idle, at(ms(5000))),
            (None, false)
        );
        let loaded = Contention::of_load(190.0, 18);
        assert!(spin_window(mark, false, Some((7, ns(ms(200)))), loaded, at(ms(2000))).1);
        assert!(!spin_window(mark, true, Some((7, ns(ms(3)))), loaded, at(ms(2000))).1);
        assert!(!spin_window(mark, true, Some((7, ns(ms(200)))), idle, at(ms(2000))).1);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn pid_cpu_ns_reads_a_spinning_child_in_nanoseconds() {
        #[repr(C)]
        struct Rusage {
            utime: libc::timeval,
            stime: libc::timeval,
            _counters: [libc::c_long; 14],
        }
        const _: () = assert!(std::mem::size_of::<Rusage>() == 144);
        unsafe extern "C" {
            fn wait4(
                pid: libc::pid_t,
                status: *mut libc::c_int,
                options: libc::c_int,
                usage: *mut Rusage,
            ) -> libc::pid_t;
        }
        let micros = |t: libc::timeval| {
            u64::try_from(t.tv_sec).expect("non-negative") * 1_000_000
                + u64::try_from(t.tv_usec).expect("non-negative")
        };

        let child = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", "while :; do :; done"])
            .spawn()
            .expect("spawn a spinning shell");
        let pid = libc::pid_t::try_from(child.id()).expect("pid");
        let deadline = Instant::now() + Duration::from_secs(60);
        while pid_cpu_ns(pid).is_none_or(|ns| ns < 50_000_000) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        // SAFETY: `kill` takes no pointers; `pid` is this test's unreaped child.
        let stop = unsafe { libc::kill(pid, libc::SIGSTOP) };
        let mut stopped: libc::c_int = 0;
        // SAFETY: `stopped` is a valid out-parameter and `pid` is this test's unreaped child.
        let waited = unsafe { libc::waitpid(pid, &mut stopped, libc::WUNTRACED) };
        let read = pid_cpu_ns(pid);
        // SAFETY: as above; a stopped child is still this test's to kill.
        let kill = unsafe { libc::kill(pid, libc::SIGKILL) };
        let mut status: libc::c_int = 0;
        let mut usage = std::mem::MaybeUninit::<Rusage>::zeroed();
        // SAFETY: valid out-parameters for `wait4` reaping this test's child `pid`.
        let reaped = unsafe { wait4(pid, &mut status, 0, usage.as_mut_ptr()) };
        std::mem::forget(child);
        assert_eq!(
            (stop, waited, kill, reaped),
            (0, pid, 0, pid),
            "stop, wait for the stop, kill, reap"
        );
        assert!(
            libc::WIFSTOPPED(stopped),
            "read while stopped: {stopped:#x}"
        );
        let read = read.expect("readable while stopped");
        // SAFETY: `rusage` was zeroed and filled by the successful `wait4` call.
        let usage = unsafe { usage.assume_init() };
        let kernel = (micros(usage.utime) + micros(usage.stime)) * 1_000;
        assert!(kernel > 0, "the child ran");
        assert!(
            read <= kernel * 2 && read * 2 >= kernel,
            "pid_cpu_ns read {read} ns; the reap's rusage for the same stopped child is {kernel} ns"
        );
        assert_eq!(pid_cpu_ns(0), None);
    }

    #[test]
    fn status_fields_print_the_reading() {
        assert_eq!(
            status_fields(None, InputWord::Unmeasured, None),
            "input=- input_bytes=- input_wait_ms=- fg_rss_mb=-"
        );
        let stalled = InputBacklog {
            spilled: Some(2),
            ..reading(1, 161, 0)
        };
        assert_eq!(
            status_fields(Some(&stalled), InputWord::Stalled, Some(39_731)),
            "input=stalled input_bytes=3 input_wait_ms=161000 fg_rss_mb=39731"
        );
    }

    #[test]
    fn since_hhmm_is_the_local_clock_at_the_stall() {
        let since = 1_700_000_000 - 60;
        assert_eq!(since_hhmm(since, Some(0)).as_deref(), Some("22:12"));
        assert_eq!(since_hhmm(since, Some(-7 * 3600)).as_deref(), Some("15:12"));
        assert_eq!(since_hhmm(since, Some(2 * 3600)).as_deref(), Some("00:12"));
        assert_eq!(since_hhmm(since, None), None);
        let wall = || aterm_time::unix_secs().and_then(|s| i64::try_from(s).ok());
        let before = wall();
        let now = Instant::now();
        let stamped = since_unix_of(now.checked_sub(Duration::from_secs(90)).unwrap_or(now));
        let after = wall();
        let (Some(before), Some(stamped), Some(after)) = (before, stamped, after) else {
            panic!("this machine has a wall clock");
        };
        assert!(
            (before - 90..=after - 90).contains(&stamped),
            "{stamped} in {before}..={after}, 90 s back"
        );
    }

    #[test]
    fn every_reading_of_a_stall_names_the_clock_time_it_was_published_with() {
        let mut app = crate::App::headless_for_test();
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        let now = Instant::now();
        let fact = stalled(now.checked_sub(Duration::from_secs(30)).unwrap_or(now));
        let attention = |ctx: &SessionCtx| {
            ctx.meta
                .lock()
                .unwrap()
                .attention
                .clone()
                .expect("attention")
        };
        app.session_status
            .inputs
            .publish_for_test(0, fact.clone(), now);
        app.publish_input_stall(0, &ctx, Some(fact), now);
        let first = attention(&ctx);
        assert!(first.contains(" since "), "a clock time: {first}");
        let later = now + Duration::from_secs(61);
        let standing = app.session_status.inputs.published(0).cloned();
        app.publish_input_stall(0, &ctx, standing, later);
        assert_eq!(attention(&ctx), first, "the republication");
        let row = {
            let tl = ctx.timeline.lock().unwrap();
            let fact = tl.agent().input.clone().expect("the timeline's stall");
            menu_row(
                &fact,
                tl.agent().program.as_deref(),
                &after_restart(&tl, fact.act.as_ref()),
                ctx.self_id.as_str(),
                later,
            )
        };
        assert_eq!(row.text, first, "the menu row");
    }

    const OWN: &str = "claude --model opus --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";

    fn own() -> AfterRestart {
        AfterRestart::Command(OWN.to_string())
    }

    #[test]
    fn attention_text_keeps_the_whole_remedy_under_the_keyed_cap() {
        let fact = InputStallFact {
            rss_mb: Some(39_731),
            ..stalled(Instant::now())
        };
        let survived = InputStallFact {
            restart: Restart {
                leader: Some(69_156),
                survived: true,
                ..Restart::default()
            },
            ..fact.clone()
        };
        let plain = InputStallFact {
            rss_mb: None,
            ..fact.clone()
        };
        let stopped = InputStallFact {
            word: InputWord::Stopped,
            stopped: true,
            ..fact.clone()
        };
        let sid = "s-b7cf523445a1b0d8658e";
        let bare = "claude --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
        for (why, fact, clock, tail) in [
            (
                "flags dropped",
                &fact,
                Some("14:02"),
                format!("@{sid} signal term, then {bare}"),
            ),
            (
                "rss dropped",
                &survived,
                Some("14:02"),
                format!("@{sid} signal kill, then {bare}"),
            ),
            (
                "all fits",
                &plain,
                None,
                format!("@{sid} signal term, then {OWN}"),
            ),
            (
                "stopped",
                &stopped,
                Some("14:02"),
                format!("resume it: aterm ctl @{sid} signal cont"),
            ),
        ] {
            let line = attention_text(
                fact,
                Some("claude"),
                &own(),
                sid,
                clock,
                Duration::from_secs(9660),
            );
            assert!(line.ends_with(&tail), "{why}: {line}");
            assert!(
                line.len() <= crate::session_timeline::META_ATTENTION_KEYED_MAX,
                "{why}: {line}"
            );
        }
    }

    fn hosted_claude(sid: &str) -> aterm_agent::harness::relaunch::Snapshot {
        let snap = aterm_agent::harness::relaunch::Snapshot {
            stored_login: false,
            profile: None,
            tab: sid.to_string(),
            pid: 7,
            start: "Sat Sep 27 01:02:03 2026".to_string(),
            shell: 6,
            program: std::path::PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".to_string()),
            cwd: "/".to_string(),
            version: None,
            run: aterm_agent::harness::relaunch::AgentRun::Claude,
            dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
            login: None,
            home: None,
            claude_dir: None,
        };
        assert!(aterm_agent::harness::relaunch::resumes_on_exit(
            std::path::Path::new("/Users//someone"),
            &snap
        ));
        snap
    }

    #[test]
    fn the_publication_names_one_remedy_wherever_it_is_read() {
        let mut app = crate::App::headless_for_test();
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        let sid = ctx.self_id.as_str().to_string();
        claude_in_front(&ctx, 7, "prompt");
        {
            let mut tl = ctx.timeline.lock().unwrap();
            assert!(tl.set_claude_footer(
                7,
                None,
                Some(aterm_agent::harness::footer::FooterFacts {
                    resume: Some(OWN.to_string()),
                    ..Default::default()
                }),
            ));
        }
        let snapshot = hosted_claude(&sid);
        crate::harness_host::publish_snapshot_for_test(&sid, Some(&snapshot));
        let fact = |leader: Option<i32>, survived: bool| InputStallFact {
            restart: Restart {
                leader,
                survived,
                ..Restart::default()
            },
            ..stalled(Instant::now())
        };
        let read = |app: &mut crate::App, fact: InputStallFact| {
            let now = Instant::now();
            app.session_status
                .inputs
                .publish_for_test(0, fact.clone(), now);
            app.publish_input_stall(0, &ctx, Some(fact), now);
            let attention = ctx
                .meta
                .lock()
                .unwrap()
                .attention
                .clone()
                .expect("attention");
            let row = {
                let handle = app.store.read().unwrap().by_local(0).cloned();
                app.need_facts_with(
                    handle.as_ref().expect("session 0"),
                    |_| true,
                    &Default::default(),
                    crate::metrics::now_us(),
                    0,
                )
                .stall
                .expect("the stall row")
                .text
            };
            let mail = crate::harness_host::offered_now(&app.store, &sid);
            (attention, row, mail)
        };
        let offered = |app: &mut crate::App, leader: Option<i32>, (pid, promised), why: &str| {
            let act = format!("aterm ctl @{sid} restart pid={pid} if-stalled");
            let tail = if promised {
                format!("; {RELAUNCH_WORDS}")
            } else {
                String::new()
            };
            let (attention, row, mail) = read(app, fact(leader, false));
            assert!(
                attention.ends_with(&format!("restart it: {act}{tail}")),
                "{why}: {attention}"
            );
            assert_eq!(row, attention, "{why}: the row is the entry's words");
            assert_eq!(mail, Some(promised), "{why}: the loop names the act too");
            let (attention, row, mail) = read(app, fact(leader, true));
            assert!(
                attention.ends_with(&format!("restart it: {act}{tail}"))
                    && !attention.contains("kill"),
                "{why}: {attention}"
            );
            assert_eq!(row, attention, "{why}");
            assert_eq!(mail, Some(promised), "{why}");
        };
        let carried = |app: &mut crate::App, why: &str| offered(app, Some(7), (7, true), why);
        let refused_then = |app: &mut crate::App, leader: Option<i32>, after: &str, why: &str| {
            let (attention, row, mail) = read(app, fact(leader, false));
            assert!(
                attention.ends_with(&format!("signal term{after}")),
                "{why}: {attention}"
            );
            assert_eq!(row, attention, "{why}");
            assert_eq!(mail, None, "{why}: the loop names no act");
            let (attention, row, _) = read(app, fact(leader, true));
            assert!(
                attention.contains("end it: aterm ctl @") && attention.contains("signal kill"),
                "{why}: {attention}"
            );
            assert_eq!(row, attention, "{why}");
        };
        let own_line = format!(", then {OWN}");
        let refused = |app: &mut crate::App, leader: i32, why: &str| {
            refused_then(app, Some(leader), &own_line, why);
        };
        let now_us = crate::metrics::now_us();
        let claim = |holder: &str| {
            ctx.meta.lock().unwrap().supervisor = None;
            crate::session_timeline::claim_supervisor(&ctx, holder, None, None, now_us)
                .expect("claimed");
        };

        refused(&mut app, 7, "no claim");
        claim(&crate::harness_host::holder());
        carried(&mut app, "the host's claim, unheld");
        refused(&mut app, 8, "a stall over another leader");
        offered(&mut app, None, (7, true), "no leader read");

        assert!(crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "-".to_string(),
                origin: "local".to_string(),
            }),
        ));
        refused(&mut app, 7, "hold=1");
        assert!(crate::fabric::apply_hold_for_test(&ctx, None));

        let lease = |l: Option<crate::Lease>| *ctx.turn_lease.lock().unwrap() = l;
        lease(Some(crate::Lease::Drive {
            holder: "orchestrator".to_string(),
            expires_us: now_us + 60_000_000,
            conn: None,
            hard: false,
            here: false,
            holder_pid: None,
        }));
        refused(&mut app, 7, "a live drive lease");
        lease(Some(crate::Lease::Turn {
            id: 41,
            driver: Some(aterm_session::SessionId::generate()),
            typing: true,
        }));
        refused(&mut app, 7, "a named driver's turn");
        lease(Some(crate::Lease::Turn {
            id: 42,
            driver: None,
            typing: true,
        }));
        carried(&mut app, "an owner turn");
        lease(None);

        crate::harness_host::publish_snapshot_for_test(&sid, None);
        refused(&mut app, 7, "no snapshot");
        crate::harness_host::publish_snapshot_for_test(&sid, Some(&snapshot));

        {
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(9);
            tl.set_program(9, Some("vim".into()));
        }
        refused_then(&mut app, None, "", "vim in front");

        {
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(11);
            tl.set_program(11, Some("codex".into()));
        }
        let codex = |session: Option<&str>| aterm_agent::harness::relaunch::Snapshot {
            pid: 11,
            program: std::path::PathBuf::from("/opt/codex/bin/codex"),
            argv: vec!["/opt/codex/bin/codex".to_string()],
            session: session.map(str::to_string),
            dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
            run: aterm_agent::harness::relaunch::AgentRun::Codex(
                aterm_agent::harness::relaunch::CodexRun {
                    home: std::path::PathBuf::from("/h/.codex"),
                    home_env: None,
                    self_claimed: None,
                },
            ),
            ..snapshot.clone()
        };
        let named = Some("019a0b6f-3c1e-7a4d-8b61-9d527f1e2c3a");
        crate::harness_host::publish_snapshot_for_test(&sid, Some(&codex(named)));
        refused_then(&mut app, Some(11), "", "a Codex's named thread, no copy");
        crate::harness_host::publish_snapshot_for_test(&sid, Some(&codex(None)));
        refused_then(&mut app, Some(11), "", "a Codex's unnamed thread");
        for (snap, why) in [
            (
                aterm_agent::harness::relaunch::Snapshot {
                    dialect: None,
                    ..codex(named)
                },
                "a Codex in a shell no line is written for",
            ),
            (
                aterm_agent::harness::relaunch::Snapshot {
                    argv: vec!["/opt/codex/bin/codex".to_string(), "--worktree".to_string()],
                    ..codex(named)
                },
                "a Codex launched with a flag its resume cannot carry",
            ),
        ] {
            crate::harness_host::publish_snapshot_for_test(&sid, Some(&snap));
            refused_then(&mut app, Some(11), "", why);
        }
        crate::harness_host::publish_snapshot_for_test(&sid, None);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_standing_stall_follows_the_one_predicate_at_each_probe() {
        let (master, slave) = raw_pty_pair();
        let sink = std::sync::Arc::new(SinkWriter::new(master));
        sink.note_master_nonblocking(true);
        let mut app = crate::App::headless_for_test();
        app.pool
            .insert(crate::stub_session_with_sink(77, sink.clone()));
        let ctx = app.pool.get(77).expect("session 77").ctx.clone();
        let sid = ctx.self_id.as_str().to_string();
        {
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_program(7, Some("claude".into()));
        }
        sink.write_frame_nonparking(b"\r")
            .expect("the human's Enter");
        sink.backdate_input_for_testing(input_backlog::STALL_AFTER + Duration::from_secs(2));
        let t0 = Instant::now();
        let fact = stalled(t0);
        app.session_status
            .inputs
            .publish_for_test(77, fact.clone(), t0);
        app.publish_input_stall(77, &ctx, Some(fact), t0);
        let entry = || {
            ctx.meta
                .lock()
                .unwrap()
                .attention
                .clone()
                .expect("attention")
        };
        let term = format!("restart it: aterm ctl @{sid} signal term");
        let act = format!("aterm ctl @{sid} restart pid=7 if-stalled");
        assert!(entry().ends_with(&term), "{}", entry());
        let mut at = t0;
        let mut probe = |app: &mut crate::App| {
            at += PROBE_MIN_GAP;
            app.observe_input_stalls(at, Some(77))
        };
        assert!(probe(&mut app).is_empty(), "nothing moved");
        assert!(entry().ends_with(&term), "{}", entry());

        crate::harness_host::publish_snapshot_for_test(&sid, Some(&hosted_claude(&sid)));
        ctx.meta.lock().unwrap().supervisor = None;
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claimed");
        assert_eq!(probe(&mut app), [77], "the act moved: republished");
        assert!(
            entry().ends_with(&format!("restart it: {act}; {RELAUNCH_WORDS}")),
            "{}",
            entry()
        );
        assert!(probe(&mut app).is_empty(), "nothing moved since");
        crate::harness_host::publish_snapshot_for_test(&sid, None);
        assert_eq!(slave_read_all(slave), b"\r");
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    fn the_memory_row_promises_the_restart_only_on_the_hosts_word() {
        let app = crate::App::headless_for_test();
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        {
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_program(7, Some("claude".into()));
            tl.publish_agent(
                "wall:memory",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
            assert!(tl.set_claude_footer(
                7,
                None,
                Some(aterm_agent::harness::footer::FooterFacts {
                    resume: Some(OWN.to_string()),
                    ..Default::default()
                }),
            ));
        }
        let now_us = crate::metrics::now_us();
        let relaunching: std::collections::HashSet<String> =
            std::iter::once(ctx.self_id.as_str().to_string()).collect();
        let restarts = |relaunching: &std::collections::HashSet<String>| {
            let handle = app.store.read().unwrap().by_local(0).cloned();
            let facts = app.need_facts_with(
                handle.as_ref().expect("session 0"),
                |_| true,
                relaunching,
                now_us,
                0,
            );
            assert!(facts.supervised, "the host supervises it: {facts:?}");
            let verdict = crate::needs::of(&facts);
            match (verdict.need, verdict.waiting) {
                (Some(need), _) => {
                    assert_eq!(need.word, aterm_types::needs::NeedWord::Memory);
                    assert!(need.what.contains(OWN), "the tab's own resume");
                    false
                }
                (None, Some(wait)) => {
                    assert_eq!(wait.word, aterm_types::needs::WaitWord::MemoryRestart);
                    true
                }
                (None, None) => panic!("the memory wall is something"),
            }
        };
        let claim = |holder: &str| {
            ctx.meta.lock().unwrap().supervisor = None;
            crate::session_timeline::claim_supervisor(&ctx, holder, None, None, now_us)
                .expect("claimed");
        };

        claim(&crate::harness_host::holder());
        assert!(!restarts(&Default::default()), "relaunch off");
        ctx.meta.lock().unwrap().supervisor = None;
        assert!(!restarts(&relaunching), "no claim");
        claim("manager@4242");
        assert!(!restarts(&relaunching), "another holder");
        claim(&crate::harness_host::holder());
        assert!(restarts(&relaunching), "the host restarts it");
    }

    #[test]
    fn the_menu_row_is_keyed_by_episode() {
        let now = Instant::now();
        let fact = InputStallFact {
            rss_mb: Some(39_731),
            ..stalled(now.checked_sub(Duration::from_secs(95)).unwrap_or(now))
        };
        let sid = "s-b7cf523445a1b0d8658e";
        let row = menu_row(&fact, Some("claude"), &own(), sid, now);
        let later = menu_row(
            &InputStallFact {
                bytes: 9,
                word: InputWord::Stopped,
                stopped: true,
                ..fact.clone()
            },
            Some("claude"),
            &own(),
            sid,
            now + Duration::from_secs(60),
        );
        assert_ne!(later.text, row.text);
        assert_eq!(later.key, row.key);
        let next = InputStallFact { since: now, ..fact };
        assert_ne!(episode_key(&next), row.key);
    }

    #[test]
    fn retiring_a_watch_unpublishes_its_stall() {
        let mut app = crate::App::headless_for_test();
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        claude_in_front(&ctx, 7, "prompt");
        let now = Instant::now();
        let fact = stalled(now);
        app.session_status
            .inputs
            .publish_for_test(0, fact.clone(), now);
        app.publish_input_stall(0, &ctx, Some(fact), now);
        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" agent=wall:unresponsive "), "{record}");
        assert!(ctx.meta.lock().unwrap().attention.is_some());

        app.retire_input_watch(0);
        assert!(!app.session_status.input_armed(0));
        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" agent=prompt "), "{record}");
        assert_eq!(ctx.meta.lock().unwrap().attention, None);
    }

    #[cfg(target_os = "macos")]
    fn stretched_around<T>(probe: impl FnOnce() -> T) -> (T, Duration, Duration) {
        let before = machine_contention().stretch(input_backlog::STALL_AFTER);
        let out = probe();
        let after = machine_contention().stretch(input_backlog::STALL_AFTER);
        (out, before.min(after), before.max(after))
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_watch_arms_on_unread_input_and_ends_when_it_is_read() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (master, slave) = raw_pty_pair();
        let sink = std::sync::Arc::new(SinkWriter::new(master));
        sink.note_master_nonblocking(true);
        let hooks = std::sync::Arc::new(AtomicUsize::new(0));
        {
            let hooks = hooks.clone();
            sink.install_input_hook(move || {
                hooks.fetch_add(1, Ordering::SeqCst);
            });
        }
        let mut app = crate::App::headless_for_test();
        app.pool
            .insert(crate::stub_session_with_sink(77, sink.clone()));

        assert!(
            app.observe_input_stalls(Instant::now(), Some(77))
                .is_empty()
        );
        assert!(!app.session_status.input_armed(77), "idle: nothing armed");
        assert_eq!(app.session_status.next_input_wake(), None);

        sink.write_frame_nonparking(b"\r")
            .expect("the human's Enter");
        sink.write_frame_nonparking(b"a").expect("a second key");
        assert_eq!(hooks.load(Ordering::SeqCst), 1, "one wake per episode");
        let t0 = Instant::now();
        let (moved, lo, hi) = stretched_around(|| app.observe_input_stalls(t0, Some(77)));
        assert!(moved.is_empty());
        assert!(app.session_status.input_armed(77));
        let wake = app.session_status.next_input_wake().expect("armed");
        assert!(
            lo >= input_backlog::STALL_AFTER
                && wake + Duration::from_secs(1) > t0 + lo
                && wake <= t0 + hi,
            "the stall instant, not a poll: {:?} in [{lo:?}, {hi:?}]",
            wake - t0
        );
        assert_eq!(app.session_status.input_stall(77), None, "pending");
        let record = app.session_status_record(77).expect("live session");
        assert!(
            record.contains(" input=pending input_bytes=2 input_wait_ms="),
            "{record}"
        );
        sink.write_frame_nonparking(b"b").expect("a third key");
        assert_eq!(hooks.load(Ordering::SeqCst), 2);
        assert!(
            app.observe_input_stalls(t0 + Duration::from_millis(100), Some(77))
                .is_empty()
        );
        assert_eq!(
            app.session_status.next_input_wake(),
            Some(t0 + PROBE_MIN_GAP)
        );
        assert_eq!(app.session_status.inputs.due(t0 + PROBE_MIN_GAP), [77]);

        assert_eq!(slave_read_all(slave), b"\rab");
        assert!(
            app.observe_input_stalls(t0 + PROBE_MIN_GAP, Some(77))
                .is_empty()
        );
        assert!(!app.session_status.input_armed(77), "read: the watch ends");
        assert_eq!(app.session_status.next_input_wake(), None);
        let record = app.session_status_record(77).expect("live session");
        assert!(
            record.contains(" input=clear input_bytes=0 input_wait_ms=0 fg_rss_mb=- disk="),
            "{record}"
        );
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_terminals_own_answer_starts_no_watch() {
        let (master, slave) = raw_pty_pair();
        let sink = std::sync::Arc::new(SinkWriter::new(master));
        sink.note_master_nonblocking(true);
        let mut app = crate::App::headless_for_test();
        app.pool
            .insert(crate::stub_session_with_sink(77, sink.clone()));
        let answer = b"\x1b]11;rgb:0000/0000/0000\x1b\\";

        sink.write_reply(answer).expect("the answer");
        let t0 = Instant::now();
        assert!(app.observe_input_stalls(t0, Some(77)).is_empty());
        assert!(!app.session_status.input_armed(77), "an answer: no watch");
        assert_eq!(app.session_status.next_input_wake(), None, "nothing armed");
        assert_eq!(slave_read_all(slave), answer, "the program reads it");

        sink.write_reply(answer).expect("the next answer");
        sink.write_frame_nonparking(b"k").expect("a key");
        let t1 = t0 + PROBE_MIN_GAP;
        let _ = app.observe_input_stalls(t1, Some(77));
        assert!(app.session_status.input_armed(77), "a key: watched");
        assert!(app.session_status.next_input_wake().is_some());
        let mut both = answer.to_vec();
        both.push(b'k');
        assert_eq!(slave_read_all(slave), both);
        let _ = app.observe_input_stalls(t1 + PROBE_MIN_GAP, Some(77));
        assert!(!app.session_status.input_armed(77), "read: the watch ends");
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_key_after_answers_let_pass_still_wakes_the_watch() {
        let (master, slave) = raw_pty_pair();
        let sink = std::sync::Arc::new(SinkWriter::new(master));
        sink.note_master_nonblocking(true);
        let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&wakes);
        sink.install_input_hook(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let woke = || wakes.load(std::sync::atomic::Ordering::SeqCst);
        let mut app = crate::App::headless_for_test();
        app.pool
            .insert(crate::stub_session_with_sink(77, sink.clone()));
        let answer = b"\x1b]11;rgb:0000/0000/0000\x1b\\";
        let t0 = Instant::now();
        for n in 1..=3 {
            sink.write_reply(answer).expect("an answer");
            assert_eq!(woke(), n, "answer {n} wakes the watch");
            assert!(app.observe_input_stalls(t0, Some(77)).is_empty());
            assert!(!app.session_status.input_armed(77), "answer {n}: passed");
            assert_eq!(slave_read_all(slave), answer);
        }
        sink.write_frame_nonparking(b"k").expect("a key");
        assert_eq!(woke(), 4, "the person's key wakes the watch");
        let _ = app.observe_input_stalls(t0, Some(77));
        assert!(
            app.session_status.input_armed(77),
            "the key unread: watched"
        );
        assert_eq!(slave_read_all(slave), b"k");
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn an_unseen_writers_byte_status_woke_is_watched() {
        let (master, slave) = raw_pty_pair();
        let sink = std::sync::Arc::new(SinkWriter::new(master));
        sink.note_master_nonblocking(true);
        let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&wakes);
        sink.install_input_hook(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let mut app = crate::App::headless_for_test();
        app.pool
            .insert(crate::stub_session_with_sink(77, sink.clone()));
        sink.write_frame_nonparking(b"k").expect("a key");
        let t0 = Instant::now();
        let _ = app.observe_input_stalls(t0, Some(77));
        assert!(
            app.session_status.input_armed(77),
            "the key unread: watched"
        );
        assert_eq!(slave_read_all(slave), b"k");
        let _ = app.observe_input_stalls(t0 + PROBE_MIN_GAP, Some(77));
        assert!(!app.session_status.input_armed(77), "read: the watch ends");

        let before = wakes.load(std::sync::atomic::Ordering::SeqCst);
        // SAFETY: `master` is an open pty master and the buffer is live.
        let n = unsafe { libc::write(master, b"\x1b[B".as_ptr().cast(), 3) };
        assert_eq!(n, 3, "the unseen writer's byte");
        let record = app.session_status_record(77).expect("live session");
        assert!(record.contains(" input=pending "), "{record}");
        assert_eq!(
            wakes.load(std::sync::atomic::Ordering::SeqCst),
            before + 1,
            "status woke the unwatched session"
        );
        let _ = app.observe_input_stalls(t0 + 2 * PROBE_MIN_GAP, Some(77));
        assert!(
            app.session_status.input_armed(77),
            "the unseen writer's unread byte is watched"
        );
        assert_eq!(slave_read_all(slave), b"\x1b[B");
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_key_that_waited_for_the_lock_across_a_look_is_watched_when_it_lands() {
        let (master, slave) = raw_pty_pair();
        let sink = std::sync::Arc::new(SinkWriter::new(master));
        sink.note_master_nonblocking(true);
        let (stood, at_lock) = std::sync::mpsc::channel::<()>();
        let (go, gate) = std::sync::mpsc::channel::<()>();
        let stood = std::sync::Mutex::new(stood);
        let gate = std::sync::Mutex::new(gate);
        sink.seam_for_testing(move |seam| {
            if seam == aterm_session::sink::Seam::FrameLock {
                let _ = stood.lock().map(|s| s.send(()));
                let _ = gate.lock().map(|g| g.recv());
            }
        });
        let mut app = crate::App::headless_for_test();
        app.pool
            .insert(crate::stub_session_with_sink(77, sink.clone()));
        let driven = sink.driven_input();
        let key = {
            let sink = std::sync::Arc::clone(&sink);
            std::thread::spawn(move || sink.write_frame(b"k"))
        };
        at_lock
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the key stands before the fd lock");
        assert_ne!(sink.driven_input(), driven, "the key is reserved");

        let answer = b"\x1b]11;rgb:0000/0000/0000\x1b\\";
        sink.write_reply(answer).expect("the answer");
        let t0 = Instant::now();
        assert!(app.observe_input_stalls(t0, Some(77)).is_empty());
        assert!(!app.session_status.input_armed(77), "the answer: no watch");
        assert_eq!(slave_read_all(slave), answer, "the program reads it");

        go.send(()).expect("the key goes on");
        let landed = key.join().expect("the key's writer");
        assert_eq!(landed.expect("the key lands"), 1);
        let _ = app.observe_input_stalls(t0 + PROBE_MIN_GAP, Some(77));
        assert!(
            app.session_status.input_armed(77),
            "the key unread: watched"
        );
        assert_eq!(slave_read_all(slave), b"k");
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_master_adopted_with_input_unread_is_watched_from_its_attach() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let wakes = Arc::new(AtomicUsize::new(0));
        let wake = || {
            let wakes = wakes.clone();
            move || {
                wakes.fetch_add(1, Ordering::SeqCst);
            }
        };
        let count = || wakes.load(Ordering::SeqCst);
        let unseen = |master: i32, bytes: &[u8]| {
            // SAFETY: `master` is an open PTY master fd and `bytes` is a valid buffer.
            let n = unsafe { libc::write(master, bytes.as_ptr().cast(), bytes.len()) };
            assert_eq!(usize::try_from(n).ok(), Some(bytes.len()), "write(master)");
        };

        let (master, slave) = raw_pty_pair();
        unseen(master, b"\r");
        let born_before = Instant::now();
        let sink = Arc::new(SinkWriter::new(master));
        let born_after = Instant::now();
        assert!(watch_input(&sink, wake()), "the attach wakes the watch");
        assert_eq!(count(), 1);
        let mut app = crate::App::headless_for_test();
        app.pool
            .insert(crate::stub_session_with_sink(77, sink.clone()));
        let record = app.session_status_record(77).expect("live session");
        assert!(record.contains(" input=pending input_bytes=1 "), "{record}");
        assert_eq!(count(), 1, "the attach's wake is in flight: not doubled");

        std::thread::sleep(Duration::from_millis(300));
        let t0 = Instant::now();
        let (moved, lo, hi) = stretched_around(|| app.observe_input_stalls(t0, Some(77)));
        assert!(moved.is_empty());
        let deadline = app.session_status.next_input_wake().expect("armed");
        assert!(
            deadline <= born_after + hi
                && deadline + Duration::from_millis(100) >= born_before + lo,
            "the stall instant of a byte dated from the sink's birth: {:?} after it, \
             in [{lo:?}, {hi:?}]",
            deadline.saturating_duration_since(born_before)
        );
        let _ = app.session_status_record(77).expect("live session");
        assert_eq!(count(), 1, "watched: a status poll wakes nothing");

        assert_eq!(slave_read_all(slave), b"\r");
        assert!(
            app.observe_input_stalls(t0 + PROBE_MIN_GAP, Some(77))
                .is_empty()
        );
        assert!(!app.session_status.input_armed(77));
        unseen(master, b"\x1b[B");
        assert_eq!(count(), 1, "an unseen write fires no hook");
        let record = app.session_status_record(77).expect("live session");
        assert!(record.contains(" input=pending input_bytes=3 "), "{record}");
        assert_eq!(count(), 2, "status woke the unwatched session");
        let _ = app.session_status_record(77).expect("live session");
        assert_eq!(count(), 2);
        assert_eq!(slave_read_all(slave), b"\x1b[B");
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn an_adopted_backlog_whose_attach_wake_is_dropped_before_commit_is_watched_at_commit() {
        use std::sync::{Arc, Mutex};
        let posted: Arc<Mutex<Vec<crate::Wake>>> = Arc::new(Mutex::new(Vec::new()));
        let proxy = |session: u64| {
            let posted = posted.clone();
            move || {
                posted
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(crate::Wake::InputWatch { session });
            }
        };
        let take = || std::mem::take(&mut *posted.lock().unwrap_or_else(|p| p.into_inner()));

        let (master, slave) = raw_pty_pair();
        // SAFETY: `master` is an open pty master and the buffer is one live byte.
        let n = unsafe { libc::write(master, b"\r".as_ptr().cast(), 1) };
        assert_eq!(n, 1, "write(master)");
        let sink = Arc::new(SinkWriter::new(master));
        let (idle_master, idle_slave) = raw_pty_pair();
        let idle = Arc::new(SinkWriter::new(idle_master));

        let mut app = crate::App::headless_for_test();
        for (id, sink) in [(77, sink.clone()), (78, idle.clone())] {
            let mut session = crate::stub_session_with_sink(id, sink);
            session.handoff_local_id = Some(id - 70);
            app.pool.insert(session);
        }
        app.incoming_handoff_pending = true;

        assert!(watch_input(&sink, proxy(77)));
        assert!(!watch_input(&idle, proxy(78)), "nothing unread: no wake");
        let wakes = take();
        assert_eq!(wakes.len(), 1);

        assert!(app.drops_wake_before_commit(&crate::Wake::MetaChanged { session: 77 }));
        assert!(
            !sink.wake_input_hook(),
            "still fired: the attach's wake is in flight"
        );

        assert!(
            app.drops_wake_before_commit(&wakes[0]),
            "pending: swallowed"
        );
        assert!(!app.session_status.input_armed(77), "no probe ran");
        assert!(
            sink.wake_input_hook(),
            "the dropped wake released the hook: a later wake can run"
        );
        assert_eq!(take().len(), 1);

        app.incoming_handoff_pending = false;
        assert!(
            !app.drops_wake_before_commit(&wakes[0]),
            "committed: delivered"
        );
        let _ = app.rewatch_adopted_input(Instant::now());
        assert!(app.session_status.input_armed(77), "watched from Commit");
        assert!(
            app.session_status.next_input_wake().is_some(),
            "a deadline owed"
        );
        assert!(
            !app.session_status.input_armed(78),
            "an idle adoption arms nothing"
        );
        sink.write_frame_nonparking(b"a").expect("a replayed key");
        assert_eq!(take().len(), 1, "the rearmed hook fired");

        assert_eq!(slave_read_all(slave), b"\ra");
        for fd in [master, slave, idle_master, idle_slave] {
            aterm_pty::close_fd(fd);
        }
    }

    #[cfg(target_os = "macos")]
    fn spinning_job() -> (i32, i32, ReapJob, i32) {
        let (master, slave, job, leader) = pty_job("trap '' TERM; printf R; while :; do :; done");
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut ready = [0u8; 1];
        loop {
            let mut fds = libc::pollfd {
                fd: master,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd for the open master, for 100 ms.
            if unsafe { libc::poll(&mut fds, 1, 100) } == 1 {
                // SAFETY: `master` is open and `ready` is a 1-byte buffer.
                let n = unsafe { libc::read(master, ready.as_mut_ptr().cast(), 1) };
                if n == 1 {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "the job never set its trap");
        }
        assert_eq!(&ready, b"R", "the job's first byte is its readiness mark");
        (master, slave, job, leader)
    }

    #[cfg(target_os = "macos")]
    fn pty_job(script: &str) -> (i32, i32, ReapJob, i32) {
        use aterm_uds::spawnfd::InheritNothing as _;
        use std::os::fd::BorrowedFd;
        use std::os::unix::process::CommandExt;
        let (master, slave) = raw_pty_pair();
        // SAFETY: `slave` is an open PTY slave fd owned by this test.
        let slave_fd = unsafe { BorrowedFd::borrow_raw(slave) };
        let stdio = || std::process::Stdio::from(slave_fd.try_clone_to_owned().expect("dup slave"));
        let mut job = std::process::Command::new("/bin/sh");
        job.inherit_nothing()
            .args(["-c", script])
            .stdin(stdio())
            .stdout(stdio())
            .stderr(stdio());
        // SAFETY: pre-exec child closure calls only async-signal-safe `setsid` and `ioctl(TIOCSCTTY)`.
        unsafe {
            job.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let job = ReapJob(job.spawn().expect("spawn the job"));
        let leader = libc::pid_t::try_from(job.0.id()).expect("pid");
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
        while unsafe { libc::tcgetpgrp(master) } != leader {
            assert!(Instant::now() < deadline, "the job never led the tty");
            std::thread::sleep(Duration::from_millis(5));
        }
        (master, slave, job, leader)
    }

    #[cfg(target_os = "macos")]
    struct ReapJob(std::process::Child);

    #[cfg(target_os = "macos")]
    impl Drop for ReapJob {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_stall_whose_program_lives_through_signal_term_stands_until_signal_kill() {
        for flush_first in [false, true] {
            let (master, slave, mut job, leader) = spinning_job();
            let sink = std::sync::Arc::new(SinkWriter::new(master));
            sink.note_master_nonblocking(true);
            let mut app = crate::App::headless_for_test();
            app.pool
                .insert(crate::stub_session_with_sink(77, sink.clone()));
            let ctx = app.pool.get(77).expect("session 77").ctx.clone();
            claude_in_front(&ctx, leader, "busy");
            let attention = || ctx.meta.lock().unwrap().attention.clone();
            let record = |app: &crate::App| app.session_status_record(77).expect("live session");

            sink.write_frame_nonparking(b"\r").expect("the Enter");
            let t0 = Instant::now();
            let on_leader = |ns: u64| Activity {
                cpu: Some((leader, ns)),
                last_output: None,
                restarts: sink.restart_discards(),
                discards: sink.discards(),
                read_since_discard: sink.read_since_discard(),
                contention: Contention::default(),
            };
            let inputs = &mut app.session_status.inputs;
            let _ = inputs.step(
                77,
                Some(&reading(1, 0, 0)),
                false,
                on_leader(0),
                || None,
                t0.checked_sub(RECHECK).expect("uptime"),
            );
            let nanos = u64::try_from(RECHECK.as_nanos()).expect("small");
            let (t, _) = inputs.step(
                77,
                Some(&reading(1, 12, 0)),
                false,
                on_leader(nanos),
                || Some(2),
                t0,
            );
            let Transition::Entered(fact) = t else {
                panic!("entered: {t:?}")
            };
            assert_eq!(fact.restart.leader, Some(leader));
            app.publish_input_stall(77, &ctx, Some(fact), t0);
            assert!(record(&app).contains(" agent=wall:unresponsive "));

            if flush_first {
                let term = app.pool.get(77).expect("session 77").term.clone();
                let reply = crate::manual_reset::cmd_reset(77, &term, &ctx, "flush");
                assert!(reply.ends_with(" discarded=1\n"), "{reply}");
                assert_eq!(
                    (sink.discards(), sink.restart_discards()),
                    (1, 0),
                    "a flush is a drop, not a restart"
                );
                let refused = refusal(&ctx, "send", "x", false).expect("held by the flush");
                assert!(
                    refused.contains("reset flush") && !refused.contains("restart signal"),
                    "{refused}"
                );
            }
            let reply = crate::control::cmd_signal(master, "term", &sink, None);
            let dropped = if flush_first { "" } else { " discarded=1" };
            assert_eq!(reply, format!("OK signalled pgrp {leader}{dropped}\n"));
            assert_eq!(sink.restart_discards(), 1, "the restart is marked");
            std::thread::sleep(Duration::from_millis(300));

            let t1 = Instant::now();
            assert!(
                app.observe_input_stalls(t1, Some(77)).is_empty(),
                "the empty queue is a drop, not a read: nothing moves"
            );
            let status = record(&app);
            assert!(status.contains(" input=stalled input_bytes=0 "), "{status}");
            assert!(status.contains(" agent=wall:unresponsive "), "{status}");
            assert!(
                attention().is_some_and(|a| a.contains("frozen")),
                "{:?}",
                attention()
            );
            let refused = refusal(&ctx, "send", OWN, false).expect("refused");
            assert!(
                refused.starts_with("ERR busy input-unread bytes=0 "),
                "{refused}"
            );
            assert!(refused.contains("retry"), "{refused}");
            assert!(!refused.contains("signal kill"), "{refused}");
            assert_eq!(refusal(&ctx, "send", OWN, true), None);
            assert_eq!(refusal(&ctx, "signal", "kill", false), None);

            let later = t1 + RESTART_GRACE;
            let burned = nanos + u64::try_from((later - t0).as_nanos()).expect("small");
            let (t, _) = app.session_status.inputs.step(
                77,
                sink.input_backlog().as_ref(),
                false,
                on_leader(burned),
                || panic!("the RSS is read at entry only"),
                later,
            );
            let Transition::Updated(fact) = t else {
                panic!("survived: {t:?}")
            };
            assert!(fact.restart.survived, "survived");
            app.publish_input_stall(77, &ctx, Some(fact), later);
            let shown = attention().expect("the server's line");
            assert!(shown.contains("signal kill"), "{shown}");
            let refused = refusal(&ctx, "send", "claude --continue", false).expect("refused");
            assert!(refused.contains("; end it: signal kill)"), "{refused}");

            let reply = crate::control::cmd_signal(master, "kill", &sink, None);
            assert_eq!(reply, format!("OK signalled pgrp {leader}\n"));
            let _ = job.0.wait();
            assert_eq!(
                app.observe_input_stalls(later + PROBE_MIN_GAP, Some(77)),
                [77]
            );
            assert_eq!(app.session_status.input_stall(77), None);
            assert_eq!(attention(), None);
            let status = record(&app);
            assert!(!status.contains("wall:unresponsive"), "{status}");
            assert!(!status.contains(" input=stalled "), "{status}");
            assert_eq!(refusal(&ctx, "send", OWN, false), None);
            drop(job);
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_program_that_lives_through_signal_term_and_reads_again_is_let_go() {
        const RECOVERS: &str =
            "trap '' TERM; trap 'go=1' USR1; while [ -z \"$go\" ]; do :; done; exec cat >/dev/null";
        let window = u64::try_from(SPIN_WINDOW.as_nanos()).expect("small");
        let (master, slave, job, leader) = pty_job(RECOVERS);
        let sink = std::sync::Arc::new(SinkWriter::new(master));
        sink.note_master_nonblocking(true);
        let mut app = crate::App::headless_for_test();
        app.pool
            .insert(crate::stub_session_with_sink(78, sink.clone()));
        let ctx = app.pool.get(78).expect("session 78").ctx.clone();
        claude_in_front(&ctx, leader, "busy");
        let record = |app: &crate::App| app.session_status_record(78).expect("live session");
        let burned = || pid_cpu_ns(leader).expect("the job's CPU");

        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while burned() < window {
            assert!(Instant::now() < deadline, "the job never spun a second");
            std::thread::sleep(Duration::from_millis(20));
        }
        sink.write_frame_nonparking(b"\r").expect("the Enter");
        let t0 = Instant::now();
        let ns = burned();
        let on_leader = |ns: u64| Activity {
            cpu: Some((leader, ns)),
            last_output: None,
            restarts: sink.restart_discards(),
            discards: sink.discards(),
            read_since_discard: false,
            contention: Contention::default(),
        };
        let inputs = &mut app.session_status.inputs;
        let _ = inputs.step(
            78,
            Some(&reading(1, 0, 0)),
            false,
            on_leader(ns - window),
            || None,
            t0.checked_sub(SPIN_WINDOW).expect("uptime"),
        );
        let (t, _) = inputs.step(
            78,
            Some(&reading(1, 12, 0)),
            false,
            on_leader(ns),
            || Some(2),
            t0,
        );
        let Transition::Entered(fact) = t else {
            panic!("entered: {t:?}")
        };
        app.publish_input_stall(78, &ctx, Some(fact), t0);

        let reply = crate::control::cmd_signal(master, "term", &sink, None);
        assert_eq!(reply, format!("OK signalled pgrp {leader} discarded=1\n"));
        assert!(
            app.observe_input_stalls(t0 + Duration::from_millis(300), Some(78))
                .is_empty(),
            "held"
        );
        assert!(refusal(&ctx, "send", "y", false).is_some());

        // SAFETY: `leader` is this test's live child and `SIGUSR1` is trapped by its script.
        assert_eq!(unsafe { libc::kill(leader, libc::SIGUSR1) }, 0);
        assert_eq!(sink.write_frame_nonparking(b"x").expect("the key"), 1);
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while aterm_pty::input_queue_len(master) != Some(0) {
            assert!(Instant::now() < deadline, "the job never read the key");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(refusal(&ctx, "send", "y", false), None);
        let status = record(&app);
        assert!(status.contains(" input=clear "), "{status}");
        assert_eq!(
            app.observe_input_stalls(t0 + Duration::from_millis(900), Some(78)),
            [78]
        );
        assert_eq!(app.session_status.input_stall(78), None);
        assert_eq!(ctx.meta.lock().unwrap().attention, None);
        assert!(!record(&app).contains("wall:unresponsive"));
        drop(job);
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }
}
