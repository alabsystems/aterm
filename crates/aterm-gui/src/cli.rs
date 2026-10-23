// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI argument parsing for `aterm-gui`, with no `App` coupling.

use std::sync::RwLock;

/// Parsed CLI: the `-e` command to run instead of `$SHELL` (if any), the `--working-directory` to start it in (if any), whether to `--hold` the window open after the command exits, and whether `--headless` was passed.
pub(crate) struct Cli {
    /// The `-e` payload as the OS gave it: a program's arguments are bytes, not text, so they are never re-encoded on the way to the spawn.
    pub(crate) exec_command: Option<Vec<std::ffi::OsString>>,
    pub(crate) cwd: Option<String>,
    pub(crate) hold: bool,
    /// `--headless`: no window — engine + PTY + control socket only — and on macOS no application object either (no Dock tile, never the front app, no WindowServer connection; `main_entry` in `lib.rs`), so a harness may boot one beside a person who is typing.
    pub(crate) headless: bool,
    /// `--lifeline-fd <n>` (headless only): the descriptor whose end-of-file means the process that started this instance is gone, so it shuts down ([`crate::lifeline`]).
    pub(crate) lifeline_fd: Option<i32>,
    /// `--ready-fd <n>` (headless only): the pipe's write end this instance writes one byte to, then closes, once its control socket listens ([`crate::lifeline`]'s ready signal).
    pub(crate) ready_fd: Option<i32>,
    /// The render/font flags, for `main` to [`crate::launch::install`].
    pub(crate) launch: crate::launch::Launch,
}

/// The launch flags: what this process's command line asked of it, recorded by [`parse_cli`] before any thread exists and read wherever the window needs it (the socket plan, the initial grid, the shell, the containment funnel).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LaunchFlags {
    /// `--columns <n>` (20..=500, validated on the way in).
    pub(crate) columns: Option<u16>,
    /// `--lines <n>` (5..=300, validated on the way in).
    pub(crate) lines: Option<u16>,
    /// `--shell <name|path>`: outranks the config `shell` for every new tab.
    pub(crate) shell: Option<String>,
    /// `--containment <mode>` / `--sandbox` / `--no-sandbox`, as typed: the parse, and its fail-closed fallback, are the launch funnel's in `main`.
    pub(crate) containment: Option<String>,
    /// `--control-sock <path|0|off>` / `--no-control-sock`: the socket directive's two inputs ([`aterm_types::control_socket::socket_directive`]).
    pub(crate) control_sock: Option<String>,
    /// `--no-control-sock`.
    pub(crate) no_control_sock: bool,
    /// `--no-shell-integration`.
    pub(crate) no_shell_integration: bool,
    /// `--verbose`: the routine startup notices on stderr.
    pub(crate) verbose: bool,
}

impl LaunchFlags {
    /// Nothing asked — the ordinary launch.
    const NONE: Self = Self {
        columns: None,
        lines: None,
        shell: None,
        containment: None,
        control_sock: None,
        no_control_sock: false,
        no_shell_integration: false,
        verbose: false,
    };

    /// The control-socket directive these flags ask for: the same decision a client makes about a socket path ([`aterm_types::control_socket::socket_directive`]).
    pub(crate) fn socket_directive(&self) -> aterm_types::control_socket::SocketDirective {
        aterm_types::control_socket::socket_directive(
            self.control_sock.as_deref(),
            self.no_control_sock.then_some("1"),
        )
    }
}

/// The process's launch flags.
static LAUNCH_FLAGS: RwLock<LaunchFlags> = RwLock::new(LaunchFlags::NONE);

/// This process's launch flags (a copy; a poisoned lock still yields its value).
pub(crate) fn launch_flags() -> LaunchFlags {
    LAUNCH_FLAGS.read().map_or_else(
        |poisoned| poisoned.into_inner().clone(),
        |flags| flags.clone(),
    )
}

/// Record one flag. Only [`parse_cli`] calls this.
fn set_launch_flag(set: impl FnOnce(&mut LaunchFlags)) {
    let mut flags = LAUNCH_FLAGS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    set(&mut flags);
}

/// The `--shell` lines of [`HELP_HEAD`], as this platform resolves a shell: Windows discovers a name (Git Bash off PATH, the `wsl`/`cmd` aliases); Unix looks it up on PATH.
#[cfg(windows)]
macro_rules! shell_flag_help {
    () => {
        concat!(
            "        --shell <name|path>        Interactive shell to spawn. Discovery-resolved:\n",
            "                                   \"bash\" finds Git Bash even off PATH; \"pwsh\",\n",
            "                                   \"cmd\", \"wsl\", \"nu\", or a path also work. Shell\n",
            "                                   integration is injected for zsh/bash/fish/pwsh and\n",
            "                                   \"wsl\" (bash login); \"cmd\" is partial (no command\n",
            "                                   text or exit code); \"nu\" gets none. [config: shell]\n",
        )
    };
}
#[cfg(not(windows))]
macro_rules! shell_flag_help {
    () => {
        concat!(
            "        --shell <name|path>        Interactive shell to spawn: a name on PATH (\"bash\",\n",
            "                                   \"zsh\", \"fish\") or a path (default: $SHELL, else\n",
            "                                   login shell). Shell integration is injected for\n",
            "                                   zsh, bash, fish and pwsh. [config: shell]\n",
        )
    };
}

/// The `--help` text: every user-facing flag with its argument, a one-line description and its
/// `[config: key]` where it has one.
const HELP_TITLE: &str = "aterm-gui — a fast, hardened terminal\n";
const HELP_HEAD: &str = concat!(
    "\n",
    "USAGE:\n",
    "    aterm-gui [OPTIONS]\n",
    "    aterm-gui [-d <dir>] -e <command> [args...]\n\n",
    "OPTIONS:\n",
    "    -e, --command <cmd> [args...]  Run <cmd> instead of $SHELL; window closes on exit.\n",
    "                                   Consumes the rest of the command line.\n",
    "    -d, --working-directory <dir>  Start the shell or command in <dir>.\n",
    "        --hold                     Keep the window open after the -e command exits.\n",
    "        --font-px <px>             Font size in px at 1x, points on macOS (6..=200),\n",
    "                                   for this launch. [config: font_px]\n",
    "        --font <name|path>         Primary font family or font file for this launch.\n",
    "                                   [config: font_family]\n",
    shell_flag_help!(),
    "        --scale <f>                Force render scale factor (font + padding); headless,\n",
    "                                   sets `image` capture DPI (e.g. 2 = 2x Retina).\n",
    "        --gpu                      Force GPU rendering — the DEFAULT (Metal/GL/DX12;\n",
    "                                   auto CPU fallback). [config: gpu]\n",
    "        --cpu                      Force CPU renderer (last of --cpu/--gpu wins).\n",
    "        --containment <mode>       Containment mode: user|safety|containment\n",
    "                                   (invalid values fail closed to containment).\n",
    "        --sandbox                  Shorthand for --containment containment.\n",
    "        --no-sandbox               Shorthand for --containment user.\n",
    "        --control-sock <path>      Bind control socket at <path> (or 0/off to disable);\n",
    "                                   clients connect with --sock <path>.\n",
    "        --no-control-sock          Disable the control socket.\n",
    "        --headless                 No window; engine + control socket only. Announces\n",
    "                                   mode on stderr; SIGTERM/SIGINT/SIGHUP quits cleanly\n",
    "                                   (a second exits immediately; ignored signals stay ignored).\n",
    "        --lifeline-fd <n>          With --headless: shut down when descriptor <n> (pipe\n",
    "                                   read end) reaches EOF because its launcher exited.\n",
    "        --ready-fd <n>             With --headless: write one byte to descriptor <n> (3+)\n",
    "                                   and close it once the control socket listens (closed\n",
    "                                   empty if the socket is disabled).\n",
    "        --columns <n>              Initial width in columns (20..=500).\n",
    "        --lines <n>                Initial height in rows (5..=300).\n",
    "        --shell-integration        OSC 133/633 command marks (default on; no-op).\n",
    "        --no-shell-integration     Disable shell-integration marks.\n",
    "        --verbose                  Verbose diagnostics on stderr.\n",
    "        --diagnose                 Print diagnostics (version, build, renderer,\n",
    "                                   capabilities, config, env) and exit.\n",
    "        --list-actions             List bindable [keybindings] action names and exit.\n",
    "        --validate-config          Check aterm.toml (unknown keys, unaccepted values,\n",
    "                                   parse errors); exits 1 only if the file fails to load.\n",
    "        --list-fonts               List font search dirs and discoverable families, then exit.\n",
    "        --show-config              Print effective launch config (font, size, renderer,\n",
    "                                   shell, theme, colours) and config load state, then exit.\n",
    "        --write-config             Write a commented starter aterm.toml if absent, then exit.\n",
    "        --list-keybinds            List built-in + configured [keybindings], [key_sequences],\n",
    "                                   and bindable actions, then exit.\n",
    "        --show-face [family]       Print resolved font face (path + metrics) for [family]\n",
    "                                   (or the configured font) and exit.\n",
    "        --list-themes              List built-in and custom colour themes and exit.\n",
    "    -h, --help                     Print this help and exit.\n",
    "    -V, --version                  Print the version and exit.\n\n",
);

