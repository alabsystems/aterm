// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cursor MOTION TRAIL — the "streaming trailer" effect — pure animation state.
//!
//! A cursor move (a single keystroke advance, or a big Ctrl-A/Ctrl-E jump) spawns
//! a comet of fading [`TrailCell`]s along the path the cursor swept, brightest at
//! the head (nearest the new cursor position) and dimming toward the tail. The
//! whole comet fades to nothing within a few hundred milliseconds, so motion is
//! unmistakable — and an idle screen returns to zero work (no live sparks → no
//! animation timer; see the scheduler in `main.rs`).
//!
//! This module owns only the *math*: it takes the observed cursor cell + the wall
//! clock each frame and produces the current trail cells (in TERMINAL coordinates
//! — the host splices/offsets them into window space exactly as it does the
//! cursor). It reads the clock only through an injected [`Instant`], so the full
//! fade is unit-testable without sleeping. The renderer is a pure consumer of the
//! resulting [`TrailCell`]s and never sees this state.

use std::time::Duration;
use web_time::Instant;

use aterm_render::TrailCell;

/// Coverage of the comet HEAD (the cell nearest the cursor) at a GENTLE move — the
/// "just a few keys / slow typing" baseline the user asked to keep subtle.
/// Deliberately well below the ignited head so a lone keystroke barely tints the
/// glyph it lands on (the freshest character stays crisply readable).
/// The whole GENTLE→IGNITED ladder was scaled ~×0.52 (80/125/22/85 → 42/70/12/44)
/// when the comet gained the continuous AA BEAM: the beam now carries the visible
/// streak, so the cell bed is a uniformly fainter ember layer — same contrast
/// ratios, lower ceiling ([`READABLE_ALPHA_CAP`]).
const HEAD_ALPHA_GENTLE: u32 = 42;
/// Coverage of the comet HEAD at FULL ignition (fast, sustained typing) — brighter
/// for the hot core, but held under [`READABLE_ALPHA_CAP`] so even mid-burst the
/// just-typed glyph the head lands on is never washed out. The "ignite" drama comes
/// from tail LENGTH, HOT COLOUR, the continuous BEAM, and the GPU bloom halo
/// (additive light AROUND the comet, which never obscures the text) — NOT from
/// blasting opacity over glyphs.
const HEAD_ALPHA_IGNITED: u32 = 70;
/// Coverage of the comet TAIL (the cell farthest from the cursor) at a GENTLE move
/// — faint, so a lone jump reads as a directional whisper rather than a solid bar.
const TAIL_ALPHA_GENTLE: u32 = 12;
/// Coverage of the comet TAIL at FULL ignition — a brighter streak trailing BEHIND
/// the cursor (older, already-read cells), so an ignited comet reads long and hot
/// without piling opacity onto the freshest glyph.
const TAIL_ALPHA_IGNITED: u32 = 44;
/// READABILITY GUARD (non-negotiable): the hard ceiling on any swept cell's
/// coverage, at ANY ignition level. The trail is a background tint the glyph is
/// drawn ON TOP of, so bounding coverage keeps the tint at most ~27% — the foreground
/// text always retains enough background contrast to stay legible while it is being
/// typed, even when a fast burst covers a whole run of freshly-typed cells at once.
/// Pinned by a unit test: no produced cell ever exceeds this, for any intensity.
/// Lowered 135 -> 70 when the comet gained the continuous anti-aliased BEAM
/// (`style_has_beam` now includes `comet`): the beam supplies the visible streak,
/// so the grid-quantized cell body becomes a faint EMBER BED under it instead of
/// dominant blocks — the "gappy blocks" look, retired.
const READABLE_ALPHA_CAP: u8 = 70;
/// At full ignition a spark lives this many times longer than the configured fade,
/// so a fast sustained burst leaves a LONGER visible comet (more overlapping live
/// sparks) while a lone keystroke fades in the base time.
const IGNITED_LIFE_MULT: f32 = 1.6;
/// TYPING CONTINUITY: at full sub-ignition WARMTH (see [`TypingCadence::warmth`])
/// a spark lives up to this many EXTRA base-durations, so consecutive keystrokes
/// at human cadence (~150-350ms apart) chain into a continuous ember bed — at
/// 350ms cadence the converged life is ~454ms > the gap, at 250ms ~586ms. Warmth
/// is 0 for a lone keystroke (heat ≤ one key's gain), so a single key keeps the
/// exact classic fade.
const CHAIN_LIFE_MULT: f32 = 1.5;

/// ConPTY HIDE-BRIDGE window (shared by the trail and glow engines): a cursor
/// that reappears within this long of last being VISIBLE is treated as having
/// moved from the last visible cell — Windows' conhost hides the cursor for
/// ~20-35ms during EVERY keystroke-echo repaint, and dropping those moves (the
/// plain hide/show guard) deletes the spark for essentially every key at human
/// typing cadence. 150ms comfortably bridges echo choreography while staying
/// well under deliberate hides (alt-screen apps park the cursor hidden for as
/// long as they draw).
pub const HIDE_BRIDGE_MS: u64 = 150;
/// The bridge applies only when the reappear position is a PLAUSIBLE TYPING move
/// from the last visible cell (Chebyshev distance ≤ this): echo advances,
/// backspaces, and wrap-adjacent steps bridge; a far teleport mid-hide (a
/// full-screen repaint parking the cursor elsewhere) stays suppressed — exactly
/// the phantom-comet case the hide/show guard exists for.
pub const HIDE_BRIDGE_MAX_DIST: u16 = 2;
/// The bridge's WIDER reach while a typed/backspace key-hint is fresh: a real
/// keystroke caused the hidden move, so batched echoes (PTY coalescing under
/// fast typing / key-repeat) that hop several cells inside one hide window
/// still spawn their wake instead of silently dropping it — the "trail gap on
/// every fast burst" hole. Unhinted moves keep [`HIDE_BRIDGE_MAX_DIST`]
/// byte-identically (the pinned bridge law): an app repositioning its cursor
/// mid-repaint without input still spawns nothing.
pub const HIDE_BRIDGE_TYPED_MAX_DIST: u16 = 8;

/// The bridge reach for one reappear: widened while a typed/backspace echo
/// hint is fresh, classic otherwise. The single decision both the trail and
/// glow engines share (they must agree, or the opaque bed and the additive
/// light desynchronize on the same move).
#[must_use]
pub fn hide_bridge_reach(typed_hint_fresh: bool) -> u16 {
    if typed_hint_fresh {
        HIDE_BRIDGE_TYPED_MAX_DIST
    } else {
        HIDE_BRIDGE_MAX_DIST
    }
}

