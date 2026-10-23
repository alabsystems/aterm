// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the crash journal to `aterm_models_gui::crash_journal_claim_model` (`CrashJournalClaim`, PTY keeper P1).

use std::collections::BTreeMap;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};

use aterm_models_gui::crash_journal_claim_model;
use aterm_spec::interp::{self, State};
use aterm_spec::verify::validate_transition_tiered;

use crate::crash_journal::tests::{layout, reaped_pid, scratch};
use crate::crash_journal::{self, Evidence, JournalHeader, JournalId, JournalOwner, QuitLayout};
use crate::crash_signal::{MarkerOwner, markers};

/// Where replays run.
pub(crate) struct Desk {
    dir: aterm_tempfile::TempDir,
    logs: aterm_tempfile::TempDir,
    /// Pids of already-reaped processes, made as first needed: run `k` of a replay is named for
    /// the `k`th, so no two runs of one replay share a pid.
    pids: Vec<u32>,
}

impl Desk {
    fn new() -> Self {
        Desk {
            dir: scratch("cjc-dir"),
            logs: scratch("cjc-logs"),
            pids: Vec::new(),
        }
    }

    /// The `run`th pid of a replay (from 0).
    fn pid(&mut self, run: usize) -> u32 {
        while self.pids.len() <= run {
            self.pids.push(reaped_pid());
        }
        self.pids[run]
    }

    /// Empty both directories for the next replay, then check each reads back empty.
    fn clear(&self) {
        for dir in [self.dir.path(), self.logs.path()] {
            for entry in std::fs::read_dir(dir).expect("the desk reads") {
                let entry = entry.expect("a desk entry");
                let path = entry.path();
                if entry.file_type().expect("its type").is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                }
                .unwrap_or_else(|error| panic!("clear {}: {error}", path.display()));
            }
            assert!(
                std::fs::read_dir(dir)
                    .expect("the desk reads")
                    .next()
                    .is_none(),
                "{} is empty for the next replay",
                dir.display()
            );
        }
    }
}

/// One replay: its desk, and what the process around the journal did.
pub(crate) struct Rig {
    desk: Desk,
    /// How many of the desk's pids this replay's runs are named for.
    runs: usize,
    id: JournalId,
    owner: Option<JournalOwner>,
    /// The run's crash marker while it exists, and its lock while the run lives.
    marker: Option<(PathBuf, Option<OwnedFd>)>,
    /// Before every claim, a windowed launch that does not claim reads every
    /// run's evidence first.
    consumed_first: bool,
    /// How many of those reads found evidence to consume.
    consumed: usize,
    booted: bool,
    live: bool,
    clean: bool,
    probation: bool,
    second: bool,
    crashed: bool,
    written: bool,
    writes: usize,
    image_probation: bool,
    image_second: bool,
    claims: i64,
    live_claim: bool,
    applied: bool,
    lost: bool,
    /// The update's successor runs and has not taken the journal over.
    successor: bool,
}

/// The quit layout's file, beside the journals in the rig's directory.
const SESSION: &str = "session.toml";

impl Rig {
    fn new(consumed_first: bool) -> Self {
        Rig::on(Desk::new(), consumed_first)
    }

    /// A replay on `desk`, empty as [`Desk::clear`] leaves it.
    fn on(mut desk: Desk, consumed_first: bool) -> Self {
        let id = JournalId::now(desk.pid(0));
        Rig {
            desk,
            runs: 1,
            id,
            owner: None,
            marker: None,
            consumed_first,
            consumed: 0,
            booted: false,
            live: true,
            clean: false,
            probation: false,
            second: false,
            crashed: false,
            written: false,
            writes: 0,
            image_probation: false,
            image_second: false,
            claims: 0,
            live_claim: false,
            applied: false,
            lost: false,
            successor: false,
        }
    }

    /// The replay is over: its owner and its marker's descriptor go, as when
    /// a rig drops, and its desk comes back emptied for the next.
    fn finish(self) -> Desk {
        let Rig {
            desk,
            owner,
            marker,
            ..
        } = self;
        drop((owner, marker));
        desk.clear();
        desk
    }

    fn dir(&self) -> &Path {
        self.desk.dir.path()
    }

    fn logs(&self) -> &Path {
        self.desk.logs.path()
    }

