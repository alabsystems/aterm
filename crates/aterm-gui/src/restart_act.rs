// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit in-place restart: `aterm ctl @<sid> restart [pid=<n>]
//! [if-stalled] [quiet=<s>]`. The durable record and exit lookup are
//! [`aterm_agent::harness::restart_stamp`]; the design and conformance model is
//! `aterm-spec::derive::harness_restart_stamp_model` (`HarnessRestartStamp`).
//!
//! 1. Read the tab's foreground group leader and kernel start. `pid=`, when
//!    supplied, must name that leader.
//! 2. Before writing or signalling, require the host's snapshot to name that
//!    process and a relaunchable lane plan or validated self-claim copy
//!    ([`unavailable`]). A Claude Code's plan must carry the Claude home its
//!    records are in (`relaunch::carries_restart` for the user's home, the
//!    plan's own `refused:home` rule): an agent whose relaunch would refuse its home is
//!    refused here rather than ended and left at its shell. The
//!    session must have this host's or a proven self-claim's supervision, with
//!    no hold, driver lease or named turn. `if-stalled` requires a running
//!    stall for the leader; `quiet=` refuses recent human input.
//! 3. Serialize requests through [`ACTING`]. Write one atomic stamp per tab before
//!    any discard or signal, without a sweep lock, or join a standing stamp for
//!    the same process. A joined stamp keeps its cause and is renewed only after
//!    a successful `SIGTERM` ([`renew`]).
//! 4. Re-read the leader and its start before each discard and signal ([`leads`]).
//!    If it moved, send nothing and take back only a stamp this act wrote
//!    ([`take_back`]); a joined stamp may already account for a prior signal.
//! 5. Discard unread input and send `SIGTERM`, replying at once with
//!    `OK restarting pid <n> cause=<stall|asked>`. The request does not wait through
//!    the grace period ([`Stamped::signal`]).
//! 6. Keep one escalation watch per process ([`watch`]). A deaf survivor is killed
//!    after [`GRACE`]; any survivor after [`DEADLINE`]. A process away from the
//!    foreground is watched until it leads again, while its stamp still stands.
//! 7. Before each escalation discard or `SIGKILL`, recheck for another supervisor,
//!    hold, driver lease or named turn ([`another_hand`]). Wait while that hand
//!    stands, and stop without signalling if the stamp expires. Never kill in the
//!    stamp's last [`KILL_READ_MARGIN`], leaving its exit reader time to find it.
//! 8. Re-read the tab's on-disk stamp and host snapshot on every escalation look
//!    ([`kill`]). A replaced or taken stamp ends the watch; a missing host snapshot
//!    postpones the kill until the host can read the resulting exit.
//!
//! The stamp survives an instance handoff: the successor's [`carry`] finishes a
//! surviving act after [`DEADLINE`], subject to the same identity, hand and expiry
//! checks. The exit reader finds the stamp by lookup; the relaunch that files its
//! record consumes it. An asked exit under a hold, lease or turn is carried until
//! that hand lifts; another supervisor's claim leaves the exit to that supervisor.
//!
//! `[harness] relaunch = false` does not refuse an explicit restart. Automatic
//! stall termination still requires `[harness] stall_term = true` (default false).

#![cfg_attr(not(unix), allow(dead_code))]

use std::sync::Arc;
#[cfg(unix)]
use std::sync::Weak;
use std::time::{Duration, Instant};

#[cfg(any(unix, test))]
use aterm_agent::harness::restart_stamp::Cause;
use aterm_agent::harness::restart_stamp::{self, Stamp};

use crate::SessionCtx;

/// How long after its `SIGTERM` a deaf program (its stall still published,
/// nothing read since the drop) is given before the act's `SIGKILL`: the
/// grace after which the stall's attention already names `signal kill`
/// ([`crate::input_stall::RESTART_GRACE`]). Waited through [`grace`].
pub(crate) const GRACE: Duration = crate::input_stall::RESTART_GRACE;

/// Minimum test value of [`GRACE`]. The grace decides which signal ends a
/// program only when the stall marks it deaf ([`escalate`]); one with no stall
/// published is given [`DEADLINE`].
const GRACE_TEST_FLOOR: Duration = Duration::from_secs(1);

/// The grace the act waits: [`GRACE`] in a shipped build, or its floored test
/// value on `aterm_gui_timing`.
fn grace() -> Duration {
    aterm_gui_timing::current().of_at_least(GRACE, GRACE_TEST_FLOOR)
}

/// The act's deadline in any case: a program that still leads its tab this
/// long after `SIGTERM` is ended with `SIGKILL` so a later manual `/exit` cannot
/// be mistaken for the act's exit and relaunched.
pub(crate) const DEADLINE: Duration = Duration::from_secs(30);

/// The verb's arguments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ask {
    /// `pid=<n>`: only while `n` leads the tab's foreground group.
    pub(crate) pid: Option<i32>,
    /// `if-stalled`: only while a running stall is published for it.
    pub(crate) if_stalled: bool,
    /// `quiet=<s>`: refused while a person gave the session input within `s`
    /// seconds.
    pub(crate) quiet: Option<u32>,
}

/// Pure parser for `restart [pid=<n>] [if-stalled] [quiet=<s>]`, in any
/// order, each at most once.
pub(crate) fn parse(rest: &str) -> Result<Ask, String> {
    let mut ask = Ask::default();
    let mut stalled_seen = false;
    for w in rest.split_whitespace() {
        let bad = || format!("ERR bad restart argument: {w}\n");
        if w == "if-stalled" && !stalled_seen {
            stalled_seen = true;
            ask.if_stalled = true;
        } else if let Some(s) = w.strip_prefix("quiet=") {
            match s.parse::<u32>() {
                Ok(s) if ask.quiet.is_none() => ask.quiet = Some(s),
                _ => return Err(bad()),
            }
        } else if let Some(n) = w.strip_prefix("pid=") {
            match n.parse::<i32>() {
                Ok(n) if n > 1 && ask.pid.is_none() => ask.pid = Some(n),
                _ => return Err(bad()),
            }
        } else {
            return Err(bad());
        }
    }
    Ok(ask)
}

