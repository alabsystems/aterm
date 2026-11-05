// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab forever --mode sysmsg`: A SYSTEM MESSAGE IS A WAIT (v6 §14.17.5, slice 12a) on every
//! harness, one chain per system-message form, each alone on a lab of its own (the stub's stand-in
//! meets every conversation it serves).
//!
//! Slice 12b's outage labs (`--mode outage-land`, `--mode outage-cruise`, [`super::outage`]) stand
//! Claude Code's overload until three tries met it. These stand each form the stub can play on
//! Claude Code, agy and Muse Code ([`Form`]) past the outage's first rung and past Cruise's
//! short-turn back-off, so a harness that reads the wall as a plain point types into it, and one
//! that waits types only its try on the rung:
//!
//! * At the chain's first point the lab raises the stub's stand-in of the form
//!   ([`Form::stub_mode`]), so Cruise's own act meets it, and gives the stub its scripted mode
//!   back [`STANDS_MS`] past the wall's first reading ([`wall_read`], [`lift_due`]): the clearing.
//!   Each agent's own retries are cut short ([`vendor_retries`]), so its turn ends on the wall
//!   soon; no clearing is due before the program's turn ended on it.
//! * [`judge`] wants: nothing typed while the wall stands but the outage's tries, each on its
//!   rung of the ladder with a wait journaled before it (a condition that names no clearing is
//!   tried blind, v6 §14.17.5.2); no escalation, need or hold in it (`outage::stood_faults`);
//!   `aterm ctl status` saying `waiting=<the outage's word>` ([`Form::outage`]) while it stands;
//!   past the clearing exactly one resume, in a cleared line ([`turn_end::is_cleared_line`]),
//!   before Cruise's own act; past the resume a short streak no longer than the one the act that
//!   met the wall was typed on ([`streak_kept`], v6 §14.17.5.7); and the harness's glass waking
//!   no more than its tries allow ([`wake_faults`]).
//! * [`state_line`], a window want, wants the tab's Settings ▸ Harness row to lead its state line
//!   with `waiting: <the outage's words>` while the wall stood, with no need on it.
//! * The verdict says the instance's wakes while the wall stood ([`WAKE_FIELDS`]), the wait's wake
//!   budget.
//!
//! Slice 12c (v6 §14.17.5.4) adds what clears an outage ahead of a blind try:
//!
//! * The rate limit's stand-in names its `retry-after` (`rate-limit:150`, [`RATE_AFTER_MS`]): in
//!   the words of the Responses API's body and of Gemini's per-minute quota's `RetryInfo`, where
//!   [`judge`] wants agy's and Muse Code's first try no sooner than that, not on the ladder's
//!   first rung ([`Form::waits_named`]); in the Anthropic API's `retry-after` header alone, its
//!   body naming no number as the real API's does not, so Claude Code's harness, which never sees
//!   the header, tries on the ladder. Each try inside the standing wall is the outage's blind
//!   line, on its rung and no later than [`super::outage::late_ms`] past it, and a wall that
//!   stood past the first rung saw one.
//! * The witness lab (`WIT`, [`Form::Witness`]) runs two tabs of one program on the stub's one
//!   route: the first meets the overload as `OVL` does; the second is prompted into the standing
//!   wall [`MATE_AFTER_MS`] past the first's ([`mate_step`]), so the first's try past the
//!   clearing answers first, and [`judge_witnessed`] wants the second's first act past the
//!   clearing to be the outage's measured line (*has cleared*) on that witness, journaled, with no
//!   blind try past the clearing.

use aterm_agent::harness::witness::WITNESS_MARK;
use aterm_agent::supervise::Outage;
use aterm_agent::supervise::approvals::RULE_POINT;
use aterm_agent::supervise::policy::turn_end::{self, RULE_OUTAGE_RESUME};

use super::outage::{acts_after, first_read, late_ms, line, rung_ms, stood_faults, wait_after};
use super::surfaces::held::stall_of;
use super::surfaces::{Ctx, observed, page_field};
use super::{
    Chain, DUE_SLACK_MS, Harness, Seen, Tab, Verdict, at, backoff_named, backoff_streak,
    first_point_unanswered, point_streak, tab_turn, word,
};
use crate::Fail;
use crate::door::Door;
use crate::run::Run;
use crate::util::one_line;

/// How long the stand-in stands past the wall's first reading ([`lift_due`]): past the outage's
/// first rung (2 min, `TurnEndTiming::outage_first`, whose try meets the standing wall) and past
/// Cruise's short-turn back-off (4 min at the most), so a harness that reads the wall as a plain
/// point types Cruise's act into it; short of the second rung (4 min from the first try's own
/// wall), so a harness that waits types its one try on the rung and one resume past the clearing.
pub const STANDS_MS: i64 = 300_000;

/// The chain's hang detector: Cruise's first back-off, the wall and the agent's own tries, the
/// stand, the second rung past it, the resume's turn and Cruise going on, with room.
pub const LIMIT_MS: i64 = 22 * 60 * 1000;

/// The `metrics` fields the chain reports for the time its stand-in stood ([`wakes`]): the wait's
/// wake budget, bounded by its ladder.
pub const WAKE_FIELDS: [&str; 4] = [
    "timer_wakes",
    "deadline_arms_by_owner",
    "deadline_fires_by_owner",
    "wakes_by_variant",
];

/// The `retry-after` the rate limit's stand-in names ([`Form::stub_mode`]'s `rate-limit:150`; in
/// its words where [`Form::waits_named`]): past the ladder's first rung (2 min), short of the
/// stand, so a harness that tries on the ladder where the words name it tries before it.
pub const RATE_AFTER_MS: i64 = 150_000;

/// How long past the witness lab's first tab's wall the second tab is prompted into the standing
/// wall ([`mate_step`]): each of the second's rungs then comes this long after the first's, so
/// the first's try past the clearing answers its route first, and the second's own next rung is
/// still this long off when its wait reads that witness (at most one wait step, 20 s, later).
pub const MATE_AFTER_MS: i64 = 90_000;

/// How long past Cruise going on after the resume the chain is watched: a second resume would
/// come at once.
const WATCH_PAST_MS: i64 = 20_000;

/// How long past the resume with Cruise not gone on the chain is watched: the resume's turn ends
/// in seconds on the stub.
const RESUME_TURN_MS: i64 = 120_000;

/// How long past the resume Land has to land (v6 §14.17.5.6): the resume's answer, the
/// land check its clean defense gets and that check's answer, each seconds on the stub, past the
/// take window Land reads an answer in; the land lab's own bound, with room.
pub const LAND_ON_MS: i64 = 240_000;