/// The macOS keyboard-shortcut help: the common Cmd-* chords.
#[cfg(target_os = "macos")]
const KEYS_HELP: &str = concat!(
    "KEYS (in the window):\n",
    "    Cmd-C / Cmd-V     Copy selection / paste (control-stripped, bracketed).\n",
    "    Cmd-= / Cmd--     Zoom font in / out.   Cmd-0  Reset zoom.\n",
    "    Cmd-click         Open hyperlink, URL (http/https/mailto) or file:line citation;\n",
    "                      Cmd-Option-click where the program tracks the mouse.\n",
    "    Cmd-F             Find (screen + scrollback): type, Enter/Shift-Enter, Esc.\n",
    "    Cmd-S / Cmd-R     Emacs search forward / backward; repeat to navigate + wrap.\n",
    "    Cmd-,             Open Settings tab; Manual edits aterm.toml.\n",
    "    Cmd-N / Cmd-T     New window (same process) / new tab (same window).\n",
    "    Cmd-W             Close focused pane -> tab -> window; last window quits aterm.\n",
    "    Cmd-Shift-T       Reopen most recently closed tab.\n",
    "    Cmd-Shift-] / [   Next / previous tab (wraps).   Cmd-1..9  Nth tab.\n",
    "    Cmd-Shift-P       Command Palette (every action, searchable).\n",
    "    Cmd-Opt-Arrow     Focus pane in that direction (no menu item).\n",
    "    The menu bar lists every action (splits, move-tab, and more) with its live chord.\n\n",
);

/// The non-macOS KEYS section, generated from
/// [`crate::keybinding::Keybindings::PLATFORM_DEFAULT_PAIRS`] (the table `platform_defaults()`
/// seeds) so it cannot drift from the window.
#[cfg(not(target_os = "macos"))]
fn keys_help() -> String {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for &(chord, action) in crate::keybinding::Keybindings::PLATFORM_DEFAULT_PAIRS {
        match groups.iter_mut().find(|(a, _)| *a == action) {
            Some((_, chords)) => chords.push(chord),
            None => groups.push((action, vec![chord])),
        }
    }
    let rows: Vec<(String, &str)> = groups
        .into_iter()
        .map(|(action, chords)| (chords.join(", "), action))
        .collect();
    let width = rows
        .iter()
        .map(|(chords, _)| chords.len())
        .max()
        .unwrap_or(0);
    let mut s = String::from(
        "KEYS (in the window; no menu bar off macOS, so these chords ARE the app menu —\n",
    );
    s.push_str("each row is a DEFAULT, rebindable via [keybindings]; see --list-keybinds):\n");
    for (chords, action) in rows {
        s.push_str(&format!("    {chords:<width$}  {action}\n"));
    }
    s.push_str("    ctrl+click  Open hyperlink, URL (http/https/mailto) or file:line citation;\n");
    s.push_str("                ctrl+alt+click where the program tracks the mouse.\n");
    s.push_str(
        "    shift+drag  Select text where the program tracks the mouse (alt+drag too).\n\n",
    );
    s
}

/// The macOS KEYS section, hand-written: macOS seeds no default chords (the menu bar owns them).
#[cfg(target_os = "macos")]
fn keys_help() -> String {
    KEYS_HELP.to_string()
}

const HELP_TAIL: &str = concat!(
    "CHILD-SHELL ENV HYGIENE:\n",
    "    Spawned shells strip AI-agent context variables before exec (CLAUDE*,\n",
    "    ANTHROPIC_*, COPILOT_*, CODEX_*, ANTIGRAVITY_*, GEMINI_*, CURSOR_*, AI_*,\n",
    "    _DEVTOOL_*) via aterm_types::env_sanitize so outer agent state never leaks in.\n\n",
    "CONFIG:  <aterm.toml>  (live settings reload; launch/session settings disclose their timing; precedence flag > config > default)\n",
    "  Appearance  font_px, font_family, theme (name, or dark:<name>,light:<name>),\n",
    "              foreground, background, cursor_color, selection_color,\n",
    "              selection_foreground, palette [array of #RRGGBB], window_theme,\n",
    "              tab_strip_rows.\n",
    "  Window/Tabs tab_status, tab_status_quiet_after_ms, tab_status_dwell_ms,\n",
    "              tab_status_badge, tab_connection_badge, spare_shell,\n",
    "              spare_shell_needs_person.\n",
    "  Cursor      serious_mode (mute sound/decorative effects), motion, cursor_style,\n",
    "              cursor_blink, cursor_momentum_glow, cursor_trail_style (default\n",
    "              rainbow kitty pet; off on Windows), cursor_trail_color/_accent/\n",
    "              _intensity/_radius/_ms/_length/_ring, cursor_trail_bloom\n",
    "              (+_strength/_radius).\n",
    "  Sound       trail_sounds (master), trail_sound_volume, trail_sound_style\n",
    "              (auto | music box | warm pluck | glitter | ice chime | droplet |\n",
    "              pew | zap | tick | crackle | mechanical | typewriter | marimba |\n",
    "              felt), tone_melody, trail_sound_bed, trail_sound_riff, bell_sound,\n",
    "              sparkle_words.profanity.bonk[_detonation].\n",
    "  Text        ligatures, font_features, bidi, ambiguous_width, text_blending\n",
    "              (linear-corrected | linear), font_thicken (macOS), stem_gamma,\n",
    "              font_hinting (Linux/Windows: full | light | off), font_subpixel\n",
    "              (Linux CPU: off | rgb | bgr), font_variation [\"wght=450\", ...],\n",
    "              font_weight, font_weight_dark_nudge, font_family_bold/_italic/\n",
    "              _bold_italic, font_synthetic_style, fallback_fonts [ordered],\n",
    "              symbol_font, emoji_font, line_height, font_size_scale.\n",
    "  Behaviour   gpu, scrollback_lines, columns, lines, copy_on_select,\n",
    "              option_as_meta, search_history_lines, focus_boost (Windows;\n",
    "              default on), conpty (Windows: bundled | inbox | auto | path),\n",
    "              desktop_alerts (aterm's own needs-you alerts as system\n",
    "              notifications; default off).\n",
    "  Security    allow_window_ops, allow_notifications, allow_kitty_file_transfer,\n",
    "              allow_osc52_query, secure_keyboard_entry (macOS) (default off);\n",
    "              allow_palette_reconfigure (default on).\n",
    "  Keys        [keybindings] \"chord\"=\"action\"; [key_sequences] \"chord\"=raw bytes.\n",
);

/// The placeholder in [`HELP_TAIL`]'s CONFIG line and [`STARTER_CONFIG`]'s header, filled at
/// print/write time with the path the loader resolves (`app_config::config_path`), since the
/// path differs by platform (Windows reads `%APPDATA%\aterm\aterm.toml`).
const CONFIG_PATH_SLOT: &str = "<aterm.toml>";

/// The variables the loader resolves the config path from, for the one message
/// that has to name them: none of them is set.
#[cfg(windows)]
const CONFIG_PATH_VARS: &str = "XDG_CONFIG_HOME, APPDATA and HOME";
#[cfg(not(windows))]
const CONFIG_PATH_VARS: &str = "XDG_CONFIG_HOME and HOME";

/// The resolved config path as `--help` prints it, or why there is none.
fn config_path_display() -> String {
    match crate::app_config::config_path() {
        Some(path) => path.display().to_string(),
        None => format!("(unresolved: {CONFIG_PATH_VARS} unset)"),
    }
}

/// [`HELP_TAIL`] with the config path filled in — what `--help` prints.
fn help_tail() -> String {
    HELP_TAIL.replacen(CONFIG_PATH_SLOT, &config_path_display(), 1)
}

/// [`STARTER_CONFIG`] headed by the path it is being written to.
fn starter_config_for(path: &std::path::Path) -> String {
    STARTER_CONFIG.replacen(CONFIG_PATH_SLOT, &path.display().to_string(), 1)
}

/// Windows-only verbs, appended to `--help` on Windows alone.
#[cfg(windows)]
const WINDOWS_HELP_TAIL: &str = concat!(
    "\nWINDOWS:\n",
    "    --install-context-menu     Add 'Open aterm here' to the Explorer right-click menu\n",
    "                               (per-user HKCU; --uninstall-context-menu removes it).\n",
    "    --unset-default-terminal   Clear aterm's default-terminal registration; another\n",
    "                               terminal's registration is left alone.\n",
);

/// The shell block of [`STARTER_CONFIG`], as this platform resolves a shell (see `shell_flag_help!`).
#[cfg(windows)]
macro_rules! starter_shell_block {
    () => {
        "\
# shell = \"bash\"        # interactive shell: \"bash\" (finds Git Bash even off PATH),
#                       # \"pwsh\", \"cmd\", \"wsl\", \"nu\", or a path. Unset = platform
#                       # default (pwsh > powershell > %COMSPEC% > cmd; `aterm doctor`
#                       # names it). Override with --shell; applies to new sessions.
#                       # Shell integration is injected for zsh/bash/fish/pwsh and
#                       # \"wsl\" (bash login); \"cmd\" is partial (no command text or
#                       # exit code); \"nu\" gets none.
# shell_args = [\"-l\", \"-i\"]  # extra argv after the shell (for \"wsl\", wsl.exe options)

"
    };
}
#[cfg(not(windows))]
macro_rules! starter_shell_block {
    () => {
        "\
# shell = \"bash\"        # interactive shell: a name on PATH (\"bash\", \"zsh\", \"fish\")
#                       # or a path. Unset = $SHELL, else login shell (`aterm doctor`
#                       # names it). Override with --shell; applies to new sessions.
#                       # Shell integration is injected for zsh, bash, fish and pwsh.
# shell_args = [\"-l\", \"-i\"]  # extra argv after the shell (macOS starts a login shell
#                       # already; elsewhere aterm starts an interactive non-login shell)

"
    };
}

