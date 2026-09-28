// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HarnessModelSwitch, Tier-1: the live upgrade's model switch, driven from
//! EVERY reachable state of the derived model, in every configuration of its
//! constants, and projected back onto it.
//!
//! WHAT IS DRIVEN. The model's `Visit` is the real
//! `upgrade_models::model_read_step` — the one read `upgrade_drive::model_judge`
//! wraps in its I/O for the drive's visit, the host's pre-filter and a riding
//! restart alike: the early return when no model can be run and none is
//! pending, what a transcript tail says runs (`live_model_at`, over a real
//! tail with the visit's own launch flag and process start), THE SETTLE STEP
//! (`ModelRecord::settle`), THE MODEL RULE (`model_due`: the conversation's own
//! family first, then the priority list, over the build's offer) over the
//! settled record, the cache's coldness
//! (`last_answer_at`) and THE DUE CLOCK (`ModelRecord::due_clock`), in the
//! order that function takes them — followed by `model_to` (with THE MODEL
//! LADDER, `model_moves_now`, inside it), as `visit_models` and
//! `riding_model_in` call it. The relaunches are the real record write before
//! the one act (`ModelRecord::asked`) and what the new process then runs
//! (`live_model_at`).
//!
//! WHAT IS TRANSCRIBED, NOT DRIVEN (a change there passes this bind):
//! `model_judge`'s and `model_read_kept`'s I/O (the record's load and save,
//! the transcript read, the ledger rows); the offer (`upgrade_models::offered`
//! over the build's catalog, the list and `availableModels` — the model's
//! `tgt` stands for its two shapes and none); `visit_models`' derivation of the announced model (the
//! upgrade's `model_list` while `Phase::Announced`) and of `build_restart`;
//! the pre-filter `model_wants_a_look`; `St::for_target`, `St::give_up`,
//! `St::unsent`, the READY handshake; `restart`'s ask of `st.model_list` and
//! the relaunch line itself (`relaunch::with_model` is taken as read: the
//! relaunch asks the notice's model, else keeps the launch flag); and which
//! view (managed or native) `riding_model_in` and `visit_models` judge a
//! session by, and the pre-filter's keep rule over two views — the model has
//! one.
//!
//! NEGATIVE CONTROLS: a TRANSCRIPTION of `model_read_step` whose parts can be
//! swapped — shown equal to the real function on every state first, so each
//! control is one defect away from the shipped code — with each real part in
//! a pre-fix or defect shape: the rule before `18090b6ae` (no remembered
//! `/model`, no saved default), the rule without its failed record, an
//! off-list model read as ranked last, the ladder before `2d4656f32` (a cold
//! cache only), the decision before `4f4c00777` (re-taken after the
//! announcement), the clock before `b3beaa542` (an unreadable visit restarts
//! it), a settle step that never records a failure, never records an
//! application, or (before 2026-09-27) fails an ask that ran; and the
//! function's own order: the rule asked before the settle step, and the early
//! return taken on an empty offer alone (a pending ask never settles). The
//! relaunch bind has its own: an ask not recorded, an ask recorded without
//! its time, and a relaunched process read as running nothing or as
//! ignoring its launch flag. Each is caught by the same enumeration that
//! passes the shipped code; a decision answering the other way disagrees on
//! every state.
//!
//! WHAT THE MODEL DOES NOT COVER (said here, where the bind is): the offer is
//! only ever every seed model, `claude-opus-5` alone, or nothing (so the only
//! move is ever INTO `claude-opus-5-5`: none into Fable, no second family
//! member above a person's choice, no family the list does not name moving
//! by the build's own word); a
//! person never relaunches the conversation by hand; the owner's `--now`
//! never re-arms a gave-up upgrade; a restart made for another reason is
//! modelled only outside an announcement, and always lands; the clock never
//! steps back (`due_since > now`); `[1m]` spellings, aliases and the time
//! within each clock band are exercised by the concretizations below, not by
//! the model.

use std::collections::{BTreeSet, VecDeque};

use aterm_agent::harness::upgrade_catalog::{Baked, BakedModel};
use aterm_agent::harness::upgrade_models::{
    self as models, CACHE_COLD_S, LiveModel, MODEL_SETTLE_S, MODEL_WARM_MAX_S, ModelRecord,
    ModelVerdict, ModelView, Priority, Settled,
};
use aterm_agent::harness::usage::rfc3339_utc;
use aterm_spec::derive::{Model, harness_model_switch_model};
use aterm_spec::interp;

