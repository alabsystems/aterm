// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Config subsystem: the `aterm.toml` model (`Config`), its loaders and
//! precedence resolvers (font px, force scale, grid size, tab-strip rows), and
//! the `App` methods that consume them (font, scale, backend, resize, reload,
//! fullscreen).

use aterm_core::terminal::{CursorStyle, Rgb};
use aterm_render::Theme;
use aterm_window::dpi::PhysicalSize;

use crate::input::{InputEvent, Source};
use crate::platform::AppRt;
use crate::{
    App, Backend, FONT_PX, FONT_PX_MAX, FONT_PX_MIN, PresentTarget, WindowId, keybinding, launch,
};

/// Platform default for decoration drawn over content: `true` everywhere except
/// Windows, whose defaults are minimal and fast (decorative GPU presents on the
/// keystroke path cost typing latency there).
///
/// It governs the cursor style's and the typing glow's defaults
/// ([`default_cursor_trail_style`], [`Config::cursor_momentum_glow_or_default`]:
/// the resident cat is drawn over text). Keys that stay out: `cursor_trail_bloom`, `hdr_glow` and `cursor_fire_shimmer` already emit
/// nothing with the trail off; `[sparkle_words] enabled` draws under text and is
/// capped (see `Config::sparkle_words_enabled_or_default`); the remaining effects
/// are opt-in.
///
/// An explicit value in `aterm.toml` always wins: every consumer is
/// `Option::unwrap_or(DEFAULT_DECORATIVE_EFFECTS)`.
pub(crate) const DEFAULT_DECORATIVE_EFFECTS: bool = !cfg!(windows);

/// The cursor style an aterm.toml that names none draws: the default style
/// where decoration over content is on by default
/// ([`DEFAULT_DECORATIVE_EFFECTS`]), else Off.
pub(crate) fn default_cursor_trail_style() -> &'static str {
    if DEFAULT_DECORATIVE_EFFECTS {
        crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE
    } else {
        "off"
    }
}

/// The cursor style an aterm.toml draws from its `cursor_trail_style`
/// (`style`) and the retired `cursor_trail` master (`master`), trimmed: the
/// one fold [`Config::cursor_trail_style_raw`] and the writer
/// (`prefs::apply_prefs_edits`, which keeps the style Off replaced) both
/// read. `master = false` is Off whatever the style says, and `master =
/// true` with no style names the default style; unset, the platform's
/// default ([`default_cursor_trail_style`]).
pub(crate) fn drawn_cursor_trail_style(master: Option<bool>, style: Option<&str>) -> &str {
    if master == Some(false) {
        return "off";
    }
    match style {
        Some(style) => style.trim(),
        None if master == Some(true) => crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
        None => default_cursor_trail_style(),
    }
}

/// Platform default for `line_height`, the cell-box multiplier: `1.1` on
/// Windows, `1.0` everywhere else.
///
/// Windows since 2026-10-16, with its default face, Iosevka Term Semibold, at
/// 17 px ([`crate::FONT_PX`]): on the face's own 1.25 em line (a 9x21 cell)
/// its capital accents start on the very row a `j` of the line above ends on
/// — the collision a judged render farm at 96 DPI marked against Iosevka on a
/// 1.0 line — and the 1.1 line (9x23, baseline row 17) puts two clear pixel
/// rows between them (render J2-lh11: no accent pixel touching a descender).
/// It applies to every face a Windows user picks; `line_height = 1.0` gives
/// back the face's own line, and with `font_family = "Cascadia Mono PL"` and
/// `font_px = 16` the 9x21 cell that was the Windows default before.
///
/// An explicit value in `aterm.toml` always wins: every consumer reads
/// [`Config::line_height_or_default`].
pub(crate) const DEFAULT_LINE_HEIGHT: f32 = if cfg!(windows) { 1.1 } else { 1.0 };

/// Platform default for the theme-pair contrast floor while `minimum_contrast`
/// is unset ([`Config::indexed_minimum_contrast_or_default`]): `4.5` on
/// Windows, where pwsh's directory names were the default foreground on palette
/// blue at 2.2:1 and Default Light's white-on-blue was 1.35:1; `1.0` (off)
/// everywhere else, which looks exactly as before.
pub(crate) const DEFAULT_INDEXED_MINIMUM_CONTRAST: f32 = if cfg!(windows) { 4.5 } else { 1.0 };

/// Whether panes show the overlay scrollbar when `scrollbar` is unset: on
/// Windows, where every Fluent scroll view (Windows Terminal's included) shows
/// one; macOS and Linux keep the brief scroll pill they had.
pub(crate) const DEFAULT_SCROLLBAR: bool = cfg!(windows);

/// User config file (`$XDG_CONFIG_HOME/aterm/aterm.toml`, else
/// `~/.config/aterm/aterm.toml`). Every field is optional; unknown keys are
/// ignored. Precedence is launch flag > config > built-in default; no environment
/// variable sets a key.
///
/// `PartialEq` lets config admission skip runtime side effects for a semantic
/// no-op; text, comments and asset generations are versioned separately by the
/// native config service.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct Config {
    /// Font size in px at 1× (points on macOS), the one font-size unit: each
    /// window draws it × its display scale (`--font-px` outranks it for one
    /// launch).
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
    /// Theme: single indexed palette slots, `#RRGGBB`, by 0-based index as the
    /// key, applied after `palette`: `palette_overrides = { 12 = "#88c0d0" }`.
    /// A slot not named keeps the theme's colour, so one tuned slot follows
    /// every theme instead of pinning the slots before it.
    pub(crate) palette_overrides: Option<std::collections::BTreeMap<String, String>>,
    /// Motion policy: `"auto"` (default: follow live macOS/Windows preferences,
    /// sample GNOME's animation preference on Linux when available), `"full"`
    /// or `"reduced"`. Governs every decorative animation
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
    /// RETIRED: the old cursor-effects master, which gated both the trail and
    /// the typing glow behind a key Settings had no control for. Still read,
    /// folded into its two successors: `false` reads as `cursor_trail_style =
    /// "off"` and `cursor_momentum_glow = false` whatever they say, `true` as
    /// their defaults where they are unset ([`Self::cursor_trail_style_raw`],
    /// [`Self::cursor_momentum_glow_or_default`]). Settings drops it from the
    /// file when it writes either successor (`prefs::apply_prefs_edits`).
    pub(crate) cursor_trail: Option<bool>,
    /// Typing-momentum glow: the cursor glows with typing speed, cools when
    /// typing stops, and does not blink while warm. On every style, `off`
    /// included; its default follows [`DEFAULT_DECORATIVE_EFFECTS`] (owner,
    /// 2026-09-08).
    pub(crate) cursor_momentum_glow: Option<bool>,
    /// Trail style: one of the Cursor Styles rows — `off`, `rainbow kitty pet`
    /// (default: rainbow ribbon plus the full-body cat; `rainbow kitty`, bare
    /// `kitty` and the `… tall` spellings are aliases of it), `rainbow kitty
    /// underline`, `rainbow dog pet`, `rainbow kitty flying` (the ribbon under
    /// the flying kitty head; also `flying kitty`, `kitty flying`, `nyan
    /// rainbow`, `nyan`, `rainbow`), `rainbow kitty flat`, `phaser`, `fire`,
    /// `water`, `laser` (listed as Lightning; also `lightning`), `comet`,
    /// `sparkle`, `lumen`, `beam`, `classic` (the old `classic mono` spellings
    /// are aliases of it that keep its one-colour face). The vocabulary is
    /// `aterm_effects::cursor_glow::spellings`.
    ///
    /// Rainbow kitty styles draw the engine in `docs/design/RAINBOW-KITTY-V2.md`.
    /// Unknown spellings (the retired `… v2`, `… v1` and `pack:` ones
    /// included) fall back to the default, and the fallback is reported
    /// (`--validate-config`, the load-time warning, `trail status`'s
    /// `resolved=`).
    pub(crate) cursor_trail_style: Option<String>,
    /// Music effects (Settings ▸ Sound): the master switch for every synth
    /// voice (`sound_policy::voice_gain`). Default on. Each style's typing
    /// notes play softly on the spawn edge that lights the trail, so only they
    /// need a drawn trail; which voices also follow focus or a reduced motion
    /// policy is the voice table's (`sound_policy::Voice`).
    /// `trail_sounds = false` mutes all of them.
    pub(crate) trail_sounds: Option<bool>,
    /// Trail sound volume, `0.0`–`1.0` (default `0.4`: a lone keystroke peaks
    /// around −22 dBFS). Settings ▸ Sound ▸ "Sound volume".
    pub(crate) trail_sound_volume: Option<f32>,
    /// Trail sound ambient bed (default on): the per-style background texture
    /// behind fast typing. `false` gates the bed mixer entirely (zero samples
    /// rendered); discrete notes and the melody are unaffected.
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
    /// Sing-along riff (default on): `false` schedules no riff bar for the
    /// held-key celebration (`aterm_effects::kitty_sing`), the loudest sound the
    /// engine emits, while other voices keep their levels. Sound only; the
    /// celebration's visuals follow the motion policy. Focus, `trail_sounds` and
    /// `trail_sound_volume` apply first.
    pub(crate) trail_sound_riff: Option<bool>,
    /// Audible terminal bell (default on). BEL rings the system alert sound
    /// (`NSBeep` on macOS, `MessageBeep` on Windows), outside the trail synth's
    /// volume controls. `false` suppresses the beep only; the visual bell and the
    /// attention request stay. Inert off macOS/Windows, and in a `--headless`
    /// instance, which never beeps (`App::on_bell`).
    pub(crate) bell_sound: Option<bool>,
    /// Trail colour, `#RRGGBB`. Unset, the style's automatic colour
    /// (`aterm_effects::cursor_fx::automatic_glow_color`): laser, beam, comet
    /// and sparkle their own hue (beam, comet and sparkle wear a configured
    /// `cursor_color` or a program's OSC 12), every other style the live cursor
    /// colour. Where it lands is `GlowStyle::reads_knob`: Classic draws its
    /// one-colour face in it; Fire, Water, Phaser and the rainbow styles wear
    /// it only on a bar cursor's light rod.
    pub(crate) cursor_trail_color: Option<String>,
    /// Secondary/accent colour, `#RRGGBB`. Where it lands is
    /// `GlowStyle::reads_knob`: Lumen and Comet shade their wake with it on a
    /// dark ground, and their landing ring; Classic its one-colour face and
    /// its ring; Phaser and Sparkle only the landing ring; the other looks
    /// never read it. Defaults to the style's automatic colour (or a trail
    /// colour the style draws) brightened 1.5×.
    pub(crate) cursor_trail_accent: Option<String>,
    /// Path to a PNG sprite for the flying cat on the `rainbow kitty flying`
    /// style (RGBA or RGB, ideally small right-facing pixel art), nearest-scaled
    /// to the cursor. Unset uses the built-in sprite. `~` expands to $HOME.
    /// The key keeps its `nyan` spelling so existing configs still apply.
    pub(crate) cursor_nyan_sprite: Option<String>,
    /// Wallpaper image drawn behind every terminal tab (not native tabs).
    /// Cover-scaled, toned toward the theme background by
    /// [`wallpaper_dim`](Self::wallpaper_dim), and visible through cells with the
    /// default background. PNG/JPEG decode everywhere; additional formats use
    /// the system decoder on macOS and Windows. `~` expands to $HOME.
    /// Unset means no wallpaper.
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
    /// Bloom-crown radius in cells (lumen styles); 0 disables the crown. Default 0.6.
    pub(crate) cursor_trail_radius: Option<f32>,
    /// Landing-ring "ping" on a jump (lumen styles). Default true.
    pub(crate) cursor_trail_ring: Option<bool>,
    /// GPU-only cursor bloom: a soft gaussian halo composited at present time.
    /// The CPU path is unaffected. Default on.
    pub(crate) cursor_trail_bloom: Option<bool>,
    /// Bloom strength: how much of the blurred glow is added back (0.0..=3.0).
    /// Default 0.85; higher is more radiant (very high can blow out toward white).
    pub(crate) cursor_trail_bloom_strength: Option<f32>,
    /// Bloom radius: how far the halo spreads, in half-res texels (0.5..=8.0).
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
    /// is byte-identical to the non-HDR path (aterm-gpu's `hdr_gate` suite).
    /// Hot-reloadable: off stops >1.0 emission immediately; on lets capable
    /// surfaces upgrade at the next probe or re-query. See
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
    /// unechoed (password) input; the echo proof ends at every key it did not
    /// predict and whenever the screen's rows move. Every mode refuses local
    /// ECHO-off input (`sudo`, `read -s`, a passphrase). Explicit `always` can
    /// briefly show unechoed input behind a remote raw-mode link until it is
    /// reconciled or expires; `adaptive` requires current-line echo proof.
    pub(crate) predictive_echo: Option<String>,
    /// Predictive editing (default on): with `predictive_echo` on, a bare `←` or
    /// `→` (the main block's or the keypad's) and a Backspace at the end of the
    /// line are predicted as edits of the line. In `adaptive` a Backspace shows
    /// at once on a line that has proven it echoes, and an arrow only once the
    /// shell has also echoed an arrow of that line exactly (an echoed Backspace
    /// proves no arrow; a cooked `cat` echoes `^[[D` and never shows one);
    /// `always` shows both at once, unproven, as it shows typing. Off, those keys
    /// are predicted as they were before (an arrow ends the proof, a Backspace
    /// cancels only a guess of its own). The kill switch.
    pub(crate) predictive_echo_editing: Option<bool>,
    /// Focus-linked shell priority (Windows): while a window is focused, its
    /// visible tab's shell processes and ConPTY hosts run above normal priority
    /// with EcoQoS off so keystroke echo is not starved; on blur they return to
    /// normal. Root shell only. Default on; hot-reloadable; no-op on Unix.
    pub(crate) focus_boost: Option<bool>,
    /// The colour theme by name: a built-in (case-insensitive), e.g.
    /// `theme = "Dracula"`, or a themes-folder id. It sets default fg/bg,
    /// cursor, selection and the ANSI 0–15 palette; the individual colour keys
    /// layer on top. An unknown name warns and uses the default. See
    /// [`aterm_types::scheme`] and `aterm help themes`.
    ///
    /// `theme = "dark:<name>,light:<name>"` follows the OS appearance, read
    /// independently of the window chrome ([`crate::os_appearance`]: macOS's
    /// application appearance, the XDG portal or gsettings on Linux, the
    /// system setting on Windows; a headless instance reads none). A missing
    /// side draws the named side's built-in pair (`dark:Gruvbox Dark` is
    /// Gruvbox Light on a light OS), else the named side itself. Unset (or a
    /// value that names no theme), it is Default on every appearance: aterm
    /// follows the OS only when a pair is written (Settings' Follow system
    /// switch writes `dark:Default,light:Default Light` over an unset theme).
    /// See [`Self::resolve_theme_name`].
    pub(crate) theme: Option<String>,
    /// Interactive shell (config `shell`): a program name or an absolute path. On
    /// Windows a name is resolved with smart discovery (`"bash"` finds Git for
    /// Windows' `bash.exe` off `%PATH%`; `"pwsh"`, `"cmd"`, `"wsl"`, `"nu"`
    /// resolve too; see `windows::shell`); on Unix it is looked up on `PATH`. Unset
    /// uses the platform default (Windows: pwsh, powershell, cmd; Unix: `$SHELL`,
    /// else the account's login shell). `--shell` overrides it.
    ///
    /// Shell integration: zsh/bash/fish/pwsh and `"wsl"` (bash login shell) get
    /// OSC 7 + OSC 133; `"cmd"` gets prompt marks and cwd only (no command text,
    /// exit code or `wait`); `"nu"` gets none. See
    /// `aterm_shell_integration::ShellType`.
    pub(crate) shell: Option<String>,
    /// Extra argv passed to the `shell` after argv[0] (config `shell_args`), e.g.
    /// `["-l", "-i"]` for a login+interactive bash — or, for `shell = "wsl"`,
    /// `wsl.exe`'s own options (`["-d", "Debian"]`). An integrated bash or pwsh
    /// gets them inside its integration argv
    /// (`aterm_shell_integration::merge_shell_args`). Ignored when `-e` runs a
    /// command. Unset → the bare interactive shell.
    pub(crate) shell_args: Option<Vec<String>>,
    /// Windows: the console HOST a new tab's pseudoconsole runs in (config
    /// `conpty`, `aterm_pty::ConsoleHostChoice`). Unset, empty or `"bundled"`
    /// (the default): the pinned `OpenConsole.exe` the Windows build ships
    /// beside aterm, else the Windows console host (never a copy of Windows
    /// Terminal's). `"inbox"`: the Windows console host, what every build
    /// before 2026-10-17 used. `"auto"`: the `OpenConsole.exe` beside aterm,
    /// else a copy of the installed Windows Terminal's (taken into
    /// `%LOCALAPPDATA%\aterm\conpty`, because an unpackaged process may not
    /// launch it inside `WindowsApps`), else the Windows console host. Under
    /// the default and `auto`, Git Bash and the other MSYS2/Cygwin programs run
    /// in OpenConsole too, their post-resize guard record written by
    /// `aterm.exe` as a second console client (`aterm_pty`'s `console_guard`),
    /// and keep the Windows console host only when no `aterm.exe` is beside
    /// aterm to write it. Any other value: the absolute path of an
    /// `OpenConsole.exe`, for every tab. A host that fails to start falls back
    /// to the Windows console host and the log says why once. Read at launch
    /// and on reload; a tab keeps the host it started with.
    /// Under OpenConsole a killed pager's alternate screen is left by the host
    /// and then by the engine from the switch it forwards
    /// (`crate::alt_screen_resync`), never locally as under conhost. Ignored off
    /// Windows.
    pub(crate) conpty: Option<String>,
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
    /// `--font` outranks it. A blank value (`""`) means aterm's own choice
    /// and still outranks a theme's `font_family` (this is how "Keep colours
    /// only" pins the built-in face).
    ///
    /// Every font key here (`font_family*`, `fallback_fonts`, `symbol_font`,
    /// `emoji_font`, `line_height`, `ligatures`, `font_features`,
    /// `font_synthetic_style`, `font_weight`, `font_size_scale`) may also be
    /// named by the theme; a key set here wins over the theme's as a whole
    /// value (an empty list blocks a theme's list).
    pub(crate) font_family: Option<String>,
    /// Display face (`display_font`): a bundled face id (`"pixel"`, `"chunky"`,
    /// `"engraved"`, `"bubble"`; [`aterm_render::DISPLAY_FACES`]). When set it
    /// outranks `font_family` and resolves as the virtual family `display:<id>`
    /// from embedded bytes. Hot-reloadable; `--font` still outranks it.
    pub(crate) display_font: Option<String>,
    /// Real bold face for SGR-bold cells: a family name or path, resolved like
    /// `font_family` and injected via the renderer's `set_bold_font`. Absent uses
    /// the discovered `-Bold` sibling or synthetic embolden; blank (`""`) means
    /// aterm's own choice too (and outranks a theme's).
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
    /// Colour emoji face: family name or path. Outranks discovery.
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
    /// `true` (default) makes Alt/Option send ESC-prefixed (Meta) sequences,
    /// except that on macOS an Option chord typing a printable ASCII character
    /// no key on the layout types bare or with Shift (German Option+L → `@`,
    /// French Option+( → `{`) types that character, as AltGr does on Linux;
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
    /// clamped to [`MAX_TAB_STRIP_ROWS`]. A Wayland window the compositor leaves
    /// undecorated keeps one row at 0: the strip is its header
    /// ([`crate::client_frame::strip_rows_with_header`]).
    pub(crate) tab_strip_rows: Option<u16>,
    /// In-grid tab band height on Windows and Linux: `"standard"` (default,
    /// [`TAB_BAND_STANDARD_LOGICAL_PX`]: a WinUI tab, or libadwaita's on Linux) or
    /// `"compact"` (the strip's cell rows plus top pad). The extra pixels are a
    /// synthetic chrome head band (`App::synthetic_strip_head_px`). Inert on
    /// macOS and when `tab_strip_rows = 0`; unknown values use the default.
    pub(crate) tab_band_height: Option<String>,
    /// Active tab background colour (`#RRGGBB`) in both the macOS toolbar and the
    /// in-grid strip; label ink flips black/white by luminance. Beats the theme's
    /// `active_tab_color`; absent uses the theme's, else the theme-derived raised
    /// card (the translucent system pill on macOS). Hot-reloadable.
    pub(crate) active_tab_color: Option<String>,
    /// The chrome accent (`#RRGGBB`): the active-tab rule, the message band's
    /// meter, Settings' selected rows, the find highlight's hue, and on macOS the
    /// native tab bar's accent. Beats the theme's `accent_color`; absent uses the
    /// theme's, else the cursor when it has colour, else the theme's ANSI blue or
    /// cyan. Each surface still holds it to 3:1. Hot-reloadable.
    pub(crate) accent_color: Option<String>,
    /// Split-pane dividers and the seams of the chrome bands (`#RRGGBB`). Beats
    /// the theme's `border_color`; absent steps the background 30% toward the
    /// foreground. Held to 1.5:1 where drawn. Hot-reloadable.
    pub(crate) border_color: Option<String>,
    /// Find's highlight on every match but the current one (`#RRGGBB`; the
    /// current match is the selection). Beats the theme's `search_match_color`;
    /// absent tints the background toward the accent, apart from the selection.
    /// Hot-reloadable.
    pub(crate) search_match_color: Option<String>,
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
    /// The warm spare shell a plain new tab adopts (`crate::spare`): `"auto"`
    /// (default; a parked shell, and one whose startup files already ran where
    /// the learned gate allows), `"parked"` (never a startup file before Cmd-T)
    /// or `"off"`. Hot-reloaded; an unknown value reads `auto` and
    /// `--validate-config` names it.
    pub(crate) spare_shell: Option<String>,
    /// Whether a spare exists only after a person's gesture in the last ten
    /// minutes (default true). `false` is for headless measurement and agents'
    /// private instances; every safety gate still applies.
    pub(crate) spare_shell_needs_person: Option<bool>,
    /// Whether a classified status sets the tab's busy/attention marks. Default
    /// on; off keeps classification (the `status` verb, tooltip) running.
    pub(crate) tab_status_badge: Option<bool>,
    /// Whether a session's connection role draws the fourth tab mark (▲ outbound,
    /// ▽ inbound, hourglass both). Default on; off hides only the mark.
    pub(crate) tab_connection_badge: Option<bool>,
    /// Whether aterm posts its own notices (escalation herald, consent
    /// attention, update health) as OS notifications, only for what needs
    /// the owner now: the herald posts one notice per need
    /// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §3), never for a wait or a
    /// record; the status item's Notify Me When I'm Needed flips it. Default
    /// OFF (the owner, 2026-09-28: "I don't know what these OSX alerts are
    /// from aterm but they are annoying, disable them"; the lead kept it off
    /// on 2026-10-18): the owner opts in with Notify Me, whose hover says
    /// whose banner they would arrive as here — aterm's own once macOS lets
    /// it post as itself (turning Notify Me on asks, as does the first notice
    /// while this is on), none until then or once it says no, none from a
    /// bundle aterm cannot name (one under ~/Desktop, ~/Documents or
    /// ~/Downloads: moved to /Applications, it posts as itself), and, not run
    /// from an app at all (a bare binary), through terminal-notifier, else
    /// Script Editor's (`notify::own_voice`). When
    /// off, the facts remain in the menu bar, tab marks, message band and
    /// `messages.log`. Program notifications (OSC 9/99/777) are governed by
    /// `allow_notifications`. Applied live (`notify::spawn_delivery`).
    pub(crate) desktop_alerts: Option<bool>,
    /// Whether the window's launch and each fresh session spawn also install or
    /// update the coding-agent primer and bundled skills for every detected agent
    /// (the `aterm agents install` upsert, on a detached thread, at most once a
    /// minute; the launch's pass, once its first frame is on glass, covers the
    /// tabs an update hands over). Default
    /// on. `false` stops it entirely, which is what makes `aterm agents remove`
    /// stick.
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
    /// `"light"` (vertical only), or `"off"`. The retired `"native"` renders as
    /// `"light"`, with a config warning. Unknown spellings mean `"full"`. Inert
    /// on macOS. Hot-reloadable (drops the glyph atlas).
    pub(crate) font_hinting: Option<String>,
    /// Linux subpixel text: `"off"` (default, grayscale), `"rgb"` or `"bgr"`.
    /// CPU compositor only; it falls back to grayscale under translucency, a
    /// wallpaper, or non-primary-family glyphs. Unknown spellings mean `"off"`.
    /// Inert on macOS and Windows. Hot-reloadable.
    pub(crate) font_subpixel: Option<String>,
    /// Line-height multiplier on the cell box, without changing glyph size;
    /// leading splits half above, half below. Clamped 0.8..=2.0; absent means
    /// [`DEFAULT_LINE_HEIGHT`] (`1.1` on Windows, `1.0` elsewhere).
    /// Hot-reloadable.
    pub(crate) line_height: Option<f32>,
    /// Text size scale: multiplies the resolved font size (`font_px`, or the
    /// automatic default) without making it explicit, so the size still
    /// follows the display. Clamped 0.8..=1.25; absent means `1.0`; a value
    /// outside the range is clamped and said on the unaccepted-values row. A
    /// live zoom (Cmd +/-) is an absolute size and is not scaled; Cmd-0
    /// returns to the scaled default. A theme may set it too; this key wins.
    /// Hot-reloadable.
    pub(crate) font_size_scale: Option<f32>,
    /// Signed px shift of every glyph baseline, for faces whose metrics sit off.
    /// Clamped ±32; absent means `0`.
    pub(crate) adjust_baseline: Option<i64>,
    /// Signed px shift of the underline top (positive is down). Clamped ±32;
    /// absent means `0`.
    pub(crate) adjust_underline_position: Option<i64>,
    /// Underline thickness escape hatch: fatten/thin the resolved underline by a
    /// signed px delta. Clamped ±32; absent means `0`.
    pub(crate) adjust_underline_thickness: Option<i64>,
    /// Skip underline coverage within 1px of the cell's glyph ink so descenders
    /// are not struck through. Absent means `true`.
    pub(crate) underline_skip_descenders: Option<bool>,
    /// Per-cell minimum WCAG contrast ratio: glyph fg, decorations and the
    /// cursor fill are floored against their actual bg. Clamped 1.0..=21.0;
    /// `1.0` is off. Absent is off too, except on Windows, where text whose
    /// colours both come from the theme is held to 4.5:1 on a coloured cell
    /// and 3:1 on the theme's background
    /// ([`Config::indexed_minimum_contrast_or_default`]); a value set here
    /// replaces that with this floor on every cell. Hot-reloadable.
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
    /// The overlay scrollbar in each pane's right gutter
    /// (`crate::scrollbar`): hidden at the live bottom, a thin thumb while
    /// scrolled or scrolling, wider and draggable under the pointer. Absent
    /// means [`DEFAULT_SCROLLBAR`] (`true` on Windows, `false` elsewhere);
    /// `false` keeps the brief scroll pill. Hot-reloadable.
    pub(crate) scrollbar: Option<bool>,
    /// Render the cursor cell per-cell so the block cursor never sits on a
    /// ligature (`LigatureMode::CursorDisabled`). Absent means `false`; ignored
    /// when `ligatures = false`.
    pub(crate) cursor_break_ligatures: Option<bool>,
    /// SGR 1 promotes indexed colors 0–7 to bright 8–15. Absent means `true`;
    /// `false` keeps bold a pure weight change.
    pub(crate) bold_is_bright: Option<bool>,
    /// Where slots 16-255 of the palette come from
    /// ([`crate::prefs::PALETTE_GENERATE_MODES`]): `"theme"` (or absent)
    /// follows the theme's own `palette_generate` key, `"always"` derives them
    /// from every theme's colours, `"never"` keeps the xterm cube and grey
    /// ramp. An unknown value warns at load and follows the theme.
    pub(crate) palette_generate: Option<String>,
    /// SGR 2 (faint): fraction of the foreground kept, blended toward the cell
    /// background in linear light, held to at least 3:1 at the default (less
    /// when the colour itself is under 4.3:1), and always fainter than the
    /// colour. Clamped 0.0..=1.0; absent means `0.5`.
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
    /// Hot-reloadable.
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
    /// under aterm's identity (`UNUserNotificationCenter` once macOS allows
    /// it, and only that from an app bundle, which posts none where it has no
    /// center, as under ~/Desktop, ~/Documents or ~/Downloads;
    /// `terminal-notifier` or `osascript` from a bare binary; see
    /// `notify.rs`), hence the opt-in. Never for a Containment tab
    /// (`spawn::host_reach`).
    pub(crate) allow_notifications: Option<bool>,
    /// Let programs set the 256 indexed colours (OSC 4 / numeric OSC 21).
    /// Default ON ([`Config::allow_palette_reconfigure_or_default`]); `false`
    /// refuses them and files one `terminal.palette-refused` digest line per
    /// run. Program colours last for the session, are never saved, and never
    /// reach chrome.
    pub(crate) allow_palette_reconfigure: Option<bool>,
    /// Allow Kitty graphics non-direct mediums (`t=f` file, `t=t` temp file,
    /// `t=s` POSIX shm) to read host files. Default off (an exfiltration
    /// surface). When on, a size-capped resolver is installed
    /// (`spawn::configure_kitty_file_transfer`), never for a Containment tab
    /// (`spawn::host_reach`).
    pub(crate) allow_kitty_file_transfer: Option<bool>,
    /// Run the per-session temporal recorder read by the `temporal` control verb.
    /// Default off (no writer thread, no retention). When on, each tab seeds a t0
    /// keyframe and taps its reader so the session can be replayed.
    pub(crate) temporal_recording: Option<bool>,
    /// Fabric bridge (`[fabric]`): the `aterm-link serve …` child holding the far
    /// ends of two socketpairs at fds 3 and 4, speaking `Scope::Bridge`. Absent
    /// means no fabric. Development builds may override it with
    /// `ATERM_FABRIC_COMMAND`. See [`FabricConfig`], [`crate::fabric_launch`].
    ///
    /// No Settings row, on purpose: which program gets the bridge authority is a
    /// launcher intent, not a preference, and a Settings row is one an
    /// Owner-scope client can drive through `settings set`. The point of putting
    /// `deliver`/`hold` behind a descriptor is that no token unlocks them, and
    /// `fabric attach` arms only the command this key names
    /// (`fabric_launch::command_on_disk`): a writable `fabric.command` would hand
    /// the Owner token the choice back, for this process and every later launch.
    /// Edit `[fabric] command` in aterm.toml (validated by `--validate-config`),
    /// or run `aterm fabric on`.
    pub(crate) fabric: Option<FabricConfig>,
    /// The fleet listener (`[fleet]`): the opt-in TLS port another member of this
    /// machine's fleet reads it through (design §5). Absent means no fleet port. See
    /// [`FleetConfig`], [`crate::fleet_listen`].
    ///
    /// NO SETTINGS ROW, on purpose, like `[fabric]`: a Settings row is one an
    /// Owner-scope client can drive through `settings set`, and opening a network port
    /// is the person's act (`aterm fleet init|join` writes it), never an in-session
    /// agent's. Edit `[fleet]` in aterm.toml.
    pub(crate) fleet: Option<FleetConfig>,
    /// Self-update (`[update]`): whether the updater runs, how a staged build
    /// applies, and (development builds only) which GitHub repo it pulls from.
    /// Absent means on, from the public `alabsystems/aterm` channel. No
    /// environment variable overrides it. The pinned Team ID and notarization are
    /// the trust anchor, not the location. Effective on macOS only. See
    /// [`UpdateConfig`], crate `aterm-update`, `docs/RELEASING.md`.
    pub(crate) update: Option<UpdateConfig>,
    /// Sparkle words (`[sparkle_words]`): sparkle matched profanity and give
    /// cat/kitty words a cat-paw. Absent means both are on. See [`SparkleWordsConfig`].
    pub(crate) sparkle_words: Option<SparkleWordsConfig>,
    /// Matrix rain (`[matrix_rain]`) under the text, in empty cells only. Absent
    /// means off. See [`MatrixRainConfig`].
    pub(crate) matrix_rain: Option<MatrixRainConfig>,
    /// Presence (`[presence]`): the driver band under the tab bar and the colour
    /// rim, each toggled from the View menu. Absent means both on. See
    /// [`PresenceConfig`].
    pub(crate) presence: Option<PresenceConfig>,
    /// The Claude Code footer's switches (`[claude_footer]`): what aterm writes
    /// into the rule under Claude Code's input box. Absent ⇒ `Σ` shown. See
    /// [`ClaudeFooterConfig`].
    pub(crate) claude_footer: Option<ClaudeFooterConfig>,
    /// Render repair (`[render]`): what aterm does about a full-screen Claude
    /// Code tab whose screen a resize of its own window left displaced. See
    /// [`RenderConfig`]. Read once at launch.
    pub(crate) render: Option<RenderConfig>,
    /// The output streak (`[output_streak]`,
    /// `docs/DESIGN-output-streak-2026-08-30.md`): a thin per-theme rainbow comet
    /// answering fresh program output, plus one soft pip per episode. Absent ⇒ on
    /// except on Windows ([`DEFAULT_DECORATIVE_EFFECTS`]); `enabled = false` opts
    /// out. See [`OutputStreakConfig`].
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
    /// macOS host settings (`[machine]`): Universal Control off by default.
    /// Applied by `atpkg` (`aterm pkg machine apply`); the GUI only displays and
    /// edits it. See [`MachineConfig`].
    pub(crate) machine: Option<MachineConfig>,
    /// System awareness (`[sys]`, `docs/DESIGN-system-awareness-2026-10-16.md`):
    /// the stats bars. Absent means the Deck shows. See [`SysConfig`].
    pub(crate) sys: Option<SysConfig>,
    /// PTY keeper (`[keeper]`, `docs/DESIGN-pty-keeper-2026-09-26.md`): register
    /// every terminal's master with the keeper so a crash or kill does not hang
    /// up its shells and the next launch reattaches them. Absent means off. Read
    /// at launch. See [`KeeperConfig`].
    pub(crate) keeper: Option<KeeperConfig>,
    /// The session log's disk journal and tab clause (`[session_log]`,
    /// `docs/DESIGN-session-log-2026-10-15.md` §5.3, §7.3): absent means the
    /// journal follows the desk and `restore_session` (D3, ruled on). Read
    /// at launch. See [`SessionLogConfig`].
    pub(crate) session_log: Option<SessionLogConfig>,
    /// The `[harness]` policy ([`HarnessPolicy`]), read from the file's whole
    /// text rather than deserialized (it also accepts dotted `harness.<key>`
    /// lines under other tables and non-TOML text); set in [`Config::parse`].
    #[serde(skip)]
    pub(crate) harness: HarnessPolicy,
    /// The `[ai]` table and the fleet document it resolves over ([`AiPolicy`],
    /// docs/DESIGN-ai-connections-2026-10-16.md §6), read by the AI layer's own
    /// lenient reader, the one `aterm ai` uses; set in [`Config::parse`]. Live:
    /// an edit re-runs the window's look (`App::ai_config_changed`).
    #[serde(skip)]
    pub(crate) ai: AiPolicy,
}

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

/// Pack an [`Rgb`] as the renderer's `0x00RRGGBB`.
pub(crate) fn rgb_u32(c: Rgb) -> u32 {
    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
}

/// An ordered font list (`fallback_fonts`, `font_variation`) that deserializes
/// from either a TOML array of strings (`["A", "B"]`) or one comma-separated
/// string (`"A, B"`, the form the Settings text editor writes, so a Save
/// round-trips through serde). Entries are trimmed; empties dropped.
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
/// Sparkle Words (non-feline word effects) and Keyword Kitties (cat/kitty
/// effects). Purely visual: never affects copied text, logs or recorded sessions.
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
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleWordsConfig {
    /// Master switch. Absent ⇒ on; `false` ⇒ no matching, no decorations.
    pub(crate) enabled: Option<bool>,
    /// Languages whose ambiguous homograph entries are un-gated (`["all"]` = every
    /// language). Non-ambiguous forms always load. Default `["en"]`.
    pub(crate) languages: Option<Vec<String>>,
    /// Force the static, non-twinkling path. Default false. The explicit user
    /// override; the OS Reduce Motion flag is folded in per frame through
    /// `MotionPolicy` (app_render).
    pub(crate) reduced_motion: Option<bool>,
    /// Suppress decorations on the alternate screen. Default false (full-screen
    /// TUIs decorate like the primary screen).
    pub(crate) suppress_in_alt_screen: Option<bool>,
    /// External lexicon TOML (`[[entry]]` blocks) merged over the builtin. `~` and
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
    /// The feline (cat-paw) sub-table.
    pub(crate) feline: Option<SparkleFelineConfig>,
    /// The typed-word dog cameo sub-table.
    pub(crate) canine: Option<SparkleCanineConfig>,
    /// The typed kitty-command sub-table (`sit`, `kitty jump`, `good kitty`).
    pub(crate) tricks: Option<SparkleTricksConfig>,
    /// The animated glyph-ink shimmer sub-table.
    pub(crate) ink: Option<SparkleInkConfig>,
    /// The emphasis / hype-word class sub-table (ink-only).
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

/// Worst-case I/O for one path-feed fingerprint pass: one lexicon and eight
/// Toy Packs, each reader taking at most one sentinel byte past its cap.
#[cfg(test)]
const MAX_PATH_FEED_OPEN_ATTEMPTS: usize = 1 + MAX_ACTIVE_TOY_PACKS;
#[cfg(test)]
const MAX_PATH_FEED_FINGERPRINT_READ_BYTES: usize =
    (aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES + 1)
        + MAX_ACTIVE_TOY_PACKS * (aterm_effects::spec::MAX_TOY_PACK_BYTES + 1);

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
/// reaches the event loop; only the launch's kitty-command vocabulary may land
/// after it ([`TrickVocabulary`]).
#[derive(Clone)]
pub(crate) struct PreparedSparkleRuntime {
    resolved: Option<crate::word_decorations::Resolved>,
    /// Kitty-command vocabulary (`aterm_lexicon::TrickLexicon`), compiled off the
    /// keystroke path from the same `[sparkle_words] languages` as the word
    /// lexicon. Kept outside [`Resolved`](crate::word_decorations::Resolved) so
    /// it neither keeps the word engine alive nor disappears with it; compiled
    /// regardless of the master switch (dispatch checks it). Replaced per
    /// generation; the input path borrows it only at word boundaries.
    tricks: TrickVocabulary,
}

/// One generation's kitty-command vocabulary: compiled before the generation is
/// handed over ([`Self::compiled`]), or still compiling on its own worker
/// ([`Self::compiling`]). The launch takes the second, so the `tricks.toml`
/// parse, for a table only a typed pet command reads, stays out of the
/// config-runtime worker the boot joins before the first window
/// (`startup_prepare_config_join_ms`). Until it lands the generation answers
/// the empty vocabulary (`TrickLexicon::default`), which classifies nothing.
#[derive(Clone)]
struct TrickVocabulary(std::sync::Arc<std::sync::OnceLock<aterm_lexicon::TrickLexicon>>);

impl TrickVocabulary {
    fn compiled(langs: Vec<String>) -> Self {
        let vocabulary = Self(std::sync::Arc::default());
        let _ = vocabulary.0.set(compile_trick_vocabulary(&langs));
        vocabulary
    }

    /// Compile on an `aterm-tricks-compile` worker and land in place, or inline
    /// when no thread can start. `Background`: nothing waits on it — the input
    /// path reads whatever has landed and never blocks.
    fn compiling(langs: Vec<String>) -> Self {
        let vocabulary = Self(std::sync::Arc::default());
        let landing = std::sync::Arc::clone(&vocabulary.0);
        let worker_langs = langs.clone();
        let spawned = std::thread::Builder::new()
            .name("aterm-tricks-compile".into())
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                #[cfg(test)]
                let _held = TRICKS_COMPILE_GATE
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = landing.set(compile_trick_vocabulary(&worker_langs));
            });
        if spawned.is_err() {
            let _ = vocabulary.0.set(compile_trick_vocabulary(&langs));
        }
        vocabulary
    }

    fn get(&self) -> &aterm_lexicon::TrickLexicon {
        static EMPTY: std::sync::OnceLock<aterm_lexicon::TrickLexicon> = std::sync::OnceLock::new();
        self.0
            .get()
            .unwrap_or_else(|| EMPTY.get_or_init(aterm_lexicon::TrickLexicon::default))
    }
}

/// Held for writing by a test, the gate every [`TrickVocabulary::compiling`] worker
/// passes before it compiles: the compile is held where it runs, so the test sees a
/// generation handed over before its vocabulary lands, whatever the machine's speed.
#[cfg(test)]
static TRICKS_COMPILE_GATE: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// The embedded kitty-command vocabulary for `langs`. Its data problems indicate
/// bad embedded data, never user error.
fn compile_trick_vocabulary(langs: &[String]) -> aterm_lexicon::TrickLexicon {
    let refs: Vec<&str> = langs.iter().map(String::as_str).collect();
    let tricks = aterm_lexicon::TrickLexicon::with_languages(&refs);
    for conflict in tricks.conflicts() {
        crate::logging::stderr_line!("aterm-gui: kitty commands vocabulary: {conflict}");
    }
    tricks
}

/// One consistent generation of every path-backed effect feed. Each consumer
/// and its fingerprint derive from the same admitted bytes, so a path swap
/// cannot label stale data as current.
#[derive(Clone)]
pub(crate) struct PreparedPathFeedGeneration {
    pub(crate) sparkle: PreparedSparkleRuntime,
    pub(crate) fingerprints: PathFeedFps,
}

impl PreparedSparkleRuntime {
    /// This generation's kitty-command vocabulary, borrowed for one press: the
    /// empty one while the launch's compile has not landed.
    pub(crate) fn tricks(&self) -> &aterm_lexicon::TrickLexicon {
        self.tricks.get()
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

/// The trail presentation resolved once per config generation, shared by `Arc`
/// between the live host and Settings so redraw does not rescan the style
/// vocabulary.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrailStyleCache {
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

/// Bounded read for the configured wallpaper image file (a photo is an ordinary
/// source, so this is far above the kitty sprite's 8 MiB).
pub(crate) const MAX_WALLPAPER_FILE_BYTES: usize = 64 * 1024 * 1024;
/// Reject a wallpaper whose decoded dims exceed this per side (the allocation
/// bound; a 5K/6K screenshot fits comfortably).
pub(crate) const MAX_WALLPAPER_SOURCE_DIMENSION: usize = 8192;
/// Downscale an admitted wallpaper so neither side exceeds this (the resident
/// memory bound — still larger than any window it will be cover-scaled to).
pub(crate) const MAX_WALLPAPER_KEEP_DIMENSION: usize = 4096;

/// Installed-theme discovery keeps a bounded, deterministic prefix and reads
/// every file under a hard byte limit. This many files are opened and parsed;
/// files that never reach a read (a name shadowing a built-in, an unsafe or
/// non-UTF-8 name) only get a diagnostic row and do not count against it.
pub(crate) const MAX_USER_THEME_FILES: usize = aterm_types::scheme::MAX_USER_THEME_FILES;
/// `.conf` candidates kept, sorted, from the folder scan (the lowest names
/// win). The config watcher's directory stamp shares this cap, so a stamp never
/// sees an edit discovery ignores.
pub(crate) const MAX_USER_THEME_CANDIDATES: usize = 512;
/// Directory entries inspected before enumeration stops. Discovery keeps the
/// candidates gathered up to that point and marks the catalog truncated.
pub(crate) const MAX_USER_THEME_DIRECTORY_ENTRIES: usize = 4_096;
pub(crate) const MAX_USER_THEME_FILE_BYTES: usize = aterm_types::scheme::MAX_USER_THEME_FILE_BYTES;

/// The three bounds one theme-folder scan keeps: directory entries inspected,
/// `.conf` candidates kept, files read. [`Self::PRODUCTION`] everywhere but a
/// test that lays down a folder past one of them at a size it can write.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ThemeScanCaps {
    pub(crate) entries: usize,
    pub(crate) candidates: usize,
    pub(crate) files: usize,
}

impl ThemeScanCaps {
    pub(crate) const PRODUCTION: Self = Self {
        entries: MAX_USER_THEME_DIRECTORY_ENTRIES,
        candidates: MAX_USER_THEME_CANDIDATES,
        files: MAX_USER_THEME_FILES,
    };
}
const MAX_THEME_SOURCE_ID_BYTES: usize = 2 * 1024;
const MAX_THEME_REASON_BYTES: usize = 320;

#[derive(Clone, Debug, PartialEq, Eq)]
enum ThemeAssetResolution {
    /// Parsed and admitted (colours, identity and typography); boxed because
    /// the package is many times the row.
    Ready(Box<aterm_types::scheme::ThemePackage>),
    Invalid(std::sync::Arc<str>),
    /// The file's name is a built-in's (ASCII case-insensitive, `Default`
    /// included). The built-in wins; the row only says why the file is unused.
    Shadowed(&'static str),
    /// The file's stem is not UTF-8, so no config value can name it; the row
    /// carries the lossy spelling.
    Unreadable,
}

/// Admit a parsed theme file whose id (file stem) is `id`: an omitted
/// `appearance` is measured by the chrome's one classifier.
/// The community pack theme `name` names (ASCII case ignored), parsed and
/// admitted as the catalog admits a file, once per process and only when
/// first asked for. The pack is embedded (`aterm_theme_pack`), so this reads
/// no file. `None` for a name the pack does not have, or a pack file that
/// does not parse.
pub(crate) fn pack_package(
    name: &str,
) -> Option<(&'static str, &'static aterm_types::scheme::ThemePackage)> {
    static PACK: std::sync::OnceLock<
        Box<[std::sync::OnceLock<Option<aterm_types::scheme::ThemePackage>>]>,
    > = std::sync::OnceLock::new();
    let themes = aterm_theme_pack::themes();
    let slots = PACK.get_or_init(|| themes.iter().map(|_| std::sync::OnceLock::new()).collect());
    let index = themes
        .iter()
        .position(|theme| theme.id.eq_ignore_ascii_case(name))?;
    let theme = &themes[index];
    slots[index]
        .get_or_init(|| {
            aterm_types::scheme::parse_theme_str(theme.text)
                .ok()
                .map(|parsed| admit_theme_file(parsed, theme.id))
        })
        .as_ref()
        .map(|package| (theme.id, package))
}

fn admit_theme_file(
    parsed: aterm_types::scheme::ParsedTheme,
    id: &str,
) -> aterm_types::scheme::ThemePackage {
    let measured = measured_appearance(parsed.scheme.background);
    aterm_types::scheme::ThemePackage::admit(parsed, id, measured)
}

/// A loaded theme file's label wherever it is listed (the command palette's
/// Theme section, Settings' theme pickers): its own `name =` verbatim, and the
/// id config writes after [`crate::native_ui::FACT_SEPARATOR`] where they
/// differ, so two files that share a name stay apart.
pub(crate) fn theme_file_label(id: &str, meta: &aterm_types::scheme::ThemeMeta) -> String {
    if meta.display_name == id {
        id.to_string()
    } else {
        format!(
            "{}{}{id}",
            meta.display_name,
            crate::native_ui::FACT_SEPARATOR
        )
    }
}

/// The partial-apply reason for a loaded theme from a newer schema: which
/// keys this aterm skipped, bounded like every catalog reason. `None` for a
/// theme read in full.
pub(crate) fn skipped_keys_reason(meta: &aterm_types::scheme::ThemeMeta) -> Option<String> {
    if meta.skipped_keys.is_empty() {
        return None;
    }
    let count = meta
        .warnings
        .iter()
        .find_map(|warning| match warning {
            aterm_types::scheme::ThemeWarning::SkippedNewerKeys { count, .. } => Some(*count),
            _ => None,
        })
        .map_or(meta.skipped_keys.len(), usize::from);
    let reason = format!(
        "some settings need a newer aterm: {count} key(s) skipped ({})",
        meta.skipped_keys.join(", ")
    );
    Some(bounded_asset_text(&reason, MAX_THEME_REASON_BYTES).to_string())
}

/// The appearance a background measures as, by the one dark/light rule
/// ([`aterm_messages::ink::bg_is_light`]).
pub(crate) fn measured_appearance(bg: aterm_types::Rgb) -> aterm_types::Appearance {
    if aterm_messages::ink::bg_is_light([bg.r, bg.g, bg.b]) {
        aterm_types::Appearance::Light
    } else {
        aterm_types::Appearance::Dark
    }
}

/// Why one custom-theme row did or did not load, as [`ThemeCatalog::listing`]
/// reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeEntryStatus<'a> {
    Ready,
    /// Read but refused (syntax, size, unsafe name, case collision).
    Invalid(&'a str),
    /// Named like this built-in, which always wins.
    Shadowed(&'static str),
    /// A file name that is not UTF-8, listed under its lossy spelling.
    Unreadable,
}

impl ThemeEntryStatus<'_> {
    /// The one-line reason a non-ready row did not load.
    pub(crate) fn reason(self) -> Option<String> {
        match self {
            Self::Ready => None,
            Self::Invalid(reason) => Some(reason.to_string()),
            Self::Shadowed(builtin) => Some(format!(
                "shadowed by the built-in theme {builtin}, which always wins; rename the file \
                 (e.g. \"{builtin} (mine).conf\")"
            )),
            Self::Unreadable => {
                Some("the file name is not valid UTF-8, so no config can name it; rename it".into())
            }
        }
    }
}

/// How much of the themes folder discovery looked at and kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ThemeCatalogLimits {
    /// Regular `.conf` files seen while enumerating (a floor when the
    /// directory-entry cap stopped the scan).
    pub(crate) seen: usize,
    /// Files opened and parsed (ready or invalid), at most
    /// [`MAX_USER_THEME_FILES`].
    pub(crate) kept: usize,
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
    limits: ThemeCatalogLimits,
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
        // A scheme handed in whole has the appearance it says.
        Self::from_parsed(schemes.into_iter().map(|(name, scheme)| {
            (
                name,
                aterm_types::scheme::ParsedTheme::colours_only(scheme, true, None, None),
            )
        }))
    }

    /// A catalog of parsed theme files, each admitted as discovery admits it.
    #[cfg(test)]
    pub(crate) fn from_parsed(
        themes: impl IntoIterator<Item = (String, aterm_types::scheme::ParsedTheme)>,
    ) -> std::sync::Arc<Self> {
        Self::from_packages(themes.into_iter().map(|(name, parsed)| {
            let package = admit_theme_file(parsed, &name);
            (name, package)
        }))
    }

    /// A catalog of whole theme packages, as [`Self::from_schemes`] makes one
    /// of bare colours.
    #[cfg(test)]
    pub(crate) fn from_packages(
        packages: impl IntoIterator<Item = (String, aterm_types::scheme::ThemePackage)>,
    ) -> std::sync::Arc<Self> {
        let entries = packages
            .into_iter()
            .map(|(name, package)| ThemeAsset {
                fingerprint: stable_asset_fingerprint(0x54, name.as_bytes(), 0, 0, &[]),
                source_id: std::sync::Arc::from(name.clone()),
                resolution: ThemeAssetResolution::Ready(Box::new(package)),
                name,
            })
            .collect::<Vec<_>>();
        let kept = entries.len();
        std::sync::Arc::new(Self {
            entries: normalize_theme_entries(entries),
            truncated: false,
            limits: ThemeCatalogLimits { seen: kept, kept },
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
        Self::discover_in_with(directory, false, || {}, ThemeScanCaps::PRODUCTION)
            .unwrap_or_default()
    }

    /// [`Self::discover_in`] (or, `live`, [`Self::try_discover_in`]) under
    /// `caps` instead of the production bounds.
    #[cfg(test)]
    pub(crate) fn discover_in_capped(
        directory: &std::path::Path,
        live: bool,
        caps: ThemeScanCaps,
    ) -> Result<Self, ThemeCatalogWatchError> {
        Self::discover_in_with(directory, live, || {}, caps)
    }

    /// Fallible discovery for the live watcher: distinguishes an empty or
    /// deleted directory from a transient enumeration failure, so a failure
    /// never discards the admitted catalog. Startup uses the best-effort
    /// [`Self::discover_in`].
    pub(crate) fn try_discover_in(
        directory: &std::path::Path,
    ) -> Result<Self, ThemeCatalogWatchError> {
        Self::discover_in_with(directory, true, || {}, ThemeScanCaps::PRODUCTION)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn try_discover_in_after_scan(
        directory: &std::path::Path,
        after_scan: impl FnOnce(),
    ) -> Result<Self, ThemeCatalogWatchError> {
        Self::discover_in_with(directory, true, after_scan, ThemeScanCaps::PRODUCTION)
    }

    fn discover_in_with(
        directory: &std::path::Path,
        retain_on_file_failure: bool,
        after_scan: impl FnOnce(),
        caps: ThemeScanCaps,
    ) -> Result<Self, ThemeCatalogWatchError> {
        let mut candidates = std::collections::BTreeSet::new();
        let mut links = std::collections::BTreeSet::new();
        let mut truncated = false;
        let mut seen = 0usize;
        let read_dir = match std::fs::read_dir(directory) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(_) => return Err(ThemeCatalogWatchError::DirectoryUnreadable),
        };
        for (index, result) in read_dir.take(caps.entries + 1).enumerate() {
            // Past the entry cap the scan stops but keeps what it gathered:
            // a folder full of strays degrades to a bounded prefix, never to
            // an empty catalog.
            if index == caps.entries {
                truncated = true;
                break;
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
            // admitted directory through an attacker-swapped link. A linked
            // `.conf` is still listed, never opened, so its row says why it
            // does not load.
            if entry.path().extension() != Some(std::ffi::OsStr::new("conf")) {
                continue;
            }
            if file_type.is_symlink() {
                links.insert(entry.path());
            } else if !file_type.is_file() {
                continue;
            }
            seen = seen.saturating_add(1);
            candidates.insert(entry.path());
            if candidates.len() > caps.candidates {
                candidates.pop_last();
                truncated = true;
            }
        }
        after_scan();

        let mut entries = Vec::new();
        let mut kept = 0usize;
        for path in candidates {
            let source_id = bounded_asset_text(&path.to_string_lossy(), MAX_THEME_SOURCE_ID_BYTES);
            let Some(stem) = path.file_stem() else {
                continue;
            };
            let Some(name) = stem.to_str() else {
                entries.push(diagnostic_theme_asset(
                    &stem.to_string_lossy(),
                    &source_id,
                    ThemeAssetResolution::Unreadable,
                ));
                continue;
            };
            // Built-ins always win, so an identically named file never
            // changes the built-in palette; its row says why it is unused.
            if let Some(builtin) = builtin_theme_name(name) {
                entries.push(diagnostic_theme_asset(
                    name,
                    &source_id,
                    ThemeAssetResolution::Shadowed(builtin),
                ));
                continue;
            }
            if let Err(reason) = safe_user_theme_name(name) {
                entries.push(invalid_theme_asset(name, &source_id, reason));
                continue;
            }
            // Diagnostic only: the link is never opened (in watch mode a
            // refused open would block every reload) and never counts
            // against the file cap.
            if links.contains(&path) {
                entries.push(invalid_theme_asset(
                    name,
                    &source_id,
                    "symlinks are not followed; copy the file into the themes folder",
                ));
                continue;
            }
            // The file cap counts only files that are actually read, so
            // shadowing and misnamed files cannot push valid themes out.
            if kept == caps.files {
                truncated = true;
                break;
            }
            kept += 1;

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
            match aterm_types::scheme::parse_theme_str(text) {
                Ok(parsed) => entries.push(ThemeAsset {
                    name: name.to_string(),
                    source_id,
                    fingerprint: stable_asset_fingerprint(
                        0x54,
                        path.to_string_lossy().as_bytes(),
                        0,
                        0,
                        &bytes,
                    ),
                    resolution: ThemeAssetResolution::Ready(Box::new(admit_theme_file(
                        parsed, name,
                    ))),
                }),
                Err(error) => {
                    entries.push(invalid_theme_asset(name, &source_id, &error.to_string()))
                }
            }
        }
        Ok(Self {
            entries: normalize_theme_entries(entries),
            truncated,
            limits: ThemeCatalogLimits { seen, kept },
        })
    }

    pub(crate) fn ready_names(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter_map(|entry| match &entry.resolution {
                ThemeAssetResolution::Ready(_) => Some(entry.name.as_str()),
                ThemeAssetResolution::Invalid(_)
                | ThemeAssetResolution::Shadowed(_)
                | ThemeAssetResolution::Unreadable => None,
            })
    }

    /// Each loaded custom theme's id (file stem, what config writes) with its
    /// identity: display name, description, appearance and warnings.
    pub(crate) fn ready_themes(
        &self,
    ) -> impl Iterator<Item = (&str, &aterm_types::scheme::ThemeMeta)> {
        self.entries
            .iter()
            .filter_map(|entry| match &entry.resolution {
                ThemeAssetResolution::Ready(theme) => Some((entry.name.as_str(), &theme.meta)),
                ThemeAssetResolution::Invalid(_)
                | ThemeAssetResolution::Shadowed(_)
                | ThemeAssetResolution::Unreadable => None,
            })
    }

    /// The identity of the theme `id` names, by the same lookup as
    /// [`Self::resolve`]: a built-in first, then a loaded custom theme.
    pub(crate) fn meta(&self, id: &str) -> Option<aterm_types::scheme::ThemeMeta> {
        if let Some(builtin) = aterm_types::scheme::builtin_theme(id) {
            return Some(aterm_types::scheme::ThemeMeta::of_builtin(builtin));
        }
        self.ready_themes()
            .find(|(name, _)| name.eq_ignore_ascii_case(id))
            .map(|(_, meta)| meta.clone())
    }

    /// Where the loaded custom theme `id` was read from (its file path, bounded
    /// as discovery recorded it), by the lookup [`Self::resolve`] uses for
    /// files. `None` for a built-in or a name that did not load.
    pub(crate) fn source_path(&self, id: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| {
                matches!(entry.resolution, ThemeAssetResolution::Ready(_))
                    && entry.name.eq_ignore_ascii_case(id)
            })
            .map(|entry| &*entry.source_id)
    }

    /// Whether discovery kept a row named `name` (ignoring case), loaded or
    /// not.
    pub(crate) fn lists(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.name.eq_ignore_ascii_case(name))
    }

    /// The community pack's theme `name`, drawn from the embedded pack, where
    /// the folder has no file by that name at all: a file that did not load
    /// is that file's problem to report, never a reason to draw the pack's
    /// (which Keep could not write over it).
    pub(crate) fn pack_fallback(
        &self,
        name: &str,
    ) -> Option<(&'static str, &'static aterm_types::scheme::ThemePackage)> {
        if self.lists(name) {
            return None;
        }
        pack_package(name)
    }

    /// Every custom theme file discovery kept a row for, by name, with its
    /// status: loaded, refused with a reason, shadowed by a built-in, or
    /// unnameable. Only [`Self::ready_names`] reach the picker.
    pub(crate) fn listing(&self) -> impl Iterator<Item = (&str, ThemeEntryStatus<'_>)> {
        self.entries.iter().map(|entry| {
            let status = match &entry.resolution {
                ThemeAssetResolution::Ready(_) => ThemeEntryStatus::Ready,
                ThemeAssetResolution::Invalid(reason) => ThemeEntryStatus::Invalid(reason),
                ThemeAssetResolution::Shadowed(builtin) => ThemeEntryStatus::Shadowed(builtin),
                ThemeAssetResolution::Unreadable => ThemeEntryStatus::Unreadable,
            };
            (entry.name.as_str(), status)
        })
    }

    /// Whether the folder held more than discovery keeps (entries, candidates
    /// or files); the kept rows are then a bounded prefix.
    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }

    /// How many `.conf` files discovery saw and how many it read.
    pub(crate) fn limits(&self) -> ThemeCatalogLimits {
        self.limits
    }

    /// Resolve a built-in or parsed user theme, case-insensitively, without
    /// filesystem access. Case-fold collisions are rejected at admission.
    pub(crate) fn resolve(&self, name: &str) -> Result<aterm_types::ColorScheme, String> {
        if let Some(scheme) = aterm_types::scheme::builtin(name) {
            return Ok(scheme);
        }
        // Shadowed rows are unreachable (the built-in answered above) and an
        // unreadable row's lossy name is not the file's, so neither resolves.
        let found = self.entries.iter().find_map(|entry| {
            if !entry.name.eq_ignore_ascii_case(name) {
                return None;
            }
            match &entry.resolution {
                ThemeAssetResolution::Ready(theme) => Some(Ok(theme.colors.clone())),
                ThemeAssetResolution::Invalid(reason) => {
                    Some(Err(format!("{} is invalid ({reason})", entry.source_id)))
                }
                ThemeAssetResolution::Shadowed(_) | ThemeAssetResolution::Unreadable => None,
            }
        });
        match found {
            Some(resolved) => resolved,
            None if self.truncated => Err(
                "no built-in or loaded custom theme by that name (the themes folder holds more \
                 files than aterm loads)"
                    .to_string(),
            ),
            None => Err("no built-in or loaded custom theme by that name".to_string()),
        }
    }

    /// The whole theme `name` names, by the same lookup as [`Self::resolve`]:
    /// a built-in (owned, with no typography) or a loaded file (borrowed).
    pub(crate) fn resolve_package(
        &self,
        name: &str,
    ) -> Result<std::borrow::Cow<'_, aterm_types::scheme::ThemePackage>, String> {
        if let Some(builtin) = aterm_types::scheme::builtin_theme(name)
            && let Some(package) = aterm_types::scheme::ThemePackage::of_builtin(builtin)
        {
            return Ok(std::borrow::Cow::Owned(package));
        }
        let found = self.entries.iter().find_map(|entry| {
            if !entry.name.eq_ignore_ascii_case(name) {
                return None;
            }
            match &entry.resolution {
                ThemeAssetResolution::Ready(theme) => {
                    Some(Ok(std::borrow::Cow::Borrowed(&**theme)))
                }
                ThemeAssetResolution::Invalid(reason) => {
                    Some(Err(format!("{} is invalid ({reason})", entry.source_id)))
                }
                ThemeAssetResolution::Shadowed(_) | ThemeAssetResolution::Unreadable => None,
            }
        });
        match found {
            Some(resolved) => resolved,
            // Not a built-in and no row by that name: `resolve` says why, in
            // the same words.
            None => Err(self
                .resolve(name)
                .err()
                .unwrap_or_else(|| "no built-in or loaded custom theme by that name".to_string())),
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
/// sentinel rather than letting directory order pick a palette. Shadowed and
/// unreadable rows never resolve, so they keep one row per file and take no
/// part in collisions.
fn normalize_theme_entries(entries: Vec<ThemeAsset>) -> Vec<ThemeAsset> {
    let (diagnostic, resolvable): (Vec<_>, Vec<_>) = entries.into_iter().partition(|entry| {
        matches!(
            entry.resolution,
            ThemeAssetResolution::Shadowed(_) | ThemeAssetResolution::Unreadable
        )
    });
    let mut normalized = collapse_case_collisions(resolvable);
    normalized.extend(diagnostic);
    normalized.sort_by(theme_entry_order);
    normalized
}

fn theme_entry_order(left: &ThemeAsset, right: &ThemeAsset) -> std::cmp::Ordering {
    left.name
        .to_ascii_lowercase()
        .cmp(&right.name.to_ascii_lowercase())
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.source_id.cmp(&right.source_id))
}

fn collapse_case_collisions(mut entries: Vec<ThemeAsset>) -> Vec<ThemeAsset> {
    entries.sort_by(theme_entry_order);
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

/// A diagnostic-only row (shadowed or unreadable): kept for listings, never
/// resolved.
fn diagnostic_theme_asset(
    name: &str,
    source_id: &std::sync::Arc<str>,
    resolution: ThemeAssetResolution,
) -> ThemeAsset {
    let tag: &[u8] = match resolution {
        ThemeAssetResolution::Shadowed(builtin) => builtin.as_bytes(),
        _ => b"unreadable",
    };
    ThemeAsset {
        name: name.to_string(),
        source_id: std::sync::Arc::clone(source_id),
        fingerprint: stable_asset_fingerprint(0x53, source_id.as_bytes(), 0, 0, tag),
        resolution,
    }
}

/// The built-in a file stem names (ASCII case-insensitive, `Default`
/// included), spelled canonically: the same match `scheme::builtin` makes.
fn builtin_theme_name(name: &str) -> Option<&'static str> {
    aterm_types::scheme::builtin_names()
        .into_iter()
        .find(|builtin| builtin.eq_ignore_ascii_case(name))
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
    /// Stable paint/install identity. The variant is part of the identity, so
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
    /// No `wallpaper` key configured: no backdrop.
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
    pub(crate) trail_style: std::sync::Arc<TrailStyleCache>,
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
            trail_style: TrailStyleCache::empty(),
            kitty_sprite: KittySpriteAsset::BuiltIn,
            wallpaper: WallpaperAsset::None,
            themes: ThemeCatalog::empty(),
            sparkle_spec_consumers: None,
        })
    }
}

impl TrailStyleCache {
    #[cfg(test)]
    pub(crate) fn empty() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    /// Resolve `raw` once for one config generation.
    pub(crate) fn for_style(raw: &str) -> Self {
        Self {
            resolved_style_raw: raw.trim().into(),
            resolved_presentation: resolve_trail_presentation(raw),
        }
    }

    /// Cached answer, with a dynamic fallback for test/preview code that
    /// mutates `Config` without publishing a new asset generation.
    pub(crate) fn presentation_for(&self, raw: &str) -> ResolvedTrailPresentation {
        let raw = raw.trim();
        if raw == self.resolved_style_raw.as_ref() {
            self.resolved_presentation
        } else {
            resolve_trail_presentation(raw)
        }
    }
}

/// Canonical interpretation of one raw `cursor_trail_style` value, shared by
/// rendering, validation and Settings so an invalid value is treated the same
/// everywhere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ResolvedTrailStyle {
    pub(crate) canonical: Option<&'static str>,
    pub(crate) style: Option<aterm_effects::cursor_glow::GlowStyle>,
    /// The authored spelling is unknown; `style` is the default it falls back to.
    pub(crate) unknown: bool,
}

/// All string-keyed presentation decisions for one resolved trail style,
/// resolved once per config generation so rendering reads only `Copy` fields.
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
                unknown: false,
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

impl Default for TrailStyleCache {
    fn default() -> Self {
        Self {
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
            unknown: false,
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
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleInkConfig {
    /// Emit ink at all. Default on (under the master switch).
    pub(crate) enabled: Option<bool>,
    /// Ink tint vs the original fg. Default 0.75, clamped 0.0..=1.0.
    pub(crate) strength: Option<f32>,
    /// One specular sweep window, ms. Default 2200, clamped 350..=6000
    /// (`loop = true` raises the floor to 600, the flash-safety margin).
    pub(crate) sweep_ms: Option<u32>,
    /// Re-sweep while the word stays visible (keeps focused wakes live).
    /// Default false. (`loop` is a Rust keyword, hence the rename.)
    #[serde(rename = "loop")]
    pub(crate) loop_: Option<bool>,
}

/// `[sparkle_words.emphasis]`: ink-only hype words. The built-in lexicon has
/// none; `extra_words` populates it.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleEmphasisConfig {
    /// Decorate emphasis words. Default on (effective only when the master and
    /// `sparkle_words.ink` are on: the class has no non-ink surface).
    pub(crate) enabled: Option<bool>,
    /// Extra whole words to treat as emphasis (added to the lexicon).
    pub(crate) extra_words: Option<Vec<String>>,
    /// Folded surfaces to never decorate as emphasis.
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// `[sparkle_words.profanity]` — the randomized, self-terminating sparkle.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
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
    /// Quasar (1/512) / Singularity (1/1024) rare nova variants. Default on.
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
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleFelineConfig {
    /// Decorate cat/kitty words. Default on (under the master).
    pub(crate) enabled: Option<bool>,
    /// `"cat"` (default) is the only graphic style; `"paw"` selects ink-only
    /// rendering. Unknown values mean `"cat"`.
    pub(crate) style: Option<String>,
    /// Parsed and preserved for compatibility; has no effect.
    pub(crate) idle: Option<bool>,
    /// Parsed and preserved for compatibility; has no effect.
    pub(crate) gaze: Option<bool>,
    /// Fortune (1/512) / Nebula (1/1024) rare cats. Default on.
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
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleCanineConfig {
    /// Summon dogs for typed canine words. Default on (under the sparkle
    /// master).
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
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleTricksConfig {
    /// Let typed kitty commands move the pet and flash the word. Default on
    /// (under the sparkle master).
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
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct MatrixRainConfig {
    /// Master switch. Absent ⇒ off.
    pub(crate) enabled: Option<bool>,
    /// Tick rate in the `Working` weather, clamped 12..=60 (`Calm` always runs
    /// 12 Hz).
    pub(crate) fps: Option<u32>,
    /// Column density, clamped 1..=12.
    pub(crate) density: Option<u32>,
    /// Fall speed, clamped 1..=10 (5 = neutral).
    pub(crate) speed: Option<u32>,
    /// Trail length, clamped 1..=10 (5 = neutral).
    pub(crate) trail: Option<u32>,
    /// Body coverage, clamped `RAIN_ALPHA_FLOOR..=RAIN_ALPHA_CAP` (16..=135).
    /// Absent ⇒ derived from the theme.
    pub(crate) alpha: Option<u32>,
    /// Bright-head coverage, clamped `alpha..=135`. Absent ⇒ derived.
    pub(crate) head_alpha: Option<u32>,
    /// `"matrix"` | `"theme"` | `"#RRGGBB"`. Malformed hex fails closed to the
    /// stock matrix green without failing the config load.
    pub(crate) hue: Option<String>,
    /// Glyph mutation window in ms, clamped 80..=2000.
    pub(crate) mutation_ms: Option<u64>,
    /// Idle seconds until the mandatory drain, clamped 2..=120; no configuration
    /// animates forever.
    pub(crate) idle_secs: Option<u64>,
    /// Suppress emission on the alternate screen. Default false.
    pub(crate) suppress_in_alt_screen: Option<bool>,
    /// Parsed but inert.
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
    /// Parsed but inert.
    pub(crate) ink_text: Option<bool>,
    /// Parsed but inert.
    pub(crate) phosphor: Option<bool>,
    /// Field seed. 0 (default) ⇒ a stable per-window seed is derived at
    /// engine build; nonzero ⇒ reproducible (demos/tests).
    pub(crate) seed: Option<u64>,
}

/// The `[presence]` table: durable state of the View menu's needs switch
/// (Show Needs in Windows: the needs border and its pill). Default on; absent
/// equals `true` ([`Config::presence_rim_enabled`]). The switch writes the leaf
/// key through the serialized config lane. `band` is retired with the band row
/// (`native_config_language`'s retired-key table names it once).
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct PresenceConfig {
    /// The needs border and its titlebar pill. Absent means shown; `false`
    /// draws neither (the tab marks and the wire stay).
    pub(crate) rim: Option<bool>,
}

/// The `[claude_footer]` table: what the Claude Code footer
/// (`crate::claude_footer`) writes into the composer's bottom rule. `usage` is
/// on by default ([`Config::claude_footer_usage`]). A GUI table, not
/// `[harness]`: that table's rule is that the file only ever takes power away,
/// and it has one reader in `aterm-agent`. There is no fast-mode key: aterm
/// draws no fast chip (Claude Code's own `↯` marks fast mode).
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct ClaudeFooterConfig {
    /// `Σ`: Claude's working time and the session's merged token totals in the
    /// rule, `Σ 7h52m ↑395M ↓2.0M`. `false` hides it; the limit wall (`⧗`) shows
    /// either way.
    pub(crate) usage: Option<bool>,
    /// The harness card (`crate::claude_card`,
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §3): the rainbow border,
    /// kitty and `enhanced by aterm harness` line Claude Code's launch card
    /// wears while the harness carries the session (and in white, the
    /// moonbow, under Autopilot Off: §10.6.15). On by default (absent and
    /// `true` are the same, [`Config::claude_footer_card`]); `false` leaves
    /// the vendor's card bare. A GUI table's switch, like `usage`: `[harness]`
    /// only takes power away.
    pub(crate) card: Option<bool>,
    /// THE ACT TICKER (`crate::harness_glass`, design
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §9.2): what the harness
    /// just did, lit for 3 s right of the Autopilot chip (`✓ approved`), at
    /// most once per 15 s. ON BY DEFAULT (absent and `true` are the same,
    /// [`Config::claude_footer_acts`]); `false` withholds the label alone —
    /// the chip's title still lists the last acts, and Settings ▸ Harness
    /// still counts them. Serious mode withholds it too.
    pub(crate) acts: Option<bool>,
    /// THE AGENTS PANEL'S GLASS (`crate::claude_panel`, design
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §10.6.13): Claude
    /// Code's agents panel under the footer wears the harness theme's
    /// rainbow, and each agent's dot tells its work (hue = which agent,
    /// brightness and glow = work rate, saturation = freshness, a grey
    /// breath = stuck). ON BY DEFAULT (absent and `true` are the same,
    /// [`Config::claude_footer_agents`]); `false` leaves the panel as
    /// Claude drew it.
    pub(crate) agents: Option<bool>,
}

/// The `[render]` table: how aterm repairs a displaced Claude Code tab.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct RenderConfig {
    /// `repaint_nudge`: `"on"` (default: aterm tells that one program its
    /// terminal is one row shorter, waits for its repaint, tells it the true
    /// size, waits for the second repaint; no key is sent, the window and the
    /// screen aterm draws are never resized), `"log"` (everything but the two
    /// size changes runs, and what would have been done goes to `aterm.log`) or
    /// `"off"`. Any case. Only Claude Code is ever nudged. Read once at launch.
    /// A word aterm does not know reads as `log` and is reported by the config
    /// diagnostics.
    pub(crate) repaint_nudge: Option<String>,
}

/// The `[harness]` table as the supervisor reads it: the policy every agent
/// session this window hosts is supervised under, and one note per key its
/// reader refused or found outside the root table.
///
/// Read by [`aterm_agent::supervise::SupervisorConfig::from_aterm_toml`], the
/// same reader the live-upgrade sweep uses (owner, 2026-09-24).
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

/// The `[ai]` table as the window holds it (docs/DESIGN-ai-connections-2026-10-16.md
/// §6): this machine's table, read by [`aterm_agent::ai::config::read`] (the
/// reader `aterm ai` uses: only `[ai]` keys interpreted, unusable values noted,
/// invalid TOML read as defaults), and the fleet document it resolves over, read
/// whole when the config is parsed. Part of [`Config`]'s `PartialEq`, so an edit
/// of either is not deduped: the fleet document is a path feed
/// ([`Config::path_feed_paths`]), and its edit re-posts the config.
///
/// The fleet document is the one `[ai] fleet` names (a file, or an `https://`
/// URL whose document aterm holds once fetched and verified), else
/// `ai-fleet.toml` beside `aterm.toml` when it exists, as `aterm ai` finds it
/// ([`aterm_agent::ai::config::load`]).
#[derive(Default, Clone, Debug, PartialEq)]
pub(crate) struct AiPolicy {
    /// This machine's `[ai]` table.
    pub(crate) local: aterm_agent::ai::config::Local,
    /// What the reader said about it, one line each.
    pub(crate) notes: Vec<String>,
    /// Where the fleet document comes from: named, else the file beside
    /// `aterm.toml` when one is there.
    pub(crate) source: Option<aterm_agent::ai::config::FleetSource>,
    /// The files whose edit re-reads it: the document (for a URL, the copy
    /// aterm holds) and, under a pinned key, its signature. The file beside
    /// `aterm.toml` is watched while absent, so the watcher sees it appear.
    pub(crate) fleet_paths: Vec<std::path::PathBuf>,
    /// The fleet document as read: admitted with its notes, or why it was
    /// refused.
    pub(crate) fleet: Option<Result<(aterm_agent::ai::config::Fleet, Vec<String>), String>>,
}

impl AiPolicy {
    /// The table `text` — the whole `aterm.toml` — writes, over the fleet
    /// document it names or the one beside the canonical `aterm.toml`.
    pub(crate) fn read(text: &str) -> Self {
        // A test reads no file beside the person's own `aterm.toml`, and no
        // fleet document aterm holds: what its config says is all it gets
        // ([`Self::read_beside`] takes a folder).
        #[cfg(test)]
        let beside: Option<std::path::PathBuf> = None;
        #[cfg(not(test))]
        let beside = config_path().and_then(|path| path.parent().map(std::path::Path::to_path_buf));
        Self::read_beside(text, beside.as_deref())
    }

    /// [`Self::read`] with `aterm.toml`'s folder given: the fleet document is
    /// `[ai] fleet`, else `ai-fleet.toml` in `dir` when it exists, read as
    /// `aterm ai` reads it ([`aterm_agent::ai::config::load_source`]: at most
    /// 64 KiB, a symbolic link followed, signed where a key is pinned, refused
    /// whole or admitted).
    pub(crate) fn read_beside(text: &str, dir: Option<&std::path::Path>) -> Self {
        use aterm_agent::ai::config::{self, FleetSource};
        let (local, notes) = config::read(text);
        #[cfg(test)]
        let ai_dir: Option<std::path::PathBuf> = None;
        #[cfg(not(test))]
        let ai_dir = config::default_ai_dir();
        let config_path = dir.map(|dir| dir.join("aterm.toml"));
        let admitted = config::fleet_source(&local, config_path.as_deref());
        let fleet = admitted.as_ref().map(|source| {
            config::load_source(
                source,
                local.fleet_key.as_deref(),
                ai_dir.as_deref(),
                aterm_agent::supervise::limit::unix_now(),
            )
        });
        let sig = |p: &std::path::Path| {
            let mut s = p.as_os_str().to_owned();
            s.push(".sig");
            std::path::PathBuf::from(s)
        };
        let watched = admitted
            .clone()
            .or_else(|| dir.map(|dir| FleetSource::Path(dir.join(config::FLEET_FILE))));
        let fleet_paths = match &watched {
            Some(FleetSource::Path(p)) if local.fleet_key.is_some() => vec![p.clone(), sig(p)],
            Some(FleetSource::Path(p)) => vec![p.clone()],
            Some(FleetSource::Url(_)) => ai_dir
                .map(|d| {
                    let held = aterm_agent::ai::fleet::cache_dir(&d);
                    vec![
                        held.join(aterm_agent::ai::fleet::DOC_FILE),
                        held.join(aterm_agent::ai::fleet::SIG_FILE),
                    ]
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        Self {
            local,
            notes,
            source: admitted,
            fleet_paths,
            fleet,
        }
    }

    /// Every choice, resolved: this machine's table over the fleet
    /// document's row for it, its top level and the built-in defaults
    /// ([`aterm_agent::ai::config::resolve`]).
    pub(crate) fn settings(&self) -> aterm_agent::ai::config::Settings {
        aterm_agent::ai::config::resolve(
            &self.local,
            self.source.as_ref(),
            self.fleet.clone(),
            self.notes.clone(),
        )
    }

    /// Whether the window looks at the AI CLIs and says what needs a person
    /// (`[ai] enabled`, on by default; never from a fleet document).
    pub(crate) fn enabled(&self) -> bool {
        self.local.enabled.unwrap_or(true)
    }
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

    /// The policy as Settings ▸ Harness says it aloud (the switch's spoken label
    /// and the save's feedback): `Automatic` at full power, `Off: …` when the
    /// master switch is, else what limits it in the words of the Harness rows
    /// ("Approve permission boxes set to Safe") and, for limits only the file
    /// holds, how many — Modified lists those keys. Never a TOML key or value,
    /// and never a change that takes no power away (an answer text, the
    /// question priority: its own row says Changed). Classification is
    /// `SupervisorConfig::against_default`'s; this only words it.
    pub(crate) fn words(&self) -> String {
        if !self.policy.enabled {
            return "Off: no agent session is supervised".to_string();
        }
        let limits = self.limits();
        if limits.is_empty() {
            return "Automatic".to_string();
        }
        let mut parts: Vec<String> = limits
            .iter()
            .filter_map(|limit| match limit {
                HarnessLimit::Row { label, value } => Some(format!("{label} set to {value}")),
                HarnessLimit::File => None,
            })
            .collect();
        let in_file = file_limits(&limits);
        if in_file > 0 {
            parts.push(format!(
                "{in_file} {} in aterm.toml",
                if in_file == 1 { "setting" } else { "settings" }
            ));
        }
        format!("Automatic, limited by {}", parts.join("; "))
    }

    /// How many of the limits only aterm.toml holds — no Settings row shows
    /// them, so the master switch's row says "Limited in Manual" for them
    /// (`native_settings::cross_field_constraints`), from the same reading
    /// [`Self::words`] counts them by.
    pub(crate) fn file_limits(&self) -> usize {
        file_limits(&self.limits())
    }

    /// Each key that takes power away, by its Harness row where it has one
    /// ([`harness_limit_row`]).
    fn limits(&self) -> Vec<HarnessLimit> {
        self.policy
            .against_default()
            .limits
            .iter()
            .map(|limit| {
                harness_limit_row(limit).map_or(HarnessLimit::File, |(label, value)| {
                    HarnessLimit::Row { label, value }
                })
            })
            .collect()
    }
}

/// One limit [`HarnessPolicy::words`] names.
enum HarnessLimit {
    /// A key with a Harness row: the row's label and the value as the row
    /// shows it.
    Row { label: &'static str, value: String },
    /// A key only aterm.toml holds.
    File,
}

/// How many of `limits` only the file holds.
fn file_limits(limits: &[HarnessLimit]) -> usize {
    limits
        .iter()
        .filter(|limit| matches!(limit, HarnessLimit::File))
        .count()
}

/// The Harness row a limit `against_default` names (`approve: safe`,
/// `autopilot: land`, `codex: off`) is set on — its label and the value as
/// the row shows it — read from the one registry the rows are drawn from
/// ([`crate::prefs::NESTED_LEAVES`]), so a row added there is a row here.
/// An agent's limit is named by the agent (`codex: off`), its row by its
/// table's key (`harness.codex.enabled`). A Bool row's limit is its Off; an
/// Enum row's, the words its segment or picker shows for the value
/// ([`crate::native_settings::setting_choice_label`]). `None`: a limit only
/// aterm.toml holds.
fn harness_limit_row(limit: &str) -> Option<(&'static str, String)> {
    let (leaf, shown) = limit.split_once(": ")?;
    let agent = format!("harness.{leaf}.enabled");
    let key = if crate::prefs::agent_switch(&agent).is_some() {
        agent
    } else {
        format!("harness.{leaf}")
    };
    let row = crate::prefs::NESTED_LEAVES
        .iter()
        .find(|row| row.key == key)?;
    let value = match row.kind {
        crate::prefs::EditKind::Bool => "Off".to_string(),
        crate::prefs::EditKind::Enum { .. } => {
            crate::native_settings::setting_choice_label(row.key, shown)
        }
        _ => return None,
    };
    Some((row.label, value))
}

/// The `[output_streak]` table (`docs/DESIGN-output-streak-2026-08-30.md`). On
/// by default. Defaults and clamps live only in the
/// `Config::output_streak_*_or_default` resolvers; numeric values clamp rather
/// than reject so a typo never fails the load.
///
/// Additive light only: cells are never mutated, and `OVER_INK_COV_CAP` keeps
/// text legible. `aterm_effects::output_streak` re-clamps every numeric value.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct OutputStreakConfig {
    /// Master switch. Absent means on except on Windows (see
    /// [`Config::output_streak_enabled_or_default`]).
    pub(crate) enabled: Option<bool>,
    /// Amplitude scale `0..=1` (default `1.0`). Non-finite reads as 0.
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
    /// Absent ⇒ on.
    pub(crate) sound: Option<bool>,
}

/// The `[packages]` table (`atpkg`, `docs/TOOLCHAIN-PACKAGE-MANAGER.md` §11).
/// Defaults live only in [`Config::packages_enabled`] and
/// [`Config::packages_auto_install`]. `atpkg` reads the same keys (and the
/// undeclared `[packages.links]` sub-table) from this file; the GUI mirrors
/// the two user-facing keys with atpkg's resolution
/// (`atpkg::config::PackagesConfig::{enabled, auto_install}`). No environment
/// variable overrides it.
///
/// ```toml
/// [packages]
/// # enabled      = true    # Keep ALab tools up to date, the switch (default true)
/// # auto_install = true    # install the ALab toolset nobody named: the first-run
/// #                        # fill and new members of the signed set (default true,
/// #                        # batteries included; uninstall/exclude/enabled=false win)
/// # exclude      = ["trust"]   # per-program opt-out
/// ```
///
/// Legacy spellings: `auto_update = false` reads as `enabled = false`, and
/// `seed_install` as `auto_install` when that is unset. `channel`/`include` are
/// ignored; `prefix`, `account` and `owner/repo` links are development-only.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct PackagesConfig {
    /// Keep ALab tools up to date, the switch. Absent ⇒ on.
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
    /// A development setting (index owner repoint), consumed by a development
    /// build's atpkg only and shown nowhere in Settings.
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

/// `[privacy] warmup`: whether aterm asks macOS for each configured folder at the
/// first quiet moment of a launch that reads Full Disk Access off (owner ask
/// 2026-10-16: file access is asked for by default on a new Mac;
/// `crate::consent_launch`), and whether the "Ask for folder access now" gesture
/// on the Security panel is offered.
///
/// Unknown spellings resolve to the default rather than failing the parse,
/// because [`crate::app_config::load_config`] discards the whole file on a
/// serde error; `--validate-config` names the typo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PrivacyWarmup {
    /// Never offered, never automatic.
    Never,
    /// Only the owner's gesture on the Security panel: no automatic pass, no control
    /// verb, no environment variable.
    OnRequest,
    /// The gesture, and the launch pass: once the Full Disk Access hand-over is left
    /// without the owner acting on it, while the grant reads off, once per bundle id
    /// until every folder is answered.
    #[default]
    Automatic,
}

impl PrivacyWarmup {
    /// The config spelling, which is also the report spelling.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnRequest => "on-request",
            Self::Automatic => "automatic",
        }
    }

    /// Every accepted spelling, in the order `--validate-config` lists them.
    pub(crate) const ALL: &'static [Self] = &[Self::Never, Self::OnRequest, Self::Automatic];

    /// Whether the Security panel's gesture is offered.
    pub(crate) const fn offered(self) -> bool {
        !matches!(self, Self::Never)
    }

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
/// It governs reporting, the owner's gestures, and what aterm puts in front of
/// the owner at launch: on a Mac where Full Disk Access reads off, the Full
/// Disk Access page with aterm's panel beside it, then the folder warm-up
/// (owner ask 2026-10-16, `crate::consent_launch`); and, once per Mac, aterm's
/// own debugger-access alert, after the Full Disk Access and Developer Tools
/// pages and before the folders (owner ruling O29,
/// `crate::debugger_access::admit_launch_ask`), whose yes alone starts macOS's
/// administrator dialog. No key here answers a system dialog, flips a switch or
/// widens aterm's access, and a key can only keep a launch question from coming
/// up, never raise one. There is no environment variable or control verb for
/// it, so a program inside a session cannot change it.
///
/// ```toml
/// [privacy]
/// # enabled            = true            # master switch; false ⇒ every consent field reads `unknown`
/// # check              = true            # the silent, prompt-free probes: Full Disk Access (opens
/// #                                      #   a TCC.db), the debugger rule (DevToolsSecurity -status)
/// #                                      #   and the Developer Tools answer (reads tccd's log)
/// # notice             = true            # the Full Disk Access question, asked once per real need (Open Settings / Not now),
/// #                                      #   and the Full Disk Access page brought up at launch while the grant reads off,
/// #                                      #   and the debugger-access alert aterm raises once per Mac at launch
/// # report_attribution = true            # per-session responsible-pid corroboration
/// # warmup             = "automatic"     # "never" | "on-request" | "automatic" (also asks for each folder at launch while Full Disk Access is off)
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
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct PrivacyConfig {
    /// Master for the consent lane. Absent means on. Off, every consent field
    /// reads `unknown` (aterm stopped looking, not "denied"), no probe, notice
    /// or launch hand-over runs, the warm-up is neither offered nor walked, and
    /// the `tccd` observer is off in every mode, Full included.
    pub(crate) enabled: Option<bool>,
    /// Silent macOS access probes. Absent means on. The Full Disk Access probe
    /// opens the user's TCC.db read-only, unread (the system one where macOS
    /// keeps no user store, macOS 27); the debugger-rule probe runs
    /// `DevToolsSecurity -status`
    /// (`aterm_containment::consent::devtools_status_command`); the Developer
    /// Tools answer is one bounded `/usr/bin/log show` of tccd's own log
    /// (`aterm_containment::consent::probe_devtool`). None can raise a dialog;
    /// all run only from inside the app bundle.
    pub(crate) check: Option<bool>,
    /// The Full Disk Access question (`consent_card`, once per real need, never
    /// at launch; its *Not now* is recorded for the life of the config
    /// directory) and the launch hand-over of the Full Disk Access page
    /// (`App::hand_fda_to_owner`): at the first quiet moment of a launch whose
    /// probe reads the grant off on a grant-stable build where aterm never
    /// verified it, System Settings opens at that page with aterm's panel
    /// beside it, once per version. It also gates the debugger-access launch
    /// ask (owner ruling O29, `AskDrop::NoticeOff`): off, aterm never raises
    /// that alert by itself; no value raises it. Absent means on.
    pub(crate) notice: Option<bool>,
    /// Per-session responsible-process corroboration on reports. Absent means on.
    /// aterm's adoption record still decides `live` vs `adopted`.
    pub(crate) report_attribution: Option<bool>,
    /// `"never"` | `"on-request"` | `"automatic"`. Absent ⇒ `automatic`. See
    /// [`PrivacyWarmup`].
    pub(crate) warmup: Option<String>,
    /// Which folders the warm-up walks, by name
    /// (`aterm_containment::consent::Folder`). Absent ⇒
    /// [`Folder::WARMUP_DEFAULT`](aterm_containment::Folder::WARMUP_DEFAULT): the
    /// three folders, not `app-data` (macOS forgets that answer when the process
    /// that received it is replaced; the name stays valid in an explicit list).
    /// An explicit empty list means nothing to walk, unlike `protected_roots`,
    /// where empty must never resolve to "nothing is sensitive".
    pub(crate) warmup_folders: Option<Vec<String>>,
    /// Cap in ms on the in-place-apply hold while a warm-up runs. Absent means
    /// [`PRIVACY_WARMUP_HOLD_MS_DEFAULT`]; clamped to [`PRIVACY_WARMUP_HOLD_MS_MAX`].
    pub(crate) warmup_hold_ms: Option<u64>,
    /// Floor on re-probing, in ms. Absent ⇒
    /// [`PRIVACY_PROBE_INTERVAL_MS_DEFAULT`].
    pub(crate) probe_interval_ms: Option<u64>,
    /// The `tccd` log observer (`crate::consent_observer`), a tri-state. Absent ⇒
    /// the sentinel: one `/usr/bin/log stream` child whose predicate matches only
    /// the line macOS writes when it shows a consent dialog
    /// (`AUTHREQ_PROMPTING`), silent between dialogs; it only tells the card that
    /// a real need exists. `false` ⇒ off. `true` ⇒ the full stream, the
    /// correlation table, the tab mark and the per-dialog banner. It depends on
    /// admin-group membership (it sees nothing otherwise, and says
    /// `unavailable`) and reads an undocumented message format. Config only: no
    /// environment variable or control verb, so a program inside a session
    /// cannot enable a consent surface. `false` also stops the Developer Tools
    /// row's `log show` (`refused_observer_off`): it reads the same log.
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
/// reach atpkg failing closed).
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct RerouteConfig {
    pub(crate) announce: Option<bool>,
    pub(crate) strict: Option<bool>,
}

/// The `[keeper]` table: `enabled` (default off) registers each terminal with
/// this build's PTY keeper (`aterm keeper start`) so a launch after a crash
/// reattaches its shells.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
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

/// The `[session_log]` table: `journal` (default on: D3, ruled 2026-10-19;
/// it follows `restore_session` and the desk, so a headless instance and
/// Windows keep none) keeps a disk journal of what each session did, so `log
/// @<sid>` and `brief @<sid>` answer for a session from an earlier run or a
/// crash (`crate::session_log_journal`). `journal = false` is the opt-out:
/// aterm then creates no file or directory for it, starts no thread and
/// deletes nothing. `clause` (default on: D6, ruled 2026-10-19; session log
/// phase 3B) adds the log's clause to a tab's tooltip and context menu (`now:
/// running shell`, a shown label with its author), never to its label;
/// `clause = false` is the opt-out, and then both are as they were before
/// the clause, byte for byte.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SessionLogConfig {
    pub(crate) journal: Option<bool>,
    pub(crate) clause: Option<bool>,
}

impl Config {
    /// Whether the session journal is asked for (`[session_log] journal`,
    /// default `true`: D3, ruled 2026-10-19; `journal = false` opts out). It
    /// also follows `restore_session` and the desk
    /// (`session_log_journal::gate`).
    pub(crate) fn session_log_journal(&self) -> bool {
        self.session_log
            .as_ref()
            .and_then(|s| s.journal)
            .unwrap_or(true)
    }

    /// Whether a tab's tooltip and menu show its session log clause
    /// (`[session_log] clause`, default `true`: D6, ruled 2026-10-19;
    /// `clause = false` opts out). Read live at each compose: the chrome
    /// cache keys on the clause, so a reload is shown at the next refresh.
    pub(crate) fn session_log_clause(&self) -> bool {
        self.session_log
            .as_ref()
            .and_then(|s| s.clause)
            .unwrap_or(true)
    }
}

/// `[machine]`: macOS host settings `atpkg` applies on every package pass and on
/// `aterm pkg machine apply`, mirrored so Settings ▸ Security can show them.
/// `atpkg::config::MachineConfig` must resolve the same defaults
/// (`machine_defaults_agree_across_the_two_config_readers`).
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct MachineConfig {
    /// `"off"` (default) writes `com.apple.universalcontrol Disable`/`DisableMagicEdges`
    /// for the current host when they are not already set; `"leave"` never touches them.
    pub(crate) universal_control: Option<String>,
    /// `true` (default) installs Rosetta 2 on an Apple-silicon Mac that lacks it, as the
    /// user (`atpkg::machine::apply_rosetta`); `false` never does.
    pub(crate) rosetta: Option<bool>,
}

/// The `[sys]` table: system awareness.
///
/// ```toml
/// [sys]
/// stats_bars = "deck"        # the default; "sill" for the compact form; "off" hides the bars
/// stats_deck_size = "auto"   # the default; "full" or "folded" force the Deck's form
/// disk_roots = ["~/Library/Developer/Xcode/DerivedData"]   # folders the disk walker may size
/// ```
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SysConfig {
    /// The stats bars, Global, aterm and Tab: `"deck"` (default, three full
    /// bars under the titlebar), `"sill"` (three light lanes in the bottom
    /// pad, no terminal row) or `"off"`. Unknown values show the Deck.
    pub(crate) stats_bars: Option<String>,
    /// The Deck's form: `"auto"` (default: Full on a tall window, Folded on
    /// a short one), `"full"` or `"folded"`
    /// (`docs/DESIGN-stats-deck-v2-2026-10-20.md` §2.2). Unknown values read
    /// as `"auto"`.
    pub(crate) stats_deck_size: Option<String>,
    /// Folders the disk walker may measure though they lie in `~/Library`
    /// outside aterm's own folders or under a privacy-protected folder
    /// (design O20: a config list; aterm never asks for one). Naming a
    /// protected folder aterm has no access to lets macOS ask once. Absolute
    /// or `~/…`; read live. Another user's home, a remote, removable or
    /// automounted volume stays refused whatever this says.
    pub(crate) disk_roots: Option<Vec<String>>,
}

/// The `[fabric]` table: the cross-aterm message bus, off by default.
///
/// ```toml
/// [fabric]
/// command = "aterm-link serve --fleet lab --broker /run/astream.sock --cap-file ~/.config/aterm/fabric.cap"
/// # or, an argv whose words are passed as written (a path with a space):
/// command = ["/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm", "link", "serve", "--fleet", "lab"]
/// ```
///
/// The command is executed directly (no shell): a string is split on
/// whitespace, an array is the argv, one word per element
/// ([`aterm_types::fabric_command`]). The
/// child inherits only two socketpair ends at fds 3 and 4; its authority is the
/// descriptor, with no token or socket path to steal.
///
/// The instance supervises it and relaunches with back-off. When a descriptor
/// closes, existing holds stay and governed sessions keep running with
/// `status hold_advisory=fabric-lost`; `hold_on_bridge_lost = true` instead
/// holds each `reason=fabric-lost origin=fleet` before any relaunch.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct FabricConfig {
    /// The bridge command: a line or an argv ([`FabricCommand`]). Absent,
    /// blank or an empty array ⇒ the fabric is off.
    pub(crate) command: Option<FabricCommand>,
    /// Whether a lost bridge holds the sessions it governed
    /// (`crate::fabric::bridge_lost`). Absent means `false` (advisory only).
    pub(crate) hold_on_bridge_lost: Option<bool>,
}

/// `[fabric] command` as the argv it names, from EITHER spelling
/// ([`aterm_types::fabric_command`]): a string, split on whitespace (the form
/// every existing config has), or an array of strings, each element one word
/// passed as written. Empty for a blank string or an empty array (the fabric is
/// off). An element that is empty or holds a NUL, an element that is not a
/// string, and a value of any other type fail the parse with the reason named,
/// as every other config type error does.
#[derive(Default, Clone, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub(crate) struct FabricCommand(pub(crate) Vec<String>);

impl<'de> serde::Deserialize<'de> for FabricCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = FabricCommand;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(aterm_types::fabric_command::EXPECTED)
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<FabricCommand, E> {
                Ok(FabricCommand(aterm_types::fabric_command::split_line(s)))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<FabricCommand, A::Error> {
                let mut argv = Vec::new();
                while let Some(word) = seq.next_element::<String>()? {
                    aterm_types::fabric_command::check_word(argv.len(), &word)
                        .map_err(<A::Error as serde::de::Error>::custom)?;
                    argv.push(word);
                }
                Ok(FabricCommand(argv))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// The `[fleet]` table: this machine's fleet listener, off by default.
///
/// ```toml
/// [fleet]
/// listen = "0.0.0.0:7100"   # numeric IP:port; absent ⇒ no fleet port
/// allow = "facts"           # or "content", or "drive"; absent ⇒ facts
/// drive_from = ["<node>:<key>"] # at "drive", who may drive; absent ⇒ nobody
/// watch = true              # read the fleet every minute; absent ⇒ false
/// ```
///
/// A peer is admitted only from a loopback, private or link-local address, and
/// only with a proof of the fleet secret in `<state>/fleet.key`; with no key the
/// listener does not start and `status` says `fleet=refused:no-key`. `allow`
/// is the level the listener proves and a fleet `survey` will honor: `facts`
/// (programs, agent states, roles, task records, models; no text a person or
/// a program wrote) or `content` (the tab titles a person set, terminal
/// titles, cwds, prompts, an agent's newest words as well). A fleet peer runs
/// only the verbs whose verb-table row is marked `fleet` — `version`, and
/// `survey` at this level, never more. Followed live: a saved edit rebinds the
/// port.
///
/// `[[fleet.peers]] host = "<host[:port]>"` names machines by hand, beside
/// the issuer's book of peers: `aterm fleet peers` lists and checks them, and
/// the listener reads none of it.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct FleetConfig {
    /// The bind address, numeric `IP:port` (`"0.0.0.0:7100"`, `"[::]:7100"`):
    /// a hostname is refused, so the listener never waits on DNS. Listen on
    /// IPv4: the listener counts an IPv6 peer by its /64, and every IPv6 host
    /// of one LAN segment shares one, so there an attacker's idle sockets and
    /// a member are one address and only the slower of the listener's two
    /// fairness rules keeps the member in (`aterm_net::fleet::serve`).
    pub(crate) listen: Option<String>,
    /// `"facts"`, `"content"` or `"drive"`: the level a peer is served.
    /// Absent means facts. `drive` reads as content and lets the nodes
    /// `drive_from` names drive this Mac's sessions (the step-in).
    pub(crate) allow: Option<String>,
    /// The nodes that may drive this Mac's sessions at `allow = "drive"`,
    /// each `<node id>:<key hex>`, the key the book bound it to when the
    /// grant was made (`aterm fleet allow drive --from <host|node>` writes
    /// it): none, nobody, whatever the level. A bare node id grants no one.
    pub(crate) drive_from: Option<Vec<String>>,
    /// Read the fleet every minute for the menu-bar item's Fleet rows and its
    /// count (`fleet_glance`), where this Mac is no issuer: the issuer reads
    /// it every minute anyway, and any other Mac reads it when the menu
    /// opens. Absent means false. The listener reads none of it.
    pub(crate) watch: Option<bool>,
}

impl FleetConfig {
    /// `watch`, absent as false.
    pub(crate) fn watch_or_default(&self) -> bool {
        self.watch.unwrap_or(false)
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
#[cfg_attr(test, derive(Debug))]
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
    /// a stage no handoff landed stays for the next launch's in-session lane, which
    /// waits for the click too. A next launch swaps it at boot only if it is a
    /// replaceable one (`aterm_update::ImageReplacement::Replaceable`: `--headless`, an
    /// explicit control socket), never a macOS window with this off.
    pub(crate) auto_apply: Option<bool>,
    /// Require every staged bundle to be signed by this Team ID and accepted by
    /// Gatekeeper. Shipped aterm compiles its Team ID in, which always wins; this
    /// matters only to forks with an empty compiled pin, where absent means
    /// structural `codesign --verify` only (authenticity then comes from the
    /// channel, the manifest SHA-256 and any configured Ed25519 signature).
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

/// The `update.auto_apply` answer; `[update] auto_apply` is the only control.
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
/// See the macOS variant above.
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

/// aterm's own theme for `appearance`, a side of Default's light/dark pair:
/// Default on a dark appearance, Default Light on a light one. A
/// Follow-system pair seeds it on a side its theme has no partner for (so
/// Follow system turned on over an unset theme writes
/// `dark:Default,light:Default Light`), and a slot's Reset puts it back. An
/// unset `theme` is not this pair: it draws Default on every appearance.
pub(crate) fn default_pair_side(appearance: aterm_types::Appearance) -> &'static str {
    match appearance {
        aterm_types::Appearance::Dark => "Default",
        aterm_types::Appearance::Light => "Default Light",
    }
}

/// Resolve a plain or appearance-split theme selector permissively through
/// the one grammar ([`aterm_types::scheme::ThemeSelector`]): a segment it cannot
/// use is ignored (and reported by [`theme_selector_notice`]), and a side the
/// value omits draws the named side's built-in pair, else the named side.
/// Shared by the renderer and Settings preview.
pub(crate) fn resolve_theme_name_value(
    raw: Option<&str>,
    appearance: aterm_types::Appearance,
) -> Option<String> {
    let (selector, _) = aterm_types::scheme::ThemeSelector::parse(raw?);
    selector.name_for(appearance).map(str::to_string)
}

/// The selector issues in a `theme` value, as one sentence that also says what
/// each appearance draws instead; `None` when the value parses cleanly.
pub(crate) fn theme_selector_notice(raw: Option<&str>) -> Option<String> {
    use aterm_types::scheme::ThemeSelector;
    let (selector, issues) = ThemeSelector::parse(raw?);
    if issues.is_empty() {
        return None;
    }
    let mut message = issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    // What a split draws for the side it leaves unset: the named side's
    // built-in pair, else the named side. With no name at all, Default.
    let unset = |side| selector.name_for(side).map(str::to_string);
    let consequence = match (&selector, selector.name_for(aterm_types::Appearance::Dark)) {
        (ThemeSelector::Single(_), None) => Some("Default is used".to_string()),
        (_, None) => Some("both modes use Default".to_string()),
        (ThemeSelector::Split { dark: None, .. }, Some(_)) => {
            unset(aterm_types::Appearance::Dark).map(|name| format!("dark mode uses {name:?}"))
        }
        (ThemeSelector::Split { light: None, .. }, Some(_)) => {
            unset(aterm_types::Appearance::Light).map(|name| format!("light mode uses {name:?}"))
        }
        _ => None,
    };
    if let Some(consequence) = consequence {
        message.push_str("; ");
        message.push_str(&consequence);
    }
    Some(message)
}

impl Config {
    /// Parse `aterm.toml` text: the typed config plus the `[harness]` policy
    /// ([`HarnessPolicy::read`]) and the `[ai]` table with its fleet document
    /// ([`AiPolicy::read`]). Every load (launch and config service) goes
    /// through here.
    pub(crate) fn parse(text: &str) -> Result<Self, aterm_toml::Error> {
        let mut config: Self = aterm_toml::from_str(text)?;
        config.harness = HarnessPolicy::read(text);
        config.ai = AiPolicy::read(text);
        Ok(config)
    }

    /// Config used when `text` does not parse: all defaults, but `[harness]`
    /// lines still read (its reader is line-based), so a limit there survives a
    /// typo elsewhere. `[ai]`'s reader reads a file that is not TOML as an
    /// empty table, with a note.
    pub(crate) fn stand_in(text: &str) -> Self {
        Self {
            harness: HarnessPolicy::read(text),
            ai: AiPolicy::read(text),
            ..Self::default()
        }
    }

    /// Scheme name this config selects for `appearance`, honouring
    /// `theme = "dark:<name>,light:<name>"`. A plain value applies to both; an
    /// omitted side draws the named side's built-in pair, else the named side,
    /// and `None` (built-in Default) means no name at all. Prefixes are case-
    /// and space-insensitive; the name keeps its case.
    pub(crate) fn resolve_theme_name(&self, appearance: aterm_types::Appearance) -> Option<String> {
        resolve_theme_name_value(self.theme.as_deref(), appearance)
    }

    /// The renderer theme (clear colour, cursor, selection): the resolved
    /// colours' chrome ([`Self::resolved_colors`]).
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
    /// catalog carried by the active config snapshot: the projection of
    /// [`Self::resolved_colors`].
    pub(crate) fn theme_for_with_assets(
        &self,
        appearance: aterm_types::Appearance,
        themes: &ThemeCatalog,
    ) -> Theme {
        self.resolved_colors(appearance, themes, None).theme()
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

    /// The warm spare's mode (`spare_shell`, default `auto`; an unknown value
    /// reads `auto`).
    pub(crate) fn spare_shell_mode(&self) -> crate::spare::SpareMode {
        self.spare_shell
            .as_deref()
            .and_then(crate::spare::SpareMode::parse)
            .unwrap_or(crate::spare::SpareMode::Auto)
    }

    /// `spare_shell_needs_person` (default true).
    pub(crate) fn spare_shell_needs_person_or_default(&self) -> bool {
        self.spare_shell_needs_person.unwrap_or(true)
    }

    /// `--validate-config` warning for an unknown `spare_shell` (at load it
    /// reads `auto`).
    pub(crate) fn spare_shell_warning(&self) -> Option<String> {
        let raw = self.spare_shell.as_deref()?;
        if crate::spare::SpareMode::parse(raw).is_some() {
            return None;
        }
        Some(format!(
            "spare_shell: {raw:?} is not a spare mode — reading auto; expected one of {}",
            crate::spare::SpareMode::SPELLINGS.join("|")
        ))
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

    /// Whether aterm posts its own notices to the desktop. Opt-IN (default
    /// off; needs only once on); see the field doc.
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

    /// Whether a cursor trail is drawn at all: the configured style
    /// ([`Self::cursor_trail_style_raw`], the retired master folded in) is not
    /// Off. Each cursor move then ignites a ~260 ms aurora that decays to zero
    /// at idle; the GPU bloom pass runs only on effect frames and sheds under
    /// load. The default style also draws a resident cat over output, which is
    /// why Windows defaults the style to Off.
    pub(crate) fn cursor_trail_drawn(&self) -> bool {
        !self.cursor_trail_style_raw().eq_ignore_ascii_case("off")
    }

    /// Typing-momentum glow switch. Default [`DEFAULT_DECORATIVE_EFFECTS`]: on
    /// except on Windows. The retired master still reads: `cursor_trail =
    /// false` keeps it off, `cursor_trail = true` is its default where it is
    /// unset.
    pub(crate) fn cursor_momentum_glow_or_default(&self) -> bool {
        if self.cursor_trail == Some(false) {
            return false;
        }
        self.cursor_momentum_glow
            .unwrap_or(self.cursor_trail.unwrap_or(DEFAULT_DECORATIVE_EFFECTS))
    }

    /// Whether a config apply should warm the demand-driven effect pipelines
    /// (`aterm_gpu::EffectPipeline`) off the frame path.
    ///
    /// Effect pipelines are built lazily by `encode_frame`; this only decides
    /// whether a config apply compiles them early to avoid a first-frame hitch.
    /// Keyed on a drawn trail ([`Self::cursor_trail_drawn`]) because it alone
    /// can bind the pipelines with a visible build cost (`fire_add`,
    /// `fire_over`, `rain_glow`, `rain_glow_over`); with the style Off, warming
    /// would compile pipelines that are never drawn.
    pub(crate) fn warms_effect_pipelines(&self) -> bool {
        self.cursor_trail_drawn()
    }

    /// Trail sound effects on/off (`trail_sounds`, default on; every gate that
    /// silences the trail's light silences them too).
    pub(crate) fn trail_sounds_or_default(&self) -> bool {
        self.trail_sounds.unwrap_or(true)
    }

    /// Tone-melody on/off (`tone_melody`, default on; the field doc lists what it
    /// moves and the gates it inherits).
    pub(crate) fn tone_melody_or_default(&self) -> bool {
        self.tone_melody.unwrap_or(true)
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

    /// Whether a fire is on `[sparkle_words.tricks] ignore_words`: its `word`
    /// (the trick token as typed) or one of its `commands`. A name glued to
    /// its command fires the whole run, and its command is the other half, so
    /// `坐下` on the list holds back `猫咪坐下` too, and a listed name holds
    /// back nothing. Both sides use the lexicon's fold without elongation
    /// folding (`purr` does not cover `purrrrr`). Allocates only when the list
    /// is non-empty; runs once per fire.
    pub(crate) fn kitty_trick_ignored<'w>(
        &self,
        word: &str,
        commands: impl IntoIterator<Item = &'w str>,
    ) -> bool {
        let Some(ignore) = self
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.tricks.as_ref())
            .and_then(|t| t.ignore_words.as_deref())
            .filter(|words| !words.is_empty())
        else {
            return false;
        };
        let listed = |surface: &str| {
            let folded = aterm_lexicon::fold(surface);
            ignore.iter().any(|w| aterm_lexicon::fold(w) == folded)
        };
        listed(word) || commands.into_iter().filter(|c| *c != word).any(listed)
    }

    /// Secure keyboard entry (default off, like every security opt-in: it
    /// suppresses other apps' global hotkeys while active).
    pub(crate) fn secure_keyboard_entry_or_default(&self) -> bool {
        self.secure_keyboard_entry.unwrap_or(false)
    }

    /// Ambient-bed on/off (`trail_sound_bed`, default on).
    pub(crate) fn trail_sound_bed_or_default(&self) -> bool {
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
    /// is attached (`wallpaper_text_tint`). Default on.
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
    /// help; unechoed input is never shown. `always` is the aggressive variant:
    /// it can briefly show a key at a remote prompt that does not echo; a local
    /// one (a tty with echo off) is never predicted in any mode.
    pub(crate) fn predictive_echo_or_default(&self) -> &str {
        self.predictive_echo
            .as_deref()
            .map(str::trim)
            .unwrap_or("adaptive")
    }

    /// Predictive editing (`aterm_predict::Predictor::set_editing`). Default on: it
    /// shows only what the shell's own echoes have proven, and with
    /// `predictive_echo = "off"` nothing is predicted at all.
    pub(crate) fn predictive_echo_editing_or_default(&self) -> bool {
        self.predictive_echo_editing.unwrap_or(true)
    }

    /// Focus-linked shell priority boost. Default on: a few syscalls per focus or
    /// tab change and none while typing; touches only the shell root and its
    /// console host.
    pub(crate) fn focus_boost_or_default(&self) -> bool {
        self.focus_boost.unwrap_or(true)
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
    /// is a colour. `None` → the caller falls back to the themed cursor colour
    /// (and `cursor`, [`Self::trail_color`], is that colour chosen).
    pub(crate) fn cursor_trail_color_u32(&self) -> Option<u32> {
        match self.trail_color() {
            aterm_effects::cursor_fx::TrailColor::Pinned(color) => Some(color),
            _ => None,
        }
    }

    /// What `cursor_trail_color` says ([`crate::prefs::trail_color_setting`]):
    /// each style's automatic colour, the cursor's colour chosen, or a colour.
    pub(crate) fn trail_color(&self) -> aterm_effects::cursor_fx::TrailColor {
        crate::prefs::trail_color_setting(self.cursor_trail_color.as_deref())
    }

    /// The cursor family's knobs as this config sets them, for a trail that
    /// may draw (`enabled`: Serious Mode folded in by the host) against
    /// `theme_cursor` in `presentation` (its brightness default is the
    /// style's, [`Self::cursor_trail_intensity_for`]): the one reading
    /// [`resolve_cursor_glow`] takes from a window's config.
    pub(crate) fn cursor_glow_inputs(
        &self,
        enabled: bool,
        theme_cursor: u32,
        presentation: ResolvedTrailPresentation,
    ) -> CursorGlowInputs {
        CursorGlowInputs {
            enabled,
            color: self.trail_color().knob(theme_cursor),
            accent: self.cursor_trail_accent_u32(),
            duration_ms: self.cursor_trail_ms_or_default(),
            length: self.cursor_trail_length_or_default(),
            intensity: self.cursor_trail_intensity_for(presentation),
            radius: self.cursor_trail_radius_or_default(),
            ring: self.cursor_trail_ring_or_default(),
        }
    }

    /// The primary-font request: `font_family`, the person's own font. A
    /// display face never stands in for it — display faces are a layer over
    /// it ([`Self::display_faces_request`]), so this font always sets the grid,
    /// draws what the layer lacks, and is the chrome's font. A legacy
    /// `font_family = "display:pixel"` names the layer, not a font, so it
    /// requests the default font, and so does a blank family (aterm's own
    /// choice: the default rung). Every font resolution site goes through this.
    pub(crate) fn font_family_request(&self) -> Option<String> {
        self.font_family.clone().filter(|family| {
            !family.trim().is_empty() && aterm_render::display_face_ids_for_family(family).is_none()
        })
    }

    /// The display layer drawn over the primary font: `display_font`'s faces
    /// (a `+`-joined mix of up to three), canonical, deduplicated and in
    /// registry order ([`aterm_render::display_layer_ids`]), so the toggle
    /// order changes nothing. Unknown ids and `"off"` are dropped. With
    /// `display_font` unset, a legacy `font_family = "display:…"` still names
    /// its faces here.
    pub(crate) fn display_faces_request(&self) -> Vec<&'static str> {
        if let Some(raw) = self.display_font.as_deref() {
            return aterm_render::display_layer_ids(raw.split('+'));
        }
        self.font_family
            .as_deref()
            .and_then(aterm_render::display_face_ids_for_family)
            .map(aterm_render::display_layer_ids)
            .unwrap_or_default()
    }

    /// The aurora accent-colour override as packed `0x00RRGGBB`, if set + valid.
    pub(crate) fn cursor_trail_accent_u32(&self) -> Option<u32> {
        self.cursor_trail_accent
            .as_deref()
            .and_then(aterm_types::scheme::parse_hex)
            .map(rgb_u32)
    }

    /// Configured cursor-trail style, trimmed but not lowercased. Borrowed to
    /// avoid allocating at status, validation and preview seams. Unset, the
    /// platform's default ([`default_cursor_trail_style`]). The retired master
    /// still reads: `cursor_trail = false` is Off whatever the style says, and
    /// `cursor_trail = true` with no style names the default style.
    pub(crate) fn cursor_trail_style_raw(&self) -> &str {
        drawn_cursor_trail_style(self.cursor_trail, self.cursor_trail_style.as_deref())
    }

    /// Trail brightness for the resolved style, clamped 0.0..=1.0. Unset, the
    /// style's own default (`aterm_effects::cursor_fx::default_trail_intensity`:
    /// 1.0, and Classic's spectrum face the v0.28 release's 0.7; a trail
    /// colour, the cursor's chosen included, draws its one-colour face).
    /// Non-finite values (`nan` is valid TOML) read as `0.0`, since NaN would
    /// slip past every downstream `intensity <= 0.0` check.
    pub(crate) fn cursor_trail_intensity_for(
        &self,
        presentation: ResolvedTrailPresentation,
    ) -> f32 {
        let mono = presentation.classic_mono || self.trail_color().chosen();
        let v = self.cursor_trail_intensity.unwrap_or(
            aterm_effects::cursor_fx::default_trail_intensity(presentation.style.style, mono),
        );
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

    /// The languages whose ambiguous homograph lexicon entries are un-gated.
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

    /// Resolve the non-feed part of a catalog while the Sparkle generation is
    /// prepared separately: admits the kitty sprite and the trail style.
    pub(crate) fn resolve_preliminary_asset_catalog_with_themes(
        &self,
        themes: std::sync::Arc<ThemeCatalog>,
    ) -> std::sync::Arc<ConfigAssetCatalog> {
        std::sync::Arc::new(ConfigAssetCatalog {
            trail_style: std::sync::Arc::new(self.resolve_trail_style_cache()),
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
            trail_style: std::sync::Arc::new(self.resolve_trail_style_cache()),
            kitty_sprite: resolve_kitty_sprite_asset(self.cursor_nyan_sprite.as_deref()),
            wallpaper: resolve_wallpaper_asset(self.wallpaper.as_deref()),
            themes,
            sparkle_spec_consumers: None,
        })
    }

    /// This config's trail presentation, resolved once per generation.
    pub(crate) fn resolve_trail_style_cache(&self) -> TrailStyleCache {
        TrailStyleCache::for_style(self.cursor_trail_style_raw())
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
        PathFeedFps {
            deco: deco.finish(),
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

    /// Resolve the pack/config bundle (the deco config and the lexicon override
    /// TOML), never per frame; the config diagnostics and tests call it directly.
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

    /// Compile the complete sparkle runtime (production goes through
    /// [`Self::prepare_path_feed_generation`]).
    #[cfg(test)]
    pub(crate) fn prepare_sparkle_runtime(&self) -> PreparedSparkleRuntime {
        self.prepare_sparkle_runtime_with_fingerprint(TrickVocabulary::compiled)
            .0
    }

    fn prepare_sparkle_runtime_with_fingerprint(
        &self,
        tricks: fn(Vec<String>) -> TrickVocabulary,
    ) -> (PreparedSparkleRuntime, u64) {
        let (parts, fingerprint) = self.sparkle_runtime_parts_with_fingerprint();
        let langs = self.sparkle_languages();
        // The kitty-command vocabulary uses the same `languages` as the word
        // lexicon and is compiled whether or not a word engine resolved.
        let tricks = tricks(langs.clone());
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
    /// Sparkle lexicon and Toy Packs (unless `[sparkle_words]` is disabled),
    /// capped at [`MAX_ACTIVE_TOY_PACKS`] and expanded like the loaders, and the
    /// AI fleet document ([`AiPolicy`]). The config watcher stats these beside
    /// `aterm.toml`; [`PathFeedFps`] decides from the bytes whether anything
    /// changed — and for the fleet document, which the parse reads whole,
    /// `Config`'s own `PartialEq`.
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
        paths.extend(self.ai.fleet_paths.iter().cloned());
        paths
    }

    /// Every file besides `aterm.toml` whose content a warm of this config
    /// presents, by the path its reader opens (warm successor P2): the user
    /// theme file each appearance selects (`<theme dir>/<name>.conf`; a
    /// built-in name, or one that is not a file stem, opens none), the custom
    /// kitty sprite, the wallpaper, and the path feeds
    /// ([`Self::path_feed_paths`]). At most `2 + 2 + 1 +
    /// MAX_ACTIVE_TOY_PACKS + 1` paths (the last the AI fleet document).
    /// `main_entry` stamps them beside the file's text before its early warm
    /// (`crate::WarmConfigRead`), so an edit to one during the dial's hold is
    /// warmed again before any carried present.
    ///
    /// Not listed, on purpose:
    /// * the rest of the theme directory: only the selected file reaches a
    ///   present (another changes the Settings picker's list, which the watcher
    ///   reconciles after Commit). The selected file is stamped at the config's
    ///   spelling, so case variants and files past [`MAX_USER_THEME_FILES`] are
    ///   not seen.
    /// * fonts: no running process re-reads a font on a change.
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
    /// same-handle bytes that entered its parser.
    pub(crate) fn prepare_path_feed_generation(&self) -> PreparedPathFeedGeneration {
        self.prepare_path_feed_generation_with(TrickVocabulary::compiled)
    }

    /// [`Self::prepare_path_feed_generation`] for the launch's config-runtime
    /// worker, which the boot joins before the first window: identical, except
    /// that the kitty-command vocabulary compiles on its own worker
    /// ([`TrickVocabulary::compiling`]) instead of inside that join.
    pub(crate) fn prepare_launch_path_feed_generation(&self) -> PreparedPathFeedGeneration {
        self.prepare_path_feed_generation_with(TrickVocabulary::compiling)
    }

    fn prepare_path_feed_generation_with(
        &self,
        tricks: fn(Vec<String>) -> TrickVocabulary,
    ) -> PreparedPathFeedGeneration {
        let (sparkle, deco) = self.prepare_sparkle_runtime_with_fingerprint(tricks);
        PreparedPathFeedGeneration {
            sparkle,
            fingerprints: PathFeedFps { deco },
        }
    }

    /// The `[sparkle_words] enabled` master, resolved in one place.
    ///
    /// Default on everywhere, outside [`DEFAULT_DECORATIVE_EFFECTS`]: matched
    /// words' heads draw `FreeZ::UnderText` so glyphs stay on top, the on-screen
    /// population is capped (`MAX_CATS = 8`), and Settings treats this key as a
    /// live product gate. The one over-text path is the profanity supernova
    /// burst, capped at one (`supernova::MAX_ACTIVE_SUPERNOVAE`) and tunable via
    /// `supernova_chance`.
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
        // margin needs the higher sweep floor.
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
            .filter_map(|s| aterm_types::scheme::parse_hex(s))
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
            // Default on, so the literal `cat` decorates too (the `cat` shell
            // command included); `allow_bare_cat = false` opts out.
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

    /// The `[presence] rim` RESOLVED bit (default TRUE): whether a window
    /// draws its needs border and pill. See [`PresenceConfig`].
    pub(crate) fn presence_rim_enabled(&self) -> bool {
        self.presence.as_ref().and_then(|p| p.rim).unwrap_or(true)
    }

    /// The `[claude_footer] usage` bit (default on): whether the Claude footer
    /// shows `Σ`, working time and tokens. See [`ClaudeFooterConfig`].
    pub(crate) fn claude_footer_usage(&self) -> bool {
        self.claude_footer
            .as_ref()
            .and_then(|c| c.usage)
            .unwrap_or(true)
    }

    /// The `[claude_footer] card` bit (default on): whether Claude Code's
    /// launch card wears the harness card while the harness carries the
    /// session, or holds it Off (in white). See [`ClaudeFooterConfig::card`].
    pub(crate) fn claude_footer_card(&self) -> bool {
        self.claude_footer
            .as_ref()
            .and_then(|c| c.card)
            .unwrap_or(true)
    }

    /// The `[claude_footer] acts` RESOLVED bit (default TRUE): whether the
    /// act ticker's label lights right of the Autopilot chip. See
    /// [`ClaudeFooterConfig::acts`].
    pub(crate) fn claude_footer_acts(&self) -> bool {
        self.claude_footer
            .as_ref()
            .and_then(|c| c.acts)
            .unwrap_or(true)
    }

    /// The `[claude_footer] agents` RESOLVED bit (default TRUE): whether
    /// Claude Code's agents panel wears the harness theme's rainbow and its
    /// dots their light while the harness's glass is on. See
    /// [`ClaudeFooterConfig::agents`].
    pub(crate) fn claude_footer_agents(&self) -> bool {
        self.claude_footer
            .as_ref()
            .and_then(|c| c.agents)
            .unwrap_or(true)
    }

    /// `[harness.<agent>] enabled` as the supervisor's reader took it: whether
    /// `agent` is left out of its `off` list. The agent's own switch, apart
    /// from the master switch (`SupervisorConfig::on_for` is both); default
    /// on, and an off spelling or a misplaced `false` reads off.
    pub(crate) fn harness_agent_enabled(&self, agent: aterm_phase::Program) -> bool {
        !self.harness.policy.off.contains(&agent)
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

    /// `[machine] rosetta`, resolved as `atpkg::config::MachineConfig::rosetta` (absent ⇒
    /// `true`).
    pub(crate) fn machine_rosetta(&self) -> bool {
        self.machine
            .as_ref()
            .and_then(|m| m.rosetta)
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

    /// The `[privacy]` `notice` bit (default true): the access card's question,
    /// the launch hand-over of the Full Disk Access page and the once-per-Mac
    /// debugger-access launch ask (`debugger_access::admit_launch_ask`). Also
    /// gated by the master; `--validate-config` warns on `enabled = false`
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

    /// The `[privacy]` `warmup` mode (default [`PrivacyWarmup::Automatic`]).
    /// Unknown spellings resolve to the default and are named by
    /// `--validate-config`.
    pub(crate) fn privacy_warmup(&self) -> PrivacyWarmup {
        self.privacy
            .as_ref()
            .and_then(|p| p.warmup.as_deref())
            .and_then(PrivacyWarmup::parse)
            .unwrap_or_default()
    }

    /// The `[privacy]` `warmup_folders` names (default
    /// `aterm_containment::Folder::WARMUP_DEFAULT`). Names only: paths resolve in
    /// `aterm_containment::consent::folder_paths`, because a protected path
    /// literal here would raise a system modal (the census guard
    /// `protected-folder`).
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

    /// The `[privacy]` `observer` key as a mode: absent is the
    /// [`ObserverMode::Sentinel`], `false` is off, `true` is the full stream.
    ///
    /// Config only — `crate::consent_observer` takes this resolved value as an
    /// argument and reads no environment knob of its own, so a program inside
    /// a session has nothing to flip. Whether a sentinel actually STARTS is
    /// the host's call (`App::effective_observer_mode`): every mode needs the
    /// master switch on, and the sentinel needs the card's own switches too,
    /// since it exists only to feed the card.
    pub(crate) fn privacy_observer_mode(&self) -> crate::consent_observer::ObserverMode {
        use crate::consent_observer::ObserverMode;
        match self.privacy.as_ref().and_then(|p| p.observer) {
            None => ObserverMode::Sentinel,
            Some(false) => ObserverMode::Off,
            Some(true) => ObserverMode::Full,
        }
    }

    /// `[privacy] observer = false`: aterm's reading of tccd's log is off, which
    /// also stops the Developer Tools row's `log show`. A plain read, so the
    /// control modules can ask it without naming the observer.
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
    /// `default_bg`/`theme_fg`/`theme_accent` (the chrome accent,
    /// [`crate::App::grid_accent`]) are the live renderer
    /// theme (`0x00RR_GGBB`).
    pub(crate) fn matrix_rain_params(
        &self,
        default_bg: u32,
        theme_fg: u32,
        theme_accent: u32,
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
            Some(h) if h.starts_with('#') => match aterm_types::scheme::parse_hex(h) {
                Some(c) => RainHue::Custom(rgb_u32(c)),
                // Malformed hex fails closed to the stock green: a typo never
                // fails the load or invents a colour.
                None => RainHue::Matrix,
            },
            _ => RainHue::Matrix,
        };
        // `materialize`, `ink_text` and `phosphor` are parsed but not forwarded.
        let _ = mr.materialize;
        let _ = mr.ink_text;
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
            theme_accent: theme_accent & 0x00FF_FFFF,
        }
    }

    /// The `[output_streak]` master (`docs/DESIGN-output-streak-2026-08-30.md`
    /// §7), default [`DEFAULT_DECORATIVE_EFFECTS`]: off on Windows, where
    /// unprompted decoration over program output is off by default. Reduced
    /// motion (the engine fully resets) and a per-session `aterm ctl streak off`
    /// also turn it off.
    pub(crate) fn output_streak_enabled_or_default(&self) -> bool {
        self.output_streak
            .as_ref()
            .and_then(|streak| streak.enabled)
            .unwrap_or(DEFAULT_DECORATIVE_EFFECTS)
    }

    /// Whether the output streak plays sound (`[output_streak] sound`, default
    /// on). A plain `true` rather than [`DEFAULT_DECORATIVE_EFFECTS`] since a
    /// sound covers no pixel; cues are recorded only on spawn edges, so with the
    /// master off it is silent anyway. The synth host is macOS-only;
    /// `trail_sounds`, `trail_sound_volume`, window focus and
    /// `SeriousEffect::TerminalSound` apply first.
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

    /// Warning for `[output_streak]`: `sound = true` under `enabled = false` is
    /// unreachable (cues are recorded only on edges that also emit light).
    /// Out-of-band numeric keys are documented clamps and are not warned. Not yet
    /// wired into `--validate-config` (`crate::diagnostics`), so test-only.
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
    /// every platform: it blooms the trail's light, so it is inert wherever no
    /// trail is drawn. The half-res blur runs only on
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
    /// 0.5..=8.0: an explicit `cursor_trail_bloom_radius`, else
    /// [`Self::bloom_radius_style_default`]. Takes the pre-parsed
    /// [`ResolvedTrailPresentation`] so the GPU pin seams parse no strings.
    /// Non-finite values fall back to the per-style default.
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

    /// Renderer text-shaping config from `ligatures`, `cursor_break_ligatures`,
    /// `font_features` and `merged_ligatures`; `ligatures = false` wins over
    /// `cursor_break_ligatures`. `ambiguous_width` goes through the engine
    /// (`ambiguous_width_double`), not here.
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
            // Opt-in N:1 merged-ligature slicing (default off).
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

    /// macOS `font_thicken` (CoreText font smoothing at raster time). Default off.
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

    /// `font_weight_dark_nudge`, default 0 (off), clamped 0..=300; non-finite
    /// values fall back to off.
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

    /// Line-height multiplier, default [`DEFAULT_LINE_HEIGHT`] (`1.1` on
    /// Windows, `1.0` elsewhere), clamped 0.8..=2.0 (the renderer floors at
    /// 0.5). Non-finite values mean the default.
    pub(crate) fn line_height_or_default(&self) -> f32 {
        match self.line_height {
            Some(v) if v.is_finite() => v.clamp(0.8, 2.0),
            _ => DEFAULT_LINE_HEIGHT,
        }
    }

    /// The text size scale in thousandths: `font_size_scale` clamped to
    /// 0.8..=1.25 and rounded, `1000` when absent or not finite.
    pub(crate) fn font_size_scale_milli_or_default(&self) -> u16 {
        match self.font_size_scale {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0.8..=1.25 first, so the thousandths are 800..=1250"
            )]
            Some(v) if v.is_finite() => (f64::from(v.clamp(0.8, 1.25)) * 1000.0).round() as u16,
            _ => 1000,
        }
    }

    /// Baseline shift in px, default `0`, clamped ±32 (the renderer clamps again
    /// at ±64).
    pub(crate) fn adjust_baseline_or_default(&self) -> i32 {
        self.adjust_baseline
            .map_or(0i64, |v| v.clamp(-32, 32))
            .try_into()
            .unwrap_or(0)
    }

    /// Underline position shift in px, default `0`, clamped ±32 (the renderer
    /// clamps again at ±64 and re-clamps the band in-cell).
    pub(crate) fn adjust_underline_position_or_default(&self) -> i32 {
        self.adjust_underline_position
            .map_or(0i64, |v| v.clamp(-32, 32))
            .try_into()
            .unwrap_or(0)
    }

    /// Underline thickness delta in px, default `0`, clamped ±32 (the renderer
    /// clamps again at ±64 and floors the thickness at 1px).
    pub(crate) fn adjust_underline_thickness_or_default(&self) -> i32 {
        self.adjust_underline_thickness
            .map_or(0i64, |v| v.clamp(-32, 32))
            .try_into()
            .unwrap_or(0)
    }

    /// Descender ink-skip, default on.
    pub(crate) fn underline_skip_descenders_or_default(&self) -> bool {
        self.underline_skip_descenders.unwrap_or(true)
    }

    /// Whether programs may set the 256 indexed colours (OSC 4 / numeric
    /// OSC 21), default `true`: only an explicit `allow_palette_reconfigure =
    /// false` refuses them. The engine library's own default stays fail-closed;
    /// this is the app's choice.
    pub(crate) fn allow_palette_reconfigure_or_default(&self) -> bool {
        self.allow_palette_reconfigure.unwrap_or(true)
    }

    /// Per-cell minimum contrast ratio (W5b), default `1.0` (off — xterm
    /// treats 1 as "do nothing"), clamped to the WCAG 1.0..=21.0 domain.
    pub(crate) fn minimum_contrast_or_default(&self) -> f32 {
        match self.minimum_contrast {
            Some(v) if v.is_finite() => v.clamp(1.0, 21.0),
            _ => 1.0,
        }
    }

    /// The engine's theme-pair contrast floor
    /// (`TerminalConfig::indexed_minimum_contrast`): while `minimum_contrast`
    /// is absent, [`DEFAULT_INDEXED_MINIMUM_CONTRAST`] — 4.5:1 on Windows, off
    /// elsewhere; a set `minimum_contrast` floors every cell in the renderer,
    /// so this one is off.
    ///
    /// Windows programs draw on the theme's colours far more than their
    /// authors can see: pwsh writes every directory name as the default
    /// foreground on palette blue (`ESC[44;1m`), `Write-Host` puts white or
    /// black on any of the 16, and the console host maps legacy attributes
    /// onto them. Windows Terminal's default (`adjustIndistinguishableColors:
    /// indexed`) adjusts exactly those pairs and leaves truecolor alone, and so
    /// does this: a program's own RGB stays exact (images drawn in half
    /// blocks, Claude Code's palette), which a 4.5:1 `minimum_contrast`
    /// default would recolour.
    pub(crate) fn indexed_minimum_contrast_or_default(&self) -> f32 {
        if self.minimum_contrast.is_some() {
            1.0
        } else {
            DEFAULT_INDEXED_MINIMUM_CONTRAST
        }
    }

    /// Whether the selection band dims while unfocused. Default off.
    pub(crate) fn selection_inactive_or_default(&self) -> bool {
        self.selection_inactive.unwrap_or(false)
    }

    /// Whether a split marks the focused pane. Default on.
    pub(crate) fn split_focus_mark_or_default(&self) -> bool {
        self.split_focus_mark.unwrap_or(true)
    }

    /// Whether panes show the overlay scrollbar ([`DEFAULT_SCROLLBAR`] when
    /// unset).
    pub(crate) fn scrollbar_or_default(&self) -> bool {
        self.scrollbar.unwrap_or(DEFAULT_SCROLLBAR)
    }

    /// SGR 2 faint opacity (W5e), default `0.5`, clamped 0.0..=1.0.
    pub(crate) fn faint_opacity_or_default(&self) -> f32 {
        match self.faint_opacity {
            Some(v) if v.is_finite() => v.clamp(0.0, 1.0),
            _ => aterm_types::DIM_FACTOR,
        }
    }

    /// Window background opacity, default `1.0` (solid), clamped to 0.0..=1.0. A
    /// non-finite value fails safe to solid.
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

    /// Effective per-cell minimum contrast: the renderer's own law
    /// ([`aterm_render::vibrancy::effective_minimum_contrast`], which the renderer
    /// applies at blit time) evaluated on the config values, for Settings, link
    /// detection and the render knob. Translucent windows clear WCAG AA (4.5:1);
    /// opaque ones keep the configured floor exactly. The law is proven in
    /// aterm-render (`tests/vibrancy_contrast.rs`, bound to
    /// `aterm_models_render::vibrancy_contrast_model`); `effective_minimum_contrast_is_the_renderers_law`
    /// proves this delegates to it.
    pub(crate) fn effective_minimum_contrast(&self) -> f32 {
        aterm_render::vibrancy::effective_minimum_contrast(
            self.minimum_contrast_or_default(),
            self.background_opacity_or_default(),
        )
    }

    /// Warning when a configured `font_family` does not resolve to a font file,
    /// for the startup/reload notice and `--validate-config`. `family` is the
    /// effective family (`--font` > config).
    pub(crate) fn font_family_warning(family: Option<&str>) -> Option<String> {
        Self::font_family_admission(family).err()
    }

    /// Admission verdict behind [`Self::font_family_warning`]: `Ok(None)` for no
    /// family, `Ok(Some(face))` with the exact face — file (or `display:` name)
    /// and collection index — the backend should build from, `Err(warning)`
    /// otherwise.
    ///
    /// The one startup resolve of a configured family; it runs on the backend
    /// worker because it walks font directories and may parse `name` tables
    /// (milliseconds warm, seconds cold).
    pub(crate) fn font_family_admission(
        family: Option<&str>,
    ) -> Result<Option<aterm_render::FaceRef>, String> {
        let Some(fam) = family.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        match aterm_render::resolve_config_font(fam, aterm_render::FaceTarget::REGULAR) {
            Ok(face) => Ok(Some(face)),
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
    pub(crate) fn cursor_trail_style_warning(&self) -> Option<String> {
        let raw = self.cursor_trail_style.as_deref().map(str::trim)?;
        if !resolve_trail_style(raw).unknown {
            return None;
        }
        // The vocabulary is joined from `CURSOR_TRAIL_STYLES` so it matches the
        // picker. The near miss leads because the banner truncates one line
        // per notice at the terminal width.
        Some(format!(
            "cursor_trail_style: unknown style {raw:?}{suggestion} (falling back to \
             {default:?}; expected one of {options}, or a documented alias like \
             nyan rainbow/nyan/rainbow/tall rainbow/ember/ocean)",
            default = crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
            options = crate::prefs::CURSOR_TRAIL_STYLES.join("|"),
            // The near miss is drawn from every accepted spelling and named by
            // the canonical it resolves to, so an alias (`rainbow kity` →
            // `rainbow kitty`) still finds its look under the name Settings
            // writes.
            suggestion = crate::native_config_language::nearest_enum_value(
                raw,
                &crate::prefs::CURSOR_TRAIL_STYLES
                    .iter()
                    .copied()
                    .chain(
                        crate::prefs::CURSOR_TRAIL_STYLE_ALIASES
                            .iter()
                            .map(|&(alias, _)| alias),
                    )
                    .collect::<Vec<_>>(),
            )
            .and_then(crate::prefs::cursor_trail_style_canonical)
            .map_or_else(String::new, |near| format!(" — did you mean {near:?}?")),
        ))
    }

    /// The stats bars' form (`[sys] stats_bars`); the Deck by default.
    pub(crate) fn stats_bars_or_default(&self) -> crate::stats_deck::StatsBars {
        crate::stats_deck::StatsBars::parse(
            self.sys.as_ref().and_then(|sys| sys.stats_bars.as_deref()),
        )
    }

    /// The Deck's size setting (`[sys] stats_deck_size`); automatic by default.
    pub(crate) fn stats_deck_size_or_default(&self) -> crate::stats_plan::DeckSizePref {
        crate::stats_plan::DeckSizePref::parse(
            self.sys
                .as_ref()
                .and_then(|sys| sys.stats_deck_size.as_deref()),
        )
    }

    /// `[sys] disk_roots` as absolute folders: `~/…` against `home`, an
    /// absolute path as written, anything else (relative) dropped.
    pub(crate) fn sys_disk_roots(&self, home: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
        self.sys
            .as_ref()
            .and_then(|sys| sys.disk_roots.as_deref())
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| match entry.strip_prefix("~/") {
                Some(rest) => home.map(|h| h.join(rest)),
                None => Some(std::path::PathBuf::from(entry)).filter(|p| p.is_absolute()),
            })
            .collect()
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
    /// Default [`TabMenuChord::MenuKey`], so Shift+F10 (`kf22`) stays with the
    /// program. Unknown values warn and fall back to the default, never to `On`.
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
/// App caches, seeds at startup and diffs on reload. Each key maps to exactly
/// one [`KnobChange`] variant and each variant to one renderer call
/// (`App::apply_render_knob`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct RenderKnobs {
    /// `line_height` → `Backend::set_line_height` (cell geometry: re-grids).
    pub(crate) line_height: f32,
    /// `minimum_contrast` → `Backend::set_minimum_contrast` (blit-time).
    pub(crate) minimum_contrast: f32,
    /// The resolved selected-text foreground (`selection_foreground` in
    /// `aterm.toml`, else the theme's) → `Backend::set_selection_fg`
    /// (blit-time).
    pub(crate) selection_fg: Option<u32>,
    /// `selection_inactive` → `Backend::set_selection_inactive`, folded with the
    /// live focus at the per-window present seam (`redraw_window`), not at reload.
    pub(crate) selection_inactive: bool,
    /// `adjust_baseline` → `Backend::set_adjust_baseline` (placement-only).
    pub(crate) adjust_baseline: i32,
    /// `adjust_underline_position` + `adjust_underline_thickness` →
    /// `Backend::set_adjust_underline` (draw-time). One knob pair, one call.
    pub(crate) adjust_underline: (i32, i32),
    /// `underline_skip_descenders` → `Backend::set_underline_skip_descenders`
    /// (draw-time, default on).
    pub(crate) underline_skip_descenders: bool,
    /// `background_opacity` → `Backend::set_background_opacity` (present-time);
    /// `1.0` = solid, the default.
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
    /// Resolve the knob values from a config and the colours it resolves to.
    /// `colors` supplies the selected-text foreground, so a theme file's
    /// `selection_foreground` reaches the renderer as well as the engine.
    pub(crate) fn from_config(
        cfg: &Config,
        colors: &crate::resolved_colors::ResolvedColors,
    ) -> Self {
        Self {
            line_height: cfg.line_height_or_default(),
            // The effective floor: raised to WCAG AA while translucent.
            minimum_contrast: cfg.effective_minimum_contrast(),
            selection_fg: colors.selection_fg().map(rgb_u32),
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
/// setters drop the glyph cache on change. Applied by one setter list
/// (`Backend::pin_glyph_raster_knobs`) because the backend worker pins them
/// before its glyph warm and the join re-pins them; values that differ discard
/// the warm and the first frame rasterizes on the UI thread.
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
    /// Resolve the knobs from config. `main_entry` resolves once and hands the
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
        let config = Config::default();
        let colors = config.resolved_colors(
            aterm_types::Appearance::Dark,
            &ThemeCatalog::default(),
            None,
        );
        Self::from_config(&config, &colors)
    }
}

/// Resolved styled/fallback font config (faces and file paths, not family
/// names): the value the App caches, seeds and diffs on reload, like
/// [`RenderKnobs`]. Family resolution uses [`aterm_render::resolve_config_font`];
/// applied by [`crate::apply_font_config_to_backend`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct FontConfig {
    /// Styled faces `[bold, italic, bold-italic]`: each a file and the face's
    /// collection index in it (`Menlo Bold` is face 1 of `Menlo.ttc`). `None`
    /// means unset or unresolvable (warned); the renderer keeps
    /// discovery/synthetic.
    pub(crate) styled_paths: [Option<aterm_render::FaceRef>; 3],
    /// `font_synthetic_style` (default `true`).
    pub(crate) synthetic_style: bool,
    /// Resolved `fallback_fonts` paths, in config order (unresolvable entries
    /// warned + dropped, the rest still apply).
    pub(crate) fallback_fonts: Vec<String>,
    /// Resolved `symbol_font` path.
    pub(crate) symbol_font: Option<String>,
    /// Resolved `emoji_font` path.
    pub(crate) emoji_font: Option<String>,
    /// The display layer drawn over the primary (`display_font`, or a
    /// `--font display:` flag): registry ids in registry order
    /// ([`crate::effective_display_faces`]). Empty = no layer.
    pub(crate) display_faces: Vec<&'static str>,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            styled_paths: [None, None, None],
            synthetic_style: true,
            fallback_fonts: Vec::new(),
            symbol_font: None,
            emoji_font: None,
            display_faces: Vec::new(),
        }
    }
}

impl FontConfig {
    /// Resolve the font keys into paths plus warnings for entries that do not
    /// resolve (skipped, never a hard failure).
    pub(crate) fn from_config(cfg: &Config) -> (Self, Vec<String>) {
        let mut warns = Vec::new();
        let mut resolve = |key: &str,
                           fam: Option<&str>,
                           target: aterm_render::FaceTarget|
         -> Option<aterm_render::FaceRef> {
            let fam = fam.map(str::trim).filter(|s| !s.is_empty())?;
            match aterm_render::resolve_config_font(fam, target) {
                Ok(face) => Some(face),
                Err(error) => {
                    warns.push(format!(
                        "config {key}: {fam:?} is not an admissible font ({error}); ignored \
                         (see `aterm list-fonts`)"
                    ));
                    None
                }
            }
        };
        // Each style slot takes its own face of a named family: `font_family_bold
        // = "Menlo"` is Menlo's Bold.
        let styled = [
            ("font_family_bold", cfg.font_family_bold.as_deref()),
            ("font_family_italic", cfg.font_family_italic.as_deref()),
            (
                "font_family_bold_italic",
                cfg.font_family_bold_italic.as_deref(),
            ),
        ];
        let styled_paths = std::array::from_fn(|slot| {
            let (key, fam) = styled[slot];
            resolve(key, fam, aterm_render::FaceTarget::for_slot(slot))
        });
        // The coverage faces are files: the renderer reads them itself.
        let regular = aterm_render::FaceTarget::REGULAR;
        let fallback_fonts: Vec<String> = cfg
            .fallback_fonts
            .as_ref()
            .map(|l| l.0.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|f| resolve("fallback_fonts", Some(f), regular).map(|face| face.path))
            .collect();
        let symbol_font =
            resolve("symbol_font", cfg.symbol_font.as_deref(), regular).map(|face| face.path);
        let emoji_font =
            resolve("emoji_font", cfg.emoji_font.as_deref(), regular).map(|face| face.path);
        (
            Self {
                styled_paths,
                synthetic_style: cfg.font_synthetic_style.unwrap_or(true),
                fallback_fonts,
                symbol_font,
                emoji_font,
                display_faces: crate::effective_display_faces(cfg),
            },
            warns,
        )
    }

    /// Keep the last working face for an authored entry that failed admission in
    /// `from_config`. For an authored key `None` means rejected, not unset, so a
    /// bad edit must not clobber a working face; valid siblings still advance.
    #[cfg(test)]
    pub(crate) fn preserve_rejected_from(&mut self, cfg: &Config, previous: &Self) {
        fn preserve<T: Clone>(authored: Option<&str>, next: &mut Option<T>, old: &Option<T>) {
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

/// Fail-closed acceptance for one AppKit titlebar-band sample: fullscreen forces
/// 0; a decorated windowed sample `<= 0` or above
/// [`TITLEBAR_BAND_SANITY_CAP_PTS`] is a transition artifact and keeps the
/// previous band (committing it would misplace row 0); an undecorated windowed 0
/// is real.
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
            // The exit-fullscreen race reads 0 while decorated: a transition
            // artifact, so the previous band is kept.
            ("windowed 0", 0.0, false, true, 55.0, 55.0),
            ("windowed -3", -3.0, false, true, 55.0, 55.0),
            // The enter-fullscreen race reads the screen-vs-window frame
            // difference as a giant band: kept at prev.
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
        // Windowed, decorated, sane 55 pt band: commits and becomes memory.
        let (applied, memory) = titlebar_band_decision(55.0, false, true, 0.0);
        assert_eq!((applied, memory), (55.0, 55.0));

        // Enter fullscreen: the transition reads an inflated artifact. Head is
        // forced to 0; the memory is kept.
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

        assert_eq!(
            titlebar_band_decision(0.0, false, false, 55.0),
            (0.0, 55.0),
            "undecorated applies 0 and keeps the memory slot"
        );
    }
}

/// Once-per-process warning that `background_opacity < 1.0` renders solid on
/// the CPU backend (the CPU presenters' surfaces are opaque). The raised contrast floor
/// still applies.
fn warn_background_opacity_unimplemented_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::logging::stderr_line!(
            "aterm-gui: background_opacity < 1.0 requests translucent glass, but the \
             CPU renderer has no translucent present path; the window \
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
             (NSVisualEffectView), but the CPU renderer cannot composite \
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
    /// exists (and clear it), else paste the clipboard; with tracking off only.
    CopyPaste,
    /// Leave the right button to the seam: reported to a tracking app, inert
    /// otherwise.
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
/// under a kitty enhancement, while Shift+F10 is terminfo `kf22` (`ESC[21;2~`),
/// which programs receive and bind.
///
/// Gated to the Windows/Linux chord arms and tests; the config key parses
/// everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(not(target_os = "macos"), test))]
pub(crate) enum TabMenuChord {
    /// Both Windows spellings: the dedicated Menu/Application key and Shift+F10.
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
    /// (23 px at Windows defaults, 96 dpi: one 9x23 cell row). The default off
    /// Windows and Linux.
    Compact,
    /// Size the whole band to [`TAB_BAND_STANDARD_LOGICAL_PX`] logical px (a real
    /// WinUI tab) by reserving the difference as a synthetic chrome head.
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
/// feed). `Config` equality compares only path strings, so reload dedupe also
/// compares these; otherwise a feed-only edit (the watcher re-posts an
/// identical `aterm.toml` observation, `config_watcher.rs`) would be ignored.
/// A disabled Sparkle table does no path I/O. A changed deco feed triggers a
/// per-window word-engine `hard_reset()`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct PathFeedFps {
    /// Lexicon + toy-pack content: feeds the compiled lexicon the per-window
    /// word decorations scan against.
    pub(crate) deco: u64,
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

/// The file name extensions of the images the wallpaper decoder reads on
/// every platform ([`decode_wallpaper_portable`]): PNG and JPEG. A picker
/// that offers images derives its list from this one table and the platform
/// decoder's own ([`wallpaper_image_filter`]), so it can never offer less (or
/// more) than the decoder reads. (macOS offers what AppKit reads, a superset,
/// and Linux has no picker: only Windows asks.)
#[cfg(any(windows, test))]
pub(crate) fn wallpaper_portable_extensions() -> Vec<&'static str> {
    std::iter::once("png")
        .chain(aterm_jpeg::EXTENSIONS.iter().copied())
        .collect()
}

/// The wallpaper picker's image row: its name and its `;`-separated patterns
/// (`*.png;*.jpg;…`, the spelling the Windows file dialog takes), for what
/// the first-party decoders read ([`wallpaper_portable_extensions`]) and
/// then what the platform's decoder adds (`platform`, on Windows the imaging
/// component's own list), each extension once whatever its case.
#[cfg(any(windows, test))]
pub(crate) fn wallpaper_image_filter(platform: &[String]) -> (String, String) {
    let mut extensions: Vec<String> = wallpaper_portable_extensions()
        .into_iter()
        .map(str::to_string)
        .collect();
    for extension in platform {
        let extension = extension.to_ascii_lowercase();
        if !extension.is_empty() && !extensions.contains(&extension) {
            extensions.push(extension);
        }
    }
    let patterns = extensions
        .iter()
        .map(|extension| format!("*.{extension}"))
        .collect::<Vec<_>>()
        .join(";");
    ("Images".to_string(), patterns)
}

/// Decode wallpaper bytes with the first-party decoders every platform has:
/// PNG through the shared hardened decoder, JPEG (baseline and progressive)
/// through `aterm-jpeg`. Straight sRGB RGBA8, upright.
pub(crate) fn decode_wallpaper_portable(bytes: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    let bound = MAX_WALLPAPER_SOURCE_DIMENSION as u32;
    aterm_png::decode_rgba8(bytes, bound).or_else(|| aterm_jpeg::decode_rgba8(bytes, bound))
}

/// Decode wallpaper bytes to straight sRGB RGBA8. PNG goes through the
/// shared hardened decoder everywhere. On macOS every other format goes
/// through `NSBitmapImageRep`, colour-managed into sRGB and re-encoded as PNG
/// so Rust reads a standard straight RGBA layout, with the first-party JPEG
/// decoder behind it. Elsewhere JPEG goes through the first-party decoder
/// ([`decode_wallpaper_portable`], which turns a photo upright by its EXIF
/// orientation), and on Windows every other format through the Windows
/// Imaging Component ([`crate::image_decode_win`]: BMP, GIF, TIFF, and HEIC,
/// WebP or AVIF where their codecs are installed), colour-managed into sRGB.
fn decode_wallpaper_bytes(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    if let Some(decoded) = aterm_png::decode_rgba8(bytes, MAX_WALLPAPER_SOURCE_DIMENSION as u32) {
        return Ok(decoded);
    }
    #[cfg(target_os = "macos")]
    {
        decode_wallpaper_appkit(bytes)
            .or_else(|appkit| decode_wallpaper_portable(bytes).ok_or(appkit))
    }
    #[cfg(windows)]
    {
        decode_wallpaper_portable(bytes).map_or_else(
            || crate::image_decode_win::decode_rgba8(bytes, MAX_WALLPAPER_SOURCE_DIMENSION),
            Ok,
        )
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        decode_wallpaper_portable(bytes).ok_or_else(|| {
            format!(
                "not a decodable PNG or JPEG (up to {MAX_WALLPAPER_SOURCE_DIMENSION} px per side)"
            )
        })
    }
}

#[cfg(target_os = "macos")]
fn decode_wallpaper_appkit(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    use std::ffi::c_void;

    use aterm_objc::{Id, Sel, autoreleasepool, class, sel};

    use crate::appkit;

    /// `NSBitmapImageRep.h:32-38`: `NSBitmapImageFileTypePNG` is the fifth
    /// enumerator of an unvalued `NS_ENUM(NSUInteger, …)`, i.e. 4.
    const NS_BITMAP_IMAGE_FILE_TYPE_PNG: usize = 4;
    /// `NSGraphics.h:124-130`: `NSColorRenderingIntentPerceptual` is the fourth
    /// enumerator of an unvalued `NS_ENUM(NSInteger, …)`, i.e. 3.
    const NS_COLOR_RENDERING_INTENT_PERCEPTUAL: isize = 3;

    autoreleasepool(|_| {
        // SAFETY: raster work on immutable bytes; no class here is
        // main-thread-bound. `+dataWithBytes:length:` copies,
        // `+imageRepWithData:` returns an autoreleased rep alive for this pool,
        // and `-pixelsWide`/`-pixelsHigh` return `NSInteger`.
        unsafe {
            let data_with: unsafe extern "C-unwind" fn(Id, Sel, *const c_void, usize) -> Id =
                aterm_objc::checked_msg();
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
            let convert: unsafe extern "C-unwind" fn(Id, Sel, Id, isize) -> Id =
                aterm_objc::checked_msg();
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
                aterm_objc::checked_msg();
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
                    aterm_objc::checked_msg();
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

/// The wallpaper decoder reads JPEG on every platform, not PNG alone off
/// macOS, and the picker offers what it reads.
#[cfg(test)]
mod portable_wallpaper_tests {
    use super::{decode_wallpaper_bytes, decode_wallpaper_portable, wallpaper_image_filter};

    /// A 33x21 baseline 4:2:0 JPEG of four coloured quadrants (aterm-jpeg's
    /// fixture, written by libjpeg-turbo's `cjpeg`).
    const QUADRANTS: &[u8] = include_bytes!("../../aterm-jpeg/tests/fixtures/b420.jpg");
    /// The same picture, progressive.
    const PROGRESSIVE: &[u8] = include_bytes!("../../aterm-jpeg/tests/fixtures/p420.jpg");

    fn dominant(rgba: &[u8], width: usize, x: usize, y: usize) -> char {
        let i = (y * width + x) * 4;
        let (r, g, b) = (rgba[i], rgba[i + 1], rgba[i + 2]);
        if r > g && r > b {
            'r'
        } else if g > r && g > b {
            'g'
        } else {
            'b'
        }
    }

    /// Before the first-party JPEG decoder, every platform but macOS refused
    /// this file as "not a decodable PNG". The portable path (what Windows and
    /// Linux run) and the platform's own path both read it now.
    #[test]
    fn wallpaper_decodes_a_jpeg_on_every_platform() {
        for bytes in [QUADRANTS, PROGRESSIVE] {
            for (path, decoded) in [
                (
                    "portable",
                    decode_wallpaper_portable(bytes).ok_or("refused"),
                ),
                (
                    "platform",
                    decode_wallpaper_bytes(bytes).map_err(|_| "refused"),
                ),
            ] {
                let (rgba, width, height) = decoded.unwrap_or_else(|e| panic!("{path}: {e}"));
                assert_eq!((width, height), (33, 21), "{path}");
                assert_eq!(rgba.len(), 33 * 21 * 4, "{path}");
                assert_eq!(dominant(&rgba, width, 4, 4), 'r', "{path}: top left");
                assert_eq!(dominant(&rgba, width, 28, 4), 'g', "{path}: top right");
                assert_eq!(dominant(&rgba, width, 4, 17), 'b', "{path}: bottom left");
            }
        }
        // A file that is neither is still refused, by both.
        assert!(decode_wallpaper_portable(b"GIF89a....").is_none());
        assert!(decode_wallpaper_bytes(b"not an image").is_err());
    }

    /// The picker's row is built from the decoders' own tables: the
    /// first-party ones', then what the platform decoder adds, each extension
    /// once whatever its case. Negative control: with no platform decoder the
    /// row is PNG and JPEG alone.
    #[test]
    fn the_image_picker_row_lists_what_the_decoder_reads() {
        let (name, patterns) = wallpaper_image_filter(&[]);
        assert_eq!(name, "Images");
        assert_eq!(patterns, "*.png;*.jpg;*.jpeg;*.jpe;*.jfif");
        let platform = ["bmp", "JPG", "heic", "bmp"].map(str::to_string);
        let (_, patterns) = wallpaper_image_filter(&platform);
        assert_eq!(patterns, "*.png;*.jpg;*.jpeg;*.jpe;*.jfif;*.bmp;*.heic");
    }
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
        // Row-major, top-down, straight RGBA: BMP's own order inverted, so a
        // decoder that forgot the flip fails here.
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
pub(crate) fn toml_basic_string(w: &str) -> String {
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

impl Config {
    /// The engine config's non-colour keys (cursor shape, scrollback, BiDi,
    /// width, faint, security opt-ins) over the engine defaults. Colours come
    /// from [`Self::resolved_colors`] in [`Self::applied_terminal_config_from`].
    pub(crate) fn engine_behaviour_config(&self) -> aterm_core::config::TerminalConfig {
        let mut tc = aterm_core::config::TerminalConfig::default();
        if let Some(n) = self.scrollback_lines {
            // 0 → unlimited (None); N → cap at N lines.
            tc.scrollback_limit = (n != 0).then_some(n);
        }
        if self.cursor_style.is_some() || self.cursor_blink.is_some() {
            let blink = self.cursor_blink.unwrap_or(true);
            let cursor_style = self.cursor_style.as_deref().unwrap_or("block").trim();
            // Underline is not a config option; programs may still request it
            // via DECSCUSR.
            tc.cursor_style = if cursor_style.eq_ignore_ascii_case("block") {
                if blink {
                    CursorStyle::BlinkingBlock
                } else {
                    CursorStyle::SteadyBlock
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
        }
        // Faint opacity, resolved by the engine's color resolution into
        // RenderCell; bold-to-bright rides with the colours.
        if self.faint_opacity.is_some() {
            tc.faint_opacity = self.faint_opacity_or_default();
        }
        // The theme-pair contrast floor, resolved the same way.
        tc.indexed_minimum_contrast = self.indexed_minimum_contrast_or_default();
        // Security opt-ins (fail-closed by default in TerminalConfig). Only a
        // present key changes the flag, so omitting them keeps the safe default.
        if let Some(v) = self.allow_osc52_query {
            tc.allow_osc52_query = v;
        }
        if let Some(v) = self.allow_window_ops {
            tc.allow_window_ops = v;
        }
        if let Some(v) = self.allow_notifications {
            tc.allow_notifications = v;
        }
        // The palette is a default-on permission, not an opt-in: a program's
        // indexed colours are session-scoped, never saved and never reach
        // chrome, and truecolour SGR plus OSC 10/11 already give a program the
        // same power over what a cell looks like. Only an explicit `false`
        // closes the engine's (fail-closed) gate.
        tc.allow_palette_reconfigure = self.allow_palette_reconfigure_or_default();
        tc
    }

    /// [`Self::applied_terminal_config_for_with_assets`] at the Dark appearance
    /// with no custom themes.
    #[cfg(test)]
    pub(crate) fn applied_terminal_config(&self) -> aterm_core::config::TerminalConfig {
        self.applied_terminal_config_for(aterm_types::Appearance::Dark)
    }

    /// [`Self::applied_terminal_config`] for a specific OS `appearance`.
    #[cfg(test)]
    pub(crate) fn applied_terminal_config_for(
        &self,
        appearance: aterm_types::Appearance,
    ) -> aterm_core::config::TerminalConfig {
        self.applied_terminal_config_for_with_assets(appearance, &ThemeCatalog::default())
    }

    /// The engine `TerminalConfig` for an admitted theme catalog:
    /// [`Self::resolved_colors`] through [`Self::applied_terminal_config_from`].
    /// The App instead installs one resolution through `App::install_colors`,
    /// which projects it once.
    #[cfg(test)]
    pub(crate) fn applied_terminal_config_for_with_assets(
        &self,
        appearance: aterm_types::Appearance,
        themes: &ThemeCatalog,
    ) -> aterm_core::config::TerminalConfig {
        self.applied_terminal_config_from(&self.resolved_colors(appearance, themes, None))
    }
}

/// Resolve the config file path without creating anything, via
/// `aterm_types::dirs::aterm_config_path` (`$XDG_CONFIG_HOME`, then `%APPDATA%`
/// on Windows, then `$HOME/.config`), the resolver the CLI's `explain-config`
/// and `doctor` also use.
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
        // No `update.*` or `packages.*` key has a launch override.
        _ => None,
    }
}

/// Load the user config. A missing file is fine (defaults); a malformed file is
/// reported and only its `[harness]` lines are read (`Config::stand_in`).
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
    Config::parse(&observation.text).unwrap_or_else(|e| {
        let notice = launch_config_notice(&path, LaunchConfigProblem::Invalid(&e));
        crate::logging::stderr_line!("aterm-gui: {notice}");
        note_launch_load_failed(notice);
        Config::stand_in(&observation.text)
    })
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

/// Why this process launched without its `aterm.toml`: the sentence the console
/// and the window banner carry, for stdout-only reports.
pub(crate) fn launch_load_failure() -> Option<&'static str> {
    LAUNCH_LOAD_FAILURE.get().map(String::as_str)
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

/// Read source and application policy from one persisted config observation.
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

/// Resolve the font size in px at 1× with the canonical precedence
/// `--font-px > config.font_px > FONT_PX default`. Only finite values inside
/// `FONT_PX_MIN..=FONT_PX_MAX` are admitted; an invalid source falls through
/// instead of being clamped. Shared by startup and config reload, so the launch
/// flag still wins after a config edit.
pub(crate) fn resolve_font_px(config: &Config) -> f32 {
    resolve_font_px_with(launch::flags().font_px, config.font_px)
}

/// [`resolve_font_px`] times the text size scale of `view` (the config with
/// any theme typography filled in): `font_size_scale` multiplies the config's
/// size or the default, never a `--font-px` given for this launch.
pub(crate) fn resolve_scaled_font_px(view: &Config) -> f32 {
    let base = resolve_font_px(view);
    if launch::flags()
        .font_px
        .is_some_and(|px| font_px_in_range(&px))
    {
        base
    } else {
        crate::scaled_font_px(base, view.font_size_scale_milli_or_default())
    }
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

/// Banner line for a `font_size_scale` outside 0.8..=1.25 or not finite
/// (clamped, or `1.0` when not finite), `None` when unset or in range.
pub(crate) fn font_size_scale_load_notice(config: &Config) -> Option<String> {
    let scale = config
        .font_size_scale
        .filter(|v| !(v.is_finite() && (0.8..=1.25).contains(v)))?;
    let used = f32::from(config.font_size_scale_milli_or_default()) / 1000.0;
    Some(format!(
        "config font_size_scale: {scale:?} is not accepted (expected 0.8–1.25; using {used})"
    ))
}

/// The size notices every config load restates: [`font_px_load_notice`] and
/// [`font_size_scale_load_notice`].
pub(crate) fn font_size_load_notices(config: &Config) -> Vec<String> {
    font_px_load_notice(config)
        .into_iter()
        .chain(font_size_scale_load_notice(config))
        .collect()
}

/// Everything about the configured `theme` the window cannot draw as written:
/// the selector's grammar issues ([`theme_selector_notice`]) first, then
/// [`theme_catalog_misses`]. The band carries them as [`theme_load_notices`],
/// so a typo in a split side is said live like every other unavailable theme.
pub(crate) fn theme_misses(config: &Config, themes: &ThemeCatalog) -> Vec<String> {
    theme_selector_notice(config.theme.as_deref())
        .into_iter()
        .chain(theme_catalog_misses(config, themes))
        .collect()
}

/// Each configured `theme` name the window cannot use against `themes`, and that it
/// draws Default instead: one sentence per name, so a plain name missing from both
/// appearances is said once. `--validate-config` and Manual print these words
/// (their selector issues come from the config-language checks instead).
pub(crate) fn theme_catalog_misses(config: &Config, themes: &ThemeCatalog) -> Vec<String> {
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

/// Whether a valid flag/config size names the font size (rather than the
/// built-in default). It changes no scaling — a named size follows each
/// window's scale like the default — and only words `--show-config`.
/// Admission and explicitness deliberately share the same predicate, so an
/// invalid `font_px` is not "named".
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

/// Effective shell for each new session: `--shell` outranks the config `shell`.
/// Shared by startup and config reload, so a saved `shell` edit reaches the next
/// tab while the launch flag keeps precedence.
pub(crate) fn resolve_shell_override(config: &Config) -> Option<String> {
    resolve_shell_override_with(crate::cli::launch_flags().shell, config.shell.as_deref())
}

fn resolve_shell_override_with(flag: Option<String>, configured: Option<&str>) -> Option<String> {
    flag.filter(|value| !value.is_empty())
        .or_else(|| configured.map(str::to_string))
}

/// Pure precedence core for [`resolve_want_gpu`], with the two flags and the
/// config value passed in.
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

/// Notices for changed restart-only keys, which config reload does not
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
/// `warns` in two families, each sentence prefixed `config …`: keys this build
/// ignores (`ConfigFamily::IgnoredKeys`, one row, a detail line per key), and
/// retired spellings still read by a newer key (`ConfigFamily::RetiredKeys`, a
/// record rather than a row, so a working legacy spelling is not shown as
/// unknown). Unusable values of known keys are [`unaccepted_value_notices`]'s.
/// Windowed launches have no stderr, so this is how a misspelled key surfaces.
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

/// The `ConfigFamily::UnacceptedValues` family, assembled ONE way at launch and
/// at every reload: the `[harness]` values the supervisor refused, a `font_px` or
/// `font_size_scale` the resolver ignores or clamps and a theme the window draws
/// Default in place of — which Settings also says on their own rows (Font size's
/// "Configured 500 → Default", Size scale's "Configured 2 → Effective 1.25", Color
/// theme's "Theme unavailable"), so they are marked shown there — then every other
/// refused value, skipping a key a resolver above already named.
pub(crate) fn collect_unaccepted_value_notices(
    warns: &mut crate::message_reporters::ConfigWarnings,
    config: &Config,
    themes: &ThemeCatalog,
    text: &str,
) {
    use crate::message_reporters::ConfigFamily;
    collect_harness_notices(warns, config);
    warns.extend_shown_in_settings(
        ConfigFamily::UnacceptedValues,
        font_size_load_notices(config),
    );
    warns.extend_shown_in_settings(
        ConfigFamily::UnacceptedValues,
        theme_load_notices(config, themes),
    );
    let unaccepted = unaccepted_value_notices(text, &warns.told());
    warns.extend(ConfigFamily::UnacceptedValues, unaccepted);
}

/// What a reload saw happen to each config problem it may find fixed, as the
/// words that problem's ✓ record takes, by `config.` key
/// ([`crate::message_reporters::config_fix_words`]): an image source that was
/// refused and now is not was taken out ("Wallpaper removed") or now loads
/// ("Wallpaper loaded"); a font or cursor-trail request that was taken out reads
/// "… setting removed", one that is still set "… applied". Consulted only for a
/// family the reload no longer finds.
pub(crate) fn config_fix_observations(
    old: &Config,
    old_assets: &ConfigAssetCatalog,
    new: &Config,
    new_assets: &ConfigAssetCatalog,
) -> std::collections::BTreeMap<String, String> {
    use crate::message_reporters::{ConfigFamily, ConfigFix, KEY_FONT_FAMILY, config_fix_words};
    let ended = |was_refused: bool, refused: bool, source: Option<&str>| {
        (was_refused && !refused).then_some(if source.is_none() {
            ConfigFix::Removed
        } else {
            ConfigFix::Applied
        })
    };
    let mut out = std::collections::BTreeMap::new();
    let images: Vec<(&str, ConfigFix)> = [
        (
            "Wallpaper",
            ended(
                old_assets.wallpaper.diagnostic().is_some(),
                new_assets.wallpaper.diagnostic().is_some(),
                new_assets.wallpaper.source_id(),
            ),
        ),
        (
            "Cursor sprite",
            ended(
                old_assets.kitty_sprite.diagnostic().is_some(),
                new_assets.kitty_sprite.diagnostic().is_some(),
                new_assets.kitty_sprite.source_id(),
            ),
        ),
    ]
    .into_iter()
    .filter_map(|(subject, how)| how.map(|how| (subject, how)))
    .collect();
    if let Some(words) = config_fix_words(ConfigFamily::Assets, &images) {
        out.insert(ConfigFamily::Assets.key().to_string(), words);
    }
    // A font request taken out, with nothing new asked for in its place.
    let requests = |c: &Config| {
        [
            c.font_family.clone(),
            c.font_family_bold.clone(),
            c.font_family_italic.clone(),
            c.font_family_bold_italic.clone(),
            c.fallback_fonts.as_ref().map(|list| list.0.join(", ")),
        ]
    };
    let (before, after) = (requests(old), requests(new));
    let fonts_removed = before
        .iter()
        .zip(&after)
        .any(|(b, a)| b.is_some() && a.is_none())
        && before
            .iter()
            .zip(&after)
            .all(|(b, a)| a.is_none() || a == b);
    let how = |removed: bool| {
        if removed {
            ConfigFix::Removed
        } else {
            ConfigFix::Applied
        }
    };
    if let Some(words) = config_fix_words(ConfigFamily::Fonts, &[("Font", how(fonts_removed))]) {
        out.insert(ConfigFamily::Fonts.key().to_string(), words);
    }
    let family_removed = old.font_family.is_some() && new.font_family.is_none();
    if let Some(words) = config_fix_words(ConfigFamily::Fonts, &[("Font", how(family_removed))]) {
        out.insert(KEY_FONT_FAMILY.to_string(), words);
    }
    let trail_removed = old.cursor_trail_style.is_some() && new.cursor_trail_style.is_none();
    if let Some(words) = config_fix_words(
        ConfigFamily::CursorTrail,
        &[("Cursor trail", how(trail_removed))],
    ) {
        out.insert(ConfigFamily::CursorTrail.key().to_string(), words);
    }
    out
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
    admitted_font_px(flag, config).unwrap_or(FONT_PX)
}

fn font_px_in_range(value: &f32) -> bool {
    value.is_finite() && *value >= FONT_PX_MIN && *value <= FONT_PX_MAX
}

fn admitted_font_px(flag: Option<f32>, config: Option<f32>) -> Option<f32> {
    flag.filter(font_px_in_range)
        .or(config.filter(font_px_in_range))
}

/// Named-size counterpart of [`resolve_font_px_with`], on the same admission
/// helper so an invalid value never counts as named.
pub(crate) fn font_px_is_explicit_with(flag: Option<f32>, config: Option<f32>) -> bool {
    admitted_font_px(flag, config).is_some()
}

/// A font size or device px as the window carry writes it: thousandths,
/// `None` outside `FONT_PX_MIN..=FONT_PX_MAX`.
#[cfg(unix)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "admitted to FONT_PX_MIN..=FONT_PX_MAX first, so the product is a positive \
              integer of at most 200 000"
)]
pub(crate) fn font_px_milli(px: f32) -> Option<u32> {
    font_px_in_range(&px).then(|| (px * 1000.0).round() as u32)
}

/// The launch font of a process that adopted a seamless update's sessions,
/// before its first window is sized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LaunchFont {
    /// The font size to draw (px at 1×, `App::font_size`).
    pub(crate) size: f32,
    /// What Cmd-0 resets to (px at 1×, `App::default_font_size`).
    pub(crate) reset: f32,
    /// The pair came from an outgoing build that carried only device px
    /// ([`CarriedZoom::device`]): `size` and `reset` are that build's device
    /// px, which the first window's scale turns into sizes
    /// (`App::font_size_from_device`).
    pub(crate) from_device: bool,
}

impl LaunchFont {
    /// The device px a backend is built at before any window exists: a
    /// headless launch's sizes at its render scale (`--scale`, else 1×); a
    /// window's attach selects its own scale's px from the unscaled build, so
    /// the build keeps the size. A device-px carry is already device px.
    pub(crate) fn build_px(&self, headless_scale: Option<f64>) -> f32 {
        match headless_scale {
            Some(scale) if !self.from_device => crate::font_px_for_scale(self.size, scale),
            _ => self.size,
        }
    }
}

/// A carried font zoom, in thousandths: the pair this build writes, in px at
/// 1× (`size`), and the device-px pair an older build wrote (`device`), each
/// `(px, reset px)`, both halves or neither.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CarriedZoom {
    pub(crate) size: Option<(u32, u32)>,
    pub(crate) device: Option<(u32, u32)>,
}

impl CarriedZoom {
    /// The zoom an authenticated window carry holds.
    pub(crate) fn of_carry(carry: &crate::session_store::WindowCarry) -> Self {
        Self {
            size: carry.font_size_milli.zip(carry.font_reset_size_milli),
            device: carry.font_px_milli.zip(carry.font_reset_px_milli),
        }
    }
}

/// The carried zoom, or the config's font. `resolved` is this launch's size
/// (px at 1×, from its flags and config); `carry` is the outgoing window's
/// frame, `Some` only on an authenticated handoff boot.
///
/// A carry holding a zoom pair wins: the successor draws at the zoomed size,
/// and Cmd-0 returns to the outgoing reset size. It outranks the flags, since
/// a successor inherits its parent's launch argv. The px-at-1× pair
/// (`WindowCarry::font_size_milli`) is read first; a carry from an older build
/// holds only device px (`font_px_milli`), which is taken as such
/// ([`LaunchFont::from_device`]). A pair with either half missing or out of
/// bounds is ignored whole (no clamping).
pub(crate) fn successor_font_px(
    resolved: f32,
    carry: Option<&crate::session_store::WindowCarry>,
) -> LaunchFont {
    launch_font_for_zoom(
        resolved,
        carry.map(CarriedZoom::of_carry).unwrap_or_default(),
    )
}

/// [`successor_font_px`] over the zoom alone, for the warm prologue that runs
/// before the dial (`WarmOrder::BeforeDial`) using the launch hint
/// (`handoff_warm_hint`). Same admission, so a matching hint builds at the size
/// the carry then selects.
#[allow(
    clippy::cast_precision_loss,
    reason = "a carried size is a u32 of thousandths; any value past f32's exact range is far \
              outside the admitted bounds and is refused by them"
)]
pub(crate) fn launch_font_for_zoom(resolved: f32, zoom: CarriedZoom) -> LaunchFont {
    let admitted = |(px, reset): (u32, u32)| {
        let (px, reset) = (px as f32 / 1000.0, reset as f32 / 1000.0);
        (font_px_in_range(&px) && font_px_in_range(&reset)).then_some((px, reset))
    };
    if let Some((size, reset)) = zoom.size.and_then(admitted) {
        LaunchFont {
            size,
            reset,
            from_device: false,
        }
    } else if let Some((size, reset)) = zoom.device.and_then(admitted) {
        LaunchFont {
            size,
            reset,
            from_device: true,
        }
    } else {
        LaunchFont {
            size: resolved,
            reset: resolved,
            from_device: false,
        }
    }
}

/// Resolve one raw style. Builtins and aliases come from `prefs`; this adds
/// the fail-closed classification.
pub(crate) fn resolve_trail_style(raw: &str) -> ResolvedTrailStyle {
    let raw = raw.trim();
    let Some(canonical) = crate::prefs::cursor_trail_style_canonical(raw) else {
        // An unrecognized spelling falls back to the default style and keeps
        // `unknown` for the warning and Settings. `style: None` is reserved for
        // `off`, since `resolve_cursor_glow` gates the whole effect on it.
        return ResolvedTrailStyle {
            canonical: Some(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE),
            style: Some(aterm_effects::cursor_glow::GlowStyle::parse(
                crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
            )),
            unknown: true,
        };
    };
    if canonical == "off" {
        return ResolvedTrailStyle::off();
    }
    ResolvedTrailStyle {
        canonical: Some(canonical),
        style: Some(aterm_effects::cursor_glow::GlowStyle::parse(canonical)),
        unknown: false,
    }
}

/// Resolve every string-keyed trail presentation fork in one pass, per config
/// generation; the `Copy` result is cached in [`TrailStyleCache`].
pub(crate) fn resolve_trail_presentation(raw: &str) -> ResolvedTrailPresentation {
    resolve_trail_presentation_from_style(raw, resolve_trail_style(raw))
}

/// Presentation half for preview surfaces that already hold a synthetic
/// resolution.
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
        // The old `classic mono` spellings are aliases of Classic, read from
        // what was authored: they draw its one-colour face in the cursor's
        // colour, as they always did.
        classic_mono: style.style == Some(GlowStyle::Classic)
            && GlowStyle::style_names_classic_mono(raw.trim()),
        pet_species,
        comet: style.style == Some(GlowStyle::Comet),
    }
}

/// The spelling the raw-token predicates must read: what the trail draws, not
/// what was authored. The companion, species, comet and ribbon forks are string
/// predicates, so a typo must be replaced by the spelling
/// [`resolve_trail_style`] settled on. Aliases collapse onto their canonical in
/// one step (`aterm_effects::cursor_glow::spellings`; `prefs`'
/// `no_spelling_that_loaded_before_changes_what_it_draws` pins that the
/// collapse changes no look).
#[cfg(test)]
pub(crate) fn effective_trail_style_token(raw: &str) -> &str {
    let raw = raw.trim();
    crate::prefs::cursor_trail_style_canonical(raw)
        .unwrap_or(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE)
}

/// Cold-path construction of the engine's `GlowConfig` through
/// [`aterm_effects::cursor_fx::resolve_glow_config`] (shared with the web
/// pipeline). The live terminal and Settings preview differ only in injected
/// geometry/theme facts (`dark_theme`, `head_dx`) and in a pinned cursor
/// colour, which the live frame folds through
/// `aterm_effects::cursor_fx::automatic_glow_color`.
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
    // style and renders; only `off` resolves to no style.
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
            beam: presentation.beam,
            // The tall body is the default; only `... underline` selects the
            // under-baseline mark.
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

/// Lexicon conflicts the sparkle runtime preparation logs for the resolved scan
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
    default_font_size: f32,
    font_size: f32,
    font_px: f32,
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
    typography: crate::resolved_typography::ResolvedTypography,
    font_size_scale_milli: u16,
    window_metrics: Vec<(WindowId, crate::MetricsView)>,
}

impl ReloadRenderSnapshot {
    fn capture(app: &App) -> Self {
        Self {
            config: app.config.clone(),
            default_font_size: app.default_font_size,
            font_size: app.font_size,
            font_px: app.font_px,
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
            typography: app.typography.clone(),
            font_size_scale_milli: app.font_size_scale_milli,
            window_metrics: app
                .windows
                .iter()
                .map(|(wid, ws)| (*wid, ws.metrics))
                .collect(),
        }
    }

    fn rollback(self, app: &mut App) {
        restore_render_config_fields(&mut app.config, &self.config);
        app.default_font_size = self.default_font_size;
        app.font_size = self.font_size;
        app.font_px = self.font_px;
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
        app.typography = self.typography;
        app.font_size_scale_milli = self.font_size_scale_milli;
        for (wid, metrics) in self.window_metrics {
            if let Some(ws) = app.windows.get_mut(&wid) {
                ws.metrics = metrics;
            }
        }

        // Theme colours feed both the engine defaults and the renderer; install
        // the hybrid config's colours so both stay consistent.
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::Rollback);
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
        palette_overrides,
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
        font_size_scale,
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
    /// Activate [`App::sparkle`] from the prepared generation under the Serious
    /// Mode policy. Runs only when `sparkle_dirty` is set, never per frame.
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

    /// Resolve the sparkle runtime if still dirty. `self.sparkle` is a
    /// render-path cache filled by the first frame, which a headless instance
    /// may never draw; gates on the cat's existence use this, never the field.
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
        let cfg = self.config.matrix_rain_params(bg, fg, self.grid_accent());
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
        // can force rain on before any config enabled it).
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
    /// edge, or (`on=done`) by its next honoured OSC 7501 `done`
    /// ([`Self::note_program_done`]). `Ok` carries
    /// `armed=<none|green|enter|done> sig=<hex> bars=<n>
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

    /// The `done` edge of `fx celebrate on=done`: the status sweep honoured an
    /// OSC 7501 `done` from the program in front of `session`
    /// (`App::observe_session_statuses`, the program-state gate). Each window's
    /// detector is offered it; only one holding an `on=done` arm bound to
    /// `session` fires (`KittySing::note_program_done`), and that window is
    /// woken to draw it. The arm is spent, so a second `done` fires nothing.
    pub(crate) fn note_program_done(&mut self, session: u64, now: std::time::Instant) {
        for ws in self.windows.values_mut() {
            if ws
                .fx
                .companions_mut()
                .kitty_sing
                .note_program_done(now, session)
                && let Some(w) = ws.os_window.as_ref()
            {
                w.request_redraw();
            }
        }
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
    /// `pending_resize`, applied at most every `RESIZE_THROTTLE` (50 ms) and once
    /// more at the trailing settle. A height-only resize rewraps nothing and
    /// applies immediately. A coalesced size never reaches `on_resize`, so the
    /// engine and the recorder only see real sizes in order. The control-socket
    /// `resize` verb stays immediate.
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
            // Re-arm the settle after a row-only apply: a live-drag apply sizes
            // only the active tab and marks the window `panes_stale`, and the
            // trailing settle pays for background tabs. After `on_resize`, since a
            // committing `apply_term_resize` clears both fields; arming with this
            // size is safe because `flush_pending_resize` re-reads `inner_size()`.
            if !reflows
                && let Some(ws) = self.windows.get_mut(&wid)
                && ws.next_resize_settle.is_none()
            {
                ws.pending_resize = Some(size);
                ws.next_resize_settle = Some(now + crate::RESIZE_THROTTLE);
            }
        } else if let Some(ws) = self.windows.get_mut(&wid) {
            // Inside the throttle window: keep only the latest size and arm the
            // trailing reflow so the final size lands even if the drag stops here.
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
        // That size is in the window's live scale, so the cells it is counted in
        // must be too. The winit fork's Wayland backend gives a window an
        // output's new scale as it reads the compositor's event, but delivers
        // `ScaleFactorChanged` only later in that loop turn, after `NewEvents`,
        // where a due resize settle lands here. A weston output at scale 2
        // re-gridded a new window 80x24 -> 162x50 -> 80x24 that way, its scale-2
        // pixels counted in scale-1 cells. Catch this window's metrics up first;
        // the `ScaleFactorChanged` that follows then changes nothing. A pinned
        // font or `--scale` keeps its metrics, as there.
        if let Some(live) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_ref())
            .map(|w| w.scale_factor())
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| (ws.scale - live).abs() > f64::EPSILON)
        {
            self.on_scale_factor_changed(wid, live);
        }
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
            // The stats Deck under the band, in the form this frame decides.
            let head = self.deck_head_px(wid, head_pts, scale, size.height);
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
        self.sync_synthetic_strip_head(wid, size.height);
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

    /// Windows/Linux synthetic tab-band head of window `wid`, computed from the
    /// cell box, strip rows and configured target. Every input funnels through
    /// [`Self::on_resize`] (font zoom and reload via `rebuild_backend`,
    /// `tab_strip_rows` edits, DPI moves via `Resized`), which runs this before
    /// `grid_dims_for`; a strip-rows change whose window its caller fits
    /// ([`Self::regrid_for_chrome_rows_but`]) runs it there, before the
    /// initial-frame settle sizes the frame from it.
    ///
    /// Attached windows only, like the other head derivations: a never-attached
    /// window keeps its sealed boot band so headless captures stay stable.
    ///
    /// A runtime `cfg!` rather than `#[cfg]`: this is the only non-test caller
    /// of the `synthetic_strip_head_px` chain, which would otherwise be dead
    /// code on macOS. On macOS `PLATFORM_DEFAULT` is `Compact` and the branch
    /// is compiled out, so the measured AppKit band in `ws.metrics.head` is
    /// never overwritten.
    ///
    /// `frame_h_px` is the window's frame height, from which the stats Deck's
    /// form over the band is re-decided (`App::deck_head_px`).
    fn sync_synthetic_strip_head(&mut self, wid: WindowId, frame_h_px: u32) {
        if cfg!(any(windows, target_os = "linux"))
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.os_window.is_some())
        {
            let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale);
            let cell_h = self.win_cell_size(wid).1;
            let strip = self.synthetic_strip_head_px(scale, cell_h);
            // Stored in points beside the applied px so the shared re-derivations
            // reading `head_pts` (`on_scale_factor_changed`,
            // `refresh_all_window_metrics`) can reproduce it. Lossy at a new
            // scale when the font is pinned, but the `Resized` that follows
            // `ScaleFactorChanged` re-derives it here within one frame.
            let strip_pts = strip as f64 / if scale > 0.0 { scale } else { 1.0 };
            // The stats Deck takes the head's top, the strip band under it
            // (`App::deck_rows_px`).
            let head = self.deck_head_px(wid, strip_pts, scale, frame_h_px);
            if let Some(ws) = self.windows.get_mut(&wid)
                && ws.metrics.head != head
            {
                ws.head_pts = strip_pts;
                ws.metrics.head = head;
                self.backend.set_head(head);
            }
        }
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
        self.regrid_for_chrome_rows_inner(None);
    }

    /// The tab strip takes `rows` rows at `now` (a config load or reload): a
    /// change re-splits every window between the strip and the terminal, one
    /// re-grid. A RISE raises every window's chrome rows and is noted on each
    /// floor, so no other source's fall re-grids inside the settle
    /// (`ChromeRegrowFloor`'s `StripRise`). A FALL — a person's Settings edit —
    /// re-grids at once unless a chrome row rose inside the settle: then it waits
    /// the floor, as the band's quiet shrink does (`StripUnwant`, then
    /// [`Self::settle_strip_fall`]'s `StripFall`), so a band row rising just
    /// before the edit never re-grids a window down and back milliseconds apart
    /// (measured on 2026-10-19: 38 ms apart, `aterm-lab presence-flap --window
    /// --phases X`). The next edit replaces a fall still waiting; an edit back to
    /// the rows the strip has takes it back (`StripBack`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ChromeRegrowFloor",
            action = "StripRise",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ChromeRegrowFloor",
            action = "StripUnwant",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ChromeRegrowFloor",
            action = "StripBack",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    pub(crate) fn set_tab_strip_rows(&mut self, rows: u16, now: std::time::Instant) {
        self.strip_fall = None;
        if rows == self.tab_strip_rows {
            return;
        }
        if aterm_messages::chrome::is_rise(self.tab_strip_rows, rows) {
            self.note_chrome_rise_everywhere(now);
        } else if !aterm_messages::chrome::fall_allowed(self.band_regrow_floor(), now) {
            self.strip_fall = Some(rows);
            return;
        }
        self.tab_strip_rows = rows;
        self.regrid_for_chrome_rows();
    }

    /// The tab strip's waiting fall ([`Self::set_tab_strip_rows`]) once the
    /// windows' regrow floor has run at `now`: the rows taken and every window
    /// re-gridded. `true` when the strip moved. Run on every settle of the chrome
    /// rows ([`Self::settle_messages`]), whose deadline names the floor
    /// ([`Self::strip_fall_due`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ChromeRegrowFloor",
            action = "StripFall",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    pub(crate) fn settle_strip_fall(&mut self, now: std::time::Instant) -> bool {
        let Some(rows) = self.strip_fall else {
            return false;
        };
        if !aterm_messages::chrome::fall_allowed(self.band_regrow_floor(), now) {
            return false;
        }
        self.strip_fall = None;
        if rows == self.tab_strip_rows {
            return false;
        }
        self.tab_strip_rows = rows;
        self.regrid_for_chrome_rows();
        true
    }

    /// When the tab strip's waiting fall may commit: the windows' regrow floor,
    /// or `None` with no fall waiting (or no floor left to wait).
    pub(crate) fn strip_fall_due(&self) -> Option<std::time::Instant> {
        self.strip_fall.and(self.band_regrow_floor())
    }

    /// [`Self::regrid_for_chrome_rows`] but for window `settling`, which its caller
    /// fits ([`Self::sync_strip_header_row`]): its strip caches are cleared, its
    /// band's head re-derived, its whole-cell minimum re-pushed and a frame asked
    /// for like every window's, but its grid is not re-fit here. Never batched:
    /// it runs at a `Resized` and at attach, which no
    /// [`Self::with_chrome_regrid_batch`] encloses, and a batch's re-grid at its
    /// end would fit `settling` too.
    #[cfg(any(test, target_os = "linux"))]
    #[track_caller]
    pub(crate) fn regrid_for_chrome_rows_but(&mut self, settling: WindowId) {
        let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Chrome);
        self.regrid_for_chrome_rows_inner(Some(settling));
    }

    /// The body of [`Self::regrid_for_chrome_rows`]; `settling` is left unfitted
    /// ([`Self::regrid_for_chrome_rows_but`]).
    fn regrid_for_chrome_rows_inner(&mut self, settling: Option<WindowId>) {
        #[cfg(test)]
        {
            crate::resize_ledger::note_chrome_regrid_site_for_test();
            let windows: Vec<WindowId> = self
                .windows
                .keys()
                .copied()
                .filter(|wid| Some(*wid) != settling)
                .collect();
            for wid in windows {
                crate::app_presence::note_chrome_regrid_applied(self, wid);
            }
        }
        let sized: Vec<(WindowId, PhysicalSize<u32>)> = self
            .windows
            .iter_mut()
            .filter_map(|(wid, ws)| {
                ws.tab_segments.clear();
                ws.last_strip_fp = None; // chrome geometry changed
                ws.last_present = None;
                ws.os_window.as_ref().map(|w| (*wid, w.inner_size()))
            })
            .collect();
        for (wid, size) in sized {
            if Some(wid) == settling {
                // Its band's head follows the rows now, so the frame its caller's
                // settle asks for has it.
                self.sync_synthetic_strip_head(wid, size.height);
            } else {
                self.on_resize(wid, size);
            }
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                // Linux: the whole-cell min-size lattice includes chrome rows, so
                // re-push it or an edge drag snaps one row off.
                #[cfg(target_os = "linux")]
                w.set_min_inner_size(Some(self.whole_cell_min_size(wid)));
                w.request_redraw();
            }
        }
        // The band moved with the chrome under a pointer that did not: resolve its
        // lit chip again.
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
    // Linux-only like its callers; `test` because this module's tests call it on
    // every host.
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

    /// Whether applying `size` to window `wid` changes its column count (a
    /// scrollback rewrap); the predicate for [`Self::on_resize_throttled`]. Reads
    /// the authoritative `inner_size()` like [`Self::on_resize`]. An unknown
    /// window, or a grid outside `MAX_GRID_ROWS`/`MAX_GRID_COLS` (whose
    /// `ws.cols` never catches up), reports a reflow, keeping the throttle.
    fn resize_changes_columns(&self, wid: WindowId, size: PhysicalSize<u32>) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return true;
        };
        let size = ws.os_window.as_ref().map_or(size, |w| w.inner_size());
        let (_rows, cols) = self.grid_dims_for(wid, size);
        cols != ws.cols
    }

    /// Live font zoom (Cmd-+/Cmd--/Cmd-0): set the font size to `size` (px at
    /// 1×), rebuild the [`Backend`] at the front window's px for it, re-grid each
    /// window and tell the PTY. A failed rebuild keeps the current size. No-op
    /// without a window.
    pub(crate) fn set_font_size(&mut self, size: f32) {
        let _ = self.set_font_size_with(size, Self::rebuild_backend);
    }

    /// Transactional core of [`Self::set_font_size`]. The rebuild is an injected
    /// closure so the rollback path is testable: on failure the App's font and
    /// per-window metrics stay paired with the live renderer.
    pub(crate) fn set_font_size_with(
        &mut self,
        size: f32,
        rebuild: impl FnOnce(&mut Self) -> bool,
    ) -> bool {
        let size = size.clamp(FONT_PX_MIN, FONT_PX_MAX);
        if (size - self.font_size).abs() < 0.01 {
            return true;
        }
        let previous = (self.font_size, self.font_px);
        self.font_size = size;
        self.font_px = crate::font_px_for_scale(size, self.front_scale());
        // Re-resolve per-window metrics to the new size before `rebuild_backend`
        // re-grids, as config reload does; otherwise the re-grid divides by the
        // stale cell box and new glyphs overflow the old layout.
        self.refresh_all_window_metrics();
        if rebuild(self) {
            true
        } else {
            // The old renderer survives a failed rebuild; restore its matching
            // App and window metrics too.
            (self.font_size, self.font_px) = previous;
            self.refresh_all_window_metrics();
            false
        }
    }

    /// The display scale the shared backend's px is derived at outside a
    /// window's own redraw: the render-scale override, else the front window's
    /// scale, else 1×.
    pub(crate) fn front_scale(&self) -> f64 {
        resolve_force_scale()
            .or_else(|| self.front().map(|ws| ws.scale))
            .unwrap_or(1.0)
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
            crate::HOST_CAPTURE.compositor_blends_client,
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
    /// and repaint (font zoom, [`Self::set_font_px`]; config reload runs the same
    /// body with worker-prepared fonts). A failed rebuild keeps the current
    /// backend.
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
            // Resize increments are per-window geometry: one shared renderer cell
            // box cannot be copied to every mixed-DPI window.
            let (cw, ch) = self.win_cell_size(wid);
            // The increment base must track the cell box too (see
            // `whole_cell_min_size`); computed before the `&mut` borrow below.
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
        // (idempotent).
        if self.backend.is_gpu() {
            self.apply_window_vibrancy();
        }
        true
    }

    /// Reset every per-window raster cache after the face or device changed:
    /// both renderers' content caches and the `RepaintKey` gate, a fresh
    /// `WindowGpu` that keeps the surface state (EDR headroom, reference-white
    /// scale, capture colour space), and the introspection scratch. Also used by
    /// `retry_attach_on_opaque_swapchain`, where old-device textures must not
    /// survive. The CPU cache keys on frame size, grid origin and font epoch,
    /// which a new face at the same cell box can repeat, so it goes too.
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
                let _ = self.video_abort_translucent_swapchain(
                    v,
                    crate::HOST_CAPTURE.compositor_blends_client,
                );
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
            // The backdrop follows the knobs: re-applied live on the GPU backend
            // (visible only when translucent); on CPU a non-`none` material warns
            // once.
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

    /// Apply the backdrop (macOS `NSVisualEffectView`, Windows DWM) and the
    /// window/layer opacity to every attached window from the resolved knobs, at
    /// attach and on a `background_opacity` / `background_material` reload. The
    /// theme `bg` restores the opaque titlebar fill when translucency turns off.
    /// Each window repaints so the swapchain composite mode reconciles.
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

    /// Live theme-only swap, the cheap sibling of [`Self::rebuild_backend`],
    /// reached through [`Self::install_colors`] (config reload,
    /// [`Self::sync_app_theme_to_appearance`]).
    /// Glyphs are coloured at draw time, so the new [`Theme`] goes onto the live
    /// backend without re-parsing fonts, dropping the atlas or re-gridding.
    ///
    /// Theme is not cell content, so content-diffing caches must be dropped: the
    /// strip-row cache (and macOS titlebar bg), per-window CPU pixel cache, GPU
    /// prev-frame, `RepaintKey` gate and introspection scratch.
    pub(crate) fn apply_theme_live(&mut self, new_theme: Theme) {
        // Assign first so later rebuilds and zooms bake the new theme.
        self.theme = new_theme;
        // Matrix rain derives its ramp from the theme; an OS appearance flip
        // skips config reload, so mark it dirty here.
        self.rain_dirty = true;
        match self.backend.ready_mut() {
            Backend::Cpu(r) => r.set_theme(new_theme),
            Backend::Gpu(g) => g.set_theme(new_theme),
        }
        self.introspect_gpu.invalidate_present();
        self.publish_theme_chrome();
        // OS appearance flips reach this without a config reload; refresh the
        // font fork so dark-nudge and theme-dependent raster state keep up.
        self.sync_chrome_fonts();
    }

    /// What the native (macOS toolbar) tab strip pins from the authored chrome:
    /// the selected tab's colour, and the accent floored to 3:1 on the theme's
    /// background, `None` keeping the system accent.
    pub(crate) fn native_strip_chrome(
        &self,
    ) -> (aterm_messages::tokens::AuthoredChrome, Option<[u8; 3]>) {
        let authored = self.installed_colors.authored_chrome();
        let bg = crate::settings::u32_rgb(self.theme.bg);
        let accent = authored.accent.map(|a| {
            aterm_messages::ink::ensure_contrast(a, bg, aterm_messages::tokens::ACCENT_FLOOR)
        });
        (authored, accent)
    }

    /// Push the installed [`Self::theme`] to the native chrome around every
    /// window: the seamless titlebar colour, the Linux CSD variant, the native
    /// toolbar strip's appearance, and each window's strip and raster caches.
    /// Shared by [`Self::apply_theme_live`] and a config reload's backend
    /// rebuild, which bakes the theme in itself.
    pub(crate) fn publish_theme_chrome(&mut self) {
        // Linux CSD: the header's light/dark variant follows this theme under
        // `window_theme = auto`, so re-push it. macOS/Windows chrome already
        // tracks via `window_set_background_color` / DWM.
        #[cfg(target_os = "linux")]
        let linux_chrome_theme = self.chrome_theme_for_apprt();
        let bg = self.theme.bg;
        let apprt = &self.apprt;
        let toolbars = &self._toolbars;
        let strip_dark = crate::tab_bar::theme_is_dark(bg);
        let (authored, strip_accent) = self.native_strip_chrome();
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
                crate::toolbar::set_active_tab_color(handle, authored.active_tab);
                crate::toolbar::set_strip_accent(handle, strip_accent);
            }
        }
    }

    /// Handle `WindowEvent::ScaleFactorChanged`: re-derive the window's font px
    /// (`round(size·scale)`, [`crate::font_px_for_scale`]) and pad for the new
    /// DPI, like [`Self::attach_os_window`] does at creation, whatever named the
    /// size. A forced `--scale` fixes the DPI and makes this a no-op.
    ///
    /// All windows share one backend, so the new scale is stored on this
    /// window's [`WindowState`] and applied lazily when it draws
    /// ([`Self::apply_window_scale`]); the `Resized` winit emits afterwards
    /// re-grids it.
    pub(crate) fn on_scale_factor_changed(&mut self, wid: WindowId, scale: f64) {
        // A force-pinned scale ⇒ DPI is intentionally fixed; ignore.
        if resolve_force_scale().is_some() {
            return;
        }
        let size = self.font_size;
        // Configured LOGICAL padding, resolved before the `windows` borrow (the
        // per-window record stays a function of the window's OWN scale).
        let (pad_l, pad_top_l) = (
            self.config.window_padding_or_default(),
            self.config.window_padding_top_or_default(),
        );
        let mut attached = None;
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.scale = scale;
            // Refresh this window's record from its own scale (the backend
            // re-tunes lazily); the head re-derives from the stored points band.
            // Attached windows only: a headless record holds the sealed boot band.
            if ws.os_window.is_some() {
                attached = Some(ws.head_pts);
                ws.metrics = crate::MetricsView::for_scale_padded(size, scale, pad_l, pad_top_l);
            }
        }
        // The head at the new scale and cell, in the stats Deck's form. The
        // frame is still the old scale's while this event is handled (winit
        // resizes after it on Windows and X11), so the form is kept (frame
        // 0) and the Resized that follows re-decides it from the real frame
        // (`App::deck_head_px`).
        if let Some(head_pts) = attached {
            let head = self.deck_head_px(wid, head_pts, scale, 0);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.metrics.head = head;
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
            // Keep the increment base on the whole-cell lattice at the new DPI
            // too (see `whole_cell_min_size`).
            #[cfg(target_os = "linux")]
            w.set_min_inner_size(Some(self.whole_cell_min_size(wid)));
        }
    }

    /// Re-tune the shared backend's font size and pad to window `wid`'s scale
    /// before it composes (top of `redraw_window`), giving per-window DPI on one
    /// renderer. A no-op when already tuned; only redraws that alternate across
    /// DPIs switch. A forced `--scale` fixes every window's px at launch.
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
        if resolve_force_scale().is_some() {
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
        // Track the drawing window's px so `self.font_px` reads this window's.
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
            None => Some(aterm_window::window::Fullscreen::Borderless(None)),
        };
        w.set_fullscreen(next);
        Ok(())
    }

    /// Path-referenced half of a byte-equal config reload: the parsed
    /// `aterm.toml` is unchanged, but files it names (sparkle lexicon, toy
    /// packs) may have changed. Compare fresh fingerprints with
    /// [`App::path_feed_fps`]:
    ///
    /// * unchanged: a full runtime no-op;
    /// * changed: arm `sparkle_dirty` to activate the compiled worker bundle,
    ///   hard-reset word decorations and request repaints. Nothing else can
    ///   have changed.
    fn refresh_path_feeds(&mut self, fresh: PathFeedFps) {
        if fresh == self.path_feed_fps {
            return;
        }
        self.path_feed_fps = fresh;
        self.sparkle_dirty = true;
        // Retire only word-owned episodes; cursor companions share the atlas
        // but have their own lifecycle.
        for ws in self.windows.values_mut() {
            ws.fx.decos_mut().hard_reset_words();
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
        // A file nothing draws changes no projection, so it costs no session
        // fan-out and no theme swap. A theme whose face changed (its file's
        // typography was edited, DTP §8) is prepared on the font worker and
        // installs with its colours; the swap is held until then.
        let reason = crate::app_colorscheme::ThemeChange::CatalogReload;
        if !self.request_theme_typography_refresh(reason) {
            self.install_resolved_colors(reason);
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
            collect_unaccepted_value_notices(
                &mut warns,
                &self.config,
                &snapshot.assets.themes,
                &snapshot.text,
            );
            for w in warns.sentences() {
                crate::logging::stderr_line!("aterm-gui: {w}");
            }
            let scope = [ConfigFamily::UnacceptedValues.key()];
            self.note_config_settings_shows(&scope, &warns.settings_shown_keys());
            let messages = warns.into_messages_routed(self.settings_in_front());
            self.replace_config_messages_keyed(&scope, messages);
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
    ///
    /// `reason` is what asked (a config observation is `ConfigReload`); the
    /// publish installs the colours under it. Returns whether the request was
    /// taken (queued, or prepared and applied inline with no event loop);
    /// `false` on every refusal, so a theme refresh falls back to its
    /// colour-only install.
    fn request_font_catalog_generation(
        &mut self,
        mut prepared: crate::native_config_service::PreparedConfigObservation,
        reason: crate::app_colorscheme::ThemeChange,
    ) -> bool {
        let Some(primary) = self.backend.ready().primary_seed() else {
            self.native_config_service.mark_reconciliation_required();
            self.reject_config_watch_admission_for(
                &prepared.observation.baseline,
                crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
            );
            aterm_log::warn!("config reload: active renderer has no immutable primary font bytes");
            return false;
        };
        let Some(previous_sources) = self.backend.admitted_font_sources() else {
            self.native_config_service.mark_reconciliation_required();
            self.reject_config_watch_admission_for(
                &prepared.observation.baseline,
                crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
            );
            aterm_log::warn!("config reload: active renderer font generation is unavailable");
            return false;
        };
        let sequence = self.next_font_catalog_sequence.max(1);
        self.next_font_catalog_sequence = sequence.saturating_add(1);
        self.requested_font_catalog_sequence = sequence;
        if !std::sync::Arc::ptr_eq(&prepared.assets.themes, &self.config_assets.themes) {
            // A theme generation can overtake the config worker; trail/kitty and
            // Sparkle consumers do not depend on it, so rebase the Arc in memory.
            prepared.assets = std::sync::Arc::new(ConfigAssetCatalog {
                trail_style: std::sync::Arc::clone(&prepared.assets.trail_style),
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
            reason,
        };
        let baseline = request.prepared.observation.baseline.clone();
        if let Some(lane) = self.native_font_catalog.as_mut() {
            if lane.request(request) {
                return true;
            }
        } else if self.proxy.is_none() {
            self.finish_font_catalog_generation(crate::native_font_catalog::prepare(request));
            return true;
        }
        // Keep the requested ticket to reject an older completion, but settle it:
        // this refusal has no future completion that could release a theme hold.
        self.finished_font_catalog_sequence = self.finished_font_catalog_sequence.max(sequence);
        self.native_config_service.mark_reconciliation_required();
        self.reject_config_watch_admission_for(
            &baseline,
            crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
        );
        aterm_log::warn!("config reload: font catalog worker unavailable; keeping current config");
        false
    }

    /// The config every font reader sees: the live config with the installed
    /// theme typography filled into the font keys it leaves unset
    /// ([`crate::resolved_typography::ResolvedTypography::overlay`]).
    pub(crate) fn font_view(&self) -> std::borrow::Cow<'_, Config> {
        self.typography.overlay(&self.config)
    }

    /// Whether a font generation can be asked for now without a refusal: the
    /// renderer holds an immutable primary and a sealed font generation, and
    /// a worker (or, with no event loop, the inline path) can prepare it.
    fn font_generation_available(&self) -> bool {
        #[cfg(test)]
        if self.font_worker_refuses_for_test {
            return false;
        }
        self.backend.ready().primary_seed().is_some()
            && self.backend.admitted_font_sources().is_some()
            && (self.native_font_catalog.is_some() || self.proxy.is_none())
    }

    /// The theme typography state changed (a catalog reload or an OS flip):
    /// whether the face it draws differs from the installed one. If it does,
    /// the colour swap is HELD and a font generation is (or already will be)
    /// prepared for it from the service's admitted observation, so the face
    /// and its colours land together (DTP §6.4, `FontThemeGeneration`'s
    /// `FaceMatchesColours`). `false` tells the caller to install the colours
    /// now: the face is unchanged, or nothing can prepare it (a refused
    /// worker), so a flip is never lost.
    pub(crate) fn request_theme_typography_refresh(
        &mut self,
        reason: crate::app_colorscheme::ThemeChange,
    ) -> bool {
        let next = self
            .config
            .resolved_typography(self.os_appearance, &self.config_assets.themes);
        if next.face_key() == self.typography.face_key() {
            return false;
        }
        // A prepare is outstanding: its completion re-prepares on the newer
        // theme generation or appearance, and its publish releases the swap.
        if self.finished_font_catalog_sequence < self.requested_font_catalog_sequence {
            self.held_theme_change = Some(reason.merged(self.held_theme_change));
            return true;
        }
        let Some(observation) = self.native_config_service.admitted_observation() else {
            // An unwritten edit or an unreconciled write: the generation that
            // admits it resolves the typography of this state, so the colours
            // install now and the face follows with it.
            return false;
        };
        if !self.font_generation_available() {
            return false;
        }
        let snapshot = self.native_config_service.snapshot();
        let prepared = crate::native_config_service::PreparedConfigObservation {
            observation,
            config: (*snapshot.config).clone(),
            values: (*snapshot.semantic_values).clone(),
            disk_faults: std::sync::Arc::clone(&snapshot.disk_faults),
            path_feeds: PreparedPathFeedGeneration {
                sparkle: self.prepared_sparkle.clone(),
                fingerprints: self.path_feed_fps,
            },
            assets: snapshot.assets,
        };
        // Held before the request: with no event loop it applies inline, and
        // that apply releases the swap.
        self.held_theme_change = Some(reason.merged(self.held_theme_change));
        if self.request_font_catalog_generation(prepared, reason) {
            true
        } else {
            self.held_theme_change = None;
            false
        }
    }

    pub(crate) fn finish_font_catalog_generation(
        &mut self,
        mut completion: crate::native_font_catalog::Completion,
    ) {
        if let Some(lane) = self.native_font_catalog.as_mut() {
            lane.worker_drained();
        }
        self.finished_font_catalog_sequence =
            self.finished_font_catalog_sequence.max(completion.sequence);
        match crate::native_font_catalog::completion_disposition(
            self.requested_font_catalog_sequence,
            completion.sequence,
            self.theme_catalog_generation,
            completion.theme_generation,
            self.os_appearance,
            completion.appearance,
        ) {
            crate::native_font_catalog::CompletionDisposition::Publish => {}
            crate::native_font_catalog::CompletionDisposition::RejectStaleConfig => return,
            crate::native_font_catalog::CompletionDisposition::ReprepareLatestTheme => {
                let path_feeds = PreparedPathFeedGeneration {
                    sparkle: completion.sparkle.clone(),
                    fingerprints: completion.path_feed_fps,
                };
                // The re-preparation carries the reason of everything it now
                // answers: the completion's own and a swap held meanwhile.
                let reason = completion.reason.merged(self.held_theme_change);
                let queued = self.request_font_catalog_generation(
                    crate::native_config_service::PreparedConfigObservation {
                        observation: completion.observation,
                        config: completion.config,
                        values: completion.values,
                        disk_faults: completion.disk_faults,
                        assets: completion.assets,
                        path_feeds,
                    },
                    reason,
                );
                // A refused re-preparation must not leave the swap held: no
                // generation is coming to release it.
                if !queued && let Some(reason) = self.held_theme_change.take() {
                    self.install_resolved_colors(reason);
                }
                return;
            }
        }
        if let Some(fonts) = completion.fonts.as_mut() {
            fonts.renderer.set_px(self.font_px);
        }
        self.apply_prepared_config_generation(completion.into_generation());
    }

    #[cfg(all(test, unix))]
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
        self.request_font_catalog_generation(
            prepared,
            crate::app_colorscheme::ThemeChange::ConfigReload,
        );
    }

    /// Admit a config generation. For `HarnessPolicyOneWriter` this is
    /// `AdmitOn`/`AdmitOff`; it writes no policy (the watcher is its one writer).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HarnessPolicyOneWriter",
            action = "AdmitOn",
            project = "aterm_gui::config_watcher::tests::project_harness_writer"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
            font_refusals,
            reason: generation_reason,
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
                // No generation is coming for a held colour swap: land it.
                if let Some(reason) = self.held_theme_change.take() {
                    self.install_resolved_colors(reason);
                }
                return;
            }
        };
        self.prepared_sparkle = prepared_sparkle;
        // The theme's type settings this generation draws, against its own
        // catalog and the live side.
        let typography =
            config.resolved_typography(self.os_appearance, &config_snapshot.assets.themes);
        // The colours install under the generation's reason, folded with a
        // swap held while its face was prepared (released here either way).
        let released = self.held_theme_change.take();
        let install_reason = generation_reason.merged(released);

        // Semantic dedupe: re-applying an identical config is side-effect churn
        // (engine re-diffs, word-deco resets, popup cancels, backend probes), so an
        // equal parse stops here (field-wise `PartialEq`; the native snapshot has
        // already seen the new text).
        //
        // Path-referenced files (sparkle lexicon, toy packs) are also applied
        // state, and a feed-only edit arrives as a byte-equal observation, so
        // `refresh_path_feeds` still checks their fingerprints.
        //
        // The theme's typography is applied state too: a theme-only face
        // change (a catalog reload or an OS flip, with the config unchanged
        // and no new font files) must not stop here.
        if config == self.config
            && prepared_fonts.is_none()
            && typography.face_key() == self.typography.face_key()
        {
            // Same face; the record of which theme drew it may still move.
            self.typography = typography;
            // A held colour swap whose face turned out unchanged still lands.
            if released.is_some() {
                self.install_resolved_colors(install_reason);
            }
            // Comments, formatting and explicit-default edits still advance the
            // service revision; publish the snapshot so the next Settings patch
            // starts from the current base.
            self.publish_config_assets(std::sync::Arc::clone(&config_snapshot.assets));
            self.publish_native_config_snapshot(&config_snapshot);
            self.publish_settings_font_refusals(font_refusals);
            self.refresh_path_feeds(fresh_feeds);
            self.finish_native_config_external_admission(&admitted_baseline);
            // Text-derived diagnostics still run: a misspelled key leaves no trace
            // in `Config`. Only the text-derived families (and an answered launch
            // load failure) are replaced, possibly with an empty set so a fixed
            // typo clears; keybinding, font and trail rows stay.
            let mut warns = crate::message_reporters::ConfigWarnings::default();
            collect_key_notices(&mut warns, &config_snapshot.text);
            // A removed feature's settings are an entry, not a band row.
            self.note_removed_settings(&config_snapshot.text);
            // The unaccepted-values row is rebuilt from scratch, so it restates an
            // ignored `font_px` or it would vanish on an unrelated edit.
            collect_unaccepted_value_notices(
                &mut warns,
                &config,
                &config_snapshot.assets.themes,
                &config_snapshot.text,
            );
            for w in warns.sentences() {
                crate::logging::stderr_line!("aterm-gui: {w}");
            }
            let scope = [
                crate::message_reporters::ConfigFamily::IgnoredKeys.key(),
                crate::message_reporters::ConfigFamily::RetiredKeys.key(),
                crate::message_reporters::ConfigFamily::UnacceptedValues.key(),
                crate::message_reporters::KEY_LAUNCH_LOAD,
            ];
            self.note_config_settings_shows(&scope, &warns.settings_shown_keys());
            let messages = warns.into_messages_routed(self.settings_in_front());
            self.replace_config_messages_keyed(&scope, messages);
            return;
        }
        // Renderer construction is the one fallible step; snapshot its authority
        // first so only this slice rolls back if the font build fails.
        let reload_render_before = ReloadRenderSnapshot::capture(self);

        // Restart-only keys (`columns`/`lines`, `gpu`): diff against the old config
        // before it is overwritten so the change is reported, not silently ignored.
        let restart_notices = restart_notices(&self.config, &config);
        // How each problem this reload may find fixed ENDED, read before the old
        // config and assets are replaced: the words its ✓ record takes are
        // decided now that it is known, never when the warning was posted.
        let fixed_as = config_fix_observations(
            &self.config,
            &self.config_assets,
            &config,
            &config_snapshot.assets,
        );

        // A changed resolved `window_padding`/`window_padding_top` must re-grid
        // every attached window (pad feeds `grid_dims_for`); diffed against the old
        // config before it is overwritten.
        let old_padding = (
            self.config.window_padding_or_default(),
            self.config.window_padding_top_or_default(),
        );
        let old_stats_bars = self.config.stats_bars_or_default();
        let old_deck_size = self.config.stats_deck_size_or_default();

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
        // The Claude footer's `[claude_footer]` switches change what every
        // window's Claude Code panes show — an idle background window too,
        // which no other branch below repaints.
        let claude_footer_changed = self.config.claude_footer != config.claude_footer;
        // `[ai]` or its fleet document moved: the AI CLIs are looked at again
        // (or no longer), after the swap so the look reads the new table.
        let ai_changed = self.config.ai != config.ai;
        self.config = config.clone();
        self.reconcile_consent_config();
        let _ = self.reconcile_consent_observer();
        // The fleet listener follows `[fleet]` live: a changed table rebinds it.
        crate::fleet_listen::configure_fleet_listener(self.config.fleet.as_ref());
        // The fleet glance follows `[fleet] watch`.
        self.follow_fleet_watch();
        // What every font reader below sees: the config with the theme's
        // applied typography in the keys it leaves unset. Explicitness of the
        // font size still reads the authored config.
        let view = typography.overlay(&config).into_owned();
        self.font_size_scale_milli = view.font_size_scale_milli_or_default();
        self.typography = typography;
        if claude_footer_changed {
            self.redraw_every_window();
        }
        if ai_changed {
            self.ai_config_changed();
        }
        // Secure Keyboard Entry is process-level (Carbon secure input): record the
        // wish once per commit (engagement is focus-gated in `secure_input`);
        // idempotent. A refusal goes to the `warns` banner below: the swap has
        // already succeeded, so without it Settings would show on over a
        // protection that is off.
        let secure_input_refusal =
            crate::secure_input::set_desired(self.config.secure_keyboard_entry_or_default()).err();
        if let Some(voice) = audition_voice {
            // After the swap, so the preview plays at the new volume and master.
            self.audition_typing_sound(voice);
        }
        // Pin the authored chrome (`active_tab_color`, `accent_color`) on every
        // reload: a tab-color-only edit triggers no rebuild branch below. The
        // setters are idempotent atomic compares.
        {
            let (authored, strip_accent) = self.native_strip_chrome();
            for handle in self._toolbars.values() {
                crate::toolbar::set_active_tab_color(handle, authored.active_tab);
                crate::toolbar::set_strip_accent(handle, strip_accent);
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
        self.publish_settings_font_refusals(font_refusals);
        // Tab chrome reads config live (the connection mark's opt-out among it), so
        // rebuild and repaint every window's strip now rather than at its next
        // unrelated refresh.
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        self.refresh_tab_chrome_windows(windows);
        // Tab status policy is copied into each classifier at construction, so
        // push it to existing sessions and the event loop's wait deadline.
        self.reconfigure_session_status();
        // `desktop_alerts` is live: the notification thread reads the switch
        // per message, so aterm's own notices stop (or resume) with this store.
        self.apply_desktop_alerts(config.desktop_alerts_or_default());
        // A palette-refused digest line ends once palette sets are allowed
        // again: the line's one action is turning the setting on, so keeping
        // it after the owner did would point at work already done.
        if config.allow_palette_reconfigure_or_default() {
            self.except_clear(&crate::message_reporters::palette_refused_key());
        }
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
        // (`app_take_over.rs`).
        self.adopt_presence_config();
        self.palette_refresh_live();
        // Re-publish the Settings ▸ Packages projection (memory-only) so consent
        // changes show immediately.
        self.publish_native_packages_state();
        // A Sparkle Words reload retires word episodes and done marks but keeps
        // the cursor companion's placement and sprite caches.
        if sparkle_feed_changed {
            for ws in self.windows.values_mut() {
                ws.fx.decos_mut().hard_reset_words();
            }
        }
        // A changed config ends a theme preview (`theme preview`, a palette
        // choice), the rolled-back half of a rejected rebuild included, except
        // a Settings trial that still draws ([`ThemeChange::keeps_preview`]):
        // that one stands in for `theme` here, so the reload installs once.
        // `self.config` and the catalog are this generation's by now. A face
        // refresh a catalog reload or an OS flip asked for keeps the preview,
        // exactly as their colour-only install would.
        self.settle_theme_preview(install_reason);
        // The colours this generation draws, resolved once: the renderer theme
        // and knobs below and the engine install at the end all project it.
        let colors = config.resolved_colors_previewing(
            self.os_appearance,
            &config_snapshot.assets.themes,
            self.theme_preview
                .as_ref()
                .map(|preview| preview.id.as_str()),
        );
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
        // Summarize in a New Tab is grey for a shell no worker's line is
        // written for (`App::summarize_start_block`).
        self.mark_status_dirty();
        // The Windows console host (`conpty`) is spawn-time too: a changed value
        // reaches the next tab, and an unchanged one keeps its resolution, a host
        // that already failed to start included, so a save never retries it. A
        // no-op off Windows.
        aterm_pty::set_console_host(aterm_pty::ConsoleHostChoice::from_config(
            config.conpty.as_deref(),
        ));
        // `temporal_recording` is a session-factory opt-in; refresh it for new
        // tabs.
        self.session_factory.temporal_recording = config.temporal_recording.unwrap_or(false);
        // The warm spare (`crate::spare`): one spawned by the old factory goes,
        // and a mode change (`spare_shell = "off"` is the kill switch) applies at
        // the next tick.
        self.spare_config_changed();
        // Re-push the global search depth cap (⌘F and the `search` verb); it is
        // part of the index cache key, so the next search rebuilds at the new depth.
        crate::control::set_search_max_lines(search_index_depth(&config));

        // Input config applies live on the next keypress. Keybindings use
        // `resolved_warn` so platform defaults stay overlaid, as in `App::new`.
        // Warnings are collected per family and replace the band's `config.*`
        // rows as a set at the end of this reload.
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
        // Restart-only edits (columns/lines) ride the same band: an edit that did
        // not fully take effect.
        warns.extend(ConfigFamily::Restart, restart_notices);
        // Ignored keys too: a misspelled key has no trace in the parsed config.
        // Retired spellings are recorded, not shown (`collect_key_notices`), and a
        // removed feature's settings are an entry naming what replaced it.
        collect_key_notices(&mut warns, &config_snapshot.text);
        self.note_removed_settings(&config_snapshot.text);
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
        // The font rows say a family or face they could not use ("Not found").
        warns.extend_shown_in_settings(ConfigFamily::Fonts, font_prepare_warnings.drain(..));
        // An unrecognized `cursor_trail_style` draws the default; warn on the band.
        if let Some(w) = config.cursor_trail_style_warning() {
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
        // the decode verdict on the same band (the kitty-sprite rule). The
        // Wallpaper page says it too; no page shows the cursor sprite's.
        if let Some(reason) = config_snapshot.assets.wallpaper.diagnostic() {
            let source = config_snapshot
                .assets
                .wallpaper
                .source_id()
                .unwrap_or("configured source");
            warns.push_shown_in_settings(
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
        // The supervisor's refused `[harness]` values, a `font_px` the resolver
        // ignores, a theme the window draws Default in place of, and every other
        // refused value — as at launch.
        collect_unaccepted_value_notices(
            &mut warns,
            &config,
            &config_snapshot.assets.themes,
            &config_snapshot.text,
        );
        for w in warns.sentences() {
            crate::logging::stderr_line!("aterm-gui: {w}");
        }
        // Post warnings, one row per family. A fresh (possibly empty) set first
        // resolves every live `config.` row, so fixed issues clear; windows
        // repaint. A problem Settings says on its own row (a font it did not
        // find, a wallpaper that did not load, a refused `font_px` or theme) is
        // a record while Settings is in front — the banner would repeat the row
        // and push the page down — and becomes one whenever Settings comes to
        // the front later (`App::record_config_rows_settings_shows`).
        let all_keys: Vec<&'static str> = ConfigFamily::ALL
            .iter()
            .map(|family| family.key())
            .chain([crate::message_reporters::KEY_FONT_FAMILY])
            .collect();
        let mut shown = warns.settings_shown_keys();
        // The backend's `font_family` verdict is always the Font family row's "Not
        // found".
        shown.push(crate::message_reporters::KEY_FONT_FAMILY);
        self.note_config_settings_shows(&all_keys, &shown);
        let messages = warns.into_messages_routed(self.settings_in_front());
        self.replace_config_messages_after_font_work(messages, family_admitted, &fixed_as);
        // Every open window's Option follows from the sync that ends this callback
        // (`App::sync_option_as_alt`).
        self.option_as_meta = config.option_as_meta_or_default();
        self.confirm_multiline_paste = config.confirm_multiline_paste_or_default();
        // Copy-on-select applies from the next selection.
        self.copy_on_select = config.copy_on_select_or_default();

        // Tab-strip rows are window chrome: a change re-splits the window between the
        // strip and the terminal, so re-grid (like a resize) if it changed.
        // A window that draws its own frame keeps the header's row whatever the
        // config says (`crate::client_frame`, LW.5).
        self.set_tab_strip_rows(
            self.strip_rows_for(resolve_tab_strip_rows(&config)),
            std::time::Instant::now(),
        );

        // Re-apply `window_theme` live and cache it for windows attached later.
        // Light/Dark are idempotent; Auto clears the forced appearance.
        let new_window_theme = config.window_theme_or_default();
        if new_window_theme != self.window_theme {
            self.window_theme = new_window_theme;
            // Chrome painters resolve from this field too
            // (`chrome_palette_theme`), and the strip row cache fingerprint has no
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
        // Windows SDR surface to the throttled output-HDR probe.
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
            // Warm the effect pipelines a few frames ahead of first use (see
            // `Config::warms_effect_pipelines`); the launch path
            // (`pin_gpu_effect_config`) does not warm. Idempotent.
            if config.warms_effect_pipelines() {
                g.warm_effect_pipelines();
            }
        }

        // Re-fold the focus-boost want-set now so a `focus_boost` flip applies to
        // an already-focused window immediately.
        self.recompute_focus_boost();

        // Renderer theme, font size and family: rebuild only when something the
        // backend bakes in changed, so a comment edit costs nothing visible.
        let new_theme = colors.theme();
        // The size (px at 1×) this config names, times its text size scale, else
        // the default — the same derivation as startup. Every window draws it at
        // its own scale; the shared backend at the front window's.
        let new_default_font_size = resolve_scaled_font_px(&view);
        // Re-apply the size only when the derived default changed, so a live zoom
        // survives unrelated edits and Cmd-0 resets to the current default.
        let default_changed = (new_default_font_size - self.default_font_size).abs() > 0.01;
        self.default_font_size = new_default_font_size;
        let new_font_size = if default_changed {
            new_default_font_size
        } else {
            self.font_size
        };
        let new_font_px = crate::font_px_for_scale(new_font_size, self.front_scale());
        self.font_size = new_font_size;
        // The renderer bakes the theme in, so any change needs a backend rebuild.
        let theme_changed = new_theme != self.theme;
        let font_changed = (new_font_px - self.font_px).abs() >= 0.5;
        // Effective family keeps `--font` > config > platform default, via the
        // same `effective_font_family` as startup and `--show-config`. A rejected
        // family keeps the last admitted face; the failure is reported above.
        let effective_family = requested_effective_family;
        let family_changed = effective_family != self.font_family;
        // Text shaping: update the source of truth before a possible rebuild; if
        // none is needed it is pushed directly below.
        let new_shaping = view.text_shaping();
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
        let new_knobs = RenderKnobs::from_config(&view, &colors);
        let knob_changes = self.render_knobs.diff(&new_knobs);
        self.render_knobs = new_knobs;
        // Font config: update the source of truth before installing the
        // renderer generation the catalog worker prepared.
        let font_cfg_changed = new_font_config != self.font_config;
        self.font_config = new_font_config;
        // Variable-font requests: a change re-instantiates the primary and can
        // move cell geometry, so it takes the full rebuild path. Malformed-entry
        // warnings were shown at startup; a reload re-parses silently.
        let (new_variations, _vf_warns) = view.font_variation_requests();
        let new_dark_nudge = view.font_weight_dark_nudge_or_default();
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
        // re-grids, since `on_resize` divides by `ws.metrics` (as in `set_font_size`).
        self.font_px = new_font_px;
        self.refresh_all_window_metrics();
        // A padding edit without a rebuild: the refresh above updated each pad but
        // not the cell count, so re-grid each sized window and redraw. A rebuild
        // re-grids on success and restores the prior metrics on failure.
        let new_padding = (
            config.window_padding_or_default(),
            config.window_padding_top_or_default(),
        );
        // The Deck (`[sys] stats_bars`) is part of the head, so a change into or
        // out of it re-grids the same way. The sill and off take no row, so a
        // change between them re-grids nothing (only repaints, below).
        let new_stats_bars = config.stats_bars_or_default();
        // `[sys] stats_deck_size` sizes the Deck: the refresh above re-decided
        // every attached window's form under the new setting, so a change of it
        // re-grids the same way while the Deck takes room.
        let new_deck_size = config.stats_deck_size_or_default();
        let stats_bars_changed = old_stats_bars.takes_room() != new_stats_bars.takes_room()
            || (new_stats_bars.takes_room() && old_deck_size != new_deck_size);
        // View ▸ Stats Bars' radios read the applied form and size
        // (`validateMenuItem:` cannot reach the App), so every apply
        // republishes them.
        crate::menu::set_stats_bars_form(new_stats_bars);
        crate::menu::set_deck_size_pref(new_deck_size);
        if old_stats_bars != new_stats_bars {
            for ws in self.windows.values_mut() {
                ws.stats_lens = crate::stats_lens::LensSlot::default();
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        if !backend_rebuild_needed && (old_padding != new_padding || stats_bars_changed) {
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
        // A rejected rebuild rolls the render config back and installs the
        // colours of that config itself, so this generation's must not follow.
        let mut rolled_back = false;
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
            rolled_back = !rebuild_succeeded;
            // Publish theme-dependent native chrome only after the fallible face
            // commit, so a rejected family cannot leave the titlebar out of step.
            if rebuild_succeeded && theme_changed {
                self.publish_theme_chrome();
            }
        } else if theme_changed {
            // Colour-only edit: the face and atlas stay valid, so the theme goes
            // onto the live backend (`install_colors` below, through
            // `apply_theme_live`) instead of a full rebuild. Shaping, typography
            // and render knobs changed in the same save apply live here first.
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
            // Each changed knob → one renderer call (LineHeight took the rebuild).
            for &change in &knob_changes {
                self.apply_render_knob(change);
            }
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
            // Each changed knob → one renderer call (LineHeight took the rebuild).
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
            // still request a repaint (the redraw early-out skips it if nothing moved).
            if let Some(w) = self.front().and_then(|ws| ws.os_window.as_ref()) {
                w.request_redraw();
            }
        }
        // The one theme-install path: engine palette and reported scheme on every
        // session and the factory, a live theme swap when the chrome changed, and
        // every colour-derived invalidation.
        if !rolled_back {
            self.install_colors(colors, install_reason);
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
        // on the backend.
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
    /// height `cell_h`. Zero with the strip off, under `tab_band_height =
    /// "compact"` (the macOS default), or when the strip rows already reach the
    /// target.
    ///
    /// A `head` band rather than a bigger `pad_top`: `pad_top` is grid padding
    /// that applies with the strip off too, while `head` already means host chrome
    /// over the grid everywhere (`grid_dims_for`, `frame_size`,
    /// `pixel_to_term_cell`, …). Takes `cell_h` because attach and the early
    /// reveal hold a cell box the per-window `MetricsView` does not yet have.
    pub(crate) fn synthetic_strip_head_px(&self, scale: f64, cell_h: usize) -> usize {
        // A sized band also reserves its foot (`tab_bar::STRIP_FOOT_LOGICAL_PX`,
        // Windows): the terminal's ground under the seam, so the band keeps its
        // target height and the grid's first row starts that much lower.
        let target = self.config.tab_band_height_or_default().target_logical_px();
        let foot = if target > 0.0 {
            crate::tab_bar::STRIP_FOOT_LOGICAL_PX
        } else {
            0.0
        };
        synthetic_band_head_px(
            target + foot,
            self.cfg_pad_top_for_scale(scale),
            self.tab_strip_rows,
            cell_h,
            scale,
        )
    }

    /// Re-resolve every window's [`MetricsView`] from the live font size: each
    /// window draws it at its own scale ([`crate::font_px_for_scale`]; a forced
    /// `--scale` stands in for every window's), whatever named the size, and
    /// an attached window's pad re-resolves from the live config at its scale.
    /// Never-attached windows keep their sealed pad and head.
    pub(crate) fn refresh_all_window_metrics(&mut self) {
        let forced = resolve_force_scale();
        let size = self.font_size;
        // Resolve the logical padding once, outside the `windows` borrow.
        let (pad_l, pad_top_l) = (
            self.config.window_padding_or_default(),
            self.config.window_padding_top_or_default(),
        );
        let mut attached = Vec::new();
        for (wid, ws) in self.windows.iter_mut() {
            // The chrome headroom re-derives from the attach-captured points at
            // the window's scale, below. A never-attached window keeps its sealed
            // record ([`Self::seed_headless_boot_metrics`]), including the
            // `$ATERM_HEADROOM_PX` band, and its sealed pad.
            let (pad, pad_top) = if let Some(w) = ws.os_window.as_ref() {
                attached.push((*wid, ws.head_pts, ws.scale, w.inner_size().height));
                let pad = crate::logical_to_device_px(pad_l, ws.scale);
                (
                    pad,
                    crate::logical_to_device_px(pad_top_l, ws.scale).min(pad),
                )
            } else {
                (ws.metrics.pad, ws.metrics.pad_top)
            };
            let font_px = crate::font_px_for_scale(size, forced.unwrap_or(ws.scale));
            ws.metrics = crate::MetricsView::applied(font_px, pad, pad_top, ws.metrics.head);
        }
        // An attached window's head at its new cell, the stats Deck's form
        // re-decided from its frame and the live `[sys] stats_deck_size`
        // (`App::deck_head_px`).
        for (wid, head_pts, scale, frame_h) in attached {
            let head = self.deck_head_px(wid, head_pts, scale, frame_h);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.metrics.head = head;
            }
        }
        // The cell box may have moved (zoom, font or theme reload): report it to
        // the engines now, since background sessions still answer DEC 1016 and
        // size OSC 1337 images. The per-window memo keys on the resolved size, so
        // unchanged windows are free and a face swap (resolved here against the
        // still-installed face) still pushes at the first `apply_window_scale`.
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
        // (1) Unparseable tokens.
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
        let old_default_font_size = app.default_font_size;
        let old_font_size = app.font_size;
        let old_font_px = app.font_px;
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
        let old_typography = app.typography.clone();
        let old_size_scale = app.font_size_scale_milli;
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
font_size_scale = 1.1
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
        app.default_font_size = 23.0;
        app.font_size = 23.0;
        app.font_px = 23.0;
        app.theme = candidate.theme_for(app.os_appearance);
        app.font_family = Some("Menlo".to_string());
        app.text_shaping = candidate.text_shaping();
        app.text_blending = candidate.text_blending_or_default();
        app.font_thicken = candidate.font_thicken_or_default();
        app.stem_gamma = candidate.stem_gamma_or_default();
        let (variations, _) = candidate.font_variation_requests();
        app.font_variations = variations;
        app.font_weight_dark_nudge = candidate.font_weight_dark_nudge_or_default();
        app.render_knobs = RenderKnobs::from_config(
            &candidate,
            &candidate.resolved_colors(app.os_appearance, &app.config_assets.themes, None),
        );
        app.font_config = FontConfig::from_config(&candidate).0;
        // A theme's typography and the size scale are render authority too.
        app.typography = crate::resolved_typography::ResolvedTypography {
            id: "Dracula".to_string(),
            applied: aterm_types::scheme::parse_theme_str(
                "fg = #ffffff\nbg = #000000\nline_height = 1.5\n",
            )
            .expect("a typography theme")
            .typography,
            ..crate::resolved_typography::ResolvedTypography::none(app.os_appearance)
        };
        app.font_size_scale_milli = candidate.font_size_scale_milli_or_default();
        assert_ne!(
            app.typography, old_typography,
            "candidate typography is live"
        );
        app.refresh_all_window_metrics();
        assert_ne!(app.windows[&wid].metrics, old_metrics, "candidate is live");

        assert!(
            !app.finish_reload_render_transaction_with(previous, |candidate_app| {
                assert_eq!(candidate_app.font_px, 23.0, "failure sees candidate");
                assert_eq!(candidate_app.render_knobs.line_height, 1.4);
                false
            })
        );

        // Every render key of the candidate rolls back; the reload's unrelated
        // live edit survives.
        let mut expected = old_config.clone();
        expected.copy_on_select = Some(true);
        assert!(
            app.config == expected,
            "config authority rolls back the render slice only"
        );
        assert_eq!(app.default_font_size, old_default_font_size);
        assert_eq!(app.font_size, old_font_size);
        assert_eq!(app.font_px, old_font_px);
        assert_eq!(app.theme, old_theme);
        assert_eq!(app.font_family, old_family);
        assert_eq!(app.text_shaping, old_shaping);
        assert_eq!(app.text_blending, old_blending);
        assert_eq!(app.font_thicken, old_thicken);
        assert_eq!(app.stem_gamma, old_stem_gamma);
        assert_eq!(app.font_variations, old_variations);
        assert_eq!(app.font_weight_dark_nudge, old_dark_nudge);
        assert_eq!(app.render_knobs, old_knobs);
        assert_eq!(app.font_config, old_font_config);
        assert_eq!(app.typography, old_typography);
        assert_eq!(app.font_size_scale_milli, old_size_scale);
        assert_eq!(app.windows[&wid].metrics, old_metrics);
    }

    /// A face rebuild that keeps the cell box (a `font_weight` edit on a variable
    /// face, as the catalog worker prepares it) repaints a CPU window in full: the
    /// CPU damage cache keys on frame size, grid origin and font epoch, all of
    /// which a sealed generation can repeat.
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
mod config_policy_tests {

    use super::Config;

    fn cfg(source: &str) -> Config {
        aterm_toml::from_str(source).expect("valid config")
    }

    /// A config that still sets every retired smart-title key, including two
    /// closed enums whose values `Config` does not know
    /// (`title_summary_provider = "ollama"`, `tab_title_format = "description"`),
    /// loads with every other setting intact, and each key is told once, in the
    /// retired-key family, that it does nothing. Refusing the file instead would
    /// put the whole window on `Config::stand_in`, every other setting back at
    /// its default.
    #[test]
    fn a_config_that_still_sets_smart_title_keys_loads_with_every_other_setting() {
        const RETIRED: &[(&str, &str)] = &[
            ("descriptive_titles", "true"),
            ("title_summary_provider", "\"ollama\""),
            ("title_summary_model", "\"qwen3.5:4b-q4_K_M\""),
            (
                "title_summary_endpoint",
                "\"http://127.0.0.1:11434/api/chat\"",
            ),
            ("title_summary_token_file", "\"/tmp/aterm-summary-token\""),
            ("title_summary_timeout_seconds", "42"),
            ("title_summary_proxy_mode", "\"direct\""),
            ("title_summary_ca_file", "\"/tmp/private-model-ca.pem\""),
            ("title_summary_interval_seconds", "15"),
            ("title_summary_context_lines", "24"),
            ("title_summary_include_output", "false"),
            ("title_summary_allow_remote", "true"),
            ("tab_title_format", "\"description\""),
            ("window_title_format", "\"description-title\""),
        ];
        let others = concat!(
            "theme = \"Nord\"\n",
            "font_px = 17.0\n",
            "scrollback_lines = 4321\n",
            "tab_status_badge = false\n",
            "[harness]\n",
            "limit_reset = true\n",
        );
        // The retired keys are top-level, so they sit above the first table.
        let retired: String = RETIRED
            .iter()
            .map(|(key, value)| format!("{key} = {value}\n"))
            .collect();
        let source = format!("{retired}{others}");

        let loaded = Config::parse(&source).expect("retired keys never fail the file");
        let baseline = Config::parse(others).expect("the other settings parse alone");
        assert!(
            loaded == baseline,
            "every other setting loads exactly as it would without the retired keys"
        );
        assert!(
            loaded != Config::stand_in(&source),
            "the file is not replaced by the stand-in defaults"
        );
        assert_eq!(loaded.theme.as_deref(), Some("Nord"));
        assert_eq!(loaded.font_px, Some(17.0));
        assert_eq!(loaded.scrollback_lines, Some(4321));
        assert_eq!(loaded.tab_status_badge, Some(false));

        // The notice: one retired-key line per key, in source order, and none
        // of them in the ignored ("unknown to this build") family or among the
        // values a reader could not use.
        let warnings = crate::native_config_language::key_warnings(&source);
        assert!(warnings.ignored.is_empty(), "{:?}", warnings.ignored);
        assert_eq!(
            warnings.retired.len(),
            RETIRED.len(),
            "{:?}",
            warnings.retired
        );
        for ((line, (key, _)), number) in warnings.retired.iter().zip(RETIRED).zip(1..) {
            assert_eq!(
                line,
                &format!(
                    "line {number}: {key} does nothing: Smart Titles was removed; safe to delete"
                )
            );
        }
        assert!(
            super::unaccepted_value_notices(&source, &[]).is_empty(),
            "a retired key's value is never reported as a value its reader rejects"
        );
    }

    /// THE PRESENCE BAND AND THE CHOICE CHIME STILL LOAD (both retired by the
    /// owner surfaces, 2026-10-16): a file that sets `choice_sound` and
    /// `[presence] band` loads with every other setting intact (the needs
    /// switch beside them included), and each retired key is told once, in the
    /// retired-key family, never as unknown.
    #[test]
    fn a_config_that_still_sets_the_band_or_the_chime_loads_and_is_told_once() {
        let others = "theme = \"Nord\"\n[presence]\nrim = false\n";
        let source =
            "choice_sound = false\ntheme = \"Nord\"\n[presence]\nband = false\nrim = false\n";
        let loaded = Config::parse(source).expect("retired keys never fail the file");
        let baseline = Config::parse(others).expect("the other settings parse alone");
        assert!(
            loaded == baseline,
            "every other setting loads as without them"
        );
        assert!(
            !loaded.presence_rim_enabled(),
            "the needs switch beside them stands"
        );
        let warnings = crate::native_config_language::key_warnings(source);
        assert!(warnings.ignored.is_empty(), "{:?}", warnings.ignored);
        assert_eq!(warnings.retired.len(), 2, "{:?}", warnings.retired);
        assert!(
            super::unaccepted_value_notices(source, &[]).is_empty(),
            "a retired key's value is never a value its reader rejects"
        );
    }

    #[test]
    fn tab_status_policy_clamps_both_ends() {
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
}

#[cfg(test)]
mod cfg_engine_tests {
    use super::{
        Config, KittySpriteAsset, MAX_KITTY_SPRITE_FILE_BYTES, MAX_USER_THEME_FILE_BYTES,
        ThemeCatalog, ThemeCatalogLimits, ThemeEntryStatus, ThemeScanCaps,
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

    /// The production bounds' law at a folder size a test can lay down: a scan
    /// past `files` keeps the lowest names (`ThemeScanCaps::PRODUCTION` is the
    /// same code with larger numbers).
    const SMALL_FILE_CAP: ThemeScanCaps = ThemeScanCaps {
        files: 4,
        ..ThemeScanCaps::PRODUCTION
    };

    // The small-cap tests prove the laws; the production bounds must keep the
    // order those laws need: more candidates than files read, so a name that
    // never reaches a read cannot push a valid theme out, and no more
    // candidates than entries inspected.
    const _: () = assert!(
        ThemeScanCaps::PRODUCTION.files < ThemeScanCaps::PRODUCTION.candidates
            && ThemeScanCaps::PRODUCTION.candidates <= ThemeScanCaps::PRODUCTION.entries
    );

    fn discover_capped(dir: &std::path::Path, caps: ThemeScanCaps) -> ThemeCatalog {
        ThemeCatalog::discover_in_capped(dir, false, caps).unwrap_or_default()
    }

    #[test]
    fn theme_catalog_is_sorted_bounded_safe_and_parse_validated() {
        let dir = theme_fixture_dir("bounded");
        let cap = SMALL_FILE_CAP.files;
        for index in (0..(cap + 2)).rev() {
            write_theme(
                &dir.join(format!("Theme-{index:03}.conf")),
                "#ddeeff",
                "#102030",
            );
        }
        let catalog = discover_capped(&dir, SMALL_FILE_CAP);
        let names = catalog.ready_names().collect::<Vec<_>>();
        assert_eq!(names.len(), cap);
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(names[0], "Theme-000");
        assert_eq!(names[cap - 1], format!("Theme-{:03}", cap - 1));
        assert!(catalog.truncated());
        assert_ne!(catalog.fingerprint(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A user file's `name =` is its display name while the file stem stays
    /// the id `resolve()` and `meta()` key on; description and author ride
    /// along, an omitted `appearance` is measured from the background, and a
    /// defect is a warning, not a refusal.
    #[test]
    fn theme_catalog_meta_shows_name_and_keys_on_the_stem() {
        use aterm_types::Appearance;
        use aterm_types::scheme::{AppearanceSource, ThemeWarning};
        let dir = theme_fixture_dir("meta");
        std::fs::write(
            dir.join("studio.conf"),
            "name = Studio Night\ndescription = deep navy\nauthor = Jo\n\
             foreground = #c0caf5\nbackground = #1a1b26\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("paper.conf"),
            "foreground = #202020\nbackground = #fafafa\ncolor7 = #fafafa\n",
        )
        .unwrap();
        let catalog = ThemeCatalog::discover_in(&dir);
        assert_eq!(
            catalog.ready_names().collect::<Vec<_>>(),
            ["paper", "studio"]
        );

        let studio = catalog.meta("STUDIO").expect("the stem is the id");
        assert_eq!(studio.display_name, "Studio Night");
        assert_eq!(studio.description.as_deref(), Some("deep navy"));
        assert_eq!(studio.author.as_deref(), Some("Jo"));
        assert_eq!(studio.appearance, Appearance::Dark);
        assert_eq!(studio.appearance_source, AppearanceSource::Measured);
        assert!(studio.warnings.is_empty(), "{:?}", studio.warnings);
        assert!(catalog.resolve("studio").is_ok());
        // Negative control: the display name is not an id.
        assert!(catalog.resolve("Studio Night").is_err());
        assert_eq!(catalog.meta("Studio Night"), None);

        // No `appearance` and a light background: Light, not a silent Dark,
        // in the meta and in the scheme the renderer gets.
        let paper = catalog.meta("paper").expect("paper loaded");
        assert_eq!(paper.display_name, "paper");
        assert_eq!(paper.appearance, Appearance::Light);
        assert_eq!(
            catalog.resolve("paper").unwrap().appearance,
            Appearance::Light
        );
        assert_eq!(
            paper.warnings,
            vec![ThemeWarning::AnsiMatchesBackground { slot: 7 }]
        );
        assert_eq!(
            catalog
                .ready_themes()
                .map(|(id, meta)| (id, meta.display_name.as_str()))
                .collect::<Vec<_>>(),
            [("paper", "paper"), ("studio", "Studio Night")]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Built-in law: every built-in's declared appearance is what the one
    /// dark/light classifier measures its background as, and its meta is its
    /// identity row (the catalog answers built-ins first).
    #[test]
    fn theme_catalog_builtin_meta_matches_the_classifier() {
        use super::measured_appearance;
        let catalog = ThemeCatalog::empty();
        for theme in aterm_types::scheme::builtin_theme_list() {
            let scheme = aterm_types::scheme::builtin(theme.name).expect("listed");
            assert_eq!(
                measured_appearance(scheme.background),
                theme.appearance,
                "{}",
                theme.name
            );
            let meta = catalog.meta(theme.name).expect("a built-in has meta");
            assert_eq!(meta.display_name, theme.name);
            assert_eq!(meta.pair.as_deref(), theme.pair);
            assert_eq!(meta.description.as_deref(), Some(theme.description));
        }
        assert_eq!(catalog.meta("no such theme"), None);
    }

    /// A file named like a built-in (any ASCII case, `Default` included)
    /// gets a Shadowed row naming the built-in, never reaches the picker, and
    /// cannot change what the name resolves to. Negative control: a file with
    /// its own name is Ready.
    #[test]
    fn theme_catalog_lists_a_file_shadowing_a_builtin() {
        for (file, row, builtin) in [
            ("Nord", "Nord", "Nord"),
            ("nord", "nord", "Nord"),
            ("default", "default", "Default"),
        ] {
            let dir = theme_fixture_dir("shadow");
            write_theme(&dir.join(format!("{file}.conf")), "#010203", "#fefdfc");
            write_theme(&dir.join("Work.conf"), "#ddeeff", "#102030");
            let catalog = ThemeCatalog::discover_in(&dir);
            let listing = catalog.listing().collect::<Vec<_>>();
            assert!(
                listing.contains(&(row, ThemeEntryStatus::Shadowed(builtin))),
                "{file}: {listing:?}"
            );
            assert!(listing.contains(&("Work", ThemeEntryStatus::Ready)));
            assert_eq!(catalog.ready_names().collect::<Vec<_>>(), ["Work"]);
            assert_eq!(
                catalog.resolve(file).unwrap(),
                aterm_types::scheme::builtin(builtin).unwrap(),
                "the built-in still wins for {file}"
            );
            assert!(
                ThemeEntryStatus::Shadowed(builtin)
                    .reason()
                    .is_some_and(|reason| reason.contains(builtin))
            );
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Both `Nord.conf` and `nord.conf` shadow the same built-in; on a
    /// case-sensitive volume each keeps its own row instead of collapsing
    /// into a case-collision sentinel, and `Nord` still resolves to the
    /// built-in.
    #[test]
    fn theme_catalog_keeps_a_row_per_case_variant_shadowing_a_builtin() {
        let dir = theme_fixture_dir("shadow-twins");
        write_theme(&dir.join("Nord.conf"), "#010203", "#fefdfc");
        write_theme(&dir.join("nord.conf"), "#030201", "#fcfdfe");
        let files = std::fs::read_dir(&dir).unwrap().count();
        let catalog = ThemeCatalog::discover_in(&dir);
        let listing = catalog.listing().collect::<Vec<_>>();
        assert_eq!(listing.len(), files, "{listing:?}");
        assert!(
            listing
                .iter()
                .all(|(_, status)| *status == ThemeEntryStatus::Shadowed("Nord")),
            "{listing:?}"
        );
        assert_eq!(
            catalog.resolve("Nord").unwrap(),
            aterm_types::scheme::builtin("Nord").unwrap()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The file cap counts files that are read, so files filtered out by
    /// name (shadowing, unsafe) cannot push valid themes out. Negative
    /// control: one more readable file trips the cap and marks truncation.
    #[test]
    fn theme_catalog_caps_files_after_the_name_filters() {
        let dir = theme_fixture_dir("cap-after-filter");
        let cap = SMALL_FILE_CAP.files;
        write_theme(&dir.join("Nord.conf"), "#010203", "#fefdfc");
        write_theme(&dir.join("Dracula.conf"), "#010203", "#fefdfc");
        for index in 0..cap {
            write_theme(
                &dir.join(format!("Theme-{index:03}.conf")),
                "#ddeeff",
                "#102030",
            );
        }
        let catalog = discover_capped(&dir, SMALL_FILE_CAP);
        assert_eq!(catalog.ready_names().count(), cap);
        assert!(!catalog.truncated());
        assert_eq!(
            catalog.limits(),
            ThemeCatalogLimits {
                seen: cap + 2,
                kept: cap,
            }
        );
        assert_eq!(
            catalog
                .listing()
                .filter(|(_, status)| matches!(status, ThemeEntryStatus::Shadowed(_)))
                .count(),
            2
        );

        write_theme(&dir.join("Theme-999.conf"), "#ddeeff", "#102030");
        let over = discover_capped(&dir, SMALL_FILE_CAP);
        assert_eq!(over.ready_names().count(), cap);
        assert!(over.truncated());
        assert!(over.resolve("Theme-999").is_err());
        assert_eq!(over.limits().kept, cap);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A folder past the directory-entry cap keeps the bounded prefix it
    /// gathered instead of dropping every theme.
    #[test]
    fn theme_catalog_past_the_entry_cap_keeps_a_prefix() {
        // Below the file cap, so only the entry cap can truncate.
        let caps = ThemeScanCaps {
            entries: 8,
            ..ThemeScanCaps::PRODUCTION
        };
        let dir = theme_fixture_dir("entry-cap");
        for index in 0..=caps.entries {
            std::fs::write(
                dir.join(format!("Theme-{index:04}.conf")),
                "foreground = #ddeeff\nbackground = #102030\n",
            )
            .unwrap();
        }
        let catalog = discover_capped(&dir, caps);
        assert!(catalog.truncated());
        let ready = catalog.ready_names().collect::<Vec<_>>();
        assert_eq!(ready.len(), caps.entries);
        assert!(catalog.resolve(ready[0]).is_ok());
        assert_eq!(catalog.limits().seen, caps.entries);
        let live = ThemeCatalog::discover_in_capped(&dir, true, caps)
            .expect("a truncated scan is not a failure");
        assert_eq!(live.ready_names().count(), caps.entries);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file whose stem is not UTF-8 gets an Unreadable row under its lossy
    /// name; no config value can resolve to it. (macOS volumes refuse such
    /// names, so this runs where the filesystem admits them.)
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn theme_catalog_lists_a_non_utf8_file_name() {
        use std::os::unix::ffi::OsStrExt;
        let dir = theme_fixture_dir("non-utf8");
        let path = dir.join(std::ffi::OsStr::from_bytes(b"Caf\xff.conf"));
        write_theme(&path, "#ddeeff", "#102030");
        let catalog = ThemeCatalog::discover_in(&dir);
        let listing = catalog.listing().collect::<Vec<_>>();
        assert_eq!(listing, [("Caf\u{FFFD}", ThemeEntryStatus::Unreadable)]);
        assert!(catalog.ready_names().next().is_none());
        assert!(catalog.resolve("Caf\u{FFFD}").is_err());
        assert_eq!(catalog.limits().kept, 0);
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
            // The link gets a row naming why, without being opened.
            let alias = catalog.listing().find(|(name, _)| *name == "Alias");
            assert!(
                matches!(alias, Some((_, ThemeEntryStatus::Invalid(why))) if why.contains("symlink")),
                "{alias:?}"
            );
            assert!(
                ThemeCatalog::try_discover_in(&dir).is_ok(),
                "a linked theme never blocks a live reload"
            );
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

    /// Every hostile sprite source resolves `Invalid` at once: missing, past the
    /// encoded-byte cap, past the dimension cap, a writerless FIFO (which must
    /// not park config admission) and a final-component symlink.
    #[test]
    fn kitty_asset_hostile_sources_fail_closed() {
        let invalid = |path: &std::path::Path, case: &str| {
            assert!(
                matches!(
                    config_with_kitty(path).resolve_asset_catalog().kitty_sprite,
                    KittySpriteAsset::Invalid { .. }
                ),
                "{case}"
            );
        };
        invalid(&kitty_fixture("missing"), "missing");

        let oversized = kitty_fixture("encoded-limit");
        std::fs::File::create(&oversized)
            .unwrap()
            .set_len((MAX_KITTY_SPRITE_FILE_BYTES as u64) + 1)
            .unwrap();
        invalid(&oversized, "encoded bytes past the cap");

        let wide = kitty_fixture("dimension-limit");
        let width = super::MAX_KITTY_SPRITE_DIMENSION + 1;
        let png = crate::app_introspect::encode_rgba8_png(&vec![0x7f; width * 4], width as u32, 1)
            .unwrap();
        std::fs::write(&wide, png).unwrap();
        invalid(&wide, "dimension past the cap");
        let fixtures = vec![oversized, wide];

        #[cfg(unix)]
        let fixtures = {
            use std::os::unix::ffi::OsStrExt as _;
            let fifo = kitty_fixture("writerless-fifo");
            let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path");
            // SAFETY: `fifo_c` is a live NUL-terminated pathname and mkfifo retains
            // no pointer. `kitty_fixture` gives this test a unique final component.
            assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
            invalid(&fifo, "writerless FIFO");
            let target = kitty_fixture("symlink-target");
            let linked = kitty_fixture("symlink-final");
            std::fs::write(&target, b"not consulted").expect("write symlink target");
            std::os::unix::fs::symlink(&target, &linked).expect("create final symlink");
            invalid(&linked, "final-component symlink");
            let mut fixtures = fixtures;
            fixtures.extend([fifo, linked, target]);
            fixtures
        };
        for path in fixtures {
            let _ = std::fs::remove_file(path);
        }
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
        working.styled_paths[0] = Some(aterm_render::FaceRef::new("/admitted/working-bold.ttf", 0));
        next.preserve_rejected_from(&config, &working);

        assert_eq!(next.styled_paths[0], working.styled_paths[0]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("font_family_bold"), "{warnings:?}");
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
        let _ = std::fs::remove_dir_all(root);
    }

    /// An ignored `font_px` (out of range or non-finite) yields a banner line
    /// and the resolver falls back; unset or admissible sizes
    /// are silent. Matches `--validate-config`.
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
        for bad in [0.0, 500.0, -5.0, f32::NAN, f32::INFINITY] {
            assert!(
                super::font_px_load_notice(&with(Some(bad))).is_some(),
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
    }

    /// Engine keys resolve trimmed and ASCII-case-insensitive (registered
    /// aliases included) onto the `TerminalConfig` field each names; keys with
    /// no engine side project exactly what an empty config does.
    #[test]
    fn engine_keys_resolve_onto_terminal_config() {
        use aterm_core::config::TerminalConfig;
        use aterm_core::terminal::CursorStyle;
        type Check = fn(&TerminalConfig) -> bool;
        let cases: [(&str, Check); 10] = [
            ("cursor_style = \" BAR \"", |t| {
                t.cursor_style == CursorStyle::BlinkingBar
            }),
            ("cursor_style = \"Beam\"\ncursor_blink = false", |t| {
                t.cursor_style == CursorStyle::SteadyBar
            }),
            ("bidi = \"disabled\"", |t| t.bidi.mode == BiDiMode::Disabled),
            ("bidi = \" Explicit \"", |t| {
                t.bidi.mode == BiDiMode::Explicit
            }),
            ("bidi = \" ON \"", |t| t.bidi.mode == BiDiMode::Implicit),
            ("ambiguous_width = \" wide \"", |t| t.ambiguous_width_double),
            ("ambiguous_width = \" DOUBLE \"", |t| {
                t.ambiguous_width_double
            }),
            ("ambiguous_width = \" NARROW \"", |t| {
                !t.ambiguous_width_double
            }),
            ("bold_is_bright = false", |t| !t.bold_is_bright),
            ("faint_opacity = 0.25", |t| t.faint_opacity == 0.25),
        ];
        for (source, check) in cases {
            let tc = cfg(source).applied_terminal_config();
            assert!(check(&tc), "{source}");
        }
        assert_eq!(
            cfg("font_px = 14.0\nline_height = 1.2").applied_terminal_config(),
            Config::default().applied_terminal_config()
        );
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
        // One list entry may carry several space-separated tags; entries that
        // name no tag build no (empty) feature set.
        let grouped = cfg("font_features = [\"+ss01 +zero\"]").text_shaping();
        assert_eq!(grouped.font_features[0].features.len(), 2);
        let empty = cfg("font_features = [\"\", \"+\", \"-\"]").text_shaping();
        assert!(empty.font_features.is_empty(), "{:?}", empty.font_features);
    }

    /// Three escape-sequence permissions are off unless written down: an
    /// unrelated engine key cannot enable one, and each opt-in reaches the engine.
    /// The palette is the one default-on permission.
    #[test]
    fn security_flags_are_opt_in_and_fail_closed() {
        let flags = |source: &str| {
            let tc = cfg(source).applied_terminal_config();
            [
                tc.allow_osc52_query,
                tc.allow_window_ops,
                tc.allow_notifications,
                tc.allow_palette_reconfigure,
            ]
        };
        assert_eq!(
            flags("scrollback_lines = 5000"),
            [false, false, false, true],
            "palette sets are allowed by default"
        );
        assert_eq!(
            flags(
                "allow_osc52_query = true\nallow_window_ops = true\n\
                 allow_notifications = true\nallow_palette_reconfigure = true"
            ),
            [true; 4]
        );
    }

    #[test]
    fn palette_sets_refused_only_when_the_owner_says_false() {
        assert!(
            !cfg("allow_palette_reconfigure = false")
                .applied_terminal_config()
                .allow_palette_reconfigure
        );
        assert!(
            cfg("allow_palette_reconfigure = true")
                .applied_terminal_config()
                .allow_palette_reconfigure
        );
        // The engine library itself stays fail-closed; only the app flips it.
        assert!(!aterm_core::config::TerminalConfig::default().allow_palette_reconfigure);
    }

    /// An absent table still decorates (the alternate screen included: the v1
    /// hardcoded suppression left Claude Code, lazygit and every full-screen
    /// TUI sparkle-less); `suppress_in_alt_screen = true` opts back out and
    /// `enabled = false` turns the whole feature off.
    #[test]
    fn sparkle_absent_table_decorates_the_alt_screen_and_master_off_disables() {
        let deco = Config::default()
            .sparkle_deco_config()
            .expect("an absent table resolves the defaults");
        assert!(!deco.suppress_in_alt_screen);
        assert!(
            cfg("[sparkle_words]\nsuppress_in_alt_screen = true")
                .sparkle_deco_config()
                .unwrap()
                .suppress_in_alt_screen
        );
        assert!(
            cfg("[sparkle_words]\nenabled = false")
                .sparkle_deco_config()
                .is_none()
        );
    }

    /// Style tokens match trimmed and ASCII-case-insensitively, like the web
    /// setters; an unknown token falls back to the default style, and
    /// `supernova_chance` clamps to 0..=100.
    #[test]
    fn sparkle_style_tokens_resolve_trimmed_any_case_with_fallback() {
        use crate::word_decorations::{FelineStyle, ProfanityStyle};
        for (style, expected) in [
            (" Nova ", ProfanityStyle::Nova),
            (" SPARKLE ", ProfanityStyle::Sparkle),
            ("rainbow", ProfanityStyle::Rainbow),
            ("comet", ProfanityStyle::Rainbow),
        ] {
            let deco = cfg(&format!("[sparkle_words.profanity]\nstyle = \"{style}\""))
                .sparkle_deco_config()
                .unwrap();
            assert_eq!(deco.profanity_style, expected, "{style:?}");
        }
        for (style, expected) in [(" Paw ", FelineStyle::Paw), ("lion", FelineStyle::Cat)] {
            let deco = cfg(&format!("[sparkle_words.feline]\nstyle = \"{style}\""))
                .sparkle_deco_config()
                .unwrap();
            assert_eq!(deco.feline_style, expected, "{style:?}");
        }
        let deco = cfg("[sparkle_words.profanity]\nsupernova_chance = 250")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(deco.supernova_chance, 100);
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
        // Without customs, ink off still gates emphasis off.
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
                font_refusals: Vec::new(),
                reason: crate::app_colorscheme::ThemeChange::ConfigReload,
            },
        );
        assert_eq!(
            super::path_feed_fingerprint_count(),
            0,
            "the event-loop reducer must consume the worker's immutable fingerprint"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Write `text` as `dir/aterm.toml` and apply it as the config worker's
    /// prepared generation, the way a live edit reaches the event loop.
    fn apply_config_text(app: &mut crate::App, dir: &std::path::Path, text: &str) {
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
                font_refusals: Vec::new(),
                reason: crate::app_colorscheme::ThemeChange::ConfigReload,
            },
        );
    }

    /// A typo added by a live edit is reported like one present at launch, even
    /// though the parsed `Config` is equal and the reload takes the dedupe path.
    ///
    /// The empty case is published too, so fixing the typo clears its row; only
    /// rows this path re-derives are replaced (an unrelated chord row stays).
    #[test]
    fn a_misspelled_key_added_by_a_live_edit_still_warns_through_the_dedupe() {
        let dir = std::env::temp_dir().join(format!("aterm-typo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut app = crate::App::headless_for_test();

        // A key this build does not know, and NOTHING else — so the parse is
        // equal to what is already applied and the dedupe fires.
        apply_config_text(&mut app, &dir, "windw_padding = 20\n");
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

        // A row from a family this path never re-derives (the chord the last
        // full reload posted) stands in for every parse-derived row.
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
        apply_config_text(&mut app, &dir, "# fixed\n");
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
        let mut app = crate::App::headless_for_test();
        apply_config_text(&mut app, &dir, &format!("theme = {NAME:?}\n"));
        let key = crate::message_reporters::ConfigFamily::UnacceptedValues.key();
        let row = app
            .messages
            .live_by_key(key)
            .expect("a theme that does not load is on the band");
        assert!(
            row.msg.detail.iter().any(|line| line.contains(NAME)),
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
        // Emphasis opts out on its own; ink stays on for profanity/feline.
        let deco = cfg("[sparkle_words.emphasis]\nenabled = false")
            .sparkle_deco_config()
            .unwrap();
        assert!(!deco.emphasis && deco.ink_enabled);
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
    use super::{Config, KnobChange, RenderKnobs, ThemeCatalog};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// The knobs `c` resolves to, against no theme files.
    fn knobs_of(c: &Config) -> RenderKnobs {
        RenderKnobs::from_config(
            c,
            &c.resolved_colors(
                aterm_types::Appearance::Dark,
                &ThemeCatalog::default(),
                None,
            ),
        )
    }

    /// (a) an out-of-range value for every clamped renderer/engine key
    /// resolves inside its range rather than failing the load.
    #[test]
    fn new_keys_parse_and_clamp() {
        let read = |key: &str, c: &Config| -> f64 {
            match key {
                "stem_gamma" => f64::from(c.stem_gamma_or_default()),
                "trail_sound_volume" => f64::from(c.trail_sound_volume()),
                "line_height" => f64::from(c.line_height_or_default()),
                "minimum_contrast" => f64::from(c.minimum_contrast_or_default()),
                "adjust_baseline" => f64::from(c.adjust_baseline_or_default()),
                "adjust_underline_position" => f64::from(c.adjust_underline_position_or_default()),
                "adjust_underline_thickness" => {
                    f64::from(c.adjust_underline_thickness_or_default())
                }
                "faint_opacity" => f64::from(c.faint_opacity_or_default()),
                "background_opacity" => f64::from(c.background_opacity_or_default()),
                other => panic!("no reader for {other}"),
            }
        };
        for (key, value, want) in [
            ("stem_gamma", "99.0", 3.0_f32),
            ("trail_sound_volume", "2.0", 1.0),
            ("trail_sound_volume", "nan", 0.0),
            ("line_height", "9.0", 2.0),
            ("line_height", "0.1", 0.8),
            ("line_height", "nan", super::DEFAULT_LINE_HEIGHT),
            ("minimum_contrast", "99.0", 21.0),
            ("adjust_baseline", "400", 32.0),
            ("adjust_underline_position", "400", 32.0),
            ("adjust_underline_thickness", "-400", -32.0),
            ("faint_opacity", "7.0", 1.0),
            ("background_opacity", "-3.0", 0.0),
        ] {
            assert_eq!(
                read(key, &cfg(&format!("{key} = {value}"))),
                f64::from(want),
                "{key} = {value}"
            );
        }
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
        use aterm_render::TextBlending;
        for (source, expected) in [
            ("text_blending = \"linear\"", TextBlending::Linear),
            (
                "text_blending = \"Linear_Corrected\"",
                TextBlending::LinearCorrected,
            ),
            ("text_blending = \"gamma\"", TextBlending::LinearCorrected),
        ] {
            assert_eq!(cfg(source).text_blending_or_default(), expected, "{source}");
        }
    }

    /// (a) `cursor_break_ligatures` maps to `LigatureMode::CursorDisabled`,
    /// with `ligatures = false` winning (everything is per-cell already), and
    /// `merged_ligatures` maps to `admit_collapsed` (default off).
    #[test]
    fn ligature_keys_map_to_shaping_modes() {
        use aterm_types::text_shaping::LigatureMode;
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
        // `merged_ligatures` admits the N:1 collapse independently of the mode.
        let merged = cfg("merged_ligatures = true\nligatures = false").text_shaping();
        assert!(merged.admit_collapsed);
        assert_eq!(merged.ligature_mode, LigatureMode::Disabled);
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
        assert_eq!(knobs_of(&c).selection_fg, Some(0x00AA_BBCC));
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
            let knobs = knobs_of(&cfg(toml));
            let changes = base.diff(&knobs);
            assert_eq!(
                changes,
                vec![expected],
                "{toml:?} must diff to exactly one renderer call"
            );
        }
        // And reverting diffs back to exactly one change per key too.
        let all = knobs_of(&cfg(
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
        // Default floor (1.0) + glass → both the opacity and the auto-floor fire.
        let glass = knobs_of(&cfg("background_opacity = 0.6"));
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
        let glass_hi = knobs_of(&cfg("background_opacity = 0.6\nminimum_contrast = 7.0"));
        let hi_base = knobs_of(&cfg("minimum_contrast = 7.0"));
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
}

/// [`Config::effective_minimum_contrast`] is the renderer's law evaluated on the
/// config values: over the opacity × configured-contrast lattice it equals
/// [`aterm_render::vibrancy::effective_minimum_contrast`] (the law itself is
/// proven in aterm-render's `tests/vibrancy_contrast.rs`). The default-glass
/// spotlight keeps the delegation non-vacuous: a default install under glass
/// floors to 4.5:1, not to its raw 1.0.
#[cfg(test)]
mod vibrancy_contrast_guarantee {
    use super::Config;
    use aterm_render::vibrancy::{CONTRAST_FLOOR, effective_minimum_contrast};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn effective_minimum_contrast_is_the_renderers_law() {
        let opacities: Vec<f32> = (0..=20).map(|i| i as f32 / 20.0).collect();
        let contrasts = [1.0_f32, 2.0, 3.0, 4.5, 4.6, 7.0, 21.0];
        for &o in &opacities {
            for &c in &contrasts {
                let config = cfg(&format!("background_opacity = {o}\nminimum_contrast = {c}"));
                let base = config.minimum_contrast_or_default();
                let opacity = config.background_opacity_or_default();
                assert_eq!(
                    config.effective_minimum_contrast().to_bits(),
                    effective_minimum_contrast(base, opacity).to_bits(),
                    "opacity={o} contrast={c}"
                );
            }
        }

        // Non-vacuity: a default install (minimum_contrast unset = 1.0) running
        // glass floors to 4.5, so the delegation is not the identity.
        let default_glass = cfg("background_opacity = 0.5");
        assert_eq!(default_glass.minimum_contrast_or_default(), 1.0);
        assert_eq!(default_glass.effective_minimum_contrast(), CONTRAST_FLOOR);
    }

    /// `minimum_contrast` unset: the renderer floors nothing, and the engine
    /// holds theme pairs to 4.5:1 on Windows only. Set to any value, `1.0`
    /// included, the renderer floors every cell at it and the theme-pair floor
    /// is off, so one key still says what is floored.
    #[test]
    fn an_unset_minimum_contrast_floors_theme_pairs_on_windows_only() {
        let unset = cfg("");
        assert_eq!(unset.effective_minimum_contrast(), 1.0);
        assert_eq!(
            unset.applied_terminal_config().indexed_minimum_contrast,
            if cfg!(windows) { 4.5 } else { 1.0 }
        );
        for set in ["minimum_contrast = 1.0", "minimum_contrast = 7.0"] {
            let config = cfg(set);
            assert_eq!(
                config.applied_terminal_config().indexed_minimum_contrast,
                1.0,
                "{set}"
            );
        }
    }
}

/// The enum-valued window keys share one parse law: trimmed, ASCII
/// case-insensitive, documented aliases accepted, and an absent, empty or
/// unknown token falls back to the key's default instead of failing the load.
#[cfg(test)]
mod config_token_tests {
    use super::{
        Config, RightClickGesture, TabBandHeight, TabMenuChord, WindowColorspace, WindowTheme,
    };

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// The tab-menu chord rows are the only way to give the keys back (the
    /// chord is not in `[keybindings]`): absent or unknown keeps Shift+F10
    /// (`kf22`) with the program, only `on`/`both` claims it, and `off`
    /// surrenders the Menu key too.
    #[test]
    fn enum_keys_parse_trimmed_any_case_with_aliases_and_fall_back() {
        let theme = |t: &str| cfg(t).window_theme_or_default();
        assert_eq!(theme("window_theme = \" Dark \""), WindowTheme::Dark);
        assert_eq!(theme("window_theme = \"LIGHT\""), WindowTheme::Light);
        assert_eq!(theme("window_theme = \"midnight\""), WindowTheme::Auto);
        assert_eq!(theme(""), WindowTheme::Auto);

        let band = |t: &str| cfg(t).tab_band_height_or_default();
        let standard = "tab_band_height = \"  STANDARD \"";
        assert_eq!(band(standard), TabBandHeight::Standard);
        assert_eq!(
            band("tab_band_height = \"compact\""),
            TabBandHeight::Compact
        );
        let unknown = "tab_band_height = \"huge\"";
        assert_eq!(band(unknown), TabBandHeight::PLATFORM_DEFAULT);

        let click = |t: &str| cfg(t).right_click_or_default();
        let copy_paste = "right_click = \" Copy-Paste \"";
        assert_eq!(click(copy_paste), RightClickGesture::CopyPaste);
        assert_eq!(
            click("right_click = \"menu\""),
            RightClickGesture::PLATFORM_DEFAULT
        );
        assert_eq!(click(""), RightClickGesture::PLATFORM_DEFAULT);

        let space = |t: &str| cfg(t).window_colorspace_or_default();
        let p3 = "window_colorspace = \" DisplayP3 \"";
        assert_eq!(space(p3), WindowColorspace::DisplayP3);
        assert_eq!(
            space("window_colorspace = \"P3\""),
            WindowColorspace::DisplayP3
        );
        let rec2020 = "window_colorspace = \"rec2020\"";
        assert_eq!(space(rec2020), WindowColorspace::Srgb);
        assert_eq!(space(""), WindowColorspace::Srgb);

        for (source, chord, menu_key, shift_f10) in [
            ("", TabMenuChord::MenuKey, true, false),
            (
                "tab_menu_chord = \"nope\"",
                TabMenuChord::MenuKey,
                true,
                false,
            ),
            (
                "tab_menu_chord = \" Menu-Key \"",
                TabMenuChord::MenuKey,
                true,
                false,
            ),
            ("tab_menu_chord = \"BOTH\"", TabMenuChord::On, true, true),
            ("tab_menu_chord = \"off\"", TabMenuChord::Off, false, false),
        ] {
            let got = cfg(source).tab_menu_chord_or_default();
            assert_eq!(got, chord, "{source}");
            assert_eq!(
                (got.claims_menu_key(), got.claims_shift_f10()),
                (menu_key, shift_f10),
                "{source}"
            );
        }
    }
}

/// [`DEFAULT_DECORATIVE_EFFECTS`]: a platform default that explicit keys always
/// override.
#[cfg(test)]
mod decorative_effect_default_tests {
    use super::{Config, DEFAULT_DECORATIVE_EFFECTS, default_cursor_trail_style};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// The cursor style and the typing glow follow [`DEFAULT_DECORATIVE_EFFECTS`];
    /// `cursor_trail_bloom` (inert with no trail drawn) and `[sparkle_words]
    /// enabled` (see [`Config::sparkle_words_enabled_or_default`]) do not.
    #[test]
    fn the_trail_master_is_platform_split_and_its_two_neighbours_are_not() {
        let fresh = Config::default();
        assert_eq!(
            fresh.cursor_trail_drawn(),
            DEFAULT_DECORATIVE_EFFECTS,
            "a fresh Windows config stands no resident cat on live output; \
             elsewhere the delight stays on"
        );
        assert_eq!(
            fresh.cursor_momentum_glow_or_default(),
            DEFAULT_DECORATIVE_EFFECTS
        );
        // A config that merely EXISTS is still a fresh config for this key.
        assert_eq!(
            cfg("font_px = 14.0").cursor_trail_drawn(),
            DEFAULT_DECORATIVE_EFFECTS
        );
        // The two neighbours stay on everywhere.
        assert!(fresh.cursor_trail_bloom_or_default());
        assert!(fresh.sparkle_words_enabled_or_default());
        // …and each still answers to its own key, in both directions, on every
        // platform: this is a DEFAULT, never a veto.
        assert!(cfg("cursor_trail_style = \"comet\"").cursor_trail_drawn());
        assert!(!cfg("cursor_trail_style = \"off\"").cursor_trail_drawn());
        assert!(!cfg("cursor_trail_bloom = false").cursor_trail_bloom_or_default());
        assert!(!cfg("[sparkle_words]\nenabled = false").sparkle_words_enabled_or_default());
    }

    /// THE RETIRED MASTER FOLDS EXACTLY: over every combination of the old
    /// switch, the style and the glow (each absent, on or off), the folded
    /// resolvers decide the drawn trail and the typing glow as the old
    /// formula did (`master && style draws`, `master && glow`, the master's
    /// absent default [`DEFAULT_DECORATIVE_EFFECTS`]) wherever the old switch
    /// decided, so an old file draws what it always drew. Where it never did
    /// (no switch written, and the platform default Off: Windows) a style or
    /// glow written alone is what the person asked for and draws; unset, both
    /// stay Off there. Negative control: a fold that ignores the old switch
    /// disagrees on some cell.
    #[test]
    fn the_retired_cursor_master_folds_into_its_successors() {
        let mut naive_disagrees = false;
        for master in [None, Some(true), Some(false)] {
            for style in [None, Some("off"), Some("fire")] {
                for glow in [None, Some(true), Some(false)] {
                    let mut toml = String::new();
                    if let Some(m) = master {
                        toml.push_str(&format!("cursor_trail = {m}\n"));
                    }
                    if let Some(s) = style {
                        toml.push_str(&format!("cursor_trail_style = \"{s}\"\n"));
                    }
                    if let Some(g) = glow {
                        toml.push_str(&format!("cursor_momentum_glow = {g}\n"));
                    }
                    let c = cfg(&toml);
                    let on = master.unwrap_or(DEFAULT_DECORATIVE_EFFECTS);
                    let old_style = style.unwrap_or(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE);
                    let old_drawn = on && old_style != "off";
                    let old_glow = on && glow.unwrap_or(true);
                    let (want_drawn, want_glow) = if master.is_some() || DEFAULT_DECORATIVE_EFFECTS
                    {
                        (old_drawn, old_glow)
                    } else {
                        (style.is_some_and(|s| s != "off"), glow.unwrap_or(false))
                    };
                    assert_eq!(c.cursor_trail_drawn(), want_drawn, "{toml}");
                    assert_eq!(c.cursor_momentum_glow_or_default(), want_glow, "{toml}");
                    let naive_drawn = style.unwrap_or(default_cursor_trail_style()).ne("off");
                    naive_disagrees |= naive_drawn != want_drawn;
                }
            }
        }
        assert!(
            naive_disagrees,
            "the matrix catches a fold that ignores the old switch"
        );
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

    /// A table that sets only other knobs keeps the platform default and an
    /// explicit `enabled` wins. Every number clamps into its band at both ends
    /// (the cell-counted ones on the engine's constants; `max_streaks = 0` is
    /// not a second off switch) and non-finite scalars fall toward less
    /// effect. Nothing rejects the file.
    #[test]
    fn output_streak_keys_resolve_clamp_and_fail_toward_less_effect() {
        assert_eq!(
            cfg("[output_streak]\ntail = 12").output_streak_enabled_or_default(),
            DEFAULT_DECORATIVE_EFFECTS
        );
        assert!(!cfg("[output_streak]\nenabled = false").output_streak_enabled_or_default());

        type Read = fn(&Config) -> f64;
        let intensity: Read = |c| f64::from(c.output_streak_intensity_or_default());
        let tail: Read = |c| f64::from(c.output_streak_tail_or_default());
        let streaks: Read = |c| f64::from(c.output_streak_max_streaks_or_default());
        let idle: Read = |c| f64::from(c.output_streak_idle_secs_or_default());
        for (key, read, value, expected) in [
            ("intensity", intensity, "4.0", 1.0),
            ("intensity", intensity, "-2.0", 0.0),
            ("intensity", intensity, "nan", 0.0),
            ("tail", tail, "4000", f64::from(TAIL_MAX)),
            ("tail", tail, "0", f64::from(TAIL_MIN)),
            ("max_streaks", streaks, "99", MAX_COMETS as f64),
            ("max_streaks", streaks, "0", 1.0),
            ("idle_secs", idle, "9000.0", 120.0),
            ("idle_secs", idle, "0.0", 2.0),
            ("idle_secs", idle, "nan", 2.0),
        ] {
            assert_eq!(
                read(&cfg(&format!("[output_streak]\n{key} = {value}"))),
                expected,
                "{key} = {value}"
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
            warning.contains("output_streak.sound"),
            "the message names the key: {warning}"
        );
    }

    /// Settings ▸ Harness words: a key that takes power away changes the
    /// summary and `Off` outranks every limit; a change that takes none away
    /// (an answer text, the question priority, the usage keys, `accounts =
    /// "early"`, a person's own trust roots) reads as the untouched table. The
    /// words never carry a TOML key or a `key: value`.
    #[test]
    fn the_harness_words_name_every_limit_and_only_limits() {
        let read = super::HarnessPolicy::read;
        let words = |text: &str| read(text).words();
        let automatic = words("");
        for neutral in [
            "[harness]\naccounts = \"early\"\n",
            "[harness]\nanswer_text = \"ask me\"\n",
            "[harness]\nquestion_priority = \"long-term\"\n",
            "[harness]\nlimit_reset = true\n",
            "[harness]\nlimit_reset = false\n",
            "[harness]\nusage_tap = true\n",
            "[harness]\nusage_tap = false\n",
            "[harness]\nusage_notes = true\n",
            "[harness]\nusage_notes = false\n",
            "[harness]\nusage_credits = true\n",
            "[harness]\nusage_credits = false\n",
        ] {
            assert_eq!(words(neutral), automatic, "{neutral}");
        }
        let safe = words("[harness]\napprove = \"safe\"\n");
        assert_ne!(safe, automatic);
        for own_roots in [
            "[harness]\napprove = \"safe\"\ntrust_roots = []\n",
            "[harness]\napprove = \"safe\"\ntrust_roots = [\"~/src/app*\"]\n",
        ] {
            assert_eq!(words(own_roots), safe, "{own_roots}");
        }
        let all = "[harness]\nheadless = false\napprove = \"none\"\ntrust_roots = []\n\
                   answer_questions = false\nanswer_text = \"ask me\"\n\
                   dismiss_surveys = false\ncontinue = false\ncontinue_text = \"go\"\n\
                   continue_per_hour = 2\nrules_file = \"/r\"\nretry_api_errors = false\n\
                   probe_api = false\nresume_limits = false\nlimit_wait = false\n\
                   accounts = \"off\"\nmodel_fallback = \"\"\n\
                   compact_on_context_wall = false\nrelaunch = false\nupgrade = false\n\
                   human_grace_s = 600\n";
        for named in [
            "[harness]\napprove = \"none\"\n",
            "[harness]\nanswer_questions = false\n",
            "[harness]\naccounts = \"wall\"\n",
            all,
        ] {
            let said = words(named);
            assert_ne!(said, automatic, "{named}");
            for key in aterm_agent::supervise::config::KEYS {
                assert!(
                    !key.contains('_') || !said.contains(key),
                    "{key} is a file's word, not the row's: {said}"
                );
            }
            assert!(!said.contains(": "), "no `key: value`: {said}");
        }
        let off = words(&format!("{all}enabled = false\n"));
        assert_eq!(
            off,
            words("[harness]\nenabled = false\n"),
            "Off outranks every limit"
        );
        assert_ne!(off, automatic);
        assert!(!super::HarnessPolicy::read("").policy.limit_reset);
        assert!(!super::HarnessPolicy::read("").policy.usage_notes);
        assert!(!super::HarnessPolicy::read("").policy.usage_credits);
        // Every limit set on a row of the Harness page is named by that row,
        // an agent's own switch and the default Autopilot too, in the table's
        // order with the agents last; only a key with no row is the file's.
        assert_eq!(
            read("[harness.claude]\nenabled = false\n").words(),
            "Automatic, limited by Supervise Claude Code sessions set to Off"
        );
        assert_eq!(
            read("[harness]\nautopilot = \"land\"\n[harness.codex]\nenabled = false\n").words(),
            "Automatic, limited by Default autopilot set to Land; Watch codex-alab sessions set \
             to Off"
        );
        assert_eq!(
            read("[harness]\nautopilot = \"off\"\nrelaunch = false\n").words(),
            "Automatic, limited by Default autopilot set to Off; 1 setting in aterm.toml"
        );
        assert_eq!(
            read("[harness]\nrelaunch = false\n").words(),
            "Automatic, limited by 1 setting in aterm.toml"
        );
        assert_eq!(read("[harness]\nrelaunch = false\n").file_limits(), 1);
        assert_eq!(read("[harness]\napprove = \"none\"\n").file_limits(), 0);
    }

    /// No row of the Harness page is blamed on the file: every value any
    /// `harness.*` row of the registry (`prefs::NESTED_LEAVES`, the rows the
    /// page draws) can take that takes power away is named by that row's label
    /// and counted as no file limit, so a row added there is classified with no
    /// code here. The master switch reads Off whole. Negative control:
    /// `relaunch`, a key with no row, is the file's.
    #[test]
    fn every_harness_rows_limit_is_named_by_its_row() {
        let read = super::HarnessPolicy::read;
        let mut limited = Vec::new();
        for leaf in crate::prefs::NESTED_LEAVES
            .iter()
            .filter(|leaf| leaf.key.starts_with("harness."))
            .filter(|leaf| leaf.key != crate::prefs::EDIT_HARNESS_ENABLED)
        {
            let (table, key) = leaf.key.rsplit_once('.').expect("a dotted key");
            let values: Vec<String> = match leaf.kind {
                crate::prefs::EditKind::Bool => vec!["false".to_string()],
                crate::prefs::EditKind::Enum { options } => options
                    .iter()
                    .map(|option| format!("\"{option}\""))
                    .collect(),
                // A free-text row takes no power: `land_at` only offers a time
                // (the Autopilot design §9.3.4), so no value it holds is a
                // limit — pinned on a sample. Another Text row is classified
                // here before it can pass.
                crate::prefs::EditKind::Text if leaf.key == crate::prefs::EDIT_HARNESS_LAND_AT => {
                    let policy = read("[harness]\nland_at = \"18:00\"\n");
                    assert!(
                        policy.policy.against_default().limits.is_empty(),
                        "land_at limits nothing: {}",
                        policy.words()
                    );
                    assert_eq!(policy.file_limits(), 0, "land_at is no file limit");
                    Vec::new()
                }
                kind => panic!("{}: a {kind:?} row", leaf.key),
            };
            for value in values {
                let source = format!("[{table}]\n{key} = {value}\n");
                let policy = read(&source);
                if policy.policy.against_default().limits.is_empty() {
                    continue;
                }
                let words = policy.words();
                assert!(words.contains(leaf.label), "{source}: {words}");
                assert!(!words.contains("aterm.toml"), "{source}: {words}");
                assert_eq!(policy.file_limits(), 0, "{source}");
                limited.push(leaf.key);
            }
        }
        for key in [
            crate::prefs::EDIT_HARNESS_CLAUDE_ENABLED,
            crate::prefs::EDIT_HARNESS_CODEX_ENABLED,
            crate::prefs::EDIT_HARNESS_AUTOPILOT,
            crate::prefs::EDIT_HARNESS_APPROVE,
            crate::prefs::EDIT_HARNESS_ANSWER_QUESTIONS,
        ] {
            assert!(limited.contains(&key), "{key} limits: {limited:?}");
        }
        assert_eq!(read("[harness]\nrelaunch = false\n").file_limits(), 1);
    }

    #[test]
    fn a_launch_that_could_not_load_the_file_still_reads_its_harness_lines() {
        let text = "font_px = \n[harness]\ncontinue = false\napprove = \"safe\"\n";
        assert!(Config::parse(text).is_err(), "the fixture is not a config");
        let stand_in = Config::stand_in(text);
        let p = &stand_in.harness.policy;
        assert!(p.enabled && !p.continue_policy);
        assert_eq!(p.approve, aterm_agent::supervise::config::Approve::Safe);
        assert!(!stand_in.harness.notes.is_empty());
        assert!(
            stand_in.font_px.is_none(),
            "every other setting at its default"
        );
        assert_eq!(
            Config::stand_in("font_px = \n").harness.policy,
            aterm_agent::supervise::SupervisorConfig::default()
        );
    }

    /// The GUI's side of `[harness]` (the reader itself, `SupervisorConfig::
    /// from_aterm_toml`, is pinned in `aterm-agent`): a reload that changes only
    /// a policy key is a changed `Config` (not deduped); an unreadable value is
    /// one `config harness.<key>:` notice while an unknown key is left to the
    /// ignored-key notice; the config language's domain check stands down for a
    /// value the reader already named (negative control: without the note it
    /// speaks); a `false` under another table is named.
    #[test]
    fn the_harness_table_is_the_supervisors_policy() {
        let cfg = |text: &str| Config::parse(text).expect("valid config");
        let text = "[harness]\ncontinue = false\n";
        assert!(cfg(text) != cfg("[harness]\ncontinue = true\n"));

        let bad = "[harness]\ncontinue = \"maybe\"\nheadless = false\ncontine = true\n";
        let notices = cfg(bad).harness_notices();
        assert_eq!(notices.len(), 1, "the unknown key is the config language's");
        assert!(
            notices[0].starts_with("config harness.continue:") && notices[0].contains("maybe"),
            "{notices:?}"
        );
        let ignored = crate::native_config_language::key_warnings(bad).ignored;
        assert!(ignored.iter().any(|l| l.contains("contine")), "{ignored:?}");

        let text = "[harness]\napprove = \"everything\"\n";
        let told = cfg(text).harness_notices();
        assert_eq!(told.len(), 1, "{told:?}");
        assert_eq!(
            super::unaccepted_value_notices(text, &told),
            Vec::<String>::new()
        );
        assert_eq!(super::unaccepted_value_notices(text, &[]).len(), 1);

        let misplaced = cfg("[packages]\nharness.enabled = false\n");
        assert!(!misplaced.harness.policy.enabled);
        assert!(
            misplaced.harness_notices()[0].contains("`packages.harness.enabled`"),
            "{:?}",
            misplaced.harness_notices()
        );
    }
}

/// Synthetic tab-band head arithmetic. The law is pure; the one host-dependent
/// fact (the shipped face's cell height) has its own Windows test.
#[cfg(test)]
mod tab_band_height_tests {
    #[cfg(windows)]
    use super::Config;
    use super::{TabBandHeight, synthetic_band_head_px};

    #[cfg(windows)]
    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// The cell a window builds for `config`, through the backend worker's own
    /// chain: the admitted family (`font_family_admission`), the resolved
    /// `font_px`, and the line height the worker pins
    /// (`line_height_or_default`). `(cell, baseline row, primary identity)`.
    #[cfg(windows)]
    fn window_cell(config: &Config) -> ((usize, usize), i32, String) {
        let face = Config::font_family_admission(config.font_family_request().as_deref())
            .expect("the family is admitted");
        let mut r = aterm_render::Renderer::from_system_with_face(
            face.as_ref(),
            super::resolve_font_px(config),
            aterm_render::Theme::default(),
        )
        .expect("a face");
        r.set_line_height(config.line_height_or_default());
        (
            r.cell_size(),
            r.baseline(),
            r.primary_source_path().unwrap_or_default().to_string(),
        )
    }

    /// THE WINDOWS DEFAULTS DRAW THE JUDGED CELL: an empty `aterm.toml` builds
    /// Iosevka Term Semibold at 17 px on the 1.1 line — 9x23, baseline row 17,
    /// the configuration two render farms chose on 2026-10-16 (render
    /// J2-lh11). The renderer asserts the same 9x23 cell from its side
    /// (`bundled::the_default_faces_own_powerline_glyphs_stand_inside_the_windows_cell`).
    #[cfg(windows)]
    #[test]
    fn the_windows_defaults_draw_the_judged_9x23_cell() {
        assert_eq!(crate::FONT_PX, 17.0);
        assert_eq!(super::DEFAULT_LINE_HEIGHT, 1.1);
        assert_eq!(
            window_cell(&Config::default()),
            (
                (9, 23),
                17,
                aterm_render::bundled::IOSEVKA_SEMIBOLD_ID.to_string()
            )
        );
    }

    /// THE OLD LOOK IN THREE LINES: `font_family = "Cascadia Mono PL"`,
    /// `font_px = 16`, `line_height = 1.0` give back the Windows default
    /// before 2026-10-16 exactly — the bundled Cascadia Mono PL's 9x21 cell on
    /// baseline row 15. (An installed Cascadia Mono PL would win the name, by
    /// design, and carries no letter floor.)
    #[cfg(windows)]
    #[test]
    fn three_lines_restore_the_cascadia_9x21_cell() {
        let family = aterm_render::bundled::CASCADIA_FAMILY;
        if aterm_render::resolve_font_family(family, aterm_render::FaceTarget::REGULAR).is_some() {
            aterm_spec::host::could_not_run(format!(
                "{family} is installed on this host, and an installed face wins the name"
            ));
        }
        let old = cfg("font_family = \"Cascadia Mono PL\"\nfont_px = 16\nline_height = 1.0\n");
        assert_eq!(
            window_cell(&old),
            ((9, 21), 15, aterm_render::bundled::CASCADIA_ID.to_string())
        );
        // Each line matters: without the line height the cell keeps the 1.1
        // line, without the size it keeps 17 px.
        let lh = cfg("font_family = \"Cascadia Mono PL\"\nfont_px = 16\n");
        assert_ne!(window_cell(&lh).0, (9, 21));
        let px = cfg("font_family = \"Cascadia Mono PL\"\nline_height = 1.0\n");
        assert_ne!(window_cell(&px).0, (9, 21));
    }

    /// `synthetic_band_head_px(target, pad_top, strip_rows, cell_h, scale)`:
    /// `head + pad_top + strip_rows·cell_h` lands on the target band (the
    /// residue is re-derived, not scaled, at 150%); no strip, a strip that
    /// already reaches the target, or a degenerate target yields no head (the
    /// guard must not be `target <= 0.0`, which is false for NaN); an
    /// oversized target, +inf included, caps at `SYNTHETIC_BAND_HEAD_CELL_CAP`
    /// cells; a degenerate scale reads as 1.0.
    #[test]
    fn synthetic_band_head_law() {
        let target = TabBandHeight::Standard.target_logical_px();
        let band = target as usize;
        let band_150 = (f64::from(target) * 1.5).round() as usize;
        let cap = 10 * super::SYNTHETIC_BAND_HEAD_CELL_CAP;
        for (case, (t, pad_top, rows, cell_h, scale), expected) in [
            ("no strip", (target, 2, 0, 21, 1.0), 0),
            (
                "96 dpi totals the target",
                (target, 2, 1, 21, 1.0),
                band - 2 - 21,
            ),
            (
                "150% re-derives",
                (target, 3, 1, 28, 1.5),
                band_150 - 3 - 28,
            ),
            (
                "one row absorbs the target",
                (target, 2, 1, band - 2, 1.0),
                0,
            ),
            ("two rows absorb it", (target, 2, 2, 21, 1.0), 0),
            ("oversized target caps", (4000.0, 0, 1, 10, 1.0), cap),
            ("+inf target caps", (f32::INFINITY, 0, 1, 10, 1.0), cap),
            ("NaN target", (f32::NAN, 2, 1, 21, 1.0), 0),
            ("zero target", (0.0, 2, 1, 21, 1.0), 0),
            ("negative target", (-32.0, 2, 1, 21, 1.0), 0),
        ] {
            assert_eq!(
                synthetic_band_head_px(t, pad_top, rows, cell_h, scale),
                expected,
                "{case}"
            );
        }
        let at_1x = synthetic_band_head_px(32.0, 2, 1, 21, 1.0);
        assert_ne!(at_1x, 0, "control: a finite positive target has a head");
        for scale in [f64::NAN, f64::INFINITY, 0.0, -2.0] {
            assert_eq!(
                synthetic_band_head_px(32.0, 2, 1, 21, scale),
                at_1x,
                "scale {scale}"
            );
        }
    }

    /// Windows only: with the shipped face at the Windows `FONT_PX` and the
    /// default line height (Iosevka Term Semibold, 17 px, 1.1: a 23 px row,
    /// 36 px at 150%), the standard band reaches the target, or the head
    /// collapses to 0 if the face is unusually tall (never overshooting).
    #[cfg(windows)]
    #[test]
    fn the_shipped_windows_face_reaches_the_standard_band() {
        let Some(renderer) =
            aterm_render::Renderer::from_system(crate::FONT_PX, aterm_render::Theme::default())
        else {
            aterm_spec::host::could_not_run("no system monospace font here");
        };
        let target = TabBandHeight::Standard.target_logical_px();
        let pad_logical = Config::default().window_padding_top_or_default();
        let mut renderer = renderer;
        renderer.set_line_height(Config::default().line_height_or_default());
        // Both DPI regimes a Windows laptop actually runs at. `font_px_for_scale`
        // is the SAME derivation attach applies, so the 150% row is the real one.
        for scale in [1.0_f64, 1.5] {
            let px = crate::font_px_for_scale(crate::FONT_PX, scale);
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
mod window_padding_tests {
    use super::Config;

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// `window_padding` clamps to 0..=64 and maps non-finite values to the
    /// default; `window_padding_top` clamps to `0..=window_padding`, so even
    /// its 2.0 default is capped by a smaller base pad.
    #[test]
    fn window_padding_clamps_and_rejects_non_finite() {
        type Read = fn(&Config) -> f32;
        let base: Read = Config::window_padding_or_default;
        let top: Read = Config::window_padding_top_or_default;
        for (source, read, expected) in [
            ("window_padding = 20.0", base, 20.0),
            ("window_padding = -3.0", base, 0.0),
            ("window_padding = 500.0", base, super::MAX_WINDOW_PADDING_PX),
            ("window_padding = nan", base, crate::PAD_LOGICAL_PX),
            ("window_padding_top = 6.0", top, 6.0),
            ("window_padding = 4.0\nwindow_padding_top = 9.0", top, 4.0),
            ("window_padding = 1.0", top, 1.0),
            ("window_padding_top = -2.0", top, 0.0),
            ("window_padding_top = inf", top, crate::PAD_TOP_LOGICAL_PX),
        ] {
            assert_eq!(read(&cfg(source)), expected, "{source}");
        }
    }
}

#[cfg(test)]
mod headless_boot_metrics_tests {
    #[test]
    fn asymmetric_vertical_padding_round_trips_resize_with_remainder() {
        use aterm_window::dpi::PhysicalSize;

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
}

#[cfg(test)]
mod split_theme_tests {
    use super::{Config, ThemeCatalog, theme_misses, theme_selector_notice};
    use aterm_types::Appearance;

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    /// `theme` resolves per appearance: a plain name (multi-word included)
    /// serves both; `dark:`/`light:` keys (any case, padded) pick their side
    /// and keep the name's case. (An omitted side: the two tests below.)
    #[test]
    fn theme_name_resolves_per_appearance() {
        for (source, dark, light) in [
            (
                r#"theme = "Tokyo Night""#,
                Some("Tokyo Night"),
                Some("Tokyo Night"),
            ),
            (
                r#"theme = "dark:Dracula,light:GitHub Light""#,
                Some("Dracula"),
                Some("GitHub Light"),
            ),
            (
                r#"theme = " DARK : Solarized Dark , Light : Solarized Light ""#,
                Some("Solarized Dark"),
                Some("Solarized Light"),
            ),
        ] {
            let c = cfg(source);
            assert_eq!(
                c.resolve_theme_name(Appearance::Dark).as_deref(),
                dark,
                "{source}"
            );
            assert_eq!(
                c.resolve_theme_name(Appearance::Light).as_deref(),
                light,
                "{source}"
            );
        }
        // End to end: each side renders the scheme it names.
        let split = cfg(r#"theme = "dark:Dracula,light:GitHub Light""#);
        assert_eq!(
            split.theme_for(Appearance::Dark).bg,
            cfg(r#"theme = "Dracula""#).theme_for(Appearance::Dark).bg
        );
        assert_eq!(split.theme_for(Appearance::Light).bg, 0x00FF_FFFF);
    }

    /// A split that omits one side, naming a theme with no built-in pair, draws
    /// the named side for both appearances rather than a Default the user never
    /// chose.
    #[test]
    fn split_omitted_side_draws_the_named_side() {
        let c = cfg(r#"theme = "light:GitHub Light""#);
        for appearance in [Appearance::Dark, Appearance::Light] {
            assert_eq!(
                c.resolve_theme_name(appearance).as_deref(),
                Some("GitHub Light")
            );
        }
        assert_eq!(c.theme_for(Appearance::Dark).bg, 0x00FF_FFFF);
        // A clean one-sided split is not a selector issue.
        assert!(theme_misses(&c, &ThemeCatalog::empty()).is_empty());
    }

    /// A split that omits a side draws the named side's built-in pair first:
    /// `dark:Gruvbox Dark` on a light OS is Gruvbox Light, end to end. A user
    /// theme has no pair, so it serves both sides (negative control).
    #[test]
    fn split_omitted_side_draws_the_builtin_pair() {
        let c = cfg(r#"theme = "dark:Gruvbox Dark""#);
        assert_eq!(
            c.resolve_theme_name(Appearance::Light).as_deref(),
            Some("Gruvbox Light")
        );
        assert_eq!(
            c.theme_for(Appearance::Light),
            cfg(r#"theme = "Gruvbox Light""#).theme_for(Appearance::Light)
        );
        assert_ne!(
            c.theme_for(Appearance::Light),
            c.theme_for(Appearance::Dark)
        );
        let user = cfg(r#"theme = "dark:Studio""#);
        assert_eq!(
            user.resolve_theme_name(Appearance::Light).as_deref(),
            Some("Studio")
        );
        // The notice for a split with an unusable side names what that side
        // actually draws: the pair, not the named side.
        let notice = theme_selector_notice(Some("dark:Gruvbox Dark,lite:x")).expect("an issue");
        assert!(
            notice.ends_with(r#"light mode uses "Gruvbox Light""#),
            "{notice}"
        );
    }

    /// Selector issues reach the band through `theme_misses`, alongside catalog
    /// misses; a clean selector adds none (negative control).
    #[test]
    fn theme_misses_include_selector_issues() {
        let themes = ThemeCatalog::empty();
        let misses = |theme: &str| {
            theme_misses(
                &Config {
                    theme: Some(theme.to_string()),
                    ..Default::default()
                },
                &themes,
            )
        };
        let selector = |theme: &str| theme_selector_notice(Some(theme));
        // dark:A,dark:B draws A and still reports the duplicate.
        let c = cfg(r#"theme = "dark:Nord,dark:Dracula""#);
        assert_eq!(
            c.resolve_theme_name(Appearance::Dark).as_deref(),
            Some("Nord")
        );
        assert_eq!(
            misses("dark:Nord,dark:Dracula"),
            vec![selector("dark:Nord,dark:Dracula").unwrap()]
        );
        // An unknown mode beside a good side: one selector notice, no catalog miss.
        assert_eq!(misses("dark:Nord,lite:Dracula").len(), 1);
        // A selector issue and a missing theme are both said.
        assert_eq!(misses("dark:Nord,light:Nope,light:Dracula").len(), 2);
        // Negative controls: clean selectors add no selector notice.
        for clean in ["Nord", "dark:Nord,light:GitHub Light", "dark:Nord"] {
            assert_eq!(selector(clean), None, "{clean:?}");
            assert!(misses(clean).is_empty(), "{clean:?}");
        }
        assert!(theme_selector_notice(None).is_none());
    }

    /// The engine config (default, cursor and selection colors) also tracks the
    /// split, so a live switch recolors cells and the OSC-reset baselines, not
    /// just chrome.
    #[test]
    fn applied_terminal_config_tracks_split() {
        let c = cfg(r#"theme = "dark:Dracula,light:GitHub Light""#);
        let rgb = |packed: u32| {
            aterm_types::Rgb::new(
                ((packed >> 16) & 0xff) as u8,
                ((packed >> 8) & 0xff) as u8,
                (packed & 0xff) as u8,
            )
        };
        assert_ne!(
            c.applied_terminal_config_for(Appearance::Dark)
                .default_background,
            c.applied_terminal_config_for(Appearance::Light)
                .default_background
        );
        for side in [Appearance::Dark, Appearance::Light] {
            let engine = c.applied_terminal_config_for(side);
            let theme = c.theme_for(side);
            assert_eq!(engine.default_background, rgb(theme.bg), "{side:?}");
            assert_eq!(
                engine.cursor_color,
                Some(rgb(theme.cursor)),
                "{side:?}: the OSC 112 reset baseline"
            );
            assert_eq!(
                engine.selection_background,
                Some(rgb(theme.selection)),
                "{side:?}: the OSC 117 reset baseline"
            );
        }
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

    /// Path-feed fingerprints track file content: stable while untouched, moved
    /// by an edit or deletion.
    #[test]
    fn path_feed_fingerprints_track_referenced_file_content() {
        let dir = std::env::temp_dir().join(format!("aterm-feed-fp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lex = dir.join("lexicon.toml");
        std::fs::write(&lex, "one").unwrap();
        // Paths go through the crate's escaper; a raw Windows path would be an
        // invalid `\U` escape.
        let config = cfg(&format!(
            "[sparkle_words]\nlexicon = {}\n",
            super::toml_basic_string(&lex.to_string_lossy())
        ));
        let base = config.path_feed_fingerprints();
        assert_eq!(
            base,
            config.path_feed_fingerprints(),
            "untouched files fingerprint stably"
        );
        std::fs::write(&lex, "two").unwrap();
        let lexed = config.path_feed_fingerprints();
        assert_ne!(lexed.deco, base.deco, "lexicon content feeds deco");
        // A file DISAPPEARING is a content change too (fail-visible, never stale).
        std::fs::remove_file(&lex).unwrap();
        assert_ne!(
            config.path_feed_fingerprints().deco,
            lexed.deco,
            "a vanished lexicon file reads as changed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A path generation is one transaction per consumer: a file replaced after
    /// the same-handle read cannot relabel the admitted bytes (Tier-1 projection
    /// of the path-feed snapshot model).
    #[test]
    fn path_feed_generation_binds_consumer_and_fingerprint_to_one_read() {
        use std::hash::Hasher as _;

        let dir =
            std::env::temp_dir().join(format!("aterm-feed-generation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lexicon = dir.join("lexicon.toml");
        let v1 = "# generation-v1\n";
        let v2 = "# generation-v2\n";
        std::fs::write(&lexicon, v1).unwrap();
        let lexicon_path = lexicon.to_string_lossy().into_owned();
        let config = cfg(&format!("[sparkle_words]\nlexicon = {lexicon_path:?}\n"));
        let model = aterm_models_misc::path_feed_snapshot_model();
        let mut model_state = model.init_state();
        let v1_fingerprint = config.path_feed_fingerprints().deco;

        let mut reads = 0;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let (_, admitted) = super::read_and_fingerprint_path_feed_with_reader(
            &lexicon_path,
            aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES,
            &mut hasher,
            |admitted_path, max_bytes| {
                reads += 1;
                let admitted =
                    aterm_effects::file_feed::read_bounded_regular_utf8(admitted_path, max_bytes);
                // Replace the file after the bounded read; parsing and
                // fingerprinting must both use the returned v1 string.
                std::fs::write(&lexicon, v2).unwrap();
                admitted
            },
        );
        let admitted_fingerprint = hasher.finish();
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
        assert_eq!(
            admitted.as_deref(),
            Ok(v1),
            "the consumer holds the admitted v1"
        );
        assert_eq!(
            admitted_fingerprint, v1_fingerprint,
            "Tier-1 projection: prepared=1 and fingerprint=1 derive from the admitted v1 String"
        );
        assert_ne!(
            admitted_fingerprint,
            config.path_feed_fingerprints().deco,
            "Tier-1 negative control: the v1 consumer never carries the live v2 fingerprint"
        );

        let replacement = config.prepare_path_feed_generation();
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

    /// The launch's generation is handed to the boot's join while its
    /// kitty-command vocabulary still compiles, and lands the same vocabulary a
    /// reload's generation carries: a typed pet command works once it is in.
    /// The compile is held where it runs ([`super::TRICKS_COMPILE_GATE`]) while
    /// the launch prepares, so the handover is seen before the vocabulary lands.
    /// Negative controls: a launch that compiles in line again hands over a
    /// generation whose vocabulary is already in; one whose worker the join waits
    /// for never hands one over (the hang detector).
    #[test]
    fn the_launch_generation_lands_the_same_kitty_vocabulary_after_the_join() {
        let config = cfg("[sparkle_words]\nlanguages = [\"en\", \"es\"]\n");
        let reload = config.prepare_path_feed_generation();
        let held = super::TRICKS_COMPILE_GATE
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (handed_tx, handed) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = handed_tx.send(config.prepare_launch_path_feed_generation());
        });
        let launch = handed
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the launch generation is handed over while its vocabulary compiles");
        let mut scratch = String::new();
        assert_eq!(
            launch.sparkle.tricks().classify("sit", &mut scratch),
            None,
            "handed over before its vocabulary landed: a press reads the empty one"
        );
        drop(held);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the launch generation's kitty vocabulary to land",
            || launch.sparkle.tricks.0.get().map(|_| ()),
        );
        assert!(
            reload
                .sparkle
                .tricks()
                .classify("sit", &mut scratch)
                .is_some(),
            "negative control: a compiled vocabulary knows `sit`"
        );
        for word in ["sit", "kitty", "please", "siéntate", "sientate", "compile"] {
            assert_eq!(
                launch.sparkle.tricks().classify(word, &mut scratch),
                reload.sparkle.tricks().classify(word, &mut scratch),
                "{word}"
            );
        }
        assert_eq!(
            launch.sparkle.tricks().surface_count(),
            reload.sparkle.tricks().surface_count()
        );
    }

    /// Until the launch's compile lands, a press reads the empty vocabulary
    /// without waiting for it, and nothing fires.
    #[test]
    fn an_unlanded_kitty_vocabulary_classifies_nothing() {
        let unlanded = super::TrickVocabulary(std::sync::Arc::default());
        let mut scratch = String::new();
        assert_eq!(unlanded.get().classify("sit", &mut scratch), None);
        assert_eq!(unlanded.get().surface_count(), 0);
        assert!(
            super::TrickVocabulary::compiled(vec!["en".to_string()])
                .get()
                .classify("sit", &mut scratch)
                .is_some(),
            "negative control: the compiled vocabulary knows `sit`"
        );
    }

    #[test]
    fn path_feed_fingerprints_enforce_the_loader_specific_caps() {
        let dir = std::env::temp_dir().join(format!("aterm-feed-caps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lexicon = dir.join("lexicon.toml");
        let toy = dir.join("toy.toml");
        let config = cfg(&format!(
            "[sparkle_words]\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
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
        assert_eq!(
            config.path_feed_fingerprints(),
            missing,
            "oversized inputs are rejected exactly like the loaders, not hashed past their caps"
        );

        std::fs::write(&lexicon, "# admitted lexicon\n").unwrap();
        std::fs::write(&toy, "# admitted toy manifest bytes\n").unwrap();
        let admitted = config.path_feed_fingerprints();
        assert_ne!(admitted.deco, missing.deco);
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
            "[sparkle_words]\nlexicon = {0:?}\ntoy_packs = [{0:?}]\n",
            path.to_string_lossy(),
        ));
        let fifo_fingerprint = config.path_feed_fingerprints();
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
    /// recompute and hard-resets word decorations.
    #[test]
    fn byte_equal_reload_still_rereads_touched_path_feed_files() {
        let dir = std::env::temp_dir().join(format!("aterm-feed-touch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lex = dir.join("lexicon.toml");
        std::fs::write(&lex, "# lexicon v1\n").unwrap();

        let mut app = crate::App::headless_for_test();
        // Escaped for TOML, as in the fingerprint test above.
        app.config = cfg(&format!(
            "[sparkle_words]\nlexicon = {}\n",
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

        // (1) Byte-equal config + untouched files ⇒ the full no-op survives.
        let fresh = app.config.path_feed_fingerprints();
        app.refresh_path_feeds(fresh);
        assert!(!app.sparkle_dirty, "nothing changed ⇒ no recompute re-arm");
        assert!(
            !app.windows[&wid].fx.decos().needs_rescan(7),
            "…and no word-deco reset (the settings-commit + watcher dedupe win)"
        );

        // (2) Lexicon file edit, config bytes identical ⇒ re-read + hard reset.
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
        // Same unrelated edit, but the lexicon file content drifted ⇒ reset.
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
        // The UX floor holds (headless scale is 1.0).
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
            crate::tab_bar::CaptionSlot::NONE,
            None,
            None,
        ));

        let new_theme =
            aterm_render::Theme::from(&aterm_types::scheme::builtin("Dracula").unwrap());
        assert_ne!(
            app.theme.bg, new_theme.bg,
            "test needs a real colour change"
        );

        app.apply_theme_live(new_theme);

        // App state carries the new theme for any later rebuild or zoom.
        assert_eq!(app.theme.bg, new_theme.bg);
        assert_eq!(app.theme.fg, new_theme.fg);
        // Present caches dropped: strip rows retint, RepaintKey gate repaints.
        assert!(app.windows[&wid].last_strip_fp.is_none(), "strip cache");
        assert!(app.windows[&wid].last_present.is_none(), "RepaintKey gate");
        // The live backend paints the new theme, byte-identical to a renderer
        // built with it: the swap needs no rebuild.
        let got = render(app.backend.ready_mut());
        assert_ne!(got.pixels, before.pixels, "the idle grid recoloured");
        let mut fresh = aterm_render::Renderer::from_system(crate::FONT_PX, new_theme)
            .expect("system font for test renderer");
        // Built as the headless App's renderer is: the default line height.
        fresh.set_line_height(super::Config::default().line_height_or_default());
        assert_eq!(got.pixels, fresh.render_input(&input).pixels);
    }
}

#[cfg(test)]
mod matrix_rain_cfg_tests {
    //! Section resolvers: `[matrix_rain]` (defaults and clamps live only in
    //! `Config::matrix_rain_params`; the enabled bit, default off, resolves
    //! separately), `[packages]`, `[reroute]`, `[update]`, `[machine]` and `[privacy]`.

    use super::{Config, PRIVACY_WARMUP_HOLD_MS_MAX, PrivacyWarmup};
    use crate::matrix_rain::{RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR, RainConfig, RainHue};

    /// Stock dark-theme chrome, as the renderer knows it (`0x00RR_GGBB`).
    const BG: u32 = 0x0011_1318;
    const FG: u32 = 0x00D0_D0D0;
    const CURSOR: u32 = 0x0050_FA7B;

    /// The enabled-gated composition of `matrix_rain_enabled` and
    /// `matrix_rain_params`, used by the parameter tests below.
    fn rain(toml: &str) -> Option<RainConfig> {
        let cfg = aterm_toml::from_str::<Config>(toml).expect("valid toml");
        cfg.matrix_rain_enabled()
            .then(|| cfg.matrix_rain_params(BG, FG, CURSOR))
    }

    /// Off unless `enabled = true` (a table of other knobs stays off), while
    /// the parameters resolve independently of that bit, so a session forcing
    /// rain on over a disabled config gets the same field as `enabled = true`.
    #[test]
    fn rain_is_off_by_default_and_params_ignore_the_enabled_bit() {
        assert!(!Config::default().matrix_rain_enabled());
        assert!(rain("[matrix_rain]\nfps = 42").is_none());
        assert!(rain("[matrix_rain]\nenabled = false\nfps = 42").is_none());
        let params = |toml: &str| {
            aterm_toml::from_str::<Config>(toml)
                .unwrap()
                .matrix_rain_params(BG, FG, CURSOR)
        };
        let disabled = params("[matrix_rain]\nenabled = false\nfps = 24");
        assert_eq!(disabled, params("[matrix_rain]\nenabled = true\nfps = 24"));
        assert_eq!(disabled.fps, 24, "knobs still resolve while disabled");
    }

    /// `[packages]`: Settings explains an off held by the legacy
    /// `auto_update = false`, in atpkg's words, but not when `enabled = false`
    /// sits beside it; the retired `channel`/`include` keys do not fail the
    /// load. (The switch values themselves are pinned against atpkg's reader
    /// below.)
    #[test]
    fn packages_retired_switch_note_and_retired_keys_load() {
        let note = |text: &str| {
            aterm_toml::from_str::<Config>(text)
                .unwrap()
                .packages_retired_switch_note()
        };
        assert_eq!(
            note("[packages]\nauto_update = false"),
            Some(atpkg::config::auto_update_note(false, None))
        );
        assert_eq!(
            note("[packages]\nenabled = true\nauto_update = false"),
            Some(atpkg::config::auto_update_note(false, Some(true)))
        );
        for text in [
            "[packages]\nenabled = false\nauto_update = false",
            "[packages]\nauto_update = true",
            "[packages]\nenabled = false",
            "",
        ] {
            assert_eq!(note(text), None, "{text:?}");
        }
        let retired = "[packages]\nchannel = 7\ninclude = \"x\"\n[packages.links]\nay = \"~/ay\"\n";
        assert!(aterm_toml::from_str::<Config>(retired).is_ok());
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

    /// The GUI `Config` and atpkg's `MachineConfig` resolve identical defaults and
    /// explicit values for `[machine]`.
    #[test]
    fn machine_defaults_agree_across_the_two_config_readers() {
        let gui = Config::default();
        let pkg = atpkg::config::MachineConfig::default();
        assert_eq!(gui.machine_universal_control(), pkg.universal_control());
        assert_eq!(gui.machine_rosetta(), pkg.rosetta());
        for text in [
            "[machine]\nuniversal_control = \"leave\"\n",
            "[machine]\nuniversal_control = \"off\"\n",
            "[machine]\nuniversal_control = \"\"\n",
            "[machine]\nrosetta = false\n",
            "[machine]\nrosetta = true\n",
            "[machine]\nuniversal_control = \"leave\"\nrosetta = false\n",
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
            assert_eq!(gui.machine_rosetta(), pkg.rosetta(), "{text:?}");
        }
        let bogus =
            aterm_toml::from_str::<Config>("[machine]\nuniversal_control = \"bogus\"\n").unwrap();
        assert_eq!(
            bogus.machine_universal_control(),
            atpkg::config::UniversalControlPolicy::Leave,
            "an unknown spelling is the inert reading, never a write on the machine"
        );
    }

    /// An absent `[privacy]` table probes, warm-up is automatic (the owner's
    /// gesture and the launch pass), and the observer is the sentinel: the
    /// full stream is opt-in (`observer = true`), `false` turns it off.
    #[test]
    fn privacy_absent_section_offers_warmup_and_observes_as_sentinel() {
        use crate::consent_observer::ObserverMode;
        let c = Config::default();
        assert!(c.privacy_probe_gate().permits());
        assert_eq!(c.privacy_warmup(), PrivacyWarmup::Automatic);
        let mode = |text: &str| {
            aterm_toml::from_str::<Config>(text)
                .expect("parses")
                .privacy_observer_mode()
        };
        assert_eq!(mode(""), ObserverMode::Sentinel, "no [privacy] table");
        assert_eq!(mode("[privacy]\n"), ObserverMode::Sentinel, "key absent");
        assert_eq!(mode("[privacy]\nobserver = false\n"), ObserverMode::Off);
    }

    /// `app-data` is a valid warm-up name but not a default one: macOS forgets
    /// its Allow when the process that received it is replaced, so asking for it
    /// up front raises a dialog whose answer cannot last.
    #[test]
    fn the_default_warmup_omits_app_data_but_an_explicit_list_may_include_it() {
        let absent = Config::default();
        let defaults = absent.privacy_warmup_folders();
        assert_eq!(defaults, vec!["documents", "desktop", "downloads"]);
        let explicit = aterm_toml::from_str::<Config>(
            "[privacy]\nwarmup_folders = [\"documents\", \"app-data\"]\n",
        )
        .expect("explicit list parses");
        assert_eq!(
            explicit.privacy_warmup_folders(),
            vec!["documents".to_string(), "app-data".to_string()]
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
                "observer = true\n",
                "protected_roots = [\"{}/aterm-privacy-explicit\"]\n",
                "auto_accept = true\n",
            ),
            ABS_TMP
        ))
        .expect("explicit table parses");
        assert!(
            !c.privacy_probe_gate().permits(),
            "either bit off ⇒ the gate refuses before any syscall"
        );
        assert_eq!(c.privacy_warmup(), PrivacyWarmup::Never);
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
            PrivacyWarmup::default(),
            "unknown spellings fall back; the validator names them"
        );
        assert_eq!(
            PrivacyWarmup::parse("Automatic"),
            Some(PrivacyWarmup::Automatic)
        );
        assert!(PrivacyWarmup::OnRequest.offered() && PrivacyWarmup::Automatic.offered());
        assert!(!PrivacyWarmup::Never.offered());
    }

    /// `protected_roots = []` resolves to the Containment private set, not to
    /// "nothing is protected". Unix only: the set lives under `$HOME`, which a
    /// Windows session need not have (Containment is a macOS sandbox).
    #[cfg(unix)]
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

    /// Every clamped knob stays inside its band at both edges (design §12):
    /// nothing resolves to zero, the frame rate never passes 60, and alpha
    /// never passes the READABLE ceiling.
    #[test]
    fn clamps_hold_at_both_edges() {
        let lo = rain(concat!(
            "[matrix_rain]\nenabled = true\n",
            "fps = 1\ndensity = 0\nspeed = 0\ntrail = 0\n",
            "alpha = 0\nhead_alpha = 0\nmutation_ms = 1\nidle_secs = 0\n"
        ))
        .unwrap();
        assert!(
            lo.fps > 1
                && lo.density > 0
                && lo.speed > 0
                && lo.trail > 0
                && lo.mutation_ms > 1
                && lo.idle_secs > 0
        );
        assert_eq!(lo.alpha_override, Some(RAIN_ALPHA_FLOOR));
        let hi = rain(concat!(
            "[matrix_rain]\nenabled = true\n",
            "fps = 240\nalpha = 255\nhead_alpha = 255\n"
        ))
        .unwrap();
        assert!(hi.fps <= 60);
        assert_eq!(hi.alpha_override, Some(RAIN_ALPHA_CAP), "READABLE ceiling");
        assert_eq!(hi.head_alpha_override, Some(RAIN_ALPHA_CAP));
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
}

#[cfg(test)]
mod matrix_rain_app_tests {
    //! Matrix rain host wiring: no engine exists when nothing enables rain; the
    //! per-session `toggle_matrix_rain` override (both directions,
    //! reload-surviving, the engine kept for the drain); the `rain` and `fx`
    //! verbs' status lines; and per-window seeds.

    use super::Config;
    use crate::keybinding::Action;

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
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
        assert!(
            app.windows.values().all(|ws| ws.fx.rain().is_none()),
            "no engine is constructed while rain is off"
        );

        // On over a disabled config.
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

        // Off again: the effective state flips; a live engine is retained for
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

        // An off override also beats an enabled config (the panic-off shape).
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
    }

    /// The `fx` verb ([`crate::App::fx_control`]): `status` is a pure read;
    /// `celebrate` arms once and a second arm is refused. `live` stays false
    /// (no keyed edge).
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
        assert!(again.is_err(), "one arm at a time");
        let status = app
            .fx_control(crate::FxCtlOp::Status)
            .expect("status reads");
        assert!(
            status.starts_with("armed=green"),
            "status is a pure read: {status}"
        );
    }

    /// Every edge (`on=done` included) meets the same refusals: the trail off,
    /// another style, Serious Mode.
    #[test]
    fn fx_control_refuses_inactive_visual_policies_without_spending_an_arm() {
        use aterm_effects::kitty_sing::CelebrateOn;
        for (config, on) in [
            "cursor_trail = false\ncursor_trail_style = \"rainbow kitty pet\"",
            "cursor_trail = true\ncursor_trail_style = \"laser\"",
            "cursor_trail = true\ncursor_trail_style = \"rainbow kitty pet\"\nserious_mode = true",
        ]
        .into_iter()
        .flat_map(|c| [CelebrateOn::Green, CelebrateOn::Done].map(|on| (c, on)))
        {
            let mut app = crate::App::headless_for_test();
            app.config = cfg(config);
            app.apply_serious_mode(app.config.serious_mode_or_default());
            let arm = app.fx_control(crate::FxCtlOp::Celebrate {
                sig: aterm_effects::kitty_sing::song_signature('C'),
                bars: 2,
                on,
            });
            assert!(arm.is_err(), "{config} {on:?}: {arm:?}");
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
}

/// The auto-prime knob: batteries-on, a real off, and readable without the
/// side effects of a full config load.
#[cfg(test)]
mod agents_auto_prime_tests {
    use super::agents_auto_prime_from_text;

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

/// The enablement gate from the config string down: only `off` resolves to no
/// style; everything else, a typo included, draws.
#[cfg(test)]
mod trail_style_resolution_tests {
    use super::*;
    use aterm_effects::cursor_glow::{GlowConfig, GlowStyle};

    fn glow(raw: &str, enabled: bool) -> GlowConfig {
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
            resolve_trail_presentation(raw),
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
        .cursor_trail_style_warning()
    }

    /// CLASSIC RECEIVES THE SHIPPED KNOBS: a window whose user named Classic
    /// and set no knob hands the engine 260 ms, 24 cells, the v0.28 release's
    /// 0.7 brightness, a 0.6-cell crown and the ring, the knobs aterm-effects'
    /// `the_host_resolved_dark_wake_is_byte_identical_to_its_base` pins the
    /// dark wake on, through the window's own resolution (`App::glow_config`).
    /// Negative controls: a set length and a set brightness reach the engine
    /// as set, and the one-colour face's and another style's unset brightness
    /// are full.
    #[test]
    fn classic_receives_the_shipped_knobs() {
        let mut app = crate::App::headless_for_test();
        app.config.cursor_trail_style = Some("classic".to_string());
        let cfg = app.glow_config();
        assert_eq!(
            (
                cfg.style,
                cfg.enabled,
                cfg.duration,
                cfg.length,
                cfg.intensity,
                cfg.radius,
                cfg.ring,
                cfg.beam,
                cfg.classic_mono,
                cfg.head_dx,
            ),
            (
                GlowStyle::Classic,
                true,
                std::time::Duration::from_millis(260),
                24,
                aterm_effects::cursor_fx::CLASSIC_DEFAULT_INTENSITY,
                0.6,
                true,
                true,
                false,
                0.5,
            ),
            "the window hands Classic other knobs than the dark wake is pinned on"
        );
        assert_eq!(
            aterm_effects::cursor_fx::CLASSIC_DEFAULT_INTENSITY,
            0.7,
            "Classic's unset brightness is the v0.28 release's"
        );
        app.config.cursor_trail_length = Some(18);
        assert_eq!(
            app.glow_config().length,
            18,
            "control: a set length is read"
        );
        app.config.cursor_trail_intensity = Some(1.0);
        assert_eq!(
            app.glow_config().intensity,
            1.0,
            "control: a set brightness is read"
        );
        app.config.cursor_trail_intensity = None;
        app.config.cursor_trail_color = Some("#d03a8c".to_string());
        assert_eq!(
            app.glow_config().intensity,
            1.0,
            "control: Classic's one-colour face (a trail colour) draws at full brightness"
        );
        app.config.cursor_trail_color = None;
        app.config.cursor_trail_style = Some("lumen".to_string());
        assert_eq!(
            app.glow_config().intensity,
            1.0,
            "control: another style's unset brightness is full"
        );
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
        let catalog = config.resolve_trail_style_cache();
        let cached = catalog.presentation_for(config.cursor_trail_style_raw());
        assert_eq!(cached.style.style, Some(GlowStyle::RainbowKitty));
        assert!(!cached.ribbon_tall);
        assert_eq!(
            cached.pet_species,
            Some(aterm_effects::kitty_pet::PetSpecies::Cat)
        );

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
    /// config and uses the music box for `auto`. `… v1` and any typo are
    /// unknown: they fall back flagged `unknown` (the fallback is not an
    /// endorsement), draw the default, feed the companion and ribbon
    /// predicates the fallback token and warn. Other styles and `off` never
    /// engage v2. `Default` and the resolver agree.
    #[test]
    fn every_rainbow_kitty_spelling_is_v2() {
        use aterm_effects::trail_sound::SoundVoice;
        let default = crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE;
        assert_eq!(
            ResolvedTrailPresentation::default(),
            resolve_trail_presentation(default),
            "`Default` must be the resolver's answer for the default spelling"
        );
        let auto = Config::default();
        let marimba = Config {
            trail_sound_style: Some("marimba".to_string()),
            ..Config::default()
        };
        let spellings = crate::prefs::CURSOR_TRAIL_STYLES.iter().copied().chain(
            crate::prefs::CURSOR_TRAIL_STYLE_ALIASES
                .iter()
                .map(|&(alias, _)| alias),
        );
        for raw in spellings {
            let p = resolve_trail_presentation(raw);
            assert!(!p.style.unknown, "{raw:?} is a documented spelling");
            let is_kitty = p.style.style == Some(GlowStyle::RainbowKitty);
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
        // The old escape and every other unknown word: the default, reported.
        let fallback = resolve_trail_presentation(default);
        for v1 in [
            "rainbow kitty v1",
            "RAINBOW KITTY V1 pet",
            "v2",
            "",
            "phasr",
            "rainbow kity tall",
        ] {
            let p = resolve_trail_presentation(v1);
            assert!(p.style.unknown, "{v1:?} names no style and says so");
            assert_eq!(
                ResolvedTrailStyle {
                    unknown: false,
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
            // The companion and ribbon predicates read the fallback token, not
            // the typo, and the typo is reported.
            assert_eq!(effective_trail_style_token(v1), default, "{v1:?}");
            assert_eq!(
                glow(v1, true).ribbon_tall,
                glow(default, true).ribbon_tall,
                "{v1:?}"
            );
            assert!(warning_for(v1).is_some(), "{v1:?} must warn");
        }
        // The generation cache agrees with the resolver, escape included.
        let config = Config {
            cursor_trail_style: Some("rainbow kitty v1".to_string()),
            ..Config::default()
        };
        let cached = config
            .resolve_trail_style_cache()
            .presentation_for(config.cursor_trail_style_raw());
        assert!(cached.style.unknown);
        assert_eq!(cached.style.style, Some(GlowStyle::RainbowKitty));
        assert_eq!(
            config.trail_sound_voice_for(cached),
            SoundVoice::RainbowKittyV2
        );
    }

    /// The two ways the trail legitimately goes dark, neither of which the
    /// fallback may re-light.
    #[test]
    fn off_and_the_master_switch_still_disable_the_trail() {
        for spelling in ["off", "OFF", "  off  "] {
            assert_eq!(resolve_trail_style(spelling).style, None);
            assert!(!resolve_trail_style(spelling).unknown);
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
        // `off` is the ONLY builtin spelling that means off, and no alias adds
        // a second one.
        for &s in crate::prefs::CURSOR_TRAIL_STYLES {
            assert_eq!(
                resolve_trail_style(s).style.is_none(),
                s == "off",
                "style {s:?}"
            );
        }
        for &(alias, _) in crate::prefs::CURSOR_TRAIL_STYLE_ALIASES {
            assert!(
                resolve_trail_style(alias).style.is_some(),
                "alias {alias:?} must not disable the trail"
            );
        }
    }

    /// Bloom radius is per style (the rainbow kitty's halo is tighter, and the
    /// default spelling is the rainbow kitty), and `cursor_trail_bloom_radius`
    /// overrides every style.
    #[test]
    fn the_bloom_radius_is_per_style_with_the_knob_as_the_override() {
        fn radius(style: Option<&str>, knob: Option<f32>) -> f32 {
            let config = Config {
                cursor_trail_style: style.map(str::to_string),
                cursor_trail_bloom_radius: knob,
                ..Config::default()
            };
            let catalog = config.resolve_trail_style_cache();
            config.cursor_trail_bloom_radius_or_default(
                catalog.presentation_for(config.cursor_trail_style_raw()),
            )
        }
        let kitty = radius(Some("rainbow kitty"), None);
        assert!(kitty < radius(Some("laser"), None));
        assert_eq!(radius(Some("rainbow kitty pet"), None), kitty);
        assert_eq!(radius(None, None), kitty);
        assert_eq!(radius(Some("rainbow kitty"), Some(3.0)), 3.0);
        assert_eq!(radius(Some("laser"), Some(3.0)), 3.0);
    }
}
