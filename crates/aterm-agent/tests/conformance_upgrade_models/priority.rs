// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HarnessModelPriority, Tier-1: the live upgrade's model priority list
//! (`aterm_agent::harness::upgrade_models`) — its writers (`Priority::admit`,
//! the owner's `set`) and its reader (`upgrade_models::target`) — driven over
//! every reachable state of the derived model and projected back onto it,
//! with a pre-fix writer and a head-of-list reader as the caught negative
//! controls.

use std::collections::{BTreeSet, VecDeque};

use aterm_agent::harness::upgrade_catalog::{Baked, BakedModel};
use aterm_agent::harness::upgrade_models::{self as models, Change, Evidence, Priority, ServedRow};
use aterm_spec::derive::{Model, harness_model_priority_model};

use super::{Vars, assert_forged_rejected, assert_transition};

// ===========================================================================
// HarnessModelPriority — the list's writers and its reader
// ===========================================================================

const A0: &str = "claude-opus-4-8";
const A1: &str = "claude-opus-5";
const AH: &str = "claude-opus-5-1";
const A2: &str = "claude-opus-5-5";
const A3: &str = "claude-opus-5-6";
const B1: &str = "claude-fable-5-1";
const B2: &str = "claude-fable-5-2";
const C1: &str = "claude-sonnet-6";

/// Model position variable ↔ model id.
const POSITIONS: [(&str, &str); 8] = [
    ("p_a0", A0),
    ("p_a1", A1),
    ("p_ah", AH),
    ("p_a2", A2),
    ("p_a3", A3),
    ("p_b1", B1),
    ("p_b2", B2),
    ("p_c1", C1),
];

/// Availability variable ↔ id (the reader's input).
const AVAIL: [(&str, &str); 5] = [
    ("av_a1", A1),
    ("av_a2", A2),
    ("av_a3", A3),
    ("av_b1", B1),
    ("av_b2", B2),
];

/// The owner's three orders, as `models set` writes them.
const HUMAN: [(&str, [&str; 3]); 3] = [
    ("HumanSetsTheSeed", [A2, B1, A1]),
    ("HumanSetsOlderOpusFirst", [A1, A2, B1]),
    ("HumanSetsFableFirst", [B1, A1, A2]),
];

/// The list a model state describes, best first. The positions must be
/// exactly `1..=n`: the model's list is a list.
fn list_of(s: &Vars) -> Vec<String> {
    let mut placed: Vec<(i64, &str)> = POSITIONS
        .iter()
        .filter(|(k, _)| s[*k] > 0)
        .map(|(k, id)| (s[*k], *id))
        .collect();
    placed.sort_unstable();
    let ranks: Vec<i64> = placed.iter().map(|(p, _)| *p).collect();
    let expect: Vec<i64> = (1..=i64::try_from(placed.len()).expect("small")).collect();
    assert_eq!(ranks, expect, "the model's positions are not a list: {s:?}");
    placed.into_iter().map(|(_, id)| id.to_string()).collect()
}

/// `ids` projected back onto the position variables of `s`.
fn with_list(s: &Vars, ids: &[String]) -> Vars {
    let mut v = s.clone();
    for (k, id) in POSITIONS {
        let pos = ids
            .iter()
            .position(|x| x == id)
            .map_or(0, |i| i64::try_from(i + 1).expect("small"));
        v.insert(k, pos);
    }
    v
}

/// Every state the model reaches at `Buggy = 0`.
fn reachable(model: &Model) -> Vec<Vars> {
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

/// The build's catalog as the state says: the owner's ids and the three
/// never-admitted ones known, the two newcomers per `k_a3` / `k_b2`.
fn known_in(s: &Vars) -> impl Fn(&str) -> bool + '_ {
    move |id: &str| match id {
        A3 => s["k_a3"] == 1,
        B2 => s["k_b2"] == 1,
        _ => true,
    }
}

type Admit = dyn Fn(&mut Priority, &str, &dyn Fn(&str) -> bool) -> Option<Change>;