    fn image(&self) -> PathBuf {
        self.dir().join(self.id.file_name())
    }

    /// The run starts: its crash marker (empty, locked, the app's) is armed before its journal
    /// and named for the same start (`JournalId::of_this_run`).
    fn boot(&mut self, probation: bool, second: bool) {
        let armed = markers::arm(self.logs(), self.id.pid, self.id.nanos, MarkerOwner::App)
            .expect("marker armed");
        assert!(armed.locked, "the marker's owner lock is held");
        // SAFETY: `arm` returns a descriptor it opened for this call and nobody
        // else owns; the `OwnedFd` closes it exactly once.
        let lock = unsafe { OwnedFd::from_raw_fd(armed.fd) };
        self.marker = Some((armed.path, Some(lock)));
        self.booted = true;
        self.probation = probation;
        self.second = second;
    }

    /// The successor's commit (`release_committed_readers`, then `Lane::adopt_predecessor` at
    /// its wake): a new run, its marker armed as `boot` arms one, takes the handed-off image
    /// over onto its own journal by the real `JournalOwner::adopt`; only then do its readers run
    /// on the live sessions.
    fn adopt(&mut self) {
        let parent = self.id;
        let mut id = JournalId::now(self.desk.pid(self.runs));
        self.runs += 1;
        while id.nanos <= parent.nanos {
            id = JournalId::now(id.pid);
        }
        self.id = id;
        self.boot(false, false);
        let (owner, taken) =
            JournalOwner::adopt(self.dir(), self.id, parent.pid, Some(parent.nanos))
                .expect("taken over");
        assert_eq!(taken, Some(parent), "the handed-off image is the one taken");
        self.owner = Some(owner);
        self.live = true;
        self.clean = false;
        self.successor = false;
    }

    /// The writer thread's two calls: arm on the first image, publish.
    fn publish(&mut self) {
        if self.owner.is_none() {
            self.owner = Some(JournalOwner::create(self.dir(), self.id).expect("armed"));
        }
        self.writes += 1;
        let cwd = format!("/w{}", self.writes);
        self.owner
            .as_ref()
            .expect("armed")
            .publish(
                &layout(&[(cwd.as_str(), "zsh")]),
                &JournalHeader::now(self.id.pid, self.probation)
                    .with_second_chance(self.probation && self.second),
            )
            .expect("published");
        self.written = true;
    }

