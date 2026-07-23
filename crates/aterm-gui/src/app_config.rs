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
    App, Backend, FONT_PX, FONT_PX_MAX, FONT_PX_MIN, PresentTarget, WindowId, build_backend,
    hud_bar, keybinding, term_lock,
};

/// User config file (`$XDG_CONFIG_HOME/aterm/aterm.toml`, else
/// `~/.config/aterm/aterm.toml`). Every field is optional; unknown keys are
/// ignored (forward-compatible). Precedence at startup is env var > config >
/// built-in default, so existing `ATERM_*` usage and `-e`/`-d` flags still win.
/// v1 exposes the settings that were previously env-only; it will grow to mirror
/// the engine's `TerminalConfig` (colours, cursor, scrollback) as themes land.
///
/// `PartialEq` (derived through every embedded table) exists for ONE consumer:
/// [`App::reload_config`]'s dedupe — a reload whose freshly parsed `Config`
/// equals the currently applied one is a no-op and skips the side-effect storm
/// (engine re-diffs, word-deco hard resets, settings popup cancels). This is
/// what collapses the Settings-panel double reload (the panel's own
/// `Wake::ConfigReload` followed by the mtime watcher's, ~500 ms later, for
/// the same bytes) into a single application.
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
    /// MOTION POLICY (W11): `"auto"` (DEFAULT — follow the OS "Reduce Motion"
    /// accessibility setting, observed live on macOS), `"full"` (always animate),
    /// or `"reduced"` (never animate). Governs every decorative animation — the
    /// cursor aurora, sparkle words, the Scene panel, and the settings effect
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
    /// Trail STYLE: `phaser` (DEFAULT — a full-spectrum additive hue sweep along the
    /// swept path), `comet` (the cadence-comet: a directional fading comet of
    /// `TrailCell`s that ignites longer/hotter with fast sustained typing, wrapped in
    /// the additive light crown), `lumen` (the additive light crown only — comet +
    /// bloom + ping), `nyan rainbow` (a momentum-driven BANDED rainbow ribbon), `sparkle`
    /// (phaser comet + spark particles), `fire` (rising embers), `laser` (white-hot
    /// beam), `water`, `beam` (a bloom-free beam-only crown, no trail body), or `off`.
    /// (`rainbow` is a back-compat alias for `nyan`.)
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
    /// Trail colour, `#RRGGBB`. Defaults to the (themed) cursor colour, so the
    /// trail matches the cursor unless overridden here.
    pub(crate) cursor_trail_color: Option<String>,
    /// Secondary/accent colour, `#RRGGBB` (comet tail + landing ring of the LUMEN
    /// styles). Defaults to a brightened cursor colour.
    pub(crate) cursor_trail_accent: Option<String>,
    /// Path to a PNG sprite for the cat that flies in front of the cursor on the
    /// `nyan` style. Supply your own image (RGBA or RGB PNG, ideally a small
    /// transparent pixel-art sprite facing right); it is nearest-scaled to fit
    /// the cursor and flown in front. Unset ⇒ the built-in homage sprite. `~`
    /// expands to $HOME.
    pub(crate) cursor_nyan_sprite: Option<String>,
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
    /// the >1.0 emission immediately; turning it ON applies to windows opened
    /// afterwards (an existing 8-bit swapchain is never re-formatted live).
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
    /// linear-light blend on an ease-out envelope). DEFAULT ON. Hard bypasses
    /// keep latency-critical paths instant — keystroke echo, the alternate
    /// screen (vim/less), a viewport scrolled away from the bottom, and a
    /// Reduced motion policy (W11) all render exact bytes. Set
    /// `stream_fade = false` to disable.
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
    pub(crate) shell: Option<String>,
    /// Extra argv passed to the `shell` after argv[0] (config `shell_args`), e.g.
    /// `["-l", "-i"]` for a login+interactive bash. Ignored when `-e` runs a
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
    /// warns and falls back to `auto`. macOS-only today (the field is parsed but
    /// inert on other platforms). Replaces the old unconditional dark-chrome force.
    pub(crate) window_theme: Option<String>,
    /// GPU-present COLOUR SPACE (M3 phase A): the colour space the window's
    /// CAMetalLayer is TAGGED with at surface attach — i.e. how ColorSync
    /// INTERPRETS the swapchain's sRGB-encoded bytes on glass. `"srgb"` (the
    /// default) is the honest tag: theme colours render as authored and ColorSync
    /// performs the one sRGB→panel mapping. `"display-p3"` reproduces the legacy
    /// UNTAGGED look on a wide-gamut Mac (the bytes are read as P3 coordinates —
    /// oversaturated but familiar). Interpretation only: the rendered/readback
    /// BYTES are identical either way (the parity/readback suites pin this).
    /// Maps to [`WindowColorspace`] via [`Config::window_colorspace_or_default`];
    /// unknown values warn and fall back to `srgb`. Hot-reloadable; inert off
    /// macOS and on the CPU (softbuffer) present path.
    pub(crate) window_colorspace: Option<String>,
    /// macOS: when `true`, the Option (Alt) modifier sends ESC-prefixed (Meta)
    /// key sequences — the standard terminal expectation. When `false`, Option
    /// produces the OS-composed character (`å`) instead. ABSENT keeps the current
    /// default (Meta), so no config = byte-identical. See [`Config::option_as_meta_or_default`].
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
    /// Copy a mouse selection to the system clipboard automatically the moment a
    /// drag-select completes (mouse-up), so no explicit Cmd-C is needed. DEFAULT
    /// ON — the X11-style copy-on-select convenience, flipped on with the other
    /// visual/UX defaults. Set `copy_on_select = false` to opt out (the
    /// explicit-copy behaviour). The selection is left highlighted either way,
    /// so Cmd-C still works. See [`Config::copy_on_select_or_default`].
    pub(crate) copy_on_select: Option<bool>,
    /// MASTER switch for the WHOLE bottom HUD band (Resources + Engine + Scene at
    /// once). Default ON. `false` hides every panel regardless of the per-panel
    /// `show_*_hud` keys — which keep their values, so flipping this back on restores
    /// the previous per-panel selection. See [`Config::show_hud_or_default`].
    pub(crate) show_hud: Option<bool>,
    /// Show the bottom RESOURCES HUD — total system vs this terminal session (CPU,
    /// memory, GPU, disk, network). Default ON — the performance GUI ships enabled.
    /// Toggleable live via the Performance control panel or View ▸ Show Resources HUD.
    /// See [`Config::show_resources_hud_or_default`].
    pub(crate) show_resources_hud: Option<bool>,
    /// Show the aterm ENGINE HUD (render backend/fps/frame-time, latency, aterm memory,
    /// app-fed streams). Default ON, like Resources — the performance GUI ships whole.
    pub(crate) show_engine_hud: Option<bool>,
    /// Show the subtle TOP-RIGHT build/version badge (`v{version} · {build}`) so the
    /// running build is answerable at a glance without opening About. Default ON.
    /// Toggleable via the Settings overlay ▸ "Show build/version badge". See
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
    /// can never make text illegible. GPU backend only; the CPU/softbuffer path
    /// falls back to a solid background (warn-once). Hot-reloadable.
    pub(crate) background_opacity: Option<f32>,
    /// M5 true vibrancy: the macOS `NSVisualEffectView` MATERIAL blended behind
    /// the translucent background — `none` | `hud` | `sidebar` | `under-window`
    /// (aliases `underwindow`/`under_window`, case-insensitive). ABSENT / `none`
    /// = no vibrancy view. Only takes effect when `background_opacity < 1.0` on
    /// the GPU backend (macOS). Hot-reloadable.
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
    /// Security opt-in: allow apps to READ the system clipboard via OSC 52
    /// (`Pd = "?"`). Default OFF (fail-closed) — a clipboard read is an
    /// exfiltration vector from untrusted output. Maps to `allow_osc52_query`.
    pub(crate) allow_osc52_query: Option<bool>,
    /// Security opt-in: allow XTWINOPS window manipulation + geometry/title
    /// reports (`CSI t`). Default OFF — title reports can fingerprint and window
    /// moves can hide the window. Maps to `allow_window_ops`.
    pub(crate) allow_window_ops: Option<bool>,
    /// Security opt-in: allow desktop notifications (OSC 9 / 99 / 777). Default
    /// OFF. Maps to `allow_notifications`.
    pub(crate) allow_notifications: Option<bool>,
    /// Security opt-in: allow apps to reconfigure the color palette (OSC 4/104).
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
    /// pulls notarized releases from. Absent ⇒ the compiled-in default
    /// (`alabsystems/aterm`). The env vars `ATERM_UPDATE_OWNER`/`ATERM_UPDATE_REPO`
    /// override these. The location is NOT the trust anchor — the compiled-in pinned
    /// Team ID + Apple notarization are — so repointing the channel cannot get an
    /// untrusted build installed. macOS-only in effect; parsed (and inert) elsewhere.
    /// See [`UpdateConfig`], crate `aterm-update`, and `docs/RELEASING.md`.
    pub(crate) update: Option<UpdateConfig>,
    /// Sparkle words (`[sparkle_words]`): decorate matched profanity words with a
    /// randomized sparkle, cat/kitty words with a cat-paw, and orca words with a
    /// splash. Absent ⇒ feature ON with every category (the on-by-default product
    /// choice); set `enabled = false` to disable. See [`SparkleWordsConfig`].
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

/// The `[sparkle_words]` table. Two independent, distinct effects:
/// a randomized SPARKLE over profanity ("fuck" family, every major language) and a
/// steady CAT-PAW over cat/kitty words. PURELY VISUAL — never affects copied text,
/// logs, or recorded sessions.
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
/// color    = "#f7a8b8"
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
    /// The randomized profanity SPARKLE sub-table.
    pub(crate) profanity: Option<SparkleProfanityConfig>,
    /// The steady feline CAT-PAW sub-table.
    pub(crate) feline: Option<SparkleFelineConfig>,
    /// The randomized orca/cetacean SPLASH sub-table.
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

