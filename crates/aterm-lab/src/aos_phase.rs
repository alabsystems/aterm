// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A windowed run's momentum trace replayed in the lab to its recorded head, and one phase of it counted as the lane counts a run (design §10.8).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Instant;

use aterm_aos::ArtifactId;
use aterm_aos::effects::{EffectPort, NoEffects};
use aterm_aos::replay::replay;
use aterm_aos::task::{Code, EntryRefusal, StepRefusal};
use aterm_aos::wire::Enc;
use aterm_effects::aos_momentum::lane::{LaneCounts, encode_glass};
use aterm_effects::aos_momentum::plan::{PLAN_LEN, SchedulePlan};
use aterm_effects::aos_momentum::{
    Applied, MomentumCode, MomentumEvent, MomentumObservation, MomentumSchema, MomentumState,
    MomentumTrace, SNAPSHOT_VALID, Sched,
};
use aterm_effects::cursor_momentum::MomentumGlowFrame;

use crate::aos_search::{read_bounded, word};
use crate::instance::take_value;
use crate::paths::from_caller;
use crate::{Fail, Res};

/// One recorded input, its cursor, and the observation the window's code gave it in the lab's replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub cursor: u64,
    pub input: MomentumEvent,
    pub obs: MomentumObservation,
}

/// A trace replayed in the lab to the head the live run recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replayed {
    /// The artifact name of the code the trace's identity names.
    pub code: String,
    pub log_epoch: u64,
    /// The cursor of the log's first input.
    pub first_cursor: u64,
    /// Every input of the log, in order.
    pub steps: Vec<Step>,
}

/// The signed offset of `t` from `epoch` in ns, as the momentum schema writes every time.
fn offset_of(epoch: Instant, t: Instant) -> i128 {
    if let Some(after) = t.checked_duration_since(epoch) {
        i128::try_from(after.as_nanos()).unwrap_or(i128::MAX)
    } else {
        epoch.checked_duration_since(t).map_or(i128::MIN, |before| {
            i128::try_from(before.as_nanos()).map_or(i128::MIN, |n| -n)
        })
    }
}

/// The window's code under the artifact its trace names, each step `Sched::apply` observed as the live wrapper observes it (a mismatch breaks the chain, and the replay is refused).
struct Replaying {
    sched: Sched,
    id: ArtifactId,
}

impl Code<MomentumSchema> for Replaying {
    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn entry(&self, schema: &MomentumSchema, state: &MomentumState) -> Result<(), EntryRefusal> {
        state
            .glow()
            .snapshot(schema.epoch())
            .and_then(|s| s.validate())
            .map_err(|r| EntryRefusal {
                law: SNAPSHOT_VALID,
                detail: r.to_string(),
            })
    }

    fn step(
        &self,
        schema: &MomentumSchema,
        state: &mut MomentumState,
        input: &MomentumEvent,
        _port: &mut dyn EffectPort,
    ) -> Result<MomentumObservation, StepRefusal> {
        let epoch = schema.epoch();
        let (mut out, mut under) = (Vec::new(), Vec::new());
        Ok(
            match self.sched.apply(state, epoch, input, &mut out, &mut under) {
                Applied::Keyed => MomentumObservation::Keyed,
                Applied::WasReset => MomentumObservation::WasReset,
                Applied::Frame(f) => MomentumObservation::Frame {
                    fill: f.fill,
                    hot: f.hot,
                    fp: f.fp,
                    out,
                    under,
                },
                Applied::Schedule {
                    is_active,
                    settling,
                    cold_at,
                    deadline,
                } => MomentumObservation::Schedule {
                    is_active,
                    settling,
                    cold_at: cold_at.map(|t| offset_of(epoch, t)),
                    deadline: deadline.map(|t| offset_of(epoch, t)),
                },
                Applied::Refused(r) => MomentumObservation::Refused(r),
            },
        )
    }
}

