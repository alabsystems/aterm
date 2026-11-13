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
    // The watch lets a program macOS holds go at its probe, so a publication names one it saw started.
    let liveness = if published.is_some() {
        Liveness::Published {
            program_check: false,
        }
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

/// Whether the session holds input its program has not read yet.
pub(crate) fn input_unread(sink: &SinkWriter) -> bool {
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
pub(crate) const fn is_restart_signal(sig: libc::c_int) -> bool {
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

/// The PTY `master`'s foreground group leader's footprint ([`aterm_sysprobe::procs::footprint`]),
/// in MiB, or `None` when unknown.
pub(crate) fn fg_footprint_mb(master: i32) -> Option<u64> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let pgrp = crate::quit_safety::foreground_pgrp(master);
        let leader = u32::try_from(pgrp).ok().filter(|p| *p > 0)?;
        aterm_sysprobe::procs::footprint(leader)
            .ok()
            .map(|bytes| bytes >> 20)
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

/// `pid`'s total CPU time, user plus system, in nanoseconds.
#[cfg(target_os = "macos")]
pub(crate) fn pid_cpu_ns(pid: libc::pid_t) -> Option<u64> {
    let (numer, denom) = aterm_sysprobe::procs::kernel_timebase()?;
    let info = pid_task_info(pid)?;
    let ticks = u128::from(info.pti_total_user) + u128::from(info.pti_total_system);
    u64::try_from(ticks * u128::from(numer) / u128::from(denom)).ok()
}

/// `pid`'s `proc_pidinfo(PROC_PIDTASKINFO)` record, read only when the kernel wrote exactly one
/// whole record.
#[cfg(target_os = "macos")]
pub(crate) fn pid_task_info(pid: libc::pid_t) -> Option<libc::proc_taskinfo> {
    const PROC_PIDTASKINFO: libc::c_int = 4;
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
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
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    if pid <= 1 {
        return None;
    }
    let path = format!("/proc/{pid}/stat");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .ok()?;
    let meta = file.metadata().ok()?;
    // SAFETY: `geteuid` is a side-effect-free libc getter.
    let ours = unsafe { libc::geteuid() };
    if meta.uid() != ours {
        return None;
    }
    let mut stat = String::with_capacity(512);
    std::io::Read::read_to_string(&mut std::io::Read::take(file, 64 * 1024), &mut stat).ok()?;
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

/// `pid`'s major page faults, the ones that read a page in from backing store (`majflt`,
/// `/proc/<pid>/stat`'s 12th field).
#[cfg(target_os = "linux")]
pub(crate) fn pid_major_faults(pid: libc::pid_t) -> Option<u64> {
    linux_proc_stat_after_comm(pid)?
        .split_whitespace()
        .nth(9)?
        .parse()
        .ok()
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
    /// macOS's program check holds the foreground leader before its first instruction ([`held_leader`]).
    pub(crate) program_check: bool,
}

/// Whether the foreground `leader` is held at macOS's program check, asked of `held` only for a raw
/// queue with unread bytes whose job is not stopped: the one state a stall is entered from or stands
/// in, where a held leader, which never spins, would read as a program asleep.
pub(crate) fn held_leader(
    backlog: Option<&InputBacklog>,
    stopped: bool,
    leader: Option<i32>,
    held: impl FnOnce(u32) -> bool,
) -> bool {
    backlog.is_some_and(|b| b.unread() > 0 && !b.canonical)
        && !stopped
        && leader
            .and_then(|pid| u32::try_from(pid).ok())
            .is_some_and(held)
}

/// How contended this machine's CPUs are now: the worse of its 1- and 5-minute load averages
/// ([`aterm_sysprobe::load_average`]) over its logical cores ([`aterm_sysprobe::cpu_layout`]).
pub(crate) fn machine_contention() -> Contention {
    aterm_sysprobe::load_average().map_or_else(Contention::default, |[one, five, _]| {
        Contention::of_load(one.max(five), aterm_sysprobe::cpu_layout().logical_cores())
    })
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
    pub(crate) const fn dropped(&self, discards: u64) -> bool {
        self.leader.is_some() && discards != self.discards
    }

    /// Return whether a drop in the stall window was caused by a session restart.
    pub(crate) const fn restarted(&self, restarts: u64) -> bool {
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
    pub(crate) footprint_mb: Option<u64>,
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
        footprint: impl FnOnce() -> Option<u64>,
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
                        input_backlog::classify(
                            backlog,
                            stopped,
                            Liveness::Published {
                                program_check: activity.program_check
                            }
                        ),
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
            Liveness::Published {
                program_check: activity.program_check,
            }
        } else {
            Liveness::Watched {
                drew: activity.last_output.is_some_and(|at| at > since),
                spinning,
                program_check: activity.program_check,
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
                    footprint_mb: footprint(),
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
    now: Instant,
) -> (Option<InputBacklog>, bool, Activity) {
    let backlog = sink.input_backlog_at(now);
    let master = sink.master();
    let unread = backlog.is_some_and(|b| b.unread() > 0);
    let stopped = unread && fg_stopped(master);
    let restarts = sink.restart_discards();
    let discards = sink.discards();
    let looks = (unread || published) && !stopped;
    let cpu = looks.then(|| fg_cpu(master)).flatten();
    let activity = Activity {
        cpu,
        program_check: held_leader(
            backlog.as_ref(),
            stopped,
            cpu.map(|(pid, _)| pid),
            crate::program_check_watch::held_now,
        ),
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

/// Give `sink` its input watch: only where its stall's recovery can discard what is unread
/// ([`aterm_pty::input_queue_len`], macOS's), though Linux counts the unread input too.
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
    footprint_mb: Option<u64>,
) -> String {
    let dash = || "-".to_string();
    format!(
        "input={} input_bytes={} input_wait_ms={} fg_footprint_mb={}",
        word.as_str(),
        backlog.map_or_else(dash, |b| b.unread().to_string()),
        backlog.map_or_else(dash, |b| b.wait.as_millis().to_string()),
        footprint_mb.map_or_else(dash, |mb| mb.to_string()),
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
    // The watch lets a program macOS holds go at its probe, so a publication names one it saw started.
    let liveness = if published.is_some() {
        Liveness::Published {
            program_check: false,
        }
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
    let footprint = matches!(word, InputWord::Stalled | InputWord::Stopped)
        .then(|| fg_footprint_mb(sink.master()))
        .flatten();
    status_fields(backlog.as_ref(), word, footprint)
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
    let footprint = fact.footprint_mb.map(|mb| {
        if mb < 1024 {
            // A small program reads `footprint 1 MB`, never `footprint 0.0 GB`.
            return format!("footprint {mb} MB");
        }
        let tenths = (mb * 10 + 512) / 1024;
        format!("footprint {}.{} GB", tenths / 10, tenths % 10)
    });
    let explicit = fact.act.as_ref().map(|act| act.cmd.as_str());
    let head = |footprint: Option<&str>| {
        if fact.restart.survived {
            let footprint =
                footprint.map_or_else(String::new, |footprint| format!(" ({footprint})"));
            let remedy = explicit.map_or_else(
                || format!("end it: aterm ctl @{sid} signal kill"),
                |cmd| format!("restart it: {cmd}"),
            );
            format!(
                "{prog} is still running after its restart signal, not reading input {when}{footprint} \
                 \u{2014} {remedy}"
            )
        } else {
            let footprint =
                footprint.map_or_else(String::new, |footprint| format!(", {footprint}"));
            let cmd =
                explicit.map_or_else(|| format!("aterm ctl @{sid} signal term"), str::to_string);
            format!(
                "{prog} is frozen: not reading input {when} ({} B queued{footprint}) \u{2014} restart \
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
    [footprint.as_deref(), None]
        .into_iter()
        .flat_map(|footprint| tails.iter().map(move |tail| (footprint, tail)))
        .map(|(footprint, tail)| format!("{}{tail}", head(footprint)))
        .find(|text| text.len() <= crate::session_timeline::META_ATTENTION_KEYED_MAX)
        .unwrap_or_else(|| head(footprint.as_deref()))
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
    other_hand(ctx, now_us)
}

/// Whose hand other than the host's is on session `ctx` at `now_us`: a hold, or a driver's turn or
/// lease.
pub(crate) fn other_hand(ctx: &crate::SessionCtx, now_us: u64) -> Option<&'static str> {
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

/// [`attention_text`] at `now`, with the zone read at `fact.since_unix`.
pub(crate) fn attention_now(
    fact: &InputStallFact,
    program: Option<&str>,
    after: &AfterRestart,
    sid: &str,
    now: Instant,
) -> String {
    attention_now_local(
        fact,
        program,
        after,
        sid,
        now,
        &crate::presence::local_offset_at,
    )
}

pub(crate) fn attention_now_local(
    fact: &InputStallFact,
    program: Option<&str>,
    after: &AfterRestart,
    sid: &str,
    now: Instant,
    offset_at: &dyn Fn(i64) -> Option<i64>,
) -> String {
    let clock = fact
        .since_unix
        .and_then(|since_unix| since_hhmm(since_unix, offset_at(since_unix)));
    attention_text(
        fact,
        program,
        after,
        sid,
        clock.as_deref(),
        now.saturating_duration_since(fact.since),
    )
}

/// The identity of one stall episode: the key of the need it raises ([`crate::needs::Need::key`]).
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
                // The hook first, then the take: a key landing after the
                // rearm marks it again and wakes again, and one before it
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
                now,
            );
            let master = ctx.sink.master();
            let (transition, _) = self.session_status.inputs.step(
                id,
                backlog.as_ref(),
                stopped,
                activity,
                || fg_footprint_mb(master),
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
#[path = "input_stall_tests.rs"]
pub(crate) mod tests;
