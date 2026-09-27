// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness` — the read views of the Claude Code harness: `usage`,
//! `limits`, `disk` and `ledger` (design `docs/DESIGN-aterm-wrapper-2026-09-17.md`
//! §5.2, §5.5, §5.8, and its §0.4 for what is no longer here).
//!
//! # What this command is now
//!
//! A reader. The supervisor that ACTS — presses a proven-safe approval,
//! continues a turn, switches a model at a bucket limit, escalates — is the
//! one engine in [`crate::supervise`], hosted by the window and started by
//! `aterm drive watch|supervise`. On 2026-09-23 the second stack that used to
//! live beside this file (`harness watch` and its wire, mark, observer, ring
//! ledger, profiles, account rotation, alignment verdict and config table)
//! was deleted: in its whole life none of its acts landed (design §0.4
//! lists the five measured reasons), and it duplicated the engine. The
//! verbs `status`, `mark`, `enable`, `disable`, `align`, `caps`, `accounts`,
//! `liveness`, `recover`, `nudge`, `switch`, `watch` and `config` went with it;
//! each is refused by name ([`DELETED`]) with where the capability went.
//!
//! * `usage` folds the vendor's transcript for this working directory — a
//!   filesystem fact no hook is needed for.
//! * `limits` reads the session's SCREEN over the control socket (the one
//!   interface) through the engine's own readers: the wall the last turn
//!   ended on ([`aterm_phase::wall`], the one wall classifier — the engine's
//!   turn-end policy acts on the same reading) and the windows the vendor
//!   painted on its `/usage` panel ([`usage::usage_panel_windows`]), each
//!   reset placed by the supervisor's own clock ([`limit::parse_reset`]).
//! * `disk` measures, and removes only a named safelist class under
//!   `disk.apply = true`; every measurement, removal and refusal is appended
//!   to `<state>/disk.jsonl`, cut back to its newest rows past
//!   [`DISK_LEDGER_MAX_BYTES`] ([`bound_ledger`]).
//! * `ledger` reads the engine's approval ledger
//!   ([`crate::supervise::approvals`], `<aterm state>/drive/<sid>.jsonl`) — or
//!   `ledger disk`, the file above.
//! * `upgrade` is the one verb here that ACTS: it moves a live Claude Code
//!   session onto a newer build in place, cooperatively
//!   ([`super::upgrade_drive`]) — and a live Codex, its shared daemon first
//!   and then each TUI by a typed `/exit` and `codex resume`
//!   ([`super::upgrade_codex`]). It is not a supervisor policy — it restarts
//!   the agent process, never answers a box or continues a turn — and the
//!   window's host takes the same steps on its own tabs, at each session's
//!   idle points, under aterm.toml's `[harness] upgrade` switch. A hand-run
//!   sweep obeys `[harness] enabled`
//!   ([`crate::supervise::SupervisorConfig::from_path`], the table's one
//!   reader).
//!
//! The hook bridge — `hook`, `statusline`, `install`, `uninstall` — is RETIRED
//! by decision "B" (2026-09-22, [`RETIRED`]) and answered before anything is
//! parsed ([`main_entry`]).
//!
//! # Nothing polls
//!
//! Every subcommand is one pass: read, decide, print, exit. `limits` makes
//! one screen read; nothing here waits on a clock.
//!
//! # The exit contracts
//!
//! * The retired `hook` and `statusline` **always exit 0** and print nothing:
//!   a bridge script an older `install` wrote still runs them, and Claude Code
//!   reads a failing hook command as a block (CHANGELOG.md:3495-3502).
//! * Every other subcommand exits 0 on success, 1 on a refusal it can
//!   explain (`limits` with no session answering, an `upgrade` the master
//!   switch stands down), 2 on a usage error (the retired
//!   `install`/`uninstall` among them) — and a one-shot `upgrade` whose sweep
//!   found another sweeper holding the lock exits 75, atpkg's `EX_TEMPFAIL`
//!   for the same contention, because that one refusal means "try again
//!   later".
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested, `limits` against a
//! scripted control connection.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_json::{Map, Value};

use super::disk;
use super::source::Source;
use super::usage::{self, AccountView, UsageView, rfc3339_utc};
use crate::supervise::journal::Journal;
use crate::supervise::run::{Ctl, Session};
use crate::supervise::transport::{Endpoint, RelayCtl};
use crate::supervise::{SupervisorConfig, approvals, limit};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The harness name, which is also the state directory's name (design §1.3).
pub const HARNESS: &str = "claude-harness";

/// Why `hook`, `statusline`, `install` and `uninstall` are retired — the one
/// decision-"B" sentence (docs/FABRIC-LITERALLY-2026-09-19.md §13).
pub const RETIRED: &str =
    "aterm installs nothing into an agent; a starting window removes what an older build installed";

/// The verbs deleted with the second harness stack on 2026-09-23. They are
/// named in the refusal so a script that still calls one learns where the
/// capability went rather than reading a bare "unknown subcommand".
pub const DELETED: &[&str] = &[
    "status", "mark", "enable", "disable", "align", "caps", "accounts", "liveness", "recover",
    "nudge", "switch", "watch", "config",
];

/// How many rows `ledger` prints when the caller names no count.
pub const LEDGER_DEFAULT_ROWS: usize = 20;

/// The most rows one read of a ledger will hold at once: the tail is kept in a
/// ring of this size while the file streams past, so a large ledger is never
/// materialised whole to print twenty rows of it.
pub const READ_MAX_ROWS: usize = 4096;

/// The disk journal's file name under the harness state directory.
pub const DISK_LEDGER: &str = "disk.jsonl";

/// The size past which `disk` cuts its journal back before appending. Every
/// run appends a report row, report-only runs included, so without a bound a
/// verb that exists to act on a full disk would grow its own file for ever
/// (the ring it wrote to until 2026-09-23 was bounded; the journal that
/// replaced it was not).
pub const DISK_LEDGER_MAX_BYTES: u64 = 1024 * 1024;

/// How many of the newest rows survive a cut ([`bound_ledger`]). At most
/// [`READ_MAX_ROWS`], so the cut reads through the same bounded ring as
/// `ledger disk`.
pub const DISK_LEDGER_KEEP_ROWS: usize = 1024;

/// The durable config's home: aterm.toml. MIRRORS `aterm-gui`'s `config_path`
/// (XDG first, then `$HOME/.config/aterm/aterm.toml`), so `disk` reads the
/// same `[disk]` table the window's Settings page writes.
#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/aterm/aterm.toml"))
}

/// Every raw value text assigned to `[<table>] <key>` in `text`, in file
/// order.
///
/// A DELIBERATELY minimal line reader for the two numeric `[disk]` knobs
/// (`warn_free_gib`, `target_stale_days`). It understands the two spellings
/// `apply_prefs_edits` can produce — a `[table]` header with `key = value`
/// under it, and the top-level dotted `table.key = value` — and ignores `#`
/// comments. [`toml_int`] takes the LAST assignment that is an integer, as
/// TOML's own last-wins would read, and a value of any other type is not an
/// answer. The boolean does not come through here: `disk.apply` is
/// [`toml_bool`], over the parser the window reads aterm.toml with, and the
/// `[harness]` switches are the supervisor policy's own reader
/// ([`crate::supervise::SupervisorConfig::from_aterm_toml`]).
///
/// Until 2026-09-23 the two numeric knobs went through the harness
/// contract's fail-closed TOML reader. That reader went with the contract
/// (design §0.4); one grammar for two scalars is this one.
fn toml_assignments<'a>(text: &'a str, table: &str, key: &str) -> Vec<&'a str> {
    let dotted = format!("{table}.{key}");
    let mut in_table = false;
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_table = header.trim() == table;
            continue;
        }
        let Some((lhs, rhs)) = line.split_once('=') else {
            continue;
        };
        let lhs = lhs.trim();
        if (in_table && lhs == key) || lhs == dotted {
            out.push(rhs.trim());
        }
    }
    out
}

/// The value of `[<table>] <key>` in `text`, when it is a TOML boolean — and
/// `false` wherever the file says `<table>.<key> = false`, even where TOML
/// files that line under another table.
///
/// The reader for `disk.apply`, the one BOOLEAN knob this module answers out
/// of `aterm.toml`; the ON/OFF switches that gate acts on the owner's
/// sessions are `[harness]`'s, read by the supervisor policy's own reader
/// ([`crate::supervise::SupervisorConfig::from_aterm_toml`]). `false` is its
/// SAFE answer — nothing removed — and any key added here must have the same
/// polarity, because every reading below that is not TOML's own can only
/// ever answer `false`. The numeric knobs go through [`toml_int`].
///
/// TWO READERS, in order:
///
/// 1. **The window's own parser, [`aterm_toml`], on every file it reads.**
///    Settings seeds from that parser, and the Settings writer keeps
///    whatever spelling the file already has. Until 2026-09-24 this was a
///    line reader alone, which understood a `[table]` header and a top-level
///    `table.key` and nothing else, so three spellings TOML (and the window)
///    read — an inline `disk = { apply = false }`, a quoted `"apply" = false`,
///    a spaced `disk . apply = false` — read `None` here. Where the root key
///    is set, this reads TOML's answer for it.
/// 2. **[`salvage_false`], only on a file that parser REFUSES.** A stray typo
///    three tables away makes the window fall back to its defaults, and a
///    `false` the owner wrote must still read `false`. A file nothing can
///    parse has no right answer, only a safe one, so that reader answers
///    `false` or nothing, never `true`: a line it mis-reads can only keep a
///    removal from happening, never the reverse, and a refused file grants
///    no removal consent.
///
/// ONE `false` WINS, in both. A `false` filed under ANY table path ending in
/// `<table>.<key>` reads `false` ([`false_at`]), even beside a root `true`:
/// a `disk.apply = false` appended below `[theme]` is `theme.disk.apply` to
/// TOML, and an explicit "no" never turns into consent because of where in
/// the file it was written. A misplaced `true` is nothing.
#[must_use]
pub fn toml_bool(text: &str, table: &str, key: &str) -> Option<bool> {
    match aterm_toml::from_str::<aterm_toml::Value>(text) {
        Ok(doc) if false_at(&doc, table, key).is_some() => Some(false),
        Ok(doc) => doc.get(table)?.get(key)?.as_bool(),
        Err(_) => salvage_false(text, table, key).then_some(false),
    }
}

