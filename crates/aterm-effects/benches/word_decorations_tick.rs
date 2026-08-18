// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-presented-frame cost of the sparkle-words engine
//! (`WordDecorations::tick`, word_decorations.rs:5913).
//!
//! WHY THIS EXISTS: `tick` is the whole module's per-frame surface — the GUI
//! calls it once per frame on the unsplit path (app_render.rs:13671) and once
//! PER PANE on the composed path (app_render.rs:16549) — and until this file
//! landed nothing measured it. The audit of this module found that cost scales
//! with three independent quantities, only ONE of which is "what is visible and
//! changing":
//!
//! `occ.len()` — visible occurrences, capped at MAX_OCCURRENCES = 128, walked
//! ~6 times per tick. `persist` — the grace-backed episode map, capped at
//! PERSIST_CAP = 512, walked IN FULL TWICE per tick (by `stamp_spent_marks` and
//! by `super_prepass`) no matter how much of it is live. Ink cells — capped at
//! MAX_INK_CELLS = 512, re-emitted through the full colour chain every frame,
//! INCLUDING when fully settled.
//!
//! The two full-map walks are the expensive-and-useless pair: in the steady
//! state every entry fails both predicates, and because an `FxHashMap` iterates
//! its CAPACITY the walk stays proportional to the map's high-water mark long
//! after the screen empties. `idle_settled_persist_512` minus `idle_settled_128`
//! is exactly that cost, isolated: the two workloads present the SAME 128-word
//! screen and emit the SAME (nothing), and differ only in how many episodes the
//! map is still holding.
//!
//! THE OFF NUMBERS, which are the ones most users pay:
//!
//!   off_disabled_128_words   every class gate off. `rescan` produces no
//!                            occurrences, so `tick` takes its `occ.is_empty()`
//!                            early-return (line 6006) and returns fingerprint
//!                            0. This is the floor.
//!   idle_settled_128         ON, and idle with nothing to draw: 128 settled
//!                            feline words, every peek spent, `SelfGlow` past
//!                            its window so not one InkCell is emitted. Zero
//!                            pixels, full machine — one `kitty` in the
//!                            scrollback is enough to buy this every frame.
//!
//! HOW THE STATE IS BUILT. Everything here is CPU-side and driven through the
//! PUBLIC API only (`occ`/`persist`/`novas` are private, and the in-crate
//! `bench_*` gates that poke them directly are deliberately not copied here):
//! a real `Terminal`, a real `cell_frame_into` snapshot, a real
//! `rescan_from_cells_with_geom_at_cursor`, then real `tick`s. The engine is
//! clockless — the host injects `now` — so every workload derives its instants
//! from ONE captured `Instant` and advances by a fixed dt. No wall clock is
//! ever sampled for state.
//!
//! Three setup details are load-bearing, and getting any of them wrong measures
//! an early-out instead of the engine:
//!
//!   1. GEOMETRY. `EffectGeom { cell_w: 10, cell_h: 20, .. }` clears
//!      CAT_MIN_CELL_W = 7 / CAT_MIN_CELL_H = 14. Below either floor the whole
//!      graphic arm degrades to `PawFallbackFloor` and emits nothing.
//!   2. TEXT CLEARANCE. `cat_eligible` requires `occ.cat_text_clear`, sampled at
//!      rescan by `cat_peek_plan`/`band_occupancy`. The word grids here put
//!      words on EVEN rows only, so every word has a clear band above and below;
//!      a wall of text on both sides yields zero cats.
//!   3. FIXED-POINT WARM-UP. Every workload is ticked at its target instant
//!      until two consecutive frames put the SAME picture on the glass. That
//!      drains the two-bake-per-frame cat budget (an un-warmed cat frame
//!      measures baking, not ticking), latches the one-shot flags, and grows
//!      every resident scratch — and it is what makes the timed loop, which
//!      re-ticks the same instant, honest. The IDLE workloads additionally
//!      advance the clock first (`Bed::settle`), because a `MAX_CATS` overflow
//!      arm re-latches its phase clock to `now` the moment a slot frees: at a
//!      frozen instant eight cats are reborn mid-entrance forever and the
//!      screen never settles. Real time has to pass for 128 words to spend
//!      their one-shots eight at a time.
//!
//! EMITTED VOLUME IS RECORDED, NOT JUST TIME. `verify_reaches_target` prints the
//! `(WordDecoration, InkCell, FreeSprite, GlowQuad)` lengths per workload and
//! asserts them from BOTH sides, so a later regression in COUNT stays separable
//! from a regression in per-item COST — and so no workload can silently decay
//! into measuring an idle engine. A one-sided `<=` would be satisfied by an
//! engine that emits nothing at all, which is precisely the failure this file
//! exists to prevent.
//!
//! The `settled` flag on a workload is checked by comparing the whole emitted
//! stream at `now` against the stream at `now + 16 ms`: EQUAL proves the frame
//! is output-invariant (the case where the recompute is provably wasted — audit
//! wd-3), DIFFERENT proves the workload really is animating. It is compared
//! BYTE-FOR-BYTE rather than through the fingerprint `tick` returns, because
//! that fingerprint deliberately folds the frame counter while a graphic is
//! animating — the repaint duty pin — and so changes on frames that draw
//! nothing at all. The probe is undone by re-running the fixed point, which
//! must reproduce the original stream exactly: a determinism check on the
//! whole harness, and the seed of the visual-equivalence harness this crate
//! needs before any of the audited rewrites can ship.
//!
//! WHAT EACH WORKLOAD WAS OBSERVED TO REACH. Measured once with temporary
//! counters compiled into the engine (since `occ`/`persist` are private and
//! entry counts are not observable from a `benches/` target), then removed.
//! Per ONE presented frame; `walk` is the number of episode-map entries the
//! pass iterated:
//!
//!   workload                   occ persist  stamp_spent_marks  super_prepass  other
//!   off_disabled_128_words       0       0  not called         not called     early-out
//!   idle_settled_128           128     128  1 call, walk 128   1 call, w 128  nothing emitted
//!   idle_settled_persist_512   128     512  1 call, walk 512   1 call, w 512  nothing emitted
//!   reduced_motion_128         128     128  1 call, walk 128   NOT CALLED     emit_cat x8
//!   cats_rising_8                8       8  1 call, walk 8     1 call, w 8    ease_out_back x8
//!   rainbow_ink_settled_512    128     128  1 call, walk 128   1 call, w 128  emit_rainbow_ink x128
//!   rainbow_ink_drifting_512   128     128  1 call, walk 128   1 call, w 128  emit_rainbow_ink x128
//!   supernova_peak               1       1  1 call, walk 1     1 call, w 1    peak_additive_channel x1
//!   nova_coupled_peak          128     128  1 call, walk 128   1 call, w 128  emit_nova_axis x128
//!   split_4_panes_settled     32/pn  32/pn  4 calls, walk 128  4 calls, w 128 ParkedPane::swap x8,
//!                                                                            burst_summary walk 128
//!
//! Three of those rows are the findings, sized: `idle_settled_persist_512` walks
//! 512 + 512 entries per frame to emit nothing (vs 128 + 128 for the identical
//! screen), `rainbow_ink_settled_512` runs the full colour chain 128 times for
//! bytes that provably cannot change, and `split_4_panes_settled` pays 8
//! `ParkedPane::swap` re-derivations over 128 episodes per frame — one park and
//! one unpark per pane — purely to maintain a summary that exists to make a
//! read cheap.

