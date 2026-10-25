// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cursor motion trail: pure animation state.

use aterm_time::Instant;
use std::time::Duration;

use aterm_render::TrailCell;

use crate::cursor_glow::{TypedStamps, band_row, hint_fresh, take_hint_fresh};

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
    const BLINK_HINT_FRESH: f32 = 0.5;
    const NAV_HINT_FRESH: f32 = 0.25;
    const MOVE_HINT_FRESH: f32 = 0.25;

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
        for hint in [&mut self.nav_hint, &mut self.move_hint] {
            if *hint == Some(at) {
                *hint = None;
            }
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
        self.last = self
            .last
            .and_then(|(row, col)| map(row).map(|row| (row, col)));
        self.last_visible = self
            .last_visible
            .and_then(|((row, col), at)| map(row).map(|row| ((row, col), at)));
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

        let spawn_from = self.last.or_else(|| {
            let ((r, c), seen) = self.last_visible?;
            let fresh = now.saturating_duration_since(seen).as_millis() as u64 <= HIDE_BRIDGE_MS;
            let typed_fresh = self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH);
            let nav_bridge = hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
                && self
                    .nav_hint
                    .is_some_and(|key| nav_bridge_admits(key, seen, now));
            let reach = hide_bridge_reach(typed_fresh, nav_bridge, self.pane_columns);
            let plausible = cur.is_some_and(|to| reach.admits((r, c), to));
            ((fresh || nav_bridge) && plausible).then_some((r, c))
        });
        let hidden_relocation = self.last.is_none()
            && cur
                .zip(self.last_visible.map(|(cell, _)| cell))
                .is_some_and(|(current, previous)| current != previous);
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
            let fresh_nav = self.nav_hint.filter(|t| {
                now.saturating_duration_since(*t).as_secs_f32() <= Self::NAV_HINT_FRESH
            });
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
                        let cached = (age_ms < s.life_ms)
                            .then(|| ((s.life_ms - age_ms) * 255 / s.life_ms) as u32);
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
        self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH)
            || hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
            || hint_fresh(self.move_hint, now, Self::MOVE_HINT_FRESH)
    }

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
        let move_paired =
            !(cr == pr && cc > pc) && hint_fresh(self.move_hint, now, Self::MOVE_HINT_FRESH);
        let paired = !move_paired
            && self
                .type_hint
                .take_fresh(now, Self::TYPE_HINT_FRESH)
                .is_some();
        let nav_paired = take_hint_fresh(&mut self.nav_hint, now, Self::NAV_HINT_FRESH).is_some();
        let _ = self.move_hint.take();
        if nav_paired {
            return false;
        }
        let dr_abs = (cr as i32 - pr as i32).abs();
        let raw_dist = dr_abs.max((cc as i32 - pc as i32).abs());
        let physical_fold = self.pane_columns.is_some_and(|(col0, cols)| {
            crate::trail_sweep::physical_margin_fold((pr, pc), (cr, cc), col0, cols)
        });
        if paired
            && !physical_fold
            && dr_abs <= 1
            && raw_dist > 2
            && (!self.ctx_alt || hint_fresh(self.blink_hint, now, Self::BLINK_HINT_FRESH))
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
        for (i, (r, c)) in self.spawn_path.iter().copied().enumerate() {
            if r < 0 || c < 0 {
                continue;
            }
            let frac = (i as u32 + 1) * 255 / n; // 1/n .. 1, scaled to 0..=255
            let born_alpha =
                (tail + (head - tail) * frac / 255).min(READABLE_ALPHA_CAP as u32) as u8;
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
fn decay_heat(heat: f32, dt: Duration, half_life: Duration) -> f32 {
    if heat <= 0.0 || dt.is_zero() {
        return heat.max(0.0);
    }
    let hl = half_life.as_secs_f32().max(1e-3);
    heat * 0.5f32.powf(dt.as_secs_f32() / hl)
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
    if t == 0.0 {
        return base & 0x00FF_FFFF;
    }
    const fn lerp(base: u32, hot: u32, t: f32, sh: u32) -> u32 {
        let a = ((base >> sh) & 0xff) as f32;
        let b = ((hot >> sh) & 0xff) as f32;
        let v = (a + (b - a) * t).round() as u32;
        if v < 255 { v } else { 255 }
    }
    (lerp(base, HOT, t, 16) << 16) | (lerp(base, HOT, t, 8) << 8) | lerp(base, HOT, t, 0)
}

/// Stamp `intensity` and `warmth` onto `cfg` and heat its colour toward ice white.
#[inline]
pub const fn ignite(cfg: &mut TrailConfig, intensity: f32, warmth: f32) {
    let q = intensity.clamp(0.0, 1.0);
    cfg.intensity = q;
    cfg.warmth = warmth.clamp(0.0, 1.0);
    cfg.color = heat_color(cfg.color, q);
}

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
mod tests {
    use super::*;

    const fn cfg(enabled: bool) -> TrailConfig {
        TrailConfig {
            enabled,
            duration: Duration::from_millis(200),
            max_len: 8,
            color: 0x0050_FA7B,
            intensity: 0.0,
            warmth: 0.0,
        }
    }

    #[test]
    fn presentation_opacity_attenuates_resident_cells_and_fingerprints_exact_output() {
        let now = Instant::now();
        let mut trail = CursorTrail::default();
        trail.sparks.push(Spark {
            row: 3,
            col: 7,
            born_alpha: 200,
            born: now,
            life_ms: 1_000,
        });
        let config = cfg(true);
        let mut full = Vec::new();
        let full_fp = trail.tick(None, now, &config, &mut full);
        assert_eq!(
            full.as_slice(),
            &[TrailCell {
                row: 3,
                col: 7,
                alpha: 200
            }]
        );
        assert_ne!(full_fp, 0, "negative control: a resident cell is emitted");

        for (opacity, expected_alpha) in [(1.0, 200), (0.75, 150), (0.5, 100), (0.25, 50)] {
            let mut presented = full.clone();
            let fp = project_trail_presentation(&mut presented, config.color, opacity);
            assert_eq!(
                presented.as_slice(),
                &[TrailCell {
                    row: 3,
                    col: 7,
                    alpha: expected_alpha,
                }],
                "the envelope must scale already-resident output at {opacity}"
            );
            assert_ne!(fp, 0);
            assert_eq!(
                fp,
                trail_presentation_fingerprint(&presented, config.color),
                "the RepaintKey witness describes the attenuated bytes"
            );
            assert_eq!(
                fp == full_fp,
                opacity == 1.0,
                "only the identity projection keeps the original fingerprint"
            );
        }

        let mut hidden = full.clone();
        assert_eq!(
            project_trail_presentation(&mut hidden, config.color, 0.0),
            0
        );
        assert!(hidden.is_empty());
        assert!(
            trail.is_active(),
            "presentation shedding must not rewrite resident engine state"
        );

        let mut non_finite = full;
        assert_eq!(
            project_trail_presentation(&mut non_finite, config.color, f32::NAN),
            0
        );
        assert!(non_finite.is_empty(), "non-finite opacity fails closed");

        let mut disabled = cfg(false);
        disabled.intensity = 1.0;
        let mut off = Vec::new();
        assert_eq!(trail.tick(None, now, &disabled, &mut off), 0);
        assert!(
            off.is_empty() && !trail.is_active(),
            "the accessibility/master hard-zero path still retires the comet"
        );
    }

    #[test]
    fn a_band_move_translates_the_trail_inside_the_band_and_drops_what_leaves() {
        let now = Instant::now();
        let mut trail = CursorTrail {
            last: Some((10, 4)),
            last_visible: Some(((10, 4), now)),
            ..CursorTrail::default()
        };
        for row in [10u16, 1, 20] {
            trail.sparks.push(Spark {
                row,
                col: 3,
                born_alpha: 42,
                born: now,
                life_ms: 500,
            });
        }
        trail.note_band_move(8, 20, 1);
        assert_eq!(
            trail.last,
            Some((11, 4)),
            "classifier anchor rides its band"
        );
        assert_eq!(
            trail.last_visible.map(|(cell, _)| cell),
            Some((11, 4)),
            "hide-bridge anchor rides its band"
        );
        let rows: Vec<u16> = trail.sparks.iter().map(|s| s.row).collect();
        assert_eq!(
            rows,
            vec![11, 1],
            "inside moved, outside untouched, past the edge gone"
        );
        trail.note_band_move(0, 5, 0);
        trail.note_band_move(9, 8, -1);
        assert_eq!(trail.last, Some((11, 4)));
        trail.note_band_move(11, 11, -1);
        assert_eq!(
            trail.last, None,
            "an anchor carried out of its band has no honest cell"
        );
        assert_eq!(trail.sparks.len(), 1);
    }

    #[test]
    fn scroll_translates_every_live_trail_cell_and_retires_off_top_state() {
        let now = Instant::now();
        let mut trail = CursorTrail {
            last: Some((10, 4)),
            last_visible: Some(((10, 4), now)),
            ..CursorTrail::default()
        };
        trail.sparks.push(Spark {
            row: 10,
            col: 3,
            born_alpha: 42,
            born: now,
            life_ms: 500,
        });
        trail.sparks.push(Spark {
            row: 1,
            col: 2,
            born_alpha: 42,
            born: now,
            life_ms: 500,
        });

        trail.note_scroll(2);
        assert_eq!(trail.last, Some((8, 4)), "classifier anchor follows");
        assert_eq!(
            trail.last_visible.map(|(cell, _)| cell),
            Some((8, 4)),
            "hide-bridge anchor follows"
        );
        assert_eq!(trail.sparks.len(), 1, "off-top light retires");
        assert_eq!(trail.sparks[0].row, 8, "surviving light follows exactly");

        let base_model = aterm_models_effects::cursor_effect_scroll_model();
        let overrides = [("StartY", 10), ("Delta", 2)];
        let model = aterm_spec::interp::with_consts(&base_model, &overrides);
        let mut source = model.init_state();
        source.insert("survivor_y", 10);
        let mut projected = source.clone();
        projected.insert("survivor_y", i64::from(trail.sparks[0].row));
        projected.insert("off_top_alive", 0);
        projected.insert("stale_proof_alive", 0);
        projected.insert("scrolled", 1);
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &base_model,
            &overrides,
            &source,
            &projected,
            Some("Scroll"),
            "CursorTrail all-family scroll translation",
        );
        assert!(admitted, "real trail scroll transition rejected: {why}");

        let mut stranded = projected.clone();
        stranded.insert("survivor_y", 10);
        let (admitted, _) = aterm_spec::verify::validate_transition_tiered(
            &base_model,
            &overrides,
            &source,
            &stranded,
            Some("Scroll"),
            "CursorTrail stranded-scroll negative control",
        );
        assert!(!admitted, "an untranslated trail cell must be rejected");
        let mut retained = projected;
        retained.insert("off_top_alive", 1);
        let (admitted, _) = aterm_spec::verify::validate_transition_tiered(
            &base_model,
            &overrides,
            &source,
            &retained,
            Some("Scroll"),
            "CursorTrail off-top-retention negative control",
        );
        assert!(!admitted, "an off-top trail cell must be rejected");

        trail.note_scroll(20);
        assert!(
            trail.sparks.is_empty(),
            "remaining trail scrolls off cleanly"
        );
        assert!(
            trail.last.is_none() && trail.last_visible.is_none(),
            "off-top anchors retire instead of forging a row-0 source"
        );
    }

    #[test]
    fn a_user_gesture_establishes_one_class_and_licenses_its_landing() {
        let now = Instant::now();
        let mut trail = CursorTrail::default();
        let mut out = Vec::new();
        let c = cfg(true);
        trail.tick(Some((2, 20)), now, &c, &mut out);
        trail.note_typed(now);
        trail.note_navigation(now);

        trail.note_user_gesture(now);
        assert!(
            !trail.type_hint.armed() && trail.nav_hint.is_none(),
            "the gesture establishes one unambiguous generic class"
        );
        let moved = now + Duration::from_millis(10);
        assert_ne!(
            trail.tick(Some((3, 3)), moved, &c, &mut out),
            0,
            "a fresh paste/Tab stamp licenses the landing that follows it"
        );
        assert!(!out.is_empty());

        let mut cold = CursorTrail::default();
        cold.tick(Some((2, 20)), now, &c, &mut out);
        assert_eq!(
            cold.tick(Some((3, 3)), moved, &c, &mut out),
            0,
            "unlicensed program motion mints nothing"
        );
        assert!(out.is_empty());

        let base_model = aterm_models_effects::cursor_hint_license_model();
        let model = aterm_spec::interp::with_consts(&base_model, &[]);
        let mut source = model.init_state();
        source.insert("hint", 1);
        source.insert("arms", 1);
        source.insert("credit_arms", 1);
        let mut projected = source.clone();
        projected.insert("hint", 1);
        projected.insert("arms", 2);
        projected.insert("credit_arms", 2);
        projected.insert(
            "superseded",
            i64::from(!trail.type_hint.armed() && trail.nav_hint.is_none()),
        );
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &base_model,
            &[],
            &source,
            &projected,
            Some("PressArmsLicense"),
            "CursorTrail gesture-class supersession",
        );
        assert!(admitted, "real trail supersession rejected: {why}");

        let mut ambiguous = projected;
        ambiguous.insert("superseded", 0);
        let (admitted, _) = aterm_spec::verify::validate_transition_tiered(
            &base_model,
            &[],
            &source,
            &ambiguous,
            Some("PressArmsLicense"),
            "CursorTrail stale-typed negative control",
        );
        assert!(!admitted, "a retained typed class must be rejected");

        for arm in [CursorTrail::note_typed, CursorTrail::note_navigation] {
            let mut stale = CursorTrail::default();
            stale.tick(Some((2, 20)), now, &c, &mut out);
            arm(&mut stale, now);
            assert_eq!(stale.tick(Some((3, 3)), moved, &c, &mut out), 0);
            assert!(out.is_empty(), "negative control must suppress the move");
        }
    }

    #[test]
    fn composed_physical_margin_fold_lights_only_departure_material() {
        let t0 = Instant::now();
        let at = t0 + Duration::from_millis(8);
        let origin = (4, 51);
        let target = (5, 12);
        let c = cfg(true);
        let mut out = Vec::new();

        let mut physical = CursorTrail::default();
        physical.note_pane_columns(12, 40);
        physical.tick(Some(origin), t0, &c, &mut out);
        physical.note_typed(at);
        assert_ne!(physical.tick(Some(target), at, &c, &mut out), 0);
        assert_eq!(
            out.iter()
                .map(|cell| (cell.row, cell.col))
                .collect::<Vec<_>>(),
            vec![(usize::from(origin.0), usize::from(origin.1))],
            "the fold lights its typed margin cell, never an interpolated diagonal"
        );

        let mut unwired = CursorTrail::default();
        unwired.tick(Some(origin), t0, &c, &mut out);
        unwired.note_typed(at);
        assert_eq!(unwired.tick(Some(target), at, &c, &mut out), 0);
        assert!(
            out.is_empty(),
            "without pane-bound authority the same large one-row move stays a dark re-anchor"
        );

        let mut wrong_pane = CursorTrail::default();
        wrong_pane.note_pane_columns(11, 40);
        wrong_pane.tick(Some(origin), t0, &c, &mut out);
        wrong_pane.note_typed(at);
        assert_eq!(wrong_pane.tick(Some(target), at, &c, &mut out), 0);
        assert!(
            out.is_empty(),
            "an adjacent pane origin cannot borrow the fold"
        );
    }

    #[test]
    fn classic_trail_requires_one_shot_correlated_candidate() {
        let now = Instant::now();
        let mut hot = cfg(true);
        hot.intensity = 1.0;
        hot.warmth = 1.0;

        let mut cold = CursorTrail::default();
        assert!(
            !cold.spawn(0, 0, 0, 1, now, &hot),
            "warm configuration is amplitude, not user provenance"
        );
        assert!(cold.sparks.is_empty());

        for arm in [
            CursorTrail::note_typed,
            CursorTrail::note_return,
            CursorTrail::note_reflow,
            CursorTrail::note_user_gesture,
        ] {
            let mut pressed = CursorTrail::default();
            arm(&mut pressed, now);
            assert!(
                pressed.spawn(0, 0, 0, 1, now + Duration::from_millis(1), &hot),
                "a fresh key stamp licenses its own echo"
            );
            assert!(!pressed.sparks.is_empty());
        }

        let mut witnessed = CursorTrail {
            last: Some((0, 0)),
            last_visible: Some(((0, 0), now)),
            ..CursorTrail::default()
        };
        witnessed.note_synthetic_move(now);
        assert!(witnessed.spawn(0, 0, 0, 1, now, &hot));
        assert!(!witnessed.sparks.is_empty());

        let mut one_shot = CursorTrail {
            last: Some((0, 0)),
            ..CursorTrail::default()
        };
        one_shot.note_synthetic_move(now);
        assert!(one_shot.spawn(0, 0, 0, 1, now, &hot));
        let earned: Vec<(u16, u16)> = one_shot.sparks.iter().map(|s| (s.row, s.col)).collect();
        assert!(
            !one_shot.spawn(0, 1, 0, 2, now + Duration::from_millis(1), &hot),
            "one candidate cannot fund a second program delta"
        );
        assert_eq!(
            one_shot
                .sparks
                .iter()
                .map(|s| (s.row, s.col))
                .collect::<Vec<_>>(),
            earned,
            "an unlicensed relocation neither adds to nor retires the earned comet bed"
        );

        let mut stale = CursorTrail {
            last: Some((0, 0)),
            ..CursorTrail::default()
        };
        stale.note_synthetic_move(now);
        assert!(
            !stale.spawn(0, 0, 0, 1, now + Duration::from_millis(251), &hot),
            "an expired stamp licenses nothing"
        );
        assert!(
            !stale.spawn(0, 0, 0, 1, now + Duration::from_millis(400), &hot),
            "an expired stamp stays inert on every later delta"
        );
        assert!(stale.sparks.is_empty());

        let mut nav = CursorTrail::default();
        nav.note_navigation(now);
        assert!(
            !nav.spawn(0, 0, 0, 1, now, &hot),
            "a scrub is licensed but classified dark — the comet is a typing effect"
        );
        assert!(nav.sparks.is_empty());

        for arm in [
            CursorTrail::note_typed,
            CursorTrail::note_navigation,
            CursorTrail::note_return,
            CursorTrail::note_user_gesture,
        ] {
            let mut hidden = CursorTrail::default();
            let mut out = Vec::new();
            hidden.tick(Some((0, 0)), now, &hot, &mut out);
            hidden.tick(None, now + Duration::from_millis(1), &hot, &mut out);
            arm(&mut hidden, now + Duration::from_millis(2));
            hidden.tick(
                Some((0, 20)),
                now + Duration::from_millis(3),
                &hot,
                &mut out,
            );
            assert!(out.is_empty(), "far reappearance declines the bridge");
            assert!(
                !hidden.type_hint.armed()
                    && hidden.nav_hint.is_none()
                    && hidden.move_hint.is_none(),
                "declined hidden relocation consumes every movement class"
            );
            hidden.tick(
                Some((0, 21)),
                now + Duration::from_millis(4),
                &hot,
                &mut out,
            );
            assert!(
                out.is_empty() && !hidden.is_active(),
                "the next cold program move cannot borrow a declined hint"
            );
        }

        for arm in [
            CursorTrail::note_typed,
            CursorTrail::note_return,
            CursorTrail::note_user_gesture,
        ] {
            let mut hidden = CursorTrail::default();
            let mut out = Vec::new();
            hidden.tick(Some((2, 4)), now, &hot, &mut out);
            hidden.tick(None, now + Duration::from_millis(1), &hot, &mut out);
            arm(&mut hidden, now + Duration::from_millis(2));
            hidden.tick(Some((2, 4)), now + Duration::from_millis(3), &hot, &mut out);
            assert!(out.is_empty(), "same-cell reappearance paints nothing");
            assert!(
                !hidden.type_hint.armed()
                    && hidden.nav_hint.is_none()
                    && hidden.move_hint.is_none(),
                "same-cell hidden completion consumes its movement class"
            );
            hidden.tick(Some((2, 5)), now + Duration::from_millis(4), &hot, &mut out);
            assert!(
                out.is_empty() && !hidden.is_active(),
                "the next cold CUP move cannot borrow a same-cell hide hint"
            );
        }

        for reset_first in [false, true] {
            for arm in [
                CursorTrail::note_typed,
                CursorTrail::note_navigation,
                CursorTrail::note_return,
                CursorTrail::note_user_gesture,
                CursorTrail::note_reflow,
            ] {
                let mut unseeded = CursorTrail::default();
                let mut out = Vec::new();
                if reset_first {
                    unseeded.tick(Some((4, 4)), now, &hot, &mut out);
                    unseeded.reset();
                }
                arm(&mut unseeded, now + Duration::from_millis(1));
                unseeded.tick(
                    Some((4, 10)),
                    now + Duration::from_millis(2),
                    &hot,
                    &mut out,
                );
                assert!(out.is_empty(), "a source-less seed paints nothing");
                assert!(
                    !unseeded.type_hint.armed()
                        && unseeded.nav_hint.is_none()
                        && unseeded.move_hint.is_none(),
                    "a source-less seed consumes every movement class"
                );
                unseeded.tick(
                    Some((4, 11)),
                    now + Duration::from_millis(3),
                    &hot,
                    &mut out,
                );
                assert!(
                    out.is_empty() && !unseeded.is_active(),
                    "the next cold CUP cannot borrow a pre-seed class"
                );
            }
        }
    }

    #[test]
    fn same_row_jump_spawns_a_bounded_comet_nearest_the_cursor() {
        let mut t = CursorTrail::default();
        let now = Instant::now();
        let mut out = Vec::new();
        let c = cfg(true);
        t.tick(Some((0, 0)), now, &c, &mut out); // seed last = (0,0)
        t.note_synthetic_move(now);
        let fp = t.tick(Some((0, 40)), now, &c, &mut out);
        assert_ne!(fp, 0);
        assert!(t.is_active());
        assert!(out.len() <= c.max_len, "comet bounded to max_len");
        assert!(out.iter().all(|t| t.row == 0 && t.col != 40));
        let head = out.iter().max_by_key(|t| t.col).unwrap();
        let tail = out.iter().min_by_key(|t| t.col).unwrap();
        assert!(head.alpha >= tail.alpha, "head brighter than tail");
        assert_eq!(head.col, 39);
    }

    #[test]
    fn fingerprint_folds_the_trail_colour() {
        let run = |color: u32| {
            let mut t = CursorTrail::default();
            let now = Instant::now();
            let mut out = Vec::new();
            let mut c = cfg(true);
            c.color = color;
            t.tick(Some((0, 0)), now, &c, &mut out);
            t.note_synthetic_move(now);
            let fp = t.tick(Some((0, 5)), now, &c, &mut out);
            (fp, out.clone())
        };
        let (fp_green, cells_green) = run(0x0050_FA7B);
        let (fp_amber, cells_amber) = run(0x00FF_B020);
        assert_ne!(fp_green, 0);
        assert_ne!(fp_amber, 0);
        assert_ne!(
            fp_green, fp_amber,
            "identical cells, different colour ⇒ different fp"
        );
        assert_eq!(cells_green.len(), cells_amber.len());
        let mut t = CursorTrail::default();
        let mut out = Vec::new();
        let mut c = cfg(true);
        c.color = 0x00FF_B020;
        assert_eq!(t.tick(Some((0, 0)), Instant::now(), &c, &mut out), 0);
    }

    #[test]
    fn comet_fades_to_empty_after_the_duration() {
        let mut t = CursorTrail::default();
        let t0 = Instant::now();
        let mut out = Vec::new();
        let c = cfg(true);
        t.tick(Some((0, 0)), t0, &c, &mut out);
        t.note_synthetic_move(t0);
        t.tick(Some((0, 5)), t0, &c, &mut out);
        assert!(t.is_active());
        let mid = t.tick(Some((0, 5)), t0 + Duration::from_millis(100), &c, &mut out);
        assert_ne!(mid, 0);
        let gone = t.tick(Some((0, 5)), t0 + Duration::from_millis(250), &c, &mut out);
        assert_eq!(gone, 0);
        assert!(out.is_empty());
        assert!(!t.is_active());
    }

    #[test]
    fn warmth_is_zero_for_a_lone_key_and_ramps_with_cadence() {
        let mut cad = TypingCadence::default();
        let t0 = Instant::now();
        cad.on_keystroke(t0);
        assert_eq!(cad.warmth(t0), 0.0, "first key of a burst contributes 0");
        let mut t = t0;
        for _ in 0..10 {
            t += Duration::from_millis(300);
            cad.on_keystroke(t);
        }
        let w = cad.warmth(t);
        assert!(w > 0.4, "300ms-cadence warmth converges high (got {w})");
        assert!(
            cad.intensity(t) < 0.1,
            "300ms cadence must not ignite (got {})",
            cad.intensity(t)
        );
        assert!(
            cad.warmth(t + Duration::from_millis(1500)) < 0.1,
            "warmth decays after a pause"
        );
    }

    #[test]
    fn warm_chain_outlives_the_gap() {
        let mut t = CursorTrail::default();
        let t0 = Instant::now();
        let mut out = Vec::new();
        let c = TrailConfig {
            warmth: 0.5,
            ..cfg(true)
        };
        t.tick(Some((0, 0)), t0, &c, &mut out);
        t.note_synthetic_typed(t0);
        t.tick(Some((0, 1)), t0, &c, &mut out); // typing advance, warmth 0.5
        let alive = t.tick(Some((0, 1)), t0 + Duration::from_millis(340), &c, &mut out);
        assert_ne!(alive, 0, "warm ember chains across a 300ms+ inter-key gap");
        assert!(t.is_active());
        let gone = t.tick(Some((0, 1)), t0 + Duration::from_millis(400), &c, &mut out);
        assert_eq!(gone, 0);
        assert!(!t.is_active());
    }

    #[test]
    fn hinted_ink_wrap_lays_no_comet_but_typing_still_trails() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut out = Vec::new();

        let mut t = CursorTrail::default();
        t.tick(Some((0, 30)), t0, &c, &mut out);
        t.note_synthetic_typed(t0 + Duration::from_millis(5));
        let fp = t.tick(Some((1, 3)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_eq!(fp, 0, "a typed re-anchor sweeps no comet");
        assert!(out.is_empty());
        assert!(!t.is_active());

        let mut generic = CursorTrail::default();
        generic.tick(Some((0, 30)), t0, &c, &mut out);
        generic.note_synthetic_move(t0 + Duration::from_millis(5));
        let fp = generic.tick(Some((1, 3)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_ne!(fp, 0, "a candidate-backed generic jump keeps its comet");
        assert!(!out.is_empty());

        let mut typing = CursorTrail::default();
        typing.tick(Some((0, 3)), t0, &c, &mut out);
        typing.note_synthetic_typed(t0 + Duration::from_millis(5));
        let fp = typing.tick(Some((0, 4)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_ne!(fp, 0, "candidate-backed typing still trails");
        assert!(!out.is_empty());
    }

    #[test]
    fn a_glyph_typed_before_the_enters_response_lands_keeps_its_stamp_for_its_own_echo() {
        let c = cfg(true);
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut out = Vec::new();
        let mut trail = CursorTrail::default();
        trail.tick(Some((2, 20)), t0, &c, &mut out);
        let enter = t0 + ms(100);
        trail.note_return(enter);
        let k = enter + ms(120);
        trail.note_typed(k);
        assert_eq!(
            trail.move_hint,
            Some(enter),
            "the glyph keeps the Enter's licence: its response is still owed"
        );
        let resp = k + ms(30);
        let fp = trail.tick(Some((3, 2)), resp, &c, &mut out);
        assert_ne!(fp, 0, "the Enter's response is licensed");
        assert!(trail.move_hint.is_none(), "…and spends the Enter's licence");
        assert!(
            trail
                .type_hint
                .any_fresh(resp, CursorTrail::TYPE_HINT_FRESH),
            "…leaving the glyph's stamp for the glyph's own echo"
        );
        let echo = resp + ms(20);
        let fp = trail.tick(Some((3, 3)), echo, &c, &mut out);
        assert_ne!(fp, 0, "the glyph's +1 is licensed by its own stamp");
        assert!(
            out.iter().any(|cell| cell.row == 3 && cell.col == 2),
            "…and lights its cell: {:?}",
            out.iter()
                .map(|cell| (cell.row, cell.col))
                .collect::<Vec<_>>()
        );
        assert!(
            !trail
                .type_hint
                .any_fresh(echo, CursorTrail::TYPE_HINT_FRESH),
            "one stamp, one echo"
        );
        assert!(!trail.move_licensed(echo + ms(50)));

        let mut bare = CursorTrail::default();
        bare.tick(Some((2, 20)), t0, &c, &mut out);
        bare.note_return(enter);
        let mut bare_out = Vec::new();
        let bare_fp = bare.tick(Some((3, 2)), resp, &c, &mut bare_out);
        let mut with_glyph = CursorTrail::default();
        with_glyph.tick(Some((2, 20)), t0, &c, &mut out);
        with_glyph.note_return(enter);
        with_glyph.note_typed(k);
        let mut glyph_out = Vec::new();
        let glyph_fp = with_glyph.tick(Some((3, 2)), resp, &c, &mut glyph_out);
        assert_eq!(
            (bare_fp, bare_out),
            (glyph_fp, glyph_out),
            "the Enter's response draws the same with a type-ahead glyph behind it"
        );
    }

    #[test]
    fn alt_without_blink_keeps_the_jump_comet() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut out = Vec::new();

        let mut vim = CursorTrail::default();
        vim.note_context(true);
        vim.tick(Some((0, 30)), t0, &c, &mut out);
        vim.note_synthetic_typed(t0 + Duration::from_millis(5));
        vim.note_context(true);
        let fp = vim.tick(Some((1, 3)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_ne!(
            fp, 0,
            "alt + typed candidate + NO blink: the jump keeps its comet"
        );
        assert!(!out.is_empty());

        let mut cc = CursorTrail::default();
        cc.note_context(true);
        cc.tick(Some((0, 30)), t0, &c, &mut out);
        cc.note_synthetic_typed(t0 + Duration::from_millis(5));
        cc.note_repaint_blink(t0 + Duration::from_millis(8));
        cc.note_context(true);
        let fp = cc.tick(Some((1, 3)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_eq!(
            fp, 0,
            "alt + hint + fresh blink: the re-anchor lays no comet"
        );
        assert!(out.is_empty());
        assert!(!cc.is_active());
    }

    #[test]
    fn bounded_tail_matches_the_legacy_full_sweep() {
        for (origin, destination) in [
            ((0, 0), (0, 40)),
            ((0, 40), (0, 0)),
            ((1, 2), (37, 9)),
            ((37, 9), (1, 2)),
            ((3, 4), (31, 45)),
            ((31, 45), (3, 4)),
        ] {
            for limit in 1..=12 {
                let mut legacy = line_cells(origin.0, origin.1, destination.0, destination.1);
                legacy.pop();
                if legacy.len() > limit {
                    legacy.drain(..legacy.len() - limit);
                }
                let mut bounded = Vec::new();
                crate::trail_sweep::line_cells_tail(
                    &mut bounded,
                    origin,
                    destination,
                    limit,
                    false,
                );
                assert_eq!(bounded, legacy, "{origin:?}->{destination:?}, {limit}");
            }
        }
    }

    #[test]
    fn full_u16_jump_reuses_bounded_spawn_scratch() {
        let now = Instant::now();
        let c = cfg(true);
        let mut trail = CursorTrail::default();
        let mut out = Vec::new();
        trail.tick(Some((0, 0)), now, &c, &mut out);
        trail.note_synthetic_move(now);
        trail.tick(
            Some((0, u16::MAX)),
            now + Duration::from_millis(1),
            &c,
            &mut out,
        );
        assert_eq!(trail.spawn_path.len(), c.max_len);
        let (ptr, capacity) = (trail.spawn_path.as_ptr(), trail.spawn_path.capacity());

        trail.note_synthetic_move(now + Duration::from_millis(2));
        trail.tick(Some((0, 0)), now + Duration::from_millis(3), &c, &mut out);
        assert_eq!(trail.spawn_path.len(), c.max_len);
        assert_eq!(trail.spawn_path.capacity(), capacity);
        assert_eq!(
            trail.spawn_path.as_ptr(),
            ptr,
            "warm move does not allocate"
        );
    }

    const fn cfg_q(q: f32) -> TrailConfig {
        TrailConfig {
            intensity: q,
            ..cfg(true)
        }
    }

    fn burst(keys: u32, interval: Duration) -> f32 {
        let mut cad = TypingCadence::default();
        let t0 = Instant::now();
        let mut t = t0;
        for _ in 0..keys {
            cad.on_keystroke(t);
            t += interval;
        }
        cad.intensity(t - interval)
    }

    #[test]
    fn a_few_keys_or_slow_typing_stay_gentle() {
        for (keys, gap_ms, below) in [
            (1u32, 50u64, 1e-6f32),
            (2, 50, 0.05),
            (3, 50, 0.15),
            (24, 400, 0.1),
        ] {
            let got = burst(keys, Duration::from_millis(gap_ms));
            assert!(
                got < below,
                "{keys} keys at {gap_ms} ms must stay under {below}, got {got}"
            );
        }
    }

    #[test]
    fn a_pause_decays_the_ignition() {
        let mut cad = TypingCadence::default();
        let t0 = Instant::now();
        let mut t = t0;
        for _ in 0..16 {
            cad.on_keystroke(t);
            t += Duration::from_millis(40);
        }
        let peak = cad.intensity(t);
        assert!(peak > 0.85, "burst ignited (got {peak})");
        assert!(
            cad.intensity(t + Duration::from_millis(600)) < 0.05,
            "cooled after a pause"
        );
        assert_eq!(cad.intensity(t + Duration::from_secs(10)), 0.0);
    }

    #[test]
    fn sample_matches_the_separate_reads_bit_for_bit() {
        fn check(cad: &TypingCadence, now: Instant, tag: &str) {
            let (i, w) = cad.sample(now);
            assert_eq!(
                i.to_bits(),
                cad.intensity(now).to_bits(),
                "{tag}: intensity"
            );
            assert_eq!(w.to_bits(), cad.warmth(now).to_bits(), "{tag}: warmth");
        }
        let t0 = Instant::now();
        let mut cad = TypingCadence::default();
        check(&cad, t0, "cold, untouched tracker");
        let mut t = t0;
        for k in 0..16 {
            cad.on_keystroke(t);
            check(&cad, t, &format!("burst key {k}"));
            t += Duration::from_millis(40);
        }
        for ms in [1u64, 100, 300, 600, 5000] {
            check(
                &cad,
                t + Duration::from_millis(ms),
                &format!("decay +{ms} ms"),
            );
        }
    }

    #[test]
    fn readability_cap_bounds_every_cell_at_any_intensity() {
        for &q in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let mut tr = CursorTrail::default();
            let now = Instant::now();
            let mut out = Vec::new();
            let c = cfg_q(q);
            tr.tick(Some((0, 0)), now, &c, &mut out); // seed
            tr.note_synthetic_move(now);
            tr.tick(Some((0, 40)), now, &c, &mut out); // long jump → many cells
            assert!(!out.is_empty());
            for cell in &out {
                assert!(
                    cell.alpha <= READABLE_ALPHA_CAP,
                    "cell alpha {} exceeds readable cap {} at intensity {q}",
                    cell.alpha,
                    READABLE_ALPHA_CAP
                );
            }
        }
    }

    #[test]
    fn ignited_comet_is_brighter_and_longer_lived_than_gentle() {
        let seed_and_jump = |q: f32| -> (u8, bool) {
            let mut tr = CursorTrail::default();
            let now = Instant::now();
            let mut out = Vec::new();
            let c = cfg_q(q);
            tr.tick(Some((0, 0)), now, &c, &mut out);
            tr.note_synthetic_move(now);
            tr.tick(Some((0, 5)), now, &c, &mut out); // same-row jump
            let head = out.iter().map(|t| t.alpha).max().unwrap();
            let alive = tr.tick(Some((0, 5)), now + Duration::from_millis(250), &c, &mut out);
            (head, alive != 0)
        };
        let (gentle_head, gentle_alive) = seed_and_jump(0.0);
        let (hot_head, hot_alive) = seed_and_jump(1.0);
        assert!(
            hot_head > gentle_head,
            "ignited core brighter ({hot_head} > {gentle_head})"
        );
        assert!(hot_head <= READABLE_ALPHA_CAP, "…but still readable");
        assert!(!gentle_alive, "gentle comet faded by 250 ms");
        assert!(
            hot_alive,
            "ignited comet still streaming at 250 ms (longer tail)"
        );
    }

    #[test]
    fn ignite_stamps_intensity_and_heats_colour() {
        let mut c = cfg(true);
        let base = c.color;
        ignite(&mut c, 0.0, 0.0);
        assert_eq!(c.intensity, 0.0);
        assert_eq!(c.warmth, 0.0);
        assert_eq!(c.color, base, "gentle keeps the base colour");
        let mut c = cfg(true);
        ignite(&mut c, 2.0, 2.0); // out-of-range clamps to 1.0
        assert_eq!(c.intensity, 1.0);
        assert_eq!(c.warmth, 1.0);
        assert_ne!(c.color, base, "ignited colour is heated");
        let ice = crate::cursor_comet::ICE_WHITE;
        let gap = |rgb: u32, sh: u32| ((rgb >> sh & 0xff) as i32 - (ice >> sh & 0xff) as i32).abs();
        for sh in [16, 8, 0] {
            assert!(
                gap(c.color, sh) <= gap(base, sh),
                "channel >>{sh} moved away from the ice white: {:06x} from {base:06x}",
                c.color
            );
        }
    }

    #[test]
    fn dedup_index_matches_the_linear_scan_byte_for_byte() {
        let c = TrailConfig {
            enabled: true,
            duration: Duration::from_millis(260),
            max_len: 24,
            color: 0x0050_FA7B,
            intensity: 1.0,
            warmth: 0.0,
        };
        let mut trail = CursorTrail::default();
        let mut out = Vec::new();
        let mut now = Instant::now();
        for f in 0u64..420 {
            now += Duration::from_millis(16);
            let cur = if f < 180 {
                (((f / 10) % 60) as u16, ((f % 10) * 24) as u16)
            } else if f < 360 {
                (4, if f.is_multiple_of(2) { 5 } else { 205 })
            } else {
                (4, 205)
            };
            if trail.cursor_anchor().is_some_and(|anchor| anchor != cur) {
                trail.note_synthetic_move(now);
            }
            let fp = trail.tick(Some(cur), now, &c, &mut out);
            let mut want: Vec<TrailCell> = Vec::new();
            for s in &trail.sparks {
                let life_ms = s.life_ms.max(1);
                let age_ms = now.saturating_duration_since(s.born).as_millis() as u64;
                let tf = ((life_ms - age_ms) * 255 / life_ms) as u32;
                let alpha = ((s.born_alpha as u32 * tf) / 255) as u8;
                if alpha == 0 {
                    continue;
                }
                match want
                    .iter_mut()
                    .find(|t| t.row == s.row as usize && t.col == s.col as usize)
                {
                    Some(existing) => existing.alpha = existing.alpha.max(alpha),
                    None => want.push(TrailCell {
                        row: s.row as usize,
                        col: s.col as usize,
                        alpha,
                    }),
                }
            }
            assert_eq!(out, want, "frame {f}: emitted cells diverged");
            assert_eq!(fp == 0, out.is_empty(), "frame {f}: fp/emptiness law");
        }
    }

    #[test]
    fn the_nav_bridge_reaches_one_pane_across_and_a_wrapped_box_down() {
        let pane = Some((0u16, 100u16));
        let ball = |d| BridgeReach {
            cols: d,
            rows: d,
            line_start: None,
        };
        assert_eq!(
            hide_bridge_reach(false, false, pane),
            ball(2),
            "an unhinted reappear keeps the pinned classic ball"
        );
        assert_eq!(
            hide_bridge_reach(true, false, pane),
            ball(8),
            "a typed echo keeps the pinned batched-echo ball"
        );
        let nav = hide_bridge_reach(false, true, pane);
        assert_eq!(
            nav,
            BridgeReach {
                cols: 100,
                rows: HIDE_BRIDGE_NAV_MAX_ROWS,
                line_start: Some(0),
            },
            "a nav hint reaches one pane across and a wrapped box down"
        );
        let from = (4u16, 42u16);
        assert!(
            nav.admits(from, (2, 1)),
            "Ctrl-A: up two rows to the box's start"
        );
        assert!(nav.admits(from, (3, 90)), "Option+Left wrapping up one row");
        assert!(
            nav.admits(from, (5, 3)),
            "Option+Right wrapping down one row"
        );
        assert!(nav.admits(from, (5, 42)), "Down: one row, the same column");
        assert!(nav.admits(from, (4, 1)), "Ctrl-A along the caret's own row");
        assert!(
            !nav.admits(from, (11, 3)),
            "the measured stray, seven rows down"
        );
        assert!(
            !nav.admits(from, (6, 90)),
            "two rows DOWN to an arbitrary column: a re-laid input box, or End \
             — the two are one shape, and neither is bridged"
        );
        assert!(
            !nav.admits(from, (8, 3)),
            "four rows DOWN to the box's start is not Home either (Home goes up)"
        );
        assert!(
            !nav.admits(from, (2, 42)),
            "two rows UP at the same column: a box that moved, not a line gesture"
        );
        assert!(
            !nav.admits(from, (2, 30)),
            "two rows up to an interior column is not Home"
        );
        assert!(hide_bridge_reach(true, false, pane).admits(from, (6, 44)));
        assert_eq!(hide_bridge_reach(false, true, Some((0, 40))).cols, 40);
        let right = hide_bridge_reach(false, true, Some((50, 50)));
        assert!(
            right.admits((4, 80), (2, 52)),
            "Home in the right-hand pane"
        );
        assert!(
            !right.admits((4, 80), (2, 2)),
            "…and column 2 is the LEFT pane's start, not this pane's"
        );
        assert_eq!(
            hide_bridge_reach(false, true, None),
            BridgeReach {
                cols: HIDE_BRIDGE_NAV_UNPANED_COLS,
                rows: HIDE_BRIDGE_NAV_MAX_ROWS,
                line_start: Some(0),
            }
        );
        assert_eq!(
            hide_bridge_reach(false, true, Some((0, 1))).cols,
            HIDE_BRIDGE_TYPED_MAX_DIST
        );
    }

    #[test]
    fn a_caret_seen_still_at_rest_after_the_press_proves_the_key_moved_nothing() {
        let key = Instant::now();
        let ms = Duration::from_millis;
        assert!(
            nav_bridge_source_ok(key, key - ms(HIDE_BRIDGE_MS)),
            "one hide window before the press is the cell it moved from"
        );
        assert!(
            !nav_bridge_source_ok(key, key - ms(HIDE_BRIDGE_MS + 1)),
            "older than that belongs to an earlier state"
        );
        assert!(
            nav_bridge_source_ok(key, key + ms(NAV_NOOP_SETTLE_MS)),
            "a sighting inside the echo window is the key's own in-flight echo"
        );
        assert!(
            !nav_bridge_source_ok(key, key + ms(NAV_NOOP_SETTLE_MS + 1)),
            "a caret still at rest past the echo window: the key moved nothing"
        );
        let seen = key - ms(20);
        assert!(nav_bridge_admits(
            key,
            seen,
            key + ms(NAV_BRIDGE_LANDING_MS)
        ));
        assert!(!nav_bridge_admits(
            key,
            seen,
            key + ms(NAV_BRIDGE_LANDING_MS + 1)
        ));
        assert!(
            !nav_bridge_admits(key, key + ms(NAV_NOOP_SETTLE_MS + 1), key + ms(100)),
            "the source bound still applies inside the landing window"
        );
    }
}