/// The documented starter config `--write-config` writes: Linux-tuned, every line commented so
/// writing it changes nothing.
const STARTER_CONFIG: &str = concat!(
    "\
# aterm — <aterm.toml>
# Every setting is optional; uncomment to override. Live settings reload on save;
# renderer/initial-grid settings require relaunch, and session settings require a new session.
# Launch flags (aterm-gui --help) take precedence over this file for that launch.

# --- shell --------------------------------------------------------------------
",
    starter_shell_block!(),
    "\
# --- appearance ---------------------------------------------------------------
# font_family = \"JetBrains Mono\"  # any installed monospace family, or a .ttf path
# font_family_bold = \"JetBrains Mono Bold\"      # real bold face (unset = auto-discover)
# font_family_italic = \"JetBrains Mono Italic\"  # real italic face
# font_family_bold_italic = \"JetBrains Mono Bold Italic\"
# font_synthetic_style = true      # false: never fake bold/italic (regular when no real face)
# fallback_fonts = [\"Sarasa Mono\"] # ordered Unicode fallbacks, tried before built-in discovery
# symbol_font = \"Symbols Nerd Font\"   # monochrome symbol fallback
# emoji_font = \"Noto Color Emoji\"     # colour-emoji face
# font_px = 16                     # px at 1x (points on macOS); drawn x the display scale
# theme = \"Default\"               # a built-in scheme, or \"dark:<name>,light:<name>\"
# foreground = \"#C8D3F5\"
# background = \"#1A1B26\"
# cursor_style = \"block\"          # block | bar
# cursor_blink = true
# cursor_momentum_glow = true     # typing-speed cursor glow; warm = no blink (default ON, except on Windows)
# selection_color = \"#33415E\"
# selection_foreground = \"#FFFFFF\" # selected-text ink; unset = auto contrast floor
# selection_inactive = false       # dim selection band while window is unfocused
# split_focus_mark = true          # in a split, ink the divider edge of the focused pane
# scrollbar = true                 # overlay scrollbar in right gutter (default: Windows on, else off)
# window_colorspace = \"srgb\"       # macOS GPU CAMetalLayer tag: srgb (colour-managed) | display-p3 (legacy stretched)
# minimum_contrast = 1             # per-cell WCAG contrast floor, 1 (off) ..= 21; unset on Windows holds theme colours to 4.5:1
# background_opacity = 1           # macOS GPU window glass, 0 (transparent) ..= 1 (solid); <1 auto-floors contrast to 4.5:1
# background_material = \"none\"     # macOS vibrancy behind glass: none | hud | sidebar | under-window
# window_padding = 12              # interior padding, logical px per edge (0..=64; hot-applies)
# window_padding_top = 2           # top-edge override (0..=window_padding; titlebar band supplies the rest)
# bold_is_bright = true            # SGR bold promotes ANSI 0-7 to bright 8-15
# faint_opacity = 0.5              # SGR dim: fg fraction kept, blended toward bg
# scrollback_lines = 100000        # total history across ring + tiered store (default 100k, 0 = unlimited)
# tab_strip_rows = 1               # in-grid tab bar rows (default 1; 0 on macOS, whose toolbar carries tabs)

# --- tab subject & status -------------------------------------------------------
# Local session phase classification (running / quiet / idle / exited) from shell
# integration, foreground job state, and screen movement. No model, no network.
# tab_status = true                # classify session status (default ON; false stops the classifier)
# tab_status_quiet_after_ms = 5000 # silent foreground job becomes \"quiet\" after this (500..=120000)
# tab_status_dwell_ms = 750        # hysteresis before publishing a phase (0..=10000)
# tab_status_badge = true          # project status onto tab busy/attention marks
# tab_connection_badge = true      # mark tabs holding/receiving a session connection (▲ out / ▽ in / hourglass both)
# spare_shell = \"auto\"           # pre-spawned shell adopted by a new tab: auto | parked | off
# spare_shell_needs_person = true  # keep a spare only after a person's gesture in the last 10 min

# --- motion / cursor aurora -----------------------------------------------------
# serious_mode = false            # mute sounds + hide decorative effects; underlying settings return when off
# motion = \"auto\"                 # auto (Reduce Motion on macOS/Windows) | full | reduced
# load_adaptive_motion = true      # shed decorative effects under sustained render overload (smooth scroll stays on)
# cursor_trail_style = \"rainbow kitty pet\"  # cursor motion trail + light crown (DEFAULT; off on Windows):
#                                  #   rainbow kitty pet (aliases: \"rainbow kitty\", \"kitty\", \"rainbow kitty tall\")
#                                  #   | rainbow kitty underline | rainbow dog pet
#                                  #   | rainbow kitty flying (aliases: \"flying kitty\", \"kitty flying\", \"nyan rainbow\", \"nyan\", \"rainbow\")
#                                  #   | rainbow kitty flat (aliases: \"rainbow flat\", \"flat rainbow\", \"nyan flat\")
#                                  #   | phaser | fire | water | laser (alias: \"lightning\") | comet | sparkle | lumen | beam
#                                  #   | classic (v0.28 comet + jump trail; \"cursor\" colour = classic mono) | off
# cursor_trail_color = \"#50FA7B\"      # base colour (\"cursor\" uses live cursor colour; unset = style default or cursor colour)
# cursor_trail_accent = \"#7AA2F7\"     # comet-tail / ring colour (default: brightened base)
# cursor_trail_ms = 260                # fade duration in ms (30..=2000)
# cursor_trail_length = 24             # max comet length in cells (1..=512)
# cursor_trail_intensity = 1           # trail brightness 0..=1 (every style)
# cursor_trail_radius = 0.6            # bloom-crown radius in cells (0..=2)
# cursor_trail_ring = true             # expanding landing \"ping\" ring on a jump (default ON)
# --- sound (Settings > Sound) -------------------------------------------------
# trail_sounds = true              # master synth switch (typing notes, song, bed, chimes, bonk; macOS-only, default ON)
# trail_sound_volume = 0.4         # 0..=1 synth level (default 0.4 ~= -22 dBFS; does not scale bell_sound)
# trail_sound_style = \"auto\"     # typing sound: auto (follow trail style) | music box | warm pluck | glitter |
#                                  #   ice chime | droplet | pew | zap | tick | crackle | mechanical | typewriter |
#                                  #   marimba | felt (aliases: trail names, bell, raindrop, mech, thock, piano, clack)
# tone_melody = true               # subtle on-device mood inflection from typed input (default ON)
# trail_sound_bed = true           # continuous ambient bed texture behind notes (default ON)
# trail_sound_riff = true          # held-key sing-along song (default ON; false quiets song, keeps visuals)
# bell_sound = true                # audible BEL beep (macOS NSBeep / Windows MessageBeep / X11 bell; default ON)
# cursor_trail_bloom = true            # GPU-only soft halo around the comet (default ON)
# cursor_trail_bloom_strength = 0.85   # 0..=3 (halo intensity)
# cursor_trail_bloom_radius = 2.2      # 0.5..=8 half-res blur texels (default 2.2; 1.8 for rainbow kitty)
# cursor_fire_shimmer = true           # GPU-only heat-haze refraction above burning cells (default ON)
# hdr_glow = true                      # EDR cursor glow above SDR white (GPU + HDR panel, macOS EDR / Windows scRGB; default ON)
# cursor_glow_sdr_boost = 0.25         # GPU-only SDR crown strength 0..=1 (dark themes only; 0 = off)
# temporal_recording = false       # per-session replay spine (`aterm-ctl temporal [tick]`); default off, applies to new sessions

# --- text shaping -------------------------------------------------------------
# ligatures = true                 # programming ligatures (=>, !=, >=, ===, ...)
# cursor_break_ligatures = true    # break the cursor cell out of ligatures (default false)
# line_height = 1                  # cell-box multiplier 0.8..=2 (default 1.1 on Windows, 1 elsewhere)
# font_size_scale = 1              # multiplies font_px or auto size, 0.8..=1.25 (wins over theme)
# adjust_baseline = 0              # px baseline shift (±32) for off-metric faces
# adjust_underline_position = 0    # px underline shift (±32, + = down)
# adjust_underline_thickness = 0   # px underline thickness delta (±32)
# underline_skip_descenders = true # gap underline around descender ink
# font_features = [\"zero\", \"ss01\"] # OpenType features to force on
# bidi = \"implicit\"               # RTL reordering: implicit | disabled | explicit
# ambiguous_width = \"narrow\"       # East-Asian ambiguous width: narrow | wide
# text_blending = \"linear-corrected\" # AA weight: linear-corrected (native feel) | linear
# font_thicken = false             # macOS: CoreText font smoothing (heavier glyphs)
# stem_gamma = 1                   # aesthetic stem weight (<1 thicker, >1 thinner)
# font_hinting = \"full\"           # Linux/Windows grid fitting: full (default) | light | off
# font_subpixel = \"off\"           # Linux CPU subpixel-RGB text: off (default) | rgb | bgr (opaque frames only)
# font_variation = [\"wght=450\"]    # variable-font axes (clamped to fvar; default wght=400)
# font_weight = 450                # wght shorthand; wins over a font_variation wght entry
# font_weight_dark_nudge = 0       # extra wght on dark themes (applied only when grid-safe)

# --- behaviour ----------------------------------------------------------------
# gpu = false                      # GPU rendering is ON by default (auto CPU fallback); false or --cpu forces CPU next launch
# copy_on_select = true            # auto-copy mouse selection to CLIPBOARD (default on; off on Linux, where selection owns PRIMARY)
# show_build_badge = false         # floating top-right v<version>·<build> pill (default off; menu bar shows v<version>)
# confirm_multiline_paste = true   # confirm unbracketed multiline paste (sheet / dialog / banner)
# focus_boost = true               # Windows: boost visible shells' priority while focused (default on)
# conpty = \"bundled\"               # Windows console host for new tabs: \"bundled\" (default) | \"inbox\" | \"auto\" | path to OpenConsole.exe
# desktop_alerts = false           # also post aterm's own needs-you alerts as system notifications (default off;
#                                  #   macOS: UNUserNotificationCenter from Applications/aterm.app once authorized,
#                                  #   else terminal-notifier or osascript when run outside an app bundle).
#                                  #   Program OSC 9/99/777 notifications are gated by allow_notifications below

# --- program permissions (default OFF unless noted) --------------------------
# allow_window_ops = false         # XTWINOPS title/grid/pixel/cell-size reports (position/screen size stay unanswered);
#                                  #   Linux also applies window ops except move
# allow_notifications = false      # OSC 9/99/777 desktop notifications (default off; macOS: UNUserNotificationCenter
#                                  #   from Applications/aterm.app once authorized, else terminal-notifier/osascript;
#                                  #   Windows: Shell_NotifyIcon balloon/toast; Linux: D-Bus org.freedesktop.Notifications)
# allow_palette_reconfigure = true   # allow OSC 4/21 256-colour palette changes for the session (default ON)
# allow_kitty_file_transfer = false # allow Kitty file/temp-file/shm media to read host files (new sessions)
# allow_osc52_query = false        # allow programs to READ the clipboard via OSC 52 (triggers macOS paste alert)
# secure_keyboard_entry = false    # macOS: block other processes from observing keystrokes while frontmost

# --- sparkle words (purely visual; NEVER affects copied text, logs, or recordings)
# Decorates matched words: sparkle over profanity and cat-paw over cat/kitty words
# (both ON by default; master or per-category `enabled = false` silences them).
# [sparkle_words]
# enabled = true                   # master switch (default ON; false → byte-identical render)
# languages = [\"en\"]               # un-gate ambiguous homographs (fr \"chat\", de \"Kater\"); [\"all\"] = every language
# reduced_motion = false           # force the static, non-twinkling path
# suppress_in_alt_screen = false   # true → full-screen TUIs (vim/less/htop/claude) never decorated
# lexicon = \"~/.config/aterm/extra-lexicon.toml\"   # extra [[entry]] blocks merged over builtin
# toy_packs = [\"~/.config/aterm/toys/tiny-triumphs/pack.toml\"]  # strict community packs (max 8; later wins)
# deny = [\"scat\"]                  # never decorate these words, any category
# [sparkle_words.profanity]
# enabled = true                   # ON by default; false silences just profanity sparkle
# style = \"rainbow\"                # \"rainbow\" (default; supernova_chance escalates to supernova) | \"nova\" (<=2/s) | \"sparkle\"
# supernova_chance = 10            # rainbow-only escalation chance, percent (0..=100; 0 disables)
# magic = true                     # Quasar (1/512) / Singularity (1/1024) rare nova variants
# palette = [\"#ffd447\", \"#ff7ce5\", \"#7cf0ff\"]   # sparkle tints; empty → hue rotation
# density = 3                      # sparks per word per frame (1..=12)
# anim_ms = 2500                   # sparkle duration in ms (350..=10000)
# jitter = 2                       # sub-cell sparkle jitter in px (0..=6)
# intensity = 0.85                 # opacity 0..=1
# bonk = true                      # curse BONK sound on a typed curse (default ON; scaled by trail_sound_volume)
# bonk_detonation = false          # also bonk when an on-screen curse's supernova ignites (default OFF)
# extra_words = [\"frak\"]           # extra words to treat as profanity
# ignore_words = [\"fluff\"]         # never decorate these as profanity
# [sparkle_words.feline]
# enabled = true                   # default ON (requires master enabled)
# style = \"cat\"                    # \"cat\" (graphic) | \"paw\" (legacy ink-only)
# magic = true                     # Fortune (1/512) / Nebula (1/1024) rare cats
# allow_bare_cat = true            # default ON: decorate literal 3-letter \"cat\"
# cjk_single_char = false          # decorate lone 猫 anywhere
# log = true                       # record sightings into kitty-log.toml beside aterm.toml
# extra_words = [\"mittens\"]        # extra words to treat as feline
# ignore_words = [\"cats\"]          # never decorate these as feline
# [[sparkle_words.custom]]         # custom word effects (repeatable block)
# words = [\"ultrathink\"]           # matched surfaces (2+ chars; CJK ok)
# ink = { colorway = \"rainbow\" }   # or \"twotone:#RRGGBB,#RRGGBB\"; omit for no ink
# burst = { kind = \"starburst\", chance = 10 }   # sparkle|nova|supernova|starburst|glow
# graphic = { collection = \"cats\" }             # peeking cat on your word
# [sparkle_words.ink]              # animated glyph-ink shimmer
# enabled = true                   # requires sparkle_words.enabled
# strength = 0.75                  # ink tint vs original fg (0..=1)
# sweep_ms = 2200                  # one specular sweep window (350..=6000)
# loop = false                     # true: re-sweep while visible (floors sweep_ms to 600)
# [sparkle_words.emphasis]         # hype words — ink-only class
# enabled = true                   # no builtin words; populate via extra_words
# extra_words = [\"megathink\"]      # extra words to treat as emphasis
# ignore_words = [\"turbo\"]         # never decorate these as emphasis

# --- matrix rain (PHOSPHOR; purely visual — falls under text in empty cells only;
# copy/selection/search/recordings read exact bytes). OFF by default; per-session
# toggles (View > Matrix Rain, palette, `aterm-ctl rain`, `toggle_matrix_rain`)
# override `enabled` either way for that session's lifetime. -------------------
# [matrix_rain]
# enabled = false                  # default for every session (OFF; per-session toggles override)
# fps = 30                         # WORKING tick rate (12..=60; CALM always runs 12 Hz)
# density = 6                      # column density (1..=12)
# speed = 5                        # fall speed (1..=10; 5 = neutral)
# trail = 5                        # trail length (1..=10; 5 = neutral)
# alpha = 96                       # body coverage (16..=135); omit → derived from theme
# head_alpha = 135                 # bright-head coverage (alpha..=135); omit → derived
# hue = \"matrix\"                   # \"matrix\" | \"theme\" | \"#RRGGBB\" (bad hex → matrix green)
# mutation_ms = 133                # glyph mutation window in ms (80..=2000)
# idle_secs = 8                    # idle seconds until mandatory drain (2..=120)
# suppress_in_alt_screen = false   # true → fullscreen TUIs (vim/less/htop/claude) never rain
# output_material = true           # sample codepoints from real output (composer band protected)
# turn_wave = true                 # synchronized head sweep when agent turn completes
# bell_alert = true                # visual bell → 2 s constant-luminance amber hue ramp
# seed = 0                         # 0 = stable per-window field; nonzero = reproducible

# --- bundled ALab toolchain manager (atpkg): shared with `atpkg` (Settings ▸ Packages;
# inert in builds without a pinned root key). ----------------------------------
# [packages]
# enabled = true                   # keep ALab tools up to date (live reload; logs to packages.log)
# auto_install = true              # install default signed toolset (`uninstall`/`exclude`/`enabled=false` win)
# exclude = []                     # per-program opt-out from the signed default set
# [packages.links]                 # local dev-link overrides per program (skips registry):
# ay = \"~/src/ay\"
# [reroute]
# announce = true                  # announce cargo/rustc/tlc reroute before running upstream

# --- host settings (macOS only; applied daily and on package pass; `aterm pkg machine`)
# [machine]
# universal_control = \"off\"        # \"off\" (default: disable for this host) | \"leave\"
# rosetta = true                   # Apple silicon: install Rosetta 2 when missing (no password) | false

# --- system awareness: live Global / aterm / Tab stats bars (⌘I opens close-up)
# [sys]
# stats_bars = \"deck\"              # \"deck\" (default: under titlebar) | \"sill\" (bottom padding) | \"off\"
# stats_deck_size = \"auto\"         # \"auto\" (default: Full on tall window, Folded on short) | \"full\" | \"folded\"
# disk_roots = []                  # ~/Library or protected folders the disk walker may size

# --- input policy: map a chord to raw bytes sent to the program (macOS menu chords
# like Cmd-C are claimed by the menu first; use single-quoted '...' for \\e / \\xNN)
# [key_sequences]
# \"shift+enter\" = \"\\n\"        # send a literal newline (LF)
# 'f5' = '\\e[15~'              # ESC[15~
#
# --- keybindings (MUST be the LAST section: a TOML table runs to EOF) ---------
# [keybindings]
# \"ctrl+shift+t\" = \"new_tab\"
",
);

