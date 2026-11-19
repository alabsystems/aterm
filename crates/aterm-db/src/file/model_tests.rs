// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `SharedFile` conformance (docs/DESIGN-shared-files-2026-11-11.md §8): the reading and the
//! writing walks of the model, taken on a stand-in lab's real stores (a registry of machines
//! alone, so no read there enters the authority judgment), and the authority walk,
//! taken on a causal history of a collection that opts into the authority rules. Each step runs
//! the code its anchor names (`aterm_file::File::read`, the accept inside `File::write`,
//! [`super::materialize::pass`], [`super::publish::pass`], [`crate::typed::authority::read`]) in
//! an evidence window of its own, and the state measured after it must be a successor the model
//! has for that action from the state measured before it. The environment's steps (a peer's
//! record, a record arriving, the fence) are made through the registry and writer succession's
//! own seam.
//!
//! What a walk measures, never assumes: the outbox, held list and done markers on disk; the
//! table's records and the register's candidates; the table files' bytes under a fence, where
//! the strict reads refuse; the snapshot's frontier and state word; and the custody seam
//! across a read.

use super::{
    MACHINES,
    limits::{self, Limit, Reading, Seats},
    publish, seam,
    tests::{NOW, SEAT, Stand, publish_pass},
    write,
};
use crate::{
    alab::{Registry, registry::MACHINE},
    att, generation,
    typed::{
        authority::{
            seam::{self as rules, Control},
            tests::{self as ruled, History},
        },
        cell::CellValue,
        grid::tests::put,
    },
};
use astream_wire::sync::Digest;
use aterm_file::{Why, Word, outbox};
use aterm_json::json;
use aterm_spec::{derive::Model, interp::State, xref::StepEvidence};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One walk's stand-in lab and what the walk has seen of it.
struct Walk {
    stand: Stand,
    evidence: StepEvidence,
    area: i64,
    /// Writes `File::write` accepted.
    accepted: i64,
    /// Every accepted intent: its id and the candidates its author saw.
    intents: Vec<(String, Vec<Digest>)>,
    /// The operations another writer put on the register an intent names.
    peer: Vec<Digest>,
    /// The operations that arrived in the replica during the walk ("Grow").
    grown: Vec<Digest>,
    /// The table files' bytes when the fence fell.
    fenced: Option<BTreeMap<PathBuf, Vec<u8>>>,
    /// The highest frontier any pass recorded.
    high: i64,
    reads: i64,
    answered: i64,
    opens: i64,
    authored: i64,
    /// The rotation area's readings, this Mac's and the peer's, as their `ai/usage` rows say.
    here: Option<Reading>,
    there: Option<Reading>,
    /// The fold of both readings in each Mac's order: this Mac's, then the peer's.
    folds: Option<(Limit, Limit)>,
}

/// The rotation walk's account, and its two week instances: the model's `1` and `2`.
const ROTATED: &str = "ac-00000000000000a1";
const OLD_WEEK: u64 = NOW + 86_400_000;
const NEW_WEEK: u64 = OLD_WEEK + 7 * 86_400_000;

/// One Mac's reading of the walk's account, as its `ai/usage` row would say it: the model's
/// week `week` and percent `percent`, read at `read_at` by that Mac's clock.
fn reading(uuid: &str, week: i64, percent: i64, read_at: u64) -> Reading {
    let reset = if week == 2 { NEW_WEEK } else { OLD_WEEK };
    let mut row = aterm_file::Row {
        pk: format!("{ROTATED}.{uuid}"),
        live: true,
        ..aterm_file::Row::default()
    };
    for (column, value) in [
        ("account", json!(ROTATED)),
        ("w7d", json!(percent * 100)),
        ("w7d_reset", json!(reset)),
        ("wall", json!("none")),
        ("read_at", json!(read_at)),
    ] {
        row.values.insert(column.to_owned(), value);
    }
    Reading::of_row(&row).expect("a usage row's key")
}

