// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness`: the read views of the Claude Code harness (`usage`, `limits`, `disk`, `ledger`;
//! design `docs/DESIGN-aterm-wrapper-2026-09-17.md` §5.2, §5.5, §5.8), the `accounts` roster and
//! its report (`docs/DESIGN-claude-accounts-2026-10-01.md` §4, §8), and `upgrade`, the one verb
//! that acts on a live session ([`super::upgrade_drive`]).

use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_json::{Map, Value};
use aterm_phase::ResetKind;
use aterm_phase::anchors::{Anchor, newer_than};

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

/// The harness name, which is also the state directory's name (design §1.3).
pub const HARNESS: &str = "claude-harness";

/// How many rows `ledger` prints when the caller names no count.
pub const LEDGER_DEFAULT_ROWS: usize = 20;

/// The most rows one read of a ledger holds: the tail streams through a ring of this size, so a large ledger is never held whole.
pub const READ_MAX_ROWS: usize = 4096;

/// The disk journal's file name under the harness state directory.
pub const DISK_LEDGER: &str = "disk.jsonl";

/// The size past which `disk` cuts its journal back before appending: every run appends a row, report-only runs included.
pub const DISK_LEDGER_MAX_BYTES: u64 = 1024 * 1024;

/// How many of the newest rows survive a cut ([`bound_ledger`]).
pub const DISK_LEDGER_KEEP_ROWS: usize = 1024;

/// Where aterm.toml lives ([`aterm_types::dirs::aterm_config_path`], the path the window's Settings page writes), so `disk` reads the window's `[disk]`.
#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    aterm_types::dirs::aterm_config_path()
}

/// The usage text: the verbs and the flags.
pub const USAGE: &str = "\
aterm harness — read views of the agent harness, its accounts, and live upgrades.
A window's supervisor acts on Claude Code, agy, Muse Code and Gemini CLI tabs by default.
Vendor `codex` is shown, not supervised; `codex-alab` supervises itself.

USAGE:
    aterm harness usage [--json]                 this session's tokens per model and usage-tap windows
    aterm harness usage --agy [--json]           agy's quota: each model group's weekly and 5-hour limits
    aterm harness agy [quota] [--refresh] [--json]
                                                 agy's quota per pool, kept and tracked: standing, burn, refill
    aterm harness agy pick [<model>] [--json]    the model an agy launch runs after quota rotation
    aterm harness limits [@<sid>] [--json]       wall and painted /usage windows on the session's screen
    aterm harness disk [<build-dir> ...] [--apply <class>] [--json]
                                                 report or reclaim stale build directories and caches
    aterm harness ledger [@<sid>] [<n>] [--json] supervisor approval ledger for a session
    aterm harness ledger disk [<n>] [--json]     disk cleanup journal
    aterm harness ledger upgrade [<n>] [--json]  live upgrade ledger
    aterm harness ledger accounts [<n>] [--json] accounts ledger (logins, pins, limits, rotations)
    aterm harness accounts [--json]              Claude Code accounts on this machine and rotation order
    aterm harness accounts stats [--json]        5h/week/Fable windows, plan, credits, \u{2248}$, and active sessions
    aterm harness accounts add <label> [--shared] [--email <e>] [--browser <b>]
                                                 create login directory and append roster row
    aterm harness accounts adopt-default <label> --email <e> [--browser <b>]
                                                 enroll this machine's default login as a renewable row
    aterm harness accounts login <label>         run Claude Code's sign-in flow and record auth status
    aterm harness accounts renew [--due | <label>...] [--on <host>,... | --here]
                                                 renew logins via staging sign-in (and over ssh)
    aterm harness accounts use <label> | --auto  pin an account or return to automatic rotation
    aterm harness accounts sync [<source>] [--machine <mN>] [--json]
                                                 sync accounts.toml and fleet-accounts.toml from <source>
                                                 or the source this machine configured (no default)
    aterm harness accounts serve [<source>] [--bind <addr:port>] [--machine <mN>]
                                                 serve account dashboard and fleet-accounts.toml
    aterm harness upgrade [<sid>] [--dry-run] [--json]
                                                 move live Claude Code sessions onto a newer build/model
    aterm harness upgrade [<sid>] --status [--json]
                                                 show pending upgrades and what each waits on
    aterm harness upgrade <sid> --now | --defer <dur> | --skip [--json]
                                                 hurry, defer, or skip one tab's upgrade
    aterm harness upgrade models [set <id>,<id>,...]
                                                 show or set the upgrade model priority list
    aterm harness versions [--json]              incremental numeric version of each CLI harness
    aterm harness tap                            Claude Code statusLine usage tap (`[harness] usage_tap`)

`hook` and `statusline` do nothing and exit 0, so settings that still name them stay harmless.

OPTIONS:
    --state <path>      harness state directory (default: $ATERM_HARNESS_STATE, else
                        <aterm state>/harness/claude-harness)
    --config <path>     aterm.toml to read; accounts.toml sits beside it (default:
                        $XDG_CONFIG_HOME/aterm/aterm.toml, else %APPDATA%\\aterm\\aterm.toml on
                        Windows, else ~/.config/aterm/aterm.toml)
    --sock <path>       limits, upgrade: control socket (default: resolved socket)
    --apply <class>     disk: safelist class to remove (`cargo-targets`, `claude-stale-versions`;
                        `atpkg-gc` and `claude-purge` are report-only and name the owning command)
    --dry-run           upgrade: print each session's next step without typing or signalling
                        (`disk --apply` and `upgrade models set` ignore it and still act)
    --status            upgrade: print recorded upgrades without sweeping
    --now               upgrade <sid>: move at first pause (still requires idle, empty draft,
                        no box/hold/child, and no keystroke in the last 20 s)
    --defer <dur>       upgrade <sid>: hold for <dur> (90s, 30m, 6h, 2d; max 30d)
    --skip              upgrade <sid>: stay on the running build until a newer target lands
    --shared            accounts add: mark account `shared` across machines
    --email <e>         accounts add, adopt-default: account email verified on renewal
    --browser <b>       accounts add, adopt-default: browser profile display name for sign-in
    --due               accounts renew: renew every due or expired login on this machine
    --on <host>,...     accounts renew: machines to renew next over ssh (`host` or `host:/abs/aterm`)
    --here              accounts renew: renew this machine's logins only
    --agy               usage: read agy's model quota (`agy -p /usage`)
    --refresh           agy: ask agy now instead of the kept reading
    --auto              accounts use: clear the pin
    --machine <mN>      accounts sync, serve: override machine id (`m8`, `m14`, \u{2026})
    --bind <addr:port>  accounts serve: listen address (default `127.0.0.1:8741`)
    --json              emit JSON
    -h, --help, help    print help (`aterm harness <verb> --help` for one verb)
";

