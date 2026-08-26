// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// PHOSPHOR (matrix rain) per-presented-frame cost.
//
// WHY THIS EXISTS: the rain runs in TWO halves per present, and the tree only
// ever priced them apart, in states chosen to make each half look cheap.
//
//   (a) The DAMAGE-GATED half — `rescan_from_cells` + `sample_material` +
//       `rain_atlas` — runs on every frame whose terminal damage epoch moved,
//       i.e. essentially EVERY present while a shell or an agent is streaming
//       output. The only existing gate for it (`bench_literal_material_refresh`)
//       forces a 100 % charset change and then calls the result "not the
//       steady-state frame path". That assumption is what `stream_*` below
//       tests: it slides a real scrolling text viewport past the engine and
//       counts how many of those ORDINARY frames pay the ROM re-author + 64-tile
//       atlas rebake + full-atlas `Vec<u8>` clone.
//
//   (b) The PER-TICK half — `emit` — walks EVERY column regardless of how many
//       are lit. `field_*` is the anchor for that walk and `idle_masked_field`
//       is the case nothing in the tree measures at all: enabled, awake, ticking
//       at 30 Hz, and provably unable to draw a single quad.
//
// AND THE OFF FLOOR. Most users have most effects off most of the time, so the
// cost of concluding "nothing to draw" is the most valuable number this crate
// was missing. `off_disabled` / `off_reduced_motion` / `off_hidden_drained` pin
// the three ways emission stops, in increasing order of how far into `emit` the
// frame gets before it gives up.
//
// EVERY WORKLOAD IS GUARDED, and the guards are two-sided on purpose. "Emits
// nothing" is satisfied by an engine that was never armed, so each OFF workload
// first proves the SAME engine emits a saturated field, and only then throws the
// switch being measured. Symmetrically, "reached the field walk" is proved
// positively even when the frame is empty: an ungated frame runs
// `baker.begin_frame` + `bake_tiles`, and every bake batch bumps
// `atlas_version()` — so after a `set_config` restart, 8 version bumps across a
// probe with ZERO quads is external, public proof that `emit_field` was entered
// and walked all 200 columns to conclude there was nothing to light.
//
// EMITTED VOLUME IS REPORTED SEPARATELY from time (the `println!` per workload:
// quad range, halo range, rebakes/frames, atlas bytes per publish) so a later
// regression in COUNT stays separable from a regression in per-item COST.
//
// The clock is INJECTED: every frame advances it by exactly one 30 Hz engine
// tick with `advance_ms(33)`. Nothing here samples the wall clock for state.
//
// ```sh
// cargo bench -p aterm-effects --bench matrix_rain_tick
// ```

use aterm_core::grid::LineSize;
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_effects::matrix_rain::{
    EffectGeom, MatrixRain, RainConfig, RainSignal, RainTickInput, RainVisibility,
};
use aterm_render::{RainHalo, SpriteQuad};
use criterion::measurement::WallTime;
use criterion::{BenchmarkGroup, Criterion, black_box, criterion_group, criterion_main};

/// The design's worst-case viewport (matching `tests/rain_bench.rs` so the
/// numbers here and the numbers in PROOF_CARRYING_PERFORMANCE.md are about the
/// same field).
const ROWS: usize = 50;
const COLS: usize = 200;
/// Theme default background — the `ColorScheme::default` #111318 that
/// `RainConfig::default` also carries. Eligibility is defined against it, so a
/// mismatch here would make every cell ineligible and every workload vacuous.
const BG: u32 = 0x0011_1318;
/// One 30 Hz engine tick of injected clock per presented frame.
const DT_MS: u64 = 33;
const CELL_W: u16 = 10;
const CELL_H: u16 = 20;
const RETINA_W: u16 = 20;
const RETINA_H: u16 = 40;

/// ~20 s of simulated clock, the recipe the in-crate gates use: long enough for
/// the density EMA to reach the density-12 ceiling AND for every column's cycle
/// to re-roll under it, so admission is field-wide. Anything shorter settles a
/// half-empty field and measures the wrong steady state.
const SETTLE_FRAMES: u64 = 600;
/// Frames per guard probe. Long enough to cover a whole 8-batch progressive
/// bake (the positive witness that the ungated branch ran) and to see the quad
/// count over several field cycles.
const PROBE_FRAMES: usize = 64;
/// 64 ROM tiles at `MAX_RAIN_BAKES_PER_TICK` = 8 per batch.
const BAKE_BATCHES: usize = 8;
/// Distinct lines of scrollback the streaming workloads slide the viewport
/// down. Deterministic and wrapping, so a criterion run of any length replays
/// the same sequence of viewports.
const STREAM_LINES: usize = 240;

// ---------------------------------------------------------------- grid shapes

fn space_cell() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [0xD0, 0xD0, 0xD0],
        bg: [0x11, 0x13, 0x18],
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold: false,
        italic: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
    }
}

fn text_cell(ch: char) -> RenderCell {
    RenderCell { ch, ..space_cell() }
}

