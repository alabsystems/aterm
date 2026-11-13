// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `compare` lane: paired A/B fresh-process startup of two aterm binaries,
//! scheduled AB/BA and reported by round.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use aterm_json::{Value, json};

use super::start::{ATTACH, Frozen, Launcher, Laws, Observation, PHASES, Sample, freeze};
use super::{Args, Halt, Scratch, sha256_file};
use crate::perf::LaneRun;
use crate::perf::stats::{self, Rounds};
use crate::perf::trend::utc_timestamp;

/// One arm of the comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Arm {
    Baseline,
    Candidate,
}

impl Arm {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Arm::Baseline => "baseline",
            Arm::Candidate => "candidate",
        }
    }
}

/// The launch side of a run: one trial of an arm, and the cooldown between
/// trials. Real launches open windows; a test injects a deterministic bench.
pub(crate) trait Bench {
    fn trial(&mut self, arm: Arm, warm: bool) -> Result<Observation, String>;
    fn cool(&mut self, seconds: f64);
}

/// The automated rule-zero minimums: at least this many pairs, at least this
/// cooldown between trials.
pub(crate) const MIN_PAIRS: u32 = 20;
pub(crate) const MIN_COOLDOWN_S: f64 = 5.0;

/// What a run is asked to do.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    pub pairs: u32,
    pub laws: Laws,
    pub cooldown_s: f64,
    pub warm: bool,
    pub chip: String,
    pub negative_control: bool,
}

/// The pmset reading's class: only the exact clean triple is clean, and any
/// warning or unrecognised shape fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Thermal {
    Clean,
    Unavailable,
    WarningOrUnrecognized,
}

pub(crate) fn classify_thermal(text: &str) -> Thermal {
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if normalized.is_empty() || normalized == "unavailable" {
        return Thermal::Unavailable;
    }
    let clean = [
        "note: no thermal warning level has been recorded",
        "note: no performance warning level has been recorded",
        "note: no cpu power status has been recorded",
    ];
    let mut observed: Vec<&str> = normalized.split('|').map(str::trim).collect();
    observed.sort_unstable();
    let mut want = clean.to_vec();
    want.sort_unstable();
    if observed == want {
        Thermal::Clean
    } else {
        Thermal::WarningOrUnrecognized
    }
}

/// One validated trial.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    pub pair: u32,
    pub slot: u8,
    pub arm: Arm,
    pub sample: Sample,
    pub thermal: Thermal,
}

/// One compared metric: each arm's values (in pair order) and the
/// candidate-faster deltas.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Compared {
    pub name: String,
    pub baseline: Vec<f64>,
    pub candidate: Vec<f64>,
    pub deltas: Vec<f64>,
    pub rounds: Option<Rounds>,
}

/// Which rule-zero inputs the automation can vouch for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Eligibility {
    pub hardware: bool,
    pub sample_count: bool,
    pub thermal_available: bool,
    pub thermal_no_warning: bool,
    pub cooldown: bool,
    pub negative_control: bool,
    pub thermal_counts: BTreeMap<Thermal, usize>,
}

impl Eligibility {
    pub(crate) const fn thermal(&self) -> bool {
        self.thermal_available && self.thermal_no_warning
    }

    pub(crate) const fn all(&self) -> bool {
        self.hardware
            && self.sample_count
            && self.thermal()
            && self.cooldown
            && !self.negative_control
    }

    pub(crate) fn reasons(&self) -> Vec<&'static str> {
        let mut r = Vec::new();
        if !self.hardware {
            r.push("hardware is not M4 Max");
        }
        if !self.sample_count {
            r.push("fewer than 20 pairs");
        }
        if !self.thermal_available {
            r.push("a thermal observation is unavailable");
        }
        if !self.thermal_no_warning {
            r.push("a thermal warning or unrecognised thermal state was observed");
        }
        if !self.cooldown {
            r.push("cooldown is below the 5-second evidence minimum");
        }
        if self.negative_control {
            r.push("identical-artifact negative control");
        }
        r
    }
}