const HELP_AGY: &str = "\
`agy` reads agy's model quota with `agy -p /usage --output-format json`, which runs no model
turn. agy's models sit in pools (Gemini; Claude and GPT), each with its own weekly and 5-hour
limit; a pool is `open`, `low` (a limit at or under `[harness.agy] low`, default 10%) or
`spent` (a limit at 0 that applies). The reading is kept under the harness state
(`agy-quota/home.json`, 15 minutes current, 5 while a pool is low or spent, until a spent
limit's refill passes) and every read adds a line to `agy-quota/home.jsonl`, from which each
limit's burn per hour and the moment it runs out at that rate are reckoned.
  - `agy pick [<model>]`: the model a launch of `<model>` (agy's default when none) runs. A
    spent pool rotates any launch to the first `[harness.agy] order` model in an open pool,
    else in the low pool with the most left, else it waits (exit 3) until the earliest refill;
    a low pool rotates agy's default, and keeps a named model. `aterm ctl spawn agent=agy`
    workers launch on this pick.
  - `[harness.agy]` in aterm.toml: `rotate = false` turns rotation off; `low = 0.1` (or `10`);
    `order = [\"claude-opus-5-5-medium\", \"gemini-4-argon-high\", ...]`.
";

const HELP_USAGE: &str = "\
`usage` folds this session's transcript and its subagent transcripts
(<session>/subagents/**/agent-*.jsonl) per model, up to a 4 GiB cap (`PREFIX` when hit;
`complete: false` in `--json` when the totals are not the whole session's).
  - Under Claude Code's tools (`CLAUDE_CODE_SESSION_ID` or `CLAUDE_PID`): that session's
    transcript. `5h`/`7d` (`windows` in `--json`) come from the usage tap's reading for
    `CLAUDE_PID`; `acct <label>` (`label` in `--json`) comes from `CLAUDE_PID`'s launch env
    when `accounts.toml` lists 2+ accounts.
  - Under a Codex tool (`CODEX_THREAD_ID`, no Claude session named): the root thread's
    rollout and its subagent rollouts under `CODEX_HOME` (else `~/.codex`), forked prefixes
    skipped, priced at list `usd` (\u{2248}$, never a bill).
  - Under agy's tools (`ANTIGRAVITY_CONVERSATION_ID`, no Claude session or Codex thread), or
    with `--agy` anywhere: agy's model quota from `agy -p /usage --output-format json`, each
    model group (Gemini; Claude and GPT) with its own weekly and 5-hour limit left, its refill
    time, and `SPENT` on a group a limit blocks.
  - Anywhere else: the newest transcript in `<claude dir>/projects/<cwd>/*.jsonl`
    (`CLAUDE_CONFIG_DIR`, else `~/.claude`). A relative `CLAUDE_CONFIG_DIR` or `CODEX_HOME`
    folds nothing.
";

const HELP_LIMITS: &str = "\
`limits` reads the session's screen (`text --json`) and prints:
  - `wall=<kind>` and `resets_at=`: the wall the last turn ended on, read by the foreground
    program's grammar (Claude Code, agy, Muse Code, Gemini CLI; Codex screens are not read).
  - `windows=`: usage windows painted on screen (Claude Code's `/usage` panel).
  - `credits=in-use|-`: whether Claude Code's notice says usage credits carry the session
    (the supervisor continues a session on credits only under `[harness] usage_credits = true`).
  At a wall, when `accounts.toml` lists accounts, a second line (not in `--json`) gives
  `account=` and `next=`.
Defaults to `$ATERM_PARENT_SESSION_ID`; exits 2 with neither `@<sid>` nor parent session,
or 1 if the session cannot be read.
";

const HELP_DISK: &str = "\
`disk` reports reclaimable build directories and caches; it removes nothing by default.
Removal needs both `disk.apply = true` in `aterm.toml` and `--apply <class>`. Every run
journals its report, and each removal or refusal, to `<state>/disk.jsonl`. An unreadable or
invalid `aterm.toml`, or a `[disk]` removal key (`apply`, `target_stale_days`,
`auto_free_gib`, `dead_build_dirs`, `idle_build_dirs`, `idle_build_days`) of the wrong type
or below another table's header, turns every manual and automatic removal off.

With no `<build-dir>` arguments, `disk` scans this working directory, `$HOME` checkouts
and worktrees (2 levels deep), `x.py` stage build dirs, aterm's Rust build cache, and
Claude Code's temp root on the current volume (never crossing mount points or protected
macOS/Windows cloud folders). `claude-stale-versions` are the installed Claude Code
versions other than the live one that no lock or running process holds.

THRESHOLDS (`watch:`):
  - `warn`: max(`disk.warn_free_gib` [default min(40 GiB, 20% volume)], 8% volume [<=64 GiB],
    2 * `room`)
  - `floor`: max(`disk.auto_free_gib` [default min(10 GiB, 10% volume)], 5% [<=32 GiB], `room`);
    <=0 turns off the window's pass and `--apply`'s tiers 1 and 4
  - `emergency`: max(4 GiB, 1% volume [<=8 GiB]), capped at `floor`; or <50,000 free inodes
  - `room`: the 2nd-largest daily 2h fall of free space over 7 days (clamped to
    [min(16 GiB, 5%), 10% <=256 GiB]), or `disk.room_gib`.
  A knob written in `aterm.toml` is read as written; only the defaults follow the volume.
  Volume state is `ok` (>=warn), `low` (<warn), `short` (<floor), or `emergency`.

WINDOW AUTOMATIC TIERS (cheapest first, re-measuring free space before each item):
  - tier 1 (`low`, `disk.dead_build_dirs = true`): whole scratch/workspace build dirs whose
    owner session or checkout is gone, with signed `CACHEDIR.TAG` and no locks or open files.
  - tier 2 (`low`): idle Cargo profile `incremental/` dirs (idle past `disk.target_stale_days`,
    default 1d) until free space reaches `warn` (when `short`, `floor + 5 GiB` if higher).
  - tier 3 (`short` or `await sys`, `disk.idle_build_dirs = true`): whole profiles of live
    checkouts idle past `disk.idle_build_days` (default 7d) with no running process or bin link.
  - tier 4 (`short`): recent profile caches (untouched for >=10m and unlocked) until free
    space reaches `floor + 5 GiB` or reclaimed bytes cover the shortfall.

`--apply cargo-targets` takes tier 1 (when `low`), tier 2 (at any free level), and tier 4
(when `short`); tier 3 (`idle_whole_up_to=`) is emptied only by the window's pass.
";

const HELP_TAP: &str = "\
`tap` is the statusLine command aterm adds to Claude Code relaunch lines while
`[harness] usage_tap = true` (the default). Claude Code pipes its usage JSON to
`<aterm> harness tap` after each reply and every 10 s; `tap` writes
`<aterm state>/harness/claude-harness/rl/<tab>-<pid>.json` (0600; `--state` and
`$ATERM_HARNESS_STATE` do not move it), prints `5h <p>% \u{b7} 7d <p>%`, and exits 0.
It is added only on the machine's own login (the default one, the roster's lone one, or,
where `accounts.toml` lists two or more accounts, any login it lists or one a renewal moved
one of its rows away from; one row's tap follows only its current login) in the default
Claude home, and only where no custom `CLAUDE_CONFIG_DIR`, credential, `statusLine`,
`disableAllHooks`, `apiKeyHelper`, `--settings` or `--bare` is present; a session started by
hand gets it when aterm restarts it in place. The window's footer shows the tab's week from
the tap or from `/usage`, whichever is newer: `weekly <p>% left` up to 15 minutes old,
`weekly <p>% left \u{b7} 40m ago` up to an hour, then nothing. Without a figure it says why:
`weekly ? \u{b7} restart tab` (a restart in place, offered now, would add the tap),
`weekly ? \u{b7} /usage` (the tap is off, or no restart would add it),
`weekly ? \u{b7} no reply yet` (tapped, and no reply since it started brought the week),
`weekly ? \u{b7} no reply 1h` (tapped, and its week is over an hour old),
`weekly unavailable` (no account profile is trusted for this process, so neither figure is:
the profile could not be read, `accounts.toml` or the store's `logins.json` could not be, or
the roster has no row for this login, or one with no `email` or another one; only the
default login, where the roster lists at most one account, needs no `email`).
The account-stats recorder rides on the same switch, and where `accounts.toml` lists two or
more accounts the readings feed early rotation (`[harness] accounts = \"early\"`). At a
usage limit a reading written since the limit was first read times the continuation
(`usage-resume@v1`) to the reset it names; where it and the notice's reset differ by more
than six minutes, the later wins and a `tap-reset@v1` ledger row records both. Switched off,
no new relaunch line carries the tap and no reading times a wait; a session already tapped
keeps writing readings until it is relaunched.
";

const HELP_VERSIONS: &str = "\
`versions` (`version`) prints the incremental numeric harness version (`v0.1`, \u{2026})
for each CLI harness (`codex-alab`, `claude`, `agy`, `grok`, `kimi`, `muse`, `gemini`).
";

const HELP_LEDGER: &str = "\
`ledger` prints the newest rows (default 20, at most 4096) of a harness jsonl ledger
(`--json`: verbatim):
  - `ledger [@<sid>] [<n>]`: supervisor approval decisions (`<aterm state>/drive/<sid>.jsonl`;
    `self.jsonl` with neither `@<sid>` nor a parent session)
  - `ledger disk [<n>]`: disk reports, removal intents, removals, and refusals
    (`<state>/disk.jsonl`, always verbatim)
  - `ledger upgrade [<n>]`: live upgrade steps and owner requests (`<state>/upgrade/ledger.jsonl`)
  - `ledger accounts [<n>]`: account walls, samples, rotations, pins, and auth checks
    (`<state>/accounts/ledger.jsonl`)
";

const HELP_ACCOUNTS: &str = "\
`accounts` manages Claude Code logins on this machine and reports rotation state.
The roster is `accounts.toml` beside `aterm.toml`: `[[account]]` rows with `label`,
`class` (`dedicated` or `shared`), optional `dir` (the one row without `dir` is the
machine's default login), and optional `email` and `browser`. An invalid roster is
refused whole (exit 1). The report prints no credential or email.

REPORT & ROTATION (`[harness] accounts = \"early\" | \"wall\" | \"off\"`):
  Prints `active: <label>, N of M sessions on it`; a row per account with its class, its
  condition (`not-enrolled`, `signed-out`, `limited until <time>`, `low`, `usable`, or
  `unseen`), its mark (`active`, `active:unproven`, `pinned`, or `-`), `offer:`, `credits:`
  and `warned:` where they apply, `on:` (its sessions, or `-`), and `login:` where it
  applies; then `next:` and `not next:`.
  - Under `early` (default), the supervisor rotates sessions at turn end off a limited or
    `low` account (>=90% 5h or >=98% week with >15m to reset, or carried by usage credits),
    and probes `/usage` on idle full-screen sessions (at most once per 3m per account).
  - Under `wall`, sessions rotate only when a turn ends on a usage limit.
  - Under `off`, limits are still recorded, but the supervisor never rotates sessions.

LIMIT RESET (`[harness] limit_reset = true`, default false):
  At Claude Code's usage-limit dialog on the machine's default login (no other account run
  since 25h before the session started, and a limit >24h away or an offer expiring within
  24h), the supervisor selects the matching reset row and confirms it, recording
  `reset-used`, `reset-kept`, or `reset-failed` from the transcript.
";

const HELP_ACCOUNTS_VERBS: &str = "\n\
VERBS:
  - `accounts add <label> [--shared] [--email <e>] [--browser <b>]`
    Creates `<state>/accounts/logins/<label>` (0700) and appends the row to `accounts.toml`
    (the first `add` also lists the default login as `main`). Labels match
    `[a-z0-9][a-z0-9._-]{0,63}` (not `help`). A row stays `not-enrolled` until signed in.
  - `accounts adopt-default <label> --email <e> [--browser <b>]`
    Lists the machine's default login (no `dir`) as a renewable row.
  - `accounts login <label>`
    Runs `CLAUDE_CONFIG_DIR='<dir>' claude auth login` (plain `claude auth login` for the
    default row) in a terminal, then checks once with `claude auth status --json` (20 s cap)
    under `CLAUDE_SECURESTORAGE_CONFIG_DIR` and records `signed in` (exit 0) or the failure
    (exit 1). With no terminal on stdin it prints the command, runs nothing, and exits 0.
    Refuses a row with `email` (use `accounts renew`).
  - `accounts renew [--due | <label>...] [--on <host>,... | --here]`
    Signs in rows with `email` through a staging dir (`<label>+<n>`), checks that the
    account email matches, and binds the row in `<state>/accounts/logins.json` so sessions
    move at their next idle turn end (the reply names a `[harness]` switch that stops this;
    `wrong-account` deletes the staging dir and changes nothing live, though a bound row
    records the attempt). Then runs `ssh -t <host> aterm harness accounts renew --due --here`
    on each host in `--on` (or `[harness] hosts` under `--due`; none with `--here`).
  - `accounts use <label> | --auto`
    Pins an enrolled account that is not signed out (clearing its recorded walls), or
    clears the pin (`--auto`).
  - `accounts stats [--json]`
    Reads 5h, week, and Fable windows, plan, credits, 8-day list-price spend (\u{2248}$), login
    state, and running sessions for the current home (`--json` is schema 1 `account-stats`).
  - `accounts sync [<source>] [--machine <mN>] [--json]`
    Synchronizes this machine's `accounts.toml` and `fleet-accounts.toml` from `<source>` (a
    file, `~/` for the home; an http(s) URL fetched with curl, a Sheets `/edit` link read as
    its CSV export; an `rclone:<remote>:<path>` source is refused), else from the source
    this machine configured. With neither it reads nothing and exits 1. It waits at most
    10 s for the accounts lock, then fails and syncs nothing.
  - `accounts serve [<source>] [--bind <addr:port>] [--machine <mN>]`
    Serves the account rotation dashboard and `fleet-accounts.toml` from the same source,
    on `127.0.0.1:8741` unless `--bind` names another address (a LAN one shares it).

FLEET ACCOUNTS (off unless this machine names a source):
  - Opt in with `[fleet] accounts_source = \"<source>\"` in aterm.toml, by running
    `accounts sync <source>` once (its `fleet-accounts.toml` then says `opt_in = \"named\"`),
    or by keeping `fleet-accounts.toml`, `ai-accounts.csv`, `ai-mailbox.toml` or
    `mailbox.toml` in `~/.secrets/`, which a window syncs from. A `fleet-accounts.toml`
    without `opt_in = \"named\"` opts nothing in. To opt out, remove the key, delete
    `fleet-accounts.toml`, and move those `~/.secrets/` files away.
  - Where a source is configured, a window running under this account's own HOME syncs a
    missing `accounts.toml` (at most once in 10 minutes) and starts a walled login's
    successor's sign-in in the background (`claude auth login --claudeai --email <row's
    email>`, answered from `<state>/accounts/mailbox` where that folder exists). An instance
    with a scratch HOME or a private state root keeps no fleet accounts.
  - Where none is, the window fetches and starts nothing and logs
    `accounts-upkeep=unconfigured` once (`fleet-file=ignored` where a fleet file opts nothing
    in). `accounts renew` by hand needs no source.
";

const HELP_UPGRADE: &str = "\
`upgrade` moves live supervised sessions onto a newer build (and newer model) in place,
keeping the conversation and running work.

MODES:
  - Quiet mode (`[harness] upgrade_ask = false`, default): waits for a genuinely idle point
    (turn over; no child shell, background task, subagent or workflow; no box; empty draft;
    nobody at the tab; the ladder's settle), sends SIGTERM, types the relaunch line with
    `--resume <session>`, and resumes without typing a prompt (`done:resumed`;
    `wait:notice-shown` before the move).
  - Cooperative mode (`[harness] upgrade_ask = true`): types a readiness notice asking the
    agent to finish background work and confirm. It repeats every 30m, up to 4 notices,
    then rests 2h (then 4h, 8h, and 24h from the fourth unanswered round in a row) and
    re-arms (`rearmed:<why>`). It never types into a session at a usage limit.

THE LADDER (activity delays, never disables):
  - <20m behind (`prefer`): screen still 20 s, no human input within `[harness] human_grace_s`.
  - >=20m (`settled`): two idle reads 20 s apart with the same last words.
  - >=1h (`keys-only`): human grace narrows to a keystroke in the last 20 s.
  - >=2h or `--now` (`land`): first idle pause with no keystroke in the last 20 s.
  Never moves over a draft, approval box, keystroke in the last 20 s, hold, usage/login
  wall, or running child process.

SCOPE & EXIT CODES:
  - Moves only foreground jobs on an aterm tab's own PTY (tmux/screen/ssh panes wait as
    `held-back:<owner>`; wrapper scripts refuse).
  - Model upgrades (`aterm harness upgrade models`) ride the relaunch line as `--model`:
    the newest model of the same family, or the next one up the priority list.
  - Muse Code (`muse resume <uuid>`) and agy (`agy --conversation=<uuid>`) are moved by the
    window in quiet mode only; Codex and Gemini CLI are never moved live.
  - Hand-run sweeps exit 0, 1 (`[harness] enabled` off without `--dry-run`, or the sweep
    lock cannot be made), 2 (no home directory), or 75 (`step=busy:another-sweep`);
    `--status` exits 1 when upgrade records or session files cannot be read whole.
";

const HELP_UPGRADE_STATUS: &str = "\
`upgrade --status` prints recorded upgrades for live sessions without sweeping:
  - `phase=`: the round's phase (`pending`, `announced:<notices>`, `exiting`, `relaunched`,
    `failed:<why>`).
  - `pending_for=`, `wait=`, `wait_for=`: time behind and the current wait (among them
    `settling`, `awaiting-ready`, `ready-unrecorded`, `superseded`, `background`, `draft`,
    `attended`, `held`, `terminal:<owner>`, `not-idle:busy`, `not-idle:waiting`,
    `status-stale:<status>`, `release:<gate>`).
  - `request=`: owner request (`-`, `now`, `defer:<left>`, `skip:<version>`).
  - `next_round=`: time until a stopped round re-arms (`due` when ready, `-` otherwise).
  - `stalled=`: `-` while healthy, else `refused:<why>`, `failed:<why>`, `held-back:<owner>`,
    `stuck:<exiting|exited|relaunched>` (>5m in restart), `unwatched` (>1h without a host
    look), or `overdue` (>6h behind, >1h past a usage-limit reset, or a notice refused for 90m).
  - `held_by=`: child processes (`<pid>(<name>:<age>),...` or `-`).
  - `release=`: release line owed after an abandoned notice (`gave-up`, `void`, `skipped`,
    `deferred`, `retargeted`, `model-dropped`, the reason a round stopped after its notice,
    or `-`).
  - `rung=`: ladder rung (`prefer`, `settled`, `keys-only`, `land`, or `-`).
  - `looked=`, `by=`, `point=`, `guard=`, `watch_at=`: last host look, build, idle point,
    the supervisor's withholding guard word (such as `wall`, `settle`, `no-background-wait`,
    `act-untaken`, `not-idle`, `stall`, `prompt`), and next watch deadline (`due` or `-`).
  - `cause=`, `told=`, `refused=`: wait class (`own-work`, `person`, `limit`, `limit-passed`,
    `tab-unread`, `under-way`, `notice`, `queued`, `unreached`, `build-fails`, `next-point`,
    `unrecorded`, `unclassified`, or `-`), surfaced alert kind (`agent-down`,
    `relaunch-stuck`, `hung-exit`, `harness-stuck`, `no-progress`, `no-terminal`,
    `build-fails`, `refused-launch`, `repeating-stop`, `suspended`, `typed-draft`,
    `limit-passed`, `unwatched`, `own-work`, or `-`), and fence refusals (`<n>/<span>`).
  - `--json` adds `attention` (`quiet`, `fault`, `digest`, `interrupt`), `attention_kind`,
    `cause`, `cause_words`, and `waits` (last 24 wait transitions).
";

const HELP_UPGRADE_WORD: &str = "\
OWNER REQUESTS (`upgrade <sid> --now | --defer <dur> | --skip`):
  Written under the upgrade lock (waits up to 10 s, else exits 75) as `requested:<word>`
  on the ledger; it wakes the tab's worker. It exits 1 when no upgrade is recorded for the
  tab or its restart is already under way.
  - `--now`: jumps to the `land` rung and re-arms a round that gave up (exit 1 for a round
    stopped any other way); a session in a multiplexer pane is recorded, with a note.
  - `--defer <dur>`: holds the upgrade for `<dur>` (up to 30d) and cancels any open notice.
  - `--skip`: keeps the session on its current build until a newer target build arrives.
";

const HOME_VAR: &str = accounts::HOMEDIR_ENV;

/// Everything the subcommands need from outside themselves, injected so every decision below is a pure function of its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    /// The harness state directory.
    pub state: PathBuf,
    /// The aterm state root the supervisor's approval ledger lives under (`<root>/drive/<sid>.jsonl`); `None` when it cannot be located.
    pub aterm_state: Option<PathBuf>,
    /// The session's working directory.
    pub cwd: PathBuf,
    /// The owner's home, when known.
    pub home: Option<PathBuf>,
    /// The Claude Code directory: `CLAUDE_CONFIG_DIR` when set, else `$HOME/.claude` ([`footer::claude_dir_of`]); `None` falls back to `home` ([`Env::claude_dir`]).
    pub claude_dir: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR` when it is set but names no directory this command can find (a relative path, resolved against the vendor's own working directory).
    pub claude_config_unusable: Option<String>,
    /// The profile Claude Code keeps for the home this command's environment names (`.claude.json`, `claude_profile_of`: beside `~/.claude` with `CLAUDE_CONFIG_DIR` unset, inside the directory a set one names, in `$HOME` for an empty one); `None` where it cannot be named.
    pub claude_profile: Option<PathBuf>,
    /// `CLAUDE_CODE_CUSTOM_OAUTH_URL` is set and not empty ([`profile::custom_oauth`]): Claude Code then names the profile `.claude-custom-oauth.json` ([`profile::effective`]). aterm strips `CLAUDE_*` from its own shells, so only Claude Code's tools carry it.
    pub claude_custom_oauth: bool,
    /// The Claude Code session this command runs under: `CLAUDE_CODE_SESSION_ID`, which Claude Code sets for every tool it runs (aterm strips `CLAUDE_*` from its own shells).
    pub claude_session: Option<String>,
    /// The Claude Code process this command runs under (`CLAUDE_PID`, set beside the session id), for a build that names only the process.
    pub claude_pid: Option<u32>,
    /// The Codex thread this command runs under, when Codex named it: `CODEX_THREAD_ID`, which Codex sets for the commands its tools run (a thread id, [`super::codex_rollout::is_thread_id`]). aterm strips `CODEX_*` from the shells it spawns, so only Codex's own tools carry it.
    pub codex_thread: Option<String>,
    /// `CODEX_HOME` as this command's environment sets it, raw: the Codex home is it, else `$HOME/.codex`, and a relative one names none ([`crate::harness::codex_rollout::codex_home`]).
    pub codex_home: Option<String>,
    /// The agy conversation this command runs under: `ANTIGRAVITY_CONVERSATION_ID`, which agy sets for the commands its tools run. aterm strips `ANTIGRAVITY_*` from the shells it spawns, so only agy's own tools carry it.
    pub agy_conversation: Option<String>,
    /// Unix seconds.
    pub now: i64,
    /// The aterm session id (`$ATERM_PARENT_SESSION_ID`), or empty.
    pub sid: String,
    /// `--sock` — the control socket `limits` reads through.
    pub sock: Option<String>,
    /// `aterm.toml` — where the `[disk]` knobs live.
    pub config: Option<PathBuf>,
    /// Claude Code's per-user temp root, where `disk` searches for the sessions' scratch builds ([`disk::discover::claude_tmp_root`]).
    pub claude_tmp: Option<PathBuf>,
    /// The running Claude Codes and the directories the processes were started with ([`disk::discover::running_claude_census`]), read for `disk` and `accounts stats` alone ([`main_entry`]).
    pub claude_running: Option<disk::discover::ClaudeCensus>,
    /// The local clock's offset from UTC in seconds ([`limit::tz_offset_s`]), read for `accounts stats`' text alone ([`main_entry`]); `0` (UTC) elsewhere.
    pub tz_s: i64,
}

impl Env {
    /// Read the process environment.
    pub fn from_process() -> Result<Env, String> {
        let home = std::env::var_os(HOME_VAR)
            .map(PathBuf::from)
            .filter(|h| h.is_absolute());
        let aterm_state = aterm_types::dirs::state_dir();
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
        let config_dir = std::env::var("CLAUDE_CONFIG_DIR").ok();
        let claude_dir = footer::claude_dir_of(
            config_dir.as_deref(),
            std::env::var(HOME_VAR).ok().as_deref(),
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
                .filter(|id| super::codex_rollout::is_thread_id(id)),
            codex_home: std::env::var("CODEX_HOME").ok(),
            agy_conversation: std::env::var("ANTIGRAVITY_CONVERSATION_ID")
                .ok()
                .filter(|id| !id.trim().is_empty()),
            now,
            sid: std::env::var("ATERM_PARENT_SESSION_ID").unwrap_or_default(),
            sock: None,
            config: default_config_path(),
            claude_tmp: Some(disk::discover::claude_tmp_root(disk::discover::own_uid())),
            claude_running: None,
            tz_s: 0,
        })
    }

    /// Runs under agy's tools and under no Claude Code session or Codex thread, so `usage` reads agy's quota.
    #[must_use]
    pub fn under_agy(&self) -> bool {
        self.agy_conversation.is_some()
            && self.claude_session.is_none()
            && self.claude_pid.is_none()
            && self.codex_thread.is_none()
    }

    /// The Claude Code directory: [`Env::claude_dir`], else `$HOME/.claude`, but none when `CLAUDE_CONFIG_DIR` is set and unusable ([`Env::claude_config_unusable`]).
    #[must_use]
    pub fn claude_dir(&self) -> Option<PathBuf> {
        if self.claude_config_unusable.is_some() {
            return None;
        }
        self.claude_dir
            .clone()
            .or_else(|| self.home.as_ref().map(|h| h.join(".claude")))
    }

    /// Where the vendor keeps its transcripts: `<claude dir>/projects`, which `--bare` (no hooks, plugins or statusLine) leaves in place.
    #[must_use]
    pub fn transcripts_root(&self) -> Option<PathBuf> {
        self.claude_dir().map(|d| d.join("projects"))
    }

    /// The disk journal.
    #[must_use]
    pub fn disk_ledger(&self) -> PathBuf {
        self.state.join(DISK_LEDGER)
    }

    /// The accounts roster: `accounts.toml` beside [`Env::config`] (`--config` moves both), by [`accounts::roster_beside`], the rule a host reads by too.
    #[must_use]
    pub fn accounts_roster(&self) -> Option<PathBuf> {
        accounts::roster_beside(self.config.as_deref()?)
    }

    /// The accounts store: `<state>/accounts` (the state file, its ledger, the lock, and the login directories `accounts add` makes).
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

/// Which ledger `ledger` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerFile {
    /// The supervisor's approval ledger for one session; `None` is this one.
    Approvals(Option<String>),
    /// The disk journal.
    Disk,
    /// The live upgrade's ledger (`<state>/upgrade/ledger.jsonl`).
    Upgrade,
    /// The accounts ledger (`<state>/accounts/ledger.jsonl`, [`accounts::Store::ledger_path`]).
    Accounts,
}

/// One form of `accounts` (design `docs/DESIGN-claude-accounts-2026-10-01.md` §4, §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountsCmd {
    /// `accounts [--json]`: the report.
    Report {
        /// The JSON form.
        json: bool,
    },
    /// `accounts add <label> [--shared] [--email <e>] [--browser <b>]`: make the login directory, append the roster row.
    Add {
        /// The label as typed; admitted when the verb runs, so an inadmissible one is a refusal by name (exit 1), not a usage error.
        label: String,
        /// `--shared`, else dedicated.
        class: Class,
        /// `--email`: the row renews through `accounts renew`.
        email: Option<String>,
        /// `--browser`: the profile its sign-in opens in.
        browser: Option<String>,
    },
    /// `accounts adopt-default <label> --email <e> [--browser <b>]`: this machine's own login (the row with no `dir`) as one renewable row.
    AdoptDefault {
        /// The label as typed.
        label: String,
        /// `--email`, required.
        email: String,
        /// `--browser`.
        browser: Option<String>,
    },
    /// `accounts renew [<label>…] [--due] [--on <host>,… | --here]`: each named login, or every due one, renewed through a staging directory and checked before it goes live ([`run_accounts_renew`]); then `renew --due --here` on each other machine named.
    Renew {
        /// The labels named, in order.
        labels: Vec<String>,
        /// `--due`: every login of this machine that is due or expired.
        due: bool,
        /// `--on`: the machines to renew on next, over SSH; `None` reads `[harness] hosts` (only under `--due`).
        on: Option<Vec<String>>,
    },
    /// `accounts login <label>`: the vendor's sign-in flow for that account, waited for, and — after a flow that exits 0 — what its new login then answers recorded.
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
    /// `accounts stats [--json]`: what this machine has of the Claude Code account of the home the environment names ([`super::account_stats`]).
    Stats {
        /// The JSON form.
        json: bool,
    },
    /// `accounts sync [<source>] [--machine <mN>] [--json]`: synchronize this machine's `accounts.toml` and `fleet-accounts.toml` from `source`, else the source this machine configured ([`super::accounts_sync::configured_source`]); with neither, nothing.
    Sync {
        /// Optional path or URL of a CSV or a `fleet-accounts.toml`.
        source: Option<String>,
        /// Optional machine ID override (`m8`, `m14`, …).
        machine: Option<String>,
        /// The JSON form.
        json: bool,
    },
    /// `accounts serve [<source>] [--bind <addr:port>] [--machine <mN>]`: serve the local/LAN account rotation dashboard and `fleet-accounts.toml`.
    Serve {
        /// Optional path or URL of a CSV or a `fleet-accounts.toml`; else the configured source.
        source: Option<String>,
        /// Optional bind address (`127.0.0.1:8741` or `0.0.0.0:8741`).
        bind: Option<String>,
        /// Optional machine ID override (`m8`, `m14`, …).
        machine: Option<String>,
    },
}

/// The owner's word on the live upgrade (`upgrade [<sid>] --status`, `upgrade <sid> --now|--defer <dur>|--skip`).
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
    /// `hook` or `statusline`: retired, a silent success ([`run_retired`]).
    Retired,
    /// `tap`: the usage tap ([`super::tap::run`]).
    Tap,
    /// The usage view (design §5.2).
    Usage {
        /// The JSON form.
        json: bool,
        /// `--agy`: agy's model quota ([`super::agy_quota`]) wherever this runs.
        agy: bool,
    },
    /// agy's model quota, kept and tracked, and the model rotation picks ([`run_agy`]).
    Agy {
        /// Quota or pick.
        what: AgyWhat,
        /// `--refresh`: ask agy now, whatever was kept.
        refresh: bool,
        /// The JSON form.
        json: bool,
    },
    /// The wall and the painted `/usage` windows on one session's screen (design §5.8).
    Limits {
        /// The session (`@s-…`); `None` is this one.
        sid: Option<String>,
        /// The JSON form.
        json: bool,
    },
    /// The disk report, and the one apply path (design §5.5).
    Disk {
        /// Build directories to consider; empty searches for them ([`disk_roots`]).
        targets: Vec<PathBuf>,
        /// The one safelist class to act on.
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
    /// Move live Claude Code sessions onto the newer build in place, resuming the same conversation in the same tab ([`super::upgrade_drive`]).
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
    /// The model priority list of the live upgrade: print it, with each model's availability on the managed build, the target, and Claude Code's own recommendations; or `set` it (best first).
    UpgradeModels {
        /// `Some(list)`: replace the list with this comma-separated one.
        set: Option<String>,
    },
    /// The Claude Code accounts: the report, and the roster's verbs.
    Accounts(AccountsCmd),
    /// The incremental numeric harness version of each CLI harness.
    Versions {
        /// The JSON form.
        json: bool,
    },
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
    /// `versions --help`: incremental numeric harness versions.
    Versions,
    /// `tap --help`: the usage tap.
    Tap,
    /// `agy --help`: agy's quota pools, their tracking, and model rotation.
    Agy,
}

fn help_reset(takes: impl Fn(ResetKind) -> bool, anchors: &[Anchor]) -> String {
    let (taken, left): (Vec<ResetKind>, Vec<ResetKind>) =
        ResetKind::ALL.into_iter().partition(|kind| takes(*kind));
    let some = |kinds: &[ResetKind]| {
        listed(
            &kinds
                .iter()
                .map(|kind| format!("a {}", kind.name()))
                .collect::<Vec<_>>(),
            "or",
        )
    };
    let mut builds: Vec<&str> = Vec::new();
    for anchor in anchors.iter().filter(|a| a.id.starts_with("reset.")) {
        if !builds.contains(&anchor.since) {
            builds.push(anchor.since);
        }
    }
    builds.sort_by(|a, b| {
        if newer_than(a, b) {
            std::cmp::Ordering::Greater
        } else if newer_than(b, a) {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Equal
        }
    });
    let mut text = String::from(
        "  The dialog draws a row for each kind of reset on offer (`Use a <kind> (<n> left,\n  \
         until <day>)`).",
    );
    if taken.is_empty() {
        text.push_str(" The supervisor takes no such row.");
    } else {
        text.push_str(
            " By default the supervisor takes one only where it is\n  \
             the only reset on offer and is ",
        );
        text.push_str(&some(&taken));
        text.push('.');
    }
    if !left.is_empty() {
        text.push_str("\n  It leaves ");
        text.push_str(&some(&left));
        text.push_str(" to you.");
    }
    if !taken.is_empty() {
        let words: Vec<String> = taken
            .iter()
            .map(|kind| format!("\"{}\"", crate::supervise::config::reset_kind_word(*kind)))
            .collect();
        text.push_str(&format!(
            "\n  Where several kinds are on offer it takes the wait row, unless\n  \
             `[harness] limit_reset_kinds = [{}]` names the kinds it may spend,\n  \
             best first: then it takes the first listed kind on offer, and never an unlisted one.\n  \
             It presses Enter on a row only where the screen read after the arrow keys shows\n  \
             that row selected.",
            words.join(", ")
        ));
    }
    text.push_str(
        "\n  Untested on a real wall: every text it reads comes from\n  \
         Claude Code",
    );
    if builds.is_empty() {
        text.push_str("'s bundle.\n");
    } else {
        let owned: Vec<String> = builds.iter().map(|b| format!("{b}'s")).collect();
        text.push(' ');
        text.push_str(&listed(&owned, "and"));
        text.push_str(if builds.len() == 1 {
            " bundle.\n"
        } else {
            " bundles.\n"
        });
    }
    text
}

fn listed(items: &[String], conj: &str) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} {conj} {last}", init.join(", ")),
    }
}