/// The model's week and percent of one fold of the walk's account.
fn folded(limit: &Limit) -> (i64, i64) {
    let (percent, reset) = limit.windows[1];
    let week = match reset {
        Some(NEW_WEEK) => 2,
        Some(OLD_WEEK) => 1,
        other => panic!("a fold names a reset no reading stated: {other:?}"),
    };
    (week, percent.map_or(0, |p| (p / 100) as i64))
}

/// Two uuids of two Macs: this one's and the peer's. The peer's clock reads an hour behind
/// this Mac's, so a fold ordered by clock would take this Mac's reading on both.
const HERE: &str = "0000000000000000000000000000000a";
const THERE: &str = "0000000000000000000000000000000b";

impl Walk {
    fn new() -> Self {
        Self {
            stand: Stand::machines_only(),
            evidence: StepEvidence::new("SharedFile"),
            area: 0,
            accepted: 0,
            intents: Vec::new(),
            peer: Vec::new(),
            grown: Vec::new(),
            fenced: None,
            high: 0,
            reads: 0,
            answered: 0,
            opens: 0,
            authored: 0,
            here: None,
            there: None,
            folds: None,
        }
    }

    fn tables(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        crate::tests::files(&self.stand.paths().dir.join("commons/tables"))
    }

    /// The table's eligible operations, where custody answers.
    fn operations(&self) -> Vec<att::Entry> {
        att::Handle::at(self.stand.paths().clone(), &att::REGISTRY)
            .history()
            .unwrap()
    }

    /// The register the walk's intents write: the stand-in machine's note.
    fn register(&self) -> String {
        format!("{}.note", self.stand.machine)
    }

    fn candidates(&self) -> Vec<Digest> {
        att::Handle::at(self.stand.paths().clone(), &att::REGISTRY)
            .view()
            .unwrap()
            .cells
            .get(&(MACHINE.name.to_owned(), self.register()))
            .map(|cell| cell.candidates.iter().map(|c| c.operation).collect())
            .unwrap_or_default()
    }

    /// Write one cell through the registry as another writer does, and name its operation.
    fn registry_write(&self, column: &str, text: &str) -> Digest {
        let before: Vec<Digest> = self.operations().iter().map(|e| e.operation).collect();
        Registry::at(self.stand.paths().clone())
            .write(vec![vec![put(
                &MACHINE,
                &self.stand.machine,
                column,
                json!(text),
            )]])
            .unwrap();
        self.operations()
            .iter()
            .map(|entry| entry.operation)
            .find(|operation| !before.contains(operation))
            .expect("the registry write appended a record")
    }

