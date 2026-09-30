// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm` — a transparent, introspecting terminal (U1).
//!
//! It spawns your `$SHELL` in a PTY and passes I/O through **unchanged**, so it
//! looks and behaves exactly like your shell. It does NOT model the screen: the
//! host terminal draws the bytes and NOTHING in this process reads them back —
//! no VT parse, no grid, no scrollback on the passthrough path. (A development
//! seam once armed an in-process model for `apply_policy_engine` on this engine,
//! docs/HARDCORE_BACKLOG.md §4 P0; nothing could read that model, so the sub-item
//! was closed and the seam deleted on 2026-09-25.)
//! This passthrough binary serves NO control socket; the
//! out-of-process, introspectable surface an AI reads and drives (via
//! `aterm ctl`) is the WINDOW mode of the one binary — `aterm --window`,
//! or `--headless` for an engine + socket with no window.
//!
//! A thin platform driver (raw mode + passthrough loop: `poll(2)`/termios in
//! `driver_unix`, console events in `driver_windows`) over the PROTECTED spawn
//! seam. The shell is launched via the [`aterm_pty`] spawn seam — cap-gated,
//! fail-closed fork/exec, resource-bounded in the confinement modes only
//! (safety/containment; see `session_limits`), and OS-sandbox-wrapped when the
//! containment mode demands it (P0) — exactly like `aterm-gui`, NOT raw
//! `forkpty`/`execvp`. Daily-driver essentials are handled: window resize is
//! forwarded (SIGWINCH / console resize event -> PTY), the loop is
//! signal-robust (EINTR), and
//! aterm exits with the shell's own status.
//!
//! Containment mode is launcher-owned (`--containment`, ATERM_DESIGN §5):
//! the default is `User` — no OS sandbox, so the daily-driver shell keeps full
//! network/credential access and behaves as before, confined by the cap gate and
//! INHERITING the launching shell's `rlimit`s unchanged (the rule `aterm-gui`
//! applies at its own spawn — see `session_limits`; on Windows, where the same
//! `Limits` go onto the child's Job Object, that means no job caps); Safety /
//! Containment keep the hardened caps. `--sandbox` (`--containment containment`) opts
//! into the macOS Seatbelt sandbox (no network, writes only to the temp roots, no
//! credential/private-data access) and is REFUSED where no OS sandbox exists; a
//! malformed value fails CLOSED to Containment.

// The macOS consent tier (docs/DESIGN-macos-tcc-prompts-2026-08-30.md §3.3): the
// ONE module that owns the prompt-free Full Disk Access probe, the in-bundle
// fence that keeps a headless-constructible path away from `tccd`, and every
// protected-folder path literal. `doctor` reads its verdict and threads it into
// the pure report; nothing here types a protected path of its own (grep_guard
// B13).
use aterm_containment::consent::{DrClass, FdaState, ProbeGate, ProbeLabel};

// The platform console driver behind a shared four-function surface
// (host_winsize / stdout_is_tty / shell_is_executable / run): termios + poll
// on POSIX (moved verbatim from this file), ConPTY console events on Windows.
#[cfg(unix)]
#[path = "driver_unix.rs"]
mod driver;
#[cfg(windows)]
#[path = "driver_windows.rs"]
mod driver;

// The AI-facing toolchain manual behind `aterm help [topic]` — the one place an AI learns
// what aterm is, how to drive it, and every tool in the toolchain.
mod manual;

// The coding-agent primer installer behind `aterm agents` lives in the
// `aterm-primer` crate — it manages the self-gating aterm primer block in
// agents' global context files (~/.claude/CLAUDE.md, ~/.codex/AGENTS.md, ...),
// the one channel that reliably reaches an agent's context in every project. The
// delivery half of the manual: `manual` is what an agent reads once it knows to
// run `aterm help`; the primer is how it learns to. A crate rather than a module
// here because the GUI runs the same installer itself on every session it opens.

// The wt-shaped WINDOWING grammar (`new-tab` / `new-window` / `split-pane`) and
// the `windowing_behavior` routing policy behind them. Pure parse + pure
// decision, kept out of the front door's `main` so both are unit-testable
// without a socket, a window, or a config file.
mod windowing;

pub use windowing::{
    LaunchEnv, LaunchIntent, SplitOrientation, WindowRequest, WindowRoute, WindowingBehavior,
    parse_window_request, plain_launch_dir_operand, plain_launch_is_policy_eligible, route_launch,
    window_verb_usage,
};

/// Polished `--help` text: synopsis, description, OPTIONS, ENVIRONMENT, EXAMPLES.
/// Mirrors `aterm-gui`'s `parse_cli()` help in tone and layout, scoped to what the
/// daily-driver CLI actually does (transparent passthrough of `$SHELL`).
const HELP_TITLE: &str = "aterm — a transparent, introspecting terminal\n";
/// What a plain `aterm` session runs, per platform — the ONE phrase `--help`
/// ([`HELP_SESSION_LINE`]) and the `aterm help aterm` page both splice in, so
/// the two cannot name two shells (they did: the page kept "your $SHELL" on
/// Windows after `--help` stopped saying it). Unix: `$SHELL`. Windows: the
/// passthrough reads no `$SHELL` (a POSIX path in MSYS shells) and no
/// aterm.toml — it spawns `aterm-pty`'s platform default (`shell_override`
/// `None` in [`session_main`]), so "your $SHELL" there named a shell it never
/// runs (audit 2026-09-22). A macro, not a `const`: both sites are
/// `concat!`-built constants, and `concat!` takes only literals.
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
    "                              tool (trust/clean/ty/ay/ny/nn). Inside a session it prints\n",
    "                              the agent operating brief. START HERE.\n",
    "    aterm <verb> [args]       A platform verb (see VERBS) or a toolchain tool (see TOOLCHAIN).\n",
    "    aterm <SUBCOMMAND>        Print diagnostics and exit (see SUBCOMMANDS).\n",
    "\n",
    "OPTIONS:\n",
    "        --containment <MODE>  master, user (the default), safety or containment;\n",
    "        --containment=<MODE>  the last containment flag given wins. An invalid\n",
    "                              value fails closed to containment.\n",
    "        --sandbox             Shorthand for --containment containment (the macOS\n",
    "                              sandbox: no network; writes only to temp dirs and\n",
    "                              shell history; no access to credential stores or\n",
    "                              private data; refused where no OS sandbox exists).\n",
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
    "    show-config               Print aterm's effective runtime configuration.\n",
    "    explain-config            Explain how aterm resolves its configuration.\n",
    "    doctor                    Pre-flight health check; exit non-zero on a problem.\n",
    "    list-fonts                List available font families.\n",
    "    show-face <family>        Show metrics for a font family.\n",
    "    list-themes               List the built-in colour schemes.\n",
    "    list-kitty-commands       List the words the cursor cat obeys, by language.\n",
    "\n",
);

/// How a Windows install is updated while no Windows updater exists — the ONE
/// spelling `aterm update status|check`, `aterm help update` and the manual's
/// header share, so the three cannot name three lanes. There is no `aterm.app`
/// on Windows and nothing checks, stages or applies a build there (design §7,
/// W8 open); until that lands the lane is the build scripts, or the MSIX
/// (`apps\aterm-win\msix\build-msix.ps1`). Measured 2026-09-22 on 0.90.0: the
/// verb answered with the macOS sentence and the header claimed a signed
/// appcast, so a reader waited for an update that could never arrive.
pub const WINDOWS_UPDATE_LANE: &str =
    "git pull, then apps\\aterm-win\\build.ps1 and apps\\aterm-win\\install.ps1";