/// A finished comparison.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Comparison {
    pub plan: Plan,
    pub records: Vec<Record>,
    pub rounds: u32,
    pub trailing_unbalanced_pair: bool,
    /// Router, phases or attach missing from an arm: only the shared totals
    /// are compared.
    pub legacy_totals_only: bool,
    pub router_by_arm: BTreeMap<Arm, bool>,
    pub visible_by_arm: BTreeMap<Arm, bool>,
    pub phases_by_arm: BTreeMap<Arm, bool>,
    pub attach_by_arm: BTreeMap<Arm, bool>,
    pub backend: String,
    /// The headline totals, in report order.
    pub totals: Vec<Compared>,
    pub phases: Vec<Compared>,
    pub attach: Vec<Compared>,
    pub eligibility: Eligibility,
}

/// Run the plan: odd pairs baseline-then-candidate, even pairs the reverse,
/// a cooldown before every trial but the first, every trial validated.
pub(crate) fn schedule(bench: &mut dyn Bench, plan: &Plan) -> Result<Comparison, String> {
    let mut records: Vec<Record> = Vec::new();
    let mut shapes: BTreeMap<Arm, super::start::Shape> = BTreeMap::new();
    let mut backend: Option<String> = None;
    let mut first = true;
    for pair in 1..=plan.pairs {
        let order = if pair % 2 == 1 {
            [Arm::Baseline, Arm::Candidate]
        } else {
            [Arm::Candidate, Arm::Baseline]
        };
        for (slot, arm) in (1u8..).zip(order) {
            if !first && plan.cooldown_s > 0.0 {
                bench.cool(plan.cooldown_s);
            }
            first = false;
            let label = format!("pair {pair:02} {}", arm.name());
            let obs = bench
                .trial(arm, plan.warm)
                .map_err(|e| format!("{label}: INVALID: {e}"))?;
            let sample =
                Sample::from_observation(&obs, &plan.laws).map_err(|e| format!("{label}: {e}"))?;
            let shape = sample.shape();
            if let Some(prev) = shapes.insert(arm, shape)
                && prev != shape
            {
                return Err(format!(
                    "{} startup-metric availability changed during the run: {prev:?} then {shape:?}",
                    arm.name()
                ));
            }
            match &backend {
                Some(b) if *b != sample.backend => {
                    return Err(format!(
                        "mixed actual backends cannot share one comparison: {b} then {}",
                        sample.backend
                    ));
                }
                _ => backend = Some(sample.backend.clone()),
            }
            eprintln!(
                "pair {pair:02} {slot}/2 {}: internal_ms={:.2} rust_main_ms={} visible_ms={} \
                 backend={} drops={} thermal={}",
                arm.name(),
                sample.first_present_ms,
                fmt_opt(sample.rust_main_ms),
                fmt_opt(sample.visible_ms),
                sample.backend,
                sample.present_drops,
                sample.thermal
            );
            records.push(Record {
                pair,
                slot,
                arm,
                thermal: classify_thermal(&sample.thermal),
                sample,
            });
        }
    }
    let by_arm = |pick: fn(&super::start::Shape) -> bool| -> BTreeMap<Arm, bool> {
        shapes.iter().map(|(a, s)| (*a, pick(s))).collect()
    };
    let router_by_arm = by_arm(|s| s.router);
    let visible_by_arm = by_arm(|s| s.visible);
    let phases_by_arm = by_arm(|s| s.phases);
    let attach_by_arm = by_arm(|s| s.attach);
    let all = |m: &BTreeMap<Arm, bool>| m.len() == 2 && m.values().all(|v| *v);
    let router = all(&router_by_arm);
    let visible = all(&visible_by_arm);
    let legacy_totals_only = !(router && all(&phases_by_arm) && all(&attach_by_arm));

    let arm_values = |arm: Arm, pick: &dyn Fn(&Sample) -> Option<f64>| -> Vec<f64> {
        let mut rows: Vec<&Record> = records.iter().filter(|r| r.arm == arm).collect();
        rows.sort_by_key(|r| r.pair);
        rows.iter().filter_map(|r| pick(&r.sample)).collect()
    };
    let compared = |name: &str, pick: &dyn Fn(&Sample) -> Option<f64>| -> Compared {
        let baseline = arm_values(Arm::Baseline, pick);
        let candidate = arm_values(Arm::Candidate, pick);
        let deltas: Vec<f64> = baseline
            .iter()
            .zip(&candidate)
            .map(|(b, c)| b - c)
            .collect();
        Compared {
            name: name.to_string(),
            rounds: stats::rounds(&deltas),
            baseline,
            candidate,
            deltas,
        }
    };
    let mut totals = vec![compared("internal_first_present_ms", &|s| {
        Some(s.first_present_ms)
    })];
    if router {
        totals.push(compared("rust_main_to_first_present_ms", &|s| {
            s.rust_main_ms
        }));
    }
    if visible {
        totals.push(compared("rust_main_to_first_visible_ms", &|s| s.visible_ms));
    }
    let (phases, attach) = if legacy_totals_only {
        (Vec::new(), Vec::new())
    } else {
        (
            PHASES
                .iter()
                .enumerate()
                .map(|(i, n)| compared(n, &|s| s.phases.as_ref().map(|p| p[i])))
                .collect(),
            ATTACH
                .iter()
                .enumerate()
                .map(|(i, n)| compared(n, &|s| s.attach.as_ref().map(|a| a[i])))
                .collect(),
        )
    };

    let mut thermal_counts: BTreeMap<Thermal, usize> = [
        Thermal::Clean,
        Thermal::Unavailable,
        Thermal::WarningOrUnrecognized,
    ]
    .into_iter()
    .map(|t| (t, 0))
    .collect();
    for r in &records {
        *thermal_counts.entry(r.thermal).or_default() += 1;
    }
    let eligibility = Eligibility {
        hardware: super::rule_zero_hardware(&plan.chip),
        sample_count: plan.pairs >= MIN_PAIRS,
        thermal_available: thermal_counts[&Thermal::Unavailable] == 0,
        thermal_no_warning: thermal_counts[&Thermal::WarningOrUnrecognized] == 0,
        cooldown: plan.cooldown_s >= MIN_COOLDOWN_S,
        negative_control: plan.negative_control,
        thermal_counts,
    };
    Ok(Comparison {
        plan: plan.clone(),
        rounds: plan.pairs / 2,
        trailing_unbalanced_pair: plan.pairs % 2 == 1,
        legacy_totals_only,
        router_by_arm,
        visible_by_arm,
        phases_by_arm,
        attach_by_arm,
        backend: backend.unwrap_or_default(),
        totals,
        phases,
        attach,
        eligibility,
        records,
    })
}

