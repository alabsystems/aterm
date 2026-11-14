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
use atpkg::reroute::{AGENTS_DIR_ENV, PASSTHROUGH_ENV, REROUTE_DIR_ENV};

#[cfg(unix)]
#[path = "driver_unix.rs"]
mod driver;
#[cfg(windows)]
#[path = "driver_windows.rs"]
mod driver;

mod beside;

/// `aterm doctor`'s own rows as converge items; `crates/aterm` composes them with the packages'
/// repair items and `aterm pkg doctor`.
pub mod doctor;
mod doctor_leftovers;
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
    "                              agent brief. Start here.\n",
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
);

/// The by-hand Windows update lane `aterm help update` prints, and the build loop that stages
/// a build for `aterm update status|check|apply` (`aterm_update::windows`).
#[cfg(win_scripts)]
pub const WINDOWS_UPDATE_LANE: &str = "git pull, then apps\\aterm-win\\build.ps1 and apps\\aterm-win\\install.ps1 \
     (or aterm-winship update, which stages a build for aterm update apply)";
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
    /// Read authenticated retained custody for the calling process tree.
    ResourceInspect,
    /// `aterm pkg` — the toolchain package manager.
    Pkg,
    /// `aterm agh` — the GitHub client: reads and writes as the routed identity, JSON out, typed exits.
    Agh,
    /// `aterm fleet` — the lab's status line and one row per seat; the lab verbs (`remove`,
    /// `admit`, `invite`, `requests`, `ignore`); fleet membership (`init`, `join`, `peers`,
    /// `allow`, `leave`).
    Fleet,
    /// `aterm name` — this machine's name: its NAME and nickname
    /// (`docs/DESIGN-machine-names-2026-11-04.md` §5.7).
    Name,
    /// `aterm node` — the windowless fleet, Commons and steward node job (`serve | status | on | off`).
    Node,
    /// `aterm drive` — the window harness's readers (`classify`, `phase`, `answer`, `ledger`,
    /// `tape`, `replay`).
    Drive,
    /// `aterm link` — the fabric bridge (`serve`, `ls`, `notify`, `broker`, `mint`, `fabric`).
    Link,
    /// `aterm fabric` — fabric state, live traffic, and `on|off|doctor`.
    Fabric,
    /// `aterm ship` — the release tool (publishing; source checkouts only).
    Ship,
    /// `aterm release` — explicit owned release-capability evidence.
    Release,
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
    /// `aterm status` — one read of this machine: membership, vault, fleet, tools, repositories
    /// and agents, each with its next step and who takes it.
    Status,
    /// `aterm secrets` — the credentials vault, its keyless server and `~/.secrets`
    /// (docs/DESIGN-secrets-2026-10-20.md).
    Secrets,
    /// `aterm members` — Macs joining ALab and the members that let them in
    /// (docs/DESIGN-fluid-swarm-2026-10-28.md §3.3.3).
    Members,
    /// `aterm db` — embedded replicated application databases, offline and LAN.
    Db,
    /// `aterm email` — read configured mail accounts without opening a window.
    Email,
    /// `aterm harness` — Claude Code harness read views, accounts roster, and live upgrade.
    Harness,
    /// `aterm keeper` — the PTY keeper (`start | stop | status | serve`).
    Keeper,
    /// `aterm acaffeine` — the macOS menu-bar keep-awake helper.
    Acaffeine,
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
        Verb::ResourceInspect,
        Verb::Pkg,
        Verb::Agh,
        Verb::Fleet,
        Verb::Name,
        Verb::Node,
        Verb::Drive,
        Verb::Link,
        Verb::Fabric,
        Verb::Ship,
        Verb::Release,
        Verb::Update,
        Verb::Agents,
        Verb::Ai,
        Verb::Machines,
        Verb::Init,
        Verb::Status,
        Verb::Secrets,
        Verb::Members,
        Verb::Db,
        Verb::Email,
        Verb::Harness,
        Verb::Keeper,
        Verb::Acaffeine,
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
            Verb::ResourceInspect => "resource-inspect",
            Verb::Pkg => "pkg",
            Verb::Agh => "agh",
            Verb::Fleet => "fleet",
            Verb::Name => "name",
            Verb::Node => "node",
            Verb::Drive => "drive",
            Verb::Link => "link",
            Verb::Fabric => "fabric",
            Verb::Ship => "ship",
            Verb::Release => "release",
            Verb::Update => "update",
            Verb::Agents => "agents",
            Verb::Ai => "ai",
            Verb::Machines => "machines",
            Verb::Init => "init",
            Verb::Status => "status",
            Verb::Secrets => "secrets",
            Verb::Members => "members",
            Verb::Db => "db",
            Verb::Email => "email",
            Verb::Harness => "harness",
            Verb::Keeper => "keeper",
            Verb::Acaffeine => "acaffeine",
            Verb::Theme => "theme",
            Verb::Edit => "edit",
            Verb::NewTab => "new-tab",
            Verb::NewWindow => "new-window",
            Verb::SplitPane => "split-pane",
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
            | Verb::ResourceInspect
            | Verb::Pkg
            | Verb::Agh
            | Verb::Fleet
            | Verb::Name
            | Verb::Node
            | Verb::Drive
            | Verb::Link
            | Verb::Fabric
            | Verb::Ship
            | Verb::Release
            | Verb::Update
            | Verb::Agents
            | Verb::Ai
            | Verb::Machines
            | Verb::Init
            | Verb::Status
            | Verb::Secrets
            | Verb::Members
            | Verb::Db
            | Verb::Email
            | Verb::Harness
            | Verb::Keeper
            | Verb::Acaffeine
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
            Verb::ResourceInspect => "aterm resource-inspect --json",
            Verb::Pkg => "aterm pkg <args>",
            Verb::Agh => "aterm agh <args>",
            Verb::Fleet => "aterm fleet <args>",
            Verb::Name => {
                "aterm name [sync | set <mN> [--nick <word>] | nick <word>|--random] [--dry-run] [--json]"
            }
            Verb::Node => "aterm node [<cmd>]",
            Verb::Drive => "aterm drive <args>",
            Verb::Link => "aterm link <args>",
            Verb::Fabric => "aterm fabric [<cmd>]",
            Verb::Ship => "aterm ship <args>",
            Verb::Release => "aterm release probe <run|status|resume|cleanup> <options>",
            Verb::Update => "aterm update [<cmd>]",
            Verb::Agents => "aterm agents [<cmd>]",
            Verb::Ai => "aterm ai [<cmd>]",
            Verb::Machines => "aterm machines <cmd>",
            Verb::Init => {
                "aterm init [--plan|--check] [--json] [--develop|--no-develop] [--only <items>]"
            }
            Verb::Status => "aterm status [--json]",
            Verb::Secrets => "aterm secrets [<cmd>]",
            Verb::Members => "aterm members <cmd>",
            Verb::Db => "aterm db <cmd> --dir <database>",
            Verb::Email => "aterm email <cmd>",
            Verb::Harness => "aterm harness <cmd>",
            Verb::Keeper => "aterm keeper <cmd>",
            Verb::Acaffeine => "aterm acaffeine [<cmd>]",
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
            Verb::ResourceInspect => &[
                "Read the calling process's authenticated retained",
                "CPU and memory custody as versioned JSON.",
            ],
            Verb::ResourceRun => &[
                "Run a command with its CPU and memory charged to",
                "a tab (--session @self --job LABEL [--ai WORD]",
                "-- PROGRAM [ARGS...]).",
            ],
            Verb::Pkg => &["Install, update and verify the toolchain."],
            Verb::Agh => &[
                "GitHub reads and writes as the routed identity:",
                "JSON out, typed exits (`aterm agh help`).",
            ],
            Verb::Fleet => &[
                "The lab at a glance: its status line and one row",
                "per seat, from this Mac's own copy (`--json`);",
                "remove | admit | invite | requests | ignore, as",
                "`aterm members` does them; and this machine's",
                "membership in a fleet (init | join | peers | allow",
                "| access | publish | sync | leave). `access` is",
                "what other members may do here: read (the default)",
                "| write | full. `allow drive --from <host|node>`",
                "names a node that may drive this machine, as far",
                "as that level lets it (`aterm help fleet`,",
                "`aterm help workers`). Unix only.",
            ],
            Verb::Name => &[
                "This machine's numeric ID and display name, made",
                "by the first read; `set` chooses its ID and name,",
                "`nick` changes its name; `sync` aligns macOS",
                "with the saved name without choosing another.",
                "`--json` gives one object. Unix only.",
            ],
            Verb::Node => &[
                "The windowless fleet and Commons job: answers",
                "port 7100, wakes a window on spawn, and runs",
                "Commons repair (status | on | off | serve). Unix.",
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
            Verb::Release => &[
                "Prove an owned scratch binary can be signed,",
                "accepted by Apple and uploaded as a GitHub draft",
                "asset, then remove only its owned remote objects.",
                "Explicit nonce and private evidence path required.",
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
            Verb::Status => &[
                "This machine at a glance: membership, vault,",
                "fleet, tools, repositories and agents, each with",
                "its next step and who takes it (aterm, an agent",
                "or a person). Reads only; Unix only.",
            ],
            Verb::Email => &[
                "Read mail without a window (accounts | folders |",
                "list | read | watch); JSON Lines output.",
            ],
            Verb::Secrets => &[
                "One credentials file for every machine: the",
                "vault, its server and ~/.secrets (status | join |",
                "sync | edit | approve | serve …); `status` is the",
                "default. Unix only.",
            ],
            Verb::Members => &[
                "Macs joining ALab: a new Mac asks every member",
                "(join), a person presses Allow on any one and",
                "types the phrase it shows; any member removes",
                "(requests | allow | invite | ignore | list |",
                "remove | undo | join | status; `aterm fleet`",
                "runs the same lab verbs). Unix only.",
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
                "status | serve). On by default; macOS only.",
            ],
            Verb::Acaffeine => &[
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

    /// Resolve an `argv[1]` operand to a verb by its name. aCaffeine's old name, `caffeine`,
    /// is none: the front door answers it before the verb table (`aterm_acaffeine::legacy`).
    pub fn from_operand(operand: &str) -> Option<Verb> {
        Verb::ALL.iter().copied().find(|v| v.name() == operand)
    }
}

const VERB_BLURB_COLUMN: usize = 30;

fn help_block(synopsis: &str, blurb: &[&str]) -> String {
    let mut out = String::new();
    let mut lines = blurb.iter();
    let width = VERB_BLURB_COLUMN - 4;
    if synopsis.len() >= width {
        out.push_str(&format!("    {synopsis}\n"));
    } else {
        let first = lines.next().unwrap_or(&"");
        out.push_str(&format!("    {synopsis:<width$}{first}\n"));
    }
    for line in lines {
        out.push_str(&format!("{:width$}{line}\n", "", width = VERB_BLURB_COLUMN));
    }
    out
}

fn verb_help_block(verb: Verb) -> String {
    help_block(verb.usage(), verb.blurb())
}

fn diag_help_block(name: &str, desc: &str) -> String {
    let (synopsis, blurb): (&str, &[&str]) = match name {
        "show-config" => (
            "show-config",
            &[
                "Print the version, shell, TERM, size and",
                "containment default.",
            ],
        ),
        "explain-config" => (
            "explain-config",
            &["Explain how aterm resolves its configuration."],
        ),
        "doctor" => (
            "doctor",
            &[
                "Check health, instances, the fabric broker and the",
                "packages, and repair what aterm lays; --read-only",
                "changes nothing. --check, --apply, --restore: the",
                "macOS fleet settings profile alone.",
            ],
        ),
        "list-fonts" => ("list-fonts", &["List available font families."]),
        "show-face" => (
            "show-face <family>",
            &["Show a font family's file and cell metrics."],
        ),
        "list-themes" => (
            "list-themes",
            &[
                "List the built-in and custom themes (the same as",
                "`aterm theme list`).",
            ],
        ),
        "list-kitty-commands" => (
            "list-kitty-commands",
            &["List the words the cursor cat obeys, by language."],
        ),
        "exceptions" => (
            "exceptions [--all]",
            &[
                "List what needs a look later and the newest ten",
                "filed entries (--all: every filed entry).",
            ],
        ),
        _ => (name, std::slice::from_ref(&desc)),
    };
    help_block(synopsis, blurb)
}

fn help_text() -> String {
    let mut out = format!(
        "{HELP_TITLE}{}\n\n{HELP_SESSION_LINE}{HELP_HEAD}",
        aterm_types::identity::ORIGIN_LINE
    );
    out.push_str("SUBCOMMANDS (each exits without starting a shell):\n");
    for &(name, desc) in DIAG_COMMANDS {
        out.push_str(&diag_help_block(name, desc));
    }
    out.push('\n');
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

/// `aterm <cmd> --help` for a diagnostic: `aterm doctor`'s grammar and its forms
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
    out.push_str(&format!("version={}\n", aterm_stamp::app_version()));
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
    let split = atpkg::config_split::of_process();
    let home = aterm_types::dirs::home_dir();
    config_path_paragraph_with(
        aterm_types::dirs::aterm_config_path().as_deref(),
        split
            .as_ref()
            .map(|split| split.as_ref().zip(home.as_deref()))
            .map_err(String::as_str),
    )
}

/// The `aterm.toml:` paragraph for `path`, and, when the app opened from Finder reads another
/// file (`split`, under its home), where each reads and how to make them one
/// ([`atpkg::config_split`]).
/// An unread query prints its cause without guessing the app's file or offering a link.
fn config_path_paragraph_with(
    path: Option<&std::path::Path>,
    split: Result<Option<(&atpkg::config_split::Split, &std::path::Path)>, &str>,
) -> String {
    let path = match path {
        Some(path) => path.display().to_string(),
        #[cfg(windows)]
        None => "(unresolved: XDG_CONFIG_HOME, APPDATA and HOME are all unset)".to_string(),
        #[cfg(not(windows))]
        None => "(unresolved: XDG_CONFIG_HOME and HOME are unset)".to_string(),
    };
    let mut out = format!(
        "aterm.toml: {path}\n\
         \x20 (need not exist; a command-line flag beats it, it beats the default;\n\
         \x20 `aterm help config` has the keys)\n"
    );
    if let Ok(Some((split, home))) = split {
        let mut line = String::from("  Not the app's:");
        for word in split.said(home).split(' ') {
            if line.len() + 1 + word.len() > 88 {
                out.push_str(&line);
                out.push('\n');
                line = format!("  {word}");
            } else {
                line.push(' ');
                line.push_str(word);
            }
        }
        out.push_str(&line);
        out.push_str(".\n");
    }
    if let Err(reason) = split {
        out.push_str(&format!("  App configuration path: unknown ({reason}).\n"));
    }
    out.push('\n');
    out
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
     \x20 when it starts (the app, or an interactive terminal session; a window a running\n\
     \x20 aterm opens does not), and by a package pass (update, seed, install) when the\n\
     \x20 table changed since it was last applied. With [packages] enabled = false no\n\
     \x20 pass runs, so an edit waits for the daily apply.\n\
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
     \x20                         time: while Full Disk Access reads off, System Settings at\n\
     \x20                         Privacy & Security \u{25b8} Full Disk Access\n\
     \x20                         with aterm's panel beside it (its icon to drag into the list,\n\
     \x20                         I've switched it on, and Not now); and the Developer Tools\n\
     \x20                         page. Each page comes once per approval, never on a build\n\
     \x20                         whose grant would not last (ad hoc or unsigned); Not now holds\n\
     \x20                         until Permissions is set up again. aterm presses nothing there:\n\
     \x20                         switching it on is yours.\n\
     \x20                         Later, a grant aterm verified that reads off again (found at\n\
     \x20                         launch, or when macOS shows its \"access data from other apps\"\n\
     \x20                         dialog for a program of yours) reads Needs you on System\n\
     \x20                         State's Full Disk Access row; nothing is filed in Messages, and\n\
     \x20                         Not now on that step keeps the row quiet until Permissions is\n\
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
     \x20                         switch) in the window and both doctors. true: the full stream\n\
     \x20                         and a tab mark per dialog. enabled = false turns every mode\n\
     \x20                         off. Config only; nothing in a session can set it.\n\
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
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    opts.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
    let mut file = opts.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let len = meta.len();
    let cut = len > tail;
    if cut {
        file.seek(SeekFrom::Start(len - tail)).ok()?;
    }
    let mut bytes =
        Vec::with_capacity(usize::try_from(len.min(tail).saturating_add(1)).unwrap_or(0));
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
    let fallback = local_offset_s(i64::try_from(now_ms / 1000).unwrap_or(i64::MAX));
    exceptions_report_at_local(&path, all, now_ms, &|unix_s| {
        Some(local_offset_at(unix_s).unwrap_or(fallback))
    })
}

#[cfg(test)]
fn exceptions_report_at(path: &std::path::Path, all: bool, now_ms: u64, offset_s: i64) -> String {
    exceptions_report_at_local(path, all, now_ms, &|_| Some(offset_s))
}

fn exceptions_report_at_local(
    path: &std::path::Path,
    all: bool,
    now_ms: u64,
    offset_at: &dyn Fn(i64) -> Option<i64>,
) -> String {
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
    let lines = aterm_messages::exceptions_report_local(&read.log, scope, offset_at, now_ms);
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
    /// [`awake_fact`].
    awake: &'a (dyn Fn() -> Option<aterm_acaffeine::held::Held> + Sync),
    /// The `end=` token the leftover view offers a pid ([`offered_token_live`]), asked only of an
    /// unsupervised broker ([`aterm_ctl::census::supervise`]).
    offered: &'a (dyn Fn(u32) -> Option<String> + Sync),
    /// The `leftovers:` row's facts ([`doctor_leftovers::fact`]).
    leftovers: &'a (dyn Fn() -> doctor_leftovers::LeftoversFact + Sync),
    /// [`person_here`]: a hand-over may follow the report.
    person_here: &'a (dyn Fn() -> bool + Sync),
}

/// The fabric's row as [`fabric_fact`] reads it: its mark, its fact, whether it went
/// unmeasured, and who it found serving the fabric's socket
/// ([`aterm_link::fabric::DoctorSummary::serving`]).
type FabricFact = Option<(Mark, String, bool, Option<Option<u32>>)>;

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
        awake: &awake_fact,
        offered: &offered_token_live,
        leftovers: &doctor_leftovers::fact,
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
    let program_check = [
        "syspolicyd:",
        "devtools:",
        "instances:",
        "fabric:",
        "leftovers:",
    ]
    .into_iter()
    .any(reads)
    .then(ports.program_check)
    .flatten();
    let withheld = args_read_withheld(program_check.as_ref());
    let withheld = withheld.as_deref();
    use beside::Beside;
    fn unread<T>() -> Result<T, String> {
        Err("not read".to_owned())
    }
    // Each probe on a thread of its own, or in this one where no thread starts.
    let (mut rows, hand_over, rust_docs) = std::thread::scope(|scope| {
        let check_full = withheld.is_some();
        let devtools = reads("devtools:").then(|| {
            beside::start(scope, "aterm-doctor-devtools", move || {
                (ports.devtools)(fix, check_full)
            })
        });
        let spotlight = reads("spotlight:")
            .then(|| beside::start(scope, "aterm-doctor-spotlight", ports.spotlight));
        let services = reads("services:")
            .then(|| beside::start(scope, "aterm-doctor-services", ports.services));
        let privacy =
            reads("privacy:").then(|| beside::start(scope, "aterm-doctor-privacy", ports.privacy));
        let debugger = reads("debugger:")
            .then(|| beside::start(scope, "aterm-doctor-debugger", ports.debugger));
        let instances = reads("instances:").then(|| {
            beside::start(scope, "aterm-doctor-instances", move || match withheld {
                Some(why) => Err(why.to_owned()),
                None => (ports.instances)(),
            })
        });
        let fabric = reads("fabric:").then(|| {
            beside::start(scope, "aterm-doctor-fabric", move || {
                (ports.fabric)(withheld)
            })
        });
        let leftovers = reads("leftovers:").then(|| {
            beside::start(scope, "aterm-doctor-leftovers", move || match withheld {
                Some(why) => Err(why.to_owned()),
                None => (ports.leftovers)(),
            })
        });
        let awake =
            reads("awake:").then(|| beside::start(scope, "aterm-doctor-awake", ports.awake));
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
            fabric: fabric.and_then(Beside::answer),
            awake: awake.and_then(Beside::answer),
            spotlight: spotlight.and_then(Beside::answer),
            services: services.and_then(Beside::answer),
            shim_cache: reads("services:").then(shim_cache_fact).flatten(),
            network: reads("network:").then(network_fact).flatten(),
            lan: reads("lan:").then(lan_fact).flatten(),
            program_check,
            leftovers: leftovers.map(Beside::answer),
            ..MachineFacts::absent()
        };
        let privacy = privacy.map_or_else(
            || PrivacyFacts::not_measured(ProbeLabel::UnsupportedPlatform),
            Beside::answer,
        );
        let debugger = debugger.map_or_else(
            || DebuggerFacts::not_measured(DebuggerProbeLabel::UnsupportedPlatform),
            Beside::answer,
        );
        let mut instances = instances.map_or_else(unread, Beside::answer);
        if let (Ok(census), Some(serving)) = (
            instances.as_mut(),
            machine.fabric.as_ref().and_then(|(.., serving)| *serving),
        ) {
            aterm_ctl::census::supervise(census, serving, ports.offered);
        }
        let (devtools, devtools_due) =
            devtools.map_or_else(|| (DevtoolsFacts::absent(), None), Beside::answer);
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
    fabric: FabricFact,
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
    /// Unix: this Mac's network place in the words every reporter uses ([`network_fact`]):
    /// whether it is away from the ALab LAN, and the place's summary.
    network: Option<(bool, String)>,
    /// macOS: whether aCaffeine keeps this Mac awake now ([`awake_fact`]).
    awake: Option<aterm_acaffeine::held::Held>,
    /// macOS: the root LAN broker's install and whether its socket answers ([`lan_fact`]).
    lan: Option<LanFact>,
    /// What closed tabs and dead verify runs left running ([`doctor_leftovers::fact`]); `None`
    /// where the row was not read.
    leftovers: Option<doctor_leftovers::LeftoversFact>,
}

impl MachineFacts {
    const fn absent() -> Self {
        Self {
            copies: None,
            console_host: None,
            fabric: None,
            awake: None,
            spotlight: None,
            program_check: None,
            services: None,
            shim_cache: None,
            network: None,
            lan: None,
            leftovers: None,
        }
    }
}

/// This Mac's network place for doctor's `network:` row: whether it is off the ALab LAN (on
/// another network or offline: `aterm_netplace::words::off_the_alab_lan`; not while checking or
/// with no fleet to learn a home from, which need no note), and
/// `aterm_netplace::words::summary`. None in a unit test (the state root and route are the
/// person's), off Unix, or with no state directory.
fn network_fact() -> Option<(bool, String)> {
    #[cfg(all(unix, not(test)))]
    {
        let place = aterm_netplace::read(&aterm_types::dirs::state_dir()?);
        Some((
            aterm_netplace::words::off_the_alab_lan(&place),
            aterm_netplace::words::summary(&place, &aterm_netplace::words::Words::now()),
        ))
    }
    #[cfg(not(all(unix, not(test))))]
    {
        None
    }
}

/// Doctor's `network:` row: the place's summary, a note anywhere but the ALab LAN (being
/// elsewhere needs nobody; it says why the fleet does not answer).
fn network_rows(facts: &MachineFacts) -> Vec<DoctorRow> {
    facts
        .network
        .as_ref()
        .map(|(away, summary)| DoctorRow {
            label: "network:",
            mark: if *away { Mark::Note } else { Mark::Ok },
            fact: summary.clone(),
            unmeasured: false,
        })
        .into_iter()
        .collect()
}

/// The root LAN broker as this account reads it (`docs/DESIGN-lan-broker-2026-11-05.md` §5).
struct LanFact {
    /// Its install against the app bundle this aterm runs from (`aterm_uds::lan::install::
    /// read_here`): `None` outside a bundle (a development build).
    install: Option<Result<LanInstall, String>>,
    /// Whether something accepts a connection at its socket ([`aterm_uds::lan::socket_path`]).
    answers: bool,
}

/// The broker's install as `aterm_uds::lan::install::Reading` reads it, in what the row shows.
struct LanInstall {
    /// Both parts are as this build installs them.
    current: bool,
    /// Setup's one elevation owes it (a part missing or another build's).
    owed: bool,
    /// `image <state>, plist <state>`.
    words: String,
}

/// macOS: the root LAN broker's install and whether its socket answers a plain connect; `None`
/// elsewhere, where no broker runs.
fn lan_fact() -> Option<LanFact> {
    let (install, answers) = lan_read()?;
    Some(LanFact {
        install: install.map(|read| {
            read.map(|(current, owed, words)| LanInstall {
                current,
                owed,
                words,
            })
        }),
        answers,
    })
}

/// What [`lan_fact`] reads: the install (`current`, `owed`, its words), and whether the socket
/// answers.
type LanRead = (Option<Result<(bool, bool, String), String>>, bool);

/// [`lan_fact`]'s reads on macOS ([`aterm_uds::lan::install::read_here`], a plain connect to
/// [`aterm_uds::lan::socket_path`]); none elsewhere or in a test.
fn lan_read() -> Option<LanRead> {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        let install = aterm_uds::lan::install::read_here().map(|(_, reading)| {
            reading.map(|reading| (reading.current(), reading.owed(), reading.words()))
        });
        let answers =
            std::os::unix::net::UnixStream::connect(aterm_uds::lan::socket_path()).is_ok();
        Some((install, answers))
    }
    #[cfg(not(all(target_os = "macos", not(test))))]
    {
        None
    }
}

