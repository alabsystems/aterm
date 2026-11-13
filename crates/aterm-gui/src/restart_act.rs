// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit in-place restart: `aterm ctl @<sid> restart [pid=<n>] [if-stalled] [quiet=<s>]`.

#![cfg_attr(not(unix), allow(dead_code))]

use std::sync::Arc;
#[cfg(unix)]
use std::sync::Weak;
use std::time::{Duration, Instant};

use aterm_agent::harness::restart_stamp::Cause;
use aterm_agent::harness::restart_stamp::{self, Stamp};

use crate::SessionCtx;

/// How long after its `SIGTERM` a deaf program is given before the act's `SIGKILL`
/// ([`crate::input_stall::RESTART_GRACE`]).
pub(crate) const GRACE: Duration = crate::input_stall::RESTART_GRACE;

/// Minimum test value of [`GRACE`] ([`escalate`], [`DEADLINE`]).
const GRACE_TEST_FLOOR: Duration = Duration::from_secs(1);

/// The grace the act waits ([`GRACE`]).
fn grace() -> Duration {
    aterm_gui_timing::current().of_at_least(GRACE, GRACE_TEST_FLOOR)
}

/// The act's deadline in any case.
pub(crate) const DEADLINE: Duration = Duration::from_secs(30);

/// The deadline an escalation started on this thread waits by: [`DEADLINE`], unless a unit test
/// set its own.
fn deadline() -> Duration {
    #[cfg(test)]
    if let Some(deadline) = DEADLINE_FOR_TEST.with(std::cell::Cell::get) {
        return deadline;
    }
    DEADLINE
}

#[cfg(test)]
thread_local! {
    static DEADLINE_FOR_TEST: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// This test thread's escalation deadline until the guard drops: a test whose stand-in must end on
/// its `SIGTERM` waits for that by the hang detector, not by [`DEADLINE`].
#[cfg(test)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct DeadlineForTest(Option<Duration>);

#[cfg(test)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn deadline_for_test(deadline: Duration) -> DeadlineForTest {
    DeadlineForTest(DEADLINE_FOR_TEST.with(|slot| slot.replace(Some(deadline))))
}

#[cfg(test)]
impl Drop for DeadlineForTest {
    fn drop(&mut self) {
        DEADLINE_FOR_TEST.with(|slot| slot.set(self.0));
    }
}

/// The verb's arguments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ask {
    /// `pid=<n>`: only while `n` leads the tab's foreground group.
    pub(crate) pid: Option<i32>,
    /// `if-stalled`: only while a running stall is published for it.
    pub(crate) if_stalled: bool,
    /// `quiet=<s>`: refused while a person gave the session input within `s` seconds.
    pub(crate) quiet: Option<u32>,
}

/// Pure parser for `restart [pid=<n>] [if-stalled] [quiet=<s>]`, in any order, each at most once.
pub(crate) fn parse(rest: &str) -> Result<Ask, String> {
    let mut ask = Ask::default();
    let mut stalled_seen = false;
    for w in rest.split_whitespace() {
        let bad = || format!("ERR bad restart argument: {w}\n");
        if w == "if-stalled" && !stalled_seen {
            stalled_seen = true;
            ask.if_stalled = true;
        } else if let Some(s) = w.strip_prefix("quiet=") {
            match s
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| s.parse::<u32>().ok())
                .flatten()
            {
                Some(s) if ask.quiet.is_none() => ask.quiet = Some(s),
                _ => return Err(bad()),
            }
        } else if let Some(n) = w.strip_prefix("pid=") {
            match n
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| n.parse::<i32>().ok())
                .flatten()
            {
                Some(n) if n > 1 && ask.pid.is_none() => ask.pid = Some(n),
                _ => return Err(bad()),
            }
        } else {
            return Err(bad());
        }
    }
    Ok(ask)
}

/// Whether a running stall is published in `tl` for the foreground group `pgrp`.
pub(crate) fn stalled_for(tl: &crate::session_timeline::SessionTimeline, pgrp: i32) -> bool {
    tl.agent().input.as_ref().is_some_and(|fact| {
        fact.restart
            .leader
            .map_or(!fact.stopped, |leader| leader == pgrp)
    })
}

/// The timeline event sent instead of signalling a scoped harness's own process.
pub(crate) const RESTART_EVENT: &str = "restart";

fn scoped_relaunch(ctx: &SessionCtx, pgrp: i32) -> bool {
    scoped_relaunch_pid(ctx, crate::metrics::now_us()).and_then(|pid| i32::try_from(pid).ok())
        == Some(pgrp)
}

fn scoped_relaunch_pid(ctx: &SessionCtx, now_us: u64) -> Option<u32> {
    ctx.meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .live_scoped(now_us)
        .filter(|scoped| {
            scoped
                .claim
                .scopes
                .contains(aterm_agent::harness::contract::Scope::Relaunch)
        })
        .map(|scoped| scoped.proven.pid)
}

fn record_restart(ctx: &SessionCtx, cause: Cause) {
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record_row(RESTART_EVENT, format!("cause={}", cause.word()));
}