use super::{Vars, assert_forged_rejected, assert_transition};

const O55: &str = "claude-opus-5-5";
const F51: &str = "claude-fable-5-1";
const O5: &str = "claude-opus-5";
const SONNET: &str = "claude-sonnet-5";

/// The wall clock every concretization is read at.
const NOW: u64 = 1_790_000_000;
/// When a relaunched process started (after every row of the old transcript).
const STARTED: u64 = NOW - 30;
/// When a VISITED process started: before every row of its transcript, so
/// the transcript says what runs.
const VISIT_STARTED: u64 = NOW - 10 * CACHE_COLD_S;

/// Every `(DefaultFable, PersonFlag, Build)` configuration (`PersonFlag`:
/// none, a person's launch id, a person's launch family alias).
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

/// The model's `live` value of a model id (`0`: none).
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

/// The model with one configuration's constants.
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

/// Every state the model reaches at `Buggy = 0`.
fn reachable(model: &Model) -> Vec<Vars> {
    let model = interp::with_buggy(model, 0);
    let mut seen: BTreeSet<Vec<(&'static str, i64)>> = BTreeSet::new();
    let mut out = Vec::new();
    let mut q = VecDeque::from([model.init_state()]);
    while let Some(s) = q.pop_front() {
        if !seen.insert(s.iter().map(|(k, v)| (*k, *v)).collect()) {
            continue;
        }
        for a in &model.actions {
            q.extend(model.successors(a.name, &s));
        }
        out.push(s);
    }
    out
}

/// The build's catalog: every model the projection names, each displayed by
/// its id (so a `/model` result row maps back to it).
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

/// `i`-th of `n` spellings for the concretization numbered `k`: the first
/// concretization of every state is spelling 0, the second rotates through
/// the rest, so every spelling is met across the space.
fn pick(k: usize, variant: usize, n: usize) -> usize {
    if variant == 0 { 0 } else { (k + variant) % n }
}

/// An answer row of the transcript, on `model`, at `t`.
fn answer(model: &str, t: u64) -> String {
    format!(
        r#"{{"type":"assistant","isSidechain":false,"timestamp":"{}","message":{{"model":"{model}","content":[{{"type":"text","text":"x"}}]}}}}"#,
        rfc3339_utc(i64::try_from(t).expect("small"))
    )
}

/// A `/model` result row naming `display`, at `t`.
fn set_model(display: &str, t: u64) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{}","message":{{"content":"<local-command-stdout>Set model to `{display}`</local-command-stdout>"}}}}"#,
        rfc3339_utc(i64::try_from(t).expect("small"))
    )
}

/// The real inputs one model state stands for.
#[derive(Debug, Clone)]
struct Inputs {
    /// The visit's transcript tail.
    tail: String,
    /// When the visited process started.
    started: Option<u64>,
    /// The build's offer (`upgrade_models::offered`).
    offered: Vec<String>,
    launch: Option<String>,
    default: Option<String>,
    record: ModelRecord,
    build: bool,
    /// The upgrade's model while it is ANNOUNCED, as `visit_models` reads it
    /// (`None`: not announced, or with no model).
    announced: Option<&'static str>,
    /// The upgrade's `model_list`, announced or gave up — what `restart`
    /// asks for.
    model_list: Option<&'static str>,
}

