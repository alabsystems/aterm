// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm`'s front door: `--help`, the verb roster, the diagnostic subcommands
//! (`doctor`, `show-config`, …) and the plain PTY shell session.
// The setup run's ledger carries `aterm_spec_anchor::refines` anchors, all in macOS code.
#![cfg_attr(
    all(anchor_rules, target_os = "macos", any(test, debug_assertions)),
    feature(macro_attr)
)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

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

/// `aterm doctor`'s own rows as converge items; `crates/aterm` composes them with the packages'
/// repair items and `aterm pkg doctor`.
pub mod doctor;
mod manual;
mod spotlight;

pub mod amacs_launch;

/// The git-and-friends shim dispatch: what the one `aterm` binary does run as `git`, `cc`, … on
/// a Mac whose Command Line Tools are still installing.
pub mod clt_shim;

mod cli_text;

/// The macOS fleet settings profile, also used by the first interactive launch.
#[cfg(target_os = "macos")]
pub mod macos_doctor;

mod theme_author;
mod theme_cmd;
mod theme_pack_cmd;
pub use theme_cmd::main_entry as theme_main_entry;
pub use theme_cmd::{ThemeHost, ThemeListFormat, ThemeListRequest, list_themes_alias};

mod front_door;
pub use front_door::{AliasRoute, alias_route, asks_completions, mode_free};

mod windowing;

pub use windowing::{
    LaunchEnv, LaunchIntent, SplitOrientation, WindowRequest, WindowRoute, WindowingBehavior,
    parse_window_request, plain_launch_dir_operand, plain_launch_is_policy_eligible, route_launch,
    window_verb_usage,
};

const HELP_TITLE: &str = "aterm — a transparent, introspecting terminal\n";
/// What a plain `aterm` session runs, per platform, for `--help` ([`HELP_SESSION_LINE`]) and
/// `aterm help aterm`.
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
/// The `--help` line under the origin line.
#[cfg(not(windows))]
const HELP_SESSION_LINE: &str = concat!(
    "Runs ",
    session_shell!(),
    " and passes its bytes through unchanged.\n"
);
/// See the Unix twin above; the wrap differs, and it adds the in-tab rule (`nests_inside_aterm`).
#[cfg(windows)]
const HELP_SESSION_LINE: &str = concat!(
    "Runs ",
    session_shell!(),
    " and passes its\nbytes through unchanged. ",
    "A bare `aterm` typed in an aterm tab opens a new tab\n",
    "there; `aterm --session` nests a session instead.\n"
);
const HELP_HEAD: &str = concat!(
    "\n",
    "`aterm ctl` reads and drives the window (`aterm --window`, or `--headless`\n",
    "for no window); a plain `aterm` shell session serves no control socket.\n",
    "\n",
    "USAGE:\n",
    "    aterm [OPTIONS]           Start an interactive shell session (the default\n",
    "                              with a TTY; with no TTY, aterm opens the window).\n",
    "    aterm --window [args]     Open the GPU window (`aterm --window --help` lists\n",
    "                              its flags); --session forces the shell session,\n",
    "                              e.g. for piped or CI runs.\n",
    "    aterm help [topic]        The manual: what aterm is, how to drive it, and\n",
    "                              every tool. Inside an aterm tab it prints the\n",
    "                              agent operating brief. Start here.\n",
    "    aterm <verb> [args]       A verb (see VERBS) or a tool (see TOOLCHAIN).\n",
    "    aterm <SUBCOMMAND>        Print diagnostics and exit; `doctor` also\n",
    "                              repairs (see SUBCOMMANDS).\n",
    "\n",
    "OPTIONS:\n",
    "        --containment <MODE>  user (the default: no sandbox, no extra limits),\n",
    "        --containment=<MODE>  safety (capped limits, no OS sandbox) or\n",
    "                              containment (= --sandbox). The last flag wins;\n",
    "                              an invalid value fails closed to containment.\n",
    "        --sandbox             Shorthand for --containment containment, the\n",
    "                              macOS sandbox: no network, writes only to temp\n",
    "                              dirs and shell history, no credential stores or\n",
    "                              mach services (`aterm explain-config` has the\n",
    "                              rest). Refused off macOS.\n",
    "        --no-sandbox          Shorthand for --containment user (the default).\n",
    "        --no-reroute          Restore the upstream names (cargo, rustc, z3, …)\n",
    "                              for this launch; see `aterm help reroute`.\n",
    "    -q, --quiet               Hide the session's startup line.\n",
    "        --verbose             On exit, print to stderr how many bytes of output\n",
    "                              passed through.\n",
    "        --completions <SHELL> Print the bash, zsh or fish completion script\n",
    "                              and exit.\n",
    "    -h, --help                Print this help and exit.\n",
    "    -V, --version             Print the version and exit.\n",
    "\n",
    "SUBCOMMANDS (each exits without starting a shell):\n",
    "    show-config               Print the version, shell, TERM, size and\n",
    "                              containment default.\n",
    "    explain-config            Explain how aterm resolves its configuration.\n",
    "    doctor                    Check health, instances, the fabric broker and the\n",
    "                              packages, and repair what aterm lays; --read-only\n",
    "                              changes nothing. --check, --apply, --restore: the\n",
    "                              macOS fleet settings profile alone.\n",
    "    list-fonts                List available font families.\n",
    "    show-face <family>        Show a font family's file and cell metrics.\n",
    "    list-themes               List the built-in and custom themes (the same as\n",
    "                              `aterm theme list`).\n",
    "    list-kitty-commands       List the words the cursor cat obeys, by language.\n",
    "    exceptions [--all]        List what needs a look later and the newest ten\n",
    "                              filed entries (--all: every filed entry).\n",
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
    /// `aterm drive` — the window harness's readers (`classify`, `phase`, `answer`, `ledger`,
    /// `tape`, `replay`).
    Drive,
    /// `aterm link` — the fabric bridge (`serve`, `ls`, `notify`, `broker`, `mint`, `fabric`).
    Link,
    /// `aterm fabric` — fabric state, live traffic, and `on|off|doctor`.
    Fabric,
    /// `aterm ship` — the release tool (publishing; source checkouts only).
    Ship,
    /// `aterm update` — the headless update lane (`status`, `check`, `identity`; on Linux also
    /// `enable`, `apply`, `rollback`).
    Update,
    /// `aterm agents` — the coding-agent primer installer.
    Agents,
    /// `aterm ai` — AI CLI logins, doctor, cross-AI reviews and the signed fleet document.
    Ai,
    /// `aterm machines` — macOS lab configuration over SSH.
    Machines,
    /// `aterm init` — configure the current macOS login and optional GitHub bootstrap.
    Init,
    /// `aterm secrets` — the credentials vault, its keyless server and `~/.secrets`
    /// (docs/DESIGN-secrets-2026-10-20.md).
    Secrets,
    /// `aterm db` — embedded replicated application databases, offline and LAN.
    Db,
    /// `aterm harness` — Claude Code harness read views, accounts roster, and live upgrade.
    Harness,
    /// `aterm keeper` — the PTY keeper (`start | stop | status | serve`).
    Keeper,
    /// `aterm acaffiene` — the macOS menu-bar keep-awake helper.
    Acaffiene,
    /// `aterm theme` — list, check, create, export, import, pack and install themes.
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
        Verb::Db,
        Verb::Harness,
        Verb::Keeper,
        Verb::Acaffiene,
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
            Verb::Db => "db",
            Verb::Harness => "harness",
            Verb::Keeper => "keeper",
            Verb::Acaffiene => "acaffiene",
            Verb::Theme => "theme",
            Verb::Edit => "edit",
            Verb::NewTab => "new-tab",
            Verb::NewWindow => "new-window",
            Verb::SplitPane => "split-pane",
        }
    }

    /// The argv0 alias this verb also answers to, if any (a symlink onto the one binary).
    #[cfg(test)]
    const fn argv0_alias(self) -> Option<&'static str> {
        match self {
            Verb::Ctl => Some("aterm-ctl"),
            Verb::Conn => None,
            Verb::ResourceRun => None,
            Verb::Pkg => Some("atpkg"),
            Verb::Agh => None,
            Verb::Fleet => Some("aterm-fleet"),
            Verb::Drive => Some("aterm-drive"),
            Verb::Link => Some("aterm-link"),
            Verb::Fabric => None,
            Verb::Edit => Some("amacs"),
            Verb::Ship
            | Verb::Update
            | Verb::Agents
            | Verb::Ai
            | Verb::Machines
            | Verb::Init
            | Verb::Secrets
            | Verb::Db
            | Verb::Harness
            | Verb::Keeper
            | Verb::Acaffiene
            | Verb::Theme
            | Verb::NewTab
            | Verb::NewWindow
            | Verb::SplitPane => None,
        }
    }

    /// Whether this is a windowing verb ([`parse_window_request`] parses it, [`route_launch`]
    /// routes it).
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
            | Verb::Db
            | Verb::Harness
            | Verb::Keeper
            | Verb::Acaffiene
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
            Verb::Init => {
                "aterm init [--plan|--check] [--json] [--develop|--no-develop] [--only <items>]"
            }
            Verb::Secrets => "aterm secrets [<cmd>]",
            Verb::Db => "aterm db <cmd> --dir <database>",
            Verb::Harness => "aterm harness <cmd>",
            Verb::Keeper => "aterm keeper <cmd>",
            Verb::Acaffiene => "aterm acaffiene [<cmd>]",
            Verb::Theme => "aterm theme <cmd>",
            Verb::Edit => "aterm edit [+L[:C]] <file>...",
            Verb::NewTab => "aterm new-tab [-d dir]",
            Verb::NewWindow => "aterm new-window [-d dir]",
            Verb::SplitPane => "aterm split-pane [-d dir]",
        }
    }

    /// The `--help` description, one entry per rendered line.
    pub const fn blurb(self) -> &'static [&'static str] {
        match self {
            Verb::Ctl => &[
                "Read and drive the sessions of an aterm window:",
                "screen, keys, turns, events and frames.",
            ],
            Verb::Conn => &[
                "Wire which sessions pull from or push to each",
                "other (ls | add | set | rm | spawn | show | map).",
            ],
            Verb::ResourceRun => &[
                "Run a command with its CPU and memory charged to",
                "a tab (--session @self --job LABEL [--ai WORD]",
                "-- PROGRAM [ARGS...]).",
            ],
            Verb::Pkg => &["Install, update and verify the toolchain."],
            Verb::Agh => &[
                "GitHub reads as the routed identity: JSON out,",
                "typed exits (`aterm agh help`).",
            ],
            Verb::Fleet => &[
                "This machine's membership in a fleet, the aterms",
                "of one LAN that read each other (init | join |",
                "peers | allow | leave); `allow drive --from",
                "<host|node>` lets that node drive this machine",
                "(`aterm ctl --host <h> …`, `aterm help workers`).",
                "Unix only.",
            ],
            Verb::Link => &[
                "The fabric bridge: carries inbox and post traffic",
                "between this instance and the bus. Unix only.",
            ],
            Verb::Fabric => &[
                "The fabric: mail between sessions and people",
                "(status | tail | brief | on | off | doctor |",
                "mint-for | halt | inbox | post); `status` is the",
                "default. Unix only.",
            ],
            Verb::Drive => &[
                "The window harness's readers, run by hand",
                "(classify | phase | answer | ledger | tape |",
                "replay); workers and reads are `aterm ctl`'s.",
            ],
            Verb::Ship => &[
                "Publish aterm: provision a signing machine, cut",
                "and release a build. Needs a source checkout.",
            ],
            Verb::Update => &[
                "Report or check for an update, or print this",
                "build's identity (status | check | identity;",
                "on Linux also enable | apply | rollback).",
            ],
            Verb::Agents => &[
                "Keep the aterm primer in coding agents' context",
                "files (status | install | remove | primer).",
            ],
            Verb::Ai => &[
                "This machine's AI CLIs (claude, codex, agy, muse,",
                "grok): one working login each (status | doctor |",
                "login), reviews one AI asks of another (review),",
                "and the signed fleet document (fleet).",
            ],
            Verb::Machines => &[
                "Configure and manage a lab of Macs over SSH from",
                "this one (init | scan | enroll | status | plan |",
                "apply | person | screen …). macOS.",
            ],
            Verb::Init => &[
                "Restore aterm's commands and shell integration",
                "(over SSH too); at this Mac's console, set its",
                "keyboard, scrolling, widgets, Dock and accounts.",
                "Join the secrets vault; clone your own GitHub",
                "repositories. Current user. Linux: the base,",
                "vault and repositories only.",
                "`--develop`: fast-forward recorded checkouts and",
                "provision their test tools, no settings.",
            ],
            Verb::Secrets => &[
                "One credentials file for every machine: the",
                "vault, its server and ~/.secrets (status | join |",
                "sync | edit | approve | serve …); `status` is the",
                "default. Unix only.",
            ],
            Verb::Db => &[
                "Encrypted embedded application databases, local",
                "and replicated directly between LAN peers",
                "(init | request | invite | join | get | put |",
                "scan | stop | budget-create | spend | sync | serve).",
            ],
            Verb::Harness => &[
                "Claude Code's spend, limits and accounts, the",
                "disk, the ledger (usage | limits | accounts |",
                "disk | ledger); `upgrade` moves a live Claude Code",
                "onto a newer build. The window supervises.",
            ],
            Verb::Keeper => &[
                "The PTY keeper holds every terminal's master, so",
                "a crashed window's shells live on (start | stop |",
                "status | serve). macOS only, opt-in.",
            ],
            Verb::Acaffiene => &[
                "The menu-bar keep-awake mug: a click keeps this",
                "Mac awake, a right click picks 1, 8 or 24 hours or",
                "forever (on | off | status | startup …). macOS.",
            ],
            Verb::Theme => &[
                "Colour themes (list | new | check | export |",
                "schema | import | pack | install); import converts",
                "kitty, Ghostty, Alacritty, WezTerm, iTerm2,",
                "Terminal.app, Windows Terminal and base16/base24.",
            ],
            Verb::Edit => &[
                "amacs, aterm's editor: inside aterm, open a file",
                "in an Editor tab beside this one and wait until it",
                "is closed; elsewhere, edit it in the terminal",
                "(`amacs` is the same command, for $EDITOR).",
            ],
            Verb::NewTab => &[
                "Open a tab: a new window by default, or a tab in",
                "the running aterm when windowing_behavior is",
                "\"attach\".",
            ],
            Verb::NewWindow => &[
                "Open a new window, never a tab in the running",
                "aterm, whatever windowing_behavior says.",
            ],
            Verb::SplitPane => &[
                "Split the focused pane (-V side by side, the",
                "default; -H stacked); a new window unless",
                "windowing_behavior is \"attach\".",
            ],
        }
    }

    /// Resolve an `argv[1]` operand to a verb.
    pub fn from_operand(operand: &str) -> Option<Verb> {
        Verb::ALL.iter().copied().find(|v| v.name() == operand)
    }
}

const VERB_BLURB_COLUMN: usize = 30;

fn verb_help_block(verb: Verb) -> String {
    let mut out = String::new();
    let mut lines = verb.blurb().iter();
    let width = VERB_BLURB_COLUMN - 4;
    if verb.usage().len() >= width {
        out.push_str(&format!("    {}\n", verb.usage()));
    } else {
        let first = lines.next().unwrap_or(&"");
        out.push_str(&format!("    {:<width$}{first}\n", verb.usage()));
    }
    for line in lines {
        out.push_str(&format!("{:width$}{line}\n", "", width = VERB_BLURB_COLUMN));
    }
    out
}