/// The text of `page`: for a verb, its lines of [`USAGE`] and then its paragraphs.
#[must_use]
pub fn help_page(page: HelpPage) -> String {
    let accounts: String;
    let accounts_paras: [&str; 1];
    let (verb, status_only, paras): (&str, bool, &[&str]) = match page {
        HelpPage::Verbs => return USAGE.to_string(),
        HelpPage::Usage => ("usage", false, &[HELP_USAGE]),
        HelpPage::Agy => ("agy", false, &[HELP_AGY]),
        HelpPage::Limits => ("limits", false, &[HELP_LIMITS]),
        HelpPage::Disk => ("disk", false, &[HELP_DISK]),
        HelpPage::Ledger => ("ledger", false, &[HELP_LEDGER]),
        HelpPage::Accounts => {
            accounts = [
                HELP_ACCOUNTS,
                &help_reset(ResetKind::takeable, aterm_phase::ANCHORS),
                HELP_ACCOUNTS_VERBS,
            ]
            .concat();
            accounts_paras = [accounts.as_str()];
            ("accounts", false, &accounts_paras)
        }
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
        HelpPage::Versions => ("versions", false, &[HELP_VERSIONS]),
        HelpPage::Tap => ("tap", false, &[HELP_TAP]),
    };
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
    let mut agy = false;
    let mut refresh = false;
    let mut email: Option<String> = None;
    let mut browser: Option<String> = None;
    let mut machine: Option<String> = None;
    let mut bind: Option<String> = None;
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
            "--agy" => agy = true,
            "--refresh" => refresh = true,
            "--email" => {
                email = Some(value(i, "--email")?);
                i += 1;
            }
            "--browser" => {
                browser = Some(value(i, "--browser")?);
                i += 1;
            }
            "--machine" => {
                machine = Some(value(i, "--machine")?);
                i += 1;
            }
            "--bind" => {
                bind = Some(value(i, "--bind")?);
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
            Some("agy") => HelpPage::Agy,
            Some("limits") => HelpPage::Limits,
            Some("disk") => HelpPage::Disk,
            Some("ledger") => HelpPage::Ledger,
            Some("accounts") => HelpPage::Accounts,
            Some("upgrade") if owner.contains(&OwnerAsk::Status) => HelpPage::UpgradeStatus,
            Some("upgrade") => HelpPage::Upgrade,
            Some("versions" | "version") => HelpPage::Versions,
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
        "hook" | "statusline" => Cmd::Retired,
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
            Cmd::Usage { json, agy }
        }
        "agy" => Cmd::Agy {
            what: match &rest[1..] {
                [] | ["quota"] => AgyWhat::Quota,
                ["pick"] => AgyWhat::Pick(None),
                ["pick", model] if !model.is_empty() => AgyWhat::Pick(Some((*model).to_owned())),
                other => {
                    return Err(format!(
                        "agy takes `quota` or `pick [<model>]`, not {other:?}"
                    ));
                }
            },
            refresh,
            json,
        },
        "versions" | "version" => {
            if rest.len() > 1 {
                return Err(format!("{verb} takes no operand, not {:?}", &rest[1..]));
            }
            Cmd::Versions { json }
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
                ["sync"] => AccountsCmd::Sync {
                    source: None,
                    machine: machine.take(),
                    json,
                },
                ["sync", src] => AccountsCmd::Sync {
                    source: Some((*src).to_string()),
                    machine: machine.take(),
                    json,
                },
                ["serve"] => AccountsCmd::Serve {
                    source: None,
                    bind: bind.take(),
                    machine: machine.take(),
                },
                ["serve", src] => AccountsCmd::Serve {
                    source: Some((*src).to_string()),
                    bind: bind.take(),
                    machine: machine.take(),
                },
                other => {
                    return Err(format!(
                        "accounts takes nothing, `stats`, `sync [<source>] [--machine <mN>]`, \
                         `serve [<source>] [--bind <addr:port>] [--machine <mN>]`, `add <label> \
                         [--shared] [--email <e>] [--browser <b>]`, `adopt-default <label> \
                         --email <e>`, `login <label>`, `renew [--due | <label>…] [--on \
                         <host>,…]` or `use <label> | --auto`, not {other:?}"
                    ));
                }
            };
            if json
                && !matches!(
                    form,
                    AccountsCmd::Report { .. }
                        | AccountsCmd::Stats { .. }
                        | AccountsCmd::Sync { .. }
                )
            {
                return Err(
                    "--json is the report's form (`aterm harness accounts --json`), \
                     `accounts stats --json`, and `accounts sync --json`: the other forms print \
                     lines"
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
    if email.is_some() || browser.is_some() {
        return Err(
            "--email and --browser belong to `accounts add <label>` and `accounts adopt-default \
             <label>`"
                .to_string(),
        );
    }
    if machine.is_some() || bind.is_some() {
        return Err(
            "--machine and --bind belong to `accounts sync` and `accounts serve`".to_string(),
        );
    }
    if (due || here || on.is_some()) && !matches!(cmd, Cmd::Accounts(AccountsCmd::Renew { .. })) {
        return Err("--due, --here and --on belong to `accounts renew`".to_string());
    }
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

/// The longest `--defer`: a month.
pub const UPGRADE_MAX_DEFER_S: u64 = 30 * 86_400;

/// `--defer`'s value: a count with a unit — `90s`, `30m`, `6h`, `2d` — or bare seconds; more than zero and at most [`UPGRADE_MAX_DEFER_S`].
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

/// How many directory entries the transcript search looks at: a fence against a project directory that holds something other than one file per session.
pub const MAX_TRANSCRIPT_ENTRIES: usize = 4096;

/// The byte cap on what one run folds over all the session's files (the window's per-read `footer::FOLD_BUDGET` does not apply).
pub const MAX_TRANSCRIPT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Which transcript `usage` folds, and how it was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// The session Claude Code named in this command's environment (`CLAUDE_CODE_SESSION_ID`, or `CLAUDE_PID`'s `sessions/<pid>.json`): this session's own file.
    Named {
        /// The transcript.
        path: PathBuf,
        /// `session-id` or `claude-pid`.
        how: &'static str,
    },
    /// The session was named but has written no transcript yet: nothing is folded — another session's file is not this one's.
    NamedButAbsent {
        /// The session id.
        session: String,
    },
    /// Nothing named a session: the newest `.jsonl` in this working directory's project directory, which may be another session's.
    Newest(PathBuf),
    /// Nothing to fold.
    None,
}

/// Choose the transcript `usage` folds: the session Claude Code named ([`Env::claude_session`], then [`Env::claude_pid`]'s sessions file), else the newest in this working directory's project directory (aterm's own shells carry no `CLAUDE_*`).
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

const USAGE_ACCOUNT: &str = "account";

trait ClaudeProcess {
    fn started(&self, pid: u32) -> Option<u64>;
    fn exec_env(&self, pid: u32) -> Option<Vec<String>>;
}

struct LiveClaude;

impl ClaudeProcess for LiveClaude {
    fn started(&self, pid: u32) -> Option<u64> {
        aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?).map(|born| born.seconds)
    }

    fn exec_env(&self, pid: u32) -> Option<Vec<String>> {
        atpkg::caller_shell::process_args(pid).map(|args| args.env)
    }
}

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

fn usage_reading(env: &Env, process: &dyn ClaudeProcess) -> Option<super::tap::Reading> {
    let pid = env.claude_pid?;
    let dir = super::tap::rl_dir_in(&env.aterm_state.as_ref()?.join("harness").join(HARNESS));
    std::fs::symlink_metadata(dir.join(super::tap::file_name(&env.sid, pid)?)).ok()?;
    let started = i64::try_from(process.started(pid)?).ok()?;
    super::tap::Reading::read(&dir, &env.sid, pid, started).map(|(reading, _)| reading)
}

fn run_usage(env: &Env, process: &dyn ClaudeProcess, json: bool, out: &mut dyn Write) -> ExitCode {
    let pick = pick_transcript(env);
    if let (Pick::Newest(_) | Pick::None, Some(thread)) = (&pick, &env.codex_thread) {
        return run_codex_usage(env, thread, json, out);
    }
    let (path, source, pick_word) = match &pick {
        Pick::Named { path, how } => (Some(path.clone()), Source::Transcript, *how),
        Pick::Newest(path) => (Some(path.clone()), Source::TranscriptNewest, "newest-mtime"),
        Pick::NamedButAbsent { .. } | Pick::None => (None, Source::None, "none"),
    };
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

/// What `aterm harness agy` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgyWhat {
    /// Every pool's limits, standing and burn.
    Quota,
    /// The model a launch of `<model>` (or agy's default) runs after rotation.
    Pick(Option<String>),
}

fn agy_store(env: &Env) -> super::agy_quota_store::Store {
    super::agy_quota_store::Store::new(&env.state, &super::agy_quota_store::launch_key(None, &[]))
}

fn run_agy(
    env: &Env,
    what: &AgyWhat,
    refresh: bool,
    json: bool,
    source: &dyn super::agy_quota_store::Source,
    out: &mut dyn Write,
) -> ExitCode {
    use super::agy_quota::{self, Pick};
    use super::agy_quota_store as store;
    let (rotation, notes) = agy_quota::Rotation::from_path(env.config.as_deref());
    let st = agy_store(env);
    let current = store::current(&st, source, env.now, rotation.low, refresh);
    let (reading, got) = match current {
        Ok(r) => r,
        Err(why) => {
            if json {
                let mut o = Map::new();
                o.insert("schema".to_owned(), Value::from(1u64));
                o.insert("kind".to_owned(), Value::from("agy-quota"));
                o.insert("source".to_owned(), Value::from("agy -p /usage"));
                o.insert("error".to_owned(), Value::from(why));
                let _ = writeln!(
                    out,
                    "{}",
                    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
                );
            } else {
                let _ = writeln!(out, "agy quota unread: {why}");
            }
            return ExitCode::FAILURE;
        }
    };
    let age = env.now.saturating_sub(reading.as_of);
    let history = st.history();
    match what {
        AgyWhat::Quota => {
            if json {
                let mut v = agy_quota::to_json(&reading.quota, env.now);
                if let Some(o) = v.as_object_mut() {
                    o.insert(
                        "as_of".to_owned(),
                        Value::from(super::usage::rfc3339_utc(reading.as_of)),
                    );
                    o.insert("got".to_owned(), Value::from(got.word()));
                    o.insert("low".to_owned(), Value::from(rotation.low));
                    if let Some(id) = &env.agy_conversation {
                        o.insert("agy_conversation".to_owned(), Value::from(id.clone()));
                    }
                    if let Some(groups) = o.get_mut("groups").and_then(Value::as_array_mut) {
                        for (g, q) in groups.iter_mut().zip(&reading.quota.groups) {
                            let Some(g) = g.as_object_mut() else { continue };
                            g.insert(
                                "standing".to_owned(),
                                Value::from(q.standing(rotation.low).word()),
                            );
                            if let Some(buckets) =
                                g.get_mut("buckets").and_then(Value::as_array_mut)
                            {
                                for (b, qb) in buckets.iter_mut().zip(&q.buckets) {
                                    let Some(b) = b.as_object_mut() else { continue };
                                    let burn = store::burn(&history, &qb.id);
                                    b.insert(
                                        "burn_per_hour".to_owned(),
                                        Value::from(burn.map(|x| x.per_hour)),
                                    );
                                    b.insert(
                                        "empty_at".to_owned(),
                                        Value::from(burn.and_then(|x| x.empty_at)),
                                    );
                                }
                            }
                        }
                    }
                    if !notes.is_empty() {
                        o.insert("config_notes".to_owned(), Value::from(notes));
                    }
                }
                let _ = writeln!(out, "{}", aterm_json::to_string(&v).unwrap_or_default());
                return ExitCode::SUCCESS;
            }
            let _ = writeln!(
                out,
                "{}  [source=agy -p /usage, {} {}]",
                agy_quota::hud_line(&reading.quota, env.now),
                got.word(),
                if age < 60 {
                    "now".to_owned()
                } else {
                    format!("{} ago", agy_quota::span(age))
                }
            );
            for g in &reading.quota.groups {
                let standing = g.standing(rotation.low);
                let mut line = format!("  {}: {}", g.name, standing.word());
                if let Some(models) = &g.models {
                    line.push_str(&format!(" \u{b7} {models}"));
                }
                for b in &g.buckets {
                    if let Some(burn) = store::burn(&history, &b.id).filter(|x| x.per_hour > 0.0) {
                        line.push_str(&format!(
                            " \u{b7} {} burns {}/h",
                            b.window.word(),
                            agy_quota::percent(burn.per_hour)
                        ));
                        if let Some(at) = burn.empty_at {
                            line.push_str(&format!(", out in {}", agy_quota::span(at - env.now)));
                        }
                    }
                }
                let _ = writeln!(out, "{line}");
            }
            for note in notes {
                let _ = writeln!(out, "  aterm.toml: {note}");
            }
            ExitCode::SUCCESS
        }
        AgyWhat::Pick(asked) => {
            let (model, named) = match asked {
                Some(m) => (m.clone(), true),
                None => match source.default_model() {
                    Ok(m) => (m, false),
                    Err(why) => {
                        let _ = writeln!(out, "agy pick: agy's default model is unread: {why}");
                        return ExitCode::FAILURE;
                    }
                },
            };
            let pick = agy_quota::pick(&reading.quota, &model, named, &rotation);
            if json {
                let mut o = Map::new();
                o.insert("schema".to_owned(), Value::from(1u64));
                o.insert("kind".to_owned(), Value::from("agy-pick"));
                o.insert("asked".to_owned(), Value::from(model.clone()));
                o.insert("named".to_owned(), Value::from(named));
                o.insert("rotate".to_owned(), Value::from(rotation.enabled));
                o.insert("got".to_owned(), Value::from(got.word()));
                let (word, until) = match &pick {
                    Pick::Keep { .. } => ("keep", None),
                    Pick::Rotate { .. } => ("rotate", None),
                    Pick::Wait { until, .. } => ("wait", *until),
                    Pick::Unknown { .. } => ("unknown", None),
                };
                o.insert("pick".to_owned(), Value::from(word));
                o.insert(
                    "model".to_owned(),
                    Value::from(pick.model().map(str::to_owned)),
                );
                o.insert("until".to_owned(), Value::from(until));
                o.insert("why".to_owned(), Value::from(pick.describe()));
                let _ = writeln!(
                    out,
                    "{}",
                    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
                );
            } else {
                match pick.model() {
                    Some(m) => {
                        let _ = writeln!(out, "{m}");
                    }
                    None => {
                        let _ = writeln!(out, "-");
                    }
                }
                let _ = writeln!(out, "  {}", pick.describe());
            }
            if pick.model().is_some() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(3)
            }
        }
    }
}

