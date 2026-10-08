// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The host-facing effects API on [`AtermGpuTerminal`]: cursor aurora / comet
//! trail and sparkle words, driven by the shared [`aterm_effects`] pipeline
//! (the same state machines the native app runs). Mirrors aterm-wasm's
//! `effects_api`; both feed the same `RenderInput` overlay channels, whose
//! CPU==GPU parity the aterm-gpu suite gates.
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
//! Every effect settles to a stable fingerprint, so `is_effects_active()`
//! returns `false` and the host goes idle. All effects default off, leaving
//! the render output unchanged.
//!
//! ## Frame-gate bumps
//!
//! Every config/ignition mutator opens with `self.note_host_visual_change()`:
//! a config change can light pixels on the next render while `is_active()`
//! still reads false at gate time, because decorations and comets ignite
//! inside `apply`, which a gated frame never runs. `advance_effects` does not
//! bump (hosts call it every rAF tick); its soundness rests on `is_active()`
//! plus these mutators.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

use crate::AtermGpuTerminal;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermGpuTerminal {
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
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_focused(focused);
    }

    /// Tri-state pane visibility for bounded rain draining:
    /// `focused|visible_unfocused|hidden`.
    pub fn set_effects_visibility(&mut self, state: &str) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_effects_visibility(state);
    }

    /// Register one keystroke for the cursor-comet ignition: sustained fast
    /// calls heat the typing cadence so the next `render` ignites the trail.
    /// The cadence reads the effects clock, so `advance_effects` must run
    /// between keystrokes. Call from the keydown handler that feeds
    /// `encode_key`. Also freezes literal-rain sampling while a draft is
    /// unsent; on submit call `note_matrix_rain_signal(10, 4)` after this.
    pub fn note_keystroke(&mut self) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.note_keystroke();
    }

    /// [`Self::note_keystroke`] with the typed character: the input-time
    /// witness the movement-admission gate requires before glow/trail styles
    /// may follow the cursor (unwitnessed movement is program output and stays
    /// dark). Call after dispatching the key's bytes and before the echo is fed
    /// to [`Self::process`], between renders; a stale witness fails closed.
    /// Returns whether movement provenance armed; otherwise the text-blind
    /// keystroke semantics still apply. The rainbow engine prices the
    /// character: a Space rests and deals no star, a capital or `!` earns a
    /// hero star.
    pub fn note_typed_char(&mut self, ch: char) -> bool {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects
            .note_committed_char(&mut self.term, &self.frame_scratch, ch)
    }

    /// Report one accepted input intent: `text`, `delete`, `navigate`,
    /// `submit`, `paste`, `kill` (`^U`/`^W`: the caret moves) or
    /// `kill-forward` (`^K`). Call after transport admission, once per
    /// committed IME run or paste, never for composition preview, formatting
    /// or PTY output. Empty input needs no note.
    ///
    /// The kind reaches the cursor engines with the native app's rules:
    /// `delete` is the erase (the ribbon retracts the erased cell),
    /// `navigate` licenses the hop it precedes, `submit` is Enter, a kill
    /// drains the line. Use this for every non-text key instead of
    /// [`Self::note_keystroke`], which would replay a Backspace as a typed
    /// glyph.
    ///
    /// Do not also call this for a scalar reported via
    /// [`Self::note_typed_char`]; for an IME bundle call this once instead.
    /// `text` and `paste` only supply pet attention. Returns `false` for an
    /// unknown kind without scheduling a frame.
    pub fn note_console_input(&mut self, kind: &str) -> bool {
        let Some(kind) = aterm_effects::kitty_pet::PetInputKind::parse(kind) else {
            return false;
        };
        // WF-1: a stationary-caret edit still needs one presentation.
        self.note_host_visual_change();
        self.effects.note_console_input(kind);
        true
    }

    /// Configure the lumen cursor aurora (additive light in the cursor's
    /// wake), with the native knobs and clamps: `style` ∈
    /// `lumen|phaser|rainbow kitty|sparkle|fire|laser|beam|water|comet`
    /// (unknown → lumen); `color`/`accent` omitted derive from the theme
    /// cursor (accent = color × 1.5); `duration_ms` 30..=2000, `length`
    /// (cells) 1..=512, `intensity` 0..=1 (0 = off), `radius` (bloom crown,
    /// cells) 0..=2, `ring` = landing-ring ping.
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
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
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

    /// Configure the opaque comet trail (native `cursor_trail_style =
    /// "comet"`). `color` omitted = theme cursor; `duration_ms` 30..=2000,
    /// `length` 1..=512. The native app enables one of trail/glow by style;
    /// the embedder decides here.
    pub fn set_cursor_trail(
        &mut self,
        enabled: bool,
        duration_ms: u32,
        length: u32,
        color: Option<u32>,
    ) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_cursor_trail(
            enabled,
            u64::from(duration_ms),
            length,
            color,
            self.theme_cursor,
        );
    }

    /// Enable phosphor matrix rain. With output material opted in, the
    /// pipeline samples supported literal codepoints outside the cursor/
    /// composer protection band and emits only into empty default-bg cells.
    pub fn set_matrix_rain_enabled(&mut self, on: bool) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_matrix_rain_enabled(on);
    }

    /// Whether PHOSPHOR matrix rain is enabled.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn matrix_rain_enabled(&self) -> bool {
        self.effects.matrix_rain_enabled()
    }

    /// Configure phosphor with the native bounds. `hue` is
    /// `matrix|theme|custom`; `hue_color` is used only for `custom`.
    /// `output_material` opts into literal screen codepoints; hosts that
    /// cannot protect their composer can leave it false.
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
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
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

    /// Alias of [`Self::set_reduced_motion`]: also pins the pet and statics
    /// sparkle words.
    pub fn set_matrix_rain_reduced_motion(&mut self, on: bool) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_matrix_rain_reduced_motion(on);
    }

    /// Feed a terminal visual bell into PHOSPHOR's bounded alert tint.
    pub fn note_matrix_rain_bell(&mut self) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.note_bell();
    }

    /// Feed wheel/PgUp activity from an alternate-screen TUI so rain pauses
    /// while the user reads its transcript.
    pub fn note_matrix_rain_alt_scroll(&mut self) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.note_matrix_rain_alt_scroll();
    }

    /// Payload-free observable-work pulse. Codes are `0 assistant, 1 inspect,
    /// 2 modify, 3 execute, 4 network, 5 branch, 6 waiting, 7 success,
    /// 8 failure, 9 interrupted, 10 turn-start`; weight clamps to `1..=8`.
    /// Turn-start also releases the unsent-composer material gate.
    pub fn note_matrix_rain_signal(&mut self, code: u32, weight: u32) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.note_matrix_rain_signal(code, weight);
    }

    /// Configure prism wake, the output streak: program output (not typing)
    /// gets a short spectrum comet on the row that took ink, metered so a
    /// burst gets one comet and a flood one constant-cost ribbon. `intensity`
    /// 0..=1 (0 resets), `tail` (cells) 4..=14, `max_streaks` 1..=4,
    /// `idle_secs` 2..=120; a pinned `seed` replays identical frames. The
    /// engine re-clamps every band, so no argument makes it loud or flashy.
    ///
    /// Off until called with `enabled = true`; toggling off drops the engine
    /// but keeps the knobs, so re-enabling restores this configuration.
    ///
    /// `sound` is accepted for parity with the native host but has no effect:
    /// the web has no audio host, so the pipeline drops the cue.
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
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
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

    /// Fold the resolved theme into prism wake. `dark_theme` picks the
    /// polarity: additive light on a dark ground, a tinted shadow-shimmer on a
    /// light one. Colours come from this binding's [`Self::set_theme`] state,
    /// so the streak always agrees with the grid.
    ///
    /// Call per frame. It does not reset the engine, so an OSC 11/12 recolour
    /// re-tints comets in flight; a host calling it only at boot keeps the old
    /// palette.
    pub fn set_output_streak_theme(&mut self, dark_theme: bool) {
        // No frame-gate bump: this is called every frame, and a bump would
        // reopen the gate every frame.
        self.effects.set_output_streak_theme(
            dark_theme,
            self.theme_bg,
            self.theme_fg,
            self.theme_cursor,
        );
    }

    /// Accessibility motion gate for prism wake. On pins amplitude to zero,
    /// which resets the engine (no quads, no cue), so `is_effects_active`
    /// settles. Off restores full amplitude; a host using a custom
    /// `intensity` must re-apply [`Self::set_output_streak`].
    pub fn set_output_streak_reduced_motion(&mut self, on: bool) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_output_streak_reduced_motion(on);
    }

    /// Stamp one keystroke for prism wake's echo discount: output within the
    /// window after a stamp is the user's echo and mints nothing. Separate from
    /// [`Self::note_keystroke`], so call both from the keydown handler. On the
    /// web this is the only echo defence (`input_hot` is false), so skipping
    /// it answers typing with comets.
    pub fn note_output_streak_keystroke(&mut self) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.note_output_streak_keystroke();
    }

    /// Master sparkle-words switch (native `[sparkle_words] enabled`).
    /// Enabling compiles the lexicon once and scans the visible grid;
    /// disabling drops all occurrence state and restores plain output next
    /// render. Defaults match the native config: every class on, animated ink
    /// on.
    pub fn set_sparkle_words_enabled(&mut self, on: bool) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_sparkle_enabled(on);
    }

    /// Whether the sparkle-words master is currently on.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn sparkle_words_enabled(&self) -> bool {
        self.effects.sparkle_enabled()
    }

    /// Per-class gates (native `[sparkle_words.<class>] enabled`): profanity
    /// (supernova/sparkle), feline (peeking cat/paw), emphasis (ink-only;
    /// effective only while ink is enabled). The third argument is accepted
    /// and ignored so `emphasis` stays in the fourth position.
    pub fn set_sparkle_classes(
        &mut self,
        profanity: bool,
        feline: bool,
        _retired_orca: bool,
        emphasis: bool,
    ) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects
            .set_sparkle_classes(profanity, feline, emphasis);
    }

    /// Animated-ink knobs (native `[sparkle_words.ink]`): glyph-ink gradient +
    /// specular sweep on matched words. `strength` 0..=1; `sweep_ms`
    /// 350..=6000 (floor 600 while `loop_`, a WCAG flash margin); `loop_`
    /// re-sweeps while the word stays visible.
    pub fn set_sparkle_ink(&mut self, enabled: bool, strength: f32, sweep_ms: u32, loop_: bool) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects
            .set_sparkle_ink(enabled, strength, sweep_ms, loop_);
    }

    /// Force the static path (no twinkle/jitter/sweep; novas become a static
    /// glint). Flash-limiter floors apply regardless. Alias of
    /// [`Self::set_reduced_motion`]: also pins the pet and freezes phosphor.
    pub fn set_sparkle_reduced_motion(&mut self, on: bool) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_sparkle_reduced_motion(on);
    }

    /// Alt-screen suppression (native `[sparkle_words] suppress_in_alt_screen`,
    /// default off): when on, full-screen apps render undecorated.
    pub fn set_sparkle_alt_screen_suppression(&mut self, on: bool) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
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
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects
            .set_sparkle_feline(style, magic, allow_bare_cat, cjk_single_char);
    }

    /// Profanity knobs (native `[sparkle_words.profanity]`): `style` =
    /// "rainbow" (animated rainbow ink, default) | "nova" | "sparkle". Clamps
    /// are the native flash-safety floors: `density` 1..=12 sparks, `anim_ms`
    /// 350..=10000, `jitter` 0..=6 px, `intensity` 0..=1. `magic` = rare
    /// Quasar/Singularity novas. `supernova_chance` (0..=100, 0 disables) is
    /// the super-nova escalation chance under `style = "rainbow"`. The
    /// window-wide ignition limiter (≤2 per rolling second) is always on.
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
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
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

    /// Custom word-effect specs (native `[[sparkle_words.custom]]`): the same
    /// TOML fragment the native config carries (per-word `ink` / `burst` /
    /// `graphic`). Custom words join the emphasis class, override class
    /// defaults and bypass class gates. Malformed TOML yields no customs;
    /// `undefined` clears.
    pub fn set_sparkle_custom_specs(&mut self, toml: Option<String>) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_sparkle_custom_specs(toml);
    }

    /// Arm or clear a Trail Pack (user-defined cursor trails as data) from its
    /// TOML source (`trail_pack::compile_trail_pack_toml`); `undefined` clears.
    /// On a compile error the prior pack stays and the joined diagnostics are
    /// returned; success returns `undefined`.
    pub fn set_cursor_trail_pack(&mut self, toml: Option<String>) -> Option<String> {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_cursor_trail_pack(toml)
    }

    /// Comma-separated languages whose ambiguous homograph entries are enabled
    /// (native `languages`, default `"en"`; `"all"` enables everything).
    /// Rebuilds the lexicon.
    pub fn set_sparkle_languages(&mut self, languages_csv: &str) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        let langs: Vec<&str> = languages_csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        self.effects.set_sparkle_languages(&langs);
    }

    /// User lexicon-override TOML merged over the builtin (native `lexicon` /
    /// `extra_words`, same `[[entry]]` schema). `undefined` clears; malformed
    /// input falls back to the builtin.
    pub fn set_sparkle_lexicon_override(&mut self, toml: Option<String>) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_sparkle_lexicon_override(toml);
    }

    /// Comma-separated exact surfaces to never decorate (the native global
    /// `deny` and `ignore_words` channel), replacing the current set. Entries
    /// are case/diacritic-folded with the scanner's own fold.
    pub fn set_sparkle_deny(&mut self, words_csv: &str) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        let words: Vec<&str> = words_csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        self.effects.set_sparkle_deny(&words);
    }

    /// Lexicon build diagnostics, newline-joined: one warning per user/custom
    /// surface that can never scan as written (single-char CJK without
    /// `cjk_single_char`, mixed-script, multi-word) or collides across
    /// classes. Empty while sparkle words are off or the lexicon is clean.
    /// Filtered by the current knobs.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn sparkle_lexicon_warnings(&self) -> String {
        self.effects.sparkle_lexicon_warnings().join("\n")
    }

    // ---- The resident pet ------------------------------------------------
    //
    // The pet's brain, art and driver live in the engine
    // (`aterm_effects::companion::CompanionOwner`); these exports are the JS
    // host's door to it. Each is O(1) (a field write, shadowed compare or rect
    // test). Off by default.

    /// Enable the resident pet beside the caret, dressed by `seed`
    /// (`KittyLook::for_launch`). The host mints the seed
    /// (`crypto.getRandomValues`, crossing as a BigInt) because the engine is
    /// clockless and dieless: same seed, bytes and `dt` stream give the same
    /// cat. The pet draws only while the cursor glow uses a style naming it
    /// (`rainbow kitty`, `kitty`, `kitty pet`, `dog pet`, …). `false` retires
    /// it. The seed is read on enable only.
    pub fn set_cursor_pet(&mut self, enabled: bool, seed: u64) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_cursor_pet(enabled, seed);
    }

    /// Whether the resident pet is enabled (the host's opt-in as last set —
    /// not whether it is drawn this frame; see `cursor_pet_alpha`).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cursor_pet_enabled(&self) -> bool {
        self.effects.cursor_pet_enabled()
    }

    /// The alpha the last `render` drew the pet with: `0` = nothing drawn
    /// (off, retired, unfocused, in history, or faded), up to `255`. Lets a
    /// page tell "no cat" from "cat asleep" without reading pixels.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cursor_pet_alpha(&self) -> u8 {
        self.effects.cursor_pet_alpha()
    }

    /// The pointer position in frame px (canvas device pixels, chrome and
    /// message band included). The pet watches a moving pointer and pounces
    /// on a fast one; feed every `mousemove`. Value-shadowed, so an idle hover
    /// costs no render. Non-finite coordinates are dropped.
    pub fn note_pointer_px(&mut self, x: f32, y: f32) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        if self.effects.note_pointer_px(x, y) {
            self.note_host_visual_change();
        }
    }

    /// The pointer left the surface (`mouseleave`): the pet stops watching and
    /// settles. Value-shadowed.
    pub fn note_pointer_leave(&mut self) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        if self.effects.note_pointer_leave() {
            self.note_host_visual_change();
        }
    }

    /// A left press at frame px `(x, y)` (chrome and message band included).
    /// Returns `0` = pass (start your selection as usual) or `1` = the pet
    /// was petted and the press is consumed (no selection, no mouse report).
    /// Higher codes are reserved. Ask before `selection_start`. The body hit
    /// rect has 4 px of slop.
    pub fn pet_press_px(&mut self, x: f32, y: f32) -> u8 {
        let outcome = self.effects.press_px(x, y);
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        if outcome != aterm_effects::host::PressOutcome::Pass {
            self.note_host_visual_change();
        }
        outcome as u8
    }

    /// Reduced motion for every effect (`prefers-reduced-motion: reduce`):
    /// cursor light off (aurora, block bodies, momentum glow, comet), pet
    /// drawn but pinned at its station, sparkle words static, phosphor
    /// frozen. `set_sparkle_reduced_motion` and
    /// `set_matrix_rain_reduced_motion` are aliases. A stable preference, not
    /// a load term (load shedding is a separate alpha envelope).
    pub fn set_reduced_motion(&mut self, on: bool) {
        // WF-1 twin bump (mirrored; see `note_host_visual_change`).
        self.note_host_visual_change();
        self.effects.set_reduced_motion(on);
    }
}
