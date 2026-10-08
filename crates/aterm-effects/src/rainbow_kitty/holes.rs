// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE HOLE TRIPWIRE** — a development sensor for the shape the band's
//! light laws rule out and the owner has seen live: a dark column INSIDE a lit
//! typed line (2026-09-21, on the shipped 0.90.0: *"I still see gaps
//! appearing in the rainbow cursor trail!"* — the three spaces after
//! `Adding`, `Fable.` and `HEY!` dark, every glyph beside them lit). That
//! exact shape never reproduced on a probe; the harnesses that census the
//! plan offline (`tests/phrase_pause_gaps.rs` and its siblings) pin the paths
//! a code reading found. This records the NEXT live occurrence instead of
//! guessing at it.
//!
//! **Who does what.** The engine senses and keeps the records; it reads no
//! environment and writes no file (§2.7: an engine crate carries no platform
//! coupling). A host arms it with [`Engine::trace_holes`] and takes the
//! records with [`Engine::drain_hole_records`] — `aterm-gui` does both under
//! the development seam `ATERM_TRACE_TRAIL_HOLES`, printing each record's
//! lines to stderr as `TRAILHOLE …` beside `ATERM_TRACE_SPAWN`'s `SPAWNSRC`,
//! and a shipped binary never arms it.
//!
//! **The walk**, after the frame is planned and emitted: a column of the
//! caret's row that no typing cell owns (none resident there that is not
//! leaving) — or, above one slab per cell, whose planned samples carry no
//! light — between two columns that are owned and lit by typing cells of any
//! cohort. A sample belongs to the column whose `[x0, x1)` holds it, its left
//! boundary vertex included — `floor((x − origin) / cw)`, as the harness
//! census reads the plan. At one slab per cell (a long composer line:
//! `Ribbon::slabs_for` clamps there) a run's only vertices sit on the cell
//! edges, one sample per column on its left edge — too few to call a column
//! dark by — so the OWNER census alone decides ([`row_holes`]).
//!
//! **The record**, made on the FIRST frame of each new hole: the clock since
//! the trace's first frame, the hole's `(row, col)`, the caret and whether
//! the mirror is known, the frame's slabs per cell, the row's cohort table,
//! every resident cell on the row with its stamps and the light attributed to
//! its column, the content witness's records for the hole column and its two
//! neighbours, and the last [`HOLE_TRACE_EVENTS`] events the engine replayed
//! with their licences. At most [`HOLE_RECORDS_HELD`] wait for a drain; past
//! that a record is counted, not kept, and the next header says how many.
//!
//! **Cost.** Unarmed: one `is_some` per tick, no ring, no walk, no
//! allocation. Armed: a walk of the caret row per frame and a record per new
//! hole — a development build's price.
//!
//! Written 2026-09-21 as item E of the four-ways fix (A-C landed as
//! b920e29a6 without it, E armed by a raw environment read and writing its
//! own file from the engine) and ported onto the reworked engine 2026-09-30
//! with the seam and the file moved to the host.

use std::collections::VecDeque;
use std::fmt::Write as _;

use aterm_time::Instant;

use super::{Engine, Event, ribbon};
use crate::cursor_glow::Geom;

/// How many replayed events a hole record carries, oldest first.
pub(super) const HOLE_TRACE_EVENTS: usize = 32;

/// How many records wait for a drain before new ones are only counted.
pub(super) const HOLE_RECORDS_HELD: usize = 64;