    fn apply(&mut self, action: &'static str) {
        let stand = &self.stand;
        let file = stand.file(&MACHINES);
        match action {
            "ChooseRead" => self.area = 1,
            "ChooseWrite" => {
                self.area = 2;
                // What a writer reads its words against: one pass before the walk's first step.
                for outcome in stand.pass(false) {
                    assert_eq!(outcome.word, Word::Live);
                }
            }
            "Fence" => {
                generation::file_seams::fence_pending(stand.paths(), &stand.lab.bootstrap(SEAT));
                self.fenced = Some(self.tables());
            }
            "Grow" => {
                let operation = self.registry_write("location", "the desk");
                self.grown.push(operation);
            }
            "PeerWrite" => {
                let operation = self.registry_write("note", "a peer wrote this");
                self.peer.push(operation);
            }
            "Materialize" => {
                let before = self.tables();
                let outcomes = self.evidence.step("Materialize", || stand.pass(false));
                assert!(outcomes.iter().all(|outcome| outcome.problem.is_none()));
                self.authored |= i64::from(self.tables() != before);
                let sfr = self.snapshot_frontier();
                self.high = self.high.max(sfr);
            }
            "Read" => {
                let before = seam::custody_opens();
                let read = self.evidence.step("Read", || file.read());
                self.reads += 1;
                // It answered: a status, and the rows of whatever snapshot is here.
                self.answered += i64::from(
                    read.status.problem.is_none()
                        && (read.snapshot.is_none() || !read.rows().is_empty()),
                );
                self.opens += i64::from(seam::custody_opens() != before);
            }
            "Accept" => {
                let text = format!("the owner's note {}", self.accepted + 1);
                let draft =
                    write::draft_set(stand.paths(), MACHINES.name, "m7", &[("note", Some(&text))])
                        .unwrap()
                        .expect("the cell does not say that yet");
                // What the author saw: an operation, or the record of an earlier intent of
                // this Mac's that is published and not in the snapshot yet (`intent:<id>`).
                let done = file.outbox().done;
                let expected: Vec<Digest> = draft.edits[0]
                    .expected
                    .iter()
                    .filter_map(|entry| match entry.strip_prefix(outbox::INTENT) {
                        None => outbox::digest(entry),
                        Some(id) => done
                            .iter()
                            .find(|done| done.intent.id == id)
                            .and_then(|done| done.op.as_deref())
                            .and_then(outbox::digest),
                    })
                    .collect();
                let act = seam::owner();
                let intent = self
                    .evidence
                    .step("Accept", || file.write(&act, draft, NOW))
                    .unwrap();
                self.accepted += 1;
                self.intents.push((intent.intent.id, expected));
            }
            "Sign" => {
                // The pass is killed once it has signed, so its record and its dequeue are
                // two steps, as the model has them.
                publish::seam::kill_at(publish::seam::Stop::AfterSign);
                let _ = self
                    .evidence
                    .step("Sign", || publish_pass(stand.paths(), &stand.config));
            }
            "Done" => {
                let _ = self
                    .evidence
                    .step("Done", || publish_pass(stand.paths(), &stand.config));
            }
            "Hold" => {
                let _ = self
                    .evidence
                    .step("Hold", || publish_pass(stand.paths(), &stand.config));
            }
            "ChooseRotation" => self.area = 5,
            // A Mac's reading is the environment: each states its own row.
            "HereOldWeek" => self.here = Some(reading(HERE, 1, 2, NOW)),
            "HereNewWeek" => self.here = Some(reading(HERE, 2, 1, NOW)),
            "PeerOldWeek" => self.there = Some(reading(THERE, 1, 1, NOW - 3_600_000)),
            "PeerNewWeek" => self.there = Some(reading(THERE, 2, 2, NOW - 3_600_000)),
            "Fold" => {
                let (here, there) = (self.here.clone().unwrap(), self.there.clone().unwrap());
                // Each Mac holds the same two rows, in the order its replica read them.
                let folds = self.evidence.step("Fold", || {
                    (
                        limits::fold(&[here.clone(), there.clone()], &Seats::default()),
                        limits::fold(&[there, here], &Seats::default()),
                    )
                });
                self.folds = Some((folds.0[ROTATED].clone(), folds.1[ROTATED].clone()));
            }
            other => panic!("the walk has no step {other}"),
        }
    }

    /// How many of the records that arrived during the walk the snapshot was projected from: the
    /// operations its history names, which the pass that wrote the snapshot wrote beside it.
    /// The frontier names stores, not records, so it is the history that says which.
    fn snapshot_frontier(&self) -> i64 {
        let file = self.stand.file(&MACHINES);
        if file.frontier().is_none() {
            return 0;
        }
        let projected: Vec<String> = file
            .history()
            .iter()
            .filter_map(|entry| entry.get("op").and_then(aterm_json::Value::as_str))
            .map(str::to_owned)
            .collect();
        self.grown
            .iter()
            .filter(|operation| projected.contains(&aterm_codec::hex::encode_lower(*operation)))
            .count() as i64
    }

