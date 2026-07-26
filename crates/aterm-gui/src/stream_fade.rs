// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M2 "ink that dries": STREAMED OUTPUT FADES IN. Cells that just arrived from
//! the engine render with their foreground blended from the cell background
//! toward the final foreground (the EXACT linear-light blend,
//! [`aterm_render::blend_text`] uncorrected), on an ease-out-cubic envelope
//! over `stream_fade_ms` (default 90 ms) — so bulk output reads as ink drying
//! onto the glass instead of teleporting.
//!
//! ARCHITECTURE (the [`crate::word_decorations`] diff-and-decorate pattern):
//! the host diffs each presented engine mirror (`cell_frame_into` output)
//! against a per-window AGE MAP, stamps changed cells with a birth instant,
//! and tints young cells' `fg` IN THE SNAPSHOT — strictly after the engine
//! filled it and before either renderer reads it. The engine grid, copied
//! text, and recordings are never touched, and CPU/GPU byte-parity holds by
//! construction (both backends consume the same tinted `RenderInput` bytes).
//!
//! Vertical scrolls do not re-ink: a row whose content HASH matches any row of
//! the previous frame carries its cell ages over (the word-decorations
//! row-independent-identity idea), so `cat bigfile` fades only the genuinely
//! new lines while everything that merely moved up stays dry.
//!
//! The SACRED introspection path (`image`/`snapshot`) deliberately captures
//! the CONVERGED frame (it refills its own snapshot and never runs this diff):
//! an AI reading the screen wants settled content, and any tint on the glass
//! is gone within `stream_fade_ms` — the sub-100 ms transient is the one
//! WYSIWYG divergence, bounded by the fade window by the convergence theorem.
//!
//! # Invariants (proven)
//!
//! * **CONVERGENCE-TO-EXACT** — a cell whose age has reached `stream_fade_ms`
//!   is NEVER touched: [`fade_alpha`] returns exactly 255 there (a structural
//!   early return, before any float math), the 255 branch of the tint is a
//!   byte-exact identity, and [`StreamFade::update`] retires the age entry so
//!   the steady frame is byte-identical to the no-feature frame forever
//!   (`converged_frame_is_byte_identical`, `fade_alpha_converges_exactly`).
//! * **BYPASS SOUNDNESS** — under ANY bypass ([`fade_permitted`] false:
//!   feature off, keystroke echo in flight, alternate screen, scrolled-back
//!   viewport, Reduced motion policy) `update` mutates nothing AND dries all
//!   ink, so output is byte-identical to the no-feature path and a resumed
//!   gate never re-tints settled cells. Two-tier proof: the ty-checked
//!   `aterm_spec::derive::stream_fade_gate_model` (Tier-0) and the exhaustive
//!   2^5 `fade_gate_exhaustive` below (Tier-1, a complete proof — the domain
//!   is finite booleans), plus the pipeline test `bypass_is_byte_identical`.
//! * **MONOTONICITY** — the fg opacity is nondecreasing in age:
//!   [`fade_alpha`] is monotone (`fade_alpha_monotone_in_age`; every step of
//!   its float chain is a monotone function of monotone inputs, and the test
//!   checks the actual rounded output exhaustively over the age lattice), and
//!   the underlying blend is monotone in coverage over its ENTIRE byte domain
//!   (aterm-render's `tests/stream_fade_blend.rs`), so a drying glyph only
//!   ever approaches its final ink (`opacity_monotone_readback`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use aterm_core::terminal::{RenderCell, UnderlineStyle};

