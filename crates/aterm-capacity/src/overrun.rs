// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! OVERRUN: a holder whose process tree holds more memory than its held claims claim. A claim's
//! reservation counts what its tree is credited with ([`crate::Proc::of_reading`]: the lesser of
//! each process's footprint and its resident size), which leaves out the pages the kernel
//! compressed or swapped, so a tree can hold twice its claim in compressed pages while every
//! figure a line shows of it reads as within its claim. At 18:00 on 2026-10-30 on this 48 GiB
//! M4 Pro, a Trust lane check held a 16 GiB claim while its trustc's footprint was 33 GB, 25 GiB
//! of it compressed; `aterm pkg capacity` said its holder's tree held 9.3 GiB, and each of the 26
//! claims waiting behind it that it held 6.7 GiB of memory. What a tree holds whole
//! ([`TreeUse::whole`]) is reckoned here against what its holder claimed, and both lines that
//! name a held claim say where it is past it: a waiter's ([`crate::Holder`]) and `aterm pkg
//! capacity`'s. No reservation changes.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::admit::{Other, gib, others};
use crate::floor::GIB;
use crate::ledger::{Accounting, Record};
use crate::reading::TreeUse;

/// The least a line names, a tenth of a GiB ([`gib`] shows tenths): a tree holds a figure past
/// another only by this much or more.
pub const TENTH: u64 = GIB / 10;

/// What the holder of held memory claims holds past what they claim (OVERRUN, [`overruns`]),
/// bytes. A line says it `30.7 GiB, 21.4 GiB of it compressed or swapped and 9.3 GiB in memory:
/// 14.7 GiB past its 16 GiB claim`, a holder of several claims past `the 24 GiB of its 2 claims`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overrun {
    /// What its tree, with the processes its held claims are credited with, holds whole,
    /// compressed and swapped pages included ([`TreeUse::whole`]).
    pub holds: u64,
    /// What of [`Self::holds`] its claims' reservations count ([`TreeUse::footprint`]).
    pub counted: u64,
    /// The memory its held claims claim, summed.
    pub claimed: u64,
    /// How many held claims of memory that is.
    pub claims: usize,
}

impl Overrun {
    /// What it holds past its claims.
    #[must_use]
    pub const fn past(&self) -> u64 {
        self.holds.saturating_sub(self.claimed)
    }

    /// What it holds past its claims, as a line says it: `14.7 GiB past its 16 GiB claim`, or
    /// `6 GiB past the 24 GiB of its 2 claims`.
    #[must_use]
    pub fn beyond(&self) -> String {
        if self.claims > 1 {
            format!(
                "{} past the {} of its {} claims",
                gib(self.past()),
                gib(self.claimed),
                self.claims
            )
        } else {
            format!("{} past its {} claim", gib(self.past()), gib(self.claimed))
        }
    }
}

impl fmt::Display for Overrun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", whole(self.holds, self.counted), self.beyond())
    }
}

/// What a tree holds, `whole` bytes of which its claims count `counted` ([`TreeUse`]), as a line
/// says it: `30.7 GiB, 21.4 GiB of it compressed or swapped and 9.3 GiB in memory`, or the counted
/// figure alone (`9.3 GiB`) where the whole is not [`TENTH`] or more past it. The part named
/// compressed or swapped is what the tree's footprints count past its resident pages: the pages
/// the kernel compressed or swapped of it, less any file-backed pages it has resident (a macOS
/// resident size counts those, a footprint does not).
#[must_use]
pub fn whole(whole: u64, counted: u64) -> String {
    let whole = whole.max(counted);
    if whole - counted < TENTH {
        gib(counted)
    } else {
        format!(
            "{}, {} of it compressed or swapped and {} in memory",
            gib(whole),
            gib(whole - counted),
            gib(counted)
        )
    }
}

/// What each holder among `others` holds past the memory its held claims claim, by its pid: its
/// tree's use (the same for every claim it holds, counted once) with that of the processes each
/// of its held claims of memory is credited with, against those claims' memory summed. Only a
/// holder [`TENTH`] or more past them is named. A claim of whole or detached accounting
/// ([`Accounting`]) attributes no current process to its record's pid, which may since have been
/// reused, and reserves its whole need, so it is neither reckoned nor named.
pub(crate) fn overruns_of(others: &[Other<'_>]) -> HashMap<u32, Overrun> {
    let mut out: HashMap<u32, Overrun> = HashMap::new();
    for o in others.iter().filter(|o| {
        o.record.accounting == Accounting::Holder
            && o.record.held.is_some()
            && o.record.need.mem > 0
    }) {
        let sum = out.entry(o.record.pid).or_insert(Overrun {
            holds: o.used.whole.max(o.used.footprint),
            counted: o.used.footprint,
            claimed: 0,
            claims: 0,
        });
        sum.holds = sum
            .holds
            .saturating_add(o.credited.whole.max(o.credited.footprint));
        sum.counted = sum.counted.saturating_add(o.credited.footprint);
        sum.claimed = sum.claimed.saturating_add(o.record.need.mem);
        sum.claims += 1;
    }
    out.retain(|_, o| o.past() >= TENTH);
    out
}

/// What each holder of `records` (a ledger's claims, [`crate::Ledger::list`]) holds past the
/// memory its held claims claim (OVERRUN), by its pid, with `trees` the use of their holders' and
/// credited processes' trees ([`crate::Procs::trees`]; `None`, no process sweep, so none is
/// known): each credited process counted once, toward the first claim that names it, as
/// [`crate::reserved`] counts it. What `aterm pkg capacity` says beside each held claim.
#[must_use]
pub fn overruns(
    records: &[Record],
    trees: Option<&HashMap<u32, TreeUse>>,
) -> HashMap<u32, Overrun> {
    let Some(trees) = trees else {
        return HashMap::new();
    };
    let records: Vec<&Record> = records.iter().collect();
    let taken: HashSet<u32> = records.iter().map(|r| r.pid).collect();
    overruns_of(&others(&records, taken, trees))
}

#[cfg(test)]
#[path = "overrun_tests.rs"]
mod tests;
