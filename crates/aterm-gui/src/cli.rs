// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI argument parsing for `aterm-gui`. Pure, no `App` coupling: parses
//! `aterm-gui [OPTIONS] [-e CMD ARGS… | --help | --version]`, promoting each
//! `ATERM_*` knob to a first-class flag (flag > env) by setting the matching env
//! var so the existing env > config > default precedence funnel is reused.

/// Parsed CLI: the `-e` command to run instead of `$SHELL` (if any), the
/// `--working-directory` to start it in (if any), whether to `--hold` the
/// window open after the command exits, and whether `--headless` was passed.
pub(crate) struct Cli {
    pub(crate) exec_command: Option<Vec<String>>,
    pub(crate) cwd: Option<String>,
    pub(crate) hold: bool,
    /// `--headless` appeared on the command line. The flag ALSO sets
    /// `$ATERM_HEADLESS=1` (the shared funnel every other knob uses), so this
    /// field is not what arms the mode — it only records the SOURCE, so the
    /// startup announcement can name the flag rather than the environment.
    pub(crate) headless: bool,
}

/// Whether this launch runs headless, and — because a misread of this decision
/// costs a full misdiagnosis (a harness that hangs on a socket that never
/// appears) — WHY, in words the startup announcement can print.
///
/// Headless has exactly ONE meaning and TWO equivalent ways to ask for it: the
/// `--headless` flag and `$ATERM_HEADLESS`. The flag is the canonical spelling
/// and simply sets the env var ([`flag_env`]), so both arrive at the single read
/// site in `main` through the same funnel every other `ATERM_*` knob uses.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HeadlessArming {
    /// No window is ever created: engine + PTY + control socket only.
    /// The payload names the source for the stderr announcement.
    Armed(HeadlessSource),
    /// Windowed, and nothing asked otherwise — the ordinary interactive launch.
    Windowed,
    /// `$ATERM_HEADLESS` is SET, but to a DISABLING value (`0`, `off`, or
    /// empty), and no `--headless` flag came with it. The launch is windowed.
    ///
    /// This case exists to be LOUD. A script that exports the variable and then
    /// waits for a control socket is one typo away from waiting forever, so the
    /// binary says on stderr that the variable it set did not arm the mode,
    /// rather than starting a window and hanging in silence. The payload is the
    /// rejected value, echoed back so the diagnostic names the real input.
    Refused(String),
}

/// Which of the two equivalent spellings armed headless mode.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HeadlessSource {
    /// `--headless` on the command line.
    Flag,
    /// `$ATERM_HEADLESS` set to an enabling value.
    Env,
}

impl HeadlessSource {
    /// How the announcement spells this source.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            HeadlessSource::Flag => "--headless",
            HeadlessSource::Env => "$ATERM_HEADLESS",
        }
    }
}

/// Decide headless mode from the `--headless` flag and the VALUE of
/// `$ATERM_HEADLESS` (`None` = unset). Pure, so the whole truth table is a unit
/// test rather than a launch experiment.
///
/// The flag wins outright (it overwrites the variable on its way in, so `flag >
/// env` holds even here). Otherwise the variable follows the same enabling
/// convention as its sibling `$ATERM_NO_CONTROL_SOCK` — `0`, `off`
/// (case-insensitive), and empty DISABLE; any other value enables — instead of
/// bare presence. Presence semantics made `ATERM_HEADLESS=0` mean *headless*,
/// which is the one reading no caller has ever intended.
#[must_use]
pub(crate) fn headless_arming(flag: bool, env: Option<&str>) -> HeadlessArming {
    if flag {
        return HeadlessArming::Armed(HeadlessSource::Flag);
    }
    match env {
        None => HeadlessArming::Windowed,
        Some(v) if v.is_empty() || v == "0" || v.eq_ignore_ascii_case("off") => {
            HeadlessArming::Refused(v.to_string())
        }
        Some(_) => HeadlessArming::Armed(HeadlessSource::Env),
    }
}

