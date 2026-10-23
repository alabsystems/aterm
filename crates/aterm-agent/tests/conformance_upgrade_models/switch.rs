// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance and negative-control suite for `HarnessModelSwitch`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use aterm_agent::harness::upgrade_catalog::{Baked, BakedModel};
use aterm_agent::harness::upgrade_models::{
    self as models, CACHE_COLD_S, LiveModel, MODEL_SETTLE_S, MODEL_WARM_MAX_S, ModelRecord,
    ModelVerdict, ModelView, Priority, Settled,
};
use aterm_agent::harness::usage::rfc3339_utc;
use aterm_models_harness::harness_model_switch_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;

use super::{Vars, assert_forged_rejected, assert_transition};

const O55: &str = "claude-opus-5-5";
const F51: &str = "claude-fable-5-1";
const O5: &str = "claude-opus-5";
const SONNET: &str = "claude-sonnet-5";

const NOW: u64 = 1_790_000_000;
const STARTED: u64 = NOW - 30;
const VISIT_STARTED: u64 = NOW - 10 * CACHE_COLD_S;

const CONFIGS: [(i64, i64, i64); 12] = [
    (0, 0, 0),
    (1, 0, 0),
    (0, 1, 0),
    (1, 1, 0),
    (0, 2, 0),
    (1, 2, 0),
    (0, 0, 1),
    (1, 0, 1),
    (0, 1, 1),
    (1, 1, 1),
    (0, 2, 1),
    (1, 2, 1),
];

fn index_of(id: Option<&str>) -> i64 {
    match id.map(|m| m.strip_suffix("[1m]").unwrap_or(m)) {
        Some(O55) => 1,
        Some(F51) => 2,
        Some(O5) => 3,
        Some(SONNET) => 4,
        None => 0,
        Some(other) => panic!("a model the projection does not name: {other}"),
    }
}

fn id_of(n: i64) -> Option<&'static str> {
    match n {
        1 => Some(O55),
        2 => Some(F51),
        3 => Some(O5),
        4 => Some(SONNET),
        _ => None,
    }
}

fn configured(default_fable: i64, person_flag: i64, build: i64) -> Model {
    interp::with_consts(
        &harness_model_switch_model(),
        &[
            ("DefaultFable", default_fable),
            ("PersonFlag", person_flag),
            ("Build", build),
        ],
    )
}

fn constant(m: &Model, name: &str) -> i64 {
    m.consts
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .expect("a model constant")
}

fn reachable(model: &Model) -> Vec<Vars> {
    interp::reachable_states(&interp::with_buggy(model, 0))
}

fn baked() -> Baked {
    Baked {
        latest_per_family: vec![],
        models: [O55, F51, O5, SONNET]
            .iter()
            .map(|id| BakedModel {
                id: (*id).to_string(),
                family: models::family_version(id).expect("a model id").0,
                display_name: (*id).to_string(),
                native_1m: true,
                efforts: vec![],
            })
            .collect(),
    }
}

static LIST: LazyLock<Priority> = LazyLock::new(|| Priority::seed(0));
static CATALOG: LazyLock<Baked> = LazyLock::new(baked);

fn pick(k: usize, variant: usize, n: usize) -> usize {
    if variant == 0 { 0 } else { (k + variant) % n }
}

fn answer(model: &str, t: u64) -> String {
    format!(
        r#"{{"type":"assistant","isSidechain":false,"timestamp":"{}","message":{{"model":"{model}","content":[{{"type":"text","text":"x"}}]}}}}"#,
        rfc3339_utc(i64::try_from(t).expect("small"))
    )
}

fn set_model(display: &str, t: u64) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{}","message":{{"content":"<local-command-stdout>Set model to `{display}`</local-command-stdout>"}}}}"#,
        rfc3339_utc(i64::try_from(t).expect("small"))
    )
}

/// The real inputs one model state stands for.
#[derive(Debug, Clone)]
struct Inputs {
    tail: String,
    started: Option<u64>,
    /// The build's offer (`upgrade_models::offered`).
    offered: Vec<String>,
    launch: Option<String>,
    default: Option<String>,
    record: ModelRecord,
    build: bool,
    /// The upgrade's model while it is announced, as `visit_models` reads it
    /// (`None`: not announced, or with no model).
    announced: Option<&'static str>,
    /// The upgrade's `model_list`, announced or gave up — what `restart`
    /// asks for, and what the read re-judges (`ModelView::carried`).
    model_list: Option<&'static str>,
}

