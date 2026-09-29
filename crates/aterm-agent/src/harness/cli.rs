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
//! * `usage` folds THIS session's transcripts — the one Claude Code names in
//!   the environment it gives its tools (`CLAUDE_CODE_SESSION_ID`, else
//!   `CLAUDE_PID`'s sessions file), else the newest in this working
//!   directory, said so — and its subagents', through the same incremental
//!   fold the window's footer reads ([`super::session_usage`]). A filesystem
//!   fact no hook is needed for.
//! * `limits` reads the session's SCREEN over the control socket (the one
//!   interface) through the engine's own readers: the wall the last turn
//!   ended on ([`aterm_phase::wall`], the one wall classifier — the engine's
//!   turn-end policy acts on the same reading) and the windows the vendor
//!   painted on its `/usage` panel ([`usage::usage_panel_windows`]), each
//!   reset placed by the supervisor's own clock ([`limit::parse_reset`]).
//! * `disk` measures, and removes only a named safelist class under
//!   `disk.apply = true` — for build directories, an idle cargo profile's
//!   `incremental/` under cargo's own locks ([`disk::target`]), and below the
//!   automatic floor recent ones too, least recently used first, until free
//!   space is back (measured, or covered by the bytes released); every
//!   measurement, removal intent, removal and refusal is appended to
//!   `<state>/disk.jsonl` as it happens, cut back to its newest rows past
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
use super::footer;
use super::session_usage::{self, SessionUsage};
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

/// The durable config's home: aterm.toml — [`aterm_types::dirs::aterm_config_path`],
/// the rule `aterm-gui`'s `config_path` delegates to, so `disk` reads the same
/// `[disk]` table the window's Settings page writes. This was a hand copy of
/// the Unix arms (XDG, then `$HOME/.config`), so on Windows it read
/// `$HOME\.config\aterm\aterm.toml`, or nothing, while the window wrote
/// `%APPDATA%\aterm\aterm.toml` (review, 2026-09-27).
#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    aterm_types::dirs::aterm_config_path()
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
    aterm harness usage [--json]                 this session's tokens per model (design 5.2)
    aterm harness limits [@<sid>] [--json]       the wall and the painted /usage windows on the session's screen
    aterm harness disk [<build-dir> ...] [--apply <class>] [--json]
    aterm harness ledger [@<sid>] [<n>] [--json] the supervisor's approval ledger
    aterm harness ledger disk [<n>] [--json]     the disk journal
    aterm harness ledger upgrade [<n>] [--json]  the live upgrade's ledger: every act, and the owner's word
    aterm harness upgrade [<sid>] [--dry-run] [--json]
                                                 move live Claude Code sessions onto a newer build
                                                 and the newest model of their own family (else
                                                 up the model priority list), and
                                                 live Codex sessions (their daemon first) onto the
                                                 managed Codex
    aterm harness upgrade [<sid>] --status [--json]
                                                 each upgrade: how long behind, what it waits on
    aterm harness upgrade <sid> --now | --defer <dur> | --skip [--json]
                                                 the owner's word on one tab's upgrade
    aterm harness upgrade models [set <id>,<id>,...]
                                                 the model priority list: show it (availability,
                                                 what is on offer, the target, Claude Code's
                                                 recommendations) or set it

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
    --apply <class>     disk: the ONE safelist class to act on. atpkg-gc and
                        claude-purge are surfaced and never removed from here;
                        they name the command that owns them
    --dry-run           upgrade: print each session's next step, type and signal nothing
    --status            upgrade: print each recorded upgrade, sweep nothing
    --now               upgrade <sid>: move it at its first pause (the ladder's last rung)
    --defer <dur>       upgrade <sid>: not for <dur> (90s, 30m, 6h, 2d; at most 30d)
    --skip              upgrade <sid>: stay on the running build until a newer target comes
    --json              the JSON form
    -h, --help          this text

`usage` folds THIS session's transcripts — its transcript and its subagents'
(<session>/subagents/**/agent-*.jsonl) — per model. Run from a Claude Code session's own
tools (they inherit CLAUDE_CODE_SESSION_ID, or CLAUDE_PID), it folds that session's own
file, named by the vendor; a named session with no transcript yet folds nothing. Anywhere
else — aterm's shells carry no CLAUDE_* — it folds the NEWEST transcript in this working
directory's project directory (<claude dir>/projects/<dir>/*.jsonl, the claude dir being
CLAUDE_CONFIG_DIR, else ~/.claude) and says so: two sessions in one directory share it,
so the newest may be the other one's; a relative CLAUDE_CONFIG_DIR folds nothing, said.
A streamed subagent message counts once, at its final figures. It reads up to a 4 GiB
cap and says PREFIX only when it hits it; a subagent transcript it could not count is
said (`complete` is false in --json). It carries spend, never a window; the window's
Claude Code footer shows the same tokens live, and a limit the session hit.

`limits` reads the session's screen once over the control socket (`text --json tail=40`)
and prints the wall the last turn ended on (the kind the supervisor acts on, its words and
its reset) and any windows a painted /usage panel shows. With no @<sid> it
reads the session it runs in ($ATERM_PARENT_SESSION_ID); with neither it refuses (exit 2)
rather than read whichever session the socket defaults to. No session answering is exit 1.

