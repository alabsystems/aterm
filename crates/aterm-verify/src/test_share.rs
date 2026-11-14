// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TEST DISPATCH'S SHARE OF THE MACHINE, READ AGAIN AT EACH START.
//!
//! A run's capacity ledger grants each claim of cores its fair share of the machine at the look
//! that admits it (`aterm_capacity`'s `grant_iter`): the cores no claim holds, shared with the
//! earlier waiters and with the work no claim holds, which the ledger reads off the 5-minute
//! load. A test run claims once for its recording pass and then once for each binary, and two
//! things follow:
//! - THE RECORDING GRANT IS ONE LOOK. It is taken as the run's own test compile ends, and an
//!   ordinary compile claims memory and no cores, so the load it left reads as somebody else's
//!   work (12 cores at a 5-minute load of 24 grant 4) though it is gone minutes later. A
//!   dispatch held to that grant keeps to it until its last binary ends.
//! - A CLAIM'S SHARE IS NOT A RUN'S. Each binary's claim is shared out of what the claims before
//!   it left and is never granted less than one core while one is unheld, so a run's binaries
//!   together take every core whatever the load: twelve one-core claims at that load of 24 are
//!   granted twelve cores, where one claim of twelve is granted 4.
//!
//! So the dispatch holds the binaries it has running to ONE share, read again at each start
//! ([`Live::refuses`]): what the ledger's rule grants one claim of the cores the run asked, that
//! claim standing for every binary the run has running ([`fair`]). The run's own binaries are
//! that claim, so their claims are neither cores held against it nor a party to the share, and
//! what they put into the 5-minute load is read off their measured CPU ([`OwnLoad`]) and taken
//! out of the work nobody claims. The share rises as the load the run's compile left decays and
//! as other work ends, and falls as other work starts. A binary that starts with none beside it
//! asks the ledger alone, whose grant to that one claim is the same share.
//!
//! The widths, charges and peaks of the plan stay those of the recording grant
//! ([`crate::testrun::Cores::planned_on`]): the share as the plan was made, and the widths the
//! kept runs measured each binary's memory and timing at. What the live share moves is how many
//! binaries run side by side.
//!
//! THE SUBPROCESS SLOTS FOLLOW THE SHARE ([`Slots`]). The run's pool of multi-process test
//! stacks is made for the cores asked, and the run itself holds the slots the share does not
//! cover, so the stacks its binaries boot rise and fall with the binaries it runs.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_capacity::{Rank, Reading, Record};
use aterm_uds::file_lock::LockFile;

/// What a test binary's ledger claim is called ([`aterm_capacity::Need::what`]): how a look
/// tells this run's binaries from its other claims.
pub(crate) const BINARY_CLAIM: &str = "verify test binary";

/// What the ledger's records say of everyone but this run's test binaries, as the ledger reads
/// them for a new claim (`aterm_capacity`'s `ranked_grant` and `grant_iter`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Others {
    /// The cores their held claims were granted.
    pub(crate) held: u32,
    /// What those held claims' jobs have put into the 5-minute load: each one's cores, by how
    /// long it has been held ([`aterm_capacity::admit::LOAD5_TIME_CONSTANT`]).
    pub(crate) in_load: f64,
    /// The claims of cores not yet admitted that are ahead of the dispatch: parties to the share.
    pub(crate) waiting: u32,
    /// The cores of those waiters that wait for cores alone, which the ledger keeps for them.
    pub(crate) reserved: u32,
}

/// Who asks: this run's dispatch, placed among the ledger's claims as one claim made when the
/// dispatch began would be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asker {
    /// This process.
    pub(crate) pid: u32,
    /// The processes above it, whose claims its own admission leaves out (the ledger's kin).
    pub(crate) ancestors: Vec<u32>,
    /// The rank its binaries claim at.
    pub(crate) rank: Rank,
    /// When the dispatch began, unix ms: a waiter that arrived later is behind it.
    pub(crate) since_ms: u64,
}