fn concretize(m: &Model, s: &Vars, k: usize, variant: usize) -> Inputs {
    let spell = |id: &'static str, i: usize| -> String {
        // `[1m]` is the same model to every comparison the rule makes.
        if i % 2 == 1 && id != SONNET {
            format!("{id}[1m]")
        } else {
            id.to_string()
        }
    };
    let launch = if s["flag"] == 1 {
        Some(spell(O55, pick(k + 1, variant, 2)))
    } else if s["flag"] == 2 {
        // A relaunch dropped this harness's failed ask: no `--model` at all.
        None
    } else if constant(m, "PersonFlag") == 1 {
        let flags = [F51, O5, SONNET];
        Some(flags[pick(k + 2, variant, flags.len())].to_string())
    } else if constant(m, "PersonFlag") == 2 {
        let aliases = ["fable", "opus"];
        Some(aliases[pick(k + 2, variant, aliases.len())].to_string())
    } else {
        None
    };
    let default = if constant(m, "DefaultFable") == 1 {
        let d = ["fable", F51];
        Some(d[pick(k + 3, variant, d.len())].to_string())
    } else {
        let d = [None, Some("opus"), Some(O55)];
        d[pick(k + 3, variant, d.len())].map(str::to_string)
    };
    let set_age = match (s["set"], s["sage"]) {
        (1, 0) => [0, MODEL_SETTLE_S - 1][pick(k + 4, variant, 2)],
        (1, _) => [MODEL_SETTLE_S, 10 * MODEL_SETTLE_S][pick(k + 4, variant, 2)],
        _ => [NOW, 7][pick(k + 4, variant, 2)],
    };
    let (due_to, due_since) = match (s["due"], s["dover"]) {
        (1, 0) => (
            O55.to_string(),
            NOW - [0, MODEL_WARM_MAX_S - 1][pick(k + 5, variant, 2)],
        ),
        (1, _) => (
            O55.to_string(),
            NOW - [MODEL_WARM_MAX_S, 3 * MODEL_WARM_MAX_S][pick(k + 5, variant, 2)],
        ),
        _ => (String::new(), 0),
    };
    let human = if s["hum"] == 1 {
        F51.to_string()
    } else {
        ["", O5, SONNET][pick(k + 6, variant, 3)].to_string()
    };
    let record = ModelRecord {
        applied: if s["ap"] == 1 {
            vec![O55.to_string()]
        } else {
            vec![]
        },
        set: if s["set"] == 1 {
            O55.to_string()
        } else {
            String::new()
        },
        set_at: NOW - set_age,
        failed: if s["fl"] == 1 {
            vec![O55.to_string()]
        } else {
            vec![]
        },
        human,
        last_seq: 0,
        seq_since_s: 0,
        due_to,
        due_since,
    };
    let age = if s["cold"] == 1 {
        [CACHE_COLD_S, 5 * CACHE_COLD_S][pick(k + 7, variant, 2)]
    } else {
        [0, CACHE_COLD_S - 1][pick(k + 7, variant, 2)]
    };
    let at = NOW - age;
    let mut started = [Some(VISIT_STARTED), None][pick(k + 8, variant, 2)];
    let tail = match (id_of(s["live"]), s["cmd"]) {
        (None, _) => answer("<synthetic>", at),
        (Some(id), 1) => {
            let display = if pick(k + 9, variant, 2) == 1 {
                format!("{id} (1M context)")
            } else {
                id.to_string()
            };
            format!("{}\n{}", answer(O5, at), set_model(&display, at))
        }
        (Some(O55), _) if s["flag"] == 1 && variant == 1 => {
            started = Some(at + 1);
            answer(O5, at)
        }
        (Some(id), _) => answer(&spell(id, pick(k, variant, 2)), at),
    };
    Inputs {
        tail,
        started,
        offered: match s["tgt"] {
            1 => vec![O55.to_string(), F51.to_string(), O5.to_string()],
            3 => vec![O5.to_string()],
            _ => vec![],
        },
        launch,
        default,
        record,
        build: constant(m, "Build") == 1,
        announced: (s["phase"] == 1 && s["ann"] == 1).then_some(O55),
        model_list: (s["phase"] > 0 && s["ann"] == 1).then_some(O55),
    }
}

fn with_record(s: &Vars, rec: &ModelRecord) -> Vars {
    let mut v = s.clone();
    let base = |m: &str| m.strip_suffix("[1m]").unwrap_or(m).to_string();
    let set = index_of((!rec.set.is_empty()).then_some(rec.set.as_str()));
    v.insert("set", set);
    v.insert(
        "sage",
        i64::from(set == 1 && NOW.saturating_sub(rec.set_at) >= MODEL_SETTLE_S),
    );
    v.insert("ap", i64::from(rec.applied.iter().any(|a| base(a) == O55)));
    v.insert("fl", i64::from(rec.failed.iter().any(|a| base(a) == O55)));
    v.insert("hum", i64::from(base(&rec.human) == F51));
    let due = index_of((!rec.due_to.is_empty()).then_some(rec.due_to.as_str()));
    v.insert(
        "due",
        if (due == 0) == (rec.due_since == 0) {
            due
        } else {
            -1
        },
    );
    v.insert(
        "dover",
        i64::from(due == 1 && NOW.saturating_sub(rec.due_since) >= MODEL_WARM_MAX_S),
    );
    v
}

