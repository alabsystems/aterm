// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Config subsystem: the `aterm.toml` model (`Config`) plus the loaders and
//! precedence resolvers (font px, force scale, grid size, tab-strip rows), AND
//! the `App`-side font/scale/backend/config methods that consume them
//! (set_font_px, rebuild_backend, on_scale_factor_changed, on_resize,
//! reload_config, toggle_fullscreen). A verbatim inherent-impl split.

use aterm_core::terminal::{ColorPalette, CursorStyle, Rgb};
use aterm_render::Theme;
use std::io::Read as _;
use winit::dpi::PhysicalSize;

use crate::input::{InputEvent, Source};
use crate::platform::AppRt;
use crate::{
    App, Backend, FONT_PX, FONT_PX_MAX, FONT_PX_MIN, PresentTarget, WindowId, keybinding, term_lock,
};

/// THE PLATFORM DEFAULT for decoration that paints OVER content the user is trying
/// to read. `true` everywhere except WINDOWS.
///
/// WHY THE SPLIT, from this repo's own history rather than taste. The owner's
/// standing Windows directive is a minimal, fast, native terminal, and it was
/// already shipped once: `6272bd7a` ("fix(windows/ux): minimal fast defaults — kill
/// typing lag, yellow cast, HUD, loud cursor") turned `cursor_trail`,
/// `cursor_trail_bloom`, `stream_fade` and `show_hud` OFF after a Windows audit
/// traced typing lag to "decorative work shipped ON by default doing full-frame GPU
/// presents on the keystroke hot path", and `d22ba722` ("keep 6272bd7a minimal-fast
/// defaults OFF; correct the stale tests") restored that after a test-driven revert
/// put them back.
///
/// IT OWNS TWO KEYS.
///
/// * `pkg_progress_effects` — the provisioning card's party trim, the cat that stands
///   on the progress bar of the first-run "Installing the ALab toolchain" toast.
/// * `cursor_trail` — the audit's HEADLINE, and the one this family exists for. The
///   default STYLE became `rainbow kitty pet` long after `5b11ff2c` made the
///   "batteries-on delight" call about a ~260 ms aurora, so the master now seats a
///   permanently resident, walking, full-body cat drawn `FreeZ::OverText` across live
///   output; the auditor photographed it sitting on a line of real terminal text
///   beside the caret. The owner's own `aterm.toml` already writes
///   `cursor_trail = false` by hand — a default a user has to undo is not a default.
///   Native Settings seeds every row from the RESOLVED config, so this default
///   re-baselines the Windows projection of the disclosure ladder ("Inactive · Cursor
///   trail Off" on every dependent row), the music-suppression reason, the
///   cursor-runway preview and the compact pickers' pagination — those screens are
///   exactly what a macOS owner with `cursor_trail = false` sees today, and they were
///   reviewed as part of this change rather than left to a later hand.
///
/// WHAT IS NOT IN IT, and why — read before adding a key:
///
/// * `cursor_trail_bloom`. Every consumer reads it as `cursor_trail_or_default() && …`,
///   so with the master off it can already emit nothing; splitting it would buy no
///   pixel and no millisecond, and would only degrade the look for the Windows owner
///   who opts back in with `cursor_trail = true`.
/// * `[sparkle_words] enabled` — the word engine pushes its animal heads
///   `FreeZ::UnderText`, so every glyph still draws on top of the fur: it decorates
///   output rather than covering it, and its population is capped (`MAX_CATS = 8`)
///   rather than accumulating. (It is also a live product gate in native Settings with
///   its own disclosure ladder and migration path.) See
///   [`Config::sparkle_words_enabled_or_default`], which also names the one honest
///   exception.
/// * `hdr_glow`, `cursor_fire_shimmer` — both default ON, and both only do work on
///   frames carrying cursor-GLOW quads, which only the trail produces; with the
///   Windows trail default off, a fresh Windows config already pays neither.
/// * `stream_fade` (already OFF everywhere since `6272bd7a`), `show_hud` (a retired
///   key — the HUD is gone), functional motion, the visual bell, cursor blink, and
///   everything already opt-in (Robi, the ambient sound bed).
///
/// Nothing here silences a key the user set: every consumer is
/// `Option::unwrap_or(DEFAULT_DECORATIVE_EFFECTS)`, so an explicit `true` in
/// `aterm.toml` turns the effect on, on Windows exactly like anywhere else.
pub(crate) const DEFAULT_DECORATIVE_EFFECTS: bool = !cfg!(windows);

/// User config file (`$XDG_CONFIG_HOME/aterm/aterm.toml`, else
/// `~/.config/aterm/aterm.toml`). Every field is optional; unknown keys are
/// ignored (forward-compatible). Precedence at startup is env var > config >
/// built-in default, so existing `ATERM_*` usage and `-e`/`-d` flags still win.
/// v1 exposes the settings that were previously env-only; it will grow to mirror
/// the engine's `TerminalConfig` (colours, cursor, scrollback) as themes land.
///
/// `PartialEq` (derived through every embedded table) lets the prepared config
/// admission path skip runtime side effects when a worker supplies a semantic
/// no-op. Exact text, comments, and asset generations still advance through the
/// versioned native config service independently of that runtime dedupe.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    /// Glyph size in physical px (like `$ATERM_FONT_PX`).
    pub(crate) font_px: Option<f32>,
    /// GPU (wgpu/Metal/Vulkan) rendering. Default ON (all platforms) with an
    /// automatic CPU fallback if the GPU is unavailable (device init OR the first
    /// window's surface fails); set `gpu = false` (or `--cpu` / `$ATERM_CPU`) to
    /// force the CPU renderer. `$ATERM_GPU` forces on.
    pub(crate) gpu: Option<bool>,
    /// Scrollback history limit, in lines (engine `TerminalConfig.scrollback_limit`;
    /// default 100 000). 0 means unlimited (bounded only by the memory budget).
    pub(crate) scrollback_lines: Option<usize>,
    /// Cursor shape: `"block"` (default) or `"bar"` (alias `"beam"`). The underline
    /// option is retired (programs may still request it via DECSCUSR).
    pub(crate) cursor_style: Option<String>,
    /// Whether the cursor blinks (default true). Combined with `cursor_style`.
    pub(crate) cursor_blink: Option<bool>,
    /// Theme: default text colour, `#RRGGBB` (engine `default_foreground`).
    pub(crate) foreground: Option<String>,
    /// Theme: default background colour, `#RRGGBB` (engine `default_background`).
    pub(crate) background: Option<String>,
    /// Theme: cursor colour, `#RRGGBB` (engine `cursor_color`).
    pub(crate) cursor_color: Option<String>,
    /// Theme: selection-highlight colour, `#RRGGBB` (renderer `Theme.selection`).
    pub(crate) selection_color: Option<String>,
    /// Theme: indexed palette colours, `#RRGGBB`, by 0-based index (0–15 are the
    /// ANSI/bright set; up to 256). e.g. `palette = ["#1d1f21", "#cc6666", …]`.
    pub(crate) palette: Option<Vec<String>>,
    /// MOTION POLICY (W11): `"auto"` (DEFAULT — follow Reduce Motion live on
    /// macOS, sample the Windows animations switch at window attach, and use no
    /// OS-driven reduction where no query exists), `"full"` (always animate), or
    /// `"reduced"` (never animate). Governs every decorative animation — the
    /// cursor aurora, sparkle words, matrix rain, and the Settings effect
    /// demo — through ONE resolved [`crate::motion::MotionPolicy`]. Unknown
    /// values fall back to `auto`. Unfocused windows always demote to static
    /// effects regardless of this value. Hot-reloadable.
    pub(crate) motion: Option<String>,
    /// LOAD-ADAPTIVE MOTION (the Change #1 effect-shedding heuristic): when `true`
    /// (DEFAULT) a sustained RENDER-overload session drops every decorative animation
    /// to the proven zero-amplitude Reduced state so aterm stays responsive. Set
    /// `false` to opt OUT of the heuristic entirely — animations then follow `motion`
    /// / the OS Reduce-Motion flag alone. `motion = "full"` overrides the shed
    /// regardless of this (an explicit "always animate"). Hot-reloadable.
    pub(crate) load_adaptive_motion: Option<bool>,
    /// SERIOUS MODE: suppress every nonessential sound and decorative effect while
    /// preserving terminal behavior, cursor blink, visual bell/attention, and
    /// functional motion such as smooth scrolling. DEFAULT OFF. This is an
    /// overriding projection only: individual effect preferences and runtime rain
    /// overrides remain intact and become effective again when serious mode turns off.
    pub(crate) serious_mode: Option<bool>,
    /// Cursor MOTION TRAIL — the "streaming trailer" effect. DEFAULT ON and
    /// exactly idle at rest; set `cursor_trail = false` to opt out.
    pub(crate) cursor_trail: Option<bool>,
    /// Trail STYLE: `rainbow kitty pet` (DEFAULT — that momentum-driven banded
    /// rainbow ribbon with the full-body cat that walks, runs and pounces along
    /// the line), `rainbow kitty` (the same ribbon under the flying kitty head),
    /// `phaser` (a full-spectrum additive hue sweep along the
    /// swept path), `comet` (the cadence-comet: a directional fading comet of
    /// `TrailCell`s that ignites longer/hotter with fast sustained typing, wrapped in
    /// the additive light crown), `lumen` (the additive light crown only — comet +
    /// bloom + ping), `sparkle`
    /// (phaser comet + spark particles), `fire` (rising embers), `laser` (white-hot
    /// beam), `water`, `beam` (a bloom-free beam-only crown, no trail body), or `off`.
    /// (`nyan rainbow`, `nyan` and `rainbow` are back-compat aliases for
    /// `rainbow kitty`; `kitty pet`/`pet kitty` for `rainbow kitty pet`.)
    pub(crate) cursor_trail_style: Option<String>,
    /// Trail Pack manifests — user-generated cursor trails as data (design
    /// `docs/trail-packs.md`). Each entry is a path to a `*.toml` Trail Pack
    /// (`~` expanded); a loaded pack with id `X` is selected with
    /// `cursor_trail_style = "pack:X"`. Compiled fail-closed at load (byte cap,
    /// unknown fields/effects rejected, every scalar clamped); an unreadable or
    /// invalid pack is skipped with a warning, never disabling the whole trail.
    pub(crate) cursor_trail_packs: Option<Vec<String>>,
    /// Trail SOUND EFFECTS — the aural half of the trail styles: each style's
    /// signature palette (water's droplets, fire's crackle, the beam's photon
    /// hum…) plays softly on the same spawn edge that lights the trail.
    /// DEFAULT ON, and silent by construction whenever the trail itself is
    /// off, reduced-motion, or the window is unfocused. `trail_sounds = false`
    /// mutes them; the Settings › Cursor › Trail effect card has the toggle.
    pub(crate) trail_sounds: Option<bool>,
    /// Trail sound VOLUME, `0.0`–`1.0` (default `0.4` — tuned so a lone
    /// keystroke peaks ≈ −22 dBFS: audible in a quiet room, far under the
    /// bell). Config-file only; the panel exposes the on/off toggle.
    pub(crate) trail_sound_volume: Option<f32>,
    /// Trail sound AMBIENT BED (`trail_sound_bed`, default OFF — the owner
    /// dislikes the drone): the continuous per-style background texture that
    /// swells behind fast typing (water's stream, fire's ember wash, the
    /// beam's hum…). `false` gates the bed mixer ENTIRELY — the synth's bed
    /// layer is never fed, so it renders exactly zero samples (not a muted
    /// gain-0 pass) — while the discrete notes, the brrrring, the bonk and
    /// the melody are untouched. The bed DSP itself is kept intact behind
    /// this gate: a redesign tournament evaluates it next phase.
    pub(crate) trail_sound_bed: Option<bool>,
    /// TYPING SOUND (`trail_sound_style`, default `"auto"`): WHICH voice
    /// speaks for the keystroke sounds — the Settings ▸ Sound ▸ "Typing
    /// sound" picker. `"auto"` follows the visual trail style (each style's
    /// signature palette — today's sound, bit for bit). Every other value
    /// picks an instrument spoken whatever the trail looks like: the nine
    /// palettes by what they SOUND like — `"glass bell"` (the rainbow
    /// kitty's bell), `"warm pluck"` (lumen), `"glitter"` (sparkle), `"ice
    /// chime"` (comet), `"droplet"` (water), `"pew"` (phaser), `"zap"`
    /// (laser), `"tick"` (beam), `"crackle"` (fire) — the `"mechanical"`
    /// keyboard (switch click + case thock), and three sound-only voices:
    /// `"typewriter"` (slug clack + platen thud; a margin-bell ding and
    /// carriage zip on Enter), `"marimba"` (a warm rosewood bar under a yarn
    /// mallet) and `"felt"` (a felt-muted piano — the hush). Aliases accepted
    /// on load, never offered: the trail-style names (`water`, `comet`,
    /// `rainbow kitty`, …), `bell`, `raindrop`, `mech`, `thock`, `mechanical
    /// keyboard`, `piano`, `clack`, … (`SoundVoice::ALIASES`). Each keystroke,
    /// deletion, Enter and the cursor's melody speak in the chosen voice and
    /// its own ambient bed; the kitty's hold-song stays the kitty's. Every
    /// voice sits on the same −21 dBFS typing floor and under the same rate
    /// governor; volume/on-off/bed gates apply as usual. Picking a voice in
    /// Settings plays one keystroke of it.
    pub(crate) trail_sound_style: Option<String>,
    /// TONE MELODY (`tone_melody`, default ON): the trail-sound melody leans
    /// with the inferred MOOD of the line being typed — a tiny on-device
    /// classifier (`aterm_effects::tone`, multilingual char-n-gram net over
    /// the TYPED line only, never screen content) picks one of five coarse
    /// tones, and the synth answers with a scale-table/feel lean (calm
    /// breathes, excitement brightens a whole tone, frustration turns minor,
    /// playfulness goes suspended and skippy; technical/uncertain is
    /// bit-exactly today's sound). Shipped ENABLED because the effect is
    /// deliberately SUBTLE — same instruments, same volume, same governor,
    /// consonance invariant intact — and inert whenever trail sounds are
    /// (off, muted, unfocused, reduced-motion, headless). `false` pins the
    /// melody to today's neutral constitution and stops the classifier from
    /// ever running.
    pub(crate) tone_melody: Option<bool>,
    /// ROBI THE HELPER ROBOT (`robi`, default OFF — owner directive: the
    /// resident is opt-IN; `robi = true` in aterm.toml or the Settings toggle
    /// invites him back). A little white robot with a cyan visor (ported from
    /// the user's Nitro Keyboard game) lives on the glass as a PERMANENT
    /// RESIDENT once invited, cycling forever through his rounds: he walks
    /// along the row being typed, does jumping jacks while sharing a
    /// getting-started tip, extends a ladder up past the grid, climbs it,
    /// swings across the tab bar like monkey bars while sharing a deeper tip
    /// (aterm features, shell tricks, Claude Code tricks), drops, and rests —
    /// then goes around again. His tips render as a speech bubble above his
    /// head (the transient-notice pill, anchored to him). Typing `robi` or
    /// `robot` restarts his rounds at the greeting. His idle stands are
    /// static, so a resting Robi costs zero repaints. Hidden under reduced
    /// motion, serious mode, or the default OFF — never by focus or time.
    pub(crate) robi: Option<bool>,
    /// RAINBOW SPARKLE CELEBRATION (`notice_sparkle`, default ON — user-facing
    /// features ship enabled; this is an opt-OUT). The post-update notice — the
    /// one card whose whole job is to feel like a small reward — wears a
    /// hue-cycling badge and a ring of twinkling rainbow sparkles instead of a
    /// flat cursor-coloured disc. Purely decorative: the wording, the timing and
    /// the click behaviour are untouched, every OTHER notice (update ready,
    /// background status, Robi's tips) is unchanged, and reduced motion holds the
    /// hues still so the card is colourful without moving.
    pub(crate) notice_sparkle: Option<bool>,
    /// PROVISIONING PROGRESS-CARD EFFECTS (`pkg_progress_effects`, default ON —
    /// user-facing features ship enabled; this is an opt-OUT). The toolchain
    /// install's progress card wears the house party trim: a rainbow-filled
    /// bar, sparkles on each completed program, and the cursor kitty riding the
    /// bar's leading edge. Purely decorative: `false` keeps the card fully
    /// functional as a plain themed accent bar with text rows — the numbers,
    /// phases, queue order, dismiss/reopen behaviour and the honest
    /// not-running/failed states are identical either way. Reduced motion and
    /// serious mode strip the same trim without touching this preference.
    pub(crate) pkg_progress_effects: Option<bool>,
    /// SING-ALONG RIFF (`trail_sound_riff`, default ON — user-facing features
    /// ship enabled; this is an opt-OUT).
    ///
    /// The held-key celebration's song (`aterm_effects::kitty_sing` →
    /// `Celebration(RiffBar)`) is TIER 5, the loudest thing the engine emits.
    /// Before this key the only ways to quiet it were the master `trail_sounds`
    /// switch — which kills every sound, keystrokes included — or
    /// `trail_sound_volume`, which turns the keystrokes down with it. `false`
    /// schedules no riff bar while leaving every other voice at its configured
    /// level.
    ///
    /// SOUND ONLY: the sing-along's visuals (ribbon saturation, star shower,
    /// the dancing cat and its singing face) are deliberately NOT gated here —
    /// motion belongs to the motion contract, and the owner asked to quiet the
    /// song, not to cancel the celebration. Subordinate to the existing law:
    /// raw window focus × `trail_sounds` × `trail_sound_volume` still apply
    /// first, so this can only ever take sound away.
    pub(crate) trail_sound_riff: Option<bool>,
    /// AUDIBLE TERMINAL BELL (`bell_sound`, default ON — opt-OUT like every
    /// other user-facing feature here).
    ///
    /// BEL (0x07) rings the user's configured system alert sound: `NSBeep` on
    /// macOS, `MessageBeep` on Windows. It is NOT a synth voice, so
    /// `trail_sound_volume` and `trail_sounds` do not reach it — before this key
    /// it was the one sound in the product with no configuration at all. `false`
    /// suppresses the beep only; the visual bell flash and the urgent-window /
    /// Dock-bounce attention request stay, because those are how a muted
    /// terminal still surfaces background activity.
    ///
    /// Parsed and preserved everywhere; INERT off macOS/Windows, which have no
    /// beep call to gate (see `crate::diagnostics` capability warnings).
    pub(crate) bell_sound: Option<bool>,
    /// Trail colour, `#RRGGBB`. Defaults to the (themed) cursor colour, so the
    /// trail matches the cursor unless overridden here.
    pub(crate) cursor_trail_color: Option<String>,
    /// Secondary/accent colour, `#RRGGBB` (comet tail + landing ring of the LUMEN
    /// styles). Defaults to a brightened cursor colour.
    pub(crate) cursor_trail_accent: Option<String>,
    /// Path to a PNG sprite for the cat that flies in front of the cursor on the
    /// `rainbow kitty` style. Supply your own image (RGBA or RGB PNG, ideally a small
    /// transparent pixel-art sprite facing right); it is nearest-scaled to fit
    /// the cursor and flown in front. Unset ⇒ the built-in homage sprite. `~`
    /// expands to $HOME.
    ///
    /// The field name IS the TOML key (serde, no rename), and a config key has
    /// no alias seam the way the trail-style VALUE does — so this one keeps the
    /// `nyan` spelling on purpose: renaming it would orphan every
    /// `cursor_nyan_sprite` line already sitting in a user's `config.toml`.
    pub(crate) cursor_nyan_sprite: Option<String>,
    /// WALLPAPER image path: a picture drawn as the BACKDROP of every terminal
    /// tab (settings and other native tabs are never wallpapered). The image is
    /// cover-scaled to the window, toned toward the theme background by
    /// [`wallpaper_dim`](Self::wallpaper_dim), and shows through every cell
    /// whose background is the terminal default; selections and explicitly
    /// colored backgrounds still paint over it. Set from Settings ▸ Wallpaper
    /// (Choose Image… / Detach) or by hand. PNG everywhere; JPEG/HEIC/TIFF/GIF
    /// and friends decode through the system on macOS. `~` expands to $HOME.
    /// Unset ⇒ no wallpaper (the flat theme background, exactly as before).
    pub(crate) wallpaper: Option<String>,
    /// How strongly the wallpaper is toned toward the theme background for
    /// text legibility: `0.0` shows the raw image, `1.0` is the flat theme
    /// background. Default 0.3.
    pub(crate) wallpaper_dim: Option<f32>,
    /// WALLPAPER TEXT TINT: default-colored glyphs pick up the hue of the
    /// backdrop under their own cell at an automatically readable brightness —
    /// light ink over the picture's dark areas, dark ink over its bright
    /// ones, always clearing a WCAG contrast floor — so the text shimmers
    /// with the picture behind it and stays legible on any wallpaper.
    /// SGR-colored text (prompts, `ls` colors) keeps its own colors. Only
    /// active while a wallpaper is attached. Default ON.
    pub(crate) wallpaper_text_tint: Option<bool>,
    /// How long (milliseconds) a swept cell takes to fade out. Default 260.
    pub(crate) cursor_trail_ms: Option<u64>,
    /// Maximum comet length in cells (a long jump keeps the brightest cells
    /// nearest the cursor). Default 24.
    pub(crate) cursor_trail_length: Option<usize>,
    /// Aurora brightness 0.0..=1.0 (LUMEN styles). Default 0.7.
    pub(crate) cursor_trail_intensity: Option<f32>,
    /// Bloom-crown radius in cells (LUMEN styles); 0 disables the crown. Default 0.6.
    pub(crate) cursor_trail_radius: Option<f32>,
    /// Landing-ring "ping" on a jump (LUMEN styles). Default true.
    pub(crate) cursor_trail_ring: Option<bool>,
    /// TYPING WAKE length, in milliseconds of recent travel (`rainbow kitty`
    /// style). The plume under the line you are typing shows exactly this much
    /// of your recent hand movement, so the number IS its length: raise it for a
    /// longer trail, lower it for a terser one, and set `0` to turn the wake off
    /// while keeping the rainbow ribbon. Default 300 ms; clamped 0..=1500.
    pub(crate) cursor_trail_wake_ms: Option<u64>,
    /// GPU-only cursor-comet BLOOM: a soft gaussian halo around the streak,
    /// composited at present time on the GPU (all wgpu backends — DX12/Vulkan/Metal);
    /// the CPU/software path is unaffected. DEFAULT ON. Set
    /// `cursor_trail_bloom = false` to disable.
    pub(crate) cursor_trail_bloom: Option<bool>,
    /// Bloom STRENGTH — how much of the blurred glow is added back (0.0..=3.0).
    /// Default 0.85; higher is more radiant (very high can blow out toward white).
    pub(crate) cursor_trail_bloom_strength: Option<f32>,
    /// Bloom RADIUS — how far the halo spreads, in half-res texels (0.5..=8.0).
    /// Default 2.2; higher is a wider, softer glow.
    pub(crate) cursor_trail_bloom_radius: Option<f32>,
    /// GPU-only HEAT SHIMMER: the air above burning/glowing cursor cells
    /// refracts — a subtle rising heat-haze composited at present time on the
    /// GPU (the bloom's parity class; the CPU/software path has no shimmer).
    /// DEFAULT ON. Set `cursor_fire_shimmer = false` to disable.
    pub(crate) cursor_fire_shimmer: Option<bool>,
    /// M3 phase B — EDR cursor glow (macOS + GPU + a wide-gamut/HDR panel).
    /// DEFAULT ON. When available, new windows get an `Rgba16Float` swapchain
    /// tagged extended-linear-sRGB (wgpu auto-sets
    /// `wantsExtendedDynamicRangeContent`) and the LUMEN cursor aurora is
    /// re-emitted ABOVE SDR reference white — real light, bounded by the
    /// screen's `maximumExtendedDynamicRangeColorComponentValue` (re-queried on
    /// monitor changes). The GRID stays reference-white SDR (proven clamp), the
    /// offscreen/readback source of truth is untouched, and with this off the
    /// present is byte-identical to pre-M3 (the `HdrPresentGate` proof +
    /// aterm-gpu's `hdr_gate` suite). Hot-reloadable: turning it OFF kills
    /// the >1.0 emission immediately; turning it ON lets an existing f16-capable
    /// Windows surface upgrade when the live output HDR probe admits it.
    /// See [`Config::hdr_glow_or_default`].
    pub(crate) hdr_glow: Option<bool>,
    /// SDR glow-boost strength (0..=1): how much additive crown the cursor glow
    /// may add on a standard (non-HDR) desktop, on top of the offscreen-baked
    /// aurora. Swapchain-side only (the introspection/readback frame is
    /// untouched), budget-shaped by the theme background's darkness (light
    /// themes self-degrade to invisible), hard-capped at 0.35 by a proven bound.
    /// `0.0` disables. See [`Config::cursor_glow_sdr_boost_or_default`].
    pub(crate) cursor_glow_sdr_boost: Option<f32>,
    /// Predictive local echo (mosh-style speculative typing): `adaptive` (DEFAULT —
    /// paint the typed glyph INSTANTLY, the moment the shell has proven it echoes the
    /// current line, on local shells and remote/ssh alike), `off`, or `always`.
    /// Alt-screen (vim/less) and unechoed (password) contexts never show a guess in
    /// `adaptive` — the epoch resets each submitted line, so a password prompt shows
    /// nothing until it echoes (which it never does).
    pub(crate) predictive_echo: Option<String>,
    /// Focus-linked shell priority boost (Windows QoS): while an aterm window
    /// is FOCUSED, its visible tab's shell processes (and their ConPTY console
    /// hosts) run at ABOVE_NORMAL with power throttling (EcoQoS) claimed off,
    /// so keystroke echo never loses the CPU to background NORMAL-priority
    /// load — the ConPTY "laggy but smooth" starvation. On blur everything
    /// returns to NORMAL / system-managed. Root-shell-only: programs the shell
    /// launches (builds) still start at NORMAL. DEFAULT ON; `focus_boost =
    /// false` opts out. Hot-reloadable (applies on the next focus/tab change,
    /// and immediately on reload). A no-op on Unix (no ConPTY middlemen).
    pub(crate) focus_boost: Option<bool>,
    /// M2 "ink that dries": newly-streamed cells FADE IN from the cell
    /// background to their final foreground over `stream_fade_ms` (the exact
    /// linear-light blend on an ease-out envelope). DEFAULT OFF when absent;
    /// the generated starter file opts in explicitly. Hard bypasses
    /// keep latency-critical paths instant — keystroke echo, the alternate
    /// screen (vim/less), a viewport scrolled away from the bottom, and a
    /// Reduced motion policy (W11) all render exact bytes. Set
    /// `stream_fade = true` to enable.
    pub(crate) stream_fade: Option<bool>,
    /// How long (milliseconds) fresh ink takes to dry — the stream-fade
    /// window. Default 90; clamped to 16..=1000 so a typo can't wedge the
    /// fade timer on or strobe it.
    pub(crate) stream_fade_ms: Option<u64>,
    /// Named built-in colour scheme ("theme palette"), e.g. `theme = "Dracula"`
    /// (case-insensitive). One key sets the default fg/bg, cursor, selection AND the
    /// full ANSI 0–15 palette. The individual `foreground`/`background`/`cursor_color`/
    /// `selection_color`/`palette` keys still layer ON TOP (last-wins). An unknown
    /// name warns and falls back to the built-in default. See [`aterm_types::scheme`].
    ///
    /// AUTO LIGHT/DARK SPLIT: `theme = "dark:<name>,light:<name>"` follows the live
    /// OS appearance — aterm switches schemes when the desktop toggles light↔dark
    /// (the same `winit` signal that drives [`crate::app_colorscheme`]). A plain
    /// `theme = "<name>"` is used for BOTH appearances. A split that omits one side
    /// uses the built-in Default for that side. See [`Self::resolve_theme_name`].
    pub(crate) theme: Option<String>,
    /// Interactive shell to spawn (config `shell`). A program name resolved with
    /// smart discovery — `"bash"` finds Git for Windows' `bash.exe` even off
    /// `%PATH%`, `"pwsh"`/`"cmd"`/`"wsl"`/`"nu"` resolve too — or an absolute path
    /// used verbatim. Unset → the platform default (Windows: pwsh → powershell →
    /// cmd; Unix: `$SHELL`). Overridden by the `--shell` flag and, on Windows,
    /// still by `%ATERM_SHELL%` when this is unset. See `windows::shell`.
    ///
    /// Shell-integration tier by shell: zsh/bash/fish/pwsh and `"wsl"` (whose
    /// distro must use bash as its login shell) get the full OSC 7 + OSC 133
    /// set; `"cmd"` is PARTIAL — prompt marks, jump-to-prompt and cwd tracking
    /// work, but cmd has no hook for when a command starts or finishes, so its
    /// blocks carry no command text and no exit code and `wait` never fires;
    /// `"nu"` gets none. See `aterm_shell_integration::ShellType`.
    pub(crate) shell: Option<String>,
    /// Extra argv passed to the `shell` after argv[0] (config `shell_args`), e.g.
    /// `["-l", "-i"]` for a login+interactive bash — or, for `shell = "wsl"`,
    /// `wsl.exe`'s own options (`["-d", "Debian"]`). Ignored when `-e` runs a
    /// command. Unset → the bare interactive shell.
    pub(crate) shell_args: Option<Vec<String>>,
    /// Initial window width in columns (default 80, clamped 20..=500).
    pub(crate) columns: Option<u16>,
    /// Initial window height in rows (default 24, clamped 5..=300).
    pub(crate) lines: Option<u16>,
    /// How many of the newest addressable lines the ⌘F / socket `search` index retains
    /// before evicting the oldest — i.e. how deep a find reaches into scrollback (default
    /// 100 000 = the engine's `DEFAULT_MAX_CACHED_LINES`). Floored to the visible row count,
    /// so the LIVE SCREEN is always searched no matter how small this is set (`0` ⇒ live
    /// screen only). The index is built off the term lock and cached across keystrokes, so
    /// raising this deepens search at the cost of index memory, not per-keystroke latency;
    /// scrollback beyond it is honestly reported as a partial result. See [`search_index_depth`].
    pub(crate) search_history_lines: Option<u32>,
    /// Primary font FAMILY name (e.g. `"JetBrains Mono"`). Resolved to a font
    /// file via [`resolve_font_family`]; on a miss the loader falls back to
    /// `$ATERM_FONT` then the built-in [`FONT_CANDIDATES`], so an unset / unknown
    /// family is byte-identical to before.
    pub(crate) font_family: Option<String>,
    /// DISPLAY FACE (`display_font`): one of the bundled display faces by id
    /// (`"pixel"`, `"chunky"`, `"engraved"`, `"bubble"` —
    /// [`aterm_render::DISPLAY_FACES`]). When set it OUTRANKS `font_family`: the
    /// whole terminal renders in that face (resolved as the virtual family
    /// `display:<id>` from embedded bytes, never the filesystem). Unset = the
    /// normal font selection, byte-identical to before this key existed. The
    /// Settings "Display Faces" page drives this as mutually-exclusive
    /// toggles (all off ⇒ the key is cleared). Hot-reloadable. `$ATERM_FONT`
    /// still outranks it (env > config, the one precedence law).
    ///
    /// `game_font` is the DEPRECATED spelling and stays a serde alias: deleting
    /// a shipped key would turn every config that carries it into a complaint
    /// about a line the user wrote correctly at the time. Legacy VALUES
    /// (`minecraft`, …) resolve the same way — see
    /// [`aterm_render::DISPLAY_FACE_LEGACY_IDS`] and
    /// [`warn_deprecated_display_font_spelling`].
    #[serde(alias = "game_font")]
    pub(crate) display_font: Option<String>,
    /// REAL BOLD face for SGR-bold cells (W6, ghostty's `font-family-bold`): a
    /// family name or file path, resolved like `font_family` and injected via the
    /// renderer's `set_bold_font` seam — a true heavier weight instead of the
    /// synthetic embolden / discovered `-Bold` sibling. ABSENT = the existing
    /// lazy sibling discovery (byte-identical).
    pub(crate) font_family_bold: Option<String>,
    /// REAL ITALIC face for SGR-italic cells (W6, ghostty's
    /// `font-family-italic`). ABSENT = discovery / synthetic shear.
    pub(crate) font_family_italic: Option<String>,
    /// REAL BOLD-ITALIC face (W6, ghostty's `font-family-bold-italic`).
    /// Outranks `font_family_bold` + synthetic shear for bold-italic cells.
    pub(crate) font_family_bold_italic: Option<String>,
    /// Whether SYNTHETIC bold/italic (coverage dilation / shear) may be applied
    /// when no real styled face exists (W6, ghostty's `font-synthetic-style`).
    /// ABSENT = `true` (byte-identical); `false` renders such cells with the
    /// regular face. Real styled faces are unaffected.
    pub(crate) font_synthetic_style: Option<bool>,
    /// Ordered broad-coverage FALLBACK font chain (W6): family names or paths,
    /// most-preferred first — `fallback_fonts = ["Sarasa Mono", "Apple Symbols"]`
    /// or the comma-separated string form `fallback_fonts = "Sarasa Mono, Apple
    /// Symbols"` (what the Settings editor writes). Explicit entries strictly
    /// outrank the deprecated `$ATERM_FALLBACK_FONT` alias, which outranks the
    /// built-in discovery candidates (the renderer's proven
    /// `fallback_chain_order` law). Hot-reloadable.
    pub(crate) fallback_fonts: Option<FontList>,
    /// Monochrome SYMBOL fallback face (W6): family name or path, consulted only
    /// after the primary + broad fallback miss. Outranks the deprecated
    /// `$ATERM_SYMBOL_FONT` alias, then discovery. Hot-reloadable.
    pub(crate) symbol_font: Option<String>,
    /// Colour-EMOJI face (W6): family name or path. Outranks the deprecated
    /// `$ATERM_EMOJI_FONT` alias, then discovery. Hot-reloadable.
    pub(crate) emoji_font: Option<String>,
    /// Window CHROME appearance (titlebar / traffic lights), independent of the
    /// terminal body theme: `"auto"` (default — follow the OS light/dark setting,
    /// including live day-night switches), `"light"`, or `"dark"`. Maps to
    /// [`WindowTheme`] via [`Config::window_theme_or_default`]; an unknown value
    /// warns and falls back to `auto`. This is cross-platform: AppKit sets the
    /// native NSWindow appearance, Windows applies the DWM/winit titlebar theme,
    /// and Linux forwards the choice to winit's system-decoration theme.
    pub(crate) window_theme: Option<String>,
    /// GPU-present COLOUR SPACE (M3 phase A): the colour space the window's
    /// CAMetalLayer is TAGGED with at surface attach — aterm's declared
    /// interpretation of the submitted sRGB-encoded bytes. `"srgb"` (the
    /// default) requests one sRGB→display mapping. `"display-p3"` reproduces the
    /// legacy UNTAGGED look on a wide-gamut Mac (the bytes are read as P3
    /// coordinates — oversaturated but familiar). Compositor colour management
    /// and display output are unobserved; the rendered/readback BYTES remain
    /// identical either way (the parity/readback suites pin this).
    /// Maps to [`WindowColorspace`] via [`Config::window_colorspace_or_default`];
    /// unknown values warn and fall back to `srgb`. Hot-reloadable; inert off
    /// macOS and on the CPU (softbuffer) present path.
    pub(crate) window_colorspace: Option<String>,
    /// When `true`, the Alt/Option modifier sends ESC-prefixed (Meta) key
    /// sequences — the standard terminal expectation. When `false`, a composed
    /// text value supplied by the OS is forwarded instead (for example Option+A
    /// → `å` on macOS); non-text Alt chords still use the terminal encoder.
    /// ABSENT keeps the cross-platform default (Meta), so no config is unchanged.
    /// See [`Config::option_as_meta_or_default`].
    pub(crate) option_as_meta: Option<bool>,
    /// Pastejacking guard: when `true` (the DEFAULT), a Cmd-V / menu paste of
    /// MULTI-LINE clipboard text is confirmed first (a native dialog) WHENEVER the
    /// terminal is not in bracketed-paste mode — so a hidden newline in copied text
    /// can't silently submit commands at a bare prompt / REPL. A single trailing
    /// newline is not multi-line and is never flagged; bracketed paste (the modern
    /// shell default) bypasses it entirely. Set `false` to paste without confirmation.
    /// See [`Config::confirm_multiline_paste_or_default`].
    pub(crate) confirm_multiline_paste: Option<bool>,
    /// RESTORE-1: reopen the previous graceful quit's windows/tabs/panes (with each
    /// pane's cwd) at the next launch, macOS-Terminal/iTerm style. Default ON — a v1
    /// daily driver is expected to come back the way it was left. Set
    /// `restore_session = false` for a fresh single window every launch. Scrollback
    /// content is NOT persisted (layout + cwd only). See
    /// [`Config::restore_session_or_default`].
    pub(crate) restore_session: Option<bool>,
    /// Copy a mouse selection to the system CLIPBOARD automatically the moment a
    /// drag-select completes (mouse-up), so no explicit Cmd-C is needed. DEFAULT
    /// ON off Linux — the copy-on-select convenience, flipped on with the other
    /// visual/UX defaults; set `copy_on_select = false` to opt out (the
    /// explicit-copy behaviour). ON LINUX the default is OFF, because a
    /// selection there already owns the X11 PRIMARY buffer unconditionally
    /// (`finish_selection`) — the platform convention is selection→PRIMARY,
    /// explicit copy→CLIPBOARD, and defaulting this on made every drag CLOBBER
    /// the explicit Ctrl+Shift+C copy (audit finding); `copy_on_select = true`
    /// still opts into writing both. The selection is left highlighted either
    /// way, so Cmd-C / Ctrl+Shift+C still works. See
    /// [`Config::copy_on_select_or_default`].
    pub(crate) copy_on_select: Option<bool>,
    /// RIGHT-CLICK gesture in the terminal grid: `"copy_paste"` — the
    /// conhost/Windows-Terminal convention (a right press with VT mouse tracking
    /// OFF copies the selection if one exists, else pastes) — or `"off"` (the
    /// press is left to the seam: reported to a tracking app, inert otherwise).
    /// ABSENT takes the PLATFORM default: `copy_paste` on Windows (both native
    /// terminals ship it, so a Windows hand expects it), `off` everywhere else
    /// (macOS right-click means "context menu"; Linux pastes on MIDDLE click —
    /// seeding a second paste button there would be surprising, not native).
    /// Maps to [`RightClickGesture`] via [`Config::right_click_or_default`]; an
    /// unknown value warns and falls back to the platform default.
    pub(crate) right_click: Option<String>,
    /// Which KEYBOARD chord pops a tab's context menu (Windows only; the strip's
    /// right-click always does): `"on"` — the dedicated Menu/Application key AND
    /// Shift+F10, the two spellings Windows has shipped since NT; `"menu_key"` —
    /// the Menu key ONLY, handing Shift+F10 back to the terminal (it is a real
    /// encodable chord, F20 in the xterm tradition); or `"off"` — neither, both
    /// keys go to the application and the menu is pointer-only. ABSENT = `"on"`.
    ///
    /// This exists because the chord is deliberately NOT a rebindable
    /// `[keybindings]` action (an OS convention, not an aterm command — and a
    /// config typo must not be able to delete the only keyboard route to the
    /// menu), which without a knob would leave a user no way to give the keys
    /// back at all. A one-key ESCAPE HATCH is not the same hazard as a rebind
    /// table: it cannot silently shadow the chord, it can only surrender it.
    ///
    /// Independent of the deference rule that always applies: a front terminal
    /// whose app negotiated the kitty enhancement that makes the key reportable
    /// keeps that key regardless of this setting (see
    /// `App::front_defers_tab_menu_chord`). Maps to [`TabMenuChord`] via
    /// [`Config::tab_menu_chord_or_default`]; an unknown value warns and falls
    /// back to the default.
    pub(crate) tab_menu_chord: Option<String>,
    /// WHERE A NEW TERMINAL OPENS when one is already running: `"new_window"`
    /// (the DEFAULT — every launch is its own window and its own process, which
    /// is what aterm has always done) or `"attach"` (a launch joins the running
    /// instance as a tab and exits; with nothing reachable it starts one, so the
    /// first launch of the day is unchanged). Windows Terminal's own spellings
    /// `useNew` / `useExisting` are accepted aliases.
    ///
    /// This key is read by the FRONT DOOR (`crates/aterm/src/main.rs`), not by
    /// the window — the decision happens before a window exists — and by the
    /// Windows jump list, which uses it to decide whether a "New Tab" taskbar
    /// task would tell the truth. `aterm new-window` is never routed by it, so
    /// there is always a way to get a separate window. Parsed by
    /// `aterm_cli::WindowingBehavior` (that crate owns the front-door grammar
    /// and this crate does not depend on it); an unrecognized value warns once
    /// and falls back to `new_window` — a typo may not move where terminals open.
    /// `$ATERM_WINDOWING_BEHAVIOR` overrides it, as every other key's env twin
    /// does.
    pub(crate) windowing_behavior: Option<String>,
    /// Show the subtle TOP-RIGHT build/version badge (`v{version} · {build}`) so the
    /// running build is answerable at a glance without opening About. Default OFF.
    /// Toggleable via the native Settings tab ▸ Window ▸ "Show build/version badge". See
    /// [`Config::show_build_badge_or_default`] and [`crate::build_badge`].
    pub(crate) show_build_badge: Option<bool>,
    /// User keyboard shortcuts: a `[keybindings]` table mapping chord strings
    /// (`"cmd+shift+t"`) to action names (`"new_tab"`). Parsed into a
    /// `HashMap<Chord, Action>` checked first in `on_key`; a miss falls through to
    /// the hardcoded defaults, and a malformed entry is warned + skipped. ABSENT =
    /// an empty map (the hardcoded path is reached unchanged).
    pub(crate) keybindings: Option<std::collections::BTreeMap<String, String>>,
    /// User INPUT POLICY: a `[key_sequences]` table mapping chord strings
    /// (`"shift+enter"`, `"f5"`) to the RAW BYTES that chord should send to the PTY,
    /// overriding aterm's default key encoding for ANY app. Values expand
    /// `\n \r \t \e \0 \a \b \f \v \\ \xNN \u{NNNN}` escapes. A TOML *basic*
    /// (double-quote) string only understands `\n \r \t`; put any value containing
    /// `\e`, `\xNN`, or `\u{...}` in a TOML *literal* (single-quote) string (`'\e[A'`),
    /// or a stray backslash escape is a TOML syntax error that fails the WHOLE config.
    /// Consulted in `on_key` after `[keybindings]` and BEFORE the encoder AND the
    /// non-menu hardcoded chords, so a rule on e.g. `f5` or `shift+enter` wins. NOTE: on
    /// macOS the native menu key equivalents (Cmd-C Copy, Cmd-V Paste, Cmd-F Find, …) are
    /// intercepted by AppKit BEFORE `on_key`, so a rule on those never fires (the menu
    /// claims them first). A bad chord / escape / empty / oversized (>1 KiB) value is
    /// warned + skipped; `aterm --validate-config` flags them. ABSENT = no overrides.
    pub(crate) key_sequences: Option<std::collections::BTreeMap<String, String>>,
    /// Rows reserved at the TOP of the window for the in-grid tab strip. DEFAULT is
    /// now `0` ([`DEFAULT_TAB_STRIP_ROWS`]) — the in-grid strip read as a non-native
    /// "ugly frame" drawn inside the terminal, and the native macOS window TOOLBAR
    /// (toolbar.rs) now carries the New Tab affordance. Set `tab_strip_rows = 1` in
    /// config to bring the in-grid strip back. Clamped to [`MAX_TAB_STRIP_ROWS`].
    pub(crate) tab_strip_rows: Option<u16>,
    /// C3 (Windows) — how TALL the in-grid tab band is: `"standard"` (the Windows
    /// DEFAULT) sizes the whole band to [`TAB_BAND_STANDARD_LOGICAL_PX`] ≈ a WinUI
    /// tab, `"compact"` keeps the pre-C3 height (exactly the strip's cell rows plus
    /// the top pad). The extra pixels are a SYNTHETIC chrome `head` band — the same
    /// mechanism macOS uses for chrome that overlaps the grid — so the resize law,
    /// the pointer mapping, the chrome bleed and the pixel band's centring already
    /// understand them; see `App::synthetic_strip_head_px`.
    ///
    /// OFF WINDOWS THIS KEY IS INERT (parsed, never consulted): macOS puts its tabs
    /// in the native toolbar and Linux's in-grid strip is tuned to its own chrome.
    /// It is also inert whenever the strip itself is off (`tab_strip_rows = 0`) —
    /// a band with no strip in it would be dead space that shifts the grid down.
    /// An unknown value warns and falls back to the platform default.
    pub(crate) tab_band_height: Option<String>,
    /// SELECTED-TAB color override (`active_tab_color`, `#RRGGBB`): paints the
    /// ACTIVE tab's background with a user-picked color in BOTH tab renderers —
    /// the native macOS toolbar pill and the in-grid strip. The label ink flips
    /// black/white by the override's own luminance so the title stays readable
    /// on any pick. ABSENT = today's translucent system default, byte-identical
    /// (the Settings "Tab Color" page calls that "Transparent white"). Driven
    /// by the Tab Color spectrum page; hot-reloadable.
    pub(crate) active_tab_color: Option<String>,
    /// Generate a live Activity fallback as terminal output changes. Default ON;
    /// `false` disables generation while preserving both the stable Title and any
    /// authored Description supplied by the session.
    pub(crate) descriptive_titles: Option<bool>,
    /// How live title descriptions are produced: `builtin` (default, local and
    /// deterministic), `ollama`, `openai-compatible`, or `off`.
    pub(crate) title_summary_provider: Option<TitleSummaryProvider>,
    /// Model identifier used by an LLM summary provider. Defaults to the small
    /// quantized local model `qwen3.5:4b-q4_K_M`.
    pub(crate) title_summary_model: Option<String>,
    /// Explicit chat-completions endpoint for an LLM summary provider. When this
    /// is absent/blank, Ollama uses an aterm-owned per-process ephemeral loopback
    /// endpoint; an OpenAI-compatible provider must configure one explicitly.
    pub(crate) title_summary_endpoint: Option<String>,
    /// Optional path to a bearer-token file for an OpenAI-compatible endpoint.
    /// The token is intentionally indirect so it need not live in `aterm.toml`.
    pub(crate) title_summary_token_file: Option<String>,
    /// End-to-end HTTP request timeout for model providers, in seconds.
    /// Defaults to 20 and is clamped to 1..=120.
    pub(crate) title_summary_timeout_seconds: Option<u64>,
    /// Proxy policy for remote model requests. `environment` (default) honors
    /// the standard HTTP(S)_PROXY/NO_PROXY environment; `direct` bypasses it.
    /// Aterm's attested managed Ollama is always direct regardless of this key.
    pub(crate) title_summary_proxy_mode: Option<TitleSummaryProxyMode>,
    /// Optional path to a PEM CA bundle for private remote model services. This
    /// is path-only: certificate material does not live in the config file. When
    /// configured, this bundle replaces platform roots for that provider.
    pub(crate) title_summary_ca_file: Option<String>,
    /// Minimum time between live-description refreshes, in seconds. Defaults
    /// to 15 and is clamped to 5..=300.
    pub(crate) title_summary_interval_seconds: Option<u64>,
    /// Number of recent terminal lines supplied to the summarizer. Defaults to
    /// 24 and is clamped to 4..=80.
    pub(crate) title_summary_context_lines: Option<usize>,
    /// Include recent terminal output in the summary context. Default ON.
    pub(crate) title_summary_include_output: Option<bool>,
    /// Permit an untrusted summary service. Default OFF; this is the explicit
    /// privacy gate for remote endpoints and pre-existing loopback listeners.
    /// Aterm's own managed, cloud-disabled Ollama child does not need consent.
    pub(crate) title_summary_allow_remote: Option<bool>,
    /// Composition of application title and live description in tab labels.
    /// Defaults to `title-description`.
    pub(crate) tab_title_format: Option<TitleFormat>,
    /// Composition of application title and live description in the native
    /// window title. Defaults to `title-description`.
    pub(crate) window_title_format: Option<TitleFormat>,
    /// Master switch for per-session status classification (RFC: Tab Subject &
    /// Status §12). Default ON. `false` genuinely stops the classifier — no
    /// evidence is gathered, no terminal lock is attempted, and no session ever
    /// publishes a status — rather than merely hiding what it computed.
    pub(crate) tab_status: Option<bool>,
    /// How long a foreground job may print nothing before its phase becomes
    /// `quiet`. Default 5000; clamped to 500..=120000 so a typo can neither make
    /// every job look stalled instantly nor keep a finished build lit for hours.
    pub(crate) tab_status_quiet_after_ms: Option<u64>,
    /// Minimum time a candidate phase must hold before it is published. Default
    /// 750; clamped to 0..=10000. Zero is legal and means "publish every
    /// transition", which is honest for scripted/headless drivers even though it
    /// lets a spinner flap the badge.
    pub(crate) tab_status_dwell_ms: Option<u64>,
    /// Whether a classified status projects onto the tab's busy/attention
    /// indicator bits. Default ON. Turning this off keeps classification running
    /// (the record stays readable through the `status` verb and the tooltip) and
    /// only stops the tab chrome from marking itself.
    pub(crate) tab_status_badge: Option<bool>,
    /// Whether a session's live connection role (Session Connections §4) draws
    /// the fourth tab status mark (▲ outbound / ▽ inbound / hourglass both).
    /// Default ON. Turning this off hides the MARK only: the connections
    /// themselves, their audit trail, and the wire/introspection surfaces
    /// (`chrome` states, `edges`) stay live — authority must never be quieter
    /// than the chrome showing it.
    pub(crate) tab_connection_badge: Option<bool>,
    /// BiDi (right-to-left) text handling: `"implicit"` (default — automatic
    /// per-line UAX#9 reordering, so Hebrew/Arabic display in visual order),
    /// `"disabled"` (keep logical order), or `"explicit"` (app-controlled). Maps to
    /// the engine `BiDiConfig.mode`. ABSENT keeps the engine default (Implicit).
    pub(crate) bidi: Option<String>,
    /// East-Asian Ambiguous-width characters: `"narrow"` (default, 1 cell) or
    /// `"wide"` (2 cells). Maps to the engine `ambiguous_width_double`. CJK users
    /// who expect ambiguous glyphs (some punctuation, line-drawing) to be
    /// double-width set `"wide"`. ghostty has no equivalent knob.
    pub(crate) ambiguous_width: Option<String>,
    /// Programming LIGATURES (`=>`, `!=`, `===`, …) for fonts that carry them
    /// (JetBrains Mono, Fira Code, Cascadia Code). Default ON (`true`). Set `false`
    /// to render strictly per-cell. Maps to the renderer's `LigatureMode`
    /// (`Enabled`/`Disabled`); the cursor's cell always renders un-ligated. ABSENT =
    /// on, so no config is byte-identical to the pre-ligature-config renderer.
    pub(crate) ligatures: Option<bool>,
    /// MERGED (Cascadia N:1) ligatures: admit runs whose OpenType shaping collapses
    /// several cells into ONE wide glyph (Cascadia Code's convention), rendered by
    /// slicing that glyph into per-cell tiles. DEFAULT OFF (`false`/absent) — the
    /// renderer stays on the 1:1 "spacer convention" every Fira/JetBrains-style font
    /// uses, byte-identical to before. Turn ON only with a merged-ligature font
    /// (Cascadia Code); a no-op on 1:1 fonts. Maps to `TextShapingConfig::admit_collapsed`.
    pub(crate) merged_ligatures: Option<bool>,
    /// OpenType FONT FEATURES applied to the primary face, e.g.
    /// `font_features = ["ss01", "zero", "-calt"]`. A bare tag (or `+tag`) ENABLES
    /// the feature, `-tag` disables it, and `tag=N` sets an explicit value
    /// (stylistic alternates). Tags are 1–4 ASCII chars; unknown/typo'd tags are
    /// harmlessly ignored by the shaper. Each list entry may itself be a
    /// space-separated group (`"+ss01 +zero"`). ABSENT = no extra features
    /// (byte-identical to before). See [`Config::text_shaping`].
    pub(crate) font_features: Option<Vec<String>>,
    /// How glyph antialiasing coverage blends over the cell (W2):
    /// `"linear-corrected"` (DEFAULT — linear-light blending with ghostty's
    /// perceptual alpha remap, so text carries the apparent stroke weight of
    /// native CoreText apps) or `"linear"` (the exact physical blend; midtone
    /// fringes render brighter, which reads thin in dark themes). An unknown
    /// value warns and keeps the default. Hot-reloadable (appearance-only).
    /// See [`Config::text_blending_or_default`].
    pub(crate) text_blending: Option<String>,
    /// macOS: rasterize glyphs with CoreText FONT SMOOTHING (Apple's stem
    /// darkening) for a heavier, native-weight glyph — ghostty's
    /// `font-thicken`. Default `false`. Parsed but inert off macOS (the
    /// portable fontdue path has no smoothing). Hot-reloadable.
    pub(crate) font_thicken: Option<bool>,
    /// Variable-font axis requests for the PRIMARY face (W9), e.g.
    /// `font_variation = ["wght=450", "opsz=14"]`. Each entry is `tag=value`
    /// (1–4 char OpenType axis tag); values are clamped to the font's `fvar`
    /// axis bounds, tags the font has no axis for are ignored, and malformed
    /// entries warn + skip. ABSENT = the default instantiation — the
    /// `Regular` named instance, else `wght=400` clamped (the SF Mono
    /// rescue) — and a non-variable font is byte-identical to before.
    /// A [`FontList`] so the Settings editor can round-trip it as one
    /// comma-joined string (like `fallback_fonts`); the TOML-array spelling
    /// `["wght=450", "opsz=14"]` still parses identically.
    pub(crate) font_variation: Option<FontList>,
    /// Requested `wght` (weight) for the primary face (W9), 1–1000 — the
    /// discoverable alias of `font_variation = ["wght=N"]`, applied AFTER it
    /// (so this key wins on conflict). Clamped to the axis; ignored for
    /// non-variable fonts.
    pub(crate) font_weight: Option<u32>,
    /// Moonshot (W9): extra `wght` added on DARK themes only, e.g.
    /// `font_weight_dark_nudge = 50` — light-on-dark text reads thinner than
    /// dark-on-light at equal stroke weight. Applied ONLY when the nudged
    /// instance's `'M'` advance equals the default instance's within 0.25px
    /// AND the cell geometry is unchanged (monospace variable fonts hold
    /// advances constant, which makes this uniquely grid-safe); otherwise
    /// the W2 linear-corrected blend (on by default) remains the weight
    /// compensation. ABSENT/0 = off. Clamped 0..=300.
    pub(crate) font_weight_dark_nudge: Option<f32>,
    /// Aesthetic stem-weight gamma applied to glyph coverage (`< 1.0`
    /// thickens, `> 1.0` thins; clamped 0.30..=3.0). The config alias of the
    /// `ATERM_STEM_GAMMA` env var, which still takes precedence (the usual
    /// env-over-config-over-default order). ABSENT = `1.0` (identity — the
    /// linear-light pipeline needs no correction). Hot-reloadable.
    pub(crate) stem_gamma: Option<f32>,
    /// Native (Linux/Windows) glyph grid-fitting mode (W13/R2): `"full"` (the
    /// autohinter snaps stems in BOTH axes, the crispest grayscale result,
    /// measured side-by-side against `light`/`native`/`off` in the R2
    /// evidence), `"light"` (vertical-only — the desktop `hintslight` look),
    /// `"native"` (the font's own bytecode when it has one), or `"off"` (no
    /// grid fitting — the raw fontdue raster). The config alias of the
    /// `ATERM_FONT_HINTING` env var, which still takes precedence. ABSENT (or
    /// an unrecognized spelling) = `"full"`. LIVE on Linux and Windows; inert
    /// on macOS, where CoreText applies its own grid discipline.
    /// Hot-reloadable (drops the glyph atlas).
    pub(crate) font_hinting: Option<String>,
    /// Linux subpixel-RGB text (RFC-linux-subpixel-text stage 1): `"off"`
    /// (the DEFAULT — grayscale everywhere, byte-identical to before),
    /// `"rgb"` (per-channel LCD coverage on horizontal-RGB panels), or
    /// `"bgr"`. CPU-COMPOSITOR ONLY this stage: the GPU backend renders
    /// grayscale regardless (run with `gpu = false` / `--cpu` / `$ATERM_CPU`
    /// to see it), and the CPU path itself falls back to grayscale under
    /// translucency (`background_opacity < 1`) or a wallpaper, and for
    /// non-primary-family glyphs. The config alias of the
    /// `ATERM_FONT_SUBPIXEL` env var, which still takes precedence. ABSENT
    /// (or an unrecognized spelling) = `"off"`. Inert on macOS (subpixel was
    /// removed OS-wide) and Windows. Hot-reloadable.
    pub(crate) font_subpixel: Option<String>,
    /// Line-height multiplier on the cell BOX (W5a): rows space out (or
    /// tighten) WITHOUT changing the glyph size. Clamped 0.8..=2.0; ABSENT =
    /// `1.0` (byte-identical). The added/removed leading splits half above /
    /// half below the glyph (the leading law). Hot-reloadable (re-grids).
    pub(crate) line_height: Option<f32>,
    /// Baseline escape hatch (W5a): shift every glyph baseline by a signed px
    /// delta, for faces whose vendor metrics still sit visually off after the
    /// half-leading law. Clamped ±32; ABSENT = `0` (pure derivation).
    pub(crate) adjust_baseline: Option<i64>,
    /// Underline position escape hatch (W7): shift the font-table-resolved
    /// underline top by a signed px delta (positive = down). Clamped ±32;
    /// ABSENT = `0` (pure table/heuristic derivation, re-clamped in-cell).
    pub(crate) adjust_underline_position: Option<i64>,
    /// Underline thickness escape hatch (W7): fatten/thin the resolved
    /// underline by a signed px delta. Clamped ±32; ABSENT = `0`.
    pub(crate) adjust_underline_thickness: Option<i64>,
    /// Descender ink-skip (W7): zero underline coverage within a 1px dilation
    /// of the cell's own glyph ink, so g/j/p/q/y descenders are never struck
    /// through. ABSENT = `true` (on — the browser behavior no terminal ships).
    pub(crate) underline_skip_descenders: Option<bool>,
    /// Per-cell minimum WCAG contrast ratio (xterm's `minimumContrastRatio`,
    /// W5b): every glyph fg (and its decorations, combining marks, and the
    /// cursor fill) is floored against the bg it actually sits on. `1.0` (and
    /// ABSENT) = off — byte-identical; clamped 1.0..=21.0. Hot-reloadable.
    pub(crate) minimum_contrast: Option<f32>,
    /// Selected-text foreground `#RRGGBB` (theme `selectionForeground`, W5c):
    /// an explicit ink for selected cells. ABSENT = the WCAG contrast-floor
    /// default (fg floored to 4.5:1 against the selection band).
    pub(crate) selection_foreground: Option<String>,
    /// Dim the selection band while the window is UNFOCUSED (xterm's
    /// `selectionInactiveBackground` behavior, W5c): the band recedes to a
    /// bg-blended tone when focus leaves. ABSENT = `false` (byte-identical:
    /// the band keeps the active colour regardless of focus).
    pub(crate) selection_inactive: Option<bool>,
    /// Break programming ligatures at the CURSOR cell (W5d): the cell under
    /// the cursor renders per-cell so the block cursor never sits on a
    /// multi-column ligature glyph (`LigatureMode::CursorDisabled`). ABSENT =
    /// `false` (ligatures render through the cursor, as before). Ignored when
    /// `ligatures = false` (everything is per-cell already).
    pub(crate) cursor_break_ligatures: Option<bool>,
    /// SGR 1 (bold) promotes indexed colors 0–7 to their bright 8–15 siblings
    /// (W5f). ABSENT = `true` (the classic xterm promotion, byte-identical);
    /// `false` keeps bold a pure weight change.
    pub(crate) bold_is_bright: Option<bool>,
    /// SGR 2 (dim/faint): the fraction of the foreground retained, blended
    /// toward the cell BACKGROUND in linear light (W5e — theme-independent, so
    /// faint recedes on light themes too). Clamped 0.0..=1.0; ABSENT = `0.5`.
    pub(crate) faint_opacity: Option<f32>,
    /// M5 true vibrancy: window BACKGROUND OPACITY (`0.0` = fully transparent
    /// glass … `1.0` = solid). Clamped 0.0..=1.0; ABSENT = `1.0` (solid —
    /// byte-identical). Multiplied into the BACKGROUND-QUAD alpha only (glyph
    /// ink, decorations, and images stay opaque over their cell fills). THE MOVE:
    /// whenever this is `< 1.0` the per-cell minimum-contrast floor auto-engages
    /// at WCAG AA (4.5:1) — see [`Config::effective_minimum_contrast`] — so glass
    /// can never make text illegible. Implemented only by the macOS GPU window
    /// path; every CPU path and non-macOS GPU window keeps a solid grid and is
    /// diagnosed as such. Hot-reloadable.
    pub(crate) background_opacity: Option<f32>,
    /// M5 window MATERIAL — `none` | `hud` | `sidebar` | `under-window` (aliases
    /// `underwindow`/`under_window`, case-insensitive). On macOS GPU this selects
    /// the `NSVisualEffectView` blended behind a translucent background and thus
    /// requires `background_opacity < 1.0`. On Windows GPU it independently maps
    /// to Mica/Acrylic DWM chrome while the grid remains opaque. Other paths have
    /// no material consumer. ABSENT / `none` disables it. Hot-reloadable.
    pub(crate) background_material: Option<String>,
    /// All-edge interior WINDOW PADDING in LOGICAL px (scaled by the display
    /// factor like the built-in default): the breathing room between the window
    /// edge and the grid. ABSENT = `12.0` (the tuned [`crate::PAD_LOGICAL_PX`]
    /// default — byte-identical). Clamped 0..=64 so a typo can't push the grid
    /// off the glass. Hot-reloadable: a reload re-resolves every window's
    /// per-window metrics and re-grids (see [`App::reload_config`]).
    pub(crate) window_padding: Option<f32>,
    /// TOP-edge padding OVERRIDE in logical px, tighter than `window_padding`
    /// (the ~46 pt chrome band already separates the grid from the window's top
    /// edge). ABSENT = `2.0` ([`crate::PAD_TOP_LOGICAL_PX`]). Clamped at the
    /// resolver to `0..=window_padding` — the renderer enforces `pad_top <= pad`
    /// ([`aterm_render::Renderer::set_pad_top`] clamps), so the resolver keeps
    /// the configured value inside the valid domain rather than letting the
    /// clamp silently rewrite it. Hot-reloadable, like `window_padding`.
    pub(crate) window_padding_top: Option<f32>,
    /// Compatibility security opt-in for OSC 52 clipboard queries (`Pd = "?"`).
    /// Default OFF (fail-closed). The GUI callback currently drops every Query
    /// and returns no clipboard contents, so enabling this has no shipping GUI
    /// effect; Manual diagnoses an authored `true` value.
    pub(crate) allow_osc52_query: Option<bool>,
    /// SECURE KEYBOARD ENTRY (`secure_keyboard_entry`, default OFF): while on
    /// AND aterm is frontmost, macOS blocks other processes from observing
    /// this app's keystrokes (`EnableSecureEventInput` — the guard iTerm2
    /// exposes under this name, with the same focus scoping: TN2150 instructs
    /// releasing on deactivation, and `crate::secure_input` gates engagement
    /// on any-window-focused, so a backgrounded aterm never suppresses other
    /// apps' global hotkeys). Process-level, recorded at launch and on every
    /// config commit; macOS-only — Wayland is secure by default, X11 cannot
    /// be secured, and the Settings row says so instead of pretending.
    pub(crate) secure_keyboard_entry: Option<bool>,
    /// Security opt-in for XTWINOPS (`CSI t`). On Linux the GUI installs a
    /// window callback (`spawn::configure_window_ops` → `App::on_window_op`),
    /// so authorized manipulations (iconify, maximize, fullscreen, resize —
    /// move stays denied in-core) reach the winit window; position/pixel
    /// geometry reports beyond the engine's window-title and text-grid-size
    /// fallbacks remain unanswered. Elsewhere no callback is installed and only
    /// those fallback reports work. Default OFF because even title/grid
    /// reports can fingerprint the host.
    pub(crate) allow_window_ops: Option<bool>,
    /// Security opt-in: allow desktop notifications (OSC 9 / 99 / 777). Default
    /// OFF. Maps to `allow_notifications`.
    pub(crate) allow_notifications: Option<bool>,
    /// Security opt-in: allow apps to set indexed colors (OSC 4 / numeric OSC 21).
    /// Default OFF. Maps to `allow_palette_reconfigure`.
    pub(crate) allow_palette_reconfigure: Option<bool>,
    /// Security opt-in: allow Kitty graphics NON-DIRECT transmission mediums to read
    /// host files / shared memory (`t=f` file, `t=t` temp file, `t=s` POSIX shm).
    /// Default OFF (fail-closed) — letting a program make the terminal read arbitrary
    /// user-readable files off an escape sequence is an exfiltration/abuse surface.
    /// When enabled, a size-capped resolver (`spawn::configure_kitty_file_transfer`)
    /// is installed; otherwise non-direct mediums are skipped cleanly.
    pub(crate) allow_kitty_file_transfer: Option<bool>,
    /// Opt-in: run the per-session hydratable temporal recorder (the B.9 spine read
    /// back by the `temporal` control verb). Default OFF — an opt-out session pays no
    /// writer thread and no retention growth. When enabled, each tab seeds a t0
    /// keyframe and taps its reader hot path so the session is time-travel-replayable.
    pub(crate) temporal_recording: Option<bool>,
    /// L3 network drive (`[net]`): the opt-in TLS listener that lets a remote
    /// aterm drive THIS one, plus the saved `[[net.connections]]` this aterm can
    /// dial to drive a remote (the `dial <name>` control verb). Absent ⇒ no
    /// network surface (secure default). See [`NetConfig`] and [`crate::net_listen`]/
    /// [`crate::net_connections`].
    pub(crate) net: Option<NetConfig>,
    /// In-app self-update channel (`[update]`): which GitHub repo the silent updater
    /// pulls notarized releases from. Absent ⇒ the compiled-in default channel
    /// (`alabsystems/aterm`, the PUBLIC mirror — readable with no token, which is
    /// what lets a freshly installed machine update). The env vars `ATERM_UPDATE_OWNER`/`ATERM_UPDATE_REPO`
    /// override these. The location is NOT the trust anchor — the compiled-in pinned
    /// Team ID + Apple notarization are — so repointing the channel cannot get an
    /// untrusted build installed. macOS-only in effect; parsed (and inert) elsewhere.
    /// See [`UpdateConfig`], crate `aterm-update`, and `docs/RELEASING.md`.
    pub(crate) update: Option<UpdateConfig>,
    /// Sparkle words (`[sparkle_words]`): decorate matched profanity words with a
    /// randomized sparkle and cat/kitty words with a cat-paw. The retained Orca
    /// config parses for compatibility but is suspended and has no runtime effect.
    /// Absent ⇒ both live keyword toys are ON; set `enabled = false` to disable.
    /// See [`SparkleWordsConfig`].
    pub(crate) sparkle_words: Option<SparkleWordsConfig>,
    /// PHOSPHOR matrix rain (`[matrix_rain]`): Matrix digital rain UNDER the
    /// text, in empty cells only. Absent ⇒ feature OFF (inverted vs sparkle —
    /// costume mode is opt-in, required by the zero-cost pins); set
    /// `enabled = true` to rain. See [`MatrixRainConfig`].
    pub(crate) matrix_rain: Option<MatrixRainConfig>,
    /// Bundled ALab toolchain manager (`[packages]`, the `atpkg` lane): the
    /// background tools loop's master/auto flags plus the account/channel/
    /// include/exclude/links keys the CO-LOCATED `atpkg` reads out of this SAME
    /// file itself. Absent ⇒ today's behavior (loop on, update-only). See
    /// [`PackagesConfig`].
    pub(crate) packages: Option<PackagesConfig>,
}

/// Source used to produce a terminal's live, human-readable description.
///
/// The config spelling is deliberately closed: a typo is rejected while
/// deserializing `aterm.toml` instead of silently selecting a network service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) enum TitleSummaryProvider {
    /// Fast in-process heuristics; no model, subprocess, or network access.
    #[default]
    #[serde(rename = "builtin")]
    Builtin,
    /// A local Ollama `/api/chat` endpoint.
    #[serde(rename = "ollama")]
    Ollama,
    /// An explicitly configured OpenAI-compatible chat endpoint.
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    /// Do not generate descriptions (the terminal-supplied title still works).
    #[serde(rename = "off")]
    Off,
}

impl TitleSummaryProvider {
    /// Canonical `aterm.toml` token, also used by the Settings selector.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Ollama => "ollama",
            Self::OpenAiCompatible => "openai-compatible",
            Self::Off => "off",
        }
    }
}

/// Proxy behavior for opt-in remote title-summary providers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) enum TitleSummaryProxyMode {
    /// Honor HTTP_PROXY, HTTPS_PROXY, and NO_PROXY from the aterm process.
    #[default]
    #[serde(rename = "environment")]
    Environment,
    /// Connect directly, ignoring proxy environment variables.
    #[serde(rename = "direct")]
    Direct,
}

impl TitleSummaryProxyMode {
    /// Canonical `aterm.toml` token, also used by the Settings selector.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::Direct => "direct",
        }
    }
}

/// Which parts of a tab's identity are shown, and in which order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) enum TitleFormat {
    #[serde(rename = "title")]
    Title,
    #[serde(rename = "description")]
    Description,
    #[default]
    #[serde(rename = "title-description")]
    TitleDescription,
    #[serde(rename = "description-title")]
    DescriptionTitle,
}

impl TitleFormat {
    /// Canonical `aterm.toml` token, also used by the Settings selector.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Description => "description",
            Self::TitleDescription => "title-description",
            Self::DescriptionTitle => "description-title",
        }
    }
}

pub(crate) const DEFAULT_TITLE_SUMMARY_MODEL: &str = "qwen3.5:4b-q4_K_M";
pub(crate) const EXAMPLE_EXPLICIT_OLLAMA_TITLE_SUMMARY_ENDPOINT: &str =
    "http://127.0.0.1:11434/api/chat";
pub(crate) const DEFAULT_TITLE_SUMMARY_INTERVAL_SECONDS: u64 = 15;
pub(crate) const MIN_TITLE_SUMMARY_INTERVAL_SECONDS: u64 = 5;
pub(crate) const MAX_TITLE_SUMMARY_INTERVAL_SECONDS: u64 = 300;
pub(crate) const DEFAULT_TITLE_SUMMARY_CONTEXT_LINES: usize = 24;
pub(crate) const MIN_TITLE_SUMMARY_CONTEXT_LINES: usize = 4;
pub(crate) const MAX_TITLE_SUMMARY_CONTEXT_LINES: usize = 80;
pub(crate) const DEFAULT_TITLE_SUMMARY_TIMEOUT_SECONDS: u64 = 20;
pub(crate) const MIN_TITLE_SUMMARY_TIMEOUT_SECONDS: u64 = 1;
pub(crate) const MAX_TITLE_SUMMARY_TIMEOUT_SECONDS: u64 = 120;

/// Tab Subject & Status policy bounds (RFC §12). Declared `pub(crate)` so
/// `prefs` derives its slider domain from the SAME numbers the resolver clamps
/// to — a Settings save can then never write a value reload would silently
/// rewrite.
pub(crate) const DEFAULT_TAB_STATUS_QUIET_AFTER_MS: u64 = 5_000;
pub(crate) const MIN_TAB_STATUS_QUIET_AFTER_MS: u64 = 500;
pub(crate) const MAX_TAB_STATUS_QUIET_AFTER_MS: u64 = 120_000;
pub(crate) const DEFAULT_TAB_STATUS_DWELL_MS: u64 = 750;
pub(crate) const MIN_TAB_STATUS_DWELL_MS: u64 = 0;
pub(crate) const MAX_TAB_STATUS_DWELL_MS: u64 = 10_000;
/// Nominal observation budget: at most one classification per session per this
/// interval (RFC §10). NOT a config key — the RFC's configuration surface is the
/// four `tab_status*` keys, and an interval the user can raise independently of
/// dwell would just be a way to make dwell unserviceable.
const NOMINAL_TAB_STATUS_OBSERVE_INTERVAL_MS: u64 = 250;
/// Floor on that budget, which `tab_status_dwell_ms = 0` would otherwise take to
/// zero and turn every output burst into a classification.
const MIN_TAB_STATUS_OBSERVE_INTERVAL_MS: u64 = 50;

/// An ordered font list (W6 `fallback_fonts`) that deserializes from EITHER a
/// TOML array of strings (`["A", "B"]`) or a single comma-separated string
/// (`"A, B"` — the form the Settings text editor writes, so a Save round-trips
/// through serde). Entries are trimmed; empties dropped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct FontList(pub(crate) Vec<String>);

impl<'de> serde::Deserialize<'de> for FontList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = FontList;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a list of font names/paths, or a comma-separated string")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<FontList, E> {
                Ok(FontList(
                    s.split(',')
                        .map(str::trim)
                        .filter(|e| !e.is_empty())
                        .map(str::to_string)
                        .collect(),
                ))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<FontList, A::Error> {
                let mut out = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    let t = s.trim();
                    if !t.is_empty() {
                        out.push(t.to_string());
                    }
                }
                Ok(FontList(out))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// The `[sparkle_words]` table. Settings presents two independent keyword toys:
/// Sparkle Words (non-feline word effects) and Keyword Kitties (cat/kitty effects).
/// They are PURELY VISUAL — never affecting copied text, logs, or recorded sessions.
///
/// ```toml
/// [sparkle_words]
/// enabled  = true          # master switch (DEFAULT on; false → byte-identical render)
/// languages = ["en"]       # un-gates ambiguous homographs (fr "chat", de "Kater")
/// suppress_in_alt_screen = false  # DEFAULT off; true → vim/less/htop never decorate
///
/// [sparkle_words.profanity]
/// enabled  = false         # DEFAULT on; set false to keep the strong words off
///
/// [sparkle_words.feline]
/// enabled  = true          # the friendly default
/// ```
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SparkleWordsConfig {
    /// Master switch. Absent ⇒ ON (default); `false` ⇒ no matching, no decorations.
    pub(crate) enabled: Option<bool>,
    /// Languages whose AMBIGUOUS homograph entries are un-gated (`["all"]` = every
    /// language). Non-ambiguous forms always load. Default `["en"]`.
    pub(crate) languages: Option<Vec<String>>,
    /// Force the static, non-twinkling path. Default false. (An OS reduced-motion
    /// query is a future refinement; this is the explicit user override.)
    pub(crate) reduced_motion: Option<bool>,
    /// Suppress decorations on the ALTERNATE screen. Default FALSE — full-screen
    /// TUIs (Claude Code foremost) decorate like the primary screen; `true`
    /// restores the pre-2026-07-04 behavior (vim/less/htop UI text never
    /// decorated — the design §1 knob, hardcoded until now).
    pub(crate) suppress_in_alt_screen: Option<bool>,
    /// External lexicon TOML (`[[entry]]` blocks) merged OVER the builtin. `~` and
    /// `$HOME` are expanded. Absent ⇒ builtin only. Re-read on config reload.
    pub(crate) lexicon: Option<String>,
    /// Strict, versioned Toy Pack manifests. Loaded in list order on
    /// startup/config reload; later packs override earlier packs, and inline
    /// `[[sparkle_words.custom]]` entries override every imported pack.
    /// `~` and `$HOME` expand exactly like `lexicon`.
    pub(crate) toy_packs: Option<Vec<String>>,
    /// Global folded surfaces to never decorate, regardless of category.
    pub(crate) deny: Option<Vec<String>>,
    /// The non-feline Sparkle Words product settings. The historical table name is
    /// retained for config compatibility; its `enabled` key gates profanity,
    /// emphasis, custom/Toy Pack words, and the suspended orca class together.
    pub(crate) profanity: Option<SparkleProfanityConfig>,
    /// The steady feline CAT-PAW sub-table.
    pub(crate) feline: Option<SparkleFelineConfig>,
    /// The typed-word DOG cameo sub-table.
    pub(crate) canine: Option<SparkleCanineConfig>,
    /// Retained compatibility-only Orca sub-table. It parses and round-trips,
    /// but `ORCA_SUSPENDED` makes the whole subtree have no runtime effect.
    pub(crate) orca: Option<SparkleOrcaConfig>,
    /// The animated glyph-ink shimmer sub-table (v2).
    pub(crate) ink: Option<SparkleInkConfig>,
    /// The emphasis / hype-word class sub-table (v2, ink-only).
    pub(crate) emphasis: Option<SparkleEmphasisConfig>,
    /// v3 §6 custom word-effect specs (`[[sparkle_words.custom]]`): per-word
    /// `ink` / `burst` / `graphic` axes, any combination. Words auto-append
    /// to the emphasis class, override class defaults regardless of the
    /// match's class, and bypass per-class enable gates.
    ///
    /// ```toml
    /// [[sparkle_words.custom]]
    /// words = ["ultrathink"]
    /// ink   = { colorway = "rainbow" }   # or "twotone:#RRGGBB,#RRGGBB"
    /// burst = { kind = "starburst", chance = 10 }
    /// graphic = { collection = "cats" }
    /// ```
    pub(crate) custom: Option<Vec<aterm_effects::spec::RawCustomEntry>>,
}

/// Keep config reload work and the combined atlas/spec surface bounded. Excess
/// paths are logged and ignored; every accepted source also carries the strict
/// per-pack byte/recipe/word caps enforced by `aterm-effects`.
const MAX_ACTIVE_TOY_PACKS: usize = 8;

/// Worst-case work for one worker/startup path-feed fingerprint pass: one
/// lexicon, eight Toy Packs, and eight Trail Packs. Each bounded reader may
/// consume one sentinel byte past its parser cap, for at most 17 opens and
/// 2,883,601 bytes read. This is an I/O-volume bound, not a wall-clock promise
/// for an unhealthy filesystem.
#[cfg(test)]
const MAX_PATH_FEED_OPEN_ATTEMPTS: usize = 1 + 2 * MAX_ACTIVE_TOY_PACKS;
#[cfg(test)]
const MAX_PATH_FEED_FINGERPRINT_READ_BYTES: usize =
    (aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES + 1)
        + MAX_ACTIVE_TOY_PACKS
            * ((aterm_effects::spec::MAX_TOY_PACK_BYTES + 1)
                + (aterm_effects::trail_pack::MAX_TRAIL_PACK_BYTES + 1));

#[derive(Default)]
struct LoadedToyPacks {
    spec_table: aterm_effects::spec::SpecTable,
    lexicon_toml: String,
}

type AdmittedPathFeed = (std::path::PathBuf, Result<String, String>);

/// Read one configured effect feed once, then bind both its consumer bytes and
/// its fingerprint to that same admitted handle. The path and readability bits
/// deliberately match the test-only independent fingerprint oracle.
fn read_and_fingerprint_path_feed(
    path: &str,
    max_bytes: usize,
    hasher: &mut std::collections::hash_map::DefaultHasher,
) -> AdmittedPathFeed {
    read_and_fingerprint_path_feed_with_reader(
        path,
        max_bytes,
        hasher,
        aterm_effects::file_feed::read_bounded_regular_utf8,
    )
}

/// Testable one-open core. `FnOnce` makes a second read through the admitted
/// reader unrepresentable; the returned `String` is the sole parser/hash input.
fn read_and_fingerprint_path_feed_with_reader(
    path: &str,
    max_bytes: usize,
    hasher: &mut std::collections::hash_map::DefaultHasher,
    read: impl FnOnce(&std::path::Path, usize) -> std::io::Result<String>,
) -> AdmittedPathFeed {
    use std::hash::Hash as _;

    #[cfg(test)]
    PATH_FEED_READS.with(|count| count.set(count.get().saturating_add(1)));
    path.hash(hasher);
    let expanded = sparkle_expand_tilde(path);
    match read(&expanded, max_bytes) {
        Ok(contents) => {
            true.hash(hasher);
            aterm_effects::file_feed::fingerprint_admitted_utf8(&contents).hash(hasher);
            (expanded, Ok(contents))
        }
        Err(error) => {
            false.hash(hasher);
            (expanded, Err(error.to_string()))
        }
    }
}

/// Immutable word-decoration runtime compiled for one admitted config
/// generation. File reads, Toy Pack compilation, and lexicon construction all
/// happen before this value reaches the event loop; render-time recomputation
/// only clones this memory-backed value through the Serious Mode gate.
#[derive(Clone)]
pub(crate) struct PreparedSparkleRuntime {
    resolved: Option<crate::word_decorations::Resolved>,
}

/// One bounded, internally consistent generation of every path-backed effect
/// feed. Each prepared consumer and its fingerprint derive from the same
/// same-handle admitted bytes, so pathname replacement and ABA edits cannot
/// label stale runtime data as the current generation.
#[derive(Clone)]
pub(crate) struct PreparedPathFeedGeneration {
    pub(crate) sparkle: PreparedSparkleRuntime,
    pub(crate) trail_packs: std::sync::Arc<TrailPackCatalog>,
    pub(crate) fingerprints: PathFeedFps,
}

impl PreparedSparkleRuntime {
    /// Exact custom-spec consumers admitted for this already-prepared runtime
    /// generation. The resolved table includes valid Toy Packs followed by
    /// inline overrides, so Settings can disclose live dependencies without
    /// reopening a manifest or treating an authored path as proof of an
    /// effect shape.
    pub(crate) fn consumer_capabilities(&self) -> aterm_effects::spec::SpecConsumerCapabilities {
        let Some(resolved) = self.resolved.as_ref() else {
            return Default::default();
        };
        let mut consumers = resolved.cfg.spec_table.consumer_capabilities();
        let opts = aterm_lexicon::ScanOptions {
            allow_bare_cat: resolved.cfg.allow_bare_cat,
            cjk_single_char: resolved.cfg.cjk_single_char,
            ignore: (!resolved.cfg.ignore.is_empty()).then_some(&resolved.cfg.ignore),
        };
        consumers.emphasis_class_default = resolved.lexicon.has_scannable_class_surface(
            aterm_lexicon::Class::Emphasis,
            &opts,
            |form_hash| resolved.cfg.spec_table.override_for(form_hash).is_some(),
        );
        consumers
    }
}

#[cfg(test)]
thread_local! {
    static SPARKLE_HOST_PREPARES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PATH_FEED_FINGERPRINTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PATH_FEED_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_sparkle_host_prepare_count() {
    SPARKLE_HOST_PREPARES.with(|count| count.set(0));
}

#[cfg(test)]
fn sparkle_host_prepare_count() -> usize {
    SPARKLE_HOST_PREPARES.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_path_feed_fingerprint_count() {
    PATH_FEED_FINGERPRINTS.with(|count| count.set(0));
}

#[cfg(test)]
fn path_feed_fingerprint_count() -> usize {
    PATH_FEED_FINGERPRINTS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_path_feed_read_count() {
    PATH_FEED_READS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn path_feed_read_count() -> usize {
    PATH_FEED_READS.with(std::cell::Cell::get)
}

/// One immutable, validated Trail Pack catalog for one config generation.
///
/// The versioned config service is the sole production resolver.  It shares this
/// value by `Arc` with the live terminal host and every semantic Settings view,
/// keeping manifest IO, diagnostics, picker ids, and rendered parameters on the
/// exact same revision instead of quietly compiling the same files again.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TrailPackCatalog {
    pub(crate) packs: std::collections::HashMap<String, aterm_effects::cursor_glow::TrailParams>,
    pub(crate) ids: Vec<String>,
    pub(crate) diagnostics: Vec<String>,
}

/// Maximum encoded PNG bytes admitted for one custom kitty cursor sprite.
///
/// The general terminal image decoder deliberately accepts much larger payloads;
/// this config asset is tiny UI chrome and therefore has a tighter independent
/// budget.  The resolver uses a `take(MAX + 1)` read, so a misleading file
/// metadata length can never turn config admission into an unbounded allocation.
pub(crate) const MAX_KITTY_SPRITE_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum width or height of a decoded custom cursor sprite.
pub(crate) const MAX_KITTY_SPRITE_DIMENSION: usize = 1024;
const MAX_KITTY_SOURCE_ID_BYTES: usize = 2 * 1024;
const MAX_KITTY_REASON_BYTES: usize = 320;

/// Bounded read for the configured WALLPAPER image file (a photo is an
/// ordinary source, so this is far above the kitty sprite's 8 MiB).
pub(crate) const MAX_WALLPAPER_FILE_BYTES: usize = 64 * 1024 * 1024;
/// Reject a wallpaper whose DECODED dims exceed this per side (the
/// allocation bound; a 5K/6K screenshot fits comfortably).
pub(crate) const MAX_WALLPAPER_SOURCE_DIMENSION: usize = 8192;
/// Downscale an admitted wallpaper so neither side exceeds this (the resident
/// memory bound — still larger than any window it will be cover-scaled to).
pub(crate) const MAX_WALLPAPER_KEEP_DIMENSION: usize = 4096;

/// The installed-theme directory is ambient user input.  Discovery retains a
/// bounded, deterministic prefix and every individual file is read through a
/// hard byte limit before it can enter a config snapshot.
pub(crate) const MAX_USER_THEME_FILES: usize = 128;
const MAX_USER_THEME_CANDIDATES: usize = 512;
/// Total directory entries inspected before theme discovery fails closed.
pub(crate) const MAX_USER_THEME_DIRECTORY_ENTRIES: usize = 4_096;
pub(crate) const MAX_USER_THEME_FILE_BYTES: usize = aterm_types::scheme::MAX_USER_THEME_FILE_BYTES;
const MAX_THEME_SOURCE_ID_BYTES: usize = 2 * 1024;
const MAX_THEME_REASON_BYTES: usize = 320;

#[derive(Clone, Debug, PartialEq, Eq)]
enum ThemeAssetResolution {
    Ready(aterm_types::ColorScheme),
    Invalid(std::sync::Arc<str>),
}

/// One bounded custom-theme file admitted before the event loop starts.
///
/// Invalid entries are deliberately retained for diagnostics but never appear
/// in the picker.  A configured bad file therefore fails closed in the live
/// renderer and Manual reports the same reason from the same catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ThemeAsset {
    name: String,
    source_id: std::sync::Arc<str>,
    fingerprint: u64,
    resolution: ThemeAssetResolution,
}

/// Parsed custom themes for one immutable config generation.
///
/// Built-ins remain owned by `aterm-types`; this catalog contains only files.
/// Consumers must resolve through [`Self::resolve`] rather than opening the
/// theme directory themselves. The background directory observer parses a new
/// catalog off-thread and the config reducer publishes it as another complete
/// generation, keeping every live surface generation-consistent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ThemeCatalog {
    entries: Vec<ThemeAsset>,
    truncated: bool,
}

/// Live watcher failures that must retain the last admitted catalog. Invalid
/// UTF-8, syntax, names, and oversized files are deterministic asset
/// diagnostics and still produce an `Invalid` entry; these variants are host
/// observation failures where publishing a replacement would make a transient
/// permission/race look like user intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeCatalogWatchError {
    DirectoryUnreadable,
    EntryUnreadable,
    FileUnavailable,
}

impl ThemeCatalog {
    pub(crate) fn empty() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    #[cfg(test)]
    pub(crate) fn from_schemes(
        schemes: impl IntoIterator<Item = (String, aterm_types::ColorScheme)>,
    ) -> std::sync::Arc<Self> {
        let entries = schemes
            .into_iter()
            .map(|(name, scheme)| ThemeAsset {
                fingerprint: stable_asset_fingerprint(0x54, name.as_bytes(), 0, 0, &[]),
                source_id: std::sync::Arc::from(name.clone()),
                name,
                resolution: ThemeAssetResolution::Ready(scheme),
            })
            .collect::<Vec<_>>();
        std::sync::Arc::new(Self {
            entries: normalize_theme_entries(entries),
            truncated: false,
        })
    }

    /// Scan and parse the user theme directory once on the cold startup path.
    pub(crate) fn discover() -> std::sync::Arc<Self> {
        let Some(directory) = aterm_types::scheme::user_theme_dir() else {
            return Self::empty();
        };
        std::sync::Arc::new(Self::discover_in(&directory))
    }

    pub(crate) fn discover_in(directory: &std::path::Path) -> Self {
        Self::discover_in_with(directory, false, || {}).unwrap_or_default()
    }

    /// Fallible counterpart used by the live directory watcher. Startup keeps
    /// its historical best-effort fallback through [`Self::discover_in`], but a
    /// running process must distinguish an actually empty/deleted directory
    /// from a transient permission, mount, or enumeration failure. Publishing
    /// `Default` for the latter would discard the last admitted custom-theme
    /// generation and could silently move the renderer off the selected theme.
    pub(crate) fn try_discover_in(
        directory: &std::path::Path,
    ) -> Result<Self, ThemeCatalogWatchError> {
        Self::discover_in_with(directory, true, || {})
    }

    #[cfg(test)]
    pub(crate) fn try_discover_in_after_scan(
        directory: &std::path::Path,
        after_scan: impl FnOnce(),
    ) -> Result<Self, ThemeCatalogWatchError> {
        Self::discover_in_with(directory, true, after_scan)
    }

    fn discover_in_with(
        directory: &std::path::Path,
        retain_on_file_failure: bool,
        after_scan: impl FnOnce(),
    ) -> Result<Self, ThemeCatalogWatchError> {
        let mut candidates = std::collections::BTreeSet::new();
        let mut truncated = false;
        let read_dir = match std::fs::read_dir(directory) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(_) => return Err(ThemeCatalogWatchError::DirectoryUnreadable),
        };
        let mut observed_entries = 0usize;
        for result in read_dir.take(MAX_USER_THEME_DIRECTORY_ENTRIES + 1) {
            observed_entries += 1;
            if observed_entries > MAX_USER_THEME_DIRECTORY_ENTRIES {
                return Ok(Self {
                    entries: Vec::new(),
                    truncated: true,
                });
            }
            let entry = match result {
                Ok(entry) => entry,
                Err(_) if retain_on_file_failure => {
                    return Err(ThemeCatalogWatchError::EntryUnreadable);
                }
                Err(_) => continue,
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) if retain_on_file_failure => {
                    return Err(ThemeCatalogWatchError::EntryUnreadable);
                }
                Err(_) => continue,
            };
            // `is_file` is false for symlinks: a theme cannot escape the
            // admitted directory through an attacker-swapped link.
            if !file_type.is_file()
                || entry.path().extension() != Some(std::ffi::OsStr::new("conf"))
            {
                continue;
            }
            candidates.insert(entry.path());
            if candidates.len() > MAX_USER_THEME_CANDIDATES {
                candidates.pop_last();
                truncated = true;
            }
        }
        if candidates.len() > MAX_USER_THEME_FILES {
            truncated = true;
        }
        after_scan();

        let mut entries = Vec::new();
        for path in candidates.into_iter().take(MAX_USER_THEME_FILES) {
            let Some(name) = path.file_stem().and_then(std::ffi::OsStr::to_str) else {
                continue;
            };
            // Built-ins always win, so an identically named file is neither
            // advertised nor capable of changing the built-in palette.
            if aterm_types::scheme::builtin(name).is_some() {
                continue;
            }
            let source_id = bounded_asset_text(&path.to_string_lossy(), MAX_THEME_SOURCE_ID_BYTES);
            if let Err(reason) = safe_user_theme_name(name) {
                entries.push(invalid_theme_asset(name, &source_id, reason));
                continue;
            }

            let bytes = match read_bounded_theme_file(&path) {
                Ok(bytes) => bytes,
                Err(ThemeFileReadError::Unavailable(reason)) if retain_on_file_failure => {
                    let _ = reason;
                    return Err(ThemeCatalogWatchError::FileUnavailable);
                }
                Err(
                    ThemeFileReadError::Unavailable(reason) | ThemeFileReadError::Invalid(reason),
                ) => {
                    entries.push(invalid_theme_asset(name, &source_id, &reason));
                    continue;
                }
            };
            let text = match std::str::from_utf8(&bytes) {
                Ok(text) => text,
                Err(_) => {
                    entries.push(invalid_theme_asset(
                        name,
                        &source_id,
                        "theme file is not valid UTF-8",
                    ));
                    continue;
                }
            };
            match aterm_types::scheme::parse_scheme_str(text) {
                Ok(scheme) => entries.push(ThemeAsset {
                    name: name.to_string(),
                    source_id,
                    fingerprint: stable_asset_fingerprint(
                        0x54,
                        path.to_string_lossy().as_bytes(),
                        0,
                        0,
                        &bytes,
                    ),
                    resolution: ThemeAssetResolution::Ready(scheme),
                }),
                Err(error) => {
                    entries.push(invalid_theme_asset(name, &source_id, &error.to_string()))
                }
            }
        }
        Ok(Self {
            entries: normalize_theme_entries(entries),
            truncated,
        })
    }

    pub(crate) fn ready_names(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter_map(|entry| match &entry.resolution {
                ThemeAssetResolution::Ready(_) => Some(entry.name.as_str()),
                ThemeAssetResolution::Invalid(_) => None,
            })
    }

    /// Resolve a built-in or parsed user-theme entry, case-insensitively. No
    /// filesystem access is possible from this API. Case-fold collisions are
    /// rejected during admission, so picker, renderer, engine, and diagnostics
    /// all observe one unambiguous identity for a configured name.
    pub(crate) fn resolve(&self, name: &str) -> Result<aterm_types::ColorScheme, String> {
        if let Some(scheme) = aterm_types::scheme::builtin(name) {
            return Ok(scheme);
        }
        match self
            .entries
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
        {
            Some(ThemeAsset {
                resolution: ThemeAssetResolution::Ready(scheme),
                ..
            }) => Ok(scheme.clone()),
            Some(ThemeAsset {
                source_id,
                resolution: ThemeAssetResolution::Invalid(reason),
                ..
            }) => Err(format!("{} is invalid ({reason})", source_id)),
            None if self.truncated => Err(format!(
                "not present in the bounded {MAX_USER_THEME_FILES}-theme active catalog"
            )),
            None => Err("not found in the active theme catalog".to_string()),
        }
    }

    #[cfg(test)]
    pub(crate) fn fingerprint(&self) -> u64 {
        self.entries.iter().fold(
            if self.truncated {
                0x5452_554E_4341_5445
            } else {
                0x5448_454D_4553
            },
            |fingerprint, entry| fingerprint.rotate_left(7) ^ entry.fingerprint,
        )
    }
}

/// Canonicalize the custom-theme namespace with the same ASCII-insensitive
/// identity used by built-ins, the native picker, and config resolution. A
/// case-sensitive filesystem may contain both `Work.conf` and `work.conf`; it
/// is safer to reject that ambiguous logical name than to let directory order
/// decide which palette a user sees. One invalid sentinel retains the
/// diagnostic while keeping both spellings out of the picker.
fn normalize_theme_entries(mut entries: Vec<ThemeAsset>) -> Vec<ThemeAsset> {
    entries.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    let mut normalized = Vec::with_capacity(entries.len());
    while !entries.is_empty() {
        let end = entries[1..]
            .iter()
            .position(|candidate| !candidate.name.eq_ignore_ascii_case(&entries[0].name))
            .map_or(entries.len(), |offset| 1 + offset);
        if end == 1 {
            normalized.push(entries.remove(0));
            continue;
        }

        let first = &entries[0];
        let names = entries[..end]
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        normalized.push(invalid_theme_asset(
            &first.name,
            &first.source_id,
            &format!("ambiguous theme filenames differ only by ASCII case: {names}"),
        ));
        entries.drain(..end);
    }
    normalized
}

fn invalid_theme_asset(name: &str, source_id: &std::sync::Arc<str>, reason: &str) -> ThemeAsset {
    ThemeAsset {
        name: name.to_string(),
        source_id: std::sync::Arc::clone(source_id),
        fingerprint: stable_asset_fingerprint(0x49, source_id.as_bytes(), 0, 0, reason.as_bytes()),
        resolution: ThemeAssetResolution::Invalid(bounded_asset_text(
            reason,
            MAX_THEME_REASON_BYTES,
        )),
    }
}

fn safe_user_theme_name(name: &str) -> Result<(), &'static str> {
    aterm_types::scheme::validate_user_theme_name(name)
}

enum ThemeFileReadError {
    /// A transient/unsafe host observation. Live reload retains the prior
    /// catalog; startup may still expose the reason as an invalid asset.
    Unavailable(String),
    /// Stable user-authored invalidity that belongs in catalog diagnostics.
    Invalid(String),
}

fn read_bounded_theme_file(path: &std::path::Path) -> Result<Vec<u8>, ThemeFileReadError> {
    let file = open_regular_theme_file(path).map_err(ThemeFileReadError::Unavailable)?;
    let mut bytes = Vec::with_capacity(MAX_USER_THEME_FILE_BYTES.min(16 * 1024));
    file.take((MAX_USER_THEME_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| ThemeFileReadError::Unavailable(format!("unreadable ({error})")))?;
    if bytes.len() > MAX_USER_THEME_FILE_BYTES {
        return Err(ThemeFileReadError::Invalid(format!(
            "larger than the {MAX_USER_THEME_FILE_BYTES}-byte theme limit"
        )));
    }
    Ok(bytes)
}

/// Open one theme candidate without following a link swapped in after the
/// directory scan, then verify the opened handle itself is a regular file.
/// The directory entry check is only a cheap filter; this handle-level check is
/// the security boundary shared by startup parsing and live watcher discovery.
#[cfg(unix)]
pub(crate) fn open_regular_theme_file(path: &std::path::Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("unreadable regular theme file ({error})"))?;
    if !file
        .metadata()
        .map_err(|error| format!("unreadable theme metadata ({error})"))?
        .file_type()
        .is_file()
    {
        return Err("theme source is not a regular file".to_string());
    }
    Ok(file)
}

#[cfg(windows)]
pub(crate) fn open_regular_theme_file(path: &std::path::Path) -> Result<std::fs::File, String> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("unreadable regular theme file ({error})"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("unreadable theme metadata ({error})"))?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err("theme source is not a regular non-reparse file".to_string());
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn open_regular_theme_file(path: &std::path::Path) -> Result<std::fs::File, String> {
    let before = std::fs::symlink_metadata(path)
        .map_err(|error| format!("unreadable theme metadata ({error})"))?;
    if !before.file_type().is_file() {
        return Err("theme source is not a regular file".to_string());
    }
    let file = std::fs::File::open(path)
        .map_err(|error| format!("unreadable regular theme file ({error})"))?;
    if !file
        .metadata()
        .map_err(|error| format!("unreadable theme metadata ({error})"))?
        .file_type()
        .is_file()
    {
        return Err("theme source changed away from a regular file".to_string());
    }
    Ok(file)
}

/// One fully-resolved custom kitty cursor asset for an admitted config generation.
///
/// `Invalid` is intentionally distinct from `BuiltIn`: a bad authored sprite
/// fails closed (the cursor companion is disabled) and remains diagnosable.  It
/// can never silently turn into the built-in homage in app-rendered output while Settings says
/// the custom value is active.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum KittySpriteAsset {
    #[default]
    BuiltIn,
    Ready {
        source_id: std::sync::Arc<str>,
        w: u16,
        h: u16,
        rgba: std::sync::Arc<[u8]>,
        fp: u64,
    },
    Invalid {
        source_id: std::sync::Arc<str>,
        bounded_reason: std::sync::Arc<str>,
    },
}

impl KittySpriteAsset {
    /// Stable paint/install identity.  The variant is part of the identity, so
    /// `Invalid` can never alias `BuiltIn` even when both carry no custom pixels.
    pub(crate) fn fingerprint(&self) -> u64 {
        match self {
            Self::BuiltIn => 0x4E59_414E_5F42_5549,
            Self::Ready { fp, .. } => *fp,
            Self::Invalid {
                source_id,
                bounded_reason,
            } => stable_asset_fingerprint(
                0x49,
                source_id.as_bytes(),
                0,
                0,
                bounded_reason.as_bytes(),
            ),
        }
    }

    pub(crate) fn source_id(&self) -> Option<&str> {
        match self {
            Self::BuiltIn => None,
            Self::Ready { source_id, .. } | Self::Invalid { source_id, .. } => Some(source_id),
        }
    }

    pub(crate) fn diagnostic(&self) -> Option<&str> {
        match self {
            Self::Invalid { bounded_reason, .. } => Some(bounded_reason),
            Self::BuiltIn | Self::Ready { .. } => None,
        }
    }
}

/// The admitted WALLPAPER image for one config revision — the `wallpaper` key
/// resolved once at the same seam as the rainbow kitty sprite, so live
/// rendering, validation, and Settings all see one decode verdict. `Ready`
/// holds straight sRGB RGBA8, already bounded to
/// [`MAX_WALLPAPER_KEEP_DIMENSION`] per side; per-window cover-scaling to the
/// frame happens at splice time against this one source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum WallpaperAsset {
    /// No `wallpaper` key configured — no backdrop, the historical frame.
    #[default]
    None,
    Ready {
        source_id: std::sync::Arc<str>,
        w: u32,
        h: u32,
        rgba: std::sync::Arc<[u8]>,
        fp: u64,
    },
    Invalid {
        source_id: std::sync::Arc<str>,
        bounded_reason: std::sync::Arc<str>,
    },
}

impl WallpaperAsset {
    /// Stable paint identity. The variant is part of the identity, so
    /// `Invalid` can never alias `None` even though both paint no backdrop.
    pub(crate) fn fingerprint(&self) -> u64 {
        match self {
            Self::None => 0x5741_4C4C_5F4F_4646, // "WALL_OFF"
            Self::Ready { fp, .. } => *fp,
            Self::Invalid {
                source_id,
                bounded_reason,
            } => stable_asset_fingerprint(
                0x77,
                source_id.as_bytes(),
                0,
                0,
                bounded_reason.as_bytes(),
            ),
        }
    }

    pub(crate) fn source_id(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Ready { source_id, .. } | Self::Invalid { source_id, .. } => Some(source_id),
        }
    }

    pub(crate) fn diagnostic(&self) -> Option<&str> {
        match self {
            Self::Invalid { bounded_reason, .. } => Some(bounded_reason),
            Self::None | Self::Ready { .. } => None,
        }
    }
}

/// Every non-text config asset admitted at one revision.  `ConfigSnapshot`
/// carries one outer `Arc<ConfigAssetCatalog>` and the live host, capture, and
/// all Settings views clone that exact Arc; there is no independently-resolved
/// Trail/rainbow kitty/theme/sparkle-consumer lane that can lag the text generation.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConfigAssetCatalog {
    pub(crate) trail_packs: std::sync::Arc<TrailPackCatalog>,
    pub(crate) kitty_sprite: KittySpriteAsset,
    /// The admitted wallpaper image (the `wallpaper` key), resolved at the
    /// same revision seam as the kitty sprite.
    pub(crate) wallpaper: WallpaperAsset,
    pub(crate) themes: std::sync::Arc<ThemeCatalog>,
    /// Shared-setting consumers derived from the exact admitted inline + Toy
    /// Pack spec table and prepared lexicon. `Some(default())` is an
    /// authoritative observation that no reachable word consumes shared
    /// tuning. `None` is the real preliminary production generation while Toy
    /// Packs/lexicon are pending worker preparation (and is also used by the
    /// manifest-I/O-free test catalog). Settings presents that state as pending;
    /// it never reopens a pack or infers effect shapes from authored paths.
    pub(crate) sparkle_spec_consumers:
        Option<std::sync::Arc<aterm_effects::spec::SpecConsumerCapabilities>>,
}

impl ConfigAssetCatalog {
    #[cfg(test)]
    pub(crate) fn empty() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            trail_packs: TrailPackCatalog::empty(),
            kitty_sprite: KittySpriteAsset::BuiltIn,
            wallpaper: WallpaperAsset::None,
            themes: ThemeCatalog::empty(),
            sparkle_spec_consumers: None,
        })
    }
}

impl TrailPackCatalog {
    #[cfg(test)]
    pub(crate) fn empty() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    pub(crate) fn get(&self, id: &str) -> Option<aterm_effects::cursor_glow::TrailParams> {
        self.packs.get(id).copied()
    }
}

/// Why a configured trail style cannot emit.  This is shared by live terminal
/// rendering, validation, and Settings previews so an invalid value never turns
/// into the parser's visual `Lumen` fallback in only one of those surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrailStyleIssue {
    Unknown,
    EmptyPackId,
    MissingPack,
}

/// Canonical interpretation of one raw `cursor_trail_style` value against one
/// immutable catalog revision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ResolvedTrailStyle {
    pub(crate) canonical: Option<&'static str>,
    pub(crate) style: Option<aterm_effects::cursor_glow::GlowStyle>,
    pub(crate) pack: Option<aterm_effects::cursor_glow::TrailParams>,
    pub(crate) issue: Option<TrailStyleIssue>,
}

impl ResolvedTrailStyle {
    fn off() -> Self {
        Self {
            canonical: Some("off"),
            style: None,
            pack: None,
            issue: None,
        }
    }
}

/// Renderer-neutral preference inputs for the shared cursor glow resolver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CursorGlowInputs<'a> {
    pub(crate) enabled: bool,
    pub(crate) style_raw: &'a str,
    pub(crate) color: Option<u32>,
    pub(crate) accent: Option<u32>,
    pub(crate) duration_ms: u64,
    pub(crate) length: usize,
    pub(crate) intensity: f32,
    pub(crate) radius: f32,
    pub(crate) ring: bool,
    /// Seconds of recent travel the rainbow kitty typing wake shows (0 ⇒ wake off).
    pub(crate) wake_persist_s: f32,
}

/// `[sparkle_words.ink]` — the animated glyph-ink shimmer (v2): matched words'
/// glyphs recolor through a two-tone gradient with one traveling specular sweep,
/// then settle to constant bytes. Applies to emphasis + profanity + feline
/// (orca untouched); takes effect only when `sparkle_words.enabled` is on.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SparkleInkConfig {
    /// Emit ink at all. Default TRUE (under the master switch).
    pub(crate) enabled: Option<bool>,
    /// Ink tint vs the original fg. Default 0.75, clamped 0.0..=1.0.
    pub(crate) strength: Option<f32>,
    /// One specular sweep window, ms. Default 2200, clamped 350..=6000
    /// (`loop = true` raises the floor to 600 — the §6.4 flash margin).
    pub(crate) sweep_ms: Option<u32>,
    /// Re-sweep while the word stays visible (keeps focused wakes live).
    /// Default false. (`loop` is a Rust keyword, hence the rename.)
    #[serde(rename = "loop")]
    pub(crate) loop_: Option<bool>,
}

/// `[sparkle_words.emphasis]` — hype words, the ink-only 4th lexicon class:
/// no sprite, just the animated ink. Ships EMPTY — the builtin lexicon has no
/// emphasis forms; `extra_words` is the sole population mechanism.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SparkleEmphasisConfig {
    /// Decorate emphasis words. Default TRUE (takes effect only when the master
    /// AND `sparkle_words.ink` are on — the class has no non-ink surface).
    pub(crate) enabled: Option<bool>,
    /// Extra whole words to treat as emphasis (added to the lexicon).
    pub(crate) extra_words: Option<Vec<String>>,
    /// Folded surfaces to never decorate as emphasis.
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// `[sparkle_words.profanity]` — the randomized, self-terminating sparkle.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SparkleProfanityConfig {
    /// Enable the non-feline Sparkle Words toy. Default TRUE. This historical
    /// location remains stable, but the switch also gates emphasis, custom/Toy
    /// Pack words, and the suspended orca class so the Top Settings toggle is an
    /// honest product-level off switch. Keyword Kitties remains independent.
    pub(crate) enabled: Option<bool>,
    /// `"rainbow"` (the v3 default: animated rainbow ink, `supernova_chance`%
    /// of episodes escalate to the FUCK SUPER NOVA) | `"nova"` = the v2
    /// classic nova (dip → flash → ring + rays → debris → ember) |
    /// `"sparkle"` = the exact v1 randomized sparkle. Unknown values fall
    /// back to `"rainbow"`.
    pub(crate) style: Option<String>,
    /// v3 §3.2: FUCK SUPER NOVA escalation chance, percent. Default 10,
    /// clamped 0..=100 (0 disables). Consulted only under
    /// `style = "rainbow"` (documented — nova/sparkle ignore it).
    pub(crate) supernova_chance: Option<u32>,
    /// Quasar (1/512) / Singularity (1/1024) rare nova variants (§3.5/§6.1).
    /// Default TRUE.
    pub(crate) magic: Option<bool>,
    /// Sparkle tints (`#RRGGBB`); empty ⇒ a lively hue rotation. Default empty.
    pub(crate) palette: Option<Vec<String>>,
    /// Sparks emitted per word per frame. Default 3, clamped 1..=12.
    pub(crate) density: Option<u32>,
    /// How long a word sparkles after appearing, ms. Default 2500, clamped 350..=10000.
    pub(crate) anim_ms: Option<u64>,
    /// Sub-cell jitter in px. Default 2, clamped 0..=6.
    pub(crate) jitter: Option<i8>,
    /// Opacity 0.0..=1.0. Default 0.85.
    pub(crate) intensity: Option<f32>,
    /// Play the discordant curse BONK when a profanity word is TYPED at the
    /// live caret (the cursor-audio framework's sparkle-words gesture — a
    /// minor-second/tritone clash against the trail melody's current degree).
    /// Default TRUE, riding the same on-by-default product choice as the
    /// sparkles and `trail_sounds`; it is silenced with them by focus loss,
    /// reduced motion, zero `trail_sound_volume`, or `enabled = false` here.
    /// HOST-side gate only, the feline `log` precedent: the effects engine
    /// always records its bounded cues and the App drains-and-drops when off.
    pub(crate) bonk: Option<bool>,
    /// ALSO bonk when an on-screen curse's supernova ignites (the detonation
    /// edge — fires for `cat` output and scrollback redraws too, since screen
    /// content detonates regardless of who typed it). Default FALSE so the
    /// bonk stays typed-provenance-only unless explicitly opted in;
    /// rate-limited by the §6.4 flash limiter when on.
    pub(crate) bonk_detonation: Option<bool>,
    /// Extra whole words to treat as profanity (added to the lexicon).
    pub(crate) extra_words: Option<Vec<String>>,
    /// Folded surfaces to never decorate as profanity.
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// `[sparkle_words.feline]` — the Keyword Kitties product.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SparkleFelineConfig {
    /// Decorate cat/kitty words. Default TRUE (takes effect only when master on).
    pub(crate) enabled: Option<bool>,
    /// `"cat"` (default) is the only graphic style. `"paw"` remains a legacy
    /// compatibility value and selects ink-only rendering; cat-art v4 emits no
    /// paw graphic. Unknown values fall back to `"cat"`.
    pub(crate) style: Option<String>,
    /// Retired idle-animation key. Parsed and preserved for compatibility, but
    /// cat-art v4 has no idle scheduler and this value has no effect.
    pub(crate) idle: Option<bool>,
    /// Retired gaze key. Parsed and preserved for compatibility, but v4 eyes
    /// are authored into the sprite and this value has no effect.
    pub(crate) gaze: Option<bool>,
    /// Fortune (1/512) / Nebula (1/1024) rare cats (§3.5/§5.4). Default TRUE.
    pub(crate) magic: Option<bool>,
    /// Retired paw-tint key. Parsed and preserved; no v4 graphic consumes it.
    pub(crate) color: Option<String>,
    /// Retired feline-opacity key. Parsed and preserved; no v4 graphic consumes it.
    pub(crate) intensity: Option<f32>,
    /// Decorate the bare 3-letter `cat` token (also the shell command). Default true.
    pub(crate) allow_bare_cat: Option<bool>,
    /// Decorate a lone CJK cat ideograph (`猫`) anywhere. Default false (high FP).
    pub(crate) cjk_single_char: Option<bool>,
    /// Record sightings into the durable, machine-owned Kitty Log
    /// (`kitty-log.toml`, §F4). Default TRUE. HOST-side gate only: the
    /// effects engine always records (bounded per tick) and the App drains-
    /// and-drops when this is false — nothing is counted or written.
    pub(crate) log: Option<bool>,
    /// Extra whole words to treat as feline (added to the lexicon).
    pub(crate) extra_words: Option<Vec<String>>,
    /// Folded surfaces to never decorate as feline.
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// `[sparkle_words.canine]` — the typed-word DOG cameo. Unlike the ambient
/// families this class draws nothing from the screen scanner; its `enabled`
/// gates the input-path summon (typing `dog`/`puppy`/… after a lot of typing),
/// and its word lists ride the same lexicon override as every other class.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SparkleCanineConfig {
    /// Summon dogs for typed canine words. Default TRUE (takes effect only
    /// when the sparkle master is on).
    pub(crate) enabled: Option<bool>,
    /// Extra whole words to treat as canine (added to the lexicon).
    pub(crate) extra_words: Option<Vec<String>>,
    /// Folded surfaces to never treat as canine.
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// Compatibility shape for the suspended `[sparkle_words.orca]` feature. These
/// fields remain deserializable so existing files survive unchanged; the
/// runtime hard-gates the entire subtree with `ORCA_SUSPENDED`.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SparkleOrcaConfig {
    /// Historical enable bit. Parsed only; currently has no effect.
    pub(crate) enabled: Option<bool>,
    /// Historical extra words. Parsed only; currently has no effect.
    pub(crate) extra_words: Option<Vec<String>>,
    /// Historical deny words. Parsed only; currently has no effect.
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// The `[matrix_rain]` table (PHOSPHOR, design §12). OFF BY DEFAULT — the
/// inverse of `[sparkle_words]`. Every field is optional; defaults + clamps
/// live ONLY in [`Config::matrix_rain_params`] (the enabled bit resolves
/// separately in [`Config::matrix_rain_enabled`]), so an absent key and a
/// default-valued key are indistinguishable. PURELY VISUAL — the grid is
/// never mutated; copy/selection/search/recordings read exact bytes.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct MatrixRainConfig {
    /// Master switch. Absent ⇒ OFF (costume mode is opt-in).
    pub(crate) enabled: Option<bool>,
    /// WORKING tick rate, clamped 12..=60 (CALM always runs 12 Hz).
    pub(crate) fps: Option<u32>,
    /// Column density, clamped 1..=12.
    pub(crate) density: Option<u32>,
    /// Fall speed, clamped 1..=10 (5 = neutral).
    pub(crate) speed: Option<u32>,
    /// Trail length, clamped 1..=10 (5 = neutral).
    pub(crate) trail: Option<u32>,
    /// Body coverage, clamped 16..=135 (READABLE_ALPHA_CAP). Absent ⇒ derived
    /// from the theme under the §6 luminance constraint.
    pub(crate) alpha: Option<u32>,
    /// Bright-head coverage, clamped `alpha..=135`. Absent ⇒ derived.
    pub(crate) head_alpha: Option<u32>,
    /// `"matrix"` | `"theme"` | `"#RRGGBB"`. Malformed hex fails CLOSED to
    /// the stock matrix green (never aborts the config load).
    pub(crate) hue: Option<String>,
    /// Glyph mutation window in ms, clamped 80..=2000.
    pub(crate) mutation_ms: Option<u64>,
    /// Idle seconds until the mandatory drain, clamped 2..=120. There is no
    /// `idle = "keep"` — no configuration animates forever (design §5).
    pub(crate) idle_secs: Option<u64>,
    /// Suppress emission on the alternate screen. Default FALSE (design §7:
    /// rain shows in fullscreen TUIs out of the box; the master switch is
    /// already opt-in).
    pub(crate) suppress_in_alt_screen: Option<bool>,
    /// v1: parsed but inert — the materialize sweep is deferred (design §14).
    pub(crate) materialize: Option<bool>,
    /// OUTPUT MATERIAL BANK (v1.1): the rain's alphabet is rasterized literally
    /// from the program's real on-screen output (case/digits/punctuation and
    /// supported Unicode retain their codepoint shapes), outside the current
    /// cursor/composer protection band.
    /// Default TRUE; `false` keeps the classic pure-kana field.
    pub(crate) output_material: Option<bool>,
    /// Turn-complete head sweep on the WORKING→CALM edge. Default true.
    pub(crate) turn_wave: Option<bool>,
    /// Visual bell → 2 s constant-luminance amber hue ramp. Default true.
    pub(crate) bell_alert: Option<bool>,
    /// Command EXIT STATUS in the weather (OSC 133/633 shell integration):
    /// success fires the finishing head-sweep, failure holds a 2 s ember
    /// tint — glanceable pass/fail without reading text. Default true.
    pub(crate) exit_tint: Option<bool>,
    /// v1: parsed but inert — occupied-cell recolour is v1.1 (design §14).
    pub(crate) ink_text: Option<bool>,
    /// v1: parsed but inert — the GPU luxe layer is deferred (design §14).
    pub(crate) phosphor: Option<bool>,
    /// Field seed. 0 (default) ⇒ a stable per-window seed is derived at
    /// engine build; nonzero ⇒ reproducible (demos/tests).
    pub(crate) seed: Option<u64>,
}

/// The `[packages]` table (the bundled-ALab-toolchain `atpkg` lane,
/// `docs/TOOLCHAIN-PACKAGE-MANAGER.md` §11). Every field is optional;
/// defaults live ONLY in the resolver ([`Config::packages_update_loop_enabled`])
/// — an absent table is exactly today's behavior. The GUI consumes ONLY the
/// loop-gate flags; the account/channel/include/exclude keys (and the
/// `[packages.links]` sub-table, which this struct deliberately does not
/// declare — serde tolerates unknown keys) are read by the co-located `atpkg`
/// binary from this SAME file, so there is one config surface and no GUI copy
/// to go stale. Env always wins on the atpkg side (`ATPKG_ACCOUNT` etc.); the
/// loop interval keeps its own `ATPKG_UPDATE_INTERVAL_SECS` env override.
///
/// ```toml
/// [packages]
/// # enabled      = true    # master for the background tools loop (default true)
/// # auto_update  = true    # run `atpkg update` on the 6h cadence (default true)
/// # auto_install = false   # ALSO install missing default-set members (default
/// #                        # FALSE — multi-GB toolchains need explicit consent)
/// # seed_install = true    # install the BUNDLED seed registry on first launch
/// #                        # (default TRUE — the bytes ship inside the app;
/// #                        # false = announce an offer instead). atpkg-only key.
/// # account      = "alabsystems"   # index owner override (default = compiled owner)
/// # channel      = "stable"
/// # include      = ["ay"]  # narrowing-only filters over the signed index set
/// # exclude      = ["trust"]
/// [packages.links]
/// # ay  = "~/ay"              # local checkout -> managed dev-link (registry skipped)
/// # orc = "alabsystems/orc"   # private-repo fetch override (signatures unchanged)
/// ```
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct PackagesConfig {
    /// Master for the background tools loop. Absent ⇒ ON (today's behavior).
    pub(crate) enabled: Option<bool>,
    /// Run `atpkg update` on the background cadence. Absent ⇒ ON.
    pub(crate) auto_update: Option<bool>,
    /// ALSO install missing index default-set members on the update pass.
    /// Absent ⇒ OFF (consent-gated — the Settings switch is the consent click).
    /// Consumed by ATPKG (its own reader), not by the GUI loop gate.
    pub(crate) auto_install: Option<bool>,
    /// `[packages].seed_install`: install the BUNDLED toolchain seal on first launch.
    /// Default TRUE (the bytes ship inside the app, so installing the app is the
    /// consent). Consumed by atpkg, mirrored here so Settings can offer the switch —
    /// the docs pointed at this key while the only way to set it was hand-editing a
    /// file a new user does not have yet, which made the documented opt-out
    /// unreachable by exactly the person it exists for.
    pub(crate) seed_install: Option<bool>,
    /// Index owner override — consumed by atpkg (`ATPKG_ACCOUNT` env beats it).
    pub(crate) account: Option<String>,
    /// Channel — consumed by atpkg (default "stable" there).
    pub(crate) channel: Option<String>,
    /// Narrowing-only include filter — consumed by atpkg.
    pub(crate) include: Option<Vec<String>>,
    /// Narrowing-only exclude filter — consumed by atpkg.
    pub(crate) exclude: Option<Vec<String>>,
}

/// The `[net]` table: the inbound listener settings (persisting what was
/// `ATERM_NET_LISTEN`/`_CERT`/`_KEY`) and the outbound `[[net.connections]]`
/// registry. Every field optional; an empty/absent table is the secure default
/// (no port bound, nothing to dial). Env vars still OVERRIDE the listener fields,
/// and the listener binds ONLY in a top-level aterm (never one launched inside
/// another aterm — see [`crate::net_listen`]).
///
/// ```toml
/// [net]
/// # Inbound: let a remote aterm drive THIS one (omit `listen` to keep it off).
/// listen = "0.0.0.0:7100"
/// cert   = "~/.config/aterm/net/server.der"        # operator cert (DER)
/// key    = "~/.config/aterm/net/server.key.der"    # PKCS#8 key (DER)
///
/// # Outbound: a remote you can `dial work-box` to drive.
/// [[net.connections]]
/// name        = "work-box"
/// host        = "work.example:7100"
/// fingerprint = "sha256:ab12…"                     # the remote cert's SHA-256
/// ```
/// Provision the drive token once with `aterm-ctl dial-token work-box <token>`
/// (macOS Keychain, else a 0600 file); then `aterm-ctl dial work-box`. The `<token>`
/// is the PERSISTENT `network-drive capability token` the remote prints at listener
/// startup (stored beside its TLS key) — it survives restarts, so this is a
/// one-time setup, NOT re-copied after every remote restart.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct NetConfig {
    /// Inbound numeric `IP:port` bind address for the TLS listener, e.g.
    /// `"0.0.0.0:7100"` or `"[::1]:7100"`. Hostnames are deliberately rejected
    /// so startup and Manual validation never block on DNS. Absent ⇒ listener
    /// OFF. (`ATERM_NET_LISTEN` overrides.)
    pub(crate) listen: Option<String>,
    /// Path to the operator's server certificate (DER). (`ATERM_NET_CERT` overrides.)
    pub(crate) cert: Option<String>,
    /// Path to the server private key (PKCS#8 DER). (`ATERM_NET_KEY` overrides.)
    pub(crate) key: Option<String>,
    /// Saved remote endpoints this aterm can `dial <name>` to drive.
    pub(crate) connections: Vec<Connection>,
}

/// One saved remote endpoint in `[[net.connections]]`. The drive TOKEN is NOT
/// stored here — it lives in the macOS Keychain (`aterm-net-drive`/`<name>`) or a
/// referenced 0600 `token_file` on other platforms (see
/// [`crate::net_connections::resolve_token`]).
///
/// The OPTIONAL `sid`/`expect_nonce` fields are the session rebind pin. Both default
/// to absent, in which case the dial path is byte-identical to an un-pinned dial
/// (only the TLS cert `fingerprint` is enforced). When `expect_nonce` is set, the
/// dialer enforces [`aterm_net::RemoteEndpoint::matches`] BEFORE relaying — a
/// relaunched/rebound remote session (fresh launch nonce) is refused. Because the
/// shipping wire protocol does not yet echo the remote's launch identity, a set
/// `expect_nonce` currently FAILS CLOSED (refuses to dial) rather than relay
/// unverified; see [`crate::net_connections::dial_relay`].
#[derive(Clone, PartialEq, serde::Deserialize)]
pub(crate) struct Connection {
    /// The name you `dial` (e.g. `"work-box"`). Unique within the registry.
    pub(crate) name: String,
    /// `host:port` of the remote's TLS listener (e.g. `"work.example:7100"`).
    pub(crate) host: String,
    /// The remote cert's SHA-256 fingerprint to pin, hex (optionally `sha256:`-prefixed).
    pub(crate) fingerprint: String,
    /// Non-macOS (or macOS fallback): path to a 0600 file holding the drive token
    /// hex. On macOS the Keychain is tried first.
    pub(crate) token_file: Option<String>,
    /// OPTIONAL: the remote session id this pin records. Carried for the endpoint
    /// record but NOT part of the rebind check (which is nonce-only); absent ⇒ unset.
    #[serde(default)]
    pub(crate) sid: Option<String>,
    /// OPTIONAL: the remote session's launch NONCE to pin (rebind guard). Absent ⇒
    /// no rebind check (un-pinned, byte-identical dial). Present ⇒ the dialer
    /// enforces it before relaying and fails closed if it cannot be verified.
    #[serde(default)]
    pub(crate) expect_nonce: Option<String>,
}

/// The `[update]` table: where the in-app self-updater pulls releases from. Both
/// fields optional; an absent table (or field) uses the compiled-in default
/// channel (`alabsystems/aterm`, the public mirror). Resolution precedence is env > this config > default,
/// applied by [`aterm_update::Source::resolve`] (the env vars are
/// `ATERM_UPDATE_OWNER` / `ATERM_UPDATE_REPO`).
///
/// ```toml
/// [update]
/// owner = "my-org"     # github.com/<owner>/<repo>
/// repo  = "aterm"
/// ```
///
/// This only chooses the SOURCE of the bytes. Authenticity is anchored by the
/// pinned Team ID compiled into the binary plus Apple notarization, so a repointed
/// (or hijacked) source still cannot install an untrusted bundle — it just fails
/// verification and nothing is staged.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct UpdateConfig {
    /// GitHub owner (the `OWNER` in `github.com/OWNER/REPO`). Absent ⇒ default
    /// (`alabsystems`). `ATERM_UPDATE_OWNER` overrides.
    pub(crate) owner: Option<String>,
    /// GitHub repository name. Absent ⇒ default (`aterm`). `ATERM_UPDATE_REPO` overrides.
    pub(crate) repo: Option<String>,
    /// Apply a freshly-staged update IMMEDIATELY (default ON): the seamless
    /// handoff carries every window/tab/split — shells, screens, layout — across
    /// the re-exec, so there is no reason to sit on a staged build (the owner:
    /// "no passive scheduler; I want immediate"). `false` restores the old
    /// stage-and-wait behavior (apply on click / next launch).
    /// `ATERM_NO_AUTO_APPLY` forces it off for one run.
    pub(crate) auto_apply: Option<bool>,
    /// OPT IN to Apple Developer-ID + notarization enforcement for self-updates.
    ///
    /// Shipped aterm compiles the real Team ID in (Tier APPLE armed 2026-08-15),
    /// which always wins; this setting matters only to forks/self-hosted builds
    /// with an empty compiled pin, where absent means the structural
    /// `codesign --verify` only. That is what lets an unsigned build update, and
    /// it is deliberate —
    /// authenticity in that tier comes from the channel plus the manifest SHA-256
    /// (and the Ed25519 manifest signature when one is configured).
    ///
    /// Set it to a Team ID once you actually sign releases with a Developer ID and
    /// the updater will additionally require every staged bundle to be signed by
    /// that team AND accepted by Gatekeeper.
    ///
    /// ```toml
    /// [update]
    /// require_team_id = "ABCDE12345"
    /// ```
    ///
    /// It can only TIGHTEN: a build that already has a Team ID compiled in ignores
    /// this key entirely, so a config file can never downgrade a signed build's
    /// trust anchor. See `aterm_update::set_required_team_id`.
    pub(crate) require_team_id: Option<String>,
}

/// Whether a staged update applies immediately (config `update.auto_apply`,
/// default TRUE; env `ATERM_NO_AUTO_APPLY` vetoes for a run — the same env-wins
/// precedence as the other update settings).
pub(crate) fn update_auto_apply(config: &Config) -> bool {
    if std::env::var_os("ATERM_NO_AUTO_APPLY").is_some() {
        return false;
    }
    config
        .update
        .as_ref()
        .and_then(|u| u.auto_apply)
        .unwrap_or(true)
}

/// Default rows reserved for the in-grid tab strip, PER PLATFORM. On macOS this is
/// `0` — tabs live in the native window toolbar (toolbar.rs), so an in-terminal
/// frame would be redundant. On every other platform (Linux/X11) the native toolbar
/// is a no-op ([`crate::platform::AppRtLinux`]), so the in-grid strip is the ONLY
/// tab UI: it defaults to `1` row, otherwise a second/third tab is completely
/// invisible and un-switchable by mouse. Override either way with config
/// `tab_strip_rows = N` or `ATERM_TAB_STRIP_ROWS`.
#[cfg(target_os = "macos")]
pub(crate) const DEFAULT_TAB_STRIP_ROWS: u16 = 0;
/// See the macOS variant above — non-macOS defaults the in-grid strip ON.
#[cfg(not(target_os = "macos"))]
pub(crate) const DEFAULT_TAB_STRIP_ROWS: u16 = 1;
/// Upper clamp on `tab_strip_rows` so a mis-set config can't starve the terminal.
pub(crate) const MAX_TAB_STRIP_ROWS: u16 = 4;

/// Resolve the configured tab-strip row count (env `ATERM_TAB_STRIP_ROWS` wins, then
/// config, then [`DEFAULT_TAB_STRIP_ROWS`]), clamped to `0..=MAX_TAB_STRIP_ROWS`.
/// Env precedence mirrors the other window settings (env > config > default).
pub(crate) fn resolve_tab_strip_rows(config: &Config) -> u16 {
    let raw = std::env::var("ATERM_TAB_STRIP_ROWS")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .or(config.tab_strip_rows)
        .unwrap_or(DEFAULT_TAB_STRIP_ROWS);
    raw.min(MAX_TAB_STRIP_ROWS)
}

/// Resolve the ⌘F / socket `search` index depth cap from `search_history_lines`: how
/// many of the newest addressable lines the trigram index retains (the rest of history
/// is reported as a partial result). Unset → the engine's
/// [`DEFAULT_MAX_CACHED_LINES`](aterm_core::search::DEFAULT_MAX_CACHED_LINES); the value
/// is pushed to [`crate::control::set_search_max_lines`] at startup and on reload.
pub(crate) fn search_index_depth(config: &Config) -> usize {
    config
        .search_history_lines
        .map_or(aterm_core::search::DEFAULT_MAX_CACHED_LINES, |n| n as usize)
}

/// Resolve a raw plain or appearance-split theme selector with the runtime's
/// deliberately permissive compatibility semantics. Existing hand-authored
/// files may contain an unrecognized segment beside a valid `dark:`/`light:`
/// side; the recognized side still applies while an omitted side is Default.
/// Renderer config and Settings preview share this helper so diagnostics can
/// flag malformed bytes without inventing different effective pixels.
pub(crate) fn resolve_theme_name_value(
    raw: Option<&str>,
    appearance: aterm_types::Appearance,
) -> Option<String> {
    let raw = raw?;
    // A "split" is any comma-segment whose key (before ':') is dark|light.
    let is_split = raw.split(',').any(|seg| {
        seg.split_once(':').is_some_and(|(k, _)| {
            matches!(k.trim().to_ascii_lowercase().as_str(), "dark" | "light")
        })
    });
    if !is_split {
        return Some(raw.trim().to_string());
    }
    let want = match appearance {
        aterm_types::Appearance::Light => "light",
        aterm_types::Appearance::Dark => "dark",
    };
    for seg in raw.split(',') {
        if let Some((key, name)) = seg.split_once(':')
            && key.trim().eq_ignore_ascii_case(want)
            && !name.trim().is_empty()
        {
            return Some(name.trim().to_string());
        }
    }
    None
}

impl Config {
    /// Resolve the scheme NAME this config selects for `appearance`, honouring the
    /// optional OS-appearance SPLIT `theme = "dark:<name>,light:<name>"`.
    ///
    /// A plain value with no `dark:`/`light:` prefix is the single theme for BOTH
    /// appearances (unchanged behavior). In the split form the segment matching
    /// `appearance` wins; an omitted side resolves to `None` (the built-in Default).
    /// Keys and the surrounding whitespace are case/space-insensitive; the theme
    /// NAME keeps its original case (so `light:GitHub Light` resolves correctly).
    pub(crate) fn resolve_theme_name(&self, appearance: aterm_types::Appearance) -> Option<String> {
        resolve_theme_name_value(self.theme.as_deref(), appearance)
    }

    /// Resolve the BASE color scheme this config selects for `appearance` (see
    /// [`Self::resolve_theme_name`]): the named built-in (case-insensitive), a user
    /// theme FILE of that name, or the built-in [`aterm_types::ColorScheme::default`]
    /// when no theme — or an unresolvable / malformed one — is set. The per-key color
    /// overrides (`foreground`/…/`palette`) are layered ON TOP of this base by the
    /// callers, so they always win.
    fn base_scheme_for_with_themes(
        &self,
        appearance: aterm_types::Appearance,
        themes: &ThemeCatalog,
    ) -> aterm_types::ColorScheme {
        // Resolves SILENTLY (unresolvable/malformed name → Default): both renderer
        // and engine projections call this. The single diagnostic is emitted by
        // `terminal_config_for_with_assets` from this exact admitted catalog.
        match self.resolve_theme_name(appearance) {
            None => aterm_types::ColorScheme::default(),
            Some(name) => themes.resolve(&name).unwrap_or_default(),
        }
    }

    /// The RENDERER theme (window clear colour, cursor, selection highlight). Starts
    /// from the selected scheme's chrome ([`Self::base_scheme_for`]); the per-key color
    /// keys then override individual slots (unchanged precedence) so the window CLEAR
    /// colour matches a configured `background` and `selection_color` themes the
    /// highlight.
    #[cfg(test)]
    pub(crate) fn theme(&self) -> Theme {
        self.theme_for(aterm_types::Appearance::Dark)
    }

    /// [`Self::theme`] resolved for a specific OS `appearance` — drives the live
    /// light↔dark scheme switch (see [`Self::resolve_theme_name`]).
    #[cfg(test)]
    pub(crate) fn theme_for(&self, appearance: aterm_types::Appearance) -> Theme {
        self.theme_for_with_assets(appearance, &ThemeCatalog::default())
    }

    /// Renderer theme resolved exclusively from the immutable custom-theme
    /// catalog carried by the active config snapshot.
    pub(crate) fn theme_for_with_assets(
        &self,
        appearance: aterm_types::Appearance,
        themes: &ThemeCatalog,
    ) -> Theme {
        let tp = self
            .base_scheme_for_with_themes(appearance, themes)
            .to_theme_parts();
        let mut t = Theme {
            fg: tp.fg,
            bg: tp.bg,
            cursor: tp.cursor,
            selection: tp.selection,
        };
        let u = |c: Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
        if let Some(c) = self.foreground.as_deref().and_then(parse_hex_color) {
            t.fg = u(c);
        }
        if let Some(c) = self.background.as_deref().and_then(parse_hex_color) {
            t.bg = u(c);
        }
        if let Some(c) = self.cursor_color.as_deref().and_then(parse_hex_color) {
            t.cursor = u(c);
        }
        if let Some(c) = self.selection_color.as_deref().and_then(parse_hex_color) {
            t.selection = u(c);
        }
        t
    }

    /// Whether Alt/Option should send ESC-prefixed (Meta) sequences. The
    /// cross-platform DEFAULT is `true`, matching the engine encoder. Setting
    /// `option_as_meta = false` forwards OS-composed text when one is available;
    /// non-text Alt chords continue through the ordinary terminal encoder.
    pub(crate) fn option_as_meta_or_default(&self) -> bool {
        self.option_as_meta.unwrap_or(true)
    }

    /// Whether a multi-line paste is confirmed when bracketed paste is off. DEFAULT
    /// when absent is `true` (safe): the modern shell default (bracketed paste) skips
    /// the check, so the dialog only ever appears for the genuinely-risky bare-prompt /
    /// REPL case. Setting `confirm_multiline_paste = false` disables it.
    pub(crate) fn confirm_multiline_paste_or_default(&self) -> bool {
        self.confirm_multiline_paste.unwrap_or(true)
    }

    /// Whether the previous graceful quit's windows/tabs/panes (+ per-pane cwd) are
    /// reopened at launch (RESTORE-1). DEFAULT when absent is `true` — batteries-on,
    /// the macOS-Terminal/iTerm expectation. `restore_session = false` opts out.
    pub(crate) fn restore_session_or_default(&self) -> bool {
        self.restore_session.unwrap_or(true)
    }

    /// Whether a completed mouse selection auto-copies to the system CLIPBOARD.
    /// DEFAULT when absent is `true` off Linux — the copy-on-select convenience,
    /// flipped on with the other visual/UX defaults; `copy_on_select = false`
    /// opts out (the ghostty/macOS explicit-copy behaviour). On LINUX the
    /// default is `false`: a selection already owns the X11 PRIMARY buffer
    /// unconditionally, and the platform convention keeps the CLIPBOARD for
    /// explicit copies only — defaulting this on made every drag clobber the
    /// user's Ctrl+Shift+C copy (audit finding). An explicit
    /// `copy_on_select = true` still opts into writing both buffers.
    pub(crate) fn copy_on_select_or_default(&self) -> bool {
        self.copy_on_select
            .unwrap_or(cfg!(not(target_os = "linux")))
    }

    /// Master switch for generated live Activity. Generation is batteries-on,
    /// while `false` or `title_summary_provider = "off"` retains Title/Description
    /// composition and only disables the generated fallback.
    pub(crate) fn descriptive_titles_or_default(&self) -> bool {
        self.descriptive_titles.unwrap_or(true)
    }

    /// Resolved description provider. The built-in, entirely local summarizer
    /// is the safe default.
    pub(crate) fn title_summary_provider_or_default(&self) -> TitleSummaryProvider {
        self.title_summary_provider.unwrap_or_default()
    }

    /// Resolved LLM model identifier. Blank values behave like an absent key so
    /// the Settings text field cannot accidentally select an empty model name.
    pub(crate) fn title_summary_model_or_default(&self) -> &str {
        self.title_summary_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .unwrap_or(DEFAULT_TITLE_SUMMARY_MODEL)
    }

    /// Explicit endpoint used by the resolved provider. An absent Ollama value is
    /// deliberately preserved as `None`: the worker selects its private ephemeral
    /// endpoint at runtime. OpenAI-compatible service configuration is fail-closed
    /// and must provide an endpoint. In-process and disabled providers ignore a
    /// stale value.
    pub(crate) fn title_summary_endpoint_or_default(&self) -> Option<&str> {
        let configured = self
            .title_summary_endpoint
            .as_deref()
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty());
        match self.title_summary_provider_or_default() {
            TitleSummaryProvider::Ollama | TitleSummaryProvider::OpenAiCompatible => configured,
            TitleSummaryProvider::Builtin | TitleSummaryProvider::Off => None,
        }
    }

    /// Optional bearer-token file, with blank Settings values normalized away.
    pub(crate) fn title_summary_token_file(&self) -> Option<&str> {
        self.title_summary_token_file
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
    }

    /// Provider request timeout, bounded so a failed service cannot strand the
    /// single summary worker indefinitely and an accidental zero is still useful.
    pub(crate) fn title_summary_timeout_seconds_or_default(&self) -> u64 {
        self.title_summary_timeout_seconds
            .unwrap_or(DEFAULT_TITLE_SUMMARY_TIMEOUT_SECONDS)
            .clamp(
                MIN_TITLE_SUMMARY_TIMEOUT_SECONDS,
                MAX_TITLE_SUMMARY_TIMEOUT_SECONDS,
            )
    }

    /// Proxy policy for remote providers. Managed local Ollama overrides this
    /// at the transport boundary and always connects directly.
    pub(crate) fn title_summary_proxy_mode_or_default(&self) -> TitleSummaryProxyMode {
        self.title_summary_proxy_mode.unwrap_or_default()
    }

    /// Optional additional CA bundle path, with blank Settings values removed.
    pub(crate) fn title_summary_ca_file(&self) -> Option<&str> {
        self.title_summary_ca_file
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
    }

    /// Refresh cadence in seconds, bounded so live descriptions neither hammer
    /// a provider nor become effectively static after a mistyped value.
    pub(crate) fn title_summary_interval_seconds_or_default(&self) -> u64 {
        self.title_summary_interval_seconds
            .unwrap_or(DEFAULT_TITLE_SUMMARY_INTERVAL_SECONDS)
            .clamp(
                MIN_TITLE_SUMMARY_INTERVAL_SECONDS,
                MAX_TITLE_SUMMARY_INTERVAL_SECONDS,
            )
    }

    /// Recent terminal lines made available to a summarizer, with a fixed
    /// privacy/performance bound.
    pub(crate) fn title_summary_context_lines_or_default(&self) -> usize {
        self.title_summary_context_lines
            .unwrap_or(DEFAULT_TITLE_SUMMARY_CONTEXT_LINES)
            .clamp(
                MIN_TITLE_SUMMARY_CONTEXT_LINES,
                MAX_TITLE_SUMMARY_CONTEXT_LINES,
            )
    }

    /// Whether recent output, rather than only title/process metadata, may be
    /// included in description context. Default ON.
    pub(crate) fn title_summary_include_output_or_default(&self) -> bool {
        self.title_summary_include_output.unwrap_or(true)
    }

    /// Explicit privacy gate for remote or otherwise unattested providers.
    pub(crate) fn title_summary_allow_remote_or_default(&self) -> bool {
        self.title_summary_allow_remote.unwrap_or(false)
    }

    /// Resolved tab-label composition.
    pub(crate) fn tab_title_format_or_default(&self) -> TitleFormat {
        self.tab_title_format.unwrap_or_default()
    }

    /// Resolved native-window-title composition.
    pub(crate) fn window_title_format_or_default(&self) -> TitleFormat {
        self.window_title_format.unwrap_or_default()
    }

    /// Master switch for status classification. Batteries-on, and `false` is a
    /// real off: the observer stops gathering evidence entirely, so a user who
    /// does not want this subsystem pays nothing for it.
    pub(crate) fn tab_status_or_default(&self) -> bool {
        self.tab_status.unwrap_or(true)
    }

    /// Whether a status projects onto the tab's indicator bits. Independent of
    /// [`Self::tab_status_or_default`]: the record can be useful to a script
    /// while the chrome stays quiet.
    pub(crate) fn tab_status_badge_or_default(&self) -> bool {
        self.tab_status_badge.unwrap_or(true)
    }

    /// Whether the connection role marks the tab (Session Connections §4).
    /// Opt-OUT like every user-facing feature; hides only the mark — see the
    /// field doc.
    pub(crate) fn tab_connection_badge_or_default(&self) -> bool {
        self.tab_connection_badge.unwrap_or(true)
    }

    /// Stall threshold, bounded so a mistyped value can neither call every job
    /// quiet on arrival nor keep a finished one lit past any useful horizon.
    pub(crate) fn tab_status_quiet_after_ms_or_default(&self) -> u64 {
        self.tab_status_quiet_after_ms
            .unwrap_or(DEFAULT_TAB_STATUS_QUIET_AFTER_MS)
            .clamp(MIN_TAB_STATUS_QUIET_AFTER_MS, MAX_TAB_STATUS_QUIET_AFTER_MS)
    }

    /// Publication dwell, bounded so hysteresis cannot be set long enough to
    /// make the badge useless.
    pub(crate) fn tab_status_dwell_ms_or_default(&self) -> u64 {
        self.tab_status_dwell_ms
            .unwrap_or(DEFAULT_TAB_STATUS_DWELL_MS)
            .clamp(MIN_TAB_STATUS_DWELL_MS, MAX_TAB_STATUS_DWELL_MS)
    }

    /// The resolved classifier policy.
    pub(crate) fn tab_status_policy(&self) -> crate::session_status::StatusPolicy {
        crate::session_status::StatusPolicy {
            quiet_after: std::time::Duration::from_millis(
                self.tab_status_quiet_after_ms_or_default(),
            ),
            dwell: std::time::Duration::from_millis(self.tab_status_dwell_ms_or_default()),
        }
    }

    /// The observation budget interval. Derived rather than configured: an
    /// interval COARSER than the dwell would make the dwell unserviceable (a
    /// candidate could not be re-seen inside its own window), so the budget
    /// tracks dwell downward and stops at a floor that keeps an output flood
    /// from becoming a classification flood.
    pub(crate) fn tab_status_observe_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.tab_status_dwell_ms_or_default().clamp(
            MIN_TAB_STATUS_OBSERVE_INTERVAL_MS,
            NOMINAL_TAB_STATUS_OBSERVE_INTERVAL_MS,
        ))
    }

    /// Whether the cursor motion-trail ("streaming trailer") is on. Default
    /// [`DEFAULT_DECORATIVE_EFFECTS`]: ON as an opt-OUT everywhere except Windows,
    /// where the minimal-fast directive makes it an opt-IN.
    ///
    /// ON (mac/Linux, and any host that says `cursor_trail = true`) the trail ignites a
    /// ~260 ms additive aurora on each cursor move and decays to EXACTLY 0% idle, so a
    /// still screen costs nothing; the GPU bloom pass is effect-frame-only and
    /// load-sheds under pressure. That aurora is what `5b11ff2c` decided — the owner's
    /// batteries-on delight call — and it stands, unchanged, on those platforms.
    ///
    /// OFF ON WINDOWS, because the key stopped meaning the thing that was decided. The
    /// default STYLE later became `rainbow kitty pet`, so this master now also seats a
    /// PERMANENTLY RESIDENT walking cat drawn `FreeZ::OverText` across live output — the
    /// 2026-08 Windows audit photographed it sitting on a line of real terminal text
    /// beside the caret. A decoration that covers output it did not produce is the one
    /// thing the minimal-fast directive rules out, and the owner's own `aterm.toml`
    /// already wrote `cursor_trail = false` by hand: a default a user has to undo is not
    /// a default. Turning it off also stops the four expensive effect pipelines this
    /// master alone can bind (see [`Self::warms_effect_pipelines`]) from ever being
    /// asked for.
    ///
    /// This is a DEFAULT, never a veto: a Windows user who wants the cat writes
    /// `cursor_trail = true` and gets it, whole. Nothing about the trail's behaviour,
    /// its style, or its gates differs by platform once it is on.
    pub(crate) fn cursor_trail_or_default(&self) -> bool {
        self.cursor_trail.unwrap_or(DEFAULT_DECORATIVE_EFFECTS)
    }

    /// Whether a config APPLY should warm the demand-driven effect pipelines
    /// (`aterm_gpu::EffectPipeline`) off the frame path.
    ///
    /// The nine effect-only cell pipelines are built by `encode_frame` the first
    /// frame that binds one, so a launch compiles none of them. Correctness never
    /// depends on this predicate — it only decides whether a config apply pays a
    /// compile EARLY so the first frame that draws a newly-enabled effect does not
    /// hitch.
    ///
    /// Keyed on the cursor-trail master because that is the sole producer of the
    /// only four pipelines whose inline build is a visible hitch: `fire_add` +
    /// `fire_over` (111.07 ms together on dx12) and `rain_glow` +
    /// `rain_glow_over` (17.70). With the trail off none of those can be bound,
    /// and the five that remain reachable are each well under one frame — so an
    /// unconditional warm would hand a `cursor_trail = false` owner the exact
    /// 136 ms of never-drawn compiles this whole design removes, merely moved off
    /// the launch and onto their first config save.
    pub(crate) fn warms_effect_pipelines(&self) -> bool {
        self.cursor_trail_or_default()
    }

    /// Trail sound effects on/off (`trail_sounds`, default ON — they are
    /// already silenced by every gate that silences the trail's light).
    pub(crate) fn trail_sounds_or_default(&self) -> bool {
        self.trail_sounds.unwrap_or(true)
    }

    /// Tone-melody on/off (`tone_melody`, default ON — subtle by design; see
    /// the field docs for what it moves and every gate it inherits).
    pub(crate) fn tone_melody_or_default(&self) -> bool {
        self.tone_melody.unwrap_or(true)
    }

    /// Robi the helper robot on/off (`robi`, default OFF — opt-in by owner
    /// directive; see the field doc).
    pub(crate) fn robi_or_default(&self) -> bool {
        self.robi.unwrap_or(false)
    }

    /// Secure Keyboard Entry (`secure_keyboard_entry`): fail-closed default OFF
    /// like every other security opt-in — the OS mechanism has real side
    /// effects (it suppresses other apps' global hotkeys while active), so it
    /// is the user's call, never a surprise.
    pub(crate) fn secure_keyboard_entry_or_default(&self) -> bool {
        self.secure_keyboard_entry.unwrap_or(false)
    }

    /// Rainbow sparkles on the post-update celebration (default ON — opt-OUT).
    pub(crate) fn notice_sparkle_or_default(&self) -> bool {
        self.notice_sparkle.unwrap_or(true)
    }

    /// Progress-card party trim — rainbow bar, sparkles, the cat — on the
    /// toolchain-provisioning card (default [`DEFAULT_DECORATIVE_EFFECTS`]: ON as an
    /// opt-OUT everywhere except Windows, where the minimal-fast directive makes it
    /// an opt-IN; see the field doc — the card itself stays fully functional and
    /// legible either way).
    pub(crate) fn pkg_progress_effects_or_default(&self) -> bool {
        self.pkg_progress_effects
            .unwrap_or(DEFAULT_DECORATIVE_EFFECTS)
    }

    /// Ambient-bed on/off (`trail_sound_bed`, default OFF — the drone is
    /// opt-in; see the field docs: notes/brrrring/bonk/melody unaffected).
    pub(crate) fn trail_sound_bed_or_default(&self) -> bool {
        self.trail_sound_bed.unwrap_or(false)
    }

    /// Sing-along RIFF on/off (`trail_sound_riff`, default ON — a shipped
    /// feature, so this is an opt-OUT). The riff's own switch: the celebration's
    /// VISUALS keep running when it is off, and every existing sound gate
    /// (raw focus × `trail_sounds` × `trail_sound_volume`) still applies first,
    /// so this can only ever remove sound. See the field docs for why the
    /// loudest voice in the engine needed a switch of its own.
    pub(crate) fn trail_sound_riff_or_default(&self) -> bool {
        self.trail_sound_riff.unwrap_or(true)
    }

    /// Audible terminal bell on/off (`bell_sound`, default ON). Gates ONLY the
    /// OS alert sound (`NSBeep` / `MessageBeep`) — never the visual bell flash
    /// or the urgent-window attention request, which are how a muted terminal
    /// still surfaces background activity. `trail_sound_volume` deliberately
    /// does not reach this: the beep is an OS sound, not a synth voice.
    pub(crate) fn bell_sound_or_default(&self) -> bool {
        self.bell_sound.unwrap_or(true)
    }

    /// The parsed `trail_sound_style` voice (default `"auto"` → follow the
    /// visual trail style, the exact pre-override identity). Resolved by the
    /// synth's own parser ([`aterm_effects::trail_sound::SoundVoice::parse`]:
    /// the picker's canonical spellings plus the documented aliases,
    /// case-insensitive) — borrow-free, no allocation, so the per-frame sound
    /// seam pays nothing (the `cursor_trail_style_raw` precedent). Unknown
    /// spellings fall back to `auto` (the sound keeps playing; the Settings
    /// picker, save-time validation and `--validate-config`'s domain warning
    /// keep the spelling honest).
    pub(crate) fn trail_sound_voice(&self) -> aterm_effects::trail_sound::SoundVoice {
        use aterm_effects::trail_sound::SoundVoice;
        self.trail_sound_style
            .as_deref()
            .and_then(SoundVoice::parse)
            .unwrap_or_default()
    }

    /// The `--validate-config` domain warning for `trail_sound_style`: an
    /// unknown spelling is not an error at load (the voice falls back to
    /// `auto` and the sound keeps playing), but it IS a silent no-op the
    /// author should hear about — the `cursor_trail_style_warning` twin.
    pub(crate) fn trail_sound_style_warning(&self) -> Option<String> {
        use aterm_effects::trail_sound::SoundVoice;
        let raw = self.trail_sound_style.as_deref().map(str::trim)?;
        if SoundVoice::parse(raw).is_some() {
            return None;
        }
        Some(format!(
            "trail_sound_style: {raw:?} is not a typing sound — playing auto (follow the \
             trail); expected one of {} (or a documented alias like water/mech/thock)",
            crate::prefs::TRAIL_SOUND_STYLES.join("|")
        ))
    }

    /// Trail sound volume 0..1 (`trail_sound_volume`, default 0.4), clamped
    /// so a config typo can never make keystrokes loud. TOML accepts NaN/Inf;
    /// those fail silent rather than poisoning the audio graph.
    pub(crate) fn trail_sound_volume(&self) -> f32 {
        match self.trail_sound_volume {
            Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
            Some(_) => 0.0,
            None => 0.4,
        }
    }

    /// The wallpaper legibility dim (`wallpaper_dim`): the fraction the image
    /// is toned toward the theme background. Clamped to `0..=1`; a non-finite
    /// value and the unset default both read 0.3.
    pub(crate) fn wallpaper_dim_or_default(&self) -> f32 {
        match self.wallpaper_dim {
            Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
            _ => 0.3,
        }
    }

    /// Whether default-fg glyphs take the backdrop-hue tint while a wallpaper
    /// is attached (`wallpaper_text_tint`). Default ON.
    pub(crate) fn wallpaper_text_tint_or_default(&self) -> bool {
        self.wallpaper_text_tint.unwrap_or(true)
    }

    /// The parsed `motion` policy mode (W11): `auto` (DEFAULT — use the available
    /// platform sample: live on macOS, attach-time on Windows, unavailable on
    /// other platforms) / `full` / `reduced`; unknown strings fall back to
    /// `auto`. Borrow-free parse per call, so the per-redraw caller
    /// (`App::motion_policy`) pays no allocation.
    pub(crate) fn motion_mode(&self) -> crate::motion::MotionMode {
        self.motion.as_deref().map_or(
            crate::motion::MotionMode::Auto,
            crate::motion::MotionMode::parse,
        )
    }

    /// Whether load-adaptive effect shedding may force the Reduced state under a
    /// sustained render-overload session (Change #1). DEFAULT `true`; `false` opts out
    /// so animations follow `motion` / the OS Reduce-Motion flag alone. `motion =
    /// "full"` overrides the shed regardless of this. See [`crate::App::motion_policy`].
    pub(crate) fn load_adaptive_motion_or_default(&self) -> bool {
        self.load_adaptive_motion.unwrap_or(true)
    }

    /// Serious-mode master gate. DEFAULT OFF; the App keeps the resolved value so
    /// toggles and hot reloads can apply one atomic transition without rewriting any
    /// of the underlying effect settings.
    pub(crate) fn serious_mode_or_default(&self) -> bool {
        self.serious_mode.unwrap_or(false)
    }

    /// Predictive-local-echo mode string (`off` / `adaptive` / `always`). DEFAULT
    /// `adaptive`: ON once an echo is confirmed on the current line and measured RTT
    /// is high enough for speculation to help; fast local echo remains unpainted.
    /// Unechoed input (passwords) is never shown because each submitted line resets
    /// the confirmation epoch.
    /// Set `off` to disable, or `always` for the aggressive (unsafe-at-prompts) variant.
    pub(crate) fn predictive_echo_or_default(&self) -> &str {
        self.predictive_echo
            .as_deref()
            .map(str::trim)
            .unwrap_or("adaptive")
    }

    /// Focus-linked shell priority boost master switch. DEFAULT ON — it costs
    /// nothing when idle (a few cheap syscalls per focus/tab CHANGE — priority
    /// class + power-throttling per transitioning session's shell root and
    /// conhost, so ~4 per session entering/leaving the visible set — and none
    /// while typing), directly counters the ConPTY echo-starvation under load,
    /// and only ever touches the shell ROOT process + its console host (never
    /// the programs the shell runs). `focus_boost = false` opts out.
    pub(crate) fn focus_boost_or_default(&self) -> bool {
        self.focus_boost.unwrap_or(true)
    }

    /// M2 stream fade master switch. DEFAULT OFF — even with the keystroke-echo bypass,
    /// leaving it on runs an O(rows×cols) whole-grid fingerprint pass on the UI thread
    /// before every committed frame. A minimal terminal does no per-frame diff work;
    /// opt in with `stream_fade = true`.
    pub(crate) fn stream_fade_or_default(&self) -> bool {
        self.stream_fade.unwrap_or(false)
    }

    /// Stream-fade window (ms). Default 90; clamped to 16..=1000.
    pub(crate) fn stream_fade_ms_or_default(&self) -> u64 {
        self.stream_fade_ms.unwrap_or(90).clamp(16, 1000)
    }

    /// Trail fade duration (ms). Default 260; clamped to a sane 30..=2000 so a
    /// typo can't wedge the animation timer on or strobe it.
    pub(crate) fn cursor_trail_ms_or_default(&self) -> u64 {
        self.cursor_trail_ms.unwrap_or(260).clamp(30, 2000)
    }

    /// Max comet length in cells. Default 24; clamped to 1..=512.
    pub(crate) fn cursor_trail_length_or_default(&self) -> usize {
        self.cursor_trail_length.unwrap_or(24).clamp(1, 512)
    }

    /// The explicit trail-colour override as packed `0x00RRGGBB`, if `cursor_trail_color`
    /// is set and valid. `None` → the caller falls back to the themed cursor colour.
    pub(crate) fn cursor_trail_color_u32(&self) -> Option<u32> {
        self.cursor_trail_color
            .as_deref()
            .and_then(parse_hex_color)
            .map(|c| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b))
    }

    /// The PRIMARY-FONT REQUEST with the display-face override applied: a valid
    /// `display_font` selection wins as the virtual family `display:<id>` — or,
    /// for a MIX of 2..=3 `+`-joined ids, `display:<id>+<id>[+<id>]` (every
    /// character then deterministically picks one face of the mix). Otherwise
    /// the plain `font_family` passes through verbatim. Unknown/duplicate ids
    /// are DROPPED (and `"off"` never counts), so a typo can never blank the
    /// terminal; more than three keeps the first three (the toggles cap there
    /// anyway). Every font resolution site (startup, catalog worker,
    /// diagnostics) funnels through this so the toggles can never half-apply.
    ///
    /// Legacy ids are CANONICALIZED here rather than special-cased downstream:
    /// `minecraft` becomes `pixel`, so one config value produces one virtual
    /// family and every consumer sees the same spelling. `mariokart` — the one
    /// retired id with no successor — canonicalizes to nothing and therefore
    /// falls through to `font_family`, which is the documented migration: a
    /// config that was valid yesterday warns and loads, it does not fail.
    pub(crate) fn font_family_request(&self) -> Option<String> {
        let display = self.display_font.as_deref().and_then(|raw| {
            let mut ids: Vec<&'static str> = Vec::new();
            for id in raw.split('+') {
                if let Some(id) = aterm_render::display_face_canonical_id(id)
                    && !ids.contains(&id)
                    && ids.len() < aterm_render::DISPLAY_FACE_MIX_MAX
                {
                    ids.push(id);
                }
            }
            (!ids.is_empty())
                .then(|| format!("{}{}", aterm_render::DISPLAY_FACE_SCHEME, ids.join("+")))
        });
        display.or_else(|| self.font_family.clone())
    }

    /// The selected-tab color override as `[r, g, b]`, if `active_tab_color` is
    /// set and a valid `#RRGGBB`. `None` → both tab renderers keep today's
    /// translucent system default ("Transparent white" in the Tab Color page).
    pub(crate) fn active_tab_color_rgb(&self) -> Option<[u8; 3]> {
        self.active_tab_color
            .as_deref()
            .and_then(parse_hex_color)
            .map(|c| [c.r, c.g, c.b])
    }

    /// The aurora accent-colour override as packed `0x00RRGGBB`, if set + valid.
    pub(crate) fn cursor_trail_accent_u32(&self) -> Option<u32> {
        self.cursor_trail_accent
            .as_deref()
            .and_then(parse_hex_color)
            .map(|c| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b))
    }

    /// The configured cursor-trail style, trimmed but NOT lowercased — borrowed, so
    /// the per-redraw caller (`glow_config`, run every keystroke) can
    /// classify it with `eq_ignore_ascii_case` / a case-insensitive `GlowStyle::parse`
    /// instead of paying a `to_ascii_lowercase` heap allocation on every frame.
    pub(crate) fn cursor_trail_style_raw(&self) -> &str {
        // Default: the RAINBOW KITTY PET (the banded rainbow ribbon trailed by the
        // walking cat — the companion the owner runs), read from the single
        // definition in `prefs` rather than re-typed here, so a rename
        // of the style cannot leave this resolver pointing at a dead spelling.
        // `glow_config`/`trail_config` split the layers.
        self.cursor_trail_style
            .as_deref()
            .unwrap_or(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE)
            .trim()
    }

    /// Aurora brightness, default 0.7, clamped 0.0..=1.0. A non-finite value
    /// (`intensity = nan` is valid TOML) FAILS OFF to `0.0`: `clamp` passes NaN
    /// through, and NaN defeats every downstream `intensity <= 0.0` disable
    /// check (NaN compares false), so it would flow into the light math instead
    /// of provably disabling — the `sdr_glow_budget` "NaN fails OFF" posture.
    pub(crate) fn cursor_trail_intensity_or_default(&self) -> f32 {
        let v = self.cursor_trail_intensity.unwrap_or(0.7);
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// TYPING-WAKE length in SECONDS of recent travel, default 0.30 s (the
    /// engine's own [`aterm_effects::cursor_glow::RAINBOW_WAKE_PERSIST`]), clamped
    /// to 0..=1.5 s. `0` turns the wake off and is a legitimate setting, not a
    /// failure, so — unlike the aurora's intensity — there is nothing here that
    /// can fail open: every representable `u64` maps into the closed range.
    pub(crate) fn cursor_trail_wake_persist_or_default(&self) -> f32 {
        let ms = self
            .cursor_trail_wake_ms
            .unwrap_or((aterm_effects::cursor_glow::RAINBOW_WAKE_PERSIST * 1000.0) as u64)
            .min(1_500);
        ms as f32 / 1000.0
    }

    /// Bloom-crown radius in cells, default 0.6, clamped 0.0..=2.0.
    pub(crate) fn cursor_trail_radius_or_default(&self) -> f32 {
        self.cursor_trail_radius.unwrap_or(0.6).clamp(0.0, 2.0)
    }

    /// Landing-ring ping, default on.
    pub(crate) fn cursor_trail_ring_or_default(&self) -> bool {
        self.cursor_trail_ring.unwrap_or(true)
    }

    /// `[sparkle_words] suppress_in_alt_screen` — the design §1 knob, wired
    /// 2026-07-04. DEFAULT FALSE: sparkle words decorate the ALTERNATE screen
    /// too (full-screen TUIs — Claude Code foremost — are a primary surface,
    /// not an exception); `true` restores the previously hardcoded suppression
    /// (vim/less/htop program UI text never decorated). Two `Option` reads per
    /// frame — the `kitty_log_enabled` precedent, no cache to invalidate.
    pub(crate) fn sparkle_suppress_alt_screen(&self) -> bool {
        self.sparkle_words
            .as_ref()
            .and_then(|sw| sw.suppress_in_alt_screen)
            .unwrap_or(false)
    }

    /// The languages whose AMBIGUOUS homograph lexicon entries are un-gated.
    /// Default `["en"]`. Keys the process-wide lexicon cache.
    pub(crate) fn sparkle_languages(&self) -> Vec<String> {
        self.sparkle_words
            .as_ref()
            .and_then(|s| s.languages.clone())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| vec!["en".to_string()])
    }

    fn admitted_sparkle_lexicon(
        &self,
        fingerprint: &mut std::collections::hash_map::DefaultHasher,
    ) -> Option<AdmittedPathFeed> {
        let path = self
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.lexicon.as_deref())?;
        Some(read_and_fingerprint_path_feed(
            path,
            aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES,
            fingerprint,
        ))
    }

    #[cfg(test)]
    fn sparkle_toy_packs(&self) -> LoadedToyPacks {
        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        self.sparkle_toy_packs_with_fingerprint(&mut fingerprint)
    }

    fn sparkle_toy_packs_with_fingerprint(
        &self,
        fingerprint: &mut std::collections::hash_map::DefaultHasher,
    ) -> LoadedToyPacks {
        let mut loaded = LoadedToyPacks::default();
        let Some(paths) = self
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.toy_packs.as_deref())
        else {
            return loaded;
        };
        if paths.len() > MAX_ACTIVE_TOY_PACKS {
            eprintln!(
                "aterm-gui: sparkle_words.toy_packs lists {} paths; only the first \
                 {MAX_ACTIVE_TOY_PACKS} are active",
                paths.len()
            );
        }
        for (index, path) in paths.iter().take(MAX_ACTIVE_TOY_PACKS).enumerate() {
            let (expanded, source) = read_and_fingerprint_path_feed(
                path,
                aterm_effects::spec::MAX_TOY_PACK_BYTES,
                fingerprint,
            );
            let source = match source {
                Ok(source) => source,
                Err(error) => {
                    eprintln!(
                        "aterm-gui: sparkle_words.toy_packs[{index}] {expanded:?} \
                         unreadable ({error}); skipping"
                    );
                    continue;
                }
            };
            let pack = match aterm_effects::spec::compile_toy_pack_toml(&source) {
                Ok(pack) => pack,
                Err(error) => {
                    eprintln!(
                        "aterm-gui: sparkle_words.toy_packs[{index}] {expanded:?} \
                         invalid ({error}); skipping"
                    );
                    continue;
                }
            };
            let (_, spec_table, lexicon_toml) = pack.into_parts();
            // Ordered overlay: this pack wins over every earlier valid pack.
            loaded.spec_table.overlay(spec_table);
            loaded.lexicon_toml.push_str(&lexicon_toml);
        }
        loaded
    }

    /// Resolve one immutable Trail Pack catalog. This is the only manifest I/O
    /// primitive. Preliminary startup catalogs remain empty; the published
    /// path-feed generation resolves consumer data and fingerprint from the
    /// same admitted bytes, then shares the returned `Arc`.
    ///
    /// Fail-closed per pack: unreadable/invalid manifests become retained
    /// diagnostics and are skipped, while a later duplicate id wins. Diagnostics
    /// are data rather than immediate stderr writes so each host can surface them
    /// once without Settings construction producing duplicate warnings.
    pub(crate) fn resolve_trail_pack_catalog(&self) -> std::sync::Arc<TrailPackCatalog> {
        self.resolve_trail_pack_catalog_with_fingerprint_and_reader(|_, path, max_bytes| {
            aterm_effects::file_feed::read_bounded_regular_utf8(path, max_bytes)
        })
        .0
    }

    fn resolve_trail_pack_catalog_with_fingerprint(
        &self,
    ) -> (std::sync::Arc<TrailPackCatalog>, u64) {
        self.resolve_trail_pack_catalog_with_fingerprint_and_reader(|_, path, max_bytes| {
            aterm_effects::file_feed::read_bounded_regular_utf8(path, max_bytes)
        })
    }

    fn resolve_trail_pack_catalog_with_fingerprint_and_reader(
        &self,
        mut read: impl FnMut(usize, &std::path::Path, usize) -> std::io::Result<String>,
    ) -> (std::sync::Arc<TrailPackCatalog>, u64) {
        use std::hash::Hasher as _;

        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        let mut loaded = TrailPackCatalog::default();
        let Some(paths) = self.cursor_trail_packs.as_deref() else {
            return (std::sync::Arc::new(loaded), fingerprint.finish());
        };
        if paths.len() > MAX_ACTIVE_TOY_PACKS {
            loaded.diagnostics.push(format!(
                "cursor_trail_packs lists {} paths; only the first \
                 {MAX_ACTIVE_TOY_PACKS} are active",
                paths.len(),
            ));
        }
        for (index, path) in paths.iter().take(MAX_ACTIVE_TOY_PACKS).enumerate() {
            let (expanded, source) = read_and_fingerprint_path_feed_with_reader(
                path,
                aterm_effects::trail_pack::MAX_TRAIL_PACK_BYTES,
                &mut fingerprint,
                |expanded, max_bytes| read(index, expanded, max_bytes),
            );
            let source = match source {
                Ok(source) => source,
                Err(error) => {
                    loaded.diagnostics.push(format!(
                        "cursor_trail_packs[{index}] {expanded:?} unreadable ({error}); skipping"
                    ));
                    continue;
                }
            };
            let pack = match aterm_effects::trail_pack::compile_trail_pack_toml(&source) {
                Ok(pack) => pack,
                Err(error) => {
                    loaded.diagnostics.push(format!(
                        "cursor_trail_packs[{index}] {expanded:?} invalid ({error}); skipping"
                    ));
                    continue;
                }
            };
            let (metadata, params) = pack.into_parts();
            loaded.packs.insert(metadata.id, params);
        }
        loaded.ids = loaded.packs.keys().cloned().collect();
        loaded.ids.sort();
        (std::sync::Arc::new(loaded), fingerprint.finish())
    }

    /// Resolve the non-feed portion of a catalog while the exact Trail/Sparkle
    /// generation is prepared separately. This admits the custom kitty sprite
    /// once and installs an intentionally empty Trail placeholder that must be
    /// replaced before publication to a live `App`.
    pub(crate) fn resolve_preliminary_asset_catalog_with_themes(
        &self,
        themes: std::sync::Arc<ThemeCatalog>,
    ) -> std::sync::Arc<ConfigAssetCatalog> {
        std::sync::Arc::new(ConfigAssetCatalog {
            trail_packs: std::sync::Arc::new(TrailPackCatalog::default()),
            kitty_sprite: resolve_kitty_sprite_asset(self.cursor_nyan_sprite.as_deref()),
            wallpaper: resolve_wallpaper_asset(self.wallpaper.as_deref()),
            themes,
            sparkle_spec_consumers: None,
        })
    }

    /// Resolve every filesystem-backed visual asset for one config generation.
    ///
    /// This is the sole production rainbow kitty PNG I/O/decode seam.  The versioned
    /// config service calls it before advancing the revision, then publishes the
    /// returned outer Arc with the exact TOML text.  Present, capture, semantic
    /// Settings view construction, and effects code only clone/read the result.
    pub(crate) fn resolve_asset_catalog_with_themes(
        &self,
        themes: std::sync::Arc<ThemeCatalog>,
    ) -> std::sync::Arc<ConfigAssetCatalog> {
        // Sparkle Toy Packs are compiled by the font/config preparation lane.
        // This preliminary catalog deliberately reports `None`: an inline-only
        // projection is not an authoritative answer for the exact combined
        // table and must never be published as though it were complete.
        std::sync::Arc::new(ConfigAssetCatalog {
            trail_packs: self.resolve_trail_pack_catalog(),
            kitty_sprite: resolve_kitty_sprite_asset(self.cursor_nyan_sprite.as_deref()),
            wallpaper: resolve_wallpaper_asset(self.wallpaper.as_deref()),
            themes,
            sparkle_spec_consumers: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn resolve_asset_catalog(&self) -> std::sync::Arc<ConfigAssetCatalog> {
        self.resolve_asset_catalog_with_themes(ThemeCatalog::empty())
    }

    /// Content fingerprints of every active file the config references BY PATH
    /// — see [`PathFeedFps`]. Streams each referenced file through the same
    /// bounded, same-handle admission used by its loader (test oracle only; the
    /// production generation carries the fingerprint from its consumer read).
    /// Disabled Sparkle feeds have no consumer and therefore no file identity. The
    /// PATH participates in each stream (two files swapping contents must not
    /// cancel out) and so does readability (a file appearing or disappearing is
    /// a content change even though its bytes stay unknown). Pack lists are
    /// capped at the same `MAX_ACTIVE_TOY_PACKS` the consumers load, so an
    /// over-cap tail can neither mask nor fake a change.
    #[cfg(test)]
    pub(crate) fn path_feed_fingerprints(&self) -> PathFeedFps {
        #[cfg(test)]
        PATH_FEED_FINGERPRINTS.with(|count| count.set(count.get().saturating_add(1)));
        use std::hash::{Hash, Hasher};
        fn fold(
            hasher: &mut std::collections::hash_map::DefaultHasher,
            path: &str,
            max_bytes: usize,
            remaining_opens: &mut usize,
            remaining_bytes: &mut usize,
        ) {
            path.hash(hasher);
            let Some(read_ceiling) = max_bytes.checked_add(1) else {
                false.hash(hasher);
                return;
            };
            if *remaining_opens == 0 || *remaining_bytes < read_ceiling {
                false.hash(hasher);
                return;
            }
            *remaining_opens -= 1;
            *remaining_bytes -= read_ceiling;
            match aterm_effects::file_feed::fingerprint_bounded_regular_utf8(
                &sparkle_expand_tilde(path),
                max_bytes,
            ) {
                Ok(content_fingerprint) => {
                    true.hash(hasher);
                    content_fingerprint.hash(hasher);
                }
                Err(_) => false.hash(hasher),
            }
        }
        let mut remaining_opens = MAX_PATH_FEED_OPEN_ATTEMPTS;
        let mut remaining_bytes = MAX_PATH_FEED_FINGERPRINT_READ_BYTES;
        let mut deco = std::collections::hash_map::DefaultHasher::new();
        if let Some(sw) = self
            .sparkle_words
            .as_ref()
            .filter(|sparkle| sparkle.enabled != Some(false))
        {
            if let Some(lexicon) = sw.lexicon.as_deref() {
                fold(
                    &mut deco,
                    lexicon,
                    aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES,
                    &mut remaining_opens,
                    &mut remaining_bytes,
                );
            }
            for path in sw
                .toy_packs
                .as_deref()
                .unwrap_or_default()
                .iter()
                .take(MAX_ACTIVE_TOY_PACKS)
            {
                fold(
                    &mut deco,
                    path,
                    aterm_effects::spec::MAX_TOY_PACK_BYTES,
                    &mut remaining_opens,
                    &mut remaining_bytes,
                );
            }
        }
        let mut trail = std::collections::hash_map::DefaultHasher::new();
        for path in self
            .cursor_trail_packs
            .as_deref()
            .unwrap_or_default()
            .iter()
            .take(MAX_ACTIVE_TOY_PACKS)
        {
            fold(
                &mut trail,
                path,
                aterm_effects::trail_pack::MAX_TRAIL_PACK_BYTES,
                &mut remaining_opens,
                &mut remaining_bytes,
            );
        }
        PathFeedFps {
            deco: deco.finish(),
            trail: trail.finish(),
        }
    }

    /// Mirror the runtime Toy Pack admission checks for `--validate-config`.
    /// This stays on the explicit diagnostics path; normal startup uses
    /// `sparkle_toy_packs` once and does not pay for a second read.
    pub(crate) fn sparkle_toy_pack_warnings(&self) -> Vec<String> {
        let Some(paths) = self
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.toy_packs.as_deref())
        else {
            return Vec::new();
        };
        let mut warnings = Vec::new();
        if paths.len() > MAX_ACTIVE_TOY_PACKS {
            warnings.push(format!(
                "sparkle_words.toy_packs: {} paths listed; only the first \
                 {MAX_ACTIVE_TOY_PACKS} load",
                paths.len()
            ));
        }
        for (index, path) in paths.iter().take(MAX_ACTIVE_TOY_PACKS).enumerate() {
            let expanded = sparkle_expand_tilde(path);
            match aterm_effects::spec::read_toy_pack_file(&expanded) {
                Err(error) => warnings.push(format!(
                    "sparkle_words.toy_packs[{index}] {expanded:?} unreadable ({error})"
                )),
                Ok(source) => {
                    if let Err(error) = aterm_effects::spec::compile_toy_pack_toml(&source) {
                        warnings.push(format!(
                            "sparkle_words.toy_packs[{index}] {expanded:?} invalid ({error})"
                        ));
                    }
                }
            }
        }
        warnings
    }

    /// Resolve the complete cold-path pack/config bundle once. The native app
    /// calls this only from `recompute_sparkle` (startup, config reload, or a
    /// user re-enable), never from the per-frame effects tick.
    pub(crate) fn sparkle_runtime_parts(
        &self,
    ) -> Option<(crate::word_decorations::DecoConfig, Option<String>)> {
        self.sparkle_runtime_parts_with_fingerprint().0
    }

    fn sparkle_runtime_parts_with_fingerprint(
        &self,
    ) -> (
        Option<(crate::word_decorations::DecoConfig, Option<String>)>,
        u64,
    ) {
        use std::hash::Hasher as _;

        #[cfg(test)]
        SPARKLE_HOST_PREPARES.with(|count| count.set(count.get().saturating_add(1)));
        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        if self
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.enabled)
            == Some(false)
        {
            // A disabled feed has no consumer. Configuring/re-enabling it changes
            // the semantic table itself and forces a fresh worker generation, so
            // touching dormant paths need not do any startup I/O or parsing.
            return (None, fingerprint.finish());
        }
        let admitted_lexicon = self.admitted_sparkle_lexicon(&mut fingerprint);
        let LoadedToyPacks {
            spec_table,
            lexicon_toml,
        } = self.sparkle_toy_packs_with_fingerprint(&mut fingerprint);
        let resolved = self
            .sparkle_deco_config_with_pack_specs(spec_table)
            .map(|cfg| {
                let override_toml = self.sparkle_override_toml_with_admitted_lexicon(
                    &lexicon_toml,
                    admitted_lexicon.as_ref(),
                );
                (cfg, override_toml)
            });
        (resolved, fingerprint.finish())
    }

    /// Compile the complete sparkle runtime on a host worker (or once during
    /// startup, before the event loop begins). This is the only production
    /// caller of the path-backed runtime resolver.
    #[cfg(test)]
    pub(crate) fn prepare_sparkle_runtime(&self) -> PreparedSparkleRuntime {
        self.prepare_sparkle_runtime_with_fingerprint().0
    }

    fn prepare_sparkle_runtime_with_fingerprint(&self) -> (PreparedSparkleRuntime, u64) {
        let (parts, fingerprint) = self.sparkle_runtime_parts_with_fingerprint();
        let resolved = parts.map(|(cfg, override_toml)| {
            let langs = self.sparkle_languages();
            let refs: Vec<&str> = langs.iter().map(String::as_str).collect();
            let lexicon = aterm_lexicon::Lexicon::with_languages_and_override(
                &refs,
                override_toml.as_deref(),
            )
            .unwrap_or_else(|error| {
                eprintln!(
                    "aterm-gui: sparkle words lexicon override rejected ({error}); using builtin"
                );
                aterm_lexicon::Lexicon::with_languages(&refs)
            });
            for warning in sparkle_logged_warnings(lexicon.conflicts(), cfg.cjk_single_char) {
                eprintln!("aterm-gui: sparkle_words lexicon: {warning}");
            }
            crate::word_decorations::Resolved {
                cfg,
                lexicon: std::sync::Arc::new(lexicon),
            }
        });
        (PreparedSparkleRuntime { resolved }, fingerprint)
    }

    /// Prepare every path-backed effect consumer and identify it from the exact
    /// same-handle bytes that entered its parser. The deco and trail feeds are
    /// independent transactions; combining their two immutable results needs
    /// no filesystem-wide snapshot or stability assumption.
    pub(crate) fn prepare_path_feed_generation(&self) -> PreparedPathFeedGeneration {
        let (sparkle, deco) = self.prepare_sparkle_runtime_with_fingerprint();
        let (trail_packs, trail) = self.resolve_trail_pack_catalog_with_fingerprint();
        PreparedPathFeedGeneration {
            sparkle,
            trail_packs,
            fingerprints: PathFeedFps { deco, trail },
        }
    }

    /// THE `[sparkle_words] enabled` MASTER, resolved — one owner for that key's
    /// default so no consumer re-types it.
    ///
    /// DEFAULT ON, on every platform, and deliberately NOT a member of the
    /// [`DEFAULT_DECORATIVE_EFFECTS`] family — re-decided in 2026-08 when
    /// `cursor_trail` DID join it, so this is a live judgement and not an omission.
    /// Three reasons, all checked against the code rather than assumed:
    ///
    /// * It DECORATES output; it does not cover it. A matched word gets its animal
    ///   head pushed `FreeZ::UnderText` (`word_decorations`), so every glyph still
    ///   draws on top of the fur — the 2026-08 Windows audit's claim that the engine
    ///   "REPLACES the words fox and dog" is wrong on the code. The resident pet, the
    ///   trail bloom and the provisioning card's cat are the ones that really do cover
    ///   live pixels, and each of those now has its own answer.
    /// * It does not ACCUMULATE. `MAX_CATS = 8` caps the on-screen population no
    ///   matter how long the dump runs; `persist` (cap `PERSIST_CAP = 512`) is
    ///   identity bookkeeping, not sprites. The audit's "accumulating across a
    ///   4000-line dump" is bounded at eight heads.
    /// * This key is not only a config default: native Settings treats it as a LIVE
    ///   PRODUCT GATE, with its own disclosure ladder ("Inactive · Sparkle Words
    ///   master Off"), its own migration path (the patch writer retires it in favour
    ///   of the two independent toy keys), and a dozen projection tests that read it.
    ///   Flipping its default by platform is a Settings redesign, not a default
    ///   change.
    ///
    /// THE ONE HONEST EXCEPTION, recorded rather than hidden: the profanity family's
    /// `supernova` escalation (`[sparkle_words.profanity] supernova_chance`, default
    /// 30) pushes its blast cloud `FreeZ::OverText`. That is a transient burst, capped
    /// by `supernova::MAX_ACTIVE_SUPERNOVAE = 1`, and it can only fire when a curse
    /// word is already on screen — it is not the resident, unprompted decoration on
    /// ORDINARY output that the minimal-fast directive rules out. If that one path
    /// ever wants a Windows answer, the aim is `supernova_chance`, not this master.
    pub(crate) fn sparkle_words_enabled_or_default(&self) -> bool {
        self.sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.enabled)
            .unwrap_or(true)
    }

    /// Resolve the `[sparkle_words]` table into a renderer-ready [`DecoConfig`],
    /// applying every default + clamp. `None` when the feature is explicitly disabled or
    /// every category is off (the caller then renders byte-identically).
    ///
    /// ON BY DEFAULT: an absent `[sparkle_words]` table (or absent `enabled` key) turns
    /// on the two live families—profanity sparkle and feline cat-paw. Orca settings
    /// remain parseable for compatibility but are suspended by `ORCA_SUSPENDED`.
    /// Set `enabled = false` (or a live category's `enabled = false`) to silence it.
    #[cfg(test)]
    pub(crate) fn sparkle_deco_config(&self) -> Option<crate::word_decorations::DecoConfig> {
        if self
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.enabled)
            == Some(false)
        {
            return None;
        }
        let packs = self.sparkle_toy_packs();
        self.sparkle_deco_config_with_pack_specs(packs.spec_table)
    }

    fn sparkle_deco_config_with_pack_specs(
        &self,
        mut spec_table: aterm_effects::spec::SpecTable,
    ) -> Option<crate::word_decorations::DecoConfig> {
        let sw = self.sparkle_words.clone().unwrap_or_default();
        if !sw.enabled.unwrap_or(true) {
            return None;
        }
        let prof = sw.profanity.clone().unwrap_or_default();
        let fel = sw.feline.clone().unwrap_or_default();
        let can_cfg = sw.canine.clone().unwrap_or_default();
        let orca_cfg = sw.orca.clone().unwrap_or_default();
        let ink_cfg = sw.ink.clone().unwrap_or_default();
        let emph = sw.emphasis.clone().unwrap_or_default();
        // Top Settings exposes exactly two independent keyword toys. Keep the
        // historical profanity key as the stable config spelling, but resolve it
        // as the product gate for every NON-FELINE word effect. Otherwise the
        // visible "Sparkle Words" switch would leave emphasis/custom recipes live.
        let sparkle_words = prof.enabled.unwrap_or(true);
        let profanity = sparkle_words;
        let feline = fel.enabled.unwrap_or(true);
        // The dog rides the PRODUCT switch like every non-feline effect (the
        // two-toys law: the historical profanity key is the Sparkle Words
        // master, keyword kitties alone stay independent), with its own
        // enable bit under it for turning just the dogs off.
        let canine = sparkle_words && can_cfg.enabled.unwrap_or(true);
        // v3 §4: the orca class is SUSPENDED — the resolver ANDs the single
        // const gate (engine/lexicon/splash untouched; flip ORCA_SUSPENDED to
        // re-enable).
        let orca =
            sparkle_words && orca_cfg.enabled.unwrap_or(true) && !aterm_effects::ORCA_SUSPENDED;
        let ink_enabled = ink_cfg.enabled.unwrap_or(true);
        // v3 §6: the custom-word spec table (per-word overrides, keyed by the
        // scanner's form_hash semantics — folded spaced surfaces, possessive
        // variants, RAW CJK hashes).
        let custom_entries = sw.custom.clone().unwrap_or_default();
        let (inline_specs, _) = aterm_effects::spec::build_custom(&custom_entries);
        // Inline config is the user's most local authority and overlays all
        // imported packs, including every possessive/hash variant. A product-off
        // Sparkle Words switch must make both sources inert, not merely hide the
        // built-in profanity class.
        if sparkle_words {
            spec_table.overlay(inline_specs);
        } else {
            spec_table = aterm_effects::spec::SpecTable::default();
        }
        // v3 §6 default-emphasis resolve. `emph.enabled` gates only ordinary,
        // non-overridden emphasis matches. Custom words are indexed under the
        // emphasis class, but the engine resolves their override BEFORE its
        // per-class gate (`WordDecorations::scan_row`: the class match runs
        // only when `ov.is_none()`), so they intentionally remain independent
        // of `emphasis.enabled`. Retain `has_custom` in this class-activity
        // projection so a graphic-only or burst-only custom has an admitted
        // emphasis lane with ink off; it is not evidence that the user's
        // emphasis switch controls that custom recipe.
        let emphasis = sparkle_words
            && emph.enabled.unwrap_or(true)
            && (ink_enabled || spec_table.has_custom());
        if !profanity && !feline && !canine && !orca && !emphasis {
            return None;
        }
        let ink_loop = ink_cfg.loop_.unwrap_or(false);
        // loop=true keeps a visible word sweeping forever, so the flash-safety
        // margin needs the higher sweep floor (§6.4).
        let ink_sweep_floor = if ink_loop { 600 } else { 350 };
        let ink_sweep_ms = ink_cfg
            .sweep_ms
            .unwrap_or(2200)
            .clamp(ink_sweep_floor, 6000);
        let ink_strength = ink_cfg.strength.unwrap_or(0.75).clamp(0.0, 1.0);
        let rgb_u32 = |c: Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
        let palette: Vec<u32> = prof
            .palette
            .unwrap_or_default()
            .iter()
            .filter_map(|s| parse_hex_color(s))
            .map(rgb_u32)
            .collect();
        // OS reduced-motion query is a future refinement; honour the explicit
        // config flag for now.
        let reduced_motion = sw.reduced_motion.unwrap_or(false);
        // Alt-screen suppression is opt-IN since 2026-07-03 (the v1 hardcoded
        // suppression left full-screen TUIs like Claude Code undecorated).
        let suppress_in_alt_screen = sw.suppress_in_alt_screen.unwrap_or(false);
        // Global `deny` + per-category `ignore_words`, folded so they match the
        // scanner's folded surfaces. Empty (the default) → no allocation churn.
        let mut ignore: std::collections::HashSet<String> = std::collections::HashSet::new();
        for words in [
            &sw.deny,
            &prof.ignore_words,
            &fel.ignore_words,
            &can_cfg.ignore_words,
            &orca_cfg.ignore_words,
            &emph.ignore_words,
        ]
        .into_iter()
        .flatten()
        {
            ignore.extend(words.iter().map(|w| aterm_lexicon::fold(w)));
        }
        Some(crate::word_decorations::DecoConfig {
            profanity,
            feline,
            canine,
            orca,
            emphasis,
            ink_enabled,
            ink_strength,
            ink_sweep_ms,
            ink_loop,
            reduced_motion,
            suppress_in_alt_screen,
            // Default ON so the literal three-letter `cat` decorates too (this also
            // decorates the `cat` shell command); set `allow_bare_cat = false` to opt out.
            allow_bare_cat: fel.allow_bare_cat.unwrap_or(true),
            cjk_single_char: fel.cjk_single_char.unwrap_or(false),
            // `paw` is retained as a legacy ink-only mode. Cat is the only mode
            // that emits a feline graphic under cat-art v4.
            feline_style: if fel
                .style
                .as_deref()
                .is_some_and(|style| style.trim().eq_ignore_ascii_case("paw"))
            {
                crate::word_decorations::FelineStyle::Paw
            } else {
                crate::word_decorations::FelineStyle::Cat
            },
            feline_magic: fel.magic.unwrap_or(true),
            // §10 / v3 §3.1: "sparkle" is the exact v1 path, "nova" the v2
            // classic nova; anything else (incl. absent) is the v3 rainbow.
            // Trimmed and case-insensitive, matching Manual's enum admission
            // and mirroring the web `set_sparkle_profanity`
            // setter — a cased "Nova" must not silently fall through to
            // Rainbow (and its supernova escalation roll).
            profanity_style: match prof.style.as_deref().map(str::trim) {
                Some(s) if s.eq_ignore_ascii_case("sparkle") => {
                    crate::word_decorations::ProfanityStyle::Sparkle
                }
                Some(s) if s.eq_ignore_ascii_case("nova") => {
                    crate::word_decorations::ProfanityStyle::Nova
                }
                _ => crate::word_decorations::ProfanityStyle::Rainbow,
            },
            profanity_magic: prof.magic.unwrap_or(true),
            // v3 §3.2: escalation chance, 0..=100 (0 disables).
            // Was `unwrap_or(30)` while the field's documented contract (see the
            // `supernova_chance` doc comment) says "Default 10" — so the shipping
            // default fired the 3.6 s screen-owning detonation ladder THREE TIMES as
            // often as documented. The doc is the specification; the code now matches
            // it. Set `supernova_chance = 30` explicitly to restore the old rate.
            supernova_chance: prof.supernova_chance.unwrap_or(10).min(100) as u8,
            spec_table,
            palette,
            density: prof.density.unwrap_or(3).clamp(1, 12),
            anim_ms: prof.anim_ms.unwrap_or(2500).clamp(350, 10_000),
            jitter: prof.jitter.unwrap_or(2).clamp(0, 6),
            intensity: prof.intensity.unwrap_or(0.85).clamp(0.0, 1.0),
            ignore,
            glyphs: vec![
                aterm_render::DecoGlyph::Star4,
                aterm_render::DecoGlyph::Star5,
                aterm_render::DecoGlyph::Dot,
                aterm_render::DecoGlyph::Plus,
            ],
        })
    }

    /// Build the lexicon override TOML for the sparkle feature. Imported Toy
    /// Packs follow the external lexicon/category extras in configured order;
    /// inline custom surfaces come last, matching their highest spec precedence.
    /// A missing/unreadable input is logged and skipped while the rest survives.
    #[cfg(test)]
    pub(crate) fn sparkle_override_toml(&self) -> Option<String> {
        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        let lexicon = self.admitted_sparkle_lexicon(&mut fingerprint);
        let packs = self.sparkle_toy_packs_with_fingerprint(&mut fingerprint);
        self.sparkle_override_toml_with_admitted_lexicon(&packs.lexicon_toml, lexicon.as_ref())
    }

    fn sparkle_override_toml_with_admitted_lexicon(
        &self,
        pack_lexicon_toml: &str,
        admitted_lexicon: Option<&AdmittedPathFeed>,
    ) -> Option<String> {
        let sw = self.sparkle_words.as_ref()?;
        let mut out = String::new();
        if let Some((expanded, admitted)) = admitted_lexicon {
            match admitted {
                Ok(contents) => {
                    let languages = self.sparkle_languages();
                    let refs: Vec<&str> = languages.iter().map(String::as_str).collect();
                    match aterm_lexicon::Lexicon::with_languages_and_override(&refs, Some(contents))
                    {
                        Ok(_) => {
                            out.push_str(contents);
                            if !contents.ends_with('\n') {
                                out.push('\n');
                            }
                        }
                        Err(e) => eprintln!(
                            "aterm-gui: sparkle_words.lexicon {expanded:?} rejected ({e}); \
                             skipping that layer"
                        ),
                    }
                }
                Err(error) => {
                    eprintln!(
                        "aterm-gui: sparkle_words.lexicon {expanded:?} unreadable ({error}); \
                         skipping that layer"
                    );
                }
            }
        }
        let prof = sw.profanity.clone().unwrap_or_default();
        let fel = sw.feline.clone().unwrap_or_default();
        let can_cfg = sw.canine.clone().unwrap_or_default();
        let orca_cfg = sw.orca.clone().unwrap_or_default();
        let emph = sw.emphasis.clone().unwrap_or_default();
        append_extra_words_entry(&mut out, "profanity", prof.extra_words.as_deref());
        append_extra_words_entry(&mut out, "feline", fel.extra_words.as_deref());
        append_extra_words_entry(&mut out, "canine", can_cfg.extra_words.as_deref());
        append_extra_words_entry(&mut out, "orca", orca_cfg.extra_words.as_deref());
        append_extra_words_entry(&mut out, "emphasis", emph.extra_words.as_deref());
        // Each strict pack was already production-scanner validated. Preserve
        // configured order so the lexicon document mirrors spec overlay order.
        out.push_str(pack_lexicon_toml);
        // v3 §6: custom spec words auto-append to the emphasis class (CJK
        // surfaces as `cjk = true` entries — the silent-drop fix), so they
        // actually scan; their per-word specs ride the resolved DecoConfig's
        // spec table.
        if let Some(customs) = sw.custom.as_deref() {
            let (_, fragment) = aterm_effects::spec::build_custom(customs);
            out.push_str(&fragment);
        }
        (!out.is_empty()).then_some(out)
    }

    /// The `[matrix_rain]` DURABLE enabled bit: `enabled = true` present in the
    /// table. Absent table / absent key ⇒ OFF (costume mode stays opt-in).
    /// Deliberately split from [`Config::matrix_rain_params`]: a per-session
    /// runtime override resolves `override.unwrap_or(THIS)`, so a session can
    /// force rain ON even when the config never enables it.
    pub(crate) fn matrix_rain_enabled(&self) -> bool {
        self.matrix_rain
            .as_ref()
            .and_then(|mr| mr.enabled)
            .unwrap_or(false)
    }

    /// The `[packages]` `auto_update` RESOLVED bit (default TRUE — today's 6h
    /// cadence). One resolver so the Settings switch seed, the loop gate, and
    /// the Packages page status card all read the same effective value.
    pub(crate) fn packages_auto_update(&self) -> bool {
        self.packages
            .as_ref()
            .and_then(|p| p.auto_update)
            .unwrap_or(true)
    }

    /// The `[packages]` background-maintenance master bit (default TRUE).
    /// Explicit package actions are still allowed while this is off; it gates
    /// only the launch-time automatic updater thread.
    pub(crate) fn packages_enabled(&self) -> bool {
        self.packages
            .as_ref()
            .and_then(|p| p.enabled)
            .unwrap_or(true)
    }

    /// The `[packages]` `auto_install` RESOLVED bit (default FALSE — multi-GB
    /// toolchains need explicit consent; the Settings switch IS the consent
    /// click, `docs/TOOLCHAIN-PACKAGE-MANAGER.md` §11). Consumed by the
    /// co-located `atpkg` from the same table; the GUI only displays/edits it.
    /// The `[packages]` `seed_install` RESOLVED bit (default TRUE).
    pub(crate) fn packages_seed_install(&self) -> bool {
        self.packages
            .as_ref()
            .and_then(|p| p.seed_install)
            .unwrap_or(true)
    }

    pub(crate) fn packages_auto_install(&self) -> bool {
        self.packages
            .as_ref()
            .and_then(|p| p.auto_install)
            .unwrap_or(false)
    }

    /// Whether the background TOOLS loop (`spawn_pkg_update_check` → the
    /// co-located `atpkg update`) runs at all: `[packages]` `enabled` (the
    /// master, default TRUE — today's behavior) AND `auto_update` (the 6h
    /// cadence flag, default TRUE). `auto_install` is deliberately NOT read
    /// here — the co-located `atpkg` reads the SAME `[packages]` table itself
    /// and applies the default-set bootstrap on its own `update` pass (one
    /// source of truth, no GUI copy to go stale). The interval keeps its
    /// `ATPKG_UPDATE_INTERVAL_SECS` env override, but the gate is config-only.
    pub(crate) fn packages_update_loop_enabled(&self) -> bool {
        let p = self.packages.clone().unwrap_or_default();
        // The remaining keys are atpkg's to consume (schema completeness here so
        // the whole table round-trips through this struct in tests) — same idiom
        // as the parsed-but-inert matrix-rain v1.1 keys.
        let _ = (
            p.auto_install,
            p.seed_install,
            p.account.as_deref(),
            p.channel.as_deref(),
            p.include.as_deref(),
            p.exclude.as_deref(),
        );
        self.packages_enabled() && p.auto_update.unwrap_or(true)
    }

    /// Resolve the `[matrix_rain]` table into engine-ready PARAMETERS
    /// ([`crate::matrix_rain::RainConfig`]), applying every default + clamp
    /// (design §12 — defaults/clamps live ONLY here; the engine re-clamps
    /// defensively). INDEPENDENT of the `enabled` bit: an absent/disabled
    /// table still synthesizes the full default + theme-derived parameter set,
    /// because a per-session override can force rain ON over a disabled config
    /// (the enabled decision lives in [`Config::matrix_rain_enabled`] +
    /// `App::session_rain_enabled`). The returned config always carries
    /// `enabled: true` — host-side gating decides whether an engine ever
    /// exists, and the zero-cost D-1 pin is preserved there (no engine is
    /// constructed while every visible session is off).
    ///
    /// `default_bg`/`theme_fg` are the live renderer theme (`0x00RR_GGBB`) —
    /// the ramp + §6 luminance derivation read them the way the renderer
    /// knows them.
    pub(crate) fn matrix_rain_params(
        &self,
        default_bg: u32,
        theme_fg: u32,
    ) -> crate::matrix_rain::RainConfig {
        use crate::matrix_rain::{RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR, RainHue};
        let mr = self.matrix_rain.clone().unwrap_or_default();
        // §6 alpha ceiling: the overrides land only when USER-set — `None`
        // keeps the engine's theme-derived luminance path. The head floor is
        // the resolved body alpha (heads never dimmer than the body).
        let alpha = mr
            .alpha
            .map(|a| a.clamp(u32::from(RAIN_ALPHA_FLOOR), u32::from(RAIN_ALPHA_CAP)) as u8);
        let head_floor = alpha.unwrap_or(RAIN_ALPHA_FLOOR);
        let head_alpha = mr
            .head_alpha
            .map(|a| (a.min(u32::from(RAIN_ALPHA_CAP)) as u8).max(head_floor));
        let hue = match mr.hue.as_deref().map(str::trim) {
            Some(h) if h.eq_ignore_ascii_case("theme") => RainHue::Theme,
            Some(h) if h.starts_with('#') => match parse_hex_color(h) {
                Some(c) => {
                    RainHue::Custom((u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b))
                }
                // Malformed hex fails CLOSED to the stock green — a typo must
                // never abort the whole config load or invent a colour.
                None => RainHue::Matrix,
            },
            _ => RainHue::Matrix,
        };
        // `materialize` is parsed but NOT forwarded in v1 — deferred, design §14.
        let _ = mr.materialize;
        // `ink_text` is parsed but NOT forwarded in v1 — v1.1, design §14.
        let _ = mr.ink_text;
        // `phosphor` is parsed but NOT forwarded in v1 — deferred, design §14.
        let _ = mr.phosphor;
        crate::matrix_rain::RainConfig {
            enabled: true,
            fps: mr.fps.unwrap_or(30).clamp(12, 60) as u8,
            density: mr.density.unwrap_or(6).clamp(1, 12) as u8,
            speed: mr.speed.unwrap_or(5).clamp(1, 10) as u8,
            trail: mr.trail.unwrap_or(5).clamp(1, 10) as u8,
            alpha_override: alpha,
            head_alpha_override: head_alpha,
            hue,
            mutation_ms: mr.mutation_ms.unwrap_or(133).clamp(80, 2000) as u16,
            idle_secs: mr.idle_secs.unwrap_or(8).clamp(2, 120) as u16,
            suppress_in_alt_screen: mr.suppress_in_alt_screen.unwrap_or(false),
            turn_wave: mr.turn_wave.unwrap_or(true),
            output_material: mr.output_material.unwrap_or(true),
            exit_tint: mr.exit_tint.unwrap_or(true),
            bell_alert: mr.bell_alert.unwrap_or(true),
            // 0 is the "derive per window at enable" sentinel — resolved to a
            // stable per-window seed at engine build (rain_config_for_window),
            // never wall-clock randomness.
            seed: mr.seed.unwrap_or(0),
            default_bg: default_bg & 0x00FF_FFFF,
            theme_fg: theme_fg & 0x00FF_FFFF,
        }
    }

    /// GPU cursor-comet bloom — the light CROWN around the comet head. DEFAULT ON,
    /// paired with whatever `cursor_trail` resolves to: with the comet's continuous
    /// beam this is the shipped "luminous streak" signature. The cost (a half-res
    /// blur pass) runs only on effect frames, which the present-paced pump drives
    /// at the display rate with ~0.2ms frame cost (measured, AMD 780M iGPU); the
    /// `perf_reduced` load-shed latch and `motion` policy both drop it under
    /// pressure/accessibility, and `cursor_trail_bloom = false` opts out.
    ///
    /// ON EVERY PLATFORM, Windows included, and deliberately NOT a member of the
    /// [`DEFAULT_DECORATIVE_EFFECTS`] family: every consumer reads this as
    /// `cursor_trail_or_default() && …`, so the Windows trail default already makes it
    /// inert on a fresh config. Splitting it too would buy no pixel and no
    /// millisecond there, and would hand the Windows owner who writes
    /// `cursor_trail = true` a trail missing its crown.
    pub(crate) fn cursor_trail_bloom_or_default(&self) -> bool {
        self.cursor_trail_bloom.unwrap_or(true)
    }

    /// Bloom strength, default 0.85, clamped 0.0..=3.0. Non-finite (`nan` is
    /// valid TOML) FAILS OFF to 0.0 — `clamp` passes NaN through, and these
    /// values feed GPU shader params directly (the intensity resolver's twin).
    pub(crate) fn cursor_trail_bloom_strength_or_default(&self) -> f32 {
        let v = self.cursor_trail_bloom_strength.unwrap_or(0.85);
        if v.is_finite() {
            v.clamp(0.0, 3.0)
        } else {
            0.0
        }
    }

    /// Bloom radius (half-res texels), default 2.2, clamped 0.5..=8.0.
    /// Non-finite falls back to the default (a NaN radius would poison the
    /// GPU blur weights; the strength resolver owns the fail-OFF arm).
    pub(crate) fn cursor_trail_bloom_radius_or_default(&self) -> f32 {
        let v = self.cursor_trail_bloom_radius.unwrap_or(2.2);
        if v.is_finite() {
            v.clamp(0.5, 8.0)
        } else {
            2.2
        }
    }

    /// GPU heat shimmer above burning cells — the bloom's parity class.
    /// DEFAULT ON (quality-first): it costs one small staged copy + one
    /// scissored pass, only on frames that carry glow quads. The `perf_reduced`
    /// load-shed latch drops it exactly as it drops the bloom, and
    /// reduced-motion / unfocused windows emit no glow quads ⇒ no hot region ⇒
    /// no shimmer automatically. `cursor_fire_shimmer = false` opts out.
    pub(crate) fn cursor_fire_shimmer_or_default(&self) -> bool {
        self.cursor_fire_shimmer.unwrap_or(true)
    }

    /// M3 phase B: whether the EDR cursor glow is opted in. DEFAULT ON — on an
    /// HDR-enabled desktop (Windows scRGB DX12 / macOS EDR) the aurora gets real
    /// headroom and the streak becomes genuinely luminous; on an SDR desktop the
    /// sanitized headroom is 0 and the EDR pass is PROVABLY INERT (the present
    /// stays byte-identical), so the default is safe everywhere and lights up
    /// exactly where the panel can show it. `hdr_glow = false` opts out.
    pub(crate) fn hdr_glow_or_default(&self) -> bool {
        self.hdr_glow.unwrap_or(true)
    }

    /// SDR glow-boost strength (`cursor_glow_sdr_boost`), DEFAULT 0.25, clamped
    /// 0..=1; `0` disables. The renderer combines it with the live background's
    /// luma into a proven-bounded budget (`aterm_render::hdr::sdr_glow_budget`)
    /// and eases it in with a ~45ms attack, so light themes self-degrade to
    /// invisible regardless of this knob and onsets BLOOM rather than strobe.
    /// NaN fails OFF inside the budget math. (Was 0.35 with instant onset — the
    /// owner reported it as a per-keystroke "cursor flash".)
    pub(crate) fn cursor_glow_sdr_boost_or_default(&self) -> f32 {
        self.cursor_glow_sdr_boost.unwrap_or(0.25).clamp(0.0, 1.0)
    }

    /// Build the renderer text-shaping config from the typography keys
    /// (`ligatures` + `cursor_break_ligatures` + `font_features`). The DEFAULT
    /// (`ligatures` absent/`true`, no break, no `font_features`) returns
    /// `TextShapingConfig::default()` — `LigatureMode::Enabled`
    /// with only the base `liga`+`calt` — so an unset config is byte-identical to the
    /// pre-WIRE-FONTFEAT renderer. `cursor_break_ligatures = true` maps to
    /// `LigatureMode::CursorDisabled` (W5d — the cursor cell renders per-cell);
    /// `ligatures = false` wins over it (everything is per-cell already).
    /// `ambiguous_width` is intentionally NOT mapped here:
    /// it is routed through the engine cell-width path (`ambiguous_width_double`),
    /// which the renderer's `TextShapingConfig.ambiguous_width` field does not drive.
    pub(crate) fn text_shaping(&self) -> aterm_render::TextShapingConfig {
        use aterm_types::text_shaping::{
            FontFeature, FontFeatureSet, LigatureMode, parse_font_features,
        };
        let ligature_mode = match (self.ligatures, self.cursor_break_ligatures) {
            (Some(false), _) => LigatureMode::Disabled,
            (_, Some(true)) => LigatureMode::CursorDisabled,
            _ => LigatureMode::Enabled,
        };
        // Each list entry may itself be a space-separated group; parse them all and
        // flatten onto the PRIMARY face (`font_id == 0`, the only face the shaper
        // drives). Malformed tags are dropped by the parser.
        let features: Vec<FontFeature> = self
            .font_features
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .flat_map(|spec| parse_font_features(spec))
            .collect();
        let font_features = if features.is_empty() {
            Vec::new()
        } else {
            vec![FontFeatureSet {
                font_id: 0,
                features,
            }]
        };
        aterm_render::TextShapingConfig {
            ligature_mode,
            font_features,
            // M4: opt-in Cascadia N:1 merged-ligature slicing (default off).
            admit_collapsed: self.merged_ligatures.unwrap_or(false),
            ..Default::default()
        }
    }

    /// Coverage blend mode (W2): `"linear-corrected"` (the DEFAULT, also for an
    /// absent key) or `"linear"`; case-insensitive, with `linear_corrected`
    /// accepted as an alias. An unknown value warns and keeps the default —
    /// same fail-safe shape as `cursor_style`/`window_theme`.
    pub(crate) fn text_blending_or_default(&self) -> aterm_render::TextBlending {
        match self.text_blending.as_deref().map(str::trim) {
            None => aterm_render::TextBlending::LinearCorrected,
            Some(s) if s.eq_ignore_ascii_case("linear") => aterm_render::TextBlending::Linear,
            Some(s)
                if s.eq_ignore_ascii_case("linear-corrected")
                    || s.eq_ignore_ascii_case("linear_corrected") =>
            {
                aterm_render::TextBlending::LinearCorrected
            }
            Some(other) => {
                eprintln!(
                    "aterm-gui: unknown text_blending {other:?} (expected \"linear\" or \
                     \"linear-corrected\"); using linear-corrected"
                );
                aterm_render::TextBlending::LinearCorrected
            }
        }
    }

    /// macOS `font_thicken` (CoreText font smoothing at raster time). Default OFF.
    pub(crate) fn font_thicken_or_default(&self) -> bool {
        self.font_thicken.unwrap_or(false)
    }

    /// The W9 variable-font requests: parsed `font_variation` entries with
    /// `font_weight` appended LAST (later wins in the renderer's overlay, so
    /// the discoverable key beats a conflicting `wght=` entry). Malformed
    /// entries are collected into the returned warnings (config-notice
    /// banner, like the other font keys) and skipped — never a hard failure.
    pub(crate) fn font_variation_requests(&self) -> (Vec<(u32, f32)>, Vec<String>) {
        let mut warns = Vec::new();
        let mut out: Vec<(u32, f32)> = self
            .font_variation
            .as_ref()
            .map_or(&[][..], |l| l.0.as_slice())
            .iter()
            .filter_map(|spec| {
                let parsed = aterm_render::variation::parse_variation_spec(spec);
                if parsed.is_none() {
                    warns.push(format!(
                        "config font_variation: ignored malformed entry {spec:?} \
                         (use \"tag=value\", e.g. \"wght=450\")"
                    ));
                }
                parsed
            })
            .collect();
        if let Some(w) = self.font_weight {
            out.push((aterm_render::variation::WGHT_TAG, w.clamp(1, 1000) as f32));
        }
        (out, warns)
    }

    /// `font_weight_dark_nudge` (W9 moonshot), default 0 (off), clamped
    /// 0..=300; non-finite values fall back to off.
    pub(crate) fn font_weight_dark_nudge_or_default(&self) -> f32 {
        match self.font_weight_dark_nudge {
            Some(v) if v.is_finite() => v.clamp(0.0, 300.0),
            _ => 0.0,
        }
    }

    /// EFFECTIVE stem gamma with the startup precedence every key follows:
    /// `$ATERM_STEM_GAMMA` (the historical env knob, now an alias) > the
    /// `stem_gamma` config key > `1.0` (identity). Clamped to the renderer's
    /// `0.30..=3.0`; a non-finite value falls back to the identity.
    pub(crate) fn stem_gamma_or_default(&self) -> f32 {
        let g = std::env::var("ATERM_STEM_GAMMA")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|g| g.is_finite())
            .or(self.stem_gamma)
            .unwrap_or(1.0);
        aterm_render::clamp_stem_gamma(g)
    }

    /// EFFECTIVE native grid-fitting mode with the startup precedence every key
    /// follows: `$ATERM_FONT_HINTING` (the historical env knob, now an alias)
    /// wins over the `font_hinting` config key, which wins over `"full"`. The
    /// renderer's own parser resolves unrecognized spellings to the default, so
    /// this stays a plain string hand-off (the setter is the single source of
    /// spelling truth).
    pub(crate) fn font_hinting_or_default(&self) -> String {
        std::env::var("ATERM_FONT_HINTING")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| self.font_hinting.clone())
            .unwrap_or_else(|| "full".to_string())
    }

    /// EFFECTIVE Linux subpixel-RGB mode with the startup precedence every key
    /// follows: `$ATERM_FONT_SUBPIXEL` (the env alias) wins over the
    /// `font_subpixel` config key, which wins over `"off"`. The renderer's own
    /// parser resolves unrecognized spellings to the default, so this stays a
    /// plain string hand-off (the setter is the single source of spelling
    /// truth) — the `font_hinting` discipline exactly.
    pub(crate) fn font_subpixel_or_default(&self) -> String {
        std::env::var("ATERM_FONT_SUBPIXEL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| self.font_subpixel.clone())
            .unwrap_or_else(|| "off".to_string())
    }

    /// Line-height multiplier (W5a), default `1.0`, clamped to the sane
    /// 0.8..=2.0 (a typo can't collapse or explode the grid; the renderer
    /// additionally floors at 0.5). Non-finite values fall back to `1.0`.
    pub(crate) fn line_height_or_default(&self) -> f32 {
        match self.line_height {
            Some(v) if v.is_finite() => v.clamp(0.8, 2.0),
            _ => 1.0,
        }
    }

    /// Baseline shift in px (W5a), default `0`, clamped ±32 (the renderer
    /// clamps again at ±64).
    pub(crate) fn adjust_baseline_or_default(&self) -> i32 {
        self.adjust_baseline
            .map_or(0i64, |v| v.clamp(-32, 32))
            .try_into()
            .unwrap_or(0)
    }

    /// Underline position shift in px (W7), default `0`, clamped ±32 (the
    /// renderer clamps again at ±64 and re-clamps the band in-cell).
    pub(crate) fn adjust_underline_position_or_default(&self) -> i32 {
        self.adjust_underline_position
            .map_or(0i64, |v| v.clamp(-32, 32))
            .try_into()
            .unwrap_or(0)
    }

    /// Underline thickness delta in px (W7), default `0`, clamped ±32 (the
    /// renderer clamps again at ±64 and floors the thickness at 1px).
    pub(crate) fn adjust_underline_thickness_or_default(&self) -> i32 {
        self.adjust_underline_thickness
            .map_or(0i64, |v| v.clamp(-32, 32))
            .try_into()
            .unwrap_or(0)
    }

    /// Descender ink-skip (W7), default `true` (on).
    pub(crate) fn underline_skip_descenders_or_default(&self) -> bool {
        self.underline_skip_descenders.unwrap_or(true)
    }

    /// Per-cell minimum contrast ratio (W5b), default `1.0` (off — xterm
    /// treats 1 as "do nothing"), clamped to the WCAG 1.0..=21.0 domain.
    pub(crate) fn minimum_contrast_or_default(&self) -> f32 {
        match self.minimum_contrast {
            Some(v) if v.is_finite() => v.clamp(1.0, 21.0),
            _ => 1.0,
        }
    }

    /// The explicit selected-text foreground as packed `0x00RRGGBB`, if
    /// `selection_foreground` is set and parses. `None` → the renderer's WCAG
    /// contrast-floor default.
    pub(crate) fn selection_foreground_u32(&self) -> Option<u32> {
        self.selection_foreground
            .as_deref()
            .and_then(parse_hex_color)
            .map(|c| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b))
    }

    /// Whether the selection band dims while the window is unfocused (W5c).
    /// Default OFF (byte-identical to the pre-W5 desktop, which never wired
    /// the renderer's inactive-selection path).
    pub(crate) fn selection_inactive_or_default(&self) -> bool {
        self.selection_inactive.unwrap_or(false)
    }

    /// SGR 2 faint opacity (W5e), default `0.5`, clamped 0.0..=1.0.
    pub(crate) fn faint_opacity_or_default(&self) -> f32 {
        match self.faint_opacity {
            Some(v) if v.is_finite() => v.clamp(0.0, 1.0),
            _ => aterm_types::DIM_FACTOR,
        }
    }

    /// M5 window background opacity, default `1.0` (solid — byte-identical),
    /// clamped to the 0.0..=1.0 domain. A non-finite value fails safe to solid.
    pub(crate) fn background_opacity_or_default(&self) -> f32 {
        match self.background_opacity {
            Some(v) if v.is_finite() => v.clamp(0.0, 1.0),
            _ => 1.0,
        }
    }

    /// M5 vibrancy material, default [`BackgroundMaterial::None`]. Parsed
    /// case-insensitively; an unrecognized value warns once (like themes) and
    /// falls back to `None`.
    pub(crate) fn background_material_or_default(&self) -> BackgroundMaterial {
        match self.background_material.as_deref() {
            None => BackgroundMaterial::None,
            Some(s) => BackgroundMaterial::parse(s).unwrap_or_else(|| {
                eprintln!(
                    "aterm-gui: config background_material: expected none|hud|sidebar|\
                     under-window, got {s:?}; using none"
                );
                BackgroundMaterial::None
            }),
        }
    }

    /// All-edge interior window padding in LOGICAL px (`window_padding`),
    /// default [`crate::PAD_LOGICAL_PX`] (12.0), clamped 0..=[`MAX_WINDOW_PADDING_PX`]
    /// so a config typo can never push the grid off the glass. Non-finite
    /// (`nan` is valid TOML) falls back to the default — NaN would poison the
    /// `round(pad·scale)` derivation every window metric flows through.
    pub(crate) fn window_padding_or_default(&self) -> f32 {
        match self.window_padding {
            Some(v) if v.is_finite() => v.clamp(0.0, MAX_WINDOW_PADDING_PX),
            _ => crate::PAD_LOGICAL_PX,
        }
    }

    /// TOP-edge padding override in logical px (`window_padding_top`), default
    /// [`crate::PAD_TOP_LOGICAL_PX`] (2.0). Clamped to `0..=window_padding` —
    /// the renderer's `set_pad_top` enforces `pad_top <= pad` in device px
    /// (aterm-render), so the resolver keeps the LOGICAL value inside the same
    /// domain and the two clamps can never disagree. The default is also capped
    /// by the resolved base pad, so `window_padding = 1` alone yields a valid
    /// 1/1 pair rather than a 2-over-1 top.
    pub(crate) fn window_padding_top_or_default(&self) -> f32 {
        let pad = self.window_padding_or_default();
        match self.window_padding_top {
            Some(v) if v.is_finite() => v.clamp(0.0, pad),
            _ => crate::PAD_TOP_LOGICAL_PX.min(pad),
        }
    }

    /// THE MOVE — the EFFECTIVE per-cell minimum contrast the renderer is handed
    /// (M5 legibility guarantee). Whenever the window is translucent
    /// (`background_opacity < 1.0`) the floor auto-engages at WCAG AA (4.5:1),
    /// raising the user's configured `minimum_contrast` if it sits lower — glass
    /// that cannot make text illegible (unlike iTerm2's blur / ghostty's
    /// `background-blur`, which let text sink into the desktop). An opaque window
    /// keeps EXACTLY the configured floor. PURE (config → f32): exhaustively
    /// proven by `vibrancy_contrast_guarantee` (Tier-1) and model-checked by
    /// `aterm_spec::derive::vibrancy_contrast_model` (Tier-0, `NeverIllegible`).
    pub(crate) fn effective_minimum_contrast(&self) -> f32 {
        let base = self.minimum_contrast_or_default();
        if aterm_render::vibrancy::is_translucent(self.background_opacity_or_default()) {
            base.max(VIBRANCY_CONTRAST_FLOOR)
        } else {
            base
        }
    }

    /// Warn (like themes) when a configured `font_family` does not RESOLVE to a
    /// font file — previously a misspelled family silently reduced to the
    /// built-in candidates with zero output and `--validate-config`
    /// false-greened (W5h). Returns the message so startup/reload can both
    /// print it AND surface it in the config-notice banner. `family` is the
    /// EFFECTIVE family (env `$ATERM_FONT` > config), matching what the
    /// backend will actually try.
    pub(crate) fn font_family_warning(family: Option<&str>) -> Option<String> {
        let fam = family.map(str::trim).filter(|s| !s.is_empty())?;
        match aterm_render::resolve_config_font(fam) {
            Ok(_) => None,
            Err(error) => Some(format!(
                "font_family {fam:?} is not an admissible font ({error}); keeping the current \
                 working font (see `aterm list-fonts` for resolvable families)"
            )),
        }
    }

    /// Warn (the `font_family_warning` twin) when a configured
    /// `cursor_trail_style` is a spelling the engine does not recognize — the
    /// `glow_config` enablement gate silently DISABLES the whole cursor effect
    /// for an unknown value (a typo'd `"phasr"` just makes the trail vanish),
    /// and `--validate-config` previously false-greened it. Checked against the
    /// canonical option set + the documented alias table
    /// ([`crate::prefs::cursor_trail_style_canonical`]) — the same source the
    /// Settings picker resolves through. Returns the message so startup, reload,
    /// AND `--validate-config` surface the identical diagnostic.
    pub(crate) fn cursor_trail_style_warning(&self, catalog: &TrailPackCatalog) -> Option<String> {
        let raw = self.cursor_trail_style.as_deref().map(str::trim)?;
        match resolve_trail_style(raw, catalog).issue {
            None => None,
            Some(TrailStyleIssue::MissingPack | TrailStyleIssue::EmptyPackId) => {
                let id = raw.strip_prefix("pack:").unwrap_or_default().trim();
                Some(format!(
                    "cursor_trail_style: no Trail Pack with id {id:?} is loaded — the cursor \
                     effect is disabled; add its manifest to cursor_trail_packs = [\"…\"]"
                ))
            }
            Some(TrailStyleIssue::Unknown) => Some(format!(
                "cursor_trail_style: unknown style {raw:?} — the cursor effect is disabled; \
                 expected one of phaser|rainbow kitty|comet|lumen|sparkle|fire|laser|water|beam|off \
                 (or a documented alias like nyan rainbow/nyan/rainbow/ember/ocean), or pack:<id> \
                 for a loaded Trail Pack"
            )),
        }
    }

    /// Whether to show the OPTIONAL floating top-right build/version pill. Default OFF:
    /// the version now lives in the menu bar (the top-level `v<version>` menu, which
    /// opens About), so the floating pill is an opt-in extra rather than the primary
    /// surface. Enable with `show_build_badge = true` or the native Settings tab. See
    /// [`crate::build_badge`].
    pub(crate) fn show_build_badge_or_default(&self) -> bool {
        self.show_build_badge.unwrap_or(false)
    }

    /// Resolve the window-chrome appearance ([`WindowTheme`]) from config. The
    /// DEFAULT when the key is absent is [`WindowTheme::Auto`] — follow the OS
    /// effective appearance — so an unset config no longer forces dark chrome on a
    /// light desktop. An unknown / malformed value warns and falls back to `Auto`.
    pub(crate) fn window_theme_or_default(&self) -> WindowTheme {
        match self.window_theme.as_deref() {
            None => WindowTheme::Auto,
            Some(s) => match WindowTheme::parse(s) {
                Some(t) => t,
                None => {
                    eprintln!(
                        "aterm-gui: config window_theme: expected auto|light|dark, got {s:?}; using auto"
                    );
                    WindowTheme::Auto
                }
            },
        }
    }

    /// Resolve the grid right-click gesture ([`RightClickGesture`]) from config
    /// `right_click`. The DEFAULT when the key is absent is PER-PLATFORM
    /// ([`RightClickGesture::PLATFORM_DEFAULT`]): `copy_paste` on Windows, `off`
    /// elsewhere. An unknown / malformed value warns and falls back to that same
    /// platform default (the `window_theme` fail-safe shape).
    pub(crate) fn right_click_or_default(&self) -> RightClickGesture {
        match self.right_click.as_deref() {
            None => RightClickGesture::PLATFORM_DEFAULT,
            Some(s) => match RightClickGesture::parse(s) {
                Some(g) => g,
                None => {
                    eprintln!(
                        "aterm-gui: config right_click: expected copy_paste|off, got {s:?}; using the platform default"
                    );
                    RightClickGesture::PLATFORM_DEFAULT
                }
            },
        }
    }

    /// Resolve the tab-context-menu KEYBOARD chord policy ([`TabMenuChord`])
    /// from config `tab_menu_chord`. The DEFAULT when the key is absent is
    /// [`TabMenuChord::On`] — both Windows spellings, which is what an unedited
    /// config has always meant. An unknown / malformed value warns and falls
    /// back to that default (the `right_click` fail-safe shape).
    ///
    /// Read only by the `#[cfg(windows)]` chord arms (`on_key`'s and the
    /// convergence seam's) and by this file's own tests, so off Windows it is a
    /// live-but-uncalled resolver rather than a missing one — the config key
    /// still parses and validates everywhere, exactly like `right_click`'s.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn tab_menu_chord_or_default(&self) -> TabMenuChord {
        match self.tab_menu_chord.as_deref() {
            None => TabMenuChord::On,
            Some(s) => match TabMenuChord::parse(s) {
                Some(g) => g,
                None => {
                    eprintln!(
                        "aterm-gui: config tab_menu_chord: expected on|menu_key|off, got {s:?}; using on"
                    );
                    TabMenuChord::On
                }
            },
        }
    }

    /// Resolve the GPU-present colour-space tag ([`WindowColorspace`]) from config
    /// `window_colorspace`. The DEFAULT when the key is absent is
    /// [`WindowColorspace::Srgb`] — the colour-managed interpretation. An unknown /
    /// malformed value warns and falls back to `Srgb` (the `window_theme`
    /// fail-safe shape).
    pub(crate) fn window_colorspace_or_default(&self) -> WindowColorspace {
        match self.window_colorspace.as_deref() {
            None => WindowColorspace::Srgb,
            Some(s) => match WindowColorspace::parse(s) {
                Some(c) => c,
                None => {
                    eprintln!(
                        "aterm-gui: config window_colorspace: expected srgb|display-p3, got {s:?}; using srgb"
                    );
                    WindowColorspace::Srgb
                }
            },
        }
    }

    /// C3: resolve the in-grid tab band's height policy ([`TabBandHeight`]) from
    /// config `tab_band_height`. The DEFAULT when the key is absent is PER-PLATFORM
    /// ([`TabBandHeight::PLATFORM_DEFAULT`]): `standard` on Windows and Linux,
    /// `compact` elsewhere. An unknown / malformed value warns and falls back to
    /// that same platform default (the `window_theme` fail-safe shape).
    pub(crate) fn tab_band_height_or_default(&self) -> TabBandHeight {
        match self.tab_band_height.as_deref() {
            None => TabBandHeight::PLATFORM_DEFAULT,
            Some(s) => match TabBandHeight::parse(s) {
                Some(h) => h,
                None => {
                    eprintln!(
                        "aterm-gui: config tab_band_height: expected compact|standard, got {s:?}; using the platform default"
                    );
                    TabBandHeight::PLATFORM_DEFAULT
                }
            },
        }
    }
}

/// The RENDERER-side typography/appearance knobs a config resolves to (W5):
/// the single value-of-record the App caches, the startup seed and every
/// hot-reload diff against. PURE (`from_config` + `diff`), so the
/// key→renderer-call routing is unit-provable without a backend: each config
/// key maps to EXACTLY ONE [`KnobChange`] variant, and each variant to exactly
/// one renderer call (see `reload_config` / `rebuild_backend`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct RenderKnobs {
    /// `line_height` → `Backend::set_line_height` (cell GEOMETRY: re-grids).
    pub(crate) line_height: f32,
    /// `minimum_contrast` → `Backend::set_minimum_contrast` (blit-time).
    pub(crate) minimum_contrast: f32,
    /// `selection_foreground` → `Backend::set_selection_fg` (blit-time).
    pub(crate) selection_fg: Option<u32>,
    /// `selection_inactive` → `Backend::set_selection_inactive`, folded with
    /// the live focus at the per-window present seam (`redraw_window`).
    pub(crate) selection_inactive: bool,
    /// `adjust_baseline` → `Backend::set_adjust_baseline` (placement-only).
    pub(crate) adjust_baseline: i32,
    /// `adjust_underline_position` + `adjust_underline_thickness` →
    /// `Backend::set_adjust_underline` (draw-time, W7). One knob pair, one call.
    pub(crate) adjust_underline: (i32, i32),
    /// `underline_skip_descenders` → `Backend::set_underline_skip_descenders`
    /// (draw-time, W7, default ON).
    pub(crate) underline_skip_descenders: bool,
    /// `background_opacity` → `Backend::set_background_opacity` (present-time,
    /// M5). `1.0` = solid (byte-identical default).
    pub(crate) background_opacity: f32,
    /// `background_material` (M5): resolved, validated and diffed before driving
    /// `AppRt::window_set_vibrancy`. macOS GPU installs a behind-window effect only
    /// while translucent; Windows GPU installs a DWM backdrop independently. CPU
    /// and unsupported-platform paths diagnose the setting as inert.
    pub(crate) background_material: BackgroundMaterial,
}

/// One changed renderer knob from a config hot-reload — routed to exactly one
/// renderer call. See [`RenderKnobs::diff`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum KnobChange {
    LineHeight(f32),
    MinimumContrast(f32),
    SelectionFg(Option<u32>),
    SelectionInactive(bool),
    AdjustBaseline(i32),
    AdjustUnderline(i32, i32),
    UnderlineSkipDescenders(bool),
    BackgroundOpacity(f32),
    BackgroundMaterial(BackgroundMaterial),
}

impl RenderKnobs {
    /// Resolve the knob values from a config (defaults are the byte-identical
    /// pre-W5 renderer state).
    pub(crate) fn from_config(cfg: &Config) -> Self {
        Self {
            line_height: cfg.line_height_or_default(),
            // THE MOVE: the renderer is handed the EFFECTIVE floor — auto-raised
            // to WCAG AA whenever the window is translucent (M5). An opaque
            // window resolves to exactly the configured `minimum_contrast`.
            minimum_contrast: cfg.effective_minimum_contrast(),
            selection_fg: cfg.selection_foreground_u32(),
            selection_inactive: cfg.selection_inactive_or_default(),
            adjust_baseline: cfg.adjust_baseline_or_default(),
            adjust_underline: (
                cfg.adjust_underline_position_or_default(),
                cfg.adjust_underline_thickness_or_default(),
            ),
            underline_skip_descenders: cfg.underline_skip_descenders_or_default(),
            background_opacity: cfg.background_opacity_or_default(),
            background_material: cfg.background_material_or_default(),
        }
    }

    /// The changes from `self` to `new`, one [`KnobChange`] per changed knob
    /// (an unchanged knob emits nothing, so a metadata-only reload is free).
    pub(crate) fn diff(&self, new: &Self) -> Vec<KnobChange> {
        let mut out = Vec::new();
        if (self.line_height - new.line_height).abs() > f32::EPSILON {
            out.push(KnobChange::LineHeight(new.line_height));
        }
        if (self.minimum_contrast - new.minimum_contrast).abs() > f32::EPSILON {
            out.push(KnobChange::MinimumContrast(new.minimum_contrast));
        }
        if self.selection_fg != new.selection_fg {
            out.push(KnobChange::SelectionFg(new.selection_fg));
        }
        if self.selection_inactive != new.selection_inactive {
            out.push(KnobChange::SelectionInactive(new.selection_inactive));
        }
        if self.adjust_baseline != new.adjust_baseline {
            out.push(KnobChange::AdjustBaseline(new.adjust_baseline));
        }
        if self.adjust_underline != new.adjust_underline {
            out.push(KnobChange::AdjustUnderline(
                new.adjust_underline.0,
                new.adjust_underline.1,
            ));
        }
        if self.underline_skip_descenders != new.underline_skip_descenders {
            out.push(KnobChange::UnderlineSkipDescenders(
                new.underline_skip_descenders,
            ));
        }
        if (self.background_opacity - new.background_opacity).abs() > f32::EPSILON {
            out.push(KnobChange::BackgroundOpacity(new.background_opacity));
        }
        if self.background_material != new.background_material {
            out.push(KnobChange::BackgroundMaterial(new.background_material));
        }
        out
    }
}

impl Default for RenderKnobs {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

/// The RESOLVED per-style / fallback font config (W6) — file paths, not family
/// names — the value-of-record the App caches, the startup seed and every
/// hot-reload diff against (like [`RenderKnobs`]). PURE resolution
/// ([`Self::from_config`]) so the key→path mapping is unit-testable; the one
/// impure step (family → file via [`aterm_render::resolve_font_family`]) is the
/// same resolver `font_family` uses. Applied to the backend by
/// [`crate::apply_font_config_to_backend`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct FontConfig {
    /// Resolved styled-face file paths `[bold, italic, bold-italic]`
    /// (`font_family_bold` / `_italic` / `_bold_italic`). `None` = unset or
    /// unresolvable (warned) — the renderer keeps discovery/synthetic.
    pub(crate) styled_paths: [Option<String>; 3],
    /// `font_synthetic_style` (default `true`).
    pub(crate) synthetic_style: bool,
    /// Resolved `fallback_fonts` paths, in config order (unresolvable entries
    /// warned + dropped, the rest still apply).
    pub(crate) fallback_fonts: Vec<String>,
    /// Resolved `symbol_font` path.
    pub(crate) symbol_font: Option<String>,
    /// Resolved `emoji_font` path.
    pub(crate) emoji_font: Option<String>,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            styled_paths: [None, None, None],
            synthetic_style: true,
            fallback_fonts: Vec::new(),
            symbol_font: None,
            emoji_font: None,
        }
    }
}

impl FontConfig {
    /// Resolve the W6 font keys from a config. Returns the resolved paths plus
    /// human warnings for entries that do not resolve to a font file (surfaced
    /// like [`Config::font_family_warning`] — never a hard failure; the
    /// unresolvable entry is skipped and everything else still applies). Also
    /// emits the once-per-process deprecation notice for the legacy
    /// `$ATERM_{FALLBACK,SYMBOL,EMOJI}_FONT` env aliases when they are set.
    pub(crate) fn from_config(cfg: &Config) -> (Self, Vec<String>) {
        warn_deprecated_font_env_aliases_once();
        let mut warns = Vec::new();
        let mut resolve = |key: &str, fam: Option<&str>| -> Option<String> {
            let fam = fam.map(str::trim).filter(|s| !s.is_empty())?;
            match aterm_render::resolve_config_font(fam) {
                Ok(path) => Some(path),
                Err(error) => {
                    warns.push(format!(
                        "config {key}: {fam:?} is not an admissible font ({error}); ignored \
                         (see `aterm list-fonts`)"
                    ));
                    None
                }
            }
        };
        let styled_paths = [
            resolve("font_family_bold", cfg.font_family_bold.as_deref()),
            resolve("font_family_italic", cfg.font_family_italic.as_deref()),
            resolve(
                "font_family_bold_italic",
                cfg.font_family_bold_italic.as_deref(),
            ),
        ];
        let fallback_fonts: Vec<String> = cfg
            .fallback_fonts
            .as_ref()
            .map(|l| l.0.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|f| resolve("fallback_fonts", Some(f)))
            .collect();
        let symbol_font = resolve("symbol_font", cfg.symbol_font.as_deref());
        let emoji_font = resolve("emoji_font", cfg.emoji_font.as_deref());
        (
            Self {
                styled_paths,
                synthetic_style: cfg.font_synthetic_style.unwrap_or(true),
                fallback_fonts,
                symbol_font,
                emoji_font,
            },
            warns,
        )
    }

    /// Preserve the last working live face for an AUTHORED entry which failed
    /// resolution/admission in `from_config`.
    ///
    /// `None` normally means “unset; restore discovery”. For an authored key,
    /// however, `None` means the new value was rejected. Treating those two cases
    /// alike would let an oversized/FIFO/missing edit clobber a working styled,
    /// symbol, or emoji face. Valid siblings still advance independently.
    #[cfg(test)]
    pub(crate) fn preserve_rejected_from(&mut self, cfg: &Config, previous: &Self) {
        fn preserve(authored: Option<&str>, next: &mut Option<String>, old: &Option<String>) {
            if authored.is_some_and(|value| !value.trim().is_empty()) && next.is_none() {
                next.clone_from(old);
            }
        }
        preserve(
            cfg.font_family_bold.as_deref(),
            &mut self.styled_paths[0],
            &previous.styled_paths[0],
        );
        preserve(
            cfg.font_family_italic.as_deref(),
            &mut self.styled_paths[1],
            &previous.styled_paths[1],
        );
        preserve(
            cfg.font_family_bold_italic.as_deref(),
            &mut self.styled_paths[2],
            &previous.styled_paths[2],
        );
        if cfg
            .fallback_fonts
            .as_ref()
            .is_some_and(|fonts| !fonts.0.is_empty())
            && self.fallback_fonts.is_empty()
        {
            self.fallback_fonts.clone_from(&previous.fallback_fonts);
        }
        if cfg
            .symbol_font
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && self.symbol_font.is_none()
        {
            self.symbol_font.clone_from(&previous.symbol_font);
        }
        if cfg
            .emoji_font
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && self.emoji_font.is_none()
        {
            self.emoji_font.clone_from(&previous.emoji_font);
        }
    }
}

/// Deprecation notice (once per process) for the legacy env-only fallback-font
/// knobs, now that the visible TOML keys exist (W6). The aliases keep working
/// — they rank BELOW explicit config entries and ABOVE discovery (the
/// renderer's proven `fallback_chain_order`) — this only nudges toward the
/// discoverable keys.
/// A titlebar band larger than this is a mid-transition artifact, never real
/// chrome: plain titlebars are ~22–40 pt and unified-toolbar bands ~52–62 pt,
/// while transition artifacts measure in the hundreds (screen-vs-window frame
/// disagreement).
const TITLEBAR_BAND_SANITY_CAP_PTS: f64 = 120.0;

/// Fail-closed acceptance for one AppKit titlebar-band sample (pure — see the
/// call site's ACCEPTANCE note). Fullscreen FORCES 0 (the titlebar detaches;
/// any other reading is a mid-transition artifact). A decorated windowed
/// sample of `<= 0` or beyond [`TITLEBAR_BAND_SANITY_CAP_PTS`] keeps the
/// previously accepted band — chrome'd windows always carry a real band, so
/// those readings can only be transition artifacts, and committing one is
/// what drew terminal row 0 under the toolbar chip (or starved fullscreen
/// rows above a dead band). An UNDECORATED windowed sample of 0 is truth,
/// not artifact, and is accepted — runtime decoration toggles must not pin a
/// stale band.
fn accept_titlebar_band_pts(
    measured_pts: f64,
    fullscreen: bool,
    decorated: bool,
    prev_pts: f64,
) -> f64 {
    if fullscreen {
        return 0.0;
    }
    if !decorated {
        return measured_pts.max(0.0);
    }
    if !titlebar_band_sane(measured_pts) {
        return prev_pts;
    }
    measured_pts
}

/// Whether a titlebar-band sample is a plausible real band (positive, within
/// the sanity cap) — the ONE predicate `accept_titlebar_band_pts`'s commit
/// path and `titlebar_band_decision`'s memory update share, so "commits" and
/// "becomes the memory" can never drift apart.
fn titlebar_band_sane(pts: f64) -> bool {
    pts > 0.0 && pts <= TITLEBAR_BAND_SANITY_CAP_PTS
}

/// The full head decision for one titlebar-band sample:
/// `(measured, fullscreen, decorated, memory) -> (applied, new_memory)`.
///
/// `applied` is [`accept_titlebar_band_pts`] with the last-good-windowed
/// MEMORY as its fallback — NOT the currently applied band. The applied band
/// is 0 for the whole of a fullscreen stay (forced, by design), so using it
/// as the fallback would leave an exit-transition artifact sample nothing to
/// restore but 0; the memory is what still remembers the real band across the
/// stay (see the two-slot note on `WindowState::head_pts`).
///
/// `new_memory` advances ONLY when a sane decorated windowed sample commits —
/// the acceptance success path. Everything else preserves it: fullscreen
/// samples (the applied 0 is forced, not measured), undecorated samples (the
/// band is legitimately gone, but the chrome may come back), and rejected
/// artifacts (keeping them is the whole point).
pub(crate) fn titlebar_band_decision(
    measured_pts: f64,
    fullscreen: bool,
    decorated: bool,
    memory_pts: f64,
) -> (f64, f64) {
    let applied = accept_titlebar_band_pts(measured_pts, fullscreen, decorated, memory_pts);
    let memory = if !fullscreen && decorated && titlebar_band_sane(measured_pts) {
        measured_pts
    } else {
        memory_pts
    };
    (applied, memory)
}

#[cfg(test)]
mod titlebar_band_acceptance_tests {
    use super::{TITLEBAR_BAND_SANITY_CAP_PTS, accept_titlebar_band_pts, titlebar_band_decision};

    #[test]
    fn fullscreen_forces_zero_regardless_of_sample() {
        assert_eq!(accept_titlebar_band_pts(38.0, true, true, 55.0), 0.0);
        assert_eq!(accept_titlebar_band_pts(0.0, true, true, 55.0), 0.0);
        assert_eq!(accept_titlebar_band_pts(900.0, true, true, 55.0), 0.0);
    }

    #[test]
    fn windowed_zero_sample_is_a_transition_artifact_and_keeps_prev() {
        // The defect-A shape: exit-fullscreen race reads 0 while decorated.
        assert_eq!(accept_titlebar_band_pts(0.0, false, true, 55.0), 55.0);
        assert_eq!(accept_titlebar_band_pts(-3.0, false, true, 55.0), 55.0);
    }

    #[test]
    fn windowed_inflated_sample_keeps_prev() {
        // The defect-B head-side shape: enter-fullscreen race reads the
        // screen-vs-window frame difference as a giant band.
        let inflated = TITLEBAR_BAND_SANITY_CAP_PTS + 1.0;
        assert_eq!(accept_titlebar_band_pts(inflated, false, true, 55.0), 55.0);
        assert_eq!(accept_titlebar_band_pts(700.0, false, true, 55.0), 55.0);
    }

    #[test]
    fn windowed_sane_sample_commits() {
        assert_eq!(accept_titlebar_band_pts(38.0, false, true, 55.0), 38.0);
        assert_eq!(accept_titlebar_band_pts(62.0, false, true, 0.0), 62.0);
    }

    #[test]
    fn undecorated_zero_is_truth_not_artifact() {
        // Runtime decoration toggle: the band really is gone; a kept stale
        // band would inset a chromeless window forever.
        assert_eq!(accept_titlebar_band_pts(0.0, false, false, 55.0), 0.0);
        assert_eq!(accept_titlebar_band_pts(-1.0, false, false, 55.0), 0.0);
    }

    /// The defect this module's `prev = 55.0` cases silently assumed away:
    /// `on_resize` used to feed the APPLIED band back as `prev_pts`, and the
    /// applied band is forced to 0 for the whole of a fullscreen stay — so on
    /// a bad exit sample the "keep the last good band" defense could only
    /// ever keep 0. Driven through `titlebar_band_decision` (the exact caller
    /// wiring; the AppKit measurement is the only thing faked), the two-slot
    /// design must carry 55 across the stay and restore it on exit.
    #[test]
    fn band_memory_survives_a_fullscreen_stay_and_restores_on_exit_artifact() {
        // Windowed, decorated, sane 55 pt band: commits AND becomes memory.
        let (applied, memory) = titlebar_band_decision(55.0, false, true, 0.0);
        assert_eq!((applied, memory), (55.0, 55.0));

        // Enter fullscreen: the transition reads an inflated artifact. Head
        // is forced to 0; the memory must NOT be clobbered.
        let (applied, memory) = titlebar_band_decision(700.0, true, true, memory);
        assert_eq!((applied, memory), (0.0, 55.0));

        // Mid-stay resizes sample a genuine 0 (the titlebar is detached).
        // Still forced 0 applied; still 55 remembered.
        let (applied, memory) = titlebar_band_decision(0.0, true, true, memory);
        assert_eq!((applied, memory), (0.0, 55.0));

        // Exit fullscreen with the classic artifact: decorated + windowed but
        // the sample still reads 0.0. The defense must restore 55, not "keep"
        // the fullscreen 0 that the old single-slot wiring fed it.
        let (applied, memory) = titlebar_band_decision(0.0, false, true, memory);
        assert_eq!((applied, memory), (55.0, 55.0));
    }

    /// The memory slot advances ONLY on the acceptance success path — every
    /// non-committing sample preserves it (rejected artifacts, fullscreen's
    /// forced 0, and the undecorated truth-0, which applies but must not
    /// poison the fallback for when the chrome returns).
    #[test]
    fn band_memory_advances_only_when_a_windowed_sample_commits() {
        // Windowed rejected artifacts: applied falls back to memory, memory holds.
        assert_eq!(titlebar_band_decision(0.0, false, true, 55.0), (55.0, 55.0));
        assert_eq!(
            titlebar_band_decision(TITLEBAR_BAND_SANITY_CAP_PTS + 1.0, false, true, 55.0),
            (55.0, 55.0)
        );
        // Fullscreen: whatever the sample, applied is 0 and memory holds.
        assert_eq!(titlebar_band_decision(38.0, true, true, 55.0), (0.0, 55.0));
        // Undecorated: 0 applies (the band really is gone) but memory holds,
        // so re-decorating can still recover a band through the artifact path.
        assert_eq!(titlebar_band_decision(0.0, false, false, 55.0), (0.0, 55.0));
        // The one advancing case: windowed + decorated + sane commits both.
        assert_eq!(
            titlebar_band_decision(38.0, false, true, 55.0),
            (38.0, 38.0)
        );
    }
}

fn warn_deprecated_font_env_aliases_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for (var, key) in [
            ("ATERM_FALLBACK_FONT", "fallback_fonts"),
            ("ATERM_SYMBOL_FONT", "symbol_font"),
            ("ATERM_EMOJI_FONT", "emoji_font"),
        ] {
            if std::env::var_os(var).is_some() {
                eprintln!(
                    "aterm-gui: ${var} is deprecated; set `{key}` in aterm.toml instead \
                     (an explicit config entry outranks the env alias)"
                );
            }
        }
    });
}

/// Deprecation notice (once per process) for the PRE-RENAME display-face
/// spellings — the `game_font` key and the game-named ids (`minecraft`, …).
///
/// Both keep working, which is the whole point: a key or value that was correct
/// when it was written must not become a config error later. But the user is
/// told the new spelling, because the old one is the one they will keep copying
/// out of old notes otherwise.
///
/// `mariokart` gets its own sentence. It is the one retired id with NO
/// successor — its face shipped a `name` table still reading "Typeface © (your
/// company)", so there was nothing to relicense and nothing to promote in its
/// place. Silence would leave the user staring at their ordinary font with no
/// idea why the setting stopped taking.
///
/// Reads the RAW config text, not the parsed struct, because `#[serde(alias)]`
/// is exactly the machinery that erases which spelling was on disk.
fn warn_deprecated_display_font_spelling(source: &str, config: &Config) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let legacy_key = source.lines().any(|line| {
            line.split('=')
                .next()
                .is_some_and(|key| key.trim() == crate::prefs::LEGACY_EDIT_DISPLAY_FONT)
        });
        if legacy_key {
            eprintln!(
                "aterm-gui: `{legacy}` is deprecated; rename it to `{current}` \
                 (the old key still works — the faces are now named for the \
                 letterform rather than a game)",
                legacy = crate::prefs::LEGACY_EDIT_DISPLAY_FONT,
                current = crate::prefs::EDIT_DISPLAY_FONT,
            );
        }
        for id in config
            .display_font
            .iter()
            .flat_map(|raw| raw.split('+'))
            .map(str::trim)
        {
            match aterm_render::DISPLAY_FACE_LEGACY_IDS
                .iter()
                .find(|(legacy, _)| *legacy == id)
            {
                Some((_, Some(current))) => eprintln!(
                    "aterm-gui: `{key} = \"{id}\"` is deprecated; write \"{current}\" \
                     instead (same face, named for its letterform)",
                    key = crate::prefs::EDIT_DISPLAY_FONT,
                ),
                Some((_, None)) => eprintln!(
                    "aterm-gui: `{key} = \"{id}\"` names a face aterm no longer ships \
                     — it carried no redistribution licence and has no substitute; \
                     your primary font is used instead",
                    key = crate::prefs::EDIT_DISPLAY_FONT,
                ),
                None => {}
            }
        }
    });
}

/// M5 honest fallback (once per process): `background_opacity < 1.0` requests
/// translucent glass. The GPU backend composites it for real (PostMultiplied
/// swapchain over an `NSVisualEffectView`), but the CPU softbuffer surface is
/// opaque with no non-opaque composite, so on the CPU backend a translucent value
/// renders solid — warn rather than silently ignore the key. The M5 legibility
/// guarantee still applies (the effective contrast floor rose regardless), so the
/// solid fallback never hurts text.
fn warn_background_opacity_unimplemented_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "aterm-gui: background_opacity < 1.0 requests translucent glass, but the \
             CPU (softbuffer) renderer has no translucent present path; the window \
             renders solid (use the GPU backend for real vibrancy; the raised contrast \
             floor still applies)"
        );
        // The stderr line above is invisible to any windowed launch (a
        // Finder-launched .app, a Start-Menu launch — the same reason
        // `config_notice` exists at all), and this is a key the user deliberately
        // set and can watch do nothing. Give it the in-window banner too.
        crate::config_notice::queue_deferred(
            "background_opacity has no effect on the CPU renderer — it has no translucent \
             present path, so the window stays solid. Enable the GPU renderer for real \
             transparency."
                .to_string(),
        );
    });
}

/// M5 honest fallback (once per process): a non-`none` `background_material`
/// selects a window-level `NSVisualEffectView` blur, driven on the GPU backend
/// (via `AppRt::window_set_vibrancy`). The CPU softbuffer surface cannot composite
/// over such a view, so on the CPU backend the material has no consumer — warn
/// once (a valid value parses cleanly, so it would otherwise be a completely
/// silent no-op) rather than let the user believe the blur engaged.
fn warn_background_material_unimplemented_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "aterm-gui: background_material selects a window-level vibrancy blur \
             (NSVisualEffectView), but the CPU (softbuffer) renderer cannot composite \
             over it; the setting has no effect on the CPU backend (use the GPU backend)"
        );
        // Same reasoning as its opacity sibling above: a deliberately-set key
        // doing nothing, explained only on a stream a windowed launch discards.
        crate::config_notice::queue_deferred(
            "background_material has no effect on the CPU renderer — it cannot composite \
             over a window-level blur. Enable the GPU renderer to see the material."
                .to_string(),
        );
    });
}

/// Window-CHROME appearance, distinct from the terminal-body color scheme.
/// Resolved from config `window_theme` via [`Config::window_theme_or_default`] and
/// applied through the cross-platform [`crate::platform::AppRt::window_set_appearance`]
/// seam (native AppKit/DWM chrome or winit's system-decoration theme).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum WindowTheme {
    /// Follow the OS light/dark setting, so chrome tracks live appearance switches.
    /// The default.
    #[default]
    Auto,
    /// Force light window chrome.
    Light,
    /// Force dark window chrome.
    Dark,
}

impl WindowTheme {
    /// Parse a config `window_theme` value (case-insensitive, trimmed): `auto`,
    /// `light`, or `dark`. `None` on any other value (caller defaults to `Auto`).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// What a RIGHT press in the terminal grid does when no app is tracking the
/// mouse. Resolved from config `right_click` via
/// [`Config::right_click_or_default`], consumed by the right-button pre-dispatch
/// arm in `app_mouse::on_mouse_input`. Deliberately NOT an `Option<bool>`: the
/// gesture is a semantics choice (section-4 decision #2 — paste vs context menu
/// vs both), so the key is an open enum a future `"menu"` variant can join
/// without a config break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightClickGesture {
    /// The conhost / Windows Terminal convention: copy the selection if one
    /// exists (and clear it), else paste the clipboard — tracking OFF only.
    CopyPaste,
    /// Leave the right button to the seam: reported to a tracking app, inert
    /// otherwise (the pre-gesture behaviour on every platform).
    Off,
}

impl RightClickGesture {
    /// The per-platform default when the key is absent: `CopyPaste` on Windows
    /// (conhost QuickEdit and Windows Terminal both ship it — a Windows user's
    /// hand expects the gesture), `Off` elsewhere (macOS right-click culturally
    /// means "context menu", and Linux already pastes on MIDDLE click). A
    /// `cfg!` const, not two `#[cfg]` items, so the non-Windows arm stays
    /// type-checked on every platform build.
    pub(crate) const PLATFORM_DEFAULT: Self = if cfg!(windows) {
        Self::CopyPaste
    } else {
        Self::Off
    };

    /// Parse a config `right_click` value (case-insensitive, trimmed):
    /// `copy_paste` (alias `copy-paste`) or `off`. `None` on any other value
    /// (caller falls back to [`Self::PLATFORM_DEFAULT`]).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "copy_paste" | "copy-paste" => Some(Self::CopyPaste),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// Which KEYBOARD spellings of "show this tab's context menu" aterm CLAIMS.
/// Resolved from config `tab_menu_chord` via
/// [`Config::tab_menu_chord_or_default`], consumed by `app_input::tab_menu_chord`
/// on both the winit and the control-seam route.
///
/// An open enum rather than an `Option<bool>` for the same reason
/// [`RightClickGesture`] is: the interesting answer is not on/off but WHICH of
/// the two spellings a user wants to keep, because their costs differ. The Menu
/// key reaches an application ONLY under a kitty enhancement (and that case is
/// already deferred unconditionally), so claiming it is nearly free; Shift+F10
/// is a real legacy-encodable chord (`ESC[21;2~`, F20 in the xterm tradition)
/// that claiming genuinely takes away.
///
/// Compiled everywhere so the config key parses and validates on every platform
/// (`--validate-config` must not depend on the host), but only READ by the
/// `#[cfg(windows)]` chord arms — hence the platform-scoped dead-code allowance
/// rather than a `#[cfg]` on the type itself.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabMenuChord {
    /// Both Windows spellings: the dedicated Menu/Application key AND Shift+F10.
    On,
    /// The Menu key only — Shift+F10 goes to the terminal application.
    MenuKey,
    /// Neither; the menu is pointer-only and both keys reach the application.
    Off,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl TabMenuChord {
    /// Parse a config `tab_menu_chord` value (case-insensitive, trimmed):
    /// `on` / `both`, `menu_key` (aliases `menu-key`, `menu`), or `off`.
    /// `None` on any other value (caller falls back to [`Self::On`]).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "on" | "both" => Some(Self::On),
            "menu_key" | "menu-key" | "menu" => Some(Self::MenuKey),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// Whether this policy claims the dedicated Menu / Application key.
    pub(crate) fn claims_menu_key(self) -> bool {
        matches!(self, Self::On | Self::MenuKey)
    }

    /// Whether this policy claims Shift+F10.
    pub(crate) fn claims_shift_f10(self) -> bool {
        matches!(self, Self::On)
    }
}

/// C3 — how tall the in-grid tab band is. Resolved from config `tab_band_height`
/// via [`Config::tab_band_height_or_default`], consumed ONLY by the Windows
/// synthetic-head derivation (`App::synthetic_strip_head_px`).
///
/// WHY AN ENUM AND NOT A PIXEL COUNT: the band is not one number the user should
/// have to compute. Its height is `pad_top + head + strip_rows·cell_h`, and two of
/// those three terms move with the font and the DPI. A named target ("as tall as a
/// native tab") is the thing a person actually wants; the residue is arithmetic.
/// A future `"tall"` (the 40 px Fluent touch target) joins without a config break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabBandHeight {
    /// The pre-C3 height: no synthetic head at all, so the band is exactly the top
    /// pad plus the strip's cell rows — 21 px at the Windows defaults, 96 dpi
    /// (2 px top pad + one 19 px cell row at FONT_PX 16), as measured, not estimated.
    /// The default off Windows, and the escape hatch for an owner who wants the
    /// tightest possible chrome.
    Compact,
    /// Size the WHOLE band to [`TAB_BAND_STANDARD_LOGICAL_PX`] logical px — a real
    /// WinUI tab — by reserving the difference as a synthetic chrome head.
    Standard,
}

impl TabBandHeight {
    /// The per-platform default when the key is absent: `Standard` on Windows (the
    /// in-grid strip is the window's ONLY tab chrome there, and a 23 px band next
    /// to a 32 px caption reads as a squashed toolbar) AND on Linux (same
    /// only-chrome premise, and the pixel band that designs the Linux strip —
    /// [`crate::tab_bar::pixel_band`] — is drawn for the full
    /// [`TAB_BAND_STANDARD_LOGICAL_PX`] canvas: cards optically centred in a
    /// native-height bar, which one 18-px cell row cannot carry). `Compact` on
    /// macOS, which carries tabs in the native toolbar and never paints this band
    /// at all. A `cfg!` const, not two `#[cfg]` items, so both arms stay
    /// type-checked everywhere.
    pub(crate) const PLATFORM_DEFAULT: Self = if cfg!(any(windows, target_os = "linux")) {
        Self::Standard
    } else {
        Self::Compact
    };

    /// Parse a config `tab_band_height` value (case-insensitive, trimmed):
    /// `compact` or `standard`. `None` on any other value (caller falls back to
    /// [`Self::PLATFORM_DEFAULT`]).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "compact" => Some(Self::Compact),
            "standard" => Some(Self::Standard),
            _ => None,
        }
    }

    /// The band height this policy targets, in LOGICAL px (1/96 in on Windows).
    /// `0.0` for [`Self::Compact`] — which makes the whole synthetic-head law
    /// collapse to `head = 0`, i.e. the byte-identical pre-C3 geometry.
    pub(crate) fn target_logical_px(self) -> f32 {
        match self {
            Self::Compact => 0.0,
            Self::Standard => TAB_BAND_STANDARD_LOGICAL_PX,
        }
    }
}

/// The `"standard"` band target in LOGICAL px, per platform's own chrome idiom.
/// WINDOWS: 32 is the Win11/WinUI tab strip height (Windows Terminal's own tab
/// row measures 32-34 px at 100%), and it is also what the caption next to it is
/// — so the two read as one chrome block rather than a full-height title bar
/// over a squashed toolbar. LINUX: 36 is the libadwaita `AdwTabBar` content
/// height (GNOME Console/Text Editor tab rows measure 34-38 px at 100%), the
/// bar the pixel band's card design is drawn against. A `cfg!` const so both
/// arms stay type-checked everywhere; only the platform that reads the value
/// through [`TabBandHeight::Standard`] ever feels it.
pub(crate) const TAB_BAND_STANDARD_LOGICAL_PX: f32 = if cfg!(target_os = "linux") {
    36.0
} else {
    32.0
};

/// Sanity cap on the SYNTHETIC head, in device px, as a multiple of the cell
/// height: a pathological config (huge `tab_band_height` target against a tiny
/// font) must never be able to push the grid down by more than the band could
/// plausibly need. Distinct from [`TITLEBAR_BAND_SANITY_CAP_PTS`], which guards a
/// MEASURED AppKit value; this one guards arithmetic we control, so it only has to
/// be a backstop.
const SYNTHETIC_BAND_HEAD_CELL_CAP: usize = 4;

/// THE C3 LAW, pure: the synthetic chrome-head band in DEVICE px for a strip of
/// `strip_rows` rows of `cell_h` px sitting under `pad_top_px` of top padding, when
/// the whole band should measure `target_logical` logical px at `scale`.
///
/// `head = clamp(round(target·scale) − pad_top − strip_rows·cell_h)`: the band the
/// viewer sees is `head + pad_top + strip_rows·cell_h`, so this is simply "reserve
/// the remainder". It is deliberately a SUBTRACTION rather than a fixed additive
/// band: at a large font (or `tab_strip_rows = 2`) the cell rows already exceed the
/// target, the remainder is zero, and the band stops growing instead of stacking a
/// constant on top of an already-tall row. Every degenerate input (strip off, zero
/// target, non-finite scale) returns 0 — the byte-identical pre-C3 geometry.
pub(crate) fn synthetic_band_head_px(
    target_logical: f32,
    pad_top_px: usize,
    strip_rows: u16,
    cell_h: usize,
    scale: f64,
) -> usize {
    // NOT `target_logical <= 0.0`: that is FALSE for NaN, and this guard must
    // send NaN down the degenerate path the doc comment above promises. The
    // `partial_cmp` form says the same thing as the negated `>` it replaces —
    // proceed ONLY on a definite Greater — while satisfying
    // `clippy::neg_cmp_op_on_partial_ord`.
    let target_is_positive = matches!(
        target_logical.partial_cmp(&0.0),
        Some(std::cmp::Ordering::Greater)
    );
    if strip_rows == 0 || !target_is_positive {
        return 0;
    }
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let target_px = crate::logical_to_device_px(target_logical, scale);
    let strip_px = (strip_rows as usize).saturating_mul(cell_h);
    let head = target_px
        .saturating_sub(pad_top_px)
        .saturating_sub(strip_px);
    head.min(cell_h.saturating_mul(SYNTHETIC_BAND_HEAD_CELL_CAP))
}

/// GPU-present colour-space TAG for the window's CAMetalLayer (M3 phase A),
/// resolved from config `window_colorspace` via
/// [`Config::window_colorspace_or_default`] and applied at surface attach (and on
/// hot reload) through [`crate::platform::AppRt::window_set_surface_colorspace`].
/// The tag changes how ColorSync INTERPRETS the presented bytes, never the bytes
/// themselves — the readback/introspection parity path is untouched by
/// construction (and pinned by the parity suites).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum WindowColorspace {
    /// Tag the layer sRGB (the default): theme colours render as authored;
    /// ColorSync performs the one honest sRGB→panel mapping.
    #[default]
    Srgb,
    /// Tag the layer Display-P3: the sRGB bytes are read as P3 coordinates —
    /// the legacy untagged behaviour on wide-gamut panels (stretched/saturated).
    DisplayP3,
}

impl WindowColorspace {
    /// Parse a config `window_colorspace` value (case-insensitive, trimmed):
    /// `srgb`, or `display-p3` (aliases `displayp3` / `p3`). `None` on any other
    /// value (caller defaults to `Srgb`).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "srgb" => Some(Self::Srgb),
            "display-p3" | "displayp3" | "p3" => Some(Self::DisplayP3),
            _ => None,
        }
    }
}

/// Upper clamp on `window_padding` (logical px): generous breathing room is
/// allowed, but past this the pad starts eating whole rows/columns on small
/// windows — the `tab_strip_rows` "can't starve the terminal" posture.
pub(crate) const MAX_WINDOW_PADDING_PX: f32 = 64.0;

/// The WCAG-AA per-cell contrast floor (4.5:1) the M5 legibility guarantee
/// auto-engages whenever the window is translucent. The twin of the `Floor`
/// const in `aterm_spec::derive::vibrancy_contrast_model`.
pub(crate) const VIBRANCY_CONTRAST_FLOOR: f32 = 4.5;

/// M5 true vibrancy: the macOS `NSVisualEffectView` MATERIAL blended behind the
/// translucent window background, resolved from config `background_material` via
/// [`Config::background_material_or_default`]. Only meaningful when
/// `background_opacity < 1.0` on the GPU backend; `None` installs no vibrancy
/// view (and is the byte-identical default). The variants mirror AppKit's
/// `NSVisualEffectView.Material` cases the terminal exposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum BackgroundMaterial {
    /// No vibrancy view — the background is whatever `background_opacity`
    /// composites over the window's own backing (the default).
    #[default]
    None,
    /// `.hud` — the heads-up-display material (darkest, most saturated blur).
    Hud,
    /// `.sidebar` — the source-list material (a lighter, source-list blur).
    Sidebar,
    /// `.underWindowBackground` — the behind-window desktop blur.
    UnderWindow,
}

impl BackgroundMaterial {
    /// Parse a config `background_material` value (case-insensitive, trimmed):
    /// `none`, `hud`, `sidebar`, or `under-window` (aliases `underwindow` /
    /// `under_window`). `None` on any other value (caller warns + defaults).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "" => Some(Self::None),
            "hud" => Some(Self::Hud),
            "sidebar" => Some(Self::Sidebar),
            "under-window" | "underwindow" | "under_window" => Some(Self::UnderWindow),
            _ => None,
        }
    }
}

/// Content fingerprints of the active files the config references BY PATH: the
/// `sparkle_words.lexicon` file and `sparkle_words.toy_packs` manifests (the
/// word-decoration feed) and the `cursor_trail_packs` manifests (the Trail Pack
/// registry feed). The APPLIED configuration includes these files' CONTENT, yet
/// `Config` equality compares only the path strings — so the reload dedupe must
/// compare fingerprints too, or the documented touch-to-reload workflow (edit a
/// pack/lexicon file, then re-save/`touch` a byte-identical `aterm.toml` —
/// docs/trail-packs.md, docs/TOY_PACKS.md, docs/sparkle-words-design.md) silently
/// stops re-reading them, with restartless recovery only via non-obvious
/// workarounds. An explicitly disabled Sparkle table has no active consumer and
/// therefore performs no path I/O; re-enabling changes the table itself and
/// prepares current bytes. Split in two so the consumers reset precisely: the deco feed
/// warrants a per-window `word_decos.hard_reset()`, the trail feed only a
/// registry rebuild (cursor glow — never a decoration reset).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct PathFeedFps {
    /// Lexicon + toy-pack content: feeds the compiled lexicon the per-window
    /// word decorations scan against.
    pub(crate) deco: u64,
    /// Trail-pack manifest content: feeds the `id → TrailParams` registry.
    pub(crate) trail: u64,
}

/// Whether a reload must HARD-RESET the per-window word decorations: the keys
/// that feed them changed — the `[sparkle_words]` table, the theme its palette
/// derives from, or (the path-referenced blindspot) the CONTENT of the
/// lexicon/toy-pack files that table names by path. Config-struct equality alone
/// cannot see the last one: a lexicon-FILE edit plus an unrelated config edit
/// parses `sparkle_words` equal while the compiled lexicon is about to change —
/// without the fingerprint term the App-level lexicon would rebuild but every
/// window would keep showing decorations from the OLD lexicon on a byte-idle
/// grid (the per-window rescan is damage-epoch-driven, never lexicon-driven).
fn deco_feed_changed(old: &Config, new: &Config, fresh_deco_fp: u64, applied_deco_fp: u64) -> bool {
    old.sparkle_words != new.sparkle_words
        || old.theme != new.theme
        || fresh_deco_fp != applied_deco_fp
}

/// Expand a leading `~` / `~/` / `$HOME` to the user's home directory.
pub(crate) fn sparkle_expand_tilde(path: &str) -> std::path::PathBuf {
    if path == "~"
        && let Some(h) = std::env::var_os("HOME")
    {
        return std::path::PathBuf::from(h);
    } else if let Some(rest) = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("$HOME/"))
        && let Some(h) = std::env::var_os("HOME")
    {
        return std::path::Path::new(&h).join(rest);
    }
    std::path::PathBuf::from(path)
}

fn bounded_asset_text(value: &str, max_bytes: usize) -> std::sync::Arc<str> {
    if value.len() <= max_bytes {
        return std::sync::Arc::from(value);
    }
    let suffix = '…';
    let mut end = max_bytes.saturating_sub(suffix.len_utf8()).min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    std::sync::Arc::from(format!("{}{}", &value[..end], suffix))
}

fn invalid_kitty_sprite(source_id: &str, reason: impl AsRef<str>) -> KittySpriteAsset {
    KittySpriteAsset::Invalid {
        source_id: bounded_asset_text(source_id, MAX_KITTY_SOURCE_ID_BYTES),
        bounded_reason: bounded_asset_text(reason.as_ref(), MAX_KITTY_REASON_BYTES),
    }
}

fn read_bounded_kitty_png(path: &std::path::Path) -> Result<Vec<u8>, String> {
    aterm_effects::file_feed::read_bounded_regular_file(path, MAX_KITTY_SPRITE_FILE_BYTES)
        .map_err(|error| format!("unreadable ({error})"))
}

fn stable_asset_fingerprint(tag: u8, source: &[u8], w: u16, h: u16, rgba: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut fold = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    fold(&[tag]);
    fold(&(source.len() as u64).to_le_bytes());
    fold(source);
    fold(&w.to_le_bytes());
    fold(&h.to_le_bytes());
    fold(&(rgba.len() as u64).to_le_bytes());
    fold(rgba);
    hash | 1
}

fn resolve_kitty_sprite_asset(raw: Option<&str>) -> KittySpriteAsset {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return KittySpriteAsset::BuiltIn;
    };
    if raw.len() > MAX_KITTY_SOURCE_ID_BYTES {
        return invalid_kitty_sprite(raw, "configured sprite source is too long");
    }
    let source_id = std::sync::Arc::<str>::from(raw);
    let path = sparkle_expand_tilde(raw);
    let bytes = match read_bounded_kitty_png(&path) {
        Ok(bytes) => bytes,
        Err(reason) => return invalid_kitty_sprite(&source_id, reason),
    };
    let Some((rgba, w, h)) = aterm_render::decode_png_rgba8(&bytes) else {
        return invalid_kitty_sprite(&source_id, "PNG decode failed");
    };
    if w == 0
        || h == 0
        || w > MAX_KITTY_SPRITE_DIMENSION
        || h > MAX_KITTY_SPRITE_DIMENSION
        || rgba.len() != w.saturating_mul(h).saturating_mul(4)
    {
        return invalid_kitty_sprite(
            &source_id,
            format!("decoded sprite must be 1..={MAX_KITTY_SPRITE_DIMENSION} pixels per side"),
        );
    }
    let Ok(w) = u16::try_from(w) else {
        return invalid_kitty_sprite(&source_id, "decoded width is out of range");
    };
    let Ok(h) = u16::try_from(h) else {
        return invalid_kitty_sprite(&source_id, "decoded height is out of range");
    };
    let fp = stable_asset_fingerprint(0x52, source_id.as_bytes(), w, h, &rgba);
    KittySpriteAsset::Ready {
        source_id,
        w,
        h,
        rgba: std::sync::Arc::from(rgba),
        fp,
    }
}

fn invalid_wallpaper(source_id: &str, reason: impl AsRef<str>) -> WallpaperAsset {
    WallpaperAsset::Invalid {
        source_id: bounded_asset_text(source_id, MAX_KITTY_SOURCE_ID_BYTES),
        bounded_reason: bounded_asset_text(reason.as_ref(), MAX_KITTY_REASON_BYTES),
    }
}

/// Decode the configured wallpaper's bytes to straight sRGB RGBA8: PNG through
/// the shared hardened decoder (wallpaper dimension budget), and every other
/// format (JPEG/HEIC/TIFF/GIF, …) through the system `NSBitmapImageRep` lane on
/// macOS — the decoded rep is converted to sRGB and re-encoded as PNG so Rust
/// reads a standardized straight-RGBA channel order (the window-capture
/// pattern), never the rep's implementation-defined layout.
fn decode_wallpaper_bytes(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    if let Some(decoded) =
        aterm_render::decode_png_rgba8_bounded(bytes, MAX_WALLPAPER_SOURCE_DIMENSION as u32)
    {
        return Ok(decoded);
    }
    #[cfg(target_os = "macos")]
    {
        decode_wallpaper_appkit(bytes)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(format!(
            "not a decodable PNG (up to {MAX_WALLPAPER_SOURCE_DIMENSION} px per side; \
             other formats need macOS)"
        ))
    }
}

#[cfg(target_os = "macos")]
fn decode_wallpaper_appkit(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey,
        NSColorRenderingIntent, NSColorSpace,
    };
    use objc2_foundation::{NSData, NSDictionary};

    let data = NSData::with_bytes(bytes);
    // SAFETY: pure raster construction from immutable bytes; NSBitmapImageRep
    // is not main-thread-bound (no view or window involvement).
    let Some(rep) = (unsafe { NSBitmapImageRep::imageRepWithData(&data) }) else {
        return Err("not a decodable image".to_string());
    };
    let (pw, ph) = unsafe { (rep.pixelsWide(), rep.pixelsHigh()) };
    if pw <= 0
        || ph <= 0
        || pw as usize > MAX_WALLPAPER_SOURCE_DIMENSION
        || ph as usize > MAX_WALLPAPER_SOURCE_DIMENSION
    {
        return Err(format!(
            "decoded image must be 1..={MAX_WALLPAPER_SOURCE_DIMENSION} pixels per side"
        ));
    }
    // Convert the pixels (not merely retag) to the renderer's canonical sRGB
    // before Rust reads them — the window-capture rule.
    let srgb = unsafe { NSColorSpace::sRGBColorSpace() };
    let rep = unsafe {
        rep.bitmapImageRepByConvertingToColorSpace_renderingIntent(
            &srgb,
            NSColorRenderingIntent::Perceptual,
        )
    }
    .ok_or_else(|| "could not convert the image to sRGB".to_string())?;
    let properties = NSDictionary::<NSBitmapImageRepPropertyKey, objc2::runtime::AnyObject>::new();
    // PNG standardizes the rep's implementation-defined channel order and
    // premultiplication into straight RGBA before Rust reads it.
    let png =
        unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties) }
            .ok_or_else(|| "could not re-encode the decoded image".to_string())?;
    let png_len = png.length();
    let mut png_bytes = vec![0_u8; png_len];
    if png_len != 0 {
        // SAFETY: the Vec owns `png_len` initialized bytes and the non-null
        // destination remains valid for the duration of NSData's bounded copy.
        let destination = std::ptr::NonNull::new(png_bytes.as_mut_ptr().cast())
            .ok_or_else(|| "could not allocate the decoded image bytes".to_string())?;
        unsafe {
            png.getBytes_length(destination, png_len);
        }
    }
    aterm_render::decode_png_rgba8_bounded(&png_bytes, MAX_WALLPAPER_SOURCE_DIMENSION as u32)
        .ok_or_else(|| "could not read the decoded image pixels".to_string())
}

pub(crate) fn resolve_wallpaper_asset(raw: Option<&str>) -> WallpaperAsset {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return WallpaperAsset::None;
    };
    if raw.len() > MAX_KITTY_SOURCE_ID_BYTES {
        return invalid_wallpaper(raw, "configured wallpaper source is too long");
    }
    let source_id = std::sync::Arc::<str>::from(raw);
    let path = sparkle_expand_tilde(raw);
    let bytes = match aterm_effects::file_feed::read_bounded_regular_file(
        &path,
        MAX_WALLPAPER_FILE_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(error) => return invalid_wallpaper(&source_id, format!("unreadable ({error})")),
    };
    let (mut rgba, mut w, mut h) = match decode_wallpaper_bytes(&bytes) {
        Ok(decoded) => decoded,
        Err(reason) => return invalid_wallpaper(&source_id, reason),
    };
    if w == 0 || h == 0 || rgba.len() != w.saturating_mul(h).saturating_mul(4) {
        return invalid_wallpaper(&source_id, "decoded image has inconsistent dimensions");
    }
    // Bound resident memory: downscale (linear-light, aspect-preserving) so
    // neither side exceeds the keep budget. Windows are cover-scaled FROM this,
    // and the budget exceeds any frame, so no visible quality is lost.
    let longest = w.max(h);
    if longest > MAX_WALLPAPER_KEEP_DIMENSION {
        let dw = (w * MAX_WALLPAPER_KEEP_DIMENSION / longest).max(1);
        let dh = (h * MAX_WALLPAPER_KEEP_DIMENSION / longest).max(1);
        rgba = aterm_render::resample_rgba(&rgba, w, h, dw, dh);
        (w, h) = (dw, dh);
    }
    let (Ok(w32), Ok(h32)) = (u32::try_from(w), u32::try_from(h)) else {
        return invalid_wallpaper(&source_id, "decoded dimensions are out of range");
    };
    // `stable_asset_fingerprint` folds u16 dims; the wallpaper's exceed u16 in
    // principle, so fold the true u32 dims through the source stream instead.
    let mut identity = Vec::with_capacity(source_id.len() + 8);
    identity.extend_from_slice(source_id.as_bytes());
    identity.extend_from_slice(&w32.to_le_bytes());
    identity.extend_from_slice(&h32.to_le_bytes());
    let fp = stable_asset_fingerprint(0x57, &identity, 0, 0, &rgba);
    WallpaperAsset::Ready {
        source_id,
        w: w32,
        h: h32,
        rgba: std::sync::Arc::from(rgba),
        fp,
    }
}

/// Escape `w` as a TOML basic string (including surrounding quotes). Escapes
/// `\`/`"` AND control characters — a raw newline/tab/etc. is illegal in a TOML
/// basic string, so without this a single malformed `extra_words` entry would
/// make the WHOLE generated override document fail to parse and silently drop
/// every custom word (profanity/feline/orca), not just the offending one.
fn toml_basic_string(w: &str) -> String {
    let mut s = String::with_capacity(w.len() + 2);
    s.push('"');
    for c in w.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            '\u{0008}' => s.push_str("\\b"),
            '\t' => s.push_str("\\t"),
            '\n' => s.push_str("\\n"),
            '\u{000C}' => s.push_str("\\f"),
            '\r' => s.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{007F}' => {
                s.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// Append a synthetic `forms`-mode lexicon `[[entry]]` for a category's
/// `extra_words` to `out` (no-op when empty). Words are TOML-escaped.
fn append_extra_words_entry(out: &mut String, class: &str, words: Option<&[String]>) {
    let Some(words) = words.filter(|w| !w.is_empty()) else {
        return;
    };
    let forms: Vec<String> = words.iter().map(|w| toml_basic_string(w)).collect();
    out.push_str(&format!(
        "\n[[entry]]\nclass = \"{class}\"\nlang = \"en\"\nmode = \"forms\"\nforms = [{}]\n",
        forms.join(", ")
    ));
}

/// Parse a `#RRGGBB` (or bare `RRGGBB`) hex colour; `None` on malformed input.
pub(crate) fn parse_hex_color(s: &str) -> Option<Rgb> {
    let h = s.trim();
    let h = h.strip_prefix('#').unwrap_or(h);
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(Rgb::new(
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ))
}

impl Config {
    /// Build the engine `TerminalConfig` deltas this config implies, or `None`
    /// when nothing engine-side is set (so the GUI skips `apply_config`).
    /// [`Self::terminal_config_for`] at the default (Dark) appearance. Test-only: the
    /// runtime always resolves for the live OS appearance via the `_for` variant.
    #[cfg(test)]
    pub(crate) fn terminal_config(&self) -> Option<aterm_core::config::TerminalConfig> {
        self.terminal_config_for(aterm_types::Appearance::Dark)
    }

    /// [`Self::terminal_config`] resolved for a specific OS `appearance` — picks the
    /// matching side of a `dark:…,light:…` split theme (see [`Self::resolve_theme_name`]).
    #[cfg(test)]
    pub(crate) fn terminal_config_for(
        &self,
        appearance: aterm_types::Appearance,
    ) -> Option<aterm_core::config::TerminalConfig> {
        self.terminal_config_for_with_assets(appearance, &ThemeCatalog::default())
    }

    /// Engine config resolved against the same parsed theme catalog carried by
    /// the active config snapshot. This path cannot reopen a theme file.
    pub(crate) fn terminal_config_for_with_assets(
        &self,
        appearance: aterm_types::Appearance,
        themes: &ThemeCatalog,
    ) -> Option<aterm_core::config::TerminalConfig> {
        let mut tc = aterm_core::config::TerminalConfig::default();
        let mut any = false;
        if let Some(n) = self.scrollback_lines {
            // 0 → unlimited (None); N → cap at N lines.
            tc.scrollback_limit = (n != 0).then_some(n);
            any = true;
        }
        if self.cursor_style.is_some() || self.cursor_blink.is_some() {
            let blink = self.cursor_blink.unwrap_or(true);
            let cursor_style = self.cursor_style.as_deref().unwrap_or("block").trim();
            // The "_" underline OPTION is retired (owner: keep block + "|").
            // Programs may still request underline via DECSCUSR — that is
            // terminal protocol, not user configuration; a config asking for
            // it falls back to the bar with a one-line note.
            tc.cursor_style = if cursor_style.eq_ignore_ascii_case("block") {
                if blink {
                    CursorStyle::BlinkingBlock
                } else {
                    CursorStyle::SteadyBlock
                }
            } else if cursor_style.eq_ignore_ascii_case("underline") {
                eprintln!("aterm-gui: config cursor_style \"underline\" is retired; using \"bar\"");
                if blink {
                    CursorStyle::BlinkingBar
                } else {
                    CursorStyle::SteadyBar
                }
            } else if cursor_style.eq_ignore_ascii_case("bar")
                || cursor_style.eq_ignore_ascii_case("beam")
            {
                if blink {
                    CursorStyle::BlinkingBar
                } else {
                    CursorStyle::SteadyBar
                }
            } else {
                eprintln!(
                    "aterm-gui: config cursor_style: expected block|bar, got {cursor_style:?}"
                );
                if blink {
                    CursorStyle::BlinkingBlock
                } else {
                    CursorStyle::SteadyBlock
                }
            };
            tc.cursor_blink = blink;
            any = true;
        }
        // A named theme seeds the engine default fg/bg, cursor, and the full ANSI
        // palette; the per-key color blocks below then override individual slots
        // (last-wins). No theme = this block is skipped, so the per-key path stays
        // byte-identical to before.
        if let Some(name) = self.resolve_theme_name(appearance) {
            // Single point that warns on a theme that is absent or invalid in
            // the admitted startup catalog. The renderer's base resolver is
            // silent so it cannot double-print this message.
            if !name.eq_ignore_ascii_case("default") {
                match themes.resolve(&name) {
                    Ok(_) => {}
                    Err(error) => {
                        eprintln!(
                            "aterm-gui: config theme: {name:?} does not resolve ({error}); using Default"
                        );
                    }
                }
            }
            let s = self.base_scheme_for_with_themes(appearance, themes);
            tc.default_foreground = s.foreground;
            tc.default_background = s.background;
            if let Some(cur) = s.cursor {
                tc.cursor_color = Some(cur);
            }
            if let Some(sel) = s.selection {
                tc.selection_background = Some(sel);
            }
            tc.custom_palette = Some(s.to_color_palette());
            any = true;
        }
        // Theme colours → engine `default_*`/`cursor_color`. The engine resolves
        // these into each `RenderCell.fg/bg`, so this is NOT a renderer change.
        for (key, raw, slot) in [
            ("foreground", &self.foreground, 0u8),
            ("background", &self.background, 1),
            ("cursor_color", &self.cursor_color, 2),
        ] {
            if let Some(s) = raw {
                match parse_hex_color(s) {
                    Some(rgb) => {
                        match slot {
                            0 => tc.default_foreground = rgb,
                            1 => tc.default_background = rgb,
                            _ => tc.cursor_color = Some(rgb),
                        }
                        any = true;
                    }
                    None => eprintln!("aterm-gui: config {key}: expected #RRGGBB, got {s:?}"),
                }
            }
        }
        // Selection highlight → engine `selection_background` (OSC-21 queryable). The
        // renderer Theme already carries it for drawing; mirror it into the engine so a
        // configured selection colour is also reported on query, not left as `None`.
        if let Some(s) = &self.selection_color {
            match parse_hex_color(s) {
                Some(rgb) => {
                    tc.selection_background = Some(rgb);
                    any = true;
                }
                None => {
                    eprintln!("aterm-gui: config selection_color: expected #RRGGBB, got {s:?}")
                }
            }
        }
        // Selected-text foreground is dynamic OSC 19 state too. Mirror the
        // renderer knob into TerminalConfig so OSC 19 query/119/RIS and newly
        // spawned sessions share one configured baseline with the pixels.
        if let Some(s) = &self.selection_foreground {
            match parse_hex_color(s) {
                Some(rgb) => {
                    tc.selection_foreground = Some(rgb);
                    any = true;
                }
                None => {
                    eprintln!("aterm-gui: config selection_foreground: expected #RRGGBB, got {s:?}")
                }
            }
        }
        // Indexed palette (engine `custom_palette`; also resolved into RenderCell).
        // Explicit overrides layer ON TOP of the theme's ANSI palette (if a theme set
        // one); without a theme this starts empty — byte-identical to before.
        if let Some(entries) = &self.palette {
            let mut pal = tc.custom_palette.take().unwrap_or_else(ColorPalette::new);
            let mut ok = false;
            for (i, hex) in entries.iter().take(256).enumerate() {
                match parse_hex_color(hex) {
                    Some(rgb) => {
                        pal.set(i as u8, rgb);
                        ok = true;
                    }
                    None => {
                        eprintln!("aterm-gui: config palette[{i}]: expected #RRGGBB, got {hex:?}")
                    }
                }
            }
            // Keep the (possibly theme-seeded) palette if any override landed OR a
            // theme already populated it; else leave custom_palette unset (as before).
            if ok || self.theme.is_some() {
                tc.custom_palette = Some(pal);
                any = true;
            }
        }
        // BiDi mode (engine `BiDiConfig.mode`; applied by Terminal::apply_config).
        if let Some(b) = self.bidi.as_deref() {
            use aterm_core::config::BiDiMode;
            match b.trim().to_ascii_lowercase().as_str() {
                "disabled" | "off" => tc.bidi.mode = BiDiMode::Disabled,
                "implicit" | "on" => tc.bidi.mode = BiDiMode::Implicit,
                "explicit" => tc.bidi.mode = BiDiMode::Explicit,
                other => eprintln!(
                    "aterm-gui: config bidi: expected implicit|disabled|explicit, got {other:?}"
                ),
            }
            any = true;
        }
        // East-Asian Ambiguous width (engine `ambiguous_width_double`).
        if let Some(w) = self.ambiguous_width.as_deref() {
            match w.trim().to_ascii_lowercase().as_str() {
                "narrow" | "single" => tc.ambiguous_width_double = false,
                "wide" | "double" => tc.ambiguous_width_double = true,
                other => eprintln!(
                    "aterm-gui: config ambiguous_width: expected narrow|wide, got {other:?}"
                ),
            }
            any = true;
        }
        // Style-attribute policy (W5e/f): bold-to-bright promotion + faint
        // opacity, resolved by the engine's color resolution into RenderCell.
        if let Some(v) = self.bold_is_bright {
            tc.bold_is_bright = v;
            any = true;
        }
        if self.faint_opacity.is_some() {
            tc.faint_opacity = self.faint_opacity_or_default();
            any = true;
        }
        // Security opt-ins (all fail-closed by default in TerminalConfig). Only a
        // present key changes the flag, so omitting them keeps the safe default.
        if let Some(v) = self.allow_osc52_query {
            tc.allow_osc52_query = v;
            any = true;
        }
        if let Some(v) = self.allow_window_ops {
            tc.allow_window_ops = v;
            any = true;
        }
        if let Some(v) = self.allow_notifications {
            tc.allow_notifications = v;
            any = true;
        }
        if let Some(v) = self.allow_palette_reconfigure {
            tc.allow_palette_reconfigure = v;
            any = true;
        }
        any.then_some(tc)
    }

    /// The engine [`TerminalConfig`] to actually APPLY to terminals: the optional
    /// config deltas ([`Self::terminal_config`]) with the engine's default fg/bg
    /// ALWAYS pinned to the renderer [`Self::theme`].
    ///
    /// The engine's spec default background is black (`0,0,0`) — correct VT
    /// semantics — but the GUI clears the window (and the interior padding) to the
    /// THEME background (`#111318`). Left unsynced, an unstyled cell paints spec-black
    /// while the margins paint the theme bg, so the text area reads visibly *blacker*
    /// than its surroundings (two visual judges flagged this "black-backed text" — see
    /// tools/visual-judge). Pinning the engine defaults to the theme makes a default
    /// cell paint exactly the colour the window clears to. `theme()` already folds in
    /// any `foreground`/`background` config, so an explicit theme is honoured too.
    #[cfg(test)]
    pub(crate) fn applied_terminal_config(&self) -> aterm_core::config::TerminalConfig {
        self.applied_terminal_config_for(aterm_types::Appearance::Dark)
    }

    /// [`Self::applied_terminal_config`] resolved for a specific OS `appearance` — the
    /// engine config the GUI applies live when the desktop toggles light↔dark under a
    /// `dark:…,light:…` split theme (see [`Self::resolve_theme_name`]).
    #[cfg(test)]
    pub(crate) fn applied_terminal_config_for(
        &self,
        appearance: aterm_types::Appearance,
    ) -> aterm_core::config::TerminalConfig {
        self.applied_terminal_config_for_with_assets(appearance, &ThemeCatalog::default())
    }

    /// Complete engine projection from an admitted, immutable theme catalog.
    pub(crate) fn applied_terminal_config_for_with_assets(
        &self,
        appearance: aterm_types::Appearance,
        themes: &ThemeCatalog,
    ) -> aterm_core::config::TerminalConfig {
        let mut tc = self
            .terminal_config_for_with_assets(appearance, themes)
            .unwrap_or_default();
        let theme = self.theme_for_with_assets(appearance, themes);
        let rgb = |c: u32| {
            Rgb::new(
                ((c >> 16) & 0xff) as u8,
                ((c >> 8) & 0xff) as u8,
                (c & 0xff) as u8,
            )
        };
        tc.default_foreground = rgb(theme.fg);
        tc.default_background = rgb(theme.bg);
        tc.cursor_color = Some(rgb(theme.cursor));
        tc.selection_background = Some(rgb(theme.selection));
        tc
    }
}

/// Resolve the config file path without creating anything.
/// The `font_px_explicit` pin after a config reload: an admitted `$ATERM_FONT_PX` /
/// `config.font_px` pins outright; otherwise an effective px that DIFFERS from the
/// (re-derived) scale default is a LIVE Cmd-+/− zoom this reload is preserving — it
/// must KEEP its pin. Dropping the pin while keeping the zoomed px re-arms
/// [`App::apply_window_scale`], which then flips the backend to the scale default at
/// the next redraw (a tab switch suffices) WITHOUT a re-grid: bigger cells painted
/// into the zoom-fitted, wider grid — a frame wider than the window, chopped at both
/// edges. Pure, so the pin law is unit-testable.
pub(crate) fn reload_font_pin(
    explicit_now: bool,
    new_font_px: f32,
    new_default_font_px: f32,
) -> bool {
    explicit_now || (new_font_px - new_default_font_px).abs() >= 0.5
}

pub(crate) fn config_path() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    // Windows has neither XDG nor (usually) HOME, so fall back to %APPDATA% — the
    // conventional per-user roaming config dir. Without this, BOTH persistence
    // (`save_prefs_edits`) and the mtime hot-reload watcher silently no-op on Windows.
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|a| !a.is_empty()) {
        return Some(PathBuf::from(appdata).join("aterm").join("aterm.toml"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/aterm/aterm.toml"))
}

/// One ambient launch override that currently outranks aterm.toml. Settings
/// captures these facts into its view model, and Manual's host diagnostics use
/// the same resolver, so preview, accessibility, save feedback, and validation
/// cannot disagree about an environment-pinned value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActiveEnvironmentOverride {
    pub(crate) variable: &'static str,
    pub(crate) effective: String,
}

/// Resolve an active environment/CLI override for one config key. Invalid
/// values are deliberately absent because the runtime also falls through to
/// aterm.toml for them.
pub(crate) fn active_environment_override(key: &str) -> Option<ActiveEnvironmentOverride> {
    let resolved = |variable, effective| ActiveEnvironmentOverride {
        variable,
        effective,
    };
    match key {
        "columns" => env_u16("ATERM_COLUMNS")
            .map(|value| resolved("ATERM_COLUMNS", value.clamp(20, 500).to_string())),
        "lines" => env_u16("ATERM_LINES")
            .map(|value| resolved("ATERM_LINES", value.clamp(5, 300).to_string())),
        "font_px" => {
            font_px_environment_override().map(|value| resolved("ATERM_FONT_PX", value.to_string()))
        }
        "font_family" => std::env::var("ATERM_FONT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| resolved("ATERM_FONT", value)),
        "window_theme"
            if cfg!(target_os = "macos") && std::env::var_os("ATERM_NO_DARK_CHROME").is_some() =>
        {
            Some(resolved("ATERM_NO_DARK_CHROME", "auto".to_string()))
        }
        "gpu" if std::env::var_os("ATERM_CPU").is_some() => {
            Some(resolved("ATERM_CPU", "CPU".to_string()))
        }
        "gpu" if std::env::var_os("ATERM_GPU").is_some() => {
            Some(resolved("ATERM_GPU", "GPU".to_string()))
        }
        "tab_strip_rows" => std::env::var("ATERM_TAB_STRIP_ROWS")
            .ok()
            .and_then(|value| value.trim().parse::<u16>().ok())
            .map(|value| {
                resolved(
                    "ATERM_TAB_STRIP_ROWS",
                    value.min(MAX_TAB_STRIP_ROWS).to_string(),
                )
            }),
        "stem_gamma" => std::env::var("ATERM_STEM_GAMMA")
            .ok()
            .and_then(|value| value.trim().parse::<f32>().ok())
            .filter(|value| value.is_finite())
            .map(|value| {
                resolved(
                    "ATERM_STEM_GAMMA",
                    aterm_render::clamp_stem_gamma(value).to_string(),
                )
            }),
        "shell" => std::env::var("ATERM_SHELL")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| resolved("ATERM_SHELL", value)),
        // Reported UNVALIDATED, deliberately: this resolver's contract is "what
        // ambient value is in force", and the front door's own fallback for an
        // unrecognized spelling is `new_window` with a warning. Filtering an
        // invalid value out here would show the operator a config value that is
        // NOT what the launch will use, which is the confusion this whole
        // resolver exists to prevent.
        "windowing_behavior" => std::env::var("ATERM_WINDOWING_BEHAVIOR")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(|value| resolved("ATERM_WINDOWING_BEHAVIOR", value)),
        "net.listen" => std::env::var("ATERM_NET_LISTEN")
            .ok()
            .map(|value| resolved("ATERM_NET_LISTEN", value)),
        "net.cert" => std::env::var("ATERM_NET_CERT")
            .ok()
            .map(|value| resolved("ATERM_NET_CERT", value)),
        "net.key" => std::env::var("ATERM_NET_KEY")
            .ok()
            .map(|value| resolved("ATERM_NET_KEY", value)),
        "update.owner" => std::env::var("ATERM_UPDATE_OWNER")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| aterm_update_core::is_valid_slug(value))
            .map(|value| resolved("ATERM_UPDATE_OWNER", value)),
        "update.repo" => std::env::var("ATERM_UPDATE_REPO")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| aterm_update_core::is_valid_slug(value))
            .map(|value| resolved("ATERM_UPDATE_REPO", value)),
        "update.auto_apply" if std::env::var_os("ATERM_NO_AUTO_APPLY").is_some() => {
            Some(resolved("ATERM_NO_AUTO_APPLY", "false".to_string()))
        }
        "packages.account" => std::env::var("ATPKG_ACCOUNT")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| aterm_update_core::is_valid_slug(value))
            .map(|value| resolved("ATPKG_ACCOUNT", value)),
        _ => None,
    }
}

/// Load the user config. A missing file is fine (defaults); a malformed file is
/// reported and ignored rather than aborting the launch.
pub(crate) fn load_config() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    let observation =
        match crate::native_config_service::VersionedConfigService::observe_path(&path, true) {
            Ok(observation) => observation,
            Err(error) => {
                eprintln!(
                    "aterm-gui: ignoring unreadable config {}: {error}",
                    path.display()
                );
                return Config::default();
            }
        };
    let config: Config = toml::from_str(&observation.text).unwrap_or_else(|e| {
        eprintln!("aterm-gui: ignoring invalid config {}: {e}", path.display());
        Config::default()
    });
    warn_deprecated_display_font_spelling(&observation.text, &config);
    config
}

/// Resolve the glyph size in physical px with the canonical precedence
/// `$ATERM_FONT_PX > config.font_px > FONT_PX default`. Only finite values inside
/// `FONT_PX_MIN..=FONT_PX_MAX` are admitted; an invalid source falls through instead
/// of being clamped. Shared by startup (`main`) and live
/// hot-reload (`App::reload_config`) so a reload re-applies the SAME precedence —
/// an env override still wins after the user edits the config file.
pub(crate) fn resolve_font_px(config: &Config) -> f32 {
    resolve_font_px_with(
        std::env::var("ATERM_FONT_PX").ok().as_deref(),
        config.font_px,
    )
}

/// Whether a valid environment/config size pins the physical glyph size.
/// Merely authoring an invalid `font_px` must not disable the display-scaled
/// default: admission and explicitness deliberately share the same predicate.
pub(crate) fn font_px_is_explicit(config: &Config) -> bool {
    font_px_is_explicit_with(
        std::env::var("ATERM_FONT_PX").ok().as_deref(),
        config.font_px,
    )
}

/// ATERM_FONT_PX only counts as an explicit pin when the runtime would
/// actually admit it. A malformed or out-of-domain inherited value falls
/// through to config/default and must not accidentally disable HiDPI auto-size.
pub(crate) fn font_px_environment_override() -> Option<f32> {
    std::env::var("ATERM_FONT_PX")
        .ok()?
        .parse::<f32>()
        .ok()
        .filter(font_px_in_range)
}

/// Whether the GPU renderer should be requested at launch, resolved with the SAME
/// precedence the `main` backend-selection funnel uses so the running renderer and
/// the `--diagnose` / `--show-config` reports cannot drift on it. Most specific
/// first: `--cpu`/`$ATERM_CPU` force CPU; else `$ATERM_GPU` forces GPU; else config
/// `gpu = false`/`true` decides; else DEFAULT TO GPU on every platform (the CPU
/// renderer is the automatic fallback when no device initializes).
pub(crate) fn resolve_want_gpu(config: &Config) -> bool {
    resolve_want_gpu_with(
        std::env::var_os("ATERM_CPU").is_some(),
        std::env::var_os("ATERM_GPU").is_some(),
        config.gpu,
    )
}

/// Effective shell command captured by every newly-created session. The CLI
/// flag collapses into `ATERM_SHELL` before the app starts, so this resolver is
/// shared by startup and config reload: saved `shell` edits reach the next tab
/// without waiting for a process restart while the launch override keeps its
/// documented precedence.
pub(crate) fn resolve_shell_override(config: &Config) -> Option<String> {
    resolve_shell_override_with(std::env::var("ATERM_SHELL").ok(), config.shell.as_deref())
}

fn resolve_shell_override_with(env: Option<String>, configured: Option<&str>) -> Option<String> {
    env.filter(|value| !value.is_empty())
        .or_else(|| configured.map(str::to_string))
}

/// Pure precedence core for [`resolve_want_gpu`] with the two env presences and the
/// config value passed in explicitly, so it is deterministically unit-testable
/// without mutating process-global env. Mirrors the `main` funnel exactly.
pub(crate) fn resolve_want_gpu_with(
    force_cpu: bool,
    gpu_env: bool,
    config_gpu: Option<bool>,
) -> bool {
    !force_cpu
        && match (gpu_env, config_gpu) {
            (true, _) => true,
            (false, Some(explicit)) => explicit,
            (false, None) => true,
        }
}

/// Diff two configs for RESTART-ONLY keys — settings read once at process launch that
/// [`App::reload_config`] deliberately does NOT hot-apply — and return a human notice
/// for each that changed. Today that is the initial window grid (`columns`/`lines`): a
/// live reload must not snap the now-freely-resizable window back to its configured
/// size, so an edit lands on the *next* launch. Without this the edit is a silent
/// no-op ("I changed `columns` and nothing happened"); the returned lines ride the same
/// transient [`crate::config_notice::ConfigNotice`] banner as dropped-rule warnings.
///
/// PURE + total (no `self`, no I/O) so it unit-tests without a window — the same shape
/// as the keybinding `*_warn` helpers that already feed the banner. Env overrides
/// (`ATERM_COLUMNS`/`ATERM_LINES`) are intentionally ignored: they are fixed for the
/// process, so they can't change across a reload and never generate a spurious notice.
pub(crate) fn restart_notices(old: &Config, new: &Config) -> Vec<String> {
    let mut out = Vec::new();
    if old.columns != new.columns || old.lines != new.lines {
        out.push(
            "columns/lines applies on next launch (resize the window to change size now)"
                .to_string(),
        );
    }
    // The renderer backend (GPU vs CPU) is built once at launch; a live GPU↔CPU
    // swap would tear down every window's present surface. Same silent-no-op
    // class as columns/lines now that `gpu` is settable from the Settings model.
    if old.gpu != new.gpu {
        out.push(
            "gpu applies on next launch (the renderer backend is chosen at startup)".to_string(),
        );
    }
    out
}

/// Parse a non-zero `u16` from an environment variable, returning `None` when the
/// var is unset, empty, unparseable, or zero. Used to let `--columns`/`--lines`
/// (which set `ATERM_COLUMNS`/`ATERM_LINES`) override the config grid size while
/// keeping the same clamp + default fallback the config path already applies.
pub(crate) fn env_u16(key: &str) -> Option<u16> {
    std::env::var(key)
        .ok()?
        .parse::<u16>()
        .ok()
        .filter(|&n| n != 0)
}

/// Fresh-launch terminal width after the documented environment > config >
/// default precedence and the same safety clamp used by window construction.
pub(crate) fn resolve_initial_columns(config: &Config) -> u16 {
    env_u16("ATERM_COLUMNS")
        .or(config.columns)
        .unwrap_or(80)
        .clamp(20, 500)
}

/// Fresh-launch terminal height; the row twin of
/// [`resolve_initial_columns`]. A seamless handoff may supply a carried frame
/// ahead of this resolver, but a normal launch and `--show-config` share it.
pub(crate) fn resolve_initial_lines(config: &Config) -> u16 {
    env_u16("ATERM_LINES")
        .or(config.lines)
        .unwrap_or(24)
        .clamp(5, 300)
}

/// The EXPLICIT launch-time grid overrides alone — `$ATERM_COLUMNS`/`$ATERM_LINES`
/// (set by `--columns`/`--lines`), WITHOUT the config fallback the resolvers
/// above fold in. W3's cold-restore grid seed needs the distinction: an explicit
/// `aterm --columns 200` is a per-launch request that outranks the persisted
/// session's grid, while a config `columns` is a static default that restore
/// exists to supersede (config > manifest would mean quitting a resized window
/// never reopens at its own size, i.e. no size restore for anyone who sets the
/// key). Unclamped on purpose — callers clamp alongside the value they merge
/// with, keeping one clamp per decision.
pub(crate) fn explicit_initial_columns() -> Option<u16> {
    env_u16("ATERM_COLUMNS")
}

/// Row twin of [`explicit_initial_columns`].
pub(crate) fn explicit_initial_lines() -> Option<u16> {
    env_u16("ATERM_LINES")
}

/// An explicit render-scale override from `$ATERM_FORCE_SCALE` (set directly or by
/// the `--scale` flag). `Some(f)` for a finite, positive value; `None` when unset
/// or invalid. When set it overrides BOTH the headless 1.0 default and a real
/// window's `scale_factor()`, driving the auto-scaled font (`round(FONT_PX·f)`) and the
/// interior padding (`pad_for_scale(f)`) so an offscreen `image` capture renders at
/// the same DPI a real window of that scale would (e.g. `--scale 2` ≈ 2× Retina).
///
/// Resolved ONCE per process (`OnceLock`), like [`crate::headroom_override`]: this
/// sits on the redraw path — `apply_window_scale` calls it for every frame BEFORE
/// the repaint early-out — and `env::var` takes the process-wide env lock and
/// linearly scans the whole environ block, which is far too expensive to repeat at
/// frame rate for a launch-time-only knob. `--scale` therefore has to keep writing
/// the variable during CLI parsing (cli.rs), before `run()` — which it already does.
pub(crate) fn resolve_force_scale() -> Option<f64> {
    static FORCE_SCALE: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    *FORCE_SCALE.get_or_init(|| {
        std::env::var("ATERM_FORCE_SCALE")
            .ok()?
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && *f > 0.0)
    })
}

/// Pure precedence core for [`resolve_font_px`], with the `$ATERM_FONT_PX` env
/// value and the config value passed in explicitly so it is deterministically
/// unit-testable (no process-global env mutation). Order: a finite, in-range env
/// value wins; else a finite, in-range config value; else the built-in default.
/// A present-but-unparseable/out-of-range env value falls through to the config,
/// matching the startup `.parse().ok().or(config).filter(in_range)` chain.
pub(crate) fn resolve_font_px_with(env: Option<&str>, config: Option<f32>) -> f32 {
    // Filter EACH source by range independently so an out-of-range env value falls
    // through to a valid config value (as documented) instead of `.or(config)`
    // pinning the bad env value and then `.filter` collapsing straight to default.
    admitted_font_px(env, config).unwrap_or(FONT_PX)
}

fn font_px_in_range(value: &f32) -> bool {
    value.is_finite() && *value >= FONT_PX_MIN && *value <= FONT_PX_MAX
}

fn admitted_font_px(env: Option<&str>, config: Option<f32>) -> Option<f32> {
    env.and_then(|value| value.parse::<f32>().ok())
        .filter(font_px_in_range)
        .or(config.filter(font_px_in_range))
}

/// Pure explicit-pin counterpart of [`resolve_font_px_with`]. Keeping this on
/// the same admission helper prevents an invalid authored value from pinning
/// the 12px fallback and silently bypassing HiDPI auto-size.
pub(crate) fn font_px_is_explicit_with(env: Option<&str>, config: Option<f32>) -> bool {
    admitted_font_px(env, config).is_some()
}

/// Resolve one raw style against the exact catalog revision used by the host.
/// Builtins and aliases come exclusively from `prefs`; this function adds only
/// the dynamic `pack:<id>` domain and fail-closed classification.
pub(crate) fn resolve_trail_style(raw: &str, catalog: &TrailPackCatalog) -> ResolvedTrailStyle {
    let raw = raw.trim();
    if let Some(id) = raw.strip_prefix("pack:") {
        let id = id.trim();
        if id.is_empty() {
            return ResolvedTrailStyle {
                canonical: None,
                style: None,
                pack: None,
                issue: Some(TrailStyleIssue::EmptyPackId),
            };
        }
        return match catalog.get(id) {
            Some(pack) => ResolvedTrailStyle {
                canonical: None,
                style: Some(aterm_effects::cursor_glow::GlowStyle::Custom),
                pack: Some(pack),
                issue: None,
            },
            None => ResolvedTrailStyle {
                canonical: None,
                style: None,
                pack: None,
                issue: Some(TrailStyleIssue::MissingPack),
            },
        };
    }
    let Some(canonical) = crate::prefs::cursor_trail_style_canonical(raw) else {
        return ResolvedTrailStyle {
            canonical: None,
            style: None,
            pack: None,
            issue: Some(TrailStyleIssue::Unknown),
        };
    };
    if canonical == "off" {
        return ResolvedTrailStyle::off();
    }
    ResolvedTrailStyle {
        canonical: Some(canonical),
        style: Some(aterm_effects::cursor_glow::GlowStyle::parse(canonical)),
        pack: None,
        issue: None,
    }
}

fn finite_clamp_or_off(value: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        0.0
    }
}

fn brighten_cursor_color(color: u32, factor: f32) -> u32 {
    let channel = |shift: u32| ((((color >> shift) & 0xff) as f32) * factor).min(255.0) as u32;
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// Canonical cold-path construction of the engine's `GlowConfig`. The live
/// terminal and Settings renderer preview differ only in explicitly injected
/// geometry/theme facts (`dark_theme` and `head_dx`); preference semantics are
/// byte-for-byte shared here.
pub(crate) fn resolve_cursor_glow(
    inputs: CursorGlowInputs<'_>,
    style: ResolvedTrailStyle,
    theme_cursor: u32,
    dark_theme: bool,
    // The theme's resolved default foreground / background (`0x00RRGGBB`, or
    // `COLOR_UNSET` when the caller has none). Parameters rather than
    // constants for the same reason `dark_theme` is: this is the cold-path
    // construction, and the live values are folded per frame.
    theme_fg: u32,
    theme_bg: u32,
    head_dx: f32,
) -> aterm_effects::cursor_glow::GlowConfig {
    use aterm_effects::cursor_glow::{
        BEAM_DEFAULT_COLOR, COMET_DEFAULT_COLOR, GlowStyle, LASER_DEFAULT_COLOR,
        SPARKLE_DEFAULT_COLOR, style_has_beam_of,
    };

    // Invalid/off values carry a harmless concrete enum because GlowConfig is a
    // POD; `enabled = false` is the sole engine gate and no geometry is emitted.
    // Laser's default is STORM VIOLET (a night strike's white-violet flash —
    // the old electric yellow read yellow-green on dark themes); Sparkle's is
    // STARLIGHT GOLD (live review disliked the theme green riding the glitter
    // emitter cursor). An explicit `cursor_trail_color` overrides any.
    let glow_style = style.style.unwrap_or(GlowStyle::Lumen);
    let default_color = match glow_style {
        GlowStyle::Laser => LASER_DEFAULT_COLOR,
        GlowStyle::Beam => BEAM_DEFAULT_COLOR,
        GlowStyle::Comet => COMET_DEFAULT_COLOR,
        GlowStyle::Sparkle => SPARKLE_DEFAULT_COLOR,
        _ => theme_cursor & 0x00ff_ffff,
    };
    let color = inputs.color.unwrap_or(default_color) & 0x00ff_ffff;
    let accent = inputs
        .accent
        .map(|value| value & 0x00ff_ffff)
        .unwrap_or_else(|| brighten_cursor_color(color, 1.5));
    let beam_only = glow_style == GlowStyle::Beam;
    let intensity = finite_clamp_or_off(inputs.intensity, 0.0, 1.0);
    let radius = finite_clamp_or_off(inputs.radius, 0.0, 2.0);
    let style_token = style.canonical.unwrap_or(inputs.style_raw.trim());
    aterm_effects::cursor_glow::GlowConfig {
        theme_fg,
        theme_bg,
        enabled: inputs.enabled && style.style.is_some(),
        style: glow_style,
        color,
        accent,
        duration: std::time::Duration::from_millis(inputs.duration_ms.clamp(30, 2_000)),
        length: inputs.length.clamp(1, 512),
        intensity,
        radius: if beam_only { 0.0 } else { radius },
        ring: !beam_only && inputs.ring,
        dark_theme,
        beam: style_has_beam_of(glow_style, style_token),
        head_dx,
        pack: style.pack,
        // The host's taste dial; the engine fails it OFF on a non-finite value,
        // and the resolver has already clamped it into 0..=1.5 s.
        wake_persist_s: inputs.wake_persist_s,
        // The ribbon's presentation rides the RAW spelling, like the pet
        // companions do: `cursor_trail_style = "rainbow kitty tall"` (and its
        // siblings) opts into the banding-era tall body; every other rainbow
        // spelling gets the default 0.43 flat under-baseline strip.
        ribbon_tall: GlowStyle::style_names_tall_ribbon(inputs.style_raw),
    }
}

/// The lexicon build-time conflicts [`App::recompute_sparkle`] actually logs,
/// given the resolved scan options: a "requires `cjk_single_char = true`"
/// warning describes a surface that WILL scan once that opt-in is on, so it
/// is dropped when the resolved config enables it (the lexicon cannot see
/// scan options; the resolver can). The predicate is shared with the web
/// resolver (`EffectsPipeline::sparkle_lexicon_warnings`) so both paths
/// filter identically.
fn sparkle_logged_warnings(
    conflicts: &[String],
    cjk_single_char: bool,
) -> impl Iterator<Item = &String> {
    conflicts
        .iter()
        .filter(move |w| aterm_effects::pipeline::lexicon_warning_applies(w, cjk_single_char))
}

/// The GUI render authority that must move in lockstep with a fallible backend
/// rebuild during config hot-reload. Config reload applies many independent
/// settings before reaching the renderer; capturing only this slice lets a
/// rejected font/geometry change roll back without discarding successful input,
/// session, title-summary, or package changes from the same file edit.
struct ReloadRenderSnapshot {
    config: Config,
    default_font_px: f32,
    font_px: f32,
    font_px_explicit: bool,
    theme: Theme,
    font_family: Option<String>,
    text_shaping: aterm_render::TextShapingConfig,
    text_blending: aterm_render::TextBlending,
    font_thicken: bool,
    stem_gamma: f32,
    font_hinting: String,
    font_subpixel: String,
    font_variations: Vec<(u32, f32)>,
    font_weight_dark_nudge: f32,
    render_knobs: RenderKnobs,
    font_config: FontConfig,
    window_metrics: Vec<(WindowId, crate::MetricsView)>,
}

impl ReloadRenderSnapshot {
    fn capture(app: &App) -> Self {
        Self {
            config: app.config.clone(),
            default_font_px: app.default_font_px,
            font_px: app.font_px,
            font_px_explicit: app.font_px_explicit,
            theme: app.theme,
            font_family: app.font_family.clone(),
            text_shaping: app.text_shaping.clone(),
            text_blending: app.text_blending,
            font_thicken: app.font_thicken,
            stem_gamma: app.stem_gamma,
            font_hinting: app.font_hinting.clone(),
            font_subpixel: app.font_subpixel.clone(),
            font_variations: app.font_variations.clone(),
            font_weight_dark_nudge: app.font_weight_dark_nudge,
            render_knobs: app.render_knobs,
            font_config: app.font_config.clone(),
            window_metrics: app
                .windows
                .iter()
                .map(|(wid, ws)| (*wid, ws.metrics))
                .collect(),
        }
    }

    fn rollback(self, app: &mut App) {
        restore_render_config_fields(&mut app.config, &self.config);
        app.default_font_px = self.default_font_px;
        app.font_px = self.font_px;
        app.font_px_explicit = self.font_px_explicit;
        app.theme = self.theme;
        app.font_family = self.font_family;
        app.text_shaping = self.text_shaping;
        app.text_blending = self.text_blending;
        app.font_thicken = self.font_thicken;
        app.stem_gamma = self.stem_gamma;
        app.font_hinting = self.font_hinting;
        app.font_subpixel = self.font_subpixel;
        app.font_variations = self.font_variations;
        app.font_weight_dark_nudge = self.font_weight_dark_nudge;
        app.render_knobs = self.render_knobs;
        app.font_config = self.font_config;
        for (wid, metrics) in self.window_metrics {
            if let Some(ws) = app.windows.get_mut(&wid) {
                ws.metrics = metrics;
            }
        }

        // Theme colours are shared by the engine's default cells and the GUI
        // renderer. The engine was already live-applied earlier in reload_config;
        // re-derive it from the hybrid config (old render slice + all other new
        // settings) so those two surfaces remain one transaction too.
        let terminal_config = app
            .config
            .applied_terminal_config_for_with_assets(app.os_appearance, &app.config_assets.themes);
        for session in app.pool.iter() {
            term_lock(&session.term).apply_config(&terminal_config);
        }
        app.session_factory.terminal_config = Some(terminal_config);
        app.sparkle_dirty = true;
        app.rain_dirty = true;
    }
}

/// Restore precisely the config keys that feed [`ReloadRenderSnapshot`]. Keeping
/// the rest of the freshly parsed `Config` in place is load-bearing: a rejected
/// font face must not undo a simultaneous keybinding, scrollback, smart-title,
/// package-consent, or other independent live edit. Restoring these keys also
/// defeats reload dedupe, so the watcher's second edge can retry a transient
/// renderer failure instead of permanently accepting a config/backend mismatch.
fn restore_render_config_fields(target: &mut Config, previous: &Config) {
    macro_rules! restore {
        ($($field:ident),+ $(,)?) => {
            $(target.$field.clone_from(&previous.$field);)+
        };
    }
    restore!(
        font_px,
        foreground,
        background,
        cursor_color,
        selection_color,
        palette,
        theme,
        font_family,
        font_family_bold,
        font_family_italic,
        font_family_bold_italic,
        font_synthetic_style,
        fallback_fonts,
        symbol_font,
        emoji_font,
        ligatures,
        merged_ligatures,
        font_features,
        text_blending,
        font_thicken,
        font_variation,
        font_weight,
        font_weight_dark_nudge,
        stem_gamma,
        font_hinting,
        font_subpixel,
        line_height,
        adjust_baseline,
        adjust_underline_position,
        adjust_underline_thickness,
        underline_skip_descenders,
        minimum_contrast,
        selection_foreground,
        selection_inactive,
        cursor_break_ligatures,
        background_opacity,
        background_material,
        window_padding,
        window_padding_top,
    );
}

impl App {
    /// Rebuild the cached sparkle-words state ([`App::sparkle`]) from the current
    /// config and Serious Mode policy. Runs only when `sparkle_dirty` is set
    /// (startup or config reload) — never per frame — so the per-frame path neither
    /// re-resolves config nor recompiles the lexicon. A malformed user lexicon
    /// override is logged and the builtin is used (config is not discarded).
    pub(crate) fn recompute_sparkle(&mut self) {
        self.sparkle_dirty = false;
        self.sparkle = if !self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::WordDecorations)
        {
            None
        } else {
            self.prepared_sparkle.resolved.clone()
        };
    }

    /// Whether the Kitty Log RECORDER is on: `[sparkle_words.feline] log`
    /// (default true, config-file-only — `apply_prefs_edits` is top-level-only,
    /// like every sparkle key). Host-side gate: the effects engine always
    /// records and the drain sites drop when this is false (§F4.7). Master-off
    /// (`sparkle == None`) never reaches a drain at all. A couple of `Option`
    /// reads — cheap enough to resolve at each drain, no cache to invalidate.
    pub(crate) fn kitty_log_enabled(&self) -> bool {
        self.config
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.feline.as_ref())
            .and_then(|f| f.log)
            .unwrap_or(true)
    }

    /// The curse-BONK host gate (`[sparkle_words.profanity] bonk`, default
    /// ON) — the feline `log` twin: the effects engine always records its
    /// bounded cues, this gate decides whether the drain makes sound.
    /// Per-class/master sparkle enables need no re-check here — a disabled
    /// profanity class produces no occurrences, hence no cues.
    pub(crate) fn curse_bonk_enabled(&self) -> bool {
        self.config
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.profanity.as_ref())
            .and_then(|p| p.bonk)
            .unwrap_or(true)
    }

    /// The detonation-edge opt-in (`bonk_detonation`, default OFF): admits
    /// the on-screen `Detonated` cue kind, which fires for output content —
    /// off by default so the bonk keeps strict typed provenance.
    pub(crate) fn curse_bonk_detonation_enabled(&self) -> bool {
        self.config
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.profanity.as_ref())
            .and_then(|p| p.bonk_detonation)
            .unwrap_or(false)
    }

    /// Rebuild the cached matrix-rain PARAMETER resolve ([`App::rain`]) from
    /// the current config + live theme. Runs only when `rain_dirty` is set
    /// (startup, config reload, appearance flip, toggle) — never per frame.
    /// Parameters resolve UNCONDITIONALLY (defaults + theme derivation even
    /// with no `[matrix_rain]` table) because a per-session override can force
    /// rain ON over a disabled config; the ENABLED decision is per-session
    /// ([`App::session_rain_enabled`]) and read at the frame gate. The new
    /// config is pushed into every LIVE engine (`set_config` keeps the tick
    /// epoch — a hot reload never time-travels the field); engines are built
    /// lazily at the next effective-on tick and are deliberately NOT dropped
    /// here when the config disables rain — the render path's suspended/drain
    /// gate winds a now-off window's field down instead (and a session
    /// override may keep another window's field legitimately alive).
    pub(crate) fn recompute_matrix_rain(&mut self) {
        self.rain_dirty = false;
        let bg = self.theme.bg & 0x00FF_FFFF;
        let fg = self.theme.fg & 0x00FF_FFFF;
        let cfg = self.config.matrix_rain_params(bg, fg);
        self.rain = Some(cfg);
        for (wid, ws) in self.windows.iter_mut() {
            if let Some(e) = ws.matrix_rain.as_mut() {
                e.set_config(crate::rain_config_for_window(cfg, *wid));
            }
        }
    }

    /// Effective rain state for session `sid`: its runtime override when one
    /// was set (View ▸ Matrix Rain / `toggle_matrix_rain` / `aterm-ctl rain`),
    /// else the durable `[matrix_rain] enabled` config bit. The override is
    /// runtime-only (never persisted; it dies with the session) and WINS over
    /// a config reload until cleared — an explicit session gesture outranks a
    /// background file flip.
    pub(crate) fn session_rain_enabled(&self, sid: u64) -> bool {
        self.serious_mode_policy()
            .allows(crate::motion::SeriousEffect::MatrixRain)
            && self
                .pool
                .rain_override(sid)
                .unwrap_or_else(|| self.config.matrix_rain_enabled())
    }

    /// Set (or clear, `None`) the runtime rain override for session `sid`,
    /// then mark the resolve stale and wake every window whose FRONT session
    /// is `sid` so the flip presents on the very next frame. Turning a session
    /// off deliberately does NOT drop that window's engine here: the render
    /// gate's suspended/drain path clears the visible field on the next frame
    /// (instant off, exactly like alt-screen suppression) and the weather
    /// machine winds down to 0% idle; re-enabling resumes/rebuilds lazily.
    pub(crate) fn set_session_rain_override(&mut self, sid: u64, over: Option<bool>) {
        self.pool.set_rain_override(sid, over);
        // Ensure the parameter resolve exists before the next frame (a session
        // can force rain ON before any config ever enabled it).
        self.rain_dirty = true;
        for ws in self.windows.values() {
            if ws.front_terminal().is_some_and(|t| t.session == sid)
                && let Some(w) = ws.os_window.as_ref()
            {
                w.request_redraw();
            }
        }
        // An OPEN palette's "Matrix Rain" checkmark mirrors the front session's
        // effective state — re-resolve it now, whatever surface flipped the
        // override (ctl, menu bar, keybinding, the palette row itself), so the
        // repaint the redraw above triggers paints the POST-flip row.
        self.palette_refresh_live();
    }

    /// Toggle matrix rain for the FRONT session of window `wid` (the
    /// `toggle_matrix_rain` action, the View ▸ Matrix Rain menu item, the
    /// palette row, and `aterm-ctl rain toggle` all converge here): flip the
    /// session's current EFFECTIVE state into an explicit per-session
    /// override. This replaced the old app-global `rain_force_off` kill latch
    /// — the toggle now turns rain ON too (even over a disabled config), and
    /// only for the session being looked at. No-op when the window's front
    /// content is not a terminal (native tabs have no session to toggle).
    pub(crate) fn toggle_matrix_rain(&mut self, wid: crate::WindowId) {
        let Some(sid) = self.front_terminal(wid).map(|t| t.session) else {
            return;
        };
        let effective = self.session_rain_enabled(sid);
        self.set_session_rain_override(sid, Some(!effective));
    }

    /// The `rain` control-socket verb ([`crate::Wake::RainControl`]), acting on
    /// the focused window's FRONT session. `Ok` carries the wire tail after
    /// `OK ` — every op (including the writes) answers with the same one-line
    /// status shape so a driver reads the post-state without a second round
    /// trip: `config_enabled=<bool> session_override=<none|on|off>
    /// effective=<bool> engine=<none|live> active=<bool>
    /// scope=<window|focused-pane> focused=<bool> animating=<bool>` (the
    /// engine tail is the split-pane-audit honesty fix: the render state is
    /// observable, so a driver can SEE that a split renders rain in the
    /// focused pane only — and that an unfocused/Reduced window animates
    /// nothing at all, the W11 law). `Err` when no window is focused or the
    /// front content is not a terminal (native tabs have no session to
    /// toggle) — an honest refusal, never a silent no-op.
    pub(crate) fn rain_control(&mut self, op: crate::RainCtlOp) -> Result<String, String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no focused window".to_string());
        };
        let Some(sid) = self.front_terminal(wid).map(|t| t.session) else {
            return Err("front content is not a terminal (no session to act on)".to_string());
        };
        match op {
            crate::RainCtlOp::Status => {}
            crate::RainCtlOp::On => self.set_session_rain_override(sid, Some(true)),
            crate::RainCtlOp::Off => self.set_session_rain_override(sid, Some(false)),
            crate::RainCtlOp::Toggle => {
                let effective = self.session_rain_enabled(sid);
                self.set_session_rain_override(sid, Some(!effective));
            }
        }
        let over = match self.pool.rain_override(sid) {
            None => "none",
            Some(true) => "on",
            Some(false) => "off",
        };
        // ENGINE introspection (split-pane audit): `effective=true` alone once
        // lied in a split (the engine was hard-dropped and nothing could
        // render while every surface reported success). Rain now follows the
        // FOCUSED pane on composed frames, and this tail makes the actual
        // render state observable to a driver: whether this window holds a
        // live engine, whether it is actively raining/draining (`is_active` —
        // the wake-arming predicate), and which SCOPE the emission covers
        // (`window` = single-pane/zoomed original path, `focused-pane` =
        // split compose).
        let (engine, active, diag) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.matrix_rain.as_ref())
            .map_or(("none", false, String::new()), |e| {
                ("live", e.is_active(), format!(" {}", e.diag_line()))
            });
        let scope = if self
            .active_tree(wid)
            .is_some_and(|t| t.len() > 1 && !t.is_zoomed())
        {
            "focused-pane"
        } else {
            "window"
        };
        // The resolved MOTION facts (W11): rain is a serious animation, so an
        // unfocused window (or Reduced motion) emits NOTHING even while
        // `effective=true engine=live` — without these two fields "why isn't
        // it raining" is undebuggable over the socket (split-pane audit).
        // Resolved through the SAME fold the render tick uses — the
        // `motion_focus` recording pin plus `motion_policy`'s load-shed
        // latch — so the status can never claim animating=true while the
        // live policy is Reduced (post-merge re-audit).
        let raw_focused = self.windows.get(&wid).is_some_and(|ws| ws.focused);
        let focused = self.motion_focus(wid, raw_focused);
        let motion = self.motion_policy(focused);
        Ok(format!(
            "config_enabled={} session_override={} effective={} engine={} active={} scope={} \
             focused={} animating={}{}",
            self.config.matrix_rain_enabled(),
            over,
            self.session_rain_enabled(sid),
            engine,
            active,
            scope,
            focused,
            motion.animate(crate::motion::MotionEffect::MatrixRain),
            diag,
        ))
    }

    /// Throttled entry for live (interactive) window resizes. A width drag with a large
    /// scrollback rewraps the entire off-screen history per intermediate width on the
    /// event-loop thread, which hitches the drag. Apply the FIRST resize of a drag
    /// immediately (leading edge), then coalesce subsequent ones into `pending_resize`
    /// and apply only the latest on the trailing settle (`new_events`) — so the reflow
    /// runs at most ~20 Hz and once more for the final size, instead of per cell-width.
    ///
    /// Recorder determinism: a COALESCED size is never passed to `on_resize`, so the
    /// engine never resizes to it and the asciicast/temporal spine never records it. The
    /// engine only ever applies the throttled subset + the final size, in the same order
    /// relative to PTY output, so a recorded session replays byte-identically. Only
    /// interactive `WindowEvent::Resized` routes here; the control-socket `resize` verb
    /// (`apply_term_resize`, echo_to_window) stays immediate + exact.
    ///
    /// WIDTH IS WHAT THE THROTTLE IS FOR, and now that is what it costs. The whole
    /// justification above — "rewraps the entire off-screen history" — is a property
    /// of a COLUMN change: reflow is what rewrapping means. A height-only drag (the
    /// bottom edge, the common vertical resize) rewraps nothing; it adds or drops
    /// grid rows and pushes the difference into scrollback. Coalescing those at
    /// 20 Hz bought no work back and made the terminal body visibly trail the window
    /// edge by up to a throttle window before snapping — the vertical half of the
    /// "shredded, then redrawn" drag. So the throttle now gates on the reflow it
    /// exists to bound: a resize that leaves the column count alone applies
    /// immediately, and only genuine width changes coalesce.
    ///
    /// Columns are derived through the SAME [`Self::grid_dims_for`] law `on_resize`
    /// uses, off the window's authoritative `inner_size()` (the winit macOS `Resized`
    /// payload can lie — see `on_resize`), so this decision cannot disagree with the
    /// resize it is deciding about. Only `cols` is compared: `rows` additionally
    /// depends on the chrome headroom `on_resize` re-derives, while `cols` is a pure
    /// function of width, pad and cell width.
    ///
    /// Recorder determinism is unchanged: applying MORE of the drag's real sizes only
    /// adds genuine resizes to the spine in the order they happened; the engine still
    /// never sees a coalesced size.
    pub(crate) fn on_resize_throttled(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        let now = std::time::Instant::now();
        let reflows = self.resize_changes_columns(wid, size);
        let apply_now = !reflows
            || self.windows.get(&wid).is_none_or(|ws| {
                ws.last_resize_at
                    .is_none_or(|t| now.saturating_duration_since(t) >= crate::RESIZE_THROTTLE)
            });
        if apply_now {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.last_resize_at = Some(now);
                ws.pending_resize = None;
                ws.next_resize_settle = None;
            }
            self.on_resize(wid, size);
            // RE-ARM THE SETTLE AFTER A ROW-ONLY APPLY — it is the payer for the
            // active-tab-only scope this apply just ran under.
            //
            // A leading-edge apply runs with `resize_live_drag` set, which sizes only
            // the ACTIVE tab's panes and marks the window `panes_stale`; the other
            // tabs' engines and PTY winsize are settled by the eager AllTabs pass the
            // trailing settle performs. On the WIDTH path a settle is guaranteed —
            // the next coalesced event arms one. A row-only drag now coalesces
            // NOTHING, so without this nothing would ever arm one and background tabs
            // would sit at the pre-drag row count until the user switched to them.
            //
            // It must run AFTER `on_resize`: a committing `apply_term_resize` clears
            // both fields itself (a committed resize supersedes a stale coalesce), so
            // arming before the call would simply be wiped.
            //
            // Re-arming with the CURRENT size is not lossy for a still-pending width
            // change: `flush_pending_resize` -> `on_resize` re-reads the window's
            // authoritative `inner_size()`, so the settle always applies the geometry
            // the window really has, not whatever was recorded when it was armed.
            if !reflows
                && let Some(ws) = self.windows.get_mut(&wid)
                && ws.next_resize_settle.is_none()
            {
                ws.pending_resize = Some(size);
                ws.next_resize_settle = Some(now + crate::RESIZE_THROTTLE);
            }
        } else if let Some(ws) = self.windows.get_mut(&wid) {
            // Inside the throttle window: keep only the LATEST size and arm the trailing
            // reflow so the final size always lands even if the drag stops right here.
            ws.pending_resize = Some(size);
            let base = ws.last_resize_at.unwrap_or(now);
            ws.next_resize_settle = Some(base + crate::RESIZE_THROTTLE);
        }
    }

    /// Apply a coalesced resize whose trailing-settle deadline fired (`new_events`):
    /// reflow the final pending size once, and clear the throttle state.
    ///
    /// This is also where the drag's DEBT is settled. Every leading-edge apply during
    /// a drag runs under `resize_live_drag`, which sizes only the ACTIVE tab's panes
    /// and leaves the window `panes_stale`; the background tabs' engines and PTY
    /// winsize are owed an eager AllTabs pass, and this is the one place that pays it.
    pub(crate) fn flush_pending_resize(&mut self, wid: WindowId) {
        let Some(size) = self.windows.get(&wid).and_then(|ws| ws.pending_resize) else {
            return;
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_resize_at = Some(std::time::Instant::now());
            ws.pending_resize = None;
            ws.next_resize_settle = None;
        }
        self.on_resize(wid, size);
        // PAY THE DEBT EXPLICITLY. `on_resize` clears `panes_stale` only as a side
        // effect of `apply_term_resize` reaching `resize_panes`, and that call
        // early-returns whenever the derived grid already equals the applied one — so
        // a drag whose settle lands on a size already applied (the common case: the
        // hand stops, the final size was the last one applied) performed NO AllTabs
        // pass and stranded every background tab at the pre-drag geometry until the
        // user happened to switch to one. Deliberately gated on the flag rather than
        // run unconditionally: with a single tab `panes_stale` is never set (the
        // scope has nothing to defer), so this is inert for the common window.
        if self.windows.get(&wid).is_some_and(|ws| ws.panes_stale) {
            self.resize_panes_scoped(wid, false);
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
            w.request_redraw();
        }
    }

    pub(crate) fn on_resize(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        // AUTHORITATIVE SIZE. winit's macOS `Resized` payload is emitted from
        // `frameDidChange:` off the raw NSView frame, which winit itself documents as
        // unreliable ("the frame size may change without a window resize occurring") and
        // can momentarily report the WRONG size — observed after a file drag is REJECTED
        // (an image dragged from a browser puts image DATA, not a file URL, on the
        // pasteboard; macOS only accepts `NSFilenamesPboardType`, so the drop inserts
        // nothing): the drop's tracking-rect churn / nested `NSEventTrackingRunLoopMode`
        // fires a `frameDidChange` with a too-small frame. Trusting that here shrinks the
        // grid + swapchain and leaves the window "scrunched" — a smaller framebuffer
        // top-anchored in a full-size window, dead background below — and it STICKS,
        // because no further resize arrives to correct it (opening a tab, or any other
        // re-grid, is what heals it). Re-read the window's authoritative `inner_size()`
        // so a spurious payload can't stick; `pad_split` below already clamps cells to
        // ≥1, so even a genuine tiny size never yields a zero-dimension grid. Headless /
        // tests have no `os_window` and keep the caller's `size` (behaviour unchanged).
        let size = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_ref())
            .map_or(size, |w| w.inner_size());
        // FIRE-INTO-CHROME: fullscreen transitions collapse (and restore) the
        // titlebar band WITHOUT a scale change, so re-derive the headroom from
        // AppKit on every real resize of a chrome'd window (a cheap geometry
        // msg_send). macOS-only: elsewhere the band is always 0. Must run
        // BEFORE `grid_dims_for` so the rows law sees the new headroom.
        //
        // ACCEPTANCE (fullscreen-toggle draw-error fix): a Resized event can
        // fire MID-fullscreen-transition, when `contentLayoutRect` and the
        // content bounds disagree transiently — sampling then used to stick a
        // bogus band forever (head=0 windowed drew row 0 under the toolbar
        // chip; an inflated head in fullscreen starved rows above a dead
        // band). The vendored winit now emits one settled Resized per
        // completed transition (windowDid{Enter,Exit}FullScreen local patch),
        // and this block additionally fail-closes each sample: fullscreen
        // FORCES head 0 (the design invariant — the titlebar detaches); a
        // windowed chrome'd sample of 0 or beyond the sanity cap restores the
        // LAST-GOOD-WINDOWED band instead of committing the artifact. The
        // fallback is `last_windowed_band_pts`, NOT `head_pts`: the applied
        // `head_pts` is (correctly) 0 for the whole of a fullscreen stay, so
        // falling back to it on a bad exit sample could only ever "keep" 0 —
        // the memory slot is what still holds the real band to return to.
        if cfg!(target_os = "macos")
            && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone())
        {
            let fullscreen = w.fullscreen().is_some();
            let decorated = w.is_decorated();
            let measured_pts = self.apprt.titlebar_band_pts(&w);
            let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale);
            let (head_pts, band_memory_pts) = titlebar_band_decision(
                measured_pts,
                fullscreen,
                decorated,
                self.windows
                    .get(&wid)
                    .map_or(0.0, |ws| ws.last_windowed_band_pts),
            );
            let head = (head_pts * scale).round() as usize;
            if let Some(ws) = self.windows.get_mut(&wid) {
                // The memory writes unconditionally (it may need to catch up —
                // e.g. the first windowed sample after attach — while the
                // applied band is unchanged); the backend retune stays gated
                // on a real applied change.
                ws.last_windowed_band_pts = band_memory_pts;
                if ws.head_pts != head_pts || ws.metrics.head != head {
                    ws.head_pts = head_pts;
                    ws.metrics.head = head;
                    self.backend.set_head(head);
                }
            }
        }
        // C3 (Windows + Linux): the SYNTHETIC tab-band head, re-derived. Unlike
        // macOS's band this one is COMPUTED, not sampled — from the window's own cell box,
        // the live strip row count and the configured target — so it has to be
        // re-derived on the one event every input funnels through. And they do all
        // funnel through here: a font zoom and a config reload both re-grid every
        // window via `rebuild_backend` → `on_resize`; a `tab_strip_rows` edit calls
        // `on_resize` explicitly; a per-monitor DPI move produces the `Resized`
        // winit emits after `ScaleFactorChanged`. Must run BEFORE `grid_dims_for`,
        // which reserves the head out of the height (same ordering rule as macOS).
        //
        // ATTACHED windows only, mirroring every other head derivation in the file
        // (`refresh_all_window_metrics`, `on_scale_factor_changed`): a never-attached
        // window has no chrome to make room for, its record carries the sealed boot
        // band, and seeding a synthetic one would move the geometry every headless
        // capture/snapshot pins.
        //
        // A runtime `cfg!` and NOT a `#[cfg]` attribute — the same form the macOS
        // band block twelve lines above uses, and for the same reason `TabBandHeight::
        // PLATFORM_DEFAULT` is a `cfg!` const rather than two `#[cfg]` items: this is
        // the ONE non-test call site of the whole C3 config chain
        // (`synthetic_strip_head_px` → `tab_band_height_or_default` → `TabBandHeight`
        // → `synthetic_band_head_px`), so an attribute here would make every link
        // unreachable on macOS and hand the lint gate (`-D warnings`, tools/verify.sh)
        // six `dead_code` findings plus "field `tab_band_height` is never read" there.
        // As a runtime `false` the body still type-checks on every platform and still
        // costs nothing: on macOS `PLATFORM_DEFAULT` is `Compact`, so the law would
        // return 0 even if the branch DID run, and the optimiser drops it whole —
        // which is also the guard that matters: macOS's `ws.metrics.head` holds a
        // real MEASURED AppKit titlebar band, and letting this write run there would
        // clobber it with the law's 0. The Windows-only attach and L1-early-reveal
        // call sites can stay `#[cfg(windows)]` — one live root per platform is
        // enough, and Linux's attach path funnels through here.
        if cfg!(any(windows, target_os = "linux")) {
            let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale);
            let cell_h = self.win_cell_size(wid).1;
            let head = self.synthetic_strip_head_px(scale, cell_h);
            if let Some(ws) = self
                .windows
                .get_mut(&wid)
                .filter(|ws| ws.os_window.is_some())
                && ws.metrics.head != head
            {
                // Stored in POINTS beside the applied px, like the measured macOS
                // band, so the two SHARED re-derivations that read `head_pts`
                // (`on_scale_factor_changed`, `refresh_all_window_metrics`) can
                // reproduce a band for this window without knowing this law.
                //
                // POINTS ARE A LOSSY CARRIER FOR A DERIVED BAND, deliberately. The law
                // is `round(target·scale) − pad_top(scale) − rows·cell_h(scale)`, and
                // only the first term is proportional to `scale`; `round(head_pts·s')`
                // therefore reproduces the law at a NEW scale only while `pad_top` and
                // `cell_h` happen to move with the DPI too. Under `font_px_explicit`
                // (or `ATERM_FORCE_SCALE`) the font — and so `cell_h` — is pinned, and
                // the interim band is off by the residue. That is accepted, not
                // overlooked: `ScaleFactorChanged` is always followed by the `Resized`
                // winit emits for the new size, which lands right back here and
                // re-derives from the law, so the drift can survive at most one frame
                // and never reaches a committed geometry.
                ws.head_pts = head as f64 / if scale > 0.0 { scale } else { 1.0 };
                ws.metrics.head = head;
                self.backend.set_head(head);
            }
        }
        let (rows, cols) = self.grid_dims_for(wid, size);
        // Phase 0.5: route through the seam so the window-resize and the control
        // `resize` verb share the one clamp + apply path. `echo_to_window: false`
        // is the KEY (RES-1 regression fix): the window ALREADY has this size (the
        // user is dragging the edge / the WM resized us), so the seam applies the
        // term+PTY+framebuffer resize via `apply_term_resize` WITHOUT calling
        // `request_inner_size` — re-requesting the size would fight an interactive
        // edge-drag and risk a resize feedback loop. Only the `resize` VERB (no
        // window event of its own) sets `echo_to_window: true`. This is a transport
        // flag, not a `Source` branch — both a human-issued and a controller-issued
        // `resize` verb echo the same way.
        self.input(
            wid,
            InputEvent::Resize {
                rows,
                cols,
                echo_to_window: false,
            },
            Source::Human,
        );
    }

    /// The grid `(rows, cols)` window `wid` gets for raw window `size` — the
    /// PURE half of [`Self::on_resize`] (which also applies it), shared with
    /// [`Self::apply_window_scale`]'s safety-net gate so both derive from the ONE law.
    ///
    /// W12: grids THIS window from ITS OWN cell metrics (mixed-DPI) — a resize of a
    /// background, different-DPI window must divide by that window's cell box, not
    /// whichever window the shared renderer is currently activated to. The grid
    /// occupies the window MINUS the `2·pad` interior border — the inverse of
    /// `frame_px`; this is the PROVEN `aterm_render::pad_split` policy (maximal grid;
    /// the `0..cell-1` remainder is absorbed into per-edge theme-bg bands at present
    /// time, so the swapchain can be the RAW window size and the compositor never
    /// rescales) — the same `cells` as the historical `max(usable/cell, 1)`, now the
    /// law the ty model + lattice tests pin. The tab strip is reserved out of the
    /// terminal grid while always leaving at least one terminal row.
    /// `pub(crate)` so the LINUX INITIAL-FRAME SETTLE
    /// ([`Self::settle_initial_frame`], in `app_window`) can ask the ONE grid law
    /// "does this surface already carry the grid the attach asked for?" instead of
    /// re-deriving a private copy of it.
    pub(crate) fn grid_dims_for(&self, wid: WindowId, size: PhysicalSize<u32>) -> (u16, u16) {
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let cols = aterm_render::pad_split(size.width as usize, pad, cw).cells as u16;
        // FIRE-INTO-CHROME rows law: the chrome headroom (the titlebar band above
        // the padded grid) is reserved OUT of the height before fitting. Top and
        // bottom are explicit: tightening the top no longer silently expands the
        // bottom. Equal pads reduce to the historical `pad_split` cell count.
        let win_rows = crate::asymmetric_pad_cells(
            (size.height as usize).saturating_sub(self.win_head(wid)),
            self.win_pad_top(wid),
            pad,
            ch,
        ) as u16;
        let rows = win_rows.saturating_sub(self.tab_strip_rows).max(1);
        (rows, cols)
    }

    /// W1 (Wayland column-shave fix, fixwave5): the min inner size that puts
    /// the resize-increment lattice ON the whole-cell frame lattice for
    /// window `wid`.
    ///
    /// winit's Wayland backend snaps every INTERACTIVE resize to
    /// `min_inner_size + k·increment` (the X11 base-size convention, with the
    /// min standing in as the base). With the min at the arbitrary UX floor
    /// (164×98 logical), that lattice is incongruent with the frame law
    /// (`2·pad + cols·cell_w` wide), so a PURE-VERTICAL bottom-edge drag
    /// re-snapped the untouched width down by up to `cell_w − 1` px — below
    /// the exact fit — and shaved one column (the band residue absorbed
    /// asymmetrically by the snap). Anchoring the min to the frame lattice
    /// makes every snapped size an exact whole-cell frame, so an edge drag
    /// can never shave a column (or row) the user did not drag away.
    ///
    /// The historical 164×98 LOGICAL floor is kept as a lower bound: the
    /// returned min is the smallest lattice point at or above it, in PHYSICAL
    /// px — the same unit the increments are passed in, so winit's logical
    /// conversion treats base and step alike.
    // PLATFORM SCOPE, mirrored from the caller: the only call site is
    // `app_window.rs`'s `set_min_inner_size`, which is `#[cfg(target_os = "linux")]`
    // ("macOS/Windows never carried a whole-cell floor"). Without the same cfg here
    // the fn is dead on every other target and trips the dead-code gate.
    // `test` is in the set because this module's own tests call it on every host.
    #[cfg(any(test, target_os = "linux"))]
    pub(crate) fn whole_cell_min_size(&self, wid: WindowId) -> PhysicalSize<u32> {
        let (cw, ch) = self.win_cell_size(wid);
        let (cw, ch) = (cw.max(1), ch.max(1));
        let pad = self.win_pad(wid);
        let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale);
        let floor_w = (164.0 * scale).round().max(0.0) as usize;
        let floor_h = (98.0 * scale).round().max(0.0) as usize;
        // Width lattice: 2·pad + C·cell_w, C ≥ 1 — the inverse of
        // `grid_dims_for`'s `pad_split`.
        let base_w = 2 * pad + cw;
        let min_w = base_w + floor_w.saturating_sub(base_w).div_ceil(cw) * cw;
        // Height lattice: head + pad_top + pad + (R + strip)·cell_h, R ≥ 1
        // (the strip rows are spliced in as real grid rows — `grid_dims_for`).
        let base_h = self.win_head(wid)
            + self.win_pad_top(wid)
            + pad
            + (1 + usize::from(self.tab_strip_rows)) * ch;
        let min_h = base_h + floor_h.saturating_sub(base_h).div_ceil(ch) * ch;
        PhysicalSize::new(
            u32::try_from(min_w).unwrap_or(u32::MAX),
            u32::try_from(min_h).unwrap_or(u32::MAX),
        )
    }

    /// Whether applying `size` to window `wid` would change its COLUMN count —
    /// i.e. whether the resize about to be applied carries a scrollback rewrap.
    ///
    /// This is the throttle's real predicate (see [`Self::on_resize_throttled`]).
    /// It reads the window's authoritative `inner_size()` exactly as
    /// [`Self::on_resize`] does, because the winit macOS `Resized` payload is
    /// documented-unreliable and a spurious width there would otherwise coalesce a
    /// resize that is not actually a reflow (or vice versa). Headless windows
    /// (no `os_window`) keep the caller's `size`, matching `on_resize`.
    ///
    /// Comparing COLS only is what makes this agree with the resize it is deciding
    /// about even though `on_resize` re-derives the macOS titlebar band first: the
    /// band is reserved out of the HEIGHT, so it can move `rows` but never `cols`,
    /// which is a pure function of width, pad and cell width.
    ///
    /// Conservative on the unknown, and on disagreement: a window we cannot find is
    /// treated as reflowing, so an unrecognized id keeps the old throttled behaviour
    /// rather than bypassing a guard on a guess. The same applies to the one case
    /// where `ws.cols` cannot track the derived value — a grid outside
    /// `MAX_GRID_ROWS`/`MAX_GRID_COLS` is `RangeRejected` before `apply_term_resize`
    /// runs, so `ws.cols` never catches up and this reports "reflows" forever. That
    /// is the safe direction (the window keeps the throttle it had), not a defect.
    fn resize_changes_columns(&self, wid: WindowId, size: PhysicalSize<u32>) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return true;
        };
        let size = ws.os_window.as_ref().map_or(size, |w| w.inner_size());
        let (_rows, cols) = self.grid_dims_for(wid, size);
        cols != ws.cols
    }

    /// Live font zoom (Cmd-+/Cmd--/Cmd-0): rebuild the [`Backend`] at `px`, then
    /// re-grid for the new cell size in the SAME window (more/fewer rows+cols) and
    /// tell the PTY. A failed rebuild (GPU hiccup / no font) keeps the current size
    /// — zoom never crashes. No-op without a window (headless).
    pub(crate) fn set_font_px(&mut self, px: f32) {
        let _ = self.set_font_px_with(px, Self::rebuild_backend);
    }

    /// Transactional core of [`Self::set_font_px`]. Keeping the fallible rebuild
    /// behind an injected closure gives the rollback path a deterministic test:
    /// renderer construction can fail, but the App's font authority and every
    /// per-window metrics record must remain paired with the renderer that is
    /// still live.
    pub(crate) fn set_font_px_with(
        &mut self,
        px: f32,
        rebuild: impl FnOnce(&mut Self) -> bool,
    ) -> bool {
        let px = px.clamp(FONT_PX_MIN, FONT_PX_MAX);
        if (px - self.font_px).abs() < 0.5 {
            return true;
        }
        let previous_px = self.font_px;
        let previous_explicit = self.font_px_explicit;
        self.font_px = px;
        // A live zoom is an EXPLICIT font size, exactly like `$ATERM_FONT_PX` /
        // `config.font_px`: pin it so (a) HiDPI auto-scale (`apply_window_scale`) can't
        // revert the zoom on the next scale re-eval, and (b) `refresh_all_window_metrics`
        // resolves each window at THIS px (`applied`) instead of the scale-derived
        // default (`for_scale`) — without the pin an auto (non-explicit) window's zoom is
        // silently discarded.
        self.font_px_explicit = true;
        // W12: the per-window `MetricsView` is the pixel authority `win_cell_size` /
        // `win_pad` read — INCLUDING the re-grid inside `rebuild_backend`. Re-resolve it
        // to the new px FIRST, mirroring the config-reload path (which skips
        // `apply_window_scale` the same way, see `refresh_all_window_metrics`'s callsite);
        // otherwise the re-grid divides each window by the STALE cell box, so the grid +
        // PTY never resize (the zoom looks dead) while the rebuilt backend draws the
        // new-size glyphs into the old layout — bigger cells overflowing their slots, i.e.
        // "font size does nothing and the glyphs garble".
        self.refresh_all_window_metrics();
        if rebuild(self) {
            true
        } else {
            // `rebuild_backend` leaves the old renderer untouched on failure.
            // Restore its matching App/window metrics too; otherwise input,
            // geometry and the next frame would disagree about the cell box.
            self.font_px = previous_px;
            self.font_px_explicit = previous_explicit;
            self.refresh_all_window_metrics();
            false
        }
    }

    /// Commit the config-reload render transaction only after the backend accepts
    /// its candidate font/geometry state. The injected rebuild seam is the same
    /// deterministic failure hook used by the regression test; production passes
    /// [`Self::rebuild_backend`].
    fn finish_reload_render_transaction_with(
        &mut self,
        previous: ReloadRenderSnapshot,
        rebuild: impl FnOnce(&mut Self) -> bool,
    ) -> bool {
        if rebuild(self) {
            true
        } else {
            previous.rollback(self);
            aterm_log::warn!(
                "config reload: renderer rejected font/geometry changes; kept prior render state"
            );
            false
        }
    }

    /// Re-pin every memory-backed App-owned renderer setting onto a freshly
    /// constructed or re-faced backend. Path-backed font generations cross
    /// their own worker-only prepare/seal boundary before publication.
    pub(crate) fn pin_backend_render_config_core(&mut self) {
        self.backend.set_text_shaping(self.text_shaping.clone());
        self.backend.set_text_blending(self.text_blending);
        self.backend.set_font_thicken(self.font_thicken);
        self.backend.set_stem_gamma(self.stem_gamma);
        let hinting = self.font_hinting.clone();
        self.backend.set_font_hinting(&hinting);
        let subpixel = self.font_subpixel.clone();
        self.backend.set_font_subpixel(&subpixel);
        self.backend.set_line_height(self.render_knobs.line_height);
        self.backend
            .set_minimum_contrast(self.render_knobs.minimum_contrast);
        self.backend
            .set_selection_fg(self.render_knobs.selection_fg);
        self.backend
            .set_adjust_baseline(self.render_knobs.adjust_baseline);
        let (upos, uthick) = self.render_knobs.adjust_underline;
        self.backend.set_adjust_underline(upos, uthick);
        self.backend
            .set_underline_skip_descenders(self.render_knobs.underline_skip_descenders);
        // A combined opacity + font/geometry reload reaches this repin path
        // instead of `apply_render_knob`. End a windowed macOS swapchain tap before the
        // newly translucent renderer can present a frame whose compositor-owned
        // backdrop is absent from the raw recording.
        let _ = self.video_abort_translucent_swapchain(
            self.render_knobs.background_opacity,
            cfg!(target_os = "macos"),
        );
        self.backend
            .set_background_opacity(self.render_knobs.background_opacity);
        // `backend_kind_undecided` keeps a headless launch's UNREDEEMED GPU intent
        // out of these two "this run cannot do it" verdicts: the backend really is
        // the CPU renderer at this instant, but `ensure_pixel_backend` may still
        // install the device, and a deferral is not allowed to invent a diagnostic
        // the same launch would not have printed before.
        if !self.backend.is_gpu() && !self.backend_kind_undecided() {
            if self.render_knobs.background_opacity < 1.0 {
                warn_background_opacity_unimplemented_once();
            }
            if self.render_knobs.background_material != BackgroundMaterial::None {
                warn_background_material_unimplemented_once();
            }
        }
    }

    /// Rebuild the [`Backend`] from the CURRENT `self.font_px` + `self.theme`,
    /// re-grid the window for the new cell metrics, and repaint. The single proven
    /// rebuild path shared by live font-zoom ([`Self::set_font_px`]) and live
    /// config hot-reload ([`Self::reload_config`]) — a font-size OR theme change.
    /// A failed rebuild (GPU hiccup / no font) keeps the current backend, so a
    /// reload/zoom never crashes. No-op re-grid without a window (headless).
    #[must_use = "a failed rebuild leaves the prior renderer live and requires caller policy"]
    pub(crate) fn rebuild_backend(&mut self) -> bool {
        self.rebuild_backend_with_prepared(None)
    }

    fn rebuild_backend_with_prepared(
        &mut self,
        prepared: Option<(
            aterm_render::Renderer,
            crate::tray_raster::PreparedChromeFonts,
        )>,
    ) -> bool {
        // Preserve the interior padding across the rebuild — a fresh backend starts
        // at `pad == 0`, so a font-zoom / config-reload would otherwise drop the
        // border. (The pad is a device-px constant for the session's scale; it does
        // not change with the font size.) Ditto the chrome headroom: a device-px
        // constant for the window's titlebar band, independent of the font.
        let pad = self.backend.pad();
        let pad_top = self.backend.pad_top();
        let head = self.backend.head();
        let mut prepared_chrome = None;
        if let Some((mut renderer, chrome)) = prepared {
            prepared_chrome = Some(chrome);
            renderer.set_px(self.font_px);
            self.backend.ready_mut().install_prepared_font(
                renderer,
                self.font_family.clone(),
                self.theme,
            );
        } else if let Err(error) = self
            .backend
            .rebuild_font_from_admitted(self.font_px, self.theme)
        {
            eprintln!("aterm-gui: resident font generation rebuild failed: {error}");
            return false;
        }
        self.backend.set_pad(pad);
        self.backend.set_pad_top(pad_top);
        self.backend.set_head(head);
        // Re-apply every configured shaping/typography/render/font setting. In
        // particular line_height lands before the re-grid below.
        self.pin_backend_render_config_core();
        // Replacing WindowGpu below destroys any swapchain/virtual tap. End the
        // recording while that tap and its reply/output-dir ownership are still
        // coherent instead of leaving a deadline-backed zombie.
        let _ = self.video_abort_backend_rebuild();
        // The atlas/face changed, so every window's offscreen + dirty-gate are stale.
        // Reset the per-window GPU caches (the swapchain stays valid — same device) and
        // the introspection scratch, and force a repaint. NOTE: the swapchains and OS
        // windows are untouched, so no surface is orphaned.
        self.reset_gpu_window_caches();
        // Re-grid EVERY window that has an OS window for the new cell metrics (from
        // ITS OWN inner_size), then repaint it. At n==1 this is the one window —
        // identical to the old front-only re-grid. W1: also refresh each window's
        // whole-cell resize increments (the cell metrics just changed) and its
        // recorded raw pixel size, so the swapchain keeps tracking the window.
        let sized: Vec<(WindowId, PhysicalSize<u32>)> = self
            .windows
            .iter()
            .filter_map(|(wid, ws)| ws.os_window.as_ref().map(|w| (*wid, w.inner_size())))
            .collect();
        for (wid, size) in sized {
            // W12: resize increments are window/monitor geometry.  One shared
            // renderer cell box cannot be copied to every mixed-DPI window.
            let (cw, ch) = self.win_cell_size(wid);
            // The increment BASE must track the cell box too (fixwave5) — see
            // `whole_cell_min_size`. Computed before the `&mut` borrow below.
            #[cfg(target_os = "linux")]
            let min_size = self.whole_cell_min_size(wid);
            if let Some(ws) = self.windows.get_mut(&wid) {
                if size.width > 0 && size.height > 0 {
                    ws.win_px = Some(size);
                }
                if let Some(w) = ws.os_window.as_ref() {
                    w.set_resize_increments(Some(PhysicalSize::new(cw as u32, ch as u32)));
                    #[cfg(target_os = "linux")]
                    w.set_min_inner_size(Some(min_size));
                }
            }
            self.on_resize(wid, size);
            if let Some(ws) = self.windows.get_mut(&wid) {
                let window = ws.os_window.clone();
                // A live font/theme rebuild is an external visual stimulus.
                // Couple the recovery reset to its repaint edge and retain the
                // acknowledgement until a real present/drop.
                let _ = crate::rearm_present_and_request(&mut ws.present_retry, true, || {
                    if let Some(window) = window {
                        window.request_redraw();
                    }
                });
            }
        }
        // The primary face may have changed (family/zoom reload): re-hand the
        // resolved face + bold sibling to the chrome rasterizer.
        if let Some(chrome) = prepared_chrome {
            crate::tray_raster::set_prepared_chrome_fonts(chrome);
        } else {
            self.sync_chrome_fonts();
        }
        // M5: a rebuild re-pinned the bg opacity on the new face; keep the
        // window-level vibrancy (backdrop + opacity flip) in step on the GPU
        // backend. Idempotent — no-op at the solid default / off macOS.
        if self.backend.is_gpu() {
            self.apply_window_vibrancy();
        }
        true
    }

    /// Reset every per-window GPU cache (`WindowGpu`: offscreen + dirty-gate /
    /// scissor state) and the introspection scratch after the renderer's face
    /// or device changed, keeping the surface/monitor state (EDR headroom,
    /// reference-white scale, capture colour space) that is NOT a render cache.
    /// Extracted verbatim from [`Self::rebuild_backend_with_prepared`]; the H1
    /// fail-soft rebuild (`retry_attach_on_opaque_swapchain`) calls it too, where
    /// the device itself was replaced — a `Virtual` target holding old-device
    /// textures must not survive into the fresh context.
    pub(crate) fn reset_gpu_window_caches(&mut self) {
        self.introspect_gpu = aterm_gpu::WindowGpu::new();
        for ws in self.windows.values_mut() {
            if let Some(
                PresentTarget::Gpu { window_gpu, .. } | PresentTarget::Virtual { window_gpu },
            ) = &mut ws.present
            {
                // M3 phase B + capture colour: per-screen EDR headroom,
                // reference-white scaling, and the compositor's colour-space
                // tag survive the cache reset. They are surface/monitor state,
                // not render caches.
                let edr = window_gpu.edr_max();
                let sdr_white_scale = window_gpu.sdr_white_scale();
                let capture_color_space = window_gpu.capture_color_space();
                *window_gpu = aterm_gpu::WindowGpu::new();
                window_gpu.set_edr_max(edr);
                window_gpu.set_sdr_white_scale(sdr_white_scale);
                window_gpu.set_capture_color_space(capture_color_space);
            }
            ws.last_present = None;
            // Headless has no winit redraw edge to acknowledge: image capture
            // renders synchronously, while an in-flight Virtual recording owns
            // its own paced timer. Reopen its gate without manufacturing a fake
            // outstanding OS request. Windowed targets use the coupled helper
            // after their geometry is rebuilt by the caller.
            if ws.os_window.is_none() {
                let _ = ws.present_retry.on_external_stimulus();
            }
        }
    }

    /// Hand the renderer's resolved PRIMARY face bytes (+ the discovered real
    /// `-Bold` sibling) to the chrome rasterizer ([`crate::tray_raster`]), so the
    /// Settings/About/Palette overlays render in the USER'S terminal font — the
    /// embedded DejaVu stays strictly a per-char coverage fallback. Called after
    /// every backend (re)build; the overlays re-rasterize on their next repaint.
    pub(crate) fn sync_chrome_fonts(&mut self) {
        let (font_px, theme) = (self.font_px, self.theme);
        let (primary, bold, semantic) = match self.backend.ready_mut() {
            Backend::Cpu(r) => (
                r.chrome_primary_face(),
                r.chrome_bold_face(),
                r.fork_semantic_surface(font_px, theme),
            ),
            Backend::Gpu(g) => (
                g.chrome_primary_face(),
                g.chrome_bold_face(),
                g.fork_semantic_surface(font_px, theme),
            ),
        };
        crate::tray_raster::set_chrome_fonts(primary, bold, semantic);
    }

    /// Route ONE renderer knob change to EXACTLY ONE renderer call (W5) — the
    /// mapping the config round-trip test pins. Shared by startup (apply the
    /// non-default knobs onto the fresh backend) and hot-reload (apply the
    /// diff). `SelectionInactive` has no immediate backend call by design: the
    /// per-window present seam (`redraw_window`) folds the App-cached knob
    /// with THIS window's live focus every frame, so a mid-session config flip
    /// takes effect on the next present.
    pub(crate) fn apply_render_knob(&mut self, change: KnobChange) {
        match change {
            KnobChange::LineHeight(v) => self.backend.set_line_height(v),
            KnobChange::MinimumContrast(v) => self.backend.set_minimum_contrast(v),
            KnobChange::SelectionFg(v) => self.backend.set_selection_fg(v),
            KnobChange::SelectionInactive(_) => {} // folded at the present seam
            KnobChange::AdjustBaseline(v) => self.backend.set_adjust_baseline(v),
            KnobChange::AdjustUnderline(p, t) => self.backend.set_adjust_underline(p, t),
            KnobChange::UnderlineSkipDescenders(v) => {
                self.backend.set_underline_skip_descenders(v);
            }
            KnobChange::BackgroundOpacity(v) => {
                // The recording was admitted while the client was opaque. Abort
                // it before this live renderer change can append a translucent
                // raw layer under the recording's original opaque admission.
                let _ = self.video_abort_translucent_swapchain(v, cfg!(target_os = "macos"));
                self.backend.set_background_opacity(v);
                // M5 TRUE VIBRANCY: on the GPU backend the translucent present path
                // IS wired (PostMultiplied swapchain + NSVisualEffectView), so a
                // live opacity edit re-applies the window/Metal-layer opacity flip
                // and backdrop (`self.render_knobs` already holds the new value).
                // The CPU softbuffer surface is opaque with no non-opaque composite,
                // so a translucent value there stays honestly solid — the warn-once.
                if self.backend.is_gpu() {
                    self.apply_window_vibrancy();
                } else if v < 1.0 && !self.backend_kind_undecided() {
                    warn_background_opacity_unimplemented_once();
                }
            }
            // The window-level NSVisualEffectView backdrop is driven from the
            // resolved knobs: on the GPU backend re-apply it live (it only shows
            // when the window is also translucent); on the CPU backend it has no
            // consumer, so a non-`none` material warns once.
            KnobChange::BackgroundMaterial(m) => {
                if self.backend.is_gpu() {
                    // H1 (Windows Mica/Acrylic): mirror the knob onto the GPU
                    // renderer FIRST, so the redraw `apply_window_vibrancy`
                    // nudges reconciles the swapchain composite mode against
                    // the new value (Opaque ⇄ PreMultiplied on a DComp visual
                    // instance). Both directions matter live: material → none
                    // MUST go opaque (with no DWM backdrop installed behind the
                    // visual, margin alpha would expose the windows behind);
                    // none → material re-engages the margins — but only when
                    // the instance was BUILT visual (material set at launch).
                    // A reload onto a plain HWND-swapchain instance stays
                    // DWM-chrome-only, which `window_set_vibrancy` diagnoses.
                    if let Some(g) = self.backend.gpu_mut() {
                        g.set_backdrop_margins(m != BackgroundMaterial::None);
                    }
                    self.apply_window_vibrancy();
                } else if m != BackgroundMaterial::None && !self.backend_kind_undecided() {
                    warn_background_material_unimplemented_once();
                }
            }
        }
    }

    /// M5 TRUE VIBRANCY: (re)apply the window-level `NSVisualEffectView` backdrop
    /// and the window/Metal-layer opacity flip to EVERY attached window from the
    /// resolved `background_material` / `background_opacity` knobs (the source of
    /// truth on `self.render_knobs`). Called at window attach and on a live
    /// `background_opacity` / `background_material` reload. `translucent` is
    /// `background_opacity < 1.0`; the theme `bg` restores the opaque seamless-
    /// titlebar fill when translucency turns off. No-op off macOS (the Linux apprt
    /// stub does nothing). Each window is nudged to repaint so the GPU present
    /// reconciles the swapchain composite-alpha mode on its next frame.
    pub(crate) fn apply_window_vibrancy(&self) {
        let material = self.render_knobs.background_material;
        let translucent =
            aterm_render::vibrancy::is_translucent(self.render_knobs.background_opacity);
        let bg = self.theme.bg;
        let apprt = &self.apprt;
        for ws in self.windows.values() {
            if let Some(w) = ws.os_window.as_ref() {
                apprt.window_set_vibrancy(w, material, translucent, bg);
                w.request_redraw();
            }
        }
    }

    /// Live THEME-ONLY swap — the cheap sibling of [`Self::rebuild_backend`], shared
    /// by [`Self::reload_config`] (colour-only config edit) and
    /// [`Self::sync_app_theme_to_appearance`] (OS light/dark flip on a split theme).
    /// Glyphs are coverage masks coloured at draw time, so a fg/bg/cursor/selection
    /// change leaves the face, glyph atlas, and cell metrics all valid: push the new
    /// [`Theme`] onto the LIVE backend — no font re-parse, no atlas drop, no re-grid.
    /// Any font-px or family change must still go through `rebuild_backend`.
    ///
    /// The theme is NOT cell content, so every presentation cache that diffs on
    /// content would stale-serve the OLD colours on a byte-idle grid (selection
    /// band, idle cursor, padding border) and must be dropped here: the E3
    /// strip-row cache (+ macOS titlebar bg, kept in step with the terminal bg),
    /// the per-window CPU pixel cache / GPU prev-frame / `RepaintKey` gate, and the
    /// introspection readback scratch.
    pub(crate) fn apply_theme_live(&mut self, new_theme: Theme) {
        // Assign FIRST so later rebuilds/zooms bake the new theme.
        self.theme = new_theme;
        // PHOSPHOR: the rain ramp + §6 luminance derivation are computed from
        // this theme's bg/fg — an OS light/dark flip (this fn's
        // `sync_app_theme_to_appearance` caller never passes through
        // `reload_config`) must re-resolve them or the field keeps raining the
        // OLD theme's tints over the new background.
        self.rain_dirty = true;
        match self.backend.ready_mut() {
            Backend::Cpu(r) => r.set_theme(new_theme),
            Backend::Gpu(g) => g.set_theme(new_theme),
        }
        self.introspect_gpu.invalidate_present();
        // LINUX CSD: the header band's light/dark variant is resolved FROM this
        // terminal theme when config `window_theme` is Auto (chrome_theme_for_apprt),
        // so a live theme swap must re-push it or the header keeps the OLD side.
        // Resolved before the windows borrow; macOS/Windows deliberately skip the
        // re-push — their chrome already tracks via `window_set_background_color`
        // below / the DWM config policy, and their `window_set_appearance` does
        // strictly more work than a variant flip.
        #[cfg(target_os = "linux")]
        let linux_chrome_theme = self.chrome_theme_for_apprt();
        let bg = new_theme.bg;
        let apprt = &self.apprt;
        let toolbars = &self._toolbars;
        let strip_dark = crate::tab_bar::theme_is_dark(bg);
        for (wid, ws) in self.windows.iter_mut() {
            ws.last_strip_fp = None;
            ws.last_present = None;
            ws.cpu_cache.invalidate();
            if let Some(
                PresentTarget::Gpu { window_gpu, .. } | PresentTarget::Virtual { window_gpu },
            ) = &mut ws.present
            {
                window_gpu.invalidate_present();
            }
            if let Some(w) = ws.os_window.as_ref() {
                apprt.window_set_background_color(w, bg);
                #[cfg(target_os = "linux")]
                apprt.window_set_appearance(w, linux_chrome_theme);
                w.request_redraw();
            }
            // Keep the native toolbar strip's appearance pinned to the (possibly
            // flipped) theme darkness, so tab labels stay legible on the new backdrop.
            if let Some(handle) = toolbars.get(wid) {
                crate::toolbar::set_strip_dark(handle, strip_dark);
                crate::toolbar::set_active_tab_color(handle, self.config.active_tab_color_rgb());
            }
        }
        // This method is also called directly for OS light/dark flips outside
        // `reload_config`; refresh the semantic fork here so variable-font
        // dark-nudge and theme-dependent raster state never lag that path.
        self.sync_chrome_fonts();
    }

    /// HiDPI follow-through for `WindowEvent::ScaleFactorChanged` — a window moved to
    /// a display with a different scale factor (or its display's scale changed). winit
    /// hands us the new factor; re-derive the auto-scaled font (`round(FONT_PX·scale)`) and
    /// interior pad and rebuild, so glyphs stay crisp and correctly sized at the new
    /// DPI. This is the SAME derivation [`Self::attach_os_window`] runs once at window
    /// creation, now applied on the fly instead of being frozen at the creation DPI.
    ///
    /// Honored only for the AUTO font (no `$ATERM_FONT_PX` / `config.font_px`) and
    /// when no scale is force-pinned (`--scale` / `$ATERM_FORCE_SCALE` deliberately
    /// ignore the real monitor — a forced scale must render identically everywhere).
    /// A no-op when neither the font nor the pad would change (a spurious event, or
    /// the initial post-creation event whose scale `attach_os_window` already applied,
    /// or returning to a display at the same DPI).
    ///
    /// PER-WINDOW DPI: aterm renders every window through ONE shared backend, so the
    /// changed window's new scale is stored on ITS [`WindowState`] and applied to the
    /// shared backend lazily, at the top of that window's `redraw_window`
    /// ([`Self::apply_window_scale`]). This is what lets two windows on
    /// different-DPI monitors each render at their own scale — the backend is re-tuned
    /// to whichever window is drawing — instead of the most-recently-scaled window
    /// dictating the font size for all of them. We only record + request a redraw here;
    /// the natural `Resized` winit emits after a scale change re-grids the window.
    pub(crate) fn on_scale_factor_changed(&mut self, wid: WindowId, scale: f64) {
        // Explicit font or a force-pinned scale ⇒ DPI is intentionally fixed; ignore.
        if self.font_px_explicit || resolve_force_scale().is_some() {
            return;
        }
        // Configured LOGICAL padding, resolved before the `windows` borrow (the
        // per-window record stays a function of the window's OWN scale).
        let (pad_l, pad_top_l) = (
            self.config.window_padding_or_default(),
            self.config.window_padding_top_or_default(),
        );
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.scale = scale;
            // W12: refresh this window's per-window metric RECORD from its OWN scale
            // (the auto-derived font_px + pad), so `WindowState::metrics` tracks the
            // live DPI rather than staying frozen at the attach-time value. The shared
            // backend is still re-tuned lazily in `apply_window_scale`; this is the
            // per-window source of truth, not the render authority. The chrome
            // headroom re-derives from the attach-captured POINTS band at the new
            // scale — no AppKit round-trip (that is why `head_pts` is stored).
            //
            // ATTACHED windows only: a never-attached (headless) window's record
            // holds the sealed boot band (`--scale` pad / `$ATERM_HEADROOM_PX`
            // head); re-deriving pad/head from the incoming scale would wipe it and
            // re-open the frames=0 class the seal closed. Mirror the attach gate in
            // `refresh_all_window_metrics` / `apply_window_scale` (ba9f05db).
            if ws.os_window.is_some() {
                ws.metrics = crate::MetricsView::for_scale_padded(scale, pad_l, pad_top_l);
                ws.metrics.head = (ws.head_pts * scale).round() as usize;
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        // W5: re-derive the whole-cell RESIZE INCREMENTS from the metrics just
        // refreshed. The increments are a `PhysicalSize` the WM stores verbatim,
        // so a per-monitor DPI change (drag to the other monitor on Windows,
        // where this event is the WM_DPICHANGED translation) leaves the OLD
        // scale's cell box in force: a 150%→100% move keeps snapping edge drags
        // in ~26 px steps against a ~17 px cell — coarser than a whole cell, so
        // most stops leave a remainder and the padding bands go uneven. The
        // attach site and the font/theme rebuild both set increments; this event
        // was the one metrics-changing path that forgot to (a regression from
        // the W12 per-window-DPI rework, which replaced the heavy
        // `rebuild_backend` — increments refresh included — with the light
        // per-window record update above). `win_cell_size` reads the refreshed
        // `ws.metrics.font_px`, so the borrow must end first; the unattached
        // (headless) case is a no-op via the `os_window` map below, matching the
        // attach gate above. macOS re-applies increments itself at
        // `windowWillStartLiveResize`, so this is redundant-but-harmless there.
        let (cw, ch) = self.win_cell_size(wid);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.set_resize_increments(Some(PhysicalSize::new(cw as u32, ch as u32)));
            // Keep the increment BASE on the whole-cell lattice at the new
            // DPI too (fixwave5) — see `whole_cell_min_size`.
            #[cfg(target_os = "linux")]
            w.set_min_inner_size(Some(self.whole_cell_min_size(wid)));
        }
    }

    /// Re-tune the shared backend's font size + interior pad to window `wid`'s DPI
    /// scale before that window composes. Called at the top of `redraw_window`. This
    /// is the mechanism behind true per-window DPI on a single shared renderer: each
    /// window loads its own scale as it draws. GUARDED — a no-op (zero cost) when the
    /// backend is already tuned to this scale, so the common case (one window, or
    /// several windows at equal DPI) never rebuilds. A rebuild only happens when
    /// consecutive redraws cross a DPI boundary; two windows at DIFFERENT DPI both
    /// animating continuously would thrash the shared font atlas, but that is rare and
    /// correctness-preserving. Honors the auto-font / no-force-scale gate, matching
    /// [`Self::attach_os_window`]'s derivation.
    pub(crate) fn apply_window_scale(&mut self, wid: WindowId) {
        // CELL-PX-1, and FIRST for the same reason the pad/head re-tune below is
        // hoisted above the pinned-font early return: the cell pixel size the
        // engines report over DEC 1016 and size OSC 1337 images with is a property
        // of the window's resolved metrics regardless of HOW the font was pinned,
        // so an explicit `$ATERM_FONT_PX` / `config.font_px` / `--scale` must not
        // route around it. This is also the seam that corrects the BOOT session,
        // spawned before the backend build was joined (`cell_px: None`) — every
        // raster seam runs `apply_window_scale`, so the first frame fixes it.
        // Guarded by a per-window memo, so the steady state takes no engine locks.
        self.sync_cell_pixel_size(wid);
        // Pad + head are PER-WINDOW variants of the ONE shared backend regardless
        // of how the font size is pinned — re-tune them BEFORE the pinned-font
        // early return (adversarial review: with a pinned font, a Settings
        // window's attach-time `set_head(0)` otherwise de-tuned every chrome'd
        // window's head for good, composing the grid under the titlebar).
        // ATTACHED windows only: a headless (never-attached) window's metrics
        // were never applied — its record carries the scale-1 defaults, and
        // re-tuning the shared backend from them WIPED the boot-time truth
        // (`--scale N` pad and the `ATERM_HEADROOM_PX` band): the video tap
        // sized itself with the boot values, the first redraw shrank the frame,
        // and the dims gate early-stopped every headless recording at frames=0
        // (the latency audit's root cause). Headless has exactly one logical
        // window; the boot values ARE its per-window metrics.
        if let Some(m) = self
            .windows
            .get(&wid)
            .filter(|ws| ws.os_window.is_some())
            .map(|ws| ws.metrics)
        {
            if m.pad != self.backend.pad() {
                self.backend.set_pad(m.pad);
            }
            if m.pad_top != self.backend.pad_top() {
                self.backend.set_pad_top(m.pad_top);
            }
            if m.head != self.backend.head() {
                self.backend.set_head(m.head);
            }
        }
        if self.font_px_explicit || resolve_force_scale().is_some() {
            return;
        }
        // W12: this window's OWN resolved metrics (the per-window source of truth,
        // maintained at attach + on `ScaleFactorChanged`) are the authority. We
        // SELECT them on the shared renderer via the LIGHT `activate_px` — which
        // keeps every other window's warm glyph atlas resident (the sizes coexist
        // by `px_q`) — instead of the heavy `rebuild_backend` (drop-atlas + re-face)
        // this used to do. For the auto font `metrics == MetricsView::for_scale(scale)`,
        // so the resolved size is byte-identical to the old `round(FONT_PX·scale)`;
        // only the switch cost changed (no teardown, no thrash between two DPIs).
        let Some(m) = self.windows.get(&wid).map(|ws| ws.metrics) else {
            return;
        };
        // Backend already tuned to this window's metrics — nothing to do (guarded
        // no-op; the common case of one window / equal-DPI windows never switches).
        if (m.font_px - self.font_px).abs() < 0.5
            && m.pad == self.backend.pad()
            && m.pad_top == self.backend.pad_top()
            && m.head == self.backend.head()
        {
            return;
        }
        self.backend.set_pad(m.pad);
        self.backend.set_pad_top(m.pad_top);
        self.backend.set_head(m.head);
        // Track the ACTIVE (drawing) window's size on the App so the remaining
        // draw-path reads that consult `self.font_px` report THIS window's size,
        // and Cmd-0 resets to this scaled default rather than the tiny FONT_PX base.
        self.default_font_px = m.font_px;
        self.font_px = m.font_px;
        self.backend.activate_px(m.font_px);
        // The ACTIVE cell box just changed for this window: if its grid no longer
        // matches (`grid_dims_for`, the SAME law `on_resize` applies), re-grid it so
        // the composed frame can never exceed the glass — the belt-and-braces for any
        // path that moved `ws.metrics` without a paired re-grid, where painting the
        // new px into the old grid would chop the frame at both edges. GATED on a
        // real dims mismatch: the genuine multi-DPI flip (two windows on different
        // scales alternating redraws) has a consistent per-window grid, and an
        // unconditional `on_resize` would take every session's term mutex and rewrite
        // a shared session's `cell_pixel_size` on every alternating redraw
        // (`apply_term_resize` does that work before its equal-dims early return).
        if let Some((size, cur)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.win_px.map(|s| (s, (ws.rows, ws.cols))))
        {
            let (rows, cols) = self.grid_dims_for(wid, size);
            if (rows, cols) != cur {
                self.on_resize(wid, size);
            }
        }
    }

    /// HEADLESS boot, the ONE-geometry-authority seal: copy the APPLIED boot
    /// metrics (`--scale` pad, the `$ATERM_HEADROOM_PX` head band, the resolved
    /// font) into every never-attached window's per-window record. Without this
    /// the record keeps the scale-1 construction defaults, and the auto-font arm
    /// of [`Self::apply_window_scale`] (live when neither `--scale` nor an
    /// explicit font pins the size — the arm 4d27aae5's attachment gate does NOT
    /// cover) re-tunes the shared backend from those never-applied values on the
    /// FIRST redraw a recording drives: the video tap arms with the boot
    /// geometry, the first present composes without the band, and the dims gate
    /// kills the take at frames=0 — with the band then gone from every later
    /// `image`/recording too.
    pub(crate) fn seed_headless_boot_metrics(&mut self) {
        let m = crate::MetricsView::applied(
            self.font_px,
            self.backend.pad(),
            self.backend.pad_top(),
            self.backend.head(),
        );
        for ws in self.windows.values_mut() {
            if ws.os_window.is_none() {
                ws.metrics = m;
            }
        }
    }

    /// Toggle the window's full-screen state (View ▸ Enter Full Screen). Uses
    /// winit's borderless full-screen on the current monitor — the same path a
    /// future keybinding would use. No-op before a window exists.
    pub(crate) fn toggle_fullscreen(&self) {
        if let Some(w) = self.front().and_then(|ws| ws.os_window.as_ref()) {
            let next = match w.fullscreen() {
                Some(_) => None,
                None => Some(winit::window::Fullscreen::Borderless(None)),
            };
            w.set_fullscreen(next);
        }
    }

    /// The path-referenced half of a BYTE-EQUAL config reload: the parsed
    /// `aterm.toml` matches the applied one, but the applied state also includes
    /// the CONTENT of files the config names by path (trail-pack manifests, the
    /// sparkle lexicon, toy packs), and editing such a file then re-saving/
    /// `touch`ing `aterm.toml` is their documented hot-reload path. Compare the
    /// fresh content fingerprints against the applied worker generation
    /// ([`App::path_feed_fps`]):
    ///
    /// * unchanged ⇒ admission stays a FULL runtime no-op — the dedupe win is kept
    ///   intact (no engine re-diffs, no word-deco reset, no settings-popup
    ///   churn), while exact text authority still advances independently;
    /// * changed ⇒ arm `sparkle_dirty` so the next frame activates the already
    ///   compiled worker bundle, hard-reset the per-window word decorations ONLY
    ///   when the deco feed (lexicon/toys) changed — a trail-manifest edit
    ///   feeds cursor glow, never decorations — and request repaints so the
    ///   rebuild lands without waiting for organic damage. The remaining
    ///   reload side-effect storm is still skipped: the parsed config is
    ///   identical, so nothing else can have changed.
    fn refresh_path_feeds(&mut self, fresh: PathFeedFps) {
        if fresh == self.path_feed_fps {
            return;
        }
        let previous = std::mem::replace(&mut self.path_feed_fps, fresh);
        self.sparkle_dirty = true;
        if fresh.deco != previous.deco {
            // Retire only word-owned episodes. Cursor companions share this
            // renderer's atlas, but a lexicon rebuild is not their lifecycle
            // authority and must not teleport or rebake them.
            for ws in self.windows.values_mut() {
                ws.word_decos.hard_reset_words();
            }
        }
        for ws in self.windows.values() {
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Publish one already-admitted immutable asset generation to the App and
    /// every existing window. This is the sole post-construction writer of
    /// [`App::config_assets`]: callers hand it the exact outer Arc carried by a
    /// versioned config snapshot, and it performs only Arc clones plus scalar
    /// effects-state installation. All path reads and PNG decoding happened
    /// before the snapshot reached this seam.
    ///
    /// Pointer identity is intentional. A fresh outer catalog can represent a
    /// byte-identical `aterm.toml` whose same-path rainbow kitty file changed, or a
    /// theme-directory-only generation. Both must reach every window before
    /// the next capture/effect tick. Re-publishing the same Arc is a complete
    /// no-op, including no redraw request.
    pub(crate) fn publish_config_assets(
        &mut self,
        assets: std::sync::Arc<ConfigAssetCatalog>,
    ) -> usize {
        if std::sync::Arc::ptr_eq(&self.config_assets, &assets) {
            return 0;
        }
        self.config_assets = assets;
        let window_ids = self.windows.keys().copied().collect::<Vec<_>>();
        let installed = window_ids
            .into_iter()
            .filter(|id| self.install_window_config_assets(*id))
            .count();
        self.request_redraw_all_windows();
        installed
    }

    /// Publish a parsed theme-directory generation produced by the background
    /// watcher. No filesystem operation occurs here: the event loop swaps one
    /// immutable catalog, reapplies the engine palette/chrome, and fans the same
    /// snapshot to Settings and Manual diagnostics.
    pub(crate) fn reload_theme_catalog(&mut self, themes: std::sync::Arc<ThemeCatalog>) {
        let before = self.native_config_service.snapshot();
        let snapshot = self.native_config_service.replace_theme_catalog(themes);
        if snapshot.revision == before.revision {
            return;
        }
        self.theme_catalog_generation = self.theme_catalog_generation.saturating_add(1);
        self.publish_config_assets(std::sync::Arc::clone(&snapshot.assets));

        let applied = self
            .config
            .applied_terminal_config_for_with_assets(self.os_appearance, &snapshot.assets.themes);
        for session in self.pool.iter() {
            term_lock(&session.term).apply_config(&applied);
        }
        self.session_factory.terminal_config = Some(applied);

        let theme = self
            .config
            .theme_for_with_assets(self.os_appearance, &snapshot.assets.themes);
        let changed = (theme.fg, theme.bg, theme.cursor, theme.selection)
            != (
                self.theme.fg,
                self.theme.bg,
                self.theme.cursor,
                self.theme.selection,
            );
        if changed {
            self.apply_theme_live(theme);
        }
        self.publish_native_config_snapshot(&snapshot);
    }

    /// Apply a worker-prepared exact config observation after the watcher read
    /// `~/.config/aterm/aterm.toml`. Parsing, path-backed asset loading, and font
    /// preparation happen off the event loop; this side installs the immutable
    /// generation into every live session without a restart.
    ///
    /// VALIDATION / FAIL-SAFE: malformed, partial, unreadable, or unstable input
    /// never reaches this typed seam and cannot replace the running config. Valid
    /// UTF-8 malformed TOML still reaches Manual from the original observation so
    /// its diagnostics can repair the exact rejected bytes.
    ///
    /// PRECEDENCE (no regression): font size flows through [`resolve_font_px`] —
    /// the SAME `$ATERM_FONT_PX > config > default` order as startup — so an env
    /// override still wins after an edit. GPU is a launch-time decision and is NOT
    /// hot-swapped here (`self.use_gpu` is fixed); only font size, the renderer
    /// theme, and the engine `TerminalConfig` (scrollback/cursor/colours/palette,
    /// diffed by `Terminal::apply_config`) are re-applied.
    fn request_font_catalog_generation(
        &mut self,
        mut prepared: crate::native_config_service::PreparedConfigObservation,
    ) {
        let Some(primary) = self.backend.ready().primary_seed() else {
            self.native_config_service.mark_reconciliation_required();
            self.reject_config_watch_admission_for(
                &prepared.observation.baseline,
                crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
            );
            aterm_log::warn!("config reload: active renderer has no immutable primary font bytes");
            return;
        };
        let Some(previous_sources) = self.backend.admitted_font_sources() else {
            self.native_config_service.mark_reconciliation_required();
            self.reject_config_watch_admission_for(
                &prepared.observation.baseline,
                crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
            );
            aterm_log::warn!("config reload: active renderer font generation is unavailable");
            return;
        };
        let sequence = self.next_font_catalog_sequence.max(1);
        self.next_font_catalog_sequence = sequence.saturating_add(1);
        self.requested_font_catalog_sequence = sequence;
        if !std::sync::Arc::ptr_eq(&prepared.assets.themes, &self.config_assets.themes) {
            // Theme discovery can overtake a config worker. Trail/rainbow kitty and
            // Sparkle consumers do not depend on the theme catalog, so rebase
            // that Arc in memory instead of reopening any source.
            prepared.assets = std::sync::Arc::new(ConfigAssetCatalog {
                trail_packs: std::sync::Arc::clone(&prepared.assets.trail_packs),
                kitty_sprite: prepared.assets.kitty_sprite.clone(),
                wallpaper: prepared.assets.wallpaper.clone(),
                themes: std::sync::Arc::clone(&self.config_assets.themes),
                sparkle_spec_consumers: prepared.assets.sparkle_spec_consumers.clone(),
            });
        }
        let request = crate::native_font_catalog::Request {
            sequence,
            theme_generation: self.theme_catalog_generation,
            prepared,
            px: self.font_px,
            appearance: self.os_appearance,
            previous_family: self.font_family.clone(),
            previous_font_config: self.font_config.clone(),
            previous_sources,
            previous_variations: self.font_variations.clone(),
            previous_dark_nudge: self.font_weight_dark_nudge,
            primary,
        };
        if let Some(lane) = self.native_font_catalog.as_mut() {
            lane.request(request);
        } else if self.proxy.is_none() {
            self.finish_font_catalog_generation(crate::native_font_catalog::prepare(request));
        } else {
            self.native_config_service.mark_reconciliation_required();
            self.reject_config_watch_admission_for(
                &request.prepared.observation.baseline,
                crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
            );
            aterm_log::warn!(
                "config reload: font catalog worker unavailable; keeping current config"
            );
        }
    }

    pub(crate) fn finish_font_catalog_generation(
        &mut self,
        mut completion: crate::native_font_catalog::Completion,
    ) {
        if let Some(lane) = self.native_font_catalog.as_mut() {
            lane.worker_drained();
        }
        match crate::native_font_catalog::completion_disposition(
            self.requested_font_catalog_sequence,
            completion.sequence,
            self.theme_catalog_generation,
            completion.theme_generation,
        ) {
            crate::native_font_catalog::CompletionDisposition::Publish => {}
            crate::native_font_catalog::CompletionDisposition::RejectStaleConfig => return,
            crate::native_font_catalog::CompletionDisposition::ReprepareLatestTheme => {
                let path_feeds = PreparedPathFeedGeneration {
                    sparkle: completion.sparkle.clone(),
                    trail_packs: std::sync::Arc::clone(&completion.assets.trail_packs),
                    fingerprints: completion.path_feed_fps,
                };
                self.request_font_catalog_generation(
                    crate::native_config_service::PreparedConfigObservation {
                        observation: completion.observation,
                        config: completion.config,
                        values: completion.values,
                        assets: completion.assets,
                        path_feeds,
                    },
                );
                return;
            }
        }
        if let Some(fonts) = completion.fonts.as_mut() {
            fonts.renderer.set_px(self.font_px);
        }
        self.apply_prepared_config_generation(completion.into_generation());
    }

    #[cfg(test)]
    pub(crate) fn reload_config(&mut self) {
        // Re-read + strictly re-parse. A parse error (malformed/partial mid-edit
        // file) or an unreadable/absent file is REJECTED so the live config is
        // never replaced by defaults; the previous config stays intact.
        let Some(path) = config_path() else { return };
        // One stable read supplies Manual, strict parsing, and the versioned
        // service. Sharing the bytes and file-generation baseline closes the
        // watcher race where each consumer could previously see a different
        // edit. Malformed-but-UTF-8 bytes still reach a clean Manual buffer and
        // its diagnostics; the running Config changes only after strict parse.
        let observation = match crate::native_config_service::VersionedConfigService::observe_path(
            &path, false,
        ) {
            Ok(observation) => observation,
            Err(error) => {
                aterm_log::warn!(
                    "config reload: {error}; keeping current config and Manual document"
                );
                return;
            }
        };
        self.reload_config_observation(observation);
    }

    /// Apply the exact bounded generation sampled by the config host. Watcher
    /// callers must use this entry point instead of reopening the logical path:
    /// acknowledgement and application then refer to one immutable observation.
    pub(crate) fn reload_config_observation(
        &mut self,
        observation: crate::native_config_service::ConfigDiskObservation,
    ) {
        self.prepare_native_config_external_observation(observation);
    }

    /// Exact worker-prepared twin of [`Self::reload_config_observation`]. The
    /// supplied feed/rainbow kitty generation is immutable; a theme that overtook
    /// persistence is rebased in memory and guarded by the theme-generation
    /// ticket.
    pub(crate) fn reload_prepared_config_observation(
        &mut self,
        prepared: crate::native_config_service::PreparedConfigObservation,
    ) {
        if let Err(error) = self.refresh_open_config_editor_observation(&prepared.observation) {
            aterm_log::warn!("config reload: Manual refresh needs attention ({error})");
        }
        self.request_font_catalog_generation(prepared);
    }

    pub(crate) fn apply_prepared_config_generation(
        &mut self,
        generation: crate::native_font_catalog::PreparedConfigGeneration,
    ) {
        if self.native_config_inflight || self.native_config_service.reconciliation_required() {
            self.defer_prepared_config_generation(generation);
            if !self.native_config_inflight
                && let Err(error) = self.pump_native_config()
            {
                self.surface_native_config_lane_error(error);
            }
            return;
        }
        self.apply_prepared_config_generation_unfenced(generation);
    }

    /// Admit a deferred runtime generation after a reconciliation worker has
    /// sampled the same exact config baseline. The matching sample is the
    /// ordering proof that clears the write fence; routing this back through
    /// [`Self::apply_prepared_config_generation`] would see that still-closed
    /// fence, defer the same payload again, and reconcile forever.
    pub(crate) fn apply_reconciled_prepared_config_generation(
        &mut self,
        generation: crate::native_font_catalog::PreparedConfigGeneration,
    ) {
        debug_assert!(!self.native_config_inflight);
        debug_assert!(self.native_config_service.reconciliation_required());
        self.apply_prepared_config_generation_unfenced(generation);
    }

    fn apply_prepared_config_generation_unfenced(
        &mut self,
        generation: crate::native_font_catalog::PreparedConfigGeneration,
    ) {
        let crate::native_font_catalog::PreparedConfigGeneration {
            observation,
            config,
            values,
            assets: prepared_assets,
            path_feed_fps: fresh_feeds,
            sparkle: prepared_sparkle,
            fonts: mut prepared_fonts,
            warnings: mut font_prepare_warnings,
        } = generation;
        let admitted_baseline = observation.baseline.clone();
        let config_snapshot = match self.native_config_service.synchronize_observation_prepared(
            observation,
            config.clone(),
            values,
            prepared_assets,
        ) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.native_config_service.mark_reconciliation_required();
                self.reject_config_watch_admission_for(
                    &admitted_baseline,
                    crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
                );
                // The fence is re-armed here with no worker sample scheduled, so
                // anything already in the semantic queue would sit until some
                // unrelated event happened to pump. That is the abandonment that
                // reported itself as a 30s wedged event loop. It matters on this
                // arm specifically because
                // `apply_reconciled_prepared_config_generation` reaches it from a
                // reconciliation completion, i.e. from the exact probe a queued
                // control caller is parked behind.
                self.reject_pending_native_config(&format!(
                    "config generation could not be admitted: {error}"
                ));
                aterm_log::warn!("native config service rejected watcher snapshot: {error}");
                return;
            }
        };
        self.prepared_sparkle = prepared_sparkle;

        // SEMANTIC DEDUPE: re-applying an identical prepared config is pure
        // side-effect churn — engine
        // re-diffs on every tab, per-window word-deco hard resets, settings
        // popup cancels, a possible font/backend probe — so a parse that
        // equals the currently applied config stops here (the native snapshot
        // above still saw the latest raw text, so an in-place formatting-only
        // edit stays synced). Field-by-field `PartialEq`, not an mtime
        // heuristic: a `touch` with unchanged content is equally a no-op.
        //
        // BUT config equality is not the whole applied state: files referenced
        // BY PATH (trail-pack manifests, the sparkle lexicon, toy packs) are
        // part of it too, and touch-to-reload is their DOCUMENTED hot-reload
        // path — every reload used to re-read them. So a byte-equal parse
        // still refreshes those feeds when their content fingerprints drifted
        // (`refresh_path_feeds`); only a reload where BOTH the parsed config
        // AND the referenced files are unchanged is the full no-op.
        if config == self.config && prepared_fonts.is_none() {
            // Semantic equality does not mean transaction equality: comments,
            // formatting, and explicit-default edits still advance the native
            // service revision. Fan that snapshot out before returning so the
            // next Settings patch does not start from a stale base revision.
            self.publish_config_assets(std::sync::Arc::clone(&config_snapshot.assets));
            self.publish_native_config_snapshot(&config_snapshot);
            self.refresh_path_feeds(fresh_feeds);
            self.finish_native_config_external_admission(&admitted_baseline);
            return;
        }

        // Renderer construction is the one fallible live-apply step. Snapshot
        // its complete authority before any of the independent config edits below
        // are published; only this slice rolls back if font discovery/build fails.
        let reload_render_before = ReloadRenderSnapshot::capture(self);

        // Restart-only keys (window `columns`/`lines`): read once at launch and NOT
        // hot-appliable (a reload must not snap the now-resizable window back to its
        // configured size). Diff the OLD `self.config` against the freshly-parsed
        // `config` BEFORE `self.config` is overwritten below, so an edit that can't take
        // effect surfaces a transient banner instead of a silent no-op ("I changed
        // `columns` and nothing happened"). Folded into the reload `warns` further down.
        let restart_notices = restart_notices(&self.config, &config);

        // Window-padding regime diff, taken against the OLD config before it is
        // overwritten below: a changed `window_padding`/`window_padding_top`
        // must RE-GRID every attached window (the pad participates in the
        // `grid_dims_for` split) and repaint, not just wait for the next
        // organic redraw. Compared at the RESOLVED level so a no-op edit
        // (e.g. writing the explicit default) stays a no-op.
        let old_padding = (
            self.config.window_padding_or_default(),
            self.config.window_padding_top_or_default(),
        );

        // Scoped-collateral diffs, taken against the OLD `self.config` before it
        // is overwritten below: (a) whether the keys that FEED the word
        // decorations changed (the `[sparkle_words]` table / lexicon inputs, or
        // the theme its palette derives from) — only then is the per-window
        // `hard_reset` warranted; (b) whether the retired Settings test scaffold's editable
        // field CATALOGUE drifted (keys added/removed/reordered) — only then can
        // an open popup's anchor row point at the wrong control after the
        // rebuild. A value-only edit (e.g. rapid cursor-trail style switching
        // from the style popup itself) keeps every anchor stable, so the popup
        // stays open and the gesture stays live; the rebuild below still
        // reseeds every row's displayed value either way.
        let sparkle_feed_changed = deco_feed_changed(
            &self.config,
            &config,
            fresh_feeds.deco,
            self.path_feed_fps.deco,
        );
        self.path_feed_fps = fresh_feeds;
        let popup_anchors_drifted = {
            let old = crate::prefs::editable_fields(&self.config);
            let new = crate::prefs::editable_fields(&config);
            old.len() != new.len() || old.iter().zip(&new).any(|(a, b)| a.key != b.key)
        };

        // Engine-side config (scrollback/cursor/theme colours/palette). Clearing a
        // previously-set key reverts it: `applied_terminal_config()` rebuilds from a
        // fresh default each reload (the engine diffs via `apply_config`, so a no-op
        // delta is free) while ALWAYS pinning the engine default fg/bg to the theme,
        // so a revert lands on the themed background, never spec-black. Apply to EVERY
        // live tab — window-level config, like a resize — and refresh the factory so
        // future Cmd-T tabs inherit the new config.
        // THE TYPING-SOUND AUDITION from the reload path: a native-window pick
        // or a hand edit that CHANGES the voice plays one keystroke of it
        // (`typing_sound_to_audition_on_swap` — the in-app row already
        // auditioned at commit time and latched the same voice, so its own
        // reload is silent here; startup never reaches this branch — the
        // initial config is set at construction and a byte-equal watcher pass
        // returns above). Decided against the latch BEFORE the swap.
        let audition_voice = self.typing_sound_to_audition_on_swap(&config);
        // Retain the parsed config so a later OS light↔dark switch can re-resolve a
        // `dark:…,light:…` split theme without re-reading disk (see
        // `App::sync_app_theme_to_appearance`). Resolve the engine/renderer theme for
        // the CURRENT OS appearance so a reload preserves the active light/dark side.
        self.config = config.clone();
        // Secure Keyboard Entry is PROCESS-level (Carbon secure input), so a
        // config commit records the wish here, once, beside the swap — not per
        // window or per session (engagement is focus-gated in secure_input).
        // Idempotent: an unchanged value is free. The refusal, if any, is
        // carried to the `warns` banner below — a SECURITY toggle that
        // silently fails to take is the one failure this feature must never
        // have, and the config swap has already succeeded by this line, so
        // without the banner the Settings row would show ON over a protection
        // that is off.
        let secure_input_refusal =
            crate::secure_input::set_desired(self.config.secure_keyboard_entry_or_default()).err();
        if let Some(voice) = audition_voice {
            // After the swap, so the preview plays at the NEW volume/master and
            // under the new look — the settings the user just wrote.
            self.audition_typing_sound(voice);
        }
        // Selected-tab color override (`active_tab_color`): pinned UNCONDITIONALLY
        // on every reload — a pure tab-color edit changes neither theme nor font,
        // so no rebuild branch below would re-sync the native strip. The setter is
        // an idempotent atomic compare, so the no-change case costs nothing.
        {
            let override_rgb = self.config.active_tab_color_rgb();
            for handle in self._toolbars.values() {
                crate::toolbar::set_active_tab_color(handle, override_rgb);
            }
            // The in-grid strip caches painted rows behind a fingerprint that
            // does not carry the override; drop the cache so the next splice
            // repaints with the new active-tab color. The PRESENT early-out
            // must be bypassed too (post-merge re-audit): the RepaintKey's
            // strip term carries titles/count, not this override, so without
            // `last_present = None` a pure tab-color edit could sit invisible
            // until some other change forced a present (the E3 rebuild branch
            // already clears both).
            for ws in self.windows.values_mut() {
                ws.last_strip_fp = None;
                ws.last_present = None;
            }
        }
        // `motion = "reduced"` is a live accessibility edge. Reconcile it at the
        // same generation-admission point that publishes the new config: every
        // retained glide lands immediately on its pinned target and is dropped,
        // instead of surviving until another wheel input or deadline tick.
        self.settle_reduced_scroll_motion(std::time::Instant::now());
        // Serious mode is an effective App projection, not a rewrite of any
        // individual effect setting. Apply its edge immediately after publishing the
        // new source config so a disable restores values from THIS generation.
        self.apply_serious_mode(config.serious_mode_or_default());
        self.publish_config_assets(std::sync::Arc::clone(&config_snapshot.assets));
        self.publish_native_config_snapshot(&config_snapshot);
        // Smart-title Settings are live authority, not restart-only metadata: revoke
        // queued provider work immediately and repaint tab/window composition even if
        // every terminal is currently idle.
        self.reconfigure_title_summaries();
        // Tab status is live authority for the same reason: the policy is COPIED
        // into every per-session classifier at construction, so without this a
        // Settings edit would apply only to sessions opened afterwards, and the
        // event loop's wait deadline would still be serving the old interval.
        self.reconfigure_session_status();
        // Per-keystroke config caches (predictive_echo / cursor_trail_style): a reload
        // can change either, so drop the resolved values — they re-resolve on the next
        // keystroke. Keeps a live style/predict change taking effect immediately.
        self.predict_mode_cache = None;
        self.kitty_cursor_enabled_cache = None;
        // Sparkle words: a reload can change `languages`/category toggles/lexicon, so
        // mark the App cache stale (rebuilt before the next frame) and flush each
        // window's cached occurrence set — otherwise, on a byte-idle grid, the stale
        // scan would persist until the next damage epoch. Cheap; a no-op when off.
        self.sparkle_dirty = true;
        // PHOSPHOR: a reload can change any [matrix_rain] knob (or the theme
        // the ramp derives from) — mark the resolve stale; the recompute gate
        // pushes the new config into every live engine via `set_config`
        // (keeps the tick epoch) before the next frame. An open palette's
        // "Matrix Rain" checkmark reads the effective state (override else the
        // just-reloaded `enabled` bit) — re-resolve it so the row can't show
        // the pre-reload state until closed and reopened.
        self.rain_dirty = true;
        self.palette_refresh_live();
        // [packages] consent flags feed the Settings ▸ Packages projection;
        // re-publish (memory-only) so a flipped switch or a hand-edit shows on
        // the status card without waiting for the next worker pass.
        self.publish_native_packages_state();
        // Terminal/theme/config palette authority changed. A resident pet may
        // remain visible across the reload, so explicitly retire only its
        // appearance contrast sample; position, action and breed stay intact.
        for ws in self.windows.values_mut() {
            ws.cursor_pet.invalidate_colors();
        }
        // A Sparkle Words reload retires word episodes and done marks, but
        // preserves the independent cursor companion's placement and sprite
        // caches. Other config edits remain a complete no-op for this state.
        if sparkle_feed_changed {
            for ws in self.windows.values_mut() {
                ws.word_decos.hard_reset_words();
            }
        }
        // Keep any OPEN retired Settings test scaffold authoritative against the
        // freshly admitted worker generation: a live watcher observation rebuilds
        // the displayed control list from the new
        // values, preserving the selection/scroll. Uses the local `config` (not
        // `self.config`) so it doesn't double-borrow `self` against `windows`.
        let trail_pack_ids = self.config_assets.trail_packs.ids.clone();
        for ws in self.windows.values_mut() {
            let band = Self::settings_band(ws);
            let wrap = Self::settings_wrap(ws);
            if let Some(s) = ws.settings_mut() {
                // Close any open popup menu FIRST — but only when the field
                // catalogue actually DRIFTED (`popup_anchors_drifted` above)
                // or the Trail Pack id list changed (the trail-style picker's
                // option list is an open-time snapshot of the OLD pack ids —
                // committing from it could write a `pack:<id>` that no longer
                // exists): a menu/wheel is an open-time snapshot of the OLD
                // fields, and its anchor row index could then point at a
                // different control after the rebuild — committing from that
                // stale snapshot could write an outdated value into the wrong
                // key. With a stable catalogue (the common case: a value-only
                // edit, including the one THIS popup just committed) every
                // anchor still points at its control, so the popup survives
                // the reload — rapid sequential trail-style picks stay one
                // fluid gesture instead of the menu slamming shut ~500 ms
                // after every choice.
                if popup_anchors_drifted || s.trail_pack_ids != trail_pack_ids {
                    s.menu_cancel();
                    s.wheel_cancel();
                }
                s.trail_pack_ids.clone_from(&trail_pack_ids);
                // The rebuild re-clamps `scroll` PER MODE (grouped `scroll` is a
                // GroupRow index, not a field index — `scroll.min(selected)` would
                // compare incommensurable units and yank the band after every save).
                s.rebuild_fields(crate::prefs::editable_fields(&config), band, wrap);
            }
        }
        let applied_tc = config.applied_terminal_config_for_with_assets(
            self.os_appearance,
            &config_snapshot.assets.themes,
        );
        for s in self.pool.iter() {
            term_lock(&s.term).apply_config(&applied_tc);
        }
        self.session_factory.terminal_config = Some(applied_tc);
        // `allow_kitty_file_transfer` is a session-factory opt-in (not part of
        // `TerminalConfig`), so refresh it here too — otherwise a reload's new value
        // would only reach tabs spawned BEFORE restart. Mirrors the startup default.
        // (Existing sessions keep their spawn-time policy; new Cmd-T tabs pick this up.)
        self.session_factory.allow_kitty_file_transfer =
            config.allow_kitty_file_transfer.unwrap_or(false);
        // Shell policy is also spawn-time, not process-time: preserve the
        // launch CLI/environment override, otherwise publish the reloaded
        // config to the factory so the next Cmd-T session uses it. Existing
        // PTYs necessarily keep the command they were spawned with.
        self.session_factory.shell_override = resolve_shell_override(&config);
        self.session_factory
            .shell_args
            .clone_from(&config.shell_args);
        // `temporal_recording` is likewise a session-factory opt-in (not part of
        // `TerminalConfig`); refresh it on reload so a live edit reaches new Cmd-T
        // tabs. Existing sessions keep their spawn-time wiring. Mirrors the default.
        self.session_factory.temporal_recording = config.temporal_recording.unwrap_or(false);
        // `search_history_lines` drives the process-global search index depth cap (read
        // by both ⌘F and the socket `search` verb). Re-push it on reload so an edit applies
        // live — the cap is part of the index cache key, so the next search rebuilds at the
        // new depth instead of waiting for a restart.
        crate::control::set_search_max_lines(search_index_depth(&config));

        // Input-level config (no backend rebuild): the keybinding table and the
        // Option-as-Meta flag are re-applied live so an edit takes effect on the next
        // keypress. Use `resolved_warn` (NOT `from_config_warn`) so a reload keeps the
        // platform DEFAULTS overlaid, matching `App::new` — on Linux those defaults seed
        // copy/paste/new-tab/etc., so a defaults-less reload would silently kill them.
        // Clearing the user `[keybindings]` table now reverts to the platform defaults
        // (empty on macOS, where the hardcoded Cmd-* path is the convention); dropping
        // `option_as_meta` restores the default (Meta) — both diff-free when unchanged.
        let (keybindings, mut warns) =
            keybinding::Keybindings::resolved_warn(config.keybindings.as_ref());
        let (key_sequences, ks_warns) =
            keybinding::KeySequences::from_config_warn(config.key_sequences.as_ref());
        self.keybindings = keybindings;
        self.key_sequences = key_sequences;
        warns.extend(ks_warns);
        // Restart-only edits (columns/lines) ride the SAME banner as dropped-rule
        // warnings — both are "your edit didn't fully take effect" messages.
        warns.extend(restart_notices);
        // …and so does a refused Secure Keyboard Entry transition (see the
        // apply site above): same class exactly — an edit that did not take.
        if let Some(status) = secure_input_refusal {
            warns.push(format!(
                "secure_keyboard_entry: the OS refused the change (OSStatus {status}) —                  Secure Keyboard Entry is NOT {}",
                if self.config.secure_keyboard_entry_or_default() {
                    "on"
                } else {
                    "off"
                }
            ));
        }
        // W5h: an unresolvable `font_family` warns (like themes) instead of
        // silently reducing to the built-in candidates. Uses the same
        // effective family (env > config > platform default) the rebuild will try.
        let requested_effective_family = prepared_fonts
            .as_ref()
            .and_then(|prepared| prepared.family.clone())
            .or_else(|| self.font_family.clone());
        warns.append(&mut font_prepare_warnings);
        // An unrecognized `cursor_trail_style` silently disables the whole cursor
        // effect (the `glow_config` gate) — warn on the same banner instead of
        // letting the typo'd style just make the trail vanish.
        warns.extend(
            config_snapshot
                .assets
                .trail_packs
                .diagnostics
                .iter()
                .map(|warning| format!("config {warning}")),
        );
        if let Some(w) = config.cursor_trail_style_warning(&config_snapshot.assets.trail_packs) {
            warns.push(format!("config {w}"));
        }
        if let Some(reason) = config_snapshot.assets.kitty_sprite.diagnostic() {
            let source = config_snapshot
                .assets
                .kitty_sprite
                .source_id()
                .unwrap_or("configured source");
            warns.push(format!(
                "config cursor_nyan_sprite {source:?} invalid: {reason}"
            ));
        }
        // An unadmittable wallpaper silently renders as no backdrop — surface
        // the decode verdict on the same banner (the kitty-sprite rule).
        if let Some(reason) = config_snapshot.assets.wallpaper.diagnostic() {
            let source = config_snapshot
                .assets
                .wallpaper
                .source_id()
                .unwrap_or("configured source");
            warns.push(format!("config wallpaper {source:?} invalid: {reason}"));
        }
        // W6: re-resolve the per-style / fallback font keys (families → paths),
        // riding the same banner for unresolvable entries. The diff against the
        // cached resolved config decides below whether the backend is touched.
        let new_font_config = prepared_fonts
            .as_ref()
            .map(|prepared| prepared.config.clone())
            .unwrap_or_else(|| self.font_config.clone());
        for w in &warns {
            eprintln!("aterm-gui: {w}");
        }
        // Surface dropped rules / restart notices in-window. A fresh (possibly `None`)
        // notice also CLEARS a stale banner once the config is fixed; repaint to reflect.
        self.config_notice =
            crate::config_notice::ConfigNotice::new(warns, std::time::Instant::now());
        for ws in self.windows.values() {
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
        self.option_as_meta = config.option_as_meta_or_default();
        self.confirm_multiline_paste = config.confirm_multiline_paste_or_default();
        // Copy-on-select is a live input-policy toggle: a reload that flips it takes
        // effect on the next selection (dropping the key restores the on-by-default
        // policy).
        self.copy_on_select = config.copy_on_select_or_default();

        // Tab-strip rows are window chrome: a change re-splits the window between the
        // strip and the terminal, so re-grid (like a resize) if it changed.
        let new_strip = resolve_tab_strip_rows(&config);
        if new_strip != self.tab_strip_rows {
            self.tab_strip_rows = new_strip;
            // The strip is GLOBAL, but `tab_segments`/`last_present` are per-window:
            // clear each window's cache + force a repaint, and re-grid each from its
            // own OS window size (the strip now takes more/fewer rows).
            let sized: Vec<(WindowId, PhysicalSize<u32>)> = self
                .windows
                .iter_mut()
                .filter_map(|(wid, ws)| {
                    ws.tab_segments.clear();
                    ws.last_strip_fp = None; // E3: strip geometry changed
                    ws.last_present = None;
                    ws.os_window.as_ref().map(|w| (*wid, w.inner_size()))
                })
                .collect();
            for (wid, size) in sized {
                self.on_resize(wid, size);
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
            }
        }

        // Window chrome appearance (titlebar light/dark/auto): re-apply live so a
        // `window_theme` edit takes effect without a restart, AND update the cached
        // field so windows opened AFTER the reload use the new value (attach reads
        // `self.window_theme`). Cross-platform: AppKit/DWM/winit each consume the
        // same value. Light/Dark re-apply is idempotent; Auto clears the forced
        // appearance and resumes the platform's system-theme behavior.
        let new_window_theme = config.window_theme_or_default();
        if new_window_theme != self.window_theme {
            self.window_theme = new_window_theme;
            // The chrome PAINTERS resolve from this field too (Linux:
            // `chrome_palette_theme` feeds the tab band's strip tones and the
            // native pages' role palette), and the strip row cache's
            // fingerprint carries no theme term — `apply_theme_live` clears it
            // by hand on a theme swap, so a `window_theme` edit must do the
            // same or the band stale-serves the old side on a byte-idle grid.
            // Native page rasters re-lower via the compile stamp (the resolved
            // chrome theme is a paint-revision term); the present caches are
            // dropped here so the rebuilt pixels actually reach glass.
            for ws in self.windows.values_mut() {
                ws.last_strip_fp = None;
                ws.last_present = None;
                ws.cpu_cache.invalidate();
                if let Some(
                    PresentTarget::Gpu { window_gpu, .. } | PresentTarget::Virtual { window_gpu },
                ) = &mut ws.present
                {
                    window_gpu.invalidate_present();
                }
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        {
            // macOS/Windows receive the raw Light/Dark/Auto policy (Auto keeps
            // following the live OS appearance through the platform seam); on Linux
            // the one resolution seam maps Auto onto the terminal theme's darkness
            // so the CSD header tracks the body (see `chrome_theme_for_apprt`).
            let chrome_theme = self.chrome_theme_for_apprt();
            let apprt = &self.apprt;
            for ws in self.windows.values() {
                if let Some(w) = ws.os_window.as_ref() {
                    apprt.window_set_appearance(w, chrome_theme);
                }
            }
        }

        // GPU-present colour space (M3): re-tag each GPU window's CAMetalLayer so a
        // `window_colorspace` edit takes effect without a restart, and cache the
        // value for windows attached after the reload. Re-tagging is cheap,
        // idempotent, and interpretation-only (bytes/readback untouched), applied
        // only on an actual change. Routed through the SAME proven precedence as
        // attach (`resolve_surface_colorspace`): an HDR (Rgba16Float) window keeps
        // its extended-linear tag — its pixels ARE linear; the config tag would
        // mis-render them. No-op off macOS / on the CPU present path.
        let new_colorspace = config.window_colorspace_or_default();
        if new_colorspace != self.window_colorspace {
            self.window_colorspace = new_colorspace;
            let apprt = &self.apprt;
            for ws in self.windows.values_mut() {
                let Some(w) = ws.os_window.as_ref() else {
                    continue;
                };
                let Some(PresentTarget::Gpu {
                    gpu_surface,
                    window_gpu,
                }) = ws.present.as_mut()
                else {
                    continue;
                };
                let surface_colorspace = crate::platform::resolve_surface_colorspace(
                    new_colorspace,
                    gpu_surface.is_hdr(),
                );
                let effective_colorspace =
                    apprt.window_set_surface_colorspace(w, surface_colorspace);
                // Keep capture's explicit source metadata in the same update as
                // the compositor tag. A P3 retag without this write would turn
                // the next unprofiled PNG into mislabeled wide-gamut bytes. A
                // failed retag leaves the old platform tag in effect, so retain
                // the prior known metadata rather than claiming the request won.
                let previous_capture_space = window_gpu.capture_color_space();
                let capture_space = crate::platform::capture_space_after_surface_tag(
                    effective_colorspace,
                    previous_capture_space,
                );
                window_gpu.set_capture_color_space(capture_space);
            }
        }

        // M3 phase B: the EDR glow opt-in, live. Turning it OFF gates the >1.0
        // aurora pass off on the NEXT present of every window (the plan follows
        // `hdr_glow` per frame — proven safe on a still-f16 swapchain, whose blit
        // keeps linear-decoding with the grid clamped at reference white).
        // Turning it ON also admits an existing f16-capable Windows SDR surface
        // to the renderer's throttled output-HDR probe; configure+scRGB tag must
        // both succeed before the next present can use the HDR arms (the
        // exhaustive hdr_gate law).
        // Heat-shimmer hot-reload gate, resolved BEFORE the `&mut` backend
        // borrow below: skipped while the load-shed latch holds the shimmer
        // off — the latch transition (app_render) restores the configured
        // value on recovery.
        let shimmer_reload = if self.serious_mode_enabled() {
            Some(false)
        } else if self.load_shed_active() {
            None
        } else {
            Some(config.cursor_fire_shimmer_or_default())
        };
        // Bloom on/off is the shimmer's parity class under the shed latch: while
        // shed holds it off, a reload must not re-enable it (the latch transition
        // in app_render restores the configured value on recovery). The
        // strength/radius PARAMS are latch-independent (the shed gates the pass,
        // not its tuning), so they always re-push — previously none of the three
        // `cursor_trail_bloom*` keys hot-reloaded at all.
        let bloom_reload = if self.serious_mode_enabled() {
            Some(false)
        } else if self.load_shed_active() {
            None
        } else {
            Some(config.cursor_trail_bloom_or_default())
        };
        if let Backend::Gpu(g) = self.backend.ready_mut() {
            g.set_hdr_glow(config.hdr_glow_or_default());
            // SDR crown budget hot-reload: takes effect on the next present (the
            // budget is re-derived per present from the live background luma).
            g.set_sdr_glow_boost(config.cursor_glow_sdr_boost_or_default());
            // Heat-shimmer hot-reload: applies on the next present.
            if let Some(on) = shimmer_reload {
                g.set_shimmer(on);
            }
            // Bloom hot-reload (on/off + strength/radius): next present.
            if let Some(on) = bloom_reload {
                g.set_bloom(on);
            }
            g.set_bloom_params(
                config.cursor_trail_bloom_strength_or_default(),
                config.cursor_trail_bloom_radius_or_default(),
            );
            // EFFECT-PIPELINE WARM-UP, and the ONE seam that gets it.
            //
            // The nine effect-only cell pipelines are demand-driven: a launch
            // builds none of them (136.13 ms of dx12 shader compiles a default
            // config never draws with), and `encode_frame` builds whichever the
            // frame in front of it actually binds. That is correct for every
            // enable path — a config reload, the Settings UI, an editor that
            // starts emitting undercurls — but the frame that FIRST draws a
            // newly-enabled effect would pay the compile inline, and for
            // EMBERFORGE fire that is `fire_add` + `fire_over` = 111.07 ms on
            // one frame. Visible.
            //
            // This is a config APPLY, which is exactly and only where an effect
            // can be switched on by a human: not the launch path (that one runs
            // `pin_gpu_effect_config`, which deliberately does NOT warm), and not
            // a frame. Warming here puts every pipeline in place several frames
            // before the first quad that needs one, where the beat a reload
            // already costs hides it and nothing is animating. Idempotent, so
            // every reload after the first is nine `is_some` checks.
            //
            // GATED ON THE TRAIL MASTER, because an UNCONDITIONAL warm here just
            // relocates the defect this whole change removes: it would hand the
            // `cursor_trail = false` owner the same 136 ms of compiles they never
            // draw with, moved off the launch and onto their first config save.
            // `cursor_trail` is the master for the entire cursor-glow family
            // (`glow_config` folds it into `GlowConfig::enabled`), and that family
            // is the sole producer of the only four pipelines whose inline build
            // is a VISIBLE hitch: `fire_add`+`fire_over` (111.07 ms together) and
            // `rain_glow`+`rain_glow_over` (17.70). With it off none of those four
            // can be bound at all, so there is nothing to hide.
            //
            // The five that remain reachable with the trail off — `deco_over`
            // (2.72 ms; an undercurl from any editor, no config knob gates it),
            // `sprite_over` (3.26; wallpaper/pets), `deco_add` (2.06),
            // `glow_add` (1.64), `cursor_blend` (0.40) — are each well under one
            // 16.7 ms frame, so the demand path absorbs them where they happen and
            // a warm would buy nothing. Correctness never rides on this either
            // way: `encode_frame` ensures whatever the frame binds regardless, so
            // a wrong guess here costs at most one sub-frame compile, never a
            // missing effect.
            if config.warms_effect_pipelines() {
                g.warm_effect_pipelines();
            }
        }

        // Focus-boost hot-reload: re-fold the want-set NOW (it reads the just-
        // replaced `self.config`), so flipping `focus_boost` while a window is
        // already focused applies immediately — OFF un-boosts every session via
        // the empty want-set, ON boosts the currently visible ones — instead of
        // waiting for the next focus/tab change.
        self.recompute_focus_boost();

        // GUI-level: renderer theme (window clear colour, cursor, selection),
        // font size, and font family. Rebuild the backend ONLY when something it
        // bakes in actually changed (theme, resolved font px, or family) — a
        // metadata-only save (e.g. a comment edit) then costs nothing visible.
        let new_theme =
            config.theme_for_with_assets(self.os_appearance, &config_snapshot.assets.themes);
        // Re-derive the AUTO default font with the SAME HiDPI logic
        // `attach_os_window` / `on_scale_factor_changed` use, so editing an
        // unrelated key (e.g. a colour) on a Retina display does NOT shrink the
        // font back to the FONT_PX (12px) base. An admitted explicit env/config font
        // is honored verbatim (and re-pins `font_px_explicit`).
        let font_explicit_now = font_px_is_explicit(&config);
        let new_default_font_px = if font_explicit_now {
            resolve_font_px(&config)
        } else {
            let scale = resolve_force_scale()
                .or_else(|| {
                    self.front()
                        .and_then(|ws| ws.os_window.as_ref())
                        .map(|w| w.scale_factor())
                })
                .unwrap_or(1.0);
            if scale > 1.0 {
                (FONT_PX * scale as f32)
                    .round()
                    .clamp(FONT_PX_MIN, FONT_PX_MAX)
            } else {
                FONT_PX
            }
        };
        // Only re-apply the font when the derived default ACTUALLY changed, so a
        // live Cmd-+/Cmd-- zoom survives an unrelated config edit (and Cmd-0 still
        // resets to the up-to-date scaled default). A metadata-only save is then a
        // true no-op (new == old) instead of forcing a font shrink.
        let default_changed = (new_default_font_px - self.default_font_px).abs() >= 0.5;
        self.default_font_px = new_default_font_px;
        let new_font_px = if default_changed {
            new_default_font_px
        } else {
            self.font_px
        };
        // THE PIN: a surviving live zoom (effective px away from the scale default)
        // must KEEP `font_px_explicit` — see `reload_font_pin`. The old
        // unconditional `= font_explicit_now` dropped a Cmd-+/− zoom's pin while
        // preserving its px, re-arming `apply_window_scale`, which then flipped the
        // backend to the scale default at the next redraw (a tab switch was enough)
        // WITHOUT a re-grid — bigger cells painted into the zoom-fitted, wider
        // grid: a frame wider than the window, chopped at both edges.
        self.font_px_explicit =
            reload_font_pin(font_explicit_now, new_font_px, new_default_font_px);
        // `Theme` is a 4×u32 POD without `PartialEq`; compare its fields directly
        // (the renderer bakes these in, so any change needs a backend rebuild).
        let theme_changed = (
            new_theme.fg,
            new_theme.bg,
            new_theme.cursor,
            new_theme.selection,
        ) != (
            self.theme.fg,
            self.theme.bg,
            self.theme.cursor,
            self.theme.selection,
        );
        let font_changed = (new_font_px - self.font_px).abs() >= 0.5;
        // Effective family keeps the env > config > platform-default precedence on
        // live reload too (the SAME `effective_font_family` as startup and the
        // `--show-config` diagnostics, so the `family_changed` diff below can never
        // see a spurious change from resolution drift): a `--font`/$ATERM_FONT
        // override set at launch stays in force across a config reload rather than
        // being clobbered by the reloaded config `font_family`.
        // A rejected configured family is not a request to swap to a built-in
        // face. Keep the last admitted family/face in the app-render artifact and surface the exact
        // admission failure above. A later valid edit advances normally.
        let effective_family = requested_effective_family;
        let family_changed = effective_family != self.font_family;
        // Text shaping (ligatures / font_features). Update the source of truth BEFORE
        // a possible rebuild so `rebuild_backend` re-applies the NEW shaping; if no
        // rebuild is needed we push it through directly below.
        let new_shaping = config.text_shaping();
        let shaping_changed = new_shaping != self.text_shaping;
        self.text_shaping = new_shaping;
        // W2 typography knobs (`text_blending` / `font_thicken` / `stem_gamma`
        // / `font_hinting`): same source-of-truth-before-rebuild discipline.
        // All four change glyph APPEARANCE but not cell CONTENT (hinting never
        // moves the linear advances — the hinted-seam contract), so a
        // knob-only edit rides the shaping branch below (backend push +
        // per-window present-cache invalidation).
        let new_blending = config.text_blending_or_default();
        let new_thicken = config.font_thicken_or_default();
        let new_stem_gamma = config.stem_gamma_or_default();
        let new_font_hinting = config.font_hinting_or_default();
        let new_font_subpixel = config.font_subpixel_or_default();
        let typography_changed = new_blending != self.text_blending
            || new_thicken != self.font_thicken
            || (new_stem_gamma - self.stem_gamma).abs() > f32::EPSILON
            || new_font_hinting != self.font_hinting
            || new_font_subpixel != self.font_subpixel;
        self.text_blending = new_blending;
        self.font_thicken = new_thicken;
        self.stem_gamma = new_stem_gamma;
        self.font_hinting = new_font_hinting;
        self.font_subpixel = new_font_subpixel;
        // W5 renderer knobs: PURE diff — one KnobChange per changed key, each
        // routed to exactly one renderer call (`apply_render_knob`). A
        // line_height change alters the CELL GEOMETRY (every window re-grids),
        // so it takes the full rebuild path below — which re-pins all knobs —
        // while the others ride the appearance-only push branch.
        let new_knobs = RenderKnobs::from_config(&config);
        let knob_changes = self.render_knobs.diff(&new_knobs);
        self.render_knobs = new_knobs;
        // W6 font config: update the source of truth before installing the
        // already-prepared renderer generation. Resolving families, admitting
        // files, and parsing faces all happened on the catalog worker.
        let font_cfg_changed = new_font_config != self.font_config;
        self.font_config = new_font_config;
        // W9 variable-font requests: same source-of-truth-before-rebuild
        // discipline. A change re-instantiates the primary, which can move
        // the CELL GEOMETRY (a heavier wght can shift MVAR metrics), so it
        // takes the full rebuild path below — like `line_height`. The
        // malformed-entry warnings were already surfaced at startup; a
        // reload re-parses silently (unchanged behaviour for other keys).
        let (new_variations, _vf_warns) = config.font_variation_requests();
        let new_dark_nudge = config.font_weight_dark_nudge_or_default();
        let variations_changed = new_variations != self.font_variations
            || (new_dark_nudge - self.font_weight_dark_nudge).abs() > f32::EPSILON;
        self.font_variations = new_variations;
        self.font_weight_dark_nudge = new_dark_nudge;
        let geometry_knob_changed = knob_changes
            .iter()
            .any(|c| matches!(c, KnobChange::LineHeight(_)));
        // Theme-only edits fall to the live fast path below; a rebuild is reserved
        // for changes the backend actually bakes in (font px/family, cell geometry,
        // or a variable-font re-instantiation).
        let prepared_font_refresh = prepared_fonts.is_some();
        let backend_rebuild_needed = font_changed
            || family_changed
            || geometry_knob_changed
            || variations_changed
            || font_cfg_changed
            || prepared_font_refresh;
        // W12 ORDERING (mirrors `set_font_px`): commit the effective px and re-resolve
        // every window's per-window metric authority BEFORE any rebuild re-grids —
        // `on_resize` divides each window by `win_cell_size` (= `ws.metrics`), so a
        // refresh AFTER the rebuild (the old tail call) left the re-grid dividing by
        // the STALE cell box, and the grid never matched the rebuilt glyphs.
        self.font_px = new_font_px;
        self.refresh_all_window_metrics();
        // LIVE-APPLY a padding edit: the refresh above re-resolved every
        // attached window's `metrics.pad`/`pad_top` from the new config, and
        // `apply_window_scale` (top of `redraw_window`) re-tunes the shared
        // backend from that record — but the GRID still holds the old pad's
        // cell count, and only the front window would organically repaint. So
        // when the resolved padding regime changed without an encompassing font /
        // geometry rebuild, re-grid each sized window
        // (`on_resize` divides the SAME glass size by the refreshed metrics —
        // the one `grid_dims_for` law) and request a redraw everywhere, which
        // routes through `apply_window_scale` to push the new pads. A combined
        // rebuild defers this: success re-grids every window in rebuild_backend,
        // while failure restores the exact prior metrics without ever changing
        // the grid/PTY dimensions.
        let new_padding = (
            config.window_padding_or_default(),
            config.window_padding_top_or_default(),
        );
        if !backend_rebuild_needed && old_padding != new_padding {
            let sized: Vec<(WindowId, PhysicalSize<u32>)> = self
                .windows
                .iter()
                .filter_map(|(wid, ws)| ws.win_px.map(|size| (*wid, size)))
                .collect();
            for (wid, size) in sized {
                self.on_resize(wid, size);
            }
            for ws in self.windows.values() {
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        if backend_rebuild_needed {
            self.theme = new_theme;
            self.font_family = effective_family;
            let prepared = prepared_fonts
                .take()
                .map(|fonts| (fonts.renderer, fonts.chrome));
            let rebuild_succeeded = self
                .finish_reload_render_transaction_with(reload_render_before, move |app| {
                    app.rebuild_backend_with_prepared(prepared)
                });
            // Publish theme-dependent native chrome only after the fallible face
            // commit. Otherwise a rejected family could leave the titlebar on the
            // new theme while the terminal renderer stays on the old one.
            if rebuild_succeeded && theme_changed {
                let bg = self.theme.bg;
                let apprt = &self.apprt;
                let toolbars = &self._toolbars;
                let strip_dark = crate::tab_bar::theme_is_dark(bg);
                for (wid, ws) in self.windows.iter_mut() {
                    ws.last_strip_fp = None;
                    // Keep the seamless titlebar (window_set_background_color) in step
                    // with the new terminal bg, so a live theme change does not reopen a
                    // colour seam between the compact bar and the terminal body. No-op
                    // off macOS (the Linux apprt does nothing here).
                    if let Some(w) = ws.os_window.as_ref() {
                        apprt.window_set_background_color(w, bg);
                    }
                    // And the native toolbar strip's appearance with it (same rationale
                    // as apply_theme_live): labels must contrast with the NEW backdrop.
                    if let Some(handle) = toolbars.get(wid) {
                        crate::toolbar::set_strip_dark(handle, strip_dark);
                        crate::toolbar::set_active_tab_color(
                            handle,
                            self.config.active_tab_color_rgb(),
                        );
                    }
                }
            }
        } else if theme_changed {
            // Colour-only edit (font px + family unchanged): the face and glyph
            // atlas stay valid, so the full rebuild — primary-font re-discovery,
            // fontdue re-parse, atlas drop, per-window re-grid — would be pure
            // waste on the event loop. Push the theme onto the LIVE backend
            // instead; every colour-save while iterating on a scheme (and every
            // native Settings colour edit admitted through the serialized config
            // lane) is then a repaint, not a font parse. ENRICHED with our appearance knobs:
            // a single save can flip the theme AND a shaping/typography/render
            // knob, and none of those force a rebuild — apply them live here
            // too. `apply_theme_live` does all the per-window cache invalidation, so
            // there is no duplicate repaint. (`font_px` was committed above, before
            // the metrics refresh.)
            self.font_family = effective_family;
            if shaping_changed {
                self.backend.set_text_shaping(self.text_shaping.clone());
            }
            if typography_changed {
                self.backend.set_text_blending(self.text_blending);
                self.backend.set_font_thicken(self.font_thicken);
                self.backend.set_stem_gamma(self.stem_gamma);
                let hinting = self.font_hinting.clone();
                self.backend.set_font_hinting(&hinting);
                let subpixel = self.font_subpixel.clone();
                self.backend.set_font_subpixel(&subpixel);
            }
            // W5: each changed knob → exactly one renderer call (LineHeight is
            // unreachable here — it takes the rebuild branch above).
            for &change in &knob_changes {
                self.apply_render_knob(change);
            }
            self.apply_theme_live(new_theme);
        } else if shaping_changed || typography_changed || !knob_changes.is_empty() {
            // Shaping/typography-only edit (no font/theme/family change, so no backend
            // rebuild): push the new settings straight onto the live backend. These
            // flips change glyph APPEARANCE but not cell CONTENT, so every cache that
            // diffs on content would re-present the OLD glyphs. `set_text_shaping` /
            // `set_font_thicken` / `set_stem_gamma` already drop the SHARED atlas /
            // glyph caches where needed (`set_text_blending` is blend-time only); here
            // we additionally drop each window's PER-WINDOW present cache (CPU pixel
            // cache + GPU prev-frame) and the GUI RepaintKey gate — mirroring the
            // theme-change role of `WindowGpu::invalidate_present` /
            // `WindowCpu::invalidate`.
            self.backend.set_text_shaping(self.text_shaping.clone());
            self.backend.set_text_blending(self.text_blending);
            self.backend.set_font_thicken(self.font_thicken);
            self.backend.set_stem_gamma(self.stem_gamma);
            let hinting = self.font_hinting.clone();
            self.backend.set_font_hinting(&hinting);
            let subpixel = self.font_subpixel.clone();
            self.backend.set_font_subpixel(&subpixel);
            // W5: each changed knob → exactly one renderer call (LineHeight is
            // unreachable here — it takes the rebuild branch above).
            for &change in &knob_changes {
                self.apply_render_knob(change);
            }
            for ws in self.windows.values_mut() {
                ws.last_present = None;
                ws.cpu_cache.invalidate();
                if let Some(
                    PresentTarget::Gpu { window_gpu, .. } | PresentTarget::Virtual { window_gpu },
                ) = &mut ws.present
                {
                    window_gpu.invalidate_present();
                }
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        } else {
            // No backend rebuild, but the engine config may have changed cells, so
            // still request a repaint (the D-1 early-out skips it if nothing moved).
            if let Some(w) = self.front().and_then(|ws| ws.os_window.as_ref()) {
                w.request_redraw();
            }
        }
        // (The per-window metric refresh moved ABOVE the rebuild — the W12 ordering
        // note before the branch — so the re-grid divides by the NEW cell box.)
        // A live fast-path mutation does not pass through `rebuild_backend`'s
        // `sync_chrome_fonts`, so refresh the exact semantic font fork only
        // after every relevant backend setter has landed. This is rare
        // config-change work; steady semantic paint stays allocation-free.
        if !backend_rebuild_needed
            && !theme_changed
            && (shaping_changed || typography_changed || !knob_changes.is_empty())
        {
            self.sync_chrome_fonts();
        }
        // Surface `font_features` that can't take effect, now that the new shaping is
        // on the backend — a no-op becomes a visible hint instead of silent confusion.
        self.warn_font_feature_issues();
        self.finish_native_config_external_admission(&admitted_baseline);
    }

    /// Re-resolve EVERY window's per-window [`MetricsView`] from the live font
    /// regime (W12). An explicit `$ATERM_FONT_PX` / `config.font_px` (or a
    /// force-pinned scale) fixes the px for all windows — each keeps its own
    /// scale-derived `pad`; otherwise each window's metrics are the pure
    /// [`MetricsView::for_scale`] of its OWN display scale. Used after a config
    /// reload changes the font size regime, so the per-window pixel authority the
    /// draw / hit-test / IME paths read never diverges from the rebuilt backend.
    /// NEVER-ATTACHED windows keep their sealed pad/head (only the font regime
    /// re-resolves) — see the seam note inside.
    /// The interior padding (device px per edge) for `scale` under the LIVE
    /// config: [`crate::logical_to_device_px`] over
    /// [`Config::window_padding_or_default`]. The config-aware twin of the free
    /// [`crate::pad_for_scale`] (which stays pinned to the built-in default for
    /// the pure MetricsView proofs); with an unset config the two are equal.
    pub(crate) fn cfg_pad_for_scale(&self, scale: f64) -> usize {
        crate::logical_to_device_px(self.config.window_padding_or_default(), scale)
    }

    /// TOP-edge twin of [`Self::cfg_pad_for_scale`] —
    /// `Config::window_padding_top_or_default` at `scale`, additionally capped at
    /// the base pad in DEVICE px (rounding could otherwise lift a logical-equal
    /// pair above the base at fractional scales; the renderer clamps the same way).
    pub(crate) fn cfg_pad_top_for_scale(&self, scale: f64) -> usize {
        crate::logical_to_device_px(self.config.window_padding_top_or_default(), scale)
            .min(self.cfg_pad_for_scale(scale))
    }

    /// C3 — THE SYNTHETIC TAB-BAND HEAD, in DEVICE px, for a window at `scale`
    /// whose cell box is `cell_h` px tall. `0` on macOS, `0` with the strip off,
    /// `0` under `tab_band_height = "compact"`, and `0` whenever the strip's cell
    /// rows already reach the target — every one of which reproduces the pre-C3
    /// geometry byte for byte.
    ///
    /// WHY A `head` BAND AND NOT A BIGGER `pad_top`. The Windows band measured 21 px
    /// (2 px lip + one 19 px cell row at FONT_PX 16 / 96 dpi) where a WinUI tab is
    /// 32-34. Two mechanisms could close that:
    ///
    ///   * raise the Windows `window_padding_top` default — but `pad_top` is
    ///     interior padding of the GRID. It applies with the strip OFF too, so a
    ///     `tab_strip_rows = 0` window would silently gain a dead 11 px gutter and
    ///     every geometry proof (`pad_split`, the asymmetric-pad lattice, hundreds
    ///     of `pad_top` call sites) would have to move with it. That is a blast
    ///     radius out of all proportion to a chrome tweak.
    ///
    ///   * a synthetic `head` — the band aterm ALREADY has for "host chrome that
    ///     overlaps the grid", which is exactly what this is. `head` is reserved
    ///     out of the height before the rows are fitted (`grid_dims_for`), added
    ///     back by `frame_size`, stripped by `pixel_to_term_cell` /
    ///     `strip_col_for_pixel` (a click in the head band over the strip already
    ///     maps to the strip — `gy` saturates to 0), filled edge-to-edge in the
    ///     band tone by `fill_chrome_bleed`'s `[0, grid_top)` rule, and folded into
    ///     the pixel band's optical centring through `band_top_px = pad_top + head`
    ///     (and into its cache key, so a change re-rasters exactly once). Nothing
    ///     new has to learn about it.
    ///
    /// It is gated on `tab_strip_rows > 0` for the same reason the first bullet is
    /// rejected: a band with nothing in it is not chrome, it is a shifted grid.
    ///
    /// Takes `cell_h` rather than reading it, because the two callers that matter
    /// hold a cell box the per-window `MetricsView` does not yet (attach, before
    /// the record is written) or cannot (the L1 early reveal, whose whole premise
    /// is CACHED metrics from a previous run).
    ///
    /// COMPILED ON EVERY PLATFORM, not `#[cfg(windows)]`. It is the one non-test
    /// consumer of the C3 config chain (`Config::tab_band_height_or_default`,
    /// [`TabBandHeight`], [`synthetic_band_head_px`], [`TAB_BAND_STANDARD_LOGICAL_PX`],
    /// `SYNTHETIC_BAND_HEAD_CELL_CAP`), so gating it out would make all of them
    /// unreachable off Windows and Linux and break the `-D warnings` gate on macOS
    /// with a fan of `dead_code` findings. On macOS it is not a stub either: the
    /// law genuinely evaluates to 0 there, because [`TabBandHeight::PLATFORM_DEFAULT`]
    /// is `Compact` and a `compact` target collapses the whole derivation. The `0`
    /// promised in the first paragraph is therefore a RESULT, computed by the same
    /// arithmetic Windows and Linux run, not a platform special case — which is
    /// also what keeps the chain type-checked and test-covered on the platform
    /// that never calls it (see `tab_band_height_tests`).
    pub(crate) fn synthetic_strip_head_px(&self, scale: f64, cell_h: usize) -> usize {
        synthetic_band_head_px(
            self.config.tab_band_height_or_default().target_logical_px(),
            self.cfg_pad_top_for_scale(scale),
            self.tab_strip_rows,
            cell_h,
            scale,
        )
    }

    // NOTE: no per-platform TWIN, deliberately — one function, one law, every
    // platform. On macOS the synthetic band does not exist (its `head` is a
    // MEASURED AppKit titlebar — see `AppRt::titlebar_band_pts`), but that absence
    // is expressed by `PLATFORM_DEFAULT = Compact` returning 0 through the shared
    // arithmetic, not by a second `#[cfg]` body. The one platform-shaped guard that
    // DOES have to stay a guard is the CALL in `on_resize`, and it is a runtime
    // `cfg!(any(windows, target_os = "linux"))` for the same reason: on macOS
    // `ws.metrics.head` holds a real measured titlebar band, so letting the C3
    // write run there would clobber it with this law's 0.

    /// THE HONESTY DRAIN. Move anything queued on the deferred notice lane
    /// ([`crate::config_notice::queue_deferred`]) into the in-window banner — the
    /// one surface a Start-Menu / Explorer launch actually has, since a
    /// GUI-subsystem process's stderr is a closed handle there.
    ///
    /// Called from `about_to_wait`, i.e. on EVERY event-loop park, because the
    /// queuing sites are spread across three contexts that cannot reach `App`: the
    /// backend BUILD THREAD (GPU init failed → the backdrop is withdrawn), an
    /// `AppRt` chrome call handed only a `&Window` (the material is styling the
    /// caption only), and `run()` itself before `App` is constructed (hdr_glow and
    /// the material are mutually exclusive). The check is one relaxed atomic load
    /// when the lane is empty, which it is on every park but a handful per run.
    ///
    /// MERGES rather than replaces: the startup config banner may still be up, and
    /// overwriting it would trade the user's typo'd keybinding warnings for one
    /// chrome sentence.
    pub(crate) fn drain_deferred_config_notices(&mut self) {
        let lines = crate::config_notice::take_deferred();
        if lines.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        match self.config_notice.as_mut().filter(|n| !n.is_expired(now)) {
            Some(live) => live.extend(lines, now),
            None => self.config_notice = crate::config_notice::ConfigNotice::new(lines, now),
        }
        self.request_redraw_all_windows();
    }

    pub(crate) fn refresh_all_window_metrics(&mut self) {
        let pinned = self.font_px_explicit || resolve_force_scale().is_some();
        let font_px = self.font_px;
        // Resolve the configured LOGICAL padding once, outside the borrow of
        // `windows` — the per-window derivation below stays pure of everything
        // but the window's own scale (the W12 law), with the config supplying
        // the logical constants.
        let (pad_l, pad_top_l) = (
            self.config.window_padding_or_default(),
            self.config.window_padding_top_or_default(),
        );
        for ws in self.windows.values_mut() {
            // The chrome headroom is a property of the window's titlebar band, not
            // of the font regime: re-derive it from the attach-captured points at
            // the window's own scale on BOTH branches (`for_scale` carries none).
            // A NEVER-ATTACHED window has no captured band: its sealed record
            // ([`Self::seed_headless_boot_metrics`]) is the applied boot truth,
            // and re-deriving from `head_pts` (0) wiped the `$ATERM_HEADROOM_PX`
            // band on every delta config reload — the next recording's first
            // redraw re-tuned the shared backend from the wiped record, re-opening
            // the frames=0 class through this seam. Keep the sealed head + pad.
            let attached = ws.os_window.is_some();
            let head = if attached {
                (ws.head_pts * ws.scale).round() as usize
            } else {
                ws.metrics.head
            };
            ws.metrics = if pinned {
                // A pinned font fixes the PX, not the padding: an ATTACHED
                // window's pad re-resolves from the live config at its own scale
                // (identical to the old kept value when `window_padding` is
                // unset, since attach derived it from the same constants) so a
                // padding edit hot-applies under `$ATERM_FONT_PX` too. A
                // never-attached window keeps its sealed boot pad, as below.
                let (pad, pad_top) = if attached {
                    let pad = crate::logical_to_device_px(pad_l, ws.scale);
                    (
                        pad,
                        crate::logical_to_device_px(pad_top_l, ws.scale).min(pad),
                    )
                } else {
                    (ws.metrics.pad, ws.metrics.pad_top)
                };
                crate::MetricsView::applied(font_px, pad, pad_top, head)
            } else {
                let mut m = crate::MetricsView::for_scale_padded(ws.scale, pad_l, pad_top_l);
                if !attached {
                    m.pad = ws.metrics.pad; // the sealed boot pad, ditto
                    m.pad_top = ws.metrics.pad_top; // preserve the sealed symmetric origin too
                }
                m
            };
            ws.metrics.head = head;
        }
        // CELL-PX-1: every window's resolved cell box may have just moved (a font
        // zoom, a `font_size` / `font_family` reload, a theme whose metrics differ),
        // so report the new one to the engines HERE rather than waiting for each
        // window's next raster seam. `apply_window_scale` alone would leave a
        // BACKGROUND window's sessions answering DEC 1016 and sizing OSC 1337
        // images from the pre-change cell box for as long as that window went
        // unpainted — a shell does not stop emitting because its window is behind
        // another. This is a metrics-change seam, not a per-frame one, so the
        // engine locks it takes are rare; the per-window memo makes the windows
        // whose resolved size did not actually move free anyway.
        //
        // A FACE swap resolves here against the face still installed on the shared
        // backend (`set_font_px_with` refreshes metrics BEFORE `rebuild_backend`,
        // deliberately — the re-grid needs the new px). That is exactly right for a
        // pure zoom, and merely early for a face change: the memo keys on the
        // RESOLVED size, so the first post-rebuild `apply_window_scale` still
        // pushes when the new face resolves differently. It can never strand a
        // stale value.
        let wids: Vec<crate::WindowId> = self.windows.keys().copied().collect();
        for wid in wids {
            self.sync_cell_pixel_size(wid);
        }
    }

    /// Warn (once per config load) about configured `font_features` that cannot take
    /// effect, so the user isn't left with a silent no-op ("it looked the same"):
    /// (1) tokens the parser rejected — typos like `+toolong` / `cv01=x` (mirrors the
    /// per-key warnings the other config keys already emit); and (2) valid tags the
    /// ACTIVE FONT doesn't advertise in its `GSUB` (any feature on a no-GSUB font such
    /// as Menlo/Monaco, or an unsupported tag like `ss99`). Must be called AFTER the
    /// shaping is applied to the backend so the font probe reflects the live face.
    pub(crate) fn warn_font_feature_issues(&self) {
        let Some(specs) = self.config.font_features.as_deref() else {
            return;
        };
        // (1) Unparseable tokens — same style as the cursor_style / ambiguous_width /
        //     colour / theme warnings.
        let rejected: Vec<&str> = specs
            .iter()
            .flat_map(|s| s.split_whitespace())
            .filter(|t| aterm_types::text_shaping::FontFeature::parse_token(t).is_none())
            .collect();
        if !rejected.is_empty() {
            eprintln!(
                "aterm-gui: config font_features: ignored unparseable token(s) {rejected:?} \
                 (use a 1–4 char tag; optional +/- prefix or tag=value)"
            );
        }
        // (2) Valid tags the active font cannot apply (no GSUB / unsupported tag), so
        //     they silently do nothing.
        let unsupported = self.backend.unsupported_user_feature_tags();
        if !unsupported.is_empty() {
            let tags: Vec<String> = unsupported
                .iter()
                .map(|t| String::from_utf8_lossy(t).trim_end().to_string())
                .collect();
            eprintln!(
                "aterm-gui: config font_features: the active font does not provide {tags:?}; \
                 those have no effect (choose a font that carries these OpenType features)"
            );
        }
    }
}

#[cfg(test)]
mod reload_render_transaction_tests {
    use super::{Config, FontConfig, ReloadRenderSnapshot, RenderKnobs};

    #[test]
    fn failed_backend_rebuild_rolls_back_complete_render_slice_only() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let old_config = app.config.clone();
        let old_default_font_px = app.default_font_px;
        let old_font_px = app.font_px;
        let old_font_px_explicit = app.font_px_explicit;
        let old_theme = app.theme;
        let old_family = app.font_family.clone();
        let old_shaping = app.text_shaping.clone();
        let old_blending = app.text_blending;
        let old_thicken = app.font_thicken;
        let old_stem_gamma = app.stem_gamma;
        let old_variations = app.font_variations.clone();
        let old_dark_nudge = app.font_weight_dark_nudge;
        let old_knobs = app.render_knobs;
        let old_font_config = app.font_config.clone();
        let old_metrics = app.windows[&wid].metrics;
        let previous = ReloadRenderSnapshot::capture(&app);

        // This is the candidate state reload_config publishes before its one
        // fallible step. It deliberately changes every cached renderer source,
        // both geometry keys, theme keys, and one UNRELATED live input policy.
        let candidate: Config = toml::from_str(
            r##"
font_px = 23.0
theme = "Dracula"
foreground = "#f0f1f2"
background = "#101112"
cursor_color = "#aabbcc"
selection_color = "#334455"
palette = ["#010203", "#040506"]
font_family = "Menlo"
font_family_bold = "Menlo Bold"
font_family_italic = "Menlo Italic"
font_family_bold_italic = "Menlo Bold Italic"
font_synthetic_style = false
fallback_fonts = ["Menlo"]
symbol_font = "Menlo"
emoji_font = "Menlo"
ligatures = false
merged_ligatures = true
font_features = ["ss01"]
text_blending = "linear"
font_thicken = true
font_variation = ["wght=550"]
font_weight = 600
font_weight_dark_nudge = 40.0
stem_gamma = 0.7
line_height = 1.4
adjust_baseline = 3
adjust_underline_position = 2
adjust_underline_thickness = 1
underline_skip_descenders = false
minimum_contrast = 7.0
selection_foreground = "#eeeeee"
selection_inactive = true
cursor_break_ligatures = true
background_opacity = 0.8
background_material = "hud"
window_padding = 31.0
window_padding_top = 7.0
copy_on_select = true
"##,
        )
        .expect("candidate config");
        app.config = candidate.clone();
        app.default_font_px = 23.0;
        app.font_px = 23.0;
        app.font_px_explicit = true;
        app.theme = candidate.theme_for(app.os_appearance);
        app.font_family = Some("Menlo".to_string());
        app.text_shaping = candidate.text_shaping();
        app.text_blending = candidate.text_blending_or_default();
        app.font_thicken = candidate.font_thicken_or_default();
        app.stem_gamma = candidate.stem_gamma_or_default();
        let (variations, _) = candidate.font_variation_requests();
        app.font_variations = variations;
        app.font_weight_dark_nudge = candidate.font_weight_dark_nudge_or_default();
        app.render_knobs = RenderKnobs::from_config(&candidate);
        app.font_config = FontConfig::from_config(&candidate).0;
        app.refresh_all_window_metrics();
        assert_ne!(app.windows[&wid].metrics, old_metrics, "candidate is live");

        assert!(
            !app.finish_reload_render_transaction_with(previous, |candidate_app| {
                assert_eq!(candidate_app.font_px, 23.0, "failure sees candidate");
                assert_eq!(candidate_app.render_knobs.line_height, 1.4);
                false
            })
        );

        macro_rules! assert_config_fields_rolled_back {
            ($($field:ident),+ $(,)?) => {
                $(assert!(
                    app.config.$field == old_config.$field,
                    concat!(stringify!($field), " config authority did not roll back")
                );)+
            };
        }
        assert_config_fields_rolled_back!(
            font_px,
            foreground,
            background,
            cursor_color,
            selection_color,
            palette,
            theme,
            font_family,
            font_family_bold,
            font_family_italic,
            font_family_bold_italic,
            font_synthetic_style,
            fallback_fonts,
            symbol_font,
            emoji_font,
            ligatures,
            merged_ligatures,
            font_features,
            text_blending,
            font_thicken,
            font_variation,
            font_weight,
            font_weight_dark_nudge,
            stem_gamma,
            line_height,
            adjust_baseline,
            adjust_underline_position,
            adjust_underline_thickness,
            underline_skip_descenders,
            minimum_contrast,
            selection_foreground,
            selection_inactive,
            cursor_break_ligatures,
            background_opacity,
            background_material,
            window_padding,
            window_padding_top,
        );
        assert_eq!(
            app.config.copy_on_select,
            Some(true),
            "an unrelated live edit from the same reload survives"
        );
        assert_eq!(app.default_font_px, old_default_font_px);
        assert_eq!(app.font_px, old_font_px);
        assert_eq!(app.font_px_explicit, old_font_px_explicit);
        assert_eq!(
            (
                app.theme.fg,
                app.theme.bg,
                app.theme.cursor,
                app.theme.selection
            ),
            (
                old_theme.fg,
                old_theme.bg,
                old_theme.cursor,
                old_theme.selection
            )
        );
        assert_eq!(app.font_family, old_family);
        assert_eq!(app.text_shaping, old_shaping);
        assert_eq!(app.text_blending, old_blending);
        assert_eq!(app.font_thicken, old_thicken);
        assert_eq!(app.stem_gamma, old_stem_gamma);
        assert_eq!(app.font_variations, old_variations);
        assert_eq!(app.font_weight_dark_nudge, old_dark_nudge);
        assert_eq!(app.render_knobs, old_knobs);
        assert_eq!(app.font_config, old_font_config);
        assert_eq!(app.windows[&wid].metrics, old_metrics);
    }
}

#[cfg(test)]
mod descriptive_title_config_tests {

    /// The TYPING-WAKE dial's config contract (Settings ▸ Cursor Kitty ▸ Rainbow
    /// wake ▸ "Typing wake"): an absent key takes the engine's own default, `0` is a
    /// real OFF setting rather than a failure, an absurd value clamps instead of
    /// escaping, and the whole `u64` domain maps into the closed range — there
    /// is nothing a config file can write here that reaches the engine unbounded.
    #[test]
    fn cursor_trail_wake_dial_defaults_clamps_and_turns_off() {
        use crate::app_config::Config;
        let cfg = Config::default();
        assert!(
            (cfg.cursor_trail_wake_persist_or_default()
                - aterm_effects::cursor_glow::RAINBOW_WAKE_PERSIST)
                .abs()
                < 1e-6,
            "an unset key takes the engine default"
        );
        let with = |ms: u64| -> f32 {
            Config {
                cursor_trail_wake_ms: Some(ms),
                ..Config::default()
            }
            .cursor_trail_wake_persist_or_default()
        };
        assert_eq!(with(0), 0.0, "0 ms is a real OFF setting");
        assert!((with(600) - 0.6).abs() < 1e-6, "ms → seconds");
        assert!((with(1_500) - 1.5).abs() < 1e-6, "the ceiling is reachable");
        assert!(
            (with(u64::MAX) - 1.5).abs() < 1e-6,
            "an absurd value clamps, never escapes"
        );
        // Round-trips through the real TOML surface the settings writer emits.
        let parsed: Config = toml::from_str("cursor_trail_wake_ms = 900\n").unwrap();
        assert_eq!(parsed.cursor_trail_wake_ms, Some(900));
        assert!((parsed.cursor_trail_wake_persist_or_default() - 0.9).abs() < 1e-6);
    }

    /// The effects-OFF owner must not pay the effect-pipeline warm-up on a config
    /// save. The nine pipelines are demand-driven so a launch compiles none of
    /// them; warming them unconditionally at the config-apply seam would hand a
    /// `cursor_trail = false` config the same 136 ms of never-drawn dx12 compiles,
    /// merely relocated off the launch onto the first save. With the trail off the
    /// only pipelines still reachable are sub-frame builds the demand path absorbs.
    #[test]
    fn an_effects_off_config_never_warms_the_effect_pipelines() {
        let off: Config = toml::from_str("cursor_trail = false\n").unwrap();
        assert!(
            !off.warms_effect_pipelines(),
            "`cursor_trail = false` must not compile fire/rain pipelines it can never bind"
        );
        // The batteries-on default and an explicit opt-in DO warm: with the trail
        // live, `fire_add` + `fire_over` alone are 111.07 ms, and paying that
        // inline on the first frame that ignites is a hitch the eye sees.
        let on: Config = toml::from_str("cursor_trail = true\n").unwrap();
        assert!(on.warms_effect_pipelines());
        // …and the ABSENT key follows the platform default, so the warm is exactly
        // as demand-driven as the trail itself: paid where the trail ships on,
        // never paid on Windows, where a fresh config can bind none of it.
        assert_eq!(
            Config::default().warms_effect_pipelines(),
            super::DEFAULT_DECORATIVE_EFFECTS,
            "the warm rides the trail's own platform default, never its own opinion"
        );
    }
    use super::{
        Config, DEFAULT_TITLE_SUMMARY_MODEL, TitleFormat, TitleSummaryProvider,
        TitleSummaryProxyMode,
    };

    fn cfg(source: &str) -> Config {
        toml::from_str(source).expect("valid descriptive-title config")
    }

    #[test]
    fn descriptive_title_defaults_are_safe_and_useful() {
        let config = Config::default();
        assert!(config.descriptive_titles_or_default());
        assert_eq!(
            config.title_summary_provider_or_default(),
            TitleSummaryProvider::Builtin
        );
        assert_eq!(
            config.title_summary_model_or_default(),
            DEFAULT_TITLE_SUMMARY_MODEL
        );
        assert_eq!(
            config.title_summary_endpoint_or_default(),
            None,
            "the in-process provider has no endpoint"
        );
        assert_eq!(config.title_summary_token_file(), None);
        assert_eq!(config.title_summary_timeout_seconds_or_default(), 20);
        assert_eq!(
            config.title_summary_proxy_mode_or_default(),
            TitleSummaryProxyMode::Environment
        );
        assert_eq!(config.title_summary_ca_file(), None);
        assert_eq!(config.title_summary_interval_seconds_or_default(), 15);
        assert_eq!(config.title_summary_context_lines_or_default(), 24);
        assert!(config.title_summary_include_output_or_default());
        assert!(!config.title_summary_allow_remote_or_default());
        assert_eq!(
            config.tab_title_format_or_default(),
            TitleFormat::TitleDescription
        );
        assert_eq!(
            config.window_title_format_or_default(),
            TitleFormat::TitleDescription
        );
    }

    #[test]
    fn every_descriptive_title_key_parses_and_resolves() {
        let config = cfg(r#"
descriptive_titles = false
title_summary_provider = "openai-compatible"
title_summary_model = "acme/terminal-summarizer"
title_summary_endpoint = "https://llm.example.test/v1/chat/completions"
title_summary_token_file = "/tmp/aterm-summary-token"
title_summary_timeout_seconds = 42
title_summary_proxy_mode = "direct"
title_summary_ca_file = "/tmp/private-model-ca.pem"
title_summary_interval_seconds = 42
title_summary_context_lines = 36
title_summary_include_output = false
title_summary_allow_remote = true
tab_title_format = "description-title"
window_title_format = "description"
"#);
        assert!(!config.descriptive_titles_or_default());
        assert_eq!(
            config.title_summary_provider_or_default(),
            TitleSummaryProvider::OpenAiCompatible
        );
        assert_eq!(
            config.title_summary_model_or_default(),
            "acme/terminal-summarizer"
        );
        assert_eq!(
            config.title_summary_endpoint_or_default(),
            Some("https://llm.example.test/v1/chat/completions")
        );
        assert_eq!(
            config.title_summary_token_file(),
            Some("/tmp/aterm-summary-token")
        );
        assert_eq!(config.title_summary_timeout_seconds_or_default(), 42);
        assert_eq!(
            config.title_summary_proxy_mode_or_default(),
            TitleSummaryProxyMode::Direct
        );
        assert_eq!(
            config.title_summary_ca_file(),
            Some("/tmp/private-model-ca.pem")
        );
        assert_eq!(config.title_summary_interval_seconds_or_default(), 42);
        assert_eq!(config.title_summary_context_lines_or_default(), 36);
        assert!(!config.title_summary_include_output_or_default());
        assert!(config.title_summary_allow_remote_or_default());
        assert_eq!(
            config.tab_title_format_or_default(),
            TitleFormat::DescriptionTitle
        );
        assert_eq!(
            config.window_title_format_or_default(),
            TitleFormat::Description
        );
    }

    #[test]
    fn providers_have_closed_tokens_and_provider_aware_endpoints() {
        for (token, expected) in [
            ("builtin", TitleSummaryProvider::Builtin),
            ("ollama", TitleSummaryProvider::Ollama),
            ("openai-compatible", TitleSummaryProvider::OpenAiCompatible),
            ("off", TitleSummaryProvider::Off),
        ] {
            let config = cfg(&format!("title_summary_provider = \"{token}\""));
            assert_eq!(config.title_summary_provider_or_default(), expected);
            assert_eq!(expected.as_str(), token);
        }
        assert!(toml::from_str::<Config>("title_summary_provider = \"remote\"").is_err());

        assert_eq!(
            cfg("title_summary_provider = \"ollama\"").title_summary_endpoint_or_default(),
            None,
            "absent Ollama endpoint is selected privately at runtime"
        );
        assert_eq!(
            cfg("title_summary_provider = \"ollama\"\n\
                 title_summary_endpoint = \"  http://localhost:9999/chat  \"")
            .title_summary_endpoint_or_default(),
            Some("http://localhost:9999/chat")
        );
        assert_eq!(
            cfg("title_summary_provider = \"openai-compatible\"")
                .title_summary_endpoint_or_default(),
            None,
            "OpenAI-compatible endpoints are explicit"
        );
        assert_eq!(
            cfg("title_summary_provider = \"builtin\"\n\
                 title_summary_endpoint = \"https://stale.example.test/chat\"")
            .title_summary_endpoint_or_default(),
            None,
            "non-network providers ignore stale endpoint settings"
        );
    }

    #[test]
    fn tab_status_policy_defaults_and_clamps_both_ends() {
        let default = Config::default();
        assert!(default.tab_status_or_default(), "batteries on");
        assert!(default.tab_status_badge_or_default());
        assert_eq!(default.tab_status_quiet_after_ms_or_default(), 5_000);
        assert_eq!(default.tab_status_dwell_ms_or_default(), 750);
        assert_eq!(
            default.tab_status_policy().quiet_after,
            std::time::Duration::from_millis(5_000)
        );
        assert_eq!(
            default.tab_status_policy().dwell,
            std::time::Duration::from_millis(750)
        );

        // Both ends of both numeric keys: a typo can neither call every job
        // stalled on arrival nor make hysteresis long enough to be useless.
        assert_eq!(
            cfg("tab_status_quiet_after_ms = 0").tab_status_quiet_after_ms_or_default(),
            500
        );
        assert_eq!(
            cfg("tab_status_quiet_after_ms = 999999").tab_status_quiet_after_ms_or_default(),
            120_000
        );
        assert_eq!(
            cfg("tab_status_dwell_ms = 0").tab_status_dwell_ms_or_default(),
            0
        );
        assert_eq!(
            cfg("tab_status_dwell_ms = 999999").tab_status_dwell_ms_or_default(),
            10_000
        );
        assert!(!cfg("tab_status = false").tab_status_or_default());
        assert!(!cfg("tab_status_badge = false").tab_status_badge_or_default());
        // The connection mark is opt-OUT like its status siblings.
        assert!(default.tab_connection_badge_or_default());
        assert!(!cfg("tab_connection_badge = false").tab_connection_badge_or_default());
    }

    /// The observation budget is DERIVED, not configured: it must never be
    /// coarser than the dwell it has to serve (a candidate that cannot be
    /// re-seen inside its own window can never be published), and never fall to
    /// zero, which would turn every output burst into a classification.
    #[test]
    fn the_observation_interval_tracks_dwell_between_a_floor_and_the_budget() {
        let ms = |toml: &str| cfg(toml).tab_status_observe_interval().as_millis();
        assert_eq!(
            Config::default().tab_status_observe_interval().as_millis(),
            250
        );
        assert_eq!(ms("tab_status_dwell_ms = 0"), 50, "floored, never zero");
        assert_eq!(ms("tab_status_dwell_ms = 100"), 100, "tracks a short dwell");
        assert_eq!(
            ms("tab_status_dwell_ms = 9000"),
            250,
            "capped at the budget"
        );
        for dwell in [0_u64, 1, 100, 750, 5_000, 10_000, 999_999] {
            let config = cfg(&format!("tab_status_dwell_ms = {dwell}"));
            let interval = config.tab_status_observe_interval().as_millis();
            let resolved = u128::from(config.tab_status_dwell_ms_or_default());
            assert!(
                interval <= resolved.max(50),
                "interval {interval} outruns dwell {resolved}"
            );
        }
    }

    #[test]
    fn title_formats_have_closed_canonical_tokens() {
        for (token, expected) in [
            ("title", TitleFormat::Title),
            ("description", TitleFormat::Description),
            ("title-description", TitleFormat::TitleDescription),
            ("description-title", TitleFormat::DescriptionTitle),
        ] {
            let config = cfg(&format!("tab_title_format = \"{token}\""));
            assert_eq!(config.tab_title_format_or_default(), expected);
            assert_eq!(expected.as_str(), token);
        }
        assert!(toml::from_str::<Config>("tab_title_format = \"title_only\"").is_err());
        assert!(toml::from_str::<Config>("window_title_format = \"both\"").is_err());
    }

    #[test]
    fn title_summary_proxy_modes_have_closed_canonical_tokens() {
        for (token, expected) in [
            ("environment", TitleSummaryProxyMode::Environment),
            ("direct", TitleSummaryProxyMode::Direct),
        ] {
            let config = cfg(&format!("title_summary_proxy_mode = \"{token}\""));
            assert_eq!(config.title_summary_proxy_mode_or_default(), expected);
            assert_eq!(expected.as_str(), token);
        }
        assert!(toml::from_str::<Config>("title_summary_proxy_mode = \"automatic\"").is_err());
    }

    #[test]
    fn numeric_bounds_and_blank_text_values_are_normalized() {
        assert_eq!(
            cfg("title_summary_interval_seconds = 0").title_summary_interval_seconds_or_default(),
            5
        );
        assert_eq!(
            cfg("title_summary_interval_seconds = 999").title_summary_interval_seconds_or_default(),
            300
        );
        assert_eq!(
            cfg("title_summary_context_lines = 0").title_summary_context_lines_or_default(),
            4
        );
        assert_eq!(
            cfg("title_summary_context_lines = 999").title_summary_context_lines_or_default(),
            80
        );
        assert_eq!(
            cfg("title_summary_timeout_seconds = 0").title_summary_timeout_seconds_or_default(),
            1
        );
        assert_eq!(
            cfg("title_summary_timeout_seconds = 999").title_summary_timeout_seconds_or_default(),
            120
        );

        let blank = cfg("title_summary_provider = \"ollama\"\n\
             title_summary_model = \"   \"\n\
             title_summary_endpoint = \"   \"\n\
             title_summary_token_file = \"   \"\n\
             title_summary_ca_file = \"   \"");
        assert_eq!(
            blank.title_summary_model_or_default(),
            DEFAULT_TITLE_SUMMARY_MODEL
        );
        assert_eq!(
            blank.title_summary_endpoint_or_default(),
            None,
            "blank Ollama endpoint remains automatic"
        );
        assert_eq!(blank.title_summary_token_file(), None);
        assert_eq!(blank.title_summary_ca_file(), None);
    }
}

#[cfg(test)]
mod cfg_engine_tests {
    use super::{
        Config, KittySpriteAsset, MAX_KITTY_SPRITE_FILE_BYTES, MAX_USER_THEME_FILE_BYTES,
        MAX_USER_THEME_FILES, ThemeCatalog, ThemeCatalogWatchError, open_regular_theme_file,
    };
    use aterm_core::config::BiDiMode;

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn shell_override_resolver_keeps_launch_precedence_for_new_sessions() {
        assert_eq!(
            super::resolve_shell_override_with(None, Some("/bin/zsh")),
            Some("/bin/zsh".to_string())
        );
        assert_eq!(
            super::resolve_shell_override_with(
                Some("/opt/homebrew/bin/fish".to_string()),
                Some("/bin/zsh")
            ),
            Some("/opt/homebrew/bin/fish".to_string())
        );
        assert_eq!(
            super::resolve_shell_override_with(Some(String::new()), Some("/bin/zsh")),
            Some("/bin/zsh".to_string()),
            "an empty environment value does not mask the config"
        );
    }

    fn kitty_fixture(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "aterm-nyan-asset-{}-{}-{name}.png",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn config_with_kitty(path: &std::path::Path) -> Config {
        cfg(&format!(
            "theme = \"Nord\"\ncursor_nyan_sprite = {:?}\n",
            path.to_string_lossy()
        ))
    }

    #[test]
    fn serious_mode_defaults_off_and_parses_both_states() {
        assert!(!Config::default().serious_mode_or_default());
        assert!(cfg("serious_mode = true").serious_mode_or_default());
        assert!(!cfg("serious_mode = false").serious_mode_or_default());
    }

    fn theme_fixture_dir(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let dir = std::env::temp_dir().join(format!(
            "aterm-theme-catalog-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_theme(path: &std::path::Path, foreground: &str, background: &str) {
        std::fs::write(
            path,
            format!("foreground = {foreground}\nbackground = {background}\n"),
        )
        .unwrap();
    }

    #[test]
    fn theme_catalog_is_sorted_bounded_safe_and_parse_validated() {
        let dir = theme_fixture_dir("bounded");
        for index in (0..(MAX_USER_THEME_FILES + 2)).rev() {
            write_theme(
                &dir.join(format!("Theme-{index:03}.conf")),
                "#ddeeff",
                "#102030",
            );
        }
        let catalog = ThemeCatalog::discover_in(&dir);
        let names = catalog.ready_names().collect::<Vec<_>>();
        assert_eq!(names.len(), MAX_USER_THEME_FILES);
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(names[0], "Theme-000");
        assert_eq!(names[MAX_USER_THEME_FILES - 1], "Theme-127");
        assert!(catalog.truncated);
        assert_ne!(catalog.fingerprint(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_catalog_hides_unsafe_malformed_oversized_and_symlink_entries() {
        let dir = theme_fixture_dir("invalid");
        write_theme(&dir.join("Work.conf"), "#123456", "#abcdef");
        write_theme(&dir.join("dark:Trap.conf"), "#ffffff", "#000000");
        write_theme(&dir.join(" Bad.conf"), "#ffffff", "#000000");
        std::fs::write(dir.join("Broken.conf"), "foreground = #ffffff\n").unwrap();
        let oversized = std::fs::File::create(dir.join("Huge.conf")).unwrap();
        oversized
            .set_len((MAX_USER_THEME_FILE_BYTES + 1) as u64)
            .unwrap();
        drop(oversized);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("Work.conf"), dir.join("Alias.conf")).unwrap();
            let fifo = dir.join("Pipe.conf");
            let fifo_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
            // SAFETY: `fifo_path` is a valid NUL-terminated path and the return
            // value is checked before the no-follow/nonblocking open is tested.
            assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
        }

        let catalog = ThemeCatalog::discover_in(&dir);
        assert_eq!(catalog.ready_names().collect::<Vec<_>>(), ["Work"]);
        for invalid in ["dark:Trap", " Bad", "Broken", "Huge"] {
            assert!(
                catalog.resolve(invalid).is_err(),
                "{invalid} must fail closed"
            );
        }
        #[cfg(unix)]
        {
            assert!(catalog.resolve("Alias").is_err());
            assert!(
                open_regular_theme_file(&dir.join("Alias.conf")).is_err(),
                "the handle-level open must reject a symlink even if it is swapped in after scanning"
            );
            assert!(
                open_regular_theme_file(&dir.join("Pipe.conf")).is_err(),
                "the nonblocking handle-level open must reject a FIFO without waiting for a writer"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn live_theme_file_swap_retains_valid_catalog_until_recovery() {
        use std::os::unix::fs::symlink;

        let dir = theme_fixture_dir("live-file-swap");
        let work = dir.join("Work.conf");
        write_theme(&work, "#123456", "#abcdef");
        let active = ThemeCatalog::try_discover_in(&dir).expect("initial valid catalog");
        assert!(active.resolve("Work").is_ok());

        let rejected = ThemeCatalog::try_discover_in_after_scan(&dir, || {
            std::fs::remove_file(&work).unwrap();
            symlink(dir.join("missing.conf"), &work).unwrap();
        });
        assert_eq!(rejected, Err(ThemeCatalogWatchError::FileUnavailable));
        assert!(
            active.resolve("Work").is_ok(),
            "a rejected live generation leaves the prior active theme intact"
        );

        std::fs::remove_file(&work).unwrap();
        write_theme(&work, "#fedcba", "#101820");
        let recovered = ThemeCatalog::try_discover_in(&dir).expect("recovered catalog");
        assert!(recovered.resolve("Work").is_ok());
        assert_ne!(recovered, active);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_catalog_has_one_case_insensitive_identity_and_rejects_collisions() {
        let dir = theme_fixture_dir("case-identity");
        write_theme(&dir.join("Work.conf"), "#123456", "#abcdef");
        let catalog = ThemeCatalog::discover_in(&dir);
        assert_eq!(catalog.ready_names().collect::<Vec<_>>(), ["Work"]);
        assert_eq!(catalog.resolve("work"), catalog.resolve("WORK"));

        // These can coexist on a case-sensitive host (the in-memory constructor
        // makes that generation portable to case-insensitive test hosts).
        // Neither spelling may silently win, because the picker and config
        // resolver intentionally expose one case-insensitive namespace.
        let collided = ThemeCatalog::from_schemes([
            (
                "Work".to_string(),
                aterm_types::scheme::builtin("Dracula").unwrap(),
            ),
            (
                "work".to_string(),
                aterm_types::scheme::builtin("GitHub Light").unwrap(),
            ),
        ]);
        assert!(collided.ready_names().next().is_none());
        let upper = collided.resolve("Work").unwrap_err();
        let lower = collided.resolve("work").unwrap_err();
        assert_eq!(upper, lower);
        assert!(upper.contains("differ only by ASCII case"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn one_theme_catalog_drives_identical_preview_renderer_and_engine_colors() {
        let dir = theme_fixture_dir("projection");
        write_theme(&dir.join("Work.conf"), "#123456", "#abcdef");
        let catalog = ThemeCatalog::discover_in(&dir);
        let config = cfg("theme = \"work\"");
        let scheme = catalog.resolve("Work").unwrap();
        assert_eq!(catalog.resolve("work").unwrap(), scheme);
        let renderer = config.theme_for_with_assets(aterm_types::Appearance::Dark, &catalog);
        let engine =
            config.applied_terminal_config_for_with_assets(aterm_types::Appearance::Dark, &catalog);
        assert_eq!(renderer.fg, scheme.to_theme_parts().fg);
        assert_eq!(renderer.bg, scheme.to_theme_parts().bg);
        assert_eq!(engine.default_foreground, scheme.foreground);
        assert_eq!(engine.default_background, scheme.background);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn kitty_asset_resolves_once_to_shared_rgba_with_stable_identity() {
        let path = kitty_fixture("ready");
        let rgba = [
            0xff, 0x10, 0x20, 0xff, 0x20, 0xff, 0x30, 0x80, 0x10, 0x20, 0xff, 0x40, 0xff, 0xff,
            0xff, 0xff,
        ];
        let png = crate::app_introspect::encode_rgba8_png(&rgba, 2, 2).unwrap();
        std::fs::write(&path, png).unwrap();
        let config = config_with_kitty(&path);
        let first = config.resolve_asset_catalog();
        let second = config.resolve_asset_catalog();
        let (
            KittySpriteAsset::Ready {
                source_id,
                w,
                h,
                rgba: resolved,
                fp,
            },
            KittySpriteAsset::Ready { fp: second_fp, .. },
        ) = (&first.kitty_sprite, &second.kitty_sprite)
        else {
            panic!("valid PNG must resolve Ready");
        };
        assert_eq!(source_id.as_ref(), path.to_string_lossy());
        assert_eq!((*w, *h), (2, 2));
        assert_eq!(resolved.as_ref(), rgba);
        assert_ne!(*fp, 0);
        assert_eq!(fp, second_fp, "same source bytes have stable identity");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn preliminary_asset_catalog_never_claims_inline_only_sparkle_authority() {
        let config = cfg(concat!(
            "[[sparkle_words.custom]]\n",
            "words = [\"typedword\"]\n",
            "burst = { kind = \"nova\", chance = 100 }\n",
        ));
        assert!(
            config
                .prepare_sparkle_runtime()
                .consumer_capabilities()
                .nova_burst,
            "negative control: the authored inline recipe has a real consumer"
        );
        assert!(
            config
                .resolve_asset_catalog()
                .sparkle_spec_consumers
                .is_none(),
            "a catalog that has not compiled Toy Packs must remain explicitly unobserved"
        );
    }

    #[test]
    fn kitty_asset_missing_or_oversized_source_is_explicit_invalid() {
        let missing = kitty_fixture("missing");
        let catalog = config_with_kitty(&missing).resolve_asset_catalog();
        assert!(matches!(
            &catalog.kitty_sprite,
            KittySpriteAsset::Invalid { bounded_reason, .. }
                if bounded_reason.contains("unreadable")
        ));

        let oversized = kitty_fixture("encoded-limit");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len((MAX_KITTY_SPRITE_FILE_BYTES as u64) + 1)
            .unwrap();
        drop(file);
        let catalog = config_with_kitty(&oversized).resolve_asset_catalog();
        assert!(matches!(
            &catalog.kitty_sprite,
            KittySpriteAsset::Invalid { bounded_reason, .. }
                if bounded_reason.contains("exceeds")
        ));
        let _ = std::fs::remove_file(oversized);
    }

    #[cfg(unix)]
    #[test]
    fn kitty_asset_refuses_writerless_fifo_and_final_symlink() {
        use std::os::unix::ffi::OsStrExt as _;

        let fifo = kitty_fixture("writerless-fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path");
        // SAFETY: `fifo_c` is a live NUL-terminated pathname and mkfifo retains
        // no pointer. `kitty_fixture` gives this test a unique final component.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert!(
            matches!(
                config_with_kitty(&fifo)
                    .resolve_asset_catalog()
                    .kitty_sprite,
                KittySpriteAsset::Invalid { .. }
            ),
            "a writerless FIFO must fail immediately instead of parking config admission"
        );

        let target = kitty_fixture("symlink-target");
        let linked = kitty_fixture("symlink-final");
        std::fs::write(&target, b"not consulted").expect("write symlink target");
        std::os::unix::fs::symlink(&target, &linked).expect("create final symlink");
        assert!(
            matches!(
                config_with_kitty(&linked)
                    .resolve_asset_catalog()
                    .kitty_sprite,
                KittySpriteAsset::Invalid { .. }
            ),
            "a final-component symlink must not be followed"
        );

        let _ = std::fs::remove_file(fifo);
        let _ = std::fs::remove_file(linked);
        let _ = std::fs::remove_file(target);
    }

    #[test]
    fn kitty_asset_dimension_cap_fails_closed() {
        let path = kitty_fixture("dimension-limit");
        let rgba = vec![0x7f; (super::MAX_KITTY_SPRITE_DIMENSION + 1) * 4];
        let png = crate::app_introspect::encode_rgba8_png(
            &rgba,
            (super::MAX_KITTY_SPRITE_DIMENSION + 1) as u32,
            1,
        )
        .unwrap();
        std::fs::write(&path, png).unwrap();
        let catalog = config_with_kitty(&path).resolve_asset_catalog();
        assert!(matches!(
            &catalog.kitty_sprite,
            KittySpriteAsset::Invalid { bounded_reason, .. }
                if bounded_reason.contains("pixels per side")
        ));
        let _ = std::fs::remove_file(path);
    }

    fn write_toy_pack(label: &str, words: &[&str], burst: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aterm-toy-pack-test-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create toy-pack fixture directory");
        let path = dir.join("pack.toml");
        let words = words
            .iter()
            .map(|word| super::toml_basic_string(word))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!(
            "schema = 1\n\
             [pack]\n\
             id = \"community.test.{label}\"\n\
             name = \"{label}\"\n\
             version = 1\n\
             authors = [\"aterm tests\"]\n\
             license = \"Apache-2.0\"\n\
             [[toy]]\n\
             id = \"recipe\"\n\
             lang = \"en\"\n\
             words = [{words}]\n\
             burst = {{ kind = \"{burst}\" }}\n"
        );
        std::fs::write(&path, source).expect("write toy-pack fixture");
        path
    }

    fn config_with_toy_packs(paths: &[std::path::PathBuf], tail: &str) -> Config {
        let paths = paths
            .iter()
            .map(|path| super::toml_basic_string(&path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(", ");
        cfg(&format!("[sparkle_words]\ntoy_packs = [{paths}]\n{tail}"))
    }

    #[test]
    fn bidi_disabled_maps_to_engine() {
        let tc = cfg("bidi = \"disabled\"")
            .terminal_config()
            .expect("bidi sets engine config");
        assert_eq!(tc.bidi.mode, BiDiMode::Disabled);
    }

    /// W6: a DEFAULT config resolves to the default [`super::FontConfig`] with
    /// no warnings — the startup `apply_font_config` is then a provable no-op
    /// (every renderer setter no-ops on its construction value).
    #[test]
    fn w6_font_config_default_resolves_to_noop() {
        let (fc, warns) = super::FontConfig::from_config(&Config::default());
        assert_eq!(fc, super::FontConfig::default());
        assert!(
            warns.is_empty(),
            "no warnings for an unset config: {warns:?}"
        );
        assert!(fc.synthetic_style, "synthesis defaults ON (byte-identical)");
    }

    /// W6: unresolvable entries warn and are SKIPPED (never a hard failure);
    /// a file path entry resolves verbatim; `font_synthetic_style = false`
    /// carries through.
    #[test]
    fn w6_font_config_warns_and_skips_unresolvable() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aterm-render/assets/DejaVuSansMono.ttf"
        );
        let toml = format!(
            "font_family_bold = \"definitely-not-a-real-font-xyzzy\"\n\
             font_synthetic_style = false\n\
             fallback_fonts = [\"{fixture}\", \"also-not-a-real-font-xyzzy\"]\n"
        );
        let (fc, warns) = super::FontConfig::from_config(&cfg(&toml));
        assert_eq!(fc.styled_paths[0], None, "bogus bold family skipped");
        assert!(!fc.synthetic_style);
        // The resolvable path entry survives; the bogus one is dropped.
        assert_eq!(fc.fallback_fonts, vec![fixture.to_string()]);
        assert_eq!(
            warns.len(),
            2,
            "one warning per unresolvable entry: {warns:?}"
        );
        assert!(warns[0].contains("font_family_bold"));
        assert!(warns[1].contains("fallback_fonts"));
    }

    /// UI apply fail-safe: an authored path which fails bounded admission is a
    /// rejected replacement, not an instruction to clear the last working face.
    /// The exact size policy reaches the Settings/Manual diagnostic.
    #[test]
    fn w6_rejected_oversized_face_preserves_last_working_font() {
        let root = std::env::temp_dir().join(format!(
            "aterm-font-config-oversized-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("oversized.ttf");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(aterm_render::font_file::MAX_FONT_FILE_BYTES as u64 + 1)
            .unwrap();
        let config = cfg(&format!(
            "font_family_bold = {}\n",
            super::toml_basic_string(&path.to_string_lossy())
        ));
        let (mut next, warnings) = super::FontConfig::from_config(&config);
        let mut working = super::FontConfig::default();
        working.styled_paths[0] = Some("/admitted/working-bold.ttf".to_string());
        next.preserve_rejected_from(&config, &working);

        assert_eq!(next.styled_paths[0], working.styled_paths[0]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("font_family_bold"), "{warnings:?}");
        assert!(
            warnings[0].contains(&format!(
                "font file exceeds the {}-byte limit",
                aterm_render::font_file::MAX_FONT_FILE_BYTES
            )),
            "{warnings:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn w6_writerless_fifo_is_diagnosed_without_entering_live_font_config() {
        use std::os::unix::ffi::OsStrExt as _;

        let root =
            std::env::temp_dir().join(format!("aterm-font-config-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("face.fifo");
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the CString is live for the call and the private fixture path
        // does not alias another test's FIFO.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let config = cfg(&format!(
            "font_family_italic = {}\n",
            super::toml_basic_string(&path.to_string_lossy())
        ));
        let (resolved, warnings) = super::FontConfig::from_config(&config);
        assert!(resolved.styled_paths[1].is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("font_family_italic"));
        assert!(warnings[0].contains("not an admissible font"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Focus-boost default — ON (the anti-starvation lane costs nothing when
    /// idle and only touches the shell root + conhost), and the opt-out
    /// spelling `focus_boost = false` is honoured.
    #[test]
    fn focus_boost_defaults_on() {
        assert!(
            Config::default().focus_boost_or_default(),
            "focus boost defaults ON"
        );
        assert!(
            !cfg("focus_boost = false").focus_boost_or_default(),
            "opt-out honoured"
        );
        assert!(
            cfg("focus_boost = true").focus_boost_or_default(),
            "explicit ON honoured"
        );
    }

    /// M2: stream-fade defaults — OFF (minimal fast defaults, 6272bd7a), window
    /// 90 ms — and the `stream_fade_ms` clamp (16..=1000), so a typo can't wedge
    /// the fade timer on or strobe it.
    #[test]
    fn m2_stream_fade_defaults_and_clamp() {
        let d = Config::default();
        assert!(!d.stream_fade_or_default(), "stream fade defaults OFF");
        assert_eq!(d.stream_fade_ms_or_default(), 90);
        let c = cfg("stream_fade = true\nstream_fade_ms = 5");
        assert!(c.stream_fade_or_default(), "master switch honoured");
        assert_eq!(c.stream_fade_ms_or_default(), 16, "clamped low");
        assert_eq!(
            cfg("stream_fade_ms = 99999").stream_fade_ms_or_default(),
            1000,
            "clamped high"
        );
    }

    #[test]
    fn restart_notices_flags_columns_and_lines_but_nothing_else() {
        use super::restart_notices;
        // No change → no notice (a metadata-only save must not nag).
        let a = cfg("columns = 100\nlines = 40");
        assert!(restart_notices(&a, &a).is_empty());
        // A non-restart key changing (font size, hot-applied) → still no notice.
        let hot = cfg("columns = 100\nlines = 40\nfont_px = 18");
        assert!(restart_notices(&a, &hot).is_empty());
        // columns changed → one notice; lines changed → one notice; both → still one.
        let cols = cfg("columns = 120\nlines = 40");
        assert_eq!(restart_notices(&a, &cols).len(), 1);
        let rows = cfg("columns = 100\nlines = 50");
        assert_eq!(restart_notices(&a, &rows).len(), 1);
        let both = cfg("columns = 120\nlines = 50");
        assert_eq!(restart_notices(&a, &both).len(), 1);
        // Clearing an explicit value (revert to default) is also a real change.
        let cleared = cfg("lines = 40");
        assert_eq!(restart_notices(&a, &cleared).len(), 1);
        assert!(restart_notices(&a, &both)[0].contains("next launch"));
    }

    #[test]
    fn cursor_style_runtime_is_trimmed_and_ascii_case_insensitive() {
        use aterm_core::terminal::CursorStyle;

        for (source, expected) in [
            ("cursor_style = \" BAR \"", CursorStyle::BlinkingBar),
            (
                "cursor_style = \"Beam\"\ncursor_blink = false",
                CursorStyle::SteadyBar,
            ),
            (
                "cursor_style = \"Underline\"\ncursor_blink = false",
                CursorStyle::SteadyBar,
            ),
        ] {
            assert_eq!(
                cfg(source).terminal_config().unwrap().cursor_style,
                expected,
                "registered spelling must resolve identically in the runtime: {source}"
            );
        }
    }

    #[test]
    fn bidi_runtime_is_trimmed_case_insensitive_and_accepts_registered_aliases() {
        let tc = cfg("bidi = \" Explicit \"").terminal_config().unwrap();
        assert_eq!(tc.bidi.mode, BiDiMode::Explicit);
        let tc = cfg("bidi = \" ON \"").terminal_config().unwrap();
        assert_eq!(tc.bidi.mode, BiDiMode::Implicit);
    }

    #[test]
    fn ambiguous_width_runtime_is_trimmed_case_insensitive_and_accepts_aliases() {
        let tc = cfg("ambiguous_width = \" wide \"")
            .terminal_config()
            .unwrap();
        assert!(tc.ambiguous_width_double);
        let tc = cfg("ambiguous_width = \" NARROW \"")
            .terminal_config()
            .unwrap();
        assert!(!tc.ambiguous_width_double);
        let tc = cfg("ambiguous_width = \" DOUBLE \"")
            .terminal_config()
            .unwrap();
        assert!(tc.ambiguous_width_double);
    }

    #[test]
    fn text_shaping_default_is_enabled_no_features() {
        use aterm_types::text_shaping::LigatureMode;
        let s = Config::default().text_shaping();
        assert_eq!(s.ligature_mode, LigatureMode::Enabled);
        assert!(s.font_features.is_empty());
        // The unset config must be byte-identical to the renderer default.
        assert_eq!(s, aterm_render::TextShapingConfig::default());
    }

    #[test]
    fn ligatures_false_disables() {
        use aterm_types::text_shaping::LigatureMode;
        assert_eq!(
            cfg("ligatures = false").text_shaping().ligature_mode,
            LigatureMode::Disabled
        );
        assert_eq!(
            cfg("ligatures = true").text_shaping().ligature_mode,
            LigatureMode::Enabled
        );
    }

    /// W2 `text_blending`: absent/`linear-corrected` (any case, either
    /// separator) → the corrected default; `linear` opts out; an unknown value
    /// fails safe to the default rather than erroring the whole config.
    #[test]
    fn text_blending_parses_with_corrected_default() {
        use aterm_render::TextBlending;
        assert_eq!(
            Config::default().text_blending_or_default(),
            TextBlending::LinearCorrected,
            "absent key must default to linear-corrected (the W2 product choice)"
        );
        assert_eq!(
            cfg("text_blending = \"linear\"").text_blending_or_default(),
            TextBlending::Linear
        );
        assert_eq!(
            cfg("text_blending = \"linear-corrected\"").text_blending_or_default(),
            TextBlending::LinearCorrected
        );
        assert_eq!(
            cfg("text_blending = \"Linear_Corrected\"").text_blending_or_default(),
            TextBlending::LinearCorrected,
            "case-insensitive with underscore alias"
        );
        assert_eq!(
            cfg("text_blending = \"gamma\"").text_blending_or_default(),
            TextBlending::LinearCorrected,
            "unknown value fails safe to the default"
        );
    }

    /// W2 `font_thicken` + `stem_gamma`: defaults (off / identity), explicit
    /// values, and the stem-gamma clamp. (The `$ATERM_STEM_GAMMA` env alias
    /// wins by the same env > config precedence as every other key; not
    /// exercised here to keep the test env-hermetic.)
    #[test]
    fn font_thicken_and_stem_gamma_parse() {
        assert!(!Config::default().font_thicken_or_default());
        assert!(cfg("font_thicken = true").font_thicken_or_default());
        assert_eq!(Config::default().stem_gamma_or_default(), 1.0);
        assert_eq!(cfg("stem_gamma = 0.85").stem_gamma_or_default(), 0.85);
        assert_eq!(
            cfg("stem_gamma = 99.0").stem_gamma_or_default(),
            3.0,
            "clamped to the renderer's 0.30..=3.0"
        );
        assert_eq!(cfg("stem_gamma = 0.01").stem_gamma_or_default(), 0.30);
    }

    #[test]
    fn trail_sound_volume_clamps_finite_values_and_fails_silent_on_nonfinite() {
        assert_eq!(Config::default().trail_sound_volume(), 0.4);
        assert_eq!(cfg("trail_sound_volume = 2.0").trail_sound_volume(), 1.0);
        assert_eq!(cfg("trail_sound_volume = -1.0").trail_sound_volume(), 0.0);
        for source in [
            "trail_sound_volume = nan",
            "trail_sound_volume = inf",
            "trail_sound_volume = -inf",
        ] {
            assert_eq!(cfg(source).trail_sound_volume(), 0.0, "{source}");
        }
    }

    /// The tone-melody knob: shipped ENABLED (subtle by design — the neutral
    /// verdict is bit-exactly today's sound), a plain bool that round-trips,
    /// and `false` resolves off (which both pins the neutral melody at the
    /// drain seams and stops the classifier via `tone_infer_active`).
    #[test]
    fn tone_melody_defaults_on_and_round_trips() {
        assert!(Config::default().tone_melody_or_default());
        assert!(cfg("tone_melody = true").tone_melody_or_default());
        assert!(!cfg("tone_melody = false").tone_melody_or_default());
    }

    /// Robi is opt-IN (owner directive: disabled by default); `robi = true`
    /// invites him back and `false` round-trips.
    #[test]
    fn robi_defaults_off_and_round_trips() {
        assert!(!Config::default().robi_or_default());
        assert!(cfg("robi = true").robi_or_default());
        assert!(!cfg("robi = false").robi_or_default());
    }

    /// The ambient-bed knob: shipped DISABLED (owner: the drone is opt-in;
    /// notes/brrrring/bonk/melody are unaffected by it), a plain bool that
    /// round-trips, and `true` re-enables the bed at the drain seams.
    #[test]
    fn trail_sound_bed_defaults_off_and_round_trips() {
        assert!(!Config::default().trail_sound_bed_or_default());
        assert!(cfg("trail_sound_bed = true").trail_sound_bed_or_default());
        assert!(!cfg("trail_sound_bed = false").trail_sound_bed_or_default());
    }

    #[test]
    fn font_features_parse_onto_primary_face() {
        let s = cfg("font_features = [\"ss01\", \"-calt\", \"+zero\"]").text_shaping();
        assert_eq!(s.font_features.len(), 1, "one primary-face feature set");
        let set = &s.font_features[0];
        assert_eq!(set.font_id, 0);
        let tags: Vec<(&[u8], u32)> = set.features.iter().map(|f| (&f.tag[..], f.value)).collect();
        assert_eq!(
            tags,
            vec![(&b"ss01"[..], 1), (&b"calt"[..], 0), (&b"zero"[..], 1)]
        );
    }

    #[test]
    fn font_features_grouped_entry_splits() {
        // A single list entry may carry several space-separated tags.
        let s = cfg("font_features = [\"+ss01 +zero\"]").text_shaping();
        assert_eq!(s.font_features[0].features.len(), 2);
    }

    #[test]
    fn font_features_empty_yields_no_set() {
        // All-malformed input collapses to no feature set (a no-op, not an empty set).
        assert!(
            cfg("font_features = [\"+toolong\"]")
                .text_shaping()
                .font_features
                .is_empty()
        );
    }

    proptest::proptest! {
        /// Mapping invariants of `Config::text_shaping` over ARBITRARY typography keys
        /// (the example tests above cover points; this covers the space). For ANY
        /// `ligatures` / `font_features`: (1) the ligature mode is total + exact
        /// (`Some(false)` => Disabled, else Enabled); (2) features land on at most ONE
        /// primary-face (`font_id == 0`) set that is never empty; (3) `ambiguous_width`
        /// is never set here (the engine cell-width path owns it).
        #[test]
        fn text_shaping_mapping_invariants(
            lig in proptest::option::of(proptest::prelude::any::<bool>()),
            feats in proptest::option::of(
                proptest::collection::vec("[+-]?[a-zA-Z0-9 ]{0,6}", 0..5)
            ),
        ) {
            use aterm_types::text_shaping::{AmbiguousWidth, LigatureMode};
            let config = Config {
                ligatures: lig,
                font_features: feats,
                ..Config::default()
            };
            let s = config.text_shaping();
            let expected = if lig == Some(false) {
                LigatureMode::Disabled
            } else {
                LigatureMode::Enabled
            };
            proptest::prop_assert_eq!(s.ligature_mode, expected);
            proptest::prop_assert_eq!(s.ambiguous_width, AmbiguousWidth::Single);
            proptest::prop_assert!(s.font_features.len() <= 1);
            if let Some(set) = s.font_features.first() {
                proptest::prop_assert_eq!(set.font_id, 0);
                proptest::prop_assert!(!set.features.is_empty());
            }
        }
    }

    #[test]
    fn absent_keys_leave_engine_defaults_and_no_config() {
        // No engine-affecting keys -> terminal_config() is None (GUI skips apply).
        assert!(cfg("font_px = 14.0").terminal_config().is_none());
        // bidi default stays Implicit when only an unrelated key is set elsewhere.
        let tc = cfg("bidi = \"implicit\"").terminal_config().unwrap();
        assert_eq!(tc.bidi.mode, BiDiMode::Implicit);
    }

    #[test]
    fn security_flags_opt_in() {
        let tc = cfg("allow_osc52_query = true\nallow_window_ops = true\n\
             allow_notifications = true\nallow_palette_reconfigure = true")
        .terminal_config()
        .unwrap();
        assert!(tc.allow_osc52_query);
        assert!(tc.allow_window_ops);
        assert!(tc.allow_notifications);
        assert!(tc.allow_palette_reconfigure);
    }

    #[test]
    fn security_flags_fail_closed_when_absent() {
        // A config that sets only an unrelated engine key must NOT enable any
        // security flag — they stay fail-closed (default false).
        let tc = cfg("scrollback_lines = 5000").terminal_config().unwrap();
        assert!(!tc.allow_osc52_query);
        assert!(!tc.allow_window_ops);
        assert!(!tc.allow_notifications);
        assert!(!tc.allow_palette_reconfigure);
    }

    /// The two live Sparkle-word products are ON by default—an absent
    /// `[sparkle_words]` table enables profanity + feline + bare-`cat`; retained
    /// Orca config stays suspended, and an explicit master-off fully opts out.
    #[test]
    fn sparkle_words_default_on_for_the_two_live_products() {
        let deco = Config::default()
            .sparkle_deco_config()
            .expect("sparkle words are ON by default (absent table → defaults)");
        assert!(deco.profanity, "profanity sparkles by default");
        assert!(deco.feline, "feline cat-paw on by default");
        // v3 §4: the orca class is SUSPENDED — this assertion is tied to
        // `aterm_effects::ORCA_SUSPENDED` (flip the const to re-enable and
        // revert this line to `assert!(deco.orca, ...)`).
        assert!(
            !deco.orca,
            "orca splash suspended (ORCA_SUSPENDED) — resolver gate ANDs the const"
        );
        assert!(
            deco.allow_bare_cat,
            "the literal `cat` decorates by default"
        );
        // An explicit master-off still disables the whole feature.
        assert!(
            cfg("[sparkle_words]\nenabled = false")
                .sparkle_deco_config()
                .is_none(),
            "enabled = false fully opts out"
        );
        // v2: ink + emphasis ride the same defaults (on under the master).
        assert!(deco.emphasis, "emphasis (ink-only class) on by default");
        assert!(deco.ink_enabled, "animated ink on by default");
        assert_eq!(deco.ink_strength, 0.75);
        assert_eq!(deco.ink_sweep_ms, 2200);
        assert!(!deco.ink_loop, "one sweep per appearance by default");
        // Cat is the only graphic mode. Retired compatibility keys do not enter
        // the runtime effects carrier at all.
        assert_eq!(deco.feline_style, crate::word_decorations::FelineStyle::Cat);
        assert!(deco.feline_magic, "Fortune/Nebula windows on by default");
    }

    /// v3 §3.1/§3.2: `style = "rainbow"` is the new profanity default (nova /
    /// sparkle stay selectable) and `supernova_chance` clamps 0..=100.
    #[test]
    fn sparkle_profanity_defaults_to_rainbow_with_chance_clamp() {
        let deco = Config::default().sparkle_deco_config().expect("defaults");
        assert_eq!(
            deco.profanity_style,
            crate::word_decorations::ProfanityStyle::Rainbow,
            "rainbow is the v3 default"
        );
        // 10, matching the `supernova_chance` field's DOCUMENTED contract ("Default
        // 10"). The resolver shipped `unwrap_or(30)` — so the escalation ladder, whose
        // nuke tier owns the screen for 3.6 s including a 260 ms full-screen flash and
        // can ignite from a word still being typed, fired three times as often as
        // documented. This test pinned the code rather than the contract.
        assert_eq!(deco.supernova_chance, 10, "10% escalation by default");
        let deco = cfg("[sparkle_words.profanity]\nstyle = \"nova\"\nsupernova_chance = 250")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(
            deco.profanity_style,
            crate::word_decorations::ProfanityStyle::Nova,
            "the classic nova stays selectable"
        );
        assert_eq!(deco.supernova_chance, 100, "chance clamps to 0..=100");
        let deco = cfg("[sparkle_words.profanity]\nstyle = \"sparkle\"")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(
            deco.profanity_style,
            crate::word_decorations::ProfanityStyle::Sparkle,
            "the v1 sparkle stays selectable"
        );
    }

    /// FIX IV regression: the native style match is trimmed and case-INsensitive, like
    /// the web `set_sparkle_profanity` setter — `style = "Nova"` selects the
    /// classic nova (it must not silently become Rainbow + supernova roll),
    /// `"SPARKLE"` the v1 sparkle.
    #[test]
    fn sparkle_profanity_style_matches_trimmed_and_case_insensitively() {
        let deco = cfg("[sparkle_words.profanity]\nstyle = \" Nova \"")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(
            deco.profanity_style,
            crate::word_decorations::ProfanityStyle::Nova,
            "\" Nova \" resolves to the classic nova, not Rainbow"
        );
        let deco = cfg("[sparkle_words.profanity]\nstyle = \" SPARKLE \"")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(
            deco.profanity_style,
            crate::word_decorations::ProfanityStyle::Sparkle,
            "\" SPARKLE \" resolves to the v1 sparkle"
        );
        // Unknown strings still fail open to the v3 rainbow default.
        let deco = cfg("[sparkle_words.profanity]\nstyle = \"comet\"")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(
            deco.profanity_style,
            crate::word_decorations::ProfanityStyle::Rainbow
        );
    }

    /// v3 §6: `[[sparkle_words.custom]]` parses, keys the resolved spec
    /// table by the scanner's folded form_hash, appends its words to the
    /// synthesized lexicon override (CJK as `cjk = true`), and keeps the
    /// emphasis class scanning with ink disabled (the resolve gate is
    /// `enabled && (ink_enabled || has_custom_specs)`).
    #[test]
    fn sparkle_custom_specs_resolve_and_survive_ink_off() {
        let toml = concat!(
            "[sparkle_words.ink]\nenabled = false\n",
            "[[sparkle_words.custom]]\nwords = [\"Ultrathink\", \"猫神\"]\n",
            "ink = { colorway = \"rainbow\" }\n",
        );
        let c = cfg(toml);
        let deco = c.sparkle_deco_config().expect("resolves");
        assert!(deco.spec_table.has_custom(), "custom specs registered");
        assert!(
            deco.spec_table
                .override_for(aterm_lexicon::form_hash("ultrathink"))
                .is_some(),
            "keyed by the FOLDED surface hash"
        );
        assert!(
            deco.spec_table
                .override_for(aterm_lexicon::form_hash("猫神"))
                .is_some(),
            "CJK surfaces keyed RAW"
        );
        assert!(
            !deco.ink_enabled && deco.emphasis,
            "emphasis scans with ink off when custom specs exist (v3 §6 gate)"
        );
        // Without customs, ink off still gates emphasis off (v2 behavior).
        let deco = cfg("[sparkle_words.ink]\nenabled = false")
            .sparkle_deco_config()
            .unwrap();
        assert!(!deco.emphasis, "no customs: the ink AND-gate stands");
        // The override TOML carries both surface shapes.
        let over = c.sparkle_override_toml().expect("custom words synthesize");
        assert!(over.contains("forms = [\"Ultrathink\"]"), "{over}");
        assert!(over.contains("cjk = true"), "{over}");
        assert!(over.contains("forms = [\"猫神\"]"), "{over}");
    }

    /// Top Settings promises two independent keyword toys. Turning Sparkle Words
    /// off must suppress every non-feline route, including custom specs that used
    /// to bypass the profanity flag, while Keyword Kitties remains live.
    #[test]
    fn sparkle_words_product_toggle_suppresses_all_non_feline_effects() {
        let c = cfg(concat!(
            "[sparkle_words.profanity]\nenabled = false\n",
            "[sparkle_words.emphasis]\nenabled = true\nextra_words = [\"wow\"]\n",
            "[[sparkle_words.custom]]\nwords = [\"ultrathink\"]\n",
            "burst = { kind = \"starburst\", chance = 100 }\n",
        ));
        let deco = c
            .sparkle_deco_config()
            .expect("keyword kitties remain independently enabled");
        assert!(!deco.profanity, "built-in sparkle words are off");
        assert!(!deco.emphasis, "emphasis cannot bypass the product switch");
        assert!(
            !deco.orca,
            "every non-feline class shares the product switch"
        );
        assert!(deco.feline, "keyword kitties remain independently on");
        assert!(
            !deco.spec_table.has_custom(),
            "custom and Toy Pack effects cannot bypass the product switch"
        );

        assert!(
            cfg("[sparkle_words.profanity]\nenabled = false\n\
                 [sparkle_words.feline]\nenabled = false")
            .sparkle_deco_config()
            .is_none(),
            "turning both top-level toys off produces no decoration engine"
        );
    }

    #[test]
    fn toy_packs_load_skip_invalid_and_apply_documented_precedence() {
        use aterm_effects::spec::BurstKind;

        let first = write_toy_pack("first", &["packshared", "firsttoy"], "sparkle");
        let invalid_dir =
            std::env::temp_dir().join(format!("aterm-toy-pack-invalid-{}", std::process::id()));
        std::fs::create_dir_all(&invalid_dir).expect("create invalid fixture directory");
        let invalid = invalid_dir.join("pack.toml");
        std::fs::write(&invalid, "schema = 99\n").expect("write invalid fixture");
        let later = write_toy_pack("later", &["packshared", "latertoy"], "glow");
        let paths = [first.clone(), invalid.clone(), later.clone()];

        let imported = config_with_toy_packs(&paths, "");
        let (deco, override_toml) = imported.sparkle_runtime_parts().expect("sparkle resolves");
        assert!(
            deco.profanity && deco.feline,
            "an invalid pack disables nothing"
        );
        let shared = deco
            .spec_table
            .override_for(aterm_lexicon::form_hash("packshared"))
            .and_then(|spec| spec.burst)
            .expect("shared imported recipe");
        assert_eq!(shared.kind, BurstKind::Glow, "later valid pack wins");
        let lexicon =
            aterm_lexicon::Lexicon::with_languages_and_override(&["en"], override_toml.as_deref())
                .expect("compiled pack lexicon parses");
        assert_eq!(
            lexicon
                .scan(
                    "firsttoy packshared latertoy",
                    &aterm_lexicon::ScanOptions::default()
                )
                .len(),
            3,
            "both valid packs reach the production scanner"
        );

        let inline = config_with_toy_packs(
            &paths,
            "[[sparkle_words.custom]]\n\
             words = [\"packshared\"]\n\
             burst = { kind = \"nova\", chance = 100 }\n",
        );
        let (deco, _) = inline.sparkle_runtime_parts().expect("inline resolves");
        let shared = deco
            .spec_table
            .override_for(aterm_lexicon::form_hash("packshared"))
            .and_then(|spec| spec.burst)
            .expect("inline shared recipe");
        assert_eq!(
            shared.kind,
            BurstKind::Nova,
            "inline custom is the final, most-local override"
        );

        for path in [first, invalid, later] {
            let _ = std::fs::remove_dir_all(path.parent().expect("fixture parent"));
        }
    }

    #[test]
    fn render_recompute_uses_only_the_prepared_sparkle_generation() {
        let pack = write_toy_pack("render-no-host-io", &["preparedword"], "glow");
        let config = config_with_toy_packs(std::slice::from_ref(&pack), "");
        let prepared = config.prepare_sparkle_runtime();

        // Make any accidental render-time reopen observable independently of
        // the counter: the admitted source no longer exists after preparation.
        std::fs::remove_file(&pack).expect("remove admitted Toy Pack source");
        super::reset_sparkle_host_prepare_count();
        let mut app = crate::App::headless_for_test();
        app.config = config;
        app.prepared_sparkle = prepared;
        app.sparkle_dirty = true;
        super::reset_sparkle_host_prepare_count();

        app.recompute_sparkle();
        assert_eq!(
            super::sparkle_host_prepare_count(),
            0,
            "render-time recompute must not open/compile any configured feed"
        );
        assert!(
            app.sparkle.as_ref().is_some_and(|resolved| resolved
                .cfg
                .spec_table
                .override_for(aterm_lexicon::form_hash("preparedword"))
                .is_some()),
            "the immutable prepared Toy Pack remains active after its source disappears"
        );
        let _ = std::fs::remove_dir_all(pack.parent().expect("fixture parent"));
    }

    #[test]
    fn prepared_config_apply_does_not_refingerprint_path_feeds() {
        let dir =
            std::env::temp_dir().join(format!("aterm-prepared-apply-no-io-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "# comment-only generation\n").unwrap();
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        let config = Config::default();
        let assets = config.resolve_asset_catalog();
        let feeds = config.path_feed_fingerprints();
        let sparkle = config.prepare_sparkle_runtime();
        let mut app = crate::App::headless_for_test();
        super::reset_path_feed_fingerprint_count();

        app.apply_prepared_config_generation(
            crate::native_font_catalog::PreparedConfigGeneration {
                observation,
                config,
                values: std::collections::BTreeMap::new(),
                assets,
                path_feed_fps: feeds,
                sparkle,
                fonts: None,
                warnings: Vec::new(),
            },
        );
        assert_eq!(
            super::path_feed_fingerprint_count(),
            0,
            "the event-loop reducer must consume the worker's immutable fingerprint"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_external_lexicon_does_not_drop_valid_toy_pack_layer() {
        let pack = write_toy_pack("lexicon-isolation", &["packstillworks"], "glow");
        let external = pack
            .parent()
            .expect("fixture parent")
            .join("broken-lexicon.toml");
        std::fs::write(&external, "[[entry]\nclass = \"emphasis\"")
            .expect("write malformed external lexicon");
        let tail = format!(
            "lexicon = {}\n",
            super::toml_basic_string(&external.to_string_lossy())
        );
        let config = config_with_toy_packs(std::slice::from_ref(&pack), &tail);
        let (deco, override_toml) = config.sparkle_runtime_parts().expect("sparkle resolves");

        assert!(
            deco.spec_table
                .override_for(aterm_lexicon::form_hash("packstillworks"))
                .is_some(),
            "the compiled Toy Pack spec survives the rejected user layer"
        );
        let lexicon =
            aterm_lexicon::Lexicon::with_languages_and_override(&["en"], override_toml.as_deref())
                .expect("generated layers remain independently valid");
        assert_eq!(
            lexicon
                .scan("packstillworks", &aterm_lexicon::ScanOptions::default())
                .len(),
            1,
            "the Toy Pack trigger still reaches the production scanner"
        );

        std::fs::remove_dir_all(pack.parent().expect("fixture parent"))
            .expect("remove isolation fixtures");
    }

    #[test]
    fn external_lexicon_cap_is_exact_and_enforced_by_the_runtime_loader() {
        let path = kitty_fixture("lexicon-cap");
        let prefix = "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"boundedword\"]\n#";
        let mut exact = prefix.to_string();
        exact.push_str(
            &"x".repeat(aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES - prefix.len()),
        );
        std::fs::write(&path, &exact).expect("write exact-limit lexicon");
        let config = cfg(&format!(
            "[sparkle_words]\nlexicon = {}\n",
            super::toml_basic_string(&path.to_string_lossy())
        ));
        assert!(
            config
                .sparkle_override_toml()
                .is_some_and(|source| source.contains("boundedword")),
            "a regular lexicon exactly at the cap is admitted"
        );

        exact.push('x');
        std::fs::write(&path, exact).expect("write over-limit lexicon");
        assert!(
            config.sparkle_override_toml().is_none(),
            "the same lexicon one byte over the cap is skipped"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn toy_pack_path_count_is_bounded() {
        let active = write_toy_pack("active", &["activepackword"], "glow");
        let overflow = write_toy_pack("overflow", &["overflowpackword"], "nova");
        let mut paths = vec![active.clone(); super::MAX_ACTIVE_TOY_PACKS];
        paths.push(overflow.clone());
        let config = config_with_toy_packs(&paths, "");
        let (deco, override_toml) = config.sparkle_runtime_parts().expect("sparkle resolves");
        assert!(
            deco.spec_table
                .override_for(aterm_lexicon::form_hash("activepackword"))
                .is_some(),
            "accepted paths load"
        );
        assert!(
            deco.spec_table
                .override_for(aterm_lexicon::form_hash("overflowpackword"))
                .is_none(),
            "paths beyond the hard cap are inactive"
        );
        assert!(
            !override_toml
                .as_deref()
                .unwrap_or_default()
                .contains("overflowpackword"),
            "capped paths do not leak into the scanner document"
        );
        for path in [active, overflow] {
            let _ = std::fs::remove_dir_all(path.parent().expect("fixture parent"));
        }
    }

    /// FIX 8 (native path) regression: `recompute_sparkle`'s warning log
    /// filters the lexicon conflicts by the RESOLVED scan options — the
    /// single-char-CJK "requires `cjk_single_char = true`" warning must not
    /// fire when the user's config already enables that opt-in (the lexicon
    /// cannot see scan options), while genuinely unscannable surfaces
    /// (mixed-script) keep warning either way. Exercises the exact
    /// config → deco-config → override → lexicon → `sparkle_logged_warnings`
    /// chain `recompute_sparkle` runs (minus the eprintln).
    #[test]
    fn sparkle_lexicon_cjk_single_char_warning_respects_opt_in() {
        let logged = |feline_extra: &str| -> Vec<String> {
            let toml = format!(
                "{feline_extra}[[sparkle_words.custom]]\nwords = [\"犬\", \"abc猫\"]\n\
                 ink = {{ colorway = \"rainbow\" }}\n"
            );
            let c = cfg(&toml);
            let deco = c.sparkle_deco_config().expect("resolves");
            let over = c.sparkle_override_toml().expect("customs synthesize");
            let lx = aterm_lexicon::Lexicon::with_languages_and_override(&["en"], Some(&over))
                .expect("override parses");
            super::sparkle_logged_warnings(lx.conflicts(), deco.cjk_single_char)
                .cloned()
                .collect()
        };
        // Opt-in absent (default false): the single-char CJK custom word
        // warns that it requires cjk_single_char = true.
        let warns = logged("");
        assert!(
            warns
                .iter()
                .any(|w| w.contains("\"犬\"") && w.contains("requires cjk_single_char = true")),
            "opt-in off: the requires-warning is logged, got {warns:?}"
        );
        assert!(
            warns
                .iter()
                .any(|w| w.contains("\"abc猫\"") && w.contains("dropped")),
            "mixed-script surfaces warn dropped, got {warns:?}"
        );
        // Opt-in enabled: the surface WILL scan — the requires-warning is
        // satisfied and suppressed; the mixed-script one still logs.
        let warns = logged("[sparkle_words.feline]\ncjk_single_char = true\n");
        assert!(
            !warns
                .iter()
                .any(|w| w.contains("requires cjk_single_char = true")),
            "cjk_single_char = true satisfies the warning (FIX 8), got {warns:?}"
        );
        assert!(
            warns
                .iter()
                .any(|w| w.contains("\"abc猫\"") && w.contains("dropped")),
            "unrelated warnings survive the filter, got {warns:?}"
        );
    }

    /// Alt-screen suppression is opt-IN (2026-07-03): the default decorates the
    /// alternate screen too (full-screen TUIs like Claude Code sparkle), and
    /// `suppress_in_alt_screen = true` restores the v1 launch behavior.
    #[test]
    fn sparkle_alt_screen_decorates_by_default_and_knob_opts_out() {
        let deco = Config::default()
            .sparkle_deco_config()
            .expect("sparkle words on by default");
        assert!(
            !deco.suppress_in_alt_screen,
            "the alt screen decorates by default — the v1 hardcoded suppression \
             left Claude Code / lazygit / any full-screen TUI sparkle-less"
        );
        let deco = cfg("[sparkle_words]\nsuppress_in_alt_screen = true")
            .sparkle_deco_config()
            .unwrap();
        assert!(
            deco.suppress_in_alt_screen,
            "suppress_in_alt_screen = true restores the v1 suppression"
        );
    }

    /// `suppress_in_alt_screen` (the design §1 knob, wired 2026-07-04): DEFAULT
    /// FALSE — the alternate screen (Claude Code, any full-screen TUI) decorates
    /// like the primary screen; an explicit `true` restores the old hardcoded
    /// vim/less/htop suppression.
    #[test]
    fn sparkle_words_decorate_alt_screen_by_default() {
        assert!(
            !Config::default().sparkle_suppress_alt_screen(),
            "absent table → alt screen decorates"
        );
        assert!(
            !cfg("[sparkle_words]\nenabled = true").sparkle_suppress_alt_screen(),
            "table present, key absent → alt screen still decorates"
        );
        assert!(
            cfg("[sparkle_words]\nsuppress_in_alt_screen = true").sparkle_suppress_alt_screen(),
            "explicit true restores the pre-2026-07-04 suppression"
        );
        assert!(
            !cfg("[sparkle_words]\nsuppress_in_alt_screen = false").sparkle_suppress_alt_screen(),
            "explicit false is the default, spelled out"
        );
    }

    /// The legacy paw mode remains parseable as ink-only. The four retired
    /// feline controls round-trip as source data but never enter live state.
    #[test]
    fn sparkle_feline_legacy_values_round_trip_without_claiming_removed_effects() {
        let config = cfg(
            "[sparkle_words.feline]\nstyle = \"paw\"\nidle = false\ngaze = false\ncolor = \"#112233\"\nintensity = 0.25\nmagic = false",
        );
        let feline = config
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.feline.as_ref())
            .expect("compatibility values parse");
        assert_eq!(feline.idle, Some(false));
        assert_eq!(feline.gaze, Some(false));
        assert_eq!(feline.color.as_deref(), Some("#112233"));
        assert_eq!(feline.intensity, Some(0.25));

        let deco = config
            .sparkle_deco_config()
            .expect("feline table keeps the feature on");
        assert_eq!(
            deco.feline_style,
            crate::word_decorations::FelineStyle::Paw,
            "style = \"paw\" selects the legacy ink-only mode"
        );
        assert!(!deco.feline_magic, "magic = false ⇒ ordinary builds only");
        // Unknown style values fall back to the only graphic mode.
        let deco = cfg("[sparkle_words.feline]\nstyle = \"lion\"")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(deco.feline_style, crate::word_decorations::FelineStyle::Cat);
    }

    #[test]
    fn sparkle_feline_style_runtime_is_trimmed_and_ascii_case_insensitive() {
        let deco = cfg("[sparkle_words.feline]\nstyle = \" Paw \"")
            .sparkle_deco_config()
            .expect("feline table keeps the feature on");
        assert_eq!(
            deco.feline_style,
            crate::word_decorations::FelineStyle::Paw,
            "the registered cased spelling must select legacy ink-only Paw at runtime"
        );
    }

    /// `[sparkle_words.feline] log` (§F4.7): the STARTER_CONFIG key
    /// deserializes, defaults ON when absent, and is a host-side gate only —
    /// it must NOT perturb the resolved `DecoConfig` (the effects-side
    /// recorder always runs; the App drains-and-drops).
    #[test]
    fn sparkle_feline_log_key_deserializes_and_defaults_on() {
        let c = cfg("[sparkle_words.feline]\nlog = false");
        assert_eq!(
            c.sparkle_words
                .as_ref()
                .and_then(|sw| sw.feline.as_ref())
                .and_then(|f| f.log),
            Some(false),
            "the starter-config key must round-trip"
        );
        assert!(
            c.sparkle_deco_config().is_some(),
            "log = false never disables the decorations themselves"
        );
        let c = cfg("[sparkle_words.feline]\nmagic = true");
        assert_eq!(
            c.sparkle_words
                .as_ref()
                .and_then(|sw| sw.feline.as_ref())
                .and_then(|f| f.log),
            None,
            "absent ⇒ None ⇒ the host resolves it to ON"
        );
    }

    /// `[sparkle_words.profanity]` curse-BONK knobs (the feline `log`
    /// pattern): both keys round-trip, `bonk` resolves ON by default,
    /// `bonk_detonation` OFF by default (typed provenance only), and neither
    /// touches the decorations themselves.
    #[test]
    fn sparkle_profanity_bonk_keys_deserialize_with_host_defaults() {
        let c = cfg("[sparkle_words.profanity]\nbonk = false\nbonk_detonation = true");
        let prof = |c: &Config| c.sparkle_words.clone().and_then(|sw| sw.profanity);
        assert_eq!(prof(&c).and_then(|p| p.bonk), Some(false));
        assert_eq!(prof(&c).and_then(|p| p.bonk_detonation), Some(true));
        assert!(
            c.sparkle_deco_config().is_some(),
            "bonk = false never disables the decorations themselves"
        );
        let c = cfg("[sparkle_words.profanity]\nstyle = \"rainbow\"");
        assert_eq!(
            prof(&c).and_then(|p| p.bonk),
            None,
            "absent ⇒ None ⇒ the host resolves bonk to ON"
        );
        assert_eq!(
            prof(&c).and_then(|p| p.bonk_detonation),
            None,
            "absent ⇒ None ⇒ the host resolves detonation to OFF"
        );
    }

    /// `[sparkle_words.ink]` round-trip: the `loop` TOML key (a Rust keyword,
    /// serde-renamed) parses, and every numeric clamps into its documented range
    /// — including the higher sweep floor a looping shimmer requires.
    #[test]
    fn sparkle_ink_round_trip_and_clamps() {
        let deco = cfg(
            "[sparkle_words.ink]\nenabled = true\nstrength = 0.4\nsweep_ms = 1000\nloop = true",
        )
        .sparkle_deco_config()
        .expect("ink table keeps the feature on");
        assert!(deco.ink_enabled);
        assert_eq!(deco.ink_strength, 0.4);
        assert_eq!(deco.ink_sweep_ms, 1000);
        assert!(deco.ink_loop);
        // Out-of-range values clamp instead of failing the load.
        let deco = cfg("[sparkle_words.ink]\nstrength = 3.0\nsweep_ms = 50")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(deco.ink_strength, 1.0, "strength clamps to 0.0..=1.0");
        assert_eq!(deco.ink_sweep_ms, 350, "sweep_ms clamps to 350..=6000");
        let deco = cfg("[sparkle_words.ink]\nsweep_ms = 99999")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(deco.ink_sweep_ms, 6000);
        // loop = true raises the effective sweep floor to 600 (flash margin).
        let deco = cfg("[sparkle_words.ink]\nloop = true\nsweep_ms = 400")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(
            deco.ink_sweep_ms, 600,
            "looping shimmer floors sweep at 600"
        );
        // Ink off is honoured — and takes emphasis (ink-only) down with it.
        let deco = cfg("[sparkle_words.ink]\nenabled = false")
            .sparkle_deco_config()
            .unwrap();
        assert!(!deco.ink_enabled);
        assert!(!deco.emphasis, "emphasis has no non-ink surface");
    }

    /// `[sparkle_words.emphasis]`: the enable gate, `extra_words` reaching the
    /// lexicon override, and `ignore_words` folding into the shared deny set.
    #[test]
    fn sparkle_emphasis_gate_and_word_lists() {
        let deco = cfg("[sparkle_words.emphasis]\nenabled = false")
            .sparkle_deco_config()
            .unwrap();
        assert!(!deco.emphasis, "emphasis opts out independently");
        assert!(deco.ink_enabled, "ink stays on for profanity/feline");
        let c = cfg(
            "[sparkle_words.emphasis]\nextra_words = [\"megathink\"]\nignore_words = [\"Turbo\"]",
        );
        let toml_out = c
            .sparkle_override_toml()
            .expect("extra words build an override");
        assert!(
            toml_out.contains("class = \"emphasis\"") && toml_out.contains("megathink"),
            "emphasis extra_words must reach the lexicon override, got: {toml_out}"
        );
        let deco = c.sparkle_deco_config().unwrap();
        assert!(
            deco.ignore.contains("turbo"),
            "emphasis ignore_words fold (case-folded) into the deny set"
        );
    }

    #[test]
    fn prepared_emphasis_capability_matches_scannable_unoverridden_runtime_words() {
        let capability = |source: &str| {
            cfg(source)
                .prepare_sparkle_runtime()
                .consumer_capabilities()
                .emphasis_class_default
        };
        assert!(capability(
            "[sparkle_words.emphasis]\nextra_words = [\"megathink\"]\n"
        ));
        for source in [
            "[sparkle_words.emphasis]\nextra_words = []\n",
            "[sparkle_words.emphasis]\nextra_words = [\"   \"]\n",
            "[sparkle_words.emphasis]\nextra_words = [\"two words\"]\n",
            "[sparkle_words.emphasis]\nextra_words = [\"kitty\"]\n",
            "[sparkle_words.emphasis]\nextra_words = [\"megathink\"]\nignore_words = [\"MEGATHINK\"]\n",
            concat!(
                "[sparkle_words.emphasis]\nextra_words = [\"megathink\"]\n",
                "[[sparkle_words.custom]]\nwords = [\"megathink\"]\n",
                "graphic = { collection = \"cats\" }\n",
            ),
        ] {
            assert!(
                !capability(source),
                "no class-default emphasis match should survive: {source}"
            );
        }

        let dir =
            std::env::temp_dir().join(format!("aterm-emphasis-capability-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lexicon = dir.join("lexicon.toml");
        std::fs::write(
            &lexicon,
            concat!(
                "[[entry]]\nclass = \"emphasis\"\nlang = \"en\"\n",
                "mode = \"forms\"\nforms = [\"fromfile\"]\n",
                "[[entry]]\nclass = \"emphasis\"\nlang = \"zh\"\n",
                "mode = \"forms\"\ncjk = true\nforms = [\"超\"]\n",
            ),
        )
        .unwrap();
        let path = super::toml_basic_string(&lexicon.to_string_lossy());
        assert!(capability(&format!("[sparkle_words]\nlexicon = {path}\n")));
        assert!(
            !capability(&format!(
                "[sparkle_words]\nlexicon = {path}\n\
                 [[sparkle_words.custom]]\nwords = [\"fromfile\"]\n\
                 graphic = {{ collection = \"cats\" }}\n\
                 [sparkle_words.emphasis]\nignore_words = [\"超\"]\n"
            )),
            "custom ownership plus ignore removes every external default consumer"
        );

        std::fs::write(
            &lexicon,
            concat!(
                "[[entry]]\nclass = \"emphasis\"\nlang = \"zh\"\n",
                "mode = \"forms\"\ncjk = true\nforms = [\"超\"]\n",
            ),
        )
        .unwrap();
        assert!(!capability(&format!("[sparkle_words]\nlexicon = {path}\n")));
        assert!(capability(&format!(
            "[sparkle_words]\nlexicon = {path}\n\
             [sparkle_words.feline]\ncjk_single_char = true\n"
        )));
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// W5 PROOF (config round-trip): every new key (a) PARSES from `aterm.toml`
/// through serde into its resolver's clamped value, (b) SERIALIZES through the
/// Settings writer (`prefs::apply_prefs_edits`) back into TOML that re-parses
/// to the same value, and (c) HOT-RELOAD-DIFFS to exactly ONE renderer call:
/// the pure [`RenderKnobs::diff`] emits exactly one [`KnobChange`] per changed
/// key (renderer knobs), and the engine keys land on exactly one
/// `TerminalConfig` field each (routed by the engine's own `apply_config`
/// diff). `apply_render_knob` is a 1:1 match from variant to `Backend` call,
/// so the chain key → KnobChange → renderer call has no fan-out anywhere.
#[cfg(test)]
mod w5_knob_tests {
    use super::{Config, KnobChange, RenderKnobs};

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    /// (a) parse + clamp for every new renderer/engine key.
    #[test]
    fn new_keys_parse_and_clamp() {
        assert_eq!(Config::default().line_height_or_default(), 1.0);
        assert_eq!(cfg("line_height = 1.4").line_height_or_default(), 1.4);
        assert_eq!(
            cfg("line_height = 9.0").line_height_or_default(),
            2.0,
            "clamped"
        );
        assert_eq!(
            cfg("line_height = 0.1").line_height_or_default(),
            0.8,
            "clamped"
        );

        assert_eq!(Config::default().minimum_contrast_or_default(), 1.0);
        assert_eq!(
            cfg("minimum_contrast = 4.5").minimum_contrast_or_default(),
            4.5
        );
        assert_eq!(
            cfg("minimum_contrast = 99.0").minimum_contrast_or_default(),
            21.0
        );

        assert_eq!(Config::default().adjust_baseline_or_default(), 0);
        assert_eq!(cfg("adjust_baseline = -3").adjust_baseline_or_default(), -3);
        assert_eq!(
            cfg("adjust_baseline = 400").adjust_baseline_or_default(),
            32
        );

        assert_eq!(Config::default().adjust_underline_position_or_default(), 0);
        assert_eq!(
            cfg("adjust_underline_position = -2").adjust_underline_position_or_default(),
            -2
        );
        assert_eq!(
            cfg("adjust_underline_position = 400").adjust_underline_position_or_default(),
            32,
            "clamped"
        );
        assert_eq!(Config::default().adjust_underline_thickness_or_default(), 0);
        assert_eq!(
            cfg("adjust_underline_thickness = 3").adjust_underline_thickness_or_default(),
            3
        );
        assert_eq!(
            cfg("adjust_underline_thickness = -400").adjust_underline_thickness_or_default(),
            -32,
            "clamped"
        );
        assert!(
            Config::default().underline_skip_descenders_or_default(),
            "descender ink-skip is DEFAULT ON"
        );
        assert!(!cfg("underline_skip_descenders = false").underline_skip_descenders_or_default());

        assert_eq!(Config::default().selection_foreground_u32(), None);
        assert_eq!(
            cfg("selection_foreground = \"#102030\"").selection_foreground_u32(),
            Some(0x0010_2030)
        );

        assert!(!Config::default().selection_inactive_or_default());
        assert!(cfg("selection_inactive = true").selection_inactive_or_default());

        assert_eq!(Config::default().faint_opacity_or_default(), 0.5);
        assert_eq!(cfg("faint_opacity = 0.8").faint_opacity_or_default(), 0.8);
        assert_eq!(cfg("faint_opacity = 7.0").faint_opacity_or_default(), 1.0);

        // M5 vibrancy keys.
        assert_eq!(Config::default().background_opacity_or_default(), 1.0);
        assert_eq!(
            cfg("background_opacity = 0.7").background_opacity_or_default(),
            0.7
        );
        assert_eq!(
            cfg("background_opacity = 2.0").background_opacity_or_default(),
            1.0,
            "clamped"
        );
        assert_eq!(
            cfg("background_opacity = -3.0").background_opacity_or_default(),
            0.0,
            "clamped"
        );
        assert_eq!(
            Config::default().background_material_or_default(),
            super::BackgroundMaterial::None
        );
        assert_eq!(
            cfg("background_material = \"HUD\"").background_material_or_default(),
            super::BackgroundMaterial::Hud,
            "case-insensitive"
        );
        assert_eq!(
            cfg("background_material = \"under_window\"").background_material_or_default(),
            super::BackgroundMaterial::UnderWindow,
            "alias"
        );
        assert_eq!(
            cfg("background_material = \"bogus\"").background_material_or_default(),
            super::BackgroundMaterial::None,
            "unknown → none"
        );
    }

    /// (a) the engine keys land on exactly one `TerminalConfig` field each.
    #[test]
    fn engine_keys_map_to_terminal_config() {
        let tc = cfg("bold_is_bright = false").terminal_config().unwrap();
        assert!(!tc.bold_is_bright);
        let base = aterm_core::config::TerminalConfig::default();
        assert_eq!(
            tc.faint_opacity, base.faint_opacity,
            "only the one field moved"
        );

        let tc = cfg("faint_opacity = 0.25").terminal_config().unwrap();
        assert_eq!(tc.faint_opacity, 0.25);
        assert_eq!(
            tc.bold_is_bright, base.bold_is_bright,
            "only the one field moved"
        );

        // Defaults: absent keys leave the engine defaults (and alone produce
        // no engine config at all — the GUI skips apply).
        assert!(cfg("line_height = 1.2").terminal_config().is_none());
    }

    /// (a) `cursor_break_ligatures` maps to `LigatureMode::CursorDisabled`,
    /// with `ligatures = false` winning (everything is per-cell already).
    #[test]
    fn cursor_break_ligatures_maps_to_cursor_disabled() {
        use aterm_types::text_shaping::LigatureMode;
        assert_eq!(
            Config::default().text_shaping().ligature_mode,
            LigatureMode::Enabled
        );
        assert_eq!(
            cfg("cursor_break_ligatures = true")
                .text_shaping()
                .ligature_mode,
            LigatureMode::CursorDisabled
        );
        assert_eq!(
            cfg("cursor_break_ligatures = false")
                .text_shaping()
                .ligature_mode,
            LigatureMode::Enabled
        );
        assert_eq!(
            cfg("ligatures = false\ncursor_break_ligatures = true")
                .text_shaping()
                .ligature_mode,
            LigatureMode::Disabled,
            "ligatures=false wins"
        );
    }

    /// M4 — `merged_ligatures` maps to `TextShapingConfig::admit_collapsed`,
    /// DEFAULT off (absent key), and does not perturb the other shaping fields.
    #[test]
    fn merged_ligatures_maps_to_admit_collapsed() {
        assert!(
            !Config::default().text_shaping().admit_collapsed,
            "default: merged (Cascadia N:1) ligatures OFF"
        );
        assert!(
            !cfg("merged_ligatures = false")
                .text_shaping()
                .admit_collapsed,
            "explicit false stays off"
        );
        assert!(
            cfg("merged_ligatures = true")
                .text_shaping()
                .admit_collapsed,
            "merged_ligatures = true admits the N:1 collapse"
        );
        // Independent of ligature_mode / font_features.
        let s = cfg("merged_ligatures = true\nligatures = false").text_shaping();
        assert!(s.admit_collapsed);
        assert_eq!(
            s.ligature_mode,
            aterm_types::text_shaping::LigatureMode::Disabled
        );
    }

    /// (b) Settings-writer round-trip: each new key serializes through
    /// `apply_prefs_edits` (typed per `edit_kind`) into TOML that re-parses to
    /// the same resolved value.
    #[test]
    fn new_keys_round_trip_through_prefs_writer() {
        use crate::prefs::{
            EDIT_ADJUST_BASELINE, EDIT_BOLD_IS_BRIGHT, EDIT_CURSOR_BREAK_LIGATURES,
            EDIT_FAINT_OPACITY, EDIT_LINE_HEIGHT, EDIT_MINIMUM_CONTRAST, EDIT_SELECTION_FOREGROUND,
            EDIT_SELECTION_INACTIVE, apply_prefs_edits,
        };
        let out = apply_prefs_edits(
            "",
            &[
                (EDIT_LINE_HEIGHT, Some("1.35".to_string())),
                (EDIT_MINIMUM_CONTRAST, Some("4.5".to_string())),
                (EDIT_SELECTION_FOREGROUND, Some("#aabbcc".to_string())),
                (EDIT_SELECTION_INACTIVE, Some("true".to_string())),
                (EDIT_CURSOR_BREAK_LIGATURES, Some("true".to_string())),
                (EDIT_BOLD_IS_BRIGHT, Some("false".to_string())),
                (EDIT_FAINT_OPACITY, Some("0.4".to_string())),
                (EDIT_ADJUST_BASELINE, Some("-2".to_string())),
            ],
        )
        .expect("writes typed values");
        let c: Config = toml::from_str(&out).expect("round-trips through serde");
        assert_eq!(c.line_height_or_default(), 1.35);
        assert_eq!(c.minimum_contrast_or_default(), 4.5);
        assert_eq!(c.selection_foreground_u32(), Some(0x00AA_BBCC));
        assert!(c.selection_inactive_or_default());
        assert_eq!(c.cursor_break_ligatures, Some(true));
        assert_eq!(c.bold_is_bright, Some(false));
        assert_eq!(c.faint_opacity_or_default(), 0.4);
        assert_eq!(c.adjust_baseline_or_default(), -2);
    }

    /// (c) hot-reload diff: each renderer-knob key, changed ALONE, emits
    /// exactly ONE `KnobChange` — the variant `apply_render_knob` routes to
    /// exactly one `Backend` call. An unchanged config emits none.
    #[test]
    fn each_knob_key_diffs_to_exactly_one_change() {
        let base = RenderKnobs::default();
        assert!(base.diff(&base).is_empty(), "no change → no calls");

        let cases: [(&str, KnobChange); 9] = [
            ("line_height = 1.5", KnobChange::LineHeight(1.5)),
            ("minimum_contrast = 4.5", KnobChange::MinimumContrast(4.5)),
            (
                "selection_foreground = \"#010203\"",
                KnobChange::SelectionFg(Some(0x0001_0203)),
            ),
            (
                "selection_inactive = true",
                KnobChange::SelectionInactive(true),
            ),
            ("adjust_baseline = 2", KnobChange::AdjustBaseline(2)),
            (
                "adjust_underline_position = 2",
                KnobChange::AdjustUnderline(2, 0),
            ),
            (
                "adjust_underline_thickness = 1",
                KnobChange::AdjustUnderline(0, 1),
            ),
            (
                "underline_skip_descenders = false",
                KnobChange::UnderlineSkipDescenders(false),
            ),
            (
                "background_material = \"hud\"",
                KnobChange::BackgroundMaterial(super::BackgroundMaterial::Hud),
            ),
        ];
        for (toml, expected) in cases {
            let knobs = RenderKnobs::from_config(&cfg(toml));
            let changes = base.diff(&knobs);
            assert_eq!(
                changes,
                vec![expected],
                "{toml:?} must diff to exactly one renderer call"
            );
        }
        // And reverting diffs back to exactly one change per key too.
        let all = RenderKnobs::from_config(&cfg(
            "line_height = 1.5\nminimum_contrast = 4.5\nadjust_baseline = 2",
        ));
        assert_eq!(all.diff(&base).len(), 3, "one change per reverted key");
    }

    /// THE MOVE (M5): `background_opacity < 1.0` is the ONE key that deliberately
    /// fans out to TWO knob changes — the opacity itself AND the auto-engaged
    /// WCAG-AA contrast floor (`MinimumContrast(4.5)`), because a user on the
    /// default `minimum_contrast = 1.0` must not get illegible glass. When the
    /// user already configured a floor >= 4.5 the fan-out collapses to just the
    /// opacity (the floor is unchanged), and reverting to solid drops the floor
    /// back to the configured value.
    #[test]
    fn translucency_couples_opacity_to_the_contrast_floor() {
        let base = RenderKnobs::default();
        // Default floor (1.0) + glass → BOTH the opacity and the auto-floor fire.
        let glass = RenderKnobs::from_config(&cfg("background_opacity = 0.6"));
        let changes = base.diff(&glass);
        assert!(
            changes.contains(&KnobChange::BackgroundOpacity(0.6)),
            "opacity change must fire: {changes:?}"
        );
        assert!(
            changes.contains(&KnobChange::MinimumContrast(4.5)),
            "glass must auto-engage the WCAG-AA floor: {changes:?}"
        );
        assert_eq!(changes.len(), 2, "exactly the opacity + the auto-floor");

        // A user who already set a >= 4.5 floor: only the opacity moves.
        let glass_hi =
            RenderKnobs::from_config(&cfg("background_opacity = 0.6\nminimum_contrast = 7.0"));
        let hi_base = RenderKnobs::from_config(&cfg("minimum_contrast = 7.0"));
        assert_eq!(
            hi_base.diff(&glass_hi),
            vec![KnobChange::BackgroundOpacity(0.6)],
            "an already-legible floor is not disturbed by engaging glass"
        );

        // Reverting glass → solid drops the auto-floor back to the configured 1.0.
        assert_eq!(
            glass.diff(&base).len(),
            2,
            "revert drops opacity + auto-floor"
        );
    }

    /// M5 true vibrancy: a translucent `background_opacity` and a valid
    /// `background_material` each resolve to a LIVE value — NOT the silent no-op
    /// default. This guards against a regression where the settings become inert (a
    /// translucent opacity collapsing to `>= 1.0`, or a valid material resolving to
    /// `None`); the resolved values are exactly what the GPU present +
    /// `AppRt::window_set_vibrancy` consume (and the CPU backend warns on).
    #[test]
    fn vibrancy_config_resolves_to_warnable_values() {
        // A translucent opacity resolves < 1.0 → drives the translucent present.
        assert!(
            cfg("background_opacity = 0.7").background_opacity_or_default() < 1.0,
            "a translucent opacity must stay < 1.0, not resolve solid"
        );
        // A valid material resolves to a non-None variant → drives the backdrop
        // (and the diff hands apply_render_knob exactly that knob).
        let mat = cfg("background_material = \"hud\"").background_material_or_default();
        assert_ne!(
            mat,
            super::BackgroundMaterial::None,
            "a valid material must resolve to a real variant, not silently to None"
        );
        let knobs = RenderKnobs::from_config(&cfg("background_material = \"hud\""));
        assert!(
            RenderKnobs::default()
                .diff(&knobs)
                .contains(&KnobChange::BackgroundMaterial(
                    super::BackgroundMaterial::Hud
                )),
            "the material must diff to a knob apply_render_knob drives vibrancy on"
        );
    }
}

/// M5 PROOF (the legibility guarantee, Tier-1). The SHIPPING resolver
/// [`Config::effective_minimum_contrast`] is driven EXHAUSTIVELY over the
/// opacity × configured-contrast lattice, asserting the SAME `NeverIllegible`
/// invariant the `ty` model `aterm_spec::derive::vibrancy_contrast_model` carries
/// abstractly (Tier-0): translucent glass ALWAYS clears WCAG AA (4.5:1),
/// regardless of how low the user set (or left) `minimum_contrast`; an opaque
/// window keeps EXACTLY the configured floor. House style: a NON-VACUITY control
/// (the floor genuinely RISES for a default install running glass) and a NEGATIVE
/// control (the pre-fix defect — raw `minimum_contrast_or_default` would ship
/// sub-4.5 glass — is reproduced and shown excluded).
#[cfg(test)]
mod vibrancy_contrast_guarantee {
    use super::{Config, VIBRANCY_CONTRAST_FLOOR};

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn vibrancy_contrast_guarantee() {
        // Dense lattice: opacity 0.0..=1.0 in 0.05 steps × configured contrast
        // across the WCAG domain (below, at, and above the 4.5 floor).
        let opacities: Vec<f32> = (0..=20).map(|i| i as f32 / 20.0).collect();
        let contrasts = [1.0_f32, 2.0, 3.0, 4.5, 4.6, 7.0, 21.0];

        let mut saw_floor_raised = false;
        let mut saw_opaque_untouched = false;

        for &o in &opacities {
            for &c in &contrasts {
                let config = cfg(&format!("background_opacity = {o}\nminimum_contrast = {c}"));
                let eff = config.effective_minimum_contrast();
                let base = config.minimum_contrast_or_default();

                if o < 1.0 {
                    // THE INVARIANT (== the model's `NeverIllegible`): translucent
                    // glass always clears the WCAG-AA floor.
                    assert!(
                        eff >= VIBRANCY_CONTRAST_FLOOR,
                        "translucent (opacity={o}, configured={c}) yielded effective \
                         contrast {eff} < {VIBRANCY_CONTRAST_FLOOR} — text could sink \
                         into the desktop"
                    );
                    // And it never LOWERS a user's already-stronger floor.
                    assert!(
                        eff >= base,
                        "the auto-floor must never weaken the configured contrast"
                    );
                    if base < VIBRANCY_CONTRAST_FLOOR {
                        saw_floor_raised = true;
                        assert_eq!(eff, VIBRANCY_CONTRAST_FLOOR, "raised exactly to AA");
                    }
                } else {
                    // Opaque window: EXACTLY the configured floor, byte-identical.
                    assert_eq!(
                        eff, base,
                        "opaque (opacity={o}) must keep the configured floor untouched"
                    );
                    saw_opaque_untouched = true;
                }
            }
        }

        // NON-VACUOUS: the lattice actually exercised BOTH the raise and the
        // no-op branch (the guarantee is not trivially true).
        assert!(
            saw_floor_raised,
            "no case raised the floor — the guarantee would be vacuous"
        );
        assert!(saw_opaque_untouched, "no opaque case exercised");

        // NON-VACUITY spotlight: a DEFAULT install (minimum_contrast unset = 1.0)
        // that engages glass is floored to 4.5 — the headline behavior.
        let default_glass = cfg("background_opacity = 0.5");
        assert_eq!(default_glass.minimum_contrast_or_default(), 1.0);
        assert_eq!(
            default_glass.effective_minimum_contrast(),
            VIBRANCY_CONTRAST_FLOOR,
            "a default install running glass must auto-engage 4.5:1"
        );

        // NEGATIVE CONTROL: the pre-fix path (handing the renderer the RAW
        // `minimum_contrast_or_default`) would have shipped 1.0 contrast under
        // glass — the exact illegibility defect the guarantee excludes.
        assert!(
            default_glass.minimum_contrast_or_default() < VIBRANCY_CONTRAST_FLOOR,
            "control precondition: the raw floor is below AA"
        );
        assert_ne!(
            default_glass.effective_minimum_contrast(),
            default_glass.minimum_contrast_or_default(),
            "effective must diverge from the raw floor under glass (the fix)"
        );
    }
}

#[cfg(test)]
mod window_theme_tests {
    use super::{Config, WindowTheme};

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn window_theme_defaults_to_auto_when_absent() {
        // No key at all -> Auto (follow the OS), so a light desktop is no longer
        // forced dark.
        assert_eq!(
            Config::default().window_theme_or_default(),
            WindowTheme::Auto
        );
        assert_eq!(
            cfg("font_px = 14.0").window_theme_or_default(),
            WindowTheme::Auto
        );
    }

    #[test]
    fn window_theme_auto_light_dark_parse() {
        assert_eq!(
            cfg("window_theme = \"auto\"").window_theme_or_default(),
            WindowTheme::Auto
        );
        assert_eq!(
            cfg("window_theme = \"light\"").window_theme_or_default(),
            WindowTheme::Light
        );
        assert_eq!(
            cfg("window_theme = \"dark\"").window_theme_or_default(),
            WindowTheme::Dark
        );
    }

    #[test]
    fn window_theme_is_case_insensitive_and_trimmed() {
        assert_eq!(
            cfg("window_theme = \" Dark \"").window_theme_or_default(),
            WindowTheme::Dark
        );
        assert_eq!(
            cfg("window_theme = \"LIGHT\"").window_theme_or_default(),
            WindowTheme::Light
        );
    }

    #[test]
    fn window_theme_invalid_defaults_to_auto() {
        assert_eq!(
            cfg("window_theme = \"midnight\"").window_theme_or_default(),
            WindowTheme::Auto
        );
        assert_eq!(
            cfg("window_theme = \"\"").window_theme_or_default(),
            WindowTheme::Auto
        );
        // Direct parser: unknown -> None (caller defaults).
        assert_eq!(WindowTheme::parse("nope"), None);
        assert_eq!(WindowTheme::parse("auto"), Some(WindowTheme::Auto));
    }
}

/// THE DECORATIVE-OVERLAY DEFAULT — [`DEFAULT_DECORATIVE_EFFECTS`] — and the one
/// law that keeps a platform default from becoming a platform veto.
#[cfg(test)]
mod decorative_effect_default_tests {
    use super::{Config, DEFAULT_DECORATIVE_EFFECTS};

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    /// THE MINIMAL-FAST DEFAULT: with a FRESH, EMPTY config a Windows terminal puts
    /// no party trim on the provisioning card, so the card's own "Installing the
    /// ALab toolchain" line and its byte counter are the only things in that band.
    ///
    /// Asserted against `cfg!` (the `tab_band_height` precedent) so the same test is
    /// honest on every host: elsewhere the delight stays on.
    #[test]
    fn decorative_overlays_default_off_on_windows_and_on_elsewhere() {
        let on = !cfg!(windows);
        assert_eq!(DEFAULT_DECORATIVE_EFFECTS, on);
        assert_eq!(
            Config::default().pkg_progress_effects_or_default(),
            on,
            "an empty config must not stand a cat on the provisioning toast"
        );
        // A config that merely EXISTS is still a fresh config for this key.
        assert_eq!(cfg("font_px = 14.0").pkg_progress_effects_or_default(), on);
    }

    /// …and the switch is a DEFAULT, never a veto: an explicit key wins on every
    /// platform, in both directions. A Windows user who wants the cat says so and
    /// gets it.
    #[test]
    fn explicit_decorative_keys_win_on_every_platform() {
        assert!(cfg("pkg_progress_effects = true").pkg_progress_effects_or_default());
        assert!(!cfg("pkg_progress_effects = false").pkg_progress_effects_or_default());
    }

    /// THE TRAIL MASTER JOINED THE FAMILY, and the two keys beside it did NOT.
    /// This test pins that boundary in both directions so neither half drifts.
    ///
    /// * `cursor_trail` is IN. It is the only key that seats a permanently resident
    ///   sprite `FreeZ::OverText` on live terminal output, which is the exact thing
    ///   the minimal-fast directive rules out.
    /// * `cursor_trail_bloom` is OUT, and that is not an oversight: every consumer
    ///   reads it as `cursor_trail_or_default() && …`, so with the master off it can
    ///   emit nothing at all. Splitting an already-inert key would buy no pixel and
    ///   no millisecond, and would rob the Windows owner who writes
    ///   `cursor_trail = true` of the crown that makes the trail look shipped.
    /// * `[sparkle_words] enabled` is OUT on the merits — see
    ///   [`Config::sparkle_words_enabled_or_default`], which carries the argument
    ///   and the one honest exception to it.
    #[test]
    fn the_trail_master_is_platform_split_and_its_two_neighbours_are_not() {
        let fresh = Config::default();
        assert_eq!(
            fresh.cursor_trail_or_default(),
            DEFAULT_DECORATIVE_EFFECTS,
            "a fresh Windows config stands no resident cat on live output; \
             elsewhere the delight stays on"
        );
        // A config that merely EXISTS is still a fresh config for this key.
        assert_eq!(
            cfg("font_px = 14.0").cursor_trail_or_default(),
            DEFAULT_DECORATIVE_EFFECTS
        );
        // The two neighbours stay ON everywhere, for the reasons above.
        assert!(fresh.cursor_trail_bloom_or_default());
        assert!(fresh.sparkle_words_enabled_or_default());
        // …and each still answers to its own key, in both directions, on every
        // platform: this is a DEFAULT, never a veto.
        assert!(cfg("cursor_trail = true").cursor_trail_or_default());
        assert!(!cfg("cursor_trail = false").cursor_trail_or_default());
        assert!(!cfg("cursor_trail_bloom = false").cursor_trail_bloom_or_default());
        assert!(!cfg("[sparkle_words]\nenabled = false").sparkle_words_enabled_or_default());
    }
}

/// C3 — the tab band's height policy and the synthetic-head arithmetic that
/// realizes it. The law is pure, so the whole geometry is provable without a
/// window; the ONE host-dependent fact (the real cell height of the shipped face)
/// gets its own Windows test below.
#[cfg(test)]
mod tab_band_height_tests {
    use super::{Config, TabBandHeight, synthetic_band_head_px};

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    /// The absent-key default is PER-PLATFORM — `standard` only where the in-grid
    /// band is the window's actual tab chrome (Windows and Linux). Asserted
    /// against `cfg!` so the same test is honest on every host.
    #[test]
    fn tab_band_height_defaults_per_platform_when_absent() {
        let expected = if cfg!(any(windows, target_os = "linux")) {
            TabBandHeight::Standard
        } else {
            TabBandHeight::Compact
        };
        assert_eq!(TabBandHeight::PLATFORM_DEFAULT, expected);
        assert_eq!(Config::default().tab_band_height_or_default(), expected);
        assert_eq!(cfg("font_px = 14.0").tab_band_height_or_default(), expected);
    }

    #[test]
    fn tab_band_height_explicit_values_override_the_platform_default() {
        assert_eq!(
            cfg("tab_band_height = \"compact\"").tab_band_height_or_default(),
            TabBandHeight::Compact
        );
        assert_eq!(
            cfg("tab_band_height = \"standard\"").tab_band_height_or_default(),
            TabBandHeight::Standard
        );
        // Trimmed + case-insensitive, like every other enum key.
        assert_eq!(
            cfg("tab_band_height = \"  STANDARD \"").tab_band_height_or_default(),
            TabBandHeight::Standard
        );
    }

    #[test]
    fn tab_band_height_invalid_falls_back_to_platform_default() {
        assert_eq!(
            cfg("tab_band_height = \"huge\"").tab_band_height_or_default(),
            TabBandHeight::PLATFORM_DEFAULT
        );
        assert_eq!(
            cfg("tab_band_height = \"\"").tab_band_height_or_default(),
            TabBandHeight::PLATFORM_DEFAULT
        );
        assert_eq!(TabBandHeight::parse("nope"), None);
    }

    /// THE DEFECT THIS BUNDLE EXISTS TO AVOID: with the strip OFF there is no band,
    /// so there must be no head — otherwise the knob would push every grid down by
    /// a gutter with nothing in it. Held for BOTH targets and every scale.
    #[test]
    fn no_strip_means_no_head_at_any_target_or_scale() {
        for target in [0.0, 32.0, 40.0] {
            for scale in [1.0, 1.25, 1.5, 2.0] {
                assert_eq!(
                    synthetic_band_head_px(target, 2, 0, 21, scale),
                    0,
                    "target {target} scale {scale}"
                );
            }
        }
    }

    /// `compact` is the pre-C3 geometry, exactly: a zero target cannot produce a
    /// head no matter what the rest of the band measures.
    #[test]
    fn compact_is_byte_identical_to_the_pre_c3_geometry() {
        assert_eq!(TabBandHeight::Compact.target_logical_px(), 0.0);
        for rows in 1..=4u16 {
            for cell_h in [10, 14, 21, 28] {
                assert_eq!(
                    synthetic_band_head_px(
                        TabBandHeight::Compact.target_logical_px(),
                        2,
                        rows,
                        cell_h,
                        1.5
                    ),
                    0
                );
            }
        }
    }

    /// The whole point: `head + pad_top + strip_rows·cell_h` lands ON the target,
    /// at 96 dpi and at 150%. Asserted against the platform's own
    /// [`super::TAB_BAND_STANDARD_LOGICAL_PX`] (32 on Windows — the WinUI tab
    /// row; 36 on Linux — the libadwaita one), with a 2-3 px top pad and one
    /// strip row, the shipped shape on both.
    #[test]
    fn standard_totals_the_target_band_at_96dpi_and_150_percent() {
        let target = TabBandHeight::Standard.target_logical_px();
        assert_eq!(target, super::TAB_BAND_STANDARD_LOGICAL_PX);
        // 96 dpi: the band = pad_top + row + head = the target, exactly.
        let band = target as usize;
        let head = synthetic_band_head_px(target, 2, 1, 21, 1.0);
        assert_eq!(head, band - 2 - 21);
        assert_eq!(
            head + 2 + 21,
            band,
            "the band the viewer sees IS the target"
        );
        // 150%: the target scales with the DPI, and so does everything it is
        // measured against, so the residue is re-derived rather than scaled.
        let band = (target as f64 * 1.5).round() as usize;
        let head = synthetic_band_head_px(target, 3, 1, 28, 1.5);
        assert_eq!(head, band - 3 - 28);
        assert_eq!(head + 3 + 28, band);
    }

    /// A SUBTRACTION, not an addition: once the strip's own rows reach the target
    /// (a big font, or `tab_strip_rows = 2`), the band stops growing instead of
    /// stacking a constant on top of an already-tall row. Cell heights are
    /// derived from the platform's own target so the premise holds on every
    /// per-platform `TAB_BAND_STANDARD_LOGICAL_PX`.
    #[test]
    fn a_tall_strip_absorbs_the_target_and_the_head_collapses() {
        let target = TabBandHeight::Standard.target_logical_px();
        let absorbing = target as usize - 2; // one row + the pad reaches the target
        assert_eq!(synthetic_band_head_px(target, 2, 1, absorbing, 1.0), 0);
        assert_eq!(synthetic_band_head_px(target, 2, 1, 64, 1.0), 0);
        assert_eq!(synthetic_band_head_px(target, 2, 2, 21, 1.0), 0);
    }

    /// The backstop: even a pathological target cannot push the grid down by more
    /// than [`super::SYNTHETIC_BAND_HEAD_CELL_CAP`] cells, and a non-finite or
    /// non-positive scale is treated as 1.0 rather than producing a NaN band.
    #[test]
    fn the_head_is_capped_and_degenerate_scales_are_survivable() {
        assert_eq!(
            synthetic_band_head_px(4000.0, 0, 1, 10, 1.0),
            10 * super::SYNTHETIC_BAND_HEAD_CELL_CAP
        );
        for scale in [f64::NAN, f64::INFINITY, 0.0, -2.0] {
            assert_eq!(synthetic_band_head_px(32.0, 2, 1, 21, scale), 9);
        }
    }

    /// A NON-FINITE TARGET is degenerate too, and the doc comment promises it
    /// returns 0 — but only `scale` was covered above, so the guard that
    /// implements it for `target_logical` was untested.
    ///
    /// It matters because the natural-looking rewrite is WRONG: the guard reads
    /// `!(target > 0.0)` rather than `target <= 0.0` precisely because the
    /// latter is FALSE for NaN and would let a NaN target through into the
    /// pixel arithmetic. This pins the behaviour so the next author who tidies
    /// that comparison finds out here instead of on a user's screen.
    #[test]
    fn a_non_finite_or_non_positive_target_yields_no_head() {
        for target in [f32::NAN, f32::NEG_INFINITY, 0.0, -32.0] {
            assert_eq!(
                synthetic_band_head_px(target, 2, 1, 21, 1.0),
                0,
                "degenerate target {target} must collapse the head"
            );
        }
        // POSITIVE INFINITY is deliberately NOT in that list: `inf > 0.0` is
        // TRUE, so it passes the guard and is bounded by the cell cap like any
        // other oversized target. The doc comment promises 0 for a non-finite
        // SCALE, not for a non-finite target — writing this test is what
        // established the difference, so it is pinned here rather than left to
        // be rediscovered.
        assert_eq!(
            synthetic_band_head_px(f32::INFINITY, 0, 1, 10, 1.0),
            10 * super::SYNTHETIC_BAND_HEAD_CELL_CAP
        );
        // The control: the SAME inputs with a finite positive target do produce
        // a head, so the assertions above are not passing for some other reason.
        assert_ne!(synthetic_band_head_px(32.0, 2, 1, 21, 1.0), 0);
    }

    /// The HOST-DEPENDENT half, Windows only: against the face aterm actually
    /// ships with at the Windows `FONT_PX`, the standard band really does reach
    /// the target (or, if this host's face is unusually tall, the head correctly
    /// collapses to 0 — never a partial band that overshoots).
    #[cfg(windows)]
    #[test]
    fn the_shipped_windows_face_reaches_the_standard_band() {
        let Some(renderer) =
            aterm_render::Renderer::from_system(crate::FONT_PX, aterm_render::Theme::default())
        else {
            eprintln!("SKIP: no system monospace font");
            return;
        };
        let target = TabBandHeight::Standard.target_logical_px();
        let pad_logical = Config::default().window_padding_top_or_default();
        let mut renderer = renderer;
        // Both DPI regimes a Windows laptop actually runs at. `hidpi_target_font_px`
        // is the SAME derivation attach applies, so the 150% row is the real one.
        for scale in [1.0_f64, 1.5] {
            // `app_window::hidpi_target_font_px`'s auto-font arm, inlined: it is
            // module-private, and the derivation IS this one line.
            let px = if scale > 1.0 {
                (crate::FONT_PX * scale as f32).round()
            } else {
                crate::FONT_PX
            };
            renderer.set_px(px);
            let (_, cell_h) = renderer.cell_size();
            let pad_top = crate::logical_to_device_px(pad_logical, scale);
            let head = synthetic_band_head_px(target, pad_top, 1, cell_h, scale);
            let band = head + pad_top + cell_h;
            eprintln!(
                "C3 band at scale {scale}: font_px={px} cell_h={cell_h} pad_top={pad_top} \
                 before={} head={head} band={band}",
                pad_top + cell_h
            );
            if head > 0 {
                assert_eq!(
                    band,
                    crate::logical_to_device_px(target, scale),
                    "the band lands ON the WinUI target at scale {scale}"
                );
            } else {
                assert!(
                    band >= crate::logical_to_device_px(target, scale),
                    "a zero head is only correct when the strip already fills the band"
                );
            }
        }
    }

    /// THE LINT-GATE GUARD, and the one proof in this module that a Windows host
    /// cannot get from running the code.
    ///
    /// `App::synthetic_strip_head_px` is the ONLY non-test consumer of this entire
    /// chain — `Config::tab_band_height_or_default` → [`TabBandHeight`] (+
    /// `PLATFORM_DEFAULT`, `parse`, `target_logical_px`) → [`synthetic_band_head_px`]
    /// → `TAB_BAND_STANDARD_LOGICAL_PX`, `SYNTHETIC_BAND_HEAD_CELL_CAP`, and the
    /// `Config::tab_band_height` field itself. Gate that one function (or its one
    /// mandatory call site) behind a `#[cfg(windows)]` ATTRIBUTE and every link goes
    /// unreachable on macOS and Linux: half a dozen `dead_code` findings plus "field
    /// `tab_band_height` is never read", against a workspace gate that is
    /// `clippy --workspace --all-targets -- -D warnings`. Nothing on a Windows host
    /// can observe that — `cargo check` here is green either way — so the invariant
    /// is pinned in SOURCE, which every host can read.
    ///
    /// The fix it pins is not a suppression: `PLATFORM_DEFAULT` is `Compact` off
    /// Windows, so the law genuinely evaluates to 0 there. A runtime `if cfg!(windows)`
    /// keeps the body compiled and type-checked on every platform while still never
    /// executing off Windows — which matters, because on macOS `ws.metrics.head`
    /// holds a real MEASURED titlebar band that this law's 0 would clobber.
    #[test]
    fn the_c3_chain_keeps_a_call_site_compiled_on_every_platform() {
        let src = include_str!("app_config.rs");

        // (a) The definition itself carries no `cfg` gate. Walk back over the doc
        //     block to the first real line: an attribute, if any, lives there.
        let decl = "pub(crate) fn synthetic_strip_head_px(";
        let at = src
            .find(decl)
            .expect("synthetic_strip_head_px is defined here");
        let above = src[..at]
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("//"))
            .unwrap_or_default();
        assert!(
            !above.starts_with("#[cfg"),
            "synthetic_strip_head_px must stay COMPILED on every platform (found {above:?} \
             directly above it): it is the only non-test consumer of the tab_band_height \
             chain, so gating it out hands macOS/Linux a fan of dead_code warnings against \
             a `-D warnings` gate. Off Windows the law already returns 0 by itself \
             (PLATFORM_DEFAULT = Compact) — there is nothing to gate."
        );

        // (b) …and its ONE mandatory call site gates at RUNTIME, so the call survives
        //     into the non-Windows HIR and keeps the chain live.
        let call = "let head = self.synthetic_strip_head_px(scale, cell_h);";
        let at = src.find(call).expect("the on_resize C3 call site is here");
        let gate = src[..at]
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| l.starts_with("#[cfg") || l.starts_with("if cfg!"))
            .unwrap_or_default();
        assert_eq!(
            gate, "if cfg!(any(windows, target_os = \"linux\")) {",
            "the on_resize C3 block must gate with a runtime `cfg!` (the form the macOS \
             band block above it uses), not a `#[cfg]` attribute — an attribute deletes \
             the only live call site on the excluded platforms and takes the whole chain \
             with it"
        );
    }
}

/// THE HONESTY GAP — the deferred notice lane's App-side half. The queuing sites
/// (the backend build thread, an `AppRt` chrome call, `run()` before `App` exists)
/// cannot be driven from a unit test, but the drain they all feed can, and the
/// drain is where the interesting policy lives.
#[cfg(test)]
mod deferred_config_notice_tests {
    use crate::config_notice::{ConfigNotice, lane_test_guard, queue_deferred};

    /// A late chrome explanation must not COST the user their config warnings.
    /// The naive `self.config_notice = ConfigNotice::new(..)` (the shape every
    /// other one-off notice site uses) would silently drop the startup banner's
    /// contents to show one sentence about Mica.
    #[test]
    fn a_deferred_notice_merges_into_a_live_banner_instead_of_replacing_it() {
        let _guard = lane_test_guard();
        let _ = crate::config_notice::take_deferred();
        let mut app = crate::App::headless_for_test();
        app.config_notice = ConfigNotice::new(
            vec!["config keybindings: dropped a bad chord".to_string()],
            std::time::Instant::now(),
        );
        queue_deferred("background_material has no effect while hdr_glow is on".to_string());
        app.drain_deferred_config_notices();
        let notice = app.config_notice.as_ref().expect("banner still up");
        assert!(
            notice
                .lines
                .iter()
                .any(|l| l.contains("dropped a bad chord")),
            "the config warning survived: {:?}",
            notice.lines
        );
        assert!(
            notice.lines.iter().any(|l| l.contains("hdr_glow")),
            "and the chrome explanation joined it: {:?}",
            notice.lines
        );
    }

    /// With NO banner up (the common case — the lane fires seconds after a clean
    /// startup) the drain raises one, and an empty lane is a complete no-op so the
    /// per-park cost is a single atomic load.
    #[test]
    fn the_drain_raises_a_banner_when_none_is_up_and_no_ops_when_the_lane_is_empty() {
        let _guard = lane_test_guard();
        let _ = crate::config_notice::take_deferred();
        let mut app = crate::App::headless_for_test();
        app.config_notice = None;
        app.drain_deferred_config_notices();
        assert!(app.config_notice.is_none(), "an empty lane raises nothing");
        queue_deferred("the GPU was lost".to_string());
        app.drain_deferred_config_notices();
        let notice = app.config_notice.as_ref().expect("banner raised");
        assert!(notice.lines.iter().any(|l| l.contains("GPU was lost")));
    }
}

#[cfg(test)]
mod right_click_tests {
    use super::{Config, RightClickGesture};

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    /// The absent-key default is PER-PLATFORM: the conhost/WT gesture on Windows,
    /// off elsewhere — asserted against `cfg!` so the same test is honest on
    /// whichever platform runs it.
    #[test]
    fn right_click_defaults_per_platform_when_absent() {
        let expected = if cfg!(windows) {
            RightClickGesture::CopyPaste
        } else {
            RightClickGesture::Off
        };
        assert_eq!(RightClickGesture::PLATFORM_DEFAULT, expected);
        assert_eq!(Config::default().right_click_or_default(), expected);
        assert_eq!(cfg("font_px = 14.0").right_click_or_default(), expected);
    }

    /// Both explicit values parse, case-insensitive and trimmed, and the dash
    /// alias is accepted — so EITHER platform can opt into the other's default
    /// (the config-escape the semantics decision requires).
    #[test]
    fn right_click_explicit_values_override_the_platform_default() {
        assert_eq!(
            cfg("right_click = \"copy_paste\"").right_click_or_default(),
            RightClickGesture::CopyPaste
        );
        assert_eq!(
            cfg("right_click = \"off\"").right_click_or_default(),
            RightClickGesture::Off
        );
        assert_eq!(
            cfg("right_click = \" Copy-Paste \"").right_click_or_default(),
            RightClickGesture::CopyPaste
        );
        assert_eq!(
            cfg("right_click = \"OFF\"").right_click_or_default(),
            RightClickGesture::Off
        );
    }

    /// Unknown / empty values fall back to the platform default (warn-and-default,
    /// the `window_theme` fail-safe shape), and the direct parser returns `None`.
    #[test]
    fn right_click_invalid_falls_back_to_platform_default() {
        assert_eq!(
            cfg("right_click = \"menu\"").right_click_or_default(),
            RightClickGesture::PLATFORM_DEFAULT
        );
        assert_eq!(
            cfg("right_click = \"\"").right_click_or_default(),
            RightClickGesture::PLATFORM_DEFAULT
        );
        assert_eq!(RightClickGesture::parse("nope"), None);
        assert_eq!(
            RightClickGesture::parse("copy_paste"),
            Some(RightClickGesture::CopyPaste)
        );
    }
}

/// C5 REMEDIATION — the tab-menu chord's ESCAPE HATCH. The chord is not in the
/// rebindable `[keybindings]` table by design, so this knob is the ONLY way a
/// user gives the keys back; it has to parse, default, and fail safe.
#[cfg(test)]
mod tab_menu_chord_tests {
    use super::{Config, TabMenuChord};

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    /// Absent ⇒ `On` — an unedited config keeps both Windows spellings, which
    /// is what shipped.
    #[test]
    fn absent_claims_both_spellings() {
        assert_eq!(
            Config::default().tab_menu_chord_or_default(),
            TabMenuChord::On
        );
        assert_eq!(
            cfg("font_px = 14.0").tab_menu_chord_or_default(),
            TabMenuChord::On
        );
        assert!(TabMenuChord::On.claims_menu_key());
        assert!(TabMenuChord::On.claims_shift_f10());
    }

    /// The point of the middle value: hand ⇧F10 back (a real legacy-encodable
    /// chord) while keeping the Menu key (which no legacy mode reports at all).
    #[test]
    fn menu_key_surrenders_shift_f10_and_off_surrenders_both() {
        let only = cfg("tab_menu_chord = \"menu_key\"").tab_menu_chord_or_default();
        assert_eq!(only, TabMenuChord::MenuKey);
        assert!(only.claims_menu_key());
        assert!(
            !only.claims_shift_f10(),
            "⇧F10 goes back to the application"
        );

        let off = cfg("tab_menu_chord = \"off\"").tab_menu_chord_or_default();
        assert_eq!(off, TabMenuChord::Off);
        assert!(!off.claims_menu_key());
        assert!(!off.claims_shift_f10());
    }

    /// Case-insensitive, trimmed, dash alias — and an unknown value WARNS AND
    /// DEFAULTS rather than failing the config or silently disabling the menu.
    #[test]
    fn parsing_is_forgiving_and_invalid_falls_back_to_on() {
        assert_eq!(
            cfg("tab_menu_chord = \" Menu-Key \"").tab_menu_chord_or_default(),
            TabMenuChord::MenuKey
        );
        assert_eq!(
            cfg("tab_menu_chord = \"BOTH\"").tab_menu_chord_or_default(),
            TabMenuChord::On
        );
        assert_eq!(
            cfg("tab_menu_chord = \"nope\"").tab_menu_chord_or_default(),
            TabMenuChord::On
        );
        assert_eq!(
            cfg("tab_menu_chord = \"\"").tab_menu_chord_or_default(),
            TabMenuChord::On
        );
        assert_eq!(TabMenuChord::parse("nope"), None);
        assert_eq!(TabMenuChord::parse("off"), Some(TabMenuChord::Off));
    }
}

#[cfg(test)]
mod window_colorspace_tests {
    use super::{Config, WindowColorspace};

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    /// Absent key -> `Srgb` (the colour-managed default): existing configs get
    /// the M3 fix without editing anything.
    #[test]
    fn window_colorspace_defaults_to_srgb_when_absent() {
        assert_eq!(
            Config::default().window_colorspace_or_default(),
            WindowColorspace::Srgb
        );
        assert_eq!(
            cfg("font_px = 14.0").window_colorspace_or_default(),
            WindowColorspace::Srgb
        );
    }

    /// The full accepted token set, case-insensitive + trimmed, including the
    /// `display-p3` aliases. Everything else falls back to `Srgb` with a warning
    /// (the `window_theme` fail-safe shape).
    #[test]
    fn window_colorspace_parse_tokens_and_fallback() {
        assert_eq!(
            cfg("window_colorspace = \"srgb\"").window_colorspace_or_default(),
            WindowColorspace::Srgb
        );
        assert_eq!(
            cfg("window_colorspace = \"display-p3\"").window_colorspace_or_default(),
            WindowColorspace::DisplayP3
        );
        assert_eq!(
            cfg("window_colorspace = \" DisplayP3 \"").window_colorspace_or_default(),
            WindowColorspace::DisplayP3
        );
        assert_eq!(
            cfg("window_colorspace = \"P3\"").window_colorspace_or_default(),
            WindowColorspace::DisplayP3
        );
        // Unknown / empty -> Srgb (never a panic, never P3 by accident).
        assert_eq!(
            cfg("window_colorspace = \"rec2020\"").window_colorspace_or_default(),
            WindowColorspace::Srgb
        );
        assert_eq!(
            cfg("window_colorspace = \"\"").window_colorspace_or_default(),
            WindowColorspace::Srgb
        );
        // Direct parser: unknown -> None (caller defaults).
        assert_eq!(WindowColorspace::parse("nope"), None);
        assert_eq!(
            WindowColorspace::parse("srgb"),
            Some(WindowColorspace::Srgb)
        );
    }
}

#[cfg(test)]
mod window_padding_tests {
    use super::Config;

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    /// Unset keys resolve to EXACTLY the built-in constants (12 / 2) — the
    /// byte-identical guarantee the MetricsView proofs rely on: with no config
    /// the config-aware padding paths and the historical constant paths agree.
    #[test]
    fn window_padding_defaults_to_builtin_constants() {
        let c = Config::default();
        assert_eq!(c.window_padding_or_default(), crate::PAD_LOGICAL_PX);
        assert_eq!(c.window_padding_top_or_default(), crate::PAD_TOP_LOGICAL_PX);
    }

    /// The resolver clamps to the documented 0..=64 domain and fails non-finite
    /// values back to the default — a `nan` must never reach the
    /// `round(pad·scale)` derivation every window metric flows through.
    #[test]
    fn window_padding_clamps_and_rejects_non_finite() {
        assert_eq!(
            cfg("window_padding = 20.0").window_padding_or_default(),
            20.0
        );
        assert_eq!(
            cfg("window_padding = -3.0").window_padding_or_default(),
            0.0
        );
        assert_eq!(
            cfg("window_padding = 500.0").window_padding_or_default(),
            super::MAX_WINDOW_PADDING_PX
        );
        assert_eq!(
            cfg("window_padding = nan").window_padding_or_default(),
            crate::PAD_LOGICAL_PX
        );
    }

    /// The TOP override is clamped to `0..=window_padding` at the RESOLVER (the
    /// renderer's `pad_top <= pad` law, enforced before device-px rounding), and
    /// the 2.0 default is itself capped by a smaller base pad.
    #[test]
    fn window_padding_top_never_exceeds_base_pad() {
        assert_eq!(
            cfg("window_padding_top = 6.0").window_padding_top_or_default(),
            6.0
        );
        // Top asks for more than the base — clamped to the base.
        let c = cfg("window_padding = 4.0\nwindow_padding_top = 9.0");
        assert_eq!(c.window_padding_top_or_default(), 4.0);
        // The DEFAULT top (2.0) is capped by an even smaller configured base.
        let c = cfg("window_padding = 1.0");
        assert_eq!(c.window_padding_top_or_default(), 1.0);
        // Negative / non-finite fail safe.
        assert_eq!(
            cfg("window_padding_top = -2.0").window_padding_top_or_default(),
            0.0
        );
        assert_eq!(
            cfg("window_padding_top = inf").window_padding_top_or_default(),
            crate::PAD_TOP_LOGICAL_PX
        );
    }

    /// LIVE-APPLY, resolver level: the padding a config reload hands
    /// `refresh_all_window_metrics` flows through `MetricsView::for_scale_padded`
    /// — prove the config→device-px derivation at the scales the reload path
    /// re-resolves, including the reload dedupe premise (equal resolved padding
    /// ⇒ no re-grid work is even attempted).
    #[test]
    fn window_padding_config_drives_metrics_derivation() {
        let c = cfg("window_padding = 20.0\nwindow_padding_top = 5.0");
        let m = crate::MetricsView::for_scale_padded(
            2.0,
            c.window_padding_or_default(),
            c.window_padding_top_or_default(),
        );
        assert_eq!((m.pad, m.pad_top), (40, 10));
        // Unset config reproduces the pure default derivation exactly.
        let d = Config::default();
        assert_eq!(
            crate::MetricsView::for_scale_padded(
                2.0,
                d.window_padding_or_default(),
                d.window_padding_top_or_default(),
            ),
            crate::MetricsView::for_scale(2.0)
        );
        // The reload diff key: same resolved pair ⇒ no padding regime change.
        let same = cfg("window_padding = 12.0\nwindow_padding_top = 2.0");
        assert_eq!(
            (
                same.window_padding_or_default(),
                same.window_padding_top_or_default()
            ),
            (
                d.window_padding_or_default(),
                d.window_padding_top_or_default()
            )
        );
    }
}

#[cfg(test)]
mod headless_boot_metrics_tests {
    #[test]
    fn asymmetric_vertical_padding_round_trips_resize_with_remainder() {
        use winit::dpi::PhysicalSize;

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 0;
        app.backend.set_pad(12);
        app.backend.set_pad_top(2);
        app.backend.set_head(5);
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.metrics.pad = 12;
            ws.metrics.pad_top = 2;
            ws.metrics.head = 5;
        }
        let (cw, ch) = app.win_cell_size(wid);
        let (rows, cols) = (24usize, 80usize);
        let exact = PhysicalSize::new((cols * cw + 24) as u32, (rows * ch + 5 + 2 + 12) as u32);
        assert_eq!(app.grid_dims_for(wid, exact), (24, 80));

        let remainder = ch.saturating_sub(1);
        let with_remainder = PhysicalSize::new(exact.width, exact.height + remainder as u32);
        assert_eq!(app.grid_dims_for(wid, with_remainder), (24, 80));

        let old_symmetric_rows =
            aterm_render::pad_split((exact.height as usize).saturating_sub(5), 12, ch).cells;
        assert!(
            old_symmetric_rows < rows,
            "negative control: conserving 2*pad cannot invert the shorter visible frame"
        );
    }

    /// The frames=0 recording law, NO-`--scale` arm (4d27aae5 gated the
    /// pad/head retune on attachment; this pins its auto-font sibling): with a
    /// boot headroom band (`$ATERM_HEADROOM_PX`) applied to the shared backend
    /// at DEFAULT scale, the first redraw's `apply_window_scale` on the
    /// never-attached headless window must not re-tune the backend from the
    /// never-applied record — a video tap armed with the boot geometry must
    /// still match the first present's frame, and the band must survive for
    /// every later `image`/recording (the wipe was permanent). The empirical
    /// repro: ~25 `image` captures (which never retune) at 584×420, then the
    /// FIRST `video`'s redraw wiped head 60→0, early-stopping the take at
    /// frames=0 while retries "worked" at a band-less 584×360.
    #[test]
    fn headless_boot_band_survives_the_first_redraw_retune() {
        let mut app = crate::App::headless_for_test();
        app.font_px_explicit = false; // auto font ⇒ the W12 activate arm is live
        // The boot seam: `--scale` pad + `$ATERM_HEADROOM_PX` head applied to
        // the shared backend (the test backend boots at pad 0 / head 0), then
        // sealed into the headless window's record exactly as boot does.
        app.backend.set_pad(12);
        app.backend.set_head(60);
        app.seed_headless_boot_metrics();
        let armed = app.backend.frame_size(24, 80);
        // The recording loop's first redraw runs this retune before the present.
        app.apply_window_scale(crate::WindowId(0));
        assert_eq!(app.backend.head(), 60, "the headroom band survives");
        assert_eq!(app.backend.pad(), 12, "the boot pad survives");
        assert_eq!(
            app.backend.frame_size(24, 80),
            armed,
            "the first present's frame still matches the armed tap geometry"
        );
    }

    /// The frames=0 recording law, CONFIG-RELOAD seam: a post-boot delta reload
    /// (`settings set` carrying a REAL change) unconditionally runs
    /// `refresh_all_window_metrics`, which used to re-derive every window's head
    /// from the ATTACH-captured `head_pts` — 0 for a never-attached headless
    /// window — discarding the boot seal. The next `video`'s first redraw then
    /// re-tuned the shared backend from the wiped record (`set_head(0)`): the
    /// take died at frames=0 / resized_early_stop and the `$ATERM_HEADROOM_PX`
    /// band was permanently gone from every later `image` (584×420 → 584×360).
    /// The seal must survive the regime re-resolve.
    #[test]
    fn headless_boot_band_survives_a_delta_config_reload() {
        let mut app = crate::App::headless_for_test();
        app.font_px_explicit = false; // auto font ⇒ the W12 activate arm is live
        // The boot seam, exactly as the first-redraw test above.
        app.backend.set_pad(12);
        app.backend.set_head(60);
        app.seed_headless_boot_metrics();
        let armed = app.backend.frame_size(24, 80);
        // The reload seam: `reload_config` commits the (unchanged) font regime
        // and re-resolves every window's record before any rebuild.
        app.refresh_all_window_metrics();
        let ws = app.windows.get(&crate::WindowId(0)).expect("window 0");
        assert_eq!(ws.metrics.head, 60, "the record keeps the sealed band");
        assert_eq!(ws.metrics.pad, 12, "the record keeps the sealed pad");
        assert_eq!(
            ws.metrics.pad_top, 12,
            "the record keeps the sealed symmetric top origin"
        );
        // The recording loop's first redraw runs this retune before the present.
        app.apply_window_scale(crate::WindowId(0));
        assert_eq!(app.backend.head(), 60, "the band survives the reload");
        assert_eq!(app.backend.pad(), 12, "the boot pad survives the reload");
        assert_eq!(
            app.backend.pad_top(),
            12,
            "the top origin survives the reload"
        );
        assert_eq!(
            app.backend.frame_size(24, 80),
            armed,
            "the first present's frame still matches the armed tap geometry"
        );
    }

    /// D2: a synthetic `ScaleFactorChanged` routed to a NEVER-ATTACHED window
    /// must keep its sealed boot band — pre-fix `on_scale_factor_changed`
    /// re-derived `pad`/`head` from the incoming scale unconditionally, wiping
    /// the seal (the same seam `refresh_all_window_metrics` closed in ba9f05db).
    /// Unreachable on real winit today (it never delivers the event to a
    /// headless window), but pinned so a future synthetic scale route stays safe.
    #[test]
    fn headless_boot_band_survives_a_scale_factor_change() {
        let mut app = crate::App::headless_for_test();
        app.font_px_explicit = false; // auto font ⇒ the re-derive arm is live
        app.backend.set_pad(12);
        app.backend.set_head(60);
        app.seed_headless_boot_metrics();
        let armed = app.backend.frame_size(24, 80);

        // A scale change for the headless window must NOT touch the sealed record.
        app.on_scale_factor_changed(crate::WindowId(0), 2.0);
        let ws = app.windows.get(&crate::WindowId(0)).expect("window 0");
        assert_eq!(ws.metrics.head, 60, "the record keeps the sealed band");
        assert_eq!(ws.metrics.pad, 12, "the record keeps the sealed pad");
        assert_eq!(
            ws.metrics.pad_top, 12,
            "the record keeps the sealed top pad"
        );

        // The next redraw's retune still composes at the boot geometry.
        app.apply_window_scale(crate::WindowId(0));
        assert_eq!(app.backend.head(), 60, "the band survives the scale change");
        assert_eq!(
            app.backend.pad(),
            12,
            "the boot pad survives the scale change"
        );
        assert_eq!(
            app.backend.frame_size(24, 80),
            armed,
            "the first present's frame still matches the armed tap geometry"
        );
    }

    /// The seal copies the APPLIED boot truth verbatim into the record, so the
    /// record and the shared backend can never disagree on headless geometry
    /// (`win_pad`/`win_cell_size` and the retune guard all read this record).
    #[test]
    fn seed_copies_applied_boot_truth_into_the_record() {
        let mut app = crate::App::headless_for_test();
        app.backend.set_pad(24);
        app.backend.set_head(60);
        app.seed_headless_boot_metrics();
        let ws = app.windows.get(&crate::WindowId(0)).expect("window 0");
        assert_eq!(
            ws.metrics,
            crate::MetricsView::applied(app.font_px, 24, 24, 60),
            "the record mirrors the applied boot truth"
        );
    }
}

#[cfg(test)]
mod split_theme_tests {
    use super::Config;
    use aterm_types::Appearance;

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    /// A plain `theme = "<name>"` (no `dark:`/`light:` prefix) resolves to the SAME
    /// name for both appearances — unchanged behavior, even for a multi-word name.
    #[test]
    fn plain_theme_used_for_both_appearances() {
        let c = cfg(r#"theme = "Tokyo Night""#);
        assert_eq!(
            c.resolve_theme_name(Appearance::Dark).as_deref(),
            Some("Tokyo Night")
        );
        assert_eq!(
            c.resolve_theme_name(Appearance::Light).as_deref(),
            Some("Tokyo Night")
        );
        // …and the resolved renderer Theme is identical across appearances.
        assert_eq!(
            c.theme_for(Appearance::Dark).bg,
            c.theme_for(Appearance::Light).bg
        );
    }

    /// The split form picks the segment matching the OS appearance; the two sides
    /// resolve to DIFFERENT schemes (different rendered background).
    #[test]
    fn split_picks_matching_side() {
        let c = cfg(r#"theme = "dark:Dracula,light:GitHub Light""#);
        assert_eq!(
            c.resolve_theme_name(Appearance::Dark).as_deref(),
            Some("Dracula")
        );
        assert_eq!(
            c.resolve_theme_name(Appearance::Light).as_deref(),
            Some("GitHub Light")
        );
        // End-to-end: each side equals naming that scheme directly, and they differ.
        assert_eq!(
            c.theme_for(Appearance::Dark).bg,
            cfg(r#"theme = "Dracula""#).theme_for(Appearance::Dark).bg
        );
        assert_eq!(
            c.theme_for(Appearance::Light).bg,
            cfg(r#"theme = "GitHub Light""#)
                .theme_for(Appearance::Light)
                .bg
        );
        assert_ne!(
            c.theme_for(Appearance::Dark).bg,
            c.theme_for(Appearance::Light).bg,
            "the two sides must render different backgrounds"
        );
        // GitHub Light's background is pure white (#ffffff) on the light side.
        assert_eq!(c.theme_for(Appearance::Light).bg, 0x00FF_FFFF);
    }

    /// Keys are case/whitespace-insensitive; the theme NAME keeps its original case
    /// and surrounding spaces are trimmed.
    #[test]
    fn split_keys_case_and_whitespace_insensitive() {
        let c = cfg(r#"theme = " DARK : Solarized Dark , Light : Solarized Light ""#);
        assert_eq!(
            c.resolve_theme_name(Appearance::Dark).as_deref(),
            Some("Solarized Dark")
        );
        assert_eq!(
            c.resolve_theme_name(Appearance::Light).as_deref(),
            Some("Solarized Light")
        );
    }

    /// A split that OMITS one side resolves that appearance to `None` (built-in
    /// Default), while the present side still resolves.
    #[test]
    fn split_omitted_side_is_default() {
        let c = cfg(r#"theme = "light:GitHub Light""#);
        assert_eq!(c.resolve_theme_name(Appearance::Dark), None);
        assert_eq!(
            c.resolve_theme_name(Appearance::Light).as_deref(),
            Some("GitHub Light")
        );
        // The dark side renders the built-in Default background.
        assert_eq!(
            c.theme_for(Appearance::Dark).bg,
            aterm_types::ColorScheme::default().to_theme_parts().bg
        );
    }

    /// No `theme` key → `None` for both appearances (built-in Default everywhere).
    #[test]
    fn absent_theme_is_none() {
        let c = cfg("font_px = 14.0");
        assert_eq!(c.resolve_theme_name(Appearance::Dark), None);
        assert_eq!(c.resolve_theme_name(Appearance::Light), None);
    }

    /// The engine config (palette + default/cursor colors) also tracks the split,
    /// so a live switch recolors cells and the OSC-reset cursor, not just chrome.
    #[test]
    fn applied_terminal_config_tracks_split() {
        let c = cfg(r#"theme = "dark:Dracula,light:GitHub Light""#);
        let dark = c.applied_terminal_config_for(Appearance::Dark);
        let light = c.applied_terminal_config_for(Appearance::Light);
        assert_ne!(
            dark.default_background, light.default_background,
            "the engine default background must differ between the two sides"
        );
        assert_ne!(
            dark.cursor_color, light.cursor_color,
            "the engine cursor baseline must differ between the two sides"
        );
        assert_ne!(
            dark.selection_background, light.selection_background,
            "the engine selection baseline must differ between the two sides"
        );
        // Light side's engine default bg is GitHub Light's white.
        assert_eq!(
            light.default_background,
            aterm_types::Rgb::new(0xff, 0xff, 0xff)
        );
        assert_eq!(
            light.cursor_color,
            Some({
                let cursor = c.theme_for(Appearance::Light).cursor;
                aterm_types::Rgb::new(
                    ((cursor >> 16) & 0xff) as u8,
                    ((cursor >> 8) & 0xff) as u8,
                    (cursor & 0xff) as u8,
                )
            }),
            "GitHub Light's cursor is the engine's OSC 112 reset baseline"
        );
        let selection = c.theme_for(Appearance::Light).selection;
        assert_eq!(
            light.selection_background,
            Some(aterm_types::Rgb::new(
                ((selection >> 16) & 0xff) as u8,
                ((selection >> 8) & 0xff) as u8,
                (selection & 0xff) as u8,
            )),
            "GitHub Light's selection is the engine's OSC 117 reset baseline"
        );
    }

    #[test]
    fn applied_terminal_config_preserves_selected_text_override() {
        let c = cfg("selection_foreground = \"#123456\"");
        assert_eq!(
            c.applied_terminal_config().selection_foreground,
            Some(aterm_types::Rgb::new(0x12, 0x34, 0x56))
        );
    }
}

#[cfg(test)]
mod reload_dedupe_tests {
    use super::Config;

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    /// The prepared-admission dedupe's equality gate: two parses of the same
    /// content are EQUAL, and equality is semantic rather than byte-level, so a
    /// comment/whitespace-only edit (or a bare `touch`) also dedupes; while a
    /// real one-key edit (the rapid trail-style switch) compares UNEQUAL and
    /// still applies.
    #[test]
    fn reload_dedupe_equality_matches_parsed_content_not_bytes() {
        let a = cfg("cursor_trail = true\ncursor_trail_style = \"fire\"\n");
        let same_bytes = cfg("cursor_trail = true\ncursor_trail_style = \"fire\"\n");
        let reformatted =
            cfg("# a comment\ncursor_trail = true\n\ncursor_trail_style = \"fire\"\n");
        let edited = cfg("cursor_trail = true\ncursor_trail_style = \"rainbow kitty\"\n");
        assert!(
            a == same_bytes,
            "identical bytes parse equal (dedupe fires)"
        );
        assert!(
            a == reformatted,
            "formatting-only edits parse equal (dedupe fires)"
        );
        assert!(
            a != edited,
            "a genuine style edit parses unequal (the reload applies)"
        );
    }

    /// The path-feed fingerprints (`Config::path_feed_fingerprints`) track the
    /// CONTENT of the files the config references by path — stable while the
    /// files are untouched, moved by an edit, and split per consumer: the
    /// lexicon feeds only the `deco` stream (word decorations), a trail-pack
    /// manifest only the `trail` stream (cursor-glow registry). A file
    /// disappearing is a content change too. This is what lets the reload
    /// dedupe keep touch-to-reload alive without giving up its no-op win.
    #[test]
    fn path_feed_fingerprints_track_referenced_file_content() {
        let dir = std::env::temp_dir().join(format!("aterm-feed-fp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lex = dir.join("lexicon.toml");
        let pack = dir.join("trail-pack.toml");
        std::fs::write(&lex, "one").unwrap();
        std::fs::write(&pack, "alpha").unwrap();
        let config = cfg(&format!(
            "cursor_trail_packs = [\"{}\"]\n[sparkle_words]\nlexicon = \"{}\"\n",
            pack.display(),
            lex.display()
        ));
        let base = config.path_feed_fingerprints();
        assert_eq!(
            base,
            config.path_feed_fingerprints(),
            "untouched files fingerprint stably"
        );
        // A lexicon CONTENT edit moves only the deco stream.
        std::fs::write(&lex, "two").unwrap();
        let lexed = config.path_feed_fingerprints();
        assert_ne!(lexed.deco, base.deco, "lexicon content feeds deco");
        assert_eq!(lexed.trail, base.trail, "…and never trail");
        // A trail-manifest edit moves only the trail stream.
        std::fs::write(&pack, "beta").unwrap();
        let packed = config.path_feed_fingerprints();
        assert_eq!(packed.deco, lexed.deco, "trail content never feeds deco");
        assert_ne!(packed.trail, lexed.trail, "trail content feeds trail");
        // A file DISAPPEARING is a content change too (fail-visible, never stale).
        std::fs::remove_file(&lex).unwrap();
        assert_ne!(
            config.path_feed_fingerprints().deco,
            packed.deco,
            "a vanished lexicon file reads as changed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A path generation is one transaction per consumer: mutation after the
    /// same-handle read cannot relabel the admitted bytes with the replacement
    /// file's fingerprint. This is the ABA-resistant Tier-1 projection for the
    /// derived path-feed snapshot model.
    #[test]
    fn path_feed_generation_binds_consumer_and_fingerprint_to_one_read() {
        let dir =
            std::env::temp_dir().join(format!("aterm-feed-generation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pack = dir.join("trail-pack.toml");
        let v1 = "pack = 1\nid = \"generation-v1\"\n";
        let v2 = "pack = 1\nid = \"generation-v2\"\n";
        std::fs::write(&pack, v1).unwrap();
        let config = cfg(&format!(
            "cursor_trail_packs = [{:?}]\n[sparkle_words]\nenabled = false\n",
            pack.to_string_lossy(),
        ));
        let model = aterm_spec::derive::path_feed_snapshot_model();
        let mut model_state = model.init_state();
        let v1_fingerprint = config.path_feed_fingerprints().trail;

        let mut reads = 0;
        let (catalog, admitted_fingerprint) = config
            .resolve_trail_pack_catalog_with_fingerprint_and_reader(
                |_, admitted_path, max_bytes| {
                    reads += 1;
                    let admitted = aterm_effects::file_feed::read_bounded_regular_utf8(
                        admitted_path,
                        max_bytes,
                    );
                    // The pathname changes after the real bounded read but before
                    // this sole injected reader returns. Consumer parsing and
                    // fingerprinting must both stay on the returned v1 String.
                    std::fs::write(&pack, v2).unwrap();
                    admitted
                },
            );
        for action in ["Read", "LiveMutate", "Publish"] {
            assert!(
                model.fire(action, &mut model_state),
                "{action}: {model_state:?}"
            );
        }
        assert!(model.check_invariant("PublishedPairComesFromAdmittedRead", &model_state));
        assert_eq!(model_state["prepared"], 1);
        assert_eq!(model_state["fingerprint"], 1);
        assert_eq!(model_state["live"], 2);
        assert_eq!(reads, 1, "the configured path is opened exactly once");
        assert!(catalog.packs.contains_key("generation-v1"));
        assert!(!catalog.packs.contains_key("generation-v2"));
        assert_eq!(
            admitted_fingerprint, v1_fingerprint,
            "Tier-1 projection: prepared=1 and fingerprint=1 derive from the admitted v1 String"
        );
        assert_ne!(
            admitted_fingerprint,
            config.path_feed_fingerprints().trail,
            "Tier-1 negative control: the v1 consumer never carries the live v2 fingerprint"
        );

        let replacement = config.prepare_path_feed_generation();
        assert!(replacement.trail_packs.packs.contains_key("generation-v2"));
        assert!(!replacement.trail_packs.packs.contains_key("generation-v1"));
        assert_eq!(
            replacement.fingerprints,
            config.path_feed_fingerprints(),
            "the next generation admits and identifies the replacement bytes"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn disabled_sparkle_performs_no_path_feed_reads() {
        let dir = std::env::temp_dir().join(format!("aterm-disabled-feed-{}", std::process::id()));
        let lexicon = dir.join("lexicon.toml");
        let toy = dir.join("toy.toml");
        let disabled = cfg(&format!(
            "[sparkle_words]\nenabled = false\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
            lexicon.to_string_lossy(),
            toy.to_string_lossy(),
        ));

        super::reset_path_feed_read_count();
        let prepared = disabled.prepare_path_feed_generation();
        assert_eq!(super::path_feed_read_count(), 0);
        assert_eq!(prepared.sparkle.consumer_capabilities(), Default::default());

        let enabled = cfg(&format!(
            "[sparkle_words]\nenabled = true\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
            lexicon.to_string_lossy(),
            toy.to_string_lossy(),
        ));
        super::reset_path_feed_read_count();
        let _ = enabled.prepare_path_feed_generation();
        assert_eq!(
            super::path_feed_read_count(),
            2,
            "negative control: enabling the same two configured feeds admits each once"
        );
    }

    #[test]
    fn preliminary_startup_catalog_defers_trail_feed_to_exact_generation() {
        let dir =
            std::env::temp_dir().join(format!("aterm-preliminary-feed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let trail = dir.join("trail.toml");
        std::fs::write(&trail, "pack = 1\nid = \"deferred-trail\"\n").unwrap();
        let config = cfg(&format!(
            "cursor_trail_packs = [{:?}]\n[sparkle_words]\nenabled = false\n",
            trail.to_string_lossy(),
        ));

        super::reset_path_feed_read_count();
        let preliminary =
            config.resolve_preliminary_asset_catalog_with_themes(super::ThemeCatalog::empty());
        assert_eq!(
            super::path_feed_read_count(),
            0,
            "serialized startup catalog must not admit Trail/Sparkle feeds"
        );
        assert!(preliminary.trail_packs.ids.is_empty());

        let exact = config.prepare_path_feed_generation();
        assert_eq!(
            super::path_feed_read_count(),
            1,
            "parallel exact generation admits the one configured trail once"
        );
        assert_eq!(exact.trail_packs.ids, ["deferred-trail"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_feed_fingerprints_enforce_the_loader_specific_caps() {
        assert_eq!(super::MAX_PATH_FEED_OPEN_ATTEMPTS, 17);
        assert_eq!(super::MAX_PATH_FEED_FINGERPRINT_READ_BYTES, 2_883_601);
        let dir = std::env::temp_dir().join(format!("aterm-feed-caps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lexicon = dir.join("lexicon.toml");
        let toy = dir.join("toy.toml");
        let trail = dir.join("trail.toml");
        let config = cfg(&format!(
            "cursor_trail_packs = [{:?}]\n[sparkle_words]\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
            trail.to_string_lossy(),
            lexicon.to_string_lossy(),
            toy.to_string_lossy(),
        ));
        let missing = config.path_feed_fingerprints();

        std::fs::write(
            &lexicon,
            vec![b'x'; aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES + 1],
        )
        .unwrap();
        std::fs::write(
            &toy,
            vec![b'x'; aterm_effects::spec::MAX_TOY_PACK_BYTES + 1],
        )
        .unwrap();
        std::fs::write(
            &trail,
            vec![b'x'; aterm_effects::trail_pack::MAX_TRAIL_PACK_BYTES + 1],
        )
        .unwrap();
        assert_eq!(
            config.path_feed_fingerprints(),
            missing,
            "oversized inputs are rejected exactly like the loaders, not hashed past their caps"
        );

        std::fs::write(&lexicon, "# admitted lexicon\n").unwrap();
        std::fs::write(&toy, "# admitted toy manifest bytes\n").unwrap();
        std::fs::write(&trail, "# admitted trail manifest bytes\n").unwrap();
        let admitted = config.path_feed_fingerprints();
        assert_ne!(admitted.deco, missing.deco);
        assert_ne!(admitted.trail, missing.trail);
        assert!(
            aterm_effects::file_feed::read_bounded_regular_utf8(
                &lexicon,
                aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES,
            )
            .is_ok(),
            "regular lexicon negative control is admitted"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn path_feed_fingerprint_and_loaders_refuse_a_writerless_fifo() {
        use std::os::unix::ffi::OsStrExt as _;

        let path = std::env::temp_dir().join(format!("aterm-feed-fifo-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("FIFO path");
        // SAFETY: `path_c` is a live NUL-terminated pathname and mkfifo retains
        // no pointer. The path is unique to this test process.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        let config = cfg(&format!(
            "cursor_trail_packs = [{0:?}]\n[sparkle_words]\nlexicon = {0:?}\ntoy_packs = [{0:?}]\n",
            path.to_string_lossy(),
        ));
        let fifo_fingerprint = config.path_feed_fingerprints();
        assert!(
            config.resolve_trail_pack_catalog().packs.is_empty(),
            "Trail Pack loader returns without waiting for a FIFO writer"
        );
        let _ = config.sparkle_runtime_parts();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            config.path_feed_fingerprints(),
            fifo_fingerprint,
            "a FIFO and a missing file are both rejected admissions"
        );
    }

    /// The BYTE-EQUAL reload path (`refresh_path_feeds`) — the touch-to-reload
    /// regression proof: (1) a repeated exact observation with NOTHING changed anywhere
    /// stays a full no-op, including no word-deco reset (the dedupe's original
    /// win); (2) an edit to the lexicon FILE alone — config bytes identical —
    /// re-arms the pre-frame recompute AND hard-resets the per-window word
    /// decorations, so the stale lexicon can't survive on a byte-idle grid;
    /// (3) a trail-pack manifest edit re-arms the recompute (registry rebuild)
    /// WITHOUT touching the decorations it never feeds.
    #[test]
    fn byte_equal_reload_still_rereads_touched_path_feed_files() {
        let dir = std::env::temp_dir().join(format!("aterm-feed-touch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lex = dir.join("lexicon.toml");
        let pack = dir.join("trail-pack.toml");
        std::fs::write(&lex, "# lexicon v1\n").unwrap();
        std::fs::write(&pack, "# pack v1\n").unwrap();

        let mut app = crate::App::headless_for_test();
        app.config = cfg(&format!(
            "cursor_trail_packs = [\"{}\"]\n[sparkle_words]\nlexicon = \"{}\"\n",
            pack.display(),
            lex.display()
        ));
        // The startup/worker step: compile feeds and carry their fingerprints
        // in the same immutable generation; render only activates that bundle.
        app.prepared_sparkle = app.config.prepare_sparkle_runtime();
        app.path_feed_fps = app.config.path_feed_fingerprints();
        app.recompute_sparkle();
        assert!(!app.sparkle_dirty, "recompute clears the dirty latch");

        // Seed a SCANNED per-window state so a hard_reset is observable:
        // after an (empty) rescan at epoch 7, `needs_rescan(7)` is false and
        // only a reset can flip it back.
        let (deco_cfg, lexicon) = {
            let r = app.sparkle.as_ref().expect("sparkle resolves on defaults");
            (r.cfg.clone(), r.lexicon.clone())
        };
        let wid = crate::WindowId(0);
        let seed_scan = |app: &mut crate::App| {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.word_decos.rescan_from_cells(
                &[],
                &[],
                0,
                0,
                &lexicon,
                &deco_cfg,
                7,
                std::time::Instant::now(),
            );
            assert!(
                !ws.word_decos.needs_rescan(7),
                "probe seeded: scanned at epoch 7"
            );
        };
        seed_scan(&mut app);

        // (1) Byte-equal config + untouched files ⇒ the FULL no-op survives.
        let fresh = app.config.path_feed_fingerprints();
        app.refresh_path_feeds(fresh);
        assert!(!app.sparkle_dirty, "nothing changed ⇒ no recompute re-arm");
        assert!(
            !app.windows[&wid].word_decos.needs_rescan(7),
            "…and no word-deco reset (the settings-commit + watcher dedupe win)"
        );

        // (2) Lexicon FILE edit, config bytes identical ⇒ re-read + hard reset.
        std::fs::write(&lex, "# lexicon v2\n").unwrap();
        let fresh = app.config.path_feed_fingerprints();
        app.refresh_path_feeds(fresh);
        assert!(
            app.sparkle_dirty,
            "lexicon content change re-arms the recompute"
        );
        assert!(
            app.windows[&wid].word_decos.needs_rescan(7),
            "changed lexicon hard-resets the per-window decorations"
        );

        // (3) Trail-manifest edit ⇒ registry re-read WITHOUT a deco reset.
        app.prepared_sparkle = app.config.prepare_sparkle_runtime();
        app.recompute_sparkle(); // memory-only activation of (2)'s worker bundle
        seed_scan(&mut app);
        std::fs::write(&pack, "# pack v2\n").unwrap();
        let fresh = app.config.path_feed_fingerprints();
        app.refresh_path_feeds(fresh);
        assert!(
            app.sparkle_dirty,
            "trail manifest change re-arms the recompute"
        );
        assert!(
            !app.windows[&wid].word_decos.needs_rescan(7),
            "a trail-only edit never resets word decorations"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The CHANGED-config path's reset gate (`deco_feed_changed`): a lexicon
    /// FILE edit hiding behind an unrelated config edit still hard-resets —
    /// config-struct equality alone cannot see it (the `[sparkle_words]` table
    /// stores only the PATH), and without the fingerprint term the rebuilt
    /// App lexicon would never reach windows idle on a byte-stable grid.
    #[test]
    fn deco_feed_gate_sees_lexicon_content_behind_an_unrelated_edit() {
        let old = cfg("font_px = 12.0\n");
        let new = cfg("font_px = 14.0\n");
        // Unrelated edit, feeds untouched ⇒ decorations survive mid-animation.
        assert!(
            !super::deco_feed_changed(&old, &new, 7, 7),
            "an unrelated edit with unchanged feeds must not wipe decorations"
        );
        // Same unrelated edit, but the lexicon FILE content drifted ⇒ reset.
        assert!(
            super::deco_feed_changed(&old, &new, 8, 7),
            "a lexicon-file edit behind an unrelated config edit must reset"
        );
        // The struct-diff arms still fire on their own (table / theme edits).
        let sparkled = cfg("font_px = 12.0\n[sparkle_words]\nenabled = false\n");
        assert!(
            super::deco_feed_changed(&old, &sparkled, 7, 7),
            "a [sparkle_words] table edit resets as before"
        );
    }

    /// The popup-anchor gate (`popup_anchors_drifted`): a VALUE-only edit
    /// keeps the Settings field catalogue's key sequence identical, so an open
    /// style popup's anchor row cannot drift and the reload may leave it open —
    /// rapid sequential style picks stay one fluid gesture.
    #[test]
    fn value_only_edit_keeps_the_editable_field_catalogue_stable() {
        let a = cfg("cursor_trail_style = \"fire\"\n");
        let b = cfg("cursor_trail_style = \"rainbow kitty\"\n");
        let fa = crate::prefs::editable_fields(&a);
        let fb = crate::prefs::editable_fields(&b);
        assert_eq!(fa.len(), fb.len(), "the catalogue keeps its size");
        assert!(
            fa.iter().zip(&fb).all(|(x, y)| x.key == y.key),
            "the catalogue keeps its key sequence (no anchor drift)"
        );
        assert!(
            a != b,
            "the styles differ, so the reload itself still applies"
        );
    }
}

#[cfg(test)]
mod whole_cell_min_size_tests {
    //! fixwave5: winit's Wayland backend snaps interactive resizes to
    //! `min_inner_size + k·increment`, so the min must sit ON the whole-cell
    //! frame lattice or an exact-fit width gets re-snapped below the fit on a
    //! pure-vertical edge drag (one column shaved).

    #[test]
    fn min_size_sits_on_the_whole_cell_frame_lattice() {
        let app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let min = app.whole_cell_min_size(wid);
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        let base_h =
            app.win_head(wid) + app.win_pad_top(wid) + pad + usize::from(app.tab_strip_rows) * ch;

        // Congruence: every exact whole-cell frame `2·pad + C·cw` at or above
        // the min is a lattice point of `min + k·cw` — the property that keeps
        // a vertical drag from re-snapping the width below the exact fit.
        assert_eq!(
            (min.width as usize - 2 * pad) % cw,
            0,
            "min width must be an exact whole-cell frame"
        );
        assert_eq!(
            (min.height as usize - base_h) % ch,
            0,
            "min height must be an exact whole-cell frame"
        );
        // The historical UX floor still holds (headless scale is 1.0).
        assert!(min.width >= 164, "min width keeps the 164 logical floor");
        assert!(min.height >= 98, "min height keeps the 98 logical floor");
        // And the min stays within one cell of the floor (no runaway growth).
        assert!((min.width as usize) < 164 + cw + 2 * pad + cw);
        assert!((min.height as usize) < 98 + ch + base_h + ch);
    }
}

#[cfg(test)]
mod theme_live_tests {
    /// `apply_theme_live` recolours the LIVE backend in place — the next frame
    /// paints the new theme without a backend rebuild — and drops the caches that
    /// would otherwise stale-serve the old colours on a byte-idle grid (the E3
    /// strip-row cache and the per-window `RepaintKey` gate).
    #[test]
    fn apply_theme_live_recolours_backend_and_drops_caches() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);

        // A 2×2 idle grid, cursor hidden: every pixel renders the theme bg.
        let mut term = aterm_core::terminal::Terminal::new(2, 2);
        term.process(b"\x1b[?25l");
        let input = term.cell_frame(2, 2);
        let render = |backend: &mut crate::Backend| match backend {
            crate::Backend::Cpu(r) => r.render_input(&input),
            crate::Backend::Gpu(_) => unreachable!("headless test backend is CPU"),
        };
        let before = render(app.backend.ready_mut());

        // Seed the strip cache so its invalidation is observable (non-vacuous).
        app.windows.get_mut(&wid).unwrap().last_strip_fp =
            Some((0xFEED, 80, false, None, false, None));

        let tp = aterm_types::scheme::builtin("Dracula")
            .unwrap()
            .to_theme_parts();
        let new_theme = aterm_render::Theme {
            fg: tp.fg,
            bg: tp.bg,
            cursor: tp.cursor,
            selection: tp.selection,
        };
        assert_ne!(
            app.theme.bg, new_theme.bg,
            "test needs a real colour change"
        );

        app.apply_theme_live(new_theme);

        // App state carries the new theme for any LATER rebuild/zoom to bake in.
        assert_eq!(app.theme.bg, new_theme.bg);
        assert_eq!(app.theme.fg, new_theme.fg);
        // Present caches dropped: strip rows retint, RepaintKey gate repaints.
        assert!(app.windows[&wid].last_strip_fp.is_none(), "strip cache");
        assert!(app.windows[&wid].last_present.is_none(), "RepaintKey gate");
        // The LIVE backend paints the NEW theme, byte-identical to a renderer
        // BUILT with it — proof the swap needs no rebuild.
        let got = render(app.backend.ready_mut());
        assert_ne!(got.pixels, before.pixels, "the idle grid recoloured");
        let mut fresh = aterm_render::Renderer::from_system(crate::FONT_PX, new_theme)
            .expect("system font for test renderer");
        assert_eq!(got.pixels, fresh.render_input(&input).pixels);
    }
}

#[cfg(test)]
mod matrix_rain_cfg_tests {
    //! PHOSPHOR `[matrix_rain]` resolver pins (design §12): defaults + clamps
    //! live ONLY in `Config::matrix_rain_params`, the durable enabled bit
    //! resolves separately (`Config::matrix_rain_enabled`, default OFF —
    //! inverted vs sparkle, required by the zero-cost pins), and malformed
    //! hue input fails CLOSED without aborting the config load.

    use super::Config;
    use crate::matrix_rain::{RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR, RainConfig, RainHue};

    /// Stock dark-theme chrome, as the renderer knows it (`0x00RR_GGBB`).
    const BG: u32 = 0x0011_1318;
    const FG: u32 = 0x00D0_D0D0;

    /// The enabled-gated composition the pre-split resolver used to return —
    /// the parameter pins below read through it so they also witness that the
    /// split (`matrix_rain_enabled` + `matrix_rain_params`) still composes to
    /// the old contract.
    fn rain(toml: &str) -> Option<RainConfig> {
        let cfg = toml::from_str::<Config>(toml).expect("valid toml");
        cfg.matrix_rain_enabled()
            .then(|| cfg.matrix_rain_params(BG, FG))
    }

    /// OFF BY DEFAULT (the zero-cost posture): an absent table, a table with
    /// no `enabled` key, and an explicit `enabled = false` all resolve the
    /// enabled bit FALSE — no engine is ever constructed on any of these
    /// paths without a session override.
    #[test]
    fn rain_is_off_by_default() {
        assert!(
            !Config::default().matrix_rain_enabled(),
            "absent [matrix_rain] table ⇒ off"
        );
        assert!(
            rain("[matrix_rain]\nfps = 42").is_none(),
            "absent `enabled` key ⇒ off, even with other knobs set"
        );
        assert!(
            rain("[matrix_rain]\nenabled = false\nfps = 42").is_none(),
            "explicit enabled = false ⇒ off"
        );
    }

    /// The parameter resolve is INDEPENDENT of the enabled bit (the session-
    /// override contract): an absent/disabled table still synthesizes the full
    /// §12 default set with the live theme, identical to the enabled resolve —
    /// so a session forcing rain ON over a disabled config gets the exact same
    /// field it would get from `enabled = true`.
    #[test]
    fn params_resolve_independent_of_enabled() {
        let disabled = toml::from_str::<Config>("[matrix_rain]\nenabled = false\nfps = 24")
            .unwrap()
            .matrix_rain_params(BG, FG);
        let enabled = toml::from_str::<Config>("[matrix_rain]\nenabled = true\nfps = 24")
            .unwrap()
            .matrix_rain_params(BG, FG);
        assert_eq!(disabled, enabled, "the enabled bit never shapes the params");
        assert_eq!(disabled.fps, 24, "knobs still resolve while disabled");
        let absent = Config::default().matrix_rain_params(BG, FG);
        assert_eq!(absent.fps, 30, "absent table synthesizes the §12 defaults");
        assert_eq!(absent.default_bg, BG);
        assert_eq!(absent.theme_fg, FG);
        assert!(
            absent.enabled,
            "params always carry enabled: true (host gates)"
        );
    }

    /// The `[packages]` loop gate: an absent table (and an empty one) is exactly
    /// today's behavior — the background tools loop runs; `enabled = false` OR
    /// `auto_update = false` each stop it; and the whole table (including the
    /// atpkg-consumed keys and the `[packages.links]` sub-table this struct
    /// deliberately leaves to atpkg's own reader) round-trips through serde.
    #[test]
    fn packages_loop_gate_resolves_defaults_and_flags() {
        assert!(
            Config::default().packages_update_loop_enabled(),
            "absent [packages] table ⇒ the loop runs (pre-config behavior)"
        );
        let empty = toml::from_str::<Config>("[packages]").unwrap();
        assert!(empty.packages_update_loop_enabled());
        let off = toml::from_str::<Config>("[packages]\nenabled = false").unwrap();
        assert!(!off.packages_update_loop_enabled(), "master off ⇒ no loop");
        let no_auto = toml::from_str::<Config>("[packages]\nauto_update = false").unwrap();
        assert!(
            !no_auto.packages_update_loop_enabled(),
            "auto_update off ⇒ no loop (enabled alone is not enough)"
        );
        let full = toml::from_str::<Config>(concat!(
            "[packages]\nenabled = true\nauto_update = true\nauto_install = true\n",
            "account = \"alabsystems\"\nchannel = \"stable\"\n",
            "include = [\"ay\"]\nexclude = [\"trust\"]\n",
            "[packages.links]\nay = \"~/ay\"\norc = \"alabsystems/orc\"\n"
        ))
        .unwrap();
        assert!(full.packages_update_loop_enabled());
        let p = full.packages.as_ref().expect("table parsed");
        assert_eq!(
            p.auto_install,
            Some(true),
            "the consent flag parses (atpkg reads it)"
        );
        assert_eq!(p.account.as_deref(), Some("alabsystems"));
        assert_eq!(p.channel.as_deref(), Some("stable"));
        assert_eq!(p.include.as_deref(), Some(["ay".to_string()].as_slice()));
        assert_eq!(p.exclude.as_deref(), Some(["trust".to_string()].as_slice()));
    }

    /// TRUTH-TRIANGLE cross-pin for the shared `[packages]` keys: the two
    /// independent readers of the ONE table — this GUI `Config` (what the
    /// Settings switches and the status card display) and the co-located
    /// atpkg's own `PackagesConfig` (what the update pass actually does) —
    /// must resolve identical defaults AND identical explicit values. A
    /// default flipped on one side only (e.g. atpkg's `auto_install` to true)
    /// fails HERE instead of shipping a switch that lies about the loop.
    #[test]
    fn packages_defaults_agree_across_the_two_config_readers() {
        // Defaults: absent table on both sides.
        let gui = Config::default();
        let pkg = atpkg::PackagesConfig::default();
        assert_eq!(
            gui.packages_auto_install(),
            pkg.auto_install(),
            "auto_install default must agree (consent gate)"
        );
        assert!(!pkg.auto_install(), "consent defaults OFF everywhere");
        assert!(
            gui.packages_auto_update() && gui.packages_update_loop_enabled(),
            "the loop defaults ON (pre-config behavior; atpkg leaves this gate to the GUI)"
        );
        assert_eq!(pkg.channel(), "stable", "atpkg's channel default");
        // Explicit values: the SAME text resolves identically through both parsers.
        let text = "[packages]\nauto_update = false\nauto_install = true\n";
        let gui = toml::from_str::<Config>(text).unwrap();
        let pkg = atpkg::config::parse_packages(text);
        assert_eq!(gui.packages_auto_install(), pkg.auto_install());
        assert!(
            gui.packages_auto_install(),
            "explicit consent lands on both"
        );
        assert_eq!(
            gui.packages.as_ref().and_then(|p| p.auto_update),
            pkg.auto_update,
            "auto_update raw value agrees (atpkg carries it; the GUI gates on it)"
        );
        assert!(!gui.packages_auto_update());
    }

    /// The §12 default column resolves exactly; alpha/head_alpha stay `None`
    /// (theme-derived in-engine) when the user never set them, and the live
    /// theme lands as the renderer knows it.
    #[test]
    fn defaults_resolve_per_design_table() {
        let c = rain("[matrix_rain]\nenabled = true").expect("enabled resolves");
        assert!(c.enabled);
        assert_eq!(c.fps, 30);
        assert_eq!(c.density, 6);
        assert_eq!(c.speed, 5);
        assert_eq!(c.trail, 5);
        assert_eq!(c.alpha_override, None, "absent alpha stays theme-derived");
        assert_eq!(c.head_alpha_override, None, "absent head_alpha derived too");
        assert_eq!(c.hue, RainHue::Matrix);
        assert_eq!(c.mutation_ms, 133);
        assert_eq!(c.idle_secs, 8);
        assert!(
            !c.suppress_in_alt_screen,
            "§7: fullscreen TUIs rain by default"
        );
        assert!(c.turn_wave);
        assert!(c.bell_alert);
        assert_eq!(c.seed, 0, "0 = derive-per-window sentinel");
        assert_eq!(c.default_bg, BG);
        assert_eq!(c.theme_fg, FG);
    }

    /// Every clamped knob pins at BOTH edges (design §12 clamp column).
    #[test]
    fn clamps_hold_at_both_edges() {
        let lo = rain(concat!(
            "[matrix_rain]\nenabled = true\n",
            "fps = 1\ndensity = 0\nspeed = 0\ntrail = 0\n",
            "alpha = 0\nhead_alpha = 0\nmutation_ms = 1\nidle_secs = 0\n"
        ))
        .unwrap();
        assert_eq!(lo.fps, 12);
        assert_eq!(lo.density, 1);
        assert_eq!(lo.speed, 1);
        assert_eq!(lo.trail, 1);
        assert_eq!(lo.alpha_override, Some(RAIN_ALPHA_FLOOR));
        assert_eq!(
            lo.head_alpha_override,
            Some(RAIN_ALPHA_FLOOR),
            "user-set head alpha floors at the resolved body alpha"
        );
        assert_eq!(lo.mutation_ms, 80);
        assert_eq!(lo.idle_secs, 2);

        let hi = rain(concat!(
            "[matrix_rain]\nenabled = true\n",
            "fps = 240\ndensity = 99\nspeed = 99\ntrail = 99\n",
            "alpha = 255\nhead_alpha = 255\nmutation_ms = 100000\nidle_secs = 100000\n"
        ))
        .unwrap();
        assert_eq!(hi.fps, 60);
        assert_eq!(hi.density, 12);
        assert_eq!(hi.speed, 10);
        assert_eq!(hi.trail, 10);
        assert_eq!(hi.alpha_override, Some(RAIN_ALPHA_CAP), "READABLE ceiling");
        assert_eq!(hi.head_alpha_override, Some(RAIN_ALPHA_CAP));
        assert_eq!(hi.mutation_ms, 2000);
        assert_eq!(hi.idle_secs, 120);
    }

    /// `head_alpha` clamps `alpha..=135`: heads are never dimmer than the
    /// body the user configured; with no body override the floor is the
    /// global alpha floor.
    #[test]
    fn head_alpha_floors_at_body_alpha() {
        let c = rain("[matrix_rain]\nenabled = true\nalpha = 120\nhead_alpha = 20").unwrap();
        assert_eq!(c.alpha_override, Some(120));
        assert_eq!(c.head_alpha_override, Some(120));
        let c = rain("[matrix_rain]\nenabled = true\nhead_alpha = 5").unwrap();
        assert_eq!(c.alpha_override, None);
        assert_eq!(c.head_alpha_override, Some(RAIN_ALPHA_FLOOR));
    }

    /// The three documented hue spellings parse; malformed hex (wrong length,
    /// non-hex digits, unknown words) fails CLOSED to the stock matrix green
    /// — never `None`-ing the resolve and never aborting the config load.
    #[test]
    fn hue_parses_and_bad_hex_fails_closed() {
        let hue = |h: &str| {
            rain(&format!("[matrix_rain]\nenabled = true\nhue = \"{h}\""))
                .expect("hue never aborts the resolve")
                .hue
        };
        assert_eq!(hue("matrix"), RainHue::Matrix);
        assert_eq!(hue("theme"), RainHue::Theme);
        assert_eq!(hue("THEME"), RainHue::Theme, "case-insensitive");
        assert_eq!(hue("#28D75F"), RainHue::Custom(0x0028_D75F));
        assert_eq!(
            hue("#a0b1c2"),
            RainHue::Custom(0x00A0_B1C2),
            "lowercase hex"
        );
        for bad in ["#28D75", "#28D75F0", "#GGGGGG", "chartreuse", "", "#"] {
            assert_eq!(hue(bad), RainHue::Matrix, "{bad:?} must fail closed");
        }
    }

    /// `suppress_in_alt_screen` reaches the engine config verbatim (the §7
    /// shared-gate knob).
    #[test]
    fn suppress_in_alt_screen_reaches_engine_config() {
        assert!(
            rain("[matrix_rain]\nenabled = true\nsuppress_in_alt_screen = true")
                .unwrap()
                .suppress_in_alt_screen
        );
        assert!(
            !rain("[matrix_rain]\nenabled = true")
                .unwrap()
                .suppress_in_alt_screen
        );
    }

    /// The v1-inert knobs (`materialize` / `ink_text` / `phosphor`) PARSE —
    /// setting them must never abort the config load — but are not forwarded:
    /// `RainConfig` carries no such fields (compile-time), and the resolve
    /// still succeeds normally with all three set (deferred, design §14).
    #[test]
    fn inert_v1_knobs_parse_without_forwarding() {
        let c = rain(concat!(
            "[matrix_rain]\nenabled = true\n",
            "materialize = true\nink_text = true\nphosphor = true\n"
        ))
        .expect("inert knobs never abort the resolve");
        assert_eq!(c.fps, 30, "the rest of the resolve is unaffected");
    }

    /// Nonzero seeds pass through the resolver unchanged (reproducible
    /// demos/tests); the 0 sentinel is resolved per window at engine build
    /// (`rain_config_for_window`), never here.
    #[test]
    fn seed_passes_through() {
        assert_eq!(
            rain("[matrix_rain]\nenabled = true\nseed = 42")
                .unwrap()
                .seed,
            42
        );
        assert_eq!(rain("[matrix_rain]\nenabled = true").unwrap().seed, 0);
    }

    /// The theme chroma is masked to `0x00RR_GGBB` (renderer convention) even
    /// if a caller hands a value with a dirty top byte.
    #[test]
    fn theme_channels_are_masked() {
        let c = toml::from_str::<Config>("[matrix_rain]\nenabled = true")
            .unwrap()
            .matrix_rain_params(0xFF11_1318, 0xFFD0_D0D0);
        assert_eq!(c.default_bg, 0x0011_1318);
        assert_eq!(c.theme_fg, 0x00D0_D0D0);
    }
}

#[cfg(test)]
mod matrix_rain_app_tests {
    //! PHOSPHOR host-wiring pins: the default-off zero-cost invariant (no
    //! engine is EVER constructed when nothing enables rain), the per-session
    //! `toggle_matrix_rain` override dispatch (config-independent, both
    //! directions, reload-surviving), the `rain` control-verb status face,
    //! the hot-reload dirty rebuild, the per-window seed derivation, and the
    //! Reduced-motion emit-nothing law (fp 0 ⇒ the `RepaintKey.rain_fp` term
    //! stays byte-identical-off; the early-out test builder pins the
    //! off-value 0 itself).

    use super::Config;
    use crate::keybinding::Action;

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    /// Config absent + no session override ⇒ every session's effective state
    /// is off, no per-window engine exists, and the scratch stays empty — the
    /// D-1 zero-cost pin: nothing rain-shaped runs (or allocates) on the
    /// disabled path. The PARAMETER resolve now always exists (a session can
    /// force rain on at any moment), but parameters alone construct nothing.
    #[test]
    fn default_off_constructs_no_engine() {
        let mut app = crate::App::headless_for_test();
        assert!(app.rain_dirty, "constructors mark the resolve stale");
        app.recompute_matrix_rain();
        assert!(!app.rain_dirty);
        assert!(app.rain.is_some(), "params resolve unconditionally");
        assert!(
            !app.session_rain_enabled(0),
            "absent config + no override ⇒ effectively off"
        );
        for ws in app.windows.values() {
            assert!(ws.matrix_rain.is_none(), "no engine constructed when off");
            assert!(ws.rain_scratch.is_empty());
            assert!(ws.rain_add_scratch.is_empty());
        }
    }

    /// `toggle_matrix_rain` (dispatched like the real keybinding) is now a
    /// PER-SESSION override on the window's front session: it turns rain ON
    /// over an absent/disabled config (the old kill latch never could), OFF
    /// over an enabled one, and the override WINS over a config reload until
    /// re-toggled. Engines are NOT dropped on the off flip — the render gate's
    /// suspended/drain path winds the field down (alt-screen precedent).
    #[test]
    fn toggle_dispatch_flips_front_session_override() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.recompute_matrix_rain();
        assert!(!app.session_rain_enabled(0), "default config: off");

        // ON over a disabled config — the capability the kill latch lacked.
        app.dispatch_action(wid, Action::ToggleMatrixRain);
        assert_eq!(app.pool.rain_override(0), Some(true));
        assert!(app.session_rain_enabled(0), "override forces rain on");
        assert!(app.rain_dirty, "toggle marks the resolve stale");
        app.recompute_matrix_rain();
        assert!(
            app.rain.is_some(),
            "params available for the forced-on field"
        );

        // A config reload cannot claw the session back: override still wins.
        app.config = cfg("[matrix_rain]\nenabled = false");
        app.rain_dirty = true;
        app.recompute_matrix_rain();
        assert!(
            app.session_rain_enabled(0),
            "session override survives (and outranks) a config reload"
        );

        // OFF again: the effective state flips; a live engine is retained for
        // the drain path, never dropped in the toggle pass.
        let rc = crate::rain_config_for_window(app.rain.unwrap(), wid);
        app.windows.get_mut(&wid).unwrap().matrix_rain =
            Some(Box::new(crate::matrix_rain::MatrixRain::new(rc)));
        app.dispatch_action(wid, Action::ToggleMatrixRain);
        assert_eq!(app.pool.rain_override(0), Some(false));
        assert!(!app.session_rain_enabled(0));
        assert!(
            app.windows[&wid].matrix_rain.is_some(),
            "the engine survives the off flip and drains via the suspend path"
        );

        // OFF override also beats an enabled config (the panic-off shape).
        app.config = cfg("[matrix_rain]\nenabled = true");
        app.rain_dirty = true;
        app.recompute_matrix_rain();
        assert!(
            !app.session_rain_enabled(0),
            "session off-override outranks an enabled config"
        );
        // A session that never toggled follows the config bit.
        assert_eq!(app.pool.rain_override(999), None, "unknown session: none");
    }

    /// The `rain` control-verb face ([`crate::App::rain_control`]): `status`
    /// reads without mutating; `on`/`off`/`toggle` write the front session's
    /// override; every reply carries the full one-line post-state.
    #[test]
    fn rain_control_verb_reads_and_writes_front_session() {
        let mut app = crate::App::headless_for_test();
        assert_eq!(
            app.rain_control(crate::RainCtlOp::Status).as_deref(),
            Ok(
                "config_enabled=false session_override=none effective=false \
                 engine=none active=false scope=window focused=true animating=true"
            ),
            "status is a pure read"
        );
        assert_eq!(
            app.rain_control(crate::RainCtlOp::On).as_deref(),
            Ok("config_enabled=false session_override=on effective=true \
                 engine=none active=false scope=window focused=true animating=true"),
        );
        assert_eq!(
            app.rain_control(crate::RainCtlOp::Toggle).as_deref(),
            Ok("config_enabled=false session_override=off effective=false \
                 engine=none active=false scope=window focused=true animating=true"),
        );
        app.config = cfg("[matrix_rain]\nenabled = true");
        app.rain_dirty = true;
        assert_eq!(
            app.rain_control(crate::RainCtlOp::Status).as_deref(),
            Ok("config_enabled=true session_override=off effective=false \
                 engine=none active=false scope=window focused=true animating=true"),
            "the off override keeps winning over the enabled config"
        );
        assert_eq!(
            app.rain_control(crate::RainCtlOp::Off).as_deref(),
            Ok("config_enabled=true session_override=off effective=false \
                 engine=none active=false scope=window focused=true animating=true"),
            "off is idempotent"
        );
    }

    /// Hot reload: the dirty gate re-resolves; a live engine receives the new
    /// config via `set_config` (kept, not dropped — no field time-travel).
    /// A reload that DISABLES the feature also keeps the engine: the render
    /// gate's suspended/drain path (not the recompute) winds a now-off field
    /// down, because a session override may legitimately keep raining.
    #[test]
    fn hot_reload_dirty_rebuild() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.config = cfg("[matrix_rain]\nenabled = true\nfps = 24");
        app.rain_dirty = true;
        app.recompute_matrix_rain();
        assert_eq!(app.rain.unwrap().fps, 24);
        let rc = crate::rain_config_for_window(app.rain.unwrap(), wid);
        app.windows.get_mut(&wid).unwrap().matrix_rain =
            Some(Box::new(crate::matrix_rain::MatrixRain::new(rc)));

        // A reload edits a knob (reload_config sets rain_dirty; mirrored here
        // without the disk round-trip).
        app.config = cfg("[matrix_rain]\nenabled = true\nfps = 60");
        app.rain_dirty = true;
        app.recompute_matrix_rain();
        assert_eq!(app.rain.unwrap().fps, 60, "reload re-resolves the knobs");
        assert!(
            app.windows[&wid].matrix_rain.is_some(),
            "a live engine is reconfigured in place, not dropped"
        );

        // A reload that disables the feature flips the effective bit but keeps
        // the engine — the frame gate drains it (and a session override could
        // keep it emitting on purpose).
        app.config = cfg("");
        app.rain_dirty = true;
        app.recompute_matrix_rain();
        assert!(app.rain.is_some(), "params keep resolving");
        assert!(!app.session_rain_enabled(0), "config off, no override");
        assert!(
            app.windows[&wid].matrix_rain.is_some(),
            "the engine is retained for the suspended/drain wind-down"
        );
    }

    /// The `seed = 0` sentinel derives a STABLE per-window seed (never
    /// wall-clock): identical across rebuilds of the same window, distinct
    /// across windows; a pinned nonzero seed passes through untouched.
    #[test]
    fn zero_seed_derives_stable_per_window() {
        let base = crate::matrix_rain::RainConfig {
            enabled: true,
            ..Default::default()
        };
        let a = crate::rain_config_for_window(base, crate::WindowId(0));
        let b = crate::rain_config_for_window(base, crate::WindowId(0));
        let c = crate::rain_config_for_window(base, crate::WindowId(1));
        assert_ne!(a.seed, 0, "the sentinel resolves to a real seed");
        assert_eq!(a.seed, b.seed, "stable for the window's whole life");
        assert_ne!(a.seed, c.seed, "distinct fields per window");
        let pinned = crate::matrix_rain::RainConfig { seed: 7, ..base };
        assert_eq!(
            crate::rain_config_for_window(pinned, crate::WindowId(3)).seed,
            7,
            "a configured seed is reproducible verbatim"
        );
    }

    /// W11: under a Reduced policy the engine emits NOTHING — empty channels,
    /// fp exactly 0 (the `RepaintKey.rain_fp` term equals the off-value, so
    /// idle frames stay byte-identical), and `is_active()` false (the rain
    /// timer disarms). Non-vacuity: the same engine reports active again the
    /// moment reduced lifts.
    #[test]
    fn reduced_motion_emits_nothing() {
        use crate::matrix_rain::{MatrixRain, RainConfig, RainTickInput};
        // 10 rows × 20 cols, cursor hidden (`Terminal::new(rows, cols)`).
        let mut term = aterm_core::terminal::Terminal::new(10, 20);
        term.process(b"real output\r\n\x1b[?25l");
        let input = term.cell_frame(10, 20);
        let dbg = term.default_background();
        let bg = aterm_render::rgb_to_u32([dbg.r, dbg.g, dbg.b]);
        let mut e = MatrixRain::new(RainConfig {
            enabled: true,
            default_bg: bg,
            ..Default::default()
        });
        e.rescan_from_cells(
            &input.cells,
            &input.line_sizes,
            &input.images,
            10,
            20,
            bg,
            1,
        );
        e.sample_material(&input.cells, 10, None, &[]);
        assert!(
            e.is_active(),
            "control: sampled REAL output makes literal rain live before Reduced"
        );
        e.set_reduced_motion(true);
        // Drive activity that would otherwise pour (agent-output streak).
        e.note_activity(1);
        e.note_keystroke();
        let geom = crate::word_decorations::EffectGeom {
            cell_w: 8,
            cell_h: 16,
            rows: 10,
            cols: 20,
        };
        let mut quads = Vec::new();
        let mut add = Vec::new();
        let t0 = std::time::Instant::now();
        for i in 0..30u64 {
            e.note_activity(i + 2);
            let fp = e.tick(
                t0 + std::time::Duration::from_millis(i * 33),
                geom,
                &RainTickInput::default(),
                &mut quads,
                &mut add,
            );
            assert_eq!(fp, 0, "Reduced ⇒ fp is EXACTLY 0 every frame");
            assert!(quads.is_empty(), "Reduced ⇒ no glyph quads");
            assert!(add.is_empty(), "Reduced ⇒ no halos");
        }
        assert!(!e.is_active(), "Reduced ⇒ inactive ⇒ the timer disarms");
        // Control: the invariant is not satisfied vacuously by a dead engine.
        e.set_reduced_motion(false);
        assert!(e.is_active(), "the same engine wakes once reduced lifts");
    }
}