/// The BYPASS GATE: whether stream fading may tint this frame at all — a pure,
/// total function of the five facts the redraw has in hand. Every bypass is a
/// bypass TO INSTANT (exact bytes), never to a different animation.
///
/// * `enabled` — config `stream_fade` (absent-config default off; the generated
///   starter file opts in explicitly).
/// * `input_hot` — a keystroke egress awaits its echo: typed characters must
///   land instantly (fading the echo would read as added latency).
/// * `alt_screen` — full-screen programs (vim/less/htop) repaint in place;
///   fading their UI churn would smear, not "dry".
/// * `scrolled_back` — the viewport shows history, which is settled ink.
/// * `motion_reduced` — the resolved W11 [`crate::motion::MotionPolicy`] is
///   `Reduced` (config / OS "Reduce Motion" / unfocused window).
///
/// # Invariant (proven)
///
/// `fade_permitted(..) == true` **implies** `enabled && !input_hot &&
/// !alt_screen && !scrolled_back && !motion_reduced`. Two-tier proof: the
/// `StreamFadeGate` derived ty model (`aterm_spec::derive::stream_fade_gate_model`,
/// checked by the real Trust `ty` in aterm-spec's `derived_ring_ty` — proven at
/// `Buggy=0`, counterexample REQUIRED at `Buggy=1`, which reproduces the
/// fading-keystroke-echo defect) and the exhaustive 2^5 enumeration below
/// (`fade_gate_exhaustive`) over this shipping policy itself.
#[must_use]
pub(crate) fn fade_permitted(
    enabled: bool,
    input_hot: bool,
    alt_screen: bool,
    scrolled_back: bool,
    motion_reduced: bool,
) -> bool {
    enabled && !input_hot && !alt_screen && !scrolled_back && !motion_reduced
}

/// Whether [`StreamFade::update`] must be CALLED this committed frame.
///
/// `update`'s step 1 fingerprints the WHOLE grid (`O(rows×cols)`,
/// [`cell_fp`] per cell) unconditionally — that pass is load-bearing whenever
/// the feature is configured on (an enabled-but-per-frame-bypassed frame still
/// has to keep the fingerprints current so a resumed gate stays sound). But
/// when the feature is configured OFF (`fade_on == false`) AND no tint from the
/// previous frame still needs erasing (`fade_shown == false`), the whole call
/// is pure discarded work — the tint block never runs and nothing reads the
/// fingerprints. Skip it: an off, already-dry window does ZERO per-frame grid
/// work (the branch's `O(damage)` idle discipline).
///
/// Gated on CONFIG (`fade_on`), never on the per-frame [`fade_permitted`]
/// bypass — gating on the bypass would drop fingerprints under `input_hot` /
/// alt-screen / scrollback and let a resumed fade re-tint instant-shown cells.
#[must_use]
pub(crate) fn fade_update_needed(fade_on: bool, fade_shown: bool) -> bool {
    fade_on || fade_shown
}

/// The fade ENVELOPE: the coverage (0..=255) of the final foreground at
/// `age_ms` into a `fade_ms` fade — ease-out cubic (`1 - (1-x)^3`), so fresh
/// ink darkens fast and settles gently.
///
/// # Invariants (proven in this module's tests)
///
/// * **CONVERGENCE**: `age_ms >= fade_ms` (including `fade_ms == 0`) returns
///   EXACTLY 255 — a structural early return, before any float math — so a
///   dried cell's blend is the byte-exact identity.
/// * **MONOTONE**: nondecreasing in `age_ms` for any fixed `fade_ms` (checked
///   exhaustively over the age range for a lattice of fade windows).
/// * **TOTAL**: defined (no panic, result always 0..=255) on the whole
///   `u64 × u64` domain.
#[must_use]
pub(crate) fn fade_alpha(age_ms: u64, fade_ms: u64) -> u8 {
    if age_ms >= fade_ms {
        return 255; // convergence: exact, before any float math (fade_ms == 0 included)
    }
    let x = age_ms as f32 / fade_ms as f32; // in [0, 1): fade_ms > age_ms >= 0 here
    let u = 1.0 - x;
    let e = 1.0 - u * u * u; // ease-out cubic
    (e * 255.0).round().clamp(0.0, 255.0) as u8
}

/// The tint of one drying cell: fg over its own cell background at coverage
/// `alpha`, composited in EXACT LINEAR LIGHT ([`aterm_render::blend_text`]
/// with the perceptual remap OFF — the pure physical blend, so the tint is
/// fringe-free on any fg/bg pair). `alpha == 255` returns `fg` untouched (no
/// u32 round-trip): the convergence endpoint is byte-exact by construction.
fn tinted_fg(fg: [u8; 3], bg: [u8; 3], alpha: u8) -> [u8; 3] {
    if alpha == 255 {
        return fg;
    }
    let f = aterm_render::rgb_to_u32(fg);
    let b = aterm_render::rgb_to_u32(bg);
    let out = aterm_render::blend_text(b, f, b, alpha, false);
    [
        ((out >> 16) & 0xff) as u8,
        ((out >> 8) & 0xff) as u8,
        (out & 0xff) as u8,
    ]
}

