// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The aterm-gui FRAME PATH — keystroke-to-echo and PTY-flood-to-present — had
// no bench target at all before this file: the crate was believed bin-only and
// unbenchable, while the audits kept filing per-presented-frame findings
// against it (CF-6's driver half, PET-03's gui half) that nothing could price.
// It is in fact a LIBRARY whose unit tests build full headless `App` fixtures;
// this bench drives those same fixtures through the feature-gated
// `bench_support` seam (the `test-open-probe` precedent — no shipping build
// compiles it).
//
// WHAT IS TIMED, EXACTLY. The engine path AND NOTHING ELSE, per workload. One
// iteration of a `frame_latency` workload is:
//
//     arm(..);                     // UNTIMED: clock += dt, cadence pulse,
//                                  //          keystroke, 8 KiB PTY feed
//     let t0 = Instant::now();     //  ── timed span opens
//     black_box(<frame call>);     //  tick_fx / compose / present_frame
//     total += t0.elapsed();       //  ── timed span closes
//
// The arms are priced SEPARATELY, under names that cannot be mistaken for the
// frame (`frame_latency_seams`), exactly as the aterm-effects benches price
// their host seams. `frame_latency/timer_floor` measures the `Instant::now()`
// pair every number in the group carries: subtract it for an absolute cost,
// ignore it for an A/B (it cancels).
//
// WHAT A HEADLESS FRAME CAN MODEL, and what each workload calls:
//
//   * `tick_fx` — `App::tick_cursor_fx`, the per-frame cursor-effect driver,
//     extracted VERBATIM from the windowed present and explicitly built for
//     two callers, one of them headless (`splice_cursor_fx`). The CF-6 finding
//     lives entirely inside it.
//   * `compose` — `App::redraw_compose`, the split/composed present (per-pane
//     locks + extraction, decorations, the compose-path cursor effects with
//     their own cadence `ignite`, the PET ink feed + brain tick, the
//     RepaintKey gate). A REAL shipping path (the video recorder's), and the
//     path the split-sparkle unit tests drive headless. The PET-03 gui half
//     lives on it.
//   * `present_frame` — the single-pane present modeled as the scout mapped
//     it: LOCK-A extract (`cell_frame_into` + `take_damage` under one lock),
//     `tick_cursor_fx`, then a REAL CPU raster through the SHIPPING
//     damage-tracked entry (`Renderer::render_input_cached` over the window's
//     persistent `WindowCpu` — RE-1): actual pixels, headless, row-scoped
//     exactly like the product's windowed present. `verify_echo` asserts the
//     model's byte-parity against a full repaint on every verify frame.
//
// SPEC ANCHORS ARE COMPILED INTO THIS BENCH. `aterm-gui`'s dev-dependency on
// `aterm-core` turns the `spec-anchors` feature ON (Cargo.toml, and dev-dep
// features apply to bench targets too), so every `#[refines]`-annotated body
// inside the timed span carries the execution-evidence probe the spec gate
// reads — including `Terminal::write_char`, the per-character path for
// non-ASCII, styled, insert-mode and VT52 output.
//
// What that costs here: ONE thread-local `bool` load and a branch per entry.
// The probe's first act is to test an `ARMED` flag that only
// `aterm_spec::xref::StepEvidence::step` ever sets, and no bench opens an
// evidence window — so no `RefCell` borrow, no `BTreeSet` insert and no string
// comparison happens on any path this file times.
// `aterm_spec_macros::tests::an_unarmed_probe_records_nothing` asserts that
// un-armed behaviour rather than leaving it as a claim. It is still a real,
// if tiny, addition to the annotated bodies, so numbers from BEFORE the probe
// landed are not exactly comparable with numbers from after; the ENFORCED perf
// gates are unaffected either way, since `xtask perf` measures release
// `aterm-bench` examples (crates/xtask/src/perf.rs) and not this criterion
// target.
//
// WHAT IS OUT OF REACH (cut honestly, not stubbed): the OS present itself —
// softbuffer damage-rect blit / GPU swapchain present, frame pacing, EDR/scale
// binding, tab-strip + native chrome, and `redraw_window`'s own inline
// single-pane compose (its present-target match bails headless before LOCK A;
// `present_frame` above is the model of it, not a call into it). Those halves
// need glass and stay unpriced here rather than mispriced.
//
// THE WORKLOADS, in the campaign's priority order:
//
//   1. effects_off_frame  — CF-6's price: `tick_cursor_fx` on a frame with
//      EVERY cursor effect off. The driver still resolves policies + configs
//      and runs the TypingCadence triple + `ignite` before any `cfg.enabled`
//      is consulted; the guard PROVES that from outside (see below).
//   2. pet_invisible_frame — PET-03's price: a composed frame with the pet
//      configured but provably invisible; the unconditional per-frame
//      `pet_ink()` + `sense_ink()` pair still runs. Its exact pair is also
//      priced alone in the seams group (`pet_ink_feed`), because inside a
//      full compose frame the pair is nanoseconds under microseconds.
//   3. flood_present      — HEAVY LOAD: an 8 KiB PTY-shaped chunk ingested
//      between frames, the compose timed while the terminal is hot. The
//      perceived-responsiveness headline.
//   4. keystroke_echo     — the latency-critical path: a real `App::input`
//      keystroke (encode, policy, predictive echo, cadence pulse) as the arm,
//      and the frame that echoes it (extract + effects + raster) timed.
//   5. many_tabs_idle/N   — N resident tabs, nothing changing AND nothing
//      animating (every cursor effect off — see `f_many_tabs`): the
//      steady-state cost of the settled compose frame as state scales.
//   6. scrolled_back_repaint — SCR-1's price, and the workload this crate
//      simply did not have: a presented frame whose viewport is SCROLLED
//      BACK 200 lines and NOT MOVING. Every workload above sits at
//      `display_offset == 0`, where a row read is a pointer into the grid
//      ring; past the offset the SAME per-frame `cell_frame_into` resolves
//      every visible row through `Grid::visible_row_view` -> the 3-tier
//      history materializer, with no memo of any kind — so a repaint of an
//      UNCHANGED scrolled-back window re-materializes all 24 rows from
//      scratch, every frame, forever (the scroll pill alone holds 900 ms
//      then fades 300 ms, and the blink/effects keys keep presenting).
//   7. live_bottom_repaint — THE CONTROL for 6, and the only reason 6's
//      number means anything: the identical fixture, identical script,
//      identical effects-off config, differing in EXACTLY ONE THING — the
//      viewport is at the live bottom. (6 - 7) IS the per-frame history
//      materialization cost, with the raster, the effects driver, the lock
//      and the timer floor all cancelling.
//   8. scrolled_back_wheel — the moving half: one 3-line wheel notch armed
//      per frame (untimed), the frame timed. A notch exposes 3 NEW rows out
//      of 24, so this is where an absolute-row-keyed memo should collapse
//      24 materializations to 3 while 6 collapses them to 0.
//
// WHAT THE SCROLLED-BACK ARMS PIN, TWO-SIDED. Reach: `display_offset` is
// re-read on every sampled frame (the arm really is scrolled back / really is
// at the bottom / really is moving), and the history is real
// (`scrollback_lines() >= depth + rows`). IDENTITY: the fill lines are
// numbered, so the frame's own extracted scratch is read back and every row
// checked against ONE formula that covers both arms —
//
//     viewport row r shows fill line `scrollback_lines() - display_offset() + r`
//
// — which is the exact mapping `visible_row_view` performs (row r -> rev_idx
// d-1-r -> history index total-d+r; history index i IS fill line i because a
// 5k fill never evicts from a 10k ring). That formula is recomputed from LIVE
// observables at every check, so it stays correct across scroll moves and
// across output arriving while scrolled back — which is what makes it a real
// invalidation guard for any row memo keyed on absolute row identity, not
// just a content spot-check. The verify pass walks the memo's whole risk
// surface deliberately: scroll to the bottom and back, feed a line while
// scrolled back (history grows under the viewport), and change depth —
// re-asserting identity immediately after each.
//
// ONE FINDING THIS BENCH MADE ON ITS FIRST RUN (FL-1), now FIXED: a SETTLED
// composed window never took the RepaintKey early-out — every scheduled
// compose of unchanged content repainted the full window. The first-run note
// blamed a per-frame `damage_epoch` advance; the root-cause trace showed the
// keys were byte-equal on settled frames (the epoch is latched and cannot
// move without grid damage) and the real driver was the per-window
// `recovery_redraw_outstanding` flag: both fixtures' setup crosses
// `sync_window`'s `rearm_present_and_request(.., request_even_if_ready =
// true, ..)` (tab switch / split sync), which latches the flag, and the
// composed path consulted it in `should_repaint_or_recover` without ever
// acknowledging it — only the WINDOWED present's outcome handlers clear it,
// and a compose-only consumer (this bench, headless captures) never reaches
// one, so the early-out was bypassed on every frame forever. Fixed by
// acknowledging the delivered edge at the compose commit
// (`PresentRetry::on_recovery_redraw_serviced`): one latched edge buys
// exactly one recomposed present. The `pet_invisible_frame` and
// `many_tabs_idle/N` guards PIN the fixed behaviour two-sided (settled
// presented == 0, fed controls still present); if a pin ever reads nonzero
// again, the early-out has regressed.
//
// AND A PIN LIKE THAT ONLY MEANS "REGRESSED" ON A FIXTURE THAT CAN SETTLE.
// `many_tabs_idle/N` read 163 of 300 for a while and it was not the early-out:
// the fixture ran the SHIPPED config, whose default cursor-trail style became
// `rainbow kitty pet`, so a walking cat moved the `RepaintKey` every frame.
// The pin was true of the code and false of the fixture, and because these
// guards run in `main()` before criterion is handed a single workload, the
// whole bench binary was unrunnable for as long as it stood. Both pinned
// fixtures now hold the cursor-trail MASTER off and SAY SO in an assertion
// (`!glow_enabled()` — `pet_invisible` already did, `many_tabs_idle` does
// now), so "presented != 0" can only mean the early-out.
//
// EVERY WORKLOAD IS VERIFIED BEFORE IT IS TIMED, template: the aterm-effects
// cursor_glow_tick bench. Two-sided guards prove the workload reaches the
// state it claims — an "off" cost measured on a script that would have drawn
// nothing anyway is not a measurement — and OFF arms carry DarkUnless-style
// controls: the identical script through a fixture missing only the off
// switch, which must light up.
//
// HOW THE CF-6 REACH IS PROVED FROM OUTSIDE. `ignite` heat-blends the
// returned `trail_color` from the cadence intensity/warmth pair
// UNCONDITIONALLY — before any enabled bit is consulted — so on an all-off
// frame the returned colour still moves with typing heat. The guard pins:
// hot-cadence trail_color != cold-cadence trail_color while every fingerprint
// and fill stays dark. That inequality IS the observable proof the cadence
// reads ran and were consumed on the off frame (pre-fix reality). The
// shared-sample adoption (`TypingCadence::sample`, already landed engine-side)
// keeps it bit-identical; an early-out fix that stops blending on all-off
// frames must revisit this guard — at which point the whole span this
// workload times is the win being claimed.
//
// REACH WAS ALSO CONFIRMED ONCE, OUT OF BAND, with temporary counters at the
// audit sites themselves (added, observed, removed — the campaign's
// prove-reach discipline): over 300 all-off `tick_fx` frames the cadence
// `rainbow_energy` read and the `ignite` pair each ran exactly 300 times
// (CF-6), and over 300 settled composed frames the compose-path
// `pet_ink`+`sense_ink` feed ran exactly 300 times while the single-pane twin
// ran 0 (it lives behind the glass-only present — headless-unreachable, as
// documented above). The standing guards keep the STATE honest from outside:
// the map is real (rows inked, live edge present), the pet is configured
// (`trail_is_kitty_pet`), and it cannot draw (`glow_cfg.enabled == false` is
// one of `pet_visible`'s AND terms).
//
// DETERMINISM. Every workload advances an INJECTED clock by a fixed dt per
// frame (one wall sample per fixture, at its origin); the measurement clock
// (criterion's) is entirely separate. Two exceptions are documented where
// they occur: `App::input` samples the wall internally (the real input seam
// is being driven, not modeled), and `RepaintKey` folds one wall read for a
// notice fingerprint on compose frames — neither feeds any guard.
//
// OBSERVABLE COUNTS ARE MEASUREMENTS, NOT PRINTOUTS. The volume group hands
// the guard-asserted counts (flood bytes/frame, the pet ink map's size) to
// criterion as their own benchmarks — 1 ns == 1 item, the aterm-effects
// convention — so a count regression is stored and A/B'd by the same tooling
// that catches a time regression.

