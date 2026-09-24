// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! LIVE UPGRADE OF A RUNNING CLAUDE CODE SESSION (owner ask, 2026-09-23: "I want
//! the HARNESS to handle upgrading the live claude session. if that means some
//! nice way of restarting, ok." — and: "we'd want to not kill the background
//! processes. we'd want to wait for them, maybe give a prompt to claude that
//! we're going to prepare a restart to upgrade and prep the AI to get to a good
//! stopping point").
//!
//! A running Claude Code cannot have its binary replaced: the kernel mapped the
//! old image at exec, and Claude's own `/update` relaunch ("Switch to the latest
//! version (conversation continues)", alias `/restart`) is compiled into 2.1.278
//! through 2.1.281 but switched off. So the least disruptive live form is the
//! one this module plans: a COOPERATIVE, IN-PLACE resume relaunch —
//!
//! 1. **Announce** — at an idle point with an empty composer, type ONE turn
//!    telling the agent an upgrade restart is coming, asking it to let its
//!    background work finish (never cancel it), save its work, and answer with a
//!    one-time READY marker ([`prepare_prompt`]).
//! 2. **Drain** — wait, with no deadline, until the agent has answered with the
//!    marker ([`transcript_has_ready`]), Claude's own session file says `idle`,
//!    nothing runs under the agent (no shell, no `caffeinate`), and the composer
//!    is still empty ([`gate_restart`]). Nothing is ever killed to get there.
//! 3. **Restart** — end the agent with SIGTERM (Claude runs its graceful
//!    shutdown; no keystroke is typed into the TUI, so no draft can be
//!    concatenated), wait for the tab's own shell to hold the terminal again,
//!    and type ONE line ([`relaunch_line`]) that re-runs the NEWER build with the
//!    original flags ([`rewrite_argv`]) and `--resume <sessionId>`, healing a
//!    stale shell's PATH in the same line when the tab needs it.
//! 4. **Continue** — once the new process has re-registered the SAME session,
//!    type one turn telling the agent it was upgraded and to carry on
//!    ([`continue_prompt`]).
//!
//! Everything here is PURE: parsers, the target rule, the argv rewrite, the
//! line, the prompts and the two gates, each over facts a driver measured. The
//! driver (`cli::run_upgrade`) owns the I/O; the facts it feeds in are named at
//! each function. The session file `~/.claude/sessions/<pid>.json` is Claude
//! Code's own record (measured 2.1.278-2.1.281: `pid`, `sessionId`, `cwd`,
//! `version`, `status` busy|shell|idle|waiting, `statusUpdatedAt` ms, `procStart`,
//! `kind`, `entrypoint`); it is a third party's file, so every read refuses on an
//! unexpected shape rather than guessing.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use aterm_json::Value;

/// A dotted-digits version (`2.1.281`), compared numerically part by part, a
/// missing part reading as 0 — so `2.1` EQUALS `2.1.0`, and equality agrees with
/// the ordering (a derived `PartialEq` over the parts would not).
#[derive(Clone, Debug)]
pub struct Version(Vec<u64>);

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl Version {
    /// Parse `2.1.281` (a leading `v` tolerated). `None` for anything that is not
    /// one or more dot-separated runs of ASCII digits — a pre-release suffix is
    /// refused rather than ordered, because the rule "never downgrade" must not
    /// rest on a guess about how a vendor orders its suffixes.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim();
        let t = t.strip_prefix('v').unwrap_or(t);
        if t.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        for p in t.split('.') {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.len() > 12 {
                return None;
            }
            parts.push(p.parse().ok()?);
        }
        Some(Self(parts))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.0.len().max(other.0.len());
        for i in 0..n {
            let a = self.0.get(i).copied().unwrap_or(0);
            let b = other.0.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        Ordering::Equal
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for p in &self.0 {
            if !first {
                f.write_char('.')?;
            }
            write!(f, "{p}")?;
            first = false;
        }
        Ok(())
    }
}