/// Deterministic LCG — the corpora must be identical on every machine and every
/// run, and must not depend on hashing order or the clock.
fn next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state >> 33
}

/// Plausible build/agent output tokens. The MIX matters more than the words:
/// `sample_material`'s alphabet is the distinct set of the last 128 supported
/// codepoints on screen, so a corpus of one repeated word would converge to a
/// tiny stable alphabet and never rebake — exactly the conclusion this bench
/// exists to test rather than assume.
const VOCAB: [&str; 22] = [
    "Compiling",
    "aterm-effects",
    "v0.9.3",
    "warning:",
    "unused",
    "variable",
    "note:",
    "expected",
    "found",
    "running",
    "test",
    "ok",
    "passed;",
    "0 failed;",
    "Finished",
    "release",
    "[optimized]",
    "target(s)",
    "src/matrix_rain/mod.rs:1292:9",
    "0.42s",
    "-->",
    "emit_field",
];

/// Exactly `len` cells of word-wrapped output text (the row is TRIMMED at
/// `len`, which is the canonical `RenderInput.cells` shape: absent trailing
/// cells are default-bg and rain-eligible).
fn text_row(seed: u64, len: usize) -> Vec<RenderCell> {
    let mut st = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xDEAD_BEEF;
    let mut row: Vec<RenderCell> = Vec::with_capacity(len);
    while row.len() < len {
        if !row.is_empty() {
            row.push(space_cell());
        }
        let word = VOCAB[(next(&mut st) % VOCAB.len() as u64) as usize];
        for ch in word.chars() {
            if row.len() == len {
                break;
            }
            row.push(text_cell(ch));
        }
    }
    row
}

/// All-space grid: every cell eligible, the maximum-density field.
fn blank_grid() -> Vec<Vec<RenderCell>> {
    vec![vec![space_cell(); COLS]; ROWS]
}

/// Two full rows of distinct printable ASCII over an otherwise empty field —
/// the shape `bench_semantic_literal_tick_worstcase` uses, so the literal
/// anchor here is comparable with the number already published for it.
fn mixed_material_grid() -> Vec<Vec<RenderCell>> {
    let mut cells = blank_grid();
    for (i, cell) in cells[0].iter_mut().enumerate() {
        cell.ch = char::from_u32(33 + (i % 64) as u32).expect("printable ASCII material");
    }
    for (i, cell) in cells[1].iter_mut().enumerate() {
        cell.ch = char::from_u32(33 + ((i + 17) % 64) as u32).expect("printable ASCII material");
    }
    cells
}

/// Forty closed titled panels tiling the WHOLE viewport (top row to bottom
/// row), so there is no open field outside a box at all. Every interior cell is
/// either adjacent to a border — cleared by the semantic-clearance pass — or
/// inside a closed frame, masked by `mask_framed_regions`. The result is an
/// all-zero occupancy bitset: the boxed-TUI screen (Claude Code / Codex panels)
/// on which rain provably cannot fall.
fn framed_grid() -> Vec<Vec<RenderCell>> {
    let mut cells = blank_grid();
    let (top, bottom, box_w) = (0usize, ROWS - 1, 5usize);
    for left in (0..COLS).step_by(box_w) {
        let right = left + box_w - 1;
        cells[top][left].ch = '╭';
        cells[top][left + 1].ch = '─';
        cells[top][left + 2].ch = 'T';
        cells[top][left + 3].ch = '─';
        cells[top][right].ch = '╮';
        for row in cells.iter_mut().take(bottom).skip(top + 1) {
            row[left].ch = '│';
            row[right].ch = '│';
        }
        cells[bottom][left].ch = '╰';
        cells[bottom][left + 1].ch = '─';
        cells[bottom][left + 2].ch = '─';
        cells[bottom][left + 3].ch = '─';
        cells[bottom][right].ch = '╯';
    }
    cells
}

/// Wrapped prose at a given text density: every row carries `pct` % of `COLS`
/// worth of real words. Row-uniform on purpose — this is the workload that
/// scales the semantic-clearance dilation (nine scalar bit-clears per
/// meaningful cell), so the independent variable has to be the meaningful-cell
/// count and nothing else.
fn prose_grid(pct: usize) -> Vec<Vec<RenderCell>> {
    let len = COLS * pct / 100;
    (0..ROWS).map(|r| text_row(r as u64 + 7_000, len)).collect()
}

/// `STREAM_LINES + ROWS` lines of scrollback: a quarter blank, the rest output
/// lines of varying length. The streaming workloads take a `ROWS`-tall WINDOW
/// of this and slide it down one line per frame, which models scrolling output
/// with zero copying inside the measured region.
fn stream_buffer() -> Vec<Vec<RenderCell>> {
    (0..STREAM_LINES + ROWS)
        .map(|i| {
            let mut st = (i as u64) ^ 0x51ED_2701;
            if next(&mut st) % 100 < 25 {
                Vec::new() // a blank line: trimmed away entirely
            } else {
                let len = 24 + (next(&mut st) as usize % (COLS - 24));
                text_row(i as u64, len)
            }
        })
        .collect()
}