`disk` REPORTS AND REMOVES NOTHING by default. Every row carries the witness that would
make it safe to remove — for a build directory, a build tool's own marker and a cargo
profile idle past `disk.target_stale_days` (default 1 day: nothing a compile writes
touched that long) with a non-empty `incremental/` and locks no build holds — below the
floor, a recent profile's own row, oldest first (see below); or a version directory the
live symlink does not point at, or a package build the live one supersedes. Removal
needs BOTH `disk.apply = true` in aterm.toml AND an explicit `--apply <class>`; a
refusal is written to the disk journal as a denial row rather than dropped. Nothing under the transcripts root is removable under any flag.
Build directories are the ones you NAME on the line, and `cargo-targets` takes from each
ONLY its idle profiles' `incremental/` (the next build of each crate is a full compile
of it) — below the floor, recent profiles' too, as the tick does — deleted while holding every one of cargo's build locks of that profile — a
profile a build holds is skipped, and nothing is reached through a symlink or on another
device. Never the directory itself, `deps/`, `build/`, `.fingerprint/`, an uplifted
binary, `examples/`, `doc/` or a benchmark's data: those deeper caches are left for a
later tier, since cargo runs tests without its locks. Each removal is journalled before
it starts and again after, with the bytes it released and every error; a removal whose
intent cannot be journalled (a full disk, no state directory) does not start. The bytes
it says it freed are COUNTED, not measured: the blocks of each deleted file that was
that file's last name. A hard-linked file frees nothing and is not counted; an
APFS clone's shared blocks are counted although the clone keeps them; a file it cannot
open to measure is deleted uncounted. The journal is cut back to its newest 1024 rows
once it passes 1 MiB. One removal needs no verb: every 6 h the window looks at free
space, and below `disk.auto_free_gib` (10 GiB; 0 or a negative value turns it off) it
reclaims the same caches in the build directories of its agents' working directories
on that volume by itself — that class only, each with its witness journalled here —
LEAST RECENTLY USED FIRST: every idle profile's cache, then, one profile at a time and
oldest first, the caches of profiles a compile wrote into within `disk.target_stale_days`
(1 day), measuring free space again before each and stopping once it is back at the
floor plus 1 GiB, or once the bytes those removals released cover what free space was
short when they began (a local snapshot can keep the figure from rising), or when it
cannot be measured (a stop row in the journal says which, and where). Even then a
profile a build holds locked is never taken, nor one on another volume. No reclaim, the
tick's or this verb's, takes a profile written into within the last 10 min (a build that
just finished is usually followed by another, which would write the cache straight back)
— not even at `target_stale_days = 0`, which otherwise makes every profile idle. Below
the floor this verb lists those profiles too, in the order they would go
(`under_pressure_up_to=` is what every one of them would free, an upper bound;
`reclaimable=` is what goes whole), and `--apply cargo-targets` takes them under the
same stop; at or above it, a profile inside the window is kept. So `disk.target_stale_days` says
what is IDLE — what `--apply cargo-targets` takes at any free figure, and what the tick
takes whole before any recent profile — and, since 2026-09-28, no longer keeps a
recently built checkout's cache once the volume is under the floor.

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
(`[harness] human_grace_s`; the LADDER below narrows it), it TYPES a notice
asking the agent to reach a stopping point — let its background tasks finish
while they make progress, never cancel them, but stop any wait of its own that
can never end — naming the shells still running under the agent (up to five, by
pid and age, and what each of the agent's own runs), and to answer with a
one-time READY marker. Only after that
answer is in the transcript, with no shell still running under the agent and
nobody holding the tab, does it send SIGTERM (never a harder signal), wait for
the shell prompt, type a relaunch line — the atpkg hook sourced first so the
shell's PATH is healed, then the same launch flags with `--resume <session>` —
and, once the new process holds the conversation, type one line telling the
agent to carry on. A launch it cannot resume (`--print`, `--worktree`, an
unknown flag) is refused, not guessed; an unanswered notice, or a READY
answer that work under the agent outlives, is asked again every 30 minutes
(at a break of the agent's background work too), four notices at most, and
then that ROUND gives up, names what still runs, and types ONE line telling the
agent the upgrade is off and to carry on — wherever a notice may go, a break of
its background work included, until the round's rest ends (past it the new
round's first notice supersedes it). NO STOP IS FOR GOOD: a
round that gave up, was refused, or whose restart stopped rests two hours
(one round's worth of asking: four notices, 30 minutes apart; the same stop
again rests four hours, then eight) and then a NEW ROUND starts by itself —
new READY markers, its asks reset, `rearmed:<why>` on the ledger — whose
first notice goes under every gate a first notice does, at an idle point or a
break (never into a session at its limit). A late READY to a round that gave
up is still acted on, and no new round starts while it stands; one the
agent's own work outlives past the drain is void, at a break as at an idle
point, and the rest begins again from the void. A SESSION AT ITS USAGE LIMIT
is never asked — its screen showing the limit, or its last
reply the limit's, until the reset that reply names (30 minutes for one that
names none) or a `/login` — and a notice a usage limit answered is not asked
again until the model takes it: nothing is typed behind it and its window
does not run. Once that limit is over and the screen shows none, the notice
is typed again as the same ask — straight away until three upgrade notices
wait untaken in the conversation; from then on it waits `queued` until the
session goes on, `--now` (one copy more), or a rest after the limit's word
ran out (one copy more): two hours, doubling with every further copy the
model has not read, up to a day — so a limit that lasts for days adds one
copy a day. A READY answer is consent only from the process the notice
reached, in its tab: a conversation resumed by hand is never restarted on it,
and waits while that process lives (`wait:notice-owned-by-other-process`);
once it is gone, the upgrade is pending again in a new round, with nothing
owed to the gone one (`reopened:notice-process-gone`), and the process that
holds the conversation now is asked afresh, under every gate a first notice
is, only while it is still behind.
THE LADDER — ACTIVITY DELAYS; IT NEVER DISABLES (the owner's decision of
2026-09-28): the longer a session has been behind, the less the upgrade waits
for a comfortable moment. Behind less than 20 minutes it asks for the natural
idle point: the screen still 20 s and nobody who gave the tab input within
`[harness] human_grace_s`. From 20 minutes a pause that only LOOKS quiet counts:
two looks 20 s apart that read the screen idle with the same last words,
however it repainted between them (a footer's clock, a goal's counter). From
an hour a person holds it only by a keystroke in the last 20 s. From two
hours it moves at the first such pause, its settle waived. No rung ever moves
it over a draft, a box, a keystroke in the last 20 s, a hold, a usage limit or
the login wall, or running work — the agent's turn or status, the shells under
it, a turn of a Codex client's own in its daemon (`wait:daemon-turn`,
`wait:goal`, `wait:daemon-busy` for one it cannot yet place in another
session) — and the READY answer and an empty process tree still come before
the restart. `--now` is the last rung at once, with the same floors.
A SESSION IS MOVED ONLY WHILE THE
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
recorded as failed and nothing more is typed into the tab. A restart that
stops after its SIGTERM ended the agent stays listed though no Claude Code
holds it any more (in the window for a day): `claude --resume <session>` in
the tab takes the conversation back. One under way that has not moved for 5
minutes — the agent still ending, its prompt not free, the new one never
idle — reads stalled (`stuck:<what>`); nothing is forced.
THE WINDOW DOES IT BY DEFAULT, with no sweep: each of its own tabs' supervisors
takes the step at its session's idle points once atpkg, or Claude Code's own
updater, installs a newer build, while aterm.toml's `[harness] enabled` and
`[harness] upgrade` are not turned off; one lock keeps it and a hand-run sweep
apart. It hands the relaunched agent straight back to its supervisor, which types
the carry-on at its first idle point. It also RELAUNCHES an agent that crashed
(its session record left behind, read as the exit is seen — a graceful exit is
someone's), on its conversation (`[harness] relaunch`). An agent the HARNESS'S
OWN restart ended — its step returned with the relaunch still to type (someone
at the returned prompt, a hold) — is never read as someone's exit, a limit's
or a graceful one: the restart is carried at least every minute, Claude Code's
and Codex's, whatever `[harness] relaunch` says, the line typed only once
nobody types at the prompt and no hold stands — and a line once typed never
typed again — until it lands or its 5 minutes run out, said on the tab either
way. Claude Code's count from the agent's exit, however long it took to shut
down; Codex's from its typed `/exit`, since a TUI still running a minute after
its `/exit` is one the `/exit` did not take.
THE MODEL goes with it: a session behind the newest model of its OWN family that
the managed build offers (`aterm harness upgrade models`) is moved onto it on the
relaunch line (`--model`, session-only — never `/model`, which also saves the
person's default) — Opus 5 -> Opus 5.5 first; only with nothing newer of its
family, up the priority list for a model nobody chose; never down, a launch
alias like `opus` kept as it is — riding a build restart when there is one, else
by itself once its prompt cache is cold, or at most an hour after the move came
due. A turn that ended with the agent's own background work in flight (Claude's
`Waiting for N dynamic workflow`, a shell it left running, a Codex background
terminal) is a NATURAL BREAK: once it has stood 20 s the window types the notice
there, and again only after a whole 30 minutes with that work still running,
four notices at most before the round gives up (and two hours later a new round
asks again, there too). The restart still waits for an idle point with nothing
running under the agent, so running work is never ended; a READY answer a
person held past the drain is void at a break as at an idle point, and the line
that releases the agent is typed there too. CLAUDE'S OWN STATUS is read against
its screen: a `busy` or `shell` status that has stood 20 s over a screen read
idle two looks in a row (of the same agent, no more than about ten minutes
apart) lets the notice and the release go as at a break, whether work runs
under the agent (a shell whose count the screen does not draw) or nothing does.
The restart waits for Claude's own `idle` all the same — work inside the
agent's own process is no process under it — asking a READY it holds again
after 30 minutes, and releasing the agent once the round gives up; that wait
is `status-stale:<status>`.
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
build than the managed Codex is moved by the vendor's verb,
`<managed codex> app-server daemon update --from-cli --yes`, run with the
environment the daemon itself was started with: the managed build is copied
in and PINNED, the updater goes, the daemon restarts, and every attached TUI
reconnects (`• Reconnected. No input was resent.`). A daemon ON the managed
build whose vendor updater is still armed (`packages/app-server-daemon/
auto-update-version`, which re-runs chatgpt.com's installer outside atpkg,
and restarted the owner's daemon mid-turn on 2026-09-28) is PINNED the same
way — it keeps its tab due for the pin alone, looked at for it at most once
an hour (`wait:pin:<why>`, which owns no turn end and shows no row or tab
mark). Only while NOTHING RUNS
in it — every thread it holds idle and settled, attached to a tab or not (its
locks reveal them), and no BACKGROUND TERMINAL a finished turn left running
under it (a unified-exec command leads a session of its own under the
daemon; the restart would end it) — no person is at a Codex tab on that home,
none is held, the OWNER'S WORD on none keeps it (a `--skip` of this build or
a `--defer` on a Codex tab holds the daemon that runs its conversation too;
that tab's `--now` narrows its own attended guard to a keystroke in the last
20 s here as well, as the ladder's last rung does), and every
Codex attached to the daemon is one the pass sees — the window's step asks
every Codex tab of its window, whichever tab's step reaches the daemon first
(a TUI in another window or a pane was asked none of this): `wait:busy-thread`,
`background-terminal`, `owner-held`, `unseen-client`, `held`, `attended`,
`settling`. A daemon ahead of the managed Codex is never moved back
(`wait:vendor-ahead`). THEN EACH TUI that is a tab's foreground job on an
older build: a DAEMON-MODE client at an idle point (no turn running on
screen, its input line's dim placeholder — `›` or 0.158.0's `»` — no box,
quiet as its rung asks, nobody at the tab — the same gates, the same `--now`
— and NO TURN OF ITS OWN RUNNING IN ITS DAEMON, since an `/exit` might stop
it: Codex 0.157 was measured printing `Disconnected from this task. Any
running work continues.`, and 0.158.0's binary also holds `… The current turn
was stopped.` — which case prints it is not measured) is ended by a TYPED
`/exit`. Its own conversation is the one the kernel names — every thread of
its daemon hangs from that conversation's root (read from each thread's
rollout: the subagents it spawned name it as their parent) and it is the
daemon's only client — and a turn anywhere in it, a subagent's included,
waits `wait:goal` while its footer says `Pursuing goal`, else
`wait:daemon-turn`. A GOAL IS PAUSED FOR THE MOVE (the owner's decision of
2026-09-28): a goal starts its next turn within milliseconds of the last one's
end, so from the Land rung (or `--now`), where its own turn is all that holds
the move and no save-then-wait switch is open, aterm types `/goal pause` —
Codex takes it while a turn runs, and it stops the NEXT turn, never the running
one — waits for that last turn to end (`wait:goal-held`), moves the tab, and
resumes the goal once: the box the relaunched Codex opens with, asking
whether to resume its paused goal, answered with its first option, or `/goal
resume` typed (an embedded session gets no carry-on then: the goal is it). A
pause that never shows is followed, at a goal turn's head only (its rollout
holds nothing of its own work), by one Esc. The record of it (`<aterm
state>/drive/<tab>.goal-hold.json`, written before every key) resumes it once
after a restart, never twice; a person resuming or changing the goal, or
their hand on a pause not yet seen, takes it from aterm; every wait at a goal
held paused is `wait:goal-held`, which keeps the supervisor's carry-on out; a
move that does not come within an hour is given up and the goal resumed, said
once — never into a thread fallen into a sandbox, where it stays paused
(`wait:goal-sandboxed`, a row naming the relaunch); the relaunched Codex's box
is the one the approval policy answers for it; a switch that opens over the
pause resumes the goal at its reset; a resume not made in an hour and a half
is a row with the one hand step (`/goal resume` in the tab), never while a
switch holds the session or the paused goal's last turn runs. Another
session's turn on the same
daemon holds it only until its screen places it there — a ROOT running at
two looks 20 s apart, while this screen's words stood still, which a turn of
its own would have changed, with another Codex attached to the daemon —
never while its footer says `Pursuing goal` (a goal's next turn runs under a
screen whose words stand still); a subagent's turn, or a second root's with
this TUI the daemon's only client, is never placed. Until then it waits
`wait:daemon-busy`, never said as a goal to pause in this tab, nor presumed
another session's. The `/exit`
is fenced on the screen it was judged by, its Enter guarded on the
composer's row, its
daemon read once more first (a turn begun since the sweep's read waits
`wait:changed`) — and
relaunched at the returned prompt as `<managed codex> resume <the same flags>
<thread>`, the thread read from the TUI's own exit hint (`Reconnect: codex
resume <id>`: its command block's output, else the rows directly above the
new prompt's FIRST row, so a prompt of two rows is no obstacle). Where the
shell integration's marks do not reach aterm (`status integration=degraded`,
as after an aterm update) the thread is the one the KERNEL names — the root
of the daemon's one conversation, this TUI its one client — recorded before
the `/exit`
(a hint naming another stops the relaunch); where the kernel cannot name it
either, the `/exit` waits `wait:no-shell-integration`, which after 30 minutes
is a row saying so (`blocked:no-shell-integration`: quit Codex in its tab and
resume it with the line it prints); the relaunch line also heals the shell's
integration. Its work never stops, in the daemon, so nothing is announced and
nothing continued. It waits for its daemon to be on the build it moves to, and says
what the daemon waits on (`wait:daemon-first:<why>`). An EMBEDDED session
(`--no-daemon`, or a launch Codex keeps out of the daemon; the kernel proves
it by the `thread-writer-locks/<id>.lock` it holds) gets Claude's protocol —
the notice, its READY answer in the rollout, nothing running under it, a
background terminal included (`wait:background-terminal`: it would end with
the TUI; one that outlives the READY a whole re-ask window is asked about
again, and past the last ask the upgrade gives up, as Claude's) — then the
same `/exit`, the relaunch through the one relaunch line
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
for a turn a hand-run sweep found in progress, `status-stale:<status>` for a status
the idle screen does not bear out — the restart's until Claude says `idle` …,
`release:<gate>` while that gate holds the line an abandoned notice owes the
agent; where no look has recorded one, a Claude Code row reads Claude's own live
status, `not-idle:waiting` for a box, never over a recorded word) with
`wait_for=`, the owner's `request=`, `next_round=` (for a round that stopped: how
long until the new round it starts by itself, `due` once it has rested, `-` for
any other, one the owner's `--skip` holds, or one that gave up and acts on a
late READY — no new round starts while that answer stands), `stalled=` — `-`
while the upgrade will move on its own, else why it will not: `refused:<why>`
(it stays said through the new round's first look, which meets it again unless
something changed), `failed:<why>`, `held-back:<owner>` (a multiplexer pane),
`stuck:<exiting|exited|relaunched>` (a restart under way that has not moved for
5 minutes — the agent still ending, its prompt not free to type at, the new one
never idle: nothing is forced, and no word moves it; a sweep says it once on the
ledger, `stuck:<what>`), `blocked:<wait>` (a wait
no rung passes, blocking the move 30 minutes whatever other wait came between:
`no-shell-integration`, `screen-unreadable` — quit it in its tab and resume it
there), or `overdue` (6 hours behind — four
past the ladder's last rung — whatever it waits on, unless the owner's `--now`
came in the last 30 minutes) — and `held_by=` (what ran under the agent at the last look that
waited, by pid, name and age — `63492(zsh:5d4h),…`, `-` for nothing; never a
command), `release=` (the release still owed to an agent the upgrade asked
to wind down and did not restart — why it abandoned that notice: `gave-up`,
`void`, `skipped`, `deferred`, `retargeted`, a stop's reason; `-` for none),
`rung=` (the ladder's rung while the move is owed: `prefer`, `settled`,
`keys-only`, `land` — `land` under the owner's `--now` — and `-` once it is
under way, done or stopped), and the watch's fields last: `looked=` (how long
ago a look off a point read the record) and `by=` (the aterm build that
looked), `point=` (how long ago the session's loop last offered an idle point
or a settled break) and `guard=` (the loop's guard that withheld the latest
point: `wall`, `settle`, `no-background-wait`, `act-untaken`, `not-idle`, …),
each `-` for none recorded, and `watch_at=` (when the record is due to be
watched: the step's deadline, or 24 hours behind, whichever is first; `due`
once past, `-` for a record nothing more is owed on); and, only while aterm's
own fence refuses the notice due, `refused=<n>/<span>` at the very end (how
many looks in a row it could not type it, and since how long).
A round that GAVE UP is no stall: it rests until its `next_round=`,
and its column reads `pending/<to>/next-round:<span>/<age>` —
`pending/<to>/ready/<age>` while it acts on a late READY instead.
The window shows the same per tab as `upgrade=`
in `aterm ctl status`/`sessions`, records it in Settings ▸ Messages — one
record, `<agent> <build> installs itself in tab N`, saying there is nothing to
do and from what time the first pause is enough — and marks a STALLED tab
(`meta attention owner=upgrade`) — never a tab that is merely waiting for its
moment.

THE OWNER'S WORD on one tab's upgrade — `upgrade <sid> --now|--defer
<dur>|--skip` — is written into that upgrade's state under the sweep's own
lock (waiting up to 10 s for a step in progress; past that it exits 75 and
writes nothing) and put on the ledger as `requested:<word>`, and it wakes the
window's worker for that tab, which takes it at the session's next idle
point. The word is on the TAB named: the same conversation resumed in another
tab is asked afresh. `--now` stands the upgrade at the LADDER'S LAST RUNG at
once: no quiet window, and a person at the tab holds it only by a keystroke
in the last 20 s (its `status human_ms=`) — never waived, the word included —
and nothing else (an idle status, an empty composer, no box, no hold, no turn
in a Codex daemon, the READY answer and nothing running under the agent are
still required, and what is typed is still fenced on the screen it was judged
by).
Every word arms a NEW ROUND of the upgrade: a notice after it asks for a READY
marker no earlier answer carries. An upgrade the owner holds (`--defer`,
`--skip`) owns none of its session's turn ends: the tab's supervisor goes on
continuing the worker as if none were pending, and a notice already typed is
ended — the hold's end brings a fresh notice and needs a fresh READY. ONE
PLACE `--now` adds an act: an upgrade that GAVE UP (no READY answer it could act
on after the last notice) is re-armed AT ONCE rather than at its next round,
so the notice is typed again and, after its READY answer, the agent is
restarted. An upgrade that was REFUSED or FAILED is not
re-armed by the word — `--now` exits 1, names why and when its next round
starts by itself. `--skip` holds a stopped round too: it is not re-armed
while the skip of its build stands, and a `--defer` holds it until it runs
out. `--now`
cannot reach an agent HELD BACK in a multiplexer pane either (typing into the
tab does not reach it; stderr says so). `--defer` holds it until the time runs
out; `--skip` holds it on the running build until a newer target than this
one comes, and is what ends a stall the harness cannot move. A restart
already under way is neither held nor hurried (exit 1).

The window writes the same words, on the same path, with no shell: a
stalled upgrade's band row offers two of them (Upgrade now where `--now`
moves it, Not today — a one-day `--defer` — and Skip version; one row for
several tabs of the same agent writes the word for each tab it lists that
takes it), Settings ▸
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
    /// The Claude Code directory: `CLAUDE_CONFIG_DIR` when set, else
    /// `$HOME/.claude` ([`footer::claude_dir_of`]); `None` falls back to
    /// `home` ([`Env::claude_dir`]).
    pub claude_dir: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR` when it is set but names no directory this
    /// command can find — a relative path, which the vendor resolves against
    /// ITS working directory, not this command's. Then no Claude Code
    /// directory is known ([`Env::claude_dir`]): `~/.claude` is not the one
    /// the session writes.
    pub claude_config_unusable: Option<String>,
    /// The Claude Code session this command runs under, when the vendor
    /// named it: `CLAUDE_CODE_SESSION_ID`, which Claude Code 2.1.283 sets for
    /// every tool it runs (MEASURED). aterm strips `CLAUDE_*` from the
    /// shells it spawns, so only Claude Code's own tools carry it.
    pub claude_session: Option<String>,
    /// The Claude Code process this command runs under (`CLAUDE_PID`, set
    /// beside the session id), for a build that names only the process.
    pub claude_pid: Option<u32>,
    /// Unix seconds. Read once, at the top of [`main_entry`].
    pub now: i64,
    /// The aterm session id (`$ATERM_PARENT_SESSION_ID`), or empty.
    pub sid: String,
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
        let config_dir = std::env::var("CLAUDE_CONFIG_DIR")
            .ok()
            .filter(|d| !d.is_empty());
        let claude_dir =
            footer::claude_dir_of(config_dir.as_deref(), std::env::var("HOME").ok().as_deref());
        let claude_config_unusable = config_dir.filter(|_| claude_dir.is_none());
        let claude_session = std::env::var("CLAUDE_CODE_SESSION_ID")
            .ok()
            .filter(|id| footer::is_session_id(id));
        let claude_pid = std::env::var("CLAUDE_PID")
            .ok()
            .and_then(|p| p.parse::<u32>().ok())
            .filter(|&p| p > 0);
        Ok(Env {
            state,
            aterm_state,
            cwd,
            home,
            claude_dir,
            claude_config_unusable,
            claude_session,
            claude_pid,
            now,
            sid: std::env::var("ATERM_PARENT_SESSION_ID").unwrap_or_default(),
            sock: None,
            config: default_config_path(),
        })
    }

    /// The Claude Code directory: [`Env::claude_dir`], else `$HOME/.claude`
    /// — but none when `CLAUDE_CONFIG_DIR` is set and unusable
    /// ([`Env::claude_config_unusable`]).
    #[must_use]
    pub fn claude_dir(&self) -> Option<PathBuf> {
        if self.claude_config_unusable.is_some() {
            return None;
        }
        self.claude_dir
            .clone()
            .or_else(|| self.home.as_ref().map(|h| h.join(".claude")))
    }

    /// Where the vendor keeps its transcripts: `<claude dir>/projects`. A
    /// FILESYSTEM fact, not a hook — `--bare` removes hooks, plugins and the
    /// statusLine in one flag and does not touch this.
    #[must_use]
    pub fn transcripts_root(&self) -> Option<PathBuf> {
        self.claude_dir().map(|d| d.join("projects"))
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

/// The byte cap on what this command folds, over all of the session's files
/// — its OWN cap: it runs once, so the window's per-read budget
/// (`footer::FOLD_BUDGET`) does not apply. The fold streams, so the cost is
/// one pass and no line is ever held; bytes past the cap are not folded, and
/// only then does the output say the totals are a PREFIX.
pub const MAX_TRANSCRIPT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Which transcript `usage` folds, and how it was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// The session Claude Code NAMED in this command's environment
    /// (`CLAUDE_CODE_SESSION_ID`, or `CLAUDE_PID`'s `sessions/<pid>.json`):
    /// this session's own file. `how` is the JSON's `transcript_pick`.
    Named {
        /// The transcript.
        path: PathBuf,
        /// `session-id` or `claude-pid`.
        how: &'static str,
    },
    /// The session was named but has written no transcript yet: nothing is
    /// folded — another session's file is not this one's.
    NamedButAbsent {
        /// The session id.
        session: String,
    },
    /// Nothing named a session: the NEWEST `.jsonl` in this working
    /// directory's project directory, which may be another session's.
    Newest(PathBuf),
    /// Nothing to fold.
    None,
}

/// Choose the transcript `usage` folds: the session Claude Code named
/// ([`Env::claude_session`], then [`Env::claude_pid`]'s sessions file), else
/// the newest in this working directory's project directory — the one rung
/// left when nothing names the session (aterm's own shells carry no
/// `CLAUDE_*`), labelled [`Source::TranscriptNewest`] and said so.
#[must_use]
pub fn pick_transcript(env: &Env) -> Pick {
    let Some(claude_dir) = env.claude_dir() else {
        return Pick::None;
    };
    let named = |entry: footer::SessionEntry, how: &'static str| match footer::transcript_path(
        &claude_dir,
        &entry,
    ) {
        Some(path) => Pick::Named { path, how },
        None => Pick::NamedButAbsent {
            session: entry.session_id,
        },
    };
    if let Some(id) = &env.claude_session {
        let entry = footer::SessionEntry {
            session_id: id.clone(),
            cwd: env.cwd.clone(),
            version: None,
            proc_start: None,
            started_at: None,
        };
        return named(entry, "session-id");
    }
    if let Some(entry) = env
        .claude_pid
        .and_then(|pid| footer::session_of_pid(&claude_dir, pid, None))
    {
        return named(entry, "claude-pid");
    }
    let dir = claude_dir.join("projects").join(project_dir_name(&env.cwd));
    newest_transcript(&dir).map_or(Pick::None, Pick::Newest)
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
    let pick = pick_transcript(env);
    let (path, source, pick_word) = match &pick {
        Pick::Named { path, how } => (Some(path.clone()), Source::Transcript, *how),
        Pick::Newest(path) => (Some(path.clone()), Source::TranscriptNewest, "newest-mtime"),
        Pick::NamedButAbsent { .. } | Pick::None => (None, Source::None, "none"),
    };
    // The window's footer folds through the same `SessionUsage`; one refresh
    // here is the whole pass, up to this command's own cap. ONE sum,
    // `total_fold`, feeds the view, the JSON and the text alike.
    let folded = path.map(|path| {
        let mut session = SessionUsage::new(path);
        let refresh = session.refresh(MAX_TRANSCRIPT_BYTES);
        let total = session.total_fold();
        (session, refresh, total)
    });
    let mut view = UsageView::new(env.now);
    if let Some((_, _, total)) = &folded {
        let mut account = AccountView::new("account", true);
        account.add_transcript(total, &usage::PriceTable::new());
        view.accounts.push(account);
    }
    if json {
        // Schema 1, keys ADDED only: `transcript_pick` gains `session-id`,
        // `claude-pid` and `none`; `complete` and the subagent counts are new.
        let mut extra = Vec::new();
        if let Some((session, _, _)) = &folded {
            extra.push((
                "transcript",
                Value::from(session.transcript().display().to_string()),
            ));
        }
        extra.push(("transcript_pick", Value::from(pick_word.to_owned())));
        if let Some((session, refresh, _)) = &folded {
            extra.push((
                "subagent_transcripts",
                Value::from(session.subagent_files() as u64),
            ));
            extra.push((
                "subagent_transcripts_unread",
                Value::from(refresh.unread as u64),
            ));
            extra.push(("complete", Value::from(refresh.complete())));
        }
        if let Pick::NamedButAbsent { session } = &pick {
            extra.push(("session", Value::from(session.clone())));
        }
        if let Some(dir) = &env.claude_config_unusable {
            extra.push(("claude_config_dir_unusable", Value::from(dir.clone())));
        }
        let _ = writeln!(
            out,
            "{}",
            sourced_json("usage", source, &usage::usage_json(&view), extra)
        );
        return ExitCode::SUCCESS;
    }
    let line = usage::hud_line(&view);
    let _ = writeln!(out, "{line}  [source={}]", source.as_str());
    let Some((session, refresh, total)) = &folded else {
        let why = match &pick {
            Pick::NamedButAbsent { session } => format!(
                "session {session} has written no transcript yet — nothing to fold (another \
                 session's file is not this one's)"
            ),
            _ if env.claude_config_unusable.is_some() => format!(
                "CLAUDE_CONFIG_DIR is {:?}, not an absolute path: the Claude Code directory it \
                 names cannot be found from here, and ~/.claude is not it — nothing to fold",
                env.claude_config_unusable.as_deref().unwrap_or_default()
            ),
            _ => "no transcript for this directory under the Claude Code projects directory — \
                  nothing to fold"
                .to_owned(),
        };
        let _ = writeln!(out, "{why}");
        return ExitCode::SUCCESS;
    };
    let facts = session_usage::UsageFacts::of(
        session_usage::model_tokens(total.per_model()),
        None,
        Some(refresh),
    );
    let tokens = match facts.as_ref().and_then(session_usage::usage_text) {
        Some(text) if facts.as_ref().is_some_and(|f| !f.models.is_empty()) => text,
        Some(text) => format!("no tokens \u{00B7} {text}"),
        None => "no tokens".to_owned(),
    };
    let _ = writeln!(out, "{} {tokens}", footer::USAGE_MARK);
    for (model, s) in total.per_model() {
        let _ = writeln!(
            out,
            "  {model}: in {} (cache write {}, cache read {}), out {}",
            s.input, s.cache_write, s.cache_read, s.output
        );
    }
    let whose = match &pick {
        Pick::Named {
            how: "claude-pid", ..
        } => "this session's own transcript, named by CLAUDE_PID's sessions file",
        Pick::Named { .. } => "this session's own transcript, named by CLAUDE_CODE_SESSION_ID",
        _ => {
            "the NEWEST transcript in this project directory: if two Claude Code sessions \
             share this working directory it may be the other one's"
        }
    };
    let mut short = String::new();
    if refresh.spent >= MAX_TRANSCRIPT_BYTES {
        short.push_str(&format!(
            " The fold stopped at its {} GiB cap before the end: these totals are a PREFIX.",
            MAX_TRANSCRIPT_BYTES >> 30
        ));
    } else if !refresh.caught_up {
        short.push_str(" The transcript could not be read to its end: these totals are partial.");
    }
    if refresh.unread > 0 {
        short.push_str(&format!(
            " {} subagent transcripts are NOT in these totals (unreadable now, or past the {} \
             files one session's fold follows).",
            refresh.unread,
            session_usage::MAX_SUBAGENT_FILES,
        ));
    }
    if refresh.walk_cut {
        short.push_str(
            " The subagent walk stopped at its entry bound: more subagent transcripts may exist \
             and are NOT in these totals.",
        );
    }
    let _ = writeln!(
        out,
        "SPEND folded from {} (+{} subagent transcripts; {} assistant rows), {whose}.{short} \
         A transcript carries no rate-limit window.",
        session.transcript().display(),
        session.subagent_files(),
        total.assistant_rows,
    );
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

/// The `[disk]` knobs ([`disk::KEYS`]), read from `aterm.toml` through
/// [`toml_bool`] and [`toml_int`]. A value of the wrong type falls back to the
/// shipped default rather than to a guess, and so does a NEGATIVE warning
/// threshold or stale window (a negative window would make every directory a
/// candidate at once). A negative `auto_free_gib` reads as `0`, OFF: a floor
/// below zero is never crossed, and the default would leave the removal on for
/// a person who wrote `-1` to stop it ([`disk::Config::negative_reading`]).
fn disk_config(env: &Env) -> disk::Config {
    disk_config_at(env.config.as_deref())
}

/// The `[disk]` knobs of the `aterm.toml` at `path` (every knob at its
/// default when there is none): what the verb reads, and the host's tick.
#[must_use]
pub fn disk_config_at(path: Option<&Path>) -> disk::Config {
    let text = path
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
        auto_free_gib: toml_int(&text, "disk", "auto_free_gib")
            .map_or(d.auto_free_gib, |v| u64::try_from(v).unwrap_or(0)),
    }
}

/// What one tick of the host's disk watch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskTick {
    /// Free space is at or above the automatic floor, or unknown: nothing was
    /// scanned, journalled or removed.
    Plenty,
    /// Below the floor: the report was journalled and the automatic grant
    /// applied ([`disk::apply_auto`]).
    Reclaimed(disk::Applied),
}

