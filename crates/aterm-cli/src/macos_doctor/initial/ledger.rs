// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The setup run's ledger (design §6.5, §6.9): per stage, how often it began since it last
//! ended, and the boot and the time of its latest begin; and the set of stages running.
//!
//! Liveness is the profile flock and the hold that began the stage, never a pid: a stage is
//! alive only while profile.lock is held by the hold its begin recorded ([`Stamp::run`], the mark
//! each hold writes in the lock file), in the boot it began in. One recorded running while
//! nobody holds the lock, while another holds it (a choice's save, `aterm doctor --apply`), or
//! in another boot, was cut off by aterm stopping. The next launch's pass resumes it once with
//! no press; one that had already begun twice is parked, and the other stages go on. A stage
//! that runs past [`HANG_LIMIT`] of awake time with no end is parked the same way by the first
//! reading of the run that sees it ([`Ledger::mark_hung`]), so a wedge, not only a crash, ends
//! at a calm row; a stage cut off after that counts it as one stop, so it resumes once. A park
//! is a pause, never a hold: it comes due by itself ([`PARK_PAUSE`], [`Ledger::comes_due`]),
//! longer after each park in a row, and the stage then begins once more, parking again should
//! aterm stop in it again; the run's worker wakes for it ([`Ledger::park_due`]). Check again
//! clears every park at once ([`Ledger::retry`]). A valid newer build with a different exact
//! commit gives only its owned, unfinished stage a fresh budget, its park and pause gone with
//! it; a legacy unattributed budget may be adopted once ([`Ledger::software_for`]). A quit or an
//! update's handoff in the middle of
//! a stage is no crash ([`Ledger::interrupt`]), unless the handoff is refused
//! ([`Ledger::refused`]); from its note until it goes or is refused, no stage begins
//! ([`quitting`]), so it cuts off only stages its note names. Nor is a restart of the Mac, or a
//! logout, that ended aterm with no note (its quit cut short, a forced restart, a power loss):
//! a stage whose begin ran in another known boot resumes without counting that begin
//! ([`Ledger::recover`]), so only an end within one boot is aterm stopping in it.

use super::*;

#[path = "ledger_history.rs"]
mod history;
pub use history::{DisplayHistory, LastRead, SetupHistoryStore};

/// The ledger's file, beside the progress record rather than in it: the progress record is
/// read and written whole by every setup path and by an older aterm, which drops a field it
/// does not know when it writes the record again. Here an older aterm never writes it, a stale
/// one is never read ([`Ledger::revision`]), and one a newer aterm wrote of the run in force is
/// left as it is ([`Loaded::frozen`]).
pub(super) const LEDGER: &str = "setup-ledger.json";

/// The ledger's short lock: the pass that holds profile.lock writes it, and so do a reading of
/// the run that marks a hung stage and a Check again or a quit, which do not hold that lock.
const LEDGER_LOCK: &str = "setup-ledger.lock";

/// The format this build writes in the ledger's `format`: a later one is a newer aterm's.
pub(super) const LEDGER_FORMAT: u64 = 2;

/// How long a parked stage waits before it begins again by itself, after the park that is the
/// `n`th in a row since it last ended: an hour after the first, so a stage that stops aterm costs
/// a stop an hour at most, twice as long after each later park, and never more than a day.
pub(super) const PARK_PAUSE: aterm_types::retry_schedule::Backoff =
    aterm_types::retry_schedule::Backoff::new(
        std::time::Duration::from_secs(60 * 60),
        std::time::Duration::from_secs(24 * 60 * 60),
    );

/// How long a stage may run with no end before the run's next reading parks it: twice the
/// longest budget any one step of a stage keeps (a GitHub clone's 30 minutes), so a step that
/// is slow but alive ends first. It counts the Mac's awake time ([`awake_secs`]) where the
/// stage's begin and the reading share a boot, so a lid closed mid-clone adds nothing; the wall
/// clock otherwise. A trade-off: a Repositories stage that clones for more than an hour reads
/// parked until it ends, which then clears the park by itself.
pub(super) const HANG_LIMIT: u64 = 60 * 60;

const STAGES: usize = Stage::ALL.len();

/// The running software's ordered build and exact source identity, when both are known.
/// Unknown or dirty builds supply no evidence that a parked stage has newer code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SoftwareEpoch {
    pub(super) build: u64,
    pub(super) commit: [u8; 20],
}

impl SoftwareEpoch {
    fn valid(self) -> bool {
        self.build > 0 && self.commit != [0; 20]
    }

    pub(super) fn current() -> Option<Self> {
        Self::from_stamp(aterm_stamp::current())
    }

    pub(super) fn from_stamp(stamp: &aterm_stamp::Stamp) -> Option<Self> {
        let build = stamp
            .build_number
            .parse::<u64>()
            .ok()
            .filter(|build| *build > 0)?;
        let mut commit = [0; 20];
        (stamp.git_dirty == "false"
            && aterm_codec::hex::decode_into(stamp.git_commit_full.as_bytes(), &mut commit)
            && commit != [0; 20])
            .then_some(Self { build, commit })
    }
}

/// Unattributed budgets may be adopted once. Unread metadata never supplies that authority:
/// it survives a rewrite as unread until an explicit Check again gives the budget back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Software {
    #[default]
    Unattributed,
    Known(SoftwareEpoch),
    Unread,
}

impl Software {
    fn record(self) -> Value {
        match self {
            Self::Unattributed => Value::from("unattributed"),
            Self::Unread => Value::from("unread"),
            Self::Known(epoch) => json!({
                "build": epoch.build,
                "commit": aterm_codec::hex::encode_lower(&epoch.commit),
            }),
        }
    }