impl Asker {
    /// This process, asking at `rank` from now on: its ancestors read once from `machine`.
    pub(crate) fn live(machine: &dyn aterm_capacity::Machine, rank: Rank) -> Asker {
        let pid = std::process::id();
        Asker {
            pid,
            ancestors: machine
                .procs()
                .map(|procs| procs.ancestors(pid))
                .unwrap_or_default(),
            rank,
            since_ms: unix_ms(),
        }
    }
}

/// Now, unix ms: the ledger's own clock for a claim's age.
fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

impl Others {
    /// Read `records` for `asker` at `now_ms`. Left out: a claim of no cores, a claim one of the
    /// asker's ancestors holds, and the asker's own test binaries, which are the claim the share
    /// is asked for.
    pub(crate) fn of(records: &[Record], asker: &Asker, now_ms: u64) -> Others {
        let tau = aterm_capacity::admit::LOAD5_TIME_CONSTANT.as_secs_f64();
        let mut others = Others::default();
        for r in records.iter().filter(|r| r.need.cores > 0) {
            let kin = r.accounting == aterm_capacity::Accounting::Holder
                && asker.ancestors.contains(&r.pid);
            if kin || (r.pid == asker.pid && r.need.what == BINARY_CLAIM) {
                continue;
            }
            match r.held {
                Some(held) => {
                    let age = Duration::from_millis(now_ms.saturating_sub(held.admitted_ms));
                    others.held = others.held.saturating_add(held.cores);
                    others.in_load +=
                        f64::from(held.cores) * (1.0 - (-age.as_secs_f64() / tau).exp());
                }
                None if (r.need.rank, r.order()) < (asker.rank, (asker.since_ms, asker.pid, 0)) => {
                    others.waiting = others.waiting.saturating_add(1);
                    if r.need.mem == 0
                        && r.need.margin == 0
                        && r.need.disk == 0
                        && r.need.quiet.is_none()
                    {
                        others.reserved = others.reserved.saturating_add(r.need.cores);
                    }
                }
                None => {}
            }
        }
        others
    }
}

/// The dispatch's fair share of `machine`, of the `asked` cores: the ledger's rule for one new
/// claim (`grant_iter`, bounded by the cores no claim holds or reserves as `ranked_grant` bounds
/// it), with the load this run's own binaries put in (`own_load`) taken out of the work nobody
/// claims. The cores the `others` leave are shared with the waiters ahead and with that
/// unclaimed work in machines' worth, rounded to the nearest core.
///
/// At least one, where the ledger grants a new claim none once every core is held or reserved:
/// this bounds what runs beside a binary already running, and a binary with none beside it asks
/// the ledger, which holds it at none until a core is free.
pub(crate) fn fair(asked: u32, machine: &Reading, others: &Others, own_load: f64) -> u32 {
    let cores = f64::from(machine.cores.max(1));
    let unclaimed = machine.load5.map_or(0.0, |load| {
        (load - others.in_load - own_load.max(0.0)).max(0.0)
    });
    let share = (cores - f64::from(others.held)).max(0.0)
        / (1.0 + f64::from(others.waiting) + unclaimed / cores);
    let share = if share.is_finite() && share >= 1.0 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "finite, at least one and at most the machine's cores"
        )]
        let whole = share.round() as u32;
        whole
    } else {
        1
    };
    let room = machine
        .cores
        .saturating_sub(others.held)
        .saturating_sub(others.reserved);
    share.min(asked).min(room).max(1)
}

/// WHAT THIS RUN'S BINARIES PUT INTO THE 5-MINUTE LOAD: their measured CPU rate, averaged as
/// the load is ([`aterm_capacity::admit::LOAD5_TIME_CONSTANT`]). A binary's ledger claim cannot
/// say it, as it says a long-held claim's: a claim is as old as its binary, and the load a
/// binary that ended put in outlives its claim.
///
/// A rate that goes unread (no meter, a descendant that left its process group) reads as work
/// nobody claimed, which lowers the share: the error is toward fewer binaries at once.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct OwnLoad {
    at: Option<Instant>,
    load: f64,
}