type Rule = dyn Fn(
    &Priority,
    Option<&LiveModel>,
    &[String],
    Option<&str>,
    Option<&str>,
    &ModelRecord,
) -> ModelVerdict;
type Ladder = dyn Fn(bool, bool, u64) -> Option<&'static str>;
type ModelTo = dyn Fn(Option<&str>, &ModelVerdict, bool, bool, u64) -> Option<String>;
type Settle = dyn Fn(&mut ModelRecord, Option<&LiveModel>, u64) -> Settled;
type Clock = dyn Fn(&mut ModelRecord, &ModelVerdict, u64) -> (u64, bool);
type RideBar = dyn Fn(
    &str,
    &Priority,
    Option<&LiveModel>,
    Option<&str>,
    Option<&str>,
    &ModelRecord,
) -> Option<&'static str>;

struct Parts<'a> {
    rule: &'a Rule,
    ladder: &'a Ladder,
    model_to: &'a ModelTo,
    settle: &'a Settle,
    clock: &'a Clock,
    /// The ride's guard over the carried model (`ride_bar`).
    ride: &'a RideBar,
    /// The rule asked before the settle step (over the record as loaded).
    rule_first: bool,
    /// The early return taken on an empty offer alone, pending ask or not.
    early_on_offer_alone: bool,
}

fn shipped<'a>() -> Parts<'a> {
    Parts {
        rule: &models::model_due,
        ladder: &models::model_moves_now,
        model_to: &models::model_to,
        settle: &|r, live, now| r.settle(live, now),
        clock: &|r, v, now| r.due_clock(v, now),
        ride: &models::ride_bar,
        rule_first: false,
        early_on_offer_alone: false,
    }
}

enum Visitor<'a> {
    /// The shipped `model_read_step`, `model_moves_now` and `model_to`.
    Real,
    /// The transcription, with its parts.
    Copy(Parts<'a>),
}

/// What one visit produced.
#[derive(Debug, PartialEq, Eq)]
struct Visited {
    record: ModelRecord,
    verdict: ModelVerdict,
    due_for_s: u64,
    cold: bool,
    model_to: Option<String>,
    riding: Option<String>,
    /// Why the carried model no longer rides (`ride_bar`), or `None`.
    ride_barred: Option<&'static str>,
}

