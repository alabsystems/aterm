// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The host-facing effects API on [`AtermTerminal`]: cursor aurora / comet
//! trail + sparkle words, driven by the shared [`aterm_effects`] pipeline (the
//! SAME state machines the native app runs — no forked art).
//!
//! ## Animation-drive contract
//!
//! The engine is clockless; the host owns time:
//!
//! ```js
//! term.set_sparkle_words_enabled(true);        // or set_cursor_glow(...)
//! let last = performance.now();
//! function frame(t) {
//!   term.advance_effects(t - last); last = t;
//!   term.render(); blit(term);
//!   const deadline = term.effects_next_deadline_ms();
//!   if (deadline !== undefined) setTimeout(frame, deadline); // rain engine tick
//!   else if (term.is_effects_active()) requestAnimationFrame(frame);
//! }
//! requestAnimationFrame(frame);
//! ```
//!
//! Every effect self-terminates to a stable fingerprint (steady ink gradient /
//! settled cat / nova ember), so `is_effects_active()` returns `false` and the
//! host drops to 0% idle. Defaults are all OFF: without any `set_*` call the
//! render output stays byte-identical to the pre-effects binding.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

use crate::AtermTerminal;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermTerminal {
    /// Advance the effects clock by `dt_ms` (the host's rAF delta). The
    /// engines never read a wall clock: same PTY bytes + same `dt` stream ⇒
    /// identical frames. Negative/NaN deltas are ignored.
    // NOTE (WF-1 frame gate): `advance_effects` deliberately does NOT bump the
    // host-visual generation. Hosts pump it once per rAF tick, so bumping here
    // would reopen the gate every frame and delete the optimization entirely.
    // Soundness without it: advancing the clock can only change pixels when
    // some effect is LIVE, and a live effect is exactly what `is_active()` (a
    // gate term) reports. The one case `is_active()` cannot see — an effect
    // that has been configured/ignited but not yet seeded, because seeding
    // happens inside `apply` — is covered by the config/ignition mutators
    // below, each of which bumps.
    pub fn advance_effects(&mut self, dt_ms: f64) {
        self.effects.advance(dt_ms);
    }

    /// `true` while any effect is animating. Consult
    /// [`Self::effects_next_deadline_ms`] first: rain is active at 12/30 Hz and
    /// must not drive a 60/120 Hz display-rAF loop.
    pub fn is_effects_active(&self) -> bool {
        self.effects.is_active()
    }

    /// Milliseconds until the next rain engine tick, or `undefined` when
    /// active frame-rate motion needs rAF (and when every effect is idle).
    pub fn effects_next_deadline_ms(&self) -> Option<f64> {
        self.effects.next_deadline_ms()
    }

    /// Focus gate for the idle one-shots (`§5.6`): an unfocused pane fires no
    /// blink events (and freezes their fingerprints). Pass the pane focus.
    pub fn set_effects_focused(&mut self, focused: bool) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_focused(focused);
    }

    /// Tri-state pane visibility for bounded rain draining:
    /// `focused|visible_unfocused|hidden`.
    pub fn set_effects_visibility(&mut self, state: &str) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_effects_visibility(state);
    }

    /// Register one keystroke for the cursor-comet ignition: sustained fast
    /// calls heat the typing cadence so the next `render` ignites the trail,
    /// sparse/slow calls keep it gentle. The cadence reads the effects clock,
    /// so the host must `advance_effects` between keystrokes for it to reflect
    /// real time. Call this from the SAME JS keydown handler that feeds
    /// `encode_key`; without it the comet stays dormant on web hosts. It also
    /// freezes literal-rain sampling while a draft is unsent; on submit call
    /// `note_matrix_rain_signal(10, 4)` after this method.
    pub fn note_keystroke(&mut self) {
        // WF-1 frame gate: cadence heat can ignite the comet on the NEXT
        // render even though `is_active()` may still read false at gate time
        // (ignition happens inside `apply`). One render per keystroke is
        // semantically right — the echo damages the grid in the same beat.
        self.note_host_visual_change();
        self.effects.note_keystroke();
    }

    /// [`Self::note_keystroke`] upgraded with the typed CHARACTER: the exact
    /// input-time content witness the movement-admission gate requires before
    /// glow/trail styles may follow the cursor (movement without one is
    /// program output and stays dark — the native app's provenance law). Call
    /// from the SAME JS handler that dispatched this key's bytes to the
    /// transport — after dispatch, BEFORE the echo is fed to [`Self::process`]
    /// — and only between renders (the witness is checked against the last
    /// rendered frame and fails closed on any staleness, so a mistimed call
    /// costs cosmetics, never correctness). Returns whether movement
    /// provenance armed; on decline the text-blind keystroke semantics
    /// (cadence heat, rain freeze, candidate cancellation) still apply.
    pub fn note_typed_char(&mut self, ch: char) -> bool {
        // WF-1 frame gate: same rule as `note_keystroke` — the arm can change
        // what the NEXT render admits while `is_active()` still reads false.
        self.note_host_visual_change();
        self.effects
            .note_committed_char(&mut self.term, &self.frame_scratch, ch)
    }

    /// Configure the LUMEN cursor aurora (additive light in the cursor's
    /// wake). Mirrors the native knobs + clamps: `style` ∈
    /// `lumen|phaser|rainbow kitty|sparkle|fire|laser|beam|water|comet` (unknown →
    /// lumen; `rainbow` = the rainbow kitty banded ribbon);
    /// `color`/`accent` omitted derive from the theme cursor (accent = color
    /// brightened 1.5×) exactly like the native app; `duration_ms` clamps
    /// 30..=2000, `length` (cells) 1..=512, `intensity` 0..=1 (0 = off),
    /// `radius` (bloom crown, cells) 0..=2, `ring` = landing-ring ping.
    #[allow(clippy::too_many_arguments)]
    pub fn set_cursor_glow(
        &mut self,
        enabled: bool,
        style: &str,
        color: Option<u32>,
        accent: Option<u32>,
        duration_ms: u32,
        length: u32,
        intensity: f32,
        radius: f32,
        ring: bool,
    ) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_cursor_glow(
            enabled,
            style,
            color,
            accent,
            u64::from(duration_ms),
            length,
            intensity,
            radius,
            ring,
            self.theme_cursor,
        );
    }

    /// Configure the legacy opaque comet trail (the native `cursor_trail_style
    /// = "comet"` look). `color` omitted = the theme cursor; `duration_ms`
    /// clamps 30..=2000, `length` 1..=512. Exactly one of trail/glow is on in
    /// the native app (chosen by style); the embedder decides here.
    pub fn set_cursor_trail(
        &mut self,
        enabled: bool,
        duration_ms: u32,
        length: u32,
        color: Option<u32>,
    ) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_cursor_trail(
            enabled,
            u64::from(duration_ms),
            length,
            color,
            self.theme_cursor,
        );
    }

    /// Enable PHOSPHOR matrix rain. With output material opted in, the shared
    /// pipeline samples supported literal codepoints outside the current
    /// cursor/composer protection band and emits only into empty default-bg cells.
    pub fn set_matrix_rain_enabled(&mut self, on: bool) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_matrix_rain_enabled(on);
    }

    /// Whether PHOSPHOR matrix rain is enabled.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn matrix_rain_enabled(&self) -> bool {
        self.effects.matrix_rain_enabled()
    }

    /// Configure PHOSPHOR using the native bounds. `hue` is
    /// `matrix|theme|custom`; `hue_color` is used only for `custom`.
    /// `output_material` opts into supported literal screen codepoints; hosts
    /// that cannot protect their current composer can leave it false.
    #[allow(clippy::too_many_arguments)]
    pub fn set_matrix_rain(
        &mut self,
        fps: u32,
        density: u32,
        speed: u32,
        trail: u32,
        alpha: Option<u32>,
        head_alpha: Option<u32>,
        hue: &str,
        hue_color: Option<u32>,
        mutation_ms: u32,
        idle_secs: u32,
        suppress_in_alt_screen: bool,
        turn_wave: bool,
        bell_alert: bool,
        output_material: bool,
        seed: u64,
    ) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_matrix_rain(
            fps,
            density,
            speed,
            trail,
            alpha,
            head_alpha,
            hue,
            hue_color,
            mutation_ms,
            idle_secs,
            suppress_in_alt_screen,
            turn_wave,
            bell_alert,
            output_material,
            seed,
            self.theme_bg,
            self.theme_fg,
        );
    }

    /// Accessibility motion gate for PHOSPHOR — an ALIAS of
    /// [`Self::set_reduced_motion`] (one host fact, every effect): it pins
    /// the pet and statics sparkle words too.
    pub fn set_matrix_rain_reduced_motion(&mut self, on: bool) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_matrix_rain_reduced_motion(on);
    }

    /// Feed a terminal visual bell into PHOSPHOR's bounded alert tint.
    pub fn note_matrix_rain_bell(&mut self) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.note_bell();
    }

    /// Feed wheel/PgUp activity from an alternate-screen TUI so rain pauses
    /// while the user reads its transcript.
    pub fn note_matrix_rain_alt_scroll(&mut self) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.note_matrix_rain_alt_scroll();
    }

    /// Payload-free observable-work pulse. Codes are `0 assistant, 1 inspect,
    /// 2 modify, 3 execute, 4 network, 5 branch, 6 waiting, 7 success,
    /// 8 failure, 9 interrupted, 10 turn-start`; weight clamps to `1..=8`.
    /// Turn-start also releases the unsent-composer material gate.
    pub fn note_matrix_rain_signal(&mut self, code: u32, weight: u32) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.note_matrix_rain_signal(code, weight);
    }

    /// Configure PRISM WAKE — the output streak. Program OUTPUT (not typing) is
    /// answered with a short spectrum comet on the row that just took ink,
    /// metered so a burst gets one comet and a flood degrades to a single
    /// constant-cost ribbon. `intensity` 0..=1 (0 is a RESET, not a dim),
    /// `tail` (cells) 4..=14, `max_streaks` 1..=4, `idle_secs` 2..=120, `seed`
    /// fixes the per-comet genome so a host that pins it replays identical
    /// frames. Every band is re-clamped inside the engine: the ceiling is
    /// STRUCTURAL, so no argument an embedder can reach makes this loud, fast,
    /// or flashy.
    ///
    /// Constructed OFF, like every effect on this binding — until this runs
    /// with `enabled = true` the pipeline holds no streak state at all and the
    /// render output stays byte-identical to a build without the effect.
    /// Toggling back off DROPS the engine while keeping the knobs, so
    /// re-enabling restores this same configuration (the rain off/on posture).
    ///
    /// `sound` is carried for contract parity with the native host and is
    /// VISUALS-ONLY here: the web has no audio host, so the shared pipeline
    /// drops the episode cue at the point it is produced rather than queueing
    /// it forever against a listener that will never exist. Passing `true`
    /// records the intent and changes nothing you can hear.
    #[allow(clippy::too_many_arguments)]
    pub fn set_output_streak(
        &mut self,
        enabled: bool,
        intensity: f32,
        tail: u32,
        max_streaks: u32,
        idle_secs: u32,
        sound: bool,
        seed: u64,
    ) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_output_streak(
            enabled,
            intensity,
            tail,
            max_streaks,
            idle_secs,
            sound,
            seed,
        );
    }

    /// Fold the resolved theme into PRISM WAKE. `dark_theme` is the one fact
    /// the colour triple cannot supply on its own: it picks the POLARITY —
    /// additive spectrum light on a dark ground, a tinted shadow-shimmer on a
    /// light one, because additive light cannot darken pale paper and would
    /// simply not exist there. The bg/fg/cursor colours are read from THIS
    /// binding's own [`Self::set_theme`] state rather than re-passed by the
    /// host, exactly as [`Self::set_matrix_rain`] sources its ramp, so the
    /// streak can never disagree with the grid about what the theme is.
    ///
    /// Call it per frame. The fold deliberately does NOT reset the engine, so
    /// an OSC 11/12 recolour re-tints the comets already in flight instead of
    /// killing them; a host that calls this once at boot and later runs
    /// `set_theme` keeps flying the old palette.
    pub fn set_output_streak_theme(&mut self, dark_theme: bool) {
        // NO WF-1 bump, and this is the one CONFIG mutator here that must not
        // have one — the documented contract is a PER-FRAME call, so a bump
        // would reopen the settled-frame gate every single frame and delete the
        // optimization outright (the exact reason `advance_effects` abstains).
        // Soundness is the same argument: re-tinting can only change pixels
        // while comets are resident, and a resident comet is precisely what
        // `is_active()` — a gate term — already reports. With the streak
        // settled there is nothing on the glass to re-tint, and the next
        // spawn reads this config at `apply` time.
        self.effects.set_output_streak_theme(
            dark_theme,
            self.theme_bg,
            self.theme_fg,
            self.theme_cursor,
        );
    }

    /// Accessibility motion gate for PRISM WAKE. On, amplitude is pinned to
    /// zero, which the engine treats as a full RESET rather than a dim: no
    /// quads, no cue, fingerprint 0 — so `is_effects_active` settles and the
    /// host's rAF loop parks instead of spinning on an invisible effect.
    /// Turning it back OFF restores FULL amplitude, so a host running a custom
    /// `intensity` must re-apply [`Self::set_output_streak`] afterwards.
    pub fn set_output_streak_reduced_motion(&mut self, on: bool) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_output_streak_reduced_motion(on);
    }

    /// Stamp one keystroke against PRISM WAKE's ECHO DISCOUNT: output arriving
    /// within the discount window of a stamp is the user's own echo coming back
    /// and mints NOTHING. Deliberately not folded into [`Self::note_keystroke`]
    /// — the streak keeps its own stamp — so call BOTH from the same JS keydown
    /// handler. On web this is effectively required rather than a refinement:
    /// the embedder pipeline reports `input_hot = false`, so the stamp is the
    /// only echo defence a browser host has, and a binding that skips it will
    /// answer the user's own typing with comets.
    pub fn note_output_streak_keystroke(&mut self) {
        // WF-1 frame gate: this stamp SUPPRESSES rather than ignites, but the
        // gate cannot tell the two apart — the licence is judged inside `apply`
        // against damage a gated frame never delivers, so a swallowed stamp
        // would be re-judged as fresh output on the next real frame. One render
        // per keystroke is right regardless: the echo damages the grid in the
        // same beat (the `note_keystroke` rule).
        self.note_host_visual_change();
        self.effects.note_output_streak_keystroke();
    }

    /// MASTER sparkle-words switch (native `[sparkle_words] enabled` +
    /// `toggle_sparkle_words` panic-off). Enabling compiles the multilingual
    /// lexicon once and starts scanning the visible grid; disabling drops all
    /// occurrence state and restores byte-identical output next render.
    /// Defaults (until other setters run) mirror the native launch config:
    /// all four families on (profanity nova / feline cat / orca splash /
    /// emphasis ink), animated ink on.
    pub fn set_sparkle_words_enabled(&mut self, on: bool) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_sparkle_enabled(on);
    }

    /// Whether the sparkle-words master is currently on.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn sparkle_words_enabled(&self) -> bool {
        self.effects.sparkle_enabled()
    }

    /// Per-class gates (native `[sparkle_words.<class>] enabled`): profanity
    /// (supernova/sparkle), feline (peeking cat/paw), orca (water splash),
    /// emphasis (ink-only; effective only while ink is enabled).
    pub fn set_sparkle_classes(
        &mut self,
        profanity: bool,
        feline: bool,
        orca: bool,
        emphasis: bool,
    ) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects
            .set_sparkle_classes(profanity, feline, orca, emphasis);
    }

    /// Animated-ink knobs (native `[sparkle_words.ink]`): the glyph-ink
    /// gradient + specular sweep on matched words. `strength` clamps 0..=1;
    /// `sweep_ms` clamps 350..=6000 (floor 600 while `loop_` — the WCAG flash
    /// margin, structural); `loop_` re-sweeps while the word stays visible.
    pub fn set_sparkle_ink(&mut self, enabled: bool, strength: f32, sweep_ms: u32, loop_: bool) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects
            .set_sparkle_ink(enabled, strength, sweep_ms, loop_);
    }

    /// Force the static, non-animating path (no twinkle/jitter/sweep; novas
    /// collapse to a static glint) — the accessibility `reduced_motion`
    /// override. The engine's flash-limiter floors apply regardless. An
    /// ALIAS of [`Self::set_reduced_motion`] (one host fact, every effect):
    /// it pins the pet and freezes PHOSPHOR too.
    pub fn set_sparkle_reduced_motion(&mut self, on: bool) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_sparkle_reduced_motion(on);
    }

    /// Alt-screen suppression (native `[sparkle_words] suppress_in_alt_screen`,
    /// default off): when on, full-screen apps render undecorated — the v1
    /// launch behavior. Off, the alternate screen sparkles like the main one.
    pub fn set_sparkle_alt_screen_suppression(&mut self, on: bool) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_sparkle_alt_screen_suppression(on);
    }

    /// Feline knobs (native `[sparkle_words.feline]`): `style = "cat"` emits
    /// the authored cat; legacy `"paw"` is ink-only and emits no paw graphic.
    /// `magic` enables rare Fortune/Nebula cats;
    /// `allow_bare_cat` decorates the literal 3-letter `cat`; and
    /// `cjk_single_char` matches a lone cat ideograph (high-FP).
    pub fn set_sparkle_feline(
        &mut self,
        style: &str,
        magic: bool,
        allow_bare_cat: bool,
        cjk_single_char: bool,
    ) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects
            .set_sparkle_feline(style, magic, allow_bare_cat, cjk_single_char);
    }

    /// Profanity knobs (native `[sparkle_words.profanity]`): `style` =
    /// "rainbow" (the v3 animated rainbow ink, the default) | "nova" (the v2
    /// classic nova) | "sparkle" (the exact v1 twinkle). Clamps are the
    /// native flash-safety floors and are not bypassable: `density` 1..=12
    /// sparks, `anim_ms` 350..=10000, `jitter` 0..=6 px, `intensity` 0..=1.
    /// `magic` = rare Quasar/Singularity novas. `supernova_chance` (0..=100,
    /// 0 disables) = the FUCK SUPER NOVA escalation chance under
    /// `style = "rainbow"`. The window-wide ignition limiter (≤2 ignitions
    /// per rolling second) is always on.
    #[allow(clippy::too_many_arguments)]
    pub fn set_sparkle_profanity(
        &mut self,
        style: &str,
        density: u32,
        anim_ms: u32,
        jitter: i8,
        intensity: f32,
        magic: bool,
        supernova_chance: u32,
    ) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_sparkle_profanity(
            style,
            density,
            anim_ms,
            jitter,
            intensity,
            magic,
            supernova_chance,
        );
    }

    /// Custom word-effect specs (native `[[sparkle_words.custom]]`): pass the
    /// SAME TOML fragment the native config carries — per-word `ink` /
    /// `burst` / `graphic` axes. Custom words are auto-appended to the
    /// emphasis class (CJK surfaces included), override class defaults, and
    /// bypass per-class enable gates. Malformed TOML fails open to no
    /// customs; pass `undefined` to clear.
    pub fn set_sparkle_custom_specs(&mut self, toml: Option<String>) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_sparkle_custom_specs(toml);
    }

    /// Arm (or clear) a **Trail Pack** — user-generated cursor trails as data.
    /// Pass the pack's TOML source (`trail_pack::compile_trail_pack_toml`);
    /// `undefined` clears any live pack. On a compile ERROR the prior pack is
    /// LEFT INTACT and the joined diagnostics are RETURNED (never silently
    /// dropped — the `set_sparkle_custom_specs` gap this closes); `Ok` returns
    /// `undefined`.
    pub fn set_cursor_trail_pack(&mut self, toml: Option<String>) -> Option<String> {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_cursor_trail_pack(toml)
    }

    /// Comma-separated languages whose AMBIGUOUS homograph lexicon entries
    /// un-gate (native `languages`, default `"en"`; non-ambiguous forms load
    /// regardless; `"all"` un-gates everything). Rebuilds the lexicon.
    pub fn set_sparkle_languages(&mut self, languages_csv: &str) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        let langs: Vec<&str> = languages_csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        self.effects.set_sparkle_languages(&langs);
    }

    /// User lexicon-override TOML merged over the builtin (the native
    /// `lexicon` file / `extra_words` channel — the same `[[entry]]` schema).
    /// Pass `undefined` to clear. A malformed override falls back to the
    /// builtin lexicon (the native fail-open posture).
    pub fn set_sparkle_lexicon_override(&mut self, toml: Option<String>) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_sparkle_lexicon_override(toml);
    }

    /// Comma-separated exact surfaces to never decorate (the native global
    /// `deny` and `ignore_words` channel), replacing the current set. Entries
    /// are case/diacritic-folded with the scanner's own fold.
    pub fn set_sparkle_deny(&mut self, words_csv: &str) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        let words: Vec<&str> = words_csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        self.effects.set_sparkle_deny(&words);
    }

    /// Lexicon build diagnostics (v3 §6), newline-joined — one warning per
    /// line for every user/custom surface that can never scan as written
    /// (single-char CJK without the `cjk_single_char` opt-in, mixed-script /
    /// multi-word) or collides across classes; the same warnings the native
    /// resolver logs. Empty string while sparkle words are off or the lexicon
    /// is clean. Filtered by the current knobs: a "requires cjk_single_char =
    /// true" warning disappears once `set_sparkle_feline` enables the opt-in.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn sparkle_lexicon_warnings(&self) -> String {
        self.effects.sparkle_lexicon_warnings().join("\n")
    }

    // ---- The resident pet (design 2026-08-30, Phase 1) -------------------
    //
    // The rainbow kitty pet's brain, art and driver all live in the engine
    // (`aterm_effects::companion::CompanionOwner`, driven by the shared
    // pipeline); these exports are the JS host's door to it. Every one of
    // them is O(1) — a field write, a value-shadowed compare, or a rect test
    // — so the wasm-process census (OB-10) sees no unbounded reach behind a
    // synchronous entry point. Defaults are OFF: a host that never calls
    // `set_cursor_pet` renders byte-identically to the pre-pet binding.

    /// Enable the resident pet beside the caret, dressed by `seed`
    /// (`KittyLook::for_launch` — the coat and iris a page load is born
    /// with). The seed is MINTED BY THE HOST (`crypto.getRandomValues` over
    /// a `BigUint64Array`; it crosses as a JS BigInt like `set_matrix_rain`'s
    /// seed) because the engine is clockless and dieless: same seed + same
    /// bytes + same `dt` stream ⇒ the same cat, pixel for pixel. The pet
    /// draws only while the cursor glow is on with a style that names it
    /// (`rainbow kitty`, `kitty`, `kitty pet`, `dog pet`, …); `false`
    /// retires it outright and the next render is byte-identical to a
    /// binding that never enabled it. The seed is read on enable only.
    pub fn set_cursor_pet(&mut self, enabled: bool, seed: u64) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — the pet fades in inside `apply`, which a gated frame
        // never runs. Bump so the gate opens for one frame and lets the
        // pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_cursor_pet(enabled, seed);
    }

    /// Whether the resident pet is enabled (the host's opt-in as last set —
    /// not whether it is drawn this frame; see `cursor_pet_alpha`).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cursor_pet_enabled(&self) -> bool {
        self.effects.cursor_pet_enabled()
    }

    /// The alpha the last `render` put the pet on glass with: `0` = nothing
    /// drawn (off, retired, unfocused, in history, or faded out), up to `255`
    /// for a fully present cat. The observability twin of the hit target —
    /// a page can tell "no cat" from "cat asleep" without reading pixels.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cursor_pet_alpha(&self) -> u8 {
        self.effects.cursor_pet_alpha()
    }

    /// The pointer's position in FRAME px (the canvas's own device pixels,
    /// chrome included — the same space `selection_start` speaks). The pet
    /// watches a moving pointer and pounces on a fast one; feed every
    /// `mousemove`. Value-shadowed: a sample equal to the last one changes
    /// nothing and costs no render, so an idle hover cannot delete the frame
    /// gate. A non-finite coordinate is dropped.
    pub fn note_pointer_px(&mut self, x: f32, y: f32) {
        // WF-1 frame gate: bump ONLY when the pointer actually moved — the
        // pipeline reports the value-shadowed edge, and an unconditional bump
        // here would reopen the gate on every mousemove event.
        if self.effects.note_pointer_px(x, y) {
            self.note_host_visual_change();
        }
    }

    /// The pointer left the surface (`mouseleave`): the pet stops watching
    /// and settles. Value-shadowed like `note_pointer_px` — a second leave
    /// changes nothing and costs no render.
    pub fn note_pointer_leave(&mut self) {
        // WF-1 frame gate: bump ONLY on the real edge (see `note_pointer_px`).
        if self.effects.note_pointer_leave() {
            self.note_host_visual_change();
        }
    }

    /// A left press at FRAME px `(x, y)`. Returns the engine's verdict as a
    /// small integer: `0` = pass (nothing of the engine's was under the
    /// pointer — start your selection as usual), `1` = the pet was petted
    /// and the press is CONSUMED (chrome wins: do not start a selection, do
    /// not encode a mouse report). Higher codes are reserved for later
    /// companions. Ask this BEFORE `selection_start`, exactly like the
    /// native app asks its chrome first. The body is padded by a 4 px slop
    /// so a near miss still strokes the cat.
    pub fn pet_press_px(&mut self, x: f32, y: f32) -> u8 {
        let outcome = self.effects.press_px(x, y);
        // WF-1 frame gate: bump ONLY on a hit — a petted cat purrs on the
        // NEXT render (the press latches; the tick acts), while a pass
        // changed nothing the gate cannot already see.
        if outcome != aterm_effects::host::PressOutcome::Pass {
            self.note_host_visual_change();
        }
        outcome as u8
    }

    /// The accessibility motion preference for EVERY effect at once (the
    /// page's `prefers-reduced-motion: reduce`): the pet is drawn but pinned
    /// at its station (no arc, no gait), sparkle words take the static path
    /// and PHOSPHOR freezes. The per-engine spellings
    /// `set_sparkle_reduced_motion` and `set_matrix_rain_reduced_motion` are
    /// ALIASES of this setter — one host fact, every effect, whichever name
    /// the page calls — NOT narrower knobs: calling either pins the pet too.
    /// A stable preference only, never a load term: the engine's own load
    /// shed is a separate post-tick alpha envelope.
    pub fn set_reduced_motion(&mut self, on: bool) {
        // WF-1 frame gate: an effects CONFIG/ignition change can light up
        // pixels on the NEXT render while `is_active()` still reads false at
        // gate time — decorations and comets ignite inside `apply`, which a
        // gated frame never runs. Bump so the gate opens for one frame and
        // lets the pipeline seed itself (same rule as `note_keystroke`).
        self.note_host_visual_change();
        self.effects.set_reduced_motion(on);
    }
}
