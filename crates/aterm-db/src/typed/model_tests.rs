// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `AlabCells` conformance (docs/DESIGN-alab-lab-model-2026-11-01.md §11 step 12): from every
//! state the model reaches in its first four areas, each step the typed code takes is a model
//! successor. A state is reached by replaying the path that first reached it on a fresh causal
//! history (the hand-built `Index` of the typed tests and the Commons projection kernel,
//! `documents::apply`, as `Log` applies them), so the concrete history behind each state is one
//! the model's own steps made. The fifth area, a preference never limiting a roster, is the
//! harness's (`aterm_agent::harness::alab_roster`'s tests). Every address is synthetic.

use super::{
    cell::{self, CellKey, CellValue, Field},
    keys,
    owned::{self, Op},
    tests::{
        ACCOUNT, ACCOUNT_V1, ADDRESS, Book, FACTS, M1, M2, PK, TABLE, actor, elem, machine_of,
        owned_change, put,
    },
};
use crate::database::Change;
use astream_wire::sync::{Actor, Digest};
use aterm_json::json;
use aterm_spec::interp::State;
use std::collections::{BTreeMap, BTreeSet};

/// The model's areas this crate's code steps; the rest are the harness's.
const AREAS: [i64; 5] = [0, 1, 2, 3, 4];

/// The concrete history behind one model state.
#[derive(Default)]
struct Run {
    /// The register's and the columns' causal history.
    book: Book,
    /// Each writer's latest register write (index 0 is writer A, 1 writer B).
    last: [Option<Digest>; 2],
    /// The other writer's write each writer has received.
    seen: [Option<Digest>; 2],
    /// The owned rows' and the quota's records, and the merges the quota counts.
    ops: Vec<Op>,
    merges: Vec<(Actor, u64)>,
    sequences: BTreeMap<u8, u64>,
    /// Each automatic record's cost: itself and the merges just before it.
    cost: BTreeMap<Digest, i64>,
    joins: i64,
    /// The owner's records and reverts.
    owner: BTreeSet<Digest>,
}

impl Run {
    fn new() -> Self {
        Self {
            book: Book::new(),
            ..Self::default()
        }
    }

    /// Append one record of `writer`'s to the owned and quota history.
    fn record(&mut self, writer: u8, revert: bool, changes: Vec<Change>) -> Digest {
        let sequence = self.sequences.entry(writer).or_default();
        *sequence += 1;
        let mut id = [0; 32];
        id[0] = writer;
        id[1..9].copy_from_slice(&sequence.to_be_bytes());
        self.ops.push(Op {
            id,
            actor: actor(writer),
            sequence: *sequence,
            roster: [7; 32],
            period: 0,
            revert,
            changes,
        });
        id
    }

    /// Writer A's causal-only merge: a sequence the quota reads from the merges, no record.
    fn merge(&mut self) {
        let sequence = self.sequences.entry(1).or_default();
        *sequence += 1;
        self.merges.push((actor(1), *sequence));
    }

    /// Every head of the cells history, for an edit that observed everything.
    fn heads(&self) -> Vec<Digest> {
        self.book.heads.values().map(|(_, id)| *id).collect()
    }
}

/// The register cell (`status`) and set element (`services.codex`) one write puts: `1` is
/// true (an add, `active`), `2` false (a removal, `paused`).
fn register(value: i64) -> Vec<Change> {
    vec![
        put(
            &ACCOUNT,
            PK,
            "status",
            json!(if value == 1 { "active" } else { "paused" }),
        ),
        elem(&ACCOUNT, PK, "services", "codex", value == 1),
    ]
}

/// An automatic value, as a birth or a seed writes one.
fn automatic(n: i64) -> Change {
    cell::change(
        &ADDRESS,
        CellKey {
            pk: &format!("ad-{n}"),
            col: "addr",
            elem: None,
        },
        &CellValue::automatic(1, json!(format!("ad-{n}@example.test"))),
    )
    .unwrap()
}

