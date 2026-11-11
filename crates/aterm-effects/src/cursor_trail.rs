// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cursor motion trail: pure animation state.

use aterm_time::Instant;
use std::time::Duration;

use aterm_render::TrailCell;

use crate::cursor_glow::{TypedStamps, band_row, hint_fresh_dur, take_hint_fresh_dur};

const HEAD_ALPHA_GENTLE: u32 = 42;
const HEAD_ALPHA_IGNITED: u32 = 70;
const TAIL_ALPHA_GENTLE: u32 = 12;
const TAIL_ALPHA_IGNITED: u32 = 44;
const READABLE_ALPHA_CAP: u8 = 70;

/// The most coverage any bed cell is born with ([`READABLE_ALPHA_CAP`]).
pub const BED_PEAK_ALPHA: u8 = READABLE_ALPHA_CAP;
const IGNITED_LIFE_MULT: f32 = 1.6;
const CHAIN_LIFE_MULT: f32 = 1.5;

/// ConPTY hide-bridge window, shared by the trail and glow engines.
pub const HIDE_BRIDGE_MS: u64 = 150;
/// Maximum Chebyshev distance in cells for the default hide-bridge reappear.
pub const HIDE_BRIDGE_MAX_DIST: u16 = 2;
/// Maximum Chebyshev distance in cells for the hide bridge while a typed key hint is fresh.
pub const HIDE_BRIDGE_TYPED_MAX_DIST: u16 = 8;
/// Rows a nav-witnessed landing may cross.
pub const HIDE_BRIDGE_NAV_MAX_ROWS: u16 = 4;

/// Columns a nav-witnessed landing may cross when the host never named a pane.
pub const HIDE_BRIDGE_NAV_UNPANED_COLS: u16 = 80;

/// Window after a navigation press in which the caret may still be seen at rest.
pub const NAV_NOOP_SETTLE_MS: u64 = 80;

/// Maximum duration after a navigation press for a hidden-to-visible landing to be bridged.
pub const NAV_BRIDGE_LANDING_MS: u64 = 150;

/// May a nav-witnessed hidden→visible landing use `seen` as its source cell?
#[must_use]
#[inline]
pub fn nav_bridge_source_ok(key: Instant, seen: Instant) -> bool {
    key.saturating_duration_since(seen).as_millis() as u64 <= HIDE_BRIDGE_MS
        && seen.saturating_duration_since(key).as_millis() as u64 <= NAV_NOOP_SETTLE_MS
}

/// The nav-bridge admission, called by both engines.
#[must_use]
#[inline]
pub fn nav_bridge_admits(key: Instant, seen: Instant, now: Instant) -> bool {
    nav_bridge_source_ok(key, seen)
        && now.saturating_duration_since(key).as_millis() as u64 <= NAV_BRIDGE_LANDING_MS
}

/// Reach bounds in cells for one hidden-to-visible reappear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgeReach {
    /// Columns the landing may sit from the source.
    pub cols: u16,
    /// Rows the landing may sit from the source.
    pub rows: u16,
    /// `Some(first column of the focused pane)` on the nav lane.
    pub line_start: Option<u16>,
}

impl BridgeReach {
    /// Does this reach admit a hidden→visible landing at `to` from `from` (`(row, col)` each)?
    #[must_use]
    #[inline]
    pub const fn admits(self, from: (u16, u16), to: (u16, u16)) -> bool {
        let dr = to.0.abs_diff(from.0);
        let dc = to.1.abs_diff(from.1);
        if dr > self.rows || dc > self.cols {
            return false;
        }
        match self.line_start {
            Some(col0) if dr >= 2 => {
                to.0 < from.0 && to.1.saturating_sub(col0) <= HIDE_BRIDGE_TYPED_MAX_DIST
            }
            _ => true,
        }
    }

    #[must_use]
    #[inline]
    const fn ball(d: u16) -> Self {
        Self {
            cols: d,
            rows: d,
            line_start: None,
        }
    }
}