/// Doctor's `lan:` row: whether the broker is installed as this build installs it and whether
/// its socket answers. A broker that is not installed is a note (setup's one elevation installs
/// it, and nothing is wrong meanwhile but that aterm's own LAN traffic waits, or, where macOS
/// already allowed this aterm, goes by the allowed direct path, `aterm_uds::lan::direct`); an
/// installed one whose socket does not answer is a warning.
fn lan_rows(facts: &MachineFacts) -> Vec<DoctorRow> {
    let Some(lan) = &facts.lan else {
        return Vec::new();
    };
    let answers = if lan.answers { "yes" } else { "no" };
    let remedy = "sudo aterm machines here apply --plain installs it";
    let (mark, fact, unmeasured) = match &lan.install {
        None => (
            Mark::Note,
            format!("not read: this aterm runs outside an app bundle; socket answers: {answers}"),
            true,
        ),
        Some(Err(why)) => (
            Mark::Note,
            format!("not read: {why}; socket answers: {answers}"),
            true,
        ),
        Some(Ok(reading)) if reading.current => (
            if lan.answers { Mark::Ok } else { Mark::Warn },
            format!("installed ({}); socket answers: {answers}", reading.words),
            false,
        ),
        Some(Ok(reading)) => (
            Mark::Note,
            format!(
                "not installed as this build installs it ({}); socket answers: {answers}; \
                 {remedy}",
                reading.words
            ),
            !reading.owed,
        ),
    };
    vec![DoctorRow {
        label: "lan:",
        mark,
        fact,
        unmeasured,
    }]
}