fn help_text() -> String {
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

const HELP_TAIL: &str = concat!(
    "\n",
    "TOOLCHAIN (`aterm help pkg` has the manual):\n",
    "    aterm <tool> [args]       Run a tool from the managed store (never $PATH),\n",
    "                              e.g. `aterm ay`, `aterm ty`, `aterm trustc`;\n",
    "                              `aterm pkg install <program>` adds one.\n",
    "\n",
    "EXAMPLES:\n",
    "    aterm                              Start an interactive shell.\n",
    "    aterm --sandbox                    The same shell under the macOS sandbox.\n",
    "    aterm --containment safety         Capped limits, no OS sandbox.\n",
    "    aterm doctor --read-only           Check this machine; change nothing.\n",
);

/// The diagnostic subcommands and their `--help` lines (`aterm list-themes --help` prints
/// `aterm theme list`'s help instead): `aterm <name>` reports and exits without starting a shell.
pub const DIAG_COMMANDS: &[(&str, &str)] = &[
    (
        "show-config",
        "Print the version, $SHELL as set (on Windows, this tab's shell or a new window's, and shell_origin=), TERM, the size (24x80 when stdout is not a terminal) and the default containment mode as key=value lines.",
    ),
    (
        "explain-config",
        "Print where aterm.toml is, how the containment mode is chosen, and the platform's own keys.",
    ),
    (
        "doctor",
        "Check this machine and repair what aterm lays (`aterm doctor --help` lists its forms).",
    ),
    ("list-fonts", "List available font families, one per line."),
    (
        "show-face",
        "Show a font family's file and cell metrics as key=value lines.",
    ),
    (
        "list-themes",
        "List the built-in themes and the themes folder's (`aterm theme list`).",
    ),
    (
        "list-kitty-commands",
        "List the words the cursor cat obeys when typed, by language.",
    ),
    (
        "exceptions",
        "List what needs a look later and the newest ten filed entries; --all lists every filed entry in the log's newest MiB.",
    ),
];

/// `aterm <cmd> --help` for a diagnostic: `aterm doctor`'s grammar and its five forms
/// ([`doctor::help`]), else the command's description and usage.
fn diag_help_text(cmd: &str) -> String {
    if cmd == "doctor" {
        return doctor::help();
    }
    let desc = DIAG_COMMANDS
        .iter()
        .find(|(name, _)| *name == cmd)
        .map(|(_, d)| *d)
        .unwrap_or("");
    format!("{cmd} — {desc}\nusage: {}\n", diag_usage(cmd))
}

fn diag_usage(cmd: &str) -> String {
    match cmd {
        "show-face" => format!("aterm {cmd} <family>"),
        "doctor" => format!("aterm {cmd} {}", doctor::GRAMMAR),
        "exceptions" => format!("aterm {cmd} [--all]"),
        _ => format!("aterm {cmd}"),
    }
}

const SHOW_FACE_NEEDS_FAMILY: &str =
    "aterm show-face: needs a font family — usage: aterm show-face <family>";

fn diag_report(cmd: &str, arg: Option<&str>) -> Option<(String, i32)> {
    match cmd {
        "show-config" => Some((show_config_report(), 0)),
        "explain-config" => Some((explain_config_report(), 0)),
        "list-fonts" => Some((list_fonts_report(), 0)),
        "show-face" => Some(show_face_report(arg)),
        "list-kitty-commands" => Some((list_kitty_commands_report(), 0)),
        "exceptions" => Some((exceptions_report(arg == Some("--all")), 0)),
        _ => None,
    }
}

fn show_config_report() -> String {
    let (rows, cols) = driver::host_winsize();
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let or = |s: String, dflt: &str| if s.is_empty() { dflt.to_string() } else { s };
    let mut out = String::new();
    out.push_str(&format!(
        "version={}\n",
        aterm_types::version::app_version()
    ));
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

fn explain_config_report() -> String {
    let mut out = String::new();
    out.push_str("aterm configuration resolution\n\n");
    out.push_str(&config_path_paragraph());
    out.push_str(
        "Containment mode: the last of --containment <mode>, --sandbox and --no-sandbox\n\
         on the command line, else user. No aterm.toml key or environment variable\n\
         sets it. A malformed value fails closed to containment.\n\n",
    );
    out.push_str("Containment modes (least → most capability):\n");
    out.push_str(
        "  containment  The OS sandbox (macOS; refused elsewhere): no network or debugger\n\
         \x20              attach; writes only to temp dirs and shell history; no credential\n\
         \x20              stores or private data; no mach service lookups but user and\n\
         \x20              group queries, so open, osascript, pbcopy and the keychain do\n\
         \x20              nothing. launchd still restarts, unloads or disables the\n\
         \x20              user's own jobs when asked. Safety's caps, as hard limits.\n",
    );
    out.push_str("  safety       Soft resource caps (binding on Windows), no OS sandbox.\n");
    out.push_str("  user         No sandbox, no extra limits (the default).\n");
    #[cfg(windows)]
    out.push_str(WINDOWS_CONFIG_PARAGRAPH);
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
     \x20 rosetta                 true (default) | false. On Apple silicon, true installs\n\
     \x20                         Rosetta 2 when it is missing, as you and with no admin\n\
     \x20                         password (`softwareupdate --install-rosetta`); false never\n\
     \x20                         does. An Intel Mac ignores it.\n\
     \x20 The table is applied by `aterm pkg machine apply`, which aterm runs once a day\n\
     \x20 at the first window or interactive terminal session, and by a package pass\n\
     \x20 (update, seed, install) when the table changed since it was last applied. With\n\
     \x20 [packages] enabled = false no pass runs, so an edit waits for the daily apply.\n\
     \x20 `aterm pkg machine` reads the measured state. Settings ▸ Security sets the\n\
     \x20 keys, shows that state and, while something is unapplied, offers Apply Now.\n\
     \x20 An apply is refused, and says so, under a redirected $HOME (`defaults` writes\n\
     \x20 the account's per-host domain whatever $HOME says) or while aterm.toml does\n\
     \x20 not parse (`aterm --window --validate-config` says why). To undo Universal\n\
     \x20 Control, run the revert line `aterm pkg machine` prints (`defaults -currentHost\n\
     \x20 delete com.apple.universalcontrol Disable`, then the same for DisableMagicEdges)\n\
     \x20 AND set universal_control = \"leave\", or the next apply turns it off again.\n";

/// The `[privacy]` paragraph of `explain-config`.
#[cfg(target_os = "macos")]
const PRIVACY_CONFIG_PARAGRAPH: &str = "\n\
     [privacy] — macOS consent (aterm.toml; the window reads it, `aterm doctor` only `observer`):\n\
     \x20 enabled / check         the silent, prompt-free probes: Full Disk Access (opens your\n\
     \x20                         TCC.db unread, or the system one where there is none per\n\
     \x20                         user), the debugger rule (`DevToolsSecurity -status`) and the\n\
     \x20                         Developer Tools list (tccd's log, and its switch in the system\n\
     \x20                         TCC.db, read-only). They raise NO dialog. Off, every field\n\
     \x20                         reads `unknown`, which is not `denied`; the report says which.\n\
     \x20 notice                  System State's access steps for an approved setup plan, only\n\
     \x20                         while System State is the visible view, one macOS gesture at a\n\
     \x20                         time: the Local Network question; while Full Disk Access reads\n\
     \x20                         off, System Settings at Privacy & Security \u{25b8} Full Disk Access\n\
     \x20                         with aterm's panel beside it (its icon to drag into the list,\n\
     \x20                         I've switched it on, and Not now); the Developer Tools page;\n\
     \x20                         and once per Mac and account debugger access (macOS's developer\n\
     \x20                         mode), behind macOS's own password dialog. Each page comes once\n\
     \x20                         per approval, never on a build whose grant would not last (ad\n\
     \x20                         hoc or unsigned); Not now holds until Permissions is set up\n\
     \x20                         again. aterm presses nothing there: switching it on is yours.\n\
     \x20                         Later, a grant aterm verified that reads off again (found at\n\
     \x20                         launch, or when macOS shows its \"access data from other apps\"\n\
     \x20                         dialog for a program of yours) puts one line on aterm's\n\
     \x20                         menu-bar menu, \"System State: Full Disk Access needs you\",\n\
     \x20                         which opens System State; nothing is filed in Messages, and\n\
     \x20                         Not now on that step keeps the line dark until Permissions is\n\
     \x20                         set up again. Off pauses the steps with no answer\n\
     \x20                         recorded, and they go on once it is on again; no key makes\n\
     \x20                         aterm start one.\n\
     \x20 report_attribution      per-session corroboration of the app macOS holds responsible\n\
     \x20                         for each session; aterm's own adoption record decides whether\n\
     \x20                         a session reads `live` or `adopted`.\n\
     \x20 observer                absent: a narrow sentinel watches for the log line macOS writes\n\
     \x20                         when it shows a consent dialog; it decides when to ask, and any\n\
     \x20                         other dialog it can name becomes a record in Messages naming\n\
     \x20                         the tab that asked (with notice on). false: off, which also\n\
     \x20                         stops the Developer Tools row's reads (tccd's log and the\n\
     \x20                         switch) in the window and both doctors. true: the full stream,\n\
     \x20                         the tab mark and a banner per dialog. enabled = false turns\n\
     \x20                         every mode off. Config only; nothing in a session can set it.\n\
     \x20 warmup                  \"automatic\" (default) | \"on-request\" | \"never\". The warm-up\n\
     \x20                         asks macOS for the folders up front, which RAISES the dialogs\n\
     \x20                         on purpose, while you are at the screen rather than inside an\n\
     \x20                         agent's blocked read. automatic: the button in Settings, and\n\
     \x20                         at launch while Full Disk Access reads off, once the Full Disk\n\
     \x20                         Access page was left without you acting on it (Not now, or its\n\
     \x20                         panel lapsed) or was not offered, as System State's last access\n\
     \x20                         step, parked whenever System State is not the visible view.\n\
     \x20                         Acting on that page (switching it on, dropping the icon) asks\n\
     \x20                         for no folder that launch; once every folder has an answer, no\n\
     \x20                         launch asks again. on-request: only the button. never: neither.\n\
     \x20                         No program inside a session can start it.\n\
     \x20 warmup_folders          what the warm-up asks for. Default: Documents, Desktop and\n\
     \x20                         Downloads. `app-data` (every other app's own data) is a valid\n\
     \x20                         name, not in the default and never asked at launch: macOS\n\
     \x20                         forgets that answer when the process that received it is\n\
     \x20                         replaced. Network and removable volumes have no name here,\n\
     \x20                         and whether Full Disk Access reaches them is unmeasured\n\
     \x20                         (`aterm ctl privacy`).\n\
     \x20 warmup_hold_ms          how long a pending update waits for a running warm-up\n\
     \x20                         (default 120000, at most 600000).\n\
     \x20 probe_interval_ms       the shortest gap between access re-checks (default 5000);\n\
     \x20                         `aterm ctl @<sid> await consent` answers when a check's result\n\
     \x20                         differs from the first one it read.\n\
     \x20 protected_roots         the sensitive set; empty = the containment tier's own list, so\n\
     \x20                         the two tiers cannot disagree about which paths are sensitive.\n\
     \x20 auto_accept             reserved, not implemented: aterm never answers a macOS consent\n\
     \x20                         dialog. Granting Full Disk Access in Settings is the answer,\n\
     \x20                         and only a person can give it.\n\
     \x20 Settings \u{25b8} Security \u{25b8} Ask Again clears macOS's saved refusals for the folders it\n\
     \x20 lists, after you confirm. Every key here applies once the file is saved.\n";

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

const EXCEPTIONS_TAIL_BYTES: u64 = 1024 * 1024;

struct ExceptionsRead {
    /// The replayed records.
    log: aterm_messages::MessageLog,
    /// Lines the codec refused (torn, foreign, corrupt).
    skipped: usize,
    /// Whether either file was there to read.
    found: bool,
    /// The path that is there but unread: `messages.log`, or its rotated `.1` when nothing
    /// was read.
    unreadable: Option<Unreadable>,
}

struct Unreadable {
    path: std::path::PathBuf,
    /// A symlink or a folder (never followed), rather than a file that would not open.
    not_a_file: bool,
}

impl Unreadable {
    fn why(&self) -> String {
        let shown = self.path.display();
        if self.not_a_file {
            format!("{shown} is a symlink or folder, not the log")
        } else {
            format!("{shown} cannot be opened (no read permission)")
        }
    }

    fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }
}

fn exceptions_log_path() -> Option<std::path::PathBuf> {
    aterm_types::dirs::logs_dir().map(|dir| dir.join("messages.log"))
}

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
        text.split_once('\n')
            .map_or_else(String::new, |(_, rest)| rest.to_string())
    } else {
        text
    })
}

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

fn local_offset_s(unix_s: i64) -> i64 {
    local_offset_at(unix_s).unwrap_or(0)
}

fn local_offset_at(unix_s: i64) -> Option<i64> {
    aterm_uds::zone::local_offset_at(unix_s)
}

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

/// How many exceptions are open, and the titles of the newest [`EXCEPTIONS_SHOWN`].
type ExceptionsFacts = Result<(usize, Vec<String>), String>;

/// How many open exceptions the `exceptions:` row names by title.
const EXCEPTIONS_SHOWN: usize = 3;

fn exceptions_facts() -> ExceptionsFacts {
    let path = exceptions_log_path().ok_or_else(|| "no log folder".to_string())?;
    let read = read_exceptions_log(&path);
    if let Some(unreadable) = &read.unreadable {
        return Err(format!("{} cannot be read", unreadable.name()));
    }
    let open: Vec<&aterm_messages::log::LogRecord> = read
        .log
        .records()
        .rev()
        .filter(|r| r.later && r.is_live())
        .collect();
    Ok((
        open.len(),
        open.iter()
            .take(EXCEPTIONS_SHOWN)
            .map(|r| r.title.clone())
            .collect(),
    ))
}

fn exceptions_row(facts: &ExceptionsFacts) -> (Mark, String) {
    match facts {
        Err(why) => (Mark::Note, format!("not measured ({why})")),
        Ok((0, _)) => (Mark::Ok, "none open".to_string()),
        Ok((n, titles)) => {
            let newest = if titles.is_empty() {
                String::new()
            } else {
                format!(": {}", titles.join("; "))
            };
            (Mark::Note, format!("{n} open{newest} (aterm exceptions)"))
        }
    }
}

/// `aterm list-kitty-commands` — every word the cursor cat obeys when it is typed at the
/// terminal, by language (the rows of `tricks.toml`).
pub(crate) fn list_kitty_commands_report() -> String {
    let mut out = String::new();
    for row in aterm_lexicon::TrickLexicon::all_rows() {
        out.push_str(&aterm_lexicon::tricks::row_line(&row));
        out.push('\n');
    }
    out
}

/// A doctor row's presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// The check passed.
    Ok,
    /// A fact worth reporting that is not a failure (nothing to fix, or nothing aterm can fix),
    /// and never a reason to refuse to launch.
    Note,
    /// A measured cost the person is paying now, with a fix only they can make (atpkg's `warn`):
    /// louder than a note, and a step for the person that leaves `aterm doctor` not ready.
    Warn,
    /// The check failed; only the shell row fails.
    Fail,
}

impl Mark {
    const fn render(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Note => "note",
            Self::Warn => "warn",
            Self::Fail => "FAIL",
        }
    }

    const fn is_fail(self) -> bool {
        matches!(self, Self::Fail)
    }

    const fn from_ok(ok: bool) -> Self {
        if ok { Self::Ok } else { Self::Fail }
    }
}

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

/// The macOS consent facts behind the `privacy:` row, gathered by [`privacy_facts`] and
/// passed into the pure [`doctor_rows`] like the shell check.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrivacyFacts {
    /// The instance's Full Disk Access state, from one prompt-free probe.
    fda: FdaState,
    /// Why `fda` reads as it does. `label.refused()` means no syscall ran, so the row names
    /// the configuration rather than implying a denial.
    fda_label: ProbeLabel,
    /// The class of this build's designated requirement — whether a grant made
    /// today survives the next build.
    dr: DrClass,
    /// The display name of the process macOS holds responsible for this one;
    /// the verdict belongs to it (under another terminal, that terminal's).
    responsible: Option<String>,
}

impl PrivacyFacts {
    const fn not_measured(label: ProbeLabel) -> Self {
        Self {
            fda: FdaState::Unknown,
            fda_label: label,
            dr: DrClass::Unknown,
            responsible: None,
        }
    }
}

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

/// Classify the running bundle's designated requirement as the copy census reads it
/// ([`aterm_containment::consent::bundle_identity`]): one `codesign` answers both.
#[cfg(target_os = "macos")]
fn designated_requirement_class() -> DrClass {
    let Some(exe) = aterm_containment::consent::fence_exe() else {
        return DrClass::Unknown;
    };
    let Some(app_root) = aterm_containment::consent::app_bundle_root(&exe) else {
        return DrClass::Unknown;
    };
    aterm_containment::consent::bundle_identity(
        &app_root,
        &aterm_containment::consent::protected_roots(&[]),
    )
    .map_or(DrClass::Unknown, |identity| identity.dr)
}

/// Off macOS there is no TCC and no designated requirement to read.
#[cfg(not(target_os = "macos"))]
fn designated_requirement_class() -> DrClass {
    DrClass::Unknown
}

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

/// What the `privacy:` row leaves for after doctor's report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrivacyNext {
    /// Nothing: the grant reads on, or it went unmeasured.
    Nothing,
    /// Full Disk Access reads off: hand it to the aterm window's permission walk, or, with no
    /// window, open its page and watch for the switch ([`permissions_hand_over`]).
    HandOver,
}

fn privacy_row(facts: &PrivacyFacts) -> (Mark, String, PrivacyNext) {
    let (mark, fact) = privacy_words(facts);
    let next = if !facts.fda_label.refused() && facts.fda == FdaState::Denied {
        PrivacyNext::HandOver
    } else {
        PrivacyNext::Nothing
    };
    (mark, fact, next)
}

fn privacy_words(facts: &PrivacyFacts) -> (Mark, String) {
    let who = facts.responsible.as_deref().unwrap_or("unknown");
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
        DebuggerMode::Unknown => (
            Mark::Note,
            format!("not measured ({})", debugger_probe_reason(probe.label)),
        ),
    }
}

struct DevtoolsFacts {
    facts: Option<atpkg::devtools::Facts>,
    did: Option<String>,
}

impl DevtoolsFacts {
    /// No list read: no row.
    const fn absent() -> Self {
        Self {
            facts: None,
            did: None,
        }
    }
}

/// Whether a shell can be started, and if not, which of the two causes `doctor` names (the
/// driver's `shell_check` tells them apart).
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
/// account's login shell from the password database, else `/bin/sh`.
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

/// What [`doctor_facts_rows`] reads, one port per probe: [`FactReads::LIVE`] reads this
/// machine, and a test counts the reads a narrowed run makes and makes none for real.
struct FactReads<'a> {
    /// One look at the program check's pool ([`program_check_fact`]).
    program_check: &'a (dyn Fn() -> Option<aterm_uds::program_check::Look> + Sync),
    /// The `devtools:` row's facts, and the hand-over due after the report ([`devtools_facts`]),
    /// given `fix` and whether the program check is full.
    devtools: &'a (dyn Fn(bool, bool) -> (DevtoolsFacts, Option<atpkg::devtools::Facts>) + Sync),
    /// [`spotlight::facts`].
    spotlight: &'a (dyn Fn() -> Option<Result<spotlight::Spotlit, String>> + Sync),
    /// [`services_fact`].
    services: &'a (dyn Fn() -> Option<Result<aterm_uds::services::Reading, String>> + Sync),
    /// [`privacy_facts`].
    privacy: &'a (dyn Fn() -> PrivacyFacts + Sync),
    /// [`debugger_facts`].
    debugger: &'a (dyn Fn() -> DebuggerFacts + Sync),
    /// [`instances_fact`].
    instances: &'a (dyn Fn() -> Result<aterm_ctl::census::Census, String> + Sync),
    /// [`fabric_fact`], given why every program's command line goes unread.
    fabric: &'a (dyn Fn(Option<&str>) -> FabricFact + Sync),
    /// [`person_here`]: a hand-over may follow the report.
    person_here: &'a (dyn Fn() -> bool + Sync),
}

/// The fabric's row as [`fabric_fact`] reads it: its mark, its fact, and whether it went
/// unmeasured.
type FabricFact = Option<(Mark, String, bool)>;

impl FactReads<'static> {
    const LIVE: Self = Self {
        program_check: &program_check_fact,
        devtools: &devtools_facts,
        spotlight: &spotlight::facts,
        services: &services_fact,
        privacy: &privacy_facts,
        debugger: &debugger_facts,
        instances: &instances_fact,
        fabric: &fabric_fact,
        person_here: &person_here,
    };
}

