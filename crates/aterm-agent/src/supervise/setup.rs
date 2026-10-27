// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Initial configuration ([`SessionSetup`]) passed to a [`Session`] before its loop runs.

use super::*;
use crate::supervise::occupancy::{due, rules};
use crate::supervise::tape::header::{Reader, Roots, VERBATIM, Writer};

/// The hosted loop's hand-over flag ([`Session::set_handover`]), as a setup holds it.
#[derive(Debug, Clone, Default)]
pub(super) struct Handover(pub(super) Option<Arc<AtomicBool>>);

impl Handover {
    /// The host raised it: a stop that hands the session to a new loop.
    pub(super) fn raised(&self) -> bool {
        self.0.as_ref().is_some_and(|h| h.load(Ordering::SeqCst))
    }
}

impl PartialEq for Handover {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

impl Eq for Handover {}

const WORKER_NOT_TAPED: &str = "the worker's environment is never written to a tape";

fn worker_source(text: &str) -> Result<WorkerSource, String> {
    let bad = || format!("atape header: [env] worker: no source {text:?}");
    match text.split_once(' ') {
        None if text == "session" => Ok(WorkerSource::Session),
        None if text == "fixed" || text == "fixed-err" => {
            Ok(WorkerSource::Fixed(Err(WORKER_NOT_TAPED.to_string())))
        }
        Some(("fixed-err", why)) => Ok(WorkerSource::Fixed(Err(why.to_string()))),
        Some(("fixed", rest)) => {
            let (digest, pid) = rest.split_once(' ').ok_or_else(bad)?;
            let digest = u64::from_str_radix(digest, 16).map_err(|_| bad())?;
            let pid = match pid {
                "-" => None,
                p => Some(p.parse::<u32>().map_err(|_| bad())?),
            };
            Ok(WorkerSource::Fixed(Ok(
                crate::supervise::policy::WorkerEnv::recorded(digest, pid),
            )))
        }
        _ => Err(bad()),
    }
}

/// Initial configuration and timing parameters for a [`Session`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSetup {
    /// The target selector (`@sid`); `None` drives the caller's own session.
    pub(super) sid: Option<String>,
    /// The name the loop claims the session under (`meta set supervisor <holder>`): [`Session::set_supervisor_name`]'s, else `aterm-supervise:<pid>` ([`claim::default_holder`]) — resolved when the setup is made, so a replay names the recorded holder and never reads its own pid.
    pub(super) holder: String,
    /// A PROVEN SELF-CLAIM's holder this loop watches beside ([`Session::set_beside`], [`claim::Claim::Beside`]): it claims nothing and types nothing, and keeps the stall's remedy.
    pub(super) beside: Option<String>,
    /// Set by the hosted loop's owner, BEFORE its stop, when the stop hands the session to a loop it starts right after (a changed policy): the badges this loop raised are left standing for that one to adopt ([`Session::reconcile`]), never cleared and raised again.
    pub(super) handover: Handover,
    /// The loop types its texts under this process's hand ([`Session::type_under_hand`]): the window's host's loop.
    pub(super) types_under_hand: bool,
    /// Where the approval ledger goes ([`Session::set_approval_ledger`]); `None` keeps none — until a hosted loop takes its options' own ([`Session::run_hosted`]).
    pub(super) ledger_path: Option<PathBuf>,
    /// What the approval policy takes from the supervisor's own process (the owner's home, uid and `$TMPDIR`; fixed in tests).
    pub(super) approval_env: ApprovalEnv,
    /// How long an outage is ridden out before the loop ends ([`Session::ride_out`]); zero ends the loop at its first unserved request.
    pub(super) reconnect: Duration,
    /// The first pause between tries to reach the session again, and the longest (each doubles).
    pub(super) pause: Duration,
    pub(super) pause_max: Duration,
    /// The claim's renewal step and lease ([`claim::CLAIM_RENEW`], [`claim::ttl_ms`] of the build's [`crate::harness::Timings`]; tests shorten them, [`Session::set_timings`] sets the lease).
    pub(super) claim_renew: Duration,
    pub(super) claim_ttl_ms: u64,
    /// How long after a fenced write's skip the point is decided again ([`crate::harness::Timings::redecide_after_skip`], the build's unless [`Session::set_timings`] gives its own).
    pub(super) redecide_after_skip: Duration,
    /// The turn-end policy's timings ([`Session::set_turn_end_timing`]), which its memory of the session starts from.
    pub(super) turn_end_timing: TurnEndTiming,
    /// How long presses that miss are tried before the session is badged ([`approval_loop::PRESS_BADGE_AFTER`]; a test shortens it).
    pub(super) press_badge_after: Duration,
    /// The survey's retry pauses ([`SURVEY_RETRY`]; a test shortens them).
    pub(super) survey_retry: [Duration; SURVEY_TRIES],
    /// How long a rating card must have shown before its `0` goes ([`SURVEY_SETTLE`]).
    pub(super) survey_settle: Duration,
    /// How long the harness's `0` is given to be taken ([`SURVEY_LAND`]).
    pub(super) survey_land: Duration,
    /// The accounts probe's bounds ([`PROBE_WAITS`]; a test shortens them).
    pub(super) probe_waits: ProbeWaits,
    /// The limit reset step's bounds ([`reset_loop::RESET_WAITS`]; a test shortens them).
    pub(super) reset_waits: reset_loop::ResetWaits,
    /// [`BOX_SETTLE`] (`Session::set_box_settle`).
    pub(super) box_settle: Duration,
    /// [`AUTOCOMPACT_SETTLE`] (`Session::set_autocompact_settle`): the box settle of Claude Code's autocompact offer and auto-mode setup offer, which drop a key pressed within 500 ms of their mount or of the key before, at least [`Self::box_settle`].
    pub(super) autocompact_settle: Duration,
    /// [`background_settle`] (`Session::set_background_settle`): read from the live upgrade's settle once, here.
    pub(super) background_settle: Duration,
    /// The step of the waits a host's request is read between ([`next_wait_step`]: [`WAIT_STEP`], never longer than the live upgrade's settle as this process runs it): read once, here, as [`Self::background_settle`] is.
    pub(super) wait_step: Duration,
    /// How long apart the agent's transcript is read again for a box's call ([`crate::supervise::policy::box_call::LAG_STEP`]; `Session::set_box_call_step`, a test shortens it).
    pub(super) box_call_step: Duration,
    /// How long a screen must read busy on background work alone before the stuck-busy canary asks the server what the agent's own title says ([`wall_memory::STUCK_AFTER`]; a test shortens it).
    pub(super) stuck_after: Duration,
    /// How long a hold for a person waits before the point is looked at again, in place of the rest of their grace; `None` (every run but a test's, `Session::set_person_hold_wait`): the rest of their grace.
    pub(super) person_hold_wait: Option<Duration>,
    /// The memory remedy's observation window beside a proven self-claim: [`WAIT_STEP`], but in tests.
    pub(super) beside_idle_min: Duration,
    /// How old a hold the occupancy door's `wait-nudge` holds from ([`rules::wait_nudge_after_ms`]: the shipped 30 min, or the lab's seam's seconds in a build that reads seams): read once, here, so a tape replays under the wait it was recorded under.
    pub(super) occupancy_nudge_after: Duration,
    /// How many looks a due line of the door's stands untold before the loop says it ready-idle ([`crate::supervise::occupancy::due::K_LOOKS`]; a test shortens it): read once, here, so a tape replays under the K it was recorded under, and one recorded before the K clock under [`crate::supervise::occupancy::due::K_NEVER`].
    pub(super) occupancy_k_looks: i64,
}

