// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm`: a transparent, introspecting terminal.
//!
//! Spawns a shell in a PTY and passes I/O through unchanged without a VT parser,
//! grid, scrollback, or control socket (`aterm --window` or `--headless` runs the
//! introspectable engine).
//!
//! Containment mode is launcher-owned (`--containment`):
//! `User` (default) applies no OS sandbox and inherits the launching shell's
//! `rlimit`s (no Job Object caps on Windows); `Safety` applies soft caps;
//! `Containment` (`--sandbox`) applies hard caps and the macOS Seatbelt sandbox,
//! refusing where no OS sandbox exists. Malformed values fail closed to `Containment`.
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]
use aterm_containment::consent::{
    DEBUGGER_ACCESS_REVERT, DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel,
    DrClass, FdaState, ProbeGate, ProbeLabel, SelfHardening,
};

#[cfg(unix)]
#[path = "driver_unix.rs"]
mod driver;
#[cfg(windows)]
#[path = "driver_windows.rs"]
mod driver;

/// `aterm doctor`'s own rows as converge items; `crates/aterm` composes them with the
/// packages' repair items and `aterm pkg doctor`.
pub mod doctor;
mod manual;
mod spotlight;

pub mod amacs_launch;

mod cli_text;

/// The macOS fleet settings profile, also used by the first interactive launch.
#[cfg(target_os = "macos")]
pub mod macos_doctor;

// `aterm theme`: the theme listing (`list`, also `aterm list-themes`), theme
// authoring (`check`, `new`, `export`, `schema`) and another terminal's theme
// into the themes folder (`import`).
mod theme_author;
mod theme_cmd;
mod theme_pack_cmd;
pub use theme_cmd::main_entry as theme_main_entry;
pub use theme_cmd::{ThemeHost, ThemeListFormat, ThemeListRequest, list_themes_alias};

mod windowing;

pub use windowing::{
    LaunchEnv, LaunchIntent, SplitOrientation, WindowRequest, WindowRoute, WindowingBehavior,
    parse_window_request, plain_launch_dir_operand, plain_launch_is_policy_eligible, route_launch,
    window_verb_usage,
};

/// `--help` text: synopsis, description, OPTIONS, ENVIRONMENT, EXAMPLES, scoped
/// to the transparent passthrough.
const HELP_TITLE: &str = "aterm — a transparent, introspecting terminal\n";
/// What a plain `aterm` session runs, per platform, shared by `--help`
/// ([`HELP_SESSION_LINE`]) and the `aterm help aterm` page. Unix: `$SHELL`.
/// Windows reads no `$SHELL` and no aterm.toml; it spawns `aterm-pty`'s
/// platform default (`shell_override` `None` in [`session_main`]). A macro
/// because both sites are `concat!`-built.
#[cfg(not(windows))]
macro_rules! session_shell {
    () => {
        "your $SHELL in a PTY"
    };
}
/// See the Unix twin above.
#[cfg(windows)]
macro_rules! session_shell {
    () => {
        "pwsh (else Windows PowerShell, else %COMSPEC%) in a ConPTY"
    };
}
pub(crate) use session_shell;
/// The `--help` line under the origin line, built on [`session_shell!`].
#[cfg(not(windows))]
const HELP_SESSION_LINE: &str = concat!(
    "Runs ",
    session_shell!(),
    " and passes its bytes through unchanged.\n"
);
/// See the Unix twin above; only the wrap differs.
#[cfg(windows)]
const HELP_SESSION_LINE: &str = concat!(
    "Runs ",
    session_shell!(),
    " and passes its\nbytes through unchanged.\n"
);
const HELP_HEAD: &str = concat!(
    "\n",
    "`aterm ctl` reads and drives the window (`aterm --window`; `--headless` runs\n",
    "it with no window); a plain `aterm` shell session serves no control socket.\n",
    "\n",
    "USAGE:\n",
    "    aterm [OPTIONS]           Start an interactive shell session (the default\n",
    "                              with a TTY; a no-TTY launch opens the window).\n",
    "    aterm --window [args]     Open the GPU window explicitly; --session forces\n",
    "                              the shell session (e.g. for piped/CI runs).\n",
    "    aterm help [topic]        The manual — what aterm is and how to drive it, plus every\n",
    "                              tool (trust/clean/ty/ay/ny/nn). In an aterm window's tab it\n",
    "                              prints the agent operating brief. START HERE.\n",
    "    aterm <verb> [args]       A platform verb (see VERBS) or a toolchain tool (see TOOLCHAIN).\n",
    "    aterm <SUBCOMMAND>        Print diagnostics and exit (see SUBCOMMANDS).\n",
    "\n",
    "OPTIONS:\n",
    "        --containment <MODE>  user (the default: no sandbox, no extra limits),\n",
    "        --containment=<MODE>  safety (capped limits, no OS sandbox) or\n",
    "                              containment (= --sandbox). The last\n",
    "                              flag wins; an invalid value fails closed to\n",
    "                              containment.\n",
    "        --sandbox             Shorthand for --containment containment (the macOS\n",
    "                              sandbox: no network; no debugger attach; writes\n",
    "                              only to temp dirs and shell history; no access to\n",
    "                              credential stores or private data; no mach\n",
    "                              service lookup but user and group queries, so\n",
    "                              open, osascript, pbcopy and the keychain do\n",
    "                              nothing; refused where no OS sandbox exists).\n",
    "        --no-sandbox          Shorthand for --containment user (no OS sandbox;\n",
    "                              full network/credential access — the default).\n",
    "        --no-reroute          Restore the upstream Rust names (cargo, rustc, …) in\n",
    "                              this session; see `aterm help reroute`.\n",
    "    -q, --quiet               Hide the session's startup line.\n",
    "        --verbose             Print a one-line session summary (bytes passed\n",
    "                              through) to stderr on exit.\n",
    "    -h, --help                Print this help and exit.\n",
    "    -V, --version             Print the version and exit.\n",
    "\n",
    "SUBCOMMANDS (print info and exit; no shell is spawned):\n",
    "    show-config               Print this session's shell, TERM, size and\n",
    "                              containment default.\n",
    "    explain-config            Explain how aterm resolves its configuration.\n",
    "    doctor                    Check health, instances, the fabric broker and the\n",
    "                              packages, and repair what aterm lays; --read-only\n",
    "                              changes nothing. --check / --apply / --restore: the\n",
    "                              macOS fleet settings profile alone.\n",
    "    list-fonts                List available font families.\n",
    "    show-face <family>        Show a font family's file and cell metrics.\n",
    "    list-themes               List the built-in and custom themes (the same as\n",
    "                              `aterm theme list`).\n",
    "    list-kitty-commands       List the words the cursor cat obeys, by language.\n",
    "    exceptions [--all]        What needs a look but not now, and what was filed.\n",
    "\n",
);

/// Windows update instructions shared by `aterm update status|check` and `aterm help update`.
#[cfg(win_scripts)]
pub const WINDOWS_UPDATE_LANE: &str =
    "git pull, then apps\\aterm-win\\build.ps1 and apps\\aterm-win\\install.ps1";
/// Windows update instructions in a checkout without `apps/`.
#[cfg(not(win_scripts))]
pub const WINDOWS_UPDATE_LANE: &str = "git pull, then targo --unverified build --locked -p aterm \
     (this needs the Trust toolchain for Windows, which is not published yet)";

/// The front-door verb roster.
///
/// Ties each verb's name, `--help` entry, exhaustive dispatch in `crates/aterm/src/main.rs`,
/// and [`is_tool_candidate`] shielding together.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Verb {
    /// `aterm ctl` — the introspection / control client.
    Ctl,
    /// `aterm conn` — session connections (the pull/push wiring front door).
    Conn,
    /// `aterm resource-run` — explicitly attribute one external job to a tab.
    ResourceRun,
    /// `aterm pkg` — the toolchain package manager.
    Pkg,
    /// `aterm agh` — the GitHub client: reads as the routed identity, JSON out, typed exits.
    Agh,
    /// `aterm fleet` — fleet membership (`init`, `join`, `peers`, `allow`, `leave`).
    Fleet,
    /// `aterm drive` — the window harness's readers (`classify`, `phase`, `answer`, `ledger`, `tape`, `replay`).
    Drive,
    /// `aterm link` — the fabric bridge (`serve`, `ls`, `notify`, `broker`, `mint`, `fabric`).
    Link,
    /// `aterm fabric` — fabric state, live traffic, and `on|off|doctor`.
    Fabric,
    /// `aterm ship` — the release tool (publishing; source checkouts only).
    Ship,
    /// `aterm update` — the headless update lane (status/check, no window).
    Update,
    /// `aterm agents` — the coding-agent primer installer.
    Agents,
    /// `aterm ai` — AI CLI login management, doctor, and cross-AI reviews.
    Ai,
    /// `aterm machines` — macOS lab configuration over SSH.
    Machines,
    /// `aterm init` — configure the current macOS login and optional GitHub bootstrap.
    Init,
    /// `aterm secrets` — the credentials vault, its keyless server and `~/.secrets`
    /// (docs/DESIGN-secrets-2026-10-20.md). Unix only: on Windows the verb refuses by name.
    Secrets,
    /// `aterm harness` — Claude Code harness read views, accounts roster, and live upgrade.
    Harness,
    /// `aterm keeper` — the PTY keeper (`start | stop | status | serve`).
    Keeper,
    /// `aterm caffeine` — the macOS menu-bar keep-awake helper.
    Caffeine,
    /// `aterm theme` — list, validate, create, export, and import themes.
    Theme,
    /// `aterm edit` — amacs editor (also the `amacs` argv0 alias).
    Edit,
    /// `aterm new-tab` — open a tab routed by `windowing_behavior`.
    NewTab,
    /// `aterm new-window` — open a window unconditionally.
    NewWindow,
    /// `aterm split-pane` — open a pane beside the focused one.
    SplitPane,
}

impl Verb {
    /// Every verb, in the order `--help` lists them.
    pub const ALL: &'static [Verb] = &[
        Verb::Ctl,
        Verb::Conn,
        Verb::ResourceRun,
        Verb::Pkg,
        Verb::Agh,
        Verb::Fleet,
        Verb::Drive,
        Verb::Link,
        Verb::Fabric,
        Verb::Ship,
        Verb::Update,
        Verb::Agents,
        Verb::Ai,
        Verb::Machines,
        Verb::Init,
        Verb::Secrets,
        Verb::Harness,
        Verb::Keeper,
        Verb::Caffeine,
        Verb::Theme,
        Verb::Edit,
        Verb::NewTab,
        Verb::NewWindow,
        Verb::SplitPane,
    ];

    /// The operand that selects this verb.
    pub const fn name(self) -> &'static str {
        match self {
            Verb::Ctl => "ctl",
            Verb::Conn => "conn",
            Verb::ResourceRun => "resource-run",
            Verb::Pkg => "pkg",
            Verb::Agh => "agh",
            Verb::Fleet => "fleet",
            Verb::Drive => "drive",
            Verb::Link => "link",
            Verb::Fabric => "fabric",
            Verb::Ship => "ship",
            Verb::Update => "update",
            Verb::Agents => "agents",
            Verb::Ai => "ai",
            Verb::Machines => "machines",
            Verb::Init => "init",
            Verb::Secrets => "secrets",
            Verb::Harness => "harness",
            Verb::Keeper => "keeper",
            Verb::Caffeine => "caffeine",
            Verb::Theme => "theme",
            Verb::Edit => "edit",
            // Hyphenated, exactly like Windows Terminal's — the whole value of a
            // familiar grammar is that the words are the same words.
            Verb::NewTab => "new-tab",
            Verb::NewWindow => "new-window",
            Verb::SplitPane => "split-pane",
        }
    }

    /// The argv0 compat alias this verb also answers to, if any (shipped as a
    /// symlink onto the one binary). Test-only: the roster the bundler and the
    /// front door's `alias_route` are checked against.
    #[cfg(test)]
    const fn argv0_alias(self) -> Option<&'static str> {
        match self {
            Verb::Ctl => Some("aterm-ctl"),
            // `conn` never was a sibling binary — it is presentation over the
            // ctl wire verbs, shipped only as a front-door word.
            Verb::Conn => None,
            Verb::ResourceRun => None,
            Verb::Pkg => Some("atpkg"),
            // The alias is the verb's own name, which the roster test forbids;
            // AliasRoute::Agh routes it.
            Verb::Agh => None,
            Verb::Fleet => Some("aterm-fleet"),
            Verb::Drive => Some("aterm-drive"),
            Verb::Link => Some("aterm-link"),
            // A report over what `aterm link` and `aterm ctl` reach; only a
            // front-door word (and `aterm-link fabric`).
            Verb::Fabric => None,
            // `$EDITOR` names one executable, run with no room for a verb.
            Verb::Edit => Some("amacs"),
            // Generic names like `new-tab` do not belong on a user's `$PATH`.
            Verb::Ship
            | Verb::Update
            | Verb::Agents
            | Verb::Ai
            | Verb::Machines
            | Verb::Init
            | Verb::Secrets
            | Verb::Harness
            | Verb::Keeper
            | Verb::Caffeine
            | Verb::Theme
            | Verb::NewTab
            | Verb::NewWindow
            | Verb::SplitPane => None,
        }
    }

    /// Whether this verb is one of the windowing verbs that
    /// [`parse_window_request`] parses and [`route_launch`] routes.
    ///
    /// Owned by the roster so alias dispatch and the verb match agree. On
    /// Windows the install is several identical copies of this binary under
    /// sibling names (the Start-Menu shortcut targets `aterm-gui.exe`), so a
    /// windowing verb can arrive under an argv0 alias and must be routed there.
    #[must_use]
    pub const fn is_windowing(self) -> bool {
        match self {
            Verb::NewTab | Verb::NewWindow | Verb::SplitPane => true,
            Verb::Ctl
            | Verb::Conn
            | Verb::ResourceRun
            | Verb::Pkg
            | Verb::Agh
            | Verb::Fleet
            | Verb::Drive
            | Verb::Link
            | Verb::Fabric
            | Verb::Ship
            | Verb::Update
            | Verb::Agents
            | Verb::Ai
            | Verb::Machines
            | Verb::Init
            | Verb::Secrets
            | Verb::Harness
            | Verb::Keeper
            | Verb::Caffeine
            | Verb::Theme
            | Verb::Edit => false,
        }
    }

    /// The `--help` synopsis column, e.g. `aterm ctl <args>`.
    pub const fn usage(self) -> &'static str {
        match self {
            Verb::Ctl => "aterm ctl <args>",
            Verb::Conn => "aterm conn [<cmd>]",
            Verb::ResourceRun => "aterm resource-run <args>",
            Verb::Pkg => "aterm pkg <args>",
            Verb::Agh => "aterm agh <args>",
            Verb::Fleet => "aterm fleet <args>",
            Verb::Drive => "aterm drive <args>",
            Verb::Link => "aterm link <args>",
            Verb::Fabric => "aterm fabric [<cmd>]",
            Verb::Ship => "aterm ship <args>",
            Verb::Update => "aterm update [<cmd>]",
            Verb::Agents => "aterm agents [<cmd>]",
            Verb::Ai => "aterm ai [<cmd>]",
            Verb::Machines => "aterm machines <cmd>",
            Verb::Init => "aterm init [--plan|--check] [--json]",
            Verb::Secrets => "aterm secrets [<cmd>]",
            Verb::Harness => "aterm harness <cmd>",
            Verb::Keeper => "aterm keeper <cmd>",
            Verb::Caffeine => "aterm caffeine [<cmd>]",
            Verb::Theme => "aterm theme <cmd>",
            Verb::Edit => "aterm edit [+L[:C]] <file>",
            // `[-d dir]` rather than `[-d <dir>]`: the synopsis column is 26
            // wide and the blurb is pinned to column 30.
            Verb::NewTab => "aterm new-tab [-d dir]",
            Verb::NewWindow => "aterm new-window [-d dir]",
            Verb::SplitPane => "aterm split-pane [-d dir]",
        }
    }

    /// The `--help` description, one entry per rendered line.
    pub const fn blurb(self) -> &'static [&'static str] {
        match self {
            Verb::Ctl => &[
                "Read and drive an aterm window's sessions: read the screen,",
                "send keys, run a turn, subscribe to events, capture a frame.",
            ],
            Verb::Conn => &[
                "See & wire SESSION CONNECTIONS: which sessions pull or",
                "push each other (ls | add | set | rm | spawn | show | map).",
            ],
            Verb::ResourceRun => &[
                "Run an exact command with authenticated per-tab resource ownership.",
                "--session @self --job label -- program [args]; optional --ai label.",
            ],
            Verb::Pkg => &["Install / update / verify the toolchain (the package manager)."],
            Verb::Agh => {
                &["GitHub reads as the routed identity: JSON out, typed exits (agh help)."]
            }
            Verb::Fleet => &[
                "This machine's membership in a fleet, the aterms of one LAN",
                "that read each other (init | join | peers | allow | leave);",
                "`allow drive --from <node>` lets that node drive this machine",
                "with `aterm ctl --host <h> …` (`aterm help workers`).",
            ],
            Verb::Link => {
                &["The fabric bridge: carry inbox/post between this instance and the bus."]
            }
            Verb::Fabric => &[
                "The fabric: mail between sessions and people (status | tail |",
                "on | off | doctor | mint-for | halt | inbox | post); `status`",
                "is the default.",
            ],
            Verb::Drive => &[
                "The window harness's readers, run by hand (classify | phase |",
                "answer | ledger | tape | replay); workers and reads are",
                "`aterm ctl`'s.",
            ],
            Verb::Ship => &[
                "Publish aterm: provision a signing machine, cut and release",
                "a build. Needs a source checkout.",
            ],
            Verb::Update => &["Report or check for an update (status | check | identity)."],
            Verb::Agents => &[
                "Keep the aterm primer in coding agents' context files (status |",
                "install | remove | primer); `agents_auto_prime = false` stops",
                "the window re-installing it.",
            ],
            Verb::Ai => &[
                "This machine's AI CLIs (claude, codex, agy, muse): one working",
                "login each (doctor | login), and reviews one AI asks of another",
                "(review). `aterm ai` alone is the doctor, read-only.",
            ],
            Verb::Machines => &[
                "Configure and manage a lab of Macs over SSH from this one (init |",
                "scan | enroll | status | plan | apply | person | screen). macOS.",
            ],
            Verb::Init => &[
                "Restore aterm's commands and shell integration (over SSH too), then set",
                "this Mac's keyboard, scrolling, widgets, Dock and accounts (at its console).",
                "Optional init.toml GitHub authentication and clones. Current macOS user.",
            ],
            Verb::Secrets => &[
                "One credentials file for every machine: the vault, its server",
                "and ~/.secrets (status | join | sync | edit | approve | serve …);",
                "`status` is the default.",
            ],
            Verb::Harness => &[
                "Claude Code's spend, limits and accounts, the disk, the ledger",
                "(usage | limits | accounts | disk | ledger); `upgrade` moves a",
                "live Claude Code onto a newer build. The window supervises.",
            ],
            Verb::Keeper => &[
                "The PTY keeper: hold every terminal's master so a crashed",
                "window's shells live on (start | stop | status | serve). macOS, opt-in.",
            ],
            Verb::Caffeine => &[
                "The menu-bar keep-awake mug: a click keeps this Mac awake,",
                "a right click picks 1, 8, 24 h or forever (on | off | status). macOS.",
            ],
            Verb::Theme => &[
                "List every theme (list); write and test your own (new, check,",
                "export, schema); convert another terminal's theme (kitty, Ghostty,",
                "Alacritty, iTerm2, Windows Terminal, base16) into it (import).",
            ],
            Verb::Edit => &[
                "amacs, aterm's editor: open a file in a fresh Editor tab",
                "beside this one and wait until it is closed (`amacs` is",
                "the same command, for $EDITOR).",
            ],
            Verb::NewTab => &[
                "Open a terminal tab. Where it opens is the",
                "`windowing_behavior` config key: a new window",
                "(the default) or a tab in the running aterm.",
            ],
            Verb::NewWindow => &[
                "Open a NEW window, always — never routed into a",
                "running instance whatever windowing_behavior says.",
            ],
            Verb::SplitPane => &[
                "Split the focused pane — a new window by default",
                "(`windowing_behavior`). -V side by side (default),",
                "-H stacked.",
            ],
        }
    }

    /// Resolve an `argv[1]` operand to a verb. The one recognizer: the front door
    /// routes with it and [`is_tool_candidate`] shields with it.
    pub fn from_operand(operand: &str) -> Option<Verb> {
        Verb::ALL.iter().copied().find(|v| v.name() == operand)
    }
}

/// The description column `--help` aligns verb blurbs to.
const VERB_BLURB_COLUMN: usize = 30;

/// Render one verb's `--help` entry: the synopsis, then its blurb lines aligned
/// under a single column.
fn verb_help_block(verb: Verb) -> String {
    let mut out = String::new();
    let mut lines = verb.blurb().iter();
    let first = lines.next().unwrap_or(&"");
    out.push_str(&format!(
        "    {:<width$}{first}\n",
        verb.usage(),
        width = VERB_BLURB_COLUMN - 4
    ));
    for line in lines {
        out.push_str(&format!("{:width$}{line}\n", "", width = VERB_BLURB_COLUMN));
    }
    out
}

/// `--help`, assembled. The VERBS section is rendered from [`Verb::ALL`] rather
/// than written out, which is what makes "advertised" and "exists" the same fact.
fn help_text() -> String {
    // Title, then the maker (`by Andrew Yates · ALab · alab.systems`) — the one
    // origin line every surface prints, from `aterm_types::identity`.
    let mut out = format!(
        "{HELP_TITLE}{}\n\n{HELP_SESSION_LINE}{HELP_HEAD}",
        aterm_types::identity::ORIGIN_LINE
    );
    out.push_str("VERBS (`aterm help <verb>` has each one's page):\n");
    for verb in Verb::ALL {
        out.push_str(&verb_help_block(*verb));
    }
    out.push_str(HELP_TAIL);
    out
}

/// The remainder of `--help`, from TOOLCHAIN on.
const HELP_TAIL: &str = concat!(
    "\n",
    "TOOLCHAIN (`aterm help pkg` has the manual):\n",
    "    aterm <tool> [args]       Run an installed tool from the managed store (never\n",
    "                              $PATH), e.g. `aterm ay`, `aterm ty`, `aterm trustc`;\n",
    "                              `aterm pkg install <program>` adds one.\n",
    "\n",
    "EXAMPLES:\n",
    "    aterm                              Start an interactive shell.\n",
    "    aterm --sandbox                    The same shell under the macOS sandbox.\n",
    "    aterm --containment safety         Capped limits, no OS sandbox.\n",
);

/// Diagnostic subcommands: `aterm <name>` prints introspection about aterm's
/// own configuration/environment and exits without spawning a shell.
///
/// `list-keybinds` is absent because the passthrough binary has no keymap.
/// `list-kitty-commands` is present: it prints the cursor cat's vocabulary,
/// pure data compiled into the binary (aterm-lexicon's `tricks` table), true
/// with no window running.
///
/// Single source of truth: [`diag_report`] handles every entry and
/// [`help_text`] advertises every entry, both enforced by
/// `diag_commands_advertised_and_dispatchable`.
pub const DIAG_COMMANDS: &[(&str, &str)] = &[
    (
        "show-config",
        "Print this session's shell, TERM, size and containment default.",
    ),
    (
        "explain-config",
        "Explain how aterm resolves its configuration.",
    ),
    (
        "doctor",
        "Check the shell, macOS access (and whether macOS checks each new program aterm starts), running instances, the fabric's broker (does it answer, what tier it serves, is its log poisoned), past crashes and the packages (`aterm pkg doctor`'s view), and fix what aterm can (the shell integration, the installed commands and their ~/.local/bin links, the reroute stubs, the rustup link, exec roots and stale copies of aterm to the Trash; the Developer Tools pane opened for you); --read-only fixes nothing; --json prints every row in the one report schema; --only <id,...> narrows to those items, families or layers; exits 0 only when all selected checks are satisfied or not applicable; unresolved plans, manual steps, blocked or deferred work, unknown state and failed changes exit 1; the shell is always checked. On macOS the fleet settings profile is settings-only: --check checks it, --apply backs it up and applies it, --restore restores the backup (--json: its fleet report; drift exits 1). See `aterm help doctor`.",
    ),
    ("list-fonts", "List available font families (one per line)."),
    ("show-face", "Show a font family's file and cell metrics."),
    (
        "list-themes",
        "List the built-in themes and the themes folder's, each with its appearance, pair and status (`aterm theme list`).",
    ),
    (
        "list-kitty-commands",
        "List the words the cursor cat obeys when typed, language by language.",
    ),
    (
        "exceptions",
        "List what needs a look but not now, and what was filed (--all: every filed entry in the newest MiB of the log).",
    ),
];

/// `aterm <diag> --help`'s usage line for a diagnostic subcommand: its operand or
/// flag where it takes one. Pure.
fn diag_usage(cmd: &str) -> String {
    match cmd {
        "show-face" => format!("aterm {cmd} <family>"),
        "doctor" => format!("aterm {cmd} {}", doctor::GRAMMAR),
        "exceptions" => format!("aterm {cmd} [--all]"),
        _ => format!("aterm {cmd}"),
    }
}

/// The refusal for `aterm show-face` with no family (exit 2).
const SHOW_FACE_NEEDS_FAMILY: &str =
    "aterm show-face: needs a font family — usage: aterm show-face <family>";

/// Build the `(report, exit_code)` for diagnostic subcommand `cmd` (with an
/// optional positional `arg`, e.g. the family for `show-face`), or `None` if
/// `cmd` is not a registry command. Never actuates containment or spawns a
/// shell; `doctor` alone acts, and only on what it reports (stale copies of
/// aterm to the Trash, the Developer Tools pane opened for the owner) unless
/// given `--read-only`. A non-zero code (`doctor`/`show-face` on bad input)
/// makes them scriptable.
fn diag_report(cmd: &str, arg: Option<&str>) -> Option<(String, i32)> {
    match cmd {
        "show-config" => Some((show_config_report(), 0)),
        "explain-config" => Some((explain_config_report(), 0)),
        // `doctor` is not answered here: `crates/aterm` composes it (this crate's items,
        // the packages' repair items and `aterm pkg doctor`'s rows) and routes it before
        // this parser runs ([`doctor::args`] is its grammar).
        "list-fonts" => Some((list_fonts_report(), 0)),
        "show-face" => Some(show_face_report(arg)),
        // `list-themes` is not answered here: the listing needs the theme
        // catalog, so the front door routes it to `aterm theme list`
        // ([`list_themes_alias`]) before this parser runs.
        "list-kitty-commands" => Some((list_kitty_commands_report(), 0)),
        "exceptions" => Some((exceptions_report(arg == Some("--all")), 0)),
        _ => None,
    }
}

/// `aterm show-config` — this session's shell, TERM, size and containment
/// default as stable `key=value` lines (one per line, scriptable). Reports the containment DEFAULT a
/// launch with no containment flag takes, NOT an actuated mode — `show-config`
/// never actuates or spawns. `shell=` is `$SHELL` on Unix and, on Windows, the
/// shell this tab runs when typed in an aterm tab, else the one a new window's
/// tab spawns (with `shell_origin=` naming the input that chose it).
fn show_config_report() -> String {
    let (rows, cols) = driver::host_winsize();
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let or = |s: String, dflt: &str| if s.is_empty() { dflt.to_string() } else { s };
    let mut out = String::new();
    out.push_str(&format!(
        "version={}\n",
        aterm_types::version::app_version()
    ));
    // Shell: `$SHELL` on Unix. On Windows the spawn reads neither `$SHELL` nor
    // `%COMSPEC%`, so the row is this tab's shell when there is one
    // (`shell_origin=tab`), else the one a new window's tab spawns, from the
    // spawn's own resolver; `shell_origin=` is tab | aterm.toml |
    // default:<arm>.
    #[cfg(not(windows))]
    out.push_str(&format!("shell={}\n", or(env("SHELL"), "(unset)")));
    #[cfg(windows)]
    {
        let window_shell = driver::WindowShell::resolve();
        out.push_str(&format!("shell={}\n", window_shell.program));
        out.push_str(&format!("shell_origin={}\n", window_shell.origin_token()));
    }
    out.push_str(&format!("term={}\n", or(env("TERM"), "(unset)")));
    out.push_str(&format!("rows={rows}\n"));
    out.push_str(&format!("cols={cols}\n"));
    out.push_str("containment_default=user\n");
    out
}

/// `aterm explain-config` — explain how aterm resolves its configuration:
/// where aterm.toml is on this machine and what beats it, then the containment
/// precedence and modes (least → most capability). Read-only.
fn explain_config_report() -> String {
    let mut out = String::new();
    out.push_str("aterm configuration resolution\n\n");
    out.push_str(&config_path_paragraph());
    out.push_str("Containment mode precedence (most-specific wins):\n");
    out.push_str(
        "  1. --containment <mode> / --sandbox / --no-sandbox (CLI flag; the last wins)\n",
    );
    out.push_str("  2. default: user\n");
    out.push_str(
        "  A malformed value fails CLOSED to the most restrictive mode (containment).\n\n",
    );
    out.push_str("Containment modes (least → most capability):\n");
    out.push_str(
        "  containment  No network; no debugger attach; writes only to temp dirs and shell\n\
         \x20              history; no access to credential stores or private data; no\n\
         \x20              mach service lookup but user and group queries, so open,\n\
         \x20              osascript, pbcopy and the keychain do nothing (macOS; refused\n\
         \x20              elsewhere).\n",
    );
    out.push_str("  safety       Capped limits, no OS sandbox.\n");
    out.push_str("  user         No sandbox, no extra limits (the default).\n\n");
    out.push_str("No environment variable selects the containment mode.\n");
    #[cfg(windows)]
    out.push_str(WINDOWS_CONFIG_PARAGRAPH);
    // Both tables are macOS settings that do nothing elsewhere: documented on
    // macOS, named once off it (so a config shared between a Mac and this machine
    // still explains its keys), never 50 lines of System Settings and `defaults`
    // on a Linux box whose `aterm pkg machine` says "not applicable".
    #[cfg(target_os = "macos")]
    out.push_str(PRIVACY_CONFIG_PARAGRAPH);
    #[cfg(target_os = "macos")]
    out.push_str(MACHINE_CONFIG_PARAGRAPH);
    #[cfg(not(target_os = "macos"))]
    out.push_str(
        "\n[privacy] and [machine] are macOS settings; aterm ignores them on this platform.\n",
    );
    out
}

