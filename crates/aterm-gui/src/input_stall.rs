// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The UNREAD-INPUT GATE (G3): a socket-driven input write into a session
//! whose program has left earlier input unread is refused
//! `ERR busy input-unread …`, and nothing is written.
//!
//! WHY THIS EXISTS (2026-09-24): a Claude Code session froze — 38.8 GiB
//! resident, still spinning on the CPU, never reading its tty again — with the
//! owner's Enter sitting unread in the slave's input queue. The in-window
//! supervisor read an approval box off the screen, and its screen-fenced
//! `key down` passed every check it had: the screen had not changed, because
//! the program had stopped drawing, so a fence on the screen can never see
//! this. The key was queued BEHIND the Enter, and both were read together,
//! later, against a screen nobody had looked at. The facts that separate a
//! frozen program from a busy one are in the kernel, not on the screen:
//! [`aterm_session::sink::SinkWriter::input_backlog`] reads how many bytes the
//! program has not read and how long the oldest has waited, and
//! [`aterm_session::input_backlog::refuses`] is the rule — raw-mode input a
//! second old, or ANY input under a stopped job. The rule is proved over a
//! bounded model (`aterm_spec::input_unread_gate_model`, machine
//! `InputUnreadGate`) and bound to a real pty in aterm-session's
//! `conformance_input_unread`.
//!
//! WHERE IT IS ASKED: on the SOCKET seams only — the verb dispatch
//! (`control::handle`), the `feed-bin`/`paste-bin` frame, the operator
//! proposal frame and the App lane — each after authorization and after the
//! halt, so an unauthorized caller learns nothing and a halted session says
//! `ERR halted` first. NEVER on the input seam the window keyboard travels
//! (`input.rs`, `app_input.rs`, `control_input.rs`'s write path, the sink): a
//! person typing into a frozen program is refused nothing, exactly as the
//! fleet halt refuses them nothing. `control.rs`'s
//! `the_unread_input_gate_sits_only_on_the_socket_seams` pins both halves.
//!
//! WHAT STAYS OPEN: `resize`, `signal`, `close`, `pane` and `tab` write no input
//! bytes and are the remedies — `signal int` interrupts, `signal term`
//! restarts a frozen program, `signal cont` resumes a stopped one. So does a
//! lone SIGNAL CHARACTER (`key ctrl+c`, `ctrl c`, `feed 03`) while the tty has
//! `ISIG` on: the line discipline turns it into a signal as it is written, so
//! it is never read behind anything (S4 review, 2026-09-24). A driver that
//! means to queue anyway leads `send`/`key` with `unread=ok`. The refusal
//! starts `ERR busy`, so every driver's existing back-off
//! (`CtlReply::is_err("busy")`) retries it rather than treating it as a hard
//! failure.

use aterm_session::input_backlog::{self, InputBacklog, InputWord, Liveness};
use aterm_session::sink::SinkWriter;
use aterm_types::keyboard::KeyboardMode;
use std::time::{Duration, Instant};

use crate::SessionCtx;
use crate::input::InputEvent;
use crate::menu::MenuAction;

/// Whether `verb` (with its argument tail `rest`) writes INPUT BYTES to a
/// session's PTY — the verbs the gate asks about.
///
/// A SUBSET of `fabric::is_pty_reaching`, the halt's set, minus the verbs that
/// reach a PTY without writing input to it: `resize` (a winsize), `signal` (a
/// signal), `close` (a hangup), `pane` and `tab` (which session the keyboard
/// drives, or retiring one) and `confirm` (the answer to a parked close, whose
/// `yes` is a hangup too). Those are exactly the remedies for a program that
/// is not reading, so they must stay answerable while this gate refuses.
/// `invoke` writes input only for an action whose
/// [`MenuAction::writes_pty_input`] says so (today, `Paste`).
pub(crate) fn is_input_writing(verb: &str, rest: &str) -> bool {
    match verb {
        "send"
        | "key"
        | "ctrl"
        | "feed"
        | "feed-bin"
        | "paste"
        | "paste-bin"
        | "mouse"
        | "focus"
        | "turn"
        | "operator-propose-bin"
        | "hwkey"
        | "pointer" => true,
        "invoke" => rest
            .split_whitespace()
            .next()
            .and_then(MenuAction::from_invoke_name)
            .is_some_and(MenuAction::writes_pty_input),
        _ => false,
    }
}

/// The refusal an input-writing `verb` answers against `ctx`'s session, or
/// `None` when it may proceed: not an input-writing verb, `unread_ok` given,
/// no reading (not macOS, or not a tty), [`input_backlog::refuses`] says the
/// unread input is young enough (or canonical type-ahead), or the write is a
/// lone signal character the tty turns into a signal
/// ([`InputBacklog::signals_on_write`]). The foreground job's stop state is
/// read only when bytes are queued, so an idle session pays for one
/// `tcgetattr` and two ioctls and no process lookup. A refusal also wakes the
/// session's input watch, in case nothing else has.
///
/// One refusal needs no unread byte at all: a published stall HELD through a
/// restart ([`Restart`]) — aterm dropped the queue before `signal term`, and
/// the program the stall was published for still leads the foreground. Its
/// queue is empty because nothing READ it, and a key sent now queues into a
/// program that is not reading — where the `claude --continue` typed after
/// the signal went (whole-branch review, third round, 2026-09-25). Only a
/// session that has had a discard pays for the timeline read that asks.
pub(crate) fn refusal(ctx: &SessionCtx, verb: &str, rest: &str, unread_ok: bool) -> Option<String> {
    if unread_ok || !is_input_writing(verb, rest) {
        return None;
    }
    let backlog = ctx.sink.input_backlog()?;
    // A `^C` into a cbreak program is SIGINT, not a byte behind the unread
    // ones (S4 review, 2026-09-24: the gate refused the remedy).
    let signals = || {
        written_bytes(verb, rest, ctx.modes.keyboard_mode())
            .is_some_and(|bytes| backlog.signals_on_write(&bytes))
    };
    if ctx.sink.discards() != 0 {
        // Copied out, so the leader lookup runs with the timeline released.
        let published = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .agent()
            .input
            .clone();
        if published.is_some_and(|fact| restart_held_now(&fact, &ctx.sink)) {
            return (!signals()).then(|| restart_refusal_text(&backlog));
        }
    }
    if backlog.queued == 0 {
        return None;
    }
    let stopped = fg_stopped(ctx.sink.master());
    if !input_backlog::refuses(&backlog, stopped) || signals() {
        return None;
    }
    // The refusal WAKES the watch (S6 review, 2026-09-24): input a second
    // stale that no write through this sink announced — an unseen writer's,
    // an adopted master's — may have no watch looking at it, and nothing this
    // gate refuses will land to start one. At most once per probe (the hook's
    // own arming, rearmed by each probe), so a driver retrying into the
    // refusal never drives the probe past `PROBE_MIN_GAP`.
    let _ = ctx.sink.wake_input_hook();
    // The word is the one `status input=` answers: `stalled` only once the
    // watch has published it — a refusal sees one reading, not the program.
    let published = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .agent()
        .input
        .is_some();
    let liveness = if published {
        Liveness::Published
    } else {
        Liveness::Unobserved
    };
    // A refused `^C` (a raw program reads it as a byte) names the signal that
    // interrupts instead (S4 review, 2026-09-24) — the legacy encoding, so a
    // kitty-mode `ctrl+c` counts too.
    let interrupt =
        written_bytes(verb, rest, KeyboardMode::default()).as_deref() == Some(b"\x03".as_slice());
    Some(refusal_text(
        &backlog,
        input_backlog::classify(Some(&backlog), stopped, liveness),
        interrupt,
    ))
}

/// The exact bytes `verb` writes for `rest`, for the verbs whose bytes follow
/// from their arguments and the keyboard mode alone: `key` and `ctrl` through
/// the encoder their seams run (so a program that asked for the kitty
/// protocol gets `ctrl+c` as the CSI sequence it will receive, not `^C`),
/// `feed` decoded from hex and `send` raw. `None` for every other verb and for
/// a tail that does not parse, which its own arm refuses later.
fn written_bytes(verb: &str, rest: &str, mode: KeyboardMode) -> Option<Vec<u8>> {
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
        event_type,
    } = event
    else {
        return None;
    };
    Some(aterm_types::keyboard::encode_key_with_layout(
        &key,
        mods,
        mode,
        event_type,
        base_layout,
    ))
}

/// The refusal line for one reading and its word: `ERR busy input-unread
/// bytes=<n> wait_ms=<ms> input=<word> (<what is true> ; <the one thing to do>)`.
/// PURE, so its wording is pinned without a pty. `bytes=` is every unread input
/// byte (the kernel's queue plus aterm's spill); `wait_ms=` is a lower bound on
/// how long the oldest has waited. ONE remedy: a stopped job is resumed; a
/// refused `^C` (`interrupt`) names `signal int`; a published stall is
/// restarted (the attention line's remedy — a program that lives through it
/// earns [`restart_refusal_text`]'s `signal kill`); input not yet a stall is
/// retried. `unread=ok` stays in `help key`.
pub(crate) fn refusal_text(b: &InputBacklog, word: InputWord, interrupt: bool) -> String {
    let wait_ms = b.wait.as_millis();
    let queued = crate::presence::fmt_dur(b.wait);
    let why = match word {
        InputWord::Stopped => "the program is stopped; resume it: signal cont".to_string(),
        _ if interrupt => {
            format!("the program has not read input queued {queued} ago; interrupt it: signal int")
        }
        InputWord::Stalled => {
            format!("the program has not read input queued {queued} ago; restart it: signal term")
        }
        _ => format!("the program has not read input queued {queued} ago; retry in a moment"),
    };
    format!(
        "ERR busy input-unread bytes={} wait_ms={wait_ms} input={} ({why})\n",
        b.unread(),
        word.as_str()
    )
}

/// The refusal line while a stall is HELD through a restart ([`Restart`]):
/// the same `ERR busy input-unread … input=stalled` shape, so every driver's
/// back-off and the supervisor's hold read it as they read the other, with
/// the remedy it has come to — the attention line's own words. PURE.
pub(crate) fn restart_refusal_text(b: &InputBacklog) -> String {
    format!(
        "ERR busy input-unread bytes={} wait_ms={} input={} (the program is still running \
         after its restart signal; end it: signal kill)\n",
        b.unread(),
        b.wait.as_millis(),
        InputWord::Stalled.as_str()
    )
}

/// Whether a `turn` RE-PRESS would only queue another Enter behind input the
/// program has not read yet. The re-press exists for an Enter the program READ
/// and swallowed without drawing; an Enter still sitting in its queue has not
/// been swallowed, it has not been read, and a second one stacks behind it, to
/// be read with it, much later, as the incident's queued keys were. Counts the
/// spill too: bytes aterm has not yet handed the kernel are just as far behind.
/// `false` with no reading, so a platform that cannot tell keeps its re-press.
pub(crate) fn repress_would_queue(sink: &SinkWriter) -> bool {
    sink.input_backlog().is_some_and(|b| b.unread() > 0)
}

/// Before `signal term|kill|hup|quit` reaches the PTY's foreground job, DROP
/// the input it left unread, when that input can only have been meant for
/// it: the tty is in raw or cbreak mode (the program, not the line
/// discipline, owns every key typed there), or the job is stopped. Returns
/// what was dropped, `None` when nothing was (another signal, canonical
/// type-ahead, nothing unread, no reading — off macOS or off a tty).
///
/// WHY (whole-branch review, 2026-09-25): `signal term` is the remedy the
/// server's attention line, the band, the menu row and the refusal all name
/// for a frozen program, and the window keyboard is never gated — a person
/// who kept typing into a frozen Claude Code builds up complete lines in its
/// queue. The queue outlives the program: reproduced on a pty, `zsh -f -i`
/// running a raw-mode program that stopped reading, a line typed into it,
/// SIGTERM to the job — zsh took the tty back and RAN the line as a command.
/// Reading the screen first cannot help; the shell reads the queue the
/// moment it gets the terminal. So the remedy drops those keys itself.
///
/// WHAT IT KEEPS: canonical input — complete lines typed during a shell
/// command are the SHELL's type-ahead (`sleep 30`, then `ls` typed ahead),
/// and the flush would also take the partial line FIONREAD cannot count.
/// `int` keeps everything: it is the program's own interrupt, and a program
/// that survives it (a REPL, Claude Code's first ^C) reads what follows.
/// `cont` resumes a job that reads its queue itself; `tstp` hands the tty to
/// the shell with the job alive to come back to.
///
/// Dropped BEFORE the signal, while the program still owns the tty: after
/// it, the shell may already be reading. Everything the program left unread
/// goes, not only the kernel's queue: the spill aterm holds behind a full
/// queue, and the rest of any frame a writer is still handing the kernel
/// ([`SinkWriter::discard_unread_input`]). The first cut of this remedy
/// flushed the kernel queue alone. Its reply said `discarded=1022 left=96`,
/// and the sink's drainer then slept on the flushed queue for good, so every
/// later key queued behind the 96 and the session was deaf (whole-branch
/// review, second round, 2026-09-25). The count is every byte dropped.
///
/// A DROP IS NOT A READ (third round, 2026-09-25): the queue this empties is
/// what a recovered program leaves too, so a published stall remembers the
/// sink's discard count and is held, not withdrawn, while its program lives
/// through the signal ([`Restart`]).
#[cfg(unix)]
pub(crate) fn discard_before_signal(
    master: i32,
    sink: &SinkWriter,
    sig: libc::c_int,
) -> Option<usize> {
    if !matches!(
        sig,
        libc::SIGTERM | libc::SIGKILL | libc::SIGHUP | libc::SIGQUIT
    ) {
        return None;
    }
    let echo = aterm_pty::tty_echo(master)?;
    // A spill the probe could not read (its mutex busy) counts as input.
    let unread = sink.input_backlog()?;
    if unread.queued == 0 && unread.spilled == Some(0) {
        return None;
    }
    if echo.canonical && !fg_stopped(master) {
        return None;
    }
    sink.discard_unread_input()
}

/// After `signal term|kill|hup|quit` has been sent: wake the session's input
/// watch, so it looks NOW rather than at its next [`RECHECK`]: a stall whose
/// program is living through the signal is held from that probe, and
/// [`RESTART_GRACE`] counts from it ([`Restart`]). The wake is the one the
/// sink's writes share ([`SinkWriter::wake_input_hook`]), so it never
/// doubles a probe already owed, and a session with no hook (off macOS, off
/// a tty) pays nothing.
#[cfg(unix)]
pub(crate) fn after_signal(sink: &SinkWriter, sig: libc::c_int) {
    if matches!(
        sig,
        libc::SIGTERM | libc::SIGKILL | libc::SIGHUP | libc::SIGQUIT
    ) {
        let _ = sink.wake_input_hook();
    }
}

/// Whether the PTY `master`'s foreground job is STOPPED (`SIGSTOP`/`SIGTSTP`):
/// its group leader's `pbi_status` is `SSTOP`. `false` when unknown, and
/// always off macOS.
pub(crate) fn fg_stopped(master: i32) -> bool {
    #[cfg(target_os = "macos")]
    {
        let pgrp = crate::quit_safety::foreground_pgrp(master);
        pgrp > 0 && pid_stopped(pgrp) == Some(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = master;
        false
    }
}

/// The PTY `master`'s foreground group leader's resident set, in MiB, or
/// `None` when unknown (and always off macOS). For the stall's publication:
/// the incident's program was 38.8 GiB resident when it was found, and an
/// owner deciding whether to restart it wants that number beside the stall.
pub(crate) fn fg_rss_mb(master: i32) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let pgrp = crate::quit_safety::foreground_pgrp(master);
        if pgrp <= 0 {
            return None;
        }
        pid_rss_mb(pgrp)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = master;
        None
    }
}