/// Publish an account-pool selection to a proven relaunch claimant. No credential value crosses
/// this boundary: only the row label and the home its sessions already share with the default.
pub(crate) fn record_scoped_account(
    ctx: &SessionCtx,
    cause: &str,
    env: &aterm_agent::harness::accounts_pool::LoginEnv,
) -> bool {
    if !matches!(cause, "account" | "account-ahead")
        || env.var != "KIMI_CODE_HOME"
        || !aterm_agent::harness::accounts::admissible_label(&env.label)
        || env
            .value
            .as_deref()
            .is_some_and(|home| !std::path::Path::new(home).is_absolute())
    {
        return false;
    }
    let Some(pid) =
        scoped_relaunch_pid(ctx, crate::metrics::now_us()).and_then(|pid| i32::try_from(pid).ok())
    else {
        return false;
    };
    if !scoped_relaunch(ctx, pid) {
        return false;
    }
    let home = env
        .value
        .as_deref()
        .map_or_else(|| "-".to_string(), crate::control::pct_encode);
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record_row(
            RESTART_EVENT,
            format!("cause={cause} to={} home={home}", env.label),
        );
    true
}

/// Why the act cannot run for `pgrp` at `start` ([`crate::input_stall::host_hand_refusal`]).
pub(crate) fn unavailable(
    ctx: &SessionCtx,
    pgrp: i32,
    start: &str,
    now_us: u64,
) -> Option<&'static str> {
    unavailable_for(
        ctx,
        pgrp,
        start,
        now_us,
        aterm_primer::home_dir().as_deref(),
    )
}

/// [`unavailable`] for the tab's shell's home `home`.
fn unavailable_for(
    ctx: &SessionCtx,
    pgrp: i32,
    start: &str,
    now_us: u64,
    home: Option<&std::path::Path>,
) -> Option<&'static str> {
    if scoped_relaunch(ctx, pgrp) {
        return None;
    }
    let snap = crate::harness_host::published_snapshot(ctx.self_id.as_str())
        .filter(|snap| i32::try_from(snap.pid).ok() == Some(pgrp) && snap.start == start);
    let Some(snap) = snap else {
        return Some("not-the-hosts-agent");
    };
    if !relaunchable(ctx.self_id.as_str(), &snap, home) {
        return Some("not-relaunchable");
    }
    crate::input_stall::host_hand_refusal(ctx, now_us)
}

/// Whether the act's relaunch would bring back the agent the host's snapshot `snap` of tab
/// `sid` names: a validated self-claim resume line, or a lane that relaunches it and carries
/// the restart for the tab's shell's home `home` ([`unavailable_for`]'s `not-relaunchable`).
fn relaunchable(
    sid: &str,
    snap: &aterm_agent::harness::relaunch::Snapshot,
    home: Option<&std::path::Path>,
) -> bool {
    match crate::self_claim::copy_of_tab(sid)
        .filter(|copy| copy.pid == snap.pid && copy.start == snap.start)
    {
        Some(copy) => aterm_agent::harness::relaunch::same_self_claim(snap, &copy),
        None => {
            lane_relaunches(snap)
                && home
                    .is_some_and(|home| aterm_agent::harness::relaunch::carries_restart(home, snap))
        }
    }
}

/// WHETHER THE ACT IS OFFERED FOR PROCESS `pid` IN TAB `sid` as far as a word outside the
/// session can tell (the Claude Code account's `weekly – · until relaunch`): the host's published
/// snapshot names that process (else `not-the-hosts-agent`: an Autopilot-off tab has none) and
/// its relaunch would bring it back (else `not-relaunchable`, a program with no shell under it
/// among them). The host's hand (`not-supervised`, `halted`, a driver's lease or turn) is the
/// act's to judge when it is asked: a hold or a lease passes, and the word stands after it.
pub(crate) fn offered_for(sid: &str, pid: u32) -> bool {
    crate::harness_host::published_snapshot(sid)
        .filter(|snap| snap.pid == pid)
        .is_some_and(|snap| relaunchable(sid, &snap, aterm_primer::home_dir().as_deref()))
}

/// WHETHER THE RELAUNCH LINE aterm would type for process `pid` in tab `sid` HAS ROOM FOR THE
/// USAGE TAP (the Claude Code account's `weekly – · until relaunch`, beside [`offered_for`]): the
/// host's published snapshot names that process and its line, with the longer of the tap's two
/// values (the one that carries the owner's statusLine) for this aterm's lasting path, fits the
/// tty's line ([`aterm_agent::harness::relaunch::tap_room`]). `false` where any of that is unread.
pub(crate) fn tap_room_for(sid: &str, pid: u32) -> bool {
    use aterm_agent::harness::tap;
    let Some(value) = tap::stable_exe().and_then(|exe| tap::settings_value_carrying(&exe, true))
    else {
        return false;
    };
    crate::harness_host::published_snapshot(sid)
        .filter(|snap| snap.pid == pid)
        .zip(aterm_primer::home_dir())
        .is_some_and(|(snap, home)| aterm_agent::harness::relaunch::tap_room(&home, &snap, &value))
}