impl SessionSetup {
    /// Creates the default setup for `sid` using process environment and build timings.
    #[must_use]
    pub fn new(sid: Option<String>) -> Self {
        Self {
            sid,
            holder: claim::default_holder(),
            beside: None,
            handover: Handover::default(),
            types_under_hand: false,
            ledger_path: None,
            approval_env: ApprovalEnv::of_process(),
            reconnect: DEFAULT_RECONNECT,
            pause: RECONNECT_PAUSE,
            pause_max: RECONNECT_PAUSE_MAX,
            claim_renew: claim::CLAIM_RENEW,
            claim_ttl_ms: claim::ttl_ms(&crate::harness::Timings::for_build()),
            redecide_after_skip: crate::harness::Timings::for_build().redecide_after_skip,
            turn_end_timing: TurnEndTiming::for_build(),
            press_badge_after: approval_loop::PRESS_BADGE_AFTER,
            survey_retry: SURVEY_RETRY,
            survey_settle: SURVEY_SETTLE,
            survey_land: SURVEY_LAND,
            probe_waits: PROBE_WAITS,
            reset_waits: reset_loop::RESET_WAITS,
            box_settle: BOX_SETTLE,
            autocompact_settle: AUTOCOMPACT_SETTLE,
            background_settle: background_settle(),
            wait_step: next_wait_step(),
            box_call_step: crate::supervise::policy::box_call::LAG_STEP,
            stuck_after: wall_memory::STUCK_AFTER,
            person_hold_wait: None,
            beside_idle_min: WAIT_STEP,
            occupancy_nudge_after: Duration::from_millis(
                rules::wait_nudge_after_ms().unsigned_abs(),
            ),
            occupancy_k_looks: due::K_LOOKS,
        }
    }