/// Integer linear interpolation `a + (b-a)*q` for `q` in `0..=1` (endpoints
/// inclusive). `a`/`b` are small coverage values, so `f32` is exact enough.
fn lerp_u32(a: u32, b: u32, q: f32) -> u32 {
    let q = q.clamp(0.0, 1.0);
    (a as f32 + (b as f32 - a as f32) * q).round() as u32
}

/// Resolved tunables for the trail effect. `Copy` so the host can read it out of
/// `self` before borrowing per-window state.
#[derive(Clone, Copy, Debug)]
pub struct TrailConfig {
    /// Master on/off (config `cursor_trail`, default on).
    pub enabled: bool,
    /// How long a swept cell takes to fade fully out (config `cursor_trail_ms`).
    pub duration: Duration,
    /// Maximum comet length in cells: on a long jump only the brightest `max_len`
    /// cells nearest the cursor are kept (config `cursor_trail_length`).
    pub max_len: usize,
    /// Fully-resolved trail colour `0x00RRGGBB` (config `cursor_trail_color`, else
    /// the theme cursor colour). The host heats this toward the comet-core tint as
    /// `intensity` rises (see [`ignite`]).
    pub color: u32,
    /// IGNITION intensity in `0.0..=1.0`, driven by the host's typing cadence (see
    /// [`TypingCadence`]): `0.0` = a few keys / slow typing → a gentle, short, dim
    /// wake; `1.0` = fast, sustained typing → the comet IGNITES (brighter capped
    /// core, longer hotter tail). Every comet bakes the intensity at spawn, so an
    /// in-flight comet keeps the heat it was born with as the cadence cools.
    pub intensity: f32,
    /// Sub-ignition WARMTH in `0.0..=1.0` ([`TypingCadence::warmth`]): continuity
    /// fuel that stretches spark LIFETIMES (never colour/alpha) so keystrokes at
    /// human cadence chain into a continuous ember bed instead of per-key pulses.
    /// `0.0` for a lone keystroke (byte-identical to the classic fade).
    pub warmth: f32,
}

/// One swept cell, fading from `born`.
#[derive(Clone, Copy)]
struct Spark {
    row: u16,
    col: u16,
    /// Coverage at birth (head bright, tail faint), scaled by time as it fades.
    born_alpha: u8,
    born: Instant,
    /// How long THIS spark takes to fade fully out. Baked from the config fade
    /// time and the ignition intensity at spawn (an ignited spark lives longer, so
    /// a fast burst leaves a longer visible comet), so the decay is per-spark.
    life_ms: u64,
}

/// Per-window trail animation state.
#[derive(Default)]
pub struct CursorTrail {
    /// Live sparks, oldest first. Bounded by [`Self::MAX_SPARKS`] and by decay.
    sparks: Vec<Spark>,
    /// Last observed cursor cell (terminal coords), to detect a move. `None` when
    /// the cursor was hidden, so a hide/show gap never spawns a spurious comet.
    last: Option<(u16, u16)>,
    /// The last VISIBLE cursor cell + when it was last seen — the ConPTY
    /// hide-bridge (see [`HIDE_BRIDGE_MS`]): Windows' conhost HIDES the cursor
    /// during every keystroke-echo repaint (hide → move → show, ~20-35ms), and at
    /// human typing cadence that hidden state is always presented, so the plain
    /// hide/show guard above would drop the spark for essentially EVERY key —
    /// the "gaps in the back of the trail" report (macOS passes the raw stream,
    /// no hide, no gap). When the cursor reappears within the bridge window at a
    /// PLAUSIBLE TYPING DISTANCE (≤ [`HIDE_BRIDGE_MAX_DIST`]), the move spawns
    /// from here as if never hidden; a long hide or a far teleport (full-screen
    /// app repaints, alt-screen) stays suppressed exactly as before.
    last_visible: Option<((u16, u16), Instant)>,
    /// A fresh typed-echo key-hint ([`Self::note_typed`]) — the trail twin of
    /// `CursorGlow`'s typed/backspace hints: armed by the host on a PLAIN
    /// Character/Space/Backspace echo key (never Enter/Tab/nav/modified
    /// chords), it classifies a paired one-row move beyond the typed advance
    /// as a TUI repaint RE-ANCHOR (Claude Code's Ink prompt rewraps its whole
    /// inset input box per keystroke) — the caret never swept those cells, so
    /// `spawn` lays NO comet across them. Hosts that never arm it are
    /// byte-identical.
    type_hint: Option<Instant>,
    /// Fresh-navigation veto twin of `CursorGlow::nav_hint` (armed from the
    /// same host site): a deliberate Ctrl-A/E leap must never re-anchor even
    /// when a typed hint + blink are live (adversarial review — the two
    /// engines must agree on a nav jump).
    nav_hint: Option<Instant>,
    /// A fresh REPAINT-BLINK hint ([`Self::note_repaint_blink`]) — the trail
    /// twin of `CursorGlow`'s: the host saw the focused terminal's
    /// hide-inside-DEC-2026 repaint bracket (its `repaint_blink_epoch`
    /// advanced). On the alt screen ([`Self::ctx_alt`]) the re-anchor
    /// additionally requires it fresh, so vim/less (never blink) keep their
    /// hinted one-row jumps' comets while Claude Code still re-anchors.
    blink_hint: Option<Instant>,
    /// Per-frame ALT-SCREEN context ([`Self::note_context`], stamped by the
    /// host each frame beside the glow's). `false` (main screen / unwired
    /// host) keeps the re-anchor classifier byte-identical.
    ctx_alt: bool,
}

impl CursorTrail {
    /// Hard cap on live sparks (defends against a pathological flood of moves):
    /// far more than a few hundred ms of typing or one capped jump can produce.
    const MAX_SPARKS: usize = 512;
    /// A typed-echo key-hint stays armed this long for its paired move
    /// (seconds) — mirrors `CursorGlow::TYPE_HINT_FRESH`.
    const TYPE_HINT_FRESH: f32 = 0.25;
    /// A repaint-blink hint stays fresh this long (seconds) — mirrors
    /// `CursorGlow::BLINK_HINT_FRESH` (the blink is noted at the PREVIOUS
    /// burst's present, so it must outlive one human inter-key gap).
    const BLINK_HINT_FRESH: f32 = 0.5;
    /// A navigation key-hint stays armed this long for its paired move
    /// (seconds) — mirrors `CursorGlow::NAV_HINT_FRESH`.
    const NAV_HINT_FRESH: f32 = 0.25;