    /// The process ends; `clean` means through an exit path, which removes its crash marker.
    fn end(&mut self, clean: bool) {
        if let Some((path, lock)) = self.marker.as_mut() {
            if clean {
                std::fs::remove_file(&*path).expect("a live run's marker is under its name");
            }
            if let Some(lock) = lock.take() {
                // SAFETY: `flock` on the descriptor the rig holds open until
                // it drops right after; LOCK_UN never waits.
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the release: LOCK_UN before the descriptor closes"
                )]
                unsafe {
                    libc::flock(lock.as_raw_fd(), libc::LOCK_UN)
                };
            }
        }
        if clean {
            self.marker = None;
        }
        drop(self.owner.take());
        self.live = false;
        self.clean = clean;
    }

    /// The quit's first step: the real `restore::write_quit_to`, stamped with
    /// this run's journal id as the quit stamps it.
    fn write_quit_layout(&mut self) {
        crate::restore::write_quit_to(
            &self.dir().join(SESSION),
            &layout(&[("/quit", "zsh")]),
            Some(&self.id.stem()),
        )
        .expect("the quit layout written");
    }

    /// The quit's `session.toml` as a later launch reads it before claiming (`main_entry` via
    /// `restore::take` and `JournalId::parse_stem`), but read, not taken, so a forbidden claim
    /// run anyway leaves it for the next.
    fn quit_layout(&self) -> QuitLayout {
        let path = self.dir().join(SESSION);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return QuitLayout::default();
        };
        QuitLayout::of(
            crate::restore::RestoreManifest::from_toml(&text).as_ref(),
            crate::restore::quit_run_of(&text)
                .as_deref()
                .and_then(JournalId::parse_stem),
            std::fs::metadata(&path)
                .ok()
                .as_ref()
                .and_then(crate::restore::modified_unix_ms),
        )
        .holding(&crate::restore::quit_also_of(&text))
    }

    /// A later launch's real claim. On the consumed pass, a windowed launch that does not claim
    /// first reads (and renames) every run's evidence, as its boot does.
    fn claim(&mut self) {
        if self.consumed_first {
            let killed =
                crate::logging::take_kill_evidence_in(self.logs(), std::process::id(), None);
            let crashed = crate::logging::take_crash_evidence_in(self.logs(), None);
            self.consumed += usize::from(killed.is_some() || crashed.is_some());
        }
        let evidence = Evidence {
            log_dir: Some(self.logs()),
            swept: &[],
        };
        let claim = crash_journal::claim_at_boot(
            self.dir(),
            &evidence,
            std::process::id(),
            self.quit_layout(),
        );
        if claim.taken.contains(&self.id) {
            self.claims += 1;
            self.live_claim |= self.live;
        }
        self.applied |= claim
            .reopened
            .as_ref()
            .is_some_and(|reopened| reopened.sources.contains(&self.id));
    }

    fn step(&mut self, action: &str) {
        match action {
            "BootFresh" => self.boot(false, false),
            "BootFromJournal" => self.boot(true, false),
            "BootSecondChance" => self.boot(true, true),
            "Write" => self.publish(),
            "Settle" => {
                self.probation = false;
                self.second = false;
                if self.written {
                    self.publish();
                }
            }
            "Quit" => {
                if let Some(owner) = self.owner.take() {
                    owner.retire().expect("retired");
                }
                self.end(true);
            }
            "WriteQuitLayout" => self.write_quit_layout(),
            "HandOff" => {
                self.end(true);
                self.successor = true;
            }
            "SuccessorDies" => self.successor = false,
            "Adopt" => self.adopt(),
            "Die" => {
                self.end(false);
                self.lost = self.written && !self.image().exists();
            }
            "Crash" => {
                if let Some((path, _)) = self.marker.as_ref() {
                    std::fs::write(path, b"aterm: fatal signal 11 (SIGSEGV)")
                        .expect("the live run's marker takes its banner");
                }
                self.crashed = true;
                self.end(false);
                self.lost = self.written && !self.image().exists();
            }
            "Claim" => self.claim(),
            other => panic!("no such action {other}"),
        }
    }

    /// The model's state, read off the directory and the replay's truth.
    pub(crate) fn project(&mut self) -> State {
        let image = self.image().exists();
        if image {
            let text = std::fs::read_to_string(self.image()).expect("a whole image");
            let header = crash_journal::decode(&text).expect("decodes").0;
            self.image_probation = header.probation;
            self.image_second = header.second_chance;
        }
        let mut s: State = BTreeMap::new();
        s.insert("booted", i64::from(self.booted));
        s.insert("live", i64::from(self.live));
        s.insert("clean", i64::from(self.clean));
        s.insert("probation", i64::from(self.probation));
        s.insert("second", i64::from(self.second));
        s.insert("crashed", i64::from(self.crashed));
        s.insert("image", i64::from(image));
        s.insert("image_probation", i64::from(self.image_probation));
        s.insert("image_second", i64::from(self.image_second));
        s.insert("torn", 0);
        s.insert("written", i64::from(self.written));
        s.insert("claims", self.claims);
        s.insert("live_claim", i64::from(self.live_claim));
        s.insert("applied", i64::from(self.applied));
        s.insert("lost", i64::from(self.lost));
        s.insert("quit_layout", i64::from(self.quit_layout().names(self.id)));
        s.insert("successor", i64::from(self.successor));
        s
    }
}

const ACTIONS: &[&str] = &[
    "BootFresh",
    "BootFromJournal",
    "BootSecondChance",
    "Write",
    "FinishWrite",
    "Settle",
    "Quit",
    "WriteQuitLayout",
    "HandOff",
    "Adopt",
    "SuccessorDies",
    "Die",
    "Crash",
    "Claim",
];