    /// The target selector, if any.
    #[must_use]
    pub fn sid(&self) -> Option<&str> {
        self.sid.as_deref()
    }

    /// The name the loop's claim is taken under, resolved when the setup was made.
    #[must_use]
    pub fn holder(&self) -> &str {
        &self.holder
    }

    /// The turn-end policy's timings the session starts from (a tape's, as its header says).
    #[must_use]
    pub const fn turn_end_timing(&self) -> &TurnEndTiming {
        &self.turn_end_timing
    }

    /// The approval ledger, if one was given.
    #[must_use]
    pub fn ledger_path(&self) -> Option<&Path> {
        self.ledger_path.as_deref()
    }

    /// The approval policy's process inputs.
    #[must_use]
    pub const fn approval_env(&self) -> &ApprovalEnv {
        &self.approval_env
    }

    /// The same setup with its ledger at `ledger_path`: a tape's header holds the path the loop
    /// opens ([`Header::capture`](crate::supervise::tape::header::Header::capture)).
    #[must_use]
    pub(crate) fn with_ledger_path(&self, ledger_path: Option<PathBuf>) -> Self {
        Self {
            ledger_path,
            ..self.clone()
        }
    }

    /// The setup as a header's `[setup]` and `[env]` sections, every path the loop opens under `roots`.
    pub(crate) fn write(&self, w: &mut Writer, roots: &Roots, seed: u64) {
        let Self {
            sid,
            holder,
            beside,
            handover,
            types_under_hand,
            ledger_path,
            approval_env,
            reconnect,
            pause,
            pause_max,
            claim_renew,
            claim_ttl_ms,
            redecide_after_skip,
            turn_end_timing,
            press_badge_after,
            survey_retry,
            survey_settle,
            survey_land,
            probe_waits,
            reset_waits,
            box_settle,
            autocompact_settle,
            background_settle,
            wait_step,
            box_call_step,
            stuck_after,
            person_hold_wait,
            beside_idle_min,
            occupancy_nudge_after,
            occupancy_k_looks,
        } = self;
        w.section("setup");
        w.opt_text("sid", sid.as_deref());
        w.text("holder", holder);
        w.opt_text("beside", beside.as_deref());
        w.shown("handover", handover.0.is_some());
        w.shown("types_under_hand", types_under_hand);
        w.opt_path("ledger", ledger_path.as_deref(), roots);
        w.duration("reconnect", *reconnect);
        w.duration("pause", *pause);
        w.duration("pause_max", *pause_max);
        w.duration("claim_renew", *claim_renew);
        w.shown("claim_ttl_ms", claim_ttl_ms);
        w.duration("redecide_after_skip", *redecide_after_skip);
        w.duration("press_badge_after", *press_badge_after);
        w.durations("survey_retry", survey_retry);
        w.duration("survey_settle", *survey_settle);
        w.duration("survey_land", *survey_land);
        w.duration("box_settle", *box_settle);
        w.duration("autocompact_settle", *autocompact_settle);
        w.duration("background_settle", *background_settle);
        w.duration("wait_step", *wait_step);
        w.duration("box_call_step", *box_call_step);
        w.duration("stuck_after", *stuck_after);
        let ProbeWaits {
            open,
            settle,
            step,
            close,
        } = probe_waits;
        w.duration("probe.open", *open);
        w.duration("probe.settle", *settle);
        w.duration("probe.step", *step);
        w.duration("probe.close", *close);
        let reset_loop::ResetWaits {
            open,
            check,
            claim,
            line,
            again,
            enter,
            enter_by,
            step,
            close,
        } = reset_waits;
        w.duration("reset.open", *open);
        w.duration("reset.check", *check);
        w.duration("reset.claim", *claim);
        w.duration("reset.line", *line);
        w.duration("reset.again", *again);
        w.duration("reset.enter", *enter);
        w.duration("reset.enter_by", *enter_by);
        w.duration("reset.step", *step);
        w.duration("reset.close", *close);
        let TurnEndTiming {
            min_work,
            short_backoff,
            short_backoff_max,
            retry_backoff,
            net_backoff,
            outage_first,
            outage_cap,
            progress,
            reset_grace,
            reset_jitter,
            auto_resume_grace,
            limit_backoff,
            entitlement_backoff,
            refused_backoff,
            window,
            take_within,
            restart_backoff,
            credits_bound,
        } = turn_end_timing;
        w.duration("turn_end.min_work", *min_work);
        w.duration("turn_end.short_backoff", *short_backoff);
        w.duration("turn_end.short_backoff_max", *short_backoff_max);
        w.durations("turn_end.retry_backoff", retry_backoff);
        w.durations("turn_end.net_backoff", net_backoff);
        w.duration("turn_end.progress", *progress);
        w.duration("turn_end.reset_grace", *reset_grace);
        w.duration("turn_end.reset_jitter", *reset_jitter);
        w.duration("turn_end.auto_resume_grace", *auto_resume_grace);
        w.durations("turn_end.limit_backoff", limit_backoff);
        w.durations("turn_end.entitlement_backoff", entitlement_backoff);
        w.durations("turn_end.refused_backoff", refused_backoff);
        w.duration("turn_end.window", *window);
        w.duration("turn_end.take_within", *take_within);
        w.durations("turn_end.restart_backoff", restart_backoff);
        w.duration("turn_end.credits_bound", *credits_bound);
        w.duration("turn_end.outage_first", *outage_first);
        w.duration("turn_end.outage_cap", *outage_cap);
        w.opt_duration("person_hold_wait", *person_hold_wait);
        w.duration("beside_idle_min", *beside_idle_min);
        w.duration("occupancy.nudge_after", *occupancy_nudge_after);
        w.shown("occupancy.k_looks", occupancy_k_looks);
        let ApprovalEnv {
            home,
            uid,
            tmpdir,
            worker,
        } = approval_env;
        w.section("env");
        w.opt_path("home", home.as_deref(), &VERBATIM);
        w.shown("uid", uid);
        w.opt_path("tmpdir", tmpdir.as_deref(), &VERBATIM);
        w.text(
            "worker",
            &match worker {
                WorkerSource::Session => "session".to_string(),
                WorkerSource::Fixed(Ok(env)) => format!(
                    "fixed {:016x} {}",
                    env.digest(seed),
                    env.pid().map_or_else(|| "-".to_string(), |p| p.to_string())
                ),
                WorkerSource::Fixed(Err(why)) => format!("fixed-err {why}"),
            },
        );
    }

