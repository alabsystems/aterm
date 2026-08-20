// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! BENCH-ONLY observation seam for `benches/frame_latency.rs` — the
//! `test-open-probe` precedent (aterm-effects) applied to this crate.
//!
//! WHY THIS EXISTS. The frame-latency bench is an EXTERNAL target, so it sees
//! only the crate's public surface — and this crate deliberately has almost
//! none (`App`, `WindowId`, every fixture constructor: all crate-private). The
//! unit tests reach the frame path through `#[cfg(test)]` fixtures
//! (`App::headless_for_test`, `stub_session`, `split_active_stub_tab`), which
//! a bench build does not compile. This module is the ONE public wrapper those
//! fixtures are driven through: gated behind the `bench-support` feature,
//! enabled only by the bench target's `required-features`, compiled by no
//! shipping build ("No shipping crate enables this feature" — the same law the
//! effects crate's probe seam carries).
//!
//! WHAT IT WRAPS, AND HOW LITTLE. Every method here is a thin forward to a
//! seam the unit tests already drive — `headless_for_test`, `term_lock` +
//! `Terminal::process`/`cell_frame_into`/`take_damage`, `tick_cursor_fx`,
//! `redraw_compose`, `App::input`, the `WindowState` engine fields — plus
//! read-only probes of state the guards in the bench assert on. It contains NO
//! logic of its own beyond the single-pane LOCK-A extraction shape
//! ([`BenchApp::present_frame`]), which mirrors `redraw_window`'s single-pane
//! path because that path is inline in a function that BAILS headless (the
//! present-target match requires an OS window), so a headless bench must model
//! extract → effects → raster itself. Where the shipping path IS reachable
//! headless (`redraw_compose`, the split/composed present used by the video
//! recorder and driven directly by the split-sparkle unit tests), the bench
//! calls it verbatim.

use std::time::Instant;

use crate::app_render::CursorFxInputs;
use crate::input::{InputEvent, InputOutcome, Source};
use crate::{App, Backend, BackendSlot, WindowId, WindowState, stub_session, term_lock};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

/// What one [`BenchApp::tick_fx`] observed — the externally checkable slice of
/// `CursorFxTick`. The bench's guards read these; the fingerprints and fills
/// are the same "lit vs dark" witnesses the present's early-out key consumes.
pub struct FxFrame {
    /// Aurora fingerprint, rainbow-folded — `0` on an idle/dark frame.
    pub glow_fp: u64,
    /// Comet-trail fingerprint — `0` on an idle/dark frame.
    pub trail_fp: u64,
    /// The resolved aurora config's master gate — the "every cursor effect
    /// off" claim, read from the very config the engines were ticked with.
    pub glow_enabled: bool,
    /// The (ignition heat-blended) comet colour the presented trail cells
    /// would render at. THE CF-6 WITNESS: `ignite` blends this from the
    /// typing-cadence intensity/warmth pair UNCONDITIONALLY — before any
    /// `cfg.enabled` is consulted — so on a frame with every cursor effect
    /// off it still moves with cadence heat. Observable from outside, which
    /// is what lets the bench PROVE the cadence triple ran on the off arm.
    pub trail_color: u32,
    /// Whether ANY block-cursor fill override (forge/rainbow/droplet/beamrod/
    /// comet/phaser/bolt) came back `Some` — all must be `None` on an off arm.
    pub any_fill: bool,
    /// The bolt / twinkle style overrides — dark-arm guards, like the fills.
    pub bolt_cursor: bool,
    /// See `bolt_cursor`.
    pub twinkle_cursor: bool,
}

/// A headless `App` plus the one window every bench fixture drives, wrapped so
/// the bench target can hold it without seeing the crate-private types.
pub struct BenchApp {
    app: App,
    wid: WindowId,
}

impl BenchApp {
    /// The unit suite's headless fixture, focused (the effect engines AND the
    /// pet gate all fold `win_focused`, and a real headless window is
    /// unfocused — the same explicit `ws.focused = true` the split-sparkle
    /// fixture sets). One window, one stub tab (session 0, no PTY), a REAL
    /// CPU render backend (needs a system monospace font), `headless = true`.
    #[must_use]
    pub fn headless() -> Self {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.windows
            .get_mut(&wid)
            .expect("headless fixture window 0")
            .focused = true;
        BenchApp { app, wid }
    }

    // ------------------------------------------------------------- config --