use std::time::{Duration, Instant};

use aterm_gui::bench_support::BenchApp;
use criterion::measurement::WallTime;
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, black_box, criterion_group, criterion_main,
};

// ------------------------------------------------------------------ clocks --

/// 60 fps — the dt of frame-paced workloads (flood, pet, idle).
const FRAME_DT: Duration = Duration::from_millis(16);

/// 8 ms per keystroke — fast human / key-repeat cadence, the same dial the
/// effects benches use for their typing scripts. It keeps the cadence heat
/// saturated, which is the state the CF-6 triple costs most in (a never-typed
/// tracker short-circuits its lazy decay entirely).
const TYPE_DT: Duration = Duration::from_millis(8);

// ------------------------------------------------------------------ script --

/// Frames of script run before anything is sampled or timed. Long enough for
/// every fixture to reach steady state: the decoration engines' longest
/// episode (a cat peek, ~600 ms) is ~40 frames, and the pet's fade/settle
/// envelope shorter — 400 frames (6.4 s injected) covers them with margin.
const WARM_FRAMES: usize = 400;

/// Frames sampled by each workload's verify pass.
const SAMPLE_FRAMES: usize = 300;

/// The flood chunk: the reader thread's 8 KiB slicing shape.
const FLOOD_CHUNK: usize = 8 * 1024;

/// Keystrokes between the echo script's line wraps (see `keystroke_echo`).
const ECHO_LINE: u32 = 60;

/// The grid every fixture presents at (the headless fixture's stub terminal).
const ROWS: usize = 24;

/// Scrollback fill for the scrolled-back arms, in lines. Comfortably inside
/// the stub session's 10_000-line grid ring (`Grid::new`'s default), so the
/// fill is never evicted and history index `i` IS fill line `i` — the fact the
/// frame-identity formula rests on. The stub session carries NO tiered store
/// (`Terminal::new`), so these arms price the RING tier: the tier interactive
/// scrollback actually lives in (the product keeps 10k live ring lines in
/// front of the tiers). Deep warm/cold tier decode has its own instrument —
/// `aterm-bench --example scroll_scrub_harness` — and is deliberately not
/// re-measured here.
const SB_FILL_LINES: usize = 5_000;

/// Where the scrolled-back arms sit: 200 lines up. Deep enough that EVERY one
/// of the 24 viewport rows is a history row (so the workload is not diluted by
/// live rows), shallow enough to be an ordinary "scrolled up to read the build
/// log" position rather than a jump to the top.
const SB_DEPTH: usize = 200;

/// One wheel notch, in lines — the delta a mouse wheel step scrolls, i.e. the
/// number of genuinely NEW rows a scrolling frame exposes out of `ROWS`.
const SB_WHEEL: usize = 3;

/// How far above [`SB_DEPTH`] the wheel arm travels before reversing. Keeps
/// the scrub inside the fill forever (it never walks off the top of history,
/// which would clamp `scroll_display` into a no-op and silently turn the
/// moving arm into the stationary one).
const SB_WHEEL_SPAN: usize = 600;

/// Warm-up frames for the scrolled-back arms. Shorter than [`WARM_FRAMES`] on
/// purpose: these fixtures have every effect OFF, so there is no multi-second
/// decoration envelope to settle — only the render scratch and the per-window
/// damage cache, which reach steady state in a handful of frames.
const SB_WARM_FRAMES: usize = 48;

// ------------------------------------------------------------------ report --

/// The one human-readable line each verify pass prints: what state the
/// workload reached, in the numbers its guards assert on.
fn report(name: &str, detail: &str) {
    println!("REACH {name:<22} | {detail}");
}

// ---------------------------------------------------------------- fixtures --

/// CF-6 fixture: every cursor effect off, cadence kept hot by the arm.
fn f_effects_off() -> BenchApp {
    let mut b = BenchApp::headless();
    b.effects_all_off();
    b
}

/// The off arm's lit control: rainbow kitty at full — the one config knob the
/// off fixture is missing.
fn f_rainbow_on() -> BenchApp {
    let mut b = BenchApp::headless();
    b.effects_rainbow_on();
    b
}

/// The SPLIT workload's window grid and pane count — a desktop-sized composed
/// frame, not the 24x80 the rest of this group runs at.
///
/// The rest of the group prices ONE terminal's worth of work, where 24x80 is a
/// fair unit. The split workload prices the compositor's per-pane amplification,
/// and at 24x80 a four-way split gives each pane ~9 columns: the per-pane fixed
/// costs would dominate the per-cell resolve the audit is about, and the
/// workload would report on the wrong half. 90x340 is roughly a 4K display at a
/// mid-size font — ~30k cells, the shape the audit item is written against.
const SPLIT_ROWS: u16 = 90;
/// See [`SPLIT_ROWS`].
const SPLIT_COLS: u16 = 340;
/// See [`SPLIT_ROWS`].
const SPLIT_PANES: usize = 4;
/// Rows of neutral text every split pane carries — more than a screenful, so
/// the caret sits at the bottom and the pane is full.
const SPLIT_FILL_ROWS: usize = 40;
/// The width those rows are filled to: the WIDEST pane a `SPLIT_PANES` split of
/// [`SPLIT_COLS`] produces, so the focused pane's rows are fully materialized.
/// See [`wide_line`] for why this is not scenery.
const SPLIT_FILL_COLS: usize = SPLIT_COLS as usize / 2;

/// Neutral, non-lexicon text: it must put INK on the rows (the pet map's
/// input) without summoning sparkle-word decorations, whose multi-second
/// episodes would keep the fixtures from ever settling.
fn bland_line(i: usize) -> Vec<u8> {
    format!("row {i:02} 0123456789 abcdef 0123456789 abcdef\r\n").into_bytes()
}

/// [`bland_line`] padded to `width` columns — the same neutral, non-lexicon
/// alphabet, filling the row instead of leaving most of it unwritten.
///
/// Width is load-bearing for the split workload and for nothing else in this
/// file. An engine row snapshot is RAGGED: `cell_frame_fill` materializes only
/// the row's WRITTEN PREFIX, so a 90x170 pane holding 40-column lines resolves
/// ~3.7k cells per frame, not 15.3k. A fixture built from `bland_line` would
/// therefore price a QUARTER of the resolve the audit item is about and report
/// the smaller saving as the whole story. Filling the pane is the honest upper
/// end of the same one number; both ends are recorded in the item's notes.
fn wide_line(i: usize, width: usize) -> Vec<u8> {
    let mut line = format!("row {i:02} ");
    while line.len() < width {
        line.push_str("0123456789 abcdef ");
    }
    line.truncate(width);
    line.push_str("\r\n");
    line.into_bytes()
}

/// PET-03 fixture: 2-pane split, pet CONFIGURED (`rainbow kitty pet`) but
/// PROVABLY invisible (glow master off), sparkle scanner at its default ON so
/// the per-row ink map is real, both panes carrying enough text that the map
/// has a live edge and inked rows to scan.
fn f_pet_invisible() -> (BenchApp, Instant) {
    let mut b = BenchApp::headless();
    b.pet_configured_glow_off();
    let sid = b.split_stub();
    let t0 = Instant::now();
    b.mark_deco_birth(t0);
    // Establishing compose: a pane the scanner has never seen SPENDS whatever
    // was already on it (the refocus-storm rule) — the rows that must be
    // inked are the ones written after the first frame, like the unit fixture.
    b.compose(t0);
    for s in [0u64, sid] {
        for i in 0..20 {
            b.feed(s, &bland_line(i));
        }
    }
    (b, t0)
}

/// Flood fixture: DEFAULT config (the shipped shape — sparkle on, cursor
/// trail at its default), 2-pane split, the focused (new) pane about to take
/// 8 KiB per frame.
fn f_flood() -> (BenchApp, u64, Instant) {
    let mut b = BenchApp::headless();
    let sid = b.split_stub();
    let t0 = Instant::now();
    b.mark_deco_birth(t0);
    b.compose(t0);
    (b, sid, t0)
}

/// One 8 KiB PTY-shaped chunk: whole build-log-ish lines, exactly
/// `FLOOD_CHUNK` bytes, ~100 lines — a hard scroll every frame.
fn flood_chunk() -> Vec<u8> {
    let mut out = Vec::with_capacity(FLOOD_CHUNK);
    let mut i = 0usize;
    while out.len() < FLOOD_CHUNK {
        let line = format!(
            "[ {:3}%] compiling module_{i:05}.o  warnings: 0  time: {:4} ms\r\n",
            i % 100,
            (i * 37) % 1000
        );
        out.extend_from_slice(line.as_bytes());
        i += 1;
    }
    out.truncate(FLOOD_CHUNK);
    out
}

/// SPLIT-COMPOSE fixture: a `SPLIT_PANES`-way split at [`SPLIT_ROWS`]x
/// [`SPLIT_COLS`], DEFAULT config (the shipped shape), every pane carrying a
/// screenful of neutral text with a prompt at the caret.
///
/// The engines are really resized to their pane rectangles
/// (`resize_settle` — the eager pass every non-drag caller runs), which is
/// load-bearing: the carrier's continuity check compares the scratch's
/// dimensions against the terminal's OWN `rows()`/`cols()`, so a fixture that
/// set only the window grid would hand every pane a dimension mismatch and both
/// arms would fall back to the full re-extract while reporting a confident zero.
fn f_split() -> (BenchApp, Instant) {
    let mut b = BenchApp::headless();
    for _ in 1..SPLIT_PANES {
        b.split_stub();
    }
    b.set_grid(SPLIT_ROWS, SPLIT_COLS);
    b.resize_settle();
    let t0 = Instant::now();
    b.mark_deco_birth(t0);
    for sid in b.session_ids() {
        for i in 0..SPLIT_FILL_ROWS {
            b.feed(sid, &wide_line(i, SPLIT_FILL_COLS));
        }
        b.feed(sid, b"user@host demo $ ");
    }
    b.compose_at(SPLIT_ROWS, SPLIT_COLS, t0);
    (b, t0)
}