use std::time::Duration;

use aterm_core::render::{FreeSprite, RenderInput};
use aterm_core::selection::{SelectionSide, SelectionType, TextSelection};
use aterm_core::terminal::Terminal;
use aterm_effects::word_decorations::{
    DecoConfig, EffectGeom, ProfanityStyle, SelView, WordDecorations,
};
use aterm_lexicon::{Class, Lexicon};
use aterm_render::{GlowQuad, InkCell, WordDecoration};
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use web_time::Instant;

/// 10x20 px cells: comfortably past CAT_MIN_CELL_W = 7 / CAT_MIN_CELL_H = 14,
/// and a plausible real metric for a 14 pt monospace face on a HiDPI display.
const CELL_W: u16 = 10;
const CELL_H: u16 = 20;

/// The unsplit workloads' grid. 32 rows x 80 cols with words on even rows only
/// gives 16 word rows; 8 words per row = 128 = MAX_OCCURRENCES exactly, which
/// is the cap the per-frame walks are bounded by.
const ROWS: usize = 32;
const COLS: usize = 80;
const STRIDE: usize = 10;
const PER_ROW: usize = COLS / STRIDE;
const FULL_OCC: usize = PER_ROW * (ROWS / 2);

/// Each pane of the split workload. 4 x 8 rows = the same 32-row window.
const PANE_ROWS: usize = 8;
const PANES: usize = 4;

/// One 60 Hz-ish frame. The clock is injected, so this is the only dt in play.
const FRAME_MS: u64 = 16;

/// Ceiling on the injected time an idle workload may take to reach a steady
/// picture. 128 feline words spend their one-shots eight at a time
/// (`MAX_CATS` = 8) at up to ~4.6 s per peek, so 16 batches can genuinely need
/// well over a minute of simulated time; this is that with headroom.
const SETTLE_MAX_MS: u64 = 240_000;

/// How long the picture must hold still to count as settled, and why it is
/// this long: a peeking cat DWELLS. Between its entrance and its descent it
/// holds one pose for the whole dwell (2.2-3.6 s, plus anticipation), so a
/// short stability window declares "settled" in the middle of an animation —
/// the first draft of this bench did exactly that and timed eight dwelling
/// cats it had labelled idle. This is longer than the longest possible peek
/// (`peek_total_ms` <= ~4.6 s), so nothing that is merely holding can pass it.
const SETTLE_STABLE_MS: u64 = 8_000;
const SETTLE_STABLE_FRAMES: usize = (SETTLE_STABLE_MS / FRAME_MS) as usize;

/// `spec::RAINBOW_DRIFT_MS`. Past it the rainbow phase is pinned to exactly
/// 0.0 and `emit_rainbow_ink` produces byte-identical output forever.
const RAINBOW_DRIFT_MS: u64 = 2_500;

/// `word_decorations::CAT_RISE_MS`. Inside it `emit_cat` runs the 24-round
/// `ease_out_back` bisection (audit wd-4); outside it never does.
const CAT_RISE_MS: u64 = 450;

/// `word_decorations::MAX_INK_CELLS`. The profanity grids are sized to
/// saturate it exactly (128 occurrences x 4 lead cells).
const MAX_INK_CELLS: usize = 512;

/// `word_decorations::PERSIST_CAP`.
const PERSIST_CAP: usize = 512;

/// How far the burst workloads sweep looking for their busiest frame.
/// `SUPER_TOTAL_MS` = 2400 plus the ignition limiter's queueing slack.
const SUPER_SEARCH_MS: u64 = 5_000;

/// Ticks allowed for a workload to reach its fixed point. Generous: the
/// settled feline screen needs ~16 (MAX_CATS = 8 slots freeing 8 at a time
/// over 128 words) and the cat workloads need ~8 more to drain the
/// two-bakes-per-frame budget.
const FIXED_POINT_TICKS: usize = 256;

/// Four DISTINCT feline surfaces, used to grow `persist` to its cap through
/// the public path. Distinct `form_hash` is what makes their idents distinct
/// (`seed_of(start_col, class, form_hash)`), and therefore what makes four
/// 128-word screens 512 episodes instead of 128 re-seen ones — asserted in
/// `verify_persist_surfaces`, because a silent fold of two of these surfaces
/// would leave the map at 384 and quietly halve the finding this workload
/// exists to size. (Measured: the map really does reach 512, against 128 for
/// `idle_settled_128`; see the reach table at the top of this file.)
const FELINE_WORDS: [&str; 4] = ["kitty", "kitten", "kitties", "kittens"];

/// The profanity surface. Four columns wide, so 128 occurrences capture
/// 4 x 128 = 512 = MAX_INK_CELLS lead cells exactly.
const PROFANE_WORD: &str = "fuck";

/// Filler with no lexicon match in any family, for the scan-path OFF leg.
const PLAIN_ROW: &str = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod";

// ---------------------------------------------------------------------------
// Emitted volume
// ---------------------------------------------------------------------------

/// The four output scratch lengths of one presented frame. Recorded per
/// workload so a regression in COUNT stays separable from a regression in
/// per-item COST.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Volume {
    /// `WordDecoration` sparkle quads.
    deco: usize,
    /// `InkCell` foreground overrides.
    ink: usize,
    /// `FreeSprite` cat/animal/nuke sprites.
    free: usize,
    /// `GlowQuad` additive nova/supernova light.
    nova: usize,
}