/// What one look of the window's disk watch is about: every path and figure is
/// the host's ([`disk_tick`]).
#[derive(Debug, Clone, Copy)]
pub struct DiskLook<'a> {
    /// The harness state directory; the ledger is `<state>/disk.jsonl`.
    pub state: &'a Path,
    /// The volume the free figure is about.
    pub volume: &'a Path,
    /// Its free bytes, measured by the host (the statvfs edge lives in
    /// `atpkg`); `None` fails OPEN.
    pub free: Option<u64>,
    /// The build directories the host found where its agents work. Only the
    /// ones on [`Self::volume`]'s volume are looked at ([`on_volume`]).
    pub targets: &'a [PathBuf],
    /// Where transcripts live: nothing under it is ever removable.
    pub transcripts: Option<&'a Path>,
    /// Unix seconds.
    pub now: i64,
}

/// ONE TICK of the window's disk watch (design §5.5's 6 h timer, hosted by
/// `aterm-gui`'s harness host): below the automatic floor
/// ([`disk::Config::below_auto_floor`], `[disk] auto_free_gib`), survey the
/// look's targets, journal the report into `<state>/disk.jsonl`, and reclaim
/// what [`disk::auto_plan`] grants through `remove` ([`disk::remover`] is the
/// real one: a profile's `incremental/`, under cargo's locks — every idle
/// one, then recent ones least recently used first, `measure` read again
/// before each of those and the pass stopped once free space is back at
/// [`disk::Config::pressure_target_bytes`]) —
/// journalling an intent row BEFORE each removal and its outcome (bytes
/// actually released, what went, what was skipped, every delete error) or a
/// denial row after, each as it happens. A removal whose intent row could not
/// be written (no state directory, a failed append) does not start: it is a
/// denial ([`disk::Refusal::Unjournalled`]), and nothing is removed without a
/// record. At or above the floor it reads
/// nothing but the config. A target on ANOTHER volume than the one measured
/// is not looked at: removing it would free nothing where space ran short
/// ([`on_volume`]).
#[must_use]
pub fn disk_tick(
    look: &DiskLook<'_>,
    config: disk::Config,
    remove: &mut disk::Remove<'_>,
    measure: &mut disk::Measure<'_>,
) -> DiskTick {
    let DiskLook {
        state,
        volume,
        free,
        targets,
        transcripts,
        now,
    } = *look;
    if !config.below_auto_floor(free) {
        return DiskTick::Plenty;
    }
    let roots = disk::Roots {
        volume: Some(volume.to_path_buf()),
        targets: on_volume(volume, targets, &volume_id),
        transcripts: transcripts.map(Path::to_path_buf),
        ..disk::Roots::default()
    };
    let survey = disk::scan(&roots, now, disk::Trigger::Tick, config, free);
    let rep = disk::report(&survey, config);
    let path = state.join(DISK_LEDGER);
    let mut sink = io::sink();
    let mut journal = if std::fs::create_dir_all(state).is_ok() {
        let _ = bound_ledger(&path, DISK_LEDGER_MAX_BYTES, DISK_LEDGER_KEEP_ROWS);
        Journal::open(Some(&path), None, &mut sink)
    } else {
        Journal::off()
    };
    journal.append_raw(&disk::report_row(now, "", &rep), &mut sink);
    let done = disk::apply_auto(
        &rep,
        survey.transcripts_root.as_deref(),
        remove,
        measure,
        &mut |step| journal.append_raw(&disk::step_row(now, "", step), &mut io::sink()),
    )
    .unwrap_or_default();
    DiskTick::Reclaimed(done)
}