/// Rows of text the RAIN split fixture writes into each pane — HALF the
/// [`SPLIT_ROWS`] screen, with the rest left blank.
///
/// THIS IS NOT COSMETIC, AND IT IS WHY THE RAIN FIXTURE CANNOT REUSE
/// [`f_split`]'s FILL. Rain only draws on cells the Tier-A occupancy scan
/// found ELIGIBLE — a space, on the default background, with a cell of
/// clearance from any real glyph. `f_split` writes `SPLIT_FILL_COLS` (170)
/// columns per line into a focused pane that a four-way split leaves 41
/// columns wide, so every line WRAPS about four times and the 90-row pane ends
/// up written edge to edge. Occupancy is then empty, the field has nowhere to
/// fall, and the engine emits ZERO quads while every config flag still says
/// "rain is on" — the exact silent non-arming this bench's reach guards exist
/// to catch, and the first thing `verify_split_rain` caught.
///
/// So this fixture writes a scene rain can actually fall through: text in the
/// top half, blank screen below it, which is also what a terminal with rain
/// enabled looks like in practice.
///
/// WHICH DIRECTION THIS BIASES THE NUMBER, stated plainly: the focused pane's
/// full extract materializes each row's WRITTEN PREFIX, so this scene is about
/// 46% of the cells `split_compose_*` resolves (45 rows x 38 cols against 90 x
/// 41). Any per-frame extraction saving measured here is therefore SMALLER
/// than the same saving on a denser screen — but a denser screen is not a rain
/// scene at all, because rain cannot emit on one.
const RAIN_FILL_ROWS: usize = SPLIT_ROWS as usize / 2;

/// The width the rain fixture's lines are filled to: inside the NARROWEST pane
/// a `SPLIT_PANES` split of [`SPLIT_COLS`] produces (41 columns), so no line
/// wraps. A wrapped line is what fills the screen and starves the field; see
/// [`RAIN_FILL_ROWS`].
const RAIN_FILL_COLS: usize = 38;

/// The RAIN-ENABLED twin of [`f_split`]: same window grid, same pane count,
/// same split shape, same echo arm — differing in `[matrix_rain] enabled` and
/// in the fill density [`RAIN_FILL_ROWS`] explains.
///
/// WHY A SEPARATE FIXTURE AND NOT A FLAG ON THE OLD ONE. Rain is resolved from
/// `App::config` at build time and its engine is retained per WINDOW across
/// frames (weather, field, literal material bank, the progressive atlas bake),
/// so a fixture cannot be flipped between arms mid-run without pricing the
/// build-up on whichever arm happens to flip it. Two fixtures, each warmed to
/// its own steady state, is the only honest shape.
///
/// `rain_on` is called BEFORE the fill so the first compose already resolves
/// parameters; the engine itself is still built lazily on that first
/// effectively-on tick, exactly as the product does.
fn f_split_rain() -> (BenchApp, Instant) {
    let mut b = BenchApp::headless();
    for _ in 1..SPLIT_PANES {
        b.split_stub();
    }
    b.set_grid(SPLIT_ROWS, SPLIT_COLS);
    b.resize_settle();
    b.rain_on();
    let t0 = Instant::now();
    b.mark_deco_birth(t0);
    for sid in b.session_ids() {
        for i in 0..RAIN_FILL_ROWS {
            b.feed(sid, &wide_line(i, RAIN_FILL_COLS));
        }
        b.feed(sid, b"user@host demo $ ");
    }
    b.compose_at(SPLIT_ROWS, SPLIT_COLS, t0);
    (b, t0)
}

/// One split frame plus its arm, returning `(presented, scoped, full)` for the
/// frame alone — the reach witness `verify_split` asserts on.
///
/// `unscoped: true` is the PRE-FIX focused extraction verbatim: the focused
/// pane's resident buffer is disowned before the frame, so its extraction can
/// only take the carrier's FULL arm, which IS the historical `cell_frame_into` +
/// `take_damage` pair. Nothing else differs — same panes, same echo, same
/// blit, same decorations, ONE binary.
fn split_frame(b: &mut BenchApp, now: Instant, tick: &mut u8, unscoped: bool) -> (bool, u64, u64) {
    b.split_echo(tick);
    if unscoped {
        b.disown_focus_scratch();
    }
    let (s0, f0) = b.refill_arms();
    let presented = b.compose_at(SPLIT_ROWS, SPLIT_COLS, now);
    let (s1, f1) = b.refill_arms();
    (presented, s1 - s0, f1 - f0)
}

/// THE K2 REACH CENSUS for one split frame: how many of the composite's
/// [`SPLIT_ROWS`] rows the retention lane left exactly as the frame before it
/// wrote them.
///
/// The echo damages EXACTLY ONE grid row, so a settled frame of this fixture
/// needs to write exactly one composed row and may leave the other 89 alone.
/// Asserted rather than assumed: without it the split arms would price a
/// retention lane that had silently refused every frame and read as a plain
/// re-measurement of the seam pass.
fn split_retained(b: &BenchApp) -> usize {
    b.composed_retained_rows()
}

/// Keystroke-echo fixture: single pane, default config, a prompt on screen.
fn f_echo() -> (BenchApp, Instant) {
    let mut b = BenchApp::headless();
    let t0 = Instant::now();
    b.feed(0, b"user@host demo $ ");
    b.present_frame(t0);
    (b, t0)
}

/// Many-tabs fixture: `n` resident tabs (stub sessions), the ACTIVE one
/// (`push_stub_tab` switches to each new tab, so the last pushed — session
/// `n-1` — is active) showing a screenful of neutral text, then IDLE.
///
/// EVERY CURSOR EFFECT OFF, and that is what makes the word "idle" true.
/// `BenchApp::headless()` starts from the SHIPPED config, whose default
/// `cursor_trail_style` became `rainbow kitty pet` — a permanently resident,
/// walking cat. A window with a live pet in it is never settled: the pet's
/// fingerprint moves every frame, the `RepaintKey` moves with it, and the
/// early-out this workload exists to price is never reached. MEASURED before
/// this line existed: `many_tabs_idle/2` presented 163 of 300 settled frames
/// while the effects-off `pet_invisible` twin presented 0 of 300 — so the
/// FL-1 pin below was reading an animating window and calling it settled, and
/// the whole bench binary died in `main()` on it, before a nanosecond of any
/// workload was timed.
///
/// Off rather than re-pinned, because the alternative measures the wrong
/// thing: with the pet live this arm prices a FULL RECOMPOSE and would report
/// it under the name `many_tabs_idle`, where the group comment, the report
/// string and the `n` sweep all promise the steady-state early-out as TAB
/// COUNT scales. A per-frame pet is a constant in `n`; it is noise on this
/// axis and a lie in this name. The lit pet has its own priced workloads
/// (`pet_invisible_frame`, `split_rain_compose`).
fn f_many_tabs(n: usize) -> (BenchApp, u64, Instant) {
    let mut b = BenchApp::headless();
    assert!(n >= 1);
    b.push_stub_tabs(n - 1);
    b.effects_all_off();
    // Sessions are minted 0..n in order, and the compose presents the ACTIVE
    // tab — feed that one, or the fed control below would damage a session no
    // visible pane folds into the RepaintKey.
    let active_sid = (n - 1) as u64;
    let t0 = Instant::now();
    for i in 0..12 {
        b.feed(active_sid, &bland_line(i));
    }
    b.compose(t0);
    (b, active_sid, t0)
}

/// One numbered fill line. The number is what the identity guard reads back
/// out of the presented frame, so the format is load-bearing: `sb <n> ...`.
fn sb_line(i: usize) -> Vec<u8> {
    format!("sb {i:05} 0123456789 abcdefghij 0123456789 abcdefghij\r\n").into_bytes()
}

/// A scroll delta as the `i32` `Grid::scroll_display` takes.
fn sb_delta(lines: usize) -> i32 {
    i32::try_from(lines).expect("scrollback bench deltas fit i32")
}

/// The scrolled-back fixture (and, at `depth == 0`, its live-bottom control):
/// a headless window with EVERY cursor effect off — so the timed span is the
/// LOCK-A extraction plus the damage-tracked raster and nothing else — over a
/// real `SB_FILL_LINES`-line history, parked `depth` lines up.
///
/// Effects are off in BOTH arms, not to flatter the number but because the
/// control subtracts them either way; what it must not subtract is a variable
/// per-frame effects cost riding on one arm's samples. The cursor-effect
/// driver has its own workload (`effects_off_frame`) two entries up.
fn f_scrollback(depth: usize) -> (BenchApp, Instant) {
    let mut b = BenchApp::headless();
    b.effects_all_off();
    let t0 = Instant::now();
    // One `process` call, not 5_000: the fill is untimed setup and the engine
    // sees the identical byte stream either way.
    let mut corpus = Vec::with_capacity(SB_FILL_LINES * 48);
    for i in 0..SB_FILL_LINES {
        corpus.extend_from_slice(&sb_line(i));
    }
    b.feed(0, &corpus);
    b.present_frame(t0);
    if depth > 0 {
        b.scroll_display(sb_delta(depth));
    }
    (b, t0)
}

/// The fill line number the LAST PRESENTED frame shows at viewport row `r`,
/// or `None` when that row is not a fill line (only the live bottom's trailing
/// empty cursor row).
fn sb_row_line(b: &BenchApp, r: usize) -> Option<usize> {
    let text = b.scratch_row_text(r);
    let mut it = text.split_whitespace();
    if it.next()? != "sb" {
        return None;
    }
    it.next()?.parse::<usize>().ok()
}

/// THE FRAME-IDENTITY PIN (see the file header for the derivation): every
/// viewport row of the frame just presented must show the fill line the
/// current `(scrollback_lines, display_offset)` pair says it must. Returns the
/// number of rows actually checked so the caller can prove the walk was not
/// vacuous. `filled` is how many fill lines exist right now — rows past it are
/// the empty cursor line and legitimately carry no number.
fn assert_sb_identity(b: &BenchApp, filled: usize, ctx: &str) -> usize {
    let base = b
        .scrollback_lines()
        .checked_sub(b.display_offset())
        .expect("display_offset <= scrollback_lines is a Grid invariant");
    let mut checked = 0usize;
    for r in 0..ROWS {
        let want = base + r;
        match sb_row_line(b, r) {
            Some(got) => {
                assert_eq!(
                    got, want,
                    "{ctx}: viewport row {r} shows fill line {got}, expected {want} — the \
                     presented frame is not the history the viewport points at"
                );
                checked += 1;
            }
            None => assert!(
                want >= filled,
                "{ctx}: viewport row {r} carries no fill line but should show {want}"
            ),
        }
    }
    checked
}