    /// HOST KEY-HINT: one PLAIN typed-glyph or Backspace echo keypress (never
    /// Enter/Tab/nav/modified chords — their jumps keep the deliberate comet).
    /// Arms the [`Self::type_hint`] re-anchor classifier; see the field doc.
    pub fn note_typed(&mut self, now: Instant) {
        self.type_hint = Some(now);
    }

    /// HOST REPAINT-BLINK (see `CursorGlow::note_repaint_blink`): the focused
    /// terminal's `repaint_blink_epoch` advanced — the attached app brackets
    /// its redraws in DECTCEM-hide-inside-DEC-2026 (Claude Code, per
    /// keystroke). Licenses the alt-screen re-anchor; never gates bytes.
    pub fn note_repaint_blink(&mut self, now: Instant) {
        self.blink_hint = Some(now);
    }

    /// Drop the blink hint — a tab/pane switch re-pointed this window at a
    /// DIFFERENT terminal, and a blink carried from the old one must not
    /// license a re-anchor (or, host-side, the probe) against the new one.
    pub fn clear_blink(&mut self) {
        self.blink_hint = None;
    }

    /// HOST PER-FRAME CONTEXT (see `CursorGlow::note_context`): whether the
    /// probed terminal is on the ALT screen this frame. `false` keeps the
    /// re-anchor classifier byte-identical to the pre-context behavior.
    pub fn note_context(&mut self, alt: bool) {
        self.ctx_alt = alt;
    }

    /// DISARM a dangling typed hint (see `CursorGlow::clear_typed`): a key
    /// with its own move semantics (plain Enter, nav, kills) must not have its
    /// jump re-anchored on a stale license from a no-move echo.
    pub fn clear_typed(&mut self) {
        self.type_hint = None;
    }

    /// Arm the navigation veto — see `CursorGlow::note_navigation`.
    pub fn note_navigation(&mut self, now: Instant) {
        self.nav_hint = Some(now);
    }