/// The first paragraph of `explain-config`, on every platform: the aterm.toml
/// this machine resolves (`aterm_types::dirs::aterm_config_path`, the file the
/// window loads) and what beats it.
fn config_path_paragraph() -> String {
    let path = match aterm_types::dirs::aterm_config_path() {
        Some(path) => path.display().to_string(),
        #[cfg(windows)]
        None => "(unresolved: XDG_CONFIG_HOME, APPDATA and HOME are all unset)".to_string(),
        #[cfg(not(windows))]
        None => "(unresolved: XDG_CONFIG_HOME and HOME are unset)".to_string(),
    };
    format!(
        "aterm.toml: {path}\n\
         \x20 (need not exist; a command-line flag beats it, it beats the default;\n\
         \x20 `aterm help config` has the keys)\n\n"
    )
}

/// The Windows paragraph of `explain-config`: the starter-file command and
/// the `shell` and `font_px` keys.
#[cfg(windows)]
const WINDOWS_CONFIG_PARAGRAPH: &str = "\n\
     \x20 `aterm --window --write-config` writes a documented starter.\n\
     \x20 shell                   the interactive shell a new tab spawns: a bare name — \"pwsh\",\n\
     \x20                         \"cmd\", \"wsl\", \"nu\", or \"bash\", which finds Git Bash even off\n\
     \x20                         PATH — or an absolute path used verbatim. A window launched\n\
     \x20                         with --shell uses that instead; with neither, the default is\n\
     \x20                         pwsh, then powershell, then %COMSPEC%, then cmd.exe. `aterm\n\
     \x20                         doctor` names the program this machine resolves it to.\n\
     \x20 font_px                 glyph size in physical pixels (6..=200). A window launched\n\
     \x20                         with --font-px uses that instead; unset, the size follows\n\
     \x20                         the display scale.\n";

/// The `[machine]` paragraph of `explain-config`.
#[cfg(target_os = "macos")]
const MACHINE_CONFIG_PARAGRAPH: &str = "\n\
     [machine] — macOS host settings (aterm.toml; applied by `aterm pkg`):\n\
     \x20 universal_control       \"off\" (default) | \"leave\". Off writes the two per-host keys\n\
     \x20                         that stop the cursor and keyboard roaming to other Macs and\n\
     \x20                         iPads on the same Apple account; leave never touches them.\n\
     \x20 rosetta                 true (default) | false. On Apple silicon, true installs Rosetta\n\
     \x20                         2 when it is missing (`softwareupdate --install-rosetta`, as\n\
     \x20                         you, no admin password); false never does. An Intel Mac\n\
     \x20                         ignores it.\n\
     \x20 It is applied by `aterm pkg machine apply`, which aterm runs once a day at the\n\
     \x20 first window or interactive terminal session, and by a package pass (update,\n\
     \x20 seed, install) when the [machine] table changed since it was last applied; no\n\
     \x20 pass runs while [packages] enabled = false, so an edit then waits for the daily\n\
     \x20 apply. `aterm pkg machine` reads the measured state. Settings ▸ Security sets\n\
     \x20 it, shows that state under its row and, while there is something to apply,\n\
     \x20 offers Apply Now, which applies it at once. `defaults` writes the ACCOUNT's\n\
     \x20 per-host domain regardless of $HOME, so an apply under a redirected HOME is\n\
     \x20 refused and says so, as is one whose aterm.toml does not parse (fix it with\n\
     \x20 `aterm --window --validate-config`). Undo Universal Control with the revert\n\
     \x20 line `aterm pkg machine` prints (`defaults -currentHost delete\n\
     \x20 com.apple.universalcontrol Disable`, then the same for DisableMagicEdges) AND\n\
     \x20 set universal_control = \"leave\", or the next apply disables it again.\n";

/// The `[privacy]` paragraph of `explain-config`.
#[cfg(target_os = "macos")]
const PRIVACY_CONFIG_PARAGRAPH: &str = "\n\
     [privacy] — macOS consent (aterm.toml; the window reads it, `aterm doctor` only `observer`):\n\
     \x20 enabled / check         the silent, prompt-free probes: Full Disk Access (opens your\n\
     \x20                         TCC.db unread, or on macOS 27, which keeps none per user, the\n\
     \x20                         system one), the debugger rule (`DevToolsSecurity -status`) and\n\
     \x20                         the Developer Tools list (tccd's log, and its switch in the\n\
     \x20                         system TCC.db, read-only). They read state that already\n\
     \x20                         exists and raise NO dialog. Off, every field reads `unknown` —\n\
     \x20                         which is not `denied`, and the report says which it is.\n\
     \x20 notice                  the Full Disk Access ask, in two places. At launch, while the\n\
     \x20                         grant reads off on a Mac where aterm has never seen it on,\n\
     \x20                         System Settings opens at Privacy & Security \u{25b8} Full Disk\n\
     \x20                         Access at the first quiet moment (a focused aterm window, your\n\
     \x20                         hands off for 10 seconds), with aterm's panel beside it: its\n\
     \x20                         icon to drag into the list, I've switched it on, and Not now.\n\
     \x20                         Once per version, never on a build whose grant would not last\n\
     \x20                         (ad hoc or unsigned), never within 10 minutes of another\n\
     \x20                         Settings page; Not now holds 30 days, then 90. aterm presses\n\
     \x20                         nothing there: switching it on is yours. Later, after macOS shows\n\
     \x20                         the \"access data from other apps\" dialog for a program of yours\n\
     \x20                         (or refuses aterm a read), it files one entry in aterm's\n\
     \x20                         Messages with an Open Settings button. At most once a week; a\n\
     \x20                         window started before you pressed Open Settings waits 14 days;\n\
     \x20                         Dismiss all files it for 30 days, then 90. Off, it also keeps\n\
     \x20                         aterm from asking, once per Mac after those pages, whether to\n\
     \x20                         allow debugger access (macOS's developer mode) in its own\n\
     \x20                         alert; no key makes aterm ask it.\n\
     \x20 report_attribution      per-session corroboration of the app macOS holds responsible for\n\
     \x20                         each session. It only corroborates: aterm's own adoption record\n\
     \x20                         decides whether a session reads `live` or `adopted`.\n\
     \x20 observer                absent: a narrow sentinel watches for the one log line macOS\n\
     \x20                         writes when it shows a consent dialog: it decides when to ask,\n\
     \x20                         and any other dialog it can name is a record in Messages saying\n\
     \x20                         which tab asked (with notice on). false: off. true: the full\n\
     \x20                         stream, the tab mark and a banner per dialog. enabled = false\n\
     \x20                         turns every mode off, true included. Config only; nothing in a\n\
     \x20                         session can set it.\n\
     \x20                         false also stops the Developer Tools row's reads (tccd's log\n\
     \x20                         and the switch), the window's and both doctors' alike.\n\
     \x20 warmup                  \"automatic\" (default) | \"on-request\" | \"never\". The warm-up\n\
     \x20                         asks macOS for the folders up front, which RAISES the dialogs\n\
     \x20                         on purpose, while you are at the screen rather than inside an\n\
     \x20                         agent's blocked read. automatic: the button in Settings, and\n\
     \x20                         at launch while Full Disk Access reads off, once the Full Disk\n\
     \x20                         Access page was left without you acting on it (Not now, or its\n\
     \x20                         panel lapsed) or was not offered, at the first quiet moment.\n\
     \x20                         Acting on that page (switching it on, dropping the icon) asks\n\
     \x20                         for no folder that launch; once every folder has an answer,\n\
     \x20                         this app is not walked at launch again. on-request: only the\n\
     \x20                         button. never: neither. No program inside a session can start\n\
     \x20                         it.\n\
     \x20 warmup_folders          what the warm-up asks for. Default: Documents, Desktop and\n\
     \x20                         Downloads. `app-data` (every other app's own data) is a valid\n\
     \x20                         name but not asked for by default, and never at launch: macOS\n\
     \x20                         forgets that answer when the process that received it is\n\
     \x20                         replaced. Network and removable volumes are not on the list:\n\
     \x20                         the warm-up has no name for them, and whether Full Disk\n\
     \x20                         Access reaches them is unmeasured (`aterm ctl privacy`).\n\
     \x20 warmup_hold_ms          how long a pending update waits for a running warm-up\n\
     \x20                         (default 120000, at most 600000).\n\
     \x20 probe_interval_ms       the shortest gap between access re-checks (default 5000);\n\
     \x20                         `aterm ctl @<sid> await consent` waits for the next completed\n\
     \x20                         check.\n\
     \x20 protected_roots         the sensitive set; empty = the containment tier's own list, so\n\
     \x20                         the two tiers cannot disagree about which paths are sensitive.\n\
     \x20 auto_accept             RESERVED, and not implemented: aterm does not answer macOS\n\
     \x20                         consent dialogs. Granting Full Disk Access in Settings is the\n\
     \x20                         supported answer, and only a human can do it.\n\
     \x20 Settings \u{25b8} Security \u{25b8} Ask Again clears macOS's saved refusals for the folders it\n\
     \x20 lists, after you confirm. No key in this section needs anything beyond saving the file.\n";

/// `aterm list-fonts` — available font families (file stems), one per line, sorted
/// and deduplicated for scriptable output. Data: [`aterm_render::list_fonts`].
fn list_fonts_report() -> String {
    let fonts = aterm_render::list_fonts();
    if fonts.is_empty() {
        return "(no fonts found)\n".to_string();
    }
    let mut out = String::new();
    for f in fonts {
        out.push_str(&f);
        out.push('\n');
    }
    out
}

/// `aterm show-face <family>`: the resolved path + cell metrics for a font
/// family as `key=value` lines; `px=` is the probe size
/// ([`aterm_render::FaceInfo::PROBE_PX`]). A missing `family` is a usage error
/// (exit 2, as `decide_args` refuses it first); an unknown one exits 1.
/// Data: [`aterm_render::face_info`].
fn show_face_report(family: Option<&str>) -> (String, i32) {
    let Some(family) = family else {
        return (format!("{SHOW_FACE_NEEDS_FAMILY}\n"), 2);
    };
    match aterm_render::face_info(family) {
        Some(info) => (
            format!(
                "family={family}\npath={}\nindex={}\npx={}\ncell_width={}\ncell_height={}\nbaseline={}\nglyph_count={}\n",
                info.path,
                info.index,
                aterm_render::FaceInfo::PROBE_PX,
                info.cell_width,
                info.cell_height,
                info.baseline,
                info.glyph_count
            ),
            0,
        ),
        None => (
            format!(
                "aterm show-face: no loadable font family {family:?} (`aterm list-fonts` lists them)\n"
            ),
            1,
        ),
    }
}

/// How much of `messages.log`'s tail the reader takes: the window's own launch load
/// (`aterm-gui`'s `messages_store::TAIL_BYTES`), across the file and its rotated
/// `.1`, so the reader sees what the window would replay. About 150 days at the
/// measured 7 KB a day.
const EXCEPTIONS_TAIL_BYTES: u64 = 1024 * 1024;

/// What the reader found in `messages.log`.
struct ExceptionsRead {
    /// The replayed records.
    log: aterm_messages::MessageLog,
    /// Lines the codec refused (torn, foreign, corrupt).
    skipped: usize,
    /// Whether any of the two files was there to read.
    found: bool,
    /// `messages.log` is there but was not read, or nothing was read and only its
    /// rotated `.1` is there: that path.
    unreadable: Option<Unreadable>,
}

/// A log path that exists but was not read.
struct Unreadable {
    path: std::path::PathBuf,
    /// A symlink or a folder (never followed), rather than a file that would not open.
    not_a_file: bool,
}

impl Unreadable {
    /// What the report says about it.
    fn why(&self) -> String {
        let shown = self.path.display();
        if self.not_a_file {
            format!("{shown} is a symlink or folder, not the log")
        } else {
            format!("{shown} cannot be opened (no read permission)")
        }
    }

    /// The file's own name, for `doctor`'s one-line reason.
    fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }
}

/// `<logs>/messages.log`; `None` when no log folder resolves.
fn exceptions_log_path() -> Option<std::path::PathBuf> {
    aterm_types::dirs::logs_dir().map(|dir| dir.join("messages.log"))
}

/// The last `tail` bytes of one regular file as text (never followed through a
/// symlink), with the partial first line of a cut dropped. `None` when it is not
/// there, not a regular file, or unreadable: a log is never worth a refusal.
fn read_log_tail(path: &std::path::Path, tail: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let len = meta.len();
    let mut file = std::fs::File::open(path).ok()?;
    let cut = len > tail;
    if cut {
        file.seek(SeekFrom::Start(len - tail)).ok()?;
    }
    let mut bytes = Vec::new();
    file.take(tail).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some(if cut {
        // A cut landed inside a line: what is left of it is not a record.
        text.split_once('\n')
            .map_or_else(String::new, |(_, rest)| rest.to_string())
    } else {
        text
    })
}

/// Read and replay `path` and its rotated `.1` (oldest first, so a `posted` in the
/// older copy meets its `retired` in the current file), exactly as the window's
/// launch load does. Read-only and bounded.
fn read_exceptions_log(path: &std::path::Path) -> ExceptionsRead {
    let rotated = {
        let mut name = path.as_os_str().to_owned();
        name.push(".1");
        std::path::PathBuf::from(name)
    };
    let current_len = std::fs::symlink_metadata(path)
        .ok()
        .filter(|m| m.file_type().is_file())
        .map_or(0, |m| m.len());
    let older_budget = EXCEPTIONS_TAIL_BYTES.saturating_sub(current_len);
    let mut found = false;
    let mut lines = Vec::new();
    let mut skipped = 0;
    let mut take = |text: Option<String>| {
        let Some(text) = text else { return };
        found = true;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            match aterm_messages::LogLine::decode(line) {
                Ok(decoded) => lines.push(decoded),
                Err(_) => skipped += 1,
            }
        }
    };
    if older_budget > 0 {
        take(read_log_tail(&rotated, older_budget));
    }
    let current = read_log_tail(path, EXCEPTIONS_TAIL_BYTES);
    let unreadable_at = |p: &std::path::Path| {
        std::fs::symlink_metadata(p).ok().map(|m| Unreadable {
            path: p.to_path_buf(),
            not_a_file: !m.file_type().is_file(),
        })
    };
    // The current file is judged on its own: one that is there but was not read
    // may hold the retirements of what the rotated copy posted.
    let current_unreadable = if current.is_none() {
        unreadable_at(path)
    } else {
        None
    };
    take(current);
    lines.sort_by_key(aterm_messages::LogLine::id);
    let mut log = aterm_messages::MessageLog::empty();
    log.replay_all(lines);
    let unreadable =
        current_unreadable.or_else(|| if found { None } else { unreadable_at(&rotated) });
    ExceptionsRead {
        log,
        skipped,
        found,
        unreadable,
    }
}

/// The local zone's offset from UTC in seconds at `unix_s` ([`local_offset_at`]),
/// `0` where `date` cannot say (Windows, or no `date`): the times then read as UTC.
fn local_offset_s(unix_s: i64) -> i64 {
    local_offset_at(unix_s).unwrap_or(0)
}

/// The local zone's offset from UTC in seconds at `unix_s`, from `date +%z` (the
/// offset daylight saving gave THAT instant), or `None` where `date` cannot say.
/// One subprocess, in the print-and-exit path only.
#[cfg(unix)]
fn local_offset_at(unix_s: i64) -> Option<i64> {
    // The system's own `date` by its path, never whichever one a PATH names.
    let mut date = aterm_uds::spawnfd::command("/bin/date");
    if cfg!(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd"
    )) {
        date.arg("-r").arg(unix_s.to_string());
    } else {
        date.arg("-d").arg(format!("@{unix_s}"));
    }
    date.arg("+%z")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_zone_offset(String::from_utf8_lossy(&o.stdout).trim()))
}

#[cfg(not(unix))]
fn local_offset_at(_unix_s: i64) -> Option<i64> {
    None
}

/// `+0100` / `-0530` (what `date +%z` prints) as seconds east of UTC.
#[cfg(any(unix, test))]
fn parse_zone_offset(z: &str) -> Option<i64> {
    let (sign, digits) = match z.as_bytes().first()? {
        b'+' => (1, &z[1..]),
        b'-' => (-1, &z[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (h, m): (i64, i64) = (digits[..2].parse().ok()?, digits[2..].parse().ok()?);
    (h < 24 && m < 60).then_some(sign * (h * 3600 + m * 60))
}

/// `aterm exceptions [--all]` — what needs a look but not now, and what was filed,
/// read from `messages.log` with NO window running. The open entries are what was
/// waiting when aterm last wrote the log; the filed ones say when and how each was
/// put away. `--all` lists every filed entry in the tail instead of the newest ten.
fn exceptions_report(all: bool) -> String {
    let Some(path) = exceptions_log_path() else {
        return "exceptions: none to read (no log folder)\n".to_string();
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let offset = local_offset_s(i64::try_from(now_ms / 1000).unwrap_or(i64::MAX));
    exceptions_report_at(&path, all, now_ms, offset)
}

/// [`exceptions_report`] over one log path, a clock and a zone: the testable core.
/// A missing file is an empty inbox, said so; one that is there but cannot be read
/// is said as that; junk lines are counted, never fatal.
fn exceptions_report_at(path: &std::path::Path, all: bool, now_ms: u64, offset_s: i64) -> String {
    let read = read_exceptions_log(path);
    if let Some(unreadable) = &read.unreadable {
        return format!("exceptions: {}\n", unreadable.why());
    }
    if !read.found {
        return format!("exceptions: none (no log yet at {})\n", path.display());
    }
    let scope = if all {
        aterm_messages::ReportScope::All
    } else {
        aterm_messages::ReportScope::Latest
    };
    let lines = aterm_messages::exceptions_report(&read.log, scope, offset_s, now_ms);
    let mut out = format!("exceptions in {}\n", path.display());
    for line in &lines {
        out.push_str(line);
        out.push('\n');
    }
    if !all
        && lines
            .iter()
            .any(|l| l.starts_with("Filed (") && l.contains("shown)"))
    {
        out.push_str("(aterm exceptions --all lists every filed entry)\n");
    }
    if read.skipped > 0 {
        out.push_str(&format!(
            "({} line(s) of the log could not be read)\n",
            read.skipped
        ));
    }
    out
}

/// The `exceptions:` fact `doctor` reads: how many entries the log leaves open, or
/// why nothing was counted (no log folder, or a log that cannot be read).
type ExceptionsFacts = Result<usize, String>;

/// How many exceptions are open in `messages.log` (0 when there is no file yet).
fn exceptions_facts() -> ExceptionsFacts {
    let path = exceptions_log_path().ok_or_else(|| "no log folder".to_string())?;
    let read = read_exceptions_log(&path);
    if let Some(unreadable) = &read.unreadable {
        return Err(format!("{} cannot be read", unreadable.name()));
    }
    Ok(read
        .log
        .records()
        .filter(|r| r.later && r.is_live())
        .count())
}

/// The `exceptions:` row, pure over the count. A NOTE when something waits for a look
/// (a fact for a person, never a reason for `doctor` to fail), OK when nothing does.
fn exceptions_row(facts: &ExceptionsFacts) -> (Mark, String) {
    match facts {
        Err(why) => (Mark::Note, format!("not measured ({why})")),
        Ok(0) => (Mark::Ok, "none open".to_string()),
        Ok(n) => (Mark::Note, format!("{n} open (aterm exceptions)")),
    }
}

/// `aterm list-kitty-commands` — every word the cursor cat obeys when it is TYPED
/// at the terminal, language by language: its trick rows, then its names for the
/// pet, then its filler words. Data: [`aterm_lexicon::TrickLexicon::all_rows`]
/// — the embedded vocabulary the window's typed-line listener compiles, every
/// language, gated rows included (`gated=1` marks a row that loads only when
/// `[sparkle_words] languages` lists its language).
///
/// No banner: every line is one [`aterm_lexicon::tricks::row_line`], so
/// `| grep lang=es` filters by language. The prose is on `aterm help kitty`.
pub(crate) fn list_kitty_commands_report() -> String {
    let mut out = String::new();
    for row in aterm_lexicon::TrickLexicon::all_rows() {
        out.push_str(&aterm_lexicon::tricks::row_line(&row));
        out.push('\n');
    }
    out
}

/// A doctor row's presentation. The legacy fact renderer counts warnings and
/// fails only for [`Mark::Fail`]; the composed doctor assesses readiness from
/// typed item outcomes, including whether a note represents missing evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// The check passed.
    Ok,
    /// A fact worth reporting that is not a failure — nothing to fix, or
    /// nothing aterm can fix, and never a reason to refuse to launch.
    Note,
    /// A measured cost the person is paying now, with a fix only they can
    /// make (atpkg's `warn`): said louder than a note and counted on the
    /// health line, and still never a reason to refuse to launch, so it does
    /// not move the exit code either.
    Warn,
    /// The check failed: `health: FAIL` and exit 1.
    Fail,
}

impl Mark {
    /// The mark column's text.
    const fn render(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Note => "note",
            Self::Warn => "warn",
            Self::Fail => "FAIL",
        }
    }

    /// `true` for the one mark that moves the exit code.
    const fn is_fail(self) -> bool {
        matches!(self, Self::Fail)
    }

    /// The mark for the two-valued shell check.
    const fn from_ok(ok: bool) -> Self {
        if ok { Self::Ok } else { Self::Fail }
    }
}

/// One `doctor` row: the label, its verdict, and the detail line (which carries
/// its own `key: …` prefix).
struct DoctorRow {
    /// The row's left column, e.g. `shell:`.
    label: &'static str,
    /// The presentation; readiness is assessed separately.
    mark: Mark,
    /// The fact alone, no label (`/bin/sh (executable)`): the row prints it after
    /// the mark.
    fact: String,
    /// The read did not establish the state. Separate from `mark`: a quiet note
    /// may be either an ordinary advisory or missing evidence.
    unmeasured: bool,
}

/// The macOS consent facts behind the `privacy:` row, gathered by
/// [`privacy_facts`] and consumed by the pure [`doctor_checks_with_origin`] — passed
/// in exactly as the shell check is, so the report stays a
/// deterministic function of its inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrivacyFacts {
    /// The instance's Full Disk Access state, from one prompt-free probe.
    fda: FdaState,
    /// Why `fda` reads as it does. `label.refused()` means NO syscall ran, so
    /// the row names the configuration rather than implying a denial.
    fda_label: ProbeLabel,
    /// The class of this build's designated requirement — whether a grant made
    /// today survives the next build. [`DrClass::Unknown`] asserts nothing.
    dr: DrClass,
    /// The display name of the process macOS holds responsible for this one;
    /// the verdict belongs to it (under another terminal, that terminal's).
    responsible: Option<String>,
}

impl PrivacyFacts {
    /// What every host that measured nothing reports: no state, no claim.
    fn not_measured(label: ProbeLabel) -> Self {
        Self {
            fda: FdaState::Unknown,
            fda_label: label,
            dr: DrClass::Unknown,
            responsible: None,
        }
    }
}

/// Gather the consent facts for the `privacy:` row.
///
/// [`aterm_containment::probe_fda`] opens `TCC.db` only when the running
/// executable resolves inside a `.app`; outside a bundle it returns the
/// refused arm without running a probe, responsible-app lookup, or `codesign`.
fn privacy_facts() -> PrivacyFacts {
    let probe = aterm_containment::probe_fda(ProbeGate::on());
    if probe.label.refused() {
        return PrivacyFacts::not_measured(probe.label);
    }
    let responsible = i32::try_from(std::process::id())
        .ok()
        .and_then(aterm_containment::responsible_app)
        .map(|app| app.display_name);
    PrivacyFacts {
        fda: probe.state,
        fda_label: probe.label,
        dr: designated_requirement_class(),
        responsible,
    }
}

/// Classify the running bundle's designated requirement via `codesign -d -r-`.
///
/// Spawned only inside a resolved `.app`; any failure or timeout returns
/// [`DrClass::Unknown`].
#[cfg(target_os = "macos")]
fn designated_requirement_class() -> DrClass {
    let Some(exe) = aterm_containment::consent::fence_exe() else {
        return DrClass::Unknown;
    };
    let Some(app_root) = aterm_containment::consent::app_bundle_root(&exe) else {
        return DrClass::Unknown;
    };
    // Bounded to 10 s: when macOS's trustd does not answer, `codesign` hangs indefinitely.
    let Ok(out) = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/codesign")
            .args(["-d", "-r-"])
            .arg(&app_root),
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    ) else {
        return DrClass::Unknown;
    };
    // `codesign` writes the requirement to stderr; read both streams.
    let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stdout));
    aterm_containment::classify_dr(&text)
}

/// Off macOS there is no TCC and no designated requirement to read.
#[cfg(not(target_os = "macos"))]
fn designated_requirement_class() -> DrClass {
    DrClass::Unknown
}

/// Why a probe reports what it does, in an actionable sentence. Every label is
/// spelled so a new variant cannot render as an empty reason.
fn probe_reason(label: ProbeLabel) -> String {
    match label {
        ProbeLabel::Pending => "the access check is pending".to_string(),
        ProbeLabel::OpenOk => "the probe succeeded".to_string(),
        ProbeLabel::OpenEperm => "the probe was refused by macOS".to_string(),
        ProbeLabel::OpenErrno(errno) => format!("the probe could not complete (errno {errno})"),
        ProbeLabel::RefusedOutOfBundle => "running outside the app bundle".to_string(),
        ProbeLabel::RefusedNoExe => "this program's own path could not be resolved".to_string(),
        ProbeLabel::RefusedDisabled => {
            "the privacy probe is switched off in the config".to_string()
        }
        ProbeLabel::RefusedNoHome => "$HOME is not set".to_string(),
        ProbeLabel::RefusedBadPath => "the store path is not usable".to_string(),
        ProbeLabel::UnsupportedPlatform => "macOS only".to_string(),
    }
}

/// The `privacy:` row: mark + detail, pure over [`PrivacyFacts`].
///
/// Never [`Mark::Fail`]: lacking Full Disk Access is normal. The grant is
/// described as access held by the responsible app, not as ending all consent
/// dialogs.
fn privacy_row(facts: &PrivacyFacts) -> (Mark, String) {
    let who = facts.responsible.as_deref().unwrap_or("unknown");
    // Only a positively classified requirement counts as a dev build.
    let churns = matches!(facts.dr, DrClass::Cdhash | DrClass::Unsigned);
    let dev = if churns {
        "; dev build: identity changes on every build, so grants do not persist"
    } else {
        ""
    };
    if facts.fda_label.refused() || facts.fda == FdaState::Unknown {
        return (
            Mark::Note,
            format!("not measured ({})", probe_reason(facts.fda_label)),
        );
    }
    match facts.fda {
        FdaState::Granted if churns => (
            Mark::Note,
            format!("full disk access for the responsible app ({who}){dev}"),
        ),
        FdaState::Granted => (
            Mark::Ok,
            format!("full disk access for the responsible app ({who})"),
        ),
        FdaState::Denied | FdaState::Unknown => (
            Mark::Note,
            format!(
                "no full disk access for the responsible app ({who}); programs run here \
                 can be interrupted by macOS consent dialogs (aterm help permissions){dev}"
            ),
        ),
    }
}

/// Debugger-access facts behind the `debugger:` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DebuggerFacts {
    /// `DevToolsSecurity -status` plus the membership query.
    probe: DebuggerProbe,
    /// Whether this binary has the hardened runtime without `get-task-allow`.
    hardening: SelfHardening,
}

impl DebuggerFacts {
    const fn not_measured(label: DebuggerProbeLabel) -> Self {
        Self {
            probe: DebuggerProbe::refused(label),
            hardening: SelfHardening::Unknown,
        }
    }
}

/// Gather the facts for the `debugger:` row behind the in-bundle fence.
fn debugger_facts() -> DebuggerFacts {
    let probe = aterm_containment::consent::probe_debugger(ProbeGate::on());
    if probe.label.refused() {
        return DebuggerFacts::not_measured(probe.label);
    }
    DebuggerFacts {
        probe,
        hardening: aterm_containment::consent::self_hardening(),
    }
}

const fn debugger_probe_reason(label: DebuggerProbeLabel) -> &'static str {
    match label {
        DebuggerProbeLabel::Pending => "the check is pending",
        DebuggerProbeLabel::StatusOk => "the check succeeded",
        DebuggerProbeLabel::StatusUnrecognized => {
            "macOS answered in words aterm does not recognise"
        }
        DebuggerProbeLabel::ToolAbsent => "this Mac has no DevToolsSecurity",
        DebuggerProbeLabel::SpawnFailed => "the check could not be started",
        DebuggerProbeLabel::TimedOut => "the check did not finish in time",
        DebuggerProbeLabel::OutputUnreadable => "the check's answer could not be read",
        DebuggerProbeLabel::RefusedDisabled => "the privacy probe is switched off in the config",
        DebuggerProbeLabel::RefusedOutOfBundle => "running outside the app bundle",
        DebuggerProbeLabel::RefusedNoExe => "this program's own path could not be resolved",
        DebuggerProbeLabel::UnsupportedPlatform => "macOS only",
    }
}