/// The trace file `file` (a window's `aos trace hex`) decoded, its checkpoint restored and its log replayed through the code its identity names in build `build`: a prebuilt code, or `plan` when it names that plan.
///
/// # Errors
/// The trace does not decode, its log is broken, its code is none this lab can replay, the checkpoint does not restore, or the replay refused an input or missed the recorded head.
pub fn replay_trace(
    file: &[u8],
    build: &str,
    plan: Option<SchedulePlan>,
) -> Result<Replayed, String> {
    let trace = MomentumTrace::decode(file).map_err(|e| format!("the trace file: {e}"))?;
    let read = trace
        .read(Instant::now())
        .map_err(|e| format!("the trace: {e}"))?;
    if let Some(why) = read.log.broken() {
        return Err(format!("the log is broken ({why}): an input is missing"));
    }
    let named = &trace.identity.code;
    let sched = MomentumCode::ALL
        .iter()
        .find(|c| c.artifact(build) == *named)
        .map(|&c| Sched::Prebuilt(c))
        .or_else(|| {
            plan.filter(|p| p.artifact(build) == *named)
                .map(Sched::Plan)
        })
        .ok_or_else(|| {
            format!(
                "the trace's code `{}` is none of build `{build}`'s prebuilt codes{}",
                named.name,
                if plan.is_some() {
                    " nor the plan the run published"
                } else {
                    ""
                }
            )
        })?;
    let start = read
        .checkpoint
        .restore(&read.schema, &trace.identity)
        .map_err(|e| format!("the checkpoint does not restore: {e}"))?;
    let first_cursor = read.log.header().first_cursor;
    let code = Replaying {
        sched,
        id: named.clone(),
    };
    let run = replay(
        &read.schema,
        &code,
        start,
        read.log.inputs(),
        first_cursor,
        &mut NoEffects,
    );
    if let Some((cursor, refusal)) = run.refused {
        return Err(format!(
            "the replay refused the input at cursor {cursor}: {refusal}"
        ));
    }
    if run.state_digest != trace.head_state {
        return Err(format!(
            "the replay reached the state {}, not the recorded head's {}",
            run.state_digest.short(),
            trace.head_state.short()
        ));
    }
    if run.observation_chain != trace.head_chain {
        return Err(format!(
            "the replay observed the chain {}, not the recorded head's {}",
            run.observation_chain.short(),
            trace.head_chain.short()
        ));
    }
    let steps = read
        .log
        .inputs()
        .iter()
        .zip(run.observations)
        .map(|(input, (cursor, obs))| Step {
            cursor,
            input: *input,
            obs,
        })
        .collect();
    Ok(Replayed {
        code: named.name.clone(),
        log_epoch: read.log.header().log_epoch,
        first_cursor,
        steps,
    })
}

/// A distribution of `t − d` in ns: how many, how many fell before `d`, and the minimum, median, 90th percentile (nearest rank) and maximum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spread {
    pub n: usize,
    pub before: usize,
    pub min: i128,
    pub p50: i128,
    pub p90: i128,
    pub max: i128,
}

/// The nearest-rank `num/den` quantile of the sorted, non-empty `v`: the smallest value with at least that share of the values at or below it.
fn rank(v: &[i128], num: usize, den: usize) -> i128 {
    let k = (v.len().saturating_mul(num)).div_ceil(den).max(1);
    v[k.min(v.len()) - 1]
}

impl Spread {
    /// The spread of `values`; `None` when there are none.
    #[must_use]
    pub fn of(values: &[i128]) -> Option<Spread> {
        let mut v = values.to_vec();
        v.sort_unstable();
        let (&min, &max) = (v.first()?, v.last()?);
        Some(Spread {
            n: v.len(),
            before: v.iter().filter(|&&x| x < 0).count(),
            min,
            p50: rank(&v, 1, 2),
            p90: rank(&v, 9, 10),
            max,
        })
    }

