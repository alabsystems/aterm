// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The materializer held to the reading walk of `SharedFile`
//! (`aterm_models_db::shared_file_model`, docs/DESIGN-shared-files-2026-11-11.md §8): each
//! action of the walk is run on a stand-in lab's stores, and what a reader then finds there is
//! what the model's state says. No live vault, registry or account is read.
//!
//! What is read from the stores ([`Seen`]): this Mac's custody and the home a pass opens, the
//! home and frontier the snapshot was projected from, the state word with the kind of custody
//! its pass met, and whether a credential-shaped value reached a file. The model's other
//! variables are its own account of how the stores came to that (the newest snapshot ever
//! written, what a view holds between its look and its record, what a snapshot lacked when it
//! was read): they follow the model's step, and the laws are judged on the two together.
//!
//! The anchors on the materializer's pass and on `aterm_file::File::read` project through
//! `super::model_tests` (`read_transition`), which also walks the writing area; this module
//! holds the reading walk's succession, refusal and overtaken-view paths, which that walk does
//! not take.

use super::{
    FILES, MACHINES,
    materialize::{self, Outcome, Pass, Projecting},
    seam,
};
use crate::{
    alab::{Registry, registry::MACHINE},
    generation::{self, Standing, file_seams},
    table::tests::Lab,
    tests::files,
    typed::{grid::tests::put, keys},
};
use aterm_file::{File, Recorded, Why, Word};
use aterm_json::json;
use aterm_secrets::{
    client::{self, Paths},
    commons::roster::Role,
};
use aterm_spec::{
    derive::Model,
    interp::{self, State},
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

/// The pass clock of every test here.
const NOW: u64 = 1_800_000_000_000;

/// How long a pass waits for the vault a refused custody holds.
const BUSY_WAIT: Duration = Duration::from_millis(50);

fn hang() -> Instant {
    Instant::now() + aterm_spec::wait::MUST_HAPPEN
}

/// A run shaped like a credential, which no file may hold.
fn run() -> String {
    "k9".repeat(24)
}

/// The label of the machine the home `era` of the lineage is given after its first (`Grow`).
fn grown(era: u64) -> String {
    format!("m8{era}")
}

/// What a reader finds on the stores, as the model's variables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    /// 0 live, 1 preserving, 2 unsettled, 3 refused.
    custody: i64,
    /// The home a pass opens now.
    era: i64,
    /// The home the snapshot was projected from, and whether it holds the record that home
    /// was given after its first.
    sera: i64,
    sfr: i64,
    /// 0 never, 1 live, 2 held.
    word: i64,
    /// The custody the last recorded pass met: 0 the newest home, 1 a fenced home (preserving
    /// or unsettled, which a state word does not tell apart), 3 none.
    met: i64,
    /// A credential-shaped value in a snapshot, a face or a history.
    ssecret: i64,
}

impl Seen {
    /// What the model's `state` says a reader finds.
    fn of(state: &State) -> Self {
        Self {
            custody: state["custody"],
            era: state["era"],
            sera: state["sera"],
            sfr: state["sfr"],
            word: state["word"],
            met: match (state["word"], state["src"]) {
                (0, _) | (_, 0) => 0,
                (_, 3) => 3,
                _ => 1,
            },
            ssecret: state["ssecret"],
        }
    }

    /// `state` with what was seen in place of what the model says.
    fn over(self, state: &State) -> State {
        let mut state = state.clone();
        for (name, value) in [
            ("custody", self.custody),
            ("era", self.era),
            ("sera", self.sera),
            ("sfr", self.sfr),
            ("word", self.word),
            ("ssecret", self.ssecret),
        ] {
            state.insert(name, value);
        }
        state
    }
}

/// A stand-in lab whose first Mac holds a registry of machines; its second is enrolled with
/// keys of its own and becomes the first's successor.
struct Stores {
    lab: Lab,
    original: Paths,
    /// The stand-in `aterm.toml`, which no test writes: sharing is on.
    config: PathBuf,
    /// The seed machine's key.
    seed: String,
    /// Whether the successor's home holds its custody yet.
    seated: Cell<bool>,
    /// A refused custody: the state lock of the vault a pass would open, held by another.
    busy: RefCell<Option<Box<dyn std::any::Any>>>,
}