/// Format the `debugger:` row from [`DebuggerFacts`]. Never returns [`Mark::Fail`].
fn debugger_row(facts: &DebuggerFacts) -> (Mark, String) {
    let probe = &facts.probe;
    if probe.label.refused() || probe.mode == DebuggerMode::Unknown {
        return (
            Mark::Note,
            format!("not measured ({})", debugger_probe_reason(probe.label)),
        );
    }
    const SCOPE: &str = "developer mode on for this Mac";
    match probe.mode {
        DebuggerMode::Enabled => match facts.hardening {
            SelfHardening::Hardened => (
                Mark::Ok,
                format!(
                    "{SCOPE} (Apple-signed debuggers; admin and _developer accounts); \
                     undo: {DEBUGGER_ACCESS_REVERT}"
                ),
            ),
            SelfHardening::NotHardened => (
                Mark::Note,
                format!(
                    "{SCOPE}, and this aterm is not hardened: any program you run can take \
                     control of it; undo: {DEBUGGER_ACCESS_REVERT}"
                ),
            ),
            SelfHardening::Unknown => (
                Mark::Note,
                format!(
                    "{SCOPE} (Apple-signed debuggers; admin and _developer accounts), and \
                     whether this aterm is hardened was not measured; \
                     undo: {DEBUGGER_ACCESS_REVERT}"
                ),
            ),
        },
        DebuggerMode::Disabled => {
            const ASKS: &str = "macOS asks for a password when lldb attaches";
            const CACHED: &str = "(Developer Tools Access, cached up to 10 h)";
            const SWITCH: &str = "to turn it on: aterm Settings ▸ Security";
            let exposure = match facts.hardening {
                SelfHardening::Hardened => "",
                SelfHardening::NotHardened => {
                    "; this aterm is not hardened (a development build), so with developer \
                     mode on any program you run could take control of it"
                }
                SelfHardening::Unknown => "; whether this aterm is hardened was not measured",
            };
            let detail = match probe.member {
                DebuggerMember::Admin | DebuggerMember::Developer => {
                    format!("{ASKS} {CACHED}; {SWITCH}{exposure}")
                }
                DebuggerMember::NotMember => {
                    format!("{ASKS}; developer mode would not change it for this account")
                }
                DebuggerMember::Unknown => format!(
                    "{ASKS} {CACHED}; whether developer mode would change it for this account \
                     was not measured; {SWITCH}{exposure}"
                ),
            };
            (Mark::Note, detail)
        }
        DebuggerMode::Unrecognized => (
            Mark::Note,
            "macOS reports the debugger rule in a shape aterm does not recognise; aterm \
             leaves it alone"
                .to_string(),
        ),
        // Handled above; spelled so the match stays exhaustive.
        DebuggerMode::Unknown => (
            Mark::Note,
            format!("not measured ({})", debugger_probe_reason(probe.label)),
        ),
    }
}

/// The Developer Tools list as `doctor` reads it ([`atpkg::devtools::facts`]:
/// the switch's row in the system TCC.db and tccd's own answers, joined), read
/// AFTER anything this run did about it, and what this run did toward the
/// switch. `facts` is `None` where there is no such list (off macOS): no row.
struct DevtoolsFacts {
    facts: Option<atpkg::devtools::Facts>,
    did: Option<String>,
}

#[cfg(test)]
impl DevtoolsFacts {
    /// No Developer Tools list here: no row.
    const fn absent() -> Self {
        Self {
            facts: None,
            did: None,
        }
    }
}

/// Whether a shell can be started, and if not, which of the two causes
/// `doctor` names (the driver's `shell_check` tells them apart).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellCheck {
    /// A file this user can execute.
    Runs,
    /// Nothing at that path.
    Missing,
    /// Something is there, but it is a directory or lacks execute permission.
    NotExecutable,
}

/// The shell a plain `aterm` on Unix starts, and the label the `shell:` row
/// carries when it is not `$SHELL`, from aterm-pty's own resolution
/// (`aterm_pty::select_shell_with_origin`): `$SHELL`, else — unset or empty — the
/// account's login shell from the password database, else `/bin/sh`. So doctor
/// checks the program the spawn runs, and an unset `$SHELL` is not a failure.
#[cfg(not(windows))]
fn unix_shell(
    (program, origin): (std::ffi::OsString, aterm_pty::ShellOrigin),
) -> (String, Option<&'static str>) {
    let label = match origin {
        aterm_pty::ShellOrigin::Override | aterm_pty::ShellOrigin::Env => None,
        aterm_pty::ShellOrigin::Passwd => Some(
            "$SHELL is unset or empty, so aterm starts your login shell from the password database",
        ),
        aterm_pty::ShellOrigin::Default => Some("$SHELL is unset or empty, so aterm starts it"),
    };
    (program.to_string_lossy().into_owned(), label)
}

/// `aterm doctor`'s own facts (the shell, the macOS consent posture, unreachable instances,
/// the fabric's broker, the recovery ledger and open exceptions) as [`doctor_rows`] makes
/// them; the stale copies are left to their item ([`doctor::Gathered`]).
///
/// With `fix` (every run but `--read-only`), when aterm is not on the Developer Tools list
/// the person is brought to it ([`devtools_hand_over`], at most once per ten minutes), then
/// the list is re-read.
fn doctor_facts_rows(fix: bool) -> Vec<DoctorRow> {
    // The program check first (one `ps -p`, under 10 ms, bounded): while its pool is at
    // the kernel's limit the reads that list every program's command line are not made
    // ([`args_read_withheld`]).
    let program_check = program_check_fact();
    let withheld = args_read_withheld(program_check.as_ref());
    std::thread::scope(|scope| {
        // The Developer Tools read (one `sqlite3` and one `log show`, up to 8 s)
        // runs beside the other facts, not after them.
        let devtools = scope.spawn(move || devtools_facts(fix));
        let spotlight = scope.spawn(spotlight::facts);
        let privacy = privacy_facts();
        let debugger = debugger_facts();
        let instances = match withheld {
            Some(why) => Err(why.to_owned()),
            None => aterm_ctl::census::window_census(),
        };
        let recovery = recovery_facts();
        let exceptions = exceptions_facts();
        let mut machine = machine_facts(withheld);
        let devtools = devtools
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        machine.spotlight = spotlight
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        machine.program_check = program_check;
        // Unix: the program this passthrough session spawns.
        #[cfg(not(windows))]
        {
            let (shell, origin) = unix_shell(aterm_pty::select_shell_with_origin(None));
            doctor_rows(
                &shell,
                driver::shell_check(&shell),
                origin,
                &privacy,
                &debugger,
                &devtools,
                &instances,
                &recovery,
                &exceptions,
                &machine,
            )
        }
        // Windows: this tab's shell inside an aterm tab, else the one a new
        // window's tab spawns, labelled with its origin. See `driver::WindowShell`.
        #[cfg(windows)]
        {
            let window_shell = driver::WindowShell::resolve();
            doctor_rows(
                &window_shell.program,
                driver::shell_check(&window_shell.program),
                Some(&window_shell.origin_sentence()),
                &privacy,
                &debugger,
                &devtools,
                &instances,
                &recovery,
                &exceptions,
                &machine,
            )
        }
    })
}

/// `aterm-cli`'s half of `aterm doctor` run alone, as `crates/aterm` runs it with its
/// packages' items beside: gathered with `fix`, applied (the stale copies to the Trash)
/// or, without `fix`, planned. For the tests that read the live report, which run on
/// macOS and Windows alone.
#[cfg(all(test, any(windows, target_os = "macos")))]
fn doctor_report(fix: bool) -> (String, i32) {
    let gathered = doctor::gather(fix);
    let items = gathered.items();
    let mode = if fix {
        aterm_converge::Mode::Apply
    } else {
        aterm_converge::Mode::Plan
    };
    let rows = aterm_converge::run_scoped(
        &items,
        mode,
        aterm_converge::Scope::REPAIR,
        &mut aterm_converge::ReadOnly,
        &mut |_| {},
    )
    .expect("doctor's items name no other");
    doctor::render(&gathered, &rows, &[])
}

/// The stale copies of aterm as `doctor` reads them ([`atpkg::copies`]), with
/// what this run did about them, the console host a new window's tab runs
/// in, and the fabric's broker. Each is `None` where there is nothing to say
/// (copies off macOS, the console host off Windows, the fabric off Unix).
struct MachineFacts {
    /// What the stale-copy prune found and did.
    copies: Option<atpkg::copies::Pruned>,
    /// Windows: the console host a new window's tab gets ([`console_host_fact`]).
    console_host: Option<aterm_pty::ConsoleHostReport>,
    /// Unix: the fabric's broker in one row ([`fabric_fact`]).
    fabric: Option<(Mark, String, bool)>,
    /// macOS: what Spotlight indexes of Rust build output and rustup's docs
    /// ([`spotlight::facts`]).
    spotlight: Option<Result<spotlight::Spotlit, String>>,
    /// macOS: one look at the program check's pool
    /// ([`aterm_uds::program_check::look`]).
    program_check: Option<aterm_uds::program_check::Look>,
}

impl MachineFacts {
    /// Nothing to say here: no row at all.
    fn absent() -> Self {
        Self {
            copies: None,
            console_host: None,
            fabric: None,
            spotlight: None,
            program_check: None,
        }
    }
}

/// Gather [`MachineFacts`]: the console host and the fabric's broker, whose process table
/// is not read while `withheld` says why ([`args_read_withheld`]). The stale copies are
/// their own item ([`doctor::Gathered`]), whose probe looks and whose apply prunes.
fn machine_facts(withheld: Option<&str>) -> MachineFacts {
    MachineFacts {
        console_host: console_host_fact(),
        fabric: match withheld {
            Some(why) => Some((Mark::Note, format!("not read ({why})"), true)),
            None => fabric_fact(),
        },
        ..MachineFacts::absent()
    }
}

/// Read the program check's pool for its row (`None` off macOS): one bounded `ps`.
fn program_check_fact() -> Option<aterm_uds::program_check::Look> {
    cfg!(target_os = "macos")
        .then(|| aterm_uds::program_check::look(aterm_uds::program_check::PS_HANG))
}

/// Why doctor lists no program's command line now: macOS's program check is at the
/// kernel's thread limit.
const ARGS_WITHHELD: &str = "macOS's program check is at its thread limit, and listing every \
                             program's command line would wait on the programs it holds \
                             mid-start; see the `syspolicyd:` row";

/// [`ARGS_WITHHELD`] while `look` reads `syspolicyd`'s pool at the kernel's limit, else
/// `None`. The instance census and the fabric's process table read every program's
/// arguments, and reading a program the check holds mid-start waits with it, for good in a
/// deadlock (M29 of `docs/DESIGN-leftover-processes-2026-10-16.md`): doctor would print
/// nothing, its `syspolicyd:` row included, and leave its `ps` hung. Pure.
fn args_read_withheld(look: Option<&aterm_uds::program_check::Look>) -> Option<&'static str> {
    matches!(look, Some(aterm_uds::program_check::Look::Read(r)) if r.full())
        .then_some(ARGS_WITHHELD)
}

/// The `syspolicyd:` row (macOS), pure over one look: `ok` while
/// `syspolicyd`'s pool has room or none runs, a `warn` while the kernel holds
/// it at its limit (held there it is deadlocked, which only a restart ends;
/// one reading cannot say how long it has stood, which a window's `status`
/// does), a `note` when it could not be read. Never a failure.
fn program_check_rows(facts: &MachineFacts) -> Vec<DoctorRow> {
    use aterm_uds::program_check::Look;
    let Some(look) = &facts.program_check else {
        return Vec::new();
    };
    let (mark, fact) = match look {
        Look::Unread => (
            Mark::Note,
            "syspolicyd's pool could not be read (the process table, syspolicyd's image or \
             `ps` gave no reading)"
                .to_owned(),
        ),
        Look::NoneRunning => (
            Mark::Ok,
            "no syspolicyd runs (launchd starts one at the next request)".to_owned(),
        ),
        Look::Read(r) => {
            let limit = r
                .limit
                .map_or_else(|| "its limit".to_owned(), |l| format!("a limit of {l}"));
            if r.full() {
                (
                    Mark::Warn,
                    format!(
                        "syspolicyd is at the kernel's thread limit ({} pool threads against \
                         {limit})\n\
                         held there it is deadlocked: no new program starts until the Mac \
                         restarts\n\
                         a window's `aterm ctl @self status` says syspolicyd=stuck once it \
                         has held five minutes",
                        r.workers
                    ),
                )
            } else {
                (
                    Mark::Ok,
                    format!(
                        "syspolicyd answers ({} pool threads against {limit})",
                        r.workers
                    ),
                )
            }
        }
    };
    vec![DoctorRow {
        label: "syspolicyd:",
        mark,
        fact,
        unmeasured: matches!(look, Look::Unread),
    }]
}

/// Probe the fabric broker for the `fabric:` row (`None` off Unix). Never [`Mark::Fail`].
fn fabric_fact() -> Option<(Mark, String, bool)> {
    #[cfg(unix)]
    {
        let summary = aterm_link::fabric::doctor_summary();
        Some((fabric_mark(summary.level), summary.text, summary.unmeasured))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(unix)]
const fn fabric_mark(level: aterm_link::fabric::SummaryLevel) -> Mark {
    use aterm_link::fabric::SummaryLevel;
    match level {
        SummaryLevel::Ok => Mark::Ok,
        SummaryLevel::Note => Mark::Note,
        SummaryLevel::Warn => Mark::Warn,
    }
}

fn fabric_rows(facts: &MachineFacts) -> Vec<DoctorRow> {
    facts
        .fabric
        .as_ref()
        .map(|(mark, fact, unmeasured)| DoctorRow {
            label: "fabric:",
            mark: *mark,
            fact: fact.clone(),
            unmeasured: *unmeasured,
        })
        .into_iter()
        .collect()
}

/// Probe the Windows console host for a new window's tab (`None` off Windows).
fn console_host_fact() -> Option<aterm_pty::ConsoleHostReport> {
    #[cfg(windows)]
    {
        Some(aterm_pty::probe_console_host(
            &aterm_pty::ConsoleHostChoice::from_config(driver::config_conpty().as_deref()),
        ))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Read the Developer Tools list and, when `fix` is set and aterm is `NotGranted`
/// on a local session, hand over the System Settings pane via [`devtools_hand_over`].
fn devtools_facts(fix: bool) -> DevtoolsFacts {
    let (facts, did) = devtools_read_and_hand_over(fix);
    DevtoolsFacts { facts, did }
}

fn devtools_read_and_hand_over(fix: bool) -> (Option<atpkg::devtools::Facts>, Option<String>) {
    use atpkg::devtools as dt;
    use std::io::IsTerminal as _;
    let Some(facts) = dt::facts() else {
        return (None, None);
    };
    let over_ssh =
        std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
    let person = std::io::stdout().is_terminal();
    let subject = facts.subject.clone().unwrap_or_default();
    let version = aterm_types::version::app_version();
    let reach = DevtoolsReach {
        ask_window: &ask_window_for_devtools,
        list_terminal: &dt::list_terminal,
        open_pane: &dt::open_pane,
        opened_within: &dt::opened_within,
        note_opened: &|| dt::note_opened(version),
        declined_at: &|| dt::declined_until().map(|declined| declined.at),
        await_on: &|| {
            let list = dt::await_switch(&subject, std::time::Duration::from_secs(120), |id| {
                aterm_containment::probe_devtool_list(aterm_containment::ProbeGate::on(), id)
            });
            list.listing == aterm_containment::DevToolListing::On
        },
        refresh_verdict: &dt::refresh_verdict,
        local_day: &|at| local_day_words(at, i64::try_from(at).ok().and_then(local_offset_at)),
        say: &|words| eprintln!("{words}"),
    };
    let (facts, did) = devtools_hand_over_when(fix, facts, over_ssh, person, &reach, dt::facts);
    (Some(facts), did)
}

/// The fix-mode hand-over over the facts read: nothing without `fix` or when aterm is not
/// missing from the list, a sentence over ssh, else [`devtools_hand_over`] — the window
/// first, doctor's own open at most once per [`atpkg::devtools::REOPEN_AFTER_S`] — and
/// the list read again (`reread`) when this run opened the page. Split out so the
/// throttle `aterm doctor` keeps is proved through the path it runs.
fn devtools_hand_over_when(
    fix: bool,
    facts: atpkg::devtools::Facts,
    over_ssh: bool,
    person: bool,
    reach: &DevtoolsReach<'_>,
    reread: impl FnOnce() -> Option<atpkg::devtools::Facts>,
) -> (atpkg::devtools::Facts, Option<String>) {
    if !fix || facts.grant() != aterm_containment::DevToolGrant::NotGranted {
        return (facts, None);
    }
    if over_ssh {
        return (
            facts,
            Some(
                "did nothing: this shell is over ssh, so no pane can open on the Mac's screen"
                    .to_string(),
            ),
        );
    }
    let (did, opened) = devtools_hand_over(&facts, person, reach);
    // Re-read: the row says what holds now, not what held before the hand-over.
    let facts = if opened {
        reread().unwrap_or(facts)
    } else {
        facts
    };
    (facts, Some(did))
}

/// What the running aterm window answered `devtools hand-over`, as `aterm
/// doctor` acts on it ([`ask_window_for_devtools`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum WindowAnswer {
    /// No aterm instance answers: none runs, or the request never went out (the
    /// connect phase failed, or refused it: `ERR auth`, `ERR control server
    /// busy`). Doctor opens the page itself.
    Unreachable,
    /// The request reached the window and no reply came (the words say how): the
    /// window may still act, so doctor opens nothing.
    Unanswered(String),
    /// The window's reply line (`OK …` / `ERR …`).
    Reply(String),
}

/// Ask the aterm instance this shell reaches (`aterm ctl`'s flagless rule, a
/// live socket) for the Developer Tools hand-over: `devtools hand-over`, or with
/// `now` for a person at this terminal. The window holds the ask to its own
/// rules (the verb's help).
fn ask_window_for_devtools(now: bool) -> WindowAnswer {
    let Some(sock) = aterm_ctl::front_door_instance() else {
        return WindowAnswer::Unreachable;
    };
    let request = if now {
        "devtools hand-over now\n"
    } else {
        "devtools hand-over\n"
    };
    window_answer(aterm_ctl::front_door_send(&sock, request))
}

/// What one [`aterm_ctl::front_door_send`] of the hand-over came to. A line the
/// connect phase returned in place of an answer (`ERR auth; …`, `ERR control
/// server busy; retry`) means the request never went out, so nothing was
/// decided about it: no instance answered, as when the connect itself failed.
/// An error after the request was written is the window's silence: it may
/// still act. Pure.
fn window_answer(sent: std::io::Result<String>) -> WindowAnswer {
    match sent {
        Ok(reply) if aterm_ctl::front_door_refused(&reply) => WindowAnswer::Unreachable,
        Ok(reply) => WindowAnswer::Reply(reply),
        Err(error) if aterm_ctl::front_door_delivered(&error) => {
            WindowAnswer::Unanswered(error.to_string())
        }
        Err(_) => WindowAnswer::Unreachable,
    }
}

/// What `aterm doctor`'s Developer Tools hand-over reaches beyond the facts it
/// read: the window, atpkg's two acts, the shared markers, the wait and the
/// terminal. Injected, so a test asks, runs, opens, writes, waits for and prints
/// nothing.
struct DevtoolsReach<'a> {
    /// Ask the running aterm window ([`ask_window_for_devtools`]); the argument
    /// is `now` (a person at this terminal).
    ask_window: &'a dyn Fn(bool) -> WindowAnswer,
    /// [`atpkg::devtools::list_terminal`].
    list_terminal: &'a dyn Fn() -> Result<(), String>,
    /// [`atpkg::devtools::open_pane`]: `/usr/bin/open`, the path measured to work
    /// from a foreground command line.
    open_pane: &'a dyn Fn() -> Result<(), String>,
    /// [`atpkg::devtools::opened_within`]: seconds since aterm last opened the
    /// pane, when under the window.
    opened_within: &'a dyn Fn(u64) -> Option<u64>,
    /// [`atpkg::devtools::note_opened`] for this version.
    note_opened: &'a dyn Fn(),
    /// When the owner said Not now to the panel (unix seconds), while it holds
    /// ([`atpkg::devtools::declined_until`]).
    declined_at: &'a dyn Fn() -> Option<u64>,
    /// Wait up to two minutes for aterm's switch: `true` once it reads on.
    await_on: &'a dyn Fn() -> bool,
    /// [`atpkg::devtools::refresh_verdict`]: make macOS ask about aterm again once the
    /// switch read on (the running aterm, checked before, keeps that verdict until a
    /// fresh aterm process asks).
    refresh_verdict: &'a dyn Fn() -> Result<(), String>,
    /// A unix time as the calendar day it fell on here ([`local_day_words`]).
    local_day: &'a dyn Fn(u64) -> String,
    /// Where the banner and the wait line go (stderr: the report itself goes to
    /// stdout only at the end).
    say: &'a dyn Fn(&str),
}

/// The line printed while doctor waits for the switch. ctrl-c ends doctor, not
/// only the wait (doctor installs no handler), so it says that.
const DEVTOOLS_WAIT_LINE: &str = "aterm doctor: waiting up to 2 min for aterm's switch to read on \
                                  in Developer Tools (ctrl-c stops doctor)\u{2026}";

/// Coordinate the Developer Tools hand-over with any running aterm window.
///
/// Asks the running window first so its helper panel can appear beside System Settings,
/// and opens the pane directly only when no window answers, the window is older than
/// the verb, or `now` left the page open to `doctor`. Returns `(did, opened)`.
fn devtools_hand_over(
    facts: &atpkg::devtools::Facts,
    person: bool,
    reach: &DevtoolsReach<'_>,
) -> (String, bool) {
    use atpkg::devtools as dt;
    let reply = match (reach.ask_window)(person) {
        WindowAnswer::Unreachable => {
            return devtools_open_here(facts, person, reach, true, !person);
        }
        WindowAnswer::Unanswered(_) if person => {
            return devtools_open_here(facts, person, reach, true, false);
        }
        WindowAnswer::Unanswered(why) => {
            return (
                format!(
                    "did nothing: asked the aterm window for the page and it did not answer \
                     ({why}); it may still bring the page up at a quiet moment, so doctor \
                     opened nothing"
                ),
                false,
            );
        }
        WindowAnswer::Reply(reply) => reply,
    };
    let no_window_press = format!(
        "{} \u{25b8} Open Developer Tools\u{2026} opens the page",
        dt::STATUS_PAGE
    );
    let did = match reply.as_str() {
        "OK shown" => return devtools_open_here(facts, person, reach, false, false),
        "OK unshown" => return devtools_open_here(facts, person, reach, true, false),
        "OK granted" | "OK unknown" if person => {
            return devtools_open_here(facts, person, reach, true, false);
        }
        _ if reply.starts_with("ERR unknown verb") => {
            return devtools_open_here(facts, person, reach, true, !person);
        }
        "OK queued" => format!(
            "asked the aterm window to show the owner the page at a quiet moment (status: {})",
            dt::STATUS_PAGE
        ),
        "OK recent" if person => format!(
            "did nothing: within the last minute aterm opened the page, or the aterm window \
             answered a request for it, or aterm's Full Disk Access page is waiting or open and \
             goes first; {no_window_press}"
        ),
        "OK recent" => match (reach.opened_within)(dt::REOPEN_AFTER_S) {
            Some(ago) => format!(
                "did nothing: aterm opened the pane {}",
                opened_ago_words(ago)
            ),
            None => format!(
                "did nothing: the aterm window says aterm opened the page within the last {} \
                 minutes",
                dt::REOPEN_AFTER_S / 60
            ),
        },
        "OK declined" => format!(
            "did nothing: you said Not now{}; {no_window_press}",
            (reach.declined_at)()
                .map_or_else(String::new, |at| format!(" on {}", (reach.local_day)(at)))
        ),
        "OK granted" => format!(
            "did nothing: the aterm window's last reading has aterm on the list and switched \
             on; {no_window_press}"
        ),
        "OK unknown" => format!(
            "did nothing: the aterm window could not tell from its last reading whether aterm \
             is on the list, so it leaves the list alone; {no_window_press}"
        ),
        "OK listed" => format!(
            "did nothing: the aterm window reads aterm's row on while macOS still checks its \
             new programs, so the page holds nothing to add; {no_window_press}"
        ),
        "OK unstable" => format!(
            "did nothing: this aterm is an ad hoc or dev build, whose grant the next build \
             would lose, so the window brings nothing up by itself; {no_window_press}"
        ),
        _ if reply.starts_with("ERR headless") => {
            "did nothing: the aterm instance doctor reached is headless, so no page opens"
                .to_string()
        }
        _ => format!("did nothing: the aterm window answered `{reply}`"),
    };
    (did, false)
}

/// Open the Developer Tools pane directly from `doctor` and optionally wait for the switch.
fn devtools_open_here(
    facts: &atpkg::devtools::Facts,
    person: bool,
    reach: &DevtoolsReach<'_>,
    full: bool,
    rate_limited: bool,
) -> (String, bool) {
    use atpkg::devtools as dt;
    if rate_limited && let Some(at) = (reach.declined_at)() {
        return (
            format!(
                "did nothing: you said Not now on {}; {} \u{25b8} Open Developer Tools\u{2026} \
                 opens the page",
                (reach.local_day)(at),
                dt::STATUS_PAGE
            ),
            false,
        );
    }
    if rate_limited && let Some(ago) = (reach.opened_within)(dt::REOPEN_AFTER_S) {
        return (
            format!(
                "did nothing: aterm opened the pane {}",
                opened_ago_words(ago)
            ),
            false,
        );
    }
    // Whether `spctl` added Terminal, for the banner's word on which switch matters.
    let listed = std::cell::Cell::new(false);
    let list_terminal = || {
        let outcome = (reach.list_terminal)();
        listed.set(outcome.is_ok());
        outcome
    };
    let open_pane = || {
        if person {
            (reach.say)(&devtools_banner(facts, full, listed.get()));
        }
        (reach.open_pane)()
    };
    let handled = dt::hand_to_owner(
        facts,
        &dt::Reach {
            list_terminal: &list_terminal,
            open_pane: &open_pane,
        },
    );
    let mut did = dt::handled_words(&handled);
    if !matches!(handled, dt::Handled::Opened { .. }) {
        return (did, false);
    }
    (reach.note_opened)();
    if person && facts.list.label == aterm_containment::DevToolListLabel::Read {
        (reach.say)(DEVTOOLS_WAIT_LINE);
        let switched = (reach.await_on)();
        if switched && let Err(why) = (reach.refresh_verdict)() {
            (reach.say)(&format!(
                "aterm doctor: could not make macOS ask about aterm again: {why}"
            ));
        }
        did.push_str(if switched {
            "; you switched it on"
        } else {
            "; not switched on within 2 min"
        });
    }
    (did, true)
}

/// The calendar day `at` (unix seconds) fell on in the local zone, for a
/// did-line: `YYYY-MM-DD` at `offset_s` east of UTC, or the UTC day marked
/// `(UTC)` when the zone could not be read. Pure.
fn local_day_words(at: u64, offset_s: Option<i64>) -> String {
    let shifted = |offset: i64| {
        let at = i64::try_from(at).unwrap_or(i64::MAX).saturating_add(offset);
        u64::try_from(at.max(0)).unwrap_or(0)
    };
    let mut day = aterm_types::rfc3339::format_rfc3339(shifted(offset_s.unwrap_or(0)));
    day.truncate(10);
    if offset_s.is_none() {
        day.push_str(" (UTC)");
    }
    day
}

/// How long ago aterm opened the pane, `ago` seconds back, for a did-line.
fn opened_ago_words(ago: u64) -> String {
    if ago < 60 {
        "under a minute ago".to_string()
    } else {
        format!("{} min ago", ago / 60)
    }
}

/// Format the stderr banner printed when `doctor` opens System Settings.
fn devtools_banner(facts: &atpkg::devtools::Facts, full: bool, listed_terminal: bool) -> String {
    use atpkg::devtools as dt;
    let skips = aterm_containment::SpikeEvidence::MEASURED.devtool_skips_first_run_check;
    let status = format!("  {}", dt::STATUS_LINK);
    if !full {
        let mut short = format!(
            "aterm doctor: {}: aterm's panel beside System Settings shows how.",
            dt::HELPER_TITLE
        );
        if listed_terminal {
            short.push_str(&format!("\n  {}", dt::only_aterm(true)));
        }
        return format!("{short}\n{status}");
    }
    let how = dt::Step::of(&facts.list).how();
    [
        format!("aterm doctor: {}", dt::HELPER_TITLE),
        format!("  {}", dt::why(skips)),
        format!("  {how}"),
        format!("  {}", dt::only_aterm(listed_terminal)),
        format!("  {}", dt::trade(skips)),
        status,
    ]
    .join("\n")
}

/// The pure half of the Windows driver's `config_conpty`: aterm.toml's
/// top-level `conpty` string, parsed with `aterm_toml` as the window parses it.
/// No key, a key inside a table, a non-string and a file that does not parse
/// (the window then loads none of it) are all `None`: the window's default.
#[cfg(any(windows, test))]
fn config_conpty_from_text(text: &str) -> Option<String> {
    let table: aterm_toml::Table = text.parse().ok()?;
    table.get("conpty")?.as_str().map(str::to_string)
}

/// The `conpty:` row (Windows): `ok` for OpenConsole and for the Windows
/// console host the choice asked for (`inbox`), a `note` when the OpenConsole
/// the choice named (the bundled default, `auto` or a path) fell back — never a
/// failure: every tab opens either way.
fn console_host_rows(facts: &MachineFacts) -> Vec<DoctorRow> {
    let Some(host) = &facts.console_host else {
        return Vec::new();
    };
    vec![DoctorRow {
        label: "conpty:",
        mark: if host.fell_back { Mark::Note } else { Mark::Ok },
        fact: host.line.clone(),
        unmeasured: false,
    }]
}

/// The `spotlight:` row, pure over the count ([`spotlight::row`]): `ok` when Spotlight
/// indexes none of it, a `note` naming the folders and the remedy otherwise; none off
/// macOS.
fn spotlight_rows(facts: &MachineFacts) -> Vec<DoctorRow> {
    facts
        .spotlight
        .as_ref()
        .map(|measured| {
            let (clean, fact) = spotlight::row(measured);
            DoctorRow {
                label: "spotlight:",
                mark: if clean { Mark::Ok } else { Mark::Note },
                fact,
                unmeasured: measured.is_err(),
            }
        })
        .into_iter()
        .collect()
}

/// The `copies:` row, pure over the facts: atpkg's words and weights, never a
/// failure (a `warn` moves no exit code: nothing in it stops a shell from
/// starting).
fn copies_rows(facts: &MachineFacts) -> Vec<DoctorRow> {
    let Some(pruned) = &facts.copies else {
        return Vec::new();
    };
    let row = atpkg::copies::row(pruned);
    vec![DoctorRow {
        label: "copies:",
        mark: mark_of_level(row.level),
        fact: row.text,
        unmeasured: matches!(
            pruned,
            atpkg::copies::Pruned::NotLooked(_)
                | atpkg::copies::Pruned::Incomplete { .. }
                | atpkg::copies::Pruned::Found {
                    complete: false,
                    ..
                }
        ),
    }]
}

/// atpkg's row weight in this report's marks.
fn mark_of_level(level: atpkg::doctor::Level) -> Mark {
    match level {
        atpkg::doctor::Level::Ok => Mark::Ok,
        atpkg::doctor::Level::Warn => Mark::Warn,
        atpkg::doctor::Level::Note => Mark::Note,
    }
}

/// The recovery ledger as `doctor` reads it: its rows and path, or why the history
/// could not be read (`aterm_update::recovery_ledger`).
type RecoveryFacts = Result<(Vec<aterm_update::recovery_ledger::Row>, std::path::PathBuf), String>;

/// Read the recovery ledger — the rows the window's recovery census writes at each
/// windowed launch of the installed app. Read-only and bounded.
fn recovery_facts() -> RecoveryFacts {
    let path = aterm_update::recovery_ledger::default_ledger_path()
        .ok_or_else(|| "no log folder, so there is no ledger".to_owned())?;
    aterm_update::recovery_ledger::read_rows_checked(&path)
        .map(|rows| (rows, path.clone()))
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// The `recovery:` row, pure over the ledger: how the runs before each recorded
/// windowed launch ended, with the last unexpected end in detail. A note when
/// any run ended unexpectedly, OK otherwise. On Windows the window writes no
/// ledger, and the row says so.
fn recovery_row(facts: &RecoveryFacts) -> (Mark, String) {
    if cfg!(windows) {
        return (Mark::Note, "not recorded on Windows".to_string());
    }
    let (rows, path) = match facts {
        Ok(read) => read,
        Err(why) => return (Mark::Note, format!("not measured ({why})")),
    };
    let (lines, unexpected) = aterm_update::recovery_ledger::doctor_lines(rows, path);
    let mark = if unexpected { Mark::Note } else { Mark::Ok };
    (mark, lines.join("\n"))
}

/// The `instances:` row, pure over [`aterm_ctl::census`]: aterm windows and
/// headless instances of this user that no published control socket reaches
/// (invisible to `aterm ctl ls`).
///
/// Never [`Mark::Fail`] and never stops anything: it names each one (pid, age,
/// command line) for the reader to decide. A failed census says
/// `not measured` and why.
fn instances_row(census: &Result<aterm_ctl::census::Census, String>) -> (Mark, String) {
    let census = match census {
        Ok(census) => census,
        Err(why) => return (Mark::Note, format!("not measured ({why})")),
    };
    let (n, dir) = (census.instances, &census.dir);
    if census.unreached.is_empty() {
        let line = match n {
            0 => "no aterm window or headless instance is running as you".to_string(),
            1 => format!("1 aterm window or headless instance, published in {dir}"),
            n => format!("{n} aterm windows and headless instances, each published in {dir}"),
        };
        return (Mark::Ok, line);
    }
    let k = census.unreached.len();
    let noun = if n == 1 { "instance" } else { "instances" };
    let (verb, pronoun) = if k == 1 {
        ("is", "it")
    } else {
        ("are", "them")
    };
    let mut detail = format!(
        "{k} of {n} aterm {noun} {verb} reachable by no control socket in {dir} \
         (`aterm ctl ls` cannot see {pronoun})"
    );
    for u in &census.unreached {
        detail.push_str(&format!("\npid {} (up {}): {}", u.pid, u.age, u.command));
        if u.socketless {
            detail.push_str(" — started with its control socket off");
        }
    }
    (Mark::Note, detail)
}

/// Pure core of `doctor` without a `shell_origin` label.
#[cfg(test)]
fn doctor_checks(
    shell: &str,
    shell_check: ShellCheck,
    privacy: &PrivacyFacts,
    debugger: &DebuggerFacts,
    instances: &Result<aterm_ctl::census::Census, String>,
    recovery: &RecoveryFacts,
    exceptions: &ExceptionsFacts,
) -> (String, i32) {
    doctor_checks_with_origin(
        shell,
        shell_check,
        None,
        privacy,
        debugger,
        &DevtoolsFacts::absent(),
        instances,
        recovery,
        exceptions,
        &MachineFacts::absent(),
    )
}

const SHELL_REMEDY: &str = if cfg!(windows) {
    "set `shell` in aterm.toml to an installed shell"
} else {
    "set $SHELL to an installed shell"
};

/// Render the `doctor` report and exit code (`0` = no [`Mark::Fail`], `1` = any [`Mark::Fail`]).
///
/// Appends `shell_origin` to the `shell:` row when present and omits rows whose
/// platform probe returned `UnsupportedPlatform`.
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is one gathered fact; passing them in is what keeps the report pure"
)]
fn doctor_rows(
    shell: &str,
    shell_check: ShellCheck,
    shell_origin: Option<&str>,
    privacy: &PrivacyFacts,
    debugger: &DebuggerFacts,
    devtools: &DevtoolsFacts,
    instances: &Result<aterm_ctl::census::Census, String>,
    recovery: &RecoveryFacts,
    exceptions: &ExceptionsFacts,
    machine: &MachineFacts,
) -> Vec<DoctorRow> {
    let (shell_ok, shell_fact) = match (shell, shell_check) {
        ("", _) => (false, "$SHELL is empty".to_string()),
        (p, ShellCheck::Runs) => (true, format!("{p} (executable)")),
        (p, ShellCheck::Missing) => (false, format!("{p} does not exist")),
        (p, ShellCheck::NotExecutable) => (false, format!("{p} is not executable")),
    };
    let shell_fact = match shell_origin {
        Some(origin) => format!("{shell_fact} — {origin}"),
        None => shell_fact,
    };

    let mut checks = vec![DoctorRow {
        label: "shell:",
        mark: Mark::from_ok(shell_ok),
        fact: shell_fact,
        unmeasured: false,
    }];
    if privacy.fda_label != ProbeLabel::UnsupportedPlatform {
        let (mark, fact) = privacy_row(privacy);
        checks.push(DoctorRow {
            label: "privacy:",
            mark,
            fact,
            unmeasured: privacy.fda_label.refused() || privacy.fda == FdaState::Unknown,
        });
    }
    if debugger.probe.label != DebuggerProbeLabel::UnsupportedPlatform {
        let (mark, fact) = debugger_row(debugger);
        checks.push(DoctorRow {
            label: "debugger:",
            mark,
            fact,
            unmeasured: debugger.probe.label.refused()
                || matches!(
                    debugger.probe.mode,
                    DebuggerMode::Unknown | DebuggerMode::Unrecognized
                )
                || debugger.hardening == SelfHardening::Unknown
                || debugger.probe.member == DebuggerMember::Unknown,
        });
    }
    if let Some(row) = devtools.facts.as_ref().and_then(atpkg::devtools::row) {
        // atpkg's weights: a `warn` is counted, never a failure.
        let mark = mark_of_level(row.level);
        let fact = match &devtools.did {
            Some(did) => format!("{}\n{did}", row.text),
            None => row.text,
        };
        checks.push(DoctorRow {
            label: "devtools:",
            mark,
            fact,
            unmeasured: devtools.facts.as_ref().is_some_and(|facts| {
                facts.grant() == aterm_containment::consent::DevToolGrant::Unknown
            }),
        });
    }
    checks.extend(copies_rows(machine));
    checks.extend(spotlight_rows(machine));
    checks.extend(program_check_rows(machine));
    checks.extend(console_host_rows(machine));
    let (instances_mark, instances_fact) = instances_row(instances);
    let (recovery_mark, recovery_fact) = recovery_row(recovery);
    let (exceptions_mark, exceptions_fact) = exceptions_row(exceptions);
    checks.push(DoctorRow {
        label: "instances:",
        mark: instances_mark,
        fact: instances_fact,
        unmeasured: instances.is_err(),
    });
    // The fabric beside the instances that carry it.
    checks.extend(fabric_rows(machine));
    checks.extend([
        DoctorRow {
            label: "recovery:",
            mark: recovery_mark,
            fact: recovery_fact,
            unmeasured: !cfg!(windows) && recovery.is_err(),
        },
        DoctorRow {
            label: "exceptions:",
            mark: exceptions_mark,
            fact: exceptions_fact,
            unmeasured: exceptions.is_err(),
        },
    ]);

    checks
}