/// The dotted path of a `<table>.<key> = false` in `v` — at `v`'s own root
/// first, else under the first table (or array-of-tables element, `name[i]`)
/// that holds one, at any depth.
fn false_at(v: &aterm_toml::Value, table: &str, key: &str) -> Option<String> {
    use aterm_toml::Value as T;
    if v.get(table).and_then(|t| t.get(key)).and_then(T::as_bool) == Some(false) {
        return Some(format!("{table}.{key}"));
    }
    let under = |head: String, at: String| {
        if at.starts_with('[') {
            format!("{head}{at}")
        } else {
            format!("{head}.{at}")
        }
    };
    match v {
        T::Table(t) => t
            .iter()
            .find_map(|(name, v)| Some(under(name.clone(), false_at(v, table, key)?))),
        T::Array(a) => a
            .iter()
            .enumerate()
            .find_map(|(i, v)| Some(under(format!("[{i}]"), false_at(v, table, key)?))),
        _ => None,
    }
}

/// Whether a file the TOML parser refused still says `<table>.<key> = false`
/// on some line ([`toml_bool`]'s fallback), in any one-line spelling TOML
/// gives the key ([`crate::supervise::config::assignments`], the line reader
/// the `[harness]` policy reads such a file with).
///
/// ONE `false` WINS, filed under any path that ENDS in `<table>.<key>`, as
/// in [`toml_bool`]'s parsed reading. A refused file can say the key twice,
/// or carry a line this reader attributes wrongly; either way the ambiguity
/// resolves to the safe side. Last-assignment-wins, which this reader used
/// to be, let a stray `apply = true` below the owner's `false` switch the
/// removal back on.
fn salvage_false(text: &str, table: &str, key: &str) -> bool {
    crate::supervise::config::assignments(text)
        .iter()
        .any(|(path, value)| path.ends_with(&[table, key]) && *value == "false")
}

/// `[<table>] <key>` as a decimal integer, or `None`.
#[must_use]
pub fn toml_int(text: &str, table: &str, key: &str) -> Option<i64> {
    toml_assignments(text, table, key)
        .into_iter()
        .rev()
        .find_map(|v| v.parse().ok())
}

/// The usage text.
pub const USAGE: &str = "\
aterm harness — read views of the Claude Code harness, and the live agent
upgrade. The supervisor that acts on a session is the window's own host, by
default, for every Claude Code and Codex session (`aterm drive watch|supervise`
runs the same engine from a terminal).