/// Whether a running stall is published in `tl` for the foreground group
/// `pgrp`: one over that leader, or — when its leader was not read — over a
/// running program (never a stopped job, whose remedy is `signal cont`).
pub(crate) fn stalled_for(tl: &crate::session_timeline::SessionTimeline, pgrp: i32) -> bool {
    tl.agent().input.as_ref().is_some_and(|fact| {
        fact.restart
            .leader
            .map_or(!fact.stopped, |leader| leader == pgrp)
    })
}

/// Why the act cannot be made in `ctx`'s session for the leader `pgrp`
/// started at `start` (`None` if it can): no host snapshot names that process
/// (`not-the-hosts-agent`), its relaunch would not bring it back
/// (`not-relaunchable`), or the host does not hold supervision
/// ([`crate::input_stall::host_hand_refusal`]: `not-supervised`, `halted`,
/// `driver-lease`, `driver-turn`).
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

/// [`unavailable`] for the tab's shell's home `home`, which a test names so the
/// relaunch's predicate never reads the person's own (its hook files).
fn unavailable_for(
    ctx: &SessionCtx,
    pgrp: i32,
    start: &str,
    now_us: u64,
    home: Option<&std::path::Path>,
) -> Option<&'static str> {
    let snap = crate::harness_host::published_snapshot(ctx.self_id.as_str())
        .filter(|snap| i32::try_from(snap.pid).ok() == Some(pgrp) && snap.start == start);
    let Some(snap) = snap else {
        return Some("not-the-hosts-agent");
    };
    // A self-supervised agent resumes on the validated line from aterm's copy of
    // its record; any other uses its lane's relaunch predicate for the tab's home.
    let relaunchable = match crate::self_claim::copy_of_tab(ctx.self_id.as_str())
        .filter(|copy| copy.pid == snap.pid && copy.start == snap.start)
    {
        Some(copy) => copy.resume.is_ok(),
        None => {
            lane_relaunches(&snap)
                && home.is_some_and(|home| {
                    aterm_agent::harness::relaunch::carries_restart(home, &snap)
                })
        }
    };
    if !relaunchable {
        return Some("not-relaunchable");
    }
    crate::input_stall::host_hand_refusal(ctx, now_us)
}

/// Whether `snap`'s agent can be relaunched by its lane without a self-claim
/// copy. Only Claude Code and agy support lane-based restart (`SIGTERM` exits
/// gracefully and `relaunch::carries_restart` checks the rest). Codex requires a
/// validated self-claim copy; Muse Code's mid-turn `SIGTERM` is unsupported;
/// Gemini CLI's parent node process ignores `SIGTERM` and `SIGKILL` would orphan
/// its child.
fn lane_relaunches(snap: &aterm_agent::harness::relaunch::Snapshot) -> bool {
    match &snap.run {
        aterm_agent::harness::relaunch::AgentRun::Claude
        | aterm_agent::harness::relaunch::AgentRun::Agy(_) => true,
        aterm_agent::harness::relaunch::AgentRun::Codex(_)
        | aterm_agent::harness::relaunch::AgentRun::Muse(_)
        | aterm_agent::harness::relaunch::AgentRun::Gemini(_) => false,
    }
}

/// Where the explicit restart would be carried ([`offered`]): the process it
/// names, and whether its exit is relaunched on the agent's own conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Offer {
    /// The process the act names (`restart pid=<n> if-stalled`): the host's
    /// snapshot's, the stall's leader where the watch read one.
    pub(crate) pid: u32,
    /// Whether the offer promises relaunch on the conversation: a validated
    /// self-claim resume line with a shell, or a lane plan satisfying
    /// [`aterm_agent::harness::relaunch::resumes_on_exit`]. The words then use
    /// [`aterm_agent::harness::resume::AfterRestart::Relaunch`]; otherwise they
    /// name the act alone ([`aterm_agent::harness::resume::AfterRestart::Act`]).
    pub(crate) promised: bool,
}

/// The shared restart offer for stall refusals, attention entries, menus,
/// bands and supervisor mail. The published snapshot must name the stall's
/// leader (when supplied), its agent must be in front, its lane plan or
/// self-claim copy must be relaunchable, and the host's hand must be admitted
/// ([`crate::input_stall::host_hand_refusal`]). No process I/O here: the act
/// itself re-reads the leader and its start before signalling.
///
/// The words promise a conversation only where [`Offer::promised`]. Without an
/// offer they name `signal term` (`signal kill` for a survivor) and the tab's
/// resume line. `[harness] relaunch` does not refuse an explicit restart.
///
/// Takes only leaf locks for the published snapshot, self-claim copy, timeline,
/// meta, fabric and lease, each released before the next. The caller holds none.
pub(crate) fn offered(ctx: &SessionCtx, leader: Option<i32>, now_us: u64) -> Option<Offer> {
    offered_unless(ctx, leader, now_us, None)
}