/// The [`WAKE_FIELDS`] of a `metrics` reply, `key=value` each, `key=?` for one it lacks.
#[must_use]
pub fn wakes(metrics: &str) -> String {
    WAKE_FIELDS
        .iter()
        .map(|k| {
            format!(
                "{k}={}",
                word(metrics, k).unwrap_or_else(|| "?".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A system-message form the stub stands in for, and its chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// The API overloaded: Claude Code's 529, agy's 503 `UNAVAILABLE`, the Responses API's 503
    /// `server_is_overloaded`.
    Overload,
    /// The API's server error: a 500 on every dialect.
    Outage,
    /// A request rate limit: a 429 with a `retry-after` that is no usage limit.
    Rate,
    /// The API host unreachable: the stub closes each connection with no answer (`drop`).
    Drop,
    /// The API overloaded under two tabs of one route (v6 §14.17.5.4): the second resumes on the
    /// first's answered turn ([`judge_witnessed`]).
    Witness,
    /// An expired login (v6 §14.17.5.6): the API refuses the session's login with a 401 in its
    /// program's own words ([`login::raise`]), which only a person's sign-in clears
    /// ([`login::judge`]).
    Login,
}

impl Form {
    /// Every form, in its Cruise chains' order ([`Chain::SYSMSG`]).
    pub const ALL: [Form; 6] = [
        Form::Overload,
        Form::Outage,
        Form::Rate,
        Form::Drop,
        Form::Witness,
        Form::Login,
    ];

    /// The forms with a chain under Land too, in its order (v6 §14.17.5.6): all but the witness,
    /// whose second tab is Cruise's.
    pub const LAND: [Form; 5] = [
        Form::Overload,
        Form::Outage,
        Form::Rate,
        Form::Drop,
        Form::Login,
    ];

    /// The form of a system message's chain, under Cruise or Land; `None` for any other chain.
    #[must_use]
    pub const fn of(chain: Chain) -> Option<Form> {
        Some(match chain {
            Chain::SysOverload | Chain::SysLandOverload => Form::Overload,
            Chain::SysOutage | Chain::SysLandOutage => Form::Outage,
            Chain::SysRate | Chain::SysLandRate => Form::Rate,
            Chain::SysDrop | Chain::SysLandDrop => Form::Drop,
            Chain::SysWitness | Chain::SysWitnessMate => Form::Witness,
            Chain::SysLogin | Chain::SysLandLogin => Form::Login,
            _ => return None,
        })
    }

    /// Its chain under Cruise ([`Chain::SYSMSG`] holds them in [`Self::ALL`]'s order, which the
    /// tests hold).
    #[cfg(test)]
    #[must_use]
    pub const fn chain(self) -> Chain {
        match self {
            Form::Overload => Chain::SysOverload,
            Form::Outage => Chain::SysOutage,
            Form::Rate => Chain::SysRate,
            Form::Drop => Chain::SysDrop,
            Form::Witness => Chain::SysWitness,
            Form::Login => Chain::SysLogin,
        }
    }

    /// Its chain under Land, where it has one ([`Self::LAND`]).
    #[cfg(test)]
    #[must_use]
    pub const fn land_chain(self) -> Option<Chain> {
        Some(match self {
            Form::Overload => Chain::SysLandOverload,
            Form::Outage => Chain::SysLandOutage,
            Form::Rate => Chain::SysLandRate,
            Form::Drop => Chain::SysLandDrop,
            Form::Login => Chain::SysLandLogin,
            Form::Witness => return None,
        })
    }

    /// The stub's mode that stands in for it: the expired login's on agy and Muse Code, Claude
    /// Code's a custom mode of its own ([`login::raise`]).
    #[must_use]
    pub const fn stub_mode(self) -> &'static str {
        match self {
            Form::Overload | Form::Witness => "overloaded",
            Form::Outage => "500",
            Form::Rate => "rate-limit:150",
            Form::Drop => "drop",
            Form::Login => "401",
        }
    }

    /// The scenario's id in the report.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Form::Overload => "OVL",
            Form::Outage => "5XX",
            Form::Rate => "429",
            Form::Drop => "NET",
            Form::Witness => "WIT",
            Form::Login => "LOG",
        }
    }

    /// Its Land chain's scenario id in the report ([`Self::LAND`]).
    #[must_use]
    pub const fn land_id(self) -> &'static str {
        match self {
            Form::Overload => "LOVL",
            Form::Outage => "L5XX",
            Form::Rate => "L429",
            Form::Drop => "LNET",
            Form::Witness => "LWIT",
            Form::Login => "LLOG",
        }
    }

    /// What the stand-in is, in the lab's words.
    #[must_use]
    pub const fn what_stands(self) -> &'static str {
        match self {
            Form::Overload | Form::Witness => "API overload",
            Form::Outage => "API server error",
            Form::Rate => "rate limit",
            Form::Drop => "unreachable API host",
            Form::Login => "expired login",
        }
    }

    /// What the scenario holds, for the summary.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Form::Overload => {
                "the API overloaded under Cruise: nothing typed into it, waiting told, one resume"
            }
            Form::Outage => {
                "the API's server error under Cruise: nothing typed into it, waiting told, one resume"
            }
            Form::Rate => {
                "a rate limit under Cruise: nothing typed into it before the retry-after its words name (the ladder's rung where they name none), waiting told, one resume"
            }
            Form::Drop => {
                "the API host unreachable under Cruise: nothing typed into it, waiting told, one resume"
            }
            Form::Witness => {
                "the API overloaded under the first of two tabs of one route: nothing typed into it, waiting told, one resume"
            }
            Form::Login => login::what_on(Harness::Claude, false, false),
        }
    }

    /// What its Land chain's scenario holds, for the summary ([`Self::LAND`]).
    #[must_use]
    pub const fn what_land(self) -> &'static str {
        match self {
            Form::Overload | Form::Witness => {
                "the API overloaded under Land: no act of Land's into it, waiting told, one resume, Land carries on"
            }
            Form::Outage => {
                "the API's server error under Land: no act of Land's into it, waiting told, one resume, Land carries on"
            }
            Form::Rate => {
                "a rate limit under Land: no act of Land's into it, its tries where its words or the ladder say, one resume, Land carries on"
            }
            Form::Drop => {
                "the API host unreachable under Land: no act of Land's into it, waiting told, one resume, Land carries on"
            }
            Form::Login => login::what_on(Harness::Claude, true, false),
        }
    }

    /// The outage harness `h`'s reader makes of the stand-in (`turn_end::outage_of`): an overload
    /// where the program names one, a server error for a 5xx and a 429 (Muse Code reads the
    /// Responses API's 503 as a server error with its code), the connection's for a dropped host;
    /// none for the expired login, which is a person's to fix, never waited out.
    #[must_use]
    pub const fn outage(self, h: Harness) -> Option<Outage> {
        Some(match (self, h) {
            (Form::Overload | Form::Witness, Harness::Muse) | (Form::Outage | Form::Rate, _) => {
                Outage::ApiError
            }
            (Form::Overload | Form::Witness, _) => Outage::Overload,
            (Form::Drop, _) => Outage::Connection,
            (Form::Login, _) => return None,
        })
    }

    /// Why the form is out of this lab's reach on harness `h`: `None` where the stub stands in
    /// for it. The expired login is in reach on every harness here: each program's reader names
    /// its 401 a login wall (Claude Code's `Please run /login`, agy's `UNAUTHENTICATED`, Muse
    /// Code's rejected key), so none is a named skip.
    #[must_use]
    pub const fn out_of_reach(self, h: Harness) -> Option<&'static str> {
        let _ = h;
        match self {
            Form::Overload
            | Form::Outage
            | Form::Rate
            | Form::Drop
            | Form::Witness
            | Form::Login => None,
        }
    }

    /// Whether harness `h` reads the form's wait in the words its program draws (v6
    /// §14.17.5.4): the rate limit's on agy (Gemini's `RetryInfo`) and Muse Code (the Responses
    /// API's sentence); not on Claude Code, whose API names it in a `retry-after` header alone,
    /// which the program never draws.
    #[must_use]
    pub const fn waits_named(self, h: Harness) -> bool {
        matches!((self, h), (Form::Rate, Harness::Agy | Harness::Muse))
    }

    /// The gap from the act before it to try `n` (from 1) inside the standing wall on harness
    /// `h`: the outage ladder's rung; where the words name the rate limit's `retry-after`
    /// ([`Form::waits_named`], [`RATE_AFTER_MS`]), that wait first and no later try sooner (v6
    /// §14.17.5.4).
    #[must_use]
    pub fn rung_ms(self, h: Harness, n: u32) -> i64 {
        let ladder = rung_ms(n, 1.0);
        match (self.waits_named(h), n) {
            (false, _) => ladder,
            (true, ..=1) => RATE_AFTER_MS,
            (true, _) => ladder.max(RATE_AFTER_MS),
        }
    }
}