/// The `--help` text. A clean OPTIONS section where every user-facing flag shows
/// its argument, a one-line description, AND its `[env: ATERM_*]` equivalent, plus
/// an ENVIRONMENT section — the discoverable surface an AI (or human) reads to
/// drive aterm without source-diving. Kept as a single `concat!` so a no-arg /
/// Finder launch never touches it. Each ATERM_* knob enumerated below also has a
/// first-class flag (precedence: flag > env > config > default).
const HELP_HEAD: &str = concat!(
    "aterm-gui — a fast, hardened terminal\n\n",
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
    "        --font-px <px>             Glyph size in physical px (6..=200).\n",
    "                                       [env: ATERM_FONT_PX]\n",
    "        --font <name>              Primary font FAMILY (e.g. \"JetBrains Mono\").\n",
    "                                       [env: ATERM_FONT]\n",
    "        --shell <name|path>        Interactive shell to spawn. Discovery-resolved:\n",
    "                                   \"bash\" finds Git Bash even off PATH; \"pwsh\",\n",
    "                                   \"cmd\", \"wsl\", \"nu\", or an absolute path also work.\n",
    "                                       [env: ATERM_SHELL] [config: shell]\n",
    "        --scale <f>                Force the render scale factor (font + padding).\n",
    "                                   In a window this overrides the display scale;\n",
    "                                   headless it makes the `image` capture render at\n",
    "                                   that DPI (e.g. --scale 2 ≈ a 2× Retina window).\n",
    "                                       [env: ATERM_FORCE_SCALE]\n",
    "        --gpu                      Force GPU rendering — the DEFAULT (wgpu: Metal\n",
    "                                   on macOS, Vulkan on Linux; auto CPU fallback).\n",
    "                                       [env: ATERM_GPU]\n",
    "        --cpu                      Force the CPU renderer (overrides --gpu/config).\n",
    "        --containment <mode>       Containment mode: master|user|safety|containment.\n",
    "                                       [env: ATERM_CONTAINMENT_MODE]\n",
    "        --sandbox                  Shorthand for --containment containment.\n",
    "        --no-sandbox               Shorthand for --containment user.\n",
    "        --control-sock <path>      Bind the control socket at <path> (or 0/off to\n",
    "                                   disable).               [env: ATERM_CONTROL_SOCK]\n",
    "        --no-control-sock          Disable the control socket.\n",
    "                                       [env: ATERM_NO_CONTROL_SOCK]\n",
    "        --headless                 No window; engine + control socket only. Exactly\n",
    "                                   equivalent to the env var; either way the launch\n",
    "                                   announces the mode on stderr.\n",
    "                                       [env: ATERM_HEADLESS]\n",
    "        --columns <n>              Initial width in columns (20..=500).\n",
    "        --lines <n>                Initial height in rows (5..=300).\n",
    "        --shell-integration        OSC 133/633 command marks (blocks/cwd/title) — ON by\n",
    "                                       default; this flag is a no-op. [env: ATERM_SHELL_INTEGRATION]\n",
    "        --no-shell-integration     Disable shell-integration marks (default is on).\n",
    "                                       [env: ATERM_NO_SHELL_INTEGRATION]\n",
    "        --no-procedural-glyphs     Disable procedural box/Powerline glyphs.\n",
    "                                       [env: ATERM_NO_PROCEDURAL_GLYPHS]\n",
    "        --trace-latency            Print PTY→present latency samples to stderr.\n",
    "                                       [env: ATERM_TRACE_LATENCY]\n",
    "        --verbose                  Verbose diagnostics.       [env: ATERM_VERBOSE]\n",
    "        --diagnose                 Print a diagnostics report (version, build,\n",
    "                                   renderer, capabilities, config, env) and exit.\n",
    "        --list-actions             List the bindable [keybindings] action names\n",
    "                                   and exit.\n",
    "        --validate-config          Parse the config file, report OK/errors, exit\n",
    "                                   0 if valid (non-zero if not).\n",
    "        --list-fonts               List the font search dirs and discoverable\n",
    "                                   font families, then exit.\n",
    "        --show-config              Print the effective resolved config (env >\n",
    "                                   config > default) and exit.\n",
    "        --write-config             Write a documented starter aterm.toml (every\n",
    "                                   key commented) if absent, then exit.\n",
    "        --list-keybinds            List built-in + configured [keybindings] and\n",
    "                                   [key_sequences], plus bindable actions, then exit.\n",
    "        --show-face [family]       Print the resolved font face (path + metrics)\n",
    "                                   for [family] (or the configured font) and exit.\n",
    "        --list-themes              List the built-in colour themes and exit.\n",
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
    "    Cmd-click         Open a hyperlink / detected URL (http/https/mailto).\n",
    "    Cmd-F             Find (screen + scrollback): type, Enter/Shift-Enter, Esc.\n",
    "    Cmd-S / Cmd-R     Emacs search forward / backward; repeat to navigate + wrap.\n",
    "    Cmd-,             Open the native Settings tab; Manual edits aterm.toml.\n",
    "    Cmd-N             Open a new window (same process, same sessions).\n",
    "    Cmd-T             Open a new tab (new shell, same window).\n",
    "    Cmd-W             Close the active tab; closing the last tab quits.\n",
    "    Cmd-Shift-] / [   Next / previous tab (wraps).   Cmd-1..9  Nth tab.\n",
    "                      Tab state shows in the title as [active/total].\n\n",
);

/// See [`KEYS_HELP`] (macOS) — the Linux / non-macOS shortcut set.
#[cfg(not(target_os = "macos"))]
const KEYS_HELP: &str = concat!(
    "KEYS (in the window — Linux has no menu bar, so these chords ARE the app menu):\n",
    "    Ctrl+Shift+C / +V    Copy selection / paste (control-stripped, bracketed).\n",
    "    Ctrl+= / Ctrl+-      Zoom the font in / out.   Ctrl+0  Reset zoom.\n",
    "    Ctrl+click           Open a hyperlink / detected URL (http/https/mailto).\n",
    "    Ctrl+Shift+F         Find (screen + scrollback): type, Enter/Shift-Enter, Esc.\n",
    "    Ctrl+Shift+S         Open the native Settings tab on Top Settings.\n",
    "                         Use Manual there for assisted aterm.toml editing.\n",
    "    Ctrl+Shift+A         About: name / version / build in a simple info dialog\n",
    "                         (OK, the close dot, or Esc closes). This IS the \"About\" menu.\n",
    "    Ctrl+Shift+N         Open a new window (same process, same sessions).\n",
    "    Ctrl+Shift+T         Open a new tab (new shell, same window).\n",
    "    Ctrl+Shift+W         Close the active tab; closing the last tab quits.\n",
    "    Ctrl+Shift+Right/Left  Next / previous tab (or Ctrl+PgUp / Ctrl+PgDn).\n",
    "                         Open tabs show in the strip at the top of the window.\n\n",
);