/// Cells the semantic-clearance pass will dilate around (every non-space cell;
/// these corpora carry no attributes, wide halves, or non-default backgrounds).
fn meaningful_cells(grid: &[Vec<RenderCell>]) -> usize {
    grid.iter().flatten().filter(|c| c.ch != ' ').count()
}

fn line_sizes() -> Vec<LineSize> {
    vec![LineSize::SingleWidth; ROWS]
}

fn geom(cell_w: u16, cell_h: u16) -> EffectGeom {
    EffectGeom {
        cell_w,
        cell_h,
        rows: ROWS as u16,
        cols: COLS as u16,
    }
}

/// The classic pure-hash field (`output_material: false`) at the pinned
/// worst-case density, seeded to match `bench_rain_tick_worstcase`.
fn classic_cfg() -> RainConfig {
    RainConfig {
        enabled: true,
        density: 12,
        output_material: false,
        seed: 7,
        default_bg: BG,
        ..RainConfig::default()
    }
}

/// The SHIPPING default: `output_material` is true in `RainConfig::default`, so
/// this — not the classic field — is what a user with rain on actually runs.
fn literal_cfg(seed: u64) -> RainConfig {
    RainConfig {
        enabled: true,
        density: 12,
        output_material: true,
        seed,
        default_bg: BG,
        ..RainConfig::default()
    }
}

// -------------------------------------------------------------- measured rig

/// What one presented frame produced. Volume is carried alongside the
/// fingerprint so a COUNT regression stays separable from a per-item COST
/// regression.
struct FrameOut {
    fp: u64,
    quads: usize,
    halos: usize,
    /// Bytes in the atlas snapshot this frame published (`RainBaker::atlas`
    /// clones the whole RGBA buffer whenever the bake is dirty), 0 when the
    /// frame published nothing new.
    atlas_bytes: usize,
    /// Whether `atlas_version()` moved this frame — a bake batch or a full
    /// literal re-author + rebake.
    rebaked: bool,
}

/// Emitted-volume accumulator over a probe.
struct Vol {
    frames: usize,
    quads_min: usize,
    quads_max: usize,
    halos_max: usize,
    lit_frames: usize,
    rebakes: usize,
    atlas_bytes: usize,
}

impl Vol {
    fn new() -> Self {
        Self {
            frames: 0,
            quads_min: usize::MAX,
            quads_max: 0,
            halos_max: 0,
            lit_frames: 0,
            rebakes: 0,
            atlas_bytes: 0,
        }
    }

    fn record(&mut self, f: &FrameOut) {
        self.frames += 1;
        self.quads_min = self.quads_min.min(f.quads);
        self.quads_max = self.quads_max.max(f.quads);
        self.halos_max = self.halos_max.max(f.halos);
        self.lit_frames += usize::from(f.fp != 0);
        self.rebakes += usize::from(f.rebaked);
        self.atlas_bytes = self.atlas_bytes.max(f.atlas_bytes);
    }

    fn quads_lo(&self) -> usize {
        if self.frames == 0 { 0 } else { self.quads_min }
    }

    fn report(&self, name: &str, note: &str) {
        println!(
            "matrix_rain_tick/{name}: {} frames, quads {}..{}, halo quads <= {}, \
             lit {}/{}, rebakes {}/{}, atlas <= {} B/publish — {note}",
            self.frames,
            self.quads_lo(),
            self.quads_max,
            self.halos_max,
            self.lit_frames,
            self.frames,
            self.rebakes,
            self.frames,
            self.atlas_bytes,
        );
    }

    /// Volume line for the scan-only halves, which emit no quads at all — the
    /// interesting volume there is the rebake rate and the bytes each publish
    /// copies.
    fn report_scan(&self, name: &str, note: &str) {
        println!(
            "matrix_rain_tick/{name}: {} frames, rebakes {}/{}, atlas <= {} B/publish — {note}",
            self.frames, self.rebakes, self.frames, self.atlas_bytes,
        );
    }
}

/// One engine plus everything a presented frame needs, so the guard probe and
/// the timed loop run the IDENTICAL body.
struct Rig {
    e: MatrixRain,
    g: EffectGeom,
    input: RainTickInput<'static>,
    /// Host cursor for `sample_material`'s composer-protection band.
    cursor: Option<(u16, u16)>,
    seq: u64,
    epoch: u64,
    step: usize,
    version: u64,
    quads: Vec<SpriteQuad>,
    halos: Vec<RainHalo>,
}

impl Rig {
    fn new(e: MatrixRain, g: EffectGeom) -> Self {
        let version = e.atlas_version();
        Self {
            e,
            g,
            input: RainTickInput::default(),
            cursor: None,
            seq: 100_000,
            epoch: 1_000,
            step: 0,
            version,
            quads: Vec::new(),
            halos: Vec::new(),
        }
    }

    fn with_cursor(mut self, cursor: Option<(u16, u16)>) -> Self {
        self.cursor = cursor;
        self.input.cursor = cursor;
        self
    }