fn fmt_opt(v: Option<f64>) -> String {
    v.map_or_else(|| "unavailable".into(), |v| format!("{v:.2}"))
}

fn p95_text(values: &[f64]) -> String {
    if values.len() >= stats::P95_MIN_N {
        format!("{:.2}", stats::percentile_nearest_rank(values, 0.95))
    } else {
        "omitted(<20)".into()
    }
}

/// The human report. Pure (unit-tested through its text).
pub(crate) fn report(c: &Comparison) -> String {
    let mut s = String::from("== comparison summary ==\n");
    s.push_str(&format!(
        "reported unit: ROUND (ABBA quad = 2 pairs = 4 launches); {} complete rounds from {} pairs\n",
        c.rounds, c.plan.pairs
    ));
    if c.trailing_unbalanced_pair {
        s.push_str(&format!(
            "WARNING: --pairs {p} is odd; pair {p} has no partner to balance its slot order and \
             is EXCLUDED from every round statistic. Use an even --pairs.\n",
            p = c.plan.pairs
        ));
    }
    if c.rounds == 0 {
        s.push_str(
            "WARNING: NO ORDER-BALANCED ROUND WAS FORMED. The pair-level delta carries the whole \
             slot-1-vs-slot-2 offset and is not evidence of anything. Run at least --pairs 2.\n",
        );
    }
    for m in &c.totals {
        s.push_str(&format!(
            "{}: baseline median={:.2} p95={}; candidate median={:.2} p95={}\n",
            m.name,
            stats::median(&m.baseline),
            p95_text(&m.baseline),
            stats::median(&m.candidate),
            p95_text(&m.candidate)
        ));
        match &m.rounds {
            None => s.push_str("  round candidate-faster delta: unavailable (no complete round)\n"),
            Some(r) => {
                s.push_str(&format!(
                    "  round candidate-faster delta: median={:.2}ms mean={:.2}ms \
                     wins/ties/losses={}/{}/{} over {} rounds\n",
                    r.median,
                    r.mean,
                    r.faster,
                    r.ties,
                    r.slower,
                    r.values.len()
                ));
                match r.mean_interval {
                    None => s.push_str("    resolution: unavailable (<2 rounds)\n"),
                    Some(i) => s.push_str(&format!(
                        "    95% t interval on the mean: [{:+.2},{:+.2}]ms; THIS RUN RESOLVES \
                         >{:.2}ms — zero is {} it\n",
                        i.low,
                        i.high,
                        i.half_width,
                        if i.low <= 0.0 && 0.0 <= i.high {
                            "inside"
                        } else {
                            "outside"
                        }
                    )),
                }
                match r.median_interval {
                    None => s.push_str("    distribution-free median interval: unavailable (<6 rounds)\n"),
                    Some(i) => s.push_str(&format!(
                        "    distribution-free median interval ({:.1}% coverage): [{:+.2},{:+.2}]ms\n",
                        i.coverage * 100.0,
                        i.low,
                        i.high
                    )),
                }
            }
        }
        let d = &m.deltas;
        s.push_str(&format!(
            "  (order-contaminated pair-level delta, drill-down only: median={:.2}ms \
             wins/ties/losses={}/{}/{})\n",
            stats::median(d),
            d.iter().filter(|v| **v > 0.0).count(),
            d.iter().filter(|v| **v == 0.0).count(),
            d.iter().filter(|v| **v < 0.0).count()
        ));
    }
    let names = |m: &BTreeMap<Arm, bool>| {
        m.iter()
            .map(|(a, v)| format!("{}={v}", a.name()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !c
        .totals
        .iter()
        .any(|m| m.name == "rust_main_to_first_present_ms")
    {
        s.push_str(&format!(
            "rust_main_to_first_present_ms: not compared (availability by arm: {})\n",
            names(&c.router_by_arm)
        ));
    }
    if !c
        .totals
        .iter()
        .any(|m| m.name == "rust_main_to_first_visible_ms")
    {
        s.push_str(&format!(
            "rust_main_to_first_visible_ms: not compared (availability by arm: {})\n",
            names(&c.visible_by_arm)
        ));
    }
    if c.legacy_totals_only {
        s.push_str(
            "comparison mode: common totals only; the arms' schemas differ, so router/phase/attach \
             comparisons are unavailable\n",
        );
    }
    for (title, group) in [
        (
            "startup phases (schema 1, candidate-median rank)",
            &c.phases,
        ),
        (
            "startup attach drill-down (schema 1, candidate-median rank)",
            &c.attach,
        ),
    ] {
        if group.is_empty() {
            continue;
        }
        s.push_str(&format!("{title}:\n"));
        let mut ranked: Vec<&Compared> = group.iter().collect();
        ranked.sort_by(|a, b| stats::median(&b.candidate).total_cmp(&stats::median(&a.candidate)));
        for m in ranked {
            s.push_str(&format!(
                "  {}: baseline={:.2}ms candidate={:.2}ms round_delta={}\n",
                m.name,
                stats::median(&m.baseline),
                stats::median(&m.candidate),
                m.rounds
                    .as_ref()
                    .map_or_else(|| "unavailable".into(), |r| format!("{:.2}ms", r.median))
            ));
        }
    }
    s.push_str(&format!(
        "chip: {}\nactual backend: {}\n",
        c.plan.chip, c.backend
    ));
    if c.eligibility.all() {
        s.push_str(
            "run class: automated Rule-Zero inputs eligible; publication still requires manual \
             display/focus/App-Nap/DND controls and a ledger entry\n",
        );
    } else {
        s.push_str(&format!(
            "run class: DEV-SMOKE, NON-PUBLISHABLE ({})\n",
            c.eligibility.reasons().join("; ")
        ));
    }
    s
}

fn compared_json(m: &Compared) -> Value {
    json!({
        "baseline": stats::distribution(&m.baseline),
        "candidate": stats::distribution(&m.candidate),
        "paired_candidate_faster_ms": stats::paired_json(&m.deltas),
        "rounds_candidate_faster_ms": m.rounds.as_ref().map(Rounds::to_json),
    })
}

fn group_json(group: &[Compared]) -> Value {
    if group.is_empty() {
        return Value::Null;
    }
    Value::Object(
        group
            .iter()
            .map(|m| (m.name.clone(), compared_json(m)))
            .collect(),
    )
}

/// The `aterm-start-compare-v2` result. `provenance` carries what only the
/// real run knows (timestamps, artifacts, the harness's own hash).
pub(crate) fn result_json(c: &Comparison, provenance: Value) -> Value {
    let total = |name: &str| {
        c.totals
            .iter()
            .find(|m| m.name == name)
            .map_or(Value::Null, compared_json)
    };
    let arms = |m: &BTreeMap<Arm, bool>| -> Value {
        Value::Object(
            m.iter()
                .map(|(a, v)| (a.name().to_string(), json!(v)))
                .collect(),
        )
    };
    let e = &c.eligibility;
    let counts: BTreeMap<&str, usize> = e
        .thermal_counts
        .iter()
        .map(|(t, n)| {
            (
                match t {
                    Thermal::Clean => "clean",
                    Thermal::Unavailable => "unavailable",
                    Thermal::WarningOrUnrecognized => "warning_or_unrecognized",
                },
                *n,
            )
        })
        .collect();
    json!({
        "schema": "aterm-start-compare-v2",
        "pairs": c.plan.pairs,
        "processes": c.plan.pairs * 2,
        "rotation": "odd=baseline,candidate; even=candidate,baseline; pairs 2k-1,2k fold into one order-balanced ABBA round",
        "reported_unit": "round",
        "rounds": c.rounds,
        "trailing_unbalanced_pair": c.trailing_unbalanced_pair,
        "backend_request": c.plan.laws.request.name(),
        "actual_backend": c.backend,
        "comparison_mode": if c.legacy_totals_only { "common_totals_only" } else { "full" },
        "legacy_totals_only": c.legacy_totals_only,
        "rust_main_available_by_arm": arms(&c.router_by_arm),
        "first_visible_available_by_arm": arms(&c.visible_by_arm),
        "startup_phases_available_by_arm": arms(&c.phases_by_arm),
        "startup_attach_available_by_arm": arms(&c.attach_by_arm),
        "warm": c.plan.warm,
        "chip": c.plan.chip,
        "negative_control": c.plan.negative_control,
        "automated_rule_zero_inputs": {
            "hardware": {"eligible": e.hardware, "observed": c.plan.chip, "required": "exact sysctl identity: Apple M4 Max"},
            "sample_count": {"eligible": e.sample_count, "observed_pairs": c.plan.pairs, "required_pairs": MIN_PAIRS},
            "thermal": {
                "eligible": e.thermal(),
                "observations_available": e.thermal_available,
                "no_warning_or_unrecognized_observation": e.thermal_no_warning,
                "status_counts": counts,
            },
            "cooldown": {"eligible": e.cooldown, "observed_seconds": c.plan.cooldown_s, "required_seconds": MIN_COOLDOWN_S},
            "negative_control": {"eligible": !e.negative_control, "observed": e.negative_control},
        },
        "automated_rule_zero_eligible": e.all(),
        "manual_rule_zero_conditions_verified": false,
        "parameters": {
            "pairs": c.plan.pairs,
            "backend": c.plan.laws.request.name(),
            "settle_ms": c.plan.laws.settle_ms,
            "cooldown_s": c.plan.cooldown_s,
            "allow_legacy_metric": c.plan.laws.allow_legacy,
            "warm": c.plan.warm,
        },
        "provenance": provenance,
        "records": c.records.iter().map(|r| {
            let mut v = r.sample.to_json();
            if let Some(o) = v.as_object_mut() {
                o.insert("pair".into(), json!(r.pair));
                o.insert("order_in_pair".into(), json!(r.slot));
                o.insert("arm".into(), json!(r.arm.name()));
            }
            v
        }).collect::<Vec<_>>(),
        "summary": {
            "internal_first_present_ms": total("internal_first_present_ms"),
            "rust_main_to_first_present_ms": total("rust_main_to_first_present_ms"),
            "rust_main_to_first_visible_ms": total("rust_main_to_first_visible_ms"),
            "startup_phases_ms": group_json(&c.phases),
            "startup_attach_phases_ms": group_json(&c.attach),
        },
    })
}

struct WindowBench<'a> {
    launchers: BTreeMap<Arm, Launcher>,
    root: &'a Path,
    sockets: &'a Scratch,
    trials: u32,
}

impl Bench for WindowBench<'_> {
    fn trial(&mut self, arm: Arm, warm: bool) -> Result<Observation, String> {
        self.trials += 1;
        let name = format!("trial-{:03}-{}", self.trials, arm.name());
        let root = self.root.join(&name);
        let sock = self.sockets.sock(&format!("{name}.sock"))?;
        self.launchers[&arm].trial(&root, &sock, warm)
    }

    fn cool(&mut self, seconds: f64) {
        if let Ok(d) = Duration::try_from_secs_f64(seconds) {
            std::thread::sleep(d);
        }
    }
}

/// The `compare` lane.
pub(crate) fn run(args: &Args) -> LaneRun {
    super::outcome(run_inner(args))
}

fn run_inner(args: &Args) -> Result<String, Halt> {
    let (Some(baseline), Some(candidate)) = (&args.baseline, &args.candidate) else {
        return Err("compare needs --baseline PATH and --candidate PATH".into());
    };
    let settle_ms = args.settle_ms.unwrap_or(1500);
    let timeout = Duration::from_secs(args.timeout_s.unwrap_or(15));
    let started = utc_timestamp();
    let run_root = Scratch::new("arena-compare")?;
    let sockets = Scratch::sockets()?;
    let a = freeze("baseline", baseline, run_root.path())?;
    let b = freeze("candidate", candidate, run_root.path())?;
    let identical = a.sha256 == b.sha256;
    if identical && !args.allow_identical {
        return Err(
            "baseline and candidate hashes are identical; pass --allow-identical only \
                    for a scheduler negative-control run"
                .into(),
        );
    }
    let plan = Plan {
        pairs: args.pairs.unwrap_or(20),
        laws: Laws {
            settle_ms,
            request: args.backend,
            allow_legacy: args.allow_legacy_metric,
        },
        cooldown_s: args.cooldown_s.unwrap_or(MIN_COOLDOWN_S),
        warm: args.warm,
        chip: super::chip(),
        negative_control: identical,
    };
    let harness = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let harness_sha = sha256_file(&harness)?;
    eprintln!("== ARENA-START-AB: rotated paired fresh processes ==");
    eprintln!("pairs: {} ({} fresh processes)", plan.pairs, plan.pairs * 2);
    eprintln!("backend request: {}", plan.laws.request.name());
    eprintln!(
        "settle_ms: {settle_ms}; timeout_s: {}; cooldown_s: {}",
        timeout.as_secs(),
        plan.cooldown_s
    );
    eprintln!(
        "isolation: {}",
        if plan.warm {
            "WARM - each trial primes its own private home with a discarded launch and measures the second"
        } else {
            "COLD - each trial measures the first launch into a fresh home"
        }
    );
    eprintln!("harness: {} sha256 {harness_sha}", harness.display());
    for f in [&a, &b] {
        eprintln!(
            "{}: {} ({}) sha256 {}",
            f.frozen.display(),
            f.source.display(),
            f.version,
            f.sha256
        );
    }
    eprintln!(
        "schedule: odd pairs AB; even pairs BA; pairs 2k-1+2k = one ABBA round ({} rounds, the \
         reported unit)",
        plan.pairs / 2
    );
    if identical {
        eprintln!("comparison class: IDENTICAL-ARTIFACT NEGATIVE CONTROL");
    }
    eprintln!(
        "manual rule-zero duties: same visible/frontmost display, refresh mode, DND, App Nap, \
         machine quiescence, and an append-only ledger entry"
    );
    let launcher = |f: &Frozen| Launcher {
        binary: f.frozen.clone(),
        backend: args.backend,
        settle: Duration::from_millis(settle_ms),
        timeout,
    };
    let mut bench = WindowBench {
        launchers: [
            (Arm::Baseline, launcher(&a)),
            (Arm::Candidate, launcher(&b)),
        ]
        .into_iter()
        .collect(),
        root: run_root.path(),
        sockets: &sockets,
        trials: 0,
    };
    let comparison = schedule(&mut bench, &plan)?;
    for f in [&a, &b] {
        if sha256_file(&f.frozen)? != f.sha256 {
            return Err(Halt::Fail(format!(
                "{}: the staged artifact changed during the run",
                f.frozen.display()
            )));
        }
    }
    print!("{}", report(&comparison));
    if let Some(path) = &args.json_out {
        let artifact = |f: &Frozen| json!({"source": f.source.display().to_string(), "version": f.version, "sha256": f.sha256});
        let provenance = json!({
            "started_at_utc": started,
            "completed_at_utc": utc_timestamp(),
            "harness": {"path": harness.display().to_string(), "sha256": harness_sha},
            "timeout_s": timeout.as_secs(),
            "allow_identical": args.allow_identical,
            "artifacts": {"baseline": artifact(&a), "candidate": artifact(&b)},
        });
        super::start::write_json_atomic(path, &result_json(&comparison, provenance))?;
    }
    Ok(super::start::reported(args.json_out.as_deref()))
}

#[cfg(test)]
#[path = "compare_tests.rs"]
mod tests;