const fn lane_relaunches(snap: &aterm_agent::harness::relaunch::Snapshot) -> bool {
    match &snap.run {
        aterm_agent::harness::relaunch::AgentRun::Claude
        | aterm_agent::harness::relaunch::AgentRun::Agy(_) => true,
        aterm_agent::harness::relaunch::AgentRun::Codex(_)
        | aterm_agent::harness::relaunch::AgentRun::Muse(_) => false,
    }
}

/// Where the explicit restart would be carried ([`offered`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Offer {
    /// The process the act names (`restart pid=<n> if-stalled`): the host's snapshot's, the stall's leader where the watch read one.
    pub(crate) pid: u32,
    /// Whether the offer promises relaunch ([`aterm_agent::harness::relaunch::resumes_on_exit`]).
    pub(crate) promised: bool,
}

/// The shared restart offer for stall refusals.
pub(crate) fn offered(ctx: &SessionCtx, leader: Option<i32>, now_us: u64) -> Option<Offer> {
    offered_unless(ctx, leader, now_us, None)
}

/// Offer check ignoring holds ([`offered`], [`crate::input_stall::host_hand_refusal`]).
pub(crate) fn offered_but_for_hold(ctx: &SessionCtx, leader: Option<i32>, now_us: u64) -> bool {
    offered_unless(ctx, leader, now_us, Some("halted")).is_some()
}

/// [`offered`], with the host hand's refusal `waived` ignored.
fn offered_unless(
    ctx: &SessionCtx,
    leader: Option<i32>,
    now_us: u64,
    waived: Option<&str>,
) -> Option<Offer> {
    let pid = leader
        .or_else(|| scoped_relaunch_pid(ctx, now_us).and_then(|pid| i32::try_from(pid).ok()))
        .or_else(|| {
            crate::harness_host::published_snapshot(ctx.self_id.as_str())
                .and_then(|snap| i32::try_from(snap.pid).ok())
        });
    if let Some(pid) = pid.filter(|pid| scoped_relaunch(ctx, *pid)) {
        return Some(Offer {
            pid: u32::try_from(pid).ok()?,
            promised: true,
        });
    }
    let snap = crate::harness_host::published_snapshot(ctx.self_id.as_str())?;
    let pid = i32::try_from(snap.pid).ok()?;
    let agent = snap.agent().program();
    let in_front = || crate::harness_host::admitted(ctx) == Some(agent);
    let home = aterm_primer::home_dir()?;
    let (relaunchable, promised) = match crate::self_claim::copy_of_tab(ctx.self_id.as_str())
        .filter(|copy| copy.pid == snap.pid && copy.start == snap.start)
    {
        Some(copy) => {
            let ok = aterm_agent::harness::relaunch::same_self_claim(&snap, &copy);
            (ok, ok && snap.shell != 0)
        }
        None => (
            lane_relaunches(&snap) && aterm_agent::harness::relaunch::carries_restart(&home, &snap),
            lane_relaunches(&snap) && aterm_agent::harness::relaunch::resumes_on_exit(&home, &snap),
        ),
    };
    if leader.is_some_and(|leader| leader != pid)
        || !relaunchable
        || !in_front()
        || crate::input_stall::host_hand_refusal(ctx, now_us).is_some_and(|why| Some(why) != waived)
    {
        return None;
    }
    Some(Offer {
        pid: snap.pid,
        promised,
    })
}

/// The `restart` verb as dispatch runs it, on `by`'s ask ([`restart_in`],
/// [`crate::harness_host::harness_state`]).
#[cfg(unix)]
pub(crate) fn restart(
    master: i32,
    rest: &str,
    ctx: &Arc<SessionCtx>,
    by: aterm_slog::Actor,
) -> String {
    restart_in(
        master,
        rest,
        ctx,
        crate::harness_host::harness_state().as_deref(),
        by,
    )
}

/// Runs [`begin`] ([`Stamped::signal`]).
#[cfg(unix)]
pub(crate) fn restart_in(
    master: i32,
    rest: &str,
    ctx: &Arc<SessionCtx>,
    state: Option<&std::path::Path>,
    by: aterm_slog::Actor,
) -> String {
    match begin(master, rest, ctx, state, by) {
        Ok(stamped) => stamped.signal(),
        Err(reply) => reply,
    }
}

