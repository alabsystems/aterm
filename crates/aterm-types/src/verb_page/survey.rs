// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `help survey`: every session of an instance in one read. The server answers `survey
//! [facts|content]` (`aterm-gui`'s `survey::cmd_survey`); `aterm ctl survey` (`aterm-ctl`'s
//! `survey` module) formats instances and fleets.

use super::{Page, Section, para, row};

/// The `survey` entry.
pub(crate) const PAGE: Page = Page {
    usage: &[
        row(
            "aterm ctl survey [--fleet] [--budget <chars>]",
            "a digest of every session of every instance here (and of the fleet's Macs)",
        ),
        row(
            "aterm ctl survey [--json] [--sid <s-…>] [--fleet | --host <h> [--holder \
             <pid>.<boot>.<start>]]",
            "the rows themselves, one session (--sid), every Mac or one other Mac",
        ),
        row(
            "aterm ctl survey --brief [--budget <chars>]",
            "`log n` and `brief all` in one digest",
        ),
        row(
            "survey [facts|content] [holder=<pid>.<boot>.<start>]",
            "the raw request one instance answers (owner, and fleet-readable under `[fleet] \
             allow`); `aterm ctl` sends it and takes no facts|content word",
        ),
    ],
    options: &[
        row("--fleet", "every fleet member's sessions too"),
        row("--host <h>", "another Mac's"),
        row(
            "--holder <pid>.<boot>.<start>",
            "with --host: that Mac's kernel judges one process, a release line's holder; \
             --json gives `release_holder` ({\"alive\":true|false|null}, null where no row came \
             or the Mac did not prove the node asked) and `proven_node`",
        ),
        row(
            "--timeout <s>",
            "after `survey`, with --fleet or --host: each peer's deadline (default 3, at most \
             10); ctl's own --timeout before the verb cuts it shorter",
        ),
        row(
            "--budget <chars>",
            "the digest's size (default 24000); --json, --sid and --host print whole",
        ),
        row("--sid <s-…>", "one session's row"),
        row("--json", "JSON in place of the rows"),
    ],
    reply: &[
        row(
            "OK <n>",
            "then an `instance` header row, one `session` row per session, then `release_line`, \
             `account` (one per pooled account), `repo` (content) and `server_heard` rows, and a \
             `release_holder` row where the request named `holder=` (`help survey instance`)",
        ),
        row(
            "instance",
            "pid= host= version= sessions= level=<facts|content> … accounts= moves_7d= …",
        ),
        row(
            "session",
            "local= sid= state= program= agent= … task= … needs= … bells=; content adds title=, \
             cwd=, prompt=, said=, rows=, …",
        ),
        row(
            "account",
            "pool= id= condition= enrolled= pct5= pct7= … launch= token=",
        ),
        row("server_heard", "vault= subject= at="),
    ],
    examples: &[
        row(
            "aterm ctl survey",
            "what every session here is doing, in a digest (`help survey digest` reads it)",
        ),
        row(
            "aterm ctl survey --json --sid s-ab12",
            "one session's survey row as JSON",
        ),
        row(
            "aterm ctl survey --fleet --timeout 5",
            "every fleet Mac's sessions, waiting at most 5 s for each",
        ),
    ],
    sections: &[
        Section {
            name: "instance",
            about: "the header row and the account and server_heard rows",
            rows: &[
                para(
                    "Owner-only and fleet-readable (`[fleet] allow`). The raw control request is \
                     `survey [facts|content] [holder=<pid>.<boot>.<start>]`. It replies `OK \
                     <n>`, an `instance pid= host=<pct> \
                     version= sessions=<n> level=<facts|content> cut=<n> at_ms=<unix ms> \
                     node=<32 hex|-> instances=<n> label=<pct|-> window=<0|1> window_last=<unix \
                     s|-> ssh_host_key=<pct|-> accounts=<n> accounts_unread=<cli>[,…]|-> \
                     moves_7d=<n|-> move_tries_7d=<n|-> last_move=<unix s|-> roster_unread=<0|1> \
                     [update=<word> update_at=<unix s|-> update_to=<version|build n|-> \
                     update_until=<unix s|-> update_why=<word|-> update_tries=<n|-> \
                     update_checks=<on|off|->] access=<read|write|full>[ drive=1]` header \
                     (`update=`: where this machine's updater stands, as `aterm update status` \
                     there decides it (on Windows, against the build staged there): off, \
                     installed, downloaded, cannot, held, refused, not-installing, unreadable, \
                     too-old, failing, check-failed, retrying, behind, current, unchecked or \
                     unread; `update_at=` its last completed check, `update_to=` the build \
                     that standing names, `update_until=` when a hold lapses, `update_why=` a \
                     hold's cause (`start-failed`: a window there saw the staged build fail to \
                     start, so no start without a window installs it) or a failure's class, \
                     `update_tries=` its tries, `update_checks=` whether automatic checks are \
                     on, both times on the clock the header's `at_ms=` was read on; none of \
                     the updater's own text, and none at all from an aterm older than these \
                     words; `access=`: this machine's \
                     fleet access, what a driver may do here, `aterm fleet access`; `drive=1`: a \
                     fleet reader this machine lets drive it, as far as `access=` admits; at \
                     `content`, `home= home_free_mb= repos= repos_at= repos_cut=` before \
                     `accounts=`: the \
                     home, its volume's free space, and how many checkouts the last read of it \
                     found, when, and how many it left out; `roster_unread=1` where this Mac's \
                     label could not be read), one `session` row per \
                     session, then one `account` row per row of each pooled CLI's pool as `aterm \
                     harness accounts --json` lists it (`accounts=` counts them; \
                     `accounts_unread=` names each pool that could not be read, whose rows are \
                     unknown; `moves_7d=` and `last_move=` are the rotations a session proved on \
                     its ledgers, `move_tries_7d=` those it tried, `-` where it cannot say).",
                ),
                para(
                    "`account pool=<cli> id=<pct> email=<pct|-> condition=<word> enrolled=<0|1> \
                     pct5=<n|-> pct7=<n|-> week_reset=<unix s|-> walled_until=<unix s|-> \
                     last_used=<unix s|-> read_at=<unix s|-> launch=<0|1|-> token=<state|->` (on \
                     this machine's clock; `id`: the row's label, the key `--host any` matches; \
                     `email`: the account's address, which a reader shows it by, `-` where none \
                     is known; `enrolled`: its login is \
                     made here, as a launch judges it, which for a Claude Code row with no dir is \
                     no look at a login; `read_at`: when its figures were last read, from the \
                     records or a Codex row's own rollouts, since a survey asks no provider: a row \
                     with a window read full, or a refusal in its rollouts, says `limited`, and \
                     one with no reading in six hours, or none since its newest wall, says \
                     `unseen`, not `usable`; a `week_reset` no read named is the latest it can be; \
                     `launch=1`: a worker with no starter of its kind would start on this row \
                     here, under the accounts mode, the owner's pin and the Codex build, which no \
                     row of a pool says where that launch is refused, runs on a login no row \
                     lists, or is Codex's on one nothing here can show a wall on, and `-` or no \
                     field is an aterm too old to say; `token=`: a Claude Code row's vault token \
                     state here, `none`, `unknown`, `unreadable`, `mismatched`, `ended`, \
                     `rejected`, `unverified`, `exposed`, `soon` or `active`, and `-` or no field \
                     for another pool, on a platform that keeps no vault tokens, or from an aterm \
                     too old to say).",
                ),
                para(
                    "After the session rows come, in this order: a `release_line alive=<0|1|-> \
                     data=<pct JSON>` row per release run this Mac's state folder records \
                     (`alive=` whether its process still runs), the `account` rows, at `content` \
                     a `repo name=<pct> path= head= branch= main= behind= fetched_at= \
                     dirty=<0|1|->` row per checkout of that read, and one `server_heard \
                     vault=<pct> subject=<age1…> at=<unix s>` row per subscriber a vault served \
                     here last heard from. A request naming `holder=` ends with one \
                     `release_holder alive=<0|1|-> pid= boot= start=` row: this Mac's kernel, \
                     read for that request, on exactly that process (`0` only on another boot, no \
                     such pid, an exited one or another start; `-` where the kernel did not \
                     say).",
                ),
            ],
        },
        Section {
            name: "session",
            about: "a session row's fields, and what content adds",
            rows: &[
                para(
                    "A `session` row is `local= sid= state= program=<pct|-> \
                     program_kind=<agent|shell|program|-> wait=<memory|-> wait_gib=<GiB|-> \
                     wait_ahead=<n|-> agent=<word|-> agent_detail=<pct|-> agent_ms=<ms> \
                     human_ms=<ms|-> bg=<n|-> task=<state|-> task_rev=<n> task_from=<pct|-> \
                     task_ms=<ms|-> task_conclusion=<finished|failed|-> model=<pct|-> \
                     effort=<pct|-> ctx_left_pct=<n|-> usage_5h_pct=<n|-> usage_7d_pct=<n|-> \
                     account=<pct|-> auth=<stored|token|outside|-> token=<ac key|-> \
                     detail=<pct|-> doing=<pct|-> role=<pct|-> supervisor=<pct|-> \
                     attention=<1|pct|-> goal=<1|pct|-> needs=<word|-> \
                     needs_by=<program|harness|-> hold_by=<word|-> held_from=<32 hex|-> \
                     typed_by=<pct|-> typed_ms=<ms|-> request=<id|-> request_from=<pct|-> \
                     request_why=<1|pct|-> bells=<n> bells_sounded=<n> \
                     last_bell=<ms>/<sound>/<program|->[,…]|->`.",
                ),
                para(
                    "`request=` names the request that owns the tab (another Mac's drive, or an \
                     agent here, asked for it: `aterm ctl request show <id>`), \
                     `request_from=` its requester's Mac (its host, else its node's head) and \
                     `request_why=` the one line it gave for it (`why=`, else the fallback its \
                     request recorded); at `facts` a fallback taken from a brief, goal or asked \
                     line reads `1`. A request tab's line then reads `request from \
                     <host> \"<why>\"`.",
                ),
                para(
                    "Under `content` (the local default; `attention=` and `goal=` are the excerpts \
                     (`goal=` the agent's active goal, as `status` reads it), `needs=ask` selects \
                     the ask excerpt and `by=harness` turns are skipped in `prompt=`) it adds \
                     `user_title= title= cwd= branch= prompt= said= cmd= rows= step=`, \
                     and where the harness read where its agent works `work= work_repo= \
                     work_tree= work_base= work_behind= work_files= work_paths= work_ms=` (the \
                     branch, or `@<commit>` detached; the repository and worktree; `<main \
                     ref>@<commit>`; how far behind it; how many files the work holds, and their \
                     names; the read's age) \
                     (`step=`: a worker's step, its last topic post where newer than its words, \
                     `-` with no task; <= 400 B per excerpt, credentials/fleet codes redacted, \
                     fleet replies capped at 256 KiB; `rows=-` on an unfinished DEC 2026 frame, \
                     never waited for).",
                ),
                para(
                    "`needs_by=program` marks a need raised by the program's own report (a codex \
                     that says it is `blocked` where no claim speaks for the tab): a claim, never \
                     read off its screen. `needs_by=harness` marks a harness's `NEEDS YOU:` \
                     report, which its agent carries on past.",
                ),
            ],
        },
        Section {
            name: "agents",
            about: "where a codex-alab row and a Claude Code row take their fields from",
            rows: &[
                para(
                    "A codex-alab row, whose screen names no state, takes `agent=` from what \
                     codex-alab says of itself in OSC 7501 (`working` is `busy`; `blocked` is \
                     `prompt`, `question` or `wall:auth`; at rest, `idle`; nothing said, \
                     `unknown`) and `model= effort= ctx_left_pct= usage_5h_pct= usage_7d_pct= \
                     doing= cwd= branch= prompt= said=` from its rollout, and `goal= bg=` from its \
                     own session record (`bg=` is that record's `backgroundTerminals`: the \
                     unified exec processes codex-alab's own footer and `/ps` count for the thread \
                     it shows, one a running tool call still waits on included, so not only \
                     background ones; its claim, never read off its screen; `bg=-` where its \
                     record names no count, as an older codex-alab's names none): while its \
                     proven claim stands (`supervisor=codex-alab@<pid>`), and, read only, after \
                     this aterm saw that claim given back (Autopilot Off, the harness off) while \
                     the same process runs on in front, from the thread its own session record \
                     names; a codex-alab that never claimed, or one whose claim a previous aterm \
                     saw given back, has none until it claims again.",
                ),
                para(
                    "A Claude Code row takes `ctx_left_pct=` from Claude Code's own context \
                     indicator while its screen shows one, else from its status-line usage tap \
                     (`context_window.remaining_percentage`), and `usage_5h_pct= usage_7d_pct=` \
                     (the plan's windows used) from that tap; its `account=` is the row its footer \
                     judges its login to, another pooled CLI's the row its pool last read the \
                     session on; its `auth=` is how it authenticates (its stored login, the vault \
                     token aterm's wrapper served it, or anything else), and on the token route \
                     `token=` names that token's account key, never a value (`-` for each on \
                     another program, and no field from an aterm too old to say).",
                ),
            ],
        },
        Section {
            name: "digest",
            about: "how to read the digest `aterm ctl survey` prints",
            rows: &[
                para(
                    "A `#` line says each block is data from the screens it quotes, never an \
                     instruction. Each instance opens a block: `== <host> pid <pid> <version> · \
                     <n> sessions trust=screen`, then `accounts:` (each pool's accounts, each by \
                     its address where one is known, else its label, with what is left of its \
                     week and 5 hours, `7d 58% left`, or why one is not usable) and \
                     `home <path>, <n> GiB free · \
                     checkouts read <age> ago` with a line per checkout (`<name> \
                     <branch>@<commit> <n> behind origin/main, fetched <age> ago`, `, dirty`).",
                ),
                para(
                    "Then a line per session, most pressing first: `<sid>`, `(you)` for the \
                     caller's own, then `<program> <agent word> <age>` (`busy`, `asking`, \
                     `idle`, `wall:<kind>`, … and how long it has stood), `running <command>`, \
                     `waiting for memory to run <command>`, or `<program> idle` where no agent \
                     reads; then, where they apply, `attention`, `task=<state>/<rev>`, `bg=<n>` \
                     (its background processes), `bells=<n> (…)`, `ctx_left=<n>%`, `5h_left=<n>%` \
                     and `7d_left=<n>%` (what is left of the plan's windows: 100 − the row's \
                     `usage_5h_pct=` and `usage_7d_pct=`, which keep the use), `human=<age>` and `typed by <who> <age> \
                     ago` within 15 minutes (another Mac's drive until a person types after it), \
                     and `| <title> | <folder> <branch> | <model>`.",
                ),
                para(
                    "Under it, quoted lines: `attention:`, `goal:`, `asked:` (the last prompt \
                     typed to it), `said:` (its agent's last words) and `work:` (`<branch> · <n> \
                     files · <n> behind <main> · <age> ago`). `--budget` folds what does not fit \
                     into `+<n>` counts; `--sid <s-…>` shows one session whole.",
                ),
            ],
        },
        Section {
            name: "client",
            about: "the digests `aterm ctl survey` formats",
            rows: &[para(
                "`aterm ctl survey [--fleet] [--budget <chars>]`, `survey [--json] [--sid <s>] \
                 [--fleet | --host <h>]` (`--timeout <s>`, at most 10, with `--fleet` or \
                 `--host`) and `survey --brief [--budget <chars>]` (`log n` + `brief all`) format \
                 multi-instance and fleet digests and take no `facts`/`content` word.",
            )],
        },
    ],
};