/// The writer bind: from every reachable state, every candidate through
/// `admit`, projected back, checked against the model's action for that
/// candidate. Returns every disagreement (empty for the shipped writer).
fn writer_disagreements(model: &Model, states: &[Vars], admit: &Admit) -> Vec<String> {
    let candidates = [
        (A3, "AdmitNewerOpus"),
        (B2, "AdmitNewerFable"),
        (A0, "OfferDowngrade"),
        (AH, "OfferBelowNewest"),
        (C1, "OfferCrossFamily"),
    ];
    let mut bad = Vec::new();
    for s in states {
        let before = Priority {
            ids: list_of(s),
            history: vec![],
        };
        for (id, action) in candidates {
            let mut after_list = before.clone();
            let change = admit(&mut after_list, id, &known_in(s));
            let mut after = with_list(s, &after_list.ids);
            if change.is_some() {
                after.insert("sel", 0);
                after.insert("pick", 0);
            }
            let admitted = model.successors(action, s).contains(&after);
            let fine = match (&change, model.action_enabled(action, s)) {
                // The writer wrote: the model's action for that candidate
                // must produce exactly that list.
                (Some(_), _) => admitted,
                // The writer refused: the model's action is disabled, or is
                // the healthy no-op of a mutant action.
                (None, enabled) => after == *s && (!enabled || admitted),
            };
            if !fine {
                bad.push(format!(
                    "{id} from {:?} -> {:?} (model `{action}`)",
                    before.ids, after_list.ids
                ));
            }
            if let Some(ch) = change {
                // The journalled placement names the id it went above.
                let at = after_list
                    .ids
                    .iter()
                    .position(|x| x == id)
                    .expect("inserted");
                if ch.placed != format!("above {}", after_list.ids[at + 1]) {
                    bad.push(format!("{id}: placed {:?}", ch.placed));
                }
            }
        }
    }
    bad
}

fn shipped_admit(p: &mut Priority, id: &str, known: &dyn Fn(&str) -> bool) -> Option<Change> {
    p.admit(id, "conformance", known, 0)
}

/// The writer as it was before the newest-listed check: strictly newer than
/// the family's best-RANKED member only.
fn pre_fix_admit(p: &mut Priority, id: &str, known: &dyn Fn(&str) -> bool) -> Option<Change> {
    let (family, version) = models::family_version(id)?;
    if p.rank(id).is_some() || !known(id) {
        return None;
    }
    let (pos, best) = p.ids.iter().enumerate().find_map(|(i, x)| {
        models::family_version(x)
            .filter(|(f, _)| *f == family)
            .map(|(_, v)| (i, v))
    })?;
    if version <= best {
        return None;
    }
    let above = p.ids[pos].clone();
    p.ids.insert(pos, id.to_string());
    Some(Change {
        id: id.to_string(),
        placed: format!("above {above}"),
        source: "conformance".into(),
        at: 0,
    })
}

/// Evidence under which exactly the ids the state marks available are
/// usable, each unavailable one refused for a different reason: absent from
/// the served catalog, deprecated, needing a newer Claude Code, denied by the
/// account, or a disabled option — five of `availability`'s refusal paths on
/// the reader's input (the build-does-not-know path is `known`'s, above).
fn evidence_for(s: &Vars) -> Evidence {
    let mut ev = Evidence {
        served: Some(Vec::new()),
        ..Evidence::default()
    };
    let rows = ev.served.as_mut().expect("served");
    for (i, (k, id)) in AVAIL.iter().enumerate() {
        let row = |section: &str, min_cc: Option<&str>| ServedRow {
            id: (*id).to_string(),
            section: section.into(),
            min_cc: min_cc.map(str::to_string),
            disabled: false,
            efforts: vec![],
        };
        if s[*k] == 1 {
            rows.push(row("main", Some("2.1.250")));
            continue;
        }
        match i % 5 {
            0 => {} // not offered at all
            1 => rows.push(row("deprecated", None)),
            2 => rows.push(row("main", Some("2.9.0"))), // needs a newer Claude Code
            3 => {
                rows.push(row("main", None));
                ev.denied.push((*id).to_string());
            }
            _ => {
                rows.push(row("main", None));
                ev.disabled_options.push((*id).to_string());
            }
        }
    }
    ev
}