/// Whether the act would be offered but for a hold: every fence of
/// [`offered`] holds except the host's hand, which a hold alone refuses
/// (`halted`, [`crate::input_stall::host_hand_refusal`]).
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
    let snap = crate::harness_host::published_snapshot(ctx.self_id.as_str())?;
    let pid = i32::try_from(snap.pid).ok()?;
    let agent = snap.agent().program();
    let in_front = || crate::harness_host::admitted(ctx) == Some(agent);
    let home = aterm_primer::home_dir()?;
    let (relaunchable, promised) = match crate::self_claim::copy_of_tab(ctx.self_id.as_str())
        .filter(|copy| copy.pid == snap.pid && copy.start == snap.start)
    {
        Some(copy) => {
            let ok = copy.resume.is_ok();
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

/// The `restart` verb as control dispatch runs it: [`restart_in`] on
/// the host's harness state directory ([`crate::harness_host::harness_state`]).
#[cfg(unix)]
pub(crate) fn restart(master: i32, rest: &str, ctx: &Arc<SessionCtx>) -> String {
    restart_in(
        master,
        rest,
        ctx,
        crate::harness_host::harness_state().as_deref(),
    )
}

/// Runs [`begin`] (reads, fences, stamp) followed by [`Stamped::signal`]
/// (re-read, discard, `SIGTERM`) against `state`.
#[cfg(unix)]
pub(crate) fn restart_in(
    master: i32,
    rest: &str,
    ctx: &Arc<SessionCtx>,
    state: Option<&std::path::Path>,
) -> String {
    match begin(master, rest, ctx, state) {
        Ok(stamped) => stamped.signal(),
        Err(reply) => reply,
    }
}

/// First half of the act (steps 1–3): reads the leader and its start, checks
/// fences, and writes or joins a standing stamp for the process. `Err` is the
/// refusal reply with nothing written or sent; `Ok` is the stamped act ready for
/// [`Stamped::signal`].
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
    // An act on this very process in flight: joined, its stamp kept as it
    // stands (its cause is the one its exit is read by).
    if let Some(stamp) = restart_stamp::standing(state, sid, pid, &start, now) {
        return Ok(Stamped {
            master,
            ctx,
            state,
            pgrp,
            stamp,
            wrote: false,
            _one_act: one_act,
        });
    }
    let stamp = Stamp {
        pid,
        start,
        cause: if stalled { Cause::Stall } else { Cause::Asked },
        at: now,
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
        _one_act: one_act,
    })
}

/// Serializes acts in this instance from first read to signal so two concurrent
/// acts on one tab cannot both write stamps and have one take back the other's.
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
    _one_act: std::sync::MutexGuard<'static, ()>,
}