/// THE front-door verb roster — the ONE place a verb exists.
///
/// `aterm ship` is why this is a roster of variants and not four hand-written
/// match arms. It shipped wired only into the WINDOW library's parser, and that
/// single mistake opened THREE holes at once, because a verb's NAME, its HELP and
/// its DISPATCH lived in three places nothing forced to agree:
///   * it was advertised nowhere, so `--help` denied it existed;
///   * it was routed nowhere at the front door, so at a terminal — where stdin is
///     a TTY and the mode fork picks the SESSION — it died as an unknown option,
///     working only when stdin happened to be a pipe;
///   * and because [`is_tool_candidate`] derives its shadowing rule from this
///     list, a co-distributed toolchain program named `ship` could have hijacked
///     the verb outright.
///
/// They can no longer disagree:
///   * name and help text live HERE, and `--help` renders this list, so an
///     unadvertised verb is unrepresentable rather than merely tested for;
///   * dispatch in `crates/aterm/src/main.rs` matches this enum EXHAUSTIVELY, so
///     a new variant fails the BUILD until it is routed;
///   * [`is_tool_candidate`] consults this list, so every verb is shielded from
///     toolchain shadowing the moment it is added;
///   * and `crates/aterm/tests/front_door_verbs.rs` drives every variant through
///     the real binary under a real pty, so "routed" is proved to mean "reachable
///     where an operator actually types it", not merely "reachable when piped".
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
    /// `aterm harness` — the Claude Code harness's four read views and the live
    /// agent upgrade (docs/DESIGN-aterm-wrapper-2026-09-17.md §0.4). Its hook
    /// bridge is retired by decision "B" (2026-09-22), and its supervising verbs
    /// were deleted with the second harness stack (2026-09-23): `aterm drive`
    /// supervises; `upgrade` only restarts an agent onto a newer build.
    Harness,
    /// `aterm keeper` — the PTY keeper (`start | stop | status | serve`): the
    /// per-login holder of a custody copy of every terminal's master
    /// (docs/DESIGN-pty-keeper-2026-09-26.md). Opt-in (P3): a window registers
    /// with one only under `[keeper] enabled = true`.
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
            // familiar grammar is that the words are the SAME words.
            Verb::NewTab => "new-tab",
            Verb::NewWindow => "new-window",
            Verb::SplitPane => "split-pane",
        }
    }

    /// The argv0 compat alias this verb also answers to, for the verbs that have
    /// one. The bundle ships these as symlinks onto the one binary, so every
    /// pre-one-binary script keeps working. `ship` has none (the release tool is
    /// a separate executable it execs) and neither does `agents` (it never was a
    /// sibling binary). Test-only: the roster the bundler and the front door's
    /// `alias_route` are checked against; neither reads it.
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
            // `fabric` never was a sibling binary: it is a report over what
            // `aterm link` and `aterm ctl` already reach, shipped only as a
            // front-door word (and as `aterm-link fabric`, the same code).
            Verb::Fabric => None,
            // The windowing verbs never were sibling binaries, and never should
            // be: `new-tab` on PATH would shadow nothing of aterm's but would be
            // a wildly generic name to install into a user's `$PATH`.
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

    /// Whether this verb is one of the WINDOWING verbs — the three
    /// [`parse_window_request`] parses and [`route_launch`] routes.
    ///
    /// The roster owns the classification so the front door's alias dispatch and
    /// its exhaustive verb match cannot disagree about which words open a
    /// terminal. That matters concretely on Windows: the shipped install is
    /// several IDENTICAL copies of this one binary under the sibling names, the
    /// Start-Menu shortcut targets `aterm-gui.exe`, and the taskbar jump list is
    /// committed by whichever copy is running — so a windowing verb genuinely
    /// arrives under an argv0 ALIAS, where it must be routed rather than handed
    /// to the window's own flag parser as an unknown option.
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
            // The synopsis column is 26 wide (VERB_BLURB_COLUMN - 4) and the
            // rendering test pins the blurb to exactly column 30, so these read
            // `[-d dir]` rather than the `[-d <dir>]` the usage lines use: the
            // angle brackets would push `new-window`/`split-pane` to 27 and
            // break the one-column alignment of the whole VERBS table.
            Verb::NewTab => "aterm new-tab [-d dir]",
            Verb::NewWindow => "aterm new-window [-d dir]",
            Verb::SplitPane => "aterm split-pane [-d dir]",
        }
    }

    /// The `--help` description, one entry per rendered line.
    pub const fn blurb(self) -> &'static [&'static str] {
        match self {
            Verb::Ctl => &[
                "Introspect & drive any terminal: read the screen, send keys,",
                "run a turn, subscribe to events, capture a real frame.",
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
                "Claude Code's spend and limits, the disk, the approval ledger",
                "(usage | limits | disk | ledger); `upgrade` moves a live Claude",
                "Code onto a newer build. `aterm drive` supervises.",
            ],
            Verb::Keeper => &[
                "The PTY keeper: hold every terminal's master so a crashed",
                "window's shells live on (start | stop | status | serve). Opt-in.",
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

    /// Resolve an `argv[1]` operand to a verb. The ONE recognizer: the front door
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

/// `--help`, assembled. The VERBS section is RENDERED from [`Verb::ALL`] rather
/// than written out, which is what makes "advertised" and "exists" the same fact.
fn help_text() -> String {
    // Title, then WHO MAKES IT (`by Andrew Yates · ALab · alab.systems`) — the one
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
    "    aterm                              Start an interactive shell (mode: user).\n",
    "    aterm --sandbox                    No network; writes only to temp dirs and\n",
    "                                       shell history; no access to credential\n",
    "                                       stores or private data (macOS; refused\n",
    "                                       where there is no OS sandbox).\n",
    "    aterm --containment safety         Capped limits, no OS sandbox.\n",
);

/// Diagnostic subcommands (CLI-DIAG): `aterm <name>` prints introspection about
/// aterm's own configuration/environment and exits 0 WITHOUT spawning a shell.
///
/// Implemented: config introspection (`show-config` / `explain-config`), a
/// `doctor` pre-flight health check, and the read-only
/// enumerators `list-fonts` / `show-face` / `list-themes` (backed by aterm-render +
/// aterm-types). `list-keybinds` is deliberately NOT here: keybindings are an
/// aterm-gui concept; the transparent passthrough binary has no keymap, so it would
/// belong in aterm-gui, not a false affordance here. `list-kitty-commands` IS here
/// although the cursor cat lives in the window: what it prints is the VOCABULARY,
/// pure data compiled into the one binary (aterm-lexicon's `tricks` table, the same
/// one the window's typed-line listener compiles), so the listing is true with no
/// window running — the `list-themes` case, not the `list-keybinds` one.
///
/// This is the SINGLE source of truth: [`diag_report`] must handle every entry
/// AND [`help_text`] must advertise every entry — both enforced by the
/// `diag_commands_advertised_and_dispatchable` gate, so a subcommand can never
/// ship undocumented or unimplemented.
pub const DIAG_COMMANDS: &[(&str, &str)] = &[
    (
        "show-config",
        "Print aterm's effective runtime configuration.",
    ),
    (
        "explain-config",
        "Explain how aterm resolves its configuration.",
    ),
    (
        "doctor",
        "Run an aggregate pre-flight health check; exit non-zero on any problem.",
    ),
    ("list-fonts", "List available font families (one per line)."),
    (
        "show-face",
        "Show metrics for a font family (usage: aterm show-face <family>).",
    ),
    (
        "list-themes",
        "List the built-in colour schemes and their descriptions.",
    ),
    (
        "list-kitty-commands",
        "List the words the cursor cat obeys when typed (one row per trick and language).",
    ),
];

/// Build the `(report, exit_code)` for diagnostic subcommand `cmd` (with an
/// optional positional `arg`, e.g. the family for `show-face`), or `None` if `cmd`
/// is not a registry command. Read-only: consults env / the controlling terminal /
/// the system font + theme registries WITHOUT actuating containment or spawning a
/// shell, so it is safe to run anywhere and unit-testable. A non-zero code
/// (`doctor`/`show-face` on bad input) makes them scriptable.
fn diag_report(cmd: &str, arg: Option<&str>) -> Option<(String, i32)> {
    match cmd {
        "show-config" => Some((show_config_report(), 0)),
        "explain-config" => Some((explain_config_report(), 0)),
        "doctor" => Some(doctor_report()),
        "list-fonts" => Some((list_fonts_report(), 0)),
        "show-face" => Some(show_face_report(arg)),
        "list-themes" => Some((list_themes_report(), 0)),
        "list-kitty-commands" => Some((list_kitty_commands_report(), 0)),
        _ => None,
    }
}

/// `aterm show-config` — aterm's effective runtime configuration as stable
/// `key=value` lines (one per line, scriptable). Reports the containment DEFAULT a
/// launch with no containment flag takes, NOT an actuated mode — `show-config`
/// never actuates or spawns. `shell=` is `$SHELL` on Unix and, on Windows, the
/// shell this tab runs when typed in an aterm tab, else the one a new window's
/// tab spawns (with `shell_origin=` naming the input that chose it).
fn show_config_report() -> String {
    let (rows, cols) = driver::host_winsize();
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let or = |s: String, dflt: &str| if s.is_empty() { dflt.to_string() } else { s };
    let mut out = String::new();
    out.push_str(&format!("version={}\n", aterm_types::version::APP_VERSION));
    // Shell: `$SHELL` on Unix — the program this passthrough session spawns.
    // On Windows the spawn reads neither `$SHELL` (a POSIX path in MSYS shells)
    // nor `%COMSPEC%` first, and reporting them here named cmd.exe from inside
    // a pwsh 7 tab and bash.exe from Git Bash (measured 2026-09-22) — the shell
    // the CLI was typed into, never the one a tab gets. So the row is the shell
    // THIS tab runs when there is one (`shell_origin=tab`: the window hands every
    // shell tab its program, since its own --shell flag is invisible to a CLI
    // process), else the one a new window's tab spawns, from the spawn's own
    // resolver, and `shell_origin=` says which input chose it (tab | aterm.toml |
    // default:<arm>; the retired environment twin is not an input).
    // Unix output is byte-identical.
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

/// `aterm explain-config` — explain how aterm resolves its configuration: the
/// precedence rule, the containment modes (least → most capability), and the
/// environment variables consulted. Read-only static reference text.
fn explain_config_report() -> String {
    let mut out = String::new();
    out.push_str("aterm configuration resolution\n\n");
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
        "  containment  No network; writes only to temp dirs and shell history; no access to\n\
         \x20              credential stores or private data (macOS; refused elsewhere).\n",
    );
    out.push_str("  safety       Capped limits, no OS sandbox.\n");
    out.push_str("  user         No sandbox, no extra limits (the default).\n");
    out.push_str("  master       Same as user.\n\n");
    out.push_str(
        "No environment variable changes what aterm does: every choice is a flag or an\n\
         aterm.toml key.\n",
    );
    #[cfg(windows)]
    out.push_str(&windows_config_paragraph());
    // Both tables are macOS settings that do nothing on Windows.
    #[cfg(not(windows))]
    out.push_str(PRIVACY_CONFIG_PARAGRAPH);
    #[cfg(not(windows))]
    out.push_str(MACHINE_CONFIG_PARAGRAPH);
    out
}

/// The Windows paragraph of `explain-config` (2026-09-22): where aterm.toml
/// lives on THIS machine — the path resolved by the same rule the window loads
/// it by (`aterm_types::dirs::aterm_config_path`), never a literal — and the two
/// keys a Windows daily driver asks about first, `shell` and `font_px`, which
/// the page never mentioned. Windows only, so the Unix report is byte-identical.
#[cfg(windows)]
fn windows_config_paragraph() -> String {
    let path = match aterm_types::dirs::aterm_config_path() {
        Some(path) => path.display().to_string(),
        None => "(unresolved: XDG_CONFIG_HOME, APPDATA and HOME are all unset)".to_string(),
    };
    format!(
        "\n\
         aterm.toml — the window's settings file on this machine:\n\
         \x20 {path}\n\
         \x20 (%APPDATA%\\aterm\\aterm.toml; a set XDG_CONFIG_HOME wins. The file need not\n\
         \x20 exist — every key has a default. `aterm --window --write-config` writes a\n\
         \x20 documented starter; `aterm help config` has the precedence rules.)\n\
         \x20 shell                   the interactive shell a new tab spawns: a bare name — \"pwsh\",\n\
         \x20                         \"cmd\", \"wsl\", \"nu\", or \"bash\", which finds Git Bash even off\n\
         \x20                         PATH — or an absolute path used verbatim. A window launched\n\
         \x20                         with --shell uses that instead; with neither, the default is\n\
         \x20                         pwsh, then powershell, then %COMSPEC%, then cmd.exe. `aterm\n\
         \x20                         doctor` names the program this machine resolves it to.\n\
         \x20 font_px                 glyph size in physical pixels (6..=200). A window launched\n\
         \x20                         with --font-px uses that instead; unset, the size follows\n\
         \x20                         the display scale.\n"
    )
}

/// The `[machine]` paragraph of `explain-config`. Hand-written like the rest.
///
/// Three things it must say, because each is otherwise guessed wrong: WHEN the
/// settings are applied — by `aterm pkg machine apply`, which the window runs as it opens
/// and a terminal session once a day, and by a package pass only when the table changed
/// (Phase 3, 2026-09-22: they used to walk $HOME at the top of every pass);
/// `defaults` writes the
/// ACCOUNT's per-host domain and ignores `$HOME`, so a redirected home is refused
/// rather than written through; and every change is undoable by one printed line.
#[cfg(not(windows))]
const MACHINE_CONFIG_PARAGRAPH: &str = "\n\
     [machine] — macOS host settings (aterm.toml; applied by the co-located atpkg):\n\
     \x20 universal_control       \"off\" (default) | \"leave\". Off writes the two per-host keys\n\
     \x20                         that stop the cursor and keyboard roaming to other Macs and\n\
     \x20                         iPads on the same Apple account; leave never touches them.\n\
     \x20 spotlight_noindex       true (default) renames the cargo target dirs the scan\n\
     \x20                         reaches under $HOME to target.noindex, so Spotlight never\n\
     \x20                         indexes build output. Cargo keeps working either way: a\n\
     \x20                         `target` symlink in a git checkout, a `[build] target-dir`\n\
     \x20                         line in .cargo/config.toml elsewhere. Documents, Desktop,\n\
     \x20                         Downloads, Pictures, Movies, Music and Library are never\n\
     \x20                         walked: macOS asks a human before a program reads those.\n\
     \x20 Both are applied by `aterm pkg machine apply`, which the window runs as it opens\n\
     \x20 and a terminal session once a day (the day's first interactive one), and by a\n\
     \x20 package pass (update, seed, install) when the [machine] table changed since\n\
     \x20 they were last applied; `aterm pkg machine` reads the measured state, and\n\
     \x20 Settings ▸ Security shows it with the two switches and an Apply now button. A\n\
     \x20 saved change lands on the next package pass, or on Apply now. `defaults` writes\n\
     \x20 the ACCOUNT's per-host domain regardless of $HOME, so an apply under a redirected\n\
     \x20 HOME is refused and says so, as is one whose aterm.toml does not parse (the\n\
     \x20 switches cannot be read, and both defaults act). Undo Universal Control with the\n\
     \x20 revert line `aterm pkg machine` prints (`defaults -currentHost delete\n\
     \x20 com.apple.universalcontrol Disable`, then the same for DisableMagicEdges) AND set\n\
     \x20 universal_control = \"leave\", or the next apply disables it again. Undo a rename in\n\
     \x20 a git checkout by removing the `target` symlink and renaming target.noindex back;\n\
     \x20 elsewhere the apply left no symlink and pointed cargo with a `[build] target-dir`\n\
     \x20 line in .cargo/config.toml, so delete that line as well.\n";

/// The `[privacy]` paragraph of `explain-config` (design §4). Hand-written, like
/// the rest of this page.
///
/// Three things it must say, because each is otherwise guessed wrong:
///
/// * the probe reads state that already exists and raises no dialog — a reader
///   who thinks otherwise turns it off and loses the only honest report;
/// * the warm-up DELIBERATELY raises the prompts, so it is an owner gesture and
///   nothing else — never first launch, never a timer, never a program inside a
///   session;
/// * no key here is deferred: saving the file is the whole of it.
///
/// What it must NOT say is that the grant ends macOS consent dialogs. Which
/// services a grant covers is unmeasured (design §7 S4), so this text describes
/// the grant by what it is and stops there.
#[cfg(not(windows))]
const PRIVACY_CONFIG_PARAGRAPH: &str = "\n\
     [privacy] — macOS consent (aterm.toml; the window reads it, the passthrough CLI does not):\n\
     \x20 enabled / check         the silent Full Disk Access probe. It reads state that already\n\
     \x20                         exists and raises NO dialog. Off, every field reads `unknown` —\n\
     \x20                         which is not `denied`, and the report says which it is.\n\
     \x20 notice                  the macOS access card: when file access is not confirmed, it\n\
     \x20                         offers Open Settings (the Full Disk Access pane) and Not now.\n\
     \x20                         Unanswered offers may return after a day; Not now is remembered.\n\
     \x20 warmup                  \"never\" | \"on-request\". The warm-up asks macOS for the folders\n\
     \x20                         up front, which RAISES the dialogs on purpose, so it happens\n\
     \x20                         only when the owner presses the button in Settings — never at\n\
     \x20                         first launch, never on a timer, and never because a program\n\
     \x20                         inside a session asked for it.\n\
     \x20 warmup_folders          what that gesture asks for: the folders, and `app-data` \u{2014}\n\
     \x20                         every other app's own data, which macOS guards with the same\n\
     \x20                         kind of grant. warmup_hold_ms caps how long an in-place\n\
     \x20                         apply will wait for it.\n\
     \x20 probe_interval_ms       floor on re-probing; macOS probe duration has no guaranteed bound.\n\
     \x20                         `aterm ctl @<sid> await consent` waits for completed observations.\n\
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

/// `aterm show-face <family>` — the resolved path + cell metrics for a font family
/// as stable `key=value` lines; `px=` is the size the metrics were measured at
/// ([`aterm_render::FaceInfo::PROBE_PX`]), so `cell_width=` is a fact and not a
/// riddle. Exit 1 (usage / not-found) when `family` is absent or unresolvable.
/// Data: [`aterm_render::face_info`].
fn show_face_report(family: Option<&str>) -> (String, i32) {
    let Some(family) = family else {
        return ("ERR: usage: aterm show-face <family>\n".to_string(), 1);
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
            format!("ERR: could not resolve or load font family {family:?}\n"),
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
    out
}

/// `aterm list-kitty-commands` — every word the cursor cat obeys when it is TYPED
/// at the terminal, one row per (trick, language), then each language's names
/// for the pet and its filler words. Data: [`aterm_lexicon::TrickLexicon::all_rows`]
/// — the embedded vocabulary the window's typed-line listener compiles, every
/// language, gated rows included (`gated=1` marks a row that loads only when
/// `[sparkle_words] languages` lists its language).
///
/// NO BANNER, like `list-fonts`: every line is one [`aterm_lexicon::tricks::row_line`]
/// — space-separated `key=value` fields — so `aterm list-kitty-commands | grep
/// lang=es` is already the language filter and nothing has to skip a heading.
/// The prose lives on `aterm help kitty`, which prints these same rows.
pub(crate) fn list_kitty_commands_report() -> String {
    let mut out = String::new();
    for row in aterm_lexicon::TrickLexicon::all_rows() {
        out.push_str(&aterm_lexicon::tricks::row_line(&row));
        out.push('\n');
    }
    out
}

/// A doctor row's verdict. Three-valued since the macOS consent work
/// (docs/DESIGN-macos-tcc-prompts-2026-08-30.md §3.8): `Note` reports something
/// the operator may want to act on WITHOUT moving the exit code, so
/// `aterm doctor && aterm` keeps working on a machine that has simply not
/// granted Full Disk Access. Only [`Mark::Fail`] fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// The check passed.
    Ok,
    /// A fact worth reporting that is NOT a failure — nothing to fix, or
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

    /// `true` for the ONE mark that moves the exit code.
    const fn is_fail(self) -> bool {
        matches!(self, Self::Fail)
    }

    /// The mark for a two-valued check (containment / shell / tty), whose
    /// Ok/Fail semantics are unchanged.
    const fn from_ok(ok: bool) -> Self {
        if ok { Self::Ok } else { Self::Fail }
    }
}

/// One `doctor` row: the label as it appears in the mark block, its verdict, and
/// the detail line. `detail` carries its own `key: …` prefix, so the detail block
/// stays greppable as a single uniform shape.
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
/// [`privacy_facts`] and consumed by the pure [`doctor_checks`] — passed in
/// exactly as `shell_executable` and `is_tty` are, so the report stays a
/// deterministic function of its inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrivacyFacts {
    /// The instance's Full Disk Access state, from ONE prompt-free probe.
    fda: FdaState,
    /// WHY `fda` reads as it does. `label.refused()` means NO syscall ran, so
    /// the row names the configuration rather than implying a denial.
    fda_label: ProbeLabel,
    /// The class of this build's designated requirement — whether a grant made
    /// today survives the next build. [`DrClass::Unknown`] asserts nothing.
    dr: DrClass,
    /// The display name of the process macOS holds RESPONSIBLE for this one.
    /// The verdict belongs to it: a probe run from a shell under another
    /// terminal reports THAT terminal's access, not aterm's.
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

/// Gather the consent facts for the `privacy:` row — the impure half, alongside
/// `$SHELL` and the tty check.
///
/// The FENCE is `aterm_containment`'s in-bundle guard, not a flag here:
/// [`aterm_containment::probe_fda`] performs its one `open()` only when the
/// running executable resolves inside a `.app` (design §3.3 guardrail 1, and
/// AGENTS.md rule 5 — the 2026-08-17 incident), and `ProbeLabel::refused()`
/// reports whether a syscall happened at all. `aterm doctor` run from
/// `target/debug/…`, or from a test binary — which
/// `diag_commands_advertised_and_dispatchable` really does — takes the refused
/// arm: no probe, no responsible-app lookup, no `codesign`.
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

/// The class of the running bundle's designated requirement — what `tccd` stores
/// beside a grant and re-validates against, i.e. whether a grant made today
/// survives the next build.
///
/// `codesign -d -r-` is the only unprivileged way to read it, and it is spawned
/// ONLY from inside a resolved `.app` (the same fence as the probe). Any failure
/// — no bundle, no `codesign`, unparseable output — is [`DrClass::Unknown`],
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
    let Ok(out) = std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .arg(&app_root)
        .output()
    else {
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

/// Why a probe reports what it does, in a sentence a human can act on. Only the
/// `not measured` arms are ever rendered, but every label is spelled so a new
/// variant cannot silently render as an empty reason.
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
/// Never [`Mark::Fail`] — a machine that has not granted Full Disk Access is a
/// normal machine, and nothing here is a reason to refuse to launch. The grant
/// is described by what it is (access held by the responsible app), never as
/// something that removes every consent dialog: which services it covers is not
/// measured, and this row does not claim it.
fn privacy_row(facts: &PrivacyFacts) -> (Mark, String) {
    let who = facts.responsible.as_deref().unwrap_or("unknown");
    // A build whose grants die on the next build is worth saying whatever the
    // access state is — but ONLY on a positively classified requirement:
    // `DrClass::Unknown` is not evidence of a dev build.
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
                 can be interrupted by macOS consent dialogs{dev}"
            ),
        ),
    }
}

/// `aterm doctor` — an aggregate pre-flight health check (the shell
/// set+executable — `$SHELL` on Unix, the shell a new WINDOW tab spawns on
/// Windows — stdout is a tty, the macOS consent posture, the aterm instances no
/// control socket reaches, the recovery ledger, plus version/size). Reads
/// env/fs/tty + the consent tier, then delegates to the pure
/// [`doctor_checks_with_origin`] (through `doctor_checks` off Windows). Exit 0 =
/// nothing FAILED; non-zero = a problem. Scriptable: `aterm doctor && aterm`.
fn doctor_report() -> (String, i32) {
    let is_tty = driver::stdout_is_tty();
    let (rows, cols) = driver::host_winsize();
    let privacy = privacy_facts();
    let instances = aterm_ctl::census::window_census();
    let recovery = recovery_facts();
    // Unix: `$SHELL`, the program this passthrough session spawns.
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").ok();
        let shell_exec = shell.as_deref().is_some_and(driver::shell_is_executable);
        doctor_checks(
            shell.as_deref(),
            shell_exec,
            is_tty,
            rows,
            cols,
            &privacy,
            &instances,
            &recovery,
        )
    }
    // Windows: this tab's shell inside an aterm tab, else the one a new window's
    // tab spawns, from the spawn's own resolver over the window's inputs, and
    // labelled as such — the row used to report
    // this process's `$SHELL`, then `%COMSPEC%`, and said `cmd.exe` from inside
    // a pwsh 7 tab and `bash.exe` from Git Bash (measured 2026-09-22): the shell
    // the CLI was typed into, which the spawn never reads. See
    // `driver::WindowShell`.
    #[cfg(windows)]
    {
        let window_shell = driver::WindowShell::resolve();
        let shell_exec = driver::shell_is_executable(&window_shell.program);
        doctor_checks_with_origin(
            Some(&window_shell.program),
            shell_exec,
            Some(&window_shell.origin_sentence()),
            is_tty,
            rows,
            cols,
            &privacy,
            &instances,
            &recovery,
        )
    }
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
/// windowed launch ended, and the last unexpected end in detail. A NOTE when any run
/// ended unexpectedly (a kill, a fatal signal, a panic) — a past crash is a fact to
/// report, never a reason for `doctor` to fail — and OK otherwise. On Windows the
/// window writes no ledger (its recovery census is Unix-only), so the row says so
/// rather than promise a census that never starts.
fn recovery_row(facts: &RecoveryFacts) -> (Mark, String) {
    if cfg!(windows) {
        return (Mark::Note, "not recorded on Windows".to_string());
    }
    let Some((rows, path)) = facts else {
        return (
            Mark::Note,
            "not measured (no log directory resolves, so there is no ledger)".to_string(),
        );
    };
    let (lines, unexpected) = aterm_update::recovery_ledger::doctor_lines(rows, path);
    let mark = if unexpected { Mark::Note } else { Mark::Ok };
    (mark, lines.join("\n"))
}

/// The `instances:` row, pure over the census ([`aterm_ctl::census`]): the aterm
/// windows and headless instances running as this user that no published control
/// socket reaches — invisible to `aterm ctl ls`, and so to everything that drives
/// or reaps through it.
///
/// Never [`Mark::Fail`], and it NEVER STOPS anything: an unreached instance may be
/// a person's deliberate `--no-control-sock` window or a harness still running, so
/// the row names each one (pid, age, command line) and leaves the decision to the
/// person reading it. A census that could not be taken says `not measured` and
/// why — never "none".
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
         (`aterm ctl ls` cannot see {pronoun}) — reported, never stopped; whether to end one \
         is your call"
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
/// testable): aggregate the pre-flight checks into a stable report + exit code
/// (0 = nothing failed, 1 = any [`Mark::Fail`]).
///
/// The verdict is three-valued: `health`/the exit code are computed from `Fail`
/// ALONE, so a `Note` row — the macOS consent posture is the first — reports a
/// fact without making `aterm doctor && aterm` refuse to launch.
///
/// The `shell:` row is unlabelled here: this is the Unix report, byte for byte.
/// [`doctor_checks_with_origin`] is the same report with the row labelled — the
/// live Windows path, which is why this shape is test-only there.
#[cfg(any(not(windows), test))]
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is one gathered fact; passing them in is what keeps the report pure"
)]
fn doctor_checks(
    shell: Option<&str>,
    shell_executable: bool,
    is_tty: bool,
    rows: u16,
    cols: u16,
    privacy: &PrivacyFacts,
    instances: &Result<aterm_ctl::census::Census, String>,
    recovery: &RecoveryFacts,
) -> (String, i32) {
    doctor_checks_with_origin(
        shell,
        shell_executable,
        None,
        is_tty,
        rows,
        cols,
        privacy,
        instances,
        recovery,
    )
}