impl Volume {
    fn total(self) -> usize {
        self.deco + self.ink + self.free + self.nova
    }

    fn add(&mut self, o: Volume) {
        self.deco += o.deco;
        self.ink += o.ink;
        self.free += o.free;
        self.nova += o.nova;
    }
}

impl std::fmt::Display for Volume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "deco {:>4}  ink {:>4}  free {:>4}  nova {:>5}",
            self.deco, self.ink, self.free, self.nova
        )
    }
}

/// Everything one presented frame actually put on the glass, byte-for-byte.
///
/// THE FINGERPRINT `tick` RETURNS CANNOT BE USED FOR THIS. It deliberately
/// folds the frame counter (`fp ^ frame.wrapping_mul(0x9E37_79B1)`, e.g.
/// word_decorations.rs:8663) whenever a graphic is animating, INCLUDING on
/// frames that emit nothing — that is the repaint duty pin, and it is a
/// feature: it keeps the host waking while a sprite is mid-entrance. It also
/// means "the fingerprint changed" proves nothing about the picture. All four
/// overlay types are `Copy + Eq` precisely so the damage cache can compare them
/// exactly, so the bench compares the real thing instead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Emitted {
    deco: Vec<WordDecoration>,
    ink: Vec<InkCell>,
    free: Vec<FreeSprite>,
    nova: Vec<GlowQuad>,
}

impl Emitted {
    fn volume(&self) -> Volume {
        Volume {
            deco: self.deco.len(),
            ink: self.ink.len(),
            free: self.free.len(),
            nova: self.nova.len(),
        }
    }

    fn clear(&mut self) {
        self.deco.clear();
        self.ink.clear();
        self.free.clear();
        self.nova.clear();
    }
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// One fully-built engine parked at the exact instant its workload measures.
struct Bed {
    wd: WordDecorations,
    cfg: DecoConfig,
    lex: Lexicon,
    geom: EffectGeom,
    /// The visible snapshot, kept so the rescan group can re-drive the damage
    /// path against the same screen the tick group ticks.
    screen: RenderInput,
    rows: usize,
    cols: usize,
    epoch: u64,
    /// The single captured instant every other instant is derived from.
    t0: Instant,
    /// The instant the timed frame runs at.
    now: Instant,
    sel: Option<TextSelection>,
    /// Pane keys for the composed workload; empty on the unsplit path.
    panes: Vec<u64>,
    /// What this same engine emitted EARLY in its life, before it settled.
    /// The other half of every "emits nothing" assertion: it proves the screen
    /// is genuinely decorable and the zero is the effect's doing, not an empty
    /// grid's.
    witness: Volume,
    deco_out: Vec<WordDecoration>,
    ink_out: Vec<InkCell>,
    free_out: Vec<FreeSprite>,
    nova_out: Vec<GlowQuad>,
}

impl Bed {
    /// A fresh engine over `screen`, rescanned once at `t0`.
    fn new(cfg: DecoConfig, screen: RenderInput, rows: usize, cols: usize) -> Bed {
        let t0 = Instant::now();
        let mut bed = Bed {
            wd: WordDecorations::default(),
            cfg,
            lex: Lexicon::with_languages(&["en"]),
            geom: EffectGeom {
                cell_w: CELL_W,
                cell_h: CELL_H,
                rows: rows as u16,
                cols: cols as u16,
            },
            screen,
            rows,
            cols,
            epoch: 0,
            t0,
            now: t0,
            sel: None,
            panes: Vec::new(),
            witness: Volume::default(),
            deco_out: Vec::new(),
            ink_out: Vec::new(),
            free_out: Vec::new(),
            nova_out: Vec::new(),
        };
        bed.rescan();
        bed
    }

    fn at(&mut self, ms: u64) {
        self.now = self.t0 + Duration::from_millis(ms);
    }

    /// Drive the damage path over the currently-held snapshot at `self.now`.
    fn rescan(&mut self) {
        self.epoch += 1;
        self.wd.rescan_from_cells_with_geom_at_cursor(
            &self.screen.cells,
            &self.screen.line_sizes,
            self.rows,
            self.cols,
            &self.lex,
            &self.cfg,
            self.epoch,
            self.now,
            self.geom,
            0,
            None,
        );
    }

    /// One `tick`, exactly as app_render calls it.
    fn tick_once(&mut self) -> u64 {
        let sv = self.sel.as_ref().map(|sel| SelView {
            sel,
            display_offset: 0,
        });
        self.wd.tick(
            self.now,
            &self.cfg,
            self.geom,
            None,
            sv,
            true,
            &mut self.deco_out,
            &mut self.ink_out,
            &mut self.free_out,
            &mut self.nova_out,
        )
    }

    fn volume(&self) -> Volume {
        Volume {
            deco: self.deco_out.len(),
            ink: self.ink_out.len(),
            free: self.free_out.len(),
            nova: self.nova_out.len(),
        }
    }

    /// ONE PRESENTED FRAME — the unit the host actually pays for.
    ///
    /// Unsplit: `begin_host_frame` + one `tick`. Composed: `begin_host_frame` +
    /// `retain_panes` + `bind_pane`/`tick` per pane, which is the shape
    /// app_render.rs:16422-16549 drives and the only shape that reaches
    /// `ParkedPane::swap` -> `burst_summary` (audit wd-6).
    fn frame(&mut self) -> (u64, Volume) {
        self.frame_inner(None)
    }

    /// `frame` with the emitted stream copied out. Used only by the setup and
    /// verification paths — the timed loop takes the `None` arm.
    fn frame_capturing(&mut self, cap: &mut Emitted) -> (u64, Volume) {
        cap.clear();
        self.frame_inner(Some(cap))
    }

    fn frame_inner(&mut self, mut cap: Option<&mut Emitted>) -> (u64, Volume) {
        self.wd.begin_host_frame();
        if self.panes.is_empty() {
            let fp = self.tick_once();
            if let Some(cap) = cap.as_deref_mut() {
                self.capture_into(cap);
            }
            return (fp, self.volume());
        }
        // Moved out and back so the per-pane loop can hold `&mut self.wd`.
        // A `Vec` move is a pointer swap — no allocation inside the timed loop.
        let keys = std::mem::take(&mut self.panes);
        self.wd.retain_panes(|k| keys.contains(&k));
        let mut fp: u64 = 0;
        let mut vol = Volume::default();
        for (i, key) in keys.iter().enumerate() {
            let origin = (0, (i as i32) * (PANE_ROWS as i32) * i32::from(CELL_H));
            self.wd.bind_pane(*key, origin);
            fp = fp.wrapping_mul(0x0000_0100_0000_01B3) ^ self.tick_once();
            vol.add(self.volume());
            if let Some(cap) = cap.as_deref_mut() {
                self.capture_into(cap);
            }
        }
        self.panes = keys;
        (fp, vol)
    }