/// One wheel notch for the moving arm: `SB_WHEEL` lines, reversing at the ends
/// of a bounded span so the scrub runs forever over OVERLAPPING viewports and
/// never clamps at the top of history (a clamped `scroll_display` is a no-op,
/// which would silently turn this arm into the stationary one).
fn wheel_arm(b: &mut BenchApp, dir: &mut i32) {
    let d = b.display_offset();
    if d >= SB_DEPTH + SB_WHEEL_SPAN {
        *dir = -1;
    } else if d <= SB_DEPTH {
        *dir = 1;
    }
    b.scroll_display(sb_delta(SB_WHEEL) * *dir);
}

// ------------------------------------------------------------------ verify --

/// PROVE the CF-6 workload's state, then hand back the warmed fixture and its
/// clock so the timed run continues from the verified state.
///
/// The guard set, all two-sided:
///   * DARK: every sampled frame's `glow_fp == 0 && trail_fp == 0`, no fill,
///     no bolt/twinkle override, and `glow_cfg.enabled == false` — the
///     "every cursor effect off" claim, read from the very config the
///     engines were ticked with.
///   * HOT: the cadence intensity stays >= 0.9 across the window — the
///     pulsed arm really is sustaining the state whose per-frame decay cost
///     CF-6 prices (a cold tracker short-circuits before the `powf`).
///   * CONSUMED (the CF-6 witness): the returned `trail_color` on the hot
///     fixture differs from a COLD control's — `ignite`'s heat blend ran on
///     an all-off frame, observed from outside. Pre-fix reality; see the
///     file header for what each fix shape does to this guard.
///   * LIT CONTROL (DarkUnless): the identical script through a fixture
///     missing only the off switch (rainbow kitty on, cursor moving) must
///     light `glow_fp` on most frames — otherwise the off arm's zero would
///     prove nothing.
fn verify_effects_off() -> (BenchApp, Instant) {
    let mut b = f_effects_off();
    let mut now = Instant::now();
    for _ in 0..WARM_FRAMES {
        now += TYPE_DT;
        b.pulse_typing(now);
        b.tick_fx(now);
    }
    let mut dark = 0usize;
    let mut off = 0usize;
    let mut hot_color = None;
    let mut int_min = f32::MAX;
    for _ in 0..SAMPLE_FRAMES {
        now += TYPE_DT;
        b.pulse_typing(now);
        let fx = b.tick_fx(now);
        dark += usize::from(
            fx.glow_fp == 0
                && fx.trail_fp == 0
                && !fx.any_fill
                && !fx.bolt_cursor
                && !fx.twinkle_cursor,
        );
        off += usize::from(!fx.glow_enabled);
        match hot_color {
            None => hot_color = Some(fx.trail_color),
            Some(c) => assert_eq!(
                c, fx.trail_color,
                "effects_off_frame: a saturated cadence must blend one steady colour"
            ),
        }
        int_min = int_min.min(b.cadence_intensity(now));
    }
    let hot_color = hot_color.expect("sampled");
    report(
        "effects_off_frame",
        &format!(
            "dark {dark}/{SAMPLE_FRAMES} | glow disabled {off}/{SAMPLE_FRAMES} | \
             cadence intensity min {int_min:.3} | trail_color {hot_color:#08x}"
        ),
    );
    assert_eq!(
        dark, SAMPLE_FRAMES,
        "effects_off_frame: a lit frame on the off arm"
    );
    assert_eq!(
        off, SAMPLE_FRAMES,
        "effects_off_frame: glow_cfg.enabled must be false"
    );
    assert!(
        int_min >= 0.9,
        "effects_off_frame: cadence went cold ({int_min}) — the arm is not sustaining \
         the state CF-6 prices"
    );

    // COLD control: the identical fixture, never typed. Its trail_color is the
    // unblended base — the two-sided half of the CONSUMED witness.
    let mut cold = f_effects_off();
    let mut cnow = Instant::now();
    let mut cold_color = None;
    for _ in 0..64 {
        cnow += TYPE_DT;
        let fx = cold.tick_fx(cnow);
        assert_eq!(
            cold.cadence_intensity(cnow),
            0.0,
            "cold control must stay cold"
        );
        cold_color = Some(fx.trail_color);
    }
    let cold_color = cold_color.expect("sampled");
    report(
        "effects_off.cold",
        &format!("trail_color {cold_color:#08x} (unblended base)"),
    );
    // FLIPPED WITNESS (the guard this replaces asserted hot != cold). The
    // pre-fix driver ran the cadence triple + ignite blend on every presented
    // frame even with all cursor effects off, so a typed-hot fixture's
    // trail_color moved with cadence heat — and this guard pinned that waste
    // by asserting the difference. The CF-6 early-out fix makes the off frame
    // skip the blend entirely, so hot and cold must now match: an off frame's
    // colour must NOT depend on typing history. If this ever fails again in
    // the hot != cold direction, the driver has regressed to paying the
    // cadence on off frames.
    assert_eq!(
        hot_color, cold_color,
        "CF-6 witness (post-fix): an all-effects-off frame's trail_color moved \
         with cadence heat — the early-out regressed and the driver is paying \
         the ignite blend on off frames again."
    );

    // LIT control (DarkUnless): only the off switch differs; the script adds a
    // cursor walk because a parked cursor never spawns and a control that
    // would have drawn nothing proves nothing.
    let mut lit = f_rainbow_on();
    let mut lnow = Instant::now();
    let mut lit_frames = 0usize;
    for i in 0..(WARM_FRAMES + SAMPLE_FRAMES) {
        lnow += TYPE_DT;
        lit.pulse_typing(lnow);
        let col = u16::try_from(2 + (i % 40)).expect("small col");
        let fx = lit.tick_fx_at(lnow, (4, col));
        if i >= WARM_FRAMES {
            lit_frames += usize::from(fx.glow_fp != 0);
        }
    }
    report(
        "effects_off.control",
        &format!("rainbow-on twin lit {lit_frames}/{SAMPLE_FRAMES}"),
    );
    assert!(
        lit_frames > SAMPLE_FRAMES / 2,
        "effects_off_frame: the CONTROL (identical script, off switch removed) drew \
         almost nothing ({lit_frames}/{SAMPLE_FRAMES}) — the off arm's zero would be \
         measuring a script with no light to suppress"
    );
    (b, now)
}

/// PROVE the PET-03 workload's state: pet configured, pet invisible, the ink
/// map REAL (sized to the grid, mostly inked, live edge present), and the
/// settled early-out steady state pinned (FL-1, fixed). Returns the warmed
/// fixture + clock + the ink-map counts for the volume group.
fn verify_pet_invisible() -> (BenchApp, Instant, usize, usize) {
    let (mut b, t0) = f_pet_invisible();
    let mut now = t0;
    for _ in 0..WARM_FRAMES {
        now += FRAME_DT;
        b.compose(now);
    }
    assert!(
        b.pet_mode(),
        "pet_invisible_frame: the pet style is not configured"
    );
    assert!(
        !b.glow_enabled(),
        "pet_invisible_frame: glow enabled — the pet could be visible and the \
         hoist under price would gate nothing"
    );
    let (map_rows, inked_rows, live) = b.pet_ink_probe();
    report(
        "pet_invisible_frame",
        &format!("ink map rows {map_rows} | inked {inked_rows} | live edge {live:?}"),
    );
    assert!(
        (1..=ROWS).contains(&map_rows) && map_rows >= 20,
        "pet_invisible_frame: ink map has {map_rows} rows — the scanner is not \
         feeding the map this workload's O(rows) claim is about"
    );
    assert!(
        inked_rows >= 10,
        "pet_invisible_frame: only {inked_rows} inked rows — the fed text never \
         reached the scanner"
    );
    assert!(live.is_some(), "pet_invisible_frame: no live output edge");
    let mut presented = 0usize;
    for _ in 0..SAMPLE_FRAMES {
        now += FRAME_DT;
        presented += usize::from(b.compose(now));
    }
    report(
        "pet_invisible.settled",
        &format!(
            "presented {presented}/{SAMPLE_FRAMES} (settled compose takes the early-out — FL-1 fixed)"
        ),
    );
    // FIXED REALITY, pinned two-sided (bench finding FL-1, full story in the
    // file header): a SETTLED composed window takes the RepaintKey early-out
    // on EVERY frame, so this workload prices the settled early-out frame —
    // pass 1's extraction + the key build + the skip. The fed control below
    // keeps the pin two-sided (fresh bytes must still present). If this pin
    // ever reads nonzero again, the early-out has regressed (a re-latched
    // recovery edge or a churning RepaintKey term) — re-read the FL-1 note.
    assert_eq!(
        presented, 0,
        "pet_invisible_frame: a settled compose presented — the RepaintKey \
         early-out regressed (FL-1) — re-read the FL-1 note"
    );
    // The fed CONTROL: fresh bytes must also present (the trivial direction,
    // kept so a future early-out fix cannot silently kill presentation).
    b.feed(0, b"x");
    now += FRAME_DT;
    assert!(
        b.compose(now),
        "pet_invisible_frame: the fed control did not present"
    );
    // Re-settle before timing so the timed run continues from the verified
    // settled state.
    for _ in 0..8 {
        now += FRAME_DT;
        b.compose(now);
    }
    (b, now, map_rows, inked_rows)
}

/// PROVE the flood workload's state: EVERY frame presents (8 KiB of fresh
/// grid damage per frame), sustained across the window.
fn verify_flood() -> (BenchApp, u64, Instant) {
    let (mut b, sid, t0) = f_flood();
    let chunk = flood_chunk();
    assert_eq!(chunk.len(), FLOOD_CHUNK);
    let mut now = t0;
    for _ in 0..WARM_FRAMES {
        now += FRAME_DT;
        b.feed(sid, &chunk);
        b.compose(now);
    }
    let mut presented = 0usize;
    for _ in 0..SAMPLE_FRAMES {
        now += FRAME_DT;
        b.feed(sid, &chunk);
        presented += usize::from(b.compose(now));
    }
    report(
        "flood_present",
        &format!(
            "presented {presented}/{SAMPLE_FRAMES} | {FLOOD_CHUNK} B/frame into the focused pane"
        ),
    );
    assert_eq!(
        presented, SAMPLE_FRAMES,
        "flood_present: a flooded frame did not present — the compose is not hot"
    );
    (b, sid, now)
}