    fn parse(value: &Value) -> Self {
        match value.as_str() {
            Some("unattributed") => return Self::Unattributed,
            Some("unread") => return Self::Unread,
            _ => {}
        }
        let Some(object) = value.as_object().filter(|object| object.len() == 2) else {
            return Self::Unread;
        };
        let Some(build) = object.get("build").and_then(Value::as_u64) else {
            return Self::Unread;
        };
        let Some(commit) = object.get("commit").and_then(Value::as_str) else {
            return Self::Unread;
        };
        let mut decoded = [0; 20];
        if !aterm_codec::hex::decode_into(commit.as_bytes(), &mut decoded) {
            return Self::Unread;
        }
        let epoch = SoftwareEpoch {
            build,
            commit: decoded,
        };
        if epoch.valid() {
            Self::Known(epoch)
        } else {
            Self::Unread
        }
    }
}

/// One stage's begins since it last ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Attempt {
    /// Begins since the stage last ended or Check again cleared it.
    pub(super) began: u8,
    /// The boot its latest begin ran in (`kern.bootsessionuuid`); 0 when that could not be read.
    pub(super) boot: u128,
    /// Its latest begin, in seconds since the epoch.
    pub(super) at: u64,
    /// Its latest begin, in seconds the Mac had been awake in `boot` ([`awake_secs`]); `None`
    /// when that could not be read, or an older aterm began it.
    pub(super) awake: Option<u64>,
    /// The profile.lock hold its latest begin ran under ([`Stamp::run`]); 0 when that could not
    /// be read, or an older aterm began it.
    pub(super) run: u128,
    /// Parks in a row since it last ended or Check again cleared it: the next park's pause
    /// grows with it ([`PARK_PAUSE`]).
    pub(super) parks: u8,
    /// When its latest park was set, in seconds since the epoch.
    pub(super) parked: u64,
}

impl Attempt {
    /// A park set at `now`: one more in a row, its pause counted from `now`.
    fn parked_at(&mut self, now: u64) {
        self.parks = self.parks.saturating_add(1);
        self.parked = now;
    }
}

/// What a begin records of the pass that makes it ([`Ledger::begin`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Stamp {
    /// This boot, where it could be read ([`boot_session`]).
    pub(super) boot: Option<u128>,
    /// Now, in seconds since the epoch.
    pub(super) at: u64,
    /// Now, in seconds the Mac has been awake in this boot ([`awake_secs`]), where read.
    pub(super) awake: Option<u64>,
    /// The mark of the profile.lock hold the pass runs under, where read: each hold writes a
    /// fresh one in the lock file as it takes the lock.
    pub(super) run: Option<u128>,
    /// The software captured once by the owning pass, shared with its parallel lane.
    pub(super) software: Option<SoftwareEpoch>,
}

/// One run revision's ledger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Ledger {
    /// The run revision it belongs to: a ledger of any other is read as empty.
    pub(super) revision: u64,
    /// The stages begun and not ended ([`Stage::bit`]): a set, since lanes run stages at once.
    pub(super) running: u16,
    /// The stages parked after aterm stopped twice while they ran.
    pub(super) crashed: u16,
    /// The stages parked after running past [`HANG_LIMIT`].
    pub(super) hung: u16,
    /// The running stages a quit or an update's handoff cut off: no crash.
    pub(super) quit: u16,
    pub(super) attempts: [Attempt; STAGES],
    /// A person pressed Check again since the run's last pass began: that pass starts every
    /// other automatic try of the run afresh ([`Self::take_afresh`]), under profile.lock.
    pub(super) afresh: bool,
    /// Each stage owns its budget's attribution; a scoped pass cannot spend a sibling's epoch.
    pub(super) software: [Software; STAGES],
}

/// Whether a stage begins ([`Ledger::begin`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Admit {
    Run,
    /// It is done: a finished stage never runs again in its run.
    Done,
    Parked(Park),
    /// aterm is quitting, or an update's Commit is about to be written ([`quitting`]): the stage
    /// does not begin, and the pass stops as the quit cuts it off, so the quit cuts off no stage
    /// its note did not name.
    Quitting,
}

/// Why a stage waits out a pause before it begins again ([`PARK_PAUSE`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Park {
    /// aterm stopped twice while it ran.
    Crashed,
    /// It ran past [`HANG_LIMIT`] with no end.
    Hung,
}

/// What a stage's row says of the ledger ([`Ledger::verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Nothing to say.
    Idle,
    /// Running in a live process.
    Running,
    /// aterm stopped while it ran: it runs again, `running` now or at the run's next pass.
    Resuming {
        running: bool,
    },
    Parked(Park),
    /// Still running, past [`HANG_LIMIT`], under the hold that began it: a reading parks it as
    /// hung, but it ends by itself or when aterm quits, and no press reaches it meanwhile (the
    /// wedged pass holds profile.lock and the setup slot), so its row offers no Check again.
    Overdue,
}

/// What a reading of the run knows of the process that holds it.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Live {
    /// Whether profile.lock is held, by any process.
    pub(super) active: bool,
    /// The mark the latest hold of profile.lock wrote in it, where read: with `active`, the hold
    /// that has it now, and a stage is alive only under the hold that began it.
    pub(super) holder: Option<u128>,
    /// This boot, where it could be read.
    pub(super) boot: Option<u128>,
    /// Now, in seconds since the epoch.
    pub(super) now: u64,
    /// Now, in seconds the Mac has been awake in this boot, where read.
    pub(super) awake: Option<u64>,
}

impl Live {
    /// The reading this process takes of the run now; its holder is read by the snapshot.
    pub(super) fn now(active: bool) -> Self {
        Self {
            active,
            holder: None,
            boot: boot_session(),
            now: now_secs(),
            awake: awake_secs(),
        }
    }
}

/// This boot of the Mac (`kern.bootsessionuuid`, its 128 bits big-endian), or `None` where it
/// cannot be read or reads as the all-zero id a damaged answer would.
pub(super) fn boot_session() -> Option<u128> {
    astream_sys::boot_session().map(u128::from_be_bytes)
}

/// Seconds since the epoch on this Mac's clock.
pub(super) fn now_secs() -> u64 {
    epoch_secs(std::time::SystemTime::now())
}