/// First half of the act (steps 1–3): reads the leader and its start, checks fences, and writes or joins a standing stamp for the process; `by` is who asked (the session log's `act word=restart by=`, which [`term`] writes).
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "Stamp",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
pub(crate) fn begin<'a>(
    master: i32,
    rest: &str,
    ctx: &'a Arc<SessionCtx>,
    state: Option<&'a std::path::Path>,
    by: aterm_slog::Actor,
) -> Result<Stamped<'a>, String> {
    let ask = parse(rest)?;
    let one_act = ACTING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
    let pgrp = unsafe { libc::tcgetpgrp(master) };
    if pgrp <= 0 {
        return Err("ERR no foreground process group\n".to_string());
    }
    if let Some(pid) = ask.pid
        && pid != pgrp
    {
        return Err(format!(
            "ERR pid {pid} is not the foreground process group ({pgrp})\n"
        ));
    }
    let refuse = |why: &str| format!("ERR restart unavailable: {why}\n");
    let Some(start) = u32::try_from(pgrp)
        .ok()
        .and_then(restart_stamp::kernel_start)
    else {
        return Err(refuse("start-unread"));
    };
    let now_us = crate::metrics::now_us();
    if let Some(why) = unavailable(ctx, pgrp, &start, now_us) {
        return Err(refuse(why));
    }
    let Some(state) = state else {
        return Err(refuse("no-state"));
    };
    let stalled = stalled_for(
        &ctx.timeline.lock().unwrap_or_else(|p| p.into_inner()),
        pgrp,
    );
    if ask.if_stalled && !stalled {
        return Err("ERR not-stalled\n".to_string());
    }
    if let Some(grace_s) = ask.quiet
        && let Some(ms) = ctx
            .human_input
            .ms_since(now_us)
            .filter(|ms| *ms < u64::from(grace_s).saturating_mul(1000))
    {
        return Err(format!("ERR busy person human_ms={ms}\n"));
    }
    let sid = ctx.self_id.as_str();
    let pid = u32::try_from(pgrp).unwrap_or(0);
    let now = aterm_time::unix_secs().unwrap_or(0);
    if let Some(stamp) = restart_stamp::standing(state, sid, pid, &start, now) {
        return Ok(Stamped {
            master,
            ctx,
            state,
            pgrp,
            stamp,
            wrote: false,
            by,
            _one_act: one_act,
        });
    }
    let stamp = Stamp {
        pid,
        start,
        cause: if stalled { Cause::Stall } else { Cause::Asked },
        at: now,
        input: restart_stamp::input_host().map(|host| restart_stamp::Input {
            host,
            seq: ctx.human_input.seq(),
        }),
    };
    if let Err(e) = restart_stamp::write(state, sid, &stamp) {
        return Err(format!("ERR restart failed: stamp ({e})\n"));
    }
    Ok(Stamped {
        master,
        ctx,
        state,
        pgrp,
        stamp,
        wrote: true,
        by,
        _one_act: one_act,
    })
}

static ACTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// An act stamped by [`begin`] and not yet signalled, holding [`ACTING`].
#[cfg(unix)]
pub(crate) struct Stamped<'a> {
    master: i32,
    ctx: &'a Arc<SessionCtx>,
    state: &'a std::path::Path,
    pgrp: i32,
    /// The stamp its exit is read by: this act's, or the one it joined.
    stamp: Stamp,
    /// Whether this act wrote [`Self::stamp`] (else it joined it).
    wrote: bool,
    /// Who asked for it.
    by: aterm_slog::Actor,
    _one_act: std::sync::MutexGuard<'static, ()>,
}

#[cfg(unix)]
impl Stamped<'_> {
    /// Second half of the act ([`take_back`]).
    pub(crate) fn signal(self) -> String {
        let Self {
            master,
            ctx,
            state,
            pgrp,
            stamp,
            wrote,
            by,
            _one_act,
        } = self;
        let sid = ctx.self_id.as_str();
        let cause = stamp.cause;
        if scoped_relaunch(ctx, pgrp) {
            record_restart(ctx, cause);
            aterm_log::info!(
                "restart @{sid}: pid {pgrp} is a scoped harness; sent restart event (cause={})",
                cause.word()
            );
            return format!("OK restarting pid {pgrp} cause={}\n", cause.word());
        }
        let asker = crate::session_log::EndAsker {
            timeline: &ctx.timeline,
            by,
            from: None,
        };
        match term(master, &ctx.sink, pgrp, &stamp.start, asker) {
            Signalled::Gone => {
                let said = take_back(state, sid, &stamp, wrote);
                aterm_log::info!(
                    "restart @{sid}: pid {pgrp} no longer leads its tab; nothing sent, {said}"
                );
                format!("OK restart gone pid {pgrp}\n")
            }
            Signalled::Failed => {
                let said = take_back(state, sid, &stamp, wrote);
                aterm_log::info!("restart @{sid}: killpg refused for pid {pgrp}; {said}");
                format!("ERR restart failed: killpg pid {pgrp}\n")
            }
            Signalled::Sent(discarded) => {
                aterm_log::info!(
                    "restart @{sid}: pid {pgrp} stamped and sent SIGTERM (cause={})",
                    cause.word()
                );
                let stamp = if wrote {
                    stamp
                } else {
                    renew(state, sid, stamp)
                };
                spawn_escalation(Arc::downgrade(ctx), state, pgrp, &stamp);
                let tail = discarded.map_or_else(String::new, |n| format!(" discarded={n}"));
                format!("OK restarting pid {pgrp} cause={}{tail}\n", cause.word())
            }
        }
    }
}

/// Renews a joined stamp once `SIGTERM` has been sent ([`KILL_READ_MARGIN`]).
#[cfg(unix)]
fn renew(state: &std::path::Path, sid: &str, joined: Stamp) -> Stamp {
    let at = aterm_time::unix_secs().unwrap_or(joined.at).max(joined.at);
    let renewed = Stamp {
        at,
        ..joined.clone()
    };
    match restart_stamp::write(state, sid, &renewed) {
        Ok(()) => renewed,
        Err(e) => {
            aterm_log::warn!(
                "restart @{sid}: the stamp this act joined could not be renewed ({e}); its \
                 escalation watches by it as it stands"
            );
            joined
        }
    }
}

