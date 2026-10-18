// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the crash journal to
//! `aterm_models_gui::crash_journal_claim_model` (`CrashJournalClaim`, PTY
//! keeper P1).
//!
//! Every schedule of the model up to seven steps — enough to reach every state
//! the model can reach, which the test checks — is replayed on the SHIPPING
//! owner and claim over a real directory: `JournalOwner::create`/`publish`
//! (the writer thread's two calls) for `Write` and `Settle`,
//! `JournalOwner::retire` for the quit's removal, `restore::write_quit_to` for
//! the quit's own layout (stamped with its run), and `claim_at_boot` — with
//! `classify_death` reading real crash markers in a real log dir — for every
//! later launch. The test plays the process around them: it arms the run's
//! crash marker at boot as the installed app arms it (`markers::arm`: locked
//! for the run's life, named for the start the journal is named for too) and
//! removes it on a clean end (what `atexit` and `clean_exit_now` do), releases
//! the owner's locks without a word on a death (what the kernel does to a
//! SIGKILLed process no forked child still shares a lock with: `Rig::end`),
//! and leaves the journal behind on the update's hand-off.
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
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};

use aterm_models_gui::crash_journal_claim_model;
use aterm_spec::interp::{self, State};
use aterm_spec::verify::validate_transition_tiered;

use crate::conformance_support::schedules;
use crate::crash_journal::tests::{layout, reaped_pid, scratch};
use crate::crash_journal::{self, Evidence, JournalHeader, JournalId, JournalOwner, QuitLayout};
use crate::crash_signal::{MarkerOwner, markers};

/// Where replays run: a journal directory, a log dir, and the pids their runs
/// are named for. A worker of [`the_real_journal_conforms_to_the_model`] keeps
/// one desk for all its replays, emptied between them ([`Desk::clear`]).
///
/// WHY (test-run speed): a fresh desk per replay made and removed two
/// directories in the system temp dir, the one every test binary of a run
/// makes its own in, and reaped one process for its pid: 1,852 replays, 7,408
/// directory makes and removes and 2,440 spawns of `/usr/bin/true` in one run.
/// Beside three `F_FULLFSYNC` writers those directory calls took 2.9-6.2 ms
/// each, 30.5 of the 210 thread-seconds the test spent in file system calls,
/// and a spawn and its reap 2.1-2.8 ms. An emptied directory is what the
/// shipping owner and claim see in a fresh one: they read names and the files
/// under them, never the directory's own metadata, and `clear` checks it
/// empty. A pid reaped earlier still names no process (the kernel hands a pid
/// out again only once its pid counter has wrapped), and the journal tells
/// runs apart by `(pid, start)`, whose start is the clock's at each replay.
pub(crate) struct Desk {
    dir: aterm_tempfile::TempDir,
    logs: aterm_tempfile::TempDir,
    /// Pids that WERE processes and are not any more, reaped as first needed:
    /// a replay names its first run for the first and each successor it adopts
    /// for the next, so no two runs of one replay share a pid.
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

    /// Empty both directories for the next replay, as fresh ones are: every
    /// entry a replay left is removed, and each directory is then read back
    /// empty.
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
    /// The update's successor runs and has not taken the journal over.
    successor: bool,
}

/// The quit's layout in the rig's directory, where `session.toml` sits beside
/// the journals.
const SESSION: &str = "session.toml";

impl Rig {
    fn new(consumed_first: bool) -> Self {
        Rig::on(Desk::new(), consumed_first)
    }