/// [`doctor_rows`] through the engine and rendered: the report the pure tests read,
/// planned and never applied — the items and the renderer `aterm doctor` runs.
#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is one gathered fact; passing them in is what keeps the report pure"
)]
fn doctor_checks_with_origin(
    shell: &str,
    shell_check: ShellCheck,
    shell_origin: Option<&str>,
    privacy: &PrivacyFacts,
    debugger: &DebuggerFacts,
    devtools: &DevtoolsFacts,
    instances: &Result<aterm_ctl::census::Census, String>,
    recovery: &RecoveryFacts,
    exceptions: &ExceptionsFacts,
    machine: &MachineFacts,
) -> (String, i32) {
    let gathered = doctor::Gathered::of_rows(doctor_rows(
        shell,
        shell_check,
        shell_origin,
        privacy,
        debugger,
        devtools,
        instances,
        recovery,
        exceptions,
        machine,
    ));
    let rows = gathered.plan();
    doctor::render(&gathered, &rows, &[])
}

/// The report's lines, the version and the health line, from one mark and fact per
/// row ([`doctor::render`] reads them off the rows the engine returned): only
/// [`Mark::Fail`] fails, and the health line names every `warn`.
fn render_doctor_lines(checks: &[DoctorRow]) -> (String, i32) {
    // Only the shell row can be `Fail`, so a failing report names it and the fix.
    let all_ok = !checks.iter().any(|row| row.mark.is_fail());
    let warned: Vec<&str> = checks
        .iter()
        .filter(|row| row.mark == Mark::Warn)
        .map(|row| row.label.trim_end_matches(':'))
        .collect();

    let mut out = render_doctor_rows(checks);
    if all_ok && warned.is_empty() {
        out.push_str("health: OK\n");
    } else if all_ok {
        out.push_str(&format!(
            "health: OK — {} warning{} ({})\n",
            warned.len(),
            if warned.len() == 1 { "" } else { "s" },
            warned.join(", ")
        ));
    } else {
        out.push_str(&format!(
            "health: FAIL — aterm cannot start this shell; {SHELL_REMEDY}\n"
        ));
    }
    (out, i32::from(!all_ok))
}

/// Facts and version, before the caller's health/readiness assessment.
fn render_doctor_rows(checks: &[DoctorRow]) -> String {
    // One row per check: the label, the mark, the fact. A fact of several lines
    // keeps its later lines under the fact column.
    const LABEL: usize = "exceptions: ".len();
    let mut out = String::new();
    for row in checks {
        let mut lines = row.fact.lines();
        out.push_str(&format!(
            "{:<LABEL$}{:<5} {}\n",
            row.label,
            row.mark.render(),
            lines.next().unwrap_or_default()
        ));
        for more in lines {
            out.push_str(&" ".repeat(LABEL + 6));
            out.push_str(more);
            out.push('\n');
        }
    }
    out.push_str(&format!(
        "version: {}\n\n",
        aterm_types::version::app_version()
    ));
    out
}

/// The outcome of parsing the command line: print-and-exit (help/version),
/// reject (usage error), or proceed with an optional containment override
/// resolved by the init funnel.
#[derive(Debug, PartialEq, Eq)]
enum CliAction {
    /// `-h`/`--help`: print [`help_text`] to stdout, exit 0.
    Help,
    /// `-V`/`--version`: print the version to stdout, exit 0.
    Version,
    /// A diagnostic subcommand (e.g. `show-config`, `show-face <family>`):
    /// print introspection and exit without spawning a shell. `cmd` is one of
    /// [`DIAG_COMMANDS`]; `arg` is the optional operand after it.
    Diag { cmd: String, arg: Option<String> },
    /// `<diag> --help`: print that subcommand's description and usage, exit 0,
    /// never run it.
    DiagHelp { cmd: String },
    /// `help [topic]`: print the toolchain manual (via [`manual`]) and exit 0
    /// without spawning a shell. `None` prints the front page (or, inside an
    /// aterm session, the agent brief).
    Manual { topic: Option<String> },
    /// `agents [<cmd> [<agent>…]]`: manage the coding-agent primer and exit
    /// without spawning a shell. `rest` (everything after `agents`) is parsed
    /// by the `aterm-primer` crate.
    Agents { rest: Vec<String> },
    /// A usage error (unknown option, missing `--containment` value): the message
    /// is already framed for stderr; exit 2 without launching a shell.
    Usage(String),
    /// Proceed to launch. `containment` is the raw mode selection (if any) the
    /// init funnel parses (`None`: the default `User`). `quiet` is `-q`/`--quiet`:
    /// suppress the interactive startup notice. `verbose` is `--verbose`: the
    /// one-line session summary on exit.
    Run {
        containment: Option<String>,
        quiet: bool,
        verbose: bool,
    },
}

/// Pure argument-decision core: takes the argv tail and returns a
/// [`CliAction`]; [`parse_args`] is the effectful wrapper.
///
/// Recognizes `-h`/`--help`, `-V`/`--version`, `--containment <MODE>` /
/// `--containment=<MODE>`, `--sandbox` (= containment) and `--no-sandbox`
/// (= user). `aterm` takes no positional operands, so any operand (including
/// one after `--`) is rejected.
///
/// An explicit flag beats the default `User` (no environment variable selects
/// the mode), and among conflicting flags the last one wins. An invalid
/// `--containment` value is passed through verbatim so `init_mode_from_flag`
/// fails it closed to Containment.
fn decide_args<I: Iterator<Item = String>>(args: I) -> CliAction {
    let mut args = args.peekable();
    // `aterm help [topic]` dispatches first, git-style. The word `help` is the
    // toolchain manual; `-h`/`--help` is the terse CLI usage.
    if args.peek().map(String::as_str) == Some("help") {
        let _ = args.next();
        return CliAction::Manual { topic: args.next() };
    }
    // `aterm agents …`: everything after the word belongs to the
    // `aterm-primer` crate's parser.
    if args.peek().map(String::as_str) == Some("agents") {
        let _ = args.next();
        return CliAction::Agents {
            rest: args.collect(),
        };
    }
    // A leading diagnostic subcommand dispatches before any flag parsing;
    // anything else falls through to the option parser.
    if let Some(first) = args.peek()
        && DIAG_COMMANDS
            .iter()
            .any(|(name, _)| *name == first.as_str())
    {
        let cmd = args.next().expect("peeked Some");
        let rest: Vec<String> = args.collect();
        // `<diag> --help` describes the subcommand (as in `aterm ctl`) instead
        // of taking `-h` as an operand.
        if rest.iter().any(|a| a == "-h" || a == "--help") {
            return CliAction::DiagHelp { cmd };
        }
        // The tail is parsed, never dropped, so a wrong flag is an error rather
        // than silently accepted.
        let takes_operand = cmd == "show-face";
        // `aterm doctor`'s grammar ([`doctor::args`], [`doctor::GRAMMAR`]): the
        // grammar is checked here too, so a wrong flag is a usage error on every path.
        if cmd == "doctor" {
            return match doctor::args(&rest) {
                Ok(_) => CliAction::Diag {
                    cmd,
                    arg: (!rest.is_empty()).then(|| rest.join(" ")),
                },
                Err(usage) => CliAction::Usage(usage),
            };
        }
        // `aterm exceptions [--all]`: its one optional operand is a flag.
        if cmd == "exceptions" {
            return match rest.as_slice() {
                [] => CliAction::Diag { cmd, arg: None },
                [all] if all == "--all" => CliAction::Diag {
                    cmd,
                    arg: Some(all.clone()),
                },
                [other, ..] => CliAction::Usage(format!(
                    "aterm exceptions: unknown argument {other:?} — usage: aterm exceptions [--all]"
                )),
            };
        }
        if !takes_operand && !rest.is_empty() {
            return CliAction::Usage(format!(
                "aterm {cmd}: unknown argument {:?} — this command takes none \
                 (try `aterm {cmd} --help`)",
                rest[0]
            ));
        }
        if takes_operand && rest.is_empty() {
            return CliAction::Usage(SHOW_FACE_NEEDS_FAMILY.to_string());
        }
        if takes_operand && rest.len() > 1 {
            return CliAction::Usage(format!(
                "aterm show-face: unknown argument {:?} — usage: aterm show-face <family>",
                rest[1]
            ));
        }
        if takes_operand
            && let Some(op) = rest.first()
            && op.starts_with('-')
        {
            return CliAction::Usage(format!(
                "aterm show-face: {op:?} is not a font family — usage: aterm show-face <family>"
            ));
        }
        let arg = rest.into_iter().next();
        return CliAction::Diag { cmd, arg };
    }
    let mut containment: Option<String> = None;
    let mut quiet = false;
    let mut verbose = false;
    let mut opts_ended = false; // set by a literal `--`
    while let Some(arg) = args.next() {
        if !opts_ended {
            match arg.as_str() {
                "-h" | "--help" => return CliAction::Help,
                "-V" | "--version" => return CliAction::Version,
                "--" => {
                    opts_ended = true;
                    continue;
                }
                "--containment" => {
                    let Some(val) = args.next() else {
                        return CliAction::Usage(
                            "aterm: --containment requires a mode (try --help)".to_string(),
                        );
                    };
                    containment = Some(val);
                    continue;
                }
                // An empty value (`--containment=`) is passed through so it
                // fails closed in the init funnel.
                _ if arg.starts_with("--containment=") => {
                    containment = Some(arg["--containment=".len()..].to_string());
                    continue;
                }
                "--sandbox" => {
                    containment = Some("containment".to_string());
                    continue;
                }
                "--no-sandbox" => {
                    containment = Some("user".to_string());
                    continue;
                }
                "-q" | "--quiet" => {
                    quiet = true;
                    continue;
                }
                "--verbose" => {
                    verbose = true;
                    continue;
                }
                _ => {}
            }
        }
        // An unrecognized option, or any operand: `aterm` accepts none, so this
        // is an exit-2 usage error. A bare word is an unresolved command.
        let noun = if arg.starts_with('-') {
            "option"
        } else {
            "command"
        };
        return CliAction::Usage(format!("aterm: unknown {noun} {arg} (try --help)"));
    }
    CliAction::Run {
        containment,
        quiet,
        verbose,
    }
}

/// Format the `-V`/`--version` output: identity line, origin line, optional build
/// number, `which_copy` lines, and the compiled update trust anchor fingerprint.
#[must_use]
pub fn version_text(copy: Option<&aterm_update::which_copy::WhichCopy>) -> String {
    let mut out = format!(
        "aterm {}\n{}\n",
        aterm_types::version::app_version(),
        aterm_types::identity::ORIGIN_LINE
    );
    if let Some(build) = running_build() {
        out.push_str(&format!("build: {build}\n"));
    }
    if let Some(copy) = copy {
        for line in copy.lines() {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push_str(&trust_anchors_line());
    out
}

fn running_build() -> Option<u64> {
    let build = aterm_types::version::installed()?
        .build_number
        .parse::<u64>()
        .ok()?;
    (build > 0).then_some(build)
}

/// Hook run when the session's shell first writes output (used by the launcher
/// to confirm a replaced Linux executable's trial).
static FIRST_SHELL_OUTPUT: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Register the [`FIRST_SHELL_OUTPUT`] hook; returns `false` if already set.
pub fn on_first_shell_output(hook: fn()) -> bool {
    FIRST_SHELL_OUTPUT.set(hook).is_ok()
}

#[cfg(unix)]
fn first_shell_output() {
    if let Some(hook) = FIRST_SHELL_OUTPUT.get() {
        hook();
    }
}

fn trust_anchors_line() -> String {
    format!(
        "trusts: master={}\n",
        aterm_update::compiled_update_pin_sha256()
    )
}

/// Write `text` to stdout without panicking on `EPIPE` and exit with `code`
/// (or `1` on non-pipe write errors).
fn answer_and_exit(text: &str, code: i32) -> ! {
    let written = aterm_uds::stdout::write_all(cli_text::for_stdout(text).as_bytes());
    std::process::exit(
        aterm_uds::stdout::answer_status("aterm", written, code).unwrap_or_else(|line| {
            eprintln!("{line}");
            1
        }),
    )
}

/// Dependency-free argument parser, the effectful shell around
/// [`decide_args`]: prints help/version and exits 0, prints a usage error and
/// exits 2, or records the launch flags ([`SessionFlags`]) and returns.
///
/// With no args (a Finder/.app launch) a normal interactive shell starts.
/// Returns whether `-q`/`--quiet` was passed.
pub fn parse_args(argv: Vec<std::ffi::OsString>) -> bool {
    // Lossy conversion (`std::env::args` panics on non-UTF-8): such an operand
    // is rejected as an unknown option with a clean exit 2.
    match decide_args(argv.into_iter().map(|a| a.to_string_lossy().into_owned())) {
        CliAction::Help => answer_and_exit(&help_text(), 0),
        CliAction::Version => answer_and_exit(
            &version_text(aterm_update::which_copy::observe().as_ref()),
            0,
        ),
        CliAction::DiagHelp { cmd } => {
            let desc = DIAG_COMMANDS
                .iter()
                .find(|(name, _)| *name == cmd)
                .map(|(_, d)| *d)
                .unwrap_or("");
            answer_and_exit(&format!("{cmd} — {desc}\nusage: {}\n", diag_usage(&cmd)), 0)
        }
        CliAction::Diag { cmd, arg } => {
            // `decide_args` only emits registry names, so `diag_report` is Some.
            match diag_report(&cmd, arg.as_deref()) {
                // Success goes to stdout; a non-zero result (e.g. doctor on a
                // broken shell) goes to stderr and sets the exit code, so it scripts.
                Some((report, 0)) => answer_and_exit(&report, 0),
                Some((report, code)) => {
                    eprint!("{}", cli_text::for_stderr(&report));
                    std::process::exit(code);
                }
                None => {
                    eprintln!("aterm: unknown subcommand {cmd} (try --help)");
                    std::process::exit(2);
                }
            }
        }
        CliAction::Manual { topic } => {
            // Inside an aterm session `aterm help` prints the agent brief (with
            // the sid); outside it prints the front page. Exit 2 on an unknown
            // topic.
            let session = manual::in_session();
            let (out, code) = manual::render(topic.as_deref(), session.as_deref());
            if code == 0 {
                answer_and_exit(&out, 0);
            }
            eprint!("{}", cli_text::for_stderr(&out));
            std::process::exit(code);
        }
        CliAction::Agents { rest } => {
            // Same print/exit discipline as the diag path: success to stdout, a
            // failure/usage report to stderr with its code, so it scripts.
            let Some(home) = aterm_primer::home_dir() else {
                let (var, shown) = if cfg!(windows) {
                    ("USERPROFILE", "%USERPROFILE%")
                } else {
                    ("HOME", "$HOME")
                };
                let state = if std::env::var_os(var).is_none() {
                    "not set"
                } else {
                    "empty"
                };
                eprintln!("aterm agents: {shown} is {state}");
                std::process::exit(1);
            };
            let (out, code) = aterm_primer::agents_report(&home, &rest);
            if code == 0 {
                answer_and_exit(&out, 0);
            }
            eprint!("{}", cli_text::for_stderr(&out));
            std::process::exit(code);
        }
        CliAction::Usage(msg) => {
            eprintln!("{}", cli_text::for_stderr(&msg));
            std::process::exit(2);
        }
        CliAction::Run {
            containment,
            quiet,
            verbose,
        } => {
            let _ = SESSION_FLAGS.set(SessionFlags {
                containment,
                verbose,
            });
            quiet
        }
    }
}

/// The session's launch flags, recorded by [`parse_args`] and read by
/// [`session_main`]. Never exported, so a child inherits nothing from them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionFlags {
    /// `--containment <mode>` / `--sandbox` / `--no-sandbox` (the last one), as typed.
    pub containment: Option<String>,
    /// `--verbose`.
    pub verbose: bool,
}

static SESSION_FLAGS: std::sync::OnceLock<SessionFlags> = std::sync::OnceLock::new();

/// This process's session flags (the defaults when [`parse_args`] recorded none).
fn session_flags() -> SessionFlags {
    SESSION_FLAGS.get().cloned().unwrap_or_default()
}

/// The `("PATH", value)` pair that puts `dir` first on the child's PATH;
/// `None` when already present. Mirrors aterm-gui's bundle-dir arm of
/// `spawn::reroute_path_env`; the separator is `;` on Windows.
fn prepend_path(dir: &str, inherited: Option<&str>) -> Option<(String, String)> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    match inherited {
        Some(p) if p.split(sep).any(|c| c == dir) => None,
        Some(p) if !p.is_empty() => Some(("PATH".to_string(), format!("{dir}{sep}{p}"))),
        _ => Some(("PATH".to_string(), dir.to_string())),
    }
}

/// aterm's own binary directory, handed to the child shell so `aterm` and every
/// `aterm <verb>` resolve even when launched by absolute path. Only for a real
/// install, identified by the `aterm-ctl` argv0 alias beside the binary; a lone
/// binary (e.g. in ~/Downloads) yields `None`.
fn front_door_bin_dir() -> Option<String> {
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    let dir = exe.parent()?;
    if !dir
        .join(format!("aterm-ctl{}", std::env::consts::EXE_SUFFIX))
        .exists()
    {
        return None;
    }
    dir.to_str().map(str::to_owned)
}

/// `atpkg::reroute::REROUTE_DIR_ENV` and `PASSTHROUGH_ENV`, restated (this
/// crate links `atpkg` for tests only); `reroute_env_names_match_atpkg` pins
/// both. `PASSTHROUGH_ENV` is internal protocol: the marker the front door sets
/// for `aterm --no-reroute` and clears otherwise.
const REROUTE_DIR_ENV: &str = "ATERM_REROUTE_DIR";
const PASSTHROUGH_ENV: &str = "__ATERM_REROUTE_PASSTHROUGH";

/// The session's reroute directory from `$ATERM_REROUTE_DIR`, which the front
/// door (`crates/aterm/src/main.rs`) resolves, lays stubs into and exports
/// before `session_main` runs.
///
/// `None` when the `--no-reroute` marker is engaged (`env_flag_engaged`), when
/// unset or empty, when it names no existing directory (Windows lays no stubs),
/// or when relative (the front door always hands an absolute path; a relative
/// stray would resolve against every later `cd`).
fn reroute_dir_from_env() -> Option<String> {
    reroute_dir_from_values(
        std::env::var(PASSTHROUGH_ENV).ok().as_deref(),
        std::env::var(REROUTE_DIR_ENV).ok().as_deref(),
    )
}

/// [`reroute_dir_from_env`] over explicit values, so the rule is testable without a
/// process environment: `no_reroute` is `$__ATERM_REROUTE_PASSTHROUGH`, `dir` is
/// `$ATERM_REROUTE_DIR`.
fn reroute_dir_from_values(no_reroute: Option<&str>, dir: Option<&str>) -> Option<String> {
    if aterm_types::control_socket::env_flag_engaged(no_reroute) {
        return None;
    }
    dir.filter(|dir| is_absolute_existing_dir(dir))
        .map(str::to_owned)
}

/// Whether `dir` is a non-empty absolute path naming an existing directory — the one
/// shape either managed directory may have before it goes first on a child's PATH.
fn is_absolute_existing_dir(dir: &str) -> bool {
    let path = std::path::Path::new(dir);
    !dir.is_empty() && path.is_absolute() && path.is_dir()
}

/// [`is_absolute_existing_dir`] and not a symlink, the shape of the front
/// door's `$ATERM_AGENTS_DIR` handoff.
fn is_absolute_real_dir(dir: &str) -> bool {
    is_absolute_existing_dir(dir)
        && std::fs::symlink_metadata(dir).is_ok_and(|md| !md.file_type().is_symlink())
}

/// `ATPKG_AGENTS`, atpkg's spelling restated (pinned by
/// `reroute_env_names_match_atpkg`): the managed `<prefix>/agents/` as an
/// enclosing shell that sourced the hook names it. Read, never exported (the
/// shell integration sources the hook only while it is unset).
const HOOK_AGENTS_ENV: &str = "ATPKG_AGENTS";

/// `atpkg::reroute::AGENTS_DIR_ENV` restated (pinned by
/// `reroute_env_names_match_atpkg`): the managed `<prefix>/agents/` as the
/// front door hands it on every lane, absolute and ensured to exist. Absent or
/// empty means none. Re-exported so a nested session reads the same.
const AGENTS_DIR_ENV: &str = "ATERM_AGENTS_DIR";

/// Resolve the managed `agents/` directory for a TTY session:
///
/// 1. `handed` (`$ATERM_AGENTS_DIR` from the front door) when it is an absolute
///    existing real directory;
/// 2. `<prefix>/agents` alongside `reroute_dir` (`$ATERM_REROUTE_DIR`), ensured
///    via [`ensure_agents_dir`];
/// 3. `enclosing_agents` (`$ATPKG_AGENTS` from an enclosing hook-sourced shell)
///    when it is an absolute existing directory.
fn managed_agents_dir(
    handed: Option<&str>,
    reroute_dir: Option<&str>,
    enclosing_agents: Option<&str>,
) -> Option<String> {
    if let Some(dir) = handed.filter(|dir| is_absolute_real_dir(dir)) {
        return Some(dir.to_owned());
    }
    let derived = reroute_dir
        .map(std::path::Path::new)
        .filter(|reroute| reroute.is_absolute())
        .and_then(|reroute| {
            let prefix = reroute.parent()?;
            let dir = prefix.join("agents");
            if ensure_agents_dir(prefix, &dir).is_err() {
                return None;
            }
            dir.to_str().map(str::to_owned)
        });
    derived.or_else(|| {
        enclosing_agents
            .filter(|dir| is_absolute_existing_dir(dir))
            .map(str::to_owned)
    })
}

/// The mode for the managed `agents/` directory, restating
/// `atpkg::store::Layout::ensure_dir`'s rule: a root-owned prefix that is not
/// group/other-writable (`platform::dir_meta_is_system`) ⇒ `0755`; a prefix we
/// own ⇒ `0700`; any other owner ⇒ `None` (refuse). Pure.
#[cfg(any(unix, test))]
fn agents_dir_mode(prefix_uid: u32, prefix_mode: u32, our_uid: u32) -> Option<u32> {
    if prefix_uid == 0 && prefix_mode & 0o022 == 0 {
        Some(0o755)
    } else if prefix_uid == our_uid {
        Some(0o700)
    } else {
        None
    }
}

