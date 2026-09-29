// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI argument parsing for `aterm-gui`. Pure, no `App` coupling: parses
//! `aterm-gui [OPTIONS] [-e CMD ARGS… | --help | --version]`. A launch flag is
//! recorded — the render/font flags (`--cpu`/`--gpu`, `--font`, `--font-px`,
//! `--scale`) in [`crate::launch`], the rest in [`LaunchFlags`], process state the
//! rest of the window reads — and never exported: no environment variable changes
//! what a shipped aterm does (owner, 2026-09-22: "NOT ENV VARS those are for
//! development"), so the flag is the one spelling and a child process inherits
//! nothing from it.

use std::sync::RwLock;

/// Parsed CLI: the `-e` command to run instead of `$SHELL` (if any), the
/// `--working-directory` to start it in (if any), whether to `--hold` the
/// window open after the command exits, and whether `--headless` was passed.
pub(crate) struct Cli {
    pub(crate) exec_command: Option<Vec<String>>,
    pub(crate) cwd: Option<String>,
    pub(crate) hold: bool,
    /// `--headless`: no window is ever created — engine + PTY + control socket
    /// only — and on macOS the process cannot be activated either (no Dock tile,
    /// never the front app; `launch_posture` in `lib.rs`), so a harness may boot
    /// one beside a human who is typing. The flag is the one spelling; an update
    /// successor inherits it with the rest of argv.
    pub(crate) headless: bool,
    /// `--lifeline-fd <n>` (headless only): the descriptor whose end-of-file
    /// means the process that started this instance is gone, so it shuts down
    /// ([`crate::lifeline`]). `None` — every launch that does not pass the flag —
    /// changes nothing.
    pub(crate) lifeline_fd: Option<i32>,
    /// The render/font flags, for `main` to [`crate::launch::install`].
    pub(crate) launch: crate::launch::Launch,
}

/// THE LAUNCH FLAGS — what this process's command line asked of it, recorded by
/// [`parse_cli`] before any thread exists and read wherever the window needs it
/// (the socket plan, the initial grid, the shell, the containment funnel).
///
/// They used to be ENVIRONMENT variables the parser wrote back (`--columns` set
/// `$ATERM_COLUMNS`, and every `ATERM_*` spelling worked on its own), so an export
/// in a shell rc changed what a shipped window did, and a flag leaked into every
/// child that did not strip it. A flag now lives here and nowhere else.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LaunchFlags {
    /// `--columns <n>` (20..=500, validated on the way in).
    pub(crate) columns: Option<u16>,
    /// `--lines <n>` (5..=300, validated on the way in).
    pub(crate) lines: Option<u16>,
    /// `--shell <name|path>`: outranks the config `shell` for every new tab.
    pub(crate) shell: Option<String>,
    /// `--containment <mode>` / `--sandbox` / `--no-sandbox`, as typed: the parse,
    /// and its fail-closed fallback, are the launch funnel's in `main`.
    pub(crate) containment: Option<String>,
    /// `--control-sock <path|0|off>` / `--no-control-sock`: the socket
    /// directive's two inputs ([`aterm_types::control_socket::socket_directive`]).
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

    /// The control-socket directive these flags ask for — the SAME decision a
    /// client makes about a socket path ([`aterm_types::control_socket::socket_directive`]).
    pub(crate) fn socket_directive(&self) -> aterm_types::control_socket::SocketDirective {
        aterm_types::control_socket::socket_directive(
            self.control_sock.as_deref(),
            self.no_control_sock.then_some("1"),
        )
    }
}

/// The process's launch flags. Written only by [`parse_cli`] (single-threaded
/// startup, so a diagnostic verb later on the same command line — `--columns 120
/// --show-config` — reports what the flags before it asked for).
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

/// The `--help` text. A clean OPTIONS section where every user-facing flag shows
/// its argument, a one-line description, AND its `[config: key]` equivalent where
/// it has one — the discoverable surface an AI (or human) reads to drive aterm
/// without source-diving. Kept in constants printed only by the `--help` arm, so a
/// no-arg / Finder launch never touches them. There is no ENVIRONMENT section: no
/// variable changes what a shipped aterm does, so precedence is flag > config >
/// default.
const HELP_TITLE: &str = "aterm-gui — a fast, hardened terminal\n";
const HELP_HEAD: &str = concat!(
    "\n",
    "USAGE:\n",
    "    aterm-gui [OPTIONS]\n",
    "    aterm-gui [-d <dir>] -e <command> [args...]\n\n",
    "OPTIONS:\n",
    "    -e, --command <cmd> [args...]  Run <cmd> in the terminal instead of $SHELL;\n",
    "                                   the window closes when it exits. Consumes the\n",
    "                                   rest of the command line.\n",
    "    -d, --working-directory <dir>  Start the shell/command in <dir>.\n",
    "        --hold                     Keep the window open after the -e command\n",
    "                                   exits (close it manually).\n",
    "        --font-px <px>             Glyph size in physical px (6..=200) for this\n",
    "                                   launch.                 [config: font_px]\n",
    "        --font <name|path>         Primary font FAMILY (e.g. \"JetBrains Mono\") or\n",
    "                                   font file for this launch. [config: font_family]\n",
    "        --shell <name|path>        Interactive shell to spawn. Discovery-resolved:\n",
    "                                   \"bash\" finds Git Bash even off PATH; \"pwsh\",\n",
    "                                   \"cmd\", \"wsl\", \"nu\", or an absolute path also work.\n",
    "                                   Shell integration (prompt marks, jump-to-prompt,\n",
    "                                   command blocks, cwd) is injected for zsh/bash/fish/\n",
    "                                   pwsh and \"wsl\" (bash login shell). \"cmd\" is partial:\n",
    "                                   prompt marks, jump-to-prompt and cwd work, but blocks\n",
    "                                   carry no command text or exit code. \"nu\" gets none.\n",
    "                                       [config: shell]\n",
    "        --scale <f>                Force the render scale factor (font + padding).\n",
    "                                   In a window this overrides the display scale;\n",
    "                                   headless it makes the `image` capture render at\n",
    "                                   that DPI (e.g. --scale 2 ≈ a 2× Retina window).\n",
    "        --gpu                      Force GPU rendering — the DEFAULT (Metal on macOS,\n",
    "                                   wgpu elsewhere; auto CPU fallback). [config: gpu]\n",
    "        --cpu                      Force the CPU renderer (overrides config; the\n",
    "                                   last of --cpu/--gpu wins).\n",
    "        --containment <mode>       Containment mode: master|user|safety|containment\n",
    "                                   (an invalid value fails closed to containment).\n",
    "        --sandbox                  Shorthand for --containment containment.\n",
    "        --no-sandbox               Shorthand for --containment user.\n",
    "        --control-sock <path>      Bind the control socket at <path> (or 0/off to\n",
    "                                   disable); clients reach it with --sock <path>.\n",
    "        --no-control-sock          Disable the control socket.\n",
    "        --headless                 No window; engine + control socket only. The\n",
    "                                   launch announces the mode on stderr.\n",
    "        --lifeline-fd <n>          With --headless: shut down when descriptor <n>\n",
    "                                   (a pipe's read end) reads end-of-file — the\n",
    "                                   launcher that holds its write end is gone. For\n",
    "                                   harnesses: a killed test run takes its instance\n",
    "                                   with it.\n",
    "        --columns <n>              Initial width in columns (20..=500).\n",
    "        --lines <n>                Initial height in rows (5..=300).\n",
    "        --shell-integration        OSC 133/633 command marks (blocks/cwd/title) — ON by\n",
    "                                       default; this flag is a no-op.\n",
    "        --no-shell-integration     Disable shell-integration marks (default is on).\n",
    "        --verbose                  Verbose diagnostics on stderr.\n",
    "        --diagnose                 Print a diagnostics report (version, build,\n",
    "                                   renderer, capabilities, config, env) and exit.\n",
    "        --list-actions             List the bindable [keybindings] action names\n",
    "                                   and exit.\n",
    "        --validate-config          Parse the config file, report OK/errors, exit\n",
    "                                   0 if valid (non-zero if not).\n",
    "        --list-fonts               List the font search dirs and discoverable\n",
    "                                   font families, then exit.\n",
    "        --show-config              Print the effective resolved config (flag >\n",
    "                                   config > default) and exit.\n",
    "        --write-config             Write a documented starter aterm.toml (every\n",
    "                                   key commented) if absent, then exit.\n",
    "        --list-keybinds            List built-in + configured [keybindings] and\n",
    "                                   [key_sequences], plus bindable actions, then exit.\n",
    "        --show-face [family]       Print the resolved font face (path + metrics)\n",
    "                                   for [family] (or the configured font) and exit.\n",
    "        --list-themes              List the built-in and custom colour themes\n",
    "                                   and exit.\n",
    "    -h, --help                     Print this help and exit.\n",
    "    -V, --version                  Print the version and exit.\n\n",
);