/// Windows: no process groups to signal.
#[cfg(not(unix))]
pub(crate) fn restart(
    master: i32,
    rest: &str,
    ctx: &Arc<SessionCtx>,
    by: aterm_slog::Actor,
) -> String {
    let _ = (master, rest, ctx, by);
    "ERR restart unsupported on this platform\n".to_string()
}

/// Takes back a stamp this act wrote when no signal was sent (step 4: leader
/// gone or `killpg` refused).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "SendsNothing",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
fn take_back(state: &std::path::Path, sid: &str, stamp: &Stamp, wrote: bool) -> &'static str {
    if !wrote {
        return "the stamp it joined kept";
    }
    if restart_stamp::take_if(state, sid, |s| s == stamp).is_some() {
        "its stamp taken back"
    } else {
        "its stamp already read"
    }
}

/// What the act's `SIGTERM` came to.
#[cfg(unix)]
enum Signalled {
    /// Sent; the input dropped before it, when counted.
    Sent(Option<usize>),
    /// The stamped leader no longer leads the tab (step 4): nothing sent.
    Gone,
    /// `killpg` refused.
    Failed,
}

/// Whether the process `pgrp` started at `start` still leads `master`'s foreground group.
#[cfg(unix)]
pub(crate) fn leads(master: i32, pgrp: i32, start: &str) -> bool {
    // SAFETY: `tcgetpgrp` only reads the master's foreground group.
    let front = unsafe { libc::tcgetpgrp(master) };
    front == pgrp
        && u32::try_from(pgrp)
            .ok()
            .and_then(restart_stamp::kernel_start)
            .as_deref()
            == Some(start)
}

/// Sends the act's `SIGTERM` (step 5) after re-reading the leader (step 4),
/// dropping unread input first and waking the watch after. Between the last re-read and the
/// `killpg`, `asker` writes the act's mark and its ask ([`crate::session_log::EndAsker::ask`]),
/// so an act that sends nothing writes nothing; a refused `killpg` takes the ask back
/// ([`crate::session_log::EndAsker::delivered`]).
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "Term",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
fn term(
    master: i32,
    sink: &aterm_session::sink::SinkWriter,
    pgrp: i32,
    start: &str,
    asker: crate::session_log::EndAsker<'_>,
) -> Signalled {
    if !leads(master, pgrp, start) {
        return Signalled::Gone;
    }
    let discarded = crate::input_stall::discard_before_signal(master, sink, libc::SIGTERM);
    if !leads(master, pgrp, start) {
        return Signalled::Gone;
    }
    asker.ask(pgrp, crate::session_log::AskWord::Restart);
    // SAFETY: `killpg` on the tab's own foreground group, re-read just above.
    let rc = unsafe { libc::killpg(pgrp, libc::SIGTERM) };
    crate::input_stall::after_signal(sink, libc::SIGTERM);
    if asker.delivered(pgrp, rc) {
        Signalled::Sent(discarded)
    } else {
        Signalled::Failed
    }
}

/// Active escalation and carrier watches ([`watch`]).
static ESCALATING: std::sync::Mutex<Vec<Watched>> = std::sync::Mutex::new(Vec::new());

/// One running watch in [`ESCALATING`].
struct Watched {
    /// Matches [`Watch::id`].
    id: u64,
    pgrp: i32,
    start: String,
    /// Latest instant the watched stamp stands until; raised by joining acts,
    /// never lowered.
    stands_until: Instant,
}

/// A watch on one process ([`ESCALATING`], [`watch`], [`Watch::left`]).
pub(crate) struct Watch {
    id: u64,
    pgrp: i32,
    pid: u32,
    start: String,
    /// Harness state directory holding the tab's stamp, re-read on every look ([`Watch::named`]).
    state: std::path::PathBuf,
}

/// Registers a watch for `pgrp` at `stamp`, or raises the `stands_until` time of an existing watch
/// for the same process and returns `None`.
pub(crate) fn watch(state: &std::path::Path, pgrp: i32, stamp: &Stamp) -> Option<Watch> {
    let until = stands_until(stamp);
    with_watches(|listed| {
        if let Some(running) = listed
            .iter_mut()
            .find(|w| w.pgrp == pgrp && w.start == stamp.start)
        {
            running.stands_until = running.stands_until.max(until);
            return None;
        }
        Some(Watch::listed_in(listed, state, pgrp, stamp, until))
    })
}

/// Lists a watch beside any already running on the same process.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn watch_beside(state: &std::path::Path, pgrp: i32, stamp: &Stamp) -> Watch {
    let until = stands_until(stamp);
    with_watches(|listed| Watch::listed_in(listed, state, pgrp, stamp, until))
}

/// How many watches run on `pgrp` now (one at most, outside [`watch_beside`]).
#[cfg(test)]
pub(crate) fn watches(pgrp: i32) -> usize {
    with_watches(|listed| listed.iter().filter(|w| w.pgrp == pgrp).count())
}

