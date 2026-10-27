// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The setup run's ledger (design §6.5, §6.9): per stage, how often it began since it last
//! ended, and the boot and the time of its latest begin; and the set of stages running.
//!
//! Liveness is the profile flock, never a pid: a stage recorded running while nobody holds
//! profile.lock, or recorded in another boot, was cut off by aterm stopping. The next launch's
//! pass resumes it once with no press; one that had already begun twice is parked, and the
//! other stages go on. A stage that runs past [`HANG_LIMIT`] with no end is parked the same way
//! by the first reading of the run that sees it ([`Ledger::mark_hung`]), so a wedge, not only a
//! crash, ends at a calm row. Check again clears every park ([`Ledger::retry`]). A quit or an
//! update's handoff in the middle of a stage is no crash ([`Ledger::interrupt`]).

use super::*;

/// The ledger's file, beside the progress record rather than in it: the progress record is
/// read and written whole by every setup path and by an older aterm, which drops a field it
/// does not know when it writes the record again. Here an older aterm never writes it, a stale
/// one is never read ([`Ledger::revision`]), and one a newer aterm wrote is left as it is
/// ([`Loaded::frozen`]).
pub(super) const LEDGER: &str = "setup-ledger.json";

/// The ledger's short lock: the pass that holds profile.lock writes it, and so do a reading of
/// the run that marks a hung stage and a Check again or a quit, which do not hold that lock.
const LEDGER_LOCK: &str = "setup-ledger.lock";

/// The format this build writes in the ledger's `format`: a later one is a newer aterm's.
const LEDGER_FORMAT: u64 = 1;

/// How long a stage may run with no end before the run's next reading parks it: twice the
/// longest budget any one step of a stage keeps (a GitHub clone's 30 minutes), so a step that
/// is slow but alive ends first. A trade-off: a Repositories stage that clones for more than an
/// hour reads parked until it ends, which then clears the park by itself.
pub(super) const HANG_LIMIT: u64 = 60 * 60;

const STAGES: usize = Stage::ALL.len();

/// One stage's begins since it last ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Attempt {
    /// Begins since the stage last ended or Check again cleared it.
    pub(super) began: u8,
    /// The boot its latest begin ran in (`kern.bootsessionuuid`); 0 when that could not be read.
    pub(super) boot: u128,
    /// Its latest begin, in seconds since the epoch.
    pub(super) at: u64,
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
}

/// Whether a stage begins ([`Ledger::begin`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Admit {
    Run,
    /// It is done: a finished stage never runs again in its run.
    Done,
    Parked(Park),
}

/// Why a stage waits for Check again.
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
}

/// What a reading of the run knows of the process that holds it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Live {
    /// Whether profile.lock is held, by any process.
    pub(super) active: bool,
    /// This boot, where it could be read.
    pub(super) boot: Option<u128>,
    /// Now, in seconds since the epoch.
    pub(super) now: u64,
}

impl Live {
    /// The reading this process takes of the run now.
    pub(super) fn now(active: bool) -> Self {
        Self {
            active,
            boot: boot_session(),
            now: now_secs(),
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

    /// Why `stage` waits for Check again, if it does.
    pub(super) const fn parked(&self, stage: Stage) -> Option<Park> {
        if self.hung & stage.bit() != 0 {
            Some(Park::Hung)
        } else if self.crashed & stage.bit() != 0 {
            Some(Park::Crashed)
        } else {
            None
        }
    }

    /// Begin `stage`, at `at` in boot `boot`, unless the run's progress has it `done` or it is
    /// parked: each begin counts.
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
    pub(super) fn begin(&mut self, stage: Stage, done: u16, boot: Option<u128>, at: u64) -> Admit {
        if done & stage.bit() != 0 {
            return Admit::Done;
        }
        if let Some(park) = self.parked(stage) {
            return Admit::Parked(park);
        }
        let attempt = &mut self.attempts[Self::index(stage)];
        attempt.began = attempt.began.saturating_add(1);
        attempt.boot = boot.unwrap_or(0);
        attempt.at = at;
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

    /// Park `stage` as hung, if it still runs the begin of `at` and is not parked already.
    /// Whether that changed the ledger.
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
    pub(super) fn mark_hung(&mut self, stage: Stage, at: u64) -> bool {
        let bit = stage.bit();
        if self.running & bit == 0 || self.hung & bit != 0 || self.attempt(stage).at != at {
            return false;
        }
        self.hung |= bit;
        true
    }

    /// A person pressed Check again: every park is lifted and every stage's begins start from
    /// none. Whether that changed the ledger.
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
        }
        *self != before
    }

    /// aterm is quitting, or handing off to an update: the stages running now are cut off by
    /// no crash, so the next launch resumes them without counting this begin. Whether that
    /// changed the ledger.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "Quit",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(super) fn interrupt(&mut self) -> bool {
        let before = self.quit;
        self.quit |= self.running;
        self.quit != before
    }

