// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK A HEAVY RUN WILL WRITE, CLAIMED BEFORE IT STARTS (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! DISK-ADMIT). verify, `ship trust run` and the lab's VM boots each claim, in the capacity
//! ledger (`aterm_capacity`), what their last runs of that kind wrote ([`aterm_capacity::Footprints`]),
//! or, where none was measured here, the kind's cold budget less what its directories hold. A
//! volume short of it is relieved first: the build cache's pass under pressure
//! ([`crate::build_cache::ReclaimAfter::by_floor`]) and, inside aterm, the window's disk
//! watch asked for the room (`await sys free>=`). Then the run waits, saying once a minute what
//! it needs, what is free and what holds the most, instead of failing at `ENOSPC` hours in,
//! having said first why it needs that ([`aterm_capacity::Estimate`]). A
//! run needing more than the volume has free above its floor waits for space freed alone, since
//! what other runs wrote stays once they end: it reserves nothing against the runs behind it,
//! which go first when they fit, the watch is asked for no room on its behalf by them, and its
//! line says `--admit now` starts it at once ([`aterm_capacity::Short::unservable`]). Once space
//! freed brings its need within the room while the runs admitted around it hold the rest, it is
//! relieved again, sized for them, where one of them reserves that disk that its last relief was
//! not sized for ([`aterm_capacity::Relief::relieve`]). The cache's pass keeps the run's own
//! directories, which the run is about to write into ([`recorder`]), trading the room a bloated
//! one would free for a warm build. A relief whose pass removed anything, or that asked the
//! window's watch (whose own passes know no run's directories), is followed by a new measure of
//! them, and so is the look that admits a claim that waited (another run's pass or the watch's
//! own may have emptied them meanwhile): the claim is sized again from it, re-entering the wait
//! when it grew, and the run's writes are measured from it ([`Sizing`]).
//! SIGINT, SIGTERM or SIGHUP ends the claim, its measuring and its relief within a second
//! ([`aterm_toolchain::interrupt`]), with [`Error::Stopped`] and nothing left in the ledger.
//! Under `--admit now`, which relieves nothing and measures before any wait, only the claim's
//! wait for the ledger's lock ends so, and a signal during its measuring reaches the process's
//! own handling.
//! While it runs, its own measure of its writes is its claim's progress
//! ([`aterm_capacity::Meter`]), and what it took is kept for the next run, tagged with the
//! directories it measured ([`dirs_tag`]) and what it built there ([`scope_tag`]): a run tagged
//! with other directories (they were re-pointed since) is not counted, nor one of another scope
//! (a `-p` run's growth is not what a whole-tree run writes, nor the reverse), and one kept
//! before runs were tagged (the build cache's move among them) counts wherever it was measured
//! until it leaves the record, but only for a kind with one scope.
//! A kind whose directories are each checkout's own (verify's) keeps each run that did all its
//! work in the machine's record as well ([`Machinewide`], [`aterm_capacity::MachineFootprints`]),
//! by what it built, and a run with no run of its scope measured on its own directories (a fresh
//! worktree's) claims what runs in this machine's checkouts bound it to, where that is less than
//! the cold budget less what they hold ([`aterm_capacity::Estimate::or_machine`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use aterm_capacity::{
    Footprints, Ledger, Live, Machine, MachineFootprints, Meter, Need, Pace, Relief, Short,
    Shortfall, admit_relieved, allocated_until,
};

/// What a caller passes and reads back, so verify, the trust lane and the lab claim through
/// this module alone.
pub use aterm_capacity::{Admit, Cover, Error, Estimate, Footprint, Selection, gib, size};

/// One run's disk, as it is claimed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// What the run is, as a wait line names it: `verify lint test -p atpkg`.
    pub what: String,
    /// What it builds, spelled the same way each time it builds the same (its checks and
    /// packages, sorted): its footprints are kept by it ([`scope_tag`]), so a run claims what the
    /// last runs of its own scope wrote. Empty for a kind with one scope (a VM's boot).
    pub scope: String,
    /// The directories it writes: measured for what they hold now, and while it runs.
    pub dirs: Vec<PathBuf>,
    /// Where the footprints of this kind of run are kept.
    pub record: PathBuf,
    /// What a run of this kind writes into empty directories: where none was measured here, it
    /// claims this less what [`Self::dirs`] hold already.
    pub cold: u64,
    /// Where its writes land: the claim is on the volume holding it.
    pub on: PathBuf,
    /// Give up its wait once every claim it waits behind is held by a holder that has made no
    /// progress for this long ([`aterm_capacity::Need::ends_when_stuck`]): [`Error::Stuck`].
    /// `None`: it waits as long as they hold.
    pub stuck_ends: Option<Duration>,
    /// Where a kind whose directories are each checkout's own keeps its runs of every checkout on
    /// this machine, and what this run builds; `None` for a kind whose directories are the
    /// machine's own (a VM's) or that keeps none.
    pub machine: Option<Machinewide>,
}