/// The bridge reach for one reappear, shared by the trail and glow engines.
#[must_use]
#[inline]
pub const fn hide_bridge_reach(
    typed_hint_fresh: bool,
    nav_hint_fresh: bool,
    pane: Option<(u16, u16)>,
) -> BridgeReach {
    if nav_hint_fresh {
        let (col0, cols) = match pane {
            Some((c0, c)) if c > 0 => (c0, c),
            _ => (0, HIDE_BRIDGE_NAV_UNPANED_COLS),
        };
        BridgeReach {
            cols: if cols > HIDE_BRIDGE_TYPED_MAX_DIST {
                cols
            } else {
                HIDE_BRIDGE_TYPED_MAX_DIST
            },
            rows: HIDE_BRIDGE_NAV_MAX_ROWS,
            line_start: Some(col0),
        }
    } else if typed_hint_fresh {
        BridgeReach::ball(HIDE_BRIDGE_TYPED_MAX_DIST)
    } else {
        BridgeReach::ball(HIDE_BRIDGE_MAX_DIST)
    }
}

/// Hidden→visible relocations the trail's hide bridge has judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgeTally {
    /// Relocations the bridge admitted a source cell for.
    pub bridged: u64,
    /// Relocations the bridge declined (no source cell: `spawn` never ran).
    pub declined: u64,
}

impl Default for BridgeTally {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl BridgeTally {
    /// Zeroed bridge tally.
    pub const EMPTY: Self = Self {
        bridged: 0,
        declined: 0,
    };
}

#[inline]
const fn lerp_u32(a: u32, b: u32, q: f32) -> u32 {
    let q = q.clamp(0.0, 1.0);
    (a as f32 + (b as f32 - a as f32) * q).round() as u32
}

#[inline]
const fn trail_presentation_fingerprint(cells: &[TrailCell], color: u32) -> u64 {
    let mut fp: u64 = 0;
    let mut i = 0;
    while i < cells.len() {
        let cell = &cells[i];
        fp = fp
            .wrapping_mul(1_000_003)
            .wrapping_add(((cell.row as u64) << 24) ^ ((cell.col as u64) << 8) ^ cell.alpha as u64);
        i += 1;
    }
    if !cells.is_empty() {
        fp = fp
            .wrapping_mul(1_000_003)
            .wrapping_add((color as u64) | (1 << 32));
    }
    fp
}

/// Apply a host presentation-opacity envelope to emitted trail cells and return their fingerprint.
#[must_use]
#[inline]
pub fn project_trail_presentation(cells: &mut Vec<TrailCell>, color: u32, opacity: f32) -> u64 {
    if cells.is_empty() {
        return 0;
    }
    let opacity = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if opacity == 0.0 {
        cells.clear();
        return 0;
    }
    if opacity >= 1.0 {
        cells.retain(|cell| cell.alpha > 0);
    } else {
        cells.retain_mut(|cell| {
            cell.alpha = (f32::from(cell.alpha) * opacity).round() as u8;
            cell.alpha > 0
        });
    }
    trail_presentation_fingerprint(cells, color)
}

/// Resolved tunables for the trail effect.
#[derive(Clone, Copy, Debug)]
pub struct TrailConfig {
    /// Whether the comet trail is enabled and allowed by Serious Mode.
    pub enabled: bool,
    /// How long a swept cell takes to fade fully out.
    pub duration: Duration,
    /// Maximum comet length in cells.
    pub max_len: usize,
    /// Fully-resolved trail colour `0x00RRGGBB`.
    pub color: u32,
    /// Ignition intensity in `0.0..=1.0` from the host's typing cadence ([`TypingCadence`]).
    pub intensity: f32,
    /// Sub-ignition warmth in `0.0..=1.0` ([`TypingCadence::warmth`]).
    pub warmth: f32,
}

#[derive(Clone, Copy)]
struct Spark {
    row: u16,
    col: u16,
    born_alpha: u8,
    born: Instant,
    life_ms: u64,
}

#[derive(Clone, Copy)]
struct DedupSlot {
    epoch: u64,
    key: u32,
    out_idx: u32,
}

impl Default for DedupSlot {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl DedupSlot {
    const EMPTY: Self = Self {
        epoch: 0,
        key: 0,
        out_idx: 0,
    };
}

/// Per-window trail animation state.
pub struct CursorTrail {
    sparks: Vec<Spark>,
    last: Option<(u16, u16)>,
    last_visible: Option<((u16, u16), Instant)>,
    type_hint: TypedStamps,
    nav_hint: Option<Instant>,
    move_hint: Option<Instant>,
    blink_hint: Option<Instant>,
    ctx_alt: bool,
    pane_columns: Option<(u16, u16)>,
    bridge_tally: BridgeTally,
    dedup: Vec<DedupSlot>,
    dedup_epoch: u64,
    spawn_path: Vec<(i32, i32)>,
}

impl Default for CursorTrail {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl CursorTrail {
    const MAX_SPARKS: usize = 512;
    const DEDUP_SLOTS: usize = 1024;
    const TYPE_HINT_FRESH: f32 = 0.25;
    const TYPE_HINT_FRESH_DUR: Duration =
        Duration::from_nanos((Self::TYPE_HINT_FRESH as f64 * 1_000_000_000.0).round() as u64);
    const BLINK_HINT_FRESH: f32 = 0.5;
    const BLINK_HINT_FRESH_DUR: Duration =
        Duration::from_nanos((Self::BLINK_HINT_FRESH as f64 * 1_000_000_000.0).round() as u64);
    const NAV_HINT_FRESH: f32 = 0.25;
    const NAV_HINT_FRESH_DUR: Duration =
        Duration::from_nanos((Self::NAV_HINT_FRESH as f64 * 1_000_000_000.0).round() as u64);
    const MOVE_HINT_FRESH: f32 = 0.25;
    const MOVE_HINT_FRESH_DUR: Duration =
        Duration::from_nanos((Self::MOVE_HINT_FRESH as f64 * 1_000_000_000.0).round() as u64);