/// The keyboard-shortcut help, PER PLATFORM: macOS shows the hardcoded Cmd-* chords;
/// every other platform shows the Ctrl+Shift defaults seeded by
/// [`crate::keybinding::Keybindings::platform_defaults`] (there is no Cmd key, and
/// the Super key is grabbed by the desktop environment).
#[cfg(target_os = "macos")]
const KEYS_HELP: &str = concat!(
    "KEYS (in the window):\n",
    "    Cmd-C / Cmd-V     Copy selection / paste (control-stripped, bracketed).\n",
    "    Cmd-= / Cmd--     Zoom the font in / out.   Cmd-0  Reset zoom.\n",
    "    Cmd-click         Open a hyperlink / detected URL (http/https/mailto);\n",
    "                      Cmd-Option-click where the program tracks the mouse.\n",
    "    Cmd-F             Find (screen + scrollback): type, Enter/Shift-Enter, Esc.\n",
    "    Cmd-S / Cmd-R     Emacs search forward / backward; repeat to navigate + wrap.\n",
    "    Cmd-,             Open the native Settings tab; Manual edits aterm.toml.\n",
    "    Cmd-N             Open a new window (same process, same sessions).\n",
    "    Cmd-T             Open a new tab (new shell, same window).\n",
    "    Cmd-W             Close the focused pane; the last pane closes the tab,\n",
    "                      and the last tab its window; the last window quits aterm.\n",
    "    Cmd-Shift-T       Reopen the most recently closed tab.\n",
    "    Cmd-Shift-] / [   Next / previous tab (wraps).   Cmd-1..9  Nth tab.\n",
    "    Cmd-Shift-P       Command Palette (every action, searchable).\n",
    "    Cmd-Opt-Arrow     Move focus to the pane in that direction (no menu item).\n",
    "    This is the common set; the menu bar lists them ALL (splits, move-tab, and\n",
    "    more) with their live chords.\n\n",
);

/// See [`KEYS_HELP`] (macOS) — the non-macOS KEYS section, GENERATED from
/// [`crate::keybinding::Keybindings::PLATFORM_DEFAULT_PAIRS`], the same const
/// `platform_defaults()` seeds, so this help can never drift from what the
/// window actually does. (It used to: the hand-written list it replaces had
/// fallen behind the table — no palette, no splits, no prompt jumps, none of
/// the plain-Ctrl paste trio — and was headed "Linux has no menu bar" on
/// Windows.) Chords are grouped per action in table order, so paste's three
/// spellings read as one row; the chord strings are printed VERBATIM because
/// they are exactly what a `[keybindings]` override key must say.
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
    // Not a chord, so not in the table: the pointer half of the keymap.
    s.push_str("    ctrl+click  Open a hyperlink / detected URL (http/https/mailto);\n");
    s.push_str("                ctrl+alt+click where the program tracks the mouse.\n\n");
    s
}

/// The macOS KEYS section: the hardcoded Cmd-* chords, hand-written above —
/// there is no seed table to generate from (macOS ships an empty
/// `platform_defaults()`; the menu bar owns these chords).
#[cfg(target_os = "macos")]
fn keys_help() -> String {
    KEYS_HELP.to_string()
}

const HELP_TAIL: &str = concat!(
    "CHILD-SHELL ENV HYGIENE:\n",
    "    The spawned shell has every AI-agent context variable STRIPPED before exec —\n",
    "    CLAUDE*, ANTHROPIC_*, COPILOT_*, CODEX_*, CURSOR_*, AI_*, and _DEVTOOL_* — so they\n",
    "    never leak into your session. An inner agent whose context vars went missing was\n",
    "    sanitized here by design (aterm_types::env_sanitize), not lost. The one exception:\n",
    "    a session spawned with `aterm ctl spawn identity=<name>` gets each agent's home\n",
    "    variable (CLAUDE_CONFIG_DIR, CODEX_HOME) pointed into <state>/identities/<name>/ —\n",
    "    set AFTER the strip, so neither your login nor the identity's leaks into the other.\n\n",
    "CONFIG:  <aterm.toml>  (live settings reload; launch/session settings disclose their timing; precedence flag > config > default)\n",
    "  Appearance  font_px, font_family, theme (name, or dark:<name>,light:<name>),\n",
    "              foreground, background, cursor_color, selection_color,\n",
    "              selection_foreground,\n",
    "              palette [array of #RRGGBB], window_theme, tab_strip_rows,\n",
    "              robi (the tip-sharing helper robot; type robi to summon him).\n",
    "  Window/Tabs descriptive_titles, title_summary_provider, title_summary_model,\n",
    "              title_summary_endpoint, title_summary_token_file,\n",
    "              title_summary_timeout_seconds, title_summary_proxy_mode,\n",
    "              title_summary_ca_file,\n",
    "              title_summary_interval_seconds,\n",
    "              title_summary_context_lines, title_summary_include_output,\n",
    "              title_summary_allow_remote, tab_title_format, window_title_format,\n",
    "              tab_status, tab_status_quiet_after_ms, tab_status_dwell_ms,\n",
    "              tab_status_badge, tab_connection_badge.\n",
    "  Cursor      serious_mode (mute all sound/decorative effects), motion,\n",
    "              cursor_style, cursor_blink, cursor_momentum_glow (typing-speed glow),\n",
    "              cursor_trail, cursor_trail_style\n",
    "              (the LUMEN aurora), cursor_trail_color/_accent/_intensity/_radius,\n",
    "              cursor_trail_ms/_length/_ring, cursor_trail_bloom (+_strength/_radius).\n",
    // Sound is its own help block because it is its own Settings box now (the
    // owner's Sound menu); listing it under Cursor is what made the volume dial
    // hard to find in the first place.
    "  Sound       trail_sounds (master), trail_sound_volume (scales every synth\n",
    "              voice), trail_sound_style (the typing sound: auto | music box |\n",
    "              warm pluck | glitter | ice chime | droplet | pew | zap | tick |\n",
    "              crackle | mechanical | typewriter | marimba | felt), tone_melody,\n",
    "              trail_sound_bed (the ambient texture; default on),\n",
    "              trail_sound_riff (the sing-along song — the loudest voice),\n",
    "              bell_sound (the audible BEL beep; macOS/Windows),\n",
    "              choice_sound (the chime when the supervisor answers a question),\n",
    "              sparkle_words.profanity.bonk[_detonation] (the curse bonk).\n",
    "  Text        ligatures, font_features, bidi, ambiguous_width,\n",
    "              text_blending (linear-corrected | linear), font_thicken (macOS),\n",
    "              stem_gamma,\n",
    "              font_hinting (Linux/Windows: full | light | native | off),\n",
    "              font_subpixel (Linux CPU renderer: off | rgb | bgr),\n",
    "              font_variation [\"wght=450\", ...], font_weight,\n",
    "              font_weight_dark_nudge (variable fonts, e.g. SF Mono),\n",
    "              font_family_bold/_italic/_bold_italic, font_synthetic_style,\n",
    "              fallback_fonts [ordered], symbol_font, emoji_font\n",
    "              (config > built-in discovery).\n",
    "  Behaviour   gpu, scrollback_lines, columns, lines, copy_on_select,\n",
    "              option_as_meta, search_history_lines, focus_boost (Windows:\n",
    "              shell priority follows window focus; default on),\n",
    "              explain_heavy_load (the band names what slows your typing;\n",
    "              default on), desktop_alerts (aterm's own alerts also as\n",
    "              system notifications; default off).\n",
    "  Security    allow_window_ops, allow_notifications, allow_palette_reconfigure,\n",
    "              allow_kitty_file_transfer, allow_osc52_query,\n",
    "              secure_keyboard_entry (macOS)  (all opt-in, default off).\n",
    "  Keys        [keybindings] \"chord\"=\"action\"; [key_sequences] \"chord\"=raw bytes.\n",
);

/// The slot [`HELP_TAIL`]'s CONFIG line and [`STARTER_CONFIG`]'s header carry
/// where the config path goes, filled at print/write time with the path the
/// loader ACTUALLY resolves (`app_config::config_path`, the rule the window
/// loads and hot-reloads by). A literal there was wrong on every Windows box:
/// measured 2026-09-22 on 0.90.0, `aterm-gui --help` said
/// `CONFIG:  ~/.config/aterm/aterm.toml` and the `--write-config` starter was
/// headed the same, while the window they describe read
/// `%APPDATA%\aterm\aterm.toml`.
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
///
/// `--unset-default-terminal` is here because it is the ESCAPE HATCH: a machine
/// whose `HKCU\Console\%%Startup` delegation points at a class nothing can
/// create stops opening consoles entirely, and the way out has to be findable
/// from the binary — not only from a README the user may not have.
/// `--set-default-terminal` is not listed: it refuses in every build (no COM
/// handoff server), so its dispatch arm stays only to answer a script with that
/// refusal.
#[cfg(windows)]
const WINDOWS_HELP_TAIL: &str = concat!(
    "\nWINDOWS:\n",
    "    --install-context-menu     Add 'Open aterm here' to the Explorer right-click menu\n",
    "                               (per-user HKCU; --uninstall-context-menu removes it).\n",
    "    --unset-default-terminal   Clear aterm's default-terminal registration; another\n",
    "                               terminal's registration is left alone.\n",
);

/// A documented starter config written by `--write-config`. Linux-tuned (real
/// key names, sensible non-macOS defaults); EVERY line is commented, so writing it
/// changes nothing — it just makes the settings surface
/// DISCOVERABLE for a new user who has no `aterm.toml` yet. The header's
/// [`CONFIG_PATH_SLOT`] is filled with the path the file is written to
/// ([`starter_config_for`]).
const STARTER_CONFIG: &str = "\
# aterm — <aterm.toml>
# Every setting is optional; uncomment to override. Live settings reload on save;
# renderer/initial-grid settings require relaunch, and session settings require a new session.
# Launch flags (aterm-gui --help) take precedence over this file for that launch.

