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
mod tests {
    use std::time::Duration;

    use aterm_effects::aos_momentum::corpus::scenario;
    use aterm_effects::aos_momentum::lane::{self, After, LaneRun};
    use aterm_effects::aos_momentum::m4::Evaluator;
    use aterm_effects::aos_momentum::plan::{Split, Wake};
    use aterm_effects::aos_momentum::{MOMENTUM_HOST_MAX_EVENTS, MomentumHost};

    use super::*;
    use crate::aos_search::LAB_BUILD;

    /// A key at `at_ms` ms, as an attached host records it.
    fn key(cursor: u64, at_ms: i128) -> Step {
        Step {
            cursor,
            input: MomentumEvent::Key {
                at: at_ms * 1_000_000,
                tau_bits: 1.2f32.to_bits(),
            },
            obs: MomentumObservation::Keyed,
        }
    }

    fn tick(cursor: u64, at_ms: i128, fp: u64, cur: (u16, u16)) -> Step {
        tick_ns(cursor, at_ms * 1_000_000, fp, cur)
    }

    /// A tick at `at` ns drawing the frame `fp` (inert when 0) with the caret at `cur`.
    fn tick_ns(cursor: u64, at: i128, fp: u64, cur: (u16, u16)) -> Step {
        let lane = aterm_effects::aos_momentum::lane::s0();
        let p = lane.start();
        Step {
            cursor,
            input: MomentumEvent::Tick {
                cur: Some(cur),
                at,
                geom: p.geom,
                cfg: p.cfg,
            },
            obs: MomentumObservation::Frame {
                fill: (fp != 0).then_some(0x00ff_00ff),
                hot: fp != 0,
                fp,
                out: Vec::new(),
                under: Vec::new(),
            },
        }
    }

    fn query(cursor: u64, at_ms: i128, deadline_ms: Option<i128>) -> Step {
        query_ns(
            cursor,
            at_ms * 1_000_000,
            16_666_667,
            deadline_ms.map(|d| d * 1_000_000),
        )
    }

    /// A query at `at` ns carrying the frame interval `frame_ns`, answered settling at `deadline` (ns), or not settling without one.
    fn query_ns(cursor: u64, at: i128, frame_ns: u64, deadline: Option<i128>) -> Step {
        Step {
            cursor,
            input: MomentumEvent::Query {
                at,
                frame_interval: Duration::from_nanos(frame_ns),
            },
            obs: MomentumObservation::Schedule {
                is_active: false,
                settling: deadline.is_some(),
                cold_at: Some(9_000_000_000),
                deadline,
            },
        }
    }

    /// A hand-made log: an earlier burst, then a phase of two keys, repeated and changed frames, settle answers and a clearing tick, then a capture's tick after it.
    fn log() -> Vec<Step> {
        vec![
            key(0, 0),
            tick(1, 10, 7, (1, 0)),
            tick(2, 20, 0, (1, 0)),
            key(3, 100),
            tick(4, 110, 5, (1, 1)),
            key(5, 160),
            tick(6, 170, 6, (1, 2)),
            tick(7, 180, 6, (1, 2)),
            query(8, 180, Some(230)),
            query(9, 200, Some(240)),
            tick(10, 243, 4, (1, 2)),
            query(11, 243, None),
            query(12, 250, Some(300)),
            tick(13, 290, 4, (1, 2)),
            query(14, 290, Some(320)),
            tick(15, 321, 0, (1, 2)),
            tick(16, 900, 0, (1, 2)),
        ]
    }