type Read = (ModelVerdict, bool, u64, Option<&'static str>);

fn transcribed(p: &Parts<'_>, record: &mut ModelRecord, view: &ModelView<'_>, tail: &str) -> Read {
    let early = view.offered.is_empty()
        && (p.early_on_offer_alone || (record.set.is_empty() && view.carried.is_none()));
    if early {
        let verdict = ModelVerdict::Keep("no-model-available");
        let _ = (p.clock)(record, &verdict, view.now);
        return (verdict, false, 0, None);
    }
    let live = models::live_model_at(tail, view.baked, view.launch, view.started_s);
    let judge = |record: &ModelRecord| {
        (p.rule)(
            view.list,
            live.as_ref(),
            view.offered,
            view.launch,
            view.default_model,
            record,
        )
    };
    let verdict = if p.rule_first {
        let verdict = judge(record);
        let _ = (p.settle)(record, live.as_ref(), view.now);
        verdict
    } else {
        let _ = (p.settle)(record, live.as_ref(), view.now);
        judge(record)
    };
    let ride_barred = view.carried.and_then(|carried| {
        (p.ride)(
            carried,
            view.list,
            live.as_ref(),
            view.launch,
            view.default_model,
            record,
        )
    });
    let cold =
        models::last_answer_at(tail).is_none_or(|at| view.now.saturating_sub(at) >= CACHE_COLD_S);
    let (due_for_s, _) = (p.clock)(record, &verdict, view.now);
    (verdict, cold, due_for_s, ride_barred)
}

struct ReadOut {
    record: ModelRecord,
    verdict: ModelVerdict,
    cold: bool,
    due_for_s: u64,
    ride_barred: Option<&'static str>,
}

fn read(
    v: &Visitor<'_>,
    Inputs {
        tail,
        started,
        offered,
        launch,
        default,
        record,
        build: _,
        announced: _,
        model_list,
    }: &Inputs,
) -> ReadOut {
    let view = ModelView {
        list: &LIST,
        baked: Some(&CATALOG),
        offered,
        launch: launch.as_deref(),
        default_model: default.as_deref(),
        started_s: *started,
        now: NOW,
        carried: *model_list,
    };
    let mut record = record.clone();
    let (verdict, cold, due_for_s, ride_barred) = match v {
        Visitor::Real => {
            let read = models::model_read_step(&mut record, &view, || tail.as_str());
            (read.verdict, read.cold, read.due_for_s, read.ride_barred)
        }
        Visitor::Copy(p) => transcribed(p, &mut record, &view, tail),
    };
    ReadOut {
        record,
        verdict,
        cold,
        due_for_s,
        ride_barred,
    }
}

fn decide(v: &Visitor<'_>, i: &Inputs, r: &ReadOut) -> Visited {
    let model_to: &ModelTo = match v {
        Visitor::Real => &models::model_to,
        Visitor::Copy(p) => p.model_to,
    };
    let announced = i.announced.filter(|_| r.ride_barred.is_none());
    Visited {
        model_to: model_to(announced, &r.verdict, r.cold, i.build, r.due_for_s),
        riding: model_to(None, &r.verdict, r.cold, true, r.due_for_s),
        record: r.record.clone(),
        verdict: r.verdict.clone(),
        due_for_s: r.due_for_s,
        cold: r.cold,
        ride_barred: r.ride_barred,
    }
}

fn visit(v: &Visitor<'_>, i: &Inputs) -> Visited {
    decide(v, i, &read(v, i))
}

/// Exactly what [`read`] is handed of a visit's inputs: the fields its parameter
/// binds.
#[derive(PartialEq, Eq, Hash)]
struct ReadKey {
    tail: String,
    started: Option<u64>,
    offered: Vec<String>,
    launch: Option<String>,
    default: Option<String>,
    /// [`ModelRecord`], field by field (it is not `Hash`).
    record: (
        Vec<String>,
        String,
        u64,
        Vec<String>,
        String,
        u64,
        u64,
        String,
        u64,
    ),
    carried: Option<&'static str>,
}

fn read_key(i: &Inputs) -> ReadKey {
    let Inputs {
        tail,
        started,
        offered,
        launch,
        default,
        record,
        build: _,
        announced: _,
        model_list,
    } = i;
    let ModelRecord {
        applied,
        set,
        set_at,
        failed,
        human,
        last_seq,
        seq_since_s,
        due_to,
        due_since,
    } = record;
    ReadKey {
        tail: tail.clone(),
        started: *started,
        offered: offered.clone(),
        launch: launch.clone(),
        default: default.clone(),
        record: (
            applied.clone(),
            set.clone(),
            *set_at,
            failed.clone(),
            human.clone(),
            *last_seq,
            *seq_since_s,
            due_to.clone(),
            *due_since,
        ),
        carried: *model_list,
    }
}

static KEYS: LazyLock<Mutex<HashMap<ReadKey, usize>>> = LazyLock::new(Mutex::default);

const RELAUNCHES: [&str; 5] = [
    "Relaunch",
    "RelaunchRunsOther",
    "Refused",
    "Ride",
    "Restart",
];

struct Space {
    m: Model,
    states: Vec<Vars>,
    /// Each state's two concretizations ([`concretize`], variants 0 and 1).
    inputs: Vec<[Inputs; 2]>,
    /// The number of each concretization's read key ([`KEYS`]).
    slots: Vec<[usize; 2]>,
    /// The model's successors of each state under `Visit`, then under each of
    /// [`RELAUNCHES`], with the state's environment built once for all six.
    steps: Vec<[Vec<Vars>; 6]>,
}

impl Space {
    fn new(m: Model) -> Space {
        let states = reachable(&m);
        let actions = ["Visit"]
            .into_iter()
            .chain(RELAUNCHES)
            .map(|name| {
                m.actions
                    .iter()
                    .find(|a| a.name == name)
                    .unwrap_or_else(|| panic!("no action `{name}` in model `{}`", m.name))
            })
            .collect::<Vec<_>>();
        let steps = states
            .iter()
            .map(|s| {
                let env = m.eval_env(s);
                std::array::from_fn(|a| m.successors_in(actions[a], &env, s))
            })
            .collect();
        let inputs: Vec<[Inputs; 2]> = states
            .iter()
            .enumerate()
            .map(|(k, s)| [0, 1].map(|variant| concretize(&m, s, k, variant)))
            .collect();
        let slots = {
            let mut keys = KEYS.lock().unwrap_or_else(PoisonError::into_inner);
            inputs
                .iter()
                .map(|pair| {
                    pair.each_ref().map(|i| {
                        let next = keys.len();
                        *keys.entry(read_key(i)).or_insert(next)
                    })
                })
                .collect()
        };
        Space {
            m,
            states,
            inputs,
            slots,
            steps,
        }
    }
}

struct Visits<'a> {
    by: Visitor<'a>,
    reads: Vec<Option<ReadOut>>,
}

impl<'a> Visits<'a> {
    fn new(by: Visitor<'a>) -> Self {
        Visits {
            by,
            reads: Vec::new(),
        }
    }

    fn visit(&mut self, i: &Inputs, slot: usize) -> Visited {
        if self.reads.len() <= slot {
            self.reads.resize_with(slot + 1, || None);
        }
        let by = &self.by;
        let r = self.reads[slot].get_or_insert_with(|| read(by, i));
        decide(by, i, r)
    }
}