/// Ensure `dir` (`<prefix>/agents`) exists as a real directory: an existing one
/// is left untouched; a symlink or non-directory is refused; otherwise one
/// `mkdir` with [`agents_dir_mode`]'s mode, made exact after the umask, with an
/// `AlreadyExists` race tolerated. The `Err` is the sentence for stderr.
#[cfg(unix)]
fn ensure_agents_dir(prefix: &std::path::Path, dir: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    let existing = |md: std::fs::Metadata| -> Result<(), String> {
        if md.file_type().is_symlink() {
            Err(format!("{} is a symlink", dir.display()))
        } else if md.is_dir() {
            Ok(())
        } else {
            Err(format!("{} exists and is not a directory", dir.display()))
        }
    };
    if let Ok(md) = std::fs::symlink_metadata(dir) {
        return existing(md);
    }
    let prefix_meta =
        std::fs::metadata(prefix).map_err(|error| format!("{}: {error}", prefix.display()))?;
    // SAFETY: getuid() takes no arguments and cannot fail.
    let our_uid = unsafe { libc::getuid() };
    let mode = agents_dir_mode(prefix_meta.uid(), prefix_meta.mode() & 0o7777, our_uid)
        .ok_or_else(|| {
            format!(
                "{} is owned by uid {}, not by this user (uid {our_uid}) and not by root",
                prefix.display(),
                prefix_meta.uid()
            )
        })?;
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(mode);
    match builder.create(dir) {
        Ok(()) => {
            if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.mode() & 0o7777 == mode) {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode))
                    .map_err(|error| format!("{}: {error}", dir.display()))?;
            }
            Ok(())
        }
        Err(error) => match std::fs::symlink_metadata(dir) {
            // Lost the race to another session or an atpkg pass: judge what is there.
            Ok(md) => existing(md),
            Err(_) => Err(format!("{}: {error}", dir.display())),
        },
    }
}

/// [`ensure_agents_dir`] where no reroute handle is ever handed (Windows lays no stubs, so
/// this is unreached in practice): a plain `create_dir`, the symlink refusal kept.
#[cfg(not(unix))]
fn ensure_agents_dir(_prefix: &std::path::Path, dir: &std::path::Path) -> Result<(), String> {
    match std::fs::symlink_metadata(dir) {
        Ok(md) if md.file_type().is_symlink() => Err(format!("{} is a symlink", dir.display())),
        Ok(md) if md.is_dir() => Ok(()),
        Ok(_) => Err(format!("{} exists and is not a directory", dir.display())),
        Err(_) => std::fs::create_dir(dir)
            .or_else(|error| if dir.is_dir() { Ok(()) } else { Err(error) })
            .map_err(|error| format!("{}: {error}", dir.display())),
    }
}

/// The one `("PATH", value)` pair the session hands its shell (the pty seam's
/// `build_child_env` overwrites by key). Order, mirroring aterm-gui's
/// `spawn::reroute_path_env`: `reroute_dir` then `agents_dir`, each moved to
/// the front (earlier occurrences removed); then `front_door_dir` via
/// [`prepend_path`] (skip-if-present); then the inherited PATH. `None` when
/// nothing is injected.
fn session_path_env(
    reroute_dir: Option<&str>,
    agents_dir: Option<&str>,
    front_door_dir: Option<&str>,
    inherited: Option<&str>,
) -> Option<(String, String)> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let moved: Vec<&str> = [reroute_dir, agents_dir].into_iter().flatten().collect();
    if moved.is_empty() {
        return front_door_dir.and_then(|dir| prepend_path(dir, inherited));
    }
    let front = front_door_dir
        .and_then(|dir| prepend_path(dir, inherited))
        .map(|(_, value)| value);
    let base = front.as_deref().or(inherited).filter(|p| !p.is_empty());
    let mut entries: Vec<&str> = moved.clone();
    entries.extend(
        base.into_iter()
            .flat_map(|p| p.split(sep))
            .filter(|entry| !moved.contains(entry)),
    );
    Some(("PATH".to_string(), entries.join(&sep.to_string())))
}

/// Whether `first` (the `argv[1]` operand) should be considered for toolchain
/// dispatch. False for no operand, an empty token, a flag, any front-door verb
/// ([`Verb::from_operand`]), `help`, and any diagnostic subcommand, so a
/// co-distributed tool can never shadow aterm's own surface. `true` means only
/// "candidate"; the package dispatcher decides whether it is installed. Pure.
pub fn is_tool_candidate(first: Option<&str>) -> bool {
    match first {
        None => false,
        Some(w) => {
            !w.is_empty()
                && !w.starts_with('-')
                && w != "help"
                && Verb::from_operand(w).is_none()
                && !DIAG_COMMANDS.iter().any(|(name, _)| *name == w)
        }
    }
}

/// Map a containment mode to the [`aterm_sandbox::Limits`] handed to the spawn seam:
/// `User` inherits the parent's limits (`no job caps` on Windows), `Safety` applies
/// soft caps ([`aterm_sandbox::Limits::shell_default`]), and `Containment` applies
/// hard caps ([`aterm_sandbox::Limits::contained`]).
fn session_limits(mode: aterm_containment::ContainmentMode) -> aterm_sandbox::Limits {
    use aterm_containment::ContainmentMode as Cm;
    match mode {
        Cm::User => aterm_sandbox::Limits::inherit(),
        Cm::Safety => aterm_sandbox::Limits::shell_default(),
        _ => aterm_sandbox::Limits::contained(),
    }
}