    /// The first pass after aterm stopped, holding profile.lock, so whatever the ledger or the
    /// progress record (`legacy`, what an older aterm or a cut-off write left) records running
    /// was cut off: a stage the progress has `done` is spent, one a quit cut off gives its
    /// begin back, and any other counts its begin and parks once it began twice. Nothing is
    /// running after. Whether that changed the ledger.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "Relaunch",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(super) fn recover(&mut self, done: u16, legacy: Option<Stage>) -> bool {
        let before = *self;
        let recorded = self.running | legacy.map_or(0, Stage::bit);
        for stage in Stage::ALL {
            let bit = stage.bit();
            if recorded & bit == 0 {
                continue;
            }
            let attempt = &mut self.attempts[Self::index(stage)];
            if done & bit != 0 {
                *attempt = Attempt::default();
                self.crashed &= !bit;
                self.hung &= !bit;
            } else if self.quit & bit != 0 {
                attempt.began = attempt.began.saturating_sub(1);
            } else {
                attempt.began = attempt.began.max(1);
                if attempt.began >= 2 {
                    self.crashed |= bit;
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
        if let Some(park) = self.parked(stage) {
            return Verdict::Parked(park);
        }
        let attempt = self.attempt(stage);
        let recorded = (self.running | legacy.map_or(0, Stage::bit)) & bit != 0;
        if !recorded {
            // Resumed by this launch's pass, which has not reached it yet.
            return if attempt.began > 0 {
                Verdict::Resuming { running: false }
            } else {
                Verdict::Idle
            };
        }
        let same_boot = attempt.boot == 0 || live.boot.is_none_or(|boot| boot == attempt.boot);
        if live.active && same_boot {
            if self.overdue(stage, live).is_some() {
                return Verdict::Parked(Park::Hung);
            }
            return if attempt.began >= 2 {
                Verdict::Resuming { running: true }
            } else {
                Verdict::Running
            };
        }
        if self.quit & bit == 0 && attempt.began.max(1) >= 2 {
            Verdict::Parked(Park::Crashed)
        } else {
            Verdict::Resuming { running: false }
        }
    }

    /// The begin of `stage` that has run past [`HANG_LIMIT`] in the live process, if it has.
    pub(super) fn overdue(&self, stage: Stage, live: Live) -> Option<u64> {
        let attempt = self.attempt(stage);
        (self.running & stage.bit() != 0
            && self.hung & stage.bit() == 0
            && live.now.saturating_sub(attempt.at) >= HANG_LIMIT)
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
            "stages": stages,
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
        };
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
                    .and_then(|boot| u128::from_str_radix(boot, 16).ok())
                    .ok_or(Unread::Damaged)?,
                at: entry["at"].as_u64().ok_or(Unread::Damaged)?,
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
    /// A newer aterm wrote the file: nothing of it is used, and it is never written.
    pub(super) frozen: bool,
}

/// `revision`'s ledger: empty for a ledger of another revision, a damaged one, none, or one a
/// newer aterm wrote (then [`Loaded::frozen`]).
pub(super) fn load(root: &Path, revision: u64) -> Loaded {
    let read = match std::fs::read(root.join(LEDGER)) {
        Ok(bytes) => aterm_json::from_slice::<Value>(&bytes)
            .map_err(|_| Unread::Damaged)
            .and_then(|value| Ledger::parse(&value))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Unread::Damaged),
    };
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
            ledger: Ledger::of(revision),
            frozen: true,
        },
    }
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
    let Loaded { mut ledger, frozen } = load(root, revision);
    let before = ledger;
    let out = change(&mut ledger);
    if !frozen && ledger != before {
        write_json(&root.join(LEDGER), &ledger.record())?;
    }
    Ok(out)
}