/// PROVE the echo workload's state: every frame's rasterized pixels differ
/// from the previous frame's (the echo visibly landed), and the just-typed
/// glyph is on the presented grid one cell left of the live cursor.
fn verify_echo() -> (BenchApp, Instant, u32) {
    let (mut b, t0) = f_echo();
    let mut now = t0;
    let mut k = 0u32;
    let mut prev = 0u64;
    let mut checked = 0usize;
    for i in 0..(WARM_FRAMES + SAMPLE_FRAMES) {
        let c = echo_arm(&mut b, &mut now, &mut k);
        // UNTIMED verify pass: the HASHED entry — the guards keep the exact
        // frame-identity fold while the timed loop (which calls the fold-free
        // `present_frame`) never pays it (RE-2).
        let sum = b.present_frame_hashed(now);
        // RE-1 PARITY GUARD (the damage_differential idiom, in-bench): the
        // damage-tracked raster the timed loop prices must be byte-identical
        // to a full repaint of the same scratch — asserted on EVERY verify
        // frame, so the timed loop inherits a proven-parity model without
        // ever paying the witness.
        let (cached_fnv, full_fnv) = b.parity_hashes();
        assert_eq!(
            cached_fnv, full_fnv,
            "keystroke_echo: damage-tracked raster diverged from the full repaint"
        );
        if i >= WARM_FRAMES {
            assert_ne!(
                sum, prev,
                "keystroke_echo: two consecutive frames rasterized identically — \
                 the echo never reached the pixels"
            );
            if let Some(ch) = c {
                let (row, col) = b.cursor_pos();
                assert!(col >= 1, "echo cursor at column 0 after a printable echo");
                assert_eq!(
                    b.scratch_cell(usize::from(row), usize::from(col) - 1),
                    ch,
                    "keystroke_echo: the echoed glyph is not on the presented grid"
                );
                checked += 1;
            }
        }
        prev = sum;
    }
    report(
        "keystroke_echo",
        &format!("echo glyph verified on {checked}/{SAMPLE_FRAMES} frames (rest: line wraps)"),
    );
    assert!(
        checked > SAMPLE_FRAMES * 8 / 10,
        "keystroke_echo: too few verified echoes"
    );
    (b, now, k)
}

/// THE TAB-STRIP FRAME fixture, PROVEN before it is timed.
///
/// A screenful of text (so a full re-extract has real work to skip), then the
/// two arms are run alternately for a warm-up and CHECKED for two things a
/// timer cannot see:
///
///   * REACH — `strip_frame_full` really takes the full re-extract arm and
///     `strip_frame_scoped` really takes the scoped one. Two arms that both fall
///     back would price identical work and report a confident zero.
///   * PARITY — the damage-tracked raster the timed loop prices is byte-identical
///     to a full repaint of the same scratch, on every verify frame. That is the
///     same `damage_differential` witness `verify_echo` carries, and here it is
///     load-bearing twice over: the scoped arm RETAINS rows, so a wrong retention
///     would show up as a raster that no longer matches the full repaint.
fn verify_strip() -> BenchApp {
    let mut b = BenchApp::headless();
    b.enable_tab_strip();
    for i in 0..20 {
        b.feed(0, &bland_line(i));
    }
    b.feed(0, b"user@host demo $ ");
    let mut tick = 0u8;
    // The full arm never chains, so every one of its frames must read Full.
    let _ = b.strip_present(1, false);
    for _ in 0..WARM_FRAMES {
        b.strip_echo(&mut tick);
        assert!(
            !b.strip_present(1, false),
            "strip_frame_full: the pre-fix reclaim took the scoped arm — the A/B \
             would be pricing the same path twice"
        );
        let (cached_fnv, full_fnv) = b.parity_hashes();
        assert_eq!(
            cached_fnv, full_fnv,
            "strip_frame_full: damage-tracked raster diverged from the full repaint"
        );
    }
    // The scoped arm chains from its second frame on.
    let _ = b.strip_present(1, true);
    let mut scoped = 0usize;
    for _ in 0..WARM_FRAMES {
        b.strip_echo(&mut tick);
        if b.strip_present(1, true) {
            scoped += 1;
        }
        let (cached_fnv, full_fnv) = b.parity_hashes();
        assert_eq!(
            cached_fnv, full_fnv,
            "strip_frame_scoped: damage-tracked raster diverged from the full repaint"
        );
    }
    assert_eq!(
        scoped, WARM_FRAMES,
        "strip_frame_scoped reached the damage-scoped arm on only {scoped}/{WARM_FRAMES} \
         warm frames — the arm does not price what it is named for"
    );
    report(
        "strip_frame",
        &format!(
            "full arm Full on {WARM_FRAMES}/{WARM_FRAMES}, scoped arm Scoped on {scoped}/{WARM_FRAMES}"
        ),
    );
    b
}

/// The keystroke arm: one HUMAN key through the real input seam, its echo fed
/// by hand (the loop a real PTY closes), a `\r\n` every `ECHO_LINE` strokes so
/// the line never hits the right margin. Returns the echoed char, `None` on
/// the wrap strokes (their frame moves the cursor, not a new glyph).
/// THE SPLIT WORKLOAD'S GUARDS, two-sided, before a nanosecond is timed.
///
/// REACH is the whole point here and it is asserted PER PANE, not per frame. A
/// composed frame extracts once per visible pane, so "some extraction took the
/// scoped arm" is exactly the claim that reads as proof and is not: the
/// BACKGROUND panes have taken it since audit-2 item 11, and an A/B whose two
/// arms differed in nothing would sail past a frame-level check.
///
/// So both arms are pinned to an exact census:
///
/// * `split_compose_full`   — `SPLIT_PANES - 1` scoped + EXACTLY 1 full. The
///   one full is the focused pane, forced by the disown; the backgrounds stay
///   scoped, which is what makes the focused extraction the ONLY difference
///   between the arms.
/// * `split_compose_scoped` — `SPLIT_PANES` scoped, ZERO full.
///
/// Both arms must also PRESENT every frame: an early-out frame prices a
/// RepaintKey comparison, not an extraction, and would silently equalize the
/// two arms.
fn verify_split() -> (BenchApp, Instant, u8) {
    let (mut b, mut now) = f_split();
    assert_eq!(
        b.pane_count(),
        SPLIT_PANES,
        "the split fixture did not build the shape it claims"
    );
    let mut tick = 0u8;
    // K2 RETENTION REACH, counted per arm over the warm frames.
    let (mut full_retained, mut scoped_retained) = (usize::MAX, usize::MAX);
    // The FULL arm never chains, so every one of its frames reads the same.
    for i in 0..WARM_FRAMES {
        now += FRAME_DT;
        let (presented, scoped, full) = split_frame(&mut b, now, &mut tick, true);
        assert!(
            presented,
            "split_compose_full: frame {i} took the early-out"
        );
        // From the SECOND warm frame on: the first compose after a fixture is
        // built extracts panes whose damage session is still `Damage::Full`,
        // and a full-damage fill publishes NO row-revision lane at all — so its
        // ledger entry cannot be compared and the frame after it is the first
        // that can retain anything. That is the D-2 lane's own contract, not a
        // property of this fixture.
        if i > 0 {
            full_retained = full_retained.min(split_retained(&b));
        }
        assert_eq!(
            (scoped, full),
            ((SPLIT_PANES - 1) as u64, 1),
            "split_compose_full frame {i}: expected the focused pane on the FULL \
             arm and every background pane still scoped — the A/B would otherwise \
             be pricing something other than the focused extraction"
        );
    }
    // The scoped arm chains from its first un-disowned frame on.
    now += FRAME_DT;
    let _ = split_frame(&mut b, now, &mut tick, false);
    for i in 0..WARM_FRAMES {
        now += FRAME_DT;
        let (presented, scoped, full) = split_frame(&mut b, now, &mut tick, false);
        assert!(
            presented,
            "split_compose_scoped: frame {i} took the early-out"
        );
        assert_eq!(
            (scoped, full),
            (SPLIT_PANES as u64, 0),
            "split_compose_scoped frame {i}: EVERY pane must take the damage-scoped \
             arm — the arm does not price what it is named for"
        );
        if i > 0 {
            scoped_retained = scoped_retained.min(split_retained(&b));
        }
    }
    // K2: the echo damages exactly ONE grid row, so a settled frame of this
    // fixture may leave every other composed row exactly as it is. Pinned at the
    // exact number on the WORST warm frame of each arm — a lane that refused
    // even once would read here, and a fixture that never armed would read 0.
    let want_retained = SPLIT_ROWS as usize - 1;
    assert_eq!(
        (scoped_retained, full_retained),
        (want_retained, want_retained),
        "the composed retention lane did not reach the split arms: scoped kept \
         {scoped_retained} and full kept {full_retained} of {SPLIT_ROWS} composed \
         rows on their worst warm frame after the first, against the \
         {want_retained} a one-row echo leaves untouched — these arms would be \
         pricing a lane that refused"
    );
    // THE DARK HALF OF THE RAIN REACH GUARD. `split_rain_compose` is only a
    // rain measurement if these two fixtures differ in rain and this one has
    // none: no engine was ever built (the D-1 zero-cost pin), nothing scanned,
    // and no sprite reached the frame. Asserted here rather than assumed from
    // "we never called `rain_on`", because the same three readings are what
    // the lit arm asserts the positive of — one witness, both directions.
    let dark = b.rain_witness();
    assert!(
        !dark.engine && !dark.active && dark.quads == 0 && dark.scanned_epoch.is_none(),
        "split_compose_* is the rain-OFF control and must be dark (engine={}, \
         active={}, quads={}, scanned={:?})",
        dark.engine,
        dark.active,
        dark.quads,
        dark.scanned_epoch
    );
    report(
        "split_compose",
        &format!(
            "{SPLIT_PANES} panes at {SPLIT_ROWS}x{SPLIT_COLS} | \
             full arm {}+1 scoped+full on {WARM_FRAMES}/{WARM_FRAMES} | \
             scoped arm {SPLIT_PANES}+0 on {WARM_FRAMES}/{WARM_FRAMES} | \
             retained {want_retained}/{SPLIT_ROWS} composed rows on every warm \
             frame after the first of BOTH arms | rain DARK (no engine, no scan, no quads)",
            SPLIT_PANES - 1
        ),
    );
    (b, now, tick)
}

/// `after - before` over the per-clause full-refill ledger, rendered as
/// `clause:frames` pairs (or `none` when nothing refused). Only clauses that
/// MOVED are printed — the ledger is process-wide and cumulative, so a warm-up
/// era's refusals would otherwise be reported as this window's.
fn refill_cause_delta(before: &[(&'static str, u64)], after: &[(&'static str, u64)]) -> String {
    let mut out: Vec<String> = Vec::new();
    for (cause, frames) in after {
        let was = before
            .iter()
            .find(|(c, _)| c == cause)
            .map_or(0, |(_, f)| *f);
        if *frames > was {
            out.push(format!("{cause}:{}", frames - was));
        }
    }
    if out.is_empty() {
        "none".to_string()
    } else {
        out.join(",")
    }
}