const USAGE_CODEX_ACCOUNT: &str = "codex";

fn run_codex_usage(env: &Env, thread: &str, json: bool, out: &mut dyn Write) -> ExitCode {
    use super::spend;
    use crate::harness::codex_rollout;
    let home = codex_rollout::codex_home(
        env.codex_home.as_deref(),
        env.home.as_deref().and_then(Path::to_str),
        None,
    );
    let conversation = home
        .as_deref()
        .map(|h| codex_rollout::conversation_rollouts(h, thread));
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
    let complete = fold.as_ref().is_some_and(|f| {
        f.caught_up && unread == 0 && f.unparsed == 0 && f.long == 0 && f.untimed == 0
    });
    if json {
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
            extra.push(("rows_unparsed", Value::from(f.unparsed)));
            extra.push(("rows_oversized", Value::from(f.long)));
            extra.push(("turns_untimed", Value::from(f.untimed)));
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
    if f.unparsed > 0 || f.long > 0 || f.untimed > 0 {
        short.push_str(&format!(
            " Skipped {} malformed rows, {} oversized rows and {} untimed turns: these totals \
             are partial.",
            f.unparsed, f.long, f.untimed
        ));
    }
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
         counts).{short} \u{2248}$ is the list price of the table of {}, never a bill; \
         rate-limit windows are not read from a rollout.",
        home.display(),
        f.copied,
        prices.as_of().unwrap_or("?"),
    );
    ExitCode::SUCCESS
}

/// What one screen read says about a limit: the wall the worker's last turn ended on, read by the tab's own reader ([`aterm_phase::identify`]: the grammar the turn-end policy also decides on), and the windows the vendor painted: Claude Code's `/usage` panel ([`usage::usage_panel_windows`]).
#[derive(Debug, Clone, PartialEq)]
pub struct LimitsView {
    /// The wall, when the screen shows one.
    pub wall: Option<aterm_phase::Wall>,
    /// The wall's reset as Unix seconds, when its words place one.
    pub resets_at: Option<i64>,
    /// Each painted window, with its reset placed (`None`: words or a zone that place none).
    pub windows: Vec<(usage::PanelWindow, Option<i64>)>,
    /// Usage credits carry the session, by Claude Code's own notice ([`aterm_phase::credits::in_use`]): work going on, never a wall.
    pub credits: bool,
}

/// [`LimitsView`] of `rows`, read by `reader` (the tab's: [`aterm_phase::identify`] of its `status` `program=`), as of `now`: a reset that names no zone (or one `zone_offset_at` does not know) is at `local_offset_s`.
#[must_use]
pub fn limits_view(
    reader: &dyn aterm_phase::ScreenReader,
    rows: &[String],
    now: i64,
    local_offset_s: i64,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> LimitsView {
    let place = |text: &str| {
        limit::parse_reset(text)
            .map(|spec| limit::reset_at_with(&spec, now, local_offset_s, zone_offset_at))
    };
    let wall = reader.wall(rows);
    let resets_at = wall.as_ref().and_then(|w| {
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
    let windows = match reader.program() {
        aterm_phase::Program::Codex
        | aterm_phase::Program::Agy
        | aterm_phase::Program::Muse
        | aterm_phase::Program::Gemini => Vec::new(),
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
            | aterm_phase::Program::Gemini
            | aterm_phase::Program::Generic => false,
        },
    }
}

/// The `harness limits` line: `wall=<kind> resets_at=<rfc3339|-> source=grid windows=<name>=<pct>%@<rfc3339|->,… credits=<in-use|-> message=<words>`, or `wall=none …`.
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

/// The `harness limits --json` document, schema 2: `{schema, kind:"limits", source, wall: null | {kind, message, reset, resets_at}, windows: [{name, used_pct, reset, resets_at}], credits: "in-use" | null}`.
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

fn limits_accounts_line(env: &Env, target: &str) -> Option<String> {
    let roster_path = env.accounts_roster()?;
    let raw = Roster::load(&roster_path)
        .ok()
        .filter(|r| !r.rows.is_empty())?;
    let store = env.accounts_store();
    let roster = store.bound(&raw, env.now).unwrap_or(raw);
    let (state, _) = store.load(env.now).ok()?;
    let enrolled = accounts::enrolled(&roster);
    let sid = target.trim_start_matches('@');
    let on_acct = state
        .sessions
        .get(sid)
        .map(|s| s.account.as_str())
        .or(state.active.as_deref())
        .or_else(|| roster.row_for_login(None).map(|r| r.label.as_str()))
        .unwrap_or("-");
    let next = accounts_next(&roster, &state, &enrolled, env.now);
    let next_str = match (&next.to, next.wait_until) {
        (Some(to), _) => to.clone(),
        (None, Some(until)) => format!(
            "wait until {} (in {})",
            rfc3339_utc(until),
            super::upgrade::span(u64::try_from(until.saturating_sub(env.now)).unwrap_or(0))
        ),
        (None, None) => "none".to_string(),
    };
    let reasons: Vec<String> = next
        .not
        .iter()
        .map(|(label, why)| match why {
            accounts::WhyNot::NotEnrolled => format!(
                "{label}: not-enrolled (run `aterm harness accounts {} {label}`)",
                roster.row(label).map_or("login", sign_in_verb)
            ),
            _ => format!("{label}: {}", why_not_words(*why)),
        })
        .collect();
    if reasons.is_empty() {
        Some(format!("account={on_acct} next={next_str}"))
    } else {
        Some(format!(
            "account={on_acct} next={next_str} — not next: {}",
            reasons.join("; ")
        ))
    }
}

fn run_limits_with<C: Ctl>(
    ctl: &mut C,
    env: &Env,
    sid: Option<&str>,
    local_offset_s: i64,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let Some(target) = env.target(sid) else {
        let _ = writeln!(err, "aterm harness limits: no session named; pass @<sid>");
        return ExitCode::from(2);
    };
    let status = ctl
        .call(&[target.as_str(), "status"])
        .ok()
        .map(|r| r.stdout);
    let program = status
        .as_deref()
        .and_then(|s| crate::supervise::screen::status_field(s, "program"))
        .map(str::to_owned);
    let mut session = Session::new(ctl, Some(target.clone()));
    match session.read_screen() {
        Ok(screen) => {
            let reader = aterm_phase::identify(program.as_deref(), &screen.rows);
            let view = limits_view(
                reader,
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
            if !json
                && view.wall.is_some()
                && let Some(acct_line) = limits_accounts_line(env, &target)
            {
                let _ = writeln!(out, "{acct_line}");
            }
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
    run_limits_with(&mut ctl, env, sid, limit::tz_offset_s(), json, out, err)
}

/// The folders tier 3's clause 5 reads under `home` ([`disk::idle::link_dirs`]), atpkg's own `bin/` among them.
#[must_use]
pub fn default_link_dirs(home: &Path) -> Vec<PathBuf> {
    disk::idle::link_dirs(home, &default_store_dir(home))
}

fn default_store_dir(home: &Path) -> PathBuf {
    atpkg::store::default_prefix(home).join("store")
}

fn disk_roots(env: &Env, targets: &[PathBuf]) -> disk::Roots {
    let home = env.home.clone();
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
    let table = aterm_sysprobe::volumes::table();
    let found = if targets.is_empty() {
        let roots = disk::discover::Roots {
            cwds: vec![env.cwd.clone()],
            home: home.clone(),
            claude_dirs: claude_dirs.clone(),
            claudes: claudes.clone(),
            claude_unread: claude_unread.clone(),
            agent_target_dirs: Vec::new(),
            claude_tmp: env.claude_tmp.clone(),
            device: disk::discover::device_of(&env.cwd),
            volume: disk::target::VolumeGuid::of_path(&env.cwd, &table),
            mounts: table.points(),
            skip_home_children: false,
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
        live_link: home.as_ref().map(|h| {
            h.join(".local")
                .join("bin")
                .join(format!("claude{}", std::env::consts::EXE_SUFFIX))
        }),
        version_locks: home
            .as_ref()
            .map(|h| h.join(".local").join("state").join("claude").join("locks")),
        version_hashes: Some(env.state.join(disk::versions::HASH_FILE)),
        store_dir: home.as_ref().map(|h| default_store_dir(h)),
        targets: targets.to_vec(),
        transcripts: home.as_ref().map(|h| h.join(".claude").join("projects")),
        placing: home
            .as_deref()
            .and_then(|h| disk::discover::placing_of(h, &table)),
        mounts: Some(table),
        demand: None,
        idle_whole: targets.is_empty(),
        session_cwds: vec![env.cwd.clone()],
        session_cwd_unknown: None,
        link_dirs: home.as_deref().map(default_link_dirs).unwrap_or_default(),
    }
}

const DISK_REMOVAL_KEYS: [&str; 6] = [
    "apply",
    "target_stale_days",
    "auto_free_gib",
    "dead_build_dirs",
    "idle_build_dirs",
    "idle_build_days",
];

const DISK_OFF: &str = "so every removal is off (disk.apply=false, auto_free_gib=0, \
                        dead_build_dirs=false, idle_build_dirs=false)";

const fn removal_off(config: disk::Config) -> disk::Config {
    disk::Config {
        apply: false,
        auto_free_gib: 0,
        dead_build_dirs: false,
        idle_build_dirs: false,
        ..config
    }
}

/// What an aterm.toml says of the `[disk]` knobs ([`disk_config_read`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskReading {
    /// The knobs the verb and the window's disk watch act on: every removal off ([`removal_off`]) where the file does not say what the owner meant by a removal knob.
    pub config: disk::Config,
    /// The automatic floor the file sets, in GiB, as read before any removal was turned off (`0` where it is off, the default where it is not written).
    pub floor_gib: Option<u64>,
    /// One line for each value not read as written.
    pub notes: Vec<String>,
    /// Whether [`Self::config`] is [`removal_off`] because the file does not say what the owner meant by a removal knob: then `config.apply` is `false` whatever the file sets it to, and a note says which value.
    pub off_by_fault: bool,
    /// Which volume-scaled knobs the file writes: those are read as written, never raised to the volume's share ([`disk::Config::on_volume`]).
    pub written: disk::Written,
}

impl DiskReading {
    fn of(config: disk::Config) -> Self {
        DiskReading {
            config,
            floor_gib: Some(config.auto_free_gib),
            notes: Vec::new(),
            off_by_fault: false,
            written: disk::Written::default(),
        }
    }

    fn unread(note: String) -> Self {
        DiskReading {
            config: removal_off(disk::Config::default()),
            floor_gib: None,
            notes: vec![note],
            off_by_fault: true,
            written: disk::Written::default(),
        }
    }

    /// Whether `free` bytes is at or above the file's floor ([`Self::floor_gib`]), counted as [`disk::Config::below_auto_floor`] counts it.
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

    /// The knobs in force on a volume of `total` bytes: each the file does not write raised to the volume's share ([`disk::Config::on_volume`]).
    #[must_use]
    pub fn on_volume(&self, total: Option<u64>) -> disk::Config {
        self.config.on_volume(total, self.written)
    }

    /// [`Self::on_volume`], the lines the file does not write raised to the volume's `room` too ([`disk::Config::on_volume_with`]).
    #[must_use]
    pub fn on_volume_with(&self, total: Option<u64>, room: Option<u64>) -> disk::Config {
        self.config.on_volume_with(total, self.written, room)
    }

    /// The thresholds in force on a volume of `total` bytes ([`disk::Config::thresholds_on`]).
    #[must_use]
    pub fn thresholds(&self, total: Option<u64>) -> disk::Thresholds {
        self.config.thresholds_on(total, self.written)
    }

    /// [`Self::thresholds`], raised to the volume's `room` ([`disk::Config::thresholds_with`]).
    #[must_use]
    pub fn thresholds_with(&self, total: Option<u64>, room: Option<u64>) -> disk::Thresholds {
        self.config.thresholds_with(total, self.written, room)
    }
}

fn disk_config(env: &Env) -> DiskReading {
    disk_config_read(env.config.as_deref())
}

/// The `[disk]` knobs ([`disk::KEYS`]) of the `aterm.toml` at `path`, the floor it sets, and a note for each value not read as written ([`disk_config_from`]).
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

fn disk_config_from(text: &str) -> DiskReading {
    disk_read(text, &mut Vec::new())
}

/// A value of an aterm.toml that the `[disk]` reader ([`disk_config_from`]) does not read as written, for its type or for where TOML files it ([`disk_faults`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFault {
    /// The dotted path TOML files the value at: `disk`, `disk.apply`, `harness.disk.auto_free_gib`.
    pub path: String,
    /// The reader's note on it, as [`DiskReading::notes`] holds it.
    pub note: String,
    /// Whether it turned every removal off ([`removal_off`]).
    pub removal_off: bool,
}

/// The values of an aterm.toml's `text` that the `[disk]` reader ([`disk_config_from`]) does not read as written, each with its note and dotted path: a `disk` that is not a table, a [`disk::KEYS`] knob of another type, and one written below another table's header.
#[must_use]
pub fn disk_faults(text: &str) -> Vec<DiskFault> {
    let mut faults = Vec::new();
    disk_read(text, &mut faults);
    faults
}

fn disk_read(text: &str, faults: &mut Vec<DiskFault>) -> DiskReading {
    use aterm_toml::Value as T;
    let d = disk::Config::default();
    let doc = match aterm_toml::from_str::<T>(text) {
        Ok(doc) => doc,
        Err(e) => {
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
    let mut written = disk::Written::default();
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
                    ("idle_build_dirs", T::Boolean(on)) => cfg.idle_build_dirs = *on,
                    ("idle_build_days", T::Integer(n)) => {
                        if *n < 1 {
                            notes.push(
                                "disk.idle_build_days is below 1, which turns tier 3 off"
                                    .to_owned(),
                            );
                        }
                        cfg.idle_build_days = *n;
                    }
                    ("room_gib", T::Integer(n)) if *n >= 0 => {
                        cfg.room_gib = u64::try_from(*n).ok();
                    }
                    ("room_gib", value) => {
                        let kind = value.type_str();
                        let note = if matches!(value, T::Integer(_)) {
                            "disk.room_gib is negative, so the measured room is used".to_owned()
                        } else {
                            format!(
                                "disk.room_gib must be a whole number, not {} {kind}, so the \
                                 measured room is used",
                                article(kind)
                            )
                        };
                        if !matches!(value, T::Integer(_)) {
                            faults.push(DiskFault {
                                path: "disk.room_gib".to_owned(),
                                note: note.clone(),
                                removal_off: false,
                            });
                        }
                        notes.push(note);
                    }
                    ("warn_free_gib" | "target_stale_days" | "auto_free_gib", T::Integer(n)) => {
                        if *n < 0
                            && let Some(reading) = disk::Config::negative_reading(key)
                        {
                            notes.push(format!("disk.{key} is negative; it is read as {reading}"));
                        }
                        match key {
                            "warn_free_gib" => {
                                cfg.warn_free_gib = u64::try_from(*n).unwrap_or(d.warn_free_gib);
                                written.warn_free_gib = *n >= 0;
                            }
                            "target_stale_days" if *n >= 0 => cfg.target_stale_days = *n,
                            "target_stale_days" => {}
                            _ => {
                                cfg.auto_free_gib = u64::try_from(*n).unwrap_or(0);
                                written.auto_free_gib = true;
                            }
                        }
                    }
                    _ if !disk::KEYS.contains(&key) => notes.push(format!(
                        "disk.{key} is not a [disk] key, so it changes nothing (the keys: {})",
                        disk::KEYS.join(", ")
                    )),
                    _ => {
                        let kind = value.type_str();
                        let wanted =
                            if matches!(key, "apply" | "dead_build_dirs" | "idle_build_dirs") {
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
        written,
    }
}

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
    /// Free space is at or above the automatic floor, or unknown: nothing was scanned, journalled or removed.
    Plenty,
    /// Below the floor: the report was journalled and the automatic grant applied ([`disk::apply_auto`]), with the largest build directories the pass left ([`disk::hogs_left`]).
    Reclaimed(disk::Applied, Vec<disk::Hog>),
}

/// What one look of the window's disk watch is about: every path and figure is the host's ([`disk_tick`]).
#[derive(Debug, Clone, Copy)]
pub struct DiskLook<'a> {
    /// The harness state directory; the ledger is `<state>/disk.jsonl`.
    pub state: &'a Path,
    /// The volume the free figure is about.
    pub volume: &'a Path,
    /// Its free bytes, measured by the host (the statvfs edge lives in `atpkg`); `None` fails open.
    pub free: Option<u64>,
    /// Build directories named by the host.
    pub targets: &'a [PathBuf],
    /// Where to search for build directories below the floor ([`disk::discover::discover`]): the agents' working directories and their worktrees, `$HOME`'s checkouts, the agents' `CARGO_TARGET_DIR`, Claude Code's temp root.
    pub discover: Option<&'a disk::discover::Roots>,
    /// Where transcripts live: nothing under it is ever removable.
    pub transcripts: Option<&'a Path>,
    /// Unix seconds.
    pub now: i64,
    /// The volume's room as the watch measured it ([`disk::room`]): carried into the journal's report row.
    pub room: Option<u64>,
    /// The mount table the look read ([`aterm_sysprobe::volumes::table`]): a whole removal needs it read whole and no mount point inside its directory, and a workspace nested in a removed checkout is placed against it.
    pub table: Option<&'a aterm_sysprobe::volumes::Table>,
    /// The need of an agent's wait the pass adopted ([`disk::Survey::demand`]).
    pub demand: Option<u64>,
    /// The working directory of every session of this instance, rostered or not: tier 3 never takes a build directory one of them works in.
    pub session_cwds: &'a [PathBuf],
    /// Why the working directory of some session of this instance is not known (on Windows a live tab that reported no absolute native one): while it stands tier 3 takes nothing.
    pub session_cwd_unknown: Option<&'a str>,
    /// The folders whose links tier 3 reads ([`disk::idle::link_dirs`]).
    pub link_dirs: &'a [PathBuf],
}

/// One pass of the window's disk watch (design §5.5's timer, a look every minute, hosted by `aterm-gui`'s harness host): below the automatic floor ([`disk::Config::below_auto_floor`], `[disk] auto_free_gib`), survey the look's targets, journal the report into `<state>/disk.jsonl`, and reclaim what [`disk::auto_plan`] grants through `remove` ([`disk::remover`] is the real one: a profile's `incremental/`, under cargo's locks; every idle one, then recent ones least recently used first, `measure` read again before each of those and the pass stopped once free space is back at [`disk::Config::pressure_target_bytes`]), journalling an intent row before each removal and its outcome (bytes actually released, what went, what was skipped, every delete error) or a denial row after, each as it happens.
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
        room,
        table,
        demand,
        session_cwds,
        session_cwd_unknown,
        link_dirs,
    } = *look;
    let admitted =
        config.auto_free_gib > 0 && free.is_some() && (config.below_warn(free) || demand.is_some());
    if !admitted {
        return DiskTick::Plenty;
    }
    let _exposed = disk::discover::expose_placeholders().ok();
    let found = discover.map_or_else(disk::discover::Discovered::default, |r| {
        disk::discover::discover(r, disk::discover::Budget::SHIPPED)
    });
    let unfinished = found.cut.clone();
    let paths: Vec<PathBuf> = found.found.iter().map(|f| f.path.clone()).collect();
    let fresh = (cfg!(windows) && table.is_none()).then(aterm_sysprobe::volumes::table);
    let ids = table.or(fresh.as_ref());
    let volume_id = |path: &Path| volume_id(path, ids);
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
        mounts: table.cloned(),
        placing: discover
            .and_then(|r| r.home.as_deref())
            .zip(table)
            .and_then(|(home, t)| disk::discover::placing_of(home, t)),
        demand,
        idle_whole: config.idle_whole_on() && (config.below_auto_floor(free) || demand.is_some()),
        session_cwds: session_cwds.to_vec(),
        session_cwd_unknown: session_cwd_unknown.map(str::to_owned),
        link_dirs: link_dirs.to_vec(),
        ..disk::Roots::default()
    };
    let mut survey = disk::scan(&roots, now, disk::Trigger::Tick, config, free);
    survey.room = room;
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
        &mut |step| {
            disk::beat::beat();
            journal.append_raw(&disk::step_row(now, "", step), &mut io::sink())
        },
    )
    .unwrap_or_default();
    let done = disk::Applied { unfinished, ..done };
    let there = |p: &Path| {
        !std::fs::symlink_metadata(p).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
    };
    let hogs = disk::hogs_left(&survey, &rep, &done, &there);
    DiskTick::Reclaimed(done, hogs)
}

/// What the reserve did on one look of the disk watch ([`disk_reserve_look`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveMove {
    /// Nothing to do: there, or not wanted at this free figure.
    Kept,
    /// Written now, free space being well above the target.
    Written,
    /// Released at an emergency: its bytes unlinked.
    Released(u64),
    /// Something not ours in its place, or the file system refused: why.
    Refused(String),
    /// Refused again for the reason last journalled for this reserve: nothing written to the ledger, nothing for the log.
    StillRefused,
}