    /// `n= before_d= min_ms= p50_ms= p90_ms= max_ms=` (µs resolution).
    #[must_use]
    pub fn words(&self) -> String {
        format!(
            "n={} before_d={} min_ms={} p50_ms={} p90_ms={} max_ms={}",
            self.n,
            self.before,
            ms(self.min),
            ms(self.p50),
            ms(self.p90),
            ms(self.max)
        )
    }
}

/// `ns` as milliseconds with three decimals (µs resolution, truncated toward zero).
#[must_use]
pub fn ms(ns: i128) -> String {
    let sign = if ns < 0 { "-" } else { "" };
    let us = ns.unsigned_abs() / 1_000;
    format!("{sign}{}.{:03}", us / 1_000, us % 1_000)
}

/// The ticks drawn before the earliest settle deadline pending at them, by cause: a key since the tick before, else a moved caret, else a floor answer as that deadline (design §10.10's class for a plan floor lock's tick), else none of these.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EarlyTicks {
    pub after_key: u64,
    pub moved: u64,
    pub other: u64,
    pub locked: u64,
}

impl EarlyTicks {
    /// Every early tick.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.after_key + self.moved + self.other + self.locked
    }
}

/// The stretch after the last key over which the glow changed at every tick and no tick's earliest pending answer lay past the floor, passing over a redraw that repeats the frame within half a frame interval of the tick before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub ticks: u64,
    /// The times of its first and last tick, ns from the recording epoch.
    pub from_ns: i128,
    pub to_ns: i128,
}

impl Run {
    /// The mean period between its ticks in ns; `None` under two ticks.
    #[must_use]
    pub fn mean_period(&self) -> Option<i128> {
        (self.ticks >= 2).then(|| (self.to_ns - self.from_ns) / i128::from(self.ticks - 1))
    }
}

/// One phase of a replayed trace, counted as the lane counts a run (replicated here, since the lane's own counter lives inside its closed loop).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Phase {
    /// The cursors of its first input (its first key) and its last (the clearing tick, or the log's last input).
    pub first: u64,
    pub last: u64,
    /// The times of its first key and of its last input, ns from the recording epoch.
    pub from_ns: i128,
    pub to_ns: i128,
    /// Whether it ended on a clearing tick.
    pub cleared: bool,
    /// Keys, resets, `T` (ticks), `P` (presented), queries and quads in the lane's counts; its parks and strandings stay 0.
    pub counts: LaneCounts,
    /// Ticks admission refused: they drew nothing and are not in `T`.
    pub refused_ticks: u64,
    /// Ticks after the phase's last key.
    pub ticks_after_keys: u64,
    /// Ticks at or after the earliest settle deadline pending at them: a lower bound on the ticks a settle wake fired.
    pub settle_wakes: u64,
    /// Ticks before the earliest settle deadline pending at them, by cause.
    pub early: EarlyTicks,
    /// `t − d` (ns) for every settle answer, in order, against the next tick.
    pub rounding: Vec<i128>,
    /// `t − d` (ns) for the earliest settle answer pending at each tick, early ticks included.
    pub earliest: Vec<i128>,
    /// `t − d` (ns) for the tick right after each early tick but a locked one, `d` the earliest deadline the early tick took.
    pub after_early: Vec<i128>,
    /// For each settle wake whose earliest answer was the floor (`d = q + frame_interval`), its period `t − t'` (ns) since the tick before, which `t − d` cannot show since the floor is re-anchored at every query.
    pub floor_period: Vec<i128>,
    /// For the same wakes, in order: `q − t'` (ns), the query's lag behind the tick before.
    pub query_lag: Vec<i128>,
    /// For each locked early tick, its period `t − t'` (ns) since the tick before.
    pub period_locked: Vec<i128>,
    /// For each locked early tick, `d − t` (ns): how far before its floor answer it came.
    pub ahead_locked: Vec<i128>,
    /// The stretch after the last key over which the glow changed at every tick.
    pub run: Run,
    /// Every frame interval a query of the phase carried (ns), with how many carried it, ascending.
    pub frame_intervals: Vec<(u128, u64)>,
    /// Settle answers no later tick of the phase followed.
    pub unanswered: u64,
}