/// **THE HOLE TRIPWIRE'S STATE** (the module doc). Built only when the trace
/// is armed, so an unarmed engine allocates none of it.
#[derive(Clone, Debug, Default)]
pub(super) struct HoleTrace {
    /// The clock a record's `t=` counts from: the first frame the armed
    /// trace walked (the engine reads no clock of its own).
    epoch: Option<Instant>,
    /// The last [`HOLE_TRACE_EVENTS`] events the engine replayed, oldest
    /// first, with their clocks.
    recent: VecDeque<(Event, Instant)>,
    /// The holes open on the last frame, `(row, col)`: a record is made on
    /// the FIRST frame of a hole only.
    open: Vec<(u16, u16)>,
    /// This frame's holes, swapped into `open` after the walk.
    next: Vec<(u16, u16)>,
    /// Planned coverage per column of the caret's row, this frame.
    cov: Vec<u8>,
    /// Records made and not yet drained, oldest first.
    records: Vec<String>,
    /// Records made while [`HOLE_RECORDS_HELD`] were waiting, over the
    /// trace's life.
    dropped: u64,
}

impl HoleTrace {
    /// One replayed event into the ring, the oldest out past
    /// [`HOLE_TRACE_EVENTS`].
    fn remember(&mut self, ev: Event, at: Instant) {
        if self.recent.len() == HOLE_TRACE_EVENTS {
            self.recent.pop_front();
        }
        self.recent.push_back((ev, at));
    }
}

/// **THE CENSUS** of the caret's row (the module doc), `cov` its columns'
/// planned light: each column that is not lit, between two that are. A column
/// is lit when a typing cell owns it (`owned`) and, above one slab per cell,
/// its samples carry light. At one slab a column holds a single sample, the
/// vertex on its left edge, too little to call it dark by, so the owner census
/// alone decides there. The row's first and last columns are never holes.
pub(super) fn row_holes<'a>(
    cov: &'a [u8],
    slabs: usize,
    owned: impl Fn(usize) -> bool + 'a,
) -> impl Iterator<Item = usize> + 'a {
    let lit = move |col: usize, light: u8| owned(col) && (slabs == 1 || light > 0);
    cov.windows(3)
        .enumerate()
        .filter(move |&(i, w)| !lit(i + 1, w[1]) && lit(i, w[0]) && lit(i + 2, w[2]))
        .map(|(i, _)| i + 1)
}

impl Engine {
    /// **ARM (or disarm) THE HOLE TRIPWIRE** (the module doc of
    /// `rainbow_kitty::holes`). Arming an armed engine keeps its state — a
    /// host may call this every frame; disarming drops it, records included.
    pub fn trace_holes(&mut self, on: bool) {
        match (on, self.holes.is_some()) {
            (true, false) => self.holes = Some(Box::default()),
            (false, true) => self.holes = None,
            _ => {}
        }
    }