/// macOS: whether aCaffeine keeps this Mac awake now, as macOS lists its assertion
/// ([`aterm_acaffeine::held::held`]); `None` elsewhere.
fn awake_fact() -> Option<aterm_acaffeine::held::Held> {
    cfg!(target_os = "macos").then(aterm_acaffeine::held::held)
}

/// The `awake:` row: whether the mug holds the assertion that keeps this Mac from idle sleep,
/// read from the power manager, not off the saved mode.
fn awake_rows(facts: &MachineFacts) -> Vec<DoctorRow> {
    use aterm_acaffeine::held::Held;
    let because = |why: &Option<String>| {
        why.as_ref()
            .map_or_else(String::new, |why| format!(" ({why})"))
    };
    let Some(held) = &facts.awake else {
        return Vec::new();
    };
    let (mark, fact, unmeasured) = match held {
        Held::NotHere(why) => (Mark::Note, format!("not read: {why}"), true),
        Held::Off => (
            Mark::Note,
            "aCaffeine is off by your choice; `aterm acaffeine on` keeps this Mac awake".to_owned(),
            false,
        ),
        Held::Held { pid } => (
            Mark::Ok,
            format!("aCaffeine (pid {pid}) holds macOS's idle-sleep assertion"),
            false,
        ),
        Held::Paused {
            pid,
            why,
            lingering,
        } => {
            let why = match why {
                aterm_acaffeine::model::Pause::LidClosed => {
                    "the lid is closed (`aterm acaffeine lid on` keeps this Mac running \
                     lid-closed, on AC power)"
                }
                aterm_acaffeine::model::Pause::Asleep => "this Mac is in a dark wake",
            };
            if *lingering {
                (
                    Mark::Warn,
                    format!(
                        "aCaffeine (pid {pid}) is paused because {why}, but macOS still lists an \
                         assertion of it; it lets go at its next look"
                    ),
                    false,
                )
            } else {
                (
                    Mark::Note,
                    format!(
                        "aCaffeine (pid {pid}) is paused and holds nothing because {why}; it \
                         holds again when that ends"
                    ),
                    false,
                )
            }
        }
        Held::NotHeld { pid, why } => (
            Mark::Warn,
            format!(
                "aCaffeine (pid {pid}) runs, but macOS lists no idle-sleep assertion of it{}; it \
                 asks again every minute",
                because(why)
            ),
            false,
        ),
        Held::Stopped { why } => (
            Mark::Warn,
            format!(
                "no aCaffeine mug runs, so this Mac may idle-sleep{}; `aterm acaffeine start` \
                 starts it",
                because(why)
            ),
            false,
        ),
        Held::Unread { pid, why } => (
            Mark::Note,
            format!(
                "aCaffeine (pid {pid}) runs; whether macOS holds its assertion is unread: {why}"
            ),
            true,
        ),
    };
    vec![DoctorRow {
        label: "awake:",
        mark,
        fact,
        unmeasured,
    }]
}