    /// [`Self::write`]'s sections, read back: a hand-over flag that was given comes back as a fresh one, not raised, and a stand-in worker environment as one that says it was never written ([`WORKER_NOT_TAPED`]).
    pub(crate) fn read(r: &mut Reader, roots: &Roots) -> Result<Self, String> {
        r.section("setup");
        r.retired_duration("switch_break_note")?;
        r.retired_duration("turn_end.unknown_hold")?;
        r.retired_duration("turn_end.limits_every")?;
        r.retired_duration("turn_end.restore_pick")?;
        r.retired_text("manager")?;
        r.retired_duration("mail_step")?;
        r.retired_duration("hold_step")?;
        let sid = r.opt_text("sid")?;
        let holder = r.text("holder")?;
        let beside = r.opt_text("beside")?;
        let handover = Handover(
            r.parsed::<bool>("handover")?
                .then(|| Arc::new(AtomicBool::new(false))),
        );
        let types_under_hand = r.or_shipped("types_under_hand", false, Reader::parsed::<bool>)?;
        let ledger_path = r.opt_io_path("ledger", roots)?;
        let reconnect = r.duration("reconnect")?;
        let pause = r.duration("pause")?;
        let pause_max = r.duration("pause_max")?;
        let claim_renew = r.duration("claim_renew")?;
        let claim_ttl_ms = r.parsed("claim_ttl_ms")?;
        let redecide_after_skip = r.duration("redecide_after_skip")?;
        let press_badge_after = r.duration("press_badge_after")?;
        let survey_retry: [Duration; SURVEY_TRIES] =
            r.durations("survey_retry")?.try_into().map_err(|_| {
                format!("atape header: [setup] survey_retry is not {SURVEY_TRIES} pauses")
            })?;
        let survey_settle = r.duration("survey_settle")?;
        let survey_land = r.duration("survey_land")?;
        let box_settle = r.duration("box_settle")?;
        let autocompact_settle = r.duration("autocompact_settle")?;
        let background_settle = r.duration("background_settle")?;
        let wait_step = r.or_shipped(
            "wait_step",
            WAIT_STEP.min(background_settle),
            Reader::duration,
        )?;
        let box_call_step = r.or_shipped(
            "box_call_step",
            crate::supervise::policy::box_call::LAG_STEP,
            Reader::duration,
        )?;
        let stuck_after = r.duration("stuck_after")?;
        let probe_waits = ProbeWaits {
            open: r.duration("probe.open")?,
            settle: r.duration("probe.settle")?,
            step: r.duration("probe.step")?,
            close: r.duration("probe.close")?,
        };
        let reset_waits = reset_loop::ResetWaits {
            open: r.duration("reset.open")?,
            check: r.duration("reset.check")?,
            claim: r.duration("reset.claim")?,
            line: r.duration("reset.line")?,
            again: r.duration("reset.again")?,
            enter: r.duration("reset.enter")?,
            enter_by: r.duration("reset.enter_by")?,
            step: r.duration("reset.step")?,
            close: r.duration("reset.close")?,
        };
        let turn_end_timing = TurnEndTiming {
            min_work: r.duration("turn_end.min_work")?,
            short_backoff: r.duration("turn_end.short_backoff")?,
            short_backoff_max: r.duration("turn_end.short_backoff_max")?,
            retry_backoff: r.durations("turn_end.retry_backoff")?,
            net_backoff: r.durations("turn_end.net_backoff")?,
            progress: r.duration("turn_end.progress")?,
            reset_grace: r.duration("turn_end.reset_grace")?,
            reset_jitter: r.duration("turn_end.reset_jitter")?,
            auto_resume_grace: r.duration("turn_end.auto_resume_grace")?,
            limit_backoff: r.durations("turn_end.limit_backoff")?,
            entitlement_backoff: r.durations("turn_end.entitlement_backoff")?,
            refused_backoff: r.or_shipped(
                "turn_end.refused_backoff",
                TurnEndTiming::default().refused_backoff,
                Reader::durations,
            )?,
            window: r.duration("turn_end.window")?,
            take_within: r.duration("turn_end.take_within")?,
            restart_backoff: r.durations("turn_end.restart_backoff")?,
            credits_bound: r.or_shipped(
                "turn_end.credits_bound",
                TurnEndTiming::default().credits_bound,
                Reader::duration,
            )?,
            outage_first: r.or_shipped(
                "turn_end.outage_first",
                TurnEndTiming::default().outage_first,
                Reader::duration,
            )?,
            outage_cap: r.or_shipped(
                "turn_end.outage_cap",
                TurnEndTiming::default().outage_cap,
                Reader::duration,
            )?,
        };
        r.retired_duration("turn_end.down_hold")?;
        let person_hold_wait = r.or_shipped("person_hold_wait", None, Reader::opt_duration)?;
        let beside_idle_min = r.or_shipped("beside_idle_min", WAIT_STEP, Reader::duration)?;
        let occupancy_nudge_after = r.or_shipped(
            "occupancy.nudge_after",
            Duration::from_millis(rules::WAIT_NUDGE_AFTER_MS.unsigned_abs()),
            Reader::duration,
        )?;
        // A tape without the row was recorded before the loop had a K clock: none of its lines
        // was ever said ready-idle, so it replays under a K no line reaches.
        let occupancy_k_looks =
            r.or_shipped("occupancy.k_looks", due::K_NEVER, Reader::parsed::<i64>)?;
        r.section("env");
        let approval_env = ApprovalEnv {
            home: r.opt_path("home", &VERBATIM)?,
            uid: r.parsed("uid")?,
            tmpdir: r.opt_path("tmpdir", &VERBATIM)?,
            worker: worker_source(&r.text("worker")?)?,
        };
        Ok(Self {
            sid,
            holder,
            beside,
            handover,
            types_under_hand,
            ledger_path,
            approval_env,
            reconnect,
            pause,
            pause_max,
            claim_renew,
            claim_ttl_ms,
            redecide_after_skip,
            turn_end_timing,
            press_badge_after,
            survey_retry,
            survey_settle,
            survey_land,
            probe_waits,
            reset_waits,
            box_settle,
            autocompact_settle,
            background_settle,
            wait_step,
            box_call_step,
            stuck_after,
            person_hold_wait,
            beside_idle_min,
            occupancy_nudge_after,
            occupancy_k_looks,
        })
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