/// How long after a failed write of the reserve ([`disk::reserve::ReserveError::Unwritten`]) the watch writes it again: an hour.
pub const RESERVE_REWRITE_AFTER_SECS: i64 = 3600;

struct ReserveRefused {
    reserve: PathBuf,
    why: String,
    unwritten_at: Option<i64>,
}

static RESERVE_REFUSED: std::sync::Mutex<Vec<ReserveRefused>> = std::sync::Mutex::new(Vec::new());

fn reserve_refusal_news(reserve: &Path, why: Option<&str>, unwritten_at: Option<i64>) -> bool {
    let mut said = RESERVE_REFUSED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let at = said.iter().position(|r| r.reserve == reserve);
    match (why, at) {
        (None, Some(i)) => {
            said.swap_remove(i);
            false
        }
        (None, None) => false,
        (Some(why), Some(i)) => {
            let r = &mut said[i];
            r.unwritten_at = unwritten_at;
            if r.why == why {
                false
            } else {
                why.clone_into(&mut r.why);
                true
            }
        }
        (Some(why), None) => {
            said.push(ReserveRefused {
                reserve: reserve.to_path_buf(),
                why: why.to_owned(),
                unwritten_at,
            });
            true
        }
    }
}

fn reserve_write_waits(reserve: &Path, now: i64) -> bool {
    RESERVE_REFUSED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|r| r.reserve == reserve)
        .and_then(|r| r.unwritten_at)
        .is_some_and(|at| now < at.saturating_add(RESERVE_REWRITE_AFTER_SECS))
}

/// THE RESERVE'S MOVE on one look ([`disk::reserve`]): at [`disk::Level::Emergency`] the reserve at `reserve` is UNLINKED FIRST and then the `released` row is appended to `<state>/disk.jsonl` — the unlink needs no space, and the room it made is what lets that row and the pass's intent rows be written (at a true `ENOSPC` they could not be, and the write-ahead rule removed nothing: [`disk::Refusal::Unjournalled`]).
pub fn disk_reserve_look(
    state: &Path,
    reserve: &Path,
    level: disk::Level,
    free: Option<u64>,
    thresholds: &disk::Thresholds,
    now: i64,
) -> ReserveMove {
    if level == disk::Level::Unknown {
        return ReserveMove::Kept;
    }
    let _exposed = disk::discover::expose_placeholders().ok();
    let mut unwritten_at = None;
    let moved = if level == disk::Level::Emergency {
        match disk::reserve::release(reserve) {
            Ok(Some(bytes)) => ReserveMove::Released(bytes),
            Ok(None) => return ReserveMove::Kept,
            Err(e) => ReserveMove::Refused(e.to_string()),
        }
    } else if free.is_some_and(|f| {
        f >= thresholds
            .target
            .max(thresholds.floor)
            .saturating_add(disk::reserve::RESERVE_BYTES)
    }) {
        if reserve_write_waits(reserve, now) {
            return ReserveMove::StillRefused;
        }
        match disk::reserve::ensure(reserve, disk::reserve::RESERVE_BYTES) {
            Ok(true) => ReserveMove::Written,
            Ok(false) => {
                reserve_refusal_news(reserve, None, None);
                return ReserveMove::Kept;
            }
            Err(e) => {
                if matches!(e, disk::reserve::ReserveError::Unwritten(_)) {
                    unwritten_at = Some(now);
                }
                ReserveMove::Refused(e.to_string())
            }
        }
    } else {
        return ReserveMove::Kept;
    };
    match &moved {
        ReserveMove::Refused(why) => {
            if !reserve_refusal_news(reserve, Some(why), unwritten_at) {
                return ReserveMove::StillRefused;
            }
        }
        _ => {
            reserve_refusal_news(reserve, None, None);
        }
    }
    let row = match &moved {
        ReserveMove::Released(bytes) => disk::reserve_row(
            now,
            "released",
            Some(*bytes),
            &format!(
                "the disk is full: the reserve {} was unlinked first, so this journal can be \
                 written",
                reserve.display()
            ),
        ),
        ReserveMove::Written => disk::reserve_row(
            now,
            "written",
            Some(disk::reserve::RESERVE_BYTES),
            &format!("the reserve {} was written", reserve.display()),
        ),
        ReserveMove::Refused(why) => disk::reserve_row(now, "refused", None, why),
        ReserveMove::Kept => return ReserveMove::Kept,
        ReserveMove::StillRefused => return ReserveMove::StillRefused,
    };
    let path = state.join(DISK_LEDGER);
    let mut sink = io::sink();
    if std::fs::create_dir_all(state).is_ok() {
        let mut journal = Journal::open(Some(&path), None, &mut sink);
        journal.append_raw(&row, &mut sink);
    }
    moved
}

/// Journal a poke ([`disk::poke`]) into `<state>/disk.jsonl`: what saw the disk full, the free figure measured on it and where that stands.
pub fn disk_poke_journal(
    state: &Path,
    cause: &disk::poke::Cause,
    free: Option<u64>,
    level: disk::Level,
    now: i64,
) {
    let path = state.join(DISK_LEDGER);
    let mut sink = io::sink();
    if std::fs::create_dir_all(state).is_ok() {
        let mut journal = Journal::open(Some(&path), None, &mut sink);
        journal.append_raw(&disk::poke_row(now, cause, free, level), &mut sink);
    }
}

/// What `aterm harness disk` says of the window's disk WATCH ([`disk_watch_status`]): one line, and the same as a JSON object (the `--json` form's second line, `kind: disk-watch`).
pub struct DiskWatchStatus {
    pub line: String,
    pub json: Value,
}

/// The disk watch as it stands for the volume `volume`: the thresholds in force there ([`disk::Config::thresholds`], the knob or the volume's share), where the volume stands against them, the reserve ([`disk::reserve`] under `home`), and the last `poke` and `reserve` rows of the ledger at `ledger`.
#[must_use]
pub fn disk_watch_status(
    ledger: &Path,
    home: Option<&Path>,
    volume: Option<&Path>,
    config: disk::Config,
    written: disk::Written,
    now: i64,
) -> DiskWatchStatus {
    let table = aterm_sysprobe::volumes::table();
    let stats = volume.and_then(|v| disk::volume_of(v, &table).ok());
    let stats = stats.as_ref();
    let room = stats
        .zip(ledger.parent())
        .map(|(r, state)| disk::room::kept_room(state, r, now));
    let t = config.thresholds_with(stats.map(|s| s.total), written, room);
    let level = t.level(stats.map(|s| s.free), stats.and_then(|s| s.files_free));
    let reserve = home.map(disk::reserve::path_for);
    let reserve_state = match reserve.as_deref().map(disk::reserve::state) {
        None => "unknown".to_owned(),
        Some(Ok(disk::reserve::State::Present(bytes))) => {
            format!("present ({})", disk::human_bytes(bytes))
        }
        Some(Ok(disk::reserve::State::Absent)) => "absent".to_owned(),
        Some(Err(e)) => format!("refused: {e}"),
    };
    let mut last_poke = Value::Null;
    let mut last_reserve = Value::Null;
    for line in std::fs::read_to_string(ledger)
        .unwrap_or_default()
        .lines()
        .rev()
        .take(DISK_LEDGER_KEEP_ROWS)
    {
        let Ok(row) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        match row["kind"].as_str() {
            Some("poke") if last_poke.is_null() => last_poke = row,
            Some("reserve") if last_reserve.is_null() => last_reserve = row,
            _ => {}
        }
        if !last_poke.is_null() && !last_reserve.is_null() {
            break;
        }
    }
    let human = |b: u64| disk::human_bytes(b);
    let line = format!(
        "watch: disk={} free={} of {} room={} (warn {}, floor {}, back at {}, emergency {}){} reserve={}",
        level.as_str(),
        stats.map_or_else(|| "unknown".to_owned(), |s| human(s.free)),
        stats.map_or_else(|| "unknown".to_owned(), |s| human(s.total)),
        room.map_or_else(|| "unknown".to_owned(), human),
        human(t.warn),
        human(t.floor),
        human(t.target),
        human(t.emergency),
        stats
            .and_then(|s| s.files_free)
            .map_or_else(String::new, |f| format!(" inodes_free={f}")),
        reserve_state,
    );
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("kind".to_owned(), Value::from("disk-watch".to_owned()));
    o.insert("level".to_owned(), Value::from(level.as_str().to_owned()));
    let num = |v: Option<u64>| v.map_or(Value::Null, Value::from);
    o.insert("free_bytes".to_owned(), num(stats.map(|s| s.free)));
    o.insert("total_bytes".to_owned(), num(stats.map(|s| s.total)));
    o.insert("room_bytes".to_owned(), num(room));
    o.insert(
        "files_free".to_owned(),
        num(stats.and_then(|s| s.files_free)),
    );
    o.insert("warn_bytes".to_owned(), Value::from(t.warn));
    o.insert("floor_bytes".to_owned(), Value::from(t.floor));
    o.insert("target_bytes".to_owned(), Value::from(t.target));
    o.insert("emergency_bytes".to_owned(), Value::from(t.emergency));
    o.insert("files_emergency".to_owned(), Value::from(t.files_emergency));
    o.insert("reserve".to_owned(), Value::from(reserve_state));
    o.insert(
        "reserve_path".to_owned(),
        reserve.map_or(Value::Null, |p| Value::from(p.display().to_string())),
    );
    o.insert("last_poke".to_owned(), last_poke);
    o.insert("last_reserve".to_owned(), last_reserve);
    DiskWatchStatus {
        line,
        json: Value::Object(o),
    }
}

/// Journal one build directory of aterm's Rust build cache the sweep (`atpkg::build_cache::reclaim_watched`, the watch's pass with no session supervised) is about to remove (`removed` false: the write-ahead intent row) or removed, `bytes` in it, and `why` it goes, into `<state>/disk.jsonl`.
pub fn disk_sweep_journal(
    state: &Path,
    path: &Path,
    bytes: u64,
    why: atpkg::build_cache::Unused,
    removed: bool,
    now: i64,
) -> bool {
    let ledger = state.join(DISK_LEDGER);
    let mut sink = io::sink();
    if std::fs::create_dir_all(state).is_err() {
        return false;
    }
    let mut journal = Journal::open(Some(&ledger), None, &mut sink);
    let action = if removed { "removed" } else { "removing" };
    journal.append_raw(&disk::sweep_row(now, path, action, bytes, why), &mut sink)
}

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