    /// PTY scroll translation — see `CursorGlow::note_scroll`: the previous
    /// caret cell now sits `rows` higher, so the next move classifies against
    /// where it actually is (a bottom-anchored TUI's box-growing wrap
    /// otherwise flattens to dr==0 and sweeps the trail across the line).
    pub fn note_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        if let Some((r, c)) = self.last {
            self.last = Some((r.saturating_sub(rows), c));
        }
    }

    /// Whether any spark is still alive (so the host should keep the animation
    /// timer armed). Cheap; consulted every `about_to_wait`.
    pub fn is_active(&self) -> bool {
        !self.sparks.is_empty()
    }

    /// Drop the in-flight comet and forget the last cursor position (the trail twin
    /// of [`crate::cursor_glow::CursorGlow::reset`]) — used on a layout transition
    /// that changes the cursor's coordinate space.
    pub fn reset(&mut self) {
        self.sparks.clear();
        self.last = None;
        self.last_visible = None;
        self.type_hint = None;
        self.nav_hint = None;
        self.blink_hint = None;
        self.ctx_alt = false;
    }

    /// Advance the trail one frame: observe the cursor at `cur` (`Some(row,col)`
    /// when the cursor is visible, `None` when hidden), spawn a comet on a move,
    /// decay expired sparks, then write the CURRENT trail cells (terminal coords)
    /// into `out` and return a fingerprint of them. The fingerprint changes on
    /// every visible change, so the host's repaint key never skips an animating
    /// frame; it is `0` for an empty trail (idle).
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &TrailConfig,
        out: &mut Vec<TrailCell>,
    ) -> u64 {
        out.clear();
        if !cfg.enabled {
            // Disabled: keep tracking the cursor (so enabling it mid-session does
            // not spawn one giant spurious comet) but draw nothing.
            self.sparks.clear();
            self.last = cur;
            self.last_visible = cur.map(|c| (c, now));
            self.type_hint = None;
            self.nav_hint = None;
            self.blink_hint = None;
            return 0;
        }

        // Spawn on a real move between two VISIBLE positions — where "visible"
        // BRIDGES ConPTY's per-echo hide window: if the previous frame(s) hid the
        // cursor but it was visible within HIDE_BRIDGE_MS and reappears within a
        // plausible typing distance, the move spawns from the last VISIBLE cell
        // (see the `last_visible` field doc — without this, Windows drops the
        // spark for nearly every keystroke at human cadence).
        let spawn_from = self.last.or_else(|| {
            let ((r, c), seen) = self.last_visible?;
            let fresh = now.saturating_duration_since(seen).as_millis() as u64 <= HIDE_BRIDGE_MS;
            // A fresh typed hint widens the plausible reach (PEEKED — `spawn`
            // still owns consumption): batched echoes hop farther than 2 cells
            // inside one hide window; unhinted moves keep the classic law.
            let typed_fresh = self.type_hint.is_some_and(|t| {
                now.saturating_duration_since(t).as_secs_f32() <= Self::TYPE_HINT_FRESH
            });
            let reach = hide_bridge_reach(typed_fresh);
            let plausible = cur.is_some_and(|(cr, cc)| cr.abs_diff(r).max(cc.abs_diff(c)) <= reach);
            (fresh && plausible).then_some((r, c))
        });
        if let (Some((pr, pc)), Some((cr, cc))) = (spawn_from, cur)
            && (pr != cr || pc != cc)
        {
            self.spawn(pr, pc, cr, cc, now, cfg);
        }
        self.last = cur;
        if let Some(c) = cur {
            self.last_visible = Some((c, now));
        }

        // Decay: drop fully-faded sparks (age >= its own baked lifetime). An
        // ignited spark carries a longer `life_ms`, so it lingers past the base fade.
        self.sparks
            .retain(|s| (now.saturating_duration_since(s.born).as_millis() as u64) < s.life_ms);

        // Resolve each spark's current coverage and accumulate into `out`,
        // de-duplicating overlapping cells (keep the brightest). Small N, so the
        // linear scan is cheaper than a map.
        for s in &self.sparks {
            let life_ms = s.life_ms.max(1);
            let age_ms = now.saturating_duration_since(s.born).as_millis() as u64;
            // time fade in 0..=255 (linear), then scale the birth coverage.
            let tf = ((life_ms - age_ms) * 255 / life_ms) as u32;
            let alpha = ((s.born_alpha as u32 * tf) / 255) as u8;
            if alpha == 0 {
                continue;
            }
            match out
                .iter_mut()
                .find(|t| t.row == s.row as usize && t.col == s.col as usize)
            {
                Some(existing) => existing.alpha = existing.alpha.max(alpha),
                None => out.push(TrailCell {
                    row: s.row as usize,
                    col: s.col as usize,
                    alpha,
                }),
            }
        }

        // Fingerprint the produced cells (order is deterministic given sparks).
        let mut fp: u64 = 0;
        for t in out.iter() {
            fp = fp
                .wrapping_mul(1_000_003)
                .wrapping_add(((t.row as u64) << 24) ^ ((t.col as u64) << 8) ^ t.alpha as u64);
        }
        // Fold the resolved trail colour: `ignite` re-heats it from the LIVE
        // typing cadence each frame, so the colour can change while every cell's
        // row/col/alpha quantizes identically — an fp that omitted it let the
        // host's repaint gate hold stale heated pixels (the frozen-glass bug
        // class). Folded only over a NON-EMPTY trail so an idle trail keeps the
        // `fp == 0` idle contract; the `| 1 << 32` bit keeps a black (0x000000)
        // colour from folding as a no-op multiply-add of zero.
        if !out.is_empty() {
            fp = fp
                .wrapping_mul(1_000_003)
                .wrapping_add(u64::from(cfg.color) | (1 << 32));
        }
        fp
    }

    /// Spawn a comet for a move from `(pr,pc)` to `(cr,cc)`: the cells the cursor
    /// swept (the straight cell-line from origin toward the destination, EXCLUDING
    /// the destination cell — the live cursor draws there). On a long jump only
    /// the `max_len` cells nearest the destination survive, so the comet has a
    /// bounded length regardless of the jump distance.
    fn spawn(&mut self, pr: u16, pc: u16, cr: u16, cc: u16, now: Instant, cfg: &TrailConfig) {
        // TYPED RE-ANCHOR (the `CursorGlow` wrap classifier's trail twin): a
        // fresh typed/backspace echo hint paired with a ONE-ROW move beyond the
        // typed advance is a TUI repaint re-anchoring its caret (Ink rewraps
        // its whole inset input box per keystroke) — the cursor never swept
        // those cells, so lay NO comet across them and simply `return` (the
        // live cursor draws itself; `last`/`last_visible` already carried over
        // in `tick`). `dr_abs <= 1` covers wrap-down, backspace wrap-back, and
        // the BOX-GROWTH wrap (live-verified in Claude Code: its bottom-anchored
        // input box grows a row UP, so the caret's terminal row stays constant —
        // dr == 0 with a large column delta);
        // `raw_dist > 2` keeps every ConPTY hide-bridgeable move (chebyshev ≤
        // [`HIDE_BRIDGE_MAX_DIST`] = 2) byte-identical. The hint is consumed on
        // any paired move (bounded state, one hint = one echo), so a host that
        // never arms it — every existing test — is byte-identical.
        let dr_abs = (cr as i32 - pr as i32).abs();
        let raw_dist = dr_abs.max((cc as i32 - pc as i32).abs());
        let paired = self
            .type_hint
            .take_if(|t| now.saturating_duration_since(*t).as_secs_f32() <= Self::TYPE_HINT_FRESH)
            .is_some();
        // ALT-SCREEN blink requirement (the glow classifier's twin): on the
        // alt screen only a repaint-BLINKING app (Claude Code's
        // hide-inside-sync bracket) re-anchors — vim's hinted one-row motions
        // keep their comet. Main screen (`!ctx_alt`) stays byte-identical.
        let blink_fresh = self.blink_hint.is_some_and(|t| {
            now.saturating_duration_since(t).as_secs_f32() <= Self::BLINK_HINT_FRESH
        });
        let nav_paired = self
            .nav_hint
            .take_if(|t| now.saturating_duration_since(*t).as_secs_f32() <= Self::NAV_HINT_FRESH)
            .is_some();
        // NAVIGATION (Ctrl-A/E, Home/End, arrows, word/line motions): a move
        // paired with a fresh nav hint is scrubbing, not typing — lay NO comet,
        // exactly as the `CursorGlow` engine's nav gate suppresses the wake/meteor
        // for every other style. Without this the ember bed painted a full-span
        // band across the whole jump on every Ctrl-A/Ctrl-E (the "nav paints a
        // comet other styles suppress" report); the live cursor still draws
        // itself, and `last`/`last_visible` already carried over in `tick`.
        if nav_paired {
            return;
        }
        if paired && dr_abs <= 1 && raw_dist > 2 && (!self.ctx_alt || blink_fresh) {
            return;
        }
        let mut path = line_cells(pr as i32, pc as i32, cr as i32, cc as i32);
        // Drop the destination cell (last entry) — the cursor occupies it.
        path.pop();
        if path.is_empty() {
            return;
        }
        // Keep at most `max_len` cells NEAREST the destination (the path's tail).
        let max_len = cfg.max_len.max(1);
        if path.len() > max_len {
            let drop = path.len() - max_len;
            path.drain(0..drop);
        }
        // IGNITION: the typing-cadence intensity (baked at spawn) scales the head +
        // tail coverage between their gentle and ignited endpoints, and stretches
        // the spark lifetime so an ignited burst leaves a longer visible comet.
        // WARMTH (typing continuity): sub-ignition cadence additionally stretches
        // LIFETIME ONLY — at human typing rhythm the new ember lands well before
        // the previous one dies, so the bed chains continuously; a lone keystroke
        // (warmth 0) keeps the classic crisp fade. Alphas are untouched by warmth.
        let q = cfg.intensity.clamp(0.0, 1.0);
        let head = lerp_u32(HEAD_ALPHA_GENTLE, HEAD_ALPHA_IGNITED, q);
        let tail = lerp_u32(TAIL_ALPHA_GENTLE, TAIL_ALPHA_IGNITED, q);
        let base_dur = cfg.duration.as_millis().max(1) as f32;
        let life_ms = (base_dur
            * (1.0 + CHAIN_LIFE_MULT * cfg.warmth.clamp(0.0, 1.0) + (IGNITED_LIFE_MULT - 1.0) * q))
            .max(1.0) as u64;
        let n = path.len() as u32;
        // index 0 = farthest from the cursor (faintest), n-1 = nearest (brightest).
        for (i, (r, c)) in path.into_iter().enumerate() {
            if r < 0 || c < 0 {
                continue;
            }
            let frac = (i as u32 + 1) * 255 / n; // 1/n .. 1, scaled to 0..=255
            // READABILITY GUARD: clamp the birth coverage to the readable ceiling so
            // no comet cell — however ignited — ever obscures the glyph beneath it.
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
        // Bound total live sparks (drop the oldest beyond the cap).
        if self.sparks.len() > Self::MAX_SPARKS {
            let drop = self.sparks.len() - Self::MAX_SPARKS;
            self.sparks.drain(0..drop);
        }
    }
}