    /// The model's state as the stores say it now, `before` supplying what this walk's area
    /// does not touch.
    fn state(&self, before: &State) -> State {
        let mut state = before.clone();
        let file = self.stand.file(&MACHINES);
        let fenced = generation::fenced(self.stand.paths()).unwrap();
        state.insert("area", self.area);
        state.insert("custody", i64::from(fenced));
        if self.area == 1 {
            let status = file.status();
            if !fenced {
                // Under a fence the strict read refuses; nothing arrives there in this walk.
                let here: Vec<Digest> = self.operations().iter().map(|e| e.operation).collect();
                let arrived = self.grown.iter().filter(|op| here.contains(op)).count();
                state.insert("rfr", arrived as i64);
            }
            state.insert("sera", file.frontier().map_or(0, |f| f.era as i64));
            state.insert("sfr", self.snapshot_frontier());
            state.insert("hfr", self.high);
            state.insert(
                "word",
                match status.word {
                    Word::Never => 0,
                    Word::Live => 1,
                    _ => 2,
                },
            );
            state.insert("src", i64::from(status.why == Some(Why::Succession)));
            state.insert("authored", self.authored);
            state.insert("reads", self.reads);
            state.insert("answered", self.answered);
            state.insert("opens", self.opens);
        }
        if self.area == 5 {
            let mine = |reading: &Option<Reading>| {
                reading.as_ref().map_or((0, 0), |reading| {
                    let (percent, reset) = reading.windows[1];
                    let week = i64::from(reset == Some(NEW_WEEK)) + 1;
                    (week, percent.map_or(0, |p| (p / 100) as i64))
                })
            };
            let ((ra, pa), (rb, pb)) = (mine(&self.here), mine(&self.there));
            for (name, value) in [("ra", ra), ("pa", pa), ("rb", rb), ("pb", pb)] {
                state.insert(name, value);
            }
            if let Some((one, two)) = &self.folds {
                let ((fr1, fp1), (fr2, fp2)) = (folded(one), folded(two));
                state.insert("folded", 1);
                for (name, value) in [("fr1", fr1), ("fp1", fp1), ("fr2", fr2), ("fp2", fp2)] {
                    state.insert(name, value);
                }
            }
        }
        if self.area == 2 {
            let outbox = file.outbox();
            let queued = |id: &str| outbox.queued.iter().any(|intent| intent.id == id);
            state.insert("acc", self.accepted);
            state.insert("q", outbox.queued.len() as i64);
            state.insert("h", outbox.held.len() as i64);
            state.insert(
                "p",
                outbox
                    .done
                    .iter()
                    .filter(|done| !queued(&done.intent.id))
                    .count() as i64,
            );
            if let Some(at_fence) = &self.fenced {
                // The strict reads refuse under the fence: the table files say whether the
                // fenced writer signed.
                state.insert("fsig", i64::from(self.tables() != *at_fence));
            } else {
                let operations = self.operations();
                let candidates = self.candidates();
                let record_of = |id: &str| -> Vec<Digest> {
                    let mut request = [0; 16];
                    assert!(aterm_codec::hex::decode_into(id.as_bytes(), &mut request));
                    operations
                        .iter()
                        .filter(|entry| entry.request == Some(request))
                        .map(|entry| entry.operation)
                        .collect()
                };
                // Records that carry an intent still in the outbox.
                let signed: Vec<Digest> = self
                    .intents
                    .iter()
                    .filter(|(id, _)| queued(id))
                    .flat_map(|(id, _)| record_of(id))
                    .collect();
                state.insert("recs", signed.len() as i64);
                state.insert(
                    "mine",
                    i64::from(signed.iter().any(|op| candidates.contains(op))),
                );
                // A candidate the first queued intent's author never saw, and not its own.
                let moved =
                    self.intents
                        .iter()
                        .find(|(id, _)| queued(id))
                        .is_some_and(|(_, expected)| {
                            candidates
                                .iter()
                                .any(|c| !expected.contains(c) && !signed.contains(c))
                        });
                state.insert("cb", i64::from(moved));
                // A record of this Mac's stands where a peer's candidate its author never saw
                // stood: that candidate was written over.
                let lost = self.intents.iter().any(|(id, expected)| {
                    record_of(id).iter().any(|op| candidates.contains(op))
                        && self
                            .peer
                            .iter()
                            .any(|peer| !candidates.contains(peer) && !expected.contains(peer))
                });
                state.insert("lost", i64::from(lost));
            }
        }
        state
    }
}

