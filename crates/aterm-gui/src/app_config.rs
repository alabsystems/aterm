// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Config subsystem: the `aterm.toml` model (`Config`), its loaders and
//! precedence resolvers (font px, force scale, grid size, tab-strip rows), and
//! the `App` methods that consume them (font, scale, backend, resize, reload,
//! fullscreen).

use aterm_core::terminal::{ColorPalette, CursorStyle, Rgb};
use aterm_render::Theme;
use winit::dpi::PhysicalSize;

use crate::input::{InputEvent, Source};
use crate::platform::AppRt;
use crate::{
    App, Backend, FONT_PX, FONT_PX_MAX, FONT_PX_MIN, PresentTarget, WindowId, keybinding, launch,
    term_lock,
};

/// Platform default for decoration drawn over content: `true` everywhere except
/// Windows, whose defaults are minimal and fast (decorative GPU presents on the
/// keystroke path cost typing latency there).
///
/// It governs `cursor_trail` (the resident cat is drawn over text). Keys that stay
/// out: `cursor_trail_bloom`, `hdr_glow` and `cursor_fire_shimmer` already emit
/// nothing with the trail off; `[sparkle_words] enabled` draws under text and is
/// capped (see `Config::sparkle_words_enabled_or_default`); `stream_fade` is off
/// everywhere and the remaining effects are opt-in.
///
/// An explicit value in `aterm.toml` always wins: every consumer is
/// `Option::unwrap_or(DEFAULT_DECORATIVE_EFFECTS)`.
pub(crate) const DEFAULT_DECORATIVE_EFFECTS: bool = !cfg!(windows);

/// User config file (`$XDG_CONFIG_HOME/aterm/aterm.toml`, else
/// `~/.config/aterm/aterm.toml`). Every field is optional; unknown keys are
/// ignored. Precedence is launch flag > config > built-in default; no environment
/// variable sets a key.
///
/// `PartialEq` lets config admission skip runtime side effects for a semantic
/// no-op; text, comments and asset generations are versioned separately by the
/// native config service.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    /// Glyph size in physical px (`--font-px` outranks it for one launch).
    pub(crate) font_px: Option<f32>,
    /// GPU rendering. Default on, with automatic CPU fallback when device init or
    /// the first surface fails; `gpu = false` (or `--cpu`) forces the CPU
    /// renderer, `--gpu` forces on.
    pub(crate) gpu: Option<bool>,
    /// Scrollback history limit, in lines (engine `TerminalConfig.scrollback_limit`;
    /// default 100 000). 0 means unlimited (bounded only by the memory budget).
    pub(crate) scrollback_lines: Option<usize>,
    /// Cursor shape: `"block"` (default) or `"bar"` (alias `"beam"`). Underline
    /// is not offered (programs may still request it via DECSCUSR).
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
    /// Motion policy: `"auto"` (default: follow Reduce Motion live on macOS,
    /// sample the Windows animations switch at window attach, no reduction
    /// elsewhere), `"full"` or `"reduced"`. Governs every decorative animation
    /// through one [`crate::motion::MotionPolicy`]; unknown values fall back to
    /// `auto`, and unfocused windows always show static effects. Hot-reloadable.
    pub(crate) motion: Option<String>,
    /// Load-adaptive motion: when `true` (default) sustained render overload drops
    /// decorative animation to the reduced state. `false` opts out;
    /// `motion = "full"` overrides the shed. Hot-reloadable.
    pub(crate) load_adaptive_motion: Option<bool>,
    /// Serious mode: suppress nonessential sound and decorative effects while
    /// keeping terminal behavior, cursor blink, visual bell and functional motion.
    /// Default off. Individual effect preferences are left intact and apply again
    /// when it turns off.
    pub(crate) serious_mode: Option<bool>,
    /// Cursor motion trail. Default on (see [`DEFAULT_DECORATIVE_EFFECTS`]) and
    /// idle at rest; `cursor_trail = false` opts out.
    pub(crate) cursor_trail: Option<bool>,
    /// Typing-momentum glow: the cursor glows with typing speed, cools when
    /// typing stops, and does not blink while warm. Rides the cursor-effects lane,
    /// so `cursor_trail = false` turns it off too; its effective default follows
    /// [`DEFAULT_DECORATIVE_EFFECTS`] (owner, 2026-09-08).
    pub(crate) cursor_momentum_glow: Option<bool>,
    /// Trail style: `rainbow kitty pet` (default: rainbow ribbon plus the
    /// full-body cat; every kitty-named spelling, bare `kitty` included, selects
    /// it), `rainbow kitty flying` (the ribbon under the flying kitty head; also
    /// `flying kitty`, `kitty flying`, `nyan rainbow`, `nyan`, `rainbow`),
    /// `rainbow kitty underline`, `rainbow kitty tall` (the default full-height
    /// geometry), `phaser`, `comet`, `lumen`, `sparkle`, `fire`, `laser`,
    /// `water`, `beam`, or `off`.
    ///
    /// Rainbow kitty styles draw the engine in `docs/design/RAINBOW-KITTY-V2.md`;
    /// `… v2` spellings mean the same as the bare ones. Unknown spellings
    /// (including `… v1`) fall back to the default, and the fallback is reported
    /// (`--validate-config`, the load-time warning, `trail status`'s `resolved=`).
    pub(crate) cursor_trail_style: Option<String>,
    /// Trail Pack manifests — user-generated cursor trails as data (design
    /// `docs/trail-packs.md`). Each entry is a path to a `*.toml` Trail Pack
    /// (`~` expanded); a loaded pack with id `X` is selected with
    /// `cursor_trail_style = "pack:X"`. Compiled fail-closed at load (byte cap,
    /// unknown fields/effects rejected, every scalar clamped); an unreadable or
    /// invalid pack is skipped with a warning, never disabling the whole trail.
    pub(crate) cursor_trail_packs: Option<Vec<String>>,
    /// Trail sound effects: each style's sound palette plays softly on the same
    /// spawn edge that lights the trail. Default on; silent whenever the trail is
    /// off, motion is reduced or the window is unfocused. `trail_sounds = false`
    /// mutes them.
    pub(crate) trail_sounds: Option<bool>,
    /// Trail sound volume, `0.0`–`1.0` (default `0.4`: a lone keystroke peaks
    /// around −22 dBFS). Config-file only.
    pub(crate) trail_sound_volume: Option<f32>,
    /// Trail sound ambient bed (default on): the per-style background texture
    /// behind fast typing. `false` gates the bed mixer entirely (zero samples
    /// rendered); discrete notes and the melody are unaffected
    /// (owner, 2026-09-09).
    pub(crate) trail_sound_bed: Option<bool>,
    /// Typing sound voice (default `"auto"`, the Settings ▸ Sound ▸ "Typing
    /// sound" picker). `"auto"` follows the trail style's palette. Other values
    /// pick a voice regardless of trail: `"music box"` (alias `"glass bell"`),
    /// `"warm pluck"`, `"glitter"`, `"ice chime"`, `"droplet"`, `"pew"`,
    /// `"zap"`, `"tick"`, `"crackle"`, `"mechanical"`, `"typewriter"`,
    /// `"marimba"` and `"felt"`. Load-only aliases are in `SoundVoice::ALIASES`.
    /// Every voice shares the same typing floor, rate governor and
    /// volume/on-off/bed gates; the kitty's hold-song stays the kitty's.
    pub(crate) trail_sound_style: Option<String>,
    /// Tone melody (default on): an on-device classifier (`aterm_effects::tone`)
    /// infers one of five moods from the typed line only (never screen content)
    /// and the synth leans its scale and feel accordingly. Inert whenever trail
    /// sounds are; `false` keeps the neutral melody and never runs the classifier.
    pub(crate) tone_melody: Option<bool>,
    /// Robi the helper robot (default off). Once enabled he lives on the glass,
    /// cycling through rounds (walking the typed row, climbing a ladder, swinging
    /// across the tab bar) while showing tips in a speech bubble (`robi_bubble`).
    /// Typing `robi` or `robot` restarts his rounds. Idle stands cost no
    /// repaints; hidden under reduced motion or serious mode.
    pub(crate) robi: Option<bool>,
    /// Rainbow sparkle on the post-update notice (default on): a hue-cycling
    /// badge and twinkling sparkles instead of a flat disc. Decorative only;
    /// other notices are unchanged, and reduced motion holds the hues still.
    pub(crate) notice_sparkle: Option<bool>,
    /// Sing-along riff (default on): `false` schedules no riff bar for the
    /// held-key celebration (`aterm_effects::kitty_sing`), the loudest sound the
    /// engine emits, while other voices keep their levels. Sound only; the
    /// celebration's visuals follow the motion policy. Focus, `trail_sounds` and
    /// `trail_sound_volume` apply first.
    pub(crate) trail_sound_riff: Option<bool>,
    /// Audible terminal bell (default on). BEL rings the system alert sound
    /// (`NSBeep` on macOS, `MessageBeep` on Windows), outside the trail synth's
    /// volume controls. `false` suppresses the beep only; the visual bell and the
    /// attention request stay. Inert off macOS/Windows.
    pub(crate) bell_sound: Option<bool>,
    /// Supervisor choice chime (default on; owner, 2026-09-23): when the
    /// in-window supervisor answers Claude Code's question dialog, a short
    /// quiet chime plays through the trail synth (`App::chose_chime`), also for
    /// background tabs, at most once per two seconds, never headless, in
    /// serious mode or with `trail_sounds` off. `false` silences the chime
    /// only. Inert off macOS.
    pub(crate) choice_sound: Option<bool>,
    /// Trail colour, `#RRGGBB`. Defaults to the (themed) cursor colour, so the
    /// trail matches the cursor unless overridden here.
    pub(crate) cursor_trail_color: Option<String>,
    /// Secondary/accent colour, `#RRGGBB` (comet tail + landing ring of the LUMEN
    /// styles). Defaults to a brightened cursor colour.
    pub(crate) cursor_trail_accent: Option<String>,
    /// Path to a PNG sprite for the flying cat on the `rainbow kitty flying`
    /// style (RGBA or RGB, ideally small right-facing pixel art), nearest-scaled
    /// to the cursor. Unset uses the built-in sprite. `~` expands to $HOME.
    /// The key keeps its `nyan` spelling so existing configs still apply.
    pub(crate) cursor_nyan_sprite: Option<String>,
    /// Wallpaper image drawn behind every terminal tab (not native tabs).
    /// Cover-scaled, toned toward the theme background by
    /// [`wallpaper_dim`](Self::wallpaper_dim), and visible through cells with the
    /// default background. PNG everywhere; other formats decode through the
    /// system on macOS. `~` expands to $HOME. Unset means no wallpaper.
    pub(crate) wallpaper: Option<String>,
    /// How strongly the wallpaper is toned toward the theme background for
    /// text legibility: `0.0` shows the raw image, `1.0` is the flat theme
    /// background. Default 0.3.
    pub(crate) wallpaper_dim: Option<f32>,
    /// Wallpaper text tint (default on): default-colored glyphs take the hue of
    /// the backdrop under their cell at a brightness that clears a WCAG contrast
    /// floor. SGR-colored text keeps its colors. Active only with a wallpaper.
    pub(crate) wallpaper_text_tint: Option<bool>,
    /// How long (milliseconds) a swept cell takes to fade out. Default 260.
    pub(crate) cursor_trail_ms: Option<u64>,
    /// Maximum comet length in cells (a long jump keeps the brightest cells
    /// nearest the cursor). Default 24.
    pub(crate) cursor_trail_length: Option<usize>,
    /// Trail brightness 0.0..=1.0, every style. Default 1.0.
    pub(crate) cursor_trail_intensity: Option<f32>,
    /// Bloom-crown radius in cells (LUMEN styles); 0 disables the crown. Default 0.6.
    pub(crate) cursor_trail_radius: Option<f32>,
    /// Landing-ring "ping" on a jump (LUMEN styles). Default true.
    pub(crate) cursor_trail_ring: Option<bool>,
    /// GPU-only cursor bloom: a soft gaussian halo composited at present time.
    /// The CPU path is unaffected. Default on.
    pub(crate) cursor_trail_bloom: Option<bool>,
    /// Bloom STRENGTH — how much of the blurred glow is added back (0.0..=3.0).
    /// Default 0.85; higher is more radiant (very high can blow out toward white).
    pub(crate) cursor_trail_bloom_strength: Option<f32>,
    /// Bloom RADIUS — how far the halo spreads, in half-res texels (0.5..=8.0).
    /// Default 1.8 for the rainbow kitty styles, 2.2 otherwise; higher is a wider,
    /// softer glow.
    pub(crate) cursor_trail_bloom_radius: Option<f32>,
    /// GPU-only heat shimmer above glowing cursor cells, composited at present
    /// time (the CPU path has none). Default on.
    pub(crate) cursor_fire_shimmer: Option<bool>,
    /// EDR cursor glow (macOS, GPU, HDR-capable screen). Default on. Where the
    /// window's screen reports EDR potential above 1.0, the window gets an
    /// `Rgba16Float` extended-linear-sRGB swapchain and the lumen aurora is drawn
    /// above SDR white, bounded by the screen's current EDR headroom (re-queried
    /// on monitor change and, throttled to 250 ms, on redraw). Other windows get
    /// the 8-bit swapchain and the SDR glow-boost crown. A window moves to f16
    /// when its screen gains EDR potential but never back to 8-bit live.
    ///
    /// The grid stays SDR and readback is unaffected; with this off the present
    /// is byte-identical to the non-HDR path (`HdrPresentGate`, aterm-gpu's
    /// `hdr_gate` suite). Hot-reloadable: off stops >1.0 emission immediately;
    /// on lets capable surfaces upgrade at the next probe or re-query. See
    /// [`Config::hdr_glow_or_default`].
    pub(crate) hdr_glow: Option<bool>,
    /// SDR glow-boost strength (0..=1): how much additive crown the cursor glow
    /// may add on a standard (non-HDR) desktop, on top of the offscreen-baked
    /// aurora. Swapchain-side only (the introspection/readback frame is
    /// untouched), budget-shaped by the theme background's darkness (light
    /// themes self-degrade to invisible), hard-capped at 0.35 by a proven bound.
    /// `0.0` disables. See [`Config::cursor_glow_sdr_boost_or_default`].
    pub(crate) cursor_glow_sdr_boost: Option<f32>,
    /// Predictive local echo: `adaptive` (default: paint typed glyphs instantly
    /// once the shell has proven it echoes the current line, local or remote),
    /// `off`, or `always`. `adaptive` shows no guess in the alternate screen or
    /// unechoed (password) input; the echo proof resets each submitted line.
    pub(crate) predictive_echo: Option<String>,
    /// Focus-linked shell priority (Windows): while a window is focused, its
    /// visible tab's shell processes and ConPTY hosts run above normal priority
    /// with EcoQoS off so keystroke echo is not starved; on blur they return to
    /// normal. Root shell only. Default on; hot-reloadable; no-op on Unix.
    pub(crate) focus_boost: Option<bool>,
    /// Stream fade: newly streamed cells fade in to their foreground over
    /// `stream_fade_ms`. Default off. Keystroke echo, the alternate screen, a
    /// scrolled viewport and reduced motion always render exact bytes.
    pub(crate) stream_fade: Option<bool>,
    /// Stream-fade duration in milliseconds. Default 90, clamped to 16..=1000.
    pub(crate) stream_fade_ms: Option<u64>,
    /// Built-in colour scheme by name, e.g. `theme = "Dracula"` (case-insensitive):
    /// sets default fg/bg, cursor, selection and the ANSI 0–15 palette. The
    /// individual colour keys layer on top. An unknown name warns and uses the
    /// default. See [`aterm_types::scheme`].
    ///
    /// `theme = "dark:<name>,light:<name>"` follows the live OS appearance (see
    /// [`crate::app_colorscheme`]); a missing side uses the built-in default.
    /// See [`Self::resolve_theme_name`].
    pub(crate) theme: Option<String>,
    /// Interactive shell (config `shell`): a program name resolved with smart
    /// discovery (`"bash"` finds Git for Windows' `bash.exe` off `%PATH%`;
    /// `"pwsh"`, `"cmd"`, `"wsl"`, `"nu"` resolve too) or an absolute path. Unset
    /// uses the platform default (Windows: pwsh, powershell, cmd; Unix: `$SHELL`).
    /// `--shell` overrides it. See `windows::shell`.
    ///
    /// Shell integration: zsh/bash/fish/pwsh and `"wsl"` (bash login shell) get
    /// OSC 7 + OSC 133; `"cmd"` gets prompt marks and cwd only (no command text,
    /// exit code or `wait`); `"nu"` gets none. See
    /// `aterm_shell_integration::ShellType`.
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
    /// How many of the newest lines the find / socket `search` index retains
    /// (default 100 000, the engine's `DEFAULT_MAX_CACHED_LINES`). Floored to the
    /// visible rows, so the live screen is always searched. The index is built off
    /// the term lock and cached, so this trades memory, not keystroke latency;
    /// deeper scrollback is reported as a partial result. See [`search_index_depth`].
    pub(crate) search_history_lines: Option<u32>,
    /// Primary font family (e.g. `"JetBrains Mono"`), resolved via
    /// [`resolve_font_family`]; a miss falls back to [`FONT_CANDIDATES`].
    /// `--font` outranks it.
    pub(crate) font_family: Option<String>,
    /// Display face (`display_font`): a bundled face id (`"pixel"`, `"chunky"`,
    /// `"engraved"`, `"bubble"`; [`aterm_render::DISPLAY_FACES`]). When set it
    /// outranks `font_family` and resolves as the virtual family `display:<id>`
    /// from embedded bytes. Hot-reloadable; `--font` still outranks it.
    ///
    /// `game_font` is accepted as a deprecated alias, and legacy values resolve
    /// through [`aterm_render::DISPLAY_FACE_LEGACY_IDS`]; see
    /// [`warn_deprecated_display_font_spelling`].
    #[serde(alias = "game_font")]
    pub(crate) display_font: Option<String>,
    /// Real bold face for SGR-bold cells: a family name or path, resolved like
    /// `font_family` and injected via the renderer's `set_bold_font`. Absent uses
    /// the discovered `-Bold` sibling or synthetic embolden.
    pub(crate) font_family_bold: Option<String>,
    /// Real italic face for SGR-italic cells. Absent uses discovery or
    /// synthetic shear.
    pub(crate) font_family_italic: Option<String>,
    /// Real bold-italic face; outranks `font_family_bold` plus synthetic shear.
    pub(crate) font_family_bold_italic: Option<String>,
    /// Whether synthetic bold/italic may be applied when no real styled face
    /// exists. Absent means `true`; `false` renders such cells with the regular
    /// face.
    pub(crate) font_synthetic_style: Option<bool>,
    /// Ordered fallback font chain: family names or paths, most preferred first,
    /// as an array or a comma-separated string (what Settings writes). Explicit
    /// entries outrank the built-in candidates. Hot-reloadable.
    pub(crate) fallback_fonts: Option<FontList>,
    /// Monochrome symbol fallback face (name or path), consulted after the
    /// primary and fallback chain miss. Hot-reloadable.
    pub(crate) symbol_font: Option<String>,
    /// Colour-EMOJI face (W6): family name or path. Outranks discovery.
    /// Hot-reloadable.
    pub(crate) emoji_font: Option<String>,
    /// Window chrome appearance, independent of the terminal theme: `"auto"`
    /// (default, follows the OS live), `"light"`, or `"dark"`. Maps to
    /// [`WindowTheme`] via [`Config::window_theme_or_default`]; unknown values
    /// warn and fall back to `auto`. Applied on macOS, Windows and Linux.
    pub(crate) window_theme: Option<String>,
    /// Colour space the macOS CAMetalLayer is tagged with: `"srgb"` (default) or
    /// `"display-p3"` (the oversaturated untagged look on wide-gamut screens).
    /// Rendered and readback bytes are identical either way. Maps to
    /// [`WindowColorspace`] via [`Config::window_colorspace_or_default`]; unknown
    /// values fall back to `srgb`. Hot-reloadable; inert off macOS and on the CPU
    /// present path.
    pub(crate) window_colorspace: Option<String>,
    /// `true` (default) makes Alt/Option send ESC-prefixed (Meta) sequences;
    /// `false` forwards the OS-composed text (Option+A → `å` on macOS) while
    /// non-text Alt chords still use the encoder. See
    /// [`Config::option_as_meta_or_default`].
    pub(crate) option_as_meta: Option<bool>,
    /// Pastejacking guard (default on): a paste holding any line end (a trailing
    /// one included: it submits the line) outside bracketed-paste mode is
    /// confirmed first. The bracketed-paste state checked is the one the bytes are
    /// framed with ([`crate::input::PasteFraming`]). See
    /// [`Config::confirm_multiline_paste_or_default`].
    pub(crate) confirm_multiline_paste: Option<bool>,
    /// Reopen the previous graceful quit's windows, tabs and panes (with each
    /// pane's cwd) at launch. Default on. Scrollback is not persisted. See
    /// [`Config::restore_session_or_default`].
    pub(crate) restore_session: Option<bool>,
    /// Copy a mouse selection to the clipboard on mouse-up. Default on except on
    /// Linux, where the selection already owns PRIMARY and copy-on-select would
    /// clobber an explicit Ctrl+Shift+C copy; `true` there writes both. See
    /// [`Config::copy_on_select_or_default`].
    pub(crate) copy_on_select: Option<bool>,
    /// Right-click in the grid: `"copy_paste"` (with mouse tracking off, copy the
    /// selection if any, else paste) or `"off"` (reported to a tracking app,
    /// otherwise inert). Absent uses the platform default: `copy_paste` on
    /// Windows, `off` elsewhere. Maps to [`RightClickGesture`] via
    /// [`Config::right_click_or_default`]; unknown values use the default.
    pub(crate) right_click: Option<String>,
    /// Keyboard chord that opens a tab's context menu on Windows and Linux:
    /// `"menu_key"` (default, the Menu key only), `"on"` (Menu key and
    /// Shift+F10), or `"off"` (pointer only). The default avoids Shift+F10
    /// because applications receive it as `kf22`.
    ///
    /// The chord is not a `[keybindings]` action (a config typo must not remove
    /// the only keyboard route to the menu); this key can only surrender it.
    /// A front terminal whose app enabled kitty key reporting keeps the key
    /// regardless (`App::front_defers_tab_menu_chord`). Maps to [`TabMenuChord`]
    /// via [`Config::tab_menu_chord_or_default`]; unknown values use the default.
    pub(crate) tab_menu_chord: Option<String>,
    /// Where a new terminal opens when one is running: `"new_window"` (default,
    /// its own window and process) or `"attach"` (joins the running instance as
    /// a tab, else starts one). `useNew` / `useExisting` are aliases.
    ///
    /// Read by the front door (`crates/aterm/src/main.rs`) and the Windows jump
    /// list, parsed by `aterm_cli::WindowingBehavior`; `aterm new-window` ignores
    /// it. Unknown values warn once and fall back to `new_window`.
    pub(crate) windowing_behavior: Option<String>,
    /// Show the top-right build/version badge. Default off. See
    /// [`Config::show_build_badge_or_default`] and [`crate::build_badge`].
    pub(crate) show_build_badge: Option<bool>,
    /// `[keybindings]`: chord strings (`"cmd+shift+t"`) to action names
    /// (`"new_tab"`), checked first in `on_key`; a miss falls through to the
    /// built-in chords. Malformed entries are warned and skipped.
    pub(crate) keybindings: Option<std::collections::BTreeMap<String, String>>,
    /// `[key_sequences]`: chord strings (`"shift+enter"`, `"f5"`) to raw bytes
    /// sent to the PTY instead of the default encoding. Values expand
    /// `\n \r \t \e \0 \a \b \f \v \\ \xNN \u{NNNN}`; use a TOML literal
    /// (single-quote) string for `\e`, `\xNN` or `\u{...}`, since a basic string
    /// rejects them and fails the whole config. Consulted after `[keybindings]`
    /// and before the encoder and non-menu built-in chords. macOS menu key
    /// equivalents (Cmd-C, Cmd-V, Cmd-F, …) never reach it. Bad or oversized
    /// (>1 KiB) entries are warned and skipped.
    pub(crate) key_sequences: Option<std::collections::BTreeMap<String, String>>,
    /// Rows reserved at the top for the in-grid tab strip. Default 0 on macOS
    /// (the toolbar carries tabs) and 1 elsewhere ([`DEFAULT_TAB_STRIP_ROWS`]);
    /// clamped to [`MAX_TAB_STRIP_ROWS`].
    pub(crate) tab_strip_rows: Option<u16>,
    /// Windows in-grid tab band height: `"standard"` (default, about
    /// [`TAB_BAND_STANDARD_LOGICAL_PX`], a WinUI tab) or `"compact"` (the strip's
    /// cell rows plus top pad). The extra pixels are a synthetic chrome head band
    /// (`App::synthetic_strip_head_px`). Inert off Windows and when
    /// `tab_strip_rows = 0`; unknown values use the default.
    pub(crate) tab_band_height: Option<String>,
    /// Active tab background colour (`#RRGGBB`) in both the macOS toolbar and the
    /// in-grid strip; label ink flips black/white by luminance. Absent uses the
    /// translucent system default. Hot-reloadable.
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
    /// the standard HTTP(S)_PROXY/ALL_PROXY/NO_PROXY environment, and a proxy
    /// setting it cannot use fails the request; `direct` bypasses it.
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
    /// Master switch for per-session status classification. Default on; `false`
    /// stops the classifier entirely.
    pub(crate) tab_status: Option<bool>,
    /// How long a foreground job may print nothing before its phase becomes
    /// `quiet`. Default 5000; clamped to 500..=120000 so a typo can neither make
    /// every job look stalled instantly nor keep a finished build lit for hours.
    pub(crate) tab_status_quiet_after_ms: Option<u64>,
    /// Minimum time a candidate phase must hold before publication. Default 750,
    /// clamped to 0..=10000; zero publishes every transition.
    pub(crate) tab_status_dwell_ms: Option<u64>,
    /// Whether a classified status sets the tab's busy/attention marks. Default
    /// on; off keeps classification (the `status` verb, tooltip) running.
    pub(crate) tab_status_badge: Option<bool>,
    /// Whether a session's connection role draws the fourth tab mark (▲ outbound,
    /// ▽ inbound, hourglass both). Default on; off hides only the mark.
    pub(crate) tab_connection_badge: Option<bool>,
    /// Explain heavy load (default on): when typing slows in a focused window
    /// while something else loads the machine, the band names the cause
    /// (`Typing slowed by cargo in tab 2`) with a level meter. `false` stops the
    /// probe entirely. Applied live.
    pub(crate) explain_heavy_load: Option<bool>,
    /// Whether aterm posts its own notices (escalation herald, embedded operator,
    /// consent attention, update health) as OS notifications. Default off. When
    /// off, the facts remain in the menu bar, tab marks, message band and
    /// `messages.log`; the operator's notices remain only in its queue
    /// (`aterm fleet status`). Program notifications (OSC 9/99/777) are governed
    /// by `allow_notifications`. Applied live (`notify::spawn_delivery`)
    /// (owner, 2026-09-28).
    pub(crate) desktop_alerts: Option<bool>,
    /// Whether each fresh session spawn also installs or updates the coding-agent
    /// primer and bundled skills for every detected agent (the `aterm agents
    /// install` upsert, on a detached thread, at most once a minute). Default on.
    /// `false` stops it entirely, which is what makes `aterm agents remove` stick.
    pub(crate) agents_auto_prime: Option<bool>,
    /// BiDi handling: `"implicit"` (default, per-line UAX#9 reordering),
    /// `"disabled"` (logical order), or `"explicit"` (app-controlled). Maps to the
    /// engine `BiDiConfig.mode`.
    pub(crate) bidi: Option<String>,
    /// East-Asian ambiguous-width characters: `"narrow"` (default, 1 cell) or
    /// `"wide"` (2 cells). Maps to the engine `ambiguous_width_double`.
    pub(crate) ambiguous_width: Option<String>,
    /// Programming ligatures for fonts that carry them. Default on; `false`
    /// renders per cell. Maps to the renderer's `LigatureMode`; the cursor's cell
    /// always renders unligated.
    pub(crate) ligatures: Option<bool>,
    /// Merged (N:1) ligatures: admit runs whose shaping collapses several cells
    /// into one wide glyph (Cascadia Code), sliced into per-cell tiles. Default
    /// off; a no-op on 1:1 fonts. Maps to `TextShapingConfig::admit_collapsed`.
    pub(crate) merged_ligatures: Option<bool>,
    /// OpenType features for the primary face, e.g.
    /// `font_features = ["ss01", "zero", "-calt"]`: `tag`/`+tag` enables, `-tag`
    /// disables, `tag=N` sets a value. Tags are 1–4 ASCII chars; unknown tags are
    /// ignored by the shaper. An entry may be a space-separated group. See
    /// [`Config::text_shaping`].
    pub(crate) font_features: Option<Vec<String>>,
    /// Glyph coverage blending: `"linear-corrected"` (default, linear-light with a
    /// perceptual alpha remap matching native CoreText stroke weight) or
    /// `"linear"` (the exact physical blend, which reads thinner on dark themes).
    /// Unknown values keep the default. Hot-reloadable. See
    /// [`Config::text_blending_or_default`].
    pub(crate) text_blending: Option<String>,
    /// macOS: rasterize with CoreText font smoothing (stem darkening) for a
    /// heavier glyph. Default `false`; inert off macOS. Hot-reloadable.
    pub(crate) font_thicken: Option<bool>,
    /// Variable-font axis requests for the primary face, e.g.
    /// `font_variation = ["wght=450", "opsz=14"]`. Values clamp to the font's
    /// `fvar` bounds, unknown axes are ignored, malformed entries warn and skip.
    /// Absent uses the `Regular` named instance, else `wght=400` clamped. A
    /// [`FontList`] so Settings can round-trip it as one comma-joined string.
    pub(crate) font_variation: Option<FontList>,
    /// Requested `wght` (1–1000) for the primary face, applied after
    /// `font_variation` (this key wins). Clamped to the axis; ignored for
    /// non-variable fonts.
    pub(crate) font_weight: Option<u32>,
    /// Extra `wght` on dark themes only (light-on-dark reads thinner). Applied
    /// only when the nudged instance keeps the `'M'` advance within 0.25px and
    /// the cell geometry unchanged. Absent or 0 is off; clamped 0..=300.
    pub(crate) font_weight_dark_nudge: Option<f32>,
    /// Stem-weight gamma on glyph coverage (`< 1.0` thickens, `> 1.0` thins;
    /// clamped 0.30..=3.0). Absent means `1.0`. Hot-reloadable.
    pub(crate) stem_gamma: Option<f32>,
    /// Linux/Windows glyph grid-fitting: `"full"` (default, both axes, crispest),
    /// `"light"` (vertical only), `"native"` (the font's bytecode), or `"off"`.
    /// Unknown spellings mean `"full"`. Inert on macOS. Hot-reloadable (drops
    /// the glyph atlas).
    pub(crate) font_hinting: Option<String>,
    /// Linux subpixel text: `"off"` (default, grayscale), `"rgb"` or `"bgr"`.
    /// CPU compositor only; it falls back to grayscale under translucency, a
    /// wallpaper, or non-primary-family glyphs. Unknown spellings mean `"off"`.
    /// Inert on macOS and Windows. Hot-reloadable.
    pub(crate) font_subpixel: Option<String>,
    /// Line-height multiplier on the cell box, without changing glyph size;
    /// leading splits half above, half below. Clamped 0.8..=2.0; absent means
    /// `1.0`. Hot-reloadable.
    pub(crate) line_height: Option<f32>,
    /// Signed px shift of every glyph baseline, for faces whose metrics sit off.
    /// Clamped ±32; absent means `0`.
    pub(crate) adjust_baseline: Option<i64>,
    /// Signed px shift of the underline top (positive is down). Clamped ±32;
    /// absent means `0`.
    pub(crate) adjust_underline_position: Option<i64>,
    /// Underline thickness escape hatch (W7): fatten/thin the resolved
    /// underline by a signed px delta. Clamped ±32; ABSENT = `0`.
    pub(crate) adjust_underline_thickness: Option<i64>,
    /// Skip underline coverage within 1px of the cell's glyph ink so descenders
    /// are not struck through. Absent means `true`.
    pub(crate) underline_skip_descenders: Option<bool>,
    /// Per-cell minimum WCAG contrast ratio: glyph fg, decorations and the
    /// cursor fill are floored against their actual bg. `1.0` (absent) is off;
    /// clamped 1.0..=21.0. Hot-reloadable.
    pub(crate) minimum_contrast: Option<f32>,
    /// Selected-text foreground `#RRGGBB`. Absent floors fg to 4.5:1 against the
    /// selection band.
    pub(crate) selection_foreground: Option<String>,
    /// Dim the selection band while the window is unfocused. Absent means
    /// `false`.
    pub(crate) selection_inactive: Option<bool>,
    /// Draw a hairline along the edges of the focused split pane, inside the
    /// divider gaps (`app_render::ActivePaneMark`). Absent means `true`; inert in
    /// a single-pane or unfocused window. Takes effect on the next composed frame.
    pub(crate) split_focus_mark: Option<bool>,
    /// Render the cursor cell per-cell so the block cursor never sits on a
    /// ligature (`LigatureMode::CursorDisabled`). Absent means `false`; ignored
    /// when `ligatures = false`.
    pub(crate) cursor_break_ligatures: Option<bool>,
    /// SGR 1 promotes indexed colors 0–7 to bright 8–15. Absent means `true`;
    /// `false` keeps bold a pure weight change.
    pub(crate) bold_is_bright: Option<bool>,
    /// SGR 2 (faint): fraction of the foreground kept, blended toward the cell
    /// background in linear light. Clamped 0.0..=1.0; absent means `0.5`.
    pub(crate) faint_opacity: Option<f32>,
    /// Window background opacity (`0.0` transparent to `1.0` solid, the default).
    /// Applies to background quads only; glyphs and images stay opaque. Below
    /// `1.0` the minimum-contrast floor engages at 4.5:1
    /// ([`Config::effective_minimum_contrast`]). Implemented only on the macOS
    /// GPU path; other paths stay solid and are diagnosed. Hot-reloadable.
    pub(crate) background_opacity: Option<f32>,
    /// Window material: `none` | `hud` | `sidebar` | `under-window` (aliases
    /// `underwindow`/`under_window`, case-insensitive). On macOS GPU it selects
    /// the `NSVisualEffectView` behind a translucent background (requires
    /// `background_opacity < 1.0`); on Windows GPU it maps to Mica/Acrylic chrome
    /// with an opaque grid. Absent / `none` disables it. Hot-reloadable.
    pub(crate) background_material: Option<String>,
    /// Window padding on all edges in logical px (scaled by the display factor).
    /// Absent means `12.0` ([`crate::PAD_LOGICAL_PX`]); clamped 0..=64.
    /// Hot-reloadable (see [`App::reload_config`]).
    pub(crate) window_padding: Option<f32>,
    /// Top padding override in logical px. Absent means `2.0`
    /// ([`crate::PAD_TOP_LOGICAL_PX`]); clamped to `0..=window_padding`, the
    /// domain [`aterm_render::Renderer::set_pad_top`] accepts. Hot-reloadable.
    pub(crate) window_padding_top: Option<f32>,
    /// Allow OSC 52 clipboard queries (`Pd = "?"`). Default off. When on,
    /// macOS/Windows answer with the system pasteboard (`spawn.rs`), so text
    /// copied in other apps becomes readable; Linux answers only selections aterm
    /// owns. On macOS 26 the read raises the system paste alert naming aterm.
    pub(crate) allow_osc52_query: Option<bool>,
    /// Secure keyboard entry (default off): while on and an aterm window is
    /// focused, macOS blocks other processes from observing keystrokes
    /// (`EnableSecureEventInput`, gated in `crate::secure_input`). macOS only.
    pub(crate) secure_keyboard_entry: Option<bool>,
    /// Allow XTWINOPS (`CSI t`). On Linux authorized manipulations (iconify,
    /// maximize, fullscreen, resize; move is denied in-core) reach the window via
    /// `spawn::configure_window_ops` → `App::on_window_op`. The engine answers
    /// reports on every platform: title/icon label, grid size (`CSI 18 t`), text
    /// area pixels (`CSI 14 t`) and cell size (`CSI 16 t`); position and screen
    /// size are never answered. Default off because reports can fingerprint the
    /// host.
    pub(crate) allow_window_ops: Option<bool>,
    /// Allow desktop notifications (OSC 9 / 99 / 777). Default off. Delivery runs
    /// under aterm's identity (`terminal-notifier` or `osascript`; see
    /// `notify.rs`), hence the opt-in.
    pub(crate) allow_notifications: Option<bool>,
    /// Security opt-in: allow apps to set indexed colors (OSC 4 / numeric OSC 21).
    /// Default OFF. Maps to `allow_palette_reconfigure`.
    pub(crate) allow_palette_reconfigure: Option<bool>,
    /// Allow Kitty graphics non-direct mediums (`t=f` file, `t=t` temp file,
    /// `t=s` POSIX shm) to read host files. Default off (an exfiltration
    /// surface). When on, a size-capped resolver is installed
    /// (`spawn::configure_kitty_file_transfer`).
    pub(crate) allow_kitty_file_transfer: Option<bool>,
    /// Run the per-session temporal recorder read by the `temporal` control verb.
    /// Default off (no writer thread, no retention). When on, each tab seeds a t0
    /// keyframe and taps its reader so the session can be replayed.
    pub(crate) temporal_recording: Option<bool>,
    /// Network drive (`[net]`): the opt-in TLS listener that lets a remote aterm
    /// drive this one, plus saved `[[net.connections]]` for the `dial <name>`
    /// verb. Absent means no network surface. See [`NetConfig`],
    /// [`crate::net_listen`], [`crate::net_connections`].
    pub(crate) net: Option<NetConfig>,
    /// Fabric bridge (`[fabric]`): the `aterm-link serve …` child holding the far
    /// ends of two socketpairs at fds 3 and 4, speaking `Scope::Bridge`. Absent
    /// means no fabric. Development builds may override it with
    /// `ATERM_FABRIC_COMMAND`. See [`FabricConfig`], [`crate::fabric_launch`].
    pub(crate) fabric: Option<FabricConfig>,
    /// Embedded operator (`[operator]`): the experimental in-process observer and
    /// attention queue behind `aterm fleet manage`. Absent means off. See
    /// [`OperatorConfig`] and [`crate::operator_host`].
    pub(crate) operator: Option<OperatorConfig>,
    /// Self-update (`[update]`): whether the updater runs, how a staged build
    /// applies, and (development builds only) which GitHub repo it pulls from.
    /// Absent means on, from the public `alabsystems/aterm` channel. No
    /// environment variable overrides it. The pinned Team ID and notarization are
    /// the trust anchor, not the location. Effective on macOS only. See
    /// [`UpdateConfig`], crate `aterm-update`, `docs/RELEASING.md`.
    pub(crate) update: Option<UpdateConfig>,
    /// Sparkle words (`[sparkle_words]`): sparkle matched profanity and give
    /// cat/kitty words a cat-paw. A `[sparkle_words.orca]` table is ignored.
    /// Absent means both are on. See [`SparkleWordsConfig`].
    pub(crate) sparkle_words: Option<SparkleWordsConfig>,
    /// Matrix rain (`[matrix_rain]`) under the text, in empty cells only. Absent
    /// means off. See [`MatrixRainConfig`].
    pub(crate) matrix_rain: Option<MatrixRainConfig>,
    /// Presence (`[presence]`): the driver band under the tab bar and the colour
    /// rim, each toggled from the View menu. Absent means both on. See
    /// [`PresenceConfig`].
    pub(crate) presence: Option<PresenceConfig>,
    /// THE CLAUDE CODE FOOTER's switches (`[claude_footer]`, owner 2026-09-30):
    /// what aterm writes into the rule under Claude Code's input box. Absent ⇒
    /// `Σ` shown. See [`ClaudeFooterConfig`].
    pub(crate) claude_footer: Option<ClaudeFooterConfig>,
    /// RENDER REPAIR (`[render]`): what aterm does about a full-screen Claude
    /// Code tab whose screen a resize of its own window left displaced. See
    /// [`RenderConfig`]. Read once at launch.
    pub(crate) render: Option<RenderConfig>,
    /// PRISM WAKE — the output streak (`[output_streak]`,
    /// `docs/DESIGN-output-streak-2026-08-30.md`): a thin per-theme rainbow
    /// comet answering fresh PROGRAM OUTPUT, plus one soft pip per episode.
    /// Absent ⇒ ON (owner ruling 2026-08-31, *"default on"* — the inverse of
    /// `[matrix_rain]`, which stays opt-in); `enabled = false` opts out. See
    /// [`OutputStreakConfig`], whose field docs carry the per-key laws.
    pub(crate) output_streak: Option<OutputStreakConfig>,
    /// Toolchain manager (`[packages]`, `atpkg`): automatic updates, install
    /// consent and per-program `exclude`, read by `atpkg` from this same file.
    /// See [`PackagesConfig`].
    pub(crate) packages: Option<PackagesConfig>,
    /// Upstream-Rust reroute (`[reroute]`): whether a rerouted `cargo`/`rustc`/`tlc`
    /// announces itself. Read by `atpkg` (`atpkg::config::RerouteConfig`);
    /// mirrored for Settings. See [`RerouteConfig`].
    pub(crate) reroute: Option<RerouteConfig>,
    /// macOS consent posture (`[privacy]`): master switch, silent access probes,
    /// the one-time access card, the folder warm-up and the protected-root set
    /// shared with Containment. See [`PrivacyConfig`].
    pub(crate) privacy: Option<PrivacyConfig>,
    /// macOS host settings (`[machine]`): Universal Control off and cargo build
    /// output hidden from Spotlight. Applied by `atpkg` (`aterm pkg machine
    /// apply`); the GUI only displays and edits it. Absent means both on. See
    /// [`MachineConfig`].
    pub(crate) machine: Option<MachineConfig>,
    /// PTY keeper (`[keeper]`, `docs/DESIGN-pty-keeper-2026-09-26.md`): register
    /// every terminal's master with the keeper so a crash or kill does not hang
    /// up its shells and the next launch reattaches them. Absent means off. Read
    /// at launch. See [`KeeperConfig`].
    pub(crate) keeper: Option<KeeperConfig>,
    /// The `[harness]` policy ([`HarnessPolicy`]), read from the file's whole
    /// text rather than deserialized (it also accepts dotted `harness.<key>`
    /// lines under other tables and non-TOML text); set in [`Config::parse`].
    #[serde(skip)]
    pub(crate) harness: HarnessPolicy,
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
    /// Honor HTTP_PROXY, HTTPS_PROXY, ALL_PROXY, and NO_PROXY from the aterm
    /// process.
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

/// Tab status bounds, shared with `prefs` so a Settings save never writes a
/// value the resolver would clamp.
pub(crate) const DEFAULT_TAB_STATUS_QUIET_AFTER_MS: u64 = 5_000;
pub(crate) const MIN_TAB_STATUS_QUIET_AFTER_MS: u64 = 500;
pub(crate) const MAX_TAB_STATUS_QUIET_AFTER_MS: u64 = 120_000;
pub(crate) const DEFAULT_TAB_STATUS_DWELL_MS: u64 = 750;
pub(crate) const MIN_TAB_STATUS_DWELL_MS: u64 = 0;
pub(crate) const MAX_TAB_STATUS_DWELL_MS: u64 = 10_000;
/// At most one classification per session per this interval. Not
/// configurable: raising it independently of dwell would break dwell.
const NOMINAL_TAB_STATUS_OBSERVE_INTERVAL_MS: u64 = 250;
/// Floor on that budget, which `tab_status_dwell_ms = 0` would otherwise take to
/// zero and turn every output burst into a classification.
const MIN_TAB_STATUS_OBSERVE_INTERVAL_MS: u64 = 50;

/// Whether two renderer themes paint the same. `aterm_render::Theme` has no
/// `PartialEq`; the exhaustive destructure makes a new field a compile error
/// here instead of a change that escapes the comparison.
pub(crate) fn theme_eq(a: Theme, b: Theme) -> bool {
    let Theme {
        fg,
        bg,
        cursor,
        selection,
    } = a;
    (fg, bg, cursor, selection) == (b.fg, b.bg, b.cursor, b.selection)
}

/// Pack an [`Rgb`] as the renderer's `0x00RRGGBB`.
fn rgb_u32(c: Rgb) -> u32 {
    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
}

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
    /// Force the static, non-twinkling path. Default false. The explicit user
    /// override; the OS Reduce Motion flag is folded in per frame through
    /// `MotionPolicy` (app_render).
    pub(crate) reduced_motion: Option<bool>,
    /// Suppress decorations on the alternate screen. Default false (full-screen
    /// TUIs decorate like the primary screen).
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
    /// The non-feline Sparkle Words settings; its `enabled` key gates profanity,
    /// emphasis and custom/Toy Pack words together.
    pub(crate) profanity: Option<SparkleProfanityConfig>,
    /// The steady feline CAT-PAW sub-table.
    pub(crate) feline: Option<SparkleFelineConfig>,
    /// The typed-word DOG cameo sub-table.
    pub(crate) canine: Option<SparkleCanineConfig>,
    /// The typed KITTY-COMMAND sub-table (`sit`, `kitty jump`, `good kitty`).
    pub(crate) tricks: Option<SparkleTricksConfig>,
    /// The animated glyph-ink shimmer sub-table (v2).
    pub(crate) ink: Option<SparkleInkConfig>,
    /// The emphasis / hype-word class sub-table (v2, ink-only).
    pub(crate) emphasis: Option<SparkleEmphasisConfig>,
    /// Custom word-effect specs (`[[sparkle_words.custom]]`): per-word `ink`,
    /// `burst` and `graphic` axes in any combination. Words join the emphasis
    /// class, override class defaults and bypass per-class enable gates.
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

/// Worst-case I/O for one path-feed fingerprint pass: one lexicon, eight Toy
/// Packs and eight Trail Packs, each reader taking at most one sentinel byte
/// past its cap (17 opens, 2,883,601 bytes).
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

/// Read one configured effect feed once and derive both its consumer bytes and
/// its fingerprint from that one handle.
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

/// Word-decoration runtime compiled for one admitted config generation. File
/// reads, Toy Pack compilation and lexicon construction happen before it
/// reaches the event loop.
#[derive(Clone)]
pub(crate) struct PreparedSparkleRuntime {
    resolved: Option<crate::word_decorations::Resolved>,
    /// Kitty-command vocabulary (`aterm_lexicon::TrickLexicon`), compiled off the
    /// keystroke path from the same `[sparkle_words] languages` as the word
    /// lexicon. Kept outside [`Resolved`](crate::word_decorations::Resolved) so
    /// it neither keeps the word engine alive nor disappears with it; compiled
    /// regardless of the master switch (dispatch checks it). Replaced per
    /// generation; the input path borrows it only at word boundaries.
    tricks: std::sync::Arc<aterm_lexicon::TrickLexicon>,
}

/// One consistent generation of every path-backed effect feed. Each consumer
/// and its fingerprint derive from the same admitted bytes, so a path swap
/// cannot label stale data as current.
#[derive(Clone)]
pub(crate) struct PreparedPathFeedGeneration {
    pub(crate) sparkle: PreparedSparkleRuntime,
    pub(crate) trail_packs: std::sync::Arc<TrailPackCatalog>,
    pub(crate) fingerprints: PathFeedFps,
}

impl PreparedSparkleRuntime {
    /// This generation's kitty-command vocabulary, borrowed for one press.
    pub(crate) fn tricks(&self) -> &aterm_lexicon::TrickLexicon {
        &self.tricks
    }

    /// Custom-spec consumers for this generation (valid Toy Packs, then inline
    /// overrides), so Settings can disclose dependencies without reopening
    /// manifests.
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

/// Validated Trail Pack catalog for one config generation, shared by `Arc`
/// between the live host and Settings so manifests are compiled once per
/// revision.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrailPackCatalog {
    pub(crate) packs: std::collections::HashMap<String, aterm_effects::cursor_glow::TrailParams>,
    pub(crate) ids: Vec<String>,
    pub(crate) diagnostics: Vec<String>,
    /// Presentation parsed once per asset generation so redraw does not rescan
    /// the style vocabulary.
    resolved_style_raw: Box<str>,
    resolved_presentation: ResolvedTrailPresentation,
}

/// Maximum encoded PNG bytes for a custom kitty cursor sprite. Read with
/// `take(MAX + 1)`, so file metadata cannot cause an unbounded allocation.
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

/// Installed-theme discovery keeps a bounded, deterministic prefix and reads
/// every file under a hard byte limit.
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

/// One bounded custom-theme file. Invalid entries are kept for diagnostics but
/// never listed in the picker, so a bad configured file fails closed and Manual
/// reports the same reason.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ThemeAsset {
    name: String,
    source_id: std::sync::Arc<str>,
    fingerprint: u64,
    resolution: ThemeAssetResolution,
}

/// Parsed custom themes (files only; built-ins live in `aterm-types`) for one
/// config generation. Consumers resolve through [`Self::resolve`]; the
/// directory observer parses a new catalog off-thread and publishes it as a
/// complete generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ThemeCatalog {
    entries: Vec<ThemeAsset>,
    truncated: bool,
}

/// Live watcher failures that keep the last admitted catalog. Deterministic
/// file problems (bad UTF-8, syntax, size) still yield an `Invalid` entry;
/// these are transient host failures that must not look like user intent.
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

    /// Fallible discovery for the live watcher: distinguishes an empty or
    /// deleted directory from a transient enumeration failure, so a failure
    /// never discards the admitted catalog. Startup uses the best-effort
    /// [`Self::discover_in`].
    pub(crate) fn try_discover_in(
        directory: &std::path::Path,
    ) -> Result<Self, ThemeCatalogWatchError> {
        Self::discover_in_with(directory, true, || {})
    }

    #[cfg(all(test, unix))]
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

    /// Every custom theme the folder held, by name, with the reason it did
    /// not load (`None` for one that loaded).
    pub(crate) fn listing(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.entries.iter().map(|entry| match &entry.resolution {
            ThemeAssetResolution::Ready(_) => (entry.name.as_str(), None),
            ThemeAssetResolution::Invalid(reason) => (entry.name.as_str(), Some(&**reason)),
        })
    }

    /// Resolve a built-in or parsed user theme, case-insensitively, without
    /// filesystem access. Case-fold collisions are rejected at admission.
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
            None if self.truncated => Err(
                "no built-in or loaded custom theme by that name (the themes folder holds more \
                 files than aterm loads)"
                    .to_string(),
            ),
            None => Err("no built-in or loaded custom theme by that name".to_string()),
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

/// Canonicalize custom theme names case-insensitively, like built-ins. Names
/// that collide by case (`Work.conf` vs `work.conf`) collapse to one invalid
/// sentinel rather than letting directory order pick a palette.
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

/// One theme candidate, read through the shared same-handle regular-file
/// reader (`aterm_effects::file_feed`): it opens without following a link
/// swapped in after the scan and verifies the opened handle is a regular file
/// before reading. That handle check is the security boundary.
fn read_bounded_theme_file(path: &std::path::Path) -> Result<Vec<u8>, ThemeFileReadError> {
    aterm_effects::file_feed::read_bounded_regular_file(path, MAX_USER_THEME_FILE_BYTES).map_err(
        |error| {
            // `InvalidData` is the reader's over-limit verdict, `InvalidInput`
            // its not-a-regular-file refusal; every other error is an
            // open/read observation.
            match error.kind() {
                std::io::ErrorKind::InvalidData => ThemeFileReadError::Invalid(format!(
                    "larger than the {MAX_USER_THEME_FILE_BYTES}-byte theme limit"
                )),
                std::io::ErrorKind::InvalidInput => ThemeFileReadError::Unavailable(
                    "theme source is not a regular non-link file".into(),
                ),
                _ => ThemeFileReadError::Unavailable(format!("unreadable ({error})")),
            }
        },
    )
}

/// Resolved custom kitty cursor asset for one config generation. `Invalid`
/// fails closed (the companion is disabled) and stays diagnosable; it never
/// falls back to the built-in sprite.
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

/// Admitted wallpaper image for one config revision, shared by rendering,
/// validation and Settings. `Ready` holds straight sRGB RGBA8 bounded to
/// [`MAX_WALLPAPER_KEEP_DIMENSION`] per side; per-window cover scaling happens
/// at splice time.
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

/// Every non-text config asset admitted at one revision. `ConfigSnapshot`
/// carries one `Arc<ConfigAssetCatalog>` that the live host, capture and
/// Settings all share.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConfigAssetCatalog {
    pub(crate) trail_packs: std::sync::Arc<TrailPackCatalog>,
    pub(crate) kitty_sprite: KittySpriteAsset,
    /// The admitted wallpaper image (the `wallpaper` key), resolved at the
    /// same revision seam as the kitty sprite.
    pub(crate) wallpaper: WallpaperAsset,
    pub(crate) themes: std::sync::Arc<ThemeCatalog>,
    /// Shared-setting consumers from the admitted inline and Toy Pack specs.
    /// `Some(default())` means no word consumes shared tuning; `None` means Toy
    /// Packs and lexicon are still pending worker preparation (also used by the
    /// test catalog), which Settings shows as pending.
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

    fn cache_presentation(&mut self, raw: &str) {
        self.resolved_presentation = resolve_trail_presentation(raw, self);
        self.resolved_style_raw = raw.trim().into();
    }

    /// Cached answer, with a dynamic fallback for test/preview code that
    /// mutates `Config` without publishing a new asset generation.
    pub(crate) fn presentation_for(&self, raw: &str) -> ResolvedTrailPresentation {
        let raw = raw.trim();
        if raw == self.resolved_style_raw.as_ref() {
            self.resolved_presentation
        } else {
            resolve_trail_presentation(raw, self)
        }
    }
}

/// Why a configured trail style cannot emit; shared by rendering, validation
/// and Settings so an invalid value is treated the same everywhere.
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

/// All string-keyed presentation decisions for one resolved trail style,
/// admitted with the Trail Pack catalog so rendering reads only `Copy` fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ResolvedTrailPresentation {
    pub(crate) style: ResolvedTrailStyle,
    pub(crate) beam: bool,
    pub(crate) ribbon_tall: bool,
    /// The explicit `... flat` spelling: the flat body instead of the default
    /// comet body and vivid rail (`RAINBOW-KITTY-V2.md` §30).
    pub(crate) ribbon_flat: bool,
    /// Classic wake colour: spectrum (`false`) or the theme-following two-tone
    /// tracer (`true`).
    pub(crate) classic_mono: bool,
    pub(crate) pet_species: Option<aterm_effects::kitty_pet::PetSpecies>,
    pub(crate) comet: bool,
}

impl Default for ResolvedTrailPresentation {
    fn default() -> Self {
        let raw = crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE;
        let style = aterm_effects::cursor_glow::GlowStyle::parse(raw);
        Self {
            style: ResolvedTrailStyle {
                canonical: Some(raw),
                style: Some(style),
                pack: None,
                issue: None,
            },
            beam: aterm_effects::cursor_glow::style_has_beam_of(style, raw),
            ribbon_tall: true,
            ribbon_flat: false,
            classic_mono: false,
            pet_species: Some(aterm_effects::kitty_pet::PetSpecies::Cat),
            comet: false,
        }
    }
}

impl Default for TrailPackCatalog {
    fn default() -> Self {
        Self {
            packs: std::collections::HashMap::new(),
            ids: Vec::new(),
            diagnostics: Vec::new(),
            resolved_style_raw: crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE.into(),
            resolved_presentation: ResolvedTrailPresentation::default(),
        }
    }
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
pub(crate) struct CursorGlowInputs {
    pub(crate) enabled: bool,
    pub(crate) color: Option<u32>,
    pub(crate) accent: Option<u32>,
    pub(crate) duration_ms: u64,
    pub(crate) length: usize,
    pub(crate) intensity: f32,
    pub(crate) radius: f32,
    pub(crate) ring: bool,
}

/// `[sparkle_words.ink]`: animated glyph-ink shimmer (two-tone gradient with
/// one specular sweep, then constant). Applies to emphasis, profanity and
/// feline words when `sparkle_words.enabled` is on.
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

/// `[sparkle_words.emphasis]`: ink-only hype words. The built-in lexicon has
/// none; `extra_words` populates it.
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
    /// Enable the non-feline Sparkle Words toy. Default true. Also gates
    /// emphasis and custom/Toy Pack words; Keyword Kitties is independent.
    pub(crate) enabled: Option<bool>,
    /// `"rainbow"` (default: animated rainbow ink; `supernova_chance`% of
    /// episodes escalate to the super nova), `"nova"` (the classic nova), or
    /// `"sparkle"` (randomized sparkle). Unknown values mean `"rainbow"`.
    pub(crate) style: Option<String>,
    /// Super nova escalation chance, percent. Default 10, clamped 0..=100 (0
    /// disables). Used only by `style = "rainbow"`.
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
    /// Play the discordant bonk when a profanity word is typed at the caret.
    /// Default true; silenced by focus loss, reduced motion or zero
    /// `trail_sound_volume`. Host-side gate: the engine records its cues and the
    /// App drops them when off.
    pub(crate) bonk: Option<bool>,
    /// Also bonk when an on-screen curse's supernova ignites (including program
    /// output and redraws). Default false; rate-limited by the flash limiter.
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
    /// `"cat"` (default) is the only graphic style; `"paw"` selects ink-only
    /// rendering. Unknown values mean `"cat"`.
    pub(crate) style: Option<String>,
    /// Parsed and preserved for compatibility; has no effect.
    pub(crate) idle: Option<bool>,
    /// Parsed and preserved for compatibility; has no effect.
    pub(crate) gaze: Option<bool>,
    /// Fortune (1/512) / Nebula (1/1024) rare cats (§3.5/§5.4). Default TRUE.
    pub(crate) magic: Option<bool>,
    /// Parsed and preserved for compatibility; has no effect.
    pub(crate) color: Option<String>,
    /// Parsed and preserved for compatibility; has no effect.
    pub(crate) intensity: Option<f32>,
    /// Decorate the bare 3-letter `cat` token (also the shell command). Default true.
    pub(crate) allow_bare_cat: Option<bool>,
    /// Decorate a lone CJK cat ideograph (`猫`) anywhere. Default false (high FP).
    pub(crate) cjk_single_char: Option<bool>,
    /// Record sightings in the Kitty Log (`kitty-log.toml`). Default true.
    /// Host-side gate: when false the App drops the engine's records.
    pub(crate) log: Option<bool>,
    /// Extra whole words to treat as feline (added to the lexicon).
    pub(crate) extra_words: Option<Vec<String>>,
    /// Folded surfaces to never decorate as feline.
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// `[sparkle_words.canine]`: the typed-word dog cameo. `enabled` gates the
/// input-path summon (typing `dog`/`puppy`/… after sustained typing); the
/// screen scanner never draws it.
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

/// `[sparkle_words.tricks]`: kitty commands. A pet-command word typed at a word
/// boundary on a line of only pet talk (`sit␣`, `kitty jump␣`, `good kitty␣`)
/// flashes rainbow and the cursor pet performs it. Fed from the input path only
/// (`aterm_effects::typed_tricks`), so output, pastes and raw controller bytes
/// never command the cat. Its vocabulary is a separate lexicon table so words
/// like `sit` are never decorated by the ambient scanner.
///
/// Config-file only. `enabled` sits under the `[sparkle_words] enabled` master
/// and is independent of the other families.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SparkleTricksConfig {
    /// Let typed kitty commands move the pet and flash the word. Default TRUE
    /// (takes effect only when the sparkle master is on).
    pub(crate) enabled: Option<bool>,
    /// Trick surfaces that never count as a command (for a real `sit` or `play`
    /// on PATH). The line is still judged; only the answer and its exit-127
    /// forgiveness are withheld.
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// The `[matrix_rain]` table. Off by default. Defaults and clamps live only in
/// [`Config::matrix_rain_params`] and [`Config::matrix_rain_enabled`], so an
/// absent key equals a default-valued one. Purely visual: the grid is never
/// mutated.
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
    /// Suppress emission on the alternate screen. Default false.
    pub(crate) suppress_in_alt_screen: Option<bool>,
    /// v1: parsed but inert — the materialize sweep is deferred (design §14).
    pub(crate) materialize: Option<bool>,
    /// Output material bank: the rain's alphabet is rasterized from the program's
    /// real on-screen output, outside the cursor/composer band. Default true;
    /// `false` keeps the pure-kana field.
    pub(crate) output_material: Option<bool>,
    /// Turn-complete head sweep on the working→calm edge. Default true.
    pub(crate) turn_wave: Option<bool>,
    /// Visual bell → 2 s constant-luminance amber hue ramp. Default true.
    pub(crate) bell_alert: Option<bool>,
    /// Show command exit status (OSC 133/633): success fires the head sweep,
    /// failure holds a 2 s ember tint. Default true.
    pub(crate) exit_tint: Option<bool>,
    /// v1: parsed but inert — occupied-cell recolour is v1.1 (design §14).
    pub(crate) ink_text: Option<bool>,
    /// v1: parsed but inert — the GPU luxe layer is deferred (design §14).
    pub(crate) phosphor: Option<bool>,
    /// Field seed. 0 (default) ⇒ a stable per-window seed is derived at
    /// engine build; nonzero ⇒ reproducible (demos/tests).
    pub(crate) seed: Option<u64>,
}

/// The `[presence]` table: durable state of the View ▸ Presence Band and
/// Presence Rim checkables. Both default on; absent equals `true`
/// ([`Config::presence_band_enabled`], [`Config::presence_rim_enabled`]). The
/// toggles write the leaf keys through the serialized config lane.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct PresenceConfig {
    /// The band row under the tab bar. Absent ⇒ shown (its fold law applies);
    /// `false` ⇒ hidden, and `chrome` reports an empty band.
    pub(crate) band: Option<bool>,
    /// The colour rim. Absent means shown; `false` never paints it (the band
    /// carries the same facts in words).
    pub(crate) rim: Option<bool>,
}

/// The `[claude_footer]` table (owner, 2026-09-30): what the Claude Code
/// footer (`crate::claude_footer`) writes into the composer's bottom rule.
/// `usage` is ON BY DEFAULT; an absent key and `true` are the same, which is
/// what [`Config::claude_footer_usage`] encodes. A GUI table, not
/// `[harness]`: that table's rule is that the file only ever takes power
/// away, and it has one reader in `aterm-agent`. There is no fast-mode key:
/// aterm draws no fast chip (Claude Code's own `↯` marks fast mode).
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct ClaudeFooterConfig {
    /// `Σ` — Claude's working time and the session's merged token totals in
    /// the rule, `Σ 7h52m ↑395M ↓2.0M`. Owner, 2026-09-30: "'↑395M ↓2.0M'
    /// bring back this summary by default. I really like it. I just can't
    /// have so many different model stats." `false` hides it; the limit wall
    /// (`⧗`) shows either way.
    pub(crate) usage: Option<bool>,
}

/// The `[render]` table: how aterm repairs a displaced Claude Code tab.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct RenderConfig {
    /// `repaint_nudge`: `"on"` (DEFAULT: aterm tells that one program its
    /// terminal is one row shorter, waits for its repaint, tells it the true
    /// size, waits for the second repaint - no key is sent, the window and the
    /// screen aterm draws are never resized), `"log"` (everything but the two
    /// size changes runs, and what would have been done goes to `aterm.log`) or
    /// `"off"`. Any case. Only Claude Code is ever nudged, whatever the value.
    /// Read once at launch: relaunch to apply a change. A word aterm does not
    /// know reads as `log` (it never acts on a word it cannot read) and is
    /// reported by the config diagnostics.
    pub(crate) repaint_nudge: Option<String>,
}

/// The `[harness]` table as the supervisor reads it: the policy every agent
/// session this window hosts is supervised under, and one note per key its
/// reader refused or found outside the root table.
///
/// Read by [`aterm_agent::supervise::SupervisorConfig::from_aterm_toml`], the
/// same reader `aterm drive watch|supervise` and the live-upgrade sweep use
/// (owner, 2026-09-24).
/// Every power defaults on and the file can only take power away, by an
/// explicit off spelling; any other value keeps the default, and a `false`
/// under another table still limits. Part of [`Config`]'s `PartialEq`, so a
/// `[harness]`-only reload is not deduped.
#[derive(Default, Clone, Debug, PartialEq)]
pub(crate) struct HarnessPolicy {
    /// The policy.
    pub(crate) policy: aterm_agent::supervise::SupervisorConfig,
    /// What the reader said about the file.
    pub(crate) notes: Vec<String>,
}

impl HarnessPolicy {
    /// The policy `text` — the whole `aterm.toml` — writes.
    pub(crate) fn read(text: &str) -> Self {
        let (policy, notes) = aterm_agent::supervise::SupervisorConfig::from_aterm_toml(text);
        Self { policy, notes }
    }

    /// The policy the file at `path` writes: no file is the defaults.
    pub(crate) fn read_path(path: Option<&std::path::Path>) -> Self {
        let (policy, notes) = aterm_agent::supervise::SupervisorConfig::from_path(path);
        Self { policy, notes }
    }

    /// The policy as Settings ▸ Harness shows it: `Automatic` at full-power
    /// defaults, `Off` when the master switch is, else
    /// `Automatic · limited: <key: value> · …`, plus `· yours: <key>, …` for
    /// non-limiting changes. Classification is
    /// `SupervisorConfig::against_default`'s; this only formats it.
    pub(crate) fn words(&self) -> String {
        if !self.policy.enabled {
            return "Off: no agent session is supervised".to_string();
        }
        let written = self.policy.against_default();
        let mut words = "Automatic".to_string();
        if !written.limits.is_empty() {
            words += &format!(" \u{b7} limited: {}", written.limits.join(" \u{b7} "));
        }
        if !written.own.is_empty() {
            words += &format!(" \u{b7} yours: {}", written.own.join(", "));
        }
        words
    }
}

/// The `[output_streak]` table (`docs/DESIGN-output-streak-2026-08-30.md`). On
/// by default. Defaults and clamps live only in the
/// `Config::output_streak_*_or_default` resolvers; numeric values clamp rather
/// than reject so a typo never aborts the load.
///
/// Additive light only: cells are never mutated, and `OVER_INK_COV_CAP` keeps
/// text legible. `aterm_effects::output_streak` re-clamps every numeric value.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct OutputStreakConfig {
    /// Master switch. Absent means on except on Windows (see
    /// [`Config::output_streak_enabled_or_default`]).
    pub(crate) enabled: Option<bool>,
    /// Amplitude scale `0..=1` (default `1.0`). Non-finite fails OFF.
    pub(crate) intensity: Option<f32>,
    /// Comet tail length in cells, clamped into
    /// [`aterm_effects::output_streak::TAIL_MIN`]`..=`[`aterm_effects::output_streak::TAIL_MAX`].
    /// `u32` so an absurd value clamps instead of failing deserialization.
    pub(crate) tail: Option<u32>,
    /// Resident comet cap, clamped `1..=`[`aterm_effects::output_streak::MAX_COMETS`].
    /// Declared `u32` for the parse-then-clamp reason above.
    pub(crate) max_streaks: Option<u32>,
    /// Idle drain in seconds, clamped `2..=120`; nothing animates forever.
    pub(crate) idle_secs: Option<f32>,
    /// Whether the episode's pip and closing exhale may be recorded at all.
    /// Absent ⇒ ON (the same owner ruling: it is ONE feature).
    pub(crate) sound: Option<bool>,
}

/// The `[packages]` table (`atpkg`, `docs/TOOLCHAIN-PACKAGE-MANAGER.md` §11).
/// Defaults live only in [`Config::packages_enabled`] and
/// [`Config::packages_auto_install`]. `atpkg` reads the same keys (and the
/// undeclared `[packages.links]` sub-table) from this file; the GUI mirrors
/// the two user-facing keys with atpkg's resolution
/// (`atpkg::config::PackagesConfig::{enabled, auto_install}`). No environment
/// variable overrides it (owner, 2026-09-23: env vars are for development).
///
/// ```toml
/// [packages]
/// # enabled      = true    # Automatic updates — THE switch (default true)
/// # auto_install = true    # install the ALab toolset nobody named: the first-run
/// #                        # fill and new members of the signed set (default TRUE,
/// #                        # batteries included; uninstall/exclude/enabled=false win)
/// # exclude      = ["trust"]   # per-program opt-out
/// ```
///
/// Legacy spellings: `auto_update = false` reads as `enabled = false`, and
/// `seed_install` as `auto_install` when that is unset. `channel`/`include` are
/// ignored; `prefix`, `account` and `owner/repo` links are development-only.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct PackagesConfig {
    /// Automatic updates — THE switch. Absent ⇒ ON.
    pub(crate) enabled: Option<bool>,
    /// Legacy spelling of `enabled`: `auto_update = false` reads as off
    /// ([`Config::packages_enabled`]); Settings drops it when writing `enabled`.
    pub(crate) auto_update: Option<bool>,
    /// Install consent: may atpkg install toolset members nobody named (first-run
    /// fill, new members of the signed set)? Absent means on
    /// ([`Config::packages_auto_install`]). Read by atpkg; mirrored for Settings.
    pub(crate) auto_install: Option<bool>,
    /// Legacy spelling of `auto_install`, read when that is unset; Settings drops
    /// it when writing `auto_install`.
    pub(crate) seed_install: Option<bool>,
    /// A DEVELOPMENT setting (index owner repoint), consumed by a development build's
    /// atpkg only and shown nowhere in Settings.
    pub(crate) account: Option<String>,
    /// Narrowing-only exclude filter — the per-program opt-out, consumed by atpkg.
    pub(crate) exclude: Option<Vec<String>>,
}

/// Default `[privacy] warmup_hold_ms`: 120 s, the same bounded hold the
/// in-place apply gives an interacting user.
pub(crate) const PRIVACY_WARMUP_HOLD_MS_DEFAULT: u64 = 120_000;

/// Ceiling for `[privacy] warmup_hold_ms`: 10 min, clamped in
/// [`Config::privacy_warmup_hold_ms`] and reported by `--validate-config`. The
/// hold keeps the in-place apply from interrupting a system modal; a longer
/// hold would amount to pinning a build.
pub(crate) const PRIVACY_WARMUP_HOLD_MS_MAX: u64 = 600_000;

/// Default `[privacy] probe_interval_ms`: 5 s, a floor on re-probing and so the
/// worst-case lag before a dialog answer changes aterm's posture.
pub(crate) const PRIVACY_PROBE_INTERVAL_MS_DEFAULT: u64 = 5_000;

/// `[privacy] warmup`: whether the "Ask for folder access now" gesture on the
/// Security panel is offered. There is no first-launch value: a consent
/// action must never fire without a gesture.
///
/// Unknown spellings resolve to the default rather than failing the parse,
/// because [`crate::app_config::load_config`] discards the whole file on a
/// serde error; `--validate-config` names the typo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PrivacyWarmup {
    /// Never offered.
    Never,
    /// Offered as an explicit owner gesture, and reachable no other way: no
    /// control verb, no environment variable, no automatic trigger.
    #[default]
    OnRequest,
}

impl PrivacyWarmup {
    /// The config spelling, which is also the report spelling.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnRequest => "on-request",
        }
    }

    /// Every accepted spelling, in the order `--validate-config` lists them.
    pub(crate) const ALL: &'static [Self] = &[Self::Never, Self::OnRequest];

    /// Parse a config spelling (case-insensitive). `None` for anything else so
    /// the validator can name it; the resolver then uses the default.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        Self::ALL
            .iter()
            .copied()
            .find(|candidate| value.eq_ignore_ascii_case(candidate.as_str()))
    }
}

/// The `[privacy]` table (macOS consent / TCC,
/// `docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §4). Defaults live only in the
/// resolvers ([`Config::privacy_enabled`] and siblings).
///
/// It governs reporting plus one owner-initiated gesture; nothing here answers
/// a system dialog or widens aterm's access. There is no environment variable
/// or control verb for it, so a program inside a session cannot change it.
///
/// ```toml
/// [privacy]
/// # enabled            = true            # master switch; false ⇒ every consent field reads `unknown`
/// # check              = true            # the silent, prompt-free probes: Full Disk Access (opens
/// #                                      #   one file), the debugger rule (DevToolsSecurity -status)
/// #                                      #   and the Developer Tools answer (reads tccd's log)
/// # notice             = true            # the Full Disk Access question, asked once per real need (Open Settings / Not now)
/// # report_attribution = true            # per-session responsible-pid corroboration
/// # warmup             = "on-request"    # "never" | "on-request"  (no first-launch value, by design)
/// # warmup_folders     = ["documents", "desktop", "downloads"]   # "app-data" may be listed, but is not asked for by default
/// # warmup_hold_ms     = 120000          # hard cap on the in-place-apply hold while a warm-up runs
/// # probe_interval_ms  = 5000            # floor on re-probing; also the worst-case posture lag
/// # observer           = (absent)        # the tccd log observer (§3.6): absent = a narrow sentinel, false = off, true = the full stream;
/// #                                      #   false also stops the Developer Tools row's log read
/// # protected_roots    = []              # empty ⇒ aterm_containment::sbpl::PRIVATE_SUBDIRS
/// # auto_accept        = false           # RESERVED — not implemented; see below
/// ```
///
/// `auto_accept` is parsed but unimplemented; setting it earns a
/// `--validate-config` warning naming the grant that does work.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct PrivacyConfig {
    /// Master for the consent lane. Absent means on. Off, every consent field
    /// reads `unknown` (aterm stopped looking, not "denied"), no probe or notice
    /// runs and the warm-up is not offered.
    pub(crate) enabled: Option<bool>,
    /// Silent macOS access probes. Absent means on. The Full Disk Access probe
    /// opens one protected file read-only; the debugger-rule probe runs
    /// `DevToolsSecurity -status`
    /// (`aterm_containment::consent::devtools_status_command`); the Developer
    /// Tools answer is one bounded `/usr/bin/log show` of tccd's own log
    /// (`aterm_containment::consent::probe_devtool`). None can raise a dialog;
    /// all run only from inside the app bundle.
    pub(crate) check: Option<bool>,
    /// The one-time macOS access card (`consent_card`), raised at launch while
    /// Full Disk Access is denied on a grant-stable build; *Not now* is recorded
    /// for the life of the config directory. Absent means on.
    pub(crate) notice: Option<bool>,
    /// Per-session responsible-process corroboration on reports. Absent means on.
    /// aterm's adoption record still decides `live` vs `adopted`.
    pub(crate) report_attribution: Option<bool>,
    /// `"never"` | `"on-request"`. Absent ⇒ `on-request`. See [`PrivacyWarmup`]
    /// for why there is no first-launch value.
    pub(crate) warmup: Option<String>,
    /// Which folders the warm-up walks, by NAME
    /// (`aterm_containment::consent::Folder`). Absent ⇒
    /// [`Folder::WARMUP_DEFAULT`](aterm_containment::Folder::WARMUP_DEFAULT): the
    /// three folders, not `app-data` (macOS forgets that answer when the process
    /// that received it is replaced; the name stays valid in an explicit list). An explicit empty
    /// list means exactly what it says — nothing to walk — which is NOT how
    /// `protected_roots` reads an empty list, on purpose: one is a work list,
    /// the other is a policy that must never resolve to "nothing is sensitive".
    pub(crate) warmup_folders: Option<Vec<String>>,
    /// Cap in ms on the in-place-apply hold while a warm-up runs. Absent means
    /// [`PRIVACY_WARMUP_HOLD_MS_DEFAULT`]; clamped to [`PRIVACY_WARMUP_HOLD_MS_MAX`].
    pub(crate) warmup_hold_ms: Option<u64>,
    /// Floor on re-probing, in ms. Absent ⇒
    /// [`PRIVACY_PROBE_INTERVAL_MS_DEFAULT`].
    pub(crate) probe_interval_ms: Option<u64>,
    /// The `tccd` log OBSERVER (design §3.6, `crate::consent_observer`), a
    /// tri-state. ABSENT ⇒ the SENTINEL: one `/usr/bin/log stream` child with
    /// a predicate that matches only the line macOS writes when it shows a
    /// consent dialog (`AUTHREQ_PROMPTING`), silent between dialogs, whose
    /// only job is to tell the card that a real need exists. `false` ⇒ off.
    /// `true` ⇒ the FULL stream, the correlation table, the tab mark and the
    /// per-dialog banner. It depends on admin-group membership (it sees
    /// nothing otherwise, and says `unavailable`) and reads an undocumented
    /// message format. Config only: there is deliberately no environment
    /// variable and no control verb, so a program inside a session cannot
    /// enable a consent surface. `false` also stops the Developer Tools row's
    /// `log show` (`refused_observer_off`): it is a read of the same log.
    pub(crate) observer: Option<bool>,
    /// Protected roots. Absent or empty means
    /// `aterm_containment::sbpl::PRIVATE_SUBDIRS` under `$HOME`, the set
    /// Containment denies. Entries must be absolute or `~/`-prefixed; others are
    /// ignored and named by `--validate-config`.
    pub(crate) protected_roots: Option<Vec<String>>,
    /// Reserved and unimplemented: aterm never answers consent dialogs. Parsed
    /// only so `--validate-config` can explain that; no resolver reads it.
    pub(crate) auto_accept: Option<bool>,
}

/// The `[reroute]` table, read by atpkg and mirrored here with atpkg's
/// resolution. `announce` (default on): a stub prints its line before running
/// an upstream tool. `strict` (default off): refuse instead of warn (`z3`
/// refused, a missing Trust tool not replaced by upstream, a stub that cannot
/// reach atpkg failing closed) (owner, 2026-09-08).
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct RerouteConfig {
    pub(crate) announce: Option<bool>,
    pub(crate) strict: Option<bool>,
}

/// The `[keeper]` table: `enabled` (default off) registers each terminal with
/// this build's PTY keeper (`aterm keeper start`) so a launch after a crash
/// reattaches its shells.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct KeeperConfig {
    pub(crate) enabled: Option<bool>,
}

impl Config {
    /// Whether the PTY keeper is on (`[keeper] enabled`, default `false`).
    pub(crate) fn keeper_enabled(&self) -> bool {
        self.keeper
            .as_ref()
            .and_then(|keeper| keeper.enabled)
            .unwrap_or(false)
    }
}

/// `[machine]`: macOS host settings `atpkg` applies on every package pass and on
/// `aterm pkg machine apply`, mirrored so Settings ▸ Security can show them.
/// `atpkg::config::MachineConfig` must resolve the same defaults
/// (`machine_defaults_agree_across_the_two_config_readers`).
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct MachineConfig {
    /// `"off"` (default) writes `com.apple.universalcontrol Disable`/`DisableMagicEdges`
    /// for the current host when they are not already set; `"leave"` never touches them.
    pub(crate) universal_control: Option<String>,
    /// Rename every cargo target dir the doctor's scan finds under `$HOME` to its
    /// `.noindex` form, leaving a `target` symlink so cargo keeps working. Default `true`.
    pub(crate) spotlight_noindex: Option<bool>,
}

/// The `[net]` table: the inbound listener and the outbound `[[net.connections]]`
/// registry. Absent means no port bound and nothing to dial. No environment
/// variable overrides it, and the listener binds only in a top-level aterm
/// (see [`crate::net_listen`]).
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
/// `fingerprint` is what the remote listener prints at startup. Provision the
/// token once with `aterm ctl dial-token work-box <token>` (Keychain, else a
/// 0600 file), then `aterm ctl dial work-box <verb>`. The token is the remote's
/// persistent `aterm-network-drive.token` beside its `[net] key`; only when that
/// file is unusable does the listener use a per-launch token printed at startup.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct NetConfig {
    /// Inbound numeric `IP:port` for the TLS listener (`"0.0.0.0:7100"`,
    /// `"[::1]:7100"`). Hostnames are rejected so validation never blocks on DNS.
    /// Absent means off.
    pub(crate) listen: Option<String>,
    /// Path to the operator's server certificate (DER).
    pub(crate) cert: Option<String>,
    /// Path to the server private key (PKCS#8 DER).
    pub(crate) key: Option<String>,
    /// Saved remote endpoints this aterm can `dial <name>` to drive.
    pub(crate) connections: Vec<Connection>,
}

/// One saved remote in `[[net.connections]]`. The drive token lives in the
/// macOS Keychain (`aterm-net-drive`/`<name>`) or a 0600 `token_file`
/// ([`crate::net_connections::resolve_token`]).
///
/// Optional `sid`/`expect_nonce` pin the remote session: with `expect_nonce`
/// set, the dialer checks [`aterm_net::RemoteEndpoint::matches`] against the
/// remote's `sessions bridge` roster before relaying and refuses a relaunched or
/// closed session ([`crate::net_connections::dial_relay`]). Without them only
/// the cert `fingerprint` is enforced.
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
    /// OPTIONAL: the remote session id whose live launch nonce `expect_nonce` pins;
    /// absent ⇒ any remote session carrying `expect_nonce` satisfies the pin.
    #[serde(default)]
    pub(crate) sid: Option<String>,
    /// Optional remote launch nonce to pin. Present, the dialer checks the
    /// remote's roster before relaying and fails closed if it is missing.
    #[serde(default)]
    pub(crate) expect_nonce: Option<String>,
}

/// The `[fabric]` table: the cross-aterm message bus, off by default.
///
/// ```toml
/// [fabric]
/// command = "aterm-link serve --fleet lab --broker /run/astream.sock --cap-file ~/.config/aterm/fabric.cap"
/// ```
///
/// The command is split on whitespace and executed directly (no shell). The
/// child inherits only two socketpair ends at fds 3 and 4; its authority is the
/// descriptor, with no token or socket path to steal.
///
/// The instance supervises it and relaunches with back-off. When a descriptor
/// closes, existing holds stay and governed sessions keep running with
/// `status hold_advisory=fabric-lost`; `hold_on_bridge_lost = true` instead
/// holds each `reason=fabric-lost origin=fleet` before any relaunch.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct FabricConfig {
    /// The bridge command line. Absent or blank ⇒ the fabric is off.
    pub(crate) command: Option<String>,
    /// Whether a lost bridge holds the sessions it governed
    /// (`crate::fabric::bridge_lost`). Absent means `false` (advisory only).
    pub(crate) hold_on_bridge_lost: Option<bool>,
}

/// The `[operator]` table: whether this process starts the embedded operator.
///
/// ```toml
/// [operator]
/// enabled = true   # then `aterm fleet manage <sid>`
/// ```
///
/// Off by default: an unconfigured operator observes nothing yet costs a
/// thread, a WAL under the state root and a place in the self-update path
/// (`docs/OPERATOR-EMBEDDED.md`). Read once, at launch.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct OperatorConfig {
    /// Start the embedded operator. Absent ⇒ `false`.
    pub(crate) enabled: Option<bool>,
}

impl Config {
    /// Whether this process starts the embedded operator (`[operator] enabled`).
    pub(crate) fn operator_enabled(&self) -> bool {
        self.operator
            .as_ref()
            .and_then(|operator| operator.enabled)
            .unwrap_or(false)
    }
}

/// The `[update]` table: whether the self-updater runs (`enabled`), whether a
/// staged build applies itself (`auto_apply`), and, in development builds only,
/// where releases come from. Absent means on, applied at once, from the
/// compiled-in public channel; a shipped binary ignores `owner`/`repo`
/// ([`aterm_update::Source::resolve`]). No environment variable overrides it.
///
/// ```toml
/// [update]
/// owner = "my-org"     # github.com/<owner>/<repo>
/// repo  = "aterm"
/// ```
///
/// This only chooses the source. The pinned Team ID and Apple notarization
/// anchor authenticity, so a repointed source cannot stage an untrusted bundle.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct UpdateConfig {
    /// "Check for updates automatically" (Settings ▸ Software Update). Absent
    /// means on. Off stops the background checker (`aterm_update::automatic`);
    /// manual checks and applies still run. Read once per process by
    /// `aterm_update_core::settings::update_enabled`, so a change applies at the
    /// next launch.
    pub(crate) enabled: Option<bool>,
    /// GitHub owner (`github.com/OWNER/REPO`). Absent means `alabsystems`.
    /// Honoured only by development builds
    /// (`aterm_update_core::REPOINT_IS_A_DEV_SEAM`); not shown in Settings.
    pub(crate) owner: Option<String>,
    /// GitHub repository name. Absent ⇒ default (`aterm`). A development setting, like
    /// `owner`.
    pub(crate) repo: Option<String>,
    /// Apply a staged update immediately (default on): the handoff carries every
    /// window, tab and split across the re-exec. `false` stages it and waits for
    /// a click (Version menu, palette, tab-strip ↻, Settings ▸ Software Update);
    /// the next launch picks up a stage only if no handoff ran.
    pub(crate) auto_apply: Option<bool>,
    /// Require Apple Developer-ID signing and notarization for self-updates.
    ///
    /// Shipped aterm compiles its Team ID in, which always wins; this matters
    /// only to forks with an empty compiled pin, where absent means structural
    /// `codesign --verify` only (authenticity then comes from the channel, the
    /// manifest SHA-256 and any configured Ed25519 signature).
    ///
    /// Set it to a Team ID to require every staged bundle to be signed by that
    /// team and accepted by Gatekeeper.
    ///
    /// ```toml
    /// [update]
    /// require_team_id = "ABCDE12345"
    /// ```
    ///
    /// It can only tighten. Development builds only
    /// ([`update_required_team_id`]); not shown in Settings. See
    /// `aterm_update::set_required_team_id`.
    pub(crate) require_team_id: Option<String>,
}

/// `[update] require_team_id` as this build honours it: the configured value in
/// a development build (`debug_assertions` or `dev-seams`); `None` in a shipped
/// binary, whose compiled pin is its only anchor.
pub(crate) fn update_required_team_id(config: &Config) -> Option<&str> {
    if !cfg!(any(debug_assertions, feature = "dev-seams")) {
        return None;
    }
    config
        .update
        .as_ref()
        .and_then(|u| u.require_team_id.as_deref())
}

/// The `update.auto_apply` answer; `[update] auto_apply` is the only control
/// (owner, 2026-09-23).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoApplySetting {
    /// The default: the in-session lane applies a staged build by itself.
    On,
    /// `[update] auto_apply = false`.
    OffByConfig,
}

/// Whether a staged update applies by itself: `update.auto_apply` (default
/// true). Read fresh every time, never memoised: a `OnceLock` whose initializer
/// asks its own owner would deadlock.
pub(crate) fn update_auto_apply_setting(config: &Config) -> AutoApplySetting {
    if config
        .update
        .as_ref()
        .and_then(|u| u.auto_apply)
        .unwrap_or(true)
    {
        AutoApplySetting::On
    } else {
        AutoApplySetting::OffByConfig
    }
}

/// "Check for updates automatically" as saved (`[update] enabled`, default on):
/// the value the next launch reads. Settings shows it beside the running value
/// (`App::update_checks_running`).
pub(crate) fn update_checks_automatic(config: &Config) -> bool {
    config
        .update
        .as_ref()
        .and_then(|u| u.enabled)
        .unwrap_or(true)
}

/// [`update_auto_apply_setting`] as the bool the lane's arm and poll read.
pub(crate) fn update_auto_apply(config: &Config) -> bool {
    matches!(update_auto_apply_setting(config), AutoApplySetting::On)
}

/// Default in-grid tab strip rows per platform: `0` on macOS (tabs live in the
/// native toolbar), `1` elsewhere, where the strip is the only tab UI. Config
/// `tab_strip_rows = N` overrides it.
#[cfg(target_os = "macos")]
pub(crate) const DEFAULT_TAB_STRIP_ROWS: u16 = 0;
/// See the macOS variant above — non-macOS defaults the in-grid strip ON.
#[cfg(not(target_os = "macos"))]
pub(crate) const DEFAULT_TAB_STRIP_ROWS: u16 = 1;
/// Upper clamp on `tab_strip_rows` so a mis-set config can't starve the terminal.
pub(crate) const MAX_TAB_STRIP_ROWS: u16 = 4;

/// Resolve the configured tab-strip row count (config, else
/// [`DEFAULT_TAB_STRIP_ROWS`]), clamped to `0..=MAX_TAB_STRIP_ROWS`.
pub(crate) fn resolve_tab_strip_rows(config: &Config) -> u16 {
    config
        .tab_strip_rows
        .unwrap_or(DEFAULT_TAB_STRIP_ROWS)
        .min(MAX_TAB_STRIP_ROWS)
}

/// Search index depth from `search_history_lines`: how many of the newest lines
/// the trigram index retains. Unset means
/// [`DEFAULT_MAX_CACHED_LINES`](aterm_core::search::DEFAULT_MAX_CACHED_LINES).
/// Pushed to [`crate::control::set_search_max_lines`] at startup and reload.
pub(crate) fn search_index_depth(config: &Config) -> usize {
    config
        .search_history_lines
        .map_or(aterm_core::search::DEFAULT_MAX_CACHED_LINES, |n| n as usize)
}

/// Resolve a plain or appearance-split theme selector permissively: an
/// unrecognized segment beside a valid `dark:`/`light:` side is ignored and an
/// omitted side is Default. Shared by the renderer and Settings preview.
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
    /// Parse `aterm.toml` text: the typed config plus the `[harness]` policy
    /// ([`HarnessPolicy::read`]). Every load (launch and config service) goes
    /// through here.
    pub(crate) fn parse(text: &str) -> Result<Self, aterm_toml::Error> {
        let mut config: Self = aterm_toml::from_str(text)?;
        config.harness = HarnessPolicy::read(text);
        Ok(config)
    }

    /// Config used when `text` does not parse: all defaults, but `[harness]`
    /// lines still read (its reader is line-based), so a limit there survives a
    /// typo elsewhere.
    pub(crate) fn stand_in(text: &str) -> Self {
        Self {
            harness: HarnessPolicy::read(text),
            ..Self::default()
        }
    }

    /// Scheme name this config selects for `appearance`, honouring
    /// `theme = "dark:<name>,light:<name>"`. A plain value applies to both; an
    /// omitted side is `None` (built-in Default). Prefixes are case- and
    /// space-insensitive; the name keeps its case.
    pub(crate) fn resolve_theme_name(&self, appearance: aterm_types::Appearance) -> Option<String> {
        resolve_theme_name_value(self.theme.as_deref(), appearance)
    }

    /// Base color scheme for `appearance` (see [`Self::resolve_theme_name`]): the
    /// named built-in, a user theme file of that name, or
    /// [`aterm_types::ColorScheme::default`]. Callers layer the per-key color
    /// overrides on top.
    fn base_scheme_for_with_themes(
        &self,
        appearance: aterm_types::Appearance,
        themes: &ThemeCatalog,
    ) -> aterm_types::ColorScheme {
        // Resolves silently (a bad name gives Default); [`theme_misses`] reports
        // it against the same catalog.
        match self.resolve_theme_name(appearance) {
            None => aterm_types::ColorScheme::default(),
            Some(name) => themes.resolve(&name).unwrap_or_default(),
        }
    }

    /// The renderer theme (clear colour, cursor, selection): the scheme's chrome
    /// ([`Self::base_scheme_for_with_themes`]) with per-key color overrides applied.
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
        if let Some(c) = self.foreground.as_deref().and_then(parse_hex_color) {
            t.fg = rgb_u32(c);
        }
        if let Some(c) = self.background.as_deref().and_then(parse_hex_color) {
            t.bg = rgb_u32(c);
        }
        if let Some(c) = self.cursor_color.as_deref().and_then(parse_hex_color) {
            t.cursor = rgb_u32(c);
        }
        if let Some(c) = self.selection_color.as_deref().and_then(parse_hex_color) {
            t.selection = rgb_u32(c);
        }
        t
    }

    /// Whether Alt/Option sends ESC-prefixed (Meta) sequences. Default `true`,
    /// matching the engine encoder.
    pub(crate) fn option_as_meta_or_default(&self) -> bool {
        self.option_as_meta.unwrap_or(true)
    }

    /// Whether a paste holding a line end (a trailing one included) is confirmed
    /// when bracketed paste is off. Default `true`; `confirm_multiline_paste =
    /// false` disables it.
    pub(crate) fn confirm_multiline_paste_or_default(&self) -> bool {
        self.confirm_multiline_paste.unwrap_or(true)
    }

    /// Whether the previous graceful quit's windows, tabs and panes (with cwd)
    /// reopen at launch. Default `true`.
    pub(crate) fn restore_session_or_default(&self) -> bool {
        self.restore_session.unwrap_or(true)
    }

    /// Whether a completed mouse selection copies to the clipboard. Default
    /// `true` except on Linux, where the selection already owns PRIMARY and the
    /// clipboard is kept for explicit copies; `true` there writes both.
    pub(crate) fn copy_on_select_or_default(&self) -> bool {
        self.copy_on_select
            .unwrap_or(cfg!(not(target_os = "linux")))
    }

    /// Master switch for generated live Activity. Default on; `false` or
    /// `title_summary_provider = "off"` keeps Title/Description composition and
    /// disables only the generated fallback.
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

    /// Explicit endpoint for the resolved provider. An absent Ollama value stays
    /// `None` (the worker picks a private ephemeral endpoint); an
    /// OpenAI-compatible provider fails closed without one. In-process and
    /// disabled providers ignore it.
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

    /// Whether aterm primes detected coding agents on each fresh session spawn.
    /// Default on; `false` reads and writes nothing.
    pub(crate) fn agents_auto_prime_or_default(&self) -> bool {
        self.agents_auto_prime.unwrap_or(true)
    }

    /// Master switch for status classification. Default on; `false` stops
    /// evidence gathering entirely.
    pub(crate) fn tab_status_or_default(&self) -> bool {
        self.tab_status.unwrap_or(true)
    }

    /// Whether a status sets the tab's indicator bits, independent of
    /// [`Self::tab_status_or_default`].
    pub(crate) fn tab_status_badge_or_default(&self) -> bool {
        self.tab_status_badge.unwrap_or(true)
    }

    /// Whether the connection role marks the tab. Default on; hides only the
    /// mark.
    pub(crate) fn tab_connection_badge_or_default(&self) -> bool {
        self.tab_connection_badge.unwrap_or(true)
    }

    /// Whether the band may explain very heavy system use (the strain row).
    /// Opt-OUT; see the field doc.
    pub(crate) fn explain_heavy_load_or_default(&self) -> bool {
        self.explain_heavy_load.unwrap_or(true)
    }

    /// Whether aterm posts its own notices to the desktop. Opt-IN (default
    /// off); see the field doc.
    pub(crate) fn desktop_alerts_or_default(&self) -> bool {
        self.desktop_alerts.unwrap_or(false)
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

    /// Observation budget interval, derived from dwell: it tracks dwell downward
    /// (a coarser interval would make dwell unserviceable) and stops at a floor
    /// so an output flood is not a classification flood.
    pub(crate) fn tab_status_observe_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.tab_status_dwell_ms_or_default().clamp(
            MIN_TAB_STATUS_OBSERVE_INTERVAL_MS,
            NOMINAL_TAB_STATUS_OBSERVE_INTERVAL_MS,
        ))
    }

    /// Whether the cursor motion trail is on. Default
    /// [`DEFAULT_DECORATIVE_EFFECTS`]: on except on Windows.
    ///
    /// When on, each cursor move ignites a ~260 ms additive aurora that decays to
    /// zero at idle; the GPU bloom pass runs only on effect frames and sheds
    /// under load. The default style also draws a resident cat over output, which
    /// is why Windows defaults it off. Off, the four expensive effect pipelines
    /// only this master binds are never requested
    /// ([`Self::warms_effect_pipelines`]). An explicit `cursor_trail = true`
    /// turns it on identically on every platform.
    pub(crate) fn cursor_trail_or_default(&self) -> bool {
        self.cursor_trail.unwrap_or(DEFAULT_DECORATIVE_EFFECTS)
    }

    /// Typing-momentum glow switch (default on). The glow also needs the
    /// effects master ([`Self::cursor_trail_or_default`]), folded in by the
    /// frame step.
    pub(crate) fn cursor_momentum_glow_or_default(&self) -> bool {
        self.cursor_momentum_glow.unwrap_or(true)
    }

    /// Whether a config apply should warm the demand-driven effect pipelines
    /// (`aterm_gpu::EffectPipeline`) off the frame path.
    ///
    /// Effect pipelines are built lazily by `encode_frame`; this only decides
    /// whether a config apply compiles them early to avoid a first-frame hitch.
    /// Keyed on the cursor-trail master because it alone can bind the pipelines
    /// with a visible build cost (`fire_add`, `fire_over`, `rain_glow`,
    /// `rain_glow_over`); with the trail off, warming would compile pipelines
    /// that are never drawn.
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

    /// Kitty-command gate: `[sparkle_words] enabled` (via
    /// [`Self::sparkle_words_enabled_or_default`]) and
    /// `[sparkle_words.tricks] enabled` (default on). Read once per press and
    /// used only at a listener event; the listener's line state is maintained
    /// regardless. Serious mode is the caller's second gate. Independent of the
    /// family bits: with every family off the pet still obeys.
    pub(crate) fn kitty_tricks_enabled(&self) -> bool {
        self.sparkle_words_enabled_or_default()
            && self
                .sparkle_words
                .as_ref()
                .and_then(|sw| sw.tricks.as_ref())
                .and_then(|t| t.enabled)
                .unwrap_or(true)
    }

    /// Whether `word` (a trick token as typed) is on
    /// `[sparkle_words.tricks] ignore_words`. Both sides use the lexicon's fold
    /// without elongation folding (`purr` does not cover `purrrrr`). Allocates
    /// only when the list is non-empty; runs once per fire.
    pub(crate) fn kitty_trick_ignored(&self, word: &str) -> bool {
        let Some(ignore) = self
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.tricks.as_ref())
            .and_then(|t| t.ignore_words.as_deref())
            .filter(|words| !words.is_empty())
        else {
            return false;
        };
        let folded = aterm_lexicon::fold(word);
        ignore.iter().any(|w| aterm_lexicon::fold(w) == folded)
    }

    /// Secure keyboard entry (default off, like every security opt-in: it
    /// suppresses other apps' global hotkeys while active).
    pub(crate) fn secure_keyboard_entry_or_default(&self) -> bool {
        self.secure_keyboard_entry.unwrap_or(false)
    }

    /// Rainbow sparkles on the post-update celebration (default ON — opt-OUT).
    pub(crate) fn notice_sparkle_or_default(&self) -> bool {
        self.notice_sparkle.unwrap_or(true)
    }

    /// Ambient-bed on/off (`trail_sound_bed`, default on).
    pub(crate) fn trail_sound_bed_or_default(&self) -> bool {
        // The pad is voiced from the live chord, so the derived line stays in key
        // (owner, 2026-09-09).
        self.trail_sound_bed.unwrap_or(true)
    }

    /// Sing-along riff on/off (`trail_sound_riff`, default on). Visuals continue
    /// when off, and focus, `trail_sounds` and `trail_sound_volume` apply first.
    pub(crate) fn trail_sound_riff_or_default(&self) -> bool {
        self.trail_sound_riff.unwrap_or(true)
    }

    /// Audible bell on/off (`bell_sound`, default on). Gates only the OS alert
    /// sound (`NSBeep` / `MessageBeep`), not the visual bell or attention
    /// request; `trail_sound_volume` does not apply.
    pub(crate) fn bell_sound_or_default(&self) -> bool {
        self.bell_sound.unwrap_or(true)
    }

    /// Choice chime on/off (`choice_sound`, default on). Gates only the chime,
    /// not the band flash or rim pulse. Read by `App::chose_chime` after serious
    /// mode and the Music master, before its rate limiter.
    pub(crate) fn choice_sound_or_default(&self) -> bool {
        self.choice_sound.unwrap_or(true)
    }

    /// Parsed `trail_sound_style` voice (default `"auto"`, follow the trail
    /// style), via [`aterm_effects::trail_sound::SoundVoice::parse`]
    /// (case-insensitive, aliases included) without allocating. Unknown
    /// spellings fall back to `auto`; `--validate-config` warns.
    pub(crate) fn trail_sound_voice(&self) -> aterm_effects::trail_sound::SoundVoice {
        use aterm_effects::trail_sound::SoundVoice;
        self.trail_sound_style
            .as_deref()
            .and_then(SoundVoice::parse)
            .unwrap_or_default()
    }

    /// [`Self::trail_sound_voice`] for a trail gesture under one resolved
    /// presentation (`RAINBOW-KITTY-V2.md` §16 row 9): with `auto` and a rainbow
    /// kitty trail, the voice is the music box
    /// ([`aterm_effects::trail_sound::SoundVoice::RainbowKittyV2`]), named on the
    /// event. An explicit voice is never overridden. Trail gestures only; the
    /// bonk, riff and output pips use the plain read.
    pub(crate) fn trail_sound_voice_for(
        &self,
        presentation: ResolvedTrailPresentation,
    ) -> aterm_effects::trail_sound::SoundVoice {
        use aterm_effects::trail_sound::SoundVoice;
        match self.trail_sound_voice() {
            SoundVoice::Style
                if presentation.style.style
                    == Some(aterm_effects::cursor_glow::GlowStyle::RainbowKitty) =>
            {
                SoundVoice::RainbowKittyV2
            }
            voice => voice,
        }
    }

    /// `--validate-config` warning for an unknown `trail_sound_style` (at load
    /// it silently falls back to `auto`).
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

    /// Trail sound volume 0..1 (`trail_sound_volume`, default 0.4), clamped;
    /// NaN/Inf read as silent.
    pub(crate) fn trail_sound_volume(&self) -> f32 {
        match self.trail_sound_volume {
            Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
            Some(_) => 0.0,
            None => 0.4,
        }
    }

    /// Wallpaper dim fraction toward the theme background, clamped to `0..=1`;
    /// unset or non-finite reads 0.3.
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

    /// Parsed `motion` mode: `auto` (default), `full` or `reduced`; unknown
    /// strings mean `auto`. Allocation-free for the per-redraw caller
    /// (`App::motion_policy`).
    pub(crate) fn motion_mode(&self) -> crate::motion::MotionMode {
        self.motion.as_deref().map_or(
            crate::motion::MotionMode::Auto,
            crate::motion::MotionMode::parse,
        )
    }

    /// Whether load-adaptive shedding may force the reduced state for decorative
    /// effects under sustained render overload. Default `true`. `motion = "full"`
    /// overrides the shed; functional motion is never shed. See
    /// [`crate::App::motion_policy`] and [`crate::App::effect_policy`].
    pub(crate) fn load_adaptive_motion_or_default(&self) -> bool {
        self.load_adaptive_motion.unwrap_or(true)
    }

    /// Serious-mode master gate. Default off; the App keeps the resolved value
    /// so toggles apply as one transition without rewriting effect settings.
    pub(crate) fn serious_mode_or_default(&self) -> bool {
        self.serious_mode.unwrap_or(false)
    }

    /// Predictive echo mode (`off` / `adaptive` / `always`). Default `adaptive`:
    /// on once echo is confirmed on the current line and RTT is high enough to
    /// help; unechoed input is never shown. `always` is the aggressive variant,
    /// unsafe at prompts.
    pub(crate) fn predictive_echo_or_default(&self) -> &str {
        self.predictive_echo
            .as_deref()
            .map(str::trim)
            .unwrap_or("adaptive")
    }

    /// Focus-linked shell priority boost. Default on: a few syscalls per focus or
    /// tab change and none while typing; touches only the shell root and its
    /// console host.
    pub(crate) fn focus_boost_or_default(&self) -> bool {
        self.focus_boost.unwrap_or(true)
    }

    /// Stream fade master switch. Default off: it runs an O(rows×cols)
    /// fingerprint pass on the UI thread before every committed frame.
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
            .map(rgb_u32)
    }

    /// Primary-font request with the display-face override applied: a valid
    /// `display_font` becomes the virtual family `display:<id>`, or for a mix of
    /// 2..=3 `+`-joined ids `display:<id>+<id>[+<id>]` (each character picks one
    /// face deterministically). Otherwise `font_family` passes through. Unknown
    /// or duplicate ids and `"off"` are dropped, and at most three are kept.
    /// Every font resolution site goes through this.
    ///
    /// Legacy ids are canonicalized here (`minecraft` → `pixel`); an id with no
    /// successor (`mariokart`) falls through to `font_family`.
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

    /// Active tab color as `[r, g, b]` when `active_tab_color` is a valid
    /// `#RRGGBB`; `None` keeps the translucent system default.
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
            .map(rgb_u32)
    }

    /// Configured cursor-trail style, trimmed but not lowercased. Borrowed to
    /// avoid allocating at status, validation and preview seams.
    pub(crate) fn cursor_trail_style_raw(&self) -> &str {
        // Default style comes from its one definition in `prefs`.
        self.cursor_trail_style
            .as_deref()
            .unwrap_or(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE)
            .trim()
    }

    /// Trail brightness, default 1.0, clamped 0.0..=1.0. Non-finite values
    /// (`nan` is valid TOML) read as `0.0`, since NaN would slip past every
    /// downstream `intensity <= 0.0` check.
    pub(crate) fn cursor_trail_intensity_or_default(&self) -> f32 {
        let v = self.cursor_trail_intensity.unwrap_or(1.0);
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

    /// `[sparkle_words] suppress_in_alt_screen`. Default false: the alternate
    /// screen is decorated too; `true` leaves full-screen program UI undecorated.
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
            crate::logging::stderr_line!(
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
                    crate::logging::stderr_line!(
                        "aterm-gui: sparkle_words.toy_packs[{index}] {expanded:?} \
                         unreadable ({error}); skipping"
                    );
                    continue;
                }
            };
            let pack = match aterm_effects::spec::compile_toy_pack_toml(&source) {
                Ok(pack) => pack,
                Err(error) => {
                    crate::logging::stderr_line!(
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

    /// Resolve one immutable Trail Pack catalog; the only manifest I/O
    /// primitive. Unreadable or invalid manifests are skipped and kept as
    /// diagnostics (data, not stderr, so each host reports them once); a later
    /// duplicate id wins.
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
        if let Some(paths) = self.cursor_trail_packs.as_deref() {
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
        }
        loaded.ids = loaded.packs.keys().cloned().collect();
        loaded.ids.sort();
        loaded.cache_presentation(self.cursor_trail_style_raw());
        (std::sync::Arc::new(loaded), fingerprint.finish())
    }

    /// Resolve the non-feed part of a catalog while the Trail/Sparkle generation
    /// is prepared separately: admits the kitty sprite and installs an empty
    /// Trail placeholder that must be replaced before publication.
    pub(crate) fn resolve_preliminary_asset_catalog_with_themes(
        &self,
        themes: std::sync::Arc<ThemeCatalog>,
    ) -> std::sync::Arc<ConfigAssetCatalog> {
        let mut trail_packs = TrailPackCatalog::default();
        trail_packs.cache_presentation(self.cursor_trail_style_raw());
        std::sync::Arc::new(ConfigAssetCatalog {
            trail_packs: std::sync::Arc::new(trail_packs),
            kitty_sprite: resolve_kitty_sprite_asset(self.cursor_nyan_sprite.as_deref()),
            wallpaper: resolve_wallpaper_asset(self.wallpaper.as_deref()),
            themes,
            sparkle_spec_consumers: None,
        })
    }

    /// Resolve every filesystem-backed visual asset for one config generation;
    /// the only production kitty PNG decode seam. The config service calls it
    /// before advancing the revision and publishes the `Arc` with the TOML text.
    pub(crate) fn resolve_asset_catalog_with_themes(
        &self,
        themes: std::sync::Arc<ThemeCatalog>,
    ) -> std::sync::Arc<ConfigAssetCatalog> {
        // Toy Packs are compiled by the preparation lane, so this preliminary
        // catalog reports `None` rather than an incomplete inline-only answer.
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

    /// Content fingerprints of every active file the config references by path
    /// ([`PathFeedFps`]), streamed through each loader's bounded admission (test
    /// oracle; production carries the fingerprint from its consumer read).
    /// Disabled Sparkle feeds are skipped. The path and readability are part of
    /// each stream, and pack lists are capped at `MAX_ACTIVE_TOY_PACKS` like the
    /// loaders.
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

    /// Toy Pack admission checks for `--validate-config`; normal startup does not
    /// pay for this second read.
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

    /// Resolve the pack/config bundle once, from `recompute_sparkle` (startup,
    /// reload, re-enable), never per frame.
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
            // A disabled feed has no consumer; re-enabling it forces a fresh
            // worker generation, so dormant paths need no I/O.
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

    /// Compile the complete sparkle runtime on a host worker (or once at
    /// startup, before the event loop).
    #[cfg(test)]
    pub(crate) fn prepare_sparkle_runtime(&self) -> PreparedSparkleRuntime {
        self.prepare_sparkle_runtime_with_fingerprint().0
    }

    fn prepare_sparkle_runtime_with_fingerprint(&self) -> (PreparedSparkleRuntime, u64) {
        let (parts, fingerprint) = self.sparkle_runtime_parts_with_fingerprint();
        let langs = self.sparkle_languages();
        // The kitty-command vocabulary uses the same worker and `languages` as the
        // word lexicon and is compiled whether or not a word engine resolved.
        // Its data problems indicate bad embedded data, never user error.
        let tricks = {
            let refs: Vec<&str> = langs.iter().map(String::as_str).collect();
            let tricks = aterm_lexicon::TrickLexicon::with_languages(&refs);
            for conflict in tricks.conflicts() {
                crate::logging::stderr_line!("aterm-gui: kitty commands vocabulary: {conflict}");
            }
            std::sync::Arc::new(tricks)
        };
        let resolved = parts.map(|(cfg, override_toml)| {
            let refs: Vec<&str> = langs.iter().map(String::as_str).collect();
            let lexicon = aterm_lexicon::Lexicon::with_languages_and_override(
                &refs,
                override_toml.as_deref(),
            )
            .unwrap_or_else(|error| {
                crate::logging::stderr_line!(
                    "aterm-gui: sparkle words lexicon override rejected ({error}); using builtin"
                );
                aterm_lexicon::Lexicon::with_languages(&refs)
            });
            for warning in sparkle_logged_warnings(lexicon.conflicts(), cfg.cjk_single_char) {
                crate::logging::stderr_line!("aterm-gui: sparkle_words lexicon: {warning}");
            }
            crate::word_decorations::Resolved {
                cfg,
                lexicon: std::sync::Arc::new(lexicon),
            }
        });
        (PreparedSparkleRuntime { resolved, tricks }, fingerprint)
    }

    /// Files this config names by path whose content is applied state: the
    /// Sparkle lexicon and Toy Packs (unless `[sparkle_words]` is disabled) and
    /// the Trail Packs, capped at [`MAX_ACTIVE_TOY_PACKS`] and expanded like the
    /// loaders. The config watcher stats these beside `aterm.toml`;
    /// [`PathFeedFps`] decides from the bytes whether anything changed.
    pub(crate) fn path_feed_paths(&self) -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        if let Some(sparkle) = self
            .sparkle_words
            .as_ref()
            .filter(|sparkle| sparkle.enabled != Some(false))
        {
            paths.extend(sparkle.lexicon.as_deref().map(sparkle_expand_tilde));
            paths.extend(
                sparkle
                    .toy_packs
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .take(MAX_ACTIVE_TOY_PACKS)
                    .map(|path| sparkle_expand_tilde(path)),
            );
        }
        paths.extend(
            self.cursor_trail_packs
                .as_deref()
                .unwrap_or_default()
                .iter()
                .take(MAX_ACTIVE_TOY_PACKS)
                .map(|path| sparkle_expand_tilde(path)),
        );
        paths
    }

    /// Every file besides `aterm.toml` whose content a warm of this config
    /// presents, by the path its reader opens (warm successor P2): the user
    /// theme file each appearance selects (`<theme dir>/<name>.conf`; a
    /// built-in name, or one that is not a file stem, opens none), the custom
    /// kitty sprite, the wallpaper, and the path feeds
    /// ([`Self::path_feed_paths`]). At most `2 + 2 + 1 + 2 ×
    /// MAX_ACTIVE_TOY_PACKS` paths. `main_entry` stamps them beside the file's
    /// text before its early warm (`crate::WarmConfigRead`), so an edit to one
    /// during the dial's hold is warmed again before any carried present.
    ///
    /// Not listed, on purpose:
    /// * the rest of the theme directory. The warm parses every `.conf` there,
    ///   but only the selected file reaches a present; another one changes the
    ///   Settings picker's list, which the watcher reconciles after Commit
    ///   with no repaint. The selected file is stamped at the config's
    ///   spelling, so on a case-sensitive volume a file whose name differs
    ///   from it only in case (which the catalog still resolves) and a
    ///   case-fold twin (which makes the catalog refuse the name) are not
    ///   seen, nor are files that push it past [`MAX_USER_THEME_FILES`].
    /// * fonts (the configured family and fallbacks, the system font
    ///   directories). No running process re-reads a font on a change, so the
    ///   outgoing process never parks under an edited one and nothing repaints
    ///   one after Commit.
    pub(crate) fn warm_read_paths(&self) -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        if let Some(directory) = aterm_types::scheme::user_theme_dir() {
            for appearance in [
                aterm_types::Appearance::Dark,
                aterm_types::Appearance::Light,
            ] {
                let Some(name) = self.resolve_theme_name(appearance) else {
                    continue;
                };
                if aterm_types::scheme::builtin(&name).is_some()
                    || safe_user_theme_name(&name).is_err()
                {
                    continue;
                }
                let path = directory.join(format!("{name}.conf"));
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        // Spelled as `resolve_kitty_sprite_asset` / `resolve_wallpaper_asset`
        // read them: trimmed, a blank value opens nothing, an over-long one is
        // refused unread.
        for raw in [
            self.cursor_nyan_sprite.as_deref(),
            self.wallpaper.as_deref(),
        ] {
            if let Some(raw) = raw
                .map(str::trim)
                .filter(|raw| !raw.is_empty() && raw.len() <= MAX_KITTY_SOURCE_ID_BYTES)
            {
                paths.push(sparkle_expand_tilde(raw));
            }
        }
        paths.extend(self.path_feed_paths());
        paths
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

    /// The `[sparkle_words] enabled` master, resolved in one place.
    ///
    /// Default on everywhere, outside [`DEFAULT_DECORATIVE_EFFECTS`]: matched
    /// words' heads draw `FreeZ::UnderText` so glyphs stay on top, the on-screen
    /// population is capped (`MAX_CATS = 8`), and Settings treats this key as a
    /// live product gate with its own disclosure ladder and migration path. The
    /// one over-text path is the profanity supernova burst, capped at one
    /// (`supernova::MAX_ACTIVE_SUPERNOVAE`) and tunable via `supernova_chance`.
    ///
    /// Callers: the kitty-command gate ([`Self::kitty_tricks_enabled`]) and tests.
    pub(crate) fn sparkle_words_enabled_or_default(&self) -> bool {
        self.sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.enabled)
            .unwrap_or(true)
    }

    /// Resolve `[sparkle_words]` into a renderer-ready [`DecoConfig`] with
    /// defaults and clamps applied. `None` when disabled or every category is
    /// off. An absent table enables profanity sparkle and feline cat-paw.
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
        let ink_cfg = sw.ink.clone().unwrap_or_default();
        let emph = sw.emphasis.clone().unwrap_or_default();
        // The profanity key is the Sparkle Words product gate for every
        // non-feline word effect, so its switch also stops emphasis and custom
        // recipes.
        let sparkle_words = prof.enabled.unwrap_or(true);
        let profanity = sparkle_words;
        let feline = fel.enabled.unwrap_or(true);
        // Dogs ride the Sparkle Words switch, with their own enable bit under it.
        let canine = sparkle_words && can_cfg.enabled.unwrap_or(true);
        let ink_enabled = ink_cfg.enabled.unwrap_or(true);
        // Custom-word spec table, keyed by the scanner's form_hash semantics
        // (folded spaced surfaces, possessive variants, raw CJK hashes).
        let custom_entries = sw.custom.clone().unwrap_or_default();
        let (inline_specs, _) = aterm_effects::spec::build_custom(&custom_entries);
        // Inline config overlays all imported packs; the Sparkle Words switch
        // makes both sources inert.
        if sparkle_words {
            spec_table.overlay(inline_specs);
        } else {
            spec_table = aterm_effects::spec::SpecTable::default();
        }
        // `emph.enabled` gates only ordinary, non-overridden emphasis matches.
        // Custom words resolve their override before the per-class gate
        // (`WordDecorations::scan_row`), so they stay independent of it;
        // `has_custom` keeps an emphasis lane admitted for graphic- or
        // burst-only customs.
        let emphasis = sparkle_words
            && emph.enabled.unwrap_or(true)
            && (ink_enabled || spec_table.has_custom());
        if !profanity && !feline && !canine && !emphasis {
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
        let palette: Vec<u32> = prof
            .palette
            .unwrap_or_default()
            .iter()
            .filter_map(|s| parse_hex_color(s))
            .map(rgb_u32)
            .collect();
        // The explicit `[sparkle_words] reduced_motion` override; the OS Reduce
        // Motion flag is folded in per frame through `MotionPolicy` (app_render).
        let reduced_motion = sw.reduced_motion.unwrap_or(false);
        // Alt-screen suppression is opt-in.
        let suppress_in_alt_screen = sw.suppress_in_alt_screen.unwrap_or(false);
        // Global `deny` + per-category `ignore_words`, folded so they match the
        // scanner's folded surfaces. Empty (the default) → no allocation churn.
        let mut ignore: std::collections::HashSet<String> = std::collections::HashSet::new();
        for words in [
            &sw.deny,
            &prof.ignore_words,
            &fel.ignore_words,
            &can_cfg.ignore_words,
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
            // `paw` is an ink-only mode; only `cat` emits a feline graphic.
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
            // "sparkle" and "nova" select those styles; anything else (including
            // absent) is rainbow. Trimmed and case-insensitive, matching Manual
            // and the web `set_sparkle_profanity` setter.
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
            // Escalation chance, 0..=100 (0 disables); default 10 as documented.
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

    /// Build the lexicon override TOML: external lexicon/category extras, then
    /// Toy Packs in configured order, then inline custom surfaces (highest
    /// precedence). Unreadable inputs are logged and skipped.
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
                        Err(e) => crate::logging::stderr_line!(
                            "aterm-gui: sparkle_words.lexicon {expanded:?} rejected ({e}); \
                             skipping that layer"
                        ),
                    }
                }
                Err(error) => {
                    crate::logging::stderr_line!(
                        "aterm-gui: sparkle_words.lexicon {expanded:?} unreadable ({error}); \
                         skipping that layer"
                    );
                }
            }
        }
        let prof = sw.profanity.clone().unwrap_or_default();
        let fel = sw.feline.clone().unwrap_or_default();
        let can_cfg = sw.canine.clone().unwrap_or_default();
        let emph = sw.emphasis.clone().unwrap_or_default();
        append_extra_words_entry(&mut out, "profanity", prof.extra_words.as_deref());
        append_extra_words_entry(&mut out, "feline", fel.extra_words.as_deref());
        append_extra_words_entry(&mut out, "canine", can_cfg.extra_words.as_deref());
        append_extra_words_entry(&mut out, "emphasis", emph.extra_words.as_deref());
        // Each strict pack was already production-scanner validated. Preserve
        // configured order so the lexicon document mirrors spec overlay order.
        out.push_str(pack_lexicon_toml);
        // Custom spec words append to the emphasis class (CJK surfaces as
        // `cjk = true` entries) so they scan; their specs ride the DecoConfig.
        if let Some(customs) = sw.custom.as_deref() {
            let (_, fragment) = aterm_effects::spec::build_custom(customs);
            out.push_str(&fragment);
        }
        (!out.is_empty()).then_some(out)
    }

    /// The durable `[matrix_rain]` enabled bit; absent means off. Separate from
    /// [`Config::matrix_rain_params`] because a per-session override resolves
    /// `override.unwrap_or(this)` and can force rain on.
    pub(crate) fn matrix_rain_enabled(&self) -> bool {
        self.matrix_rain
            .as_ref()
            .and_then(|mr| mr.enabled)
            .unwrap_or(false)
    }

    /// The `[render] repaint_nudge` mode: `on` when the key is absent, the
    /// word's own mode when it is `on`, `log` or `off`, and `log` for any other
    /// word: a word aterm cannot read is no leave to act (the config diagnostics
    /// report it). Read once, at launch.
    pub(crate) fn repaint_nudge_mode(&self) -> aterm_core::terminal::repaint_nudge::Mode {
        use aterm_core::terminal::repaint_nudge::Mode;
        self.render
            .as_ref()
            .and_then(|r| r.repaint_nudge.as_deref())
            .map_or_else(Mode::default, |word| {
                Mode::parse(word.trim()).unwrap_or(Mode::Log)
            })
    }

    /// The `[presence] band` RESOLVED bit (default TRUE): whether the presence
    /// band row is shown. See [`PresenceConfig`].
    pub(crate) fn presence_band_enabled(&self) -> bool {
        self.presence.as_ref().and_then(|p| p.band).unwrap_or(true)
    }

    /// The `[presence] rim` RESOLVED bit (default TRUE): whether the presence
    /// rim is painted.
    pub(crate) fn presence_rim_enabled(&self) -> bool {
        self.presence.as_ref().and_then(|p| p.rim).unwrap_or(true)
    }

    /// The `[claude_footer] usage` RESOLVED bit (default TRUE): whether the
    /// Claude footer shows `Σ` — working time and tokens. See
    /// [`ClaudeFooterConfig`].
    pub(crate) fn claude_footer_usage(&self) -> bool {
        self.claude_footer
            .as_ref()
            .and_then(|c| c.usage)
            .unwrap_or(true)
    }

    /// The `[harness] enabled` RESOLVED bit (default TRUE): whether the
    /// window supervises the agent sessions it hosts — the Settings ▸ Harness
    /// row. It is the SUPERVISOR'S bit ([`Self::harness`], read by the
    /// table's one reader), so the row and the supervisor cannot disagree:
    /// absent is ON; an explicit off (`false`, `"no"`, `"off"`, `0`) — or one
    /// filed under another table — is OFF; and a value that says neither
    /// (`enabled = "maybe"`) is no setting and leaves it ON (owner ruling of
    /// 2026-09-25: only an explicit setting takes power away).
    pub(crate) fn harness_enabled(&self) -> bool {
        self.harness.policy.enabled
    }

    /// Resolved `[harness] approve` level (`all`, `safe` or `none`; default
    /// `all`), from the same reader as [`Self::harness_enabled`]. An off
    /// spelling is `none`; a non-level value keeps `all` (owner, 2026-09-24).
    pub(crate) fn harness_approve(&self) -> &'static str {
        use aterm_agent::supervise::config::Approve;
        match self.harness.policy.approve {
            Approve::All => "all",
            Approve::Safe => "safe",
            Approve::None => "none",
        }
    }

    /// Resolved `[harness] question_priority` (default `recommended`): answer
    /// with the recommended option or by the `long-term` / `short-term` policy
    /// text. From the same reader as [`Self::harness_enabled`]; an unreadable
    /// value keeps the default and disables `answer_questions` (fail closed).
    pub(crate) fn harness_question_priority(&self) -> aterm_agent::supervise::QuestionPriority {
        self.harness.policy.question_priority
    }

    /// The `[harness]` reader's notes as config notices (`config harness.<key>:
    /// …`), each with its resolved value. Non-harness and retired keys are left
    /// to the ignored/retired-key notices ([`collect_key_notices`]).
    pub(crate) fn harness_notices(&self) -> Vec<String> {
        use aterm_agent::supervise::config::{is_retired_key_note, is_unknown_key_note};
        self.harness
            .notes
            .iter()
            .filter(|note| !is_unknown_key_note(note) && !is_retired_key_note(note))
            .map(|note| format!("config {note}"))
            .collect()
    }

    /// `[packages] enabled` (default true), folding `auto_update = false` in as
    /// atpkg does (`atpkg::config::PackagesConfig::enabled`). Gates the window's
    /// background package loop; clicked actions still run. The loop reads atpkg's
    /// twin live from the file (`LiveSwitch` in lib.rs); this seeds the Settings
    /// switch and service line.
    pub(crate) fn packages_enabled(&self) -> bool {
        self.packages
            .as_ref()
            .is_none_or(|p| p.enabled.unwrap_or(true) && p.auto_update != Some(false))
    }

    /// Why [`Self::packages_enabled`] reads off when a legacy
    /// `auto_update = false` holds it, in atpkg's words
    /// (`atpkg::config::auto_update_note`). Informational only; writing `enabled`
    /// drops the legacy key (`prefs::RETIRED_PACKAGES_SPELLINGS`).
    pub(crate) fn packages_retired_switch_note(&self) -> Option<String> {
        let packages = self.packages.as_ref()?;
        (packages.auto_update == Some(false) && packages.enabled != Some(false))
            .then(|| atpkg::config::auto_update_note(false, packages.enabled))
    }

    /// `[packages] auto_install` (default true), reading legacy `seed_install`
    /// when unset, as atpkg does (`atpkg::config::PackagesConfig::auto_install`).
    pub(crate) fn packages_auto_install(&self) -> bool {
        self.packages
            .as_ref()
            .and_then(|p| p.auto_install.or(p.seed_install))
            .unwrap_or(true)
    }

    /// `[machine] universal_control`, resolved like
    /// `atpkg::config::MachineConfig::universal_control` (absent, empty, `"off"`
    /// ⇒ `Off`; anything else ⇒ `Leave`). Bad spellings are reported by the
    /// config language service and atpkg, not here.
    pub(crate) fn machine_universal_control(&self) -> atpkg::config::UniversalControlPolicy {
        match self
            .machine
            .as_ref()
            .and_then(|m| m.universal_control.as_deref())
            .map(str::trim)
        {
            None | Some("") | Some("off") => atpkg::config::UniversalControlPolicy::Off,
            Some(_) => atpkg::config::UniversalControlPolicy::Leave,
        }
    }

    /// `[machine] spotlight_noindex`, resolved (default TRUE).
    pub(crate) fn machine_spotlight_noindex(&self) -> bool {
        self.machine
            .as_ref()
            .and_then(|m| m.spotlight_noindex)
            .unwrap_or(true)
    }

    /// The `[privacy]` master bit (default true). Off, every consent field reads
    /// `unknown`, not `denied`.
    pub(crate) fn privacy_enabled(&self) -> bool {
        self.privacy
            .as_ref()
            .and_then(|p| p.enabled)
            .unwrap_or(true)
    }

    /// The `[privacy]` `check` bit (default true): the silent access probes.
    /// Read through [`Config::privacy_probe_gate`], which also applies the master.
    pub(crate) fn privacy_check(&self) -> bool {
        self.privacy.as_ref().and_then(|p| p.check).unwrap_or(true)
    }

    /// The `[privacy]` `notice` bit (default true): the one-time access card.
    /// Also gated by the master; `--validate-config` warns on `enabled = false`
    /// with `notice = true`.
    pub(crate) fn privacy_notice(&self) -> bool {
        self.privacy.as_ref().and_then(|p| p.notice).unwrap_or(true)
    }

    /// The `[privacy]` `report_attribution` bit (default true): per-session
    /// responsible-process corroboration. Off removes a column, not a verdict.
    pub(crate) fn privacy_report_attribution(&self) -> bool {
        self.privacy
            .as_ref()
            .and_then(|p| p.report_attribution)
            .unwrap_or(true)
    }

    /// Both probe bits as the consent module's gate type, so the master always
    /// outranks `check`. `aterm_containment::probe_fda` refuses before any
    /// syscall when either is off.
    pub(crate) fn privacy_probe_gate(&self) -> aterm_containment::ProbeGate {
        aterm_containment::ProbeGate {
            enabled: self.privacy_enabled(),
            check: self.privacy_check(),
        }
    }

    /// The `[privacy]` `warmup` mode (default [`PrivacyWarmup::OnRequest`]).
    /// Unknown spellings resolve to the default and are named by
    /// `--validate-config`.
    pub(crate) fn privacy_warmup(&self) -> PrivacyWarmup {
        self.privacy
            .as_ref()
            .and_then(|p| p.warmup.as_deref())
            .and_then(PrivacyWarmup::parse)
            .unwrap_or_default()
    }

    /// The `[privacy]` `warmup_folders` NAMES (default: the consent module's
    /// `aterm_containment::Folder::WARMUP_DEFAULT`, in the order the warm-up walks
    /// them — taken from there rather than written out, so this default cannot
    /// drift from the module that owns the paths; `app-data` is not in it, because
    /// its Allow is forgotten when the process that received it is replaced).
    ///
    /// Names only: paths are resolved by
    /// `aterm_containment::consent::folder_paths`, because a protected path
    /// literal here would raise a system modal (`tools/grep_guard.sh` B13).
    /// Unknown names are kept for the validator and skipped by the resolver.
    pub(crate) fn privacy_warmup_folders(&self) -> Vec<String> {
        self.privacy
            .as_ref()
            .and_then(|p| p.warmup_folders.clone())
            .unwrap_or_else(|| {
                aterm_containment::Folder::WARMUP_DEFAULT
                    .iter()
                    .map(|folder| folder.as_str().to_string())
                    .collect()
            })
    }

    /// Resolved `[privacy] warmup_hold_ms` (default
    /// [`PRIVACY_WARMUP_HOLD_MS_DEFAULT`]), clamped to
    /// [`PRIVACY_WARMUP_HOLD_MS_MAX`]; `--validate-config` reports the clamp.
    pub(crate) fn privacy_warmup_hold_ms(&self) -> u64 {
        self.privacy
            .as_ref()
            .and_then(|p| p.warmup_hold_ms)
            .unwrap_or(PRIVACY_WARMUP_HOLD_MS_DEFAULT)
            .min(PRIVACY_WARMUP_HOLD_MS_MAX)
    }

    /// Resolved `[privacy] probe_interval_ms` floor (default
    /// [`PRIVACY_PROBE_INTERVAL_MS_DEFAULT`]), also the worst-case posture lag.
    pub(crate) fn privacy_probe_interval_ms(&self) -> u64 {
        self.privacy
            .as_ref()
            .and_then(|p| p.probe_interval_ms)
            .unwrap_or(PRIVACY_PROBE_INTERVAL_MS_DEFAULT)
    }

    /// The `[privacy]` `observer` key as a mode (design §3.6): absent is the
    /// [`ObserverMode::Sentinel`], `false` is off, `true` is the full stream.
    ///
    /// Config only — `crate::consent_observer` takes this resolved value as an
    /// argument and reads no environment knob of its own, so a program inside
    /// a session has nothing to flip. Whether a sentinel actually STARTS is
    /// the host's call (`App::effective_observer_mode`): it needs the card's
    /// own switches on, since it exists only to feed the card.
    pub(crate) fn privacy_observer_mode(&self) -> crate::consent_observer::ObserverMode {
        use crate::consent_observer::ObserverMode;
        match self.privacy.as_ref().and_then(|p| p.observer) {
            None => ObserverMode::Sentinel,
            Some(false) => ObserverMode::Off,
            Some(true) => ObserverMode::Full,
        }
    }

    /// `[privacy] observer = false`: the owner turned off aterm's reading of
    /// tccd's log, which also stops the Developer Tools row's `log show`. A
    /// plain read, so the control modules can ask it without naming the
    /// observer.
    pub(crate) fn privacy_observer_off(&self) -> bool {
        self.privacy_observer_mode() == crate::consent_observer::ObserverMode::Off
    }

    /// `[privacy] protected_roots` resolved to absolute paths. Absent or empty
    /// means `aterm_containment::sbpl::PRIVATE_SUBDIRS` under `$HOME`, the set
    /// Containment denies; resolution lives in `aterm_containment::consent`.
    ///
    /// An empty result means `$HOME` is unset with no absolute override: treat
    /// it as "cannot answer", never "nothing is protected".
    pub(crate) fn privacy_protected_roots(&self) -> Vec<std::path::PathBuf> {
        let overrides = self
            .privacy
            .as_ref()
            .and_then(|p| p.protected_roots.as_deref())
            .unwrap_or(&[]);
        aterm_containment::protected_roots(overrides)
    }

    /// Resolve `[matrix_rain]` into [`crate::matrix_rain::RainConfig`] with every
    /// default and clamp (the engine re-clamps). Independent of the enabled bit:
    /// a per-session override can force rain on over a disabled config
    /// ([`Config::matrix_rain_enabled`], `App::session_rain_enabled`), so the
    /// result always carries `enabled: true` and host gating decides whether an
    /// engine exists.
    ///
    /// `default_bg`/`theme_fg` are the live renderer theme (`0x00RR_GGBB`).
    pub(crate) fn matrix_rain_params(
        &self,
        default_bg: u32,
        theme_fg: u32,
    ) -> crate::matrix_rain::RainConfig {
        use crate::matrix_rain::{RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR, RainHue};
        let mr = self.matrix_rain.clone().unwrap_or_default();
        // Alpha overrides apply only when user-set; `None` keeps the engine's
        // theme-derived path. Heads are never dimmer than the body.
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
                Some(c) => RainHue::Custom(rgb_u32(c)),
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
            // 0 means "derive a stable per-window seed at engine build"
            // (rain_config_for_window), never wall-clock randomness.
            seed: mr.seed.unwrap_or(0),
            default_bg: default_bg & 0x00FF_FFFF,
            theme_fg: theme_fg & 0x00FF_FFFF,
        }
    }

    /// The `[output_streak]` master (`docs/DESIGN-output-streak-2026-08-30.md`
    /// §7). Default [`DEFAULT_DECORATIVE_EFFECTS`]: on except on Windows, where
    /// unprompted decoration over program output is off by default.
    ///
    /// Off switches:
    ///
    /// * `enabled = false` under `[output_streak]` (hot-reloaded).
    /// * Reduced motion (OS or `motion = "reduced"`): `MotionEffect::OutputStreak`
    ///   at amplitude 0 fully resets the engine.
    /// * `aterm ctl streak off`: a per-session override that wins until the
    ///   session ends.
    pub(crate) fn output_streak_enabled_or_default(&self) -> bool {
        self.output_streak
            .as_ref()
            .and_then(|streak| streak.enabled)
            .unwrap_or(DEFAULT_DECORATIVE_EFFECTS)
    }

    /// Whether the output streak plays sound (`[output_streak] sound`, default
    /// on). A plain `true` rather than [`DEFAULT_DECORATIVE_EFFECTS`] since a
    /// sound covers no pixel; cues are recorded only on spawn edges, so with the
    /// master off it is silent anyway. The synth host is macOS-only
    /// ([`crate::diagnostics`]). `trail_sounds`, `trail_sound_volume`, window
    /// focus and `SeriousEffect::TerminalSound` apply first.
    pub(crate) fn output_streak_sound_or_default(&self) -> bool {
        self.output_streak
            .as_ref()
            .and_then(|streak| streak.sound)
            .unwrap_or(true)
    }

    /// Output streak amplitude scale (`[output_streak] intensity`, default
    /// `1.0`), clamped `0..=1`. It scales `output_streak::ALPHA_DEFAULT` under
    /// the engine's hard `ALPHA_CLAMP`; `0` is a full reset, costing the same as
    /// `enabled = false`. Non-finite values read as `0.0` (they would reach
    /// vertex colours), like [`Self::trail_sound_volume`] and
    /// [`Self::cursor_trail_bloom_strength_or_default`].
    pub(crate) fn output_streak_intensity_or_default(&self) -> f32 {
        match self
            .output_streak
            .as_ref()
            .and_then(|streak| streak.intensity)
        {
            Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
            Some(_) => 0.0,
            None => 1.0,
        }
    }

    /// Comet tail length in cells (`[output_streak] tail`, default 9), clamped
    /// into [`aterm_effects::output_streak::TAIL_MIN`]`..=`[`aterm_effects::output_streak::TAIL_MAX`],
    /// the engine's own bounds. Out-of-band values clamp; they never reject the
    /// file.
    pub(crate) fn output_streak_tail_or_default(&self) -> u16 {
        use aterm_effects::output_streak::{TAIL_MAX, TAIL_MIN};
        let raw = self
            .output_streak
            .as_ref()
            .and_then(|streak| streak.tail)
            .unwrap_or(9);
        raw.clamp(u32::from(TAIL_MIN), u32::from(TAIL_MAX)) as u16
    }

    /// Resident comet cap (`[output_streak] max_streaks`, default 3), clamped
    /// `1..=`[`aterm_effects::output_streak::MAX_COMETS`]. The floor is 1 because
    /// off is spelled `enabled = false`. The engine's per-pane ignition floor
    /// (`aterm_effects::output_streak::SPAWN_MIN_GAP_MS`) binds first, so a
    /// higher cap adds burst density, not rate.
    pub(crate) fn output_streak_max_streaks_or_default(&self) -> u8 {
        use aterm_effects::output_streak::MAX_COMETS;
        let raw = self
            .output_streak
            .as_ref()
            .and_then(|streak| streak.max_streaks)
            .unwrap_or(3);
        raw.clamp(1, MAX_COMETS as u32) as u8
    }

    /// Idle drain in seconds (`[output_streak] idle_secs`, default 10), clamped
    /// `2..=120`; nothing animates forever. Non-finite values read as the floor,
    /// not the default, so a typo never lengthens the animation.
    pub(crate) fn output_streak_idle_secs_or_default(&self) -> f32 {
        match self
            .output_streak
            .as_ref()
            .and_then(|streak| streak.idle_secs)
        {
            Some(value) if value.is_finite() => value.clamp(2.0, 120.0),
            Some(_) => 2.0,
            None => 10.0,
        }
    }

    /// `--validate-config` warning for `[output_streak]`: `sound = true` under
    /// `enabled = false` is unreachable (cues are recorded only on edges that
    /// also emit light). Out-of-band numeric keys are documented clamps and are
    /// not warned.
    ///
    /// Test-only until [`crate::diagnostics`] pushes it as a
    /// `ConfigSemanticWarning` keyed `output_streak.sound`.
    #[cfg(test)]
    pub(crate) fn output_streak_warning(&self) -> Option<String> {
        let streak = self.output_streak.as_ref()?;
        (streak.sound == Some(true) && !self.output_streak_enabled_or_default()).then(|| {
            "output_streak.sound is configured as true but is effectively silent because \
             output_streak is disabled; a streak pip is only ever recorded on an edge that \
             also mints light"
                .to_string()
        })
    }

    /// GPU cursor bloom, the light crown around the comet head. Default on, on
    /// every platform: consumers read it as `cursor_trail_or_default() && …`, so
    /// it is inert wherever the trail is off. The half-res blur runs only on
    /// effect frames and is dropped by the `perf_reduced` latch and the `motion`
    /// policy.
    pub(crate) fn cursor_trail_bloom_or_default(&self) -> bool {
        self.cursor_trail_bloom.unwrap_or(true)
    }

    /// Bloom strength, default 0.85, clamped 0.0..=3.0. Non-finite values read
    /// as 0.0 since they feed shader params directly.
    pub(crate) fn cursor_trail_bloom_strength_or_default(&self) -> f32 {
        let v = self.cursor_trail_bloom_strength.unwrap_or(0.85);
        if v.is_finite() {
            v.clamp(0.0, 3.0)
        } else {
            0.0
        }
    }

    /// Bloom radius (half-res blur texels) for every style except the rainbow
    /// kitty.
    pub(crate) const CURSOR_TRAIL_BLOOM_RADIUS_DEFAULT: f32 = 2.2;

    /// The rainbow kitty's bloom radius (`RAINBOW-KITTY-V2.md` §21 row 14): a
    /// tighter halo than the default because its stars carry their own halos.
    /// The colour train and ribbon never bloom, so text stays crisp.
    pub(crate) const RAINBOW_KITTY_BLOOM_RADIUS: f32 = 1.8;

    /// The unset bloom radius for a resolved trail style:
    /// [`Self::RAINBOW_KITTY_BLOOM_RADIUS`] for the rainbow kitty,
    /// [`Self::CURSOR_TRAIL_BLOOM_RADIUS_DEFAULT`] otherwise.
    pub(crate) fn bloom_radius_style_default(style: ResolvedTrailStyle) -> f32 {
        if style.style == Some(aterm_effects::cursor_glow::GlowStyle::RainbowKitty) {
            Self::RAINBOW_KITTY_BLOOM_RADIUS
        } else {
            Self::CURSOR_TRAIL_BLOOM_RADIUS_DEFAULT
        }
    }

    /// Bloom radius (half-res texels) for the resolved trail style, clamped
    /// 0.5..=8.0: an explicit `cursor_trail_bloom_radius` for every style, else
    /// [`Self::RAINBOW_KITTY_BLOOM_RADIUS`] for
    /// [`aterm_effects::cursor_glow::GlowStyle::RainbowKitty`] and
    /// [`Self::CURSOR_TRAIL_BLOOM_RADIUS_DEFAULT`] otherwise. Takes the
    /// pre-parsed [`ResolvedTrailPresentation`] so the GPU pin seams parse no
    /// strings. Non-finite values fall back to the per-style default.
    pub(crate) fn cursor_trail_bloom_radius_or_default(
        &self,
        presentation: ResolvedTrailPresentation,
    ) -> f32 {
        let per_style = Self::bloom_radius_style_default(presentation.style);
        let v = self.cursor_trail_bloom_radius.unwrap_or(per_style);
        if v.is_finite() {
            v.clamp(0.5, 8.0)
        } else {
            per_style
        }
    }

    /// GPU heat shimmer above burning cells. Default on: one staged copy and one
    /// scissored pass, only on frames with glow quads. Dropped with the bloom by
    /// the `perf_reduced` latch; reduced motion and unfocused windows emit no
    /// glow quads, hence no shimmer.
    pub(crate) fn cursor_fire_shimmer_or_default(&self) -> bool {
        self.cursor_fire_shimmer.unwrap_or(true)
    }

    /// Whether the EDR cursor glow is enabled. Default on: on an HDR desktop
    /// (Windows scRGB DX12 / macOS EDR) the aurora gets real headroom; on SDR the
    /// headroom is 0 and the EDR pass is inert (byte-identical present).
    pub(crate) fn hdr_glow_or_default(&self) -> bool {
        self.hdr_glow.unwrap_or(true)
    }

    /// SDR glow-boost strength (`cursor_glow_sdr_boost`), default 0.25, clamped
    /// 0..=1; `0` disables. The renderer combines it with background luma into
    /// a bounded budget (`aterm_render::hdr::sdr_glow_budget`) and eases it in
    /// over ~45ms, so light themes fade it out and onsets do not strobe. NaN
    /// reads as off inside the budget math.
    pub(crate) fn cursor_glow_sdr_boost_or_default(&self) -> f32 {
        self.cursor_glow_sdr_boost.unwrap_or(0.25).clamp(0.0, 1.0)
    }

    /// Renderer text-shaping config from `ligatures`, `cursor_break_ligatures`
    /// and `font_features`. Defaults give `TextShapingConfig::default()`
    /// (`LigatureMode::Enabled`, base `liga`+`calt`). `cursor_break_ligatures`
    /// maps to `LigatureMode::CursorDisabled`; `ligatures = false` wins over it.
    /// `ambiguous_width` goes through the engine (`ambiguous_width_double`), not
    /// here.
    pub(crate) fn text_shaping(&self) -> aterm_render::TextShapingConfig {
        use aterm_types::text_shaping::{
            FontFeature, FontFeatureSet, LigatureMode, parse_font_features,
        };
        let ligature_mode = match (self.ligatures, self.cursor_break_ligatures) {
            (Some(false), _) => LigatureMode::Disabled,
            (_, Some(true)) => LigatureMode::CursorDisabled,
            _ => LigatureMode::Enabled,
        };
        // Entries may be space-separated groups; flatten onto the primary face
        // (`font_id == 0`). The parser drops malformed tags.
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

    /// Coverage blend mode: `"linear-corrected"` (default) or `"linear"`;
    /// case-insensitive, `linear_corrected` accepted. Unknown values warn and
    /// keep the default.
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
                crate::logging::stderr_line!(
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

    /// Variable-font requests: parsed `font_variation` entries with
    /// `font_weight` appended last (so it wins). Malformed entries are returned
    /// as warnings and skipped.
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

    /// Effective stem gamma: `stem_gamma`, else `1.0`, clamped to
    /// `0.30..=3.0`; non-finite values mean `1.0`.
    pub(crate) fn stem_gamma_or_default(&self) -> f32 {
        aterm_render::clamp_stem_gamma(self.stem_gamma.unwrap_or(1.0))
    }

    /// Effective grid-fitting mode: `font_hinting`, else `"full"`. The
    /// renderer's parser handles unknown spellings.
    pub(crate) fn font_hinting_or_default(&self) -> String {
        self.font_hinting
            .clone()
            .unwrap_or_else(|| "full".to_string())
    }

    /// Effective Linux subpixel mode: `font_subpixel`, else `"off"`. The
    /// renderer's parser handles unknown spellings.
    pub(crate) fn font_subpixel_or_default(&self) -> String {
        self.font_subpixel
            .clone()
            .unwrap_or_else(|| "off".to_string())
    }

    /// Line-height multiplier, default `1.0`, clamped 0.8..=2.0 (the renderer
    /// floors at 0.5). Non-finite values mean `1.0`.
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

    /// Selected-text foreground as `0x00RRGGBB` when `selection_foreground`
    /// parses; `None` uses the renderer's contrast-floor default.
    pub(crate) fn selection_foreground_u32(&self) -> Option<u32> {
        self.selection_foreground
            .as_deref()
            .and_then(parse_hex_color)
            .map(rgb_u32)
    }

    /// Whether the selection band dims while unfocused. Default off.
    pub(crate) fn selection_inactive_or_default(&self) -> bool {
        self.selection_inactive.unwrap_or(false)
    }

    /// Whether a split marks the focused pane. Default on.
    pub(crate) fn split_focus_mark_or_default(&self) -> bool {
        self.split_focus_mark.unwrap_or(true)
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

    /// Window material, default [`BackgroundMaterial::None`]. Case-insensitive;
    /// unknown values warn once and fall back to `None`.
    pub(crate) fn background_material_or_default(&self) -> BackgroundMaterial {
        match self.background_material.as_deref() {
            None => BackgroundMaterial::None,
            Some(s) => BackgroundMaterial::parse(s).unwrap_or_else(|| {
                crate::logging::stderr_line!(
                    "aterm-gui: config background_material: expected none|hud|sidebar|\
                     under-window, got {s:?}; using none"
                );
                BackgroundMaterial::None
            }),
        }
    }

    /// Window padding in logical px (`window_padding`), default
    /// [`crate::PAD_LOGICAL_PX`] (12.0), clamped 0..=[`MAX_WINDOW_PADDING_PX`].
    /// Non-finite values use the default (NaN would poison every window metric).
    pub(crate) fn window_padding_or_default(&self) -> f32 {
        match self.window_padding {
            Some(v) if v.is_finite() => v.clamp(0.0, MAX_WINDOW_PADDING_PX),
            _ => crate::PAD_LOGICAL_PX,
        }
    }

    /// Top padding in logical px (`window_padding_top`), default
    /// [`crate::PAD_TOP_LOGICAL_PX`] (2.0), clamped to `0..=window_padding` to
    /// match the renderer's `set_pad_top` (`pad_top <= pad`). The default is
    /// also capped by the resolved base pad.
    pub(crate) fn window_padding_top_or_default(&self) -> f32 {
        let pad = self.window_padding_or_default();
        match self.window_padding_top {
            Some(v) if v.is_finite() => v.clamp(0.0, pad),
            _ => crate::PAD_TOP_LOGICAL_PX.min(pad),
        }
    }

    /// Effective per-cell minimum contrast for the renderer: when the window is
    /// translucent (`background_opacity < 1.0`) the floor is raised to at least
    /// WCAG AA (4.5:1); an opaque window keeps the configured floor exactly.
    /// Proven by `vibrancy_contrast_guarantee` (Tier-1) and
    /// `aterm_spec::derive::vibrancy_contrast_model` (Tier-0, `NeverIllegible`).
    pub(crate) fn effective_minimum_contrast(&self) -> f32 {
        let base = self.minimum_contrast_or_default();
        if aterm_render::vibrancy::is_translucent(self.background_opacity_or_default()) {
            base.max(VIBRANCY_CONTRAST_FLOOR)
        } else {
            base
        }
    }

    /// Warning when a configured `font_family` does not resolve to a font file,
    /// for the startup/reload notice and `--validate-config`. `family` is the
    /// effective family (`--font` > config).
    pub(crate) fn font_family_warning(family: Option<&str>) -> Option<String> {
        Self::font_family_admission(family).err()
    }

    /// Admission verdict behind [`Self::font_family_warning`]: `Ok(None)` for no
    /// family, `Ok(Some(path))` with the exact file (or `display:` name) the
    /// backend should build from, `Err(warning)` otherwise.
    ///
    /// The one startup resolve of a configured family; it runs on the backend
    /// worker because it walks font directories and may parse `name` tables
    /// (milliseconds warm, seconds cold).
    pub(crate) fn font_family_admission(family: Option<&str>) -> Result<Option<String>, String> {
        let Some(fam) = family.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        match aterm_render::resolve_config_font(fam) {
            Ok(path) => Ok(Some(path)),
            Err(error) => Err(format!(
                "font_family {fam:?} is not an admissible font ({error}); keeping the current \
                 working font (see `aterm list-fonts` for resolvable families)"
            )),
        }
    }

    /// Warning when `cursor_trail_style` is an unrecognized spelling (it falls
    /// back to [`crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE`]). Checked against
    /// [`crate::prefs::cursor_trail_style_canonical`], the source the Settings
    /// picker uses. Shared by startup, reload and `--validate-config`.
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
            // The vocabulary is joined from `CURSOR_TRAIL_STYLES` so it matches the
            // picker. The near miss leads because the banner truncates one line
            // per notice at the terminal width.
            Some(TrailStyleIssue::Unknown) => Some(format!(
                "cursor_trail_style: unknown style {raw:?}{suggestion} (falling back to \
                 {default:?}; expected one of {options}, a documented alias like \
                 nyan rainbow/nyan/rainbow/tall rainbow/ember/ocean, or pack:<id> for a \
                 loaded Trail Pack)",
                default = crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
                options = crate::prefs::CURSOR_TRAIL_STYLES.join("|"),
                suggestion = crate::native_config_language::nearest_enum_value(
                    raw,
                    crate::prefs::CURSOR_TRAIL_STYLES
                )
                .map_or_else(String::new, |near| format!(" — did you mean {near:?}?")),
            )),
        }
    }

    /// Whether to show the floating top-right build/version pill. Default off
    /// (the version lives in the menu bar). See [`crate::build_badge`].
    pub(crate) fn show_build_badge_or_default(&self) -> bool {
        self.show_build_badge.unwrap_or(false)
    }

    /// Window-chrome appearance ([`WindowTheme`]); default [`WindowTheme::Auto`].
    /// Unknown values warn and fall back to `Auto`.
    pub(crate) fn window_theme_or_default(&self) -> WindowTheme {
        match self.window_theme.as_deref() {
            None => WindowTheme::Auto,
            Some(s) => match WindowTheme::parse(s) {
                Some(t) => t,
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config window_theme: expected auto|light|dark, got {s:?}; using auto"
                    );
                    WindowTheme::Auto
                }
            },
        }
    }

    /// Grid right-click gesture ([`RightClickGesture`]) from `right_click`.
    /// Default [`RightClickGesture::PLATFORM_DEFAULT`]; unknown values warn and
    /// use it.
    pub(crate) fn right_click_or_default(&self) -> RightClickGesture {
        match self.right_click.as_deref() {
            None => RightClickGesture::PLATFORM_DEFAULT,
            Some(s) => match RightClickGesture::parse(s) {
                Some(g) => g,
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config right_click: expected copy_paste|off, got {s:?}; using the platform default"
                    );
                    RightClickGesture::PLATFORM_DEFAULT
                }
            },
        }
    }

    /// Tab-context-menu keyboard chord ([`TabMenuChord`]) from `tab_menu_chord`.
    /// Default [`TabMenuChord::MenuKey`], which no application can receive, so
    /// Shift+F10 (`kf22`) stays with the program. Unknown values warn and fall
    /// back to the default, never to `On`.
    ///
    /// Read only by the in-grid strip chord arms (Windows, Linux) and tests.
    #[cfg(any(not(target_os = "macos"), test))]
    pub(crate) fn tab_menu_chord_or_default(&self) -> TabMenuChord {
        match self.tab_menu_chord.as_deref() {
            None => TabMenuChord::MenuKey,
            Some(s) => match TabMenuChord::parse(s) {
                Some(g) => g,
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config tab_menu_chord: expected on|menu_key|off, got {s:?}; using menu_key"
                    );
                    TabMenuChord::MenuKey
                }
            },
        }
    }

    /// GPU-present colour-space tag ([`WindowColorspace`]) from
    /// `window_colorspace`; default [`WindowColorspace::Srgb`]. Unknown values
    /// warn and fall back to `Srgb`.
    pub(crate) fn window_colorspace_or_default(&self) -> WindowColorspace {
        match self.window_colorspace.as_deref() {
            None => WindowColorspace::Srgb,
            Some(s) => match WindowColorspace::parse(s) {
                Some(c) => c,
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config window_colorspace: expected srgb|display-p3, got {s:?}; using srgb"
                    );
                    WindowColorspace::Srgb
                }
            },
        }
    }

    /// In-grid tab band height ([`TabBandHeight`]) from `tab_band_height`;
    /// default [`TabBandHeight::PLATFORM_DEFAULT`]. Unknown values warn and use
    /// it.
    pub(crate) fn tab_band_height_or_default(&self) -> TabBandHeight {
        match self.tab_band_height.as_deref() {
            None => TabBandHeight::PLATFORM_DEFAULT,
            Some(s) => match TabBandHeight::parse(s) {
                Some(h) => h,
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config tab_band_height: expected compact|standard, got {s:?}; using the platform default"
                    );
                    TabBandHeight::PLATFORM_DEFAULT
                }
            },
        }
    }
}

/// Renderer typography/appearance knobs resolved from config: the value the
/// App caches, seeds at startup and diffs on reload. Pure (`from_config`,
/// `diff`): each key maps to exactly one [`KnobChange`] variant and each
/// variant to one renderer call (see `reload_config` / `rebuild_backend`).
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
    /// `background_material`, applied via `AppRt::window_set_vibrancy`. macOS GPU
    /// installs a behind-window effect only while translucent; Windows GPU
    /// installs a DWM backdrop independently; other paths diagnose it as inert.
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
            // The effective floor: raised to WCAG AA while translucent.
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
/// Renderer settings that bake into rasterized coverage or the cell box, whose
/// setters drop the glyph cache on change: `set_text_shaping` (ambiguous
/// width), `set_font_thicken`, `set_stem_gamma`, `set_font_hinting`,
/// `set_line_height` and `set_adjust_baseline`.
///
/// Applied by one setter list (`Backend::pin_glyph_raster_knobs`) because the
/// backend worker pins them before its glyph warm and the join re-pins them;
/// if the values differ the warm is discarded and the first frame rasterizes
/// on the UI thread.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct GlyphRasterKnobs {
    /// `ligatures` / `font_features` / `merged_ligatures` → `set_text_shaping`.
    pub(crate) text_shaping: aterm_render::TextShapingConfig,
    /// `font_thicken` → `set_font_thicken`.
    pub(crate) font_thicken: bool,
    /// `stem_gamma` → `set_stem_gamma`.
    pub(crate) stem_gamma: f32,
    /// `font_hinting` → `set_font_hinting`.
    pub(crate) font_hinting: String,
    /// `line_height` → `set_line_height`.
    pub(crate) line_height: f32,
    /// `adjust_baseline` → `set_adjust_baseline`.
    pub(crate) adjust_baseline: i32,
}

impl GlyphRasterKnobs {
    /// Resolve the knobs with the same precedence the App's fields use (env
    /// alias > config key > default). `main_entry` resolves once and hands the
    /// value to both the worker and the join.
    pub(crate) fn from_config(cfg: &Config) -> Self {
        Self {
            text_shaping: cfg.text_shaping(),
            font_thicken: cfg.font_thicken_or_default(),
            stem_gamma: cfg.stem_gamma_or_default(),
            font_hinting: cfg.font_hinting_or_default(),
            line_height: cfg.line_height_or_default(),
            adjust_baseline: cfg.adjust_baseline_or_default(),
        }
    }
}

impl Default for RenderKnobs {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

/// Resolved styled/fallback font config (file paths, not family names): the
/// value the App caches, seeds and diffs on reload, like [`RenderKnobs`]. Family
/// resolution uses [`aterm_render::resolve_font_family`]; applied by
/// [`crate::apply_font_config_to_backend`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct FontConfig {
    /// Styled-face paths `[bold, italic, bold-italic]`. `None` means unset or
    /// unresolvable (warned); the renderer keeps discovery/synthetic.
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
    /// Resolve the font keys into paths plus warnings for entries that do not
    /// resolve (skipped, never a hard failure).
    pub(crate) fn from_config(cfg: &Config) -> (Self, Vec<String>) {
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

    /// Keep the last working face for an authored entry that failed admission in
    /// `from_config`. For an authored key `None` means rejected, not unset, so a
    /// bad edit must not clobber a working face; valid siblings still advance.
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

/// A titlebar band larger than this is a mid-transition artifact: real bands
/// are ~22–62 pt, artifacts measure in the hundreds.
const TITLEBAR_BAND_SANITY_CAP_PTS: f64 = 120.0;

/// Fail-closed acceptance for one AppKit titlebar-band sample. Fullscreen forces
/// 0. A decorated windowed sample `<= 0` or above
/// [`TITLEBAR_BAND_SANITY_CAP_PTS`] is a transition artifact and keeps the
/// previous band (committing it would misplace row 0). An undecorated windowed
/// 0 is real and accepted.
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

/// Whether a titlebar-band sample is plausible (positive, within the cap);
/// shared by the commit path and the memory update.
fn titlebar_band_sane(pts: f64) -> bool {
    pts > 0.0 && pts <= TITLEBAR_BAND_SANITY_CAP_PTS
}

/// Titlebar head decision for one sample:
/// `(measured, fullscreen, decorated, memory) -> (applied, new_memory)`.
///
/// `applied` is [`accept_titlebar_band_pts`] with the last-good windowed memory
/// as fallback, not the applied band (which is 0 throughout fullscreen); see
/// `WindowState::head_pts`. `new_memory` advances only when a sane decorated
/// windowed sample commits; fullscreen, undecorated and rejected samples keep
/// it.
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

    /// `accept_titlebar_band_pts(sample, fullscreen, decorated, prev)`, row by row.
    #[test]
    fn a_titlebar_band_sample_is_accepted_only_when_windowed_decorated_and_sane() {
        let inflated = TITLEBAR_BAND_SANITY_CAP_PTS + 1.0;
        for (label, sample, fullscreen, decorated, prev, expected) in [
            // Fullscreen forces 0 regardless of the sample.
            ("fullscreen 38", 38.0, true, true, 55.0, 0.0),
            ("fullscreen 0", 0.0, true, true, 55.0, 0.0),
            ("fullscreen 900", 900.0, true, true, 55.0, 0.0),
            // The defect-A shape: the exit-fullscreen race reads 0 while decorated —
            // a transition artifact, so the previous band is kept.
            ("windowed 0", 0.0, false, true, 55.0, 55.0),
            ("windowed -3", -3.0, false, true, 55.0, 55.0),
            // The defect-B head-side shape: the enter-fullscreen race reads the
            // screen-vs-window frame difference as a giant band — kept at prev.
            ("windowed past the cap", inflated, false, true, 55.0, 55.0),
            ("windowed 700", 700.0, false, true, 55.0, 55.0),
            // A sane windowed sample commits.
            ("windowed 38", 38.0, false, true, 55.0, 38.0),
            ("windowed 62, no prev", 62.0, false, true, 0.0, 62.0),
            // Runtime decoration toggle: the band really is gone; a kept stale band
            // would inset a chromeless window forever.
            ("undecorated 0", 0.0, false, false, 55.0, 0.0),
            ("undecorated -1", -1.0, false, false, 55.0, 0.0),
        ] {
            assert_eq!(
                accept_titlebar_band_pts(sample, fullscreen, decorated, prev),
                expected,
                "{label}"
            );
        }
    }

    /// The memory slot carries the real band (55) across a fullscreen stay and
    /// restores it when the exit sample is an artifact.
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

        // Exit fullscreen with the classic artifact: decorated, windowed, 0.0.
        // The band must restore to 55.
        let (applied, memory) = titlebar_band_decision(0.0, false, true, memory);
        assert_eq!((applied, memory), (55.0, 55.0));
    }

    /// The memory slot advances only on successful acceptance; artifacts,
    /// fullscreen's forced 0 and the undecorated 0 preserve it.
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

/// Once-per-process deprecation notice for the `game_font` key and game-named
/// display-face ids (`minecraft`, …), which still work. `mariokart` has no
/// successor and gets its own sentence. Reads the raw config text because
/// `#[serde(alias)]` erases which spelling was used.
fn warn_deprecated_display_font_spelling(source: &str, config: &Config) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let legacy_key = source.lines().any(|line| {
            line.split('=')
                .next()
                .is_some_and(|key| key.trim() == crate::prefs::LEGACY_EDIT_DISPLAY_FONT)
        });
        if legacy_key {
            crate::logging::stderr_line!(
                "aterm-gui: `{legacy}` is deprecated; rename it to `{current}` \
                 (the old key still works)",
                legacy = crate::prefs::LEGACY_EDIT_DISPLAY_FONT,
                current = crate::prefs::EDIT_DISPLAY_FONT,
            );
        }
        for notice in display_font_notices(config) {
            crate::logging::stderr_line!("aterm-gui: {notice}");
        }
    });
}

/// Notices for each retired id in `display_font`: a renamed one names its new
/// id, a deleted one says what draws instead. Also printed by
/// `--validate-config`.
pub(crate) fn display_font_notices(config: &Config) -> Vec<String> {
    let Some(raw) = config.display_font.as_deref() else {
        return Vec::new();
    };
    let any_face = raw
        .split('+')
        .any(|id| aterm_render::display_face_canonical_id(id).is_some());
    raw.split('+')
        .map(str::trim)
        .filter_map(|id| {
            let (_, current) = aterm_render::DISPLAY_FACE_LEGACY_IDS
                .iter()
                .find(|(legacy, _)| legacy.eq_ignore_ascii_case(id))?;
            Some(match current {
                Some(current) => {
                    format!("display_font: {id:?} is deprecated; write {current:?} (same face)")
                }
                None if any_face => {
                    format!("display_font: {id:?} is no longer shipped and is skipped")
                }
                None => {
                    format!("display_font: {id:?} is no longer shipped; your primary font is used")
                }
            })
        })
        .collect()
}

/// Once-per-process warning that `background_opacity < 1.0` renders solid on
/// the CPU backend (the softbuffer surface is opaque). The raised contrast floor
/// still applies.
fn warn_background_opacity_unimplemented_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::logging::stderr_line!(
            "aterm-gui: background_opacity < 1.0 requests translucent glass, but the \
             CPU (softbuffer) renderer has no translucent present path; the window \
             renders solid (use the GPU backend for real vibrancy; the raised contrast \
             floor still applies)"
        );
        // Windowed launches never see stderr, so also queue a message-band
        // notice on the pre-App inbox (the first pin can run before `App`).
        crate::message_inbox::queue_message(crate::message_reporters::cpu_renderer_no_effect(
            "background_opacity",
            "it has no translucent present path, so the window stays solid; enable the GPU \
             renderer for real transparency",
        ));
    });
}

/// Once-per-process warning that a non-`none` `background_material` has no
/// effect on the CPU backend, which cannot composite over an
/// `NSVisualEffectView`.
fn warn_background_material_unimplemented_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::logging::stderr_line!(
            "aterm-gui: background_material selects a window-level vibrancy blur \
             (NSVisualEffectView), but the CPU (softbuffer) renderer cannot composite \
             over it; the setting has no effect on the CPU backend (use the GPU backend)"
        );
        // Same reasoning as its opacity sibling above: a deliberately-set key
        // doing nothing, explained only on a stream a windowed launch discards.
        crate::message_inbox::queue_message(crate::message_reporters::cpu_renderer_no_effect(
            "background_material",
            "it cannot composite over a window-level blur; enable the GPU renderer to see \
             the material",
        ));
    });
}

/// Window-chrome appearance, distinct from the terminal color scheme. Resolved
/// via [`Config::window_theme_or_default`] and applied through
/// [`crate::platform::AppRt::window_set_appearance`].
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

/// What a right press in the grid does when no app tracks the mouse. Resolved
/// via [`Config::right_click_or_default`], consumed in
/// `app_mouse::on_mouse_input`. An enum so further gestures can join without a
/// config break.
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
    /// Default when the key is absent: `CopyPaste` on Windows (conhost and
    /// Windows Terminal convention), `Off` elsewhere (macOS right-click is the
    /// context menu, Linux pastes on middle click). A `cfg!` const so the
    /// non-Windows arm stays type-checked on every platform build.
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

/// Which keyboard spellings of "show this tab's context menu" aterm claims.
/// Resolved via [`Config::tab_menu_chord_or_default`], consumed by
/// `app_input::tab_menu_chord` on both the winit and control-seam routes.
///
/// The default is [`Self::MenuKey`]: the Menu key reaches an application only
/// under a kitty enhancement (already deferred), while Shift+F10 is terminfo
/// `kf22` (`ESC[21;2~`), which programs receive and bind.
///
/// Gated to the Windows/Linux chord arms and tests; the config key parses
/// everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(not(target_os = "macos"), test))]
pub(crate) enum TabMenuChord {
    /// Both Windows spellings: the dedicated Menu/Application key AND Shift+F10.
    /// The opt-in (`tab_menu_chord = "on"`), never the default.
    On,
    /// The Menu key only — Shift+F10 goes to the terminal application. The
    /// default.
    MenuKey,
    /// Neither; the menu is pointer-only and both keys reach the application.
    Off,
}

#[cfg(any(not(target_os = "macos"), test))]
impl TabMenuChord {
    /// Parse a config `tab_menu_chord` value (case-insensitive, trimmed):
    /// `on` / `both`, `menu_key` (aliases `menu-key`, `menu`), or `off`.
    /// `None` on any other value (caller falls back to [`Self::MenuKey`]).
    #[cfg(any(not(target_os = "macos"), test))]
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "on" | "both" => Some(Self::On),
            "menu_key" | "menu-key" | "menu" => Some(Self::MenuKey),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// Whether this policy claims the dedicated Menu / Application key.
    #[cfg(any(not(target_os = "macos"), test))]
    pub(crate) fn claims_menu_key(self) -> bool {
        matches!(self, Self::On | Self::MenuKey)
    }

    /// Whether this policy claims Shift+F10.
    #[cfg(any(not(target_os = "macos"), test))]
    pub(crate) fn claims_shift_f10(self) -> bool {
        matches!(self, Self::On)
    }
}

/// In-grid tab band height. Resolved via [`Config::tab_band_height_or_default`],
/// consumed only by `App::synthetic_strip_head_px`. A named target rather than a
/// pixel count because the band is `pad_top + head + strip_rows·cell_h` and two
/// terms move with font and DPI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabBandHeight {
    /// No synthetic head: the band is the top pad plus the strip's cell rows
    /// (21 px at Windows defaults, 96 dpi). The default off Windows and Linux.
    Compact,
    /// Size the WHOLE band to [`TAB_BAND_STANDARD_LOGICAL_PX`] logical px — a real
    /// WinUI tab — by reserving the difference as a synthetic chrome head.
    Standard,
}

impl TabBandHeight {
    /// Default when the key is absent: `Standard` on Windows and Linux, where the
    /// in-grid strip is the only tab chrome (and the Linux pixel band,
    /// [`crate::tab_bar::pixel_band`], is drawn for the full
    /// [`TAB_BAND_STANDARD_LOGICAL_PX`] canvas); `Compact` on macOS, which never
    /// paints this band. A `cfg!` const so both arms stay type-checked.
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

    /// Target band height in logical px (1/96 in on Windows); `0.0` for
    /// [`Self::Compact`], which makes the synthetic head 0.
    pub(crate) fn target_logical_px(self) -> f32 {
        match self {
            Self::Compact => 0.0,
            Self::Standard => TAB_BAND_STANDARD_LOGICAL_PX,
        }
    }
}

/// The `"standard"` band target in logical px: 32 on Windows (the WinUI tab
/// strip and caption height), 36 on Linux (libadwaita `AdwTabBar`, the height
/// the pixel band's cards are drawn against). A `cfg!` const so both arms stay
/// type-checked.
pub(crate) const TAB_BAND_STANDARD_LOGICAL_PX: f32 = if cfg!(target_os = "linux") {
    36.0
} else {
    32.0
};

/// Backstop on the synthetic head, in multiples of the cell height, so a
/// pathological config cannot push the grid far down. Unlike
/// [`TITLEBAR_BAND_SANITY_CAP_PTS`] it guards our own arithmetic.
const SYNTHETIC_BAND_HEAD_CELL_CAP: usize = 4;

/// Synthetic chrome-head band in device px for `strip_rows` rows of `cell_h` px
/// under `pad_top_px`, when the whole band should measure `target_logical`
/// logical px at `scale`.
///
/// `head = clamp(round(target·scale) − pad_top − strip_rows·cell_h)`: it
/// reserves the remainder, so at a large font the head goes to zero rather
/// than stacking on a tall row. Degenerate input (strip off, zero target,
/// non-finite scale) returns 0.
pub(crate) fn synthetic_band_head_px(
    target_logical: f32,
    pad_top_px: usize,
    strip_rows: u16,
    cell_h: usize,
    scale: f64,
) -> usize {
    // Proceed only on a definite Greater so NaN takes the degenerate path
    // (`target_logical <= 0.0` is false for NaN); this form also satisfies
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

/// Colour-space tag for the window's CAMetalLayer, resolved via
/// [`Config::window_colorspace_or_default`] and applied at surface attach and
/// on reload through [`crate::platform::AppRt::window_set_surface_colorspace`].
/// The tag changes how ColorSync interprets the bytes, never the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum WindowColorspace {
    /// Tag the layer sRGB (the default): theme colours render as authored;
    /// ColorSync performs the one honest sRGB→panel mapping.
    #[default]
    Srgb,
    /// Tag the layer Display-P3: sRGB bytes are read as P3 coordinates (the
    /// untagged, oversaturated look on wide-gamut panels).
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

/// Upper clamp on `window_padding` (logical px); beyond it the pad eats whole
/// rows or columns on small windows.
pub(crate) const MAX_WINDOW_PADDING_PX: f32 = 64.0;

/// WCAG AA contrast floor (4.5:1) applied while the window is translucent; the
/// twin of `Floor` in `aterm_spec::derive::vibrancy_contrast_model`.
pub(crate) const VIBRANCY_CONTRAST_FLOOR: f32 = 4.5;

/// macOS `NSVisualEffectView` material behind a translucent window, resolved
/// via [`Config::background_material_or_default`]. Meaningful only with
/// `background_opacity < 1.0` on the GPU backend; `None` installs no view.
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

/// Content fingerprints of the active files the config references by path: the
/// `sparkle_words.lexicon` file and `sparkle_words.toy_packs` manifests (deco
/// feed) and the `cursor_trail_packs` manifests (trail feed). `Config`
/// equality compares only path strings, so reload dedupe also compares these;
/// otherwise a feed-only edit (the watcher re-posts an identical `aterm.toml`
/// observation, `config_watcher.rs`) would be ignored. A disabled Sparkle table
/// does no path I/O. Split so consumers reset precisely: the deco feed triggers
/// a per-window word-engine `hard_reset()`, the trail feed only a registry
/// rebuild.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct PathFeedFps {
    /// Lexicon + toy-pack content: feeds the compiled lexicon the per-window
    /// word decorations scan against.
    pub(crate) deco: u64,
    /// Trail-pack manifest content: feeds the `id → TrailParams` registry.
    pub(crate) trail: u64,
}

/// Whether a reload must hard-reset per-window word decorations: the
/// `[sparkle_words]` table, its theme, or the content of its path-referenced
/// lexicon/toy-pack files changed. The fingerprint term catches file edits that
/// leave `sparkle_words` equal; the per-window rescan is damage-driven, so
/// without it windows would keep old-lexicon decorations.
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
    let mut hash = aterm_hash::Fnv1a64::new();
    hash.write_u8(tag);
    hash.write(&(source.len() as u64).to_le_bytes());
    hash.write(source);
    hash.write(&w.to_le_bytes());
    hash.write(&h.to_le_bytes());
    hash.write(&(rgba.len() as u64).to_le_bytes());
    hash.write(rgba);
    hash.finish() | 1
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

/// Decode wallpaper bytes to straight sRGB RGBA8: PNG through the shared
/// hardened decoder, other formats through `NSBitmapImageRep` on macOS,
/// converted to sRGB and re-encoded as PNG so Rust reads a standard straight
/// RGBA layout.
fn decode_wallpaper_bytes(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    if let Some(decoded) = aterm_png::decode_rgba8(bytes, MAX_WALLPAPER_SOURCE_DIMENSION as u32) {
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
    use std::ffi::c_void;

    use aterm_objc::{Id, Sel, autoreleasepool, class, sel};

    use crate::appkit;

    /// `NSBitmapImageRep.h:32-38` — `NSBitmapImageFileTypePNG` is the FIFTH
    /// enumerator of an unvalued `NS_ENUM(NSUInteger, …)`, i.e. 4.
    const NS_BITMAP_IMAGE_FILE_TYPE_PNG: usize = 4;
    /// `NSGraphics.h:124-130` — `NSColorRenderingIntentPerceptual` is the
    /// FOURTH enumerator of an unvalued `NS_ENUM(NSInteger, …)`, i.e. 3.
    const NS_COLOR_RENDERING_INTENT_PERCEPTUAL: isize = 3;

    autoreleasepool(|_| {
        // SAFETY: raster work on immutable bytes; no class here is
        // main-thread-bound. `+dataWithBytes:length:` copies,
        // `+imageRepWithData:` returns an autoreleased rep alive for this pool,
        // and `-pixelsWide`/`-pixelsHigh` return `NSInteger`.
        unsafe {
            let data_with: unsafe extern "C-unwind" fn(Id, Sel, *const c_void, usize) -> Id =
                aterm_objc::msg();
            let data = data_with(
                class(c"NSData").as_id(),
                sel!(dataWithBytes:length:),
                bytes.as_ptr().cast(),
                bytes.len(),
            );
            if data.is_null() {
                return Err("not a decodable image".to_string());
            }
            let rep = appkit::send_id_id(
                class(c"NSBitmapImageRep").as_id(),
                sel!(imageRepWithData:),
                data,
            );
            if rep.is_null() {
                return Err("not a decodable image".to_string());
            }
            let pw = appkit::send_isize(rep, sel!(pixelsWide));
            let ph = appkit::send_isize(rep, sel!(pixelsHigh));
            if pw <= 0
                || ph <= 0
                || pw as usize > MAX_WALLPAPER_SOURCE_DIMENSION
                || ph as usize > MAX_WALLPAPER_SOURCE_DIMENSION
            {
                return Err(format!(
                    "decoded image must be 1..={MAX_WALLPAPER_SOURCE_DIMENSION} pixels per side"
                ));
            }
            // Convert (not retag) the pixels to sRGB before Rust reads them.
            // `+sRGBColorSpace` is an immortal shared space;
            // `-bitmapImageRepByConvertingToColorSpace:renderingIntent:` takes
            // an `NSInteger` intent.
            let srgb = appkit::send_id(class(c"NSColorSpace").as_id(), sel!(sRGBColorSpace));
            if srgb.is_null() {
                return Err("could not convert the image to sRGB".to_string());
            }
            let convert: unsafe extern "C-unwind" fn(Id, Sel, Id, isize) -> Id = aterm_objc::msg();
            let rep = convert(
                rep,
                sel!(bitmapImageRepByConvertingToColorSpace:renderingIntent:),
                srgb,
                NS_COLOR_RENDERING_INTENT_PERCEPTUAL,
            );
            if rep.is_null() {
                return Err("could not convert the image to sRGB".to_string());
            }
            // PNG normalizes channel order and premultiplication to straight
            // RGBA. `-representationUsingType:properties:` returns `NSData *`;
            // the empty properties dictionary is an autoreleased `+dictionary`.
            let properties = appkit::send_id(class(c"NSDictionary").as_id(), sel!(dictionary));
            let representation: unsafe extern "C-unwind" fn(Id, Sel, usize, Id) -> Id =
                aterm_objc::msg();
            let png = representation(
                rep,
                sel!(representationUsingType:properties:),
                NS_BITMAP_IMAGE_FILE_TYPE_PNG,
                properties,
            );
            if png.is_null() {
                return Err("could not re-encode the decoded image".to_string());
            }
            // `-length` returns `NSUInteger`; `-getBytes:length:` is a bounded
            // copy into a caller-owned buffer.
            let png_len = appkit::send_usize(png, sel!(length));
            let mut png_bytes = vec![0_u8; png_len];
            if png_len != 0 {
                let get_bytes: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void, usize) =
                    aterm_objc::msg();
                get_bytes(
                    png,
                    sel!(getBytes:length:),
                    png_bytes.as_mut_ptr().cast(),
                    png_len,
                );
            }
            aterm_png::decode_rgba8(&png_bytes, MAX_WALLPAPER_SOURCE_DIMENSION as u32)
                .ok_or_else(|| "could not read the decoded image pixels".to_string())
        }
    })
}

/// The AppKit wallpaper decoder on real bytes: a 24-bit BMP (which only the
/// AppKit path can decode) must come back with the right geometry and channel
/// order.
#[cfg(all(test, target_os = "macos"))]
mod appkit_wallpaper_tests {
    use super::decode_wallpaper_appkit;

    /// A 2x2 24-bit BMP: red, green on the top row; blue, white below. BMP
    /// stores rows bottom-up as B,G,R with 4-byte row padding, so the decoder
    /// cannot just echo the bytes.
    fn two_by_two_bmp() -> Vec<u8> {
        let mut v = Vec::new();
        // BITMAPFILEHEADER: "BM", size, reserved, pixel-data offset.
        v.extend_from_slice(b"BM");
        v.extend_from_slice(&70_u32.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        v.extend_from_slice(&54_u32.to_le_bytes());
        // BITMAPINFOHEADER: 40 bytes, 2x2, 1 plane, 24bpp, BI_RGB.
        v.extend_from_slice(&40_u32.to_le_bytes());
        v.extend_from_slice(&2_i32.to_le_bytes());
        v.extend_from_slice(&2_i32.to_le_bytes());
        v.extend_from_slice(&1_u16.to_le_bytes());
        v.extend_from_slice(&24_u16.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        v.extend_from_slice(&16_u32.to_le_bytes());
        v.extend_from_slice(&2835_i32.to_le_bytes());
        v.extend_from_slice(&2835_i32.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        // Bottom row first: blue, white — then two padding bytes.
        v.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]);
        // Top row: red, green — then two padding bytes.
        v.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        v
    }

    /// The strongest channel of an RGBA pixel, as an index, plus its alpha.
    fn dominant(px: &[u8]) -> (usize, u8) {
        let (mut best, mut best_v) = (0, px[0]);
        for (i, &v) in px.iter().take(3).enumerate() {
            if v > best_v {
                best = i;
                best_v = v;
            }
        }
        (best, px[3])
    }

    #[test]
    fn appkit_decodes_a_bmp_with_the_right_geometry_and_channel_order() {
        let (rgba, w, h) =
            decode_wallpaper_appkit(&two_by_two_bmp()).expect("AppKit decoded the BMP");
        assert_eq!((w, h), (2, 2));
        assert_eq!(rgba.len(), 2 * 2 * 4);
        // Row-major, TOP-DOWN, straight RGBA — which is BMP's own order
        // inverted, so a decoder that forgot the flip fails here.
        let px = |i: usize| &rgba[i * 4..i * 4 + 4];
        assert_eq!(dominant(px(0)).0, 0, "top-left is red");
        assert_eq!(dominant(px(1)).0, 1, "top-right is green");
        assert_eq!(dominant(px(2)).0, 2, "bottom-left is blue");
        for i in 0..4 {
            assert_eq!(px(i)[3], 255, "pixel {i} lost its alpha");
        }
        let white = px(3);
        assert!(
            white[0] > 200 && white[1] > 200 && white[2] > 200,
            "bottom-right should be near-white, got {white:?}"
        );
    }

    /// Bytes that are not an image are an error, not a panic and not a
    /// zero-sized success — the failure mode the wallpaper picker relies on.
    #[test]
    fn appkit_refuses_bytes_that_are_not_an_image() {
        assert!(decode_wallpaper_appkit(b"not an image at all").is_err());
        assert!(decode_wallpaper_appkit(&[]).is_err());
    }
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
    // Downscale (linear-light, aspect-preserving) to the keep budget; windows
    // cover-scale from this, and the budget exceeds any frame.
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

/// Escape `w` as a TOML basic string (with quotes), including control
/// characters; one bad `extra_words` entry would otherwise break the whole
/// override document.
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
    /// Engine `TerminalConfig` deltas this config implies, or `None` when
    /// nothing engine-side is set; [`Self::terminal_config_for`] at the default
    /// (Dark) appearance. Test-only.
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
            // Underline is not a config option; programs may still request it
            // via DECSCUSR. A config asking for it falls back to the bar with a
            // one-line note.
            tc.cursor_style = if cursor_style.eq_ignore_ascii_case("block") {
                if blink {
                    CursorStyle::BlinkingBlock
                } else {
                    CursorStyle::SteadyBlock
                }
            } else if cursor_style.eq_ignore_ascii_case("underline") {
                crate::logging::stderr_line!(
                    "aterm-gui: config cursor_style \"underline\" is retired; using \"bar\""
                );
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
                crate::logging::stderr_line!(
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
        // A named theme seeds the engine fg/bg, cursor and ANSI palette; the
        // per-key colors below override individual slots.
        if self.resolve_theme_name(appearance).is_some() {
            // A theme that is absent or invalid resolves to Default here in
            // silence; [`theme_load_notices`] says so, once per launch or reload.
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
                    None => crate::logging::stderr_line!(
                        "aterm-gui: config {key}: expected #RRGGBB, got {s:?}"
                    ),
                }
            }
        }
        // Mirror the selection colour into the engine so OSC 21 queries report it.
        if let Some(s) = &self.selection_color {
            match parse_hex_color(s) {
                Some(rgb) => {
                    tc.selection_background = Some(rgb);
                    any = true;
                }
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config selection_color: expected #RRGGBB, got {s:?}"
                    )
                }
            }
        }
        // Selected-text foreground is OSC 19 state; mirror it so queries, 119,
        // RIS and new sessions share the configured baseline.
        if let Some(s) = &self.selection_foreground {
            match parse_hex_color(s) {
                Some(rgb) => {
                    tc.selection_foreground = Some(rgb);
                    any = true;
                }
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config selection_foreground: expected #RRGGBB, got {s:?}"
                    )
                }
            }
        }
        // Indexed palette (engine `custom_palette`): explicit entries layer on
        // top of the theme's ANSI palette, else start empty.
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
                        crate::logging::stderr_line!(
                            "aterm-gui: config palette[{i}]: expected #RRGGBB, got {hex:?}"
                        )
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
                other => crate::logging::stderr_line!(
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
                other => crate::logging::stderr_line!(
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

    /// The engine [`TerminalConfig`] to apply: [`Self::terminal_config`] with the
    /// default fg/bg pinned to the renderer [`Self::theme`].
    ///
    /// The engine's spec default background is black, but the window clears to
    /// the theme background; pinning makes a default cell paint exactly the
    /// window colour. `theme()` already folds in `foreground`/`background`.
    #[cfg(test)]
    pub(crate) fn applied_terminal_config(&self) -> aterm_core::config::TerminalConfig {
        self.applied_terminal_config_for(aterm_types::Appearance::Dark)
    }

    /// [`Self::applied_terminal_config`] for a specific OS `appearance`, applied
    /// live when a `dark:…,light:…` theme flips (see
    /// [`Self::resolve_theme_name`]).
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

/// The `font_px_explicit` pin after a reload: an admitted `--font-px` /
/// `config.font_px` pins; otherwise a px that differs from the scale default is
/// a live zoom and keeps its pin. Dropping it would let
/// [`App::apply_window_scale`] reset the px without a re-grid, drawing a frame
/// wider than the window.
pub(crate) fn reload_font_pin(
    explicit_now: bool,
    new_font_px: f32,
    new_default_font_px: f32,
) -> bool {
    explicit_now || (new_font_px - new_default_font_px).abs() >= 0.5
}

/// Resolve the config file path without creating anything, via
/// `aterm_types::dirs::aterm_config_path` (`$XDG_CONFIG_HOME`, then `%APPDATA%`
/// on Windows, then `$HOME/.config`). Shared with the CLI's `explain-config` and
/// `doctor`.
pub(crate) fn config_path() -> Option<std::path::PathBuf> {
    aterm_types::dirs::aterm_config_path()
}

/// One launch override that outranks aterm.toml, shared by Settings and
/// Manual's host diagnostics so they agree on pinned values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActiveEnvironmentOverride {
    /// The launch flag that set it (`--font-px`, `--columns`); no environment
    /// variable overrides an aterm.toml key.
    pub(crate) variable: &'static str,
    pub(crate) effective: String,
}

/// Resolve an active launch override for one config key. Invalid values are
/// absent because the runtime falls through to aterm.toml for them too.
pub(crate) fn active_environment_override(key: &str) -> Option<ActiveEnvironmentOverride> {
    let resolved = |variable, effective| ActiveEnvironmentOverride {
        variable,
        effective,
    };
    match key {
        "columns" => explicit_initial_columns()
            .map(|value| resolved("--columns", value.clamp(20, 500).to_string())),
        "lines" => explicit_initial_lines()
            .map(|value| resolved("--lines", value.clamp(5, 300).to_string())),
        "font_px" => launch::flags()
            .font_px
            .map(|value| resolved("--font-px", value.to_string())),
        "font_family" => launch::flags()
            .font_family
            .clone()
            .map(|value| resolved("--font", value)),
        "gpu" => launch::flags().renderer.map(|flag| match flag {
            launch::RendererFlag::Cpu => resolved("--cpu", "CPU".to_string()),
            launch::RendererFlag::Gpu => resolved("--gpu", "GPU".to_string()),
        }),
        "shell" => crate::cli::launch_flags()
            .shell
            .filter(|value| !value.is_empty())
            .map(|value| resolved("--shell", value)),
        // No `update.*`, `packages.*` or `net.*` key has a launch override.
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
                let notice = launch_config_notice(&path, LaunchConfigProblem::Unreadable(&error));
                crate::logging::stderr_line!("aterm-gui: {notice}");
                note_launch_load_failed(notice);
                return Config::default();
            }
        };
    let config = Config::parse(&observation.text).unwrap_or_else(|e| {
        let notice = launch_config_notice(&path, LaunchConfigProblem::Invalid(&e));
        crate::logging::stderr_line!("aterm-gui: {notice}");
        note_launch_load_failed(notice);
        Config::stand_in(&observation.text)
    });
    warn_deprecated_display_font_spelling(&observation.text, &config);
    config
}

/// Why the launch could not take `aterm.toml`: unreadable (permissions,
/// dangling link, directory) or not a configuration.
pub(crate) enum LaunchConfigProblem<'a> {
    Unreadable(&'a dyn std::fmt::Display),
    Invalid(&'a dyn std::fmt::Display),
}

/// Launch-time config notice, shown in the window banner and on the console
/// when `aterm.toml` did not load at start: names the path, the problem, the
/// consequence (defaults in use) and what ends it.
pub(crate) fn launch_config_notice(
    path: &std::path::Path,
    problem: LaunchConfigProblem<'_>,
) -> String {
    match problem {
        LaunchConfigProblem::Unreadable(error) => format!(
            "aterm.toml could not be read at launch ({error}) — every setting is running at its \
             default. Fix {} and it loads on the next change.",
            path.display()
        ),
        LaunchConfigProblem::Invalid(error) => format!(
            "aterm.toml is not a valid configuration ({error}) — the window's settings except \
             [harness] are at their defaults. Fix {} and it loads on the next change.",
            path.display()
        ),
    }
}

/// The [`launch_config_notice`] from a failed launch load, set once by
/// [`load_config`]. The config watcher uses it so a later reload refusal says
/// the running settings are launch defaults, and `--diagnose` /
/// `--show-config` report it on stdout.
static LAUNCH_LOAD_FAILURE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn note_launch_load_failed(notice: String) {
    // First failure wins (`load_config` runs more than once). The band row goes
    // through the pre-App inbox, which folds repeats.
    crate::message_inbox::queue_message(crate::message_reporters::launch_load_failure(&notice));
    let _ = LAUNCH_LOAD_FAILURE.set(notice);
}

/// WHY this process launched without its `aterm.toml` — the same sentence the
/// console and the window banner carry, so a stdout-only report can carry it too.
pub(crate) fn launch_load_failure() -> Option<&'static str> {
    LAUNCH_LOAD_FAILURE.get().map(String::as_str)
}

/// The current `[net]` table, read without [`load_config`]'s stderr lines and
/// launch banner, using the same file resolution and parse. A missing file is
/// no table; `Err` says why an existing file gives none, for `dial` and
/// `dial-list`.
pub(crate) fn net_config_now() -> Result<Option<NetConfig>, String> {
    let Some(path) = config_path() else {
        return Ok(None);
    };
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, true)
            .map_err(|error| error.replace('\n', "; "))?;
    net_config_from_text(&observation.text)
}

/// [`net_config_now`] over `text`: the parse error as `line <n>: <message>`
/// (its annotated `Display` spans several lines, and a control reply is one).
pub(crate) fn net_config_from_text(text: &str) -> Result<Option<NetConfig>, String> {
    Config::parse(text)
        .map(|config| config.net)
        .map_err(|error| {
            let message = error.message_text().replace('\n', "; ");
            match error.span() {
                Some(span) => {
                    // End of input sits on the last line, not the empty one after it.
                    let before = text.as_bytes().get(..span.start).unwrap_or(text.as_bytes());
                    let before = if span.start >= text.len() {
                        before.strip_suffix(b"\n").unwrap_or(before)
                    } else {
                        before
                    };
                    let line = before.iter().filter(|&&byte| byte == b'\n').count() + 1;
                    format!("line {line}: {message}")
                }
                None => message,
            }
        })
}

/// The `agents_auto_prime` setting, read without [`load_config`]'s stderr lines
/// or notices (the auto-prime thread must stay quiet). Same file resolution and
/// parse; unreadable, missing or malformed files give the default (on).
pub(crate) fn agents_auto_prime_setting() -> bool {
    let Some(path) = config_path() else {
        return true;
    };
    match crate::native_config_service::VersionedConfigService::observe_path(&path, true) {
        Ok(observation) => agents_auto_prime_from_text(&observation.text),
        Err(_) => true,
    }
}

/// Read source and application policy from ONE persisted config observation.
/// Missing config keeps defaults; unreadable/malformed existing config refuses
/// automatic work rather than silently turning a saved manual policy on.
pub(crate) fn update_check_settings_setting() -> Option<aterm_update::CheckSettings> {
    let config = if let Some(path) = config_path() {
        match crate::native_config_service::VersionedConfigService::observe_path(&path, true) {
            Ok(observation) => return update_check_settings_from_text(&observation.text),
            Err(_) => match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Config::default(),
                _ => return None,
            },
        }
    } else {
        Config::default()
    };
    Some(update_check_settings(&config))
}

fn update_check_settings_from_text(text: &str) -> Option<aterm_update::CheckSettings> {
    aterm_toml::from_str::<Config>(text)
        .ok()
        .map(|config| update_check_settings(&config))
}

pub(crate) fn update_check_settings(config: &Config) -> aterm_update::CheckSettings {
    let update = config.update.as_ref();
    aterm_update::CheckSettings {
        source: aterm_update::Source::resolve(
            update.and_then(|u| u.owner.as_deref()),
            update.and_then(|u| u.repo.as_deref()),
        ),
        auto_apply: update.and_then(|u| u.auto_apply).unwrap_or(true),
    }
}

/// Pure core of [`agents_auto_prime_setting`]: the knob's value in `text`, the
/// default when the text does not parse as a config.
pub(crate) fn agents_auto_prime_from_text(text: &str) -> bool {
    match aterm_toml::from_str::<Config>(text) {
        Ok(config) => config.agents_auto_prime_or_default(),
        Err(_) => true,
    }
}

/// Resolve the glyph size in physical px with the canonical precedence
/// `--font-px > config.font_px > FONT_PX default`. Only finite values inside
/// `FONT_PX_MIN..=FONT_PX_MAX` are admitted; an invalid source falls through instead
/// of being clamped. Shared by startup (`main`) and live
/// hot-reload (`App::reload_config`) so a reload re-applies the SAME precedence —
/// the launch flag still wins after the user edits the config file.
pub(crate) fn resolve_font_px(config: &Config) -> f32 {
    resolve_font_px_with(launch::flags().font_px, config.font_px)
}

/// Banner line for a `font_px` that [`resolve_font_px`] ignores (outside
/// `FONT_PX_MIN..=FONT_PX_MAX` or not finite), or `None` when unset or
/// admissible. Uses the resolver's predicate ([`font_px_in_range`]); says the
/// value is ignored and the accepted range. Also printed by `--validate-config`
/// and pushed at startup and every reload.
pub(crate) fn font_px_load_notice(config: &Config) -> Option<String> {
    let px = config.font_px.filter(|px| !font_px_in_range(px))?;
    Some(format!(
        "config font_px: {px:?} is not accepted (expected {FONT_PX_MIN}–{FONT_PX_MAX}; the \
         value is ignored)"
    ))
}

/// Each configured `theme` name the window cannot use against `themes`, and that it
/// draws Default instead: one sentence per name, so a plain name missing from both
/// appearances is said once. `--validate-config` and Manual print these words; the
/// band carries them as [`theme_load_notices`].
pub(crate) fn theme_misses(config: &Config, themes: &ThemeCatalog) -> Vec<String> {
    let mut misses = Vec::new();
    for appearance in [
        aterm_types::Appearance::Dark,
        aterm_types::Appearance::Light,
    ] {
        if let Some(name) = config.resolve_theme_name(appearance)
            && !name.eq_ignore_ascii_case("default")
            && let Err(error) = themes.resolve(&name)
        {
            let miss = format!("theme: {name:?} is unavailable ({error}); Default is used");
            if !misses.contains(&miss) {
                misses.push(miss);
            }
        }
    }
    misses
}

/// [`theme_misses`] for the band's unaccepted-values row, pushed at startup,
/// every reload and every theme-directory change.
pub(crate) fn theme_load_notices(config: &Config, themes: &ThemeCatalog) -> Vec<String> {
    prefixed_notices(theme_misses(config, themes))
}

/// Whether a valid flag/config size pins the physical glyph size.
/// Merely authoring an invalid `font_px` must not disable the display-scaled
/// default: admission and explicitness deliberately share the same predicate.
pub(crate) fn font_px_is_explicit(config: &Config) -> bool {
    font_px_is_explicit_with(launch::flags().font_px, config.font_px)
}

/// Whether to request the GPU renderer, with the same precedence as `main`'s
/// backend selection: `--cpu`, else `--gpu` (last one given wins), else config
/// `gpu`, else GPU (the CPU renderer is the automatic fallback).
pub(crate) fn resolve_want_gpu(config: &Config) -> bool {
    let flag = launch::flags().renderer;
    resolve_want_gpu_with(
        flag == Some(launch::RendererFlag::Cpu),
        flag == Some(launch::RendererFlag::Gpu),
        config.gpu,
    )
}

/// Effective shell command captured by every newly-created session: `--shell`
/// outranks the config `shell`. Shared by startup and config reload, so saved
/// `shell` edits reach the next tab without waiting for a process restart while
/// the launch flag keeps its documented precedence.
pub(crate) fn resolve_shell_override(config: &Config) -> Option<String> {
    resolve_shell_override_with(crate::cli::launch_flags().shell, config.shell.as_deref())
}

fn resolve_shell_override_with(flag: Option<String>, configured: Option<&str>) -> Option<String> {
    flag.filter(|value| !value.is_empty())
        .or_else(|| configured.map(str::to_string))
}

/// Pure precedence core for [`resolve_want_gpu`] with the two flags and the config
/// value passed in explicitly, so it is deterministically unit-testable. Mirrors
/// the `main` funnel exactly.
pub(crate) fn resolve_want_gpu_with(
    force_cpu: bool,
    force_gpu: bool,
    config_gpu: Option<bool>,
) -> bool {
    !force_cpu
        && match (force_gpu, config_gpu) {
            (true, _) => true,
            (false, Some(explicit)) => explicit,
            (false, None) => true,
        }
}

/// Notices for changed restart-only keys, which [`App::reload_config`] does not
/// hot-apply: the initial grid (`columns`/`lines`, so a reload does not snap the
/// window back) and `gpu`. The lines feed the `config.restart` band row
/// (`message_reporters::ConfigFamily::Restart`). Pure; launch flags are fixed
/// per process and ignored.
pub(crate) fn restart_notices(old: &Config, new: &Config) -> Vec<String> {
    let mut out = Vec::new();
    if old.columns != new.columns || old.lines != new.lines {
        out.push(
            "columns/lines applies on next launch (resize the window to change size now)"
                .to_string(),
        );
    }
    // The renderer backend is built once at launch; a live GPU↔CPU swap would
    // tear down every window's present surface.
    if old.gpu != new.gpu {
        out.push(
            "gpu applies on next launch (the renderer backend is chosen at startup)".to_string(),
        );
    }
    out
}

/// The band's view of [`crate::native_config_language::key_warnings`], added to
/// `warns` in two families, each sentence prefixed `config …`:
///
/// * keys `aterm.toml` sets that this build ignores
///   (`message_reporters::ConfigFamily::IgnoredKeys`), one row with every
///   sentence as a detail line;
/// * retired and deprecated spellings still read by a newer key
///   (`ConfigFamily::RetiredKeys`), a record rather than a row, so a working
///   legacy spelling is not presented as unknown.
///
/// A known key whose value its reader could not use
/// (`native_config_language::ConfigAnalysis::unaccepted_values`) is in
/// neither: [`unaccepted_value_notices`] tells it in
/// `ConfigFamily::UnacceptedValues`.
///
/// Windowed launches have no stderr, and a misspelled key is otherwise
/// invisible. Startup and reload both call this.
pub(crate) fn collect_key_notices(
    warns: &mut crate::message_reporters::ConfigWarnings,
    text: &str,
) {
    use crate::message_reporters::ConfigFamily;
    let keys = crate::native_config_language::key_warnings(text);
    warns.extend(ConfigFamily::IgnoredKeys, prefixed_notices(keys.ignored));
    warns.extend(ConfigFamily::RetiredKeys, prefixed_notices(keys.retired));
}

/// The supervisor's `[harness]` notices ([`Config::harness_notices`]) added to
/// `warns` as `ConfigFamily::UnacceptedValues`; shared by launch and both reload
/// paths.
pub(crate) fn collect_harness_notices(
    warns: &mut crate::message_reporters::ConfigWarnings,
    config: &Config,
) {
    use crate::message_reporters::ConfigFamily;
    warns.extend(ConfigFamily::UnacceptedValues, config.harness_notices());
}

/// The `IgnoredKeys` half of [`collect_key_notices`], for the tests that read
/// its sentences.
#[cfg(test)]
pub(crate) fn ignored_key_notices(text: &str) -> Vec<String> {
    prefixed_notices(crate::native_config_language::key_warnings(text).ignored)
}

/// The band's view of
/// [`crate::native_config_language::unaccepted_value_warnings`]: keys whose
/// value this build rejects or its reader could not use, minus any
/// `already_told` explains, prefixed like [`collect_key_notices`]. Call it
/// last, after every loader-specific warning, so a resolver's fuller sentence
/// is not duplicated by the generic one.
pub(crate) fn unaccepted_value_notices(text: &str, already_told: &[String]) -> Vec<String> {
    prefixed_notices(crate::native_config_language::unaccepted_value_warnings(
        text,
        already_told,
    ))
}

/// Give config lines the `config …` prefix used on stderr.
fn prefixed_notices(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| format!("config {line}"))
        .collect()
}

/// Fresh-launch terminal width after the documented flag > config > default
/// precedence and the same safety clamp used by window construction.
pub(crate) fn resolve_initial_columns(config: &Config) -> u16 {
    explicit_initial_columns()
        .or(config.columns)
        .unwrap_or(80)
        .clamp(20, 500)
}

/// Fresh-launch terminal height; the row twin of [`resolve_initial_columns`]. A
/// seamless handoff may supply a carried frame first.
pub(crate) fn resolve_initial_lines(config: &Config) -> u16 {
    explicit_initial_lines()
        .or(config.lines)
        .unwrap_or(24)
        .clamp(5, 300)
}

/// Explicit launch grid overrides only (`--columns`/`--lines`), without the
/// config fallback. Cold restore needs the distinction: an explicit flag
/// outranks the persisted session grid, a config `columns` does not.
/// Unclamped; callers clamp alongside the value they merge with.
pub(crate) fn explicit_initial_columns() -> Option<u16> {
    crate::cli::launch_flags().columns
}

/// Row twin of [`explicit_initial_columns`].
pub(crate) fn explicit_initial_lines() -> Option<u16> {
    crate::cli::launch_flags().lines
}

/// Explicit render scale from `--scale` (finite, positive). Overrides both the
/// headless 1.0 default and a window's `scale_factor()`, driving the auto-scaled
/// font and padding so an offscreen `image` renders at that DPI. One load of the
/// launch flags, since this is on the redraw path.
pub(crate) fn resolve_force_scale() -> Option<f64> {
    launch::flags().scale
}

/// Pure precedence core for [`resolve_font_px`], with the `--font-px` flag value
/// and the config value passed in explicitly so it is deterministically
/// unit-testable. Order: a finite, in-range flag value wins; else a finite,
/// in-range config value; else the built-in default. An out-of-range source falls
/// through rather than being clamped.
pub(crate) fn resolve_font_px_with(flag: Option<f32>, config: Option<f32>) -> f32 {
    // Filter each source by range independently so a bad flag falls through to
    // a valid config value.
    admitted_font_px(flag, config).unwrap_or(FONT_PX)
}

fn font_px_in_range(value: &f32) -> bool {
    value.is_finite() && *value >= FONT_PX_MIN && *value <= FONT_PX_MAX
}

fn admitted_font_px(flag: Option<f32>, config: Option<f32>) -> Option<f32> {
    flag.filter(font_px_in_range)
        .or(config.filter(font_px_in_range))
}

/// Explicit-pin counterpart of [`resolve_font_px_with`], on the same admission
/// helper so an invalid value never pins the fallback and bypasses auto-size.
pub(crate) fn font_px_is_explicit_with(flag: Option<f32>, config: Option<f32>) -> bool {
    admitted_font_px(flag, config).is_some()
}

/// A glyph size as the window carry writes it: thousandths of a physical px,
/// `None` outside `FONT_PX_MIN..=FONT_PX_MAX`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "admitted to FONT_PX_MIN..=FONT_PX_MAX first, so the product is a positive \
              integer of at most 200 000"
)]
pub(crate) fn font_px_milli(px: f32) -> Option<u32> {
    font_px_in_range(&px).then(|| (px * 1000.0).round() as u32)
}

/// The launch font of a process that ADOPTED a seamless update's sessions,
/// before its first window is sized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LaunchFont {
    /// The glyph size to draw at (`App::font_px`).
    pub(crate) px: f32,
    /// Whether it is pinned (`App::font_px_explicit`), which stops the
    /// Retina auto-scale from replacing it at the first window's attach.
    pub(crate) explicit: bool,
    /// What Cmd-0 resets to (`App::default_font_px`).
    pub(crate) reset_px: f32,
}

/// The carried zoom, or the config's font. `resolved`/`explicit` come from
/// this launch's flags and config; `carry` is the outgoing window's frame,
/// `Some` only on an authenticated handoff boot.
///
/// A carry holding the zoom pair (`WindowCarry::font_px_milli` and
/// `font_reset_px_milli`) wins: the successor draws at the zoomed px, pinned
/// like a live zoom (`App::set_font_px_with`), and Cmd-0 returns to the
/// outgoing reset size. It outranks the flags, since a successor inherits its
/// parent's launch argv. A pair with either half missing or out of bounds is
/// ignored whole (no clamping).
pub(crate) fn successor_font_px(
    resolved: f32,
    explicit: bool,
    carry: Option<&crate::session_store::WindowCarry>,
) -> LaunchFont {
    let (px_milli, reset_milli) = carry.map_or((None, None), |carry| {
        (carry.font_px_milli, carry.font_reset_px_milli)
    });
    launch_font_for_zoom(resolved, explicit, px_milli, reset_milli)
}

/// [`successor_font_px`] over the zoom pair alone, for the warm prologue that
/// runs before the dial (`WarmOrder::BeforeDial`) using the launch hint
/// (`handoff_warm_hint`). Same admission, so a matching hint builds at the size
/// the carry then selects.
#[allow(
    clippy::cast_precision_loss,
    reason = "a carried size is a u32 of thousandths; any value past f32's exact range is far \
              outside the admitted bounds and is refused by them"
)]
pub(crate) fn launch_font_for_zoom(
    resolved: f32,
    explicit: bool,
    px_milli: Option<u32>,
    reset_milli: Option<u32>,
) -> LaunchFont {
    let admitted = |milli: Option<u32>| {
        milli
            .map(|milli| milli as f32 / 1000.0)
            .filter(font_px_in_range)
    };
    let zoom = admitted(px_milli).zip(admitted(reset_milli));
    match zoom {
        Some((px, reset_px)) => LaunchFont {
            px,
            explicit: true,
            reset_px,
        },
        None => LaunchFont {
            px: resolved,
            explicit,
            reset_px: resolved,
        },
    }
}

/// Resolve one raw style against the host's catalog revision. Builtins and
/// aliases come from `prefs`; this adds the `pack:<id>` domain and fail-closed
/// classification.
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
        // An unrecognized spelling falls back to the default style and keeps
        // `issue` for the warning and Settings. `style: None` is reserved for
        // `off` and an unloaded `pack:`, since `resolve_cursor_glow` gates the
        // whole effect on it.
        return ResolvedTrailStyle {
            canonical: Some(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE),
            style: Some(aterm_effects::cursor_glow::GlowStyle::parse(
                crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
            )),
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

/// Resolve every string-keyed trail presentation fork in one pass, per config
/// generation; the `Copy` result is cached in [`TrailPackCatalog`].
pub(crate) fn resolve_trail_presentation(
    raw: &str,
    catalog: &TrailPackCatalog,
) -> ResolvedTrailPresentation {
    resolve_trail_presentation_from_style(raw, resolve_trail_style(raw, catalog))
}

/// Presentation half for preview surfaces that already hold a synthetic
/// resolution (notably an in-memory Trail Pack not installed in a catalog).
pub(crate) fn resolve_trail_presentation_from_style(
    raw: &str,
    style: ResolvedTrailStyle,
) -> ResolvedTrailPresentation {
    use aterm_effects::cursor_glow::{GlowStyle, style_has_beam_of};

    let token = style.canonical.unwrap_or(raw.trim());
    let glow_style = style.style.unwrap_or(GlowStyle::Lumen);
    let pet_species = (style.style == Some(GlowStyle::RainbowKitty)
        && GlowStyle::style_names_any_pet(token))
    .then(|| {
        if GlowStyle::style_names_dog_pet(token) {
            aterm_effects::kitty_pet::PetSpecies::Dog
        } else {
            aterm_effects::kitty_pet::PetSpecies::Cat
        }
    });
    ResolvedTrailPresentation {
        style,
        beam: style_has_beam_of(glow_style, token),
        ribbon_tall: !GlowStyle::style_names_underline_ribbon(token),
        ribbon_flat: GlowStyle::style_names_flat_ribbon(token),
        classic_mono: GlowStyle::style_names_classic_mono(token),
        pet_species,
        comet: style.style == Some(GlowStyle::Comet),
    }
}

/// The spelling the raw-token predicates must read: what the trail draws, not
/// what was authored. The companion, species, comet and ribbon forks are string
/// predicates, so a typo must be replaced by the spelling
/// [`resolve_trail_style`] settled on. Aliases collapse onto their canonical
/// (pinned by `prefs`' `cursor_trail_style_aliases_agree_with_engine_parse`);
/// `pack:<id>` is returned unchanged.
#[cfg(test)]
pub(crate) fn effective_trail_style_token(raw: &str) -> &str {
    let raw = raw.trim();
    if raw.starts_with("pack:") {
        return raw;
    }
    crate::prefs::cursor_trail_style_canonical(raw)
        .unwrap_or(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE)
}

/// Cold-path construction of the engine's `GlowConfig` through
/// [`aterm_effects::cursor_fx::resolve_glow_config`] (shared with the web
/// pipeline). The live terminal and Settings preview differ only in injected
/// geometry/theme facts (`dark_theme`, `head_dx`).
pub(crate) fn resolve_cursor_glow(
    inputs: CursorGlowInputs,
    presentation: ResolvedTrailPresentation,
    theme_cursor: u32,
    dark_theme: bool,
    // Theme default fg/bg (`0x00RRGGBB`, or `COLOR_UNSET`); parameters because
    // live values are folded per frame.
    theme_fg: u32,
    theme_bg: u32,
    head_dx: f32,
) -> aterm_effects::cursor_glow::GlowConfig {
    // An unrecognized spelling arrives here already resolved to the default
    // style and renders; only `off` and a missing pack resolve to no style.
    aterm_effects::cursor_fx::resolve_glow_config(
        aterm_effects::cursor_fx::GlowKnobs {
            enabled: inputs.enabled,
            color: inputs.color,
            accent: inputs.accent,
            duration_ms: inputs.duration_ms,
            length: inputs.length,
            intensity: inputs.intensity,
            radius: inputs.radius,
            ring: inputs.ring,
        },
        aterm_effects::cursor_fx::GlowSpelling {
            style: presentation.style.style,
            pack: presentation.style.pack,
            beam: presentation.beam,
            // The tall body is the default; only `... underline` selects the
            // under-baseline mark (owner, 2026-08-29).
            ribbon_tall: presentation.ribbon_tall,
            ribbon_flat: presentation.ribbon_flat,
            classic_mono: presentation.classic_mono,
        },
        theme_cursor,
        dark_theme,
        theme_fg,
        theme_bg,
        head_dx,
    )
}

/// Lexicon conflicts [`App::recompute_sparkle`] logs for the resolved scan
/// options: a "requires `cjk_single_char = true`" warning is dropped when that
/// opt-in is on. Shared with `EffectsPipeline::sparkle_lexicon_warnings`.
fn sparkle_logged_warnings(
    conflicts: &[String],
    cjk_single_char: bool,
) -> impl Iterator<Item = &String> {
    conflicts
        .iter()
        .filter(move |w| aterm_effects::pipeline::lexicon_warning_applies(w, cjk_single_char))
}

/// Render authority that must roll back with a failed backend rebuild during
/// reload, so a rejected font/geometry change does not discard the same edit's
/// successful input, session or package changes.
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

        // Theme colours feed both the engine defaults and the renderer; re-derive
        // the engine config from the hybrid config so both stay consistent.
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

/// Restore only the config keys behind [`ReloadRenderSnapshot`], keeping every
/// other freshly parsed edit. Restoring them also defeats reload dedupe, so the
/// watcher's next edge can retry a transient renderer failure.
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
    /// Rebuild [`App::sparkle`] from config and Serious Mode policy. Runs only
    /// when `sparkle_dirty` is set, never per frame. A malformed user lexicon is
    /// logged and the builtin used.
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

    /// Resolve the sparkle runtime if still dirty, so a gate asking "do cats
    /// exist?" gets the same answer a drawn frame would.
    ///
    /// `self.sparkle` is a render-path cache filled by the first frame. A
    /// headless instance driven over the control socket may not have drawn one,
    /// so reading the raw field would report effects off. Gates on the cat's
    /// existence use this, never the field.
    pub(crate) fn ensure_sparkle(&mut self) {
        if self.sparkle_dirty {
            self.recompute_sparkle();
        }
    }

    /// Whether the Kitty Log recorder is on (`[sparkle_words.feline] log`,
    /// default true, config-file only). The engine always records; drain sites
    /// drop when false. Resolved at each drain.
    pub(crate) fn kitty_log_enabled(&self) -> bool {
        self.config
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.feline.as_ref())
            .and_then(|f| f.log)
            .unwrap_or(true)
    }

    /// Curse-bonk host gate (`[sparkle_words.profanity] bonk`, default on). The
    /// engine records cues; this decides whether the drain plays them. A
    /// disabled profanity class produces no cues.
    pub(crate) fn curse_bonk_enabled(&self) -> bool {
        self.config
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.profanity.as_ref())
            .and_then(|p| p.bonk)
            .unwrap_or(true)
    }

    /// Detonation-edge opt-in (`bonk_detonation`, default off): admits
    /// `Detonated` cues from on-screen output.
    pub(crate) fn curse_bonk_detonation_enabled(&self) -> bool {
        self.config
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.profanity.as_ref())
            .and_then(|p| p.bonk_detonation)
            .unwrap_or(false)
    }

    /// Rebuild [`App::rain`] parameters from config and live theme when
    /// `rain_dirty` is set, never per frame. Parameters resolve unconditionally
    /// because a session override can force rain on; the enabled decision is
    /// per-session ([`App::session_rain_enabled`]). The config is pushed into
    /// every live engine (`set_config` keeps the tick epoch); engines are built
    /// lazily and not dropped here, since the render path winds a disabled
    /// window's field down.
    pub(crate) fn recompute_matrix_rain(&mut self) {
        self.rain_dirty = false;
        let bg = self.theme.bg & 0x00FF_FFFF;
        let fg = self.theme.fg & 0x00FF_FFFF;
        let cfg = self.config.matrix_rain_params(bg, fg);
        self.rain = Some(cfg);
        for (wid, ws) in self.windows.iter_mut() {
            if let Some(e) = ws.fx.rain_mut() {
                e.set_config(crate::rain_config_for_window(cfg, *wid));
            }
        }
    }

    /// Effective rain state for session `sid`: its runtime override (View ▸
    /// Matrix Rain, `toggle_matrix_rain`, `aterm-ctl rain`) if set, else the
    /// `[matrix_rain] enabled` bit. The override is never persisted and wins
    /// over reloads until cleared.
    pub(crate) fn session_rain_enabled(&self, sid: u64) -> bool {
        self.serious_mode_policy()
            .allows(crate::motion::SeriousEffect::MatrixRain)
            && self
                .pool
                .rain_override(sid)
                .unwrap_or_else(|| self.config.matrix_rain_enabled())
    }

    /// Set or clear the rain override for `sid`, mark the resolve stale and wake
    /// every window whose front session is `sid`. Turning off does not drop the
    /// engine; the render path clears the field next frame and winds down.
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
        // Refresh an open palette's "Matrix Rain" checkmark so the repaint shows
        // the post-flip state, whatever surface flipped it.
        self.palette_refresh_live();
    }

    /// Toggle matrix rain for the front session of window `wid` (action, menu,
    /// palette, `aterm-ctl rain toggle`): the session's effective state flips
    /// into an explicit override, so it can turn rain on over a disabled config.
    /// No-op when the front content is not a terminal.
    pub(crate) fn toggle_matrix_rain(&mut self, wid: crate::WindowId) {
        let Some(sid) = self.front_terminal(wid).map(|t| t.session) else {
            return;
        };
        let effective = self.session_rain_enabled(sid);
        self.set_session_rain_override(sid, Some(!effective));
    }

    /// The `fx` control verb ([`crate::Wake::FxControl`]) on the focused window's
    /// front session: `Status` reads; `Celebrate` arms the sing-along detector
    /// (`kitty_sing::KittySing::arm_external`), spent by the session's next keyed
    /// edge. `Ok` carries `armed=<none|green|enter> sig=<hex> bars=<n>
    /// live=<bool> bar=<n|none> drive=<f> cooldown_ms=<n>
    /// style=<rainbow-kitty|other>`. `Err` when no window is focused, the front
    /// is not a terminal, the trail is not the rainbow kitty, or the detector
    /// refuses (pending arm, cooldown, bars).
    pub(crate) fn fx_control(&mut self, op: crate::FxCtlOp) -> Result<String, String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no focused window".to_string());
        };
        let Some(sid) = self.front_terminal(wid).map(|t| t.session) else {
            return Err("front content is not a terminal (no session to arm)".to_string());
        };
        let glow = self.glow_config();
        let rainbow = matches!(glow.style, crate::cursor_glow::GlowStyle::RainbowKitty);
        let allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::CursorCat);
        let now = std::time::Instant::now();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return Err("no focused window".to_string());
        };
        if let crate::FxCtlOp::Celebrate { sig, bars, on } = op {
            if !allowed {
                return Err("Serious Mode suppresses cursor companions".to_string());
            }
            if !glow.enabled {
                return Err("cursor trail is disabled (nothing to celebrate with)".to_string());
            }
            if !rainbow {
                return Err(
                    "cursor trail is not the rainbow kitty (nothing to celebrate with)".to_string(),
                );
            }
            ws.fx
                .companions_mut()
                .kitty_sing
                .arm_external(now, sid, sig, bars, on)
                .map_err(|refusal| refusal.to_string())?;
        }
        let sing = &ws.fx.companions_mut().kitty_sing;
        let (armed, sig, bars) = sing
            .armed_external()
            .map_or(("none", 0, 0), |arm| (arm.on.as_str(), arm.sig, arm.bars));
        Ok(format!(
            "armed={armed} sig={sig:08x} bars={bars} live={} bar={} drive={:.2} cooldown_ms={} style={}",
            sing.external_live(),
            sing.bar(now).map_or("none".to_string(), |b| b.to_string()),
            sing.drive(now),
            sing.cooldown_remaining(now).as_millis(),
            if rainbow { "rainbow-kitty" } else { "other" },
        ))
    }

    /// The `rain` control verb ([`crate::Wake::RainControl`]) on the focused
    /// window's front session. Every op answers with one status line:
    /// `config_enabled=<bool> session_override=<none|on|off>
    /// effective=<bool> engine=<none|live> active=<bool>
    /// scope=<window|focused-pane> focused=<bool> animating=<bool>`. `Err` when no
    /// window is focused or the front content is not a terminal.
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
        // Engine introspection: whether the window holds a live engine, whether
        // it is raining/draining (`is_active`, the wake predicate), and the
        // emission scope (`window` for single-pane/zoomed, `focused-pane` for a
        // split, where rain follows the focused pane).
        let (engine, active, diag) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.fx.rain())
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
        // Motion facts: an unfocused or reduced-motion window emits nothing even
        // with `effective=true engine=live`. Resolved through the same fold as
        // the render tick (`motion_focus` plus `motion_policy`'s load-shed
        // latch) so the status matches the live policy.
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

    /// Throttled entry for interactive window resizes. A width change rewraps the
    /// whole scrollback on the event-loop thread, so the first resize of a drag
    /// applies immediately and later width changes coalesce into
    /// `pending_resize`, applied at most ~20 Hz and once more at the trailing
    /// settle (`new_events`). A resize that keeps the column count (height-only)
    /// rewraps nothing and applies immediately.
    ///
    /// Columns come from [`Self::grid_dims_for`] on the window's authoritative
    /// `inner_size()`, the same law `on_resize` uses; rows are not compared
    /// because they also depend on chrome headroom.
    ///
    /// A coalesced size never reaches `on_resize`, so the engine and the
    /// recorder spine only see real sizes in order and replay stays
    /// byte-identical. The control-socket `resize` verb (`apply_term_resize`)
    /// stays immediate.
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
            // Re-arm the settle after a row-only apply. A live-drag apply sizes only
            // the active tab and marks the window `panes_stale`; the trailing settle
            // pays the AllTabs pass for background tabs. A row-only drag coalesces
            // nothing, so without this no settle would be armed.
            //
            // Must run after `on_resize`, since a committing `apply_term_resize`
            // clears both fields. Arming with the current size is safe:
            // `flush_pending_resize` re-reads `inner_size()`.
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

    /// Apply a coalesced resize whose settle deadline fired: reflow the final
    /// size once and clear the throttle state. Also pays the drag's debt: the
    /// eager AllTabs pass for background tabs left `panes_stale` by live-drag
    /// applies.
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
        // Pay the AllTabs debt explicitly: `on_resize` clears `panes_stale` only
        // via `resize_panes`, which early-returns when the grid is unchanged (the
        // usual settle), which would strand background tabs at the pre-drag size.
        // Single-tab windows never set `panes_stale`.
        if self.windows.get(&wid).is_some_and(|ws| ws.panes_stale) {
            self.resize_panes_scoped(wid, false);
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
            w.request_redraw();
        }
    }

    /// Resize entry point for the ledger (`site=window`); only this shim is
    /// `#[track_caller]`, the body is [`Self::on_resize_inner`].
    #[track_caller]
    pub(crate) fn on_resize(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Window);
        self.on_resize_inner(wid, size);
    }

    /// The body of [`Self::on_resize`].
    fn on_resize_inner(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        // Re-read the authoritative `inner_size()`: winit's macOS `Resized` payload
        // comes from `frameDidChange:` and can briefly report a wrong (too small)
        // size, e.g. during a rejected drag-and-drop, and nothing later corrects
        // it. `pad_split` clamps cells to ≥1. Headless windows keep `size`.
        let size = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_ref())
            .map_or(size, |w| w.inner_size());
        // Fullscreen transitions collapse and restore the titlebar band without a
        // scale change, so re-derive the headroom from AppKit on every real resize
        // of a chrome'd window (macOS only), before `grid_dims_for`.
        //
        // Each sample fails closed (`titlebar_band_decision`): fullscreen forces 0;
        // a windowed chrome'd sample of 0 or above the sanity cap restores
        // `last_windowed_band_pts` (not `head_pts`, which is 0 throughout
        // fullscreen). The vendored winit also emits one settled Resized per
        // completed transition.
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
                // The memory always updates (it may need to catch up); the backend
                // retune runs only on a real applied change.
                ws.last_windowed_band_pts = band_memory_pts;
                if ws.head_pts != head_pts || ws.metrics.head != head {
                    ws.head_pts = head_pts;
                    ws.metrics.head = head;
                    self.backend.set_head(head);
                }
            }
        }
        // Windows/Linux synthetic tab-band head, computed from the cell box, strip
        // rows and configured target. Every input funnels through here (font zoom
        // and reload via `rebuild_backend`, `tab_strip_rows` edits, DPI moves via
        // `Resized`); must run before `grid_dims_for`.
        //
        // Attached windows only, like the other head derivations: a never-attached
        // window keeps its sealed boot band so headless captures stay stable.
        //
        // A runtime `cfg!` rather than `#[cfg]`: this is the only non-test caller
        // of the `synthetic_strip_head_px` chain, which would otherwise be dead
        // code on macOS. On macOS `PLATFORM_DEFAULT` is `Compact` and the branch
        // is compiled out, so the measured AppKit band in `ws.metrics.head` is
        // never overwritten.
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
                // Stored in points beside the applied px so the shared re-derivations
                // reading `head_pts` (`on_scale_factor_changed`,
                // `refresh_all_window_metrics`) can reproduce it. Lossy at a new
                // scale when the font is pinned, but the `Resized` that follows
                // `ScaleFactorChanged` re-derives it here within one frame.
                ws.head_pts = head as f64 / if scale > 0.0 { scale } else { 1.0 };
                ws.metrics.head = head;
                self.backend.set_head(head);
            }
        }
        let (rows, cols) = self.grid_dims_for(wid, size);
        // Route through the shared seam so window resizes and the `resize` verb
        // use one clamp and apply path. `echo_to_window: false` because the
        // window already has this size; re-requesting it would fight an edge drag.
        // Only the `resize` verb echoes.
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

    /// Chrome rows changed (tab strip or status bar): clear each window's strip
    /// cache, repaint, and re-grid it from its OS size (`on_resize` →
    /// `apply_term_resize`, a no-op when the count did not move). Shared by the
    /// `tab_strip_rows` reload and `App::sync_message_band_rows`.
    ///
    /// Resize entry point for the ledger (`site=chrome`, outermost, so each
    /// window's `on_resize` books under it); only this shim is
    /// `#[track_caller]`. Inside [`Self::with_chrome_regrid_batch`] it is
    /// recorded and applied at the batch's end.
    #[track_caller]
    pub(crate) fn regrid_for_chrome_rows(&mut self) {
        if let Some(batch) = self.chrome_regrid_batch.as_mut() {
            batch.all();
            return;
        }
        let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Chrome);
        self.regrid_for_chrome_rows_inner();
    }

    /// The body of [`Self::regrid_for_chrome_rows`].
    fn regrid_for_chrome_rows_inner(&mut self) {
        #[cfg(test)]
        {
            crate::resize_ledger::note_chrome_regrid_site_for_test();
            let windows: Vec<WindowId> = self.windows.keys().copied().collect();
            for wid in windows {
                crate::app_presence::note_chrome_regrid_applied(self, wid);
            }
        }
        let sized: Vec<(WindowId, PhysicalSize<u32>)> = self
            .windows
            .iter_mut()
            .filter_map(|(wid, ws)| {
                ws.tab_segments.clear();
                ws.last_strip_fp = None; // E3: chrome geometry changed
                ws.last_present = None;
                ws.os_window.as_ref().map(|w| (*wid, w.inner_size()))
            })
            .collect();
        for (wid, size) in sized {
            self.on_resize(wid, size);
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                // Linux: the whole-cell min-size lattice includes chrome rows, so
                // re-push it or an edge drag snaps one row off.
                #[cfg(target_os = "linux")]
                w.set_min_inner_size(Some(self.whole_cell_min_size(wid)));
                w.request_redraw();
            }
        }
        // The band moved with the chrome, under a pointer that did not
        // (round 44): its lit chip is resolved again.
        self.refresh_band_hover();
    }

    /// The `(rows, cols)` window `wid` gets for raw `size`: the pure half of
    /// [`Self::on_resize`], shared with [`Self::apply_window_scale`] and
    /// [`Self::settle_initial_frame`].
    ///
    /// Uses this window's own cell metrics (mixed DPI). The grid fills the window
    /// minus the `2·pad` border by `aterm_render::pad_split` (maximal grid; the
    /// remainder becomes theme-bg bands at present, so the swapchain is the raw
    /// window size). Chrome rows ([`Self::chrome_rows`]) are reserved out of the
    /// grid, leaving at least one terminal row.
    pub(crate) fn grid_dims_for(&self, wid: WindowId, size: PhysicalSize<u32>) -> (u16, u16) {
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let cols = aterm_render::pad_split(size.width as usize, pad, cw).cells as u16;
        // Reserve the chrome headroom out of the height before fitting; top and
        // bottom pads are explicit.
        let win_rows = crate::asymmetric_pad_cells(
            (size.height as usize).saturating_sub(self.win_head(wid)),
            self.win_pad_top(wid),
            pad,
            ch,
        ) as u16;
        let rows = win_rows.saturating_sub(self.chrome_rows(wid)).max(1);
        (rows, cols)
    }

    /// Minimum inner size that aligns the resize-increment lattice with the
    /// whole-cell frame lattice for window `wid`.
    ///
    /// winit's Wayland backend snaps interactive resizes to
    /// `min_inner_size + k·increment`; with an arbitrary minimum, a vertical drag
    /// could snap the width down and shave a column. The result is the smallest
    /// lattice point at or above the 164×98 logical floor, in physical px like
    /// the increments.
    // Linux-only like its sole caller (`app_window.rs`'s `set_min_inner_size`);
    // `test` because this module's tests call it on every host.
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
            + (1 + usize::from(self.chrome_rows(wid))) * ch;
        let min_h = base_h + floor_h.saturating_sub(base_h).div_ceil(ch) * ch;
        PhysicalSize::new(
            u32::try_from(min_w).unwrap_or(u32::MAX),
            u32::try_from(min_h).unwrap_or(u32::MAX),
        )
    }

    /// Whether applying `size` to window `wid` changes its column count, i.e.
    /// carries a scrollback rewrap; the predicate for
    /// [`Self::on_resize_throttled`].
    ///
    /// Reads the authoritative `inner_size()` like [`Self::on_resize`] (headless
    /// windows keep `size`). Only cols are compared because the titlebar band
    /// affects rows, not cols. An unknown window, or a grid outside
    /// `MAX_GRID_ROWS`/`MAX_GRID_COLS` (whose `ws.cols` never catches up),
    /// reports "reflows", keeping the throttle.
    fn resize_changes_columns(&self, wid: WindowId, size: PhysicalSize<u32>) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return true;
        };
        let size = ws.os_window.as_ref().map_or(size, |w| w.inner_size());
        let (_rows, cols) = self.grid_dims_for(wid, size);
        cols != ws.cols
    }

    /// Live font zoom (Cmd-+/Cmd--/Cmd-0): rebuild the [`Backend`] at `px`,
    /// re-grid each window and tell the PTY. A failed rebuild keeps the current
    /// size. No-op without a window.
    pub(crate) fn set_font_px(&mut self, px: f32) {
        let _ = self.set_font_px_with(px, Self::rebuild_backend);
    }

    /// Transactional core of [`Self::set_font_px`]. The rebuild is an injected
    /// closure so the rollback path is testable: on failure the App's font and
    /// per-window metrics stay paired with the live renderer.
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
        // A live zoom is an explicit size like `--font-px`: pin it so auto-scale
        // cannot revert it and `refresh_all_window_metrics` resolves at this px.
        self.font_px_explicit = true;
        // Re-resolve per-window metrics to the new px before `rebuild_backend`
        // re-grids, as config reload does; otherwise the re-grid divides by the
        // stale cell box and new glyphs overflow the old layout.
        self.refresh_all_window_metrics();
        if rebuild(self) {
            true
        } else {
            // The old renderer survives a failed rebuild; restore its matching
            // App and window metrics too.
            self.font_px = previous_px;
            self.font_px_explicit = previous_explicit;
            self.refresh_all_window_metrics();
            false
        }
    }

    /// Commit the reload render transaction only after the backend accepts the
    /// candidate font/geometry. Production passes [`Self::rebuild_backend`]; the
    /// seam lets tests inject a failure.
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

    /// The live glyph-affecting knobs, input to
    /// `Backend::pin_glyph_raster_knobs`. At launch they equal what `main_entry`
    /// handed the backend worker.
    pub(crate) fn glyph_raster_knobs(&self) -> GlyphRasterKnobs {
        GlyphRasterKnobs {
            text_shaping: self.text_shaping.clone(),
            font_thicken: self.font_thicken,
            stem_gamma: self.stem_gamma,
            font_hinting: self.font_hinting.clone(),
            line_height: self.render_knobs.line_height,
            adjust_baseline: self.render_knobs.adjust_baseline,
        }
    }

    /// Re-pin every memory-backed App-owned renderer setting onto a new or
    /// re-faced backend. Path-backed font generations are prepared and sealed
    /// on their own worker.
    pub(crate) fn pin_backend_render_config_core(&mut self) {
        // The glyph subset uses the same setter list the worker ran before its
        // warm, so each setter early-outs and the warm survives.
        let raster = self.glyph_raster_knobs();
        self.backend.pin_glyph_raster_knobs(&raster);
        self.backend.set_text_blending(self.text_blending);
        let subpixel = self.font_subpixel.clone();
        self.backend.set_font_subpixel(&subpixel);
        self.backend
            .set_minimum_contrast(self.render_knobs.minimum_contrast);
        self.backend
            .set_selection_fg(self.render_knobs.selection_fg);
        let (upos, uthick) = self.render_knobs.adjust_underline;
        self.backend.set_adjust_underline(upos, uthick);
        self.backend
            .set_underline_skip_descenders(self.render_knobs.underline_skip_descenders);
        // A combined opacity and font/geometry reload lands here, so end a
        // windowed macOS swapchain tap before a translucent frame (whose
        // compositor backdrop the raw recording lacks) can present.
        let _ = self.video_abort_translucent_swapchain(
            self.render_knobs.background_opacity,
            cfg!(target_os = "macos"),
        );
        self.backend
            .set_background_opacity(self.render_knobs.background_opacity);
        // `backend_kind_undecided`: a headless launch may still install the GPU
        // (`ensure_pixel_backend`), so do not print CPU-only diagnostics yet.
        if !self.backend.is_gpu() && !self.backend_kind_undecided() {
            if self.render_knobs.background_opacity < 1.0 {
                warn_background_opacity_unimplemented_once();
            }
            if self.render_knobs.background_material != BackgroundMaterial::None {
                warn_background_material_unimplemented_once();
            }
        }
    }

    /// Rebuild the [`Backend`] from the current `font_px` and `theme`, re-grid
    /// and repaint. Shared by font zoom ([`Self::set_font_px`]) and config reload
    /// ([`Self::reload_config`]). A failed rebuild keeps the current backend.
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
        // Preserve the padding and chrome headroom across the rebuild (a fresh
        // backend starts at 0); both are device-px constants independent of font.
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
            // The catalog worker sealed its generation, so the swap settles any
            // deferred headless seal debt; redeeming first would read fonts into a
            // renderer about to be replaced.
            self.settle_deferred_font_seal();
        } else {
            // `rebuild_from_admitted` needs resident bytes and refuses an unsealed
            // deferred headless generation, so redeem the seal first.
            self.redeem_deferred_font_seal();
            if let Err(error) = self
                .backend
                .rebuild_font_from_admitted(self.font_px, self.theme)
            {
                crate::logging::stderr_line!(
                    "aterm-gui: resident font generation rebuild failed: {error}"
                );
                return false;
            }
        }
        self.backend.set_pad(pad);
        self.backend.set_pad_top(pad_top);
        self.backend.set_head(head);
        // Re-apply every configured shaping/typography/render/font setting. In
        // particular line_height lands before the re-grid below.
        self.pin_backend_render_config_core();
        // Replacing WindowGpu destroys any swapchain/virtual tap; end the
        // recording while its ownership is still coherent.
        let _ = self.video_abort_backend_rebuild();
        // The atlas/face changed: reset both renderers' per-window caches and the
        // introspection scratch, and repaint. Swapchains and OS windows survive.
        self.reset_window_raster_caches();
        // Re-grid every OS window for the new cell metrics from its own
        // inner_size, refresh its resize increments and recorded raw size, then
        // repaint.
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
                // A font/theme rebuild is an external visual stimulus: couple the
                // recovery reset to its repaint edge until a real present or drop.
                ws.rearm_present(true);
            }
        }
        // The primary face may have changed (family/zoom reload): re-hand the
        // resolved face + bold sibling to the chrome rasterizer.
        if let Some(chrome) = prepared_chrome {
            crate::tray_raster::set_prepared_chrome_fonts(chrome);
        } else {
            self.sync_chrome_fonts();
        }
        // Re-apply window vibrancy after the rebuild re-pinned bg opacity
        // (idempotent; no-op when solid or off macOS).
        if self.backend.is_gpu() {
            self.apply_window_vibrancy();
        }
        true
    }

    /// Reset every per-window raster cache after the face or device changed:
    /// both renderers' content caches and the `RepaintKey` gate
    /// (`WindowState::invalidate_raster_caches`), a fresh `WindowGpu`
    /// (offscreen, dirty gate, scissor) that keeps the surface state (EDR
    /// headroom, reference-white scale, capture colour space), and the
    /// introspection scratch. Also used by `retry_attach_on_opaque_swapchain`,
    /// where the device itself was replaced and old-device textures must not
    /// survive.
    ///
    /// The CPU cache must go too: it keys on frame size, grid origin and font
    /// epoch, and a new face at the same cell box can repeat all three (a
    /// prepared generation seals to the boot epoch, `rebuild_from_admitted`
    /// restarts it), so it would gate-hit on the old glyphs.
    pub(crate) fn reset_window_raster_caches(&mut self) {
        self.introspect_gpu = aterm_gpu::WindowGpu::new();
        for ws in self.windows.values_mut() {
            ws.invalidate_raster_caches();
            if let Some(
                PresentTarget::Gpu { window_gpu, .. } | PresentTarget::Virtual { window_gpu },
            ) = &mut ws.present
            {
                // EDR headroom, reference-white scaling and the colour-space tag
                // are surface state, not caches.
                let edr = window_gpu.edr_max();
                let sdr_white_scale = window_gpu.sdr_white_scale();
                let capture_color_space = window_gpu.capture_color_space();
                *window_gpu = aterm_gpu::WindowGpu::new();
                window_gpu.set_edr_max(edr);
                window_gpu.set_sdr_white_scale(sdr_white_scale);
                window_gpu.set_capture_color_space(capture_color_space);
            }
            // Headless has no winit redraw edge: capture renders synchronously
            // and a Virtual recording has its own timer, so reopen the gate
            // directly. Windowed targets use the coupled helper later.
            if ws.os_window.is_none() {
                let _ = ws.present_retry.on_external_stimulus();
            }
        }
    }

    /// Hand the renderer's primary face bytes (and the discovered `-Bold`
    /// sibling) to [`crate::tray_raster`] so the Settings/About/Palette overlays
    /// use the user's terminal font; embedded DejaVu stays a per-char fallback.
    /// Called after every backend build.
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

    /// Route one renderer knob change to exactly one renderer call (pinned by the
    /// config round-trip test). Used at startup and on reload.
    /// `SelectionInactive` has no backend call: `redraw_window` folds it with
    /// the window's focus every frame.
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
                // The recording was admitted opaque; abort it before a translucent
                // raw layer can be appended.
                let _ = self.video_abort_translucent_swapchain(v, cfg!(target_os = "macos"));
                self.backend.set_background_opacity(v);
                // On the GPU backend, re-apply the window/layer opacity flip and
                // backdrop. The CPU surface is opaque, so it stays solid and warns
                // once.
                if self.backend.is_gpu() {
                    self.apply_window_vibrancy();
                } else if v < 1.0 && !self.backend_kind_undecided() {
                    warn_background_opacity_unimplemented_once();
                }
            }
            // The NSVisualEffectView backdrop follows the knobs: re-applied live on
            // the GPU backend (visible only when translucent); on CPU a non-`none`
            // material warns once.
            KnobChange::BackgroundMaterial(m) => {
                if self.backend.is_gpu() {
                    // Windows Mica/Acrylic: update the GPU renderer first so the
                    // redraw reconciles the swapchain composite mode (Opaque ⇄
                    // PreMultiplied on a DComp visual). Material → none must go
                    // opaque; none → material re-engages only if the instance was
                    // built as a visual. A plain HWND swapchain stays DWM-chrome
                    // only, which `window_set_vibrancy` diagnoses.
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

    /// Apply the `NSVisualEffectView` backdrop and window/layer opacity to every
    /// attached window from the resolved knobs, at attach and on a
    /// `background_opacity` / `background_material` reload. The theme `bg`
    /// restores the opaque titlebar fill when translucency turns off. No-op off
    /// macOS. Each window repaints so the swapchain composite mode reconciles.
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

    /// Live theme-only swap, the cheap sibling of [`Self::rebuild_backend`], used
    /// by [`Self::reload_config`] and [`Self::sync_app_theme_to_appearance`].
    /// Glyphs are coloured at draw time, so the new [`Theme`] goes onto the live
    /// backend without re-parsing fonts, dropping the atlas or re-gridding.
    ///
    /// Theme is not cell content, so content-diffing caches must be dropped: the
    /// strip-row cache (and macOS titlebar bg), per-window CPU pixel cache, GPU
    /// prev-frame, `RepaintKey` gate and introspection scratch.
    pub(crate) fn apply_theme_live(&mut self, new_theme: Theme) {
        // Assign FIRST so later rebuilds/zooms bake the new theme.
        self.theme = new_theme;
        // Matrix rain derives its ramp from the theme; an OS appearance flip
        // skips `reload_config`, so mark it dirty here.
        self.rain_dirty = true;
        match self.backend.ready_mut() {
            Backend::Cpu(r) => r.set_theme(new_theme),
            Backend::Gpu(g) => g.set_theme(new_theme),
        }
        self.introspect_gpu.invalidate_present();
        // Linux CSD: the header's light/dark variant follows this theme under
        // `window_theme = auto`, so re-push it. macOS/Windows chrome already
        // tracks via `window_set_background_color` / DWM.
        #[cfg(target_os = "linux")]
        let linux_chrome_theme = self.chrome_theme_for_apprt();
        let bg = new_theme.bg;
        let apprt = &self.apprt;
        let toolbars = &self._toolbars;
        let strip_dark = crate::tab_bar::theme_is_dark(bg);
        for (wid, ws) in self.windows.iter_mut() {
            ws.last_strip_fp = None;
            ws.invalidate_raster_caches();
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
        // Also reached directly on OS appearance flips; refresh the font fork so
        // dark-nudge and theme-dependent raster state keep up.
        self.sync_chrome_fonts();
    }

    /// Handle `WindowEvent::ScaleFactorChanged`: re-derive the auto-scaled font
    /// (`round(FONT_PX·scale)`) and pad for the new DPI, like
    /// [`Self::attach_os_window`] does at creation.
    ///
    /// Only for the auto font (no `--font-px` / `config.font_px`) and no forced
    /// `--scale`. A no-op when nothing would change.
    ///
    /// All windows share one backend, so the new scale is stored on this
    /// window's [`WindowState`] and applied lazily when it draws
    /// ([`Self::apply_window_scale`]); the `Resized` winit emits afterwards
    /// re-grids it.
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
            // Refresh this window's metric record from its own scale (the per-window
            // source of truth; the backend is re-tuned lazily). The head re-derives
            // from the stored points band without an AppKit round-trip.
            //
            // Attached windows only: a headless window's record holds the sealed
            // boot band (`--scale` pad, `$ATERM_HEADROOM_PX`) and must not be
            // overwritten.
            if ws.os_window.is_some() {
                ws.metrics = crate::MetricsView::for_scale_padded(scale, pad_l, pad_top_l);
                ws.metrics.head = (ws.head_pts * scale).round() as usize;
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        // Re-derive the whole-cell resize increments from the refreshed metrics:
        // the WM stores them verbatim, so a per-monitor DPI move would otherwise
        // keep the old cell box and leave uneven padding bands. Headless windows
        // are skipped via `os_window`; macOS also re-applies them at
        // `windowWillStartLiveResize`.
        let (cw, ch) = self.win_cell_size(wid);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.set_resize_increments(Some(PhysicalSize::new(cw as u32, ch as u32)));
            // Keep the increment BASE on the whole-cell lattice at the new
            // DPI too (fixwave5) — see `whole_cell_min_size`.
            #[cfg(target_os = "linux")]
            w.set_min_inner_size(Some(self.whole_cell_min_size(wid)));
        }
    }

    /// Re-tune the shared backend's font size and pad to window `wid`'s scale
    /// before it composes (top of `redraw_window`), giving per-window DPI on one
    /// renderer. A no-op when already tuned; only redraws that alternate across
    /// DPIs switch. Honors the auto-font / no-force-scale gate like
    /// [`Self::attach_os_window`].
    pub(crate) fn apply_window_scale(&mut self, wid: WindowId) {
        // The cell pixel size reported over DEC 1016 and used for OSC 1337 images
        // depends on the resolved metrics however the font was pinned, so sync it
        // before the pinned-font early return. This also corrects the boot session
        // spawned before the backend joined (`cell_px: None`). Memoized per window.
        self.sync_cell_pixel_size(wid);
        // Pad and head are per-window on the shared backend however the font is
        // pinned, so re-tune them before the pinned-font early return.
        // Attached windows only: a headless window's record holds scale-1
        // defaults, and applying them would wipe the boot `--scale` pad and
        // `ATERM_HEADROOM_PX` band that headless recordings are sized by.
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
        // This window's resolved metrics are the authority. Select them with the
        // light `activate_px`, which keeps other sizes' glyph atlases resident,
        // instead of a full rebuild.
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
        // Track the drawing window's size so `self.font_px` reads and Cmd-0 use
        // this window's scaled default.
        self.default_font_px = m.font_px;
        self.font_px = m.font_px;
        self.backend.activate_px(m.font_px);
        // If the grid no longer matches the new cell box (`grid_dims_for`), re-grid
        // so the frame never exceeds the window. Gated on a real mismatch: an
        // unconditional `on_resize` would lock every session and rewrite shared
        // `cell_pixel_size` on each alternating multi-DPI redraw.
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

    /// Headless boot seal: copy the applied boot metrics (`--scale` pad,
    /// `$ATERM_HEADROOM_PX` head, resolved font) into every never-attached
    /// window's record, so [`Self::apply_window_scale`]'s auto-font arm does not
    /// re-tune from scale-1 defaults on the first recorded redraw.
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

    /// Toggle borderless full screen on the current monitor (View ▸ Enter Full
    /// Screen). Returns the reason when no OS window exists, so the menu seam can
    /// report the refusal; key-path callers discard it.
    pub(crate) fn toggle_fullscreen(&self) -> Result<(), &'static str> {
        let Some(w) = self.front().and_then(|ws| ws.os_window.as_ref()) else {
            return Err("no window is on screen to full-screen");
        };
        let next = match w.fullscreen() {
            Some(_) => None,
            None => Some(winit::window::Fullscreen::Borderless(None)),
        };
        w.set_fullscreen(next);
        Ok(())
    }

    /// Path-referenced half of a byte-equal config reload: the parsed
    /// `aterm.toml` is unchanged, but files it names (trail-pack manifests,
    /// sparkle lexicon, toy packs) may have changed. Compare fresh fingerprints
    /// with [`App::path_feed_fps`]:
    ///
    /// * unchanged: a full runtime no-op;
    /// * changed: arm `sparkle_dirty` to activate the compiled worker bundle,
    ///   hard-reset word decorations only when the deco feed changed, and
    ///   request repaints. Nothing else can have changed.
    fn refresh_path_feeds(&mut self, fresh: PathFeedFps) {
        if fresh == self.path_feed_fps {
            return;
        }
        let previous = std::mem::replace(&mut self.path_feed_fps, fresh);
        self.sparkle_dirty = true;
        if fresh.deco != previous.deco {
            // Retire only word-owned episodes; cursor companions share the atlas
            // but have their own lifecycle.
            for ws in self.windows.values_mut() {
                ws.fx.decos_mut().hard_reset_words();
            }
        }
        for ws in self.windows.values() {
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Publish one admitted asset generation to the App and every window; the
    /// only post-construction writer of [`App::config_assets`]. Callers pass the
    /// `Arc` from a versioned config snapshot; this only clones `Arc`s and
    /// installs scalar effects state (all I/O happened earlier).
    ///
    /// Pointer identity decides: a new catalog may carry a changed kitty file or
    /// theme directory under byte-identical `aterm.toml`, and must reach every
    /// window. Re-publishing the same `Arc` is a complete no-op.
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

    /// Publish a theme-directory generation from the background watcher: swap
    /// the catalog, reapply the engine palette and chrome, and fan the snapshot
    /// to Settings and Manual. No filesystem access.
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
        if !theme_eq(theme, self.theme) {
            self.apply_theme_live(theme);
        }
        self.publish_native_config_snapshot(&snapshot);

        // A theme file arriving or leaving changes whether the configured theme
        // draws, so rebuild the unaccepted-values row, unless the snapshot holds
        // an unapplied config edit whose apply will rebuild it.
        if *snapshot.config == self.config
            && theme_misses(&self.config, &before.assets.themes)
                != theme_misses(&self.config, &snapshot.assets.themes)
        {
            use crate::message_reporters::ConfigFamily;
            let mut warns = crate::message_reporters::ConfigWarnings::default();
            collect_harness_notices(&mut warns, &self.config);
            warns.extend(
                ConfigFamily::UnacceptedValues,
                font_px_load_notice(&self.config),
            );
            warns.extend(
                ConfigFamily::UnacceptedValues,
                theme_load_notices(&self.config, &snapshot.assets.themes),
            );
            let unaccepted = unaccepted_value_notices(&snapshot.text, &warns.told());
            warns.extend(ConfigFamily::UnacceptedValues, unaccepted);
            for w in warns.sentences() {
                crate::logging::stderr_line!("aterm-gui: {w}");
            }
            self.replace_config_messages_keyed(
                &[ConfigFamily::UnacceptedValues.key()],
                warns.into_messages(),
            );
        }
    }

    /// Apply a worker-prepared config observation of `aterm.toml`. Parsing,
    /// asset loading and font preparation happen off the event loop; this
    /// installs the generation into every live session.
    ///
    /// Malformed, partial or unreadable input never reaches this seam and cannot
    /// replace the running config; malformed UTF-8 TOML still reaches Manual for
    /// diagnostics.
    ///
    /// Font size uses [`resolve_font_px`] (`--font-px > config > default`). GPU
    /// is a launch-time decision; only font size, renderer theme and engine
    /// `TerminalConfig` (via `Terminal::apply_config`) are re-applied.
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
            // A theme generation can overtake the config worker; Trail/kitty and
            // Sparkle consumers do not depend on it, so rebase the Arc in memory.
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
                        disk_faults: completion.disk_faults,
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
        // Re-read and strictly re-parse; malformed, partial or unreadable files
        // are rejected and the previous config stays.
        let Some(path) = config_path() else { return };
        // One stable read feeds Manual, strict parsing and the versioned service,
        // so every consumer sees the same edit. Malformed UTF-8 bytes still reach
        // Manual; the running Config changes only after a strict parse.
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

    /// Apply the exact generation sampled by the config host. Watchers use this
    /// rather than reopening the path, so acknowledgement and application refer
    /// to one observation.
    pub(crate) fn reload_config_observation(
        &mut self,
        observation: crate::native_config_service::ConfigDiskObservation,
    ) {
        self.prepare_native_config_external_observation(observation);
    }

    /// Worker-prepared twin of [`Self::reload_config_observation`]. A theme that
    /// overtook persistence is rebased in memory and guarded by the
    /// theme-generation ticket.
    pub(crate) fn reload_prepared_config_observation(
        &mut self,
        prepared: crate::native_config_service::PreparedConfigObservation,
    ) {
        if let Err(error) = self.refresh_open_config_editor_observation(&prepared.observation) {
            aterm_log::warn!("config reload: Manual refresh needs attention ({error})");
        }
        self.request_font_catalog_generation(prepared);
    }

    /// Admit a config generation. For `HarnessPolicyOneWriter` this is
    /// `AdmitOn`/`AdmitOff`; it writes no policy (the watcher is its one writer).
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "HarnessPolicyOneWriter",
            action = "AdmitOn",
            project = "aterm_gui::config_watcher::tests::project_harness_writer"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "HarnessPolicyOneWriter",
            action = "AdmitOff",
            project = "aterm_gui::config_watcher::tests::project_harness_writer"
        )
    )]
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

    /// Admit a deferred generation after a reconciliation worker sampled the same
    /// baseline, which clears the write fence. Going back through
    /// [`Self::apply_prepared_config_generation`] would hit the closed fence and
    /// defer forever.
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
            mut config,
            values,
            disk_faults,
            assets: prepared_assets,
            path_feed_fps: fresh_feeds,
            sparkle: prepared_sparkle,
            fonts: mut prepared_fonts,
            warnings: mut font_prepare_warnings,
        } = generation;
        // The lane never writes the supervisor's policy: the watcher's read is its
        // one writer (`config_watcher::observed_harness`,
        // `Wake::HarnessPolicyObserved`). Workers finish in arbitrary order, so a
        // generation keeps the running policy, and the dedupe compares with it.
        config.harness = self.harness_policy.get().clone();
        let admitted_baseline = observation.baseline.clone();
        let config_snapshot = match self.native_config_service.synchronize_observation_prepared(
            observation,
            config.clone(),
            values,
            disk_faults,
            prepared_assets,
        ) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.native_config_service.mark_reconciliation_required();
                self.reject_config_watch_admission_for(
                    &admitted_baseline,
                    crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
                );
                // The fence is re-armed with no worker sample scheduled, so reject
                // queued semantic requests now rather than leaving them until some
                // unrelated event pumps. This arm is reached from a reconciliation
                // completion, which a queued control caller may be waiting on.
                self.reject_pending_native_config(&format!(
                    "config generation could not be admitted: {error}"
                ));
                aterm_log::warn!("native config service rejected watcher snapshot: {error}");
                return;
            }
        };
        self.prepared_sparkle = prepared_sparkle;

        // Semantic dedupe: re-applying an identical config is side-effect churn
        // (engine re-diffs, word-deco resets, popup cancels, backend probes), so an
        // equal parse stops here (field-wise `PartialEq`; the native snapshot has
        // already seen the new text).
        //
        // Path-referenced files (trail-pack manifests, sparkle lexicon, toy packs)
        // are also applied state, and a feed-only edit arrives as a byte-equal
        // observation, so `refresh_path_feeds` still checks their fingerprints.
        if config == self.config && prepared_fonts.is_none() {
            // Comments, formatting and explicit-default edits still advance the
            // service revision; publish the snapshot so the next Settings patch
            // starts from the current base.
            self.publish_config_assets(std::sync::Arc::clone(&config_snapshot.assets));
            self.publish_native_config_snapshot(&config_snapshot);
            self.refresh_path_feeds(fresh_feeds);
            self.finish_native_config_external_admission(&admitted_baseline);
            // Text-derived diagnostics still run: a misspelled key leaves no trace
            // in `Config`, so the parse is equal but the notice is new. Posting a
            // possibly empty set also clears a fixed typo. Only the families
            // derived from text (and a now-answered launch load failure) are
            // replaced; keybinding, font and trail rows from the last full reload
            // stay.
            let mut warns = crate::message_reporters::ConfigWarnings::default();
            collect_key_notices(&mut warns, &config_snapshot.text);
            collect_harness_notices(&mut warns, &config);
            // The unaccepted-values row is rebuilt from scratch, so restate an
            // ignored `font_px` or it vanishes on an unrelated edit.
            warns.extend(
                crate::message_reporters::ConfigFamily::UnacceptedValues,
                font_px_load_notice(&config),
            );
            warns.extend(
                crate::message_reporters::ConfigFamily::UnacceptedValues,
                theme_load_notices(&config, &config_snapshot.assets.themes),
            );
            let unaccepted = unaccepted_value_notices(&config_snapshot.text, &warns.told());
            warns.extend(
                crate::message_reporters::ConfigFamily::UnacceptedValues,
                unaccepted,
            );
            for w in warns.sentences() {
                crate::logging::stderr_line!("aterm-gui: {w}");
            }
            self.replace_config_messages_keyed(
                &[
                    crate::message_reporters::ConfigFamily::IgnoredKeys.key(),
                    crate::message_reporters::ConfigFamily::RetiredKeys.key(),
                    crate::message_reporters::ConfigFamily::UnacceptedValues.key(),
                    crate::message_reporters::KEY_LAUNCH_LOAD,
                ],
                warns.into_messages(),
            );
            return;
        }

        // Renderer construction is the one fallible step; snapshot its authority
        // first so only this slice rolls back if the font build fails.
        let reload_render_before = ReloadRenderSnapshot::capture(self);

        // Restart-only keys (`columns`/`lines`, `gpu`): diff against the old config
        // before it is overwritten so the change is reported, not silently ignored.
        let restart_notices = restart_notices(&self.config, &config);

        // A changed resolved `window_padding`/`window_padding_top` must re-grid
        // every attached window (pad feeds `grid_dims_for`); diffed against the old
        // config before it is overwritten.
        let old_padding = (
            self.config.window_padding_or_default(),
            self.config.window_padding_top_or_default(),
        );

        // Whether the word-decoration inputs (`[sparkle_words]`, lexicon, theme)
        // changed; only then is the per-window `hard_reset` warranted.
        let sparkle_feed_changed = deco_feed_changed(
            &self.config,
            &config,
            fresh_feeds.deco,
            self.path_feed_fps.deco,
        );
        self.path_feed_fps = fresh_feeds;

        // Typing-sound audition: a changed voice plays one keystroke
        // (`typing_sound_to_audition_on_swap`; an in-app pick already auditioned
        // and latched it, so its reload is silent). Decided before the swap.
        let audition_voice = self.typing_sound_to_audition_on_swap(&config);
        // A changed privacy probe gate is applied before the swap.
        if self.config.privacy_probe_gate() != config.privacy_probe_gate() {
            // Retire an in-flight answer even if an off/on pair of reloads
            // arrives before any card/status read sees the intermediate state.
            self.consent.invalidate();
        }
        // The Claude footer's `[claude_footer]` switches change what every
        // window's Claude Code panes show — an idle background window too,
        // which no other branch below repaints.
        let claude_footer_changed = self.config.claude_footer != config.claude_footer;
        self.config = config.clone();
        if claude_footer_changed {
            self.redraw_every_window();
        }
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
        // Pin `active_tab_color` on every reload: a tab-color-only edit triggers
        // no rebuild branch below. The setter is an idempotent atomic compare.
        {
            let override_rgb = self.config.active_tab_color_rgb();
            for handle in self._toolbars.values() {
                crate::toolbar::set_active_tab_color(handle, override_rgb);
            }
            // The strip-row cache fingerprint and the RepaintKey do not include the
            // override, so drop the cache and `last_present` to repaint.
            for ws in self.windows.values_mut() {
                ws.last_strip_fp = None;
                ws.last_present = None;
            }
        }
        // `motion = "reduced"` is a live accessibility edge: land every retained
        // glide on its target now rather than at the next input or tick.
        self.settle_reduced_scroll_motion(std::time::Instant::now());
        // Serious mode is a projection, not a rewrite of effect settings; apply
        // its edge right after publishing so a disable restores this generation's
        // values.
        self.apply_serious_mode(config.serious_mode_or_default());
        self.publish_config_assets(std::sync::Arc::clone(&config_snapshot.assets));
        self.publish_native_config_snapshot(&config_snapshot);
        // Smart-title settings apply live: revoke queued provider work and
        // repaint tab/window composition.
        self.reconfigure_title_summaries();
        // Tab status policy is copied into each classifier at construction, so
        // push it to existing sessions and the event loop's wait deadline.
        self.reconfigure_session_status();
        // `explain_heavy_load` is live: `false` parks the strain engine,
        // withdraws its row and drops the probe thread; nothing is armed after.
        self.reconfigure_strain();
        // `desktop_alerts` is live: the notification thread reads the switch
        // per message, so aterm's own notices stop (or resume) with this store.
        self.apply_desktop_alerts(config.desktop_alerts_or_default());
        // Drop per-keystroke caches (predictive_echo, cursor_trail_style); they
        // re-resolve on the next keystroke.
        self.predict_mode_cache = None;
        self.kitty_cursor_enabled_cache = None;
        // Sparkle words: mark the App cache stale and flush each window's cached
        // occurrences so a byte-idle grid does not keep a stale scan.
        self.sparkle_dirty = true;
        // Matrix rain: mark the resolve stale (pushed into live engines via
        // `set_config` before the next frame) and refresh an open palette's
        // checkmark.
        self.rain_dirty = true;
        // [presence]: adopt the band/rim bits and re-project every window
        // (`app_fabric_menu.rs`).
        self.adopt_presence_config();
        self.palette_refresh_live();
        // Re-publish the Settings ▸ Packages projection (memory-only) so consent
        // changes show immediately.
        self.publish_native_packages_state();
        // Palette authority changed: retire only a resident pet's contrast sample,
        // keeping position, action and breed.
        for ws in self.windows.values_mut() {
            ws.fx.companion_mut().invalidate_colors();
        }
        // A Sparkle Words reload retires word episodes and done marks but keeps
        // the cursor companion's placement and sprite caches.
        if sparkle_feed_changed {
            for ws in self.windows.values_mut() {
                ws.fx.decos_mut().hard_reset_words();
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
        // `allow_kitty_file_transfer` is a session-factory setting; refresh it so
        // new tabs pick it up (existing sessions keep their spawn-time policy).
        self.session_factory.allow_kitty_file_transfer =
            config.allow_kitty_file_transfer.unwrap_or(false);
        // Shell policy is spawn-time: keep a launch `--shell` override, else
        // publish the reloaded config so the next tab uses it.
        self.session_factory.shell_override = resolve_shell_override(&config);
        self.session_factory
            .shell_args
            .clone_from(&config.shell_args);
        // `temporal_recording` is a session-factory opt-in; refresh it for new
        // tabs.
        self.session_factory.temporal_recording = config.temporal_recording.unwrap_or(false);
        // Re-push the global search depth cap (⌘F and the `search` verb); it is
        // part of the index cache key, so the next search rebuilds at the new depth.
        crate::control::set_search_max_lines(search_index_depth(&config));

        // Input config applies live on the next keypress. Keybindings use
        // `resolved_warn` so platform defaults stay overlaid, as in `App::new`
        // (on Linux they seed copy/paste/new-tab). Clearing `[keybindings]` or
        // `option_as_meta` restores the defaults.
        // Config warnings are collected per family
        // (`message_reporters::ConfigWarnings`) and replace the band's
        // `config.*` rows as a set at the end of this reload.
        use crate::message_reporters::ConfigFamily;
        let mut warns = crate::message_reporters::ConfigWarnings::default();
        let (keybindings, kb_warns) =
            keybinding::Keybindings::resolved_warn(config.keybindings.as_ref());
        let (key_sequences, ks_warns) =
            keybinding::KeySequences::from_config_warn(config.key_sequences.as_ref());
        self.keybindings = keybindings;
        self.key_sequences = key_sequences;
        warns.extend(ConfigFamily::Keybindings, kb_warns);
        warns.extend(ConfigFamily::Keybindings, ks_warns);
        // Restart-only edits (columns/lines) ride the SAME band as dropped-rule
        // warnings — both are "your edit didn't fully take effect" messages.
        warns.extend(ConfigFamily::Restart, restart_notices);
        // Ignored keys too: a misspelled key has no trace in the parsed config.
        // Retired spellings are recorded, not shown (`collect_key_notices`).
        collect_key_notices(&mut warns, &config_snapshot.text);
        // …and so does a refused Secure Keyboard Entry transition (see the
        // apply site above): same class exactly — an edit that did not take.
        if let Some(status) = secure_input_refusal {
            warns.push(
                ConfigFamily::SecureKeyboard,
                format!(
                    "secure_keyboard_entry: the OS refused the change (OSStatus {status}) \u{2014} \
                     Secure Keyboard Entry is NOT {}",
                    if self.config.secure_keyboard_entry_or_default() {
                        "on"
                    } else {
                        "off"
                    }
                ),
            );
        }
        // An unresolvable `font_family` warns. The catalog worker's answer is
        // authoritative, `None` included: a rejected family comes back as
        // `previous_family` (keep the working face), while a cleared family comes
        // back as `None` (built-ins). Fall back to the current family only when no
        // font work ran this reload.
        let requested_effective_family = match prepared_fonts.as_ref() {
            Some(prepared) => prepared.family.clone(),
            None => self.font_family.clone(),
        };
        // Font work ran and admitted the family, so the launch-time `font_family`
        // verdict is answered too.
        let family_admitted = prepared_fonts.is_some()
            && !font_prepare_warnings
                .iter()
                .any(|w| w.starts_with(crate::native_font_catalog::FAMILY_REFUSED));
        warns.extend(ConfigFamily::Fonts, font_prepare_warnings.drain(..));
        // An unrecognized `cursor_trail_style` draws the default; warn on the band.
        warns.extend(
            ConfigFamily::CursorTrail,
            config_snapshot
                .assets
                .trail_packs
                .diagnostics
                .iter()
                .map(|warning| format!("config {warning}")),
        );
        if let Some(w) = config.cursor_trail_style_warning(&config_snapshot.assets.trail_packs) {
            warns.push(ConfigFamily::CursorTrail, format!("config {w}"));
        }
        if let Some(reason) = config_snapshot.assets.kitty_sprite.diagnostic() {
            let source = config_snapshot
                .assets
                .kitty_sprite
                .source_id()
                .unwrap_or("configured source");
            warns.push(
                ConfigFamily::Assets,
                format!("config cursor_nyan_sprite {source:?} invalid: {reason}"),
            );
        }
        // An unadmittable wallpaper silently renders as no backdrop — surface
        // the decode verdict on the same band (the kitty-sprite rule).
        if let Some(reason) = config_snapshot.assets.wallpaper.diagnostic() {
            let source = config_snapshot
                .assets
                .wallpaper
                .source_id()
                .unwrap_or("configured source");
            warns.push(
                ConfigFamily::Assets,
                format!("config wallpaper {source:?} invalid: {reason}"),
            );
        }
        // Re-resolve the per-style/fallback font keys (families → paths), warning
        // for unresolvable entries; the diff below decides whether the backend is
        // touched.
        let new_font_config = prepared_fonts
            .as_ref()
            .map(|prepared| prepared.config.clone())
            .unwrap_or_else(|| self.font_config.clone());
        // The supervisor's refused `[harness]` values (a known key with a value it
        // cannot take keeps its default).
        collect_harness_notices(&mut warns, &config);
        // A `font_px` the resolver ignores says so here, like it does at launch,
        // and so does a theme the window draws Default in place of.
        warns.extend(ConfigFamily::UnacceptedValues, font_px_load_notice(&config));
        warns.extend(
            ConfigFamily::UnacceptedValues,
            theme_load_notices(&config, &config_snapshot.assets.themes),
        );
        let unaccepted = unaccepted_value_notices(&config_snapshot.text, &warns.told());
        warns.extend(ConfigFamily::UnacceptedValues, unaccepted);
        for w in warns.sentences() {
            crate::logging::stderr_line!("aterm-gui: {w}");
        }
        // Post warnings, one row per family. A fresh (possibly empty) set first
        // resolves every live `config.` row, so fixed issues clear; windows
        // repaint.
        self.replace_config_messages_after_font_work(warns.into_messages(), family_admitted);
        self.option_as_meta = config.option_as_meta_or_default();
        self.confirm_multiline_paste = config.confirm_multiline_paste_or_default();
        // Copy-on-select applies from the next selection.
        self.copy_on_select = config.copy_on_select_or_default();

        // Tab-strip rows are window chrome: a change re-splits the window between the
        // strip and the terminal, so re-grid (like a resize) if it changed.
        let new_strip = resolve_tab_strip_rows(&config);
        if new_strip != self.tab_strip_rows {
            // A strip rise raises every window's chrome rows and is noted, so no
            // presence fold or band shrink re-grids inside the settle. A fall
            // (a Settings edit) is immediate.
            if aterm_messages::chrome::is_rise(self.tab_strip_rows, new_strip) {
                self.note_chrome_rise_everywhere(std::time::Instant::now());
            }
            self.tab_strip_rows = new_strip;
            self.regrid_for_chrome_rows();
        }

        // Re-apply `window_theme` live and cache it for windows attached later.
        // Light/Dark are idempotent; Auto clears the forced appearance.
        let new_window_theme = config.window_theme_or_default();
        if new_window_theme != self.window_theme {
            self.window_theme = new_window_theme;
            // Chrome painters resolve from this field too (Linux
            // `chrome_palette_theme`), and the strip row cache fingerprint has no
            // theme term, so drop it and the present caches as `apply_theme_live`
            // does. Native pages re-lower via the compile stamp.
            for ws in self.windows.values_mut() {
                ws.last_strip_fp = None;
                ws.invalidate_raster_caches();
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        {
            // macOS/Windows get the raw policy (Auto follows the OS); on Linux Auto
            // maps onto the terminal theme's darkness (`chrome_theme_for_apprt`).
            let chrome_theme = self.chrome_theme_for_apprt();
            let apprt = &self.apprt;
            for ws in self.windows.values() {
                if let Some(w) = ws.os_window.as_ref() {
                    apprt.window_set_appearance(w, chrome_theme);
                }
            }
        }

        // Re-tag each GPU window's CAMetalLayer on a `window_colorspace` change
        // and cache it for later windows. Uses the attach precedence
        // (`resolve_surface_colorspace`): an HDR (Rgba16Float) window keeps its
        // extended-linear tag. No-op off macOS or on the CPU path.
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
                // Update capture's source metadata with the compositor tag so the
                // next PNG is labelled correctly; a failed retag keeps the old
                // metadata.
                let previous_capture_space = window_gpu.capture_color_space();
                let capture_space = crate::platform::capture_space_after_surface_tag(
                    effective_colorspace,
                    previous_capture_space,
                );
                window_gpu.set_capture_color_space(capture_space);
            }
        }

        // `hdr_glow` applies from the next present (the plan reads it per frame;
        // safe on an f16 swapchain). Turning it on also admits an f16-capable
        // Windows SDR surface to the throttled output-HDR probe (hdr_gate).
        // Shimmer reload is resolved before the backend borrow and skipped while
        // the load-shed latch holds it off; the latch restores it on recovery.
        let shimmer_reload = if self.serious_mode_enabled() {
            Some(false)
        } else if self.load_shed_active() {
            None
        } else {
            Some(config.cursor_fire_shimmer_or_default())
        };
        // Bloom on/off follows the shimmer rule under the shed latch; strength and
        // radius always re-push (the latch gates the pass, not its tuning).
        let bloom_reload = if self.serious_mode_enabled() {
            Some(false)
        } else if self.load_shed_active() {
            None
        } else {
            Some(config.cursor_trail_bloom_or_default())
        };
        // The radius is per style, read from this generation's presentation
        // before the backend borrow.
        let bloom_radius = config.cursor_trail_bloom_radius_or_default(self.trail_presentation());
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
                bloom_radius,
            );
            // Effect-pipeline warm-up. Effect pipelines are built lazily by
            // `encode_frame`, so the first frame drawing a newly enabled effect
            // would pay its compile inline (visible for the fire pipelines). A
            // config apply is where a person enables effects, so warm here, several
            // frames ahead; the launch path (`pin_gpu_effect_config`) does not warm.
            // Idempotent.
            //
            // Gated on the trail master, the only producer of the pipelines with a
            // visible build cost (`fire_add`, `fire_over`, `rain_glow`,
            // `rain_glow_over`); the rest are sub-frame and handled on demand.
            // Correctness never depends on this.
            if config.warms_effect_pipelines() {
                g.warm_effect_pipelines();
            }
        }

        // Re-fold the focus-boost want-set now so a `focus_boost` flip applies to
        // an already-focused window immediately.
        self.recompute_focus_boost();

        // Renderer theme, font size and family: rebuild only when something the
        // backend bakes in changed, so a comment edit costs nothing visible.
        let new_theme =
            config.theme_for_with_assets(self.os_appearance, &config_snapshot.assets.themes);
        // Re-derive the auto font with the same HiDPI logic as attach and scale
        // changes, so an unrelated edit does not shrink it to the 12px base. An
        // explicit font is honored and re-pins `font_px_explicit`.
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
        // Re-apply the font only when the derived default changed, so a live zoom
        // survives unrelated edits and Cmd-0 resets to the current default.
        let default_changed = (new_default_font_px - self.default_font_px).abs() >= 0.5;
        self.default_font_px = new_default_font_px;
        let new_font_px = if default_changed {
            new_default_font_px
        } else {
            self.font_px
        };
        // A surviving live zoom keeps `font_px_explicit` (see `reload_font_pin`).
        self.font_px_explicit =
            reload_font_pin(font_explicit_now, new_font_px, new_default_font_px);
        // The renderer bakes the theme in, so any change needs a backend rebuild.
        let theme_changed = !theme_eq(new_theme, self.theme);
        let font_changed = (new_font_px - self.font_px).abs() >= 0.5;
        // Effective family keeps `--font` > config > platform default, via the
        // same `effective_font_family` as startup and `--show-config`. A rejected
        // family keeps the last admitted face; the failure is reported above.
        let effective_family = requested_effective_family;
        let family_changed = effective_family != self.font_family;
        // Text shaping: update the source of truth before a possible rebuild; if
        // none is needed it is pushed directly below.
        let new_shaping = config.text_shaping();
        let shaping_changed = new_shaping != self.text_shaping;
        self.text_shaping = new_shaping;
        // Typography knobs (`text_blending`, `font_thicken`, `stem_gamma`,
        // `font_hinting`) change glyph appearance but not advances, so a
        // knob-only edit rides the shaping branch (backend push plus present-cache
        // invalidation).
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
        // Renderer knobs: one KnobChange per changed key. `line_height` changes
        // cell geometry and takes the full rebuild path; the rest are
        // appearance-only pushes.
        let new_knobs = RenderKnobs::from_config(&config);
        let knob_changes = self.render_knobs.diff(&new_knobs);
        self.render_knobs = new_knobs;
        // Font config: update the source of truth before installing the
        // renderer generation the catalog worker prepared.
        let font_cfg_changed = new_font_config != self.font_config;
        self.font_config = new_font_config;
        // Variable-font requests: a change re-instantiates the primary and can
        // move cell geometry, so it takes the full rebuild path. Malformed-entry
        // warnings were shown at startup; a reload re-parses silently.
        let (new_variations, _vf_warns) = config.font_variation_requests();
        let new_dark_nudge = config.font_weight_dark_nudge_or_default();
        let variations_changed = new_variations != self.font_variations
            || (new_dark_nudge - self.font_weight_dark_nudge).abs() > f32::EPSILON;
        self.font_variations = new_variations;
        self.font_weight_dark_nudge = new_dark_nudge;
        let geometry_knob_changed = knob_changes
            .iter()
            .any(|c| matches!(c, KnobChange::LineHeight(_)));
        // Theme-only edits take the live fast path; a rebuild is only for font
        // px/family, cell geometry or variable-font changes.
        let prepared_font_refresh = prepared_fonts.is_some();
        let backend_rebuild_needed = font_changed
            || family_changed
            || geometry_knob_changed
            || variations_changed
            || font_cfg_changed
            || prepared_font_refresh;
        // Commit the px and re-resolve every window's metrics before any rebuild
        // re-grids, since `on_resize` divides by `ws.metrics` (as in `set_font_px`).
        self.font_px = new_font_px;
        self.refresh_all_window_metrics();
        // Live-apply a padding edit: the refresh above updated each attached
        // window's pad, but the grid still has the old cell count. When the
        // padding changed without a font/geometry rebuild, re-grid each sized
        // window and redraw everywhere (`apply_window_scale` pushes the pads). A
        // rebuild handles it itself: success re-grids, failure restores the prior
        // metrics.
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
            // commit, so a rejected family cannot leave the titlebar out of step.
            if rebuild_succeeded && theme_changed {
                let bg = self.theme.bg;
                let apprt = &self.apprt;
                let toolbars = &self._toolbars;
                let strip_dark = crate::tab_bar::theme_is_dark(bg);
                for (wid, ws) in self.windows.iter_mut() {
                    ws.last_strip_fp = None;
                    // Keep the seamless titlebar colour in step with the new
                    // terminal bg (no-op off macOS).
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
            // Colour-only edit: the face and atlas stay valid, so push the theme
            // onto the live backend instead of a full rebuild. Shaping, typography
            // and render knobs changed in the same save apply live here too;
            // `apply_theme_live` handles cache invalidation.
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
            // Shaping/typography-only edit: push settings onto the live backend.
            // The setters drop shared atlas/glyph caches as needed; also drop each
            // window's present cache (CPU pixel cache, GPU prev-frame) and the
            // RepaintKey gate, since content-diffing caches would show old glyphs.
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
                ws.invalidate_raster_caches();
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
        // The live fast path skips `rebuild_backend`'s `sync_chrome_fonts`, so
        // refresh the font fork after every backend setter has landed.
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

    /// Interior padding (device px per edge) at `scale` under the live config:
    /// [`crate::logical_to_device_px`] over [`Config::window_padding_or_default`].
    /// Config-aware twin of [`crate::pad_for_scale`]; equal when unset.
    pub(crate) fn cfg_pad_for_scale(&self, scale: f64) -> usize {
        crate::logical_to_device_px(self.config.window_padding_or_default(), scale)
    }

    /// Top-edge twin of [`Self::cfg_pad_for_scale`], capped at the base pad in
    /// device px (rounding could exceed it at fractional scales).
    pub(crate) fn cfg_pad_top_for_scale(&self, scale: f64) -> usize {
        crate::logical_to_device_px(self.config.window_padding_top_or_default(), scale)
            .min(self.cfg_pad_for_scale(scale))
    }

    /// Synthetic tab-band head in device px for a window at `scale` with cell
    /// height `cell_h`. Zero on macOS, with the strip off, under
    /// `tab_band_height = "compact"`, or when the strip rows already reach the
    /// target.
    ///
    /// A `head` band rather than a bigger `pad_top`: `pad_top` is grid padding
    /// that applies with the strip off too, while `head` already means "host
    /// chrome over the grid" and is handled everywhere (`grid_dims_for`,
    /// `frame_size`, `pixel_to_term_cell`, `strip_col_for_pixel`,
    /// `fill_chrome_bleed`, the pixel band's centring and cache key). Gated on
    /// `tab_strip_rows > 0`.
    ///
    /// Takes `cell_h` because attach and the early reveal hold a cell box the
    /// per-window `MetricsView` does not yet have.
    ///
    /// Compiled on every platform so the config chain stays live and tested; on
    /// macOS it computes 0 because [`TabBandHeight::PLATFORM_DEFAULT`] is
    /// `Compact`.
    pub(crate) fn synthetic_strip_head_px(&self, scale: f64, cell_h: usize) -> usize {
        synthetic_band_head_px(
            self.config.tab_band_height_or_default().target_logical_px(),
            self.cfg_pad_top_for_scale(scale),
            self.tab_strip_rows,
            cell_h,
            scale,
        )
    }

    /// Re-resolve every window's [`MetricsView`] from the live font regime. An
    /// explicit `--font-px` / `config.font_px` or forced scale fixes the px for
    /// all windows (each keeps its own pad); otherwise each window gets
    /// [`MetricsView::for_scale`] of its own scale. Never-attached windows keep
    /// their sealed pad and head.
    pub(crate) fn refresh_all_window_metrics(&mut self) {
        let pinned = self.font_px_explicit || resolve_force_scale().is_some();
        let font_px = self.font_px;
        // Resolve the logical padding once, outside the `windows` borrow.
        let (pad_l, pad_top_l) = (
            self.config.window_padding_or_default(),
            self.config.window_padding_top_or_default(),
        );
        for ws in self.windows.values_mut() {
            // The chrome headroom re-derives from the attach-captured points at
            // the window's scale on both branches (`for_scale` carries none). A
            // never-attached window keeps its sealed record
            // ([`Self::seed_headless_boot_metrics`]), including the
            // `$ATERM_HEADROOM_PX` band.
            let attached = ws.os_window.is_some();
            let head = if attached {
                (ws.head_pts * ws.scale).round() as usize
            } else {
                ws.metrics.head
            };
            ws.metrics = if pinned {
                // A pinned font fixes the px, not the padding: an attached
                // window's pad re-resolves from the live config so padding edits
                // apply under `--font-px` too; a never-attached window keeps its
                // sealed pad.
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
        // The resolved cell box may have moved (zoom, font or theme reload), so
        // report it to the engines here rather than waiting for each window's next
        // raster seam; background windows' sessions still answer DEC 1016 and size
        // OSC 1337 images. The per-window memo makes unchanged windows free.
        //
        // A face swap resolves here against the still-installed face (metrics are
        // refreshed before `rebuild_backend`); the memo keys on the resolved size,
        // so the first post-rebuild `apply_window_scale` still pushes a change.
        let wids: Vec<crate::WindowId> = self.windows.keys().copied().collect();
        for wid in wids {
            self.sync_cell_pixel_size(wid);
        }
    }

    /// Warn once per config load about `font_features` that cannot take effect:
    /// tokens the parser rejected, and valid tags the active font's `GSUB` does
    /// not advertise. Call after shaping is applied so the probe sees the live
    /// face.
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
            crate::logging::stderr_line!(
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
            crate::logging::stderr_line!(
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

        // The candidate state reload_config publishes before its fallible step:
        // changes every cached renderer source, both geometry keys, theme keys
        // and one unrelated live input policy.
        let candidate: Config = aterm_toml::from_str(
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
        assert!(super::theme_eq(app.theme, old_theme));
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

    /// A face rebuild that keeps the cell box (a `font_weight` edit on a variable
    /// face, as the catalog worker prepares it) repaints a CPU window in full.
    /// The CPU damage cache keys on frame size, grid origin and font epoch, and a
    /// sealed generation can repeat all three, so without the rebuild dropping it
    /// the window re-presents the old glyphs (the windowed CPU present copies
    /// `presented_pixels` on a gate hit).
    #[test]
    fn a_same_cell_box_face_rebuild_repaints_the_cpu_window() {
        use aterm_render::DamageOutcome;

        fn present_cpu(
            app: &mut crate::App,
            input: &aterm_render::RenderInput,
        ) -> (Vec<u32>, DamageOutcome) {
            let crate::App {
                backend, windows, ..
            } = app;
            let crate::Backend::Cpu(renderer) = backend.ready_mut() else {
                panic!("headless test backend is CPU")
            };
            let ws = windows
                .get_mut(&crate::WindowId(0))
                .expect("headless fixture window");
            let _ = renderer.render_input_cached(&mut ws.cpu_cache, input);
            (
                ws.cpu_cache.presented_pixels().to_vec(),
                ws.cpu_cache.last_damage(),
            )
        }

        let mut app = crate::App::headless_for_test();
        let mut term = aterm_core::terminal::Terminal::new(3, 8);
        term.process(b"\x1b[?25lABC");
        let input = term.cell_frame(3, 8);
        let _ = present_cpu(&mut app, &input);
        assert_eq!(
            present_cpu(&mut app, &input).1,
            DamageOutcome::GateHit,
            "negative control: an identical frame really is cached"
        );
        let cell = app.backend.cell_size();

        let mut prepared = aterm_render::Renderer::from_system(app.font_px, app.theme)
            .expect("system font for the prepared generation");
        let _ = prepared.set_font_variations(&[(u32::from_be_bytes(*b"wght"), 800.0)], 0.0);
        let _ = prepared.seal_admitted_font_sources();
        let chrome = crate::tray_raster::prepare_chrome_fonts(None, None, None);
        assert!(app.rebuild_backend_with_prepared(Some((prepared, chrome))));
        assert_eq!(
            app.backend.cell_size(),
            cell,
            "the case needs an unchanged cell box"
        );

        let (pixels, damage) = present_cpu(&mut app, &input);
        assert_eq!(damage, DamageOutcome::Full, "the face rebuild repaints");
        let crate::Backend::Cpu(renderer) = app.backend.ready_mut() else {
            panic!("headless test backend is CPU")
        };
        assert!(
            pixels == renderer.render_input(&input).pixels,
            "the window shows what the new face renders"
        );
    }
}

#[cfg(test)]
mod descriptive_title_config_tests {

    /// With `cursor_trail = false`, a config save does not warm the effect
    /// pipelines (the only ones still reachable build within a frame).
    #[test]
    fn an_effects_off_config_never_warms_the_effect_pipelines() {
        let off: Config = aterm_toml::from_str("cursor_trail = false\n").unwrap();
        assert!(
            !off.warms_effect_pipelines(),
            "`cursor_trail = false` must not compile fire/rain pipelines it can never bind"
        );
        // The default and an explicit opt-in warm (the fire pipelines are a
        // visible hitch inline).
        let on: Config = aterm_toml::from_str("cursor_trail = true\n").unwrap();
        assert!(on.warms_effect_pipelines());
        // An absent key follows the platform default, like the trail itself.
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
        aterm_toml::from_str(source).expect("valid descriptive-title config")
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
        assert!(aterm_toml::from_str::<Config>("title_summary_provider = \"remote\"").is_err());

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

    /// The derived observation budget is never coarser than the dwell and never
    /// zero.
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
        assert!(aterm_toml::from_str::<Config>("tab_title_format = \"title_only\"").is_err());
        assert!(aterm_toml::from_str::<Config>("window_title_format = \"both\"").is_err());
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
        assert!(
            aterm_toml::from_str::<Config>("title_summary_proxy_mode = \"automatic\"").is_err()
        );
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
        MAX_USER_THEME_FILES, ThemeCatalog, TrailPackCatalog, ignored_key_notices,
        unaccepted_value_notices,
    };
    #[cfg(unix)]
    use super::{ThemeCatalogWatchError, read_bounded_theme_file};
    use aterm_core::config::BiDiMode;

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
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

    /// The plain-bool knobs: each resolves its shipped default when absent and
    /// honours both explicit spellings.
    #[test]
    fn bool_knobs_resolve_their_default_and_both_spellings() {
        /// `(key, shipped default, resolver)`.
        type Knob = (&'static str, bool, fn(&Config) -> bool);
        let rows: [Knob; 7] = [
            // Serious mode is opt-in.
            ("serious_mode", false, Config::serious_mode_or_default),
            // Focus boost ships ON: the anti-starvation lane costs nothing when idle
            // and only touches the shell root + conhost; `false` opts out.
            ("focus_boost", true, Config::focus_boost_or_default),
            // Tone melody defaults on; `false` pins the neutral melody and stops
            // the classifier via `tone_infer_active`.
            ("tone_melody", true, Config::tone_melody_or_default),
            // Robi is opt-IN (owner directive: disabled by default).
            ("robi", false, Config::robi_or_default),
            // The ambient bed defaults on; `false` silences only the drone.
            ("trail_sound_bed", true, Config::trail_sound_bed_or_default),
            // The strain row is opt-OUT: the owner asked for it.
            (
                "explain_heavy_load",
                true,
                Config::explain_heavy_load_or_default,
            ),
            // aterm's own desktop notices are opt-IN.
            ("desktop_alerts", false, Config::desktop_alerts_or_default),
        ];
        for (key, default, read) in rows {
            assert_eq!(
                read(&Config::default()),
                default,
                "{key}: the shipped default"
            );
            assert!(read(&cfg(&format!("{key} = true"))), "{key} = true");
            assert!(!read(&cfg(&format!("{key} = false"))), "{key} = false");
        }
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
                read_bounded_theme_file(&dir.join("Alias.conf")).is_err(),
                "the handle-level open must reject a symlink even if it is swapped in after scanning"
            );
            assert!(
                read_bounded_theme_file(&dir.join("Pipe.conf")).is_err(),
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

        // Case-colliding names can coexist on a case-sensitive host; neither may
        // win, since the picker and resolver share one case-insensitive namespace.
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

    /// A default config resolves to the default [`super::FontConfig`] with no
    /// warnings, so the startup apply is a no-op.
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

    /// Unresolvable entries warn and are skipped; a file path resolves verbatim;
    /// `font_synthetic_style = false` carries through.
    #[test]
    fn w6_font_config_warns_and_skips_unresolvable() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aterm-render/assets/DejaVuSansMono.ttf"
        );
        // `CARGO_MANIFEST_DIR` is a native path (backslashes on Windows), so escape
        // it for the TOML basic string.
        let toml = format!(
            "font_family_bold = \"definitely-not-a-real-font-xyzzy\"\n\
             font_synthetic_style = false\n\
             fallback_fonts = [{}, \"also-not-a-real-font-xyzzy\"]\n",
            super::toml_basic_string(fixture)
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

    /// An authored path that fails bounded admission is a rejected replacement,
    /// not a clear of the working face; the size policy reaches the diagnostic.
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

    /// Stream fade defaults off with a 90 ms window, and `stream_fade_ms` clamps
    /// to 16..=1000.
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

    /// An ignored `font_px` (out of range or non-finite) yields a banner line
    /// with the value and accepted range, and the resolver falls back; unset or
    /// admissible sizes are silent. Matches `--validate-config`.
    #[test]
    fn an_ignored_font_px_is_a_load_notice() {
        let with = |font_px: Option<f32>| Config {
            font_px,
            ..Config::default()
        };
        let validate_flags = |config: &Config| {
            crate::diagnostics::config_semantic_warnings(config)
                .iter()
                .any(|w| w.key == "font_px")
        };
        for fine in [
            None,
            Some(14.0),
            Some(crate::FONT_PX_MIN),
            Some(crate::FONT_PX_MAX),
        ] {
            assert_eq!(super::font_px_load_notice(&with(fine)), None, "{fine:?}");
            assert!(!validate_flags(&with(fine)), "{fine:?}");
        }
        for (bad, shown) in [
            (0.0, "0.0"),
            (500.0, "500.0"),
            (-5.0, "-5.0"),
            (f32::NAN, "NaN"),
            (f32::INFINITY, "inf"),
        ] {
            assert_eq!(
                super::font_px_load_notice(&with(Some(bad))).as_deref(),
                Some(
                    format!(
                        "config font_px: {shown} is not accepted (expected 6–200; the value \
                         is ignored)"
                    )
                    .as_str()
                ),
                "{bad}"
            );
            assert!(validate_flags(&with(Some(bad))), "{bad}");
            assert_eq!(
                super::resolve_font_px_with(None, Some(bad)),
                crate::FONT_PX,
                "{bad}: the banner's claim is the resolver's behaviour"
            );
        }
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

    /// The warning's vocabulary and near-miss suggestion both come from
    /// [`crate::prefs::CURSOR_TRAIL_STYLES`], so the joined list must match the
    /// picker's entries, order and separator verbatim.
    #[test]
    fn the_trail_style_message_offers_the_pickers_domain_and_names_a_near_miss_in_it() {
        let catalog = TrailPackCatalog::default();
        let message = cfg("cursor_trail_style = \"definitely not a style\"")
            .cursor_trail_style_warning(&catalog)
            .expect("an unknown style warns");
        assert!(
            message.contains(&crate::prefs::CURSOR_TRAIL_STYLES.join("|")),
            "the offered list must BE the picker's domain, in its order: {message}"
        );
        for style in crate::prefs::CURSOR_TRAIL_STYLES {
            let dropped_letter = &style[..style.len() - 1];
            let message = cfg(&format!("cursor_trail_style = \"{dropped_letter}\""))
                .cursor_trail_style_warning(&catalog)
                .expect("a truncated style is not a style");
            assert!(
                message.contains(&format!("did you mean {style:?}")),
                "{dropped_letter:?} must name {style:?}: {message}"
            );
        }
    }

    /// One dropped letter in a two-word value: the near miss is stated before
    /// the list, which is what an 80-column banner truncates.
    #[test]
    fn a_mistyped_trail_style_names_the_style_it_meant_before_listing_the_rest() {
        let catalog = TrailPackCatalog::default();
        let message = cfg("cursor_trail_style = \"rainbow kity\"")
            .cursor_trail_style_warning(&catalog)
            .expect("an unknown style warns");
        let suggestion = message
            .find("did you mean \"rainbow kitty\"")
            .unwrap_or_else(|| panic!("{message}"));
        let vocabulary = message.find("expected one of").expect("the message lists");
        assert!(
            suggestion < vocabulary,
            "the recovery must precede the list the banner cuts: {message}"
        );
    }

    /// A misspelled `[section]` header yields one ignored leaf per entry, and
    /// every one is reported with the `config …` prefix (no cap).
    #[test]
    fn ignored_key_notices_carry_every_key_and_the_config_prefix() {
        let many = (0..12)
            .map(|index| format!("not_a_key_{index} = 1"))
            .collect::<Vec<_>>()
            .join("\n");
        let notices = ignored_key_notices(&many);
        assert_eq!(notices.len(), 12, "{notices:?}");
        assert!(
            notices
                .iter()
                .all(|line| line.starts_with("config ") && line.contains("not_a_key_")),
            "{notices:?}"
        );
        assert!(
            ignored_key_notices("columns = 100\n").is_empty(),
            "an accepted config raises no row"
        );
    }

    /// A value this build rejects is reported on the band with the `config …`
    /// prefix, and stands down for a key whose resolver already named the
    /// refused value.
    #[test]
    fn an_unaccepted_value_reaches_the_banner_unless_a_resolver_already_named_it() {
        let notices = unaccepted_value_notices("window_theme = \"drak\"\n", &[]);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].starts_with("config ") && notices[0].contains("did you mean \"dark\""),
            "{notices:?}"
        );
        assert!(
            unaccepted_value_notices(
                "window_theme = \"drak\"\n",
                &["config window_theme: expected auto|light|dark, got \"drak\"".to_string()],
            )
            .is_empty(),
            "a resolver that named the value keeps its own fuller sentence"
        );
        assert!(
            unaccepted_value_notices("window_theme = \"dark\"\n", &[]).is_empty(),
            "an accepted value raises no banner"
        );
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

    /// `text_blending`: absent or `linear-corrected` (any case, either separator)
    /// is the default; `linear` opts out; unknown values fall back to the default.
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
    /// values, and the stem-gamma clamp.
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

    /// Mapping invariants of `Config::text_shaping` over generated typography
    /// keys (the example tests above cover points; this covers the space). For
    /// ANY `ligatures` / `font_features`: (1) the ligature mode is total + exact
    /// (`Some(false)` => Disabled, else Enabled); (2) features land on at most
    /// ONE primary-face (`font_id == 0`) set that is never empty; (3)
    /// `ambiguous_width` is never set here (the engine cell-width path owns it).
    #[test]
    fn text_shaping_mapping_invariants() {
        use aterm_types::text_shaping::{AmbiguousWidth, LigatureMode};
        const CHARS: &[u8] = b"abzAZ09 ";
        let mut rng = aterm_spec::rng::XorShift::new(0x7E47_5A9E);
        let mut next = move |n: u64| rng.below(n);
        for _ in 0..256 {
            let lig = [None, Some(false), Some(true)][next(3) as usize];
            let feats = (next(2) == 0).then(|| {
                (0..next(5))
                    .map(|_| {
                        let mut f = String::new();
                        match next(3) {
                            0 => f.push('+'),
                            1 => f.push('-'),
                            _ => {}
                        }
                        for _ in 0..next(7) {
                            f.push(char::from(CHARS[next(CHARS.len() as u64) as usize]));
                        }
                        f
                    })
                    .collect::<Vec<_>>()
            });
            let config = Config {
                ligatures: lig,
                font_features: feats.clone(),
                ..Config::default()
            };
            let s = config.text_shaping();
            let expected = if lig == Some(false) {
                LigatureMode::Disabled
            } else {
                LigatureMode::Enabled
            };
            assert_eq!(s.ligature_mode, expected, "{lig:?} {feats:?}");
            assert_eq!(s.ambiguous_width, AmbiguousWidth::Single);
            assert!(s.font_features.len() <= 1, "{feats:?}");
            if let Some(set) = s.font_features.first() {
                assert_eq!(set.font_id, 0);
                assert!(!set.features.is_empty(), "{feats:?}");
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

    /// Both live Sparkle-word products default on (an absent table enables
    /// profanity, feline and bare `cat`); an explicit master-off opts out fully.
    #[test]
    fn sparkle_words_default_on_for_the_two_live_products() {
        let deco = Config::default()
            .sparkle_deco_config()
            .expect("sparkle words are ON by default (absent table → defaults)");
        assert!(deco.profanity, "profanity sparkles by default");
        assert!(deco.feline, "feline cat-paw on by default");
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
        // Cat is the only graphic mode; legacy keys never reach the runtime.
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
        // Matches the documented `supernova_chance` default.
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

    /// The native style match is trimmed and case-insensitive, like the web
    /// `set_sparkle_profanity` setter: `"Nova"` selects nova, `"SPARKLE"` sparkle.
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

    /// `[[sparkle_words.custom]]` parses, keys the spec table by folded
    /// form_hash, appends words to the lexicon override (CJK as `cjk = true`),
    /// and keeps emphasis scanning with ink off (gate:
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

    /// Sparkle Words off suppresses every non-feline route, custom specs
    /// included, while Keyword Kitties stays live.
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
            !deco.canine,
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

        // Delete the admitted source so any render-time reopen would fail.
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
                disk_faults: std::sync::Arc::default(),
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

    /// A typo added by a live edit is reported like one present at launch, even
    /// though the parsed `Config` is equal and the reload takes the dedupe path.
    ///
    /// The empty case is published too, so fixing the typo clears its row; only
    /// rows this path re-derives are replaced (an unrelated chord row stays).
    #[test]
    fn a_misspelled_key_added_by_a_live_edit_still_warns_through_the_dedupe() {
        fn apply(app: &mut crate::App, dir: &std::path::Path, text: &str) {
            let path = dir.join("aterm.toml");
            std::fs::write(&path, text).expect("write config");
            let observation =
                crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                    .expect("observe");
            let config: Config = aterm_toml::from_str(text).expect("valid toml");
            let assets = config.resolve_asset_catalog();
            let path_feed_fps = config.path_feed_fingerprints();
            let sparkle = config.prepare_sparkle_runtime();
            app.apply_prepared_config_generation(
                crate::native_font_catalog::PreparedConfigGeneration {
                    observation,
                    config,
                    values: std::collections::BTreeMap::new(),
                    disk_faults: std::sync::Arc::default(),
                    assets,
                    path_feed_fps,
                    sparkle,
                    fonts: None,
                    warnings: Vec::new(),
                },
            );
        }

        let dir = std::env::temp_dir().join(format!("aterm-typo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut app = crate::App::headless_for_test();

        // A key this build does not know, and NOTHING else — so the parse is
        // equal to what is already applied and the dedupe fires.
        apply(&mut app, &dir, "windw_padding = 20\n");
        let ignored = app
            .messages
            .live_by_key(crate::message_reporters::ConfigFamily::IgnoredKeys.key())
            .expect("a misspelled key must warn even when the parse dedupes");
        assert!(
            ignored
                .msg
                .detail
                .iter()
                .any(|l| l.contains("windw_padding")),
            "the row names the key that was ignored: {:?}",
            ignored.msg
        );

        // A row from a family this path never re-derives — the chord the
        // last FULL reload posted — stands in for every parse-derived row.
        let mut chord = crate::message_reporters::ConfigWarnings::default();
        chord.push(
            crate::message_reporters::ConfigFamily::Keybindings,
            "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
        );
        for msg in chord.into_messages() {
            app.post_message(msg);
        }

        // Negative control: fixing the typo clears its row only; the chord row
        // stays.
        apply(&mut app, &dir, "# fixed\n");
        assert!(
            app.messages
                .live_by_key(crate::message_reporters::ConfigFamily::IgnoredKeys.key())
                .is_none(),
            "removing the bad key clears its row"
        );
        assert!(
            app.messages
                .live_by_key(crate::message_reporters::ConfigFamily::Keybindings.key())
                .is_some(),
            "a comment-only save leaves the chord's still-true row standing"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unloadable theme is named on the unaccepted-values row, and the theme
    /// file arriving in the themes directory clears it.
    #[test]
    fn a_theme_that_does_not_load_is_on_the_band_until_its_file_arrives() {
        const NAME: &str = "Aterm Test Theme 7F1C";
        let dir = std::env::temp_dir().join(format!("aterm-theme-miss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("aterm.toml");
        let text = format!("theme = {NAME:?}\n");
        std::fs::write(&path, &text).expect("write config");
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .expect("observe");
        let config: Config = aterm_toml::from_str(&text).expect("valid toml");
        let assets = config.resolve_asset_catalog();
        let path_feed_fps = config.path_feed_fingerprints();
        let sparkle = config.prepare_sparkle_runtime();
        let mut app = crate::App::headless_for_test();
        app.apply_prepared_config_generation(
            crate::native_font_catalog::PreparedConfigGeneration {
                observation,
                config,
                values: std::collections::BTreeMap::new(),
                disk_faults: std::sync::Arc::default(),
                assets,
                path_feed_fps,
                sparkle,
                fonts: None,
                warnings: Vec::new(),
            },
        );
        let key = crate::message_reporters::ConfigFamily::UnacceptedValues.key();
        let row = app
            .messages
            .live_by_key(key)
            .expect("a theme that does not load is on the band");
        let expected = format!(
            "theme: {NAME:?} is unavailable (no built-in or loaded custom theme by that name); \
             Default is used"
        );
        assert!(
            row.msg.detail.iter().any(|line| line.contains(&expected)),
            "{:?}",
            row.msg
        );

        app.reload_theme_catalog(ThemeCatalog::from_schemes([(
            NAME.to_string(),
            aterm_types::ColorScheme::default(),
        )]));
        assert!(
            app.messages.live_by_key(key).is_none(),
            "the theme file arriving takes the row down"
        );
        let _ = std::fs::remove_dir_all(&dir);
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

    /// `recompute_sparkle`'s warning filter uses the resolved scan options: the
    /// "requires `cjk_single_char = true`" warning is dropped when that opt-in is
    /// on, while mixed-script surfaces warn either way. Runs the full
    /// config → deco-config → override → lexicon → `sparkle_logged_warnings`
    /// chain.
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

    /// Alt-screen suppression is opt-in: the default decorates the alternate
    /// screen; `suppress_in_alt_screen = true` suppresses it.
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

    /// `suppress_in_alt_screen` defaults to false; an explicit `true`
    /// suppresses alternate-screen decoration.
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

    /// The legacy paw mode parses as ink-only; the four legacy feline controls
    /// round-trip as source data but never enter live state.
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

    /// `[sparkle_words.feline] log` deserializes, defaults on, and is host-side
    /// only: it does not change the resolved `DecoConfig`.
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

    /// Curse-bonk knobs round-trip: `bonk` defaults on, `bonk_detonation` off,
    /// and neither changes the decorations.
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

    /// `[sparkle_words.ink]`: the serde-renamed `loop` key parses and every
    /// number clamps to its documented range, including the higher sweep floor
    /// for a looping shimmer.
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

/// Config round-trip: each renderer key (a) parses to its clamped resolver
/// value, (b) serializes through `prefs::apply_prefs_edits` and re-parses
/// equal, and (c) diffs to exactly one [`KnobChange`] (or one `TerminalConfig`
/// field for engine keys). `apply_render_knob` maps each variant to one
/// `Backend` call, so there is no fan-out.
#[cfg(test)]
mod w5_knob_tests {
    use super::{Config, KnobChange, RenderKnobs};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
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

    /// Each key serializes through `apply_prefs_edits` and re-parses to the same
    /// resolved value.
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
        let c: Config = aterm_toml::from_str(&out).expect("round-trips through serde");
        assert_eq!(c.line_height_or_default(), 1.35);
        assert_eq!(c.minimum_contrast_or_default(), 4.5);
        assert_eq!(c.selection_foreground_u32(), Some(0x00AA_BBCC));
        assert!(c.selection_inactive_or_default());
        assert_eq!(c.cursor_break_ligatures, Some(true));
        assert_eq!(c.bold_is_bright, Some(false));
        assert_eq!(c.faint_opacity_or_default(), 0.4);
        assert_eq!(c.adjust_baseline_or_default(), -2);
    }

    /// Each renderer key changed alone emits exactly one `KnobChange`; an
    /// unchanged config emits none.
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

    /// `background_opacity < 1.0` fans out to two changes: the opacity and the
    /// auto-raised floor (`MinimumContrast(4.5)`). With a configured floor ≥ 4.5
    /// only the opacity changes; reverting to solid restores the configured
    /// floor.
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

    /// A translucent `background_opacity` and a valid `background_material`
    /// resolve to live values (not `>= 1.0` or `None`), which the GPU present and
    /// `AppRt::window_set_vibrancy` consume.
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

/// Legibility guarantee (Tier-1): [`Config::effective_minimum_contrast`] over
/// the opacity × configured-contrast lattice satisfies the `NeverIllegible`
/// invariant of `aterm_spec::derive::vibrancy_contrast_model`: translucent
/// windows always clear 4.5:1, opaque ones keep the configured floor. Includes
/// a non-vacuity control (the floor rises for a default install) and a
/// negative control (the raw configured floor would ship sub-4.5 glass).
#[cfg(test)]
mod vibrancy_contrast_guarantee {
    use super::{Config, VIBRANCY_CONTRAST_FLOOR};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
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

        // Negative control: the raw `minimum_contrast_or_default` would ship 1.0
        // under glass.
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
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn window_theme_resolves_auto_light_dark_and_defaults_to_auto() {
        // No key means Auto (follow the OS).
        assert_eq!(
            Config::default().window_theme_or_default(),
            WindowTheme::Auto
        );
        for (source, expected) in [
            ("font_px = 14.0", WindowTheme::Auto),
            ("window_theme = \"auto\"", WindowTheme::Auto),
            ("window_theme = \"light\"", WindowTheme::Light),
            ("window_theme = \"dark\"", WindowTheme::Dark),
            // Case-insensitive and trimmed.
            ("window_theme = \" Dark \"", WindowTheme::Dark),
            ("window_theme = \"LIGHT\"", WindowTheme::Light),
            // An invalid value defaults to Auto.
            ("window_theme = \"midnight\"", WindowTheme::Auto),
            ("window_theme = \"\"", WindowTheme::Auto),
        ] {
            assert_eq!(cfg(source).window_theme_or_default(), expected, "{source}");
        }
        // Direct parser: unknown -> None (caller defaults).
        assert_eq!(WindowTheme::parse("nope"), None);
        assert_eq!(WindowTheme::parse("auto"), Some(WindowTheme::Auto));
    }
}

/// [`DEFAULT_DECORATIVE_EFFECTS`]: a platform default that explicit keys always
/// override.
#[cfg(test)]
mod decorative_effect_default_tests {
    use super::{Config, DEFAULT_DECORATIVE_EFFECTS};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// `cursor_trail` follows [`DEFAULT_DECORATIVE_EFFECTS`];
    /// `cursor_trail_bloom` (inert with the master off) and
    /// `[sparkle_words] enabled` (see
    /// [`Config::sparkle_words_enabled_or_default`]) do not.
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

#[cfg(test)]
mod output_streak_cfg_tests {
    //! `[output_streak]` resolver tests
    //! (`docs/DESIGN-output-streak-2026-08-30.md` §5, §7, §10): the table
    //! defaults on except on Windows, numeric keys clamp at both ends, and
    //! non-finite scalars fall toward less effect.

    use super::{Config, DEFAULT_DECORATIVE_EFFECTS};
    use aterm_effects::output_streak::{MAX_COMETS, TAIL_MAX, TAIL_MIN};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// Default follows [`DEFAULT_DECORATIVE_EFFECTS`]; a config or table that
    /// sets only other keys still gets the default, and an explicit value wins
    /// on every platform.
    #[test]
    fn the_output_streak_ships_on_as_an_opt_out_under_the_windows_platform_law() {
        assert_eq!(DEFAULT_DECORATIVE_EFFECTS, !cfg!(windows));
        assert_eq!(
            Config::default().output_streak_enabled_or_default(),
            DEFAULT_DECORATIVE_EFFECTS,
            "absent [output_streak] table ⇒ the ruling's default on, minus Windows"
        );
        assert_eq!(
            cfg("font_px = 14.0").output_streak_enabled_or_default(),
            DEFAULT_DECORATIVE_EFFECTS
        );
        assert_eq!(
            cfg("[output_streak]\ntail = 12").output_streak_enabled_or_default(),
            DEFAULT_DECORATIVE_EFFECTS,
            "an absent `enabled` key is still the default, even with other knobs set"
        );
        assert!(cfg("[output_streak]\nenabled = true").output_streak_enabled_or_default());
        assert!(!cfg("[output_streak]\nenabled = false").output_streak_enabled_or_default());
    }

    /// Sound defaults on (plain `true`, not the decorative family); on Windows
    /// the master is already off.
    #[test]
    fn the_streak_pip_ships_on_because_the_ruling_calls_it_one_feature() {
        assert!(
            Config::default().output_streak_sound_or_default(),
            "absent table ⇒ the pip is ON everywhere, platform-independently"
        );
        assert!(cfg("[output_streak]").output_streak_sound_or_default());
        assert!(cfg("[output_streak]\nsound = true").output_streak_sound_or_default());
        assert!(!cfg("[output_streak]\nsound = false").output_streak_sound_or_default());
    }

    /// Every numeric key clamps at both ends into its documented band; the
    /// cell-counted ones use the engine's constants. Nothing rejects the file.
    #[test]
    fn every_output_streak_number_clamps_into_its_band_at_both_ends() {
        let fresh = Config::default();
        assert_eq!(fresh.output_streak_intensity_or_default(), 1.0);
        assert_eq!(fresh.output_streak_tail_or_default(), 9);
        assert_eq!(fresh.output_streak_max_streaks_or_default(), 3);
        assert_eq!(fresh.output_streak_idle_secs_or_default(), 10.0);

        assert_eq!(
            cfg("[output_streak]\nintensity = 4.0").output_streak_intensity_or_default(),
            1.0
        );
        assert_eq!(
            cfg("[output_streak]\nintensity = -2.0").output_streak_intensity_or_default(),
            0.0,
            "0 is a real setting — design §1 resets rather than dims there"
        );

        assert_eq!(
            cfg("[output_streak]\ntail = 4000").output_streak_tail_or_default(),
            TAIL_MAX,
            "a tail past the ceiling would be a rainbow bar, not a sheen"
        );
        assert_eq!(
            cfg("[output_streak]\ntail = 0").output_streak_tail_or_default(),
            TAIL_MIN
        );

        assert_eq!(
            cfg("[output_streak]\nmax_streaks = 99").output_streak_max_streaks_or_default(),
            MAX_COMETS as u8
        );
        assert_eq!(
            cfg("[output_streak]\nmax_streaks = 0").output_streak_max_streaks_or_default(),
            1,
            "\"no comets\" is spelled enabled = false, never a silent second off switch"
        );

        assert_eq!(
            cfg("[output_streak]\nidle_secs = 9000.0").output_streak_idle_secs_or_default(),
            120.0,
            "no configuration animates forever (design §5)"
        );
        assert_eq!(
            cfg("[output_streak]\nidle_secs = 0.0").output_streak_idle_secs_or_default(),
            2.0
        );

        // In-band values pass through untouched, so the clamps above are real
        // bounds rather than a resolver that ignores its input.
        let set =
            cfg("[output_streak]\nintensity = 0.5\ntail = 6\nmax_streaks = 2\nidle_secs = 33.5");
        assert_eq!(set.output_streak_intensity_or_default(), 0.5);
        assert_eq!(set.output_streak_tail_or_default(), 6);
        assert_eq!(set.output_streak_max_streaks_or_default(), 2);
        assert_eq!(set.output_streak_idle_secs_or_default(), 33.5);
    }

    /// Non-finite values fall toward less effect: intensity reads 0.0 and
    /// `idle_secs` the band floor, never the 10 s default.
    #[test]
    fn non_finite_output_streak_scalars_fail_silent_toward_less_effect() {
        for source in [
            "[output_streak]\nintensity = nan",
            "[output_streak]\nintensity = inf",
            "[output_streak]\nintensity = -inf",
        ] {
            assert_eq!(
                cfg(source).output_streak_intensity_or_default(),
                0.0,
                "{source}"
            );
        }
        for source in [
            "[output_streak]\nidle_secs = nan",
            "[output_streak]\nidle_secs = inf",
            "[output_streak]\nidle_secs = -inf",
        ] {
            assert_eq!(
                cfg(source).output_streak_idle_secs_or_default(),
                2.0,
                "{source}"
            );
        }
    }

    /// Only `sound = true` under a disabled master warns; out-of-band numbers
    /// (documented clamps) do not.
    #[test]
    fn the_streak_warning_names_only_the_silently_unreachable_pip() {
        assert!(Config::default().output_streak_warning().is_none());
        assert!(
            cfg("[output_streak]\ntail = 4000\nmax_streaks = 99\nidle_secs = 9000.0")
                .output_streak_warning()
                .is_none(),
            "a documented clamp is not a warning in this repo"
        );
        assert!(
            cfg("[output_streak]\nenabled = true\nsound = true")
                .output_streak_warning()
                .is_none()
        );
        let warning = cfg("[output_streak]\nenabled = false\nsound = true")
            .output_streak_warning()
            .expect("an unreachable pip is worth saying out loud");
        assert!(
            warning.contains("output_streak.sound") && warning.contains("effectively silent"),
            "the message names the key and the effective result: {warning}"
        );
    }

    /// Settings ▸ Harness words: each limited key named in the supervisor's KEYS
    /// order as `<key>: <value>`; a rewritten text named apart as `yours`;
    /// `trust_roots` only where `safe` reads it; `Off` outranks every limit.
    /// Negative control: no table is `Automatic`.
    #[test]
    fn the_harness_words_name_every_limit_in_key_order() {
        assert_eq!(super::HarnessPolicy::read("").words(), "Automatic");
        let all = "[harness]\nheadless = false\napprove = \"none\"\ntrust_roots = []\n\
                   answer_questions = false\nanswer_text = \"ask me\"\n\
                   dismiss_surveys = false\ncontinue = false\ncontinue_text = \"go\"\n\
                   continue_per_hour = 2\nrules_file = \"/r\"\nretry_api_errors = false\n\
                   probe_api = false\nresume_limits = false\nlimit_wait = false\n\
                   accounts = \"off\"\nmodel_fallback = \"\"\n\
                   compact_on_context_wall = false\nrelaunch = false\nupgrade = false\n\
                   human_grace_s = 600\n";
        assert_eq!(
            super::HarnessPolicy::read(all).words(),
            "Automatic \u{b7} limited: headless: off \u{b7} approve: none \
             \u{b7} answer_questions: off \u{b7} dismiss_surveys: off \u{b7} continue: off \
             \u{b7} continue_per_hour: 2 \u{b7} retry_api_errors: off \u{b7} probe_api: off \
             \u{b7} resume_limits: off \u{b7} limit_wait: off \u{b7} accounts: off \
             \u{b7} model_fallback: off \u{b7} compact_on_context_wall: off \u{b7} relaunch: off \
             \u{b7} upgrade: off \u{b7} human_grace_s: 600 \
             \u{b7} yours: answer_text, continue_text, rules_file"
        );
        assert_eq!(
            super::HarnessPolicy::read("[harness]\napprove = \"safe\"\ntrust_roots = []\n").words(),
            "Automatic \u{b7} limited: approve: safe \u{b7} trust_roots: off"
        );
        assert_eq!(
            super::HarnessPolicy::read("[harness]\nanswer_text = \"ask me\"\n").words(),
            "Automatic \u{b7} yours: answer_text"
        );
        // `accounts`: `wall` is a limit of `early` and named; `early`, the
        // default, is none (NEGATIVE CONTROL).
        assert_eq!(
            super::HarnessPolicy::read("[harness]\naccounts = \"wall\"\n").words(),
            "Automatic \u{b7} limited: accounts: wall"
        );
        assert_eq!(
            super::HarnessPolicy::read("[harness]\naccounts = \"early\"\n").words(),
            "Automatic"
        );
        // `limit_reset`: its `true` gives the supervisor more, so it is named
        // as the owner's own choice and never as a limit; `false`, the
        // default, names nothing (NEGATIVE CONTROL).
        assert_eq!(
            super::HarnessPolicy::read("[harness]\nlimit_reset = true\n").words(),
            "Automatic \u{b7} yours: limit_reset"
        );
        assert_eq!(
            super::HarnessPolicy::read("[harness]\nlimit_reset = false\n").words(),
            "Automatic"
        );
        assert!(!super::HarnessPolicy::read("").policy.limit_reset);
        assert_eq!(
            super::HarnessPolicy::read(&format!("{all}enabled = false\n")).words(),
            "Off: no agent session is supervised"
        );
    }

    #[test]
    fn a_launch_that_could_not_load_the_file_still_reads_its_harness_lines() {
        let text = "font_px = \n[harness]\ncontinue = false\napprove = \"safe\"\n";
        assert!(Config::parse(text).is_err(), "the fixture is not a config");
        let stand_in = Config::stand_in(text);
        let p = &stand_in.harness.policy;
        assert!(p.enabled && !p.continue_policy);
        assert_eq!(p.approve, aterm_agent::supervise::config::Approve::Safe);
        assert!(stand_in.harness.notes[0].contains("not valid TOML"));
        assert!(
            stand_in.font_px.is_none(),
            "every other setting at its default"
        );
        assert_eq!(
            Config::stand_in("font_px = \n").harness.policy,
            aterm_agent::supervise::SupervisorConfig::default()
        );
    }

    /// `[harness] question_priority`: its three words are read without touching
    /// `answer_questions`; a refused value keeps the default with one notice;
    /// `answer_questions = false` answers nothing but keeps the priority.
    #[test]
    fn a_refused_question_priority_changes_nothing() {
        use aterm_agent::supervise::QuestionPriority;
        let cfg = |text: &str| Config::parse(text).expect("valid config");
        for (word, want) in [
            ("recommended", QuestionPriority::Recommended),
            ("long-term", QuestionPriority::LongTerm),
            ("short-term", QuestionPriority::ShortTerm),
        ] {
            let c = cfg(&format!("[harness]\nquestion_priority = \"{word}\"\n"));
            let policy = &c.harness.policy;
            assert!(c.harness.notes.is_empty(), "{word}: {:?}", c.harness.notes);
            assert_eq!(policy.question_priority, want);
            assert!(policy.answer_questions && policy.session_questions);
            assert_eq!(c.harness_question_priority(), want);
            assert!(c.harness_notices().is_empty());
        }
        assert_eq!(
            Config::default().harness_question_priority(),
            QuestionPriority::Recommended
        );
        for text in [
            "[harness]\nquestion_priority = \"long_term\"\n",
            "[harness]\nquestion_priority = true\n",
            "[harness]\nanswer_questions = true\nquestion_priority = \"sometimes\"\n",
        ] {
            let c = cfg(text);
            assert_eq!(
                c.harness.policy,
                aterm_agent::supervise::SupervisorConfig::default(),
                "{text:?}"
            );
            let told = c.harness_notices();
            assert_eq!(told.len(), 1, "{told:?}");
            assert!(
                told[0].starts_with("config harness.question_priority:")
                    && told[0].ends_with("keeps its default"),
                "{told:?}"
            );
        }
        for text in [
            "[harness]\nanswer_questions = false\nquestion_priority = \"long-term\"\n",
            "[harness]\nquestion_priority = \"long-term\"\nanswer_questions = false\n",
        ] {
            let c = cfg(text);
            assert!(c.harness.notes.is_empty(), "{:?}", c.harness.notes);
            assert!(!c.harness.policy.answer_questions, "{text:?}");
            assert_eq!(
                c.harness.policy.question_priority,
                QuestionPriority::LongTerm
            );
            assert!(
                c.harness.policy.session_questions,
                "a switch written right keeps the session's say"
            );
        }
    }

    /// `[harness]` as `SupervisorConfig::from_aterm_toml` reads it: a changed
    /// key makes a changed `Config` (not deduped); an unreadable value keeps its
    /// default and is a notice naming the key (an off spelling is a limit); a
    /// `false` under another table still limits and is named; an unknown key is
    /// left to the ignored-key notice. Never a failed load.
    #[test]
    fn the_harness_table_is_the_supervisors_policy() {
        use aterm_agent::supervise::{SuperviseOpts, SupervisorConfig};
        let cfg = |text: &str| Config::parse(text).expect("valid config");
        assert_eq!(Config::default().harness, super::HarnessPolicy::default());
        assert_eq!(
            Config::default().harness.policy,
            SupervisorConfig::default()
        );

        let text = "[harness]\nenabled = true\nheadless = false\ncontinue = false\n\
                    continue_per_hour = 2\ntrust_roots = [\"~/a*\", \"/b\"]\n\
                    model_fallback = \"\"\n";
        let set = cfg(text);
        let policy = &set.harness.policy;
        assert!(set.harness.notes.is_empty(), "{:?}", set.harness.notes);
        assert!(policy.enabled && !policy.headless && !policy.continue_policy);
        assert_eq!(policy.continue_per_hour, 2);
        assert_eq!(
            policy.trust_roots,
            vec!["~/a*".to_string(), "/b".to_string()]
        );
        assert_eq!(policy.model_fallback, None);
        assert!(set.harness_enabled());
        // A reload that changes only a policy key is a changed Config.
        assert!(set != cfg(&text.replace("continue = false", "continue = true")));

        // An unreadable value keeps its default and is a notice naming the key;
        // the load stands and the rest of the table applies.
        let bad = cfg("[harness]\ncontinue = \"maybe\"\nheadless = false\ncontine = true\n");
        assert!(!bad.harness.policy.headless, "the other keys still apply");
        assert!(
            bad.harness.policy.continue_policy,
            "the unreadable switch keeps its default"
        );
        assert_eq!(bad.harness.notes.len(), 2, "{:?}", bad.harness.notes);
        let notices = bad.harness_notices();
        assert_eq!(notices.len(), 1, "the unknown key is the config language's");
        assert!(
            notices[0].starts_with("config harness.continue:")
                && notices[0].contains("maybe")
                && notices[0].contains("keeps its default"),
            "{notices:?}"
        );
        let ignored = crate::native_config_language::key_warnings(
            "[harness]\ncontinue = \"maybe\"\nheadless = false\ncontine = true\n",
        )
        .ignored;
        assert!(ignored.iter().any(|l| l.contains("contine")), "{ignored:?}");
        // An explicit off spelled `"no"` is OFF; `"yes"` is ON.
        assert!(!cfg("[harness]\nenabled = \"no\"\n").harness_enabled());
        assert!(cfg("[harness]\nenabled = \"yes\"\n").harness_enabled());
        assert_eq!(
            cfg("[harness]\napprove = \"everything\"\n").harness_approve(),
            "all"
        );
        // The config language's domain check stands down for a value the reader
        // already named. Negative control: without the reader's note it speaks.
        let text = "[harness]\napprove = \"everything\"\n";
        let told = cfg(text).harness_notices();
        assert!(told[0].contains("keeps its default"), "{told:?}");
        assert_eq!(
            super::unaccepted_value_notices(text, &told),
            Vec::<String>::new()
        );
        assert_eq!(super::unaccepted_value_notices(text, &[]).len(), 1);

        // A `false` below another table's header still limits, and is said.
        let misplaced = cfg("[packages]\nharness.enabled = false\n");
        assert!(!misplaced.harness_enabled());
        assert!(
            misplaced.harness_notices()[0].contains("`packages.harness.enabled`"),
            "{:?}",
            misplaced.harness_notices()
        );

        // `approve = "none"` stops the engine pressing; negative control: the
        // default presses.
        let presses = |c: &Config| {
            SuperviseOpts::hosted_with(&c.harness.policy).policy.approve
                != aterm_agent::supervise::config::Approve::None
        };
        assert!(!presses(&cfg("[harness]\napprove = \"none\"\n")));
        assert!(
            !presses(&cfg("[harness]\napprove = 0\n")),
            "`0` is an off: none"
        );
        assert!(
            presses(&cfg("[harness]\napprove = \"most\"\n")),
            "a value that is no level is no setting: all"
        );
        assert!(presses(&Config::default()));
        assert!(presses(&cfg("[harness]\napprove = \"safe\"\n")));
    }
    /// The whole table round-trips through serde; with `PartialEq` a no-op
    /// dedupes and a real edit lands without a restart.
    #[test]
    fn the_output_streak_table_round_trips_for_hot_reload() {
        let source = concat!(
            "[output_streak]\nenabled = false\nintensity = 0.6\ntail = 11\n",
            "max_streaks = 2\nidle_secs = 45.0\nsound = false\n"
        );
        let parsed = cfg(source);
        let streak = parsed.output_streak.as_ref().expect("table parsed");
        assert_eq!(streak.enabled, Some(false));
        assert_eq!(streak.intensity, Some(0.6));
        assert_eq!(streak.tail, Some(11));
        assert_eq!(streak.max_streaks, Some(2));
        assert_eq!(streak.idle_secs, Some(45.0));
        assert_eq!(streak.sound, Some(false));
        // `Config` is not `Debug` (it may carry secrets), so compare with
        // `assert!`.
        assert!(
            parsed == cfg(source),
            "a semantic no-op reload compares equal"
        );
        assert!(
            parsed != Config::default(),
            "a real edit is visible to the reload dedupe"
        );
        // An unknown key inside the table is IGNORED, never fatal — the
        // forward-compatibility law this whole struct is built on.
        assert!(
            cfg("[output_streak]\nfuture_knob = 3\nenabled = true")
                .output_streak_enabled_or_default()
        );
    }
}

/// Tab band height policy and synthetic-head arithmetic. The law is pure; the
/// one host-dependent fact (the shipped face's cell height) has its own
/// Windows test.
#[cfg(test)]
mod tab_band_height_tests {
    use super::{Config, TabBandHeight, synthetic_band_head_px};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// The absent-key default is `standard` on Windows and Linux only (checked
    /// via `cfg!`).
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

    /// With the strip off there is no head, for both targets and every scale.
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

    /// `head + pad_top + strip_rows·cell_h` lands on
    /// [`super::TAB_BAND_STANDARD_LOGICAL_PX`] at 96 dpi and 150%, with a 2-3 px
    /// top pad and one strip row.
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
        // At 150% the residue is re-derived, not scaled.
        let band = (target as f64 * 1.5).round() as usize;
        let head = synthetic_band_head_px(target, 3, 1, 28, 1.5);
        assert_eq!(head, band - 3 - 28);
        assert_eq!(head + 3 + 28, band);
    }

    /// Once the strip rows reach the target (big font or two rows), the head is
    /// zero rather than stacking. Cell heights derive from the platform target.
    #[test]
    fn a_tall_strip_absorbs_the_target_and_the_head_collapses() {
        let target = TabBandHeight::Standard.target_logical_px();
        let absorbing = target as usize - 2; // one row + the pad reaches the target
        assert_eq!(synthetic_band_head_px(target, 2, 1, absorbing, 1.0), 0);
        assert_eq!(synthetic_band_head_px(target, 2, 1, 64, 1.0), 0);
        assert_eq!(synthetic_band_head_px(target, 2, 2, 21, 1.0), 0);
    }

    /// A pathological target is capped at [`super::SYNTHETIC_BAND_HEAD_CELL_CAP`]
    /// cells; a non-finite or non-positive scale is treated as 1.0.
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

    /// A NaN or non-positive target returns 0; the guard must not be rewritten
    /// as `target <= 0.0`, which is false for NaN.
    #[test]
    fn a_non_finite_or_non_positive_target_yields_no_head() {
        for target in [f32::NAN, f32::NEG_INFINITY, 0.0, -32.0] {
            assert_eq!(
                synthetic_band_head_px(target, 2, 1, 21, 1.0),
                0,
                "degenerate target {target} must collapse the head"
            );
        }
        // Positive infinity passes the guard (`inf > 0.0`) and is bounded by the
        // cell cap like any oversized target.
        assert_eq!(
            synthetic_band_head_px(f32::INFINITY, 0, 1, 10, 1.0),
            10 * super::SYNTHETIC_BAND_HEAD_CELL_CAP
        );
        // The control: the SAME inputs with a finite positive target do produce
        // a head, so the assertions above are not passing for some other reason.
        assert_ne!(synthetic_band_head_px(32.0, 2, 1, 21, 1.0), 0);
    }

    /// Windows only: with the shipped face at the Windows `FONT_PX`, the
    /// standard band reaches the target, or the head collapses to 0 if the face
    /// is unusually tall (never overshooting).
    #[cfg(windows)]
    #[test]
    fn the_shipped_windows_face_reaches_the_standard_band() {
        let Some(renderer) =
            aterm_render::Renderer::from_system(crate::FONT_PX, aterm_render::Theme::default())
        else {
            crate::logging::stderr_line!("SKIP: no system monospace font");
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
            crate::logging::stderr_line!(
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
}

#[cfg(test)]
mod right_click_tests {
    use super::{Config, RightClickGesture};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// The absent-key default is copy/paste on Windows, off elsewhere (checked
    /// via `cfg!`).
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

    /// Both explicit values parse case-insensitively and trimmed, with the dash
    /// alias, so either platform can opt into the other's default.
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

/// The tab-menu chord knob, the only way to give the keys back (the chord is
/// not in `[keybindings]`): it must parse, default and fail safe.
#[cfg(test)]
mod tab_menu_chord_tests {
    use super::{Config, TabMenuChord};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// Absent means `MenuKey`, so Shift+F10 (`kf22`) stays with the program.
    #[test]
    fn absent_claims_the_menu_key_only() {
        assert_eq!(
            Config::default().tab_menu_chord_or_default(),
            TabMenuChord::MenuKey
        );
        assert_eq!(
            cfg("font_px = 14.0").tab_menu_chord_or_default(),
            TabMenuChord::MenuKey
        );
        assert!(TabMenuChord::MenuKey.claims_menu_key());
        assert!(
            !TabMenuChord::MenuKey.claims_shift_f10(),
            "the default never takes kf22 from the application"
        );
    }

    /// The second Windows spelling is an OPT-IN: `"on"` is the only value that
    /// claims Shift+F10, and it has to be written down.
    #[test]
    fn on_is_the_opt_in_that_claims_shift_f10() {
        let both = cfg("tab_menu_chord = \"on\"").tab_menu_chord_or_default();
        assert_eq!(both, TabMenuChord::On);
        assert!(both.claims_menu_key());
        assert!(both.claims_shift_f10());
    }

    /// `menu_key` hands Shift+F10 back while keeping the Menu key.
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

    /// Case-insensitive, trimmed, dash alias; an unknown value warns and falls
    /// back to `MenuKey`, never `On`.
    #[test]
    fn parsing_is_forgiving_and_invalid_falls_back_to_menu_key() {
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
            TabMenuChord::MenuKey
        );
        assert_eq!(
            cfg("tab_menu_chord = \"\"").tab_menu_chord_or_default(),
            TabMenuChord::MenuKey
        );
        assert_eq!(TabMenuChord::parse("nope"), None);
        assert_eq!(TabMenuChord::parse("off"), Some(TabMenuChord::Off));
    }
}

#[cfg(test)]
mod window_colorspace_tests {
    use super::{Config, WindowColorspace};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
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

    /// The accepted token set (case-insensitive, trimmed, `display-p3` aliases);
    /// anything else falls back to `Srgb` with a warning.
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
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// Unset keys resolve to exactly the built-in constants (12 / 2), so the
    /// config-aware padding paths agree with the constant paths.
    #[test]
    fn window_padding_defaults_to_builtin_constants() {
        let c = Config::default();
        assert_eq!(c.window_padding_or_default(), crate::PAD_LOGICAL_PX);
        assert_eq!(c.window_padding_top_or_default(), crate::PAD_TOP_LOGICAL_PX);
    }

    /// The resolver clamps to 0..=64 and maps non-finite values to the default.
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

    /// The top override clamps to `0..=window_padding` at the resolver, and the
    /// 2.0 default is capped by a smaller base pad.
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

    /// Config padding flows through `MetricsView::for_scale_padded` at the
    /// scales the reload path re-resolves, and equal resolved padding means no
    /// re-grid is attempted.
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

    /// A post-boot config reload runs `refresh_all_window_metrics`; the
    /// headless boot seal (pad and `$ATERM_HEADROOM_PX` head) must survive it so
    /// later recordings and captures keep the band.
    #[test]
    fn headless_boot_band_survives_a_delta_config_reload() {
        let mut app = crate::App::headless_for_test();
        app.font_px_explicit = false; // auto font ⇒ the W12 activate arm is live
        // Boot seam: apply the `--scale` pad and `$ATERM_HEADROOM_PX` head to the
        // backend (test backend boots at 0/0), then seal them as boot does.
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

    /// A synthetic `ScaleFactorChanged` to a never-attached window keeps its
    /// sealed boot band. winit never delivers this to a headless window today.
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

    /// The seal copies the applied boot values verbatim, so the record and the
    /// shared backend agree on headless geometry.
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
        aterm_toml::from_str(toml).expect("valid toml")
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
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// Dedupe equality is semantic: two parses of the same content, or a
    /// comment/whitespace-only edit, compare equal; a one-key edit compares
    /// unequal.
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

    /// Path-feed fingerprints track file content: stable while untouched, moved
    /// by an edit or deletion, and split per consumer (lexicon → `deco`, trail
    /// manifest → `trail`).
    #[test]
    fn path_feed_fingerprints_track_referenced_file_content() {
        let dir = std::env::temp_dir().join(format!("aterm-feed-fp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lex = dir.join("lexicon.toml");
        let pack = dir.join("trail-pack.toml");
        std::fs::write(&lex, "one").unwrap();
        std::fs::write(&pack, "alpha").unwrap();
        // Paths go through the crate's escaper; a raw Windows path would be an
        // invalid `\U` escape.
        let config = cfg(&format!(
            "cursor_trail_packs = [{}]\n[sparkle_words]\nlexicon = {}\n",
            super::toml_basic_string(&pack.to_string_lossy()),
            super::toml_basic_string(&lex.to_string_lossy())
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

    /// A path generation is one transaction per consumer: a file replaced after
    /// the same-handle read cannot relabel the admitted bytes (Tier-1 projection
    /// of the path-feed snapshot model).
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
                    // Replace the file after the bounded read; parsing and
                    // fingerprinting must both use the returned v1 string.
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

    /// Byte-equal reload (`refresh_path_feeds`): (1) nothing changed is a full
    /// no-op with no word-deco reset; (2) a lexicon file edit re-arms the
    /// recompute and hard-resets word decorations; (3) a trail-pack edit re-arms
    /// the recompute without touching decorations.
    #[test]
    fn byte_equal_reload_still_rereads_touched_path_feed_files() {
        let dir = std::env::temp_dir().join(format!("aterm-feed-touch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lex = dir.join("lexicon.toml");
        let pack = dir.join("trail-pack.toml");
        std::fs::write(&lex, "# lexicon v1\n").unwrap();
        std::fs::write(&pack, "# pack v1\n").unwrap();

        let mut app = crate::App::headless_for_test();
        // Escaped, not raw — see the sibling fingerprint test: a Windows
        // fixture path in a TOML basic string is an invalid escape sequence.
        app.config = cfg(&format!(
            "cursor_trail_packs = [{}]\n[sparkle_words]\nlexicon = {}\n",
            super::toml_basic_string(&pack.to_string_lossy()),
            super::toml_basic_string(&lex.to_string_lossy())
        ));
        // The startup/worker step: compile feeds and carry their fingerprints
        // in the same immutable generation; render only activates that bundle.
        app.prepared_sparkle = app.config.prepare_sparkle_runtime();
        app.path_feed_fps = app.config.path_feed_fingerprints();
        app.recompute_sparkle();
        assert!(!app.sparkle_dirty, "recompute clears the dirty latch");

        // Seed a scanned per-window state so a hard_reset is observable: after an
        // empty rescan at epoch 7 only a reset makes `needs_rescan(7)` true.
        let (deco_cfg, lexicon) = {
            let r = app.sparkle.as_ref().expect("sparkle resolves on defaults");
            (r.cfg.clone(), r.lexicon.clone())
        };
        let wid = crate::WindowId(0);
        let seed_scan = |app: &mut crate::App| {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.fx.decos_mut().rescan_from_cells(
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
                !ws.fx.decos().needs_rescan(7),
                "probe seeded: scanned at epoch 7"
            );
        };
        seed_scan(&mut app);

        // (1) Byte-equal config + untouched files ⇒ the FULL no-op survives.
        let fresh = app.config.path_feed_fingerprints();
        app.refresh_path_feeds(fresh);
        assert!(!app.sparkle_dirty, "nothing changed ⇒ no recompute re-arm");
        assert!(
            !app.windows[&wid].fx.decos().needs_rescan(7),
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
            app.windows[&wid].fx.decos().needs_rescan(7),
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
            !app.windows[&wid].fx.decos().needs_rescan(7),
            "a trail-only edit never resets word decorations"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `deco_feed_changed`: a lexicon file edit behind an unrelated config edit
    /// still hard-resets (the table stores only the path).
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
}

#[cfg(test)]
mod whole_cell_min_size_tests {
    //! Wayland snaps interactive resizes to `min_inner_size + k·increment`, so
    //! the min must lie on the whole-cell frame lattice or a vertical drag
    //! shaves a column.

    #[test]
    fn min_size_sits_on_the_whole_cell_frame_lattice() {
        let app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let min = app.whole_cell_min_size(wid);
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        let base_h =
            app.win_head(wid) + app.win_pad_top(wid) + pad + usize::from(app.tab_strip_rows) * ch;

        // Every exact frame `2·pad + C·cw` at or above the min is a lattice point
        // of `min + k·cw`.
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
    /// `apply_theme_live` recolours the live backend without a rebuild and drops
    /// the strip-row cache and per-window `RepaintKey` gate.
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
        app.windows.get_mut(&wid).unwrap().last_strip_fp = Some((
            0xFEED,
            80,
            false,
            None,
            false,
            None,
            crate::tab_bar::VersionSlotCols::NONE,
            false,
        ));

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
    //! `[matrix_rain]` resolver tests: defaults and clamps live only in
    //! `Config::matrix_rain_params`, the enabled bit resolves separately
    //! (`Config::matrix_rain_enabled`, default off), and malformed hue input
    //! fails closed without aborting the load.

    use super::{
        Config, PRIVACY_PROBE_INTERVAL_MS_DEFAULT, PRIVACY_WARMUP_HOLD_MS_DEFAULT,
        PRIVACY_WARMUP_HOLD_MS_MAX, PrivacyWarmup,
    };
    use crate::matrix_rain::{RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR, RainConfig, RainHue};

    /// Stock dark-theme chrome, as the renderer knows it (`0x00RR_GGBB`).
    const BG: u32 = 0x0011_1318;
    const FG: u32 = 0x00D0_D0D0;

    /// The enabled-gated composition of `matrix_rain_enabled` and
    /// `matrix_rain_params`, used by the parameter tests below.
    fn rain(toml: &str) -> Option<RainConfig> {
        let cfg = aterm_toml::from_str::<Config>(toml).expect("valid toml");
        cfg.matrix_rain_enabled()
            .then(|| cfg.matrix_rain_params(BG, FG))
    }

    /// Off by default: an absent table, a table without `enabled`, and
    /// `enabled = false` all resolve false.
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

    /// Parameters resolve independently of the enabled bit, so a session forcing
    /// rain on over a disabled config gets the same field as `enabled = true`.
    #[test]
    fn params_resolve_independent_of_enabled() {
        let disabled = aterm_toml::from_str::<Config>("[matrix_rain]\nenabled = false\nfps = 24")
            .unwrap()
            .matrix_rain_params(BG, FG);
        let enabled = aterm_toml::from_str::<Config>("[matrix_rain]\nenabled = true\nfps = 24")
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

    /// `[packages]`: an absent or empty table is the default (loop runs, toolset
    /// installs); `enabled = false` or legacy `auto_update = false` stops the
    /// loop; `channel`/`include` are ignored unknown keys; the rest, including
    /// `[packages.links]`, round-trips.
    #[test]
    fn packages_switch_resolves_defaults_and_the_retired_spellings() {
        assert!(
            Config::default().packages_enabled(),
            "absent [packages] table ⇒ the loop runs"
        );
        assert!(
            Config::default().packages_auto_install(),
            "batteries included"
        );
        let empty = aterm_toml::from_str::<Config>("[packages]").unwrap();
        assert!(empty.packages_enabled());
        let off = aterm_toml::from_str::<Config>("[packages]\nenabled = false").unwrap();
        assert!(!off.packages_enabled(), "the switch off ⇒ no loop");
        let retired = aterm_toml::from_str::<Config>("[packages]\nauto_update = false").unwrap();
        assert!(
            !retired.packages_enabled(),
            "the retired auto_update = false still reads as off"
        );
        // Settings explains an off held by the legacy key, in atpkg's words, but
        // not when `enabled = false` is set beside it.
        assert_eq!(
            retired.packages_retired_switch_note(),
            Some(atpkg::config::auto_update_note(false, None))
        );
        let both =
            aterm_toml::from_str::<Config>("[packages]\nenabled = true\nauto_update = false")
                .unwrap();
        assert!(!both.packages_enabled());
        assert_eq!(
            both.packages_retired_switch_note(),
            Some(atpkg::config::auto_update_note(false, Some(true)))
        );
        for text in [
            "[packages]\nenabled = false\nauto_update = false",
            "[packages]\nauto_update = true",
            "[packages]\nenabled = false",
            "",
        ] {
            let config = aterm_toml::from_str::<Config>(text).unwrap();
            assert_eq!(config.packages_retired_switch_note(), None, "{text:?}");
        }
        let seed = aterm_toml::from_str::<Config>("[packages]\nseed_install = false").unwrap();
        assert!(
            !seed.packages_auto_install(),
            "the retired seed_install = false still keeps a machine bare"
        );
        let full = aterm_toml::from_str::<Config>(concat!(
            "[packages]\nenabled = true\nauto_install = false\n",
            "account = \"alabsystems\"\nchannel = 7\ninclude = \"x\"\n",
            "exclude = [\"trust\"]\n",
            "[packages.links]\nay = \"~/ay\"\norc = \"alabsystems/orc\"\n"
        ))
        .unwrap();
        assert!(full.packages_enabled());
        assert!(!full.packages_auto_install());
        let p = full.packages.as_ref().expect("table parsed");
        assert_eq!(p.account.as_deref(), Some("alabsystems"));
        assert_eq!(p.exclude.as_deref(), Some(["trust".to_string()].as_slice()));
    }

    /// The GUI `Config` and atpkg's `PackagesConfig` resolve identical defaults
    /// and explicit values for the shared `[packages]` keys, legacy spellings
    /// included; `[reroute]` is pinned the same way.
    #[test]
    fn packages_defaults_agree_across_the_two_config_readers() {
        for text in [
            "",
            "[packages]\n",
            "[packages]\nenabled = false\n",
            "[packages]\nauto_update = false\n",
            "[packages]\nenabled = true\nauto_update = false\n",
            "[packages]\nauto_update = true\n",
            "[packages]\nauto_install = false\n",
            "[packages]\nseed_install = false\n",
            "[packages]\nseed_install = false\nauto_install = true\n",
            "[packages]\nseed_install = true\nauto_install = false\n",
        ] {
            let gui = aterm_toml::from_str::<Config>(text).unwrap();
            let pkg = atpkg::config::parse_packages(text);
            assert_eq!(gui.packages_enabled(), pkg.enabled(), "enabled: {text:?}");
            assert_eq!(
                gui.packages_auto_install(),
                pkg.auto_install(),
                "auto_install: {text:?}"
            );
        }
        let defaults = atpkg::PackagesConfig::default();
        assert!(
            defaults.enabled() && defaults.auto_install(),
            "both default ON"
        );
        for text in [
            "",
            "[reroute]\nannounce = true\n",
            "[reroute]\nannounce = false\n",
            "[reroute]\nstrict = true\n",
            "[reroute]\nannounce = false\nstrict = false\n",
        ] {
            let gui = aterm_toml::from_str::<Config>(text).unwrap();
            assert_eq!(
                gui.reroute
                    .as_ref()
                    .and_then(|r| r.announce)
                    .unwrap_or(true),
                atpkg::config::parse_reroute(text).announce(),
                "reroute.announce: {text:?}"
            );
            assert_eq!(
                gui.reroute.as_ref().and_then(|r| r.strict).unwrap_or(false),
                atpkg::config::parse_reroute(text).strict(),
                "reroute.strict: {text:?}"
            );
        }
    }

    /// `[update] enabled` resolves the same in the GUI and in
    /// `aterm_update_core::settings`, default on.
    #[test]
    fn update_enabled_agrees_with_the_updaters_own_reader() {
        for text in [
            "",
            "[update]\n",
            "[update]\nenabled = true\n",
            "[update]\nenabled = false\n",
        ] {
            let gui = aterm_toml::from_str::<Config>(text).unwrap();
            assert_eq!(
                gui.update.as_ref().and_then(|u| u.enabled).unwrap_or(true),
                aterm_update_core::settings::parse_update_enabled(text),
                "{text:?}"
            );
        }
    }

    /// `[update] require_team_id` is honoured in development builds, which
    /// includes test builds.
    #[test]
    fn a_required_team_id_is_honoured_by_a_development_build() {
        let config =
            aterm_toml::from_str::<Config>("[update]\nrequire_team_id = \"ABCDE12345\"\n").unwrap();
        assert_eq!(super::update_required_team_id(&config), Some("ABCDE12345"));
    }

    /// `[machine]` defaults: Universal Control off and cargo output hidden from
    /// Spotlight.
    #[test]
    fn machine_absent_section_resolves_documented_defaults() {
        let c = Config::default();
        assert_eq!(
            c.machine_universal_control(),
            atpkg::config::UniversalControlPolicy::Off
        );
        assert!(c.machine_spotlight_noindex());
        assert!(c.machine.is_none());
    }

    /// An EMPTY `[machine]` table is exactly the absent one (every field is
    /// `Option`, so the table's presence alone changes nothing).
    #[test]
    fn machine_empty_table_resolves_the_same_as_an_absent_one() {
        let empty = aterm_toml::from_str::<Config>("[machine]").expect("empty table parses");
        let absent = Config::default();
        assert!(empty.machine.is_some(), "the table itself parsed");
        assert_eq!(
            empty.machine_universal_control(),
            absent.machine_universal_control()
        );
        assert_eq!(
            empty.machine_spotlight_noindex(),
            absent.machine_spotlight_noindex()
        );
    }

    /// Explicit values win over the defaults; an unrecognised policy spelling
    /// resolves to the inert `Leave`, never to a write on the machine.
    #[test]
    fn machine_explicit_values_win_over_the_defaults() {
        let c = aterm_toml::from_str::<Config>(concat!(
            "[machine]\n",
            "universal_control = \"leave\"\n",
            "spotlight_noindex = false\n",
        ))
        .unwrap();
        assert_eq!(
            c.machine_universal_control(),
            atpkg::config::UniversalControlPolicy::Leave
        );
        assert!(!c.machine_spotlight_noindex());
        let off =
            aterm_toml::from_str::<Config>("[machine]\nuniversal_control = \"off\"\n").unwrap();
        assert_eq!(
            off.machine_universal_control(),
            atpkg::config::UniversalControlPolicy::Off
        );
        let bogus =
            aterm_toml::from_str::<Config>("[machine]\nuniversal_control = \"bogus\"\n").unwrap();
        assert_eq!(
            bogus.machine_universal_control(),
            atpkg::config::UniversalControlPolicy::Leave,
            "an unknown spelling is the inert reading"
        );
    }

    /// The GUI `Config` and atpkg's `MachineConfig` resolve identical defaults and
    /// explicit values for `[machine]`.
    #[test]
    fn machine_defaults_agree_across_the_two_config_readers() {
        let gui = Config::default();
        let pkg = atpkg::config::MachineConfig::default();
        assert_eq!(gui.machine_universal_control(), pkg.universal_control());
        assert_eq!(gui.machine_spotlight_noindex(), pkg.spotlight_noindex());
        for text in [
            "[machine]\nuniversal_control = \"leave\"\nspotlight_noindex = false\n",
            "[machine]\nuniversal_control = \"off\"\nspotlight_noindex = true\n",
            "[machine]\nuniversal_control = \"\"\n",
            "[machine]\n",
            "",
        ] {
            let gui = aterm_toml::from_str::<Config>(text).unwrap();
            let pkg = atpkg::config::parse_machine(text);
            assert_eq!(
                gui.machine_universal_control(),
                pkg.universal_control(),
                "{text:?}"
            );
            assert_eq!(
                gui.machine_spotlight_noindex(),
                pkg.spotlight_noindex(),
                "{text:?}"
            );
        }
    }

    /// `[privacy]` defaults: lane, probe, notice and attribution on; warm-up
    /// offered but never automatic; folders are the consent module's, in walk
    /// order.
    #[test]
    fn observer_mode_defaults_to_sentinel_false_is_off_true_is_full() {
        use crate::consent_observer::ObserverMode;
        let mode = |text: &str| {
            aterm_toml::from_str::<Config>(text)
                .expect("parses")
                .privacy_observer_mode()
        };
        assert_eq!(mode(""), ObserverMode::Sentinel, "no [privacy] table");
        assert_eq!(mode("[privacy]\n"), ObserverMode::Sentinel, "key absent");
        assert_eq!(mode("[privacy]\nobserver = false\n"), ObserverMode::Off);
        assert_eq!(mode("[privacy]\nobserver = true\n"), ObserverMode::Full);
    }

    #[test]
    fn privacy_absent_section_resolves_every_documented_default() {
        let c = Config::default();
        assert!(c.privacy_enabled(), "master default on");
        assert!(c.privacy_check(), "silent probe default on");
        assert!(c.privacy_notice(), "notice default on");
        assert!(c.privacy_report_attribution(), "attribution default on");
        assert!(
            c.privacy_probe_gate().permits(),
            "both bits on ⇒ the gate permits a probe"
        );
        assert_eq!(c.privacy_warmup(), PrivacyWarmup::OnRequest);
        assert_eq!(c.privacy_warmup().as_str(), "on-request");
        assert_eq!(
            c.privacy_warmup_folders(),
            aterm_containment::Folder::WARMUP_DEFAULT
                .iter()
                .map(|folder| folder.as_str().to_string())
                .collect::<Vec<_>>(),
            "the default folder list IS the consent module's, not a copy"
        );
        assert_eq!(c.privacy_warmup_hold_ms(), PRIVACY_WARMUP_HOLD_MS_DEFAULT);
        assert_eq!(
            c.privacy_probe_interval_ms(),
            PRIVACY_PROBE_INTERVAL_MS_DEFAULT
        );
        // The reserved key has no resolver at all — nothing in the build can
        // read it, which is what makes "unimplemented" structural rather than
        // a promise. It still round-trips so the validator can name it.
        assert_eq!(
            c.privacy_observer_mode(),
            crate::consent_observer::ObserverMode::Sentinel,
            "the tccd observer defaults to the narrow sentinel (design §3.6)"
        );
        assert_eq!(c.privacy.as_ref().and_then(|p| p.auto_accept), None);
    }

    /// `app-data` is a valid warm-up name but not a default one: macOS forgets
    /// its Allow when the process that received it is replaced, so asking for it
    /// up front raises a dialog whose answer cannot last.
    #[test]
    fn the_default_warmup_omits_app_data_but_an_explicit_list_may_include_it() {
        let absent = Config::default();
        let defaults = absent.privacy_warmup_folders();
        assert_eq!(defaults, vec!["documents", "desktop", "downloads"]);
        assert!(
            !defaults.iter().any(|name| name == "app-data"),
            "{defaults:?}"
        );
        assert!(
            aterm_containment::Folder::ALL.contains(&aterm_containment::Folder::AppData),
            "the roster still knows the item (reset, report rows)"
        );
        let explicit = aterm_toml::from_str::<Config>(
            "[privacy]\nwarmup_folders = [\"documents\", \"app-data\"]\n",
        )
        .expect("explicit list parses");
        assert_eq!(
            explicit.privacy_warmup_folders(),
            vec!["documents".to_string(), "app-data".to_string()]
        );
        let paths = aterm_containment::consent::folder_paths(&explicit.privacy_warmup_folders());
        assert_eq!(
            paths.len(),
            2,
            "an explicit app-data still resolves to a path"
        );
    }

    /// An EMPTY `[privacy]` table is exactly the absent one: every field is
    /// `Option`, so serde fills none of them and the resolvers answer the same
    /// defaults. (The table's mere presence must not change behavior — that is
    /// the trap `#[serde(default)]` on a struct of non-optional fields sets.)
    #[test]
    fn privacy_empty_table_resolves_the_same_as_an_absent_one() {
        let empty = aterm_toml::from_str::<Config>("[privacy]").expect("empty table parses");
        let absent = Config::default();
        assert!(empty.privacy.is_some(), "the table itself parsed");
        assert_eq!(empty.privacy_enabled(), absent.privacy_enabled());
        assert_eq!(empty.privacy_check(), absent.privacy_check());
        assert_eq!(empty.privacy_notice(), absent.privacy_notice());
        assert_eq!(
            empty.privacy_report_attribution(),
            absent.privacy_report_attribution()
        );
        assert_eq!(empty.privacy_warmup(), absent.privacy_warmup());
        assert_eq!(
            empty.privacy_warmup_folders(),
            absent.privacy_warmup_folders()
        );
        assert_eq!(
            empty.privacy_warmup_hold_ms(),
            absent.privacy_warmup_hold_ms()
        );
        assert_eq!(
            empty.privacy_probe_interval_ms(),
            absent.privacy_probe_interval_ms()
        );
        assert_eq!(
            empty.privacy_observer_mode(),
            absent.privacy_observer_mode()
        );
        assert_eq!(
            empty.privacy_protected_roots(),
            absent.privacy_protected_roots()
        );
    }

    /// An absolute root for the fixtures below; on Windows an absolute path
    /// needs a drive. Forward slashes work on both hosts.
    const ABS_TMP: &str = if cfg!(windows) { "C:/tmp" } else { "/tmp" };

    /// Explicit values win, the table round-trips, an over-ceiling hold is
    /// clamped, and an unknown `warmup` spelling falls back.
    #[test]
    fn privacy_explicit_values_win_over_the_defaults() {
        // Positional `{}`: a format string expanded from `concat!` may not
        // capture variables inline.
        let c = aterm_toml::from_str::<Config>(&format!(
            concat!(
                "[privacy]\n",
                "enabled = false\n",
                "check = false\n",
                "notice = false\n",
                "report_attribution = false\n",
                "warmup = \"never\"\n",
                "warmup_folders = [\"desktop\"]\n",
                "warmup_hold_ms = 45000\n",
                "probe_interval_ms = 250\n",
                "observer = true\n",
                "protected_roots = [\"{}/aterm-privacy-explicit\"]\n",
                "auto_accept = true\n",
            ),
            ABS_TMP
        ))
        .expect("explicit table parses");
        assert!(!c.privacy_enabled());
        assert!(!c.privacy_check());
        assert!(!c.privacy_notice());
        assert!(!c.privacy_report_attribution());
        assert!(
            !c.privacy_probe_gate().permits(),
            "either bit off ⇒ the gate refuses before any syscall"
        );
        assert_eq!(c.privacy_warmup(), PrivacyWarmup::Never);
        assert_eq!(c.privacy_warmup_folders(), vec!["desktop".to_string()]);
        assert_eq!(c.privacy_warmup_hold_ms(), 45_000);
        assert_eq!(c.privacy_probe_interval_ms(), 250);
        assert_eq!(
            c.privacy_observer_mode(),
            crate::consent_observer::ObserverMode::Full,
            "the full stream is opt-in from config, and config alone"
        );
        assert_eq!(
            c.privacy_protected_roots(),
            vec![std::path::PathBuf::from(format!(
                "{ABS_TMP}/aterm-privacy-explicit"
            ))],
            "an absolute override replaces the containment set verbatim"
        );
        assert_eq!(
            c.privacy.as_ref().and_then(|p| p.auto_accept),
            Some(true),
            "the reserved key round-trips so --validate-config can refuse it"
        );

        // The clamp, and the fallback, each on their own.
        let over = aterm_toml::from_str::<Config>("[privacy]\nwarmup_hold_ms = 3600000\n")
            .expect("parses");
        assert_eq!(
            over.privacy_warmup_hold_ms(),
            PRIVACY_WARMUP_HOLD_MS_MAX,
            "an hour-long hold is clamped, not honored"
        );
        let typo = aterm_toml::from_str::<Config>("[privacy]\nwarmup = \"first-launch\"\n")
            .expect("an unknown spelling must NOT fail the whole file");
        assert_eq!(
            typo.privacy_warmup(),
            PrivacyWarmup::OnRequest,
            "unknown spellings fall back; the validator names them"
        );
        assert_eq!(
            PrivacyWarmup::parse("first-launch"),
            None,
            "there is deliberately no first-launch value"
        );
        assert_eq!(PrivacyWarmup::parse(" NEVER "), Some(PrivacyWarmup::Never));
    }

    /// `protected_roots = []` resolves to the Containment private set, not to
    /// "nothing is protected".
    #[test]
    fn privacy_empty_protected_roots_resolve_to_the_containment_private_set() {
        let expected: Vec<std::path::PathBuf> = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .map(|home| {
                aterm_containment::sbpl::PRIVATE_SUBDIRS
                    .iter()
                    .map(|sub| home.join(sub))
                    .collect()
            })
            .unwrap_or_default();
        let explicit_empty =
            aterm_toml::from_str::<Config>("[privacy]\nprotected_roots = []\n").expect("parses");
        assert_eq!(explicit_empty.privacy_protected_roots(), expected);
        assert_eq!(
            Config::default().privacy_protected_roots(),
            expected,
            "an absent key resolves the same way an empty one does"
        );
        assert!(
            !expected.is_empty(),
            "this test is only meaningful with a $HOME; the resolver returns \
             empty for 'cannot answer', never for 'nothing is protected'"
        );
    }

    /// A `~/` override is expanded; an entry that is neither absolute nor
    /// `~`-prefixed is dropped (and `--validate-config` names it).
    #[test]
    fn privacy_protected_root_overrides_expand_tilde_and_drop_relatives() {
        let c = aterm_toml::from_str::<Config>(&format!(
            "[privacy]\nprotected_roots = [\"~/src\", \"relative/path\", \"{ABS_TMP}/absolute\"]\n"
        ))
        .expect("parses");
        let roots = c.privacy_protected_roots();
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let mut expected = Vec::new();
        if let Some(home) = home {
            expected.push(home.join("src"));
        }
        expected.push(std::path::PathBuf::from(format!("{ABS_TMP}/absolute")));
        assert_eq!(
            roots, expected,
            "the relative entry is dropped, not guessed"
        );
    }

    /// Defaults resolve exactly; alpha/head_alpha stay `None` (theme-derived)
    /// when unset, and the live theme passes through.
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

    /// `head_alpha` clamps to `alpha..=135`; with no body override the floor is
    /// the global alpha floor.
    #[test]
    fn head_alpha_floors_at_body_alpha() {
        let c = rain("[matrix_rain]\nenabled = true\nalpha = 120\nhead_alpha = 20").unwrap();
        assert_eq!(c.alpha_override, Some(120));
        assert_eq!(c.head_alpha_override, Some(120));
        let c = rain("[matrix_rain]\nenabled = true\nhead_alpha = 5").unwrap();
        assert_eq!(c.alpha_override, None);
        assert_eq!(c.head_alpha_override, Some(RAIN_ALPHA_FLOOR));
    }

    /// The three hue spellings parse; malformed hex or words fall back to the
    /// stock green without failing the resolve or the load.
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

    /// `materialize` / `ink_text` / `phosphor` parse without aborting the load
    /// but are not forwarded (`RainConfig` has no such fields).
    #[test]
    fn inert_v1_knobs_parse_without_forwarding() {
        let c = rain(concat!(
            "[matrix_rain]\nenabled = true\n",
            "materialize = true\nink_text = true\nphosphor = true\n"
        ))
        .expect("inert knobs never abort the resolve");
        assert_eq!(c.fps, 30, "the rest of the resolve is unaffected");
    }

    /// Nonzero seeds pass through; the 0 sentinel is resolved per window at
    /// engine build (`rain_config_for_window`).
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
        let c = aterm_toml::from_str::<Config>("[matrix_rain]\nenabled = true")
            .unwrap()
            .matrix_rain_params(0xFF11_1318, 0xFFD0_D0D0);
        assert_eq!(c.default_bg, 0x0011_1318);
        assert_eq!(c.theme_fg, 0x00D0_D0D0);
    }
}

#[cfg(test)]
mod matrix_rain_app_tests {
    //! Matrix rain host wiring: no engine exists when nothing enables rain; the
    //! per-session `toggle_matrix_rain` override (both directions,
    //! reload-surviving); the `rain` verb's status line; hot-reload rebuild;
    //! per-window seeds; and reduced motion emitting nothing (fp 0).

    use super::Config;
    use crate::keybinding::Action;

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// No config and no override: every session is off, no engine exists and
    /// the scratch stays empty. Parameters resolve but construct nothing.
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
            assert!(ws.fx.rain().is_none(), "no engine constructed when off");
            assert!(ws.fx.rain_empty());
        }
    }

    /// `toggle_matrix_rain` is a per-session override on the front session: on
    /// over a disabled config, off over an enabled one, and it wins over reloads
    /// until re-toggled. Engines are not dropped on the off flip.
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
        app.windows.get_mut(&wid).unwrap().fx.build_rain(rc);
        app.dispatch_action(wid, Action::ToggleMatrixRain);
        assert_eq!(app.pool.rain_override(0), Some(false));
        assert!(!app.session_rain_enabled(0));
        assert!(
            app.windows[&wid].fx.rain().is_some(),
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

    /// The `rain` verb ([`crate::App::rain_control`]): `status` reads only;
    /// `on`/`off`/`toggle` write the front session's override; every reply has
    /// the full post-state line.
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

    /// The `fx` verb ([`crate::App::fx_control`]): `status` is a pure read;
    /// `celebrate` arms once; a second arm is refused with the detector's
    /// sentence; a non-rainbow trail refuses. `live` stays false (no keyed edge).
    #[test]
    fn fx_control_verb_arms_once_and_reports() {
        let mut app = crate::App::headless_for_test();
        app.config = cfg("cursor_trail = true\ncursor_trail_style = \"rainbow kitty pet\"");
        let status = app
            .fx_control(crate::FxCtlOp::Status)
            .expect("status reads");
        assert!(
            status.starts_with(
                "armed=none sig=00000000 bars=0 live=false bar=none drive=0.00 cooldown_ms=0 style="
            ),
            "{status}"
        );
        assert!(
            status.ends_with("style=rainbow-kitty"),
            "positive fixture must enable the real style"
        );
        let sig = aterm_effects::kitty_sing::song_signature('C');
        let arm = app.fx_control(crate::FxCtlOp::Celebrate {
            sig,
            bars: 2,
            on: aterm_effects::kitty_sing::CelebrateOn::Green,
        });
        let line = arm.expect("the first arm is admitted");
        assert!(
            line.starts_with(&format!(
                "armed=green sig={sig:08x} bars=2 live=false bar=none drive=0.00 cooldown_ms="
            )),
            "{line}"
        );
        let again = app.fx_control(crate::FxCtlOp::Celebrate {
            sig,
            bars: 1,
            on: aterm_effects::kitty_sing::CelebrateOn::Enter,
        });
        assert_eq!(
            again.as_deref().map_err(String::as_str),
            Err("a celebration is already armed (one arm at a time)")
        );
        let status = app
            .fx_control(crate::FxCtlOp::Status)
            .expect("status reads");
        assert!(
            status.starts_with("armed=green"),
            "status is a pure read: {status}"
        );
    }

    #[test]
    fn fx_control_refuses_inactive_visual_policies_without_spending_an_arm() {
        for (config, refusal) in [
            (
                "cursor_trail = false\ncursor_trail_style = \"rainbow kitty pet\"",
                "cursor trail is disabled (nothing to celebrate with)",
            ),
            (
                "cursor_trail = true\ncursor_trail_style = \"laser\"",
                "cursor trail is not the rainbow kitty (nothing to celebrate with)",
            ),
            (
                "cursor_trail = true\ncursor_trail_style = \"rainbow kitty pet\"\nserious_mode = true",
                "Serious Mode suppresses cursor companions",
            ),
        ] {
            let mut app = crate::App::headless_for_test();
            app.config = cfg(config);
            app.apply_serious_mode(app.config.serious_mode_or_default());
            let arm = app.fx_control(crate::FxCtlOp::Celebrate {
                sig: aterm_effects::kitty_sing::song_signature('C'),
                bars: 2,
                on: aterm_effects::kitty_sing::CelebrateOn::Green,
            });
            assert_eq!(arm.as_deref().map_err(String::as_str), Err(refusal));
            let status = app
                .fx_control(crate::FxCtlOp::Status)
                .expect("status still reads");
            assert!(
                status.starts_with(
                    "armed=none sig=00000000 bars=0 live=false bar=none drive=0.00 cooldown_ms=0 "
                ),
                "refusal spends no arm or cooldown: {status}"
            );
        }
    }

    /// Hot reload re-resolves and pushes the new config into a live engine via
    /// `set_config`; a reload that disables rain keeps the engine for the render
    /// path to drain.
    #[test]
    fn hot_reload_dirty_rebuild() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.config = cfg("[matrix_rain]\nenabled = true\nfps = 24");
        app.rain_dirty = true;
        app.recompute_matrix_rain();
        assert_eq!(app.rain.unwrap().fps, 24);
        let rc = crate::rain_config_for_window(app.rain.unwrap(), wid);
        app.windows.get_mut(&wid).unwrap().fx.build_rain(rc);

        // A reload edits a knob (reload_config sets rain_dirty; mirrored here
        // without the disk round-trip).
        app.config = cfg("[matrix_rain]\nenabled = true\nfps = 60");
        app.rain_dirty = true;
        app.recompute_matrix_rain();
        assert_eq!(app.rain.unwrap().fps, 60, "reload re-resolves the knobs");
        assert!(
            app.windows[&wid].fx.rain().is_some(),
            "a live engine is reconfigured in place, not dropped"
        );

        // Disabling flips the effective bit but keeps the engine.
        app.config = cfg("");
        app.rain_dirty = true;
        app.recompute_matrix_rain();
        assert!(app.rain.is_some(), "params keep resolving");
        assert!(!app.session_rain_enabled(0), "config off, no override");
        assert!(
            app.windows[&wid].fx.rain().is_some(),
            "the engine is retained for the suspended/drain wind-down"
        );
    }

    /// `seed = 0` derives a stable per-window seed: equal across rebuilds of a
    /// window, distinct across windows; a nonzero seed passes through.
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

    /// Under reduced motion the engine emits nothing (empty channels, fp 0) and
    /// `is_active()` is false; it becomes active again when reduced lifts.
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

/// The auto-prime knob: batteries-on, a real off, and readable without the
/// side effects of a full config load.
#[cfg(test)]
mod agents_auto_prime_tests {
    use super::{Config, agents_auto_prime_from_text};

    #[test]
    fn agents_auto_prime_defaults_on_and_parses_off() {
        assert!(
            Config::default().agents_auto_prime_or_default(),
            "batteries on"
        );
        let off: Config = aterm_toml::from_str("agents_auto_prime = false").expect("valid config");
        assert_eq!(off.agents_auto_prime, Some(false));
        assert!(!off.agents_auto_prime_or_default());
        let on: Config = aterm_toml::from_str("agents_auto_prime = true").expect("valid config");
        assert!(on.agents_auto_prime_or_default());
    }

    #[test]
    fn the_knob_reads_without_a_full_config_load() {
        assert!(agents_auto_prime_from_text(""), "empty file: default");
        assert!(
            agents_auto_prime_from_text("tab_status = false"),
            "unrelated key: default"
        );
        assert!(!agents_auto_prime_from_text("agents_auto_prime = false"));
        assert!(agents_auto_prime_from_text("agents_auto_prime = true"));
        // A malformed file resolves the way `load_config` resolves it: defaults.
        assert!(agents_auto_prime_from_text("agents_auto_prime = "));
        assert!(agents_auto_prime_from_text("agents_auto_prime = \"no\""));
    }
}

/// `aterm_gui::configured_update_settings`: source and apply policy come from one
/// parse, and a malformed existing policy refuses automatic work.
#[cfg(test)]
mod update_check_settings_tests {
    #[test]
    fn persisted_check_settings_bind_source_and_manual_policy_in_one_parse() {
        let defaults = super::update_check_settings_from_text("").unwrap();
        assert!(defaults.auto_apply);
        assert_eq!(defaults.source, aterm_update::Source::resolve(None, None));
        let manual = super::update_check_settings_from_text(
            "[update]\nowner = \"private-org\"\nrepo = \"fork\"\nauto_apply = false\n",
        )
        .unwrap();
        assert!(!manual.auto_apply);
        assert_eq!(
            manual.source,
            aterm_update::Source::resolve(Some("private-org"), Some("fork"))
        );
        assert!(
            super::update_check_settings_from_text("[update]\nauto_apply = invalid").is_none(),
            "malformed existing policy must not fall back to automatic"
        );
    }
}

/// The enablement gate from the config string down: only `off` and a `pack:`
/// naming no loaded pack resolve to no style; everything else, a typo
/// included, draws.
#[cfg(test)]
mod trail_style_resolution_tests {
    use super::*;
    use aterm_effects::cursor_glow::{GlowConfig, GlowStyle};

    fn glow(raw: &str, enabled: bool) -> GlowConfig {
        let catalog = TrailPackCatalog::empty();
        resolve_cursor_glow(
            CursorGlowInputs {
                enabled,
                color: None,
                accent: None,
                duration_ms: 260,
                length: 24,
                intensity: 0.7,
                radius: 0.6,
                ring: true,
            },
            resolve_trail_presentation(raw, &catalog),
            0x00FF_FFFF,
            true,
            0x00C8_D3F5,
            0x001A_1B26,
            0.5,
        )
    }

    fn warning_for(raw: &str) -> Option<String> {
        Config {
            cursor_trail_style: Some(raw.to_string()),
            ..Config::default()
        }
        .cursor_trail_style_warning(&TrailPackCatalog::empty())
    }

    #[test]
    fn native_ribbon_resolution_keeps_tall_default_and_explicit_underline() {
        for (style, expected_tall) in [
            ("rainbow kitty", true),
            (crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE, true),
            ("rainbow kitty flying", true),
            ("rainbow kitty tall", true),
            ("rainbow kitty underline", false),
        ] {
            assert_eq!(
                glow(style, true).ribbon_tall,
                expected_tall,
                "native ribbon resolver returned the wrong geometry for {style:?}"
            );
        }
    }

    #[test]
    fn immutable_catalog_caches_the_complete_trail_presentation() {
        let config = Config {
            cursor_trail_style: Some("rainbow kitty underline".to_string()),
            ..Config::default()
        };
        let catalog = config.resolve_trail_pack_catalog();
        let cached = catalog.presentation_for(config.cursor_trail_style_raw());
        assert_eq!(cached.style.style, Some(GlowStyle::RainbowKitty));
        assert!(!cached.ribbon_tall);
        assert_eq!(
            cached.pet_species,
            Some(aterm_effects::kitty_pet::PetSpecies::Cat)
        );
        assert!(!cached.beam);
        assert!(!cached.comet);

        // Direct test/preview mutation bypasses publication. It must receive a
        // correct dynamic answer without changing the immutable generation.
        let fallback = catalog.presentation_for("rainbow dog pet");
        assert!(fallback.ribbon_tall);
        assert_eq!(
            fallback.pet_species,
            Some(aterm_effects::kitty_pet::PetSpecies::Dog)
        );
        assert_eq!(
            catalog
                .presentation_for(config.cursor_trail_style_raw())
                .pet_species,
            cached.pet_species
        );
    }

    /// Whether a fresh engine given this spelling's config owns the frame as v2
    /// on its first tick (`CursorGlow::v2_status` is `Some`).
    fn engine_is_v2_for(raw: &str) -> bool {
        use aterm_effects::cursor_glow::{CursorGlow, Geom};
        let cfg = glow(raw, true);
        let mut engine = CursorGlow::default();
        let mut out = Vec::new();
        engine.tick(
            Some((2, 4)),
            std::time::Instant::now(),
            &cfg,
            Geom {
                cw: 8,
                ch: 16,
                rows: 24,
                cols: 80,
                origin_x: 0,
                origin_y: 0,
                win_w: 640,
                win_h: 384,
                head: 0,
            },
            &mut out,
        );
        engine.v2_status().is_some()
    }

    /// Every `cursor_trail_style` resolving to `RainbowKitty` (picker entries,
    /// aliases, the default, a typo's fallback) engages v2 from the resolved
    /// config and uses the music box for `auto`. `… v1` is unknown and falls
    /// back with an `Unknown` issue; other styles, `off` and packs never engage
    /// v2. `Default` and the resolver agree.
    #[test]
    fn every_rainbow_kitty_spelling_is_v2() {
        use aterm_effects::trail_sound::SoundVoice;
        let catalog = TrailPackCatalog::empty();
        let default = crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE;
        assert_eq!(
            ResolvedTrailPresentation::default(),
            resolve_trail_presentation(default, &catalog),
            "`Default` must be the resolver's answer for the default spelling"
        );
        assert_eq!(
            resolve_trail_presentation(default, &catalog).style.style,
            Some(GlowStyle::RainbowKitty),
            "the default {default:?} is the rainbow kitty"
        );
        let auto = Config::default();
        let marimba = Config {
            trail_sound_style: Some("marimba".to_string()),
            ..Config::default()
        };
        let mut rainbow_kitties = 0;
        let spellings = crate::prefs::CURSOR_TRAIL_STYLES.iter().copied().chain(
            crate::prefs::CURSOR_TRAIL_STYLE_ALIASES
                .iter()
                .map(|&(alias, _)| alias),
        );
        for raw in spellings {
            let p = resolve_trail_presentation(raw, &catalog);
            assert_eq!(p.style.issue, None, "{raw:?} is a documented spelling");
            let is_kitty = p.style.style == Some(GlowStyle::RainbowKitty);
            rainbow_kitties += usize::from(is_kitty);
            assert_eq!(
                engine_is_v2_for(raw),
                is_kitty,
                "{raw:?}: the engine owns the frame as v2 iff the spelling is the rainbow kitty"
            );
            assert_eq!(
                auto.trail_sound_voice_for(p),
                if is_kitty {
                    SoundVoice::RainbowKittyV2
                } else {
                    SoundVoice::Style
                },
                "{raw:?}: `auto` is the music box iff the spelling is the rainbow kitty"
            );
            assert_eq!(
                marimba.trail_sound_voice_for(p),
                SoundVoice::Marimba,
                "{raw:?}: an explicit instrument is the user's"
            );
        }
        assert!(
            rainbow_kitties >= 20,
            "the picker and the alias table name the rainbow kitty {rainbow_kitties} ways"
        );
        // The old escape and every other unknown word: the default, reported.
        let fallback = resolve_trail_presentation(default, &catalog);
        for v1 in [
            "rainbow kitty v1",
            "RAINBOW KITTY V1",
            "rainbow kitty v1 pet",
            "rainbow kitty v1 flying",
            "rainbow kitty v1 underline",
            "rainbow kitty v1 tall",
            "kitty v1",
            "nyan v1",
            "rainbow v1",
            "rainbow kity v1",
            "rainbow kity v2",
            "v1",
            "v2",
            "",
        ] {
            let p = resolve_trail_presentation(v1, &catalog);
            assert_eq!(
                p.style.issue,
                Some(TrailStyleIssue::Unknown),
                "{v1:?} names no style and says so"
            );
            assert_eq!(
                ResolvedTrailStyle {
                    issue: None,
                    ..p.style
                },
                fallback.style,
                "{v1:?} falls back to the default"
            );
            assert!(
                engine_is_v2_for(v1),
                "{v1:?} draws the default, which is v2"
            );
            assert_eq!(auto.trail_sound_voice_for(p), SoundVoice::RainbowKittyV2);
        }
        // The generation cache agrees with the resolver, escape included.
        let config = Config {
            cursor_trail_style: Some("rainbow kitty v1".to_string()),
            ..Config::default()
        };
        let cached = config
            .resolve_trail_pack_catalog()
            .presentation_for(config.cursor_trail_style_raw());
        assert_eq!(cached.style.issue, Some(TrailStyleIssue::Unknown));
        assert_eq!(cached.style.style, Some(GlowStyle::RainbowKitty));
        assert_eq!(
            config.trail_sound_voice_for(cached),
            SoundVoice::RainbowKittyV2
        );
        // …and nothing else is v2 or the music box.
        for other in ["phaser", "comet", "lumen", "classic", "pack:v1", "pack:"] {
            let p = resolve_trail_presentation(other, &catalog);
            assert!(!engine_is_v2_for(other), "{other:?} must not engage v2");
            assert_eq!(
                auto.trail_sound_voice_for(p),
                SoundVoice::Style,
                "{other:?}"
            );
        }
        assert_eq!(
            resolve_trail_presentation("off", &catalog).style.style,
            None,
            "`off` resolves to no style at all"
        );
        assert_eq!(
            auto.trail_sound_voice_for(resolve_trail_presentation("off", &catalog)),
            SoundVoice::Style
        );
    }

    /// A mistyped style falls back to the default and reports the issue.
    #[test]
    fn an_unknown_style_draws_the_default_and_is_reported() {
        let default = crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE;
        let catalog = TrailPackCatalog::empty();
        for typo in ["phasr", "rainbow kity tall", "plasma", ""] {
            let resolved = resolve_trail_style(typo, &catalog);
            assert_eq!(resolved.canonical, Some(default), "{typo:?}");
            assert_eq!(resolved.style, Some(GlowStyle::parse(default)), "{typo:?}");
            assert_eq!(resolved.pack, None, "{typo:?}");
            assert_eq!(
                resolved.issue,
                Some(TrailStyleIssue::Unknown),
                "{typo:?} stays flagged — the fallback is not an endorsement"
            );
            assert!(glow(typo, true).enabled, "{typo:?} must still draw");

            // The companion and ribbon predicates read the fallback token, not
            // the typo.
            let token = effective_trail_style_token(typo);
            assert_eq!(token, default, "{typo:?}");
            assert!(GlowStyle::style_names_any_pet(token), "{typo:?}");
            assert_eq!(
                glow(typo, true).ribbon_tall,
                glow(default, true).ribbon_tall,
                "{typo:?}"
            );

            let warning = warning_for(typo).unwrap_or_else(|| panic!("{typo:?} must warn"));
            assert!(warning.contains("cursor_trail_style"), "{warning}");
            assert!(
                warning.contains(&format!("{typo:?}")),
                "the warning must name the offending value: {warning}"
            );
            assert!(
                warning.contains(default) && warning.contains("expected one of"),
                "the warning must name the fallback and the accepted set: {warning}"
            );
        }
    }

    /// The three ways the trail legitimately goes dark, none of which the
    /// fallback may re-light.
    #[test]
    fn off_the_master_switch_and_a_missing_pack_still_disable_the_trail() {
        let catalog = TrailPackCatalog::empty();
        for spelling in ["off", "OFF", "  off  "] {
            assert_eq!(resolve_trail_style(spelling, &catalog).style, None);
            assert_eq!(resolve_trail_style(spelling, &catalog).issue, None);
            assert!(!glow(spelling, true).enabled, "{spelling:?}");
            assert!(
                warning_for(spelling).is_none(),
                "{spelling:?} is not a typo"
            );
        }
        assert!(
            !glow(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE, false).enabled,
            "`cursor_trail = false` owns the whole effect"
        );
        for pack in ["pack:", "pack:  ", "pack:not-loaded"] {
            assert_eq!(resolve_trail_style(pack, &catalog).style, None, "{pack:?}");
            assert!(!glow(pack, true).enabled, "{pack:?}");
            assert!(warning_for(pack).is_some(), "{pack:?}");
        }
        // `off` is the ONLY builtin spelling that means off, and no alias adds
        // a second one.
        for &s in crate::prefs::CURSOR_TRAIL_STYLES {
            assert_eq!(
                resolve_trail_style(s, &catalog).style.is_none(),
                s == "off",
                "style {s:?}"
            );
        }
        for &(alias, _) in crate::prefs::CURSOR_TRAIL_STYLE_ALIASES {
            assert!(
                resolve_trail_style(alias, &catalog).style.is_some(),
                "alias {alias:?} must not disable the trail"
            );
        }
    }

    /// [`effective_trail_style_token`] returns what `resolve_trail_style`
    /// settled on.
    #[test]
    fn the_effective_token_never_disagrees_with_the_resolver() {
        let catalog = TrailPackCatalog::empty();
        let extra = ["phasr", "", "  ", "pack:", "pack:not-loaded", " NYAN "];
        let aliases = crate::prefs::CURSOR_TRAIL_STYLE_ALIASES
            .iter()
            .map(|&(alias, _)| alias);
        for raw in crate::prefs::CURSOR_TRAIL_STYLES
            .iter()
            .copied()
            .chain(aliases)
            .chain(extra)
        {
            let resolved = resolve_trail_style(raw, &catalog);
            assert_eq!(
                effective_trail_style_token(raw),
                resolved.canonical.unwrap_or(raw.trim()),
                "raw {raw:?}"
            );
        }
    }

    /// Bloom radius is per style (1.8 for the rainbow kitty, 2.2 otherwise), and
    /// `cursor_trail_bloom_radius` overrides all of them.
    #[test]
    fn the_bloom_radius_is_per_style_with_the_knob_as_the_override() {
        fn radius(style: &str, knob: Option<f32>) -> f32 {
            let config = Config {
                cursor_trail_style: Some(style.to_string()),
                cursor_trail_bloom_radius: knob,
                ..Config::default()
            };
            let catalog = config.resolve_trail_pack_catalog();
            config.cursor_trail_bloom_radius_or_default(
                catalog.presentation_for(config.cursor_trail_style_raw()),
            )
        }
        assert_eq!(radius("rainbow kitty", None), 1.8);
        assert_eq!(radius("rainbow kitty pet", None), 1.8);
        assert_eq!(radius("laser", None), 2.2);
        assert_eq!(radius("comet", None), 2.2);
        assert_eq!(radius("lumen", None), 2.2);
        assert_eq!(radius("rainbow kitty", Some(3.0)), 3.0);
        assert_eq!(radius("laser", Some(3.0)), 3.0);
        // The default spelling IS the rainbow kitty, so an unconfigured
        // style resolves to the tighter halo too.
        let config = Config::default();
        let catalog = config.resolve_trail_pack_catalog();
        assert_eq!(
            config.cursor_trail_bloom_radius_or_default(
                catalog.presentation_for(config.cursor_trail_style_raw())
            ),
            1.8
        );
    }
}

#[cfg(test)]
mod launch_notice_tests {
    /// The launch-time notice names the path, the problem and the consequence —
    /// "every setting is running at its default" (an invalid file still reads its
    /// `[harness]` lines, `Config::stand_in`, and says so).
    #[test]
    fn launch_config_notice_names_the_consequence_and_the_path() {
        let path = std::path::Path::new("/home/ana/.config/aterm/aterm.toml");
        let unreadable = super::launch_config_notice(
            path,
            super::LaunchConfigProblem::Unreadable(&"permission denied (os error 13)"),
        );
        assert!(
            unreadable.starts_with("aterm.toml could not be read at launch (permission denied"),
            "{unreadable}"
        );
        assert!(
            unreadable.contains("every setting is running at its default"),
            "{unreadable}"
        );
        assert!(
            unreadable.contains("/home/ana/.config/aterm/aterm.toml"),
            "{unreadable}"
        );
        let invalid = super::launch_config_notice(
            path,
            super::LaunchConfigProblem::Invalid(&"expected `=` at line 3"),
        );
        assert!(
            invalid.starts_with("aterm.toml is not a valid configuration (expected `=`"),
            "{invalid}"
        );
        assert!(
            invalid.contains("the window's settings except [harness] are at their defaults"),
            "{invalid}"
        );
        assert!(
            invalid.contains("Fix /home/ana/.config/aterm/aterm.toml"),
            "{invalid}"
        );
    }
}