/// What harness `h` is launched with in a system message's lab: its own retries of a failed
/// request cut short, so its turn ends on the wall at once (Claude Code's
/// `CLAUDE_CODE_MAX_RETRIES=0`, agy's `AGY_CLI_MODEL_API_MAX_RETRIES=2`, Muse Code's
/// `provider_retry.max_attempts=1`): the environment, then the arguments.
#[must_use]
pub fn vendor_retries(h: Harness) -> (Vec<String>, Vec<String>) {
    match h {
        Harness::Claude => (vec!["CLAUDE_CODE_MAX_RETRIES=0".to_string()], Vec::new()),
        Harness::Agy => (
            vec!["AGY_CLI_MODEL_API_MAX_RETRIES=2".to_string()],
            Vec::new(),
        ),
        Harness::Muse => (
            Vec::new(),
            vec![
                "-c".to_string(),
                "provider_retry.max_attempts=1".to_string(),
            ],
        ),
    }
}

/// When the stand-in was met: Cruise's first act after its raise.
#[must_use]
pub fn met(seen: &Seen) -> Option<i64> {
    let raised = seen.cover.outage_ms?;
    seen.all_acts()
        .into_iter()
        .map(|(t, _)| *t)
        .find(|t| *t > raised)
}

/// The wall's first reading: the first point the loop read past the act that met the stand-in
/// (its `EVENT` journal line), the program's turn ended on the wall, on every build alike; `None`
/// before it.
#[must_use]
pub fn wall_read(seen: &Seen) -> Option<i64> {
    first_read(seen, met(seen)?, i64::MAX)
}

/// When the lab clears the stand-in: [`STANDS_MS`] past the wall's first reading
/// ([`wall_read`]); `None` before it, so the stand-in never clears before the program's turn
/// ended on it (a program whose own tries outlast the stand would otherwise meet no wall).
#[must_use]
pub fn lift_due(seen: &Seen) -> Option<i64> {
    wall_read(seen).map(|t| t + STANDS_MS)
}

/// `1 try`, `2 tries`.
fn tries_said(n: u32) -> String {
    format!("{n} {}", if n == 1 { "try" } else { "tries" })
}

/// How many times the harness's glass (`harness_acts`) fired in a `metrics` reply's
/// `deadline_fires_by_owner`; `None` where the reply names no such field, zero where it names no
/// such owner.
#[must_use]
pub fn harness_acts_fires(metrics: &str) -> Option<u64> {
    let fires = word(metrics, "deadline_fires_by_owner")?;
    Some(
        fires
            .split(',')
            .find_map(|o| o.strip_prefix("harness_acts:"))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0),
    )
}

/// What the wakes while the wall stood say against the wait's budget, `tries` typed on their
/// rungs: the harness's glass fires once at the meeting, once a try, and once at the clearing, at
/// the most; more is a wake the wait added past its back-off. Nothing judged where the metrics
/// reset at the raise failed (the figure would count wakes from before it) or no figure was read.
#[must_use]
pub fn wake_faults(seen: &Seen, tries: u32) -> Vec<String> {
    if seen.cover.wall_wakes_unreset {
        return Vec::new();
    }
    let Some(fires) = seen
        .cover
        .wall_wakes
        .as_deref()
        .and_then(harness_acts_fires)
    else {
        return Vec::new();
    };
    let bound = u64::from(tries).saturating_add(2);
    if fires > bound {
        vec![format!(
            "WAKES PAST THE BACK-OFF: the harness's glass fired {fires} times while the wall \
             stood, past the {bound} its {}, the meeting and the clearing allow",
            tries_said(tries)
        )]
    } else {
        Vec::new()
    }
}

/// Whether `tab`'s stand-in is due: under Cruise at its first point while nothing was typed, so
/// Cruise's own act meets it; under Land once the stub read the chain's first prompt, whose
/// answer the stub holds until the lab opens its gate ([`Chain::raise_gate`], [`open_gate`]), so
/// Land's act on that answer, which comes at once, meets it (v6 §14.17.5.6).
fn raise_due(run: &Run, tab: &Tab) -> bool {
    if tab.chain.raise_gate().is_some() {
        !super::dumps_of(run, &tab.chain.token()).is_empty()
    } else {
        first_point_unanswered(&tab.seen, tab.t0)
    }
}