USAGE:
    aterm harness usage [--json]                 this directory's transcript spend (design 5.2)
    aterm harness limits [@<sid>] [--json]       the wall and the painted /usage windows on the session's screen
    aterm harness disk [<build-dir> ...] [--apply <class>] [--json]
    aterm harness ledger [@<sid>] [<n>] [--json] the supervisor's approval ledger
    aterm harness ledger disk [<n>] [--json]     the disk journal
    aterm harness ledger upgrade [<n>] [--json]  the live upgrade's ledger: every act, and the owner's word
    aterm harness upgrade [<sid>] [--dry-run] [--json]
                                                 move live Claude Code sessions onto a newer build
                                                 and the priority list's best available model, and
                                                 live Codex sessions (their daemon first) onto the
                                                 managed Codex
    aterm harness upgrade [<sid>] --status [--json]
                                                 each upgrade: how long behind, what it waits on
    aterm harness upgrade <sid> --now | --defer <dur> | --skip [--json]
                                                 the owner's word on one tab's upgrade
    aterm harness upgrade models [set <id>,<id>,...]
                                                 the model priority list: show it (availability,
                                                 target, Claude Code's recommendations) or set it

`hook`, `statusline`, `install` and `uninstall` are RETIRED: aterm installs nothing into
an agent. `install`/`uninstall` refuse (exit 2); `hook` and `statusline` do nothing and
exit 0, for a bridge an older build installed. A starting window removes what `install`
wrote to ~/.claude/settings.json.

`status`, `mark`, `enable`, `disable`, `align`, `caps`, `accounts`, `liveness`, `recover`,
`nudge`, `switch`, `watch` and `config` are GONE: the engine behind `aterm drive` is the
one supervisor, and aterm.toml's [harness] table is its policy.

OPTIONS:
    --state <path>      the harness state directory, where the disk journal lives
                        (default: $ATERM_HARNESS_STATE, else <aterm state>/harness/claude-harness)
    --config <path>     where the [disk] knobs are read from
                        (default: $XDG_CONFIG_HOME/aterm/aterm.toml, else $HOME/.config/...)
    --sock <path>       limits: the control socket (default: the resolved one)
    --utc-offset <s>    seconds east of UTC for the times usage prints (default: 0, i.e. UTC)
    --apply <class>     disk: the ONE safelist class to act on. atpkg-gc and
                        claude-purge are surfaced and never removed from here;
                        they name the command that owns them
    --dry-run           upgrade: print each session's next step, type and signal nothing
    --status            upgrade: print each recorded upgrade, sweep nothing
    --now               upgrade <sid>: restart it at its next turn end (the quiet waits waived)
    --defer <dur>       upgrade <sid>: not for <dur> (90s, 30m, 6h, 2d; at most 30d)
    --skip              upgrade <sid>: stay on the running build until a newer target comes
    --json              the JSON form
    -h, --help          this text

`usage` folds the NEWEST transcript in this working directory's project directory
(~/.claude/projects/<dir>/*.jsonl) and says so: two Claude Code sessions in one directory
share it, so the newest file may be the other one's. It carries spend, never a window.

`limits` reads the session's screen once over the control socket (`text --json tail=40`)
and prints the wall the last turn ended on (the kind the supervisor acts on, its words and
its reset) and any windows a painted /usage panel shows. With no @<sid> it
reads the session it runs in ($ATERM_PARENT_SESSION_ID); with neither it refuses (exit 2)
rather than read whichever session the socket defaults to. No session answering is exit 1.

`disk` REPORTS AND REMOVES NOTHING by default. Every row carries the witness that would
make it safe to remove — a build tool's own marker plus two clocks past
`disk.target_stale_days`, or a version directory the live symlink does not point at, or
a package build the live one supersedes. Removal needs BOTH `disk.apply = true` in
aterm.toml AND an explicit `--apply <class>`; a refusal is written to the disk journal as
a denial row rather than dropped. Nothing under the transcripts root is removable under
any flag. Build directories are the ones you NAME on the line. The journal is cut back to
its newest 1024 rows once it passes 1 MiB.

`ledger` prints the newest rows of the approval ledger the supervisor keeps for one
session (<aterm state>/drive/<sid>.jsonl: every box its approval policy decided —
approved, typed, skipped, escalated or deferred — with the rule and the command). With no @<sid>
it reads the session it runs in.

`upgrade` MOVES A LIVE CLAUDE CODE SESSION ONTO A NEWER BUILD without losing
the conversation or anything it has running. A newer build is aterm's managed
twin, or — for a session that runs Claude's own native install, the one whose
footer shows Claude Code's own `Update installed` notice — that install's
current version; never an older one. Each sweep moves each session one step: when the
agent is idle, its composer empty, no approval box up and nobody typing into the tab
(`[harness] human_grace_s`), it TYPES a notice
asking the agent to reach a stopping point — let its background tasks finish,
never cancel them — and answer with a one-time READY marker. Only after that
answer is in the transcript, with no shell still running under the agent and
nobody holding the tab, does it send SIGTERM (never a harder signal), wait for
the shell prompt, type a relaunch line — the atpkg hook sourced first so the
shell's PATH is healed, then the same launch flags with `--resume <session>` —
and, once the new process holds the conversation, type one line telling the
agent to carry on. A launch it cannot resume (`--print`, `--worktree`, an
unknown flag) is refused, not guessed; an unanswered notice is asked again
every 30 minutes, four times at most. A SESSION IS MOVED ONLY WHILE THE
AGENT IS ITS SHELL'S FOREGROUND JOB ON A TERMINAL AN ATERM TAB OWNS: an agent
in a multiplexer pane (tmux, screen, zellij) or on any other pty the tab does
not own (`script`, ssh) is held back — said once in the ledger, then waited
on — because nothing typed into the tab reaches it; one started by a launcher
script, a wrapper (`caffeinate claude`), `sh -c`, an IDE task or a shell with
job control off is refused once, before anything is typed, because nothing
would take the terminal back to relaunch it; and a suspended or backgrounded
one waits. Each session's step lands in
`<state>/upgrade/`, with a ledger of every act beside it, so the next sweep
picks up where this one stopped — between the exit and the relaunch too, for
5 minutes and while the agent's shell lives; past that the restart is
recorded as failed and nothing more is typed into the tab.
THE WINDOW DOES IT BY DEFAULT, with no sweep: each of its own tabs' supervisors
takes the step at its session's idle points once atpkg, or Claude Code's own
updater, installs a newer build, while aterm.toml's `[harness] enabled` and
`[harness] upgrade` are not turned off; one lock keeps it and a hand-run sweep
apart. It hands the relaunched agent straight back to its supervisor, which types
the carry-on at its first idle point. It also RELAUNCHES an agent that crashed
(its session record left behind, read as the exit is seen — a graceful exit is
someone's), on its conversation (`[harness] relaunch`).
THE MODEL goes with it: a session behind the priority list's best model available
on the managed build (`aterm harness upgrade models`) is moved onto it on the
relaunch line (`--model`, session-only — never `/model`, which also saves the
person's default), riding a build restart when there is one, else by itself once
its prompt cache is cold, or at most an hour after the move came due. A turn that
ended with the agent's own background work in flight (Claude's `Waiting for N
dynamic workflow`, a shell it left running, a Codex background terminal) is a
NATURAL BREAK: once it has stood 20 s the window types the notice there — it
interrupts the agent's orchestration once — and the restart still waits for an
idle point with nothing running under the agent (a re-ask waits for one too), so
running work is never ended.
A HAND-RUN sweep obeys `[harness] enabled` too: with it off it prints `step=refused:bypassed`, types and signals nothing
and exits 1 (a `--dry-run` says so on stderr, still prints its plan, and
exits 0); naming the verb is the consent `[harness] upgrade` would give.
The refusal also names, on stderr, every restart an earlier sweep left part
way (its agent already signalled): that conversation stays where it stopped
until the switch is back on. A `false` wins wherever it is: the line written
below another `[table]` header, which TOML files under that table, still turns
the sweep off, and `upgrade` says on stderr where it found it.
A sweep that did not run exits non-zero: `step=busy:another-sweep` (another
sweeper holds the lock — often the window's own) exits 75, try again later;
`step=busy:state-unwritable` or `busy:lock-unopenable` exits 1.
A `--dry-run` line also says `pending_for=` (how long the session has been
behind) and `wait_for=` (how long its current wait has lasted), as the last
step recorded them.

AND CODEX, by the same step under the same lock and ledger — a hand-run
sweep's, and in the window each Codex tab's own worker at its session's idle
points (Codex 0.157 runs TWO processes: a shared app-server DAEMON per
$CODEX_HOME that holds every conversation, and the TUI in the tab, a client of
it). THE DAEMON FIRST: for ~/.codex (a hand-run sweep) and every home a live
Codex TUI runs with, a daemon on an older
build than the managed Codex — or on the same build with the vendor's own
updater armed (`packages/app-server-daemon/auto-update-version`, which re-runs
chatgpt.com's installer outside atpkg) — is moved by the vendor's verb,
`<managed codex> app-server daemon update --from-cli --yes`, run with the
environment the daemon itself was started with: the managed build is copied
in and PINNED, the updater goes, the daemon restarts, and every attached TUI
reconnects (`• Reconnected. No input was resent.`). Only while NOTHING RUNS
in it — every thread it holds idle and settled, attached to a tab or not (its
locks reveal them), and no BACKGROUND TERMINAL a finished turn left running
under it (a unified-exec command leads a session of its own under the
daemon; the restart would end it) — no person is at a Codex tab on that home,
none is held, the OWNER'S WORD on none keeps it (a `--skip` of this build or
a `--defer` on a Codex tab holds the daemon that runs its conversation too;
that tab's `--now` waives its own attended guard here as well), and every
Codex attached to the daemon is one the pass sees — the window's step asks
every Codex tab of its window, whichever tab's step reaches the daemon first
(a TUI in another window or a pane was asked none of this): `wait:busy-thread`,
`background-terminal`, `owner-held`, `unseen-client`, `held`, `attended`,
`settling`. A daemon ahead of the managed Codex is never moved back
(`wait:vendor-ahead`). THEN EACH TUI that is a tab's foreground job on an
older build: a DAEMON-MODE client at an idle point (no turn running on
screen, its composer's dim placeholder, no box, quiet 20 s, nobody at the tab
— the same gates, the same `--now`) is ended by a TYPED `/exit` — fenced on
the screen it was judged by, its Enter guarded on the composer's row — and
relaunched at the returned prompt as `<managed codex> resume <the same flags>
<thread>`, the thread read from the TUI's own exit hint (`Reconnect: codex
resume <id>`: its command block's output, else the rows directly above the
new prompt's FIRST row, so a prompt of two rows is no obstacle — which is why
the `/exit` waits for the shell integration's marks, `wait:no-shell-integration`);
its work never stops, in the daemon, so nothing is announced and nothing
continued. It waits for its daemon to be on the build it moves to, and says
what the daemon waits on (`wait:daemon-first:<why>`). An EMBEDDED session
(`--no-daemon`, or a launch Codex keeps out of the daemon; the kernel proves
it by the `thread-writer-locks/<id>.lock` it holds) gets Claude's protocol —
the notice, its READY answer in the rollout, nothing running under it, a
background terminal included (`wait:background-terminal`: it would end with
the TUI) — then the same `/exit`, the relaunch through the one relaunch line
Claude Code's restart types, and a carry-on line (the window hands the new
TUI back to its supervisor, which types it at its next idle point). NO
SIGNAL IS EVER SENT TO CODEX: SIGTERM leaves its tab in the alternate screen
with kitty keyboard, mouse and bracketed paste on. What the lane types and
does not submit (a guarded Enter that missed) is said on the ledger
(`left-typed:<notice|exit>`) and cleared while it alone is in the composer
(`left-typed-cleared`), never read as a person's draft; a `/exit` the TUI
outlives is never waited on again (`wait:exit-not-taken`, then
`failed:stale-exit`). A TUI that names no thread as it exits is not
relaunched (`failed:no-resume-hint`: its conversation runs on in the daemon;
`codex resume` in the tab takes it back). Its state is per TAB: `codex-<sid>`
in `--status`, the ledger and the `upgrade=` column — a move that stopped
after its `/exit` stays listed though no Codex holds it any more (in the
window for a day).

`upgrade --status` READS the recorded upgrades and sweeps nothing: one line
for each a live Claude Code or Codex still holds on a build older than its
target, in that tab (a conversation that ended, or was moved onto the build by hand,
leaves its file behind; it is not listed) — the tab, the conversation, from
and to, the phase, `pending_for=`,
`wait=` (what the last step waited on: `settling`, `awaiting-ready`,
`background`, `draft`, `attended`, `held`, `terminal:<owner>`, `not-idle:busy`
for a turn a hand-run sweep found in progress …) with `wait_for=`, the owner's
`request=`, and `stalled=` — `-` while the upgrade will move on its own, else
why it will not: `gave-up` (no READY answer after the last notice),
`refused:<why>`, `failed:<why>`, `held-back:<owner>` (a multiplexer pane), or
`overdue` (6 hours behind, whatever it waits on, unless the owner's `--now`
came in the last 30 minutes). The window shows the same per tab as `upgrade=`
in `aterm ctl status`/`sessions`, records it in Settings ▸ Messages, and marks
a STALLED tab (`meta attention owner=upgrade`) — never a tab that is merely
waiting for its turn end.

THE OWNER'S WORD on one tab's upgrade — `upgrade <sid> --now|--defer
<dur>|--skip` — is written into that upgrade's state under the sweep's own
lock (waiting up to 10 s for a step in progress; past that it exits 75 and
writes nothing) and put on the ledger as `requested:<word>`, and it wakes the
window's worker for that tab, which takes it at the session's next idle
point. The word is on the TAB named: the same conversation resumed in another
tab is asked afresh. `--now` waives the two waits that keep a person from
being typed over — the quiet window, and the hold on a tab a person gave input
to within `[harness] human_grace_s` (its `status human_ms=`) — the word IS
that person — and nothing else (an idle status, an empty composer, no box, no
hold, the READY answer and nothing running under the agent are still
required, and what is typed is still fenced on the screen it was judged by).
Every word arms a NEW ROUND of the upgrade: a notice after it asks for a READY
marker no earlier answer carries. An upgrade the owner holds (`--defer`,
`--skip`) owns none of its session's turn ends: the tab's supervisor goes on
continuing the worker as if none were pending, and a notice already typed is
ended — the hold's end brings a fresh notice and needs a fresh READY. ONE
PLACE `--now` adds an act: an upgrade that GAVE UP (no READY answer after the
last notice) is re-armed, so the notice is typed again and, after its READY
answer, the agent is restarted. An upgrade that was REFUSED or FAILED is not
re-armed — `--now` exits 1 and names why; what stopped it still holds. `--now`
cannot reach an agent HELD BACK in a multiplexer pane either (typing into the
tab does not reach it; stderr says so). `--defer` holds it until the time runs
out; `--skip` holds it on the running build until a newer target than this
one comes, and is what ends a stall the harness cannot move. A restart
already under way is neither held nor hurried (exit 1).

The window writes the same words, on the same path, with no shell: a
stalled upgrade's band row offers two of them (Upgrade now where `--now`
moves it, Not today — a one-day `--defer` — and Skip version), Settings ▸
Messages offers them on the waiting record while exactly one session waits,
and the tab's context menu offers every word that does something for the
upgrade of the pane it was opened on. Each is for the session and the build
it names: pressed after the upgrade moved on to a newer one, nothing is
written.
";

// ---------------------------------------------------------------------------
// Injected environment
// ---------------------------------------------------------------------------

/// Everything the subcommands need from outside themselves, injected so every
/// decision below is a pure function of its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    /// The harness state directory. Absolute. Holds the disk journal.
    pub state: PathBuf,
    /// The aterm state root the supervisor's approval ledger lives under
    /// (`<root>/drive/<sid>.jsonl`); `None` when it cannot be located.
    pub aterm_state: Option<PathBuf>,
    /// The session's working directory. Absolute.
    pub cwd: PathBuf,
    /// The owner's home, when known.
    pub home: Option<PathBuf>,
    /// Unix seconds. Read once, at the top of [`main_entry`].
    pub now: i64,
    /// The aterm session id (`$ATERM_PARENT_SESSION_ID`), or empty.
    pub sid: String,
    /// Seconds east of UTC for printed wall-clock times.
    pub utc_offset_s: i64,
    /// `--sock` — the control socket `limits` reads through. `None` resolves
    /// it the way every other aterm client does.
    pub sock: Option<String>,
    /// `aterm.toml` — where the `[disk]` knobs live. `None` when neither
    /// `$XDG_CONFIG_HOME` nor `$HOME` is set: every knob at its default.
    pub config: Option<PathBuf>,
}

impl Env {
    /// THE one environment read in this module.
    ///
    /// # Errors
    ///
    /// The state root cannot be located, or the working directory cannot be
    /// read.
    pub fn from_process() -> Result<Env, String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute());
        let aterm_state = crate::operator::default_state_root().ok();
        let state = match std::env::var_os("ATERM_HARNESS_STATE") {
            Some(dir) if Path::new(&dir).is_absolute() => PathBuf::from(dir),
            Some(_) => return Err("ATERM_HARNESS_STATE must be absolute".to_string()),
            None => aterm_state
                .clone()
                .ok_or_else(|| "the aterm state root is unknown".to_string())?
                .join("harness")
                .join(HARNESS),
        };
        let cwd = std::env::current_dir()
            .map_err(|e| format!("the working directory cannot be read: {e}"))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        Ok(Env {
            state,
            aterm_state,
            cwd,
            home,
            now,
            sid: std::env::var("ATERM_PARENT_SESSION_ID").unwrap_or_default(),
            utc_offset_s: 0,
            sock: None,
            config: default_config_path(),
        })
    }

    /// Where the vendor keeps its transcripts: `~/.claude/projects`. A
    /// FILESYSTEM fact, not a hook — `--bare` removes hooks, plugins and the
    /// statusLine in one flag and does not touch this.
    #[must_use]
    pub fn transcripts_root(&self) -> Option<PathBuf> {
        self.home
            .as_ref()
            .map(|h| h.join(".claude").join("projects"))
    }

    /// The disk journal.
    #[must_use]
    pub fn disk_ledger(&self) -> PathBuf {
        self.state.join(DISK_LEDGER)
    }

    /// `sid` as a selector (`@s-…`), else this session's, else `None`.
    #[must_use]
    pub fn target(&self, sid: Option<&str>) -> Option<String> {
        let sid = sid.map_or(self.sid.as_str(), |s| s.trim_start_matches('@'));
        (!sid.is_empty()).then(|| format!("@{sid}"))
    }
}

// ---------------------------------------------------------------------------
// The grammar
// ---------------------------------------------------------------------------

/// Which ledger `ledger` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerFile {
    /// The supervisor's approval ledger for one session; `None` is this one.
    Approvals(Option<String>),
    /// The disk journal.
    Disk,
    /// The live upgrade's ledger (`<state>/upgrade/ledger.jsonl`).
    Upgrade,
}

/// The owner's word on the live upgrade (`upgrade [<sid>] --status`, `upgrade
/// <sid> --now|--defer <dur>|--skip`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerAsk {
    /// Print the recorded upgrades; change nothing.
    Status,
    /// [`super::upgrade_drive::Ask::Now`].
    Now,
    /// [`super::upgrade_drive::Ask::Defer`], in seconds.
    Defer(u64),
    /// [`super::upgrade_drive::Ask::Skip`].
    Skip,
}

/// One parsed invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// `hook`, `statusline`, `install` or `uninstall` — RETIRED by decision
    /// "B" ([`RETIRED`]).
    Retired {
        /// The verb as typed.
        verb: String,
    },
    /// The usage view (design §5.2).
    Usage {
        /// The JSON form.
        json: bool,
    },
    /// The failure classifier over one session's screen (design §5.8).
    Limits {
        /// The session (`@s-…`); `None` is this one.
        sid: Option<String>,
        /// The JSON form.
        json: bool,
    },
    /// The disk report, and the one apply path (design §5.5).
    Disk {
        /// Build directories to consider. EMPTY means no `cargo-targets`
        /// rows: this command enumerates no workspace of its own.
        targets: Vec<PathBuf>,
        /// The ONE safelist class to act on. `None` is report-only.
        apply: Option<disk::Class>,
        /// The JSON form.
        json: bool,
    },
    /// The newest rows of one ledger.
    Ledger {
        /// Which file.
        file: LedgerFile,
        /// How many rows, newest last.
        count: usize,
        /// The JSON form (the rows verbatim, one object per line).
        json: bool,
    },
    /// UPGRADE LIVE CLAUDE CODE SESSIONS IN PLACE, cooperatively: announce,
    /// wait for the agent's READY and for its background work to finish,
    /// then end it with SIGTERM and resume the same conversation on the
    /// newer build in the same tab ([`super::upgrade_drive`]).
    Upgrade {
        /// Only this tab (`s-<hex>`); empty means every session.
        sid: String,
        /// Plan and print; type, signal and write nothing.
        dry_run: bool,
        /// The JSON form: one object per session.
        json: bool,
    },
    /// The owner's view of, and word on, the live upgrade: nothing is swept.
    UpgradeOwner {
        /// The tab (`s-<hex>`); empty (only with `Status`) means every one.
        sid: String,
        /// What the owner asks.
        ask: OwnerAsk,
        /// The JSON form.
        json: bool,
    },
    /// THE MODEL PRIORITY LIST of the live upgrade: print it, with each
    /// model's availability on the managed build, the target, and Claude
    /// Code's own recommendations; or `set` it (best first).
    UpgradeModels {
        /// `Some(list)`: replace the list with this comma-separated one.
        set: Option<String>,
    },
    /// Print [`USAGE`] and exit 0.
    Help,
}

/// Flags that are not part of a [`Cmd`] because they configure [`Env`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overrides {
    /// `--state`.
    pub state: Option<PathBuf>,
    /// `--utc-offset`.
    pub utc_offset_s: Option<i64>,
    /// `--config`.
    pub config: Option<PathBuf>,
    /// `--sock`.
    pub sock: Option<String>,
}

/// Parse `args` (the operands AFTER `harness`).
///
/// # Errors
///
/// An unknown or deleted subcommand, an unknown flag, a flag with no value,
/// or a non-numeric count.
pub fn parse(args: &[String]) -> Result<(Cmd, Overrides), String> {
    let mut over = Overrides::default();
    let mut rest: Vec<&str> = Vec::new();
    let mut json = false;
    let mut help = false;
    let mut apply: Option<disk::Class> = None;
    let mut dry_run = false;
    let mut owner: Vec<OwnerAsk> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let value = |i: usize, flag: &str| -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match a {
            "-h" | "--help" | "help" => help = true,
            "--json" => json = true,
            "--dry-run" => dry_run = true,
            "--status" => owner.push(OwnerAsk::Status),
            "--now" => owner.push(OwnerAsk::Now),
            "--skip" => owner.push(OwnerAsk::Skip),
            "--defer" => {
                let v = value(i, "--defer")?;
                owner.push(OwnerAsk::Defer(parse_defer(&v)?));
                i += 1;
            }
            "--config" => {
                over.config = Some(PathBuf::from(value(i, "--config")?));
                i += 1;
            }
            "--state" => {
                over.state = Some(PathBuf::from(value(i, "--state")?));
                i += 1;
            }
            "--sock" => {
                over.sock = Some(value(i, "--sock")?);
                i += 1;
            }
            "--utc-offset" => {
                let v = value(i, "--utc-offset")?;
                over.utc_offset_s =
                    Some(v.parse::<i64>().map_err(|_| {
                        format!("--utc-offset wants a number of seconds, not {v:?}")
                    })?);
                i += 1;
            }
            "--apply" => {
                let v = value(i, "--apply")?;
                apply = Some(disk::Class::parse(&v).ok_or_else(|| {
                    format!(
                        "--apply wants one of {}, not {v:?}",
                        disk::Class::ALL
                            .iter()
                            .map(|c| c.as_str())
                            .collect::<Vec<_>>()
                            .join(" | ")
                    )
                })?);
                i += 1;
            }
            other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
            other => rest.push(other),
        }
        i += 1;
    }
    if help {
        return Ok((Cmd::Help, over));
    }
    let Some(&verb) = rest.first() else {
        return Ok((Cmd::Help, over));
    };
    let sid_operand = |ops: &[&str]| -> Result<Option<String>, String> {
        match ops {
            [] => Ok(None),
            [s] if s.starts_with('@') && s.len() > 1 => Ok(Some((*s).to_string())),
            other => Err(format!("{verb} takes one @<sid> at most, not {other:?}")),
        }
    };
    let cmd = match verb {
        verb @ ("hook" | "statusline" | "install" | "uninstall") => Cmd::Retired {
            verb: verb.to_string(),
        },
        "usage" => {
            if rest.len() > 1 {
                return Err(format!("usage takes no operand, not {:?}", &rest[1..]));
            }
            Cmd::Usage { json }
        }
        "limits" => Cmd::Limits {
            sid: sid_operand(&rest[1..])?,
            json,
        },
        "disk" => Cmd::Disk {
            targets: rest[1..].iter().map(PathBuf::from).collect(),
            apply,
            json,
        },
        "ledger" => {
            let mut file = LedgerFile::Approvals(None);
            let mut count = LEDGER_DEFAULT_ROWS;
            for op in &rest[1..] {
                if *op == "disk" {
                    file = LedgerFile::Disk;
                } else if *op == "upgrade" {
                    file = LedgerFile::Upgrade;
                } else if op.starts_with('@') && op.len() > 1 {
                    file = LedgerFile::Approvals(Some((*op).to_string()));
                } else if let Ok(n) = op.parse::<usize>() {
                    count = n;
                } else {
                    return Err(format!(
                        "ledger takes `disk`, `upgrade`, an @<sid> and a row count, not {op:?}"
                    ));
                }
            }
            Cmd::Ledger { file, count, json }
        }
        "upgrade" if rest.get(1) == Some(&"models") => match (rest.get(2), rest.get(3)) {
            (None, None) => Cmd::UpgradeModels { set: None },
            (Some(&"set"), Some(list)) if rest.len() == 4 => Cmd::UpgradeModels {
                set: Some((*list).to_string()),
            },
            _ => {
                return Err(
                    "upgrade models takes nothing, or `set <id>,<id>,...` (best first)".to_string(),
                );
            }
        },
        "upgrade" => {
            let sid = rest.get(1).map_or(String::new(), |s| (*s).to_string());
            if !sid.is_empty() && !sid.starts_with("s-") {
                return Err(format!(
                    "upgrade takes a tab's session id (`s-<hex>`, as `aterm ctl sessions` \
                     prints it), not {sid:?}"
                ));
            }
            if rest.len() > 2 {
                return Err(format!(
                    "upgrade takes one tab at most, not {:?}",
                    &rest[1..]
                ));
            }
            match owner.as_slice() {
                [] => {}
                [ask] => {
                    if dry_run {
                        return Err(
                            "--status, --now, --defer and --skip sweep nothing: they take no \
                             --dry-run"
                                .to_string(),
                        );
                    }
                    if sid.is_empty() && *ask != OwnerAsk::Status {
                        return Err(
                            "--now, --defer and --skip are the owner's word on ONE tab: name it \
                             (`aterm harness upgrade s-<hex> --now`; `--status` lists them)"
                                .to_string(),
                        );
                    }
                    return Ok((
                        Cmd::UpgradeOwner {
                            sid,
                            ask: *ask,
                            json,
                        },
                        over,
                    ));
                }
                _ => {
                    return Err("one of --status, --now, --defer and --skip at a time".to_string());
                }
            }
            Cmd::Upgrade { sid, dry_run, json }
        }
        deleted if DELETED.contains(&deleted) => {
            // `config` was also where main's live upgrade kept its switch
            // (`cap.upgrade.enabled`) until the two met: say where it went.
            let upgrade = if deleted == "config" {
                " (the live upgrade's switch is `upgrade` in that table)"
            } else {
                ""
            };
            return Err(format!(
                "`harness {deleted}` is gone: the supervisor is `aterm drive watch|supervise` \
                 and aterm.toml's [harness] table is its policy{upgrade}"
            ));
        }
        other => return Err(format!("unknown subcommand {other:?}")),
    };
    if !owner.is_empty() {
        return Err("--status, --now, --defer and --skip belong to `upgrade`".to_string());
    }
    Ok((cmd, over))
}

/// The longest `--defer`: a month. Longer is a skip or the switch.
pub const UPGRADE_MAX_DEFER_S: u64 = 30 * 86_400;

/// `--defer`'s value: a count with a unit — `90s`, `30m`, `6h`, `2d` — or bare
/// seconds; more than zero and at most [`UPGRADE_MAX_DEFER_S`].
///
/// # Errors
///
/// Anything else, named.
pub fn parse_defer(v: &str) -> Result<u64, String> {
    let (digits, unit) = match v.find(|c: char| !c.is_ascii_digit()) {
        Some(at) => v.split_at(at),
        None => (v, "s"),
    };
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => 0,
    };
    let secs = digits
        .parse::<u64>()
        .ok()
        .filter(|_| scale > 0)
        .and_then(|n| n.checked_mul(scale))
        .ok_or_else(|| format!("--defer wants a span like 90s, 30m, 6h or 2d, not {v:?}"))?;
    if secs == 0 || secs > UPGRADE_MAX_DEFER_S {
        return Err(format!(
            "--defer {v} is outside 1s..30d: longer is `--skip`, or the `[harness] upgrade` switch"
        ));
    }
    Ok(secs)
}

// ---------------------------------------------------------------------------
// usage
// ---------------------------------------------------------------------------

/// The vendor's project directory name for a working directory: every byte
/// outside `[A-Za-z0-9-]` folded to `-` (MEASURED against `~/.claude/projects`).
#[must_use]
pub fn project_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// How many directory entries the transcript search will look at. A project
/// directory holds one file per session; this is the fence against a
/// directory that holds something else entirely.
pub const MAX_TRANSCRIPT_ENTRIES: usize = 4096;

/// The byte cap on the transcript this command folds. `fold_reader` streams,
/// so the cost is one pass and no line is ever held; rows past the cap are
/// not folded, and the fold's own counters say how much was read.
pub const MAX_TRANSCRIPT_BYTES: u64 = 256 * 1024 * 1024;

/// Fold the NEWEST transcript in this working directory's project directory,
/// with the path it read. `None` when there is no home, no project directory
/// or no `.jsonl` in it.
///
/// Newest-by-mtime is the only rung left. The two that outranked it — the
/// session a statusLine payload or a hook row NAMED — arrived on the hook
/// bridge decision "B" retired, so the answer is labelled
/// [`Source::TranscriptNewest`] and the text says what that costs.
fn fold_newest_transcript(env: &Env) -> Option<(PathBuf, usage::TranscriptUsage)> {
    let dir = env.transcripts_root()?.join(project_dir_name(&env.cwd));
    let path = newest_transcript(&dir)?;
    let file = std::fs::File::open(&path).ok()?;
    let mut fold = usage::TranscriptUsage::new();
    // A read error mid-file keeps what was folded before it: a partial spend
    // is a measurement of part of the session, labelled by the row counters.
    let _ = fold.fold_reader(std::io::BufReader::new(std::io::Read::take(
        file,
        MAX_TRANSCRIPT_BYTES,
    )));
    Some((path, fold))
}

/// The newest `.jsonl` in `dir` by modification time.
fn newest_transcript(dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .take(MAX_TRANSCRIPT_ENTRIES)
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let Ok(when) = meta.modified() else { continue };
        if newest.as_ref().is_none_or(|(have, _)| when > *have) {
            newest = Some((when, path));
        }
    }
    newest.map(|(_, path)| path)
}

/// One `{schema, kind, source, …extra, <kind>: body}` document.
fn sourced_json(
    kind: &str,
    source: Source,
    body: &str,
    extra: Vec<(&'static str, Value)>,
) -> String {
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("kind".to_owned(), Value::from(kind.to_owned()));
    o.insert("source".to_owned(), Value::from(source.as_str().to_owned()));
    for (key, value) in extra {
        o.insert(key.to_owned(), value);
    }
    o.insert(
        kind.to_owned(),
        aterm_json::from_str::<Value>(body).unwrap_or(Value::Null),
    );
    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
}

/// `aterm harness usage`.
fn run_usage(env: &Env, json: bool, out: &mut dyn Write) -> ExitCode {
    let mut view = UsageView::new(env.now);
    let folded = fold_newest_transcript(env);
    let source = if let Some((_, fold)) = &folded {
        let mut account = AccountView::new("account", true);
        account.add_transcript(fold, &usage::PriceTable::new());
        view.accounts.push(account);
        Source::TranscriptNewest
    } else {
        Source::None
    };
    if json {
        let extra = match &folded {
            Some((path, _)) => vec![
                ("transcript", Value::from(path.display().to_string())),
                ("transcript_pick", Value::from("newest-mtime".to_owned())),
            ],
            None => Vec::new(),
        };
        let _ = writeln!(
            out,
            "{}",
            sourced_json("usage", source, &usage::usage_json(&view), extra)
        );
        return ExitCode::SUCCESS;
    }
    let line = usage::hud_line(&view, env.utc_offset_s);
    let _ = writeln!(out, "{line}  [source={}]", source.as_str());
    match &folded {
        Some((path, fold)) => {
            let _ = writeln!(
                out,
                "spend from {} ({} assistant rows), the newest transcript for this directory; \
                 if two Claude Code sessions share the directory, it may be the other one's",
                path.display(),
                fold.assistant_rows,
            );
        }
        None => {
            let _ = writeln!(
                out,
                "no transcript for this directory under ~/.claude/projects — nothing to fold"
            );
        }
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// limits
// ---------------------------------------------------------------------------

/// What ONE screen read says about a limit: the wall the worker's last turn
/// ended on, read by the engine's own reader ([`aterm_phase::wall`], the one
/// wall classifier — the turn-end policy decides on the same reading), and
/// the windows the vendor painted on its `/usage` panel
/// ([`usage::usage_panel_windows`]). Every reset is placed on the clock by
/// the supervisor's grammar ([`limit::parse_reset`], [`limit::reset_at`]).
#[derive(Debug, Clone, PartialEq)]
pub struct LimitsView {
    /// The wall, when the screen shows one.
    pub wall: Option<aterm_phase::Wall>,
    /// The wall's reset as Unix seconds, when its words place one.
    pub resets_at: Option<i64>,
    /// Each painted window, with its reset placed.
    pub windows: Vec<(usage::PanelWindow, Option<i64>)>,
}

/// [`LimitsView`] of `rows` as of `now`: a reset that names no zone (or one
/// `zone_offset` does not know) is at `local_offset_s`.
#[must_use]
pub fn limits_view(
    rows: &[String],
    now: i64,
    local_offset_s: i64,
    zone_offset: fn(&str) -> Option<i64>,
) -> LimitsView {
    let place = |text: &str| {
        limit::parse_reset(text)
            .map(|spec| limit::reset_at(&spec, now, local_offset_s, zone_offset))
    };
    let wall = aterm_phase::wall(rows);
    let resets_at = wall
        .as_ref()
        .and_then(|w| w.reset.as_deref())
        .and_then(place);
    let windows = usage::usage_panel_windows(rows)
        .into_iter()
        .map(|w| {
            let at = w.reset_text.as_deref().and_then(place);
            (w, at)
        })
        .collect();
    LimitsView {
        wall,
        resets_at,
        windows,
    }
}

/// The `harness limits` line: `wall=<kind> resets_at=<rfc3339|-> source=grid
/// message=<words> windows=<name>=<pct>%@<rfc3339|->,…`, or `wall=none …`.
#[must_use]
pub fn limits_text(view: &LimitsView) -> String {
    let at = |r: Option<i64>| r.map_or("-".to_string(), rfc3339_utc);
    let windows = if view.windows.is_empty() {
        "-".to_string()
    } else {
        view.windows
            .iter()
            .map(|(w, r)| format!("{}={}%@{}", w.name, w.used_pct, at(*r)))
            .collect::<Vec<_>>()
            .join(",")
    };
    match &view.wall {
        None => format!(
            "wall=none source={} windows={windows} — no wall on the screen",
            Source::Grid.as_str()
        ),
        Some(w) => format!(
            "wall={} resets_at={} source={} windows={windows} message={}",
            w.kind.name(),
            at(view.resets_at),
            Source::Grid.as_str(),
            super::one_line(&w.message, usize::MAX),
        ),
    }
}

/// The `harness limits --json` document, schema 2: `{schema, kind:"limits",
/// source, wall: null | {kind, message, reset, resets_at}, windows: [{name,
/// used_pct, reset, resets_at}]}`. (Schema 1 was the deleted classifier's
/// `class`/`unpaired`/`storm` verdict.)
#[must_use]
pub fn limits_json(view: &LimitsView) -> String {
    let at = |r: Option<i64>| r.map_or(Value::Null, |r| Value::from(rfc3339_utc(r)));
    let text = |t: Option<&str>| t.map_or(Value::Null, |t| Value::from(t.to_owned()));
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(2u64));
    o.insert("kind".to_owned(), Value::from("limits".to_owned()));
    o.insert(
        "source".to_owned(),
        Value::from(Source::Grid.as_str().to_owned()),
    );
    let wall = view.wall.as_ref().map_or(Value::Null, |w| {
        let mut m = Map::new();
        m.insert("kind".to_owned(), Value::from(w.kind.name().to_owned()));
        m.insert("message".to_owned(), Value::from(w.message.clone()));
        m.insert("reset".to_owned(), text(w.reset.as_deref()));
        m.insert("resets_at".to_owned(), at(view.resets_at));
        Value::Object(m)
    });
    o.insert("wall".to_owned(), wall);
    let windows = view
        .windows
        .iter()
        .map(|(w, r)| {
            let mut m = Map::new();
            m.insert("name".to_owned(), Value::from(w.name.clone()));
            m.insert("used_pct".to_owned(), Value::from(u64::from(w.used_pct)));
            m.insert("reset".to_owned(), text(w.reset_text.as_deref()));
            m.insert("resets_at".to_owned(), at(*r));
            Value::Object(m)
        })
        .collect();
    o.insert("windows".to_owned(), Value::Array(windows));
    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
}

/// `aterm harness limits` through `ctl`: one read, one verdict.
/// `local_offset_s` places a reset the notice names without a zone.
fn run_limits_with<C: Ctl>(
    ctl: &mut C,
    env: &Env,
    sid: Option<&str>,
    local_offset_s: i64,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    // NO SESSION NAMED AND NONE TO BE IN: refuse, rather than send an
    // unaddressed read that classifies whichever session the socket defaults
    // to and print a verdict that does not say whose it is.
    let Some(target) = env.target(sid) else {
        let _ = writeln!(err, "aterm harness limits: no session named; pass @<sid>");
        return ExitCode::from(2);
    };
    match Session::new(ctl, Some(target.clone())).read_screen() {
        Ok(screen) => {
            let view = limits_view(&screen.rows, env.now, local_offset_s, limit::zone_offset_s);
            let text = if json {
                limits_json(&view)
            } else {
                limits_text(&view)
            };
            let _ = writeln!(out, "{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = writeln!(
                err,
                "aterm harness limits: cannot read the screen of {target}: {e}"
            );
            ExitCode::from(1)
        }
    }
}

/// `aterm harness limits`, over the resolved control socket.
fn run_limits(
    env: &Env,
    sid: Option<&str>,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let endpoint = match &env.sock {
        Some(sock) => Endpoint::Socket(sock.clone()),
        None => Endpoint::Resolved {
            self_sid: (!env.sid.is_empty()).then(|| env.sid.clone()),
        },
    };
    let mut ctl = RelayCtl::new(endpoint, None);
    // A reset the notice names with no zone is the machine's local time, the
    // clock the vendor painted it in — the engine places it the same way.
    run_limits_with(&mut ctl, env, sid, limit::tz_offset_s(), json, out, err)
}

// ---------------------------------------------------------------------------
// disk
// ---------------------------------------------------------------------------

fn default_store_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("pkg")
            .join("store")
    } else {
        home.join(".local")
            .join("share")
            .join("aterm")
            .join("pkg")
            .join("store")
    }
}

/// Where `disk` looks. Every path is derived HERE and handed to
/// [`disk::scan`], which reads no environment of its own.
fn disk_roots(env: &Env, targets: &[PathBuf]) -> disk::Roots {
    let home = env.home.clone();
    disk::Roots {
        volume: Some(env.cwd.clone()),
        versions_dir: home.as_ref().map(|h| {
            h.join(".local")
                .join("share")
                .join("claude")
                .join("versions")
        }),
        live_link: home
            .as_ref()
            .map(|h| h.join(".local").join("bin").join("claude")),
        store_dir: home.as_ref().map(|h| default_store_dir(h)),
        targets: targets.to_vec(),
        transcripts: home.as_ref().map(|h| h.join(".claude").join("projects")),
    }
}

/// The `[disk]` knobs, all three, read from `aterm.toml` through
/// [`toml_bool`] and [`toml_int`]. A value of the wrong type — and a NEGATIVE stale window,
/// which would make every directory a candidate at once — falls back to the
/// shipped default rather than to a guess.
fn disk_config(env: &Env) -> disk::Config {
    let text = env
        .config
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let d = disk::Config::default();
    disk::Config {
        apply: toml_bool(&text, "disk", "apply").unwrap_or(disk::DEFAULT_APPLY),
        warn_free_gib: toml_int(&text, "disk", "warn_free_gib")
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(d.warn_free_gib),
        target_stale_days: toml_int(&text, "disk", "target_stale_days")
            .filter(|v| *v >= 0)
            .unwrap_or(d.target_stale_days),
    }
}

/// `aterm harness disk` (design §5.5). Report-only unless a class is named
/// AND `disk.apply` is on; every refusal becomes a denial row.
fn run_disk(
    env: &Env,
    targets: &[PathBuf],
    class: Option<disk::Class>,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let roots = disk_roots(env, targets);
    let config = disk_config(env);
    let free = roots.volume.as_deref().and_then(disk::free_bytes);
    let survey = disk::scan(&roots, env.now, disk::Trigger::OnDemand, config, free);
    let rep = disk::report(&survey, config);
    let sid = (!env.sid.is_empty()).then_some(env.sid.as_str());
    let path = env.disk_ledger();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = bound_ledger(&path, DISK_LEDGER_MAX_BYTES, DISK_LEDGER_KEEP_ROWS) {
        let _ = writeln!(
            err,
            "aterm harness disk: cannot cut {} back: {e}",
            path.display()
        );
    }
    let mut journal = Journal::open(Some(&path), sid, err);
    journal.append_raw(&disk::report_row(env.now, &env.sid, &rep), err);

    let done = class.map(|_| {
        disk::apply(
            &rep,
            survey.transcripts_root.as_deref(),
            class,
            &mut disk::remove_tree,
        )
    });
    if let Some(done) = &done {
        for row in &done.removed {
            journal.append_raw(&disk::removal_row(env.now, &env.sid, row), err);
        }
        for refusal in &done.denials {
            journal.append_raw(&disk::denial_row(env.now, &env.sid, refusal), err);
        }
    }

    if json {
        let _ = writeln!(out, "{}", rep.to_json());
        if let Some(done) = &done {
            let mut o = Map::new();
            o.insert("schema".to_owned(), Value::from(1u64));
            o.insert("kind".to_owned(), Value::from("disk-apply".to_owned()));
            o.insert(
                "removed".to_owned(),
                Value::from(u64::try_from(done.removed.len()).unwrap_or(u64::MAX)),
            );
            o.insert("freed_bytes".to_owned(), Value::from(done.freed_bytes));
            o.insert(
                "denials".to_owned(),
                Value::Array(
                    done.denials
                        .iter()
                        .map(|d| Value::from(d.describe()))
                        .collect(),
                ),
            );
            let _ = writeln!(
                out,
                "{}",
                aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
            );
        }
        return ExitCode::SUCCESS;
    }

    let _ = writeln!(out, "{}", rep.headline());
    for row in &rep.rows {
        let _ = writeln!(out, "  {}", row.line());
    }
    for note in &rep.notes {
        let _ = writeln!(out, "  note: {note}");
    }
    if rep.rows.is_empty() {
        let _ = writeln!(out, "  nothing to reclaim");
    }
    match &done {
        None => {
            // `describe` already opens with "report only:" (measured live: the
            // line used to print it twice).
            let _ = writeln!(out, "{}", disk::Refusal::NoClass.describe());
        }
        Some(done) => {
            let _ = writeln!(out, "{}", done.headline());
            for row in &done.removed {
                let _ = writeln!(
                    out,
                    "  removed {} — {}",
                    row.path.display(),
                    row.witness.describe()
                );
            }
            for refusal in &done.denials {
                let _ = writeln!(out, "  denied: {}", refusal.describe());
            }
        }
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// ledger
// ---------------------------------------------------------------------------

/// The newest `max` lines of the JSONL file at `path`, streamed through a ring
/// of at most [`READ_MAX_ROWS`] so the file is never held whole. A missing
/// file is no rows; a line that is not UTF-8 is skipped.
///
/// # Errors
///
/// The file exists and cannot be read.
pub fn tail_lines(path: &Path, max: usize) -> io::Result<Vec<String>> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let keep = max.min(READ_MAX_ROWS);
    let mut ring: VecDeque<String> = VecDeque::with_capacity(keep);
    if keep == 0 {
        return Ok(Vec::new());
    }
    for line in io::BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        if ring.len() == keep {
            ring.pop_front();
        }
        ring.push_back(line);
    }
    Ok(ring.into_iter().collect())
}

/// Cut the journal at `path` back to its newest `keep` rows once it is larger
/// than `max_bytes`, so it stays within `max_bytes` plus one run's rows. The
/// rows are streamed through [`tail_lines`]' ring (never read whole), written
/// to a sibling `.tmp` file (`0600`) and renamed over the journal, so a reader
/// sees the old file or the cut one and never half of either. A missing or
/// small file is left alone. Two `disk` runs cutting at once can lose the
/// rows the other appended between its read and its rename; each run's own
/// report row is appended after its cut.
pub fn bound_ledger(path: &Path, max_bytes: u64, keep: usize) -> io::Result<bool> {
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if len <= max_bytes {
        return Ok(false);
    }
    let rows = tail_lines(path, keep)?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let mut o = std::fs::OpenOptions::new();
    o.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = io::BufWriter::new(o.open(&tmp)?);
    for row in &rows {
        f.write_all(row.as_bytes())?;
        f.write_all(b"\n")?;
    }
    f.into_inner()
        .map_err(io::IntoInnerError::into_error)?
        .sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// The ledger file a [`LedgerFile`] names, or why there is none.
fn ledger_path(env: &Env, file: &LedgerFile) -> Result<PathBuf, String> {
    match file {
        LedgerFile::Disk => Ok(env.disk_ledger()),
        LedgerFile::Upgrade => Ok(super::upgrade_drive::ledger_path(&upgrade_opts(
            env,
            "",
            &SupervisorConfig::default(),
        )?)),
        LedgerFile::Approvals(sid) => {
            let state = env
                .aterm_state
                .as_ref()
                .ok_or_else(|| "the aterm state root is unknown".to_string())?;
            let sid = sid
                .as_deref()
                .map(|s| s.trim_start_matches('@'))
                .or_else(|| (!env.sid.is_empty()).then_some(env.sid.as_str()));
            approvals::path_under(state, sid)
                .ok_or_else(|| format!("{}/drive cannot be opened", state.display()))
        }
    }
}

/// One approval row as a line a person reads: time, decision, rule, command.
fn approval_line(row: &str) -> String {
    let Ok(doc) = aterm_json::from_str::<Value>(row) else {
        return row.to_string();
    };
    let s = |k: &str| doc.get(k).and_then(Value::as_str).unwrap_or("-");
    let ts = doc
        .get("ts")
        .and_then(Value::as_i64)
        .map_or_else(|| "-".to_string(), |ms| rfc3339_utc(ms / 1000));
    let command = super::one_line(s("command"), 160);
    let reason = s("reason");
    if reason.is_empty() || reason == "-" {
        format!("{ts} {} {} {command}", s("decision"), s("rule_id"))
    } else {
        format!(
            "{ts} {} {} {command} — {}",
            s("decision"),
            s("rule_id"),
            super::one_line(reason, 160)
        )
    }
}

/// One upgrade ledger row as a line a person reads: time, step, tab and
/// conversation, from and to, then the detail (the marker, the relaunch line,
/// the outcome, the reason).
fn upgrade_ledger_line(row: &str) -> String {
    let Ok(doc) = aterm_json::from_str::<Value>(row) else {
        return row.to_string();
    };
    let s = |k: &str| doc.get(k).and_then(Value::as_str).unwrap_or("-");
    let ts = doc
        .get("t")
        .and_then(Value::as_i64)
        .map_or_else(|| "-".to_string(), rfc3339_utc);
    let detail = super::one_line(s("detail"), 200);
    let head = format!(
        "{ts} {} tab={} session={} {} -> {}",
        s("step"),
        s("tab"),
        s("session"),
        s("from"),
        s("to")
    );
    if detail.is_empty() {
        head
    } else {
        format!("{head} — {detail}")
    }
}

/// `aterm harness ledger`.
fn run_ledger(
    env: &Env,
    file: &LedgerFile,
    count: usize,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let path = match ledger_path(env, file) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(err, "aterm harness ledger: {e}");
            return ExitCode::from(1);
        }
    };
    let rows = match tail_lines(&path, count) {
        Ok(rows) => rows,
        Err(e) => {
            let _ = writeln!(err, "aterm harness ledger: {}: {e}", path.display());
            return ExitCode::from(1);
        }
    };
    for row in &rows {
        match (json, file) {
            (true, _) | (false, LedgerFile::Disk) => {
                let _ = writeln!(out, "{row}");
            }
            (false, LedgerFile::Upgrade) => {
                let _ = writeln!(out, "{}", upgrade_ledger_line(row));
            }
            (false, LedgerFile::Approvals(_)) => {
                let _ = writeln!(out, "{}", approval_line(row));
            }
        }
    }
    if rows.is_empty() && !json {
        let _ = writeln!(
            out,
            "no rows in {} — nothing has been decided there yet",
            path.display()
        );
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// upgrade
// ---------------------------------------------------------------------------

/// The step a hand-run `upgrade` answers while `[harness] enabled` reads off
/// ([`upgrade_pass`]): the verdict word the deleted `switch` and `watch` verbs
/// answered under the same switch, kept so a script that keyed on it still
/// reads it.
pub const UPGRADE_BYPASSED: &str = "refused:bypassed";

/// `aterm harness upgrade models [set <list>]` — the live upgrade's model
/// priority list ([`super::upgrade_models`]).
fn run_upgrade_models(
    env: &Env,
    set: Option<&str>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let Some(home) = env.home.clone() else {
        let _ = writeln!(
            err,
            "aterm harness: upgrade models needs the home directory"
        );
        return ExitCode::from(2);
    };
    let opts = super::upgrade_drive::Opts {
        home,
        state: env.state.clone(),
        sock: None,
        only_sid: None,
        dry_run: set.is_none(),
        // Reads the list, or writes it: no tab is looked at or typed into.
        human_grace_s: 0,
        hand_back: false,
        background: false,
    };
    match set {
        Some(list) => match super::upgrade_drive::set_models(&opts, list) {
            Ok(ids) => {
                let _ = writeln!(out, "models (best first): {}", ids.join(", "));
                ExitCode::SUCCESS
            }
            Err(e) => {
                let _ = writeln!(err, "aterm harness: {e}");
                ExitCode::from(2)
            }
        },
        None => {
            let _ = write!(out, "{}", super::upgrade_drive::models_report(&opts));
            ExitCode::SUCCESS
        }
    }
}

/// `aterm harness upgrade` — sweep every live Claude Code session once and
/// move each one step toward the newer build
/// ([`super::upgrade_drive::sweep`]). A session that is waiting is a line,
/// not a failure; a sweep that did not RUN is a line AND an exit code
/// ([`upgrade_pass`] says which). There is no loop: the window's host takes
/// the same steps at each session's idle points, on atpkg's activation
/// notice, so a script that wants them repeated runs this again.
fn run_upgrade(
    env: &Env,
    sid: &str,
    dry_run: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let (policy, notes) = SupervisorConfig::from_path(env.config.as_deref());
    let opts = match upgrade_opts(env, sid, &policy) {
        Ok(opts) => super::upgrade_drive::Opts { dry_run, ..opts },
        Err(e) => {
            let _ = writeln!(err, "aterm harness: {e}");
            return ExitCode::from(2);
        }
    };
    let code = upgrade_pass(env, &opts, (&policy, &notes), json, out, err);
    let _ = out.flush();
    code
}

/// A hand-run pass's options for `env`, scoped to `sid` when it names one,
/// under `policy`'s `[harness] human_grace_s`. No loop runs behind a hand-run
/// pass, so it carries a relaunched agent on itself (`hand_back: false`).
fn upgrade_opts(
    env: &Env,
    sid: &str,
    policy: &SupervisorConfig,
) -> Result<super::upgrade_drive::Opts, String> {
    let home = env
        .home
        .clone()
        .ok_or("upgrade needs the home directory (Claude's sessions live under it)")?;
    Ok(super::upgrade_drive::Opts {
        home,
        state: env.state.clone(),
        sock: env.sock.clone(),
        only_sid: (!sid.is_empty()).then(|| sid.to_string()),
        dry_run: false,
        human_grace_s: policy.human_grace_s,
        hand_back: false,
        background: false,
    })
}

/// `aterm harness upgrade [<sid>] --status` and `upgrade <sid>
/// --now|--defer|--skip` ([`OwnerAsk`]): the recorded upgrades read, or the
/// owner's word written ([`super::upgrade_drive::ask`]). Nothing is swept,
/// typed or signalled. A word the busy lock kept out exits 75, like a step
/// that could not run; any other refusal exits 1. With either switch the
/// window's host reads off (the one `[harness]` reader,
/// [`SupervisorConfig::from_path`]) the word is still written — it is the
/// owner's — and stderr says what it waits on: `[harness] enabled` off,
/// nothing moves until it is back on; `[harness] upgrade` off, the window
/// takes no upgrade step, so the word takes effect at the next hand-run
/// `aterm harness upgrade <sid>` or once the switch is back on (review of
/// 2026-09-25: only `enabled` was read, and under `upgrade = false` the word
/// printed `request=now`, exited 0 and said nothing, while nothing would ever
/// read it).
fn run_upgrade_owner(
    env: &Env,
    sid: &str,
    ask: OwnerAsk,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let (policy, _) = SupervisorConfig::from_path(env.config.as_deref());
    let opts = match upgrade_opts(env, sid, &policy) {
        Ok(opts) => opts,
        Err(e) => {
            let _ = writeln!(err, "aterm harness: {e}");
            return ExitCode::from(2);
        }
    };
    let now = u64::try_from(env.now).unwrap_or(0);
    let print = |out: &mut dyn Write, row: &super::upgrade_drive::Row| {
        let _ = if json {
            writeln!(
                out,
                "{}",
                aterm_json::to_string(&row.to_json(now)).unwrap_or_default()
            )
        } else {
            writeln!(out, "{}", row.line(now))
        };
    };
    let word = match ask {
        OwnerAsk::Status => {
            let (rows, vetted) = super::upgrade_drive::status_rows(&opts);
            let rows: Vec<_> = rows
                .into_iter()
                .filter(|r| r.phase != super::upgrade::Phase::Done)
                .collect();
            for row in &rows {
                print(out, row);
            }
            if !vetted {
                let _ = writeln!(
                    err,
                    "aterm harness: Claude's session files could not be read whole, so stale \
                     upgrades may be listed"
                );
            }
            if rows.is_empty() && !json {
                let _ = writeln!(
                    out,
                    "no upgrade is recorded{}",
                    if sid.is_empty() {
                        String::new()
                    } else {
                        format!(" for tab {sid}")
                    }
                );
            }
            return ExitCode::SUCCESS;
        }
        OwnerAsk::Now => super::upgrade_drive::Ask::Now,
        OwnerAsk::Defer(secs) => super::upgrade_drive::Ask::Defer(secs),
        OwnerAsk::Skip => super::upgrade_drive::Ask::Skip,
    };
    match super::upgrade_drive::ask(&opts, sid, word) {
        Ok(row) => {
            print(out, &row);
            // `--now` is read only after the terminal check, so an agent in a
            // pane is not moved by it: say so, with what does move it.
            if ask == OwnerAsk::Now
                && row.remedy(now) == Some(super::upgrade_drive::Remedy::InItsPane)
            {
                let _ = writeln!(
                    err,
                    "aterm harness: upgrade {sid}: recorded, but its agent runs under {}, which \
                     typing into the tab does not reach — quit it there and resume it in the tab",
                    super::upgrade_drive::runs_under(
                        row.wait.strip_prefix("terminal:").unwrap_or(&row.wait)
                    )
                );
            }
            if let Some(path) = env
                .config
                .as_deref()
                .filter(|_| !(policy.enabled && policy.upgrade))
            {
                let _ = if policy.enabled {
                    writeln!(
                        err,
                        "aterm harness: the word is recorded, but `[harness] upgrade` reads off \
                         in {}, so it takes effect at the next `aterm harness upgrade {sid}`",
                        path.display()
                    )
                } else {
                    writeln!(
                        err,
                        "aterm harness: the word is recorded, but `[harness] enabled` reads off \
                         in {} (Settings ▸ Harness): nothing moves until it is back on",
                        path.display()
                    )
                };
            }
            ExitCode::SUCCESS
        }
        Err(e) if e.starts_with("busy:") => {
            let _ = writeln!(
                err,
                "aterm harness: upgrade {sid}: a sweep holds the lock ({e}); nothing was written, \
                 try again"
            );
            ExitCode::from(atpkg::lock::CONTENDED_EXIT)
        }
        Err(e) => {
            let _ = writeln!(err, "aterm harness: upgrade {sid}: {e}");
            ExitCode::from(1)
        }
    }
}

/// ONE pass of `upgrade`: the master switch, then the sweep, every report
/// printed — and the exit code it owes for it.
///
/// THE MASTER SWITCH GATES A HAND-RUN SWEEP. With aterm.toml's
/// `[harness] enabled` reading off
/// ([`crate::supervise::SupervisorConfig::from_path`], the reader the window's
/// host reads the same table with) the pass answers one
/// [`UPGRADE_BYPASSED`] line, takes no lock, types and signals nothing, and
/// exits 1; the window's host stands down on the same switch. Until
/// 2026-09-24 a hand-run sweep did not read the switch at all: with the
/// harness switched off it still typed its notices, sent SIGTERM and
/// relaunched. `[harness] upgrade` does NOT gate it — naming the verb is that
/// consent; it is the window's own switch. A
/// `--dry-run` acts on nothing, so it still sweeps and prints — after the
/// refusal line, with a sentence on stderr saying a real run would act on
/// none of it — and exits 0. The stderr sentence also names every restart an
/// earlier sweep left in flight ([`stranded`]), because standing down
/// strands it, and every note the reader has about the file (a `false` filed
/// under another table, a value it could not take).
///
/// A SWEEP THAT DID NOT RUN is not a success: it decided nothing about any
/// session. `busy:another-sweep` — another actor holds the lock, most often
/// the window's own host at a session's step — exits 75, atpkg's
/// "try again later" code for the same thing, a single-writer lock held by a
/// sibling ([`atpkg::lock::CONTENDED_EXIT`]), so a script retries by CODE
/// rather than reading the line. `busy:state-unwritable` and
/// `busy:lock-unopenable` exit 1: a state directory that cannot hold a lock
/// is not transient. All three exited 0 until 2026-09-24.
fn upgrade_pass(
    env: &Env,
    opts: &super::upgrade_drive::Opts,
    (policy, notes): (&SupervisorConfig, &[String]),
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let off = env.config.as_deref().filter(|_| !policy.enabled);
    if let Some(path) = off {
        upgrade_line(
            out,
            json,
            &super::upgrade_drive::Report {
                pid: 0,
                tab: "-".to_string(),
                session: "-".to_string(),
                from: "-".to_string(),
                to: "-".to_string(),
                step: UPGRADE_BYPASSED.to_string(),
            },
        );
        let _ = writeln!(
            err,
            "aterm harness: upgrade refused: `[harness] enabled` reads off in {} (Settings ▸ \
             Harness){}{}",
            path.display(),
            stranded(&super::upgrade_drive::in_flight(opts)),
            if opts.dry_run {
                "; the lines below are what it would do with the switch on"
            } else {
                ""
            }
        );
        for note in notes {
            let _ = writeln!(err, "aterm harness: {note}");
        }
        if !opts.dry_run {
            return ExitCode::from(1);
        }
    }
    let mut code = ExitCode::SUCCESS;
    // A dry run says how long each session has been behind and waiting, as
    // the last real sweep recorded it (a dry run records nothing itself).
    let recorded = if opts.dry_run {
        super::upgrade_drive::rows(opts)
    } else {
        Vec::new()
    };
    let now = u64::try_from(env.now).unwrap_or(0);
    for r in super::upgrade_drive::sweep(opts) {
        let row = recorded.iter().find(|row| row.session == r.session);
        upgrade_line_with(out, json, &r, row.map(|row| (row, now)));
        match r.step.strip_prefix("busy:") {
            Some("another-sweep") => code = ExitCode::from(atpkg::lock::CONTENDED_EXIT),
            Some(_) => code = ExitCode::from(1),
            None => {}
        }
    }
    code
}

/// What a refused `upgrade` owes about the restarts it leaves where they are
/// ([`super::upgrade_drive::in_flight`]): an agent an earlier sweep already
/// signalled is not relaunched while the switch is off — by this sweep, or
/// by the window's host, which stands down on the same switch — and the
/// refusal says so rather than leaving a conversation silently stopped.
fn stranded(sessions: &[String]) -> String {
    match sessions {
        [] => String::new(),
        [one] => format!(
            "; the restart of session {one} an earlier sweep began stays stopped until the \
             switch is back on"
        ),
        many => format!(
            "; the {} restarts an earlier sweep began (sessions {}) stay stopped until the \
             switch is back on",
            many.len(),
            many.join(", ")
        ),
    }
}

/// One `upgrade` report, in the text or the JSON form.
fn upgrade_line(out: &mut dyn Write, json: bool, r: &super::upgrade_drive::Report) {
    upgrade_line_with(out, json, r, None);
}

/// [`upgrade_line`], with a dry run's `pending_for=`/`wait_for=` from the
/// session's recorded upgrade when there is one (`-` for no recorded wait).
fn upgrade_line_with(
    out: &mut dyn Write,
    json: bool,
    r: &super::upgrade_drive::Report,
    recorded: Option<(&super::upgrade_drive::Row, u64)>,
) {
    let ages = recorded.map(|(row, now)| {
        (
            now.saturating_sub(row.behind_since),
            (!row.wait.is_empty()).then(|| now.saturating_sub(row.wait_since)),
        )
    });
    if !json {
        let _ = match ages {
            Some((behind, wait)) => writeln!(
                out,
                "{} pending_for={} wait_for={}",
                r.line(),
                super::upgrade::span(behind),
                wait.map_or_else(|| "-".to_string(), super::upgrade::span)
            ),
            None => writeln!(out, "{}", r.line()),
        };
        return;
    }
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("kind".to_owned(), Value::from("upgrade".to_owned()));
    o.insert("pid".to_owned(), Value::from(r.pid));
    for (k, v) in [
        ("tab", &r.tab),
        ("session", &r.session),
        ("from", &r.from),
        ("to", &r.to),
        ("step", &r.step),
    ] {
        o.insert(k.to_owned(), Value::from(v.clone()));
    }
    if let Some((behind, wait)) = ages {
        o.insert("pending_for_s".to_owned(), Value::from(behind));
        o.insert(
            "wait_for_s".to_owned(),
            wait.map_or(Value::Null, Value::from),
        );
    }
    let _ = writeln!(
        out,
        "{}",
        aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
    );
}

// ---------------------------------------------------------------------------
// The retired verbs, and the router
// ---------------------------------------------------------------------------

/// The front door's answer to a retired verb, with stdin injected. See
/// [`is_tombstone`]: from a pipe `hook`/`statusline` drain the payload to its
/// end and say nothing; typed at a terminal they read nothing and say, on one
/// stderr line, that they are retired. Both exit 0.
fn answer_retired(
    verb: &str,
    stdin_is_tty: bool,
    stdin: &mut dyn io::Read,
    err: &mut dyn Write,
) -> ExitCode {
    if is_tombstone(verb) {
        if stdin_is_tty {
            let _ = writeln!(err, "aterm harness {verb}: retired; it does nothing");
        } else {
            let _ = io::copy(stdin, &mut io::sink());
        }
    }
    run_retired(verb, err)
}

/// `hook`/`statusline` (a silent success) or `install`/`uninstall` (the
/// [`RETIRED`] refusal, exit 2). See [`Cmd::Retired`].
fn run_retired(verb: &str, err: &mut dyn Write) -> ExitCode {
    if is_tombstone(verb) {
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(err, "aterm harness {verb}: retired: {RETIRED}");
    ExitCode::from(2)
}

/// The retired verbs something an older build installed may still RUN: the
/// bridge script `install` wrote calls `harness hook` and `harness statusline`
/// with the vendor's payload on stdin. They answer exit 0 with nothing on
/// stdout, which every hook event reads as "no opinion" and the footer as an
/// empty line — never the usage error (exit 2) that Claude Code reads as a
/// block.
fn is_tombstone(verb: &str) -> bool {
    matches!(verb, "hook" | "statusline")
}

/// Route one parsed command. Injected writers so the tests drive this directly.
pub fn run(cmd: &Cmd, env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    match cmd {
        Cmd::Help => {
            let _ = out.write_all(USAGE.as_bytes());
            ExitCode::SUCCESS
        }
        Cmd::Retired { verb } => run_retired(verb, err),
        Cmd::Usage { json } => run_usage(env, *json, out),
        Cmd::Limits { sid, json } => run_limits(env, sid.as_deref(), *json, out, err),
        Cmd::Disk {
            targets,
            apply,
            json,
        } => run_disk(env, targets, *apply, *json, out, err),
        Cmd::Ledger { file, count, json } => run_ledger(env, file, *count, *json, out, err),
        Cmd::Upgrade { sid, dry_run, json } => run_upgrade(env, sid, *dry_run, *json, out, err),
        Cmd::UpgradeOwner { sid, ask, json } => run_upgrade_owner(env, sid, *ask, *json, out, err),
        Cmd::UpgradeModels { set } => run_upgrade_models(env, set.as_deref(), out, err),
    }
}

/// The front door's entry point (`aterm harness …`).
///
/// The retired verbs answer FIRST, before any parsing and before the
/// environment is read, so no argv or environment an old install runs them
/// with can turn their answer into an error: `hook` and `statusline` drain
/// stdin to its end and exit 0 printing nothing; `install` and `uninstall`
/// refuse with [`RETIRED`], exit 2. See [`answer_retired`].
pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    use std::io::IsTerminal as _;
    let args: Vec<String> = argv
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Some(verb) = args.first().map(String::as_str)
        && matches!(verb, "hook" | "statusline" | "install" | "uninstall")
    {
        let stdin = io::stdin();
        let tty = stdin.is_terminal();
        return answer_retired(verb, tty, &mut stdin.lock(), &mut io::stderr().lock());
    }
    let (cmd, over) = match parse(&args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("aterm harness: {e}; run `aterm harness --help`");
            return ExitCode::from(2);
        }
    };
    let mut env = match Env::from_process() {
        Ok(env) => env,
        Err(e) => {
            eprintln!("aterm harness: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(state) = over.state {
        env.state = state;
    }
    if let Some(offset) = over.utc_offset_s {
        env.utc_offset_s = offset;
    }
    if let Some(config) = over.config {
        env.config = Some(config);
    }
    if let Some(sock) = over.sock {
        env.sock = Some(sock);
    }
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    run(&cmd, &env, &mut out, &mut err)
}

#[path = "cli_tests.rs"]
#[cfg(test)]
mod tests;