    fn capture_into(&self, cap: &mut Emitted) {
        cap.deco.extend_from_slice(&self.deco_out);
        cap.ink.extend_from_slice(&self.ink_out);
        cap.free.extend_from_slice(&self.free_out);
        cap.nova.extend_from_slice(&self.nova_out);
    }

    /// Tick at `self.now` until two consecutive frames put the SAME picture on
    /// the glass, so the timed loop re-measures a genuine fixed point rather
    /// than a state still converging (cat slots freeing as peeks complete, the
    /// two-bakes-per-frame budget draining, one-shot flags latching).
    fn fixed_point(&mut self) -> Emitted {
        let mut prev = Emitted::default();
        let mut cur = Emitted::default();
        self.frame_capturing(&mut prev);
        for _ in 0..FIXED_POINT_TICKS {
            self.frame_capturing(&mut cur);
            if cur == prev {
                return cur;
            }
            std::mem::swap(&mut prev, &mut cur);
        }
        panic!(
            "no fixed point after {FIXED_POINT_TICKS} ticks at one instant \
             (last two volumes: {} then {}) — the workload is not in a steady \
             state and its timing would be meaningless",
            prev.volume(),
            cur.volume()
        );
    }

    /// ADVANCE the injected clock until the engine draws the same picture on
    /// every frame — the honest definition of "idle".
    ///
    /// This cannot be done by parking at a late instant and ticking: an
    /// occurrence that took the `PawFallbackOverflow` arm is re-decided every
    /// tick and, the moment a `MAX_CATS` slot frees, upgrades to `Cat` with
    /// `phase_start` RE-LATCHED to `now` (word_decorations.rs:6757, the
    /// "upgraded fallback plays from the top" rule). At a frozen instant those
    /// eight cats are reborn mid-entrance forever and the screen never settles.
    /// Real time has to pass, eight cats at a time, for all 128 words to spend
    /// their one-shot.
    fn settle(&mut self, max_ms: u64) -> u64 {
        let mut prev = Emitted::default();
        let mut cur = Emitted::default();
        let mut stable = 0usize;
        let mut ms = 0;
        self.frame_capturing(&mut prev);
        while ms <= max_ms {
            self.at(ms);
            self.frame_capturing(&mut cur);
            stable = if cur == prev { stable + 1 } else { 0 };
            if stable >= SETTLE_STABLE_FRAMES {
                return ms;
            }
            std::mem::swap(&mut prev, &mut cur);
            ms += FRAME_MS;
        }
        panic!(
            "still animating after {max_ms} ms of injected time (last frame {}) \
             — the idle workloads must reach a steady picture or they measure a \
             half-played animation",
            prev.volume()
        );
    }

    /// Drive frames from `t0` for `span_ms` and return the offset of the
    /// busiest one. Used by the burst workloads, whose ignition instant is
    /// granted by the flash limiter (`IGNITION_WINDOW` = 1 s,
    /// `MAX_RECENT_IGNITIONS` = 2) and therefore cannot be predicted from the
    /// outside — hard-coding a phase there is exactly how a workload ends up
    /// timing an empty window.
    fn busiest_offset_ms(&mut self, span_ms: u64) -> u64 {
        let mut best = (0usize, 0u64);
        let mut ms = 0;
        while ms <= span_ms {
            self.at(ms);
            let (_, vol) = self.frame();
            if vol.total() > best.0 {
                best = (vol.total(), ms);
            }
            ms += FRAME_MS;
        }
        best.1
    }
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

/// Render `text` into a headless `Terminal` and take the snapshot the render
/// path scans (`cell_frame_into` — the same rows the GUI hands the rescan).
fn snapshot(text: &[u8], rows: usize, cols: usize) -> RenderInput {
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(text);
    let mut input = RenderInput::empty();
    term.cell_frame_into(&mut input, rows, cols);
    input
}

/// `word` laid out `PER_ROW` times on every EVEN row, blank rows between.
///
/// The blank rows are not decoration: `cat_peek_plan` rejects a word whose
/// band above AND below is occupied past `CAT_MAX_BAND_OCCUPANCY`, so a solid
/// wall of text sets `cat_text_clear = false` on every occurrence and the
/// entire graphic arm silently becomes `PawFallbackFloor`.
fn word_grid(word: &str, rows: usize) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(rows * (COLS + 8));
    for r in (0..rows).step_by(2) {
        let _ = write!(out, "\x1b[{};1H", r + 1);
        let mut row = String::with_capacity(COLS);
        for _ in 0..PER_ROW {
            let _ = write!(row, "{word:<width$}", width = STRIDE);
        }
        // Trimmed so the last column never arms DECAWM's pending wrap.
        out.push_str(row.trim_end());
    }
    out.into_bytes()
}

/// A screen with no lexicon match anywhere.
fn plain_grid(rows: usize) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(rows * (COLS + 8));
    for r in 0..rows {
        let _ = write!(
            out,
            "\x1b[{};1H{}",
            r + 1,
            &PLAIN_ROW[..PLAIN_ROW.len().min(COLS)]
        );
    }
    out.into_bytes()
}

/// One simple selection over `[c0, c1]` of `row`.
fn select_row(row: i32, c0: u16, c1: u16) -> TextSelection {
    let mut s = TextSelection::new();
    s.start_selection(row, c0, SelectionSide::Left, SelectionType::Simple);
    s.update_selection(row, c1, SelectionSide::Right);
    s.complete_selection();
    s
}

// ---------------------------------------------------------------------------
// Configs
// ---------------------------------------------------------------------------

/// The native launch defaults — what actually ships when a user turns the
/// feature on without setting a knob.
fn cfg_default() -> DecoConfig {
    DecoConfig::default()
}

/// Every category gate off: the effect as most users run it.
fn cfg_off() -> DecoConfig {
    DecoConfig {
        profanity: false,
        feline: false,
        canine: false,
        orca: false,
        emphasis: false,
        ink_enabled: false,
        ..DecoConfig::default()
    }
}