/// Opens `tab`'s raise gate where it has one ([`Chain::raise_gate`]): the stub answers the first
/// prompt now that the stand-in stands.
pub(super) fn open_gate(run: &Run, tab: &Tab) {
    let Some(gate) = tab.chain.raise_gate() else {
        return;
    };
    let path = crate::stub::gate_path(&run.w.join("modes"), &gate);
    match crate::util::write_fresh(&path, b"") {
        Ok(()) => println!(
            "  [{}] +{} gate    the stand-in stands: the first prompt's answer goes",
            tab.chain.name(),
            at(tab.t0, crate::util::now_ms())
        ),
        Err(e) => println!(
            "  [{}] gate    {} not written: {e}",
            tab.chain.name(),
            path.display()
        ),
    }
}

/// Raises the stand-in of `tab`'s form when it is due ([`raise_due`]), and lifts it at
/// [`lift_due`], reading the instance's wakes while it stood; the expired login's is
/// [`login::step`]'s, harness `h`'s.
///
/// # Errors
/// The stub did not take the mode.
pub(super) fn step(
    run: &Run,
    door: &Door<'_>,
    h: Harness,
    tab: &mut Tab,
    now: i64,
) -> Result<(), Fail> {
    let Some(form) = Form::of(tab.chain).filter(|_| tab.chain != Chain::SysWitnessMate) else {
        return Ok(());
    };
    if form == Form::Login {
        return login::step(run, door, h, tab, now);
    }
    let cover = &tab.seen.cover;
    if cover.outage_lifted_ms.is_some() {
        return Ok(());
    }
    if cover.outage_ms.is_none() {
        if raise_due(run, tab) {
            tab.seen.cover.wall_wakes_unreset =
                !door.ctl(&["metrics", "reset"]).is_ok_and(|r| r.code == 0);
            let answer = crate::verbs::set_mode(run, form.stub_mode())?;
            tab.seen.cover.outage_ms = Some(now);
            println!(
                "  [{}] +{} stub    the {} raised: mode {}, cleared {} s past the wall's first reading ({})",
                tab.chain.name(),
                at(tab.t0, now),
                form.what_stands(),
                form.stub_mode(),
                STANDS_MS / 1000,
                one_line(&answer, 120)
            );
            open_gate(run, tab);
        }
        return Ok(());
    }
    if lift_due(&tab.seen).is_some_and(|due| now >= due) {
        tab.seen.cover.wall_wakes = Some(wakes(&door.out(&["metrics"])));
        let answer = crate::verbs::set_mode(run, super::MODE)?;
        tab.seen.cover.outage_lifted_ms = Some(now);
        println!(
            "  [{}] +{} stub    the stand-in cleared: mode {} again ({})",
            tab.chain.name(),
            at(tab.t0, now),
            super::MODE,
            one_line(&answer, 120)
        );
    }
    Ok(())
}

/// Whether the chain's watch is over: Cruise gone on past the resume [`WATCH_PAST_MS`] ago, or
/// the resume's turn [`RESUME_TURN_MS`] old with nothing after it; under Land (`land`), Land's
/// landing or hold past the resume [`WATCH_PAST_MS`] ago, or [`LAND_ON_MS`] past the resume with
/// neither.
#[must_use]
pub fn watched(seen: &Seen, land: bool, now: i64) -> bool {
    let Some(clear) = seen.cover.outage_lifted_ms else {
        return false;
    };
    let on = acts_after(seen, clear);
    let Some(resumed) = on
        .iter()
        .find(|a| a.rule == RULE_OUTAGE_RESUME)
        .map(|a| a.at)
    else {
        return false;
    };
    if land {
        return match land_end(seen, resumed) {
            Some((t, _)) => now - t >= WATCH_PAST_MS,
            None => now - resumed >= LAND_ON_MS,
        };
    }
    match on
        .iter()
        .find(|a| a.at > resumed && a.rule != RULE_OUTAGE_RESUME)
    {
        Some(a) => now - a.at >= WATCH_PAST_MS,
        None => now - resumed >= RESUME_TURN_MS,
    }
}

/// How Land's run ended past `since`: its landing (`DONE`) or its hold, whichever came first, and
/// the line.
#[must_use]
pub fn land_end(seen: &Seen, since: i64) -> Option<(i64, &str)> {
    let done = seen
        .first_after("DONE ", since)
        .map(|(t, l)| (*t, l.as_str()));
    let held = stall_of(seen, since + 1).map(|(t, l)| (*t, l.as_str()));
    match (done, held) {
        (Some(d), Some(h)) => Some(if h.0 < d.0 { h } else { d }),
        (d, h) => d.or(h),
    }
}