    /// The hole tripwire's records since the last drain, oldest first —
    /// each one or more `\n`-separated lines, the first starting
    /// `== trail hole`. Empty when unarmed.
    pub fn drain_hole_records(&mut self) -> impl Iterator<Item = String> + '_ {
        self.holes
            .as_mut()
            .map(|tr| tr.records.drain(..))
            .into_iter()
            .flatten()
    }

    /// The frame's events into the tripwire's ring before the tick drops
    /// them — bridge sweeps included. Nothing when unarmed.
    pub(super) fn remember_for_holes(&mut self) {
        if let Some(tr) = &mut self.holes {
            for &(ev, at) in &self.events {
                tr.remember(ev, at);
            }
        }
    }

    /// The tripwire's walk on the planned frame (the module doc). Nothing
    /// when unarmed.
    pub(super) fn sense_holes_after_emit(&mut self, now: Instant, geom: Geom) {
        if let Some(mut tr) = self.holes.take() {
            self.sense_holes(&mut tr, now, geom);
            self.holes = Some(tr);
        }
    }

    /// **THE WALK** — one record per NEW hole on the caret's row.
    fn sense_holes(&self, tr: &mut HoleTrace, now: Instant, geom: Geom) {
        tr.epoch.get_or_insert(now);
        let row = self.caret.0;
        let cols = geom.cols;
        let mut cov = std::mem::take(&mut tr.cov);
        cov.clear();
        cov.resize(cols, 0);
        let cw = geom.cw.max(1) as f32;
        let ch = geom.ch.max(1) as f32;
        let (ox, oy) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
        for s in self.ribbon.plan_segments() {
            if ((s.spine - oy) / ch - 0.5).floor() != f32::from(row) {
                continue;
            }
            let col = ((s.x - ox) / cw).floor();
            if col < 0.0 || col >= cols as f32 {
                continue;
            }
            let c = col as usize;
            cov[c] = cov[c].max(s.cov);
        }
        let owned = |col: usize| {
            self.ribbon
                .cells()
                .iter()
                .any(|c| c.row == row && usize::from(c.col) == col && c.typing && !c.leaving())
        };
        let mut next = std::mem::take(&mut tr.next);
        next.clear();
        next.extend(
            row_holes(&cov, self.ribbon.slabs_per_cell(), owned)
                .map(|col| (row, u16::try_from(col).unwrap_or(u16::MAX))),
        );
        tr.cov = cov;
        for &(hrow, hcol) in &next {
            if tr.open.contains(&(hrow, hcol)) {
                continue;
            }
            if tr.records.len() < HOLE_RECORDS_HELD {
                let record = self.hole_record(tr, now, hrow, hcol);
                tr.records.push(record);
            } else {
                tr.dropped += 1;
            }
        }
        tr.open.clear();
        tr.open.append(&mut next);
        tr.next = next;
    }

    /// One tripwire record (the module doc's fields), one line per part.
    fn hole_record(&self, tr: &HoleTrace, now: Instant, row: u16, col: u16) -> String {
        let age = |at: Instant| now.saturating_duration_since(at).as_millis();
        let mut s = String::new();
        let _ = writeln!(
            s,
            "== trail hole t=+{:.3}s row={row} col={col} caret=({},{}) known={} slabs={} \
             dropped={}",
            now.saturating_duration_since(tr.epoch.unwrap_or(now))
                .as_secs_f32(),
            self.caret.0,
            self.caret.1,
            self.caret_known,
            self.ribbon.slabs_per_cell(),
            tr.dropped
        );
        let _ = write!(s, "cohorts:");
        for k in self.ribbon.cohorts().iter().filter(|k| k.row == row) {
            let _ = write!(
                s,
                " (id={} anchor_col={} retract_col={:?} phase={:?} phrase={} wake={} \
                 abandoned={} alive_age_ms={} flow_age_ms={:?})",
                k.id,
                k.anchor_col,
                k.retract_col,
                k.phase,
                k.phrase,
                k.wake,
                k.abandoned,
                age(k.alive_at),
                k.flow.map(age)
            );
        }
        let _ = writeln!(s);
        let _ = write!(s, "cells@row {row}:");
        let mut cells: Vec<&ribbon::Cell> = self
            .ribbon
            .cells()
            .iter()
            .filter(|c| c.row == row)
            .collect();
        cells.sort_by_key(|c| (c.col, c.born));
        for c in cells {
            let _ = write!(
                s,
                " (col={} cohort={} born_age_ms={} typing={} layer={:?} cov={} retract={} \
                 retire={} released={})",
                c.col,
                c.cohort,
                age(c.born),
                c.typing,
                c.layer,
                tr.cov.get(usize::from(c.col)).copied().unwrap_or(0),
                c.retract_at.is_some(),
                c.retire_at.is_some(),
                c.released_at.is_some()
            );
        }
        let _ = writeln!(s);
        let _ = write!(s, "witness:");
        for c in col.saturating_sub(1)..=col.saturating_add(1) {
            let _ = write!(s, " ({row},{c})=[");
            self.witness.describe_cell(row, c, now, &mut s);
            let _ = write!(s, "]");
        }
        let _ = writeln!(s);
        let _ = write!(s, "events (oldest first, last {}):", tr.recent.len());
        for &(ev, at) in &tr.recent {
            let _ = write!(s, " (-{}ms {:?})", age(at), ev);
        }
        s
    }
}