/// Seconds this Mac has been awake since it booted (`mach_absolute_time`, which stops while the
/// Mac sleeps), or `None` where that cannot be read. Compared only within one boot.
pub(super) fn awake_secs() -> Option<u64> {
    aterm_uds::process::awake_ms().map(|ms| ms / 1000)
}

/// A defect the conformance test's negative control plants in the code under test, one at a
/// time and on its own thread, to show the walk catches it
/// (`tests::ledger_conformance`). Nothing plants one outside a test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Defect {
    /// [`Ledger::begin`] does not ask whether the stage is done.
    BeginIgnoresDone,
    /// [`Ledger::begin`] does not ask whether the stage is parked.
    BeginIgnoresPark,
    /// [`Ledger::begin`] does not ask whether aterm is quitting.
    BeginIgnoresQuit,
    /// [`Ledger::recover`] resumes a stage aterm stopped in twice.
    RecoverNeverParks,
    /// [`Ledger::recover`] parks a stage with no pause to come due: the hold only a person's
    /// Check again lifted.
    ParkWithoutDue,
    /// [`Ledger::recover`] counts a stage the Mac restarted under as one aterm stopped in.
    RestartCountsAsCrash,
    /// [`Ledger::refused`] leaves the notes of the handoff it takes back.
    RefusalKeepsNotes,
    /// [`super::kept::Kept::hold`] leaves out a kept key.
    HoldForgetsKept,
    /// [`super::resume::owes`] reads a run as owing no pass, so its login job goes before it
    /// finishes.
    NothingOwed,
    /// [`super::kept::Kept::hold`] leaves the first change after a read-back as it is, as if
    /// whoever made it had chosen it: the key is kept with no try again.
    KeepsFirstChange,
}

#[cfg(test)]
thread_local! {
    /// The defect planted on this thread ([`Defect`]).
    pub(super) static PLANTED: std::cell::Cell<Option<Defect>> = const { std::cell::Cell::new(None) };
}

/// Whether `defect` is planted on this thread: never outside a test.
pub(super) fn planted(defect: Defect) -> bool {
    #[cfg(test)]
    {
        PLANTED.with(|planted| planted.get() == Some(defect))
    }
    #[cfg(not(test))]
    {
        let _ = defect;
        false
    }
}

impl Ledger {
    /// An empty ledger of run `revision`.
    pub(super) fn of(revision: u64) -> Self {
        Self {
            revision,
            ..Self::default()
        }
    }

    const fn index(stage: Stage) -> usize {
        stage as usize
    }

    /// `stage`'s begins since it last ended.
    pub(super) const fn attempt(&self, stage: Stage) -> Attempt {
        self.attempts[Self::index(stage)]
    }

    /// Why `stage` waits out a park, if it does.
    pub(super) const fn parked(&self, stage: Stage) -> Option<Park> {
        if self.hung & stage.bit() != 0 {
            Some(Park::Hung)
        } else if self.crashed & stage.bit() != 0 {
            Some(Park::Crashed)
        } else {
            None
        }
    }