# --- shell --------------------------------------------------------------------
# shell = \"bash\"        # interactive shell. Discovery-resolved: \"bash\" finds Git
#                       # Bash even if it is not on PATH; \"pwsh\", \"cmd\", \"wsl\",
#                       # \"nu\", or an absolute path also work. Unset = platform
#                       # default (Windows: pwsh > powershell > %COMSPEC% > cmd;
#                       # `aterm doctor` names the one this machine gets). Override
#                       # at launch with --shell.
#                       # Shell integration (prompt marks, jump-to-prompt, command
#                       # blocks, cwd tracking) is injected automatically for zsh,
#                       # bash, fish, pwsh/powershell and \"wsl\" (whose distro must
#                       # use bash as its login shell). \"cmd\" is PARTIAL: prompt
#                       # marks, jump-to-prompt and cwd tracking all work, but cmd
#                       # has no hook for when a command starts or finishes, so
#                       # `blocks` shows prompt-delimited regions with no command
#                       # text and no exit code, and `wait` never fires. \"nu\" gets
#                       # none.
# shell_args = [\"-l\", \"-i\"]  # extra argv after the shell (e.g. a login bash);
#                       # for \"wsl\" these are wsl.exe options, e.g. [\"-d\", \"Debian\"]

# --- appearance ---------------------------------------------------------------
# font_family = \"JetBrains Mono\"  # any installed monospace family, or a .ttf path
# font_family_bold = \"JetBrains Mono Bold\"      # real bold face (unset = auto-discover)
# font_family_italic = \"JetBrains Mono Italic\"  # real italic face
# font_family_bold_italic = \"JetBrains Mono Bold Italic\"
# font_synthetic_style = true      # false: never fake bold/italic (regular when no real face)
# fallback_fonts = [\"Sarasa Mono\"] # ordered Unicode fallbacks, tried before built-in discovery
# symbol_font = \"Symbols Nerd Font\"   # monochrome symbol fallback
# emoji_font = \"Noto Color Emoji\"     # colour-emoji face
# font_px = 16                     # physical px (13 looks small on a 100+ DPI panel)
# theme = \"Default\"               # a built-in scheme, or \"dark:<name>,light:<name>\"
# foreground = \"#C8D3F5\"
# background = \"#1A1B26\"
# cursor_style = \"block\"          # block | bar
# cursor_blink = true
# cursor_momentum_glow = true     # the cursor glows with how fast you type and cools when you stop; warm = no blink (default ON)
# selection_color = \"#33415E\"
# selection_foreground = \"#FFFFFF\" # selected-text ink; unset = auto contrast floor
# selection_inactive = false       # dim the selection band while the window is unfocused
# split_focus_mark = true          # in a split, ink the divider edge of the pane taking keystrokes
# window_colorspace = \"srgb\"       # macOS GPU CAMetalLayer tag: srgb (colour-managed) | display-p3 (legacy stretched)
# minimum_contrast = 1.0           # per-cell WCAG contrast floor, 1.0 (off) ..= 21.0
# background_opacity = 1.0         # macOS GPU window glass, 0.0 (transparent) ..= 1.0 (solid); other renderers stay solid; <1.0 auto-floors contrast to 4.5:1
# background_material = \"none\"     # macOS vibrancy behind glass: none | hud | sidebar | under-window
# window_padding = 12.0            # interior padding, logical px per edge (0..=64; hot-applies)
# window_padding_top = 2.0         # tighter TOP-edge override (0..=window_padding; the titlebar band supplies the rest)
# bold_is_bright = true            # SGR bold promotes ANSI 0-7 to bright 8-15
# faint_opacity = 0.5              # SGR dim: fg fraction kept, blended toward the bg
# scrollback_lines = 100000        # total history across ring + tiered store (default 100k, 0 = unlimited)
# tab_strip_rows = 1               # in-grid tab bar (Linux has no native toolbar)

# --- smart titles (window + tabs) ---------------------------------------------
# aterm keeps the stable Title and an authored Description, and can add a generated
# live Activity fallback describing what the session is doing. The builtin provider is local, deterministic,
# and sends nothing anywhere. Aterm auto-starts only its managed Ollama install,
# with Ollama cloud access disabled. A pre-existing localhost service is untrusted,
# like any remote provider, and stays blocked until title_summary_allow_remote = true.
# descriptive_titles = true        # generate live Activity (default ON); false preserves authored Description
# title_summary_provider = \"builtin\" # builtin | ollama | openai-compatible | off
# title_summary_model = \"qwen3.5:4b-q4_K_M\" # Ollama/service model name
# title_summary_endpoint = \"\" # Ollama only: blank = private per-process ephemeral Ollama endpoint; OpenAI-compatible requires a URL
# title_summary_token_file = \"~/.config/aterm/title-summary.token\" # path only; NEVER put a raw token here
# title_summary_timeout_seconds = 20 # provider deadline, clamped to 1..=120
# title_summary_proxy_mode = \"environment\" # environment | direct; managed Ollama is always direct
# title_summary_ca_file = \"~/.config/aterm/private-model-ca.pem\" # custom PEM roots (replaces platform roots)
# title_summary_interval_seconds = 15 # refresh cadence, clamped to 5..=300
# title_summary_context_lines = 24 # recent terminal lines considered, clamped to 4..=80
# title_summary_include_output = true # false limits context to shell/title metadata
# title_summary_allow_remote = false # privacy gate; filtering is heuristic, so remote consent may expose terminal context
# tab_title_format = \"title-description\" # title | description | title-description | description-title
# window_title_format = \"title-description\" # title | description | title-description | description-title

# --- tab subject & status -------------------------------------------------------
# Entirely local classification of what each session is DOING (running / quiet /
# idle / exited), from shell integration, the foreground-job boolean, and screen
# movement. No model, no network. tab_status = false stops the classifier itself.
# tab_status = true                # classify session status (default ON)
# tab_status_quiet_after_ms = 5000 # a silent foreground job becomes \"quiet\" after this, clamped to 500..=120000
# tab_status_dwell_ms = 750        # hysteresis before a phase is published, clamped to 0..=10000
# tab_status_badge = true          # project status onto the tab's busy/attention marks
# tab_connection_badge = true      # mark tabs holding/receiving a session connection (▲ out / ▽ in / hourglass both)

# --- motion / cursor aurora -----------------------------------------------------
# serious_mode = false            # mute sounds + hide decorative effects; underlying effect settings return when switched off
# robi = true                      # Robi the helper robot lives on your terminal (walks your typed row, ladder up, tab-bar monkey bars, tips above his head); type robi to make him greet you (default OFF)
# motion = \"auto\"                 # auto (live Reduce Motion on macOS; sampled at Windows window attach; no OS query elsewhere) | full | reduced
# load_adaptive_motion = true      # drop decorative effects under sustained render overload (smooth scrolling is never shed); false = never shed (motion=\"full\" also forces effects on)
# cursor_trail = true              # the cursor motion trail + light crown, plus the walking cat the default style rides it with. Default ON — except on Windows, where it is opt-IN: uncomment this line for the whole show
# cursor_trail_style = \"rainbow kitty pet\"  # rainbow kitty pet (DEFAULT; the tall full-height rainbow body — letters inside the light — with the walking cat; \"rainbow kitty\"/\"kitty\" name the same resident) | rainbow kitty flying (same tall ribbon under the earned flying head; aliases \"flying kitty\"/\"kitty flying\" and historical \"nyan rainbow\"/\"nyan\"/\"rainbow\") | rainbow kitty underline (the explicit highlighter-plus-under-baseline alternate) | rainbow kitty tall (an explicit spelling of the default tall body; aliases \"rainbow tall\"/\"tall rainbow\"/\"nyan tall\") | rainbow kitty flat (the A/B control: the flat body of 2026-09-13, before the comet body and its vivid rail; aliases \"rainbow flat\"/\"flat rainbow\"/\"nyan flat\") | rainbow dog pet | phaser | comet | lumen | sparkle | fire | laser | water | beam | classic (the v0.28 trail restored: thin comet, soft square bloom, and the jump comet the modern gates no longer draw; aliases \"v0.28\"/\"retro\") | classic mono (the same salvaged trail in ONE hue from the theme cursor colour, following cursor_trail_color/OSC 12, instead of the rolling spectrum) | off
# cursor_trail_color = \"#50FA7B\"      # base colour (default: the theme's cursor colour)
# cursor_trail_accent = \"#7AA2F7\"     # comet-tail / ring colour (default: brightened base)
# cursor_trail_ms = 260                # fade duration in ms (30..=2000)
# cursor_trail_length = 24             # max comet length in cells (1..=512)
# cursor_trail_intensity = 1.0         # aurora brightness 0.0..=1.0
# cursor_trail_radius = 0.6            # bloom-crown radius in cells (0.0..=2.0)
# cursor_trail_ring = true             # expanding landing \"ping\" ring on a jump (default ON)
# --- sound (Settings > Cursor & Motion > Sound) -------------------------------
# trail_sounds = true              # macOS-only trail-style audio (parsed but inert elsewhere); silent whenever the trail is (default ON)
# trail_sound_volume = 0.4         # 0.0..=1.0 trail sound level (default 0.4 ~= -22 dBFS peaks, far under the bell); does NOT scale bell_sound
# trail_sound_style = \"auto\"     # typing sound: auto = follow the trail style; or an instrument for every keystroke whatever the trail looks like:
#                                  #   music box | warm pluck | glitter | ice chime | droplet | pew | zap | tick | crackle  (the nine palettes, by sound)
#                                  #   mechanical (keyboard click + thock) | typewriter (clack + platen, bell + carriage on Enter) | marimba | felt (muted piano)
#                                  #   aliases: the trail-style names (water, comet, rainbow kitty, ...), bell, raindrop, mech, thock, piano, clack
# tone_melody = true               # the melody leans with the typed line's inferred mood (on-device, typed input only); default ON and deliberately subtle
# trail_sound_bed = true           # the continuous ambient BED texture behind the notes (default ON; false silences the bed)
# trail_sound_riff = true          # the held-key SING-ALONG song (the loudest voice); false quiets just the song and keeps its visuals (default ON)
# bell_sound = true                # the audible BEL beep (macOS NSBeep / Windows MessageBeep); false keeps the visual flash and window attention (default ON)
# cursor_trail_bloom = true            # GPU-only soft halo around the comet (default ON)
# cursor_trail_bloom_strength = 0.85   # 0.0..=3.0 (halo intensity)
# cursor_trail_bloom_radius = 2.2      # 0.5..=8.0 half-res blur texels; default 2.2, 1.8 for rainbow kitty; set to override
# cursor_fire_shimmer = true           # GPU-only heat-haze refraction above burning cells (default ON)
# hdr_glow = true                      # EDR cursor glow above SDR white (GPU + HDR panel, macOS EDR or Windows scRGB; provably inert on SDR; default ON)
# cursor_glow_sdr_boost = 0.25         # GPU-only SDR crown strength 0..=1 (dark themes only — light themes self-degrade; 0 = off)
# stream_fade = true               # set true to enable streamed-output fade-in (default OFF; obeys Reduce Motion)
# stream_fade_ms = 90              # fade-in duration in ms (16..=1000)
# temporal_recording = false       # record a per-session replay spine (query via `aterm-ctl temporal [tick]`); default off, costs memory