    /// EVERY cursor effect off: master switch off AND the style token "off"
    /// (belt and braces — either alone already resolves `enabled: false`, and
    /// "off" also blanks the style so no style-matched body can tick). This is
    /// the CF-6 target state: `tick_cursor_fx` still runs its whole driver —
    /// including the TypingCadence triple + `ignite` — on such a frame.
    pub fn effects_all_off(&mut self) {
        self.app.config.cursor_trail = Some(false);
        self.app.config.cursor_trail_style = Some("off".into());
        self.refresh_effect_caches();
    }

    /// The lit CONTROL for the off arm: rainbow kitty at full, the default-ish
    /// shipped shape the unit tests use.
    pub fn effects_rainbow_on(&mut self) {
        self.app.config.cursor_trail = Some(true);
        self.app.config.cursor_trail_style = Some("rainbow kitty".into());
        self.refresh_effect_caches();
    }

    /// The PET-03 target state: the pet IS configured (`cursor_trail_style`
    /// names the pet, so `trail_is_kitty_pet()` holds and the brain is fed
    /// every frame) but CANNOT be visible (`cursor_trail = false` resolves
    /// `glow_cfg.enabled = false`, one of `pet_visible`'s AND terms). Sparkle
    /// words stay at their default ON so the word scanner keeps a REAL per-row
    /// ink map — without it `pet_ink`'s row walk would scan an empty map and
    /// the workload would price nothing.
    pub fn pet_configured_glow_off(&mut self) {
        self.app.config.cursor_trail = Some(false);
        self.app.config.cursor_trail_style = Some("rainbow kitty pet".into());
        self.refresh_effect_caches();
    }

    /// The exact cache-invalidation pair the unit tests perform after mutating
    /// effect config in place (`cursor_trail_master_owns_ordinary_kitty…`).
    fn refresh_effect_caches(&mut self) {
        self.app.kitty_cursor_enabled_cache = None;
        self.app.recompute_sparkle();
    }

    /// `trail_is_kitty_pet()` — the "pet configured" half of the PET-03 guard.
    #[must_use]
    pub fn pet_mode(&self) -> bool {
        self.app.trail_is_kitty_pet()
    }

    /// The resolved aurora master gate — the "cannot be visible" half of the
    /// PET-03 guard (`pet_visible` ANDs `glow_cfg.enabled`) and the "off"
    /// proof of the CF-6 arm.
    #[must_use]
    pub fn glow_enabled(&self) -> bool {
        self.app.glow_config().enabled
    }

    // ----------------------------------------------------- windows + tabs --

    /// Split the active tab into the unit suite's 2-pane vertical split with a
    /// fresh stub session; returns the new pane's session id.
    pub fn split_stub(&mut self) -> u64 {
        self.app.split_active_stub_tab(self.wid)
    }

    /// Append `n` stub tabs (each a fresh no-PTY session) to the window — the
    /// many-tabs fixture. Mirrors `open_tab`'s bookkeeping via the test seam.
    pub fn push_stub_tabs(&mut self, n: usize) {
        for _ in 0..n {
            let sid = self.app.next_session_id;
            let session = stub_session(sid);
            self.app.push_stub_tab(self.wid, session);
        }
    }

    /// Tabs resident in the window (layout trees), for the fixture guard.
    #[must_use]
    pub fn tab_count(&self) -> usize {
        self.ws().layouts.len()
    }

    /// Arm the decoration-birth window exactly as the split-sparkle fixture
    /// does (`ws.pending_deco_birth = Some(now)`), so words typed into the
    /// fixture may summon their decorations on the establishing frames.
    pub fn mark_deco_birth(&mut self, now: Instant) {
        self.ws_mut().pending_deco_birth = Some(now);
    }

    // ------------------------------------------------------------ feeding --

    /// PTY-shaped ingest: `Terminal::process` under the session lock — the
    /// byte-identical seam the reader thread drives. This is the flood arm.
    pub fn feed(&mut self, sid: u64, bytes: &[u8]) {
        let term = self
            .app
            .pool
            .get(sid)
            .expect("bench fixture session")
            .term
            .clone();
        term_lock(&term).process(bytes);
    }

    /// One HUMAN keystroke through the real input seam (`App::input`,
    /// `Source::Human`): encode, policy, predictive echo, the typing-cadence
    /// pulse — everything the event loop does before the PTY write. The
    /// fixture's sink fd is `-1`, so the final write itself reports
    /// `WriteFailed`; the bench feeds the echo byte by hand (closing the loop
    /// a real shell would). Returns `false` only on `RangeRejected` — a
    /// broken fixture, which the guards treat as fatal.
    pub fn keystroke(&mut self, c: char) -> bool {
        let out = self.app.input(
            self.wid,
            InputEvent::Key {
                key: Key::Character(c),
                mods: Modifiers::empty(),
                base_layout: None,
                event_type: KeyEventType::Press,
            },
            Source::Human,
        );
        !matches!(out, InputOutcome::RangeRejected)
    }

