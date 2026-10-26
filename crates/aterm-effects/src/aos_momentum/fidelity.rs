// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `momentum-glow.lane-fidelity` v1: judges a closed-loop lane run ([`LaneRun`]) against a fresh shipped engine re-run over its recorded calls, never the candidate's state.

use std::time::Duration;

use aterm_aos::contract::Verdict;
use aterm_aos::wire::Enc;
use aterm_aos::{ArtifactId, ArtifactKind};
use aterm_time::Instant;

use super::MomentumEvent;
use super::lane::{After, HANG_NS, LANE_MAX_EVENTS, LANE_MODEL, LaneRun, encode_glass, glass_key};
use crate::cursor_momentum::{MomentumGlow, MomentumGlowFrame};

/// The contract artifact's name.
pub const LANE_FIDELITY_NAME: &str = "momentum-glow.lane-fidelity";

/// The law: no tick leaves its frame on glass across a step the shipped engine would draw (blind to the interval after a presentation change).
pub const NO_STEP_SLEPT: &str = "NoStepSlept";
/// The law: the glass is inert at the scenario's end.
pub const CLEARED_BY_END: &str = "ClearedByEnd";
/// The law: the lane's wakes move forward and the run is bounded.
pub const LANE_SETTLES: &str = "LaneSettles";

/// δ: how far before the next lane event the probe is drawn, more than `SETTLE_MARGIN`, so a probe δ before a shipped wake is before the step it was armed for.
pub const PROBE_DELTA: Duration = Duration::from_millis(2);

/// The oracle, as the descriptor names it.
pub const ORACLE: &str = "a first-party shipped tick (aterm_effects::cursor_momentum::MomentumGlow::tick) \
     on a copy of the engine state after p's tick, re-run from rest over the run's recorded keys, \
     resets and ticks, drawn at t - delta with p's presentation when t - delta > p + f";

#[inline]
fn fail(cursor: usize, law: &'static str, detail: String) -> Verdict {
    Verdict::Fail {
        cursor: u64::try_from(cursor).unwrap_or(u64::MAX),
        law,
        detail,
    }
}

/// `origin + at` for a recorded offset (never negative in a run).
#[inline]
fn instant(origin: Instant, at: u64) -> Instant {
    origin + Duration::from_nanos(at)
}

/// A recorded call's offset as the lane's `u64`.
#[inline]
const fn offset(at: i128) -> u64 {
    if at >= 0 && at <= u64::MAX as i128 {
        at as u64
    } else {
        u64::MAX
    }
}

/// The `momentum-glow.lane-fidelity` contract, version 1.
#[derive(Clone, Debug)]
pub struct LaneFidelity {
    id: ArtifactId,
}

impl Default for LaneFidelity {
    fn default() -> Self {
        Self::new()
    }
}