impl OwnLoad {
    /// Note that the binaries used `rate` cores since the last note, as of `now`: the load they
    /// have put in.
    pub(crate) fn note(&mut self, rate: f64, now: Instant) -> f64 {
        if let Some(at) = self.at {
            let tau = aterm_capacity::admit::LOAD5_TIME_CONSTANT.as_secs_f64();
            let kept = (-now.saturating_duration_since(at).as_secs_f64() / tau).exp();
            let rate = if rate.is_finite() { rate.max(0.0) } else { 0.0 };
            self.load = self.load * kept + rate * (1.0 - kept);
        }
        self.at = Some(now);
        self.load
    }
}

/// One look at the machine and the ledger.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Look {
    /// The machine's reading.
    pub(crate) machine: Reading,
    /// Everyone but this run's test binaries.
    pub(crate) others: Others,
}

/// The share as one look found it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Standing {
    /// The dispatch's share, cores.
    pub(crate) share: u32,
    /// The look it was read from; `None` where the ledger could not be read, and the share is
    /// the recording pass's grant.
    pub(crate) look: Option<Look>,
    /// The load this run's binaries had put in ([`OwnLoad`]).
    pub(crate) own_load: f64,
    /// The subprocess slots and boot permits left open to the tests ([`Slots::follow`]).
    pub(crate) slots: Option<[usize; 2]>,
}

struct State {
    own: OwnLoad,
    read: Option<(Instant, u32)>,
    /// The least and the most the share has been.
    range: Option<(u32, u32)>,
}

/// The live share of one dispatch ([`fair`], read again as [`Live::read`] is asked).
pub(crate) struct Live {
    asked: u32,
    /// The recording pass's grant: the ledger's own last word, used where no look can be taken.
    granted: u32,
    look: Box<dyn Fn() -> Option<Look> + Send + Sync>,
    slots: Option<Slots>,
    state: Mutex<State>,
}

impl Live {
    /// A dispatch that asked `asked` cores and whose recording pass was granted `granted`,
    /// looking through `look`, its subprocess pool `slots` held to the share from now on.
    pub(crate) fn new(
        asked: u32,
        granted: u32,
        look: Box<dyn Fn() -> Option<Look> + Send + Sync>,
        slots: Option<Slots>,
    ) -> Live {
        let live = Live {
            asked: asked.max(1),
            granted: granted.clamp(1, asked.max(1)),
            look,
            slots,
            state: Mutex::new(State {
                own: OwnLoad::default(),
                read: None,
                range: None,
            }),
        };
        if let Some(slots) = &live.slots {
            slots.follow(live.granted);
        }
        live
    }

    /// A dispatch on the user's ledger and this machine, its binaries claiming at `rank`.
    pub(crate) fn on_the_ledger(
        asked: u32,
        granted: u32,
        rank: Rank,
        slots: Option<Slots>,
    ) -> Live {
        let asker = Asker::live(&aterm_capacity::Live, rank);
        Live::new(
            asked,
            granted,
            Box::new(move || {
                use aterm_capacity::Machine as _;
                let ledger = aterm_capacity::Ledger::user()?;
                if ledger.cohort_aware().ok()? {
                    return None;
                }
                let records = ledger.list().ok()?;
                Some(Look {
                    machine: aterm_capacity::Live.read(None),
                    others: Others::of(&records, &asker, unix_ms()),
                })
            }),
            slots,
        )
    }