/// Concretization `variant` (0 or 1) of state `s` (the `k`-th reachable).
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
    } else if constant(m, "PersonFlag") == 1 {
        // A person's own id: one the list ranks lower, or does not name —
        // never the one this harness asks for.
        let flags = [F51, O5, SONNET];
        Some(flags[pick(k + 2, variant, flags.len())].to_string())
    } else if constant(m, "PersonFlag") == 2 {
        // A person's family alias, which only the build resolves.
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
    // Seconds within each clock band: its edges.
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
    // The transcript: its last answer, as old as the cache is warm or cold
    // (the band's edges), and a `/model` result after it when `cmd`.
    let age = if s["cold"] == 1 {
        [CACHE_COLD_S, 5 * CACHE_COLD_S][pick(k + 7, variant, 2)]
    } else {
        [0, CACHE_COLD_S - 1][pick(k + 7, variant, 2)]
    };
    let at = NOW - age;
    let mut started = [Some(VISIT_STARTED), None][pick(k + 8, variant, 2)];
    let tail = match (id_of(s["live"]), s["cmd"]) {
        // Only a synthetic row: an answer's time, and no model.
        (None, _) => answer("<synthetic>", at),
        (Some(id), 1) => {
            let display = if pick(k + 9, variant, 2) == 1 {
                format!("{id} (1M context)")
            } else {
                id.to_string()
            };
            format!("{}\n{}", answer(O5, at), set_model(&display, at))
        }
        // The relaunched process has not answered yet: what runs is its
        // launch flag, the transcript's last answer the process before it.
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

/// The record projected back onto the model's record variables of `s`. A
/// record the model cannot hold (another model asked for or due, a clock that
/// names no model) projects to a value no model state has, so it is a
/// disagreement, never a silent match.
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

/// A TRANSCRIPTION of `model_read_step` whose parts can be swapped: a
/// negative control's harness, one defect away from the shipped function
/// (shown equal to it on every state with the shipped parts).
struct Parts<'a> {
    rule: &'a Rule,
    ladder: &'a Ladder,
    model_to: &'a ModelTo,
    settle: &'a Settle,
    clock: &'a Clock,
    /// The rule asked BEFORE the settle step (over the record as loaded).
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
        rule_first: false,
        early_on_offer_alone: false,
    }
}

/// How a visit is run.
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
}

