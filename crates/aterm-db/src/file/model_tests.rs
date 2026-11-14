// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `SharedFile` conformance (docs/DESIGN-shared-files-2026-11-11.md §8): the reading and the
//! writing walks of the model, taken on a stand-in lab's real stores. Each step runs the code
//! its anchor names (`aterm_file::File::read`, the accept inside `File::write`,
//! [`super::materialize::pass`], [`super::publish::pass`]) in an evidence window of its own, and
//! the state measured from the stores after it must be a successor the model has for that
//! action from the state measured before it. The environment's steps (a peer's record, a
//! record arriving, the fence) are made through the registry and writer succession's own seam.
//!
//! What a walk measures, never assumes: the outbox, held list and done markers on disk; the
//! table's records and the register's candidates; the table files' bytes under a fence, where
//! the strict reads refuse; the snapshot's frontier and state word; and the custody seam
//! across a read.

use super::{
    MACHINES, publish, seam,
    tests::{NOW, SEAT, Stand, publish_pass},
    write,
};
use crate::{
    alab::{Registry, registry::MACHINE},
    att, generation,
    typed::grid::tests::put,
};
use astream_wire::sync::Digest;
use aterm_file::{OwnerAct, Why, Word, outbox};
use aterm_json::json;
use aterm_spec::{interp::State, xref::StepEvidence};
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
}

impl Walk {
    fn new() -> Self {
        Self {
            stand: Stand::new(),
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
                let outcomes = self.evidence.step("Materialize", || stand.pass(true));
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
                    write::draft_set(stand.paths(), MACHINES.name, "m7", "note", Some(&text))
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
                let act = OwnerAct::verb("person", None).unwrap();
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
            other => panic!("the walk has no step {other}"),
        }
    }

    /// How many of the records that arrived during the walk the snapshot was projected from.
    fn snapshot_frontier(&self) -> i64 {
        let Some(frontier) = self.stand.file(&MACHINES).frontier() else {
            return 0;
        };
        self.grown
            .iter()
            .filter(|operation| frontier.ids.contains(*operation))
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

/// Run `path` on a fresh stand-in lab, each step held to the model: the stores' state after it
/// is a successor the model has for that action. Returns the walk and its last state.
fn run(model: &aterm_spec::derive::Model, path: &[&'static str]) -> (Walk, State) {
    let mut walk = Walk::new();
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
    let (mut walk, state) = run(&model, path);
    walk.apply(action);
    walk.state(&state)
}

/// The `SharedFile` projection of `action` from the state `path` reaches, in the reading area:
/// what the anchors on the materializer's pass and on `File::read` name.
pub(crate) fn read_transition(path: &[&'static str], action: &'static str) -> State {
    write_transition(path, action)
}

/// The shared files' code refines its model along the reading and the writing walks: a read
/// before anything is materialized, after a pass, after a record arrived, and under a fence; a
/// write accepted, signed and done, twice over; a write whose register a peer moved, held; and
/// a write accepted under a fence, which no pass signs. Every anchored action is driven in a
/// window that enters its own function.
///
/// NEGATIVE CONTROL: with the publisher ignoring what the intent's author saw
/// (`publish::seam::ignore_expected`), the pass signs over the peer's candidate. The stores
/// then say `recs == 1 && lost == 1`, a state the model has no `Sign` step to (its `Sign` is
/// not enabled while a peer's candidate stands) and that breaks `PublishObservedOnly`.
#[test]
fn shared_file_conforms_and_a_publish_that_ignores_expected_is_detected() {
    let model = aterm_models_db::shared_file_model();
    let paths: [&[&'static str]; 8] = [
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
    ];
    let mut evidence = StepEvidence::new("SharedFile");
    for path in paths {
        let (walk, state) = run(&model, path);
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
        for law in &model.invariants {
            assert!(
                model.check_invariant(law.name, &state),
                "{path:?} ends in a state that breaks {}",
                law.name
            );
        }
        evidence.absorb(walk.evidence);
    }
    let audit = evidence.audit();
    assert!(audit.is_complete(), "{audit:#?}");
    assert_eq!(
        audit.actions.iter().copied().collect::<Vec<_>>(),
        ["Accept", "Done", "Hold", "Materialize", "Read", "Sign"]
    );

    let (mut walk, state) = run(&model, &["ChooseWrite", "Accept", "PeerWrite"]);
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