/// Judges one form's chain on `program` ([`Harness::name`]), under Land where `land` (v6
/// §14.17.5.6): the same wants inside the standing wall, where no act of Land's may come either
/// (`land@v1`, `land-nudge@v1`, `land-check@v1`, `land-answer@v1`: [`ladder_faults`] counts
/// any act but the outage's tries), and past the one resume Land carries on to its landing or
/// its hold ([`land_on`]) where Cruise goes on.
#[must_use]
pub fn judge(seen: &Seen, form: Form, land: bool, program: &str, t0: i64) -> Verdict {
    let mode = if land { "Land" } else { "Cruise" };
    let Some(raised) = seen.cover.outage_ms else {
        return Verdict::Unobserved(if land {
            "the stub's stand-in was never raised (the first prompt never reached it)".into()
        } else {
            "the stub's stand-in was never raised (no first point read)".into()
        });
    };
    let Some(t1) = met(seen) else {
        return Verdict::Fail(format!(
            "the {} raised at +{}, but no act of {mode}'s met it within +{}",
            form.what_stands(),
            at(t0, raised),
            at(t0, seen.end_ms)
        ));
    };
    let Some(clear) = seen.cover.outage_lifted_ms else {
        return Verdict::Unobserved(format!(
            "the {} was never cleared within +{}",
            form.what_stands(),
            at(t0, seen.end_ms)
        ));
    };
    let Some(h) = Harness::of(program) else {
        return Verdict::Unobserved(format!("no harness {program}"));
    };
    let Some(want) = form.outage(h) else {
        return Verdict::Unobserved(format!(
            "the {} is no outage: the login's judge holds it",
            form.what_stands()
        ));
    };
    let first = acts_after(seen, t1 - 1)
        .first()
        .map_or("?", |a| a.rule)
        .to_string();
    let mut said = format!(
        "the {} raised at +{} (cleared +{}); {mode}'s {first} typed at +{} met it",
        form.what_stands(),
        at(t0, raised),
        at(t0, clear),
        at(t0, t1)
    );
    let after: Vec<_> = acts_after(seen, t1)
        .into_iter()
        .filter(|a| a.at >= clear)
        .collect();
    let (tries, _, mut bad) = ladder_faults(seen, (form, h), (t1, clear), t0);
    let mut unseen: Vec<String> = Vec::new();
    if tries > 0 {
        said.push_str(&format!(
            "; {} on the ladder's rungs while it stood",
            tries_said(tries)
        ));
    }
    if form.waits_named(h)
        && let Some(first) = acts_after(seen, t1).first().filter(|a| a.at < clear)
    {
        said.push_str(&format!(
            "; its first try {} after the act that met it (the retry-after its words name: {})",
            at(t1, first.at),
            at(0, RATE_AFTER_MS)
        ));
    }
    bad.extend(stood_faults(seen, (t1, clear), land, t0));
    match wait_after(seen, t1, clear) {
        Some((tw, _)) => said.push_str(&format!("; waited from +{}", at(t0, tw))),
        None => bad.push(format!(
            "NOT WAITED: no `waiting: {}` journaled while the wall stood",
            want.words()
        )),
    }
    let words: Vec<(i64, String)> = seen
        .status
        .iter()
        .filter(|(t, _)| *t > t1 && *t < clear)
        .map(|(t, s)| (*t, word(s, "waiting").unwrap_or_else(|| "-".to_string())))
        .collect();
    match words.iter().find(|(_, w)| w == want.word()) {
        Some((t, w)) => said.push_str(&format!("; status waiting={w} at +{}", at(t0, *t))),
        None => bad.push(format!(
            "NOT WAITING IN STATUS: `aterm ctl status` never said waiting={} while the wall stood (it said: {})",
            want.word(),
            if words.is_empty() {
                "no status read".to_string()
            } else {
                let mut seen_words: Vec<&str> = words.iter().map(|(_, w)| w.as_str()).collect();
                seen_words.dedup();
                seen_words.join(", ")
            }
        )),
    }
    let resumes: Vec<_> = after
        .iter()
        .filter(|a| a.rule == RULE_OUTAGE_RESUME)
        .collect();
    match after.first() {
        None => bad.push(format!(
            "NO RESUME: nothing typed past the clearing within +{}",
            at(t0, seen.end_ms)
        )),
        Some(a) if a.rule != RULE_OUTAGE_RESUME => bad.push(format!(
            "NO RESUME: {mode}'s {} typed at +{} first past the clearing",
            a.rule,
            at(t0, a.at)
        )),
        Some(_) => {}
    }
    if resumes.len() > 1 {
        bad.push(format!(
            "{} resumes past the clearing: {}",
            resumes.len(),
            resumes
                .iter()
                .map(|a| format!("{} at +{}", a.rule, at(t0, a.at)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(r) = resumes.first() {
        if turn_end::is_cleared_line(r.text)
            || r.text.contains(&super::outage::line(want))
            || r.text.contains(turn_end::cleared_text(
                turn_end::Cleared::Outage(want),
                turn_end::Clearing::Measured,
            ))
        {
            said.push_str(&format!(
                "; one resume, {} at +{}: \"{}\"",
                r.rule,
                at(t0, r.at),
                one_line(r.text, 100)
            ));
        } else {
            bad.push(format!(
                "the resume ({} at +{}) is not the cleared line: \"{}\"",
                r.rule,
                at(t0, r.at),
                one_line(r.text, 100)
            ));
        }
        let next = after
            .iter()
            .find(|a| a.at > r.at)
            .map_or(i64::MAX, |a| a.at);
        // A want it could not read is decided last: every other want still judges the run, so
        // one unread never hides another's fault.
        match streak_kept(seen, t1, (r.at, "the resume", next), t0) {
            Ok(kept) => said.push_str(&kept),
            Err(LandOn::Fault(why)) => bad.push(why),
            Err(LandOn::Unwatched(why)) => unseen.push(why),
        }
        if land {
            match land_on(seen, r.at, t0) {
                Ok(on) => said.push_str(&on),
                Err(LandOn::Fault(why)) => bad.push(why),
                Err(LandOn::Unwatched(why)) => unseen.push(why),
            }
        } else if let Some(a) = after
            .iter()
            .find(|a| a.at > r.at && a.rule != RULE_OUTAGE_RESUME)
        {
            said.push_str(&format!(
                "; Cruise went on with {} at +{}",
                a.rule,
                at(t0, a.at)
            ));
        }
    }
    if seen.cover.wall_wakes_unreset {
        said.push_str(
            "; the wakes while it stood UNOBSERVED (the metrics reset at the raise failed)",
        );
    } else if let Some(w) = &seen.cover.wall_wakes {
        said.push_str(&format!(
            "; while it stood ({} s): {w}",
            (clear - raised) / 1000
        ));
    }
    bad.extend(wake_faults(seen, tries));
    if !bad.is_empty() {
        for why in &unseen {
            said.push_str(&format!("; {why}"));
        }
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    } else if !unseen.is_empty() {
        Verdict::Unobserved(format!("{said}; {}", unseen.join("; ")))
    } else {
        Verdict::Pass(said)
    }
}

/// Why Land's run past the resume is not a carrying on ([`land_on`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandOn {
    /// It is not: the fault, in the verdict's words.
    Fault(String),
    /// The watch ended before Land landed or held, short of [`LAND_ON_MS`]: unobserved.
    Unwatched(String),
}

/// LAND CARRIES ON PAST THE RESUME at `resumed` (v6 §14.17.5.6), as in the land lab: it lands
/// (`DONE … rule=land@v1`) within [`LAND_ON_MS`], typing nothing on the way but its own acts
/// (`turn_end::is_land_rule`) and no second resume. The chain's stub answers every act of Land's
/// clean and names no need, so a hold is no carrying on: it parks the session the wait was
/// meant to keep going. What it did, in the verdict's words ([`land_on_by`]).
///
/// # Errors
/// [`LandOn::Fault`] where it did not; [`LandOn::Unwatched`] where the watch ended first.
pub fn land_on(seen: &Seen, resumed: i64, t0: i64) -> Result<String, LandOn> {
    land_on_by(seen, resumed, resumed + LAND_ON_MS, t0)
}

/// [`land_on`] with its landing or hold due by `by` (the expired login's: past the back-off the
/// person's turn drew, `login::land_by`).
///
/// # Errors
/// [`LandOn::Fault`] where it did not; [`LandOn::Unwatched`] where the watch ended first.
pub fn land_on_by(seen: &Seen, resumed: i64, by: i64, t0: i64) -> Result<String, LandOn> {
    let Some((end, line)) = land_end(seen, resumed) else {
        return if seen.end_ms >= by {
            Err(LandOn::Fault(format!(
                "LAND DID NOT CARRY ON: neither landed nor held in the {} watched past the resume",
                at(0, seen.end_ms - resumed)
            )))
        } else {
            Err(LandOn::Unwatched(format!(
                "watched to +{}, Land not landed or held yet",
                at(t0, seen.end_ms)
            )))
        };
    };
    let on: Vec<_> = acts_after(seen, resumed)
        .into_iter()
        .filter(|a| a.at < end)
        .collect();
    if let Some(a) = on.iter().find(|a| !turn_end::is_land_rule(a.rule)) {
        return Err(LandOn::Fault(format!(
            "NOT LAND'S: {} typed at +{} between the resume and Land's {} at +{}",
            a.rule,
            at(t0, a.at),
            if line.starts_with("DONE ") {
                "landing"
            } else {
                "hold"
            },
            at(t0, end)
        )));
    }
    if !line.starts_with("DONE ") || word(line, "rule").as_deref() != Some(turn_end::RULE_LAND) {
        return Err(LandOn::Fault(format!(
            "HELD, NOT LANDED: Land's run past the resume ended at +{} on {}, where the stub answered each of its acts clean",
            at(t0, end),
            one_line(line, 100)
        )));
    }
    if end > by {
        return Err(LandOn::Fault(format!(
            "LAND DID NOT CARRY ON: its landing came only {} past the resume, {} past when it was due",
            at(resumed, end),
            at(by, end)
        )));
    }
    let acts = if on.is_empty() {
        "nothing typed".to_string()
    } else {
        on.iter()
            .map(|a| format!("{} at +{}", a.rule, at(t0, a.at)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Ok(format!(
        "; Land carried on ({acts}) and landed at +{}: {}",
        at(t0, end),
        one_line(line, 100)
    ))
}

/// THE STREAK STANDS AT THE AGENT'S OWN SHORT ANSWERS past a wall (v6 §14.17.5.7). The act that
/// met the wall at `met` was answered by the wall, not the agent (the wall's one pass, as the
/// first cut since the agent's last answer is); the wall's tries, its resume and a person's own
/// turn are no short answers of the agent's to aterm: none adds to the short streak, and none
/// resets it. So every point record (`point@v1`) past `from` (the resume, or the person's prompt)
/// and before `before` (aterm's next act), and every back-off journaled there, names the short
/// turns in a row the point record that typed that act named, the last at or before `met`: no
/// more, and, as the sysmsg stub answers every act short with no tool, so that no turn there is
/// real work, no fewer. What it read, in the verdict's words.
///
/// # Errors
/// [`LandOn::Fault`] where one names more (a build that counts the act the wall cut or the
/// person's turn: 12d's one short turn before an expired login, three past the person's sign-in
/// and an 8 min back-off) or fewer (a build that resets the streak at a wall or a person's turn,
/// which leaves a wall that cuts every act prompting at one pace); [`LandOn::Unwatched`] where no
/// point record stands at or before `met` or none was journaled past `from` before `before` and
/// the watch's end.
pub fn streak_kept(
    seen: &Seen,
    met: i64,
    (from, past, before): (i64, &str, i64),
    t0: i64,
) -> Result<String, LandOn> {
    let Some((then, was)) = seen
        .ledger
        .iter()
        .filter(|r| r.rule == RULE_POINT && r.ts <= met)
        .filter_map(|r| Some((r.ts, point_streak(r)?)))
        .next_back()
    else {
        return Err(LandOn::Unwatched(format!(
            "no point record at or before the act that met the wall (+{}), so the streak it was \
             typed on is unread",
            at(t0, met)
        )));
    };
    let mut named: Vec<(i64, u32, &str)> = seen
        .ledger
        .iter()
        .filter(|r| r.rule == RULE_POINT && r.ts > from && r.ts < before)
        .filter_map(|r| Some((r.ts, point_streak(r)?, "point")))
        .chain(
            seen.journal
                .iter()
                .filter(|(t, l)| *t > from && *t < before && backoff_named(l))
                .filter_map(|(t, l)| Some((*t, backoff_streak(l)?, "back-off"))),
        )
        .collect();
    named.sort_by_key(|(t, _, _)| *t);
    if let Some((t, n, what)) = named.iter().find(|(_, n, _)| *n > was) {
        return Err(LandOn::Fault(format!(
            "COUNTED A TURN THAT WAS NOT THE AGENT'S ANSWER: at +{} a {what} names {n} short turns \
             in a row past {past} (+{}), of the {was} the point at +{} named before the act the \
             wall cut (+{}): the cut act, the wall's tries and resume and a person's own turn add \
             nothing (v6 §14.17.5.7)",
            at(t0, *t),
            at(t0, from),
            at(t0, then),
            at(t0, met)
        )));
    }
    if let Some((t, n, what)) = named.iter().find(|(_, n, _)| *n < was) {
        return Err(LandOn::Fault(format!(
            "DROPPED THE AGENT'S OWN SHORT ANSWERS: at +{} a {what} names {n} short turns in a \
             row past {past} (+{}), of the {was} the point at +{} named before the act the wall \
             cut (+{}), with no real work between: a wall and a person's own turn reset nothing \
             (v6 §14.17.5.7)",
            at(t0, *t),
            at(t0, from),
            at(t0, then),
            at(t0, met)
        )));
    }
    match named.iter().map(|(_, n, _)| *n).max() {
        Some(n) => Ok(format!(
            "; the streak kept: {n} short turn(s) in a row past {past}, of the {was} before the act \
             the wall cut"
        )),
        None => Err(LandOn::Unwatched(format!(
            "no point judged past {past} (+{}) before +{}, so the streak past it is unread",
            at(t0, from),
            at(t0, before.min(seen.end_ms))
        ))),
    }
}

/// A system message's form the stub cannot play, a note in every `--mode sysmsg` run, not a
/// verdict, which no run could observe (v6 §14.17.5.4): the probe's own forms, which a lab
/// instance never reaches, as it runs with `probe_api` off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unplayed {
    /// The API host refusing the connection.
    Refused,
    /// The API host's name not resolving.
    Unresolved,
}

impl Unplayed {
    /// Every unplayed form.
    pub const ALL: [Unplayed; 2] = [Unplayed::Refused, Unplayed::Unresolved];

    /// Its verdict id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Unplayed::Refused => "REF",
            Unplayed::Unresolved => "DNS",
        }
    }

    /// What it would show.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Unplayed::Refused => {
                "the API host refusing the connection: measured down, nothing typed, one resume"
            }
            Unplayed::Unresolved => {
                "the API host's name not resolving: measured down, nothing typed, one resume"
            }
        }
    }

    /// Why the lab cannot play it.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Unplayed::Refused => {
                "the stub has no mode that refuses a connection (it answers or drops each request on its one listening port), and a lab instance runs with probe_api off, so no probe would read the refusal: the probe's tests hold it"
            }
            Unplayed::Unresolved => {
                "the agents reach the stub by its loopback address, which no resolver fails, and a lab instance runs with probe_api off: the probe's tests hold an unresolvable host"
            }
        }
    }
}