    /// A phase counts `T`, `P` and `Z` as the lane counts them, and pairs each settle answer with the next tick.
    #[test]
    fn a_phase_is_counted_as_the_lane_counts_and_its_answers_are_paired_with_the_next_tick() {
        let p = phase(&log(), 1).expect("a phase after one key");
        assert_eq!((p.first, p.last, p.cleared), (3, 15, true));
        assert_eq!((p.from_ns, p.to_ns), (100_000_000, 321_000_000));
        assert_eq!(
            (
                p.counts.keys,
                p.counts.ticks,
                p.counts.presented,
                p.counts.queries
            ),
            (2, 6, 4, 5)
        );
        assert_eq!(p.counts.wasted(), 2);
        assert_eq!(
            (p.ticks_after_keys, p.settle_wakes, p.refused_ticks),
            (5, 2, 0)
        );
        let ms = |v: &[i128]| v.iter().map(|ns| ns / 1_000_000).collect::<Vec<_>>();
        assert_eq!(ms(&p.rounding), [63 - 50, 3, -10, 1]);
        assert_eq!(ms(&p.earliest), [13, -10, 1]);
        assert_eq!(
            p.early,
            EarlyTicks {
                after_key: 0,
                moved: 0,
                other: 1,
                locked: 0
            }
        );
        assert_eq!(ms(&p.after_early), [21]);
        assert!(p.floor_period.is_empty() && p.query_lag.is_empty());
        assert_eq!(
            p.run,
            Run {
                ticks: 1,
                from_ns: 170_000_000,
                to_ns: 170_000_000
            }
        );
        assert_eq!(p.run.mean_period(), None);
        assert_eq!(p.frame_intervals, [(16_666_667, 5)]);
        assert_eq!(p.frame_interval(), Some(16_666_667));
        assert_eq!(p.unanswered, 0);

        // From the first key, the earlier burst's clearing tick presents (hot to inert).
        let all = phase(&log(), 0).expect("a phase from the first key");
        assert_eq!(
            (all.first, all.counts.ticks, all.counts.presented),
            (0, 8, 6)
        );
        assert_eq!(phase(&log(), 3), None);
        // A glow that never clears runs to the log's last input, its last answer unanswered.
        let mut open = log();
        open.truncate(15);
        let p = phase(&open, 1).expect("a phase");
        assert_eq!((p.last, p.cleared, p.unanswered), (14, false, 1));
    }

    /// A floor wake's period holds the query's lag, which the slot rounding `t − d` cannot see.
    #[test]
    fn a_floor_wakes_period_holds_the_querys_lag_which_the_rounding_cannot_see() {
        const F: u64 = 33_333_332;
        let f = i128::from(F);
        let (lag, late) = (1_500_000, 1_100_000);
        let mut steps = vec![key(0, 0)];
        let mut t = 100_000;
        steps.push(tick_ns(1, t, 1, (1, 1)));
        for k in 0..10u64 {
            let q = t + lag;
            steps.push(query_ns(2 + 2 * k, q, F, Some(q + f)));
            t = q + f + late;
            steps.push(tick_ns(3 + 2 * k, t, 2 + k, (1, 1)));
        }
        let q = t + lag;
        steps.push(query_ns(22, q, F, Some(q + 100_000_000)));
        steps.push(tick_ns(23, q + 100_000_000 + late, 0, (1, 1)));
        let p = phase(&steps, 0).expect("a phase");
        assert!(p.cleared);
        assert_eq!((p.counts.ticks, p.counts.presented), (12, 12));
        assert_eq!(p.rounding, vec![late; 11]);
        assert_eq!(p.earliest, p.rounding);
        assert_eq!(p.settle_wakes, 11);
        assert_eq!(p.early.total(), 0);
        assert_eq!(p.floor_period, vec![lag + f + late; 10]);
        assert_eq!(p.query_lag, vec![lag; 10]);
        assert_eq!(p.run.ticks, 11);
        assert_eq!(p.run.mean_period(), Some(lag + f + late));
        assert_eq!(p.frame_intervals, [(u128::from(F), 11)]);
        let lines = measured_lines(&p, None);
        for want in [
            "rounding all n=11 before_d=0 min_ms=1.100 p50_ms=1.100 p90_ms=1.100 max_ms=1.100 \
             unanswered=0",
            "period floor n=10 min_ms=35.933 p50_ms=35.933 p90_ms=35.933 max_ms=35.933 \
             frame_ms=33.333",
            "query_lag floor n=10 min_ms=1.500 p50_ms=1.500 p90_ms=1.500 max_ms=1.500",
            "run ticks=11 span_ms=359.333 mean_period_ms=35.933 frame_ms=33.333",
            "frame_interval 33333332ns=11",
        ] {
            assert!(lines.iter().any(|l| l == want), "{want}\n{lines:#?}");
        }
        assert!(lines[0].starts_with("phase T=12 P=12 Z=0 "), "{}", lines[0]);
        assert!(
            lines[0].ends_with(" settle_wakes=11 early_ticks=0 cleared=true span_ms=462.033"),
            "{}",
            lines[0]
        );
        assert!(measured_lines(&p, Some(12))[0].ends_with(" presents=12"));
    }