/// The transcription of `model_read_step` (see [`Parts`]).
fn transcribed(
    p: &Parts<'_>,
    record: &mut ModelRecord,
    view: &ModelView<'_>,
    tail: &str,
) -> (ModelVerdict, bool, u64) {
    let early = view.offered.is_empty() && (p.early_on_offer_alone || record.set.is_empty());
    if early {
        let verdict = ModelVerdict::Keep("no-model-available");
        let _ = (p.clock)(record, &verdict, view.now);
        return (verdict, false, 0);
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
    let cold =
        models::last_answer_at(tail).is_none_or(|at| view.now.saturating_sub(at) >= CACHE_COLD_S);
    let (due_for_s, _) = (p.clock)(record, &verdict, view.now);
    (verdict, cold, due_for_s)
}

/// One visit from inputs `i`: the read (`model_read_step`, or its
/// transcription), then `model_to` as `visit_models` calls it (the announced
/// model first, `build_restart` when a newer build is due) and as
/// `riding_model_in` calls it (no announcement, the restart happening).
fn visit(v: &Visitor<'_>, i: &Inputs) -> Visited {
    let list = Priority::seed(0);
    let catalog = baked();
    let view = ModelView {
        list: &list,
        baked: Some(&catalog),
        offered: &i.offered,
        launch: i.launch.as_deref(),
        default_model: i.default.as_deref(),
        started_s: i.started,
        now: NOW,
    };
    let mut record = i.record.clone();
    let shipped_to: &ModelTo = &models::model_to;
    let (verdict, cold, due_for_s, model_to) = match v {
        Visitor::Real => {
            let read = models::model_read_step(&mut record, &view, || i.tail.as_str());
            (read.verdict, read.cold, read.due_for_s, shipped_to)
        }
        Visitor::Copy(p) => {
            let (verdict, cold, due_for_s) = transcribed(p, &mut record, &view, &i.tail);
            (verdict, cold, due_for_s, p.model_to)
        }
    };
    Visited {
        model_to: model_to(i.announced, &verdict, cold, i.build, due_for_s),
        riding: model_to(None, &verdict, cold, true, due_for_s),
        record,
        verdict,
        due_for_s,
        cold,
    }
}

/// The visit bind: from every reachable state, both concretizations through
/// `v`, projected back and checked against the model's `Visit` — the verdict
/// against the model's rule, the ladder against its guard, the riding model
/// against `Ride`. Returns every disagreement.
fn visit_disagreements(m: &Model, states: &[Vars], v: &Visitor<'_>) -> Vec<String> {
    let ladder: &Ladder = match v {
        Visitor::Real => &models::model_moves_now,
        Visitor::Copy(p) => p.ladder,
    };
    let mut bad = Vec::new();
    for (k, s) in states.iter().enumerate() {
        if s["forged"] != 0 {
            continue;
        }
        let succ = m.successors("Visit", s);
        let [want] = succ.as_slice() else {
            bad.push(format!("Visit is not one step from {s:?}"));
            continue;
        };
        for variant in 0..2 {
            let i = concretize(m, s, k, variant);
            let got = visit(v, &i);
            let mut after = with_record(s, &got.record);
            after.insert("fresh", 1);
            after.insert("mto", index_of(got.model_to.as_deref()));
            if after != *want {
                bad.push(format!(
                    "Visit from {s:?} with {i:?}: real {after:?}, model {want:?}"
                ));
                continue;
            }
            // The rule's verdict IS the model's: due on a model it could read.
            let due =
                matches!(&got.verdict, ModelVerdict::Due { to, .. } if index_of(Some(to)) == 1);
            let model_due = want["due"] == 1 && want["live"] > 0;
            if due != model_due {
                bad.push(format!("the rule said {:?} at {s:?}", got.verdict));
            }
            // The cache and the ladder ARE the model's guard, wherever a move
            // is due.
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
            // A restart made for another reason carries exactly the due move
            // (the model's `Ride`).
            if index_of(got.riding.as_deref()) != i64::from(model_due) {
                bad.push(format!("the riding model is {:?} at {s:?}", got.riding));
            }
        }
    }
    bad
}

/// A transcript tail that names `s`'s live model the way the real code reads
/// it: its last answer, a `/model` result newer than that answer (`cmd`), or
/// (unread) only a synthetic row. Every row predates [`STARTED`].
fn tail_of(s: &Vars) -> Vec<String> {
    match (id_of(s["live"]), s["cmd"]) {
        (None, _) => vec![answer("<synthetic>", NOW - 100)],
        (Some(id), 1) => vec![answer(O5, NOW - 200), set_model(id, NOW - 100)],
        (Some(id), _) => vec![answer(id, NOW - 100)],
    }
}

type Ask = dyn Fn(&mut ModelRecord, &str, u64);
type LiveAt = dyn Fn(&str, Option<&Baked>, Option<&str>, Option<u64>) -> Option<LiveModel>;

/// The relaunch bind: from every reachable state where the model relaunches,
/// the record write before the one act (`ask`: the shipped
/// `ModelRecord::asked`), and what the new process then runs (`live_at`: the
/// shipped `live_model_at` over the old transcript, with the new launch flag
/// and start time), projected back and checked against the model's step.
fn relaunch_disagreements(m: &Model, states: &[Vars], ask: &Ask, live_at: &LiveAt) -> Vec<String> {
    let catalog = baked();
    let mut bad = Vec::new();
    for (k, s) in states.iter().enumerate() {
        for action in ["Relaunch", "RelaunchRunsOther", "Refused", "Ride"] {
            let succ = m.successors(action, s);
            let want = match succ.as_slice() {
                [] => continue,
                [one] => one,
                _ => {
                    bad.push(format!("{action} is not one step from {s:?}"));
                    continue;
                }
            };
            let i = concretize(m, s, k, 1);
            // The model the relaunch asks for: the upgrade's `model_list`
            // (announced, or a gave-up upgrade's late READY), or the due one a
            // restart made for another reason carries.
            let asks = match action {
                "Ride" => Some(O55),
                _ => i.model_list,
            };
            let mut record = i.record.clone();
            if let Some(model) = asks {
                ask(&mut record, model, NOW);
            }
            let mut after = with_record(s, &record);
            if action != "Refused" {
                // The relaunch line asks `--model` when there is a model, and
                // keeps the launch's own flag when there is none: this
                // harness's (the list's id, as it writes it) or the person's
                // (`claude-fable-5-1` in the model — an alias or another
                // model there is a visit's variant only, not a relaunch's).
                let kept = if s["flag"] == 1 {
                    Some(O55.to_string())
                } else if constant(m, "PersonFlag") == 1 {
                    Some(F51.to_string())
                } else if constant(m, "PersonFlag") == 2 {
                    // A person's alias: only the build resolves it, so what
                    // runs is read off the transcript.
                    Some("fable".to_string())
                } else {
                    None
                };
                let launch = asks.map(str::to_string).or(kept);
                let mut tail = tail_of(s);
                if action == "RelaunchRunsOther" {
                    // The new process answered, on another model.
                    tail.push(format!(
                        r#"{{"type":"assistant","isSidechain":false,"timestamp":"{}","message":{{"model":"{O5}"}}}}"#,
                        rfc3339_utc(i64::try_from(STARTED + 10).expect("small"))
                    ));
                }
                let live = live_at(
                    &tail.join("\n"),
                    Some(&catalog),
                    launch.as_deref(),
                    Some(STARTED),
                );
                after.insert("live", index_of(live.as_ref().map(|l| l.id.as_str())));
                after.insert(
                    "cmd",
                    i64::from(live.as_ref().is_some_and(|l| l.by_command)),
                );
                after.insert(
                    "flag",
                    i64::from(
                        launch
                            .as_deref()
                            .is_some_and(|l| l.strip_suffix("[1m]").unwrap_or(l) == O55),
                    ),
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

/// The shipped ask.
fn shipped_ask(r: &mut ModelRecord, model: &str, now: u64) {
    r.asked(model, now);
}

#[test]
fn upgrade_model_switch_is_the_models_on_every_reachable_state() {
    for (default_fable, person_flag, build) in CONFIGS {
        let m = configured(default_fable, person_flag, build);
        let states = reachable(&m);
        // A person's launch alias keeps everything, so its space is the one
        // with nothing ever asked for.
        let floor = if person_flag == 2 { 100 } else { 5000 };
        assert!(
            states.len() > floor,
            "the bind covers the space ({} states)",
            states.len()
        );
        let wrong = visit_disagreements(&m, &states, &Visitor::Real);
        assert!(
            wrong.is_empty(),
            "DefaultFable={default_fable} PersonFlag={person_flag} Build={build}: the real \
             visit is not the model's: {} case(s), first {:?}",
            wrong.len(),
            wrong.first()
        );
        // The controls' harness IS the shipped function, part for part.
        let copy = Visitor::Copy(shipped());
        for (k, s) in states.iter().enumerate() {
            for variant in 0..2 {
                let i = concretize(&m, s, k, variant);
                assert_eq!(
                    visit(&copy, &i),
                    visit(&Visitor::Real, &i),
                    "the transcription is not model_read_step at {s:?} with {i:?}"
                );
            }
        }
        let wrong = relaunch_disagreements(&m, &states, &shipped_ask, &models::live_model_at);
        assert!(
            wrong.is_empty(),
            "DefaultFable={default_fable} PersonFlag={person_flag} Build={build}: the real \
             relaunch is not the model's: {} case(s), first {:?}",
            wrong.len(),
            wrong.first()
        );
        eprintln!(
            "HarnessModelSwitch DefaultFable={default_fable} PersonFlag={person_flag} \
             Build={build}: the real model_read_step, model_to, ask and live_model_at agree \
             with the model on all {} reachable states",
            states.len()
        );
    }
}

/// THE OPPOSITE DECISION: a `model_to` that answers the other way disagrees
/// with the model on EVERY state, both concretizations — so the bind above
/// could not have passed one. A sample goes through every installed tier
/// (`ty trace validate`): each accepted as the real code produced it, and
/// refused with the decision flipped.
#[test]
fn upgrade_model_switch_refuses_the_opposite_decision() {
    let m = configured(0, 0, 0);
    let states = reachable(&m);
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
    let wrong = visit_disagreements(&m, &states, &flipped);
    assert_eq!(
        wrong.len(),
        2 * visited,
        "a decision answering the other way passed somewhere"
    );
    assert!(visited > 5000, "{visited}");

    let real = Visitor::Real;
    let mut s = m.init_state();
    s.insert("cold", 1);
    let got = visit(&real, &concretize(&m, &s, 0, 0));
    let mut moved = with_record(&s, &got.record);
    moved.insert("fresh", 1);
    moved.insert("mto", index_of(got.model_to.as_deref()));
    assert_eq!(
        (moved["due"], moved["mto"]),
        (1, 1),
        "due, and cold: it moves"
    );
    assert_transition(&m, &s, &moved, "Visit", "a due move on a cold cache");
    assert_forged_rejected(
        &m,
        &s,
        &moved,
        "Visit",
        &[("mto", 0)],
        "a cold cache that waits",
    );
    assert_forged_rejected(
        &m,
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
    let got = visit(&real, &concretize(&m, &announced, 0, 0));
    let mut sticky = with_record(&announced, &got.record);
    sticky.insert("fresh", 1);
    sticky.insert("mto", index_of(got.model_to.as_deref()));
    assert_eq!(
        (sticky["due"], sticky["mto"]),
        (0, 1),
        "the offer moved away and the announced model still rides"
    );
    assert_transition(
        &m,
        &announced,
        &sticky,
        "Visit",
        "the announced model rides",
    );
    assert_forged_rejected(
        &m,
        &announced,
        &sticky,
        "Visit",
        &[("mto", 0)],
        "the announced model dropped",
    );

    let mut record = concretize(&m, &sticky, 0, 0).record;
    record.asked(O55, NOW);
    let mut relaunched = with_record(&sticky, &record);
    relaunched.insert("fresh", 0);
    relaunched.insert("mto", 0);
    relaunched.insert("phase", 0);
    relaunched.insert("ann", 0);
    relaunched.insert("flag", 1);
    relaunched.insert("live", 1);
    assert_transition(
        &m,
        &sticky,
        &relaunched,
        "Relaunch",
        "the ask, then the relaunch",
    );
    assert_forged_rejected(
        &m,
        &sticky,
        &relaunched,
        "Relaunch",
        &[("set", 0)],
        "a relaunch that did not record its ask",
    );
}

/// NEGATIVE CONTROLS: each real part in a pre-fix or defect shape, and the
/// read's own order broken, is caught by the same enumeration that passes the
/// shipped code; so is each relaunch control.
#[test]
fn upgrade_model_switch_binds_catch_every_pre_fix_shape() {
    fn base(m: &str) -> String {
        m.strip_suffix("[1m]").unwrap_or(m).to_string()
    }
    // The rule before 18090b6ae: a person's choice read only off the launch
    // flag and a `/model` newer than the last answer.
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
    // The failed record not read: a model the relaunch did not take is asked
    // for again.
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
    // An off-list model's missing rank read as ranked last.
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
    // The ladder before 2d4656f32: a cold cache only.
    let cold_only = |cold: bool, _build: bool, _due: u64| cold.then_some("cache-cold");
    let cold_only_to =
        |announced: Option<&str>, v: &ModelVerdict, cold: bool, _build: bool, _due: u64| {
            announced.map(str::to_string).or_else(|| match v {
                ModelVerdict::Due { to, .. } if cold => Some(to.clone()),
                _ => None,
            })
        };
    // The decision before 4f4c00777: re-taken at every visit.
    let retaken =
        |_announced: Option<&str>, v: &ModelVerdict, cold: bool, build: bool, due: u64| {
            models::model_to(None, v, cold, build, due)
        };
    // The clock before b3beaa542: an unreadable visit restarts it.
    let unknown_clears = |r: &mut ModelRecord, v: &ModelVerdict, now: u64| {
        let v = match v {
            ModelVerdict::Keep("model-unknown") => &ModelVerdict::Keep("model-current"),
            other => other,
        };
        r.due_clock(v, now)
    };
    // A settle step that never records a failure (the ask stays pending),
    // one that never records an application, and the one before 2026-09-27,
    // whose failed arm did not read `applied`.
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
        // What it verified now stays verified; the earlier ones come back.
        r.applied = applied
            .into_iter()
            .chain(verified.then_some(asked))
            .collect();
        out
    };

    let controls: [(&str, Parts<'_>); 11] = [
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
    // The committed configuration catches every one.
    let m = configured(0, 0, 0);
    let states = reachable(&m);
    for (name, parts) in controls {
        let wrong = visit_disagreements(&m, &states, &Visitor::Copy(parts));
        assert!(!wrong.is_empty(), "the bind passed {name}");
        eprintln!(
            "HarnessModelSwitch Tier-1 negative control CAUGHT — {name}: {} disagreement(s), \
             first {}",
            wrong.len(),
            wrong[0]
        );
    }

    // The relaunch bind's own.
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
    let relaunch_controls: [(&str, &Ask, &LiveAt); 4] = [
        (
            "an ask not recorded before the one act",
            &no_ask,
            &models::live_model_at,
        ),
        (
            "an ask recorded without its time",
            &ask_untimed,
            &models::live_model_at,
        ),
        (
            "a relaunched process read as running nothing",
            &shipped_ask,
            &runs_nothing,
        ),
        (
            "a relaunched process read without its launch flag",
            &shipped_ask,
            &flag_ignored,
        ),
    ];
    for (name, ask, live_at) in relaunch_controls {
        let wrong = relaunch_disagreements(&m, &states, ask, live_at);
        assert!(!wrong.is_empty(), "the relaunch bind passed {name}");
        eprintln!(
            "HarnessModelSwitch Tier-1 relaunch control CAUGHT — {name}: {} disagreement(s), \
             first {}",
            wrong.len(),
            wrong[0]
        );
    }
}