/// Run the transparent PTY session: containment init, root-authority mint,
/// protected shell spawn, and passthrough loop. Never returns.
pub fn session_main(quiet: bool) -> ! {
    let (rows, cols) = driver::host_winsize();

    let flags = session_flags();
    let mode = aterm_containment::init_mode_from_flag(
        flags.containment.as_deref(),
        aterm_containment::ContainmentMode::User,
    )
    .unwrap_or_else(|e| {
        if matches!(e, aterm_containment::InitModeFromFlagError::Parse(_)) {
            eprintln!("aterm: --containment takes user, safety or containment; using containment");
        } else {
            eprintln!("aterm: {e}; using containment");
        }
        let _ = aterm_containment::init_mode(aterm_containment::ContainmentMode::Containment);
        aterm_containment::ContainmentMode::Containment
    });

    let sandbox_wrap: Option<String> = match aterm_containment::decide_spawn(mode) {
        aterm_containment::SpawnDecision::Permit { sbpl, .. } => sbpl,
        aterm_containment::SpawnDecision::Deny { reason, .. } => {
            eprintln!("aterm: {reason}");
            std::process::exit(1);
        }
        _ => {
            eprintln!("aterm: containment mode {mode} denies spawning a shell (fail-closed)");
            std::process::exit(1);
        }
    };

    // SAFETY: trusted process entry point, reached once before spawning or reading PTY input.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

    let reroute_dir = reroute_dir_from_env();
    let agents_dir = managed_agents_dir(
        std::env::var(AGENTS_DIR_ENV).ok().as_deref(),
        reroute_dir.as_deref(),
        std::env::var(HOOK_AGENTS_ENV).ok().as_deref(),
    );
    let inherited = std::env::var("PATH").ok();
    let path = session_path_env(
        reroute_dir.as_deref(),
        agents_dir.as_deref(),
        front_door_bin_dir().as_deref(),
        inherited.as_deref(),
    );
    let path = aterm_types::cli_path::selected_path(
        aterm_types::dirs::home_dir().as_deref(),
        reroute_dir.as_deref(),
        agents_dir.as_deref(),
        path.as_ref()
            .map(|(_, value)| value.as_str())
            .or(inherited.as_deref()),
    )
    .or(path);
    let mut env_add: Vec<(String, String)> = path.into_iter().collect();
    if let Some(dir) = &reroute_dir {
        env_add.push((REROUTE_DIR_ENV.to_string(), dir.clone()));
    } else if std::env::var_os(REROUTE_DIR_ENV).is_some() {
        env_add.push((REROUTE_DIR_ENV.to_string(), String::new()));
    }
    if let Some(dir) = &agents_dir {
        env_add.push((AGENTS_DIR_ENV.to_string(), dir.clone()));
    } else if std::env::var_os(AGENTS_DIR_ENV).is_some() {
        env_add.push((AGENTS_DIR_ENV.to_string(), String::new()));
    }
    // `ATERM_CHILD` is omitted here so `fleet_listen`'s root-only nesting guard stays clear.
    env_add.push(("AGY_CLI_FAST_PROMPT_EDITOR".to_string(), "true".to_string()));

    let shell = aterm_pty::spawn_shell_with_pid(
        rows,
        cols,
        &spawn_cap,
        &sandbox_cap,
        &env_add,
        None,
        None,
        None,
        None,
        None,
        sandbox_wrap.as_deref(),
        session_limits(mode),
    )
    .unwrap_or_else(|e| {
        eprintln!("aterm: could not start the shell: {e}");
        std::process::exit(1);
    });

    {
        use std::io::IsTerminal as _;
        if !quiet && std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
            eprintln!(
                "aterm {} — session started · start with `aterm help` · `-q` hides this line",
                aterm_types::version::app_version()
            );
        }
    }

    let code = driver::run(shell, flags.verbose);
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    #[cfg(any(windows, target_os = "macos"))]
    use super::doctor_report;
    use super::{
        AGENTS_DIR_ENV, CliAction, DEBUGGER_ACCESS_REVERT, DIAG_COMMANDS, DebuggerFacts,
        DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, DevtoolsFacts, DrClass,
        ExceptionsFacts, FdaState, HOOK_AGENTS_ENV, MachineFacts, Mark, PASSTHROUGH_ENV,
        PrivacyFacts, ProbeLabel, REROUTE_DIR_ENV, RecoveryFacts, SelfHardening, ShellCheck, Verb,
        agents_dir_mode, config_conpty_from_text, decide_args, diag_report, doctor_checks,
        doctor_checks_with_origin, exceptions_report_at, explain_config_report, help_text,
        is_tool_candidate, list_fonts_report, list_themes_alias, managed_agents_dir,
        parse_zone_offset, prepend_path, read_exceptions_log, reroute_dir_from_values,
        session_limits, session_path_env, show_face_report, version_text,
    };
    // The Windows-only tests read the window's shell and the Windows paragraphs.
    use super::driver;
    #[cfg(windows)]
    use super::show_config_report;
    #[cfg(unix)]
    use super::{ARGS_WITHHELD, args_read_withheld, fabric_mark, machine_facts, unix_shell};

    fn decide(args: &[&str]) -> CliAction {
        decide_args(args.iter().map(|s| s.to_string()))
    }

    fn unmeasured() -> PrivacyFacts {
        PrivacyFacts::not_measured(ProbeLabel::RefusedOutOfBundle)
    }

    fn no_debugger() -> DebuggerFacts {
        DebuggerFacts::not_measured(DebuggerProbeLabel::RefusedOutOfBundle)
    }

    fn no_devtools() -> DevtoolsFacts {
        DevtoolsFacts::absent()
    }

    fn debugger(
        mode: DebuggerMode,
        member: DebuggerMember,
        hardening: SelfHardening,
    ) -> DebuggerFacts {
        DebuggerFacts {
            probe: DebuggerProbe {
                mode,
                member,
                label: DebuggerProbeLabel::StatusOk,
            },
            hardening,
        }
    }

    fn no_recovery() -> RecoveryFacts {
        Ok((
            Vec::new(),
            std::path::PathBuf::from("/logs/recovery-ledger.log"),
        ))
    }

    fn no_exceptions() -> ExceptionsFacts {
        Ok(0)
    }

    fn no_census() -> Result<aterm_ctl::census::Census, String> {
        Ok(aterm_ctl::census::Census {
            dir: "/run/aterm".to_string(),
            instances: 0,
            unreached: Vec::new(),
        })
    }

    #[test]
    fn doctor_readiness_keeps_unmeasured_notes_unknown_and_advisories_nonfatal() {
        use aterm_converge::{Outcome, Readiness};
        let census = Err("process table unavailable".to_owned());
        let exceptions = Err("log unreadable".to_owned());
        let machine = MachineFacts {
            spotlight: Some(Err("index unavailable".to_owned())),
            program_check: Some(aterm_uds::program_check::Look::Unread),
            fabric: Some((Mark::Note, "withheld".to_owned(), true)),
            ..MachineFacts::absent()
        };
        let gathered = super::doctor::Gathered::of_rows(super::doctor_rows(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &no_devtools(),
            &census,
            &Err("ledger unreadable".to_owned()),
            &exceptions,
            &machine,
        ));
        let all = gathered.plan();
        for id in [
            "base.privacy.fda",
            "base.privacy.debugger",
            "base.session.instances",
            "base.session.exceptions",
            "base.machine.spotlight",
            "base.machine.syspolicyd",
            "base.session.fabric",
            "base.session.recovery",
        ] {
            if cfg!(windows) && id == "base.session.recovery" {
                continue;
            }
            let rows: Vec<_> = all
                .iter()
                .filter(|r| r.id == id || r.id == super::doctor::SHELL)
                .cloned()
                .collect();
            assert_eq!(
                rows.iter().find(|r| r.id == id).unwrap().outcome,
                Outcome::Unknown,
                "{id}"
            );
            let readiness = Readiness::of(&rows, &[id.to_owned()]);
            assert!(!readiness.all_green, "{id}");
            assert_eq!(readiness.unresolved, [id]);
            let text = super::doctor::render_readiness(&gathered, &rows, &[], &readiness);
            assert!(text.contains("readiness: NOT READY"), "{id}: {text}");
        }

        // Known optional choices and facts for later retain their note semantics.
        let gathered = super::doctor::Gathered::of_rows(super::doctor_rows(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &measured(FdaState::Denied, DrClass::Identity, "aterm"),
            &debugger(
                DebuggerMode::Disabled,
                DebuggerMember::Admin,
                SelfHardening::Hardened,
            ),
            &no_devtools(),
            &no_census(),
            &no_recovery(),
            &Ok(2),
            &MachineFacts::absent(),
        ));
        let rows = gathered.plan();
        assert!(Readiness::of(&rows, &[]).all_green, "{rows:?}");
    }

    fn measured(fda: FdaState, dr: DrClass, who: &str) -> PrivacyFacts {
        PrivacyFacts {
            fda,
            fda_label: match fda {
                FdaState::Granted => ProbeLabel::OpenOk,
                FdaState::Denied => ProbeLabel::OpenEperm,
                FdaState::Unknown => ProbeLabel::OpenErrno(2),
            },
            dr,
            responsible: Some(who.to_string()),
        }
    }

    fn diag(cmd: &str, arg: Option<&str>) -> CliAction {
        CliAction::Diag {
            cmd: cmd.to_string(),
            arg: arg.map(str::to_string),
        }
    }

    /// Diagnostics reject unknown flags (e.g. `aterm show-config --json`)
    /// instead of ignoring them at exit 0.
    #[test]
    fn a_diag_subcommand_refuses_an_argument_it_would_have_swallowed() {
        for cmd in [
            "show-config",
            "explain-config",
            "list-fonts",
            "list-kitty-commands",
        ] {
            match decide(&[cmd, "--json"]) {
                CliAction::Usage(msg) => {
                    assert!(
                        msg.contains(cmd) && msg.contains("--json") && msg.contains("takes none"),
                        "the refusal must name the verb, the argument, and the \
                         arity: {msg}"
                    );
                }
                other => panic!("`aterm {cmd} --json` must be a usage error, got {other:?}"),
            }
            // The bare form is untouched.
            assert_eq!(decide(&[cmd]), diag(cmd, None), "bare `{cmd}` still runs");
        }
    }

    /// `<diag> --help` describes the subcommand instead of running it (so `-h`
    /// is never taken as a `show-face` family).
    #[test]
    fn a_diag_help_request_describes_and_never_runs() {
        for argv in [
            vec!["doctor", "--help"],
            vec!["show-config", "-h"],
            vec!["show-face", "--help"],
            vec!["show-face", "-h"],
        ] {
            match decide(&argv) {
                CliAction::DiagHelp { cmd } => assert_eq!(cmd, argv[0]),
                other => panic!("{argv:?} must be a help request, got {other:?}"),
            }
        }
    }

    /// The diagnostic edge passes doctor's full grammar to the typed parser.
    #[test]
    fn doctor_operand_grammar_is_enforced_at_the_edge() {
        const USAGE: &str = "usage: aterm doctor [--read-only | --check | --apply | --restore] [--json] [--only <id,...>]";
        assert_eq!(decide(&["doctor"]), diag("doctor", None));
        assert_eq!(
            decide(&["doctor", "--read-only"]),
            diag("doctor", Some("--read-only"))
        );
        assert_eq!(
            decide(&["doctor", "--json", "--only", "pkg"]),
            diag("doctor", Some("--json --only pkg"))
        );
        assert_eq!(
            decide(&["doctor", "--check", "--json"]),
            diag("doctor", Some("--check --json"))
        );
        for argv in [
            vec!["doctor", "--fix"],
            vec!["doctor", "--read-only", "x"],
            vec!["doctor", "--json", "--json"],
            vec!["doctor", "--check", "--apply"],
            vec!["doctor", "--apply", "--only", "pkg"],
        ] {
            match decide(&argv) {
                CliAction::Usage(msg) => assert!(msg.contains(USAGE), "{argv:?}: {msg}"),
                other => panic!("{argv:?} must be a usage error, got {other:?}"),
            }
        }
        // `aterm doctor --help` states the same grammar as the refusal.
        assert_eq!(format!("usage: {}", super::diag_usage("doctor")), USAGE);
        assert_eq!(super::diag_usage("exceptions"), "aterm exceptions [--all]");
        assert_eq!(super::diag_usage("show-face"), "aterm show-face <family>");
        assert_eq!(super::diag_usage("list-fonts"), "aterm list-fonts");
    }

    /// `aterm exceptions` takes one optional flag, `--all`; anything else is a
    /// usage error naming the grammar, never silently dropped.
    #[test]
    fn exceptions_operand_grammar_is_enforced_at_the_edge() {
        assert_eq!(decide(&["exceptions"]), diag("exceptions", None));
        assert_eq!(
            decide(&["exceptions", "--all"]),
            diag("exceptions", Some("--all"))
        );
        assert_eq!(
            decide(&["exceptions", "--help"]),
            CliAction::DiagHelp {
                cmd: "exceptions".to_string()
            }
        );
        for argv in [
            vec!["exceptions", "--json"],
            vec!["exceptions", "--all", "extra"],
            vec!["exceptions", "all"],
        ] {
            match decide(&argv) {
                CliAction::Usage(msg) => assert!(
                    msg.contains("aterm exceptions [--all]"),
                    "the refusal must state the grammar: {msg}"
                ),
                other => panic!("{argv:?} must be a usage error, got {other:?}"),
            }
        }
    }

    /// `date +%z` output is read as seconds east of UTC, and only that shape.
    #[test]
    fn the_zone_offset_reads_date_plus_z() {
        assert_eq!(parse_zone_offset("+0100"), Some(3_600));
        assert_eq!(parse_zone_offset("-0530"), Some(-19_800));
        assert_eq!(parse_zone_offset("+0000"), Some(0));
        for bad in ["", "0100", "+01:00", "+2500", "+0160", "UTC", "+01000"] {
            assert_eq!(parse_zone_offset(bad), None, "{bad:?}");
        }
    }

    /// A scratch folder for the reader's files, removed when dropped.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "aterm-exceptions-reader-{}-{}",
                std::process::id(),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn log(&self) -> std::path::PathBuf {
            self.0.join("messages.log")
        }

        fn rotated(&self) -> std::path::PathBuf {
            self.0.join("messages.log.1")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The lines a real center writes for two exceptions and one dismissal, from
    /// the engine's own encoder: `(posted x2, retired x1)` for the first batch and
    /// `retired` for the second, so a test can put them in either file.
    fn exception_lines() -> Vec<String> {
        use aterm_messages::need::{Fingerprint, Reason, Resurface, Then, Why};
        use aterm_messages::{
            Instant, Message, MessageCenter, MessageLog, Severity, WallStamp, tags,
        };
        const T0: u64 = 1_790_000_000_000;
        let now = Instant::now();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        let ask = |stem: &str, cause: &str, why: &str, then: &str| {
            aterm_messages::need::LaterAsk::person(
                Why::Question(Reason::new(why)),
                Then::step(then),
                Fingerprint::new(stem, "", cause),
                Resurface::CHOICE,
            )
        };
        c.post(
            Message::new(
                tags::PRIVACY,
                Severity::Info,
                "macOS keeps asking for access",
            )
            .later(ask(
                "privacy",
                "fda",
                "macOS forgets an Allow when aterm updates",
                "Turn on aterm in Full Disk Access",
            )),
            WallStamp { unix_ms: T0 },
            now,
        );
        c.post(
            Message::new(tags::UPDATE, Severity::Warn, "An older thing").later(ask(
                "update",
                "old",
                "the old thing needs you",
                "Do the old thing",
            )),
            WallStamp {
                unix_ms: T0 + 1_000,
            },
            now,
        );
        let mut lines: Vec<String> = c
            .drain_shelved_for_persist()
            .iter()
            .map(|(l, _)| l.encode())
            .collect();
        c.file_digest(
            WallStamp {
                unix_ms: T0 + 60_000,
            },
            now,
        );
        // Reopen one: a NEW fact after the filing stays open in the log.
        c.post(
            Message::new(tags::UPDATE, Severity::Warn, "Updates keep failing").later(ask(
                "update",
                "failing",
                "the updater has failed for a day",
                "Open Settings and check the update lane",
            )),
            WallStamp {
                unix_ms: T0 + 120_000,
            },
            now,
        );
        lines.extend(
            c.drain_shelved_for_persist()
                .iter()
                .map(|(l, _)| l.encode()),
        );
        lines
    }

    /// Replays `messages.log` (and its rotated `.1` pair) without a window running,
    /// rendering open and filed entries while counting torn lines.
    #[test]
    fn the_exceptions_reader_replays_the_log_without_a_window() {
        let dir = Scratch::new();
        // No file: said, not an error and not an empty list that looks like health.
        let none = exceptions_report_at(&dir.log(), false, 1, 0);
        assert!(
            none.starts_with("exceptions: none (no log yet at "),
            "{none}"
        );

        let lines = exception_lines();
        // The rotated pair holds the older lines (both posts and the batch's
        // dismissals sit in the current file: a `posted` in `.1` meets its
        // `retired` in the current one); a torn line sits between.
        let posted: Vec<&String> = lines.iter().filter(|l| l.contains("kind=posted")).collect();
        let rest: Vec<&String> = lines
            .iter()
            .filter(|l| !l.contains("kind=posted"))
            .collect();
        std::fs::write(dir.rotated(), format!("{}\n{}\n", posted[0], posted[1])).unwrap();
        std::fs::write(
            dir.log(),
            format!(
                "m1\tkind=posted\tid=9\n{}\n{}\n",
                rest.iter()
                    .map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                posted[2]
            ),
        )
        .unwrap();

        const T0: u64 = 1_790_000_000_000;
        // One hour after the filing, on a reader one hour east of UTC.
        let report = exceptions_report_at(&dir.log(), false, T0 + 3_600_000, 3_600);
        assert!(report.contains("To look at (1)"), "{report}");
        assert!(report.contains("Updates keep failing"), "{report}");
        assert!(
            report.contains("why    the updater has failed for a day"),
            "{report}"
        );
        assert!(
            report.contains("then   Open Settings and check the update lane"),
            "{report}"
        );
        assert!(report.contains("Filed (2)"), "{report}");
        assert!(report.contains("macOS keeps asking for access"), "{report}");
        assert!(report.contains("dismissed"), "{report}");
        assert!(
            report.contains("(1 line(s) of the log could not be read)"),
            "{report}"
        );
        // The open entry comes before the filed ones.
        assert!(
            report.find("To look at").unwrap() < report.find("Filed (").unwrap(),
            "{report}"
        );
        // `--all` says the same here (two filed entries are under the cap).
        let all = exceptions_report_at(&dir.log(), true, T0 + 3_600_000, 3_600);
        assert!(all.contains("Filed (2)"), "{all}");
        assert!(!all.contains("--all lists every filed entry"), "{all}");

        // The doctor's count is the open ones.
        let read = read_exceptions_log(&dir.log());
        assert_eq!(
            read.log
                .records()
                .filter(|r| r.later && r.is_live())
                .count(),
            1
        );
    }

    /// A log is never worth a symlink: a `messages.log` that points elsewhere is not
    /// followed and is said as unreadable (never "does not exist"), and a directory in
    /// its place likewise, even when the rotated copy reads.
    #[cfg(unix)]
    #[test]
    fn the_exceptions_reader_never_follows_a_symlink() {
        let dir = Scratch::new();
        let real = dir.0.join("elsewhere.log");
        std::fs::write(&real, format!("{}\n", exception_lines().join("\n"))).unwrap();
        std::os::unix::fs::symlink(&real, dir.log()).unwrap();
        let out = exceptions_report_at(&dir.log(), false, 1, 0);
        assert!(out.contains("is a symlink or folder, not the log"), "{out}");
        // A directory in its place likewise, and the doctor does not call it "none open".
        let other = Scratch::new();
        std::fs::create_dir(other.log()).unwrap();
        let out = exceptions_report_at(&other.log(), false, 1, 0);
        assert!(out.contains("is a symlink or folder, not the log"), "{out}");
        assert!(read_exceptions_log(&other.log()).unreadable.is_some());
        // Only the rotated copy there, as a folder: the report names IT, not the
        // missing current file.
        let rotated = Scratch::new();
        let older = rotated.0.join("messages.log.1");
        std::fs::create_dir(&older).unwrap();
        let out = exceptions_report_at(&rotated.log(), false, 1, 0);
        assert!(
            out.contains(&format!("{} is a symlink", older.display())),
            "{out}"
        );
        let unreadable = read_exceptions_log(&rotated.log()).unreadable.unwrap();
        assert_eq!(unreadable.name(), "messages.log.1");
        // The rotated copy reads, and a folder stands at the current file: the
        // report names the current file, never the older copy's entries as what
        // is open (their retirements may be in the file it could not read).
        let stale = Scratch::new();
        let posted: Vec<String> = exception_lines()
            .into_iter()
            .filter(|l| l.contains("kind=posted"))
            .collect();
        std::fs::write(stale.rotated(), format!("{}\n", posted.join("\n"))).unwrap();
        std::fs::create_dir(stale.log()).unwrap();
        let out = exceptions_report_at(&stale.log(), false, 1, 0);
        assert!(!out.starts_with("exceptions in "), "{out}");
        let unreadable = read_exceptions_log(&stale.log()).unreadable.unwrap();
        assert_eq!(unreadable.name(), "messages.log");
        assert!(unreadable.not_a_file);
    }

    /// The `exceptions:` row: OK with nothing open, a NOTE (never a FAIL, so a waiting
    /// entry cannot make `aterm doctor && aterm` refuse to launch) naming the command
    /// when something is, and "not measured" with no log folder.
    #[test]
    fn the_doctor_exceptions_row_counts_the_open_entries_and_never_fails() {
        let row = |facts: ExceptionsFacts| {
            let (r, code) = doctor_checks(
                "/bin/sh",
                ShellCheck::Runs,
                &unmeasured(),
                &no_debugger(),
                &no_census(),
                &no_recovery(),
                &facts,
            );
            assert_eq!(code, 0, "{r}");
            r
        };
        let none = row(Ok(0));
        assert!(
            none.lines().any(|l| l == "exceptions: ok    none open"),
            "{none}"
        );
        let two = row(Ok(2));
        assert!(
            two.lines()
                .any(|l| l == "exceptions: note  2 open (aterm exceptions)"),
            "{two}"
        );
        let homeless = row(Err("no log folder".to_string()));
        assert!(
            homeless
                .lines()
                .any(|l| l == "exceptions: note  not measured (no log folder)"),
            "{homeless}"
        );
    }

    /// `show-face` keeps its one operand; a flag-shaped or surplus operand is a
    /// usage error naming the grammar, never a family lookup.
    #[test]
    fn show_face_operand_grammar_is_enforced_at_the_edge() {
        assert_eq!(
            decide(&["show-face", "Menlo"]),
            diag("show-face", Some("Menlo"))
        );
        for argv in [
            vec!["show-face"],
            vec!["show-face", "--verbose"],
            vec!["show-face", "Menlo", "extra"],
        ] {
            match decide(&argv) {
                CliAction::Usage(msg) => assert!(
                    msg.contains("show-face <family>"),
                    "the refusal must state the grammar: {msg}"
                ),
                other => panic!("{argv:?} must be a usage error, got {other:?}"),
            }
        }
    }

    /// [`Verb::is_windowing`] is exactly the set the windowing grammar parses.
    /// The argv0-alias dispatch keys on it, so drift would break the verbs
    /// under `aterm-gui.exe` (the Windows shortcut's target).
    #[test]
    fn the_windowing_verbs_are_exactly_the_ones_the_grammar_parses() {
        for verb in Verb::ALL {
            let parses =
                super::parse_window_request(verb.name(), &[], |raw| Ok(raw.to_string())).is_ok();
            assert_eq!(
                verb.is_windowing(),
                parses,
                "`{}`: is_windowing() must agree with parse_window_request",
                verb.name()
            );
        }
        assert_eq!(
            Verb::ALL.iter().filter(|v| v.is_windowing()).count(),
            3,
            "new-tab, new-window, split-pane"
        );
    }

    /// Every registry command is both advertised in `--help` and dispatchable.
    #[test]
    fn diag_commands_advertised_and_dispatchable() {
        assert!(!DIAG_COMMANDS.is_empty(), "registry must not be empty");
        for (name, _desc) in DIAG_COMMANDS {
            // Advertised as its own SUBCOMMANDS line (leading token), so a name
            // that is a substring of another cannot pass vacuously.
            assert!(
                help_text()
                    .lines()
                    .any(|l| l.trim_start().starts_with(name)),
                "subcommand {name:?} is not advertised as a --help line"
            );
            // `list-themes` is dispatched as `aterm theme list`; `doctor` is composed by
            // the front door (`crates/aterm`'s doctor, whose tests run it) over this
            // crate's items ([`doctor::gather`]).
            assert!(
                diag_report(name, None).is_some()
                    || list_themes_alias(name, &[]).is_some()
                    || *name == "doctor",
                "subcommand {name:?} is in the registry but has no dispatch"
            );
            // And it must actually be recognized by the parser (`show-face`
            // needs its family operand).
            if *name == "show-face" {
                assert_eq!(decide(&[name, "Menlo"]), diag(name, Some("Menlo")));
            } else {
                assert_eq!(decide(&[name]), diag(name, None));
            }
        }
        // A non-registry name is not dispatchable.
        assert!(diag_report("definitely-not-a-command", None).is_none());
    }

    /// `aterm list-kitty-commands` prints the listener's vocabulary: every
    /// trick has an English row, and every line is one `key=value` row with no
    /// banner.
    #[test]
    fn list_kitty_commands_prints_one_row_per_line_and_every_trick() {
        let (report, code) =
            diag_report("list-kitty-commands", None).expect("the subcommand is dispatchable");
        assert_eq!(code, 0);
        assert!(report.ends_with('\n'), "the report ends its last row");
        for trick in aterm_lexicon::Trick::ALL {
            let row = format!("command trick={} lang=en gated=0 ", trick.code());
            assert!(
                report.lines().any(|l| l.starts_with(&row)),
                "no English row for trick {:?} in:\n{report}",
                trick.code()
            );
        }
        for line in report.lines() {
            let kind = line.split(' ').next().unwrap_or_default();
            assert!(
                matches!(kind, "command" | "vocative" | "filler"),
                "not a vocabulary row (a banner would break `| grep lang=`): {line:?}"
            );
            assert!(
                line.split(' ').skip(1).all(|field| field.contains('=')),
                "every field after the row kind is key=value: {line:?}"
            );
        }
        // English is the priority language: its rows come first.
        assert!(
            report
                .lines()
                .next()
                .is_some_and(|l| l.contains(" lang=en ")),
            "the first row is English"
        );
        // `--help` describes and never runs (the per-verb help law).
        assert_eq!(
            decide(&["list-kitty-commands", "--help"]),
            CliAction::DiagHelp {
                cmd: "list-kitty-commands".to_string()
            }
        );
    }

    #[test]
    fn help_word_routes_to_the_manual_and_is_advertised() {
        // `aterm help` and `aterm help <topic>` parse to Manual, dispatched before flag
        // parsing / shell spawn — the word `help`, distinct from `-h`/`--help`.
        assert_eq!(decide(&["help"]), CliAction::Manual { topic: None });
        assert_eq!(
            decide(&["help", "trust"]),
            CliAction::Manual {
                topic: Some("trust".to_string())
            }
        );
        assert_eq!(decide(&["-h"]), CliAction::Help);
        // Advertised as its own USAGE line so it never ships undocumented.
        assert!(
            help_text()
                .lines()
                .any(|l| l.trim_start().starts_with("aterm help")),
            "`aterm help` is not advertised in --help"
        );
        // aterm's own `help` must never be shadowed by a co-distributed tool named `help`.
        assert!(!is_tool_candidate(Some("help")));
    }

    #[test]
    fn agents_word_routes_to_the_primer_installer_and_is_advertised() {
        // `aterm agents …` parses like `help`: a git-style leading operand, dispatched
        // before flag parsing / shell spawn; everything after it belongs to aterm-primer.
        assert_eq!(decide(&["agents"]), CliAction::Agents { rest: vec![] });
        assert_eq!(
            decide(&["agents", "install", "claude"]),
            CliAction::Agents {
                rest: vec!["install".to_string(), "claude".to_string()]
            }
        );
        // Advertised as its own VERB line so it never ships undocumented.
        assert!(
            help_text()
                .lines()
                .any(|l| l.trim_start().starts_with("aterm agents")),
            "`aterm agents` is not advertised in --help"
        );
        // Never shadowed by a co-distributed tool named `agents`.
        assert!(!is_tool_candidate(Some("agents")));
    }

    /// The front-door roster gate: every verb is advertised, well-formed and
    /// shielded from toolchain shadowing. Dispatch is covered by the exhaustive
    /// `match Verb` in `crates/aterm/src/main.rs` and by
    /// `crates/aterm/tests/front_door_verbs.rs`.
    #[test]
    fn front_door_verbs_are_the_one_command_surface() {
        assert!(!Verb::ALL.is_empty(), "roster must not be empty");
        let help = help_text();
        for verb in Verb::ALL {
            // Advertised as its own VERB line (leading token), so a substring of
            // another name cannot pass vacuously.
            assert!(
                help.lines().any(|l| l
                    .trim_start()
                    .starts_with(&format!("aterm {}", verb.name()))),
                "verb `aterm {}` is not advertised in --help",
                verb.name()
            );
            // Shielded — a co-distributed toolchain program of the same name can
            // never shadow a verb of the one command.
            assert!(
                !is_tool_candidate(Some(verb.name())),
                "verb {:?} can be shadowed by a store tool of the same name",
                verb.name()
            );
            // Recognized — the one recognizer round-trips, so routing and
            // shielding cannot disagree about what a verb is called.
            assert_eq!(Verb::from_operand(verb.name()), Some(*verb));
            // Well-formed: a verb with no blurb would render a bare synopsis.
            assert!(
                !verb.blurb().is_empty(),
                "verb {:?} has no help",
                verb.name()
            );
        }
        // Names are distinct: `from_operand` must be unambiguous.
        for (i, verb) in Verb::ALL.iter().enumerate() {
            assert!(
                !Verb::ALL[..i].iter().any(|u| u.name() == verb.name()),
                "duplicate front-door verb {}",
                verb.name()
            );
        }
        // A non-verb resolves to nothing (the recognizer is not a prefix match).
        assert_eq!(Verb::from_operand("definitely-not-a-verb"), None);
        assert_eq!(Verb::from_operand("ct"), None);
        assert_eq!(Verb::from_operand(""), None);
    }

    #[test]
    fn verb_argv0_aliases_are_distinct_and_unambiguous() {
        let aliases: Vec<&str> = Verb::ALL.iter().filter_map(|v| v.argv0_alias()).collect();
        for (i, alias) in aliases.iter().enumerate() {
            assert!(
                !aliases[..i].contains(alias),
                "duplicate argv0 alias {alias}"
            );
            assert!(
                Verb::from_operand(alias).is_none(),
                "argv0 alias {alias} is also a verb name"
            );
        }
    }

    #[test]
    fn path_injection_prepends_once_and_is_idempotent() {
        let sep = if cfg!(windows) { ';' } else { ':' };
        let bin = "/opt/aterm/bin";
        // Prepended ahead of the inherited PATH so the co-located `aterm` resolves.
        assert_eq!(
            prepend_path(bin, Some("/usr/bin")),
            Some(("PATH".to_string(), format!("{bin}{sep}/usr/bin")))
        );
        // No inherited PATH: the bin dir alone.
        assert_eq!(
            prepend_path(bin, None),
            Some(("PATH".to_string(), bin.to_string()))
        );
        // Already on PATH (aterm-inside-aterm): inject nothing, no duplicate stacking.
        let already = format!("/usr/bin{sep}{bin}{sep}/bin");
        assert_eq!(prepend_path(bin, Some(&already)), None);
    }

    /// The composed session PATH: the reroute dir first (moved from wherever
    /// the inherited PATH listed it, exactly once), then the front door, then
    /// the rest; idempotent; with no reroute dir, exactly `prepend_path`'s
    /// answer.
    #[test]
    fn session_path_puts_the_reroute_dir_first_by_move_to_front_and_is_idempotent() {
        let sep = if cfg!(windows) { ';' } else { ':' };
        let join = |parts: &[&str]| parts.join(&sep.to_string());
        let reroute = "/Users//u/Library/Application Support/aterm/pkg/reroute";
        let bin = "/opt/aterm/bin";
        // Front position, ahead of the front door and the inherited PATH.
        assert_eq!(
            session_path_env(
                Some(reroute),
                None,
                Some(bin),
                Some(&join(&["/usr/bin", "/bin"]))
            ),
            Some((
                "PATH".to_string(),
                join(&[reroute, bin, "/usr/bin", "/bin"])
            ))
        );
        // Listed later (twice) in the inherited PATH: moved to the front, once.
        let inherited = join(&["/Users//u/.cargo/bin", reroute, bin, "/usr/bin", reroute]);
        let (_, value) =
            session_path_env(Some(reroute), None, Some(bin), Some(&inherited)).expect("injects");
        assert_eq!(
            value,
            join(&[reroute, "/Users//u/.cargo/bin", bin, "/usr/bin"])
        );
        assert_eq!(value.matches(reroute).count(), 1);
        // Applied to its own output: the same value (a nested aterm stacks nothing).
        let (_, again) =
            session_path_env(Some(reroute), None, Some(bin), Some(&value)).expect("injects");
        assert_eq!(again, value);
        // No front door (a lone binary), no inherited PATH: the reroute dir alone.
        assert_eq!(
            session_path_env(Some(reroute), None, None, None).map(|p| p.1),
            Some(reroute.to_string())
        );
        assert_eq!(
            session_path_env(Some(reroute), None, None, Some("")).map(|p| p.1),
            Some(reroute.to_string())
        );
        // No reroute dir (`--no-reroute`, Windows): `prepend_path`'s contract, unchanged.
        assert_eq!(
            session_path_env(None, None, Some(bin), Some("/usr/bin")),
            prepend_path(bin, Some("/usr/bin"))
        );
        assert_eq!(
            session_path_env(None, None, Some(bin), Some(&join(&["/usr/bin", bin]))),
            None
        );
        assert_eq!(session_path_env(None, None, None, Some("/usr/bin")), None);
    }

    /// The managed `agents/` goes in front in the window's order (reroute,
    /// agents, front door, inherited) by move-to-front: a `~/.local/bin` or
    /// `/opt/homebrew/bin` listed earlier ends up behind it, idempotently.
    #[test]
    fn session_path_puts_the_agents_dir_second_by_the_same_move_to_front_rule() {
        let sep = if cfg!(windows) { ';' } else { ':' };
        let join = |parts: &[&str]| parts.join(&sep.to_string());
        let reroute = "/Users//u/Library/Application Support/aterm/pkg/reroute";
        let agents = "/Users//u/Library/Application Support/aterm/pkg/agents";
        let bin = "/opt/aterm/bin";
        let foreign = ["/Users//u/.local/bin", "/opt/homebrew/bin", "/usr/bin"];
        // The window's order, ahead of the foreign homes.
        let (_, value) = session_path_env(
            Some(reroute),
            Some(agents),
            Some(bin),
            Some(&join(&foreign)),
        )
        .expect("injects");
        assert_eq!(
            value,
            join(&[
                reroute,
                agents,
                bin,
                "/Users//u/.local/bin",
                "/opt/homebrew/bin",
                "/usr/bin"
            ])
        );
        // Listed later (twice, behind the foreign homes): moved to the front, once.
        let inherited = join(&["/Users//u/.local/bin", agents, "/opt/homebrew/bin", agents]);
        let (_, value) =
            session_path_env(Some(reroute), Some(agents), None, Some(&inherited)).expect("injects");
        assert_eq!(
            value,
            join(&[reroute, agents, "/Users//u/.local/bin", "/opt/homebrew/bin"])
        );
        assert_eq!(value.matches(agents).count(), 1);
        // Applied to its own output: the same value.
        let (_, again) =
            session_path_env(Some(reroute), Some(agents), None, Some(&value)).expect("injects");
        assert_eq!(again, value);
        // Without a reroute dir the agents dir still leads (`$ATPKG_AGENTS` from an
        // enclosing shell); alone, it is the whole PATH.
        assert_eq!(
            session_path_env(None, Some(agents), Some(bin), Some("/usr/bin")).map(|p| p.1),
            Some(join(&[agents, bin, "/usr/bin"]))
        );
        assert_eq!(
            session_path_env(None, Some(agents), None, None).map(|p| p.1),
            Some(agents.to_string())
        );
    }

    /// The derived agents dir is atpkg's `Layout::agents_dir` for the prefix of
    /// `$ATERM_REROUTE_DIR` (pinned against the real layout), and the
    /// derivation ensures it exists (`0700`), so a fresh machine's first
    /// session has it on PATH.
    #[test]
    fn the_agents_dir_is_the_reroute_dirs_sibling_in_atpkgs_layout() {
        let scratch = std::env::temp_dir().join(format!(
            "aterm-cli-agents-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
        ));
        let layout = atpkg::store::Layout {
            prefix: scratch.join("pkg"),
        };
        let reroute = layout.reroute_dir();
        std::fs::create_dir_all(&reroute).expect("the front door's reroute dir");
        assert!(
            !layout.agents_dir().is_dir(),
            "a fresh prefix has no agents/"
        );
        let derived =
            managed_agents_dir(None, reroute.to_str(), None).expect("derived and created");
        assert_eq!(derived, layout.agents_dir().to_str().unwrap());
        assert!(layout.agents_dir().is_dir(), "ensured, like the window's");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(layout.agents_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "a $HOME prefix's directory is private");
        }
        // A second call is a no-op with the same answer.
        assert_eq!(
            managed_agents_dir(None, reroute.to_str(), None).as_deref(),
            Some(derived.as_str())
        );
        // And the sibling rule is the layout's: the reroute dir is `<prefix>/reroute`.
        assert_eq!(reroute, layout.prefix.join(atpkg::reroute::DIR_NAME));
        assert_eq!(layout.agents_dir(), layout.prefix.join("agents"));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// The reroute and agents variables this crate reads match atpkg's
    /// spellings, and the front door's agents handoff is not the hook's export.
    #[test]
    fn reroute_env_names_match_atpkg() {
        assert_eq!(REROUTE_DIR_ENV, atpkg::reroute::REROUTE_DIR_ENV);
        assert_eq!(PASSTHROUGH_ENV, atpkg::reroute::PASSTHROUGH_ENV);
        assert_eq!(AGENTS_DIR_ENV, atpkg::reroute::AGENTS_DIR_ENV);
        assert_ne!(AGENTS_DIR_ENV, HOOK_AGENTS_ENV);
    }

    /// Precedence: a valid `$ATERM_AGENTS_DIR` (absolute, existing, real dir)
    /// wins with or without a reroute handle and over `$ATPKG_AGENTS`, and
    /// nothing is created for it; an invalid one falls through to the sibling
    /// derivation (ensured), then `$ATPKG_AGENTS`, then none.
    #[test]
    fn managed_agents_dir_prefers_the_front_doors_handoff_then_the_sibling_then_the_hook() {
        let scratch = scratch_dir("agents-precedence");
        let handed = scratch.join("handed-agents");
        std::fs::create_dir(&handed).unwrap();
        let handed_str = handed.to_str().unwrap();
        let prefix = scratch.join("pkg");
        let reroute = prefix.join("reroute");
        std::fs::create_dir_all(&reroute).unwrap();
        let reroute_str = reroute.to_str().unwrap();
        let hook = scratch.join("hook-agents");
        std::fs::create_dir(&hook).unwrap();
        let hook_str = hook.to_str().unwrap();
        // 1. The handoff wins — the `--no-reroute` shape (no reroute handle) included —
        //    and derives nothing.
        assert_eq!(
            managed_agents_dir(Some(handed_str), None, None).as_deref(),
            Some(handed_str),
            "the --no-reroute shape: the handoff alone fronts the managed agents"
        );
        assert_eq!(
            managed_agents_dir(Some(handed_str), Some(reroute_str), Some(hook_str)).as_deref(),
            Some(handed_str)
        );
        assert!(
            !prefix.join("agents").exists(),
            "a taken handoff derives and creates nothing beside the reroute dir"
        );
        // Shapes that are not the front door's fall through.
        for stray in ["", "agents", "./handed-agents"] {
            assert_eq!(
                managed_agents_dir(Some(stray), None, Some(hook_str)).as_deref(),
                Some(hook_str),
                "{stray:?} is not an absolute handoff"
            );
        }
        assert_eq!(
            managed_agents_dir(scratch.join("absent").to_str(), None, Some(hook_str)).as_deref(),
            Some(hook_str),
            "must exist"
        );
        let filed = scratch.join("filed-agents");
        std::fs::write(&filed, b"not a dir").unwrap();
        assert_eq!(
            managed_agents_dir(filed.to_str(), None, Some(hook_str)).as_deref(),
            Some(hook_str),
            "must be a directory"
        );
        #[cfg(unix)]
        {
            let linked = scratch.join("linked-agents");
            std::os::unix::fs::symlink(&handed, &linked).unwrap();
            assert!(linked.is_dir(), "the link resolves");
            assert_eq!(
                managed_agents_dir(linked.to_str(), None, Some(hook_str)).as_deref(),
                Some(hook_str),
                "a symlink is never the front door's handoff"
            );
        }
        // 2. No handoff: the sibling derivation, ensured here, over the hook's value.
        let derived = managed_agents_dir(None, Some(reroute_str), Some(hook_str)).expect("derived");
        assert_eq!(derived, prefix.join("agents").to_str().unwrap());
        assert!(prefix.join("agents").is_dir(), "ensured");
        assert_eq!(
            managed_agents_dir(Some(""), Some(reroute_str), Some(hook_str)).as_deref(),
            Some(derived.as_str()),
            "an EMPTY handoff means none — the sibling rule applies"
        );
        // 3. Neither: the enclosing shell's hook export; then nothing.
        assert_eq!(
            managed_agents_dir(None, None, Some(hook_str)).as_deref(),
            Some(hook_str)
        );
        assert_eq!(managed_agents_dir(None, None, None), None);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A fresh scratch directory for one test, named by pid and nanos.
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-cli-{tag}-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// `$ATERM_REROUTE_DIR` is taken only as a non-empty absolute path to an
    /// existing directory, and never while the `--no-reroute` marker is engaged
    /// (by `env_flag_engaged`: `0` and empty do not engage it).
    #[test]
    fn reroute_dir_from_values_takes_only_an_absolute_existing_dir() {
        let scratch = scratch_dir("reroute-values");
        let reroute = scratch.join("reroute");
        std::fs::create_dir(&reroute).unwrap();
        let abs = reroute.to_str().unwrap();
        assert_eq!(
            reroute_dir_from_values(None, Some(abs)).as_deref(),
            Some(abs)
        );
        assert_eq!(
            reroute_dir_from_values(Some(""), Some(abs)).as_deref(),
            Some(abs)
        );
        assert_eq!(
            reroute_dir_from_values(Some("0"), Some(abs)).as_deref(),
            Some(abs)
        );
        assert_eq!(reroute_dir_from_values(Some("1"), Some(abs)), None);
        assert_eq!(reroute_dir_from_values(None, None), None);
        assert_eq!(reroute_dir_from_values(None, Some("")), None);
        assert_eq!(
            reroute_dir_from_values(None, Some("reroute")),
            None,
            "relative"
        );
        assert_eq!(
            reroute_dir_from_values(None, Some("./reroute")),
            None,
            "relative"
        );
        assert_eq!(
            reroute_dir_from_values(None, scratch.join("absent").to_str()),
            None,
            "must exist"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// `agents_dir_mode` matches `Layout::ensure_dir` (`0700` for the `$HOME`
    /// shape, `0755` for a root-owned system prefix) and refuses a prefix owned
    /// by another user: under `sudo aterm`, root gets `0755` over a system
    /// prefix and creates nothing in a user's `$HOME` prefix.
    #[test]
    fn agents_dir_mode_restates_layout_ensure_dirs_rule() {
        // A user's own $HOME prefix, by that user.
        assert_eq!(agents_dir_mode(501, 0o700, 501), Some(0o700));
        assert_eq!(agents_dir_mode(501, 0o755, 501), Some(0o700));
        // A system prefix, by any user: 0755 (a non-root mkdir there fails on its own).
        assert_eq!(agents_dir_mode(0, 0o755, 0), Some(0o755));
        assert_eq!(agents_dir_mode(0, 0o755, 501), Some(0o755));
        assert_eq!(agents_dir_mode(0, 0o700, 0), Some(0o755));
        // Root-owned but group/other-writable is not the system shape; root owns it ⇒ 0700.
        assert_eq!(agents_dir_mode(0, 0o775, 0), Some(0o700));
        assert_eq!(agents_dir_mode(0, 0o777, 501), None);
        // Someone else's prefix: refused — root over a user's $HOME prefix included.
        assert_eq!(agents_dir_mode(501, 0o700, 0), None);
        assert_eq!(agents_dir_mode(501, 0o700, 502), None);
    }

    /// `managed_agents_dir` creates nothing from a relative reroute handle,
    /// refuses a symlink or regular file at `agents/`, and without a derivable
    /// directory falls back to an absolute existing `$ATPKG_AGENTS`. The
    /// derived directory wins over the fallback.
    #[test]
    fn managed_agents_dir_refuses_relative_handles_and_links_and_falls_back_to_the_enclosing_shell()
    {
        let scratch = scratch_dir("agents-refuse");
        let cwd_agents = std::path::Path::new("agents");
        assert!(
            !cwd_agents.exists(),
            "precondition: the test cwd has no `agents` entry"
        );
        // Relative handle: nothing derived, nothing created.
        assert_eq!(managed_agents_dir(None, Some("reroute"), None), None);
        assert_eq!(managed_agents_dir(None, Some("./pkg/reroute"), None), None);
        assert!(!cwd_agents.exists(), "no `agents` created in the cwd");
        // Fallback shapes.
        let enclosing = scratch.join("enclosing-agents");
        std::fs::create_dir(&enclosing).unwrap();
        let enclosing_str = enclosing.to_str().unwrap();
        assert_eq!(
            managed_agents_dir(None, None, Some(enclosing_str)).as_deref(),
            Some(enclosing_str)
        );
        assert_eq!(
            managed_agents_dir(None, Some("reroute"), Some(enclosing_str)).as_deref(),
            Some(enclosing_str),
            "a refused relative handle still leaves the fallback"
        );
        assert_eq!(managed_agents_dir(None, None, Some("")), None);
        assert_eq!(
            managed_agents_dir(None, None, Some("agents")),
            None,
            "relative"
        );
        assert_eq!(
            managed_agents_dir(None, None, scratch.join("absent").to_str()),
            None,
            "must exist"
        );
        assert_eq!(managed_agents_dir(None, None, None), None);
        // Derived wins over the fallback.
        let prefix = scratch.join("pkg");
        let reroute = prefix.join("reroute");
        std::fs::create_dir_all(&reroute).unwrap();
        let derived =
            managed_agents_dir(None, reroute.to_str(), Some(enclosing_str)).expect("derived");
        assert_eq!(derived, prefix.join("agents").to_str().unwrap());
        // A regular file at agents/: refused silently and left alone; the
        // enclosing shell's directory is still the fallback.
        let filed = scratch.join("filed");
        std::fs::create_dir_all(filed.join("reroute")).unwrap();
        std::fs::write(filed.join("agents"), b"not a dir").unwrap();
        assert_eq!(
            managed_agents_dir(None, filed.join("reroute").to_str(), None),
            None
        );
        assert_eq!(
            managed_agents_dir(None, filed.join("reroute").to_str(), Some(enclosing_str))
                .as_deref(),
            Some(enclosing_str)
        );
        assert!(filed.join("agents").is_file(), "left alone");
        #[cfg(unix)]
        {
            // A symlink at agents/ — even one that points at a real directory: refused.
            let linked = scratch.join("linked");
            std::fs::create_dir_all(linked.join("reroute")).unwrap();
            std::os::unix::fs::symlink(&enclosing, linked.join("agents")).unwrap();
            assert!(linked.join("agents").is_dir(), "the link resolves");
            assert_eq!(
                managed_agents_dir(None, linked.join("reroute").to_str(), None),
                None
            );
            assert_eq!(
                managed_agents_dir(None, linked.join("reroute").to_str(), Some(enclosing_str))
                    .as_deref(),
                Some(enclosing_str)
            );
            assert!(
                std::fs::symlink_metadata(linked.join("agents"))
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "left alone"
            );
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A prefix that refuses the `mkdir` yields `None` and creates nothing.
    /// COULD NOT RUN as root, which ignores mode bits.
    #[cfg(unix)]
    #[test]
    fn managed_agents_dir_says_no_when_the_prefix_refuses_the_mkdir() {
        use std::os::unix::fs::PermissionsExt as _;
        // SAFETY: getuid() takes no arguments and cannot fail.
        if unsafe { libc::getuid() } == 0 {
            aterm_spec::host::could_not_run("running as root, a read-only prefix refuses nothing");
        }
        let scratch = scratch_dir("agents-readonly");
        let prefix = scratch.join("pkg");
        let reroute = prefix.join("reroute");
        std::fs::create_dir_all(&reroute).unwrap();
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(managed_agents_dir(None, reroute.to_str(), None), None);
        assert!(!prefix.join("agents").exists());
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Writable again: created, private.
        assert_eq!(
            managed_agents_dir(None, reroute.to_str(), None).as_deref(),
            prefix.join("agents").to_str()
        );
        let mode = std::fs::metadata(prefix.join("agents"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o700);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn show_config_report_has_stable_keys() {
        let (r, code) = diag_report("show-config", None).expect("show-config dispatches");
        assert_eq!(code, 0, "show-config always succeeds");
        for key in [
            "version=",
            "shell=",
            "term=",
            "rows=",
            "cols=",
            "containment_default=user",
        ] {
            assert!(
                r.contains(key),
                "show-config missing {key:?}\n--- report ---\n{r}"
            );
        }
        // Stable, scriptable shape: every non-empty line is `key=value`.
        for line in r.lines().filter(|l| !l.is_empty()) {
            assert!(line.contains('='), "non key=value line: {line:?}");
        }
    }

    #[test]
    fn show_face_requires_and_validates_family() {
        // No family → usage error, exit 2 like its sibling usage errors.
        let (msg, code) = show_face_report(None);
        assert_eq!(code, 2);
        assert!(msg.contains("usage: aterm show-face <family>"), "{msg}");
        // An unresolvable family → not-found error, exit 1, naming where the valid
        // names are (the positive path is covered by aterm-render's face_info
        // test against a real system font).
        let (msg, code) = show_face_report(Some("definitely-not-a-real-font-xyzzy"));
        assert_eq!(code, 1);
        assert!(msg.contains("aterm list-fonts"), "{msg}");
    }

    /// The mark of the `doctor` row labelled `label` — the row's second word.
    fn mark_of(report: &str, label: &str) -> String {
        report
            .lines()
            .find(|l| l.starts_with(label))
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or_else(|| panic!("no {label} row in\n{report}"))
            .to_string()
    }

    /// `doctor`'s console-host row reads the top-level `conpty` the window
    /// reads; anything else (no key, a commented example, a key in a table, a
    /// non-string, a file that does not parse) is the window's default.
    #[test]
    fn config_conpty_reads_the_top_level_key_the_window_reads() {
        assert_eq!(config_conpty_from_text(""), None);
        assert_eq!(config_conpty_from_text("# conpty = \"auto\"\n"), None);
        assert_eq!(config_conpty_from_text("[x]\nconpty = \"auto\"\n"), None);
        assert_eq!(config_conpty_from_text("conpty = 1\n"), None);
        assert_eq!(config_conpty_from_text("conpty = \"auto\" x\n"), None);
        assert_eq!(
            config_conpty_from_text("conpty = 'C:\\oc\\OpenConsole.exe'\n").as_deref(),
            Some("C:\\oc\\OpenConsole.exe")
        );
    }

    /// The Windows `conpty:` row lands with the machine rows, in the rows'
    /// columns: `ok` for OpenConsole (the default) and for the inbox host
    /// `conpty = "inbox"` asked for, a `note` when a named OpenConsole fell
    /// back, and the verdict untouched either way. No fact (every platform but
    /// Windows) is no row.
    #[test]
    fn the_conpty_row_is_ok_for_the_default_and_a_note_for_a_fallback() {
        let report_with = |host: Option<aterm_pty::ConsoleHostReport>| {
            doctor_checks_with_origin(
                "/bin/sh",
                ShellCheck::Runs,
                None,
                &unmeasured(),
                &no_debugger(),
                &no_devtools(),
                &no_census(),
                &no_recovery(),
                &no_exceptions(),
                &MachineFacts {
                    console_host: host,
                    ..MachineFacts::absent()
                },
            )
        };
        let (none, code) = report_with(None);
        assert_eq!(code, 0);
        assert!(!none.lines().any(|l| l.starts_with("conpty:")), "{none}");

        let inbox = aterm_pty::ConsoleHostReport {
            kind: aterm_pty::ConsoleHostKind::Inbox,
            fell_back: false,
            line: "Windows console host (aterm.toml conpty = \"inbox\")".to_string(),
        };
        let (r, code) = report_with(Some(inbox));
        assert_eq!(code, 0);
        let rows: Vec<&str> = r.lines().collect();
        let at = rows
            .iter()
            .position(|l| l.starts_with("conpty:"))
            .expect("the row");
        assert_eq!(
            rows[at],
            "conpty:     ok    Windows console host (aterm.toml conpty = \"inbox\")"
        );
        assert!(rows[at + 1].starts_with("instances: "), "{r}");
        assert!(r.contains("health: OK\n"), "{r}");

        let fell_back = aterm_pty::ConsoleHostReport {
            kind: aterm_pty::ConsoleHostKind::Inbox,
            fell_back: true,
            line: "Windows console host; OpenConsole did not start (x)".to_string(),
        };
        let (r, code) = report_with(Some(fell_back));
        assert_eq!(code, 0, "a fallback never fails: {r}");
        assert_eq!(mark_of(&r, "conpty:"), "note", "{r}");
        assert!(r.contains("health: OK\n"), "a note is not a warning: {r}");

        let openconsole = aterm_pty::ConsoleHostReport {
            kind: aterm_pty::ConsoleHostKind::OpenConsole,
            fell_back: false,
            line: "OpenConsole 1.24.2607.10001 (bundled beside aterm: x)".to_string(),
        };
        let (r, _) = report_with(Some(openconsole));
        assert_eq!(mark_of(&r, "conpty:"), "ok", "{r}");
    }

    #[test]
    fn doctor_checks_pass_and_flag_each_failure() {
        // /bin/sh is executable on every POSIX host.
        let (r, code) = doctor_checks(
            "/bin/sh",
            ShellCheck::Runs,
            &unmeasured(),
            &no_debugger(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
        );
        assert_eq!(code, 0, "{r}");
        assert!(r.contains("health: OK"), "{r}");
        assert_eq!(mark_of(&r, "shell:"), "ok", "{r}");
        // No row about doctor's own stdout, and no containment row: the mode is
        // a launch flag a separate `doctor` run cannot read.
        assert!(
            !r.contains("tty:") && !r.contains("cols") && !r.contains("containment"),
            "{r}"
        );
        // Every row is a uniform `key: …` line (no stray `OK:`/`ERR:` prefix).
        for key in ["shell: ", "privacy: ", "debugger: ", "version: "] {
            assert!(
                r.lines().any(|l| l.starts_with(key)),
                "doctor detail missing a {key:?} line\n{r}"
            );
        }

        // The two causes a shell cannot start are told apart, and the summary
        // names the shell and the one fix.
        let (r, code) = doctor_checks(
            "/no/such/shell",
            ShellCheck::Missing,
            &unmeasured(),
            &no_debugger(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
        );
        assert_eq!(code, 1);
        assert_eq!(mark_of(&r, "shell:"), "FAIL", "{r}");
        assert!(r.contains("health: FAIL"), "{r}");
        let (r, code) = doctor_checks(
            "/etc/hosts",
            ShellCheck::NotExecutable,
            &unmeasured(),
            &no_debugger(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
        );
        assert_eq!(code, 1);
        assert_eq!(mark_of(&r, "shell:"), "FAIL", "{r}");

        // A shell named by nothing fails rather than reading as a program. No
        // Unix resolution yields one (an empty $SHELL reads as unset, and doctor
        // then checks the login shell); the row guards every platform's input.
        let (r, code) = doctor_checks(
            "",
            ShellCheck::Missing,
            &unmeasured(),
            &no_debugger(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
        );
        assert_eq!(code, 1);
        assert_eq!(mark_of(&r, "shell:"), "FAIL", "{r}");
    }

    /// `$SHELL` unset is not a failure: a plain `aterm` then starts the
    /// account's login shell, else `/bin/sh` (aterm-pty's resolver), so that is
    /// the shell doctor checks and names, and says why.
    #[cfg(unix)]
    #[test]
    fn doctor_checks_the_login_shell_or_bin_sh_when_shell_is_unset() {
        use aterm_pty::ShellOrigin;
        assert_eq!(
            unix_shell(("/bin/zsh".into(), ShellOrigin::Env)),
            ("/bin/zsh".to_string(), None)
        );
        assert_eq!(
            unix_shell(("/usr/bin/fish".into(), ShellOrigin::Passwd)),
            (
                "/usr/bin/fish".to_string(),
                Some(
                    "$SHELL is unset or empty, so aterm starts your login shell from the password database"
                )
            )
        );
        let (shell, origin) = unix_shell(("/bin/sh".into(), ShellOrigin::Default));
        assert_eq!(shell, "/bin/sh");
        let (r, code) = doctor_checks_with_origin(
            &shell,
            driver::shell_check(&shell),
            origin,
            &unmeasured(),
            &no_debugger(),
            &no_devtools(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts::absent(),
        );
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "shell:"), "ok", "{r}");
        assert!(origin.is_some(), "the row says why /bin/sh");
    }

    /// The driver's shell check tells a missing path from one that cannot run.
    #[cfg(unix)]
    #[test]
    fn shell_check_names_the_cause() {
        assert_eq!(driver::shell_check("/bin/sh"), ShellCheck::Runs);
        assert_eq!(
            driver::shell_check("/no/such/aterm-shell"),
            ShellCheck::Missing
        );
        assert_eq!(driver::shell_check("/"), ShellCheck::NotExecutable);
        assert_eq!(driver::shell_check("/etc/hosts"), ShellCheck::NotExecutable);
        assert_eq!(driver::shell_check("/etc/hosts/zsh"), ShellCheck::Missing);
    }

    /// Off macOS the privacy and debugger probes do not exist, so their rows are
    /// left out instead of printing `note  not measured (macOS only)` every run.
    #[test]
    fn doctor_leaves_out_rows_for_probes_this_platform_lacks() {
        let (r, code) = doctor_checks(
            "/bin/sh",
            ShellCheck::Runs,
            &PrivacyFacts::not_measured(ProbeLabel::UnsupportedPlatform),
            &DebuggerFacts::not_measured(DebuggerProbeLabel::UnsupportedPlatform),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
        );
        assert_eq!(code, 0, "{r}");
        assert!(!r.contains("privacy:") && !r.contains("debugger:"), "{r}");
        assert!(!r.contains("macOS only"), "{r}");
        assert!(r.contains("instances:"), "{r}");
    }

    /// The `shell:` row can carry where the shell came from; without a label it
    /// is the unlabelled Unix row.
    #[test]
    fn doctor_shell_row_carries_an_origin_label_only_when_given() {
        let shell_row = |report: &str| -> String {
            report
                .lines()
                .find(|l| l.starts_with("shell:"))
                .unwrap_or_else(|| panic!("no shell: row in\n{report}"))
                .to_string()
        };
        let (plain, _) = doctor_checks(
            "/bin/sh",
            ShellCheck::Runs,
            &unmeasured(),
            &no_debugger(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
        );
        let (unlabelled, _) = doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &no_devtools(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts::absent(),
        );
        assert_eq!(plain, unlabelled, "no label, no change");

        let (labelled, code) = doctor_checks_with_origin(
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            ShellCheck::Runs,
            Some("a new window's default shell, the platform default (pwsh on PATH)"),
            &unmeasured(),
            &no_debugger(),
            &no_devtools(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts::absent(),
        );
        assert_eq!(code, 0);
        assert_eq!(mark_of(&labelled, "shell:"), "ok", "{labelled}");
        assert!(
            shell_row(&labelled)
                .ends_with("a new window's default shell, the platform default (pwsh on PATH)"),
            "{labelled}"
        );

        // The label rides on a failing row too: the reader needs to know which
        // input named the missing shell before they can fix it.
        let (failed, code) = doctor_checks_with_origin(
            "nosuch",
            ShellCheck::Missing,
            Some("a new window's default shell, from aterm.toml shell = \"nosuch\""),
            &unmeasured(),
            &no_debugger(),
            &no_devtools(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts::absent(),
        );
        assert_eq!(code, 1);
        assert_eq!(mark_of(&failed, "shell:"), "FAIL", "{failed}");
        assert!(
            shell_row(&failed)
                .ends_with("a new window's default shell, from aterm.toml shell = \"nosuch\""),
            "{failed}"
        );
    }

    /// Windows: `doctor` and `show-config` report the shell a window tab runs
    /// (this tab's inside one, a new window's default outside), labelled, never
    /// this process's `$SHELL`/`%COMSPEC%`.
    #[cfg(windows)]
    #[test]
    fn windows_doctor_and_show_config_report_the_windows_shell() {
        let window_shell = driver::WindowShell::resolve();
        let (doctor, _) = doctor_report(true);
        let shell_line = doctor
            .lines()
            .find(|l| l.starts_with("shell: "))
            .expect("a shell row");
        assert!(shell_line.contains(&window_shell.program), "{shell_line}");
        let in_a_tab = std::env::var_os(aterm_types::domain::ENV_TAB_SHELL)
            .is_some_and(|program| !program.is_empty());
        let whose = if in_a_tab {
            "— this tab's shell"
        } else {
            "— a new window's default shell"
        };
        assert!(shell_line.contains(whose), "{shell_line}");
        assert!(
            shell_line.contains(&window_shell.origin_sentence()),
            "{shell_line}"
        );
        let show = show_config_report();
        assert!(
            show.contains(&format!("shell={}\n", window_shell.program)),
            "{show}"
        );
        assert!(
            show.contains(&format!("shell_origin={}\n", window_shell.origin_token())),
            "{show}"
        );
        // Never the CLI process's POSIX `$SHELL` (Git Bash exports
        // `/usr/bin/bash`, which CreateProcessW cannot run).
        if let Ok(posix_shell) = std::env::var("SHELL")
            && posix_shell.starts_with('/')
        {
            assert!(!doctor.contains(&posix_shell), "{doctor}");
            assert!(!show.contains(&posix_shell), "{show}");
        }
    }

    /// `explain-config` names this machine's resolved aterm.toml first, on
    /// every platform, and never claims no environment variable matters
    /// (XDG_CONFIG_HOME moves the file it names).
    #[test]
    fn explain_config_names_the_resolved_config_path() {
        let r = explain_config_report();
        let path = aterm_types::dirs::aterm_config_path()
            .expect("APPDATA or HOME is set on a test box")
            .display()
            .to_string();
        assert!(
            r.contains(&format!("\n\naterm.toml: {path}\n")),
            "explain-config missing the resolved path {path:?}\n{r}"
        );
    }

    /// The four `privacy:` states (§3.8): a `note` never moves the exit code,
    /// while a real `Fail` elsewhere still exits 1.
    #[test]
    fn the_privacy_row_is_a_note_and_never_moves_the_exit_code() {
        let ok = |privacy: &PrivacyFacts| {
            doctor_checks(
                "/bin/sh",
                ShellCheck::Runs,
                privacy,
                &no_debugger(),
                &no_census(),
                &no_recovery(),
                &no_exceptions(),
            )
        };

        for (facts, mark) in [
            (
                measured(FdaState::Granted, DrClass::Identity, "aterm"),
                "ok",
            ),
            (
                measured(FdaState::Denied, DrClass::Identity, "iTerm2"),
                "note",
            ),
            // A dev build: the grant may be real today and dead after the next build.
            (
                measured(FdaState::Granted, DrClass::Cdhash, "aterm (dev)"),
                "note",
            ),
            (unmeasured(), "note"),
        ] {
            let (r, code) = ok(&facts);
            assert_eq!(code, 0, "a privacy row never fails doctor\n{r}");
            assert!(r.contains("health: OK"), "{r}");
            assert_eq!(mark_of(&r, "privacy:"), mark, "{r}");
        }

        // And a genuine Fail still fails, with the note alongside it.
        let (r, code) = doctor_checks(
            "/no/such/shell",
            ShellCheck::Missing,
            &unmeasured(),
            &no_debugger(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
        );
        assert_eq!(code, 1, "{r}");
        assert!(r.contains("health: FAIL"), "{r}");
        assert_eq!(mark_of(&r, "privacy:"), "note", "{r}");
    }

    /// The `recovery:` row: an empty ledger is OK and says where the record will
    /// write; a recorded kill is a NOTE that names it — never a FAIL, so a past crash
    /// does not make `aterm doctor && aterm` refuse to launch; no log directory is
    /// "not measured", never "none". Unix only: Windows records no ledger.
    #[cfg(unix)]
    #[test]
    fn the_recovery_row_reports_the_ledger_and_never_fails() {
        use aterm_update::recovery_ledger::{
            EndClass, LaunchKind, Pressure, PrevRun, Row, Tri, Uptime,
        };
        let check = |recovery: RecoveryFacts| {
            doctor_checks(
                "/bin/sh",
                ShellCheck::Runs,
                &unmeasured(),
                &no_debugger(),
                &no_census(),
                &recovery,
                &no_exceptions(),
            )
        };
        let (r, code) = check(no_recovery());
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "recovery:"), "ok", "{r}");

        let killed = Row {
            at: 1_790_500_000,
            pid: 2,
            started: 1_790_499_990,
            started_us: None,
            build: 1_790_400_000,
            version: "0.94.0".into(),
            launch: LaunchKind::Cold,
            prev: PrevRun {
                pid: Some(1),
                build: Some(1_790_305_290),
                version: Some("0.93.0".into()),
                class: EndClass::Killed,
                uptime: Some(Uptime {
                    secs: 7200,
                    at_least: true,
                }),
                stall: Tri::Yes,
                pressure: Pressure::Normal,
                trial: 0,
            },
            keeper: None,
            restores: None,
        };
        let path = std::path::PathBuf::from("/logs/recovery-ledger.log");
        let (r, code) = check(Ok((vec![killed], path)));
        assert_eq!(code, 0, "a past kill never fails doctor: {r}");
        assert_eq!(mark_of(&r, "recovery:"), "note", "{r}");
        assert!(
            r.contains("1790305290"),
            "the row names the killed build: {r}"
        );
        // One row, several lines: the label once, the later lines under the fact
        // column, as every multi-line row prints.
        assert_eq!(r.matches("recovery:").count(), 1, "{r}");

        let (r, code) = check(Err("no log folder, so there is no ledger".to_owned()));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "recovery:"), "note", "{r}");
        // The label is said once, by the row: the fact under it does not repeat it.
        assert_eq!(r.matches("recovery:").count(), 1, "{r}");
    }

    /// The `instances:` row in every state: a note that names each unreached
    /// instance and never moves the exit code.
    #[test]
    fn the_instances_row_names_what_nothing_reaches_and_never_fails() {
        use aterm_ctl::census::{Census, Unreached};
        let check = |census: Result<Census, String>| {
            doctor_checks(
                "/bin/sh",
                ShellCheck::Runs,
                &unmeasured(),
                &no_debugger(),
                &census,
                &no_recovery(),
                &no_exceptions(),
            )
        };
        let census = |instances: usize, unreached: Vec<Unreached>| {
            Ok(Census {
                dir: "/run/aterm".to_string(),
                instances,
                unreached,
            })
        };

        for n in [0, 2] {
            let (r, code) = check(census(n, Vec::new()));
            assert_eq!(code, 0, "{r}");
            assert_eq!(mark_of(&r, "instances:"), "ok", "{r}");
        }

        let orphan = Unreached {
            pid: 67548,
            age: "11d 22h".to_string(),
            command: "./aterm-before --headless".to_string(),
            socketless: false,
        };
        let deliberate = Unreached {
            pid: 68000,
            age: "5s".to_string(),
            command: "aterm --headless --no-control-sock".to_string(),
            socketless: true,
        };
        let (r, code) = check(census(3, vec![orphan, deliberate]));
        assert_eq!(
            code, 0,
            "an unreachable instance is a note, not a failure\n{r}"
        );
        assert!(r.contains("health: OK"), "{r}");
        assert_eq!(mark_of(&r, "instances:"), "note", "{r}");
        assert!(
            r.contains("pid 67548") && r.contains("pid 68000"),
            "each unreached instance is named: {r}"
        );

        let (r, code) = check(Err("the process table could not be read".to_string()));
        assert_eq!(code, 0, "{r}");
        assert_eq!(
            mark_of(&r, "instances:"),
            "note",
            "a census that was not taken never reads as none\n{r}"
        );
    }

    /// Running `doctor_report` from a test binary outside an `.app` bundle must
    /// report `not measured` without probing `tccd`, `DevToolsSecurity`, `csops`,
    /// `Copies`, or Spotlight.
    #[cfg(target_os = "macos")]
    #[test]
    fn doctor_run_from_a_test_binary_measures_nothing() {
        let (r, _) = doctor_report(true);
        let row = |label: &str| {
            r.lines()
                .find(|l| l.starts_with(label))
                .unwrap_or_else(|| panic!("no {label} row in\n{r}"))
        };
        for label in ["privacy:", "debugger:", "devtools:"] {
            assert_eq!(mark_of(&r, label), "note", "{r}");
            assert!(
                row(label).contains("not measured"),
                "the fence let a {label} verdict through\n{r}"
            );
        }
        assert!(
            row("copies:").contains("not checked"),
            "a test binary must never prune\n{r}"
        );
        assert!(
            row("spotlight:").contains("not measured (a test binary)"),
            "{r}"
        );
    }

    /// The `spotlight:` row reports indexed build/doc folders as `note` (exit 0),
    /// clean state as `ok`, and omits the row when `spotlight` is `None`.
    #[test]
    fn the_spotlight_row_is_a_note_at_worst() {
        let report = |spotlight| {
            doctor_checks_with_origin(
                "/bin/sh",
                ShellCheck::Runs,
                None,
                &unmeasured(),
                &no_debugger(),
                &DevtoolsFacts::absent(),
                &no_census(),
                &no_recovery(),
                &no_exceptions(),
                &MachineFacts {
                    spotlight,
                    ..MachineFacts::absent()
                },
            )
        };
        let lit = crate::spotlight::Spotlit {
            builds: vec![("~/w/target".into(), 12)],
            rustup: None,
        };
        let (r, code) = report(Some(Ok(lit)));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "spotlight:"), "note", "{r}");
        assert!(r.contains("~/w/target 12"), "{r}");
        assert!(r.contains("health: OK\n"), "{r}");
        let clean = crate::spotlight::Spotlit {
            builds: vec![],
            rustup: None,
        };
        let (r, _) = report(Some(Ok(clean)));
        assert_eq!(mark_of(&r, "spotlight:"), "ok", "{r}");
        let (r, _) = report(None);
        assert!(!r.contains("spotlight:"), "{r}");
    }

    /// The `syspolicyd:` row is `ok` with room or with none running, a counted
    /// `warn` while the kernel holds the pool at its limit (exit 0: a restart is
    /// the person's), a `note` when unread, and absent off macOS.
    #[test]
    fn the_syspolicyd_row_warns_while_the_pool_is_full() {
        let report = |program_check| {
            doctor_checks_with_origin(
                "/bin/sh",
                ShellCheck::Runs,
                None,
                &unmeasured(),
                &no_debugger(),
                &DevtoolsFacts::absent(),
                &no_census(),
                &no_recovery(),
                &no_exceptions(),
                &MachineFacts {
                    program_check,
                    ..MachineFacts::absent()
                },
            )
        };
        let reading = |workers, at_limit| {
            Some(aterm_uds::program_check::Look::Read(
                aterm_uds::program_check::Reading {
                    daemon: aterm_uds::program_check::Daemon {
                        pid: 690,
                        start_s: 1,
                        start_us: 0,
                    },
                    workers,
                    at_limit,
                    limit: Some(90),
                },
            ))
        };
        let (r, code) = report(reading(4, false));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "syspolicyd:"), "ok", "{r}");
        assert!(r.contains("4 pool threads against a limit of 90"), "{r}");
        let (r, code) = report(reading(95, true));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "syspolicyd:"), "warn", "{r}");
        assert!(r.contains("restarts"), "{r}");
        assert!(r.contains("at the kernel's thread limit"), "{r}");
        // Negative control: a count past the limit with no kernel flag is
        // overcommit and idle threads, not a full pool.
        let (r, _) = report(reading(100, false));
        assert_eq!(mark_of(&r, "syspolicyd:"), "ok", "{r}");
        let (r, _) = report(Some(aterm_uds::program_check::Look::Unread));
        assert_eq!(mark_of(&r, "syspolicyd:"), "note", "{r}");
        let (r, _) = report(Some(aterm_uds::program_check::Look::NoneRunning));
        assert_eq!(mark_of(&r, "syspolicyd:"), "ok", "{r}");
        let (r, _) = report(None);
        assert!(!r.contains("syspolicyd:"), "{r}");
    }

    /// The `fabric:` row renders after `instances:`, counts warnings on the health
    /// line without failing exit status, and is omitted when `fabric` is `None`.
    #[test]
    fn the_fabric_row_is_a_counted_warning_never_a_failure() {
        let report = |fabric: Option<(Mark, String)>| {
            doctor_checks_with_origin(
                "/bin/sh",
                ShellCheck::Runs,
                None,
                &unmeasured(),
                &no_debugger(),
                &no_devtools(),
                &no_census(),
                &no_recovery(),
                &no_exceptions(),
                &MachineFacts {
                    fabric: fabric.map(|(mark, fact)| (mark, fact, false)),
                    ..MachineFacts::absent()
                },
            )
        };
        let (none, code) = report(None);
        assert_eq!(code, 0);
        assert!(!none.lines().any(|l| l.starts_with("fabric:")), "{none}");
        assert!(!none.contains("fabric"), "{none}");

        let down = "the broker at /r/bus.sock does not answer (connect: refused) — `aterm \
                    fabric doctor` names the fix";
        let (r, code) = report(Some((Mark::Warn, down.to_string())));
        assert_eq!(code, 0, "a broker that does not answer fails no shell\n{r}");
        let rows: Vec<&str> = r.lines().collect();
        let at = rows
            .iter()
            .position(|l| l.starts_with("fabric:"))
            .expect("the row");
        assert!(rows[at - 1].starts_with("instances: "), "{r}");
        assert_eq!(rows[at], format!("fabric:     warn  {down}"));
        assert!(r.contains("health: OK — 1 warning (fabric)\n"), "{r}");

        let (r, code) = report(Some((Mark::Ok, "the broker answers".to_string())));
        assert_eq!(code, 0);
        assert_eq!(mark_of(&r, "fabric:"), "ok", "{r}");
        assert!(r.contains("health: OK\n"), "{r}");
    }

    #[cfg(unix)]
    #[test]
    fn the_fabric_weights_are_never_a_failure() {
        use aterm_link::fabric::SummaryLevel;
        assert_eq!(fabric_mark(SummaryLevel::Ok), Mark::Ok);
        assert_eq!(fabric_mark(SummaryLevel::Note), Mark::Note);
        assert_eq!(fabric_mark(SummaryLevel::Warn), Mark::Warn);
        for level in [SummaryLevel::Ok, SummaryLevel::Note, SummaryLevel::Warn] {
            assert!(!fabric_mark(level).is_fail());
        }
    }

    /// `machine_facts` populates `fabric` on Unix and never produces `Mark::Fail`.
    #[cfg(unix)]
    #[test]
    fn the_doctors_machine_facts_read_the_fabric_row() {
        let (mark, fact, _) = machine_facts(None).fabric.expect("a fabric row on Unix");
        assert!(!mark.is_fail(), "{fact}");
        assert!(!fact.trim().is_empty());
    }

    /// While the program check's pool is at the kernel's limit, doctor lists no
    /// program's command line: the census and the fabric's process table say why
    /// instead of reading. Room, no reading, no daemon and no macOS read as before.
    #[cfg(unix)]
    #[test]
    fn a_full_program_check_withholds_the_argument_reads() {
        use aterm_uds::program_check::{Daemon, Look, Reading};
        let reading = |workers, at_limit| {
            Look::Read(Reading {
                daemon: Daemon {
                    pid: 690,
                    start_s: 1,
                    start_us: 0,
                },
                workers,
                at_limit,
                limit: Some(90),
            })
        };
        assert_eq!(
            args_read_withheld(Some(&reading(95, true))),
            Some(ARGS_WITHHELD)
        );
        for look in [
            Some(&reading(4, false)),
            Some(&reading(100, false)),
            Some(&Look::Unread),
            Some(&Look::NoneRunning),
            None,
        ] {
            assert_eq!(args_read_withheld(look), None, "{look:?}");
        }
        let (mark, fact, unmeasured) = machine_facts(Some(ARGS_WITHHELD))
            .fabric
            .expect("the fabric row says why it was not read");
        assert_eq!(mark, Mark::Note);
        assert_eq!(fact, format!("not read ({ARGS_WITHHELD})"));
        assert!(unmeasured);
    }

    /// The `devtools:` and `copies:` rows: aterm not on the Developer Tools list
    /// is a counted `warn` (the health line names it; the exit stays 0) that
    /// says the cost and the owner's fix, followed by what this run did; on the
    /// list is `ok`; a copy moved to the Trash is named.
    #[test]
    fn the_devtools_row_is_a_counted_warning_and_says_what_doctor_did() {
        use aterm_containment::consent::{
            DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, DevToolProbeLabel,
        };
        let list = |listing| DevToolList {
            listing,
            label: DevToolListLabel::Read,
            modified_at_s: Some(1),
            rows: Some(2),
        };
        let devtools = |listing, did: Option<&str>| DevtoolsFacts {
            facts: Some(atpkg::devtools::Facts {
                subject: Some("com.aterm.aterm".to_string()),
                list: list(listing),
                log: DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest),
                other_responsible: None,
            }),
            did: did.map(str::to_string),
        };
        let machine = MachineFacts {
            copies: Some(atpkg::copies::Pruned::Done {
                moved: vec![(
                    std::path::PathBuf::from("/Applications/aterm-prev-1.app"),
                    aterm_containment::consent::Retired::Moved,
                )],
                kept: Vec::new(),
            }),
            console_host: None,
            fabric: None,
            spotlight: None,
            program_check: None,
        };
        let report = |d: &DevtoolsFacts| {
            doctor_checks_with_origin(
                "/bin/sh",
                ShellCheck::Runs,
                None,
                &unmeasured(),
                &no_debugger(),
                d,
                &no_census(),
                &no_recovery(),
                &no_exceptions(),
                &machine,
            )
        };
        // atpkg's own words for the hand-over, so this pins where they land, not their
        // spelling (atpkg's devtools tests pin that).
        let did = atpkg::devtools::handled_words(&atpkg::devtools::Handled::Opened {
            listed_terminal: None,
            step: atpkg::devtools::Step::SwitchOn,
        });
        let did = did.as_str();
        let (r, code) = report(&devtools(DevToolListing::Off, Some(did)));
        assert_eq!(code, 0, "a warning never fails: {r}");
        assert_eq!(mark_of(&r, "devtools:"), "warn", "{r}");
        assert!(r.contains(did), "what doctor did is said:\n{r}");
        assert!(r.contains("1 warning (devtools)"), "{r}");
        assert_eq!(mark_of(&r, "copies:"), "note", "{r}");
        assert!(r.contains("aterm-prev-1.app"), "{r}");
        let (r, code) = report(&devtools(DevToolListing::On, None));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "devtools:"), "ok", "{r}");
        assert!(r.contains("health: OK\n"), "{r}");
        // No Developer Tools list here: no row at all.
        let (r, _) = report(&no_devtools());
        assert!(!r.contains("devtools:"), "{r}");
    }

    /// A scripted [`super::DevtoolsReach`] for doctor's Developer Tools hand-over:
    /// the window answers `answer`, the opened marker reads `opened_ago`, a decline
    /// on record dates `declined_at`, and every act is logged in order instead of
    /// done (nothing is asked, run, opened, written, waited for or printed).
    struct Scripted {
        answer: super::WindowAnswer,
        opened_ago: Option<u64>,
        declined_at: Option<u64>,
        /// The wait sees the switch go on.
        switched: bool,
        log: std::cell::RefCell<Vec<String>>,
    }

    impl Scripted {
        fn new(answer: super::WindowAnswer) -> Self {
            Self {
                answer,
                opened_ago: None,
                declined_at: None,
                switched: true,
                log: std::cell::RefCell::new(Vec::new()),
            }
        }

        /// Run the hand-over for `facts` with a person at the terminal or not:
        /// `(did, opened, log)`.
        fn run(&self, facts: &atpkg::devtools::Facts, person: bool) -> (String, bool, Vec<String>) {
            let ((did, opened), log) =
                self.with(|reach| super::devtools_hand_over(facts, person, reach));
            (did, opened, log)
        }

        /// `act` over this script's reach, and the log of what it did.
        fn with<R>(&self, act: impl FnOnce(&super::DevtoolsReach<'_>) -> R) -> (R, Vec<String>) {
            let note = |entry: String| self.log.borrow_mut().push(entry);
            let reach = super::DevtoolsReach {
                ask_window: &|now| {
                    note(format!("ask now={now}"));
                    self.answer.clone()
                },
                list_terminal: &|| {
                    note("spctl".to_string());
                    Ok(())
                },
                open_pane: &|| {
                    note("open".to_string());
                    Ok(())
                },
                opened_within: &|window| self.opened_ago.filter(|ago| *ago < window),
                note_opened: &|| note("marker".to_string()),
                declined_at: &|| self.declined_at,
                await_on: &|| {
                    note("wait".to_string());
                    self.switched
                },
                refresh_verdict: &|| {
                    note("refresh".to_string());
                    Ok(())
                },
                local_day: &|at| super::local_day_words(at, Some(0)),
                say: &|words| note(format!("say {words}")),
            };
            let out = act(&reach);
            (out, self.log.take())
        }
    }

    /// THE PANE THROTTLE HOLDS IN THE DOCTOR'S OWN PATH (S5). Bare `aterm doctor`
    /// gathers its facts with `fix`, which hands the owner to the Developer Tools pane
    /// ([`super::devtools_hand_over_when`]): within ten minutes of the last opening, with
    /// no window and no person here, it opens nothing, and the doctor's `devtools:` row
    /// says why. `--read-only` asks nothing at all, and over ssh nothing opens.
    #[test]
    fn the_doctors_devtools_hand_over_keeps_the_ten_minute_throttle() {
        use super::WindowAnswer;
        use aterm_containment::DevToolListing;
        let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
        let mut recent = Scripted::new(WindowAnswer::Unreachable);
        recent.opened_ago = Some(120);
        let ((facts, did), log) = recent.with(|reach| {
            super::devtools_hand_over_when(true, absent.clone(), false, false, reach, || {
                panic!("nothing opened, so nothing is read again")
            })
        });
        assert_eq!(
            did.as_deref(),
            Some("did nothing: aterm opened the pane 2 min ago")
        );
        assert_eq!(log, ["ask now=false"], "the pane is not opened");
        let (report, code) = super::doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &DevtoolsFacts {
                facts: Some(facts),
                did,
            },
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts::absent(),
        );
        assert_eq!(code, 0);
        assert!(
            report.contains("did nothing: aterm opened the pane 2 min ago"),
            "{report}"
        );

        // Past the ten minutes, the same run opens the page.
        let mut stale = Scripted::new(WindowAnswer::Unreachable);
        stale.opened_ago = Some(atpkg::devtools::REOPEN_AFTER_S + 1);
        let (_, log) = stale.with(|reach| {
            super::devtools_hand_over_when(true, absent.clone(), false, false, reach, || None)
        });
        assert!(log.iter().any(|entry| entry == "open"), "{log:?}");

        // `--read-only` and ssh: nothing asked, nothing opened.
        for (fix, over_ssh) in [(false, false), (true, true)] {
            let (_, log) = Scripted::new(WindowAnswer::Unreachable).with(|reach| {
                super::devtools_hand_over_when(fix, absent.clone(), over_ssh, false, reach, || None)
            });
            assert!(log.is_empty(), "fix={fix} ssh={over_ssh}: {log:?}");
        }
    }

    /// Facts for aterm not on the list: `list` as read, tccd's log silent.
    fn devtools_facts_with(list: aterm_containment::DevToolList) -> atpkg::devtools::Facts {
        use aterm_containment::consent::{DevToolProbe, DevToolProbeLabel};
        atpkg::devtools::Facts {
            subject: Some("com.aterm.aterm".to_string()),
            list,
            log: DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest),
            other_responsible: None,
        }
    }

    /// A list read whole, `rows` long, with aterm's row `listing`.
    fn read_list(
        listing: aterm_containment::DevToolListing,
        rows: u32,
    ) -> aterm_containment::DevToolList {
        aterm_containment::DevToolList {
            listing,
            label: aterm_containment::DevToolListLabel::Read,
            modified_at_s: Some(1),
            rows: Some(rows),
        }
    }

    /// `devtools_hand_over` asks the running window first and opens System Settings
    /// directly only when no window answers, the window is older than the verb, or
    /// `now` left the page open to `doctor`.
    #[test]
    fn doctor_asks_the_window_first_and_opens_only_where_the_window_leaves_it() {
        use super::{DEVTOOLS_WAIT_LINE, WindowAnswer};
        use aterm_containment::DevToolListing;
        use atpkg::devtools as dt;
        let skips = aterm_containment::SpikeEvidence::MEASURED.devtool_skips_first_run_check;
        let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
        let reply = |line: &str| WindowAnswer::Reply(line.to_string());
        let said = |log: &[String]| -> String {
            log.iter()
                .filter_map(|entry| entry.strip_prefix("say "))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let opened_words = dt::handled_words(&dt::Handled::Opened {
            listed_terminal: None,
            step: dt::Step::Add,
        });

        // No window: the whole banner, then the page, the marker and the wait.
        let (did, opened, log) = Scripted::new(WindowAnswer::Unreachable).run(&absent, true);
        assert!(opened);
        assert_eq!(did, format!("{opened_words}; you switched it on"));
        let banner = said(&log);
        for words in [
            dt::HELPER_TITLE,
            dt::why(skips),
            dt::HELPER_HOW,
            dt::only_aterm(false),
            dt::trade(skips),
            "Status: aterm Settings \u{25b8} Security",
            DEVTOOLS_WAIT_LINE,
        ] {
            assert!(banner.contains(words), "{words:?} in\n{banner}");
        }
        let at = |what: &str| {
            log.iter()
                .position(|entry| entry.starts_with(what))
                .unwrap_or_else(|| panic!("{what} in {log:?}"))
        };
        assert_eq!(log[0], "ask now=true", "a person here asks with `now`");
        assert!(
            at("say aterm doctor: Add") < at("open"),
            "the banner first: {log:?}"
        );
        assert!(
            at("open") < at("marker") && at("marker") < at("wait"),
            "{log:?}"
        );
        assert!(
            at("wait") < at("refresh"),
            "the switch read on: macOS is asked about aterm again: {log:?}"
        );
        // NEGATIVE CONTROL: a wait that never sees the switch asks macOS nothing.
        let mut unswitched = Scripted::new(WindowAnswer::Unreachable);
        unswitched.switched = false;
        let (did, _, log) = unswitched.run(&absent, true);
        assert!(log.contains(&"wait".to_string()), "{log:?}");
        assert!(!log.contains(&"refresh".to_string()), "{did}: {log:?}");

        // The window's panel is up: two lines naming it, then doctor's own open,
        // whatever the marker says (the window judged that).
        let mut shown = Scripted::new(reply("OK shown"));
        shown.opened_ago = Some(5);
        let (did, opened, log) = shown.run(&absent, true);
        assert!(opened, "{did}");
        let banner = said(&log);
        assert!(
            banner.contains("aterm's panel beside System Settings"),
            "{banner}"
        );
        assert!(
            !banner.contains(dt::HELPER_HOW),
            "the panel says how: {banner}"
        );
        assert!(log.contains(&"open".to_string()), "{log:?}");

        // No panel could be shown, or the window is older than the verb: the
        // whole banner and the page.
        for answer in ["OK unshown", "ERR unknown verb (try: help)"] {
            let (_, opened, log) = Scripted::new(reply(answer)).run(&absent, true);
            assert!(opened, "{answer}");
            assert!(said(&log).contains(dt::HELPER_HOW), "{answer}: {log:?}");
        }

        // No person here: the bare form, which the window holds for a quiet
        // moment; doctor opens nothing.
        let (did, opened, log) = Scripted::new(reply("OK queued")).run(&absent, false);
        assert!(!opened);
        assert_eq!(log, ["ask now=false"]);
        assert!(did.contains("at a quiet moment"), "{did}");

        // No window and no person: doctor's own ten minutes still hold.
        let mut looped = Scripted::new(WindowAnswer::Unreachable);
        looped.opened_ago = Some(120);
        let (did, opened, log) = looped.run(&absent, false);
        assert_eq!(
            (did.as_str(), opened),
            ("did nothing: aterm opened the pane 2 min ago", false)
        );
        assert_eq!(log, ["ask now=false"]);
        // No window and no person: the owner's Not now holds it too, as it holds
        // the window's own hand-over; a person here still gets the page.
        for answer in [
            WindowAnswer::Unreachable,
            reply("ERR unknown verb (try: help)"),
        ] {
            let mut declined_here = Scripted::new(answer.clone());
            declined_here.declined_at = Some(1_790_812_800); // 2026-10-01
            let (did, opened, log) = declined_here.run(&absent, false);
            assert_eq!(
                (did.as_str(), opened),
                (
                    "did nothing: you said Not now on 2026-10-01; aterm Settings \u{25b8} \
                     Security \u{25b8} Open Developer Tools\u{2026} opens the page",
                    false
                ),
                "{answer:?}"
            );
            assert_eq!(log, ["ask now=false"], "{answer:?}");
            let mut person_here = Scripted::new(answer);
            person_here.declined_at = Some(1_790_812_800);
            let (did, opened, _) = person_here.run(&absent, true);
            assert!(opened, "a person asked: {did}");
        }
        let (_, opened, log) = Scripted::new(WindowAnswer::Unreachable).run(&absent, false);
        assert!(opened);
        assert!(said(&log).is_empty(), "nobody to read a banner: {log:?}");
        assert!(
            !log.contains(&"wait".to_string()) && !log.contains(&"refresh".to_string()),
            "nobody to wait for: {log:?}"
        );

        // The request went out and no reply came. A person here asked with `now`,
        // which never opens the page: doctor opens it, with the whole banner. With
        // nobody here the bare form may still bring it up at a quiet moment, so
        // doctor opens nothing.
        let silent = || {
            Scripted::new(WindowAnswer::Unanswered(
                "did not answer within 10s".to_string(),
            ))
        };
        let (did, opened, log) = silent().run(&absent, true);
        assert!(opened, "{did}");
        assert!(said(&log).contains(dt::HELPER_HOW), "{log:?}");
        let (did, opened, log) = silent().run(&absent, false);
        assert!(
            !opened && did.contains("may still bring the page up"),
            "{did}"
        );
        assert_eq!(log, ["ask now=false"]);

        // `recent`: for `now` the window's own minute, which its last answer to a
        // `now` starts too, so no age read from the marker is claimed; for the
        // bare form the marker's age, or the window's word when the marker says
        // otherwise.
        for (person, opened_ago, words) in [
            (
                true,
                Some(30),
                "within the last minute aterm opened the page, or the aterm window",
            ),
            (
                true,
                None,
                "within the last minute aterm opened the page, or the aterm window",
            ),
            (
                false,
                Some(120),
                "did nothing: aterm opened the pane 2 min ago",
            ),
            (
                false,
                None,
                "the aterm window says aterm opened the page within the last 10",
            ),
        ] {
            let mut recent = Scripted::new(reply("OK recent"));
            recent.opened_ago = opened_ago;
            let (did, opened, log) = recent.run(&absent, person);
            assert!(!opened && did.contains(words), "{words:?} in {did}");
            assert_eq!(log.len(), 1, "asked, nothing else: {log:?}");
        }

        // `granted` or `unknown` with a person here contradicts doctor's own read
        // a moment ago (not granted): the window's verdict is older, so doctor
        // opens the page itself, with the whole banner. With nobody here they are
        // the window's word, and nothing opens.
        for answer in ["OK granted", "OK unknown"] {
            let (did, opened, log) = Scripted::new(reply(answer)).run(&absent, true);
            assert!(opened, "{answer}: {did}");
            assert!(said(&log).contains(dt::HELPER_HOW), "{answer}: {log:?}");
            let (did, opened, log) = Scripted::new(reply(answer)).run(&absent, false);
            assert!(
                !opened && did.starts_with("did nothing: "),
                "{answer}: {did}"
            );
            assert!(did.contains("last reading"), "{answer}: {did}");
            assert_eq!(log, ["ask now=false"], "{answer}");
        }

        // Everything else is the window's decision: nothing opens here.
        let mut declined = Scripted::new(reply("OK declined"));
        declined.declined_at = Some(1_790_812_800); // 2026-10-01
        for (script, words) in [
            (
                declined,
                "did nothing: you said Not now on 2026-10-01; aterm Settings \u{25b8} Security \
                 \u{25b8} Open Developer Tools\u{2026} opens the page",
            ),
            (Scripted::new(reply("OK unstable")), "ad hoc or dev build"),
            (Scripted::new(reply("OK listed")), "nothing to add"),
            (
                Scripted::new(reply("ERR headless: no page")),
                "the aterm instance doctor reached is headless",
            ),
            (
                Scripted::new(reply("ERR main thread stalled 4s; retry")),
                "stalled",
            ),
        ] {
            let (did, opened, log) = script.run(&absent, true);
            assert!(!opened, "{did}");
            assert!(did.starts_with("did nothing: "), "{did}");
            assert!(did.contains(words), "{words:?} in {did}");
            assert_eq!(log, ["ask now=true"], "{did}");
        }
    }

    /// A line the front door's CONNECT PHASE returned in place of the window's
    /// answer (`ERR auth`, `ERR control server busy`) means the request never
    /// went out: no window answered, so doctor takes the no-window path. A line
    /// the window wrote — `OK …` or an `ERR` of its own — is its answer; an
    /// error before the request was written is no window; only an error after it
    /// is the window's silence (`aterm_ctl::front_door_delivered`).
    #[test]
    fn a_refused_connect_is_no_window_and_a_reply_is_the_windows() {
        use super::{WindowAnswer, window_answer};
        for refused in [
            "ERR auth",
            "ERR auth; the server refused the token in /tmp/aterm/token; re-run",
            "ERR control server busy; retry",
        ] {
            assert_eq!(
                window_answer(Ok(refused.to_string())),
                WindowAnswer::Unreachable,
                "{refused}"
            );
        }
        for answered in [
            "OK queued",
            "OK recent",
            "ERR headless: a headless instance brings no System Settings page up",
            "ERR usage: devtools hand-over [now]",
            "ERR main thread stalled 4s; retry",
        ] {
            assert_eq!(
                window_answer(Ok(answered.to_string())),
                WindowAnswer::Reply(answered.to_string()),
                "{answered}"
            );
        }
        assert_eq!(
            window_answer(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "took no connection within 10 s; the request was not sent",
            ))),
            WindowAnswer::Unreachable
        );
    }

    /// The day a decline is dated: the local calendar day, or the UTC one said
    /// to be UTC when the zone could not be read.
    #[test]
    fn a_decline_is_dated_by_the_local_day() {
        let at = 1_790_812_800; // 2026-10-01T00:00:00Z
        assert_eq!(super::local_day_words(at, Some(0)), "2026-10-01");
        assert_eq!(super::local_day_words(at, Some(-7 * 3_600)), "2026-09-30");
        assert_eq!(
            super::local_day_words(at + 3_600, Some(2 * 3_600)),
            "2026-10-01"
        );
        assert_eq!(super::local_day_words(at, None), "2026-10-01 (UTC)");
    }

    /// The banner's words follow what doctor read and did: on an empty list
    /// `spctl` runs first and the banner says whose Terminal row that is; for an
    /// aterm listed switched off it asks for the switch, never a drag; and with
    /// the list unreadable (tccd's answer decided) nothing is waited for.
    #[test]
    fn the_doctor_banner_follows_the_list_it_read() {
        use super::WindowAnswer;
        use aterm_containment::consent::{DevToolProbe, DevToolProbeLabel, DevToolState};
        use aterm_containment::{DevToolList, DevToolListLabel, DevToolListing};
        use atpkg::devtools as dt;
        let said = |log: &[String]| -> String {
            log.iter()
                .filter_map(|entry| entry.strip_prefix("say "))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let empty = devtools_facts_with(read_list(DevToolListing::Absent, 0));
        let (_, opened, log) = Scripted::new(WindowAnswer::Unreachable).run(&empty, true);
        assert!(opened);
        let spctl = log.iter().position(|e| e == "spctl").expect("spctl ran");
        let banner = log
            .iter()
            .position(|e| e.starts_with("say aterm doctor: Add"))
            .expect("the banner");
        assert!(spctl < banner, "{log:?}");
        assert!(said(&log).contains(dt::only_aterm(true)), "{log:?}");
        // The window's panel is up (shown before `spctl` ran, so it cannot say
        // whose Terminal row that is): the short banner says it. NEGATIVE CONTROL:
        // with no `spctl`, the short banner speaks of no Terminal.
        let (_, opened, log) =
            Scripted::new(WindowAnswer::Reply("OK shown".to_string())).run(&empty, true);
        assert!(opened);
        assert!(said(&log).contains(dt::only_aterm(true)), "{log:?}");
        let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
        let (_, _, log) =
            Scripted::new(WindowAnswer::Reply("OK shown".to_string())).run(&absent, true);
        assert!(!said(&log).contains("Terminal"), "{log:?}");

        let off = devtools_facts_with(read_list(DevToolListing::Off, 2));
        let (_, _, log) = Scripted::new(WindowAnswer::Unreachable).run(&off, true);
        let banner = said(&log);
        assert!(banner.contains(dt::HELPER_SWITCH_ON), "{banner}");
        assert!(!banner.contains("Drag"), "aterm is listed: {banner}");
        assert!(!log.contains(&"spctl".to_string()), "{log:?}");

        let mut unread =
            devtools_facts_with(DevToolList::refused(DevToolListLabel::NoFullDiskAccess));
        unread.log = DevToolProbe {
            state: DevToolState::Unlisted,
            label: DevToolProbeLabel::Observed,
            requests: Some(3),
            not_allowed: Some(3),
            answered_at_ms: Some(1),
        };
        assert_eq!(unread.grant(), aterm_containment::DevToolGrant::NotGranted);
        let (_, opened, log) = Scripted::new(WindowAnswer::Unreachable).run(&unread, true);
        assert!(opened);
        assert!(said(&log).contains(dt::HELPER_HOW), "{log:?}");
        assert!(
            !log.contains(&"wait".to_string()),
            "an unread list is not waited on: {log:?}"
        );
    }

    /// §4.6 of the debugger-access design: the `debugger:` row shares
    /// `privacy:`'s property — it is never a `FAIL`, so `aterm doctor && aterm`
    /// launches whatever the rule says — and the one command it prints is the
    /// undo, from the consent module's constant, only once the rule is on.
    #[test]
    fn the_debugger_row_never_fails_and_prints_the_undo_only_when_on() {
        use DebuggerMember::{Admin, Developer, NotMember};
        use DebuggerMode::{Disabled, Enabled, Unrecognized};
        use SelfHardening::{Hardened, NotHardened};
        let unrecognised_words = DebuggerFacts {
            probe: DebuggerProbe {
                mode: DebuggerMode::Unknown,
                member: Admin,
                label: DebuggerProbeLabel::StatusUnrecognized,
            },
            hardening: Hardened,
        };
        for facts in [
            debugger(Enabled, Admin, Hardened),
            debugger(Enabled, Developer, NotHardened),
            debugger(Enabled, Admin, SelfHardening::Unknown),
            debugger(Disabled, Admin, Hardened),
            debugger(Disabled, Developer, NotHardened),
            debugger(Disabled, NotMember, NotHardened),
            debugger(Disabled, DebuggerMember::Unknown, Hardened),
            debugger(Unrecognized, Admin, Hardened),
            no_debugger(),
            unrecognised_words,
        ] {
            let (r, code) = doctor_checks(
                "/bin/sh",
                ShellCheck::Runs,
                &unmeasured(),
                &facts,
                &no_census(),
                &no_recovery(),
                &no_exceptions(),
            );
            assert_eq!(code, 0, "the debugger row never moves the exit code\n{r}");
            assert!(r.contains("health: OK"), "{r}");
            let line = r
                .lines()
                .find(|l| l.starts_with("debugger:"))
                .unwrap_or_else(|| panic!("no debugger: row in\n{r}"));
            assert_eq!(
                line.contains(DEBUGGER_ACCESS_REVERT),
                facts.probe.mode == Enabled,
                "{line}"
            );
            assert!(
                !line.contains("-enable") && !line.contains("sudo"),
                "{line}"
            );
        }
    }

    /// `Note` renders as its own word and is not a failure; the other two keep
    /// the spellings every script and screenshot already knows.
    #[test]
    fn the_three_marks_render_and_only_one_fails() {
        assert_eq!(Mark::Ok.render(), "ok");
        assert_eq!(Mark::Note.render(), "note");
        assert_eq!(Mark::Fail.render(), "FAIL");
        assert!(!Mark::Ok.is_fail() && !Mark::Note.is_fail() && Mark::Fail.is_fail());
        assert_eq!(Mark::from_ok(true), Mark::Ok);
        assert_eq!(Mark::from_ok(false), Mark::Fail);
    }

    #[test]
    fn show_face_success_path_emits_metrics() {
        // Pick any real, resolvable font and assert the key=value metrics shape.
        // A host with no resolvable font: COULD NOT RUN, never a pass.
        let family = aterm_render::list_fonts()
            .into_iter()
            .find(|f| aterm_render::face_info(f).is_some())
            .unwrap_or_else(|| aterm_spec::host::could_not_run("no resolvable system font"));
        let (r, code) = show_face_report(Some(&family));
        assert_eq!(code, 0, "{r}");
        for key in [
            "family=",
            "path=",
            "index=",
            "px=16\n",
            "cell_width=",
            "cell_height=",
            "baseline=",
            "glyph_count=",
        ] {
            assert!(r.contains(key), "show-face missing {key:?}\n{r}");
        }
    }

    #[test]
    fn list_fonts_report_shape() {
        let r = list_fonts_report();
        // Either the sentinel, or a newline-terminated list of non-empty lines that
        // matches the enumeration exactly (deterministic, scriptable).
        if r == "(no fonts found)\n" {
            aterm_spec::host::could_not_run("no fonts found on this host to list");
        }
        assert!(r.ends_with('\n'), "must be newline-terminated");
        let lines: Vec<&str> = r.lines().collect();
        assert!(lines.iter().all(|l| !l.is_empty()), "no empty lines");
        assert_eq!(
            lines,
            aterm_render::list_fonts(),
            "must mirror list_fonts()"
        );
    }

    #[test]
    fn tool_dispatch_never_shadows_aterm_surface() {
        // Absence, empty, and flags are never tool candidates (aterm owns those).
        assert!(!is_tool_candidate(None));
        assert!(!is_tool_candidate(Some("")));
        for flag in [
            "-h",
            "--help",
            "-V",
            "--version",
            "--sandbox",
            "--containment",
        ] {
            assert!(
                !is_tool_candidate(Some(flag)),
                "{flag} is a flag, not a tool"
            );
        }
        // Every front-door verb and `help` stay aterm's own; driven off the
        // roster so new verbs are covered automatically.
        for verb in Verb::ALL {
            assert!(
                !is_tool_candidate(Some(verb.name())),
                "{} is a front-door verb",
                verb.name()
            );
        }
        assert!(!is_tool_candidate(Some("help")));
        // Every diagnostic subcommand stays aterm's own (so `aterm doctor` is aterm's doctor,
        // never a co-distributed tool named "doctor").
        for (name, _) in DIAG_COMMANDS {
            assert!(
                !is_tool_candidate(Some(name)),
                "{name} is a diag subcommand"
            );
        }
        // Bare, tool-shaped operands are candidates. Whether they are actually installed is
        // decided later by the package dispatcher; this predicate only screens aterm's surface.
        for tool in ["ay", "ty", "trust", "trust-mc", "clean", "ny"] {
            assert!(
                is_tool_candidate(Some(tool)),
                "{tool} should be a candidate"
            );
        }
    }

    /// The launch flags, as `decide` reads them: the containment mode is
    /// carried through as typed (an empty or unknown one fails closed in the
    /// init funnel, never here), `--sandbox`/`--no-sandbox` are its two
    /// spellings, the last containment flag on the line wins, `-q`/`--verbose`
    /// compose with it, and a lone `--` ends the options.
    #[test]
    fn launch_flags_decide_the_run() {
        let run = |containment: Option<&str>, quiet: bool, verbose: bool| CliAction::Run {
            containment: containment.map(str::to_string),
            quiet,
            verbose,
        };
        let cases: &[(&[&str], CliAction)] = &[
            (&[], run(None, false, false)),
            (&["--containment", "user"], run(Some("user"), false, false)),
            (&["--containment=user"], run(Some("user"), false, false)),
            (&["--containment="], run(Some(""), false, false)),
            (&["--containment", "xyz"], run(Some("xyz"), false, false)),
            (&["--sandbox"], run(Some("containment"), false, false)),
            (&["--no-sandbox"], run(Some("user"), false, false)),
            (
                &["--sandbox", "--containment", "user"],
                run(Some("user"), false, false),
            ),
            (
                &["--containment", "user", "--sandbox"],
                run(Some("containment"), false, false),
            ),
            (
                &["--no-sandbox", "--sandbox"],
                run(Some("containment"), false, false),
            ),
            (&["-q"], run(None, true, false)),
            (&["--quiet"], run(None, true, false)),
            (&["--sandbox", "-q"], run(Some("containment"), true, false)),
            (
                &["--quiet", "--containment", "user"],
                run(Some("user"), true, false),
            ),
            (
                &["--verbose", "--sandbox"],
                run(Some("containment"), false, true),
            ),
            (&["--"], run(None, false, false)),
            (&["--sandbox", "--"], run(Some("containment"), false, false)),
        ];
        for (argv, want) in cases {
            assert_eq!(&decide(argv), want, "{argv:?}");
        }
    }

    /// A launch line `decide` cannot read is a usage error (exit 2), never a
    /// silent run: a trailing `--containment`, an unknown flag, an operand (a
    /// `-`-prefixed one after `--` included), and a diagnostic word after a
    /// flag (subcommands are first-operand only, git-style).
    #[test]
    fn malformed_launch_argv_is_a_usage_error() {
        for argv in [
            &["--containment"][..],
            &["--bogus"],
            &["--", "extra"],
            &["--", "--help"],
            &["--no-sandbox", "show-config"],
        ] {
            assert!(
                matches!(decide(argv), CliAction::Usage(_)),
                "{argv:?}: {:?}",
                decide(argv)
            );
        }
    }

    #[test]
    fn help_and_version_short_and_long() {
        for a in [["-h"], ["--help"]] {
            assert_eq!(decide(&a), CliAction::Help);
        }
        for a in [["-V"], ["--version"]] {
            assert_eq!(decide(&a), CliAction::Version);
        }
    }

    /// `--version`: identity line first (install.sh greps `^aterm `), then the
    /// origin line, then which copy runs in the updater's own words.
    #[test]
    fn version_text_names_the_running_copy_and_any_other() {
        use aterm_update::which_copy::{OtherCopy, Running, WhichCopy};
        let identity = format!(
            "aterm {}\n{}\n",
            aterm_types::version::app_version(),
            aterm_types::identity::ORIGIN_LINE
        );
        assert!(identity.contains("by Andrew Yates") && identity.contains("alab.systems"));
        let anchors = super::trust_anchors_line();
        assert!(
            anchors.starts_with("trusts: master=") && !anchors.contains("channel="),
            "{anchors}"
        );
        // The build line follows the identity once the binary installs its
        // stamp; the start probe greps the number. Installed for the whole test
        // binary, with UNSTAMPED's version so no other test's read moves.
        aterm_types::version::install(aterm_types::version::BuildStamp {
            build_number: "1789432052",
            ..aterm_types::version::BuildStamp::UNSTAMPED
        });
        let build = "build: 1789432052\n";
        assert_eq!(version_text(None), format!("{identity}{build}{anchors}"));
        let copy = WhichCopy {
            running: std::path::PathBuf::from("/Applications/aterm.app"),
            kind: Running::InstalledApp,
            others: vec![OtherCopy {
                path: std::path::PathBuf::from("/Users//ana/Applications/aterm.app"),
                version: Some("0.60.0".to_string()),
            }],
        };
        assert_eq!(
            version_text(Some(&copy)),
            format!(
                "{identity}{build}running: /Applications/aterm.app\nanother copy: \
                 /Users//ana/Applications/aterm.app (0.60.0) \u{2014} the updater leaves it \
                 alone\n{anchors}"
            )
        );
        assert!(version_text(Some(&copy)).starts_with("aterm "));
    }

    /// Per-mode limits: daily-driver modes inherit, confinement modes cap (the
    /// rule shared with `aterm-gui`'s spawn and the seam's `limits` contract).
    #[test]
    fn the_session_inherits_rlimits_in_user_and_caps_in_safety_and_containment() {
        use aterm_containment::ContainmentMode as Cm;
        use aterm_sandbox::Limits;
        assert_eq!(
            session_limits(Cm::User),
            Limits::inherit(),
            "user: must inherit"
        );
        assert_eq!(
            session_limits(Cm::Safety),
            Limits::shell_default(),
            "safety: soft caps"
        );
        assert_eq!(
            session_limits(Cm::Containment),
            Limits::contained(),
            "containment: binding caps"
        );
        assert_eq!(
            Limits {
                binding: false,
                ..Limits::contained()
            },
            Limits::shell_default(),
            "containment binds the same numbers safety sets"
        );
    }

    /// This process's soft and hard `RLIMIT_NOFILE`, read back from the kernel.
    #[cfg(unix)]
    fn current_nofile() -> (libc::rlim_t, libc::rlim_t) {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: a valid resource id and a valid out-param for the call.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) }, 0);
        (lim.rlim_cur, lim.rlim_max)
    }

    /// Read a pty master until the child hangs up (EOF, or EIO on Linux once the
    /// slave is closed), bounded by a deadline so a wedged probe fails the test
    /// instead of hanging the suite.
    #[cfg(unix)]
    fn read_until_hangup(master: i32) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut out = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            let mut p = libc::pollfd {
                fd: master,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd, 50 ms timeout.
            if unsafe { libc::poll(&mut p, 1, 50) } > 0 {
                // SAFETY: `read` fills at most `buf.len()` bytes of this owned buffer.
                let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
                match usize::try_from(n) {
                    Ok(0) => break,
                    Ok(n) => out.extend_from_slice(&buf[..n]),
                    Err(_) => {
                        let errno = std::io::Error::last_os_error().raw_os_error();
                        if errno != Some(libc::EAGAIN) && errno != Some(libc::EINTR) {
                            break; // EIO: the slave side is gone
                        }
                    }
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the probe shell did not hang up within 60 s; got {:?}",
                String::from_utf8_lossy(&out)
            );
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// End-to-end check that `ulimit -n` in a spawned `/bin/sh` matches the
    /// parent's soft limit in `User` mode, can be raised above the soft cap in
    /// `Safety` mode, and is hard-capped in `Containment` mode.
    #[cfg(unix)]
    #[test]
    fn the_session_child_sees_the_posture_of_its_mode() {
        use aterm_containment::ContainmentMode as Cm;

        // SAFETY: a trusted test entry point, before any untrusted input flows.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

        // The output lines of `script` run by `/bin/sh` in a `mode` session.
        let child_lines = |mode: Cm, script: &str| -> Vec<String> {
            let exec: Vec<std::ffi::OsString> = vec!["/bin/sh".into(), "-c".into(), script.into()];
            let shell = aterm_pty::spawn_shell_with_pid(
                24,
                80,
                &spawn_cap,
                &sandbox_cap,
                &[],
                None, // shell_override
                None, // shell_args
                None, // argv_override
                Some(&exec),
                None, // cwd
                None, // sandbox_wrap
                session_limits(mode),
            )
            .expect("the probe shell must spawn");
            let out = read_until_hangup(shell.master);
            // SAFETY: reaping our own child; the master is ours to close.
            unsafe {
                let mut status = 0;
                libc::waitpid(shell.pid, &mut status, 0);
                libc::close(shell.master);
            }
            out.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        };
        let child_ulimit = |mode: Cm| -> String {
            child_lines(mode, "ulimit -n")
                .into_iter()
                .next()
                .unwrap_or_default()
        };
        let render = |v: libc::rlim_t| {
            if v == libc::RLIM_INFINITY {
                "unlimited".to_string()
            } else {
                v.to_string()
            }
        };

        let (soft, hard) = current_nofile();
        assert_eq!(
            child_ulimit(Cm::User),
            render(soft),
            "user mode: the child keeps the parent's soft limit"
        );
        let cap = aterm_sandbox::Limits::shell_default()
            .open_files
            .expect("the hardened set caps open files");
        assert_eq!(
            child_ulimit(Cm::Safety),
            render(core::cmp::min(cap as libc::rlim_t, hard)),
            "safety mode: the child gets the cap, clamped to the inherited hard ceiling"
        );

        // `ulimit -n` in sh sets the soft and the hard limit.
        let raise = cap + 1;
        assert!(
            hard > raise as libc::rlim_t,
            "test precondition: the inherited hard NOFILE ({hard}) must exceed {raise}"
        );
        let script = format!("ulimit -n {raise} 2>/dev/null; ulimit -n; ulimit -Hn");
        assert_eq!(
            child_lines(Cm::Safety, &script),
            [raise.to_string(), raise.to_string()],
            "safety mode: the cap is a soft guardrail the shell's rc can raise"
        );
        assert_eq!(
            child_lines(Cm::Containment, &script),
            [cap.to_string(), cap.to_string()],
            "containment mode: the contained program raised its cap back"
        );
    }
}