# --- text shaping -------------------------------------------------------------
# ligatures = true                 # programming ligatures (=>, !=, >=, ===, ...)
# cursor_break_ligatures = true    # break the cursor cell out of ligatures (default false leaves ligatures intact)
# line_height = 1.0                # cell-box multiplier 0.8..=2.0 (leading splits half above/below)
# adjust_baseline = 0              # px baseline escape hatch (±32) for off-metric faces
# adjust_underline_position = 0    # px underline shift (±32, + = down) over the font's post table
# adjust_underline_thickness = 0   # px underline thickness delta (±32) over the font's post table
# underline_skip_descenders = true # gap the underline around descender ink (browser-style)
# font_features = [\"zero\", \"ss01\"] # OpenType features to force on
# bidi = \"implicit\"               # RTL reordering: implicit | disabled | explicit
# ambiguous_width = \"narrow\"       # East-Asian ambiguous width: narrow | wide
# text_blending = \"linear-corrected\" # AA weight: linear-corrected (native feel) | linear
# font_thicken = false             # macOS: CoreText font smoothing (heavier glyphs)
# stem_gamma = 1.0                 # aesthetic stem weight (<1 thicker, >1 thinner)
# font_hinting = \"full\"           # Linux/Windows grid fitting: full (crispest, default) | light
#                                  # (hintslight look) | native (font bytecode) | off
# font_subpixel = \"off\"           # Linux subpixel-RGB text (CPU renderer only, stage 1):
#                                  # off (default) | rgb | bgr; opaque frames only
# font_variation = [\"wght=450\"]    # variable-font axes (clamped to fvar; default = Regular / wght=400)
# font_weight = 450                # wght shorthand; wins over a font_variation wght entry
# font_weight_dark_nudge = 0       # extra wght on DARK themes (applied only when grid-safe)

# --- behaviour ----------------------------------------------------------------
# gpu = false                      # GPU rendering is ON by default (auto CPU fallback); set false (or launch with --cpu) to force CPU
# copy_on_select = true            # auto-copy mouse selection to CLIPBOARD (DEFAULT on; OFF on Linux, where a
                                   # selection owns PRIMARY and the CLIPBOARD stays for explicit copies; true opts into both)
# show_build_badge = false         # OPTIONAL floating top-right v<version>·<build> pill — DEFAULT off
                                   # (the version lives in the menu bar: the v<version> menu opens About)
# confirm_multiline_paste = true   # confirm unbracketed multiline paste (macOS sheet / Windows dialog / Linux in-window banner)
# focus_boost = true               # Windows: boost the visible shells' priority while aterm is focused (DEFAULT on; no-op elsewhere)
# explain_heavy_load = true        # when typing slows because something else loads the machine, the message band names it
                                   # (\"Typing slowed by cargo in tab 2\"); details go to the log. false: off entirely
# desktop_alerts = false           # aterm's OWN alerts (an agent that needs you, the operator, update health) also as
                                   # system notifications (macOS: terminal-notifier, else osascript = \"Script Editor\").
                                   # DEFAULT off: they stay in the band, the menu bar and messages.log. Programs'
                                   # OSC 9/99/777 notifications are allow_notifications' below

# --- security opt-ins (all default OFF) ---------------------------------------
# allow_window_ops = false         # XTWINOPS title, text-grid-size, text-area-pixels and cell-size reports (window/screen
#                                  # position and screen size stay unanswered); Linux also applies window
#                                  # manipulations (move stays denied)
# allow_notifications = false      # OSC 9/99/777 desktop notifications; macOS delivers through terminal-notifier
#                                  # if installed, else osascript — a subprocess under aterm's identity; Windows
#                                  # shows a notification-area balloon/toast from aterm itself (Shell_NotifyIcon,
#                                  # no subprocess); Linux has no delivery yet — the request is dropped
# allow_palette_reconfigure = false
# allow_kitty_file_transfer = false
# allow_osc52_query = false        # programs may READ the clipboard (OSC 52); answered only when on. On macOS 26 that
#                                  # read is what raises the system's \"aterm would like to paste from …\" alert
# secure_keyboard_entry = false    # macOS: block other processes from observing keystrokes (held while aterm is frontmost)

# --- sparkle words (purely visual; NEVER affects copied text, logs, or recordings)
# Decorate matched words: a randomized SPARKLE over profanity (the \"fuck\" family in
# every major language) and a steady CAT-PAW over cat/kitty words. Both live toys are
# ON by default. Use the independent Sparkle words and Keyword kitties
# switches in Settings, or set a live category's `enabled = false` here, to
# silence either product. The retained master `enabled = false` silences both.
# [sparkle_words]
# enabled = true                   # master switch (default ON; false → byte-identical render)
# languages = [\"en\"]               # un-gate ambiguous homographs (fr \"chat\", de \"Kater\"); [\"all\"] = every language
# reduced_motion = false           # force the static, non-twinkling path
# suppress_in_alt_screen = false   # true → full-screen TUIs (vim/less/htop/claude) never decorated
# lexicon = \"~/.config/aterm/extra-lexicon.toml\"   # extra [[entry]] blocks merged over the builtin
# toy_packs = [\"~/.config/aterm/toys/tiny-triumphs/pack.toml\"]  # strict community packs (max 8; later wins)
# deny = [\"scat\"]                  # never decorate these words, any category
# [sparkle_words.profanity]
# enabled = true                   # ON by default; set false to silence just the expletive sparkle
# style = \"rainbow\"                # \"rainbow\" (default) = the v3 animated rainbow ink; 10% of
#                                  #   episodes escalate to the FUCK SUPER NOVA (supernova_chance)
#                                  #   | \"nova\" = the v2 classic nova (one flash per appearance,
#                                  #   WCAG-limited to <= 2 ignitions/s window-wide) | \"sparkle\" = v1
# supernova_chance = 30            # rainbow-only escalation chance, percent (0..=100; 0 disables)
# magic = true                     # Quasar (1/512) / Singularity (1/1024) rare nova variants
# palette = [\"#ffd447\", \"#ff7ce5\", \"#7cf0ff\"]   # sparkle tints; empty → lively hue rotation
# density = 3                      # sparks per word per frame (1..=12)
# anim_ms = 2500                   # how long a word sparkles after appearing (350..=10000)
# jitter = 2                       # sub-cell sparkle jitter in px (0..=6)
# intensity = 0.85                 # opacity 0.0..=1.0
# bonk = true                      # the curse BONK sound effect on a TYPED curse (default ON; scaled by trail_sound_volume — Settings shows it in the Sound box)
# bonk_detonation = false          # also bonk when an on-screen curse's supernova ignites (default OFF; typed provenance only unless opted in)
# extra_words = [\"frak\"]           # extra words to treat as profanity
# ignore_words = [\"fluff\"]         # never decorate these as profanity
# [sparkle_words.feline]
# enabled = true                   # the friendly default (takes effect only when master on)
# style = \"cat\"                    # \"cat\" is the only graphic mode; legacy \"paw\" is
#                                  #   ink-only and renders no paw graphic (Manual-only)
# magic = true                     # Fortune (1/512) / Nebula (1/1024) rare cats
# allow_bare_cat = true            # DEFAULT on: decorate the literal 3-letter \"cat\" (also the shell command)
# cjk_single_char = false          # decorate a lone 猫 anywhere (high false-positive rate)
# log = true                       # record sightings into the Kitty Log collection book
#                                  #   (machine-owned kitty-log.toml beside aterm.toml)
# extra_words = [\"mittens\"]        # extra words to treat as feline
# ignore_words = [\"cats\"]          # never decorate these as feline
# [[sparkle_words.custom]]         # v3 custom word effects: data, not code (repeatable block)
# words = [\"ultrathink\"]           # surfaces (2-char+ ok — explicit config is consent; CJK ok)
# ink = { colorway = \"rainbow\" }   # or \"twotone:#RRGGBB,#RRGGBB\"; omit for no ink
# burst = { kind = \"starburst\", chance = 10 }   # sparkle|nova|supernova|starburst|glow
# graphic = { collection = \"cats\" }             # the peeking cat on your own word
# [sparkle_words.ink]              # animated glyph-ink shimmer
# enabled = true                   # takes effect only when sparkle_words.enabled is on
# strength = 0.75                  # ink tint vs original fg; clamp 0.0..=1.0
# sweep_ms = 2200                  # one specular sweep window; clamp 350..=6000
# loop = false                     # true: re-sweep while visible (keeps focused wakes live;
#                                  #   raises the sweep_ms floor to 600 — flash margin)
# [sparkle_words.emphasis]         # hype words — ink-only class (4th lexicon class)
# enabled = true                   # no builtin words — populate via extra_words
# extra_words = [\"megathink\"]      # extra words to treat as emphasis
# ignore_words = [\"turbo\"]         # never decorate these as emphasis