    // ------------------------------------------------------------ cadence --

    /// Pulse the typing-cadence tracker with an INJECTED clock — the exact
    /// call the human key path makes (`ws.typing_cadence.on_keystroke`), used
    /// by arms that must stay on the bench's injected clock (App::input
    /// samples the wall internally, which would break reproducibility).
    pub fn pulse_typing(&mut self, now: Instant) {
        self.ws_mut().typing_cadence.on_keystroke(now);
    }

    /// The cadence intensity at `now` — the guards' hot/cold proof.
    #[must_use]
    pub fn cadence_intensity(&self, now: Instant) -> f32 {
        self.ws().typing_cadence.intensity(now)
    }

    /// THE PRE-FIX DRIVER SHAPE, verbatim (CF-6): `intensity` (the
    /// `rainbow_energy` read) then `intensity` + `warmth` again (the `ignite`
    /// pair) — three lazy heat decays for one instant. The seams group times
    /// this against [`Self::cadence_sample`] so the shared-sample adoption's
    /// engine-half delta is priced directly.
    #[must_use]
    pub fn cadence_triple(&self, now: Instant) -> (f32, f32, f32) {
        let ws = self.ws();
        (
            ws.typing_cadence.intensity(now),
            ws.typing_cadence.intensity(now),
            ws.typing_cadence.warmth(now),
        )
    }

    /// The PREPARED engine seam (`TypingCadence::sample`, already landed in
    /// aterm-effects): both channels off ONE decay — what the driver adopts.
    #[must_use]
    pub fn cadence_sample(&self, now: Instant) -> (f32, f32) {
        self.ws().typing_cadence.sample(now)
    }

    // -------------------------------------------------------------- frame --

    /// The per-frame cursor-effect pass (`App::tick_cursor_fx`) with the
    /// cursor parked — the CF-6 workload's timed unit. Inputs are the neutral
    /// test frame (the one constructor beside the struct — never a bare
    /// literal) at the fixture's real grid, stamped with the injected `now`.
    pub fn tick_fx(&mut self, now: Instant) -> FxFrame {
        self.tick_fx_at(now, (0, 0))
    }

    /// [`Self::tick_fx`] with the cursor placed — the lit control's script
    /// moves it so the enabled engines actually spawn (a parked cursor never
    /// calls `spawn`, and a control that would have drawn nothing proves
    /// nothing about the off arm's zero).
    pub fn tick_fx_at(&mut self, now: Instant, cur: (u16, u16)) -> FxFrame {
        let mut fx = CursorFxInputs::sample_for_test(now);
        fx.cur = Some(cur);
        let out = self
            .app
            .tick_cursor_fx(self.wid, fx)
            .expect("bench fixture window is alive");
        FxFrame {
            glow_fp: out.glow_fp,
            trail_fp: out.trail_fp,
            glow_enabled: out.glow_cfg.enabled,
            trail_color: out.trail_color,
            any_fill: out.forge_fill.is_some()
                || out.rainbow_fill.is_some()
                || out.droplet_fill.is_some()
                || out.beamrod_fill.is_some()
                || out.comet_fill.is_some()
                || out.phaser_fill.is_some()
                || out.bolt_fill.is_some(),
            bolt_cursor: out.bolt_cursor,
            twinkle_cursor: out.twinkle_cursor,
        }
    }

    /// The SHIPPING composed present (`App::redraw_compose` — the split/video
    /// path, headless-drivable, the one the split-sparkle unit tests call
    /// verbatim): per-pane locks + extraction, decorations, the compose-path
    /// cursor effects (its own cadence `ignite`), the PET feed + brain tick,
    /// the RepaintKey early-out. `true` when the frame presented (repainted),
    /// `false` when the early-out took it — an idle frame's honest cost.
    pub fn compose(&mut self, now: Instant) -> bool {
        self.app
            .redraw_compose(self.wid, 24, 80, false, false, None, 0, now)
            .is_some()
    }