impl Stores {
    fn new() -> Self {
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let original = lab.paths[0].clone();
        let config = lab._home.path().join("aterm.toml");
        let registry = Registry::at(original.clone());
        registry.open().unwrap();
        let seed = keys::machine_pk("m7");
        registry
            .write(vec![vec![
                put(&MACHINE, &seed, "label", json!("m7")),
                put(&MACHINE, &seed, "status", json!("active")),
            ]])
            .unwrap();
        Self {
            lab,
            original,
            config,
            seed,
            seated: Cell::new(false),
            busy: RefCell::new(None),
        }
    }

    /// How this Mac's lineage stands, and the home a pass opens.
    fn held(&self) -> generation::Held {
        generation::materializer_generation_until(&self.original, hang(), &Projecting::in_test())
            .unwrap()
    }

    fn file(&self) -> File {
        File::at(&super::root(&self.original), &MACHINES.file())
    }

    /// One ordinary pass over every file.
    fn pass(&self) -> Vec<Outcome> {
        materialize::pass(
            &self.original,
            &self.config,
            None,
            &Pass {
                read_by: hang(),
                vault_wait: BUSY_WAIT,
                again: false,
                now_ms: NOW,
            },
        )
    }

    /// Run the model's `action` on the stores. `Materialize` returns what its pass did.
    fn apply(&self, action: &str) -> Option<Vec<Outcome>> {
        let held = self.held();
        match action {
            "ChooseRead" => {}
            // The writer this lineage selects now authors one record at its home.
            "Grow" => {
                let label = grown(held.era);
                let pk = keys::machine_pk(&label);
                Registry::at(self.original.clone())
                    .write(vec![vec![
                        put(&MACHINE, &pk, "label", json!(label)),
                        put(&MACHINE, &pk, "status", json!("active")),
                    ]])
                    .unwrap();
            }
            // A record whose note is shaped like a credential, stored as an older or modified
            // build would store it (no guard ran).
            "PeerSecret" => {
                let unchecked = crate::database::Change {
                    collection: MACHINE.name.to_owned(),
                    key: format!("{}.note", self.seed),
                    value: Some(crate::typed::cell::CellValue::owner(1, json!(run())).encode()),
                };
                Registry::at(self.original.clone())
                    .write(vec![vec![unchecked]])
                    .unwrap();
            }
            "Fence" => file_seams::fence(&self.original, &self.lab.bootstrap(0)),
            // The succession completes: for the first time, once its successor is seated, or
            // again after an invalidation.
            "Admit" => {
                if !self.seated.replace(true) {
                    file_seams::seat(&self.original, &self.lab.paths[1]);
                }
                file_seams::complete(&self.original);
            }
            "Invalidate" => file_seams::invalidate_completion(&self.original),
            "Refuse" => {
                let lock = client::test_hold_state_lock(&held.paths).unwrap();
                assert!(self.busy.replace(Some(Box::new(lock))).is_none());
            }
            "Recover" => assert!(self.busy.take().is_some()),
            "Materialize" => {
                let fenced = held.standing != Standing::Newest && self.busy.borrow().is_none();
                let before = fenced.then(|| files(&self.original.dir));
                let outcomes = self.pass();
                if let Some(before) = before {
                    assert_eq!(
                        files(&self.original.dir),
                        before,
                        "a pass over a fenced home wrote to this Mac's vault, custody or tables"
                    );
                }
                return Some(outcomes);
            }
            // A read answers, and takes no step toward custody.
            "Read" => {
                let before = seam::custody_opens();
                for file in FILES {
                    let read = File::at(&super::root(&self.original), &file.file()).read();
                    assert!(read.status.problem.is_none(), "{:?}", read.status.problem);
                    assert_eq!(read.snapshot.is_some(), read.status.word != Word::Never);
                }
                assert_eq!(seam::custody_opens(), before, "a read opened custody");
            }
            _ => panic!("no step of the reading walk is named {action}"),
        }
        None
    }