/// FNV-1a fold of one word into a running hash.
fn fnv(mut h: u64, x: u64) -> u64 {
    h ^= x;
    h.wrapping_mul(0x0000_0100_0000_01B3)
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn underline_id(u: UnderlineStyle) -> u64 {
    match u {
        UnderlineStyle::None => 0,
        UnderlineStyle::Single => 1,
        UnderlineStyle::Double => 2,
        UnderlineStyle::Curly => 3,
        UnderlineStyle::Dotted => 4,
        UnderlineStyle::Dashed => 5,
    }
}

/// One cell's content fingerprint: `(ch, style-hash)` — every visible field of
/// the RAW engine cell (char, resolved fg/bg, rendition flags, underline
/// style + colour). Computed BEFORE any tint is applied, so the age map always
/// diffs engine truth against engine truth (a tint can never feed back into
/// the next frame's diff).
fn cell_fp(c: &RenderCell) -> u64 {
    let flags = u64::from(c.wide)
        | (u64::from(c.emoji_presentation) << 1)
        | (u64::from(c.bold) << 2)
        | (u64::from(c.italic) << 3)
        | (u64::from(c.strikethrough) << 4)
        | (u64::from(c.overline) << 5)
        | (underline_id(c.underline) << 6);
    // Sentinel bit 32 distinguishes "no underline colour" from colour 0x000000.
    let ul = c
        .underline_color
        .map_or(1 << 32, |rgb| u64::from(aterm_render::rgb_to_u32(rgb)));
    let mut h = FNV_OFFSET;
    for x in [
        u64::from(u32::from(c.ch)),
        u64::from(aterm_render::rgb_to_u32(c.fg)),
        u64::from(aterm_render::rgb_to_u32(c.bg)),
        flags,
        ul,
    ] {
        h = fnv(h, x);
    }
    h
}

/// The view a frame was extracted from: `(session identity, alternate screen,
/// scrolled-back)`. Any change re-baselines the age map — a switched-to tab,
/// a vim exit, or a return from scrollback shows SETTLED content, not fresh
/// ink, so none of it fades.
pub(crate) type ViewKey = (usize, bool, bool);

/// Per-window stream-fade state: the engine-mirror fingerprint grid, per-cell
/// birth instants, and per-row content hashes of the last presented frame.
/// Idle (feature off / everything dry) it holds no live births, arms no wakes,
/// and `update` leaves every frame byte-identical — the 0%-idle property.
#[derive(Default)]
pub(crate) struct StreamFade {
    rows: usize,
    cols: usize,
    /// Identity of the mirrored view; `None` forces a re-baseline (fresh
    /// window, layout change, explicit reset).
    view: Option<ViewKey>,
    /// `rows × cols` raw-content fingerprints of the last accepted frame.
    fp: Vec<u64>,
    /// Birth instant per cell; `None` = dry (steady — rendered exact forever).
    born: Vec<Option<Instant>>,
    /// Per-row content hash of the last accepted frame (scroll alignment).
    row_hash: Vec<u64>,
    /// Double buffers, swapped each update (resident — no per-frame alloc).
    fp_scratch: Vec<u64>,
    born_scratch: Vec<Option<Instant>>,
    hash_scratch: Vec<u64>,
    /// Previous frame's `row hash → row index` (first occurrence), rebuilt per
    /// update from the resident map (reuses its allocation).
    row_of: HashMap<u64, usize>,
    /// The latest instant any live fade still needs frames (`max(born) +
    /// fade_ms`); `None` when everything is dry. Lets the scheduler poll
    /// [`Self::is_active`] without the config, like the sparkle words.
    active_until: Option<Instant>,
}

impl StreamFade {
    /// Drop the mirrored view (layout-space change / explicit invalidation):
    /// the next `update` re-baselines — everything on screen is settled ink.
    pub(crate) fn reset(&mut self) {
        self.view = None;
        self.active_until = None;
    }

    /// Whether any ink is still drying — the scheduler keeps frame-paced wakes
    /// armed while true, then drops to pure `Wait` (0% idle). Cheap: reads the
    /// deadline last computed by [`Self::update`].
    pub(crate) fn is_active(&self, now: Instant) -> bool {
        self.active_until.is_some_and(|d| now < d)
    }

    /// Diff this frame's RAW engine mirror into the age map, then tint every
    /// cell younger than `fade_ms` toward its own cell background (exact
    /// linear-light, ease-out cubic). Returns whether any cell was tinted —
    /// the caller's `fade_shown` (the erase-frame discipline, like
    /// `pred_shown`).
    ///
    /// * `permitted == false` (any bypass): NOTHING is mutated — the frame
    ///   renders byte-exact — and every age is dried, so a later-resumed gate
    ///   cannot re-tint cells that were shown instant (no flicker).
    /// * A `view` change (tab switch / alt-screen flip / scrollback return) or
    ///   a dimension change re-baselines: the whole frame is absorbed as
    ///   settled ink (exact bytes, no births).
    /// * Otherwise rows are aligned by content hash first (a row that merely
    ///   scrolled keeps its ages), then changed rows diff per cell against the
    ///   same viewport position.
    ///
    /// `cols` is the frame's declared [`aterm_core::render::RenderInput::cols`],
    /// not an inner-row length. Engine rows are sparse prefixes, so even the
    /// first row may be empty while later rows contain output.
    pub(crate) fn update(
        &mut self,
        cells: &mut [Vec<RenderCell>],
        cols: usize,
        view: ViewKey,
        permitted: bool,
        fade_ms: u64,
        now: Instant,
    ) -> bool {
        let rows = cells.len();

        // 1) Fingerprint THIS frame (raw, pre-tint) + per-row content hashes.
        self.fp_scratch.clear();
        self.fp_scratch.reserve(rows * cols);
        self.hash_scratch.clear();
        for row in cells.iter() {
            let mut rh = FNV_OFFSET;
            // `cell_frame_into` deliberately emits SPARSE inner rows: an omitted
            // tail is the terminal's implicit empty cell through the declared
            // `RenderInput.cols`, not a malformed/ragged frame. Fingerprint the
            // full logical row with a stable absent-slot sentinel so a short top
            // row cannot shrink the whole age map and later rows remain visible
            // to the diff.
            for col in 0..cols {
                let f = row.get(col).map_or(0, cell_fp);
                self.fp_scratch.push(f);
                rh = fnv(rh, f);
            }
            self.hash_scratch.push(rh);
        }

        let rebaseline = self.view != Some(view) || self.rows != rows || self.cols != cols;
        self.born_scratch.clear();
        self.born_scratch.resize(rows * cols, None);
        let mut tinted = false;
        let mut active_until: Option<Instant> = None;

        if permitted && !rebaseline {
            // 2) Scroll alignment: previous frame's row hash → row index.
            self.row_of.clear();
            for (i, h) in self.row_hash.iter().enumerate() {
                self.row_of.entry(*h).or_insert(i);
            }
            for r in 0..rows {
                let nh = self.hash_scratch[r];
                // Prefer the same viewport position (in-place identical row),
                // else any content-identical previous row (it merely moved).
                let src = if self.row_hash.get(r) == Some(&nh) {
                    Some(r)
                } else {
                    self.row_of.get(&nh).copied()
                };
                let dst = r * cols;
                match src {
                    Some(sr) => {
                        // Content-identical row: carry its ages over verbatim
                        // (dims match — `rebaseline` covered any change).
                        let s = sr * cols;
                        self.born_scratch[dst..dst + cols].copy_from_slice(&self.born[s..s + cols]);
                    }
                    None => {
                        // Genuinely changed row: per-cell diff against the same
                        // viewport position, so an in-place edit (progress bar,
                        // spinner) births only the cells that actually changed.
                        for c in 0..cols {
                            let i = dst + c;
                            self.born_scratch[i] = if self.fp[i] == self.fp_scratch[i] {
                                self.born[i]
                            } else {
                                Some(now)
                            };
                        }
                    }
                }
            }
            // 3) Tint pass: young cells blend toward their own background; a
            // cell at (or rounded to) full coverage is retired — EXACT bytes
            // from here on, forever (the convergence theorem).
            for (i, slot) in self.born_scratch.iter_mut().enumerate() {
                let Some(b) = *slot else { continue };
                let age_ms =
                    u64::try_from(now.saturating_duration_since(b).as_millis()).unwrap_or(u64::MAX);
                let alpha = fade_alpha(age_ms, fade_ms);
                if alpha == 255 {
                    *slot = None;
                    continue;
                }
                // A materialized cell may disappear back into the sparse,
                // implicit-empty tail while its slot is young. That transition
                // still belongs in the fingerprint map, but there are no cell
                // bytes to tint; retire it instead of indexing a ragged row.
                let Some(cell) = cells
                    .get_mut(i / cols)
                    .and_then(|row| row.get_mut(i % cols))
                else {
                    *slot = None;
                    continue;
                };
                cell.fg = tinted_fg(cell.fg, cell.bg, alpha);
                tinted = true;
                let until = b + Duration::from_millis(fade_ms);
                active_until = Some(active_until.map_or(until, |d| d.max(until)));
            }
        }
        // Bypassed or re-baselined: `born_scratch` stays all-`None` — the frame
        // renders exact bytes and ALL ink dries (bypass soundness: a resumed
        // gate never re-tints what was shown instant).

        std::mem::swap(&mut self.fp, &mut self.fp_scratch);
        std::mem::swap(&mut self.born, &mut self.born_scratch);
        std::mem::swap(&mut self.row_hash, &mut self.hash_scratch);
        self.rows = rows;
        self.cols = cols;
        self.view = Some(view);
        self.active_until = active_until;
        tinted
    }
}

#[cfg(test)]
mod tests {
    //! Two-tier proofs, Tier-1 (real code): `fade_gate_exhaustive` enumerates
    //! the SAME invariant the derived ty model
    //! `aterm_spec::derive::stream_fade_gate_model` carries (Tier-0, checked by
    //! the real Trust `ty` in aterm-spec's `derived_ring_ty`), over the
    //! SHIPPING `fade_permitted` itself — the domain is 2^5 booleans, so the
    //! exhaustive enumeration is a COMPLETE proof under plain `cargo test`.
    //! The envelope/pipeline tests pin the M2 PROVE bullets (convergence,
    //! bypass soundness, monotonicity) on the shipping `StreamFade`.

    use super::*;

    /// BYPASS SOUNDNESS (gate half): over ALL 2^5 input combinations the gate
    /// permits fading iff enabled and NO bypass holds — so any single bypass
    /// forces the instant path. Non-vacuity: the permitted point is reachable
    /// (exactly one of the 32).
    #[test]
    fn fade_gate_exhaustive() {
        let mut permitted_points = 0;
        for bits in 0u8..32 {
            let enabled = bits & 1 != 0;
            let hot = bits & 2 != 0;
            let alt = bits & 4 != 0;
            let scrolled = bits & 8 != 0;
            let reduced = bits & 16 != 0;
            let got = fade_permitted(enabled, hot, alt, scrolled, reduced);
            let want = enabled && !hot && !alt && !scrolled && !reduced;
            assert_eq!(
                got, want,
                "gate({enabled},{hot},{alt},{scrolled},{reduced}) truth table"
            );
            // Each bypass individually forces instant.
            if hot || alt || scrolled || reduced || !enabled {
                assert!(!got, "a live bypass must force the instant path");
            }
            permitted_points += usize::from(got);
        }
        assert_eq!(permitted_points, 1, "non-vacuity: fading IS reachable");
    }

    /// NEGATIVE CONTROL (the ty model's `Buggy=1` twin): a gate that ignores
    /// `input_hot` disagrees with the proven one exactly on the typing point —
    /// the truth-table assertion above genuinely catches that regression.
    #[test]
    fn ignoring_input_hot_is_caught() {
        let buggy = |enabled: bool, _hot: bool, alt: bool, scrolled: bool, reduced: bool| {
            enabled && !alt && !scrolled && !reduced
        };
        assert!(!fade_permitted(true, true, false, false, false));
        assert!(
            buggy(true, true, false, false, false),
            "control: the buggy gate would fade the keystroke echo"
        );
    }

    /// IDLE GATE (perf): over ALL 2^2 inputs, `fade_update_needed` is true iff
    /// the feature is on OR a tint from last frame still needs erasing — the
    /// ONLY skippable point is `(off, dry)`. Non-vacuity: both the skip point
    /// and at least one run point are reachable.
    #[test]
    fn fade_update_needed_exhaustive() {
        let mut skip_points = 0;
        let mut run_points = 0;
        for bits in 0u8..4 {
            let fade_on = bits & 1 != 0;
            let fade_shown = bits & 2 != 0;
            let got = fade_update_needed(fade_on, fade_shown);
            let want = fade_on || fade_shown;
            assert_eq!(got, want, "needed({fade_on},{fade_shown}) truth table");
            if got {
                run_points += 1;
            } else {
                skip_points += 1;
                assert!(
                    !fade_on && !fade_shown,
                    "the only skippable frame is off-and-dry"
                );
            }
        }
        assert_eq!(skip_points, 1, "non-vacuity: exactly one skippable point");
        assert_eq!(run_points, 3, "non-vacuity: the run path IS reachable");
    }

    /// NEGATIVE CONTROL (the pre-fix defect): the old code ALWAYS called
    /// `update` (an implicit `|_, _| true`), so the disabled-and-dry frame paid
    /// the whole-grid fingerprint pass. The gate above disagrees with that
    /// constant exactly on `(off, dry)` — the point that was pure wasted work.
    #[test]
    fn always_calling_update_is_caught() {
        let pre_fix = |_fade_on: bool, _fade_shown: bool| true;
        assert!(!fade_update_needed(false, false), "off+dry now skips");
        assert!(
            pre_fix(false, false),
            "control: the old code fingerprinted the whole grid anyway"
        );
    }

    /// CONVERGENCE (envelope half): `age >= fade` returns EXACTLY 255 for a
    /// lattice of fade windows (0 — the degenerate instant config — through
    /// the clamp extremes), including the u64 edges. No permanent tint.
    #[test]
    fn fade_alpha_converges_exactly() {
        for fade in [0u64, 1, 2, 16, 89, 90, 91, 250, 1000, u64::MAX] {
            for over in [0u64, 1, 7, 1000] {
                let age = fade.saturating_add(over);
                if age >= fade {
                    assert_eq!(
                        fade_alpha(age, fade),
                        255,
                        "age {age} >= fade {fade} must be exact"
                    );
                }
            }
        }
        assert_eq!(fade_alpha(u64::MAX, u64::MAX), 255);
        assert_eq!(fade_alpha(0, 0), 255, "fade_ms 0 is the instant config");
    }

    /// MONOTONICITY (envelope half) + totality/range: for each fade window in
    /// the lattice, sweep EVERY age 0..=fade+8 and require the rounded output
    /// never decreases (and stays a valid byte by type). Non-vacuity: the ramp
    /// genuinely starts translucent (alpha(0) == 0 for a real window) and
    /// passes through interior coverage.
    #[test]
    fn fade_alpha_monotone_in_age() {
        for fade in [1u64, 2, 3, 5, 16, 90, 250, 1000] {
            let mut prev = fade_alpha(0, fade);
            let mut saw_interior = false;
            for age in 1..=fade + 8 {
                let a = fade_alpha(age, fade);
                assert!(
                    a >= prev,
                    "fade_alpha({age}, {fade}) = {a} < previous {prev}: not monotone"
                );
                saw_interior |= a > 0 && a < 255;
                prev = a;
            }
            assert_eq!(prev, 255, "the sweep must end converged");
            if fade >= 5 {
                assert_eq!(fade_alpha(0, fade), 0, "fresh ink starts at the bg");
                assert!(saw_interior, "non-vacuity: interior coverage is reached");
            }
        }
    }

    /// CONVERGENCE (blend half): the 255-coverage tint is a byte-exact
    /// identity for extreme and interior fg/bg pairs — no u32 round-trip, no
    /// sRGB round-trip (readback over the blend, per the M2 PROVE bullet).
    #[test]
    fn tint_at_full_coverage_is_byte_exact() {
        let lattice = [0u8, 1, 63, 127, 128, 200, 254, 255];
        for &fr in &lattice {
            for &br in &lattice {
                let fg = [fr, fr ^ 0x5a, 255 - fr];
                let bg = [br, br ^ 0xa5, 255 - br];
                assert_eq!(tinted_fg(fg, bg, 255), fg, "alpha 255 must be identity");
                assert_eq!(tinted_fg(fg, bg, 0), bg, "alpha 0 is exactly the bg");
            }
        }
        // Non-vacuity: interior coverage genuinely tints.
        let mid = tinted_fg([255, 255, 255], [0, 0, 0], 128);
        assert!(
            mid != [255, 255, 255] && mid != [0, 0, 0],
            "alpha 128 must land strictly between bg and fg, got {mid:?}"
        );
    }

    // ---- pipeline tests over the shipping StreamFade -----------------------

    fn cell(ch: char, fg: [u8; 3], bg: [u8; 3]) -> RenderCell {
        RenderCell {
            ch,
            fg,
            bg,
            wide: false,
            emoji_presentation: false,
            bold: false,
            italic: false,
            underline: UnderlineStyle::None,
            strikethrough: false,
            overline: false,
            underline_color: None,
        }
    }

    fn grid_of(lines: &[&str]) -> Vec<Vec<RenderCell>> {
        lines
            .iter()
            .map(|l| {
                l.chars()
                    .map(|ch| cell(ch, [220, 220, 220], [10, 10, 30]))
                    .collect()
            })
            .collect()
    }

    const VIEW: ViewKey = (0x1000, false, false);
    const FADE_MS: u64 = 90;

    /// CONVERGENCE-TO-EXACT (the critical theorem, whole pipeline): fresh ink
    /// tints, and once every age reaches `fade_ms` the rendered frame is
    /// byte-identical to the raw engine mirror — no permanent tint.
    #[test]
    fn converged_frame_is_byte_identical() {
        let t0 = Instant::now();
        let mut sf = StreamFade::default();
        let base = grid_of(&["hello   ", "        "]);

        // Frame 1: re-baseline (fresh view) — settled ink, byte-exact.
        let mut f1 = base.clone();
        assert!(!sf.update(&mut f1, 8, VIEW, true, FADE_MS, t0));
        assert_eq!(f1, base, "the baseline frame must not tint");

        // Frame 2: a new word arrives → young cells tint (non-vacuity).
        let with_ink = grid_of(&["hello   ", "world   "]);
        let mut f2 = with_ink.clone();
        let t1 = t0 + Duration::from_millis(5);
        assert!(
            sf.update(&mut f2, 8, VIEW, true, FADE_MS, t1),
            "fresh ink must tint"
        );
        assert_ne!(f2, with_ink, "young cells must differ from the raw mirror");
        assert!(sf.is_active(t1), "a live fade must arm the wake");
        // The untouched first row stays byte-exact even mid-fade.
        assert_eq!(f2[0], with_ink[0], "settled rows are never touched");

        // Frame 3: past the fade window — byte-identical to the raw mirror.
        let mut f3 = with_ink.clone();
        let t2 = t1 + Duration::from_millis(FADE_MS);
        assert!(
            !sf.update(&mut f3, 8, VIEW, true, FADE_MS, t2),
            "dry ink must not tint"
        );
        assert_eq!(f3, with_ink, "the converged frame must be byte-identical");
        assert!(!sf.is_active(t2), "everything dry: the wake must disarm");
    }

    /// BYPASS SOUNDNESS (whole pipeline): with any bypass in force the frame
    /// is byte-identical to the no-feature path — even mid-flight (a fade in
    /// progress cuts to exact instantly) — and the bypassed change is absorbed
    /// as settled ink, so a resumed gate never re-tints it (no flicker).
    #[test]
    fn bypass_is_byte_identical() {
        let t0 = Instant::now();
        let mut sf = StreamFade::default();
        let base = grid_of(&["prompt$ ", "        "]);
        let mut f1 = base.clone();
        sf.update(&mut f1, 8, VIEW, true, FADE_MS, t0);

        // Start a fade…
        let inked = grid_of(&["prompt$ ", "output  "]);
        let mut f2 = inked.clone();
        let t1 = t0 + Duration::from_millis(5);
        assert!(sf.update(&mut f2, 8, VIEW, true, FADE_MS, t1));

        // …then a bypass (keystroke echo): exact bytes, mid-flight fade cut.
        let echoed = grid_of(&["prompt$ x", "output   "]);
        let mut f3 = echoed.clone();
        let t2 = t1 + Duration::from_millis(5);
        assert!(
            !sf.update(&mut f3, 9, VIEW, false, FADE_MS, t2),
            "bypass must not tint"
        );
        assert_eq!(f3, echoed, "bypassed output must be byte-identical");
        assert!(!sf.is_active(t2), "bypass dries all ink (wake disarms)");

        // Gate reopens with content unchanged: the absorbed echo stays exact.
        let mut f4 = echoed.clone();
        let t3 = t2 + Duration::from_millis(5);
        assert!(
            !sf.update(&mut f4, 9, VIEW, true, FADE_MS, t3),
            "no re-tint after a bypass"
        );
        assert_eq!(f4, echoed, "settled echo must never fade late");
    }

    /// MONOTONICITY (whole pipeline, readback): sampling one drying white-on-
    /// black cell at 1 ms steps, every channel is nondecreasing in age and the
    /// final sample equals the exact fg. (The blend itself is proven monotone
    /// in coverage over its whole byte domain in aterm-render's
    /// `tests/stream_fade_blend.rs`; this binds the composed pipeline.)
    #[test]
    fn opacity_monotone_readback() {
        let t0 = Instant::now();
        let mut sf = StreamFade::default();
        let blank = vec![vec![cell(' ', [255, 255, 255], [0, 0, 0])]];
        let inked = vec![vec![cell('X', [255, 255, 255], [0, 0, 0])]];

        let mut f = blank.clone();
        sf.update(&mut f, 1, VIEW, true, FADE_MS, t0);
        let mut prev = [0u8; 3];
        for age in 0..=FADE_MS {
            let mut frame = inked.clone();
            sf.update(
                &mut frame,
                1,
                VIEW,
                true,
                FADE_MS,
                t0 + Duration::from_millis(age),
            );
            let fg = frame[0][0].fg;
            for ch in 0..3 {
                assert!(
                    fg[ch] >= prev[ch],
                    "channel {ch} regressed at age {age}: {} -> {}",
                    prev[ch],
                    fg[ch]
                );
            }
            prev = fg;
        }
        assert_eq!(
            prev,
            [255, 255, 255],
            "the last sample must be the exact fg"
        );
    }

    /// SCROLL ALIGNMENT (taste, pinned): a row that merely scrolled up keeps
    /// its ages — only the genuinely new line tints. Without the row-hash
    /// carry, every scroll would re-ink the whole screen.
    #[test]
    fn scrolled_rows_do_not_refade() {
        let t0 = Instant::now();
        let mut sf = StreamFade::default();
        let f1_raw = grid_of(&["alpha   ", "beta    ", "        "]);
        let mut f1 = f1_raw.clone();
        sf.update(&mut f1, 8, VIEW, true, FADE_MS, t0);

        // One line of output scrolls everything up; rows 0/1 are old content.
        let f2_raw = grid_of(&["beta    ", "        ", "gamma   "]);
        let mut f2 = f2_raw.clone();
        let t1 = t0 + Duration::from_millis(500); // long after baseline: old ink is dry
        assert!(sf.update(&mut f2, 8, VIEW, true, FADE_MS, t1));
        assert_eq!(f2[0], f2_raw[0], "a scrolled row must stay byte-exact");
        assert_ne!(f2[2], f2_raw[2], "the new line must tint");
    }

    /// VIEW CHANGES re-baseline: a tab switch / alt-screen exit / scrollback
    /// return shows settled content — byte-exact, nothing fades.
    #[test]
    fn view_change_rebaselines() {
        let t0 = Instant::now();
        let mut sf = StreamFade::default();
        let mut f1 = grid_of(&["vim buffer  "]);
        sf.update(&mut f1, 12, (7, true, false), false, FADE_MS, t0); // alt screen: bypassed

        // Leaving the alternate screen restores completely different content.
        let shell = grid_of(&["prompt$     "]);
        let mut f2 = shell.clone();
        let t1 = t0 + Duration::from_millis(5);
        assert!(
            !sf.update(&mut f2, 12, (7, false, false), true, FADE_MS, t1),
            "an alt-screen exit must not fade the restored screen"
        );
        assert_eq!(f2, shell, "the restored view is settled ink");
    }

    /// RESIZE re-baselines: a reflowed grid is settled content, not fresh ink.
    #[test]
    fn resize_rebaselines() {
        let t0 = Instant::now();
        let mut sf = StreamFade::default();
        let mut f1 = grid_of(&["abcdef", "      "]);
        sf.update(&mut f1, 6, VIEW, true, FADE_MS, t0);
        let wide = grid_of(&["abcdefgh", "        "]);
        let mut f2 = wide.clone();
        let t1 = t0 + Duration::from_millis(5);
        assert!(!sf.update(&mut f2, 8, VIEW, true, FADE_MS, t1));
        assert_eq!(f2, wide, "a resized frame must not tint");
    }
}