/// The `targets` on the same volume as `volume`, by `id_of` (a device id): the
/// free figure the tick acts on is that volume's, and a build directory on an
/// external disk or a second APFS volume frees nothing there. A path whose
/// volume cannot be read is left out, and so is every target when the
/// measured volume's own id cannot be: nothing is removed on a fact nobody
/// read.
fn on_volume<V: PartialEq>(
    volume: &Path,
    targets: &[PathBuf],
    id_of: &dyn Fn(&Path) -> Option<V>,
) -> Vec<PathBuf> {
    let Some(measured) = id_of(volume) else {
        return Vec::new();
    };
    targets
        .iter()
        .filter(|t| id_of(t).is_some_and(|id| id == measured))
        .cloned()
        .collect()
}

/// A path's volume: its device id (`st_dev`).
#[cfg(unix)]
fn volume_id(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|m| m.dev())
}

/// A path's volume where there is no `st_dev`: its drive or share prefix.
#[cfg(not(unix))]
fn volume_id(path: &Path) -> Option<std::ffi::OsString> {
    match path.components().next()? {
        std::path::Component::Prefix(prefix) => Some(prefix.as_os_str().to_os_string()),
        _ => None,
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

    // The hand-run verb reclaims the idle profiles of the build directories
    // it was NAMED, on whatever volume they are: `device` is not fenced. A
    // pressure row carries the device it was measured on, and free space is
    // measured again (`df`) before each.
    let judge = disk::Judge {
        now: env.now,
        threshold_days: config.target_stale_days,
        device: None,
    };
    let volume = roots.volume.clone();
    let done = class.map(|_| {
        disk::apply(
            &rep,
            survey.transcripts_root.as_deref(),
            class,
            &mut disk::remover(judge),
            &mut || volume.as_deref().and_then(disk::free_bytes),
            &mut |step| journal.append_raw(&disk::step_row(env.now, &env.sid, step), err),
        )
    });

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
            // What each row's removal did, as the text form prints it: what
            // went, what was kept and why, every delete error — so a partial
            // reclaim reads as partial here too.
            let texts = |items: Vec<String>| {
                Value::Array(items.into_iter().map(Value::from).collect::<Vec<_>>())
            };
            o.insert(
                "rows".to_owned(),
                Value::Array(
                    done.removed
                        .iter()
                        .map(|(row, removed)| {
                            let mut r = Map::new();
                            r.insert(
                                "path".to_owned(),
                                Value::from(row.path.display().to_string()),
                            );
                            r.insert("freed_bytes".to_owned(), Value::from(removed.bytes));
                            r.insert(
                                "units".to_owned(),
                                texts(
                                    removed
                                        .units
                                        .iter()
                                        .map(|p| p.display().to_string())
                                        .collect(),
                                ),
                            );
                            r.insert(
                                "skipped".to_owned(),
                                texts(
                                    removed
                                        .skipped
                                        .iter()
                                        .map(|(p, why)| format!("{}: {why}", p.display()))
                                        .collect(),
                                ),
                            );
                            r.insert("trouble".to_owned(), texts(removed.trouble.clone()));
                            Value::Object(r)
                        })
                        .collect(),
                ),
            );
            o.insert(
                "denials".to_owned(),
                Value::Array(
                    done.denials
                        .iter()
                        .map(|d| Value::from(d.describe()))
                        .collect(),
                ),
            );
            o.insert(
                "stopped".to_owned(),
                done.stopped
                    .as_ref()
                    .map_or(Value::Null, |s| Value::from(s.describe())),
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
    if rep
        .rows
        .iter()
        .any(|r| matches!(r.witness, disk::Witness::LeastRecentlyUsed { .. }))
    {
        let _ = writeln!(
            out,
            "  under pressure: the least-recently-used rows above go in that order, free space measured again before each, until {} is free or the bytes released cover what was short — under_pressure_up_to is what every one of them would free, an upper bound",
            disk::human_bytes(config.pressure_target_bytes())
        );
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
            for (row, removed) in &done.removed {
                let _ = writeln!(
                    out,
                    "  removed {} — {}; freed about {}",
                    row.path.display(),
                    row.witness.describe(),
                    disk::human_bytes(removed.bytes)
                );
                for unit in &removed.units {
                    let _ = writeln!(out, "    went: {}", unit.display());
                }
                for (unit, why) in &removed.skipped {
                    let _ = writeln!(out, "    kept: {} — {why}", unit.display());
                }
                for trouble in &removed.trouble {
                    let _ = writeln!(out, "    trouble: {trouble}");
                }
            }
            for refusal in &done.denials {
                let _ = writeln!(out, "  denied: {}", refusal.describe());
            }
            if let Some(stop) = &done.stopped {
                let _ = writeln!(out, "  stopped: {}", stop.describe());
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

/// How much of a give-up's upgrade ledger row [`upgrade_ledger_line`] shows,
/// in bytes (every other row's detail is cut at 200): its list of what held
/// the move, whole.
const GAVE_UP_DETAIL_BYTES: usize = 8 * 1024;

// THE LIST FITS, checked here rather than trusted: it is bounded where it is
// written (`upgrade::held_list`) — at most `upgrade::HELD_NAMED` processes
// named, each by a name of at most 32 characters, a command of at most
// `upgrade::HELD_COMMAND_CHARS` and at most 32 characters of pid, age and
// punctuation, any character at most 4 bytes — after the give-up's own words
// (under 512 bytes, `upgrade_drive::gave_up_words`) and a count of the rest.
// A bound raised past the cut fails the build here, not in a ledger view.
const _: () = assert!(
    512 + super::upgrade::HELD_NAMED * 4 * (32 + super::upgrade::HELD_COMMAND_CHARS + 32) + 32
        <= GAVE_UP_DETAIL_BYTES
);

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
    // A give-up's row is its list of what held the move, after words that
    // alone run past the cut (the review of 2026-09-27: the view showed none
    // of the list): [`GAVE_UP_DETAIL_BYTES`] holds it whole.
    let cap = if s("step") == "gave-up" {
        GAVE_UP_DETAIL_BYTES
    } else {
        200
    };
    let detail = super::one_line(s("detail"), cap);
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
        aterm_state: None,
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
        aterm_state: env.aterm_state.clone(),
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
                         in {} (Settings: search \"harness\"): nothing moves until it is back on",
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
            let _ = writeln!(
                err,
                "aterm harness: upgrade {sid}: {}",
                super::upgrade_drive::refusal_sentence(&e)
            );
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
            "aterm harness: upgrade refused: `[harness] enabled` reads off in {} (Settings: \
             search \"harness\"){}{}",
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
            // A FINISHED upgrade is history: the session caught up, so it is
            // not behind, whatever its old record's clock says. `--status`
            // already reads the clock only for a Pending or Announced row; the
            // dry-run line read it for every row, so a session reported
            // `step=current` beside `pending_for=1d19h` — the age of an
            // upgrade that had landed that long ago (seen 2026-09-27).
            (!matches!(row.phase, super::upgrade::Phase::Done))
                .then(|| now.saturating_sub(row.behind_since)),
            (!row.wait.is_empty()).then(|| now.saturating_sub(row.wait_since)),
        )
    });
    if !json {
        let _ = match ages {
            Some((behind, wait)) => writeln!(
                out,
                "{} pending_for={} wait_for={}",
                r.line(),
                behind.map_or_else(|| "-".to_string(), super::upgrade::span),
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
        o.insert(
            "pending_for_s".to_owned(),
            behind.map_or(Value::Null, Value::from),
        );
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