fn visit_disagreements(space: &Space, v: &mut Visits<'_>) -> Vec<String> {
    let ladder: &Ladder = match &v.by {
        Visitor::Real => &models::model_moves_now,
        Visitor::Copy(p) => p.ladder,
    };
    let mut bad = Vec::new();
    for (k, s) in space.states.iter().enumerate() {
        if s["forged"] != 0 {
            continue;
        }
        let succ = &space.steps[k][0];
        let [want] = succ.as_slice() else {
            bad.push(format!("Visit is not one step from {s:?}"));
            continue;
        };
        for variant in 0..2 {
            let i = &space.inputs[k][variant];
            let got = v.visit(i, space.slots[k][variant]);
            let mut after = with_record(s, &got.record);
            after.insert("fresh", 1);
            after.insert("mto", index_of(got.model_to.as_deref()));
            if got.ride_barred.is_some() {
                after.insert("ann", 0);
            }
            if after != *want {
                bad.push(format!(
                    "Visit from {s:?} with {i:?}: real {after:?}, model {want:?}"
                ));
                continue;
            }
            let due =
                matches!(&got.verdict, ModelVerdict::Due { to, .. } if index_of(Some(to)) == 1);
            let model_due = want["due"] == 1 && want["live"] > 0;
            if due != model_due {
                bad.push(format!("the rule said {:?} at {s:?}", got.verdict));
            }
            if due {
                let moves = ladder(got.cold, i.build, got.due_for_s).is_some();
                let guard = want["cold"] == 1 || i.build || want["dover"] == 1;
                if got.cold != (want["cold"] == 1) || moves != guard {
                    bad.push(format!(
                        "the ladder said {moves} for cold={} build={} due_for={} at {s:?}",
                        got.cold, i.build, got.due_for_s
                    ));
                }
            }
            if index_of(got.riding.as_deref()) != i64::from(model_due) {
                bad.push(format!("the riding model is {:?} at {s:?}", got.riding));
            }
        }
    }
    bad
}

fn tail_of(s: &Vars) -> Vec<String> {
    match (id_of(s["live"]), s["cmd"]) {
        (None, _) => vec![answer("<synthetic>", NOW - 100)],
        (Some(id), 1) => vec![answer(O5, NOW - 200), set_model(id, NOW - 100)],
        (Some(id), _) => vec![answer(id, NOW - 100)],
    }
}

type Ask = dyn Fn(&mut ModelRecord, &str, u64);
type Refuse = dyn Fn(&mut ModelRecord, &str, u64);
type LiveAt = dyn Fn(&str, Option<&Baked>, Option<&str>, Option<u64>) -> Option<LiveModel>;
type Line = dyn Fn(Option<&str>, Option<&str>, &ModelRecord) -> Option<String>;

fn relaunch_disagreements(
    space: &Space,
    ask: &Ask,
    refuse: &Refuse,
    line: &Line,
    live_at: &LiveAt,
) -> Vec<String> {
    let (m, catalog) = (&space.m, &*CATALOG);
    let mut bad = Vec::new();
    for (k, s) in space.states.iter().enumerate() {
        for (action, succ) in RELAUNCHES.into_iter().zip(&space.steps[k][1..]) {
            let want = match succ.as_slice() {
                [] => continue,
                [one] => one,
                _ => {
                    bad.push(format!("{action} is not one step from {s:?}"));
                    continue;
                }
            };
            let i = &space.inputs[k][1];
            let asks = match action {
                "Ride" => Some(O55),
                "Restart" => None,
                _ => i.model_list,
            };
            let mut record = i.record.clone();
            if let Some(model) = asks {
                if action == "Refused" {
                    refuse(&mut record, model, NOW);
                } else {
                    ask(&mut record, model, NOW);
                }
            }
            let mut after = with_record(s, &record);
            if action != "Refused" {
                let on_line = match s["flag"] {
                    1 => Some(O55),
                    0 if constant(m, "PersonFlag") == 1 => Some(F51),
                    0 if constant(m, "PersonFlag") == 2 => Some("fable"),
                    _ => None,
                };
                let launch = match line(asks, on_line, &i.record).as_deref() {
                    Some("") => None,
                    Some(model) => Some(model.to_string()),
                    None => on_line.map(str::to_string),
                };
                let mut tail = tail_of(s);
                if action == "RelaunchRunsOther" {
                    tail.push(format!(
                        r#"{{"type":"assistant","isSidechain":false,"timestamp":"{}","message":{{"model":"{O5}"}}}}"#,
                        rfc3339_utc(i64::try_from(STARTED + 10).expect("small"))
                    ));
                }
                let live = live_at(
                    &tail.join("\n"),
                    Some(catalog),
                    launch.as_deref(),
                    Some(STARTED),
                );
                after.insert("live", index_of(live.as_ref().map(|l| l.id.as_str())));
                after.insert(
                    "cmd",
                    i64::from(live.as_ref().is_some_and(|l| l.by_command)),
                );
                let ours = launch
                    .as_deref()
                    .is_some_and(|l| l.strip_suffix("[1m]").unwrap_or(l) == O55);
                after.insert(
                    "flag",
                    if ours {
                        1
                    } else if s["flag"] > 0 {
                        2
                    } else {
                        0
                    },
                );
                after.insert("phase", 0);
                after.insert("ann", 0);
            }
            after.insert("fresh", 0);
            after.insert("mto", 0);
            if after != *want {
                bad.push(format!(
                    "{action} from {s:?}: real {after:?}, model {want:?}"
                ));
            }
        }
    }
    bad
}