/// Whether an escalation (or a carrier) watches `pgrp` now.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn escalating(pgrp: i32) -> bool {
    watches(pgrp) > 0
}

/// Locks [`ESCALATING`] for `f` ([`Watch::left`]).
fn with_watches<R>(f: impl FnOnce(&mut Vec<Watched>) -> R) -> R {
    f(&mut ESCALATING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

fn stands_until(stamp: &Stamp) -> Instant {
    let now = Instant::now();
    now.checked_add(stamp.stands_for(aterm_time::unix_secs().unwrap_or(0)))
        .unwrap_or(now)
}

fn kill_until_of(stands_until: Instant) -> Option<Instant> {
    stands_until.checked_sub(KILL_READ_MARGIN)
}

/// Why a watch lets its program go ([`Watch::left`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LetGo {
    /// The latest stamp it watches by no longer stands past [`KILL_READ_MARGIN`], or the watch is
    /// no longer listed.
    Lapsed,
    /// The tab's stamp on disk no longer names its program (replaced by another
    /// act or taken).
    Unnamed,
}

impl Watch {
    fn listed_in(
        listed: &mut Vec<Watched>,
        state: &std::path::Path,
        pgrp: i32,
        stamp: &Stamp,
        until: Instant,
    ) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        listed.push(Watched {
            id,
            pgrp,
            start: stamp.start.clone(),
            stands_until: until,
        });
        Self {
            id,
            pgrp,
            pid: stamp.pid,
            start: stamp.start.clone(),
            state: state.to_path_buf(),
        }
    }

    fn named(&self, tab: &str) -> bool {
        let now = aterm_time::unix_secs().unwrap_or(0);
        restart_stamp::standing(&self.state, tab, self.pid, &self.start, now).is_some()
    }

    /// Whether the last instant to send `SIGKILL` ([`kill_until_of`]) has passed or the watch is
    /// no longer listed.
    fn lapsed(&self) -> bool {
        with_watches(|listed| {
            listed
                .iter()
                .find(|w| w.id == self.id)
                .and_then(|w| kill_until_of(w.stands_until))
                .is_none_or(|until| Instant::now() >= until)
        })
    }

    /// Remaining wait duration for `tab`, or why the watch lets go ([`ESCALATING`], [`watch`]).
    fn left(&self, tab: &str) -> Result<Duration, LetGo> {
        with_watches(|listed| {
            let at = listed
                .iter()
                .position(|w| w.id == self.id)
                .ok_or(LetGo::Lapsed)?;
            let left = kill_until_of(listed[at].stands_until).map_or(Duration::ZERO, |until| {
                until.saturating_duration_since(Instant::now())
            });
            let gone = if left.is_zero() {
                Some(LetGo::Lapsed)
            } else if !self.named(tab) {
                Some(LetGo::Unnamed)
            } else {
                None
            };
            if let Some(why) = gone {
                listed.swap_remove(at);
                return Err(why);
            }
            Ok(left)
        })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        with_watches(|listed| {
            if let Some(at) = listed.iter().position(|w| w.id == self.id) {
                listed.swap_remove(at);
            }
        });
    }
}

/// Processes whose watch holds `SIGKILL` back ([`kill`], [`another_hand`], [`read_by_host`]).
static HELD_OFF: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());

/// Whether the watch on `pgrp` is waiting out another hand on its tab, or its host's read of it.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn held_off(pgrp: i32) -> bool {
    HELD_OFF
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&pgrp)
}

/// One watch listed in `list` ([`HELD_OFF`]) while this lives.
struct Listed {
    list: &'static std::sync::Mutex<Vec<i32>>,
    pgrp: i32,
}

impl Listed {
    fn on(list: &'static std::sync::Mutex<Vec<i32>>, pgrp: i32) -> Self {
        list.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(pgrp);
        Self { list, pgrp }
    }
}

impl Drop for Listed {
    fn drop(&mut self) {
        let mut list = self
            .list
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = list.iter().position(|p| *p == self.pgrp) {
            list.swap_remove(at);
        }
    }
}

/// Whether another hand delays `SIGKILL` ([`aterm_agent::harness::upgrade_drive::is_host_holder`]).
pub(crate) fn another_hand(ctx: &SessionCtx, now_us: u64) -> bool {
    let claimed = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .live_claim(now_us)
        .is_some_and(|claim| {
            claim.proven.is_none()
                && !aterm_agent::harness::upgrade_drive::is_host_holder(&claim.holder)
        });
    if claimed || ctx.fabric.hold().is_some() {
        return true;
    }
    let own = crate::harness_host::holder();
    let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
    match lease.as_ref() {
        Some(crate::Lease::Turn { driver, .. }) => driver.is_some(),
        Some(crate::Lease::Drive {
            holder, expires_us, ..
        }) => *expires_us > now_us && *holder != own,
        None => false,
    }
}

/// How often a watch looks again at a stamped program that does not lead its tab ([`kill`]).
pub(crate) const LOOK_AGAIN: Duration = Duration::from_secs(1);

/// Minimum test value of [`LOOK_AGAIN`].
const LOOK_AGAIN_TEST_FLOOR: Duration = Duration::from_millis(20);

