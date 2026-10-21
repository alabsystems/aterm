// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KERNEL EDGES, the gather's half (card engine §3.3): the session's
//! [`ProcWatch`] heard at each poll the PTY gather already makes, turned into
//! foreground edges by [`FgCutter::proc_seen`].
//!
//! The watch joins the gather's polls (`aterm_pty::read_or_wake`, both parks of
//! `drain_more_nonblocking`), so a batch with no process event costs no system
//! call. When it is readable the gather drains it here, at the offset the poll
//! left the batch at, with `tcgetpgrp` sampled once for the drain, and only for
//! a drain that holds an `exec` (a fork's and an exit's need none). An `exec`
//! is an edge only when it enters or leaves a card-relevant image (a drawer,
//! or atpkg's shim launching one) in the group in front or in place of its
//! leader; its image is read here, at the event, so the parse stage publishes
//! it before any slice that may hold the image's bytes. Everything else (a
//! `/usr/bin/true`, a compiler's `exec`s, a member's `exec`) folds into the
//! sampled cut after one image read.
//!
//! At most one edge per batch, as before: an `exec` heard while the batch
//! already carries one waits here and goes out after the batch as a byte-free
//! edge (`GatherMsg::Edge`). A sampled edge first watches its new group's
//! leader ([`ProcWatch::follow`]) and hears what is queued: an `exec` of that
//! leader is this edge's own, read with it, and moves where the new holder's
//! run begins to the end of the batch ([`ExecEdges::absorb`]), since its bytes
//! may follow that `exec` anywhere after the cut.

use std::collections::VecDeque;
use std::sync::Arc;

use aterm_uds::procwatch::{NoWatch, ProcEvent, ProcWatch};

use crate::foreground_handback::{FgCutter, ProcSeen};
use crate::holder_image::{HolderImage, ReadImage};

/// `SessionFactory::proc_watch` as shipped: the session's kqueue watch
/// ([`ProcWatch::open`]), counting a reader on macOS that wanted one and got
/// none for want of a kqueue (`EMFILE`; `metrics`' `procwatch_fallbacks`): it
/// runs the byte-driven arm in full. A gone shell and a platform with no
/// `exec` event are no fallback.
pub(crate) fn shipping_watch(shell: i32) -> Option<ProcWatch> {
    match ProcWatch::open(shell) {
        Ok(watch) => Some(watch),
        Err(NoWatch::NoQueue) => {
            crate::metrics::note_procwatch_fallback();
            None
        }
        Err(NoWatch::NoShell | NoWatch::Unsupported) => None,
    }
}

/// Whether the shipping factory makes process watches: always, but in a build
/// that compiles dev seams started with `ATERM_DEV_NO_PROCWATCH=1`
/// ([`aterm_types::dev_seam!`]), read once at startup, which measures the
/// readers without one in the same binary (§8's fork loop, the watch on
/// against off).
pub(crate) fn watch_wanted() -> bool {
    !aterm_types::dev_seam!("ATERM_DEV_NO_PROCWATCH").is_some_and(|v| v == "1")
}

/// One reader's process watch and what its gather knows of the holder's image.
#[derive(Debug)]
pub(crate) struct ExecEdges {
    watch: ProcWatch,
    /// One drain's events (kept for its allocation).
    events: Vec<ProcEvent>,
    /// `exec`s heard while the batch carried its edge, oldest first.
    deferred: VecDeque<ProcEvent>,
    /// The image this gather last read for an edge, and the group it was read
    /// for: what an `exec` leaves, and what a late `exec` is compared with.
    held: Option<Arc<HolderImage>>,
    /// The image the batch's kernel edge was read with, until the batch goes.
    image: Option<Arc<HolderImage>>,
    /// The image read at the last `exec` that folded (no edge), and its group:
    /// the sampled edge to that group, with no `exec` of its leader heard
    /// since, gives it to the read as the group's last
    /// ([`ExecEdges::read_edge`]), which keeps it re-stamped when the leader's
    /// start and executable are unchanged (a fork loop's command is read once,
    /// not twice).
    folded: Option<Arc<HolderImage>>,
    read: ReadImage,
    /// The watch's overflow count last booked in `metrics`.
    overflow: u64,
}