/// Claude Code's own record of one running interactive process,
/// `<config>/sessions/<pid>.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFile {
    /// The process id the record describes.
    pub pid: u32,
    /// The conversation this process holds now (Claude rewrites it when the
    /// session changes, so it is re-read at every decision).
    pub session_id: String,
    /// The process's working directory as Claude last recorded it.
    pub cwd: String,
    /// The running build's version, as the running build reports itself.
    pub version: String,
    /// `busy`, `shell` (a background shell runs), `idle` or `waiting` (a
    /// dialog or permission prompt is up).
    pub status: String,
    /// When `status` last changed, unix milliseconds.
    pub status_updated_at_ms: u64,
    /// The process start time as Claude renders it (`ps -o lstart=` in UTC,
    /// C locale): the pid-reuse guard.
    pub proc_start: String,
    /// `interactive` for a TUI session.
    pub kind: String,
    /// `cli` for a session started from a shell.
    pub entrypoint: String,
}

/// Parse a session file, refusing any shape this module was not written for.
///
/// # Errors
/// The text is not a JSON object, or a required field is missing or of the
/// wrong type — named, so a schema change in a new Claude reads as exactly
/// that rather than as an idle session.
pub fn parse_session_file(text: &str) -> Result<SessionFile, String> {
    let v: Value = aterm_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let obj = v.as_object().ok_or("not a JSON object")?;
    let s = |k: &str| -> Result<String, String> {
        obj.get(k)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("no string `{k}`"))
    };
    let n = |k: &str| -> Result<u64, String> {
        obj.get(k)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("no number `{k}`"))
    };
    let pid = u32::try_from(n("pid")?).map_err(|_| "pid out of range".to_string())?;
    Ok(SessionFile {
        pid,
        session_id: s("sessionId")?,
        cwd: s("cwd")?,
        version: s("version")?,
        status: s("status")?,
        status_updated_at_ms: n("statusUpdatedAt")?,
        proc_start: s("procStart")?,
        kind: s("kind")?,
        entrypoint: s("entrypoint")?,
    })
}

/// Whether a session id is safe to put on a command line: Claude's ids are
/// UUIDs, so nothing but hex and dashes, 8 to 64 bytes.
#[must_use]
pub fn is_session_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Where a relaunch target comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// aterm's managed build (the agents twin's store build).
    Managed,
    /// The vendor's own installer's copy (Claude's native `~/.local` install,
    /// which its updater moves — the source of `Update installed · Restart to
    /// update`).
    Native,
}

impl Source {
    /// The word the ledger and the notice print.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Managed => "managed",
            Source::Native => "native",
        }
    }
}

/// A build a session could be relaunched on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// What to execute: an absolute path.
    pub exe: PathBuf,
    /// Its version, as it reports itself.
    pub version: Version,
    /// Where it comes from.
    pub source: Source,
}

/// THE TARGET RULE: the newest candidate strictly newer than what runs, and on a
/// version tie the MANAGED one (inside aterm the managed copy leads — owner
/// decision 2026-09-10 — but never at the price of going backwards). `None`
/// when nothing is newer: a session is never moved sideways or down, so a native
/// 2.1.281 session is left alone while the managed copy is 2.1.280, and moved
/// onto managed the day managed reaches 2.1.282.
#[must_use]
pub fn choose_target(running: &Version, candidates: &[Candidate]) -> Option<Candidate> {
    let mut best: Option<&Candidate> = None;
    for c in candidates.iter().filter(|c| c.version > *running) {
        best = match best {
            None => Some(c),
            Some(b) => match c.version.cmp(&b.version) {
                Ordering::Greater => Some(c),
                Ordering::Equal if c.source == Source::Managed => Some(c),
                _ => Some(b),
            },
        };
    }
    best.cloned()
}

/// Why a flag list cannot be carried into a resume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgvRefusal {
    /// A flag this table does not know: fail closed rather than guess whether it
    /// takes a value.
    UnknownFlag(String),
    /// A flag whose session cannot be resumed as a TUI in place (`--print`,
    /// `--bg`, `--tmux`, `--worktree`, `--no-session-persistence`, …).
    NotResumable(String),
}

impl std::fmt::Display for ArgvRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgvRefusal::UnknownFlag(fl) => write!(f, "unknown-flag {fl}"),
            ArgvRefusal::NotResumable(fl) => write!(f, "not-resumable {fl}"),
        }
    }
}

/// How a Claude flag takes its value (from `claude --help`, 2.1.281), consumed
/// exactly as Claude's own parser consumes it (see [`maybe_option`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arity {
    /// A switch.
    None,
    /// Exactly one value (`<value>`): the next token, whatever it looks like.
    One,
    /// A REQUIRED list (`<values...>`): the next token whatever it looks like,
    /// then every further token until one is an option. An OPTIONAL list
    /// (`[values...]`) is not this — none exists in 2.1.281, and one added later
    /// needs its own arm, not this one.
    Many,
    /// An optional value (`[value]`): the next token if it is not an option (a
    /// lone `-` is a value).
    Optional,
}

