// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the crash journal to
//! `aterm_spec::derive::crash_journal_claim_model` (`CrashJournalClaim`, PTY
//! keeper P1).
//!
//! Every schedule of the model up to six steps — enough to reach every state
//! the model can reach, which the test checks — is replayed on the SHIPPING
//! owner and claim over a real directory: `JournalOwner::create`/`publish`
//! (the writer thread's two calls) for `Write` and `Settle`,
//! `JournalOwner::retire` for the quit's removal, and `claim_at_boot` — with
//! `classify_death` reading real crash markers in a real log dir — for every
//! later launch. The test plays the process around them: it arms the run's
//! crash marker at boot as the installed app arms it (`markers::arm`: locked
//! for the run's life, named for the start the journal is named for too) and
//! removes it on a clean end (what `atexit` and `clean_exit_now` do), releases
//! the owner's locks without a word on a death (what the kernel does to a
//! SIGKILLed process), and leaves the journal behind on the update's hand-off.
//! Every schedule is replayed twice: once as above, and once with a windowed
//! launch that does NOT claim — an update's successor, a launch with
//! `restore_session` off — reading every run's crash and kill evidence before
//! each claim (`take_kill_evidence_in`/`take_crash_evidence_in`, which rename
//! it `.seen`), so the claim's verdict must not depend on who read the
//! evidence first. Every real step is projected onto the model
//! and checked as a transition the committed model admits (interpreter, and
//! `ty` wherever installed); every reached state satisfies every invariant; and
//! wherever the model forbids a claim, the real claim is run anyway and must
//! change nothing.
//!
//! NEGATIVE CONTROL: the same real steps against `Buggy = 1` — a claim that
//! takes a running window's journal, copies instead of renaming, and reopens
//! whatever it took, and a writer that unlinks before it publishes — must be
//! rejected: at the claim of a live owner's journal, and at every write.

use std::collections::BTreeMap;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::path::PathBuf;

use aterm_spec::derive::{Model, crash_journal_claim_model};
use aterm_spec::interp::{self, State};
use aterm_spec::verify::validate_transition_tiered;

use crate::crash_journal::tests::{await_released, layout, reaped_pid, scratch};
use crate::crash_journal::{self, Evidence, JournalHeader, JournalId, JournalOwner};
use crate::crash_signal::{MarkerOwner, markers};

/// One replay: a journal directory, a log dir, and what the process around
/// the journal did.
pub(crate) struct Rig {
    dir: aterm_tempfile::TempDir,
    logs: aterm_tempfile::TempDir,
    id: JournalId,
    owner: Option<JournalOwner>,
    /// TRUTH: the run's crash marker while it exists, and its lock while the
    /// run lives.
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
}

impl Rig {
    fn new(consumed_first: bool) -> Self {
        Rig {
            dir: scratch("cjc-dir"),
            logs: scratch("cjc-logs"),
            // A pid no process has: the owner's liveness is its lock alone.
            id: JournalId::now(reaped_pid()),
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
        }
    }

    fn image(&self) -> PathBuf {
        self.dir.path().join(self.id.file_name())
    }