impl ExecEdges {
    /// The gather's half for `watch`, reading images with `read`
    /// (`SessionFactory::edge_image`). `holder` is the cutter's seed: watched
    /// from the start when it is not the shell (an adopted or restored job the
    /// watch did not see forked is still adopted by `ProcWatch::new`; one past
    /// the cap is followed here).
    pub(crate) fn new(mut watch: ProcWatch, read: ReadImage, holder: i32) -> Self {
        if holder > 0 && holder != watch.shell() {
            watch.follow(holder);
        }
        Self {
            watch,
            events: Vec::new(),
            deferred: VecDeque::new(),
            held: None,
            image: None,
            folded: None,
            read,
            overflow: 0,
        }
    }

    /// The descriptor the gather's polls include.
    pub(crate) fn fd(&self) -> i32 {
        self.watch.fd()
    }

    /// The watch was readable after `filled` bytes of this batch (a park it
    /// ended, or `0`: before the batch's first byte), and `fg` reads
    /// `tcgetpgrp(master)` (at most once, after the drain, for its first
    /// `exec`). Drain it into `cutter`; `true` delivers the batch now (it
    /// carries an edge).
    pub(crate) fn heard(
        &mut self,
        cutter: &mut FgCutter,
        fg: impl FnMut() -> i32,
        filled: usize,
    ) -> bool {
        let start = std::time::Instant::now();
        let mut events = std::mem::take(&mut self.events);
        self.watch.drain(&mut events);
        crate::metrics::note_proc_events(events.len());
        let mut fg = FgOnce::new(fg);
        let mut deliver = false;
        for ev in events.drain(..) {
            deliver |= self.one(cutter, ev, &mut fg, filled);
        }
        self.events = events;
        cutter.proc_drained(filled);
        self.book_overflow();
        crate::metrics::note_proc_drain(crate::metrics::leg_elapsed_ns(start));
        deliver
    }

    /// Whether `exec`s wait for a byte-free edge of their own.
    pub(crate) fn has_deferred(&self) -> bool {
        !self.deferred.is_empty()
    }

    /// Between batches: feed the waiting `exec`s to a cutter just begun, until
    /// one is an edge (`true`; the rest keep waiting) or none is left. `fg` as
    /// for [`ExecEdges::heard`].
    pub(crate) fn replay(&mut self, cutter: &mut FgCutter, fg: impl FnMut() -> i32) -> bool {
        let mut fg = FgOnce::new(fg);
        while let Some(ev) = self.deferred.pop_front() {
            if self.one(cutter, ev, &mut fg, 0) {
                return true;
            }
        }
        cutter.proc_drained(0);
        false
    }

    fn one<F: FnMut() -> i32>(
        &mut self,
        cutter: &mut FgCutter,
        ev: ProcEvent,
        fg: &mut FgOnce<F>,
        filled: usize,
    ) -> bool {
        let ProcEvent::Exec { pid, pgid, late } = ev else {
            return cutter.carries_edge();
        };
        // A leader's `exec` retires the image its group folded with, edge or
        // not: the leader is no longer that image.
        if self.folded.as_ref().is_some_and(|f| f.pgid == pid) {
            self.folded = None;
        }
        if cutter.carries_edge() {
            self.deferred.push_back(ev);
            return true;
        }
        let from = cutter.holder();
        let fg = fg.get();
        let edge = cutter.proc_seen(ProcSeen { pid, pgid, fg }, filled, |_, to| {
            self.relevant(from, to, late)
        });
        if edge {
            crate::metrics::note_exec_edge();
        }
        edge
    }