    /// A replay on `desk`, empty as [`Desk::clear`] leaves it.
    fn on(mut desk: Desk, consumed_first: bool) -> Self {
        // A pid no process has: the owner's liveness is its lock alone.
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

    /// The run starts: its crash marker is armed (empty, locked, the installed
    /// app's), before its journal, and named for the start the journal is
    /// named for (`JournalId::of_this_run`). `second`: the journal it
    /// reopened was a second chance's to give (`Lane::begin_probation`'s
    /// flag, ruling 285).
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

    /// THE SUCCESSOR'S COMMIT (`release_committed_readers`, then
    /// `Lane::adopt_predecessor` at its wake): a new run — its crash marker
    /// armed at its start, as `boot` arms one — takes the handed-off image
    /// over onto its own journal by the real `JournalOwner::adopt`, and from
    /// here the journal is that run's; then, and only then, its readers run
    /// on the live sessions (round eight of the update audit, finding 45).
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

    /// The process ends. `clean`: through an exit path, which removes its
    /// crash marker before the process goes. Its locks go with it however it
    /// ends, as the kernel releases a dead process's locks when no child
    /// forked under them is still short of its `exec`: by `LOCK_UN`, because a
    /// close alone leaves a lock held in the copy of its description that any
    /// sibling test's child forked meanwhile keeps until its `exec`.
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
        // Its drop releases the lock at once, as the kernel does.
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

    /// What a later launch reads of the quit's `session.toml` before it claims
    /// (`restore::take` then `JournalId::parse_stem`, as `main_entry` does):
    /// read, not taken, so a claim the model forbids, run anyway, leaves it
    /// for the next.
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

    /// A later launch's claim: the real one, over the real directory — after,
    /// on the consumed pass, an earlier windowed launch that did not claim
    /// read (and renamed) every run's evidence, as its boot does.
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
            // The successor ends before it took anything over: the parent left
            // no image (the shipping waiter takes one over before any reader
            // runs), so nothing of the journal is its to leave.
            "SuccessorDies" => self.successor = false,
            "Adopt" => self.adopt(),
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

/// Replay `schedule` on the real owner and claim, on `desk` (handed back
/// emptied) — with the evidence read
/// first by a launch that does not claim before every claim when
/// `consumed_first` (`consumed` counts the reads that found some). After every
/// step, where the committed model forbids a claim, the real claim runs anyway
/// and must leave the projection unchanged (`refused` counts those).
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

/// The shipping owner and claim refine `CrashJournalClaim` on every schedule
/// up to seven steps — each replayed twice, the second time with every claim's
/// evidence read (and renamed `.seen`) first by a launch that does not claim —
/// every state they reach satisfies every invariant, and between them they
/// reach EVERY state the model can reach. Each step is admitted by the
/// interpreter; each DISTINCT real transition is also validated once on both
/// tiers.
///
/// The machine has fourteen actions. `Write`, `Settle`, `Quit`,
/// `WriteQuitLayout`, `Adopt` and `Claim` carry real `#[refines]` anchors
/// (`JournalOwner::publish`/`retire`/`adopt`, `restore::write_quit_to`,
/// `claim_at_boot`). The other eight are the process around the journal,
/// which this runner plays and projects rather than code any journal function
/// runs.
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "BootFresh",
    reason = "The run's own start: the crash marker it arms is crash_signal's install; the \
              runner arms one in the real log dir and the journal is armed lazily at the \
              first Write."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "BootFromJournal",
    reason = "The run's own start after a reopened journal: Lane::begin_probation only sets \
              the header mark the runner writes through JournalOwner::publish; its 90 s \
              clock is crash_journal::tests::the_lane_writes_only_changes_at_the_bounded_rate."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "BootSecondChance",
    reason = "The run's own start after the brake let a stopped probation writer's journal \
              through (ruling 285): Lane::begin_probation's second-chance flag only sets the \
              header mark the runner writes through JournalOwner::publish \
              (crash_journal::tests::a_relapse_inside_probation_is_skipped_and_said)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "Crash",
    reason = "A fatal signal runs no journal code: crash_signal's handler writes its banner \
              into the run's own marker, which the runner writes, and the kernel releases the \
              locks as for Die."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "HandOff",
    reason = "The update parent's _exit after Commit is seamless::commit_and_exit, which \
              removes the crash marker and ends the process; the runner removes the marker \
              and drops the owner, whose drop releases its lock by LOCK_UN."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "Die",
    reason = "A SIGKILL, a signal or a panic runs no journal code: the runner drops the owner, \
              whose lock goes as the kernel releases a dead process's (a child forked \
              under it and not yet exec'd keeps it until its exec), leaving the marker."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "FinishWrite",
    reason = "Exists only in the Buggy = 1 unlink-first writer; the shipping publish is one \
              rename, which the_defects_reject_the_real_steps pins, and a reader racing 400 real \
              rewrites always finds a whole image (crash_journal::tests::\
              a_rewrite_never_leaves_the_name_without_a_whole_image)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "SuccessorDies",
    reason = "A successor's end before it took anything over runs no journal code: the \
              shipping Commit waiter takes the parent's image over before it releases a reader \
              (release_committed_readers, round eight of the update audit, finding 45; \
              crash_journal::tests::\
              a_committed_successor_that_dies_before_its_commit_wake_still_reopens_the_desk), so \
              the runner plays it only where the parent left no image."
)]
#[test]
fn the_real_journal_conforms_to_the_model() {
    let model = crash_journal_claim_model();
    let all = schedules(&model, ACTIONS, 7);
    assert!(
        all.len() > 60,
        "the enumeration reaches the schedules it exists for: {}",
        all.len()
    );
    // Each replay runs on a rig of its own (its worker's desk, emptied for it,
    // and its own journal and markers), so the replays are independent: they
    // run side by side (in turn they were 29.8 s of this test's 31.7 s on a
    // 6-core host), each exactly once, every step checked where it was
    // replayed, and the states they reach are gathered below.
    //
    // Each DISTINCT step is validated on both tiers by the worker that met it
    // first, between its replays, and not after the last replay, for speed on
    // a loaded machine: with the installed `ty` (0.13.0, no `--cases` co-process)
    // the 129 of them are 262 `ty` spawns, and run one after another once
    // every replay was done they were the last 5.4 s of a 63.3 s run beside
    // eight spinners and three `F_FULLFSYNC` writers. Beside replays that wait
    // on the file system they cost the wall nothing. With that and one
    // `Desk` per worker this test took 43.9-49.1 s there instead of
    // 53.4-56.6 s (13.7 CPU-s instead of 16.9), and 2.1-2.3 s alone instead
    // of 3.8-3.9 s.
    //
    // FOUR AT MOST (test-run speed). A replay mostly waits on the
    // shipping writers' syncs (`restore`'s `F_FULLFSYNC`, the journal's
    // `fsync`), so the wall stops falling at about four threads (run alone on
    // 14 cores: 23.6 s on one, 18.7 s on two, 15.6 s on four, 15.7 s on
    // fourteen), and every thread past that only spins in the kernel: 14.4
    // CPU-s on four against 51 on fourteen, three interleaved runs each. That
    // kernel time also stalled every other test's file system calls: beside
    // it, `config_watcher`'s entry-cap test (4,097 files) took 15.6 s and
    // 8.1 s against fourteen, 2.1 s and 1.2 s against four. Those runs had
    // `restore`'s sync as an `F_FULLFSYNC`; since `durable::sync_all`'s
    // unit-test arm made it a plain `fsync` (2026-10-05), this test takes
    // 4.1-7.3 s alone at load 20-22. The cap of four was not re-measured.
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
    // The ends a person meets are among them.
    for ends in [
        vec!["BootFresh", "Write", "Die", "Claim"],
        vec!["BootFresh", "Write", "Quit"],
        vec!["BootFresh", "Write", "WriteQuitLayout", "Quit"],
        vec!["BootFresh", "Write", "WriteQuitLayout", "Die", "Claim"],
        vec!["BootFresh", "Write", "WriteQuitLayout", "Crash", "Claim"],
        vec!["BootFresh", "Write", "HandOff", "Claim"],
        vec!["BootFresh", "Write", "HandOff", "Adopt", "Die", "Claim"],
        vec!["BootFresh", "Write", "HandOff", "Adopt", "Crash", "Claim"],
        vec!["BootFresh", "Write", "HandOff", "Adopt", "Quit"],
        vec!["BootFresh", "HandOff", "SuccessorDies"],
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