impl LaneFidelity {
    /// The contract.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        LaneFidelity {
            id: Self::artifact(),
        }
    }

    /// Its artifact, kind Contract, version 1, whose descriptor names the three laws, δ, the event and hang bounds, the lane model and the oracle.
    #[must_use]
    pub fn artifact() -> ArtifactId {
        let mut e = Enc::new();
        for law in [NO_STEP_SLEPT, CLEARED_BY_END, LANE_SETTLES] {
            e.str(law);
        }
        e.duration(PROBE_DELTA);
        e.u64(u64::try_from(LANE_MAX_EVENTS).unwrap_or(u64::MAX));
        e.u64(HANG_NS);
        e.str(LANE_MODEL);
        e.str(ORACLE);
        ArtifactId::new(ArtifactKind::Contract, LANE_FIDELITY_NAME, 1, e.as_bytes())
    }

    /// Its identity.
    #[inline]
    #[must_use]
    pub const fn id(&self) -> &ArtifactId {
        &self.id
    }

    /// The verdict on `run`: the first failing law at the tick it fails on (the cursor is the tick's ordinal), or [`Verdict::Pass`].
    #[must_use]
    pub fn judge(&self, run: &LaneRun) -> Verdict {
        let scenario = run.scenario();
        let origin = run.origin();
        let f = scenario.frame_interval_ns();
        let delta = u64::try_from(PROBE_DELTA.as_nanos()).unwrap_or(u64::MAX);
        let inputs = scenario.events();
        let ticks = run.ticks();
        let fault = run.fault();
        if let Some(fault) = fault
            && fault.ticks() == 0
        {
            return fail(
                0,
                LANE_SETTLES,
                format!("the lane stopped before its first tick: {fault}"),
            );
        }
        if run.calls().len() > LANE_MAX_EVENTS {
            return fail(
                ticks.len(),
                LANE_SETTLES,
                format!("{} calls, more than {LANE_MAX_EVENTS}", run.calls().len()),
            );
        }
        let mut g = MomentumGlow::default();
        let (mut out, mut under) = (Vec::new(), Vec::new());
        let (mut probe_out, mut probe_under) = (Vec::new(), Vec::new());
        let (mut drawn, mut probed) = (Enc::new(), Enc::new());
        let mut n = 0usize;
        let mut next_input = 0usize;
        let mut last_inert = true;
        for (i, call) in run.calls().iter().enumerate() {
            let (cur, at, geom, cfg) = match *call {
                MomentumEvent::Key { at, tau_bits } => {
                    g.on_key(instant(origin, offset(at)), f32::from_bits(tau_bits));
                    continue;
                }
                MomentumEvent::Reset => {
                    g.reset();
                    continue;
                }
                MomentumEvent::Query { .. } => continue,
                MomentumEvent::Tick { cur, at, geom, cfg } => (cur, offset(at), geom, cfg),
            };
            let cursor = n;
            let Some(tick) = ticks.get(n) else {
                return fail(
                    cursor,
                    LANE_SETTLES,
                    format!("call {i}, a tick at {at} ns, is not the run's tick {n}"),
                );
            };
            if tick.call != i || tick.at != at {
                return fail(
                    cursor,
                    LANE_SETTLES,
                    format!("call {i}, a tick at {at} ns, is not the run's tick {n}"),
                );
            }
            n += 1;
            let p = at;
            let (geom, cfg) = (geom.geom(), cfg.config());
            out.clear();
            under.clear();
            let frame = g.tick(cur, instant(origin, p), geom, &cfg, &mut out, &mut under);
            drawn.clear();
            encode_glass(&mut drawn, &frame, &out, &under);
            if frame != tick.frame
                || out.len() != tick.out
                || under.len() != tick.under
                || glass_key(drawn.as_bytes()) != tick.key
            {
                return fail(
                    cursor,
                    NO_STEP_SLEPT,
                    format!(
                        "the frame on glass from {p} ns (fp {:#x}, {} + {} quads) is not the one \
                         the first-party engine draws there (fp {:#x}, {} + {} quads)",
                        tick.frame.fp,
                        tick.out,
                        tick.under,
                        frame.fp,
                        out.len(),
                        under.len()
                    ),
                );
            }
            last_inert =
                frame == MomentumGlowFrame::default() && out.is_empty() && under.is_empty();
            if let After::Wake { wake: Some(w), .. } = tick.after
                && w <= p
            {
                return fail(
                    cursor,
                    LANE_SETTLES,
                    format!("the settle wake at {w} ns is not after the tick at {p} ns"),
                );
            }
            while let Some(&(t, _)) = inputs.get(next_input)
                && t <= p
            {
                next_input += 1;
            }
            let latest = if next_input > 0
                && let Some(&(t, _)) = inputs.get(next_input - 1)
            {
                t
            } else {
                0
            };
            if p.saturating_sub(latest) > HANG_NS {
                return fail(
                    cursor,
                    LANE_SETTLES,
                    format!(
                        "the tick at {p} ns comes more than {HANG_NS} ns after the latest input ({latest} ns)"
                    ),
                );
            }
            if let Some(fault) = fault
                && fault.ticks() == n
            {
                return fail(
                    cursor,
                    LANE_SETTLES,
                    format!("the lane stopped after the tick at {p} ns: {fault}"),
                );
            }
            let mut next = scenario.end_ns();
            if let Some(t) = ticks.get(n)
                && t.at < next
            {
                next = t.at;
            }
            if let Some(&(t, _)) = inputs.get(next_input)
                && t < next
            {
                next = t;
            }
            // An interval ending within a frame and δ of `p` is not judged: a probe clamped to `p + f` sees the step a shipped wake one margin later was armed for.
            let floor = p.saturating_add(f);
            // The drawn step only decays between lane events, so one probe δ before the next event sees any step slept through.
            if next.saturating_sub(delta) > floor {
                let s = next.saturating_sub(delta);
                let mut copy = g;
                probe_out.clear();
                probe_under.clear();
                let probe = copy.tick(
                    cur,
                    instant(origin, s),
                    geom,
                    &cfg,
                    &mut probe_out,
                    &mut probe_under,
                );
                probed.clear();
                encode_glass(&mut probed, &probe, &probe_out, &probe_under);
                if probed.as_bytes() != drawn.as_bytes() {
                    return fail(
                        cursor,
                        NO_STEP_SLEPT,
                        format!(
                            "the tick at {p} ns left its frame (fp {:#x}, {} quads) on glass until the next \
                             lane event at {next} ns, but a shipped tick at {s} ns draws another (fp {:#x}, {} \
                             quads): a step was slept through",
                            frame.fp,
                            out.len() + under.len(),
                            probe.fp,
                            probe_out.len() + probe_under.len()
                        ),
                    );
                }
            }
        }
        if n != ticks.len() {
            return fail(
                n,
                LANE_SETTLES,
                format!("{} ticks recorded for {n} tick calls", ticks.len()),
            );
        }
        if let Some(fault) = fault {
            return fail(n, LANE_SETTLES, format!("the lane stopped early: {fault}"));
        }
        if !last_inert {
            let (at, fp, quads) = match ticks.last() {
                Some(t) => (t.at, t.frame.fp, t.out + t.under),
                None => (0, 0, 0),
            };
            return fail(
                n.saturating_sub(1),
                CLEARED_BY_END,
                format!(
                    "the glass is lit at the end {} ns: the last tick, at {at} ns, drew fp {fp:#x} with {quads} quads",
                    scenario.end_ns(),
                ),
            );
        }
        Verdict::Pass
    }
}
