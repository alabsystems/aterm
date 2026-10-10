// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness` — the read views of the Claude Code harness: `usage`,
//! `limits`, `disk` and `ledger` (design `docs/DESIGN-aterm-wrapper-2026-09-17.md`
//! §5.2, §5.5, §5.8, and its §0.4 for what is no longer here), and `accounts`,
//! the roster of Claude Code accounts and its report
//! (`docs/DESIGN-claude-accounts-2026-10-01.md` §4, §8).
//!
//! A reader. The supervisor that ACTS — presses a proven-safe approval,
//! continues a turn, switches a model at a bucket limit, escalates — is the
//! one engine in [`crate::supervise`], hosted by the window and started by
//! `aterm drive watch|supervise`. The verbs of the deleted second stack are
//! refused by name ([`DELETED`]) with where the capability went.
//!
//! * `usage` folds THIS session's transcripts — the one Claude Code names in
//!   the environment it gives its tools (`CLAUDE_CODE_SESSION_ID`, else
//!   `CLAUDE_PID`'s sessions file), else the newest in this working
//!   directory, said so — and its subagents', through the same incremental
//!   fold the window's footer reads ([`super::session_usage`]). A filesystem
//!   fact no hook is needed for. Its one account is named by the roster's
//!   label where two or more are listed and the exec environment of the
//!   Claude Code that session runs in says which login it is
//!   ([`usage_label`]).
//! * `limits` reads the session's SCREEN over the control socket (the one
//!   interface) through the engine's own readers: the wall the last turn
//!   ended on ([`aterm_phase::wall`], the one wall classifier — the engine's
//!   turn-end policy acts on the same reading) and the windows the vendor
//!   painted on its `/usage` panel ([`usage::usage_panel_windows`]), each
//!   reset placed by the supervisor's own clock ([`limit::parse_reset`]).
//! * `disk` measures, and removes only a named safelist class under
//!   `disk.apply = true`: for build directories, an idle cargo profile's
//!   `incremental/` under cargo's own locks ([`disk::target`]), and below the
//!   automatic floor recent ones too, least recently used first, until free
//!   space is back. Every measurement, removal intent, removal and refusal is
//!   appended to `<state>/disk.jsonl` as it happens, cut back to its newest
//!   rows past [`DISK_LEDGER_MAX_BYTES`] ([`bound_ledger`]).
//! * `ledger` reads the engine's approval ledger
//!   ([`crate::supervise::approvals`], `<aterm state>/drive/<sid>.jsonl`), or
//!   `ledger disk`, the file above.
//! * `accounts` reads the roster (`accounts.toml` beside `aterm.toml`) and what
//!   this machine recorded of each account ([`super::accounts`]), and prints
//!   each one's condition and which is next. Its sub-forms WRITE, and only
//!   files: `add` appends a roster row and makes that login's directory,
//!   `use` records the owner's pin, `check` records what each login answers
//!   to `claude auth status --json` — exit status and the one key `loggedIn`,
//!   nothing else of that output read, printed or kept. `login` runs the
//!   vendor's own sign-in flow at a terminal. NOTHING HERE TOUCHES A SESSION:
//!   the move these facts feed is the WINDOW's — its supervisor relaunches a
//!   hosted Claude Code under the pinned account at its next turn end,
//!   under the next listed account at a usage limit and, under `early`,
//!   AHEAD of one (aterm-gui's `harness_host`) — and the report and `use`
//!   say what is built (that move, the usage probe that samples for it, and
//!   the limit reset, which ships off and has never been run against a real
//!   wall) and that none of it has been observed with two real accounts
//!   ([`ACCOUNTS_ROTATION`]).
//! * `upgrade` is the one verb here that ACTS: it moves a live Claude Code
//!   session onto a newer build in place, cooperatively
//!   ([`super::upgrade_drive`]), and a live Codex, its shared daemon first and
//!   then each TUI by a typed `/exit` and `codex resume`
//!   ([`super::upgrade_codex`]). It restarts the agent process, never answers
//!   a box or continues a turn; the window's host takes the same steps on its
//!   own tabs at each session's idle points, under aterm.toml's `[harness]
//!   upgrade` switch. A hand-run sweep obeys `[harness] enabled`
//!   ([`crate::supervise::SupervisorConfig::from_path`], the table's one
//!   reader).
//!
//! The hook bridge (`hook`, `statusline`, `install`, `uninstall`) is retired
//! ([`RETIRED`]) and answered before anything is parsed ([`main_entry`]).
//!
//! Every subcommand is one pass: read, decide, print, exit; nothing polls.
//!
//! * The retired `hook` and `statusline` **always exit 0** and print nothing:
//!   a bridge script an older `install` wrote still runs them, and Claude Code
//!   reads a failing hook command as a block (CHANGELOG.md, 0.92.0, "The Claude
//!   Code hooks.").
//! * Every other subcommand exits 0 on success, 1 on a refusal it can
//!   explain (`limits` with no session answering, an `upgrade` the master
//!   switch stands down, an `accounts` roster that does not admit, a label
//!   it cannot take), 2 on a usage error (the retired
//!   `install`/`uninstall` among them) — and a one-shot `upgrade` whose sweep
//!   found another sweeper holding the lock exits 75, atpkg's `EX_TEMPFAIL`
//!   for the same contention, because that one refusal means "try again
//!   later".
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested, `limits` against a
//! scripted control connection, `accounts check` and `login` against a fake
//! host ([`AccountsHost`]): no test runs `claude`, and neither verb has been
//! run against a second real account.

use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_json::{Map, Value};

use super::accounts::{self, Auth, Class, Roster};
use super::align::Captured;
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

/// Why `hook`, `statusline`, `install` and `uninstall` are retired.
pub const RETIRED: &str =
    "aterm installs nothing into an agent; a starting window removes what an older build installed";

/// The verbs deleted with the second harness stack. They are named in the
/// refusal so a script that still calls one learns where the capability went
/// rather than reading a bare "unknown subcommand". `switch` stays refused:
/// no verb moves a session by hand.
pub const DELETED: &[&str] = &[
    "status", "mark", "enable", "disable", "align", "caps", "liveness", "recover", "nudge",
    "switch", "watch", "config",
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
/// verb that exists to act on a full disk would grow its own file for ever.
pub const DISK_LEDGER_MAX_BYTES: u64 = 1024 * 1024;

/// How many of the newest rows survive a cut ([`bound_ledger`]). At most
/// [`READ_MAX_ROWS`], so the cut reads through the same bounded ring as
/// `ledger disk`.
pub const DISK_LEDGER_KEEP_ROWS: usize = 1024;

/// The durable config's home: aterm.toml,
/// [`aterm_types::dirs::aterm_config_path`], the rule `aterm-gui`'s
/// `config_path` delegates to, so `disk` reads the same `[disk]` table the
/// window's Settings page writes, on every platform.
#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    aterm_types::dirs::aterm_config_path()
}

/// The usage text: the verbs and the flags. Each verb's own page is
/// [`help_page`].
pub const USAGE: &str = "\
aterm harness — read views of the Claude Code harness, and the live agent
upgrade. The supervisor that acts on a session is the window's own host, by
default, for every Claude Code and Codex session (`aterm drive watch|supervise`
runs the same engine from a terminal).

USAGE:
    aterm harness usage [--json]                 this session's tokens per model
    aterm harness limits [@<sid>] [--json]       the wall and the painted /usage windows on the session's screen
    aterm harness disk [<build-dir> ...] [--apply <class>] [--json]
    aterm harness ledger [@<sid>] [<n>] [--json] the supervisor's approval ledger
    aterm harness ledger disk [<n>] [--json]     the disk journal
    aterm harness ledger upgrade [<n>] [--json]  the live upgrade's ledger: every act, and the owner's word
    aterm harness ledger accounts [<n>] [--json] the accounts ledger: checks, pins, limits, rotations
    aterm harness accounts [--json]              the Claude Code accounts listed on this machine: each
                                                 one's condition, and which is next
    aterm harness accounts add <label> [--shared]
                                                 list an account: make its login directory, add its row
    aterm harness accounts login <label>         sign it in: Claude Code's own flow, then record whether it took
    aterm harness accounts check                 ask each login whether it is signed in
    aterm harness accounts use <label> | --auto  the owner's word: pin one account, or end the pin
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

`status`, `mark`, `enable`, `disable`, `align`, `caps`, `liveness`, `recover`, `nudge`,
`switch`, `watch` and `config` are GONE: the engine behind `aterm drive` is the one
supervisor, and aterm.toml's [harness] table is its policy.

OPTIONS:
    --state <path>      the harness state directory: the disk journal, the upgrade records and ledger,
                        and the accounts store (<state>/accounts)
                        (default: $ATERM_HARNESS_STATE, else <aterm state>/harness/claude-harness)
    --config <path>     where the [disk] knobs are read from; accounts.toml is read beside it
                        (default: $XDG_CONFIG_HOME/aterm/aterm.toml, else $HOME/.config/...)
    --sock <path>       limits: the control socket (default: the resolved one)
    --apply <class>     disk: the ONE safelist class to act on. atpkg-gc and
                        claude-purge are surfaced and never removed from here;
                        they name the command that owns them
    --dry-run           upgrade: print each session's next step, type and signal nothing
    --status            upgrade: print each recorded upgrade, sweep nothing
    --now               upgrade <sid>: move it at its first pause (still never over a draft,
                        a box, a hold, running work or a keystroke in the last
                        20 s)
    --defer <dur>       upgrade <sid>: not for <dur> (90s, 30m, 6h, 2d; at most 30d)
    --skip              upgrade <sid>: stay on the running build until a newer target comes
    --shared            accounts add: the account is `shared` (your other machines draw on it too)
    --auto              accounts use: end the pin
    --json              the JSON form
    -h, --help          this text; `aterm harness <verb> --help` explains one verb
                        (`upgrade --status --help`: the status fields)
";