impl Phase {
    /// The frame interval every query of the phase carried (ns); `None` when they carried none or more than one.
    #[must_use]
    pub fn frame_interval(&self) -> Option<u128> {
        match self.frame_intervals.as_slice() {
            [(ns, _)] => Some(*ns),
            _ => None,
        }
    }
}

/// A tick after the phase's last key as [`Run`] reads it: its time, the tick before's, whether it presented, and whether no answer later than the floor was the earliest pending at it.
#[derive(Clone, Copy, Debug)]
struct AfterKeys {
    at: i128,
    before: Option<i128>,
    presented: bool,
    floor: bool,
}

/// The [`Run`] of the ticks after the phase's last key, a redraw being one that repeats the frame less than half of `frame` (ns) after the tick before.
fn run_of(ticks: &[AfterKeys], frame: Option<u128>) -> Run {
    let half = frame.and_then(|f| i128::try_from(f / 2).ok());
    let mut run = Run::default();
    for t in ticks {
        let redraw =
            !t.presented && half.is_some_and(|h| t.before.is_some_and(|before| t.at - before < h));
        if redraw {
            continue;
        }
        if !(t.presented && t.floor) {
            break;
        }
        if run.ticks == 0 {
            run.from_ns = t.at;
        }
        run.ticks += 1;
        run.to_ns = t.at;
    }
    run
}

/// A settle answer pending at the next tick: its deadline, whether that is its query's floor, and the query's time.
#[derive(Clone, Copy, Debug)]
struct Answer {
    d: i128,
    floor: bool,
    q: i128,
}

/// An input's time, if it has one.
fn time_of(ev: &MomentumEvent) -> Option<i128> {
    match *ev {
        MomentumEvent::Key { at, .. }
        | MomentumEvent::Tick { at, .. }
        | MomentumEvent::Query { at, .. } => Some(at),
        MomentumEvent::Reset => None,
    }
}

/// A frame observation's glass encoding (`lane::encode_glass`); `None` for any other.
fn glass_of(obs: &MomentumObservation) -> Option<Vec<u8>> {
    let MomentumObservation::Frame {
        fill,
        hot,
        fp,
        out,
        under,
    } = obs
    else {
        return None;
    };
    let mut e = Enc::new();
    let frame = MomentumGlowFrame {
        fill: *fill,
        hot: *hot,
        fp: *fp,
    };
    encode_glass(&mut e, &frame, out, under);
    Some(e.finish())
}

/// The inert frame: nothing drawn and not hot.
fn inert(obs: &MomentumObservation) -> bool {
    matches!(
        obs,
        MomentumObservation::Frame { fill: None, hot: false, fp: 0, out, under }
            if out.is_empty() && under.is_empty()
    )
}

/// A tick step's time; `None` for any other step.
fn tick_of(s: &Step) -> Option<i128> {
    match s.input {
        MomentumEvent::Tick { at, .. } => Some(at),
        _ => None,
    }
}

/// A drawn tick's time and caret cell; `None` for any other step, and for a refused tick (it drew nothing).
fn drawn_tick(s: &Step) -> Option<(i128, Option<(u16, u16)>)> {
    match s.input {
        MomentumEvent::Tick { at, cur, .. } if glass_of(&s.obs).is_some() => Some((at, cur)),
        _ => None,
    }
}