    /// The stores, read back as the model's variables.
    fn seen(&self) -> Seen {
        let held = self.held();
        let custody = if self.busy.borrow().is_some() {
            3
        } else {
            match held.standing {
                Standing::Newest => 0,
                Standing::Preserving => 1,
                Standing::Unsettled => 2,
            }
        };
        let file = self.file();
        let read = file.read();
        let sera = file.frontier().map_or(0, |frontier| frontier.era);
        let grown = grown(sera);
        let sfr = read.rows().iter().any(|row| row.cell("label") == grown);
        let (word, met) = match (read.status.word, read.status.why) {
            (Word::Never, None) => (0, 0),
            (Word::Live, None) => (1, 0),
            (Word::Held, Some(Why::Succession)) => (2, 1),
            (Word::Held, Some(Why::Busy)) => (2, 3),
            other => panic!("a word the reading walk has no state for: {other:?}"),
        };
        // Every file of a pass records the same word.
        for other in FILES {
            let status = File::at(&super::root(&self.original), &other.file()).status();
            assert_eq!(
                (status.word, status.why),
                (read.status.word, read.status.why),
                "{}",
                other.name
            );
        }
        let root = super::root(&self.original);
        let run = run();
        let ssecret = FILES.iter().any(|file| {
            [
                root.snapshot(&file.file()),
                root.face(&file.file(), "tsv"),
                root.history(&file.file()),
            ]
            .iter()
            .any(|path| std::fs::read_to_string(path).is_ok_and(|text| text.contains(&run)))
        });
        Seen {
            custody,
            era: i64::try_from(held.era).unwrap(),
            sera: i64::try_from(sera).unwrap(),
            sfr: i64::from(sfr),
            word,
            met,
            ssecret: i64::from(ssecret),
        }
    }
}

/// One path of the model's actions on one lab: `states[i]` is the model's state after
/// `path[i]`.
struct Walk {
    path: Vec<&'static str>,
    states: Vec<State>,
    stores: Stores,
}