/// Pull the next argument as the value for `flag`, exiting 2 with a hint if it is
/// missing.
fn flag_value(flag: &str, args: &mut impl Iterator<Item = std::ffi::OsString>) -> String {
    match args.next() {
        Some(v) => v.to_string_lossy().into_owned(),
        None => {
            eprintln!("aterm: {flag} requires a value (try --help)");
            std::process::exit(2);
        }
    }
}

fn valid_font_px_flag(value: &str) -> bool {
    value
        .parse::<f32>()
        .is_ok_and(|px| px.is_finite() && (crate::FONT_PX_MIN..=crate::FONT_PX_MAX).contains(&px))
}

/// `value` as an initial grid dimension within `min..=max`, or `None`.
fn initial_dimension_flag(value: &str, min: u16, max: u16) -> Option<u16> {
    value
        .parse::<u16>()
        .ok()
        .filter(|dimension| (min..=max).contains(dimension))
}

/// Write a print-and-exit flag's answer to stdout and end the process with the flag's `code`, or
/// 1 when the answer could not be written.
fn answer_and_exit(argv: &[String], text: &str, code: i32) -> ! {
    use std::io::Write as _;
    let written = aterm_uds::stdout::write_all(text.as_bytes());
    let status = answer_status(written, code).unwrap_or_else(|line| {
        let _ = writeln!(std::io::stderr(), "{line}");
        1
    });
    #[cfg(windows)]
    if answered_onto_an_unwaited_console() {
        let _ = writeln!(std::io::stderr(), "{}", unwaited_answer_line(argv));
    }
    #[cfg(not(windows))]
    let _ = argv;
    std::process::exit(status)
}

