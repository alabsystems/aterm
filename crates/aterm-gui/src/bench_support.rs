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

/// Ring capacity of the tiered engine [`BenchApp::feed_history`] installs.
/// SMALL on purpose: the ring is the bounded tier, and history that never
/// leaves it is history the off-thread hand-off does not have to carry. A
/// production-sized ring (`spawn.rs`'s 10k lines) would hold every line a
/// bench-sized workload feeds, so the detached store would be empty and the
/// workers would finish instantly — the concurrency MPT-1 is about would
/// collapse before a gauge could see it. Mirrors the reflow-worker tests'
/// `term_with_history`.
const HISTORY_RING_LINES: usize = 8;

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
        let mut b = BenchApp { app, wid };
        b.pin_theme_defaults();
        b
    }

    /// Pin every resident session's ENGINE default fg/bg to the render theme —
    /// the pinning the SHIPPING launch does unconditionally and
    /// `App::headless_for_test` omits.
    ///
    /// The product builds its session factory with
    /// `terminal_config: Some(config.applied_terminal_config_for_with_assets(..))`
    /// — "Always Some: pins the engine default fg/bg to the theme so unstyled
    /// cells paint the themed background, not spec-black" — and
    /// `applied_terminal_config_for_with_assets` unconditionally writes
    /// `tc.default_background = rgb(theme.bg)`.
    ///
    /// WITHOUT IT this fixture is a pristine `Terminal::new`: `cell_frame`
    /// takes the "standalone-renderer compatibility" arm and publishes
    /// `default_bg = COLOR_UNSET` while every unstyled cell carries VT-spec
    /// black — so the raster paints a BLACK grid over a THEME-BG clear. That is
    /// the "black-backed text" state the product deliberately does not ship
    /// (two visual judges flagged it; see `applied_terminal_config`), and it
    /// makes the fixture's base-clear/cell-colour relationship the opposite of
    /// every shipping frame's. Pricing a background pass against it prices a
    /// colour arrangement the product never renders.
    pub fn pin_theme_defaults(&mut self) {
        let theme = aterm_render::Theme::default();
        let rgb = |c: u32| {
            aterm_core::terminal::Rgb::new(
                ((c >> 16) & 0xff) as u8,
                ((c >> 8) & 0xff) as u8,
                (c & 0xff) as u8,
            )
        };
        let terms: Vec<_> = self.app.pool.iter().map(|s| s.term.clone()).collect();
        for t in terms {
            let mut guard = term_lock(&t);
            guard.set_default_background(rgb(theme.bg));
            guard.set_default_foreground(rgb(theme.fg));
        }
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
        let sid = self.app.split_active_stub_tab(self.wid);
        self.pin_theme_defaults();
        sid
    }

    /// Append `n` stub tabs (each a fresh no-PTY session) to the window — the
    /// many-tabs fixture. Mirrors `open_tab`'s bookkeeping via the test seam.
    pub fn push_stub_tabs(&mut self, n: usize) {
        for _ in 0..n {
            let sid = self.app.next_session_id;
            let session = stub_session(sid);
            self.app.push_stub_tab(self.wid, session);
        }
        self.pin_theme_defaults();
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

    /// ONE headless single-pane rastered frame, modeled exactly as the scout
    /// mapped `redraw_window`'s single-pane path (which bails headless at its
    /// present-target match, so it cannot be called directly):
    ///
    ///   1. LOCK A: one coherent cursor snapshot + `cell_frame_into` +
    ///      `take_damage` under the session lock (the compare-and-consume
    ///      damage contract).
    ///   2. EFFECTS: `tick_cursor_fx` — the extracted-verbatim shared driver.
    ///   3. RASTER: `Renderer::render_input_cached` over the refilled scratch
    ///      through the window's PERSISTENT `WindowCpu` damage cache on the
    ///      fixture's REAL CPU backend — the SHIPPING windowed CPU present's
    ///      exact raster entry (`present_input_scratch` →
    ///      `render_input_cached(&mut ws.cpu_cache, ..)`) — actual pixels,
    ///      headless, row-scoped exactly like the product.
    ///
    /// The rastered pixels stay RESIDENT in the window's persistent
    /// `cpu_cache`; each public wrapper picks its own witness over them:
    /// [`Self::present_frame`] (the TIMED entry) black-boxes a pointer,
    /// [`Self::present_frame_hashed`] (the guard entry) FNV-folds the frame.
    ///
    /// NOT modeled (needs glass): softbuffer/GPU presentation, pacing,
    /// scale/EDR binding, tab-strip + native chrome. Those are the same
    /// exclusions the crate's own headless capture (`splice_cursor_fx`) lives
    /// with.
    fn raster_frame(&mut self, now: Instant) -> (usize, usize) {
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
        // Disjoint sub-borrows — the `present_input_scratch` idiom
        // (app_render.rs): `backend` and `windows` are separate `App` fields,
        // and within the window `cpu_cache` / `input_scratch` are separate
        // `WindowState` fields, so the renderer, the persistent damage cache,
        // and the input snapshot are borrowed at once with no aliasing.
        let App {
            backend, windows, ..
        } = &mut self.app;
        let ws = windows.get_mut(&self.wid).expect("bench fixture window");
        let BackendSlot::Ready(Backend::Cpu(renderer)) = backend else {
            unreachable!("headless_for_test always builds a ready CPU backend");
        };
        // THE MODEL FIX (RE-1): raster through the SHIPPING per-window damage
        // cache. `render_input` builds a THROWAWAY `WindowCpu` per call, so
        // its cache match is vacuously `None` → FullRepaint: the old model
        // re-rastered all 24x80 cells every keystroke (plus a ~1.5 MB
        // owned-`Frame` clone) while the product's windowed present repaints
        // only the 1-2 dirty rows. Renderer blink/override state is untouched
        // (the bench never toggles it); byte-parity of this entry against the
        // full repaint is asserted by `verify_echo` via
        // [`Self::parity_hashes`] on every verify frame.
        let view = renderer.render_input_cached(&mut ws.cpu_cache, &ws.input_scratch);
        (view.width(), view.height())
    }

    /// The TIMED present seam (RE-2): extract + effects + damage-tracked
    /// raster (RE-1), NO pixel fold. The pre-fix entry FNV-folded EVERY
    /// rastered pixel inside the timed span (a serial dependent chain,
    /// measured ~331 us/frame at 24x80 — ~48% of the pre-fix keystroke_echo
    /// reading) AND rastered through the always-full owned path; neither
    /// cost is paid by any shipping frame. A pointer-read `black_box` keeps
    /// the raster observable so the optimizer can never hollow the span.
    pub fn present_frame(&mut self, now: Instant) -> (usize, usize) {
        let dims = self.raster_frame(now);
        std::hint::black_box(self.ws().cpu_cache.frame_pixels().as_ptr());
        dims
    }

    /// [`Self::raster_frame`] + the frame-identity FNV fold — the UNTIMED
    /// guard entry `verify_echo` asserts on (two frames that differ in one
    /// glyph cannot collide by accident at this hash width for guard
    /// purposes). The timed loop calls [`Self::present_frame`] and never pays
    /// the fold.
    pub fn present_frame_hashed(&mut self, now: Instant) -> u64 {
        let _ = self.raster_frame(now);
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &px in self.ws().cpu_cache.frame_pixels() {
            h = (h ^ u64::from(px)).wrapping_mul(0x100_0000_01b3);
        }
        h
    }

    /// THE TAB-STRIP FRAME (D-2 SPLICE / DMG-1 reach), modelled at exactly the
    /// three seams the strip-lane fix touches and in the shipping order —
    /// `redraw_window`'s resident-scratch reclaim (hoisted ahead of LOCK A), the
    /// damage-scoped re-extract under LOCK B, and `splice_tab_strip_with` — then
    /// the same REAL CPU raster through the window's persistent damage cache that
    /// [`Self::present_frame`] uses.
    ///
    /// This is the one workload in this crate's benches that has a tab strip at
    /// all: every other one runs at `tab_strip_rows == 0`, which the module header
    /// lists among the honest cuts, and which is exactly why they cannot price
    /// this change. Both halves of the fix are inside the timed span — the
    /// extraction arm (`FrameRefill::Scoped` vs `Full`) and the dirty-row diff the
    /// spliced revision lane feeds (inside `render_input_cached`).
    ///
    /// `unsplice: false` is the PRE-FIX reclaim verbatim: salvage the surplus
    /// TAIL and hand the extractor a scratch that is still `strip` rows too tall
    /// with its continuity tokens broken, so every frame falls back to the full
    /// re-extract. The two arms are otherwise byte-identical work in one binary,
    /// which makes the A/B free of any build or binary difference at all.
    ///
    /// Returns whether the extract took the SCOPED arm, so the bench can assert
    /// REACH on both sides instead of trusting that it did.
    pub fn strip_present(&mut self, strip: usize, unsplice: bool) -> bool {
        let term = self
            .app
            .pool
            .get(0)
            .expect("bench fixture session 0")
            .term
            .clone();
        let (rows, cols) = {
            let t = term_lock(&term);
            (usize::from(t.rows()), usize::from(t.cols()))
        };
        let scoped = {
            let ws = self
                .app
                .windows
                .get_mut(&self.wid)
                .expect("bench fixture window");
            let unspliced = unsplice
                && strip > 0
                && ws
                    .input_scratch
                    .undo_host_row_prepend(rows, &mut ws.strip_row_pool, strip);
            if !unspliced && strip > 0 && ws.input_scratch.cells.len() > rows {
                let pool = &mut ws.strip_row_pool;
                for buf in ws.input_scratch.cells.drain(rows..) {
                    if pool.len() >= strip {
                        break;
                    }
                    pool.push(buf);
                }
            }
            let mut t = term_lock(&term);
            let refill = t.cell_frame_damage_scoped_into(&mut ws.input_scratch, rows, cols);
            matches!(refill, aterm_core::render::FrameRefill::Scoped { .. })
        };
        self.app.tab_strip_rows = u16::try_from(strip).unwrap_or(0);
        self.app.splice_tab_strip_with(self.wid, 1);
        // Disjoint sub-borrows — the `present_input_scratch` idiom, exactly as
        // `raster_frame` takes them.
        let App {
            backend, windows, ..
        } = &mut self.app;
        let ws = windows.get_mut(&self.wid).expect("bench fixture window");
        let BackendSlot::Ready(Backend::Cpu(renderer)) = backend else {
            unreachable!("headless_for_test always builds a ready CPU backend");
        };
        let view = renderer.render_input_cached(&mut ws.cpu_cache, &ws.input_scratch);
        std::hint::black_box(view.width());
        scoped
    }

    /// The strip workload's UNTIMED arm: one keystroke echo on the bottom row —
    /// real damage every tick, exactly one damaged row, no wrap or scroll (a wrap
    /// would advance `base_y` and honestly force the full arm on both sides).
    pub fn strip_echo(&mut self, tick: &mut u8) {
        *tick = tick.wrapping_add(1);
        let bytes: &[u8] = if tick.is_multiple_of(2) {
            b"\rx"
        } else {
            b"\ry"
        };
        let term = self
            .app
            .pool
            .get(0)
            .expect("bench fixture session 0")
            .term
            .clone();
        term_lock(&term).process(bytes);
    }

    /// RE-1's PARITY WITNESS (the damage_differential idiom, in-bench): hash
    /// the per-window damage cache's CURRENT pixels (the frame the model just
    /// presented) against a from-scratch FULL repaint of the SAME scratch
    /// through the SAME renderer (`render_input` — the owned entry, a full
    /// repaint by construction; same fonts, theme, blink state). Equal hashes
    /// prove the damage-tracked model byte-identical to the full raster; the
    /// verify pass asserts this every frame, and the timed loop never pays
    /// the witness.
    #[must_use]
    pub fn parity_hashes(&mut self) -> (u64, u64) {
        fn fnv(pixels: &[u32]) -> u64 {
            let mut h = 0xcbf2_9ce4_8422_2325u64;
            for &px in pixels {
                h = (h ^ u64::from(px)).wrapping_mul(0x100_0000_01b3);
            }
            h
        }
        let App {
            backend, windows, ..
        } = &mut self.app;
        let ws = windows.get(&self.wid).expect("bench fixture window");
        let BackendSlot::Ready(Backend::Cpu(renderer)) = backend else {
            unreachable!("headless_for_test always builds a ready CPU backend");
        };
        let full = renderer.render_input(&ws.input_scratch);
        (fnv(ws.cpu_cache.frame_pixels()), fnv(&full.pixels))
    }

    // -------------------------------------------------------------- probes --

    /// `(total, at_base)` background runs the LAST rastered frame resolved —
    /// [`aterm_render::Renderer::last_bg_runs`] lifted to the fixture, so a
    /// bench can prove two-sided that its content reaches the
    /// redundant-background state (and that a control does not).
    #[must_use]
    pub fn bg_run_probe(&self) -> (u32, u32) {
        let BackendSlot::Ready(Backend::Cpu(renderer)) = &self.app.backend else {
            unreachable!("headless_for_test always builds a ready CPU backend");
        };
        renderer.last_bg_runs()
    }

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

    // --------------------------------------------------------- scrollback --
    //
    // THE SCROLLED-BACK FRAME (SCR-1/SCR-2). Everything above drives frames at
    // the LIVE bottom, where a row read is a pointer into the grid ring. The
    // moment `display_offset > 0` the same per-frame extraction resolves every
    // visible row through `Grid::visible_row_view` -> the 3-tier history
    // materializer instead, and it does so with no memo of any kind: a frame
    // that repaints an UNCHANGED scrolled-back viewport re-materializes the
    // whole window from scratch. Nothing in this crate could price that before
    // these four seams — every existing workload sits at offset 0.
    //
    // All four are read-only viewport touches under the session lock, the same
    // etiquette the rest of this module keeps.

    /// Scroll the viewport `delta` lines UP into history (negative scrolls back
    /// down) through `Grid::scroll_display` — the exact call the wheel handler
    /// makes. A pure VIEWPORT move: it marks display-offset damage and leaves
    /// `content_gen` untouched, so the frames that follow it repaint UNCHANGED
    /// content at a new offset (the state the finding is about).
    pub fn scroll_display(&mut self, delta: i32) {
        let term = self
            .app
            .pool
            .get(0)
            .expect("bench fixture session 0")
            .term
            .clone();
        term_lock(&term).grid_mut().scroll_display(delta);
    }

    /// Snap the viewport back to the live bottom (`display_offset == 0`) — the
    /// control arm's state, and the invalidation guard's round trip.
    pub fn scroll_to_bottom(&mut self) {
        let term = self
            .app
            .pool
            .get(0)
            .expect("bench fixture session 0")
            .term
            .clone();
        term_lock(&term).grid_mut().scroll_to_bottom();
    }

    /// The viewport's scrollback depth in lines (`0` = live bottom) — the
    /// "is this workload actually scrolled back" half of the reach guard.
    #[must_use]
    pub fn display_offset(&self) -> usize {
        let term = self
            .app
            .pool
            .get(0)
            .expect("bench fixture session 0")
            .term
            .clone();
        let t = term_lock(&term);
        t.grid().display_offset()
    }

    /// Retained history depth in lines — the "there is real history under this
    /// viewport" half of the reach guard, and the anchor the frame-identity
    /// formula is computed from.
    #[must_use]
    pub fn scrollback_lines(&self) -> usize {
        let term = self
            .app
            .pool
            .get(0)
            .expect("bench fixture session 0")
            .term
            .clone();
        let t = term_lock(&term);
        t.grid().scrollback_lines()
    }

    /// One row of the window's render scratch as text (trailing blanks
    /// trimmed) — the frame the last present actually extracted. The
    /// scrolled-back guards read the fill LINE NUMBER out of this, which is
    /// what makes "the frame shows the right history rows" checkable from
    /// outside: a memo that ever hands back a row it filled under a different
    /// scroll position or history length fails it on the next frame.
    #[must_use]
    pub fn scratch_row_text(&self, row: usize) -> String {
        self.ws().input_scratch.cells[row]
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    // --------------------------------------------- workspace scaling (MPT) --
    //
    // WHY THESE EXIST. `many_tabs_idle/N` prices `App::redraw_compose`, which
    // takes the strip fingerprint as a PARAMETER and sits BELOW
    // `redraw_window`'s wrapper. Four of the workspace-scaling findings live
    // in that wrapper (`resize_panes_scoped`, `present_latency_ns`,
    // `redraw_tab_strip_state`) and two on the `Wake::Output` arm
    // (`observe_session_statuses`, `observe_title_drift`). The seams below
    // reach each of those SHIPPING entry points directly, so
    // `benches/workspace_scaling.rs` prices the passes themselves rather than
    // a model of them.

    /// Stage a WORKSPACE: `tabs` resident tabs in the fixture's window, each
    /// holding `panes` terminal panes (stub sessions, no PTY) — the 30x4 shape
    /// the findings are written against. Tab 0 already exists (the headless
    /// fixture's window), so this splits it and appends `tabs - 1` more,
    /// splitting each as it becomes active (`push_stub_tab` switches to every
    /// tab it appends, so the splits always land on the tab just created).
    ///
    /// Leaves the LAST tab active, matching `f_many_tabs`.
    pub fn stage_workspace(&mut self, tabs: usize, panes: usize) {
        assert!(tabs >= 1, "a workspace has at least one tab");
        assert!(panes >= 1, "a tab has at least one pane");
        for _ in 1..panes {
            self.split_stub();
        }
        for _ in 1..tabs {
            let sid = self.app.next_session_id;
            let session = stub_session(sid);
            self.app.push_stub_tab(self.wid, session);
            for _ in 1..panes {
                self.split_stub();
            }
        }
        self.pin_theme_defaults();
    }

    /// Panes resident across EVERY tab of the window — the count the
    /// whole-workspace passes are linear in, and the fixture guard that the
    /// staging really built the shape it claims.
    #[must_use]
    pub fn pane_count(&self) -> usize {
        self.ws()
            .tab_set
            .tabs()
            .iter()
            .map(|tab| tab.root.len())
            .sum()
    }

    /// Make tab `i` the active one, through the shipping switch
    /// (`App::switch_tab_in`) — the seam the deferred-resize flush and the
    /// strip refresh both hang off.
    pub fn switch_tab(&mut self, i: usize) {
        self.app.switch_tab_in(self.wid, i);
    }

    /// Set the window's CELL grid — the rectangle `resize_panes_scoped` lays
    /// every tab's panes out against. Changing `cols` is what makes a resize a
    /// WIDTH change, and only a width change detaches a pane's off-screen
    /// history for rewrap; `rows` alone is a bounded, offload-free resize.
    pub fn set_grid(&mut self, rows: u16, cols: u16) {
        let ws = self.ws_mut();
        ws.rows = rows;
        ws.cols = cols;
    }

    /// The window's current cell grid, `(rows, cols)`.
    #[must_use]
    pub fn grid(&self) -> (u16, u16) {
        let ws = self.ws();
        (ws.rows, ws.cols)
    }

    /// Push `lines` lines of scrolled-off history into session `sid` through
    /// the real ingest seam (`Terminal::process` under the session lock, the
    /// reader thread's own path), in reader-shaped chunks.
    ///
    /// THE REACH WITNESS for the resize workloads: a pane whose grid owns no
    /// off-screen history hands off a job with nothing in it, and a settle
    /// measured on such a workspace would price 120 thread creations with no
    /// rewrap behind any of them — a spawn storm is still a spawn storm, but
    /// the CONCURRENCY it creates would collapse instantly and the peak would
    /// read as noise. [`Self::history_lines`] proves the history landed.
    ///
    /// A TIERED STORE IS INSTALLED FIRST, and that is the load-bearing half.
    /// `Grid::resize_offloading_scrollback` — the hand-off site MPT-1 is about
    /// — returns `None` unless a tiered store is ATTACHED (without one the
    /// resize is already O(viewport) and there is nothing to move off-thread),
    /// and `stub_session` builds a bare `Terminal::new`, which has no store at
    /// all. A workload fed history into a store-less stub would hand off ZERO
    /// jobs and price an early return; the bench's `handed_off == panes` guard
    /// caught exactly that. The shape mirrors a real session
    /// (`spawn.rs`: `Terminal::with_scrollback`) and, deliberately, the
    /// small-ring `term_with_history` fixture the reflow-worker tests use, so
    /// the bulk of the fed history lands in the TIERED tier — the unbounded
    /// part the hand-off exists to carry — rather than in the ring.
    ///
    /// Replacing the engine (rather than mutating it) is safe here precisely
    /// because these are stubs: `stub_session` attaches no callbacks, no PTY
    /// and no reader, and the session's `Arc<Mutex<Terminal>>` identity — the
    /// thing panes, the pool and any in-flight reflow job hold — is preserved.
    /// It is built at the pane's CURRENT derived size so the staging's own
    /// resizes stay settled and the next grid flip is a real width change.
    pub fn feed_history(&mut self, sid: u64, lines: usize) {
        let term = self
            .app
            .pool
            .get(sid)
            .expect("bench fixture session")
            .term
            .clone();
        let mut t = term_lock(&term);
        let (rows, cols) = (t.rows(), t.cols());
        *t = aterm_core::terminal::Terminal::with_scrollback(
            rows,
            cols,
            HISTORY_RING_LINES,
            aterm_core::scrollback::Scrollback::new(64, 512, 8_000_000),
        );
        let mut chunk = String::with_capacity(8 * 1024);
        for i in 0..lines {
            chunk.push_str(&format!(
                "hist {i:07} 0123456789 abcdef 0123456789 abcdef\r\n"
            ));
            if chunk.len() >= 8 * 1024 {
                t.process(chunk.as_bytes());
                chunk.clear();
            }
        }
        if !chunk.is_empty() {
            t.process(chunk.as_bytes());
        }
    }

    /// Off-screen history lines session `sid` currently owns (ring + tiered) —
    /// the reach witness described on [`Self::feed_history`].
    #[must_use]
    pub fn history_lines(&self, sid: u64) -> usize {
        let term = self
            .app
            .pool
            .get(sid)
            .expect("bench fixture session")
            .term
            .clone();
        let t = term_lock(&term);
        t.grid().scrollback_lines()
    }

    /// Every live session id, ascending — the workloads iterate it to feed
    /// history into, and arm output stamps on, every pane of the workspace.
    #[must_use]
    pub fn session_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.app.pool.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        ids
    }

    /// THE SETTLE: `App::resize_panes_scoped(wid, false)` — the eager AllTabs
    /// pass every non-drag caller runs (`flush_pending_resize`, split, tab
    /// activate, the control `resize` verb, a scale/font re-grid). MPT-1 and
    /// MPT-2 are both priced on this call: its MAIN-THREAD span is where the
    /// per-pane `Builder::spawn` storm and the discarded background-tab layout
    /// plans both land.
    pub fn resize_settle(&mut self) {
        self.app.resize_panes_scoped(self.wid, false);
    }

    /// THE LIVE-DRAG TICK: `App::resize_panes_scoped(wid, true)` — the exact
    /// call `redraw_window` makes on EVERY presented frame while the window's
    /// `panes_stale` flag stands (i.e. the whole drag and its trailing
    /// settle). MPT-2's number.
    pub fn resize_scoped_active(&mut self) {
        self.app.resize_panes_scoped(self.wid, true);
    }

    /// Whether the window still owes a deferred background-tab resize — the
    /// flag whose standing is what makes [`Self::resize_scoped_active`] a
    /// per-frame cost.
    #[must_use]
    pub fn panes_stale(&self) -> bool {
        self.ws().panes_stale
    }

    /// The `(rows, cols)` session `sid`'s ENGINE is currently sized to — the
    /// differential witness that a scoped pass resized exactly the panes it
    /// claims to and left the deferred ones alone.
    #[must_use]
    pub fn engine_size(&self, sid: u64) -> (u16, u16) {
        let term = self
            .app
            .pool
            .get(sid)
            .expect("bench fixture session")
            .term
            .clone();
        let t = term_lock(&term);
        (t.rows(), t.cols())
    }

    // ------------------------------------------------------ reflow workers --

    /// One read of the process-global `aterm-reflow` gauge:
    /// `(running, peak, submitted, finished, threads)`. See
    /// `app_render::reflow_gauge` for what each means; `submitted == finished`
    /// is the exact quiescence witness.
    #[must_use]
    pub fn reflow_gauge(&self) -> (u64, u64, u64, u64, u64) {
        let s = crate::app_render::reflow_gauge::snapshot();
        (s.running, s.peak, s.submitted, s.finished, s.threads)
    }

    /// Re-baseline the gauge's high-water mark to the live count, so a sweep
    /// reads the peak of its OWN arm.
    pub fn reflow_gauge_reset_peak(&self) {
        crate::app_render::reflow_gauge::reset_peak();
    }

    /// Block until every handed-off reflow job has re-attached or aborted, or
    /// `timeout` elapses. Returns whether it converged.
    ///
    /// This is a REQUIRED arm between two settles, not politeness: while a
    /// pane's history is detached its grid has no tiered store, so
    /// `Terminal::resize_offloading_scrollback` returns `None` and the next
    /// settle hands off NOTHING for that pane (the documented self-throttle).
    /// A workload that did not quiesce between iterations would measure a
    /// settle with an empty hand-off set and report it as fast.
    pub fn reflow_quiesce(&self, timeout: std::time::Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let s = crate::app_render::reflow_gauge::snapshot();
            if s.submitted == s.finished && s.running == 0 {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    }

    /// The SHIPPING claim about reflow concurrency: the most OS
    /// `aterm-reflow` threads one settle may create for `jobs` hand-offs. Read
    /// from the shipping code (`app_render::reflow_thread_ceiling`) so the
    /// bench's two-sided guard tightens with a fix instead of being re-tuned
    /// by hand.
    #[must_use]
    pub fn reflow_thread_ceiling(jobs: usize) -> usize {
        crate::app_render::reflow_thread_ceiling(jobs)
    }

    // ------------------------------------------------------ present latency --

    /// `App::present_latency_ns(wid)` — the latency self-introspection walk
    /// every SUCCESSFUL present runs (`finalize_successful_present`). MPT-3's
    /// number.
    pub fn present_latency(&mut self) -> u64 {
        self.app.present_latency_ns(self.wid)
    }

    /// Arm EVERY live session's output stamp at `ns` on the app's latency
    /// epoch — what each session's PTY reader does on the leading edge of an
    /// output burst. The walk consumes stamps (`swap(0)`), so an unarmed
    /// workspace would price the walk over an all-zero pool.
    pub fn arm_output_stamps(&self, ns: u64) {
        for s in self.app.pool.iter() {
            s.last_output_ns
                .store(ns, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The raw output-stamp cell of session `sid` — the very
    /// `Arc<AtomicU64>` its PTY reader CASes. Handed out so a workload can put
    /// a REAL contending writer on the other end of the line the present walk
    /// does its read-modify-write on; without that, the walk's atomics are L1
    /// hits and the microbenchmark cannot see the coherence traffic the
    /// product pays.
    #[must_use]
    pub fn output_stamp_cell(&self, sid: u64) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        self.app
            .pool
            .get(sid)
            .expect("bench fixture session")
            .last_output_ns
            .clone()
    }

    /// "Now" on the app's latency epoch, in ns — the clock
    /// `present_latency_ns` subtracts armed stamps from.
    #[must_use]
    pub fn lat_now_ns(&self) -> u64 {
        u64::try_from(self.app.lat_epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    // ----------------------------------------------------------- tab strip --

    /// Turn the in-grid tab strip ON (`tab_strip_rows = 1`). The whole strip
    /// read path is gated on it, so a strip workload measured with it off
    /// would price the `else { 0 }` arm.
    pub fn enable_tab_strip(&mut self) {
        self.app.tab_strip_rows = 1;
    }

    /// `App::redraw_tab_strip_state(wid)` — the per-tab title read + metadata
    /// refill + SipHash that runs on EVERY redraw, BEFORE the RepaintKey
    /// early-out. MPT-6's read side.
    pub fn strip_state(&mut self) -> u64 {
        self.app.redraw_tab_strip_state(self.wid)
    }

    /// `App::refresh_window_tabs(wid)` — the whole-strip rebuild a SINGLE
    /// tab's label or status change funnels into. MPT-6's write side.
    pub fn refresh_tabs(&mut self) {
        let _ = self.app.refresh_window_tabs(self.wid);
    }

    // ---------------------------------------------- the Wake::Output gates --

    /// `App::observe_session_statuses(now)` — the FIRST thing the
    /// `Wake::Output` arm does, at the PTY reader's batch rate (thousands/sec
    /// under a flood). Returns how many sessions' published status moved.
    /// MPT-4's number.
    pub fn observe_statuses(&mut self, now: Instant) -> usize {
        self.app.observe_session_statuses(now).len()
    }

    /// Whether the classifier considers session `sid` DUE right now — the
    /// predicate the whole-pool scan evaluates. The reach guard asserts this
    /// is `false` for every session on the sampled turns, which is the
    /// steady state the finding describes ("O(sessions) probes per burst to
    /// usually find zero").
    #[must_use]
    pub fn status_due(&self, sid: u64, now: Instant) -> bool {
        self.app.session_status.due(sid, now)
    }

    /// Whether the tab-status subsystem is live — `false` makes
    /// `observe_session_statuses` a single early return, so a workload
    /// measured with it off prices nothing.
    #[must_use]
    pub fn tab_status_on(&self) -> bool {
        self.app.config.tab_status_or_default()
    }

    /// `App::observe_title_drift(sid, now)` — the per-wake title/cwd drift
    /// gate, whose documented cost contract is "one try_lock + one u64 epoch
    /// load + id compares". MPT-5's number.
    pub fn title_gate(&mut self, sid: u64, now: Instant) {
        self.app.observe_title_drift(sid, now);
    }

    /// The last `title_epoch` this app CONSUMED for `sid` — the drift gate's
    /// steady-state witness (equal to the live epoch ⇒ the cheap first
    /// disjunct is false and the whole-workspace scan is what runs).
    #[must_use]
    pub fn title_seen_epoch(&self, sid: u64) -> Option<u64> {
        self.app.title_drift.seen.get(&sid).copied()
    }

    /// Session `sid`'s LIVE `title_epoch`.
    #[must_use]
    pub fn title_epoch(&self, sid: u64) -> u64 {
        let term = self
            .app
            .pool
            .get(sid)
            .expect("bench fixture session")
            .term
            .clone();
        let t = term_lock(&term);
        t.title_epoch()
    }

    /// Set session `sid`'s terminal title through the engine — the DarkUnless
    /// control for the drift gate: it must move `title_epoch`, and the gate
    /// must then flush.
    pub fn set_title(&mut self, sid: u64, title: &str) {
        let term = self
            .app
            .pool
            .get(sid)
            .expect("bench fixture session")
            .term
            .clone();
        term_lock(&term).set_title(title);
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

// ------------------------------------------------------- subscribe digest --

/// The public face of the `events`-digest driver for
/// `benches/subscribe_digest.rs`.
///
/// Same shape, and same law, as [`BenchApp`]: a thin forward to a crate-private
/// seam ([`crate::subscribe::bench_seam`]) with no logic of its own. It exists
/// because the thing being timed — the push loop's per-target per-wake body —
/// is module-private and must STAY module-private; a bench is an external
/// target and would otherwise have to be given a copy of it, which is how a
/// bench comes to measure something the product does not run.
pub struct DigestBench {
    inner: crate::subscribe::bench_seam::DigestFixture,
}

impl DigestBench {
    /// `targets` watched sessions, each with `blocks` shell blocks, `turns` turn
    /// records and `timeline` timeline events retained. Depths past the shipping
    /// ring caps saturate; [`Self::retained`] reports what was really reached.
    /// NOT named `new`, for the same reason its inner `DigestFixture::build` is
    /// not: the lock-order census resolves a held one-hop call by callee NAME, so
    /// a `fn new` here would capture every `Vec::new()` made under a held `term`
    /// guard and report an OB-7 re-entrancy suspect against a bench fixture's own
    /// private engine.
    #[must_use]
    pub fn build(targets: usize, blocks: usize, turns: usize, timeline: usize) -> Self {
        DigestBench {
            inner: crate::subscribe::bench_seam::DigestFixture::build(
                targets, blocks, turns, timeline,
            ),
        }
    }

    /// One wake of the shipping digest across every target. Returns the frame
    /// bytes produced: `0` on a coalesced idle wake.
    pub fn wake(&mut self, woke: bool) -> usize {
        self.inner.wake(woke)
    }

    /// `(blocks, turns, timeline events)` actually retained on target 0.
    #[must_use]
    pub fn retained(&self) -> (usize, usize, usize) {
        self.inner.retained()
    }

    /// Append one real turn record to every target's ledger.
    pub fn land_turn(&mut self) {
        self.inner.land_turn();
    }
}

/// The public face of the INSTANCE ROSTER tick (see
/// [`crate::subscribe::bench_seam::RosterRebuild`] for exactly what is real and
/// what is modelled).
pub struct RosterBench {
    inner: crate::subscribe::bench_seam::RosterRebuild,
}

impl RosterBench {
    /// An instance with `sessions` live sessions and a caught-up subscriber.
    #[must_use]
    pub fn new(sessions: usize) -> Self {
        RosterBench {
            inner: crate::subscribe::bench_seam::RosterRebuild::new(sessions),
        }
    }

    /// One roster wake: rebuild, diff, adopt. Returns emitted bytes.
    pub fn tick(&mut self) -> usize {
        self.inner.tick()
    }

    /// Retire one session and open another, so the next tick has real work.
    pub fn churn(&mut self) {
        self.inner.churn();
    }

    /// Live session count.
    #[must_use]
    pub fn sessions(&self) -> usize {
        self.inner.sessions()
    }
}