fn shipped_ask(r: &mut ModelRecord, model: &str, now: u64) {
    r.asked(model, now);
}

fn shipped_refuse(r: &mut ModelRecord, model: &str, now: u64) {
    let asked = r.asked(model, now);
    r.take_back(asked);
}

fn shipped_line(asks: Option<&str>, launch: Option<&str>, r: &ModelRecord) -> Option<String> {
    models::line_model(asks, launch, r).map(str::to_owned)
}

#[test]
fn upgrade_model_switch_is_the_models_on_every_reachable_state() {
    let mut real = Visits::new(Visitor::Real);
    let mut copy = Visits::new(Visitor::Copy(shipped()));
    for (default_fable, person_flag, build) in CONFIGS {
        let space = Space::new(configured(default_fable, person_flag, build));
        let states = &space.states;
        let floor = if person_flag == 2 { 100 } else { 2500 };
        assert!(
            states.len() > floor,
            "the bind covers the space ({} states)",
            states.len()
        );
        let wrong = visit_disagreements(&space, &mut real);
        assert!(
            wrong.is_empty(),
            "DefaultFable={default_fable} PersonFlag={person_flag} Build={build}: the real \
             visit is not the model's: {} case(s), first {:?}",
            wrong.len(),
            wrong.first()
        );
        for (k, s) in states.iter().enumerate() {
            for variant in 0..2 {
                let (i, slot) = (&space.inputs[k][variant], space.slots[k][variant]);
                assert_eq!(
                    copy.visit(i, slot),
                    real.visit(i, slot),
                    "the transcription is not model_read_step at {s:?} with {i:?}"
                );
            }
        }
        let wrong = relaunch_disagreements(
            &space,
            &shipped_ask,
            &shipped_refuse,
            &shipped_line,
            &models::live_model_at,
        );
        assert!(
            wrong.is_empty(),
            "DefaultFable={default_fable} PersonFlag={person_flag} Build={build}: the real \
             relaunch is not the model's: {} case(s), first {:?}",
            wrong.len(),
            wrong.first()
        );
        eprintln!(
            "HarnessModelSwitch DefaultFable={default_fable} PersonFlag={person_flag} \
             Build={build}: the real model_read_step, model_to, ask, line_model and \
             live_model_at agree with the model on all {} reachable states",
            states.len()
        );
    }
}

#[test]
fn upgrade_model_switch_refuses_the_opposite_decision() {
    let space = Space::new(configured(0, 0, 0));
    let (m, states) = (&space.m, &space.states);
    let opposite = |a: Option<&str>, v: &ModelVerdict, cold: bool, build: bool, due: u64| {
        match models::model_to(a, v, cold, build, due) {
            Some(_) => None,
            None => Some(O55.to_string()),
        }
    };
    let flipped = Visitor::Copy(Parts {
        model_to: &opposite,
        ..shipped()
    });
    let visited = states.iter().filter(|s| s["forged"] == 0).count();
    let wrong = visit_disagreements(&space, &mut Visits::new(flipped));
    assert_eq!(
        wrong.len(),
        2 * visited,
        "a decision answering the other way passed somewhere"
    );
    assert!(visited > 3000, "{visited}");

    let real = Visitor::Real;
    let mut s = m.init_state();
    s.insert("cold", 1);
    let got = visit(&real, &concretize(m, &s, 0, 0));
    let mut moved = with_record(&s, &got.record);
    moved.insert("fresh", 1);
    moved.insert("mto", index_of(got.model_to.as_deref()));
    assert_eq!(
        (moved["due"], moved["mto"]),
        (1, 1),
        "due, and cold: it moves"
    );
    assert_transition(m, &s, &moved, "Visit", "a due move on a cold cache");
    assert_forged_rejected(
        m,
        &s,
        &moved,
        "Visit",
        &[("mto", 0)],
        "a cold cache that waits",
    );
    assert_forged_rejected(
        m,
        &s,
        &moved,
        "Visit",
        &[("due", 0), ("mto", 0)],
        "a due move not seen",
    );

    let mut announced = moved.clone();
    announced.insert("phase", 1);
    announced.insert("ann", 1);
    announced.insert("fresh", 0);
    announced.insert("mto", 0);
    announced.insert("cold", 0);
    announced.insert("tgt", 3);
    let got = visit(&real, &concretize(m, &announced, 0, 0));
    let mut sticky = with_record(&announced, &got.record);
    sticky.insert("fresh", 1);
    sticky.insert("mto", index_of(got.model_to.as_deref()));
    assert_eq!(
        (sticky["due"], sticky["mto"]),
        (0, 1),
        "the offer moved away and the announced model still rides"
    );
    assert_transition(m, &announced, &sticky, "Visit", "the announced model rides");
    assert_forged_rejected(
        m,
        &announced,
        &sticky,
        "Visit",
        &[("mto", 0)],
        "the announced model dropped",
    );

    let mut record = concretize(m, &sticky, 0, 0).record;
    record.asked(O55, NOW);
    let mut relaunched = with_record(&sticky, &record);
    relaunched.insert("fresh", 0);
    relaunched.insert("mto", 0);
    relaunched.insert("phase", 0);
    relaunched.insert("ann", 0);
    relaunched.insert("flag", 1);
    relaunched.insert("live", 1);
    assert_transition(
        m,
        &sticky,
        &relaunched,
        "Relaunch",
        "the ask, then the relaunch",
    );
    assert_forged_rejected(
        m,
        &sticky,
        &relaunched,
        "Relaunch",
        &[("set", 0)],
        "a relaunch that did not record its ask",
    );
}