    /// Re-sync the version watermark after an out-of-band `set_config`.
    fn resync(&mut self) {
        self.version = self.e.atlas_version();
    }

    fn finish(&mut self, fp: u64, atlas_bytes: usize) -> FrameOut {
        let version = self.e.atlas_version();
        let rebaked = version != self.version;
        self.version = version;
        FrameOut {
            fp,
            quads: self.quads.len(),
            halos: self.halos.len(),
            atlas_bytes,
            rebaked,
        }
    }

    /// The PER-TICK half alone: the host's content note, exactly one engine
    /// tick of injected clock, and the field emission.
    fn tick(&mut self) -> FrameOut {
        self.seq += 1;
        self.e.note_activity(self.seq);
        self.e.advance_ms(DT_MS);
        let fp = self
            .e
            .emit(self.g, &self.input, &mut self.quads, &mut self.halos);
        self.finish(fp, 0)
    }

    /// The per-tick half with the Execute choreography held live, so the
    /// semantic tape traversal stays on the expensive branch instead of
    /// decaying to the cheap `AssistantStream` path after 24 ticks.
    fn tick_semantic(&mut self) -> FrameOut {
        self.e.note_signal(RainSignal::Execute as u32, 8);
        self.tick()
    }

    /// The DAMAGE-GATED half alone, over a fixed grid with the epoch moving
    /// every frame (which is what defeats the host's damage gate during
    /// streaming output).
    fn rescan(&mut self, grid: &[Vec<RenderCell>], sizes: &[LineSize]) -> FrameOut {
        self.epoch += 1;
        self.e
            .rescan_from_cells(grid, sizes, &[], ROWS, COLS, BG, self.epoch);
        self.finish(0, 0)
    }

    /// The viewport this frame sees: the scrollback window slid down one line
    /// since the last frame.
    fn window<'b>(&mut self, buf: &'b [Vec<RenderCell>]) -> &'b [Vec<RenderCell>] {
        let base = self.step % STREAM_LINES;
        self.step += 1;
        &buf[base..base + ROWS]
    }

    /// The literal-material half alone over a SCROLLING viewport: sample the
    /// visible alphabet, then take the published atlas (which is where the
    /// full-atlas `Vec<u8>` clone lives).
    fn material(&mut self, buf: &[Vec<RenderCell>]) -> FrameOut {
        let (cursor, rows) = (self.cursor, ROWS);
        let view = self.window(buf);
        self.e.sample_material(view, rows, cursor, &[]);
        let bytes = self.e.rain_atlas().map_or(0, |a| a.rgba.len());
        self.finish(0, bytes)
    }

    /// THE WHOLE PER-PRESENT BILL over a scrolling viewport: rescan + material
    /// sample + one engine tick + emit + atlas publish. This is what the user's
    /// frame budget actually pays while an agent streams, and it is the sum
    /// nothing in the tree measured.
    fn present(&mut self, buf: &[Vec<RenderCell>], sizes: &[LineSize]) -> FrameOut {
        let (cursor, rows) = (self.cursor, ROWS);
        self.epoch += 1;
        let epoch = self.epoch;
        let view = self.window(buf);
        self.e
            .rescan_from_cells(view, sizes, &[], rows, COLS, BG, epoch);
        self.e.sample_material(view, rows, cursor, &[]);
        self.seq += 1;
        self.e.note_activity(self.seq);
        self.e.advance_ms(DT_MS);
        let fp = self
            .e
            .emit(self.g, &self.input, &mut self.quads, &mut self.halos);
        let bytes = self.e.rain_atlas().map_or(0, |a| a.rgba.len());
        self.finish(fp, bytes)
    }

    fn probe(&mut self, frames: usize, mut body: impl FnMut(&mut Self) -> FrameOut) -> Vol {
        let mut v = Vol::new();
        for _ in 0..frames {
            let out = body(self);
            v.record(&out);
        }
        v
    }
}

/// One field of `MatrixRain::diag_line` — the only public window onto the
/// weather/occupancy state, and the thing that separates "this workload is in
/// the state it claims" from "this workload silently measured an early-out".
fn diag(e: &MatrixRain, key: &str) -> String {
    let line = e.diag_line();
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=').map(str::to_owned))
        .unwrap_or_else(|| panic!("diag_line carries no `{key}=`: {line}"))
}

fn diag_num(e: &MatrixRain, key: &str) -> u64 {
    let raw = diag(e, key);
    raw.parse()
        .unwrap_or_else(|_| panic!("diag `{key}` is not a number: {raw}"))
}