    /// The share now, the run's binaries using `rate` cores at their last reads: read again
    /// once the last read is [`aterm_capacity::admit::RESOURCE_PROBE_MAX_AGE`] old, with what
    /// that look found where it moved the share (or is the first). The subprocess slots follow
    /// each look.
    pub(crate) fn read(&self, rate: f64, now: Instant) -> (u32, Option<Standing>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, share)) = state.read
            && now.saturating_duration_since(at) < aterm_capacity::admit::RESOURCE_PROBE_MAX_AGE
        {
            return (share, None);
        }
        let own_load = state.own.note(rate, now);
        let look = (self.look)();
        let share = look.as_ref().map_or(self.granted, |look| {
            fair(self.asked, &look.machine, &look.others, own_load)
        });
        let slots = self.slots.as_ref().map(|slots| slots.follow(share));
        let moved = state.read.is_none_or(|(_, was)| was != share);
        state.read = Some((now, share));
        state.range = Some(
            state
                .range
                .map_or((share, share), |(lo, hi)| (lo.min(share), hi.max(share))),
        );
        (
            share,
            moved.then_some(Standing {
                share,
                look,
                own_load,
                slots,
            }),
        )
    }

    /// Why a binary charged `charge` cores does not start now beside the run's binaries holding
    /// `held` cores and using `rate`: its charge does not fit in what they leave of the share,
    /// or their live rate with its charge is past the share and one core
    /// ([`crate::test_plan::Dispatcher`]'s two rules, on the live share). A binary with none
    /// beside it is never refused here: it asks the ledger.
    pub(crate) fn refuses(
        &self,
        charge: u32,
        held: u32,
        rate: f64,
        now: Instant,
    ) -> (Option<Refused>, Option<Standing>) {
        let (share, moved) = self.read(rate, now);
        let refused = (held > 0
            && (held.saturating_add(charge) > share
                || rate + f64::from(charge) > f64::from(share) + 1.0))
            .then_some(Refused {
                share,
                asked: self.asked,
            });
        (refused, moved)
    }

    /// The cores the run asked: the most the share is.
    pub(crate) const fn asked(&self) -> u32 {
        self.asked
    }

    /// The least and the most the share has been at a look; `None` before the first.
    pub(crate) fn range(&self) -> Option<(u32, u32)> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .range
    }
}

/// A start the live share refused ([`Live::refuses`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Refused {
    /// The share at that look.
    pub(crate) share: u32,
    /// The cores the run asked.
    pub(crate) asked: u32,
}

/// The two kinds of member a subprocess pool holds, by their file names' stems.
const KINDS: [&str; 2] = ["slot", "boot"];

/// THE RUN'S HOLD ON ITS OWN SUBPROCESS POOL. The pool (`aterm_spec::subprocess_lane`) is a
/// directory of lock files made once, for the cores the run asked; a test thread that boots a
/// multi-process stack holds one of them while the stack lives. The stacks' processes are in
/// their binary's ledger claim only where its kept runs measured them, and those that leave its
/// process group never are (KI-1103-3), so the slots are the bound they have. The run holds the
/// slots the live share does not cover ([`Slots::follow`]), by the same lock a test takes: a
/// held-back slot reads as busy to every test, and one a test holds is taken once it is free.
pub(crate) struct Slots {
    dir: PathBuf,
    /// The pool's stack slots and boot permits.
    total: [usize; 2],
    held: Mutex<[Vec<(usize, LockFile)>; 2]>,
}

impl Slots {
    /// The pool at `dir`, made with `slots` stack slots
    /// ([`crate::testrun::subprocess_slots_for_cores`] of the cores asked).
    pub(crate) fn of(dir: &Path, slots: usize) -> Slots {
        let slots = slots.clamp(1, 64);
        Slots {
            dir: dir.to_path_buf(),
            total: [slots, crate::testrun::subprocess_boot_slots(slots)],
            held: Mutex::new([Vec::new(), Vec::new()]),
        }
    }

    /// Leave the tests the slots a share of `cores` cores sizes, and its boot permits, holding
    /// the rest: the open stack slots and boot permits now. A slot a test holds stays the
    /// test's; the next call tries again.
    pub(crate) fn follow(&self, cores: u32) -> [usize; 2] {
        let slots = crate::testrun::subprocess_slots_for_cores(cores).min(self.total[0]);
        let open = [
            slots,
            crate::testrun::subprocess_boot_slots(slots).min(self.total[1]),
        ];
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let mut left = [0; 2];
        for (kind, held) in held.iter_mut().enumerate() {
            let back = self.total[kind] - open[kind];
            held.truncate(back);
            for index in (0..self.total[kind]).rev() {
                if held.len() >= back {
                    break;
                }
                if held.iter().any(|(at, _)| *at == index) {
                    continue;
                }
                let path = self.dir.join(format!("{}-{index:04}.lock", KINDS[kind]));
                if let Ok(lock) = LockFile::take_existing(&path, Instant::now(), "busy") {
                    held.push((index, lock));
                }
            }
            left[kind] = self.total[kind] - held.len();
        }
        left
    }
}

#[cfg(test)]
#[path = "test_share_tests.rs"]
mod tests;