fn baked_all() -> Baked {
    Baked {
        latest_per_family: vec![],
        models: [A0, A1, AH, A2, A3, B1, B2, C1]
            .iter()
            .map(|id| baked_model(id))
            .collect(),
    }
}

type Reader = dyn Fn(&Priority, &Evidence) -> Option<String>;

/// The reader bind: from every reachable state, the reader's pick projected
/// onto `pick`, checked against the model's `Select` (or, where the model
/// already holds a pick, against it).
fn reader_disagreements(model: &Model, states: &[Vars], reader: &Reader) -> Vec<String> {
    let mut bad = Vec::new();
    for s in states {
        let list = Priority {
            ids: list_of(s),
            history: vec![],
        };
        let got = reader(&list, &evidence_for(s));
        let pick = got
            .as_deref()
            .and_then(|id| list.ids.iter().position(|x| x == id))
            .map_or(0, |i| i64::try_from(i + 1).expect("small"));
        let fine = if s["sel"] == 1 {
            s["pick"] == pick
        } else {
            let mut after = s.clone();
            after.insert("sel", 1);
            after.insert("pick", pick);
            model.successors("Select", s).contains(&after)
        };
        if !fine {
            bad.push(format!("{:?} with {s:?} -> {got:?}", list.ids));
        }
    }
    bad
}

fn shipped_reader(list: &Priority, ev: &Evidence) -> Option<String> {
    // The live upgrade's model half asks exactly this, once per sweep
    // (`upgrade_drive`'s `ModelCtx::read`; `None`: no `availableModels`).
    models::target_allowed(list, "2.1.300", ev, Some(&baked_all()), None)
}

#[test]
fn upgrade_priority_writers_and_reader_are_the_models_on_every_reachable_state() {
    let model = harness_model_priority_model();
    let states = reachable(&model);
    assert!(
        states.len() > 1000,
        "the bind covers the space ({} states)",
        states.len()
    );

    // The file both halves share: every reachable list round-trips.
    for s in &states {
        let p = Priority {
            ids: list_of(s),
            history: vec![],
        };
        assert_eq!(
            Priority::parse(&p.render()).map(|q| q.ids),
            Some(p.ids.clone())
        );
    }

    let wrong = writer_disagreements(&model, &states, &shipped_admit);
    assert!(
        wrong.is_empty(),
        "the real admit is not the model's: {} case(s), first {:?}",
        wrong.len(),
        wrong.first()
    );

    let wrong = reader_disagreements(&model, &states, &shipped_reader);
    assert!(
        wrong.is_empty(),
        "the real reader is not the model's: {} case(s), first {:?}",
        wrong.len(),
        wrong.first()
    );

    // The owner's `models set`: the whole list, through the file, from every
    // reachable state.
    for s in &states {
        for (i, (action, order)) in HUMAN.iter().enumerate() {
            let written = Priority {
                ids: order.iter().map(|x| (*x).to_string()).collect(),
                history: vec![],
            };
            let read = Priority::parse(&written.render()).expect("the owner's list parses");
            let mut after = with_list(s, &read.ids);
            after.insert("hord", i64::try_from(i).expect("small"));
            after.insert("sel", 0);
            after.insert("pick", 0);
            assert!(
                model.successors(action, s).contains(&after),
                "`{action}` from {s:?} is not the real `models set`"
            );
        }
    }

    // One of each kind through every installed tier (`ty trace validate`).
    let seed = model.init_state();
    let mut known = seed.clone();
    known.insert("k_a3", 1);
    known.insert("k_b2", 1);
    let mut p = Priority::seed(0);
    assert!(p.admit(A3, "conformance", &known_in(&known), 0).is_some());
    let mut admitted = with_list(&known, &p.ids);
    admitted.insert("sel", 0);
    assert_transition(
        &model,
        &known,
        &admitted,
        "AdmitNewerOpus",
        "admit the newer Opus",
    );
    let mut q = p.clone();
    assert!(q.admit(B2, "conformance", &known_in(&known), 0).is_some());
    assert_transition(
        &model,
        &admitted,
        &with_list(&admitted, &q.ids),
        "AdmitNewerFable",
        "admit the newer Fable",
    );
    let mut avail = admitted.clone();
    avail.insert("av_a2", 1);
    let got = shipped_reader(&p, &evidence_for(&avail)).expect("claude-opus-5-5 is available");
    assert_eq!(got, A2);
    let mut picked = avail.clone();
    picked.insert("sel", 1);
    picked.insert("pick", 2);
    assert_transition(
        &model,
        &avail,
        &picked,
        "Select",
        "select the first available",
    );

    // NEGATIVE CONTROLS on the model: a forged downgrade and a forged
    // cross-family write offered as the writer's own steps are refused.
    let mut down = with_list(
        &known,
        &[
            A0.to_string(),
            A2.to_string(),
            B1.to_string(),
            A1.to_string(),
        ],
    );
    down.insert("sel", 0);
    assert_forged_rejected(
        &model,
        &known,
        &known,
        "OfferDowngrade",
        &[("p_a0", 1), ("p_a2", 2), ("p_b1", 3), ("p_a1", 4)],
        "a downgrade admitted",
    );
    assert_forged_rejected(
        &model,
        &known,
        &admitted,
        "AdmitNewerOpus",
        &[("p_a3", 0), ("p_a0", 1)],
        "a downgrade in the newcomer's place",
    );
    assert_forged_rejected(
        &model,
        &known,
        &known,
        "OfferCrossFamily",
        &[("p_c1", 1), ("p_a2", 2), ("p_b1", 3), ("p_a1", 4)],
        "a family never listed",
    );
    assert!(!model.check_invariant("AutoInsertIsNewerThanTheFamilysBest", &down));
}