/// A person at this Mac's console reads doctor's stdout ([`console_absence`], and a stdout
/// that is a terminal): a page may be brought up for them after the report.
fn person_here() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdout().is_terminal() && console_absence().is_none()
}

/// The grants `aterm doctor` hands over after its report: Full Disk Access when the
/// `privacy:` row reads it off ([`PrivacyNext::HandOver`]), Developer Tools when its hand-over
/// is due ([`devtools_hand_over_due`]).
#[derive(Debug, Clone, Default)]
pub(crate) struct Missing {
    /// Full Disk Access reads off.
    pub(crate) fda: bool,
    /// Developer Tools' facts, when its hand-over is due.
    pub(crate) devtools: Option<atpkg::devtools::Facts>,
}

impl Missing {
    /// The hand-over due after a report, when there is one.
    fn due(self) -> Option<Self> {
        (self.fda || self.devtools.is_some()).then_some(self)
    }

    /// The walk's subjects, in its order: `permissions walk now <words>`.
    fn words(&self) -> String {
        [
            self.fda.then_some("fda"),
            self.devtools.is_some().then_some("devtools"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
    }

    /// The pages, as a person reads them.
    fn pages(&self) -> &'static str {
        match (self.fda, self.devtools.is_some()) {
            (true, true) => "Full Disk Access, then Developer Tools",
            (true, false) => "Full Disk Access",
            _ => "Developer Tools",
        }
    }
}

/// `aterm doctor`'s own rows that `reads` names (the shell's always), each probe on a thread
/// of its own, and the grants to hand over after the report ([`Missing`]). Nothing here waits
/// on a person or opens a pane.
fn doctor_facts_rows(fix: bool, reads: &dyn Fn(&str) -> bool) -> FactsRead {
    facts_rows_over(fix, reads, &FactReads::LIVE)
}

/// What [`facts_rows_over`] read: the rows, the grants to hand over after the report
/// ([`Missing`]), and whether Spotlight indexes `rust-docs` pages
/// ([`spotlight::Spotlit::docs_indexed`]).
struct FactsRead {
    rows: Vec<DoctorRow>,
    hand_over: Option<Missing>,
    rust_docs: bool,
}

/// [`doctor_facts_rows`] over `ports`.
fn facts_rows_over(fix: bool, reads: &dyn Fn(&str) -> bool, ports: &FactReads<'_>) -> FactsRead {
    let program_check = ["syspolicyd:", "devtools:", "instances:", "fabric:"]
        .into_iter()
        .any(reads)
        .then(ports.program_check)
        .flatten();
    let withheld = args_read_withheld(program_check.as_ref());
    let withheld = withheld.as_deref();
    fn join<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
        handle
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }
    fn unread<T>() -> Result<T, String> {
        Err("not read".to_owned())
    }
    let (mut rows, hand_over, rust_docs) = std::thread::scope(|scope| {
        let check_full = withheld.is_some();
        let devtools =
            reads("devtools:").then(|| scope.spawn(move || (ports.devtools)(fix, check_full)));
        let spotlight = reads("spotlight:").then(|| scope.spawn(ports.spotlight));
        let services = reads("services:").then(|| scope.spawn(ports.services));
        let privacy = reads("privacy:").then(|| scope.spawn(ports.privacy));
        let debugger = reads("debugger:").then(|| scope.spawn(ports.debugger));
        let instances = reads("instances:").then(|| {
            scope.spawn(move || match withheld {
                Some(why) => Err(why.to_owned()),
                None => (ports.instances)(),
            })
        });
        let fabric = reads("fabric:").then(|| scope.spawn(move || (ports.fabric)(withheld)));
        let recovery = if reads("recovery:") {
            recovery_facts()
        } else {
            unread()
        };
        let exceptions = if reads("exceptions:") {
            exceptions_facts()
        } else {
            unread()
        };
        let machine = MachineFacts {
            console_host: reads("conpty:").then(console_host_fact).flatten(),
            fabric: fabric.and_then(join),
            spotlight: spotlight.and_then(join),
            services: services.and_then(join),
            shim_cache: reads("services:").then(shim_cache_fact).flatten(),
            program_check,
            ..MachineFacts::absent()
        };
        let privacy = privacy.map_or_else(
            || PrivacyFacts::not_measured(ProbeLabel::UnsupportedPlatform),
            join,
        );
        let debugger = debugger.map_or_else(
            || DebuggerFacts::not_measured(DebuggerProbeLabel::UnsupportedPlatform),
            join,
        );
        let instances = instances.map_or_else(unread, join);
        let (devtools, devtools_due) =
            devtools.map_or_else(|| (DevtoolsFacts::absent(), None), join);
        let fda_due =
            fix && privacy_row(&privacy).2 == PrivacyNext::HandOver && (ports.person_here)();
        let hand_over = Missing {
            fda: fda_due,
            devtools: devtools_due,
        }
        .due();
        let rust_docs = machine
            .spotlight
            .as_ref()
            .is_some_and(|lit| lit.as_ref().is_ok_and(spotlight::Spotlit::docs_indexed));
        #[cfg(not(windows))]
        let rows = {
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
        };
        #[cfg(windows)]
        let rows = {
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
        };
        (rows, hand_over, rust_docs)
    });
    rows.retain(|row| reads(row.label));
    FactsRead {
        rows,
        hand_over,
        rust_docs,
    }
}

/// `aterm-cli`'s half of `aterm doctor`, without the packages' items: gathered, then
/// applied with `fix` (the stale copies to the Trash) or planned without it.
#[cfg(all(test, any(windows, target_os = "macos")))]
fn doctor_report(fix: bool) -> (String, i32) {
    let gathered = doctor::gather(fix, &[]);
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
    /// macOS: what its own services hold ([`services_fact`]).
    services: Option<Result<aterm_uds::services::Reading, String>>,
    /// macOS: the cache root plain `targo` builds in ([`shim_cache_fact`]).
    shim_cache: Option<std::path::PathBuf>,
}

impl MachineFacts {
    const fn absent() -> Self {
        Self {
            copies: None,
            console_host: None,
            fabric: None,
            spotlight: None,
            program_check: None,
            services: None,
            shim_cache: None,
        }
    }
}

/// The window census ([`aterm_ctl::census::window_census`]) through the front door.
fn instances_fact() -> Result<aterm_ctl::census::Census, String> {
    aterm_ctl::census::window_census(FRONT_DOOR)
}

fn program_check_fact() -> Option<aterm_uds::program_check::Look> {
    cfg!(target_os = "macos")
        .then(|| aterm_uds::program_check::look(aterm_uds::program_check::PS_HANG))
}

/// macOS: one unprivileged read of what its own services hold ([`aterm_uds::services::read`]);
/// `None` elsewhere.
fn services_fact() -> Option<Result<aterm_uds::services::Reading, String>> {
    cfg!(target_os = "macos").then(|| {
        aterm_uds::services::read()
            .ok_or_else(|| "the process table or the machine's memory could not be read".to_owned())
    })
}

/// macOS: the cache root plain `targo` builds in, as the `targo` shim on `PATH` names it
/// (`aterm_toolchain::build_cache::shim_root`); `None` elsewhere and with no home.
fn shim_cache_fact() -> Option<std::path::PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = aterm_types::dirs::home_dir()?;
    Some(atpkg::build_cache::shim_root(
        &home,
        &std::env::var_os("PATH").unwrap_or_default(),
    ))
}

/// Why fseventsd hears nothing of a `.ubd` folder.
const QUIET_WHY: &str = "whose file events fseventsd drops before it delivers or keeps them \
                         (observed on macOS 27.0, documented nowhere)";

/// The `services:` row's last line once the services were read: what aterm keeps from
/// fseventsd by where it puts its own files (`aterm_tempfile::QUIET`), the build cache only
/// where the `targo` shim on `PATH` builds in a quiet folder (`shim_cache`).
fn services_prevented(shim_cache: Option<&std::path::Path>) -> String {
    match shim_cache {
        Some(root) if aterm_tempfile::is_quiet(root) => format!(
            "prevented: aterm's build cache, verify's side lanes, release targets and test \
             scratch sit in .ubd folders, {QUIET_WHY}"
        ),
        Some(root) => format!(
            "prevented in part: verify's side lanes, release targets and test scratch sit in \
             .ubd folders, {QUIET_WHY}; the targo shim on PATH still builds in {}, outside one, \
             until an aterm update lays the new shim",
            root.display()
        ),
        None => format!(
            "prevented: verify's side lanes, release targets and test scratch sit in .ubd \
             folders, {QUIET_WHY}"
        ),
    }
}

/// The `services:` row: each of macOS's services past the bar any service needs
/// ([`aterm_vitals::services`]) with the restart aterm knows for it, else the largest, then
/// what aterm prevents ([`services_prevented`]).
fn services_rows(facts: &MachineFacts) -> Vec<DoctorRow> {
    use aterm_vitals::services::{Size, large_bar, restart_of, size};
    use aterm_vitals::words::mem;
    let Some(read) = &facts.services else {
        return Vec::new();
    };
    let name = |s: &aterm_uds::services::Service| aterm_vitals::sys::exe_word(&s.name).to_owned();
    let (mark, mut fact, unmeasured) = match read {
        Err(why) => (
            Mark::Note,
            format!("what macOS's services hold could not be read ({why})"),
            true,
        ),
        Ok(r) => {
            let bar = mem(large_bar(r.total));
            let over: Vec<String> = r
                .services
                .iter()
                .filter(|s| size(s.footprint, r.total) != Size::Usual)
                .map(|s| {
                    let then = restart_of(s).map_or_else(
                        || {
                            "aterm knows no safe restart for it: restart the Mac once your work \
                             is saved"
                                .to_owned()
                        },
                        |restart| {
                            format!(
                                "restart it: `aterm init --only {}` (one password prompt)",
                                restart.init
                            )
                        },
                    );
                    format!(
                        "{} holds {}, past the {bar} any service needs\n{then}",
                        name(s),
                        mem(s.footprint)
                    )
                })
                .collect();
            match (over.is_empty(), r.services.first()) {
                (false, _) => (Mark::Warn, over.join("\n"), false),
                (true, Some(top)) => (
                    Mark::Ok,
                    format!(
                        "each holds under {bar}; the largest, {}, holds {}",
                        name(top),
                        mem(top.footprint)
                    ),
                    false,
                ),
                (true, None) => (
                    Mark::Note,
                    "no service of macOS's own accounts was read".to_owned(),
                    true,
                ),
            }
        }
    };
    if read.is_ok() {
        fact.push('\n');
        fact.push_str(&services_prevented(facts.shim_cache.as_deref()));
    }
    vec![DoctorRow {
        label: "services:",
        mark,
        fact,
        unmeasured,
    }]
}

fn args_read_withheld(look: Option<&aterm_uds::program_check::Look>) -> Option<String> {
    let Some(aterm_uds::program_check::Look::Read(r)) = look else {
        return None;
    };
    r.full().then(|| {
        format!(
            "macOS's program check (syspolicyd) is at the kernel's thread limit, {} pool \
             threads against {}, and listing every program's command line would wait on the \
             programs it holds mid-start",
            r.workers,
            r.limit
                .map_or_else(|| "its limit".to_owned(), |l| format!("a limit of {l}"))
        )
    })
}

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
                         held there it is deadlocked: most new programs cannot start until \
                         the Mac restarts\n\
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

fn fabric_fact(withheld: Option<&str>) -> Option<(Mark, String, bool)> {
    #[cfg(unix)]
    {
        let summary = aterm_link::fabric::doctor_summary(withheld);
        Some((fabric_mark(summary.level), summary.text, summary.unmeasured))
    }
    #[cfg(not(unix))]
    {
        let _ = withheld;
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

/// The `devtools:` row's facts: the list and tccd's answers ([`atpkg::devtools::facts`]), the
/// wait beside them, which reads thread lists and start records only
/// ([`aterm_uds::process::held_programs`]), never a process's arguments, so it reads while the
/// check's pool is full too, and the row's word on a hand-over this run cannot make. The facts
/// come back a second time when the hand-over is due after the report
/// ([`devtools_hand_over_due`]); nothing is asked, opened or waited for here.
fn devtools_facts(fix: bool, check_full: bool) -> (DevtoolsFacts, Option<atpkg::devtools::Facts>) {
    use std::io::IsTerminal as _;
    let Some(mut facts) = atpkg::devtools::facts() else {
        return (DevtoolsFacts::absent(), None);
    };
    facts.program_check = aterm_uds::process::held_programs();
    let person = std::io::stdout().is_terminal();
    let (did, due) = devtools_hand_over_due(fix, &facts, check_full, person, console_absence);
    let hand_over = due.then(|| facts.clone());
    (
        DevtoolsFacts {
            facts: Some(facts),
            did,
        },
        hand_over,
    )
}

/// Whether `aterm doctor`'s Developer Tools hand-over follows its report, and the row's word
/// when it cannot be made. Due only with `fix` (never `--read-only` or `--json`), aterm not
/// granted, a person at this Mac's console (`away` reads why not: [`console_absence`]) and
/// stdout a terminal (`person`). Where nobody is at the console (an SSH session, a coding
/// agent, no terminal to ask at), or macOS's program check is at its thread limit, the row
/// says why; a stdout that is no terminal (a pipe, a file) is nobody to bring to a pane, so
/// nothing is opened or queued for it. `away` is read only once a hand-over could be due.
fn devtools_hand_over_due(
    fix: bool,
    facts: &atpkg::devtools::Facts,
    check_full: bool,
    person: bool,
    away: impl FnOnce() -> Option<&'static str>,
) -> (Option<String>, bool) {
    if !fix || facts.grant() != aterm_containment::DevToolGrant::NotGranted {
        return (None, false);
    }
    if let Some(why) = away() {
        return (
            Some(format!(
                "did nothing: no pane opens without a person at this Mac's console ({why})"
            )),
            false,
        );
    }
    if check_full {
        return (
            Some(
                "did nothing: macOS's program check is at its thread limit (`syspolicyd:`); \
                 held there, the switch cannot change until the Mac restarts, so run doctor \
                 again once it has room"
                    .to_string(),
            ),
            false,
        );
    }
    (None, person)
}

/// Why nobody at this process may be brought to the Developer Tools page: SAFE-1's one
/// person-at-console reading ([`aterm_machines::console::here`]: an SSH session, a coding
/// agent's tab, no terminal to ask at, another login, or none), `None` for a person at the
/// Mac's console.
#[cfg(target_os = "macos")]
fn console_absence() -> Option<&'static str> {
    aterm_machines::console::here().why()
}

/// No Mac's console to bring anyone to: the Developer Tools list is macOS's.
#[cfg(not(target_os = "macos"))]
const fn console_absence() -> Option<&'static str> {
    Some("this is not a Mac")
}

/// `aterm doctor`'s hand-over, after its report ([`doctor::answer`]), over the live reach: the
/// window and its permission walk, System Settings, the markers, the waits for a switch that
/// Enter ends ([`enter_pressed`]), and stderr for its lines.
fn permissions_hand_over_live(missing: &Missing) {
    use atpkg::devtools as dt;
    let subject = missing
        .devtools
        .as_ref()
        .and_then(|facts| facts.subject.clone())
        .unwrap_or_default();
    let version = aterm_types::version::app_version();
    let reach = DevtoolsReach {
        ask_walk: &ask_window_for_walk,
        ask_window: &ask_window_for_devtools,
        list_terminal: &dt::list_terminal,
        open_pane: &dt::open_pane,
        note_opened: &|| dt::note_opened(version),
        declined_at: &|| dt::declined_until().map(|declined| declined.at),
        await_on: &|| {
            let entered = enter_pressed();
            let list = dt::await_switch(
                &subject,
                DEVTOOLS_WAIT,
                |id| aterm_containment::probe_devtool_list(aterm_containment::ProbeGate::on(), id),
                || entered.load(std::sync::atomic::Ordering::Relaxed),
            );
            list.listing == aterm_containment::DevToolListing::On
        },
        refresh_verdict: &dt::refresh_verdict,
        open_fda_pane: &open_fda_pane,
        await_fda_on: &await_fda_on,
        local_day: &|at| local_day_words(at, i64::try_from(at).ok().and_then(local_offset_at)),
        say: &|words| eprintln!("{words}"),
    };
    permissions_hand_over_after(missing, &reach);
}

/// The hand-over over `reach` ([`permissions_hand_over`]), then a line for each thing it did:
/// the report is already out, so nothing it did is a row.
fn permissions_hand_over_after(missing: &Missing, reach: &DevtoolsReach<'_>) {
    for did in permissions_hand_over(missing, reach) {
        (reach.say)(&format!("aterm doctor: {did}"));
    }
}

/// The Developer Tools hand-over alone over `reach` ([`devtools_hand_over`]), as an aterm
/// window older than the permission walk takes it, then its one line.
#[cfg(test)]
fn devtools_hand_over_after(facts: &atpkg::devtools::Facts, reach: &DevtoolsReach<'_>) {
    let (did, _) = devtools_hand_over(facts, reach);
    (reach.say)(&format!("aterm doctor: {did}"));
}