/// THE MACHINE'S RECORD OF A KIND WHOSE DIRECTORIES ARE EACH CHECKOUT'S OWN (verify's build
/// directory and `<target>/verify`): a fresh worktree's hold no run of anything, so its record
/// beside them ([`Run::record`]) has none to read, and a run of a scope run in another checkout
/// claims what that run bounds ([`aterm_capacity::MachineFootprints::cover_by`]) instead of the
/// cold budget. Kept only for a run that did all its work ([`Admitted::finish_whole`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machinewide {
    /// The record's file, the same for every checkout on this machine.
    pub record: PathBuf,
    /// What the run builds, as the record keeps it.
    pub selection: Selection,
    /// The words of the kind's work each of which adds work to the same directories and takes
    /// none away (verify's checks that build), so a run kept that did the run's work and more of
    /// them bounds it too.
    pub more: Vec<String>,
}

/// WHAT A FINISHED RUN TOOK, AND WHICH RECORDS TOOK IT ([`Admitted::finish_here`],
/// [`Admitted::finish_whole`]), so what its log says it kept is what it kept.
#[derive(Debug)]
pub struct Finished {
    /// What it took.
    pub run: Footprint,
    /// Whether the record beside its directories took it ([`Run::record`]).
    pub here: std::io::Result<()>,
    /// Whether the machine's record took it ([`Run::machine`]): `None` where it was not offered
    /// (a run that did not do all its work, or of a kind, or in a place, that keeps none).
    pub machine: Option<std::io::Result<()>>,
}

/// An admitted run: its claim held, and its writes measured, until [`Self::finish`].
#[derive(Debug)]
pub struct Admitted {
    meter: Meter,
    record: PathBuf,
    machine: Option<Machinewide>,
    /// What it claimed, and why.
    pub estimate: Estimate,
    /// How long it waited for room.
    pub waited: Duration,
}

impl Admitted {
    /// The run is over: let its claim go, and keep what it took for the next run of its kind on
    /// its directories ([`Run::record`]), but not for this machine's other checkouts
    /// ([`Run::machine`]): a run that failed may have stopped short of what its scope writes, and
    /// what other checkouts claim would then be too little (on its own directories, a heavier run
    /// it kept bounds it, as it always did).
    pub fn finish(self) -> Footprint {
        self.finish_here().run
    }

    /// [`Self::finish`], saying whether the record beside its directories took the run.
    pub fn finish_here(self) -> Finished {
        let run = self.meter.finish();
        let mut kept = Footprints::load(&self.record);
        kept.push(run);
        Finished {
            run,
            here: kept.save(&self.record),
            machine: None,
        }
    }

    /// The run did all its work (verify: every check passed), so it built every unit of its
    /// scope: let its claim go and keep what it took on its directories ([`Self::finish_here`])
    /// and, where its kind keeps them ([`Run::machine`]), in the machine's record for every
    /// checkout, under its lock. A record that cannot be written leaves the one before, and the
    /// run's next kin claim by it; [`Finished`] says which took it.
    pub fn finish_whole(mut self) -> Finished {
        let machine = self.machine.take();
        let mut done = self.finish_here();
        done.machine = machine.map(|m| MachineFootprints::keep(&m.record, m.selection, done.run));
        done
    }

    /// The run was cut short (an interrupt ended it): let its claim go and return what it took,
    /// keeping nothing. A run that stopped short of its work wrote less than a run of its scope
    /// writes; kept, that would be what the next one claims, which would then write the rest
    /// unclaimed.
    pub fn abandon(self) -> Footprint {
        self.meter.finish()
    }
}

/// Claim `run`'s disk in this user's ledger on the live machine, relieving a short volume
/// ([`Reclaim::live`]) before it waits and saying through `say` what it does and waits for.
///
/// # Errors
/// [`Error::Never`]: the need is more than the volume holds above its floor; [`Error::Ledger`]:
/// no home, or the ledger could not be written (the caller runs without a claim, and says so);
/// [`Error::Stopped`]: an interrupt ended the wait (the caller ends as NOT RUN);
/// [`Error::Stuck`]: the wait gave up behind holders that made no progress
/// ([`Run::stuck_ends`]).
pub fn admit(run: Run, mode: Admit, say: &mut dyn FnMut(&str)) -> Result<Admitted, Error> {
    let ledger = Ledger::user().ok_or_else(|| {
        Error::Ledger(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no home directory to keep the ledger under",
        ))
    })?;
    let mut relief = Reclaim::live(&run.dirs);
    admit_on(run, &ledger, &Live, mode, Pace::default(), &mut relief, say)
}