    /// Fresh dormant trail animation state.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sparks: Vec::new(),
            last: None,
            last_visible: None,
            type_hint: TypedStamps::EMPTY,
            nav_hint: None,
            move_hint: None,
            blink_hint: None,
            ctx_alt: false,
            pane_columns: None,
            bridge_tally: BridgeTally::EMPTY,
            dedup: Vec::new(),
            dedup_epoch: 0,
            spawn_path: Vec::new(),
        }
    }

    /// Hidden→visible relocations the hide bridge admitted or declined ([`BridgeTally`]).
    #[must_use]
    #[cfg(test)]
    pub const fn bridge_tally(&self) -> BridgeTally {
        self.bridge_tally
    }

    /// Last visible source owned by this engine.
    #[must_use]
    #[inline]
    pub const fn cursor_anchor(&self) -> Option<(u16, u16)> {
        match self.last {
            Some(cell) => Some(cell),
            None => match self.last_visible {
                Some((cell, _)) => Some(cell),
                None => None,
            },
        }
    }

    /// Host key hint for one plain typed-glyph or Backspace echo keypress.
    #[inline]
    pub const fn note_typed(&mut self, now: Instant) {
        self.type_hint.stamp(now);
    }

    /// Host coalesced typing across batched key events between two render callbacks.
    #[inline]
    pub const fn note_typed_batch(&mut self, now: Instant, keys: u32) {
        if keys == 0 {
            return;
        }
        if keys <= HIDE_BRIDGE_MAX_DIST as u32 {
            self.note_typed(now);
        } else {
            self.note_move(now);
        }
    }

    /// Host repaint blink (see `CursorGlow::note_repaint_blink`).
    #[inline]
    pub const fn note_repaint_blink(&mut self, now: Instant) {
        self.blink_hint = Some(now);
    }

    /// Drop the blink hint: a tab/pane switch pointed this window at a different terminal.
    #[inline]
    pub const fn clear_blink(&mut self) {
        self.blink_hint = None;
    }

    /// Host per-frame context (see `CursorGlow::note_context`).
    #[inline]
    pub const fn note_context(&mut self, alt: bool) {
        self.ctx_alt = alt;
    }

    /// Stamp the focused pane's exact columns in the coordinate space handed to [`Self::tick`].
    #[inline]
    pub const fn note_pane_columns(&mut self, first_col: u16, cols: usize) {
        self.pane_columns = if cols > 0 && cols <= u16::MAX as usize {
            Some((first_col, cols as u16))
        } else {
            None
        };
    }

    /// Disarm dangling movement state (see `CursorGlow::clear_typed`).
    #[inline]
    pub const fn clear_typed(&mut self) {
        self.type_hint.clear();
        self.nav_hint = None;
        self.move_hint = None;
    }

    /// Arm the navigation veto (see `CursorGlow::note_navigation`).
    #[inline]
    pub const fn note_navigation(&mut self, now: Instant) {
        self.clear_typed();
        self.nav_hint = Some(now);
    }

    #[inline]
    const fn note_move(&mut self, now: Instant) {
        self.clear_typed();
        self.move_hint = Some(now);
    }