/// Inside a standing wall (`(from, clear)`, `from` the act or the point that met it) only the
/// outage's tries, each on its rung from the act before it ([`Form::rung_ms`] on harness `h`,
/// the earliest its wall can be) with a wait journaled between: a condition that names no
/// clearing is tried blind on the ladder (v6 §14.17.5.2), a rate limit naming its wait first
/// after that (v6 §14.17.5.4). Each try is the outage's blind line (nothing measured the wall's
/// end while it stood) and comes no later than [`late_ms`] past its rung from its wall's reading
/// (the point read past the act before it); a wall that stood that long past its reading saw a
/// try. The tries counted, the last one's time (`from` with none), and every fault.
fn ladder_faults(
    seen: &Seen,
    (form, h): (Form, Harness),
    (from, clear): (i64, i64),
    t0: i64,
) -> (u32, i64, Vec<String>) {
    let mut bad = Vec::new();
    let blind = form.outage(h).map(line).unwrap_or_default();
    let (mut tries, mut prev) = (0_u32, from);
    for a in acts_after(seen, from).into_iter().filter(|a| a.at < clear) {
        let rung = form.rung_ms(h, tries + 1);
        let on_rung = a.rule == RULE_OUTAGE_RESUME
            && a.at - prev >= rung - DUE_SLACK_MS
            && wait_after(seen, prev, a.at).is_some();
        if on_rung {
            tries += 1;
            let wall = first_read(seen, prev, a.at).unwrap_or(prev);
            if a.at - wall > rung + late_ms(rung) {
                bad.push(format!(
                    "LATE: try {tries} at +{}, {} past its wall's reading at +{}, its rung {}",
                    at(t0, a.at),
                    at(wall, a.at),
                    at(t0, wall),
                    at(0, rung)
                ));
            }
            if !a.text.contains(&blind) {
                bad.push(format!(
                    "NOT BLIND INSIDE THE STANDING WALL: try {tries} at +{} is not the outage's \
                     blind line (\"{}\"): \"{}\"",
                    at(t0, a.at),
                    one_line(&blind, 80),
                    one_line(a.text, 100)
                ));
            }
            prev = a.at;
        } else {
            bad.push(format!(
                "TYPED INTO THE STANDING WALL: {} at +{}{}",
                a.rule,
                at(t0, a.at),
                if a.rule == RULE_OUTAGE_RESUME {
                    format!(", off its rung ({} from the act before it)", at(0, rung))
                } else {
                    String::new()
                }
            ));
        }
    }
    let rung = form.rung_ms(h, tries + 1);
    if let Some(wall) = first_read(seen, prev, clear)
        && clear - wall > rung + late_ms(rung)
    {
        bad.push(format!(
            "NO TRY ON ITS RUNG: the wall read at +{} stood {} more, and try {} on its rung {} \
             never came",
            at(t0, wall),
            at(wall, clear),
            tries + 1,
            at(0, rung)
        ));
    }
    (tries, prev, bad)
}