/// [`admit`] in `ledger` on `machine`, relieved by `relief`. Under `--admit wait` the measuring
/// of the run's directories is part of the wait: an interrupt then ends it as one during the
/// wait does, before its first look, whether or not the run would claim anything. A claim that
/// says anything (a relief, a wait, what `--admit now` started it short of) or is refused first
/// says why its figure is what it is (`disk: claims` and the [`Estimate`]: the runs it was read
/// from, the cold budget, or the budget capping a peak), since a wait line names the figure
/// alone; one admitted at once says nothing. After a relief that may have removed what the
/// directories held, and at the look that admits a claim that waited ([`Sizing`]), they are
/// measured again: a claim that grew says so and re-enters the wait with it, one refused then is
/// refused for that figure, and the run's writes are measured from what they held at the last
/// measure.
///
/// # Errors
/// As [`admit`].
pub fn admit_on(
    run: Run,
    ledger: &Ledger,
    machine: &dyn Machine,
    mode: Admit,
    pace: Pace,
    relief: &mut Reclaim,
    say: &mut dyn FnMut(&str),
) -> Result<Admitted, Error> {
    let watch = (mode == Admit::Wait).then(aterm_toolchain::interrupt::Watch::enter);
    let holds = allocated_until(&run.dirs, &|| {
        watch.as_ref().is_some_and(|w| w.caught().is_some())
    });
    if let Some(signal) = watch
        .as_ref()
        .and_then(aterm_toolchain::interrupt::Watch::take)
    {
        return Err(Error::Stopped {
            signal: Some(signal),
            short: None,
        });
    }
    let tag = dirs_tag(&run.dirs);
    let scope = scope_tag(&run.scope);
    let kept = Footprints::load(&run.record);
    let across = run
        .machine
        .as_ref()
        .map(|m| (MachineFootprints::load(&m.record), m.clone()));
    let estimate = need_of(&kept, across.as_ref(), holds, run.cold, tag, scope);
    let mut sizing = Sizing {
        relief,
        dirs: &run.dirs,
        tag,
        scope,
        kept,
        across,
        cold: run.cold,
        holds,
        estimate,
        relieved: false,
    };
    let claim = if estimate.bytes == 0 {
        None
    } else {
        let mut need = Need::new(run.what.clone()).disk(estimate.bytes, run.on.clone());
        if let Some(after) = run.stuck_ends {
            need = need.ends_when_stuck(after);
        }
        let why = format!("disk: claims {estimate}");
        let mut told = false;
        let admitted = admit_relieved(
            ledger,
            machine,
            need,
            mode,
            pace,
            &mut sizing,
            &mut |line: &str| {
                if !std::mem::replace(&mut told, true) {
                    say(&why);
                }
                say(line);
            },
        );
        if matches!(admitted, Err(Error::Never(_))) && !told {
            say(&why);
        }
        Some(admitted?)
    };
    let Sizing {
        holds, estimate, ..
    } = sizing;
    let waited = claim
        .as_ref()
        .map_or(Duration::ZERO, aterm_capacity::Claim::waited);
    Ok(Admitted {
        meter: Meter::start(run.dirs, tag, scope, holds, claim),
        record: run.record,
        machine: run.machine,
        estimate,
        waited,
    })
}

/// A CLAIM OF A KNOWN SIZE, MEASURED BY NOBODY: one claim for a whole tree of work whose
/// caller sized it from its own record of what that work wrote (the release's,
/// `aterm_release::release_disk`), so nothing is walked to size it before it, while it is held,
/// or at its end, and nothing is kept when it ends. The work it starts under it is covered by it
/// ([`Whole::marker`], [`aterm_capacity::DISK_COVER_ENV`]) and claims nothing more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SizedClaim {
    /// What the work is, as a wait line names it: `release 0.156.0`.
    pub what: String,
    /// What it claims, bytes.
    pub bytes: u64,
    /// Where its writes land: the claim is on the volume holding it.
    pub on: PathBuf,
    /// The directories its relief keeps ([`Reclaim::live`]): those the work is about to write into
    /// (its build directories), which a pass under pressure would otherwise turn cold.
    pub keep: Vec<PathBuf>,
    /// The checkout it runs for, which its record and its wait lines name
    /// ([`aterm_capacity::Need::checkout`]).
    pub checkout: Option<String>,
    /// Give up its wait once every claim it waits behind is held by a holder that has made no
    /// progress for this long ([`aterm_capacity::Need::ends_when_stuck`]): [`Error::Stuck`].
    pub stuck_ends: Option<Duration>,
}

/// A HELD CLAIM OF A KNOWN SIZE ([`SizedClaim`]): it ends when dropped, keeping nothing.
#[derive(Debug)]
pub struct Whole {
    claim: aterm_capacity::Claim,
    /// How long it waited for room.
    pub waited: Duration,
}

impl Whole {
    /// The claim's name ([`aterm_capacity::Record::cover_marker`]): what its holder hands the work
    /// it starts as [`aterm_capacity::DISK_COVER_ENV`], so that work is covered by it alone
    /// ([`aterm_capacity::covering_disk`]).
    #[must_use]
    pub fn marker(&self) -> String {
        self.claim.record().cover_marker()
    }

    /// The claim as the ledger holds it.
    #[must_use]
    pub const fn record(&self) -> &aterm_capacity::Record {
        self.claim.record()
    }

    /// The work has written `bytes` so far (its own count, never a walk): a waiter counts only the
    /// rest of the claim as reserved, since free space has lost those bytes already
    /// ([`aterm_capacity::Claim::wrote`]).
    ///
    /// # Errors
    /// The claim's file could not be written; the claim stands as it was.
    pub fn wrote(&mut self, bytes: u64) -> std::io::Result<()> {
        self.claim.wrote(bytes)
    }
}

/// Claim `run`'s disk in this user's ledger on the live machine, relieving a short volume
/// ([`Reclaim::live`], keeping [`SizedClaim::keep`]) before it waits and saying through `say` what it
/// does and waits for. Nothing is walked to size it; only a wait short of room sizes the
/// largest holders its lines name, as every wait does ([`Reclaim::live`]).
///
/// # Errors
/// As [`admit`]: [`Error::Never`], [`Error::Ledger`], [`Error::Stopped`], [`Error::Stuck`].
pub fn admit_sized(
    run: SizedClaim,
    mode: Admit,
    say: &mut dyn FnMut(&str),
) -> Result<Whole, Error> {
    let ledger = Ledger::user().ok_or_else(|| {
        Error::Ledger(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no home directory to keep the ledger under",
        ))
    })?;
    let mut relief = Reclaim::live(&run.keep);
    admit_sized_on(run, &ledger, &Live, mode, Pace::default(), &mut relief, say)
}

