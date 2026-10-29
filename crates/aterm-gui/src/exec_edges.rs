// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel process-event edge gatherer ([`ProcWatch`], [`ProcWatch::follow`],
//! [`FgCutter::proc_seen`], [`ExecEdges::absorb`]), and the deaths it hears
//! ([`ExecEdges::take_gone`], `holder_image::DrawerGroups`).

use std::collections::VecDeque;
use std::sync::Arc;

use aterm_uds::procwatch::{NoWatch, ProcEvent, ProcWatch};

use crate::foreground_handback::{FgCutter, ProcSeen};
use crate::holder_image::{HolderImage, ReadImage};

/// `SessionFactory::proc_watch` as shipped: the session's kqueue watch ([`ProcWatch::open`]).
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

/// Whether the shipping factory makes process watches ([`aterm_types::dev_seam!`]).
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
    /// The image this gather last read for an edge, and the group it was read for.
    held: Option<Arc<HolderImage>>,
    /// The image the batch's kernel edge was read with, until the batch goes.
    image: Option<Arc<HolderImage>>,
    /// Image and group of the last folded `exec` ([`ExecEdges::read_edge`]).
    folded: Option<Arc<HolderImage>>,
    read: ReadImage,
    /// The watch's overflow count last booked in `metrics`.
    overflow: u64,
    /// Watched processes heard exiting since the last [`ExecEdges::take_gone`], oldest first.
    gone: Vec<i32>,
    /// Stuck-engine edge notified on process events ([`crate::vitals_host::PROC_EDGE`]).
    proc_edge: &'static crate::vitals_host::ProcEdge,
    /// Where each heard `exec` is heard as it is drained ([`crate::command_ran::Ran`]).
    feed: crate::command_ran::RanFeed,
    /// Reads the bytes the PTY holds unread, with the master it is asked of (`TIOCOUTQ`;
    /// `None` where it cannot be read, and each exec is placed by its stamp).
    queued: (fn(i32) -> Option<usize>, i32),
}

impl ExecEdges {
    /// The gather's half for `watch`, reading images with `read` (`SessionFactory::edge_image`)
    /// and noting each `exec` it drains in `feed`.
    pub(crate) fn new(
        mut watch: ProcWatch,
        read: ReadImage,
        holder: i32,
        feed: crate::command_ran::RanFeed,
    ) -> Self {
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
            gone: Vec::new(),
            proc_edge: &crate::vitals_host::PROC_EDGE,
            feed,
            queued: (|_| None, -1),
        }
    }

    /// This watch reading the PTY's unread bytes with `queued` asked of `master`
    /// (`aterm_pty::output_queue_len`), so each `exec` heard in front is placed where the shell's
    /// output stood when it was heard ([`crate::command_ran::Spot`]).
    #[must_use]
    pub(crate) const fn reading_queue(
        mut self,
        queued: fn(i32) -> Option<usize>,
        master: i32,
    ) -> Self {
        self.queued = (queued, master);
        self
    }

    /// The descriptor the gather's polls include.
    pub(crate) fn fd(&self) -> i32 {
        self.watch.fd()
    }

    /// Drain readable process events after `filled` bytes into `cutter`.
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
        self.tell_vitals(&events);
        let mut fg = FgOnce::new(fg);
        self.note_execs(&events, filled, || fg.get(), None);
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

    /// Notify the stuck engine's kernel edge ([`crate::vitals_host::ProcEdge`]).
    fn tell_vitals(&self, events: &[ProcEvent]) {
        if !events.is_empty() {
            self.proc_edge.heard();
        }
    }

    /// Hear each `exec` of one drain in the session's ran log, as it is drained (a deferred one
    /// is heard once, when drained, not again when replayed), when its group holds the terminal's
    /// foreground, read once for the drain by `fg` (or, after a sampled edge, is `held`, the
    /// group the edge found in front): a background job's or a hook's `exec` outside the
    /// foreground is no command's, so it confirms nothing. The PTY's unread bytes are read once
    /// first, after the drain and before the foreground, so `filled` and them, with the bytes
    /// handed on, are where the shell's output stood when each was heard; an exec whose group
    /// still held the foreground then is placed there ([`crate::command_ran::Spot`]): the shell
    /// wrote its block's C before starting its job and writes the block's D only once the job is
    /// done. It counts once the parse stage has parsed there ([`crate::command_ran::Ran::hear`]).
    fn note_execs(
        &self,
        events: &[ProcEvent],
        filled: usize,
        mut fg: impl FnMut() -> i32,
        held: Option<i32>,
    ) {
        let mut read: Option<(Option<usize>, i32)> = None;
        let shell = self.feed.shell();
        for ev in events {
            if let ProcEvent::Exec { pid, pgid, .. } = *ev {
                let (queued, front) = *read.get_or_insert_with(|| {
                    let (probe, master) = self.queued;
                    (probe(master), fg())
                });
                if pgid > 1 && (pgid == front || held == Some(pgid)) {
                    let spot = crate::command_ran::Spot {
                        filled,
                        queued: queued.filter(|_| pgid == front),
                        job: pgid != shell,
                    };
                    self.feed.exec(pid, pgid, spot);
                }
            }
        }
    }

    /// Whether `exec`s wait for a byte-free edge of their own.
    pub(crate) fn has_deferred(&self) -> bool {
        !self.deferred.is_empty()
    }

    /// Feed waiting `exec`s between batches until one cuts an edge ([`ExecEdges::heard`]).
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
            if let ProcEvent::Exit { pid } = ev {
                self.gone.push(pid);
            }
            return cutter.carries_edge();
        };
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

    /// Read `to`'s image and check for a card edge ([`ExecEdges::read_edge`]).
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
    pub(crate) const fn take_image(&mut self) -> Option<Arc<HolderImage>> {
        self.image.take()
    }

    /// A sampled edge to `to` was found after `filled` bytes: watch its leader, then hear what
    /// is queued, each exec of `to`'s group heard whether or not it still holds the foreground
    /// `fg` reads now, and placed where the shell's output stood only when it does.
    pub(crate) fn absorb(&mut self, to: i32, filled: usize, fg: impl FnMut() -> i32) -> bool {
        let start = std::time::Instant::now();
        self.watch.follow(to);
        let mut events = std::mem::take(&mut self.events);
        self.watch.drain(&mut events);
        crate::metrics::note_proc_events(events.len());
        self.tell_vitals(&events);
        self.note_execs(&events, filled, fg, Some(to));
        let mut absorbed = false;
        for ev in events.drain(..) {
            match ev {
                ProcEvent::Exec { pid, .. } if pid == to => absorbed = true,
                ProcEvent::Exec { .. } => self.deferred.push_back(ev),
                ProcEvent::Exit { pid } => self.gone.push(pid),
                ProcEvent::Fork { .. } => {}
            }
        }
        self.events = events;
        self.book_overflow();
        crate::metrics::note_proc_drain(crate::metrics::leg_elapsed_ns(start));
        absorbed
    }

    /// The watched processes heard exiting since the last take, oldest first: the gather
    /// forwards each to the parse stage, which retires a stopped drawer's mark
    /// ([`crate::holder_image::DrawerGroups`]).
    pub(crate) fn take_gone(&mut self) -> std::vec::Drain<'_, i32> {
        self.gone.drain(..)
    }

    /// A sampled edge's new holder's image, held.
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

/// One drain's `tcgetpgrp(master)`.
struct FgOnce<F> {
    probe: F,
    read: Option<i32>,
}

impl<F: FnMut() -> i32> FgOnce<F> {
    const fn new(probe: F) -> Self {
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