/// `aterm harness usage --help`.
const HELP_USAGE: &str = "\
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
`acct <label>` (`label` in --json) is `account` unless accounts.toml lists two or more
accounts and the run is from a Claude Code session's own tools with CLAUDE_PID set: then
it is the label of the row that process's login names, read from the environment the
process was STARTED with, never from this command's own. A login no row names, a Claude
home of its own (CLAUDE_CONFIG_DIR), a credential or a route in that environment, a
roster that is refused, the newest-transcript pick, and a CLAUDE_PID that is no longer
the Claude Code of the session folded all stay `account`; a credential given by a
settings file is not seen. No account's limits or state are read here: `aterm harness
accounts` prints them.
";

/// `aterm harness limits --help`.
const HELP_LIMITS: &str = "\
`limits` reads the session's screen once over the control socket (`text --json tail=40`)
and prints the wall the last turn ended on (the kind the supervisor acts on, its words and
its reset) and any windows a painted /usage panel shows. With no @<sid> it
reads the session it runs in ($ATERM_PARENT_SESSION_ID); with neither it refuses (exit 2)
rather than read whichever session the socket defaults to. No session answering is exit 1.
";

/// `aterm harness disk --help`.
const HELP_DISK: &str = "\
`disk` REPORTS AND REMOVES NOTHING by default. Every row carries the witness that would
make it safe to remove — for a build directory, a build tool's own marker and a cargo
profile idle past `disk.target_stale_days` (default 1 day: nothing a compile writes
touched that long) with a non-empty `incremental/` and locks no build holds — below the
floor, a recent profile's own row, oldest first (see below); or a version directory the
live symlink does not point at, or a package build the live one supersedes. Removal
needs BOTH `disk.apply = true` in aterm.toml AND an explicit `--apply <class>`; a
refusal is written to the disk journal as a denial row rather than dropped. Nothing under the transcripts root is removable under any flag.
If aterm.toml cannot be read or parsed, `disk` is not a table, or a `[disk]` key other
than `warn_free_gib` has the wrong type or sits below another table's header, every
removal is off (`disk.apply`, `disk.auto_free_gib`, `disk.dead_build_dirs`) for this verb
and for the window's automatic cleanup until it is fixed; this verb says why on stderr.
Build directories are the ones you NAME on the line; named none, it SEARCHES for them,
as the window's automatic cleanup does: this working directory and its worktree,
the checkouts in $HOME and in ~/Developer and their git worktrees two levels down (a
`target -> target.noindex` link resolved), x.py's bootstrap build directories (`build/<host>/stage<N>-<kind>`, never
a sysroot, stage0 or llvm), Claude Code's temp root (each session's scratchpad) and the cells
cache — on this working directory's volume only, never into a mount point or (macOS)
~/Desktop, ~/Documents or ~/Downloads. Under the rows it lists the largest build
directories it can NOT reclaim and what frees each. `cargo-targets` takes from each
build directory
ONLY its idle profiles' `incremental/` (the next build of each crate is a full compile
of it) — below the floor, recent profiles' too, as the window's 6-hourly look does —
deleted while holding every one of cargo's build locks of that profile — a
profile a build holds is skipped, and nothing is reached through a symlink or on another
device. Never the directory itself, `deps/`, `build/`, `.fingerprint/`, an uplifted
binary, `examples/`, `doc/` or a benchmark's data — but the build directory of a GONE
owner below the floor (below) — `deps/`, `build/` and `.fingerprint/`
are `targo clean`'s, since cargo runs tests without its locks. Each removal is journalled before
it starts and again after, with the bytes it released and every error; a removal whose
intent cannot be journalled (a full disk, no state directory) does not start. The bytes
it says it freed are COUNTED, not measured: the blocks of each deleted file that was
that file's last name. A hard-linked file frees nothing and is not counted; an
APFS clone's shared blocks are counted although the clone keeps them; a file it cannot
open to measure is deleted uncounted. The journal is cut back to its newest 1024 rows
once it passes 1 MiB. One removal needs no verb: every 6 h the window looks at free
space, and below `disk.auto_free_gib` (10 GiB; 0 or a negative value turns it off) it
searches as above (from its agents' working directories, and their CARGO_TARGET_DIR
where readable) and reclaims in the build directories on that volume by itself — that
class only, each with its witness journalled here —
LEAST RECENTLY USED FIRST: every idle profile's cache; then, WHOLE, the build directories
whose owner is gone — an ended Claude Code session's scratch build (no running Claude
Code names the session, nothing of it written for `disk.target_stale_days`, at least a
day) or the cells cache of a removed checkout, each with a signed CACHEDIR.TAG and no
build holding a lock, the owner checked again right before, and no running process with
its working directory, executable or an open file there (`disk.dead_build_dirs`,
default true; this verb's `--apply cargo-targets` takes them below the floor too); then,
one profile at a time and
oldest first, the caches of profiles a compile wrote into within `disk.target_stale_days`
(1 day), measuring free space again before each and stopping once it is back at the
floor plus 1 GiB, or once the bytes those removals released cover what free space was
short when they began (a local snapshot can keep the figure from rising), or when it
cannot be measured (a stop row in the journal says which, and where). Even then a
profile a build holds locked is never taken, nor one on another volume. No reclaim, the
window's or this verb's, takes a profile written into within the last 10 min (a build that
just finished is usually followed by another, which would write the cache straight back)
— not even at `target_stale_days = 0`, which otherwise makes every profile idle. Below
the floor this verb lists those profiles too, in the order they would go
(`under_pressure_up_to=` is what every one of them would free, an upper bound;
`reclaimable=` is what goes whole), and `--apply cargo-targets` takes them under the
same stop; at or above it, a profile inside the window is kept. So `disk.target_stale_days` says
what is IDLE — what `--apply cargo-targets` takes at any free figure, and what the
window's automatic cleanup takes whole before any recent profile; once the volume is
under the floor it does not protect a recently built checkout's cache.
";

/// `aterm harness ledger --help`.
const HELP_LEDGER: &str = "\
`ledger` prints the newest rows of the approval ledger the supervisor keeps for one
session (<aterm state>/drive/<sid>.jsonl: every box its approval policy decided —
approved, typed, skipped, escalated or deferred — with the rule and the command). With no @<sid>
it reads the session it runs in.
";

/// `aterm harness accounts --help`.
const HELP_ACCOUNTS: &str = "\
`accounts` LISTS THE CLAUDE CODE ACCOUNTS OF THIS MACHINE AND WHAT IS KNOWN OF EACH. The
roster is `accounts.toml` beside aterm.toml: [[account]] rows of `label`, `class`
(`dedicated`: this machine's alone; `shared`: your own account that your other machines
draw on too) and `dir` (the login's directory name; the one row with none is the login
this machine already has). It ships empty, and with no rows this verb says so, names the
file it read and the `add` line to run. A file that does not admit — an unknown key, a
repeated label or dir, a relative dir, two rows with no dir, more than 32 rows — is
REFUSED whole (exit 1, the reason by name). No credential is stored, read or copied: an
account is a login you made through Claude Code's own flow, known here only by its label;
no email, person or organization name is read or printed.
THE REPORT: `active: <label>, N of M sessions on it`, then one row per account — label,
class, condition (`not-enrolled`: no login was made here yet — its login directory is
missing or holds nothing, and `accounts login <label>` is what fills it; `signed-out`: the
last check said so; `limited until <time>`: a usage limit stands; `low`: the session
window at 90% or the week at 97%, until that window resets; `usable`; `unseen`: nothing
observed yet), `active`, `active:unproven` or `pinned` (`-` for neither), the limit-reset
offer last read (`offer: N left, use by <time>`, only once one was read), the sessions on
it (`on: @s-…`, `(held: jobs)` for one that could not move) — then `next:`, the account
a session at a limit would move to, or the time to wait until, and every other account's
reason by name. The session and week percent, with their resets and when they were
sampled, are `--json`'s alone (`null` for a figure nobody read, never 0). WHAT MOVES A SESSION is a window's supervisor (`[harness] accounts`, on unless
`off`): a supervised Claude Code session is relaunched on its own conversation under the
pinned account (`use`) at its next turn end, and under `next` when its turn ends on a
usage limit — one session first, the others once that account has served a turn. A
session waiting at the limit of the very account `use` names is continued where it is.
`accounts = \"off\"` stops all of that for the window's sessions: nothing is advised,
moved or proven there, and a session's own limit is still recorded for this report.
UNDER `accounts = \"early\"` (the default) a session is also moved AHEAD of a limit, at
a plain turn end: off an account another session found at its limit, or one its last
sample shows `low` by a window more than 15 minutes from its reset, to `next`. `wall`
moves a session only at a limit.
THE USAGE PROBE takes that sample, under `early` only and never with one account
listed: at an idle turn end, nobody at the tab and nothing in the composer, the
supervisor types `/usage` into the session, reads the Settings panel and closes it with
Escape — at most once in three minutes per account, ninety seconds apart across
accounts, and only where Claude Code draws full screen. It records the percentages only
of a panel read WHOLE and SETTLED, twice the same. MEASURED by hand on Claude Code
2.1.287: the panel stands whole for about a second, then grows taller than an ordinary
window (58 rows at 150 columns) and its last row goes under the fold. The read is a race
there: lost, NOTHING IS SAMPLED and the percents stay `null`. A window that never
shows the panel's last row (110x32) is not probed again by that supervisor. RUN LIVE
2026-10-02 against one real session at three sizes, with one account: it sampled at
150x80 and 150x50 and recorded nothing at 110x32. Each probe is one line in the
session's journal (<aterm state>/drive/<sid>.journal.jsonl; `aterm drive ledger @<sid>
--journal <that file>` puts it on the session's timeline): `PROBED seq=<n>
rule=usage-probe@v1 /usage session=<p>% week=<p>%` (`?%` for a figure not read), or `…
/usage no sample (<why>)`. A probe that was due and not run says why, once a point:
`SKIPPED seq=<n> rule=usage-probe@v1 gate=<word>`.
A LIMIT RESET is built and OFF BY DEFAULT (`[harness] limit_reset = true` turns it on).
At Claude Code's usage-limit dialog a window's supervisor then takes the dialog's own
reset row, matched by its exact label, and confirms only a confirmation it reads
exactly — for the limit this session's own process hit, with the row's count and day;
anything else it closes with the reset kept. It never types a command for a reset. Only
on this machine's own login (the row with no dir), with no other account on record
running within 25 hours — and under `accounts = \"off\"`, which records no session's
login, only where the roster lists no other login; once per limit on the machine; and
only where the limit stands more than a day, or the reset on offer ends within one. It
needs no roster: with none listed the login is recorded as `main` — in the ledger
(`ledger accounts`); this report lists no account without a roster. How it ended is read
from the session's transcript, never the screen — `reset-used` (the account's limits are
cleared for every session on it), `reset-kept` or `reset-failed` — and while it is being
taken no session is moved off that account. The offer column shows what a confirmation,
or a reset used or kept, last showed. NEVER RUN AGAINST A REAL WALL: every text of it was
read in Claude Code 2.1.287's bundle and none was seen on a screen.
None of it has been observed with two real accounts: the report's last line says so.
`accounts add <label> [--shared]` makes the login directory
(<state>/accounts/logins/<label>, 0700), appends the row (creating the file) and prints
the line to run next. THE ROW IS `not-enrolled` UNTIL THAT LINE HAS BEEN RUN: an empty
login directory is an account listed, not one signed in, and no session is moved to it
or offered it as `next`. The first `add` on a machine with no roster also lists this
machine's own login as `main` (dedicated, no dir), so the login its sessions already run
on is in the roster. A label is [a-z0-9][a-z0-9._-]{0,63} and not `help` (this command's
help word: a roster row named so is refused like any other bad row); one already listed
is refused (exit 1). A roster that is a symlink is written through it: the link stays and
the file it points at gets the row and keeps its mode. A roster that defines `account`
inline (`account = [ … ]`) cannot be appended to: the refusal prints the row to add by
hand (both rows, where the first `add` would have listed `main` too).
`accounts login <label>` prints `CLAUDE_CONFIG_DIR='<dir>' claude auth login` — Claude
Code's own browser flow, in a home of its own so this machine's profile is not touched —
and, at a terminal, runs it (`claude` from PATH; the dir exactly as the roster spells it,
because the login is named by a hash of that string) and WAITS for it. When the flow exits
0 it asks that one login whether it is signed in, as `check` does, prints `<label>: signed
in | signed out | not enrolled | could not run (<why>)` and records the answer: exit 0 for
`signed in`, exit 1 for anything else. A `claude` that cannot be started is said (exit 1)
with nothing asked and nothing recorded. A flow that exits non-zero, Ctrl-C included (on
unix this command waits through the key), is said (exit 1), and when it left files in the
login directory (Claude Code writes its start-up files there before any sign-in), that login
is asked and recorded as above, so the row reads `signed out`, not `unseen`. With stdin
not a terminal it prints the line, runs nothing and exits 0. With neither USER nor LOGNAME
set no login can be asked, so the flow is not started (exit 1). The row with no dir is this
machine's own login: its line is `claude auth login`, run in the default home with neither
CLAUDE_CONFIG_DIR nor CLAUDE_SECURESTORAGE_CONFIG_DIR set, and asked and recorded alike.
`accounts check` asks each enrolled login whether it is signed in: `claude auth status
--json` under CLAUDE_SECURESTORAGE_CONFIG_DIR=<dir> (empty for the row with no dir), 20 s
at most each. A row whose login directory is missing or empty is `not enrolled`: nothing
is run for it and nothing recorded (`accounts login <label>` first). It reads the exit
status and the one key `loggedIn`; the rest of that output, which names an email and an
organization, is never printed, logged or kept. One line per account: `<label>: signed in
| signed out | not enrolled | could not run (<why>)`; an answer is recorded, with a ledger
row when it changed. Exit 1 when a login could not be asked — as when neither USER nor
LOGNAME is set, where Claude Code reports a login that works as signed out: then nothing
is run and nothing recorded.
`accounts use <label>` is THE OWNER'S WORD: the account's recorded limits are cleared and
it is the active account, pinned. A label that is not listed, not enrolled or signed out
is refused by name (exit 1). `accounts use --auto` ends the pin. Both are recorded, and a
window's supervisor acts on them: each supervised Claude Code session moves to the pinned
account at its next turn end (a session waiting at a limit is told at once, and one
waiting at that account's own limit is continued there).
A state file this build cannot read as its own — another schema, unreadable, too large —
is never written over: `check` and `use` refuse (exit 1, naming the file) and the report
shows an empty state beside the reason. `accounts` takes no --dry-run and no --sock.
`ledger accounts` is the history (<state>/accounts/ledger.jsonl, cut to its newest 1024
rows past 1 MiB): `wall`, `sample`, `rotate`, `proven`, `struck`, `move`, `probe`,
`reset-used`, `reset-kept`, `reset-failed`, `auth`, `use`, `unpin`, each with its account,
session and reason. This command writes `auth`, `use` and `unpin`; a window's supervisor
writes `wall`, `move`, `rotate`, `proven` and `struck` (with the `auth` and `unpin` they
bring), under `early` `probe` (a `/usage` read claimed) and `sample` (what it read), and
with `limit_reset` on the one `reset-…` row of each limit-reset attempt, with its reason.
";

/// `aterm harness upgrade --help`: what moves a session, and when.
const HELP_UPGRADE: &str = "\
`upgrade` MOVES A LIVE CLAUDE CODE SESSION ONTO A NEWER BUILD without losing
the conversation or anything it has running. A newer build is aterm's managed
twin, or — for a session that runs Claude's own native install, the one whose
footer shows Claude Code's own `Update installed` notice — that install's
current version; never an older one.
BY DEFAULT IT ASKS NOTHING AND TYPES NOTHING (`[harness] upgrade_ask =
false`): a session is moved only at a GENUINELY IDLE point — its
turn over by Claude's own status, no shell or background task, subagent or
workflow of its own still running, no box, no draft, nobody at the tab, the
LADDER's settle below — by SIGTERM and the relaunch line, and resumed with
nothing typed after it (`done:resumed`): the tab's loop takes that point as if
nothing had moved. Until then the tab only says the newer build is installed
and that it moves over when idle, and that record always stands a look before
the move (`wait:notice-shown`). A Codex goal is never paused for it. With
`[harness] upgrade_ask = true` it runs the COOPERATIVE flow instead, the rest
of this paragraph: each sweep moves each session one step: when the
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
THE LADDER — ACTIVITY DELAYS; IT NEVER DISABLES: the longer a session has
been behind, the less the upgrade waits for a comfortable moment. Behind less
than 20 minutes it asks for the natural idle point: the screen still 20 s and
nobody who gave the tab input within
`[harness] human_grace_s`. From 20 minutes a pause that only LOOKS quiet counts:
two looks 20 s apart that read the screen idle with the same last words,
however it repainted between them (a footer's clock, a goal's counter). From
an hour a person holds it only by a keystroke in the last 20 s. From two
hours it moves at the first such pause, its settle waived. No rung ever moves
it over a draft, a box, a keystroke in the last 20 s, a hold, a usage limit or
the login wall, or running work — the agent's turn or status, the shells under
it, a turn of a Codex client's own in its daemon (`wait:daemon-turn`,
`wait:goal`, `wait:daemon-busy` for one it cannot yet place in another
session) — and an empty process tree, and under `upgrade_ask = true` the
READY answer, still come before the restart. `--now` is the last rung at once,
with the same floors.
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
apart. It hands the relaunched agent straight back to its supervisor: by default
nothing is typed (`done:resumed`); under `upgrade_ask = true` the supervisor types
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
step recorded them. A give-up or a new round, which the upgrade takes
whatever the session shows, is said with what holds the point it waits for
after `@` (`step=gave-up@box`, `step=would-rearm:unanswered@not-idle:busy`).

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
auto-update-version`, which re-runs chatgpt.com's installer outside atpkg)
is PINNED the same
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
it: Codex prints `Disconnected from this task. Any running work continues.`,
and 0.158.0 can also print `… The current turn was stopped.`) is ended by a TYPED
`/exit`. Its own conversation is the one the kernel names — every thread of
its daemon hangs from that conversation's root (read from each thread's
rollout: the subagents it spawned name it as their parent) and it is the
daemon's only client — and a turn anywhere in it, a subagent's included,
waits `wait:goal` while its footer says `Pursuing goal`, else
`wait:daemon-turn`. UNDER `upgrade_ask = true` A GOAL IS PAUSED FOR THE
MOVE: a goal starts its next turn within milliseconds of the last one's end,
so from two hours behind (or `--now`), where its own turn is all that holds
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
(`left-typed:<what>`) and left standing: never taken back, never read as a
person's draft and never sent by the supervisor; a person clears it (the
upgrade waits `wait:left-typed:unsent`); a `/exit` the TUI
outlives is never waited on again (`wait:exit-not-taken`, then
`failed:stale-exit`). A TUI that names no thread as it exits is not
relaunched (`failed:no-resume-hint`: its conversation runs on in the daemon;
`codex resume` in the tab takes it back). Its state is per TAB: `codex-<sid>`
in `--status`, the ledger and the `upgrade=` column — a move that stopped
after its `/exit` stays listed though no Codex holds it any more (in the
window for a day).
";

/// `aterm harness upgrade --status --help`: every field of a status line.
const HELP_UPGRADE_STATUS: &str = "\
`upgrade --status` READS the recorded upgrades and sweeps nothing: one line
(and under it, indented, the move and what holds it) for each a live Claude Code or Codex still holds on a build older than its
target, in that tab (a conversation that ended, or was moved onto the build by hand,
leaves its file behind; it is not listed) — the tab, the conversation, from
and to, the phase, `pending_for=`,
`wait=` (what the last step waited on: `settling`, `awaiting-ready`,
`ready-unrecorded` (the agent's READY is on its screen, not yet in its
transcript: the next look reads it), `superseded` (it answered READY, then got
more work before the restart: it is asked again when that turn ends),
`background`, `draft`, `attended`, `held`, `terminal:<owner>`, `not-idle:busy`
for a turn a hand-run sweep found in progress, `status-stale:<status>` for a status
the idle screen does not bear out — the restart's until Claude says `idle` …,
`release:<gate>` while that gate holds the line an abandoned notice owes the
agent; where no look has recorded one, a Claude Code row reads Claude's own live
status, `not-idle:waiting` for a box, never over a recorded word) with
`wait_for=`, the owner's `request=` (`-`, `now`, `defer:<time left>`,
`skip:<version>`), `next_round=` (for a round that stopped: how
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
there), `unwatched` (no look has read the record for an hour, its due watch
half an hour late: aterm's own fault — the window's host stopped, none runs
for the tab, or its watch cannot write; said over every kind below it, never
within 10 minutes of the Mac waking or booting (the window also waits 10
minutes after its host starts watching), never of a conversation running
outside every aterm tab
(no host watches it there), and the column reads
`stalled/<to>/unwatched:<span>/<age>`, how long since it was checked), or
`overdue` (6 hours behind — four
past the point where it moves at its first pause — whatever it waits on, unless the owner's `--now`
came in the last 30 minutes; and at once, whatever its age and any `--now`, for
a usage limit an hour past its end (the reset its transcript named, or for
Codex the one its wall on the screen names — plus 10 minutes under the default
flow; a week after its wait began where no reset was named, never for a spend
or credit cap) with its
notice still the recorded wait: where the reset was named,
the default upgrade moves it at its next idle point; otherwise (no reset
named, or `upgrade_ask = true`) only a turn typed in its tab moves it) —
and `held_by=` (what ran under the agent at the last look that
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
once past, `-` for a record nothing more is owed on); then `cause=` (what holds
the move, by class, whether or not anything is told of it yet: `own-work`,
`person`, `limit`, `limit-passed`, `notice`, `unreached`, `unrecorded`,
`unclassified`, …; `-` once it is under way or done) and `told=` (what the
owner is told of it: the kind of the interrupt, digest line or log fault the
move is — `own-work` after a day, `agent-down`, `harness-stuck`,
`limit-passed`, `no-progress`, … — and `-` for a record, however long it has
waited: a wait is no alert); and, only while aterm's
own fence refuses the notice due, `refused=<n>/<span>` at the very end (how
many looks in a row it could not type it, and since how long).
A round that GAVE UP is no stall: it rests until its `next_round=`,
and its column reads `pending/<to>/next-round:<span>/<age>` —
`pending/<to>/ready/<age>` while it acts on a late READY instead.
The window shows the same per tab as `upgrade=`
in `aterm ctl status`/`sessions` and records it in Settings ▸ Messages — one
record, `<agent> <build> installs itself in tab N`, saying there is nothing to
do and from what time the first pause is enough. It marks a tab
(`meta attention owner=upgrade`) and puts a row on the message band ONLY where
the owner is needed NOW and the action is theirs alone: the agent the upgrade
ended is not back (`agent-down`, and the command that resumes it), it came back
and has reached no prompt in half an hour (`relaunch-stuck`), a shutdown it
asked for has hung (`hung-exit`), or a Codex goal it paused was not resumed
(`goal-paused`). Every other wait — the
agent's own work, a person at the tab, a usage limit, a box the supervisor
answers — is this status, the ledger, the wait log and the log of Settings ▸
Messages; after a day of the agent's own work (and a whole round of notices
answered `not yet`) or a draft unsent it is one line of the exceptions digest,
and aterm's own faults are one Warn record. `--json` says which, for every
tab: `attention` (`quiet`, `fault`, `digest`, `interrupt`), `attention_kind`,
`cause` (what holds the move, as a class word), `cause_words` (the same, as a
sentence) and `waits` (the last 24 changes of
wait with the facts that decided each).
";

/// `aterm harness upgrade --help`, after [`HELP_UPGRADE`]: the owner's `--now`, `--defer` and `--skip`.
const HELP_UPGRADE_WORD: &str = "\
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
in a Codex daemon and nothing running under the agent are still required —
and, under `upgrade_ask = true`, the READY answer — and what is typed is still
fenced on the screen it was judged by).
Every word arms a NEW ROUND of the upgrade: under `upgrade_ask = true`, a
notice after it asks for a READY marker no earlier answer carries. An upgrade
the owner holds (`--defer`, `--skip`) owns none of its session's turn ends:
the tab's supervisor goes on continuing the worker as if none were pending,
and a notice already typed is ended — under `upgrade_ask = true` the hold's
end brings a fresh notice and needs a fresh READY. ONE
PLACE `--now` adds an act: an upgrade that GAVE UP (no READY answer it could act
on after the last notice) is re-armed AT ONCE rather than at its next round,
so, under `upgrade_ask = true`, the notice is typed again and, after its READY
answer, the agent is restarted. An upgrade that was REFUSED or FAILED is not
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
waiting upgrade's record offers two of them (Upgrade now where `--now` moves
it, Not today — a one-day `--defer` — and Skip version; a row that stands for
an agent that is down offers none, for none of them brings it back), Settings ▸
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
    /// `CLAUDE_CONFIG_DIR` when it is set but names no directory this command
    /// can find: a relative path, which the vendor resolves against its own
    /// working directory. Then no Claude Code directory is known
    /// ([`Env::claude_dir`]): `~/.claude` is not the one the session writes.
    pub claude_config_unusable: Option<String>,
    /// The Claude Code session this command runs under, when the vendor named
    /// it: `CLAUDE_CODE_SESSION_ID`, which Claude Code 2.1.283 sets for every
    /// tool it runs. aterm strips `CLAUDE_*` from the shells it spawns, so only
    /// Claude Code's own tools carry it.
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
    /// Claude Code's per-user temp root, where `disk` searches for the
    /// sessions' scratch builds ([`disk::discover::claude_tmp_root`]).
    pub claude_tmp: Option<PathBuf>,
    /// The cells cache root, where `disk` searches too
    /// ([`disk::discover::cells_root`]).
    pub cells: Option<PathBuf>,
    /// The running Claude Codes and the directories the processes were
    /// started with ([`disk::discover::running_claude_census`]), read for
    /// `disk` alone ([`main_entry`]). `None`, not read: no session is called
    /// ended.
    pub claude_running: Option<disk::discover::ClaudeCensus>,
}

impl Env {
    /// THE one environment read in this module that a decision rests on —
    /// but for one: [`LiveAccounts`] hands five variables through to the
    /// `claude` it runs, as they are, and does not run it when neither USER
    /// nor LOGNAME is set ([`auth_check_env`]). ANOTHER process's is read in
    /// one place, and not here: `usage` reads the exec environment of
    /// `CLAUDE_PID` for its account's label ([`usage_label`]).
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
        let now = crate::supervise::limit::unix_now();
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
        let xdg_cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from);
        let cells = home
            .as_deref()
            .map(|h| disk::discover::cells_root(h, xdg_cache.as_deref()));
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
            claude_tmp: Some(disk::discover::claude_tmp_root(disk::discover::own_uid())),
            cells,
            claude_running: None,
        })
    }

    /// The Claude Code directory: [`Env::claude_dir`], else `$HOME/.claude`,
    /// but none when `CLAUDE_CONFIG_DIR` is set and unusable
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
    /// filesystem fact, not a hook: `--bare` removes hooks, plugins and the
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

    /// The accounts roster: `accounts.toml` in the directory of
    /// [`Env::config`], so beside the `aterm.toml` this command reads
    /// (`--config` moves both) — [`accounts::roster_beside`], the rule a
    /// host reads its roster by too ([`accounts::default_roster_path`]).
    /// `None` when no config path is known (neither `$XDG_CONFIG_HOME` nor
    /// `$HOME`): then there is no roster to read.
    #[must_use]
    pub fn accounts_roster(&self) -> Option<PathBuf> {
        accounts::roster_beside(self.config.as_deref()?)
    }

    /// The accounts store: `<state>/accounts` (the state file, its ledger,
    /// the lock, and the login directories `accounts add` makes).
    #[must_use]
    pub fn accounts_store(&self) -> accounts::Store {
        accounts::Store::at(&self.state)
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
    /// The accounts ledger (`<state>/accounts/ledger.jsonl`,
    /// [`accounts::Store::ledger_path`]).
    Accounts,
}

/// One form of `accounts` (design `docs/DESIGN-claude-accounts-2026-10-01.md`
/// §4, §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountsCmd {
    /// `accounts [--json]`: the report. Writes nothing.
    Report {
        /// The JSON form.
        json: bool,
    },
    /// `accounts add <label> [--shared]`: make the login directory, append
    /// the roster row.
    Add {
        /// The label as typed; admitted when the verb runs, so an
        /// inadmissible one is a refusal by name (exit 1), not a usage error.
        label: String,
        /// `--shared`, else dedicated.
        class: Class,
    },
    /// `accounts login <label>`: the vendor's sign-in flow for that account,
    /// waited for, and what its login then answers recorded.
    Login {
        /// The label as typed.
        label: String,
    },
    /// `accounts check`: ask each enrolled login whether it is signed in.
    Check,
    /// `accounts use <label>`: the owner's pin.
    Use {
        /// The label as typed.
        label: String,
    },
    /// `accounts use --auto`: end the pin.
    Auto,
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
    /// `hook`, `statusline`, `install` or `uninstall`: retired ([`RETIRED`]).
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
        /// Build directories to consider. EMPTY: it searches for them
        /// ([`disk_roots`]).
        targets: Vec<PathBuf>,
        /// The one safelist class to act on. `None` is report-only.
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
    /// Move live Claude Code and Codex sessions onto the newer build in
    /// place, resuming the same conversation in the same tab
    /// ([`super::upgrade_drive`], [`super::upgrade_codex`]).
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
    /// The model priority list of the live upgrade: print it, with each
    /// model's availability on the managed build, the target, and Claude
    /// Code's own recommendations; or `set` it (best first).
    UpgradeModels {
        /// `Some(list)`: replace the list with this comma-separated one.
        set: Option<String>,
    },
    /// The Claude Code accounts: the report, and the roster's verbs.
    Accounts(AccountsCmd),
    /// Print a help page ([`help_page`]) and exit 0.
    Help(HelpPage),
}

/// Which help page `--help` prints: the verb list, or one verb's page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HelpPage {
    /// [`USAGE`]: every verb and flag.
    #[default]
    Verbs,
    /// `usage --help`.
    Usage,
    /// `limits --help`.
    Limits,
    /// `disk --help`.
    Disk,
    /// `ledger --help`.
    Ledger,
    /// `accounts --help`: the roster, the report and its verbs.
    Accounts,
    /// `upgrade --help`: what moves a session, and the owner's word.
    Upgrade,
    /// `upgrade --status --help`: every field of a status line.
    UpgradeStatus,
}

/// The text of `page`: for a verb, its lines of [`USAGE`] and then its
/// paragraphs.
#[must_use]
pub fn help_page(page: HelpPage) -> String {
    let (verb, status_only, paras): (&str, bool, &[&str]) = match page {
        HelpPage::Verbs => return USAGE.to_string(),
        HelpPage::Usage => ("usage", false, &[HELP_USAGE]),
        HelpPage::Limits => ("limits", false, &[HELP_LIMITS]),
        HelpPage::Disk => ("disk", false, &[HELP_DISK]),
        HelpPage::Ledger => ("ledger", false, &[HELP_LEDGER]),
        HelpPage::Accounts => ("accounts", false, &[HELP_ACCOUNTS]),
        HelpPage::Upgrade => (
            "upgrade",
            false,
            &[
                HELP_UPGRADE,
                HELP_UPGRADE_WORD,
                "The fields of `--status`: `aterm harness upgrade --status --help`.\n",
            ],
        ),
        HelpPage::UpgradeStatus => ("upgrade", true, &[HELP_UPGRADE_STATUS]),
    };
    // USAGE's entries: a line naming the verb, and the deeper-indented lines
    // that continue it.
    let mut entries: Vec<String> = Vec::new();
    for line in USAGE
        .lines()
        .skip_while(|l| *l != "USAGE:")
        .skip(1)
        .take_while(|l| !l.is_empty())
    {
        match entries.last_mut() {
            Some(last) if !line.trim_start().starts_with("aterm harness ") => {
                last.push('\n');
                last.push_str(line);
            }
            _ => entries.push(line.to_string()),
        }
    }
    let mut text = String::from("USAGE:\n");
    for entry in entries.iter().filter(|e| {
        e.split_whitespace().nth(2) == Some(verb) && (!status_only || e.contains("--status"))
    }) {
        text.push_str(entry);
        text.push('\n');
    }
    for para in paras {
        text.push('\n');
        text.push_str(para);
    }
    text.push_str("\nFlags: `aterm harness --help`.\n");
    text
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

/// Parse `args` (the operands after `harness`).
///
/// # Errors
///
/// An unknown or deleted subcommand, an unknown flag, a flag with no value,
/// a flag on a verb it does not belong to, or a non-numeric count.
pub fn parse(args: &[String]) -> Result<(Cmd, Overrides), String> {
    let mut over = Overrides::default();
    let mut rest: Vec<&str> = Vec::new();
    let mut json = false;
    let mut help = false;
    let mut apply: Option<disk::Class> = None;
    let mut dry_run = false;
    let mut shared = false;
    let mut auto = false;
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
            "--shared" => shared = true,
            "--auto" => auto = true,
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
        let page = match rest.first().copied() {
            Some("usage") => HelpPage::Usage,
            Some("limits") => HelpPage::Limits,
            Some("disk") => HelpPage::Disk,
            Some("ledger") => HelpPage::Ledger,
            Some("accounts") => HelpPage::Accounts,
            Some("upgrade") if owner.contains(&OwnerAsk::Status) => HelpPage::UpgradeStatus,
            Some("upgrade") => HelpPage::Upgrade,
            _ => HelpPage::Verbs,
        };
        return Ok((Cmd::Help(page), over));
    }
    let Some(&verb) = rest.first() else {
        return Ok((Cmd::Help(HelpPage::Verbs), over));
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
                } else if *op == "accounts" {
                    file = LedgerFile::Accounts;
                } else if op.starts_with('@') && op.len() > 1 {
                    file = LedgerFile::Approvals(Some((*op).to_string()));
                } else if let Ok(n) = op.parse::<usize>() {
                    count = n;
                } else {
                    return Err(format!(
                        "ledger takes `disk`, `upgrade`, `accounts`, an @<sid> and a row count, \
                         not {op:?}"
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
        "accounts" => {
            let form = match &rest[1..] {
                [] => AccountsCmd::Report { json },
                ["add", label] => AccountsCmd::Add {
                    label: (*label).to_string(),
                    class: if shared {
                        Class::Shared
                    } else {
                        Class::Dedicated
                    },
                },
                ["login", label] => AccountsCmd::Login {
                    label: (*label).to_string(),
                },
                ["check"] => AccountsCmd::Check,
                ["use"] if auto => AccountsCmd::Auto,
                ["use", label] if !auto => AccountsCmd::Use {
                    label: (*label).to_string(),
                },
                ["use", ..] => {
                    return Err(
                        "accounts use takes ONE of a label (`aterm harness accounts use \
                         <label>`) and --auto, which ends the pin"
                            .to_string(),
                    );
                }
                other => {
                    return Err(format!(
                        "accounts takes nothing, `add <label> [--shared]`, `login <label>`, \
                         `check`, or `use <label> | --auto`, not {other:?}"
                    ));
                }
            };
            if json && !matches!(form, AccountsCmd::Report { .. }) {
                return Err(
                    "--json is the report's form (`aterm harness accounts --json`): the other \
                     forms print lines"
                        .to_string(),
                );
            }
            Cmd::Accounts(form)
        }
        deleted if DELETED.contains(&deleted) => {
            // `config` once held the live upgrade's switch: say where it went.
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
    if shared && !matches!(cmd, Cmd::Accounts(AccountsCmd::Add { .. })) {
        return Err("--shared belongs to `accounts add <label>`".to_string());
    }
    if auto && !matches!(cmd, Cmd::Accounts(AccountsCmd::Auto)) {
        return Err("--auto belongs to `accounts use`".to_string());
    }
    // A flag `accounts` does not take is refused, never ignored: `add`,
    // `check`, `login` and `use` WRITE, and one that swallowed `--dry-run`
    // would write for real under a flag that promises it will not.
    if matches!(cmd, Cmd::Accounts(_)) {
        if dry_run {
            return Err(
                "accounts takes no --dry-run: `add`, `login`, `check` and `use` do what they \
                 say, and the report writes nothing"
                    .to_string(),
            );
        }
        if over.sock.is_some() {
            return Err("accounts takes no --sock: it reads no session".to_string());
        }
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
/// outside `[A-Za-z0-9-]` folded to `-` (as `~/.claude/projects` names them).
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

/// The byte cap on what this command folds, over all of the session's files:
/// its own cap, since it runs once and the window's per-read budget
/// (`footer::FOLD_BUDGET`) does not apply. The fold streams, so the cost is
/// one pass and no line is held; bytes past the cap are not folded, and only
/// then does the output say the totals are a prefix.
pub const MAX_TRANSCRIPT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Which transcript `usage` folds, and how it was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// The session Claude Code named in this command's environment
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
    /// Nothing named a session: the newest `.jsonl` in this working
    /// directory's project directory, which may be another session's.
    Newest(PathBuf),
    /// Nothing to fold.
    None,
}

/// Choose the transcript `usage` folds: the session Claude Code named
/// ([`Env::claude_session`], then [`Env::claude_pid`]'s sessions file), else
/// the newest in this working directory's project directory (the one rung
/// left when nothing names the session: aterm's own shells carry no
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

/// The label `usage` gives the one account it shows when nothing names it.
const USAGE_ACCOUNT: &str = "account";

/// What `usage` asks the kernel of the Claude Code `CLAUDE_PID` names, for
/// its account's label ([`usage_label`]). [`LiveClaude`] asks; a test answers.
trait ClaudeProcess {
    /// When the kernel started `pid`, unix seconds.
    fn started(&self, pid: u32) -> Option<u64>;
    /// The environment `pid` was started with, as `KEY=value` entries.
    fn exec_env(&self, pid: u32) -> Option<Vec<String>>;
}

/// The kernel's own answers: the start record (`aterm_uds::process`, `None`
/// on a platform that keeps none: then no account is named) and atpkg's
/// `process_args`, the read the window's recorder makes of a Claude Code.
struct LiveClaude;

impl ClaudeProcess for LiveClaude {
    fn started(&self, pid: u32) -> Option<u64> {
        aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?).map(|born| born.seconds)
    }

    fn exec_env(&self, pid: u32) -> Option<Vec<String>> {
        atpkg::caller_shell::process_args(pid).map(|args| args.env)
    }
}

/// The label of the account `usage` shows: the roster's
/// ([`accounts::footer_label`], the window footer's rule: two or more
/// listed) for the login the Claude Code this command runs under was started
/// with — the exec environment of `CLAUDE_PID`, never this command's own: a
/// child that did not inherit the login variable would read as the default
/// login. Else [`USAGE_ACCOUNT`], and nothing is guessed:
///
/// * a transcript picked as the newest, which may be another session's;
/// * no `CLAUDE_PID`, or a roster that is missing, refused or lists fewer
///   than two — the process is not read then;
/// * an environment that cannot be read or reads empty, a login no row
///   names, a home of its own, a credential or a route in that environment;
/// * a pid that is not PROVEN to be the Claude Code of the session folded:
///   its registry row must name that session and the second the kernel
///   started the process (a shell that outlived its Claude Code keeps
///   `CLAUDE_PID`, and the kernel hands the number to another process).
///   The start is read AFTER the environment, so the process that was read
///   is the one the row is checked against.
fn usage_label(env: &Env, pick: &Pick, process: &dyn ClaudeProcess) -> String {
    let label = || {
        let Pick::Named { path, .. } = pick else {
            return None;
        };
        let pid = env.claude_pid?;
        let roster = Roster::load(&env.accounts_roster()?)
            .ok()
            .filter(Roster::several)?;
        let who = accounts::ProcessLogin::of_exec_env(&process.exec_env(pid)?)?;
        let started = process.started(pid)?;
        let entry = footer::session_of_pid(&env.claude_dir()?, pid, Some(started))?;
        let folded = path.file_stem().and_then(|stem| stem.to_str());
        let proven = entry.proc_start == Some(started) && folded == Some(entry.session_id.as_str());
        accounts::footer_label(&roster, &who)
            .filter(|_| proven)
            .map(str::to_owned)
    };
    label().unwrap_or_else(|| USAGE_ACCOUNT.to_owned())
}

/// `aterm harness usage`.
fn run_usage(env: &Env, process: &dyn ClaudeProcess, json: bool, out: &mut dyn Write) -> ExitCode {
    let pick = pick_transcript(env);
    let (path, source, pick_word) = match &pick {
        Pick::Named { path, how } => (Some(path.clone()), Source::Transcript, *how),
        Pick::Newest(path) => (Some(path.clone()), Source::TranscriptNewest, "newest-mtime"),
        Pick::NamedButAbsent { .. } | Pick::None => (None, Source::None, "none"),
    };
    // The window's footer folds through the same `SessionUsage`; one refresh
    // here is the whole pass, up to this command's own cap. One sum,
    // `total_fold`, feeds the view, the JSON and the text alike.
    let folded = path.map(|path| {
        let mut session = SessionUsage::new(path);
        let refresh = session.refresh(MAX_TRANSCRIPT_BYTES);
        let total = session.total_fold();
        (session, refresh, total)
    });
    let mut view = UsageView::new(env.now);
    if let Some((_, _, total)) = &folded {
        let mut account = AccountView::new(usage_label(env, &pick, process), true);
        account.add_transcript(total, &usage::PriceTable::new());
        view.accounts.push(account);
    }
    if json {
        // Schema 1: keys are only ever added, never changed.
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
    )
    .map(|facts| {
        facts.with_runtime(if refresh.caught_up {
            session.busy_ms()
        } else {
            0
        })
    });
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

/// What one screen read says about a limit: the wall the worker's last turn
/// ended on, read by the engine's own reader ([`aterm_phase::wall`], the one
/// wall classifier the turn-end policy also decides on), and the windows the
/// vendor painted on its `/usage` panel ([`usage::usage_panel_windows`]).
/// Every reset is placed on the clock by the supervisor's grammar
/// ([`limit::parse_reset`], [`limit::reset_at`]).
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
/// `zone_offset_at` does not know) is at `local_offset_s`. A zone's offset is
/// read AT the instant placed for ([`limit::zone_offset_at`] in production), as
/// the loop that waits for the reset reads it, so a reset across a
/// daylight-saving change shows the time the supervisor will continue at.
#[must_use]
pub fn limits_view(
    rows: &[String],
    now: i64,
    local_offset_s: i64,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> LimitsView {
    let place = |text: &str| {
        limit::parse_reset(text)
            .map(|spec| limit::reset_at_with(&spec, now, local_offset_s, zone_offset_at))
    };
    let wall = aterm_phase::wall(rows);
    // The wall's reset is placed as the loop that waits for it places it: a
    // session window's by the five-hour rule, any other wall's bare clock as
    // the first occurrence after the print the screen's done row gives, so
    // `harness limits` says the time the supervisor will continue at.
    let resets_at = wall.as_ref().and_then(|w| {
        let spec = limit::parse_reset(w.reset.as_deref()?)?;
        let window = w.kind == aterm_phase::WallKind::UsageSession
            && limit::names_a_session_window(&w.message);
        let printed = limit::printed_by_the_screen(rows, now, local_offset_s, false);
        Some(limit::place_reset_with(
            &spec,
            window,
            now,
            printed,
            local_offset_s,
            zone_offset_at,
        ))
    });
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
/// used_pct, reset, resets_at}]}`.
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
    // No session named and none to be in: refuse, rather than send an
    // unaddressed read that classifies whichever session the socket defaults
    // to and print a verdict that does not say whose it is.
    let Some(target) = env.target(sid) else {
        let _ = writeln!(err, "aterm harness limits: no session named; pass @<sid>");
        return ExitCode::from(2);
    };
    match Session::new(ctl, Some(target.clone())).read_screen() {
        Ok(screen) => {
            let view = limits_view(
                &screen.rows,
                env.now,
                local_offset_s,
                &limit::zone_offset_at,
            );
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
/// [`disk::scan`], which reads no environment of its own. With no build
/// directory named on the line it SEARCHES ([`disk::discover::discover`]):
/// this working directory and its worktree, the checkouts in `$HOME` and in
/// `~/Developer` and their worktrees, Claude Code's temp root and the cells cache.
fn disk_roots(env: &Env, targets: &[PathBuf]) -> disk::Roots {
    let home = env.home.clone();
    // Who may hold a session: the running Claude Codes
    // ([`Env::claude_running`]); none ends while one that may be a Claude
    // Code cannot be read. Its end is read in `~/.claude`, in this verb's
    // own Claude Code directory ([`Env::claude_dir`]) and in each one a
    // running process was started with.
    let (running, claude_unread) = match env.claude_running.clone() {
        Some(Ok(running)) => (running, None),
        Some(Err(why)) => (disk::discover::Running::default(), Some(why)),
        None => (
            disk::discover::Running::default(),
            Some("the running processes were not read".to_owned()),
        ),
    };
    let claude_dirs = disk::discover::claude_dirs(
        home.as_deref(),
        env.claude_dir().into_iter().chain(running.dirs),
    );
    let claudes = running.claudes;
    let found = if targets.is_empty() {
        // Only on the volume whose free space this verb measures (its
        // working directory's): one elsewhere would free nothing there.
        let roots = disk::discover::Roots {
            cwds: vec![env.cwd.clone()],
            home: home.clone(),
            claude_dirs: claude_dirs.clone(),
            claudes: claudes.clone(),
            claude_unread: claude_unread.clone(),
            agent_target_dirs: Vec::new(),
            claude_tmp: env.claude_tmp.clone(),
            cells: env.cells.clone(),
            device: disk::discover::device_of(&env.cwd),
            mounts: disk::discover::mount_points(),
        };
        disk::discover::discover(&roots, disk::discover::Budget::SHIPPED)
    } else {
        disk::discover::Discovered::default()
    };
    disk::Roots {
        found: found.found,
        found_notes: found.notes,
        home: home.clone(),
        claude_dirs,
        claudes,
        claude_unread,
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

/// The `[disk]` knobs that govern a removal. A value of one that this
/// reader cannot read turns every removal off ([`removal_off`]): what the
/// owner meant by it is unknown, and a reading that removes nothing cannot
/// remove against them. `warn_free_gib` words a warning and removes nothing,
/// so an unreadable value of it is its default.
const DISK_REMOVAL_KEYS: [&str; 4] = [
    "apply",
    "target_stale_days",
    "auto_free_gib",
    "dead_build_dirs",
];

/// What [`removal_off`] does, as a note says it.
const DISK_OFF: &str = "so every removal is off (disk.apply false, auto_free_gib 0, \
                        dead_build_dirs false) until it is fixed";

/// `config` with every removal off: the owner's verb (`apply`), the
/// automatic floor (`auto_free_gib = 0`) and the whole-directory reclaim
/// (`dead_build_dirs`). The warning threshold and the idle window keep their
/// readings; neither removes anything by itself.
fn removal_off(config: disk::Config) -> disk::Config {
    disk::Config {
        apply: false,
        auto_free_gib: 0,
        dead_build_dirs: false,
        ..config
    }
}

/// What an aterm.toml says of the `[disk]` knobs ([`disk_config_read`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskReading {
    /// The knobs the verb and the window's disk watch act on: every removal
    /// off ([`removal_off`]) where the file does not say what the owner meant
    /// by a removal knob.
    pub config: disk::Config,
    /// The automatic floor the file sets, in GiB, as read before any removal
    /// was turned off (`0` where it is off, the default where it is not
    /// written). `None` where the file does not say it: a file nothing can
    /// read, a `disk` that is not a table, an `auto_free_gib` of another type
    /// or one TOML files under another table.
    pub floor_gib: Option<u64>,
    /// One line for each value not read as written. None quotes a value or a
    /// line of the file, which can hold a secret (a proxy's password).
    pub notes: Vec<String>,
    /// Whether [`Self::config`] is [`removal_off`] because the file does not
    /// say what the owner meant by a removal knob: then `config.apply` is
    /// `false` whatever the file sets it to, and a note says which value.
    pub off_by_fault: bool,
}

impl DiskReading {
    fn of(config: disk::Config) -> Self {
        DiskReading {
            config,
            floor_gib: Some(config.auto_free_gib),
            notes: Vec::new(),
            off_by_fault: false,
        }
    }

    /// Every removal off, the floor unknown, and `note` said
    /// ([`removal_off`]): a file of which no knob can be read.
    fn unread(note: String) -> Self {
        DiskReading {
            config: removal_off(disk::Config::default()),
            floor_gib: None,
            notes: vec![note],
            off_by_fault: true,
        }
    }

    /// Whether `free` bytes is at or above the floor the file sets
    /// ([`Self::floor_gib`]), counted as [`disk::Config::below_auto_floor`]
    /// counts it (a floor of `0`, or an unknown figure, is never crossed).
    /// `false` where the floor cannot be read: a reading that turned the
    /// removal off because the file could not be read cannot say the volume
    /// has space back.
    #[must_use]
    pub fn at_or_above_floor(&self, free: Option<u64>) -> bool {
        self.floor_gib.is_some_and(|floor| {
            !disk::Config {
                auto_free_gib: floor,
                ..self.config
            }
            .below_auto_floor(free)
        })
    }
}

/// The `[disk]` knobs ([`disk_config_read`]) of `env`'s aterm.toml.
fn disk_config(env: &Env) -> DiskReading {
    disk_config_read(env.config.as_deref())
}

/// The `[disk]` knobs ([`disk::KEYS`]) of the `aterm.toml` at `path`, the
/// floor it sets, and a note for each value not read as written
/// ([`disk_config_from`]). No path, or no file, is every knob at its default;
/// a file that exists and cannot be read as text (unreadable, or not UTF-8,
/// which TOML requires) is [`removal_off`] with no floor read, said.
#[must_use]
pub fn disk_config_read(path: Option<&Path>) -> DiskReading {
    let Some(path) = path else {
        return DiskReading::of(disk::Config::default());
    };
    match std::fs::read_to_string(path) {
        Ok(text) => disk_config_from(&text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => DiskReading::of(disk::Config::default()),
        Err(e) => DiskReading::unread(format!(
            "{}: cannot be read ({e}), {DISK_OFF}",
            path.display()
        )),
    }
}

/// The `[disk]` knobs of an aterm.toml's `text`, the floor it sets, and a
/// note for each value not read as written.
///
/// Read with the window's own parser, [`aterm_toml`], so every spelling it
/// reads reads the same here: a `[disk]` header, a dotted `disk.key` (spaced
/// or quoted), an inline `disk = { ... }`, a quoted key, and an integer with
/// `_` separators or a `0x`/`0o`/`0b` prefix. Where the text does not say what
/// the owner meant by a removal knob ([`DISK_REMOVAL_KEYS`]), the reading is
/// [`removal_off`]:
///
/// - a text the parser refuses (a typo anywhere, a key said twice): none of
///   its knobs can be read, an `auto_free_gib = 0` included;
/// - a `disk` that is not a table, or a removal knob of another type
///   (`auto_free_gib = "off"`, `apply = "yes"`, `target_stale_days = 30.0`);
/// - a knob of [`disk::KEYS`] that TOML files under another table:
///   `disk.auto_free_gib = 0` written below `[theme]` is
///   `theme.disk.auto_free_gib`, which nothing reads, and an off switch must
///   not turn into the default because of where in the file it was written.
///   A removal knob there is off whatever its value; a misplaced
///   `warn_free_gib` changes nothing.
///
/// The floor ([`DiskReading::floor_gib`]) is unknown where the parser refuses
/// the text, where `disk` is not a table, and where `auto_free_gib` is of
/// another type or written under another table; otherwise it is
/// `auto_free_gib` as read, before any removal is turned off.
///
/// A negative number is read as [`disk::Config::negative_reading`] says,
/// the words the settings checker shows on the line: `auto_free_gib` as `0`,
/// off; `target_stale_days` and `warn_free_gib` as their defaults. A
/// `warn_free_gib` of another type is its default, and a key `[disk]` does
/// not have changes nothing. Each of these is a note; no note quotes a value
/// or a line of the file, which can hold a secret (a proxy's password).
fn disk_config_from(text: &str) -> DiskReading {
    disk_read(text, &mut Vec::new())
}

/// A value of an aterm.toml that the `[disk]` reader ([`disk_config_from`])
/// does not read as written, for its type or for where TOML files it
/// ([`disk_faults`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFault {
    /// The dotted path TOML files the value at: `disk`, `disk.apply`,
    /// `harness.disk.auto_free_gib`.
    pub path: String,
    /// The reader's note on it, as [`DiskReading::notes`] holds it.
    pub note: String,
    /// Whether it turned every removal off ([`removal_off`]). Otherwise it
    /// is read as its default (a `warn_free_gib` of another type) or changes
    /// nothing (a `warn_free_gib` under another table).
    pub removal_off: bool,
}

/// The values of an aterm.toml's `text` that the `[disk]` reader
/// ([`disk_config_from`]) does not read as written, each with the reader's
/// note on it and the dotted path TOML files it at: a `disk` that is not a
/// table, a knob of [`disk::KEYS`] of another type (a float where a whole
/// number goes included), and a knob of [`disk::KEYS`] written below another
/// table's header. These are what the settings checker shows on the line, in
/// the reader's words. A negative number and a key `[disk]` does not have are
/// not among them, and a text the parser refuses has none: no value of it can
/// be read.
#[must_use]
pub fn disk_faults(text: &str) -> Vec<DiskFault> {
    let mut faults = Vec::new();
    disk_read(text, &mut faults);
    faults
}

/// [`disk_config_from`], with each value it does not read as written for its
/// type or place also pushed to `faults` ([`disk_faults`]).
fn disk_read(text: &str, faults: &mut Vec<DiskFault>) -> DiskReading {
    use aterm_toml::Value as T;
    let d = disk::Config::default();
    let doc = match aterm_toml::from_str::<T>(text) {
        Ok(doc) => doc,
        Err(e) => {
            // The line number only: the parser's own text quotes the line.
            // End of input is on the last line, as the parser counts it.
            let at = e.span().map_or_else(String::new, |s| {
                let head = &text.as_bytes()[..s.start.min(text.len().saturating_sub(1))];
                let line = 1 + head.iter().filter(|b| **b == b'\n').count();
                format!(" at line {line}")
            });
            return DiskReading::unread(format!("aterm.toml is not valid TOML{at}, {DISK_OFF}"));
        }
    };
    let article = |kind: &str| {
        if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
            "an"
        } else {
            "a"
        }
    };
    let mut cfg = d;
    let mut notes = Vec::new();
    let mut off = false;
    let mut floor_read = true;
    match doc.get("disk") {
        None => {}
        Some(T::Table(table)) => {
            for (key, value) in table {
                let key = key.as_str();
                match (key, value) {
                    ("apply", T::Boolean(on)) => cfg.apply = *on,
                    ("dead_build_dirs", T::Boolean(on)) => cfg.dead_build_dirs = *on,
                    ("warn_free_gib" | "target_stale_days" | "auto_free_gib", T::Integer(n)) => {
                        if *n < 0
                            && let Some(reading) = disk::Config::negative_reading(key)
                        {
                            notes.push(format!("disk.{key} is negative; it is read as {reading}"));
                        }
                        match key {
                            "warn_free_gib" => {
                                cfg.warn_free_gib = u64::try_from(*n).unwrap_or(d.warn_free_gib);
                            }
                            "target_stale_days" if *n >= 0 => cfg.target_stale_days = *n,
                            "target_stale_days" => {}
                            _ => cfg.auto_free_gib = u64::try_from(*n).unwrap_or(0),
                        }
                    }
                    _ if !disk::KEYS.contains(&key) => notes.push(format!(
                        "disk.{key} is not a [disk] key, so it changes nothing (the keys: {})",
                        disk::KEYS.join(", ")
                    )),
                    _ => {
                        let kind = value.type_str();
                        let wanted = if matches!(key, "apply" | "dead_build_dirs") {
                            "true or false"
                        } else {
                            "a whole number"
                        };
                        floor_read &= key != "auto_free_gib";
                        let removal = DISK_REMOVAL_KEYS.contains(&key);
                        off |= removal;
                        let effect = if removal {
                            DISK_OFF.to_owned()
                        } else {
                            format!("so it is read as the default, {}", d.warn_free_gib)
                        };
                        let note = format!(
                            "disk.{key} must be {wanted}, not {} {kind}, {effect}",
                            article(kind)
                        );
                        faults.push(DiskFault {
                            path: format!("disk.{key}"),
                            note: note.clone(),
                            removal_off: removal,
                        });
                        notes.push(note);
                    }
                }
            }
        }
        Some(other) => {
            off = true;
            floor_read = false;
            let kind = other.type_str();
            let note = format!(
                "disk must be a table, not {} {kind}, {DISK_OFF}",
                article(kind)
            );
            faults.push(DiskFault {
                path: "disk".to_owned(),
                note: note.clone(),
                removal_off: true,
            });
            notes.push(note);
        }
    }
    let mut below = Vec::new();
    disk_tables_below(&doc, String::new(), &mut below);
    for (at, table) in below {
        for key in table.keys().filter(|k| disk::KEYS.contains(&k.as_str())) {
            let removal = DISK_REMOVAL_KEYS.contains(&key.as_str());
            off |= removal;
            floor_read &= key != "auto_free_gib";
            let effect = if removal {
                DISK_OFF
            } else {
                "where nothing reads it, so it changes nothing"
            };
            let path = format!("{at}.disk.{key}");
            let note = format!(
                "disk.{key}: written below another table's header, which TOML files as \
                 `{path}`, {effect}"
            );
            faults.push(DiskFault {
                path,
                note: note.clone(),
                removal_off: removal,
            });
            notes.push(note);
        }
    }
    DiskReading {
        config: if off { removal_off(cfg) } else { cfg },
        floor_gib: floor_read.then_some(cfg.auto_free_gib),
        notes,
        off_by_fault: off,
    }
}

/// Every `disk` table in `v` below its root, with the dotted path of the
/// table that holds it (`theme`, `keybind[0]`), at any depth, arrays of
/// tables included: the walk `crate::supervise::config` makes for `harness`.
fn disk_tables_below<'a>(
    v: &'a aterm_toml::Value,
    at: String,
    out: &mut Vec<(String, &'a aterm_toml::Table)>,
) {
    let join = |seg: &str| {
        if at.is_empty() || seg.starts_with('[') {
            format!("{at}{seg}")
        } else {
            format!("{at}.{seg}")
        }
    };
    match v {
        aterm_toml::Value::Table(t) => {
            for (k, v) in t {
                if k == "disk"
                    && !at.is_empty()
                    && let Some(d) = v.as_table()
                {
                    out.push((at.clone(), d));
                } else {
                    disk_tables_below(v, join(k), out);
                }
            }
        }
        aterm_toml::Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                disk_tables_below(v, join(&format!("[{i}]")), out);
            }
        }
        _ => {}
    }
}