/// Rainbow ink with the supernova roll disabled, so the ink workloads measure
/// `emit_rainbow_ink` alone and no burst arm contaminates them.
fn cfg_rainbow_only() -> DecoConfig {
    DecoConfig {
        profanity_style: ProfanityStyle::Rainbow,
        supernova_chance: 0,
        ..DecoConfig::default()
    }
}

/// Every rolled rainbow episode escalates: the only way to reach
/// `super_prepass`'s grant path and `emit_super_axis` deterministically.
fn cfg_supernova() -> DecoConfig {
    DecoConfig {
        profanity_style: ProfanityStyle::Rainbow,
        supernova_chance: 100,
        ..DecoConfig::default()
    }
}

/// The classic nova — the only style that reaches `emit_nova_axis` and the
/// per-nova `dist_scratch` sort in `nova_prepass`.
fn cfg_nova() -> DecoConfig {
    DecoConfig {
        profanity_style: ProfanityStyle::Nova,
        ..DecoConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Workload builders
// ---------------------------------------------------------------------------

/// 128 feline words, every peek spent, `SelfGlow` past its window.
///
/// The witness frame at +100 ms is taken FIRST, on this same engine, so the
/// "emits nothing" assertion below is anchored: the screen provably decorates.
fn build_idle_settled() -> Bed {
    let mut bed = Bed::new(
        cfg_default(),
        snapshot(&word_grid(FELINE_WORDS[0], ROWS), ROWS, COLS),
        ROWS,
        COLS,
    );
    bed.at(100);
    let (_, vol) = bed.frame();
    bed.witness = vol;
    bed.settle(SETTLE_MAX_MS);
    bed
}

/// `idle_settled_128` with the episode map grown to PERSIST_CAP.
///
/// Four screens of four DISTINCT feline surfaces are scrolled through inside
/// the setup region (128 distinct idents each), all within the 10 s grace TTL
/// so none are swept, and then the FIRST screen is rescanned so the visible set
/// is byte-identical to `idle_settled_128`. The delta between the two workloads
/// is therefore exactly the two unconditional full-map walks
/// (`stamp_spent_marks` and `super_prepass`) plus `prune_ignitions` — nothing
/// that scales with what is on screen, because what is on screen is the same.
fn build_idle_settled_persist() -> Bed {
    let screens: Vec<RenderInput> = FELINE_WORDS
        .iter()
        .map(|w| snapshot(&word_grid(w, ROWS), ROWS, COLS))
        .collect();
    let mut bed = Bed::new(cfg_default(), screens[0].clone(), ROWS, COLS);
    // The witness comes from the FIRST screen at +100 ms, exactly as
    // `idle_settled_128` takes it — so the two workloads are anchored by the
    // same evidence that their shared visible screen decorates.
    bed.at(100);
    let (_, vol) = bed.frame();
    bed.witness = vol;
    // Screens 1..4 pass through, each leaving 128 grace-resident episodes.
    // Every step stays well inside GRACE_TTL = 10 s, so `rescan_end`'s sweep
    // retains all of them and the map climbs 128 -> 256 -> 384 -> 512.
    for (i, screen) in screens.iter().enumerate().skip(1) {
        bed.at(100 * (i as u64 + 1));
        bed.frame();
        bed.screen = screen.clone();
        bed.rescan();
    }
    bed.at(100 * (screens.len() as u64 + 1));
    bed.frame();
    // Back to the visible screen of `idle_settled_128`.
    bed.screen = screens[0].clone();
    bed.rescan();
    bed.settle(SETTLE_MAX_MS);
    bed
}

/// `idle_settled_128` under the W11b unfocused / accessibility demotion, where
/// `super_prepass` and `nova_prepass` early-out and the graphic derivation is
/// skipped. The floor the idle case should be approaching.
fn build_reduced_motion() -> Bed {
    let mut bed = Bed::new(
        DecoConfig {
            reduced_motion: true,
            ..DecoConfig::default()
        },
        snapshot(&word_grid(FELINE_WORDS[0], ROWS), ROWS, COLS),
        ROWS,
        COLS,
    );
    bed.at(100);
    let (_, vol) = bed.frame();
    bed.witness = vol;
    bed.settle(SETTLE_MAX_MS);
    bed
}

/// Every category gate off over the SAME 128-word screen: `tick` takes its
/// `occ.is_empty()` early-return. The floor.
fn build_off_disabled() -> Bed {
    let mut bed = Bed::new(
        cfg_off(),
        snapshot(&word_grid(FELINE_WORDS[0], ROWS), ROWS, COLS),
        ROWS,
        COLS,
    );
    // Witness: the same screen under the DEFAULT config really does decorate,
    // so "all zero" here is the config gate and not an empty grid.
    let mut live = Bed::new(
        cfg_default(),
        snapshot(&word_grid(FELINE_WORDS[0], ROWS), ROWS, COLS),
        ROWS,
        COLS,
    );
    live.at(100);
    let (_, vol) = live.frame();
    bed.witness = vol;
    bed.settle(SETTLE_MAX_MS);
    bed
}

/// Eight feline words mid-rise (`t < CAT_RISE_MS`): the ON-and-animating
/// graphic path — `cat_geometry`, the `BakeKeyV4` lookup, `push_cat_free`, and
/// `ease_out_back`'s 24-round bisection (audit wd-4), which is reached from
/// nowhere else.
///
/// Exactly `MAX_CATS` words, on one row band, so no occurrence takes the
/// overflow fallback and all eight really draw.
fn build_cats_rising() -> Bed {
    let text = word_grid(FELINE_WORDS[0], 2);
    let mut bed = Bed::new(cfg_default(), snapshot(&text, ROWS, COLS), ROWS, COLS);
    bed.at(CAT_RISE_MS / 2);
    bed.witness = bed.fixed_point().volume();
    bed
}

/// 128 profanity words, ink saturated at MAX_INK_CELLS, ticked PAST the
/// rainbow drift window.
///
/// This is the case audit wd-3 is about: the phase is pinned to exactly 0.0,
/// the emitted bytes cannot change, and `emit_rainbow_ink` still runs its
/// legibility guard (up to 9 passes x 4 `hsv2rgb`) plus a per-lead-cell
/// `hue_at` for all 512 cells, every frame, forever.
fn build_rainbow_settled() -> Bed {
    let mut bed = Bed::new(
        cfg_rainbow_only(),
        snapshot(&word_grid(PROFANE_WORD, ROWS), ROWS, COLS),
        ROWS,
        COLS,
    );
    // Four drift windows in: the phase is pinned to exactly 0.0 and stays there
    // for as long as the word is on screen.
    bed.at(RAINBOW_DRIFT_MS * 4);
    bed.witness = bed.fixed_point().volume();
    bed
}

/// The same screen mid-drift: identical emitted volume, live phase.
fn build_rainbow_drifting() -> Bed {
    let mut bed = Bed::new(
        cfg_rainbow_only(),
        snapshot(&word_grid(PROFANE_WORD, ROWS), ROWS, COLS),
        ROWS,
        COLS,
    );
    bed.at(RAINBOW_DRIFT_MS / 2);
    bed.witness = bed.fixed_point().volume();
    bed
}

/// One rolled supernova, parked on its busiest frame, with a selection inside
/// the blast radius but OFF the word's span.
///
/// The selection placement is the whole trick: a selection ON the span defers
/// the ignition outright (§6.4), so it would measure a window that never opens;
/// a selection four rows above splits the emitted quads through
/// `split_super_selection` instead, which is the path worth timing. This is
/// also the only workload that reaches `supernova::emit_super` and therefore
/// `bound_additive_overlap` -> `peak_additive_channel` (cursor-family CF-1:
/// a fresh `Vec` collect, a sort, and a per-row cubic probe on every frame of
/// the ~950 ms shock window).
fn build_supernova() -> Bed {
    let word_row = ROWS / 2;
    let make = |col: usize| {
        use std::fmt::Write as _;
        let mut text = String::new();
        let _ = write!(text, "\x1b[{};{}H{PROFANE_WORD}", word_row + 1, col + 1);
        let mut bed = Bed::new(
            cfg_supernova(),
            snapshot(text.as_bytes(), ROWS, COLS),
            ROWS,
            COLS,
        );
        bed.sel = Some(select_row((word_row - 4) as i32, 0, (COLS - 1) as u16));
        bed
    };
    // THE TIER IS A GENOME ROLL, and the genome is a function of the word's
    // COLUMN (`seed_of(start_col, class, form_hash)`). `supernova_chance: 100`
    // guarantees an escalation but not WHICH degree, so pinning a single
    // hard-coded column would silently measure whichever tier that column
    // happened to draw — the audit's own caveat about this module. Sweep a
    // handful of placements and keep the one whose window actually gets
    // biggest, which is where `peak_additive_channel`'s per-row cubic bites.
    let placements = [8usize, 18, 28, 38, 48, 58, 68];
    let best = placements
        .into_iter()
        .map(|col| {
            let mut probe = make(col);
            let ms = probe.busiest_offset_ms(SUPER_SEARCH_MS);
            probe.at(ms);
            let vol = probe.fixed_point().volume();
            (vol.nova, col, ms)
        })
        .max()
        .expect("a non-empty placement sweep");
    let (_, col, peak_ms) = best;
    let mut bed = make(col);
    // Re-drive frame by frame to the chosen instant: the flash limiter grants
    // the ignition off the frame history, so jumping straight there would land
    // in a different state than the sweep measured. Deterministic by
    // construction (injected clock, genome frozen at birth).
    let mut ms = 0;
    while ms < peak_ms {
        bed.at(ms);
        bed.frame();
        ms += FRAME_MS;
    }
    bed.at(peak_ms);
    bed.witness = bed.fixed_point().volume();
    bed
}

/// Classic novas over a screen of 128 ink-bearing neighbours, parked on the
/// busiest frame.
///
/// Reaches `nova_prepass`'s per-live-nova `dist_scratch.sort_unstable()` over
/// ~128 candidates of which only MAX_COUPLING_WORDS = 16 are ever read, plus
/// `ink_fx`'s per-occurrence `nova_features`/`hue_nudge` recompute and
/// `emit_nova_axis`.
fn build_nova_coupled() -> Bed {
    let text = word_grid(PROFANE_WORD, ROWS);
    let make = || Bed::new(cfg_nova(), snapshot(&text, ROWS, COLS), ROWS, COLS);
    let peak_ms = make().busiest_offset_ms(SUPER_SEARCH_MS);
    let mut bed = make();
    let mut ms = 0;
    while ms < peak_ms {
        bed.at(ms);
        bed.frame();
        ms += FRAME_MS;
    }
    bed.at(peak_ms);
    bed.witness = bed.fixed_point().volume();
    bed
}

/// Four panes, each with its own settled episode map, driven the way the
/// composed host drives them: ONE `begin_host_frame` + `retain_panes`, then
/// `bind_pane`/`tick` per pane.
///
/// The only workload that reaches `ParkedPane::swap` -> `burst_summary`
/// (audit wd-6), which re-derives the parked burst summary by walking the whole
/// episode map it just took ownership of — once per pane per presented frame,
/// with the effect idle and nothing to draw.
fn build_split_panes() -> Bed {
    let text = word_grid(FELINE_WORDS[0], PANE_ROWS);
    let mut bed = Bed::new(
        cfg_default(),
        snapshot(&text, PANE_ROWS, COLS),
        PANE_ROWS,
        COLS,
    );
    let keys: Vec<u64> = (0..PANES as u64).map(|i| 0xC0FF_EE00 + i).collect();
    // Give every pane its own episode map through the real binding seam.
    for (i, key) in keys.iter().enumerate() {
        bed.wd.begin_host_frame();
        bed.wd.bind_pane(
            *key,
            (0, (i as i32) * (PANE_ROWS as i32) * i32::from(CELL_H)),
        );
        // Birth at t0 and FIRST TICK at +100 ms, not both at the same instant:
        // an episode ticked at exactly its own birth instant is at phase 0, so
        // the cat has zero height and the glow has zero lift, and the frame
        // emits nothing at all. The witness would then be empty and every
        // "emits nothing" assertion on this workload would be vacuous.
        bed.at(0);
        bed.rescan();
        bed.at(100);
        bed.tick_once();
    }
    bed.panes = keys;
    bed.at(100);
    let (_, vol) = bed.frame();
    bed.witness = vol;
    bed.settle(SETTLE_MAX_MS);
    bed
}

/// The damage path over the settled 128-word feline screen: the entry the host
/// takes whenever `needs_rescan(epoch)` is true, which during scrolling output
/// is every frame. Instrumented alongside the frame path so a tick-side win
/// that merely pushes work into the scan is visible.
fn build_rescan_feline() -> Bed {
    let mut bed = Bed::new(
        cfg_default(),
        snapshot(&word_grid(FELINE_WORDS[0], ROWS), ROWS, COLS),
        ROWS,
        COLS,
    );
    bed.at(100);
    let (_, vol) = bed.frame();
    bed.witness = vol;
    // Warm the scan memo, the alignment plane and the ink capture buffers.
    for _ in 0..8 {
        bed.rescan();
    }
    bed
}

/// The scan path's OFF leg: a full screen with no match in any family.
fn build_rescan_plain() -> Bed {
    let mut bed = Bed::new(
        cfg_default(),
        snapshot(&plain_grid(ROWS), ROWS, COLS),
        ROWS,
        COLS,
    );
    bed.at(100);
    let (_, vol) = bed.frame();
    bed.witness = vol;
    for _ in 0..8 {
        bed.rescan();
    }
    bed
}

// ---------------------------------------------------------------------------
// Targets
// ---------------------------------------------------------------------------

/// Inclusive `[lo, hi]` bound on an emitted count. BOTH sides always: a lone
/// `<=` is satisfied by an engine that emits nothing, which is exactly the
/// silent decay this file has to be immune to.
type Bound = (usize, usize);

struct Expect {
    deco: Bound,
    ink: Bound,
    free: Bound,
    nova: Bound,
    /// `true` when `tick` must take an early-return (fingerprint exactly 0);
    /// `false` when the machine must have run (fingerprint non-zero).
    early_out: bool,
    /// `Some(true)`: the frame must be output-invariant across a 16 ms step
    /// (the wasted-recompute case). `Some(false)`: it must be animating.
    /// `None`: not asserted (the split fold is per-pane and not phase-pure).
    settled: Option<bool>,
    /// Lower bound on what this engine emitted while still animating — the
    /// proof that a workload asserting "emits nothing" is looking at a live
    /// screen rather than an empty one.
    witness_min: usize,
}

struct Workload {
    name: &'static str,
    build: fn() -> Bed,
    expect: Expect,
}

fn tick_workloads() -> Vec<Workload> {
    vec![
        Workload {
            name: "off_disabled_128_words",
            build: build_off_disabled,
            expect: Expect {
                deco: (0, 0),
                ink: (0, 0),
                free: (0, 0),
                nova: (0, 0),
                early_out: true,
                settled: Some(true),
                witness_min: 1,
            },
        },
        Workload {
            name: "idle_settled_128",
            build: build_idle_settled,
            expect: Expect {
                deco: (0, 0),
                ink: (0, 0),
                free: (0, 0),
                nova: (0, 0),
                early_out: false,
                settled: Some(true),
                witness_min: 1,
            },
        },
        Workload {
            name: "idle_settled_persist_512",
            build: build_idle_settled_persist,
            expect: Expect {
                deco: (0, 0),
                ink: (0, 0),
                free: (0, 0),
                nova: (0, 0),
                early_out: false,
                settled: Some(true),
                witness_min: 1,
            },
        },
        Workload {
            name: "reduced_motion_128",
            build: build_reduced_motion,
            expect: Expect {
                // NOT zero, unlike every other idle leg. Reduced motion latches
                // the Cat arm but never advances a one-shot flag, so the eight
                // `MAX_CATS` slots hold ONE STATIC POSE each, forever — the
                // arm is decided outside the `reduced_motion` guard while the
                // flags that would retire it are inside it. So the accessibility
                // path is cheaper per frame but never reaches the empty steady
                // state the normal path does; that is a real behavioural
                // difference this bench should keep visible, not paper over.
                deco: (0, 0),
                ink: (0, 0),
                free: (1, 64),
                nova: (0, 0),
                early_out: false,
                settled: Some(true),
                witness_min: 1,
            },
        },
        Workload {
            name: "cats_rising_8",
            build: build_cats_rising,
            expect: Expect {
                deco: (0, 0),
                ink: (1, MAX_INK_CELLS),
                free: (4, 64),
                nova: (0, 0),
                early_out: false,
                settled: Some(false),
                witness_min: 4,
            },
        },
        Workload {
            name: "rainbow_ink_settled_512",
            build: build_rainbow_settled,
            expect: Expect {
                deco: (0, 0),
                ink: (MAX_INK_CELLS - 8, MAX_INK_CELLS),
                free: (0, 0),
                nova: (0, 0),
                early_out: false,
                settled: Some(true),
                witness_min: MAX_INK_CELLS - 8,
            },
        },
        Workload {
            name: "rainbow_ink_drifting_512",
            build: build_rainbow_drifting,
            expect: Expect {
                deco: (0, 0),
                ink: (MAX_INK_CELLS - 8, MAX_INK_CELLS),
                free: (0, 0),
                nova: (0, 0),
                early_out: false,
                settled: Some(false),
                witness_min: MAX_INK_CELLS - 8,
            },
        },
        Workload {
            name: "supernova_peak",
            build: build_supernova,
            expect: Expect {
                // `nova` is the load-bearing bound: it is the buffer
                // `bound_additive_overlap` sorts and probes, so a floor here is
                // the external witness that `supernova::emit_super` ran at all.
                deco: (1, 4_096),
                ink: (1, 64),
                free: (0, 64),
                nova: (32, 4_096),
                early_out: false,
                settled: Some(false),
                witness_min: 32,
            },
        },
        Workload {
            name: "nova_coupled_peak",
            build: build_nova_coupled,
            expect: Expect {
                deco: (0, 4_096),
                ink: (1, MAX_INK_CELLS),
                free: (0, 0),
                nova: (32, 4_096),
                early_out: false,
                settled: Some(false),
                witness_min: 32,
            },
        },
        Workload {
            name: "split_4_panes_settled",
            build: build_split_panes,
            expect: Expect {
                deco: (0, 0),
                ink: (0, 0),
                free: (0, 0),
                nova: (0, 0),
                early_out: false,
                settled: None,
                witness_min: 1,
            },
        },
    ]
}

fn rescan_workloads() -> Vec<Workload> {
    vec![
        Workload {
            name: "feline_128_all_hit",
            build: build_rescan_feline,
            expect: Expect {
                deco: (0, 0),
                ink: (1, MAX_INK_CELLS),
                free: (0, 64),
                nova: (0, 0),
                early_out: false,
                settled: None,
                witness_min: 1,
            },
        },
        Workload {
            name: "plain_no_matches",
            build: build_rescan_plain,
            expect: Expect {
                deco: (0, 0),
                ink: (0, 0),
                free: (0, 0),
                nova: (0, 0),
                early_out: true,
                settled: None,
                witness_min: 0,
            },
        },
    ]
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

fn in_bound(v: usize, (lo, hi): Bound) -> bool {
    v >= lo && v <= hi
}

/// The four feline surfaces `idle_settled_persist_512` grows the map with must
/// be four DISTINCT lexicon forms of the FELINE class, or that workload holds
/// fewer episodes than it claims and quietly under-sizes the finding.
fn verify_persist_surfaces() {
    let lex = Lexicon::with_languages(&["en"]);
    let cfg = cfg_default();
    let opts = cfg.scan_opts();
    let mut hashes = Vec::new();
    for w in FELINE_WORDS {
        let ms = lex.scan(w, &opts);
        assert_eq!(
            ms.len(),
            1,
            "`{w}` must be exactly one lexicon match; the persist-growth screens \
             assume one occurrence per slot"
        );
        assert_eq!(
            ms[0].class,
            Class::Feline,
            "`{w}` must be Feline — a non-feline surface takes no graphic arm \
             and the workload would stop reaching the cat path"
        );
        hashes.push(ms[0].form_hash);
    }
    hashes.sort_unstable();
    let before = hashes.len();
    hashes.dedup();
    assert_eq!(
        hashes.len(),
        before,
        "two of {FELINE_WORDS:?} fold to the same lexicon form, so their \
         occurrences share `seed_of(start_col, class, form_hash)` and the four \
         setup screens produce fewer than {} distinct episodes — \
         `idle_settled_persist_512` would not grow `persist` to PERSIST_CAP = \
         {PERSIST_CAP}",
        4 * FULL_OCC
    );
}

/// Prove each workload really is in the state it claims BEFORE it is timed, and
/// record the emitted volume it is timed at.
fn verify_reaches_target(w: &Workload, bed: &mut Bed) -> Volume {
    let steady = bed.fixed_point();
    let vol = steady.volume();
    let (fp, fp_vol) = bed.frame();
    // Printed BEFORE the assertions so a failing run still shows the numbers
    // that explain why.
    println!(
        "reaches  {:<26} {vol}  fp {fp:#018x}  witness {}",
        w.name, bed.witness
    );
    assert_eq!(
        fp_vol, vol,
        "{}: the emitted volume moved between two ticks of the same instant \
         after the fixed point was declared",
        w.name
    );

    assert_eq!(
        fp == 0,
        w.expect.early_out,
        "{}: fingerprint {fp:#x} — `tick` {} the early-return it is supposed to \
         take. A fingerprint of exactly 0 is `tick`'s only \"I did nothing\" \
         answer (`frozen_at` / `occ.is_empty()`), so this is the one bit that \
         separates \"the engine ran and emitted nothing\" from \"the engine \
         never started\".",
        w.name,
        if w.expect.early_out { "missed" } else { "took" }
    );

    assert!(
        in_bound(vol.deco, w.expect.deco)
            && in_bound(vol.ink, w.expect.ink)
            && in_bound(vol.free, w.expect.free)
            && in_bound(vol.nova, w.expect.nova),
        "{}: emitted {vol}, outside the expected bounds deco {:?} ink {:?} \
         free {:?} nova {:?}. Both sides are asserted deliberately: a one-sided \
         upper bound is satisfied by an engine emitting nothing, which is how a \
         workload silently stops reaching the code it names.",
        w.name,
        w.expect.deco,
        w.expect.ink,
        w.expect.free,
        w.expect.nova
    );

    assert!(
        bed.witness.total() >= w.expect.witness_min,
        "{}: this engine only ever emitted {} items while animating (needed \
         >= {}), so its screen is not decorable and every \"emits nothing\" \
         assertion above is vacuous",
        w.name,
        bed.witness.total(),
        w.expect.witness_min
    );

    if let Some(settled) = w.expect.settled {
        // Step one frame and come back. The engine is clockless, so this is a
        // pure function of the injected instant and the round trip must be exact.
        let base = bed.now;
        let mut stepped = Emitted::default();
        bed.now = base + Duration::from_millis(FRAME_MS);
        bed.frame_capturing(&mut stepped);
        bed.now = base;
        let again = bed.fixed_point();
        assert_eq!(
            again, steady,
            "{}: stepping one frame and returning did not restore the picture — \
             the workload is not a fixed point and its timing is not reproducible",
            w.name
        );
        if settled {
            assert_eq!(
                stepped, steady,
                "{}: the emitted stream CHANGES across {FRAME_MS} ms, so this is \
                 not the settled, output-invariant case the workload is timing \
                 (that case is the whole point: the same bytes, recomputed in \
                 full, every frame, forever)",
                w.name
            );
        } else {
            assert!(
                stepped != steady,
                "{}: the emitted stream is IDENTICAL across {FRAME_MS} ms — this \
                 workload must be ANIMATING, and a static one measures the wrong \
                 arm entirely (no rise, no drift, no burst window). Note the \
                 returned fingerprint cannot be used here: it folds the frame \
                 counter as a repaint duty pin and changes even when the picture \
                 does not.",
                w.name
            );
        }
    }

    vol
}

// ---------------------------------------------------------------------------
// Benches
// ---------------------------------------------------------------------------

fn word_decorations_tick(c: &mut Criterion) {
    verify_persist_surfaces();

    let mut group = c.benchmark_group("word_decorations_tick");
    for w in tick_workloads() {
        let mut bed = (w.build)();
        verify_reaches_target(&w, &mut bed);
        group.bench_with_input(BenchmarkId::from_parameter(w.name), &w.name, |b, _| {
            b.iter(|| {
                let (fp, vol) = bed.frame();
                black_box(fp);
                black_box(vol);
            });
        });
    }
    group.finish();
}

fn word_decorations_rescan(c: &mut Criterion) {
    let mut group = c.benchmark_group("word_decorations_rescan");
    for w in rescan_workloads() {
        let mut bed = (w.build)();
        verify_reaches_target(&w, &mut bed);
        group.bench_with_input(BenchmarkId::from_parameter(w.name), &w.name, |b, _| {
            b.iter(|| {
                bed.rescan();
                black_box(&bed.wd);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, word_decorations_tick, word_decorations_rescan);
criterion_main!(benches);