/// NEGATIVE CONTROLS for the priority binds: the writer as it was before the
/// newest-listed check, and a reader that answers the head of the list, are
/// each caught by the same enumeration that passes the shipped code.
#[test]
fn upgrade_priority_binds_catch_a_pre_fix_writer_and_a_head_reader() {
    let model = harness_model_priority_model();
    let states = reachable(&model);

    let wrong = writer_disagreements(&model, &states, &pre_fix_admit);
    assert!(!wrong.is_empty(), "the pre-fix writer must be caught");
    assert!(
        wrong.iter().all(|w| w.starts_with(AH)),
        "and caught exactly on the id between the best-ranked and the newest Opus: {wrong:?}"
    );

    let head = |list: &Priority, _: &Evidence| list.ids.first().cloned();
    let wrong = reader_disagreements(&model, &states, &head);
    assert!(
        !wrong.is_empty(),
        "a reader that ignores availability must be caught"
    );
}

/// The history the real writers keep is the provenance the journal needs;
/// `ingest` (the pass's path into `admit`) admits from recommendations only
/// what the model's writer admits.
#[test]
fn upgrade_priority_ingest_admits_only_what_the_model_admits() {
    let model = harness_model_priority_model();
    let mut s = model.init_state();
    s.insert("k_a3", 1);
    let baked = Baked {
        latest_per_family: vec![("opus".into(), A3.into()), ("sonnet".into(), C1.into())],
        models: [A1, A2, A3, B1, C1, A0]
            .iter()
            .map(|id| baked_model(id))
            .collect(),
    };
    let mut p = Priority::seed(0);
    let recs = vec![
        (A0.to_string(), "announcement:old".to_string()),
        (C1.to_string(), "announcement:sonnet".to_string()),
    ];
    let mut all = recs.clone();
    all.extend(models::recommendations(
        &Evidence::default(),
        Some(&baked),
        "2.1.300",
    ));
    let changes = models::ingest(&mut p, &all, Some(&baked), 0);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].id, A3);
    let mut after = with_list(&s, &p.ids);
    after.insert("sel", 0);
    assert_transition(&model, &s, &after, "AdmitNewerOpus", "ingest");
}

fn baked_model(id: &str) -> BakedModel {
    BakedModel {
        id: id.into(),
        family: models::family_version(id).expect("a model id").0,
        display_name: id.into(),
        native_1m: true,
        efforts: vec!["high".into()],
    }
}