const HELP_TAIL: &str = concat!(
    "ENVIRONMENT (each has a flag above; precedence is flag > env > config > default):\n",
    "    ATERM_FONT_PX=N            Glyph size in physical pixels.\n",
    "    ATERM_FONT=<name>          Primary font family.\n",
    "    ATERM_FORCE_SCALE=<f>      Force the render scale factor (font + padding).\n",
    "    ATERM_GPU=1                Force GPU (already the DEFAULT; CPU is the auto fallback).\n",
    "    ATERM_CONTAINMENT_MODE=<m> master|user|safety|containment (fail-closed).\n",
    "    ATERM_CONTROL_SOCK=<path>  Control socket path (0/off disables it).\n",
    "    ATERM_NO_CONTROL_SOCK=1    Disable the control socket.\n",
    "    ATERM_HEADLESS=1           No window; engine + control socket only — the exact\n",
    "                               equivalent of --headless. 0/off/empty do NOT arm it\n",
    "                               (and say so on stderr rather than starting a window\n",
    "                               under a script that is waiting for a socket).\n",
    "    ATERM_SHELL_INTEGRATION=1  OSC 133/633 command marks (ON by default; this is a no-op).\n",
    "    ATERM_NO_SHELL_INTEGRATION=1  Disable shell-integration marks (default is on).\n",
    "    ATERM_NO_PROCEDURAL_GLYPHS=1  Disable procedural box/Powerline glyphs.\n",
    "    ATERM_TRACE_LATENCY=1      Print PTY→present latency samples.\n",
    "    ATERM_VERBOSE=1            Verbose diagnostics.\n\n",
    "ENVIRONMENT (no flag; opt-in):\n",
    "    ATERM_AI_HINT=1           Inject a one-line, dim \"this terminal is AI-introspectable,\n",
    "                              drive it with aterm-ctl\" hint above the first prompt. OFF by\n",
    "                              default — a transparent terminal injects nothing; opt-in only.\n\n",
    "CHILD-SHELL ENV HYGIENE:\n",
    "    The spawned shell has every AI-agent context variable STRIPPED before exec —\n",
    "    CLAUDE*, ANTHROPIC_*, COPILOT_*, CODEX_*, CURSOR_*, AI_*, and _DEVTOOL_* — so they\n",
    "    never leak into your session. An inner agent whose context vars went missing was\n",
    "    sanitized here by design (aterm_types::env_sanitize), not lost.\n\n",
    "CONFIG:  ~/.config/aterm/aterm.toml  (live settings reload; launch/session settings disclose their timing; precedence env > config > default)\n",
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
    "              tab_status_badge.\n",
    "  Cursor      serious_mode (mute all sound/decorative effects), motion,\n",
    "              cursor_style, cursor_blink, cursor_trail, cursor_trail_style\n",
    "              (the LUMEN aurora), cursor_trail_color/_accent/_intensity/_radius,\n",
    "              cursor_trail_ms/_length/_ring, cursor_trail_bloom (+_strength/_radius).\n",
    // Sound is its own help block because it is its own Settings box now (the
    // owner's Sound menu); listing it under Cursor is what made the volume dial
    // hard to find in the first place.
    "  Sound       trail_sounds (master), trail_sound_volume (scales every synth\n",
    "              voice), trail_sound_style (the typing sound: auto | glass bell |\n",
    "              warm pluck | glitter | ice chime | droplet | pew | zap | tick |\n",
    "              crackle | mechanical | typewriter | marimba | felt), tone_melody,\n",
    "              trail_sound_bed (the ambient texture; default off),\n",
    "              trail_sound_riff (the sing-along song — the loudest voice),\n",
    "              bell_sound (the audible BEL beep; macOS/Windows),\n",
    "              sparkle_words.profanity.bonk[_detonation] (the curse bonk).\n",
    "  Text        ligatures, font_features, bidi, ambiguous_width,\n",
    "              text_blending (linear-corrected | linear), font_thicken (macOS),\n",
    "              stem_gamma (aliases $ATERM_STEM_GAMMA),\n",
    "              font_variation [\"wght=450\", ...], font_weight,\n",
    "              font_weight_dark_nudge (variable fonts, e.g. SF Mono),\n",
    "              font_family_bold/_italic/_bold_italic, font_synthetic_style,\n",
    "              fallback_fonts [ordered], symbol_font, emoji_font\n",
    "              (config > $ATERM_{FALLBACK,SYMBOL,EMOJI}_FONT alias > discovery).\n",
    "  Behaviour   gpu, scrollback_lines, columns, lines, copy_on_select,\n",
    "              option_as_meta, search_history_lines, focus_boost (Windows:\n",
    "              shell priority follows window focus; default on).\n",
    "  Security    allow_window_ops, allow_notifications, allow_palette_reconfigure,\n",
    "              allow_kitty_file_transfer  (all opt-in, default off).\n",
    "  Keys        [keybindings] \"chord\"=\"action\"; [key_sequences] \"chord\"=raw bytes.\n",
);

/// A documented starter config written by `--write-config`. Linux-tuned (real
/// key names, sensible non-macOS defaults); EVERY line is commented, so writing it
/// changes nothing — it just makes the settings surface
/// DISCOVERABLE for a new user who has no `aterm.toml` yet.
const STARTER_CONFIG: &str = "\
# aterm — ~/.config/aterm/aterm.toml
# Every setting is optional; uncomment to override. Live settings reload on save;
# renderer/initial-grid settings require relaunch, and session settings require a new session.
# Environment (ATERM_*) and CLI flags take precedence over this file.

