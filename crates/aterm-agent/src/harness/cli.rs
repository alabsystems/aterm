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
//! `aterm drive watch|supervise`.
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
//!   ended on, by the grammar of the program `status` names in front
//!   ([`aterm_phase::identify`] — the engine's turn-end policy acts on the
//!   same reading), and the windows the vendor painted: Claude Code's `/usage`
//!   panel ([`usage::usage_panel_windows`]), each reset placed by the
//!   supervisor's own clock ([`limit::parse_reset`]), or Codex's `/status`
//!   card ([`usage::codex_status_windows`]), each reset placed in the zone its
//!   TUI draws in ([`limits_view`]); a Codex wall's reset is the one the
//!   window's harness placed in that zone, where `status` publishes it
//!   (`reset_at=`, [`limits_view_placed`]).
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
//!   `use` records the owner's pin, and `login` runs the vendor's own
//!   sign-in flow at a terminal and, after a flow that exits 0, records what
//!   that one new login answers to `claude auth status --json` — exit status
//!   and the one key `loggedIn`, nothing else of that output read, printed
//!   or kept. A flow that fails runs nothing more under any login: that
//!   command spends an idle login's single-use refresh token, and only a
//!   login minted seconds ago has nothing to refresh
//!   ([`run_accounts_login`]). `adopt-default` lists this machine's own
//!   login as a renewable row, and `renew` renews a login before it ends:
//!   the vendor's sign-in into a new staging directory, ONE check in that
//!   home whose address is compared in memory with the row's, and only on a
//!   match the row bound to the new login ([`run_accounts_renew`]); then the
//!   same on each other machine named, over SSH.
//!   NOTHING HERE TOUCHES A SESSION: the move these facts feed is the
//!   WINDOW's — its supervisor relaunches a hosted Claude Code under the
//!   pinned account at its next turn end, under the next listed account at a
//!   usage limit and, under `early`, AHEAD of one (aterm-gui's
//!   `harness_host`) — and the report and `use` say what is built (that
//!   move, the usage probe that samples for it, and the limit reset, which
//!   ships off and has never been run against a real wall) and that none of
//!   it has been observed with two real accounts ([`ACCOUNTS_ROTATION`]).
//! * `upgrade` is the one verb here that ACTS: it moves a live Claude Code
//!   session onto a newer build in place, cooperatively
//!   ([`super::upgrade_drive`]), and a live Codex, its shared daemon first and
//!   then each TUI by a typed `/exit` and `codex resume`
//!   ([`super::upgrade_codex`]). It restarts the agent process, never answers
//!   a box or continues a turn; the window's host takes the same steps on its
//!   own tabs at each session's idle points, under aterm.toml's `[harness]
//!   upgrade` switch. A hand-run sweep obeys `[harness] enabled` and each
//!   agent's own `[harness.<agent>] enabled`
//!   ([`crate::supervise::SupervisorConfig::from_path`], the table's one
//!   reader).
//!
//! The hook bridge (`hook`, `statusline`, `install`, `uninstall`) is retired
//! ([`RETIRED`]) and answered before anything is parsed ([`main_entry`]).
//! So is `tap`, the usage tap ([`super::tap`]): Claude Code's own status-line
//! command on the relaunch lines aterm types under `[harness] usage_tap`,
//! which writes a reading of its own and prints one line, whatever happens.
//!
//! Every subcommand is one pass: read, decide, print, exit; nothing polls.
//!
//! * The retired `hook` and `statusline` **always exit 0** and print nothing:
//!   a bridge script an older `install` wrote still runs them, and Claude Code
//!   reads a failing hook command as a block (CHANGELOG.md, 0.92.0, "The Claude
//!   Code hooks."). `tap` always exits 0 too, and always prints one line.
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
//! scripted control connection, `accounts login` against a fake host
//! ([`AccountsHost`]): no test runs `claude`, and the verb has not been run
//! against a second real account.

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
use super::profile;
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
default, for every Claude Code, Codex and agy session (`aterm drive
watch|supervise` runs the same engine from a terminal).

USAGE:
    aterm harness usage [--json]                 this session's tokens per model, and its usage
                                                 tap's limits where it has one; from a Codex
                                                 session's tools, its conversation's, at list ≈$
    aterm harness limits [@<sid>] [--json]       the wall and the painted /usage or /status windows on the session's screen
    aterm harness disk [<build-dir> ...] [--apply <class>] [--json]
    aterm harness ledger [@<sid>] [<n>] [--json] the supervisor's approval ledger
    aterm harness ledger disk [<n>] [--json]     the disk journal
    aterm harness ledger upgrade [<n>] [--json]  the live upgrade's ledger: every act, and the owner's word
    aterm harness ledger accounts [<n>] [--json] the accounts ledger: sign-ins, pins, limits, rotations
    aterm harness accounts [--json]              the Claude Code accounts listed on this machine: each
                                                 one's condition, and which is next
    aterm harness accounts stats [--json]        what Claude Code's profile says of its account: the
                                                 plan and tier, usage credits, how Fable is billed
    aterm harness accounts add <label> [--shared] [--email <e>] [--browser <b>]
                                                 list an account: make its login directory, add its row
    aterm harness accounts adopt-default <label> --email <e> [--browser <b>]
                                                 list this machine's own login as one renewable row
    aterm harness accounts login <label>         sign it in: Claude Code's own flow, then record whether it took
    aterm harness accounts renew [--due | <label>...] [--on <host>,... | --here]
                                                 renew logins before they end: a staging sign-in, checked
                                                 against the row's email, then every due login elsewhere
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
    aterm harness tap                            the usage tap: Claude Code's status-line command
                                                 on aterm's relaunch lines (`[harness] usage_tap`)