    /// The run starts: its crash marker is armed (empty, locked, the installed
    /// app's), before its journal, and named for the start the journal is
    /// named for (`JournalId::of_this_run`). `second`: the journal it
    /// reopened was a second chance's to give (`Lane::begin_probation`'s
    /// flag, ruling 285).
    fn boot(&mut self, probation: bool, second: bool) {
        let armed = markers::arm(
            self.logs.path(),
            self.id.pid,
            self.id.nanos,
            MarkerOwner::App,
        )
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

    /// The writer thread's two calls: arm on the first image, publish.
    fn publish(&mut self) {
        if self.owner.is_none() {
            self.owner = Some(JournalOwner::create(self.dir.path(), self.id).expect("armed"));
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

    /// The process ends. `clean`: through an exit path, which removes its
    /// crash marker before the process goes. Its locks go with it however it
    /// ends.
    fn end(&mut self, clean: bool) {
        if let Some((path, lock)) = self.marker.as_mut() {
            if clean {
                std::fs::remove_file(&*path).expect("a live run's marker is under its name");
            }
            drop(lock.take());
            if !clean {
                await_released(path);
            }
        }
        if clean {
            self.marker = None;
        }
        if let Some(owner) = self.owner.take() {
            drop(owner);
            await_released(&self.dir.path().join(self.id.lock_name()));
        }
        self.live = false;
        self.clean = clean;
    }

    /// A later launch's claim: the real one, over the real directory — after,
    /// on the consumed pass, an earlier windowed launch that did not claim
    /// read (and renamed) every run's evidence, as its boot does.
    fn claim(&mut self) {
        if self.consumed_first {
            let killed =
                crate::logging::take_kill_evidence_in(self.logs.path(), std::process::id());
            let crashed = crate::logging::take_crash_evidence_in(self.logs.path());
            self.consumed += usize::from(killed.is_some() || crashed.is_some());
        }
        let evidence = Evidence {
            log_dir: Some(self.logs.path()),
            swept: &[],
        };
        let claim =
            crash_journal::claim_at_boot(self.dir.path(), &evidence, std::process::id(), false);
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
            "HandOff" => self.end(true),
            "Die" => {
                self.end(false);
                self.lost = self.written && !self.image().exists();
            }
            // A fatal signal: the handler writes its banner into the run's
            // own marker (`crash_signal`), then the process goes.
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
        // A real publish is one rename: no state has an empty name mid-write.
        s.insert("torn", 0);
        s.insert("written", i64::from(self.written));
        s.insert("claims", self.claims);
        s.insert("live_claim", i64::from(self.live_claim));
        s.insert("applied", i64::from(self.applied));
        s.insert("lost", i64::from(self.lost));
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
    "HandOff",
    "Die",
    "Crash",
    "Claim",
];

/// Every schedule of `model` up to `depth` steps that cannot be extended
/// inside it: the full enumeration, not a hand-picked few.
fn schedules(model: &Model, depth: usize) -> Vec<Vec<&'static str>> {
    fn walk(
        model: &Model,
        state: &State,
        prefix: &mut Vec<&'static str>,
        depth: usize,
        out: &mut Vec<Vec<&'static str>>,
    ) {
        let enabled: Vec<&'static str> = ACTIONS
            .iter()
            .copied()
            .filter(|action| model.action_enabled(action, state))
            .collect();
        if prefix.len() == depth || enabled.is_empty() {
            out.push(prefix.clone());
            return;
        }
        for action in enabled {
            let mut next = state.clone();
            assert!(model.fire(action, &mut next));
            prefix.push(action);
            walk(model, &next, prefix, depth, out);
            prefix.pop();
        }
    }
    let mut out = Vec::new();
    walk(model, &model.init_state(), &mut Vec::new(), depth, &mut out);
    out
}

/// One real step, projected: `(before, action, after)`.
type Step = (State, &'static str, State);

/// Replay `schedule` on the real owner and claim — with the evidence read
/// first by a launch that does not claim before every claim when
/// `consumed_first` (`consumed` counts the reads that found some). After every
/// step, where the committed model forbids a claim, the real claim runs anyway
/// and must leave the projection unchanged (`refused` counts those).
fn replay(
    schedule: &[&'static str],
    consumed_first: bool,
    refused: &mut usize,
    consumed: &mut usize,
) -> (State, Vec<Step>) {
    let model = crash_journal_claim_model();
    let mut rig = Rig::new(consumed_first);
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
    (initial, steps)
}

/// The shipping owner and claim refine `CrashJournalClaim` on every schedule
/// up to six steps — each replayed twice, the second time with every claim's
/// evidence read (and renamed `.seen`) first by a launch that does not claim —
/// every state they reach satisfies every invariant, and between them they
/// reach EVERY state the model can reach. Each step is admitted by the
/// interpreter; each DISTINCT real transition is then validated once on both
/// tiers.
///
/// The machine has eleven actions. `Write`, `Settle`, `Quit` and `Claim` carry
/// real `#[refines]` anchors (`JournalOwner::publish`/`retire`,
/// `claim_at_boot`). The other seven are the process around the journal, which
/// this runner plays and projects rather than code any journal function runs.
#[aterm_spec::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "BootFresh",
    reason = "The run's own start: the crash marker it arms is crash_signal's install; the \
              runner arms one in the real log dir and the journal is armed lazily at the \
              first Write."
)]
#[aterm_spec::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "BootFromJournal",
    reason = "The run's own start after a reopened journal: Lane::begin_probation only sets \
              the header mark the runner writes through JournalOwner::publish; its 90 s \
              clock is crash_journal::tests::the_lane_writes_only_changes_at_the_bounded_rate."
)]
#[aterm_spec::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "BootSecondChance",
    reason = "The run's own start after the brake let a stopped probation writer's journal \
              through (ruling 285): Lane::begin_probation's second-chance flag only sets the \
              header mark the runner writes through JournalOwner::publish \
              (crash_journal::tests::a_relapse_inside_probation_is_skipped_and_said)."
)]
#[aterm_spec::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "Crash",
    reason = "A fatal signal runs no journal code: crash_signal's handler writes its banner \
              into the run's own marker, which the runner writes, and the kernel releases the \
              locks as for Die."
)]
#[aterm_spec::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "HandOff",
    reason = "The update parent's _exit after Commit is seamless::commit_and_exit, which \
              removes the crash marker and ends the process; the runner removes the marker \
              and drops the owner, and the kernel releases the lock."
)]
#[aterm_spec::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "Die",
    reason = "A SIGKILL, a signal or a panic runs no journal code: the runner drops the owner \
              and waits for the kernel to release its lock, leaving the marker."
)]
#[aterm_spec::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "FinishWrite",
    reason = "Exists only in the Buggy = 1 unlink-first writer; the shipping publish is one \
              rename, which the_defects_reject_the_real_steps pins, and a reader racing 400 real \
              rewrites always finds a whole image (crash_journal::tests::\
              a_rewrite_never_leaves_the_name_without_a_whole_image)."
)]
#[test]
fn the_real_journal_conforms_to_the_model() {
    let model = crash_journal_claim_model();
    let all = schedules(&model, 6);
    assert!(
        all.len() > 60,
        "the enumeration reaches the schedules it exists for: {}",
        all.len()
    );
    let mut distinct: Vec<Step> = Vec::new();
    let mut reached: Vec<State> = vec![model.init_state()];
    let (mut refused, mut consumed) = (0usize, 0usize);
    for (schedule, consumed_first) in all
        .iter()
        .flat_map(|schedule| [(schedule, false), (schedule, true)])
    {
        let (initial, steps) = replay(schedule, consumed_first, &mut refused, &mut consumed);
        assert_eq!(initial, model.init_state(), "{schedule:?}: initial");
        for (before, action, after) in steps {
            assert!(
                model.successors(action, &before).contains(&after),
                "{schedule:?} (evidence read first: {consumed_first}) step {action}: \
                 {before:?} -> {after:?} does not conform to CrashJournalClaim"
            );
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, &after),
                    "{schedule:?} (evidence read first: {consumed_first}) after {action}: {} \
                     on {after:?}",
                    invariant.name
                );
            }
            if !reached.contains(&after) {
                reached.push(after.clone());
            }
            if !distinct.contains(&(before.clone(), action, after.clone())) {
                distinct.push((before, action, after));
            }
        }
    }
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
    for (before, action, after) in &distinct {
        let label = format!("distinct step {action}: {before:?} -> {after:?}");
        assert!(
            validate_transition_tiered(&model, &[], before, after, Some(action), &label).0,
            "{label} does not conform to CrashJournalClaim"
        );
    }
    // The ends a person meets are among them.
    for ends in [
        vec!["BootFresh", "Write", "Die", "Claim"],
        vec!["BootFresh", "Write", "Quit"],
        vec!["BootFresh", "Write", "HandOff", "Claim"],
        vec!["BootFromJournal", "Write", "Die", "Claim"],
        vec!["BootFromJournal", "Write", "Crash", "Claim"],
        vec!["BootSecondChance", "Write", "Die", "Claim"],
        vec!["BootFromJournal", "Write", "Settle", "Die", "Claim"],
    ] {
        assert!(
            all.iter().any(|schedule| schedule.starts_with(&ends)),
            "{ends:?} is replayed"
        );
    }
}

/// NEGATIVE CONTROL: `Buggy = 1` is rejected by the real steps — at the claim
/// of a running window's journal (the real claim takes nothing: its owner's
/// lock is held) and at every write (the real publish is one rename, never an
/// empty name). A pass of the committed model is therefore not vacuous.
#[test]
fn the_defects_reject_the_real_steps() {
    let buggy = interp::with_buggy(&crash_journal_claim_model(), 1);
    let rejected = |schedule: &[&'static str], at: &str| {
        let (mut refused, mut consumed) = (0, 0);
        let (_, steps) = replay(schedule, false, &mut refused, &mut consumed);
        steps.iter().any(|(before, action, after)| {
            *action == at && !buggy.successors(action, before).contains(after)
        })
    };
    assert!(
        rejected(&["BootFresh", "Write"], "Write"),
        "the unlink-first writer disagrees with the real publish"
    );
    // The committed machine's path to a live journal, then the mutant's claim.
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