/// THE WITNESS LAB'S SECOND TAB (v6 §14.17.5.4): given the first tab's raise and clearing, by
/// which it is judged and watched ([`judge_witnessed`], [`watched`]), and prompted into the
/// standing wall [`MATE_AFTER_MS`] past the first tab's wall's first reading.
///
/// # Errors
/// The composer could not be read.
pub(super) fn mate_step(
    door: &Door<'_>,
    h: Harness,
    tabs: &mut [Tab],
    now: i64,
) -> Result<(), Fail> {
    let Some(first) = tabs.iter().find(|t| t.chain == Chain::SysWitness) else {
        return Ok(());
    };
    let cover = &first.seen.cover;
    let (raised, lifted, wall) = (
        cover.outage_ms,
        cover.outage_lifted_ms,
        wall_read(&first.seen),
    );
    let first_t0 = first.t0;
    let Some(mate) = tabs
        .iter_mut()
        .find(|t| t.chain == Chain::SysWitnessMate && !t.over)
    else {
        return Ok(());
    };
    mate.seen.cover.outage_ms = raised;
    mate.seen.cover.outage_lifted_ms = lifted;
    let due = wall.map(|w| w + MATE_AFTER_MS);
    if mate.seen.cover.mate_ms.is_some() || due.is_none_or(|d| now < d) {
        return Ok(());
    }
    let sent = crate::util::now_ms();
    let took = tab_turn(door, &mate.sid, h.name(), &mate.chain.prompt())?;
    mate.seen.cover.mate_ms = Some(sent);
    println!(
        "  [{}] +{} prompt  into the standing wall, {} past the first tab's wall (+{} on its clock){}",
        mate.chain.name(),
        at(mate.t0, sent),
        at(0, MATE_AFTER_MS),
        wall.map_or_else(|| "?".to_string(), |w| at(first_t0, w)),
        if took {
            String::new()
        } else {
            " — the composer did NOT take it".to_string()
        }
    );
    if !took {
        mate.over = true;
    }
    Ok(())
}