    /// Classify a Tab completion or non-empty paste while preserving banked typed stamps.
    #[inline]
    pub const fn note_user_gesture(&mut self, now: Instant) {
        self.nav_hint = None;
        self.move_hint = Some(now);
    }

    /// Classify a plain Return while preserving banked typed stamps.
    #[inline]
    pub const fn note_return(&mut self, now: Instant) {
        self.move_hint = Some(now);
    }

    /// Classify a settled user resize/re-grid.
    #[cfg(test)]
    pub const fn note_reflow(&mut self, now: Instant) {
        self.note_move(now);
    }

    /// Explicit test/preview driver for a deliberately scripted move.
    #[doc(hidden)]
    #[inline]
    pub const fn note_synthetic_move(&mut self, now: Instant) {
        self.note_move(now);
    }

    /// Scripted typed-move twin for previews, examples, and benchmarks.
    #[doc(hidden)]
    #[inline]
    pub const fn note_synthetic_typed(&mut self, now: Instant) {
        self.note_typed(now);
    }

    /// Re-stamp the generic move slot at a completed write.
    #[inline]
    pub const fn note_move_delivered(&mut self, at: Instant) {
        self.move_hint = Some(at);
    }

    /// The nav-slot twin of [`Self::note_move_delivered`].
    #[inline]
    pub const fn note_motion_delivered(&mut self, at: Instant) {
        self.nav_hint = Some(at);
    }

    /// Cancel only the license written by one not-yet-egressed dispatch.
    #[inline]
    pub fn revoke_input_hints_at(&mut self, at: Instant) {
        self.type_hint.revoke_at(at);
        if self.nav_hint == Some(at) {
            self.nav_hint = None;
        }
        if self.move_hint == Some(at) {
            self.move_hint = None;
        }
    }

    /// PTY scroll translation (see `CursorGlow::note_scroll`).
    #[inline]
    pub fn note_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        self.remap_rows(|row| row.checked_sub(rows));
    }

    /// Translate sparks and cursor positions within row band `top..=bottom` by `delta`.
    #[inline]
    pub fn note_band_move(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        self.remap_rows(|row| band_row(row, top, bottom, delta));
    }

    #[inline]
    fn remap_rows(&mut self, map: impl Fn(u16) -> Option<u16>) {
        if let Some((row, _)) = &mut self.last {
            match map(*row) {
                Some(r) => *row = r,
                None => self.last = None,
            }
        }
        if let Some(((row, _), _)) = &mut self.last_visible {
            match map(*row) {
                Some(r) => *row = r,
                None => self.last_visible = None,
            }
        }
        self.sparks.retain_mut(|spark| match map(spark.row) {
            Some(row) => {
                spark.row = row;
                true
            }
            None => false,
        });
    }