/// The code's step for each `AlabCells` action of areas 0 to 4 from `state`, whose concrete
/// history is `run`: the write, edit or record the action names, then the typed readers over
/// the result. The register's candidates are read off the projected cell; `lost` and `stale`
/// compare them with the candidates before; `ka` and `kb` are what each writer received, which
/// is the environment's (`SeeA`, `SeeB`).
fn step(state: &State, run: &mut Run, action: &str) -> State {
    let mut next = state.clone();
    let int = |b: bool| i64::from(b);
    match action {
        "ChooseRegister" => {
            next.insert("area", 1);
        }
        "ChooseOwned" => {
            next.insert("area", 2);
        }
        "ChooseColumns" => {
            next.insert("area", 3);
        }
        "ChooseQuota" => {
            next.insert("area", 4);
        }
        "SeeA" | "SeeB" => {
            let (w, knows) = if action == "SeeA" {
                (0, "ka")
            } else {
                (1, "kb")
            };
            run.seen[w] = run.last[1 - w];
            next.insert(knows, 1);
        }
        "WriteA1" | "WriteA2" | "WriteB1" | "WriteB2" => {
            let w = usize::from(action.starts_with("WriteB"));
            let value = if action.ends_with('1') { 1 } else { 2 };
            let [mine, theirs] = if w == 0 { ["a", "b"] } else { ["b", "a"] };
            let knows = state[format!("k{mine}").as_str()] == 1;
            let had = state[format!("c{theirs}").as_str()] == 1;
            let seen: Vec<Digest> = run.seen[w].into_iter().collect();
            let id = run
                .book
                .write(u8::try_from(w + 1).unwrap(), &seen, register(value));
            run.last[w] = Some(id);
            let cells = run.book.cells();
            let candidates: Vec<Digest> = cells[&("account".to_owned(), format!("{PK}.status"))]
                .candidates
                .iter()
                .map(|c| c.operation)
                .collect();
            let holds = |w: usize| run.last[w].is_some_and(|id| candidates.contains(&id));
            let row = cell::project(&ACCOUNT, &cells).rows.remove(PK).unwrap();
            let shown = match row.fields.get("status") {
                None => 0,
                Some(Field::Conflict { candidates, .. }) => {
                    i64::try_from(candidates.len()).unwrap()
                }
                Some(_) => 1,
            };
            let kept = holds(1 - w);
            next.insert("ca", int(holds(0)));
            next.insert("cb", int(holds(1)));
            next.insert(leak(format!("v{mine}")), value);
            next.insert(leak(format!("k{theirs}")), 0);
            next.insert("writes", state["writes"] + 1);
            next.insert("shown", shown);
            next.insert(
                "present",
                int(row.fields["services"].members().contains("codex")),
            );
            if had && !knows && !kept {
                next.insert("lost", 1);
            }
            if had && knows && kept {
                next.insert("stale", 1);
            }
        }
        "OwnA" | "OwnB" | "ForgeB" => {
            let (writer, key, n, count) = match action {
                "OwnA" => (1, keys::uuid32(&M1), state["oa"] + 1, Some("oa")),
                "OwnB" => (2, keys::uuid32(&M2), state["ob"] + 1, Some("ob")),
                _ => (1, keys::uuid32(&M2), 3, None),
            };
            run.record(
                writer,
                false,
                vec![owned_change(&FACTS, &key, u64::try_from(n).unwrap())],
            );
            match count {
                Some(count) => {
                    next.insert(count, n);
                }
                None => {
                    next.insert("forgery", 1);
                }
            }
            let read = owned::read_owned(&FACTS, &run.ops, machine_of);
            let at = |machine: [u8; 16]| {
                read.rows
                    .get(&keys::uuid32(&machine))
                    .map(|row| (row.machine, row.value["n"].as_i64().unwrap()))
            };
            next.insert("pa", at(M1).map_or(0, |(_, n)| n));
            next.insert("pb", at(M2).map_or(0, |(_, n)| n));
            next.insert(
                "forged",
                int(at(M2).is_some_and(|(machine, _)| machine == M1)),
            );
        }
        "NewRegion" | "OldEdit" | "NewEdit" => {
            let heads = run.heads();
            let (writer, change) = match action {
                "NewRegion" => (1, put(&ACCOUNT, PK, "region", json!("mainland-cn"))),
                "OldEdit" => (2, put(&ACCOUNT_V1, PK, "status", json!("paused"))),
                _ => (1, put(&ACCOUNT, PK, "status", json!("banned"))),
            };
            run.book.write(writer, &heads, vec![change]);
            if action == "NewRegion" {
                next.insert("region", 1);
            } else {
                next.insert("writes", state["writes"] + 1);
            }
            let cells = run.book.cells();
            let new = cell::project(&ACCOUNT, &cells).rows.remove(PK);
            let old = cell::project(&ACCOUNT_V1, &cells).rows.remove(PK);
            next.insert(
                "kept",
                int(new.is_some_and(|row| row.get("region").is_some())),
            );
            next.insert(
                "raw",
                int(old
                    .is_some_and(|row| row.raw.contains("region") && row.get("region").is_some())),
            );
        }
        "Merge" => {
            run.merge();
            run.joins += 1;
            next.insert("joins", run.joins);
        }
        "Automatic" | "Owner" | "Revert" => {
            let n = i64::try_from(run.ops.len()).unwrap();
            if action == "Automatic" {
                let id = run.record(1, false, vec![automatic(n)]);
                run.cost.insert(id, 1 + run.joins);
            } else {
                // A revert restores an automatic value: still the owner's verb.
                let change = if action == "Owner" {
                    put(&ACCOUNT, PK, "units", json!(n))
                } else {
                    automatic(n)
                };
                let id = run.record(1, action == "Revert", vec![change]);
                run.owner.insert(id);
                next.insert("owner", state["owner"] + 1);
            }
            run.joins = 0;
            next.insert("joins", 0);
            sync_quota(run, &mut next, &TABLE);
        }
        _ => panic!("no code step for {action}"),
    }
    next
}