/// Judges the witness lab's second tab on `program` ([`Harness::name`]): its prompt met the
/// standing wall; inside it only the outage's tries on their rungs; past the clearing its first
/// act is the outage's measured line (*has cleared*), on a witness its journal names, once, and
/// no blind line follows the clearing (v6 §14.17.5.4: an outage clears on a witness, not a blind
/// try).
#[must_use]
pub fn judge_witnessed(seen: &Seen, program: &str, t0: i64) -> Verdict {
    let Some(prompted) = seen.cover.mate_ms else {
        return Verdict::Unobserved(
            "the second tab was never prompted (the first tab's wall was never read)".into(),
        );
    };
    let Some(clear) = seen.cover.outage_lifted_ms else {
        return Verdict::Unobserved(format!(
            "the API overload was never cleared within +{}",
            at(t0, seen.end_ms)
        ));
    };
    let Some(h) = Harness::of(program) else {
        return Verdict::Unobserved(format!("no harness {program}"));
    };
    let want = Form::Witness.outage(h).unwrap_or(Outage::Overload);
    let Some(wall) = first_read(seen, prompted, clear) else {
        return Verdict::Fail(format!(
            "the second tab's prompt at +{} met no wall: no point read before the clearing at +{}",
            at(t0, prompted),
            at(t0, clear)
        ));
    };
    let mut said = format!(
        "its prompt at +{} met the standing overload (point +{}, cleared +{})",
        at(t0, prompted),
        at(t0, wall),
        at(t0, clear)
    );
    let (tries, last, mut bad) = ladder_faults(seen, (Form::Witness, h), (prompted, clear), t0);
    if tries > 0 {
        said.push_str(&format!(
            "; {} on the ladder's rungs while it stood",
            tries_said(tries)
        ));
    }
    let after = acts_after(seen, clear - 1);
    let measured = turn_end::cleared_text(
        turn_end::Cleared::Outage(want),
        turn_end::Clearing::Measured,
    );
    let blind = turn_end::cleared_text(turn_end::Cleared::Outage(want), turn_end::Clearing::Blind);
    match after.first() {
        None => bad.push(format!(
            "NO RESUME: nothing typed past the clearing within +{}",
            at(t0, seen.end_ms)
        )),
        Some(a) if a.rule != RULE_OUTAGE_RESUME => bad.push(format!(
            "NO RESUME: Cruise's {} typed at +{} first past the clearing",
            a.rule,
            at(t0, a.at)
        )),
        Some(a) if !a.text.contains(measured) => bad.push(format!(
            "NOT ON A WITNESS: the resume at +{} is not the measured line (\"{}\"), \"{}\"",
            at(t0, a.at),
            one_line(measured, 80),
            one_line(a.text, 100)
        )),
        Some(a) => {
            let next_rung = last + Form::Witness.rung_ms(h, tries + 1);
            said.push_str(&format!(
                "; resumed at +{}, {} past the clearing and {} before its own next rung: \"{}\"",
                at(t0, a.at),
                at(clear, a.at),
                at(a.at, next_rung),
                one_line(a.text, 100)
            ));
            match seen
                .journal
                .iter()
                .find(|(t, l)| *t >= clear && *t <= a.at + 2_000 && l.contains(WITNESS_MARK))
            {
                Some((t, l)) => said.push_str(&format!(
                    "; journal +{}: {}",
                    at(t0, *t),
                    one_line(l, 140)
                )),
                None => bad.push(format!(
                    "NO WITNESS JOURNALED: no `witness:` note between the clearing and the resume at +{}",
                    at(t0, a.at)
                )),
            }
        }
    }
    let blinds: Vec<_> = after
        .iter()
        .filter(|a| a.rule == RULE_OUTAGE_RESUME && a.text.contains(blind))
        .collect();
    if !blinds.is_empty() {
        bad.push(format!(
            "BLIND PAST THE CLEARING: {}",
            blinds
                .iter()
                .map(|a| format!("{} at +{}", a.rule, at(t0, a.at)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let resumes = after
        .iter()
        .filter(|a| a.rule == RULE_OUTAGE_RESUME)
        .count();
    if resumes > 1 {
        bad.push(format!("{resumes} resumes past the clearing"));
    }
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

/// The window want of a system message's chain ([`state_line`]).
pub const STATE_LINE_WANT: &str =
    "while the wall stood, its state line led with `waiting: <the outage's words>`, no need on it";

/// A system message's window want ([`STATE_LINE_WANT`]): of the tab's Settings ▸ Harness rows read
/// while the wall stood (from its first wait to the clearing), one leads its state line with
/// `waiting: <words>` (`Outage::words` of [`Form::outage`]), and none says a need (v6 §14.17.5: an
/// outage shows as `waiting: <condition>` on the tab's state line and nowhere else).
#[must_use]
pub fn state_line(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some(want) = Form::of(cx.chain)
        .zip(Harness::of(cx.program))
        .and_then(|(f, h)| f.outage(h))
    else {
        return Verdict::Unobserved("no system message's outage chain".into());
    };
    let Some(met) = met(seen) else {
        return Verdict::Unobserved("the stand-in was never met".into());
    };
    let stood_to = seen.cover.outage_lifted_ms.unwrap_or(seen.end_ms + 1);
    let Some((first, _)) = wait_after(seen, met, stood_to) else {
        return Verdict::Unobserved("no wait on the wall was journaled".into());
    };
    let w = &seen.window;
    let rows = observed(&w.page, &w.page_reads, first, stood_to);
    if rows.is_empty() {
        return Verdict::Unobserved("the Harness page was not read while the wall stood".into());
    }
    let lines: Vec<(i64, &str)> = rows
        .iter()
        .filter(|(_, p)| *p != "-")
        .filter_map(|(t, p)| Some((*t, page_field(p, "state")?)))
        .collect();
    if lines.is_empty() {
        return Verdict::Unobserved(
            "the tab's row was never on the Harness page while the wall stood".into(),
        );
    }
    let head = format!("waiting: {}", want.words());
    let mut bad = Vec::new();
    if let Some((t, l)) = lines.iter().find(|(_, l)| l.contains("needs you:")) {
        bad.push(format!(
            "+{} a need on the state line: \"{}\"",
            at(cx.t0, *t),
            one_line(l, 160)
        ));
    }
    let said = match lines.iter().find(|(_, l)| l.starts_with(&head)) {
        Some((t, l)) => format!(
            "+{} \"{}\" ({} read(s) while the wall stood)",
            at(cx.t0, *t),
            one_line(l, 160),
            rows.len()
        ),
        None => {
            bad.push(format!(
                "NEVER SAID WAITING: none of its {} state line(s) leads with \"{head}\"{}",
                lines.len(),
                lines.last().map_or_else(String::new, |(t, l)| format!(
                    ", the last +{} \"{}\"",
                    at(cx.t0, *t),
                    one_line(l, 160)
                ))
            ));
            format!("{} read(s) while the wall stood", rows.len())
        }
    };
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

#[path = "forever_sysmsg_login.rs"]
pub mod login;