/// The phase of `steps` from the key after the first `prior` keys to the first inert tick after the last key, counted; `None` when there are no more than `prior` keys.
#[must_use]
pub fn phase(steps: &[Step], prior: usize) -> Option<Phase> {
    let keys: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s.input, MomentumEvent::Key { .. }))
        .map(|(i, _)| i)
        .collect();
    let (&start, &last_key) = (keys.get(prior)?, keys.last()?);
    let clearing = steps
        .iter()
        .enumerate()
        .skip(last_key)
        .find(|(_, s)| tick_of(s).is_some() && inert(&s.obs))
        .map(|(i, _)| i);
    let (end, cleared) = clearing.map_or((steps.len() - 1, false), |i| (i, true));
    let mut glass = steps[..start]
        .iter()
        .rev()
        .filter(|s| tick_of(s).is_some())
        .find_map(|s| glass_of(&s.obs))
        .unwrap_or_else(|| {
            let mut e = Enc::new();
            encode_glass(&mut e, &MomentumGlowFrame::default(), &[], &[]);
            e.finish()
        });
    let mut p = Phase {
        first: steps[start].cursor,
        last: steps[end].cursor,
        from_ns: time_of(&steps[start].input).unwrap_or(0),
        to_ns: steps[..=end]
            .iter()
            .rev()
            .find_map(|s| time_of(&s.input))
            .unwrap_or(0),
        cleared,
        ..Phase::default()
    };
    let mut before = steps[..start].iter().rev().find_map(drawn_tick);
    let (mut keyed, mut took) = (false, None::<i128>);
    let mut after_keys: Vec<AfterKeys> = Vec::new();
    let mut intervals: BTreeMap<u128, u64> = BTreeMap::new();
    let mut pending: Vec<Answer> = Vec::new();
    for (i, s) in steps.iter().enumerate().take(end + 1).skip(start) {
        match s.input {
            MomentumEvent::Key { .. } => {
                p.counts.keys += 1;
                keyed = true;
            }
            MomentumEvent::Reset => p.counts.resets += 1,
            MomentumEvent::Query {
                at, frame_interval, ..
            } => {
                p.counts.queries += 1;
                *intervals.entry(frame_interval.as_nanos()).or_default() += 1;
                if let MomentumObservation::Schedule {
                    settling: true,
                    deadline: Some(d),
                    ..
                } = s.obs
                {
                    let floor = i128::try_from(frame_interval.as_nanos())
                        .is_ok_and(|f| at.checked_add(f) == Some(d));
                    pending.push(Answer { d, floor, q: at });
                }
            }
            MomentumEvent::Tick { at, cur, .. } => {
                let Some(drawn) = glass_of(&s.obs) else {
                    p.refused_ticks += 1;
                    continue;
                };
                p.counts.ticks += 1;
                if let MomentumObservation::Frame { out, under, .. } = &s.obs {
                    let quads =
                        u64::try_from(out.len().saturating_add(under.len())).unwrap_or(u64::MAX);
                    p.counts.quads = p.counts.quads.saturating_add(quads);
                    p.counts.max_quads = p.counts.max_quads.max(quads);
                }
                let presented = drawn != glass;
                if presented {
                    p.counts.presented += 1;
                    glass = drawn;
                }
                if let Some(d) = took.take() {
                    p.after_early.push(at - d);
                }
                let earliest = pending.iter().min_by_key(|a| a.d).copied();
                if i > last_key {
                    p.ticks_after_keys += 1;
                    after_keys.push(AfterKeys {
                        at,
                        before: before.map(|(t, _)| t),
                        presented,
                        floor: earliest.is_none_or(|a| a.floor),
                    });
                }
                if let Some(a) = earliest {
                    p.earliest.push(at - a.d);
                    if at >= a.d {
                        p.settle_wakes += 1;
                        if let (true, Some((prev, _))) = (a.floor, before) {
                            p.floor_period.push(at - prev);
                            p.query_lag.push(a.q - prev);
                        }
                    } else if keyed {
                        took = Some(a.d);
                        p.early.after_key += 1;
                    } else if before.is_some_and(|(_, c)| c != cur) {
                        took = Some(a.d);
                        p.early.moved += 1;
                    } else if a.floor {
                        p.early.locked += 1;
                        p.ahead_locked.push(a.d - at);
                        if let Some((prev, _)) = before {
                            p.period_locked.push(at - prev);
                        }
                    } else {
                        took = Some(a.d);
                        p.early.other += 1;
                    }
                }
                p.rounding.extend(pending.drain(..).map(|a| at - a.d));
                before = Some((at, cur));
                keyed = false;
            }
        }
    }
    p.frame_intervals = intervals.into_iter().collect();
    p.run = run_of(&after_keys, p.frame_interval());
    p.unanswered = u64::try_from(pending.len()).unwrap_or(u64::MAX);
    Some(p)
}