/// PROVE THE RAIN ARM, four ways, on EVERY sampled frame.
///
/// An effects fixture that silently fails to arm is the single most repeated
/// failure this campaign has had, and rain has more ways to stay dark than any
/// other effect here: the per-session override, the serious-mode policy,
/// reduced motion, the load-shed latch, alt-screen suppression, a non-zero
/// display offset, an empty literal material bank, and the drain. An
/// `enabled = true` in the config proves none of them.
///
/// So the guard reads the state the FRAME left behind, at the END of the
/// pipeline, and refuses to accept any one reading as the whole answer:
///
/// * `engine` + `active` — the window really built an engine and the engine
///   itself says it is live (`is_active` folds material, drain, reading gate
///   and whether the last emit produced light).
/// * `quads > 0` — sprites actually reached the COMPOSED frame, past the pane
///   translation and the clip to the focused pane's box. This is the reading a
///   dark fixture cannot fake.
/// * `scanned_epoch` ADVANCING — the Tier-A occupancy rescan consumed THIS
///   frame's damage epoch. This is the one that pins the workload to the
///   finding: the duplicate focused extract under audit exists only on frames
///   where `rain_refresh` fires, and `rain_refresh` firing is exactly what
///   moves this number. A sticky "scanned once" or a frozen epoch would mean
///   the frames being timed are not the frames the finding is about.
///
/// The refill census is asserted too, but WEAKLY on purpose: `scoped + full`
/// must equal the pane count (every pane extracted, nothing took a silent
/// early-out) and the three BACKGROUND panes must stay scoped, so the arms
/// differ only at the focused pane. The focused pane's own arm is REPORTED,
/// not asserted, because it is the thing the fix under test changes —
/// asserting it would make this guard fail on exactly one side of the A/B it
/// exists to referee.
fn verify_split_rain() -> (BenchApp, Instant, u8) {
    let (mut b, mut now) = f_split_rain();
    assert_eq!(
        b.pane_count(),
        SPLIT_PANES,
        "the rain split fixture did not build the shape it claims"
    );
    let mut tick = 0u8;
    // Warm: spawn the field, sample the literal material bank, finish the
    // atlas bake, settle the weather at WORKING off the echo script.
    for _ in 0..WARM_FRAMES {
        now += FRAME_DT;
        let _ = split_frame(&mut b, now, &mut tick, false);
    }
    let mut prev_scan = b
        .rain_witness()
        .scanned_epoch
        .expect("the rain warm-up must have scanned at least once");
    let mut min_quads = usize::MAX;
    let mut focus_full = 0u64;
    // A CONTENT FINGERPRINT OF THE FIELD ITSELF, folded over the sampled
    // window. The arm exists to referee a change to which BUFFER the Tier-A
    // rescan and the literal material sample read; if that change altered what
    // rain draws, it is a rendering change and not an optimization at all.
    // Per-frame quad counts are a cheap, order-sensitive proxy for the emitted
    // field (they move with occupancy, weather, drops, drain and clearance),
    // and this fold makes the whole 300-frame sequence one comparable number
    // rather than a spot check. Printed on BOTH sides of the A/B.
    let mut field_fold = 0u64;
    let causes_before = b.refill_full_causes();
    for i in 0..SAMPLE_FRAMES {
        now += FRAME_DT;
        let (presented, scoped, full) = split_frame(&mut b, now, &mut tick, false);
        assert!(
            presented,
            "split_rain_compose: frame {i} took the early-out"
        );
        let w = b.rain_witness();
        assert!(
            w.engine && w.active,
            "split_rain_compose frame {i}: rain is NOT live (engine={}, active={}) — \
             the arm would price a rain-shaped frame with no rain in it",
            w.engine,
            w.active
        );
        assert!(
            w.quads > 0,
            "split_rain_compose frame {i}: no rain quad reached the composed frame"
        );
        let scan = w
            .scanned_epoch
            .expect("a live rain engine has scanned at least once");
        assert!(
            scan != prev_scan,
            "split_rain_compose frame {i}: the Tier-A rescan did not consume a NEW \
             damage epoch ({scan}) — this frame is not a rain-refresh frame, which \
             is the only kind the finding is about"
        );
        prev_scan = scan;
        min_quads = min_quads.min(w.quads);
        field_fold = field_fold.rotate_left(7) ^ (w.quads as u64);
        assert_eq!(
            scoped + full,
            SPLIT_PANES as u64,
            "split_rain_compose frame {i}: {scoped} scoped + {full} full is not one \
             extraction per pane"
        );
        assert!(
            scoped >= (SPLIT_PANES - 1) as u64,
            "split_rain_compose frame {i}: a BACKGROUND pane left the scoped arm \
             ({scoped} scoped) — the arms must differ only at the focused pane"
        );
        focus_full += full;
    }
    // WHICH CLAUSE refused, by name, over exactly the sampled window. The
    // finding this arm exists to referee claims the extra `take_damage` inside
    // the rain-refresh block is what costs the focused pane its scoped arm; if
    // that is right the delta is `damage_taken` and nothing else, and if it is
    // wrong the stated mechanism is wrong and so is the proposed fix. Reported
    // rather than asserted for the same reason `focus_full` is: it is the
    // quantity the fix under test changes.
    let causes = refill_cause_delta(&causes_before, &b.refill_full_causes());
    report(
        "split_rain_compose",
        &format!(
            "{SPLIT_PANES} panes at {SPLIT_ROWS}x{SPLIT_COLS} | rain LIVE on \
             {SAMPLE_FRAMES}/{SAMPLE_FRAMES} frames | min quads {min_quads} | \
             rescan epoch advanced {SAMPLE_FRAMES}/{SAMPLE_FRAMES} | \
             focused pane on the FULL arm {focus_full}/{SAMPLE_FRAMES} | \
             refusing clause {causes} | field fold {field_fold:#018x}"
        ),
    );
    (b, now, tick)
}

fn echo_arm(b: &mut BenchApp, now: &mut Instant, k: &mut u32) -> Option<char> {
    *now += TYPE_DT;
    *k += 1;
    if (*k).is_multiple_of(ECHO_LINE) {
        b.feed(0, b"\r\nuser@host demo $ ");
        return None;
    }
    let c = char::from(b'a' + u8::try_from(*k % 26).expect("mod 26"));
    assert!(b.keystroke(c), "keystroke rejected by the input seam");
    b.feed(0, &[c as u8]);
    Some(c)
}

/// PROVE the many-tabs workload's state: N tabs resident, NOTHING animating,
/// the settled early-out steady state pinned (FL-1, fixed), the fed control
/// presenting.
fn verify_many_tabs(n: usize) -> (BenchApp, Instant) {
    let (mut b, active_sid, t0) = f_many_tabs(n);
    let mut now = t0;
    for _ in 0..WARM_FRAMES {
        now += FRAME_DT;
        b.compose(now);
    }
    assert_eq!(
        b.tab_count(),
        n,
        "many_tabs_idle: fixture staged the wrong tab count"
    );
    // THE PRECONDITION OF THE WORD "IDLE", checked rather than assumed: the
    // shipped default seats a resident walking pet, and with one on screen no
    // frame is ever settled (see `f_many_tabs`). If this ever reads true the
    // pin below is measuring an animating window again.
    assert!(
        !b.glow_enabled(),
        "many_tabs_idle: a cursor effect is live — an animating window has no \
         settled frame to price, and this workload's whole claim is the settled \
         early-out (see `f_many_tabs`)"
    );
    let mut presented = 0usize;
    for _ in 0..SAMPLE_FRAMES {
        now += FRAME_DT;
        presented += usize::from(b.compose(now));
    }
    report(
        &format!("many_tabs_idle/{n}"),
        &format!(
            "tabs {n} | effects off | presented {presented}/{SAMPLE_FRAMES} \
             (settled early-out — FL-1 fixed)"
        ),
    );
    // Same FL-1 pin as pet_invisible_frame: a settled window takes the
    // early-out every frame, so this workload prices the steady-state
    // early-out frame as tab count scales. Two-sided on purpose — the fed
    // control below must still present; a nonzero settled count means the
    // early-out regressed.
    assert_eq!(
        presented, 0,
        "many_tabs_idle: a settled compose presented — the RepaintKey \
         early-out regressed (FL-1) — re-read the FL-1 note"
    );
    b.feed(active_sid, b"y");
    now += FRAME_DT;
    assert!(
        b.compose(now),
        "many_tabs_idle: the fed control did not present"
    );
    for _ in 0..8 {
        now += FRAME_DT;
        b.compose(now);
    }
    (b, now)
}

/// PROVE the scrolled-back workload's state and, with it, the whole
/// invalidation surface any materialized-row memo would have to survive.
///
/// The guard set:
///   * REACH: `display_offset == SB_DEPTH` on every sampled frame, and the
///     history under it is real (`scrollback_lines >= SB_DEPTH + ROWS`), so
///     all 24 viewport rows resolve through the history materializer and none
///     through the live ring.
///   * IDENTITY: every row of every sampled frame matches the one formula
///     (see the file header) — read back out of the frame the present
///     actually extracted, not out of the grid.
///   * INVALIDATION, walked deliberately: bottom -> back (the viewport moves
///     wholesale), a line ARRIVING while scrolled back (history grows under
///     the viewport, so every row's history index shifts), and a depth change
///     (a different, overlapping window). Identity is re-asserted immediately
///     after each — these are exactly the three ways a row memo goes stale,
///     and a stale hit is a wrong glyph on screen, which this catches.
///
/// Returns the warmed fixture + clock + the history depth for the volume
/// group; the timed run continues from the verified state.
fn verify_scrolled_back() -> (BenchApp, Instant, usize) {
    let (mut b, t0) = f_scrollback(SB_DEPTH);
    let mut now = t0;
    for _ in 0..SB_WARM_FRAMES {
        now += FRAME_DT;
        b.present_frame(now);
    }
    assert_eq!(
        b.display_offset(),
        SB_DEPTH,
        "scrolled_back_repaint: the fixture is not scrolled back"
    );
    assert!(
        b.scrollback_lines() >= SB_DEPTH + ROWS,
        "scrolled_back_repaint: history is too shallow ({}) for a {SB_DEPTH}-line \
         scroll with {ROWS} rows — some viewport rows would be LIVE rows",
        b.scrollback_lines()
    );
    let mut checked = 0usize;
    for _ in 0..SAMPLE_FRAMES {
        now += FRAME_DT;
        b.present_frame(now);
        assert_eq!(
            b.display_offset(),
            SB_DEPTH,
            "scrolled_back_repaint: the viewport moved during the sample window"
        );
        checked += assert_sb_identity(&b, SB_FILL_LINES, "scrolled_back_repaint");
    }
    let depth = b.scrollback_lines();
    report(
        "scrolled_back_repaint",
        &format!(
            "offset {SB_DEPTH} | history {depth} lines | history rows/frame {ROWS} | \
             identity checked {checked}/{}",
            SAMPLE_FRAMES * ROWS
        ),
    );
    assert_eq!(
        checked,
        SAMPLE_FRAMES * ROWS,
        "scrolled_back_repaint: some viewport row carried no fill line — the guard \
         walked a frame that is not showing history"
    );

    // INVALIDATION WALK. Each step changes exactly one thing a row memo can be
    // keyed wrong on, and identity is re-asserted on the very next frame.
    b.scroll_to_bottom();
    now += FRAME_DT;
    b.present_frame(now);
    assert_eq!(
        b.display_offset(),
        0,
        "scroll_to_bottom did not reach the bottom"
    );
    assert_sb_identity(&b, SB_FILL_LINES, "after scroll_to_bottom");
    b.scroll_display(sb_delta(SB_DEPTH));
    now += FRAME_DT;
    b.present_frame(now);
    assert_sb_identity(&b, SB_FILL_LINES, "after scrolling back");
    // Output ARRIVING while scrolled back: `scrollback_lines` grows, so the
    // history index of every visible row shifts by one under a live memo.
    let before = b.scrollback_lines();
    b.feed(0, &sb_line(SB_FILL_LINES));
    now += FRAME_DT;
    b.present_frame(now);
    assert!(
        b.scrollback_lines() > before,
        "the fed line did not enter history — the invalidation guard would prove nothing"
    );
    assert_sb_identity(
        &b,
        SB_FILL_LINES + 1,
        "after a line arrived while scrolled back",
    );
    // A different, OVERLAPPING depth — the wheel-scroll shape, checked once
    // here even though the moving arm below prices it.
    b.scroll_display(sb_delta(SB_WHEEL));
    now += FRAME_DT;
    b.present_frame(now);
    assert_sb_identity(&b, SB_FILL_LINES + 1, "after a wheel-sized depth change");

    // Re-park at the verified depth so the timed run measures the stationary
    // state this workload is named for.
    b.scroll_to_bottom();
    b.scroll_display(sb_delta(SB_DEPTH));
    for _ in 0..8 {
        now += FRAME_DT;
        b.present_frame(now);
    }
    assert_eq!(b.display_offset(), SB_DEPTH);
    (b, now, depth)
}