    /// A locked floor wake, an early tick one frame after the tick before with its floor answer pending, keeps the frame period and comes the query's lag before that answer, counted early but not `other` and measured by no `after_early`; a key or a caret move since the tick before takes precedence over it; a trace of floor wakes armed at the query's floor reads none.
    #[test]
    fn a_locked_floor_wake_keeps_the_frame_period() {
        const F: u64 = 33_333_332;
        let f = i128::from(F);
        let (lag, late) = (1_500_000, 1_160_000);
        let trace = |locked: bool, moved_at: Option<u64>, key_at: Option<u64>| {
            let mut steps = vec![key(0, 0)];
            let mut t = 100_000;
            steps.push(tick_ns(1, t, 1, (1, 1)));
            for k in 0..10u64 {
                let q = t + lag;
                steps.push(query_ns(2 + 3 * k, q, F, Some(q + f)));
                if key_at == Some(k) {
                    steps.push(key(3 + 3 * k, q / 1_000_000));
                }
                t = if locked { t + f } else { q + f + late };
                let cur = if moved_at.is_some_and(|m| k >= m) {
                    (1, 2)
                } else {
                    (1, 1)
                };
                steps.push(tick_ns(4 + 3 * k, t, 2 + k, cur));
            }
            let q = t + lag;
            steps.push(query_ns(32, q, F, Some(q + 100_000_000)));
            steps.push(tick_ns(33, q + 100_000_000 + late, 0, (1, 1)));
            phase(&steps, 0).expect("a phase")
        };
        let p = trace(true, None, None);
        assert!(p.cleared);
        assert_eq!((p.counts.ticks, p.counts.presented), (12, 12));
        assert_eq!(
            p.early,
            EarlyTicks {
                after_key: 0,
                moved: 0,
                other: 0,
                locked: 10
            }
        );
        assert_eq!(p.early.total(), 10);
        assert_eq!(p.period_locked, vec![f; 10]);
        assert_eq!(p.ahead_locked, vec![lag; 10]);
        assert!(p.after_early.is_empty(), "a locked tick takes no deadline");
        assert!(p.floor_period.is_empty() && p.query_lag.is_empty());
        assert_eq!(p.settle_wakes, 1);
        assert_eq!(p.run.ticks, 11);
        assert_eq!(p.run.mean_period(), Some(f));
        let lines = measured_lines(&p, None);
        for want in [
            "early_ticks after_key=0 moved=0 other=0",
            "period floor n=0 frame_ms=33.333",
            "period locked n=10 min_ms=33.333 p50_ms=33.333 p90_ms=33.333 max_ms=33.333 \
             frame_ms=33.333",
            "ahead locked n=10 min_ms=1.500 p50_ms=1.500 p90_ms=1.500 max_ms=1.500",
            "run ticks=11 span_ms=333.333 mean_period_ms=33.333 frame_ms=33.333",
        ] {
            assert!(lines.iter().any(|l| l == want), "{want}\n{lines:#?}");
        }
        assert!(
            lines[0].contains(" settle_wakes=1 early_ticks=10 cleared=true "),
            "{}",
            lines[0]
        );

        let moved = trace(true, Some(4), None);
        assert_eq!((moved.early.moved, moved.early.locked), (1, 9));
        assert_eq!(
            moved.after_early.len(),
            1,
            "a moved tick takes its deadline"
        );

        let keyed = trace(true, None, Some(4));
        assert_eq!((keyed.early.after_key, keyed.early.locked), (1, 9));
        assert_eq!(
            keyed.after_early.len(),
            1,
            "a keyed tick takes its deadline"
        );
        assert_eq!(keyed.period_locked.len(), 9);

        let unlocked = trace(false, None, None);
        assert_eq!(unlocked.early.total(), 0);
        assert_eq!(unlocked.floor_period, vec![lag + f + late; 10]);
        let lines = measured_lines(&unlocked, None);
        for want in ["period locked n=0 frame_ms=33.333", "ahead locked n=0"] {
            assert!(lines.iter().any(|l| l == want), "{want}\n{lines:#?}");
        }
    }

