// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A BUILD'S PHASES AND THE MEMORY EACH CLAIMS (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! PHASE-CLAIM, CAP3). A `targo test` compiles, and then runs the programs it built, its test
//! binaries one at a time and then its doctests; the run takes a small share of what the compile
//! took. On this Mac's records (2026-11-07) verify's test run of a selection took 11% to 25% of
//! its test compile, over the seven selections both were kept for (`-p aterm-gui`: 17.15 GiB to
//! compile, 1.88 GiB to run its tests, its doctests none), and a claim admitted with its whole
//! run's peak held that peak through the run, so the claims behind it waited for memory nothing
//! used.
//!
//! So a build's need names what its RUN PHASE claims ([`crate::Need::run_mem`]: the most its
//! runs' run phases took, as its holder kept them under [`run_key`], and a fifth more; else a
//! wider selection's, which holds its own but only bounds the step, [`phased`]), and its
//! holder STEPS the claim down to that at the first read of its tree ([`Phases::read`]) that finds
//! a program its build made running in it ([`built`]: a Cargo artifact named `<name>-<16 hex>`, as
//! `aterm_uds::test_binary::is_test_binary` reads one, never a build script). The phase is known
//! from the processes the tree runs, never from the build's words or output: nothing here names a
//! command, and a build that runs nothing it built (`build`, `check`, a lint, `test --no-run`)
//! never steps. A key with no run phase kept, its own nor a wider selection's, asks no step, and
//! its first run keeps one.
//!
//! A PHASE NEVER CLAIMS LESS THAN ITS TREE HOLDS: the claim steps down only where its run holds
//! no more than the step at that read, and steps back up to what it was admitted with where its
//! run comes to hold more than the step later (a heavier run than any kept), once. What its run
//! holds at a read is what its tree holds together then, or the most one process its compile
//! never ran has held in its life, which the kernel keeps, whichever is more ([`Phases::held`]),
//! the rule the meter keeps the whole run's peak by: a test binary that took more between two
//! reads ([`crate::tree_meter::READ_EVERY`]) than either read saw counts, in the figure its holder
//! keeps and in the step. A rise between two reads is still seen only at the next one, so other
//! claims may be admitted into that room meanwhile, as any claim's tree may outgrow its need.
//!
//! WHY ONLY DOWN, AND NO STEP UP THAT WAITS. A build compiles before it runs what it compiled,
//! and its run lasts to its end (the doctests' compile with it, which its run phase's peak
//! counts). A claim admitted with its whole run's peak ([`crate::Need::mem`]: what its holder
//! kept of its whole tree, the most of every phase) holds from its first second the most of every
//! phase ahead of it, so its step down never needs room the claims behind it may have taken, and
//! the run phase's own peak is learned apart from the compile's, under its own key. A claim
//! admitted with its compile's peak alone would have to step up into a heavier run and wait for
//! that room in its turn, and its holder cannot hold the run back while it waits without stopping
//! the build's processes (a stopped test's clocks go on, and its waits would read the stop as a
//! hang), so the run would go on beside claims admitted into the room it lacked: two heavy runs
//! at once on a Mac that holds one. Admitted with the whole peak, they run one at a time. The
//! step back up is the one step up there is, and it takes back room only from claims admitted
//! beside the step, as any held claim's overrun does.
//!
//! The step is one more claim end to every look: what it gives back is known when the claim
//! asks ([`crate::Need::step_gives`]), so a waiter that cannot start reckons the step's room as it
//! reckons a claim's end ([`crate::spared`]), and a claim let go first past it never delays it.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::tree_meter::Member;

/// What a run phase's stage adds to its build's ([`run_key`]): `targo test run`.
pub const RUN: &str = " run";

/// The key a build's RUN PHASE is kept under: `key`'s stage with [`RUN`] after it, the same
/// checkout, selection and variant. A run of more test targets runs each a narrower run does,
/// one at a time, so a wider selection's run phase holds a narrower one's, as
/// [`crate::book::chain`] reads it.
#[must_use]
pub fn run_key(key: &crate::book::Key) -> crate::book::Key {
    crate::book::Key {
        stage: format!("{}{RUN}", key.stage),
        ..key.clone()
    }
}