    /// Whether any spark is still alive (the host keeps the animation timer armed).
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        !self.sparks.is_empty()
    }

    /// Drop the in-flight comet and forget the last cursor position.
    #[inline]
    pub fn reset(&mut self) {
        self.sparks.clear();
        self.spawn_path.clear();
        self.last = None;
        self.last_visible = None;
        self.clear_typed();
        self.blink_hint = None;
        self.ctx_alt = false;
        self.pane_columns = None;
    }

    /// Advance the trail one frame into `out` and return the cell fingerprint (`0` when idle).
    #[inline]
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &TrailConfig,
        out: &mut Vec<TrailCell>,
    ) -> u64 {
        out.clear();
        if !cfg.enabled {
            self.sparks.clear();
            self.last = cur;
            self.last_visible = cur.map(|c| (c, now));
            self.clear_typed();
            self.blink_hint = None;
            return 0;
        }

        let spawn_from = match self.last {
            Some(c) => Some(c),
            None => match (self.last_visible, cur) {
                (Some(((r, c), seen)), Some(to)) if (r, c) != to => {
                    let fresh =
                        now.saturating_duration_since(seen).as_millis() as u64 <= HIDE_BRIDGE_MS;
                    let nav_bridge = hint_fresh_dur(self.nav_hint, now, Self::NAV_HINT_FRESH_DUR)
                        && match self.nav_hint {
                            Some(key) => nav_bridge_admits(key, seen, now),
                            None => false,
                        };
                    if fresh || nav_bridge {
                        let typed_fresh = self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH_DUR);
                        let reach = hide_bridge_reach(typed_fresh, nav_bridge, self.pane_columns);
                        if reach.admits((r, c), to) {
                            Some((r, c))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            },
        };
        let hidden_relocation = self.last.is_none()
            && matches!(
                (cur, self.last_visible),
                (Some(current), Some((previous, _))) if current != previous
            );
        let declined_hidden_relocation = spawn_from.is_none() && hidden_relocation;
        if hidden_relocation {
            if spawn_from.is_some() {
                self.bridge_tally.bridged += 1;
            } else {
                self.bridge_tally.declined += 1;
            }
        }
        let completed_hidden_reappearance =
            self.last.is_none() && cur.is_some() && self.last_visible.is_some();
        let unseeded_visible = self.last.is_none() && self.last_visible.is_none() && cur.is_some();
        if let (Some((pr, pc)), Some((cr, cc))) = (spawn_from, cur)
            && (pr != cr || pc != cc)
        {
            self.spawn(pr, pc, cr, cc, now, cfg);
        } else if declined_hidden_relocation {
            self.clear_typed();
        } else if completed_hidden_reappearance {
            let fresh_nav = if let Some(t) = self.nav_hint
                && now.saturating_duration_since(t) <= Self::NAV_HINT_FRESH_DUR
            {
                Some(t)
            } else {
                None
            };
            self.clear_typed();
            self.nav_hint = fresh_nav;
        } else if unseeded_visible {
            self.clear_typed();
        }
        self.last = cur;
        if let Some(c) = cur {
            self.last_visible = Some((c, now));
        }

        if !self.sparks.is_empty() {
            if self.dedup.is_empty() {
                self.dedup = vec![DedupSlot::EMPTY; Self::DEDUP_SLOTS];
            }
            self.dedup_epoch += 1;
            let epoch = self.dedup_epoch;
            let dedup = &mut self.dedup;
            const MASK: usize = CursorTrail::DEDUP_SLOTS - 1;
            const SHIFT: u32 = 32 - CursorTrail::DEDUP_SLOTS.trailing_zeros();
            let mut last_cohort: Option<((Instant, u64), Option<u32>)> = None;
            self.sparks.retain(|s| {
                let cohort_key = (s.born, s.life_ms);
                let tf = match last_cohort {
                    Some((prev_key, cached)) if prev_key == cohort_key => cached,
                    _ => {
                        let age_ms = now.saturating_duration_since(s.born).as_millis() as u64;
                        let cached = if age_ms < s.life_ms {
                            Some(((s.life_ms - age_ms) * 255 / s.life_ms) as u32)
                        } else {
                            None
                        };
                        last_cohort = Some((cohort_key, cached));
                        cached
                    }
                };
                let Some(tf) = tf else {
                    return false;
                };
                let alpha = ((u32::from(s.born_alpha) * tf) / 255) as u8;
                if alpha == 0 {
                    return true;
                }
                let key = (u32::from(s.row) << 16) | u32::from(s.col);
                let mut i = (key.wrapping_mul(0x9E37_79B9) >> SHIFT) as usize;
                loop {
                    let slot = &mut dedup[i];
                    if slot.epoch != epoch {
                        *slot = DedupSlot {
                            epoch,
                            key,
                            out_idx: out.len() as u32,
                        };
                        out.push(TrailCell {
                            row: s.row as usize,
                            col: s.col as usize,
                            alpha,
                        });
                        break;
                    }
                    if slot.key == key {
                        let cell = &mut out[slot.out_idx as usize];
                        cell.alpha = cell.alpha.max(alpha);
                        break;
                    }
                    i = (i + 1) & MASK;
                }
                true
            });
        }

        trail_presentation_fingerprint(out, cfg.color)
    }

    /// The license, twin of `CursorGlow::move_licensed`: did a human touch the keyboard just now?
    #[must_use]
    #[inline]
    pub fn move_licensed(&self, now: Instant) -> bool {
        self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH_DUR)
            || hint_fresh_dur(self.nav_hint, now, Self::NAV_HINT_FRESH_DUR)
            || hint_fresh_dur(self.move_hint, now, Self::MOVE_HINT_FRESH_DUR)
    }

    #[inline]
    fn spawn(
        &mut self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &TrailConfig,
    ) -> bool {
        if !self.move_licensed(now) {
            return false;
        }
        let move_paired = !(cr == pr && cc > pc)
            && hint_fresh_dur(self.move_hint, now, Self::MOVE_HINT_FRESH_DUR);
        let paired = !move_paired
            && self
                .type_hint
                .take_fresh(now, Self::TYPE_HINT_FRESH_DUR)
                .is_some();
        let nav_paired =
            take_hint_fresh_dur(&mut self.nav_hint, now, Self::NAV_HINT_FRESH_DUR).is_some();
        let _ = self.move_hint.take();
        if nav_paired {
            return false;
        }
        let dr_abs = (cr as i32 - pr as i32).abs();
        let raw_dist = dr_abs.max((cc as i32 - pc as i32).abs());
        let physical_fold = matches!(
            self.pane_columns,
            Some((col0, cols)) if crate::trail_sweep::physical_margin_fold((pr, pc), (cr, cc), col0, cols)
        );
        if paired
            && !physical_fold
            && dr_abs <= 1
            && raw_dist > 2
            && (!self.ctx_alt || hint_fresh_dur(self.blink_hint, now, Self::BLINK_HINT_FRESH_DUR))
        {
            return false;
        }
        let max_len = cfg.max_len.max(1);
        self.spawn_path.clear();
        if physical_fold {
            self.spawn_path.push((i32::from(pr), i32::from(pc)));
        } else {
            crate::trail_sweep::line_cells_tail(
                &mut self.spawn_path,
                (i32::from(pr), i32::from(pc)),
                (i32::from(cr), i32::from(cc)),
                max_len,
                false,
            );
        }
        if self.spawn_path.is_empty() {
            return false;
        }
        let q = cfg.intensity.clamp(0.0, 1.0);
        let head = lerp_u32(HEAD_ALPHA_GENTLE, HEAD_ALPHA_IGNITED, q);
        let tail = lerp_u32(TAIL_ALPHA_GENTLE, TAIL_ALPHA_IGNITED, q);
        let base_dur = cfg.duration.as_millis().max(1) as f32;
        let life_ms = (base_dur
            * (1.0 + CHAIN_LIFE_MULT * cfg.warmth.clamp(0.0, 1.0) + (IGNITED_LIFE_MULT - 1.0) * q))
            .max(1.0) as u64;
        let n = self.spawn_path.len() as u32;
        let delta_alpha = head - tail;
        let mut i = 0;
        while i < self.spawn_path.len() {
            let (r, c) = self.spawn_path[i];
            let cur_i = i as u32;
            i += 1;
            if r < 0 || c < 0 {
                continue;
            }
            let frac = (cur_i + 1) * 255 / n; // 1/n .. 1, scaled to 0..=255
            let born_alpha = (tail + delta_alpha * frac / 255).min(READABLE_ALPHA_CAP as u32) as u8;
            self.sparks.push(Spark {
                row: r as u16,
                col: c as u16,
                born_alpha,
                born: now,
                life_ms,
            });
        }
        if self.sparks.len() > Self::MAX_SPARKS {
            let drop = self.sparks.len() - Self::MAX_SPARKS;
            self.sparks.drain(0..drop);
        }
        true
    }
}