/// The window census ([`aterm_ctl::census::window_census`]) through the front door.
fn instances_fact() -> Result<aterm_ctl::census::Census, String> {
    aterm_ctl::census::window_census(FRONT_DOOR)
}

/// The `end=` token `aterm ctl sys left` would offer `pid` now: its facts read once on fresh
/// state ([`aterm_uds::left::read_one`], over this boot's verify run records in this user's
/// rendezvous, W-writer read with no socket connect), judged by [`offered_token`].
fn offered_token_live(pid: u32) -> Option<String> {
    use aterm_uds::runs::Kernel as _;
    let dir = aterm_uds::control_socket_dir();
    let boot = aterm_uds::process::boot_id();
    let records = dir
        .as_deref()
        .map(|dir| {
            aterm_uds::runs::records_dir(dir, aterm_uds::process::pid_namespace().as_deref())
        })
        .and_then(|records| aterm_uds::runs::read_records(&records, boot.as_deref()).ok())
        .unwrap_or_default();
    let me = std::process::id();
    let known = aterm_uds::left::Known {
        self_pid: me,
        now_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        boot: boot.as_deref(),
        records: &records,
        held: &[],
        waiting: &[],
        shells: &[],
    };
    let kernel = aterm_uds::left::Live {
        writer: |pid| {
            aterm_uds::control_socket_dir().is_some_and(|dir| aterm_ctl::graph_writer(&dir, pid))
        },
    };
    let facts = aterm_uds::left::read_one(&kernel, pid, &known).ok()??;
    offered_token(&facts, kernel.launchd_version(), me)
}