/// What a need asks its run phase to claim ([`run_need`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunNeed {
    /// Bytes: the most kept and a fifth more, as [`crate::book::need`] reckons it.
    pub bytes: u64,
    /// Whether it is the same work's run phase: its key's own, or its key's in another checkout
    /// ([`crate::book::Via::Own`], [`crate::book::Via::Elsewhere`]). Only that may raise what the
    /// need claims whole ([`phased`]); a wider selection's ([`crate::book::Via::Wider`],
    /// [`crate::book::Via::WiderElsewhere`]) holds its run phase, but not its whole run.
    pub same_work: bool,
}

/// What a need under `key` asks its run phase to claim on a machine of `physical` bytes, as
/// `own` (a plain build's book) keeps it: what [`crate::book::need`] finds kept under
/// [`run_key`], the most and a fifth more ([`crate::peak_claim`]), at most the cold figure where
/// a wider selection's in another checkout is what it found; `None` where none is kept.
#[must_use]
pub fn run_need(
    own: &crate::book::Records,
    key: &crate::book::Key,
    physical: Option<u64>,
) -> Option<RunNeed> {
    let need = crate::book::need(own, &run_key(key), None, physical);
    let found = need.found?;
    Some(RunNeed {
        bytes: need.bytes,
        same_work: matches!(
            found.via,
            crate::book::Via::Own | crate::book::Via::Elsewhere(_)
        ),
    })
}

/// THE NEED A BUILD ASKS WITH ITS PHASES: `whole`, what its whole run claims
/// ([`crate::book::need`]), and `run`, what its run phase claims ([`run_need`]). A run phase of
/// the same work kept heavier than `whole` is its peak ([`crate::Need::run_mem`]), since its own
/// run took that; a lighter one plans its step down. A wider selection's run phase holds its own,
/// so it only bounds the step: never what it claims whole, which its own whole run's record (or
/// the cold figure) bounds, run phase included.
#[must_use]
pub fn phased(whole: u64, run: Option<RunNeed>) -> crate::Need {
    let need = crate::Need::default().mem(whole);
    match run {
        Some(run) if run.same_work || run.bytes < whole => need.run_mem(run.bytes),
        _ => need,
    }
}

/// Whether `image` is a program a build itself made, run: a Cargo artifact named
/// `<name>-<16 hex>` (a test or bench binary), never a build script nor an example
/// (`aterm_uds::test_binary::is_test_binary`'s reading of Cargo's names).
#[must_use]
pub fn built(image: &Path) -> bool {
    aterm_uds::test_binary::is_test_binary(image)
}

/// The executable process `pid` runs, read from the kernel (`aterm_sysprobe::process_path`):
/// the probe a live [`Phases::read`] is handed.
#[must_use]
pub fn image(pid: u32) -> Option<PathBuf> {
    aterm_sysprobe::process_path(pid)
}

/// WHAT A CLAIM THAT STEPS ANSWERS AND DOES ([`crate::Claim`]'s; a test's stand-in).
pub trait Steps {
    /// What it was admitted with ([`crate::Need::mem`]).
    fn admitted(&self) -> u64;
    /// What its run phase claims ([`crate::Need::run_mem`]).
    fn run_mem(&self) -> Option<u64>;
    /// Step it down to its run phase's memory ([`crate::Claim::step_down`]).
    ///
    /// # Errors
    /// Its record could not be written, or it asks no step: it claims what it did.
    fn step_down(&mut self) -> io::Result<()>;
    /// Step it back up to what it was admitted with ([`crate::Claim::step_back`]).
    ///
    /// # Errors
    /// Its record could not be written: it claims what it did.
    fn step_back(&mut self) -> io::Result<()>;
}

impl Steps for crate::Claim {
    fn admitted(&self) -> u64 {
        self.record().need.mem
    }