/// PROVE the CONTROL's state: the identical fixture and script at the LIVE
/// BOTTOM. Its only difference from `verify_scrolled_back`'s arm is
/// `display_offset == 0`, which is precisely what makes the subtraction
/// meaningful — so the guard asserts that difference and nothing else.
fn verify_live_bottom() -> (BenchApp, Instant) {
    let (mut b, t0) = f_scrollback(0);
    let mut now = t0;
    for _ in 0..SB_WARM_FRAMES {
        now += FRAME_DT;
        b.present_frame(now);
    }
    let mut checked = 0usize;
    for _ in 0..SAMPLE_FRAMES {
        now += FRAME_DT;
        b.present_frame(now);
        assert_eq!(
            b.display_offset(),
            0,
            "live_bottom_repaint: the control is not at the live bottom"
        );
        checked += assert_sb_identity(&b, SB_FILL_LINES, "live_bottom_repaint");
    }
    report(
        "live_bottom_repaint",
        &format!(
            "offset 0 | history {} lines | history rows/frame 0 | identity checked \
             {checked}/{}",
            b.scrollback_lines(),
            SAMPLE_FRAMES * ROWS
        ),
    );
    // ROWS-1, not ROWS: at the bottom the last viewport row is the empty
    // cursor line, which carries no fill number. That one-row difference IS
    // the two-sided proof that this arm is at the bottom and the other is not.
    assert_eq!(
        checked,
        SAMPLE_FRAMES * (ROWS - 1),
        "live_bottom_repaint: the control is not showing the live tail"
    );
    (b, now)
}

/// PROVE the moving arm's state: the viewport really moves every frame, by
/// exactly one notch, inside the bounded span, and identity holds on every
/// sampled frame (a scrub is where an absolute-row-keyed memo is most likely
/// to hand back the wrong row).
fn verify_wheel() -> (BenchApp, Instant, i32) {
    let (mut b, t0) = f_scrollback(SB_DEPTH);
    let mut now = t0;
    let mut dir = 1i32;
    for _ in 0..SB_WARM_FRAMES {
        wheel_arm(&mut b, &mut dir);
        now += FRAME_DT;
        b.present_frame(now);
    }
    let mut moved = 0usize;
    let mut checked = 0usize;
    let (mut lo, mut hi) = (usize::MAX, 0usize);
    for _ in 0..SAMPLE_FRAMES {
        let before = b.display_offset();
        wheel_arm(&mut b, &mut dir);
        let after = b.display_offset();
        moved += usize::from(after != before);
        lo = lo.min(after);
        hi = hi.max(after);
        now += FRAME_DT;
        b.present_frame(now);
        checked += assert_sb_identity(&b, SB_FILL_LINES, "scrolled_back_wheel");
    }
    report(
        "scrolled_back_wheel",
        &format!(
            "moved {moved}/{SAMPLE_FRAMES} frames | offset span {lo}..={hi} | \
             {SB_WHEEL} lines/notch | identity checked {checked}/{}",
            SAMPLE_FRAMES * ROWS
        ),
    );
    assert_eq!(
        moved, SAMPLE_FRAMES,
        "scrolled_back_wheel: a frame did not move the viewport — the arm clamped \
         and this is measuring the stationary workload"
    );
    assert!(
        lo >= SB_DEPTH && hi <= SB_DEPTH + SB_WHEEL_SPAN + SB_WHEEL,
        "scrolled_back_wheel: the scrub left its span ({lo}..={hi})"
    );
    assert_eq!(
        checked,
        SAMPLE_FRAMES * ROWS,
        "scrolled_back_wheel: a sampled frame was not showing pure history"
    );
    (b, now, dir)
}

// ---------------------------------------------------------------- counting --

/// Record a COUNT as a criterion measurement — 1 ns == 1 item, verbatim the
/// aterm-effects convention (see cursor_glow_tick.rs for why the spin loop
/// and the `k % 4` jitter are not ceremony: the spin keeps criterion's
/// wall-clock warm-up from doubling `iters` forever, and the jitter keeps the
/// sample distribution non-degenerate so the PDF plot never divides by zero).
fn bench_count(g: &mut BenchmarkGroup<'_, WallTime>, id: &str, count: usize) {
    assert!(
        count > 0,
        "{id}: a zero count cannot be recorded as a duration"
    );
    let n = count as u64;
    let mut k = 0u64;
    g.bench_function(BenchmarkId::from_parameter(id), |b| {
        b.iter_custom(|iters| {
            let mut spin = 0u64;
            for i in 0..iters {
                spin = spin.wrapping_add(black_box(i));
            }
            black_box(spin);
            k = k.wrapping_add(1);
            Duration::from_nanos(n.saturating_mul(iters).saturating_add(k % 4))
        });
    });
}

/// PROVE the redundant-BACKGROUND state every CPU-raster arm in this group
/// renders in — TWO-SIDED, on real presented frames, and on a counter that
/// reads the same on either side of the background elision it exists to gate
/// (`Renderer::last_bg_runs` counts the runs `render_row_bg` RESOLVED, not the
/// fills it emitted).
///
/// TARGET — the echo fixture's own content: ordinary text on the DEFAULT
/// background. Every background run such a row resolves carries exactly the
/// colour the band's base already holds, so `at_base > 0` on a presented frame.
///
/// CONTROL — the same fixture with every column of every row wrapped in an SGR
/// background colour. Those runs resolve to a colour the base does NOT hold, so
/// `at_base == 0` while `total` stays positive. Without this half the target's
/// half is an assertion an EMPTY frame would also satisfy: a frame that
/// rastered no row resolves no runs at all, and "no run disagreed with the
/// base" would read as reach where there was none.
fn verify_bg_runs() {
    let (mut tgt, t0) = f_echo();
    // A frame with REAL damage: an idle re-present has an empty dirty set, so
    // its row background pass never runs and the probe would read 0/0 — which
    // is exactly the vacuous reading the control half exists to catch.
    tgt.feed(0, b"echo hello");
    tgt.present_frame(t0 + FRAME_DT);
    let (total, at_base) = tgt.bg_run_probe();
    report(
        "bg_runs_target",
        &format!("{at_base}/{total} resolved bg runs carry the band base colour"),
    );
    assert!(
        total > 0,
        "bg_runs_target: the presented frame resolved NO background run — the \
         fixture never reached the row background pass"
    );
    assert!(
        at_base > 0,
        "bg_runs_target: {at_base}/{total} runs carry the band base colour — the \
         plain-text fixture does not reach the redundant-background state at all"
    );

    let mut ctl = BenchApp::headless();
    let (rows, cols) = ctl.grid();
    // Home, then an SGR BACKGROUND colour, then a glyph in EVERY column of
    // every row: no column anywhere resolves to the frame default, so a run
    // that matched the band base would be a bug in the counter, not content.
    let mut paint: Vec<u8> = b"\x1b[H\x1b[44m".to_vec();
    for r in 0..rows {
        paint.extend(std::iter::repeat_n(b'x', cols as usize));
        if r + 1 < rows {
            paint.extend_from_slice(b"\r\n");
        }
    }
    let c0 = Instant::now();
    ctl.present_frame(c0);
    ctl.feed(0, &paint);
    ctl.present_frame(c0 + FRAME_DT);
    let (ctl_total, ctl_at_base) = ctl.bg_run_probe();
    report(
        "bg_runs_control",
        &format!("{ctl_at_base}/{ctl_total} resolved bg runs carry the band base colour"),
    );
    assert!(
        ctl_total > 0,
        "bg_runs_control: the presented frame resolved NO background run"
    );
    assert_eq!(
        ctl_at_base, 0,
        "bg_runs_control: {ctl_at_base}/{ctl_total} runs still carry the band base \
         colour on an all-SGR-background grid — the counter is not reading content"
    );
}

// -------------------------------------------------------------- the groups --