#[test]
fn upgrade_model_switch_binds_catch_every_pre_fix_shape() {
    fn base(m: &str) -> String {
        m.strip_suffix("[1m]").unwrap_or(m).to_string()
    }
    let no_memory = |list: &Priority,
                     live: Option<&LiveModel>,
                     offered: &[String],
                     launch: Option<&str>,
                     _default: Option<&str>,
                     rec: &ModelRecord| {
        let mut rec = rec.clone();
        rec.human.clear();
        models::model_due(list, live, offered, launch, None, &rec)
    };
    let no_failed = |list: &Priority,
                     live: Option<&LiveModel>,
                     offered: &[String],
                     launch: Option<&str>,
                     default: Option<&str>,
                     rec: &ModelRecord| {
        let mut rec = rec.clone();
        rec.failed.clear();
        models::model_due(list, live, offered, launch, default, &rec)
    };
    let off_list_last = |list: &Priority,
                         live: Option<&LiveModel>,
                         offered: &[String],
                         launch: Option<&str>,
                         default: Option<&str>,
                         rec: &ModelRecord| {
        let mut list = list.clone();
        if let Some(l) = live
            && list.rank(&l.id).is_none()
        {
            list.ids.push(base(&l.id));
        }
        models::model_due(&list, live, offered, launch, default, rec)
    };
    let cold_only = |cold: bool, _build: bool, _due: u64| cold.then_some("cache-cold");
    let cold_only_to =
        |announced: Option<&str>, v: &ModelVerdict, cold: bool, _build: bool, _due: u64| {
            announced.map(str::to_string).or_else(|| match v {
                ModelVerdict::Due { to, .. } if cold => Some(to.clone()),
                _ => None,
            })
        };
    let retaken =
        |_announced: Option<&str>, v: &ModelVerdict, cold: bool, build: bool, due: u64| {
            models::model_to(None, v, cold, build, due)
        };
    let unknown_clears = |r: &mut ModelRecord, v: &ModelVerdict, now: u64| {
        let v = match v {
            ModelVerdict::Keep("model-unknown") => &ModelVerdict::Keep("model-current"),
            other => other,
        };
        r.due_clock(v, now)
    };
    let never_fails = |r: &mut ModelRecord, live: Option<&LiveModel>, _now: u64| {
        let at = r.set_at;
        r.settle(live, at)
    };
    let never_applies = |r: &mut ModelRecord, live: Option<&LiveModel>, now: u64| {
        let applied = r.applied.clone();
        let out = r.settle(live, now);
        r.applied = applied;
        out
    };
    let fails_what_ran = |r: &mut ModelRecord, live: Option<&LiveModel>, now: u64| {
        let applied = std::mem::take(&mut r.applied);
        let asked = r.set.clone();
        let out = r.settle(live, now);
        let verified = out.verified.is_some();
        r.applied = applied
            .into_iter()
            .chain(verified.then_some(asked))
            .collect();
        out
    };

    let ride_no_record = |carried: &str,
                          list: &Priority,
                          live: Option<&LiveModel>,
                          launch: Option<&str>,
                          default: Option<&str>,
                          rec: &ModelRecord| {
        let mut rec = rec.clone();
        rec.applied.clear();
        rec.failed.clear();
        models::ride_bar(carried, list, live, launch, default, &rec)
    };
    let ride_no_choice = |carried: &str,
                          list: &Priority,
                          live: Option<&LiveModel>,
                          _launch: Option<&str>,
                          _default: Option<&str>,
                          rec: &ModelRecord| {
        let mut rec = rec.clone();
        rec.human.clear();
        let live = live.map(|l| LiveModel {
            by_command: false,
            ..l.clone()
        });
        models::ride_bar(carried, list, live.as_ref(), None, None, &rec)
    };
    let ride_off_list_last = |carried: &str,
                              list: &Priority,
                              live: Option<&LiveModel>,
                              launch: Option<&str>,
                              default: Option<&str>,
                              rec: &ModelRecord| {
        let mut list = list.clone();
        if let Some(l) = live
            && list.rank(&l.id).is_none()
        {
            list.ids.push(base(&l.id));
        }
        models::ride_bar(carried, &list, live, launch, default, rec)
    };

    let controls: [(&str, Parts<'_>); 14] = [
        (
            "the rule before 18090b6ae (no remembered /model)",
            Parts {
                rule: &no_memory,
                ..shipped()
            },
        ),
        (
            "the rule without its failed record",
            Parts {
                rule: &no_failed,
                ..shipped()
            },
        ),
        (
            "an off-list model read as ranked last",
            Parts {
                rule: &off_list_last,
                ..shipped()
            },
        ),
        (
            "the ladder before 2d4656f32 (cold only)",
            Parts {
                ladder: &cold_only,
                model_to: &cold_only_to,
                ..shipped()
            },
        ),
        (
            "the decision before 4f4c00777 (re-taken after the announcement)",
            Parts {
                model_to: &retaken,
                ..shipped()
            },
        ),
        (
            "the clock before b3beaa542 (an unreadable visit restarts it)",
            Parts {
                clock: &unknown_clears,
                ..shipped()
            },
        ),
        (
            "a settle step that never records a failure",
            Parts {
                settle: &never_fails,
                ..shipped()
            },
        ),
        (
            "a settle step that never records an application",
            Parts {
                settle: &never_applies,
                ..shipped()
            },
        ),
        (
            "the settle step before 2026-09-27 (an ask that ran is failed)",
            Parts {
                settle: &fails_what_ran,
                ..shipped()
            },
        ),
        (
            "the ride before 2026-10-01, (a): a model recorded failed rides",
            Parts {
                ride: &ride_no_record,
                ..shipped()
            },
        ),
        (
            "the ride before 2026-10-01, (b): a person's /model of another family moved",
            Parts {
                ride: &ride_no_choice,
                ..shipped()
            },
        ),
        (
            "the ride before 2026-10-01, (c): a model off the list moved",
            Parts {
                ride: &ride_off_list_last,
                ..shipped()
            },
        ),
        (
            "the rule asked before the settle step",
            Parts {
                rule_first: true,
                ..shipped()
            },
        ),
        (
            "the early return on an empty offer alone (a pending ask never settles)",
            Parts {
                early_on_offer_alone: true,
                ..shipped()
            },
        ),
    ];
    let space = Space::new(configured(0, 0, 0));
    for (name, parts) in controls {
        let wrong = visit_disagreements(&space, &mut Visits::new(Visitor::Copy(parts)));
        assert!(!wrong.is_empty(), "the bind passed {name}");
        eprintln!(
            "HarnessModelSwitch Tier-1 negative control CAUGHT — {name}: {} disagreement(s), \
             first {}",
            wrong.len(),
            wrong[0]
        );
    }

    let no_ask = |_r: &mut ModelRecord, _model: &str, _now: u64| {};
    let ask_untimed = |r: &mut ModelRecord, model: &str, _now: u64| {
        model.clone_into(&mut r.set);
    };
    let runs_nothing =
        |_t: &str, _b: Option<&Baked>, _l: Option<&str>, _s: Option<u64>| -> Option<LiveModel> {
            None
        };
    let flag_ignored = |t: &str, b: Option<&Baked>, _l: Option<&str>, s: Option<u64>| {
        models::live_model_at(t, b, None, s)
    };
    let keeps_every_flag =
        |asks: Option<&str>, _l: Option<&str>, _r: &ModelRecord| asks.map(str::to_owned);
    let relaunch_controls: [(&str, &Ask, &Refuse, &Line, &LiveAt); 6] = [
        (
            "an ask not recorded before the one act",
            &no_ask,
            &shipped_refuse,
            &shipped_line,
            &models::live_model_at,
        ),
        (
            "an ask recorded without its time",
            &ask_untimed,
            &shipped_refuse,
            &shipped_line,
            &models::live_model_at,
        ),
        (
            "a refused signal's ask kept",
            &shipped_ask,
            &shipped_ask,
            &shipped_line,
            &models::live_model_at,
        ),
        (
            "the line before 2026-10-01 (a --model recorded failed kept)",
            &shipped_ask,
            &shipped_refuse,
            &keeps_every_flag,
            &models::live_model_at,
        ),
        (
            "a relaunched process read as running nothing",
            &shipped_ask,
            &shipped_refuse,
            &shipped_line,
            &runs_nothing,
        ),
        (
            "a relaunched process read without its launch flag",
            &shipped_ask,
            &shipped_refuse,
            &shipped_line,
            &flag_ignored,
        ),
    ];
    for (name, ask, refuse, line, live_at) in relaunch_controls {
        let wrong = relaunch_disagreements(&space, ask, refuse, line, live_at);
        assert!(!wrong.is_empty(), "the relaunch bind passed {name}");
        eprintln!(
            "HarnessModelSwitch Tier-1 relaunch control CAUGHT — {name}: {} disagreement(s), \
             first {}",
            wrong.len(),
            wrong[0]
        );
    }
}