/// [`admit_sized`] in `ledger` on `machine`, relieved by `relief`. A claim that says anything (a
/// relief, a wait) or is refused first says what it claims (`disk: claims`), since a wait line
/// names the figure alone; one admitted at once says nothing.
///
/// # Errors
/// As [`admit_sized`].
pub fn admit_sized_on(
    run: SizedClaim,
    ledger: &Ledger,
    machine: &dyn Machine,
    mode: Admit,
    pace: Pace,
    relief: &mut dyn Relief,
    say: &mut dyn FnMut(&str),
) -> Result<Whole, Error> {
    let mut need = Need::new(run.what.clone()).disk(run.bytes, run.on.clone());
    if let Some(checkout) = run.checkout {
        need = need.checkout(checkout);
    }
    if let Some(after) = run.stuck_ends {
        need = need.ends_when_stuck(after);
    }
    let why = format!(
        "disk: claims {} for {} on the volume of {}, sized from its record of its runs, not by \
         walking its directories",
        size(run.bytes),
        run.what,
        run.on.display()
    );
    let mut told = false;
    let admitted = admit_relieved(ledger, machine, need, mode, pace, relief, &mut |line| {
        if !std::mem::replace(&mut told, true) {
            say(&why);
        }
        say(line);
    });
    if matches!(admitted, Err(Error::Never(_))) && !told {
        say(&why);
    }
    let claim = admitted?;
    Ok(Whole {
        waited: claim.waited(),
        claim,
    })
}

/// THE IDENTITY OF A RUN'S DIRECTORIES, kept with each run measured on them
/// ([`Footprint::dirs`]), so a run measured on other directories is not counted
/// ([`aterm_capacity::Footprints::need`]): an FNV-1a digest ([`aterm_hash::Fnv1a64`]) of their
/// canonical paths ([`canonical`]), sorted and each once, each ended by a NUL, which no path
/// holds. The digest is stable, so a record one build writes is read the same by the next (never
/// std's `DefaultHasher`, whose output may change between releases); the order the directories
/// are given in and the spelling they are given by (a link, `/tmp` for `/private/tmp`) do not
/// change it.
#[must_use]
pub fn dirs_tag(dirs: &[PathBuf]) -> u64 {
    let mut paths: Vec<PathBuf> = dirs.iter().map(|d| canonical(d)).collect();
    paths.sort();
    paths.dedup();
    let mut h = aterm_hash::Fnv1a64::new();
    for p in &paths {
        h.write(p.as_os_str().as_encoded_bytes());
        h.write_u8(0);
    }
    h.finish()
}

/// THE IDENTITY OF WHAT A RUN BUILDS, kept with each run measured ([`Footprint::scope`]), so a
/// run of another scope is not counted ([`aterm_capacity::Footprints::need`]): an FNV-1a digest
/// ([`aterm_hash::Fnv1a64`]) of `scope`, which the caller spells the same each time it builds the
/// same; stable across builds, as [`dirs_tag`] is. `None` for an empty one: a kind with one
/// scope, whose runs kept before runs were scoped still count.
#[must_use]
pub fn scope_tag(scope: &str) -> Option<u64> {
    (!scope.is_empty()).then(|| {
        let mut h = aterm_hash::Fnv1a64::new();
        h.write(scope.as_bytes());
        h.finish()
    })
}

/// `path` with every link resolved as far as it exists, and the rest appended as written: the
/// one spelling of a run's directories, which verify's run records name their roots by too
/// ([`aterm_uds::runs::canonical`]).
pub use aterm_uds::runs::canonical;

/// A RUN'S CLAIM, SIZED AGAIN AFTER ITS RELIEF AND AS IT IS ADMITTED: the relief ([`Reclaim`])
/// can remove what the run's directories held, whatever its own pass keeps (the window's disk
/// watch, asked for the room, runs passes of its own that know no run's directories and empty
/// idle build directories, a live checkout's among them), and so can any pass that runs while
/// the claim waits (another waiting run's relief keeps that run's directories alone, and the
/// watch runs its own each minute); the run then writes it all again from empty. So after a
/// relief whose pass removed anything, or that asked the watch, they are measured again, and at
/// the look that admits a claim that waited, whatever its last relief did ([`Relief::resized`]):
/// the claim is sized from that measure ([`Footprints::need`]) and, grown, says why and re-enters
/// the wait with it; the run's writes are measured from the last measure ([`Meter::start`]), so
/// its record says what it wrote from there, not a growth of 0 from a measure taken before its
/// directories went.
struct Sizing<'a> {
    relief: &'a mut Reclaim,
    /// The run's directories.
    dirs: &'a [PathBuf],
    /// Their tag ([`dirs_tag`]): the runs kept that were measured on them are counted.
    tag: u64,
    /// What the run builds ([`scope_tag`]): the runs kept of that scope are counted.
    scope: Option<u64>,
    /// What its kind's last runs took on its directories.
    kept: Footprints,
    /// What its kind's runs took in this machine's checkouts, and what it builds, where its kind
    /// keeps them ([`Run::machine`]).
    across: Option<(MachineFootprints, Machinewide)>,
    /// What its kind writes into empty directories.
    cold: u64,
    /// What its directories held at the last measure.
    holds: u64,
    /// What it claims, and why: the figure its claim holds.
    estimate: Estimate,
    /// Whether the call to come follows a relief ([`Relief::relieve`]): that one measures only
    /// where the relief may have removed something (`Reclaim`'s `touched`); any other (the look
    /// that admits a claim that waited) measures.
    relieved: bool,
}