/// The quota's reading of `run` into `next`: what A's automatic records cost, the cost of the
/// ones that project, and the owner's records it holds inert.
fn sync_quota(run: &Run, next: &mut State, table: &super::Table) {
    let quota = owned::quota(table, &run.ops, &run.merges);
    next.insert(
        "spent",
        i64::try_from(quota.spent.get(&actor(1)).copied().unwrap_or(0)).unwrap(),
    );
    next.insert(
        "live",
        run.cost
            .iter()
            .filter(|(id, _)| !quota.over.contains(*id))
            .map(|(_, cost)| cost)
            .sum(),
    );
    next.insert(
        "oover",
        i64::try_from(run.owner.intersection(&quota.over).count()).unwrap(),
    );
}

/// A state key built at run time, for the four writer-named variables.
fn leak(name: String) -> &'static str {
    match name.as_str() {
        "va" => "va",
        "vb" => "vb",
        "ka" => "ka",
        "kb" => "kb",
        _ => panic!("{name} is no register variable"),
    }
}

/// The `AlabCells` projection of `action` from the state `path` reaches: [`step`] on the history
/// that path builds.
pub(crate) fn cells_transition(path: &[&'static str], action: &str) -> State {
    let (state, mut run) = replay(path);
    step(&state, &mut run, action)
}

/// The state and history after `path` from the model's initial state.
fn replay(path: &[&'static str]) -> (State, Run) {
    let model = aterm_models_db::alab_cells_model();
    let mut state: State = model.init_state();
    let mut run = Run::new();
    for action in path {
        state = step(&state, &mut run, action);
    }
    (state, run)
}

/// Every state the model reaches in areas 0 to 4, with the path that first reaches it, each
/// step checked to be a model successor.
fn walk(
    model: &aterm_spec::derive::Model,
    check: impl Fn(&State, &'static str, &State),
) -> BTreeMap<State, Vec<&'static str>> {
    let init = model.init_state();
    let mut reached = BTreeMap::from([(init.clone(), Vec::new())]);
    let mut queue = vec![init];
    while let Some(state) = queue.pop() {
        let path = reached[&state].clone();
        for action in &model.actions {
            if !model.action_enabled(action.name, &state)
                || action.name == "ChoosePreference"
                || !AREAS.contains(&state["area"])
            {
                continue;
            }
            let (replayed, _) = replay(&path);
            assert_eq!(replayed, state, "replaying {path:?}");
            let next = cells_transition(&path, action.name);
            check(&state, action.name, &next);
            if !reached.contains_key(&next) {
                let mut longer = path.clone();
                longer.push(action.name);
                reached.insert(next.clone(), longer);
                queue.push(next);
            }
        }
    }
    reached
}

/// Every step the typed code takes is an `AlabCells` successor, from every state the model
/// reaches in the register, owned-row, column and quota areas, and the walk reaches every such
/// state.
/// NEGATIVE CONTROLS: a write that drops the candidate it did not observe (the whole-register
/// write `documents::apply` refuses) breaks `ObservedOnly` and is no successor; a resolver that
/// names Mac B for every signer projects A's row at B's key and is no `ForgeB` successor; and a
/// quota of three lets a third automatic record project, no `Automatic` successor.
#[test]
fn alab_cells_conforms() {
    let model = aterm_models_db::alab_cells_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let reached = walk(&model, |state, action, next| {
        assert!(
            model.successors(action, state).contains(next),
            "{action} from {state:?} steps to {next:?}"
        );
    });
    let expected = aterm_spec::interp::reachable_states(&model)
        .into_iter()
        .filter(|state| state["area"] < 5)
        .count();
    assert_eq!(reached.len(), expected);

    // A whole-register write: B's unobserved candidate dropped.
    let (state, path) = reached
        .iter()
        .find(|(state, _)| state["area"] == 1 && state["ka"] == 0 && state["cb"] == 1)
        .expect("a state where A has not observed B's candidate");
    let (_, mut run) = replay(path);
    let mut clobbered = step(state, &mut run, "WriteA1");
    assert_eq!(
        clobbered["cb"], 1,
        "the code keeps the unobserved candidate"
    );
    clobbered.insert("cb", 0);
    clobbered.insert("lost", 1);
    clobbered.insert("shown", 1);
    assert!(!model.check_invariant("ObservedOnly", &clobbered));
    assert!(!model.successors("WriteA1", state).contains(&clobbered));

    // A resolver that takes every signer for Mac B.
    let (state, path) = reached
        .iter()
        .find(|(state, _)| state["area"] == 2 && state["forgery"] == 0)
        .unwrap();
    let (_, mut run) = replay(path);
    let mut forged = step(state, &mut run, "ForgeB");
    let read = owned::read_owned(&FACTS, &run.ops, |_, _| Some(M2));
    let row = &read.rows[&keys::uuid32(&M2)];
    forged.insert("pb", row.value["n"].as_i64().unwrap());
    forged.insert("forged", i64::from(row.operation[0] == 1));
    assert!(!model.successors("ForgeB", state).contains(&forged));

    // A quota of three.
    let (state, path) = reached
        .iter()
        .find(|(state, _)| state["area"] == 4 && state["spent"] == 2 && state["joins"] == 0)
        .expect("a state with the quota spent");
    let (_, mut run) = replay(path);
    let mut generous = step(state, &mut run, "Automatic");
    let roomy = super::Table {
        self_quota: 3,
        ..TABLE
    };
    sync_quota(&run, &mut generous, &roomy);
    assert!(!model.successors("Automatic", state).contains(&generous));
}