/// `n= min_ms= p50_ms= p90_ms= max_ms=` of `values` (µs resolution), or `n=0`.
fn values_words(values: &[i128]) -> String {
    Spread::of(values).map_or_else(
        || "n=0".to_string(),
        |s| {
            format!(
                "n={} min_ms={} p50_ms={} p90_ms={} max_ms={}",
                s.n,
                ms(s.min),
                ms(s.p50),
                ms(s.p90),
                ms(s.max)
            )
        },
    )
}

/// The phase's `MEASURED` figures (without the prefix, none a gate) as `aos-repair` and `aos-phase` print them, `presents` being what only a live run reads.
#[must_use]
pub fn measured_lines(p: &Phase, presents: Option<u64>) -> Vec<String> {
    let c = &p.counts;
    let frame = p.frame_interval().map_or_else(
        || "mixed".to_string(),
        |ns| ms(i128::try_from(ns).unwrap_or(i128::MAX)),
    );
    let mut lines = vec![format!(
        "phase T={} P={} Z={} keys={} resets={} queries={} quads={} max_quads={} \
         refused_ticks={} ticks_after_last_key={} settle_wakes={} early_ticks={} cleared={} \
         span_ms={}{}",
        c.ticks,
        c.presented,
        c.wasted(),
        c.keys,
        c.resets,
        c.queries,
        c.quads,
        c.max_quads,
        p.refused_ticks,
        p.ticks_after_keys,
        p.settle_wakes,
        p.early.total(),
        p.cleared,
        ms(p.to_ns - p.from_ns),
        presents.map_or_else(String::new, |n| format!(" presents={n}"))
    )];
    lines.push(format!(
        "early_ticks after_key={} moved={} other={}",
        p.early.after_key, p.early.moved, p.early.other
    ));
    for (label, v) in [
        ("all", &p.rounding),
        ("earliest", &p.earliest),
        ("after_early", &p.after_early),
    ] {
        let words = Spread::of(v).map_or_else(|| "n=0".to_string(), |s| s.words());
        lines.push(format!(
            "rounding {label} {words} unanswered={}",
            p.unanswered
        ));
    }
    lines.push(format!(
        "period floor {} frame_ms={frame}",
        values_words(&p.floor_period)
    ));
    lines.push(format!("query_lag floor {}", values_words(&p.query_lag)));
    lines.push(format!(
        "period locked {} frame_ms={frame}",
        values_words(&p.period_locked)
    ));
    lines.push(format!("ahead locked {}", values_words(&p.ahead_locked)));
    lines.push(format!(
        "run ticks={} span_ms={} mean_period_ms={} frame_ms={frame}",
        p.run.ticks,
        ms(p.run.to_ns - p.run.from_ns),
        p.run.mean_period().map_or_else(|| "none".to_string(), ms)
    ));
    lines.push(if p.frame_intervals.is_empty() {
        "frame_interval none".to_string()
    } else {
        let words: Vec<String> = p
            .frame_intervals
            .iter()
            .map(|(ns, n)| format!("{ns}ns={n}"))
            .collect();
        format!("frame_interval {}", words.join(" "))
    });
    lines
}