/// The status a print-and-exit flag ends with: its own `code` when the answer was written or the
/// reader closed the pipe (`BrokenPipe`, which both Windows pipe-closed errors map to; aterm-ctl's
/// rule for `| head`), else the stderr line naming the failure, which exits 1.
fn answer_status(written: std::io::Result<()>, code: i32) -> Result<i32, String> {
    aterm_uds::stdout::answer_status("aterm-gui", written, code)
}

/// Whether this answer went straight onto a console this process attached to: the windowed
/// image (GUI subsystem) typed at a shell prompt with its output not redirected, the one shape
/// whose answer can land after the prompt.
#[cfg(windows)]
fn answered_onto_an_unwaited_console() -> bool {
    use std::io::IsTerminal as _;
    crate::win32::attached_a_console() && std::io::stdout().is_terminal()
}

/// The one stderr line under such an answer.
#[cfg(any(windows, test))]
fn unwaited_answer_line(argv: &[String]) -> String {
    let flags: Vec<String> = argv
        .iter()
        .map(|arg| {
            if arg.is_empty() || arg.chars().any(char::is_whitespace) {
                format!("\"{arg}\"")
            } else {
                arg.clone()
            }
        })
        .collect();
    format!(
        "aterm-gui: a pwsh or cmd prompt does not wait for the windowed image, so its answer \
         can print after the prompt; run `aterm --window {}` for one the prompt waits for",
        flags.join(" ")
    )
}

/// Ordinary window options shared by parsing and first-launch classification.
#[derive(Clone, Copy)]
enum OrdinaryWindowOption {
    WorkingDirectory,
    Hold,
    FontPx,
    Font,
    Scale,
    Gpu,
    Cpu,
    Shell,
    Columns,
    Lines,
    ShellIntegration,
    NoShellIntegration,
    Verbose,
}

impl OrdinaryWindowOption {
    fn parse(arg: &str) -> Option<Self> {
        Some(match arg {
            "-d" | "--working-directory" => Self::WorkingDirectory,
            "--hold" => Self::Hold,
            "--font-px" => Self::FontPx,
            "--font" => Self::Font,
            "--scale" => Self::Scale,
            "--gpu" => Self::Gpu,
            "--cpu" => Self::Cpu,
            "--shell" => Self::Shell,
            "--columns" => Self::Columns,
            "--lines" => Self::Lines,
            "--shell-integration" => Self::ShellIntegration,
            "--no-shell-integration" => Self::NoShellIntegration,
            "--verbose" => Self::Verbose,
            _ => return None,
        })
    }

    fn takes_value(self) -> bool {
        matches!(
            self,
            Self::WorkingDirectory
                | Self::FontPx
                | Self::Font
                | Self::Scale
                | Self::Shell
                | Self::Columns
                | Self::Lines
        )
    }
}

/// Whether a command line contains only ordinary window options.
#[must_use]
pub fn initial_setup_window_args(args: &[std::ffi::OsString]) -> bool {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let Some(option) = arg.to_str().and_then(OrdinaryWindowOption::parse) else {
            return false;
        };
        if option.takes_value() && args.next().is_none() {
            return false;
        }
    }
    true
}