/// What a run claims when its directories, tagged `tag`, hold `holds` and it builds `scope`: what
/// its kind's runs on them (`kept`) bound ([`Footprints::need`]), or, with none of its scope, what
/// the machine's runs (`across`: its record, and the run's selection and the words that add
/// work) bound where lower ([`Estimate::or_machine`]).
fn need_of(
    kept: &Footprints,
    across: Option<&(MachineFootprints, Machinewide)>,
    holds: u64,
    cold: u64,
    tag: u64,
    scope: Option<u64>,
) -> Estimate {
    kept.need(holds, cold, tag, scope)
        .or_machine(across.and_then(|(m, w)| m.cover_by(&w.selection, holds, cold, &w.more)))
}

impl Relief for Sizing<'_> {
    fn relieve(&mut self, short: &Shortfall, say: &mut dyn FnMut(&str), stopped: Stopped<'_>) {
        self.relief.relieve(short, say, stopped);
        self.relieved = true;
    }

    fn holders(&mut self, short: &Shortfall, stopped: Stopped<'_>) -> Option<String> {
        self.relief.holders(short, stopped)
    }

    fn renewed(&mut self, stopped: Stopped<'_>) -> bool {
        self.relief.renewed(stopped)
    }

    fn resized(&mut self, say: &mut dyn FnMut(&str), stopped: Stopped<'_>) -> Option<u64> {
        let touched = std::mem::take(&mut self.relief.touched);
        if std::mem::take(&mut self.relieved) && !touched {
            return None;
        }
        let holds = allocated_until(self.dirs, stopped);
        if stopped() {
            return None;
        }
        self.holds = holds;
        let estimate = need_of(
            &self.kept,
            self.across.as_ref(),
            holds,
            self.cold,
            self.tag,
            self.scope,
        );
        // A smaller figure (the directories grew meanwhile) leaves the claim, and why it is what
        // it is, as they were: a waiting claim is never made smaller.
        if estimate.bytes < self.estimate.bytes {
            return None;
        }
        let was = std::mem::replace(&mut self.estimate, estimate);
        (estimate.bytes > was.bytes).then(|| {
            say(&format!(
                "disk: its directories hold {} now, not the {} its claim was sized on, so it \
                 claims {estimate}",
                size(holds),
                size(was.holds)
            ));
            estimate.bytes
        })
    }
}

/// What a pass retained: counted entries for an ordinary failure, an uncounted remainder
/// after revocation, or an entry nothing of which went once its intent was on record
/// ([`crate::build_cache::Unused::Refused`]). These notes accompany the relief line's freed bytes.
fn stayed_notes(went: &[(PathBuf, crate::build_cache::Unused)]) -> Vec<String> {
    went.iter()
        .filter_map(|(stage, why)| match *why {
            crate::build_cache::Unused::Stayed { stayed, .. } => {
                Some(crate::build_cache::remove::stayed_words(stage, stayed))
            }
            crate::build_cache::Unused::Revoked { freed } => Some(format!(
                "{}: retirement permission revoked; {} went; retained remainder not counted",
                stage.display(),
                crate::cost::human_bytes(freed)
            )),
            crate::build_cache::Unused::Refused { by } => Some(format!(
                "{}: kept, nothing of it went ({}): {}",
                stage.display(),
                by.word(),
                by.reason()
            )),
            _ => None,
        })
        .collect()
}

/// The paths a pass's record let go (`asked`) that it removed, or began to: each but those it
/// returned as nothing of them gone ([`crate::build_cache::Unused::nothing_went`]). Each is
/// paired with its outcome by their order, not by path: the pass returns one outcome for each
/// intent it put on record, in their order, and one kept in its stage is returned as the
/// stage's path ([`crate::build_cache::Refusal::Staged`]), never the path it was asked for.
fn went_of(asked: Vec<PathBuf>, went: &[(PathBuf, crate::build_cache::Unused)]) -> Vec<PathBuf> {
    asked
        .into_iter()
        .enumerate()
        .filter(|(n, _)| went.get(*n).is_none_or(|(_, why)| !why.nothing_went()))
        .map(|(_, path)| path)
        .collect()
}

/// A volume's free bytes that the build cache's pass under pressure gave back, the
/// directories it removed, and what stayed of each it could not finish, in
/// [`crate::build_cache::remove::stayed_words`]'s words.
pub type Passed = (u64, Vec<PathBuf>, Vec<String>);

/// Whether an interrupt ended the wait a relief works for ([`Relief`]): each part gives up what
/// is left of its work once it says so.
pub type Stopped<'a> = &'a dyn Fn() -> bool;

/// What the build cache's pass under pressure reads of a short volume.
#[derive(Clone, Copy, Debug)]
pub struct Room<'a> {
    /// The short volume's mount.
    pub mount: &'a Path,
    /// How long what it takes must stand unwritten.
    pub after: crate::build_cache::ReclaimAfter,
}