impl Walk {
    fn new(model: &Model, path: &[&'static str]) -> Rc<Self> {
        let mut state = model.init_state();
        let states = path
            .iter()
            .map(|action| {
                assert!(
                    model.fire(action, &mut state),
                    "{path:?}: {action} is not enabled at {state:?}"
                );
                state.clone()
            })
            .collect();
        Rc::new(Self {
            path: path.to_vec(),
            states,
            stores: Stores::new(),
        })
    }

    /// The stores are where the model is after step `at`.
    #[track_caller]
    fn check(&self, at: usize) {
        assert_eq!(
            self.stores.seen(),
            Seen::of(&self.states[at]),
            "{:?}: the stores (left) and the model (right) after step {at}, {}",
            self.path,
            self.path[at]
        );
    }

    /// Run the steps from `from` on the stores, each held to the model, up to the end of the
    /// path or to the `RefreshWrite` that ends the view this was called under; returns where it
    /// stopped. A view is one real pass: `RefreshLook` is its look and its read, the steps up
    /// to its `RefreshWrite` run between that read and its record, and the record is the
    /// `RefreshWrite`.
    fn run(self: &Rc<Self>, from: usize) -> usize {
        let mut at = from;
        while at < self.path.len() {
            match self.path[at] {
                "RefreshWrite" => return at,
                "RefreshLook" => {
                    let (inner, look) = (Rc::clone(self), at);
                    let stopped = Rc::new(Cell::new(None));
                    let reached = Rc::clone(&stopped);
                    seam::before_record(move || {
                        // The look and the read changed nothing a reader finds.
                        inner.check(look);
                        reached.set(Some(inner.run(look + 1)));
                    });
                    let outcomes = self.stores.pass();
                    let write = stopped.get().expect("the pass reached its record");
                    assert_eq!(
                        self.path.get(write),
                        Some(&"RefreshWrite"),
                        "{:?}",
                        self.path
                    );
                    // A view another pass overtook records nothing.
                    let overtaken = self.states[write - 1]["look"] == 2;
                    for outcome in &outcomes {
                        assert_eq!(
                            outcome.recorded == Some(Recorded::Raced),
                            overtaken,
                            "{:?} at step {write}: {:?}",
                            self.path,
                            outcome.recorded
                        );
                    }
                    self.check(write);
                    at = write + 1;
                }
                action => {
                    if let Some(outcomes) = self.stores.apply(action) {
                        for outcome in &outcomes {
                            assert!(outcome.problem.is_none(), "{:?}", outcome.problem);
                            assert_ne!(outcome.recorded, Some(Recorded::Raced));
                        }
                    }
                    self.check(at);
                    at += 1;
                }
            }
        }
        at
    }
}

/// A walk run to its end.
fn walked(model: &Model, path: &[&'static str]) -> Rc<Walk> {
    let walk = Walk::new(model, path);
    assert_eq!(walk.run(0), path.len(), "{path:?} ran to its end");
    walk
}

/// The successor authors while its succession is complete and no pass reads its home; the
/// completion is invalidated, and the pass projects the original.
const UNSETTLED_PROJECTS: [&str; 9] = [
    "ChooseRead",
    "Materialize",
    "Grow",
    "Fence",
    "Materialize",
    "Admit",
    "Grow",
    "Invalidate",
    "Materialize",
];

/// The successor's home is snapshotted, with a record of its own, and the completion is
/// invalidated.
const UNSETTLED_BEHIND: [&str; 7] = [
    "ChooseRead",
    "Fence",
    "Admit",
    "Materialize",
    "Grow",
    "Materialize",
    "Invalidate",
];

/// The materializer refines the reading walk of `SharedFile`: along each path of its actions
/// the stand-in stores step as the model does, so the model's laws hold of what a reader finds
/// there. The paths run every action of the walk: a pass at the newest home, at a fenced home
/// whose succession is unfinished, and at one whose completion was invalidated, where it
/// projects the original (no pass had read the successor) and where it keeps the successor's
/// snapshot, with and without a record of the successor's own in it; a completion made again;
/// a pass custody refuses; a view another pass overtakes
/// (a fenced pass, a refused pass) and one none does; a credential-shaped record; a read.
///
/// NEGATIVE CONTROLS, each a state the model has no step to, which breaks the law named:
/// - a recording that calls the unsettled home's read the newest (`Opened::newest` set where
///   the standing is `Unsettled`) leaves the rows that lack the successor's record `live`
///   (`HeldNeverLive`);
/// - a pass over stores whose frontier names an earlier home than the snapshot beside it was
///   projected from (the sidecar the monotone rule reads is rewritten as the original home's,
///   with no record) replaces the successor's snapshot with the original home's
///   (`SnapshotMonotone`). The sidecar removed does not: a fenced home's read is behind a
///   snapshot whose frontier is not there to compare (`aterm_file`'s tests hold that).
///
/// Observed with the code changed (2026-11-12): with `with_custody` calling an unsettled home
/// `newest`, the first path stops at its unsettled pass, the stores at `word: 1, met: 0` where
/// the model says `word: 2, met: 1`; with `Frontier::admits_over` never ordering two homes,
/// the third stops at its unsettled pass, the stores at `sera: 0` where the model says
/// `sera: 1`.
#[test]
fn the_materializer_conforms_to_the_reading_walk_and_a_live_unsettled_read_or_a_snapshot_moved_back_is_detected()
 {
    let model = aterm_models_db::shared_file_model();
    let paths: [&[&'static str]; 5] = [
        &[
            "ChooseRead",
            "Materialize",
            "Grow",
            "Fence",
            "Materialize",
            "Admit",
            "Grow",
            "Invalidate",
            "Materialize",
            "Read",
            "Admit",
            "Materialize",
        ],
        &[
            "ChooseRead",
            "Fence",
            "Admit",
            "Materialize",
            "Grow",
            "Materialize",
            "Invalidate",
            "Materialize",
            "Admit",
            "Materialize",
        ],
        // The successor's snapshot holds exactly the records the original holds: only the
        // order of the homes keeps the unsettled view from replacing it.
        &[
            "ChooseRead",
            "Fence",
            "Admit",
            "Materialize",
            "Invalidate",
            "Materialize",
            "Admit",
            "Materialize",
        ],
        &[
            "ChooseRead",
            "Materialize",
            "RefreshLook",
            "Refuse",
            "Materialize",
            "RefreshWrite",
            "Recover",
            "Materialize",
            "PeerSecret",
            "Materialize",
            "Read",
        ],
        &[
            "ChooseRead",
            "RefreshLook",
            "Fence",
            "Materialize",
            "RefreshWrite",
            "RefreshLook",
            "Admit",
            "RefreshWrite",
            "Materialize",
        ],
    ];
    assert_eq!(paths[0][..9], UNSETTLED_PROJECTS);
    assert_eq!(paths[1][..7], UNSETTLED_BEHIND);
    // Every action the model can take in its reading walk runs on the stores.
    let ran: BTreeSet<&str> = paths.iter().flat_map(|path| path.iter().copied()).collect();
    let mut reading = BTreeSet::from(["ChooseRead"]);
    for state in interp::reachable_states(&model) {
        if state["area"] == 1 {
            reading.extend(
                model
                    .actions
                    .iter()
                    .map(|action| action.name)
                    .filter(|action| model.action_enabled(action, &state)),
            );
        }
    }
    assert_eq!(ran, reading, "every action of the reading walk runs");
    for path in paths {
        let walk = walked(&model, path);
        let last = walk.states.last().unwrap();
        for law in &model.invariants {
            assert!(model.check_invariant(law.name, last), "{}", law.name);
        }
    }

    // NEGATIVE CONTROL: the unsettled read recorded as the newest home's.
    let walk = walked(&model, &UNSETTLED_PROJECTS);
    let held = walk.states.last().unwrap();
    assert_eq!((held["custody"], held["word"], held["lacks"]), (2, 2, 1));
    for file in FILES {
        let file = File::at(&super::root(&walk.stores.original), &file.file());
        let recorded = file
            .record(
                file.look(),
                aterm_file::Pass::Opened(Box::new(aterm_file::Opened {
                    newest: true,
                    frontier: file.frontier().unwrap(),
                    projection: materialize::PROJECTION,
                    withheld: 0,
                    projected: None,
                })),
                NOW,
            )
            .unwrap();
        assert_eq!(recorded, Recorded::Recorded { replaced: false });
    }
    let live = walk.stores.seen();
    assert_eq!(
        (live.custody, live.word, live.sfr),
        (2, 1, 1),
        "the negative control must leave the unsettled home's rows live"
    );
    let live = live.over(held);
    assert_eq!(interp::admits(&model, held, &live), None);
    assert!(!model.check_invariant("HeldNeverLive", &live));

    // NEGATIVE CONTROL: the frontier kept beside the successor's snapshot is not its own.
    let walk = walked(&model, &UNSETTLED_BEHIND);
    let unsettled = walk.states.last().unwrap();
    assert_eq!(
        (unsettled["custody"], unsettled["sera"], unsettled["sfr"]),
        (2, 1, 1)
    );
    let root = super::root(&walk.stores.original);
    for file in FILES {
        let name = file.file();
        let mut earlier = File::at(&root, &name).frontier().unwrap();
        assert_eq!(earlier.era, 1);
        earlier.era = 0;
        earlier.ids.clear();
        std::fs::write(root.frontier(&name), earlier.encode()).unwrap();
    }
    walk.stores.apply("Materialize");
    let mut kept = unsettled.clone();
    assert!(model.fire("Materialize", &mut kept));
    let back = walk.stores.seen();
    assert_eq!(
        (back.sera, back.sfr, back.word),
        (0, 0, 2),
        "the negative control must replace the successor's snapshot with the original's"
    );
    assert_ne!(back, Seen::of(&kept));
    assert!(!model.check_invariant("SnapshotMonotone", &back.over(&kept)));
}