# --- matrix rain (PHOSPHOR; purely visual — rain falls UNDER the text, in EMPTY cells
# only; copy/selection/search/recordings read exact bytes). OFF BY DEFAULT — the rain
# follows what the session is doing: agent output pours, typing drizzles, idle drains
# to still glass. `enabled` is the default for every session; View > Matrix Rain, the
# command palette, `aterm-ctl rain`, or a bound \"toggle_matrix_rain\" chord override
# it PER SESSION (either direction — a session can rain over a disabled config), and
# toggling the session you're looking at is still the instant panic-off. Session
# overrides are runtime-only: they die with the session and win over this key until
# then. ------------------------------------------------------------------------
# [matrix_rain]
# enabled = false                  # default for every session (OFF; true → it rains);
#                                  #   per-session toggles override it either way
# fps = 30                         # WORKING tick rate (12..=60; CALM always runs 12 Hz)
# density = 6                      # column density (1..=12)
# speed = 5                        # fall speed (1..=10; 5 = neutral)
# trail = 5                        # trail length (1..=10; 5 = neutral)
# alpha = 96                       # body coverage (16..=135); omit → derived from the theme
#                                  #   under the below-dim-text luminance bound
# head_alpha = 135                 # bright-head coverage (alpha..=135); omit → derived
# hue = \"matrix\"                   # \"matrix\" | \"theme\" | \"#RRGGBB\" (bad hex → matrix green)
# mutation_ms = 133                # glyph mutation window in ms (80..=2000)
# idle_secs = 8                    # idle seconds until the mandatory drain (2..=120;
#                                  #   there is no \"keep\" — nothing animates forever)
# suppress_in_alt_screen = false   # true → fullscreen TUIs (vim/less/htop/claude) never rain
# output_material = true           # supported literal codepoints from REAL output; current composer band protected
# turn_wave = true                 # synchronized head sweep when the agent's turn completes
# bell_alert = true                # visual bell → 2 s constant-luminance amber hue ramp
# seed = 0                         # 0 = stable per-window field; nonzero = reproducible

# --- bundled ALab toolchain manager (atpkg): the SAME table the co-located `atpkg`
# reads, so there is exactly ONE config surface — and no environment alternative
# to any of it. Inert in builds without a pinned root key — Settings ▸ Packages
# shows the live posture. --
# [packages]
# enabled = true                   # Automatic updates — THE switch (Settings ▸ Packages):
#                                  #   a published index within minutes, a full signed
#                                  #   check every 6 hours; read LIVE — an edit stands
#                                  #   the loop down or resumes it within seconds (what
#                                  #   every lane did is in packages.log, beside aterm.log)
# auto_install = true              # install the ALab toolset nobody named: the first-run
#                                  #   fill and new members of the signed set (batteries
#                                  #   included; may download GBs; `uninstall`, `exclude`
#                                  #   and `enabled = false` always win)
# exclude = []                     # per-program opt-out from the SIGNED default set
# [packages.links]                 # a maintainer's local checkout, per program:
# ay = \"~/ay\"                      #   path → managed dev-link (registry skipped)
# [reroute]
# announce = true                  # a rerouted cargo/rustc/tlc says so before it runs
#                                  #   upstream (false silences the line, never the run)

# --- this Mac: host settings, applied as the window opens, by a terminal
# session once a day, and by the next package pass after an edit here.
# `aterm pkg machine apply` applies now; `aterm pkg machine` reads them.
# Settings ▸ Security shows the measured state and has Apply now. macOS only.
# [machine]
# universal_control = \"off\"        # \"off\" (default: disable it for this host) | \"leave\"
# spotlight_noindex = true         # rename cargo target dirs under $HOME to .noindex
#                                  #   (a `target` symlink keeps cargo working)

# --- input policy: map a chord to RAW BYTES sent to the program, overriding the
# default key encoding + non-menu hardcoded chords (NOT macOS menu keys like Cmd-C,
# which the menu claims first). Put a value with \\e / \\xNN
# in a TOML literal '...' string; a basic \"...\" string only understands \\n \\r \\t. -
# [key_sequences]
# \"shift+enter\" = \"\\n\"        # send a literal newline (LF)
# 'f5' = '\\e[15~'              # ESC[15~  (literal '...' string so \\e is ESC)
#
# --- keybindings (MUST be the LAST section: a TOML table runs to end-of-file, so
# any bare top-level key placed below it would parse as a keybinding entry) -------
# [keybindings]
# \"ctrl+shift+t\" = \"new_tab\"
# \"ctrl+shift+space\" = \"toggle_vi_mode\"   # keyboard copy-mode (h/j/k/l, w/b/e, f/t, v, Esc)
";