/// What the rewrite does with a flag.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Carried verbatim.
    Keep,
    /// Dropped with its value — the resume family, replaced by `--resume <id>`.
    Drop,
    /// The whole relaunch is refused.
    Refuse,
}

/// Claude Code 2.1.281's options. A flag missing from here refuses the rewrite
/// (fail closed): a new version's flag is added here, on purpose, when it is
/// known to be safe to carry.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("--add-dir", Arity::Many, Fate::Keep),
    ("--agent", Arity::One, Fate::Keep),
    ("--agents", Arity::One, Fate::Keep),
    (
        "--allow-dangerously-skip-permissions",
        Arity::None,
        Fate::Keep,
    ),
    ("--allowedTools", Arity::Many, Fate::Keep),
    ("--allowed-tools", Arity::Many, Fate::Keep),
    ("--append-system-prompt", Arity::One, Fate::Keep),
    ("--append-system-prompt-file", Arity::One, Fate::Keep),
    ("--autocompact", Arity::One, Fate::Keep),
    ("--ax-screen-reader", Arity::None, Fate::Keep),
    ("--bg", Arity::None, Fate::Refuse),
    ("--background", Arity::None, Fate::Refuse),
    ("--bare", Arity::None, Fate::Keep),
    ("--betas", Arity::Many, Fate::Keep),
    ("--brief", Arity::None, Fate::Keep),
    ("--chrome", Arity::None, Fate::Keep),
    ("--cloud", Arity::Optional, Fate::Refuse),
    ("-c", Arity::None, Fate::Drop),
    ("--continue", Arity::None, Fate::Drop),
    ("--dangerously-skip-permissions", Arity::None, Fate::Keep),
    ("-d", Arity::Optional, Fate::Keep),
    ("--debug", Arity::Optional, Fate::Keep),
    ("--debug-file", Arity::One, Fate::Keep),
    ("--disable-slash-commands", Arity::None, Fate::Keep),
    ("--disallowedTools", Arity::Many, Fate::Keep),
    ("--disallowed-tools", Arity::Many, Fate::Keep),
    ("--effort", Arity::One, Fate::Keep),
    ("--environment", Arity::One, Fate::Refuse),
    (
        "--exclude-dynamic-system-prompt-sections",
        Arity::None,
        Fate::Keep,
    ),
    ("--fallback-model", Arity::One, Fate::Keep),
    ("--file", Arity::Many, Fate::Refuse),
    ("--fork-session", Arity::None, Fate::Drop),
    ("--forward-subagent-text", Arity::None, Fate::Keep),
    ("--from-pr", Arity::Optional, Fate::Drop),
    ("-h", Arity::None, Fate::Refuse),
    ("--help", Arity::None, Fate::Refuse),
    ("--ide", Arity::None, Fate::Keep),
    ("--include-hook-events", Arity::None, Fate::Keep),
    ("--include-partial-messages", Arity::None, Fate::Keep),
    ("--input-format", Arity::One, Fate::Refuse),
    ("--json-schema", Arity::One, Fate::Refuse),
    ("--max-budget-usd", Arity::One, Fate::Keep),
    ("--mcp-config", Arity::Many, Fate::Keep),
    ("--model", Arity::One, Fate::Keep),
    ("-n", Arity::One, Fate::Keep),
    ("--name", Arity::One, Fate::Keep),
    ("--no-chrome", Arity::None, Fate::Keep),
    ("--no-session-persistence", Arity::None, Fate::Refuse),
    ("--output-format", Arity::One, Fate::Refuse),
    ("--permission-mode", Arity::One, Fate::Keep),
    ("--permission-prompts", Arity::One, Fate::Keep),
    ("--plugin-dir", Arity::One, Fate::Keep),
    ("--plugin-url", Arity::One, Fate::Keep),
    ("-p", Arity::None, Fate::Refuse),
    ("--print", Arity::None, Fate::Refuse),
    ("--prompt-suggestions", Arity::Optional, Fate::Keep),
    ("--remote-control", Arity::Optional, Fate::Refuse),
    (
        "--remote-control-session-name-prefix",
        Arity::One,
        Fate::Refuse,
    ),
    ("--replay-user-messages", Arity::None, Fate::Refuse),
    ("--restricted", Arity::None, Fate::Keep),
    ("-r", Arity::Optional, Fate::Drop),
    ("--resume", Arity::Optional, Fate::Drop),
    ("--safe-mode", Arity::None, Fate::Keep),
    ("--session-id", Arity::One, Fate::Drop),
    ("--setting-sources", Arity::One, Fate::Keep),
    ("--settings", Arity::One, Fate::Keep),
    ("--strict-mcp-config", Arity::None, Fate::Keep),
    ("--system-prompt", Arity::One, Fate::Keep),
    ("--system-prompt-file", Arity::One, Fate::Keep),
    ("--system-prompt-snapshot", Arity::One, Fate::Keep),
    ("--teleport", Arity::Optional, Fate::Refuse),
    ("--tmux", Arity::Optional, Fate::Refuse),
    ("--tools", Arity::Many, Fate::Keep),
    ("--verbose", Arity::None, Fate::Keep),
    ("-v", Arity::None, Fate::Refuse),
    ("--version", Arity::None, Fate::Refuse),
    ("-w", Arity::Optional, Fate::Refuse),
    ("--worktree", Arity::Optional, Fate::Refuse),
];

fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

/// Whether Claude's own parser reads `tok` as an option. The Commander bundled
/// in Claude Code 2.1.281 asks `o.length>1&&o[0]==="-"` (its `parseOptions`,
/// read out of the shipped binary), so a lone `-` is a VALUE or a positional,
/// never a flag. Every "does a flag start here" question [`rewrite_argv`] asks
/// is this one, so the rewrite consumes exactly the tokens Claude consumed — a
/// flag left bare by a rewrite that consumed less would swallow the appended
/// `--resume <id>` as its own value, and the relaunch would not resume at all.
fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// THE ARGV REWRITE: the original argv (argv[0] included, which is dropped)
/// becomes the flags a resume carries — every known flag and its values kept
/// verbatim, the resume family (`--resume`/`-r [id]`, `--continue`/`-c`,
/// `--session-id <v>`, `--fork-session`, `--from-pr [v]`) dropped, then
/// `--resume <session_id>` appended.
///
/// A POSITIONAL word (the prompt a session was started with, `claude "fix the
/// bug"`) is DROPPED, not refused: the conversation already holds it, and
/// sending it again on resume would ask the same thing twice. `--` ends option
/// parsing, so everything after it is positional and dropped the same way —
/// unless it is a flag's value (`--add-dir --`), which Claude's parser takes
/// whatever it looks like, and so does this one.
///
/// # Errors
/// An unknown flag ([`ArgvRefusal::UnknownFlag`]) or one whose session is not a
/// TUI resumable in place ([`ArgvRefusal::NotResumable`]).
pub fn rewrite_argv(argv: &[String], session_id: &str) -> Result<Vec<String>, ArgvRefusal> {
    let mut out: Vec<String> = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let tok = &argv[i];
        if tok == "--" {
            break;
        }
        if !maybe_option(tok) {
            // A positional: the first prompt. Dropped (see the doc).
            i += 1;
            continue;
        }
        let (name, inline) = match tok.split_once('=') {
            Some((n, _)) if n.starts_with("--") => (n, true),
            _ => (tok.as_str(), false),
        };
        let (arity, fate) = flag(name).ok_or_else(|| ArgvRefusal::UnknownFlag(name.to_string()))?;
        if fate == Fate::Refuse {
            return Err(ArgvRefusal::NotResumable(name.to_string()));
        }
        // The token and its values, as the CLI parsed them.
        let start = i;
        i += 1;
        if !inline {
            match arity {
                Arity::None => {}
                Arity::One => {
                    if i < argv.len() {
                        i += 1;
                    }
                }
                Arity::Optional => {
                    if i < argv.len() && !maybe_option(&argv[i]) {
                        i += 1;
                    }
                }
                Arity::Many => {
                    // The first value is REQUIRED, so Claude's parser takes it
                    // whatever it looks like (`--add-dir -`, `--add-dir -c`);
                    // stopping at it would leave the flag bare.
                    if i < argv.len() {
                        i += 1;
                    }
                    while i < argv.len() && !maybe_option(&argv[i]) {
                        i += 1;
                    }
                }
            }
        }
        if fate == Fate::Keep {
            out.extend(argv[start..i].iter().cloned());
        }
    }
    out.push("--resume".to_string());
    out.push(session_id.to_string());
    Ok(out)
}

