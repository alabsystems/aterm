// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A test binary's kin (W-kin, `docs/DESIGN-leftover-processes-2026-10-16.md` §4.1): the
//! processes of its tree that left its process group, for a group of their own
//! (`process_group(0)`) or a session of their own (`setsid`), each booked by its identity while
//! verify holds the binary unreaped, and handed to the run's record once the binary has left its
//! run ([`Left`]). verify's own group end ([`super::group::Live`]) reaches the binary's group
//! alone; the run's record ends the kin, each by pid and never by group
//! ([`super::StageGroupEnd::stage_left`], `crate::runs`).

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

use aterm_uds::runs::{Kin, LINK, LiveKernel, bind};

use super::{Host, StageGroupEnd};

/// What one test binary's kin book holds.
#[derive(Debug, Default)]
struct Book {
    /// The rows bound or tried, by pid and the sweep's start: each is tried once.
    tried: BTreeSet<(u32, u64)>,
    /// What it booked.
    booked: BTreeSet<Kin>,
    /// When it last read the tree at its cadence.
    read_at: Option<Instant>,
}

/// A test binary's kin book, kept for the binary's life and never replaced at a read: each row
/// of its tree outside its group is bound to the binary once ([`bind`]: its chain of parents,
/// read hop by hop, reaches the binary or a booked kin, through no spared program), and each it
/// binds is told to the run's record as it is booked ([`StageGroupEnd::kin_seen`]).
pub(super) struct KinTrack<'a> {
    groups: &'a dyn StageGroupEnd,
    label: &'a str,
    root: Kin,
    /// The directories W-kin never proves ([`StageGroupEnd::kin_spared`]).
    spared: &'a [PathBuf],
    book: RefCell<Book>,
}

impl<'a> KinTrack<'a> {
    /// The book of the test binary `pid`, which leads its own group and which the caller holds
    /// unreaped; `None` where its lineage does not read.
    pub(super) fn new(groups: &'a dyn StageGroupEnd, label: &'a str, pid: u32) -> Option<Self> {
        let lin = aterm_uds::process::read_lineage(i32::try_from(pid).ok()?)?;
        Some(Self {
            groups,
            label,
            root: Kin::of(&lin),
            spared: groups.kin_spared(),
            book: RefCell::default(),
        })
    }

    /// The binary's group, which it leads.
    fn pgid(&self) -> u32 {
        self.root.pid.unsigned_abs()
    }

    /// Book what `host` reads of the binary's tree: at its cadence ([`Host::usage_every`]), from
    /// the sweep the clock's own tree reads share ([`Host::tree_kin`]); or, `fresh`, from a sweep
    /// begun now ([`Host::fresh_tree_kin`]), as each of `finish`'s own kill paths reads it before
    /// its group signal.
    pub(super) fn read(&self, host: &dyn Host, fresh: bool) {
        let Ok(mut book) = self.book.try_borrow_mut() else {
            return;
        };
        let now = Instant::now();
        if !fresh
            && book
                .read_at
                .is_some_and(|at| now.saturating_duration_since(at) < host.usage_every())
        {
            return;
        }
        book.read_at = Some(now);
        let pgid = self.pgid();
        let rows = if fresh {
            host.fresh_tree_kin(pgid)
        } else {
            host.tree_kin(pgid)
        };
        for row in rows.into_iter().flatten() {
            if row.pid == pgid || row.pgid == pgid || !book.tried.insert((row.pid, row.start)) {
                continue;
            }
            let Ok(pid) = i32::try_from(row.pid) else {
                continue;
            };
            let mut reads = 0;
            if let Some(kin) = bind(
                &LiveKernel,
                pid,
                Some(self.root),
                &book.booked,
                self.spared,
                LINK,
                &mut reads,
            ) && book.booked.insert(kin)
            {
                self.groups.kin_seen(pgid, self.label, &kin);
            }
        }
    }
}

/// Hands a test binary's kin book to its run's record once, as it drops
/// ([`StageGroupEnd::stage_left`]). Made right after the binary's group is held, it drops as the
/// child's run returns, on every path: after the binary's reap wherever the run reaped it (a
/// binary left as it is after its `SIGKILL`, and one an interrupt of the run is ending, is not
/// reaped by then). Nothing is handed while the thread unwinds a panic.
pub(super) struct Left<'t, 'a>(pub(super) Option<&'t KinTrack<'a>>);

impl Drop for Left<'_, '_> {
    fn drop(&mut self) {
        let Some(track) = self.0 else {
            return;
        };
        if std::thread::panicking() {
            return;
        }
        let Ok(book) = track.book.try_borrow() else {
            return;
        };
        track
            .groups
            .stage_left(track.pgid(), track.label, &track.root, &book.booked);
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "exec_kin_tests.rs"]
mod tests;