/// What the log shows around a caret moved no earlier than `from_ns`: the last tick before it, the first at or after it, and the first of those that carries `cell`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MoveTicks {
    /// The last tick before `from_ns`: its time and its caret cell.
    pub before: Option<(i128, Option<(u16, u16)>)>,
    /// The first tick at or after `from_ns`.
    pub next: Option<(i128, Option<(u16, u16)>)>,
    /// The time of the first tick at or after `from_ns` whose caret cell is `cell`.
    pub at_cell: Option<i128>,
}

/// [`MoveTicks`] of `steps` for a move no earlier than `from_ns` to `cell`.
#[must_use]
pub fn move_ticks(steps: &[Step], from_ns: i128, cell: (u16, u16)) -> MoveTicks {
    let ticks = steps.iter().filter_map(|s| match s.input {
        MomentumEvent::Tick { cur, at, .. } => Some((at, cur)),
        _ => None,
    });
    let mut m = MoveTicks::default();
    for (at, cur) in ticks {
        if at < from_ns {
            m.before = Some((at, cur));
            continue;
        }
        if m.next.is_none() {
            m.next = Some((at, cur));
        }
        if cur == Some(cell) {
            m.at_cell = Some(at);
            break;
        }
    }
    m
}

/// Whether the first tick at or after `barrier_ns` carries `cell`, which proves that the first present booked after a main-loop read asked no earlier than `barrier_ns` showed the caret moved to `cell` after that read.
///
/// The main loop answers between passes, so every present composed before the reply is booked by then; each present after it is composed at a glow tick (the hosted glow logs one per frame) from `barrier_ns` on, and with no further move every tick after one that carries the moved cell carries it too. `false` proves nothing: the first tick may be one composed before the echo reached the grid, whose present can be the first after the read, or one composed before the reply, already booked by it.
#[must_use]
pub fn first_present_carries(steps: &[Step], barrier_ns: i128, cell: (u16, u16)) -> bool {
    move_ticks(steps, barrier_ns, cell)
        .next
        .is_some_and(|(_, cur)| cur == Some(cell))
}

/// The keys of the log a measured phase holds: the drive's fresh keys, the log's last.
#[must_use]
pub fn phase_keys() -> usize {
    crate::aos_repair::FRESH_KEYS.chars().count()
}

/// The keys `steps` holds.
#[must_use]
pub fn keys_in(steps: &[Step]) -> usize {
    steps
        .iter()
        .filter(|s| matches!(s.input, MomentumEvent::Key { .. }))
        .count()
}

/// The replay's `MEASURED` figure (without the prefix): the code, the log's epoch and first cursor, its inputs and keys, and the phase's cursors.
#[must_use]
pub fn replay_line(r: &Replayed, p: &Phase) -> String {
    format!(
        "replay code={} log_epoch={} first_cursor={} inputs={} keys_in_log={} \
         phase_cursors={}..={}",
        r.code,
        r.log_epoch,
        r.first_cursor,
        r.steps.len(),
        keys_in(&r.steps),
        p.first,
        p.last
    )
}

/// The most bytes `aos-phase` reads from its trace file, far above any trace a window exports (at most `MOMENTUM_HOST_MAX_EVENTS` inputs and one checkpoint).
pub const MAX_TRACE_BYTES: u64 = 16 << 20;

/// The verb's own help (`aos-phase --help`).
pub const HELP: &str = "\
usage: targo --unverified lab aos-phase TRACE --build BUILD [--plan PLAN_HEX]