/// [`doctor_checks`] with the `shell:` row labelled by WHERE the shell came from
/// (`shell_origin`, appended after the fact as ` — <label>`). Windows passes
/// `driver::WindowShell::origin_sentence`, so the row says whose shell it names
/// and which input chose it — a reader who sees `pwsh.exe` from inside a
/// Git Bash tab must not have to guess whether doctor looked at `$SHELL`. `None`
/// renders exactly the unlabelled row.
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is one gathered fact; passing them in is what keeps the report pure"
)]
fn doctor_checks_with_origin(
    shell: Option<&str>,
    shell_executable: bool,
    shell_origin: Option<&str>,
    is_tty: bool,
    rows: u16,
    cols: u16,
    privacy: &PrivacyFacts,
    instances: &Result<aterm_ctl::census::Census, String>,
    recovery: &RecoveryFacts,
) -> (String, i32) {
    let (shell_ok, shell_fact) = match shell {
        None => (false, "$SHELL unset".to_string()),
        Some("") => (false, "$SHELL is empty".to_string()),
        Some(p) if shell_executable => (true, format!("{p} (executable)")),
        Some(p) => (false, format!("{p} (not executable or missing)")),
    };
    let shell_fact = match shell_origin {
        Some(origin) => format!("{shell_fact} — {origin}"),
        None => shell_fact,
    };

    // A NOTE, never a FAIL, and this is the row that made `doctor` unusable as
    // the thing its own help calls it: "Pre-flight health check; exit non-zero
    // on a problem."
    //
    // The check is SELF-REFERENTIAL — it measures `doctor`'s own stdout, not
    // aterm's ability to run. aterm is a windowed terminal that allocates its
    // own pty; whether the process that asked for a health report had its
    // output piped says nothing about the machine. But `Mark::from_ok(false)`
    // made it `FAIL`, so `aterm doctor > out.txt`, `aterm doctor | tee`, every
    // CI invocation, and every capture-the-output caller got `health: FAIL —
    // one or more checks did not pass` and exit 1 on a perfectly healthy box.
    // A health check that fails whenever anything reads its output cannot gate
    // anything, and the plan to have the GUI run repair "when doctor fails"
    // would have run it every single launch.
    //
    // `Mark::Note` is defined for exactly this: "a fact worth reporting that is
    // NOT a failure — nothing to fix, or nothing aterm can fix, and never a
    // reason to refuse to launch."
    //
    // The size goes with the fd it was measured on: on a terminal it is that
    // terminal's; piped, `host_winsize` answers the 24x80 fallback, which is not a
    // fact about anything, so the row carries none.
    let (tty_mark, tty_fact) = if is_tty {
        (
            Mark::Ok,
            format!("stdout is a terminal, {cols} cols x {rows} rows"),
        )
    } else {
        (Mark::Note, "stdout is not a terminal".to_string())
    };

    let (privacy_mark, privacy_fact) = privacy_row(privacy);
    let (instances_mark, instances_fact) = instances_row(instances);
    let (recovery_mark, recovery_fact) = recovery_row(recovery);
    let checks = [
        DoctorRow {
            label: "shell:",
            mark: Mark::from_ok(shell_ok),
            fact: shell_fact,
        },
        DoctorRow {
            label: "tty:",
            mark: tty_mark,
            fact: tty_fact,
        },
        DoctorRow {
            label: "privacy:",
            mark: privacy_mark,
            fact: privacy_fact,
        },
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
    ];

    let all_ok = !checks.iter().any(|row| row.mark.is_fail());

    // One row per check — the label, the mark, the fact — so each fact is said once
    // (every one used to be said twice: a marks block, then each fact again with its
    // label). A fact of several lines keeps its later lines under the fact column.
    const LABEL: usize = "instances: ".len();
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
        aterm_types::version::APP_VERSION
    ));
    out.push_str(if all_ok {
        "health: OK — aterm is ready to run\n"
    } else {
        "health: FAIL — one or more checks did not pass\n"
    });
    (out, i32::from(!all_ok))
}