    /// A redraw within half a frame of the tick before neither counts in the run nor ends it, unless the phase carried no frame interval.
    #[test]
    fn a_redraw_within_half_a_frame_neither_counts_in_the_run_nor_ends_it() {
        const F: u64 = 33_333_332;
        let f = i128::from(F);
        let t0 = 500_000;
        let mut steps = vec![key(0, 0), tick_ns(1, t0, 1, (1, 1))];
        let mut t = t0;
        for k in 1..=4u64 {
            t = t0 + i128::from(k) * f;
            steps.push(tick_ns(1 + k, t, 1 + k, (1, 1)));
        }
        steps.push(tick_ns(6, t + 2_500_000, 5, (1, 1)));
        let q = t + 4_000_000;
        steps.push(query_ns(7, q, F, Some(q + f)));
        t = q + f + 1_200_000;
        let resumed = t;
        steps.push(tick_ns(8, t, 6, (1, 1)));
        for (cursor, fp) in [(9, 7), (10, 8)] {
            t += f;
            steps.push(tick_ns(cursor, t, fp, (1, 1)));
        }
        let last = t;
        steps.push(tick_ns(11, t + f, 8, (1, 1)));
        let q = t + f + 1_000_000;
        steps.push(query_ns(12, q, F, Some(q + 200_000_000)));
        steps.push(tick_ns(13, q + 201_000_000, 0, (1, 1)));
        let p = phase(&steps, 0).expect("a phase");
        assert_eq!((p.counts.ticks, p.counts.presented), (11, 9));
        assert_eq!(
            p.run,
            Run {
                ticks: 8,
                from_ns: t0,
                to_ns: last
            }
        );
        assert_eq!(p.run.mean_period(), Some((last - t0) / 7));
        assert_eq!(p.floor_period, [resumed - (t0 + 4 * f + 2_500_000)]);

        let mut no_query = steps.clone();
        no_query.retain(|s| !matches!(s.input, MomentumEvent::Query { .. }));
        let p = phase(&no_query, 0).expect("a phase");
        assert_eq!(p.frame_interval(), None);
        assert_eq!(p.run.ticks, 5);
    }

    /// Early ticks are counted by cause, and the tick after each is measured against the deadline it took.
    #[test]
    fn early_ticks_are_counted_by_cause_and_the_tick_after_each_is_measured() {
        let steps = vec![
            key(0, 0),
            tick(1, 1, 5, (0, 0)),
            query(2, 2, Some(40)),
            key(3, 20),
            tick(4, 20, 6, (0, 1)),
            query(5, 21, Some(60)),
            tick(6, 30, 7, (1, 0)),
            query(7, 31, Some(70)),
            tick(8, 35, 7, (1, 0)),
            tick(9, 71, 0, (1, 0)),
        ];
        let p = phase(&steps, 0).expect("a phase");
        assert_eq!(
            p.early,
            EarlyTicks {
                after_key: 1,
                moved: 1,
                other: 1,
                locked: 0
            }
        );
        assert_eq!(p.early.total(), 3);
        assert_eq!(p.settle_wakes, 0);
        let ms = |v: &[i128]| v.iter().map(|ns| ns / 1_000_000).collect::<Vec<_>>();
        assert_eq!(ms(&p.earliest), [-20, -30, -35]);
        assert_eq!(ms(&p.after_early), [-10, -25, 1]);
        assert_eq!(
            measured_lines(&p, None)[1],
            "early_ticks after_key=1 moved=1 other=1"
        );
    }

    /// The frame on glass before a phase is the last tick before it, so a first tick that repeats it is not presented.
    #[test]
    fn the_frame_on_glass_before_a_phase_is_the_last_tick_before_it() {
        let steps = vec![
            key(0, 0),
            tick(1, 10, 9, (0, 0)),
            key(2, 20),
            tick(3, 30, 9, (0, 0)),
            tick(4, 40, 0, (0, 0)),
        ];
        let p = phase(&steps, 1).expect("a phase");
        assert_eq!((p.counts.ticks, p.counts.presented), (2, 1));
    }