#[derive(Default)]
struct LoadedToyPacks {
    spec_table: aterm_effects::spec::SpecTable,
    lexicon_toml: String,
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

/// Maximum encoded PNG bytes admitted for one custom Nyan cursor sprite.
///
/// The general terminal image decoder deliberately accepts much larger payloads;
/// this config asset is tiny UI chrome and therefore has a tighter independent
/// budget.  The resolver uses a `take(MAX + 1)` read, so a misleading file
/// metadata length can never turn config admission into an unbounded allocation.
pub(crate) const MAX_NYAN_SPRITE_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum width or height of a decoded custom cursor sprite.
pub(crate) const MAX_NYAN_SPRITE_DIMENSION: usize = 1024;
const MAX_NYAN_SOURCE_ID_BYTES: usize = 2 * 1024;
const MAX_NYAN_REASON_BYTES: usize = 320;

/// One fully-resolved custom Nyan cursor asset for an admitted config generation.
///
/// `Invalid` is intentionally distinct from `BuiltIn`: a bad authored sprite
/// fails closed (the cursor companion is disabled) and remains diagnosable.  It
/// can never silently turn into the built-in homage on glass while Settings says
/// the custom value is active.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum NyanSpriteAsset {
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

impl NyanSpriteAsset {
    /// Stable paint/install identity.  The variant is part of the identity, so
    /// `Invalid` can never alias `BuiltIn` even when both carry no custom pixels.
    pub(crate) fn fingerprint(&self) -> u64 {
        match self {
            Self::BuiltIn => 0x4E59_414E_5F42_5549,
            Self::Ready { fp, .. } => *fp,
            Self::Invalid {
                source_id,
                bounded_reason,
            } => {
                stable_nyan_fingerprint(0x49, source_id.as_bytes(), 0, 0, bounded_reason.as_bytes())
            }
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

/// Every non-text config asset admitted at one revision.  `ConfigSnapshot`
/// carries one outer `Arc<ConfigAssetCatalog>` and the live host, capture, and
/// all Settings views clone that exact Arc; there is no independently-resolved
/// Trail/Nyan lane that can lag the text generation.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConfigAssetCatalog {
    pub(crate) trail_packs: std::sync::Arc<TrailPackCatalog>,
    pub(crate) nyan_sprite: NyanSpriteAsset,
}

impl ConfigAssetCatalog {
    pub(crate) fn empty() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            trail_packs: TrailPackCatalog::empty(),
            nyan_sprite: NyanSpriteAsset::BuiltIn,
        })
    }
}

impl TrailPackCatalog {
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
    /// Decorate profanity at all. Default TRUE (the on-by-default product choice);
    /// set false to keep the strong words off, e.g. for screenshares / HR contexts.
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

/// `[sparkle_words.feline]` — the steady cat-paw.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SparkleFelineConfig {
    /// Decorate cat/kitty words. Default TRUE (takes effect only when master on).
    pub(crate) enabled: Option<bool>,
    /// `"cat"` (default) = the v2 peeking cat (design §5; auto-falls back to
    /// the paw below 14 px cell height / 7 px width) | `"paw"` = the exact v1
    /// steady paw. Unknown values fall back to `"cat"`.
    pub(crate) style: Option<String>,
    /// Blink / ear-twitch one-shots (§5.6: ≤ 1 event/s window-wide,
    /// focus-gated). Default TRUE; `false` ⇒ exact 0% between damage.
    pub(crate) idle: Option<bool>,
    /// Pupils track the cursor (§5.8: present-driven, zero new wakes).
    /// Default TRUE; `false` ⇒ centered pupils, no tracking.
    pub(crate) gaze: Option<bool>,
    /// Fortune (1/512) / Nebula (1/1024) rare cats (§3.5/§5.4). Default TRUE.
    pub(crate) magic: Option<bool>,
    /// Paw tint `#RRGGBB`. Default soft pink `#f7a8b8`.
    pub(crate) color: Option<String>,
    /// Opacity 0.0..=1.0. Default 0.7.
    pub(crate) intensity: Option<f32>,
    /// Decorate the bare 3-letter `cat` token (also the shell command). Default true.
    pub(crate) allow_bare_cat: Option<bool>,
    /// Decorate a lone CJK cat ideograph (`猫`) anywhere. Default false (high FP).
    pub(crate) cjk_single_char: Option<bool>,
    /// Record sightings into the durable Kitty Log (`kitty-log.toml` — the
    /// settings collection book, §F4). Default TRUE. HOST-side gate only: the
    /// effects engine always records (bounded per tick) and the App drains-
    /// and-drops when this is false — nothing is counted or written.
    pub(crate) log: Option<bool>,
    /// Extra whole words to treat as feline (added to the lexicon).
    pub(crate) extra_words: Option<Vec<String>>,
    /// Folded surfaces to never decorate as feline.
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// `[sparkle_words.orca]` — the randomized water-droplet SPLASH (ocean palette,
/// droplets that spray upward). Aesthetics are fixed; it reuses the profanity sparkle's
/// motion params (density / anim_ms / jitter / intensity).
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SparkleOrcaConfig {
    /// Decorate orca/cetacean words. Default TRUE (takes effect only when master on).
    pub(crate) enabled: Option<bool>,
    /// Extra whole words to treat as orca (added to the lexicon).
    pub(crate) extra_words: Option<Vec<String>>,
    /// Folded surfaces to never decorate as orca.
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
    /// Inbound bind address for the TLS listener, e.g. `"0.0.0.0:7100"`. Absent ⇒
    /// listener OFF. (`ATERM_NET_LISTEN` overrides.)
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
/// (`alabsystems/aterm`). Resolution precedence is env > this config > default,
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
        let raw = self.theme.as_deref()?;
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
        None // split form that omits this appearance's side → built-in Default
    }

    /// Resolve the BASE color scheme this config selects for `appearance` (see
    /// [`Self::resolve_theme_name`]): the named built-in (case-insensitive), a user
    /// theme FILE of that name, or the built-in [`aterm_types::ColorScheme::default`]
    /// when no theme — or an unresolvable / malformed one — is set. The per-key color
    /// overrides (`foreground`/…/`palette`) are layered ON TOP of this base by the
    /// callers, so they always win.
    fn base_scheme_for(&self, appearance: aterm_types::Appearance) -> aterm_types::ColorScheme {
        // Resolves SILENTLY (unresolvable/malformed name → Default): both `theme()`
        // and `terminal_config()` call this, so warning here would double-print. The
        // single "unknown theme" diagnostic is emitted in `terminal_config`.
        match self.resolve_theme_name(appearance) {
            None => aterm_types::ColorScheme::default(),
            Some(name) => aterm_types::scheme::load(&name).unwrap_or_default(),
        }
    }

    /// The RENDERER theme (window clear colour, cursor, selection highlight). Starts
    /// from the selected scheme's chrome ([`Self::base_scheme_for`]); the per-key color
    /// keys then override individual slots (unchanged precedence) so the window CLEAR
    /// colour matches a configured `background` and `selection_color` themes the
    /// highlight.
    pub(crate) fn theme(&self) -> Theme {
        self.theme_for(aterm_types::Appearance::Dark)
    }