// ===========================================================================
// TYPING-CADENCE IGNITION
// ===========================================================================
//
// The comet "ignites" only when typing is FAST and SUSTAINED. The model is a
// decaying "heat" accumulator: each keystroke adds a fixed quantum of heat, and
// heat decays with a fixed half-life between keystrokes. Type fast and the decay
// between keys is small, so heat compounds toward a hot steady state; type slowly
// (or stop) and each key's heat has mostly decayed before the next, so heat never
// climbs. A pure knee mapping ([`ignition_intensity`]) then turns heat into the
// `0.0..=1.0` intensity the comet consumes — with a deliberately high lower knee so
// a FEW characters stay gentle (intensity ~0) and only a sustained burst crosses
// into ignition. Everything is clockless (injected `now`), so the ramp-up, the
// gentle-for-a-few-chars floor, and the decay are all unit-testable without sleeping.

/// Tunables for the typing-cadence heat → ignition mapping. `Copy` plain data; the
/// [`Default`] is what the host uses (tuned for a comfortable "fast sustained
/// typing ignites in ~8-10 keys, a few keys stay calm, a pause cools in ~½s").
#[derive(Clone, Copy, Debug)]
pub struct CadenceParams {
    /// Heat added by one keystroke (arbitrary units).
    pub gain: f32,
    /// Heat half-life: the time for accumulated heat to halve with no typing.
    pub half_life: Duration,
    /// Heat where ignition BEGINS to ramp. Below it → intensity 0 (a few keys /
    /// slow typing live here), so the effect stays gentle.
    pub knee_lo: f32,
    /// Heat where ignition is FULL (intensity 1). Fast, sustained typing reaches it.
    pub knee_hi: f32,
}

impl Default for CadenceParams {
    fn default() -> Self {
        Self {
            gain: 1.0,
            half_life: Duration::from_millis(220),
            knee_lo: 2.0,
            knee_hi: 6.5,
        }
    }
}

/// Decay `heat` forward by `dt` under a `half_life` (exponential). Pure + total;
/// `dt == 0` returns `heat` unchanged, and a non-positive half-life is floored so
/// the power is finite.
fn decay_heat(heat: f32, dt: Duration, half_life: Duration) -> f32 {
    let hl = half_life.as_secs_f32().max(1e-3);
    heat * 0.5f32.powf(dt.as_secs_f32() / hl)
}