#[cfg(unix)]
impl Stamped<'_> {
    /// Second half of the act (steps 4–6): re-reads the leader, discards unread
    /// input, sends `SIGTERM`, and starts the escalation watch — or takes back a
    /// newly written stamp if the leader moved or `killpg` failed ([`take_back`]).
    pub(crate) fn signal(self) -> String {
        let Self {
            master,
            ctx,
            state,
            pgrp,
            stamp,
            wrote,
            _one_act,
        } = self;
        let sid = ctx.self_id.as_str();
        let cause = stamp.cause;
        match term(master, &ctx.sink, pgrp, &stamp.start) {
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

/// Renews a joined stamp with the current timestamp once `SIGTERM` has been
/// sent, preserving its process identity and cause so the escalation watch has a
/// full window beyond [`KILL_READ_MARGIN`]. Never called before `SIGTERM` is sent.
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

/// Windows: no process groups to signal; the verb answers so.
#[cfg(not(unix))]
pub(crate) fn restart(master: i32, rest: &str, ctx: &Arc<SessionCtx>) -> String {
    let _ = (master, rest, ctx);
    "ERR restart unsupported on this platform\n".to_string()
}

/// Takes back a stamp this act wrote when no signal was sent (step 4: leader
/// gone or `killpg` refused). A joined stamp is kept because its earlier act
/// already signalled.
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

/// Whether the process `pgrp` started at `start` still leads `master`'s
/// foreground group (step 4): the same group in front, its leader the same
/// process.
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
/// dropping unread input first and waking the watch after.
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "Term",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
fn term(master: i32, sink: &aterm_session::sink::SinkWriter, pgrp: i32, start: &str) -> Signalled {
    if !leads(master, pgrp, start) {
        return Signalled::Gone;
    }
    let discarded = crate::input_stall::discard_before_signal(master, sink, libc::SIGTERM);
    if !leads(master, pgrp, start) {
        return Signalled::Gone;
    }
    // SAFETY: `killpg` on the tab's own foreground group, re-read just above.
    let rc = unsafe { libc::killpg(pgrp, libc::SIGTERM) };
    crate::input_stall::after_signal(sink, libc::SIGTERM);
    if rc == 0 {
        Signalled::Sent(discarded)
    } else {
        Signalled::Failed
    }
}

/// Active escalation and carrier watches, at most one per process (group and
/// kernel start; [`watch`]), each recording the latest instant its stamp stands
/// until.
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

/// A watch on one process (`pgrp` started at `start`), listed in [`ESCALATING`]
/// from [`watch`] until it lets its program go ([`Watch::left`]) or is dropped.
pub(crate) struct Watch {
    id: u64,
    pgrp: i32,
    pid: u32,
    start: String,
    /// Harness state directory holding the tab's stamp, re-read on every look
    /// ([`Watch::named`]).
    state: std::path::PathBuf,
}

/// Registers a watch for `pgrp` at `stamp`, or raises the `stands_until` time of
/// an existing watch for the same process (`pgrp` and `stamp.start`) and returns
/// `None`. Keeping at most one watch thread per process prevents repeated joined
/// restart requests from accumulating kqueue descriptors and threads. A watch
/// that lets its program go unlists itself in [`Watch::left`] so a subsequent act
/// starts a fresh watch.
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

/// Lists a watch beside any already running on the same process (for tests that
/// run a watch with custom clocks alongside an act's).
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn watch_beside(state: &std::path::Path, pgrp: i32, stamp: &Stamp) -> Watch {
    let until = stands_until(stamp);
    with_watches(|listed| Watch::listed_in(listed, state, pgrp, stamp, until))
}

/// How many watches run on `pgrp` now (one at most, outside
/// [`watch_beside`]).
#[cfg(test)]
pub(crate) fn watches(pgrp: i32) -> usize {
    with_watches(|listed| listed.iter().filter(|w| w.pgrp == pgrp).count())
}

/// Whether an escalation (or a carrier) watches `pgrp` now.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn escalating(pgrp: i32) -> bool {
    watches(pgrp) > 0
}

/// Locks [`ESCALATING`] for `f` (which takes no other lock, though it may read a
/// tab's stamp file in [`Watch::left`]).
fn with_watches<R>(f: impl FnOnce(&mut Vec<Watched>) -> R) -> R {
    f(&mut ESCALATING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// The instant `stamp` stands until on this instance's clock, or `Instant::now()`
/// on overflow (a corrupt future timestamp in the state directory, so its watch
/// sends nothing).
fn stands_until(stamp: &Stamp) -> Instant {
    let now = Instant::now();
    now.checked_add(stamp.stands_for(aterm_time::unix_secs().unwrap_or(0)))
        .unwrap_or(now)
}

/// The last instant a `SIGKILL` leaves its reader a stamp standing until
/// `stands_until` (`None` if the clock cannot represent it).
fn kill_until_of(stands_until: Instant) -> Option<Instant> {
    stands_until.checked_sub(KILL_READ_MARGIN)
}

/// Why a watch lets its program go ([`Watch::left`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LetGo {
    /// The latest stamp it watches by no longer stands past [`KILL_READ_MARGIN`],
    /// or the watch is no longer listed.
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

    /// Whether the tab's on-disk stamp still names this watch's program and has
    /// not expired. Re-read on every look so a replaced or consumed stamp stops
    /// escalation; [`restart_stamp::take_if`] inspects stamps in place so a
    /// non-matching take never transiently hides a standing stamp.
    fn named(&self, tab: &str) -> bool {
        let now = aterm_time::unix_secs().unwrap_or(0);
        restart_stamp::standing(&self.state, tab, self.pid, &self.start, now).is_some()
    }

    /// Whether the last instant to send `SIGKILL` ([`kill_until_of`]) has passed
    /// or the watch is no longer listed.
    fn lapsed(&self) -> bool {
        with_watches(|listed| {
            listed
                .iter()
                .find(|w| w.id == self.id)
                .and_then(|w| kill_until_of(w.stands_until))
                .is_none_or(|until| Instant::now() >= until)
        })
    }

    /// Remaining wait duration for `tab`, or why the watch lets go. Unlists the
    /// watch under [`ESCALATING`]'s lock in the same check that decides to let go
    /// so a concurrent [`watch`] call either raises this watch before the check
    /// or starts a new watch after it.
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

/// The processes whose watch holds its `SIGKILL` back right now for someone
/// else ([`kill`]): another hand holds their tab ([`another_hand`]), or the
/// host holds no snapshot of the program its exit's reader would read it by
/// yet ([`read_by_host`]), by pid: what a test reads to see a watch wait.
static HELD_OFF: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());

/// Whether the watch on `pgrp` is waiting out another hand on its tab, or
/// its host's read of it, now.
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

/// Whether another hand on `ctx`'s session at `now_us` must be waited out before
/// sending `SIGKILL`: a live non-host, non-self-proven supervisor claim
/// ([`aterm_agent::harness::upgrade_drive::is_host_holder`]), a hold, a turn run
/// by a named driver, or another holder's live drive lease. Unlike
/// [`crate::input_stall::host_hand_refusal`] (the start fence), an unclaimed
/// session or one still bearing a predecessor host's claim does not block
/// escalation so a successor's [`carry`] can finish.
pub(crate) fn another_hand(ctx: &SessionCtx, now_us: u64) -> bool {
    // A proven self-claim is the program being restarted, not another supervisor.
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

/// How often a watch looks again at a stamped program that does not lead
/// its tab ([`kill`]: a job stopped from elsewhere, its shell in front).
/// Waited through the one timing value ([`look_again`]).
pub(crate) const LOOK_AGAIN: Duration = Duration::from_secs(1);

/// The least a test value of [`LOOK_AGAIN`] may be: each look reads the
/// tab's stamp from disk and the pty's foreground group.
const LOOK_AGAIN_TEST_FLOOR: Duration = Duration::from_millis(20);

/// The cadence a watch looks again at: [`LOOK_AGAIN`] in a shipped build,
/// its floored test value on the test timing value (`aterm_gui_timing`).
pub(crate) fn look_again() -> Duration {
    aterm_gui_timing::current().of_at_least(LOOK_AGAIN, LOOK_AGAIN_TEST_FLOOR)
}

/// Whether the host's published snapshot for `tab` names `pid` started at
/// `start` ([`crate::harness_host::published_snapshot`]). Escalation and
/// [`carry`] wait for this snapshot before sending `SIGKILL` so the host has a
/// worker to observe and relaunch the resulting exit.
fn read_by_host(tab: &str, pid: u32, start: &str) -> bool {
    crate::harness_host::published_snapshot(tab)
        .is_some_and(|snap| snap.pid == pid && snap.start == start)
}

/// Minimum remaining stamp lifetime required before [`kill`] or [`carry`] sends
/// `SIGKILL`, leaving the exit reader time to look up the stamp before it goes
/// stale (`relaunch::STALE_S`).
pub(crate) const KILL_READ_MARGIN: Duration = Duration::from_secs(30);

/// Start the act's escalation thread for the leader `pgrp` its `stamp` (in the
/// harness state directory `state`) names (step 6) — unless a watch already
/// ends that very process ([`watch`]):
/// then that watch is given `stamp`'s time and nothing starts. Listed in
/// [`ESCALATING`] before this returns, so the verb's reply is never read
/// before its watch.
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
    let spawned = std::thread::Builder::new()
        .name(format!("aterm-restart-{pgrp}"))
        .spawn(move || {
            let said = escalate(&ctx, &watch, grace(), DEADLINE);
            aterm_log::info!("restart: pid {pgrp} (cause={}): {said}", cause.word());
        });
    if let Err(e) = spawned {
        aterm_log::warn!("restart: pid {pgrp}'s escalation could not start: {e}");
    }
}

/// Escalation wait (step 6): waits for `watch.pid` to exit; if still running
/// after `grace` while deaf (stall still published and nothing read since the
/// drop) or after `deadline` in any case, escalates to [`kill`].
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
    /// Its stamp no longer stands past [`KILL_READ_MARGIN`], or the tab's
    /// on-disk stamp no longer names it.
    Lapsed,
    /// The host holds no snapshot of it yet ([`read_by_host`]).
    Unread,
}

impl Withheld {
    /// The watch's word when the stamp stops standing with this the last
    /// thing it saw.
    fn gave_up(self) -> &'static str {
        match self {
            Self::Away => "never led its tab again while its stamp stood; nothing sent",
            Self::Hand => "another hand held its tab while its stamp stood; nothing sent",
            Self::Lapsed => "its stamp no longer stands long enough to be read; nothing sent",
            Self::Unread => "the host never read it again while its stamp stood; nothing sent",
        }
    }

    /// The watch's word when the program ended by itself with this the last
    /// thing it saw.
    fn ended(self) -> &'static str {
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
/// and the host holds its snapshot ([`read_by_host`]). While withheld because the
/// process is away from the foreground, held by another hand, or unread by the
/// host, polls every [`LOOK_AGAIN`] until it can be killed, exits on its own, or
/// its stamp lapses or is replaced ([`Watch::left`]).
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
                // SAFETY: `killpg` on the tab's own foreground group, re-read
                // just above.
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

/// Finishes an in-flight restart across an instance handoff: if `state` holds a
/// stamp for `ctx` with more than [`KILL_READ_MARGIN`] remaining, watches the
/// process on a background thread until the stamp reaches [`DEADLINE`] and calls
/// [`kill`]. Returns whether a watch (newly spawned or already listed in
/// [`watch`]) covers the process.
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "CarrierFinishes",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
pub(crate) fn carry(ctx: &Arc<SessionCtx>, state: &std::path::Path) -> bool {
    let Some(stamp) = restart_stamp::read(state, ctx.self_id.as_str()) else {
        return false;
    };
    let Ok(pgrp) = i32::try_from(stamp.pid) else {
        return false;
    };
    let now = aterm_time::unix_secs().unwrap_or(0);
    if stamp.stands_for(now) <= KILL_READ_MARGIN {
        return false;
    }
    let left = DEADLINE.saturating_sub(Duration::from_secs(stamp.age_s(now)));
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
            for session in self.pool.iter().filter(|s| s.handoff_local_id.is_some()) {
                let _ = carry(&session.ctx, &state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The verb's arguments, in any order, each once; everything else is
    /// refused naming the word.
    #[test]
    fn the_restart_verb_parses_its_three_arguments() {
        assert_eq!(parse(""), Ok(Ask::default()));
        assert_eq!(
            parse("pid=4242 if-stalled quiet=120"),
            Ok(Ask {
                pid: Some(4242),
                if_stalled: true,
                quiet: Some(120),
            })
        );
        assert_eq!(
            parse("quiet=0 pid=7"),
            Ok(Ask {
                pid: Some(7),
                if_stalled: false,
                quiet: Some(0),
            })
        );
        for bad in [
            "pid=1",
            "pid=x",
            "pid=4 pid=5",
            "quiet=-1",
            "quiet=1 quiet=2",
            "if-stalled if-stalled",
            "term",
        ] {
            assert!(
                parse(bad).is_err_and(|e| e.starts_with("ERR bad restart argument: ")),
                "{bad}"
            );
        }
    }

    /// [`another_hand`] returns `true` for another supervisor's live claim, a
    /// hold, a named driver's turn, or another holder's live drive lease, and
    /// `false` for no claim, this host's or a predecessor host's claim, an
    /// owner-class turn, a lapsed lease, or this host's own lease.
    #[test]
    fn another_hand_is_what_the_acts_kill_waits_out() {
        let now = crate::metrics::now_us();
        let fresh = |n: u64| crate::session_store::test_handle_on(7400 + n, -1).ctx;
        let claimed = |ctx: &SessionCtx, holder: &str| {
            assert_eq!(
                crate::session_timeline::claim_supervisor(ctx, holder, None, None, now),
                Ok(true),
                "{holder}"
            );
        };
        let leased = |ctx: &SessionCtx, lease: crate::Lease| {
            *ctx.turn_lease.lock().unwrap() = Some(lease);
        };
        let drive = |holder: &str, expires_us: u64| crate::Lease::Drive {
            holder: holder.to_string(),
            expires_us,
            conn: None,
            hard: true,
            here: false,
            holder_pid: None,
        };
        let driver = fresh(0).self_id.clone();
        let mut n = 1;
        let mut case = |set: &dyn Fn(&SessionCtx), want: bool, what: &str| {
            let ctx = fresh(n);
            n += 1;
            set(ctx.as_ref());
            assert_eq!(another_hand(&ctx, now), want, "{what}");
        };
        case(&|_| {}, false, "nothing on the session");
        case(
            &|ctx| claimed(ctx, &crate::harness_host::holder()),
            false,
            "this host's own claim",
        );
        case(
            &|ctx| claimed(ctx, "aterm-harness@1"),
            false,
            "a predecessor host's claim, carried",
        );
        case(
            &|ctx| claimed(ctx, "watcher@elsewhere"),
            true,
            "another supervisor's claim",
        );
        case(
            &|ctx| {
                assert!(crate::fabric::apply_hold_for_test(
                    ctx,
                    Some(crate::fabric::Hold {
                        reason: "-".to_string(),
                        origin: "fleet".to_string(),
                    }),
                ));
            },
            true,
            "a halt",
        );
        case(
            &|ctx| {
                leased(
                    ctx,
                    crate::Lease::Turn {
                        id: 1,
                        driver: Some(driver.clone()),
                        typing: false,
                    },
                );
            },
            true,
            "a named driver's turn",
        );
        case(
            &|ctx| {
                leased(
                    ctx,
                    crate::Lease::Turn {
                        id: 2,
                        driver: None,
                        typing: true,
                    },
                );
            },
            false,
            "an owner-class turn",
        );
        case(
            &|ctx| leased(ctx, drive("someone", now + 60_000_000)),
            true,
            "another driver's live lease",
        );
        case(
            &|ctx| leased(ctx, drive("someone", now.saturating_sub(1))),
            false,
            "a lapsed lease",
        );
        case(
            &|ctx| leased(ctx, drive(&crate::harness_host::holder(), now + 60_000_000)),
            false,
            "this host's own lease",
        );
    }

    /// The act is offered only where its relaunch would carry it: an agent whose
    /// records are in a directory no relaunch line can name is `not-relaunchable`,
    /// whereas the same snapshot under the user's own home or under another `HOME`'s
    /// `.claude` is offered the act.
    #[test]
    fn the_act_is_not_offered_for_an_agent_its_relaunch_refuses() {
        let now = crate::metrics::now_us();
        let other = aterm_tempfile::TempDir::new().expect("another home");
        let snap =
            |claude_dir: Option<std::path::PathBuf>| aterm_agent::harness::relaunch::Snapshot {
                tab: String::new(),
                pid: 0x3fff_ff01,
                start: "t".to_string(),
                shell: 1,
                program: std::path::PathBuf::from("/opt/claude"),
                argv: vec!["/opt/claude".to_string()],
                session: Some("0badf00d-1111-2222-3333-444455556666".to_string()),
                cwd: "/".to_string(),
                version: Some("2.1.300".to_string()),
                run: aterm_agent::harness::relaunch::AgentRun::Claude,
                dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
                login: None,
                stored_login: false,
                profile: None,
                home: None,
                claude_dir,
            };
        let refusal = |n: u64, claude_dir: Option<std::path::PathBuf>| {
            let ctx = crate::session_store::test_handle_on(7600 + n, -1).ctx;
            assert_eq!(
                crate::session_timeline::claim_supervisor(
                    &ctx,
                    &crate::harness_host::holder(),
                    None,
                    None,
                    now
                ),
                Ok(true),
                "the host's claim"
            );
            let snap = aterm_agent::harness::relaunch::Snapshot {
                tab: ctx.self_id.as_str().to_string(),
                ..snap(claude_dir)
            };
            crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(&snap));
            let why = unavailable(&ctx, 0x3fff_ff01, "t", now);
            crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), None);
            why
        };
        assert_eq!(
            refusal(1, Some(other.path().join("records"))),
            Some("not-relaunchable"),
            "its relaunch refuses it: the act is not offered"
        );
        assert_eq!(refusal(2, None), None, "CONTROL: the user's own home");
        assert_eq!(
            refusal(3, Some(other.path().join(".claude"))),
            None,
            "CONTROL: another HOME's .claude, which its line carries"
        );
    }

    /// An agy is offered the act when it has a witnessed conversation and a
    /// relaunchable launch line under the host's claim, and refused when no
    /// conversation is witnessed, `--remote-control` was passed, its `HOME` or
    /// zsh working directory cannot be represented in the relaunch line, or the
    /// agent is Muse Code.
    #[test]
    fn an_agy_is_offered_the_act_where_its_relaunch_resumes_it() {
        use aterm_agent::harness::relaunch::{AgentRun, AgyRun, MuseRun};
        let now = crate::metrics::now_us();
        // The tab's home: a scratch one, so the relaunch reads no hook of the
        // person's.
        let tab_home = aterm_tempfile::tempdir().expect("a scratch home");
        let conversation = "eaeec4fb-4e43-47da-b46f-8a10c0fee186";
        let refusal =
            |n: u64,
             session: Option<&str>,
             extra: &[&str],
             muse: bool,
             home: &str,
             (dialect, cwd): (aterm_agent::harness::upgrade::Dialect, &str)| {
                let ctx = crate::session_store::test_handle_on(7800 + n, -1).ctx;
                assert_eq!(
                    crate::session_timeline::claim_supervisor(
                        &ctx,
                        &crate::harness_host::holder(),
                        None,
                        None,
                        now
                    ),
                    Ok(true),
                    "the host's claim"
                );
                let mut argv = vec!["/opt/agy".to_string()];
                argv.extend(extra.iter().map(|w| (*w).to_string()));
                let run = if muse {
                    AgentRun::Muse(MuseRun {
                        data: "/h/.local/share".into(),
                        env: Vec::new(),
                        workspace: None,
                        lock: None,
                        started_ms: 0,
                        block: None,
                    })
                } else {
                    AgentRun::Agy(AgyRun {
                        config: "/h/.gemini/antigravity-cli".into(),
                        env: if home.is_empty() {
                            Vec::new()
                        } else {
                            vec![("HOME".to_string(), home.to_string())]
                        },
                        lock: None,
                        crash: None,
                        started_ms: 0,
                        block: None,
                    })
                };
                let snap = aterm_agent::harness::relaunch::Snapshot {
                    tab: ctx.self_id.as_str().to_string(),
                    pid: 0x3fff_ff21,
                    start: "t".to_string(),
                    shell: 1,
                    program: std::path::PathBuf::from("/opt/agy"),
                    argv,
                    session: session.map(str::to_string),
                    cwd: cwd.to_string(),
                    version: None,
                    run,
                    dialect: Some(dialect),
                    login: None,
                    stored_login: false,
                    profile: None,
                    home: None,
                    claude_dir: None,
                };
                crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(&snap));
                let why = unavailable_for(&ctx, 0x3fff_ff21, "t", now, Some(tab_home.path()));
                crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), None);
                why
            };
        use aterm_agent::harness::upgrade::Dialect;
        let zsh = (Dialect::Zsh, "/");
        assert_eq!(
            refusal(1, Some(conversation), &["--effort=high"], false, "", zsh),
            None,
            "a witnessed agy whose line carries its launch"
        );
        assert_eq!(
            refusal(2, None, &[], false, "", zsh),
            Some("not-relaunchable"),
            "no conversation proven"
        );
        assert_eq!(
            refusal(3, Some(conversation), &["--remote-control"], false, "", zsh),
            Some("not-relaunchable"),
            "its line refuses the launch"
        );
        assert_eq!(
            refusal(4, Some(conversation), &[], true, "", zsh),
            Some("not-relaunchable"),
            "a Muse Code: never yet"
        );
        assert_eq!(
            refusal(5, Some(conversation), &[], false, "/h/a\nb", zsh),
            Some("not-relaunchable"),
            "a HOME its relaunch line cannot hold"
        );
        assert_eq!(
            refusal(6, Some(conversation), &[], false, "/h", zsh),
            None,
            "CONTROL: another HOME its line carries"
        );
        assert_eq!(
            refusal(
                7,
                Some(conversation),
                &[],
                false,
                "/h",
                (Dialect::Fish, "/")
            ),
            None,
            "a fish shell"
        );
        for (n, dialect, want) in [
            (8, Dialect::Fish, None),
            (9, Dialect::Zsh, Some("not-relaunchable")),
        ] {
            assert_eq!(
                refusal(n, Some(conversation), &[], false, "", (dialect, "/w/a\nb")),
                want,
                "{dialect:?}: a directory no `cd` names"
            );
        }
    }

    /// A Codex is restarted only when a validated self-claim copy names that
    /// exact process; without one (or with a copy for another pid) the act is
    /// `not-relaunchable`.
    #[test]
    fn a_codex_with_no_validated_copy_is_not_offered_the_act() {
        let now = crate::metrics::now_us();
        let home = aterm_primer::home_dir().expect("a home");
        let pid = 0x3fff_ff11;
        let refusal = |n: u64, copy: Option<u32>| {
            let ctx = crate::session_store::test_handle_on(7700 + n, -1).ctx;
            let sid = ctx.self_id.as_str().to_string();
            let snap = aterm_agent::harness::relaunch::Snapshot {
                tab: sid.clone(),
                pid,
                start: "t".to_string(),
                shell: 1,
                program: std::path::PathBuf::from("/opt/codex/bin/codex"),
                argv: vec!["/opt/codex/bin/codex".to_string()],
                session: Some("0badf00d-1111-2222-3333-444455556666".to_string()),
                cwd: "/".to_string(),
                version: None,
                run: aterm_agent::harness::relaunch::AgentRun::Codex(
                    aterm_agent::harness::relaunch::CodexRun {
                        home: home.join(".codex"),
                        home_env: None,
                        self_claimed: None,
                    },
                ),
                dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
                login: None,
                stored_login: false,
                profile: None,
                home: None,
                claude_dir: None,
            };
            assert!(
                !aterm_agent::harness::relaunch::carries_restart(&home, &snap),
                "no line rides the snapshot: the relaunch's own predicate carries nothing"
            );
            let copy = copy.map(|of| aterm_agent::harness::self_claimed::Copy {
                pid: of,
                start: "t".to_string(),
                home: home.join(".codex"),
                seen: aterm_agent::harness::self_claimed::Seen::Removed,
                resume: Ok(aterm_agent::harness::self_claimed::Resume {
                    argv: vec!["/opt/codex/bin/codex".to_string()],
                    session: "0badf00d-1111-2222-3333-444455556666".to_string(),
                    codex_home: None,
                    cwd: None,
                }),
            });
            crate::self_claim::set_copy_for_test(&sid, copy);
            crate::harness_host::publish_snapshot_for_test(&sid, Some(&snap));
            let why = unavailable(&ctx, i32::try_from(pid).unwrap(), "t", now);
            crate::harness_host::publish_snapshot_for_test(&sid, None);
            crate::self_claim::set_copy_for_test(&sid, None);
            why
        };
        assert_eq!(refusal(1, None), Some("not-relaunchable"), "no copy");
        assert_eq!(
            refusal(2, Some(pid + 1)),
            Some("not-relaunchable"),
            "a copy of another process"
        );
        assert_ne!(
            refusal(3, Some(pid)),
            Some("not-relaunchable"),
            "CONTROL: the copy of that very process validated its line"
        );
    }

    /// Tab key for the watch registry tests.
    const TAB: &str = "s-watch";

    fn stamped(state: &aterm_tempfile::TempDir, stamp: &Stamp) {
        restart_stamp::write(state.path(), TAB, stamp).expect("the stamp");
    }

    /// A watch that lets go is unlisted in the same [`Watch::left`] check, while
    /// a joining [`watch`] call before the check raises its deadline and keeps it
    /// active.
    #[test]
    fn a_watch_that_lets_go_is_unlisted_in_that_look_and_a_raise_before_it_keeps_it() {
        let pid = 0x3fff_fff1_i32;
        let state = aterm_tempfile::TempDir::new().expect("a state dir");
        let now = aterm_time::unix_secs().expect("clock");
        let stamp = |at: u64| Stamp {
            pid: pid.unsigned_abs(),
            start: "t".to_string(),
            cause: Cause::Asked,
            at,
        };
        let short = stamp(now + 10 - aterm_agent::harness::relaunch::STALE_S);
        stamped(&state, &short);
        let w = watch(state.path(), pid, &short).expect("the first watch");
        assert!(w.lapsed(), "10 s left: inside the read margin");
        // A join writes its stamp first, then finds the watch running.
        stamped(&state, &stamp(now));
        assert!(
            watch(state.path(), pid, &stamp(now)).is_none(),
            "a join finds it running"
        );
        assert!(!w.lapsed(), "its KILL may go: by the join's stamp");
        assert!(w.left(TAB).is_ok(), "raised before its look: it waits on");
        assert_eq!(watches(pid), 1);
        drop(w);
        assert_eq!(watches(pid), 0, "unlisted as its thread drops it");
        stamped(&state, &short);
        let w = watch(state.path(), pid, &short).expect("a watch again");
        assert_eq!(w.left(TAB), Err(LetGo::Lapsed), "not raised: it lets go");
        assert_eq!(
            watches(pid),
            0,
            "unlisted in that same look, the watch itself still held"
        );
        assert!(w.lapsed(), "let go: no KILL");
        stamped(&state, &stamp(now));
        let join =
            watch(state.path(), pid, &stamp(now)).expect("a join after the let-go gets its own");
        assert_eq!(watches(pid), 1);
        drop(w);
        assert_eq!(watches(pid), 1, "the let-go watch's end leaves the join's");
        drop(join);
        assert_eq!(watches(pid), 0);
    }

    /// A watch whose tab's on-disk stamp is replaced by another process's stamp
    /// or taken lets go and unlists itself on the next [`Watch::left`] check.
    #[test]
    fn a_watch_whose_tabs_stamp_names_another_lets_go_in_that_look() {
        let pid = 0x3fff_fff3_i32;
        let state = aterm_tempfile::TempDir::new().expect("a state dir");
        let now = aterm_time::unix_secs().expect("clock");
        let ours = Stamp {
            pid: pid.unsigned_abs(),
            start: "t".to_string(),
            cause: Cause::Stall,
            at: now,
        };
        stamped(&state, &ours);
        let w = watch(state.path(), pid, &ours).expect("a watch");
        assert!(w.left(TAB).is_ok(), "its stamp names it: it waits on");
        stamped(
            &state,
            &Stamp {
                pid: pid.unsigned_abs() + 1,
                ..ours.clone()
            },
        );
        assert_eq!(w.left(TAB), Err(LetGo::Unnamed), "another act's stamp");
        assert_eq!(watches(pid), 0, "unlisted in that same look");
        stamped(&state, &ours);
        let join = watch(state.path(), pid, &ours).expect("a join gets its own");
        assert!(join.left(TAB).is_ok());
        let _ = restart_stamp::take_if(state.path(), TAB, |_| true);
        assert_eq!(join.left(TAB), Err(LetGo::Unnamed), "its stamp taken");
        assert_eq!(watches(pid), 0);
        drop((w, join));
    }

    /// A watch remains listed and active when a concurrent [`restart_stamp::take_if`]
    /// for another process inspects and rejects the tab's stamp in place.
    #[test]
    fn a_watch_waits_on_while_another_take_judges_its_tabs_stamp() {
        let pid = 0x3fff_fff4_i32;
        let state = aterm_tempfile::TempDir::new().expect("a state dir");
        let ours = Stamp {
            pid: pid.unsigned_abs(),
            start: "t".to_string(),
            cause: Cause::Stall,
            at: aterm_time::unix_secs().expect("clock"),
        };
        stamped(&state, &ours);
        let w = watch(state.path(), pid, &ours).expect("a watch");
        let mut looked = None;
        let taken = restart_stamp::take_if(state.path(), TAB, |s| {
            looked = Some(w.left(TAB));
            s.names(pid.unsigned_abs() + 1, "t")
        });
        assert_eq!(taken, None, "not that snapshot's stamp");
        assert!(
            looked.is_some_and(|left| left.is_ok()),
            "a look while it was judged: {looked:?}"
        );
        assert_eq!(watches(pid), 1, "still listed");
        assert!(w.left(TAB).is_ok(), "it waits on");
        assert_eq!(restart_stamp::read(state.path(), TAB), Some(ours));
        drop(w);
        assert_eq!(watches(pid), 0);
    }

    /// A corrupt stamp dated at `u64::MAX` saturates to an immediate deadline
    /// rather than panicking on `Instant` addition and lets go on its first look.
    #[test]
    fn a_stamp_dated_past_any_instant_gives_its_watch_no_time() {
        let pid = 0x3fff_fff2_i32;
        let state = aterm_tempfile::TempDir::new().expect("a state dir");
        let w = watch(
            state.path(),
            pid,
            &Stamp {
                pid: pid.unsigned_abs(),
                start: "t".to_string(),
                cause: Cause::Asked,
                at: u64::MAX,
            },
        )
        .expect("a watch");
        assert!(w.lapsed(), "no KILL");
        assert_eq!(w.left(TAB), Err(LetGo::Lapsed), "let go at its first look");
        assert_eq!(watches(pid), 0);
    }
}