    /// ONE headless single-pane presented frame, modeled exactly as the scout
    /// mapped `redraw_window`'s single-pane path (which bails headless at its
    /// present-target match, so it cannot be called directly):
    ///
    ///   1. LOCK A: one coherent cursor snapshot + `cell_frame_into` +
    ///      `take_damage` under the session lock (the compare-and-consume
    ///      damage contract).
    ///   2. EFFECTS: `tick_cursor_fx` — the extracted-verbatim shared driver.
    ///   3. RASTER: `Renderer::render_input` over the refilled scratch on the
    ///      fixture's REAL CPU backend — actual pixels, headless.
    ///
    /// Returns an FNV fold of the rasterized pixels: the frame-identity
    /// witness the echo guard asserts on (two frames that differ in one glyph
    /// cannot collide by accident at this hash width for guard purposes).
    ///
    /// NOT modeled (needs glass): softbuffer/GPU presentation, pacing,
    /// scale/EDR binding, tab-strip + native chrome. Those are the same
    /// exclusions the crate's own headless capture (`splice_cursor_fx`) lives
    /// with.
    pub fn present_frame(&mut self, now: Instant) -> u64 {
        const ROWS: usize = 24;
        const COLS: usize = 80;
        let term = self
            .app
            .pool
            .get(0)
            .expect("bench fixture session 0")
            .term
            .clone();
        let (cur, cursor_visible, cursor_style);
        {
            // LOCK A: snapshot + extract + consume under ONE lock, so the
            // cursor and the grid are one coherent observation — the same
            // etiquette the shipping path documents.
            let ws = self
                .app
                .windows
                .get_mut(&self.wid)
                .expect("bench fixture window");
            let mut t = term_lock(&term);
            let cpos = t.cursor();
            cursor_visible = t.cursor_visible();
            cursor_style = t.cursor_style();
            cur = cursor_visible.then_some((cpos.row, cpos.col));
            t.cell_frame_into(&mut ws.input_scratch, ROWS, COLS);
            t.take_damage();
        }
        let mut fx = CursorFxInputs::sample_for_test(now);
        fx.rows = ROWS;
        fx.cols = COLS;
        fx.cur = cur;
        fx.cursor_visible = cursor_visible;
        fx.cursor_style = cursor_style;
        let _ = self
            .app
            .tick_cursor_fx(self.wid, fx)
            .expect("bench fixture window is alive");
        let ws = self
            .app
            .windows
            .get(&self.wid)
            .expect("bench fixture window");
        let BackendSlot::Ready(Backend::Cpu(renderer)) = &mut self.app.backend else {
            unreachable!("headless_for_test always builds a ready CPU backend");
        };
        let frame = renderer.render_input(&ws.input_scratch);
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &px in &frame.pixels {
            h = (h ^ u64::from(px)).wrapping_mul(0x100_0000_01b3);
        }
        h
    }

    // -------------------------------------------------------------- probes --

    /// The scanner's per-row pet-ink map, from the SAME `pet_ink()` call the
    /// unconditional per-frame feed makes: `(rows_in_map, inked_rows,
    /// live_edge)`. The PET-03 guard's "the map is real" proof — a workload
    /// whose map were empty would price an O(0) scan and claim it was O(rows).
    #[must_use]
    pub fn pet_ink_probe(&self) -> (usize, usize, Option<u16>) {
        let (spans, live) = self.ws().word_decos.pet_ink();
        let inked = spans.iter().filter(|&&(first, end)| end > first).count();
        (spans.len(), inked, live)
    }

    /// EXACTLY the per-frame pair PET-03 prices, lifted verbatim from the
    /// compose site ("THE INK SEAM … split-path twin"): the producer's
    /// O(rows) live-edge scan + the consumer's O(rows) copy into the brain.
    /// The seams group times this alone, because inside a full compose frame
    /// the pair is nanoseconds under microseconds — a delta the full-frame
    /// number can contextualize but not resolve.
    pub fn pet_ink_feed(&mut self) {
        let ws = self.ws_mut();
        let (spans, live) = ws.word_decos.pet_ink();
        ws.cursor_pet.sense_ink(0, spans, live);
    }

    /// A glyph out of the window's render scratch (the extracted frame), for
    /// the echo guard: the just-typed character must be ON the presented grid.
    #[must_use]
    pub fn scratch_cell(&self, row: usize, col: usize) -> char {
        self.ws().input_scratch.cells[row][col].ch
    }

    /// The live cursor cell (terminal coords), read under the session lock —
    /// the echo guard uses it to find the cell the echoed glyph landed in.
    #[must_use]
    pub fn cursor_pos(&self) -> (u16, u16) {
        let term = self
            .app
            .pool
            .get(0)
            .expect("bench fixture session 0")
            .term
            .clone();
        let t = term_lock(&term);
        let p = t.cursor();
        (p.row, p.col)
    }

    // ------------------------------------------------------------- private --

    fn ws(&self) -> &WindowState {
        self.app
            .windows
            .get(&self.wid)
            .expect("bench fixture window 0 is never closed")
    }

    fn ws_mut(&mut self) -> &mut WindowState {
        self.app
            .windows
            .get_mut(&self.wid)
            .expect("bench fixture window 0 is never closed")
    }
}