/// What a walk does: take one of the model's steps on the code, and measure the model's state.
trait Steps {
    fn apply(&mut self, action: &'static str);
    /// The model's state as the code says it now, `before` supplying what the walk's area does
    /// not touch.
    fn state(&self, before: &State) -> State;
}

impl Steps for Walk {
    fn apply(&mut self, action: &'static str) {
        Walk::apply(self, action);
    }
    fn state(&self, before: &State) -> State {
        Walk::state(self, before)
    }
}

/// The authority walk: one row of `typed::authority`'s scratch collection, written by the owner
/// (writer 0), by a peer that births the row before it hears the owner (writer 1), and by a
/// Mac whose automatic paths and importer hear everything (writer 2), and read as every read of
/// a collection that opts in reads it. The note is an Owner column, the status a Birth column
/// the owner may set, the realm a fixed one and the label one only an owner's verb writes.
struct AuthorityWalk {
    history: History,
    evidence: StepEvidence,
    area: i64,
    /// Names the importer and the automatic paths wrote, so they take turns.
    names: usize,
}

impl AuthorityWalk {
    fn new() -> Self {
        Self {
            history: History::new(),
            evidence: StepEvidence::new("SharedFile"),
            area: 0,
            names: 0,
        }
    }

    /// Each candidate of `column`'s register, as the read admits it.
    fn candidates(cells: &crate::typed::cell::Cells, column: &str) -> Vec<CellValue> {
        ruled::field(cells, column).map_or_else(Vec::new, |field| {
            field.candidates().iter().map(|c| c.value.clone()).collect()
        })
    }
}

impl Steps for AuthorityWalk {
    fn apply(&mut self, action: &'static str) {
        let all = self.history.all();
        let (writer, seen, changes) = match action {
            "ChooseAuthority" => {
                self.area = 3;
                return;
            }
            "OwnerWrites" => (0, all, vec![ruled::owner("note", json!("the owner's"))]),
            "AutomaticWrites" => (
                2,
                all,
                vec![ruled::automatic(2, "note", json!("automatic"))],
            ),
            // The peer has heard nothing: its birth is concurrent with what the owner wrote.
            "Birth" => (1, Vec::new(), ruled::birth(ruled::automatic2)),
            "OwnerSets" => (0, all, vec![ruled::owner("status", json!("active"))]),
            "Rewrite" => (0, all, vec![ruled::owner("realm", json!("codex"))]),
            "OwnerNames" => (0, all, vec![ruled::owner("label", json!("work"))]),
            // An import's name, then an older peer's default one, written at version 1.
            "ImportNames" => {
                self.names += 1;
                let name = if self.names % 2 == 1 {
                    ruled::imported("label", json!("imported"))
                } else {
                    ruled::automatic(1, "label", json!("claude-1a2b3c"))
                };
                (2, all, vec![name])
            }
            other => panic!("the authority walk has no step {other}"),
        };
        let history = &mut self.history;
        self.evidence.step(action, || {
            history.write(writer, &seen, changes);
            history.read(&ruled::TABLE)
        });
    }