    /// Reconcile only an owned, unfinished stage after its interrupted begin was recovered.
    /// A legacy unattributed budget gets one fresh adoption; it proves no earlier build.
    /// Known budgets require both a strictly larger build and a different exact commit.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "AdoptSoftwareA",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "AdoptSoftwareB",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "NewSoftwareA",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "NewSoftwareB",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "RetainSoftwareA",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "RetainSoftwareB",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(super) fn software_for(
        &mut self,
        stage: Stage,
        done: u16,
        current: Option<SoftwareEpoch>,
        root: &Path,
    ) -> bool {
        let bit = stage.bit();
        if quitting(root) || done & bit != 0 || self.running & bit != 0 || self.quit & bit != 0 {
            return false;
        }
        let Some(current) = current.filter(|epoch| epoch.valid()) else {
            return false;
        };
        let index = Self::index(stage);
        match self.software[index] {
            Software::Unattributed => {}
            Software::Known(before)
                if current.build > before.build && current.commit != before.commit => {}
            Software::Known(_) | Software::Unread => return false,
        }
        self.software[index] = Software::Known(current);
        self.attempts[index] = Attempt::default();
        self.crashed &= !bit;
        self.hung &= !bit;
        true
    }

    /// Begin `stage`, as `stamp` says, unless aterm is quitting `root`'s setup (the process's
    /// latch, [`quitting`], which this reads itself, under the ledger's lock), the run's progress
    /// has it `done`, or it is parked and its pause has not come due at `stamp` (one that has is
    /// lifted, [`Self::comes_due`]): each begin counts.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "BeginA",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "BeginB",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "BeginDueA",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "BeginDueB",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(super) fn begin(&mut self, stage: Stage, done: u16, stamp: Stamp, root: &Path) -> Admit {
        if !planted(Defect::BeginIgnoresQuit) && quitting(root) {
            return Admit::Quitting;
        }
        if done & stage.bit() != 0 && !planted(Defect::BeginIgnoresDone) {
            return Admit::Done;
        }
        // A newer build's fresh budget first: it lifts the park outright, so the stage begins
        // as any first begin does; else a park whose pause has passed comes due.
        if stamp.software.is_some() {
            self.software_for(stage, done, stamp.software, root);
        }
        self.comes_due(stage, stamp.at);
        if let Some(park) = self.parked(stage)
            && !planted(Defect::BeginIgnoresPark)
        {
            return Admit::Parked(park);
        }
        let attempt = &mut self.attempts[Self::index(stage)];
        attempt.began = attempt.began.saturating_add(1);
        attempt.boot = stamp.boot.unwrap_or(0);
        attempt.at = stamp.at;
        attempt.awake = stamp.awake;
        attempt.run = stamp.run.unwrap_or(0);
        self.running |= stage.bit();
        self.quit &= !stage.bit();
        Admit::Run
    }

    /// `stage` ended, done or not: it is no longer running, its begins are spent and its park,
    /// if a reading marked one meanwhile, is lifted.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "EndA",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "EndB",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "FailA",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "FailB",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(super) fn end(&mut self, stage: Stage) {
        let bit = stage.bit();
        self.running &= !bit;
        self.crashed &= !bit;
        self.hung &= !bit;
        self.quit &= !bit;
        self.attempts[Self::index(stage)] = Attempt::default();
    }

    /// Park `stage` as hung at `now`, if it still runs the begin of `at` and is not parked
    /// already. Whether that changed the ledger.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "HangA",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "HangB",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(super) fn mark_hung(&mut self, stage: Stage, at: u64, now: u64) -> bool {
        let bit = stage.bit();
        if self.running & bit == 0 || self.hung & bit != 0 || self.attempt(stage).at != at {
            return false;
        }
        self.hung |= bit;
        self.attempts[Self::index(stage)].parked_at(now);
        true
    }

    /// When `stage`'s park comes due ([`PARK_PAUSE`]), in seconds since the epoch, read at `now`:
    /// at once for a park set after `now`, the clock set back since, as a pause that cannot be
    /// measured ([`aterm_types::retry_schedule::Backoff::due_by`]); `None` when it is not parked,
    /// or is parked with no pause, which nothing but a press would lift.
    pub(super) fn park_due(&self, stage: Stage, now: u64) -> Option<u64> {
        let attempt = self.attempt(stage);
        self.parked(stage)
            .filter(|_| attempt.parks > 0)
            .map(|_| PARK_PAUSE.due_by(attempt.parked, u32::from(attempt.parks), now))
    }

    /// `stage`'s park came due at `now` ([`Self::park_due`]), with no press: it is lifted, and the
    /// begin that asked ([`Self::begin`]) is the stage's last before another stop parks it again,
    /// for longer. A stage still running, or one whose pause is still ahead, stays as it is.
    /// Whether that changed the ledger.
    fn comes_due(&mut self, stage: Stage, now: u64) -> bool {
        let bit = stage.bit();
        let Some(due) = self.park_due(stage, now) else {
            return false;
        };
        let attempt = &mut self.attempts[Self::index(stage)];
        if self.running & bit != 0 || now < due {
            return false;
        }
        self.crashed &= !bit;
        self.hung &= !bit;
        attempt.began = 1;
        true
    }

    /// A person pressed Check again: every park is lifted, every stage's begins and parks start
    /// from none, and the run's next pass starts every other automatic try afresh
    /// ([`Self::afresh`]). Whether that changed the ledger.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "CheckAgain",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(super) fn retry(&mut self) -> bool {
        let before = *self;
        self.crashed = 0;
        self.hung = 0;
        for attempt in &mut self.attempts {
            attempt.began = 0;
            attempt.parks = 0;
            attempt.parked = 0;
        }
        self.afresh = true;
        for software in &mut self.software {
            if *software == Software::Unread {
                *software = Software::Unattributed;
            }
        }
        *self != before
    }

    /// Whether a person pressed Check again since a pass last took it, which this takes: the
    /// pass that holds profile.lock then starts the run's other automatic tries afresh.
    pub(super) fn take_afresh(&mut self) -> bool {
        std::mem::take(&mut self.afresh)
    }

    /// aterm is quitting, or an update's handoff is about to write its Commit, in the process
    /// `process`: the stages it runs now are cut off by no crash, so the next launch resumes them
    /// without counting this begin. A stage another process began (the login job's, while a
    /// window quits, or a window's while the job is stopped) is left as it is: should that one
    /// stop in it with no note, it counts. A begin that names no hold (an older aterm's, or a
    /// mark that could not be read) is taken as this process's. Whether that changed the ledger.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "NoteQuit",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(super) fn interrupt(&mut self, process: u32) -> bool {
        let before = self.quit;
        self.quit |= self.running & self.begun_by(process);
        self.quit != before
    }

    /// The stages whose last begin ran under a profile.lock hold of the process `process`
    /// ([`hold_process`]), or under no hold a mark names (an older aterm's begin, a mark that
    /// could not be read), which every process takes as its own.
    fn begun_by(&self, process: u32) -> u16 {
        Stage::ALL
            .into_iter()
            .filter(|stage| {
                let run = self.attempt(*stage).run;
                run == 0 || hold_process(run) == process
            })
            .fold(0, |bits, stage| bits | stage.bit())
    }

    /// The handoff that noted the stages it would cut off ([`Self::interrupt`]) in the process
    /// `process` did not go: aterm goes on, so aterm stopping in one of them later is a crash
    /// again, which counts. Another process's notes stay (the login job's, as a logout ends it).
    /// Whether that changed the ledger.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "RefuseQuit",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(super) fn refused(&mut self, process: u32) -> bool {
        if planted(Defect::RefusalKeepsNotes) {
            return false;
        }
        let before = self.quit;
        self.quit &= !self.begun_by(process);
        self.quit != before
    }

    /// The first pass after aterm stopped (the login job's, or a window's), holding profile.lock at
    /// `now` in the Mac's boot `boot` (where read), so whatever the ledger or the progress record
    /// (`legacy`, what an older aterm or a cut-off write left) records running was cut off: a stage
    /// the progress has `done` is spent; one a quit cut off, or one whose begin ran in another boot
    /// than `boot` (the Mac restarted, or its account logged out, under it: an interruption,
    /// however aterm ended), gives its begin back; and any other counts its begin and parks once it
    /// began twice, its pause counted from `now`. A boot that cannot be read, now or at the begin,
    /// tells no restart. A stage a reading parked as hung while it ran counts as stopped in, quit
    /// or restart or not: its hung park goes, so it resumes once with no press, and parks the
    /// second time. Nothing is running after. Whether that changed the ledger.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "Login",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "Relaunch",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(super) fn recover(
        &mut self,
        done: u16,
        legacy: Option<Stage>,
        boot: Option<u128>,
        now: u64,
    ) -> bool {
        let before = *self;
        let recorded = self.running | legacy.map_or(0, Stage::bit);
        for stage in Stage::ALL {
            let bit = stage.bit();
            if recorded & bit == 0 {
                continue;
            }
            let attempt = &mut self.attempts[Self::index(stage)];
            let restarted = attempt.boot != 0
                && boot.is_some_and(|boot| boot != attempt.boot)
                && !planted(Defect::RestartCountsAsCrash);
            if done & bit != 0 {
                *attempt = Attempt::default();
                self.crashed &= !bit;
                self.hung &= !bit;
            } else if (self.quit & bit != 0 || restarted) && self.hung & bit == 0 {
                attempt.began = attempt.began.saturating_sub(1);
            } else {
                // Parked as hung while it ran: the park says so again should it stop a second
                // time, and a crash's otherwise.
                let hung = self.hung & bit != 0;
                self.hung &= !bit;
                attempt.began = attempt.began.max(1);
                if attempt.began >= 2 && !planted(Defect::RecoverNeverParks) {
                    if hung {
                        self.hung |= bit;
                    } else {
                        self.crashed |= bit;
                    }
                    if !planted(Defect::ParkWithoutDue) {
                        attempt.parked_at(now);
                    }
                }
            }
        }
        self.running = 0;
        self.quit = 0;
        *self != before
    }

    /// What `stage`'s row says, for a run whose progress has `done` and records `legacy`
    /// running, read as `live` says.
    pub(super) fn verdict(
        &self,
        stage: Stage,
        done: u16,
        legacy: Option<Stage>,
        live: Live,
    ) -> Verdict {
        let bit = stage.bit();
        if done & bit != 0 {
            return Verdict::Idle;
        }
        let attempt = self.attempt(stage);
        let recorded = (self.running | legacy.map_or(0, Stage::bit)) & bit != 0;
        let same_boot = attempt.boot == 0 || live.boot.is_none_or(|boot| boot == attempt.boot);
        // Alive only under the hold that began it: another hold of the lock (a choice's save,
        // `aterm doctor --apply`, the Widgets follow-up) says nothing of a stage a stopped aterm
        // left. A begin an older aterm recorded names no hold, and any hold keeps it alive.
        let ours = attempt.run == 0 || live.holder == Some(attempt.run);
        let alive = recorded && live.active && same_boot && ours;
        // Past the hang limit and still alive: parked as hung, or due to be, yet still running
        // where Check again cannot reach it.
        if alive && (self.hung & bit != 0 || self.overdue(stage, live).is_some()) {
            return Verdict::Overdue;
        }
        if let Some(park) = self.parked(stage) {
            return Verdict::Parked(park);
        }
        if !recorded {
            // Resumed by this launch's pass, which has not reached it yet.
            return if attempt.began > 0 {
                Verdict::Resuming { running: false }
            } else {
                Verdict::Idle
            };
        }
        if alive {
            return if attempt.began >= 2 {
                Verdict::Resuming { running: true }
            } else {
                Verdict::Running
            };
        }
        // Cut off by a quit, or by a restart (its begin ran in another known boot): it resumes,
        // as the next pass's recovery gives it its begin back ([`Self::recover`]).
        if self.quit & bit == 0 && same_boot && attempt.began.max(1) >= 2 {
            Verdict::Parked(Park::Crashed)
        } else {
            Verdict::Resuming { running: false }
        }
    }

    /// The begin of `stage` that has run past [`HANG_LIMIT`] in the live process, if it has: in
    /// awake time where the begin and the reading share a known boot, so sleep counts for
    /// nothing, and on the wall clock otherwise.
    pub(super) fn overdue(&self, stage: Stage, live: Live) -> Option<u64> {
        let attempt = self.attempt(stage);
        let ran = match (attempt.awake, live.awake) {
            (Some(began), Some(now)) if attempt.boot != 0 && live.boot == Some(attempt.boot) => {
                now.saturating_sub(began)
            }
            _ => live.now.saturating_sub(attempt.at),
        };
        (self.running & stage.bit() != 0 && self.hung & stage.bit() == 0 && ran >= HANG_LIMIT)
            .then_some(attempt.at)
    }

    pub(super) fn record(&self) -> Value {
        let stages: Vec<Value> = Stage::ALL
            .iter()
            .filter(|stage| self.attempt(**stage) != Attempt::default())
            .map(|stage| {
                let attempt = self.attempt(*stage);
                json!({
                    "stage": *stage as u8,
                    "began": attempt.began,
                    "boot": format!("{:032x}", attempt.boot),
                    "at": attempt.at,
                    "awake": attempt.awake,
                    "run": format!("{:032x}", attempt.run),
                    "parks": attempt.parks,
                    "parked": attempt.parked,
                })
            })
            .collect();
        json!({
            "profile": PROFILE,
            "format": LEDGER_FORMAT,
            "revision": self.revision,
            "running": self.running,
            "crashed": self.crashed,
            "hung": self.hung,
            "quit": self.quit,
            "afresh": self.afresh,
            "stages": stages,
            "software": self.software.iter().map(|software| software.record()).collect::<Vec<_>>(),
        })
    }

    pub(super) fn parse(value: &Value) -> Result<Self, Unread> {
        if value["profile"].as_str() != Some(PROFILE) {
            return Err(Unread::Damaged);
        }
        match value.get("format").map(Value::as_u64) {
            Some(Some(format)) if format > LEDGER_FORMAT => return Err(Unread::Newer),
            Some(Some(_)) => {}
            _ => return Err(Unread::Damaged),
        }
        let bits = |field: &str| -> Result<u16, Unread> {
            let bits = value[field].as_u64().ok_or(Unread::Damaged)?;
            if bits & !u64::from(Stage::KNOWN) != 0 {
                return Err(Unread::Newer);
            }
            u16::try_from(bits).map_err(|_| Unread::Damaged)
        };
        let mut ledger = Self {
            revision: value["revision"].as_u64().ok_or(Unread::Damaged)?,
            running: bits("running")?,
            crashed: bits("crashed")?,
            hung: bits("hung")?,
            quit: bits("quit")?,
            attempts: [Attempt::default(); STAGES],
            // A field an older aterm's ledger has not: no press waits.
            afresh: match value.get("afresh") {
                None | Some(Value::Null) => false,
                Some(afresh) => afresh.as_bool().ok_or(Unread::Damaged)?,
            },
            software: [Software::Unread; STAGES],
        };
        if value["format"].as_u64() == Some(1) && value.get("software").is_none() {
            ledger.software = [Software::Unattributed; STAGES];
        } else if let Some(software) = value
            .get("software")
            .and_then(Value::as_array)
            .filter(|software| software.len() == STAGES)
        {
            for (stored, value) in ledger.software.iter_mut().zip(software) {
                *stored = Software::parse(value);
            }
        }
        for entry in value["stages"].as_array().ok_or(Unread::Damaged)? {
            let index = entry["stage"].as_u64().ok_or(Unread::Damaged)?;
            let index = usize::try_from(index).map_err(|_| Unread::Damaged)?;
            if index >= STAGES {
                return Err(Unread::Newer);
            }
            ledger.attempts[index] = Attempt {
                began: entry["began"]
                    .as_u64()
                    .and_then(|began| u8::try_from(began).ok())
                    .ok_or(Unread::Damaged)?,
                boot: entry["boot"]
                    .as_str()
                    .filter(|boot| !boot.is_empty() && boot.bytes().all(|b| b.is_ascii_hexdigit()))
                    .and_then(|boot| u128::from_str_radix(boot, 16).ok())
                    .ok_or(Unread::Damaged)?,
                at: entry["at"].as_u64().ok_or(Unread::Damaged)?,
                // Fields an older aterm's ledger has not: none is unknown.
                awake: match entry.get("awake") {
                    None | Some(Value::Null) => None,
                    Some(awake) => Some(awake.as_u64().ok_or(Unread::Damaged)?),
                },
                run: match entry.get("run") {
                    None => 0,
                    Some(run) => run
                        .as_str()
                        .filter(|run| !run.is_empty() && run.bytes().all(|b| b.is_ascii_hexdigit()))
                        .and_then(|run| u128::from_str_radix(run, 16).ok())
                        .ok_or(Unread::Damaged)?,
                },
                // An older aterm's park kept no pause: it reads as one park, set at no time, so it
                // comes due at once and is never a hold.
                parks: match entry.get("parks") {
                    None => 1,
                    Some(parks) => parks
                        .as_u64()
                        .and_then(|parks| u8::try_from(parks).ok())
                        .ok_or(Unread::Damaged)?,
                },
                parked: match entry.get("parked") {
                    None => 0,
                    Some(parked) => parked.as_u64().ok_or(Unread::Damaged)?,
                },
            };
        }
        Ok(ledger)
    }
}