Opens nothing. A trace file a windowed `aos-repair --control` or `--publish` run kept (<stem>.fresh.trace, the
window's `aos trace hex`) replayed in the lab to the head the window recorded, through the code its identity names in
the build BUILD (the run's `build=`, in its measured.txt), and the phase of the log's last 12 keys measured as the run
measured it: the same MEASURED lines but presents and lag, which only a live run reads.

  --build BUILD     the window's build, e.g. 0.114.0+gfb88da050159 (required)
  --plan PLAN_HEX   the plan the run published (24 hex digits), for a --publish run's trace

exit: 0 the trace replayed and its phase was measured; 1 it did not replay to its head, or holds no such phase; 2 usage,
or TRACE cannot be read
";

/// `aos-phase`'s arguments: the trace file, the window's build and the plan it published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhaseArgs {
    pub trace: String,
    pub build: String,
    pub plan: Option<SchedulePlan>,
}

/// Parse `aos-phase`'s arguments (`--help` aside).
///
/// # Errors
/// Usage (2): no trace or more than one, no `--build`, a flag without its value, or a plan that is not 24 hex digits or does not decode.
pub fn parse_phase_args(args: &[String]) -> Result<PhaseArgs, Fail> {
    let mut a = args.to_vec();
    let build = take_value(&mut a, "--build")?
        .ok_or_else(|| Fail::usage("aos-phase: --build BUILD is required (aos-phase --help)"))?;
    let plan = take_value(&mut a, "--plan")?
        .map(|hex| {
            let mut bytes = [0u8; PLAN_LEN];
            if !aterm_codec::hex::decode_into(hex.as_bytes(), &mut bytes) {
                return Err(Fail::usage(format!(
                    "aos-phase: --plan takes a plan's {PLAN_LEN} bytes as {} hex digits, not \
                     '{hex}'",
                    PLAN_LEN * 2
                )));
            }
            SchedulePlan::decode(&bytes)
                .map_err(|e| Fail::usage(format!("aos-phase: --plan {hex}: {e}")))
        })
        .transpose()?;
    let trace = match a.as_slice() {
        [one] if !one.starts_with("--") => one.clone(),
        [] => return Err(Fail::usage("aos-phase: name one TRACE (aos-phase --help)")),
        [_, ..] => {
            return Err(Fail::usage(format!(
                "aos-phase: unexpected '{}' (aos-phase --help)",
                a.join(" ")
            )));
        }
    };
    Ok(PhaseArgs { trace, build, plan })
}

/// The report of `aos-phase` on the trace `file` (read from `name`): a header, the `MEASURED` lines of the phase of its last [`phase_keys`] keys, then `aos-phase result=pass`.
///
/// # Errors
/// The trace did not replay to its head, or it holds fewer keys than the phase's.
pub fn phase_report(file: &[u8], name: &str, a: &PhaseArgs) -> Result<String, String> {
    let replayed = replay_trace(file, &a.build, a.plan)?;
    let keys = keys_in(&replayed.steps);
    let p = phase(&replayed.steps, keys.saturating_sub(phase_keys()))
        .filter(|_| keys >= phase_keys())
        .ok_or_else(|| format!("no phase: the log holds {keys} keys"))?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "aos-phase file={} bytes={} build={} plan={}",
        word(name),
        file.len(),
        word(&a.build),
        a.plan.map_or_else(
            || "none".to_string(),
            |p| aterm_codec::hex::encode_lower(&p.encode())
        )
    );
    for line in std::iter::once(replay_line(&replayed, &p)).chain(measured_lines(&p, None)) {
        let _ = writeln!(out, "MEASURED {line}");
    }
    let _ = writeln!(out, "aos-phase result=pass cleared={}", p.cleared);
    Ok(out)
}

/// `aos-phase`: replays a kept trace to its head and prints its phase's `MEASURED` lines.
///
/// # Errors
/// Usage (2), or the trace did not replay or holds no phase (1).
pub fn cmd_aos_phase(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    let a = parse_phase_args(args)?;
    let path = from_caller(&a.trace);
    let file = read_bounded(&path, MAX_TRACE_BYTES)
        .map_err(|why| Fail::usage(format!("aos-phase: {path}: {why}")))?;
    let report =
        phase_report(&file, &path, &a).map_err(|why| Fail::die(format!("aos-phase: {why}")))?;
    print!("{report}");
    Ok(0)
}

#[cfg(test)]
#[path = "aos_phase_tests.rs"]
mod tests;