/// A stage begun in the ledger, which ends it there when the pass leaves the stage by an error
/// before its end is recorded ([`Self::ended`]): an error is the stage's failure, not aterm
/// stopping. A panic leaves it running, as aterm stopping there would.
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
/// a mark that outlives the process, so the next launch leaves the stage parked and runs the
/// others. An empty ledger for no progress.
pub(super) fn read_marking_hung(root: &Path, progress: Option<Progress>, live: Live) -> Ledger {
    let Some(progress) = progress else {
        return Ledger::default();
    };
    let mut ledger = load(root, progress.revision).ledger;
    for stage in Stage::ALL {
        let verdict = ledger.verdict(stage, progress.done, progress.running, live);
        if verdict == Verdict::Parked(Park::Hung)
            && let Some(at) = ledger.overdue(stage, live)
            && let Ok(true) = update(root, progress.revision, |ledger| {
                ledger.mark_hung(stage, at)
            })
        {
            ledger.mark_hung(stage, at);
        }
    }
    ledger
}

/// A person pressed Check again: lift every parked stage of the current run, so its next pass
/// runs them. Nothing for a run with no progress or a ledger a newer aterm wrote.
///
/// # Errors
/// The run's progress cannot be read, or its ledger cannot be saved.
pub(super) fn retry_parked_at(root: &Path) -> Result<bool, String> {
    let Some(progress) = progress(root)? else {
        return Ok(false);
    };
    update(root, progress.revision, Ledger::retry)
}

/// aterm is quitting or handing off to an update: the stages running now resume at the next
/// launch without counting as a crash.
///
/// # Errors
/// The run's progress cannot be read, or its ledger cannot be saved.
pub(super) fn interrupt_at(root: &Path) -> Result<bool, String> {
    let Some(progress) = progress(root)? else {
        return Ok(false);
    };
    update(root, progress.revision, Ledger::interrupt)
}

/// The words a parked stage's row says.
pub(super) fn parked_words(stage: Stage, park: Park) -> String {
    match park {
        Park::Crashed => format!(
            "Stopped twice while {}; Check again tries again.",
            gerund(stage)
        ),
        Park::Hung => format!(
            "Took over an hour {}; Check again tries again.",
            gerund(stage)
        ),
    }
}

/// The words a resumed stage's row says.
pub(super) const RESUMING: &str = "Resuming after aterm stopped.";

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

/// The `SetupLedger` actions the setup run does not perform, named where the whole-source
/// closure (aterm-models' `spec_xref_closure`) collects them: the environment's crash and the
/// model's `Buggy=1` mutants, which the conformance test's negative control replays. A
/// waiver in the test module would compile into that test alone, never into the closure.
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
        action = "RerunDone",
        reason = "The Buggy=1 mutant: a begin that does not ask the progress, running a done \
                  stage again; replayed by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "RelaunchForgetsDone",
        reason = "The Buggy=1 mutant: a relaunch that loses a done stage, which then runs \
                  again; replayed by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "RelaunchNeverParks",
        reason = "The Buggy=1 mutant: a relaunch that resumes a stage aterm stopped in twice; \
                  replayed by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SetupLedger",
        action = "ApplyKept",
        reason = "The Buggy=1 mutant: an unattended pass that writes a key the person changed; \
                  replayed by the negative control."
    )
)]
#[doc(hidden)]
pub fn setup_ledger_environment() {}