/// Why the ledger file was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unread {
    /// A newer aterm wrote it: left exactly as it is.
    Newer,
    /// It cannot be read: this build's own scratch, read as empty and written again.
    Damaged,
}

/// The ledger of one run revision, as read.
#[derive(Clone, Copy, Debug)]
pub(super) struct Loaded {
    pub(super) ledger: Ledger,
    /// A newer aterm wrote the file of this revision: nothing of it is used, and it is never
    /// written.
    pub(super) frozen: bool,
}

fn read_value(root: &Path) -> Result<Option<Value>, Unread> {
    match crate::macos_doctor::read_record_bytes(&root.join(LEDGER)) {
        Ok(bytes) => aterm_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Unread::Damaged),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Unread::Damaged),
    }
}

fn loaded(value: Result<Option<&Value>, Unread>, revision: u64) -> Loaded {
    let read = value.and_then(|value| match value {
        Some(value) => match Ledger::parse(value) {
            Err(Unread::Newer) if value["revision"].as_u64().is_some_and(|of| of != revision) => {
                Ok(None)
            }
            read => read.map(Some),
        },
        None => Ok(None),
    });
    match read {
        Ok(Some(ledger)) if ledger.revision == revision => Loaded {
            ledger,
            frozen: false,
        },
        Ok(_) | Err(Unread::Damaged) => Loaded {
            ledger: Ledger::of(revision),
            frozen: false,
        },
        Err(Unread::Newer) => Loaded {
            ledger: Ledger {
                software: [Software::Unread; STAGES],
                ..Ledger::of(revision)
            },
            frozen: true,
        },
    }
}