/// CLI: `aterm-gui [OPTIONS] [-e CMD ARGS… | --help | --version]`.
pub(crate) fn parse_cli(os_argv: Vec<std::ffi::OsString>) -> Cli {
    let argv: Vec<String> = os_argv
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut args = os_argv.into_iter();
    let update_successor = std::env::var_os("ATERM_UPDATED_FROM").is_some();
    let mut cwd: Option<String> = None;
    let mut hold = false;
    let mut headless = false;
    let mut lifeline_fd: Option<String> = None;
    let mut ready_fd: Option<String> = None;
    let mut exec_command: Option<Vec<std::ffi::OsString>> = None;
    let mut launch = crate::launch::Launch::default();
    while let Some(arg) = args.next() {
        if let Some(option) = arg.to_str().and_then(OrdinaryWindowOption::parse) {
            match option {
                OrdinaryWindowOption::WorkingDirectory => {
                    let dir = flag_value("-d/--working-directory", &mut args);
                    if let Some(refusal) = start_dir_refusal(&dir, update_successor) {
                        eprintln!("aterm: {refusal}");
                        std::process::exit(2);
                    }
                    cwd = Some(dir);
                }
                OrdinaryWindowOption::Hold => hold = true,
                OrdinaryWindowOption::FontPx => {
                    let v = flag_value("--font-px", &mut args);
                    if valid_font_px_flag(&v) {
                        launch.font_px = v.parse().ok();
                    } else {
                        eprintln!(
                            "aterm: --font-px expects a number from {} through {}, got '{v}' (try --help)",
                            crate::FONT_PX_MIN,
                            crate::FONT_PX_MAX,
                        );
                        std::process::exit(2);
                    }
                }
                OrdinaryWindowOption::Font => {
                    let v = flag_value("--font", &mut args);
                    launch.font_family = (!v.trim().is_empty()).then_some(v);
                }
                OrdinaryWindowOption::Scale => {
                    let v = flag_value("--scale", &mut args);
                    match v.parse::<f64>() {
                        Ok(f) if f.is_finite() && f > 0.0 => launch.scale = Some(f),
                        _ => {
                            eprintln!(
                                "aterm: --scale expects a positive number, got '{v}' (try --help)"
                            );
                            std::process::exit(2);
                        }
                    }
                }
                OrdinaryWindowOption::Gpu => {
                    launch.renderer = Some(crate::launch::RendererFlag::Gpu);
                }
                OrdinaryWindowOption::Cpu => {
                    launch.renderer = Some(crate::launch::RendererFlag::Cpu);
                }
                OrdinaryWindowOption::Shell => {
                    let shell = flag_value("--shell", &mut args);
                    set_launch_flag(|f| f.shell = Some(shell));
                }
                OrdinaryWindowOption::Columns => {
                    let v = flag_value("--columns", &mut args);
                    if let Some(n) = initial_dimension_flag(&v, 20, 500) {
                        set_launch_flag(|f| f.columns = Some(n));
                    } else {
                        eprintln!(
                            "aterm: --columns expects an integer from 20 through 500, got '{v}' (try --help)"
                        );
                        std::process::exit(2);
                    }
                }
                OrdinaryWindowOption::Lines => {
                    let v = flag_value("--lines", &mut args);
                    if let Some(n) = initial_dimension_flag(&v, 5, 300) {
                        set_launch_flag(|f| f.lines = Some(n));
                    } else {
                        eprintln!(
                            "aterm: --lines expects an integer from 5 through 300, got '{v}' (try --help)"
                        );
                        std::process::exit(2);
                    }
                }
                OrdinaryWindowOption::ShellIntegration => {}
                OrdinaryWindowOption::NoShellIntegration => {
                    set_launch_flag(|f| f.no_shell_integration = true);
                }
                OrdinaryWindowOption::Verbose => {
                    set_launch_flag(|f| f.verbose = true);
                    aterm_gpu::set_verbose(true);
                }
            }
            continue;
        }
        match &*arg.to_string_lossy() {
            "-h" | "--help" => {
                let help = format!(
                    "{HELP_TITLE}{}\n{HELP_HEAD}{}{}",
                    aterm_types::identity::ORIGIN_LINE,
                    keys_help(),
                    help_tail()
                );
                #[cfg(windows)]
                let help = help + WINDOWS_HELP_TAIL;
                answer_and_exit(&argv, &help, 0);
            }
            "-V" | "--version" => {
                let version = format!("aterm-gui {}\n", crate::build_info::version_display());
                answer_and_exit(&argv, &version, 0);
            }
            "--diagnose" => {
                crate::launch::install(launch.clone());
                answer_and_exit(&argv, &crate::diagnostics::collect().render(), 0);
            }
            "--list-actions" => {
                let names: String = crate::keybinding::ACTION_NAMES
                    .iter()
                    .map(|name| format!("{name}\n"))
                    .collect();
                answer_and_exit(&argv, &names, 0);
            }
            "--validate-config" => {
                crate::launch::install(launch.clone());
                let (msg, ok) = crate::diagnostics::validate_config();
                answer_and_exit(&argv, &format!("{msg}\n"), i32::from(!ok));
            }
            "--list-fonts" => {
                answer_and_exit(&argv, &crate::diagnostics::list_fonts(), 0);
            }
            "--show-config" => {
                crate::launch::install(launch.clone());
                answer_and_exit(&argv, &crate::diagnostics::show_config(), 0);
            }
            "--write-config" => {
                let said = match crate::app_config::config_path() {
                    Some(path) if path.exists() => format!(
                        "config already exists: {} (left unchanged)\n",
                        path.display()
                    ),
                    Some(path) => {
                        if let Some(dir) = path.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        match std::fs::write(&path, starter_config_for(&path)) {
                            Ok(()) => {
                                format!("wrote a documented starter config: {}\n", path.display())
                            }
                            Err(e) => {
                                eprintln!("could not write {}: {e}", path.display());
                                std::process::exit(1);
                            }
                        }
                    }
                    None => {
                        eprintln!("could not resolve the config path ({CONFIG_PATH_VARS} unset)");
                        std::process::exit(1);
                    }
                };
                answer_and_exit(&argv, &said, 0);
            }
            "--list-keybinds" => {
                answer_and_exit(&argv, &crate::diagnostics::list_keybinds(), 0);
            }
            "--show-face" => {
                crate::launch::install(launch.clone());
                let family = args
                    .next()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let (msg, ok) = crate::diagnostics::show_face(&family);
                answer_and_exit(&argv, &msg, i32::from(!ok));
            }
            "--list-themes" => {
                let report = crate::diagnostics::theme_list_report(
                    None,
                    crate::diagnostics::ThemeListFormat::Text,
                );
                answer_and_exit(&argv, &report, 0);
            }
            #[cfg(windows)]
            "--install-context-menu" => {
                if let Err(e) = crate::explorer_win::install() {
                    eprintln!("aterm-gui: context-menu install failed: {e}");
                    std::process::exit(1);
                }
                answer_and_exit(
                    &argv,
                    "aterm-gui: installed the 'Open aterm here' Explorer context menu (per-user). \
                     Right-click a folder, its empty background, or a drive \
                     (on Windows 11 the entry appears under 'Show more options' / Shift+F10).\n",
                    0,
                );
            }
            #[cfg(windows)]
            "--uninstall-context-menu" => {
                let _ = crate::explorer_win::uninstall();
                answer_and_exit(
                    &argv,
                    "aterm-gui: removed the 'Open aterm here' Explorer context menu.\n",
                    0,
                );
            }
            #[cfg(windows)]
            "--set-default-terminal" => match crate::defterm_win::set_default_terminal() {
                Ok(()) => answer_and_exit(
                    &argv,
                    "aterm-gui: registered aterm as the Windows default terminal. \
                         New consoles (a double-clicked .bat, `Win+R cmd`, an installer's \
                         console) will open in aterm. Undo with --unset-default-terminal.\n",
                    0,
                ),
                Err(e) => {
                    eprintln!("aterm-gui: cannot become the default terminal: {e}");
                    let (console, terminal) =
                        crate::defterm_win::current_delegation().unwrap_or((None, None));
                    eprintln!(
                        "  current HKCU\\{}: {}={} {}={}{}",
                        crate::defterm_win::STARTUP_KEY,
                        crate::defterm_win::VALUE_CONSOLE,
                        console.as_deref().unwrap_or("(unset)"),
                        crate::defterm_win::VALUE_TERMINAL,
                        terminal.as_deref().unwrap_or("(unset)"),
                        if crate::defterm_win::is_aterm_default(terminal.as_deref()) {
                            "  <- already aterm"
                        } else {
                            ""
                        },
                    );
                    eprintln!(
                        "  set the default terminal in Settings > System > For developers > Terminal."
                    );
                    std::process::exit(1);
                }
            },
            #[cfg(windows)]
            "--unset-default-terminal" => {
                use crate::defterm_win::UnsetOutcome;
                let said = match crate::defterm_win::unset_default_terminal() {
                    Ok(UnsetOutcome::Removed) => "aterm-gui: removed aterm's default-terminal \
                                                  registration (consoles go back to the Windows \
                                                  default).\n"
                        .to_string(),
                    Ok(UnsetOutcome::NothingRegistered) => "aterm-gui: no default-terminal \
                                                            registration to remove (consoles \
                                                            already use the Windows default).\n"
                        .to_string(),
                    Ok(UnsetOutcome::NotOurs { console, terminal }) => format!(
                        "aterm-gui: nothing changed — the default terminal is not aterm's.\n  \
                         current HKCU\\{}: {}={} {}={}\n  change it in Settings > System > For \
                         developers > Terminal.\n",
                        crate::defterm_win::STARTUP_KEY,
                        crate::defterm_win::VALUE_CONSOLE,
                        console.as_deref().unwrap_or("(unset)"),
                        crate::defterm_win::VALUE_TERMINAL,
                        terminal.as_deref().unwrap_or("(unset)"),
                    ),
                    Err(e) => {
                        eprintln!("aterm-gui: could not clear the default-terminal keys: {e}");
                        std::process::exit(1);
                    }
                };
                answer_and_exit(&argv, &said, 0);
            }
            "--containment" => {
                let mode = flag_value("--containment", &mut args);
                set_launch_flag(|f| f.containment = Some(mode));
            }
            "--sandbox" => set_launch_flag(|f| f.containment = Some("containment".into())),
            "--no-sandbox" => set_launch_flag(|f| f.containment = Some("user".into())),
            "--control-sock" => {
                let sock = flag_value("--control-sock", &mut args);
                set_launch_flag(|f| f.control_sock = Some(sock));
            }
            "--no-control-sock" => set_launch_flag(|f| f.no_control_sock = true),
            "--headless" => headless = true,
            "--lifeline-fd" => lifeline_fd = Some(flag_value("--lifeline-fd", &mut args)),
            "--ready-fd" => ready_fd = Some(flag_value("--ready-fd", &mut args)),
            "-e" | "--command" => {
                let cmd: Vec<std::ffi::OsString> = args.by_ref().collect();
                if cmd.is_empty() {
                    eprintln!("aterm: -e/--command requires a command (try --help)");
                    std::process::exit(2);
                }
                exec_command = Some(cmd);
                break;
            }
            other => {
                let noun = if other.starts_with('-') {
                    "option"
                } else {
                    "command"
                };
                eprintln!("aterm: unknown {noun} '{other}' (try --help)");
                std::process::exit(2);
            }
        }
    }
    let lifeline_fd = lifeline_request(lifeline_fd.as_deref(), headless).unwrap_or_else(|why| {
        eprintln!("aterm: {why} (try --help)");
        std::process::exit(2);
    });
    let relaunch_of_running = std::env::var_os(aterm_update::RELAUNCH_OF_RUNNING_ENV).is_some();
    let ready_fd = ready_request(ready_fd.as_deref(), headless, relaunch_of_running)
        .unwrap_or_else(|why| {
            eprintln!("aterm: {why} (try --help)");
            std::process::exit(2);
        });
    Cli {
        exec_command,
        cwd,
        hold,
        headless,
        lifeline_fd,
        ready_fd,
        launch,
    }
}

/// The usage error `-d <dir>` stops a launch with, or `None` to take the folder.
fn start_dir_refusal(dir: &str, update_successor: bool) -> Option<String> {
    (!update_successor && !std::path::Path::new(dir).is_dir())
        .then(|| format!("not a directory: {dir}"))
}

/// The `--lifeline-fd` decision, pure: `Ok(None)` when the flag is absent, the descriptor number
/// when it is well-formed on a headless launch, and the usage error otherwise.
fn lifeline_request(value: Option<&str>, headless: bool) -> Result<Option<i32>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(fd) = value.parse::<i32>().ok().filter(|fd| *fd >= 0) else {
        return Err(format!(
            "--lifeline-fd expects a descriptor number, got '{value}'"
        ));
    };
    if !cfg!(unix) {
        return Err("--lifeline-fd is not supported on this platform".to_string());
    }
    if !headless {
        return Err("--lifeline-fd needs --headless (a window is a person's to close)".to_string());
    }
    Ok(Some(fd))
}