# --- shell --------------------------------------------------------------------
# shell = \"bash\"        # interactive shell. Discovery-resolved: \"bash\" finds Git
#                       # Bash even if it is not on PATH; \"pwsh\", \"cmd\", \"wsl\",
#                       # \"nu\", or an absolute path also work. Unset = platform
#                       # default (Windows: pwsh > powershell > cmd). Override at
#                       # launch with --shell or ATERM_SHELL.
# shell_args = [\"-l\", \"-i\"]  # extra argv after the shell (e.g. a login bash)

# --- appearance ---------------------------------------------------------------
# font_family = \"JetBrains Mono\"  # any installed monospace family, or a .ttf path
# font_family_bold = \"JetBrains Mono Bold\"      # real bold face (unset = auto-discover)
# font_family_italic = \"JetBrains Mono Italic\"  # real italic face
# font_family_bold_italic = \"JetBrains Mono Bold Italic\"
# font_synthetic_style = true      # false: never fake bold/italic (regular when no real face)
# fallback_fonts = [\"Sarasa Mono\"] # ordered Unicode fallbacks; outranks $ATERM_FALLBACK_FONT
# symbol_font = \"Symbols Nerd Font\"   # monochrome symbol fallback; outranks $ATERM_SYMBOL_FONT
# emoji_font = \"Noto Color Emoji\"     # colour-emoji face; outranks $ATERM_EMOJI_FONT
# font_px = 16                     # physical px (13 looks small on a 100+ DPI panel)
# theme = \"Default\"               # a built-in scheme, or \"dark:<name>,light:<name>\"
# foreground = \"#C8D3F5\"
# background = \"#1A1B26\"
# cursor_style = \"block\"          # block | bar
# cursor_blink = true
# selection_color = \"#33415E\"
# selection_foreground = \"#FFFFFF\" # selected-text ink; unset = auto contrast floor
# selection_inactive = false       # dim the selection band while the window is unfocused
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

# --- motion / cursor aurora -----------------------------------------------------
# serious_mode = false            # mute sounds + hide decorative effects; underlying effect settings return when switched off
# robi = true                      # Robi the helper robot lives on your terminal (walks your typed row, ladder up, tab-bar monkey bars, tips above his head); type robi to make him greet you (default OFF)
# motion = \"auto\"                 # auto (live Reduce Motion on macOS; sampled at Windows window attach; no OS query elsewhere) | full | reduced
# load_adaptive_motion = true      # drop effects under sustained render overload; false = never shed (motion=\"full\" also forces effects on)
# cursor_trail = true              # the cursor motion trail + light crown (default ON)
# cursor_trail_style = \"rainbow kitty pet\"  # rainbow kitty pet (DEFAULT; the banded rainbow ribbon with the full-body cat that walks, runs and pounces along your line; aliases \"kitty pet\"/\"pet kitty\") | rainbow kitty (the same ribbon under the FLYING kitty head; old spellings \"nyan rainbow\"/\"nyan\"/\"rainbow\" still work) | phaser (full-spectrum additive hue sweep) | comet (cadence-comet body + light crown) | lumen | sparkle | fire | laser | water | beam (steady power-down tube of cool light) | off
# cursor_trail_color = \"#50FA7B\"      # base colour (default: the theme's cursor colour)
# cursor_trail_accent = \"#7AA2F7\"     # comet-tail / ring colour (default: brightened base)
# cursor_trail_ms = 260                # fade duration in ms (30..=2000)
# cursor_trail_length = 24             # max comet length in cells (1..=512)
# cursor_trail_intensity = 0.7         # aurora brightness 0.0..=1.0
# cursor_trail_radius = 0.6            # bloom-crown radius in cells (0.0..=2.0)
# cursor_trail_wake_ms = 300           # rainbow-kitty TYPING WAKE: ms of recent travel the
#                                      # plume under your words shows (0 = off, max 1500)
# cursor_trail_ring = true             # expanding landing \"ping\" ring on a jump (default ON)
# --- sound (Settings > Cursor & Motion > Sound) -------------------------------
# trail_sounds = true              # macOS-only trail-style audio (parsed but inert elsewhere); silent whenever the trail is (default ON)
# trail_sound_volume = 0.4         # 0.0..=1.0 trail sound level (default 0.4 ~= -22 dBFS peaks, far under the bell); does NOT scale bell_sound
# trail_sound_style = \"auto\"     # typing sound: auto = follow the trail style; or an instrument for every keystroke whatever the trail looks like:
#                                  #   glass bell | warm pluck | glitter | ice chime | droplet | pew | zap | tick | crackle  (the nine palettes, by sound)
#                                  #   mechanical (keyboard click + thock) | typewriter (clack + platen, bell + carriage on Enter) | marimba | felt (muted piano)
#                                  #   aliases: the trail-style names (water, comet, rainbow kitty, ...), bell, raindrop, mech, thock, piano, clack
# tone_melody = true               # the melody leans with the typed line's inferred mood (on-device, typed input only); default ON and deliberately subtle
# trail_sound_bed = false          # the continuous ambient BED texture behind the notes (default OFF; true re-enables the per-style drone)
# trail_sound_riff = true          # the held-key SING-ALONG song (the loudest voice); false quiets just the song and keeps its visuals (default ON)
# bell_sound = true                # the audible BEL beep (macOS NSBeep / Windows MessageBeep); false keeps the visual flash and window attention (default ON)
# cursor_trail_bloom = true            # GPU-only soft halo around the comet (default ON)
# cursor_trail_bloom_strength = 0.85   # 0.0..=3.0 (halo intensity)
# cursor_trail_bloom_radius = 2.2      # 0.5..=8.0 (half-res blur texels)
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
# stem_gamma = 1.0                 # aesthetic stem weight (<1 thicker, >1 thinner);
#                                  # aliases $ATERM_STEM_GAMMA (env wins)
# font_variation = [\"wght=450\"]    # variable-font axes (clamped to fvar; default = Regular / wght=400)
# font_weight = 450                # wght shorthand; wins over a font_variation wght entry
# font_weight_dark_nudge = 0       # extra wght on DARK themes (applied only when grid-safe)