/// One real step, projected: `(before, action, after)`.
type Step = (State, &'static str, State);

/// The ends a person meets, each replayed whatever the cover holds.
const ENDS: &[&[&str]] = &[
    &["BootFresh", "Write", "Die", "Claim"],
    &["BootFresh", "Write", "Quit"],
    &["BootFresh", "Write", "WriteQuitLayout", "Quit"],
    &["BootFresh", "Write", "WriteQuitLayout", "Die", "Claim"],
    &["BootFresh", "Write", "WriteQuitLayout", "Crash", "Claim"],
    &["BootFresh", "Write", "HandOff", "Claim"],
    &["BootFresh", "Write", "HandOff", "Adopt", "Die", "Claim"],
    &["BootFresh", "Write", "HandOff", "Adopt", "Crash", "Claim"],
    &["BootFresh", "Write", "HandOff", "Adopt", "Quit"],
    &["BootFresh", "HandOff", "SuccessorDies"],
    &["BootFromJournal", "Write", "Die", "Claim"],
    &["BootFromJournal", "Write", "Crash", "Claim"],
    &["BootSecondChance", "Write", "Die", "Claim"],
    &["BootFromJournal", "Write", "Settle", "Die", "Claim"],
];

/// EVERY STEP OF THE MODEL, EACH ONCE: breadth first from the initial
/// state, every reachable state by the shortest schedule that reaches it,
/// and that schedule extended by every action enabled there (`fire`d, the
/// canonical successor), with every one of [`ENDS`] beside them.
fn transition_cover(
    model: &aterm_spec::derive::Model,
    actions: &[&'static str],
) -> Vec<Vec<&'static str>> {
    let init = model.init_state();
    let mut seen = std::collections::BTreeSet::from([init.clone()]);
    let mut frontier = std::collections::VecDeque::from([(init, Vec::new())]);
    let mut cover: Vec<Vec<&'static str>> = ENDS.iter().map(|end| end.to_vec()).collect();
    while let Some((state, path)) = frontier.pop_front() {
        for &action in actions {
            let mut next = state.clone();
            if !model.fire(action, &mut next) {
                continue;
            }
            let mut schedule: Vec<&'static str> = path.clone();
            schedule.push(action);
            if seen.insert(next.clone()) {
                frontier.push_back((next, schedule.clone()));
            }
            cover.push(schedule);
        }
    }
    cover.sort();
    cover.dedup();
    let longer =
        |s: &Vec<&'static str>| cover.iter().any(|t| t.len() > s.len() && t.starts_with(s));
    cover.iter().filter(|s| !longer(s)).cloned().collect()
}

/// Replay `schedule` on the real owner and claim on `desk` (handed back emptied). With
/// `consumed_first`, a launch that does not claim reads the evidence before every claim
/// (`consumed` counts the reads that found some).
fn replay(
    desk: Desk,
    schedule: &[&'static str],
    consumed_first: bool,
    refused: &mut usize,
    consumed: &mut usize,
) -> (Desk, State, Vec<Step>) {
    let model = crash_journal_claim_model();
    let mut rig = Rig::on(desk, consumed_first);
    let initial = rig.project();
    let mut state = initial.clone();
    let mut steps = Vec::new();
    for &action in schedule {
        rig.step(action);
        let after = rig.project();
        steps.push((state, action, after.clone()));
        if !model.action_enabled("Claim", &after) {
            rig.claim();
            assert_eq!(
                rig.project(),
                after,
                "{schedule:?} after {action}: a claim the model forbids changed something"
            );
            *refused += 1;
        }
        state = after;
    }
    *consumed += rig.consumed;
    (rig.finish(), initial, steps)
}

/// The shipping owner and claim refine `CrashJournalClaim` on every step the
/// model can take from every state it reaches ([`transition_cover`]) — each
/// schedule replayed twice, the second time with every claim's
/// evidence read (and renamed `.seen`) first by a launch that does not claim —
/// every state they reach satisfies every invariant, and between them they
/// reach EVERY state the model can reach.
#[test]
fn the_real_journal_conforms_to_the_model() {
    let model = crash_journal_claim_model();
    for end in ENDS {
        let mut state = model.init_state();
        for action in *end {
            assert!(
                model.fire(action, &mut state),
                "{end:?}: the model admits {action}"
            );
        }
    }
    let all = transition_cover(&model, ACTIONS);
    assert!(
        all.len() > 60,
        "the cover reaches the schedules it exists for: {}",
        all.len()
    );
    let jobs: Vec<(&Vec<&'static str>, bool)> = all
        .iter()
        .flat_map(|schedule| [(schedule, false), (schedule, true)])
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let validated = std::sync::Mutex::new(std::collections::BTreeSet::<Step>::new());
    let threads = std::thread::available_parallelism().map_or(1, |n| usize::from(n).min(4));
    let mut reached: std::collections::BTreeSet<State> =
        std::collections::BTreeSet::from([model.init_state()]);
    let (mut refused, mut consumed) = (0usize, 0usize);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                let (model, jobs, next, validated) = (&model, &jobs, &next, &validated);
                scope.spawn(move || {
                    let (mut refused, mut consumed) = (0usize, 0usize);
                    let mut reached = std::collections::BTreeSet::new();
                    let mut desk = Desk::new();
                    loop {
                        let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&(schedule, consumed_first)) = jobs.get(k) else {
                            break;
                        };
                        let (emptied, initial, steps) =
                            replay(desk, schedule, consumed_first, &mut refused, &mut consumed);
                        desk = emptied;
                        assert_eq!(initial, model.init_state(), "{schedule:?}: initial");
                        for (before, action, after) in &steps {
                            assert!(
                                model.successors(action, before).contains(after),
                                "{schedule:?} (evidence read first: {consumed_first}) step \
                                 {action}: {before:?} -> {after:?} does not conform to \
                                 CrashJournalClaim"
                            );
                            for invariant in &model.invariants {
                                assert!(
                                    model.check_invariant(invariant.name, after),
                                    "{schedule:?} (evidence read first: {consumed_first}) after \
                                     {action}: {} on {after:?}",
                                    invariant.name
                                );
                            }
                        }
                        for step in steps {
                            let first = validated
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(step.clone());
                            let (before, action, after) = step;
                            if first {
                                let label =
                                    format!("distinct step {action}: {before:?} -> {after:?}");
                                assert!(
                                    validate_transition_tiered(
                                        model,
                                        &[],
                                        &before,
                                        &after,
                                        Some(action),
                                        &label
                                    )
                                    .0,
                                    "{label} does not conform to CrashJournalClaim"
                                );
                            }
                            reached.insert(after);
                        }
                    }
                    (reached, refused, consumed)
                })
            })
            .collect();
        for worker in workers {
            let (states, r, c) = worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            reached.extend(states);
            refused += r;
            consumed += c;
        }
    });
    assert_eq!(
        reached.len(),
        interp::bmc(&model).expect("the committed model proves"),
        "the real replays reach every state the model can reach"
    );
    assert!(refused > 0, "the forbidden claims were run");
    assert!(
        consumed > 0,
        "the second pass's earlier launch found evidence to read first"
    );
}