/// The PURE cadence→intensity mapping the whole feature turns on: a smoothstep of
/// accumulated `heat` between the two knees, clamped to `0.0..=1.0`. Below `knee_lo`
/// → `0.0` (gentle: a few keys never ignite); above `knee_hi` → `1.0` (ignited:
/// fast sustained typing); a smooth S-curve between. Total for all finite inputs.
#[must_use]
pub fn ignition_intensity(heat: f32, knee_lo: f32, knee_hi: f32) -> f32 {
    // Positive valid-interval test (not `!(knee_hi > knee_lo)`): keeps the guard free
    // of a negated partial-ord comparison, and NaN/inverted/equal knees still fall to
    // the `else` and step at the (single) threshold rather than dividing by zero.
    if knee_hi > knee_lo {
        let t = ((heat - knee_lo) / (knee_hi - knee_lo)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    } else if heat >= knee_hi {
        1.0
    } else {
        0.0
    }
}

/// Typing-cadence tracker → comet ignition intensity. Clockless: [`Self::on_keystroke`]
/// and [`Self::intensity`] both take an injected `now`, so a host feeds it real
/// `Instant`s while tests feed synthetic ones. Cheap (`f32` + one `Instant`); the
/// host keeps one per window and reads [`Self::intensity`] each frame.
///
/// scope-waiver: heat is an AESTHETIC intensity, not a safety budget — a
/// second tracker costs a second lazily-decayed `f32` and a differently-lit
/// comet, never a bound anyone can exceed. (The ignition limiter that IS a
/// budget lives in `word_decorations`, under the `flash-limiter` scope claim.)
#[derive(Clone, Debug)]
pub struct TypingCadence {
    params: CadenceParams,
    /// Accumulated heat as of `last` (decayed lazily on the next read/keystroke).
    heat: f32,
    /// When `heat` was last updated. `None` until the first keystroke.
    last: Option<Instant>,
}

impl Default for TypingCadence {
    fn default() -> Self {
        Self::new(CadenceParams::default())
    }
}

impl TypingCadence {
    /// A cold tracker (no heat) with the given tunables.
    #[must_use]
    pub fn new(params: CadenceParams) -> Self {
        Self {
            params,
            heat: 0.0,
            last: None,
        }
    }

    /// Register one keystroke at `now`: decay the standing heat to `now`, then add
    /// one `gain` quantum (capped at `knee_hi` so a long hold saturates cleanly and
    /// decay from the top reaches the gentle band predictably).
    pub fn on_keystroke(&mut self, now: Instant) {
        if let Some(last) = self.last {
            let dt = now.saturating_duration_since(last);
            self.heat = decay_heat(self.heat, dt, self.params.half_life);
        }
        self.heat = (self.heat + self.params.gain).min(self.params.knee_hi);
        self.last = Some(now);
    }

    /// The ignition intensity `0.0..=1.0` as of `now` — the heat decayed forward to
    /// `now`, then run through [`ignition_intensity`]. Non-mutating (safe to call
    /// every frame): a long idle simply reads as a fully-decayed, gentle `0.0`.
    #[must_use]
    pub fn intensity(&self, now: Instant) -> f32 {
        let heat = match self.last {
            Some(last) => {
                let dt = now.saturating_duration_since(last);
                decay_heat(self.heat, dt, self.params.half_life)
            }
            None => 0.0,
        };
        ignition_intensity(heat, self.params.knee_lo, self.params.knee_hi)
    }

    /// Sub-ignition "WARMTH" `0.0..=1.0` as of `now`: `0` at or below ONE key's
    /// worth of heat (the first key of a burst — and therefore any lone keystroke
    /// — contributes nothing, keeping it exactly as crisp as before), ramping to
    /// `1` at the ignition knee. Continuity fuel, NOT brightness: the host feeds
    /// it into the trail LIFETIME only (see [`ignite`]/`TrailConfig::warmth`), so
    /// consecutive keystrokes at human cadence CHAIN into a continuous ember bed
    /// while colour/alpha stay gentle until real ignition. The ignition knees and
    /// [`ignition_intensity`] are untouched — a sustained fast burst still ignites
    /// exactly as today. Non-mutating, like [`Self::intensity`].
    #[must_use]
    pub fn warmth(&self, now: Instant) -> f32 {
        let heat = match self.last {
            Some(last) => {
                let dt = now.saturating_duration_since(last);
                decay_heat(self.heat, dt, self.params.half_life)
            }
            None => 0.0,
        };
        let span = (self.params.knee_lo - self.params.gain).max(f32::EPSILON);
        ((heat - self.params.gain) / span).clamp(0.0, 1.0)
    }
}

/// Blend `base` toward the comet-core HOT tint by `q` (`0` = base, `1` = the max
/// heat blend). A warm white-amber, so an ignited comet reads as a hot streak
/// rather than merely a brighter cursor colour — while staying a PARTIAL blend so
/// the trail keeps the theme's identity and never becomes pure white.
fn heat_color(base: u32, q: f32) -> u32 {
    /// The comet-core tint an ignited trail heats toward (warm white-amber).
    const HOT: u32 = 0x00FF_E6B0;
    /// Even at full ignition the colour is only this far toward [`HOT`] — tasteful,
    /// and it keeps enough of the base hue that the wake still "matches the theme".
    const HEAT_COLOR_MAX: f32 = 0.5;
    let t = (q.clamp(0.0, 1.0) * HEAT_COLOR_MAX).clamp(0.0, 1.0);
    let lerp = |sh: u32| {
        let a = ((base >> sh) & 0xff) as f32;
        let b = ((HOT >> sh) & 0xff) as f32;
        ((a + (b - a) * t).round() as u32).min(255)
    };
    (lerp(16) << 16) | (lerp(8) << 8) | lerp(0)
}

/// Stamp the current typing-cadence `intensity` AND sub-ignition `warmth` onto a
/// resolved [`TrailConfig`] and heat its colour toward the comet core. The host
/// calls this each frame with [`TypingCadence::intensity`]/[`TypingCadence::warmth`]
/// just before the trail tick, so a fast sustained burst ignites the comet
/// (brighter capped core + longer hotter tail) while normal-cadence typing merely
/// CHAINS spark lifetimes (warmth extends life, never colour/brightness — see the
/// spawn life formula). Pure + total (intensity `0.0` leaves the colour untouched
/// — byte-identical to the pre-ignition path; warmth `0.0` leaves the life at the
/// classic base).
pub fn ignite(cfg: &mut TrailConfig, intensity: f32, warmth: f32) {
    let q = intensity.clamp(0.0, 1.0);
    cfg.intensity = q;
    cfg.warmth = warmth.clamp(0.0, 1.0);
    cfg.color = heat_color(cfg.color, q);
}

/// Integer cell-line (Bresenham) from `(r0,c0)` to `(r1,c1)` INCLUSIVE of both
/// endpoints — the cells a straight cursor move sweeps through. Pure + total.
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

    fn cfg(enabled: bool) -> TrailConfig {
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
    fn no_trail_until_a_move() {
        let mut t = CursorTrail::default();
        let now = Instant::now();
        let mut out = Vec::new();
        // First observation establishes `last`; no move yet → nothing.
        assert_eq!(t.tick(Some((0, 0)), now, &cfg(true), &mut out), 0);
        assert!(out.is_empty());
        assert!(!t.is_active());
    }

    #[test]
    fn same_row_jump_spawns_a_bounded_comet_nearest_the_cursor() {
        let mut t = CursorTrail::default();
        let now = Instant::now();
        let mut out = Vec::new();
        let c = cfg(true);
        t.tick(Some((0, 0)), now, &c, &mut out); // seed last = (0,0)
        // Jump to col 40 on the same row: a long move, capped to max_len cells.
        let fp = t.tick(Some((0, 40)), now, &c, &mut out);
        assert_ne!(fp, 0);
        assert!(t.is_active());
        // Bounded to max_len, all on row 0, none on the destination column 40.
        assert!(out.len() <= c.max_len, "comet bounded to max_len");
        assert!(out.iter().all(|t| t.row == 0 && t.col != 40));
        // The cell nearest the cursor (col 39) is the brightest.
        let head = out.iter().max_by_key(|t| t.col).unwrap();
        let tail = out.iter().min_by_key(|t| t.col).unwrap();
        assert!(head.alpha >= tail.alpha, "head brighter than tail");
        assert_eq!(head.col, 39);
    }

    /// The fingerprint folds the RESOLVED trail colour: `ignite` re-heats it
    /// from the live typing cadence every frame, so a colour-only change over
    /// geometrically identical cells must still perturb the fp (else the
    /// host's repaint gate freezes stale heated pixels on glass). Idle stays
    /// fp 0 no matter the colour (the idle-zero law).
    #[test]
    fn fingerprint_folds_the_trail_colour() {
        let run = |color: u32| {
            let mut t = CursorTrail::default();
            let now = Instant::now();
            let mut out = Vec::new();
            let mut c = cfg(true);
            c.color = color;
            // Deterministic seed → identical cells for both colours; the
            // single shared `now` keeps the alphas byte-identical too.
            t.tick(Some((0, 0)), now, &c, &mut out);
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
        // The cells themselves ARE identical — only the colour differs.
        assert_eq!(cells_green.len(), cells_amber.len());
        // And an idle (empty) trail keeps fp 0 regardless of colour.
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
        t.tick(Some((0, 5)), t0, &c, &mut out);
        assert!(t.is_active());
        // Halfway: still alive, dimmer.
        let mid = t.tick(Some((0, 5)), t0 + Duration::from_millis(100), &c, &mut out);
        assert_ne!(mid, 0);
        // Past the duration: fully faded, no sparks, fingerprint 0.
        let gone = t.tick(Some((0, 5)), t0 + Duration::from_millis(250), &c, &mut out);
        assert_eq!(gone, 0);
        assert!(out.is_empty());
        assert!(!t.is_active());
    }

    /// WARMTH LAW: sub-ignition warmth is 0 for a lone key (the first key's heat
    /// equals `gain`, so the `-gain` shift zeroes it — a lone keystroke is exactly
    /// as crisp as before) and RAMPS with sustained human-cadence typing, while
    /// staying below full ignition (intensity ~0 at 300ms cadence).
    #[test]
    fn warmth_is_zero_for_a_lone_key_and_ramps_with_cadence() {
        let mut cad = TypingCadence::default();
        let t0 = Instant::now();
        cad.on_keystroke(t0);
        assert_eq!(cad.warmth(t0), 0.0, "first key of a burst contributes 0");
        // Ten keys at a human 300ms cadence: warmth converges well above 0…
        let mut t = t0;
        for _ in 0..10 {
            t += Duration::from_millis(300);
            cad.on_keystroke(t);
        }
        let w = cad.warmth(t);
        assert!(w > 0.4, "300ms-cadence warmth converges high (got {w})");
        // …while real IGNITION stays essentially off at that cadence.
        assert!(
            cad.intensity(t) < 0.1,
            "300ms cadence must not ignite (got {})",
            cad.intensity(t)
        );
        // A pause cools warmth back toward 0 (continuity fuel drains).
        assert!(
            cad.warmth(t + Duration::from_millis(1500)) < 0.1,
            "warmth decays after a pause"
        );
    }

    /// CHAIN LAW: with warmth, an ember outlives a human inter-key gap (alive at
    /// +350ms) — while at warmth 0 the same spark is gone by +250ms (the classic
    /// crisp fade, pinned above). The bed therefore chains during typing and stays
    /// crisp for a lone key.
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
        t.tick(Some((0, 1)), t0, &c, &mut out); // typing advance, warmth 0.5
        // life = 200ms * (1 + 1.5*0.5) = 350ms → still alive at +340ms…
        let alive = t.tick(Some((0, 1)), t0 + Duration::from_millis(340), &c, &mut out);
        assert_ne!(alive, 0, "warm ember chains across a 300ms+ inter-key gap");
        assert!(t.is_active());
        // …and fully gone at +400ms (crisp tail after the last key).
        let gone = t.tick(Some((0, 1)), t0 + Duration::from_millis(400), &c, &mut out);
        assert_eq!(gone, 0);
        assert!(!t.is_active());
    }

    #[test]
    fn disabled_draws_nothing_but_tracks_cursor() {
        let mut t = CursorTrail::default();
        let now = Instant::now();
        let mut out = Vec::new();
        let c = cfg(false);
        t.tick(Some((0, 0)), now, &c, &mut out);
        let fp = t.tick(Some((0, 40)), now, &c, &mut out);
        assert_eq!(fp, 0);
        assert!(out.is_empty());
        assert!(!t.is_active());
    }

    /// TYPED RE-ANCHOR LAW (the trail twin of the `CursorGlow` classifier): a
    /// typed-hinted one-row move beyond the typed advance — Claude Code's Ink
    /// box rewrapping its inset prompt — lays NO comet (the caret never swept
    /// those cells), while the SAME move without the hint keeps today's comet
    /// byte-identically, and a hinted plain typing advance still trails.
    #[test]
    fn hinted_ink_wrap_lays_no_comet_but_typing_still_trails() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut out = Vec::new();

        // Hinted Ink wrap: (0,30) -> (1,3) — dr=1, dc=-27. No cells.
        let mut t = CursorTrail::default();
        t.tick(Some((0, 30)), t0, &c, &mut out);
        t.note_typed(t0 + Duration::from_millis(5));
        let fp = t.tick(Some((1, 3)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_eq!(fp, 0, "a typed re-anchor sweeps no comet");
        assert!(out.is_empty());
        assert!(!t.is_active());

        // IDENTITY: the same move with NO hint keeps its comet exactly.
        let mut bare = CursorTrail::default();
        bare.tick(Some((0, 30)), t0, &c, &mut out);
        let fp = bare.tick(Some((1, 3)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_ne!(fp, 0, "unhinted hosts are byte-identical to before");
        assert!(!out.is_empty());

        // A hinted PLAIN advance (dist 1) still lays its typing spark.
        let mut typing = CursorTrail::default();
        typing.tick(Some((0, 3)), t0, &c, &mut out);
        typing.note_typed(t0 + Duration::from_millis(5));
        let fp = typing.tick(Some((0, 4)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_ne!(fp, 0, "ordinary typing still trails under the hint");
        assert!(!out.is_empty());
    }

    /// THE KITTY→BLINK SWAP (trail twin of the glow law): on the ALT screen a
    /// typed hint alone no longer re-anchors — without a fresh REPAINT BLINK
    /// (vim never hides inside DEC-2026) the hinted dr==1 big-dc move keeps
    /// its comet, while WITH the blink (Claude Code's per-keystroke bracket)
    /// the same move lays none. Main-screen behavior is pinned byte-identical
    /// by `hinted_ink_wrap_lays_no_comet_but_typing_still_trails` above.
    #[test]
    fn alt_without_blink_keeps_the_jump_comet() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut out = Vec::new();

        // vim (alt ctx, hint armed, NO blink): the jump comet flies.
        let mut vim = CursorTrail::default();
        vim.note_context(true);
        vim.tick(Some((0, 30)), t0, &c, &mut out);
        vim.note_typed(t0 + Duration::from_millis(5));
        vim.note_context(true);
        let fp = vim.tick(Some((1, 3)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_ne!(fp, 0, "alt + hint + NO blink: the jump keeps its comet");
        assert!(!out.is_empty());

        // Claude Code (alt ctx, hint armed, FRESH blink): the wrap re-anchors.
        let mut cc = CursorTrail::default();
        cc.note_context(true);
        cc.tick(Some((0, 30)), t0, &c, &mut out);
        cc.note_typed(t0 + Duration::from_millis(5));
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
    fn hidden_cursor_does_not_spawn_on_reappear() {
        let mut t = CursorTrail::default();
        let now = Instant::now();
        let mut out = Vec::new();
        let c = cfg(true);
        t.tick(Some((0, 0)), now, &c, &mut out); // seen at (0,0)
        t.tick(None, now, &c, &mut out); // cursor hidden
        let fp = t.tick(Some((0, 40)), now, &c, &mut out); // reappears far away
        assert_eq!(fp, 0, "no comet across a hide/show gap");
        assert!(out.is_empty());
    }

    #[test]
    fn line_cells_includes_both_endpoints() {
        let l = line_cells(0, 0, 0, 3);
        assert_eq!(l, vec![(0, 0), (0, 1), (0, 2), (0, 3)]);
        let diag = line_cells(0, 0, 2, 2);
        assert_eq!(diag.first(), Some(&(0, 0)));
        assert_eq!(diag.last(), Some(&(2, 2)));
    }

    // ---- typing-cadence ignition -----------------------------------------

    /// A `TrailConfig` at a chosen ignition level (else the standard test cfg).
    fn cfg_q(q: f32) -> TrailConfig {
        TrailConfig {
            intensity: q,
            ..cfg(true)
        }
    }

    /// Drive `keys` keystrokes `interval` apart into a fresh tracker and return the
    /// intensity immediately after the last one.
    fn burst(keys: u32, interval: Duration) -> f32 {
        let mut cad = TypingCadence::default();
        let t0 = Instant::now();
        let mut t = t0;
        for _ in 0..keys {
            cad.on_keystroke(t);
            t += interval;
        }
        // `t` advanced one interval past the last key; read AT the last key.
        cad.intensity(t - interval)
    }

    #[test]
    fn ignition_intensity_is_a_clamped_smoothstep() {
        // Below the low knee → gentle 0; above the high knee → full 1; monotone between.
        assert_eq!(ignition_intensity(0.0, 2.0, 6.5), 0.0);
        assert_eq!(ignition_intensity(2.0, 2.0, 6.5), 0.0);
        assert_eq!(ignition_intensity(6.5, 2.0, 6.5), 1.0);
        assert_eq!(ignition_intensity(100.0, 2.0, 6.5), 1.0);
        let mid = ignition_intensity(4.25, 2.0, 6.5); // exact midpoint → 0.5
        assert!((mid - 0.5).abs() < 1e-4, "smoothstep midpoint {mid}");
        // Monotone non-decreasing across the ramp.
        let mut prev = -1.0;
        for i in 0..=45 {
            let h = i as f32 * 0.2;
            let v = ignition_intensity(h, 2.0, 6.5);
            assert!(v >= prev - 1e-6, "monotone at heat {h}: {v} < {prev}");
            assert!((0.0..=1.0).contains(&v));
            prev = v;
        }
        // Degenerate knees don't divide by zero — they step at the threshold.
        assert_eq!(ignition_intensity(5.0, 3.0, 3.0), 1.0);
        assert_eq!(ignition_intensity(2.0, 3.0, 3.0), 0.0);
    }

    #[test]
    fn a_few_keys_stay_gentle() {
        // One, two, even three quick keys must not ignite (the user's "don't make it
        // intense for a few characters"). 50 ms apart = a brisk but short tap.
        let iv = Duration::from_millis(50);
        assert_eq!(burst(1, iv), 0.0, "a single key never ignites");
        assert!(
            burst(2, iv) < 0.05,
            "two keys stay gentle: {}",
            burst(2, iv)
        );
        assert!(
            burst(3, iv) < 0.15,
            "three keys stay gentle: {}",
            burst(3, iv)
        );
    }

    #[test]
    fn fast_sustained_burst_ignites() {
        // A sustained fast burst (a real touch-typist, ~40 ms/key) crosses into
        // ignition — the comet lights up.
        let hot = burst(16, Duration::from_millis(40));
        assert!(hot > 0.85, "fast sustained typing ignites (got {hot})");
    }

    #[test]
    fn slow_typing_never_ignites() {
        // Hunt-and-peck (400 ms/key) never accumulates heat, however long you go.
        let cold = burst(24, Duration::from_millis(400));
        assert!(cold < 0.1, "slow typing stays gentle (got {cold})");
    }

    #[test]
    fn a_pause_decays_the_ignition() {
        // Ignite with a fast burst, then stop: within about half a second the comet
        // has cooled back to gentle.
        let mut cad = TypingCadence::default();
        let t0 = Instant::now();
        let mut t = t0;
        for _ in 0..16 {
            cad.on_keystroke(t);
            t += Duration::from_millis(40);
        }
        let peak = cad.intensity(t);
        assert!(peak > 0.85, "burst ignited (got {peak})");
        // Mid-pause it is already dropping; by ~600 ms it is essentially out.
        assert!(
            cad.intensity(t + Duration::from_millis(600)) < 0.05,
            "cooled after a pause"
        );
        // Non-mutating read: querying far in the future still reports gentle.
        assert_eq!(cad.intensity(t + Duration::from_secs(10)), 0.0);
    }

    #[test]
    fn readability_cap_bounds_every_cell_at_any_intensity() {
        // The non-negotiable guard: no swept cell ever exceeds the readable ceiling,
        // for ANY ignition level — so the glyph drawn on top always stays legible.
        for &q in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let mut tr = CursorTrail::default();
            let now = Instant::now();
            let mut out = Vec::new();
            let c = cfg_q(q);
            tr.tick(Some((0, 0)), now, &c, &mut out); // seed
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
            tr.tick(Some((0, 5)), now, &c, &mut out); // same-row jump
            let head = out.iter().map(|t| t.alpha).max().unwrap();
            // The base fade is 200 ms; look 250 ms out — past the gentle life, but
            // inside the ignited (1.6×) life.
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
        // Zero intensity is a no-op on colour (byte-identical to the pre-ignition path).
        let mut c = cfg(true);
        let base = c.color;
        ignite(&mut c, 0.0, 0.0);
        // (warmth/chain law tests live below: warmth_is_zero_for_a_lone_key…,
        // warm_chain_outlives_the_gap)
        assert_eq!(c.intensity, 0.0);
        assert_eq!(c.warmth, 0.0);
        assert_eq!(c.color, base, "gentle keeps the base colour");
        // Full ignition warms the colour toward the hot core and clamps intensity.
        let mut c = cfg(true);
        ignite(&mut c, 2.0, 2.0); // out-of-range clamps to 1.0
        assert_eq!(c.intensity, 1.0);
        assert_eq!(c.warmth, 1.0);
        assert_ne!(c.color, base, "ignited colour is heated");
        // Heated toward warm white-amber: red channel rises above the cool base.
        assert!(
            (c.color >> 16) & 0xff > (base >> 16) & 0xff,
            "red channel warmed"
        );
    }

    /// NAVIGATION SUPPRESSION: a move paired with a fresh nav hint (Ctrl-A/E,
    /// Home/End, arrows) lays NO comet — the ember bed is silent on navigation,
    /// matching the glow engine's nav gate for every other style. Without the
    /// hint the same jump still spawns its comet (byte-identical to before).
    #[test]
    fn navigation_lays_no_comet() {
        let now = Instant::now();
        let c = cfg(true);
        // Baseline: a bare same-row jump (no nav hint) DOES spawn a comet.
        let mut plain = CursorTrail::default();
        let mut out = Vec::new();
        plain.tick(Some((0, 30)), now, &c, &mut out); // seed last
        let fp = plain.tick(Some((0, 0)), now, &c, &mut out); // Ctrl-A-shaped move
        assert_ne!(fp, 0, "an un-hinted jump still lays a comet");
        assert!(plain.is_active());
        // With a fresh nav hint the identical move lays nothing.
        let mut nav = CursorTrail::default();
        out.clear();
        nav.tick(Some((0, 30)), now, &c, &mut out); // seed last
        nav.note_navigation(now);
        let fp = nav.tick(Some((0, 0)), now, &c, &mut out); // Ctrl-A to home
        assert_eq!(fp, 0, "navigation lays no comet");
        assert!(out.is_empty());
        assert!(!nav.is_active());
    }
}