/// Assert the engine is in the settled, fully-armed state every ON workload
/// depends on. Bounded from BOTH sides where a one-sided test would pass on an
/// idle engine: `density` has a floor (a CALM drizzle would sail through
/// `> 0`), `drain` a ceiling, and the scan flag must be set or `emit` gates out
/// before it does any work at all.
fn assert_armed(e: &MatrixRain, name: &str) {
    let weather = diag(e, "weather");
    assert_eq!(
        weather, "working",
        "{name}: weather is `{weather}`, not `working` — the settle loop failed to \
         reach a downpour, so this workload measures a drizzle"
    );
    let density = diag_num(e, "density");
    assert!(
        (200..=255).contains(&density),
        "{name}: density staircase settled at {density}, outside 200..=255 — the \
         EMA never reached the density-12 ceiling and the field is half empty"
    );
    assert_eq!(
        diag(e, "scanned"),
        "true",
        "{name}: no Tier-A scan is resident, so `emit` takes the `!have_scanned` \
         gate and measures nothing"
    );
    assert_eq!(
        diag_num(e, "drain"),
        0,
        "{name}: the field is draining — `drain_ticks >= DRAIN_TICKS` gates \
         emission entirely"
    );
}

/// Build an engine settled into a WORKING downpour over `cells`, exactly the
/// public-notes recipe the in-crate gates use (no test-only state pokes exist
/// outside `#[cfg(test)]`, and none are needed).
fn settled(cfg: RainConfig, cells: &[Vec<RenderCell>], semantic: bool) -> MatrixRain {
    let material = cfg.output_material;
    let mut e = MatrixRain::new(cfg);
    let sizes = line_sizes();
    e.rescan_from_cells(cells, &sizes, &[], ROWS, COLS, BG, 1);
    if material {
        e.sample_material(cells, ROWS, Some((ROWS as u16 - 1, 0)), &[]);
        assert!(
            e.notes_can_wake(),
            "non-vacuity: literal mode needs a populated material bank, or `emit` \
             skips the whole field walk on `material_ready`"
        );
    }
    let g = geom(CELL_W, CELL_H);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for i in 0..SETTLE_FRAMES {
        // Strictly increasing content_seq on consecutive frames, 33 ms apart
        // (well inside STREAK_WINDOW_MS) and with NO keystroke note — a
        // keystroke inside ECHO_DISCOUNT_MS discounts the frame to zero credit
        // and the field never leaves CALM.
        e.note_activity(i + 1);
        if semantic {
            e.note_signal(RainSignal::Execute as u32, 8);
        }
        e.advance_ms(DT_MS);
        e.emit(g, &RainTickInput::default(), &mut q, &mut a);
    }
    e
}

// ------------------------------------------------------------ OFF / IDLE floor

#[derive(Clone, Copy)]
enum Off {
    /// The config flag itself.
    Disabled,
    /// OS / config reduce-motion.
    ReducedMotion,
    /// An occluded pane whose mandatory drain completed. Unlike the two above
    /// this does NOT early-return: the weather machine and the gate expression
    /// still run every frame.
    HiddenDrained,
}

impl Off {
    fn name(self) -> &'static str {
        match self {
            Off::Disabled => "off_disabled",
            Off::ReducedMotion => "off_reduced_motion",
            Off::HiddenDrained => "off_hidden_drained",
        }
    }
}

fn bench_off_floor(group: &mut BenchmarkGroup<'_, WallTime>) {
    let blank = blank_grid();
    for mode in [Off::Disabled, Off::ReducedMotion, Off::HiddenDrained] {
        let name = mode.name();
        let mut rig = Rig::new(settled(classic_cfg(), &blank, false), geom(CELL_W, CELL_H));

        // CONTROL SIDE. Without this half, "emits nothing" is satisfied by an
        // engine that was never in a state to emit anything, and the OFF number
        // would be the cost of a broken fixture rather than of a disabled effect.
        assert_armed(&rig.e, name);
        let armed = rig.probe(PROBE_FRAMES, Rig::tick);
        assert!(
            armed.quads_lo() >= 1_500 && armed.quads_max <= 4_096,
            "{name}: the control engine emitted {}..{} quads, outside 1500..=4096 — \
             it is not the saturated downpour this workload switches OFF",
            armed.quads_lo(),
            armed.quads_max,
        );

        // Restart the bake so the OFF side has a POSITIVE witness: an ungated
        // frame runs `baker.begin_frame` + `bake_tiles`, and every batch bumps
        // `atlas_version()`. A frame that early-returns cannot move it.
        rig.e.set_config(classic_cfg());
        match mode {
            Off::Disabled => rig.e.set_config(RainConfig {
                enabled: false,
                ..classic_cfg()
            }),
            Off::ReducedMotion => rig.e.set_reduced_motion(true),
            Off::HiddenDrained => rig.e.set_visibility(RainVisibility::Hidden),
        }
        rig.resync();

        let off = rig.probe(PROBE_FRAMES, Rig::tick);
        assert_eq!(
            off.quads_max, 0,
            "{name}: emitted {} quads with the effect off",
            off.quads_max
        );
        assert_eq!(off.lit_frames, 0, "{name}: a lit frame with the effect off");
        assert_eq!(
            off.rebakes, 0,
            "{name}: the atlas version moved {} times, so these frames DID reach \
             `baker.begin_frame` inside the emit gate — this is not the early-out \
             path it claims to measure",
            off.rebakes
        );
        assert!(
            !rig.e.is_active(),
            "{name}: `is_active()` is still true, so the host keeps the shared \
             ticker armed and the real idle cost is this number times 30 Hz"
        );
        off.report(
            name,
            "the floor a user pays for an effect they never turned on",
        );

        group.bench_function(name, |b| {
            b.iter(|| black_box(rig.tick().fp));
        });
    }
}