/// What one tick of the host's disk watch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskTick {
    /// Free space is at or above the automatic floor, or unknown: nothing was
    /// scanned, journalled or removed.
    Plenty,
    /// Below the floor: the report was journalled and the automatic grant
    /// applied ([`disk::apply_auto`]); with it, the largest build
    /// directories the pass LEFT ([`disk::hogs_left`]: one whose removal
    /// was refused, or went only in part, among them; one something else
    /// removed during the pass never).
    Reclaimed(disk::Applied, Vec<disk::Hog>),
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
    /// `atpkg`); `None` fails open.
    pub free: Option<u64>,
    /// Build directories named by the host. Only the ones on
    /// [`Self::volume`]'s volume are looked at ([`on_volume`]).
    pub targets: &'a [PathBuf],
    /// Where to SEARCH for build directories below the floor
    /// ([`disk::discover::discover`]): the agents' working directories and
    /// their worktrees, `$HOME`'s and `~/Developer`'s checkouts, the agents' `CARGO_TARGET_DIR`,
    /// Claude Code's temp root, the cells cache. Searched only below the
    /// floor, and only what is on [`Self::volume`]'s volume is surveyed.
    pub discover: Option<&'a disk::discover::Roots>,
    /// Where transcripts live: nothing under it is ever removable.
    pub transcripts: Option<&'a Path>,
    /// Unix seconds.
    pub now: i64,
}