/// The tab's shell, as its executable names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// zsh.
    Zsh,
    /// bash.
    Bash,
    /// fish.
    Fish,
}

impl Dialect {
    /// From the shell's executable basename (a login shell's `-zsh` included).
    /// `None` for any other shell: the relaunch line is written for these three
    /// and no other.
    #[must_use]
    pub fn from_exe_name(name: &str) -> Option<Self> {
        let base = name.rsplit('/').next().unwrap_or(name);
        match base.trim_start_matches('-') {
            "zsh" => Some(Dialect::Zsh),
            "bash" => Some(Dialect::Bash),
            "fish" => Some(Dialect::Fish),
            _ => None,
        }
    }

    /// The atpkg shell hook for this dialect's extension.
    #[must_use]
    pub fn hook_ext(self) -> &'static str {
        match self {
            Dialect::Zsh => "zsh",
            Dialect::Bash => "bash",
            Dialect::Fish => "fish",
        }
    }
}

/// Quote one word for `dialect`. POSIX shells: single quotes with `'\''`. fish:
/// single quotes, where `\\` and `\'` are the only escapes.
#[must_use]
pub fn quote(dialect: Dialect, word: &str) -> String {
    match dialect {
        Dialect::Zsh | Dialect::Bash => format!("'{}'", word.replace('\'', r"'\''")),
        Dialect::Fish => format!("'{}'", word.replace('\\', r"\\").replace('\'', r"\'")),
    }
}

/// The longest relaunch line this module writes on the kernel it runs on
/// (`LineDiscipline::HOST`). A longer one is refused, never truncated: a cut
/// line would run something no one planned.
///
/// The bound is the TERMINAL's, not ours. The line is typed once the shell holds
/// the terminal again, and that can be before its line editor has put the tty in
/// raw mode: prompt work the shell does ITSELF (a `$(git …)` or `$(starship
/// prompt)` substitution, `vcs_info`, a loop) runs with the terminal already
/// back and the tty still canonical. Typing that lands in that window goes to
/// the kernel's line buffer, which keeps a bounded number of bytes of one line
/// (`LineDiscipline::line_keeps`) and loses the rest. `--resume <id>` is the
/// END of the line, so any cut loses it: measured 2026-09-23 (Darwin 25.6, zsh
/// 5.9) with the Enter in that window, a 1023-byte line ran and a 1024-byte one
/// did not, a 1213-byte relaunch line left the shell at `quote>`, and one cut on
/// a word boundary ran claude WITHOUT `--resume` — a new conversation. (An
/// external command in a `precmd` holds the terminal itself, and the driver's
/// wait for the shell to hold it again already covers that case.)
///
/// So the line, a bracketed-paste frame around it (`PASTE_FRAME`) and the
/// Enter fit in one canonical line of each kernel, with room to spare; the
/// checks below hold that for both at compile time. The bound is per kernel
/// because the kernels differ fourfold, and one bound for both would refuse on
/// Linux the lines it keeps whole. The driver builds the line before it types
/// the upgrade notice, and again before its one signal, so a launch whose line
/// would not fit is refused before the agent is asked to wind down, and a
/// refusal ends nothing.
pub const MAX_LINE: usize = LineDiscipline::HOST.max_line();

/// The kernel line discipline a relaunch line is typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineDiscipline {
    /// macOS — and any kernel not named here, which gets the smaller bound.
    Darwin,
    /// Linux `n_tty`.
    Linux,
}

impl LineDiscipline {
    /// The kernel this build runs on.
    const HOST: Self = if cfg!(target_os = "linux") {
        Self::Linux
    } else {
        Self::Darwin
    };

    /// The most bytes of one canonical line the kernel keeps, the Enter
    /// included. macOS: `MAX_INPUT` (`getconf MAX_INPUT /dev/tty` = 1024; the
    /// rest is DROPPED, Enter included, so a cut line never runs — measured
    /// above). Linux: `N_TTY_BUF_SIZE`, 4095 bytes and the newline (termios(3);
    /// the rest is discarded but the newline still arrives, so a cut line RUNS).
    const fn line_keeps(self) -> usize {
        match self {
            Self::Darwin => 1024,
            Self::Linux => 4096,
        }
    }

    /// The longest relaunch line written for this kernel.
    const fn max_line(self) -> usize {
        match self {
            Self::Darwin => 1000,
            Self::Linux => 4000,
        }
    }
}