    fn state(&self, before: &State) -> State {
        let mut state = before.clone();
        state.insert("area", self.area);
        if self.area != 3 {
            return state;
        }
        let (cells, _) = self.history.read(&ruled::TABLE);
        let owners = |values: &[CellValue]| values.iter().any(|v| !v.a && v.s.is_none());
        let automatic = |values: &[CellValue]| values.iter().any(|v| v.a || v.s.is_some());
        let note = Self::candidates(&cells, "note");
        state.insert("oc", i64::from(owners(&note)));
        state.insert(
            "proj",
            if automatic(&note) {
                2
            } else {
                i64::from(!note.is_empty())
            },
        );
        let status = Self::candidates(&cells, "status");
        state.insert("birth", i64::from(ruled::field(&cells, "row").is_some()));
        state.insert("own", i64::from(owners(&status)));
        state.insert("bshown", i64::from(status.iter().any(|v| v.a)));
        state.insert(
            "ident",
            match ruled::shown(&cells, "realm") {
                None => 0,
                Some(realm) if realm == json!("claude") => 1,
                Some(_) => 2,
            },
        );
        let label = Self::candidates(&cells, "label");
        state.insert("named", i64::from(owners(&label)));
        state.insert(
            "vlabel",
            if automatic(&label) {
                2
            } else {
                i64::from(!label.is_empty())
            },
        );
        state
    }
}

/// Run `path` on `walk`, each step held to the model: the state measured after it is a
/// successor the model has for that action. Returns the walk and its last state.
fn run<W: Steps>(model: &Model, mut walk: W, path: &[&'static str]) -> (W, State) {
    let mut state = walk.state(&model.init_state());
    assert_eq!(
        state,
        model.init_state(),
        "the fixture starts at the model's start"
    );
    for action in path {
        walk.apply(action);
        let after = walk.state(&state);
        assert!(
            model.successors(action, &state).contains(&after),
            "{path:?} at {action}: {state:?} -> {after:?}"
        );
        state = after;
    }
    (walk, state)
}

/// The `SharedFile` projection of `action` from the state `path` reaches, in the writing area:
/// what the anchors on the accept and on the publisher name.
pub(crate) fn write_transition(path: &[&'static str], action: &'static str) -> State {
    let model = aterm_models_db::shared_file_model();
    let (mut walk, state) = run(&model, Walk::new(), path);
    walk.apply(action);
    walk.state(&state)
}

/// The `SharedFile` projection of `action` from the state `path` reaches, in the reading area:
/// what the anchors on the materializer's pass and on `File::read` name.
pub(crate) fn read_transition(path: &[&'static str], action: &'static str) -> State {
    write_transition(path, action)
}

/// The `SharedFile` projection of `action` from the state `path` reaches, in the rotation area:
/// what the anchor on `limits::fold` names.
pub(crate) fn rotation_transition(path: &[&'static str], action: &'static str) -> State {
    write_transition(path, action)
}

/// The `SharedFile` projection of `action` from the state `path` reaches, in the authority area:
/// what the anchors on `typed::authority`'s judgment name.
pub(crate) fn authority_transition(path: &[&'static str], action: &'static str) -> State {
    let model = aterm_models_db::shared_file_model();
    let (mut walk, state) = run(&model, AuthorityWalk::new(), path);
    walk.apply(action);
    walk.state(&state)
}

/// The authority walks: an automatic note before and after the owner's; a birth the owner then
/// sets and rewrites, and the same birth arriving after the owner set the row's status; and
/// names an import and an older peer write, before and after the owner's.
const AUTHORITY_PATHS: [&[&str]; 4] = [
    &[
        "ChooseAuthority",
        "AutomaticWrites",
        "OwnerWrites",
        "AutomaticWrites",
    ],
    &["ChooseAuthority", "Birth", "OwnerSets", "Rewrite"],
    &["ChooseAuthority", "OwnerSets", "Birth", "Rewrite"],
    &[
        "ChooseAuthority",
        "ImportNames",
        "ImportNames",
        "OwnerNames",
        "ImportNames",
    ],
];

/// Every law of `model` holds in the state `path` ended in.
fn holds_every_law(model: &Model, path: &[&str], state: &State) {
    for law in &model.invariants {
        assert!(
            model.check_invariant(law.name, state),
            "{path:?} ends in a state that breaks {}",
            law.name
        );
    }
}

/// The shared files' code refines its model along the reading, the writing, the rotation and
/// the authority walks: a read before anything is materialized, after a pass, after a record
/// arrived, and under a fence; a write accepted, signed and done, twice over; a write whose
/// register a peer moved, held; a write accepted under a fence, which no pass signs; two Macs'
/// readings of one account's week, in each order, folded; and the authority walks
/// ([`AUTHORITY_PATHS`]). Every anchored action is driven in a window that enters its own
/// function.
///
/// NEGATIVE CONTROL: with the publisher ignoring what the intent's author saw
/// (`publish::seam::ignore_expected`), the pass signs over the peer's candidate. The stores
/// then say `recs == 1 && lost == 1`, a state the model has no `Sign` step to (its `Sign` is
/// not enabled while a peer's candidate stands) and that breaks `PublishObservedOnly`. With the
/// fold taking the reading whose Mac's clock read last (`limits::seam::fold_by_clock`, the
/// model's `ClockOrdersReadings`), this Mac's old week wins over the peer's new one on both
/// Macs: `fr1 == fr2 == 1`, a state the model has no `Fold` step to. With
/// `typed::authority::read` judging every read of a table one of whose collections opts in (its
/// return where no write names one removed), the reading and writing walks' passes enter the
/// judgment the authority steps are anchored on: `` stray: {"judge": {"Done", "Hold",
/// "Materialize", "Sign"}} ``.
#[test]
fn shared_file_conforms_and_a_publish_that_ignores_expected_is_detected() {
    let model = aterm_models_db::shared_file_model();
    let paths: [&[&'static str]; 12] = [
        &["ChooseRead", "Read"],
        &["ChooseRead", "Materialize", "Read"],
        &["ChooseRead", "Grow", "Materialize", "Read"],
        &["ChooseRead", "Materialize", "Grow", "Read"],
        &["ChooseRead", "Fence", "Materialize", "Read"],
        &[
            "ChooseWrite",
            "Accept",
            "Sign",
            "Done",
            "Accept",
            "Sign",
            "Done",
        ],
        &["ChooseWrite", "Accept", "PeerWrite", "Hold"],
        &["ChooseWrite", "Fence", "Accept"],
        &["ChooseRotation", "HereOldWeek", "PeerNewWeek", "Fold"],
        &["ChooseRotation", "HereNewWeek", "PeerOldWeek", "Fold"],
        &["ChooseRotation", "HereOldWeek", "PeerOldWeek", "Fold"],
        &["ChooseRotation", "PeerNewWeek", "HereNewWeek", "Fold"],
    ];
    let mut evidence = StepEvidence::new("SharedFile");
    for path in paths {
        let (walk, state) = run(&model, Walk::new(), path);
        if path == ["ChooseWrite", "Fence", "Accept"] {
            // A pass under the fence is no step: the intent stays, and nothing is signed.
            let outcomes = publish_pass(walk.stand.paths(), &walk.stand.config);
            assert!(
                matches!(
                    outcomes
                        .iter()
                        .find_map(|outcome| outcome.answer(&walk.intents[0].0)),
                    Some(publish::Answer::Queued { why }) if why.starts_with("succession: ")
                ),
                "{outcomes:?}"
            );
            assert_eq!(walk.state(&state), state, "a fenced pass changes nothing");
            assert_eq!((state["q"], state["fsig"]), (1, 0));
        }
        holds_every_law(&model, path, &state);
        evidence.absorb(walk.evidence);
    }
    for path in AUTHORITY_PATHS {
        let (walk, state) = run(&model, AuthorityWalk::new(), path);
        holds_every_law(&model, path, &state);
        evidence.absorb(walk.evidence);
    }
    let audit = evidence.audit();
    assert!(audit.is_complete(), "{audit:#?}");
    assert_eq!(
        audit.actions.iter().copied().collect::<Vec<_>>(),
        [
            "Accept",
            "AutomaticWrites",
            "Birth",
            "Done",
            "Fold",
            "Hold",
            "ImportNames",
            "Materialize",
            "OwnerNames",
            "OwnerSets",
            "OwnerWrites",
            "Read",
            "Rewrite",
            "Sign"
        ]
    );

    let (mut walk, state) = run(&model, Walk::new(), &["ChooseWrite", "Accept", "PeerWrite"]);
    let blind = {
        let _blind = publish::seam::ignore_expected();
        walk.apply("Sign");
        walk.state(&state)
    };
    assert_eq!(
        (blind["recs"], blind["lost"]),
        (1, 1),
        "the negative control must sign over the peer's candidate"
    );
    assert!(!model.successors("Sign", &state).contains(&blind));
    assert!(!model.check_invariant("PublishObservedOnly", &blind));

    // The fold takes the peer's new week on both Macs, though this Mac's clock read last.
    let fold = rotation_transition(&["ChooseRotation", "HereOldWeek", "PeerNewWeek"], "Fold");
    assert_eq!(
        (fold["fr1"], fold["fp1"], fold["fr2"], fold["fp2"]),
        (2, 2, 2, 2)
    );
    let (mut walk, state) = run(
        &model,
        Walk::new(),
        &["ChooseRotation", "HereOldWeek", "PeerNewWeek"],
    );
    let by_clock = {
        let _clock = limits::seam::fold_by_clock();
        walk.apply("Fold");
        walk.state(&state)
    };
    assert_eq!(
        (by_clock["fr1"], by_clock["fr2"]),
        (1, 1),
        "the negative control must take the reading its clock read last"
    );
    assert!(!model.successors("Fold", &state).contains(&by_clock));
    // The same walk with the comparison on holds the intent instead.
    let held = write_transition(&["ChooseWrite", "Accept", "PeerWrite"], "Hold");
    assert_eq!((held["h"], held["recs"], held["lost"]), (1, 0, 0));
    // And the materializer's read of that table is still a pass that authors nothing.
    let read = read_transition(&["ChooseRead", "Materialize"], "Read");
    assert_eq!(
        (read["answered"], read["opens"], read["authored"]),
        (1, 0, 0)
    );
}

/// Each law of the authority area holds because the read leaves out what the column's writer
/// would not write: walked under a read that admits every write, each walk ends in the state
/// its law forbids, one the model has no step to.
/// NEGATIVE CONTROL: this test is the control. With `typed::authority` judging nothing
/// (`Control::AdmitAll`), the automatic note projects (`proj == 2`), the birth's status shows
/// beside the owner's (`own == 1 && bshown == 1`), the rewrite takes (`ident == 2`) and the
/// import's name projects (`vlabel == 2`).
#[test]
fn a_read_that_admits_every_write_breaks_each_authority_law() {
    let model = aterm_models_db::shared_file_model();
    for (path, law, measured) in [
        (
            &["ChooseAuthority", "AutomaticWrites"][..],
            "OwnerOverAutomatic",
            ("proj", 2),
        ),
        (
            &["ChooseAuthority", "OwnerSets", "Birth"][..],
            "OwnerOverBirth",
            ("bshown", 1),
        ),
        (
            &["ChooseAuthority", "Birth", "Rewrite"][..],
            "FixedIdentity",
            ("ident", 2),
        ),
        (
            &["ChooseAuthority", "ImportNames"][..],
            "OwnerVerbOnly",
            ("vlabel", 2),
        ),
    ] {
        let (before, last) = path.split_at(path.len() - 1);
        let (mut walk, state) = run(&model, AuthorityWalk::new(), before);
        let admitted = {
            let _all = rules::control(Control::AdmitAll);
            walk.apply(last[0]);
            walk.state(&state)
        };
        assert_eq!(admitted[measured.0], measured.1, "{path:?}");
        assert!(
            !model.successors(last[0], &state).contains(&admitted),
            "{path:?}"
        );
        assert!(
            !model.check_invariant(law, &admitted),
            "{path:?} keeps {law}"
        );
        // The same step judged holds the law.
        let judged = authority_transition(before, last[0]);
        assert!(model.check_invariant(law, &judged), "{path:?}");
    }
}