/// The token the leftover view's judge offers for `facts` ([`aterm_vitals::left::judge`]),
/// judged as a reading that knows no tab: only a run's witness (W-group or W-run with C-tree, of
/// a dead run) or W-writer can offer one, and nothing tied by a name. `None` for every other row.
fn offered_token(
    facts: &aterm_vitals::left::Facts,
    launchd: Option<i32>,
    me: u32,
) -> Option<String> {
    let spared = [me];
    let ctx = aterm_vitals::left::Ctx {
        launchd,
        live: &[],
        closed: &[],
        spared: &spared,
        tellers: &[],
    };
    aterm_vitals::left::judge(facts, &ctx)?.token
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

fn fabric_fact(withheld: Option<&str>) -> FabricFact {
    #[cfg(unix)]
    {
        let summary = aterm_link::fabric::doctor_summary(withheld);
        Some((
            fabric_mark(summary.level),
            summary.text,
            summary.unmeasured,
            summary.serving,
        ))
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

fn leftovers_rows(facts: &MachineFacts) -> Vec<DoctorRow> {
    facts
        .leftovers
        .as_ref()
        .map(|fact| {
            let (mark, words) = doctor_leftovers::row(fact);
            DoctorRow {
                label: "leftovers:",
                mark,
                fact: words,
                unmeasured: fact.is_err(),
            }
        })
        .into_iter()
        .collect()
}

fn fabric_rows(facts: &MachineFacts) -> Vec<DoctorRow> {
    facts
        .fabric
        .as_ref()
        .map(|(mark, fact, unmeasured, _)| DoctorRow {
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
    let version = aterm_stamp::app_version();
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
        held_programs: &aterm_uds::process::held_programs,
        refresh_verdict: &dt::refresh_verdict,
        window_responsible: &window_responsible,
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

/// What doctor did when the window's desk door refused its ask
/// ([`aterm_types::macos_setup::desk_refused`]): nothing, here or in the window.
const DESK_REFUSED_DID: &str = "did nothing: this tab is not the one in front of you, so the                                 aterm window brings no page up over what you are looking at;                                 run aterm doctor from the tab in front, or use System State's                                 Permissions rows";

/// Hand every grant doctor found missing to the running aterm window's permission walk
/// (`permissions walk now <subjects>`): one panel beside System Settings takes the person from
/// page to page, each turning to the next once its switch reads on, so doctor opens nothing
/// and waits for nothing. Full Disk Access goes to the walk only from the window macOS holds
/// responsible for doctor (`window_responsible`): under another terminal the grant doctor read
/// missing is that terminal's, so doctor opens its page itself and waits for its switch, and
/// only then hands Developer Tools to the walk. Where no walk takes them (no window, no answer,
/// a window that gave the request up before it took it (`ERR late`), a window whose reading has
/// them held already) doctor opens each page itself and waits for its switch; a page the
/// owner's Not now holds back from the walk (`OK declined`, or `not-now:<page>` in `OK walking`)
/// it opens nothing for; a window older than the walk takes Developer Tools through its own verb
/// ([`devtools_hand_over`]). Returns what it did, a line each.
fn permissions_hand_over(missing: &Missing, reach: &DevtoolsReach<'_>) -> Vec<String> {
    if missing.fda && !(reach.window_responsible)() {
        // The walk would turn on the window's own Full Disk Access, not the terminal's that
        // doctor read: its page here first, then the walk's, so the two are never up together.
        let mut did = vec![fda_open_here(reach)];
        let rest = Missing {
            fda: false,
            devtools: missing.devtools.clone(),
        };
        if let Some(rest) = rest.due() {
            did.extend(permissions_hand_over(&rest, reach));
        }
        return did;
    }
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
    if reply.starts_with("ERR late") {
        // The window's lane gave the request up before its main thread took it, so no walk
        // starts for it, then or later.
        return here(missing, reach);
    }
    if reply.starts_with("OK walking") {
        // The answer names the pages the walk has still to show. A page it names
        // `not-now:<page>` the owner's Not now holds back from it, which stands as `OK declined`
        // does: doctor opens nothing for it. Any other page it leaves out the window reads held,
        // its reading older than doctor's, as `OK granted` says, so doctor opens that page
        // itself.
        let walked = |word: &str| walk_names(reply, word);
        let not_now = |word: &str| !walked(word) && walk_holds_back(reply, word);
        let taken = Missing {
            fda: missing.fda && walked("fda"),
            devtools: missing.devtools.clone().filter(|_| walked("devtools")),
        };
        let held = Missing {
            fda: missing.fda && not_now("fda"),
            devtools: missing.devtools.clone().filter(|_| not_now("devtools")),
        };
        let left = Missing {
            fda: missing.fda && !taken.fda && !held.fda,
            devtools: missing
                .devtools
                .clone()
                .filter(|_| taken.devtools.is_none() && held.devtools.is_none()),
        };
        let mut did = Vec::new();
        if taken.fda || taken.devtools.is_some() {
            did.push(format!(
                "the aterm window walks you through {} beside System Settings: each page turns \
                 to the next once its switch reads on, and its Not now skips it",
                taken.pages()
            ));
        }
        if held.fda || held.devtools.is_some() {
            let (pages, them) = match (held.fda, held.devtools.is_some()) {
                (true, true) => ("Full Disk Access or Developer Tools", "them"),
                (true, false) => ("Full Disk Access", "it"),
                _ => ("Developer Tools", "it"),
            };
            did.push(format!(
                "did not open {pages}: you said Not now to {them}; System State's Permissions \
                 rows in the aterm window open {them} again"
            ));
        }
        did.extend(here(&left, reach));
        return did;
    }
    if reply == "OK granted" {
        // The window's reading has them held, doctor's a moment ago did not: the window's is
        // older.
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
    if aterm_types::macos_setup::desk_refused(reply) {
        return vec![DESK_REFUSED_DID.to_string()];
    }
    vec![format!("did nothing: the aterm window answered `{reply}`")]
}

/// `aterm init`'s hand-over of the grants its Permissions phase left to the person
/// (`aterm_machines::init::Host::walk`): the running window's permission walk takes them, for a
/// person at this Mac's console reading the run, Full Disk Access and Accessibility only when
/// that window is the app macOS holds responsible for the run
/// ([`aterm_machines::port::responsible_window`], [`walks`]). The line to print once it does;
/// `None` where no walk takes them, and the rows' own words stand.
#[must_use]
pub fn init_permissions_walk(missing: &[aterm_types::macos_setup::Access]) -> Option<String> {
    init_walk_over(
        missing,
        person_here(),
        window_responsible(),
        &ask_window_for_walk,
    )
}

/// Whether the running window is the app macOS holds responsible for this process
/// ([`aterm_machines::port::responsible_window`]), which charges it Full Disk Access and
/// Accessibility.
#[cfg(unix)]
fn window_responsible() -> bool {
    aterm_machines::port::responsible_window().is_some()
}

/// No app macOS holds responsible: this is not a Mac.
#[cfg(not(unix))]
const fn window_responsible() -> bool {
    false
}

/// [`init_permissions_walk`] over a person's presence, whether the window is the app macOS holds
/// responsible for the run (`own`), and the window's answer.
fn init_walk_over(
    missing: &[aterm_types::macos_setup::Access],
    here: bool,
    own: bool,
    ask_walk: &dyn Fn(&str) -> WindowAnswer,
) -> Option<String> {
    let walked: Vec<aterm_types::macos_setup::Access> = missing
        .iter()
        .copied()
        .filter(|access| walks(*access, own))
        .collect();
    if !here || walked.is_empty() {
        return None;
    }
    let words: Vec<&str> = walked.iter().map(|access| access.word()).collect();
    let WindowAnswer::Reply(reply) = ask_walk(&words.join(" ")) else {
        return None;
    };
    // The answer names the pages the walk has still to show: only those are the window's.
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
/// (`full-disk-access`, `developer-tools`), which the walk answers with its own. Its
/// `not-now:<subject>` words name pages the walk does not take ([`walk_holds_back`]).
fn walk_names(reply: &str, word: &str) -> bool {
    walk_words(reply).any(|walked| own_walk_word(walked) == own_walk_word(word))
}

/// Whether the window's `OK walking …` answer names `word`'s page `not-now:<subject>`: one asked
/// for that the owner's Not now holds back from the verb's walk. A window older than that word
/// never names one.
fn walk_holds_back(reply: &str, word: &str) -> bool {
    walk_words(reply).any(|walked| {
        walked
            .strip_prefix("not-now:")
            .is_some_and(|held| own_walk_word(held) == own_walk_word(word))
    })
}

/// The words after an `OK walking` answer's prefix; none for any other answer.
fn walk_words(reply: &str) -> impl Iterator<Item = &str> {
    reply
        .trim_end()
        .strip_prefix("OK walking")
        .unwrap_or_default()
        .split_whitespace()
}

/// The walk's own word for a Permissions row's (`full-disk-access`, `developer-tools`).
fn own_walk_word(word: &str) -> &str {
    match word {
        "full-disk-access" => "fda",
        "developer-tools" => "devtools",
        other => other,
    }
}

/// Whether the window's permission walk takes `access` for this run. Full Disk Access and
/// Accessibility macOS charges to the app it holds responsible for the run, and the walk turns on
/// the window's own, so it takes them only when the window is that app (`own`, aterm's own tab);
/// under another terminal their rows name that terminal's switch. Developer Tools is aterm's own
/// entry and the debugger this Mac's, whoever runs.
const fn walks(access: aterm_types::macos_setup::Access, own: bool) -> bool {
    use aterm_types::macos_setup::Access;
    match access {
        Access::DeveloperTools | Access::Debugger => true,
        Access::FullDiskAccess | Access::Accessibility => own,
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
    let now = std::time::Instant::now();
    let deadline = now
        .checked_add(std::time::Duration::from_secs(10))
        .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 86_400));
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
    let now = std::time::Instant::now();
    let until = now
        .checked_add(DEVTOOLS_WAIT)
        .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 86_400));
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
            std::thread::spawn(|| count_enters(std::io::stdin(), &LINES));
        }
    });
    &LINES
}

/// Count each `\n` read from `input` into `lines` until it ends or fails. Bytes, not text: a key
/// whose byte is not UTF-8 (a pasted Latin-1 `é`, a stray Meta key) is no Enter, and it does not
/// end the count.
fn count_enters(mut input: impl std::io::Read, lines: &std::sync::atomic::AtomicU64) {
    let mut read = [0u8; 256];
    loop {
        match input.read(&mut read) {
            Ok(0) => return,
            Ok(n) => {
                let enters = read[..n].iter().filter(|&&byte| byte == b'\n').count();
                lines.fetch_add(
                    u64::try_from(enters).unwrap_or(u64::MAX),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
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
    /// [`aterm_uds::process::held_programs`]: this user's programs macOS's program check holds
    /// now, read again once the switch reads on, since the report's count is a wait older.
    held_programs: &'a dyn Fn() -> Option<aterm_uds::process::HeldNow>,
    /// [`atpkg::devtools::refresh_verdict`]: make macOS ask about aterm again once the
    /// switch read on (the running aterm, checked before, keeps that verdict until a
    /// fresh aterm process asks).
    refresh_verdict: &'a dyn Fn() -> Result<(), atpkg::devtools::RefreshFailed>,
    /// Whether the running aterm window is the app macOS holds responsible for doctor
    /// ([`aterm_machines::port::responsible_window`]): only then is the Full Disk Access grant
    /// its walk turns on the one doctor read.
    window_responsible: &'a dyn Fn() -> bool,
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
        _ if aterm_types::macos_setup::desk_refused(&reply) => DESK_REFUSED_DID.to_string(),
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
        if switched {
            devtools_prime(facts.program_check, reach);
        }
        did.push_str(if switched {
            "; you switched it on"
        } else {
            "; not switched on while doctor waited"
        });
    }
    (did, true)
}

/// Asks macOS about aterm again once the switch read on ([`DevtoolsReach::refresh_verdict`]),
/// unless this user's programs read backed up at macOS's program check now, after the report's
/// count `before` ([`aterm_uds::process::HeldNow::backed_up`]): the prime is itself a new
/// program, which would wait in that line, so doctor starts none and says why. A count that does
/// not read proves no queue.
fn devtools_prime(before: Option<aterm_uds::process::HeldNow>, reach: &DevtoolsReach<'_>) {
    if let Some(held) = (reach.held_programs)().filter(|now| now.backed_up(before)) {
        (reach.say)(&prime_held_line(held));
    } else if let Err(why) = (reach.refresh_verdict)() {
        (reach.say)(&refresh_failed_line(&why));
    }
}

/// What doctor says when it holds its prime back from a backed-up queue ([`devtools_prime`]),
/// and what asks instead: an aterm process started after the grant is exempt at once, and from
/// then so are the programs the one running before it starts.
fn prime_held_line(held: aterm_uds::process::HeldNow) -> String {
    format!(
        "aterm doctor: did not ask macOS about aterm again: {}, and that ask is a new program \
         that would wait behind them; macOS asks again when aterm next starts",
        atpkg::devtools::held_words(held)
    )
}

/// What doctor says when asking macOS about aterm again did not take: a prime macOS's backed-up
/// queue still holds has not settled, which says nothing of the grant, so it is never "could not";
/// the prime's own words, where it said any, name the copy it kept.
fn refresh_failed_line(why: &atpkg::devtools::RefreshFailed) -> String {
    match why {
        atpkg::devtools::RefreshFailed::NotSettled(said) => format!(
            "aterm doctor: asking macOS about aterm again has not settled: {}",
            said.as_deref().unwrap_or("macOS's queue is backed up")
        ),
        atpkg::devtools::RefreshFailed::Failed(why) => {
            format!("aterm doctor: could not make macOS ask about aterm again: {why}")
        }
    }
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
        let mut facts: Vec<String> = [
            (p.adopted, "its launcher exited and launchd adopted it"),
            (
                p.unsupervised,
                "unsupervised: it is not the one broker found serving the fabric's socket",
            ),
            (p.build, "from a build directory"),
            (p.deleted, "running a deleted binary"),
        ]
        .into_iter()
        .filter(|(holds, _)| *holds)
        .map(|(_, said)| said.to_owned())
        .collect();
        if let Some(token) = &p.token {
            facts.push(format!("end it with `aterm ctl end {token}`"));
        }
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
    checks.extend(network_rows(machine));
    checks.extend(awake_rows(machine));
    checks.extend(lan_rows(machine));
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
    checks.extend(leftovers_rows(machine));
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
    out.push_str(&format!("version: {}\n\n", aterm_stamp::app_version()));
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
    /// `help [<topic> [<section>]]`: print a manual page or one of its sections ([`manual`]);
    /// exit 0, or 2 for an unknown topic or section.
    Manual {
        topic: Option<String>,
        section: Option<String>,
    },
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
        let topic = args.next();
        let section = args.next();
        if let Some(extra) = args.next() {
            return CliAction::Usage(format!(
                "aterm help: a topic and at most one section (`aterm help sys wait`), not {extra:?}"
            ));
        }
        return CliAction::Manual { topic, section };
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
                [all, extra, ..] if all == "--all" => CliAction::Usage(format!(
                    "aterm exceptions: unknown argument {extra:?} — usage: aterm exceptions [--all]"
                )),
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
    version_text_with_stamp(copy, aterm_stamp::installed())
}

/// [`version_text`] over an explicit [`aterm_stamp::Stamp`].
#[must_use]
pub fn version_text_with_stamp(
    copy: Option<&aterm_update::which_copy::WhichCopy>,
    stamp: Option<&aterm_stamp::Stamp>,
) -> String {
    let app_version = stamp.unwrap_or(&aterm_stamp::Stamp::UNSTAMPED).app_version;
    let mut out = format!(
        "aterm {app_version}\n{}\n",
        aterm_types::identity::ORIGIN_LINE
    );
    if let Some(build) = running_build_from(stamp) {
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

fn running_build_from(stamp: Option<&aterm_stamp::Stamp>) -> Option<u64> {
    let s = stamp?.build_number;
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let build = s.parse::<u64>().ok()?;
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
        CliAction::Manual { topic, section } => {
            let session = manual::in_session();
            let (out, code) =
                manual::render(topic.as_deref(), section.as_deref(), session.as_deref());
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
                use std::os::unix::fs::OpenOptionsExt as _;
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(dir)
                    .and_then(|f| f.set_permissions(std::fs::Permissions::from_mode(mode)))
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
    let mut moved: Vec<&str> = Vec::with_capacity(2);
    for dir in [reroute_dir, agents_dir].into_iter().flatten() {
        if !dir.is_empty() && !moved.contains(&dir) {
            moved.push(dir);
        }
    }
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

/// A PTY host's owned maintenance, with a deadline only while it still owes work.
#[cfg(unix)]
pub type SessionMaintenance = Box<dyn FnMut() -> Option<std::time::Instant> + Send>;

/// Run the transparent PTY session: containment init, root-authority mint, protected shell
/// spawn, and passthrough loop, retaining maintenance through shell settlement.
pub fn session_main(quiet: bool, #[cfg(unix)] maintenance: Option<SessionMaintenance>) -> ! {
    let code = session_code(
        quiet,
        #[cfg(unix)]
        maintenance,
    );
    std::process::exit(code);
}

fn session_code(quiet: bool, #[cfg(unix)] maintenance: Option<SessionMaintenance>) -> i32 {
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
            return 1;
        }
        _ => {
            eprintln!("aterm: containment mode {mode} denies spawning a shell (fail-closed)");
            return 1;
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

    let shell = match aterm_pty::spawn_shell_with_pid(
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
    ) {
        Ok(shell) => shell,
        Err(e) => {
            eprintln!("aterm: could not start the shell: {e}");
            return 1;
        }
    };

    {
        use std::io::IsTerminal as _;
        if !quiet && std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
            eprintln!(
                "aterm {} — session started · start with `aterm help` · `-q` hides this line",
                aterm_stamp::app_version()
            );
        }
    }

    driver::run(
        shell,
        flags.verbose,
        #[cfg(unix)]
        maintenance,
    )
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