/// `ESC [ 200 ~` and `ESC [ 201 ~`, the frame a paste may be wrapped in.
const PASTE_FRAME: usize = 12;

// The line and its frame, strictly under each kernel's limit: the Enter is the
// last byte kept.
const _: () =
    assert!(LineDiscipline::Darwin.max_line() + PASTE_FRAME < LineDiscipline::Darwin.line_keeps());
const _: () =
    assert!(LineDiscipline::Linux.max_line() + PASTE_FRAME < LineDiscipline::Linux.line_keeps());

/// THE RELAUNCH LINE the tab's shell runs, prefixed with one space (kept out of
/// history where the shell honours that). `heal` is the atpkg hook to source
/// first when the tab's shell predates aterm's managed `agents/` directory (a
/// frozen PATH: the heal and the relaunch are one line, so the shell is right for
/// every later command too); `cd` is the agent's own launch directory when it
/// differs from the shell's (Claude files a conversation under its launch cwd,
/// so `--resume` must run there), entered in a subshell so the person's shell
/// keeps its own directory.
///
/// # Errors
/// A line past [`MAX_LINE`], a control character in any word, or a `cd` for fish
/// (no subshell form is written for it).
pub fn relaunch_line(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    relaunch_line_on(LineDiscipline::HOST, dialect, heal, cd, exe, args)
}

/// [`relaunch_line`] for the kernel `tty`, whose bound it keeps.
fn relaunch_line_on(
    tty: LineDiscipline,
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    let exe_s = exe.to_string_lossy();
    for w in std::iter::once(exe_s.as_ref())
        .chain(args.iter().map(String::as_str))
        .chain(cd)
    {
        if w.chars().any(char::is_control) {
            return Err("a control character in the line".to_string());
        }
    }
    let mut line = String::from(" ");
    if let Some(hook) = heal {
        let h = quote(dialect, &hook.to_string_lossy());
        match dialect {
            Dialect::Zsh => {
                let _ = write!(line, ". {h}; rehash; ");
            }
            Dialect::Bash => {
                let _ = write!(line, ". {h}; hash -r; ");
            }
            Dialect::Fish => {
                let _ = write!(line, "source {h}; ");
            }
        }
    }
    let mut cmd = quote(dialect, &exe_s);
    for a in args {
        cmd.push(' ');
        cmd.push_str(&quote(dialect, a));
    }
    match cd {
        None => line.push_str(&cmd),
        Some(dir) => match dialect {
            Dialect::Zsh | Dialect::Bash => {
                let _ = write!(line, "( cd -- {} && exec {cmd} )", quote(dialect, dir));
            }
            Dialect::Fish => {
                return Err("fish: the agent's directory differs from the shell's".to_string());
            }
        },
    }
    let max = tty.max_line();
    if line.len() > max {
        return Err(format!(
            "the line would be {} bytes (max {max})",
            line.len()
        ));
    }
    Ok(line)
}