    /// The spread is nearest rank over the sorted values and counts those before `d`, and `ms` prints three decimals.
    #[test]
    fn the_rounding_spread_is_nearest_rank_and_counts_the_early_ticks() {
        assert_eq!(Spread::of(&[]), None);
        let s = Spread::of(&[5, -2, 9, 1, 3, 7, 0, 8, 4, 6]).expect("ten values");
        assert_eq!(
            s,
            Spread {
                n: 10,
                before: 1,
                min: -2,
                p50: 4,
                p90: 8,
                max: 9
            }
        );
        let one = Spread::of(&[1_234_567]).expect("one value");
        assert_eq!((one.p50, one.p90), (1_234_567, 1_234_567));
        assert_eq!(
            one.words(),
            "n=1 before_d=0 min_ms=1.234 p50_ms=1.234 p90_ms=1.234 max_ms=1.234"
        );
        assert_eq!(ms(-2_500_000), "-2.500");
        assert_eq!(ms(999), "0.000");
        assert_eq!(ms(16_666_667), "16.666");
    }

    /// The ticks around a move are the last before it, the first after it, and the first that carries the new cell.
    #[test]
    fn the_ticks_around_a_move_name_the_cells_they_carry() {
        let steps = log();
        let m = move_ticks(&steps, 175_000_000, (1, 2));
        assert_eq!(m.before, Some((170_000_000, Some((1, 2)))));
        assert_eq!(m.next, Some((180_000_000, Some((1, 2)))));
        assert_eq!(m.at_cell, Some(180_000_000));
        let m = move_ticks(&steps, 100_000_000, (1, 2));
        assert_eq!(m.next, Some((110_000_000, Some((1, 1)))));
        assert_eq!(m.at_cell, Some(170_000_000));
        assert_eq!(move_ticks(&steps, 1_000_000_000, (1, 2)).next, None);
    }

    /// The lane's slot rounding, in order: `t − w` for the tick after each tick that armed a settle wake at `w`.
    fn lane_rounding(run: &LaneRun) -> Vec<i128> {
        let ticks = run.ticks();
        ticks
            .iter()
            .zip(ticks.iter().skip(1))
            .filter_map(|(t, next)| match t.after {
                After::Wake { wake: Some(w), .. } => Some(i128::from(next.at) - i128::from(w)),
                _ => None,
            })
            .collect()
    }

    /// `ev` with its time moved `by` ns later: a lane's call (ns from the lane's origin) as the log records it (ns from the attach epoch).
    fn later(ev: MomentumEvent, by: i128) -> MomentumEvent {
        match ev {
            MomentumEvent::Key { at, tau_bits } => MomentumEvent::Key {
                at: at + by,
                tau_bits,
            },
            MomentumEvent::Tick { cur, at, geom, cfg } => MomentumEvent::Tick {
                cur,
                at: at + by,
                geom,
                cfg,
            },
            MomentumEvent::Query { at, frame_interval } => MomentumEvent::Query {
                at: at + by,
                frame_interval,
            },
            MomentumEvent::Reset => MomentumEvent::Reset,
        }
    }