/// Hand every grant doctor found missing to the running aterm window's permission walk
/// (`permissions walk now <subjects>`): one panel beside System Settings takes the person from
/// page to page, each turning to the next once its switch reads on, so doctor opens nothing
/// and waits for nothing. Where no walk takes them (no window, no answer, a window whose
/// reading has them held already) doctor opens each page itself and waits for its switch; a
/// window older than the walk takes Developer Tools through its own verb
/// ([`devtools_hand_over`]). Returns what it did, a line each.
fn permissions_hand_over(missing: &Missing, reach: &DevtoolsReach<'_>) -> Vec<String> {
    let here = |missing: &Missing, reach: &DevtoolsReach<'_>| {
        let mut did = Vec::new();
        if missing.fda {
            did.push(fda_open_here(reach));
        }
        if let Some(facts) = &missing.devtools {
            did.push(devtools_open_here(facts, reach, true).0);
        }
        did
    };
    let reply = match (reach.ask_walk)(&missing.words()) {
        WindowAnswer::Unreachable | WindowAnswer::Unanswered(_) => return here(missing, reach),
        WindowAnswer::Reply(reply) => reply,
    };
    let reply = reply.trim_end();
    if reply.starts_with("OK walking") {
        // A walk already under way answers with its own pages, which may leave out one doctor
        // asked for: the window reads that grant held (another app holds doctor's grant, the
        // terminal running it), as `OK granted` says, so doctor opens that page itself.
        let walked = |word: &str| walk_names(reply, word);
        let left = Missing {
            fda: missing.fda && !walked("fda"),
            devtools: missing.devtools.clone().filter(|_| !walked("devtools")),
        };
        let taken = Missing {
            fda: missing.fda && !left.fda,
            devtools: missing.devtools.clone().filter(|_| left.devtools.is_none()),
        };
        let mut did = Vec::new();
        if taken.fda || taken.devtools.is_some() {
            did.push(format!(
                "the aterm window walks you through {} beside System Settings: each page turns \
                 to the next once its switch reads on, and its Not now skips it",
                taken.pages()
            ));
        }
        did.extend(here(&left, reach));
        return did;
    }
    if reply == "OK granted" {
        // The window's reading has them held, doctor's a moment ago did not: the window's is
        // older, or another app (the terminal running doctor) is the one without them.
        return here(missing, reach);
    }
    if reply.starts_with("ERR unknown verb") {
        let mut did = Vec::new();
        if missing.fda {
            did.push(fda_open_here(reach));
        }
        if let Some(facts) = &missing.devtools {
            did.push(devtools_hand_over(facts, reach).0);
        }
        return did;
    }
    if reply.starts_with("ERR headless") {
        return vec![
            "did nothing: the aterm instance doctor reached is headless, so no page opens"
                .to_string(),
        ];
    }
    if reply == "OK declined" {
        return vec![
            "did nothing: you said Not now to these pages; System State's Permissions rows in \
             the aterm window open them again"
                .to_string(),
        ];
    }
    if reply == "OK recent" {
        return vec![
            "did nothing: the aterm window walked these pages within the last minute".to_string(),
        ];
    }
    vec![format!("did nothing: the aterm window answered `{reply}`")]
}

/// `aterm init`'s hand-over of the grants its Permissions phase left to the person
/// (`aterm_machines::init::Host::walk`): the running window's permission walk takes them, for a
/// person at this Mac's console reading the run. The line to print once it does; `None` where
/// no walk takes them, and the rows' own words stand.
#[must_use]
pub fn init_permissions_walk(missing: &[aterm_types::macos_setup::Access]) -> Option<String> {
    init_walk_over(missing, person_here(), &ask_window_for_walk)
}

/// [`init_permissions_walk`] over a person's presence and the window's answer.
fn init_walk_over(
    missing: &[aterm_types::macos_setup::Access],
    here: bool,
    ask_walk: &dyn Fn(&str) -> WindowAnswer,
) -> Option<String> {
    let walked: Vec<aterm_types::macos_setup::Access> = missing
        .iter()
        .copied()
        .filter(|access| walks(*access))
        .collect();
    if !here || walked.is_empty() {
        return None;
    }
    let words: Vec<&str> = walked.iter().map(|access| access.word()).collect();
    let WindowAnswer::Reply(reply) = ask_walk(&words.join(" ")) else {
        return None;
    };
    // A walk already under way names its own pages: only those it takes are the window's.
    let names: Vec<&str> = walked
        .iter()
        .filter(|access| walk_names(&reply, access.word()))
        .map(|access| access.name())
        .collect();
    (!names.is_empty()).then(|| {
        format!(
            "the aterm window walks you through {} beside System Settings: each page turns to \
             the next once its switch reads on",
            names.join(", then ")
        )
    })
}

/// Whether the window's `OK walking <subject>…` answer names `word`'s page: the walk's own word
/// (`fda`, `devtools`, `accessibility`, `debugger`), or a Permissions row's
/// (`full-disk-access`, `developer-tools`), which the walk answers with its own.
fn walk_names(reply: &str, word: &str) -> bool {
    fn own(word: &str) -> &str {
        match word {
            "full-disk-access" => "fda",
            "developer-tools" => "devtools",
            other => other,
        }
    }
    reply
        .trim_end()
        .strip_prefix("OK walking")
        .is_some_and(|walking| {
            walking
                .split_whitespace()
                .any(|walked| own(walked) == own(word))
        })
}

/// Whether the window's permission walk has a step for `access`: Local Network has no switch to
/// turn (macOS asks it on a program's first look at the network), so it stays a row of its own.
const fn walks(access: aterm_types::macos_setup::Access) -> bool {
    use aterm_types::macos_setup::Access;
    match access {
        Access::FullDiskAccess
        | Access::DeveloperTools
        | Access::Accessibility
        | Access::Debugger => true,
        Access::LocalNetwork => false,
    }
}

/// The banner doctor prints before it opens Full Disk Access itself.
const FDA_BANNER: &str = "aterm doctor: switch on aterm (or the terminal running this) in \
                          System Settings \u{25b8} Privacy & Security \u{25b8} Full Disk Access, \
                          so macOS stops interrupting programs run here with consent dialogs";

/// The line printed while doctor waits for Full Disk Access to read on.
const FDA_WAIT_LINE: &str = "aterm doctor: waiting up to 2 min for Full Disk Access to read on \
                             (Enter or ctrl-c ends the wait; the report above stands)\u{2026}";

/// Open Full Disk Access from doctor, after its report, and wait for its switch.
fn fda_open_here(reach: &DevtoolsReach<'_>) -> String {
    (reach.say)(FDA_BANNER);
    if let Err(why) = (reach.open_fda_pane)() {
        return format!("could not open Full Disk Access: {why}");
    }
    (reach.say)(FDA_WAIT_LINE);
    if (reach.await_fda_on)() {
        "opened Full Disk Access; you switched it on".to_string()
    } else {
        "opened Full Disk Access; not switched on while doctor waited".to_string()
    }
}

/// `/usr/bin/open` at Privacy & Security ▸ Full Disk Access.
#[cfg(target_os = "macos")]
fn open_fda_pane() -> Result<(), String> {
    let mut command = aterm_uds::spawnfd::command("/usr/bin/open");
    command.arg(aterm_machines::items::person::PANE_FULL_DISK_ACCESS);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    match aterm_uds::deadline::output_until(&mut command, deadline) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "open exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(error) => Err(error.to_string()),
    }
}

/// No Full Disk Access page to open off macOS; the hand-over is never due there
/// ([`console_absence`]).
#[cfg(not(target_os = "macos"))]
fn open_fda_pane() -> Result<(), String> {
    Err("Full Disk Access is macOS's".to_owned())
}

/// How often doctor reads Full Disk Access while it waits for the switch.
const FDA_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Read Full Disk Access every [`FDA_POLL`] for up to [`DEVTOOLS_WAIT`], or until Enter:
/// `true` once it reads on.
fn await_fda_on() -> bool {
    let entered = enter_pressed();
    let until = std::time::Instant::now() + DEVTOOLS_WAIT;
    loop {
        if aterm_containment::probe_fda(ProbeGate::on()).state == FdaState::Granted {
            return true;
        }
        if entered.load(std::sync::atomic::Ordering::Relaxed) || std::time::Instant::now() >= until
        {
            return false;
        }
        std::thread::sleep(FDA_POLL);
    }
}

/// Whether Enter was pressed at this terminal since the wait that holds it began
/// ([`enter_pressed`]).
struct EnterSince(u64);

impl EnterSince {
    fn load(&self, order: std::sync::atomic::Ordering) -> bool {
        enters().load(order) > self.0
    }
}

/// How many lines were read on this terminal's stdin so far, by the one reader thread the first
/// wait starts: read only while stdin is a terminal this process's group holds in the
/// foreground, so a closed or redirected stdin never ends a wait and a background job (`aterm
/// doctor &`) is never stopped by `SIGTTIN`. One reader for the process, so a later wait (the
/// Developer Tools switch after Full Disk Access) hears its own Enter, never a reader the earlier
/// wait left blocked on stdin; the thread is left to the process's end.
fn enters() -> &'static std::sync::atomic::AtomicU64 {
    use std::io::IsTerminal as _;
    static LINES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static READER: std::sync::Once = std::sync::Once::new();
    READER.call_once(|| {
        if std::io::stdin().is_terminal() && stdin_in_foreground() {
            std::thread::spawn(|| {
                let mut line = String::new();
                while std::io::stdin()
                    .read_line(&mut line)
                    .is_ok_and(|read| read > 0)
                {
                    line.clear();
                    LINES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
    });
    &LINES
}

/// Set once Enter is pressed at this terminal after this call ([`enters`]): it ends doctor's
/// wait for a switch early.
fn enter_pressed() -> EnterSince {
    EnterSince(enters().load(std::sync::atomic::Ordering::Relaxed))
}

/// Whether this process's group is the foreground group of the terminal on stdin; a read
/// from it by a background group stops the whole process (`SIGTTIN`).
#[cfg(unix)]
fn stdin_in_foreground() -> bool {
    // SAFETY: tcgetpgrp takes a descriptor by value and returns -1 when it is no terminal
    // (which never equals a group id); getpgrp takes no arguments and cannot fail. Neither
    // reads or writes memory of this process.
    unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) == libc::getpgrp() }
}

/// Windows stops no console reader for being in the background.
#[cfg(not(unix))]
const fn stdin_in_foreground() -> bool {
    true
}

/// What the running aterm window answered `devtools hand-over now`, as `aterm
/// doctor` acts on it ([`ask_window_for_devtools`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum WindowAnswer {
    /// No aterm instance answers: none runs, or the request never went out (the
    /// connect phase failed, or refused it: `ERR auth`, `ERR control server
    /// busy`).
    Unreachable,
    /// The request reached the window and no reply came (the words say how): no
    /// panel can be counted on, so doctor opens the page itself.
    Unanswered(String),
    /// The window's reply line (`OK …` / `ERR …`).
    Reply(String),
}

fn ask_window_for_devtools() -> WindowAnswer {
    let Some(sock) = aterm_ctl::front_door_instance() else {
        return WindowAnswer::Unreachable;
    };
    window_answer(aterm_ctl::front_door_send(
        &sock,
        "devtools hand-over now\n",
    ))
}

/// Ask the running aterm window to walk the person through `subjects`
/// (`permissions walk now <subjects>`).
fn ask_window_for_walk(subjects: &str) -> WindowAnswer {
    let Some(sock) = aterm_ctl::front_door_instance() else {
        return WindowAnswer::Unreachable;
    };
    window_answer(aterm_ctl::front_door_send(
        &sock,
        &format!("permissions walk now {subjects}\n"),
    ))
}

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

struct DevtoolsReach<'a> {
    /// Ask the running aterm window's permission walk to take these subjects
    /// ([`ask_window_for_walk`]).
    ask_walk: &'a dyn Fn(&str) -> WindowAnswer,
    /// Ask the running aterm window ([`ask_window_for_devtools`]), for a person at
    /// this terminal (`now`): a window older than the walk.
    ask_window: &'a dyn Fn() -> WindowAnswer,
    /// [`atpkg::devtools::list_terminal`].
    list_terminal: &'a dyn Fn() -> Result<(), String>,
    /// [`atpkg::devtools::open_pane`]: `/usr/bin/open`, the path measured to work
    /// from a foreground command line.
    open_pane: &'a dyn Fn() -> Result<(), String>,
    /// [`atpkg::devtools::note_opened`] for this version.
    note_opened: &'a dyn Fn(),
    /// When the owner said Not now to the panel (unix seconds), while it holds
    /// ([`atpkg::devtools::declined_until`]).
    declined_at: &'a dyn Fn() -> Option<u64>,
    /// Wait up to [`DEVTOOLS_WAIT`] for aterm's switch, or until Enter: `true` once it
    /// reads on.
    await_on: &'a dyn Fn() -> bool,
    /// [`atpkg::devtools::refresh_verdict`]: make macOS ask about aterm again once the
    /// switch read on (the running aterm, checked before, keeps that verdict until a
    /// fresh aterm process asks).
    refresh_verdict: &'a dyn Fn() -> Result<(), String>,
    /// [`open_fda_pane`].
    open_fda_pane: &'a dyn Fn() -> Result<(), String>,
    /// [`await_fda_on`].
    await_fda_on: &'a dyn Fn() -> bool,
    /// A unix time as the calendar day it fell on here ([`local_day_words`]).
    local_day: &'a dyn Fn(u64) -> String,
    /// Where the banner, the wait line and the did-line go: stderr, after the report
    /// on stdout.
    say: &'a dyn Fn(&str),
}

/// How long doctor waits for the switch after its report, when no window's panel
/// watches it.
const DEVTOOLS_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// The line printed while doctor waits for the switch, after its report: ctrl-c ends
/// doctor (it installs no handler), which leaves the report as printed.
const DEVTOOLS_WAIT_LINE: &str = "aterm doctor: waiting up to 2 min for aterm's switch to read on \
                                  in Developer Tools (Enter or ctrl-c ends the wait; the report \
                                  above stands)\u{2026}";