/// ENABLED, AWAKE, AND UNABLE TO DRAW: a boxed-TUI screen whose occupancy
/// bitset is entirely zero. `emit` still walks every column every tick to
/// conclude there is nothing to light, and `is_active()` keeps the host timer
/// armed to do it again at 30 Hz, forever. This is the number an `occ_any`
/// early-out would drive to nearly nothing.
fn bench_idle_masked(group: &mut BenchmarkGroup<'_, WallTime>) {
    let name = "idle_masked_field";
    let blank = blank_grid();
    let framed = framed_grid();
    let sizes = line_sizes();
    let mut rig = Rig::new(settled(classic_cfg(), &blank, false), geom(CELL_W, CELL_H));

    // CONTROL SIDE: over the open field this very engine rains hard.
    assert_armed(&rig.e, name);
    let open = rig.probe(PROBE_FRAMES, Rig::tick);
    assert!(
        open.quads_lo() >= 1_500,
        "{name}: the control field emitted only {} quads — the masked comparison \
         below would be meaningless",
        open.quads_lo()
    );

    // Same engine, same weather, same clock: only the occupancy changes.
    rig.e.set_config(classic_cfg()); // restart the bake for the entry witness
    rig.rescan(&framed, &sizes);
    rig.resync();
    let masked = rig.probe(PROBE_FRAMES, Rig::tick);

    assert_eq!(
        masked.quads_max, 0,
        "{name}: the closed panels leaked {} quads — occupancy is not empty and \
         this workload is measuring an ordinary field",
        masked.quads_max
    );
    assert_armed(&rig.e, name); // still WORKING, still scanned, still undrained
    assert!(
        masked.rebakes >= BAKE_BATCHES,
        "{name}: only {} atlas-version bumps across {} empty frames — fewer than \
         the {BAKE_BATCHES} progressive bake batches an UNGATED frame must \
         produce, so `emit` is taking the `gated` early-out and this is not the \
         full-column-walk cost it claims to be",
        masked.rebakes,
        masked.frames
    );
    assert!(
        rig.e.is_active(),
        "{name}: `is_active()` went false on a masked field — the compounding \
         half of this finding (the host timer stays armed to repeat the walk) is \
         gone, and this workload no longer describes the shipped behaviour"
    );
    masked.report(
        name,
        "enabled, WORKING, timer armed — the full column walk to conclude nothing can be lit",
    );

    group.bench_function(name, |b| {
        b.iter(|| black_box(rig.tick().fp));
    });
}

// ------------------------------------------------------------- steady field

/// The regression anchor for the per-tick field walk: the classic pure-hash
/// field at the design's worst case, quad budget saturated so the whole-column
/// truncation branch is live. Comparable with `bench_rain_tick_worstcase`.
fn bench_field_classic(group: &mut BenchmarkGroup<'_, WallTime>) {
    let name = "field_classic_200x50";
    let blank = blank_grid();
    let mut rig = Rig::new(settled(classic_cfg(), &blank, false), geom(CELL_W, CELL_H));

    assert_armed(&rig.e, name);
    let v = rig.probe(PROBE_FRAMES, Rig::tick);
    assert!(
        v.quads_lo() >= 1_500 && v.quads_max <= 4_096,
        "{name}: quads {}..{} outside 1500..=4096 — a saturated downpour is the \
         whole point of this anchor (too few: half-empty field; too many: the \
         budget cap moved and the truncation branch is no longer exercised)",
        v.quads_lo(),
        v.quads_max
    );
    assert_eq!(
        v.lit_frames,
        v.frames,
        "{name}: {} of {} frames emitted nothing",
        v.frames - v.lit_frames,
        v.frames
    );
    v.report(name, "classic pure-hash field, saturated");

    group.bench_function(name, |b| {
        b.iter(|| black_box(rig.tick().fp));
    });
}

/// The SHIPPING default (`output_material: true`) with the Execute
/// choreography held live, which is what puts the per-quad semantic tape
/// traversal on the measured path.
fn bench_field_literal(group: &mut BenchmarkGroup<'_, WallTime>) {
    let name = "field_literal_semantic_200x50";
    let mixed = mixed_material_grid();
    let mut rig = Rig::new(settled(literal_cfg(19), &mixed, true), geom(CELL_W, CELL_H));

    assert_armed(&rig.e, name);
    let slots = diag_num(&rig.e, "material");
    assert!(
        (8..=64).contains(&slots),
        "{name}: the literal alphabet holds {slots} characters, outside 8..=64 — \
         an empty bank makes `material_ready` false and skips the entire field \
         walk, and a one-glyph bank is not the tape traversal being priced"
    );
    let v = rig.probe(PROBE_FRAMES, Rig::tick_semantic);
    assert!(
        v.quads_lo() >= 800 && v.quads_max <= 4_096,
        "{name}: quads {}..{} outside 800..=4096",
        v.quads_lo(),
        v.quads_max
    );
    assert_eq!(
        v.lit_frames,
        v.frames,
        "{name}: {} of {} frames emitted nothing",
        v.frames - v.lit_frames,
        v.frames
    );
    v.report(
        name,
        "shipping default: literal material + Execute choreography",
    );

    group.bench_function(name, |b| {
        b.iter(|| black_box(rig.tick_semantic().fp));
    });
}