`hook`, `statusline`, `install` and `uninstall` are RETIRED: aterm installs nothing into
an agent. `install`/`uninstall` refuse (exit 2); `hook` and `statusline` do nothing and
exit 0, for a bridge an older build installed. A starting window removes what `install`
wrote to ~/.claude/settings.json.

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
    --email <e>         accounts add, adopt-default: the account's address, which its renewal is checked against
    --browser <b>       accounts add, adopt-default: the browser profile (display name) holding its claude.ai session
    --due               accounts renew: every login of this machine that is due or ended
    --on <host>,...     accounts renew: the machines to renew on next, over ssh (`host` or `host:/abs/aterm`)
    --here              accounts renew: this machine's logins alone, no other machine (what each one runs)
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
said (`complete` is false in --json). Its windows (`5h`/`7d`, `windows` in --json) are
the usage tap's last reading for the Claude Code CLAUDE_PID names in this tab, where
`[harness] usage_tap` put one on its line (`aterm harness tap --help`); else `?`. The
window's Claude Code footer shows the same tokens live, and a limit the session hit.
`acct <label>` (`label` in --json) is `account` unless accounts.toml lists two or more
accounts and the run is from a Claude Code session's own tools with CLAUDE_PID set: then
it is the label of the row that process's login names, read from the environment the
process was STARTED with, never from this command's own. A login no row names, a Claude
home of its own (CLAUDE_CONFIG_DIR), a credential or a route in that environment, a
roster that is refused, the newest-transcript pick, and a CLAUDE_PID that is no longer
the Claude Code of the session folded all stay `account`; a credential given by a
settings file is not seen. No account's limits or state are read here: `aterm harness
accounts` prints them.

Run from a Codex session's own tools (CODEX_THREAD_ID names its thread) where no Claude
Code session is named, it folds that thread's conversation instead: the rollout of its
TUI's own thread (the `session_id` the named thread's rollout gives) and every subagent
rollout that names it, under CODEX_HOME, else ~/.codex (a relative CODEX_HOME folds
nothing, said). Each served turn counts once: the last_token_usage of a token_count
whose running total grew, on the model of the latest turn_context; a fork's copied
prefix (the burst of its parent's lines it opens with) counts nothing. `in` is the
uncached input; `cache read` is Codex's cached input. Where the shipped price table
prices the model, `usd` is its list dollars (≈$, never a bill); a model it does not is
unpriced (`usd` null). `acct codex`; its windows stay `?` (`aterm harness limits` reads
Codex's /status card).
";

/// `aterm harness limits --help`.
const HELP_LIMITS: &str = "\
`limits` reads the session's screen once over the control socket (`text --json tail=40`)
and prints the wall the last turn ended on (the kind the supervisor acts on, its words and
its reset; a Codex's reset where the window's harness placed it, in the Codex's own zone, as
`status` names it in `reset_at=`), read by the grammar of the program `status` names in front
(Claude Code's or Codex's), any windows that program painted (Claude Code's /usage panel,
Codex's /status card, whose `N% left` is printed as what is used and whose resets are placed
in the Codex's own zone), and `credits=in-use` where Claude Code's own notice says usage
credits carry the session (`-` otherwise; credits are work going on, never a wall). With no
@<sid> it reads the session it runs in ($ATERM_PARENT_SESSION_ID); with neither it refuses
(exit 2) rather than read whichever session the socket defaults to. No session answering is
exit 1.
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
$HOME's checkouts and their git worktrees two levels down (a `target -> target.noindex`
link resolved), x.py's bootstrap build directories (`build/<host>/stage<N>-<kind>`, never
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

/// `aterm harness tap --help`.
const HELP_TAP: &str = "\
`tap` is not run by hand: it is the status-line command aterm puts on the Claude Code
relaunch lines it types under `[harness] usage_tap = true` (off by default), an inline
`--settings` whose statusLine command is `<aterm> harness tap`, refreshed every 10 s.
Claude Code runs it after every reply and every 10 s, with its own usage figures on stdin;
it writes them into <aterm state>/harness/claude-harness/rl/<tab>-<pid>.json (0600) and
prints one line, `5h 42% \u{b7} 7d 61%`, which Claude Code draws under its composer. It
always exits 0. It is added only on the machine's own login (no CLAUDE_CONFIG_DIR, no
credential, profile or route of the session's own, no named login but the one `accounts
renew` bound the machine's one account to), only where nothing in the session's settings
sets a statusLine, disableAllHooks or an apiKeyHelper when the line is typed, only where
you passed no --settings, and only from an aterm whose path has no space; a session
started by hand gets it at its first relaunch (an upgrade, a restart).
Claude Code puts the line's --settings above your settings files, so a statusLine you set
later in a tapped session (/statusline, an edited file) shows from its next relaunch,
which leaves the tap off. A status line from an organization's server settings or a
plugin is not seen. Switched off, a tapped session keeps its tap, and is still recorded,
until its next relaunch. The window records the figures for this machine's account
(`aterm harness accounts`, `usage`); they move no session. A tapped session whose tap
goes quiet is said once a day in Messages.
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
draw on too), `dir` (the login's directory name; the one row with none is the login
this machine already has), and optionally `email` and `browser` (below). It ships empty,
and with no rows this verb says so, names the file it read and the `add` line to run — and,
where `[harness] usage_tap` recorded this machine's own login, that login's figures and when
the tap read them (in --json, the one row `main`). A file that does not admit — an unknown
key, a repeated label, dir or email, a relative dir, an email that is not one address, two
rows with no dir, more than 32 rows — is REFUSED whole (exit 1, the reason by name, an
address never quoted). No credential is stored, read or copied: an account is a login you
made through Claude Code's own flow, known here by its label; an email is read from this
file alone, and no email, person or organization name is printed or logged.
THE REPORT: `active: <label>, N of M sessions on it`, then one row per account — label,
class, condition (`not-enrolled`: no login was made here yet — its login directory is
missing or holds nothing, and `accounts login <label>` is what fills it — `accounts renew
<label>` for a row with an email; `signed-out`: a login wall right after a session was moved
onto it (before the move served a turn), a login wall on a row with an email (its login's own
end, renewed by `accounts renew`), a first login whose flow failed, or the check after a
sign-in said so (any other login wall records nothing yet); `limited until <time>`: a usage
limit stands; `low`: the session window at 90% or the week at 97%, until that window resets;
`usable`; `unseen`:
nothing observed yet), `active`,
`active:unproven` or `pinned` (`-` for neither), the limit-reset offer last read (`offer: N left, use by <time>`, only once one was read), the sessions on
it (`on: @s-…`, `(held: jobs)` for one that could not move) — then `next:`, the account
a session at a limit would move to, or the time to wait until, and every other account's
reason by name. The session and week percent, with their resets and when they were
sampled, are `--json`'s alone (`null` for a figure nobody read, never 0). WHAT MOVES A SESSION is a window's supervisor (`[harness] accounts`, on unless
`off`, and off with Claude Code's own `[harness.claude] enabled = false`): a supervised Claude Code session is relaunched on its own conversation under the
pinned account (`use`) at its next turn end, and under `next` when its turn ends on a
usage limit — one session first, the others once that account has served a turn. A
session waiting at the limit of the very account `use` names is continued where it is.
`accounts = \"off\"` stops all of that for the window's sessions: nothing is advised,
moved or proven there, and a session's own limit is still recorded for this report. (The
login lifecycle below has its own switch, `[harness] logins`, and goes on under it.)
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
0 it checks that one new login — `claude auth status --json` under
CLAUDE_SECURESTORAGE_CONFIG_DIR=<dir> (empty for the row with no dir), 20 s at most —
prints `<label>: signed in | signed out | not enrolled | could not run (<why>)` and records
the answer: exit 0 for `signed in`, exit 1 for anything else. It reads the exit status and
the one key `loggedIn`; the rest of that output, which names an email and an organization,
is never printed, logged or kept. This verb runs that command only then: under a login
that is not brand new it spends the single-use refresh token (anthropics/claude-code#95822),
and a token minted seconds ago has nothing to refresh. A FLOW THAT FAILS — it exits
non-zero, Ctrl-C included (on unix this command waits through the key) — RUNS NOTHING MORE
UNDER ANY LOGIN (no check follows it) and is said (exit 1). Over a login directory that held
anything before the flow, and for the row with no dir, nothing is recorded (one an earlier,
unrecorded flow left its files in stays as it was). Over one that was EMPTY before the flow,
where the flow left Claude Code's start-up files (written there before any sign-in), the row
is recorded `signed out`: those files enrol it, and `unseen` would offer it as `next` with
no login behind it. A flow that left nothing records nothing. A `claude` that cannot be
started is said (exit 1) with nothing run and nothing recorded. With stdin not a terminal it
prints the line, runs nothing and exits 0. With neither USER nor LOGNAME set the check would read
a login that works as signed out, so the flow is not started (exit 1). The row with no dir
is this machine's own login: its line is `claude auth login`, run in the default home with
neither CLAUDE_CONFIG_DIR nor CLAUDE_SECURESTORAGE_CONFIG_DIR set, and checked alike.
THE LOGIN LIFECYCLE (rows with an `email`; docs/DESIGN-claude-accounts-v2-2026-10-15.md §5).
A row may carry `email` (the account's address) and `browser` (the display name of the
browser profile that holds its claude.ai session). The address is read from this file to
hint the sign-in and check a renewal, compared in memory, and never printed, logged or
kept elsewhere. Such a row's login is `ok` (a turn served in the last 6 h), `idle`, `due`
(its forecast end within 5 days) or `expired` (a login wall in a session on it), and the
report adds `login: <state>, renew by <date>`: the forecast is the last renewal plus a
lifetime learned per login (3 weeks to start, lowered by each end seen and by Claude Code's
own notice that the login expires in N days), never by running anything under the login.
`accounts adopt-default <label> --email <e> [--browser <b>]` lists this machine's own login
(the row with no dir) as such a row; `add --email` lists another.
`accounts renew [--due | <label>...] [--on <host>,... | --here]` renews each login named,
and with --due every due or expired one: `CLAUDE_CONFIG_DIR=<state>/accounts/logins/<label>+<n>
claude auth login --claudeai --email <e>` into a NEW staging directory, its page opened in
the row's browser profile (aterm's own opener, `aterm-open-url`; else your default browser).
A flow that exits non-zero (Ctrl-C included) runs nothing more and records nothing. One
that exits 0 is checked ONCE, in that staging home — `claude auth status --json`, its
address compared with the row's — and only on a match is the row bound to the new login
(<state>/accounts/logins.json) and recorded `ok`: sessions on the old login move onto it at
their next idle turn end (one stopped at its ended login is relaunched there, with its wall's
continuation) where a window's supervisor relaunches — `[harness] enabled`, Claude Code's
`[harness.claude] enabled`, `logins`, `relaunch` and `resume_limits` on, which the line says — and it prints the line that starts
Claude Code under the new login by hand (`CLAUDE_SECURESTORAGE_CONFIG_DIR='<dir>'
claude --resume`). Another account's address is REFUSED: the staging directory is removed,
`wrong-account` recorded, and nothing live changes. Then, for each machine `--on` names
(with --due and no --on, `[harness] hosts`), `ssh -t <host> aterm harness accounts
renew --due --here` runs there, in this terminal — `--here`: that machine's own logins
alone, whatever its own hosts list names, so one sitting goes one machine deep — and over
ssh Claude Code prints its URL and waits for the code you paste at its own prompt (open the
URL in the profile it names); aterm reads and relays nothing of it. A row with an email signs
in only this way: `accounts login` refuses it. A window's supervisor never types /login into
a session on such a login: it waits, asks you once to renew it, and relaunches it once
renewed. Where it does not wait (`relaunch` or `resume_limits` off, a draft in the composer)
it flags the tab with the renewal to run, and once renewed with what still holds the tab on
its old login, which no renewal reaches: the draft (send or clear it, and the tab moves by
itself), or, where nothing relaunches it, the line `renew` printed (quit Claude Code and
start it with that line). `[harness] logins = \"off\"` hands the lost login back to /login.
aterm never deletes a keychain item: each renewal leaves the old one, which nothing reads.
`accounts use <label>` is THE OWNER'S WORD: the account's recorded limits are cleared and
it is the active account, pinned. A label that is not listed, not enrolled or signed out
is refused by name (exit 1). `accounts use --auto` ends the pin. Both are recorded, and a
window's supervisor acts on them: each supervised Claude Code session moves to the pinned
account at its next turn end (a session waiting at a limit is told at once, and one
waiting at that account's own limit is continued there).
A state file this build cannot read as its own — another schema, unreadable, too large —
is never written over: `use` refuses and `login` records nothing (exit 1, naming the file),
and the report shows an empty state beside the reason. `accounts` takes no --dry-run and
no --sock.
`ledger accounts` is the history (<state>/accounts/ledger.jsonl, cut to its newest 1024
rows past 1 MiB): `wall`, `sample`, `rotate`, `proven`, `struck`, `move`, `probe`,
`reset-used`, `reset-kept`, `reset-failed`, `auth`, `use`, `unpin`, each with its account,
session and reason. This command writes `auth`, `use` and `unpin`; a window's supervisor
writes `wall`, `move`, `rotate`, `proven` and `struck` (with the `auth` and `unpin` they
bring), under `early` `probe` (a `/usage` read claimed) and `sample` (what it read), and
with `limit_reset` on the one `reset-…` row of each limit-reset attempt, with its reason.
`accounts stats [--json]` READS WHAT CLAUDE CODE'S PROFILE SAYS OF ITS ACCOUNT, on demand:
the `.claude.json` of the home this command's environment names (beside ~/.claude with
CLAUDE_CONFIG_DIR unset; inside the directory a set one names, ~/.claude itself included;
in $HOME for an empty one), named `.claude-custom-oauth.json` with
CLAUDE_CODE_CUSTOM_OAUTH_URL set — or, as Claude Code 2.1.301 does, a `.config.json` in
the Claude directory (~/.claude, or CLAUDE_CONFIG_DIR's; none is looked for under an
empty one with no CLAUDE_PID, whose directory is not known) in its place when that is not
empty, or is empty and writable while the other is absent. It prints the plan (Max, Pro,
Team, Enterprise) and its tier (`5x`), whether usage credits are `on`, `out` or `off
(<reason>)` — the word Claude Code keeps from its last served reply — and, as a hint, how
the plan bills Fable: `Fable included`, `credits only`, `limits end <date>` or `unknown`
(a flag Claude Code caches under another name or shape than the one read). The account is
labelled by its home, whichever of its files was read, by the first that applies: the
roster row of the home where it is enrolled (for ~/.claude.json, CLAUDE_CONFIG_DIR unset
or empty, the row with no `dir`; for any other, the row whose `dir` names
CLAUDE_CONFIG_DIR, once its login was made); else `main` for ~/.claude.json; else
`home-<key6>`. --json adds its key (`c-` and twelve hex digits of a hash of the account's
own id, the same on every machine) and the billing, seat and display words. The file is
read by an allowlist: those fields and nothing else — never its API key, its MCP servers'
env or headers, or the email, names and ids of the login — so none of that is printed or
kept; the id is hashed as it is read. A home that could not be resolved (a relative
CLAUDE_CONFIG_DIR with no CLAUDE_PID, or no absolute $HOME), one with no profile, or a
profile that is not JSON says so (`no-home`, `no-profile`, `not-a-file`, `too-large`,
`unreadable`). Writes nothing; exit 0. It reads this machine only, Claude Code only, and
no usage window yet.
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
the sweep off, and `upgrade` says on stderr where it found it. An agent's own
`[harness.<agent>] enabled = false` leaves that agent's sessions out of the sweep (said
on stderr) while the others are swept, and a `--now`/`--defer`/`--skip` on its tab is
recorded but waits, as under `[harness] enabled = false`.
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
    /// The profile Claude Code keeps for the home this command's
    /// environment names (`.claude.json`, `claude_profile_of`: beside
    /// `~/.claude` with `CLAUDE_CONFIG_DIR` unset, inside the directory a set
    /// one names, `$HOME`'s for an empty one); `None` where it cannot be
    /// named. The home's identity (`stats_label`); `accounts stats` reads
    /// the file Claude Code reads in its place ([`profile::effective`]), only
    /// through the allowlist reader ([`profile::read`]).
    pub claude_profile: Option<PathBuf>,
    /// `CLAUDE_CODE_CUSTOM_OAUTH_URL` is set and not empty
    /// ([`profile::custom_oauth`]): Claude Code then names the profile
    /// `.claude-custom-oauth.json` ([`profile::effective`]).
    /// aterm strips `CLAUDE_*` from the shells it spawns, so only Claude
    /// Code's own tools carry it.
    pub claude_custom_oauth: bool,
    /// The Claude Code session this command runs under, when the vendor named
    /// it: `CLAUDE_CODE_SESSION_ID`, which Claude Code 2.1.283 sets for every
    /// tool it runs. aterm strips `CLAUDE_*` from the shells it spawns, so only
    /// Claude Code's own tools carry it.
    pub claude_session: Option<String>,
    /// The Claude Code process this command runs under (`CLAUDE_PID`, set
    /// beside the session id), for a build that names only the process.
    pub claude_pid: Option<u32>,
    /// The Codex thread this command runs under, when Codex named it:
    /// `CODEX_THREAD_ID`, which Codex sets for the commands its tools run
    /// (a thread id, [`super::upgrade_codex::is_thread_id`]). aterm strips
    /// `CODEX_*` from the shells it spawns, so only Codex's own tools carry
    /// it.
    pub codex_thread: Option<String>,
    /// `CODEX_HOME` as this command's environment sets it, raw: the Codex
    /// home is it, else `$HOME/.codex`, and a relative one names none
    /// ([`crate::supervise::codex_usage::codex_home`]).
    pub codex_home: Option<String>,
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
        let claude_pid = std::env::var("CLAUDE_PID")
            .ok()
            .and_then(|p| p.parse::<u32>().ok())
            .filter(|&p| p > 0);
        // RAW, as Claude Code reads it: an EMPTY one is set (`??`), and it and
        // a relative one name a directory under the one the Claude Code this
        // command runs under runs in (`CLAUDE_PID`'s), never this command's
        // own (its Bash tool's `cd` moves that) — by the one resolver
        // ([`footer::claude_dir_of`]).
        let config_dir = std::env::var("CLAUDE_CONFIG_DIR").ok();
        let claude_dir = footer::claude_dir_of(
            config_dir.as_deref(),
            std::env::var("HOME").ok().as_deref(),
            || claude_pid.and_then(aterm_sysprobe::process_cwd),
        );
        let claude_profile = claude_profile_of(
            home.as_deref(),
            config_dir.as_deref(),
            claude_dir.as_deref(),
        );
        let claude_config_unusable = config_dir.filter(|_| claude_dir.is_none());
        let claude_custom_oauth =
            profile::custom_oauth(std::env::var_os("CLAUDE_CODE_CUSTOM_OAUTH_URL").as_deref());
        let claude_session = std::env::var("CLAUDE_CODE_SESSION_ID")
            .ok()
            .filter(|id| footer::is_session_id(id));
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
            claude_profile,
            claude_custom_oauth,
            claude_session,
            claude_pid,
            codex_thread: std::env::var("CODEX_THREAD_ID")
                .ok()
                .filter(|id| super::upgrade_codex::is_thread_id(id)),
            codex_home: std::env::var("CODEX_HOME").ok(),
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
    /// `accounts add <label> [--shared] [--email <e>] [--browser <b>]`: make
    /// the login directory, append the roster row.
    Add {
        /// The label as typed; admitted when the verb runs, so an
        /// inadmissible one is a refusal by name (exit 1), not a usage error.
        label: String,
        /// `--shared`, else dedicated.
        class: Class,
        /// `--email`: the row renews through `accounts renew`.
        email: Option<String>,
        /// `--browser`: the profile its sign-in opens in.
        browser: Option<String>,
    },
    /// `accounts adopt-default <label> --email <e> [--browser <b>]`: this
    /// machine's own login (the row with no `dir`) as one renewable row.
    AdoptDefault {
        /// The label as typed.
        label: String,
        /// `--email`, required.
        email: String,
        /// `--browser`.
        browser: Option<String>,
    },
    /// `accounts renew [<label>…] [--due] [--on <host>,… | --here]`: each
    /// named login, or every due one, renewed through a staging directory and
    /// checked before it goes live ([`run_accounts_renew`]); then `renew --due
    /// --here` on each other machine named.
    Renew {
        /// The labels named, in order.
        labels: Vec<String>,
        /// `--due`: every login of this machine that is due.
        due: bool,
        /// `--on`: the machines to renew on next, over SSH; `None` is
        /// `[harness] hosts` (read only with `--due`). `--here` is
        /// `Some` and empty: this machine's logins alone — what a sitting
        /// runs on each other machine, so that run never goes on to others.
        on: Option<Vec<String>>,
    },
    /// `accounts login <label>`: the vendor's sign-in flow for that account,
    /// waited for, and — after a flow that exits 0 — what its new login then
    /// answers recorded.
    Login {
        /// The label as typed.
        label: String,
    },
    /// `accounts use <label>`: the owner's pin.
    Use {
        /// The label as typed.
        label: String,
    },
    /// `accounts use --auto`: end the pin.
    Auto,
    /// `accounts stats [--json]`: what this machine's Claude Code profile
    /// says of its account. Writes nothing.
    Stats {
        /// The JSON form.
        json: bool,
    },
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
    /// `tap`: the usage tap ([`super::tap::run`]). The front door runs a
    /// bare `tap` before anything is parsed ([`main_entry`]); this is the
    /// same verb reached past a flag.
    Tap,
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
    /// `tap --help`: the usage tap.
    Tap,
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
        HelpPage::Tap => ("tap", false, &[HELP_TAP]),
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
    let mut due = false;
    let mut here = false;
    let mut email: Option<String> = None;
    let mut browser: Option<String> = None;
    let mut on: Option<Vec<String>> = None;
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
            "--due" => due = true,
            "--here" => here = true,
            "--email" => {
                email = Some(value(i, "--email")?);
                i += 1;
            }
            "--browser" => {
                browser = Some(value(i, "--browser")?);
                i += 1;
            }
            "--on" => {
                let hosts: Vec<String> = value(i, "--on")?
                    .split(',')
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .map(str::to_owned)
                    .collect();
                on.get_or_insert_with(Vec::new).extend(hosts);
                i += 1;
            }
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
            Some("tap") => HelpPage::Tap,
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
        "tap" => {
            if rest.len() > 1 {
                return Err(format!("tap takes no operand, not {:?}", &rest[1..]));
            }
            Cmd::Tap
        }
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
                [] => Cmd::Upgrade { sid, dry_run, json },
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
                    Cmd::UpgradeOwner {
                        sid,
                        ask: *ask,
                        json,
                    }
                }
                _ => {
                    return Err("one of --status, --now, --defer and --skip at a time".to_string());
                }
            }
        }
        "accounts" => {
            let form = match &rest[1..] {
                [] => AccountsCmd::Report { json },
                ["stats"] => AccountsCmd::Stats { json },
                ["add", label] => AccountsCmd::Add {
                    label: (*label).to_string(),
                    class: if shared {
                        Class::Shared
                    } else {
                        Class::Dedicated
                    },
                    email: email.take(),
                    browser: browser.take(),
                },
                ["adopt-default", label] => AccountsCmd::AdoptDefault {
                    label: (*label).to_string(),
                    email: email.take().ok_or_else(|| {
                        "accounts adopt-default needs --email <address>: the login it renews \
                         is checked against that address"
                            .to_string()
                    })?,
                    browser: browser.take(),
                },
                ["renew", labels @ ..] => {
                    if labels.is_empty() && !due && on.is_none() {
                        return Err(
                            "accounts renew takes --due, the labels to renew, or --on <host>,…"
                                .to_string(),
                        );
                    }
                    if here && on.is_some() {
                        return Err(
                            "accounts renew: --here renews this machine alone and --on names \
                             others; take one"
                                .to_string(),
                        );
                    }
                    AccountsCmd::Renew {
                        labels: labels.iter().map(|l| (*l).to_string()).collect(),
                        due,
                        // `--here`: no machine next, whatever the config names.
                        on: if here { Some(Vec::new()) } else { on.take() },
                    }
                }
                ["login", label] => AccountsCmd::Login {
                    label: (*label).to_string(),
                },
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
                        "accounts takes nothing, `stats`, `add <label> [--shared] [--email <e>] \
                         [--browser <b>]`, `adopt-default <label> --email <e>`, `login <label>`, \
                         `renew [--due | <label>…] [--on <host>,…]` or `use <label> | --auto`, not \
                         {other:?}"
                    ));
                }
            };
            if json && !matches!(form, AccountsCmd::Report { .. } | AccountsCmd::Stats { .. }) {
                return Err(
                    "--json is the report's form (`aterm harness accounts --json`) and \
                     `accounts stats --json`: the other forms print lines"
                        .to_string(),
                );
            }
            Cmd::Accounts(form)
        }
        other => return Err(format!("unknown subcommand {other:?}")),
    };
    if !owner.is_empty() && !matches!(cmd, Cmd::UpgradeOwner { .. }) {
        return Err("--status, --now, --defer and --skip belong to `upgrade`".to_string());
    }
    if shared && !matches!(cmd, Cmd::Accounts(AccountsCmd::Add { .. })) {
        return Err("--shared belongs to `accounts add <label>`".to_string());
    }
    if auto && !matches!(cmd, Cmd::Accounts(AccountsCmd::Auto)) {
        return Err("--auto belongs to `accounts use`".to_string());
    }
    // Taken by the forms they belong to above: one left over is on another.
    if email.is_some() || browser.is_some() {
        return Err(
            "--email and --browser belong to `accounts add <label>` and `accounts adopt-default \
             <label>`"
                .to_string(),
        );
    }
    if (due || here || on.is_some()) && !matches!(cmd, Cmd::Accounts(AccountsCmd::Renew { .. })) {
        return Err("--due, --here and --on belong to `accounts renew`".to_string());
    }
    // A flag `accounts` does not take is refused, never ignored: `add`,
    // `login` and `use` WRITE, and one that swallowed `--dry-run` would
    // write for real under a flag that promises it will not.
    if matches!(cmd, Cmd::Accounts(_)) {
        if dry_run {
            return Err(
                "accounts takes no --dry-run: `add`, `login` and `use` do what they say, and \
                 the report writes nothing"
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
    let dir = claude_dir
        .join("projects")
        .join(footer::project_slug(&env.cwd));
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
        let roster = env.accounts_store().bound(&roster, env.now).ok()?;
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

/// THE USAGE TAP'S LAST READING for the Claude Code this command runs under
/// ([`super::tap::Reading::read`]): its own file, named by this tab
/// (`ATERM_PARENT_SESSION_ID`, [`Env::sid`]) and `CLAUDE_PID`, written since
/// the kernel started that process, under the aterm state root the window
/// keeps it in. `None` — and the windows stay unknown — with no tab, no pid,
/// no state root, no start record, or no such file.
fn usage_reading(env: &Env, process: &dyn ClaudeProcess) -> Option<super::tap::Reading> {
    let pid = env.claude_pid?;
    let dir = super::tap::rl_dir_in(&env.aterm_state.as_ref()?.join("harness").join(HARNESS));
    // The kernel is asked only where a file is there to be judged.
    std::fs::symlink_metadata(dir.join(super::tap::file_name(&env.sid, pid)?)).ok()?;
    let started = i64::try_from(process.started(pid)?).ok()?;
    super::tap::Reading::read(&dir, &env.sid, pid, started).map(|(reading, _)| reading)
}

/// `aterm harness usage`.
fn run_usage(env: &Env, process: &dyn ClaudeProcess, json: bool, out: &mut dyn Write) -> ExitCode {
    let pick = pick_transcript(env);
    // A Codex session's own tools name its thread; a Claude Code session the
    // vendor named still comes first, as before.
    if let (Pick::Newest(_) | Pick::None, Some(thread)) = (&pick, &env.codex_thread) {
        return run_codex_usage(env, thread, json, out);
    }
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
    let reading = usage_reading(env, process);
    if folded.is_some() || reading.is_some() {
        let mut account = AccountView::new(usage_label(env, &pick, process), true);
        if let Some((_, _, total)) = &folded {
            account.add_transcript(total);
        }
        if let Some(reading) = &reading {
            account.add_reading(reading, env.now);
        }
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

/// The label `usage` gives a Codex conversation's account: the account
/// stats' label for Codex's (the design's §3.1). Which login it is is not
/// read here.
const USAGE_CODEX_ACCOUNT: &str = "codex";

/// `aterm harness usage` from a Codex session's own tools: every served turn
/// of the conversation the thread `CODEX_THREAD_ID` names belongs to — its
/// TUI's own thread and the subagents spawned in it
/// ([`crate::supervise::codex_usage::conversation_rollouts`]) — in the Codex
/// home this command's environment names, each turn once and nothing of a
/// fork's copied prefix ([`super::spend::fold_conversation`]), with the list
/// dollars of the shipped price table where it prices a model. Its windows
/// stay unknown.
fn run_codex_usage(env: &Env, thread: &str, json: bool, out: &mut dyn Write) -> ExitCode {
    use super::spend;
    use crate::supervise::codex_usage;
    // This command runs in the session's tool shell, whose folder need not
    // be the one the TUI started in: a relative CODEX_HOME places no home
    // here, and nothing is read.
    let home = codex_usage::codex_home(
        env.codex_home.as_deref(),
        env.home.as_deref().and_then(Path::to_str),
        None,
    );
    let conversation = home
        .as_deref()
        .map(|h| codex_usage::conversation_rollouts(h, thread));
    // The shipped table loads (its own test); one that did not would price
    // nothing, never guess.
    let prices = spend::PriceTable::from_toml(spend::PRICES_TOML).unwrap_or_default();
    let fold = conversation
        .as_ref()
        .filter(|c| !c.files.is_empty())
        .map(|c| {
            spend::fold_conversation(&c.files, c.own.as_deref(), &prices, MAX_TRANSCRIPT_BYTES)
        });
    let mut view = UsageView::new(env.now);
    if let Some(fold) = &fold {
        let mut account = AccountView::new(USAGE_CODEX_ACCOUNT, true);
        for (model, m) in &fold.models {
            account.spend.insert(
                model.clone(),
                usage::SpendView {
                    input: m.tokens.input,
                    output: m.tokens.output,
                    cache_write: m.tokens.cache_write,
                    cache_read: m.tokens.cache_read,
                    usd: (!m.unpriced).then_some(m.usd),
                },
            );
        }
        account.model.clone_from(&fold.model);
        view.accounts.push(account);
    }
    let source = if fold.is_some() {
        Source::Rollout
    } else {
        Source::None
    };
    let unread =
        conversation.as_ref().map_or(0, |c| c.unread) + fold.as_ref().map_or(0, |f| f.unread);
    let complete = fold.as_ref().is_some_and(|f| f.caught_up && unread == 0);
    if json {
        // Schema 1: keys are only ever added, never changed.
        let mut extra = vec![("codex_thread", Value::from(thread.to_owned()))];
        if let Some(c) = &conversation {
            if let Some(own) = &c.own {
                extra.push(("rollout", Value::from(own.display().to_string())));
            }
            extra.push(("conversation", Value::from(c.root.clone())));
            extra.push(("subagent_rollouts", Value::from(c.subagents as u64)));
            extra.push(("rollouts_unread", Value::from(unread as u64)));
        }
        extra.push(("transcript_pick", Value::from("codex-thread")));
        if let Some(f) = &fold {
            extra.push(("turns", Value::from(f.turns)));
            extra.push(("copied", Value::from(f.copied)));
            extra.push(("complete", Value::from(complete)));
            extra.push((
                "prices_as_of",
                Value::from(prices.as_of().unwrap_or_default().to_owned()),
            ));
        }
        if home.is_none()
            && let Some(set) = &env.codex_home
        {
            extra.push(("codex_home_unusable", Value::from(set.clone())));
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
    let (Some(c), Some(f), Some(home)) = (&conversation, &fold, &home) else {
        let why = match (&home, &env.codex_home) {
            (None, Some(set)) => format!(
                "CODEX_HOME is {set:?}, not an absolute path: the Codex home it names cannot be \
                 found from here, and ~/.codex is not it — nothing to fold"
            ),
            (None, None) => {
                "no home directory is known, so no Codex home — nothing to fold".to_owned()
            }
            (Some(home), _) => format!(
                "Codex thread {thread} has written no rollout under {} yet — nothing to fold",
                home.join("sessions").display()
            ),
        };
        let _ = writeln!(out, "{why}");
        return ExitCode::SUCCESS;
    };
    let all = f
        .models
        .values()
        .fold(usage::TokenUsage::default(), |sum, m| sum.plus(&m.tokens));
    let priced = f.models.values().all(|m| !m.unpriced);
    let usd: f64 = f.models.values().map(|m| m.usd).sum();
    let tokens = if f.models.is_empty() {
        "no tokens".to_owned()
    } else {
        format!(
            "\u{2191}{} \u{2193}{} \u{00B7} {}",
            session_usage::tokens_short(all.prompt()),
            session_usage::tokens_short(all.output),
            if priced {
                format!("\u{2248}${usd:.4} (list)")
            } else {
                format!("\u{2248}${usd:.4} (list) of the priced models; some unpriced")
            }
        )
    };
    let _ = writeln!(out, "{} {tokens}", footer::USAGE_MARK);
    for (model, m) in &f.models {
        let price = if m.unpriced {
            "unpriced".to_owned()
        } else {
            format!("\u{2248}${:.4} (list)", m.usd)
        };
        let approx = if m.approx > 0 {
            format!(", {} of its turns approximate", m.approx)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "  {model}: in {} (cache write {}, cache read {}), out {}, {} turns \u{00B7} {price}{approx}",
            m.tokens.input, m.tokens.cache_write, m.tokens.cache_read, m.tokens.output, m.turns
        );
    }
    let mut short = String::new();
    if f.read >= MAX_TRANSCRIPT_BYTES {
        short.push_str(&format!(
            " The fold stopped at its {} GiB cap before the end: these totals are a PREFIX.",
            MAX_TRANSCRIPT_BYTES >> 30
        ));
    } else if !f.caught_up {
        short.push_str(" A rollout could not be read to its end: these totals are partial.");
    }
    if unread > 0 {
        short.push_str(&format!(
            " {unread} rollouts that may be this conversation's could not be read: they are NOT in \
             these totals."
        ));
    }
    // `files` holds the TUI's thread's rollout first, where it was found,
    // then the subagents'.
    let folded = match c.files.first() {
        Some(first) if c.files.len() > c.subagents => format!(
            "{} (+{} subagent rollouts; {} turns)",
            first.display(),
            c.subagents,
            f.turns
        ),
        _ => format!(
            "{} subagent rollouts ({} turns; its TUI's thread {} has none here)",
            c.subagents, f.turns, c.root
        ),
    };
    let _ = writeln!(
        out,
        "SPEND folded from {folded}, the conversation of Codex thread {thread} \
         (CODEX_THREAD_ID) under {}. A fork's copied prefix counts nothing ({} copied token \
         counts).{short} \u{2248}$ is the list price of the table of {}, never a bill. A \
         rollout's rate-limit windows are not read here: `aterm harness limits` reads Codex's \
         /status card.",
        home.display(),
        f.copied,
        prices.as_of().unwrap_or("?"),
    );
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// limits
// ---------------------------------------------------------------------------

/// What one screen read says about a limit: the wall the worker's last turn
/// ended on, read by the tab's own reader ([`aterm_phase::identify`]: Claude
/// Code's grammar or Codex's, the one the turn-end policy also decides on),
/// and the windows the vendor painted: Claude Code's `/usage` panel
/// ([`usage::usage_panel_windows`]), Codex's `/status` card
/// ([`usage::codex_status_windows`]). Every reset is placed on the clock by
/// the supervisor's grammar ([`limit::parse_reset`], [`limit::reset_at`];
/// the card's by [`limit::parse_card_reset`]).
#[derive(Debug, Clone, PartialEq)]
pub struct LimitsView {
    /// The wall, when the screen shows one.
    pub wall: Option<aterm_phase::Wall>,
    /// The wall's reset as Unix seconds, when its words place one.
    pub resets_at: Option<i64>,
    /// Each painted window, with its reset placed (`None`: none painted, or
    /// words or a zone that place none).
    pub windows: Vec<(usage::PanelWindow, Option<i64>)>,
    /// Usage credits carry the session, by Claude Code's own notice
    /// ([`aterm_phase::credits::in_use`]): work going on, never a wall.
    pub credits: bool,
}

/// [`LimitsView`] of `rows`, read by `reader` (the tab's: [`aterm_phase::identify`]
/// of its `status` `program=`), as of `now`: a reset that names no zone (or one
/// `zone_offset_at` does not know) is at `local_offset_s`. A zone's offset is
/// read AT the instant placed for ([`limit::zone_offset_at`] in production), as
/// the loop that waits for the reset reads it, so a reset across a
/// daylight-saving change shows the time the supervisor will continue at.
///
/// A CODEX TAB's windows are its `/status` card's, and the card names no zone:
/// codex-rs draws it with the TUI's own clock. `codex_offset_at` is that
/// zone's offset at an instant (the `TZ` of the TUI's exec environment, else
/// the machine's: [`super::upgrade_drive::codex_zone_offset_at`]), and a reset
/// is placed in it as its wall's is (`upgrade_drive::codex_wall_reset`): a
/// bare clock is the reading's own day, never tomorrow. Unlike a wall's words,
/// which the daemon may write, the card is the TUI's own drawing, so its
/// daemon's zone is not asked. `None` there (the TUI unread) places no reset,
/// never one in this process's zone. Another tab never asks it.
#[must_use]
pub fn limits_view(
    reader: &dyn aterm_phase::ScreenReader,
    rows: &[String],
    now: i64,
    local_offset_s: i64,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
    codex_offset_at: &dyn Fn(i64) -> Option<i64>,
) -> LimitsView {
    limits_view_placed(
        reader,
        rows,
        now,
        local_offset_s,
        zone_offset_at,
        codex_offset_at,
        None,
    )
}

/// [`limits_view`], a CODEX tab's wall reset taken from `placed`: where the
/// window's harness placed it (the tab's `status` `reset_at=`, unix seconds;
/// `aterm-gui`'s `presence::reset_at_field`), in the Codex's own zone, as
/// its loop and the tab's countdown place it — the zone the card's windows
/// are placed in too ([`super::upgrade_drive::codex_zone_offset_at`]). A
/// Codex prints its `try again at` time in its own process's zone, which a
/// `TZ` its shell exported sets and this process never sees. `None` (an
/// aterm that prints no such field, `reset_at=-`, or no harness placing
/// it): the wall's words placed here, in this process's zone, as before.
/// Claude Code's wall names its zone and is never taken from it.
#[must_use]
pub fn limits_view_placed(
    reader: &dyn aterm_phase::ScreenReader,
    rows: &[String],
    now: i64,
    local_offset_s: i64,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
    codex_offset_at: &dyn Fn(i64) -> Option<i64>,
    placed: Option<i64>,
) -> LimitsView {
    let place = |text: &str| {
        limit::parse_reset(text)
            .map(|spec| limit::reset_at_with(&spec, now, local_offset_s, zone_offset_at))
    };
    let wall = reader.wall(rows);
    // Only a Codex wall's reset is placed by its harness (`reset_at=`).
    let codex = match reader.program() {
        aterm_phase::Program::Codex => true,
        aterm_phase::Program::Claude
        | aterm_phase::Program::Agy
        | aterm_phase::Program::Muse
        | aterm_phase::Program::Generic => false,
    };
    // The wall's reset is placed as the loop that waits for it places it: on
    // Claude Code's screen a session window's by the five-hour rule, any other
    // wall's bare clock as the first occurrence after the print the screen's
    // done row gives; Codex's where its harness placed them, else where its
    // words first fall. So `harness limits` says the time the supervisor will
    // continue at.
    let resets_at = wall.as_ref().and_then(|w| {
        if codex && let Some(at) = placed {
            return Some(at);
        }
        let spec = limit::parse_reset(w.reset.as_deref()?)?;
        if !crate::supervise::run::places_resets_on_print(reader.program()) {
            return Some(limit::reset_at_with(
                &spec,
                now,
                local_offset_s,
                zone_offset_at,
            ));
        }
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
    // Each program's own surface: a Claude Code panel left on a Codex tab's
    // screen is no window of this Codex's.
    let windows = match reader.program() {
        aterm_phase::Program::Codex => usage::codex_status_windows(rows)
            .into_iter()
            .map(|w| {
                let at = w
                    .reset_text
                    .as_deref()
                    .and_then(limit::parse_card_reset)
                    .and_then(|spec| limit::reset_in_zone(&spec, now, 0, codex_offset_at));
                (w, at)
            })
            .collect(),
        // Neither draws a usage surface anyone measured: no window is read
        // off their screens.
        aterm_phase::Program::Agy | aterm_phase::Program::Muse => Vec::new(),
        aterm_phase::Program::Claude | aterm_phase::Program::Generic => {
            usage::usage_panel_windows(rows)
                .into_iter()
                .map(|w| {
                    let at = w.reset_text.as_deref().and_then(place);
                    (w, at)
                })
                .collect()
        }
    };
    LimitsView {
        wall,
        resets_at,
        windows,
        credits: match reader.program() {
            aterm_phase::Program::Claude => aterm_phase::credits::in_use(rows).is_some(),
            aterm_phase::Program::Codex
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Generic => false,
        },
    }
}

/// The `harness limits` line: `wall=<kind> resets_at=<rfc3339|-> source=grid
/// windows=<name>=<pct>%@<rfc3339|->,… credits=<in-use|-> message=<words>`, or
/// `wall=none …`.
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
    let credits = if view.credits { "in-use" } else { "-" };
    match &view.wall {
        None => format!(
            "wall=none source={} windows={windows} credits={credits} — no wall on the screen",
            Source::Grid.as_str()
        ),
        Some(w) => format!(
            "wall={} resets_at={} source={} windows={windows} credits={credits} message={}",
            w.kind.name(),
            at(view.resets_at),
            Source::Grid.as_str(),
            super::one_line(&w.message, usize::MAX),
        ),
    }
}

/// The `harness limits --json` document, schema 2: `{schema, kind:"limits",
/// source, wall: null | {kind, message, reset, resets_at}, windows: [{name,
/// used_pct, reset, resets_at}], credits: "in-use" | null}`.
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
    o.insert(
        "credits".to_owned(),
        if view.credits {
            Value::from("in-use".to_owned())
        } else {
            Value::Null
        },
    );
    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
}

/// `aterm harness limits` through `ctl`: the tab's `status`, then one screen
/// read, one verdict. `local_offset_s` places a reset the notice names
/// without a zone. `codex_zone` reads the zone a Codex tab's TUI prints in
/// (`Some(None)`: no `TZ`, the machine's; `None`: unread), asked only where
/// its `/status` card is on the screen, whose resets name none
/// ([`limits_view`]).
#[allow(clippy::too_many_arguments)]
fn run_limits_with<C: Ctl>(
    ctl: &mut C,
    env: &Env,
    sid: Option<&str>,
    local_offset_s: i64,
    codex_zone: &dyn Fn(&mut Session<'_, C>) -> Option<Option<String>>,
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
    // The program `status` names as the tab's foreground (`program=`) picks
    // the grammar its screen is read by. A reply that names none leaves it
    // to the screen ([`aterm_phase::identify`]); a session that is not there
    // fails the screen read below.
    let status = ctl
        .call(&[target.as_str(), "status"])
        .ok()
        .map(|r| r.stdout);
    let field = |name: &str| {
        status
            .as_deref()
            .and_then(|s| crate::supervise::screen::status_field(s, name))
    };
    let program = field("program").map(str::to_owned);
    // Where the window's harness placed a Codex's wall reset (`reset_at=`,
    // unix seconds); `-`, or an aterm that prints no such field, places none.
    let placed = field("reset_at").and_then(|at| at.parse::<i64>().ok());
    let mut session = Session::new(ctl, Some(target.clone()));
    match session.read_screen() {
        Ok(screen) => {
            let reader = aterm_phase::identify(program.as_deref(), &screen.rows);
            let zone = match reader.program() {
                aterm_phase::Program::Codex
                    if !usage::codex_status_windows(&screen.rows).is_empty() =>
                {
                    codex_zone(&mut session)
                }
                _ => None,
            };
            let codex_offset_at = |t: i64| {
                zone.as_ref()
                    .and_then(|tz| super::upgrade_drive::codex_zone_offset_at(tz.as_deref(), t))
            };
            let view = limits_view_placed(
                reader,
                &screen.rows,
                env.now,
                local_offset_s,
                &limit::zone_offset_at,
                &codex_offset_at,
                placed,
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
    // A Codex's card is in its TUI's own zone: the `TZ` of the exec
    // environment of the tab's foreground process, bound to the tab
    // (`Session::worker_env`), as its upgrade reads a Codex's zone.
    run_limits_with(
        &mut ctl,
        env,
        sid,
        limit::tz_offset_s(),
        &|session| {
            let worker = session.worker_env().ok()?;
            Some(worker.var("TZ").map(str::to_owned))
        },
        json,
        out,
        err,
    )
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
/// this working directory and its worktree, `$HOME`'s checkouts and their
/// worktrees, Claude Code's temp root and the cells cache.
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
    /// their worktrees, `$HOME`'s checkouts, the agents' `CARGO_TARGET_DIR`,
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

/// What keeps the window's host from taking an agent's upgrade steps
/// ([`window_off`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowOff {
    /// `[harness] enabled`: no session is supervised.
    Harness,
    /// `[harness.<agent>] enabled`: that agent's sessions are not.
    Agent(aterm_phase::Program),
    /// `[harness] upgrade`: the window takes no upgrade step.
    Upgrade,
}

impl WindowOff {
    /// The switch as aterm.toml spells it.
    fn switch(self) -> String {
        match self {
            WindowOff::Harness => "[harness] enabled".to_string(),
            WindowOff::Agent(p) => format!("[harness.{}] enabled", p.name()),
            WindowOff::Upgrade => "[harness] upgrade".to_string(),
        }
    }

    /// Whose upgrades it keeps the window from: every agent's, or one
    /// agent's.
    fn upgrades(self) -> String {
        match self {
            WindowOff::Harness | WindowOff::Upgrade => "these upgrades".to_string(),
            WindowOff::Agent(p) => format!("{}'s upgrades", p.product()),
        }
    }
}

/// The switch that keeps the window's host from `agent`'s upgrades — the
/// master switch, then the agent's own ([`SupervisorConfig::on_for`]), then
/// `[harness] upgrade` — or `None` while it takes them.
fn window_off(policy: &SupervisorConfig, agent: super::upgrade::Agent) -> Option<WindowOff> {
    let program = agent.program();
    if !policy.on_for(program) {
        Some(if policy.enabled {
            WindowOff::Agent(program)
        } else {
            WindowOff::Harness
        })
    } else if !policy.upgrade {
        Some(WindowOff::Upgrade)
    } else {
        None
    }
}

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
        // Reads the list, or writes it: no line is typed.
        usage_tap: None,
        instances: crate::harness::upgrade_drive::Instances::Machine,
        agents_off: Vec::new(),
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
        // `[harness] usage_tap`, under the master switch: a relaunch line
        // this pass types carries the tap where its gate holds — on the
        // login of the machine's one account too.
        usage_tap: (policy.enabled && policy.usage_tap)
            .then(super::tap::stable_exe)
            .flatten()
            .map(|exe| super::tap::Armed {
                exe,
                lone_login: accounts::lone_login_at(
                    env.accounts_roster().as_deref(),
                    &env.accounts_store(),
                    env.now,
                ),
            }),
        instances: crate::harness::upgrade_drive::Instances::Machine,
        // `[harness.<agent>] enabled = false`: the pass leaves that agent's
        // sessions alone, as the window's host does.
        agents_off: policy.off.clone(),
    })
}

/// `aterm harness upgrade [<sid>] --status` and `upgrade <sid>
/// --now|--defer|--skip` ([`OwnerAsk`]): the recorded upgrades read, or the
/// owner's word written ([`super::upgrade_drive::ask`]). Nothing is swept,
/// typed or signalled. A word the busy lock kept out exits 75, like a step
/// that could not run; any other refusal exits 1. With `[harness] enabled`,
/// the tab's agent's own `[harness.<agent>] enabled` ([`window_off`]) or
/// `upgrade` off the word is still written (it is the owner's) and stderr
/// says what it waits on: with an `enabled` off nothing moves until it is back
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
            // window's host stands down — for every agent, or for the one
            // whose own switch is off — so a record reads `unwatched` by the
            // owner's own choice, said once per switch, beside the rows.
            if let Some(path) = env.config.as_deref() {
                let mut said: Vec<WindowOff> = Vec::new();
                for off in rows
                    .iter()
                    .filter(|r| r.stall(now).as_deref() == Some("unwatched"))
                    .filter_map(|r| window_off(&policy, r.agent))
                {
                    if said.contains(&off) {
                        continue;
                    }
                    said.push(off);
                    let _ = writeln!(
                        err,
                        "aterm harness: `{}` reads off in {}, so no window watches {}: \
                         `unwatched` here is that switch, not a fault",
                        off.switch(),
                        path.display(),
                        off.upgrades()
                    );
                }
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
            // The switch that keeps the window off this row's agent: the word
            // stands (it is the owner's) and waits on it.
            if let Some((path, off)) = env.config.as_deref().zip(window_off(&policy, row.agent)) {
                let _ = match off {
                    WindowOff::Upgrade => writeln!(
                        err,
                        "aterm harness: the word is recorded, but `[harness] upgrade` reads off \
                         in {}, so it takes effect at the next `aterm harness upgrade {sid}`",
                        path.display()
                    ),
                    WindowOff::Harness => writeln!(
                        err,
                        "aterm harness: the word is recorded, but `[harness] enabled` reads off \
                         in {} (Settings: search \"harness\"): nothing moves until it is back on",
                        path.display()
                    ),
                    // A hand-run sweep leaves that agent's sessions alone too.
                    WindowOff::Agent(_) => writeln!(
                        err,
                        "aterm harness: the word is recorded, but `{}` reads off in {} \
                         (Settings ▸ Harness): nothing moves until it is back on",
                        off.switch(),
                        path.display()
                    ),
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
/// An agent whose own `[harness.<agent>] enabled` reads off is left out of
/// the sweep ([`super::upgrade_drive::Opts::agents_off`]), said on stderr;
/// the other agents' sessions are swept as ever.
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
            // The master switch strands every agent's restart, those of an
            // agent switched off by its own table too.
            stranded(&super::upgrade_drive::in_flight(
                &super::upgrade_drive::Opts {
                    agents_off: Vec::new(),
                    ..opts.clone()
                }
            )),
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
    if let Some(path) = env.config.as_deref() {
        for &program in &opts.agents_off {
            let _ = writeln!(
                err,
                "aterm harness: `{}` reads off in {} (Settings ▸ Harness): {}'s sessions are left \
                 as they are",
                WindowOff::Agent(program).switch(),
                path.display(),
                program.product()
            );
        }
        // The restarts an earlier sweep began for an agent switched off now
        // stay stopped until its switch is on again: named, as the master
        // switch's refusal names them, since no lane this sweep runs carries
        // them — unless that refusal (a dry run under the master switch off)
        // has named every one already.
        if policy.enabled && !opts.agents_off.is_empty() {
            let ours = super::upgrade_drive::in_flight(opts);
            let every = super::upgrade_drive::in_flight(&super::upgrade_drive::Opts {
                agents_off: Vec::new(),
                ..opts.clone()
            });
            let left: Vec<String> = every.into_iter().filter(|s| !ours.contains(s)).collect();
            if let Some(words) = stranded(&left).strip_prefix("; ") {
                let _ = writeln!(err, "aterm harness: {words}");
            }
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

/// How long one `claude auth status --json` may run (the check after an
/// `accounts login` flow that exits 0, [`auth_status_of`]). A bound, not a
/// budget: MEASURED 2026-10-01 on 2.1.286, the command answered in under a
/// second, signed in or not — and a `claude` that is not going to answer
/// (one was measured hanging on `--version` too) is stopped here and said as
/// `could not run`.
pub const AUTH_CHECK_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// The most of that command's stdout that is read. The document is a few
/// hundred bytes; one cut at this cap does not parse and is `could not run`.
pub const MAX_AUTH_STDOUT: usize = 64 * 1024;

/// What `accounts login` needs of the machine beyond files: its two `claude`
/// runs (the flow, and the check after a flow that exits 0), whether stdin
/// is a terminal, and the clock once the flow has ended. Injected, so the
/// verb's test runs no `claude`; [`LiveAccounts`] is the shipped one.
pub trait AccountsHost {
    /// Whether stdin is a terminal. Only then is the login flow run: it
    /// opens a browser and waits for a person.
    fn stdin_is_terminal(&self) -> bool;

    /// Run `claude auth status --json` with [`accounts::LOGIN_ENV`] set to
    /// `login` — a row's `dir` verbatim, or the empty string for this
    /// machine's own login — bounded, and hand back its exit and its stdout.
    /// Asked ONLY of a login a flow that exited 0 has just made
    /// ([`auth_status_of`] says why).
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

    /// A RENEWAL'S SIGN-IN ([`run_accounts_renew`]): `claude auth login
    /// --claudeai --email <email>` on this terminal, WAITED FOR, with
    /// [`accounts::HOME_ENV`] set to `home` — the new staging directory —
    /// and [`accounts::LOGIN_ENV`] removed, so the new login is that
    /// directory's own keychain item and no live session's ([`renew_command`]).
    /// `browser` says what opens the page. Answers the flow's exit code.
    ///
    /// # Errors
    ///
    /// Why `claude` could not be started.
    fn renew_flow(&self, home: &str, email: &str, browser: &RenewBrowser) -> Result<i32, String> {
        let _ = (home, email, browser);
        Err("this host renews nothing".to_string())
    }

    /// THE CHECK AFTER A RENEWAL'S FLOW EXITED 0: `claude auth status --json`
    /// IN THE STAGING HOME — [`accounts::HOME_ENV`] set to `home`,
    /// [`accounts::LOGIN_ENV`] removed — bounded, its exit and stdout handed
    /// back. In that home because the address it answers is that home's own
    /// record of the sign-in (READ in the 2.1.300 bundle: `email` is the
    /// home's `.claude.json` `oauthAccount`); asked under the shared home it
    /// would answer the shared home's address. A token minted seconds ago
    /// has nothing to refresh, so this run cannot spend it.
    ///
    /// THE STDOUT IS NOT TO BE SHOWN OR LOGGED: its one reader is
    /// [`status_facts`], which compares the address in memory and drops it.
    ///
    /// # Errors
    ///
    /// Why it did not run, never any of its output.
    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        let _ = home;
        Err("this host checks nothing".to_string())
    }

    /// `ssh -t <host> <aterm> harness accounts renew --due --here` (`argv`
    /// after the host), its stdio this process's, WAITED FOR: the remote
    /// machine's own renewal, run in one sitting from here. Answers its exit
    /// code.
    ///
    /// # Errors
    ///
    /// Why `ssh` could not be started.
    fn ssh(&self, host: &str, argv: &[String]) -> Result<i32, String> {
        let _ = (host, argv);
        Err("this host runs no ssh".to_string())
    }

    /// Whether this command runs over SSH (`SSH_CONNECTION` set): the
    /// browser is on the machine the owner is at, so nothing is opened here
    /// and Claude Code's own paste-code flow is the one used
    /// ([`RenewBrowser::Remote`]).
    fn under_ssh(&self) -> bool {
        false
    }

    /// This program's own executable, for the opener's argv0 link
    /// ([`open_url::OPENER`](super::open_url::OPENER)). `None`: no profile
    /// can be opened, and the system's default browser opens the page.
    fn current_exe(&self) -> Option<PathBuf> {
        None
    }
}

/// What opens a renewal's sign-in page ([`AccountsHost::renew_flow`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenewBrowser {
    /// Claude Code's own opener: the system's default browser.
    Default,
    /// The account's own browser profile, through aterm's argv0 opener
    /// (`BROWSER=<opener>`, the profile's name in
    /// [`super::open_url::PROFILE_ENV`]).
    Profile {
        /// The opener's link ([`super::open_url::OPENER`]).
        opener: PathBuf,
        /// The profile's display name.
        profile: String,
    },
    /// Over SSH: NOTHING OPENS ON THIS MACHINE. `BROWSER` is aterm's opener
    /// with no profile named, which opens nothing and exits 1 (`None`, no
    /// opener: `BROWSER` removed). MEASURED on 2.1.300 in the lab
    /// (2026-10-15): `auth login --claudeai --email` prints its paste-code
    /// URL (`code=true`, the `platform.claude.com/oauth/code/callback`
    /// redirect, the `login_hint`) and `Paste code here if prompted >`
    /// whatever `BROWSER` says, and calls its opener under
    /// `SSH_CONNECTION` too — with `BROWSER` unset that is `open <url>`, a
    /// browser on THIS machine's screen. The owner opens the printed URL on
    /// the machine they are at and pastes the code at Claude Code's prompt.
    Remote {
        /// The opener's link ([`super::open_url::OPENER`]), where one could
        /// be made.
        opener: Option<PathBuf>,
    },
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

    fn renew_flow(&self, home: &str, email: &str, browser: &RenewBrowser) -> Result<i32, String> {
        renew_flow_of(std::ffi::OsStr::new("claude"), home, email, browser)
    }

    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        status_in_home_within(
            std::ffi::OsStr::new("claude"),
            home,
            &|name| std::env::var_os(name),
            AUTH_CHECK_BUDGET,
        )
    }

    fn ssh(&self, host: &str, argv: &[String]) -> Result<i32, String> {
        let mut cmd = aterm_uds::spawnfd::command("ssh");
        cmd.arg("-t").arg(host).args(argv);
        terminal_flow(|| cmd.spawn(), |mut child| child.wait(), || {})
            .map(exit_code)
            .map_err(|e| e.to_string())
    }

    fn under_ssh(&self) -> bool {
        std::env::var_os("SSH_CONNECTION").is_some_and(|v| !v.is_empty())
    }

    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }
}

/// `<program> auth login --claudeai --email <email>` as a RENEWAL runs it
/// ([`AccountsHost::renew_flow`]): the login made under the staging home
/// ALONE ([`accounts::HOME_ENV`] = `home`), [`accounts::LOGIN_ENV`] removed
/// — set, it would name the keychain item instead, and the new login would
/// land in a live session's — and the page opened as `browser` says: the
/// account's profile through the argv0 opener, Claude Code's default, or,
/// over SSH, nothing at all (the opener with no profile, which opens
/// nothing: the paste-code flow).
fn renew_command(
    program: &std::ffi::OsStr,
    home: &str,
    email: &str,
    browser: &RenewBrowser,
) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::Command::new(program);
    cmd.args(["auth", "login", "--claudeai", "--email", email])
        .env_remove(accounts::LOGIN_ENV)
        .env(accounts::HOME_ENV, home)
        .env_remove(super::open_url::PROFILE_ENV);
    match browser {
        RenewBrowser::Default => {}
        RenewBrowser::Profile { opener, profile } => {
            cmd.env("BROWSER", opener)
                .env(super::open_url::PROFILE_ENV, profile);
        }
        RenewBrowser::Remote {
            opener: Some(opener),
        } => {
            cmd.env("BROWSER", opener);
        }
        RenewBrowser::Remote { opener: None } => {
            cmd.env_remove("BROWSER");
        }
    }
    cmd
}

/// ONE RUN OF A RENEWAL'S SIGN-IN ([`renew_command`]) on this terminal,
/// WAITED FOR, as [`login_flow_of`] runs the plain one: a child with this
/// process's stdio, this process standing aside from the terminal's Ctrl-C
/// ([`ignoring_interrupts`]) while the flow decides what the key means.
///
/// # Errors
///
/// Why the program could not be started.
fn renew_flow_of(
    program: &std::ffi::OsStr,
    home: &str,
    email: &str,
    browser: &RenewBrowser,
) -> Result<i32, String> {
    terminal_flow(
        || renew_command(program, home, email, browser).spawn(),
        |mut child| child.wait(),
        || {},
    )
    .map(exit_code)
    .map_err(|e| e.to_string())
}

/// `<program> auth status --json` IN THE STAGING HOME `home`
/// ([`AccountsHost::status_in_home`]), bounded by `budget`: the bounded
/// runner's environment (PATH, HOME, TMPDIR), [`accounts::HOME_ENV`] set to
/// `home`, and the variables [`auth_check_env`] hands through.
/// [`accounts::LOGIN_ENV`] is not among them: the login is the home's own
/// item.
///
/// # Errors
///
/// As [`auth_status_of`].
fn status_in_home_within(
    program: &std::ffi::OsStr,
    home: &str,
    var: &dyn Fn(&str) -> Option<OsString>,
    budget: std::time::Duration,
) -> Result<Captured, String> {
    let pass = auth_check_env(var)?;
    let mut cmd = aterm_uds::spawnfd::Command::new(program);
    cmd.args(["auth", "status", "--json"])
        .env(accounts::HOME_ENV, home);
    cmd.envs(pass);
    super::align::capture_bounded(cmd, budget, MAX_AUTH_STDOUT)
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
/// WAITED FOR. It is a child and not a replacement of this process, so its
/// end can be recorded; its stdio is this process's, so the flow reads the
/// keyboard. A Ctrl-C the terminal delivers as a signal reaches the whole
/// foreground group, and this process STANDS ASIDE from it while it waits
/// ([`ignoring_interrupts`]): the flow decides what the key means, and an end
/// it caused (130) is answered like any other failed exit.
///
/// # Errors
///
/// Why the program could not be started.
fn login_flow_of(program: &std::ffi::OsStr, dir: Option<&str>) -> Result<i32, String> {
    terminal_flow(
        || login_command(program, dir).spawn(),
        |mut child| child.wait(),
        || {},
    )
    .map(exit_code)
    .map_err(|e| e.to_string())
}

/// ONE RUN OF `cmd` ON THIS TERMINAL, waited for exactly as [`login_flow_of`] waits:
/// the AI CLIs' sign-in flows (`crate::ai::login`). Answers [`exit_code`].
///
/// # Errors
///
/// Why the program could not be started.
pub(crate) fn terminal_run(mut cmd: aterm_uds::spawnfd::Command) -> Result<i32, String> {
    terminal_flow(|| cmd.spawn(), |mut child| child.wait(), || {})
        .map(exit_code)
        .map_err(|e| e.to_string())
}

/// Process-wide dispositions admit one foreground flow at a time. Admission
/// starts before spawn: another flow's SIG_IGN must not reach a new child.
#[cfg(unix)]
static TERMINAL_FLOWS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One foreground flow: admit its spawn, then stand aside while it waits.
/// `contended` observes admission contention without changing the operation.
fn terminal_flow<T, R, E>(
    spawn: impl FnOnce() -> Result<T, E>,
    wait: impl FnOnce(T) -> Result<R, E>,
    contended: impl FnOnce(),
) -> Result<R, E> {
    #[cfg(unix)]
    let _admission = match TERMINAL_FLOWS.try_lock() {
        Ok(admission) => admission,
        Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            contended();
            TERMINAL_FLOWS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }
    };
    #[cfg(not(unix))]
    let _ = contended;
    let child = spawn()?;
    ignoring_interrupts(|| wait(child))
}

/// Run `wait` with this process IGNORING SIGINT AND SIGQUIT — the rule
/// `system(3)` keeps, and `atpkg::platform::wait_standing_aside`'s for a
/// `std` child. Called AFTER the spawn: an ignored signal is inherited
/// across `exec`, and the child must meet the key with its own disposition.
/// Each disposition is put back whole before this returns or unwinds. The dispositions
/// are the whole process's: its callers are the waits of `aterm harness
/// accounts login` and `renew` ([`login_flow_of`], [`renew_flow_of`], and a
/// renewal's `ssh` to another machine) and of `aterm ai`'s sign-ins
/// ([`terminal_run`]).
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
    struct Restore([(libc::c_int, Option<libc::sigaction>); 2]);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (sig, was) in &self.0 {
                if let Some(was) = was {
                    // SAFETY: puts back the whole action this scope replaced.
                    unsafe { libc::sigaction(*sig, was, std::ptr::null_mut()) };
                }
            }
        }
    }
    let _restore = Restore(replaced);
    wait()
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
/// its stdout capped at [`MAX_AUTH_STDOUT`]. `var` reads the caller's
/// environment.
///
/// ITS ONE CALLER IS THE CHECK AFTER AN `accounts login` FLOW THAT EXITED 0
/// ([`run_accounts_login`], through [`LiveAccounts`]: the `claude` PATH
/// resolves). In 2.1.300 the command starts a token refresh and exits before
/// it saves the result, which spends the login's single-use refresh token
/// (anthropics/claude-code#95822; `docs/DESIGN-claude-accounts-v2-2026-10-15.md`
/// §1), so it is never run under an idle login. A token minted seconds ago
/// has nothing to refresh, so this run cannot spend it — and it is the only
/// proof that the new login is filed where sessions under that directory
/// look (that design's §5.3 step 4).
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
    super::align::capture_bounded(cmd, budget, MAX_AUTH_STDOUT)
}

/// Why `claude auth status --json` is not run with neither USER nor LOGNAME
/// in the environment: [`LoginCheck::CouldNotRun`]'s reason, word for word.
pub const AUTH_CHECK_NO_USER: &str = "USER is not set";

/// The variables the check after a sign-in ([`auth_status_of`]) hands
/// through to the `claude` it runs, as `var` reads this process's
/// environment: USER and LOGNAME, and the markers the agents' reroute stub
/// decides by — each as it is, unread.
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

/// What one login answered to the check after an `accounts login` flow that
/// exited 0 ([`login_check`]).
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

/// What a RENEWAL'S CHECK read of `claude auth status --json` in the staging
/// home ([`status_facts`]): four facts and nothing else. The address itself
/// is not among them: it was compared in memory and dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFacts {
    /// `loggedIn: true` with exit 0.
    pub signed_in: bool,
    /// `authMethod` is `claude.ai`: a subscription's login, not a key's.
    pub claude_ai: bool,
    /// The `email` it answers is the row's ([`accounts::email_matches`]).
    pub email_matches: bool,
    /// `subscriptionType`, where it names one (one bounded word).
    pub subscription: Option<String>,
}

impl StatusFacts {
    /// The renewal's check passes: signed in to claude.ai as the row's
    /// account.
    #[must_use]
    pub fn renews(&self) -> bool {
        self.signed_in && self.claude_ai && self.email_matches
    }
}

/// THE FOUR FACTS of one `claude auth status --json` run in a staging home
/// (READ in the 2.1.300 bundle: `loggedIn`, `authMethod`, the home's
/// `oauthAccount` address as `email`, the token's `subscriptionType`;
/// exit 0 only when logged in), `want` the row's address. The document is
/// parsed, those keys are taken and EVERYTHING ELSE — the organization's id
/// and name among it — is dropped unread; the address answered is compared
/// with `want` here and goes no further. `None` for an answer that cannot be
/// read: not UTF-8, not a JSON object, no `loggedIn`.
#[must_use]
pub fn status_facts(got: &Captured, want: &str) -> Option<StatusFacts> {
    let text = std::str::from_utf8(&got.stdout).ok()?;
    let doc: Value = aterm_json::from_str(text).ok()?;
    let doc = doc.as_object()?;
    let logged_in = doc.get("loggedIn")?.as_bool()?;
    Some(StatusFacts {
        signed_in: logged_in && got.code == Some(0),
        claude_ai: doc.get("authMethod").and_then(Value::as_str) == Some("claude.ai"),
        email_matches: doc
            .get("email")
            .and_then(Value::as_str)
            .is_some_and(|answered| accounts::email_matches(want, answered)),
        subscription: doc
            .get("subscriptionType")
            .and_then(Value::as_str)
            .map(|s| super::one_line(s, 32))
            .filter(|s| !s.is_empty()),
    })
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

/// The line `accounts` adds under [`accounts_none_listed`] for this
/// machine's own login, where the usage tap recorded it: each window's
/// figure and reset, and when it was read.
fn accounts_own_reading(state: &accounts::State, now: i64) -> String {
    let Some(sample) = state
        .accounts
        .get(ACCOUNTS_OWN_LABEL)
        .and_then(|account| account.sample.as_ref())
    else {
        return String::new();
    };
    let window = |name: &str, figure: Option<(u32, i64)>| match figure {
        Some((pct, until)) => format!("{name} {pct}% until {}", rfc3339_utc(until)),
        None => format!("{name} ?"),
    };
    format!(
        "this login ({ACCOUNTS_OWN_LABEL}), read by the usage tap at {}: {} \u{b7} {}",
        rfc3339_utc(sample.at),
        window("session", sample.session(now)),
        window("week", sample.week(now)),
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

/// What the record says of an account's usage (accounts v2 S5), as the
/// report's row words it, each item followed by two blanks; empty where it
/// says nothing: `credits: in use since <time>` while usage credits carry its
/// sessions, else `credits: off (<the vendor's reason>)[ until <time>]` where
/// the newest word on them is a limit row's `off`; then `warned: <weekly|session>
/// <n>% (resets <time>)` of Claude Code's warnings that a window fills: the
/// week's figure while it holds, else the session's.
fn usage_words(account: &accounts::Account, now: i64) -> String {
    let mut words = String::new();
    match &account.credits {
        Some(accounts::Credits::InUse(c)) if c.stands(now) => {
            words.push_str(&format!("credits: in use since {}  ", rfc3339_utc(c.since)));
        }
        Some(accounts::Credits::Off(off)) => {
            let why = if off.reason.is_empty() {
                "named no reason"
            } else {
                off.reason.as_str()
            };
            words.push_str(&format!("credits: off ({why})"));
            if let Some(until) = off.until.filter(|&until| now < until) {
                words.push_str(&format!(" until {}", rfc3339_utc(until)));
            }
            words.push_str("  ");
        }
        _ => {}
    }
    let shown = |window: &str, said: &Option<accounts::WindowSaid>, window_s: i64| {
        let said = said.as_ref()?;
        let (pct, _) = said.figure(window_s, now)?;
        Some(match said.reset {
            Some(at) => format!("warned: {window} {pct}% (resets {})  ", rfc3339_utc(at)),
            None => format!("warned: {window} {pct}%  "),
        })
    };
    if let Some(said) =
        shown("weekly", &account.warned_week, accounts::WEEK_WINDOW_S).or_else(|| {
            shown(
                "session",
                &account.warned_session,
                accounts::SESSION_WINDOW_S,
            )
        })
    {
        words.push_str(&said);
    }
    words
}

/// The `accounts` report as text (design §8): the header, one row per
/// account, `next:` with every non-candidate's reason, and
/// [`ACCOUNTS_ROTATION`]. No usage figure is shown (the JSON keeps them). A
/// limit-reset offer on record shows as `offer: N left[, use by <time>]`,
/// and what the record says of the account's usage as [`usage_words`]. With
/// no account listed, the machine's own row is shown where the record holds
/// its usage (`[harness] usage_notes`).
fn accounts_text(
    path: &Path,
    roster: &Roster,
    (state, logins): (&accounts::State, &accounts::Logins),
    enrolled: &BTreeSet<String>,
    now: i64,
) -> String {
    if roster.rows.is_empty() {
        let mut text = accounts_none_listed(path);
        let own = usage_words(&state.account(ACCOUNTS_OWN_LABEL), now);
        if !own.is_empty() {
            text.push_str(&format!(
                "this machine's own account ({ACCOUNTS_OWN_LABEL}): {}\n",
                own.trim_end()
            ));
        }
        return text;
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
        let usage = usage_words(&state.account(&row.label), now);
        text.push_str(&format!(
            "{:width$}  {:9}  {condition}  {}  {offer}{usage}on: {}{}\n",
            row.label,
            row.class.as_str(),
            account_mark(state, &row.label).unwrap_or("-"),
            if on.is_empty() {
                "-".to_string()
            } else {
                on.join(" ")
            },
            login_words(row, state, logins, now),
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
                    "{label}: {} (no login made yet: run `aterm harness accounts {} {label}`)",
                    why.as_str(),
                    roster.row(label).map_or("login", sign_in_verb)
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

/// A RENEWABLE ROW'S LOGIN as the report says it (design v2 §5.2): `
/// login: <state>`, its renew-by date where a forecast has one (five days
/// before the forecast end), and `last renewal: wrong account` where the
/// last one answered another account. Empty for a row with no email: its
/// line is as it was before renewal existed.
fn login_words(
    row: &accounts::Row,
    state: &accounts::State,
    logins: &accounts::Logins,
    now: i64,
) -> String {
    if row.email.is_none() {
        return String::new();
    }
    let binding = logins.get(&row.label);
    let mut words = format!(
        "  login: {}",
        accounts::login_state(row, logins, state, now).as_str()
    );
    if let Some(by) = binding.and_then(accounts::forecast) {
        words.push_str(&format!(
            ", renew by {}",
            rfc3339_utc(by.saturating_sub(accounts::DUE_S))
        ));
    }
    if binding.is_some_and(|b| b.wrong_at.is_some()) {
        words.push_str(", last renewal: wrong account");
    }
    words
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
    (state, logins): (&accounts::State, &accounts::Logins),
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
            // The login's lifecycle: what is on record, never the address.
            let binding = logins.get(&row.label);
            m.insert("renews".to_owned(), Value::from(row.email.is_some()));
            m.insert(
                "login".to_owned(),
                Value::from(accounts::login_state(row, logins, state, now).as_str()),
            );
            let by = binding.and_then(accounts::forecast);
            put_time(&mut m, "login_ends", by);
            put_time(
                &mut m,
                "renew_by",
                by.map(|by| by.saturating_sub(accounts::DUE_S)),
            );
            put_time(&mut m, "renewed_at", binding.and_then(|b| b.renewed_at));
            m.insert(
                "subscription".to_owned(),
                Value::from(binding.and_then(|b| b.subscription.clone())),
            );
            m.insert(
                "last_renewal".to_owned(),
                Value::from(
                    binding
                        .and_then(|b| b.wrong_at)
                        .map(|_| "wrong-account".to_string()),
                ),
            );
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
    // The roster as this machine's logins bind it: a renewed login is the
    // row's, and the one it replaced is no row's.
    let (logins, note) = match store.load_logins(env.now) {
        Ok(read) => read,
        Err(why) => {
            let _ = writeln!(
                err,
                "aterm harness accounts: {why}; account identities are unknown"
            );
            return ExitCode::from(1);
        }
    };
    if let Some(note) = note {
        let _ = writeln!(err, "aterm harness accounts: {note}");
    }
    // No account listed, and the usage tap recorded this machine's own login
    // (`super::tap`; `[harness] usage_tap`): that login's row is shown, its
    // figures with it — the roster's own row, as a reset is judged by.
    let own = roster.rows.is_empty()
        && state
            .accounts
            .get(ACCOUNTS_OWN_LABEL)
            .is_some_and(|account| account.sample.is_some());
    if own && !json {
        let _ = write!(out, "{}", accounts_none_listed(&path));
        let _ = writeln!(out, "{}", accounts_own_reading(&state, env.now));
        return ExitCode::SUCCESS;
    }
    let roster = accounts::bound(&if own { Roster::own() } else { roster }, &logins);
    let enrolled = accounts::enrolled(&roster);
    let read = (&state, &logins);
    let _ = if json {
        writeln!(
            out,
            "{}",
            accounts_json(&path, &roster, read, &enrolled, env.now)
        )
    } else {
        write!(
            out,
            "{}",
            accounts_text(&path, &roster, read, &enrolled, env.now)
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
    asked: &accounts::Row,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let (label, class) = (asked.label.as_str(), asked.class);
    match accounts_add(env, asked) {
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
            let verb = sign_in_verb(asked);
            let _ = writeln!(out, "run: aterm harness accounts {verb} {label}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts add: {why}");
            ExitCode::from(1)
        }
    }
}

/// THE VERB THAT SIGNS `row` IN: `renew` for a row with an email — it signs
/// in to a staging directory, checked before anything uses it, and `login`
/// refuses it — else `login`. Every line this command prints that tells the
/// owner to sign a row in names it so.
fn sign_in_verb(row: &accounts::Row) -> &'static str {
    if row.email.is_some() {
        "renew"
    } else {
        "login"
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

/// WHERE A NEW ROSTER TEXT IS WRITTEN, and the mode it keeps: the file at
/// `path`, or — where it is a SYMLINK (a dotfiles checkout) — the file it
/// points at, written THROUGH the link: the replacement goes beside that
/// file, so the link survives and the file gets the row. Renamed over the
/// link itself the roster would become a regular file the checkout knows
/// nothing of. The file a link points at is its owner's to keep as they made
/// it, so the replacement carries that file's mode; a roster this command
/// makes, or one in the config directory itself, is 0600 (`None`).
///
/// # Errors
///
/// A symlink whose target cannot be resolved.
fn roster_target(path: &Path) -> Result<(PathBuf, Option<u32>), String> {
    let through_link = path.symlink_metadata().is_ok_and(|m| m.is_symlink());
    let write_to = if through_link {
        std::fs::canonicalize(path).map_err(|e| {
            format!(
                "the roster at {} is a symlink whose target cannot be resolved ({e}); nothing \
                 was written",
                path.display()
            )
        })?
    } else {
        path.to_path_buf()
    };
    let keep_mode = through_link.then(|| file_mode(&write_to)).flatten();
    Ok((write_to, keep_mode))
}

/// [`run_accounts_add`]'s work, or the refusal. `asked` is the row asked for,
/// its `dir` none: it is the login directory made here.
fn accounts_add(env: &Env, asked: &accounts::Row) -> Result<Added, String> {
    let label = asked.label.as_str();
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
    // Resolve a dotfiles link before the strict read so a dangling link keeps
    // its actionable refusal, and read the same target this transaction writes.
    let (write_to, keep_mode) = roster_target(&path)?;
    let existing = accounts::roster_text(&write_to)?.unwrap_or_default();
    let roster = Roster::parse(&existing).map_err(|why| {
        format!(
            "the roster at {} is REFUSED: {why}; nothing was written",
            path.display()
        )
    })?;
    if let Some(row) = roster.row(label) {
        return Err(format!(
            "`{label}` is already listed in {}; `aterm harness accounts {} {label}` signs it in",
            path.display(),
            sign_in_verb(row)
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
    let own = accounts::Row {
        label: ACCOUNTS_OWN_LABEL.to_owned(),
        ..accounts::Row::default()
    };
    let asked = accounts::Row {
        dir: Some(dir_text.to_owned()),
        ..asked.clone()
    };
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

/// What the login a flow that exited 0 left in its row's directory answers
/// now ([`AccountsHost::auth_status`], [`login_check`]): asked under the
/// row's `dir` verbatim, or the empty string for the row with none. `made` is
/// whether the directory holds a login after the flow
/// ([`accounts::login_made`]; the row with no `dir` always does): one that
/// holds nothing is not asked at all.
fn ask_login(host: &dyn AccountsHost, dir: Option<&str>, made: bool) -> LoginCheck {
    if !made {
        return LoginCheck::NotEnrolled;
    }
    login_check(host.auth_status(dir.unwrap_or("")))
}

/// Record `auth` of `label`'s login at `now`, in one transaction
/// ([`accounts_transact`]): [`accounts::note_auth`], a ledger `auth` row when
/// the word changed, and the wake marker replaced when the login now works.
fn record_auth(
    env: &Env,
    now: i64,
    label: &str,
    auth: Auth,
    err: &mut dyn Write,
) -> Result<Vec<accounts::Event>, String> {
    accounts_transact(&env.accounts_store(), now, err, |state| {
        Ok(accounts::note_auth(state, label, auth, now))
    })
}

/// `aterm harness accounts login <label>` (design §4, §8): print the line
/// that signs the account in — `CLAUDE_CONFIG_DIR='<dir>' claude auth login`,
/// the vendor's own browser flow in a home of its own, the `dir` VERBATIM
/// from the roster because the login is named by a hash of that string; for
/// the row with no `dir`, this machine's own login, `claude auth login` — and,
/// with stdin a terminal, run it and wait for it ([`AccountsHost::login`]).
///
/// A FLOW THAT EXITS 0 IS NOT YET A LOGIN THAT WORKS: that one new login is
/// then checked ([`ask_login`]), its line printed and its answer recorded
/// ([`record_auth`]). Exit 0 only for `signed in`. That check is the one
/// `claude auth status` this verb runs, and it runs on a token minted seconds
/// ago, which has nothing to refresh ([`auth_status_of`] says why nothing
/// else may). A `claude` that cannot be started is said with nothing run and
/// nothing recorded, exit 1.
///
/// A FLOW THAT FAILS (denied, timed out, Ctrl-C) RUNS NOTHING MORE UNDER
/// ANY LOGIN, exit 1: whatever login the directory holds is not brand new,
/// and the check would spend its refresh token. What it records rests on
/// whether the row's directory held ANY entry BEFORE the flow
/// ([`accounts::login_made`], read before the directory is made) and after
/// it:
///
/// * It held any, or the row has no `dir`: nothing is recorded. A flow that
///   failed is no evidence about a login made before it, one made by hand
///   with nothing on record included; a directory an earlier, unrecorded
///   flow left its start-up files in stays as it was.
/// * It was EMPTY, and now holds the vendor's start-up files (MEASURED on
///   2.1.287: `claude auth status --json` in an empty home
///   ([`accounts::HOME_ENV`]) exited 1 and left `.claude.json`, start-up keys
///   only, its lock and `backups/`; `auth login` starts the same way). Those
///   files enrol the row, and with nothing on record it would read `unseen` —
///   fresh, `next:`, pinnable — with no login behind it, so it is recorded
///   [`Auth::SignedOut`] as it stands, fail-closed, with nothing asked;
///   stderr then says what the record came to.
/// * It was empty and still is: nothing is recorded; the row stays
///   `not-enrolled`.
///
/// With stdin not a terminal the line is printed, nothing is run and the
/// exit is 0. Where no login could be checked after the flow
/// ([`AccountsHost::unaskable`]) the flow is not started, exit 1. A label the
/// roster does not list is exit 1.
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
    // A row with an email signs in only the way it renews: to a staging
    // directory whose login is checked before anything uses it. Run here,
    // the flow would write the live login's own keychain item, which every
    // session on it adopts within seconds — another account's, if the
    // browser was signed in to one.
    if row.email.is_some() {
        let _ = writeln!(
            err,
            "aterm harness accounts login: `{label}` has an email, so it signs in through `aterm \
             harness accounts renew {label}`: a staging directory, checked against that address \
             before any session uses it; nothing was run"
        );
        return ExitCode::from(1);
    }
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
    // Read before the directory is made and before the flow: what a failed
    // flow may record rests on it.
    let had_login = dir.is_some_and(|d| accounts::login_made(Path::new(d)));
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
        "aterm harness accounts login: running; when it exits 0 `{label}`'s new login is checked \
         and recorded"
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
    // Whether a login was made is read now: the flow is what makes it.
    let made = dir.is_none_or(|d| accounts::login_made(Path::new(d)));
    // What is recorded is as new as the flow's end, however long the flow
    // waited for a person: the state is read against the clock of that
    // moment, since one read against the command's start would save the
    // stamps other writers made meanwhile moved back to it. Never behind the
    // command's own clock, should the machine's have been set back.
    let now = host.now().max(env.now);
    if exited != 0 {
        let failed = format!(
            "aterm harness accounts login: `claude auth login` exited {exited}; nothing else was \
             run under `{label}`'s login"
        );
        if dir.is_none() || had_login || !made {
            let left = if dir.is_some() && !made {
                " (its login directory holds nothing: the row stays not-enrolled)"
            } else {
                ""
            };
            let _ = writeln!(err, "{failed} and nothing was recorded{left}");
            return ExitCode::from(1);
        }
        // A FIRST LOGIN THAT FAILED: the flow's start-up files enrol the row.
        // Recorded first, so stderr says only what the record came to.
        let _ = writeln!(out, "{label}: signed out");
        let _ = out.flush();
        let left = "It left Claude Code's start-up files in a login directory that was empty \
                    before it";
        let _ = match record_auth(env, now, label, Auth::SignedOut, err) {
            Ok(_) => writeln!(
                err,
                "{failed}. {left}, so `{label}` is recorded signed out (not unseen) until \
                 `aterm harness accounts login {label}` signs it in"
            ),
            Err(why) => writeln!(
                err,
                "{failed}. {left}, but `{label}` was NOT recorded signed out: {why}"
            ),
        };
        return ExitCode::from(1);
    }
    let answer = ask_login(host, dir, made);
    let _ = writeln!(out, "{label}: {}", answer.words());
    let _ = out.flush();
    match &answer {
        LoginCheck::CouldNotRun(_) => {
            let _ = writeln!(
                err,
                "aterm harness accounts login: `claude auth login` exited 0, but `{label}` could \
                 not be asked whether it is signed in, so nothing was recorded; `aterm harness \
                 accounts login {label}` signs it in and asks again"
            );
        }
        LoginCheck::NotEnrolled => {
            let _ = writeln!(
                err,
                "aterm harness accounts login: `claude auth login` exited 0 and left no login in \
                 `{label}`'s directory, so nothing was asked or recorded"
            );
        }
        LoginCheck::SignedIn | LoginCheck::SignedOut => {}
    }
    if let Some(auth) = answer.auth()
        && let Err(why) = record_auth(env, now, label, auth, err)
    {
        let _ = writeln!(
            err,
            "aterm harness accounts login: the answer above was NOT recorded: {why}"
        );
        return ExitCode::from(1);
    }
    if answer == LoginCheck::SignedIn {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
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
                Some(row) => format!(
                    "sign it in with `aterm harness accounts {} {label}`, in a terminal",
                    sign_in_verb(row)
                ),
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

/// `aterm harness accounts adopt-default <label> --email <e> [--browser <b>]`
/// (design v2 §4.5): THIS MACHINE'S OWN LOGIN as one renewable row — the
/// row with no `dir`, its email and its browser profile — appended to the
/// roster under the store's lock, written as [`run_accounts_add`] writes
/// (through a symlink, re-read before it is written). Nothing is run and no
/// login is touched: the login is the one this machine's sessions run on
/// already, and `accounts renew <label>` is what renews it.
///
/// Refused by name, exit 1, with the roster untouched: a bad label, email
/// or browser (the email never quoted), a label already listed, and a roster
/// that lists this machine's own login already (the row with no `dir`).
fn run_accounts_adopt(
    env: &Env,
    row: &accounts::Row,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let done = (|| -> Result<PathBuf, String> {
        let path = env.accounts_roster().ok_or_else(|| {
            format!(
                "no config directory is known, so there is no {} to write; name one with \
                 `--config <dir>/aterm.toml`",
                accounts::ACCOUNTS_FILE
            )
        })?;
        let store = env.accounts_store();
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
        if roster.row(&row.label).is_some() {
            return Err(format!(
                "`{}` is already listed in {}",
                row.label,
                path.display()
            ));
        }
        if let Some(own) = roster.row_for_login(None) {
            return Err(format!(
                "this machine's own login is listed already, as `{}` (the row with no dir); add \
                 its email to that row by hand",
                own.label
            ));
        }
        let text = accounts::append_rows(&existing, std::slice::from_ref(row))?;
        let (write_to, keep_mode) = roster_target(&path)?;
        super::upgrade_models::write_atomic_mode(&write_to, &text, keep_mode)
            .map_err(|e| format!("{}: {e}; the roster is unchanged", write_to.display()))?;
        Ok(path)
    })();
    match done {
        Ok(path) => {
            let _ = writeln!(
                out,
                "listed `{}` (dedicated): this machine's own login, in {}",
                row.label,
                path.display()
            );
            let _ = writeln!(
                out,
                "renew it with: aterm harness accounts renew {}",
                row.label
            );
            ExitCode::SUCCESS
        }
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts adopt-default: {why}");
            ExitCode::from(1)
        }
    }
}

/// THE MACHINES `renew` RENEWS ON NEXT ([`run_accounts_renew`]): `--on`'s,
/// else under `--due` `[harness] hosts` (read from aterm.toml), else
/// none. Each is `host` or `host:/abs/path/to/aterm`; anything that is not —
/// a host that could be read as an option of `ssh`, a word with a space or a
/// shell character in it, a relative path — refuses the whole run before
/// anything is run. Answers `(host, the aterm to run there)`.
///
/// # Errors
///
/// The first entry that is not a machine, by its words.
fn machines_to_renew(
    env: &Env,
    due: bool,
    on: Option<&[String]>,
) -> Result<Vec<(String, String)>, String> {
    let hosts: Vec<String> = match on {
        Some(on) => on.to_vec(),
        None if due => SupervisorConfig::from_path(env.config.as_deref()).0.hosts,
        None => Vec::new(),
    };
    machine_entries(&hosts)
}

/// `[harness] hosts` entries (`host` or `host:/abs/path/to/aterm`) as `(host, aterm)`,
/// each refused by name unless both halves are plain words: no leading `-` (an `ssh`
/// option), no whitespace, control character or shell metacharacter, and an absolute path
/// for the aterm. The accounts renewal's fan-out and `aterm ai doctor --on` read it.
///
/// # Errors
///
/// The first entry that is not a machine, by name.
pub(crate) fn machine_entries(hosts: &[String]) -> Result<Vec<(String, String)>, String> {
    let word = |w: &str| {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | '/'))
    };
    hosts
        .iter()
        .map(|entry| {
            let (host, aterm) = entry.split_once(':').unwrap_or((entry.as_str(), "aterm"));
            let host_ok = word(host) && !host.starts_with('-') && !host.contains('/');
            let aterm_ok = aterm == "aterm" || (aterm.starts_with('/') && word(aterm));
            if host_ok && aterm_ok {
                Ok((host.to_string(), aterm.to_string()))
            } else {
                Err(format!(
                    "`{}` is not a machine (`host` or `host:/abs/path/to/aterm`)",
                    super::one_line(entry, 80)
                ))
            }
        })
        .collect()
}

/// How one login's renewal ended ([`renew_one`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Renewal {
    /// Its check answered the row's account, and the row is bound to it.
    Renewed,
    /// Its check answered ANOTHER account: refused, nothing live changed.
    WrongAccount,
    /// The flow failed, its login could not be checked, or recording did
    /// not finish. After recording was attempted its home is retained:
    /// publication may already have changed the live binding.
    Failed,
}

/// THE OPENER'S LINK, `<accounts store>/bin/aterm-open-url` → this program
/// ([`AccountsHost::current_exe`]), made again on every renewal that opens
/// a profile or runs over SSH (a temp link, then a rename), so a moved or
/// updated binary is what it names. `None` with no executable known or a
/// link that cannot be made: the system's default browser opens the page
/// (over SSH: `BROWSER` removed).
fn opener_link(store: &accounts::Store, host: &dyn AccountsHost) -> Option<PathBuf> {
    let exe = host.current_exe()?;
    let bin = store.dir.join("bin");
    aterm_types::fs_restricted::ensure_private_dir(&store.dir).ok()?;
    aterm_types::fs_restricted::ensure_private_dir(&bin).ok()?;
    let link = bin.join(super::open_url::OPENER);
    #[cfg(unix)]
    {
        let temp = bin.join(format!(
            ".{}.{}",
            super::open_url::OPENER,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&temp);
        std::os::unix::fs::symlink(&exe, &temp).ok()?;
        if std::fs::rename(&temp, &link).is_err() {
            let _ = std::fs::remove_file(&temp);
            return None;
        }
        Some(link)
    }
    #[cfg(not(unix))]
    {
        let _ = (exe, link);
        None
    }
}

/// A staging directory a renewal is not keeping, removed: the flow failed,
/// or its login answered another account or could not be checked. Its
/// keychain item stays (aterm deletes none, D5): nothing reads it, and the
/// next renewal of the row signs in to this very name again.
fn drop_staging(staging: &Path, err: &mut dyn Write) {
    if let Err(e) = std::fs::remove_dir_all(staging)
        && e.kind() != io::ErrorKind::NotFound
    {
        let _ = writeln!(
            err,
            "aterm harness accounts renew: {} could not be removed ({e}); nothing reads it",
            staging.display()
        );
    }
}

/// WHAT A RENEWAL DOES TO THE SESSIONS ON THE LOGIN IT REPLACED, as `cfg`
/// (this machine's aterm.toml) has the window's supervisor: with `[harness]
/// enabled`, Claude Code's own `[harness.claude] enabled`, `logins`,
/// `relaunch` and `resume_limits` on, each is relaunched onto the new login
/// at its next idle turn end; with any of them off nothing moves one, and the
/// switch is named — the master switch first, which holds every other off,
/// then Claude Code's own, whose sessions the logins are (the window's host
/// turns the accounts and logins off with it).
fn renewed_sessions(cfg: &SupervisorConfig) -> String {
    if cfg.enabled && !cfg.on_for(aterm_phase::Program::Claude) {
        return "`[harness.claude] enabled` is off, so nothing moves a session onto the new login"
            .to_string();
    }
    let off = if !cfg.enabled {
        "enabled"
    } else if !cfg.logins {
        "logins"
    } else if !cfg.relaunch {
        "relaunch"
    } else if !cfg.resume_limits {
        "resume_limits"
    } else {
        return "its sessions move onto the new login at their next idle turn end".to_string();
    };
    format!("`[harness] {off}` is off, so nothing moves a session onto the new login")
}

/// ONE LOGIN'S RENEWAL (design v2 §5.3), `row` read through this machine's
/// bindings and `own_dir` the roster file's own `dir` for it:
///
/// 1. A NEW STAGING DIRECTORY ([`accounts::Store::claim_staging_dir`]) and the
///    vendor's own sign-in into it ([`AccountsHost::renew_flow`]): `--email`
///    the row's address, the page opened by `browser`.
/// 2. A flow that exits non-zero (Ctrl-C included) RUNS NOTHING MORE and
///    records nothing; the staging directory goes.
/// 3. A flow that exits 0: ONE `auth status --json` in that staging home
///    ([`AccountsHost::status_in_home`]), its address compared in memory
///    ([`status_facts`]).
/// 4. The row's account: the row is bound to the staging directory and the
///    login recorded `ok` in one transaction ([`accounts::note_renewed`],
///    the `renewed` row, the wake marker): every session on the old login
///    moves onto the new one at its next idle turn end, where the window's
///    supervisor relaunches ([`renewed_sessions`] says which), and the line
///    that starts Claude Code under it by hand is printed (what a tab
///    nothing relaunches is told to run). Another account: refused,
///    the staging directory goes, `wrong-account` recorded (labels only);
///    nothing live changes. No answer that can be read: the same, unrecorded.
///    If recording the checked login fails, its home stays: the binding may
///    already have been published before a later state, ledger or wake write
///    failed. That error is not a rollback.
///
/// The address is never printed: not by this, not by the flow's argv
/// echoed, not in any refusal.
#[allow(clippy::too_many_arguments)]
fn renew_one(
    env: &Env,
    host: &dyn AccountsHost,
    store: &accounts::Store,
    row: &accounts::Row,
    own_dir: Option<&str>,
    browser: &RenewBrowser,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Renewal {
    let label = row.label.as_str();
    let Some(email) = row.email.as_deref() else {
        return Renewal::Failed;
    };
    let say = |err: &mut dyn Write, what: &str| {
        let _ = writeln!(err, "aterm harness accounts renew: {label}: {what}");
    };
    let staging = match store.claim_staging_dir(label) {
        Ok(staging) => staging,
        Err(e) => {
            say(
                err,
                &format!(
                    "no staging directory could be made under {}: {e}",
                    store.logins_dir().display()
                ),
            );
            return Renewal::Failed;
        }
    };
    let Some(staging_text) = staging.to_str().map(str::to_owned) else {
        drop_staging(&staging, err);
        say(err, "the staging directory's name is not UTF-8");
        return Renewal::Failed;
    };
    let how = match browser {
        RenewBrowser::Default => "it opens in your default browser".to_string(),
        RenewBrowser::Profile { profile, .. } => {
            format!(
                "it opens in browser profile '{}'",
                super::one_line(profile, 64)
            )
        }
        RenewBrowser::Remote { .. } => match row.browser.as_deref() {
            Some(profile) => format!(
                "open the URL it prints in Chrome profile '{}' on the machine you are at, and \
                 paste the code it shows here",
                super::one_line(profile, 64)
            ),
            None => "open the URL it prints on the machine you are at, and paste the code it \
                     shows here"
                .to_string(),
        },
    };
    let _ = writeln!(out, "renewing `{label}`: Claude Code's own sign-in; {how}");
    let _ = out.flush();
    let _ = err.flush();
    let exited = host.renew_flow(&staging_text, email, browser);
    let now = host.now().max(env.now);
    let exited = match exited {
        Ok(code) => code,
        Err(why) => {
            drop_staging(&staging, err);
            say(
                err,
                &format!("`claude` could not be run: {}", super::one_line(&why, 160)),
            );
            return Renewal::Failed;
        }
    };
    if exited != 0 {
        drop_staging(&staging, err);
        say(
            err,
            &format!(
                "`claude auth login` exited {exited}; nothing was recorded, nothing else was run, \
                 and nothing live changed"
            ),
        );
        return Renewal::Failed;
    }
    if !accounts::login_made(&staging) {
        drop_staging(&staging, err);
        say(
            err,
            "the sign-in exited 0 and left no login; nothing was recorded",
        );
        return Renewal::Failed;
    }
    let facts = match host.status_in_home(&staging_text) {
        Ok(got) => status_facts(&got, email).ok_or_else(|| {
            let exit = got
                .code
                .map_or_else(|| "ended by a signal".to_string(), |c| format!("exit {c}"));
            format!("{exit}, with no answer that can be read")
        }),
        Err(why) => Err(super::one_line(&why, 160)),
    };
    let facts = match facts {
        Ok(facts) => facts,
        Err(why) => {
            drop_staging(&staging, err);
            say(
                err,
                &format!("the new login could not be checked ({why}); nothing was recorded"),
            );
            return Renewal::Failed;
        }
    };
    if facts.signed_in && facts.claude_ai && !facts.email_matches {
        drop_staging(&staging, err);
        let _ = writeln!(out, "{label}: REFUSED: wrong account");
        let recorded = accounts_transact_logins(store, now, err, |_, logins| {
            Ok(accounts::note_wrong_account(logins, label, now))
        });
        say(
            err,
            &format!(
                "the browser signed in ANOTHER account than this row's, so nothing live changed{}; \
                 sign in as the row's account (its own browser profile) and run `aterm harness \
                 accounts renew {label}` again",
                if recorded.is_ok() {
                    ""
                } else {
                    " (and the attempt was not recorded)"
                }
            ),
        );
        return Renewal::WrongAccount;
    }
    if !facts.renews() {
        drop_staging(&staging, err);
        let what = if facts.signed_in {
            "a login that is not a Claude subscription's"
        } else {
            "signed out"
        };
        say(
            err,
            &format!("the new login answers {what}; nothing was recorded"),
        );
        return Renewal::Failed;
    }
    let done = accounts_transact_logins(store, now, err, |state, logins| {
        Ok(accounts::note_renewed(
            state,
            logins,
            label,
            own_dir,
            &staging_text,
            facts.subscription.as_deref(),
            now,
        ))
    });
    match done {
        Ok(_) => {
            let by = store
                .load_logins(now)
                .ok()
                .and_then(|(logins, _)| logins.get(label).and_then(accounts::forecast));
            let _ = writeln!(
                out,
                "{label}: renewed; {}{}",
                renewed_sessions(&SupervisorConfig::from_path(env.config.as_deref()).0),
                by.map_or_else(String::new, |by| format!(
                    "; renew by {}",
                    rfc3339_utc(by.saturating_sub(accounts::DUE_S))
                ))
            );
            let _ = writeln!(
                out,
                "start Claude Code under it by hand with: {}={} claude --resume",
                accounts::LOGIN_ENV,
                super::upgrade::quote(super::upgrade::Dialect::Zsh, &staging_text)
            );
            Renewal::Renewed
        }
        Err(why) => {
            // The transaction publishes logins before state, ledger and
            // wake writes. Any later error can leave this home live; even
            // a failed read cannot prove that cleanup is safe.
            say(
                err,
                &format!(
                    "the new login checked out, but recording did not finish: {why}; its home \
                     is kept at {} because the binding or state may already have changed. Run \
                     `aterm harness accounts` to inspect before trying again",
                    staging.display()
                ),
            );
            Renewal::Failed
        }
    }
}

/// [`accounts_transact`] over the state and the logins together
/// ([`accounts::Store::transact_logins`]).
fn accounts_transact_logins(
    store: &accounts::Store,
    now: i64,
    err: &mut dyn Write,
    edit: impl FnOnce(
        &mut accounts::State,
        &mut accounts::Logins,
    ) -> Result<Vec<accounts::Event>, String>,
) -> Result<Vec<accounts::Event>, String> {
    let mut note = None;
    let done = store.transact_logins(now, &mut note, edit);
    if let Some(note) = note {
        let _ = writeln!(err, "aterm harness accounts: {note}");
    }
    done
}

/// `aterm harness accounts renew [<label>…] [--due] [--on <host>,… |
/// --here]` (design v2 §5.3, ruling R-7: batched, ahead of time, every due
/// login on every machine in one sitting): each login named, and under
/// `--due` every login of this machine that is due or expired
/// ([`accounts::renew_due`]), renewed in turn ([`renew_one`]); then, for each
/// machine `--on` names (under `--due`, `[harness] hosts` where `--on`
/// names none; `--here`, none), `ssh -t <host> aterm harness accounts renew
/// --due --here`, its stdio this terminal's — Claude Code's own paste-code
/// flow runs there (over SSH it prints its URL and waits for the code), and
/// aterm reads and relays nothing of it. That run renews its machine's own
/// logins and goes on to no other: one sitting is one machine deep, however
/// the machines' configs name each other (or themselves).
///
/// Every label named must be listed with an email (a row without one renews
/// nothing: `accounts login` signs it in), and every machine must be one
/// ([`machines_to_renew`]): otherwise nothing runs, exit 1. With stdin not a
/// terminal the logins are named and nothing runs, exit 0, as `login` does.
/// Where no login could be asked afterwards whether it is signed in
/// ([`AccountsHost::unaskable`]), no flow starts, exit 1. Nothing due and no
/// machine named: said, exit 0. Otherwise exit 0 only when every renewal
/// and every machine's run succeeded.
fn run_accounts_renew(
    env: &Env,
    host: &dyn AccountsHost,
    (labels, due, on): (&[String], bool, Option<&[String]>),
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let Some((path, file_roster)) = accounts_read_roster(env, err) else {
        return ExitCode::from(1);
    };
    let machines = match machines_to_renew(env, due, on) {
        Ok(machines) => machines,
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts renew: {why}; nothing was run");
            return ExitCode::from(1);
        }
    };
    let store = env.accounts_store();
    let now = env.now;
    let (logins, logins_note) = match store.load_logins(now) {
        Ok(read) => read,
        Err(why) => {
            let _ = writeln!(
                err,
                "aterm harness accounts renew: {why}: {} is left as it is and nothing was run",
                store.logins_file().display()
            );
            return ExitCode::from(1);
        }
    };
    if let Some(note) = logins_note {
        let _ = writeln!(err, "aterm harness accounts: {note}");
    }
    let roster = accounts::bound(&file_roster, &logins);
    let mut chosen: Vec<String> = Vec::new();
    for label in labels {
        match roster.row(label) {
            None => {
                let _ = writeln!(
                    err,
                    "aterm harness accounts renew: `{}` is not in the roster ({}); nothing was run",
                    super::one_line(label, 80),
                    path.display()
                );
                return ExitCode::from(1);
            }
            Some(row) if row.email.is_none() => {
                let _ = writeln!(
                    err,
                    "aterm harness accounts renew: `{label}` has no email, so its renewal could not \
                     be checked; add `email = \"…\"` to its row (or sign it in with `aterm harness \
                     accounts login {label}`); nothing was run"
                );
                return ExitCode::from(1);
            }
            Some(_) if !chosen.contains(label) => chosen.push(label.clone()),
            Some(_) => {}
        }
    }
    if due {
        let (state, _) = match store.load(now) {
            Ok(read) => read,
            Err(why) => {
                let _ = writeln!(
                    err,
                    "aterm harness accounts renew: {why}; due logins could not be determined, \
                     so nothing was run"
                );
                return ExitCode::from(1);
            }
        };
        let enrolled = accounts::enrolled(&roster);
        for label in accounts::renew_due(&roster, &logins, &state, &enrolled, now) {
            if !chosen.contains(&label) {
                chosen.push(label);
            }
        }
    }
    if chosen.is_empty() && machines.is_empty() {
        let _ = writeln!(out, "nothing is due on this machine");
        return ExitCode::SUCCESS;
    }
    if !chosen.is_empty() && !host.stdin_is_terminal() {
        let _ = writeln!(out, "due here: {}", chosen.join(", "));
        let _ = writeln!(
            err,
            "aterm harness accounts renew: not run: stdin is not a terminal (each renewal opens a \
             browser and waits). Run it in a terminal"
        );
        return ExitCode::SUCCESS;
    }
    if !chosen.is_empty()
        && let Some(why) = host.unaskable()
    {
        let _ = writeln!(
            err,
            "aterm harness accounts renew: not run: {why}, so no renewed login could be checked \
             afterwards"
        );
        return ExitCode::from(1);
    }
    let mut ok = true;
    let mut opener: Option<Option<PathBuf>> = None;
    for label in &chosen {
        let Some(row) = roster.row(label) else {
            continue;
        };
        let own_dir = file_roster.row(label).and_then(|r| r.dir.as_deref());
        let browser = if host.under_ssh() {
            RenewBrowser::Remote {
                opener: opener
                    .get_or_insert_with(|| opener_link(&store, host))
                    .clone(),
            }
        } else {
            match row.browser.as_deref() {
                None => RenewBrowser::Default,
                Some(profile) => match opener.get_or_insert_with(|| opener_link(&store, host)) {
                    Some(link) => RenewBrowser::Profile {
                        opener: link.clone(),
                        profile: profile.to_string(),
                    },
                    None => RenewBrowser::Default,
                },
            }
        };
        let renewal = renew_one(env, host, &store, row, own_dir, &browser, out, err);
        ok &= renewal == Renewal::Renewed;
    }
    for (machine, aterm) in &machines {
        let _ = writeln!(out, "renewing on {machine}:");
        let _ = out.flush();
        // `--here`: that machine renews its own logins and goes on to none —
        // its own `hosts` would name this one, or itself.
        let argv = [
            aterm.as_str(),
            "harness",
            "accounts",
            "renew",
            "--due",
            "--here",
        ]
        .map(str::to_string);
        match host.ssh(machine, &argv) {
            Ok(0) => {}
            Ok(code) => {
                ok = false;
                let _ = writeln!(
                    err,
                    "aterm harness accounts renew: on {machine}: exited {code}"
                );
            }
            Err(why) => {
                ok = false;
                let _ = writeln!(
                    err,
                    "aterm harness accounts renew: on {machine}: `ssh` could not be run: {}",
                    super::one_line(&why, 160)
                );
            }
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// THE PROFILE CLAUDE CODE KEEPS FOR THE HOME a raw `CLAUDE_CONFIG_DIR`
/// (`config`, `None` unset) names, for a user whose home is `home`, its
/// directory `dir` where this command could resolve it
/// ([`footer::claude_dir_of`]): [`profile::path_in`]. Where it could not —
/// an empty or relative one, with no `CLAUDE_PID` to read the working
/// directory of — by the raw value alone ([`profile::path_of`], no working
/// directory): an EMPTY one's is `$HOME`'s wherever its records are
/// (`CLAUDE_CONFIG_DIR || homedir()`), a relative one's is not known.
fn claude_profile_of(
    home: Option<&Path>,
    config: Option<&str>,
    dir: Option<&Path>,
) -> Option<PathBuf> {
    let user = home?;
    match dir {
        Some(dir) => Some(profile::path_in(user, config, dir)),
        None => profile::path_of(user, config, None, || None),
    }
}

/// Route one `accounts` form, the machine injected ([`AccountsHost`]) so the
/// tests drive `login` without spawning anything.
pub fn run_accounts(
    form: &AccountsCmd,
    env: &Env,
    host: &dyn AccountsHost,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    match form {
        AccountsCmd::Report { json } => run_accounts_report(env, *json, out, err),
        AccountsCmd::Add {
            label,
            class,
            email,
            browser,
        } => {
            let row = accounts::Row {
                label: label.clone(),
                class: *class,
                email: email.clone(),
                browser: browser.clone(),
                ..accounts::Row::default()
            };
            run_accounts_add(env, &row, out, err)
        }
        AccountsCmd::AdoptDefault {
            label,
            email,
            browser,
        } => {
            let row = accounts::Row {
                label: label.clone(),
                email: Some(email.clone()),
                browser: browser.clone(),
                ..accounts::Row::default()
            };
            run_accounts_adopt(env, &row, out, err)
        }
        AccountsCmd::Renew { labels, due, on } => {
            run_accounts_renew(env, host, (labels, *due, on.as_deref()), out, err)
        }
        AccountsCmd::Login { label } => run_accounts_login(env, host, label, out, err),
        AccountsCmd::Use { label } => run_accounts_use(env, Some(label), out, err),
        AccountsCmd::Auto => run_accounts_use(env, None, out, err),
        AccountsCmd::Stats { json } => run_accounts_stats(env, *json, out),
    }
}

/// `aterm harness accounts stats [--json]` (design
/// `docs/DESIGN-account-stats-2026-10-15.md` §7): what the Claude Code
/// profile of the home this command's environment names says of its
/// account — the plan and its tier, the usage-credits word, and how the plan
/// bills Fable (a hint) — read on demand from the file Claude Code reads
/// for it ([`profile::effective`]; where the home's records directory is
/// unknown — an empty `CLAUDE_CONFIG_DIR` with no `CLAUDE_PID` — no
/// `.config.json` is looked for) through the allowlist reader
/// ([`profile::read`]), which keeps no field off its list, and labelled by
/// [`stats_label`]. A home that could not be resolved reads nothing
/// ([`profile::NoProfile::NoHome`]). Labels and keys only: no email, id,
/// path or vendor flag name is printed. Writes nothing and takes no lock;
/// exit 0, a figure that could not be read naming why.
fn run_accounts_stats(env: &Env, json: bool, out: &mut dyn Write) -> ExitCode {
    let read = env
        .claude_profile
        .as_deref()
        .map(|own| profile::effective(own, env.claude_dir().as_deref(), env.claude_custom_oauth))
        .map_or(Err(profile::NoProfile::NoHome), |file| profile::read(&file));
    let label = stats_label(env, read.as_ref().ok().and_then(|p| p.key));
    let _ = if json {
        writeln!(out, "{}", stats_json(&label, &read, env.now))
    } else {
        write!(out, "{}", stats_text(&label, &read, env.now))
    };
    ExitCode::SUCCESS
}

/// THE LABEL `accounts stats` NAMES THE HOME'S LOGIN BY (design §3.1), the
/// first that applies: the roster row's label where the home is enrolled —
/// for the default home the row with no `dir`, for any other the row whose
/// `dir` names the directory `CLAUDE_CONFIG_DIR` resolved to, once a login
/// was made there ([`accounts::enrolled`]); else `main` for the default
/// home; else `home-<key6>` from the profile's `key` (`home` with none). A
/// roster that is missing or refused lists no row.
fn stats_label(env: &Env, key: Option<profile::AccountKey>) -> String {
    let roster = env
        .accounts_roster()
        .and_then(|path| Roster::load(&path).ok())
        .unwrap_or_default();
    // The default home is the one whose profile is `$HOME/.claude.json`. An
    // EMPTY `CLAUDE_CONFIG_DIR` is that home too (the profile's
    // `CLAUDE_CONFIG_DIR || homedir()` and the keychain item's
    // `!CLAUDE_CONFIG_DIR` read it as unset: READ in the 2.1.301 bundle); a
    // set one, `~/.claude` itself included, is a home of its own.
    let default_profile = env.home.as_ref().map(|home| home.join(".claude.json"));
    if env.claude_profile.is_some() && env.claude_profile == default_profile {
        return roster
            .row_for_login(None)
            .map_or(ACCOUNTS_OWN_LABEL, |row| row.label.as_str())
            .to_owned();
    }
    let row = env.claude_dir.as_deref().and_then(|dir| {
        roster
            .rows
            .iter()
            .find(|row| row.dir.as_deref().is_some_and(|d| Path::new(d) == dir))
    });
    match (row, key) {
        (Some(row), _) if accounts::enrolled(&roster).contains(&row.label) => row.label.clone(),
        (_, Some(key)) => format!("home-{}", key.short()),
        (_, None) => "home".to_owned(),
    }
}

/// `accounts stats --json`: schema 1 of design §7.2, the fields this
/// machine's profile fills, each `null` where it says nothing.
fn stats_json(
    label: &str,
    read: &Result<profile::Profile, profile::NoProfile>,
    now: i64,
) -> String {
    let word = |w: &Option<profile::Word>| Value::from(w.as_ref().map(|w| w.as_str().to_owned()));
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("kind".to_owned(), Value::from("account-stats".to_owned()));
    put_time(&mut o, "as_of", Some(now));
    o.insert("complete".to_owned(), Value::from(true));
    let mut account = Map::new();
    account.insert("agent".to_owned(), Value::from("claude".to_owned()));
    account.insert("label".to_owned(), Value::from(label.to_owned()));
    let mut missing = Vec::new();
    match read {
        Ok(p) => {
            account.insert("key".to_owned(), Value::from(p.key.map(|k| k.to_string())));
            let mut plan = Map::new();
            plan.insert(
                "word".to_owned(),
                Value::from(p.plan.word.map(|w| w.word().to_owned())),
            );
            plan.insert("tier".to_owned(), word(&p.plan.tier));
            plan.insert("seat".to_owned(), word(&p.plan.seat));
            plan.insert("billing".to_owned(), word(&p.plan.billing));
            plan.insert("display".to_owned(), word(&p.plan.display));
            put_time(&mut plan, "as_of", p.plan.fetched_at);
            plan.insert("source".to_owned(), Value::from("profile".to_owned()));
            account.insert("plan".to_owned(), Value::Object(plan));
            let mut fable = Map::new();
            fable.insert("plan".to_owned(), Value::from(p.fable.word().to_owned()));
            put_time(&mut fable, "ends_at", p.fable.ends_at());
            fable.insert("hint".to_owned(), Value::from(true));
            account.insert("fable".to_owned(), Value::Object(fable));
            let mut credits = Map::new();
            credits.insert(
                "state".to_owned(),
                Value::from(p.credits.state().map(str::to_owned)),
            );
            credits.insert("reason".to_owned(), word(&p.credits.reason().cloned()));
            credits.insert("source".to_owned(), Value::from("profile".to_owned()));
            account.insert("credits".to_owned(), Value::Object(credits));
        }
        Err(why) => {
            account.insert("key".to_owned(), Value::Null);
            for figure in ["plan", "fable", "credits"] {
                account.insert(figure.to_owned(), Value::Null);
            }
            let mut reason = Map::new();
            reason.insert("reason".to_owned(), Value::from(why.word().to_owned()));
            account.insert("missing".to_owned(), Value::Object(reason));
            let mut what = Map::new();
            what.insert("what".to_owned(), Value::from("profile".to_owned()));
            what.insert("reason".to_owned(), Value::from(why.word().to_owned()));
            missing.push(Value::Object(what));
        }
    }
    o.insert("missing".to_owned(), Value::Array(missing));
    o.insert(
        "accounts".to_owned(),
        Value::Array(vec![Value::Object(account)]),
    );
    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
}

/// `accounts stats`: the account's line — label, agent, plan, credits —
/// then its Fable line.
fn stats_text(
    label: &str,
    read: &Result<profile::Profile, profile::NoProfile>,
    now: i64,
) -> String {
    let mut text = format!(
        "accounts at {} \u{b7} this machine only \u{b7} from Claude Code's profile\n",
        rfc3339_utc(now)
    );
    let p = match read {
        Ok(p) => p,
        Err(why) => {
            text.push_str(&format!(
                "{label} \u{b7} claude \u{b7} plan \u{2014} ({})\n",
                why.word()
            ));
            return text;
        }
    };
    let plan = p.plan.title().unwrap_or_else(|| "plan \u{2014}".to_owned());
    let credits = match (&p.credits, p.credits.reason()) {
        (profile::Credits::Off(_), Some(reason)) => format!("credits off ({})", reason.as_str()),
        (credits, _) => format!("credits {}", credits.state().unwrap_or("\u{2014}")),
    };
    let fable = match &p.fable {
        profile::FablePlan::Included => "Fable included".to_owned(),
        profile::FablePlan::CreditsOnly => "credits only".to_owned(),
        profile::FablePlan::LimitsEnd(Some(at)) => {
            format!(
                "limits end {}",
                rfc3339_utc(*at).get(..10).unwrap_or_default()
            )
        }
        profile::FablePlan::LimitsEnd(None) => "limits end (date unread)".to_owned(),
        profile::FablePlan::Unknown => "unknown".to_owned(),
    };
    text.push_str(&format!(
        "{label} \u{b7} claude \u{b7} {plan} \u{b7} {credits}\n  Fable  plan: {fable}\n"
    ));
    text
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
        Cmd::Tap => run_tap(out),
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

/// `aterm harness tap` on this process's stdin and environment
/// ([`super::tap::run`], [`super::tap::TapVars::from_process`]), into the
/// readings' directory under this process's state root
/// ([`super::tap::rl_dir`]). Always exit 0.
fn run_tap(out: &mut dyn Write) -> ExitCode {
    use std::io::IsTerminal as _;
    let stdin = io::stdin();
    let tty = stdin.is_terminal();
    super::tap::run(
        &mut stdin.lock(),
        tty,
        &super::tap::TapVars::from_process(),
        super::tap::rl_dir().as_deref(),
        limit::unix_now(),
        out,
    )
}

/// The front door's entry point (`aterm harness …`).
///
/// The retired verbs answer first, before any parsing and before the
/// environment is read, so no argv or environment an old install runs them
/// with can turn their answer into an error: `hook` and `statusline` drain
/// stdin to its end and exit 0 printing nothing; `install` and `uninstall`
/// refuse with [`RETIRED`], exit 2. See [`answer_retired`]. A bare `tap`
/// answers there too ([`run_tap`]): Claude Code runs it after every reply,
/// and nothing the environment says may turn its line into an error.
pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    use std::io::IsTerminal as _;
    let args: Vec<String> = argv
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args == ["tap"] {
        return run_tap(&mut io::stdout().lock());
    }
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