/// The `--ready-fd` decision, pure: `Ok(None)` when the flag is absent, the descriptor number
/// when it is well-formed on a headless launch, and the usage error otherwise — refused for a
/// window (a person sees it come up) and for 0-2 (the instance's own stdio).
fn ready_request(
    value: Option<&str>,
    headless: bool,
    relaunch_of_running: bool,
) -> Result<Option<i32>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(fd) = value.parse::<i32>().ok().filter(|fd| *fd > 2) else {
        return Err(format!(
            "--ready-fd expects a descriptor number above stdio (3 or more), got '{value}'"
        ));
    };
    if !cfg!(unix) {
        return Err("--ready-fd is not supported on this platform".to_string());
    }
    if !headless {
        return Err("--ready-fd needs --headless (a window is seen coming up)".to_string());
    }
    Ok((!relaunch_of_running).then_some(fd))
}

#[cfg(test)]
mod tests {
    #[test]
    fn setup_window_classification_consumes_values_before_scope_flags() {
        let classify = |args: &[&str]| {
            super::initial_setup_window_args(
                &args
                    .iter()
                    .map(std::ffi::OsString::from)
                    .collect::<Vec<_>>(),
            )
        };
        for args in [
            &["--font", "--headless"][..],
            &["--shell", "--control-sock"][..],
            &["-d", "--diagnose", "--gpu"][..],
        ] {
            assert!(classify(args), "option values keep their place: {args:?}");
        }
        for args in [
            &["--font", "Menlo", "--headless"][..],
            &["--gpu", "--containment", "user"][..],
            &["--gpu", "--control-sock", "off"][..],
            &["--font-px"][..],
            &["--shell"][..],
        ] {
            assert!(
                !classify(args),
                "a scope flag or missing value refuses setup: {args:?}"
            );
        }
    }

    /// A `-d` folder that has gone since the window opened stops a person's launch (exit 2) and
    /// never an update's relaunch of that window.
    #[test]
    fn a_vanished_start_folder_stops_a_launch_and_never_an_update_relaunch() {
        let gone = std::env::temp_dir().join("aterm-start-dir-that-is-not-there-r8-6");
        let gone = gone.to_string_lossy();
        assert!(
            super::start_dir_refusal(&gone, false).is_some_and(|why| why.contains(&*gone)),
            "a person's launch is refused, naming the folder"
        );
        assert_eq!(super::start_dir_refusal(&gone, true), None);
        let here = std::env::temp_dir();
        let here = here.to_string_lossy();
        assert_eq!(super::start_dir_refusal(&here, false), None);
        assert_eq!(super::start_dir_refusal(&here, true), None);
    }

    /// A reader that closed the pipe (Windows os errors 232 and 109 included) ends the answer
    /// quietly with the flag's own status; any other failure is said, with status 1.
    #[test]
    fn a_closed_pipe_ends_the_answer_quietly_and_any_other_failure_is_said() {
        use std::io::{Error, ErrorKind};
        assert_eq!(super::answer_status(Ok(()), 0), Ok(0));
        assert_eq!(
            super::answer_status(Ok(()), 1),
            Ok(1),
            "the flag's own verdict"
        );
        assert_eq!(
            super::answer_status(Err(Error::from(ErrorKind::BrokenPipe)), 0),
            Ok(0)
        );
        #[cfg(windows)]
        for (os_error, what) in [
            (232, "the pipe is being closed"),
            (109, "the pipe has been ended"),
        ] {
            assert_eq!(
                super::answer_status(Err(Error::from_raw_os_error(os_error)), 3),
                Ok(3),
                "{what} (os error {os_error}) is the reader going away"
            );
        }
        assert!(
            super::answer_status(Err(Error::from(ErrorKind::StorageFull)), 0).is_err(),
            "a full disk is a failure to report"
        );
    }

    /// The line under an answer that went straight onto a prompt's console names
    /// the command to type instead — the console image with the same flags — and
    /// keeps an argument with a space in it one argument.
    #[test]
    fn the_unwaited_answer_line_names_the_waited_for_spelling() {
        let line = super::unwaited_answer_line(&["--version".to_string()]);
        assert_eq!(
            line,
            "aterm-gui: a pwsh or cmd prompt does not wait for the windowed image, so its answer \
             can print after the prompt; run `aterm --window --version` for one the prompt waits \
             for"
        );
        let line =
            super::unwaited_answer_line(&["--show-face".to_string(), "Cascadia Code".to_string()]);
        assert!(
            line.contains("`aterm --window --show-face \"Cascadia Code\"`"),
            "{line}"
        );
    }

    /// `--headless` arms the mode through the `Cli` field alone: nothing is
    /// exported, so a child the window spawns inherits no headless request.
    #[test]
    fn the_headless_flag_is_recorded_and_exports_nothing() {
        let cli = super::parse_cli(vec![std::ffi::OsString::from("--headless")]);
        assert!(cli.headless);
        assert_eq!(cli.lifeline_fd, None);
        assert_eq!(cli.ready_fd, None);
    }

    /// `--ready-fd` arms only a headless launch, in either order, with a descriptor above stdio;
    /// an update's relaunch of a running instance drops the first image's number unread;
    /// everything else is a usage error that names the fix.
    #[test]
    fn the_ready_flag_arms_only_a_headless_launch() {
        use super::ready_request as req;
        assert_eq!(req(None, true, false), Ok(None));
        #[cfg(unix)]
        {
            assert_eq!(req(Some("9"), true, false), Ok(Some(9)));
            assert_eq!(
                req(Some("9"), true, true),
                Ok(None),
                "the first image's, unread"
            );
            let window = req(Some("9"), false, false).expect_err("a window is refused");
            assert!(window.contains("needs --headless"), "{window}");
        }
        for bad in ["0", "2", "-1", "x", ""] {
            let err = req(Some(bad), true, false).expect_err(bad);
            assert!(err.contains("above stdio"), "{err}");
        }
    }

    /// `-e`'s payload is handed on as the OS gave it: a raw byte that is not
    /// UTF-8 (`aterm --window -e prog $'\xff'`) stays that byte, never U+FFFD,
    /// and the arguments after it keep their places.
    #[cfg(unix)]
    #[test]
    fn the_command_payload_keeps_the_bytes_it_was_given() {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
        let raw = std::ffi::OsString::from_vec(vec![0xff, b'a', 0x80]);
        let cli = super::parse_cli(vec!["-e".into(), "printf".into(), raw, "c".into()]);
        let bytes: Vec<Vec<u8>> = cli
            .exec_command
            .expect("the -e payload")
            .iter()
            .map(|a| AsRef::<std::ffi::OsStr>::as_ref(a).as_bytes().to_vec())
            .collect();
        assert_eq!(
            bytes,
            [b"printf".to_vec(), vec![0xff, b'a', 0x80], b"c".to_vec()]
        );
    }

    /// `--lifeline-fd` arms only a headless launch, in either order, with a
    /// descriptor number; everything else is a usage error that names the fix —
    /// and the `-e` payload after it is still the payload.
    #[test]
    fn the_lifeline_flag_arms_only_a_headless_launch() {
        use super::lifeline_request as req;
        assert_eq!(req(None, true), Ok(None));
        assert_eq!(req(None, false), Ok(None));
        #[cfg(unix)]
        {
            assert_eq!(req(Some("0"), true), Ok(Some(0)));
            assert_eq!(req(Some("7"), true), Ok(Some(7)));
            let window = req(Some("0"), false).expect_err("a window is refused");
            assert!(window.contains("needs --headless"), "{window}");
            let os = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
            for argv in [
                os(&["--lifeline-fd", "0", "--headless", "-e", "sh"]),
                os(&["--headless", "--lifeline-fd", "0", "-e", "sh"]),
            ] {
                let cli = super::parse_cli(argv);
                assert_eq!(cli.lifeline_fd, Some(0));
                assert_eq!(cli.exec_command, Some(vec!["sh".into()]));
            }
        }
        for bad in ["-1", "x", ""] {
            let err = req(Some(bad), true).expect_err(bad);
            assert!(err.contains("expects a descriptor number"), "{err}");
        }
    }

    /// The socket directive of the launch flags is the shared one: a path binds
    /// there, `0`/`off` and `--no-control-sock` disable, nothing is per-instance.
    #[test]
    fn launch_flags_decide_the_socket_like_every_client() {
        use aterm_types::control_socket::SocketDirective as D;
        let with = |sock: Option<&str>, off: bool| super::LaunchFlags {
            control_sock: sock.map(str::to_string),
            no_control_sock: off,
            ..super::LaunchFlags::default()
        };
        assert_eq!(with(None, false).socket_directive(), D::PerInstance);
        assert_eq!(
            with(Some("/r/a.sock"), false).socket_directive(),
            D::Explicit("/r/a.sock".to_string())
        );
        assert_eq!(with(Some("off"), false).socket_directive(), D::Disabled);
        assert_eq!(
            with(Some("/r/a.sock"), true).socket_directive(),
            D::Disabled
        );
    }

    #[test]
    fn font_px_flag_accepts_exact_runtime_domain_only() {
        for accepted in ["6", "12.5", "200"] {
            assert!(super::valid_font_px_flag(accepted), "{accepted}");
        }
        for rejected in ["5.99", "201", "500", "NaN", "inf", "nope"] {
            assert!(!super::valid_font_px_flag(rejected), "{rejected}");
        }
    }