/// `revision`'s ledger: empty for a ledger of another revision (a newer aterm's among them,
/// which is stale as any other's and written over once this run's changes), a damaged one,
/// none, or one a newer aterm wrote of this revision or of none it names (then
/// [`Loaded::frozen`]).
pub(super) fn load(root: &Path, revision: u64) -> Loaded {
    let value = read_value(root);
    loaded(
        value.as_ref().map(Option::as_ref).map_err(|why| *why),
        revision,
    )
}

/// Apply `change` to `revision`'s ledger under its short lock, and save it when that changed
/// it, unless a newer aterm wrote the file ([`Loaded::frozen`]): then `change` sees an empty
/// ledger, and the file is left as it is.
///
/// # Errors
/// The lock cannot be taken, or the changed ledger cannot be saved.
pub(super) fn update<T>(
    root: &Path,
    revision: u64,
    change: impl FnOnce(&mut Ledger) -> T,
) -> Result<T, String> {
    let _lock = HeldLock::take_named(root, LEDGER_LOCK)?;
    let value = read_value(root);
    let Loaded { mut ledger, frozen } = loaded(
        value.as_ref().map(Option::as_ref).map_err(|why| *why),
        revision,
    );
    let before = ledger;
    let out = change(&mut ledger);
    if !frozen && ledger != before {
        let mut record = ledger.record();
        history::preserve(value.as_ref().ok().and_then(Option::as_ref), &mut record);
        write_json(&root.join(LEDGER), &record)?;
    }
    Ok(out)
}