// ----------------------------------------------------------- damage rescan

/// `rescan_from_cells` alone over ordinary wrapped prose, with the epoch moving
/// every frame so the host's damage gate never short-circuits it — i.e. every
/// present during streaming output. Swept over text density because the
/// semantic-clearance pass costs nine scalar bit-clears per MEANINGFUL cell, so
/// the cost scales with the text on screen and not with the rain.
fn bench_rescan_prose(group: &mut BenchmarkGroup<'_, WallTime>) {
    let blank = blank_grid();
    let sizes = line_sizes();
    let mut lit_by_density: Vec<(usize, usize)> = Vec::new();

    for pct in [10usize, 40, 80] {
        let name = format!("rescan_prose_{pct}pct");
        let grid = prose_grid(pct);
        let meaningful = meaningful_cells(&grid);
        let nominal = ROWS * COLS * pct / 100;
        assert!(
            meaningful * 10 >= nominal * 8 && meaningful <= nominal,
            "{name}: {meaningful} meaningful cells against a nominal {nominal} — \
             the corpus is not at the density it is named for, so the sweep does \
             not isolate the dilation cost"
        );

        let mut rig = Rig::new(settled(classic_cfg(), &blank, false), geom(CELL_W, CELL_H));
        let v = rig.probe(PROBE_FRAMES, |r| r.rescan(&grid, &sizes));
        assert_eq!(
            diag(&rig.e, "scanned"),
            "true",
            "{name}: the rescan left no resident scan"
        );

        // Two-sided occupancy witness. A rescan is silent — it returns nothing —
        // so the only external evidence that it wrote the bitset this corpus
        // implies is what the NEXT emit does with it: more text must leave
        // strictly less field. One-sided "it emitted something" would pass on a
        // corpus that never dilated at all.
        let lit = rig.probe(PROBE_FRAMES, Rig::tick).quads_max;
        lit_by_density.push((pct, lit));
        v.report_scan(
            &name,
            &format!("{meaningful} meaningful cells, field after rescan {lit} quads"),
        );

        group.bench_function(name.as_str(), |b| {
            b.iter(|| {
                let out = rig.rescan(black_box(&grid), &sizes);
                black_box(out.fp)
            });
        });
    }

    for pair in lit_by_density.windows(2) {
        let ((lo_pct, lo_lit), (hi_pct, hi_lit)) = (pair[0], pair[1]);
        assert!(
            lo_lit > hi_lit,
            "rescan sweep: {lo_pct}% text left {lo_lit} quads and {hi_pct}% left \
             {hi_lit} — occupancy did not shrink with text, so the clearance pass \
             is not being exercised by this corpus"
        );
    }
    assert!(
        lit_by_density.last().is_some_and(|&(_, lit)| lit > 0),
        "rescan sweep: the densest corpus masked the field completely, so the \
         sweep's top end is indistinguishable from the framed-TUI workload"
    );
}

// --------------------------------------------------------- streaming output