#[allow(
    clippy::too_many_lines,
    reason = "one linear registry of verified workloads, the effects-bench shape"
)]
fn frame_latency(c: &mut Criterion) {
    // PROVE FIRST, TIME SECOND: every fixture is built, warmed and verified
    // before a nanosecond is measured; the timed run continues from the
    // verified state with the same arm sustaining it.
    verify_bg_runs();
    let (mut fx_off, mut off_now) = verify_effects_off();
    let (mut pet, mut pet_now, pet_map_rows, pet_inked) = verify_pet_invisible();
    let (mut flood, flood_sid, mut flood_now) = verify_flood();
    let chunk = flood_chunk();
    let (mut echo, mut echo_now, mut echo_k) = verify_echo();
    let mut tabs: Vec<(usize, BenchApp, Instant)> = [2usize, 8, 32]
        .into_iter()
        .map(|n| {
            let (b, now) = verify_many_tabs(n);
            (n, b, now)
        })
        .collect();
    let (mut sb_deep, mut sb_deep_now, sb_depth) = verify_scrolled_back();
    let (mut sb_live, mut sb_live_now) = verify_live_bottom();
    let (mut sb_wheel, mut sb_wheel_now, mut sb_wheel_dir) = verify_wheel();
    let mut strip = verify_strip();
    let (mut split, mut split_now, mut split_tick) = verify_split();
    let (mut rain, mut rain_now, mut rain_tick) = verify_split_rain();

    {
        let mut group = c.benchmark_group("frame_latency");
        // THE FLOOR UNDER EVERY NUMBER IN THIS GROUP: the `Instant::now()`
        // pair with an empty span between them.
        group.bench_function("timer_floor", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let t0 = Instant::now();
                    black_box(0u64);
                    total += t0.elapsed();
                }
                total
            });
        });
        // 1. CF-6: the whole per-frame cursor-effect driver on an all-off
        // frame — policy resolves, config folds, cadence triple, ignite,
        // every engine's own early-out. The arm keeps the cadence saturated.
        group.bench_function("effects_off_frame", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    off_now += TYPE_DT;
                    fx_off.pulse_typing(off_now);
                    let t0 = Instant::now();
                    black_box(fx_off.tick_fx(off_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 2. PET-03: a SETTLED composed frame with the pet configured but
        // invisible — post-FL-1-fix this is the settled EARLY-OUT frame (see
        // the pin in its verify fn): pass-1 extraction + key build + skip.
        // The unconditional pet feed the finding's hoist would delete still
        // runs inside it.
        group.bench_function("pet_invisible_frame", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    pet_now += FRAME_DT;
                    let t0 = Instant::now();
                    black_box(pet.compose(pet_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 3. HEAVY LOAD: 8 KiB ingested (untimed arm), the hot compose timed.
        group.bench_function("flood_present", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    flood_now += FRAME_DT;
                    flood.feed(flood_sid, &chunk);
                    let t0 = Instant::now();
                    black_box(flood.compose(flood_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 4. The latency-critical path: keystroke armed, the echoing frame
        // (extract + effects + real CPU raster) timed.
        group.bench_function("keystroke_echo", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    echo_arm(&mut echo, &mut echo_now, &mut echo_k);
                    let t0 = Instant::now();
                    black_box(echo.present_frame(echo_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 4b. THE TAB-STRIP FRAME (D-2 SPLICE / DMG-1 reach). The only workload
        // in this group with `tab_strip_rows != 0` — every other one runs at 0,
        // which the header lists among the honest cuts and which is exactly why
        // none of them can price this seam.
        //
        // ONE BINARY, TWO ARMS, so the A/B carries no build difference at all:
        // `strip_frame_full` is the PRE-FIX reclaim (the prepend is never
        // inverted, so the extractor falls back to a full re-extract on
        // literally every frame — a78dd8a1's recorded residue),
        // `strip_frame_scoped` inverts it. Everything else — the same keystroke
        // arm, the same splice, the same real CPU raster through the persistent
        // damage cache — is identical.
        //
        // REACH, asserted on BOTH sides rather than assumed: an arm that
        // silently stopped taking the arm it is named for would mis-price this
        // seam in whichever direction the noise happened to point.
        {
            let mut strip_tick = 0u8;
            assert!(
                !strip.strip_present(1, false),
                "strip_frame_full must take the FULL arm"
            );
            strip.strip_echo(&mut strip_tick);
            assert!(
                !strip.strip_present(1, false),
                "strip_frame_full must keep taking the FULL arm"
            );
            group.bench_function("strip_frame_full", |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        strip.strip_echo(&mut strip_tick);
                        let t0 = Instant::now();
                        black_box(strip.strip_present(1, false));
                        total += t0.elapsed();
                    }
                    total
                });
            });
            // Re-establish the chain for the scoped arm: the first inverted
            // frame is still Full (the scratch it inherits was never blessed),
            // the second must be Scoped.
            let _ = strip.strip_present(1, true);
            strip.strip_echo(&mut strip_tick);
            assert!(
                strip.strip_present(1, true),
                "strip_frame_scoped must take the SCOPED arm — the bench would \
                 otherwise price two identical full re-extracts against each other"
            );
            group.bench_function("strip_frame_scoped", |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        strip.strip_echo(&mut strip_tick);
                        let t0 = Instant::now();
                        black_box(strip.strip_present(1, true));
                        total += t0.elapsed();
                    }
                    total
                });
            });
        }
        // 4c. THE SPLIT COMPOSE FRAME (the compose-pane-amplification audit
        // item). ONE BINARY, TWO ARMS, differing in EXACTLY ONE THING: whether
        // the FOCUSED pane's extraction may chain from its own resident buffer.
        //
        // `split_compose_full` disowns that buffer before every frame, which is
        // the pre-fix focused extraction verbatim (the historical
        // `cell_frame_into` + `take_damage` pair could not chain either), and
        // leaves the background panes on the scoped arm they have had since
        // audit-2 item 11. `split_compose_scoped` lets it chain. Same fixture,
        // same one-row echo, same per-pane blit, same decorations — so what is
        // left between the two numbers is one full O(rows x cols) engine resolve
        // of the focused pane per presented frame.
        //
        // `verify_split` pins the per-pane arm census on BOTH sides. The disown
        // itself is untimed (it is not work any shipping frame does), and it
        // costs the full arm the carrier's O(1) continuity check on top of the
        // historical pair — a handful of scalar compares against a whole-grid
        // resolve, in the only direction that is honest to state.
        group.bench_function("split_compose_full", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    split_now += FRAME_DT;
                    split.split_echo(&mut split_tick);
                    split.disown_focus_scratch();
                    let t0 = Instant::now();
                    black_box(split.compose_at(SPLIT_ROWS, SPLIT_COLS, split_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // Re-establish the chain the disowned arm above broke: the first
        // un-disowned frame is still Full, the next one may be Scoped.
        split_now += FRAME_DT;
        let _ = split_frame(&mut split, split_now, &mut split_tick, false);
        split_now += FRAME_DT;
        assert_eq!(
            split_frame(&mut split, split_now, &mut split_tick, false),
            (true, SPLIT_PANES as u64, 0),
            "split_compose_scoped must take the SCOPED arm on every pane — the \
             bench would otherwise price two identical full re-extracts against \
             each other"
        );
        group.bench_function("split_compose_scoped", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    split_now += FRAME_DT;
                    split.split_echo(&mut split_tick);
                    let t0 = Instant::now();
                    black_box(split.compose_at(SPLIT_ROWS, SPLIT_COLS, split_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 4d. THE SAME COMPOSED FRAME WITH PHOSPHOR RAIN ON — the instrument
        // the "duplicate focused-pane extract under rain" finding was refused
        // for want of. `rain_refresh` fires on every frame here (the echo moves
        // the damage epoch, so the Tier-A occupancy scan is always stale), and
        // a rain-refresh frame is the ONLY frame the finding is about.
        //
        // Its control is `split_compose_scoped` directly above: same pane
        // count, same geometry, same fill, same one-row echo, same timed call
        // — differing in `[matrix_rain] enabled` and nothing else. The
        // difference between the two numbers is therefore the whole per-frame
        // price of rain on a split compose (engine tick + occupancy rescan +
        // literal material sample + the extraction that feeds them + the quad
        // translation), which is the envelope any deletion inside that block
        // has to be a fraction of.
        //
        // `verify_split_rain` proves the rain is LIVE on every sampled frame
        // four independent ways, and `verify_split` proves the control is dark.
        group.bench_function("split_rain_compose", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    rain_now += FRAME_DT;
                    rain.split_echo(&mut rain_tick);
                    let t0 = Instant::now();
                    black_box(rain.compose_at(SPLIT_ROWS, SPLIT_COLS, rain_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 5. Steady state as resident state scales.
        for (n, b_app, now) in tabs.iter_mut() {
            group.bench_function(BenchmarkId::new("many_tabs_idle", *n), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        *now += FRAME_DT;
                        let t0 = Instant::now();
                        black_box(b_app.compose(*now));
                        total += t0.elapsed();
                    }
                    total
                });
            });
        }
        // 6. SCR-1: a presented frame over a SCROLLED-BACK, MOTIONLESS
        // viewport. Nothing about the grid changed since the last frame and
        // nothing about the viewport moved, yet the extraction re-resolves all
        // 24 rows through the 3-tier history materializer. Read this against
        // `live_bottom_repaint` below: the difference is the whole cost.
        group.bench_function("scrolled_back_repaint", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    sb_deep_now += FRAME_DT;
                    let t0 = Instant::now();
                    black_box(sb_deep.present_frame(sb_deep_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 7. THE CONTROL for 6: identical fixture, identical script, viewport
        // at the live bottom. Everything except the history read cancels.
        group.bench_function("live_bottom_repaint", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    sb_live_now += FRAME_DT;
                    let t0 = Instant::now();
                    black_box(sb_live.present_frame(sb_live_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 8. The MOVING half: one wheel notch armed (untimed), the frame
        // timed. 3 of the 24 rows are new; the other 21 were on the previous
        // frame.
        group.bench_function("scrolled_back_wheel", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    wheel_arm(&mut sb_wheel, &mut sb_wheel_dir);
                    sb_wheel_now += FRAME_DT;
                    let t0 = Instant::now();
                    black_box(sb_wheel.present_frame(sb_wheel_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        group.finish();
    }

    {
        // WHAT THE FRAME NUMBERS EXCLUDE — and the two findings' exact seams,
        // priced alone where a full-frame delta could not resolve them.
        let mut group = c.benchmark_group("frame_latency_seams");
        // CF-6 engine half, both sides of the A/B in one run: the driver's
        // PRE-FIX triple (intensity, intensity, warmth — three lazy decays for
        // one instant) vs the prepared shared sample (one decay). The gui
        // adoption replaces the former with the latter; the difference between
        // these two numbers is the engine-half win, measured on the same hot
        // tracker the effects_off arm sustains.
        group.bench_function("cadence_triple_hot", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    off_now += TYPE_DT;
                    fx_off.pulse_typing(off_now);
                    let t0 = Instant::now();
                    black_box(fx_off.cadence_triple(off_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        group.bench_function("cadence_sample_hot", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    off_now += TYPE_DT;
                    fx_off.pulse_typing(off_now);
                    let t0 = Instant::now();
                    black_box(fx_off.cadence_sample(off_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // PET-03's exact pair — producer scan + consumer copy — on the
        // verified real map. This is the number the hoist deletes from every
        // pet-invisible frame.
        group.bench_function("pet_ink_feed", |b| {
            b.iter(|| pet.pet_ink_feed());
        });
        // The echo workload's whole untimed arm (real input seam + hand-fed
        // echo), priced under a name that cannot be mistaken for the frame.
        group.bench_function("keystroke_arm", |b| {
            b.iter(|| echo_arm(&mut echo, &mut echo_now, &mut echo_k));
        });
        // RE-2's evicted witness, kept priced: the SAME modeled present PLUS
        // the full-frame frame-identity FNV fold the timed workload no longer
        // pays. `present_hashed` minus the group's `keystroke_echo` is ~the
        // price of the fold alone (measured 331 us/frame at 24x80 on this
        // campaign's machine when it still sat inside the timed span).
        group.bench_function("present_hashed", |b| {
            b.iter(|| black_box(echo.present_frame_hashed(echo_now)));
        });
        // The flood workload's untimed arm: 8 KiB through `Terminal::process`
        // under the session lock. Add it to `flood_present` and subtract the
        // timer floor for the cost of one whole flooded frame.
        group.bench_function("flood_feed_8kib", |b| {
            b.iter(|| flood.feed(flood_sid, black_box(&chunk)));
        });
        group.finish();
    }

    {
        // The guard-asserted counts as measurements (1 ns == 1 item), so a
        // count regression is stored and A/B'd like a time regression.
        let mut group = c.benchmark_group("frame_latency_volume");
        group
            .warm_up_time(Duration::from_millis(1))
            .measurement_time(Duration::from_millis(10))
            .sample_size(10);
        bench_count(&mut group, "flood_present/bytes_per_frame", FLOOD_CHUNK);
        bench_count(&mut group, "pet_invisible_frame/ink_map_rows", pet_map_rows);
        bench_count(&mut group, "pet_invisible_frame/inked_rows", pet_inked);
        // The scrolled-back arms' guard-asserted volumes: how many rows the
        // frame re-materializes (ALL of them today), how deep the history
        // under it is, and how many of those rows a notch actually replaces.
        bench_count(
            &mut group,
            "scrolled_back_repaint/history_rows_per_frame",
            ROWS,
        );
        bench_count(
            &mut group,
            "scrolled_back_repaint/scrollback_depth",
            sb_depth,
        );
        bench_count(&mut group, "scrolled_back_wheel/lines_per_notch", SB_WHEEL);
        group.finish();
    }
}

criterion_group!(benches, frame_latency);
criterion_main!(benches);