/// The build cache's pass under pressure, given the short volume.
pub type Pass = Box<dyn FnMut(Room<'_>, Stopped<'_>) -> Passed>;

/// The window's disk watch asked for a number of bytes free on the volume mounted at a path:
/// its answer, or `None` outside aterm.
pub type Ask = Box<dyn FnMut(u64, &Path, Stopped<'_>) -> Option<String>>;

/// The largest holders of the volume mounted at a path, with their sizes, largest first.
pub type Largest = Box<dyn FnMut(&Path, Stopped<'_>) -> Vec<(PathBuf, u64)>>;

/// WHAT A SHORT VOLUME IS RELIEVED BY before a run waits on it, and who a wait line names as
/// holding the rest.
pub struct Reclaim {
    /// The build cache's pass under pressure, given the short volume.
    pass: Pass,
    /// The window's disk watch asked for `bytes` free on the volume mounted at `mount`: its
    /// answer, or `None` outside aterm.
    ask: Ask,
    /// The largest holders of the volume mounted at the path, largest first.
    largest: Largest,
    /// The holders' words, read once a wait begins.
    named: Option<String>,
    /// Whether a relief since the claim was last sized ([`Sizing`]) may have removed what the
    /// run's directories held: its pass removed something, or the window's disk watch answered
    /// (its own passes, run for the room asked, know no run's directories).
    touched: bool,
    /// The build directories of gone workspaces now ([`crate::build_cache::gone_leaves`]), read
    /// at each waiting look; `None` reads none, and the relief is never renewed.
    gone: Option<Gone>,
    /// What `gone` listed as the last relief's pass began: a directory it did not list then (its
    /// workspace removed since) renews the relief ([`Relief::renewed`]). One the pass kept (a
    /// build holds it) is listed, so it renews nothing.
    gone_after: std::collections::BTreeSet<PathBuf>,
    /// Whether the last relief was asked for disk: only a disk shortfall is renewed, so a wait
    /// short of memory or load alone never relieves again at every look.
    short_of_disk: bool,
}

/// The build directories of gone workspaces now, cheaply ([`crate::build_cache::gone_leaves`]).
pub type Gone = Box<dyn FnMut() -> std::collections::BTreeSet<PathBuf>>;

impl Reclaim {
    /// The relief made of these parts.
    #[must_use]
    pub fn of(pass: Pass, ask: Ask, largest: Largest) -> Reclaim {
        Reclaim {
            pass,
            ask,
            largest,
            named: None,
            touched: false,
            gone: None,
            gone_after: std::collections::BTreeSet::new(),
            short_of_disk: false,
        }
    }

    /// This relief, renewed at a waiting look short of disk where `gone` lists a build directory
    /// it did not list as the last pass began ([`Relief::renewed`]).
    #[must_use]
    pub fn watching(mut self, gone: Gone) -> Reclaim {
        self.gone = Some(gone);
        self
    }

    /// The live relief: [`crate::build_cache::reclaim_home_watched`] where the build cache is on
    /// the short volume ([`cache_on`]), by [`Room::after`], keeping `own` (the run's directories,
    /// [`recorder`]), then inside
    /// aterm `aterm ctl @self await sys free>=<bytes> vol=<mount>` for up to [`ASK_FOR`]; the
    /// largest holders are the build cache's directories on the volume and `own`. Once an
    /// interrupt came, the pass is left behind removing nothing more ([`on_thread`]), the ask
    /// is ended and the sizing stops.
    #[must_use]
    pub fn live(own: &[PathBuf]) -> Reclaim {
        let (keep, own) = (own.to_vec(), own.to_vec());
        Reclaim::of(
            Box::new(move |room: Room<'_>, stopped| {
                if !cache_on(room.mount) {
                    return (0, Vec::new(), Vec::new());
                }
                let before = free_on(room.mount);
                let keep = keep.clone();
                let after = room.after;
                let (removed, stayed) = on_thread(stopped, move |halt| {
                    let mut removed = Vec::new();
                    let went = crate::build_cache::reclaim_home_watched(
                        after,
                        None,
                        &mut recorder(&keep, halt, &mut removed),
                    );
                    (went_of(removed, &went), stayed_notes(&went))
                })
                .unwrap_or_default();
                (free_on(room.mount).saturating_sub(before), removed, stayed)
            }),
            Box::new(ask_watch),
            Box::new(move |mount, stopped| largest_on(mount, &own, stopped)),
        )
        .watching(Box::new(|| {
            aterm_types::dirs::home_dir()
                .filter(|_| cfg!(target_os = "macos"))
                .map(|home| {
                    crate::build_cache::gone_leaves(
                        &crate::build_cache::cache_root(&home),
                        Some(&home),
                    )
                })
                .unwrap_or_default()
        }))
    }
}

/// WHAT A RUN'S OWN RELIEF KEEPS: the record its build cache pass asks before each removal
/// ([`crate::build_cache::reclaim_watched`]). `true` lets the path go, and it is pushed to
/// `removed`; `false` keeps it, and the pass goes on to the next, so nothing kept stops the
/// relief from freeing the rest. Every path is kept once `halt` says an interrupt came, and
/// one that is one of `own` (the run's directories), lies inside one or holds one, as given or
/// with its links resolved: a directory the run is about to write into is not unused, whatever
/// its idle time says (30 days unwritten takes a build directory whatever its workspace's
/// state). Removing it turns a warm build into a cold one, and frees the run no room where the
/// run writes it all again from empty (its claim grows by as much, [`Sizing`]); it does free
/// room where the directory holds more than the run writes from empty (`Footprints::need` caps a
/// rewrite at the kind's cold budget), which the keep trades for the warm build: the pass is not
/// sized to the run's shortfall, so it would take that build whenever it ran, even when the rest
/// it freed was room enough, and where it was not, the run's wait line names that directory
/// among the largest holders. It takes all it may, since a look still short after it asks it again
/// only where its need grew or, for a need that could not fit, where a claim its last call did not
/// count now reserves that disk ([`aterm_capacity::Relief`]). A stale part of a directory the run
/// uses (`Unused::Stale`, staged beside it) goes: below the floor, where stale means an hour
/// unwritten, that can cost the run a cold compile.
fn recorder<'a>(
    own: &'a [PathBuf],
    halt: &'a AtomicBool,
    removed: &'a mut Vec<PathBuf>,
) -> impl FnMut(&Path, crate::build_cache::Unused) -> bool + 'a {
    let spelled = |p: &Path| -> Vec<PathBuf> {
        std::iter::once(p.to_path_buf())
            .chain(p.canonicalize().ok())
            .collect()
    };
    let mine: Vec<PathBuf> = own.iter().flat_map(|d| spelled(d)).collect();
    move |path: &Path, _why: crate::build_cache::Unused| {
        if halt.load(Ordering::SeqCst) {
            return false;
        }
        let used = spelled(path)
            .iter()
            .any(|p| mine.iter().any(|d| p.starts_with(d) || d.starts_with(p)));
        if used {
            return false;
        }
        removed.push(path.to_path_buf());
        true
    }
}