/// The outcome of parsing the command line: either print-and-exit (help/version),
/// reject (usage error), or proceed with an optional containment override that the
/// init funnel in `main()` will resolve.
#[derive(Debug, PartialEq, Eq)]
enum CliAction {
    /// `-h`/`--help`: print [`help_text`] to stdout, exit 0.
    Help,
    /// `-V`/`--version`: print the version to stdout, exit 0.
    Version,
    /// A diagnostic subcommand (e.g. `show-config`, or `show-face <family>`): print
    /// introspection and exit WITHOUT spawning a shell. `cmd` is a registry-validated
    /// name (one of [`DIAG_COMMANDS`]); `arg` is the optional positional operand
    /// after it (the family for `show-face`).
    Diag { cmd: String, arg: Option<String> },
    /// `<diag> --help`: print that subcommand's one-line description and usage,
    /// exit 0 — never run it. The same rule the ctl surface follows: asking a
    /// verb how it works must never do the thing.
    DiagHelp { cmd: String },
    /// `help [topic]`: print the AI-facing toolchain manual (via [`manual`]) and exit 0
    /// WITHOUT spawning a shell. `topic` is the optional deep-dive selector; `None`
    /// prints the front page (or, inside an aterm session, the agent brief).
    Manual { topic: Option<String> },
    /// `agents [<cmd> [<agent>…]]`: manage the coding-agent primer (via the
    /// `aterm-primer` crate) and exit WITHOUT spawning a shell. `rest` is everything
    /// after the word `agents` (subcommand + optional agent names), parsed by the
    /// crate itself.
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

/// Pure argument-decision core (no process exit, no env mutation, no I/O): takes the
/// argv tail and returns a [`CliAction`]. Factored out so the precedence + fail-closed
/// edge cases are unit-testable; [`parse_args`] is the thin effectful wrapper.
///
/// Recognizes `-h`/`--help`, `-V`/`--version`, and the containment selection:
/// `--containment <MODE>` (space form) or `--containment=<MODE>` (`=` form), plus the
/// convenience `--sandbox` (= containment) and `--no-sandbox` (= user). A bare `--`
/// ends option parsing; since `aterm` takes no positional operands, anything after it
/// that begins with `-` is no longer treated as a flag (a trailing non-flag operand is
/// still rejected, as `aterm` accepts none).
///
/// Precedence is deterministic and total:
///   explicit flag  >  default `User` (no environment variable selects the mode),
/// and among MULTIPLE conflicting flags the LAST one on the line wins (standard
/// last-flag-wins, e.g. `--sandbox --containment user` selects `user`; `--no-sandbox
/// --sandbox` selects sandbox/containment). Only the surviving selection is returned,
/// so the caller hands exactly one value to the single init funnel.
///
/// An INVALID `--containment <value>` is carried through verbatim (NOT validated here),
/// so it reaches `session_main`'s `init_mode_from_flag` and fails CLOSED to
/// Containment — never a parallel/bypass validation.
fn decide_args<I: Iterator<Item = String>>(args: I) -> CliAction {
    let mut args = args.peekable();
    // `aterm help [topic]` dispatches first, git-style: the manual is the first operand.
    // It prints and exits without spawning a shell. Distinct from `-h`/`--help` (the terse
    // CLI usage) — the WORD `help` is the toolchain manual. A single optional topic follows.
    if args.peek().map(String::as_str) == Some("help") {
        let _ = args.next();
        return CliAction::Manual { topic: args.next() };
    }
    // `aterm agents …` dispatches the same way, git-style: the primer installer is
    // the first operand, everything after it belongs to the `aterm-primer` crate's
    // own little parser (subcommand + agent names). Prints and exits, no shell spawned.
    if args.peek().map(String::as_str) == Some("agents") {
        let _ = args.next();
        return CliAction::Agents {
            rest: args.collect(),
        };
    }
    // A leading diagnostic subcommand (`aterm show-config`) dispatches BEFORE any
    // flag parsing — git-style: the subcommand is the first operand. It prints
    // introspection and exits without spawning a shell. Anything else (flags, or an
    // unknown first operand) falls through to the existing option parser.
    if let Some(first) = args.peek()
        && DIAG_COMMANDS
            .iter()
            .any(|(name, _)| *name == first.as_str())
    {
        let cmd = args.next().expect("peeked Some");
        let rest: Vec<String> = args.collect();
        // `<diag> --help` DESCRIBES the subcommand — same rule as `aterm ctl`.
        // Before this, `-h` after `show-face` became the FAMILY operand and the
        // report fabricated a plausible, successful, wrong answer for a font
        // named "-h" (audit D-8).
        if rest.iter().any(|a| a == "-h" || a == "--help") {
            return CliAction::DiagHelp { cmd };
        }
        // The tail is PARSED, never dropped. These verbs used to run to
        // completion at exit 0 on any argument — the comment here read
        // "Subcommands that take none simply ignore it" — so `atpkg doctor
        // --json`-style probes taught script authors that their flag was
        // accepted when it was discarded (audit D-12). A caller must be able
        // to distinguish "takes no flags" from "your flag was wrong".
        let takes_operand = cmd == "show-face";
        if !takes_operand && !rest.is_empty() {
            return CliAction::Usage(format!(
                "aterm {cmd}: unknown argument {:?} — this command takes none \
                 (try `aterm {cmd} --help`)",
                rest[0]
            ));
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
                // `=` form: `--containment=<MODE>`. An empty value (`--containment=`)
                // is carried through verbatim so it fails CLOSED in the init funnel,
                // exactly like an invalid mode — never silently ignored.
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
        // Either an unrecognized option, or any operand after `--`: `aterm` accepts
        // no positional operands, so reject with exit-2 usage rather than ignoring it.
        // A bare word is a command the front door did not resolve, not an option.
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

/// The `-V`/`--version` text. Line one is the identity, `aterm <version>` — the
/// self-identification `tools/install.sh` greps (`^aterm `), unchanged. Then WHICH
/// COPY runs (S12 of `docs/DESIGN-which-copy-runs-2026-08-27.md`): `running: <path>`
/// (the `.app` on macOS, the executable elsewhere) and, per other `aterm.app` in the
/// usual places, `another copy: <path> (<version>) — the updater leaves it alone` —
/// the lines `aterm_update::which_copy` spells, so Settings ▸
/// About says the same words. `None` (no executable path at all) prints identity only
/// — then `build: <N>`, the monotonic build number the updater orders by
/// (2026-09-14, audit BA-8): the pre-swap start probe runs the candidate with
/// `--version` and requires the text to name the build it is about to install,
/// a clause that had been dead since versions moved to `MAJOR.MINOR.0` and the
/// number left the identity line. Omitted (no line) when the launcher has not
/// published one ([`set_running_build`]) — a `0` would be a claim. And last,
/// WHAT THIS BUILD TRUSTS: `trusts: master=<sha256>`, the fingerprint of the
/// paper master compiled in — the one anchor that authorizes a release — so a
/// client stranded by a master rotation can be told from a healthy one by its
/// own output (`empty` names an unarmed tier).
#[must_use]
pub fn version_text(copy: Option<&aterm_update::which_copy::WhichCopy>) -> String {
    // Identity line first (install.sh greps `^aterm `), then the origin line —
    // `by Andrew Yates · ALab · alab.systems` — so `--version` says where the
    // program comes from; then the build number, which copy runs, and the trust roots.
    let mut out = format!(
        "aterm {}\n{}\n",
        aterm_types::version::APP_VERSION,
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

/// The running build number, published by the one-binary launcher before it
/// dispatches (`crates/aterm/src/main.rs`): this crate has no build stamp of its
/// own — the number is minted by aterm-gui's build script — so the launcher,
/// which links both, hands it over. First call wins; `0` (an unstamped dev
/// build) publishes nothing.
static RUNNING_BUILD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// Publish the running build number for [`version_text`]. See [`RUNNING_BUILD`].
pub fn set_running_build(build: u64) {
    if build > 0 {
        let _ = RUNNING_BUILD.set(build);
    }
}

fn running_build() -> Option<u64> {
    RUNNING_BUILD.get().copied()
}

/// What the one-binary launcher runs when the session's shell first speaks: the
/// passthrough loop has written the shell's first output to the terminal. The launcher
/// (`crates/aterm/src/main.rs`) confirms a replaced Linux executable's trial on it — a
/// prompt on the screen is the session's proof of a healthy start, as the first frame is
/// the window's (2026-09-28). First registration wins.
static FIRST_SHELL_OUTPUT: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Register the hook [`FIRST_SHELL_OUTPUT`] names; `false` when one is already set. It
/// runs ONCE, on the passthrough loop itself (the unix driver), so it must not block:
/// signal a thread, never do the work. Register it before [`session_main`] spawns the
/// shell, or its first output can come first.
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

/// Dependency-free argument parser for the daily-driver CLI — the effectful shell
/// around [`decide_args`]: prints help/version and exits 0, prints a usage error and
/// exits 2, or records the session's launch flags ([`SessionFlags`], read by
/// [`session_main`]) and returns.
///
/// With no args (a Finder/.app launch) this is a no-op and a normal interactive shell
/// starts, unchanged. Returns whether `-q`/`--quiet` was passed (suppress the
/// interactive startup notice).
pub fn parse_args(argv: Vec<std::ffi::OsString>) -> bool {
    // Lossy conversion (NOT `std::env::args`, which PANICS on a non-UTF8 argument): a
    // non-UTF8 operand becomes a replacement-char string that `decide_args` rejects as an
    // unknown option (clean exit-2 usage) — never an exit-101 abort.
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
            // Context-aware: inside an aterm session `aterm help` prints the agent brief
            // (with the session's sid wired in); outside it prints the reference front
            // page. `help <topic>` prints the deep dive either way. Exit 2 on an unknown
            // topic so it scripts, mirroring the diag path.
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

/// The session's launch flags — what its command line asked of it. Recorded by
/// [`parse_args`], read by [`session_main`]; never exported, so a child inherits
/// nothing from them. They used to be environment variables the parser wrote back
/// (`--containment` set `$ATERM_CONTAINMENT_MODE`), which let an export change what
/// a shipped session did (owner, 2026-09-22: "NOT ENV VARS those are for
/// development").
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

/// Pure PATH-prepend: the `("PATH", value)` pair that puts `dir` first on the child's
/// PATH. `None` when `dir` is already present (idempotent for aterm-inside-aterm nesting),
/// mirroring aterm-gui's bundle-dir arm of `spawn::reroute_path_env`. The platform
/// separator is `;` on Windows.
fn prepend_path(dir: &str, inherited: Option<&str>) -> Option<(String, String)> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    match inherited {
        Some(p) if p.split(sep).any(|c| c == dir) => None,
        Some(p) if !p.is_empty() => Some(("PATH".to_string(), format!("{dir}{sep}{p}"))),
        _ => Some(("PATH".to_string(), dir.to_string())),
    }
}

/// aterm's own binary directory, to hand the child shell so the ONE front door — `aterm`
/// itself, and thus every `aterm <verb>` — is reachable even when aterm was launched by an
/// absolute path from a dir not on `$PATH`. With ONE binary the dir IS the toolset when it
/// is a REAL install — proven by the `aterm-ctl` argv0 alias the bundle and the source
/// store both lay down beside the binary. A lone binary in an uncontrolled directory
/// (~/Downloads) yields `None`, preserving the binary-era invariant.
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

/// `atpkg::reroute::REROUTE_DIR_ENV` and `PASSTHROUGH_ENV`, restated: this crate links
/// `atpkg` for its tests only (Cargo.toml: "the router composes these crates, aterm-cli
/// does not call atpkg"), and `reroute_env_names_match_atpkg` pins both spellings.
/// `PASSTHROUGH_ENV` is INTERNAL protocol — the marker the front door establishes for an
/// `aterm --no-reroute` session and clears on every other launch — never a setting.
const REROUTE_DIR_ENV: &str = "ATERM_REROUTE_DIR";
const PASSTHROUGH_ENV: &str = "__ATERM_REROUTE_PASSTHROUGH";

/// The session's reroute directory, read at this edge from `$ATERM_REROUTE_DIR` — which the
/// front door (`crates/aterm/src/main.rs`) resolves from the configured store, lays the
/// stubs into, and establishes in THIS process's environment before `session_main` runs.
/// `None` when the `--no-reroute` marker (`$__ATERM_REROUTE_PASSTHROUGH`) is engaged
/// (non-empty and not `0` — `env_flag_engaged`, THE reading, the same the stubs and the
/// window apply), when the variable is unset or
/// empty, when it does not name an existing directory (Windows lays no stubs; a stale
/// value must never put a nonexistent entry first on every child's PATH), or when the
/// value is RELATIVE (2026-09-16 audit: the front door always hands an absolute path, so a
/// relative one is an inherited stray — put first on PATH it would resolve against every
/// directory the shell later `cd`s into, and its `agents/` sibling would be derived in and
/// created under the session's cwd).
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

/// Whether `dir` is a non-empty ABSOLUTE path naming an existing directory — the one
/// shape either managed directory may have before it goes first on a child's PATH.
fn is_absolute_existing_dir(dir: &str) -> bool {
    let path = std::path::Path::new(dir);
    !dir.is_empty() && path.is_absolute() && path.is_dir()
}

/// [`is_absolute_existing_dir`] and not a symlink: the shape the front door's
/// `$ATERM_AGENTS_DIR` handoff always has (it refuses a link at `agents/`), so a stray
/// that is one is not the front door's and is not taken.
fn is_absolute_real_dir(dir: &str) -> bool {
    is_absolute_existing_dir(dir)
        && std::fs::symlink_metadata(dir).is_ok_and(|md| !md.file_type().is_symlink())
}

/// `ATPKG_AGENTS`, atpkg's spelling restated (`atpkg::hooks` exports it from the shell
/// hook; pinned by `reroute_env_names_match_atpkg`): the managed `<prefix>/agents/` as an
/// enclosing shell that sourced the hook names it. Read here, NEVER exported (the shell
/// integration keys its hook-sourcing on it being unset).
const HOOK_AGENTS_ENV: &str = "ATPKG_AGENTS";

/// `atpkg::reroute::AGENTS_DIR_ENV` restated (pinned by `reroute_env_names_match_atpkg`):
/// the managed `<prefix>/agents/` as THE FRONT DOOR hands it (2026-09-18) — resolved
/// from the configured store, ensured to exist, absolute — on every lane, engaged
/// reroute or not. Absent or empty means none. The one handle this lane prefers,
/// [`managed_agents_dir`]; re-exported to the shell so a nested session reads the same.
const AGENTS_DIR_ENV: &str = "ATERM_AGENTS_DIR";

/// THE MANAGED `agents/` DIRECTORY A TTY SESSION PUTS IN FRONT OF ITS SHELL'S PATH
/// (2026-09-16). The window's spawn seam has front-inserted `<prefix>/agents/` — the
/// shims of the agent programs aterm is the version manager for (`claude`, `codex`),
/// ahead of `~/.local/bin/claude` and the brew casks — since 2026-09-10, and ensured the
/// directory at launch since 2026-09-16 (`aterm-gui`'s `spawn::managed_agents_dir`).
/// The `aterm` TTY session never did: its shell got `reroute/` in front and nothing
/// else, so `claude` there was whichever copy the user's rc put first until the shell
/// hook ran. Owner, 2026-09-16: "all the latest and best MUST WORK IN THE SAME TAB with
/// live update!"
///
/// WHAT THIS LANE'S FRONT-INSERT IS, HONESTLY (audit 2026-09-16): a PRE-RC hint. The
/// session spawns its shell as a LOGIN shell with no aterm shell integration (that is
/// the window's ZDOTDIR/`--rcfile` seam; this lane sets neither `ATERM_CHILD` nor
/// `ATERM_SESSION_ID`, see `session_main`), so on macOS `/etc/zprofile`'s `path_helper`
/// rebuilds PATH before any rc runs — measured on this Mac, 2026-09-16: the two managed
/// dirs handed first came out at positions 12–13, behind `/usr/local/bin` and
/// `/opt/homebrew/bin`. What puts `agents/` FIRST at the prompt of a TTY session is the
/// rc-sourced `~/.aterm/shell.d/00-atpkg.<shell>` hook atpkg wires into `~/.zshrc` /
/// `~/.bashrc` / `config.fish` (it MOVES the dir to the front, every earlier mention
/// removed), once, at shell start; there is no per-prompt re-assert and no live
/// re-source of a rewritten hook in this lane — those are the window tab's. So in a TTY
/// session the managed `claude`/`codex` lead from the first prompt on a machine whose rc
/// carries the hook (and a later update pass is picked up on the next invocation, because
/// atpkg re-lays the twins in place under the directory that is already on PATH); on a
/// machine whose rc does not yet carry it, the entry handed here survives (once, behind
/// `path_helper`'s list) and `. ~/.aterm/shell.d/00-atpkg.<shell>` moves it first.
/// Since 2026-09-27 the hook does the same for `reroute/`, inside aterm only, one step
/// ahead of `agents/` (the order this lane hands), so a bare `cargo` meets the signpost.
/// Pinned by `a_login_zsh_demotes_the_seams_front_insert_and_the_rc_hook_puts_reroute_first_in_aterm`.
///
/// WHICH DIRECTORY, in order (the precedence is pinned by
/// `managed_agents_dir_prefers_the_front_doors_handoff_then_the_sibling_then_the_hook`):
///
/// 1. `handed` — `$ATERM_AGENTS_DIR`, THE FRONT DOOR'S HANDOFF (2026-09-18, closing
///    R3 below): the one binary that links atpkg (`crates/aterm/src/main.rs`) resolves
///    the configured store, ensures `<prefix>/agents` through
///    `Layout::ensure_agents_dir` — the rule the window's `spawn::managed_agents_dir`
///    shares, a symlink/file at `agents/` refused — and
///    establishes the absolute directory in this process's environment on EVERY lane,
///    engaged reroute or not, removing an inherited stray when it hands nothing (no
///    layout, a refused `mkdir`, a link or file at `agents/`: one stderr line there, and
///    this lane sees no handoff). Taken when it is a
///    non-empty ABSOLUTE path naming an existing REAL directory (not a symlink — the
///    front door never hands one; a stray that is one must not capture the twins);
///    nothing is created for it here, the front door already did.
/// 2. the SIBLING DERIVATION — this crate does not link atpkg (Cargo.toml: "the router
///    composes these crates, aterm-cli does not call atpkg"), so the store's layout is
///    not asked here; the directory is DERIVED from the same contract the window
///    resolves through `atpkg::store::Layout` — `reroute/` and `agents/` are siblings
///    under the ONE manager prefix (`Layout::reroute_dir` = `<prefix>/reroute`,
///    `Layout::agents_dir` = `<prefix>/agents`; pinned against the real layout by
///    `the_agents_dir_is_the_reroute_dirs_sibling_in_atpkgs_layout`) — from the reroute
///    directory the front door hands this process as `$ATERM_REROUTE_DIR` (which it
///    sets only for an existing directory, so the prefix exists too; a relative value is
///    refused before anything is derived or created, [`reroute_dir_from_env`]). The
///    fallback for a REFUSED handoff (the front door hands nothing when `ensure_dir`
///    refused) — ENSURED here, below, and silent on a refusal because the front door
///    said it.
/// 3. `enclosing_agents` — `$ATPKG_AGENTS` as an enclosing shell that sourced the atpkg
///    hook exported it — when it is an absolute existing directory; otherwise there is
///    nothing to front-insert.
///
/// R3 (2026-09-16), CLOSED 2026-09-18: the reroute escape hatch is for the UPSTREAM
/// RUST NAMES (`aterm help reroute`), yet in this lane it also dropped the managed
/// `claude`/`codex` front-insert, because the agents dir was derived from the reroute
/// handle and the front door handed nothing else — an `aterm --no-reroute` from
/// Terminal.app (no `$ATPKG_AGENTS` in the environment) left `agents/` to the rc hook
/// alone. Closed by rule 1: the front door hands `$ATERM_AGENTS_DIR` regardless of the
/// reroute switch. The variable is deliberately not `ATPKG_AGENTS` (an inherited
/// `ATPKG_AGENTS` would stop the window's shell integration from ever sourcing the
/// hook), and the shell integration must never read it — that contract keys on
/// `$ATPKG_AGENTS` alone and is left as it is.
///
/// ENSURED TO EXIST (rule 2), the way the window ensures it through `Layout::ensure_dir`, whose
/// rule is restated from the PREFIX's own metadata ([`agents_dir_mode`]): one `mkdir`
/// (`0700` in a prefix we own — the `$HOME` shape; `0755` in a root-owned system
/// prefix — every user must traverse it, a `0700` there is exactly the failure
/// `ensure_dir`'s doc records; REFUSED in a prefix owned by anyone else), never a wait,
/// never through a symlink at `agents/` (refused, as atpkg's `ensure_shared_dir` and
/// `ensure_private_dir` refuse it: a pre-created link must never capture the twins), and
/// a failure is refused silently here: the front door said it, with the remedy chosen
/// for what is at `agents/` (`aterm pkg repair` re-lays the directory with the twins —
/// `activate`'s `ensure_dir` — but refuses a link or a file there, which must go by
/// hand); `aterm pkg doctor` has no row for an absent `agents/` (audit 2026-09-16).
/// What the session needs synchronously is only that the directory exist,
/// so the twins atpkg lays into it later are found on the next invocation. No
/// `ATPKG_AGENTS` is exported here (the window exports none either): the shell
/// integration sources the hook while that variable is unset, and a seam-exported value
/// would stop it from ever doing so.
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
            // Refused: said ONCE, by the front door (`atpkg::store::agents_dir_refusal_line`
            // from crates/aterm, first in this same process), with the remedy chosen for
            // what is at `agents/` — a second line here repeated it with a remedy
            // that is false for a link or a file there. Fall through to rule 3.
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

/// The mode the managed `agents/` directory is created with under a prefix whose owner is
/// `prefix_uid` with permission bits `prefix_mode`, by a process whose real uid is
/// `our_uid` — `atpkg::store::Layout::ensure_dir`'s rule restated (this crate does not
/// link atpkg): a root-owned prefix that is not group/other-writable is the SYSTEM shape
/// (`platform::dir_meta_is_system`) ⇒ `0755`, so every user can traverse it; a prefix we
/// own ⇒ `0700`, the private `$HOME` shape; any other owner ⇒ `None`, refuse — a
/// directory of ours inside someone else's prefix (a `sudo aterm` over a user's `$HOME`
/// prefix, say) is never right. Pure, so the rule is testable without a filesystem.
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

/// Ensure `dir` (`<prefix>/agents`) exists as a REAL directory, [`managed_agents_dir`]'s
/// contract: an existing directory is left exactly as it is (its mode is atpkg's to keep;
/// a same-mode `chmod` would still move `st_ctime`); a symlink or a non-directory at the
/// path is refused; otherwise one `mkdir` with [`agents_dir_mode`]'s mode, made exact
/// afterwards (the request is subject to the umask), the `AlreadyExists` race with a
/// concurrent session or atpkg pass tolerated. The `Err` is the sentence for stderr.
#[cfg(unix)]
fn ensure_agents_dir(prefix: &std::path::Path, dir: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    let existing = |md: std::fs::Metadata| -> Result<(), String> {
        if md.file_type().is_symlink() {
            Err(format!("{} is a symlink; refusing", dir.display()))
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
        Ok(md) if md.file_type().is_symlink() => {
            Err(format!("{} is a symlink; refusing", dir.display()))
        }
        Ok(md) if md.is_dir() => Ok(()),
        Ok(_) => Err(format!("{} exists and is not a directory", dir.display())),
        Err(_) => std::fs::create_dir(dir)
            .or_else(|error| if dir.is_dir() { Ok(()) } else { Err(error) })
            .map_err(|error| format!("{}: {error}", dir.display())),
    }
}

/// THE ONE `("PATH", value)` pair the session hands its shell (the pty seam's
/// `build_child_env` is key-overwrite; a second PATH pair would drop the first):
/// `reroute_dir` FIRST — MOVED to the front, every occurrence already in `inherited`
/// removed, so a nested aterm or a user PATH that already lists it later still ends up
/// with it first (the measured 2026-09-07 failure was an ORDER: `~/.cargo/bin` at position
/// 17 ahead of the managed store at 19; skip-if-present would have left it there) — then
/// `agents_dir` by the same move-to-front rule (2026-09-16, [`managed_agents_dir`]: the
/// managed `claude`/`codex` must outrank `~/.local/bin` and the brew casks wherever the
/// inherited PATH had them) — then `front_door_dir` through [`prepend_path`]
/// (skip-if-present: a second front door is harmless, a shadowed managed dir is not),
/// then the inherited PATH verbatim. `None` when nothing is injected. Mirrors
/// aterm-gui's `spawn::reroute_path_env`, in its order: reroute, agents, front door,
/// inherited.
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

/// Whether `first` (the `argv[1]` operand) should be CONSIDERED for toolchain dispatch.
/// False for: no operand, an empty token, a flag (`-…`), any front-door verb ([`Verb`]), the
/// `help` manual, and any diagnostic subcommand name — those are aterm's own surface and must
/// never be shadowed by a co-distributed tool (so a store tool named `help`/`ctl`/… can never
/// hijack `aterm`'s verbs). A `true` here means only "candidate"; whether it is actually an
/// installed tool is decided by the co-located `atpkg` (so non-tools still fall through to the
/// normal unknown-operand handling). Pure, so precedence is testable.
///
/// The verb shield reads [`Verb::from_operand`], so a verb is protected the moment it joins the
/// roster. `ship` spent its first release unshielded precisely because the old list was a
/// hand-maintained second copy of the verb names that nobody updated.
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

/// The [`aterm_sandbox::Limits`] the SESSION hands the protected spawn seam,
/// chosen by containment mode — the rule `aterm-gui` applies at ITS spawn
/// (`spawn.rs`), the one [`aterm_sandbox::Limits::inherit`]'s doc states, and the
/// one the seam's `limits` parameter comment states (`spawn_shell_with_pid_cell_px`
/// in aterm-pty's unix seam): the daily-driver modes (`user`, the default, and
/// `master`) request NOTHING, so the shell inherits the launching shell's
/// `rlimit`s unchanged; the opt-in confinement modes (`safety`, `containment`)
/// keep the hardened caps of [`aterm_sandbox::Limits::shell_default`].
///
/// The same set reaches the Windows ConPTY seam, which writes it onto the child's
/// Job Object instead (there is no `setrlimit` lane there): `inherit()` installs
/// no job caps, `shell_default()` installs 16 GiB of per-process memory, 512
/// active processes and the Job Object UI restrictions. So this rule also means a
/// `user`/`master` session's shell no longer gets the job caps the blanket
/// `shell_default()` used to install — exactly as under the window.
///
/// Through 0.87.0 the session passed `shell_default()` in EVERY mode — carried
/// over from the historical `spawn_shell` wrapper, never a decision. The unix
/// actuator sets the soft limit to `min(cap, hard)` whatever the inherited soft
/// value was (and keeps the hard ceiling), so a plain `aterm` typed into another
/// terminal forced its shell's soft `RLIMIT_NOFILE` to 8192 in BOTH directions:
/// a higher limit was lowered to it, and a lower one — macOS's launchd default is
/// 256 (`launchctl limit maxfiles`) — was raised to it; off macOS the shell also
/// got a soft 16 GiB `RLIMIT_AS`. Measured through the seam on an Intel Mac
/// (macOS 13.7): a user-mode child read 8192 both from a parent at soft 1048576
/// and from one at soft 256. Now a `user`/`master` session hands its shell the
/// launching shell's soft limits, whichever side of the cap they are on. A
/// terminal must not constrain the programs you run more than the shell that
/// started it would; the confinement modes are where a cap is asked for.
fn session_limits(mode: aterm_containment::ContainmentMode) -> aterm_sandbox::Limits {
    use aterm_containment::ContainmentMode as Cm;
    match mode {
        Cm::Master | Cm::User => aterm_sandbox::Limits::inherit(),
        // Safety / Containment — and, the enum being `#[non_exhaustive]`, any
        // mode this build does not know: fail-safe to the hardened caps.
        _ => aterm_sandbox::Limits::shell_default(),
    }
}

/// The whole transparent SESSION as a callable: containment init, the single
/// root-authority mint, the protected shell spawn, and the passthrough driver
/// loop — never returns. The ONE `aterm` binary calls this after routing
/// (verbs/tools handled there; flags via [`parse_args`]); `quiet` is the
/// parsed `-q`/`--quiet`.
pub fn session_main(quiet: bool) -> ! {
    // One-line greeting, INTERACTIVE sessions only. The passthrough is so
    // transparent that a bare `aterm` is indistinguishable from nothing — a
    // fresh prompt, no window, no banner — which reads as "it did nothing"
    // (it fooled the owner on first run). One stderr line BEFORE the shell
    // spawns fixes that without touching byte-transparency: it never enters
    // the PTY stream, and piped/scripted/CI invocations (either end not a
    // TTY) stay perfectly silent, so captured output is unchanged. It also
    // makes the help system discoverable — `aterm help` is the headline.
    // `-q`/`--quiet` silences it for humans who want the old silence.
    {
        use std::io::IsTerminal as _;
        if !quiet && std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
            eprintln!(
                "aterm {} — session started · start with `aterm help` · `-q` hides this line",
                aterm_types::version::APP_VERSION
            );
        }
    }

    let (rows, cols) = driver::host_winsize();

    // P0 — the daily-driver CLI runs the shell through the PROTECTED spawn seam,
    // closing the gap where the shipped binary ran `forkpty`/`execvp` with ZERO
    // confinement while only aterm-gui used the protected path.
    //
    // Containment mode is launcher-owned. Default `User`: no OS sandbox, the shell
    // keeps full network/credential access (byte-for-byte daily behavior) and is
    // confined only by the cap gate — its rlimits are the launching shell's own,
    // unchanged (`session_limits`). `--sandbox` (`--containment containment`)
    // opts into the macOS Seatbelt sandbox; a MALFORMED value fails CLOSED to
    // Containment (never silently disables confinement).
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

    // Ask the actuator whether the shell may spawn for this mode and, for
    // Containment on macOS, the SBPL profile the spawn must be wrapped in. A `Deny`
    // — Containment on a platform with no OS sandbox, or any future variant —
    // fails closed: no shell, and the reason on stderr.
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

    // The SINGLE `unsafe` root-authority mint in this binary (CAP-1): trusted
    // launcher, before any PTY bytes flow. Grants the spawn + sandbox capabilities
    // the protected seam requires.
    // SAFETY: trusted process entry point, reached exactly once before the spawn
    // and before any untrusted PTY input is read.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

    // THE session PATH — exactly ONE `("PATH", value)` pair, composed as the window's
    // baseline `env_add` composes it (`docs/DESIGN-toolchain-reroute-2026-09-07.md`
    // §"Reaching PATH"): the REROUTE dir first (move-to-front — a bare `cargo`/`rustc` in
    // the session is announced or signposted instead of running upstream Rust silently),
    // then the managed `agents/` (move-to-front too, 2026-09-16 — the managed
    // `claude`/`codex` ahead of the native installer's and the casks'; handed by the
    // front door as `$ATERM_AGENTS_DIR` since 2026-09-18, else derived beside the
    // reroute dir and ensured to exist, `managed_agents_dir`), then aterm's own
    // binary directory so `aterm` (and thus every `aterm <verb>`) always resolves even
    // when aterm was launched by an absolute path from a dir not on $PATH (inert for a
    // lone binary or when already on PATH), then the inherited PATH verbatim. The
    // reroute dir arrives as `$ATERM_REROUTE_DIR` from the front door, which owns the
    // store (`reroute_dir_from_env`); `--no-reroute` (its marker engaged) ⇒ no prepend
    // and no export, the same rule the stubs and the window apply.
    //
    // WHAT THIS PAIR IS IN THIS LANE (audit 2026-09-16): the PRE-RC environment of a
    // LOGIN shell (`aterm-pty` spawns `-zsh`) that carries NO aterm shell integration —
    // that is the window's ZDOTDIR/`--rcfile` seam, gated on `ATERM_CHILD` /
    // `ATERM_SESSION_ID`, and this lane sets neither (below). So the order composed here
    // holds until the user's startup files run and no further: on macOS `/etc/zprofile`'s
    // `path_helper` rebuilds PATH first (measured on this Mac, 2026-09-16: the two managed
    // dirs at positions 12–13, behind `/usr/local/bin` and `/opt/homebrew/bin`), and what
    // puts `agents/` FIRST at the prompt is the rc-sourced `~/.aterm/shell.d/00-atpkg.*`
    // hook atpkg wires into the rc — once, at shell start. There is no per-prompt
    // re-assert and no live hook re-source here; those are the window tab's
    // (`__aterm_managed_path_live`). The reroute dir is still re-exported so a NESTED
    // window or session reads the same handle; nothing here exports `ATPKG_AGENTS`, so a
    // shell that does get the integration still sources the hook the moment it lands.
    // The managed `agents/` arrives as `$ATERM_AGENTS_DIR` from the front door on EVERY
    // lane since 2026-09-18 (`managed_agents_dir`, rule 1 — the R3 closure: the reroute
    // escape no longer drops the managed `claude`/`codex`), and is re-exported the same
    // way the reroute dir is — the directory this lane front-inserted, or blank when an
    // inherited value was not taken, so "none" reads as "not set or empty" downstream.
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
        // The escape is engaged (or nothing is laid) and an ENCLOSING session's
        // directory travelled in by inheritance: blank it, so the shell
        // integration's re-assert stays inert. "No export" has to mean "not set".
        env_add.push((REROUTE_DIR_ENV.to_string(), String::new()));
    }
    if let Some(dir) = &agents_dir {
        env_add.push((AGENTS_DIR_ENV.to_string(), dir.clone()));
    } else if std::env::var_os(AGENTS_DIR_ENV).is_some() {
        env_add.push((AGENTS_DIR_ENV.to_string(), String::new()));
    }
    // Deliberately NOT setting `ATERM_CHILD` here: this lane has never carried it,
    // `net_listen`'s ROOT-ONLY nesting guard reads it, and the reroute gates on
    // nothing but its own two variables. Changing what a headless aterm launched
    // from an `aterm --session` shell counts as is a separate decision.

    // PROTECTED spawn: cap-gated, fail-closed, resource-bounded by the MODE's
    // posture (`session_limits`: User/Master request nothing — the shell inherits
    // the launching shell's rlimits, and on Windows its Job Object gets no caps;
    // Safety/Containment apply the hardened caps — `setrlimit` in the child
    // before execve on POSIX, the Job Object on Windows; the rule aterm-gui's
    // spawn applies and the unix seam's `limits` parameter comment states), and
    // OS-sandbox-wrapped when `sandbox_wrap` is `Some`. Returns the PTY master +
    // child pid. The seam itself fails closed if a demanded sandbox wrapper is
    // missing (it refuses to spawn an unsandboxed shell); the pid is what both
    // drivers reap for the exit code (the unix driver waits on exactly this pid,
    // never `waitpid(-1)`: the session process can have other children).
    let shell = aterm_pty::spawn_shell_with_pid(
        rows,
        cols,
        &spawn_cap,
        &sandbox_cap,
        &env_add, // prepend aterm's bin dir so `aterm` (and its verbs) resolves in the shell
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

    // PARENT: the platform driver owns raw mode, the passthrough loop, resize
    // forwarding to the PTY, terminal restore, and the reap — and returns the
    // shell's own exit status (non-exit → 1).
    let code = driver::run(shell, flags.verbose);
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::{
        AGENTS_DIR_ENV, CliAction, DIAG_COMMANDS, DrClass, FdaState, HOOK_AGENTS_ENV, Mark,
        PASSTHROUGH_ENV, PrivacyFacts, ProbeLabel, REROUTE_DIR_ENV, RecoveryFacts,
        VERB_BLURB_COLUMN, Verb, agents_dir_mode, decide_args, diag_report, doctor_checks,
        doctor_checks_with_origin, doctor_report, help_text, is_tool_candidate, list_fonts_report,
        list_themes_report, managed_agents_dir, prepend_path, reroute_dir_from_values,
        session_limits, session_path_env, show_face_report, verb_help_block, version_text,
    };
    // The Windows-only tests read the window's shell and the Windows paragraphs.
    #[cfg(windows)]
    use super::{driver, explain_config_report, show_config_report};

    fn decide(args: &[&str]) -> CliAction {
        decide_args(args.iter().map(|s| s.to_string()))
    }

    /// The privacy facts a test binary really produces: nothing measured, because
    /// the in-bundle fence refused the probe. The default for every doctor test
    /// that is not about the `privacy:` row itself.
    fn unmeasured() -> PrivacyFacts {
        PrivacyFacts::not_measured(ProbeLabel::RefusedOutOfBundle)
    }

    /// An empty recovery ledger: the default for every doctor test that is not
    /// about the `recovery:` row itself.
    fn no_recovery() -> RecoveryFacts {
        Some((
            Vec::new(),
            std::path::PathBuf::from("/logs/recovery-ledger.log"),
        ))
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

    /// The diagnostics no longer swallow unknown flags at exit 0 (audit D-12).
    ///
    /// `aterm show-config --json` used to print the ordinary report,
    /// byte-identical to the bare form, and exit 0 — so a script author
    /// concluded their flag was accepted when it was discarded, and could not
    /// distinguish "takes no flags" from "your flag was wrong". The dispatch
    /// comment even said so: "Subcommands that take none simply ignore it."
    /// No test pinned that behaviour, which is how it survived an audit cycle.
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

    /// `<diag> --help` DESCRIBES the subcommand instead of running it — the rule
    /// the ctl surface already follows. Before this, `-h` after `show-face`
    /// became the FAMILY operand and the report fabricated a plausible,
    /// successful, wrong answer for a font named "-h" (audit D-8).
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

    /// `show-face` keeps its one operand; a flag-shaped or surplus operand is a
    /// usage error naming the grammar, never a family lookup.
    #[test]
    fn show_face_operand_grammar_is_enforced_at_the_edge() {
        assert_eq!(
            decide(&["show-face", "Menlo"]),
            diag("show-face", Some("Menlo"))
        );
        for argv in [
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

    /// [`Verb::is_windowing`] is the roster's own answer to "does this word open
    /// a terminal", and it must be EXACTLY the set the windowing grammar parses —
    /// not a hand-kept second list. The front door's argv0-alias dispatch keys on
    /// it, so a verb that drifted out of this set would silently stop working
    /// under `aterm-gui.exe` (the name the shipped Windows shortcut and the
    /// taskbar jump list actually launch) while still working under `aterm.exe`.
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
    fn diag_subcommand_recognized_as_first_operand() {
        // `aterm show-config` → Diag, dispatched before flag parsing / shell spawn.
        assert_eq!(decide(&["show-config"]), diag("show-config", None));
        // A positional operand after the subcommand is captured as `arg`
        // (`aterm show-face Menlo`); commands that take none simply ignore it.
        assert_eq!(
            decide(&["show-face", "Menlo"]),
            diag("show-face", Some("Menlo"))
        );
    }

    #[test]
    fn diag_subcommand_only_as_first_operand_not_after_flags() {
        // A subcommand name after a flag is NOT a subcommand (git-style: first only),
        // so it falls through to the option parser and is rejected as unknown.
        assert!(matches!(
            decide(&["--no-sandbox", "show-config"]),
            CliAction::Usage(_)
        ));
    }

    /// THE CLI-DIAG gate (`cli-subcommands-advertised`): every registry command must
    /// be BOTH advertised in `--help` AND dispatchable — so a new subcommand cannot
    /// ship undocumented or unimplemented (the test fails if either regresses).
    #[test]
    fn diag_commands_advertised_and_dispatchable() {
        assert!(!DIAG_COMMANDS.is_empty(), "registry must not be empty");
        for (name, _desc) in DIAG_COMMANDS {
            // Advertised as its OWN SUBCOMMANDS line (a leading token), not merely a
            // loose substring — so a future name that is a substring of another
            // can't pass vacuously.
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
            // And it must actually be recognized by the parser.
            assert_eq!(decide(&[name]), diag(name, None));
        }
        // A non-registry name is NOT dispatchable.
        assert!(diag_report("definitely-not-a-command", None).is_none());
    }

    /// `aterm list-kitty-commands` prints the vocabulary the window's listener
    /// compiles — so the listing is COMPLETE by construction, and this pins the
    /// two things a script relies on: every trick has an English row, and every
    /// line is one `key=value` row with no banner to skip.
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
        // `--help` DESCRIBES and never runs (the per-verb help law).
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
        // parsing / shell spawn — the WORD `help`, distinct from `-h`/`--help`.
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

    /// `--help` names the shell the session RUNS: `$SHELL` on Unix, the
    /// platform default on Windows, whose passthrough never reads `$SHELL`
    /// (audit 2026-09-22: "Runs your $SHELL" there named a shell it never ran).
    /// The line sits right under the origin line, where it always sat.
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

    /// THE front-door gate. Every property that `aterm ship` violated, asserted
    /// over the WHOLE roster rather than over a list someone must remember to
    /// extend.
    ///
    /// Dispatch is deliberately NOT checked here — it cannot be. The exhaustive
    /// `match Verb` in `crates/aterm/src/main.rs` makes an unrouted verb a
    /// COMPILE error, and `crates/aterm/tests/front_door_verbs.rs` proves each
    /// one is reachable at a real terminal. This test owns the other three legs.
    #[test]
    fn front_door_verbs_are_the_one_command_surface() {
        assert!(!Verb::ALL.is_empty(), "roster must not be empty");
        let help = help_text();
        for verb in Verb::ALL {
            // ADVERTISED — as its own VERB line, a leading token rather than a
            // loose substring, so a name that happens to be a substring of
            // another cannot pass vacuously.
            assert!(
                help.lines().any(|l| l
                    .trim_start()
                    .starts_with(&format!("aterm {}", verb.name()))),
                "verb `aterm {}` is not advertised in --help",
                verb.name()
            );
            // SHIELDED — a co-distributed toolchain program of the same name can
            // never shadow a verb of the one command.
            assert!(
                !is_tool_candidate(Some(verb.name())),
                "verb {:?} can be shadowed by a store tool of the same name",
                verb.name()
            );
            // RECOGNIZED — the one recognizer round-trips, so routing and
            // shielding cannot disagree about what a verb is called.
            assert_eq!(Verb::from_operand(verb.name()), Some(*verb));
            // WELL-FORMED — a verb with no blurb would render a bare synopsis.
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

    /// The argv0 compat aliases the bundle symlinks onto the one binary. Every
    /// alias must be distinct and must not collide with a verb NAME, or argv0
    /// dispatch and operand dispatch would disagree about the same string.
    /// `argv0_alias()` had no consumer, and that is how `aterm-link` shipped in
    /// four alias lists and not the fifth (the release bundler): five copies of
    /// one set, maintained by hand, and the roster that could have pinned them
    /// read by nothing but its own distinctness test. This reads the two Rust
    /// sites that spell the set — the macOS bundler's symlink loop and the front
    /// door's `alias_route` — and asserts every alias the roster yields is in
    /// both. `aterm-release` deliberately does not depend on this crate, so the
    /// pin is a file read rather than an import; a hard-coded list checked
    /// against a hard-coded list would prove only that one author typed it twice.
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

    /// `--help` is RENDERED from the roster, so this pins the rendering itself:
    /// the alignment column and the continuation indent that make the VERBS block
    /// read as one table.
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

    /// The composed session PATH: the reroute dir FIRST — moved there from wherever the
    /// inherited PATH already listed it (a nested aterm; the 2026-09-07 shape with
    /// `~/.cargo/bin` ahead of the store), exactly once — then the front door, then the
    /// rest; idempotent when applied to its own output; and, with no reroute dir, exactly
    /// `prepend_path`'s answer.
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

    /// THE TTY SESSION'S SHELL GETS THE MANAGED `agents/` IN FRONT (2026-09-16), in the
    /// window's order — reroute, agents, front door, inherited — by the same
    /// move-to-front rule: a `~/.local/bin` (Anthropic's native `claude`) or
    /// `/opt/homebrew/bin` (the casks) listed ahead of it in the inherited PATH ends up
    /// behind it, every earlier occurrence removed, idempotently. Owner, 2026-09-16:
    /// "all the latest and best MUST WORK IN THE SAME TAB with live update!" — an
    /// `aterm` session in another terminal is a tab too.
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

    /// The agents dir this crate derives is atpkg's own: `reroute/` and `agents/` are
    /// siblings under the one manager prefix (`Layout::reroute_dir`, `Layout::agents_dir`),
    /// so `<parent of $ATERM_REROUTE_DIR>/agents` IS `Layout::agents_dir` for the same
    /// prefix — pinned against the real layout (atpkg is a test-only dependency here), so
    /// a relocation of either directory in atpkg fails this test rather than silently
    /// putting a directory that is not the store's in front of every session's PATH.
    /// And the derivation ENSURES the directory (private, `0700`), the window's own
    /// discipline, so the very first session on a fresh machine has it on PATH before
    /// atpkg lays a twin.
    #[test]
    fn the_agents_dir_is_the_reroute_dirs_sibling_in_atpkgs_layout() {
        let scratch = std::env::temp_dir().join(format!(
            "aterm-cli-agents-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
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

    /// The reroute variables and the two agents variables this crate reads are atpkg's
    /// spellings — restated in this crate because atpkg is a test-only dependency here,
    /// and pinned so the copies cannot drift apart. `ATERM_AGENTS_DIR` is
    /// `atpkg::reroute::AGENTS_DIR_ENV`, the front door's handoff (2026-09-18).
    /// `ATPKG_AGENTS` has no constant on atpkg's side; it is the name the shell hook
    /// exports, so the pin is the hook body — which READS the handoff as one of the
    /// three markers of its agents gate (03513b5d7: the managed copy leads inside aterm
    /// only, and no single marker covers every lane) and never sets it: it is a
    /// launcher→session handle. `TERM_PROGRAM` is user-settable and is not a marker.
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
        // THE REROUTE ORDER (owner ruling 2026-09-27): inside aterm the hook's gated arm
        // puts the reroute dir FIRST and agents/ second — this lane has no shell
        // integration to re-assert it past path_helper — and reads `--no-reroute`'s marker
        // by THIS crate's spelling of it, never setting it or the seam's handle. Same-day
        // review: it also READS this lane's `$ATERM_REROUTE_DIR` as a marker for the
        // reroute dir, because a launch whose front door handed no agents dir carries no
        // other marker at all (this lane sets neither ATERM_CHILD nor ATERM_SESSION_ID).
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
        // And neither must the SHELL INTEGRATION itself — every dialect this workspace
        // ships and the copies the macOS app bundles: it keys its hook-sourcing on
        // `$ATPKG_AGENTS` being unset, and a script that started reading the handoff
        // would pass every test above while the contract silently changed. A file read,
        // as `every_argv0_alias_is_bundled_and_routed` does, because those crates do not
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

    /// THE PRECEDENCE (2026-09-18, R3 closed): the front door's `$ATERM_AGENTS_DIR` —
    /// absolute, existing, a real directory — is taken FIRST, with or without a reroute
    /// handle and over an enclosing shell's `$ATPKG_AGENTS`, and nothing is created for
    /// it (the front door ensured it); a relative, empty, absent, nonexistent or
    /// symlinked value is not the front door's and falls through to the sibling
    /// derivation (ensured here), and that to `$ATPKG_AGENTS`, and that to none — so
    /// under `--no-reroute` (no reroute handle) the managed `claude`/`codex` still lead
    /// from the handoff alone.
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
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// `$ATERM_REROUTE_DIR` is taken only as a non-empty ABSOLUTE path naming an existing
    /// directory (2026-09-16 audit: a relative inherited stray put first on PATH would
    /// resolve against every later cwd, and its derived `agents/` sibling would be created
    /// in the session's cwd), and never while the `--no-reroute` marker is engaged — by
    /// `env_flag_engaged`'s reading, so `0` and the empty string do not engage it.
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

    /// `agents_dir_mode` IS `Layout::ensure_dir`'s rule (`0700` for the `$HOME` shape,
    /// `0755` for a root-owned system prefix, `platform::dir_meta_is_system`'s
    /// "root-owned and not group/other-writable"), plus the refusal `ensure_dir` never
    /// needs because atpkg vets its prefix first: a prefix owned by another user. The
    /// `sudo aterm` cases are the ones that bit (audit 2026-09-16): root over a system
    /// prefix gets `0755`, not the `0700` no other user could traverse; root over a
    /// user's `$HOME` prefix creates nothing.
    #[test]
    fn agents_dir_mode_restates_layout_ensure_dirs_rule() {
        // A user's own $HOME prefix, by that user.
        assert_eq!(agents_dir_mode(501, 0o700, 501), Some(0o700));
        assert_eq!(agents_dir_mode(501, 0o755, 501), Some(0o700));
        // A system prefix, by any user: 0755 (a non-root mkdir there fails on its own).
        assert_eq!(agents_dir_mode(0, 0o755, 0), Some(0o755));
        assert_eq!(agents_dir_mode(0, 0o755, 501), Some(0o755));
        assert_eq!(agents_dir_mode(0, 0o700, 0), Some(0o755));
        // Root-owned but group/other-writable is NOT the system shape; root owns it ⇒ 0700.
        assert_eq!(agents_dir_mode(0, 0o775, 0), Some(0o700));
        assert_eq!(agents_dir_mode(0, 0o777, 501), None);
        // Someone else's prefix: refused — root over a user's $HOME prefix included.
        assert_eq!(agents_dir_mode(501, 0o700, 0), None);
        assert_eq!(agents_dir_mode(501, 0o700, 502), None);
    }

    /// `managed_agents_dir` derives and creates NOTHING from a relative reroute handle
    /// (no `./agents` in the session's cwd, no relative entry first on PATH), refuses a
    /// symlink and a regular file at `agents/` (as atpkg's `ensure_shared_dir` /
    /// `ensure_private_dir` refuse a symlink — a pre-created link must never capture the
    /// twins), and with no derivable directory — no handle, a refused one — falls back to
    /// the enclosing shell's `$ATPKG_AGENTS` only when that is an absolute existing
    /// directory. The derived directory wins over the fallback.
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
        // A regular file at agents/: refused (silently — the front door says it), left
        // alone; the enclosing
        // shell's directory — one that shell already had first on its PATH — is still the
        // fallback, as it was before the refusal existed.
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

    /// A prefix that refuses the `mkdir` (read-only, owned by us) yields `None` — said on
    /// stderr, never a nonexistent entry first on PATH — and creates nothing. Root
    /// ignores mode bits, so the case is not observable as root and is skipped there.
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

    /// THE TTY LANE'S PATH, MEASURED AT A REAL PROMPT (audit 2026-09-16). The session
    /// hands its shell `session_path_env`'s order and spawns it as a LOGIN shell with no
    /// aterm shell integration, so what a user sees at the prompt is what the startup
    /// files leave. Two runs of `/bin/zsh -l -i -c 'print -r -- $PATH'` with a private
    /// `ZDOTDIR` and an empty `HOME`:
    ///
    /// 1. an EMPTY `.zshrc` — the pre-rc front-insert alone. Where `/etc/zprofile` runs
    ///    `path_helper` (macOS), the managed dirs come out DEMOTED behind `/etc/paths`'
    ///    list (measured here 2026-09-16: positions 12–13, behind `/usr/local/bin` and
    ///    `/opt/homebrew/bin`) — the honest shape of this lane's front-insert; both
    ///    survive, exactly once. Elsewhere only survival is asserted.
    /// 2. a `.zshrc` sourcing atpkg's REAL `00-atpkg.zsh` hook body (the crate is a
    ///    test-only dependency here), the block atpkg wires into `~/.zshrc`, measured
    ///    twice: with none of the agents gate's markers the hook DEMOTES `agents/` AND
    ///    the reroute dir — both leave the prompt's PATH (session-scoped: aterm only) —
    ///    and with `ATERM_CHILD` set the reroute dir is FIRST and `agents/` second, each
    ///    exactly once (owner ruling 2026-09-27: in aterm hosted shells a bare `cargo` is
    ///    announced; measured 2026-09-24, before this, the reroute dir sat behind
    ///    `/usr/local/bin` and `/opt/homebrew/bin` at the prompt). The managed bin/ is
    ///    unconditional (03513b5d7: the managed `claude`/`codex` lead inside aterm only,
    ///    the Trust toolchain in every terminal). The same order with THIS lane's own
    ///    pair (`$ATERM_AGENTS_DIR` + `$ATERM_REROUTE_DIR`, no `ATERM_CHILD`), added by
    ///    the same day's review.
    /// 3. the DEGRADED launch (same review): the agents handoff blank, the reroute handle
    ///    the one marker left — the reroute dir still first, `agents/` out.
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
            assert_eq!(bare[0], "/usr/local/bin", "/etc/paths leads: {bare:?}");
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
        // Inside: a window tab's marker, and THIS lane's own pair (the front door's
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
        // 3. THE DEGRADED LAUNCH (review finding 2026-09-27): the front door handed no
        // agents dir (a file or a link at `agents/`) and this lane blanked the inherited
        // handoff, so the reroute handle is the one marker left. The hook still puts the
        // reroute dir first — a bare `cargo` is still announced — and leaves agents/ out.
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
        // No family → usage error, exit 1.
        let (msg, code) = show_face_report(None);
        assert_eq!(code, 1);
        assert!(msg.contains("usage"), "{msg}");
        // An unresolvable family → not-found error, exit 1 (the positive path is
        // covered by aterm-render's face_info test against a real system font).
        let (msg, code) = show_face_report(Some("definitely-not-a-real-font-xyzzy"));
        assert_eq!(code, 1);
        assert!(msg.starts_with("ERR"), "{msg}");
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
        // /bin/sh is executable on every POSIX host; both legacy checks pass.
        let (r, code) = doctor_checks(
            Some("/bin/sh"),
            true,
            true,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert_eq!(code, 0, "{r}");
        assert!(r.contains("health: OK"), "{r}");
        // One row per check: the label, the mark, the fact (2026-09-26 — it was a
        // marks block and then every fact again). The size sits on the tty row, the
        // fd it was measured on; the version row is the version.
        assert!(
            r.lines()
                .any(|l| l == "shell:     ok    /bin/sh (executable)"),
            "{r}"
        );
        assert!(
            r.lines()
                .any(|l| l == "tty:       ok    stdout is a terminal, 80 cols x 24 rows"),
            "{r}"
        );
        assert!(r.lines().any(|l| l.starts_with("version: ")), "{r}");
        assert!(!r.contains("80x24"), "{r}");

        // Shell not executable → fail.
        let (r, code) = doctor_checks(
            Some("/no/such/shell"),
            false,
            true,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert_eq!(code, 1);
        assert!(
            r.contains("health: FAIL") && r.contains("not executable or missing"),
            "{r}"
        );

        // $SHELL unset → fail.
        let (r, code) = doctor_checks(
            None,
            false,
            true,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert_eq!(code, 1);
        assert!(r.contains("$SHELL unset"), "{r}");

        // No tty (headless/piped) → a NOTE, and the exit code does NOT move.
        // This is the invocation every script, every CI run and every
        // capture-the-output caller makes; scoring it FAIL meant `doctor` could
        // never gate anything, because reading its answer changed the answer.
        // A shell problem below still fails with no tty, so the note is not a
        // blanket.
        let (r, code) = doctor_checks(
            Some("/bin/sh"),
            true,
            false,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert_eq!(code, 0, "a piped healthy machine is healthy: {r}");
        // The fact alone — the mark column and `health: OK` carry the verdict; and no
        // size, because piped the number is the 24x80 fallback, not a measurement.
        assert!(
            r.lines()
                .any(|l| l == "tty:       note  stdout is not a terminal"),
            "{r}"
        );
        assert!(r.contains("health: OK") && !r.contains("cols"), "{r}");
        let (r, code) = doctor_checks(
            Some("/no/such/shell"),
            false,
            false,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert_eq!(code, 1, "a real failure still fails without a tty: {r}");

        // No containment row: the mode is a launch flag, not an environment
        // variable a separate `doctor` run could read.
        let (r, _) = doctor_checks(
            Some("/bin/sh"),
            true,
            true,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert!(!r.contains("containment"), "{r}");

        // Every row is a uniform `key: …` line (no stray `OK:`/`ERR:` prefix).
        let (r, _) = doctor_checks(
            Some("/bin/sh"),
            true,
            true,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        for key in ["shell: ", "tty: ", "privacy: ", "version: "] {
            assert!(
                r.lines().any(|l| l.starts_with(key)),
                "doctor detail missing a {key:?} line\n{r}"
            );
        }
    }

    /// `shell` keeps its EXACT two-valued semantics: `ok` when it passes, `FAIL`
    /// — the same spelling scripts grep for — when it does not. The mark column
    /// is still the aligned block it was.
    ///
    /// `tty` is no longer one of them. It measures `doctor`'s OWN stdout, so
    /// scoring it `FAIL` meant reading doctor's answer changed the answer: every
    /// piped, redirected or CI invocation exited 1 on a healthy machine. It is a
    /// `note` now, like `privacy`, for the reason `Mark::Note` exists — a fact
    /// that is never a reason to refuse to launch.
    #[test]
    fn the_two_valued_doctor_rows_are_unchanged_by_the_third_mark() {
        let (r, _) = doctor_checks(
            Some("/bin/sh"),
            true,
            true,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert_eq!(mark_of(&r, "shell:"), "ok", "{r}");
        assert_eq!(mark_of(&r, "tty:"), "ok", "{r}");
        let (r, _) = doctor_checks(
            Some("/no/such/shell"),
            false,
            false,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert_eq!(mark_of(&r, "shell:"), "FAIL", "{r}");
        assert_eq!(mark_of(&r, "tty:"), "note", "{r}");
    }

    /// The `shell:` row can carry WHERE the shell came from — the Windows report
    /// names the window's shell and must say so — and without a label the row
    /// is byte-identical to before, which is what keeps the Unix report fixed.
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
            Some("/bin/sh"),
            true,
            true,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        let (unlabelled, _) = doctor_checks_with_origin(
            Some("/bin/sh"),
            true,
            None,
            true,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert_eq!(plain, unlabelled, "no label, no change");

        let (labelled, code) = doctor_checks_with_origin(
            Some(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            true,
            Some("a new window's default shell, the platform default (pwsh on PATH)"),
            true,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert_eq!(code, 0);
        assert_eq!(mark_of(&labelled, "shell:"), "ok", "{labelled}");
        assert!(
            shell_row(&labelled).ends_with(
                r" C:\Program Files\PowerShell\7\pwsh.exe (executable) — a new window's default shell, the platform default (pwsh on PATH)"
            ),
            "{labelled}"
        );

        // The label rides on a FAILING row too: the reader needs to know which
        // input named the missing shell before they can fix it.
        let (failed, code) = doctor_checks_with_origin(
            Some("nosuch"),
            false,
            Some("a new window's default shell, from aterm.toml shell = \"nosuch\""),
            true,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert_eq!(code, 1);
        assert_eq!(mark_of(&failed, "shell:"), "FAIL", "{failed}");
        assert!(
            shell_row(&failed).ends_with(
                " nosuch (not executable or missing) — a new window's default shell, from aterm.toml shell = \"nosuch\""
            ),
            "{failed}"
        );
    }

    /// Windows: the live `doctor` and `show-config` report the shell a tab of
    /// the WINDOW runs — this tab's, inside one; a new window's default outside
    /// — labelled, never this process's `$SHELL`/`%COMSPEC%` (measured
    /// 2026-09-22: `cmd.exe` from inside a pwsh 7 tab, `bash.exe` from Git Bash
    /// — the shell the CLI was typed into, which the spawn never reads).
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
        // Never the CLI process's own POSIX `$SHELL` (Git Bash exports
        // `/usr/bin/bash`, a path CreateProcessW could not run).
        if let Ok(posix_shell) = std::env::var("SHELL")
            && posix_shell.starts_with('/')
        {
            assert!(!doctor.contains(&posix_shell), "{doctor}");
            assert!(!show.contains(&posix_shell), "{show}");
        }
    }

    /// Windows `explain-config` names where aterm.toml lives on THIS machine —
    /// the resolved path, from the rule the window loads by — and the two keys
    /// the page never mentioned, `shell` and `font_px`.
    #[cfg(windows)]
    #[test]
    fn windows_explain_config_names_the_config_path_shell_and_font_px() {
        let r = explain_config_report();
        let path = aterm_types::dirs::aterm_config_path()
            .expect("APPDATA or HOME is set on a test box")
            .display()
            .to_string();
        assert!(
            r.contains(&format!("\n  {path}\n")),
            "explain-config missing the resolved path {path:?}\n{r}"
        );
        for needle in [
            "aterm.toml — the window's settings file on this machine:",
            "%APPDATA%\\aterm\\aterm.toml",
            "\n  shell ",
            "\n  font_px ",
            "with --shell uses that instead",
            "with --font-px uses that instead",
            "pwsh, then powershell, then %COMSPEC%, then cmd.exe",
            // The console image's spelling: `aterm-gui` is the GUI-subsystem
            // image, which a pwsh prompt does not wait for.
            "`aterm --window --write-config`",
            "aterm help config",
            "doctor` names the program this machine resolves it to",
        ] {
            assert!(r.contains(needle), "explain-config missing {needle:?}\n{r}");
        }
        assert!(
            !r.contains("~/.config/aterm"),
            "no Unix literal for the Windows config path:\n{r}"
        );
    }

    /// §3.8's four `privacy:` states, and the property the whole three-valued
    /// change exists for: a `note` NEVER moves the exit code, so
    /// `aterm doctor && aterm` still launches on a machine that has simply not
    /// granted Full Disk Access — while a real `Fail` elsewhere still exits 1.
    #[test]
    fn the_privacy_row_is_a_note_and_never_moves_the_exit_code() {
        let ok = |privacy: &PrivacyFacts| {
            doctor_checks(
                Some("/bin/sh"),
                true,
                true,
                24,
                80,
                privacy,
                &no_census(),
                &no_recovery(),
            )
        };

        // 1. granted, on a build whose identity is stable → the one `ok` arm.
        let (r, code) = ok(&measured(FdaState::Granted, DrClass::Identity, "aterm"));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "privacy:"), "ok", "{r}");
        assert!(
            r.lines()
                .any(|l| l == "privacy:   ok    full disk access for the responsible app (aterm)"),
            "{r}"
        );
        // The grant is described by what it IS, never as the end of every dialog
        // (which services it covers is unmeasured — design §7 S4).
        assert!(
            !r.contains("no more prompts") && !r.contains("removes all"),
            "{r}"
        );

        // 2. denied → a note naming the RESPONSIBLE app, not aterm.
        let (r, code) = ok(&measured(FdaState::Denied, DrClass::Identity, "iTerm2"));
        assert_eq!(code, 0, "a missing grant is not a doctor failure\n{r}");
        assert!(r.contains("health: OK"), "{r}");
        assert_eq!(mark_of(&r, "privacy:"), "note", "{r}");
        assert!(
            r.contains("no full disk access for the responsible app (iTerm2)")
                && r.contains("can be interrupted by macOS consent dialogs"),
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

        // 4. out of the bundle: not measured, and it says WHY.
        let (r, code) = ok(&unmeasured());
        assert_eq!(code, 0, "{r}");
        assert!(
            r.lines()
                .any(|l| l == "privacy:   note  not measured (running outside the app bundle)"),
            "{r}"
        );

        // And a genuine Fail still fails, with the note alongside it.
        let (r, code) = doctor_checks(
            None,
            false,
            true,
            24,
            80,
            &unmeasured(),
            &no_census(),
            &no_recovery(),
        );
        assert_eq!(code, 1, "{r}");
        assert!(r.contains("health: FAIL"), "{r}");
        assert_eq!(mark_of(&r, "privacy:"), "note", "{r}");
    }

    /// The `recovery:` row: an empty ledger is OK and says where the census will
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
                Some("/bin/sh"),
                true,
                true,
                24,
                80,
                &unmeasured(),
                &no_census(),
                &recovery,
            )
        };
        let (r, code) = check(no_recovery());
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "recovery:"), "ok", "{r}");
        assert!(r.contains("no windowed launch of the installed app"), "{r}");

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

    /// The `instances:` row, every state, and the property it shares with
    /// `privacy:`: it is a NOTE, so an unreachable instance never moves the exit
    /// code — and it names each one for the person to decide about, never killed.
    #[test]
    fn the_instances_row_names_what_nothing_reaches_and_never_fails() {
        use aterm_ctl::census::{Census, Unreached};
        let check = |census: Result<Census, String>| {
            doctor_checks(
                Some("/bin/sh"),
                true,
                true,
                24,
                80,
                &unmeasured(),
                &census,
                &no_recovery(),
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
                    == "instances: ok    no aterm window or headless instance is running as you"),
            "{r}"
        );

        let (r, _) = check(census(2, Vec::new()));
        assert!(
            r.contains(
                "instances: ok    2 aterm windows and headless instances, each published in /run/aterm"
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
                "instances: note  2 of 3 aterm instances are reachable by no control socket in /run/aterm"
            ),
            "{r}"
        );
        assert!(r.contains("never stopped"), "{r}");
        assert!(
            r.lines()
                .any(|l| l == "                 pid 67548 (up 11d 22h): ./aterm-before --headless"),
            "{r}"
        );
        assert!(
            r.lines()
                .any(|l| l.starts_with("                 pid 68000")
                    && l.ends_with("started with its control socket off")),
            "{r}"
        );

        let (r, code) = check(Err("the process table could not be read".to_string()));
        assert_eq!(code, 0, "{r}");
        assert!(
            r.contains("instances: note  not measured (the process table could not be read)"),
            "a census that was not taken never reads as none\n{r}"
        );
    }

    /// An unclassified requirement asserts NOTHING. `DrClass::Unknown` is the
    /// default the gatherer falls back to whenever `codesign` could not be read,
    /// and `grant_stable()` is false for it — so keying the dev-build sentence on
    /// "not stable" would have printed it on every machine where the probe simply
    /// did not run.
    #[test]
    fn an_unclassified_requirement_never_claims_a_dev_build() {
        let (r, _) = doctor_checks(
            Some("/bin/sh"),
            true,
            true,
            24,
            80,
            &measured(FdaState::Denied, DrClass::Unknown, "Terminal"),
            &no_census(),
            &no_recovery(),
        );
        assert!(!r.contains("dev build"), "{r}");
    }

    /// THE FENCE, end to end, from the binary that is most at risk of tripping
    /// it. `doctor_report` is dispatchable (`aterm doctor`) and really is called
    /// from this test binary by `diag_commands_advertised_and_dispatchable` — a
    /// path under `target/debug/deps`, which is exactly the shape that made
    /// `tccd` walk a million-entry directory on 2026-08-17. It must report `not
    /// measured` with no syscall behind it.
    #[test]
    fn doctor_run_from_a_test_binary_measures_nothing() {
        let (r, _) = doctor_report();
        assert!(
            r.contains("privacy:   note  not measured ("),
            "a test binary must never report a measured consent state\n{r}"
        );
        assert!(
            !r.contains("full disk access for the responsible app"),
            "the fence let a verdict through\n{r}"
        );
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
        // Every front-door verb and the `help` manual stay aterm's own — a store tool of the
        // same name can never hijack `aterm ctl`/`aterm ship`/etc. Driven off the roster, so a
        // verb added tomorrow is covered here without anyone remembering to extend this list.
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
        // Bare, tool-shaped operands ARE candidates. Whether they are actually installed is
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

    /// `--version` keeps its identity line first (install.sh greps `^aterm `), then
    /// the origin line (who makes it), and then says which copy runs — the
    /// updater's own S12 lines, verbatim.
    #[test]
    fn version_text_names_the_running_copy_and_any_other() {
        use aterm_update::which_copy::{OtherCopy, Running, WhichCopy};
        let identity = format!(
            "aterm {}\n{}\n",
            aterm_types::version::APP_VERSION,
            aterm_types::identity::ORIGIN_LINE
        );
        assert!(identity.contains("by Andrew Yates") && identity.contains("alab.systems"));
        let anchors = super::trust_anchors_line();
        assert!(
            anchors.starts_with("trusts: master=") && !anchors.contains("channel="),
            "{anchors}"
        );
        // The build line follows the identity once the launcher publishes it
        // (2026-09-14): the start probe greps the number out of this text. Set
        // here for the whole binary — the other assertions carry it too.
        super::set_running_build(1789432052);
        super::set_running_build(7);
        let build = "build: 1789432052\n";
        assert_eq!(version_text(None), format!("{identity}{build}{anchors}"));
        assert!(
            version_text(None).contains("1789432052"),
            "the first publication stands, and the probe finds it"
        );
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
        // `--containment=` → empty string, which fails CLOSED in the init funnel
        // exactly like an invalid mode; it must NOT be silently dropped.
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
        // The parser does NOT validate the mode; it hands the garbage to the single
        // init funnel, which fails CLOSED. So decide_args still returns Run(Some(..)).
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
        // Flags BEFORE `--` still apply.
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
        // aterm accepts no positional operands; one after `--` is still a usage error
        // (so a stray path can't be silently swallowed), and a `-`-prefixed token
        // after `--` is treated as that same operand, NOT re-parsed as a flag.
        match decide(&["--", "extra"]) {
            CliAction::Usage(m) => assert!(m.contains("unknown command extra"), "{m}"),
            other => panic!("expected Usage, got {other:?}"),
        }
        match decide(&["--", "--help"]) {
            CliAction::Usage(m) => assert!(m.contains("unknown option --help"), "{m}"),
            other => panic!("expected Usage (post-`--` is an operand), got {other:?}"),
        }
    }

    /// THE RULE the session now shares with the window (`aterm-gui`'s spawn) and
    /// with the seam's own `limits` contract: the daily-driver modes inherit, the
    /// opt-in confinement modes cap. Pinned as a table so the posture per mode is
    /// readable without a source dive, and so the regression — the session
    /// passing `shell_default()` in EVERY mode, which forced a User-mode shell's
    /// soft RLIMIT_NOFILE to 8192 whether the launching shell's was higher or
    /// lower — cannot come back quietly. The numbers `aterm help aterm`, the man
    /// page and the CHANGELOG quote are pinned to what the sandbox crate installs,
    /// and `aterm help aterm` must quote them (`aterm --help` names no numbers).
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
        // Every number the manual, man page and CHANGELOG quote, pinned to what the
        // sandbox crate installs: open files 8192 (POSIX), 512 active processes +
        // UI restrictions (Windows), 16 GiB of address space / job memory off macOS.
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

    /// END-TO-END through the real seam, with the session's own choice: the
    /// child of a User-mode session reads back the SAME soft `RLIMIT_NOFILE`
    /// this process has, and the child of a Safety-mode session reads back the
    /// hardened cap (clamped to the inherited hard ceiling, which the actuator
    /// never lowers). Measured with `ulimit -n` in the spawned `/bin/sh` — the
    /// number a program in the session sees, not a struct compared with itself.
    /// The first assertion discriminates whenever this process's soft limit is
    /// on either side of the cap: run from a zsh at soft 1048576 the pre-fix
    /// session lowered its child to 8192, and run under `ulimit -Sn 256` (macOS's
    /// launchd default) it raised its child to 8192. Where the parent's soft
    /// limit equals the cap the assertion still holds, it is only not
    /// discriminating.
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