    /// [`Self::theme`] resolved for a specific OS `appearance` — drives the live
    /// light↔dark scheme switch (see [`Self::resolve_theme_name`]).
    pub(crate) fn theme_for(&self, appearance: aterm_types::Appearance) -> Theme {
        let tp = self.base_scheme_for(appearance).to_theme_parts();
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

    /// Whether the Option/Alt modifier should send ESC-prefixed (Meta) sequences.
    /// The DEFAULT when the key is absent is `true` — aterm already routes Option
    /// through the engine encoder, which ESC-prefixes Alt, so "absent = Meta" is
    /// exactly today's behavior (no regression). Setting `option_as_meta = false`
    /// opts into OS-composed characters (`å`) instead.
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

    /// Whether a completed mouse selection auto-copies to the clipboard. DEFAULT
    /// when absent is `true` — the X11-style copy-on-select convenience, flipped on
    /// with the other visual/UX defaults. Setting `copy_on_select = false` opts out
    /// (the ghostty/macOS explicit-copy behaviour).
    pub(crate) fn copy_on_select_or_default(&self) -> bool {
        self.copy_on_select.unwrap_or(true)
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

    /// Whether the cursor motion-trail ("streaming trailer") is on. DEFAULT ON with the
    /// `phaser` style (owner call — batteries-on delight): the trail ignites a ~260ms
    /// additive aurora on each cursor move and decays to EXACTLY 0% idle, so a still
    /// screen costs nothing. The GPU bloom pass is effect-frame-only and load-sheds
    /// under pressure. Opt out with `cursor_trail = false`.
    pub(crate) fn cursor_trail_or_default(&self) -> bool {
        self.cursor_trail.unwrap_or(true)
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

    /// Ambient-bed on/off (`trail_sound_bed`, default OFF — the drone is
    /// opt-in; see the field docs: notes/brrrring/bonk/melody unaffected).
    pub(crate) fn trail_sound_bed_or_default(&self) -> bool {
        self.trail_sound_bed.unwrap_or(false)
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

    /// The parsed `motion` policy mode (W11): `auto` (DEFAULT — follow the OS
    /// "Reduce Motion" setting) / `full` / `reduced`; unknown strings fall back
    /// to `auto`. Borrow-free parse per call, so the per-redraw caller
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
        // Default: the NYAN RAINBOW ribbon (the banded rainbow whose blinking block
        // twinkles like a little star — the effect the owner made the default),
        // under its canonical two-word spelling.
        // `glow_config`/`trail_config` split the layers.
        self.cursor_trail_style
            .as_deref()
            .unwrap_or("nyan rainbow")
            .trim()
    }

    /// The RAW configured Nyan-sprite path (trimmed, NOT `~`-expanded). Startup
    /// and config reload publish this borrowed value to the asynchronous loader;
    /// path expansion, filesystem access, and decode all happen on its worker.
    /// Empty/whitespace means the built-in homage.
    pub(crate) fn cursor_nyan_sprite_raw(&self) -> Option<&str> {
        let raw = self.cursor_nyan_sprite.as_deref()?.trim();
        (!raw.is_empty()).then_some(raw)
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

    fn sparkle_toy_packs(&self) -> LoadedToyPacks {
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
            let expanded = sparkle_expand_tilde(path);
            let source = match aterm_effects::spec::read_toy_pack_file(&expanded) {
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

    /// Resolve one immutable Trail Pack catalog. This is the only manifest IO
    /// primitive; production calls it exactly once when the versioned config
    /// service admits a new generation, then shares the returned `Arc`.
    ///
    /// Fail-closed per pack: unreadable/invalid manifests become retained
    /// diagnostics and are skipped, while a later duplicate id wins. Diagnostics
    /// are data rather than immediate stderr writes so each host can surface them
    /// once without Settings construction producing duplicate warnings.
    pub(crate) fn resolve_trail_pack_catalog(&self) -> std::sync::Arc<TrailPackCatalog> {
        let mut loaded = TrailPackCatalog::default();
        let Some(paths) = self.cursor_trail_packs.as_deref() else {
            return std::sync::Arc::new(loaded);
        };
        if paths.len() > MAX_ACTIVE_TOY_PACKS {
            loaded.diagnostics.push(format!(
                "cursor_trail_packs lists {} paths; only the first \
                 {MAX_ACTIVE_TOY_PACKS} are active",
                paths.len(),
            ));
        }
        for (index, path) in paths.iter().take(MAX_ACTIVE_TOY_PACKS).enumerate() {
            let expanded = sparkle_expand_tilde(path);
            let source = match aterm_effects::trail_pack::read_trail_pack_file(&expanded) {
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
        std::sync::Arc::new(loaded)
    }

    /// Resolve every filesystem-backed visual asset for one config generation.
    ///
    /// This is the sole production Nyan PNG I/O/decode seam.  The versioned
    /// config service calls it before advancing the revision, then publishes the
    /// returned outer Arc with the exact TOML text.  Present, capture, semantic
    /// Settings view construction, and effects code only clone/read the result.
    pub(crate) fn resolve_asset_catalog(&self) -> std::sync::Arc<ConfigAssetCatalog> {
        std::sync::Arc::new(ConfigAssetCatalog {
            trail_packs: self.resolve_trail_pack_catalog(),
            nyan_sprite: resolve_nyan_sprite_asset(self.cursor_nyan_sprite.as_deref()),
        })
    }

    /// Content fingerprints of every file the config references BY PATH — see
    /// [`PathFeedFps`]. Reads each referenced file once (small manifest-scale
    /// files, cold path only: config reload + [`App::recompute_sparkle`]). The
    /// PATH participates in each stream (two files swapping contents must not
    /// cancel out) and so does readability (a file appearing or disappearing is
    /// a content change even though its bytes stay unknown). Pack lists are
    /// capped at the same `MAX_ACTIVE_TOY_PACKS` the consumers load, so an
    /// over-cap tail can neither mask nor fake a change.
    pub(crate) fn path_feed_fingerprints(&self) -> PathFeedFps {
        use std::hash::{Hash, Hasher};
        fn fold(hasher: &mut std::collections::hash_map::DefaultHasher, path: &str) {
            path.hash(hasher);
            match std::fs::read(sparkle_expand_tilde(path)) {
                Ok(bytes) => {
                    true.hash(hasher);
                    bytes.hash(hasher);
                }
                Err(_) => false.hash(hasher),
            }
        }
        let mut deco = std::collections::hash_map::DefaultHasher::new();
        if let Some(sw) = self.sparkle_words.as_ref() {
            if let Some(lexicon) = sw.lexicon.as_deref() {
                fold(&mut deco, lexicon);
            }
            for path in sw
                .toy_packs
                .as_deref()
                .unwrap_or_default()
                .iter()
                .take(MAX_ACTIVE_TOY_PACKS)
            {
                fold(&mut deco, path);
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
            fold(&mut trail, path);
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
    fn sparkle_runtime_parts(
        &self,
    ) -> Option<(crate::word_decorations::DecoConfig, Option<String>)> {
        if self
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.enabled)
            == Some(false)
        {
            return None;
        }
        let LoadedToyPacks {
            spec_table,
            lexicon_toml,
        } = self.sparkle_toy_packs();
        let cfg = self.sparkle_deco_config_with_pack_specs(spec_table)?;
        let override_toml = self.sparkle_override_toml_with_packs(&lexicon_toml);
        Some((cfg, override_toml))
    }

    /// Resolve the `[sparkle_words]` table into a renderer-ready [`DecoConfig`],
    /// applying every default + clamp. `None` when the feature is explicitly disabled or
    /// every category is off (the caller then renders byte-identically).
    ///
    /// ON BY DEFAULT: an absent `[sparkle_words]` table (or an absent `enabled` key) turns
    /// the feature ON with ALL THREE families decorating — profanity sparkle, feline
    /// cat-paw, and orca splash. Set `enabled = false` (or a category's `enabled = false`)
    /// to silence it; the `toggle_sparkle_words` keybinding is the instant panic-off.
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
        let orca_cfg = sw.orca.clone().unwrap_or_default();
        let ink_cfg = sw.ink.clone().unwrap_or_default();
        let emph = sw.emphasis.clone().unwrap_or_default();
        // All families ON by default; the master `enabled` (default on) or each
        // category's `enabled = false` opts out.
        let profanity = prof.enabled.unwrap_or(true);
        let feline = fel.enabled.unwrap_or(true);
        // v3 §4: the orca class is SUSPENDED — the resolver ANDs the single
        // const gate (engine/lexicon/splash untouched; flip ORCA_SUSPENDED to
        // re-enable).
        let orca = orca_cfg.enabled.unwrap_or(true) && !aterm_effects::ORCA_SUSPENDED;
        let ink_enabled = ink_cfg.enabled.unwrap_or(true);
        // v3 §6: the custom-word spec table (per-word overrides, keyed by the
        // scanner's form_hash semantics — folded spaced surfaces, possessive
        // variants, RAW CJK hashes).
        let custom_entries = sw.custom.clone().unwrap_or_default();
        let (inline_specs, _) = aterm_effects::spec::build_custom(&custom_entries);
        // Inline config is the user's most local authority and overlays all
        // imported packs, including every possessive/hash variant.
        spec_table.overlay(inline_specs);
        // v3 §6 emphasis resolve: `enabled && (ink_enabled || has_custom)` —
        // a graphic-only or burst-only custom word must scan with ink off
        // (custom words ride the emphasis class).
        let emphasis = emph.enabled.unwrap_or(true) && (ink_enabled || spec_table.has_custom());
        if !profanity && !feline && !orca && !emphasis {
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
        let feline_color = fel
            .color
            .as_deref()
            .and_then(parse_hex_color)
            .map_or(0x00F7_A8B8, rgb_u32);
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
            feline_color,
            feline_intensity: fel.intensity.unwrap_or(0.7).clamp(0.0, 1.0),
            // §10: "paw" is the exact v1 path; anything else (incl. absent) is
            // the v2 peeking cat.
            feline_style: if fel.style.as_deref() == Some("paw") {
                crate::word_decorations::FelineStyle::Paw
            } else {
                crate::word_decorations::FelineStyle::Cat
            },
            feline_idle: fel.idle.unwrap_or(true),
            feline_gaze: fel.gaze.unwrap_or(true),
            feline_magic: fel.magic.unwrap_or(true),
            // §10 / v3 §3.1: "sparkle" is the exact v1 path, "nova" the v2
            // classic nova; anything else (incl. absent) is the v3 rainbow.
            // Case-insensitive, mirroring the web `set_sparkle_profanity`
            // setter — a cased "Nova" must not silently fall through to
            // Rainbow (and its supernova escalation roll).
            profanity_style: match prof.style.as_deref() {
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
        let packs = self.sparkle_toy_packs();
        self.sparkle_override_toml_with_packs(&packs.lexicon_toml)
    }

    fn sparkle_override_toml_with_packs(&self, pack_lexicon_toml: &str) -> Option<String> {
        let sw = self.sparkle_words.as_ref()?;
        let mut out = String::new();
        if let Some(path) = sw.lexicon.as_deref() {
            let expanded = sparkle_expand_tilde(path);
            match std::fs::read_to_string(&expanded) {
                Ok(contents) => {
                    let languages = self.sparkle_languages();
                    let refs: Vec<&str> = languages.iter().map(String::as_str).collect();
                    match aterm_lexicon::Lexicon::with_languages_and_override(
                        &refs,
                        Some(&contents),
                    ) {
                        Ok(_) => {
                            out.push_str(&contents);
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
                Err(e) => {
                    eprintln!(
                        "aterm-gui: sparkle_words.lexicon {expanded:?} unreadable ({e}); \
                         skipping that layer"
                    );
                }
            }
        }
        let prof = sw.profanity.clone().unwrap_or_default();
        let fel = sw.feline.clone().unwrap_or_default();
        let orca_cfg = sw.orca.clone().unwrap_or_default();
        let emph = sw.emphasis.clone().unwrap_or_default();
        append_extra_words_entry(&mut out, "profanity", prof.extra_words.as_deref());
        append_extra_words_entry(&mut out, "feline", fel.extra_words.as_deref());
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

    /// The `[packages]` `auto_install` RESOLVED bit (default FALSE — multi-GB
    /// toolchains need explicit consent; the Settings switch IS the consent
    /// click, `docs/TOOLCHAIN-PACKAGE-MANAGER.md` §11). Consumed by the
    /// co-located `atpkg` from the same table; the GUI only displays/edits it.
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
            p.account.as_deref(),
            p.channel.as_deref(),
            p.include.as_deref(),
            p.exclude.as_deref(),
        );
        p.enabled.unwrap_or(true) && p.auto_update.unwrap_or(true)
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

    /// GPU cursor-comet bloom — the light CROWN around the comet head. DEFAULT ON
    /// (paired with the on-by-default `cursor_trail`): with the comet's continuous
    /// beam this is the shipped "luminous streak" signature. The cost (a half-res
    /// blur pass) runs only on effect frames, which the present-paced pump drives
    /// at the display rate with ~0.2ms frame cost (measured, AMD 780M iGPU); the
    /// `perf_reduced` load-shed latch and `motion` policy both drop it under
    /// pressure/accessibility, and `cursor_trail_bloom = false` opts out.
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
        if aterm_render::resolve_font_family(fam).is_some() {
            return None;
        }
        Some(format!(
            "font_family {fam:?} does not resolve to a font file; using the built-in \
             candidates (see `aterm list-fonts` for resolvable families)"
        ))
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
                 expected one of phaser|nyan rainbow|comet|lumen|sparkle|fire|laser|water|beam|off \
                 (or a documented alias like nyan/rainbow/ember/ocean), or pack:<id> for a loaded \
                 Trail Pack"
            )),
        }
    }

    /// The MASTER bottom-HUD switch (DEFAULT OFF): `true` shows the whole band. The
    /// per-panel `show_*_hud` values are ANDed with this by every consumer (startup
    /// seed, config reload, the user-gesture toggles) — the keys themselves are never
    /// rewritten by the master, so re-enabling restores the previous selection. Off by
    /// default so a fresh terminal is a clean grid; opt in with `show_hud = true`.
    pub(crate) fn show_hud_or_default(&self) -> bool {
        self.show_hud.unwrap_or(false)
    }

    /// Whether to show the OPTIONAL floating top-right build/version pill. Default OFF:
    /// the version now lives in the menu bar (the top-level `v<version>` menu, which
    /// opens About), so the floating pill is an opt-in extra rather than the primary
    /// surface. Enable with `show_build_badge = true` or the Settings overlay. See
    /// [`crate::build_badge`].
    pub(crate) fn show_build_badge_or_default(&self) -> bool {
        self.show_build_badge.unwrap_or(false)
    }

    /// Whether to show the bottom Resources HUD (system vs session). Default ON — the
    /// performance GUI ships enabled (toggle it from the Performance control panel, the
    /// View menu, or `show_resources_hud = false` in aterm.toml).
    pub(crate) fn show_resources_hud_or_default(&self) -> bool {
        self.show_resources_hud.unwrap_or(true)
    }

    /// Whether to show the aterm Engine HUD (render speed / memory). Default ON;
    /// disable with `show_engine_hud = false` or View ▸ Show Engine HUD.
    pub(crate) fn show_engine_hud_or_default(&self) -> bool {
        self.show_engine_hud.unwrap_or(true)
    }

    /// Mirror a live per-panel HUD gesture into THIS config, so later live decisions
    /// that resolve through [`Config::hud_enabled`] (the master toggle's per-panel
    /// wants, palette checkmarks) see the gesture immediately instead of waiting on
    /// the ~500ms config-watcher reload of the file the gesture also wrote (which may
    /// never land — persistence is best-effort).
    pub(crate) fn set_hud_enabled(&mut self, id: hud_bar::PanelId, on: bool) {
        match id {
            hud_bar::PanelId::Resources => self.show_resources_hud = Some(on),
            hud_bar::PanelId::Engine => self.show_engine_hud = Some(on),
        }
    }

    /// Whether HUD panel `id` is enabled per THIS config — the seed for its settings
    /// control (and the default the View menu / Performance panel resolve). Resources
    /// and Engine default ON, Scene OFF (see the `*_or_default` resolvers above).
    /// Deliberately NOT gated by the master [`Config::show_hud_or_default`] — the
    /// per-panel settings rows must reflect the per-panel keys; consumers that decide
    /// what is actually on screen fold the master in themselves.
    pub(crate) fn hud_enabled(&self, id: hud_bar::PanelId) -> bool {
        match id {
            hud_bar::PanelId::Resources => self.show_resources_hud_or_default(),
            hud_bar::PanelId::Engine => self.show_engine_hud_or_default(),
        }
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
    /// `background_material` (M5 true vibrancy): the window-level
    /// `NSVisualEffectView` blur. Resolved, validated and diffed; on the GPU
    /// backend it drives `AppRt::window_set_vibrancy` (install/update/remove the
    /// behind-window backdrop) whenever the window is also translucent
    /// (`background_opacity < 1.0`). The CPU softbuffer surface has no non-opaque
    /// composite, so a non-`none` material there warns once and has no effect.
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
/// [`crate::App::apply_font_config`].
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
            let path = aterm_render::resolve_font_family(fam);
            if path.is_none() {
                warns.push(format!(
                    "config {key}: {fam:?} does not resolve to a font file; ignored \
                     (see `aterm list-fonts`)"
                ));
            }
            path
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
    if measured_pts <= 0.0 || measured_pts > TITLEBAR_BAND_SANITY_CAP_PTS {
        return prev_pts;
    }
    measured_pts
}

#[cfg(test)]
mod titlebar_band_acceptance_tests {
    use super::{TITLEBAR_BAND_SANITY_CAP_PTS, accept_titlebar_band_pts};

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
    });
}

/// Window-CHROME appearance (titlebar + traffic lights), distinct from the
/// terminal-body color scheme. Resolved from config `window_theme` via
/// [`Config::window_theme_or_default`] and applied to the NSWindow appearance in
/// `platform::AppRtMacOS::window_set_appearance` (macOS).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum WindowTheme {
    /// Follow the OS light/dark setting (no `NSAppearance` override), so the
    /// chrome tracks live day-night appearance switches. The default.
    #[default]
    Auto,
    /// Force light chrome (`NSAppearanceNameAqua`).
    Light,
    /// Force dark chrome (`NSAppearanceNameDarkAqua`).
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

/// Content fingerprints of the files the config references BY PATH: the
/// `sparkle_words.lexicon` file and `sparkle_words.toy_packs` manifests (the
/// word-decoration feed) and the `cursor_trail_packs` manifests (the Trail Pack
/// registry feed). The APPLIED configuration includes these files' CONTENT, yet
/// `Config` equality compares only the path strings — so the reload dedupe must
/// compare fingerprints too, or the documented touch-to-reload workflow (edit a
/// pack/lexicon file, then re-save/`touch` a byte-identical `aterm.toml` —
/// docs/trail-packs.md, docs/TOY_PACKS.md, docs/sparkle-words-design.md) silently
/// stops re-reading them, with restartless recovery only via non-obvious
/// workarounds. Split in two so the consumers reset precisely: the deco feed
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
fn sparkle_expand_tilde(path: &str) -> std::path::PathBuf {
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

fn bounded_nyan_text(value: &str, max_bytes: usize) -> std::sync::Arc<str> {
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

fn invalid_nyan_sprite(source_id: &str, reason: impl AsRef<str>) -> NyanSpriteAsset {
    NyanSpriteAsset::Invalid {
        source_id: bounded_nyan_text(source_id, MAX_NYAN_SOURCE_ID_BYTES),
        bounded_reason: bounded_nyan_text(reason.as_ref(), MAX_NYAN_REASON_BYTES),
    }
}

fn read_bounded_nyan_png(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("unreadable ({error})"))?;
    let mut bytes = Vec::with_capacity(MAX_NYAN_SPRITE_FILE_BYTES.min(64 * 1024));
    file.take((MAX_NYAN_SPRITE_FILE_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unreadable ({error})"))?;
    if bytes.len() > MAX_NYAN_SPRITE_FILE_BYTES {
        return Err(format!(
            "encoded PNG exceeds the {} byte limit",
            MAX_NYAN_SPRITE_FILE_BYTES
        ));
    }
    Ok(bytes)
}

fn stable_nyan_fingerprint(tag: u8, source: &[u8], w: u16, h: u16, rgba: &[u8]) -> u64 {
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

fn resolve_nyan_sprite_asset(raw: Option<&str>) -> NyanSpriteAsset {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return NyanSpriteAsset::BuiltIn;
    };
    if raw.len() > MAX_NYAN_SOURCE_ID_BYTES {
        return invalid_nyan_sprite(raw, "configured sprite source is too long");
    }
    let source_id = std::sync::Arc::<str>::from(raw);
    let path = sparkle_expand_tilde(raw);
    let bytes = match read_bounded_nyan_png(&path) {
        Ok(bytes) => bytes,
        Err(reason) => return invalid_nyan_sprite(&source_id, reason),
    };
    let Some((rgba, w, h)) = aterm_render::decode_png_rgba8(&bytes) else {
        return invalid_nyan_sprite(&source_id, "PNG decode failed");
    };
    if w == 0
        || h == 0
        || w > MAX_NYAN_SPRITE_DIMENSION
        || h > MAX_NYAN_SPRITE_DIMENSION
        || rgba.len() != w.saturating_mul(h).saturating_mul(4)
    {
        return invalid_nyan_sprite(
            &source_id,
            format!("decoded sprite must be 1..={MAX_NYAN_SPRITE_DIMENSION} pixels per side"),
        );
    }
    let Ok(w) = u16::try_from(w) else {
        return invalid_nyan_sprite(&source_id, "decoded width is out of range");
    };
    let Ok(h) = u16::try_from(h) else {
        return invalid_nyan_sprite(&source_id, "decoded height is out of range");
    };
    let fp = stable_nyan_fingerprint(0x52, source_id.as_bytes(), w, h, &rgba);
    NyanSpriteAsset::Ready {
        source_id,
        w,
        h,
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
    pub(crate) fn terminal_config_for(
        &self,
        appearance: aterm_types::Appearance,
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
            tc.cursor_style = match self.cursor_style.as_deref().unwrap_or("block") {
                "block" if blink => CursorStyle::BlinkingBlock,
                "block" => CursorStyle::SteadyBlock,
                // The "_" underline OPTION is retired (owner: keep block + "|").
                // Programs may still request underline via DECSCUSR — that is
                // terminal protocol, not user configuration; a config asking for
                // it falls back to the bar with a one-line note.
                "underline" => {
                    eprintln!(
                        "aterm-gui: config cursor_style \"underline\" is retired; using \"bar\""
                    );
                    if blink {
                        CursorStyle::BlinkingBar
                    } else {
                        CursorStyle::SteadyBar
                    }
                }
                "bar" | "beam" if blink => CursorStyle::BlinkingBar,
                "bar" | "beam" => CursorStyle::SteadyBar,
                other => {
                    eprintln!("aterm-gui: config cursor_style: expected block|bar, got {other:?}");
                    if blink {
                        CursorStyle::BlinkingBlock
                    } else {
                        CursorStyle::SteadyBlock
                    }
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
            // Single point that warns on a theme that does not resolve to a built-in
            // OR a parseable user theme file (base_scheme_for resolves silently, so this
            // never double-prints from theme() + here). A NotFound names the built-in
            // set + the user theme dir; a Parse error surfaces the offending line.
            if !name.eq_ignore_ascii_case("default") {
                match aterm_types::scheme::load(&name) {
                    Ok(_) => {}
                    Err(aterm_types::scheme::ThemeError::NotFound(_)) => {
                        let where_ = aterm_types::scheme::user_theme_dir()
                            .map(|p| format!(" or a file in {}", p.display()))
                            .unwrap_or_default();
                        eprintln!(
                            "aterm-gui: config theme: unknown theme {name:?}; using Default (built-ins: {}{where_})",
                            aterm_types::scheme::builtin_names().join(", ")
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "aterm-gui: config theme: failed to load {name:?} ({e}); using Default"
                        );
                    }
                }
            }
            let s = self.base_scheme_for(appearance);
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
            match b.to_ascii_lowercase().as_str() {
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
            match w.to_ascii_lowercase().as_str() {
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
    pub(crate) fn applied_terminal_config(&self) -> aterm_core::config::TerminalConfig {
        self.applied_terminal_config_for(aterm_types::Appearance::Dark)
    }

    /// [`Self::applied_terminal_config`] resolved for a specific OS `appearance` — the
    /// engine config the GUI applies live when the desktop toggles light↔dark under a
    /// `dark:…,light:…` split theme (see [`Self::resolve_theme_name`]).
    pub(crate) fn applied_terminal_config_for(
        &self,
        appearance: aterm_types::Appearance,
    ) -> aterm_core::config::TerminalConfig {
        let mut tc = self.terminal_config_for(appearance).unwrap_or_default();
        let theme = self.theme_for(appearance);
        let rgb = |c: u32| {
            Rgb::new(
                ((c >> 16) & 0xff) as u8,
                ((c >> 8) & 0xff) as u8,
                (c & 0xff) as u8,
            )
        };
        tc.default_foreground = rgb(theme.fg);
        tc.default_background = rgb(theme.bg);
        tc
    }
}

/// Resolve the config file path without creating anything.
/// The `font_px_explicit` pin after a config reload: an explicit `$ATERM_FONT_PX` /
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

/// Load the user config. A missing file is fine (defaults); a malformed file is
/// reported and ignored rather than aborting the launch.
pub(crate) fn load_config() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Config::default(); // not present / unreadable → defaults
    };
    toml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("aterm-gui: ignoring invalid config {}: {e}", path.display());
        Config::default()
    })
}

/// Resolve the glyph size in physical px with the canonical precedence
/// `$ATERM_FONT_PX > config.font_px > FONT_PX default`, clamped to the sane
/// `FONT_PX_MIN..=FONT_PX_MAX` bounds. Shared by startup (`main`) and live
/// hot-reload (`App::reload_config`) so a reload re-applies the SAME precedence —
/// an env override still wins after the user edits the config file.
pub(crate) fn resolve_font_px(config: &Config) -> f32 {
    resolve_font_px_with(
        std::env::var("ATERM_FONT_PX").ok().as_deref(),
        config.font_px,
    )
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

/// An explicit render-scale override from `$ATERM_FORCE_SCALE` (set directly or by
/// the `--scale` flag). `Some(f)` for a finite, positive value; `None` when unset
/// or invalid. When set it overrides BOTH the headless 1.0 default and a real
/// window's `scale_factor()`, driving the auto-scaled font (`round(FONT_PX·f)`) and the
/// interior padding (`pad_for_scale(f)`) so an offscreen `image` capture renders at
/// the same DPI a real window of that scale would (e.g. `--scale 2` ≈ 2× Retina).
pub(crate) fn resolve_force_scale() -> Option<f64> {
    std::env::var("ATERM_FORCE_SCALE")
        .ok()?
        .parse::<f64>()
        .ok()
        .filter(|f| f.is_finite() && *f > 0.0)
}

/// Pure precedence core for [`resolve_font_px`], with the `$ATERM_FONT_PX` env
/// value and the config value passed in explicitly so it is deterministically
/// unit-testable (no process-global env mutation). Order: a finite, in-range env
/// value wins; else a finite, in-range config value; else the built-in default.
/// A present-but-unparseable/out-of-range env value falls through to the config,
/// matching the startup `.parse().ok().or(config).filter(in_range)` chain.
pub(crate) fn resolve_font_px_with(env: Option<&str>, config: Option<f32>) -> f32 {
    let in_range = |p: &f32| p.is_finite() && *p >= FONT_PX_MIN && *p <= FONT_PX_MAX;
    // Filter EACH source by range independently so an out-of-range env value falls
    // through to a valid config value (as documented) instead of `.or(config)`
    // pinning the bad env value and then `.filter` collapsing straight to default.
    env.and_then(|s| s.parse::<f32>().ok())
        .filter(&in_range)
        .or(config.filter(&in_range))
        .unwrap_or(FONT_PX)
}

/// Max HUD rows a `win_rows`-tall window can show below a `strip`-row tab strip,
/// always leaving at least one terminal row. The bottom of the HUD stack is dropped
/// past this so the composed frame never exceeds the window (no off-glass clip).
pub(crate) fn hud_cap_for(win_rows: u16, strip: u16) -> u16 {
    win_rows.saturating_sub(strip).saturating_sub(1)
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
        let terminal_config = app.config.applied_terminal_config_for(app.os_appearance);
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
    /// config + force-off flag. Runs only when `sparkle_dirty` is set (startup,
    /// config reload, toggle) — never per frame — so the per-frame path neither
    /// re-resolves config nor recompiles the lexicon. A malformed user lexicon
    /// override is logged and the builtin is used (config is not discarded).
    pub(crate) fn recompute_sparkle(&mut self) {
        self.sparkle_dirty = false;
        // Fingerprint the path-referenced feed files as consumed by THIS rebuild
        // (the single writer of `path_feed_fps`): the reload dedupe compares
        // against it to keep touch-to-reload alive — a byte-equal `aterm.toml`
        // re-save must still pick up pack/lexicon FILE edits (see
        // `refresh_path_feeds`).
        // (The Trail Pack registry itself now lives in the versioned
        // `config_assets` catalog and is rebuilt by the config service, not
        // here.)
        self.path_feed_fps = self.config.path_feed_fingerprints();
        self.sparkle = if self.sparkle_force_off
            || !self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::WordDecorations)
        {
            None
        } else {
            self.config
                .sparkle_runtime_parts()
                .map(|(cfg, override_toml)| {
                    let langs = self.config.sparkle_languages();
                    let refs: Vec<&str> = langs.iter().map(String::as_str).collect();
                    let lexicon = aterm_lexicon::Lexicon::with_languages_and_override(
                        &refs,
                        override_toml.as_deref(),
                    )
                    .unwrap_or_else(|e| {
                        eprintln!(
                            "aterm-gui: sparkle_words lexicon override rejected ({e}); using builtin"
                        );
                        aterm_lexicon::Lexicon::with_languages(&refs)
                    });
                    // v3 §6: surface build-time data problems (a custom/extra word
                    // that can never scan as written — single-char CJK without the
                    // opt-in, mixed-script surfaces — or a cross-class collision)
                    // instead of silently accepting the config. Filtered by the
                    // RESOLVED scan options (the lexicon cannot see them): a
                    // single-char-CJK warning is satisfied — not a problem — once
                    // `cjk_single_char = true` is set.
                    for warning in sparkle_logged_warnings(lexicon.conflicts(), cfg.cjk_single_char)
                    {
                        eprintln!("aterm-gui: sparkle_words lexicon: {warning}");
                    }
                    crate::word_decorations::Resolved {
                        cfg,
                        lexicon: std::sync::Arc::new(lexicon),
                    }
                })
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

    /// Flip the in-memory sparkle-words master kill (the `toggle_sparkle_words`
    /// action / menu item) — an instant panic-off that overrides config without a
    /// TOML edit. Marks the cache stale and flushes per-window occurrence state so
    /// the next frame reflects the change.
    pub(crate) fn toggle_sparkle_words(&mut self) {
        self.sparkle_force_off = !self.sparkle_force_off;
        self.sparkle_dirty = true;
        for ws in self.windows.values_mut() {
            // v3 §1.1 reset table: master toggle off→on is a hard_reset —
            // fresh start is user intent, done marks clear too.
            ws.word_decos.hard_reset();
            ws.deco_scratch.clear();
            // The panic-off kills EVERY v2 surface too: a stale ink scratch would
            // otherwise recolor glyphs (and stale cat sprites keep a cat) for
            // one more frame.
            ws.ink_scratch.clear();
            ws.free_scratch.clear();
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
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
    /// effective=<bool>`. `Err` when no window is focused or the front content
    /// is not a terminal (native tabs have no session to toggle) — an honest
    /// refusal, never a silent no-op.
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
        Ok(format!(
            "config_enabled={} session_override={} effective={}",
            self.config.matrix_rain_enabled(),
            over,
            self.session_rain_enabled(sid),
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
    pub(crate) fn on_resize_throttled(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        let now = std::time::Instant::now();
        let apply_now = self.windows.get(&wid).is_none_or(|ws| {
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
        // windowed chrome'd sample of 0 or beyond the sanity cap keeps the
        // previous accepted band instead of committing the artifact.
        if cfg!(target_os = "macos")
            && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone())
        {
            let fullscreen = w.fullscreen().is_some();
            let decorated = w.is_decorated();
            let measured_pts = self.apprt.titlebar_band_pts(&w);
            let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale);
            let head_pts = accept_titlebar_band_pts(
                measured_pts,
                fullscreen,
                decorated,
                self.windows.get(&wid).map_or(0.0, |ws| ws.head_pts),
            );
            let head = (head_pts * scale).round() as usize;
            if let Some(ws) = self.windows.get_mut(&wid)
                && (ws.head_pts != head_pts || ws.metrics.head != head)
            {
                ws.head_pts = head_pts;
                ws.metrics.head = head;
                self.backend.set_head(head);
            }
        }
        let (rows, cols, hud_cap) = self.grid_dims_for(wid, size);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.hud_cap = hud_cap;
        }
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

    /// The grid `(rows, cols, hud_cap)` window `wid` gets for raw window `size` — the
    /// PURE half of [`Self::on_resize`] (which also applies it), shared with
    /// [`Self::apply_window_scale`]'s safety-net gate so both derive from the ONE law.
    ///
    /// W12: grids THIS window from ITS OWN cell metrics (mixed-DPI) — a resize of a
    /// background, different-DPI window must divide by that window's cell box, not
    /// whichever window the shared renderer is currently activated to. The grid
    /// occupies the window MINUS its independent top/bottom interior borders —
    /// the inverse of `frame_px`. The `0..cell-1` remainder is absorbed into
    /// theme-bg bands at present time, so the swapchain can be the RAW window size
    /// and the compositor never rescales. The tab strip (top) and HUD stack
    /// (bottom) rows are reserved out of the terminal grid, FIT TO THE WINDOW (≥1
    /// terminal row; HUD rows drop before the frame would exceed the glass —
    /// `hud_cap`).
    fn grid_dims_for(&self, wid: WindowId, size: PhysicalSize<u32>) -> (u16, u16, u16) {
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
        let hud_cap = hud_cap_for(win_rows, self.tab_strip_rows);
        let eff_hud = self.hud_rows.min(hud_cap);
        let rows = win_rows
            .saturating_sub(self.tab_strip_rows)
            .saturating_sub(eff_hud)
            .max(1);
        (rows, cols, hud_cap)
    }

    /// `hud_rows` = the TOTAL bottom rows reserved by the HUD = the sum of each ENABLED
    /// widget's row count (Resources reserves 3, Engine 1). Kept in sync after any toggle
    /// / config reload.
    pub(crate) fn recompute_hud_rows(&mut self) {
        self.hud_rows = self
            .panels
            .iter()
            .filter(|p| p.enabled())
            .map(|p| p.rows())
            .sum();
    }

    /// Whether the panel with `id` is currently enabled (for menu state + toggles).
    #[must_use]
    pub(crate) fn panel_enabled(&self, id: hud_bar::PanelId) -> bool {
        self.panels.iter().any(|p| p.id() == id && p.enabled())
    }

    /// Toggle a HUD panel on/off, re-gridding every window so the terminal grid
    /// releases / reclaims the panel's bottom row (the bottom analog of changing
    /// `tab_strip_rows`). Shared by the View-menu items and config reload. No-op when
    /// already in the requested state.
    pub(crate) fn set_panel(&mut self, id: hud_bar::PanelId, on: bool) {
        let changed = self.panels.iter_mut().any(|p| {
            if p.id() == id && p.enabled() != on {
                p.set_enabled(on);
                true
            } else {
                false
            }
        });
        if !changed {
            return;
        }
        self.recompute_hud_rows();
        // Re-grid each window from its own OS size (the HUD now takes/frees rows),
        // forcing a fresh present so the band appears/disappears immediately.
        let sized: Vec<(WindowId, PhysicalSize<u32>)> = self
            .windows
            .iter_mut()
            .filter_map(|(wid, ws)| {
                ws.last_present = None;
                ws.next_hud_tick = None; // re-armed by about_to_wait if now on
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

    /// Re-pin every App-owned renderer setting onto a freshly constructed or
    /// re-faced backend. Backend construction starts from defaults; keeping this
    /// as one funnel prevents startup, zoom, first-surface fallback, and runtime
    /// device-loss fallback from silently losing different subsets of typography,
    /// contrast, opacity, or configured faces.
    pub(crate) fn pin_backend_render_config(&mut self) {
        self.backend.set_text_shaping(self.text_shaping.clone());
        self.backend.set_text_blending(self.text_blending);
        self.backend.set_font_thicken(self.font_thicken);
        self.backend.set_stem_gamma(self.stem_gamma);
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
        self.backend
            .set_background_opacity(self.render_knobs.background_opacity);
        if !self.backend.is_gpu() {
            if self.render_knobs.background_opacity < 1.0 {
                warn_background_opacity_unimplemented_once();
            }
            if self.render_knobs.background_material != BackgroundMaterial::None {
                warn_background_material_unimplemented_once();
            }
        }
        self.apply_font_config();
    }

    /// Rebuild the [`Backend`] from the CURRENT `self.font_px` + `self.theme`,
    /// re-grid the window for the new cell metrics, and repaint. The single proven
    /// rebuild path shared by live font-zoom ([`Self::set_font_px`]) and live
    /// config hot-reload ([`Self::reload_config`]) — a font-size OR theme change.
    /// A failed rebuild (GPU hiccup / no font) keeps the current backend, so a
    /// reload/zoom never crashes. No-op re-grid without a window (headless).
    #[must_use = "a failed rebuild leaves the prior renderer live and requires caller policy"]
    pub(crate) fn rebuild_backend(&mut self) -> bool {
        // Preserve the interior padding across the rebuild — a fresh backend starts
        // at `pad == 0`, so a font-zoom / config-reload would otherwise drop the
        // border. (The pad is a device-px constant for the session's scale; it does
        // not change with the font size.) Ditto the chrome headroom: a device-px
        // constant for the window's titlebar band, independent of the font.
        let pad = self.backend.pad();
        let pad_top = self.backend.pad_top();
        let head = self.backend.head();
        match self.backend.ready_mut() {
            Backend::Gpu(g) => {
                // In-place: keep the device + EVERY window's swapchain. Dropping the
                // device would orphan every other window's surface, so the GPU path
                // rebuilds the font/theme on the SAME device.
                //
                // Resolve the candidate face before publishing either family or
                // theme. A discovery failure therefore leaves the old GPU face AND
                // its configured-family authority untouched, matching the CPU
                // candidate-build path below.
                if let Err(e) =
                    g.set_font_family_theme(self.font_family.clone(), self.font_px, self.theme)
                {
                    eprintln!("aterm-gui: GPU font/theme rebuild failed: {e}");
                    return false; // keep the current backend; never crash a zoom/reload
                }
            }
            Backend::Cpu(_) => {
                // The CPU renderer owns no device, so a full rebuild is free and safe.
                let Some(backend) = build_backend(
                    self.font_px,
                    self.use_gpu,
                    self.theme,
                    self.font_family.as_deref(),
                ) else {
                    return false;
                };
                self.backend = crate::BackendSlot::Ready(backend);
            }
        }
        self.backend.set_pad(pad);
        self.backend.set_pad_top(pad_top);
        self.backend.set_head(head);
        // Re-apply every configured shaping/typography/render/font setting. In
        // particular line_height lands before the re-grid below.
        self.pin_backend_render_config();
        // The atlas/face changed, so every window's offscreen + dirty-gate are stale.
        // Reset the per-window GPU caches (the swapchain stays valid — same device) and
        // the introspection scratch, and force a repaint. NOTE: the swapchains and OS
        // windows are untouched, so no surface is orphaned.
        self.introspect_gpu = aterm_gpu::WindowGpu::new();
        for ws in self.windows.values_mut() {
            if let Some(
                PresentTarget::Gpu { window_gpu, .. } | PresentTarget::Virtual { window_gpu },
            ) = &mut ws.present
            {
                // M3 phase B: the per-screen EDR headroom survives the cache
                // reset — it is OS state keyed to the window's monitor, not a
                // render cache (re-queried only on real monitor changes).
                let edr = window_gpu.edr_max();
                *window_gpu = aterm_gpu::WindowGpu::new();
                window_gpu.set_edr_max(edr);
            }
            ws.last_present = None;
            // Headless has no winit redraw edge to acknowledge: image capture
            // renders synchronously, while an in-flight Virtual recording owns
            // its own paced timer. Reopen its gate without manufacturing a fake
            // outstanding OS request. Windowed targets use the coupled helper
            // after their geometry is rebuilt below.
            if ws.os_window.is_none() {
                let _ = ws.present_retry.on_external_stimulus();
            }
        }
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
            if let Some(ws) = self.windows.get_mut(&wid) {
                if size.width > 0 && size.height > 0 {
                    ws.win_px = Some(size);
                }
                if let Some(w) = ws.os_window.as_ref() {
                    w.set_resize_increments(Some(PhysicalSize::new(cw as u32, ch as u32)));
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
        self.sync_chrome_fonts();
        // M5: a rebuild re-pinned the bg opacity on the new face; keep the
        // window-level vibrancy (backdrop + opacity flip) in step on the GPU
        // backend. Idempotent — no-op at the solid default / off macOS.
        if self.backend.is_gpu() {
            self.apply_window_vibrancy();
        }
        true
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
                self.backend.set_background_opacity(v);
                // M5 TRUE VIBRANCY: on the GPU backend the translucent present path
                // IS wired (PostMultiplied swapchain + NSVisualEffectView), so a
                // live opacity edit re-applies the window/Metal-layer opacity flip
                // and backdrop (`self.render_knobs` already holds the new value).
                // The CPU softbuffer surface is opaque with no non-opaque composite,
                // so a translucent value there stays honestly solid — the warn-once.
                if self.backend.is_gpu() {
                    self.apply_window_vibrancy();
                } else if v < 1.0 {
                    warn_background_opacity_unimplemented_once();
                }
            }
            // The window-level NSVisualEffectView backdrop is driven from the
            // resolved knobs: on the GPU backend re-apply it live (it only shows
            // when the window is also translucent); on the CPU backend it has no
            // consumer, so a non-`none` material warns once.
            KnobChange::BackgroundMaterial(m) => {
                if self.backend.is_gpu() {
                    self.apply_window_vibrancy();
                } else if m != BackgroundMaterial::None {
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

    /// Push the W6 per-style / fallback font config (`self.font_config`, the
    /// resolved source of truth) onto the live backend: the injected real-bold
    /// face + styled italic/bold-italic slots, the synthetic-style flag, and
    /// the config fallback / symbol / emoji chains. Idempotent for the chain
    /// and flag setters (they no-op on equal values), and called after every
    /// backend (re)build — the same re-pin discipline as `set_text_shaping` —
    /// so a zoom / reload / family change never reverts the user's fonts. A
    /// styled file that fails to read/parse warns and is skipped (never a
    /// crash; the other faces still apply).
    pub(crate) fn apply_font_config(&mut self) {
        let fc = self.font_config.clone();
        self.backend.set_synthetic_styles(fc.synthetic_style);
        // font_family_bold rides the existing injected-real-bold seam
        // (`set_bold_font`, previously reachable only from the web hosts);
        // italic / bold-italic fill the renderer's styled sibling slots.
        let styled: [(&str, Option<&String>); 3] = [
            ("font_family_bold", fc.styled_paths[0].as_ref()),
            ("font_family_italic", fc.styled_paths[1].as_ref()),
            ("font_family_bold_italic", fc.styled_paths[2].as_ref()),
        ];
        for (slot, (key, path)) in styled.into_iter().enumerate() {
            let Some(path) = path else { continue };
            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("aterm-gui: config {key}: cannot read {path:?} ({e}); ignored");
                    continue;
                }
            };
            let res = if slot == 0 {
                self.backend.set_bold_font(&bytes)
            } else {
                self.backend.set_styled_font(slot, &bytes)
            };
            if let Err(e) = res {
                eprintln!("aterm-gui: config {key}: {path:?} rejected ({e}); ignored");
            }
        }
        self.backend.set_config_fallback_fonts(&fc.fallback_fonts);
        self.backend
            .set_config_symbol_font(fc.symbol_font.as_deref());
        self.backend.set_config_emoji_font(fc.emoji_font.as_deref());
        // W9: the variable-font instantiation config (requests + dark nudge),
        // same re-pin discipline — a fresh face resolved only the DEFAULT
        // instantiation, so pin the user's requests before the caller
        // re-grids. A default config is a free no-op (the setter no-ops on
        // equal values and a non-variable primary resolves to nothing).
        let reqs = self.font_variations.clone();
        self.backend
            .set_font_variations(&reqs, self.font_weight_dark_nudge);
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
                w.request_redraw();
            }
            // Keep the native toolbar strip's appearance pinned to the (possibly
            // flipped) theme darkness, so tab labels stay legible on the new backdrop.
            if let Some(handle) = toolbars.get(wid) {
                crate::toolbar::set_strip_dark(handle, strip_dark);
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
            let (rows, cols, _) = self.grid_dims_for(wid, size);
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
    /// fresh content fingerprints against the applied ones
    /// ([`App::path_feed_fps`], written only by [`App::recompute_sparkle`]):
    ///
    /// * unchanged ⇒ the reload stays a FULL no-op — the dedupe win is kept
    ///   intact (no engine re-diffs, no word-deco reset, no settings-popup
    ///   churn) for the settings-commit + watcher double reload;
    /// * changed ⇒ arm `sparkle_dirty` so the pre-frame recompute re-reads
    ///   every feed (lexicon, toy packs, trail-pack registry — one rebuild
    ///   covers all three), hard-reset the per-window word decorations ONLY
    ///   when the deco feed (lexicon/toys) changed — a trail-manifest edit
    ///   feeds cursor glow, never decorations — and request repaints so the
    ///   rebuild lands without waiting for organic damage. The remaining
    ///   reload side-effect storm is still skipped: the parsed config is
    ///   identical, so nothing else can have changed.
    fn refresh_path_feeds(&mut self, fresh: PathFeedFps) {
        if fresh == self.path_feed_fps {
            return;
        }
        self.sparkle_dirty = true;
        if fresh.deco != self.path_feed_fps.deco {
            // v3 §1.1 reset table: a lexicon rebuild is a hard_reset, matching
            // the changed-config path's `sparkle_feed_changed` arm.
            for ws in self.windows.values_mut() {
                ws.word_decos.hard_reset();
            }
        }
        for ws in self.windows.values() {
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Live config hot-reload (`Wake::ConfigReload`): the user edited
    /// `~/.config/aterm/aterm.toml` and the watcher saw its mtime change. Re-read +
    /// VALIDATE the file, then apply the new settings to every live session
    /// WITHOUT a restart.
    ///
    /// VALIDATION / FAIL-SAFE: `load_config` is the same parser the startup path
    /// uses — a malformed or partial mid-edit file fails to parse, is logged, and
    /// yields `Config::default()`. We must NOT clobber the running config with
    /// those defaults, so a parse failure is detected (re-read the raw text and
    /// re-parse strictly) and the reload is REJECTED, leaving every session
    /// exactly as it was. A missing/unreadable file is treated the same as a parse
    /// failure here: a reload that produced all-defaults is a no-op against the
    /// live state rather than a silent reset to built-ins.
    ///
    /// PRECEDENCE (no regression): font size flows through [`resolve_font_px`] —
    /// the SAME `$ATERM_FONT_PX > config > default` order as startup — so an env
    /// override still wins after an edit. GPU is a launch-time decision and is NOT
    /// hot-swapped here (`self.use_gpu` is fixed); only font size, the renderer
    /// theme, and the engine `TerminalConfig` (scrollback/cursor/colours/palette,
    /// diffed by `Terminal::apply_config`) are re-applied.
    pub(crate) fn reload_config(&mut self) {
        // Re-read + strictly re-parse. A parse error (malformed/partial mid-edit
        // file) or an unreadable/absent file is REJECTED so the live config is
        // never replaced by defaults; the previous config stays intact.
        let Some(path) = config_path() else { return };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                aterm_log::warn!(
                    "config reload: {} unreadable ({e}); keeping current config",
                    path.display()
                );
                return;
            }
        };
        let config: Config = match toml::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                aterm_log::warn!(
                    "config reload: {} is invalid ({e}); keeping current config",
                    path.display()
                );
                return;
            }
        };
        let config_snapshot = match self.sync_native_config_external(text.clone()) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                // A durable Settings write owns the config lane. Its completion
                // re-wakes reload after the queued external text is admitted.
                return;
            }
            Err(error) => {
                aterm_log::warn!("native config service rejected watcher snapshot: {error}");
                return;
            }
        };

        // DEDUPE (the double-reload storm): an in-panel Settings commit writes
        // the file AND posts `Wake::ConfigReload` immediately, then the mtime
        // watcher posts a SECOND reload within ~500 ms for the SAME bytes.
        // Re-applying an identical config is pure side-effect churn — engine
        // re-diffs on every tab, per-window word-deco hard resets, settings
        // popup cancels, a possible font/backend probe — so a parse that
        // EQUALS the currently applied config stops here (the native snapshot
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
        let fresh_feeds = config.path_feed_fingerprints();
        if config == self.config {
            self.refresh_path_feeds(fresh_feeds);
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
        // `hard_reset` warranted; (b) whether the Settings overlay's editable
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
        // Retain the parsed config so a later OS light↔dark switch can re-resolve a
        // `dark:…,light:…` split theme without re-reading disk (see
        // `App::sync_app_theme_to_appearance`). Resolve the engine/renderer theme for
        // the CURRENT OS appearance so a reload preserves the active light/dark side.
        self.config = config.clone();
        // Serious mode is an effective App projection, not a rewrite of any
        // individual effect setting. Apply its edge immediately after publishing the
        // new source config so a disable restores values from THIS generation.
        self.apply_serious_mode(config.serious_mode_or_default());
        self.config_assets = std::sync::Arc::clone(&config_snapshot.assets);
        self.publish_native_config_snapshot(&config_snapshot);
        // Smart-title Settings are live authority, not restart-only metadata: revoke
        // queued provider work immediately and repaint tab/window composition even if
        // every terminal is currently idle.
        self.reconfigure_title_summaries();
        // Per-keystroke config caches (predictive_echo / cursor_trail_style): a reload
        // can change either, so drop the resolved values — they re-resolve on the next
        // keystroke. Keeps a live style/predict change taking effect immediately.
        self.predict_mode_cache = None;
        self.nyan_style_cache = None;
        // File IO + PNG decode belong to the pre-armed sprite worker. Config
        // reload merely publishes the newest raw path through a bounded,
        // nonblocking channel; any older completion is generation-rejected.
        crate::nyan_sprite_loader::sync_config(self);
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
        // v3 §1.1 reset table: config reload / lexicon rebuild is a hard_reset
        // (matches the web knob setters' parity arm) — but ONLY when the keys
        // that actually feed the decorations changed (`sparkle_feed_changed`
        // above). An unrelated edit (a cursor-trail style switch, a font tweak)
        // must not wipe every window's live decorations mid-animation — the
        // collateral half of the double-reload audit.
        if sparkle_feed_changed {
            for ws in self.windows.values_mut() {
                ws.word_decos.hard_reset();
            }
        }
        // Keep any OPEN Settings overlay authoritative against the freshly-loaded config:
        // a live hot-reload (the file watcher OR an in-panel edit's own
        // `Wake::ConfigReload`) rebuilds the displayed control list from the new
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
        let applied_tc = config.applied_terminal_config_for(self.os_appearance);
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
        // W5h: an unresolvable `font_family` warns (like themes) instead of
        // silently reducing to the built-in candidates. Uses the same
        // effective family (env > config > platform default) the rebuild will try.
        let effective_family_now = crate::effective_font_family(config.font_family.as_deref());
        if let Some(w) = Config::font_family_warning(effective_family_now.as_deref()) {
            warns.push(format!("config {w}"));
        }
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
        if let Some(reason) = config_snapshot.assets.nyan_sprite.diagnostic() {
            let source = config_snapshot
                .assets
                .nyan_sprite
                .source_id()
                .unwrap_or("configured source");
            warns.push(format!(
                "config cursor_nyan_sprite {source:?} invalid: {reason}"
            ));
        }
        // W6: re-resolve the per-style / fallback font keys (families → paths),
        // riding the same banner for unresolvable entries. The diff against the
        // cached resolved config decides below whether the backend is touched.
        let (new_font_config, font_cfg_warns) = FontConfig::from_config(&config);
        warns.extend(font_cfg_warns);
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
        // effect on the next selection (dropping the key reverts to the off default).
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

        // HUD panels are bottom chrome: toggling any re-grids the window between the
        // terminal and the HUD stack, exactly like the tab strip above. The master
        // `show_hud` gates every panel: master off ⇒ the whole band drops, while the
        // per-panel keys keep their values for when it comes back.
        let hud_master = config.show_hud_or_default();
        for id in hud_bar::PanelId::ALL {
            let want = hud_master && config.hud_enabled(id);
            if want != self.panel_enabled(id) {
                self.set_panel(id, want);
            }
        }

        // Window chrome appearance (titlebar light/dark/auto): re-apply live so a
        // `window_theme` edit takes effect without a restart, AND update the cached
        // field so windows opened AFTER the reload use the new value (attach reads
        // `self.window_theme`). No-op off macOS. Light/Dark re-apply is idempotent;
        // the Auto branch clears the forced appearance (see `window_set_appearance`).
        let new_window_theme = config.window_theme_or_default();
        if new_window_theme != self.window_theme {
            self.window_theme = new_window_theme;
        }
        {
            // On Windows, Auto resolves to match the terminal body (see
            // `window_theme_for_chrome`); elsewhere this is the raw config value.
            let chrome_theme = self.window_theme_for_chrome();
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
            for ws in self.windows.values() {
                if let (Some(w), Some(PresentTarget::Gpu { gpu_surface, .. })) =
                    (ws.os_window.as_ref(), ws.present.as_ref())
                {
                    apprt.window_set_surface_colorspace(
                        w,
                        crate::platform::resolve_surface_colorspace(
                            new_colorspace,
                            gpu_surface.is_hdr(),
                        ),
                    );
                }
            }
        }

        // M3 phase B: the EDR glow opt-in, live. Turning it OFF gates the >1.0
        // aurora pass off on the NEXT present of every window (the plan follows
        // `hdr_glow` per frame — proven safe on a still-f16 swapchain, whose blit
        // keeps linear-decoding with the grid clamped at reference white).
        // Turning it ON affects windows opened afterwards: an existing 8-bit
        // swapchain is never re-formatted live, and the plan keeps every HDR arm
        // off for it (the exhaustive hdr_gate law).
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
        let new_theme = config.theme_for(self.os_appearance);
        // Re-derive the AUTO default font with the SAME HiDPI logic
        // `attach_os_window` / `on_scale_factor_changed` use, so editing an
        // unrelated key (e.g. a colour) on a Retina display does NOT shrink the
        // font back to the FONT_PX (12px) base. An explicit env/config font is honored
        // verbatim (and re-pins `font_px_explicit`).
        let font_explicit_now =
            std::env::var_os("ATERM_FONT_PX").is_some() || config.font_px.is_some();
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
        let effective_family = crate::effective_font_family(config.font_family.as_deref());
        let family_changed = effective_family != self.font_family;
        // Text shaping (ligatures / font_features). Update the source of truth BEFORE
        // a possible rebuild so `rebuild_backend` re-applies the NEW shaping; if no
        // rebuild is needed we push it through directly below.
        let new_shaping = config.text_shaping();
        let shaping_changed = new_shaping != self.text_shaping;
        self.text_shaping = new_shaping;
        // W2 typography knobs (`text_blending` / `font_thicken` / `stem_gamma`):
        // same source-of-truth-before-rebuild discipline. All three change glyph
        // APPEARANCE but not cell CONTENT, so a knob-only edit rides the shaping
        // branch below (backend push + per-window present-cache invalidation).
        let new_blending = config.text_blending_or_default();
        let new_thicken = config.font_thicken_or_default();
        let new_stem_gamma = config.stem_gamma_or_default();
        let typography_changed = new_blending != self.text_blending
            || new_thicken != self.font_thicken
            || (new_stem_gamma - self.stem_gamma).abs() > f32::EPSILON;
        self.text_blending = new_blending;
        self.font_thicken = new_thicken;
        self.stem_gamma = new_stem_gamma;
        // W5 renderer knobs: PURE diff — one KnobChange per changed key, each
        // routed to exactly one renderer call (`apply_render_knob`). A
        // line_height change alters the CELL GEOMETRY (every window re-grids),
        // so it takes the full rebuild path below — which re-pins all knobs —
        // while the others ride the appearance-only push branch.
        let new_knobs = RenderKnobs::from_config(&config);
        let knob_changes = self.render_knobs.diff(&new_knobs);
        self.render_knobs = new_knobs;
        // W6 font config: update the source of truth BEFORE a possible rebuild
        // (rebuild_backend re-applies it); a font-config-only edit rides the
        // appearance push branch below.
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
        let backend_rebuild_needed =
            font_changed || family_changed || geometry_knob_changed || variations_changed;
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
            let rebuild_succeeded = self
                .finish_reload_render_transaction_with(reload_render_before, Self::rebuild_backend);
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
                    }
                }
            }
        } else if theme_changed {
            // Colour-only edit (font px + family unchanged): the face and glyph
            // atlas stay valid, so the full rebuild — primary-font re-discovery,
            // fontdue re-parse, atlas drop, per-window re-grid — would be pure
            // waste on the event loop. Push the theme onto the LIVE backend
            // instead; every colour-save while iterating on a scheme (and every
            // settings-overlay colour edit, which posts its own ConfigReload) is
            // then a repaint, not a font parse. ENRICHED with our appearance knobs:
            // a single save can flip the theme AND a shaping/typography/font-config/
            // render knob, and none of those force a rebuild — apply them live here
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
            }
            // W5: each changed knob → exactly one renderer call (LineHeight is
            // unreachable here — it takes the rebuild branch above).
            for &change in &knob_changes {
                self.apply_render_knob(change);
            }
            // W6: a per-style / fallback font edit re-pins the resolved config.
            if font_cfg_changed {
                self.apply_font_config();
            }
            self.apply_theme_live(new_theme);
        } else if shaping_changed
            || typography_changed
            || font_cfg_changed
            || !knob_changes.is_empty()
        {
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
            // W5: each changed knob → exactly one renderer call (LineHeight is
            // unreachable here — it takes the rebuild branch above).
            for &change in &knob_changes {
                self.apply_render_knob(change);
            }
            // W6: a per-style / fallback font edit re-pins the resolved config
            // (glyph appearance changed, cell content did not — the setters
            // drop the shared glyph caches / GPU atlas where needed, and the
            // per-window present caches are invalidated just below).
            if font_cfg_changed {
                self.apply_font_config();
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
            && (shaping_changed
                || typography_changed
                || font_cfg_changed
                || !knob_changes.is_empty())
        {
            self.sync_chrome_fonts();
        }
        // Surface `font_features` that can't take effect, now that the new shaping is
        // on the backend — a no-op becomes a visible hint instead of silent confusion.
        self.warn_font_feature_issues();
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
    use super::{Config, MAX_NYAN_SPRITE_FILE_BYTES, NyanSpriteAsset};
    use aterm_core::config::BiDiMode;

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    fn nyan_fixture(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "aterm-nyan-asset-{}-{}-{name}.png",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn config_with_nyan(path: &std::path::Path) -> Config {
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

    #[test]
    fn nyan_asset_resolves_once_to_shared_rgba_with_stable_identity() {
        let path = nyan_fixture("ready");
        let rgba = [
            0xff, 0x10, 0x20, 0xff, 0x20, 0xff, 0x30, 0x80, 0x10, 0x20, 0xff, 0x40, 0xff, 0xff,
            0xff, 0xff,
        ];
        let png = crate::app_introspect::encode_rgba8_png(&rgba, 2, 2).unwrap();
        std::fs::write(&path, png).unwrap();
        let config = config_with_nyan(&path);
        let first = config.resolve_asset_catalog();
        let second = config.resolve_asset_catalog();
        let (
            NyanSpriteAsset::Ready {
                source_id,
                w,
                h,
                rgba: resolved,
                fp,
            },
            NyanSpriteAsset::Ready { fp: second_fp, .. },
        ) = (&first.nyan_sprite, &second.nyan_sprite)
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
    fn nyan_asset_missing_or_oversized_source_is_explicit_invalid() {
        let missing = nyan_fixture("missing");
        let catalog = config_with_nyan(&missing).resolve_asset_catalog();
        assert!(matches!(
            &catalog.nyan_sprite,
            NyanSpriteAsset::Invalid { bounded_reason, .. }
                if bounded_reason.contains("unreadable")
        ));

        let oversized = nyan_fixture("encoded-limit");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len((MAX_NYAN_SPRITE_FILE_BYTES as u64) + 1)
            .unwrap();
        drop(file);
        let catalog = config_with_nyan(&oversized).resolve_asset_catalog();
        assert!(matches!(
            &catalog.nyan_sprite,
            NyanSpriteAsset::Invalid { bounded_reason, .. }
                if bounded_reason.contains("exceeds")
        ));
        let _ = std::fs::remove_file(oversized);
    }

    #[test]
    fn nyan_asset_dimension_cap_fails_closed() {
        let path = nyan_fixture("dimension-limit");
        let rgba = vec![0x7f; (super::MAX_NYAN_SPRITE_DIMENSION + 1) * 4];
        let png = crate::app_introspect::encode_rgba8_png(
            &rgba,
            (super::MAX_NYAN_SPRITE_DIMENSION + 1) as u32,
            1,
        )
        .unwrap();
        std::fs::write(&path, png).unwrap();
        let catalog = config_with_nyan(&path).resolve_asset_catalog();
        assert!(matches!(
            &catalog.nyan_sprite,
            NyanSpriteAsset::Invalid { bounded_reason, .. }
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
    fn bidi_explicit_and_case_insensitive() {
        let tc = cfg("bidi = \"Explicit\"").terminal_config().unwrap();
        assert_eq!(tc.bidi.mode, BiDiMode::Explicit);
    }

    #[test]
    fn ambiguous_width_wide_maps_to_double() {
        let tc = cfg("ambiguous_width = \"wide\"").terminal_config().unwrap();
        assert!(tc.ambiguous_width_double);
        let tc = cfg("ambiguous_width = \"narrow\"")
            .terminal_config()
            .unwrap();
        assert!(!tc.ambiguous_width_double);
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

    /// Sparkle words are ON by default with EVERY category — an absent `[sparkle_words]`
    /// table materializes the defaults (profanity + feline + orca + bare-`cat`), and an
    /// explicit `enabled = false` still fully opts out.
    #[test]
    fn sparkle_words_default_on_all_categories() {
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
        // v2.1 feline sub-keys: peeking cat + idle life + gaze + magic all
        // default ON (§10).
        assert_eq!(deco.feline_style, crate::word_decorations::FelineStyle::Cat);
        assert!(deco.feline_idle, "blink/twitch one-shots on by default");
        assert!(deco.feline_gaze, "cursor gaze on by default");
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

    /// FIX IV regression: the native style match is case-INsensitive, like
    /// the web `set_sparkle_profanity` setter — `style = "Nova"` selects the
    /// classic nova (it must not silently become Rainbow + supernova roll),
    /// `"SPARKLE"` the v1 sparkle.
    #[test]
    fn sparkle_profanity_style_matches_case_insensitively() {
        let deco = cfg("[sparkle_words.profanity]\nstyle = \"Nova\"")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(
            deco.profanity_style,
            crate::word_decorations::ProfanityStyle::Nova,
            "\"Nova\" resolves to the classic nova, not Rainbow"
        );
        let deco = cfg("[sparkle_words.profanity]\nstyle = \"SPARKLE\"")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(
            deco.profanity_style,
            crate::word_decorations::ProfanityStyle::Sparkle,
            "\"SPARKLE\" resolves to the v1 sparkle"
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

    /// `[sparkle_words.feline]` v2.1 keys round-trip: `style`/`idle`/`gaze`/
    /// `magic` parse, opt out independently, and unknown styles fall back to
    /// the peeking cat.
    #[test]
    fn sparkle_feline_v2_keys_round_trip() {
        let deco = cfg(
            "[sparkle_words.feline]\nstyle = \"paw\"\nidle = false\ngaze = false\nmagic = false",
        )
        .sparkle_deco_config()
        .expect("feline table keeps the feature on");
        assert_eq!(
            deco.feline_style,
            crate::word_decorations::FelineStyle::Paw,
            "style = \"paw\" selects the exact v1 path"
        );
        assert!(
            !deco.feline_idle,
            "idle = false ⇒ no one-shot deadlines ever arm"
        );
        assert!(
            !deco.feline_gaze,
            "gaze = false ⇒ centered pupils, no tracking"
        );
        assert!(!deco.feline_magic, "magic = false ⇒ ordinary builds only");
        // Unknown style values fall back to the peeking cat (documented §10).
        let deco = cfg("[sparkle_words.feline]\nstyle = \"lion\"")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(deco.feline_style, crate::word_decorations::FelineStyle::Cat);
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
        let c = cfg("[sparkle_words.feline]\nidle = true");
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
mod hud_fit_tests {
    use super::hud_cap_for;

    #[test]
    fn chrome_fits_the_window_and_never_clips() {
        // Plenty of room: all desired HUD rows fit.
        assert_eq!(hud_cap_for(100, 1), 98);
        // Exactly enough for a 4-panel stack + 1-row strip + 1 terminal row.
        assert_eq!(hud_cap_for(6, 1), 4);
        // One row too short for 4 panels → cap drops to 3 (bottom panel hidden).
        assert_eq!(hud_cap_for(5, 1), 3);
        // Window only fits terminal + strip → no HUD rows.
        assert_eq!(hud_cap_for(2, 1), 0);
        assert_eq!(hud_cap_for(1, 0), 0);

        // Invariant across sizes: terminal stays >=1 row AND the composed frame
        // (terminal + strip + effective HUD) never exceeds the window.
        for win in 1u16..=40 {
            for strip in 0u16..=2 {
                for desired_hud in 0u16..=4 {
                    let eff = desired_hud.min(hud_cap_for(win, strip));
                    let term = win.saturating_sub(strip).saturating_sub(eff).max(1);
                    assert!(term >= 1, "win={win} strip={strip}: terminal underflowed");
                    if win > strip {
                        assert!(
                            term + strip + eff <= win,
                            "win={win} strip={strip} hud={desired_hud}: frame {} > window",
                            term + strip + eff
                        );
                    }
                }
            }
        }
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
        app.hud_rows = 0;
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
        assert_eq!(app.grid_dims_for(wid, exact), (24, 80, 23));

        let remainder = ch.saturating_sub(1);
        let with_remainder = PhysicalSize::new(exact.width, exact.height + remainder as u32);
        assert_eq!(app.grid_dims_for(wid, with_remainder), (24, 80, 23));

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

    /// The engine config (palette + default bg) also tracks the split, so the live
    /// switch re-colours cells, not just the chrome.
    #[test]
    fn applied_terminal_config_tracks_split() {
        let c = cfg(r#"theme = "dark:Dracula,light:GitHub Light""#);
        let dark = c.applied_terminal_config_for(Appearance::Dark);
        let light = c.applied_terminal_config_for(Appearance::Light);
        assert_ne!(
            dark.default_background, light.default_background,
            "the engine default background must differ between the two sides"
        );
        // Light side's engine default bg is GitHub Light's white.
        assert_eq!(
            light.default_background,
            aterm_types::Rgb::new(0xff, 0xff, 0xff)
        );
    }
}

#[cfg(test)]
mod reload_dedupe_tests {
    use super::Config;

    fn cfg(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    /// The reload dedupe's equality gate (`reload_config`'s early return):
    /// two parses of the same content are EQUAL — so the mtime watcher's
    /// second `Wake::ConfigReload` for the bytes a Settings commit just wrote
    /// is a no-op — and equality is PARSE-level, not byte-level, so a
    /// comment/whitespace-only edit (or a bare `touch`) also dedupes; while a
    /// real one-key edit (the rapid trail-style switch) compares UNEQUAL and
    /// still applies.
    #[test]
    fn reload_dedupe_equality_matches_parsed_content_not_bytes() {
        let a = cfg("cursor_trail = true\ncursor_trail_style = \"fire\"\n");
        let same_bytes = cfg("cursor_trail = true\ncursor_trail_style = \"fire\"\n");
        let reformatted =
            cfg("# a comment\ncursor_trail = true\n\ncursor_trail_style = \"fire\"\n");
        let edited = cfg("cursor_trail = true\ncursor_trail_style = \"nyan rainbow\"\n");
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

    /// The BYTE-EQUAL reload path (`refresh_path_feeds`) — the touch-to-reload
    /// regression proof: (1) a double reload with NOTHING changed anywhere
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
        // The startup/render-loop step: consume the feeds + capture their
        // fingerprints (the applied state the dedupe compares against).
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
        app.recompute_sparkle(); // the pre-frame rebuild consumes (2)'s edit
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
        let b = cfg("cursor_trail_style = \"nyan rainbow\"\n");
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
        app.windows.get_mut(&wid).unwrap().last_strip_fp = Some((0xFEED, 80, false));

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
            Ok("config_enabled=false session_override=none effective=false"),
            "status is a pure read"
        );
        assert_eq!(
            app.rain_control(crate::RainCtlOp::On).as_deref(),
            Ok("config_enabled=false session_override=on effective=true"),
        );
        assert_eq!(
            app.rain_control(crate::RainCtlOp::Toggle).as_deref(),
            Ok("config_enabled=false session_override=off effective=false"),
        );
        app.config = cfg("[matrix_rain]\nenabled = true");
        app.rain_dirty = true;
        assert_eq!(
            app.rain_control(crate::RainCtlOp::Status).as_deref(),
            Ok("config_enabled=true session_override=off effective=false"),
            "the off override keeps winning over the enabled config"
        );
        assert_eq!(
            app.rain_control(crate::RainCtlOp::Off).as_deref(),
            Ok("config_enabled=true session_override=off effective=false"),
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