/// The PTY `master`'s foreground group leader and its total CPU time in
/// nanoseconds ([`pid_cpu_ns`]), or `None` when unknown (and always off
/// macOS). The LEADER only: zsh's completion child runs in zsh's group and
/// may burn CPU while zsh, which holds the unread key, sleeps.
pub(crate) fn fg_cpu(master: i32) -> Option<(i32, u64)> {
    #[cfg(target_os = "macos")]
    {
        let pgrp = crate::quit_safety::foreground_pgrp(master);
        if pgrp <= 0 {
            return None;
        }
        pid_cpu_ns(pgrp).map(|ns| (pgrp, ns))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = master;
        None
    }
}

/// `pid`'s stop state from `proc_pidinfo(PROC_PIDTBSDINFO)` — the
/// `seamless::read_process_birth` read: exact size, and the record must name
/// `pid` and belong to this uid. `Some(true)` for `SSTOP`; `None` when the
/// record cannot be read or is not ours.
#[cfg(target_os = "macos")]
pub(crate) fn pid_stopped(pid: libc::pid_t) -> Option<bool> {
    /// `p_stat` of a stopped process (`<sys/proc.h>`); aterm-libc does not
    /// export it, and it is generated rather than hand-edited.
    const SSTOP: u32 = 4;
    if pid <= 1 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` points at `size` writable bytes of exactly the structure
    // PROC_PIDTBSDINFO fills; libproc returns the number of bytes it wrote.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: the exact-size success above initialized the whole record.
    let info = unsafe { info.assume_init() };
    // SAFETY: `geteuid` is a side-effect-free libc getter.
    let ours = unsafe { libc::geteuid() };
    if u32::try_from(pid).ok()? != info.pbi_pid || info.pbi_uid != ours {
        return None;
    }
    Some(info.pbi_status == SSTOP)
}

/// `struct proc_taskinfo` (`<sys/proc_info.h>`), which aterm-libc does not
/// declare: six `u64` then twelve `i32`, 96 bytes — measured on Darwin 25.6
/// (`proc_pidinfo(pid, PROC_PIDTASKINFO)` returns 96) and the layout the libc
/// crate's `apple` module carries. The resident size and the total CPU times
/// are read; the times are in MACH ABSOLUTE TIME units, not nanoseconds
/// (measured 2026-09-25 on this Mac: a process spinning one core for 2.005 s
/// of wall time gained 45.9 M user ticks, which the 125/3 timebase makes
/// 1.91 s) — [`pid_cpu_ns`] converts them.
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

/// `struct mach_timebase_info` (`<mach/mach_time.h>`): mach absolute time
/// units times `numer / denom` is nanoseconds.
#[cfg(target_os = "macos")]
#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// Declared here: aterm-libc does not export it.
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> libc::c_int;
}

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<ProcTaskInfo>() == 96);

/// `pid`'s resident set in MiB from `proc_pidinfo(PROC_PIDTASKINFO)`, read
/// only when the kernel wrote exactly one whole record. A same-uid, non-parent
/// reader is allowed (measured on the incident's process).
#[cfg(target_os = "macos")]
pub(crate) fn pid_rss_mb(pid: libc::pid_t) -> Option<u64> {
    pid_task_info(pid).map(|info| info.resident_size / (1024 * 1024))
}

/// `pid`'s total CPU time, user plus system, in nanoseconds — the liveness
/// half of a stall ([`Liveness::Watched`]'s `spinning`): the incident's
/// frozen program burned ~380% CPU while it read nothing, where `less` on a
/// slow pipe or zsh inside a completion sleeps. `None` when the record or
/// the timebase cannot be read.
#[cfg(target_os = "macos")]
pub(crate) fn pid_cpu_ns(pid: libc::pid_t) -> Option<u64> {
    let info = pid_task_info(pid)?;
    let mut base = MachTimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: `base` is a live, writable `mach_timebase_info` record; the
    // call only fills it.
    let rc = unsafe { mach_timebase_info(&raw mut base) };
    if rc != 0 || base.denom == 0 {
        return None;
    }
    let ticks = u128::from(info.total_user) + u128::from(info.total_system);
    u64::try_from(ticks * u128::from(base.numer) / u128::from(base.denom)).ok()
}

/// `proc_pidinfo(pid, PROC_PIDTASKINFO)`, read only when the kernel wrote
/// exactly one whole record.
#[cfg(target_os = "macos")]
fn pid_task_info(pid: libc::pid_t) -> Option<ProcTaskInfo> {
    /// `PROC_PIDTASKINFO` (`<sys/proc_info.h>`); not in aterm-libc.
    const PROC_PIDTASKINFO: libc::c_int = 4;
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<ProcTaskInfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<ProcTaskInfo>()).ok()?;
    // SAFETY: `info` points at `size` writable bytes laid out as the
    // `proc_taskinfo` PROC_PIDTASKINFO fills (pinned to 96 bytes above);
    // libproc returns the number of bytes it wrote.
    let read =
        unsafe { libc::proc_pidinfo(pid, PROC_PIDTASKINFO, 0, info.as_mut_ptr().cast(), size) };
    if read != size {
        return None;
    }
    // SAFETY: the exact-size success above initialized the whole record, and
    // every field is a plain integer, valid for any bit pattern.
    Some(unsafe { info.assume_init() })
}

// ---------------------------------------------------------------------------
// THE WATCH (G1, and the server half of G2): publishing a stall.
// ---------------------------------------------------------------------------

/// The attention owner the SERVER writes a stall under (`meta attention_owner=
/// aterm`). A keyed entry, so it outranks an older supervisor badge while the
/// stall stands (the most recent write wins) and uncovers it again when the
/// stall clears — the supervisor's entry is never touched.
pub(crate) const SERVER_ATTENTION_OWNER: &str = "aterm";

/// The cadence of a probe that has nothing sooner to wait for: a stall or a
/// stopped job being watched for its end, canonical type-ahead being watched
/// for its read, or a pending byte already past [`input_backlog::STALL_AFTER`]
/// that aterm's own unread output keeps from entering the stall.
pub(crate) const RECHECK: Duration = Duration::from_secs(5);

/// The least time between two probes of one session asked for by a WAKE (the
/// input hook, the session's own output) rather than by its deadline: an
/// output flood from an armed session costs one probe a quarter second, never
/// one per burst.
pub(crate) const PROBE_MIN_GAP: Duration = Duration::from_millis(250);

/// The least wall time a CPU reading of the foreground leader is compared
/// across: a shorter window (two probes a wake apart) keeps the previous
/// window's verdict rather than judging a quarter second of scheduling.
pub(crate) const SPIN_WINDOW: Duration = Duration::from_secs(1);

/// What one probe saw of the PROGRAM, beside its input queue — the evidence
/// [`InputWatches::step`] folds into [`Liveness::Watched`] (whole-branch
/// review, 2026-09-25: an old unread byte alone published `less` on a slow
/// pipe, zsh inside a slow completion and a cbreak progress line as frozen)
/// — and whether aterm itself has dropped its input since ([`Restart`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Activity {
    /// The foreground group leader and its total CPU time in nanoseconds
    /// ([`fg_cpu`]); `None` when unknown.
    pub(crate) cpu: Option<(i32, u64)>,
    /// When aterm last read output from the session's PTY (the reader's
    /// `latest_output_activity_ns` stamp); `None` = never.
    pub(crate) last_output: Option<Instant>,
    /// The sink's discard count ([`SinkWriter::discards`]).
    pub(crate) discards: u64,
    /// The program has READ input accepted after aterm's last discard
    /// ([`SinkWriter::read_since_discard`]). Asked only while a stall is
    /// published, the one case that needs it ([`Restart`]).
    pub(crate) read_since_discard: bool,
}

/// How long a program that has been sent a restart signal (`signal
/// term|hup|quit`, whose discard emptied its queue) may take to end before
/// its stall's remedy moves on to `signal kill` ([`Restart::survived`]). A
/// program that can run its handler ends well inside it; one whose handler
/// never runs does not end at all.
pub(crate) const RESTART_GRACE: Duration = Duration::from_secs(5);

/// What a published stall keeps so that a RESTART is not taken for a
/// recovery (whole-branch review, third round, 2026-09-25).
///
/// `signal term` drops the input a frozen program left unread before it
/// signals ([`discard_before_signal`]), so the next probe finds the queue
/// empty — exactly what a program that read its input looks like — and the
/// watch took it for that. It withdrew the whole publication
/// (`agent=wall:unresponsive`, the server's attention, the band and the menu
/// row, the supervisor's hold) from a program that never ended: a spinning
/// Node program with a SIGTERM listener. The listener replaces the default
/// action, and only the JS thread runs it — the thread that is spinning.
/// Measured on a headless instance: `status` read `input=clear` 15 s after
/// `OK signalled … discarded=1` while `ps` showed the program still at 90%
/// CPU, and the `claude --continue` typed next queued into it. Claude Code
/// registers such listeners, and the incident's process was busy on its main
/// thread (`ps -M`). Nothing named `signal kill`.
///
/// So a stall is HELD while aterm, not the program, emptied its queue
/// ([`Self::dropped`]) and the leader it was published for still leads the
/// foreground group: `status input=stalled` (`input_bytes=0`: the keys were
/// dropped, never read), the publication standing, and every input-writing
/// verb refused ([`refusal`]). A leader still there [`RESTART_GRACE`] after
/// the drop has SURVIVED, and the attention line, the menu row and the band
/// name `signal kill`.
///
/// The hold ends, and the episode with it, on the first sign that the program
/// is alive ([`InputWatches::step`]):
///
/// - its leader stops leading: the program ended and the shell took the
///   terminal back;
/// - it draws after the drop;
/// - it READS a byte sent after the drop ([`SinkWriter::read_since_discard`],
///   told only on a raw tty, where the driver eats no byte itself — a cbreak
///   program's `^C` leaves the queue unread);
/// - or it was spinning when the hold began, a later window judges it NOT
///   spinning, and nothing sent since has waited
///   [`input_backlog::REFUSE_AFTER`] unread.
///
/// WHY THE LAST TWO (whole-branch review, fourth round, 2026-09-25). The hold
/// used to end only on the first two. A program that lived through `signal
/// term` and then recovered stayed published as frozen for good if it was
/// asleep in `read`, read every key sent to it and drew nothing. Its status
/// read `input=stalled`, its line read `… still running after its restart
/// signal, not reading input … end it: signal kill`, and every input verb was
/// refused, all while it was reading. The reviewer measured this on a
/// headless instance with a Python fixture that ignored SIGTERM, spun for
/// 25 s, then read without drawing. Two facts the hold did not check said the
/// program was alive. The spin was the evidence the stall was entered on, and
/// it had stopped. A byte sent with `unread=ok` was read at once. The
/// incident's program keeps spinning and reads nothing, so it stays held. A
/// program that stopped spinning but leaves new input unread is not reading,
/// so it stays held too. One that was asleep when the hold began has no spin
/// to stop, so only a read, a draw or its end lets it go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Restart {
    /// The foreground group leader the stall was published for, from the
    /// entry probe's CPU reading; `None` when that read failed (and for a
    /// stopped job's stall, whose remedy is `signal cont`), and then no
    /// restart is ever held.
    pub(crate) leader: Option<i32>,
    /// The sink's discard count at entry ([`SinkWriter::discards`]): one that
    /// has moved since means aterm dropped the queue.
    pub(crate) discards: u64,
    /// The leader has outlived its restart signal by [`RESTART_GRACE`],
    /// drawing nothing: the remedy is `signal kill` now.
    pub(crate) survived: bool,
}

impl Restart {
    /// Whether aterm has DROPPED the queue of a stall published with this
    /// record: it knows the stall's leader, and the sink's discard count
    /// (`discards`) has moved since entry. The hold then stands while that
    /// leader leads and shows no sign of life ([`InputWatches::step`]).
    pub(crate) fn dropped(&self, discards: u64) -> bool {
        self.leader.is_some() && discards != self.discards
    }
}

/// Whether `fact`'s restart hold stands NOW against `sink`, as far as one
/// reading can tell. `status` and the refusal need this without waiting for
/// the watch's next probe: a driver's `send` right behind its `signal term`
/// is exactly the key that must not queue into the program. The discard count
/// is checked first, so an ordinary reading pays for no process lookup.
///
/// One reading sees a READ since the drop ([`SinkWriter::read_since_discard`])
/// and the leader, but not the spin window or a draw, which only the watch
/// sees. A read lets the hold go here at once, and WAKES the watch so the
/// publication follows within a turn instead of at its next [`RECHECK`]. The
/// wake shares the hook's arming, so a driver polling `status` adds at most
/// one probe per probe.
fn restart_held_now(fact: &InputStallFact, sink: &SinkWriter) -> bool {
    if !fact.restart.dropped(sink.discards()) {
        return false;
    }
    if sink.read_since_discard() {
        let _ = sink.wake_input_hook();
        return false;
    }
    fg_cpu(sink.master()).map(|(pid, _)| pid) == fact.restart.leader
}

/// Fold one CPU reading into a watch's window: `mark` is the reading the
/// window started at (leader, instant, CPU ns), `spinning` the last verdict.
/// A window of at least [`SPIN_WINDOW`] is judged — spinning when the leader
/// burned at least half a core across it — and restarts at this reading; a
/// shorter one keeps its mark and its verdict. A new leader (the foreground
/// job changed) or no reading restarts the window with no verdict.
fn spin_window(
    mark: Option<(i32, Instant, u64)>,
    spinning: bool,
    cpu: Option<(i32, u64)>,
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
            (Some((pid, now, ns)), burned.saturating_mul(2) >= wall_ns)
        }
        _ => (Some((pid, now, ns)), false),
    }
}

/// One session's PUBLISHED input stall: its program has left raw-mode input
/// unread past [`input_backlog::STALL_AFTER`] ([`InputWord::Stalled`]), or its
/// job is stopped with input queued ([`InputWord::Stopped`]).
///
/// WHY THE SERVER PUBLISHES IT (2026-09-24): the incident's frozen Claude Code
/// held the owner's Enter for 2h41m while the in-window supervisor re-read an
/// unmoving approval box and re-raised "answer this box" — the one badge that
/// was wrong, because the box could not be answered by anyone. A frozen
/// program's screen does not move, so every screen reader keeps its last
/// verdict for ever; the unread byte in the kernel's queue is the fact that
/// moved. A supervisor-written "frozen" badge would go stale the moment the
/// program recovered (nothing would clear it) — so aterm writes and clears it
/// ITSELF, in lockstep with the probe: `status agent=wall:unresponsive` for an
/// identified agent, a keyed attention entry owned by [`SERVER_ATTENTION_OWNER`]
/// for every program, and the presence refresh that carries both to the band,
/// the rim and the menu.
///
/// Fixed at ENTRY: `since` dates the oldest unread byte, and `bytes` and
/// `rss_mb` are what they were when the stall was published (the RSS is read
/// once per episode — the incident's program was 38.8 GiB resident), and so
/// are the leader and the discard count its [`Restart`] is judged against.
/// The live numbers are `status input_bytes= input_wait_ms= fg_rss_mb=`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputStallFact {
    /// [`InputWord::Stalled`] or [`InputWord::Stopped`].
    pub(crate) word: InputWord,
    /// When the oldest unread byte was accepted by the kernel (a lower bound
    /// on its age: `now - wait` at the reading).
    pub(crate) since: Instant,
    /// Every unread input byte at entry: the kernel's queue plus the spill.
    pub(crate) bytes: usize,
    /// The foreground job is stopped (`word == Stopped`): the remedy is
    /// `signal cont`, not a restart.
    pub(crate) stopped: bool,
    /// The foreground group leader's resident set at entry, in MiB.
    pub(crate) rss_mb: Option<u64>,
    /// What tells a restart from a recovery ([`Restart`]).
    pub(crate) restart: Restart,
}

/// What one probe did to a session's published stall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Transition {
    /// Nothing published moved.
    None,
    /// A stall was published: a new episode. One that follows a restart
    /// hold's recovery ([`Restart`]) in the same probe replaces the old
    /// episode's publication.
    Entered(InputStallFact),
    /// The published stall cleared.
    Left,
    /// The published stall changed its word (stalled ⇄ stopped) or its
    /// program survived its restart ([`Restart::survived`]); the episode, its
    /// date and its RSS carry over.
    Updated(InputStallFact),
}