# --- behaviour ----------------------------------------------------------------
# gpu = false                      # GPU rendering is ON by default (auto CPU fallback); set false / --cpu / $ATERM_CPU to force CPU
# copy_on_select = true            # auto-copy mouse selection to clipboard (DEFAULT on; false opts out)
# show_build_badge = false         # OPTIONAL floating top-right v<version>·<build> pill — DEFAULT off
                                   # (the version lives in the menu bar: the v<version> menu opens About)
# confirm_multiline_paste = true   # macOS/Windows: confirm unbracketed multiline paste; preserved but no prompt elsewhere
# focus_boost = true               # Windows: boost the visible shells' priority while aterm is focused (DEFAULT on; no-op elsewhere)

# --- security opt-ins (all default OFF) ---------------------------------------
# allow_window_ops = false         # XTWINOPS title/text-grid-size reports; GUI manipulation and most geometry requests are ignored
# allow_notifications = false
# allow_palette_reconfigure = false
# allow_kitty_file_transfer = false

# --- sparkle words (purely visual; NEVER affects copied text, logs, or recordings)
# Decorate matched words: a randomized SPARKLE over profanity (the \"fuck\" family in
# every major language) and a steady CAT-PAW over cat/kitty words. Both live toys are
# ON by default. Retained orca/cetacean settings parse for compatibility but are suspended
# and have no effect. Use the independent Sparkle words and Keyword kitties
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
# [sparkle_words.ink]              # animated glyph-ink shimmer (all classes but orca)
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
# reads, so there is exactly ONE config surface. Env always wins over config
# (ATPKG_ACCOUNT / ATPKG_REGISTRY / ATPKG_INDEX_REPO / ATPKG_DISABLE). Inert in
# builds without a pinned root key — Settings ▸ Packages shows the live posture. --
# [packages]
# enabled = true                   # master for the background tools loop (Settings ▸ Packages)
# auto_update = true               # run `atpkg update` on the 6-hour cadence
#                                  #   (both loop gates are read at LAUNCH)
# auto_install = false             # ALSO bootstrap-install missing default-set members —
#                                  #   multi-GB consent; the Settings switch is the click
# seed_install = true              # install the BUNDLED seed on first launch (batteries
#                                  #   included — the bytes ship inside the app; false
#                                  #   turns the first run into an announced offer)
# account = \"alabsystems\"          # index owner; omit → the compiled default
# channel = \"stable\"               # the pin set install/update/rollback resolve against
# include = [\"ay\", \"ty\"]           # narrowing-only filters over the SIGNED index
# exclude = []                     #   default set (nothing outside the index installs)
# [packages.links]                 # local mode / private repos, per program:
# ay = \"~/ay\"                      #   path → managed dev-link (registry skipped)
# orc = \"alabsystems/orc\"        #   owner/repo → private fetch override
#                                  #   (signature verification unchanged either way)

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

/// Set an environment variable so a downstream env read (the existing precedence
/// funnel) observes the CLI flag. The flag OVERWRITES any inherited env value,
/// which is exactly the desired `flag > env` precedence; every existing
/// `env::var(...)` site is then byte-identical whether the knob came from a flag
/// or the environment. SAFETY: called only from [`parse_cli`], which runs at the
/// very top of `main` before any thread is spawned (no concurrent env access), so
/// the edition-2024 `set_var` safety contract holds.
fn flag_env(key: &str, val: &str) {
    // Single-threaded program startup (see fn doc) — no other thread can be
    // reading the environment concurrently — and routed through the workspace's
    // one lock-scoped env helper rather than a raw `set_var`.
    aterm_log::env::set(key, val);
}

/// Pull the next argument as the value for `flag`, exiting 2 with a hint if it is
/// missing. Used by the value-taking flags so they share one error shape.
fn flag_value(flag: &str, args: &mut impl Iterator<Item = String>) -> String {
    match args.next() {
        Some(v) => v,
        None => {
            eprintln!("aterm-gui: {flag} requires a value (try --help)");
            std::process::exit(2);
        }
    }
}

fn valid_font_px_flag(value: &str) -> bool {
    value
        .parse::<f32>()
        .is_ok_and(|px| px.is_finite() && (crate::FONT_PX_MIN..=crate::FONT_PX_MAX).contains(&px))
}

fn valid_initial_dimension_flag(value: &str, min: u16, max: u16) -> bool {
    value
        .parse::<u16>()
        .is_ok_and(|dimension| (min..=max).contains(&dimension))
}