/// Negative control: `Buggy = 1` disagrees with the real steps at a write (the real publish is
/// one rename), at a claim blind to the quit layout's run, and at the claim of a running
/// window's journal (its owner's lock is held), so a pass of the committed model is not vacuous.
#[test]
fn the_defects_reject_the_real_steps() {
    let buggy = interp::with_buggy(&crash_journal_claim_model(), 1);
    let rejected = |schedule: &[&'static str], at: &str| {
        let (mut refused, mut consumed) = (0, 0);
        let (_, _, steps) = replay(Desk::new(), schedule, false, &mut refused, &mut consumed);
        steps.iter().any(|(before, action, after)| {
            *action == at && !buggy.successors(action, before).contains(after)
        })
    };
    assert!(
        rejected(&["BootFresh", "Write"], "Write"),
        "the unlink-first writer disagrees with the real publish"
    );
    assert!(
        rejected(
            &["BootFresh", "Write", "WriteQuitLayout", "Die", "Claim"],
            "Claim"
        ),
        "the claim blind to the quit layout's run disagrees with the real one (finding 48's \
         review: a quit cut short opens its desk once)"
    );
    let committed = crash_journal_claim_model();
    let mut state = committed.init_state();
    for action in ["BootFresh", "Write"] {
        assert!(committed.fire(action, &mut state));
    }
    assert!(buggy.action_enabled("Claim", &state));
    assert!(!committed.action_enabled("Claim", &state));
    let mut rig = Rig::new(false);
    rig.step("BootFresh");
    rig.step("Write");
    let before = rig.project();
    assert_eq!(before, state);
    rig.step("Claim");
    let after = rig.project();
    assert!(
        !buggy.successors("Claim", &before).contains(&after),
        "the lock-blind claim disagrees with the real one: {before:?} -> {after:?}"
    );
    assert_eq!(
        after, before,
        "the real claim left the running window's journal"
    );
}
