// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm`: a transparent, introspecting terminal.
//!
//! Spawns your shell in a PTY and passes I/O through unchanged; the host
//! terminal draws the bytes and nothing in this process reads them back (no VT
//! parse, grid or scrollback). This mode serves no control socket: the
//! introspectable surface driven via `aterm ctl` is the window mode
//! (`aterm --window`, or `--headless` for an engine + socket with no window).
//!
//! A thin platform driver (`poll(2)`/termios in `driver_unix`, console events
//! in `driver_windows`) over the [`aterm_pty`] spawn seam: cap-gated,
//! fail-closed fork/exec, resource-bounded in the confinement modes (see
//! `session_limits`) and OS-sandboxed when the containment mode demands it,
//! exactly like `aterm-gui`. Window resize is forwarded to the PTY, the loop is
//! EINTR-robust, and aterm exits with the shell's status.
//!
//! Containment mode is launcher-owned (`--containment`, ATERM_DESIGN §5):
//! the default is `User` — no OS sandbox, so the daily-driver shell keeps full
//! network/credential access and behaves as before, confined by the cap gate and
//! INHERITING the launching shell's `rlimit`s unchanged (the rule `aterm-gui`
//! applies at its own spawn — see `session_limits`; on Windows, where the same
//! `Limits` go onto the child's Job Object, that means no job caps); Safety /
//! Containment keep the hardened caps. `--sandbox` (`--containment containment`) opts
//! into the macOS Seatbelt sandbox (no network, no debugger attach, writes only to
//! the temp roots, no credential/private-data access) and is REFUSED where no OS
//! sandbox exists; a
//! malformed value fails CLOSED to Containment.

// The macOS consent tier (docs/DESIGN-macos-tcc-prompts-2026-08-30.md §3.3):
// owns the prompt-free Full Disk Access probe, the in-bundle fence that keeps
// headless paths away from `tccd`, and every protected-folder path literal
// (grep_guard B13). `doctor` threads its verdict into the pure report.
use aterm_containment::consent::{
    DEBUGGER_ACCESS_REVERT, DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel,
    DrClass, FdaState, ProbeGate, ProbeLabel, SelfHardening,
};

// The platform console driver behind a shared surface (host_winsize /
// shell_check / run): termios + poll on POSIX, ConPTY console events on Windows.
#[cfg(unix)]
#[path = "driver_unix.rs"]
mod driver;
#[cfg(windows)]
#[path = "driver_windows.rs"]
mod driver;

// The manual behind `aterm help [topic]`.
mod manual;

// The wt-shaped windowing grammar (`new-tab` / `new-window` / `split-pane`) and
// its `windowing_behavior` routing policy: pure parse + pure decision,
// unit-testable without a socket, window or config file.
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
    "        --containment=<MODE>  master (same as user), safety (capped limits, no OS\n",
    "                              sandbox) or containment (= --sandbox). The last\n",
    "                              flag wins; an invalid value fails closed to\n",
    "                              containment.\n",
    "        --sandbox             Shorthand for --containment containment (the macOS\n",
    "                              sandbox: no network; no debugger attach; writes\n",
    "                              only to temp dirs and shell history; no access to\n",
    "                              credential stores or private data; refused where\n",
    "                              no OS sandbox exists).\n",
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
    "    doctor                    Check shell, macOS access, instances, crashes;\n",
    "                              exit 1 only when the shell cannot start.\n",
    "    list-fonts                List available font families.\n",
    "    show-face <family>        Show a font family's file and cell metrics.\n",
    "    list-themes               List the built-in colour schemes.\n",
    "    list-kitty-commands       List the words the cursor cat obeys, by language.\n",
    "    exceptions [--all]        What needs a look but not now, and what was filed.\n",
    "\n",
);

/// How a Windows install is updated while no Windows updater exists, shared by
/// `aterm update status|check`, `aterm help update` and the manual's header.
/// Nothing checks, stages or applies a build on Windows; the lane is the build
/// scripts or the MSIX (`apps\aterm-win\msix\build-msix.ps1`).
///
/// The scripts are named only where the checkout has them (`win_scripts`, set by
/// build.rs). The public source snapshot ships no `apps/`, so a build from it
/// names a workspace rebuild with Trust's `targo` (the only compiler its build
/// accepts natively) and, as the README does, that Trust for Windows is not
/// published yet — in the const, so the verb and the page both carry it.
#[cfg(win_scripts)]
pub const WINDOWS_UPDATE_LANE: &str =
    "git pull, then apps\\aterm-win\\build.ps1 and apps\\aterm-win\\install.ps1";
/// The lane in a checkout without the Windows scripts.
#[cfg(not(win_scripts))]
pub const WINDOWS_UPDATE_LANE: &str = "git pull, then targo --unverified build --locked -p aterm \
     (this needs the Trust toolchain for Windows, which is not published yet)";

/// The front-door verb roster: the one place a verb exists.
///
/// A verb's name, help and dispatch are tied together here:
///   * name and help text live here and `--help` renders this list, so an
///     unadvertised verb is unrepresentable;
///   * dispatch in `crates/aterm/src/main.rs` matches this enum exhaustively, so
///     a new variant fails the build until it is routed;
///   * [`is_tool_candidate`] consults this list, so every verb is shielded from
///     toolchain-program shadowing;
///   * `crates/aterm/tests/front_door_verbs.rs` drives every variant through the
///     real binary under a real pty, where the mode fork picks the session.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Verb {
    /// `aterm ctl` — the introspection / control client.
    Ctl,
    /// `aterm conn` — session connections (the pull/push wiring front door).
    Conn,
    /// `aterm pkg` — the toolchain package manager.
    Pkg,
    /// `aterm fleet` — fleet federation (events + exec).
    Fleet,
    /// `aterm drive` — the agent drive CLI (sugar over await/send).
    Drive,
    /// `aterm link` — the fabric bridge (`serve`, `ls`, `hook`, `notify`, …).
    Link,
    /// `aterm fabric` — the fabric's state on one screen, its traffic live, and
    /// `on|off|doctor`: the one command that turns it on and proves it.
    Fabric,
    /// `aterm ship` — the release tool (publishing; source checkouts only).
    Ship,
    /// `aterm update` — the headless update lane (status/check, no window).
    Update,
    /// `aterm agents` — the coding-agent primer installer.
    Agents,
    /// `aterm harness` — the Claude Code harness's read views, the accounts
    /// roster (docs/DESIGN-claude-accounts-2026-10-01.md) and the live agent
    /// upgrade (docs/DESIGN-aterm-wrapper-2026-09-17.md §0.4). `aterm drive`
    /// supervises; `upgrade` only restarts an agent onto a newer build.
    Harness,
    /// `aterm keeper`: the PTY keeper (`start | stop | status | serve`), the
    /// per-login holder of a custody copy of every terminal's master
    /// (docs/DESIGN-pty-keeper-2026-09-26.md). Opt-in via `[keeper] enabled`.
    Keeper,
    /// `aterm new-tab` — a tab, routed by `windowing_behavior` (S12 / design §5).
    NewTab,
    /// `aterm new-window` — a window, unconditionally (the `attach` escape hatch).
    NewWindow,
    /// `aterm split-pane` — a pane beside the focused one.
    SplitPane,
}