/// The cadence a watch looks again at: [`LOOK_AGAIN`] in a shipped build.
pub(crate) fn look_again() -> Duration {
    aterm_gui_timing::current().of_at_least(LOOK_AGAIN, LOOK_AGAIN_TEST_FLOOR)
}

/// Whether `tab`'s snapshot names `pid` at `start` ([`crate::harness_host::published_snapshot`]).
fn read_by_host(tab: &str, pid: u32, start: &str) -> bool {
    crate::harness_host::published_snapshot(tab)
        .is_some_and(|snap| snap.pid == pid && snap.start == start)
}

/// Minimum remaining stamp lifetime required before [`kill`] or [`carry`] sends `SIGKILL`.
pub(crate) const KILL_READ_MARGIN: Duration = Duration::from_secs(30);

/// Start the act's escalation thread for the leader `pgrp` its `stamp` names, unless a watch
/// already ends that very process ([`watch`]).
#[cfg(unix)]
fn spawn_escalation(ctx: Weak<SessionCtx>, state: &std::path::Path, pgrp: i32, stamp: &Stamp) {
    let cause = stamp.cause;
    let Some(watch) = watch(state, pgrp, stamp) else {
        aterm_log::info!(
            "restart: pid {pgrp} (cause={}): already watched; that watch ends it by this \
             act's stamp",
            cause.word()
        );
        return;
    };
    let deadline = deadline();
    let spawned = std::thread::Builder::new()
        .name(format!("aterm-restart-{pgrp}"))
        .spawn(move || {
            let said = escalate(&ctx, &watch, grace(), deadline);
            aterm_log::info!("restart: pid {pgrp} (cause={}): {said}", cause.word());
        });
    if let Err(e) = spawned {
        aterm_log::warn!("restart: pid {pgrp}'s escalation could not start: {e}");
    }
}

/// Escalation wait (step 6): waits for `watch.pid` to exit ([`kill`]).
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "EscalationSeesExit",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
pub(crate) fn escalate(
    ctx: &Weak<SessionCtx>,
    watch: &Watch,
    grace: Duration,
    deadline: Duration,
) -> String {
    let exited = |within| aterm_agent::harness::upgrade_wake::wait_exit(watch.pid, within);
    if exited(grace) {
        return "ended on its SIGTERM".to_string();
    }
    let deaf = ctx.upgrade().is_some_and(|ctx| {
        let stalled = stalled_for(
            &ctx.timeline.lock().unwrap_or_else(|p| p.into_inner()),
            watch.pgrp,
        );
        stalled && !ctx.sink.read_since_discard()
    });
    if !deaf && exited(deadline.saturating_sub(grace)) {
        return "ended on its SIGTERM".to_string();
    }
    let why = if deaf {
        "deaf to SIGTERM (its stall stands, nothing read)"
    } else {
        "still running at the act's deadline"
    };
    format!("{why}: {}", kill(ctx, watch))
}

/// Why one look of [`kill`] sent nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Withheld {
    /// It does not lead its tab (e.g. a stopped job with its shell in front).
    Away,
    /// Another hand holds the session ([`another_hand`]).
    Hand,
    /// Its stamp no longer stands past [`KILL_READ_MARGIN`], or the tab's on-disk stamp no longer
    /// names it.
    Lapsed,
    /// The host holds no snapshot of it yet ([`read_by_host`]).
    Unread,
}

impl Withheld {
    const fn gave_up(self) -> &'static str {
        match self {
            Self::Away => "never led its tab again while its stamp stood; nothing sent",
            Self::Hand => "another hand held its tab while its stamp stood; nothing sent",
            Self::Lapsed => "its stamp no longer stands long enough to be read; nothing sent",
            Self::Unread => "the host never read it again while its stamp stood; nothing sent",
        }
    }

    const fn ended(self) -> &'static str {
        match self {
            Self::Hand => "ended while another hand held its tab; nothing sent",
            Self::Unread => "ended before the host read it again; nothing sent",
            Self::Away | Self::Lapsed => "ended away from its tab; nothing sent",
        }
    }
}