/// The one-time READY marker for one upgrade: fixed words plus a short id the
/// agent cannot have seen before this announcement, so an assistant message
/// carrying it can only be the answer to it.
#[must_use]
pub fn ready_marker(session_id: &str, to: &Version, salt: u64) -> String {
    // FNV-1a over the inputs: a label, not a secret.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in session_id
        .bytes()
        .chain(to.to_string().bytes())
        .chain(salt.to_le_bytes())
    {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{READY_PREFIX}{:08x}", h & 0xffff_ffff)
}

/// The fixed words every READY marker opens with ([`ready_marker`]).
pub const READY_PREFIX: &str = "ATERM-UPGRADE-READY-";

/// How [`prepare_prompt`]'s announcement opens: the tag and the first words
/// that set it apart from [`continue_prompt`]'s (`[aterm harness] Upgraded:`).
/// The supervisor's turn-end policy types nothing at a point that answers an
/// announcement: the sweep owns that session until it restarts it.
pub const ANNOUNCE_HEAD: &str = "[aterm harness] Claude Code ";

/// THE ANNOUNCEMENT, typed as one ordinary user turn. It asks for a good
/// stopping point and names the one line to answer with; it never asks the
/// agent to cancel anything.
#[must_use]
pub fn prepare_prompt(from: &Version, to: &Version, source: Source, marker: &str) -> String {
    format!(
        "{ANNOUNCE_HEAD}{to} ({}) is installed; this session runs {from}. \
         To move you onto it, aterm will restart this Claude Code in place and resume this \
         same conversation (claude --resume, same tab, same flags). Please get to a good \
         stopping point first: let any background tasks, subagents or workflows you started \
         finish (do not cancel them), save or commit work in progress, and do not start new \
         long-running work. When nothing of yours is still running, reply with {marker} on a \
         line by itself. If you cannot stop now, say why; aterm will wait and ask again later.",
        source.as_str()
    )
}

/// THE CONTINUATION, typed once the relaunched process holds the same session.
#[must_use]
pub fn continue_prompt(from: &Version, to: &Version) -> String {
    format!(
        "[aterm harness] Upgraded: this session was restarted on Claude Code {to} (from {from}) \
         and resumed. Continue where you left off; if you were waiting on the user, say so in \
         one line."
    )
}

/// Whether the transcript's assistant turns carry `marker` as a line of their
/// own text. Only ASSISTANT entries are read: the announcement itself (a user
/// entry) contains the marker too, and must never count as the answer.
/// `jsonl` is the transcript's tail; a line that is not JSON is skipped (the
/// tail's first line may be cut).
#[must_use]
pub fn transcript_has_ready(jsonl: &str, marker: &str) -> bool {
    for line in jsonl.lines() {
        if !line.contains(marker) {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(content) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) != Some("text") {
                continue;
            }
            if let Some(text) = part.get("text").and_then(Value::as_str)
                && text.lines().any(|l| l.trim().trim_matches('`') == marker)
            {
                return true;
            }
        }
    }
    false
}

/// What the driver measured about one session, for the gates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Claude's own `status` (the session file).
    pub status: String,
    /// Seconds since Claude last changed `status`.
    pub status_age_s: u64,
    /// The composer is present and holds no typed text (a dim placeholder
    /// suggestion is not typed text — [`composer_is_empty`]).
    pub composer_empty: bool,
    /// An approval or question box is up.
    pub approval_box: bool,
    /// The busy footer (`esc to interrupt`) is on screen.
    pub busy_footer: bool,
    /// Executable basenames of the agent's descendants that are background
    /// WORK — shells and `caffeinate` (MCP servers are not: resume restarts them).
    pub background: Vec<String>,
    /// aterm's hold on the session (a halt, or a driver's custody or lease).
    pub held: bool,
    /// Seconds the screen has been unchanged (no output, no keystroke echo).
    pub quiet_s: u64,
}

/// A gate's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Go.
    Go,
    /// Not yet, and why (one word).
    Wait(&'static str),
}

/// Seconds Claude must have been idle, and the screen quiet, before anything is
/// typed: long enough that a person who just read the answer and started to
/// think is not typed over, short enough that an idle session moves the same
/// hour.
pub const QUIET_S: u64 = 20;

/// May the ANNOUNCEMENT be typed now? Claude idle and settled, the composer
/// empty, no box, no busy footer, no hold, the screen quiet.
#[must_use]
pub fn gate_announce(f: &Facts) -> Gate {
    if f.held {
        return Gate::Wait("held");
    }
    if f.status != "idle" {
        return Gate::Wait("not-idle");
    }
    if f.busy_footer {
        return Gate::Wait("busy");
    }
    if f.approval_box {
        return Gate::Wait("box");
    }
    if !f.composer_empty {
        return Gate::Wait("draft");
    }
    if f.status_age_s < QUIET_S || f.quiet_s < QUIET_S {
        return Gate::Wait("settling");
    }
    Gate::Go
}

/// May the agent be ended now? Everything [`gate_announce`] asks, plus the
/// agent's READY answer and NOTHING running under it. Nothing is ever killed to
/// reach this gate: background work is waited for.
#[must_use]
pub fn gate_restart(f: &Facts, ready: bool) -> Gate {
    if !ready {
        return Gate::Wait("not-ready");
    }
    if !f.background.is_empty() {
        return Gate::Wait("background");
    }
    gate_announce(f)
}