/// A stage begun in the ledger, which ends it there when the pass leaves the stage by an error
/// before its end is recorded ([`Self::ended`]): an error is the stage's failure, not aterm
/// stopping. A panic leaves it running, as aterm stopping there would: the worker's supervisor
/// starts the run's worker again after a pause, and that pass's recovery counts the stage as one
/// aterm stopped in, so a stage that keeps panicking parks with its own pause while the others go
/// on, rather than failing over and over at once.
pub(super) struct Begun<'r> {
    root: &'r Path,
    revision: u64,
    stage: Option<Stage>,
}

impl<'r> Begun<'r> {
    pub(super) const fn new(root: &'r Path, revision: u64, stage: Stage) -> Self {
        Self {
            root,
            revision,
            stage: Some(stage),
        }
    }

    /// The stage ended: record it.
    ///
    /// # Errors
    /// The ledger cannot be saved.
    pub(super) fn ended(mut self) -> Result<(), String> {
        let Some(stage) = self.stage.take() else {
            return Ok(());
        };
        update(self.root, self.revision, |ledger| ledger.end(stage))
    }
}

impl Drop for Begun<'_> {
    fn drop(&mut self) {
        if let Some(stage) = self.stage.take()
            && !std::thread::panicking()
        {
            let _ = update(self.root, self.revision, |ledger| ledger.end(stage));
        }
    }
}

/// The ledger of the run `progress` records, for a reading of the run taken as `live` says: a
/// stage a live process has run past the hang limit is parked by the first reading that sees it,
/// a mark that outlives the process, so the next launch leaves the stage parked until its pause
/// comes due and runs the others. An empty ledger for no progress.
pub(super) fn read_marking_hung(root: &Path, progress: Option<Progress>, live: Live) -> Ledger {
    let Some(progress) = progress else {
        return Ledger::default();
    };
    let mut ledger = load(root, progress.revision).ledger;
    for stage in Stage::ALL {
        let verdict = ledger.verdict(stage, progress.done, progress.running, live);
        if verdict == Verdict::Overdue
            && let Some(at) = ledger.overdue(stage, live)
            && let Ok(true) = update(root, progress.revision, |ledger| {
                ledger.mark_hung(stage, at, live.now)
            })
        {
            ledger.mark_hung(stage, at, live.now);
        }
    }
    ledger
}

/// When each parked, unfinished stage of the run `progress` records comes due, read at `now`
/// ([`Ledger::park_due`]): what the run's worker sets its timer by. None for a ledger a newer
/// aterm wrote, which this build's passes do not begin from.
pub(super) fn park_dues(root: &Path, progress: Progress, now: u64) -> Vec<u64> {
    let ledger = read(root, Some(progress));
    Stage::ALL
        .iter()
        .filter(|stage| progress.done & stage.bit() == 0)
        .filter_map(|stage| ledger.park_due(*stage, now))
        .collect()
}

/// The ledger of the run `progress` records, as it stands: [`read_marking_hung`] for a reader
/// that writes nothing, so a stage past the hang limit reads as running past it. An empty ledger
/// for no progress.
pub(super) fn read(root: &Path, progress: Option<Progress>) -> Ledger {
    progress.map_or_else(Ledger::default, |progress| {
        load(root, progress.revision).ledger
    })
}

/// A person pressed Check again: lift every parked stage of the current run, so its next pass
/// runs them, and have that pass start the run's other automatic tries afresh
/// ([`Ledger::take_afresh`]). Nothing for a run with no progress or a ledger a newer aterm wrote.
///
/// # Errors
/// The run's progress cannot be read, or its ledger cannot be saved.
pub(super) fn retry_parked_at(root: &Path) -> Result<bool, String> {
    let Some(progress) = progress(root)? else {
        return Ok(false);
    };
    update(root, progress.revision, Ledger::retry)
}

/// The profile roots a quit, or an update's Commit about to be written, cuts off in this
/// process ([`interrupt_at`]), until a refused handoff takes its note back ([`refused_at`]). No
/// stage of theirs begins meanwhile ([`Ledger::begin`]): the setup worker runs on through the
/// rest of a quit's shutdown, and a stage begun after the note would be cut off unnoted, which
/// the next launch counts as a crash, so two quick quits would park it. Process memory, never
/// the ledger: the next launch is another process, which nothing latches.
static QUITTING: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Whether a quit, or an update's Commit about to be written, cuts off `root`'s setup in this
/// process now ([`QUITTING`]). A begin reads it under the ledger's lock.
pub(super) fn quitting(root: &Path) -> bool {
    QUITTING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|quitting| quitting == root)
}

/// Latch or release [`QUITTING`] for `root`: [`interrupt_at`] and [`refused_at`] do, and the
/// ledger's conformance walk, which binds the model's `noting` to this latch.
pub(super) fn set_quitting(root: &Path, on: bool) {
    let mut roots = QUITTING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    roots.retain(|quitting| quitting != root);
    if on {
        roots.push(root.to_owned());
    }
}

/// A test's next launch of `root`'s setup, which a real one makes in another process: nothing
/// latches it ([`QUITTING`]).
#[cfg(test)]
pub(super) fn relaunched(root: &Path) {
    set_quitting(root, false);
}

/// aterm is quitting, or an update's handoff is about to write its Commit: the stages this
/// process runs now resume at the next launch without counting as a crash, and no stage begins
/// from now in this process ([`quitting`]). The latch goes first, then the note under the
/// ledger's lock: a begin, which reads the latch under that lock, either ran before the note,
/// which then names its stage, or sees the latch and does not begin.
///
/// # Errors
/// The run's progress cannot be read, or its ledger cannot be saved.
pub(super) fn interrupt_at(root: &Path) -> Result<bool, String> {
    set_quitting(root, true);
    let Some(progress) = progress(root)? else {
        return Ok(false);
    };
    update(root, progress.revision, |ledger| {
        ledger.interrupt(std::process::id())
    })
}