/// One tick of the window's disk watch (design §5.5's 6 h timer, hosted by
/// `aterm-gui`'s harness host): below the automatic floor
/// ([`disk::Config::below_auto_floor`], `[disk] auto_free_gib`), survey the
/// look's targets, journal the report into `<state>/disk.jsonl`, and reclaim
/// what [`disk::auto_plan`] grants through `remove` ([`disk::remover`] is the
/// real one: a profile's `incremental/`, under cargo's locks; every idle one,
/// then recent ones least recently used first, `measure` read again before
/// each of those and the pass stopped once free space is back at
/// [`disk::Config::pressure_target_bytes`]), journalling an intent row
/// before each removal and its outcome (bytes actually released, what went,
/// what was skipped, every delete error) or a denial row after, each as it
/// happens. A removal whose intent row could not be written does not start:
/// it is a denial ([`disk::Refusal::Unjournalled`]). At or above the floor it
/// reads nothing but the config. A target on another volume than the one
/// measured is not looked at: removing it would free nothing where space ran
/// short ([`on_volume`]).
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
        discover,
        transcripts,
        now,
    } = *look;
    if !config.below_auto_floor(free) {
        return DiskTick::Plenty;
    }
    let found = discover.map_or_else(disk::discover::Discovered::default, |r| {
        disk::discover::discover(r, disk::discover::Budget::SHIPPED)
    });
    let paths: Vec<PathBuf> = found.found.iter().map(|f| f.path.clone()).collect();
    let here = on_volume(volume, &paths, &volume_id);
    let roots = disk::Roots {
        volume: Some(volume.to_path_buf()),
        targets: on_volume(volume, targets, &volume_id),
        found: found
            .found
            .into_iter()
            .filter(|f| here.contains(&f.path))
            .collect(),
        found_notes: found.notes,
        home: discover.and_then(|r| r.home.clone()),
        claude_dirs: discover.map(|r| r.claude_dirs.clone()).unwrap_or_default(),
        claudes: discover.map(|r| r.claudes.clone()).unwrap_or_default(),
        claude_unread: discover.and_then(|r| r.claude_unread.clone()),
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
    // A build directory is named only while it is there: one SEEN gone
    // (`NotFound`) holds nothing. One that cannot be looked at stays named.
    let there = |p: &Path| {
        !std::fs::symlink_metadata(p).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
    };
    let hogs = disk::hogs_left(&survey, &rep, &done, &there);
    DiskTick::Reclaimed(done, hogs)
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
    let DiskReading {
        config,
        notes,
        off_by_fault,
        ..
    } = disk_config(env);
    for note in &notes {
        let _ = writeln!(err, "aterm harness disk: {note}");
    }
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
    // it was NAMED, on whatever volume they are: `device` is not fenced. The
    // ones it SEARCHED for are on its working directory's volume only (the
    // search's own device filter). A pressure row carries the device it was
    // measured on, and free space is measured again (`df`) before each.
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
            // went, what was kept and why, every delete error, so a partial
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
            "  under pressure: the least-recently-used rows go in the order listed, until free space is back or what they freed covers the shortfall; under_pressure_up_to= is the most they would free"
        );
    }
    for note in &rep.notes {
        let _ = writeln!(out, "  note: {note}");
    }
    if rep.rows.is_empty() {
        let _ = writeln!(out, "  nothing to reclaim");
    }
    if !rep.hogs.is_empty() {
        let _ = writeln!(out, "  the largest it cannot reclaim:");
        for hog in &rep.hogs {
            let _ = writeln!(out, "    {}", hog.line(rep.home.as_deref()));
        }
    }
    let config_file = env
        .config
        .as_deref()
        .map_or_else(|| "aterm.toml".to_string(), |p| p.display().to_string());
    // What turns removal on where `disk.apply` reads off: the value the
    // stderr note names, where a fault turned every removal off (setting
    // `apply = true` again would change nothing, and a second one is a key
    // said twice, which the parser refuses); the switch itself otherwise.
    let remedy = if off_by_fault {
        format!("fix what the stderr note names in {config_file}")
    } else {
        format!("set `[disk] apply = true` in {config_file}")
    };
    match &done {
        None => {
            // Only the classes with a row it may remove, and what else a
            // removal needs: nothing to offer is said by the rows alone.
            let classes: Vec<&str> = disk::Class::ALL
                .iter()
                .filter(|c| rep.rows.iter().any(|r| r.class == **c && r.removable))
                .map(|c| c.as_str())
                .collect();
            if !classes.is_empty() {
                let config = if rep.config.apply {
                    String::new()
                } else {
                    format!("; also {remedy}")
                };
                let _ = writeln!(
                    out,
                    "report only: to remove, add --apply <class> ({}){config}",
                    classes.join(", ")
                );
            }
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
                let fix = if matches!(refusal, disk::Refusal::ApplyOff(_)) {
                    format!("; {remedy}")
                } else {
                    String::new()
                };
                let _ = writeln!(out, "  denied: {}{fix}", refusal.describe());
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
    crate::durable::sync_all(&f.into_inner().map_err(io::IntoInnerError::into_error)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// The ledger file a [`LedgerFile`] names, or why there is none.
fn ledger_path(env: &Env, file: &LedgerFile) -> Result<PathBuf, String> {
    match file {
        LedgerFile::Disk => Ok(env.disk_ledger()),
        LedgerFile::Accounts => Ok(env.accounts_store().ledger_path()),
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

// The list fits, checked here rather than trusted: it is bounded where it is
// written (`upgrade::held_list`): at most `upgrade::HELD_NAMED` processes
// named, each by a name of at most 32 characters, a command of at most
// `upgrade::HELD_COMMAND_CHARS` and at most 32 characters of pid, age and
// punctuation, any character at most 4 bytes, after the give-up's own words
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
    // alone run past the usual cut: [`GAVE_UP_DETAIL_BYTES`] holds it whole.
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

/// One accounts ledger row as a line a person reads: time, kind, the account
/// (and the one moved or rotated to), the session, then the reason. A line
/// that is no row of that ledger ([`accounts::Event::of_json_line`]) is shown
/// as it is, folded onto one bounded line.
fn accounts_ledger_line(row: &str) -> String {
    let Some(event) = accounts::Event::of_json_line(row) else {
        return super::one_line(row, 200);
    };
    let mut line = format!(
        "{} {} {}",
        rfc3339_utc(event.t),
        event.kind.as_str(),
        event.account
    );
    if let Some(to) = &event.to {
        line.push_str(&format!(" -> {to}"));
    }
    if let Some(sid) = &event.sid {
        line.push_str(&format!(" session={sid}"));
    }
    if !event.why.is_empty() {
        line.push_str(&format!(" — {}", event.why));
    }
    line
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
            (false, LedgerFile::Accounts) => {
                let _ = writeln!(out, "{}", accounts_ledger_line(row));
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
/// ([`upgrade_pass`]), kept stable so scripts that key on it still read it.
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
        ask: false,
        // Two or more accounts listed: the home's model caches are whichever
        // login started last, and are not read (`Opts::several_logins`).
        several_logins: accounts::several_listed(env.accounts_roster().as_deref()),
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

/// `aterm harness upgrade`: sweep every live Claude Code session once and
/// move each one step toward the newer build
/// ([`super::upgrade_drive::sweep`]). A session that is waiting is a line,
/// not a failure; a sweep that did not run is a line and an exit code
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
        // `[harness] upgrade_ask`: the notice and its READY first, or —
        // the default — nothing typed before a move at a genuinely idle point.
        ask: policy.upgrade_ask,
        // Two or more accounts listed: the home's model caches are whichever
        // login started last, and are not read (`Opts::several_logins`).
        several_logins: accounts::several_listed(env.accounts_roster().as_deref()),
    })
}

/// `aterm harness upgrade [<sid>] --status` and `upgrade <sid>
/// --now|--defer|--skip` ([`OwnerAsk`]): the recorded upgrades read, or the
/// owner's word written ([`super::upgrade_drive::ask`]). Nothing is swept,
/// typed or signalled. A word the busy lock kept out exits 75, like a step
/// that could not run; any other refusal exits 1. With `[harness] enabled`
/// or `upgrade` off the word is still written (it is the owner's) and stderr
/// says what it waits on: with `enabled` off nothing moves until it is back
/// on; with `upgrade` off the window takes no upgrade step, so the word takes
/// effect at the next hand-run `aterm harness upgrade <sid>` or once the
/// switch is back on.
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
            writeln!(out, "{}\n  {}", row.line(now), row.summary(now))
        };
    };
    let word = match ask {
        OwnerAsk::Status => {
            // Another supervisor's claim is read only while the window's host
            // runs: with a switch off, the switch is why nothing watches.
            let (rows, vetted) =
                super::upgrade_drive::status_rows(&opts, policy.enabled && policy.upgrade);
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
            // No window watches a record while a switch is off (design
            // `docs/DESIGN-upgrade-never-stuck-2026-09-28.md`, C10): the
            // window's host stands down, so a record reads `unwatched` by the
            // owner's own choice, said once, beside the rows.
            if let Some(path) = env
                .config
                .as_deref()
                .filter(|_| !(policy.enabled && policy.upgrade))
                && rows
                    .iter()
                    .any(|r| r.stall(now).as_deref() == Some("unwatched"))
            {
                let _ = writeln!(
                    err,
                    "aterm harness: `[harness] {}` reads off in {}, so no window watches these \
                     upgrades: `unwatched` here is that switch, not a fault",
                    if policy.enabled { "upgrade" } else { "enabled" },
                    path.display()
                );
            }
            if rows.is_empty() && !json {
                let _ = writeln!(
                    out,
                    "no session waits for an upgrade{}",
                    if sid.is_empty() {
                        String::new()
                    } else {
                        format!(" in tab {sid}")
                    }
                );
            }
            return ExitCode::SUCCESS;
        }
        OwnerAsk::Now => super::upgrade_drive::Ask::Now,
        OwnerAsk::Defer(secs) => super::upgrade_drive::Ask::Defer(secs),
        OwnerAsk::Skip => super::upgrade_drive::Ask::Skip,
    };
    // Its row is read as `--status` reads it: a claim only while the window's
    // host runs.
    match super::upgrade_drive::ask(&opts, sid, word, policy.enabled && policy.upgrade) {
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

/// One pass of `upgrade`: the master switch, then the sweep, every report
/// printed, and the exit code it owes for it.
///
/// With `[harness] enabled` off ([`SupervisorConfig::from_path`], the
/// window's host's reader) the pass answers one [`UPGRADE_BYPASSED`] line,
/// takes no lock, types and signals nothing, and exits 1; stderr names every
/// restart an earlier sweep left in flight ([`stranded`]) and the reader's
/// notes. `[harness] upgrade` does not gate it: naming the verb is consent.
/// A `--dry-run` acts on nothing, so it still sweeps, prints and exits 0.
///
/// A sweep that did not run decided nothing: `busy:another-sweep` exits 75
/// ([`atpkg::lock::CONTENDED_EXIT`], "try again later"), so a script
/// retries by code; `busy:state-unwritable` and `busy:lock-unopenable` exit
/// 1, since a state directory that cannot hold a lock is not transient.
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
/// signalled is not relaunched while the switch is off (by this sweep, or by
/// the window's host, which stands down on the same switch), and the refusal
/// says so rather than leaving a conversation silently stopped.
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
            // A finished upgrade is history: the session caught up, so it is
            // not behind, whatever its old record's clock says (`--status`
            // reads the clock only for a Pending or Announced row, and so does
            // this line).
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
// accounts
// ---------------------------------------------------------------------------

/// What `accounts use` says once the owner's word is recorded. A window's
/// supervisor reads the pin at each hosted session's turn end
/// (`accounts::advise` rule 2) and relaunches it under the pinned account; a
/// session waiting at this account's own limit is continued where it is
/// (`accounts::Advice::Resume`). The line claims no move: this command makes
/// none, and a session with no supervisor (or under `[harness] accounts =
/// "off"`) follows nothing.
pub const ACCOUNTS_USE_RECORDED: &str = "recorded: a window's supervisor moves each Claude Code \
                                         session to it at the session's next turn end, and \
                                         continues one that waits at this account's own limit";

/// The report's last line: WHAT ACTS ON THIS STATE, what does not yet, and
/// what has never been seen. `next:` is the account a supervised session at
/// a usage limit is relaunched under; the pin is followed at any turn end;
/// under `accounts = "early"` (design §3) a session is moved AHEAD of a
/// limit too, off an account that is limited on record or sampled low; the
/// `/usage` probe that takes the sample (design §5) records only a panel
/// read whole and settled — which an ordinary window shows for about a
/// second (`accounts --help` says the measurement, and the one live run
/// of the probe) — so the line claims no percent; a
/// limit reset is built, ships off (`[harness] limit_reset`) and has never
/// been run against a real wall (design §7: every text of it was read in the
/// vendor's bundle); and every word of it rests on stand-ins — no session
/// has been moved between two real accounts (design §9).
pub const ACCOUNTS_ROTATION: &str = "rotation: a supervised session follows the pinned account \
                                     (`accounts use`), rotates to `next` at a usage limit and, \
                                     under `accounts = \"early\"`, ahead of one off an account \
                                     that is limited or sampled low; the usage probe samples \
                                     only a `/usage` panel it reads whole; a limit reset is \
                                     taken only under `limit_reset = true` (off by default) \
                                     and has never been run against a real wall; none of it \
                                     has been observed with two real accounts";

/// The label the FIRST `accounts add` gives this machine's own login (the
/// row with no `dir`), so the login every session already runs on is in the
/// roster: a session whose login no row names is never advised to move
/// ([`accounts::StayWhy::NotListed`]).
pub const ACCOUNTS_OWN_LABEL: &str = "main";

/// How long one `claude auth status --json` may run (`accounts check`). A
/// bound, not a budget: MEASURED 2026-10-01 on 2.1.286, the command answered
/// in under a second, signed in or not — and a `claude` that is not going to
/// answer (one was measured hanging on `--version` too) is stopped here and
/// said as `could not run`.
pub const AUTH_CHECK_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// The most of that command's stdout that is read. The document is a few
/// hundred bytes; one cut at this cap does not parse and is `could not run`.
pub const MAX_AUTH_STDOUT: usize = 64 * 1024;

/// What `accounts check` and `accounts login` need of the machine beyond
/// files: the two `claude` runs, whether stdin is a terminal, and the clock
/// once a login flow has ended. Injected, so a verb's test runs no `claude`;
/// [`LiveAccounts`] is the shipped one.
pub trait AccountsHost {
    /// Whether stdin is a terminal. Only then is the login flow run: it
    /// opens a browser and waits for a person.
    fn stdin_is_terminal(&self) -> bool;

    /// Run `claude auth status --json` with [`accounts::LOGIN_ENV`] set to
    /// `login` — a row's `dir` verbatim, or the empty string for this
    /// machine's own login — bounded, and hand back its exit and its stdout.
    ///
    /// THE STDOUT IS NOT TO BE SHOWN: it names an email and an organization.
    /// Its one reader is [`login_check`], which takes the exit status and the
    /// key `loggedIn` and drops the rest.
    ///
    /// # Errors
    ///
    /// The sentence that says why it did not run (no `claude` on PATH, the
    /// deadline) — never any of the command's output.
    fn auth_status(&self, login: &str) -> Result<Captured, String>;

    /// Why no login can be asked here whether it is signed in, whatever the
    /// login ([`AUTH_CHECK_NO_USER`]); `None` where one can. Asked BEFORE a
    /// login flow is started: a flow whose login could not be asked
    /// afterwards would cost the owner a browser sign-in and record nothing.
    fn unaskable(&self) -> Option<String>;

    /// Run `claude auth login` on this terminal and WAIT for it, with
    /// [`accounts::HOME_ENV`] set to `dir` — or, for `None`, this machine's
    /// own login in the default home, with that variable unset. Answers the
    /// flow's exit code ([`exit_code`]).
    ///
    /// # Errors
    ///
    /// Why `claude` could not be started.
    fn login(&self, dir: Option<&str>) -> Result<i32, String>;

    /// Unix seconds, read when it is asked. A login flow waits for a person,
    /// so what it left is stamped by this and not by [`Env::now`], which was
    /// read before the wait.
    fn now(&self) -> i64;
}

/// The shipped [`AccountsHost`]: `claude` as PATH resolves it.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveAccounts;

impl AccountsHost for LiveAccounts {
    fn stdin_is_terminal(&self) -> bool {
        use std::io::IsTerminal as _;
        io::stdin().is_terminal()
    }

    fn auth_status(&self, login: &str) -> Result<Captured, String> {
        auth_status_of(std::ffi::OsStr::new("claude"), login, &|name| {
            std::env::var_os(name)
        })
    }

    fn unaskable(&self) -> Option<String> {
        auth_check_env(&|name| std::env::var_os(name)).err()
    }

    fn login(&self, dir: Option<&str>) -> Result<i32, String> {
        login_flow_of(std::ffi::OsStr::new("claude"), dir)
    }

    fn now(&self) -> i64 {
        crate::supervise::limit::unix_now()
    }
}

/// `<program> auth login` as [`login_flow_of`] runs it: the workspace's own
/// command, which hands the vendor's flow this process's stdio and nothing
/// else.
///
/// The login is made under the HOME ALONE ([`accounts::HOME_ENV`]): a
/// [`accounts::LOGIN_ENV`] this process inherited would name the keychain
/// item instead (it wins when set), and the login would land under another
/// account's name. So that variable never reaches the flow; and for `None`,
/// this machine's own login, the home's variable does not either — the login
/// Claude Code uses here with neither set.
fn login_command(program: &std::ffi::OsStr, dir: Option<&str>) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::Command::new(program);
    cmd.args(["auth", "login"]).env_remove(accounts::LOGIN_ENV);
    match dir {
        Some(dir) => cmd.env(accounts::HOME_ENV, dir),
        None => cmd.env_remove(accounts::HOME_ENV),
    };
    cmd
}

/// ONE RUN OF `<program> auth login` ([`login_command`]) on this terminal,
/// WAITED FOR. It is a child and not a replacement of this process, so what
/// the login then answers can be asked and recorded; its stdio is this
/// process's, so the flow reads the keyboard. A Ctrl-C the terminal delivers
/// as a signal reaches the whole foreground group, and this process STANDS
/// ASIDE from it while it waits ([`ignoring_interrupts`]): the flow decides
/// what the key means, and an end it caused (130) is answered like any other
/// exit, so the login is still asked afterwards.
///
/// # Errors
///
/// Why the program could not be started.
fn login_flow_of(program: &std::ffi::OsStr, dir: Option<&str>) -> Result<i32, String> {
    let mut child = login_command(program, dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    ignoring_interrupts(|| child.wait())
        .map(exit_code)
        .map_err(|e| e.to_string())
}

/// Run `wait` with this process IGNORING SIGINT AND SIGQUIT — the rule
/// `system(3)` keeps, and `atpkg::platform::wait_standing_aside`'s for a
/// `std` child. Called AFTER the spawn: an ignored signal is inherited
/// across `exec`, and the child must meet the key with its own disposition.
/// Each disposition is put back whole before this returns. The dispositions
/// are the whole process's: its one caller is `aterm harness accounts
/// login`'s wait ([`login_flow_of`]).
#[cfg(unix)]
fn ignoring_interrupts<T>(wait: impl FnOnce() -> T) -> T {
    // SAFETY: `sigaction` is plain data for which all-zero is a valid value
    // (an empty mask, no flags).
    let mut ignore: libc::sigaction = unsafe { std::mem::zeroed() };
    ignore.sa_sigaction = libc::SIG_IGN;
    let replaced = [libc::SIGINT, libc::SIGQUIT].map(|sig| {
        // SAFETY: as above.
        let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a valid disposition for `sig`; `was` receives the one it
        // replaces.
        let set = unsafe { libc::sigaction(sig, &ignore, &mut was) } == 0;
        (sig, set.then_some(was))
    });
    let value = wait();
    for (sig, was) in replaced {
        if let Some(was) = was {
            // SAFETY: puts back the disposition this function replaced.
            unsafe { libc::sigaction(sig, &was, std::ptr::null_mut()) };
        }
    }
    value
}

/// Elsewhere the wait is plain: the console's Ctrl-C reaches this process as
/// it reaches the flow.
#[cfg(not(unix))]
fn ignoring_interrupts<T>(wait: impl FnOnce() -> T) -> T {
    wait()
}

/// A child's end as one number: its exit code, or 128 plus the signal that
/// ended it (a shell's `$?`), so a flow a signal ended is never read as one
/// that exited 0.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    status
        .code()
        .or_else(|| signal.map(|signal| 128_i32.saturating_add(signal)))
        .unwrap_or(1)
}

/// ONE RUN OF `<program> auth status --json` UNDER `login`
/// ([`accounts::LOGIN_ENV`] set to it: a row's `dir` verbatim, or the empty
/// string for this machine's own login), bounded by [`AUTH_CHECK_BUDGET`],
/// its stdout capped at [`MAX_AUTH_STDOUT`]. The one runner of that command:
/// `accounts check` runs the `claude` PATH resolves ([`LiveAccounts`]), and a
/// window's supervisor runs THE SESSION'S OWN PROGRAM, by its path, before it
/// moves a session to an account nobody has seen serve (aterm-gui's
/// `harness_host`, the accounts design §6). `var` reads the caller's
/// environment.
///
/// THE STDOUT IS NOT TO BE SHOWN OR LOGGED: it names an email and an
/// organization. Its one reader is [`login_check`], which takes the exit
/// status and the key `loggedIn` and drops the rest.
///
/// The bounded runner clears the environment and rebuilds it from PATH, HOME,
/// TMPDIR and what is set on the command. Two more groups go through, each
/// MEASURED on 2026-10-01 (Claude Code 2.1.286, this machine's one login, the
/// environment below and nothing else):
///
/// * USER and LOGNAME. With them the default login answers `loggedIn: true`,
///   exit 0; WITHOUT them the same login answers `loggedIn: false`, exit 1 —
///   a signed-in account recorded as signed out. So with neither set the
///   command is NOT RUN ([`auth_check_env`]).
/// * The markers the agents' reroute stub decides by
///   (`atpkg::hooks::AGENTS_MARKERS`): `claude` on PATH is that stub inside
///   aterm, and with no marker it runs the user's own copy rather than the
///   managed one the sessions run. The check asks the `claude` this shell
///   would run. (A program named by its path meets no stub; the markers ride
///   along unread.)
///
/// Nothing else does. `CLAUDE_CONFIG_DIR` in particular is dropped: the login
/// is named by the variable set here whatever the home is (READ in the
/// bundle, design §1; MEASURED: naming an empty directory answers `loggedIn:
/// false`, exit 1, and writes nothing into it).
///
/// # Errors
///
/// The sentence that says why it did not run ([`AUTH_CHECK_NO_USER`], no such
/// program, the deadline) — never any of the command's output.
pub fn auth_status_of(
    program: &std::ffi::OsStr,
    login: &str,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Captured, String> {
    auth_status_within(program, login, var, AUTH_CHECK_BUDGET)
}

/// [`auth_status_of`] bounded by `budget`: the product's is
/// [`AUTH_CHECK_BUDGET`]; a test's is a hang detector, since its stand-in's
/// first exec can wait on the system's code-policy check for longer.
fn auth_status_within(
    program: &std::ffi::OsStr,
    login: &str,
    var: &dyn Fn(&str) -> Option<OsString>,
    budget: std::time::Duration,
) -> Result<Captured, String> {
    let pass = auth_check_env(var)?;
    let mut cmd = aterm_uds::spawnfd::Command::new(program);
    cmd.args(["auth", "status", "--json"])
        .env(accounts::LOGIN_ENV, login);
    cmd.envs(pass);
    super::align::capture_bounded(cmd, budget, None, MAX_AUTH_STDOUT)
}

/// Why `claude auth status --json` is not run with neither USER nor LOGNAME
/// in the environment: [`LoginCheck::CouldNotRun`]'s reason, word for word.
pub const AUTH_CHECK_NO_USER: &str = "USER is not set";

/// The variables `accounts check` hands through to the `claude` it runs, as
/// `var` reads this process's environment: USER and LOGNAME, and the markers
/// the agents' reroute stub decides by — each as it is, unread.
///
/// # Errors
///
/// [`AUTH_CHECK_NO_USER`] when neither USER nor LOGNAME is set to anything:
/// MEASURED (2.1.286), a login that works then answers `loggedIn: false`,
/// exit 1, and the check would record a signed-in account as signed out (and
/// end a pin on it). An answer that cannot be trusted is not asked for.
fn auth_check_env(
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Vec<(&'static str, OsString)>, String> {
    let named = ["USER", "LOGNAME"]
        .into_iter()
        .any(|name| var(name).is_some_and(|value| !value.is_empty()));
    if !named {
        return Err(AUTH_CHECK_NO_USER.to_string());
    }
    Ok(["USER", "LOGNAME"]
        .into_iter()
        .chain(atpkg::hooks::AGENTS_MARKERS.iter().copied())
        .filter_map(|name| var(name).map(|value| (name, value)))
        .collect())
}

/// What one login answered to `accounts check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginCheck {
    /// `loggedIn: true`, exit 0.
    SignedIn,
    /// `loggedIn: false` (MEASURED on 2.1.286: with exit 1, design §1).
    SignedOut,
    /// No login was made in the row's login directory on this machine — it
    /// is absent, or holds nothing ([`accounts::login_made`]): nothing was
    /// run.
    NotEnrolled,
    /// The command did not run, or gave no answer that can be read: nothing
    /// is recorded. The reason is this command's own words, never the
    /// child's output.
    CouldNotRun(String),
}

impl LoginCheck {
    /// The line's words after `<label>: `.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            LoginCheck::SignedIn => "signed in".to_string(),
            LoginCheck::SignedOut => "signed out".to_string(),
            LoginCheck::NotEnrolled => "not enrolled".to_string(),
            LoginCheck::CouldNotRun(why) => format!("could not run ({why})"),
        }
    }

    /// What is recorded of it ([`accounts::note_auth`]); `None` records
    /// nothing.
    #[must_use]
    pub fn auth(&self) -> Option<Auth> {
        match self {
            LoginCheck::SignedIn => Some(Auth::Ok),
            LoginCheck::SignedOut => Some(Auth::SignedOut),
            LoginCheck::NotEnrolled | LoginCheck::CouldNotRun(_) => None,
        }
    }
}

/// THE ONE KEY of `claude auth status --json` that is read: `loggedIn`, a
/// boolean at the top of one JSON object. The document is parsed, that key
/// is taken and everything else — the email and the organization it names —
/// is dropped here, unread. `None` for anything else: not UTF-8, not JSON,
/// not an object, no such key, a key that is not a boolean.
fn logged_in(stdout: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(stdout).ok()?;
    let doc: Value = aterm_json::from_str(text).ok()?;
    doc.as_object()?.get("loggedIn")?.as_bool()
}

/// One run of `claude auth status --json` as an answer: the exit status and
/// [`logged_in`], and nothing else of it.
///
/// `true` counts only with exit 0. `false` counts with any exit (MEASURED:
/// exit 1). `true` beside a failing exit, and an exit with no answer, are
/// `could not run`: nothing is recorded on evidence that disagrees with
/// itself. No reason quotes the output or a parser's message about it.
#[must_use]
pub fn login_check(ran: Result<Captured, String>) -> LoginCheck {
    let got = match ran {
        Ok(got) => got,
        Err(why) => return LoginCheck::CouldNotRun(super::one_line(&why, 160)),
    };
    let exit = got
        .code
        .map_or_else(|| "ended by a signal".to_string(), |c| format!("exit {c}"));
    match (logged_in(&got.stdout), got.code) {
        (Some(true), Some(0)) => LoginCheck::SignedIn,
        (Some(false), _) => LoginCheck::SignedOut,
        (Some(true), _) => LoginCheck::CouldNotRun(format!("it said loggedIn, and {exit}")),
        (None, _) => LoginCheck::CouldNotRun(format!("{exit}, with no `loggedIn` answer")),
    }
}

/// The roster `accounts` works on — its path and its rows — or `None` with
/// the reason already on `err` (the caller's exit 1): no config directory is
/// known, or the file does not admit. A missing file is the empty roster.
fn accounts_read_roster(env: &Env, err: &mut dyn Write) -> Option<(PathBuf, Roster)> {
    let Some(path) = env.accounts_roster() else {
        let _ = writeln!(
            err,
            "aterm harness accounts: no config directory is known (neither $XDG_CONFIG_HOME nor \
             $HOME is set), so there is no {} to read; name one with `--config \
             <dir>/aterm.toml`",
            accounts::ACCOUNTS_FILE
        );
        return None;
    };
    match Roster::load(&path) {
        Ok(roster) => Some((path, roster)),
        Err(why) => {
            let _ = writeln!(err, "{}", accounts_refusal(&path, &why));
            None
        }
    }
}

/// The sentence for a roster that does not admit.
fn accounts_refusal(path: &Path, why: &str) -> String {
    format!(
        "aterm harness accounts: the roster at {} is REFUSED: {why}. It is admitted whole or \
         not at all, so no account is listed until it is fixed.",
        path.display()
    )
}

/// What `accounts` prints for a roster with no rows: that, the file it read,
/// and the one line that lists an account.
fn accounts_none_listed(path: &Path) -> String {
    format!(
        "no accounts listed\nroster: {}{}\nlist one: aterm harness accounts add <label>\n",
        path.display(),
        if path.exists() {
            " (no rows)"
        } else {
            " (no such file)"
        }
    )
}

/// ONE TRANSACTION on the accounts state ([`accounts::Store::transact`]: the
/// lock, the load, `edit`, the save of a state that changed, the ledger
/// rows, the wake marker), with the store's note — a torn state file read as
/// empty — said on `err`, whether or not the transaction then succeeds.
///
/// # Errors
///
/// As [`accounts::Store::transact`]: a state file that is not ours to
/// replace, by name, with nothing written; `edit`'s own; or the file that
/// could not be locked or written, by path.
fn accounts_transact(
    store: &accounts::Store,
    now: i64,
    err: &mut dyn Write,
    edit: impl FnOnce(&mut accounts::State) -> Result<Vec<accounts::Event>, String>,
) -> Result<Vec<accounts::Event>, String> {
    let mut note = None;
    let done = store.transact(now, &mut note, edit);
    if let Some(note) = note {
        let _ = writeln!(err, "aterm harness accounts: {note}");
    }
    done
}

/// Where a session at a limit on the account in use would go, and why each
/// other row is not that place: the report's `next:`.
struct AccountsNext {
    /// The first candidate, when there is one.
    to: Option<String>,
    /// With no candidate: the instant the roster next has a usable account,
    /// when that is ahead.
    wait_until: Option<i64>,
    /// Every row that is not a candidate, with its reason, in roster order.
    not: Vec<(String, accounts::WhyNot)>,
}

/// [`AccountsNext`] at `now`. The account in use is `active`, else the row
/// with no `dir` (the login a session starts on before anything rotates);
/// the point is a wall, where a `low` account is still a candidate.
fn accounts_next(
    roster: &Roster,
    state: &accounts::State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> AccountsNext {
    let me = state
        .active
        .as_deref()
        .filter(|label| roster.row(label).is_some())
        .or_else(|| roster.row_for_login(None).map(|row| row.label.as_str()));
    let point = accounts::Point::Wall {
        window: String::new(),
        at: None,
    };
    let mut to = None;
    let mut not = Vec::new();
    for (label, verdict) in accounts::candidates(roster, state, enrolled, me, &point, now) {
        match verdict {
            Ok(_) => {
                to.get_or_insert(label);
            }
            Err(why) => not.push((label, why)),
        }
    }
    let wait_until = match to {
        Some(_) => None,
        None => accounts::next_usable(roster, state, enrolled, now).filter(|at| *at > now),
    };
    AccountsNext {
        to,
        wait_until,
        not,
    }
}

/// A non-candidate's reason as the report words it, its instant placed.
fn why_not_words(why: accounts::WhyNot) -> String {
    match why {
        accounts::WhyNot::Me => "the account in use".to_string(),
        accounts::WhyNot::Limited { until } => format!("limited until {}", rfc3339_utc(until)),
        accounts::WhyNot::LowWhileOneIsNear { at } => {
            format!("{} ({})", why.as_str(), rfc3339_utc(at))
        }
        other => other.as_str().to_string(),
    }
}

/// `active`, `active:unproven`, `pinned`, or `None` for an account that is
/// not the active one.
fn account_mark(state: &accounts::State, label: &str) -> Option<&'static str> {
    if state.active.as_deref() != Some(label) {
        None
    } else if state.pinned {
        Some("pinned")
    } else if state.proven {
        Some("active")
    } else {
        Some("active:unproven")
    }
}

/// The `accounts` report as text (design §8): the header, one row per
/// account, `next:` with every non-candidate's reason, and
/// [`ACCOUNTS_ROTATION`]. No usage figure is shown (the JSON keeps them). A
/// limit-reset offer on record shows as `offer: N left[, use by <time>]`.
fn accounts_text(
    path: &Path,
    roster: &Roster,
    state: &accounts::State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> String {
    if roster.rows.is_empty() {
        return accounts_none_listed(path);
    }
    let mut text = match state.active.as_deref() {
        Some(active) => format!(
            "active: {active}, {} of {} sessions on it\n",
            state.sessions_on(active).len(),
            state.sessions.len()
        ),
        None => format!(
            "active: none chosen yet (a session runs on the login it started with), {} sessions \
             seen\n",
            state.sessions.len()
        ),
    };
    let width = roster.rows.iter().map(|r| r.label.len()).max().unwrap_or(0);
    for row in &roster.rows {
        let here = row.dir.is_none() || enrolled.contains(&row.label);
        let condition = match accounts::condition(row, state, here, now) {
            accounts::Condition::Limited { until } => {
                format!("limited until {}", rfc3339_utc(until))
            }
            other => other.as_str().to_string(),
        };
        let on: Vec<String> = state
            .sessions
            .iter()
            .filter(|(_, s)| s.account == row.label)
            .map(|(sid, s)| {
                let sid = format!("@{}", sid.trim_start_matches('@'));
                match &s.held {
                    Some(held) => format!("{sid} (held: {held})"),
                    None => sid,
                }
            })
            .collect();
        let offer = state
            .accounts
            .get(&row.label)
            .and_then(|account| account.offer.as_ref())
            .map_or_else(String::new, |offer| match offer.use_by {
                Some(by) => format!("offer: {} left, use by {}  ", offer.left, rfc3339_utc(by)),
                None => format!("offer: {} left  ", offer.left),
            });
        text.push_str(&format!(
            "{:width$}  {:9}  {condition}  {}  {offer}on: {}\n",
            row.label,
            row.class.as_str(),
            account_mark(state, &row.label).unwrap_or("-"),
            if on.is_empty() {
                "-".to_string()
            } else {
                on.join(" ")
            },
        ));
    }
    let next = accounts_next(roster, state, enrolled, now);
    text.push_str(&match (&next.to, next.wait_until) {
        (Some(to), _) => format!("next: {to}\n"),
        (None, Some(until)) => format!(
            "next: wait until {} (in {})\n",
            rfc3339_utc(until),
            super::upgrade::span(u64::try_from(until.saturating_sub(now)).unwrap_or(0))
        ),
        (None, None) => "next: none (no other account can be run on)\n".to_string(),
    });
    if !next.not.is_empty() {
        let reasons: Vec<String> = next
            .not
            .iter()
            .map(|(label, why)| match why {
                // The one reason a command of this verb's ends: named with it.
                // (Only a row that names a directory is ever not enrolled.)
                accounts::WhyNot::NotEnrolled => format!(
                    "{label}: {} (no login made yet: run `aterm harness accounts login {label}`)",
                    why.as_str()
                ),
                _ => format!("{label}: {}", why_not_words(*why)),
            })
            .collect();
        text.push_str(&format!("not next: {}\n", reasons.join("; ")));
    }
    if accounts::rotations_spent(state, roster, now) {
        text.push_str("the rotation bound for this hour is spent: a limit is waited out\n");
    }
    text.push_str(ACCOUNTS_ROTATION);
    text.push('\n');
    text
}

/// `key` as Unix seconds and `key_rfc3339` as the same instant in UTC, both
/// `null` for an instant nobody read.
fn put_time(o: &mut Map, key: &str, t: Option<i64>) {
    o.insert(key.to_owned(), Value::from(t));
    o.insert(format!("{key}_rfc3339"), Value::from(t.map(rfc3339_utc)));
}

/// The `accounts --json` document's head, shared by the report and by a
/// roster that is refused: `{schema: 1, kind: "accounts", as_of, roster}`.
fn accounts_json_head(path: &Path, now: i64) -> Map {
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("kind".to_owned(), Value::from("accounts".to_owned()));
    put_time(&mut o, "as_of", Some(now));
    o.insert("roster".to_owned(), Value::from(path.display().to_string()));
    o
}

/// The `accounts --json` document for a roster that does not admit:
/// `refused` is the reason, and no account is listed.
fn accounts_refused_json(path: &Path, why: &str, now: i64) -> String {
    let mut o = accounts_json_head(path, now);
    o.insert("refused".to_owned(), Value::from(why.to_owned()));
    o.insert("accounts".to_owned(), Value::Array(Vec::new()));
    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
}

/// The `accounts --json` document, schema 1, one line, keys sorted:
///
/// `{schema, kind: "accounts", as_of, roster, refused: null, rotation:
/// {follows_use, at_limit, ahead_of_limit, probe, limit_reset, observed},
/// active, proven, pinned,
/// sessions, next: {to, wait_until,
/// not: [{label, why}]}, accounts: [{label, class, dir?, enrolled, condition,
/// limited_until, auth, auth_at, active, pinned, session_pct, session_reset,
/// week_pct, week_reset, sampled_at, offer, walls, sessions: [{sid, at,
/// held}]}]}`.
///
/// An unknown is `null`, never `0`; every instant is Unix seconds with its
/// UTC form beside it as `<key>_rfc3339`; `dir` is absent for the row that
/// names none (this machine's own login). `rotation` is
/// [`ACCOUNTS_ROTATION`] as six facts about the BUILD: this command does not
/// read which mode a window runs under. `session_pct`/`week_pct` are `null`
/// for an account no settled `/usage` panel was read for, and `offer` for one
/// no reset confirmation was read for.
fn accounts_json(
    path: &Path,
    roster: &Roster,
    state: &accounts::State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> String {
    let mut o = accounts_json_head(path, now);
    o.insert("refused".to_owned(), Value::Null);
    let mut rotation = Map::new();
    rotation.insert("follows_use".to_owned(), Value::from(true));
    rotation.insert("at_limit".to_owned(), Value::from(true));
    rotation.insert("ahead_of_limit".to_owned(), Value::from(true));
    rotation.insert("probe".to_owned(), Value::from(true));
    rotation.insert("limit_reset".to_owned(), Value::from(true));
    rotation.insert("observed".to_owned(), Value::from(false));
    o.insert("rotation".to_owned(), Value::Object(rotation));
    o.insert("active".to_owned(), Value::from(state.active.clone()));
    o.insert("proven".to_owned(), Value::from(state.proven));
    o.insert("pinned".to_owned(), Value::from(state.pinned));
    o.insert(
        "sessions".to_owned(),
        Value::from(state.sessions.len() as u64),
    );
    let next = accounts_next(roster, state, enrolled, now);
    let mut n = Map::new();
    n.insert("to".to_owned(), Value::from(next.to.clone()));
    put_time(&mut n, "wait_until", next.wait_until);
    n.insert(
        "not".to_owned(),
        Value::Array(
            next.not
                .iter()
                .map(|(label, why)| {
                    let mut m = Map::new();
                    m.insert("label".to_owned(), Value::from(label.clone()));
                    m.insert("why".to_owned(), Value::from(why_not_words(*why)));
                    Value::Object(m)
                })
                .collect(),
        ),
    );
    o.insert("next".to_owned(), Value::Object(n));
    let rows = roster
        .rows
        .iter()
        .map(|row| {
            let account = state.account(&row.label);
            let here = row.dir.is_none() || enrolled.contains(&row.label);
            let condition = accounts::condition(row, state, here, now);
            let mut m = Map::new();
            m.insert("label".to_owned(), Value::from(row.label.clone()));
            m.insert("class".to_owned(), Value::from(row.class.as_str()));
            if let Some(dir) = &row.dir {
                m.insert("dir".to_owned(), Value::from(dir.clone()));
            }
            m.insert("enrolled".to_owned(), Value::from(here));
            m.insert("condition".to_owned(), Value::from(condition.as_str()));
            put_time(
                &mut m,
                "limited_until",
                match condition {
                    accounts::Condition::Limited { until } => Some(until),
                    _ => None,
                },
            );
            m.insert("auth".to_owned(), Value::from(account.auth.as_str()));
            put_time(&mut m, "auth_at", account.auth_at);
            let mark = account_mark(state, &row.label);
            m.insert("active".to_owned(), Value::from(mark.is_some()));
            m.insert("pinned".to_owned(), Value::from(mark == Some("pinned")));
            let sample = account.sample.as_ref();
            let session = sample.and_then(|s| s.session(now));
            let week = sample.and_then(|s| s.week(now));
            m.insert(
                "session_pct".to_owned(),
                Value::from(session.map(|(pct, _)| pct)),
            );
            put_time(
                &mut m,
                "session_reset",
                session.and(sample.and_then(|s| s.session_reset)),
            );
            m.insert("week_pct".to_owned(), Value::from(week.map(|(pct, _)| pct)));
            put_time(
                &mut m,
                "week_reset",
                week.and(sample.and_then(|s| s.week_reset)),
            );
            put_time(&mut m, "sampled_at", sample.map(|s| s.at));
            m.insert(
                "offer".to_owned(),
                account.offer.as_ref().map_or(Value::Null, |offer| {
                    let mut f = Map::new();
                    f.insert("left".to_owned(), Value::from(offer.left));
                    put_time(&mut f, "use_by", offer.use_by);
                    put_time(&mut f, "at", Some(offer.at));
                    Value::Object(f)
                }),
            );
            m.insert(
                "walls".to_owned(),
                Value::Object(
                    account
                        .walls
                        .iter()
                        .map(|(window, wall)| {
                            let mut w = Map::new();
                            put_time(&mut w, "until", wall.until);
                            put_time(&mut w, "at", Some(wall.at));
                            put_time(&mut w, "stands_until", Some(wall.stands_until()));
                            w.insert("attempts".to_owned(), Value::from(wall.attempts));
                            (window.clone(), Value::Object(w))
                        })
                        .collect(),
                ),
            );
            m.insert(
                "sessions".to_owned(),
                Value::Array(
                    state
                        .sessions
                        .iter()
                        .filter(|(_, s)| s.account == row.label)
                        .map(|(sid, s)| {
                            let mut e = Map::new();
                            e.insert("sid".to_owned(), Value::from(sid.clone()));
                            put_time(&mut e, "at", Some(s.at));
                            e.insert("held".to_owned(), Value::from(s.held.clone()));
                            Value::Object(e)
                        })
                        .collect(),
                ),
            );
            Value::Object(m)
        })
        .collect();
    o.insert("accounts".to_owned(), Value::Array(rows));
    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
}

/// `aterm harness accounts [--json]`: the report. Reads the roster, the state
/// file and each login directory ([`accounts::enrolled`]); writes nothing and
/// takes no lock (the state file is replaced by a rename, so a read sees one
/// whole file). A roster that does not admit is exit 1 — under `--json`, a
/// document whose `refused` says why.
fn run_accounts_report(
    env: &Env,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let Some(path) = env.accounts_roster() else {
        // The one sentence, from the one place that words it.
        let _ = accounts_read_roster(env, err);
        return ExitCode::from(1);
    };
    let roster = match Roster::load(&path) {
        Ok(roster) => roster,
        Err(why) => {
            let _ = writeln!(err, "{}", accounts_refusal(&path, &why));
            if json {
                let _ = writeln!(out, "{}", accounts_refused_json(&path, &why, env.now));
            }
            return ExitCode::from(1);
        }
    };
    // A state file that is not this build's is left as it is and SHOWN as
    // an empty state, beside the reason: the report writes nothing.
    let store = env.accounts_store();
    let (state, note) = store.load(env.now).unwrap_or_else(|why| {
        (
            accounts::State::default(),
            Some(format!(
                "{why}; shown as an empty state ({} is left as it is)",
                store.state_file().display()
            )),
        )
    });
    if let Some(note) = note {
        let _ = writeln!(err, "aterm harness accounts: {note}");
    }
    let enrolled = accounts::enrolled(&roster);
    let _ = if json {
        writeln!(
            out,
            "{}",
            accounts_json(&path, &roster, &state, &enrolled, env.now)
        )
    } else {
        write!(
            out,
            "{}",
            accounts_text(&path, &roster, &state, &enrolled, env.now)
        )
    };
    ExitCode::SUCCESS
}

/// `aterm harness accounts add <label> [--shared]` (design §4): make the
/// login directory (`<state>/accounts/logins/<label>`, 0700), append the row
/// to `accounts.toml` (creating it), and print the line to run next.
///
/// THE FIRST `add` — a roster with no rows — also lists this machine's own
/// login, as [`ACCOUNTS_OWN_LABEL`], dedicated, with no `dir`, before the new
/// row, and says so. Without that row the login every session already runs
/// on would be outside the roster, and a session whose login no row names is
/// never advised to move.
///
/// The roster is read, appended to and replaced (temp file, then rename)
/// under the store's lock, so two `add`s do not lose a row; the text written
/// is one [`accounts::append_rows`] already re-read, so the next read cannot
/// refuse it. A symlinked roster is written THROUGH the link. Every refusal
/// is exit 1 with the roster untouched: a bad or already-listed label, a
/// roster that does not admit or defines `account` inline (the refusal
/// carries the rows to add by hand), a state directory that is not absolute.
fn run_accounts_add(
    env: &Env,
    label: &str,
    class: Class,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    match accounts_add(env, label, class) {
        Ok(added) => {
            if added.listed_own {
                let _ = writeln!(
                    out,
                    "listed `{ACCOUNTS_OWN_LABEL}` (dedicated): this machine's own login, the \
                     first row of a new roster"
                );
            }
            let _ = writeln!(
                out,
                "listed `{label}` ({}) in {}",
                class.as_str(),
                added.roster.display()
            );
            let _ = writeln!(out, "login directory: {}", added.dir.display());
            if added.dir_held_files {
                let _ = writeln!(
                    err,
                    "aterm harness accounts add: that directory already held files and is kept \
                     as it is: a login made under this name before is this account's again"
                );
            }
            let _ = writeln!(out, "run: aterm harness accounts login {label}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts add: {why}");
            ExitCode::from(1)
        }
    }
}

/// What one `accounts add` did.
struct Added {
    /// The roster file written.
    roster: PathBuf,
    /// The login directory made.
    dir: PathBuf,
    /// This machine's own login was listed too (the first `add`).
    listed_own: bool,
    /// The login directory was already there, with something in it.
    dir_held_files: bool,
}

/// The permission bits of the file at `path` (unix), where it can be read.
fn file_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// [`run_accounts_add`]'s work, or the refusal.
fn accounts_add(env: &Env, label: &str, class: Class) -> Result<Added, String> {
    if let Some(why) = accounts::label_refusal(label) {
        return Err(format!(
            "`{}` is not a label: it {why}",
            super::one_line(label, 80)
        ));
    }
    let path = env.accounts_roster().ok_or_else(|| {
        format!(
            "no config directory is known, so there is no {} to write; name one with `--config \
             <dir>/aterm.toml`",
            accounts::ACCOUNTS_FILE
        )
    })?;
    let store = env.accounts_store();
    let dir = store
        .login_dir(label)
        .ok_or_else(|| format!("`{label}` names no login directory"))?;
    let dir_text = dir.to_str().ok_or_else(|| {
        format!(
            "the login directory {} is not UTF-8 and cannot be a roster row",
            dir.display()
        )
    })?;
    // Said before anything is made: the roster would refuse the row anyway
    // (a login is named by this exact string, which must not depend on
    // where a relaunch starts), and a refusal makes no directory.
    if !dir.is_absolute() {
        return Err(format!(
            "the login directory {} is relative: the harness state directory must be absolute \
             (`--state`, $ATERM_HARNESS_STATE)",
            dir.display()
        ));
    }
    let _held = store
        .acquire()
        .map_err(|e| format!("{}: {e}", store.dir.display()))?;
    let existing = accounts::roster_text(&path)?.unwrap_or_default();
    let roster = Roster::parse(&existing).map_err(|why| {
        format!(
            "the roster at {} is REFUSED: {why}; nothing was written",
            path.display()
        )
    })?;
    if roster.row(label).is_some() {
        return Err(format!(
            "`{label}` is already listed in {}; `aterm harness accounts login {label}` signs it \
             in",
            path.display()
        ));
    }
    // A roster with no rows gets this machine's own login first.
    let listed_own = roster.rows.is_empty();
    if listed_own && label == ACCOUNTS_OWN_LABEL {
        return Err(format!(
            "`{ACCOUNTS_OWN_LABEL}` is the label the first `add` gives this machine's own login \
             (the row with no dir); pick another label for the new account"
        ));
    }
    let own = (ACCOUNTS_OWN_LABEL, Class::Dedicated, None);
    let asked = (label, class, Some(dir_text));
    let rows = if listed_own {
        vec![own, asked]
    } else {
        vec![asked]
    };
    let text = accounts::append_rows(&existing, &rows).map_err(|why| {
        // The refusal that prints the rows to add by hand prints this
        // machine's own with them: say whose it is, as a first `add` that
        // goes through does.
        let own = accounts::row_inline(ACCOUNTS_OWN_LABEL, Class::Dedicated, None);
        if listed_own && own.is_some_and(|own| why.contains(&own)) {
            format!(
                "a roster with no rows gets this machine's own login too (`{ACCOUNTS_OWN_LABEL}`, \
                 the row with no dir): {why}"
            )
        } else {
            why
        }
    })?;
    // A roster that is a SYMLINK (a dotfiles checkout) is written through
    // the link: the replacement goes beside the file it points at, so the
    // link survives and that file gets the row. Renamed over the link itself
    // the roster would become a regular file the checkout knows nothing of.
    // The file it points at is its owner's to keep as they made it, so the
    // replacement carries that file's mode; a roster this command makes, or
    // one in the config directory itself, is 0600.
    let through_link = path.symlink_metadata().is_ok_and(|m| m.is_symlink());
    let write_to = if through_link {
        std::fs::canonicalize(&path).map_err(|e| {
            format!(
                "the roster at {} is a symlink whose target cannot be resolved ({e}); nothing \
                 was written",
                path.display()
            )
        })?
    } else {
        path.clone()
    };
    let keep_mode = through_link.then(|| file_mode(&write_to)).flatten();
    let dir_held_files = accounts::login_made(&dir);
    aterm_types::fs_restricted::ensure_private_dir(&store.logins_dir())
        .and_then(|()| aterm_types::fs_restricted::ensure_private_dir(&dir))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    super::upgrade_models::write_atomic_mode(&write_to, &text, keep_mode).map_err(|e| {
        format!(
            "{}: {e}; the roster is unchanged (the login directory {} was made)",
            write_to.display(),
            dir.display()
        )
    })?;
    Ok(Added {
        roster: path,
        dir,
        listed_own,
        dir_held_files,
    })
}

/// What the login of `row` answers now ([`AccountsHost::auth_status`],
/// [`login_check`]): asked under the row's `dir` verbatim, or the empty
/// string for the row with none. A row whose login directory holds no login
/// (`enrolled` does not name it) is not asked at all.
fn ask_login(
    host: &dyn AccountsHost,
    row: &accounts::Row,
    enrolled: &BTreeSet<String>,
) -> LoginCheck {
    match row.dir.as_deref() {
        Some(_) if !enrolled.contains(&row.label) => LoginCheck::NotEnrolled,
        dir => login_check(host.auth_status(dir.unwrap_or(""))),
    }
}

/// Record what logins answered at `now`, in one transaction
/// ([`accounts_transact`]): [`accounts::note_auth`] for each, a ledger `auth`
/// row for each word that changed, and the wake marker replaced when one of
/// them now works.
fn record_answers(
    env: &Env,
    now: i64,
    seen: &[(&str, Auth)],
    err: &mut dyn Write,
) -> Result<Vec<accounts::Event>, String> {
    accounts_transact(&env.accounts_store(), now, err, |state| {
        Ok(seen
            .iter()
            .flat_map(|(label, auth)| accounts::note_auth(state, label, *auth, now))
            .collect())
    })
}

/// `aterm harness accounts login <label>` (design §4, §8): print the line
/// that signs the account in — `CLAUDE_CONFIG_DIR='<dir>' claude auth login`,
/// the vendor's own browser flow in a home of its own, the `dir` VERBATIM
/// from the roster because the login is named by a hash of that string; for
/// the row with no `dir`, this machine's own login, `claude auth login` — and,
/// with stdin a terminal, run it and wait for it ([`AccountsHost::login`]).
///
/// A FLOW THAT EXITS 0 IS NOT YET A LOGIN THAT WORKS: that one login is then
/// asked ([`ask_login`], as `accounts check` asks it), its line printed and
/// its answer recorded ([`record_answers`]). Exit 0 only for `signed in`
/// after a flow that exited 0. A `claude` that cannot be started is said with
/// nothing asked and nothing recorded, exit 1.
///
/// A FLOW THAT FAILS CAN STILL LEAVE FILES IN THE DIRECTORY, and files are
/// what enrol a row ([`accounts::login_made`]). MEASURED on 2.1.287: `claude
/// auth status --json` in an empty home ([`accounts::HOME_ENV`]) exited 1
/// and left `.claude.json` (start-up keys only), its lock and `backups/`;
/// `auth login` starts the same way. So a flow that exits non-zero (denied,
/// timed out, Ctrl-C) is said, exit 1, and when its row's directory now holds
/// an entry that login is asked and recorded anyway, so the row reads
/// `signed-out` rather than `unseen`. A failed flow on the row with no `dir`
/// asks nothing: that row is enrolled whatever a flow did. An answer that
/// records nothing is said on stderr with what to run.
///
/// With stdin not a terminal the line is printed, nothing is run and the
/// exit is 0. Where no login can be asked at all ([`AccountsHost::unaskable`])
/// the flow is not started, exit 1. A label the roster does not list is
/// exit 1.
fn run_accounts_login(
    env: &Env,
    host: &dyn AccountsHost,
    label: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let Some((path, roster)) = accounts_read_roster(env, err) else {
        return ExitCode::from(1);
    };
    let Some(row) = roster.row(label) else {
        let _ = writeln!(
            err,
            "aterm harness accounts login: `{}` is not in the roster ({}); `aterm harness \
             accounts add <label>` lists an account",
            super::one_line(label, 80),
            path.display()
        );
        return ExitCode::from(1);
    };
    let dir = row.dir.as_deref();
    let _ = match dir {
        Some(dir) => writeln!(
            out,
            "CLAUDE_CONFIG_DIR={} claude auth login",
            super::upgrade::quote(super::upgrade::Dialect::Zsh, dir)
        ),
        None => writeln!(out, "claude auth login"),
    };
    if !host.stdin_is_terminal() {
        let _ = writeln!(
            err,
            "aterm harness accounts login: not run: stdin is not a terminal (the flow opens a \
             browser and waits). Run `aterm harness accounts login {label}` in a terminal"
        );
        return ExitCode::SUCCESS;
    }
    if let Some(why) = host.unaskable() {
        let _ = writeln!(
            err,
            "aterm harness accounts login: not run: {why}, so `{label}` could not be asked \
             afterwards whether it is signed in"
        );
        return ExitCode::from(1);
    }
    // A row written by hand, or one this machine has not enrolled yet: the
    // flow gets a private directory to write its home into.
    if let Some(dir) = dir
        && !Path::new(dir).is_dir()
        && let Err(e) = aterm_types::fs_restricted::create_dir_restricted(Path::new(dir))
    {
        let _ = writeln!(err, "aterm harness accounts login: {dir}: {e}");
        return ExitCode::from(1);
    }
    let _ = writeln!(
        err,
        "aterm harness accounts login: running; `{label}`'s sign-in is checked and recorded when \
         it ends"
    );
    let _ = out.flush();
    let _ = err.flush();
    let exited = match host.login(dir) {
        Ok(code) => code,
        Err(why) => {
            let _ = writeln!(
                err,
                "aterm harness accounts login: `claude` could not be run: {}",
                super::one_line(&why, 160)
            );
            return ExitCode::from(1);
        }
    };
    if exited != 0 {
        let _ = writeln!(
            err,
            "aterm harness accounts login: `claude auth login` exited {exited}"
        );
        // A failed flow still started `claude` under the home, which writes
        // its start-up files there: the row may now read enrolled
        // ([`accounts::login_made`]), and unasked it would read `unseen` —
        // fresh, `next:`, pinnable — with no login behind it. The row with no
        // `dir` is enrolled whatever a flow did.
        if !dir.is_some_and(|d| accounts::login_made(Path::new(d))) {
            return ExitCode::from(1);
        }
    }
    // Whether a login was made is read now: the flow is what makes it.
    let answer = ask_login(host, row, &accounts::enrolled(&roster));
    let _ = writeln!(out, "{label}: {}", answer.words());
    let _ = out.flush();
    match &answer {
        LoginCheck::CouldNotRun(_) => {
            let _ = writeln!(
                err,
                "aterm harness accounts login: `claude auth login` exited {exited}, but `{label}` \
                 could not be asked whether it is signed in, so nothing was recorded; `aterm \
                 harness accounts check` asks it again, with no browser"
            );
        }
        LoginCheck::NotEnrolled => {
            let _ = writeln!(
                err,
                "aterm harness accounts login: `claude auth login` exited {exited} and left no \
                 login in `{label}`'s directory, so nothing was asked or recorded"
            );
        }
        LoginCheck::SignedIn | LoginCheck::SignedOut => {}
    }
    if let Some(auth) = answer.auth() {
        // The answer is as new as the flow's end, however long the flow
        // waited for a person: the state is read against the clock of that
        // moment, since one read against the command's start would save the
        // stamps other writers made meanwhile moved back to it. Never behind
        // the command's own clock, should the machine's have been set back.
        let now = host.now().max(env.now);
        if let Err(why) = record_answers(env, now, &[(label, auth)], err) {
            let _ = writeln!(
                err,
                "aterm harness accounts login: the answer above was NOT recorded: {why}"
            );
            return ExitCode::from(1);
        }
    }
    if exited == 0 && answer == LoginCheck::SignedIn {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `aterm harness accounts check` (design §8), on the owner's word only: ask
/// each enrolled login whether it is signed in ([`ask_login`]), print one
/// line per account, and record each answer ([`record_answers`]).
///
/// THE COMMAND'S OUTPUT GOES NOWHERE. It names an email and an organization;
/// this reads its exit status and the one key `loggedIn`, and no byte of it
/// is printed, logged, journalled or kept.
///
/// The runs happen with NO lock held (each may take [`AUTH_CHECK_BUDGET`]);
/// the answers are recorded afterwards in one transaction. Exit 1 when a
/// login could not be asked, or the answers could not be recorded.
fn run_accounts_check(
    env: &Env,
    host: &dyn AccountsHost,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let Some((path, roster)) = accounts_read_roster(env, err) else {
        return ExitCode::from(1);
    };
    if roster.rows.is_empty() {
        let _ = write!(out, "{}", accounts_none_listed(&path));
        return ExitCode::SUCCESS;
    }
    let enrolled = accounts::enrolled(&roster);
    let mut code = ExitCode::SUCCESS;
    let mut seen: Vec<(&str, Auth)> = Vec::new();
    for row in &roster.rows {
        let answer = ask_login(host, row, &enrolled);
        let _ = writeln!(out, "{}: {}", row.label, answer.words());
        let _ = out.flush();
        match answer.auth() {
            Some(auth) => seen.push((&row.label, auth)),
            None if matches!(answer, LoginCheck::CouldNotRun(_)) => code = ExitCode::from(1),
            None => {}
        }
    }
    if seen.is_empty() {
        return code;
    }
    if let Err(why) = record_answers(env, env.now, &seen, err) {
        let _ = writeln!(
            err,
            "aterm harness accounts check: the answers above were NOT recorded: {why}"
        );
        return ExitCode::from(1);
    }
    code
}

/// `aterm harness accounts use <label>` and `use --auto` (design §6, "The
/// owner's word"): the pin, or its end, as one transaction under the store's
/// lock ([`accounts::pin`], [`accounts::unpin`]) with its ledger row and the
/// wake marker.
///
/// WHAT IT DOES NOT DO, said on its own line ([`ACCOUNTS_USE_RECORDED`]): it
/// moves no session itself. A window's supervisor reads the pin at each
/// hosted session's turn end and makes the move there; the wake marker this
/// transaction replaces is what tells a session WAITING at a limit to decide
/// again now.
///
/// A label the roster does not list, one not enrolled here and one known
/// signed out are refused by name, exit 1, with nothing written. `--auto`
/// with no pin standing writes nothing and says so.
fn run_accounts_use(
    env: &Env,
    label: Option<&str>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let store = env.accounts_store();
    let now = env.now;
    let done = match label {
        Some(label) => {
            let Some((_, roster)) = accounts_read_roster(env, err) else {
                return ExitCode::from(1);
            };
            let enrolled = accounts::enrolled(&roster);
            let remedy = match roster.row(label) {
                None => "`aterm harness accounts add <label>` lists an account".to_string(),
                Some(_) => {
                    format!("sign it in with `aterm harness accounts login {label}`, in a terminal")
                }
            };
            accounts_transact(&store, now, err, |state| {
                accounts::pin(state, &roster, &enrolled, label, now)
                    .map(|event| vec![event.because("the owner's word")])
                    .map_err(|why| format!("{why}; {remedy}"))
            })
            .map(|_| format!("{label}: the active account, pinned\n{ACCOUNTS_USE_RECORDED}"))
        }
        None => accounts_transact(&store, now, err, |state| {
            Ok(accounts::unpin(state, "auto", now).into_iter().collect())
        })
        .map(|events| match events.first() {
            Some(event) => format!(
                "{}: no longer pinned\nrecorded: it stays the active account; a session leaves \
                 it only at a usage limit, for the account the report names `next`",
                event.account
            ),
            None => "no pin holds: nothing to end".to_string(),
        }),
    };
    match done {
        Ok(said) => {
            let _ = writeln!(out, "{said}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts use: {why}");
            ExitCode::from(1)
        }
    }
}

/// Route one `accounts` form, the machine injected ([`AccountsHost`]) so the
/// tests drive `check` and `login` without spawning anything.
pub fn run_accounts(
    form: &AccountsCmd,
    env: &Env,
    host: &dyn AccountsHost,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    match form {
        AccountsCmd::Report { json } => run_accounts_report(env, *json, out, err),
        AccountsCmd::Add { label, class } => run_accounts_add(env, label, *class, out, err),
        AccountsCmd::Login { label } => run_accounts_login(env, host, label, out, err),
        AccountsCmd::Check => run_accounts_check(env, host, out, err),
        AccountsCmd::Use { label } => run_accounts_use(env, Some(label), out, err),
        AccountsCmd::Auto => run_accounts_use(env, None, out, err),
    }
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

/// The retired verbs something an older build installed may still run: the
/// bridge script `install` wrote calls `harness hook` and `harness
/// statusline` with the vendor's payload on stdin. They answer exit 0 with
/// nothing on stdout, which every hook event reads as "no opinion" and the
/// footer as an empty line, never the usage error (exit 2) that Claude Code
/// reads as a block.
fn is_tombstone(verb: &str) -> bool {
    matches!(verb, "hook" | "statusline")
}

/// Route one parsed command. Injected writers so the tests drive this directly.
pub fn run(cmd: &Cmd, env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    match cmd {
        Cmd::Help(page) => {
            let _ = out.write_all(help_page(*page).as_bytes());
            ExitCode::SUCCESS
        }
        Cmd::Retired { verb } => run_retired(verb, err),
        Cmd::Usage { json } => run_usage(env, &LiveClaude, *json, out),
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
        Cmd::Accounts(form) => run_accounts(form, env, &LiveAccounts, out, err),
    }
}

/// The front door's entry point (`aterm harness …`).
///
/// The retired verbs answer first, before any parsing and before the
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
    if matches!(cmd, Cmd::Disk { .. }) {
        // Whether a Claude Code session ended is read from what runs now,
        // and only `disk` asks it: no other verb reads the process table.
        env.claude_running = Some(disk::discover::running_claude_census(env.home.as_deref()));
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