/// CLI: `aterm-gui [OPTIONS] [-e CMD ARGS… | --help | --version]`.
/// `--help`/`--version` print and exit; an unknown option, a `-d` without a valid
/// directory, `-e` without a command, or a value flag missing its argument prints
/// a hint and exits 2 (no window launch). With no args (a Finder/.app launch) this
/// is a no-op and a normal interactive shell starts in the inherited working
/// directory. Each `ATERM_*` knob ALSO has a flag here; a flag sets the matching
/// env var ([`flag_env`]) so the existing env > config > default precedence funnel
/// is reused unchanged and `flag > env` falls out naturally (overwrite). Numeric
/// flags are validated here for a clean early error; containment is validated by
/// its own fail-closed funnel in `main`.
pub(crate) fn parse_cli(argv: Vec<std::ffi::OsString>) -> Cli {
    // Lossy conversion mirrors the binary era's `env::args()` UTF-8 boundary
    // (a non-UTF8 flag was a panic there; here it degrades to a usage error).
    let mut args = argv.into_iter().map(|a| a.to_string_lossy().into_owned());
    let mut cwd: Option<String> = None;
    let mut hold = false;
    let mut headless = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP_HEAD}{KEYS_HELP}{HELP_TAIL}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                // The DISPLAY version: semver + the compiler-provenance suffix
                // (+r.<slug> = upstream Rust, +t.<slug> = Trust fork) — what
                // the ship tool (aterm-release buildplan.rs) echoes into the
                // cut transcript as provenance.
                println!("aterm-gui {}", crate::build_info::version_display());
                std::process::exit(0);
            }
            // Diagnostics ("doctor"): print the report and exit (no window). Placed
            // after the env-setting flags so e.g. `--gpu --diagnose` reports the
            // effective renderer.
            "--diagnose" => {
                print!("{}", crate::diagnostics::collect().render());
                std::process::exit(0);
            }
            "--list-actions" => {
                for name in crate::keybinding::ACTION_NAMES {
                    println!("{name}");
                }
                std::process::exit(0);
            }
            "--validate-config" => {
                let (msg, ok) = crate::diagnostics::validate_config();
                println!("{msg}");
                std::process::exit(i32::from(!ok));
            }
            "--list-fonts" => {
                print!("{}", crate::diagnostics::list_fonts());
                std::process::exit(0);
            }
            "--show-config" => {
                print!("{}", crate::diagnostics::show_config());
                std::process::exit(0);
            }
            "--write-config" => {
                // Discoverability: drop a fully-documented starter config (every key
                // commented, so it changes nothing) where the loader looks for it.
                match crate::app_config::config_path() {
                    Some(path) if path.exists() => {
                        println!("config already exists: {}", path.display());
                        println!("(edit it directly — settings hot-reload on save)");
                    }
                    Some(path) => {
                        if let Some(dir) = path.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        match std::fs::write(&path, STARTER_CONFIG) {
                            Ok(()) => {
                                println!("wrote a documented starter config: {}", path.display());
                            }
                            Err(e) => {
                                eprintln!("could not write {}: {e}", path.display());
                                std::process::exit(1);
                            }
                        }
                    }
                    None => {
                        eprintln!("could not resolve the config path ($HOME/$XDG_CONFIG_HOME)");
                        std::process::exit(1);
                    }
                }
                std::process::exit(0);
            }
            "--list-keybinds" => {
                print!("{}", crate::diagnostics::list_keybinds());
                std::process::exit(0);
            }
            "--show-face" => {
                // Optional family argument; empty falls back to the effective
                // font_family (env > config). Exits non-zero if it does not resolve.
                let family = args.next().unwrap_or_default();
                let (msg, ok) = crate::diagnostics::show_face(&family);
                print!("{msg}");
                std::process::exit(i32::from(!ok));
            }
            "--list-themes" => {
                print!("{}", crate::diagnostics::list_themes());
                std::process::exit(0);
            }
            // Windows: install/remove the "Open aterm here" Explorer context menu
            // (per-user HKCU verb on directories/backgrounds/drives → `aterm-gui -d <path>`).
            #[cfg(windows)]
            "--install-context-menu" => {
                match crate::explorer_win::install() {
                    Ok(()) => println!(
                        "aterm-gui: installed the 'Open aterm here' Explorer context menu (per-user). \
                         Right-click a folder, its empty background, or a drive."
                    ),
                    Err(e) => {
                        eprintln!("aterm-gui: context-menu install failed: {e}");
                        std::process::exit(1);
                    }
                }
                std::process::exit(0);
            }
            #[cfg(windows)]
            "--uninstall-context-menu" => {
                let _ = crate::explorer_win::uninstall();
                println!("aterm-gui: removed the 'Open aterm here' Explorer context menu.");
                std::process::exit(0);
            }
            "-d" | "--working-directory" => {
                let dir = flag_value("-d/--working-directory", &mut args);
                if !std::path::Path::new(&dir).is_dir() {
                    eprintln!("aterm-gui: not a directory: {dir}");
                    std::process::exit(2);
                }
                cwd = Some(dir);
            }
            "--hold" => hold = true,
            // --- ATERM_* knobs promoted to first-class flags (flag > env). ---
            "--font-px" => {
                let v = flag_value("--font-px", &mut args);
                if valid_font_px_flag(&v) {
                    flag_env("ATERM_FONT_PX", &v);
                } else {
                    eprintln!(
                        "aterm-gui: --font-px expects a number from {} through {}, got '{v}' (try --help)",
                        crate::FONT_PX_MIN,
                        crate::FONT_PX_MAX,
                    );
                    std::process::exit(2);
                }
            }
            "--font" => flag_env("ATERM_FONT", &flag_value("--font", &mut args)),
            // --shell: the interactive shell to spawn. Discovery-resolved by the
            // PTY layer — "bash" finds Git for Windows off-PATH, "pwsh"/"cmd"/
            // "wsl"/"nu" resolve, an absolute path is verbatim. Sets ATERM_SHELL
            // (early, single-threaded → the set_var contract holds), which the
            // spawn resolver reads at highest precedence over config `shell`.
            "--shell" => flag_env("ATERM_SHELL", &flag_value("--shell", &mut args)),
            "--scale" => {
                let v = flag_value("--scale", &mut args);
                if v.parse::<f64>()
                    .map(|f| f.is_finite() && f > 0.0)
                    .unwrap_or(false)
                {
                    flag_env("ATERM_FORCE_SCALE", &v);
                } else {
                    eprintln!(
                        "aterm-gui: --scale expects a positive number, got '{v}' (try --help)"
                    );
                    std::process::exit(2);
                }
            }
            "--gpu" => {
                // Symmetric last-flag-wins precedence: an inherited or earlier
                // --cpu must not outrank this explicit later flag.
                // Startup, single-threaded (see flag_env).
                aterm_log::env::unset("ATERM_CPU");
                flag_env("ATERM_GPU", "1");
            }
            // CPU override: clear any inherited/earlier ATERM_GPU so the GPU path
            // is not taken (config `gpu = true` still loses to an explicit --cpu).
            "--cpu" => {
                // Startup, single-threaded (see flag_env).
                aterm_log::env::unset("ATERM_GPU");
                flag_env("ATERM_CPU", "1");
            }
            "--containment" => {
                flag_env(
                    "ATERM_CONTAINMENT_MODE",
                    &flag_value("--containment", &mut args),
                );
            }
            "--sandbox" => flag_env("ATERM_CONTAINMENT_MODE", "containment"),
            "--no-sandbox" => flag_env("ATERM_CONTAINMENT_MODE", "user"),
            "--control-sock" => {
                flag_env(
                    "ATERM_CONTROL_SOCK",
                    &flag_value("--control-sock", &mut args),
                );
            }
            "--no-control-sock" => flag_env("ATERM_NO_CONTROL_SOCK", "1"),
            // --headless: no window, engine + control socket only. Sets the env
            // var like every other knob (so the single read site in `main` is
            // unchanged and `flag > env` falls out of the overwrite), and records
            // the SOURCE so the startup announcement can name the flag.
            "--headless" => {
                flag_env("ATERM_HEADLESS", "1");
                headless = true;
            }
            "--columns" => {
                let v = flag_value("--columns", &mut args);
                if valid_initial_dimension_flag(&v, 20, 500) {
                    flag_env("ATERM_COLUMNS", &v);
                } else {
                    eprintln!(
                        "aterm-gui: --columns expects an integer from 20 through 500, got '{v}' (try --help)"
                    );
                    std::process::exit(2);
                }
            }
            "--lines" => {
                let v = flag_value("--lines", &mut args);
                if valid_initial_dimension_flag(&v, 5, 300) {
                    flag_env("ATERM_LINES", &v);
                } else {
                    eprintln!(
                        "aterm-gui: --lines expects an integer from 5 through 300, got '{v}' (try --help)"
                    );
                    std::process::exit(2);
                }
            }
            "--shell-integration" => flag_env("ATERM_SHELL_INTEGRATION", "1"),
            "--no-shell-integration" => flag_env("ATERM_NO_SHELL_INTEGRATION", "1"),
            "--no-procedural-glyphs" => flag_env("ATERM_NO_PROCEDURAL_GLYPHS", "1"),
            "--trace-latency" => flag_env("ATERM_TRACE_LATENCY", "1"),
            "--verbose" => flag_env("ATERM_VERBOSE", "1"),
            "-e" | "--command" => {
                let cmd: Vec<String> = args.by_ref().collect();
                if cmd.is_empty() {
                    eprintln!("aterm-gui: -e/--command requires a command (try --help)");
                    std::process::exit(2);
                }
                return Cli {
                    exec_command: Some(cmd),
                    cwd,
                    hold,
                    headless,
                };
            }
            // NOTE: verbs are NOT parsed here. `ship` briefly was, and that was the
            // whole defect — this parser is reached only when the mode fork already
            // chose the window, so a verb wired here is invisible at a terminal. The
            // front door (`crates/aterm/src/main.rs`) owns every verb in
            // `aterm_cli::Verb`, above the fork.
            other => {
                eprintln!("aterm-gui: unknown option '{other}' (try --help)");
                std::process::exit(2);
            }
        }
    }
    Cli {
        exec_command: None,
        cwd,
        hold,
        headless,
    }
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

    #[test]
    fn help_advertises_diagnostic_flags() {
        // Every user-facing diagnostic flag must be discoverable in --help.
        for flag in DIAGNOSTIC_VERBS {
            assert!(
                HELP_HEAD.contains(flag),
                "{flag} must be advertised in the help text"
            );
        }
    }

    /// `--headless` and `$ATERM_HEADLESS` are ONE mechanism with two spellings.
    /// The flag wins outright (it overwrites the variable on the way in), and a
    /// bare enabling value arms the same mode by the same funnel.
    #[test]
    fn headless_flag_and_env_are_the_same_mechanism() {
        use super::{HeadlessArming as A, HeadlessSource as S, headless_arming};
        // The flag arms it, whatever the environment says — including the
        // disabling values, which it overwrote.
        for env in [None, Some("1"), Some("0"), Some("off"), Some("")] {
            assert_eq!(headless_arming(true, env), A::Armed(S::Flag), "{env:?}");
        }
        // The variable alone arms exactly the same mode.
        for env in ["1", "yes", "true", "headless"] {
            assert_eq!(headless_arming(false, Some(env)), A::Armed(S::Env), "{env}");
        }
    }

    /// The failure mode must never be silent. A variable set to a DISABLING
    /// value is a refusal the binary reports, not a windowed launch nobody
    /// mentions — that combination is what costs a harness its whole run.
    #[test]
    fn headless_env_refusal_is_reported_not_silent() {
        use super::{HeadlessArming as A, headless_arming};
        for env in ["0", "off", "OFF", "Off", ""] {
            assert_eq!(
                headless_arming(false, Some(env)),
                A::Refused(env.to_string()),
                "ATERM_HEADLESS={env} must be REFUSED (and thus announced), not \
                 silently windowed"
            );
        }
        // An unset variable is the ordinary interactive launch: windowed, and
        // nothing to announce (nobody asked for headless).
        assert_eq!(headless_arming(false, None), A::Windowed);
    }

    /// `--headless` must reach `main` by BOTH channels: the env var (the shared
    /// flag > env > config > default funnel every knob uses) and the `Cli` field
    /// that names the source for the startup announcement.
    #[test]
    fn headless_flag_sets_the_env_var_and_records_its_source() {
        // No `env::scoped_*` wrapper here: `parse_cli` writes through the same
        // module, and that lock is not reentrant (documented on `scoped`). The
        // variable is instead restored by hand at the end of the test.
        let cli = super::parse_cli(vec![std::ffi::OsString::from("--headless")]);
        assert!(cli.headless, "the flag must record itself as the source");
        assert_eq!(
            aterm_log::env::read("ATERM_HEADLESS").as_deref(),
            Some(std::ffi::OsStr::new("1")),
            "the flag must also set the env var the single read site consumes"
        );
        // And the two channels agree on the outcome.
        assert_eq!(
            super::headless_arming(cli.headless, Some("1")),
            super::HeadlessArming::Armed(super::HeadlessSource::Flag)
        );
        aterm_log::env::unset("ATERM_HEADLESS");
    }

    #[test]
    fn help_documents_headless_as_flag_and_env_equivalents() {
        assert!(
            HELP_HEAD.contains("--headless"),
            "--headless must be advertised in the help text"
        );
        assert!(
            HELP_HEAD.contains("[env: ATERM_HEADLESS]"),
            "--help must name the env var as the flag's equivalent"
        );
        assert!(
            super::HELP_TAIL.contains("ATERM_HEADLESS=1"),
            "the ENVIRONMENT section must document ATERM_HEADLESS"
        );
        assert!(
            super::HELP_TAIL.contains("0/off/empty do NOT arm it"),
            "--help must document the values that do NOT arm headless mode"
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
                super::valid_initial_dimension_flag(accepted, 20, 500),
                "columns {accepted}"
            );
        }
        for rejected in ["0", "1", "19", "501", "65536", "nope"] {
            assert!(
                !super::valid_initial_dimension_flag(rejected, 20, 500),
                "columns {rejected}"
            );
        }
        for accepted in ["5", "24", "300"] {
            assert!(
                super::valid_initial_dimension_flag(accepted, 5, 300),
                "lines {accepted}"
            );
        }
        for rejected in ["0", "1", "4", "301", "65536", "nope"] {
            assert!(
                !super::valid_initial_dimension_flag(rejected, 5, 300),
                "lines {rejected}"
            );
        }
    }

    #[test]
    fn gpu_and_cpu_flag_arms_are_symmetric_last_writer_wins() {
        let source = include_str!("cli.rs");
        let gpu = source
            .split_once("\"--gpu\" => {")
            .and_then(|(_, tail)| tail.split_once("\"--cpu\" =>"))
            .map(|(arm, _)| arm)
            .expect("GPU flag arm");
        assert!(gpu.contains("env::unset(\"ATERM_CPU\")"));
        let cpu = source
            .split_once("\"--cpu\" => {")
            .and_then(|(_, tail)| tail.split_once("\"--containment\" =>"))
            .map(|(arm, _)| arm)
            .expect("CPU flag arm");
        assert!(cpu.contains("env::unset(\"ATERM_GPU\")"));
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

    #[test]
    fn help_documents_ai_hint_and_env_stripping() {
        // FINDING #7: the opt-in AI-discoverability banner and the child-shell env
        // sanitization must be discoverable from --help, not only the README.
        assert!(
            super::HELP_TAIL.contains("ATERM_AI_HINT"),
            "the opt-in AI hint must be documented in --help"
        );
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
        let paste = line_for("confirm_multiline_paste");
        assert!(paste.contains("macOS/Windows"), "{paste}");
        assert!(paste.contains("no prompt elsewhere"), "{paste}");
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
        // aborts the WHOLE `toml::from_str::<Config>` at load, so `load_config` falls
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
                    toml::from_str::<Config>(rest).is_ok(),
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
        let parsed: Config = toml::from_str(&block).unwrap_or_else(|e| {
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