    /// Read `to`'s image now and say whether an edge from `from` to it is the
    /// card's business: it is a drawer or a launcher, or the image it leaves
    /// is (or is unknown to this gather: fail toward cutting). A `late` event,
    /// which may restate an `exec` already read, is an edge only where the
    /// image differs from the one held.
    ///
    /// The read is fresh (no last image to keep): the kernel says the image
    /// changed, and a kept read cannot see an `exec` of the same executable
    /// (atpkg's `sh` reroute stub `exec`ing its `sh` twin). One that folds is
    /// kept for the sampled edge's read ([`ExecEdges::read_edge`]).
    fn relevant(&mut self, from: i32, to: i32, late: bool) -> bool {
        let image = Arc::new((self.read)(to, None));
        let left = self
            .held
            .as_ref()
            .filter(|h| h.pgid == from)
            .is_none_or(|h| h.card_relevant());
        let changed = self
            .held
            .as_ref()
            .filter(|h| h.pgid == to)
            .is_none_or(|h| !h.same_image(&image));
        if (image.card_relevant() || left) && (!late || changed) {
            self.image = Some(Arc::clone(&image));
            self.held = Some(image);
            self.folded = None;
            return true;
        }
        self.folded = Some(image);
        false
    }

    /// The image the batch's kernel edge was read with, once.
    pub(crate) fn take_image(&mut self) -> Option<Arc<HolderImage>> {
        self.image.take()
    }

    /// A sampled edge to `to` was found: watch its leader, then hear what is
    /// queued. `true` when that includes an `exec` of the leader: the image
    /// read next names it, and its bytes may lie anywhere after the cut. Every
    /// other `exec` waits for after the batch.
    pub(crate) fn absorb(&mut self, to: i32) -> bool {
        let start = std::time::Instant::now();
        self.watch.follow(to);
        let mut events = std::mem::take(&mut self.events);
        self.watch.drain(&mut events);
        crate::metrics::note_proc_events(events.len());
        let mut absorbed = false;
        for ev in events.drain(..) {
            match ev {
                ProcEvent::Exec { pid, .. } if pid == to => absorbed = true,
                ProcEvent::Exec { .. } => self.deferred.push_back(ev),
                ProcEvent::Fork { .. } | ProcEvent::Exit { .. } => {}
            }
        }
        self.events = events;
        self.book_overflow();
        crate::metrics::note_proc_drain(crate::metrics::leg_elapsed_ns(start));
        absorbed
    }

    /// A sampled edge's new holder's image, held. Read afresh (as A1 reads
    /// every sampled edge) when `absorbed` (its leader's `exec` came with the
    /// edge); otherwise given the image the folded `exec` of that group read,
    /// if any, which the read keeps when the leader's start and executable
    /// are unchanged: every `exec` of the leader since was heard (it is
    /// registered from its fork, or followed), and would have replaced it.
    pub(crate) fn read_edge(&mut self, to: i32, absorbed: bool) -> Arc<HolderImage> {
        let folded = self.folded.take().filter(|f| !absorbed && f.pgid == to);
        let image = Arc::new((self.read)(to, folded.as_deref()));
        self.held = Some(Arc::clone(&image));
        image
    }

    fn book_overflow(&mut self) {
        let now = self.watch.overflow();
        if now > self.overflow {
            crate::metrics::note_procwatch_overflow(now - self.overflow);
            self.overflow = now;
        }
    }
}

/// One drain's `tcgetpgrp(master)`: read when its first `exec` needs it, and
/// then the same for the rest of the drain.
struct FgOnce<F> {
    probe: F,
    read: Option<i32>,
}

impl<F: FnMut() -> i32> FgOnce<F> {
    fn new(probe: F) -> Self {
        Self { probe, read: None }
    }

    fn get(&mut self) -> i32 {
        *self.read.get_or_insert_with(&mut self.probe)
    }
}

#[cfg(test)]
#[path = "exec_edges_tests.rs"]
mod tests;

/// The watch on a real `zsh -f -i`: a launch chain's fork and `exec`s, and
/// the controls that are neither.
#[cfg(all(test, target_vendor = "apple"))]
#[path = "exec_edges_kernel_tests.rs"]
mod kernel_tests;