fn volume_id(
    path: &Path,
    table: Option<&aterm_sysprobe::volumes::Table>,
) -> Option<(u64, Option<disk::target::VolumeGuid>)> {
    let dev = aterm_sysprobe::volumes::file_id(path, true)?.dev;
    if cfg!(windows) {
        let guid = disk::target::VolumeGuid::of_path(path, table?)?;
        return Some((dev, Some(guid)));
    }
    Some((dev, None))
}

#[cfg(test)]
thread_local! {
    static DISK_ERRORS_QUIET: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

fn run_disk(
    env: &Env,
    targets: &[PathBuf],
    class: Option<disk::Class>,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let _quiet = aterm_sysprobe::quiet_errors();
    #[cfg(test)]
    DISK_ERRORS_QUIET.set(Some(aterm_sysprobe::errors_quiet()));
    let _exposed = disk::discover::expose_placeholders().ok();
    let roots = disk_roots(env, targets);
    let DiskReading {
        config,
        notes,
        off_by_fault,
        written,
        ..
    } = disk_config(env);
    for note in &notes {
        let _ = writeln!(err, "aterm harness disk: {note}");
    }
    let table = aterm_sysprobe::volumes::table();
    let reading = roots
        .volume
        .as_deref()
        .and_then(|v| disk::volume_of(v, &table).ok());
    let free = reading.as_ref().map(|r| r.free);
    let total = reading.as_ref().map(|r| r.total);
    let room = reading
        .as_ref()
        .zip(env.disk_ledger().parent())
        .map(|(r, state)| disk::room::kept_room(state, r, env.now));
    let in_force = config.on_volume_with(total, written, room);
    let mut survey = disk::scan(&roots, env.now, disk::Trigger::OnDemand, in_force, free);
    survey.room = room;
    let rep = disk::report(&survey, in_force);
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

    let judge = disk::Judge {
        now: env.now,
        threshold_days: config.target_stale_days,
        device: None,
        volume: reading
            .as_ref()
            .and_then(|r| disk::target::VolumeGuid::of_key(&r.key)),
    };
    let volume = roots.volume.clone();
    let sessions = disk::idle::Sessions {
        cwds: roots.session_cwds.clone(),
        unknown: roots.session_cwd_unknown.clone(),
    };
    let done = class.map(|_| {
        disk::apply(
            &rep,
            survey.transcripts_root.as_deref(),
            class,
            &mut disk::remover(judge, || sessions.clone()),
            &mut || {
                volume
                    .as_deref()
                    .and_then(|v| disk::volume_of(v, &table).ok())
                    .map(|r| r.free)
            },
            &mut |step| journal.append_raw(&disk::step_row(env.now, &env.sid, step), err),
        )
    });

    let watch = disk_watch_status(
        &path,
        env.home.as_deref(),
        roots.volume.as_deref(),
        config,
        written,
        env.now,
    );
    if json {
        let _ = writeln!(out, "{}", rep.to_json());
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&watch.json).unwrap_or_default()
        );
        if let Some(done) = &done {
            let mut o = Map::new();
            o.insert("schema".to_owned(), Value::from(1u64));
            o.insert("kind".to_owned(), Value::from("disk-apply".to_owned()));
            o.insert(
                "removed".to_owned(),
                Value::from(u64::try_from(done.removed.len()).unwrap_or(u64::MAX)),
            );
            o.insert("freed_bytes".to_owned(), Value::from(done.freed_bytes));
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
    let _ = writeln!(out, "  {}", watch.line);
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
        let _ = writeln!(out, "  note: {}", disk::shown(note));
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
    let remedy = if off_by_fault {
        format!("fix what the stderr note names in {config_file}")
    } else {
        format!("set `[disk] apply = true` in {config_file}")
    };
    match &done {
        None => {
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
                let line = format!(
                    "  removed {} — {}; freed about {}",
                    row.path.display(),
                    row.witness.describe(),
                    disk::human_bytes(removed.bytes)
                );
                let _ = writeln!(out, "{}", disk::shown(&line));
                for unit in &removed.units {
                    let _ = writeln!(
                        out,
                        "    went: {}",
                        disk::shown(&unit.display().to_string())
                    );
                }
                for (unit, why) in &removed.skipped {
                    let line = format!("    kept: {} — {why}", unit.display());
                    let _ = writeln!(out, "{}", disk::shown(&line));
                }
                for trouble in &removed.trouble {
                    let _ = writeln!(out, "    trouble: {}", disk::shown(trouble));
                }
            }
            for refusal in &done.denials {
                let fix = if matches!(refusal, disk::Refusal::ApplyOff(_)) {
                    format!("; {remedy}")
                } else {
                    String::new()
                };
                let _ = writeln!(out, "  denied: {}{fix}", disk::shown(&refusal.describe()));
            }
            if let Some(stop) = &done.stopped {
                let _ = writeln!(out, "  stopped: {}", disk::shown(&stop.describe()));
            }
        }
    }
    ExitCode::SUCCESS
}

/// The newest `max` lines of the JSONL file at `path`, streamed through a ring of at most [`READ_MAX_ROWS`] so the file is never held whole.
pub fn tail_lines(path: &Path, max: usize) -> io::Result<Vec<String>> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let keep = max.min(READ_MAX_ROWS);
    let mut ring: VecDeque<String> = VecDeque::with_capacity(keep.saturating_add(1));
    if keep == 0 {
        return Ok(Vec::new());
    }
    for line in io::BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        ring.push_back(line);
        while ring.len() > keep {
            ring.pop_front();
        }
    }
    Ok(ring.into_iter().collect())
}

/// Cut the journal at `path` back to its newest `keep` rows once it is larger than `max_bytes`.
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
    replace_ledger_rows(path, &rows)?;
    Ok(true)
}

/// The rows a cut keeps of a ledger ([`bound_ledger_cut`]), handed its rows in order (the cut must read them all), the files beside it read by the reader it is handed; `None` keeps the ledger as it is.
pub type LedgerCut<'a> = dyn Fn(&mut dyn Iterator<Item = String>, &dyn Fn(&Path) -> Option<String>) -> Option<Vec<String>>
    + 'a;

/// [`bound_ledger`] with the rows kept chosen by `cut` from the file's rows (the approval ledger's cut, `approvals::bound`, which folds the loop's records and keeps the rows of the states that stand), the files beside it read by the reader `cut` is handed.
pub fn bound_ledger_cut(path: &Path, max_bytes: u64, cut: &LedgerCut<'_>) -> io::Result<bool> {
    bound_ledger_cut_with_cap(path, max_bytes, approvals::HELD_BYTES as u64, cut)
}

fn bound_ledger_cut_with_cap(
    path: &Path,
    max_bytes: u64,
    input_cap: u64,
    cut: &LedgerCut<'_>,
) -> io::Result<bool> {
    use std::io::Read as _;

    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if len <= max_bytes {
        return Ok(false);
    }
    let Ok((file, before)) = open_regular(path) else {
        return Ok(false);
    };
    let read_bytes = std::cell::Cell::new(0_u64);
    let broken = std::cell::Cell::new(false);
    let mut reader = io::BufReader::new(&file);
    let mut rows = std::iter::from_fn(|| {
        let mut line = String::new();
        match (&mut reader)
            .take(input_cap.saturating_add(1))
            .read_line(&mut line)
        {
            Ok(0) => None,
            Ok(n) if n as u64 > input_cap => {
                broken.set(true);
                None
            }
            Ok(n) => {
                read_bytes.set(read_bytes.get().saturating_add(n as u64));
                if line.ends_with('\n') {
                    line.pop();
                    if line.ends_with('\r') {
                        line.pop();
                    }
                }
                Some(line)
            }
            Err(_) => {
                broken.set(true);
                None
            }
        }
    });
    let left = std::cell::Cell::new(input_cap);
    let complete = std::cell::Cell::new(true);
    let rows = cut(&mut rows, &|p| match read_complete_text(p, left.get()) {
        Ok(text) => {
            left.set(left.get().saturating_sub(text.len() as u64));
            Some(text)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(_) => {
            complete.set(false);
            None
        }
    });
    let Some(rows) = rows else {
        return Ok(false);
    };
    if !complete.get() || broken.get() || !unchanged(&file, &before, read_bytes.get())? {
        return Ok(false);
    }
    replace_ledger_rows(path, &rows)?;
    Ok(true)
}

fn open_regular(path: &Path) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    Ok((file, before))
}

fn unchanged(file: &std::fs::File, before: &std::fs::Metadata, read: u64) -> io::Result<bool> {
    let after = file.metadata()?;
    let same = before.len() == after.len()
        && before.modified()? == after.modified()?
        && read == before.len();
    #[cfg(unix)]
    let same = {
        use std::os::unix::fs::MetadataExt as _;
        same && before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
    };
    Ok(same)
}

/// Read one complete, capped regular file from its held descriptor.
pub(super) fn read_complete_text(path: &Path, cap: u64) -> io::Result<String> {
    use std::io::Read as _;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "incomplete ledger input");
    let (file, before) = open_regular(path)?;
    if before.len() > cap {
        return Err(invalid());
    }
    let mut text = String::new();
    (&file)
        .take(cap.saturating_add(1))
        .read_to_string(&mut text)?;
    if unchanged(&file, &before, text.len() as u64)? {
        Ok(text)
    } else {
        Err(invalid())
    }
}

/// Replace the ledger at `path` with `rows` as [`bound_ledger`] does: written to a sibling `.tmp` file (`0600`), ordered and renamed over it, so a reader sees the old file or the new one and never half of either.
pub fn replace_ledger_rows(path: &Path, rows: &[String]) -> io::Result<()> {
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
    for row in rows {
        f.write_all(row.as_bytes())?;
        f.write_all(b"\n")?;
    }
    aterm_durable::ordered(&f.into_inner().map_err(io::IntoInnerError::into_error)?)?;
    std::fs::rename(&tmp, path)
}