    /// Asserts the trace `file` of `run` (started `lane_ns` after the attach epoch) replays to its head and counts as the lane counted it.
    fn assert_counts_as_the_lane(
        run: &LaneRun,
        lane_ns: i128,
        file: &[u8],
        plan: Option<SchedulePlan>,
        why: &str,
    ) {
        let replayed = replay_trace(file, LAB_BUILD, plan).unwrap_or_else(|e| panic!("{why}: {e}"));
        assert_eq!(
            replayed.steps.iter().map(|s| s.input).collect::<Vec<_>>(),
            run.calls()
                .iter()
                .map(|&c| later(c, lane_ns))
                .collect::<Vec<_>>(),
            "{why}: the log is the lane's calls"
        );
        let p = phase(&replayed.steps, 0).expect("the run typed");
        let c = run.counts();
        assert!(p.cleared, "{why}: the run cleared");
        assert_eq!(
            (
                p.counts.keys,
                p.counts.ticks,
                p.counts.presented,
                p.counts.wasted()
            ),
            (c.keys, c.ticks, c.presented, c.wasted()),
            "{why}: T, P and Z"
        );
        assert_eq!(p.counts.queries, c.queries, "{why}: queries");
        assert_eq!(p.counts.quads, c.quads, "{why}: quads");
        assert!(
            !p.rounding.is_empty(),
            "{why}: the lane woke a settling glow"
        );
        assert_eq!(
            p.rounding,
            lane_rounding(run),
            "{why}: each answer, the tick it woke"
        );
        assert_eq!(p.earliest, p.rounding, "{why}: the lane asks once per tick");
        assert!(
            p.rounding.iter().all(|&r| r <= 0),
            "{why}: a lane wakes at d, or an input beats it"
        );
        assert_eq!(p.early.locked, 0, "{why}: the lane cannot lock");
        assert!(
            p.period_locked.is_empty() && p.ahead_locked.is_empty(),
            "{why}: no locked tick is measured"
        );
    }

    /// A shipped trace replays to its head and counts as the lane counted, and a trace of another build or a cut file is refused.
    #[test]
    fn a_shipped_trace_replays_to_its_head_and_counts_as_the_lane_counted() {
        for seed in [6, 1] {
            let sc = scenario(seed).expect("a corpus scenario");
            let origin = Instant::now();
            let mut host = MomentumHost::default();
            host.aos_attach(origin, MomentumCode::Shipped, LAB_BUILD, 1);
            let run = lane::run(&mut host, &sc, origin);
            assert_eq!(run.fault(), None, "seed {seed}");
            assert!(
                run.calls().len() < MOMENTUM_HOST_MAX_EVENTS,
                "fixture: one log epoch"
            );
            let file = host.aos_trace().expect("attached").encode();
            assert_counts_as_the_lane(&run, 0, &file, None, &format!("seed {seed}"));
            let other = replay_trace(&file, "another build", None).expect_err("another build");
            assert!(
                other.contains("is none of build `another build`'s prebuilt codes"),
                "{other}"
            );
            let cut = replay_trace(&file[..file.len() - 1], LAB_BUILD, None).expect_err("cut");
            assert!(cut.starts_with("the trace file: "), "{cut}");
        }
    }

    /// A published plan's trace replays only when the run names that plan, and counts as the lane counted.
    #[test]
    fn a_published_plans_trace_replays_with_its_plan_and_counts_as_the_lane_counted() {
        let origin = Instant::now();
        let ev = Evaluator::v1(LAB_BUILD, origin).expect("the pinned policy");
        let winner =
            SchedulePlan::new(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep).expect("in range");
        let admitted = ev
            .admit_one(&winner.encode())
            .expect("the winner is admitted");
        let mut host = MomentumHost::default();
        host.aos_attach(origin, MomentumCode::Shipped, LAB_BUILD, 1);
        host.aos_pause(origin, Duration::from_nanos(16_666_667))
            .expect("a running glow pauses");
        host.aos_publish(admitted).expect("the winner publishes");
        let sc = scenario(1).expect("a corpus scenario");
        let run = lane::run(&mut host, &sc, origin + Duration::from_secs(1));
        assert_eq!(run.fault(), None);
        let file = host.aos_trace().expect("attached").encode();
        assert_counts_as_the_lane(
            &run,
            1_000_000_000,
            &file,
            Some(winner),
            "the published plan",
        );
        let unnamed = replay_trace(&file, LAB_BUILD, None).expect_err("no plan named");
        assert!(unnamed.contains("is none of build"), "{unnamed}");
        let cadence = SchedulePlan::new(Split::Shipped, Wake::Cadence).expect("in range");
        let other = replay_trace(&file, LAB_BUILD, Some(cadence)).expect_err("another plan");
        assert!(other.contains("nor the plan the run published"), "{other}");
    }