/// One watched session: when it is probed next and what is published.
#[derive(Clone, Debug)]
struct InputWatch {
    /// The deadline the event loop folds (`DeadlineOwner::InputWatch`).
    next_probe: Instant,
    /// When it was last probed, for [`PROBE_MIN_GAP`].
    last_probe: Instant,
    /// The published stall, if any.
    published: Option<InputStallFact>,
    /// Where the CPU window started ([`spin_window`]): leader, instant, ns.
    cpu_mark: Option<(i32, Instant, u64)>,
    /// The last judged window's verdict: the leader was spinning.
    spinning: bool,
    /// The probe that first found the published stall's queue dropped by
    /// aterm with its leader still leading ([`Restart`]), and whether the
    /// leader was spinning then. The instant starts [`RESTART_GRACE`], and
    /// output after it means the program is drawing again. `signal term`
    /// wakes the watch after its signal ([`after_signal`]), so this is the
    /// signal's own instant give or take a [`PROBE_MIN_GAP`]. A leader that
    /// was spinning then and is judged not spinning later has stopped
    /// spinning since.
    dropped: Option<(Instant, bool)>,
}

/// Every session's input watch. A session is watched from the first probe
/// that finds input unread until one finds nothing unread with nothing
/// published — so an IDLE machine arms nothing: no entry, no deadline, and the
/// only thing that starts a watch is the sink's input hook firing on a write
/// that landed (`Wake::InputWatch`).
///
/// Each probe decides the next ([`Self::step`]):
///
/// | word | next probe |
/// |---|---|
/// | `clear`, `-` | none — the watch ends unless a stall is still published |
/// | `pending` | when the oldest byte turns [`input_backlog::STALL_AFTER`] old ([`RECHECK`] once it has, until the program is seen spinning or [`input_backlog::QUIET_STALL_AFTER`] asleep) |
/// | `stalled`, held through a restart not yet survived ([`Restart`]) | when [`RESTART_GRACE`] ends |
/// | `typeahead`, `stalled`, `stopped` | [`RECHECK`] |
#[derive(Debug, Default)]
pub(crate) struct InputWatches {
    watches: std::collections::HashMap<u64, InputWatch>,
}

impl InputWatches {
    /// Fold one probe of session `id` — `backlog` its reading (`None` = no
    /// reading), `stopped` its foreground job's stop state, `activity` what
    /// the program itself did — into the watch, and say what the publication
    /// must do and when the next probe is owed. PURE (no clock, no syscall):
    /// `rss` is asked only on ENTRY to a stall.
    ///
    /// The watch keeps the leader's CPU window across probes ([`spin_window`])
    /// and hands [`input_backlog::classify`] what it saw: `drew` when output
    /// arrived after the oldest unread byte, `spinning` from the window. The
    /// first probe of an episode has no window yet, so a stall is entered on
    /// a later one — the one at [`input_backlog::STALL_AFTER`], which the
    /// first schedules.
    ///
    /// A published stall whose queue aterm DROPPED — the discard before a
    /// restart signal — while its leader still leads stays `stalled` whatever
    /// the queue holds ([`Restart`]); past [`RESTART_GRACE`] it is
    /// republished as survived. The first sign of life ends the EPISODE: the
    /// leader goes, draws, reads what was sent after the drop, or stops
    /// spinning with nothing left unread. The stall is withdrawn, and what is
    /// unread then is judged as a new episode's, with no carry-over.
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
            now,
        );
        let dropped_before = prior.as_ref().and_then(|w| w.dropped);
        let mut was = prior.and_then(|w| w.published);
        let wait = backlog.map_or(Duration::ZERO, |b| b.wait);
        let since = now.checked_sub(wait).unwrap_or(now);
        // THE RESTART HOLD ([`Restart`]): a queue aterm dropped is not a queue
        // the program read. Taken at the first probe that sees the drop (its
        // instant, and whether the leader was spinning then), and let go on
        // the first sign the program is alive. `Some(None)` is that sign.
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
                    spun && !spinning && !backlog.is_some_and(|b| input_backlog::refuses(b, false));
                let alive = leader != fact.restart.leader
                    || activity.last_output.is_some_and(|out| out > at)
                    || activity.read_since_discard
                    || stopped_spinning;
                (!alive).then_some((at, spun))
            });
        // RECOVERED: the episode is over. What is unread now is judged as a
        // new episode's, on what the watch saw, not kept by the old one.
        let recovered = matches!(hold, Some(None));
        let dropped = hold.flatten();
        if recovered {
            was = None;
        }
        let liveness = if was.is_some() {
            Liveness::Published
        } else {
            Liveness::Watched {
                drew: activity.last_output.is_some_and(|at| at > since),
                spinning,
            }
        };
        let word = if dropped.is_some() {
            InputWord::Stalled
        } else {
            input_backlog::classify(backlog, stopped, liveness)
        };
        let survived = dropped.is_some_and(|(at, _)| now >= at + RESTART_GRACE);
        let next_probe = match word {
            InputWord::Unmeasured | InputWord::Clear => None,
            InputWord::Pending => Some(
                Some(since + input_backlog::STALL_AFTER)
                    .filter(|at| *at > now)
                    .unwrap_or(now + RECHECK),
            ),
            InputWord::Typeahead | InputWord::Stalled | InputWord::Stopped => Some(
                dropped
                    .filter(|_| !survived)
                    .map_or(now + RECHECK, |(at, _)| {
                        (at + RESTART_GRACE).min(now + RECHECK)
                    }),
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
                    bytes: backlog.map_or(0, InputBacklog::unread),
                    stopped: word == InputWord::Stopped,
                    rss_mb: rss(),
                    restart: Restart {
                        leader,
                        discards: activity.discards,
                        survived: false,
                    },
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
        // A published stall is always `stalled`/`stopped`, so it always owes
        // a next probe: nothing published is ever left unwatched.
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
                },
            );
        }
        (transition, next_probe)
    }

    /// Session `id`'s published stall.
    pub(crate) fn published(&self, id: u64) -> Option<&InputStallFact> {
        self.watches.get(&id).and_then(|w| w.published.as_ref())
    }

    /// Whether session `id` is being watched (its output wakes probe it).
    pub(crate) fn armed(&self, id: u64) -> bool {
        self.watches.contains_key(&id)
    }

    /// Whether a WAKE-driven probe of `id` must wait: it was probed less than
    /// [`PROBE_MIN_GAP`] ago. An unwatched session is never throttled, so the
    /// hook's first wake always probes.
    pub(crate) fn throttled(&self, id: u64, now: Instant) -> bool {
        self.watches
            .get(&id)
            .is_some_and(|w| now < w.last_probe + PROBE_MIN_GAP)
    }

    /// A throttled wake is DEFERRED, never dropped: pull `id`'s deadline in to
    /// the end of its [`PROBE_MIN_GAP`]. A wake says something moved — the
    /// program drew, or a write landed — and the burst's LAST wake is often the
    /// one inside the gap: measured on a headless instance (2026-09-24), a
    /// frozen program killed with `signal term` drew nothing after its shell's
    /// prompt, and its stall stood until the next [`RECHECK`] instead of a
    /// quarter second.
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

    /// Stop watching `id`; returns what was published, which the caller
    /// must unpublish if the session outlives this (a `--hold` pane).
    pub(crate) fn forget(&mut self, id: u64) -> Option<InputStallFact> {
        self.watches.remove(&id).and_then(|w| w.published)
    }
}

/// One probe of a session's input for [`InputWatches::step`]: the sink's
/// reading, its foreground job's stop state, and the [`Activity`] beside it.
/// `published` says the watch has a stall published; `last_output` is when
/// aterm last read the program's output.
///
/// The leader's CPU is asked only while the job runs and either input is
/// unread or a stall is published, so an idle or stopped session pays for no
/// process lookup. A published stall pays for it with an empty queue too: that
/// is the probe after a restart's discard, and the leader is what tells the
/// program that lived through the signal from the one that ended ([`Restart`]).
/// Only a published stall on a sink that has had a discard asks whether the
/// program has read since ([`SinkWriter::read_since_discard`]).
pub(crate) fn probe(
    sink: &SinkWriter,
    published: bool,
    last_output: Option<Instant>,
) -> (Option<InputBacklog>, bool, Activity) {
    let backlog = sink.input_backlog();
    let master = sink.master();
    let unread = backlog.is_some_and(|b| b.unread() > 0);
    let stopped = unread && fg_stopped(master);
    let discards = sink.discards();
    let activity = Activity {
        cpu: ((unread || published) && !stopped)
            .then(|| fg_cpu(master))
            .flatten(),
        last_output,
        discards,
        read_since_discard: published && discards != 0 && sink.read_since_discard(),
    };
    (backlog, stopped, activity)
}

/// Give `sink` its input watch: install `wake` (the event loop's
/// `Wake::InputWatch` post) as its input hook, and fire it NOW when the
/// program already has input unread. Called on every reader attach
/// (`spawn::attach_reader_inner`); returns whether it fired.
///
/// The hook alone wakes the watch only for a write THROUGH this sink, and
/// that left a hole (S6 review, 2026-09-24): a master adopted across a
/// seamless update — attached at once, or by a deferred reader after the
/// handoff — arrives holding whatever the old process wrote and the program
/// never read. The new sink dates those bytes from its own birth, but no
/// write of this process lands behind them to start a watch: the refusal
/// gate stops every socket write a second later, so only a human keystroke
/// could. The stall was never published — `agent=` kept the frozen screen's
/// verdict and `meta attention` stayed empty — while `status input=` read
/// `stalled` live beside them. That is the incident's own delivery path: the
/// frozen tab runs a build without the watch, and the watch arrives by a live
/// apply.
///
/// Only where the probe can answer — macOS, a tty master — so no other
/// platform pays a wake per keystroke for a reading it cannot take; the
/// caller builds `wake` on every target and it is simply dropped there. The
/// hook is installed BEFORE the queue is read, so a write landing between the
/// two fires it itself, and the explicit wake shares the writes' arming
/// ([`SinkWriter::wake_input_hook`]), so it never doubles one already in
/// flight. First install wins: a re-attach keeps the first attach's hook
/// (same session id, same event loop) and only re-asks the queue.
pub(crate) fn watch_input(sink: &SinkWriter, wake: impl Fn() + Send + Sync + 'static) -> bool {
    if aterm_pty::input_queue_len(sink.master()).is_none() {
        return false;
    }
    sink.install_input_hook(wake);
    sink.input_backlog().is_some_and(|b| b.unread() > 0) && sink.wake_input_hook()
}

/// The four `status` fields, after `integration=`:
/// `input=<word> input_bytes=<n|-> input_wait_ms=<ms|-> fg_rss_mb=<n|->`.
/// `-` = no reading (not macOS, or not a tty); `rss_mb` is the caller's, read
/// only while stalled or stopped.
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

/// `status`'s four fields for `sink`, probed LIVE on the call: the reading,
/// its word, and the foreground RSS only while stalled or stopped. The word
/// is `stalled` exactly while the watch has one PUBLISHED (`was_stalled`):
/// one reading sees the queue, never whether the program is spinning or
/// drawing ([`Liveness`]), so an old byte the watch has not called a stall
/// reads `pending` here — the same word the supervisor, the band and the
/// menu act on, never a second verdict beside them.
///
/// A reading that finds input unread while no watch is looking (`watched`
/// false) WAKES the watch ([`SinkWriter::wake_input_hook`]; S6 review,
/// 2026-09-24): bytes no write through the sink announced — an unseen
/// writer's, or any a future attach path forgets ([`watch_input`]) — would
/// otherwise read `stalled` here, poll after poll, beside an `agent=` and a
/// `meta attention` that never heard of them. A watched session is never
/// woken from here, so a driver polling `status` adds no probes to one.
///
/// `published` is the watch's stall. One whose queue aterm dropped for a
/// restart its leader is living through reads `stalled` with nothing unread
/// ([`Restart`]), from the drop on — not from the watch's next probe, which
/// is what the supervisor's hold waits on — and its live word again from the
/// moment the program reads a key sent since, a reading that wakes the watch
/// to withdraw the stall ([`restart_held_now`]).
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
    // One reading sees the queue, not the program: `stalled` is what the
    // watch has PUBLISHED, never a verdict of this call.
    let liveness = if published.is_some() {
        Liveness::Published
    } else {
        Liveness::Unobserved
    };
    let word = match input_backlog::classify(backlog.as_ref(), stopped, liveness) {
        word @ (InputWord::Stopped | InputWord::Unmeasured) => word,
        word => {
            if published.is_some_and(|fact| restart_held_now(fact, sink)) {
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

/// The local wall-clock `HH:MM` at which `since` fell, given the same moment
/// as an `Instant` (`now`) and as Unix seconds (`now_unix`) and the zone's
/// offset from UTC. `None` without an offset — then the caller says `for
/// <dur>` instead of inventing a clock.
pub(crate) fn since_hhmm(
    since: Instant,
    now: Instant,
    now_unix: i64,
    offset_s: Option<i64>,
) -> Option<String> {
    let offset = offset_s?;
    let ago = i64::try_from(now.saturating_duration_since(since).as_secs()).ok()?;
    let local = (now_unix - ago + offset).rem_euclid(86_400);
    Some(format!("{:02}:{:02}", local / 3600, (local % 3600) / 60))
}

/// The server's attention line for a stall, at most
/// [`crate::session_timeline::META_ATTENTION_KEYED_MAX`] bytes once stored:
///
/// ```text
/// <prog> is frozen: not reading input since <HH:MM> (<n> B queued[, rss <x.y> GB|<n> MB]) — restart it: aterm ctl @<sid> signal term[, then claude --continue]
/// <prog> is stopped with input queued since <HH:MM> — resume it: aterm ctl @<sid> signal cont
/// <prog> is still running after its restart signal, not reading input since <HH:MM>[ (rss …)] — end it: aterm ctl @<sid> signal kill[, then claude --continue]
/// ```
///
/// `program` is `status program=` (`the program` when unresolved), `clock` the
/// [`since_hhmm`] of the stall (`for <dur>` of `waited` without one), and the
/// resume command appears only for the agent whose reader names one
/// ([`aterm_phase::resume_hint`] — `claude --continue` for Claude Code). The
/// third line is a stall whose program SURVIVED `signal term` by
/// [`RESTART_GRACE`] ([`Restart::survived`]): its queue was dropped, so it
/// counts no bytes, and the remedy moves on to the signal no handler can
/// replace.
pub(crate) fn attention_text(
    fact: &InputStallFact,
    program: Option<&str>,
    reader: Option<aterm_phase::Program>,
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
        // Tenths of a GiB, rounded, in integers: 39731 MiB reads `38.8 GB`.
        let tenths = (mb * 10 + 512) / 1024;
        format!("rss {}.{} GB", tenths / 10, tenths % 10)
    });
    let resume = reader
        .and_then(aterm_phase::resume_hint)
        .map_or_else(String::new, |hint| format!(", then {hint}"));
    if fact.restart.survived {
        let rss = rss.map_or_else(String::new, |rss| format!(" ({rss})"));
        return format!(
            "{prog} is still running after its restart signal, not reading input {when}{rss} \
             \u{2014} end it: aterm ctl @{sid} signal kill{resume}"
        );
    }
    let rss = rss.map_or_else(String::new, |rss| format!(", {rss}"));
    format!(
        "{prog} is frozen: not reading input {when} ({} B queued{rss}) \u{2014} restart it: \
         aterm ctl @{sid} signal term{resume}",
        fact.bytes
    )
}

/// [`attention_text`] at `now`, with the wall clock and the zone read here:
/// the ONE composition the server's attention entry and the menu-bar row
/// share, so the row a human clicks says what `meta attention=` says.
pub(crate) fn attention_now(
    fact: &InputStallFact,
    program: Option<&str>,
    reader: Option<aterm_phase::Program>,
    sid: &str,
    now: Instant,
) -> String {
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let clock = since_hhmm(fact.since, now, now_unix, crate::presence::local_offset_s());
    attention_text(
        fact,
        program,
        reader,
        sid,
        clock.as_deref(),
        now.saturating_duration_since(fact.since),
    )
}

/// The band's phase slot for a published stall — `(phase, since clauses,
/// spoken)`, the three things `presence::words` composes a phase from:
///
/// ```text
/// frozen 2m03s · not reading input     frozen, not reading input for 2m03s; restart it
/// frozen 2m03s · survived its restart  frozen, still running after its restart signal; end it with signal kill
/// stopped 41s · input queued           stopped with input queued for 41s; resume it
/// ```
///
/// It takes the slot AHEAD of typed attention (2026-09-24): the incident's
/// band read the supervisor's "answer this box" for 2h41m over a program that
/// could read no answer. During a stall the attention entry is the server's
/// own sentence, cut to fit — the band says the short form and the menu row
/// carries the long one. The duration counts from the oldest unread byte.
pub(crate) fn band_phase(fact: &InputStallFact, now: Instant) -> (String, Vec<String>, String) {
    let dur = crate::presence::fmt_dur(now.saturating_duration_since(fact.since));
    if fact.stopped {
        return (
            "stopped".to_string(),
            vec![dur.clone(), "input queued".to_string()],
            format!("stopped with input queued for {dur}; resume it"),
        );
    }
    if fact.restart.survived {
        // The program lived through `signal term` ([`Restart`]).
        return (
            "frozen".to_string(),
            vec![dur, "survived its restart".to_string()],
            "frozen, still running after its restart signal; end it with signal kill".to_string(),
        );
    }
    (
        "frozen".to_string(),
        vec![dur.clone(), "not reading input".to_string()],
        format!("frozen, not reading input for {dur}; restart it"),
    )
}

/// The identity of one stall EPISODE, for the menu herald's notification key:
/// a hash of the instant its oldest unread byte was accepted. [`InputWatches`]
/// fixes `since` at entry and keeps it through a stalled ⇄ stopped change, so
/// one episode is one key — its row's text can move (a `for <dur>` clause
/// without a zone, a program name resolving late) without a second
/// notification, and a new episode after a thaw is a new key.
pub(crate) fn episode_key(fact: &InputStallFact) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    fact.since.hash(&mut h);
    h.finish()
}