/// Tunables for the typing-cadence heat → ignition mapping.
#[derive(Clone, Copy, Debug)]
pub struct CadenceParams {
    /// Heat added by one keystroke (arbitrary units).
    pub gain: f32,
    /// Heat half-life: the time for accumulated heat to halve with no typing.
    pub half_life: Duration,
    /// Heat where ignition begins to ramp; below it intensity is 0 (a few keys, slow typing).
    pub knee_lo: f32,
    /// Heat where ignition is full (intensity 1).
    pub knee_hi: f32,
}

impl CadenceParams {
    /// Default cadence tunables.
    pub const DEFAULT: Self = Self {
        gain: 1.0,
        half_life: Duration::from_millis(220),
        knee_lo: 2.0,
        knee_hi: 6.5,
    };
}

impl Default for CadenceParams {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[inline]
const fn decay_heat(heat: f32, dt: Duration, half_life: Duration) -> f32 {
    if heat <= 0.0 || dt.is_zero() {
        return heat.max(0.0);
    }
    let hl = half_life.as_secs_f32().max(1e-3);
    heat * crate::color_math::powf_pos_f32(0.5, dt.as_secs_f32() / hl)
}

/// Smoothstep mapping of `heat` between `knee_lo` and `knee_hi` into `0.0..=1.0`.
#[must_use]
#[inline]
pub const fn ignition_intensity(heat: f32, knee_lo: f32, knee_hi: f32) -> f32 {
    if knee_hi > knee_lo {
        let t = ((heat - knee_lo) / (knee_hi - knee_lo)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    } else if heat >= knee_hi {
        1.0
    } else {
        0.0
    }
}

/// Typing-cadence tracker → comet ignition intensity, with an injected `now`.
#[derive(Clone, Debug)]
pub struct TypingCadence {
    params: CadenceParams,
    heat: f32,
    last: Option<Instant>,
}

impl Default for TypingCadence {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TypingCadence {
    /// Cold tracker with [`CadenceParams::DEFAULT`].
    pub const DEFAULT: Self = Self::new(CadenceParams::DEFAULT);

    /// A cold tracker (no heat) with the given tunables.
    #[must_use]
    #[inline]
    pub const fn new(params: CadenceParams) -> Self {
        Self {
            params,
            heat: 0.0,
            last: None,
        }
    }

    /// Register one keystroke at `now`, decaying standing heat and adding one `gain` quantum.
    #[inline]
    pub fn on_keystroke(&mut self, now: Instant) {
        if self.heat > 0.0
            && let Some(last) = self.last
        {
            let dt = now.saturating_duration_since(last);
            self.heat = decay_heat(self.heat, dt, self.params.half_life);
        }
        self.heat = (self.heat + self.params.gain).min(self.params.knee_hi);
        self.last = Some(now);
    }

    #[must_use]
    #[inline]
    fn heat_at(&self, now: Instant) -> f32 {
        if self.heat <= 0.0 {
            return 0.0;
        }
        match self.last {
            Some(last) => {
                let dt = now.saturating_duration_since(last);
                decay_heat(self.heat, dt, self.params.half_life)
            }
            None => 0.0,
        }
    }

    #[must_use]
    #[inline]
    const fn warmth_of(&self, heat: f32) -> f32 {
        let span = (self.params.knee_lo - self.params.gain).max(f32::EPSILON);
        ((heat - self.params.gain) / span).clamp(0.0, 1.0)
    }

    /// The ignition intensity `0.0..=1.0` as of `now`.
    #[must_use]
    #[inline]
    pub fn intensity(&self, now: Instant) -> f32 {
        ignition_intensity(self.heat_at(now), self.params.knee_lo, self.params.knee_hi)
    }

    /// Sub-ignition warmth `0.0..=1.0` as of `now`.
    #[must_use]
    #[inline]
    pub fn warmth(&self, now: Instant) -> f32 {
        self.warmth_of(self.heat_at(now))
    }

    /// Sample both `(intensity, warmth)` at `now` in a single decay pass.
    #[must_use]
    #[inline]
    pub fn sample(&self, now: Instant) -> (f32, f32) {
        let heat = self.heat_at(now);
        (
            ignition_intensity(heat, self.params.knee_lo, self.params.knee_hi),
            self.warmth_of(heat),
        )
    }
}

#[inline]
const fn heat_color(base: u32, q: f32) -> u32 {
    const HOT: u32 = crate::cursor_comet::ICE_WHITE;
    const HEAT_COLOR_MAX: f32 = 0.5;
    let t = (q.clamp(0.0, 1.0) * HEAT_COLOR_MAX).clamp(0.0, 1.0);
    crate::effect_util::lerp_rgb(base, HOT, t)
}

/// Stamp `intensity` and `warmth` onto `cfg` and heat its colour toward ice white.
#[inline]
pub const fn ignite(cfg: &mut TrailConfig, intensity: f32, warmth: f32) {
    let q = intensity.clamp(0.0, 1.0);
    cfg.intensity = q;
    cfg.warmth = warmth.clamp(0.0, 1.0);
    cfg.color = heat_color(cfg.color, q);
}

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
fn line_cells(r0: i32, c0: i32, r1: i32, c1: i32) -> Vec<(i32, i32)> {
    let (dr, dc) = ((r1 - r0).abs(), (c1 - c0).abs());
    let (sr, sc) = (if r0 < r1 { 1 } else { -1 }, if c0 < c1 { 1 } else { -1 });
    let mut err = dc - dr;
    let (mut r, mut c) = (r0, c0);
    let mut out = Vec::with_capacity((dr.max(dc) + 1) as usize);
    loop {
        out.push((r, c));
        if r == r1 && c == c1 {
            break;
        }
        let e2 = 2 * err;
        if e2 > -dr {
            err -= dr;
            c += sc;
        }
        if e2 < dc {
            err += dc;
            r += sr;
        }
    }
    out
}

#[cfg(test)]
#[path = "cursor_trail_tests.rs"]
mod tests;
