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
            "aterm ctl survey [--json] [--sid <sid>] [--fleet | --host <h> [--holder \
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
        row("--sid <sid>", "one session's row"),
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
                     s|-> ssh_host_key=<pct|-> home_free_mb=<MiB|-> home_total_mb=<MiB|-> \
                     home_build_mb=<MiB|-> home_build_at_ms=<unix ms|-> \
                     home_build_partial=<0|1|-> accounts=<n> \
                     accounts_unread=<cli>[,…]|-> \
                     moves_7d=<n|-> move_tries_7d=<n|-> last_move=<unix s|-> roster_unread=<0|1> \
                     [update=<word> update_at=<unix s|-> update_to=<version|build n|-> \
                     update_until=<unix s|-> update_why=<word|-> update_tries=<n|-> \
                     update_checks=<on|off|->] store_worst=<word|-> store_worst_pct=<n|-> \
                     access=<read|write|full>[ drive=1]` header \
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
                     fleet reader this machine lets drive it, as far as `access=` admits; \
                     `home_free_mb=` and `home_total_mb=`: the free space on its home's volume \
                     and that volume's size; `home_build_mb=`: the build output aterm sizes as \
                     rebuildable there (its build caches and build folders) as the disk watch \
                     last sized it, read from that sizing's record and never walked, \
                     `home_build_at_ms=` when that sizing ended, on the clock `at_ms=` was read \
                     on, and `home_build_partial=1` where it was cut short and the figure is a \
                     floor; each `-` where unknown, and none from an aterm older than the key; \
                     sizes and a time, no path; the machine's look follows them, below; \
                     `store_worst=`: the fullest of the stores aterm itself keeps there that \
                     refuse writes at a bound (the vault's records log, the lab journal's \
                     outbox), by the fixed word `aterm ctl sys disk` names it, and \
                     `store_worst_pct=` the share of that bound it holds, rounded down and past \
                     100 where it stands past it, sized as the header was made by the store's \
                     own files and never a walk; both `-` where no such store is kept, and \
                     none from an aterm older than the keys; at \
                     `content`, `home= repos= repos_at= repos_cut=` before \
                     `accounts=`: the \
                     home, and how many checkouts the last read of it \
                     found, when, and how many it left out; `roster_unread=1` where this Mac's \
                     label could not be read), one `session` row per \
                     session, then one `account` row per row of each pooled CLI's pool as `aterm \
                     harness accounts --json` lists it (`accounts=` counts them; \
                     `accounts_unread=` names each pool that could not be read, whose rows are \
                     unknown; `moves_7d=` and `last_move=` are the rotations a session proved on \
                     its ledgers, `move_tries_7d=` those it tried, `-` where it cannot say).",
                ),
                para(
                    "The machine's look follows `home_build_partial=` on every header, at \
                     `facts` as at `content`: `os=<macos|linux|windows|-> target=<triple|-> \
                     m_at_ms=<unix \
                     ms|-> cores= load1_x100= load5_x100= load15_x100= cpu_busy_x100= \
                     rate_win_ms= mem_total_mb= mem_used_mb= mem_avail_mb= \
                     mem_pressure=<normal|warn|critical|-> mem_psi60_x100= swap_used_mb= \
                     swap_total_mb= gpu_cores= gpu_busy_pct= net_link_kbps= \
                     net_rx_kbps= net_tx_kbps= net_other_kbps= up_since_ms=<unix ms|-> \
                     cpu_lim=<0|1|-> mem_lim= disk_lim= gpu_lim= load=<level>:<over|->|-`. Each \
                     figure is a whole number \
                     in the unit its key names, capacity beside use, and `-` where the machine \
                     did not read it: none is 0 for unread, and a header with none of them is an \
                     aterm before the look. `m_at_ms`: when the look was taken, on its own clock \
                     (one look is kept 10 s); `target`: the triple its packages are built for; \
                     `cpu_busy_x100`: cores busy times 100, and `net_rx_kbps`, `net_tx_kbps`: \
                     its fastest link's traffic against that link's rate `net_link_kbps` (with \
                     no link rate, every link's), each the mean over the `rate_win_ms` before \
                     the look (45 to 90 s; `-` where no survey read this machine in that time); \
                     `net_other_kbps`: its other links' traffic; `mem_psi60_x100`: on Linux, the \
                     share of the last minute some task stalled on memory, times 100; \
                     `up_since_ms`: the later of its boot and its last wake. The four `_lim` \
                     words are the machine's own verdicts that a resource is at its limit: the \
                     CPU with its 1-minute load over its cores and 95% of them busy, memory \
                     under pressure (`warn` or `critical`; on Linux a tenth of the last minute \
                     stalled with under a tenth of memory available), the home's volume below \
                     the free space aterm starts reclaiming at, the GPU 95% busy; `-` where an \
                     input was unread. `load` is the system-load score its own Stats Deck dial \
                     reads, in hundredths: the level (the busier of its CPU and GPU over the rate \
                     window, lifted by memory pressure) and how far past capacity (0 within it; \
                     from 12 for work waiting on busy cores, swapping or critical pressure), `-` \
                     where its CPU's busy share is unread, and the over `-` where its load \
                     average, its swap traffic or (with neither over) its memory pressure is. A \
                     node service answering for a window older than the look adds its own, and \
                     makes `home_free_mb=` and `home_total_mb=` the look's, saying the size \
                     once.",
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
                     upgrade=<-|<state>/<to>/<why>/<age>> \
                     detail=<pct|-> doing=<pct|-> role=<pct|-> supervisor=<pct|-> \
                     attention=<1|pct|-> goal=<1|pct|-> needs=<word|-> \
                     needs_by=<program|harness|-> hold_by=<word|-> held_from=<32 hex|-> \
                     typed_by=<pct|-> typed_ms=<ms|-> request=<id|-> request_from=<pct|-> \
                     request_why=<1|pct|-> bells=<n> bells_sounded=<n> \
                     last_bell=<ms>/<sound>/<program|->[,…]|-> turn=<1|0|-> turn_ms=<ms|-> \
                     turn_end_ms=<ms|-> step_ms=<ms|-> out_ms=<ms|-> \
                     out_src=<transcript|screen|-> tok_in=<n|-> tok_out=<n|-> \
                     tok_for_ms=<ms|-> turns=<n|-> tools=<n|-> pinned=<0|1> \
                     opened_ms=<unix ms|->`.",
                ),
                para(
                    "A row is read at its header's `at_ms=`, but for one the node service of a \
                     Mac with several windows sent under a header of another time (the first \
                     window's, or the machine's look, where that is later): that row ends with \
                     its own window's `at_ms=<unix ms|->`, when its ages and counters were read \
                     (`-` where that window's clock was unread).",
                ),
                para(
                    "`turn=` says whether the session log holds a turn open, `turn_ms=` how long \
                     ago it opened, `turn_end_ms=` how long ago the newest closed turn closed and \
                     `step_ms=` how long ago the agent's open step (a tool call, an approval or a \
                     question) opened. `out_ms=` is the age of the agent's last output: for Claude \
                     Code and codex-alab (`out_src=transcript`) the newest of the API's last served \
                     row, its newest text row and its newest step, which a spinner never moves; \
                     for every other harness, or a transcript with no time yet, the screen's last \
                     move (`out_src=screen`). `tok_in=` and `tok_out=` are the conversation's \
                     cumulative tokens, sent only while they are the whole conversation's (Claude \
                     Code's once its usage read reached every file, its subagents' included; \
                     codex-alab's rollout's own running total), and `tok_for_ms=` how long they \
                     have counted that conversation unbroken (`-` where it started with the \
                     clock unread): a new or resumed conversation, a restart or a read that is \
                     not whole starts it again, so take a rate only between two rows inside it. \
                     `turns=` counts the turns the session opened and `tools=` the tool steps \
                     of this run, the agent run in front, which a new run counts from 0 (`-` \
                     for a harness with no transcript, where no agent run is open, while this \
                     run's transcript join binds no file or its reader stopped, and for the \
                     rest of this run once its join loses a row, as its count is then short). \
                     A row no agent reads \
                     says `-` for each; an older aterm sends none of them.",
                ),
                para(
                    "`pinned=1` marks a session a person or a lead agent pinned (`aterm ctl \
                     @<sid> pin`), which the fleet band shows first among its Mac's agents; a \
                     worker it spawns says `0`. An older aterm sends no `pinned=`, which reads as \
                     unread, not as unpinned.",
                ),
                para(
                    "`opened_ms=` is the Unix ms at which the session's lineage first opened, as \
                     `status` prints it: a session a cold restore, a seamless update or a reopened \
                     tab brought back keeps its first open's time, one an older aterm handed \
                     over in a seamless update keeps the open of the log it carried, or the \
                     update's own time where that log did not carry, one from a layout an older \
                     aterm saved for a cold restore says when that restore opened it, and `-` is \
                     a log not yet open. Its age is the reader's to compute: the `at_ms=` the \
                     row was read at minus it. An older aterm sends no `opened_ms=`, which reads \
                     as unread.",
                ),
                para(
                    "`upgrade=` is the harness upgrade word `aterm ctl @<sid> status` prints for \
                     the tab, `<state>/<to>/<why>/<age>` (`pending/2.1.325/account/2h`: the \
                     session has been behind 2.1.325 for two hours and its move now waits on its \
                     account; `<age>` is how long the session has been behind in every state but \
                     `done`), `done/<to>/-/<age>` for ten minutes after a move lands (`<age>` \
                     since it landed), `-` where the host sent no row, at `facts` and `content` \
                     alike; a row from an aterm too old to say has no field, which a reader takes \
                     as unknown, never as current.",
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
                     `usage_5h_pct=` and `usage_7d_pct=`, which keep the use), \
                     `upgrade=<state>/<to>/<why>/<age>` while a harness upgrade is pending (any \
                     state but `done`), \
                     `human=<age>` and `typed by <who> <age> \
                     ago` within 15 minutes (another Mac's drive until a person types after it), \
                     and `| <title> | <folder> <branch> | <model>`.",
                ),
                para(
                    "Under it, quoted lines: `attention:`, `goal:`, `asked:` (the last prompt \
                     typed to it), `said:` (its agent's last words) and `work:` (`<branch> · <n> \
                     files · <n> behind <main> · <age> ago`). `--budget` folds what does not fit \
                     into `+<n>` counts; `--sid <sid>` shows one session whole.",
                ),
                para(
                    "A `--fleet` digest leads, under the `#` line, with `upgrades held (stalled \
                     or 2 h+ behind): <n> sessions on <m> Macs (<wait> <n>, …)` counting the \
                     sessions whose upgrade word reads `stalled` (the harness's own word for a \
                     move refused, failed, held back, stuck, unwatched or overdue), or reads \
                     `pending`, `announced` or `held` with the session behind two hours or more \
                     (past the ladder's last rung), grouped by its wait word; a person's \
                     `deferred` or `skipped` and a `restarting` move are never counted. It adds `unknown on <k> Macs (an aterm before the \
                     field)` where an agent's row says nothing of its upgrade, and is kept only \
                     where every block still shows with it; a current fleet's digest has no such \
                     line.",
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