    fn run_mem(&self) -> Option<u64> {
        self.record().need.run_mem
    }

    fn step_down(&mut self) -> io::Result<()> {
        crate::Claim::step_down(self)
    }

    fn step_back(&mut self) -> io::Result<()> {
        crate::Claim::step_back(self)
    }
}

/// A step a read took ([`Phases::read`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Its run phase began, and it stepped down to its run phase's memory, bytes.
    Down(u64),
    /// Its run outgrew its step, and it stepped back up to what it was admitted with, bytes.
    Up(u64),
}

/// A HELD CLAIM'S PHASES, read from its tree ([`mod@crate::phase`]).
#[derive(Debug, Default)]
pub struct Phases {
    /// Whether its run phase has begun.
    entered: bool,
    /// What it stepped down to, bytes; `None` where it has not.
    stepped: Option<u64>,
    /// Whether it stepped back up.
    raised: bool,
    /// The most its run phase held since it began ([`Self::held`]), bytes.
    run_peak: u64,
    /// The members (a pid and its start) a read found before its run phase began: its
    /// compile's, whose lifetime peaks are no run's.
    compiled: HashSet<(u32, u64)>,
}

impl Phases {
    /// A claim whose run has not begun.
    #[must_use]
    pub fn new() -> Phases {
        Phases::default()
    }

    /// One read of the claim's tree: `members`, the processes it holds now, each with its
    /// footprint and its lifetime peak (the meter's latest read,
    /// [`crate::tree_meter::TreeMeter::members`]); `image` reads a pid's executable ([`image`]
    /// live). Steps `claim` as the module says, by what its run phase holds ([`Self::held`]), and
    /// answers the step it took. A step whose record could not be written is not taken: a step
    /// down is not tried again (the claim claims its peak on), and a step back up is, at each
    /// read its run still holds more than the step.
    pub fn read(
        &mut self,
        claim: &mut dyn Steps,
        members: &[Member],
        image: &dyn Fn(u32) -> Option<PathBuf>,
    ) -> Option<Step> {
        let entering = !self.entered;
        if entering {
            if !members
                .iter()
                .any(|m| image(m.pid).is_some_and(|p| built(&p)))
            {
                self.compiled
                    .extend(members.iter().map(|m| (m.pid, m.start)));
                return None;
            }
            self.entered = true;
        }
        let held = self.held(members);
        self.run_peak = self.run_peak.max(held);
        if entering {
            let run = claim.run_mem().filter(|run| *run < claim.admitted())?;
            if held > run || claim.step_down().is_err() {
                return None;
            }
            self.stepped = Some(run);
            return Some(Step::Down(run));
        }
        match self.stepped {
            Some(run) if !self.raised && held > run => {
                claim.step_back().ok()?;
                self.raised = true;
                Some(Step::Up(claim.admitted()))
            }
            _ => None,
        }
    }

    /// WHAT ITS RUN PHASE HOLDS at a read of `members`: what they hold together now, or the
    /// most one of them its compile never ran has held in its life, whichever is more: the rule
    /// the meter keeps its whole run's peak by ([`crate::tree_meter::Tally::peak`]), so what a
    /// test binary took between two reads is its run's too. A process a read found before the
    /// run began (the driver) counts by what it holds now: the most it held was its compile's.
    #[must_use]
    pub fn held(&self, members: &[Member]) -> u64 {
        let together = members
            .iter()
            .map(|m| m.footprint)
            .fold(0, u64::saturating_add);
        let one = members
            .iter()
            .filter(|m| !self.compiled.contains(&(m.pid, m.start)))
            .map(|m| m.peak)
            .max()
            .unwrap_or(0);
        together.max(one)
    }

    /// The most its run phase held since it began ([`Self::held`]), bytes: what its holder
    /// keeps under [`run_key`]. `None` where it never began.
    #[must_use]
    pub const fn run_peak(&self) -> Option<u64> {
        if self.entered {
            Some(self.run_peak)
        } else {
            None
        }
    }
}

#[cfg(test)]
#[path = "phase_tests.rs"]
mod tests;