    /// `aos-phase` takes one trace, the window's build and the plan a publish run published; anything else is a usage error.
    #[test]
    fn aos_phase_takes_one_trace_the_windows_build_and_its_plan() {
        let words = |a: &[&str]| a.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        let usage = |a: &[&str]| {
            let f = parse_phase_args(&words(a)).expect_err("a usage error");
            assert_eq!(f.code, 2, "{:?}", f.msg);
            f.msg.unwrap_or_default()
        };
        assert_eq!(
            parse_phase_args(&words(&["t.trace", "--build", "0.1.0+gabc"])).expect("parses"),
            PhaseArgs {
                trace: "t.trace".into(),
                build: "0.1.0+gabc".into(),
                plan: None
            }
        );
        let winner =
            SchedulePlan::new(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep).expect("in range");
        assert_eq!(
            parse_phase_args(&words(&[
                "--plan",
                "010100000000000000000000",
                "--build",
                "b",
                "t.trace"
            ]))
            .expect("parses")
            .plan,
            Some(winner)
        );
        assert!(usage(&["t.trace"]).contains("--build BUILD is required"));
        assert!(usage(&["--build", "b"]).contains("name one TRACE"));
        assert!(usage(&["a", "b", "--build", "b"]).contains("unexpected 'a b'"));
        assert!(usage(&["t", "--keep", "--build", "b"]).contains("unexpected"));
        assert!(usage(&["t", "--build"]).contains("--build needs a value"));
        assert!(usage(&["t", "--build", "b", "--plan", "zz"]).contains("24 hex digits"));
        assert!(
            usage(&["t", "--build", "b", "--plan", "020100000000000000000000"])
                .starts_with("aos-phase: --plan 020100000000000000000000: ")
        );
    }

    /// `aos-phase` measures a kept trace as the drive does, a trace of another build exits 1, and a missing file is usage.
    #[test]
    fn a_kept_trace_is_measured_as_the_drive_measures_it() {
        let sc = scenario(5).expect("a corpus scenario of 24 keys");
        let origin = Instant::now();
        let mut host = MomentumHost::default();
        host.aos_attach(origin, MomentumCode::Shipped, LAB_BUILD, 1);
        let run = lane::run(&mut host, &sc, origin);
        assert_eq!(run.fault(), None);
        let file = host.aos_trace().expect("attached").encode();
        let replayed = replay_trace(&file, LAB_BUILD, None).expect("replays");
        let keys = keys_in(&replayed.steps);
        assert!(keys >= phase_keys(), "fixture: {keys} keys");
        let p = phase(&replayed.steps, keys - phase_keys()).expect("a phase");
        let a = PhaseArgs {
            trace: "t.trace".into(),
            build: LAB_BUILD.into(),
            plan: None,
        };
        let mut want = format!(
            "aos-phase file=t.trace bytes={} build={} plan=none\nMEASURED {}\n",
            file.len(),
            word(LAB_BUILD),
            replay_line(&replayed, &p)
        );
        for line in measured_lines(&p, None) {
            want.push_str(&format!("MEASURED {line}\n"));
        }
        want.push_str(&format!("aos-phase result=pass cleared={}\n", p.cleared));
        assert_eq!(phase_report(&file, "t.trace", &a).expect("measured"), want);
        assert!(!want.contains("presents="));
        assert_eq!(p.counts.keys, 12);

        let other = PhaseArgs {
            build: "another build".into(),
            ..a.clone()
        };
        let why = phase_report(&file, "t.trace", &other).expect_err("another build");
        assert!(why.contains("is none of build `another build`"), "{why}");

        let dir = aterm_tempfile::tempdir().expect("a scratch dir");
        let path = dir.path().join("kept.fresh.trace");
        std::fs::write(&path, &file).expect("written");
        let path = path.display().to_string();
        let args = |build: &str| {
            [path.as_str(), "--build", build]
                .iter()
                .map(|w| (*w).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(cmd_aos_phase(&args(LAB_BUILD)).expect("measured"), 0);
        let f = cmd_aos_phase(&args("another build")).expect_err("no replay");
        assert_eq!(f.code, 1, "{:?}", f.msg);
        let missing = dir.path().join("none.trace").display().to_string();
        let f =
            cmd_aos_phase(&[missing, "--build".into(), LAB_BUILD.into()]).expect_err("unreadable");
        assert_eq!(f.code, 2, "{:?}", f.msg);
    }
}