/// Sends `SIGKILL` to `watch`'s leader after re-checking before both input
/// discard and `killpg` that the process still leads its tab ([`leads`]), no
/// other hand holds the session ([`another_hand`]), its stamp still stands on
/// disk with [`KILL_READ_MARGIN`] to spare ([`Watch::lapsed`], [`Watch::named`]),
/// and the host holds its snapshot ([`read_by_host`]). Just before the `killpg` it renews the
/// end its act asked of the group ([`crate::session_log::renew_ask`]), and writes no mark; a
/// refused `SIGKILL` leaves that ask standing, since the `SIGTERM` before it was delivered and a
/// group that just led its tab and is now gone ended on it.
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "Escalate",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
fn kill(ctx: &Weak<SessionCtx>, watch: &Watch) -> &'static str {
    let (pgrp, pid, start) = (watch.pgrp, watch.pid, watch.start.as_str());
    let mut held_off: Option<(Withheld, Listed)> = None;
    loop {
        let Some(ctx) = ctx.upgrade() else {
            return "its session is gone; nothing sent";
        };
        let master = ctx.sink.master();
        let tab = ctx.self_id.as_str().to_string();
        let withheld = || {
            if !leads(master, pgrp, start) {
                Some(Withheld::Away)
            } else if another_hand(&ctx, crate::metrics::now_us()) {
                Some(Withheld::Hand)
            } else if watch.lapsed() || !watch.named(&tab) {
                Some(Withheld::Lapsed)
            } else if !read_by_host(&tab, pid, start) {
                Some(Withheld::Unread)
            } else {
                None
            }
        };
        let mut why = withheld();
        if why.is_none() {
            let _ = crate::input_stall::discard_before_signal(master, &ctx.sink, libc::SIGKILL);
            why = withheld();
            if why.is_none() {
                crate::session_log::renew_ask(&ctx.timeline, pgrp);
                // SAFETY: `killpg` on the tab's own foreground group, re-read just above.
                let rc = unsafe { libc::killpg(pgrp, libc::SIGKILL) };
                crate::input_stall::after_signal(&ctx.sink, libc::SIGKILL);
                return if rc == 0 {
                    "sent SIGKILL"
                } else {
                    "SIGKILL refused"
                };
            }
        }
        drop(ctx);
        let why = why.unwrap_or(Withheld::Away);
        if !matches!(why, Withheld::Hand | Withheld::Unread) {
            held_off = None;
        } else if held_off.as_ref().is_none_or(|(was, _)| *was != why) {
            if why == Withheld::Hand {
                aterm_log::info!(
                    "restart: pid {pgrp}: another hand holds its tab (a hold, another driver's \
                     lease or turn, another supervisor's claim); its SIGKILL waits while its stamp \
                     stands"
                );
            } else {
                aterm_log::info!(
                    "restart: pid {pgrp}: the host has not read it again (a successor that has not \
                     named it since its Commit); its SIGKILL waits for that read while its stamp \
                     stands"
                );
            }
            held_off = Some((why, Listed::on(&HELD_OFF, pgrp)));
        }
        if !restart_stamp::alive_as(pid, start) {
            return "ended; nothing sent";
        }
        let left = match watch.left(&tab) {
            Ok(left) => left,
            Err(LetGo::Lapsed) => return why.gave_up(),
            Err(LetGo::Unnamed) => {
                return "its tab's stamp names it no more (another act's replaced it); nothing sent";
            }
        };
        if aterm_agent::harness::upgrade_wake::wait_exit(pid, left.min(look_again())) {
            return why.ended();
        }
    }
}

/// Finishes an in-flight restart across handoff ([`KILL_READ_MARGIN`], [`DEADLINE`], [`kill`]).
/// The act's end stands as aterm's ask again in this log ([`crate::session_log::EndAsker::stand`]),
/// with no mark: the predecessor wrote it.
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "CarrierFinishes",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
pub(crate) fn carry(ctx: &Arc<SessionCtx>, state: &std::path::Path, now_s: u64) -> bool {
    let Some(stamp) = restart_stamp::read(state, ctx.self_id.as_str()) else {
        return false;
    };
    let Ok(pgrp) = i32::try_from(stamp.pid) else {
        return false;
    };
    if stamp.stands_for(now_s) <= KILL_READ_MARGIN {
        return false;
    }
    let left = DEADLINE.saturating_sub(Duration::from_secs(stamp.age_s(now_s)));
    // The act this carries ends its program on its asker's word; its predecessor wrote its mark,
    // and this log, which carries no ask across the update, stands it again as aterm's.
    crate::session_log::EndAsker {
        timeline: &ctx.timeline,
        by: aterm_slog::Actor::Aterm,
        from: None,
    }
    .stand(pgrp);
    let Some(watch) = watch(state, pgrp, &stamp) else {
        aterm_log::info!(
            "restart: pid {pgrp}: an act a Commit cut is watched already; that watch ends it"
        );
        return true;
    };
    let weak = Arc::downgrade(ctx);
    let spawned = std::thread::Builder::new()
        .name(format!("aterm-restart-carry-{pgrp}"))
        .spawn(move || {
            if !restart_stamp::alive_as(watch.pid, &watch.start)
                || aterm_agent::harness::upgrade_wake::wait_exit(watch.pid, left)
            {
                return;
            }
            let said = kill(&weak, &watch);
            aterm_log::info!(
                "restart: carried an act a Commit cut: pid {pgrp} still ran {}s after its \
                 stamp: {said}",
                DEADLINE.as_secs()
            );
        });
    if let Err(e) = spawned {
        aterm_log::warn!("restart: the carry of pid {pgrp}'s act could not start: {e}");
        return false;
    }
    true
}

impl crate::App {
    /// Runs [`carry`] at a successor's Commit for every adopted session.
    pub(crate) fn carry_cut_restarts(&self) {
        #[cfg(unix)]
        if let Some(state) = crate::harness_host::harness_state() {
            let now_s = aterm_time::unix_secs().unwrap_or(0);
            for session in self.pool.iter().filter(|s| s.handoff_local_id.is_some()) {
                let _ = carry(&session.ctx, &state, now_s);
            }
        }
    }
}

#[cfg(test)]
#[path = "restart_act_tests.rs"]
mod tests;
