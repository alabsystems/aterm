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
        self.effects.set_focused(focused);
    }

    /// Tri-state pane visibility for bounded rain draining:
    /// `focused|visible_unfocused|hidden`.
    pub fn set_effects_visibility(&mut self, state: &str) {
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
        self.effects.note_keystroke();
    }

    /// Configure the LUMEN cursor aurora (additive light in the cursor's
    /// wake). Mirrors the native knobs + clamps: `style` ∈
    /// `lumen|phaser|nyan|sparkle|fire|laser|beam|water|comet` (unknown →
    /// lumen; `rainbow` = the Nyan banded ribbon);
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

    /// Accessibility motion gate for PHOSPHOR.
    pub fn set_matrix_rain_reduced_motion(&mut self, on: bool) {
        self.effects.set_matrix_rain_reduced_motion(on);
    }

    /// Feed a terminal visual bell into PHOSPHOR's bounded alert tint.
    pub fn note_matrix_rain_bell(&mut self) {
        self.effects.note_bell();
    }

    /// Feed wheel/PgUp activity from an alternate-screen TUI so rain pauses
    /// while the user reads its transcript.
    pub fn note_matrix_rain_alt_scroll(&mut self) {
        self.effects.note_matrix_rain_alt_scroll();
    }

    /// Payload-free observable-work pulse. Codes are `0 assistant, 1 inspect,
    /// 2 modify, 3 execute, 4 network, 5 branch, 6 waiting, 7 success,
    /// 8 failure, 9 interrupted, 10 turn-start`; weight clamps to `1..=8`.
    /// Turn-start also releases the unsent-composer material gate.
    pub fn note_matrix_rain_signal(&mut self, code: u32, weight: u32) {
        self.effects.note_matrix_rain_signal(code, weight);
    }

    /// MASTER sparkle-words switch (native `[sparkle_words] enabled` +
    /// `toggle_sparkle_words` panic-off). Enabling compiles the multilingual
    /// lexicon once and starts scanning the visible grid; disabling drops all
    /// occurrence state and restores byte-identical output next render.
    /// Defaults (until other setters run) mirror the native launch config:
    /// all four families on (profanity nova / feline cat / orca splash /
    /// emphasis ink), animated ink on.
    pub fn set_sparkle_words_enabled(&mut self, on: bool) {
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
        self.effects
            .set_sparkle_classes(profanity, feline, orca, emphasis);
    }

    /// Animated-ink knobs (native `[sparkle_words.ink]`): the glyph-ink
    /// gradient + specular sweep on matched words. `strength` clamps 0..=1;
    /// `sweep_ms` clamps 350..=6000 (floor 600 while `loop_` — the WCAG flash
    /// margin, structural); `loop_` re-sweeps while the word stays visible.
    pub fn set_sparkle_ink(&mut self, enabled: bool, strength: f32, sweep_ms: u32, loop_: bool) {
        self.effects
            .set_sparkle_ink(enabled, strength, sweep_ms, loop_);
    }

    /// Force the static, non-animating path (no twinkle/jitter/sweep; novas
    /// collapse to a static glint) — the accessibility `reduced_motion`
    /// override. The engine's flash-limiter floors apply regardless.
    pub fn set_sparkle_reduced_motion(&mut self, on: bool) {
        self.effects.set_sparkle_reduced_motion(on);
    }

    /// Alt-screen suppression (native `[sparkle_words] suppress_in_alt_screen`,
    /// default off): when on, full-screen apps render undecorated — the v1
    /// launch behavior. Off, the alternate screen sparkles like the main one.
    pub fn set_sparkle_alt_screen_suppression(&mut self, on: bool) {
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
        self.effects.set_sparkle_custom_specs(toml);
    }

    /// Arm (or clear) a **Trail Pack** — user-generated cursor trails as data.
    /// Pass the pack's TOML source (`trail_pack::compile_trail_pack_toml`);
    /// `undefined` clears any live pack. On a compile ERROR the prior pack is
    /// LEFT INTACT and the joined diagnostics are RETURNED (never silently
    /// dropped — the `set_sparkle_custom_specs` gap this closes); `Ok` returns
    /// `undefined`.
    pub fn set_cursor_trail_pack(&mut self, toml: Option<String>) -> Option<String> {
        self.effects.set_cursor_trail_pack(toml)
    }

    /// Comma-separated languages whose AMBIGUOUS homograph lexicon entries
    /// un-gate (native `languages`, default `"en"`; non-ambiguous forms load
    /// regardless; `"all"` un-gates everything). Rebuilds the lexicon.
    pub fn set_sparkle_languages(&mut self, languages_csv: &str) {
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
        self.effects.set_sparkle_lexicon_override(toml);
    }

    /// Comma-separated exact surfaces to never decorate (the native global
    /// `deny` and `ignore_words` channel), replacing the current set. Entries
    /// are case/diacritic-folded with the scanner's own fold.
    pub fn set_sparkle_deny(&mut self, words_csv: &str) {
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
}