/// Coordinate the Developer Tools hand-over with any running aterm window, for a person at
/// this terminal, after the report.
///
/// Asks the running window first (`now`), so its helper panel appears beside System Settings
/// and confirms the switch; doctor then opens the page and says one line, with no wait. Where
/// no panel is up (no window, no answer, an older window, or a window whose last reading
/// disagrees with doctor's) doctor opens the page with the whole banner and waits for the
/// switch itself. Returns `(did, opened)`.
fn devtools_hand_over(facts: &atpkg::devtools::Facts, reach: &DevtoolsReach<'_>) -> (String, bool) {
    use atpkg::devtools as dt;
    let reply = match (reach.ask_window)() {
        WindowAnswer::Unreachable | WindowAnswer::Unanswered(_) => {
            return devtools_open_here(facts, reach, true);
        }
        WindowAnswer::Reply(reply) => reply,
    };
    let no_window_press = format!(
        "{} \u{25b8} Open Developer Tools\u{2026} opens the page",
        dt::STATUS_PAGE
    );
    let did = match reply.as_str() {
        "OK shown" => return devtools_open_here(facts, reach, false),
        // No panel could be shown, or the window's last reading disagrees with doctor's a
        // moment ago (not granted): the window's verdict is older.
        "OK unshown" | "OK granted" | "OK unknown" => {
            return devtools_open_here(facts, reach, true);
        }
        _ if reply.starts_with("ERR unknown verb") => {
            return devtools_open_here(facts, reach, true);
        }
        "OK recent" => format!(
            "did nothing: within the last minute aterm opened the page, or the aterm window \
             answered a request for it, or aterm's Full Disk Access page is waiting or open and \
             goes first; {no_window_press}"
        ),
        "OK declined" => format!(
            "did nothing: you said Not now{}; {no_window_press}",
            (reach.declined_at)()
                .map_or_else(String::new, |at| format!(" on {}", (reach.local_day)(at)))
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

/// Open the Developer Tools pane from `doctor`, after its report. `full` (no panel beside
/// it): the whole banner first, then, on a list read whole, a wait for the switch that Enter
/// ends. Otherwise the window's panel is up and confirms the switch, so doctor says one line
/// and waits for nothing.
fn devtools_open_here(
    facts: &atpkg::devtools::Facts,
    reach: &DevtoolsReach<'_>,
    full: bool,
) -> (String, bool) {
    use atpkg::devtools as dt;
    let listed = std::cell::Cell::new(false);
    let list_terminal = || {
        let outcome = (reach.list_terminal)();
        listed.set(outcome.is_ok());
        outcome
    };
    let open_pane = || {
        if full {
            (reach.say)(&devtools_banner(facts, listed.get()));
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
    let dt::Handled::Opened { step, .. } = &handled else {
        return (dt::handled_words(&handled), false);
    };
    (reach.note_opened)();
    if !full {
        return (devtools_panel_line(*step, listed.get()), true);
    }
    let mut did = dt::handled_words(&handled);
    if facts.list.label == aterm_containment::DevToolListLabel::Read {
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
            "; not switched on while doctor waited"
        });
    }
    (did, true)
}

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

/// The one line `aterm doctor` says once it opened the page beside the window's panel, which
/// watches the switch and has macOS ask about aterm again: the step, the + route beside a
/// drag, and, when this run's `spctl` added Terminal, whose row that is.
fn devtools_panel_line(step: atpkg::devtools::Step, listed_terminal: bool) -> String {
    use atpkg::devtools as dt;
    let route = if step == dt::Step::Add {
        format!(" ({})", dt::PLUS_ROUTE)
    } else {
        String::new()
    };
    let mut line = format!(
        "System Settings is open at Developer Tools: {}{route}. The aterm window confirms \
         when it is on.",
        step.action()
    );
    if listed_terminal {
        line.push(' ');
        line.push_str(dt::only_aterm(true));
    }
    line
}

/// The banner `doctor` prints on stderr when it opens System Settings with no window's
/// panel beside it.
fn devtools_banner(facts: &atpkg::devtools::Facts, listed_terminal: bool) -> String {
    use atpkg::devtools as dt;
    let skips = aterm_containment::SpikeEvidence::MEASURED.devtool_skips_first_run_check;
    let how = dt::Step::of(&facts.list).how();
    [
        format!("aterm doctor: {}", dt::HELPER_TITLE),
        format!("  {}", dt::why(skips)),
        format!("  {how}"),
        format!("  {}", dt::only_aterm(listed_terminal)),
        format!("  {}", dt::trade(skips)),
        format!("  {}", dt::STATUS_LINK),
    ]
    .join("\n")
}

/// The pure half of the Windows driver's `config_conpty`: aterm.toml's
/// top-level `conpty` string, parsed with `aterm_toml` as the window parses it.
#[cfg(any(windows, test))]
fn config_conpty_from_text(text: &str) -> Option<String> {
    let table: aterm_toml::Table = text.parse().ok()?;
    table.get("conpty")?.as_str().map(str::to_string)
}

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

const fn mark_of_level(level: atpkg::doctor::Level) -> Mark {
    match level {
        atpkg::doctor::Level::Ok => Mark::Ok,
        atpkg::doctor::Level::Warn => Mark::Warn,
        atpkg::doctor::Level::Note => Mark::Note,
    }
}

type RecoveryFacts = Result<(aterm_update::recovery_ledger::Checked, std::path::PathBuf), String>;

fn recovery_facts() -> RecoveryFacts {
    let path = aterm_update::recovery_ledger::default_ledger_path()
        .ok_or_else(|| "no log folder, so there is no ledger".to_owned())?;
    aterm_update::recovery_ledger::read_rows_checked(&path)
        .map(|checked| (checked, path.clone()))
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn recovery_row(facts: &RecoveryFacts) -> (Mark, String) {
    if cfg!(windows) {
        return (Mark::Note, "not recorded on Windows".to_string());
    }
    let (checked, path) = match facts {
        Ok(read) => read,
        Err(why) => return (Mark::Note, format!("not measured ({why})")),
    };
    let (mut lines, unexpected) = aterm_update::recovery_ledger::doctor_lines(&checked.rows, path);
    let mark = if unexpected || checked.skipped > 0 {
        Mark::Note
    } else {
        Mark::Ok
    };
    if checked.skipped > 0 {
        lines.push(format!(
            "{} ledger row{} this build cannot read, skipped (a newer aterm's, or cut short)",
            checked.skipped,
            if checked.skipped == 1 { "" } else { "s" },
        ));
    }
    (mark, lines.join("\n"))
}

/// The front door's routes, for the census ([`aterm_ctl::census::FrontDoor`]).
const FRONT_DOOR: aterm_ctl::census::FrontDoor<'static> = aterm_ctl::census::FrontDoor {
    alias: &front_door_alias,
    verb: &front_door_verb,
};

/// What the front door does under `argv[0]`'s file name `name` with `argv[1]` `first`
/// ([`alias_route`]), for the census: a window alias is a window, any other alias another
/// program, and its own names (`AliasRoute::FrontDoor`) route by `argv[1]`.
fn front_door_alias(name: &str, first: &str) -> Option<aterm_ctl::census::Verb> {
    use aterm_ctl::census::Verb as Census;
    match alias_route(name, first) {
        AliasRoute::AliasWindow | AliasRoute::AliasWindowVerb => Some(Census::Window),
        AliasRoute::Ctl
        | AliasRoute::Pkg
        | AliasRoute::Abrew
        | AliasRoute::Agh
        | AliasRoute::Fleet
        | AliasRoute::Drive
        | AliasRoute::OpenUrl
        | AliasRoute::Link
        | AliasRoute::Askpass
        | AliasRoute::MachinesAskpass
        | AliasRoute::Amacs
        | AliasRoute::CommandLineToolsShim => Some(Census::Program),
        AliasRoute::FrontDoor => None,
    }
}

/// What the front door does with `word` as `argv[1]`, for the census: a verb, `hook`, or a word
/// or flag it answers before it routes a window ([`mode_free`], [`asks_completions`]).
fn front_door_verb(word: &str) -> Option<aterm_ctl::census::Verb> {
    use aterm_ctl::census::Verb as Census;
    match Verb::from_operand(word) {
        Some(verb) if verb.is_windowing() => Some(Census::Window),
        Some(_) => Some(Census::Program),
        None => {
            (word == "hook" || mode_free(word) || asks_completions(word)).then_some(Census::Program)
        }
    }
}

const PROGRAM_GROUPS: usize = 6;

fn instances_row(census: &Result<aterm_ctl::census::Census, String>) -> (Mark, String) {
    let census = match census {
        Ok(census) => census,
        Err(why) => return (Mark::Note, format!("not measured ({why})")),
    };
    let (n, dir) = (census.instances, &census.dir);
    let mut detail = if census.unreached.is_empty() {
        match n {
            0 => "no aterm window or headless instance is running as you".to_string(),
            1 => format!("1 aterm window or headless instance, published in {dir}"),
            n => format!("{n} aterm windows and headless instances, each published in {dir}"),
        }
    } else {
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
        detail
    };
    if !census.programs.is_empty() {
        let mut groups: Vec<(&str, usize)> = Vec::new();
        for p in &census.programs {
            match groups.iter_mut().find(|(name, _)| *name == p.name) {
                Some((_, k)) => *k += 1,
                None => groups.push((&p.name, 1)),
            }
        }
        groups.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let mut named: Vec<String> = groups
            .iter()
            .take(PROGRAM_GROUPS)
            .map(|(name, k)| format!("{name} ×{k}"))
            .collect();
        if groups.len() > PROGRAM_GROUPS {
            named.push("…".to_string());
        }
        let m = census.programs.len();
        let (noun, verb) = if m == 1 {
            ("program", "runs")
        } else {
            ("programs", "run")
        };
        detail.push_str(&format!(
            "\n{m} other aterm {noun} {verb} as you: {}",
            named.join(", ")
        ));
    }
    // A deleted binary is named only from a build directory, its worktree removed: an installed
    // one is the normal state after a Linux update renames the new binary over the old.
    for p in census
        .programs
        .iter()
        .filter(|p| p.adopted || (p.build && p.deleted))
    {
        let facts: Vec<&str> = [
            (p.adopted, "its launcher exited and launchd adopted it"),
            (p.build, "from a build directory"),
            (p.deleted, "running a deleted binary"),
        ]
        .into_iter()
        .filter_map(|(holds, said)| holds.then_some(said))
        .collect();
        detail.push_str(&format!(
            "\npid {} `{}` (up {}): {}",
            p.pid,
            p.name,
            p.age,
            facts.join("; ")
        ));
    }
    let mark = if census.unreached.is_empty() {
        Mark::Ok
    } else {
        Mark::Note
    };
    (mark, detail)
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

/// `doctor`'s rows from the gathered facts, in report order.
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
        let (mark, fact, _) = privacy_row(privacy);
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
        let mark = mark_of_level(row.level);
        // The first line is the state and its one action, which bare doctor shows alone
        // ([`atpkg::devtools::brief`]); `--verbose` prints the whole row under it.
        let fact = match devtools.facts.as_ref().and_then(atpkg::devtools::brief) {
            Some(brief) => format!("{brief}\n{}", row.text),
            None => row.text,
        };
        let fact = match &devtools.did {
            Some(did) => format!("{fact}\n{did}"),
            None => fact,
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
    checks.extend(services_rows(machine));
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

/// [`doctor_rows`] as `aterm doctor`'s items, planned (never applied) and rendered with
/// the `health:` line: the report the pure tests read.
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

fn render_doctor_lines(checks: &[DoctorRow]) -> (String, i32) {
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

fn render_doctor_rows(checks: &[DoctorRow]) -> String {
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
    /// print introspection and exit without spawning a shell.
    Diag { cmd: String, arg: Option<String> },
    /// `<diag> --help`: print that subcommand's description and usage, exit 0,
    /// never run it.
    DiagHelp { cmd: String },
    /// `help [topic]`: print a manual page ([`manual`]); exit 0, or 2 for an
    /// unknown topic.
    Manual { topic: Option<String> },
    /// `agents [<cmd> [<agent>…]]`: manage the coding-agent primer and exit
    /// without spawning a shell.
    Agents { rest: Vec<String> },
    /// A usage error (unknown option, missing `--containment` value): the message
    /// is already framed for stderr; exit 2 without launching a shell.
    Usage(String),
    /// Proceed to launch. `containment` is the raw mode selection (if any) the
    /// init funnel parses (`None`: the default `User`).
    Run {
        containment: Option<String>,
        quiet: bool,
        verbose: bool,
    },
}

fn decide_args<I: Iterator<Item = String>>(args: I) -> CliAction {
    let mut args = args.peekable();
    if args.peek().map(String::as_str) == Some("help") {
        let _ = args.next();
        return CliAction::Manual { topic: args.next() };
    }
    if args.peek().map(String::as_str) == Some("agents") {
        let _ = args.next();
        return CliAction::Agents {
            rest: args.collect(),
        };
    }
    if let Some(first) = args.peek()
        && DIAG_COMMANDS
            .iter()
            .any(|(name, _)| *name == first.as_str())
    {
        let cmd = args.next().expect("peeked Some");
        let rest: Vec<String> = args.collect();
        if rest.iter().any(|a| a == "-h" || a == "--help") {
            return CliAction::DiagHelp { cmd };
        }
        let takes_operand = cmd == "show-face";
        if cmd == "doctor" {
            return match doctor::args(&rest) {
                Ok(_) => CliAction::Diag {
                    cmd,
                    arg: (!rest.is_empty()).then(|| rest.join(" ")),
                },
                Err(usage) => CliAction::Usage(usage),
            };
        }
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
    let mut opts_ended = false;
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

/// Format the `-V`/`--version` output: identity line, origin line, optional build number,
/// `which_copy` lines, and the compiled update trust anchor fingerprint.
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

static FIRST_SHELL_OUTPUT: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Register the hook the session runs once, when the shell first writes output; `false`
/// if one is already set.
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

fn answer_and_exit(text: &str, code: i32) -> ! {
    let written = aterm_uds::stdout::write_all(cli_text::for_stdout(text).as_bytes());
    std::process::exit(
        aterm_uds::stdout::answer_status("aterm", written, code).unwrap_or_else(|line| {
            eprintln!("{line}");
            1
        }),
    )
}

/// The effectful shell around [`decide_args`]: answers `--help`, `--version`, a diagnostic,
/// `help` or `agents` and exits (2 on a usage error), or records the launch flags
/// ([`SessionFlags`]) and returns whether `--quiet` was given.
pub fn parse_args(argv: Vec<std::ffi::OsString>) -> bool {
    match decide_args(argv.into_iter().map(|a| a.to_string_lossy().into_owned())) {
        CliAction::Help => answer_and_exit(&help_text(), 0),
        CliAction::Version => answer_and_exit(
            &version_text(aterm_update::which_copy::observe().as_ref()),
            0,
        ),
        CliAction::DiagHelp { cmd } => answer_and_exit(&diag_help_text(&cmd), 0),
        CliAction::Diag { cmd, arg } => match diag_report(&cmd, arg.as_deref()) {
            Some((report, 0)) => answer_and_exit(&report, 0),
            Some((report, code)) => {
                eprint!("{}", cli_text::for_stderr(&report));
                std::process::exit(code);
            }
            None => {
                eprintln!("aterm: `{cmd}` must be the first argument (`aterm {cmd} …`)");
                std::process::exit(2);
            }
        },
        CliAction::Manual { topic } => {
            let session = manual::in_session();
            let (out, code) = manual::render(topic.as_deref(), session.as_deref());
            if code == 0 {
                answer_and_exit(&out, 0);
            }
            eprint!("{}", cli_text::for_stderr(&out));
            std::process::exit(code);
        }
        CliAction::Agents { rest } => {
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

/// The session's launch flags, recorded by [`parse_args`] and read by [`session_main`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionFlags {
    /// `--containment <mode>` / `--sandbox` / `--no-sandbox` (the last one), as typed.
    pub containment: Option<String>,
    /// `--verbose`.
    pub verbose: bool,
}

static SESSION_FLAGS: std::sync::OnceLock<SessionFlags> = std::sync::OnceLock::new();

fn session_flags() -> SessionFlags {
    SESSION_FLAGS.get().cloned().unwrap_or_default()
}

fn prepend_path(dir: &str, inherited: Option<&str>) -> Option<(String, String)> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    match inherited {
        Some(p) if p.split(sep).any(|c| c == dir) => None,
        Some(p) if !p.is_empty() => Some(("PATH".to_string(), format!("{dir}{sep}{p}"))),
        _ => Some(("PATH".to_string(), dir.to_string())),
    }
}

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

const REROUTE_DIR_ENV: &str = "ATERM_REROUTE_DIR";
const PASSTHROUGH_ENV: &str = "__ATERM_REROUTE_PASSTHROUGH";

fn reroute_dir_from_env() -> Option<String> {
    reroute_dir_from_values(
        std::env::var(PASSTHROUGH_ENV).ok().as_deref(),
        std::env::var(REROUTE_DIR_ENV).ok().as_deref(),
    )
}

fn reroute_dir_from_values(no_reroute: Option<&str>, dir: Option<&str>) -> Option<String> {
    if aterm_types::control_socket::env_flag_engaged(no_reroute) {
        return None;
    }
    dir.filter(|dir| is_absolute_existing_dir(dir))
        .map(str::to_owned)
}

fn is_absolute_existing_dir(dir: &str) -> bool {
    let path = std::path::Path::new(dir);
    !dir.is_empty() && path.is_absolute() && path.is_dir()
}

fn is_absolute_real_dir(dir: &str) -> bool {
    is_absolute_existing_dir(dir)
        && std::fs::symlink_metadata(dir).is_ok_and(|md| !md.file_type().is_symlink())
}

const HOOK_AGENTS_ENV: &str = "ATPKG_AGENTS";

const AGENTS_DIR_ENV: &str = "ATERM_AGENTS_DIR";

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
/// group/other-writable (`platform::dir_meta_is_system`) ⇒ `0755`; a prefix this
/// user owns ⇒ `0700`; any other owner ⇒ `None` (refuse).
#[cfg(any(unix, test))]
const fn agents_dir_mode(prefix_uid: u32, prefix_mode: u32, our_uid: u32) -> Option<u32> {
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
/// `AlreadyExists` race tolerated.
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
            Ok(md) => existing(md),
            Err(_) => Err(format!("{}: {error}", dir.display())),
        },
    }
}

/// [`ensure_agents_dir`] off Unix: a plain `create_dir`, the symlink refusal kept. Unreached in
/// practice: Windows lays no reroute stubs, so no reroute directory is handed.
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

/// Whether `first` (the `argv[1]` operand) should be considered for toolchain dispatch.
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

const fn session_limits(mode: aterm_containment::ContainmentMode) -> aterm_sandbox::Limits {
    use aterm_containment::ContainmentMode as Cm;
    match mode {
        Cm::User => aterm_sandbox::Limits::inherit(),
        Cm::Safety => aterm_sandbox::Limits::shell_default(),
        _ => aterm_sandbox::Limits::contained(),
    }
}

/// Run the transparent PTY session: containment init, root-authority mint, protected shell
/// spawn, and passthrough loop.
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
    if std::env::var_os("AGY_CLI_FAST_PROMPT_EDITOR").is_none() {
        env_add.push(("AGY_CLI_FAST_PROMPT_EDITOR".to_string(), "true".to_string()));
    }

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
    use super::driver;
    #[cfg(windows)]
    use super::show_config_report;
    use super::{
        AGENTS_DIR_ENV, CliAction, DEBUGGER_ACCESS_REVERT, DIAG_COMMANDS, DebuggerFacts,
        DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, DevtoolsFacts, DrClass,
        ExceptionsFacts, FdaState, HOOK_AGENTS_ENV, MachineFacts, Mark, PASSTHROUGH_ENV,
        PrivacyFacts, ProbeLabel, REROUTE_DIR_ENV, RecoveryFacts, SelfHardening, ShellCheck, Verb,
        agents_dir_mode, config_conpty_from_text, decide_args, diag_report, doctor_checks,
        doctor_checks_with_origin, exceptions_report_at, explain_config_report, front_door_alias,
        front_door_verb, help_text, is_tool_candidate, list_fonts_report, list_themes_alias,
        managed_agents_dir, prepend_path, read_exceptions_log, reroute_dir_from_values,
        session_limits, session_path_env, show_face_report, version_text,
    };
    #[cfg(unix)]
    use super::{args_read_withheld, fabric_fact, fabric_mark, unix_shell};

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
            aterm_update::recovery_ledger::Checked::default(),
            std::path::PathBuf::from("/logs/recovery-ledger.log"),
        ))
    }

    fn no_exceptions() -> ExceptionsFacts {
        Ok((0, Vec::new()))
    }

    fn no_census() -> Result<aterm_ctl::census::Census, String> {
        Ok(aterm_ctl::census::Census {
            dir: "/run/aterm".to_string(),
            instances: 0,
            unreached: Vec::new(),
            programs: Vec::new(),
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
            &Ok((2, Vec::new())),
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
            assert_eq!(decide(&[cmd]), diag(cmd, None), "bare `{cmd}` still runs");
        }
    }

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

    /// `aterm doctor --help` is the doctor's grammar and five forms; another diagnostic's help
    /// is its description and usage (the control).
    #[test]
    fn doctor_help_is_its_grammar_and_forms() {
        assert_eq!(super::diag_help_text("doctor"), crate::doctor::help());
        let exceptions = super::diag_help_text("exceptions");
        assert!(
            exceptions.starts_with("exceptions — ")
                && exceptions.ends_with("usage: aterm exceptions [--all]\n"),
            "{exceptions}"
        );
    }

    #[test]
    fn doctor_operand_grammar_is_enforced_at_the_edge() {
        const USAGE: &str = "usage: aterm doctor [--read-only] [--json] [--verbose] [--only <id,...>] | (--check [--verbose] | --apply | --restore) [--json]";
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
        assert_eq!(
            decide(&["doctor", "--verbose", "--read-only"]),
            diag("doctor", Some("--verbose --read-only"))
        );
        assert_eq!(
            decide(&["doctor", "--check", "--verbose"]),
            diag("doctor", Some("--check --verbose"))
        );
        for argv in [
            vec!["doctor", "--fix"],
            vec!["doctor", "--verbose", "--verbose"],
            vec!["doctor", "--apply", "--verbose"],
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
        assert_eq!(format!("usage: {}", super::diag_usage("doctor")), USAGE);
        assert_eq!(super::diag_usage("exceptions"), "aterm exceptions [--all]");
        assert_eq!(super::diag_usage("show-face"), "aterm show-face <family>");
        assert_eq!(super::diag_usage("list-fonts"), "aterm list-fonts");
    }

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

    #[test]
    fn the_exceptions_reader_replays_the_log_without_a_window() {
        let dir = Scratch::new();
        let none = exceptions_report_at(&dir.log(), false, 1, 0);
        assert!(
            none.starts_with("exceptions: none (no log yet at "),
            "{none}"
        );

        let lines = exception_lines();
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
        assert!(
            report.find("To look at").unwrap() < report.find("Filed (").unwrap(),
            "{report}"
        );
        let all = exceptions_report_at(&dir.log(), true, T0 + 3_600_000, 3_600);
        assert!(all.contains("Filed (2)"), "{all}");
        assert!(!all.contains("--all lists every filed entry"), "{all}");

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
        let other = Scratch::new();
        std::fs::create_dir(other.log()).unwrap();
        let out = exceptions_report_at(&other.log(), false, 1, 0);
        assert!(out.contains("is a symlink or folder, not the log"), "{out}");
        assert!(read_exceptions_log(&other.log()).unreadable.is_some());
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
        let none = row(Ok((0, Vec::new())));
        assert!(
            none.lines().any(|l| l == "exceptions: ok    none open"),
            "{none}"
        );
        let two = row(Ok((2, Vec::new())));
        assert!(
            two.lines()
                .any(|l| l == "exceptions: note  2 open (aterm exceptions)"),
            "{two}"
        );
        // The newest open ones by title, inline.
        let titled = row(Ok((
            5,
            vec![
                "disk low".to_owned(),
                "update held".to_owned(),
                "vault quiet".to_owned(),
            ],
        )));
        assert!(
            titled.lines().any(|l| l
                == "exceptions: note  5 open: disk low; update held; vault quiet (aterm exceptions)"),
            "{titled}"
        );
        let homeless = row(Err("no log folder".to_string()));
        assert!(
            homeless
                .lines()
                .any(|l| l == "exceptions: note  not measured (no log folder)"),
            "{homeless}"
        );
    }

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

    #[test]
    fn diag_commands_advertised_and_dispatchable() {
        assert!(!DIAG_COMMANDS.is_empty(), "registry must not be empty");
        for (name, _desc) in DIAG_COMMANDS {
            assert!(
                help_text()
                    .lines()
                    .any(|l| l.trim_start().starts_with(name)),
                "subcommand {name:?} is not advertised as a --help line"
            );
            assert!(
                diag_report(name, None).is_some()
                    || list_themes_alias(name, &[]).is_some()
                    || *name == "doctor",
                "subcommand {name:?} is in the registry but has no dispatch"
            );
            if *name == "show-face" {
                assert_eq!(decide(&[name, "Menlo"]), diag(name, Some("Menlo")));
            } else {
                assert_eq!(decide(&[name]), diag(name, None));
            }
        }
        assert!(diag_report("definitely-not-a-command", None).is_none());
    }

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
        assert!(
            report
                .lines()
                .next()
                .is_some_and(|l| l.contains(" lang=en ")),
            "the first row is English"
        );
        assert_eq!(
            decide(&["list-kitty-commands", "--help"]),
            CliAction::DiagHelp {
                cmd: "list-kitty-commands".to_string()
            }
        );
    }

    #[test]
    fn help_word_routes_to_the_manual_and_is_advertised() {
        assert_eq!(decide(&["help"]), CliAction::Manual { topic: None });
        assert_eq!(
            decide(&["help", "trust"]),
            CliAction::Manual {
                topic: Some("trust".to_string())
            }
        );
        assert_eq!(decide(&["-h"]), CliAction::Help);
        assert!(
            help_text()
                .lines()
                .any(|l| l.trim_start().starts_with("aterm help")),
            "`aterm help` is not advertised in --help"
        );
        assert!(!is_tool_candidate(Some("help")));
    }

    #[test]
    fn agents_word_routes_to_the_primer_installer_and_is_advertised() {
        assert_eq!(decide(&["agents"]), CliAction::Agents { rest: vec![] });
        assert_eq!(
            decide(&["agents", "install", "claude"]),
            CliAction::Agents {
                rest: vec!["install".to_string(), "claude".to_string()]
            }
        );
        assert!(
            help_text()
                .lines()
                .any(|l| l.trim_start().starts_with("aterm agents")),
            "`aterm agents` is not advertised in --help"
        );
        assert!(!is_tool_candidate(Some("agents")));
    }

    #[test]
    fn no_synopsis_runs_into_its_blurb() {
        assert!(Verb::Init.usage().len() >= crate::VERB_BLURB_COLUMN - 4);
        for verb in Verb::ALL {
            let block = crate::verb_help_block(*verb);
            let first = block.lines().next().unwrap_or_default();
            let rest = first
                .trim_start()
                .strip_prefix(verb.usage())
                .unwrap_or_else(|| panic!("{first:?}"));
            assert!(rest.is_empty() || rest.starts_with(' '), "{first:?}");
            assert_eq!(
                block.lines().count(),
                verb.blurb().len() + usize::from(rest.is_empty()),
                "{block}"
            );
        }
    }

    #[test]
    fn front_door_verbs_are_the_one_command_surface() {
        assert!(!Verb::ALL.is_empty(), "roster must not be empty");
        let help = help_text();
        for verb in Verb::ALL {
            assert!(
                help.lines().any(|l| l
                    .trim_start()
                    .starts_with(&format!("aterm {}", verb.name()))),
                "verb `aterm {}` is not advertised in --help",
                verb.name()
            );
            assert!(
                !is_tool_candidate(Some(verb.name())),
                "verb {:?} can be shadowed by a store tool of the same name",
                verb.name()
            );
            assert_eq!(Verb::from_operand(verb.name()), Some(*verb));
            assert!(
                !verb.blurb().is_empty(),
                "verb {:?} has no help",
                verb.name()
            );
        }
        for (i, verb) in Verb::ALL.iter().enumerate() {
            assert!(
                !Verb::ALL[..i].iter().any(|u| u.name() == verb.name()),
                "duplicate front-door verb {}",
                verb.name()
            );
        }
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
        assert_eq!(
            prepend_path(bin, Some("/usr/bin")),
            Some(("PATH".to_string(), format!("{bin}{sep}/usr/bin")))
        );
        assert_eq!(
            prepend_path(bin, None),
            Some(("PATH".to_string(), bin.to_string()))
        );
        let already = format!("/usr/bin{sep}{bin}{sep}/bin");
        assert_eq!(prepend_path(bin, Some(&already)), None);
    }

    #[test]
    fn session_path_puts_the_reroute_dir_first_by_move_to_front_and_is_idempotent() {
        let sep = if cfg!(windows) { ';' } else { ':' };
        let join = |parts: &[&str]| parts.join(&sep.to_string());
        let reroute = "/Users//u/Library/Application Support/aterm/pkg/reroute";
        let bin = "/opt/aterm/bin";
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
        let inherited = join(&["/Users//u/.cargo/bin", reroute, bin, "/usr/bin", reroute]);
        let (_, value) =
            session_path_env(Some(reroute), None, Some(bin), Some(&inherited)).expect("injects");
        assert_eq!(
            value,
            join(&[reroute, "/Users//u/.cargo/bin", bin, "/usr/bin"])
        );
        assert_eq!(value.matches(reroute).count(), 1);
        let (_, again) =
            session_path_env(Some(reroute), None, Some(bin), Some(&value)).expect("injects");
        assert_eq!(again, value);
        assert_eq!(
            session_path_env(Some(reroute), None, None, None).map(|p| p.1),
            Some(reroute.to_string())
        );
        assert_eq!(
            session_path_env(Some(reroute), None, None, Some("")).map(|p| p.1),
            Some(reroute.to_string())
        );
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

    #[test]
    fn session_path_puts_the_agents_dir_second_by_the_same_move_to_front_rule() {
        let sep = if cfg!(windows) { ';' } else { ':' };
        let join = |parts: &[&str]| parts.join(&sep.to_string());
        let reroute = "/Users//u/Library/Application Support/aterm/pkg/reroute";
        let agents = "/Users//u/Library/Application Support/aterm/pkg/agents";
        let bin = "/opt/aterm/bin";
        let foreign = ["/Users//u/.local/bin", "/opt/homebrew/bin", "/usr/bin"];
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
        let inherited = join(&["/Users//u/.local/bin", agents, "/opt/homebrew/bin", agents]);
        let (_, value) =
            session_path_env(Some(reroute), Some(agents), None, Some(&inherited)).expect("injects");
        assert_eq!(
            value,
            join(&[reroute, agents, "/Users//u/.local/bin", "/opt/homebrew/bin"])
        );
        assert_eq!(value.matches(agents).count(), 1);
        let (_, again) =
            session_path_env(Some(reroute), Some(agents), None, Some(&value)).expect("injects");
        assert_eq!(again, value);
        assert_eq!(
            session_path_env(None, Some(agents), Some(bin), Some("/usr/bin")).map(|p| p.1),
            Some(join(&[agents, bin, "/usr/bin"]))
        );
        assert_eq!(
            session_path_env(None, Some(agents), None, None).map(|p| p.1),
            Some(agents.to_string())
        );
    }

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
        assert_eq!(
            managed_agents_dir(None, reroute.to_str(), None).as_deref(),
            Some(derived.as_str())
        );
        assert_eq!(reroute, layout.prefix.join(atpkg::reroute::DIR_NAME));
        assert_eq!(layout.agents_dir(), layout.prefix.join("agents"));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn reroute_env_names_match_atpkg() {
        assert_eq!(REROUTE_DIR_ENV, atpkg::reroute::REROUTE_DIR_ENV);
        assert_eq!(PASSTHROUGH_ENV, atpkg::reroute::PASSTHROUGH_ENV);
        assert_eq!(AGENTS_DIR_ENV, atpkg::reroute::AGENTS_DIR_ENV);
        assert_ne!(AGENTS_DIR_ENV, HOOK_AGENTS_ENV);
    }

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
        let derived = managed_agents_dir(None, Some(reroute_str), Some(hook_str)).expect("derived");
        assert_eq!(derived, prefix.join("agents").to_str().unwrap());
        assert!(prefix.join("agents").is_dir(), "ensured");
        assert_eq!(
            managed_agents_dir(Some(""), Some(reroute_str), Some(hook_str)).as_deref(),
            Some(derived.as_str()),
            "an EMPTY handoff means none — the sibling rule applies"
        );
        assert_eq!(
            managed_agents_dir(None, None, Some(hook_str)).as_deref(),
            Some(hook_str)
        );
        assert_eq!(managed_agents_dir(None, None, None), None);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-cli-{tag}-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

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

    #[test]
    fn agents_dir_mode_restates_layout_ensure_dirs_rule() {
        assert_eq!(agents_dir_mode(501, 0o700, 501), Some(0o700));
        assert_eq!(agents_dir_mode(501, 0o755, 501), Some(0o700));
        assert_eq!(agents_dir_mode(0, 0o755, 0), Some(0o755));
        assert_eq!(agents_dir_mode(0, 0o755, 501), Some(0o755));
        assert_eq!(agents_dir_mode(0, 0o700, 0), Some(0o755));
        assert_eq!(agents_dir_mode(0, 0o775, 0), Some(0o700));
        assert_eq!(agents_dir_mode(0, 0o777, 501), None);
        assert_eq!(agents_dir_mode(501, 0o700, 0), None);
        assert_eq!(agents_dir_mode(501, 0o700, 502), None);
    }

    #[test]
    fn managed_agents_dir_refuses_relative_handles_and_links_and_falls_back_to_the_enclosing_shell()
    {
        let scratch = scratch_dir("agents-refuse");
        let cwd_agents = std::path::Path::new("agents");
        assert!(
            !cwd_agents.exists(),
            "precondition: the test cwd has no `agents` entry"
        );
        assert_eq!(managed_agents_dir(None, Some("reroute"), None), None);
        assert_eq!(managed_agents_dir(None, Some("./pkg/reroute"), None), None);
        assert!(!cwd_agents.exists(), "no `agents` created in the cwd");
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
        let prefix = scratch.join("pkg");
        let reroute = prefix.join("reroute");
        std::fs::create_dir_all(&reroute).unwrap();
        let derived =
            managed_agents_dir(None, reroute.to_str(), Some(enclosing_str)).expect("derived");
        assert_eq!(derived, prefix.join("agents").to_str().unwrap());
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
        for line in r.lines().filter(|l| !l.is_empty()) {
            assert!(line.contains('='), "non key=value line: {line:?}");
        }
    }

    #[test]
    fn show_face_requires_and_validates_family() {
        let (msg, code) = show_face_report(None);
        assert_eq!(code, 2);
        assert!(msg.contains("usage: aterm show-face <family>"), "{msg}");
        let (msg, code) = show_face_report(Some("definitely-not-a-real-font-xyzzy"));
        assert_eq!(code, 1);
        assert!(msg.contains("aterm list-fonts"), "{msg}");
    }

    fn mark_of(report: &str, label: &str) -> String {
        report
            .lines()
            .find(|l| l.starts_with(label))
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or_else(|| panic!("no {label} row in\n{report}"))
            .to_string()
    }

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
        assert!(
            !r.contains("tty:") && !r.contains("cols") && !r.contains("containment"),
            "{r}"
        );
        for key in ["shell: ", "privacy: ", "debugger: ", "version: "] {
            assert!(
                r.lines().any(|l| l.starts_with(key)),
                "doctor detail missing a {key:?} line\n{r}"
            );
        }

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
        if let Ok(posix_shell) = std::env::var("SHELL")
            && posix_shell.starts_with('/')
        {
            assert!(!doctor.contains(&posix_shell), "{doctor}");
            assert!(!show.contains(&posix_shell), "{show}");
        }
    }

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

    /// The `recovery:` row: an empty ledger is ok and says where the record will write; a
    /// recorded kill is a note that names it, never a fail; no log directory is
    /// "not measured", never "none".
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
        let read = |rows, skipped| aterm_update::recovery_ledger::Checked { rows, skipped };
        let (r, code) = check(Ok((read(vec![killed], 0), path.clone())));
        assert_eq!(code, 0, "a past kill never fails doctor: {r}");
        assert_eq!(mark_of(&r, "recovery:"), "note", "{r}");
        assert!(
            r.contains("1790305290"),
            "the row names the killed build: {r}"
        );
        assert_eq!(r.matches("recovery:").count(), 1, "{r}");

        let (r, code) = check(Err("no log folder, so there is no ledger".to_owned()));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "recovery:"), "note", "{r}");
        assert_eq!(r.matches("recovery:").count(), 1, "{r}");

        // A row a newer aterm wrote is skipped and named; the row is a note, measured.
        let (r, code) = check(Ok((read(Vec::new(), 2), path)));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "recovery:"), "note", "{r}");
        assert!(
            r.contains("2 ledger rows this build cannot read, skipped"),
            "{r}"
        );
        let recovery = r
            .lines()
            .find(|l| l.starts_with("recovery:"))
            .unwrap_or_default();
        assert!(!recovery.contains("not measured"), "{r}");
    }

    #[test]
    fn the_instances_row_names_what_nothing_reaches_and_never_fails() {
        use aterm_ctl::census::{Census, Program, Unreached};
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
                programs: Vec::new(),
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

        let program = |pid: u32, name: &str, adopted: bool| Program {
            pid,
            name: name.to_string(),
            age: "3h 05m".to_string(),
            adopted,
            build: adopted,
            deleted: false,
        };
        let with_programs = |programs: Vec<Program>| {
            Ok(Census {
                dir: "/run/aterm".to_string(),
                instances: 1,
                unreached: Vec::new(),
                programs,
            })
        };
        let (r, code) = check(with_programs(vec![
            program(71100, "aterm link serve", false),
            program(71101, "aterm link serve", false),
        ]));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "instances:"), "ok", "{r}");
        assert!(
            r.contains("2 other aterm programs run as you: aterm link serve ×2"),
            "{r}"
        );
        assert!(
            !r.contains("pid 71100"),
            "a program whose launcher lives is counted, not named: {r}"
        );

        let (r, code) = check(with_programs(vec![
            program(71100, "aterm link serve", false),
            program(71102, "aterm link broker", true),
            Program {
                build: false,
                ..program(71103, "aterm pkg update", true)
            },
        ]));
        assert_eq!(code, 0, "{r}");
        assert_eq!(
            mark_of(&r, "instances:"),
            "ok",
            "an adopted program is a fact, not a leftover: aterm detaches passes of its own\n{r}"
        );
        assert!(r.contains("pid 71102 `aterm link broker`"), "{r}");
        assert!(r.contains("pid 71103 `aterm pkg update`"), "{r}");
        assert!(
            r.contains("its launcher exited and launchd adopted it"),
            "{r}"
        );
        assert!(!r.contains("nothing supervises"), "{r}");
        assert!(r.contains("from a build directory"), "{r}");
        assert!(
            r.contains(
                "3 other aterm programs run as you: aterm link broker ×1, aterm link serve ×1, \
                 aterm pkg update ×1"
            ),
            "{r}"
        );
        assert!(
            r.contains(
                "pid 71102 `aterm link broker` (up 3h 05m): its launcher exited and launchd \
                 adopted it; from a build directory\n"
            ),
            "{r}"
        );

        let (r, code) = check(with_programs(vec![
            Program {
                build: true,
                deleted: true,
                ..program(71104, "aterm link broker", false)
            },
            Program {
                build: true,
                ..program(71105, "aterm link serve", false)
            },
            Program {
                deleted: true,
                ..program(71106, "aterm", false)
            },
        ]));
        assert_eq!(code, 0, "{r}");
        assert_eq!(
            mark_of(&r, "instances:"),
            "ok",
            "a deleted binary is a fact\n{r}"
        );
        assert!(
            r.contains(
                "pid 71104 `aterm link broker` (up 3h 05m): from a build directory; running a \
                 deleted binary"
            ),
            "a build's program whose binary is gone with its worktree is named, adopted or not: \
             {r}"
        );
        assert!(
            !r.contains("pid 71105"),
            "a build's program whose launcher lives is counted, not named: {r}"
        );
        assert!(
            !r.contains("pid 71106"),
            "an installed program whose binary an update replaced is counted, not named: {r}"
        );

        let many: Vec<Program> = (0..8)
            .map(|i| program(72000 + i, &format!("aterm v{i}"), false))
            .collect();
        let (r, _) = check(with_programs(many));
        assert!(
            r.contains("aterm v5 ×1, …"),
            "at most six names, then a cut: {r}"
        );
        assert!(!r.contains("aterm v6"), "{r}");
    }

    #[test]
    fn the_front_door_verbs_name_a_program() {
        use aterm_ctl::census::Verb as Census;
        for word in ["link", "ctl", "doctor", "help", "hook"] {
            assert_eq!(front_door_verb(word), Some(Census::Program), "{word}");
        }
        for flag in [
            "-h",
            "--help",
            "-V",
            "--version",
            "--completions",
            "--completions=zsh",
        ] {
            assert_eq!(
                front_door_verb(flag),
                Some(Census::Program),
                "the front door answers {flag} before it routes a window"
            );
        }
        for word in ["new-tab", "new-window", "split-pane"] {
            assert_eq!(front_door_verb(word), Some(Census::Window), "{word}");
        }
        for word in ["frobnicate", "--headless", "--completion", ""] {
            assert_eq!(front_door_verb(word), None, "{word}");
        }
        for (name, first) in [
            ("aterm-ctl", "new-window"),
            ("atpkg", "new-tab"),
            ("abrew", "--real"),
            ("atpkg-abrew", ""),
            ("agh", ""),
            ("aterm-fleet", ""),
            ("aterm-drive", ""),
            ("aterm-askpass", ""),
            ("aterm-machines-askpass", "password: "),
            ("aterm-link", "broker"),
            ("aterm-open-url", "https://claude.ai/x"),
            ("amacs", ""),
        ] {
            assert_eq!(
                front_door_alias(name, first),
                Some(Census::Program),
                "the front door hands {name} to its own program"
            );
        }
        assert_eq!(front_door_alias("aterm-gui", ""), Some(Census::Window));
        assert_eq!(
            front_door_alias("aterm-windowed", "new-tab"),
            Some(Census::Window)
        );
        for (name, first) in [
            ("aterm", "new-window"),
            ("aterm-cli", ""),
            ("a-renamed-copy", ""),
            ("aterm-gui", aterm_containment::consent::OPEN_PROBE_VERB),
        ] {
            assert_eq!(
                front_door_alias(name, first),
                None,
                "{name} {first}: the front door's own route"
            );
        }
    }

    /// `doctor_report` from a test binary, outside an `.app` bundle, measures nothing: the
    /// privacy, debugger, devtools and Spotlight rows read `not measured`, and the stale-copy
    /// prune does not run.
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
            docs_kept: None,
            builds: vec![("~/w/target".into(), 12)],
            rustup: None,
        };
        let (r, code) = report(Some(Ok(lit)));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "spotlight:"), "note", "{r}");
        assert!(r.contains("~/w/target 12"), "{r}");
        assert!(r.contains("health: OK\n"), "{r}");
        let clean = crate::spotlight::Spotlit {
            docs_kept: None,
            builds: vec![],
            rustup: None,
        };
        let (r, _) = report(Some(Ok(clean)));
        assert_eq!(mark_of(&r, "spotlight:"), "ok", "{r}");
        let (r, _) = report(None);
        assert!(!r.contains("spotlight:"), "{r}");
    }

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
        let (r, _) = report(reading(100, false));
        assert_eq!(mark_of(&r, "syspolicyd:"), "ok", "{r}");
        let (r, _) = report(Some(aterm_uds::program_check::Look::Unread));
        assert_eq!(mark_of(&r, "syspolicyd:"), "note", "{r}");
        let (r, _) = report(Some(aterm_uds::program_check::Look::NoneRunning));
        assert_eq!(mark_of(&r, "syspolicyd:"), "ok", "{r}");
        let (r, _) = report(None);
        assert!(!r.contains("syspolicyd:"), "{r}");
    }

    /// The `services:` row names each of macOS's services past the bar and the restart aterm
    /// knows for it, or says none exists; under the bar it names the largest; once read, it
    /// says what aterm keeps from fseventsd. NEGATIVE CONTROL: a look-alike of fseventsd at
    /// another image gets no restart, and a row that could not read claims nothing.
    #[test]
    fn the_services_row_names_a_service_past_the_bar_and_its_restart() {
        use aterm_uds::services::{Reading, Service};
        let report = |services| {
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
                    services,
                    ..MachineFacts::absent()
                },
            )
        };
        let service = |name: &str, path: &str, footprint: u64| Service {
            name: name.into(),
            path: Some(path.into()),
            live: 1,
            footprint,
            ..Service::default()
        };
        let fseventsd = aterm_vitals::services::FSEVENTSD.path;
        let read = |services| {
            Some(Ok(Reading {
                total: 128 << 30,
                services,
                ..Reading::default()
            }))
        };
        let (r, code) = report(read(vec![
            service("fseventsd", fseventsd, 152_000_000_000),
            service("mds", "/System/mds", 1_330_000_000),
        ]));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "services:"), "warn", "{r}");
        assert!(
            r.contains("fseventsd holds 141.6 GiB, past the 8.0 GiB any service needs"),
            "{r}"
        );
        assert!(
            r.contains("restart it: `aterm init --only init.fseventsd` (one password prompt)"),
            "{r}"
        );
        let (r, _) = report(read(vec![service("fseventsd", "/tmp/fseventsd", 40 << 30)]));
        assert!(r.contains("aterm knows no safe restart for it"), "{r}");
        let (r, _) = report(read(vec![service("mds", "/System/mds", 1_330_000_000)]));
        assert_eq!(mark_of(&r, "services:"), "ok", "{r}");
        assert!(r.contains("the largest, mds, holds 1.2 GiB"), "{r}");
        assert!(r.contains("whose file events fseventsd drops"), "{r}");
        let with_shim = |root: &str| {
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
                    services: read(vec![service("mds", "/System/mds", 1_330_000_000)]),
                    shim_cache: Some(root.into()),
                    ..MachineFacts::absent()
                },
            )
            .0
        };
        let quiet = with_shim("/Users//u/Library/Caches/aterm/.ubd/cargo-build.noindex");
        assert!(
            quiet.contains("prevented: aterm's build cache, "),
            "{quiet}"
        );
        let old = with_shim("/Users//u/Library/Caches/aterm/cargo-build.noindex");
        assert!(
            old.contains("prevented in part: ")
                && old.contains("the targo shim on PATH still builds in /Users//u/")
                && !old.contains("aterm's build cache, "),
            "an old shim's cache is claimed for nothing: {old}"
        );
        let (r, _) = report(Some(Err("unread".into())));
        assert_eq!(mark_of(&r, "services:"), "note", "{r}");
        assert!(
            !r.contains("prevented:"),
            "nothing claimed of an unread row: {r}"
        );
        let (r, _) = report(None);
        assert!(!r.contains("services:"), "{r}");
    }

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

    /// `fabric_fact` reads the fabric row on Unix and never produces `Mark::Fail`.
    #[cfg(unix)]
    #[test]
    fn the_doctors_machine_facts_read_the_fabric_row() {
        let (mark, fact, _) = fabric_fact(None).expect("a fabric row on Unix");
        assert!(!mark.is_fail(), "{fact}");
        assert!(!fact.trim().is_empty());
    }

    /// While the program check's pool is at the kernel's limit, doctor lists no
    /// program's command line, and the reason the census and the fabric rows give carries
    /// the reading itself.
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
        let why = args_read_withheld(Some(&reading(95, true))).expect("a full pool withholds");
        assert!(
            why.contains("95 pool threads against a limit of 90")
                && why.contains("every program's command line"),
            "{why}"
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
    }

    /// Doctor's `devtools:` facts carry this user's programs macOS's program check holds now, read
    /// from the kernel beside the list (a test binary's list reads `not measured`; the count is
    /// read all the same).
    #[cfg(target_os = "macos")]
    #[test]
    fn doctor_reads_the_live_wait_beside_the_list() {
        let facts = super::devtools_facts(false, false)
            .0
            .facts
            .expect("macOS has a Developer Tools list");
        assert!(facts.program_check.is_some(), "{facts:?}");
    }

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
                program_check: None,
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
            services: None,
            shim_cache: None,
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
        // The one hand-over word a row carries (nobody at the console, where no pane may
        // open), so this pins where it lands, not its spelling.
        let off = devtools(DevToolListing::Off, None).facts.unwrap();
        let (did, due) =
            super::devtools_hand_over_due(true, &off, false, true, || Some("a remote shell"));
        assert!(!due, "away from the console nothing is handed over");
        let did = did.expect("the row says why");
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
        let (r, _) = report(&no_devtools());
        assert!(!r.contains("devtools:"), "{r}");
    }

    /// A scripted [`super::DevtoolsReach`]: the window answers `answer`, a decline on record
    /// dates `declined_at`, `before_wait` runs as the wait starts, and every act is logged in
    /// order instead of done (nothing is asked, run, opened, written, waited for or printed).
    struct Scripted {
        answer: super::WindowAnswer,
        /// What the window's permission walk answers.
        walk: super::WindowAnswer,
        declined_at: Option<u64>,
        /// The wait sees the switch go on.
        switched: bool,
        before_wait: Box<dyn Fn()>,
        log: std::cell::RefCell<Vec<String>>,
    }

    impl Scripted {
        fn new(answer: super::WindowAnswer) -> Self {
            Self {
                answer,
                walk: super::WindowAnswer::Reply("ERR unknown verb (try: help)".to_string()),
                declined_at: None,
                switched: true,
                before_wait: Box::new(|| {}),
                log: std::cell::RefCell::new(Vec::new()),
            }
        }

        /// The hand-over for `facts`: `(did, opened, log)`.
        fn run(&self, facts: &atpkg::devtools::Facts) -> (String, bool, Vec<String>) {
            let ((did, opened), log) = self.with(|reach| super::devtools_hand_over(facts, reach));
            (did, opened, log)
        }

        fn with<R>(&self, act: impl FnOnce(&super::DevtoolsReach<'_>) -> R) -> (R, Vec<String>) {
            let note = |entry: String| self.log.borrow_mut().push(entry);
            let reach = super::DevtoolsReach {
                ask_walk: &|subjects| {
                    note(format!("walk {subjects}"));
                    self.walk.clone()
                },
                ask_window: &|| {
                    note("ask".to_string());
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
                note_opened: &|| note("marker".to_string()),
                declined_at: &|| self.declined_at,
                await_on: &|| {
                    (self.before_wait)();
                    note("wait".to_string());
                    self.switched
                },
                refresh_verdict: &|| {
                    note("refresh".to_string());
                    Ok(())
                },
                open_fda_pane: &|| {
                    note("open fda".to_string());
                    Ok(())
                },
                await_fda_on: &|| {
                    note("wait fda".to_string());
                    self.switched
                },
                local_day: &|at| super::local_day_words(at, Some(0)),
                say: &|words| note(format!("say {words}")),
            };
            let out = act(&reach);
            (out, self.log.take())
        }
    }

    /// THE HAND-OVER IS DUE ONLY FOR A PERSON AT THE MAC'S CONSOLE, READING STDOUT (S5,
    /// SAFE-1): a gather with `fix` leaves it due when aterm is not granted, the console's
    /// presence reads a person, and stdout is a terminal. `--read-only` and `--json` (no
    /// `fix`), a granted list (neither reads the presence), and a stdout that is no terminal
    /// (a pipe, a file) leave nothing due and say nothing. Away from the console (an SSH
    /// session, a coding agent's tab with a terminal of its own, no terminal to ask at), or
    /// with the program check at its thread limit, nothing is due and the row says why.
    /// NEGATIVE CONTROL: a hand-over judged by stdout and SSH alone is due for an agent's tab.
    #[test]
    fn the_hand_over_is_due_only_for_a_person_at_the_macs_own_terminal() {
        use aterm_containment::DevToolListing;
        let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
        let granted = devtools_facts_with(read_list(DevToolListing::On, 2));
        assert_eq!(granted.grant(), aterm_containment::DevToolGrant::Granted);
        let here = || None;
        assert_eq!(
            super::devtools_hand_over_due(true, &absent, false, true, here),
            (None, true)
        );
        assert_eq!(
            super::devtools_hand_over_due(true, &absent, false, false, here),
            (None, false),
            "a stdout that is no terminal"
        );
        for (fix, facts) in [(false, &absent), (true, &granted)] {
            assert_eq!(
                super::devtools_hand_over_due(fix, facts, false, true, || unreachable!(
                    "no hand-over could be due, so nobody's presence is read"
                )),
                (None, false),
                "fix={fix} grant={:?}",
                facts.grant()
            );
        }
        for (away, check_full, words) in [
            (
                Some("a coding agent started this"),
                false,
                "a coding agent started this",
            ),
            (
                Some("this is a remote shell"),
                false,
                "this is a remote shell",
            ),
            (None, true, "program check is at its thread limit"),
        ] {
            let (did, due) =
                super::devtools_hand_over_due(true, &absent, check_full, true, || away);
            assert!(!due, "{words}");
            assert!(did.is_some_and(|did| did.contains(words)), "{words}");
        }
    }

    /// Counting stand-ins for every probe [`super::facts_rows_over`] makes, each meeting the
    /// others at `met` (a barrier of every threaded probe, or none), and the reads made.
    struct CountedReads {
        met: Option<std::sync::Barrier>,
        reads: std::sync::Mutex<Vec<&'static str>>,
    }

    impl CountedReads {
        fn new(met: Option<std::sync::Barrier>) -> Self {
            Self {
                met,
                reads: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn read(&self, what: &'static str) {
            self.reads.lock().unwrap().push(what);
            if let Some(met) = &self.met {
                met.wait();
            }
        }

        /// The rows `reads` names, over these stand-ins, and the probes they read (sorted).
        fn rows(&self, reads: &dyn Fn(&str) -> bool) -> (Vec<&'static str>, Vec<&'static str>) {
            let ports = super::FactReads {
                program_check: &|| None,
                devtools: &|_, _| {
                    self.read("devtools");
                    (DevtoolsFacts::absent(), None)
                },
                spotlight: &|| {
                    self.read("spotlight");
                    None
                },
                services: &|| {
                    self.read("services");
                    None
                },
                privacy: &|| {
                    self.read("privacy");
                    unmeasured()
                },
                debugger: &|| {
                    self.read("debugger");
                    no_debugger()
                },
                instances: &|| {
                    self.read("instances");
                    no_census()
                },
                fabric: &|_| {
                    self.read("fabric");
                    None
                },
                person_here: &|| false,
            };
            let rows = super::facts_rows_over(false, reads, &ports).rows;
            let mut read = self.reads.lock().unwrap().clone();
            read.sort_unstable();
            (rows.iter().map(|row| row.label).collect(), read)
        }
    }

    /// `--ONLY NARROWS WHAT DOCTOR'S OWN ROWS READ`: the help's "shell usability alone" check
    /// reads only the shell (no Developer Tools `log show`, no Spotlight `mdfind`, no
    /// codesign); a word naming one row reads that row's probe alone. NEGATIVE CONTROL: with
    /// no words every probe reads once.
    #[test]
    fn doctors_own_rows_read_only_what_the_words_name() {
        let shell_only = CountedReads::new(None);
        let (rows, read) = shell_only
            .rows(&|label| super::doctor::reads(&["base.shell.starts".to_owned()], label));
        assert_eq!(rows, ["shell:"]);
        assert_eq!(read, Vec::<&str>::new(), "the shell alone is read");

        let spotlight = CountedReads::new(None);
        let (rows, read) = spotlight
            .rows(&|label| super::doctor::reads(&["base.machine.spotlight".to_owned()], label));
        assert_eq!(rows, ["shell:"], "a Spotlight that read nothing has no row");
        assert_eq!(read, ["spotlight"]);

        let every = CountedReads::new(None);
        let (rows, read) = every.rows(&|label| super::doctor::reads(&[], label));
        assert_eq!(
            read,
            [
                "debugger",
                "devtools",
                "fabric",
                "instances",
                "privacy",
                "services",
                "spotlight"
            ],
            "each probe once"
        );
        assert!(
            rows.contains(&"instances:") && rows.contains(&"exceptions:"),
            "{rows:?}"
        );
    }

    /// DOCTOR'S OWN PROBES READ AT ONCE, NOT ONE AFTER ANOTHER: each threaded probe waits on
    /// one barrier of them all, which only a gather that starts every probe before it joins
    /// any passes. The one-minute wait is the hang detector a serial gather trips (its first
    /// probe's wait never returns).
    #[test]
    fn doctors_own_probes_read_at_once() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let all = CountedReads::new(Some(std::sync::Barrier::new(7)));
            let _ = tx.send(all.rows(&|label| super::doctor::reads(&[], label)).1);
        });
        let read = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the probes never met: one waited for another to finish");
        assert_eq!(read.len(), 7, "{read:?}");
    }

    fn devtools_facts_with(list: aterm_containment::DevToolList) -> atpkg::devtools::Facts {
        use aterm_containment::consent::{DevToolProbe, DevToolProbeLabel};
        atpkg::devtools::Facts {
            subject: Some("com.aterm.aterm".to_string()),
            list,
            log: DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest),
            other_responsible: None,
            program_check: None,
        }
    }

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

    /// THE REPORT IS OUT, ON STDOUT, BEFORE THE HAND-OVER WAITS (S5): `aterm doctor` prints
    /// its whole report before it hands anything over, so a wait for the switch holds no row
    /// back and ctrl-c during it leaves the report printed. A NOT READY report (exit 1) goes
    /// to stdout like a green one, and stderr carries only the hand-over's lines; a refusal
    /// goes to stderr. The scripted wait panics unless the report is already out. NEGATIVE
    /// CONTROL: the order the gather had (the hand-over first, the report after) trips it.
    #[test]
    fn the_report_is_out_on_stdout_before_the_hand_over_waits() {
        use super::WindowAnswer;
        use aterm_containment::DevToolListing;
        use std::cell::RefCell;
        use std::rc::Rc;
        let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
        let report = "shell:      ok    /bin/sh (executable)\n\
                      readiness: NOT READY \u{2014} 1 satisfied, 0 not applicable, 0 pending \
                      activation, 1 unresolved\n";
        let waits_after = |out: &Rc<RefCell<String>>| {
            let mut script = Scripted::new(WindowAnswer::Unreachable);
            let out = Rc::clone(out);
            script.before_wait = Box::new(move || {
                assert!(
                    out.borrow().contains("readiness: NOT READY"),
                    "the hand-over waited before the report was out"
                );
            });
            script
        };

        let stdout = Rc::new(RefCell::new(String::new()));
        let stderr = RefCell::new(String::new());
        let script = waits_after(&stdout);
        let waited = std::cell::Cell::new(false);
        let hand_over = |missing: &super::Missing| {
            let facts = missing.devtools.as_ref().expect("Developer Tools is due");
            let ((), log) = script.with(|reach| super::devtools_hand_over_after(facts, reach));
            waited.set(log.iter().any(|entry| entry == "wait"));
            for line in log.iter().filter_map(|entry| entry.strip_prefix("say ")) {
                stderr.borrow_mut().push_str(&format!("{line}\n"));
            }
        };
        let ends = super::doctor::Ends {
            stdout: &|text| {
                stdout.borrow_mut().push_str(text);
                Ok(())
            },
            stderr: &|text| stderr.borrow_mut().push_str(text),
            hand_over: &hand_over,
        };
        let due = super::Missing {
            fda: false,
            devtools: Some(absent.clone()),
        };
        let status = super::doctor::answer_over(report, 1, true, Some(&due), &ends);
        assert_eq!(status, 1, "the report's own code");
        assert!(waited.get(), "no window: doctor waits for the switch");
        assert_eq!(*stdout.borrow(), report, "the whole report, on stdout");
        let said = stderr.take();
        assert!(!said.contains("readiness:"), "{said}");
        assert!(said.contains(super::DEVTOOLS_WAIT_LINE), "{said}");
        assert!(said.ends_with("; you switched it on\n"), "{said}");

        // A refusal is stderr's, and nothing is handed over without a hand-over due.
        stdout.borrow_mut().clear();
        let refusal = "aterm doctor: --only typo: no item, family or layer has that name\n";
        assert_eq!(
            super::doctor::answer_over(refusal, 2, false, None, &ends),
            2
        );
        assert_eq!(
            (stdout.borrow().as_str(), stderr.take()),
            ("", refusal.to_owned())
        );

        // NEGATIVE CONTROL: the hand-over before the report trips the scripted wait.
        let early = Rc::new(RefCell::new(String::new()));
        let first = waits_after(&early);
        let tripped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            first.with(|reach| super::devtools_hand_over_after(&absent, reach));
        }));
        assert!(tripped.is_err(), "the check must trip on the old order");
    }

    /// `devtools_hand_over` asks the running window first (`now`) and opens System Settings
    /// itself: with one line and no wait beside the window's panel, which confirms the switch;
    /// with the whole banner and a wait where no panel is up. Every other answer is the
    /// window's decision, and nothing opens.
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
        let (did, opened, log) = Scripted::new(WindowAnswer::Unreachable).run(&absent);
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
        assert_eq!(log[0], "ask", "the window is asked first");
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
        // NEGATIVE CONTROL: a wait that never sees the switch (Enter, or the two minutes)
        // asks macOS nothing, and the did-line says so.
        let mut unswitched = Scripted::new(WindowAnswer::Unreachable);
        unswitched.switched = false;
        let (did, _, log) = unswitched.run(&absent);
        assert!(log.contains(&"wait".to_string()), "{log:?}");
        assert!(!log.contains(&"refresh".to_string()), "{did}: {log:?}");
        assert!(
            did.ends_with("; not switched on while doctor waited"),
            "{did}"
        );

        // The window's panel is up and confirms the switch: doctor opens the page and says
        // one line, the step with its + route and the panel, and waits for nothing.
        let (did, opened, log) = Scripted::new(reply("OK shown")).run(&absent);
        assert!(opened, "{did}");
        assert_eq!(log, ["ask", "open", "marker"], "no banner and no wait");
        for words in [
            "System Settings is open at Developer Tools",
            dt::HELPER_STEP,
            dt::PLUS_ROUTE,
            "The aterm window confirms",
        ] {
            assert!(did.contains(words), "{words:?} in {did}");
        }
        assert!(!did.contains('\n'), "one line: {did}");

        // No panel is up: none could be shown, the window is older than the verb, the request
        // went out and no reply came, or the window's last reading disagrees with doctor's a
        // moment ago. The whole banner, the page and the wait.
        for answer in [
            reply("OK unshown"),
            reply("ERR unknown verb (try: help)"),
            reply("OK granted"),
            reply("OK unknown"),
            WindowAnswer::Unanswered("did not answer within 10s".to_string()),
        ] {
            let (did, opened, log) = Scripted::new(answer.clone()).run(&absent);
            assert!(opened, "{answer:?}: {did}");
            assert!(said(&log).contains(dt::HELPER_HOW), "{answer:?}: {log:?}");
            assert!(log.contains(&"wait".to_string()), "{answer:?}: {log:?}");
        }
        // A decline on record does not hold back a person who asked here.
        let mut declined_here = Scripted::new(WindowAnswer::Unreachable);
        declined_here.declined_at = Some(1_790_812_800);
        let (did, opened, _) = declined_here.run(&absent);
        assert!(opened, "a person asked: {did}");

        // Everything else is the window's decision: nothing opens here.
        let mut declined = Scripted::new(reply("OK declined"));
        declined.declined_at = Some(1_790_812_800);
        for (script, words) in [
            (
                declined,
                "did nothing: you said Not now on 2026-10-01; aterm Settings \u{25b8} Security \
                 \u{25b8} Open Developer Tools\u{2026} opens the page",
            ),
            (
                Scripted::new(reply("OK recent")),
                "within the last minute aterm opened the page, or the aterm window",
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
            let (did, opened, log) = script.run(&absent);
            assert!(!opened, "{did}");
            assert!(did.starts_with("did nothing: "), "{did}");
            assert!(did.contains(words), "{words:?} in {did}");
            assert_eq!(log, ["ask"], "{did}");
        }
    }

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

    /// A Full Disk Access reading off is no longer only a note pointing at `aterm help
    /// permissions`: the row stays a note, and leaves a hand-over for after the report. A grant,
    /// a dev build's grant and an unmeasured reading leave none. NEGATIVE CONTROL: the row's
    /// mark alone (a note either way) cannot tell them apart.
    #[test]
    fn a_full_disk_access_reading_off_is_handed_over_after_the_report() {
        use super::{PrivacyNext, privacy_row};
        let (mark, fact, next) =
            privacy_row(&measured(FdaState::Denied, DrClass::Identity, "aterm"));
        assert_eq!((mark, next), (Mark::Note, PrivacyNext::HandOver), "{fact}");
        for facts in [
            measured(FdaState::Granted, DrClass::Identity, "aterm"),
            measured(FdaState::Granted, DrClass::Cdhash, "aterm (dev)"),
            unmeasured(),
        ] {
            assert_eq!(privacy_row(&facts).2, PrivacyNext::Nothing, "{facts:?}");
        }
        assert_eq!(
            privacy_row(&unmeasured()).0,
            mark,
            "the mark alone is a note for both"
        );
    }

    /// The gather leaves Full Disk Access due only with `fix`, a reading off and a person here.
    #[test]
    fn full_disk_access_is_due_only_for_a_person_here_with_fix() {
        let due = |fix: bool, fda: FdaState, here: bool| {
            let privacy = move || measured(fda, DrClass::Identity, "aterm");
            let person = move || here;
            let ports = super::FactReads {
                program_check: &|| None,
                devtools: &|_, _| (DevtoolsFacts::absent(), None),
                spotlight: &|| None,
                services: &|| None,
                privacy: &privacy,
                debugger: &no_debugger,
                instances: &no_census,
                fabric: &|_| None,
                person_here: &person,
            };
            super::facts_rows_over(fix, &|label| label == "privacy:", &ports)
                .hand_over
                .map(|missing| (missing.fda, missing.devtools.is_some()))
        };
        assert_eq!(due(true, FdaState::Denied, true), Some((true, false)));
        assert_eq!(due(false, FdaState::Denied, true), None, "--read-only");
        assert_eq!(due(true, FdaState::Denied, false), None, "nobody here");
        assert_eq!(due(true, FdaState::Granted, true), None, "granted");
    }

    /// `aterm doctor` hands what it found missing to the window's permission walk in one
    /// request, and then opens and waits for nothing. With no walk to take it (no window, no
    /// answer, a window that reads them held) it opens each page itself and waits for each
    /// switch; a window older than the walk gets Developer Tools through its own verb.
    #[test]
    fn doctor_hands_every_missing_grant_to_the_windows_walk() {
        use super::{Missing, WindowAnswer};
        use aterm_containment::DevToolListing;
        let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
        let both = Missing {
            fda: true,
            devtools: Some(absent.clone()),
        };
        let reply = |line: &str| WindowAnswer::Reply(line.to_string());
        let run = |script: &Scripted, missing: &Missing| {
            script.with(|reach| super::permissions_hand_over(missing, reach))
        };

        let mut walking = Scripted::new(WindowAnswer::Unreachable);
        walking.walk = reply("OK walking fda devtools\n");
        let (did, log) = run(&walking, &both);
        assert_eq!(
            log,
            ["walk fda devtools"],
            "one request, nothing opened here"
        );
        assert_eq!(did.len(), 1, "{did:?}");
        assert!(
            did[0].contains("walks you through Full Disk Access, then Developer Tools"),
            "{did:?}"
        );
        let fda_only = Missing {
            fda: true,
            devtools: None,
        };
        let (_, log) = run(&walking, &fda_only);
        assert_eq!(log, ["walk fda"]);

        // A walk already under way over Developer Tools alone: the window reads Full Disk
        // Access held (another app is the one without it), so doctor opens that page itself and
        // leaves Developer Tools to the walk. NEGATIVE CONTROL: its answer is `OK walking` too.
        let mut under_way = Scripted::new(WindowAnswer::Unreachable);
        under_way.walk = reply("OK walking devtools\n");
        let (did, log) = run(&under_way, &both);
        assert_eq!(log[0], "walk fda devtools");
        assert!(log.iter().any(|e| e == "open fda"), "{log:?}");
        assert!(!log.iter().any(|e| e == "open" || e == "wait"), "{log:?}");
        assert_eq!(did.len(), 2, "{did:?}");
        assert!(
            did[0].contains("walks you through Developer Tools "),
            "{did:?}"
        );
        assert_eq!(did[1], "opened Full Disk Access; you switched it on");

        for answer in [
            WindowAnswer::Unreachable,
            WindowAnswer::Unanswered("did not answer within 10s".to_string()),
            reply("OK granted"),
        ] {
            let mut script = Scripted::new(WindowAnswer::Unreachable);
            script.walk = answer.clone();
            let (did, log) = run(&script, &both);
            assert_eq!(did.len(), 2, "{answer:?}: {did:?}");
            assert_eq!(did[0], "opened Full Disk Access; you switched it on");
            for entry in ["open fda", "wait fda", "open", "wait"] {
                assert!(
                    log.iter().any(|e| e == entry),
                    "{answer:?}: {entry} in {log:?}"
                );
            }
            assert!(!log.iter().any(|e| e == "ask"), "{log:?}");
        }

        // An older window: Full Disk Access here, Developer Tools through `devtools hand-over
        // now`, whose panel confirms the switch.
        let older = Scripted::new(reply("OK shown"));
        let (did, log) = run(&older, &both);
        assert_eq!(did.len(), 2, "{did:?}");
        assert!(log.iter().any(|e| e == "ask"), "{log:?}");
        assert!(log.iter().any(|e| e == "open fda"), "{log:?}");

        let mut headless = Scripted::new(WindowAnswer::Unreachable);
        headless.walk =
            reply("ERR headless: a headless instance brings no System Settings page up");
        let (did, log) = run(&headless, &both);
        assert_eq!(log, ["walk fda devtools"]);
        assert!(did[0].contains("headless"), "{did:?}");

        // The owner's Not now, or a walk within the minute: the window's answer stands, and
        // doctor opens nothing past it.
        for (answer, words) in [("OK declined", "Not now"), ("OK recent", "last minute")] {
            let mut held = Scripted::new(WindowAnswer::Unreachable);
            held.walk = reply(answer);
            let (did, log) = run(&held, &both);
            assert_eq!(log, ["walk fda devtools"], "{answer}");
            assert_eq!(did.len(), 1, "{answer}: {did:?}");
            assert!(did[0].contains(words), "{answer}: {did:?}");
        }
    }

    /// Doctor's two waits (Full Disk Access, then the Developer Tools switch) share one stdin
    /// reader: each hears an Enter pressed after it began. NEGATIVE CONTROL: the Enter that ended
    /// the first wait does not end the second.
    #[test]
    fn each_wait_hears_only_an_enter_pressed_after_it_began() {
        use std::sync::atomic::Ordering::Relaxed;
        let first = super::enter_pressed();
        assert!(!first.load(Relaxed));
        super::enters().fetch_add(1, Relaxed);
        assert!(first.load(Relaxed), "Enter during the first wait ends it");
        let second = super::enter_pressed();
        assert!(
            !second.load(Relaxed),
            "the first wait's Enter is not the second's"
        );
        super::enters().fetch_add(1, Relaxed);
        assert!(second.load(Relaxed));
    }

    /// `aterm init` hands its Permissions phase's grants to the window's walk by their row
    /// words, for a person here; with nobody here, nothing asked, or a window that takes none,
    /// it says nothing more.
    #[test]
    fn init_hands_its_grants_to_the_windows_walk() {
        use super::{WindowAnswer, init_walk_over};
        use aterm_types::macos_setup::Access;
        let asked = std::cell::RefCell::new(Vec::new());
        let walking = |words: &str| {
            asked.borrow_mut().push(words.to_owned());
            WindowAnswer::Reply(format!("OK walking {words}"))
        };
        let missing = [
            Access::FullDiskAccess,
            Access::LocalNetwork,
            Access::Debugger,
        ];
        let line = init_walk_over(&missing, true, &walking).expect("a walk took them");
        assert_eq!(
            asked.take(),
            ["full-disk-access debugger"],
            "Local Network has no step of the walk's"
        );
        assert_eq!(
            init_walk_over(&[Access::LocalNetwork], true, &walking),
            None,
            "nothing walkable asks nothing"
        );
        assert!(
            line.contains("Full Disk Access, then Debugger access")
                || line.contains("Full Disk Access, then Debugger"),
            "{line}"
        );
        assert_eq!(
            init_walk_over(&missing, false, &walking),
            None,
            "nobody here"
        );
        assert_eq!(init_walk_over(&[], true, &walking), None);
        assert!(asked.take().is_empty(), "neither asks");
        for answer in [
            WindowAnswer::Unreachable,
            WindowAnswer::Reply("OK granted".to_owned()),
            WindowAnswer::Reply("ERR unknown verb (try: help)".to_owned()),
            // A walk already under way over none of these pages.
            WindowAnswer::Reply("OK walking devtools".to_owned()),
        ] {
            assert_eq!(
                init_walk_over(&missing, true, &|_| answer.clone()),
                None,
                "{answer:?}"
            );
        }
        let under_way = WindowAnswer::Reply("OK walking fda devtools\n".to_owned());
        let line = init_walk_over(&missing, true, &|_| under_way.clone()).expect("it takes one");
        assert!(line.contains("through Full Disk Access beside"), "{line}");
    }

    #[test]
    fn a_decline_is_dated_by_the_local_day() {
        let at = 1_790_812_800;
        assert_eq!(super::local_day_words(at, Some(0)), "2026-10-01");
        assert_eq!(super::local_day_words(at, Some(-7 * 3_600)), "2026-09-30");
        assert_eq!(
            super::local_day_words(at + 3_600, Some(2 * 3_600)),
            "2026-10-01"
        );
        assert_eq!(super::local_day_words(at, None), "2026-10-01 (UTC)");
    }

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
        let shown = || Scripted::new(WindowAnswer::Reply("OK shown".to_string()));

        let empty = devtools_facts_with(read_list(DevToolListing::Absent, 0));
        let (_, opened, log) = Scripted::new(WindowAnswer::Unreachable).run(&empty);
        assert!(opened);
        let spctl = log.iter().position(|e| e == "spctl").expect("spctl ran");
        let banner = log
            .iter()
            .position(|e| e.starts_with("say aterm doctor: Add"))
            .expect("the banner");
        assert!(spctl < banner, "{log:?}");
        assert!(said(&log).contains(dt::only_aterm(true)), "{log:?}");
        // Beside the window's panel the one line says it. NEGATIVE CONTROL: with no `spctl`,
        // the line speaks of no Terminal.
        let (did, opened, log) = shown().run(&empty);
        assert!(opened && log.contains(&"spctl".to_string()), "{log:?}");
        assert!(did.contains(dt::only_aterm(true)), "{did}");
        let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
        let (did, _, _) = shown().run(&absent);
        assert!(!did.contains("Terminal"), "{did}");

        let off = devtools_facts_with(read_list(DevToolListing::Off, 2));
        let (_, _, log) = Scripted::new(WindowAnswer::Unreachable).run(&off);
        let banner = said(&log);
        assert!(banner.contains(dt::HELPER_SWITCH_ON), "{banner}");
        assert!(!banner.contains("Drag"), "aterm is listed: {banner}");
        assert!(!log.contains(&"spctl".to_string()), "{log:?}");
        let (did, _, _) = shown().run(&off);
        assert!(did.contains(dt::STEP_SWITCH_ON), "{did}");
        assert!(!did.contains("Drag"), "aterm is listed: {did}");

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
        let (_, opened, log) = Scripted::new(WindowAnswer::Unreachable).run(&unread);
        assert!(opened);
        assert!(said(&log).contains(dt::HELPER_HOW), "{log:?}");
        assert!(
            !log.contains(&"wait".to_string()),
            "an unread list is not waited on: {log:?}"
        );
    }

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
        for verb in Verb::ALL {
            assert!(
                !is_tool_candidate(Some(verb.name())),
                "{} is a front-door verb",
                verb.name()
            );
        }
        assert!(!is_tool_candidate(Some("help")));
        for (name, _) in DIAG_COMMANDS {
            assert!(
                !is_tool_candidate(Some(name)),
                "{name} is a diag subcommand"
            );
        }
        for tool in ["ay", "ty", "trust", "trust-mc", "clean", "ny"] {
            assert!(
                is_tool_candidate(Some(tool)),
                "{tool} should be a candidate"
            );
        }
    }

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
                            break;
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

        let child_lines = |mode: Cm, script: &str| -> Vec<String> {
            let exec: Vec<std::ffi::OsString> = vec!["/bin/sh".into(), "-c".into(), script.into()];
            let shell = aterm_pty::spawn_shell_with_pid(
                24,
                80,
                &spawn_cap,
                &sandbox_cap,
                &[],
                None,
                None,
                None,
                Some(&exec),
                None,
                None,
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