/// THE MISSING MEASUREMENT (matrix-01): the literal-material half against a
/// viewport that SCROLLS, which is what ordinary output does. Reports the
/// rebake RATE — what fraction of ordinary streaming frames pay the ROM
/// re-author + 64-tile atlas rebake + full-atlas clone that
/// `bench_literal_material_refresh` prices at ~2 ms and calls a rare edge.
fn bench_stream_material(group: &mut BenchmarkGroup<'_, WallTime>) {
    let name = "stream_sample_material";
    let buf = stream_buffer();
    let mut rig = Rig::new(
        settled(literal_cfg(23), &buf[..ROWS], false),
        geom(CELL_W, CELL_H),
    )
    .with_cursor(Some((ROWS as u16 - 1, 0)));

    // THE VIEWPORT MUST GENUINELY MOVE. A stationary window would take the
    // "charset unchanged" path on every frame after the first, and this
    // workload would measure a cache hit forever while every other assertion
    // here still passed. Guarded on the CORPUS, not on the rebake count: the
    // rate is the RESULT being measured, and a sticky-slot fix driving it to
    // zero must show up as a win here, never as a failing bench.
    let non_blank = buf.iter().filter(|row| !row.is_empty()).count();
    assert!(
        non_blank * 2 > buf.len() && non_blank < buf.len(),
        "{name}: {non_blank} of {} scrollback lines carry text — the corpus must \
         be mostly-but-not-entirely output for the sampler to have a moving \
         alphabet AND the field somewhere to fall",
        buf.len()
    );
    let moving = buf
        .windows(2)
        .filter(|w| w[0].iter().map(|c| c.ch).ne(w[1].iter().map(|c| c.ch)))
        .count();
    assert!(
        moving * 10 >= (buf.len() - 1) * 8,
        "{name}: only {moving} of {} consecutive scrollback lines differ — a \
         viewport that repeats itself would stop moving the sampled \
         128-codepoint window",
        buf.len() - 1
    );

    assert_armed(&rig.e, name);
    let v = rig.probe(PROBE_FRAMES, |r| r.material(&buf));
    let slots = diag_num(&rig.e, "material");
    assert!(
        (8..=64).contains(&slots),
        "{name}: the literal alphabet holds {slots} characters, outside 8..=64 — \
         an empty bank means the scrolling corpus carries no supported glyphs and \
         the sampler is returning on its empty-sample path"
    );
    let expect_atlas = usize::from(CELL_W) * usize::from(CELL_H) * 8 * 8 * 4;
    assert_eq!(
        v.atlas_bytes, expect_atlas,
        "{name}: the published atlas is {} B, not the {expect_atlas} B a full 8x8 \
         tile grid of {CELL_W}x{CELL_H} cells implies — the full-atlas clone this \
         workload exists to price is not on the measured path",
        v.atlas_bytes
    );
    v.report_scan(
        name,
        "scrolling viewport: the rebake rate ordinary output actually pays",
    );

    group.bench_function(name, |b| {
        b.iter(|| black_box(rig.material(black_box(&buf)).atlas_bytes));
    });
}

/// THE WHOLE PER-PRESENT BILL during streaming output, at both a standard and a
/// retina cell metric: rescan + material sample + engine tick + emit + atlas
/// publish, over a viewport that scrolls one line per frame with the damage
/// epoch moving every frame. Every other rain number in the tree is one part of
/// this in isolation.
fn bench_stream_present(group: &mut BenchmarkGroup<'_, WallTime>, cell: (u16, u16), name: &str) {
    let buf = stream_buffer();
    let sizes = line_sizes();
    let mut rig = Rig::new(
        settled(literal_cfg(29), &buf[..ROWS], false),
        geom(cell.0, cell.1),
    )
    .with_cursor(Some((ROWS as u16 - 1, 0)));

    assert_armed(&rig.e, name);
    // Warm at THIS cell metric: `begin_frame` restarts the bake when the metric
    // changes, so without this the first measured frames would carry a whole
    // 64-tile bake that the steady state does not pay.
    rig.probe(PROBE_FRAMES, |r| r.present(&buf, &sizes));
    rig.resync();
    let v = rig.probe(PROBE_FRAMES, |r| r.present(&buf, &sizes));

    assert_armed(&rig.e, name);
    assert!(
        v.lit_frames * 4 >= v.frames * 3,
        "{name}: only {}/{} frames were lit — a streaming viewport with this much \
         blank space must rain on nearly all of them; the field is being masked \
         away and this is not the streaming bill",
        v.lit_frames,
        v.frames
    );
    assert!(
        v.quads_lo() > 0 && v.quads_max <= 4_096,
        "{name}: quads {}..{} — expected a live but unsaturated field over real \
         text",
        v.quads_lo(),
        v.quads_max
    );
    // The atlas clone is the tail of matrix-01: it only happens on a frame that
    // rebaked, and when it happens it is the WHOLE 8x8-tile RGBA buffer. Pin
    // both halves so a future "we only publish deltas now" cannot slip through
    // as an unchanged number.
    let expect_atlas = if v.rebakes > 0 {
        usize::from(cell.0) * usize::from(cell.1) * 8 * 8 * 4
    } else {
        0
    };
    assert_eq!(
        v.atlas_bytes, expect_atlas,
        "{name}: published atlas is {} B across {} rebaking frames, not the \
         {expect_atlas} B an 8x8 tile grid of {}x{} cells implies",
        v.atlas_bytes, v.rebakes, cell.0, cell.1
    );
    v.report(name, "rescan + material + tick + emit + atlas publish");

    group.bench_function(name, |b| {
        b.iter(|| black_box(rig.present(black_box(&buf), &sizes).fp));
    });
}

fn matrix_rain_tick(c: &mut Criterion) {
    let mut group = c.benchmark_group("matrix_rain_tick");
    bench_off_floor(&mut group);
    bench_idle_masked(&mut group);
    bench_field_classic(&mut group);
    bench_field_literal(&mut group);
    bench_rescan_prose(&mut group);
    bench_stream_material(&mut group);
    bench_stream_present(&mut group, (CELL_W, CELL_H), "stream_present_10x20");
    bench_stream_present(&mut group, (RETINA_W, RETINA_H), "stream_present_retina");
    group.finish();
}

criterion_group!(benches, matrix_rain_tick);
criterion_main!(benches);
