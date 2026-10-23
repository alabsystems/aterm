// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel process-event edge gatherer ([`ProcWatch`], [`ProcWatch::follow`],
//! [`FgCutter::proc_seen`], [`ExecEdges::absorb`]).

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
    /// Stuck-engine edge notified on process events ([`crate::vitals_host::PROC_EDGE`]).
    proc_edge: &'static crate::vitals_host::ProcEdge,
}

impl ExecEdges {
    /// The gather's half for `watch`, reading images with `read` (`SessionFactory::edge_image`).
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
            proc_edge: &crate::vitals_host::PROC_EDGE,
        }
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
    pub(crate) fn take_image(&mut self) -> Option<Arc<HolderImage>> {
        self.image.take()
    }

    /// A sampled edge to `to` was found: watch its leader, then hear what is queued.
    pub(crate) fn absorb(&mut self, to: i32) -> bool {
        let start = std::time::Instant::now();
        self.watch.follow(to);
        let mut events = std::mem::take(&mut self.events);
        self.watch.drain(&mut events);
        crate::metrics::note_proc_events(events.len());
        self.tell_vitals(&events);
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