/// Pull the next argument as the value for `flag`, exiting 2 with a hint if it is
/// missing. Used by the value-taking flags so they share one error shape.
fn flag_value(flag: &str, args: &mut impl Iterator<Item = String>) -> String {
    match args.next() {
        Some(v) => v,
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

/// Write a print-and-exit flag's ANSWER to stdout and end the process: with the
/// flag's own `code`, or 1 when the answer could not be written.
///
/// Never `print!`: it PANICS when stdout is a pipe whose reader has gone, and on
/// Windows that is an ordinary shape of the windowed image's answer. Measured
/// 2026-09-27 on the installed 0.95.0: pwsh does not wait for a GUI-subsystem
/// child even while it captures it, so `aterm-gui --version 1>$null` and `$v =
/// aterm-gui --version` close the pipe before the answer is written, and the
/// panic banner (`failed printing to stdout: The pipe is being closed. (os error
/// 232)`) printed after the next prompt; a reader that stops early panicked
/// both images (`aterm-gui --help | findstr /c:"x" nosuchfile.txt` and `aterm
/// --window --help | …`: `The pipe has been ended. (os error 109)`). The one
/// `write_all` also hands the whole answer to the console in as few writes as
/// the stream allows, where each `println!` was a write of its own.
fn answer_and_exit(argv: &[String], text: &str, code: i32) -> ! {
    use std::io::Write as _;
    let written = {
        let mut out = std::io::stdout().lock();
        out.write_all(text.as_bytes()).and_then(|()| out.flush())
    };
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

/// The status a print-and-exit flag ends with, from how its answer's write went:
/// the flag's own `code` when it was written — or when the reader CLOSED the pipe
/// (`BrokenPipe`, which Windows' "pipe is being closed" and "pipe has been
/// ended" both map to): a reader that went away has said it wants no more,
/// aterm-ctl's rule for `| head` — and otherwise the stderr line naming the
/// failure (a full disk behind `> file`), which ends the process with 1.
fn answer_status(written: std::io::Result<()>, code: i32) -> Result<i32, String> {
    match written {
        Ok(()) => Ok(code),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(code),
        Err(e) => Err(format!("aterm-gui: the answer was not written ({e})")),
    }
}

/// Whether this answer went straight onto a console this process ATTACHED to —
/// the windowed image (GUI subsystem) typed at a shell prompt with its output not
/// redirected, which is the one shape whose answer can land after the prompt.
/// The console image, a redirected or piped answer, and a launch with no console
/// behind it (Explorer, the console image's own handoff, whose stdio is NUL)
/// never are.
#[cfg(windows)]
fn answered_onto_an_unwaited_console() -> bool {
    use std::io::IsTerminal as _;
    crate::win32::attached_a_console() && std::io::stdout().is_terminal()
}

/// The one stderr line under such an answer. No handoff can make a prompt wait:
/// the prompt is waiting for no one — it did not wait for THIS process, so it
/// would not wait for a console image this process started and waited on
/// either. So the answer stays, and the line names the spelling a prompt does
/// wait for: the console image, `aterm --window` and the same flags, whose
/// answer is this parser's, byte for byte. Measured 2026-09-27 on the installed
/// 0.95.0: `aterm-gui --version; Write-Output AFTER-GUI` printed `AFTER-GUI`
/// first in pwsh and in an interactive cmd; `cmd /c` and a pwsh pipeline (`|
/// Out-String`) do wait, which is why the line says "can", not "did".
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

/// CLI: `aterm-gui [OPTIONS] [-e CMD ARGS… | --help | --version]`.
/// `--help`/`--version` print and exit; an unknown option, a `-d` without a valid
/// directory, `-e` without a command, or a value flag missing its argument prints
/// a hint and exits 2 (no window launch). With no args (a Finder/.app launch) this
/// is a no-op and a normal interactive shell starts in the inherited working
/// directory. A launch flag is recorded, never exported: the render/font flags in
/// [`Cli::launch`], the rest in [`LaunchFlags`]. Numeric flags are validated here
/// for a clean early error; containment is validated by its own fail-closed funnel
/// in `main`. Every print-and-exit flag answers through [`answer_and_exit`].
pub(crate) fn parse_cli(argv: Vec<std::ffi::OsString>) -> Cli {
    // Lossy conversion mirrors the binary era's `env::args()` UTF-8 boundary
    // (a non-UTF8 flag was a panic there; here it degrades to a usage error).
    // Kept whole as well as walked: an answer that may print after the prompt
    // names the command line to type instead (`unwaited_answer_line`).
    let argv: Vec<String> = argv
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut args = argv.clone().into_iter();
    let mut cwd: Option<String> = None;
    let mut hold = false;
    let mut headless = false;
    let mut lifeline_fd: Option<String> = None;
    let mut exec_command: Option<Vec<String>> = None;
    let mut launch = crate::launch::Launch::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                // Title, then the origin line (`by Andrew Yates · ALab ·
                // alab.systems`), then the body.
                let help = format!(
                    "{HELP_TITLE}{}\n{HELP_HEAD}{}{}",
                    aterm_types::identity::ORIGIN_LINE,
                    keys_help(),
                    help_tail()
                );
                // Windows-only verbs, printed here rather than folded into the
                // cross-platform HELP_TAIL so no Unix build advertises a flag it
                // does not have. `--unset-default-terminal` in particular is the
                // ESCAPE HATCH from a delegation that stops consoles opening;
                // leaving it discoverable only through the README strands anyone
                // who reaches that state on a stripped machine. `--set-...`
                // refuses in every build, so it is not advertised.
                #[cfg(windows)]
                let help = help + WINDOWS_HELP_TAIL;
                answer_and_exit(&argv, &help, 0);
            }
            "-V" | "--version" => {
                // The DISPLAY version: semver + the compiler-provenance suffix
                // (+r.<slug> = upstream Rust, +t.<slug> = Trust fork) — what
                // the ship tool (aterm-release buildplan.rs) echoes into the
                // cut transcript as provenance.
                let version = format!("aterm-gui {}\n", crate::build_info::version_display());
                answer_and_exit(&argv, &version, 0);
            }
            // Diagnostics ("doctor"): print the report and exit (no window). Placed
            // after the env-setting flags so e.g. `--gpu --diagnose` reports the
            // effective renderer.
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
                // Discoverability: drop a fully-documented starter config (every key
                // commented, so it changes nothing) where the loader looks for it.
                let said = match crate::app_config::config_path() {
                    Some(path) if path.exists() => format!(
                        "config already exists: {}\n(edit it directly — settings hot-reload on save)\n",
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
                // Optional family argument; empty falls back to the effective
                // font_family (--font > config). Exits non-zero if it does not resolve.
                crate::launch::install(launch.clone());
                let family = args.next().unwrap_or_default();
                let (msg, ok) = crate::diagnostics::show_face(&family);
                answer_and_exit(&argv, &msg, i32::from(!ok));
            }
            "--list-themes" => {
                answer_and_exit(&argv, &crate::diagnostics::list_themes(), 0);
            }
            // Windows: install/remove the "Open aterm here" Explorer context menu
            // (per-user HKCU verb on directories/backgrounds/drives → `aterm-gui -d <path>`).
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
            // Windows 11 default-terminal (DefTerm) handoff. Opt-in only, and
            // currently REFUSED — see `defterm_win::handoff_server_available`.
            // The refusal is deliberately loud and specific: silently doing
            // nothing would leave the user believing their consoles had been
            // redirected, and "it didn't take" is the one DefTerm failure that
            // looks identical to a wrong registry key.
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
            // Never GATED on the handoff server: removal is the escape hatch
            // from a broken delegation and must work in every build, including
            // on a machine whose registering exe is already gone. But it is
            // guarded on OWNERSHIP — `HKCU\Console\%%Startup` is a shared,
            // machine-wide console setting, so a registration aterm did not
            // write is left alone and said so, never deleted and then reported
            // as ours.
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
            "-d" | "--working-directory" => {
                let dir = flag_value("-d/--working-directory", &mut args);
                if !std::path::Path::new(&dir).is_dir() {
                    eprintln!("aterm: not a directory: {dir}");
                    std::process::exit(2);
                }
                cwd = Some(dir);
            }
            "--hold" => hold = true,
            // --- The render/font flags: carried by `launch`, no env twin. ---
            "--font-px" => {
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
            "--font" => {
                let v = flag_value("--font", &mut args);
                launch.font_family = (!v.trim().is_empty()).then_some(v);
            }
            "--scale" => {
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
            // `--gpu` / `--cpu`: the LAST one given wins (each overwrites the other),
            // and either outranks config `gpu`.
            "--gpu" => launch.renderer = Some(crate::launch::RendererFlag::Gpu),
            "--cpu" => launch.renderer = Some(crate::launch::RendererFlag::Cpu),
            // --- The rest of the launch flags: recorded in `LaunchFlags`. ---
            // --shell: the interactive shell to spawn. Discovery-resolved by the
            // PTY layer — "bash" finds Git for Windows off-PATH, "pwsh"/"cmd"/
            "--shell" => {
                let shell = flag_value("--shell", &mut args);
                set_launch_flag(|f| f.shell = Some(shell));
            }
            // Containment: the LAST of these wins (`--sandbox --containment
            // user` is user); the fail-closed parse is the launch funnel's.
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
            // Validated once the whole command line is read: it needs `--headless`,
            // which may come after it.
            "--lifeline-fd" => lifeline_fd = Some(flag_value("--lifeline-fd", &mut args)),
            "--columns" => {
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
            "--lines" => {
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
            // Accepted for compatibility and does nothing: shell integration is
            // on by default and `--no-shell-integration` is the opt-out. This
            // used to `flag_env("ATERM_SHELL_INTEGRATION", "1")`, which exported
            // a variable no code has ever read — a write-only name in the env
            // surface. The flag stays; the phantom variable is gone.
            "--shell-integration" => {}
            "--no-shell-integration" => set_launch_flag(|f| f.no_shell_integration = true),
            "--verbose" => {
                set_launch_flag(|f| f.verbose = true);
                aterm_gpu::set_verbose(true);
            }
            "-e" | "--command" => {
                let cmd: Vec<String> = args.by_ref().collect();
                if cmd.is_empty() {
                    eprintln!("aterm: -e/--command requires a command (try --help)");
                    std::process::exit(2);
                }
                exec_command = Some(cmd);
                break;
            }
            // NOTE: verbs are NOT parsed here. `ship` briefly was, and that was the
            // whole defect — this parser is reached only when the mode fork already
            // chose the window, so a verb wired here is invisible at a terminal. The
            // front door (`crates/aterm/src/main.rs`) owns every verb in
            // `aterm_cli::Verb`, above the fork.
            other => {
                // A bare word is a command the front door did not resolve, not an
                // option — the session parser's same split, so one mistake reads
                // one way at a terminal and through a pipe.
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
    Cli {
        exec_command,
        cwd,
        hold,
        headless,
        lifeline_fd,
        launch,
    }
}

/// The `--lifeline-fd` decision, pure: `Ok(None)` when the flag is absent — the
/// launch every person and service makes, left exactly as it was — the descriptor
/// number when it is well-formed on a headless launch, and the usage error
/// otherwise. A WINDOW is refused rather than armed: it is a person's to close,
/// and the quit path a cut lifeline takes would ask them first.
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

#[cfg(test)]
mod tests {
    use super::HELP_HEAD;

    /// Every user-facing diagnostic verb. The advertise-vs-dispatch gate below
    /// requires EACH entry to be both documented in `--help` AND have a real match
    /// arm in [`parse_cli`], so a new verb can never be added to one without the
    /// other (or silently advertised without a handler).
    const DIAGNOSTIC_VERBS: &[&str] = &[
        "--diagnose",
        "--list-actions",
        "--validate-config",
        "--list-fonts",
        "--show-config",
        "--write-config",
        "--list-keybinds",
        "--show-face",
        "--list-themes",
    ];

    /// A reader that CLOSED the pipe ends the answer quietly with the flag's own
    /// status — both Windows spellings of it included, the two the installed
    /// 0.95.0 panicked on (232 under `aterm-gui --version 1>$null` in pwsh, 109
    /// under `| findstr … nosuchfile.txt`) — and any other failure is said, with
    /// status 1, never swallowed.
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
        let refused = super::answer_status(Err(Error::from(ErrorKind::StorageFull)), 0)
            .expect_err("a full disk is a failure to report");
        assert!(
            refused.starts_with("aterm-gui: the answer was not written ("),
            "{refused}"
        );
    }

    /// No print-and-exit arm prints with `print!`/`println!` any more: each
    /// panicked on a closed stdout. `eprintln!` (a usage error) is not an answer.
    #[test]
    fn every_answer_goes_through_the_panic_free_writer() {
        let src = include_str!("cli.rs");
        let body = src
            .split_once("pub(crate) fn parse_cli(")
            .expect("parse_cli exists")
            .1;
        let body = &body[..body.find("\nfn lifeline_request(").expect("the next item")];
        for (at, _) in body.match_indices("print") {
            let before = body[..at].chars().next_back();
            let macro_call =
                body[at..].starts_with("print!(") || body[at..].starts_with("println!(");
            assert!(
                !macro_call || before == Some('e'),
                "parse_cli prints with a panicking macro: {}",
                &body[at..(at + 40).min(body.len())]
            );
        }
        assert!(body.contains("answer_and_exit(&argv, &help, 0)"));
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

    /// Only the windowed image ATTACHED to a prompt's console adds the line. The
    /// test harness, like the console image, was handed every std handle, so it
    /// attached nothing and never does.
    #[cfg(windows)]
    #[test]
    fn a_process_that_attached_no_console_adds_no_line() {
        assert!(!crate::win32::attached_a_console());
        assert!(!super::answered_onto_an_unwaited_console());
    }

    #[test]
    fn help_advertises_diagnostic_flags() {
        // Every user-facing diagnostic flag must be discoverable in --help.
        for flag in DIAGNOSTIC_VERBS {
            assert!(
                HELP_HEAD.contains(flag),
                "{flag} must be advertised in the help text"
            );
        }
        // No environment rung in any precedence the help states: the env
        // overrides were retired 2026-09-24, and the CONFIG line already reads
        // `flag > config > default`.
        assert!(
            HELP_HEAD.contains("(flag >\n                                   config > default)"),
            "--show-config states the precedence the build has"
        );
        assert!(
            !HELP_HEAD.contains("(env >"),
            "no retired env rung in --help"
        );
    }

    /// Off macOS the KEYS section is generated from the platform seed table, so
    /// EVERY seeded chord and action is documented (the hand-written list it
    /// replaced had drifted to omit five actions and the plain-Ctrl paste trio)
    /// and no macOS `cmd+*` chord — which off macOS would mean the shell-owned
    /// Win/Super key — can leak in.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn keys_help_documents_every_seeded_default_and_no_cmd_chords() {
        let keys = super::keys_help();
        for &(chord, action) in crate::keybinding::Keybindings::PLATFORM_DEFAULT_PAIRS {
            assert!(keys.contains(chord), "{chord} must be documented");
            assert!(keys.contains(action), "{action} must be documented");
        }
        assert!(!keys.contains("cmd+"), "no Cmd chords off macOS:\n{keys}");
        assert!(
            keys.contains("ctrl+click"),
            "the pointer half of the keymap stays documented"
        );
        assert!(
            keys.contains("ctrl+alt+click"),
            "the gesture the link caption names over a mouse-tracking program:\n{keys}"
        );
    }

    /// On macOS the KEYS section is hand-written (macOS ships an empty
    /// `platform_defaults()`; the menu bar owns the chords), so it has no
    /// generative source to be complete against. It documents the COMMON set
    /// and must (audit-2 item 17) both cover the high-traffic chords the old
    /// list omitted — the Command Palette and reopen-tab — and SIGNAL that it
    /// is partial, pointing at the menu bar as the exhaustive reference, so a
    /// user does not read the absence of splits/pane-focus as their absence.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_keys_help_covers_the_common_chords_and_admits_it_is_partial() {
        let keys = super::keys_help();
        for chord in [
            "Cmd-T",
            "Cmd-W",
            "Cmd-N",
            "Cmd-F",
            "Cmd-Shift-T",
            "Cmd-Shift-P",
            // Pane focus has no menu item, so the menu bar cannot teach it
            // (`keybinding::BUILTIN_CMD_CHORDS`: cmd+alt+arrow).
            "Cmd-Opt-Arrow",
        ] {
            assert!(keys.contains(chord), "{chord} must be documented:\n{keys}");
        }
        assert!(
            keys.contains("Command Palette"),
            "the palette — the door to every action — must be named"
        );
        assert!(
            keys.contains("menu bar lists them ALL"),
            "the section must signal it is not exhaustive:\n{keys}"
        );
        assert!(
            keys.contains("Cmd-Option-click"),
            "the gesture the link caption names over a mouse-tracking program:\n{keys}"
        );
    }

    /// `--headless` arms the mode through the `Cli` field alone: nothing is
    /// exported, so a child the window spawns inherits no headless request.
    #[test]
    fn the_headless_flag_is_recorded_and_exports_nothing() {
        let cli = super::parse_cli(vec![std::ffi::OsString::from("--headless")]);
        assert!(cli.headless);
        assert!(
            HELP_HEAD.contains("--headless") && !HELP_HEAD.contains("[env: ATERM_HEADLESS]"),
            "--help names the flag and no environment spelling"
        );
        // No lifeline unless one is asked for: a person's or a service's headless
        // instance runs exactly as it did before the flag existed.
        assert_eq!(cli.lifeline_fd, None);
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
                assert_eq!(cli.exec_command, Some(vec!["sh".to_string()]));
            }
        }
        for bad in ["-1", "x", ""] {
            let err = req(Some(bad), true).expect_err(bad);
            assert!(err.contains("expects a descriptor number"), "{err}");
        }
        assert!(
            HELP_HEAD.contains("--lifeline-fd <n>"),
            "--help documents it"
        );
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
        // The parser has no way left to write the environment at all (the
        // `flag_env` funnel every flag once exported through is deleted).
        let source = include_str!("cli.rs");
        for writer in [["aterm_log::env::", "set("], ["fn flag_", "env("]] {
            let writer = writer.concat();
            assert!(
                !source.contains(&writer),
                "a launch flag must not export anything: found `{writer}`"
            );
        }
    }

    #[test]
    fn every_advertised_verb_is_dispatchable() {
        // Each advertised verb must have a real `"<flag>" =>` match arm in this
        // file (the dispatch side). Reading the source keeps the gate honest
        // without invoking the arms (they call `std::process::exit`).
        let src = include_str!("cli.rs");
        for flag in DIAGNOSTIC_VERBS {
            let arm = format!("\"{flag}\" =>");
            assert!(
                src.contains(&arm),
                "{flag} is advertised but has no dispatch arm ({arm})"
            );
        }
    }

    /// An escape hatch nobody can find is not an escape hatch. Every Windows
    /// verb except `--set-default-terminal` (which refuses in every build and
    /// keeps only its dispatch arm) must be advertised in the Windows help
    /// block, that block must actually reach `--help`, and each verb must have
    /// a dispatch arm.
    #[cfg(windows)]
    #[test]
    fn windows_help_advertises_every_windows_verb() {
        let src = include_str!("cli.rs");
        for flag in [
            "--install-context-menu",
            "--uninstall-context-menu",
            "--unset-default-terminal",
        ] {
            assert!(
                super::WINDOWS_HELP_TAIL.contains(flag),
                "{flag} must be documented in the Windows --help block"
            );
            let arm = format!("\"{flag}\" =>");
            assert!(
                src.contains(&arm),
                "{flag} is advertised but has no dispatch arm ({arm})"
            );
        }
        // The verb that refuses in every build keeps its arm and stays unadvertised.
        assert!(src.contains("\"--set-default-terminal\" =>"));
        assert!(!super::WINDOWS_HELP_TAIL.contains("--set-default-terminal"));
        // ...and the block is actually printed by the -h/--help arm.
        let help_arm = src
            .split_once("\"-h\" | \"--help\" => {")
            .expect("the help arm exists")
            .1;
        let help_arm = &help_arm[..help_arm.len().min(1200)];
        assert!(
            help_arm.contains("WINDOWS_HELP_TAIL"),
            "--help must print the Windows verb block, not just define it"
        );
    }

    /// THE INVARIANT THAT MATTERS on the unset side: it must never be gated on
    /// the handoff server being available. Clearing `HKCU\Console\%%Startup` is
    /// the escape hatch from a delegation that points at a class nothing can
    /// create — a state where every new console fails to open — so a user in
    /// that hole must be able to dig out with the binary they already have.
    ///
    /// Behavioural, not source-text: `set` really does refuse with `Unsupported`
    /// while the gate is false, and `unset` really does return an outcome rather
    /// than that refusal, on the live machine, without writing anything.
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
        // On a machine where aterm is not the default (every machine today) the
        // unset must also have been INERT — see `defterm_win`'s scratch-key test
        // for the foreign-registration case driven end to end.
        assert_eq!(
            before, after,
            "unset must not touch a delegation aterm does not own"
        );
    }

    #[test]
    fn help_documents_env_stripping() {
        // FINDING #7: the child-shell env sanitization must be discoverable from
        // --help, not only the README.
        for prefix in [
            "CLAUDE",
            "ANTHROPIC_",
            "COPILOT_",
            "CODEX_",
            "CURSOR_",
            "AI_",
        ] {
            assert!(
                super::HELP_TAIL.contains(prefix),
                "the env-hygiene note must name the {prefix} deny prefix"
            );
        }
    }

    /// `--help`'s CONFIG line and the starter's header name the path the loader
    /// resolves on THIS machine, never a literal: measured 2026-09-22 on 0.90.0,
    /// both printed the Unix `~/.config/aterm/aterm.toml` on a Windows box
    /// whose window was reading `%APPDATA%\aterm\aterm.toml`.
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

    /// The `allow_notifications` comment says where delivery happens on each
    /// platform that delivers — macOS by subprocess, Windows in-process
    /// (notify.rs's `Shell_NotifyIcon` balloon) — and that Linux has none. It
    /// used to name macOS alone, as if the other platforms dropped the request.
    #[test]
    fn starter_config_says_where_notifications_are_delivered_per_platform() {
        let block: Vec<&str> = super::STARTER_CONFIG
            .lines()
            .skip_while(|l| !l.starts_with("# allow_notifications ="))
            .take_while(|l| {
                l.starts_with("# allow_notifications =")
                    || l.starts_with("#                                  #")
            })
            .collect();
        let text = block.join("\n");
        assert!(block.len() >= 3, "{text}");
        for needle in [
            "terminal-notifier",
            "osascript",
            "Windows",
            "notification-area balloon/toast",
            "Shell_NotifyIcon",
            "Linux has no delivery",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
        // The prose is the code's claim: the platforms it says deliver are the
        // ones `notify` builds a delivery host for.
        assert_eq!(
            crate::notify::delivery_available(),
            cfg!(any(target_os = "macos", windows))
        );
    }

    /// The `shell` comment names the Windows default in the order the spawn
    /// takes it (`aterm-pty`'s `select_shell`: pwsh, powershell, `%COMSPEC%`,
    /// then cmd) — it used to skip `%COMSPEC%` — and where to see the one this
    /// machine resolves.
    #[test]
    fn starter_config_names_the_windows_default_shell_order() {
        let shell_block: String = super::STARTER_CONFIG
            .lines()
            .skip_while(|l| !l.starts_with("# shell = "))
            .take(6)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            shell_block.contains("(Windows: pwsh > powershell > %COMSPEC% > cmd;"),
            "{shell_block}"
        );
        assert!(
            shell_block.contains("`aterm doctor` names the one this machine gets"),
            "{shell_block}"
        );
    }

    #[test]
    fn starter_config_discloses_timing_defaults_and_platform_limits() {
        let line_for = |key: &str| {
            super::STARTER_CONFIG
                .lines()
                .find(|line| line.trim_start().starts_with(&format!("# {key} =")))
                .unwrap_or_else(|| panic!("missing starter line for {key}"))
        };

        assert!(super::HELP_TAIL.contains("launch/session settings disclose their timing"));
        assert!(
            super::STARTER_CONFIG.contains(
                "renderer/initial-grid settings require relaunch, and session settings require a new session"
            )
        );

        let cursor_break = line_for("cursor_break_ligatures");
        assert!(cursor_break.contains("= true"), "{cursor_break}");
        assert!(cursor_break.contains("default false"), "{cursor_break}");

        // The [machine] host settings are disclosed with their defaults and the
        // ways they apply (as the window opens, a session once a day, a pass after an edit;
        // `aterm pkg machine apply` now) — and the block sits ABOVE
        // `[key_sequences]`/`[keybindings]`.
        let universal_control = line_for("universal_control");
        assert!(
            universal_control.contains("= \"off\""),
            "{universal_control}"
        );
        assert!(
            universal_control.contains("\"leave\""),
            "{universal_control}"
        );
        let noindex = line_for("spotlight_noindex");
        assert!(noindex.contains("= true"), "{noindex}");
        assert!(noindex.contains(".noindex"), "{noindex}");
        let machine_at = super::STARTER_CONFIG.find("# [machine]").unwrap();
        let sequences_at = super::STARTER_CONFIG.find("# [key_sequences]").unwrap();
        assert!(
            machine_at < sequences_at,
            "[machine] must precede the tail tables"
        );
        // THE COMMAND IS SPLIT BY THE COMMENT'S OWN WRAP, and the wrap is not free
        // to change: `settings.rs` derives its category list from these `# --- <label>`
        // headers, so reflowing this block to keep the command on one line adds a
        // category and turns `category_layout_matches_grouping_table` red (measured
        // 2026-09-14). Assert the command the way the file can actually carry it.
        let machine_block = &super::STARTER_CONFIG[machine_at.saturating_sub(400)..machine_at];
        assert!(
            machine_block.contains("aterm pkg")
                && machine_block.contains("machine apply` applies now"),
            "the starter config must tell the reader that `aterm pkg machine apply` \
             applies these now, even though the comment wrap splits the command"
        );

        let colorspace = line_for("window_colorspace");
        assert!(
            colorspace.contains("macOS GPU CAMetalLayer"),
            "{colorspace}"
        );
        let opacity = line_for("background_opacity");
        assert!(opacity.contains("macOS GPU window glass"), "{opacity}");
        assert!(opacity.contains("other renderers stay solid"), "{opacity}");
        let audio = line_for("trail_sounds");
        assert!(audio.contains("macOS-only"), "{audio}");
        assert!(audio.contains("inert elsewhere"), "{audio}");
        let sdr_glow = line_for("cursor_glow_sdr_boost");
        assert!(sdr_glow.contains("GPU-only"), "{sdr_glow}");
        let stream_fade = line_for("stream_fade");
        assert!(stream_fade.contains("set true to enable"), "{stream_fade}");
        assert!(stream_fade.contains("default OFF"), "{stream_fade}");
        // The pastejacking confirm has a surface on every platform now — the
        // starter config must name all three, and never resurrect the audited
        // "no prompt elsewhere" caveat.
        let paste = line_for("confirm_multiline_paste");
        assert!(paste.contains("macOS sheet"), "{paste}");
        assert!(paste.contains("Linux in-window banner"), "{paste}");
        let cos = line_for("copy_on_select");
        assert!(cos.contains("OFF on Linux"), "{cos}");
    }

    #[test]
    fn help_and_starter_document_smart_title_privacy() {
        for key in [
            "descriptive_titles",
            "title_summary_provider",
            "title_summary_model",
            "title_summary_endpoint",
            "title_summary_token_file",
            "title_summary_timeout_seconds",
            "title_summary_proxy_mode",
            "title_summary_ca_file",
            "title_summary_interval_seconds",
            "title_summary_context_lines",
            "title_summary_include_output",
            "title_summary_allow_remote",
            "tab_title_format",
            "window_title_format",
        ] {
            assert!(
                super::HELP_TAIL.contains(key),
                "--help must make smart-title setting `{key}` discoverable"
            );
            assert!(
                super::STARTER_CONFIG.contains(&format!("# {key} =")),
                "the starter config must document smart-title setting `{key}`"
            );
        }
        assert!(super::STARTER_CONFIG.contains("sends nothing anywhere"));
        assert!(super::STARTER_CONFIG.contains("NEVER put a raw token here"));
        assert!(super::STARTER_CONFIG.contains("title_summary_allow_remote = false"));
        assert!(super::STARTER_CONFIG.contains("filtering is heuristic"));
        assert!(super::STARTER_CONFIG.contains("private per-process ephemeral Ollama"));
        assert!(
            !super::STARTER_CONFIG
                .contains("title_summary_endpoint = \"http://127.0.0.1:11434/api/chat\"")
        );
    }

    #[test]
    fn starter_config_keys_all_deserialize() {
        // Every commented top-level `key = value` line the `--write-config` starter
        // ships must, when uncommented, deserialize into `Config`. A TYPE-mismatched
        // example (e.g. the old `bidi = false` against an `Option<String>` field)
        // aborts the WHOLE `aterm_toml::from_str::<Config>` at load, so `load_config` falls
        // back to `Config::default()` and silently discards the user's entire config.
        // This gate makes the discoverability surface honest: an uncommentable line
        // can never reach the starter again.
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
            // A commented table header ([keybindings]): keys below it are table-scoped
            // and not valid as a standalone top-level document, so stop checking.
            if rest.starts_with('[') {
                in_table = true;
                continue;
            }
            // Only top-level `ident = …` lines (skip prose comments / section rules).
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
        // The security opt-ins were once BELOW the `[keybindings]` table header, so the
        // table-scope skip above silently never checked them (and a user uncommenting
        // both the table and an opt-in aborted the whole config). They MUST stay above
        // `[keybindings]` (the last section) — assert each was actually reached here.
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
        // The M2 "ink that dries" keys default OFF in an absent config; the
        // generated starter file opts in. Guard that this intentional starter
        // choice and its duration never silently drop out of the sample.
        for must in ["stream_fade", "stream_fade_ms"] {
            assert!(
                checked.iter().any(|k| k == must),
                "STARTER_CONFIG dropped `{must}` — keep the explicit starter-file \
                 stream-fade opt-in and its duration together."
            );
        }
        for inert in [
            "[sparkle_words.orca]",
            "materialize =",
            "ink_text =",
            "phosphor =",
        ] {
            assert!(
                !super::STARTER_CONFIG.contains(inert),
                "new starter configs must not advertise compatibility-only `{inert}`"
            );
        }
    }

    /// `aterm help config` tells the reader HOW MANY keys the starter ships, and
    /// that count lives in another crate (`aterm-cli`'s manual) that cannot read
    /// this private const. So the number is pinned HERE, where the starter is
    /// edited: growing the starter reds this test, and the message names the one
    /// other copy to move. The count is of DISTINCT key names over every
    /// commented `key = …` line, table-scoped ones included — the same thing a
    /// reader counts in the written file.
    #[test]
    fn starter_config_key_count_matches_the_manual() {
        let mut keys: Vec<&str> = Vec::new();
        for raw in super::STARTER_CONFIG.lines() {
            let line = raw.trim_start();
            let Some(rest) = line.strip_prefix('#') else {
                continue;
            };
            let rest = rest.trim_start();
            if rest.starts_with('[') {
                continue;
            }
            let Some((key, _)) = rest.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            {
                keys.push(key);
            }
        }
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(
            keys.len(),
            157,
            "the starter config's key count moved — update the `157 keys` line in \
             `aterm help config` (crates/aterm-cli/src/manual.rs, CONFIG_PAGE) and \
             this number together"
        );
    }

    /// The `[matrix_rain]` starter block is TABLE-scoped, which the `in_table`
    /// latch above never validates (a known blind spot: table-scoped keys are
    /// only checked line-by-line as top-level docs, which they are not).
    /// Uncomment the WHOLE block (strip the leading `# `) and parse it as one
    /// document: every example line must deserialize into [`Config`] — a
    /// type-mismatched value would abort the entire config load — AND every
    /// documented key must actually land in [`crate::app_config::MatrixRainConfig`]
    /// (serde ignores unknown keys, so a typo'd starter key would otherwise be
    /// silently dead documentation).
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
            // Only `ident = …` lines (prose comment lines have no bare key).
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
        // Every documented knob must be Some — a typo'd key in the starter
        // would leave its field None (serde(default) ignores unknown keys).
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
        assert_eq!(mr.materialize, None, "starter omits inert materialize");
        assert_eq!(mr.ink_text, None, "starter omits inert ink_text");
        assert_eq!(mr.phosphor, None, "starter omits inert phosphor");
        assert!(mr.seed.is_some(), "starter documents `seed`");
        // The starter documents the DEFAULT-OFF posture (costume mode is opt-in).
        assert_eq!(mr.enabled, Some(false), "the starter example ships OFF");
    }
}
