// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The effects API on [`WebTerminal`](crate::WebTerminal): cursor aurora / trail, sparkle words, rain, output streak and the pet, driven by the shared [`aterm_effects`] pipeline.

/// The JS-visible surface.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// Advance the effects clock by `dt_ms` (the host's rAF delta): same bytes and same `dt` stream give identical frames.
            pub fn advance_effects(&mut self, dt_ms: f64) {
                self.effects.advance(dt_ms);
            }

            /// `true` while any effect is animating.
            pub fn is_effects_active(&self) -> bool {
                self.effects.is_active()
            }

            /// Milliseconds until the next engine wake (a rain tick, a settling cursor's next step), or `undefined` when active frame-rate motion needs rAF (and when every effect is idle).
            pub fn effects_next_deadline_ms(&mut self) -> Option<f64> {
                self.effects.next_deadline_ms()
            }

            /// Focus gate: an unfocused pane fires no idle blink events.
            pub fn set_effects_focused(&mut self, focused: bool) {
                self.note_host_visual_change();
                self.effects.set_focused(focused);
            }

            /// Tri-state pane visibility for bounded rain draining: `focused|visible_unfocused|hidden`.
            pub fn set_effects_visibility(&mut self, state: &str) {
                self.note_host_visual_change();
                self.effects.set_effects_visibility(state);
            }

            /// Register one keystroke for comet ignition: fast sustained calls heat the cadence so the next `render` ignites the trail.
            pub fn note_keystroke(&mut self) {
                self.note_host_visual_change();
                self.effects.note_keystroke();
            }

            /// [`Self::note_keystroke`] with the typed character: the input-time witness the movement-admission gate needs before glow/trail styles follow the cursor (movement without one is program output).
            pub fn note_typed_char(&mut self, ch: char) -> bool {
                self.note_host_visual_change();
                self.effects
                    .note_committed_char(&mut self.term, &self.frame_scratch, ch)
            }

            /// Report one accepted input intent: `text`, `delete`, `navigate`, `submit`, `paste`, `kill` (`^U`/`^W`) or `kill-forward` (`^K`).
            pub fn note_console_input(&mut self, kind: &str) -> bool {
                let Some(kind) = aterm_effects::kitty_pet::PetInputKind::parse(kind) else {
                    return false;
                };
                self.note_host_visual_change();
                self.effects.note_console_input(kind);
                true
            }

            /// Configure the cursor aurora with the native knobs and clamps: `style` is any native `cursor_trail_style` spelling, read through the native table (`aterm_effects::cursor_glow::spellings`), aliases included (`rainbow kitty`, `nyan`, `lightning`, …); `off` draws nothing and an unknown spelling draws lumen.
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
                self.note_host_visual_change();
                self.effects.set_cursor_glow(
                    enabled,
                    style,
                    aterm_effects::cursor_fx::TrailColor::automatic_or_pinned(color),
                    accent,
                    u64::from(duration_ms),
                    length,
                    intensity,
                    radius,
                    ring,
                    self.theme_cursor,
                );
            }

            /// Set only the cursor glow's trail colour, written as the native `cursor_trail_color` is: `""` for each style's own colour, `"cursor"` for the cursor's colour on every style (laser drops its violet; it follows OSC 12 and OSC 112), or `"#RRGGBB"`.
            pub fn set_cursor_glow_color(&mut self, color: &str) -> bool {
                self.note_host_visual_change();
                self.effects.set_cursor_glow_color(color)
            }

            /// Configure the opaque comet trail (native `cursor_trail_style = "comet"`).
            pub fn set_cursor_trail(
                &mut self,
                enabled: bool,
                duration_ms: u32,
                length: u32,
                color: Option<u32>,
            ) {
                self.note_host_visual_change();
                self.effects.set_cursor_trail(
                    enabled,
                    u64::from(duration_ms),
                    length,
                    color,
                    self.theme_cursor,
                );
            }

            /// Enable PHOSPHOR matrix rain.
            pub fn set_matrix_rain_enabled(&mut self, on: bool) {
                self.note_host_visual_change();
                self.effects.set_matrix_rain_enabled(on);
            }

            /// Whether PHOSPHOR matrix rain is enabled.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn matrix_rain_enabled(&self) -> bool {
                self.effects.matrix_rain_enabled()
            }

            /// Configure PHOSPHOR using the native bounds.
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
                self.effects
                    .set_matrix_rain_theme(self.theme_bg, self.theme_fg, self.theme_cursor);
            }

            /// Alias of [`Self::set_reduced_motion`].
            pub fn set_matrix_rain_reduced_motion(&mut self, on: bool) {
                self.note_host_visual_change();
                self.effects.set_matrix_rain_reduced_motion(on);
            }

            /// Feed a terminal visual bell into PHOSPHOR's bounded alert tint.
            pub fn note_matrix_rain_bell(&mut self) {
                self.note_host_visual_change();
                self.effects.note_bell();
            }

            /// Feed wheel/PgUp activity from an alternate-screen TUI so rain pauses while the user reads its transcript.
            pub fn note_matrix_rain_alt_scroll(&mut self) {
                self.note_host_visual_change();
                self.effects.note_matrix_rain_alt_scroll();
            }

            /// Payload-free observable-work pulse.
            pub fn note_matrix_rain_signal(&mut self, code: u32, weight: u32) {
                self.note_host_visual_change();
                self.effects.note_matrix_rain_signal(code, weight);
            }

            /// Configure PRISM WAKE: program output (not typing) gets a short spectrum comet on the row that took ink, metered so a flood degrades to one constant-cost ribbon.
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

            /// Fold the theme into PRISM WAKE: additive light on a dark ground, a tinted shadow on a light one.
            pub fn set_output_streak_theme(&mut self, dark_theme: Option<bool>) {
                self.effects.set_output_streak_theme(
                    streak_is_dark(dark_theme, self.theme_bg),
                    self.theme_bg,
                    self.theme_fg,
                    self.theme_cursor,
                );
            }

            /// Reduced motion for PRISM WAKE: amplitude pins to zero, a full reset, so `is_effects_active` settles.
            pub fn set_output_streak_reduced_motion(&mut self, on: bool) {
                self.note_host_visual_change();
                self.effects.set_output_streak_reduced_motion(on);
            }

            /// Stamp one keystroke for PRISM WAKE's echo discount: output arriving within the window is the user's own echo and mints nothing.
            pub fn note_output_streak_keystroke(&mut self) {
                self.note_host_visual_change();
                self.effects.note_output_streak_keystroke();
            }

            /// Master sparkle-words switch.
            pub fn set_sparkle_words_enabled(&mut self, on: bool) {
                self.note_host_visual_change();
                self.effects.set_sparkle_enabled(on);
            }

            /// Whether the sparkle-words master is currently on.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn sparkle_words_enabled(&self) -> bool {
                self.effects.sparkle_enabled()
            }

            /// Per-class gates (native `[sparkle_words.<class>] enabled`); emphasis applies only while ink is on.
            pub fn set_sparkle_classes(
                &mut self,
                profanity: bool,
                feline: bool,
                _retired_orca: bool,
                emphasis: bool,
            ) {
                self.note_host_visual_change();
                self.effects
                    .set_sparkle_classes(profanity, feline, emphasis);
            }

            /// Animated-ink knobs (native `[sparkle_words.ink]`): `strength` 0..=1, `sweep_ms` 350..=6000 (floor 600 while `loop_`, the flash margin); `loop_` re-sweeps while the word stays visible.
            pub fn set_sparkle_ink(
                &mut self,
                enabled: bool,
                strength: f32,
                sweep_ms: u32,
                loop_: bool,
            ) {
                self.note_host_visual_change();
                self.effects
                    .set_sparkle_ink(enabled, strength, sweep_ms, loop_);
            }

            /// Alias of [`Self::set_reduced_motion`].
            pub fn set_sparkle_reduced_motion(&mut self, on: bool) {
                self.note_host_visual_change();
                self.effects.set_sparkle_reduced_motion(on);
            }

            /// Alt-screen suppression (native `[sparkle_words] suppress_in_alt_screen`, default off): when on, full-screen apps render undecorated.
            pub fn set_sparkle_alt_screen_suppression(&mut self, on: bool) {
                self.note_host_visual_change();
                self.effects.set_sparkle_alt_screen_suppression(on);
            }

            /// Feline knobs (native `[sparkle_words.feline]`): `style = "cat"` emits the cat; `"paw"` is ink-only.
            pub fn set_sparkle_feline(
                &mut self,
                style: &str,
                magic: bool,
                allow_bare_cat: bool,
                cjk_single_char: bool,
            ) {
                self.note_host_visual_change();
                self.effects
                    .set_sparkle_feline(style, magic, allow_bare_cat, cjk_single_char);
            }

            /// Profanity knobs (native `[sparkle_words.profanity]`): `style` is `"rainbow"` (default), `"nova"` or `"sparkle"`.
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

            /// Custom word-effect specs: the native `[[sparkle_words.custom]]` TOML fragment.
            pub fn set_sparkle_custom_specs(&mut self, toml: Option<String>) {
                self.note_host_visual_change();
                self.effects.set_sparkle_custom_specs(toml);
            }

            /// Comma-separated languages whose ambiguous homograph entries un-gate (native `languages`, default `"en"`; `"all"` un-gates everything).
            pub fn set_sparkle_languages(&mut self, languages_csv: &str) {
                self.note_host_visual_change();
                let langs: Vec<&str> = languages_csv
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                self.effects.set_sparkle_languages(&langs);
            }

            /// User lexicon-override TOML (the native `[[entry]]` schema) merged over the builtin; `undefined` clears.
            pub fn set_sparkle_lexicon_override(&mut self, toml: Option<String>) {
                self.note_host_visual_change();
                self.effects.set_sparkle_lexicon_override(toml);
            }

            /// Comma-separated exact surfaces never to decorate (native `deny` / `ignore_words`), replacing the current set; folded like the scanner folds.
            pub fn set_sparkle_deny(&mut self, words_csv: &str) {
                self.note_host_visual_change();
                let words: Vec<&str> = words_csv
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                self.effects.set_sparkle_deny(&words);
            }

            /// Lexicon build diagnostics, newline-joined: one warning per user/custom surface that can never scan as written or collides across classes, filtered by the current knobs.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn sparkle_lexicon_warnings(&self) -> String {
                self.effects.sparkle_lexicon_warnings().join("\n")
            }

            /// Enable the resident pet beside the caret, dressed by `seed` (`KittyLook::for_launch`).
            pub fn set_cursor_pet(&mut self, enabled: bool, seed: u64) {
                self.note_host_visual_change();
                self.effects.set_cursor_pet(enabled, seed);
            }

            /// Whether the resident pet is enabled (the host's opt-in, not whether it is drawn; see `cursor_pet_alpha`).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn cursor_pet_enabled(&self) -> bool {
                self.effects.cursor_pet_enabled()
            }

            /// The alpha the last `render` drew the pet with: `0` = nothing drawn, up to `255` for a fully present cat.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn cursor_pet_alpha(&self) -> u8 {
                self.effects.cursor_pet_alpha()
            }

            /// The pointer's position in frame px (the canvas's device pixels, chrome and message band included); feed every `mousemove`.
            pub fn note_pointer_px(&mut self, x: f32, y: f32) {
                // Bump only when the pointer moved, or every mousemove would reopen the gate.
                if self.effects.note_pointer_px(x, y) {
                    self.note_host_visual_change();
                }
            }

            /// The pointer left the surface (`mouseleave`): the pet stops watching and settles.
            pub fn note_pointer_leave(&mut self) {
                // Bump only on the real edge (see `note_pointer_px`).
                if self.effects.note_pointer_leave() {
                    self.note_host_visual_change();
                }
            }

            /// A left press at frame px `(x, y)`.
            pub fn pet_press_px(&mut self, x: f32, y: f32) -> u8 {
                let outcome = self.effects.press_px(x, y);
                if outcome != aterm_effects::host::PressOutcome::Pass {
                    self.note_host_visual_change();
                }
                outcome as u8
            }

            /// The reduced-motion preference for every effect (the page's `prefers-reduced-motion: reduce`): the cursor's light is off, the pet is drawn but pinned at its station, sparkle words take the static path and PHOSPHOR freezes.
            pub fn set_reduced_motion(&mut self, on: bool) {
                self.note_host_visual_change();
                self.effects.set_reduced_motion(on);
            }
        }
    };
}
web_terminal_impl!(bind);

/// The streak's polarity: the host's override if given, else the native classifier ([`aterm_render::theme_is_dark`]) applied to the `set_theme` background.
#[inline]
pub(crate) fn streak_is_dark(dark_theme: Option<bool>, theme_bg: u32) -> bool {
    dark_theme.unwrap_or_else(|| aterm_render::theme_is_dark(theme_bg))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::streak_is_dark;

    /// With no override the streak takes the native classifier's side, mid-greys
    #[test]
    fn streak_polarity_follows_the_theme_background_by_default() {
        for v in 0..=255u32 {
            let bg = v * 0x0001_0101;
            assert_eq!(
                streak_is_dark(None, bg),
                aterm_render::theme_is_dark(bg),
                "{bg:06x}"
            );
        }
        assert!(!streak_is_dark(None, 0x00A0_A0A0));
        assert!(streak_is_dark(None, 0x0011_1318));
    }

    /// A host-supplied bool overrides the derived side either way.
    #[test]
    fn a_host_override_wins_over_the_derived_side() {
        assert!(streak_is_dark(Some(true), 0x00FF_FFFF));
        assert!(!streak_is_dark(Some(false), 0x0000_0000));
    }
}