/// Whether the composer holds no TYPED text: present, and either blank or a dim
/// placeholder suggestion — the cursor at column 2 of the caret row with text
/// after it ([`aterm_phase::phase::is_placeholder`]) AND that text drawn DIM.
/// `dim_at_caret` is the caret row's column 2 read with the `cell` verb
/// (measured 2026-09-23 under Claude Code 2.1.280 and 2.1.281: the suggestion's
/// first letter reads `dim`, a typed draft's `none`). The cursor alone cannot
/// tell them apart: a typed draft whose caret was moved home (←, Home, ctrl-a)
/// leaves it at column 2 too, and a notice typed there lands IN FRONT of the
/// person's draft and is submitted with it (measured the same day). A
/// suggestion drawn without SGR dim (no colour at all) therefore reads as a
/// draft: nothing is typed, which is the safe way to be wrong. A draft on a
/// second row is typed text whatever the cursor says. `cursor` is `(row, col)`
/// in the same coordinates as `rows`.
#[must_use]
pub fn composer_is_empty(
    rows: &[String],
    cursor: Option<(usize, usize)>,
    dim_at_caret: bool,
) -> bool {
    let Some((caret, lines)) = aterm_phase::phase::composer_draft(rows) else {
        return false;
    };
    if lines.iter().skip(1).any(|l| !l.is_empty()) {
        return false;
    }
    if lines.first().is_none_or(|l| l.is_empty()) {
        return true;
    }
    dim_at_caret && matches!(cursor, Some((row, col)) if row == caret && col == 2)
}

/// Where the upgrade of one session stands. Persisted per session id, so a sweep
/// that starts fresh every minute picks up exactly where the last one stopped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Nothing typed yet.
    #[default]
    Pending,
    /// The announcement was typed at `at_s` (unix seconds), `asks` times so far.
    Announced {
        /// When the latest announcement was typed.
        at_s: u64,
        /// How many announcements were typed for this upgrade.
        asks: u32,
    },
    /// SIGTERM was sent at `at_s`.
    Exiting {
        /// When.
        at_s: u64,
    },
    /// The relaunch line was typed at `at_s`.
    Relaunched {
        /// When.
        at_s: u64,
    },
    /// The new process holds the session and the continuation was typed.
    Done,
    /// Stopped, and why. Not retried for the same (session, target).
    Failed(String),
}

/// Seconds between announcements when the agent has not answered READY: the
/// agent may have said it cannot stop yet, or the person took the turn.
pub const REASK_S: u64 = 30 * 60;

/// The most announcements one upgrade makes before it stops asking (and says so
/// in the ledger). Never a forced restart.
pub const MAX_ASKS: u32 = 4;

impl Phase {
    /// The word the ledger and `status` print.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Phase::Pending => "pending".to_string(),
            Phase::Announced { asks, .. } => format!("announced:{asks}"),
            Phase::Exiting { .. } => "exiting".to_string(),
            Phase::Relaunched { .. } => "relaunched".to_string(),
            Phase::Done => "done".to_string(),
            Phase::Failed(why) => format!("failed:{why}"),
        }
    }
}

/// One step the driver takes next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Type the announcement.
    Announce,
    /// Nothing to do this tick, and why.
    Wait(&'static str),
    /// End the agent (SIGTERM to its pid only).
    Terminate,
    /// Stop asking: [`MAX_ASKS`] announcements went unanswered.
    GiveUp,
}

/// THE REDUCER for the cooperative half: from where the upgrade stands, the
/// facts and whether the agent has answered, the one next step. The exit and
/// relaunch halves are the driver's, because each is a wait on the kernel.
#[must_use]
pub fn next_step(phase: &Phase, f: &Facts, ready: bool, now_s: u64) -> Step {
    match phase {
        Phase::Pending => match gate_announce(f) {
            Gate::Go => Step::Announce,
            Gate::Wait(w) => Step::Wait(w),
        },
        Phase::Announced { at_s, asks } => {
            if ready {
                return match gate_restart(f, true) {
                    Gate::Go => Step::Terminate,
                    Gate::Wait(w) => Step::Wait(w),
                };
            }
            if now_s.saturating_sub(*at_s) < REASK_S {
                return Step::Wait("awaiting-ready");
            }
            if *asks >= MAX_ASKS {
                return Step::GiveUp;
            }
            match gate_announce(f) {
                Gate::Go => Step::Announce,
                Gate::Wait(w) => Step::Wait(w),
            }
        }
        Phase::Exiting { .. } | Phase::Relaunched { .. } => Step::Wait("in-flight"),
        Phase::Done => Step::Wait("done"),
        Phase::Failed(_) => Step::Wait("failed"),
    }
}

#[cfg(test)]
#[path = "upgrade_tests.rs"]
mod tests;