/// The process whose profile.lock hold wrote the mark `run` ([`Stamp::run`]): each hold's mark
/// carries its process id in its top 32 bits.
pub(super) fn hold_process(run: u128) -> u32 {
    u32::try_from(run >> 96).unwrap_or(u32::MAX)
}

/// The handoff that noted the stages it would cut off ([`interrupt_at`]) did not go: those
/// notes are taken back, so aterm stopping in one of them later counts, and stages begin again.
/// The latch is released however the take-back went: aterm goes on.
///
/// # Errors
/// The run's progress cannot be read, or its ledger cannot be saved.
pub(super) fn refused_at(root: &Path) -> Result<bool, String> {
    let taken = progress(root).and_then(|progress| match progress {
        Some(progress) => update(root, progress.revision, |ledger| {
            let taken = ledger.refused(std::process::id());
            set_quitting(root, false);
            taken
        }),
        None => Ok(false),
    });
    set_quitting(root, false);
    taken
}

/// The words a parked stage's row says: why, then that aterm tries it again by itself.
pub(super) fn parked_words(stage: Stage, park: Park) -> String {
    match park {
        Park::Crashed => format!("Stopped twice while {}; {PARKED_TAIL}", gerund(stage)),
        Park::Hung => format!("Took over an hour {}; {PARKED_TAIL}", gerund(stage)),
    }
}

/// How a parked stage's words end, which System State reads them by: the park comes due by
/// itself ([`PARK_PAUSE`]), or at once on Check again.
const PARKED_TAIL: &str = aterm_types::macos_setup::StageRow::PARKED_TAIL;

/// The words the row of a stage still running past the hang limit says ([`Verdict::Overdue`]):
/// it ends by itself or when aterm quits, and no Check again.
pub(super) fn overdue_words(stage: Stage) -> String {
    format!(
        "Still {} after more than an hour; {STILL_RUNNING_TAIL}",
        gerund(stage)
    )
}

/// How the words of a stage still running past the hang limit end, which System State reads
/// them by.
const STILL_RUNNING_TAIL: &str = aterm_types::macos_setup::StageRow::STILL_RUNNING_TAIL;

/// The words a resumed stage's row says.
pub(super) const RESUMING: &str = aterm_types::macos_setup::StageRow::RESUMING;

/// What `stage` does, as a row says it stopped, or took too long, doing it.
const fn gerund(stage: Stage) -> &'static str {
    match stage {
        Stage::Workstation => "setting your Mac settings",
        Stage::Spotlight => "setting up Spotlight and Rust documentation",
        Stage::Permissions => "setting aterm's permissions",
        Stage::Secrets => "joining the secrets vault",
        Stage::Repositories => "cloning GitHub repositories",
        Stage::Accounts => "signing out of Messages and FaceTime",
    }
}

/// The `SetupLedger` and `SetupKeptKey` actions the setup run does not perform, named where the
/// whole-source closure (aterm-models' `spec_xref_closure`) collects them: the environment's
/// crash, quit and reboot, and the models' `Buggy=1` mutants, which the conformance test's
/// negative control replays. A waiver in the test module would compile into that test alone,
/// never into the closure.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "Crash",
        reason = "The environment: the process ends mid-stage (a crash, a kill, a power loss), \
                  leaving what it recorded; profile.lock's flock goes with it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "Reboot",
        reason = "The environment: the Mac restarts or logs out and the process ends with it, \
                  leaving what it recorded; the next launch's `recover` reads the new boot \
                  against the begin's `kern.bootsessionuuid`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "CommitQuit",
        reason = "The environment: the quit or the handoff's Commit goes and the process ends, \
                  leaving the notes `interrupt` made; there is no code step after it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "BeginWhileQuitting",
        reason = "The Buggy=1 mutant: a begin while a quit decides, which the quit cuts off \
                  unnoted; planted in `begin` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "RefuseKeepsNote",
        reason = "The Buggy=1 mutant: a refused handoff whose notes stay; planted in `refused` \
                  by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "RerunDone",
        reason = "The Buggy=1 mutant: a begin that does not ask the progress, running a done \
                  stage again; planted in `begin` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "RelaunchForgetsDone",
        reason = "The Buggy=1 mutant: a relaunch that loses a done stage, which then runs \
                  again: the progress record losing it, which the negative control's rig does, \
                  since the ledger holds no done stage."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "RelaunchNeverParks",
        reason = "The Buggy=1 mutant: a relaunch that resumes a stage aterm stopped in twice; \
                  planted in `recover` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "RelaunchParksForGood",
        reason = "The Buggy=1 mutant: a relaunch that parks a stage with no pause to come due, \
                  the hold only a person's Check again lifted; planted in `recover` by the \
                  negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "RelaunchCountsRestart",
        reason = "The Buggy=1 mutant: a relaunch after the Mac restarted that counts the stage \
                  the restart cut off as aterm stopping in it, so a restart parks it as a second \
                  stop would; planted in `recover` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupKeptKey",
        action = "ApplyKept",
        reason = "The Buggy=1 mutant: an unattended pass that writes a kept key; planted in \
                  `Kept::hold` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupKeptKey",
        action = "KeepAnyDrift",
        reason = "The Buggy=1 mutant: an unattended pass that keeps the first change after a \
                  read-back, with no try again; planted in `Kept::hold` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupKeptKey",
        action = "OtherWriterChanges",
        reason = "The environment: something other than aterm (the person, an app, an \
                  installer, macOS) changes a key aterm set; nothing records who, and no code \
                  step runs until the next pass reads it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "DisarmWhileOwed",
        reason = "The Buggy=1 mutant: a save that reads an unfinished run as owing nothing and \
                  removes its login job; planted in `resume::owes` by the negative control."
    )
)]
#[doc(hidden)]
pub fn setup_ledger_environment() {}