    #[test]
    fn initial_dimension_flags_accept_exact_documented_domains_only() {
        for accepted in ["20", "80", "500"] {
            assert!(
                super::initial_dimension_flag(accepted, 20, 500).is_some(),
                "columns {accepted}"
            );
        }
        for rejected in ["0", "1", "19", "501", "65536", "nope"] {
            assert!(
                super::initial_dimension_flag(rejected, 20, 500).is_none(),
                "columns {rejected}"
            );
        }
        for accepted in ["5", "24", "300"] {
            assert!(
                super::initial_dimension_flag(accepted, 5, 300).is_some(),
                "lines {accepted}"
            );
        }
        for rejected in ["0", "1", "4", "301", "65536", "nope"] {
            assert!(
                super::initial_dimension_flag(rejected, 5, 300).is_none(),
                "lines {rejected}"
            );
        }
    }

    /// `--cpu` / `--gpu` are symmetric, the last one wins, and neither — nor any
    /// other launch flag — writes the environment: the render/font flags reach
    /// their readers through `Cli::launch` alone.
    #[test]
    fn render_flags_ride_the_launch_struct_and_the_last_renderer_flag_wins() {
        use crate::launch::RendererFlag;
        let parse = |args: &[&str]| {
            super::parse_cli(args.iter().map(std::ffi::OsString::from).collect()).launch
        };
        assert_eq!(parse(&["--cpu", "--gpu"]).renderer, Some(RendererFlag::Gpu));
        assert_eq!(parse(&["--gpu", "--cpu"]).renderer, Some(RendererFlag::Cpu));
        assert_eq!(parse(&[]), crate::launch::Launch::NONE);
        let all = parse(&["--font-px", "24", "--font", "Menlo", "--scale", "2"]);
        assert_eq!(all.font_px, Some(24.0));
        assert_eq!(all.font_family.as_deref(), Some("Menlo"));
        assert_eq!(all.scale, Some(2.0));
        assert_eq!(
            parse(&["--font", "  "]).font_family,
            None,
            "a blank family is no pin"
        );
    }

    /// Unset is never gated on the handoff server: clearing `HKCU\Console\%%Startup` is the
    /// escape hatch from a delegation to a class nothing can create.
    #[cfg(windows)]
    #[test]
    fn unset_default_terminal_is_never_gated_while_set_refuses() {
        assert!(
            !crate::defterm_win::handoff_server_available(),
            "no COM handoff server ships in this build yet"
        );
        let set_err = crate::defterm_win::set_default_terminal()
            .expect_err("set must refuse while nothing can answer the handoff");
        assert_eq!(set_err.kind(), std::io::ErrorKind::Unsupported);

        let before = crate::defterm_win::current_delegation().expect("read");
        crate::defterm_win::unset_default_terminal()
            .expect("unset must not fail on the live machine, gated or not");
        let after = crate::defterm_win::current_delegation().expect("read");
        assert_eq!(
            before, after,
            "unset must not touch a delegation aterm does not own"
        );
    }

    /// `--help`'s CONFIG line and the starter's header name the path the loader resolves on this
    /// machine, never a literal (Windows reads `%APPDATA%\aterm\aterm.toml`).
    #[test]
    fn help_and_starter_name_the_resolved_config_path() {
        assert!(
            super::HELP_TAIL.contains("CONFIG:  <aterm.toml>  (live settings reload"),
            "the CONFIG line carries the slot, not a literal path"
        );
        assert!(
            super::STARTER_CONFIG.starts_with("# aterm — <aterm.toml>\n"),
            "the starter's header carries the slot, not a literal path"
        );
        let tail = super::help_tail();
        assert!(!tail.contains(super::CONFIG_PATH_SLOT), "{tail}");
        let shown = super::config_path_display();
        assert!(
            tail.contains(&format!("CONFIG:  {shown}  (live settings reload")),
            "{tail}"
        );
        let Some(path) = crate::app_config::config_path() else {
            assert!(shown.starts_with("(unresolved: "), "{shown}");
            return;
        };
        assert_eq!(shown, path.display().to_string());
        assert!(path.ends_with("aterm.toml"), "{}", path.display());
        let starter = super::starter_config_for(&path);
        assert_eq!(
            starter.lines().next(),
            Some(format!("# aterm — {}", path.display()).as_str())
        );
        assert!(!starter.contains(super::CONFIG_PATH_SLOT), "{starter}");
        assert_eq!(
            starter.lines().count(),
            super::STARTER_CONFIG.lines().count(),
            "only the header changes"
        );
    }

    #[test]
    fn starter_config_keys_all_deserialize() {
        use crate::app_config::Config;
        let mut in_table = false;
        let mut checked: Vec<String> = Vec::new();
        for raw in super::STARTER_CONFIG.lines() {
            let line = raw.trim_start();
            let Some(rest) = line.strip_prefix('#') else {
                if line.starts_with('[') {
                    in_table = true;
                }
                continue;
            };
            let rest = rest.trim_start();
            if rest.starts_with('[') {
                in_table = true;
                continue;
            }
            let key = rest.split('=').next().map(str::trim).filter(|k| {
                !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
            if !in_table
                && rest.contains('=')
                && let Some(key) = key
            {
                assert!(
                    aterm_toml::from_str::<Config>(rest).is_ok(),
                    "STARTER_CONFIG ships an uncommentable key — uncommenting it would \
                     abort the config load and revert to defaults:\n  {rest}"
                );
                checked.push(key.to_string());
            }
        }
        let mut numbers = 0;
        for raw in super::STARTER_CONFIG.lines() {
            let Some(rest) = raw.trim_start().strip_prefix('#') else {
                continue;
            };
            let Some((key, value)) = rest.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let literal = value.split('#').next().unwrap_or_default().trim();
            let Some(number) = literal
                .parse::<i64>()
                .ok()
                .map(|n| n as f64)
                .or_else(|| literal.parse::<f64>().ok())
            else {
                continue;
            };
            assert_eq!(
                literal,
                crate::prefs::written_number(number).to_string().trim(),
                "STARTER_CONFIG spells `{key}` as aterm never writes it"
            );
            numbers += 1;
        }
        assert!(numbers > 30, "only {numbers} numbers checked");
        for must in [
            "allow_window_ops",
            "allow_notifications",
            "allow_palette_reconfigure",
            "allow_kitty_file_transfer",
            "allow_osc52_query",
            "secure_keyboard_entry",
        ] {
            assert!(
                checked.iter().any(|k| k == must),
                "STARTER_CONFIG key `{must}` was not validated — is it buried under a \
                 `[table]` header? Keep all bare top-level keys ABOVE every table."
            );
        }
    }

    /// The `[matrix_rain]` starter block is table-scoped, so the line-by-line check above skips
    /// it: uncomment the whole block and parse it as one document.
    #[test]
    fn starter_config_matrix_rain_block_deserializes() {
        use crate::app_config::Config;
        let mut block = String::new();
        let mut in_rain = false;
        for raw in super::STARTER_CONFIG.lines() {
            let Some(rest) = raw.trim_start().strip_prefix('#') else {
                continue;
            };
            let rest = rest.trim_start();
            if rest.starts_with('[') {
                in_rain = rest.starts_with("[matrix_rain]");
                if in_rain {
                    block.push_str("[matrix_rain]\n");
                }
                continue;
            }
            let key_ok = rest.split('=').next().map(str::trim).is_some_and(|k| {
                !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
            if in_rain && rest.contains('=') && key_ok {
                block.push_str(rest);
                block.push('\n');
            }
        }
        let parsed: Config = aterm_toml::from_str(&block).unwrap_or_else(|e| {
            panic!(
                "STARTER_CONFIG ships an uncommentable [matrix_rain] line — \
                 uncommenting the block would abort the config load:\n{block}\n{e}"
            )
        });
        let mr = parsed
            .matrix_rain
            .expect("the assembled block populates the [matrix_rain] table");
        assert!(mr.enabled.is_some(), "starter documents `enabled`");
        assert!(mr.fps.is_some(), "starter documents `fps`");
        assert!(mr.density.is_some(), "starter documents `density`");
        assert!(mr.speed.is_some(), "starter documents `speed`");
        assert!(mr.trail.is_some(), "starter documents `trail`");
        assert!(mr.alpha.is_some(), "starter documents `alpha`");
        assert!(mr.head_alpha.is_some(), "starter documents `head_alpha`");
        assert!(mr.hue.is_some(), "starter documents `hue`");
        assert!(mr.mutation_ms.is_some(), "starter documents `mutation_ms`");
        assert!(mr.idle_secs.is_some(), "starter documents `idle_secs`");
        assert!(
            mr.suppress_in_alt_screen.is_some(),
            "starter documents `suppress_in_alt_screen`"
        );
        assert!(mr.turn_wave.is_some(), "starter documents `turn_wave`");
        assert!(mr.bell_alert.is_some(), "starter documents `bell_alert`");
        assert!(mr.seed.is_some(), "starter documents `seed`");
        assert_eq!(mr.enabled, Some(false), "the starter example ships OFF");
    }
}