impl Relief for Reclaim {
    fn relieve(&mut self, short: &Shortfall, say: &mut dyn FnMut(&str), stopped: Stopped<'_>) {
        self.short_of_disk = disk_short(short).is_some();
        let Some((need, mount, floor, reserved, above_floor)) = disk_short(short) else {
            return;
        };
        let room = Room {
            mount: &mount,
            after: crate::build_cache::ReclaimAfter::by_floor(above_floor == 0),
        };
        if let Some(gone) = self.gone.as_mut() {
            // Before the pass: one removed while it runs renews the next look.
            self.gone_after = gone();
        }
        let (freed, removed, stayed) = (self.pass)(room, stopped);
        self.touched |= !removed.is_empty();
        if !removed.is_empty() {
            // What held the volume changed: the next wait line reads its holders again.
            self.named = None;
        }
        if stopped() {
            return;
        }
        say(&if removed.is_empty() {
            format!(
                "disk: {} is short; the build cache had nothing unused to reclaim on it",
                mount.display()
            )
        } else {
            let mut line = format!(
                "disk: {} is short; reclaimed {} from {} unused build director{}: {}",
                mount.display(),
                gib(freed),
                removed.len(),
                if removed.len() == 1 { "y" } else { "ies" },
                removed
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for words in &stayed {
                line.push_str("; ");
                line.push_str(words);
            }
            line
        });
        let want = floor.saturating_add(reserved).saturating_add(need);
        if let Some(answer) = (self.ask)(want, &mount, stopped) {
            self.touched = true;
            say(&format!(
                "disk: asked the window's disk watch for {} free on {}: {answer}",
                gib(want),
                mount.display()
            ));
        }
    }

    fn holders(&mut self, short: &Shortfall, stopped: Stopped<'_>) -> Option<String> {
        let (_, mount, ..) = disk_short(short)?;
        let named = self.named.get_or_insert_with(|| {
            let found = (self.largest)(&mount, stopped);
            if found.is_empty() {
                return String::new();
            }
            let each: Vec<String> = found
                .iter()
                .map(|(p, b)| format!("{} {}", p.display(), gib(*b)))
                .collect();
            format!("largest on {}: {}", mount.display(), each.join(", "))
        });
        (!named.is_empty()).then(|| named.clone())
    }

    fn renewed(&mut self, stopped: Stopped<'_>) -> bool {
        let Some(gone) = self.gone.as_mut().filter(|_| self.short_of_disk) else {
            return false;
        };
        !stopped() && gone().iter().any(|leaf| !self.gone_after.contains(leaf))
    }
}

/// The disk a shortfall is short of: `(need, mount, floor, reserved, above_floor)`.
fn disk_short(short: &Shortfall) -> Option<(u64, PathBuf, u64, u64, u64)> {
    short.shorts.iter().find_map(|s| match s {
        Short::Disk {
            need,
            mount,
            floor,
            reserved,
            above_floor,
        } => Some((*need, mount.clone(), *floor, *reserved, *above_floor)),
        _ => None,
    })
}

/// How long the window's disk watch is waited for when asked for room: one pass of it. The run
/// waits on in its own admission after, with no deadline.
pub const ASK_FOR: Duration = Duration::from_secs(60);

/// The volume mounted at `mount`'s free bytes now; 0 where unread.
fn free_on(mount: &Path) -> u64 {
    Live.read(Some(mount)).volume.map_or(0, |v| v.free)
}

/// `work` (the build cache's pass) on a thread of its own, waited for until `stopped` says an
/// interrupt came: `work`'s `halt` then says `true`, so it removes nothing more, and it is left
/// to end the step it is in while this returns `None` at once. That step can be a whole build
/// directory's removal; one the process's end cuts short leaves its stage, which the next pass
/// finishes. `None` too when the thread cannot start or `work` panics: the relief is skipped.
fn on_thread<T: Send + 'static>(
    stopped: Stopped<'_>,
    work: impl FnOnce(&AtomicBool) -> T + Send + 'static,
) -> Option<T> {
    let halt = Arc::new(AtomicBool::new(false));
    let told = Arc::clone(&halt);
    let (done, result) = mpsc::channel();
    std::thread::Builder::new()
        .name("disk-relief".to_owned())
        .spawn(move || {
            let _ = done.send(work(&told));
        })
        .ok()?;
    loop {
        if stopped() {
            halt.store(true, Ordering::SeqCst);
            return None;
        }
        match result.recv_timeout(aterm_toolchain::interrupt::NAP) {
            Ok(got) => return Some(got),
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// Whether this user's build cache, or the root it moved from, is on the volume mounted at
/// `mount`: a pass over it frees nothing on any other volume, so a short scratch image or
/// external disk is not reclaimed for.
fn cache_on(mount: &Path) -> bool {
    aterm_types::dirs::home_dir().is_some_and(|h| {
        [
            crate::build_cache::cache_root(&h),
            crate::build_cache::moved_from(&h),
        ]
        .iter()
        .filter(|c| c.is_dir())
        .any(|c| Live.read(Some(c)).volume.is_some_and(|v| v.mount == mount))
    })
}

/// Inside aterm (`ATERM_SESSION_ID` names this session), ask the window's disk watch for `bytes`
/// free on the volume mounted at `mount` (`aterm ctl @self await sys free>=<bytes>B vol=<mount>`),
/// waiting for it up to [`ASK_FOR`], or until `stopped` says an interrupt came, which ends the
/// ask (this process's own child) at once: its answer, or `None` outside aterm, with no `aterm`
/// on `PATH`, or once stopped.
fn ask_watch(bytes: u64, mount: &Path, stopped: Stopped<'_>) -> Option<String> {
    std::env::var_os("ATERM_SESSION_ID").filter(|v| !v.is_empty())?;
    let path = std::env::var_os("PATH")?;
    let aterm = crate::vendor::first_on_path("aterm", Some(&path))?;
    let mut child = aterm_uds::spawnfd::command(&aterm)
        .args([
            "ctl".to_owned(),
            "@self".to_owned(),
            "await".to_owned(),
            "sys".to_owned(),
            format!("free>={bytes}B"),
            format!("vol={}", pct_encode(&mount.to_string_lossy())),
            format!("timeout={}", ASK_FOR.as_millis()),
        ])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .ok()?;
    // Each pipe is read on a thread of its own while the ask lasts, so no answer, however long,
    // stalls it; an ask ended by an interrupt leaves them, which end with its pipes.
    let reader = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut text = String::new();
            if let Some(p) = pipe {
                let _ = p.take(64 * 1024).read_to_string(&mut text);
            }
            text
        })
    };
    let out = reader(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let err = reader(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let status = loop {
        if stopped() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(aterm_toolchain::interrupt::NAP),
            Err(_) => return None,
        }
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    let said = if out.is_empty() { err } else { out }
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    Some(if said.is_empty() {
        format!("no answer ({status})")
    } else {
        said
    })
}

/// `text` with every byte but `[A-Za-z0-9/._~-]` written `%XX`: a control request's word.
fn pct_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'~' | b'-') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The three largest holders of the volume mounted at `mount` among those a build tool may
/// size: the run's `own` directories and the build cache's directories there. Nothing else on
/// the volume is walked: its other folders are the person's (Documents, Desktop, other apps'
/// data), and a look into those can put a privacy dialog on their screen. A directory inside
/// another named is left out, so no bytes are named twice. The sizing stops once `stopped`
/// says an interrupt came.
fn largest_on(mount: &Path, own: &[PathBuf], stopped: Stopped<'_>) -> Vec<(PathBuf, u64)> {
    let on = |p: &Path| Live.read(Some(p)).volume.is_some_and(|v| v.mount == mount);
    let mut found: Vec<PathBuf> = own.iter().filter(|d| on(d)).cloned().collect();
    let home = aterm_types::dirs::home_dir();
    let caches = home.iter().flat_map(|h| {
        [
            crate::build_cache::cache_root(h),
            crate::build_cache::moved_from(h),
        ]
    });
    for cache in caches.filter(|c| c.is_dir() && on(c)) {
        found.extend(
            crate::build_cache::workspaces(&cache)
                .iter()
                .map(|w| crate::build_cache::build_dir(&cache, w)),
        );
    }
    found.sort();
    found.dedup();
    let outer = found.clone();
    found.retain(|p| !outer.iter().any(|o| o != p && p.starts_with(o)));
    let mut sized: Vec<(PathBuf, u64)> = found
        .into_iter()
        .map(|p| {
            let b = allocated_until(std::slice::from_ref(&p), stopped);
            (p, b)
        })
        .filter(|(_, b)| *b > 0)
        .collect();
    sized.sort_by_key(|a| std::cmp::Reverse(a.1));
    sized.truncate(3);
    sized
}

#[cfg(test)]
#[path = "admit_disk_tests.rs"]
mod tests;