impl Verb {
    /// Every verb, in the order `--help` lists them.
    pub const ALL: &'static [Verb] = &[
        Verb::Ctl,
        Verb::Conn,
        Verb::Pkg,
        Verb::Fleet,
        Verb::Drive,
        Verb::Link,
        Verb::Fabric,
        Verb::Ship,
        Verb::Update,
        Verb::Agents,
        Verb::Harness,
        Verb::Keeper,
        Verb::NewTab,
        Verb::NewWindow,
        Verb::SplitPane,
    ];

    /// The operand that selects this verb.
    pub const fn name(self) -> &'static str {
        match self {
            Verb::Ctl => "ctl",
            Verb::Conn => "conn",
            Verb::Pkg => "pkg",
            Verb::Fleet => "fleet",
            Verb::Drive => "drive",
            Verb::Link => "link",
            Verb::Fabric => "fabric",
            Verb::Ship => "ship",
            Verb::Update => "update",
            Verb::Agents => "agents",
            Verb::Harness => "harness",
            Verb::Keeper => "keeper",
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
            Verb::Pkg => Some("atpkg"),
            Verb::Fleet => Some("aterm-fleet"),
            Verb::Drive => Some("aterm-drive"),
            Verb::Link => Some("aterm-link"),
            // A report over what `aterm link` and `aterm ctl` reach; only a
            // front-door word (and `aterm-link fabric`).
            Verb::Fabric => None,
            // Generic names like `new-tab` do not belong on a user's `$PATH`.
            Verb::Ship
            | Verb::Update
            | Verb::Agents
            | Verb::Harness
            | Verb::Keeper
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
            | Verb::Pkg
            | Verb::Fleet
            | Verb::Drive
            | Verb::Link
            | Verb::Fabric
            | Verb::Ship
            | Verb::Update
            | Verb::Agents
            | Verb::Harness
            | Verb::Keeper => false,
        }
    }

    /// The `--help` synopsis column, e.g. `aterm ctl <args>`.
    pub const fn usage(self) -> &'static str {
        match self {
            Verb::Ctl => "aterm ctl <args>",
            Verb::Conn => "aterm conn [<cmd>]",
            Verb::Pkg => "aterm pkg <args>",
            Verb::Fleet => "aterm fleet <args>",
            Verb::Drive => "aterm drive <args>",
            Verb::Link => "aterm link <args>",
            Verb::Fabric => "aterm fabric [<cmd>]",
            Verb::Ship => "aterm ship <args>",
            Verb::Update => "aterm update [<cmd>]",
            Verb::Agents => "aterm agents [<cmd>]",
            Verb::Harness => "aterm harness <cmd>",
            Verb::Keeper => "aterm keeper <cmd>",
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
            Verb::Pkg => &["Install / update / verify the toolchain (the package manager)."],
            Verb::Fleet => &[
                "Watch and drive the sessions of every aterm window",
                "on this machine.",
            ],
            Verb::Link => {
                &["The fabric bridge: carry inbox/post between this instance and the bus."]
            }
            Verb::Fabric => &[
                "The fabric: mail between sessions and hosts (status | tail |",
                "on | off | doctor | mint-for | join); `status` is the default.",
            ],
            Verb::Drive => &[
                "Drive an agent (prompt | read | await | shot) and supervise",
                "it (supervise | watch | task | report | ledger).",
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
            Verb::Harness => &[
                "Claude Code's spend, limits and accounts, the disk, the ledger",
                "(usage | limits | accounts | disk | ledger); `upgrade` moves a",
                "live Claude Code onto a newer build. `aterm drive` supervises.",
            ],
            Verb::Keeper => &[
                "The PTY keeper: hold every terminal's master so a crashed",
                "window's shells live on (start | stop | status | serve). macOS, opt-in.",
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
    "                              `aterm pkg install <tool>` adds one.\n",
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
        "Check the shell, macOS access, where code lives and whether Spotlight skips it, running instances and past crashes; exits 1 only when the shell cannot start.",
    ),
    ("list-fonts", "List available font families (one per line)."),
    ("show-face", "Show a font family's file and cell metrics."),
    (
        "list-themes",
        "List the built-in colour schemes and their descriptions.",
    ),
    (
        "list-kitty-commands",
        "List the words the cursor cat obeys when typed (one row per trick and language).",
    ),
    (
        "exceptions",
        "List what needs a look but not now, and what was filed (--all: every filed entry in the newest MiB of the log).",
    ),
];

/// The refusal for `aterm show-face` with no family (exit 2).
const SHOW_FACE_NEEDS_FAMILY: &str =
    "aterm show-face: needs a font family — usage: aterm show-face <family>";

/// Build the `(report, exit_code)` for diagnostic subcommand `cmd` (with an
/// optional positional `arg`, e.g. the family for `show-face`), or `None` if
/// `cmd` is not a registry command. Read-only: never actuates containment or
/// spawns a shell. A non-zero code (`doctor`/`show-face` on bad input) makes
/// them scriptable.
fn diag_report(cmd: &str, arg: Option<&str>) -> Option<(String, i32)> {
    match cmd {
        "show-config" => Some((show_config_report(), 0)),
        "explain-config" => Some((explain_config_report(), 0)),
        "doctor" => Some(doctor_report()),
        "list-fonts" => Some((list_fonts_report(), 0)),
        "show-face" => Some(show_face_report(arg)),
        "list-themes" => Some((list_themes_report(), 0)),
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
         \x20              history; no access to credential stores or private data (macOS;\n\
         \x20              refused elsewhere).\n",
    );
    out.push_str("  safety       Capped limits, no OS sandbox.\n");
    out.push_str("  user         No sandbox, no extra limits (the default).\n");
    out.push_str("  master       Same as user.\n\n");
    out.push_str("No environment variable selects the containment mode.\n");
    #[cfg(windows)]
    out.push_str(WINDOWS_CONFIG_PARAGRAPH);
    // Both tables are macOS settings that do nothing on Windows.
    #[cfg(not(windows))]
    out.push_str(PRIVACY_CONFIG_PARAGRAPH);
    #[cfg(not(windows))]
    out.push_str(MACHINE_CONFIG_PARAGRAPH);
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
///
/// It says when the settings are applied (by `aterm pkg machine apply`, which
/// the window runs as it opens, a terminal session runs once a day, and a
/// package pass runs when the table changed); that `defaults` writes the
/// account's per-host domain, so a redirected `$HOME` is refused; and that
/// every change is undoable by one printed line.
#[cfg(not(windows))]
const MACHINE_CONFIG_PARAGRAPH: &str = "\n\
     [machine] — macOS host settings (aterm.toml; applied by the co-located atpkg):\n\
     \x20 universal_control       \"off\" (default) | \"leave\". Off writes the two per-host keys\n\
     \x20                         that stop the cursor and keyboard roaming to other Macs and\n\
     \x20                         iPads on the same Apple account; leave never touches them.\n\
     \x20 It is applied by `aterm pkg machine apply`, which aterm runs once a day at the\n\
     \x20 first window or interactive terminal session, and by a package pass (update,\n\
     \x20 seed, install) when the [machine] table changed since it was last applied;\n\
     \x20 `aterm pkg machine` reads the measured state, and Settings ▸ Security shows it\n\
     \x20 with the switch and an Apply now button. A saved change lands on the next\n\
     \x20 package pass, or on Apply now. `defaults` writes\n\
     \x20 the ACCOUNT's per-host domain regardless of $HOME, so an apply under a redirected\n\
     \x20 HOME is refused and says so, as is one whose aterm.toml does not parse (fix it\n\
     \x20 with `aterm --window --validate-config`). Undo Universal Control with the\n\
     \x20 revert line `aterm pkg machine` prints (`defaults -currentHost delete\n\
     \x20 com.apple.universalcontrol Disable`, then the same for DisableMagicEdges) AND set\n\
     \x20 universal_control = \"leave\", or the next apply disables it again.\n";

/// The `[privacy]` paragraph of `explain-config` (design §4).
///
/// It says that the probes (Full Disk Access's one `open()` and
/// `DevToolsSecurity -status`) read existing state and raise no dialog; that
/// the warm-up deliberately raises the prompts, so it runs only on an owner
/// gesture; and that no key is deferred. It does not claim a grant ends macOS
/// consent dialogs, since which services a grant covers is unmeasured.
#[cfg(not(windows))]
const PRIVACY_CONFIG_PARAGRAPH: &str = "\n\
     [privacy] — macOS consent (aterm.toml; the window reads it, the passthrough CLI does not):\n\
     \x20 enabled / check         the silent, prompt-free probes: Full Disk Access (opens one file),\n\
     \x20                         the debugger rule (`DevToolsSecurity -status`) and the Developer\n\
     \x20                         Tools answer (reads tccd's log). They read state that already\n\
     \x20                         exists and raise NO dialog. Off, every field reads `unknown` —\n\
     \x20                         which is not `denied`, and the report says which it is.\n\
     \x20 notice                  the Full Disk Access ask: after macOS shows the \"access data\n\
     \x20                         from other apps\" dialog for a program of yours (or refuses\n\
     \x20                         aterm a read), it files one entry in aterm's Messages with an\n\
     \x20                         Open Settings button. At most once a week; a window started\n\
     \x20                         before you pressed Open Settings waits 14 days; Dismiss all\n\
     \x20                         files it for 30 days, then 90.\n\
     \x20 report_attribution      per-session corroboration of the app macOS holds responsible for\n\
     \x20                         each session. It only corroborates: aterm's own adoption record\n\
     \x20                         decides whether a session reads `live` or `adopted`.\n\
     \x20 observer                absent: a narrow sentinel watches for the one log line macOS\n\
     \x20                         writes when it shows a consent dialog, only to decide when to\n\
     \x20                         ask. false: off. true: the full stream, the tab mark and a\n\
     \x20                         banner per dialog. Config only; nothing in a session can set it.\n\
     \x20                         false also stops the Developer Tools row's read of tccd's log.\n\
     \x20 warmup                  \"never\" | \"on-request\". The warm-up asks macOS for the folders\n\
     \x20                         up front, which RAISES the dialogs on purpose, so it happens\n\
     \x20                         only when the owner presses the button in Settings — never at\n\
     \x20                         first launch, never on a timer, and never because a program\n\
     \x20                         inside a session asked for it.\n\
     \x20 warmup_folders          what that gesture asks for. Default: Documents, Desktop and\n\
     \x20                         Downloads. `app-data` (every other app's own data) is a valid\n\
     \x20                         name but not asked for by default: macOS forgets that answer\n\
     \x20                         when the process that received it is replaced.\n\
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
                "family={family}\npath={}\npx={}\ncell_width={}\ncell_height={}\nbaseline={}\nglyph_count={}\n",
                info.path,
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

/// `aterm list-themes` — the built-in colour schemes + one-line descriptions.
/// Data: [`aterm_types::scheme::builtin_themes`].
fn list_themes_report() -> String {
    let mut out = String::from("Built-in colour schemes:\n\n");
    for (name, desc) in aterm_types::scheme::builtin_themes() {
        out.push_str(&format!("{name:<18} {desc}\n"));
    }
    out.push_str(
        "\nSet one in aterm.toml: theme = \"Nord\" (or \"dark:Nord,light:GitHub Light\" to follow the OS).\n",
    );
    out
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

/// The local zone's offset from UTC in seconds at `unix_s`, from `date +%z` (the
/// offset daylight saving gave THAT instant). `0` where `date` cannot say (Windows,
/// or no `date`): the times then read as UTC. One subprocess, in the print-and-exit
/// path only.
#[cfg(unix)]
fn local_offset_s(unix_s: i64) -> i64 {
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
        .unwrap_or(0)
}

#[cfg(not(unix))]
fn local_offset_s(_unix_s: i64) -> i64 {
    0
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
/// at the terminal, one row per (trick, language), then each language's names
/// for the pet and its filler words. Data: [`aterm_lexicon::TrickLexicon::all_rows`]
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

/// A doctor row's verdict. `Note` reports something worth acting on without
/// moving the exit code, so `aterm doctor && aterm` works on a machine without
/// Full Disk Access. Only [`Mark::Fail`] fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// The check passed.
    Ok,
    /// A fact worth reporting that is not a failure — nothing to fix, or
    /// nothing aterm can fix, and never a reason to refuse to launch.
    Note,
    /// The check failed: `health: FAIL` and exit 1.
    Fail,
}

impl Mark {
    /// The mark column's text.
    const fn render(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Note => "note",
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
    /// The verdict; only [`Mark::Fail`] moves the exit code.
    mark: Mark,
    /// The fact alone, no label (`/bin/sh (executable)`): the row prints it after
    /// the mark.
    fact: String,
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

/// Gather the consent facts for the `privacy:` row: the impure half, alongside
/// the shell check.
///
/// [`aterm_containment::probe_fda`] performs its one `open()` only when the
/// running executable resolves inside a `.app` (design §3.3, AGENTS.md), and
/// `ProbeLabel::refused()` reports whether a syscall happened. From
/// `target/debug/…` or a test binary it takes the refused arm: no probe, no
/// responsible-app lookup, no `codesign`.
fn privacy_facts() -> PrivacyFacts {
    let probe = aterm_containment::probe_fda(ProbeGate::on());
    if probe.label.refused() {
        return PrivacyFacts::not_measured(probe.label);
    }
    // Past the fence: this is the bundled CLI, so naming the responsible app and
    // reading the signature are both in bounds.
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

/// The class of the running bundle's designated requirement, which `tccd`
/// stores beside a grant: whether a grant survives the next build.
///
/// `codesign -d -r-` is the only unprivileged way to read it, and it is spawned
/// ONLY from inside a resolved `.app` (the same fence as the probe). Any failure
/// — no bundle, no `codesign`, a `codesign` that does not finish in 10 s,
/// unparseable output — is [`DrClass::Unknown`],
/// which claims nothing: the `privacy:` row names a dev build only on a
/// positively identified cdhash/unsigned requirement.
#[cfg(target_os = "macos")]
fn designated_requirement_class() -> DrClass {
    let Ok(exe) = std::env::current_exe() else {
        return DrClass::Unknown;
    };
    let Some(app_root) = aterm_containment::consent::app_bundle_root(&exe) else {
        return DrClass::Unknown;
    };
    // Within the ceiling containment's own `codesign -d` reads use: the read
    // takes tens of milliseconds, and when macOS's code-signing service
    // (trustd) does not answer, codesign never finishes (measured 2026-10-01)
    // and is killed — Unknown, which claims nothing, instead of a doctor that
    // hangs.
    let Ok(out) = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/codesign")
            .args(["-d", "-r-"])
            .arg(&app_root),
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    ) else {
        return DrClass::Unknown;
    };
    // codesign writes the requirement to stderr; read both streams so the
    // classifier sees whatever it printed.
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

/// The debugger-access facts behind the `debugger:` row
/// (docs/DESIGN-macos-debugger-access-2026-09-29.md §3.10), gathered by
/// [`debugger_facts`] and consumed by the pure [`doctor_checks_with_origin`] exactly as
/// [`PrivacyFacts`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DebuggerFacts {
    /// One `DevToolsSecurity -status` plus the membership query. A refused
    /// label means nothing was spawned.
    probe: DebuggerProbe,
    /// Whether this aterm has the hardened runtime without `get-task-allow` —
    /// read only past the fence, [`SelfHardening::Unknown`] otherwise.
    hardening: SelfHardening,
}

impl DebuggerFacts {
    /// What every host that measured nothing reports: no state, no claim.
    const fn not_measured(label: DebuggerProbeLabel) -> Self {
        Self {
            probe: DebuggerProbe::refused(label),
            hardening: SelfHardening::Unknown,
        }
    }
}

/// Gather the facts for the `debugger:` row, behind the same fence as
/// [`privacy_facts`]: [`aterm_containment::consent::probe_debugger`] spawns its
/// one `-status` only inside a `.app`, so from `target/debug/…` or a test
/// binary no spawn, directory query or `csops` happens.
///
/// The gate is [`ProbeGate::on`]: the passthrough CLI reads no aterm.toml and
/// `-status` raises no dialog. It observes and never actuates.
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

/// Why the debugger probe reports what it does ([`probe_reason`]'s twin).
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

/// The `debugger:` row: mark + detail, pure over [`DebuggerFacts`] (design
/// §4.6).
///
/// Never [`Mark::Fail`]: a Mac that asks for a password when lldb attaches is a
/// normal Mac, and nothing here is a reason to refuse to launch. It observes
/// only. Where developer mode would help it names the owner's switch as a PLACE,
/// aterm's Settings ▸ Security (ruling O7), never the command that turns it on,
/// on every build (ruling O8); an aterm that is not hardened says on the same
/// line that with developer mode on any program the owner runs could take
/// control of it. The one command it prints is the undo, and only once the rule
/// is on.
///
/// `mode=disabled` with an unknown membership says only what was measured: an
/// admin is also a `_developer` member, so an admin check that could not answer
/// leaves whether the switch would help unknown, and the row says that rather
/// than guess.
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
            // What turning it on would expose, for an aterm that is not
            // hardened (O8): said wherever the switch is named.
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
/// carries when it is not `$SHELL`: with `$SHELL` unset aterm-pty's resolver
/// falls back to `/bin/sh`, so that is what doctor checks — an unset `$SHELL`
/// is not a failure. An empty one is (the spawn execs `""`), and stays one.
#[cfg(not(windows))]
fn unix_shell(env_shell: Option<&str>) -> (&str, Option<&'static str>) {
    match env_shell {
        Some(shell) => (shell, None),
        None => ("/bin/sh", Some("$SHELL is unset, so aterm starts it")),
    }
}

/// `aterm doctor` — the shell (`$SHELL` on Unix, or `/bin/sh` when it is
/// unset; the shell a new WINDOW tab spawns on Windows), the macOS consent
/// posture, the aterm instances no control socket reaches, the recovery
/// ledger and open exceptions, plus the version. Reads env/fs + the consent
/// tier, then delegates to the pure [`doctor_checks_with_origin`]. Exit 1 only
/// when the shell cannot start. Scriptable: `aterm doctor && aterm`.
fn doctor_report() -> (String, i32) {
    let privacy = privacy_facts();
    let debugger = debugger_facts();
    let instances = aterm_ctl::census::window_census();
    let recovery = recovery_facts();
    let exceptions = exceptions_facts();
    let devroot = devroot_facts();
    // Unix: the program this passthrough session spawns.
    #[cfg(not(windows))]
    {
        let env_shell = std::env::var("SHELL").ok();
        let (shell, origin) = unix_shell(env_shell.as_deref());
        doctor_checks_with_origin(
            shell,
            driver::shell_check(shell),
            origin,
            &privacy,
            &debugger,
            &instances,
            &recovery,
            &exceptions,
            &devroot,
        )
    }
    // Windows: this tab's shell inside an aterm tab, else the one a new
    // window's tab spawns, labelled with its origin. See `driver::WindowShell`.
    #[cfg(windows)]
    {
        let window_shell = driver::WindowShell::resolve();
        doctor_checks_with_origin(
            &window_shell.program,
            driver::shell_check(&window_shell.program),
            Some(&window_shell.origin_sentence()),
            &privacy,
            &debugger,
            &instances,
            &recovery,
            &exceptions,
            &devroot,
        )
    }
}

/// The developer root as `doctor` reads it ([`atpkg::devroot`]): the survey of
/// `$HOME`'s checkouts, and whether Spotlight skips `~/Developer` as MEASURED for
/// this report. `survey` is `None` where there is no such arrangement (off macOS)
/// or no home to survey.
struct DevrootFacts {
    survey: Option<atpkg::devroot::Survey>,
    spotlight: atpkg::devroot::Spotlight,
}

impl DevrootFacts {
    /// Facts that measured nothing, for `why`.
    fn not_measured(why: &str) -> Self {
        Self {
            survey: None,
            spotlight: atpkg::devroot::Spotlight::NotMeasured(why.to_string()),
        }
    }

    /// No developer root here (off macOS): no row at all.
    fn absent() -> Self {
        Self {
            survey: None,
            spotlight: atpkg::devroot::Spotlight::Measured(atpkg::noindex::Verdict::NotApplicable),
        }
    }
}

/// Survey `$HOME` and MEASURE the root: a probe planted in `~/Developer` and a later
/// control beside it, removed whatever the answer (up to
/// [`atpkg::noindex::Timing::worst_case`], announced on stderr first so the wait never
/// reads as a hang). A test binary plants nothing in a real home: it surveys and says
/// it did not measure.
fn devroot_facts() -> DevrootFacts {
    if !atpkg::noindex::SUPPORTED {
        return DevrootFacts::absent();
    }
    let Some(home) = aterm_types::dirs::home_dir().filter(|h| !h.as_os_str().is_empty()) else {
        return DevrootFacts::not_measured("no home folder");
    };
    let survey = Some(atpkg::devroot::survey(&home));
    let spotlight = if cfg!(test) {
        atpkg::devroot::Spotlight::NotMeasured("a test binary plants nothing in a real home".into())
    } else {
        if let (root, atpkg::devroot::RootIs::Folder) = atpkg::devroot::root_at(&home) {
            eprintln!(
                "aterm doctor: measuring whether Spotlight skips {} (up to {}s)…",
                root.display(),
                atpkg::noindex::Timing::DEFAULT.worst_case().as_secs()
            );
        }
        atpkg::devroot::spotlight(&home)
    };
    DevrootFacts { survey, spotlight }
}

/// The `devroot:` and `spotlight:` rows, pure over the facts: atpkg's words, never a
/// failure (a warning reads `note` here: nothing in it stops a shell from starting).
fn devroot_rows(facts: &DevrootFacts) -> Vec<DoctorRow> {
    let mark = |level| match level {
        atpkg::devroot::Level::Ok => Mark::Ok,
        atpkg::devroot::Level::Warn | atpkg::devroot::Level::Note => Mark::Note,
    };
    let mut rows = Vec::new();
    if let Some(survey) = &facts.survey {
        let row = atpkg::devroot::devroot_row(survey);
        rows.push(DoctorRow {
            label: "devroot:",
            mark: mark(row.level),
            fact: row.text,
        });
    }
    if let Some(row) = atpkg::devroot::spotlight_row(&facts.spotlight) {
        rows.push(DoctorRow {
            label: "spotlight:",
            mark: mark(row.level),
            fact: row.text,
        });
    }
    rows
}

/// The recovery ledger as `doctor` reads it: its rows and its path, or `None` when
/// no log directory resolves (`aterm_update::recovery_ledger`).
type RecoveryFacts = Option<(Vec<aterm_update::recovery_ledger::Row>, std::path::PathBuf)>;

/// Read the recovery ledger — the rows the window's recovery census writes at each
/// windowed launch of the installed app. Read-only and bounded.
fn recovery_facts() -> RecoveryFacts {
    let path = aterm_update::recovery_ledger::default_ledger_path()?;
    Some((aterm_update::recovery_ledger::read_rows(&path), path))
}

/// The `recovery:` row, pure over the ledger: how the runs before each recorded
/// windowed launch ended, with the last unexpected end in detail. A note when
/// any run ended unexpectedly, OK otherwise. On Windows the window writes no
/// ledger, and the row says so.
fn recovery_row(facts: &RecoveryFacts) -> (Mark, String) {
    if cfg!(windows) {
        return (Mark::Note, "not recorded on Windows".to_string());
    }
    let Some((rows, path)) = facts else {
        return (
            Mark::Note,
            "not measured (no log folder, so there is no ledger)".to_string(),
        );
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

/// Pure core of `doctor` (all state is an input, so it is deterministically
/// testable): the checks as a stable report + exit code (0 = nothing failed,
/// 1 = any [`Mark::Fail`]).
///
/// The verdict is three-valued: `health`/the exit code are computed from `Fail`
/// ALONE, so a `Note` row — the macOS consent posture is the first — reports a
/// fact without making `aterm doctor && aterm` refuse to launch.
///
/// The `shell:` row is unlabelled here; [`doctor_checks_with_origin`] is the
/// same report with the row labelled, which is what the live paths call.
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
        instances,
        recovery,
        exceptions,
        &DevrootFacts::absent(),
    )
}

/// What to do when the shell cannot start — the one row that can fail.
const SHELL_REMEDY: &str = if cfg!(windows) {
    "set `shell` in aterm.toml to an installed shell"
} else {
    "set $SHELL to an installed shell"
};

/// `doctor_checks` with the `shell:` row labelled by WHERE the shell came from
/// (`shell_origin`, appended after the fact as ` — <label>`). Windows passes
/// `driver::WindowShell::origin_sentence`, so the row says whose shell it names
/// and which input chose it — a reader who sees `pwsh.exe` from inside a
/// Git Bash tab must not have to guess whether doctor looked at `$SHELL`. Unix
/// labels only the `/bin/sh` fallback ([`unix_shell`]). `None` renders exactly
/// the unlabelled row.
///
/// The privacy and debugger rows exist only where their probe applies: on a
/// platform without them (`UnsupportedPlatform`) the rows are left out rather
/// than printed as a `note` nobody can act on.
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
    instances: &Result<aterm_ctl::census::Census, String>,
    recovery: &RecoveryFacts,
    exceptions: &ExceptionsFacts,
    devroot: &DevrootFacts,
) -> (String, i32) {
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
    }];
    if privacy.fda_label != ProbeLabel::UnsupportedPlatform {
        let (mark, fact) = privacy_row(privacy);
        checks.push(DoctorRow {
            label: "privacy:",
            mark,
            fact,
        });
    }
    if debugger.probe.label != DebuggerProbeLabel::UnsupportedPlatform {
        let (mark, fact) = debugger_row(debugger);
        checks.push(DoctorRow {
            label: "debugger:",
            mark,
            fact,
        });
    }
    checks.extend(devroot_rows(devroot));
    let (instances_mark, instances_fact) = instances_row(instances);
    let (recovery_mark, recovery_fact) = recovery_row(recovery);
    let (exceptions_mark, exceptions_fact) = exceptions_row(exceptions);
    checks.extend([
        DoctorRow {
            label: "instances:",
            mark: instances_mark,
            fact: instances_fact,
        },
        DoctorRow {
            label: "recovery:",
            mark: recovery_mark,
            fact: recovery_fact,
        },
        DoctorRow {
            label: "exceptions:",
            mark: exceptions_mark,
            fact: exceptions_fact,
        },
    ]);

    // Only the shell row can be `Fail`, so a failing report names it and the fix.
    let all_ok = !checks.iter().any(|row| row.mark.is_fail());

    // One row per check: the label, the mark, the fact. A fact of several lines
    // keeps its later lines under the fact column.
    const LABEL: usize = "exceptions: ".len();
    let mut out = String::new();
    for row in &checks {
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
    if all_ok {
        out.push_str("health: OK\n");
    } else {
        out.push_str(&format!(
            "health: FAIL — aterm cannot start this shell; {SHELL_REMEDY}\n"
        ));
    }
    (out, i32::from(!all_ok))
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

/// The `-V`/`--version` text:
///
/// * `aterm <version>`, the identity line `tools/install.sh` greps (`^aterm `);
/// * `build: <N>`, the monotonic build number the updater orders by (the
///   pre-swap start probe requires it to name the build being installed);
///   omitted when the binary installed no stamp, or build `0` (no git);
/// * which copy runs (`docs/DESIGN-which-copy-runs-2026-08-27.md`):
///   `running: <path>` and `another copy: <path> (<version>) — …` lines as
///   spelled by `aterm_update::which_copy`; identity only with no executable
///   path;
/// * `trusts: master=<sha256>`, the fingerprint of the compiled-in paper
///   master (`empty` names an unarmed tier).
#[must_use]
pub fn version_text(copy: Option<&aterm_update::which_copy::WhichCopy>) -> String {
    // Identity line first (install.sh greps `^aterm `), then the origin line,
    // then build number, which copy runs, and trust roots.
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

/// The running build number from the binary's installed stamp
/// (`aterm_types::version`); `None` without one, or for build `0` (no git).
fn running_build() -> Option<u64> {
    let build = aterm_types::version::installed()?
        .build_number
        .parse::<u64>()
        .ok()?;
    (build > 0).then_some(build)
}

/// Hook run when the session's shell first writes output. The launcher
/// (`crates/aterm/src/main.rs`) uses it to confirm a replaced Linux
/// executable's trial: a prompt on screen proves a healthy start. First
/// registration wins.
static FIRST_SHELL_OUTPUT: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Register the hook [`FIRST_SHELL_OUTPUT`] names; `false` when one is already
/// set. It runs once on the passthrough loop, so it must not block. Register it
/// before [`session_main`] spawns the shell.
pub fn on_first_shell_output(hook: fn()) -> bool {
    FIRST_SHELL_OUTPUT.set(hook).is_ok()
}

/// The shell's first output reached the terminal: run the hook, if one is registered.
#[cfg(unix)]
fn first_shell_output() {
    if let Some(hook) = FIRST_SHELL_OUTPUT.get() {
        hook();
    }
}

/// The `trusts:` line of [`version_text`].
fn trust_anchors_line() -> String {
    format!(
        "trusts: master={}\n",
        aterm_update::compiled_update_pin_sha256()
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
        CliAction::Help => {
            print!("{}", help_text());
            std::process::exit(0);
        }
        CliAction::Version => {
            print!(
                "{}",
                version_text(aterm_update::which_copy::observe().as_ref())
            );
            std::process::exit(0);
        }
        CliAction::DiagHelp { cmd } => {
            let desc = DIAG_COMMANDS
                .iter()
                .find(|(name, _)| *name == cmd)
                .map(|(_, d)| *d)
                .unwrap_or("");
            let usage = if cmd == "show-face" {
                format!("aterm {cmd} <family>")
            } else if cmd == "exceptions" {
                format!("aterm {cmd} [--all]")
            } else {
                format!("aterm {cmd}")
            };
            println!("{cmd} — {desc}");
            println!("usage: {usage}");
            std::process::exit(0);
        }
        CliAction::Diag { cmd, arg } => {
            // `decide_args` only emits registry names, so `diag_report` is Some.
            match diag_report(&cmd, arg.as_deref()) {
                // Success goes to stdout; a non-zero result (e.g. doctor on a
                // broken shell) goes to stderr and sets the exit code, so it scripts.
                Some((report, 0)) => {
                    print!("{report}");
                    std::process::exit(0);
                }
                Some((report, code)) => {
                    eprint!("{report}");
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
                print!("{out}");
            } else {
                eprint!("{out}");
            }
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
                print!("{out}");
            } else {
                eprint!("{out}");
            }
            std::process::exit(code);
        }
        CliAction::Usage(msg) => {
            eprintln!("{msg}");
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

/// The managed `agents/` directory (shims for `claude`, `codex`) a TTY session
/// puts in front of its shell's PATH, as the window's spawn seam does
/// (`aterm-gui`'s `spawn::managed_agents_dir`).
///
/// This is a pre-rc hint: the session spawns a login shell with no aterm shell
/// integration, so on macOS `path_helper` rebuilds PATH before any rc runs.
/// What puts `agents/` (and, inside aterm, `reroute/` ahead of it) first at the
/// prompt is the rc-sourced `~/.aterm/shell.d/00-atpkg.<shell>` hook, once at
/// shell start; there is no per-prompt re-assert in this lane. Pinned by
/// `a_login_zsh_demotes_the_seams_front_insert_and_the_rc_hook_puts_reroute_first_in_aterm`.
///
/// Which directory, in order (pinned by
/// `managed_agents_dir_prefers_the_front_doors_handoff_then_the_sibling_then_the_hook`):
///
/// 1. `handed`: `$ATERM_AGENTS_DIR`, the front door's handoff. It resolves the
///    store and ensures `<prefix>/agents` via `Layout::ensure_agents_dir` on
///    every lane (reroute engaged or not), and clears an inherited stray when
///    it hands nothing. Taken when it is an absolute path to an existing real
///    directory (not a symlink); nothing is created here.
/// 2. The sibling of the reroute directory (`$ATERM_REROUTE_DIR`): `reroute/`
///    and `agents/` share one manager prefix (pinned by
///    `the_agents_dir_is_the_reroute_dirs_sibling_in_atpkgs_layout`). The
///    fallback when the front door refused; ensured here, silently on refusal.
/// 3. `enclosing_agents`: `$ATPKG_AGENTS` from an enclosing hook-sourced shell,
///    when it is an absolute existing directory.
///
/// Rule 2 ensures the directory as `Layout::ensure_dir` does, with the mode
/// from the prefix's metadata ([`agents_dir_mode`]): one `mkdir`, never
/// through a symlink at `agents/`, and a refusal is silent here because the
/// front door already reported it. No `ATPKG_AGENTS` is exported, so the shell
/// integration still sources the hook.
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
            // Refused: the front door already reported it once with the right
            // remedy. Fall through to rule 3.
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
/// "candidate"; the co-located `atpkg` decides whether it is installed. Pure.
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

/// The [`aterm_sandbox::Limits`] the session hands the spawn seam, by
/// containment mode (the same rule as `aterm-gui`'s `spawn.rs` and
/// [`aterm_sandbox::Limits::inherit`]'s doc): `user` (the default) and `master`
/// request nothing, so the shell inherits the launching shell's `rlimit`s;
/// `safety` and `containment` keep [`aterm_sandbox::Limits::shell_default`].
///
/// On Windows the same set goes onto the child's Job Object: `inherit()`
/// installs no job caps; `shell_default()` installs 16 GiB per-process memory,
/// 512 active processes and the UI restrictions.
///
/// Capping in the daily-driver modes would force the soft `RLIMIT_NOFILE` to
/// the cap in both directions (the actuator sets `min(cap, hard)`); a terminal
/// must not constrain programs more than the shell that started it.
fn session_limits(mode: aterm_containment::ContainmentMode) -> aterm_sandbox::Limits {
    use aterm_containment::ContainmentMode as Cm;
    match mode {
        Cm::Master | Cm::User => aterm_sandbox::Limits::inherit(),
        // Safety / Containment — and, the enum being `#[non_exhaustive]`, any
        // mode this build does not know: fail-safe to the hardened caps.
        _ => aterm_sandbox::Limits::shell_default(),
    }
}

/// The whole transparent session: containment init, the root-authority mint,
/// the protected shell spawn, and the passthrough loop. Never returns. The one
/// `aterm` binary calls this after routing; `quiet` is `-q`/`--quiet`.
pub fn session_main(quiet: bool) -> ! {
    // One stderr line before the shell spawns, interactive sessions only, so a
    // bare `aterm` visibly did something and `aterm help` is discoverable. It
    // never enters the PTY stream; piped/CI runs stay silent; `-q` silences it.
    {
        use std::io::IsTerminal as _;
        if !quiet && std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
            eprintln!(
                "aterm {} — session started · start with `aterm help` · `-q` hides this line",
                aterm_types::version::app_version()
            );
        }
    }

    let (rows, cols) = driver::host_winsize();

    // The shell runs through the protected spawn seam. Containment mode is
    // launcher-owned: default `User` has no OS sandbox and inherited rlimits
    // (`session_limits`); `--sandbox` opts into Seatbelt; a malformed value
    // fails closed to Containment.
    let flags = session_flags();
    let mode = aterm_containment::init_mode_from_flag(
        flags.containment.as_deref(),
        aterm_containment::ContainmentMode::User,
    )
    .unwrap_or_else(|e| {
        if matches!(e, aterm_containment::InitModeFromFlagError::Parse(_)) {
            eprintln!(
                "aterm: --containment takes master, user, safety or containment; using containment"
            );
        } else {
            eprintln!("aterm: {e}; using containment");
        }
        let _ = aterm_containment::init_mode(aterm_containment::ContainmentMode::Containment);
        aterm_containment::ContainmentMode::Containment
    });

    // Ask the actuator whether the shell may spawn and, for Containment on
    // macOS, the SBPL profile to wrap it in. `Deny` fails closed: no shell, and
    // the reason on stderr.
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

    // The single root-authority mint in this binary, granting the
    // spawn + sandbox capabilities the seam requires.
    // SAFETY: trusted process entry point, reached exactly once before the spawn
    // and before any untrusted PTY input is read.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

    // The session PATH, one `("PATH", value)` pair composed like the window's
    // `env_add` (`docs/DESIGN-toolchain-reroute-2026-09-07.md` §"Reaching
    // PATH"): the reroute dir (so a bare `cargo`/`rustc` is announced or
    // signposted), the managed `agents/` (`managed_agents_dir`), aterm's own
    // binary dir, then the inherited PATH. `--no-reroute` means no reroute
    // prepend and no export.
    //
    // This holds only until the login shell's startup files run (see
    // `managed_agents_dir`). The reroute and agents dirs are re-exported
    // (blank when not taken) so a nested window or session reads the same
    // handles; `ATPKG_AGENTS` is not exported.
    let reroute_dir = reroute_dir_from_env();
    let agents_dir = managed_agents_dir(
        std::env::var(AGENTS_DIR_ENV).ok().as_deref(),
        reroute_dir.as_deref(),
        std::env::var(HOOK_AGENTS_ENV).ok().as_deref(),
    );
    let mut env_add: Vec<(String, String)> = session_path_env(
        reroute_dir.as_deref(),
        agents_dir.as_deref(),
        front_door_bin_dir().as_deref(),
        std::env::var("PATH").ok().as_deref(),
    )
    .into_iter()
    .collect();
    if let Some(dir) = &reroute_dir {
        env_add.push((REROUTE_DIR_ENV.to_string(), dir.clone()));
    } else if std::env::var_os(REROUTE_DIR_ENV).is_some() {
        // The escape is engaged (or nothing is laid) and an enclosing session's
        // directory was inherited: blank it so "no export" means "not set".
        env_add.push((REROUTE_DIR_ENV.to_string(), String::new()));
    }
    if let Some(dir) = &agents_dir {
        env_add.push((AGENTS_DIR_ENV.to_string(), dir.clone()));
    } else if std::env::var_os(AGENTS_DIR_ENV).is_some() {
        env_add.push((AGENTS_DIR_ENV.to_string(), String::new()));
    }
    // `ATERM_CHILD` is not set in this lane: `net_listen`'s root-only nesting
    // guard reads it, and the reroute gates only on its own two variables.

    // Protected spawn: cap-gated, fail-closed, resource-bounded per
    // `session_limits` (`setrlimit` before execve on POSIX, the Job Object on
    // Windows), and sandbox-wrapped when `sandbox_wrap` is `Some` (the seam
    // refuses to spawn unsandboxed if a demanded wrapper is missing). Returns
    // the PTY master + child pid; the unix driver waits on exactly this pid,
    // never `waitpid(-1)`.
    let shell = aterm_pty::spawn_shell_with_pid(
        rows,
        cols,
        &spawn_cap,
        &sandbox_cap,
        &env_add, // the session PATH and the reroute/agents handles, above
        None,     // shell_override — platform default
        None,     // shell_args
        None,     // argv_override
        None,     // exec_command — interactive $SHELL
        None,     // cwd — inherit
        sandbox_wrap.as_deref(),
        session_limits(mode), // by mode — NOT a blanket `shell_default()`; see its doc
    )
    .unwrap_or_else(|e| {
        eprintln!("aterm: could not start the shell: {e}");
        std::process::exit(1);
    });

    // The platform driver owns raw mode, the passthrough loop, resize
    // forwarding, terminal restore and the reap, and returns the shell's exit
    // status (non-exit → 1).
    let code = driver::run(shell, flags.verbose);
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::{
        AGENTS_DIR_ENV, CliAction, DEBUGGER_ACCESS_REVERT, DIAG_COMMANDS, DebuggerFacts,
        DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, DevrootFacts, DrClass,
        ExceptionsFacts, FdaState, HOOK_AGENTS_ENV, Mark, PASSTHROUGH_ENV, PrivacyFacts,
        ProbeLabel, REROUTE_DIR_ENV, RecoveryFacts, SelfHardening, ShellCheck, VERB_BLURB_COLUMN,
        Verb, agents_dir_mode, decide_args, diag_report, doctor_checks, doctor_checks_with_origin,
        doctor_report, exceptions_report_at, explain_config_report, help_text, is_tool_candidate,
        list_fonts_report, list_themes_report, managed_agents_dir, parse_zone_offset, prepend_path,
        read_exceptions_log, reroute_dir_from_values, session_limits, session_path_env,
        show_face_report, verb_help_block, version_text,
    };
    // The Windows-only tests read the window's shell and the Windows paragraphs.
    use super::driver;
    #[cfg(windows)]
    use super::show_config_report;
    #[cfg(unix)]
    use super::unix_shell;

    fn decide(args: &[&str]) -> CliAction {
        decide_args(args.iter().map(|s| s.to_string()))
    }

    /// The privacy facts a test binary produces: nothing measured (the
    /// in-bundle fence refuses the probe).
    fn unmeasured() -> PrivacyFacts {
        PrivacyFacts::not_measured(ProbeLabel::RefusedOutOfBundle)
    }

    /// The debugger facts a test binary produces: nothing measured.
    fn no_debugger() -> DebuggerFacts {
        DebuggerFacts::not_measured(DebuggerProbeLabel::RefusedOutOfBundle)
    }

    /// A completed debugger probe, for the rows that only exist on a real install.
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

    /// An empty recovery ledger: the default for every doctor test that is not
    /// about the `recovery:` row itself.
    fn no_recovery() -> RecoveryFacts {
        Some((
            Vec::new(),
            std::path::PathBuf::from("/logs/recovery-ledger.log"),
        ))
    }

    /// Nothing open in the exceptions log: the default for every doctor test that is
    /// not about the `exceptions:` row itself.
    fn no_exceptions() -> ExceptionsFacts {
        Ok(0)
    }

    /// A census with nothing running: the default for every doctor test that is
    /// not about the `instances:` row itself.
    fn no_census() -> Result<aterm_ctl::census::Census, String> {
        Ok(aterm_ctl::census::Census {
            dir: "/run/aterm".to_string(),
            instances: 0,
            unreached: Vec::new(),
        })
    }

    /// A measured posture, for the rows that only exist on a real install.
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
            "doctor",
            "list-fonts",
            "list-themes",
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

    /// THE READER WORKS WITH ATERM NOT RUNNING: it replays `messages.log` (and its
    /// rotated pair) with the engine's own decoder and prints what needs a look with
    /// why the owner is needed and the one action, then what was filed, newest
    /// first; a torn line is counted, a missing file is said.
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

    #[test]
    fn diag_subcommand_only_as_first_operand_not_after_flags() {
        // A subcommand name after a flag is not a subcommand (git-style: first only),
        // so it falls through to the option parser and is rejected as unknown.
        assert!(matches!(
            decide(&["--no-sandbox", "show-config"]),
            CliAction::Usage(_)
        ));
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
            assert!(
                diag_report(name, None).is_some(),
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

    /// `--help` names the shell the session runs (`$SHELL` on Unix, the
    /// platform default on Windows), right under the origin line.
    #[test]
    fn help_names_the_shell_the_session_runs() {
        let help = help_text();
        let after_origin = format!("{}\n\n", aterm_types::identity::ORIGIN_LINE);
        let session = help
            .split_once(after_origin.as_str())
            .map(|(_, rest)| rest.split("\n\n").next().unwrap_or_default())
            .unwrap_or_else(|| panic!("no origin line in\n{help}"));
        if cfg!(windows) {
            assert_eq!(
                session,
                "Runs pwsh (else Windows PowerShell, else %COMSPEC%) in a ConPTY and passes its\n\
                 bytes through unchanged."
            );
            assert!(!help.contains("$SHELL"), "{help}");
        } else {
            assert_eq!(
                session,
                "Runs your $SHELL in a PTY and passes its bytes through unchanged."
            );
        }
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

    /// Every argv0 alias the roster yields is symlinked by the macOS bundler and
    /// routed by the front door's `alias_route`. `aterm-release` does not depend
    /// on this crate, so the pin reads both source files.
    #[test]
    fn every_argv0_alias_is_bundled_and_routed() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/aterm-cli sits under crates/");
        let bundle = std::fs::read_to_string(root.join("aterm-release/src/bundle.rs"))
            .expect("the release bundler");
        let front_door =
            std::fs::read_to_string(root.join("aterm/src/main.rs")).expect("the one binary's main");
        for alias in Verb::ALL.iter().filter_map(|v| v.argv0_alias()) {
            assert!(
                bundle.contains(&format!("\"{alias}\"")),
                "`{alias}` is a Verb argv0 alias and the macOS bundle does not symlink it: \
                 the name works from a checkout and is dead in a release"
            );
            assert!(
                front_door.contains(&format!("\"{alias}\" => AliasRoute::")),
                "`{alias}` is a Verb argv0 alias and `alias_route` does not route it: \
                 invoked by that name it falls through to the front door — for a \
                 bridge spawned by name that meant opening a WINDOW"
            );
        }
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

    /// `--help` rendering: the alignment column and continuation indent of the
    /// VERBS table.
    #[test]
    fn verb_help_blocks_align_to_one_column() {
        for verb in Verb::ALL {
            let block = verb_help_block(*verb);
            let mut lines = block.lines();
            let first = lines.next().expect("a verb renders at least one line");
            assert!(first.starts_with("    "), "{first:?} is not indented");
            assert_eq!(
                first.find(verb.blurb()[0]),
                Some(VERB_BLURB_COLUMN),
                "verb {} blurb does not start at the shared column",
                verb.name()
            );
            for cont in lines {
                assert!(
                    cont.starts_with(&" ".repeat(VERB_BLURB_COLUMN)),
                    "continuation {cont:?} is not aligned under the blurb column"
                );
            }
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
    /// spellings. `ATPKG_AGENTS` has no atpkg constant, so the pin is the shell
    /// hook body, which reads the `ATERM_AGENTS_DIR` handoff as one of its
    /// agents-gate markers and never sets it. `TERM_PROGRAM` is not a marker.
    #[test]
    fn reroute_env_names_match_atpkg() {
        assert_eq!(REROUTE_DIR_ENV, atpkg::reroute::REROUTE_DIR_ENV);
        assert_eq!(PASSTHROUGH_ENV, atpkg::reroute::PASSTHROUGH_ENV);
        assert_eq!(AGENTS_DIR_ENV, atpkg::reroute::AGENTS_DIR_ENV);
        assert_ne!(AGENTS_DIR_ENV, HOOK_AGENTS_ENV);
        let hooks = atpkg::hooks::hook_files(
            std::path::Path::new("/p/bin"),
            std::path::Path::new("/p/agents"),
            std::path::Path::new("/p/reroute"),
        );
        let zsh = hooks
            .iter()
            .find(|(name, _)| name.ends_with(".zsh"))
            .map(|(_, body)| body.as_str())
            .expect("the zsh hook");
        assert!(
            zsh.contains(&format!("export {HOOK_AGENTS_ENV}=")),
            "the hook exports {HOOK_AGENTS_ENV}: {zsh}"
        );
        // Inside aterm the hook's gated arm puts the reroute dir first and
        // agents/ second, reads `--no-reroute`'s marker by this crate's
        // spelling, and treats `$ATERM_REROUTE_DIR` as a marker (this lane sets
        // neither ATERM_CHILD nor ATERM_SESSION_ID).
        assert!(
            zsh.contains("__atpkg_reroute=\"/p/reroute\"")
                && zsh.contains(&format!(
                    "case \"${{{PASSTHROUGH_ENV}-}}\" in \"\"|0) case \"${{{AGENTS_DIR_ENV}-}}${{ATERM_CHILD-}}${{ATERM_SESSION_ID-}}${{{REROUTE_DIR_ENV}-}}\" in \"\") ;; *) __atpkg_front=\"$__atpkg_reroute${{__atpkg_front:+:}}$__atpkg_front\" ;;"
                )),
            "the gated arm leads reroute:agents unless {PASSTHROUGH_ENV} is engaged: {zsh}"
        );
        for (name, body) in &hooks {
            assert!(
                body.contains(PASSTHROUGH_ENV),
                "{name} reads the --no-reroute marker: {body}"
            );
            for write in [
                format!("export {PASSTHROUGH_ENV}="),
                format!("set -gx {PASSTHROUGH_ENV}"),
                format!("$env:{PASSTHROUGH_ENV} ="),
                format!("{REROUTE_DIR_ENV}="),
                format!("set -gx {REROUTE_DIR_ENV}"),
                format!("$env:{REROUTE_DIR_ENV} ="),
            ] {
                assert!(
                    !body.contains(&write),
                    "{name} never sets the marker or the seam's handle ({write}): {body}"
                );
            }
        }
        for (name, body) in &hooks {
            for marker in [AGENTS_DIR_ENV, "ATERM_CHILD", "ATERM_SESSION_ID"] {
                assert!(
                    body.contains(marker),
                    "{name} gates agents/ on {marker}: {body}"
                );
            }
            assert!(
                !body.contains("ATPKG_AGENTS_EVERYWHERE"),
                "{name}: no environment escape widens the gate past aterm: {body}"
            );
            for write in [
                format!("export {AGENTS_DIR_ENV}="),
                format!("set -gx {AGENTS_DIR_ENV}"),
                format!("$env:{AGENTS_DIR_ENV} ="),
            ] {
                assert!(
                    !body.contains(&write),
                    "{name} must never set the front door's handoff: {body}"
                );
            }
            assert!(
                !body.contains("TERM_PROGRAM"),
                "{name}: TERM_PROGRAM is user-settable, not a marker: {body}"
            );
        }
        // The shell integration (every dialect, and the macOS bundle's copies)
        // must not read the handoff either: it keys hook-sourcing on
        // `$ATPKG_AGENTS` being unset. Read as files, since those crates do not
        // depend on this one.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/aterm-cli sits under crates/");
        let workspace = root.parent().expect("crates/ sits under the workspace");
        let mut scripts = Vec::new();
        for dir in [
            root.join("aterm-shell-integration/src/scripts"),
            workspace.join("apps/aterm-mac/Sources/ATermMac/Resources/ShellIntegration"),
        ] {
            for entry in
                std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            {
                let path = entry.expect("a directory entry").path();
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("aterm_shell_integration."))
                {
                    scripts.push(path);
                }
            }
        }
        assert!(
            scripts.len() >= 7,
            "four dialects plus the three bundled copies: {scripts:?}"
        );
        for script in scripts {
            let body = std::fs::read_to_string(&script).expect("a shell integration script");
            assert!(
                body.contains(HOOK_AGENTS_ENV),
                "{} keys on {HOOK_AGENTS_ENV}",
                script.display()
            );
            assert!(
                !body.contains(AGENTS_DIR_ENV),
                "{} must not read the front door's handoff {AGENTS_DIR_ENV}",
                script.display()
            );
        }
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
    /// Skipped as root, which ignores mode bits.
    #[cfg(unix)]
    #[test]
    fn managed_agents_dir_says_no_when_the_prefix_refuses_the_mkdir() {
        use std::os::unix::fs::PermissionsExt as _;
        // SAFETY: getuid() takes no arguments and cannot fail.
        if unsafe { libc::getuid() } == 0 {
            eprintln!("running as root; a read-only prefix refuses nothing — skipping");
            return;
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

    /// The TTY lane's PATH at a real prompt: `/bin/zsh -l -i -c 'print -r -- $PATH'`
    /// with a private `ZDOTDIR` and empty `HOME`, given `session_path_env`'s
    /// order:
    ///
    /// 1. An empty `.zshrc`: where `path_helper` runs (macOS) the managed dirs
    ///    are demoted behind `/etc/paths`' list; both survive exactly once.
    ///    Elsewhere only survival is asserted.
    /// 2. A `.zshrc` sourcing atpkg's real `00-atpkg.zsh` hook: with none of the
    ///    agents gate's markers the hook removes `agents/` and the reroute dir
    ///    from PATH; with `ATERM_CHILD` set, or with this lane's own pair
    ///    (`$ATERM_AGENTS_DIR` + `$ATERM_REROUTE_DIR`), the reroute dir is first
    ///    and `agents/` second, each once. The managed bin/ is unconditional.
    /// 3. A degraded launch (agents handoff blank, reroute handle the only
    ///    marker): the reroute dir is still first, `agents/` out.
    #[cfg(unix)]
    #[test]
    fn a_login_zsh_demotes_the_seams_front_insert_and_the_rc_hook_puts_reroute_first_in_aterm() {
        let zsh = std::path::Path::new("/bin/zsh");
        if !zsh.exists() {
            eprintln!("/bin/zsh not installed; skipping the login-zsh PATH measurement");
            return;
        }
        let scratch = scratch_dir("login-zsh");
        let home = scratch.join("home");
        let zdotdir = scratch.join("zdotdir");
        let prefix = scratch.join("pkg");
        let reroute = prefix.join("reroute");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&zdotdir).unwrap();
        std::fs::create_dir_all(&reroute).unwrap();
        let agents = managed_agents_dir(None, reroute.to_str(), None).expect("derived and created");
        let reroute = reroute.to_str().unwrap().to_owned();
        let foreign = "/opt/homebrew/bin:/usr/bin:/bin";
        let (_, seam_path) = session_path_env(
            Some(&reroute),
            Some(&agents),
            Some("/opt/aterm/bin"),
            Some(foreign),
        )
        .expect("injects");
        assert!(seam_path.starts_with(&format!("{reroute}:{agents}:")));
        let prompt_path = |rc: &str, markers: &[(&str, &str)]| -> Vec<String> {
            std::fs::write(zdotdir.join(".zshrc"), rc).unwrap();
            let mut cmd = std::process::Command::new(zsh);
            cmd.args(["-l", "-i", "-c", "print -r -- $PATH"])
                .env_clear()
                .env("HOME", &home)
                .env("ZDOTDIR", &zdotdir)
                .env("TERM", "dumb")
                .env("PATH", &seam_path)
                .stdin(std::process::Stdio::null());
            for (key, value) in markers {
                cmd.env(key, value);
            }
            let out = cmd.output().expect("spawn /bin/zsh");
            assert!(
                out.status.success(),
                "zsh: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout)
                .trim_end()
                .split(':')
                .map(str::to_owned)
                .collect()
        };
        // 1. The pre-rc front-insert alone.
        let bare = prompt_path("", &[]);
        assert_eq!(bare.iter().filter(|e| **e == agents).count(), 1, "{bare:?}");
        assert_eq!(
            bare.iter().filter(|e| **e == reroute).count(),
            1,
            "{bare:?}"
        );
        let path_helper_runs = std::fs::read_to_string("/etc/zprofile")
            .is_ok_and(|body| body.contains("path_helper"))
            && std::path::Path::new("/usr/libexec/path_helper").exists();
        if path_helper_runs {
            assert_ne!(
                bare[0], agents,
                "path_helper rebuilds PATH ahead of the seam's order: {bare:?}"
            );
        }
        // 2. The rc-sourced atpkg hook, the block atpkg wires into ~/.zshrc.
        let hook = atpkg::hooks::hook_files(
            &prefix.join("bin"),
            std::path::Path::new(&agents),
            std::path::Path::new(&reroute),
        )
        .into_iter()
        .find(|(name, _)| name.ends_with(".zsh"))
        .map(|(_, body)| body)
        .expect("the zsh hook");
        let shell_d = home.join(".aterm/shell.d");
        std::fs::create_dir_all(&shell_d).unwrap();
        std::fs::write(shell_d.join("00-atpkg.zsh"), hook).unwrap();
        let rc = "[ -f \"$HOME/.aterm/shell.d/00-atpkg.zsh\" ] && . \"$HOME/.aterm/shell.d/00-atpkg.zsh\"\n";
        let outside = prompt_path(rc, &[]);
        assert!(
            !outside.contains(&agents),
            "outside aterm the hook demotes agents/: {outside:?}"
        );
        assert!(
            !outside.contains(&reroute),
            "outside aterm the hook takes the reroute dir out too: {outside:?}"
        );
        assert_eq!(
            outside.last().map(String::as_str),
            Some(prefix.join("bin").to_str().unwrap()),
            "the bin half is unconditional: {outside:?}"
        );
        // Inside: a window tab's marker, and this lane's own pair (the front door's
        // agents handoff and the reroute handle — never ATERM_CHILD, `session_main`).
        for markers in [
            &[("ATERM_CHILD", "1")][..],
            &[
                (AGENTS_DIR_ENV, agents.as_str()),
                (REROUTE_DIR_ENV, reroute.as_str()),
            ][..],
        ] {
            let inside = prompt_path(rc, markers);
            assert_eq!(
                inside[..2],
                [reroute.as_str(), agents.as_str()],
                "inside aterm ({markers:?}) the hook moves the reroute dir first and agents/ second: {inside:?}"
            );
            for dir in [&reroute, &agents] {
                assert_eq!(
                    inside.iter().filter(|e| *e == dir).count(),
                    1,
                    "{dir} once: {inside:?}"
                );
            }
        }
        // 3. Degraded launch: no agents handoff, so the reroute handle is the
        // only marker. The reroute dir is still first; agents/ is out.
        let degraded = prompt_path(
            rc,
            &[(AGENTS_DIR_ENV, ""), (REROUTE_DIR_ENV, reroute.as_str())],
        );
        assert_eq!(
            degraded[0], reroute,
            "no agents handoff: the reroute dir still leads: {degraded:?}"
        );
        assert_eq!(
            degraded.iter().filter(|e| **e == reroute).count(),
            1,
            "{degraded:?}"
        );
        assert!(
            !degraded.contains(&agents),
            "no agents marker, so agents/ stays out: {degraded:?}"
        );
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
        // One row per check: the label, the mark, the fact.
        assert!(
            r.lines()
                .any(|l| l == "shell:      ok    /bin/sh (executable)"),
            "{r}"
        );
        assert!(r.lines().any(|l| l.starts_with("version: ")), "{r}");
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
        assert!(
            r.lines()
                .any(|l| l == "shell:      FAIL  /no/such/shell does not exist"),
            "{r}"
        );
        assert!(
            r.contains(
                "health: FAIL — aterm cannot start this shell; set $SHELL to an installed shell"
            ),
            "{r}"
        );
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
        assert!(r.contains("/etc/hosts is not executable"), "{r}");

        // An empty $SHELL is a real failure: the spawn would exec "".
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
        assert!(r.contains("$SHELL is empty"), "{r}");
    }

    /// `$SHELL` unset is not a failure: a plain `aterm` then starts `/bin/sh`
    /// (aterm-pty's resolver), so that is the shell doctor checks and names.
    #[cfg(unix)]
    #[test]
    fn doctor_checks_bin_sh_when_shell_is_unset() {
        assert_eq!(unix_shell(Some("/bin/zsh")), ("/bin/zsh", None));
        let (shell, origin) = unix_shell(None);
        assert_eq!(shell, "/bin/sh");
        let (r, code) = doctor_checks_with_origin(
            shell,
            driver::shell_check(shell),
            origin,
            &unmeasured(),
            &no_debugger(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &DevrootFacts::absent(),
        );
        assert_eq!(code, 0, "{r}");
        assert!(
            r.lines().any(|l| l
                == "shell:      ok    /bin/sh (executable) — $SHELL is unset, so aterm starts it"),
            "{r}"
        );
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
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &DevrootFacts::absent(),
        );
        assert_eq!(plain, unlabelled, "no label, no change");

        let (labelled, code) = doctor_checks_with_origin(
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            ShellCheck::Runs,
            Some("a new window's default shell, the platform default (pwsh on PATH)"),
            &unmeasured(),
            &no_debugger(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &DevrootFacts::absent(),
        );
        assert_eq!(code, 0);
        assert_eq!(mark_of(&labelled, "shell:"), "ok", "{labelled}");
        assert!(
            shell_row(&labelled).ends_with(
                r" C:\Program Files\PowerShell\7\pwsh.exe (executable) — a new window's default shell, the platform default (pwsh on PATH)"
            ),
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
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &DevrootFacts::absent(),
        );
        assert_eq!(code, 1);
        assert_eq!(mark_of(&failed, "shell:"), "FAIL", "{failed}");
        assert!(
            shell_row(&failed).ends_with(
                " nosuch does not exist — a new window's default shell, from aterm.toml shell = \"nosuch\""
            ),
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
        let (doctor, _) = doctor_report();
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
        assert!(r.contains("aterm help config"), "{r}");
        assert!(
            !r.contains("No environment variable changes what aterm does"),
            "{r}"
        );
    }

    /// Windows `explain-config` names the starter-file command and the `shell`
    /// and `font_px` keys.
    #[cfg(windows)]
    #[test]
    fn windows_explain_config_names_the_starter_shell_and_font_px() {
        let r = explain_config_report();
        for needle in [
            "\n  `aterm --window --write-config` writes a documented starter.\n",
            "\n  shell ",
            "\n  font_px ",
            "with --shell uses that instead",
            "with --font-px uses that instead",
            "pwsh, then powershell, then %COMSPEC%, then cmd.exe",
            // The console image's spelling: `aterm-gui` is the GUI-subsystem
            // image, which a pwsh prompt does not wait for.
            "`aterm --window --write-config`",
            "doctor` names the program this machine resolves it to",
        ] {
            assert!(r.contains(needle), "explain-config missing {needle:?}\n{r}");
        }
        assert!(
            !r.contains("~/.config/aterm"),
            "no Unix literal for the Windows config path:\n{r}"
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

        // 1. granted, on a build whose identity is stable → the one `ok` arm.
        let (r, code) = ok(&measured(FdaState::Granted, DrClass::Identity, "aterm"));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "privacy:"), "ok", "{r}");
        assert!(
            r.lines()
                .any(|l| l == "privacy:    ok    full disk access for the responsible app (aterm)"),
            "{r}"
        );
        // The grant is described by what it IS, never as the end of every dialog
        // (which services it covers is unmeasured — design §7 S4).
        assert!(
            !r.contains("no more prompts") && !r.contains("removes all"),
            "{r}"
        );

        // 2. Denied → a note naming the responsible app, not aterm.
        let (r, code) = ok(&measured(FdaState::Denied, DrClass::Identity, "iTerm2"));
        assert_eq!(code, 0, "a missing grant is not a doctor failure\n{r}");
        assert!(r.contains("health: OK"), "{r}");
        assert_eq!(mark_of(&r, "privacy:"), "note", "{r}");
        assert!(
            r.contains("no full disk access for the responsible app (iTerm2)")
                && r.contains(
                    "can be interrupted by macOS consent dialogs (aterm help permissions)"
                ),
            "{r}"
        );

        // 3. a dev build: the grant may be real today and dead after the next build.
        let (r, code) = ok(&measured(FdaState::Granted, DrClass::Cdhash, "aterm (dev)"));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "privacy:"), "note", "{r}");
        assert!(
            r.contains("dev build: identity changes on every build, so grants do not persist"),
            "{r}"
        );

        // 4. Out of the bundle: not measured, and it says why.
        let (r, code) = ok(&unmeasured());
        assert_eq!(code, 0, "{r}");
        assert!(
            r.lines()
                .any(|l| l == "privacy:    note  not measured (running outside the app bundle)"),
            "{r}"
        );

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
        assert!(r.contains("no windowed launch recorded yet"), "{r}");

        let killed = Row {
            at: 1_790_500_000,
            pid: 2,
            started: 1_790_499_990,
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
        };
        let path = std::path::PathBuf::from("/logs/recovery-ledger.log");
        let (r, code) = check(Some((vec![killed], path)));
        assert_eq!(code, 0, "a past kill never fails doctor: {r}");
        assert_eq!(mark_of(&r, "recovery:"), "note", "{r}");
        assert!(r.contains("1 killed"), "{r}");
        assert!(r.contains("build 1790305290 (0.93.0)"), "{r}");
        assert!(r.contains("up 2 h 0 min or more"), "{r}");
        assert!(r.contains("stalled at its end"), "{r}");
        // One row, several lines: the label once, the later lines under the fact
        // column, as every multi-line row prints.
        assert_eq!(r.matches("recovery:").count(), 1, "{r}");

        let (r, code) = check(None);
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "recovery:"), "note", "{r}");
        assert!(r.contains("not measured"), "{r}");
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

        let (r, code) = check(census(0, Vec::new()));
        assert_eq!(code, 0, "{r}");
        assert!(
            r.lines()
                .any(|l| l
                    == "instances:  ok    no aterm window or headless instance is running as you"),
            "{r}"
        );

        let (r, _) = check(census(2, Vec::new()));
        assert!(
            r.contains(
                "instances:  ok    2 aterm windows and headless instances, each published in /run/aterm"
            ),
            "{r}"
        );

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
            r.contains(
                "instances:  note  2 of 3 aterm instances are reachable by no control socket in /run/aterm"
            ),
            "{r}"
        );
        assert!(
            r.lines()
                .any(|l| l == "                  pid 67548 (up 11d 22h): ./aterm-before --headless"),
            "{r}"
        );
        assert!(
            r.lines()
                .any(|l| l.starts_with("                  pid 68000")
                    && l.ends_with("started with its control socket off")),
            "{r}"
        );

        let (r, code) = check(Err("the process table could not be read".to_string()));
        assert_eq!(code, 0, "{r}");
        assert!(
            r.contains("instances:  note  not measured (the process table could not be read)"),
            "a census that was not taken never reads as none\n{r}"
        );
    }

    /// `DrClass::Unknown` (codesign unreadable) asserts nothing: the dev-build
    /// sentence needs a positively classified requirement.
    #[test]
    fn an_unclassified_requirement_never_claims_a_dev_build() {
        let (r, _) = doctor_checks(
            "/bin/sh",
            ShellCheck::Runs,
            &measured(FdaState::Denied, DrClass::Unknown, "Terminal"),
            &no_debugger(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
        );
        assert!(!r.contains("dev build"), "{r}");
    }

    /// THE FENCE, end to end: this test binary runs from `target/debug/deps`,
    /// the shape that makes `tccd` walk a million-entry directory, so
    /// `doctor_report` must report `not measured` with no syscall behind it.
    #[cfg(target_os = "macos")]
    #[test]
    fn doctor_run_from_a_test_binary_measures_nothing() {
        let (r, _) = doctor_report();
        assert!(
            r.contains("privacy:    note  not measured ("),
            "a test binary must never report a measured consent state\n{r}"
        );
        assert!(
            !r.contains("full disk access for the responsible app"),
            "the fence let a verdict through\n{r}"
        );
        // The debugger row sits behind the same fence: no `DevToolsSecurity`
        // spawn, no directory query, no `csops` — and so no mode.
        assert!(
            r.contains("debugger:   note  not measured ("),
            "a test binary must never report a measured debugger rule\n{r}"
        );
        // And the Spotlight probe: a test binary plants nothing in a real home.
        assert!(
            r.contains("spotlight:  note  not measured (a test binary plants nothing"),
            "a test binary must never probe the real ~/Developer\n{r}"
        );
        assert!(r.contains("devroot:"), "the survey only reads: {r}");
    }

    /// The `devroot:` and `spotlight:` rows say what atpkg surveyed and MEASURED, in
    /// atpkg's words: a checkout outside `~/Developer` and an indexed root are each a
    /// `note` naming the one thing to do, a skipped root is `ok`, an unmeasured one
    /// says so — and none of them moves the exit code.
    #[test]
    fn the_devroot_rows_say_what_atpkg_measured_and_never_fail() {
        use atpkg::devroot::{Checkout, Spotlight, Survey};
        use atpkg::noindex::{Unmeasured, Verdict};
        let home = std::path::PathBuf::from("/Users//u");
        let outside = Survey {
            home: home.clone(),
            root: atpkg::devroot::root(&home),
            root_is: atpkg::devroot::RootIs::Folder,
            outside: vec![Checkout {
                path: home.join("proj"),
                common: home.join("proj/.git"),
                linked: false,
            }],
            inside: Vec::new(),
        };
        let report = |survey: Option<Survey>, spotlight: Spotlight| {
            doctor_checks_with_origin(
                "/bin/sh",
                ShellCheck::Runs,
                None,
                &unmeasured(),
                &no_debugger(),
                &no_census(),
                &no_recovery(),
                &no_exceptions(),
                &DevrootFacts { survey, spotlight },
            )
        };
        let (r, code) = report(Some(outside.clone()), Spotlight::Measured(Verdict::Indexed));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "devroot:"), "note", "{r}");
        assert!(
            r.contains("1 git checkout outside ~/Developer (~/proj)"),
            "{r}"
        );
        assert!(r.contains("`aterm pkg devroot move --all`"), "{r}");
        assert_eq!(mark_of(&r, "spotlight:"), "note", "{r}");
        assert!(
            r.contains("Spotlight indexes ~/Developer (measured: the probe was found)"),
            "{r}"
        );
        assert!(
            r.contains(
                "add ~/Developer to System Settings \u{25b8} Spotlight \u{25b8} Search Privacy"
            ),
            "{r}"
        );
        let inside = Survey {
            inside: outside.outside.clone(),
            outside: Vec::new(),
            ..outside
        };
        let (r, code) = report(Some(inside), Spotlight::Measured(Verdict::Excluded));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "devroot:"), "ok", "{r}");
        assert_eq!(mark_of(&r, "spotlight:"), "ok", "{r}");
        let unknown = Verdict::Unknown(Unmeasured::ControlNeverIndexed { waited_secs: 20 });
        let (r, _) = report(None, Spotlight::Measured(unknown));
        assert!(!r.contains("devroot:"), "no survey, no row: {r}");
        assert_eq!(mark_of(&r, "spotlight:"), "note", "{r}");
        assert!(
            !r.contains("indexes ~/Developer"),
            "unknown is never a verdict: {r}"
        );
        let (r, _) = report(None, Spotlight::Measured(Verdict::NotApplicable));
        assert!(!r.contains("spotlight:"), "no Spotlight, no row: {r}");
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
        // Skips cleanly on a host with no resolvable fonts.
        let Some(family) = aterm_render::list_fonts()
            .into_iter()
            .find(|f| aterm_render::face_info(f).is_some())
        else {
            eprintln!("SKIP: no resolvable system font");
            return;
        };
        let (r, code) = show_face_report(Some(&family));
        assert_eq!(code, 0, "{r}");
        for key in [
            "family=",
            "path=",
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
            return;
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
    fn list_themes_includes_default_and_named() {
        let r = list_themes_report();
        for name in ["Default", "Dracula", "Nord", "Solarized Dark"] {
            assert!(r.contains(name), "list-themes missing {name:?}\n{r}");
        }
        assert!(r.contains("Set one in aterm.toml: theme = "), "{r}");
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
        // decided later by the co-located atpkg — this predicate only screens aterm's surface.
        for tool in ["ay", "ty", "trust", "trust-mc", "clean", "ny"] {
            assert!(
                is_tool_candidate(Some(tool)),
                "{tool} should be a candidate"
            );
        }
    }

    #[test]
    fn no_args_runs_with_no_override() {
        // A Finder/.app launch: no flags → launch, the default decides the mode.
        assert_eq!(
            decide(&[]),
            CliAction::Run {
                containment: None,
                quiet: false,
                verbose: false,
            }
        );
    }

    /// `--verbose` is the session's summary switch — a flag, no environment.
    #[test]
    fn verbose_flag_composes_with_the_rest() {
        assert_eq!(
            decide(&["--verbose", "--sandbox"]),
            CliAction::Run {
                containment: Some("containment".into()),
                quiet: false,
                verbose: true,
            }
        );
    }

    #[test]
    fn quiet_flag_short_and_long_and_composes() {
        for a in [["-q"], ["--quiet"]] {
            assert_eq!(
                decide(&a),
                CliAction::Run {
                    containment: None,
                    quiet: true,
                    verbose: false,
                }
            );
        }
        // Order-independent alongside a containment flag.
        assert_eq!(
            decide(&["--sandbox", "-q"]),
            CliAction::Run {
                containment: Some("containment".to_string()),
                quiet: true,
                verbose: false,
            }
        );
        assert_eq!(
            decide(&["--quiet", "--containment", "user"]),
            CliAction::Run {
                containment: Some("user".to_string()),
                quiet: true,
                verbose: false,
            }
        );
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

    #[test]
    fn containment_space_form() {
        assert_eq!(
            decide(&["--containment", "master"]),
            CliAction::Run {
                containment: Some("master".into()),
                quiet: false,
                verbose: false,
            }
        );
    }

    #[test]
    fn containment_eq_form() {
        // `=` syntax must be accepted and carried through identically to the space form.
        assert_eq!(
            decide(&["--containment=user"]),
            CliAction::Run {
                containment: Some("user".into()),
                quiet: false,
                verbose: false,
            }
        );
    }

    #[test]
    fn containment_eq_empty_value_is_carried_through_not_ignored() {
        // `--containment=` → empty string, which fails closed in the init funnel
        // exactly like an invalid mode; it must not be silently dropped.
        assert_eq!(
            decide(&["--containment="]),
            CliAction::Run {
                containment: Some(String::new()),
                quiet: false,
                verbose: false,
            }
        );
    }

    #[test]
    fn containment_missing_value_is_usage_error_not_swallow() {
        // Trailing `--containment` with no value: a usage error (exit 2), never a
        // silent run nor a panic.
        match decide(&["--containment"]) {
            CliAction::Usage(m) => assert!(m.contains("--containment requires a mode"), "{m}"),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn invalid_mode_is_carried_through_for_fail_closed_not_validated_here() {
        // The parser does not validate the mode; it hands the garbage to the single
        // init funnel, which fails closed. So decide_args still returns Run(Some(..)).
        assert_eq!(
            decide(&["--containment", "xyz"]),
            CliAction::Run {
                containment: Some("xyz".into()),
                quiet: false,
                verbose: false,
            }
        );
    }

    #[test]
    fn sandbox_and_no_sandbox_map_to_modes() {
        assert_eq!(
            decide(&["--sandbox"]),
            CliAction::Run {
                containment: Some("containment".into()),
                quiet: false,
                verbose: false,
            }
        );
        assert_eq!(
            decide(&["--no-sandbox"]),
            CliAction::Run {
                containment: Some("user".into()),
                quiet: false,
                verbose: false,
            }
        );
    }

    #[test]
    fn conflicting_flags_last_one_wins() {
        // Documented precedence among flags: last on the line wins.
        assert_eq!(
            decide(&["--sandbox", "--containment", "user"]),
            CliAction::Run {
                containment: Some("user".into()),
                quiet: false,
                verbose: false,
            }
        );
        assert_eq!(
            decide(&["--containment", "user", "--sandbox"]),
            CliAction::Run {
                containment: Some("containment".into()),
                quiet: false,
                verbose: false,
            }
        );
        assert_eq!(
            decide(&["--no-sandbox", "--sandbox"]),
            CliAction::Run {
                containment: Some("containment".into()),
                quiet: false,
                verbose: false,
            }
        );
    }

    #[test]
    fn unknown_flag_is_usage_error() {
        match decide(&["--bogus"]) {
            CliAction::Usage(m) => assert!(m.contains("unknown option --bogus"), "{m}"),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn double_dash_ends_options_and_bare_form_runs() {
        // A lone `--` ends option parsing; aterm takes no operands, so a bare `--`
        // is a clean no-op run (not an "unknown option").
        assert_eq!(
            decide(&["--"]),
            CliAction::Run {
                containment: None,
                quiet: false,
                verbose: false,
            }
        );
        // Flags before `--` still apply.
        assert_eq!(
            decide(&["--sandbox", "--"]),
            CliAction::Run {
                containment: Some("containment".into()),
                quiet: false,
                verbose: false,
            }
        );
    }

    #[test]
    fn operand_after_double_dash_is_rejected() {
        // An operand after `--` is still a usage error, and a `-`-prefixed token
        // after `--` is that same operand, not a flag.
        match decide(&["--", "extra"]) {
            CliAction::Usage(m) => assert!(m.contains("unknown command extra"), "{m}"),
            other => panic!("expected Usage, got {other:?}"),
        }
        match decide(&["--", "--help"]) {
            CliAction::Usage(m) => assert!(m.contains("unknown option --help"), "{m}"),
            other => panic!("expected Usage (post-`--` is an operand), got {other:?}"),
        }
    }

    /// Per-mode limits: daily-driver modes inherit, confinement modes cap (the
    /// rule shared with `aterm-gui`'s spawn and the seam's `limits` contract).
    /// The numbers `aterm help aterm`, the man page and the CHANGELOG quote are
    /// pinned to what the sandbox crate installs, and `aterm help aterm` must
    /// quote them.
    #[test]
    fn the_session_inherits_rlimits_in_user_and_master_and_caps_in_safety_and_containment() {
        use aterm_containment::ContainmentMode as Cm;
        use aterm_sandbox::Limits;
        for mode in [Cm::User, Cm::Master] {
            assert_eq!(
                session_limits(mode),
                Limits::inherit(),
                "{mode}: must inherit"
            );
        }
        for mode in [Cm::Safety, Cm::Containment] {
            assert_eq!(
                session_limits(mode),
                Limits::shell_default(),
                "{mode}: must cap"
            );
        }
        // Numbers the docs quote: open files 8192 (POSIX), 512 active processes
        // + UI restrictions (Windows), 16 GiB address space / job memory off
        // macOS.
        let hardened = Limits::shell_default();
        assert_eq!(hardened.open_files, Some(8192));
        assert_eq!(hardened.active_processes, Some(512));
        assert!(hardened.restrict_ui);
        if cfg!(target_os = "macos") {
            assert_eq!(
                hardened.address_space, None,
                "a 16 GiB RLIMIT_AS is below what every macOS process already maps"
            );
        } else {
            assert_eq!(hardened.address_space, Some(16 * 1024 * 1024 * 1024));
        }
        let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let (page, code) = crate::manual::render(Some("aterm"), None);
        assert_eq!(code, 0, "`aterm help aterm` must render");
        let page = flat(&page);
        for needle in [
            "open files at a soft 8192",
            "address space at a soft 16 GiB on Linux",
            "hard limits untouched",
            "512 active processes",
            "UI restrictions",
        ] {
            assert!(
                page.contains(needle),
                "`aterm help aterm` must quote {needle:?}:\n{page}"
            );
        }
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

    /// End to end through the real seam: `ulimit -n` in the spawned `/bin/sh`
    /// of a User-mode session equals this process's soft `RLIMIT_NOFILE`, and a
    /// Safety-mode child reads the hardened cap (clamped to the inherited hard
    /// ceiling). Discriminating whenever the parent's soft limit differs from
    /// the cap.
    #[cfg(unix)]
    #[test]
    fn the_session_child_sees_the_posture_of_its_mode() {
        use aterm_containment::ContainmentMode as Cm;

        // SAFETY: a trusted test entry point, before any untrusted input flows.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let exec: Vec<String> = vec!["/bin/sh".into(), "-c".into(), "ulimit -n".into()];

        let child_ulimit = |mode: Cm| -> String {
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
                .find(|l| !l.is_empty())
                .unwrap_or_default()
                .to_string()
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
    }
}