/// The menu-bar row's stall half ([`crate::status_item::SessionRow::input_stall`])
/// for a session whose timeline carries a published stall: the server's own
/// attention words and the episode's key. Built for EVERY program — a frozen
/// `vim` is as stuck as a frozen Claude Code — and the resume command rides
/// it only where the reader names one.
pub(crate) fn menu_row(
    fact: &InputStallFact,
    program: Option<&str>,
    reader: Option<aterm_phase::Program>,
    sid: &str,
    now: Instant,
) -> crate::status_item::InputStallRow {
    crate::status_item::InputStallRow {
        text: attention_now(fact, program, reader, sid, now),
        key: episode_key(fact),
    }
}

impl crate::App {
    /// Probe the input watches that are owed a probe and publish what moved.
    /// `only = None` is the event loop's turn: every session whose deadline
    /// has come. `only = Some(id)` is a WAKE for one session — its input hook
    /// fired, or it produced output while watched — and is skipped inside
    /// [`PROBE_MIN_GAP`] of its last probe — deferred to the end of that gap,
    /// never dropped ([`InputWatches::defer`]).
    /// Returns the sessions whose publication moved.
    ///
    /// Each probe REARMS the sink's input hook first, so a write racing the
    /// probe wakes the watch again rather than falling between the two. The
    /// probe is the sink's lock-free reading (a `tcgetattr` and two ioctls,
    /// never the fd lock a parked writer holds), plus one `proc_pidinfo` for
    /// the stop state while bytes are unread and one more for the RSS on
    /// entry — all on the event loop, all non-blocking.
    pub(crate) fn observe_input_stalls(&mut self, now: Instant, only: Option<u64>) -> Vec<u64> {
        let ids = match only {
            Some(id) if self.session_status.inputs.throttled(id, now) => {
                self.session_status.inputs.defer(id);
                return Vec::new();
            }
            Some(id) => vec![id],
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
                Transition::None => continue,
                Transition::Entered(fact) | Transition::Updated(fact) => Some(fact),
                Transition::Left => None,
            };
            self.publish_input_stall(id, &ctx, fact, now);
            moved.push(id);
        }
        moved
    }

    /// Stop watching a session whose PTY ended, unpublishing a stall it still
    /// shows (a `--hold` pane outlives its program; a frozen badge on a dead
    /// pane would never clear).
    pub(crate) fn retire_input_watch(&mut self, session: u64) {
        if self.session_status.forget_input(session).is_none() {
            return;
        }
        if let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) {
            self.publish_input_stall(session, &ctx, None, Instant::now());
        }
    }

    /// A `Wake::InputWatch` the pre-Commit gate SWALLOWED
    /// (`App::drops_wake_before_commit`) releases its sink's input hook, as the
    /// dropped Output wake releases its coalescing latch: the hook fires at most
    /// once until a probe rearms it, and the probe it asked for will never run.
    ///
    /// WHY (whole-branch review, 2026-09-25): an overlap live upgrade adopts
    /// each session's master, and the reader's prepare runs
    /// [`watch_input`] BEFORE the proof — so a session that arrives with
    /// input unread fires its hook while `incoming_handoff_pending` holds, and
    /// the wake is dropped. Nothing rearmed the hook after that: every later
    /// write, the refusal's wake and `status`'s wake all found it already
    /// fired, and the stall — the frozen Claude Code's Enter, delivered by the
    /// very live apply that brings this watch — was never published
    /// (`agent=`, the server's attention, the band and the menu row), while
    /// `status input=` read it live beside them. Any other wake is left alone.
    pub(crate) fn release_input_wake_dropped_before_commit(&self, ev: &crate::Wake) {
        if let crate::Wake::InputWatch { session } = ev
            && let Some(s) = self.pool.get(*session)
        {
            s.ctx.sink.rearm_input_hook();
        }
    }

    /// At Commit, probe every ADOPTED session's input (`handoff_local_id` set)
    /// once, as its attach wake would have: the wake that attach posted was
    /// dropped before Commit ([`Self::release_input_wake_dropped_before_commit`]),
    /// and bytes the predecessor queued between the reader's prepare and
    /// Commit are announced by no wake at all. Each probe rearms the hook
    /// first ([`Self::observe_input_stalls`]), so a session with nothing
    /// unread is left idle — no watch, no deadline — and one with input
    /// unread is watched from here, its bytes dated from the sink's birth.
    /// Returns the sessions whose publication moved.
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

    /// Publish (`Some`) or clear (`None`) session `id`'s stall, in lockstep:
    /// the timeline's effective agent word, the server's attention entry, and
    /// the presence, chrome and menu refreshes that carry them — plus a
    /// subscriber notify when either moved, so `EVENT <local> agent …` and
    /// `EVENT <local> meta …` push now.
    fn publish_input_stall(
        &mut self,
        id: u64,
        ctx: &std::sync::Arc<SessionCtx>,
        fact: Option<InputStallFact>,
        now: Instant,
    ) {
        let (agent_moved, text) = {
            let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let text = fact.as_ref().map(|fact| {
                attention_now(
                    fact,
                    tl.agent().program.as_deref(),
                    tl.agent().reader,
                    ctx.self_id.as_str(),
                    now,
                )
            });
            (tl.set_input(fact), text)
        };
        let attention_moved =
            crate::session_timeline::write_server_attention(ctx, SERVER_ATTENTION_OWNER, text);
        if (agent_moved || attention_moved) && self.subscribers.any() {
            self.subscribers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .notify(id);
        }
        if attention_moved {
            // What `Wake::MetaChanged` refreshes for a driver's `meta set
            // attention`: the tab labels and the menu-bar item.
            self.refresh_meta_dependent_chrome(id);
            self.refresh_operator_status_item();
        }
        self.refresh_presence_session(id, false);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use super::*;

    /// Past [`input_backlog::REFUSE_AFTER`] with margin: the plan's 1.1 s.
    const TICK: Duration = Duration::from_millis(1100);

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

    /// A real pty pair with a RAW slave nobody reads — the incident's shape.
    /// Both ends close-on-exec at once, so a child another test spawns in this
    /// multi-threaded harness cannot inherit the slave and hold it open (the
    /// aterm-pty test module measured an inheritable openpty slave SIGHUP an
    /// unrelated test's child). The master is O_NONBLOCK, as production's is.
    /// Returns `(master, slave)`; the caller closes both.
    #[cfg(target_os = "macos")]
    pub(crate) fn raw_pty_pair() -> (i32, i32) {
        let (mut master, mut slave) = (-1i32, -1i32);
        // SAFETY: `openpty` fills the two out-params; the name/termios/winsize
        // pointers are optional and null.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty");
        aterm_pty::set_cloexec(master, true).expect("cloexec master");
        aterm_pty::set_cloexec(slave, true).expect("cloexec slave");
        // SAFETY: `libc::termios` is plain integer fields plus a byte array;
        // zeroed is a valid value, and `tcgetattr` fills it on the open slave.
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

    /// Everything the slave has queued, read in ONE `read()` — the way the
    /// incident's program read its Enter and the supervisor's key together.
    #[cfg(target_os = "macos")]
    pub(crate) fn slave_read_all(slave: i32) -> Vec<u8> {
        let mut buf = [0u8; 256];
        // SAFETY: `slave` is open and `buf` is a writable buffer of its length.
        let n = unsafe { libc::read(slave, buf.as_mut_ptr().cast(), buf.len()) };
        assert!(n >= 0, "read(slave)");
        buf[..usize::try_from(n).unwrap_or(0)].to_vec()
    }

    /// The gate's verb set is a SUBSET of the halt's (`fabric::is_pty_reaching`)
    /// over every verb the table ships, and what it leaves out is EXACTLY the
    /// remedies — `resize signal close pane tab confirm` — plus `invoke` of an action
    /// that writes no input. A verb added to the halt set later lands in one
    /// list or the other on purpose, or this fails.
    #[test]
    fn the_input_writing_set_is_the_halt_set_minus_the_remedies() {
        let mut exempt = Vec::new();
        for spec in aterm_types::control_verbs::VERBS {
            let writes = is_input_writing(spec.name, "Paste");
            if writes {
                assert!(
                    crate::fabric::is_pty_reaching(spec.name),
                    "{}: an input-writing verb the halt does not cover",
                    spec.name
                );
            } else if crate::fabric::is_pty_reaching(spec.name) {
                exempt.push(spec.name);
            }
        }
        exempt.sort_unstable();
        assert_eq!(
            exempt,
            ["close", "confirm", "pane", "resize", "signal", "tab"]
        );
        // `invoke` is input-writing only for an action that writes input.
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
        // The halt set holds verbs the table does not (`operator-propose-bin`
        // is a frame, not a row); the gate covers it too.
        assert!(is_input_writing("operator-propose-bin", ""));
        assert!(!is_input_writing("status", ""));
    }

    /// Only `Paste` writes input, over every action the menu carries (every
    /// variant has a tag, which `from_tag` maps back).
    #[test]
    fn only_paste_writes_pty_input() {
        let actions: Vec<_> = (0..=255isize).filter_map(MenuAction::from_tag).collect();
        assert!(
            actions.len() >= 63,
            "every action has a tag: {}",
            actions.len()
        );
        let writing: Vec<_> = actions
            .into_iter()
            .filter(|a| a.writes_pty_input())
            .collect();
        assert_eq!(writing, [MenuAction::Paste]);
    }

    /// The refusal starts `ERR busy input-unread`, so a driver's existing
    /// `is_err("busy")` back-off retries it (any other `ERR` is a hard failure
    /// to the supervisor), and it names the facts and the way out.
    #[test]
    fn refusal_text_is_a_busy_refusal_naming_the_facts_and_the_remedy() {
        let b = backlog(1, TICK, false);
        let line = refusal_text(&b, InputWord::Pending, false);
        assert!(line.starts_with("ERR busy input-unread "), "{line}");
        // `CtlReply::is_err("busy")` is `err_text().starts_with("ERR busy")`.
        assert!(line.starts_with("ERR busy"));
        assert!(
            line.ends_with(")\n") && line.matches('\n').count() == 1,
            "{line}"
        );
        assert!(
            line.starts_with("ERR busy input-unread bytes=1 wait_ms=1100 input=pending ("),
            "{line}"
        );
        assert!(line.contains("queued 1s ago"), "{line}");
        // Not yet a stall: one remedy, and it is waiting.
        assert!(line.contains("; retry in a moment)"), "{line}");
        assert!(!line.contains("signal"), "{line}");
        // A refused `^C`: a raw program's ^C is a byte the gate refuses; the
        // signal is not.
        let line = refusal_text(&b, InputWord::Pending, true);
        assert!(line.ends_with("; interrupt it: signal int)\n"), "{line}");
        // The spill counts toward bytes=.
        let spilled = InputBacklog {
            spilled: Some(5),
            ..backlog(2, Duration::from_secs(700), false)
        };
        let line = refusal_text(&spilled, InputWord::Stalled, false);
        assert!(
            line.starts_with("ERR busy input-unread bytes=7 wait_ms=700000 input=stalled ("),
            "{line}"
        );
        assert!(line.contains("queued 11m40s ago"), "{line}");
        // A published stall: the attention line's remedy (a program that lives
        // through it earns `signal kill`, below).
        assert!(line.ends_with("; restart it: signal term)\n"), "{line}");
        // Stopped: the remedy is `signal cont`, not a restart.
        let line = refusal_text(&b, InputWord::Stopped, false);
        assert!(
            line.contains("input=stopped (the program is stopped"),
            "{line}"
        );
        assert!(line.contains("signal cont"), "{line}");
        assert!(!line.contains("signal term"), "{line}");
        // No wire word the gate prints is `frozen`: that is `path=frozen`'s.
        for word in [InputWord::Pending, InputWord::Stalled, InputWord::Stopped] {
            assert!(!refusal_text(&b, word, false).contains("input=frozen"));
        }
        // A stall held through a restart ([`Restart`]): the same busy shape
        // and word, with nothing unread, the reason and `signal kill`.
        let line = restart_refusal_text(&backlog(0, Duration::ZERO, false));
        assert!(
            line.starts_with("ERR busy input-unread bytes=0 wait_ms=0 input=stalled ("),
            "{line}"
        );
        assert!(
            line.ends_with(")\n") && line.matches('\n').count() == 1,
            "{line}"
        );
        assert!(
            line.contains("still running after its restart signal"),
            "{line}"
        );
        assert!(line.contains("end it: signal kill"), "{line}");
        assert!(!line.contains("signal term"), "{line}");
    }

    /// A sink over no tty has no reading, so it never refuses and never stops
    /// a re-press — the Linux/Windows answer, and every pipe-backed test's.
    #[test]
    fn no_reading_refuses_nothing() {
        let ctx = crate::session_store::test_handle(1).ctx;
        assert_eq!(refusal(&ctx, "key", "down", false), None);
        assert!(!repress_would_queue(&ctx.sink));
        assert!(!fg_stopped(-1));
        assert_eq!(fg_rss_mb(-1), None);
    }

    /// `pid_stopped` reads a real SIGSTOP and SIGCONT off a child it did not
    /// stop itself.
    #[test]
    #[cfg(target_os = "macos")]
    fn pid_stopped_reads_a_stopped_child_and_its_resume() {
        use std::time::Instant;
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = libc::pid_t::try_from(child.id()).expect("pid");
        let until = |want: bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
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
        // Not a process: no answer rather than a guess.
        assert_eq!(pid_stopped(0), None);
        assert_eq!(pid_stopped(-5), None);
    }

    /// `pid_rss_mb` reads this test process's own resident set.
    #[test]
    #[cfg(target_os = "macos")]
    fn pid_rss_mb_reads_a_resident_set() {
        // SAFETY: `getpid` is a side-effect-free libc getter.
        let me = unsafe { libc::getpid() };
        let rss = pid_rss_mb(me).expect("our own task info");
        assert!(rss > 0, "{rss} MiB");
        assert_eq!(pid_rss_mb(0), None);
    }

    /// THE INCIDENT, AT THE GATE. A raw program that has stopped reading holds
    /// the human's Enter; 1.1 s later a driver's `key down` is refused and
    /// nothing reaches the queue — the slave still reads exactly `\r`. The
    /// override queues, the remedies stay open, and a re-press is withheld.
    /// The refusal wakes the input watch once per arming (S6 review): the
    /// gate refuses every write that would otherwise have started one.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_key_behind_a_stale_byte_is_refused_and_writes_nothing() {
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
        // Nothing queued: nothing refused, and a re-press may press.
        assert_eq!(refusal(&ctx, "key", "down", false), None);
        assert!(!repress_would_queue(&ctx.sink));

        ctx.sink
            .write_frame_nonparking(b"\r")
            .expect("the human's Enter");
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "the Enter landed");
        // The watch's probe rearms the hook; nothing unread is announced again.
        ctx.sink.rearm_input_hook();
        // Young input is a program busy for a moment, not a stall.
        assert_eq!(refusal(&ctx, "key", "down", false), None);
        assert!(repress_would_queue(&ctx.sink), "the Enter is unread");
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "a pass wakes nothing");
        std::thread::sleep(TICK);

        let refused = refusal(&ctx, "key", "down", false).expect("refused");
        assert!(
            refused.starts_with("ERR busy input-unread bytes=1 "),
            "{refused}"
        );
        assert!(refused.contains(" input=pending "), "{refused}");
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            2,
            "the refusal woke the watch"
        );
        for verb in ["send", "ctrl", "feed", "paste", "turn", "mouse", "hwkey"] {
            assert!(refusal(&ctx, verb, "x", false).is_some(), "{verb}");
        }
        assert!(refusal(&ctx, "invoke", "Paste", false).is_some());
        // The override, and the remedies, pass.
        assert_eq!(refusal(&ctx, "key", "down", true), None);
        for verb in ["signal", "resize", "close", "status", "text"] {
            assert_eq!(refusal(&ctx, verb, "term", false), None, "{verb}");
        }
        assert_eq!(refusal(&ctx, "invoke", "SelectAll", false), None);
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            2,
            "eight more refusals, no probe between them: no second wake"
        );
        // A refusal wrote nothing: the program, reading at last, reads the
        // human's Enter and nothing behind it.
        assert_eq!(slave_read_all(slave), b"\r");
        assert_eq!(refusal(&ctx, "key", "down", false), None, "read: clear");
        assert!(!repress_would_queue(&ctx.sink));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    /// The bytes the gate judges are the bytes the seam writes: `key` and
    /// `ctrl` through the encoder under the session's keyboard mode, `feed`
    /// decoded, `send` raw. Under the kitty protocol `ctrl+c` is a CSI
    /// sequence, not `^C`, so it is input like any other key.
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
        ] {
            assert_eq!(
                written_bytes(verb, rest, legacy).as_deref(),
                Some(want),
                "{verb} {rest:?}"
            );
        }
        let kitty =
            written_bytes("key", "ctrl+c", KeyboardMode::DISAMBIGUATE_ESC_CODES).expect("encodes");
        assert_ne!(kitty, b"\x03", "the kitty protocol reports ctrl+c as a key");
        for (verb, rest) in [
            ("key", "no-such-key"),
            ("ctrl", "cc"),
            ("feed", "zz"),
            ("paste", "x"),
            ("mouse", "click 1 1"),
            ("turn", "x"),
        ] {
            assert_eq!(written_bytes(verb, rest, legacy), None, "{verb} {rest}");
        }
    }

    /// THE REMEDY IS NOT REFUSED (S4 review, 2026-09-24). A cbreak program —
    /// ICANON off, ISIG on: readline between keys, `watch`, `htop` — has left
    /// a `q` unread for over a second, so a driver's `key down` is refused as
    /// ever. But `key ctrl+c`, `ctrl c`, `ctrl z` and `feed 03` are signals
    /// the line discipline raises as they are written: they never queue
    /// behind the `q`, and writing one flushes it — the gate lets them
    /// through, and the queue reads 0 after. In RAW mode (ISIG off) the same
    /// `^C` is a byte that would be read after the `q`, so it is refused, and
    /// the refusal names `signal int`. Fails before the exemption: the cbreak
    /// `key ctrl+c` answered `ERR busy input-unread`.
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
        std::thread::sleep(TICK);

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
            assert_eq!(refusal(&ctx, verb, rest, false), None, "{verb} {rest}");
        }
        ctx.sink.write_frame_nonparking(b"\x03").expect("^C");
        assert_eq!(
            ctx.sink.input_backlog().map(|b| b.queued),
            Some(0),
            "the ^C was a signal and flushed the `q`: nothing was read behind it"
        );
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);

        // Raw: the same `^C` is a byte behind the `q`, so it is refused.
        let (master, slave) = raw_pty_pair();
        let ctx = crate::session_store::test_handle_on(9, master).ctx;
        ctx.sink.note_master_nonblocking(true);
        ctx.sink
            .write_frame_nonparking(b"q")
            .expect("a human's key");
        std::thread::sleep(TICK);
        let refused = refusal(&ctx, "key", "ctrl+c", false).expect("raw ^C is input");
        assert!(refused.contains("signal int"), "{refused}");
        assert!(refusal(&ctx, "feed", "03", false).is_some());
        assert_eq!(slave_read_all(slave), b"q", "nothing queued behind it");
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    /// A YIELDING TURN ASKS THE GATE AGAIN BEFORE ITS FIRST BYTE (S4 review,
    /// 2026-09-24). The dispatch asks once, when the turn arrives; a turn that
    /// arrives within a second of the human's Enter into a frozen program is
    /// let in, and its yield — parked until the human's ribbon exhales, τ =
    /// 2 s — can then outlast [`input_backlog::REFUSE_AFTER`] with that Enter
    /// still unread. Typing then would be the incident's shape exactly. Here
    /// the ribbon reads 0.40 against a 0.20 floor (a crossing at 2·ln 2 ≈
    /// 1.39 s): the turn yields, is refused `ERR busy input-unread`, and types
    /// and presses NOTHING — the slave reads exactly the human's `\r`. Without
    /// the second ask it pastes `hi` and presses Enter behind it. CONTROL: once
    /// the program has read, the same yielding turn types and presses.
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
        let turn = |start: f32, request: &str| -> String {
            let reads = Cell::new(0u32);
            let momentum = || {
                reads.set(reads.get() + 1);
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
                &crate::subscribe::new_registry(),
                &h.ctx,
                &crate::control::TurnIo {
                    paste: &paste,
                    press: &press,
                    key: &key,
                    momentum: &momentum,
                    driver: None,
                },
            )
        };

        h.ctx
            .sink
            .write_frame_nonparking(b"\r")
            .expect("the human's Enter");
        assert_eq!(
            refusal(&h.ctx, "turn", "", false),
            None,
            "the turn arrives while the Enter is young: the dispatch lets it in"
        );
        let reply = turn(0.40, "yield=0.20 timeout=5000 idle=1 -- hi");
        assert!(
            reply.starts_with("ERR busy input-unread bytes=1 "),
            "{reply}"
        );
        assert_eq!(slave_read_all(slave), b"\r", "nothing typed behind it");

        // CONTROL: nothing unread, so the second ask passes and the turn types.
        let reply = turn(
            0.25,
            "yield=0.20 timeout=5000 idle=1 submit_window=150 presses=1 -- hi",
        );
        assert!(reply.starts_with("OK "), "{reply}");
        assert_eq!(slave_read_all(slave), b"hi\r");
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    // -- THE WATCH ------------------------------------------------------------

    /// A reading: `queued` raw bytes, the oldest `wait_s` seconds old, and
    /// `output` bytes of the program's own output aterm has not read.
    fn reading(queued: usize, wait_s: u64, output: usize) -> InputBacklog {
        InputBacklog {
            output_backlog: Some(output),
            ..backlog(queued, Duration::from_secs(wait_s), false)
        }
    }

    /// What the probe sees of the incident's program: leader 4242, having
    /// burned `burned` of CPU since its window started — a whole core, when
    /// `burned` is the wall time since then. Nothing drawn.
    fn spin(burned: Duration) -> Activity {
        Activity {
            cpu: Some((4242, u64::try_from(burned.as_nanos()).expect("small"))),
            last_output: None,
            discards: 0,
            read_since_discard: false,
        }
    }

    /// Start `id`'s CPU window [`RECHECK`] before `now` with a young byte, so
    /// a step at `now` passing `spin(RECHECK)` judges a spinning window.
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

    /// Nothing unread arms nothing: no watch, no deadline, no transition —
    /// the idle machine's case, and every probe that finds the program caught
    /// up. No reading at all (`-`, not macOS or not a tty) is the same.
    #[test]
    fn step_clear_arms_nothing() {
        let mut w = InputWatches::default();
        let t0 = Instant::now();
        for b in [Some(reading(0, 0, 0)), None] {
            let (t, next) = w.step(
                1,
                b.as_ref(),
                false,
                Activity::default(),
                || panic!("no rss"),
                t0,
            );
            assert_eq!((t, next), (Transition::None, None));
            assert!(!w.armed(1));
            assert_eq!(w.next_wake(), None);
        }
    }

    /// Unread raw input younger than a stall is PENDING: nothing published,
    /// and the next probe is the instant its oldest byte turns
    /// [`input_backlog::STALL_AFTER`] old — not a poll.
    #[test]
    fn step_pending_schedules_the_stall_instant() {
        let mut w = InputWatches::default();
        let t0 = Instant::now();
        let (t, next) = w.step(
            1,
            Some(&reading(1, 3, 0)),
            false,
            Activity::default(),
            || panic!(),
            t0,
        );
        assert_eq!(t, Transition::None);
        assert_eq!(next, Some(t0 + Duration::from_secs(7)));
        assert!(w.armed(1));
        assert_eq!(w.next_wake(), next);
        assert_eq!(w.published(1), None);
        assert!(w.due(t0).is_empty());
        assert_eq!(w.due(t0 + Duration::from_secs(7)), [1]);
    }

    /// ENTRY needs aterm's read of the program's output caught up: a program
    /// blocked writing to an aterm that is behind is waiting for aterm. Held
    /// there, the probe re-checks every [`RECHECK`] rather than at a stall
    /// instant already past (which would spin the loop). Once published, a
    /// stall STAYS on `wait` alone, whatever the output backlog.
    #[test]
    fn step_entry_needs_output_caught_up_and_staying_needs_only_the_wait() {
        let mut w = InputWatches::default();
        let t0 = Instant::now();
        let (t, next) = w.step(
            1,
            Some(&reading(1, 12, 64)),
            false,
            spin(Duration::ZERO),
            || panic!(),
            t0,
        );
        assert_eq!(t, Transition::None, "aterm is behind: pending");
        assert_eq!(next, Some(t0 + RECHECK));

        let mut rss_reads = 0;
        let t1 = t0 + RECHECK;
        let (t, next) = w.step(
            1,
            Some(&reading(1, 17, 0)),
            false,
            spin(RECHECK),
            || {
                rss_reads += 1;
                Some(39_731)
            },
            t1,
        );
        let Transition::Entered(fact) = t else {
            panic!("entered: {t:?}")
        };
        assert_eq!(fact.word, InputWord::Stalled);
        assert_eq!(fact.since, t1 - Duration::from_secs(17));
        assert_eq!(
            (fact.bytes, fact.stopped, fact.rss_mb),
            (1, false, Some(39_731))
        );
        assert_eq!(next, Some(t1 + RECHECK));
        assert_eq!(w.published(1), Some(&fact));

        // Staying: aterm falls behind again — the stall does not flap.
        let t2 = t1 + RECHECK;
        let (t, _) = w.step(
            1,
            Some(&reading(2, 22, 512)),
            false,
            Activity::default(),
            || panic!(),
            t2,
        );
        assert_eq!(t, Transition::None);
        assert_eq!(w.published(1), Some(&fact), "entry values are kept");
        assert_eq!(rss_reads, 1, "the RSS is read once, on entry");
    }

    /// A STOPPED job with input queued is published at any age, and a stall
    /// that becomes a stop (or a stop that resumes into a stall) is an UPDATE
    /// of the same episode: its date and RSS carry over.
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

    /// The program reads: the stall LEAVES and, with nothing unread, the watch
    /// ends. Unread input that is younger again (it read the old byte, a new
    /// one came) also leaves — that is a new episode, still watched. So does a
    /// reading that went dark: a published stall is never left standing.
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

    /// A DROP IS NOT A READ (whole-branch review, third round, 2026-09-25):
    /// the stall's queue emptied by aterm's own discard — the one before
    /// `signal term` — while the leader it was published for still leads is
    /// HELD, `stalled` whatever the queue holds, and past [`RESTART_GRACE`]
    /// it is republished as survived, the remedy `signal kill`. It is let go
    /// when the leader goes (the program ended and the shell has the tty) or
    /// draws. The negative control is the fix's absence: the same empty queue
    /// with no discard is the program reading, and the stall leaves.
    #[test]
    fn step_holds_a_stall_whose_queue_aterm_dropped_until_its_leader_goes() {
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
            let Transition::Entered(fact) = t else {
                panic!("{t:?}")
            };
            assert_eq!(
                fact.restart,
                Restart {
                    leader: Some(4242),
                    discards: 0,
                    survived: false,
                },
                "the episode's leader and the sink's discard count"
            );
        };
        // The leader after the discard (the count moved), still spinning: a
        // whole core since `primed` started its window, whenever it is read.
        let after_drop = |discards: u64, out: Option<Instant>, now: Instant| Activity {
            discards,
            last_output: out,
            ..spin(now.duration_since(t0.checked_sub(RECHECK).expect("uptime")))
        };
        let t1 = t0 + Duration::from_secs(1);

        // The negative control: emptied by a READ, the stall leaves.
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

        // Emptied by aterm: held, and looked at again when the grace ends.
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
        // Keys typed into it meanwhile do not make it young again.
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
        // Survived: republished, with the episode's date and its RSS.
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
        // `signal kill` again, a second discard: nothing more moves.
        let (t, _) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after_drop(2, None, at + RECHECK),
            || panic!(),
            at + RECHECK,
        );
        assert_eq!(t, Transition::None);
        // The leader went: the shell's job leads, or nothing readable does.
        for gone in [Some((5151, 0)), None] {
            let mut w2 = InputWatches::default();
            entered(&mut w2);
            let _ = w2.step(
                1,
                Some(&reading(0, 0, 0)),
                false,
                after_drop(1, None, t1),
                || None,
                t1,
            );
            let (t, _) = w2.step(
                1,
                Some(&reading(0, 0, 0)),
                false,
                Activity {
                    cpu: gone,
                    ..after_drop(1, None, t1 + Duration::from_millis(300))
                },
                || panic!(),
                t1 + Duration::from_millis(300),
            );
            assert_eq!(t, Transition::Left, "leader now {gone:?}");
        }
        // It drew after the drop: alive, let go. Output from before is not.
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
        // No leader at entry (a sleeper published with its CPU read
        // failing): no hold is ever taken, even for a leader seen later.
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

    /// A HELD STALL IS LET GO WHEN ITS PROGRAM SHOWS IT IS ALIVE (whole-branch
    /// review, fourth round, 2026-09-25). The reviewer's program lived through
    /// `signal term`, stopped spinning and read every key sent to it without
    /// drawing, and it stayed `stalled` for good, told `signal kill`. Two more
    /// signs end the hold now ([`Restart`]):
    ///
    /// * a READ of a byte sent after the drop, spinning or not;
    /// * the spin STOPPING: a window judged not spinning after the hold began
    ///   spinning, with nothing sent since left unread for
    ///   [`input_backlog::REFUSE_AFTER`].
    ///
    /// The negative controls are the incident and its neighbours. A leader
    /// still spinning that reads nothing stays held. So does one that stopped
    /// spinning but leaves a byte sent since unread, because it is not
    /// reading. One that was asleep when the hold began has no spin to stop,
    /// so it stays held until it reads. A recovered episode is over. With
    /// nothing unread the watch ends, and a stall behind the recovery is
    /// entered as a new episode, with its own restart record.
    #[test]
    fn step_lets_a_held_stall_go_when_its_program_reads_or_stops_spinning() {
        let t0 = Instant::now();
        let base = t0.checked_sub(RECHECK).expect("uptime");
        // The leader after the discard: `burned` of CPU since `primed`'s
        // window began (a whole core when that is `now - base`), and whether
        // a byte sent after the drop has been read.
        let after_drop = |burned: Duration, read: bool| Activity {
            discards: 1,
            read_since_discard: read,
            ..spin(burned)
        };
        let core = |now: Instant| now.duration_since(base);
        let t1 = t0 + Duration::from_secs(1);
        let t2 = t1 + Duration::from_secs(2);
        // Entered spinning at t0, and HELD from t1, spinning.
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
            assert!(w.published(1).is_some());
        };

        // The incident: still spinning, nothing read. Held.
        let mut w = InputWatches::default();
        held(&mut w);
        let (t, _) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after_drop(core(t2), false),
            || panic!(),
            t2,
        );
        assert_eq!(t, Transition::None, "spinning and reading nothing");
        assert!(w.published(1).is_some());

        // It READ a byte sent after the drop, still spinning: let go, and
        // with nothing unread the watch ends.
        let mut w = InputWatches::default();
        held(&mut w);
        let (t, next) = w.step(
            1,
            Some(&reading(0, 0, 0)),
            false,
            after_drop(core(t2), true),
            || panic!(),
            t2,
        );
        assert_eq!((t, next), (Transition::Left, None), "it read");
        assert!(!w.armed(1));

        // Its spin STOPPED (no CPU since t1), nothing unread: let go. This is
        // the reviewer's program, asleep in `read` with nothing sent to it.
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
        // …and a key just sent, not yet due, does not keep it: the episode
        // is over, and the key is a new one's, pending.
        let mut w = InputWatches::default();
        held(&mut w);
        let young = InputBacklog {
            wait: Duration::from_millis(200),
            ..reading(1, 0, 0)
        };
        let (t, next) = w.step(
            1,
            Some(&young),
            false,
            after_drop(core(t1), false),
            || panic!(),
            t2,
        );
        assert_eq!(t, Transition::Left);
        assert_eq!(
            next,
            Some(t2 + input_backlog::STALL_AFTER - Duration::from_millis(200))
        );
        assert!(w.published(1).is_none());
        // Stopped spinning, but a byte sent since has waited a second unread:
        // it is not reading. Held.
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

        // Asleep when the hold began (entered on the quiet threshold): no
        // spin to stop, so a quiet window lets nothing go; a read does.
        let asleep = |discards: u64, read: bool| Activity {
            cpu: Some((4242, 0)),
            last_output: None,
            discards,
            read_since_discard: read,
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
        let Transition::Entered(fact) = t else {
            panic!("{t:?}")
        };
        assert_eq!(fact.restart.leader, Some(4242));
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

        // A new stall behind a recovery: it read one key, and one sent later
        // has waited twelve seconds while it spins. A NEW episode, dated by
        // that key, with its own RSS and a restart record at today's count.
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
                survived: false,
            }
        );
    }

    /// NOT EVERY OLD BYTE IS A FROZEN PROGRAM (whole-branch review,
    /// 2026-09-25). The three shapes measured on Darwin ptys — each with a
    /// cbreak byte unread at twelve seconds and aterm's output queue empty,
    /// each published "frozen … restart it" before this gate — as the watch
    /// sees them across two probes, and the incident beside them:
    ///
    /// * `less` in front of a slow `git log -S`, `q` typed: the leader
    ///   asleep, nothing drawn → `pending`, until [`input_backlog::
    ///   QUIET_STALL_AFTER`] (a deadlock looks the same from outside);
    /// * zsh inside a slow completion widget, a key typed: the LEADER (zsh)
    ///   asleep while its child works → `pending`;
    /// * a cbreak progress line redrawn every 100 ms, `q` typed: drawing →
    ///   `pending` at any age and any CPU;
    /// * the incident: spinning, nothing drawn → `stalled` at ten seconds.
    #[test]
    fn step_enters_only_a_program_seen_spinning_and_silent() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(12);
        let asleep = |burned_ms: u64| Activity {
            cpu: Some((4242, burned_ms * 1_000_000)),
            last_output: None,
            discards: 0,
            read_since_discard: false,
        };
        // Two probes of one shape: the arming one at t0 (a young byte), the
        // stall-instant one at t1 (the byte twelve seconds old).
        let probe = |first: Activity, second: Activity, wait_s: u64| {
            let mut w = InputWatches::default();
            let _ = w.step(1, Some(&reading(1, 0, 0)), false, first, || None, t0);
            let (t, next) = w.step(1, Some(&reading(1, wait_s, 0)), false, second, || None, t1);
            (t, next, w)
        };

        // less / zsh: 3 ms of CPU across twelve seconds, nothing drawn.
        let (t, next, w) = probe(asleep(0), asleep(3), 12);
        assert_eq!(t, Transition::None, "asleep: not published");
        assert_eq!(next, Some(t1 + RECHECK), "still watched");
        assert!(w.published(1).is_none());
        // …until it has slept past the quiet threshold.
        let quiet = input_backlog::QUIET_STALL_AFTER.as_secs();
        let (t, _, _) = probe(asleep(0), asleep(3), quiet);
        assert!(matches!(t, Transition::Entered(_)), "{t:?}");

        // The progress line: output after the oldest byte, spinning or not.
        let drawing = |cpu: Activity| Activity {
            last_output: Some(t1 - Duration::from_millis(100)),
            ..cpu
        };
        for cpu in [asleep(40), spin(Duration::from_secs(12))] {
            let (t, _, _) = probe(spin(Duration::ZERO), drawing(cpu), 3600);
            assert_eq!(t, Transition::None, "a program that draws is alive");
        }
        // Output from BEFORE the oldest byte is not drawing since it.
        let stale = Activity {
            last_output: Some(t1 - Duration::from_secs(13)),
            ..spin(Duration::from_secs(12))
        };
        let (t, _, _) = probe(spin(Duration::ZERO), stale, 12);
        assert!(matches!(t, Transition::Entered(_)), "{t:?}");

        // The incident: a core burned across the window, nothing drawn.
        let (t, _, _) = probe(spin(Duration::ZERO), spin(Duration::from_secs(12)), 12);
        assert!(matches!(t, Transition::Entered(_)), "{t:?}");
        // …but not on its first probe: one reading has no window.
        let (t, _) = InputWatches::default().step(
            1,
            Some(&reading(1, 12, 0)),
            false,
            spin(Duration::from_secs(12)),
            || None,
            t1,
        );
        assert_eq!(t, Transition::None, "no window yet");
        // A different leader restarts the window: its CPU is not the old one's.
        let other = Activity {
            cpu: Some((5151, 12_000_000_000)),
            last_output: None,
            discards: 0,
            read_since_discard: false,
        };
        let (t, _, _) = probe(spin(Duration::ZERO), other, 12);
        assert_eq!(t, Transition::None, "a new foreground job has no window");
        // No CPU reading at all is no evidence of spinning.
        let (t, _, _) = probe(Activity::default(), Activity::default(), 12);
        assert_eq!(t, Transition::None);
    }

    /// The CPU window: judged at [`SPIN_WINDOW`] or more (half a core
    /// spins), kept with its verdict when shorter, restarted by a new leader
    /// or a missing reading.
    #[test]
    fn spin_window_judges_half_a_core_over_a_second_or_more() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mark = Some((7, t0, 1_000));
        let at = |d: Duration| t0 + d;
        let ns = |d: Duration| 1_000 + u64::try_from(d.as_nanos()).expect("small");
        // Half a core over two seconds spins; just under does not.
        assert_eq!(
            spin_window(mark, false, Some((7, ns(ms(1000)))), at(ms(2000))),
            (Some((7, at(ms(2000)), ns(ms(1000)))), true)
        );
        assert!(
            !spin_window(mark, true, Some((7, ns(ms(999)))), at(ms(2000))).1,
            "999 ms of CPU across two seconds is under half a core"
        );
        // A window shorter than a second keeps its mark and its verdict.
        assert_eq!(
            spin_window(mark, true, Some((7, ns(ms(0)))), at(ms(999))),
            (mark, true)
        );
        // A new leader, or no reading, restarts with no verdict.
        assert_eq!(
            spin_window(mark, true, Some((8, 5)), at(ms(5000))),
            (Some((8, at(ms(5000)), 5)), false)
        );
        assert_eq!(spin_window(mark, true, None, at(ms(5000))), (None, false));
    }

    /// The CPU reader counts a child's CPU in nanoseconds — the mach-tick
    /// conversion ([`pid_cpu_ns`]; the raw record counts 125/3-ns ticks on
    /// this Mac) — checked against the kernel's own account of the SAME child:
    /// the `rusage` its reap returns, in microseconds, which no tick
    /// conversion touches.
    ///
    /// The oracle is that rusage, never the wall clock. A spinning thread gets
    /// a core per second of wall time only on an idle machine; beside other
    /// builds it gets its share, cores over runnable threads (14 cores at a
    /// load of 60 is 0.23 of one), so a `burned * 4 >= wall` floor can fail a
    /// correct reader by load alone. The child is STOPPED before the reading,
    /// and the stop is waited for (`WUNTRACED`), so the reading and the reap
    /// count the same CPU at any load, while a reader off by the tick factor
    /// (41.7x either way) lands far outside the 2x band.
    #[test]
    #[cfg(target_os = "macos")]
    fn pid_cpu_ns_reads_a_spinning_child_in_nanoseconds() {
        /// `struct rusage` (`<sys/resource.h>`): the two times, then fourteen
        /// `long` counters this test does not read; 144 bytes on Darwin.
        #[repr(C)]
        struct Rusage {
            utime: libc::timeval,
            stime: libc::timeval,
            _counters: [libc::c_long; 14],
        }
        const _: () = assert!(std::mem::size_of::<Rusage>() == 144);
        unsafe extern "C" {
            /// Declared here: aterm-libc does not export it.
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

        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "while :; do :; done"])
            .spawn()
            .expect("spawn a spinning shell");
        let pid = libc::pid_t::try_from(child.id()).expect("pid");
        // Burn until the reader under test says 50 ms. The loop decides
        // nothing: a reader off by the tick factor ends it early or late, and
        // the comparison below catches either. Its deadline only ends a reader
        // that never moves, which the comparison then fails.
        let deadline = Instant::now() + Duration::from_secs(60);
        while pid_cpu_ns(pid).is_none_or(|ns| ns < 50_000_000) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        // Stop, read, kill, reap — and only then assert, so no failure leaves
        // a stopped shell behind.
        // SAFETY: `kill` takes no pointers; `pid` is this test's unreaped child.
        let stop = unsafe { libc::kill(pid, libc::SIGSTOP) };
        let mut stopped: libc::c_int = 0;
        // SAFETY: `stopped` is a live out-param; `pid` is this test's own
        // child, and nothing else in this process waits on it.
        let waited = unsafe { libc::waitpid(pid, &mut stopped, libc::WUNTRACED) };
        let read = pid_cpu_ns(pid);
        // SAFETY: as above; a stopped child is still this test's to kill.
        let kill = unsafe { libc::kill(pid, libc::SIGKILL) };
        let mut status: libc::c_int = 0;
        let mut usage = std::mem::MaybeUninit::<Rusage>::zeroed();
        // SAFETY: live out-params of the declared layouts; `pid` is reaped
        // here, so `child` is never waited on again.
        let reaped = unsafe { wait4(pid, &mut status, 0, usage.as_mut_ptr()) };
        drop(child);
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
        // SAFETY: zeroed, then filled by the successful `wait4` above; every
        // field is a plain integer.
        let usage = unsafe { usage.assume_init() };
        let kernel = (micros(usage.utime) + micros(usage.stime)) * 1_000;
        assert!(kernel > 0, "the child ran");
        assert!(
            read <= kernel * 2 && read * 2 >= kernel,
            "pid_cpu_ns read {read} ns; the reap's rusage for the same stopped child is {kernel} ns"
        );
        assert_eq!(pid_cpu_ns(0), None);
    }

    /// Canonical type-ahead is watched (until the shell reads it) and never
    /// published; a wake-driven probe inside [`PROBE_MIN_GAP`] waits; and
    /// `forget` hands back what was published.
    #[test]
    fn step_typeahead_throttle_and_forget() {
        let mut w = InputWatches::default();
        let t0 = Instant::now();
        let typed = backlog(3, Duration::from_secs(60), true);
        let (t, next) = w.step(1, Some(&typed), false, Activity::default(), || panic!(), t0);
        assert_eq!((t, next), (Transition::None, Some(t0 + RECHECK)));
        assert!(
            !w.throttled(2, t0),
            "an unwatched session is never throttled"
        );
        assert!(w.throttled(1, t0 + Duration::from_millis(249)));
        assert!(!w.throttled(1, t0 + PROBE_MIN_GAP));
        // A throttled wake is deferred to the gap's end, never dropped; an
        // unwatched session has nothing to defer.
        w.defer(1);
        assert_eq!(w.next_wake(), Some(t0 + PROBE_MIN_GAP));
        w.defer(2);
        assert!(!w.armed(2));
        assert_eq!(w.forget(1), None);
        primed(&mut w, 1, t0);
        let _ = w.step(
            1,
            Some(&reading(1, 10, 0)),
            false,
            spin(RECHECK),
            || None,
            t0,
        );
        assert!(w.forget(1).is_some());
        assert!(!w.armed(1));
    }

    /// The four `status` fields: `-` without a reading, the unread count
    /// (kernel plus spill) and the oldest byte's wait with one, and the RSS
    /// only when the caller read it.
    #[test]
    fn status_fields_print_the_reading() {
        assert_eq!(
            status_fields(None, InputWord::Unmeasured, None),
            "input=- input_bytes=- input_wait_ms=- fg_rss_mb=-"
        );
        let clear = reading(0, 0, 0);
        assert_eq!(
            status_fields(Some(&clear), InputWord::Clear, None),
            "input=clear input_bytes=0 input_wait_ms=0 fg_rss_mb=-"
        );
        let stalled = InputBacklog {
            spilled: Some(2),
            ..reading(1, 161, 0)
        };
        assert_eq!(
            status_fields(Some(&stalled), InputWord::Stalled, Some(39_731)),
            "input=stalled input_bytes=3 input_wait_ms=161000 fg_rss_mb=39731"
        );
        // No reading answers `-` through the live probe too.
        let ctx = crate::session_store::test_handle(1).ctx;
        assert_eq!(
            status_input(&ctx.sink, None, false),
            "input=- input_bytes=- input_wait_ms=- fg_rss_mb=-"
        );
    }

    /// `HH:MM` of the stall's local clock time, across midnight and zones;
    /// no offset, no clock.
    #[test]
    fn since_hhmm_is_the_local_clock_at_the_stall() {
        let now = Instant::now();
        let since = now - Duration::from_secs(60);
        // 1_700_000_000 is 22:13:20 UTC.
        assert_eq!(
            since_hhmm(since, now, 1_700_000_000, Some(0)).as_deref(),
            Some("22:12")
        );
        assert_eq!(
            since_hhmm(since, now, 1_700_000_000, Some(-7 * 3600)).as_deref(),
            Some("15:12")
        );
        assert_eq!(
            since_hhmm(since, now, 1_700_000_000, Some(2 * 3600)).as_deref(),
            Some("00:12")
        );
        assert_eq!(since_hhmm(since, now, 1_700_000_000, None), None);
    }

    /// The server's attention line: the remedy by name and by command, the
    /// resume command only for the agent that has one, `the program` when
    /// unresolved, `for <dur>` without a clock, and the stopped wording. The
    /// incident's line fits the keyed cap with room to spare.
    #[test]
    fn attention_text_names_the_program_the_facts_and_the_remedy() {
        let fact = InputStallFact {
            word: InputWord::Stalled,
            since: Instant::now(),
            bytes: 1,
            stopped: false,
            rss_mb: Some(39_731),
            restart: Restart::default(),
        };
        let sid = "s-b7cf523445a1b0d8658e";
        let line = attention_text(
            &fact,
            Some("claude"),
            Some(aterm_phase::Program::Claude),
            sid,
            Some("14:02"),
            Duration::from_secs(9660),
        );
        assert_eq!(
            line,
            "claude is frozen: not reading input since 14:02 (1 B queued, rss 38.8 GB) \u{2014} \
             restart it: aterm ctl @s-b7cf523445a1b0d8658e signal term, then claude --continue"
        );
        assert!(line.len() <= crate::session_timeline::META_ATTENTION_KEYED_MAX);

        let plain = InputStallFact {
            rss_mb: None,
            ..fact.clone()
        };
        let line = attention_text(&plain, None, None, sid, None, Duration::from_secs(75));
        assert_eq!(
            line,
            "the program is frozen: not reading input for 1m15s (1 B queued) \u{2014} restart \
             it: aterm ctl @s-b7cf523445a1b0d8658e signal term"
        );
        let small = InputStallFact {
            rss_mb: Some(1),
            ..fact.clone()
        };
        let line = attention_text(
            &small,
            Some("sleep"),
            None,
            sid,
            Some("22:54"),
            Duration::ZERO,
        );
        assert!(line.contains("(1 B queued, rss 1 MB)"), "{line}");
        let codex = attention_text(
            &plain,
            Some("codex"),
            Some(aterm_phase::Program::Codex),
            sid,
            Some("09:00"),
            Duration::ZERO,
        );
        assert!(!codex.contains("--continue"), "{codex}");

        let stopped = InputStallFact {
            word: InputWord::Stopped,
            stopped: true,
            ..fact
        };
        assert_eq!(
            attention_text(
                &stopped,
                Some("claude"),
                Some(aterm_phase::Program::Claude),
                sid,
                Some("14:02"),
                Duration::ZERO,
            ),
            "claude is stopped with input queued since 14:02 \u{2014} resume it: aterm ctl \
             @s-b7cf523445a1b0d8658e signal cont"
        );
    }

    /// The menu row and the server's attention entry are ONE composition
    /// ([`attention_now`]): the row a human clicks says what `meta
    /// attention=` says. The row's key is the episode's — the same fact read
    /// later, with more bytes or turned stopped, keeps it; a later episode
    /// does not.
    #[test]
    fn the_menu_row_says_what_the_attention_entry_says_keyed_by_episode() {
        let now = Instant::now();
        let fact = InputStallFact {
            word: InputWord::Stalled,
            since: now.checked_sub(Duration::from_secs(95)).unwrap_or(now),
            bytes: 1,
            stopped: false,
            rss_mb: Some(39_731),
            restart: Restart::default(),
        };
        let sid = "s-b7cf523445a1b0d8658e";
        let claude = Some(aterm_phase::Program::Claude);
        let row = menu_row(&fact, Some("claude"), claude, sid, now);
        assert_eq!(
            row.text,
            attention_now(&fact, Some("claude"), claude, sid, now)
        );
        assert!(row.text.starts_with("claude is frozen: not reading input "));
        assert!(row.text.ends_with("signal term, then claude --continue"));
        let later = menu_row(
            &InputStallFact {
                bytes: 9,
                word: InputWord::Stopped,
                stopped: true,
                ..fact.clone()
            },
            Some("claude"),
            claude,
            sid,
            now + Duration::from_secs(60),
        );
        assert_eq!(later.key, row.key);
        let next = InputStallFact {
            since: now,
            ..fact.clone()
        };
        assert_ne!(episode_key(&next), row.key);
    }

    /// The band's phase slot for a stall: `frozen <dur> · not reading input`
    /// and its spoken remedy; `stopped <dur> · input queued` for a stopped
    /// job. The duration counts from the oldest unread byte.
    #[test]
    fn the_band_says_frozen_or_stopped_with_the_age_and_the_remedy() {
        let now = Instant::now();
        let fact = InputStallFact {
            word: InputWord::Stalled,
            since: now,
            bytes: 1,
            stopped: false,
            rss_mb: None,
            restart: Restart::default(),
        };
        assert_eq!(
            band_phase(&fact, now + Duration::from_secs(9660)),
            (
                "frozen".to_string(),
                vec!["2h41m".to_string(), "not reading input".to_string()],
                "frozen, not reading input for 2h41m; restart it".to_string(),
            )
        );
        let survived = InputStallFact {
            restart: Restart {
                leader: Some(4242),
                discards: 0,
                survived: true,
            },
            ..fact.clone()
        };
        assert_eq!(
            band_phase(&survived, now + Duration::from_secs(9660)),
            (
                "frozen".to_string(),
                vec!["2h41m".to_string(), "survived its restart".to_string()],
                "frozen, still running after its restart signal; end it with signal kill"
                    .to_string(),
            )
        );
        let stopped = InputStallFact {
            word: InputWord::Stopped,
            stopped: true,
            ..fact
        };
        assert_eq!(
            band_phase(&stopped, now + Duration::from_secs(12)),
            (
                "stopped".to_string(),
                vec!["12s".to_string(), "input queued".to_string()],
                "stopped with input queued for 12s; resume it".to_string(),
            )
        );
    }

    /// A program that SURVIVED its restart ([`Restart::survived`]): the
    /// server's line says so, counts no bytes (they were dropped, not read),
    /// and names `signal kill` — then the resume command, only for the agent
    /// with one. The incident's session id and RSS fit the keyed cap.
    #[test]
    fn a_survived_restart_names_signal_kill() {
        let fact = InputStallFact {
            word: InputWord::Stalled,
            since: Instant::now(),
            bytes: 1,
            stopped: false,
            rss_mb: Some(39_731),
            restart: Restart {
                leader: Some(69_156),
                discards: 0,
                survived: true,
            },
        };
        let sid = "s-b7cf523445a1b0d8658e";
        let line = attention_text(
            &fact,
            Some("claude"),
            Some(aterm_phase::Program::Claude),
            sid,
            Some("14:02"),
            Duration::from_secs(9660),
        );
        assert_eq!(
            line,
            "claude is still running after its restart signal, not reading input since 14:02 \
             (rss 38.8 GB) \u{2014} end it: aterm ctl @s-b7cf523445a1b0d8658e signal kill, then \
             claude --continue"
        );
        assert!(line.len() <= crate::session_timeline::META_ATTENTION_KEYED_MAX);
        let plain = InputStallFact {
            rss_mb: None,
            ..fact.clone()
        };
        assert_eq!(
            attention_text(&plain, None, None, sid, None, Duration::from_secs(75)),
            "the program is still running after its restart signal, not reading input for \
             1m15s \u{2014} end it: aterm ctl @s-b7cf523445a1b0d8658e signal kill"
        );
        // The row a human clicks says the same, under the episode's key.
        let row = menu_row(
            &fact,
            Some("claude"),
            Some(aterm_phase::Program::Claude),
            sid,
            Instant::now(),
        );
        assert!(
            row.text.contains("signal kill, then claude --continue"),
            "{}",
            row.text
        );
        assert_eq!(row.key, episode_key(&fact));
    }

    /// THE LOCKSTEP, on a live App: publishing a stall moves `status agent=`
    /// to `wall:unresponsive` and puts the server's line over the supervisor's
    /// badge; clearing it hands both back. Retiring a watch whose PTY ended
    /// unpublishes what it still showed.
    #[test]
    fn a_published_stall_moves_the_agent_word_and_the_attention_together() {
        let mut app = crate::App::headless_for_test();
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        {
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_program(7, Some("claude".into()));
            tl.publish_agent(
                "prompt",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        }
        crate::session_timeline::write_attention_owned(
            &ctx,
            "supervisor",
            crate::session_timeline::MetaEdit::Set("claude other: answer this box"),
        )
        .expect("accepted");
        let attention = || ctx.meta.lock().unwrap().attention.clone();

        let now = Instant::now();
        primed(&mut app.session_status.inputs, 0, now);
        let (t, _) = app.session_status.inputs.step(
            0,
            Some(&reading(1, 161, 0)),
            false,
            spin(RECHECK),
            || Some(39_731),
            now,
        );
        let Transition::Entered(fact) = t else {
            panic!("entered: {t:?}")
        };
        app.publish_input_stall(0, &ctx, Some(fact), now);
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(" program=claude agent=wall:unresponsive agent_detail=- "),
            "{record}"
        );
        let shown = attention().expect("the server's line");
        assert!(
            shown.starts_with("claude is frozen: not reading input "),
            "{shown}"
        );
        assert!(
            shown.contains(&format!(
                "aterm ctl @{} signal term, then claude --continue",
                ctx.self_id.as_str()
            )),
            "{shown}"
        );
        assert!(shown.contains("(1 B queued, rss 38.8 GB)"), "{shown}");

        // The Exit arm's retire: the stall goes with the program.
        app.retire_input_watch(0);
        assert!(!app.session_status.input_armed(0));
        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" agent=prompt "), "{record}");
        assert_eq!(
            attention().as_deref(),
            Some("claude other: answer this box"),
            "the supervisor's badge is uncovered, untouched"
        );
        // Nothing published: retiring again moves nothing.
        app.retire_input_watch(0);
        assert_eq!(
            attention().as_deref(),
            Some("claude other: answer this box")
        );
    }

    /// THE WATCH ON A REAL PTY. An idle session arms nothing; the human's
    /// byte into a raw program that has not read it arms ONE deadline — the
    /// instant it turns a stall — and publishes nothing yet; a wake inside
    /// [`PROBE_MIN_GAP`] waits; the program reading ends the watch. Every
    /// probe rearms the sink's input hook FIRST, so the next write that lands
    /// wakes the watch again.
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
        assert!(app.observe_input_stalls(t0, Some(77)).is_empty());
        assert!(app.session_status.input_armed(77));
        let wake = app.session_status.next_input_wake().expect("armed");
        assert!(
            wake > t0 + Duration::from_secs(9) && wake <= t0 + input_backlog::STALL_AFTER,
            "the stall instant, not a poll: {:?}",
            wake - t0
        );
        assert_eq!(app.session_status.input_stall(77), None, "pending");
        let record = app.session_status_record(77).expect("live session");
        assert!(
            record.contains(" input=pending input_bytes=2 input_wait_ms="),
            "{record}"
        );
        // The probe rearmed the hook: the next write that lands wakes again.
        sink.write_frame_nonparking(b"b").expect("a third key");
        assert_eq!(hooks.load(Ordering::SeqCst), 2);
        // A wake inside the gap waits — DEFERRED to the gap's end, not
        // dropped: the deadline is pulled in to it.
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
            record.contains(" input=clear input_bytes=0 input_wait_ms=0 fg_rss_mb=- supervisor="),
            "{record}"
        );
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    /// THE ADOPTED BACKLOG (S6 review, 2026-09-24). A master that arrives with
    /// input already unread — a seamless update's handoff, the old process's
    /// Enter still queued — is watched from its ATTACH, although no write of
    /// this process ever lands behind it (the refusal gate stops every socket
    /// write a second later). The byte is dated from the sink's birth, so the
    /// watch's one deadline is that birth plus [`input_backlog::STALL_AFTER`],
    /// and the reading there enters the stall. `status` wakes an unwatched
    /// session the same way — here a byte an unseen writer queued after the
    /// watch had ended — and never a watched one, nor one whose wake is
    /// already in flight. An idle master's attach fires nothing; off a tty
    /// nothing is installed at all.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_master_adopted_with_input_unread_is_watched_from_its_attach() {
        use std::os::fd::AsRawFd;
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
        // A write straight into the master, past every sink of this process:
        // the old process's Enter, or an unseen writer's byte.
        let unseen = |master: i32, bytes: &[u8]| {
            // SAFETY: `master` is an open pty master and `bytes` a live buffer
            // of the length passed.
            let n = unsafe { libc::write(master, bytes.as_ptr().cast(), bytes.len()) };
            assert_eq!(usize::try_from(n).ok(), Some(bytes.len()), "write(master)");
        };

        // Off a tty: nothing installed, so a write that lands wakes nothing.
        let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let socket = SinkWriter::new(writer.as_raw_fd());
        assert!(!watch_input(&socket, wake()));
        assert_eq!(socket.write_frame(b"x").expect("write"), 1);
        assert_eq!(count(), 0, "no reading, no hook");

        // An idle master: installed, and silent until a write lands.
        let (idle_master, idle_slave) = raw_pty_pair();
        let idle = SinkWriter::new(idle_master);
        assert!(!watch_input(&idle, wake()), "nothing unread: no wake");
        assert_eq!(count(), 0);
        idle.write_frame_nonparking(b"a").expect("a key");
        assert_eq!(count(), 1, "the hook is installed");

        // The adopted master: its Enter predates this process's sink.
        let (master, slave) = raw_pty_pair();
        unseen(master, b"\r");
        let born_before = Instant::now();
        let sink = Arc::new(SinkWriter::new(master));
        let born_after = Instant::now();
        assert!(watch_input(&sink, wake()), "the attach wakes the watch");
        assert_eq!(count(), 2);
        let mut app = crate::App::headless_for_test();
        app.pool
            .insert(crate::stub_session_with_sink(77, sink.clone()));
        let record = app.session_status_record(77).expect("live session");
        assert!(record.contains(" input=pending input_bytes=1 "), "{record}");
        assert_eq!(count(), 2, "the attach's wake is in flight: not doubled");

        // The wake's probe, a moment later: watched, and the byte dated from
        // the sink's birth, not from the probe that first saw it. (The lower
        // bound gives the probe's own latency: `wait` is read after `t0`.)
        std::thread::sleep(Duration::from_millis(300));
        let t0 = Instant::now();
        assert!(app.observe_input_stalls(t0, Some(77)).is_empty());
        let deadline = app.session_status.next_input_wake().expect("armed");
        assert!(
            deadline <= born_after + input_backlog::STALL_AFTER
                && deadline + Duration::from_millis(100)
                    >= born_before + input_backlog::STALL_AFTER,
            "the stall instant of a byte dated from the sink's birth: {:?} after it",
            deadline.saturating_duration_since(born_before)
        );
        let _ = app.session_status_record(77).expect("live session");
        assert_eq!(count(), 2, "watched: a status poll wakes nothing");
        // At that deadline the same reading, aged as the clock has, enters.
        let live = sink.input_backlog().expect("a tty");
        let aged = InputBacklog {
            wait: input_backlog::STALL_AFTER,
            ..live
        };
        let mut watches = InputWatches::default();
        primed(&mut watches, 77, deadline);
        let (t, _) = watches.step(77, Some(&aged), false, spin(RECHECK), || None, deadline);
        let Transition::Entered(fact) = t else {
            panic!("entered: {t:?}")
        };
        assert_eq!((fact.word, fact.bytes), (InputWord::Stalled, 1));

        // The program reads: the watch ends.
        assert_eq!(slave_read_all(slave), b"\r");
        assert!(
            app.observe_input_stalls(t0 + PROBE_MIN_GAP, Some(77))
                .is_empty()
        );
        assert!(!app.session_status.input_armed(77));
        // An unseen writer queues a byte: nothing announces it, and only the
        // next `status` wakes the watch — once, while that wake is in flight.
        unseen(master, b"\x1b[B");
        assert_eq!(count(), 2, "an unseen write fires no hook");
        let record = app.session_status_record(77).expect("live session");
        assert!(record.contains(" input=pending input_bytes=3 "), "{record}");
        assert_eq!(count(), 3, "status woke the unwatched session");
        let _ = app.session_status_record(77).expect("live session");
        assert_eq!(count(), 3);
        assert_eq!(slave_read_all(slave), b"\x1b[B");
        for fd in [idle_master, idle_slave, master, slave] {
            aterm_pty::close_fd(fd);
        }
    }

    /// THE ATTACH WAKE AN OVERLAP UPGRADE DROPS (whole-branch review,
    /// 2026-09-25). The adopted master's attach runs before this process's
    /// proof, so the wake [`watch_input`] posts for its unread Enter reaches
    /// `user_event` while `incoming_handoff_pending` holds and is SWALLOWED —
    /// routed here through that very gate (`App::drops_wake_before_commit`),
    /// not handed to [`crate::App::observe_input_stalls`] as the test above
    /// does. The drop releases the hook it fired (else every later wake — a
    /// refusal's, `status`'s, a replayed write's — found it fired for good),
    /// and Commit probes every adopted session once
    /// ([`crate::App::rewatch_adopted_input`]): the one with input unread is
    /// watched from there, an idle one arms nothing, and a wake that is not an
    /// input watch releases no hook.
    #[test]
    #[cfg(target_os = "macos")]
    fn an_adopted_backlog_whose_attach_wake_is_dropped_before_commit_is_watched_at_commit() {
        use std::sync::{Arc, Mutex};
        let posted: Arc<Mutex<Vec<crate::Wake>>> = Arc::new(Mutex::new(Vec::new()));
        // The event-loop proxy, as the attach builds it (`spawn.rs`).
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

        // The adopted master: the old process wrote the Enter, straight into it.
        let (master, slave) = raw_pty_pair();
        // SAFETY: `master` is an open pty master and the buffer is one live byte.
        let n = unsafe { libc::write(master, b"\r".as_ptr().cast(), 1) };
        assert_eq!(n, 1, "write(master)");
        let sink = Arc::new(SinkWriter::new(master));
        // A second adopted session with nothing unread.
        let (idle_master, idle_slave) = raw_pty_pair();
        let idle = Arc::new(SinkWriter::new(idle_master));

        let mut app = crate::App::headless_for_test();
        for (id, sink) in [(77, sink.clone()), (78, idle.clone())] {
            let mut session = crate::stub_session_with_sink(id, sink);
            session.handoff_local_id = Some(id - 70);
            app.pool.insert(session);
        }
        app.incoming_handoff_pending = true;

        // The prepare-time attach: the unread Enter posts one wake.
        assert!(watch_input(&sink, proxy(77)));
        assert!(!watch_input(&idle, proxy(78)), "nothing unread: no wake");
        let wakes = take();
        assert_eq!(wakes.len(), 1);

        // A wake that is no input watch releases nothing it does not own.
        assert!(app.drops_wake_before_commit(&crate::Wake::MetaChanged { session: 77 }));
        assert!(
            !sink.wake_input_hook(),
            "still fired: the attach's wake is in flight"
        );

        // The attach's wake meets the pending gate: swallowed, its probe never
        // runs — and its hook is released with it.
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

        // Commit: every adopted session probed once.
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
        // The probe rearmed the hook: the next write that lands wakes again.
        sink.write_frame_nonparking(b"a").expect("a replayed key");
        assert_eq!(take().len(), 1, "the rearmed hook fired");

        assert_eq!(slave_read_all(slave), b"\ra");
        for fd in [master, slave, idle_master, idle_slave] {
            aterm_pty::close_fd(fd);
        }
    }

    /// A job on its own pty: `/bin/sh` spinning in raw mode as the
    /// controlling terminal's session leader — so it leads the foreground
    /// group `tcgetpgrp` reads — with SIGTERM ignored (`trap '' TERM`, the
    /// shape of a Node program whose SIGTERM listener its busy JS thread never
    /// runs) or not. Returns `(master, slave, job, leader)`; the job is killed
    /// and reaped however the test ends.
    #[cfg(target_os = "macos")]
    fn spinning_job(ignore_term: bool) -> (i32, i32, ReapJob, i32) {
        pty_job(if ignore_term {
            "trap '' TERM; while :; do :; done"
        } else {
            "while :; do :; done"
        })
    }

    /// `/bin/sh -c script` on its own raw pty as the controlling terminal's
    /// session leader ([`spinning_job`]).
    #[cfg(target_os = "macos")]
    fn pty_job(script: &str) -> (i32, i32, ReapJob, i32) {
        use std::os::fd::BorrowedFd;
        use std::os::unix::process::CommandExt;
        let (master, slave) = raw_pty_pair();
        // SAFETY: `slave` is open for the whole of this function and was
        // returned by `raw_pty_pair`, which this test owns.
        let slave_fd = unsafe { BorrowedFd::borrow_raw(slave) };
        let stdio = || std::process::Stdio::from(slave_fd.try_clone_to_owned().expect("dup slave"));
        let mut job = std::process::Command::new("/bin/sh");
        job.args(["-c", script])
            .stdin(stdio())
            .stdout(stdio())
            .stderr(stdio());
        // SAFETY: runs in the forked child before exec, after stdio is in
        // place: `setsid` and `ioctl(TIOCSCTTY)` are async-signal-safe and
        // touch nothing of the parent's.
        unsafe {
            job.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY.into(), 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let job = ReapJob(job.spawn().expect("spawn the job"));
        let leader = libc::pid_t::try_from(job.0.id()).expect("pid");
        let deadline = Instant::now() + Duration::from_secs(10);
        // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
        while unsafe { libc::tcgetpgrp(master) } != leader {
            assert!(Instant::now() < deadline, "the job never led the tty");
            std::thread::sleep(Duration::from_millis(5));
        }
        (master, slave, job, leader)
    }

    /// Kills and reaps a [`spinning_job`] however its test ends.
    #[cfg(target_os = "macos")]
    struct ReapJob(std::process::Child);

    #[cfg(target_os = "macos")]
    impl Drop for ReapJob {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A DROP IS NOT A READ, on a live App, a real pty and a real process
    /// (whole-branch review, third round, 2026-09-25). A stall is published
    /// for a spinning raw job that ignores SIGTERM, as a spinning Node
    /// program with a SIGTERM listener does, and `signal term` goes through
    /// the verb: its discard empties the queue and the job lives on. Before
    /// this fix the next probe read that empty queue as recovery and withdrew
    /// everything — `agent=`, the server's attention, and with them the band,
    /// the menu row and the supervisor's hold — and the `claude --continue`
    /// typed next went into the frozen program. Now the probe HOLDS it:
    /// `status input=stalled input_bytes=0`, the agent word and the attention
    /// stand, and the resume command is refused. Past [`RESTART_GRACE`] the
    /// line names `signal kill`, and `signal kill` ends it: the probe after
    /// the job is reaped withdraws it all.
    ///
    /// A job that dies on SIGTERM is the other half: its stall is withdrawn
    /// by the first probe after it ends, as before.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_stall_whose_program_lives_through_signal_term_stands_until_signal_kill() {
        for ignore_term in [true, false] {
            let (master, slave, mut job, leader) = spinning_job(ignore_term);
            let sink = std::sync::Arc::new(SinkWriter::new(master));
            sink.note_master_nonblocking(true);
            let mut app = crate::App::headless_for_test();
            app.pool
                .insert(crate::stub_session_with_sink(77, sink.clone()));
            let ctx = app.pool.get(77).expect("session 77").ctx.clone();
            {
                let mut tl = ctx.timeline.lock().unwrap();
                tl.note_foreground_group(leader);
                tl.set_program(leader, Some("claude".into()));
                tl.publish_agent(
                    "busy",
                    None,
                    None,
                    Some(aterm_phase::Program::Claude),
                    crate::session_timeline::AgentStamp {
                        generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                        fp: 1,
                    },
                );
            }
            let attention = || ctx.meta.lock().unwrap().attention.clone();
            let record = |app: &crate::App| app.session_status_record(77).expect("live session");

            // The Enter it never reads, then the stall: entered on readings
            // aged by hand (ten real seconds is no unit test), on the job's
            // REAL leader and the sink's real discard count.
            sink.write_frame_nonparking(b"\r").expect("the Enter");
            let t0 = Instant::now();
            let on_leader = |ns: u64| Activity {
                cpu: Some((leader, ns)),
                last_output: None,
                discards: sink.discards(),
                read_since_discard: sink.read_since_discard(),
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

            let reply = crate::control::cmd_signal(master, "term", &sink, None);
            assert_eq!(reply, format!("OK signalled pgrp {leader} discarded=1\n"));
            // Past PROBE_MIN_GAP of the entry probe, so the wake-driven probe runs.
            std::thread::sleep(Duration::from_millis(300));

            if !ignore_term {
                // It ends: the probe after it is reaped withdraws the stall.
                let _ = job.0.wait();
                assert_eq!(app.observe_input_stalls(Instant::now(), Some(77)), [77]);
                assert_eq!(app.session_status.input_stall(77), None);
                assert_eq!(attention(), None);
                assert!(
                    !record(&app).contains("wall:unresponsive"),
                    "{}",
                    record(&app)
                );
                assert_eq!(refusal(&ctx, "send", "claude --continue", false), None);
                drop(job);
                aterm_pty::close_fd(master);
                aterm_pty::close_fd(slave);
                continue;
            }

            assert!(
                job.0.try_wait().expect("try_wait").is_none(),
                "it ignored SIGTERM"
            );
            let t1 = Instant::now();
            assert!(
                app.observe_input_stalls(t1, Some(77)).is_empty(),
                "the empty queue is a drop, not a read: nothing moves"
            );
            let held = app
                .session_status
                .input_stall(77)
                .expect("still published")
                .clone();
            assert!(!held.restart.survived);
            let status = record(&app);
            assert!(status.contains(" input=stalled input_bytes=0 "), "{status}");
            assert!(status.contains(" agent=wall:unresponsive "), "{status}");
            assert!(
                attention().is_some_and(|a| a.starts_with("claude is frozen: ")),
                "{:?}",
                attention()
            );
            // The resume command typed into it is refused, naming the remedy;
            // `unread=ok` and the signals stay open.
            let refused = refusal(&ctx, "send", "claude --continue", false).expect("refused");
            assert!(
                refused.starts_with("ERR busy input-unread bytes=0 "),
                "{refused}"
            );
            assert!(refused.contains("signal kill"), "{refused}");
            assert_eq!(refusal(&ctx, "send", "claude --continue", true), None);
            assert_eq!(refusal(&ctx, "signal", "kill", false), None);

            // Survived: the server's line moves on to `signal kill`. The job
            // still spins, but a real CPU reading across five synthetic
            // seconds would read as a stopped spin and let the hold go
            // ([`Restart`]). So this one probe is folded by hand: a whole
            // core since the hand-aged window, on the sink's real reading,
            // discard count and read evidence.
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
            assert!(
                shown.starts_with("claude is still running after its restart signal, "),
                "{shown}"
            );
            assert!(
                shown.contains(&format!(
                    "aterm ctl @{} signal kill, then claude --continue",
                    ctx.self_id.as_str()
                )),
                "{shown}"
            );

            // `signal kill` ends it (nothing left to drop), the job is reaped
            // as its shell would, and the probe withdraws everything.
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
            assert_eq!(refusal(&ctx, "send", "claude --continue", false), None);
            drop(job);
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
    }

    /// A PROGRAM THAT LIVES THROUGH `signal term` AND RECOVERS IS LET GO, on
    /// a live App, a real pty and a real process (whole-branch review, fourth
    /// round, 2026-09-25). The reviewer's fixture ignored SIGTERM, spun, then
    /// read its input without drawing, and it stayed published as frozen with
    /// no end. Every input verb was refused while it read. This job has that
    /// shape: `/bin/sh` ignoring TERM and spinning until SIGUSR1, then `exec
    /// cat`, which keeps the pid and the ignored TERM, sleeps in `read`, and
    /// draws nothing.
    ///
    /// Each run is entered and held as the job that never recovers is
    /// ([`a_stall_whose_program_lives_through_signal_term_stands_until_signal_kill`]),
    /// on the job's REAL CPU: a real second of spinning is the entry window,
    /// so the probes after it read what the job really burned. Then the job
    /// recovers, and one sign of life lets the hold go:
    ///
    /// * `read`: a key sent after the drop is read. The live refusal and
    ///   `status` stop claiming "not reading" at once, and the probe inside
    ///   the spin window, which still reads spinning, withdraws everything
    ///   on the read alone;
    /// * `spin`: nothing is sent. A probe whose window the job spent asleep
    ///   judges it not spinning, and that withdraws everything.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_program_that_lives_through_signal_term_and_reads_again_is_let_go() {
        const RECOVERS: &str =
            "trap '' TERM; trap 'go=1' USR1; while [ -z \"$go\" ]; do :; done; exec cat >/dev/null";
        let window = u64::try_from(SPIN_WINDOW.as_nanos()).expect("small");
        for by_read in [true, false] {
            let (master, slave, mut job, leader) = pty_job(RECOVERS);
            let sink = std::sync::Arc::new(SinkWriter::new(master));
            sink.note_master_nonblocking(true);
            let mut app = crate::App::headless_for_test();
            app.pool
                .insert(crate::stub_session_with_sink(78, sink.clone()));
            let ctx = app.pool.get(78).expect("session 78").ctx.clone();
            {
                let mut tl = ctx.timeline.lock().unwrap();
                tl.note_foreground_group(leader);
                tl.set_program(leader, Some("claude".into()));
                tl.publish_agent(
                    "busy",
                    None,
                    None,
                    Some(aterm_phase::Program::Claude),
                    crate::session_timeline::AgentStamp {
                        generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                        fp: 1,
                    },
                );
            }
            let attention = || ctx.meta.lock().unwrap().attention.clone();
            let record = |app: &crate::App| app.session_status_record(78).expect("live session");
            let burned = || pid_cpu_ns(leader).expect("the job's CPU");

            // A real second of spinning, so the entry window is the job's own.
            let deadline = Instant::now() + Duration::from_secs(30);
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
                discards: sink.discards(),
                read_since_discard: false,
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

            // `signal term`: the Enter is dropped, the job lives on, held. The
            // probes run on instants past t0 by less than a spin window, so
            // they keep the entry's verdict whatever the machine's load.
            let reply = crate::control::cmd_signal(master, "term", &sink, None);
            assert_eq!(reply, format!("OK signalled pgrp {leader} discarded=1\n"));
            let t1 = t0 + Duration::from_millis(300);
            assert!(app.observe_input_stalls(t1, Some(78)).is_empty(), "held");
            assert!(
                record(&app).contains(" input=stalled input_bytes=0 "),
                "{}",
                record(&app)
            );
            assert!(refusal(&ctx, "send", "y", false).is_some());

            // It recovers: the spin ends and `cat` sleeps in `read`.
            // SAFETY: `leader` is this test's live child (reaped only by
            // `job`'s drop), and SIGUSR1 is the signal its script traps.
            assert_eq!(unsafe { libc::kill(leader, libc::SIGUSR1) }, 0);
            let released = if by_read {
                // A key sent after the drop, read at once.
                assert_eq!(sink.write_frame_nonparking(b"x").expect("the key"), 1);
                let deadline = Instant::now() + Duration::from_secs(5);
                while aterm_pty::input_queue_len(master) != Some(0) {
                    assert!(Instant::now() < deadline, "the job never read the key");
                    std::thread::sleep(Duration::from_millis(10));
                }
                // One reading already sees the read: no refusal, no `stalled`.
                assert_eq!(refusal(&ctx, "send", "y", false), None);
                let status = record(&app);
                assert!(status.contains(" input=clear "), "{status}");
                t0 + Duration::from_millis(900)
            } else {
                // Asleep: the job's CPU stops moving.
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let before = burned();
                    std::thread::sleep(Duration::from_millis(200));
                    if burned().saturating_sub(before) < 20_000_000 {
                        break;
                    }
                    assert!(Instant::now() < deadline, "the job never slept");
                }
                // A window at least twice what really passed since t0, so no
                // spin before the sleep can fill half of it.
                t0 + 2 * t0.elapsed() + Duration::from_secs(2)
            };
            assert_eq!(app.observe_input_stalls(released, Some(78)), [78]);
            assert!(
                job.0.try_wait().expect("try_wait").is_none(),
                "the job is alive: this is a recovery, not an end"
            );
            assert_eq!(app.session_status.input_stall(78), None);
            assert_eq!(attention(), None);
            let status = record(&app);
            assert!(!status.contains("wall:unresponsive"), "{status}");
            assert!(!status.contains(" input=stalled "), "{status}");
            assert_eq!(refusal(&ctx, "send", "y", false), None);
            drop(job);
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
    }
}