fn ledger_path(env: &Env, file: &LedgerFile) -> Result<PathBuf, String> {
    match file {
        LedgerFile::Disk => Ok(env.disk_ledger()),
        LedgerFile::Accounts => Ok(env.accounts_store().ledger_path()),
        LedgerFile::Upgrade => Ok(super::upgrade_drive::ledger_path(&upgrade_opts(
            env,
            "",
            &SupervisorConfig::default(),
            super::tap::stable_exe,
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

const GAVE_UP_DETAIL_BYTES: usize = 8 * 1024;

const _: () = assert!(
    512 + super::upgrade::HELD_NAMED * 4 * (32 + super::upgrade::HELD_COMMAND_CHARS + 32) + 32
        <= GAVE_UP_DETAIL_BYTES
);

fn upgrade_ledger_line(row: &str) -> String {
    let Ok(doc) = aterm_json::from_str::<Value>(row) else {
        return row.to_string();
    };
    let s = |k: &str| doc.get(k).and_then(Value::as_str).unwrap_or("-");
    let ts = doc
        .get("t")
        .and_then(Value::as_i64)
        .map_or_else(|| "-".to_string(), rfc3339_utc);
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

/// The step a hand-run `upgrade` answers while `[harness] enabled` reads off ([`upgrade_pass`]), kept stable so scripts that key on it still read it.
pub const UPGRADE_BYPASSED: &str = "refused:bypassed";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowOff {
    Harness,
    Agent(aterm_phase::Program),
    Upgrade,
}

impl WindowOff {
    fn switch(self) -> String {
        match self {
            WindowOff::Harness => "[harness] enabled".to_string(),
            WindowOff::Agent(p) => format!("[harness.{}] enabled", p.name()),
            WindowOff::Upgrade => "[harness] upgrade".to_string(),
        }
    }

    fn upgrades(self) -> String {
        match self {
            WindowOff::Harness | WindowOff::Upgrade => "these upgrades".to_string(),
            WindowOff::Agent(p) => format!("{}'s upgrades", p.product()),
        }
    }
}

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
        token: None,
        only_sid: None,
        dry_run: set.is_none(),
        human_grace_s: 0,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: accounts::several_listed(env.accounts_roster().as_deref()),
        usage_credits: SupervisorConfig::from_path(env.config.as_deref())
            .0
            .usage_credits,
        usage_tap: None,
        instances: crate::harness::upgrade_drive::Instances::Machine,
        agents_off: Vec::new(),
        land: false,
        timings: super::Timings::for_build(),
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

fn run_upgrade(
    env: &Env,
    sid: &str,
    dry_run: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let (policy, notes) = SupervisorConfig::from_path(env.config.as_deref());
    let opts = match upgrade_opts(env, sid, &policy, super::tap::stable_exe) {
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

/// The options of an `aterm harness upgrade` pass under `policy`, for session `sid` (every
/// session where empty), its relaunch lines offered the usage tap for the aterm `exe` names
/// ([`super::tap::Armed::arm`], the block the window's lane arms by too).
fn upgrade_opts(
    env: &Env,
    sid: &str,
    policy: &SupervisorConfig,
    exe: impl FnOnce() -> Option<PathBuf>,
) -> Result<super::upgrade_drive::Opts, String> {
    let home = env
        .home
        .clone()
        .ok_or("upgrade needs the home directory (Claude's sessions live under it)")?;
    Ok(super::upgrade_drive::Opts {
        home,
        state: env.state.clone(),
        sock: env.sock.clone(),
        token: None,
        only_sid: (!sid.is_empty()).then(|| sid.to_string()),
        dry_run: false,
        human_grace_s: policy.human_grace_s,
        hand_back: false,
        background: false,
        aterm_state: env.aterm_state.clone(),
        ask: policy.upgrade_ask,
        several_logins: accounts::several_listed(env.accounts_roster().as_deref()),
        usage_credits: policy.usage_credits,
        usage_tap: super::tap::Armed::arm(
            policy.usage_tap_on(),
            exe,
            env.accounts_roster().as_deref(),
            &env.accounts_store(),
            Some(env.now),
        ),
        instances: crate::harness::upgrade_drive::Instances::Machine,
        agents_off: policy.off.clone(),
        land: false,
        timings: super::Timings::for_build(),
    })
}

fn run_upgrade_owner(
    env: &Env,
    sid: &str,
    ask: OwnerAsk,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let (policy, _) = SupervisorConfig::from_path(env.config.as_deref());
    let opts = match upgrade_opts(env, sid, &policy, super::tap::stable_exe) {
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
            let (rows, whole) =
                super::upgrade_drive::status_rows(&opts, policy.enabled && policy.upgrade);
            let rows: Vec<_> = rows
                .into_iter()
                .filter(|r| r.phase != super::upgrade::Phase::Done)
                .collect();
            for row in &rows {
                print(out, row);
            }
            if !whole {
                let _ = writeln!(
                    err,
                    "aterm harness: upgrade status is incomplete: upgrade records or agent \
                     session files could not be read whole, so upgrades may be missing or stale"
                );
            }
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
            if whole && rows.is_empty() && !json {
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
            return if whole {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            };
        }
        OwnerAsk::Now => super::upgrade_drive::Ask::Now,
        OwnerAsk::Defer(secs) => super::upgrade_drive::Ask::Defer(secs),
        OwnerAsk::Skip => super::upgrade_drive::Ask::Skip,
    };
    match super::upgrade_drive::ask(&opts, sid, word, policy.enabled && policy.upgrade) {
        Ok(row) => {
            print(out, &row);
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

fn upgrade_line(out: &mut dyn Write, json: bool, r: &super::upgrade_drive::Report) {
    upgrade_line_with(out, json, r, None);
}

fn upgrade_line_with(
    out: &mut dyn Write,
    json: bool,
    r: &super::upgrade_drive::Report,
    recorded: Option<(&super::upgrade_drive::Row, u64)>,
) {
    let ages = recorded.map(|(row, now)| {
        (
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

/// What `accounts use` says once the owner's word is recorded.
pub const ACCOUNTS_USE_RECORDED: &str = "recorded: unless `[harness] accounts = \"off\"`, the \
                                         supervisor moves Claude Code sessions to it at their next \
                                         turn end, and continues one waiting at its limit";

const ACCOUNTS_ADD_LINE: &str = "aterm harness accounts add <label>";

/// The label the first `accounts add` gives this machine's own login (the row with no `dir`), so the login every session already runs on is in the roster: a session whose login no row names is never advised to move ([`accounts::StayWhy::NotListed`]).
pub const ACCOUNTS_OWN_LABEL: &str = "main";

/// How long one `claude auth status --json` may run ([`auth_status_of`]).
pub const AUTH_CHECK_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// The most of that command's stdout that is read.
pub const MAX_AUTH_STDOUT: usize = 64 * 1024;

/// What `accounts login` and `renew` need of the machine beyond files: their `claude` runs (a sign-in flow, and the check after one that exits 0), a renewal's `ssh`, whether stdin is a terminal and whether this runs over SSH, this executable, and the clock once a flow has ended.
pub trait AccountsHost {
    /// Whether stdin is a terminal.
    fn stdin_is_terminal(&self) -> bool;

    /// Run `claude auth status --json` with [`accounts::LOGIN_ENV`] set to `login` (a row's `dir` verbatim, or empty for this machine's own login), bounded, and hand back its exit and stdout.
    fn auth_status(&self, login: &str) -> Result<Captured, String>;

    /// Why no login can be asked here whether it is signed in ([`AUTH_CHECK_NO_USER`]); `None` where one can.
    fn unaskable(&self) -> Option<String>;

    /// Run `claude auth login` on this terminal and wait for it, with [`accounts::HOME_ENV`] set to `dir` — or, for `None`, this machine's own login in the default home, with that variable unset.
    fn login(&self, dir: Option<&str>) -> Result<i32, String>;

    /// Unix seconds, read when it is asked.
    fn now(&self) -> i64;

    /// A renewal's sign-in ([`run_accounts_renew`]): `claude auth login --claudeai --email <email>` on this terminal, waited for, with [`accounts::HOME_ENV`] set to `home` (the new staging directory) and [`accounts::LOGIN_ENV`] removed, so the new login is that directory's own keychain item and no live session's ([`renew_command`]).
    fn renew_flow(&self, home: &str, email: &str, browser: &RenewBrowser) -> Result<i32, String> {
        let _ = (home, email, browser);
        Err("this host renews nothing".to_string())
    }

    /// The check after a renewal's flow exited 0: `claude auth status --json` in the staging home ([`accounts::HOME_ENV`] set to `home`, [`accounts::LOGIN_ENV`] removed), bounded, its exit and stdout handed back.
    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        let _ = home;
        Err("this host checks nothing".to_string())
    }

    /// `ssh -t <host> <aterm> harness accounts renew --due --here` (`argv` after the host), its stdio this process's, waited for: the remote machine's own renewal, run in the same sitting.
    fn ssh(&self, host: &str, argv: &[String]) -> Result<i32, String> {
        let _ = (host, argv);
        Err("this host runs no ssh".to_string())
    }

    /// Whether this command runs over SSH (`SSH_CONNECTION` set): the browser is on the machine the owner is at, so nothing is opened here and Claude Code's own paste-code flow is used ([`RenewBrowser::Remote`]).
    fn under_ssh(&self) -> bool {
        false
    }

    /// This program's own executable, for the opener's argv0 link ([`open_url::OPENER`](super::open_url::OPENER)).
    fn current_exe(&self) -> Option<PathBuf> {
        None
    }
}

/// What opens a renewal's sign-in page ([`AccountsHost::renew_flow`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenewBrowser {
    /// Claude Code's own opener: the system's default browser.
    Default,
    /// The account's own browser profile, through aterm's argv0 opener (`BROWSER=<opener>`, the profile's name in [`super::open_url::PROFILE_ENV`]).
    Profile {
        /// The opener's link ([`super::open_url::OPENER`]).
        opener: PathBuf,
        /// The profile's display name.
        profile: String,
    },
    /// Over SSH: nothing opens on this machine.
    Remote {
        /// The opener's link ([`super::open_url::OPENER`]), where one could be made.
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
        let user_home = std::env::var_os("HOME").map(PathBuf::from);
        let prog = user_home
            .as_deref()
            .map(super::accounts_sync::resolve_claude_program)
            .unwrap_or_else(|| PathBuf::from("claude"));
        auth_status_of(prog.as_os_str(), login, &|name| std::env::var_os(name))
    }

    fn unaskable(&self) -> Option<String> {
        auth_check_env(&|name| std::env::var_os(name)).err()
    }

    fn login(&self, dir: Option<&str>) -> Result<i32, String> {
        let user_home = std::env::var_os("HOME").map(PathBuf::from);
        let prog = user_home
            .as_deref()
            .map(super::accounts_sync::resolve_claude_program)
            .unwrap_or_else(|| PathBuf::from("claude"));
        login_flow_of(prog.as_os_str(), dir)
    }

    fn now(&self) -> i64 {
        crate::supervise::limit::unix_now()
    }

    fn renew_flow(&self, home: &str, email: &str, browser: &RenewBrowser) -> Result<i32, String> {
        let user_home = std::env::var_os("HOME").map(PathBuf::from);
        let prog = user_home
            .as_deref()
            .map(super::accounts_sync::resolve_claude_program)
            .unwrap_or_else(|| PathBuf::from("claude"));
        renew_flow_of(prog.as_os_str(), home, email, browser)
    }

    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        let user_home = std::env::var_os("HOME").map(PathBuf::from);
        let prog = user_home
            .as_deref()
            .map(super::accounts_sync::resolve_claude_program)
            .unwrap_or_else(|| PathBuf::from("claude"));
        status_in_home_within(
            prog.as_os_str(),
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

/// `<program> auth login --claudeai --email <email>` as a renewal runs it ([`AccountsHost::renew_flow`]): the login made under the staging home alone ([`accounts::HOME_ENV`] = `home`) with [`accounts::LOGIN_ENV`] removed (set, it would name the keychain item instead, and the new login would land in a live session's), and the page opened as `browser` says: the account's profile through the argv0 opener, Claude Code's default, or, over SSH, nothing (the opener with no profile: the paste-code flow).
pub(super) fn renew_command(
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

fn renew_flow_of(
    program: &std::ffi::OsStr,
    home: &str,
    email: &str,
    browser: &RenewBrowser,
) -> Result<i32, String> {
    terminal_flow(
        || {
            let mut cmd = renew_command(program, home, email, browser);
            cmd.env(super::open_url::EMAIL_ENV, email);
            if let Some(harness_state) = Path::new(home)
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
            {
                let store = accounts::Store::at(harness_state);
                let user_home = std::env::var_os("HOME").map(PathBuf::from);
                if std::env::var_os(super::open_url::MAILBOX_ENV).is_none_or(|v| v.is_empty())
                    && let Some(mailbox) =
                        super::accounts_sync::resolve_mailbox_dir(user_home.as_deref(), &store)
                {
                    cmd.env(super::open_url::MAILBOX_ENV, mailbox);
                }
                let label = Path::new(home)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|s| s.rsplit_once('+').map(|(l, _)| l))
                    .unwrap_or("default");
                let has_mailbox = super::accounts_sync::has_configured_mailbox_for_row(
                    user_home.as_deref(),
                    &store,
                    Some(label),
                    Some(email),
                );
                if matches!(browser, RenewBrowser::Remote { .. })
                    && !has_mailbox
                    && super::headless_oauth::has_browser_profile(&store.dir, label, Some(email))
                {
                    cmd.env(super::headless_oauth::HEADLESS_ENV, "1");
                }
            }
            cmd.spawn()
        },
        |mut child| child.wait(),
        || {},
    )
    .map(exit_code)
    .map_err(|e| e.to_string())
}

/// `<program> auth status --json` in the staging home `home` ([`AccountsHost::status_in_home`]), bounded by `budget`: the bounded runner's environment (PATH, HOME, TMPDIR), [`accounts::HOME_ENV`] set to `home`, and the variables [`auth_check_env`] hands through, which do not include [`accounts::LOGIN_ENV`]: the login is the home's own item.
pub(super) fn status_in_home_within(
    program: &std::ffi::OsStr,
    home: &str,
    var: &dyn Fn(&str) -> Option<OsString>,
    budget: std::time::Duration,
) -> Result<Captured, String> {
    let pass = auth_check_env(var)?;
    let mut cmd = aterm_uds::spawnfd::Command::new(program);
    cmd.args(["auth", "status", "--json"])
        .env(accounts::HOME_ENV, home);
    if let Some(user_home) = var("HOME").filter(|v| !v.is_empty()) {
        cmd.env("HOME", user_home);
    }
    cmd.envs(pass);
    super::align::capture_bounded(cmd, budget, MAX_AUTH_STDOUT)
}

fn login_command(program: &std::ffi::OsStr, dir: Option<&str>) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::Command::new(program);
    cmd.args(["auth", "login"]).env_remove(accounts::LOGIN_ENV);
    match dir {
        Some(dir) => cmd.env(accounts::HOME_ENV, dir),
        None => cmd.env_remove(accounts::HOME_ENV),
    };
    cmd
}

fn login_flow_of(program: &std::ffi::OsStr, dir: Option<&str>) -> Result<i32, String> {
    terminal_flow(
        || login_command(program, dir).spawn(),
        |mut child| child.wait(),
        || {},
    )
    .map(exit_code)
    .map_err(|e| e.to_string())
}

/// One run of `cmd` on this terminal, waited for as [`login_flow_of`] waits: the AI CLIs' sign-in flows (`crate::ai::login`).
pub(crate) fn terminal_run(mut cmd: aterm_uds::spawnfd::Command) -> Result<i32, String> {
    terminal_flow(|| cmd.spawn(), |mut child| child.wait(), || {})
        .map(exit_code)
        .map_err(|e| e.to_string())
}

#[cfg(unix)]
static TERMINAL_FLOWS: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

#[cfg(unix)]
fn ignoring_interrupts<T>(wait: impl FnOnce() -> T) -> T {
    // SAFETY: `sigaction` is plain data for which all-zero is a valid value (an empty mask, no flags).
    let mut ignore: libc::sigaction = unsafe { std::mem::zeroed() };
    ignore.sa_sigaction = libc::SIG_IGN;
    let replaced = [libc::SIGINT, libc::SIGQUIT].map(|sig| {
        // SAFETY: as above.
        let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a valid disposition for `sig`; `was` receives the one it replaces.
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

#[cfg(not(unix))]
fn ignoring_interrupts<T>(wait: impl FnOnce() -> T) -> T {
    wait()
}

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

/// One run of `<program> auth status --json` under `login` ([`accounts::LOGIN_ENV`] set to a row's `dir` verbatim, or empty for this machine's own login), bounded by [`AUTH_CHECK_BUDGET`], its stdout capped at [`MAX_AUTH_STDOUT`].
pub fn auth_status_of(
    program: &std::ffi::OsStr,
    login: &str,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Captured, String> {
    auth_status_within(program, login, var, AUTH_CHECK_BUDGET)
}

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

/// Why `claude auth status --json` is not run with neither USER nor LOGNAME in the environment: [`LoginCheck::CouldNotRun`]'s reason, word for word.
pub const AUTH_CHECK_NO_USER: &str = "USER is not set";

/// The variables the check after a sign-in ([`auth_status_of`], [`status_in_home_within`]) hands through to the `claude` it runs, as they are: USER and LOGNAME, and the reroute stub's markers.
pub(super) fn auth_check_env(
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

/// What one login answered to the check after an `accounts login` flow that exited 0 ([`login_check`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginCheck {
    /// `loggedIn: true`, exit 0.
    SignedIn,
    /// `loggedIn: false` (with exit 1 on 2.1.286, design §1).
    SignedOut,
    /// No login was made in the row's login directory on this machine — it is absent, or holds nothing ([`accounts::login_made`]): nothing was run.
    NotEnrolled,
    /// The command did not run, or gave no answer that can be read: nothing is recorded.
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

    /// What is recorded of it ([`accounts::note_auth`]); `None` records nothing.
    #[must_use]
    pub const fn auth(&self) -> Option<Auth> {
        match self {
            LoginCheck::SignedIn => Some(Auth::Ok),
            LoginCheck::SignedOut => Some(Auth::SignedOut),
            LoginCheck::NotEnrolled | LoginCheck::CouldNotRun(_) => None,
        }
    }
}

fn logged_in(stdout: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(stdout).ok()?;
    let doc: Value = aterm_json::from_str(text).ok()?;
    doc.as_object()?.get("loggedIn")?.as_bool()
}

/// One run of `claude auth status --json` as an answer: the exit status and [`logged_in`], nothing else.
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

/// What a renewal's check read of `claude auth status --json` in the staging home ([`status_facts`]): four facts and nothing else.
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
    /// The check passes: signed in to claude.ai as the row's account.
    #[must_use]
    pub const fn renews(&self) -> bool {
        self.signed_in && self.claude_ai && self.email_matches
    }
}

/// The four facts of one `claude auth status --json` run in a staging home (Claude Code 2.1.300: `loggedIn`, `authMethod`, the home's `oauthAccount` address as `email`, the token's `subscriptionType`; exit 0 only when logged in), `want` the row's address.
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

fn accounts_refusal(path: &Path, why: &str) -> String {
    format!(
        "aterm harness accounts: the roster at {} is REFUSED: {why}. It is admitted whole or \
         not at all, so no account is listed until it is fixed.",
        path.display()
    )
}

fn accounts_none_listed(path: &Path) -> String {
    format!(
        "no accounts listed\nroster: {}{}\nlist one: {ACCOUNTS_ADD_LINE}\n",
        path.display(),
        if path.exists() {
            " (no rows)"
        } else {
            " (no such file)"
        }
    )
}

#[derive(Debug, Clone, Copy)]
struct HeldBy {
    tap: super::account_stats::Tap,
    key: Option<profile::AccountKey>,
}

impl HeldBy {
    fn of(env: &Env) -> HeldBy {
        let several = accounts::several_listed(env.accounts_roster().as_deref());
        HeldBy {
            tap: stats_tap(env, |_| true),
            key: env
                .home
                .as_deref()
                .and_then(profile::default_home_key)
                .filter(|_| !several),
        }
    }

    fn admit(self, mut state: accounts::State) -> accounts::State {
        state
            .accounts
            .retain(|_, account| !account.of_another(self.key));
        state
    }

    fn windows(
        self,
        account: &accounts::Account,
        state: &accounts::State,
        now: i64,
    ) -> super::account_stats::View {
        super::account_stats::held_windows(account, self.key, Some(&state.machine), self.tap, now)
    }
}

fn accounts_own_reading(state: &accounts::State, by: HeldBy, now: i64) -> String {
    use super::account_stats::{self, Win};
    let Some((account, sample)) = state
        .accounts
        .get(ACCOUNTS_OWN_LABEL)
        .and_then(|account| Some((account, account.sample.as_ref()?)))
    else {
        return format!(
            "this login ({ACCOUNTS_OWN_LABEL}): nothing read of it by the usage tap yet (what is \
             recorded is of the account signed in to the home before it)"
        );
    };
    let held = by.windows(account, state, now);
    let window = |name: &str, win: Option<&Win>| match win.and_then(|w| w.current(now)) {
        Some(f) => format!(
            "{name} {} until {}",
            account_stats::percent(f.pct_x10, held.tenths),
            rfc3339_utc(f.reset)
        ),
        None => format!("{name} ?"),
    };
    format!(
        "this login ({ACCOUNTS_OWN_LABEL}), read by the usage tap at {}: {} \u{b7} {}",
        rfc3339_utc(sample.at),
        window("session", held.windows.first()),
        window("week", held.windows.get(1)),
    )
}

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

struct AccountsNext {
    to: Option<String>,
    wait_until: Option<i64>,
    not: Vec<(String, accounts::WhyNot)>,
}

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
            .map_or_else(String::new, |offer| {
                let left = offer
                    .kind
                    .map_or_else(|| offer.left.to_string(), |kind| kind.resets(offer.left));
                match offer.use_by {
                    Some(by) => format!("offer: {left} left, use by {}  ", rfc3339_utc(by)),
                    None => format!("offer: {left} left  "),
                }
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
                accounts::WhyNot::NotEnrolled => match state
                    .accounts
                    .get(label)
                    .and_then(|a| a.limited_until(now))
                {
                    Some(until) => {
                        format!("{label}: {} (limited until {})", why.as_str(), rfc3339_utc(until))
                    }
                    None => format!(
                        "{label}: {} (no login made yet: run `aterm harness accounts {} {label}`)",
                        why.as_str(),
                        roster.row(label).map_or("login", sign_in_verb)
                    ),
                },
                _ => format!("{label}: {}", why_not_words(*why)),
            })
            .collect();
        text.push_str(&format!("not next: {}\n", reasons.join("; ")));
    }
    let to_sign_in = next.not.iter().any(|(_, why)| {
        matches!(
            why,
            accounts::WhyNot::NotEnrolled | accounts::WhyNot::SignedOut
        )
    });
    if next.to.is_none() && !to_sign_in {
        text.push_str(&format!("list another: {ACCOUNTS_ADD_LINE}\n"));
    }
    if accounts::rotations_spent(state, roster, now) {
        text.push_str("the rotation bound for this hour is spent: a limit is waited out\n");
    }
    text
}

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

fn put_time(o: &mut Map, key: &str, t: Option<i64>) {
    o.insert(key.to_owned(), Value::from(t));
    o.insert(format!("{key}_rfc3339"), Value::from(t.map(rfc3339_utc)));
}

fn accounts_json_head(path: &Path, now: i64) -> Map {
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("kind".to_owned(), Value::from("accounts".to_owned()));
    put_time(&mut o, "as_of", Some(now));
    o.insert("roster".to_owned(), Value::from(path.display().to_string()));
    o
}

fn accounts_refused_json(path: &Path, why: &str, now: i64) -> String {
    let mut o = accounts_json_head(path, now);
    o.insert("refused".to_owned(), Value::from(why.to_owned()));
    o.insert("accounts".to_owned(), Value::Array(Vec::new()));
    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
}

fn accounts_json(
    path: &Path,
    roster: &Roster,
    (state, logins): (&accounts::State, &accounts::Logins),
    enrolled: &BTreeSet<String>,
    by: HeldBy,
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
            let binding = logins.get(&row.label);
            m.insert("renews".to_owned(), Value::from(row.email.is_some()));
            m.insert(
                "login".to_owned(),
                Value::from(accounts::login_state(row, logins, state, now).as_str()),
            );
            let ends = binding.and_then(accounts::forecast);
            put_time(&mut m, "login_ends", ends);
            put_time(
                &mut m,
                "renew_by",
                ends.map(|ends| ends.saturating_sub(accounts::DUE_S)),
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
            let key = by.key.or(account.key);
            m.insert("key".to_owned(), Value::from(key.map(|k| k.to_string())));
            m.insert(
                "windows".to_owned(),
                Value::Array(
                    by.windows(&account, state, now)
                        .windows
                        .iter()
                        .map(|w| super::account_stats::window_value(w, now))
                        .collect(),
                ),
            );
            m.insert(
                "offer".to_owned(),
                account.offer.as_ref().map_or(Value::Null, |offer| {
                    let mut f = Map::new();
                    f.insert(
                        "kind".to_owned(),
                        offer
                            .kind
                            .map_or(Value::Null, |kind| Value::from(kind.word())),
                    );
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
                            put_time(&mut w, "stands_until", wall.stands_until());
                            w.insert("cleared".to_owned(), Value::from(wall.cleared));
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

fn run_accounts_report(
    env: &Env,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let Some(path) = env.accounts_roster() else {
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
    let by = HeldBy::of(env);
    let own = roster.rows.is_empty()
        && state
            .accounts
            .get(ACCOUNTS_OWN_LABEL)
            .is_some_and(|account| account.sample.is_some());
    let state = by.admit(state);
    if own && !json {
        let _ = write!(out, "{}", accounts_none_listed(&path));
        let _ = writeln!(out, "{}", accounts_own_reading(&state, by, env.now));
        return ExitCode::SUCCESS;
    }
    let roster = accounts::bound(&if own { Roster::own() } else { roster }, &logins);
    let enrolled = accounts::enrolled(&roster);
    let read = (&state, &logins);
    let _ = if json {
        writeln!(
            out,
            "{}",
            accounts_json(&path, &roster, read, &enrolled, by, env.now)
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

const fn sign_in_verb(row: &accounts::Row) -> &'static str {
    if row.email.is_some() {
        "renew"
    } else {
        "login"
    }
}

struct Added {
    roster: PathBuf,
    dir: PathBuf,
    listed_own: bool,
    dir_held_files: bool,
}

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

/// Where a new roster text is written, and the mode it keeps.
pub(super) fn roster_target(path: &Path) -> Result<(PathBuf, Option<u32>), String> {
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

fn ask_login(host: &dyn AccountsHost, dir: Option<&str>, made: bool) -> LoginCheck {
    if !made {
        return LoginCheck::NotEnrolled;
    }
    login_check(host.auth_status(dir.unwrap_or("")))
}

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
    let had_login = dir.is_some_and(|d| accounts::login_made(Path::new(d)));
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
    let made = dir.is_none_or(|d| accounts::login_made(Path::new(d)));
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
            let Some((_, raw)) = accounts_read_roster(env, err) else {
                return ExitCode::from(1);
            };
            let _ = super::accounts_sync::recover_staging_renewals(&store, &raw, now);
            let roster = store.bound(&raw, now).unwrap_or(raw);
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
                 it at a usage limit (under `early` also once it is `low`; under `off` never), \
                 for the account the report names `next`",
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

/// `[harness] hosts` entries (`host` or `host:/abs/path/to/aterm`) as `(host, aterm)`, each refused by name unless both halves are plain words: no leading `-` (an `ssh` option), no whitespace, control character or shell metacharacter, and an absolute path for the aterm.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Renewal {
    Renewed,
    AuthFailed,
    WrongAccount,
    Failed,
}

impl Renewal {
    pub(super) fn authentication_failed(code: Option<i32>) -> Self {
        // The terminal flow maps signals to 128 + signal; bounded captures
        // retain no exit code when a signal ended the child.
        match code {
            Some(0..=127) => Self::AuthFailed,
            _ => Self::Failed,
        }
    }

    pub(super) fn can_retry_authentication(self) -> bool {
        matches!(self, Self::AuthFailed | Self::WrongAccount)
    }
}

/// The opener's link, `<accounts store>/bin/aterm-open-url` → this program ([`AccountsHost::current_exe`]), made again on every renewal that opens a profile or runs over SSH (a temp link, then a rename), so it names a moved or updated binary.
pub(super) fn opener_link(store: &accounts::Store, host: &dyn AccountsHost) -> Option<PathBuf> {
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
        return Renewal::authentication_failed(Some(exited));
    }
    if !accounts::login_made(&staging) {
        drop_staging(&staging, err);
        say(
            err,
            "the sign-in exited 0 and left no login; nothing was recorded",
        );
        return Renewal::AuthFailed;
    }
    let facts = match host.status_in_home(&staging_text) {
        Ok(got) => status_facts(&got, email)
            .map(|facts| (facts, got.code))
            .ok_or_else(|| {
                let exit = got
                    .code
                    .map_or_else(|| "ended by a signal".to_string(), |c| format!("exit {c}"));
                format!("{exit}, with no answer that can be read")
            }),
        Err(why) => Err(super::one_line(&why, 160)),
    };
    let (facts, status_code) = match facts {
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
        return if recorded.is_ok() {
            Renewal::WrongAccount
        } else {
            Renewal::Failed
        };
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
        return Renewal::authentication_failed(status_code);
    }
    let now = host.now().max(now);
    if let Err(why) = accounts::write_renewal_checkpoint(
        label,
        email,
        &staging_text,
        facts.subscription.as_deref(),
        now,
    ) {
        say(
            err,
            &format!(
                "the new login checked out, but its completion checkpoint could not be saved \
                 ({}); nothing was recorded; its home is kept at {}",
                super::one_line(&why.to_string(), 160),
                staging.display(),
            ),
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
    if due && labels.is_empty() {
        let _ = super::accounts_sync::recover_staging_renewals(&store, &file_roster, now);
    }
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
    let mailbox_dir = super::accounts_sync::resolve_mailbox_dir(env.home.as_deref(), &store);
    let has_headless_browser =
        super::headless_oauth::find_headless_browser(env.home.as_deref()).is_some();
    let row_has_mailbox = |label: &str| {
        let em = roster.row(label).and_then(|r| r.email.as_deref());
        mailbox_dir.is_some()
            || super::accounts_sync::has_configured_mailbox_for_row(
                env.home.as_deref(),
                &store,
                Some(label),
                em,
            )
    };
    let row_can_run_headless = |label: &str| {
        let em = roster.row(label).and_then(|r| r.email.as_deref());
        row_has_mailbox(label)
            || (has_headless_browser
                && logins.get(label).and_then(|b| b.wrong_at).is_none()
                && super::headless_oauth::has_browser_profile(&store.dir, label, em))
    };
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
        let due_labels = accounts::renew_due(&roster, &logins, &state, &enrolled, now);
        for label in &due_labels {
            if !chosen.contains(label) {
                chosen.push(label.clone());
            }
        }
        let has_non_limited_due = due_labels.iter().any(|label| {
            !state
                .accounts
                .get(label)
                .is_some_and(|a| a.limited_until(now).is_some())
        });
        if !has_non_limited_due {
            let candidates =
                super::accounts_sync::auto_renew_candidates(&file_roster, &state, &logins, now);
            let target = candidates
                .iter()
                .find(|r| {
                    row_can_run_headless(&r.label)
                        || (host.stdin_is_terminal() && r.browser.is_some())
                })
                .or_else(|| candidates.first());
            if let Some(target) = target
                && !chosen.contains(&target.label)
            {
                if labels.is_empty() {
                    chosen.retain(|label| {
                        !state
                            .accounts
                            .get(label)
                            .is_some_and(|a| a.limited_until(now).is_some())
                    });
                }
                chosen.push(target.label.clone());
            }
        }
        if labels.is_empty()
            && !host.stdin_is_terminal()
            && chosen.iter().any(|label| row_can_run_headless(label))
        {
            chosen.retain(|label| row_can_run_headless(label));
        }
    }
    if chosen.is_empty() && machines.is_empty() {
        let _ = writeln!(out, "nothing is due on this machine");
        return ExitCode::SUCCESS;
    }
    let all_can_run_headless =
        !chosen.is_empty() && chosen.iter().all(|label| row_can_run_headless(label));
    if !chosen.is_empty() && !host.stdin_is_terminal() && !all_can_run_headless {
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
        let interactive_browser =
            |opener: &mut Option<Option<PathBuf>>| match row.browser.as_deref() {
                None => RenewBrowser::Default,
                Some(profile) => match opener.get_or_insert_with(|| opener_link(&store, host)) {
                    Some(link) => RenewBrowser::Profile {
                        opener: link.clone(),
                        profile: profile.to_string(),
                    },
                    None => RenewBrowser::Default,
                },
            };
        let used_headless_profile_only =
            !host.under_ssh() && !row_has_mailbox(label) && row_can_run_headless(label);
        let browser = if host.under_ssh() || row_can_run_headless(label) {
            RenewBrowser::Remote {
                opener: opener
                    .get_or_insert_with(|| opener_link(&store, host))
                    .clone(),
            }
        } else {
            interactive_browser(&mut opener)
        };
        let mut renewal = renew_one(env, host, &store, row, own_dir, &browser, out, err);
        if renewal.can_retry_authentication()
            && used_headless_profile_only
            && host.stdin_is_terminal()
        {
            let fallback = interactive_browser(&mut opener);
            renewal = renew_one(env, host, &store, row, own_dir, &fallback, out, err);
        }
        ok &= renewal == Renewal::Renewed;
    }
    for (machine, aterm) in &machines {
        let _ = writeln!(out, "renewing on {machine}:");
        let _ = out.flush();
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

/// Route one `accounts` form, the machine injected ([`AccountsHost`]) so the tests drive `login` and `renew` without spawning anything.
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
        AccountsCmd::Sync {
            source,
            machine,
            json,
        } => super::accounts_sync::run_accounts_sync(
            env,
            source.as_deref(),
            machine.as_deref(),
            *json,
            out,
            err,
        ),
        AccountsCmd::Serve {
            source,
            bind,
            machine,
        } => super::accounts_sync::run_accounts_serve(
            env,
            bind.as_deref(),
            source.as_deref(),
            machine.as_deref(),
            out,
            err,
        ),
    }
}

fn run_accounts_stats(env: &Env, json: bool, out: &mut dyn Write) -> ExitCode {
    use super::account_stats::{self, Look, SpendLook};
    use super::spend;
    let read = env
        .claude_profile
        .as_deref()
        .map(|own| profile::effective(own, env.claude_dir().as_deref(), env.claude_custom_oauth))
        .map_or(Err(profile::NoProfile::NoHome), |file| profile::read(&file));
    let key = read.as_ref().ok().and_then(|p| p.key);
    let label = stats_label(env, key);
    let store = env.accounts_store();
    let (state, state_unread) = match store.load(env.now) {
        Ok((state, _)) => (state, false),
        Err(_) => (accounts::State::default(), true),
    };
    let machine = store.machine_id(&state.machine).ok();
    let tap = stats_tap(env, |roster| roster.row(&label).is_some());
    let raw_roster = env
        .accounts_roster()
        .and_then(|path| Roster::load(&path).ok())
        .unwrap_or_default();
    let logins = store
        .load_logins(env.now)
        .map(|(logins, _)| logins)
        .unwrap_or_default();
    let roster = accounts::bound(&raw_roster, &logins);
    let several = roster.several();
    let login = roster
        .row(&label)
        .filter(|row| row.email.is_some())
        .map(|row| {
            let admitted = HeldBy { tap, key }.admit(state.clone());
            account_stats::Login {
                state: accounts::login_state(row, &logins, &admitted, env.now).as_str(),
                renew_by: logins
                    .get(&row.label)
                    .and_then(accounts::forecast)
                    .map(|by| by.saturating_sub(accounts::DUE_S)),
            }
        });
    let prices = spend::PriceTable::from_toml(spend::PRICES_TOML).ok();
    let folded = env.claude_dir().zip(prices.as_ref()).map(|(dir, prices)| {
        let mut spend = spend::Spend::new(&dir, prices);
        let limits = spend::Limits {
            bytes: u64::MAX,
            until: Some(std::time::Instant::now() + std::time::Duration::from_secs(STATS_FOLD_S)),
        };
        let report = spend.fold_claude_home(prices, env.now, limits);
        (spend, report.complete())
    });
    let (sessions, api_key_seen) = match &env.claude_running {
        Some(Ok(running)) => {
            let here: Vec<_> = running
                .claudes
                .iter()
                .filter(|c| c.profile.is_some() && c.profile == env.claude_profile)
                .collect();
            let sessions = if several {
                Err("several-logins")
            } else {
                Ok(here.len())
            };
            (sessions, here.iter().any(|c| !c.stored_login))
        }
        Some(Err(_)) => (Err("census-unread"), false),
        None => (Err("not-counted"), false),
    };
    let look = Look {
        now: env.now,
        tz_s: env.tz_s,
        machine: machine.as_deref(),
        label: &label,
        profile: &read,
        account: state.accounts.get(&label),
        state_unread,
        several,
        api_key_seen,
        tap,
        spend: folded.as_ref().map(|(spend, complete)| SpendLook {
            spend,
            complete: *complete,
        }),
        prices_as_of: prices.as_ref().and_then(spend::PriceTable::as_of),
        login,
        sessions,
    };
    let view = account_stats::view(&look);
    let _ = if json {
        writeln!(
            out,
            "{}",
            account_stats::json(&look, &view, env!("CARGO_PKG_VERSION"))
        )
    } else {
        write!(out, "{}", account_stats::text(&look, &view))
    };
    ExitCode::SUCCESS
}

/// How long `accounts stats` folds the home's transcripts, at most (design §4.1): a fold cut there says `complete: false`.
pub const STATS_FOLD_S: u64 = 10;

fn stats_default_home(env: &Env) -> bool {
    let default_profile = env.home.as_ref().map(|home| home.join(".claude.json"));
    env.claude_profile.is_some() && env.claude_profile == default_profile
}

fn stats_tap(env: &Env, records: impl FnOnce(&Roster) -> bool) -> super::account_stats::Tap {
    use super::account_stats::Tap;
    let (policy, _) = SupervisorConfig::from_path(env.config.as_deref());
    let served = accounts::tap_roster(
        env.accounts_roster().as_deref(),
        &env.accounts_store(),
        env.now,
    )
    .is_some_and(|roster| records(&roster));
    match (policy.usage_tap, policy.usage_tap_on(), served) {
        (false, ..) => Tap::Off,
        (true, false, _) => Tap::HarnessOff,
        (true, true, true) => Tap::On,
        (true, true, false) => Tap::OtherHome,
    }
}

fn stats_label(env: &Env, key: Option<profile::AccountKey>) -> String {
    let raw = env
        .accounts_roster()
        .and_then(|path| Roster::load(&path).ok())
        .unwrap_or_default();
    let roster = env.accounts_store().bound(&raw, env.now).unwrap_or(raw);
    if stats_default_home(env) {
        return roster
            .row_for_login(None)
            .or_else(|| roster.row_left(None))
            .map_or(ACCOUNTS_OWN_LABEL, |row| row.label.as_str())
            .to_owned();
    }
    let row = env.claude_dir.as_deref().and_then(|dir| {
        roster.rows.iter().find(|row| {
            row.dir.as_deref().is_some_and(|d| Path::new(d) == dir)
                || row.was.iter().flatten().any(|d| Path::new(d) == dir)
        })
    });
    match (row, key) {
        (Some(row), _) if accounts::enrolled(&roster).contains(&row.label) => row.label.clone(),
        (_, Some(key)) => format!("home-{}", key.short()),
        (_, None) => "home".to_owned(),
    }
}

fn answer_retired(
    verb: &str,
    stdin_is_tty: bool,
    stdin: &mut dyn io::Read,
    err: &mut dyn Write,
) -> ExitCode {
    if stdin_is_tty {
        let _ = writeln!(err, "aterm harness {verb}: retired; it does nothing");
    } else {
        let _ = io::copy(stdin, &mut io::sink());
    }
    run_retired()
}

const fn run_retired() -> ExitCode {
    ExitCode::SUCCESS
}

/// Route one parsed command.
pub fn run(cmd: &Cmd, env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    match cmd {
        Cmd::Help(page) => {
            let _ = out.write_all(help_page(*page).as_bytes());
            ExitCode::SUCCESS
        }
        Cmd::Retired => run_retired(),
        Cmd::Tap => run_tap(out),
        Cmd::Agy {
            what,
            refresh,
            json,
        } => run_agy(
            env,
            what,
            *refresh,
            *json,
            &super::agy_quota_store::Agy::on_path(),
            out,
        ),
        Cmd::Usage { json, agy } => {
            if *agy || env.under_agy() {
                return run_agy(
                    env,
                    &AgyWhat::Quota,
                    false,
                    *json,
                    &super::agy_quota_store::Agy::on_path(),
                    out,
                );
            }
            run_usage(env, &LiveClaude, *json, out)
        }
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
        Cmd::Versions { json } => run_versions(*json, out),
    }
}

fn run_versions(json: bool, out: &mut dyn Write) -> ExitCode {
    if json {
        let mut items = Vec::new();
        for harness in aterm_phase::CliHarness::ALL {
            let v = harness.version();
            items.push(format!(
                r#"{{"id":"{}","slug":"{}","label":"{}","version":"{}","major":{},"minor":{}}}"#,
                harness.id(),
                harness.cli(),
                harness.product(),
                v.as_str(),
                v.major(),
                v.minor()
            ));
        }
        let _ = writeln!(out, "[{}]", items.join(","));
    } else {
        for harness in aterm_phase::CliHarness::ALL {
            let _ = writeln!(
                out,
                "{:<12} {:<16} v{}",
                harness.id(),
                harness.product(),
                harness.version()
            );
        }
    }
    ExitCode::SUCCESS
}

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
        && matches!(verb, "hook" | "statusline")
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
    let stats = matches!(cmd, Cmd::Accounts(AccountsCmd::Stats { .. }));
    if matches!(cmd, Cmd::Disk { .. }) || stats {
        env.claude_running = Some(disk::discover::running_claude_census(env.home.as_deref()));
    }
    if stats {
        env.tz_s = limit::tz_offset_s();
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
