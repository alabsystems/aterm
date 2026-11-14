// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `help status`: one line of a session's state. The REPLY rows whose term names a section hold
//! every field in the order `aterm-gui`'s `session_status_record` writes them, their text the
//! field names alone ([`crate::control_verbs::status_fields`]; a test there holds the record to
//! them); each group's section says what its fields mean.

use super::{Page, Section, para, row};
use crate::{no_checked_record, setup_state};

/// The `status` entry.
pub(crate) const PAGE: Page = Page {
    usage: &[
        row(
            "status",
            "one line of the target session's state (`aterm ctl @<sid> status`, or `aterm ctl \
             status <sid>`)",
        ),
        para(
            "No `--json`. Each field is `<name>=<value>`; `-` is unset and free text is \
             percent-encoded.",
        ),
    ],
    options: &[],
    reply: &[
        row(
            "OK schema=1 sid=<local> …",
            "one line, its fields in this order, each group a section (`help status <group>`):",
        ),
        row(
            "process",
            "schema sid subject subject_source observed phase since_ms outcome exit_code signal \
         detail confidence reasons attribution fs_consent conflict revision enabled",
        ),
        row(
            "fabric",
            "hold hold_advisory fabric fabric_rtt_ms fabric_link_age_ms fleet",
        ),
        row("units", "units"),
        row("setup", "setup"),
        row(
            "net",
            "net net_kind net_since_ms internet internet_ms members away_since_ms",
        ),
        row(
            "presence",
            "hand hand_pid level story why needs needs_n hold_by typed_by typed_ms path",
        ),
        row(
            "agent",
            "program agent agent_detail agent_rev agent_since_ms agent_gen agent_fp wait wait_gib \
         wait_ahead composer program_state program_state_ms last_act auth token",
        ),
        row(
            "theme",
            "os_appearance theme_side reported_scheme scheme_from bg_dark session_theme \
         theme_origin conpty",
        ),
        row(
            "host",
            "integration input input_bytes input_wait_ms fg_footprint_mb disk disk_free_mb \
         disk_room_mb disk_age_ms job job_quiet_ms program_check program_check_stranded left \
         syspolicyd syspolicyd_workers syspolicyd_full_ms syspolicyd_pid syspolicyd_up_ms \
         syspolicyd_age_ms service_mem service_mem_top occupancy occupancy_hold ready slots idle_ready_min waits waits_ready",
        ),
        row(
            "harness",
            "supervisor_contract supervisor_scope supervisor_revision supervisor supervisor_proven \
         supervisor_version supervisor_vendor supervisor_act autopilot human_ms human_typed_ms \
         human_seq launch launch_seq path_evidence copy upgrade history_lost journal \
         journal_off journal_tail journal_ring_lag journal_ring journal_bytes \
         journal_ledger_lag restore restore_why in_flight_lost tail_overflow marks_lost \
         resizes_unjournaled integration_rev born render resizes flaps",
        ),
        row(
            "landing",
            "land_at task pause land_held land_nudges land_since held waiting land_asked \
         landed_head landed_on landed_leg landed_repo goal",
        ),
        row(
            "records",
            "task_state task_rev log_seq log_ep log_base run turn step need need_since_ms rollout \
         rollout_asks log_journal log_journal_why exec_watch exec_watch_why editing editor \
         bells bells_sounded last_bell",
        ),
        row("gen", "gen seq hash"),
        para(
            "`syspolicyd=stuck`: macOS's program check is deadlocked, and most new programs \
             cannot start until the Mac restarts: record that and stop retrying (`help status \
             host`). `disk=short` or `emergency`: the volume is nearly full (`aterm help sys`).",
        ),
        para(
            "Windows adds `conpty=`; `journal=` is followed by `journal_off=`, or by the five \
             `journal_*` fields of a live journal, then by the `restore` fields after a crash \
             restore. `if-gen=`, `if-human=` and `if-fp=` on a write fence on `gen=`, \
             `human_seq=` and `hash=`.",
        ),
    ],
    examples: &[
        row(
            "aterm ctl @self status",
            "this tab's state, every field on one line",
        ),
        row(
            "aterm ctl status s-ab12 | tr ' ' '\\n' | grep -E '^(phase|agent|task_state)='",
            "three fields of another session, one per line",
        ),
    ],
    sections: &[
        Section {
            name: "process",
            about: "the program's phase, outcome, detail and the status record's own state",
            rows: &[
                para(
                    "`schema=1 sid=<local> subject= subject_source=<pin|osc|cwd|unavailable> \
                     observed=<true|false> phase=<unknown|starting|idle|running|quiet|exited> \
                     since_ms= outcome=<none|success|failure|signal> exit_code= signal= detail= \
                     confidence=<unknown|heuristic|strong|exact> \
                     reasons=<shell_block,fg_job,…|-> attribution=<live|adopted|unknown> \
                     fs_consent=<covered|denied|unknown> conflict= revision= enabled=`",
                ),
                para("`help status records` says what hears the session's `exec`s for `detail=`."),
            ],
        },
        Section {
            name: "fabric",
            about: "the hold and the fabric link",
            rows: &[para(
                "`hold=<0|1> hold_advisory=<fabric-lost|-> \
                 fabric=<absent|connected|stale|stalled|disconnected> fabric_rtt_ms= \
                 fabric_link_age_ms= fleet=`",
            )],
        },
        Section {
            name: "units",
            about: "units=: whether this Mac serves its toolset archives to its fleet",
            rows: &[row(
                "units=<serving|idle|no-member|off|->",
                "whether this Mac serves the toolset archives it keeps to its fleet members: \
                 `serving` while a server of its store listens, `idle` with members and none \
                 listening now, `no-member`, `off` after `aterm pkg serve-units --off` or \
                 Settings ▸ Packages' switch; the doctor's `base.pkg.lan-sharing` decision, read \
                 off the event loop: a word under 2 s old stands, an older one is read again \
                 before the reply (which waits at most 10 ms for it), and `-` is no word: the \
                 package manager is inert, no store location resolves (no home directory), or no \
                 read has landed in the last 4 s",
            )],
        },
        Section {
            name: "setup",
            about: "setup=: this Mac's first-run setup, as its worker last published it",
            rows: &[para(concat!(
                setup_state!(),
                " (`setup status` is the same state with its rows' standings)"
            ))],
        },
        Section {
            name: "net",
            about: "where this Mac is on the network, and how many lab members are up",
            rows: &[
                para(
                    "`net=<alab-lan|remote|offline|checking|-> \
                     net_kind=<hotspot|lan|cellular|vpn|-> net_since_ms= \
                     internet=<ok|down|path-up|unknown|-> internet_ms= members=<up>/<members>|- \
                     away_since_ms=`",
                ),
                para(
                    "This Mac's network place, one Mac-wide answer every reporter words alike: \
                     where it is and whether the internet answered a measurement in this stay \
                     (`path-up` is the path alone, never measured); `net_since_ms=`, \
                     `internet_ms=` and `away_since_ms=` are unix ms, `-` where unknown; \
                     `checking` while a move settles or before any network is learned as the \
                     ALab LAN, which raises nothing; read from the place's record, cached for 2 \
                     s.",
                ),
                para(
                    "`members=` is apart from the place: how many of the lab's members presence \
                     reads up, from this window's last read of this Mac's copy (its Fleet glance, \
                     macOS with the mesh on; `aterm fleet` reads it whole), `-` without a fresh \
                     one.",
                ),
            ],
        },
        Section {
            name: "presence",
            about: "who has the hand, the chip's level, and what the session needs",
            rows: &[
                para(
                    "`hand=<turn:<id>[:<holder>]|lease:<holder>|driving:<sid>|-> hand_pid= \
                     level=<quiet|note|story|driving|driven|attention|limited|hold> story= why= \
                     needs=<frozen|stopped|stalled|login|memory|wall|ask|approval|question|-> \
                     needs_n= hold_by=<person|remote|local|bridge|-> typed_by=<pct|-> typed_ms= \
                     path=<frozen|live>`",
                ),
                para(
                    "`needs_n=` counts a harness's needs (`meta set harness_needs`) and any its \
                     entry's `(+N more)` counts. Debug builds also write `needs_told=` after \
                     `needs_n=`, and `end= end_seen= end_cheered= done_unseen=` after `landed_repo=` \
                     (`done_unseen=1` while the session holds a done or an end nobody looked \
                     at, in front or behind).",
                ),
            ],
        },
        Section {
            name: "agent",
            about: "the program in front, the agent's state, a build's memory wait",
            rows: &[
                para(
                    "`program= agent=<busy|prompt|question|idle|survey|unknown|wall:<kind>|-> \
                     agent_detail= agent_rev= agent_since_ms= agent_gen= agent_fp= \
                     wait=<memory|-> wait_gib=<GiB|-> wait_ahead=<n|-> \
                     composer=<empty|placeholder|draft|-> \
                     program_state=<idle|working|blocked|done|error>[:<permission|question|auth>] \
                     program_state_ms= last_act= auth=<stored|token|outside|-> token=<state|->`",
                ),
                para(
                    "`wait=memory` while the program in front is a build's front waiting for its \
                     memory claim, which `program=` names by the build it becomes, `targo`: \
                     `wait_gib=` its claim to a tenth of a GiB and `wait_ahead=` the claims ahead \
                     of it, as its ledger said at most about 5 s ago, `-` where its ledger is \
                     unread; `agent_detail=bash:read-only` is the read-only rule's advisory \
                     verdict on a Bash box's command alone; `composer=` is the composer's proven \
                     content, with `-` where no composer is drawn or its text is not proven.",
                ),
                para(
                    "`program_state=` is the root OSC 7501 report of the program in front when \
                     it names that program (a codex's `app=codex`, a Grok Build's \
                     `app=grok-build`), a claim beside an `agent=` it never moves, `-` otherwise, \
                     and `program_state_ms=` how long it has stood, counted through sleep as the \
                     survey's `agent_ms=` is; `agent_since_ms=` and `human_ms=` stop while the \
                     Mac sleeps, so across a sleep it is not comparable with them.",
                ),
                row(
                    "auth=",
                    "how a Claude Code tab signs in: its `stored` login, its account's vault \
                     `token`, or `outside` where its environment brings its own; `token=` that \
                     account's vault token state (`none`, `soon`, `active`, …); `-` each for \
                     another program, before its footer read the process, or where this platform \
                     keeps no vault tokens",
                ),
            ],
        },
        Section {
            name: "theme",
            about: "the appearance the session sees and where its theme comes from",
            rows: &[para(
                "`os_appearance= theme_side= reported_scheme= scheme_from=<theme|program> \
                 bg_dark= session_theme= theme_origin=` (`conpty=` on Windows)",
            )],
        },
        Section {
            name: "host",
            about: "input, disk, jobs, macOS's program check, services, occupancy and wakes",
            rows: &[
                para(
                    "`integration=<on|off|degraded> \
                     input=<clear|pending|typeahead|stalled|stopped|-> input_bytes= \
                     input_wait_ms= fg_footprint_mb= disk=<ok|low|short|emergency|-> disk_free_mb= \
                     disk_room_mb= disk_age_ms= job=<verdict|none> job_quiet_ms= \
                     program_check=<n>:<oldest_ms>|0|- \
                     program_check_stranded=<n>:<oldest_ms>|0|- left=<n>:<oldest_ms>|0:-|- \
                     syspolicyd=<-|ok|full|stuck> syspolicyd_workers= syspolicyd_full_ms= \
                     syspolicyd_pid= syspolicyd_up_ms= syspolicyd_age_ms= \
                     service_mem=<-|ok|large|huge> service_mem_top=<name>:<MiB>|- \
                     occupancy=<off|observe|prompt> \
                     occupancy_hold=<declared|undeclared|none> ready= slots= idle_ready_min= \
                     waits= waits_ready=`",
                ),
                row(
                    "disk=",
                    "the session's volume against this Mac's free-space lines: `ok`, `low` below \
                     the warning line, `short` below the floor where aterm's own cleanup pass \
                     runs, `emergency` where writes are about to fail (too little space or too \
                     few free inodes), `-` before a reading; `disk_free_mb=` its free space in \
                     MiB, `disk_room_mb=` what one round of this machine's builds takes, in MiB \
                     (`-` unmeasured; `await sys 'free>=room'` waits for it, `aterm help sys`), \
                     `disk_age_ms=` the reading's age",
                ),
                row(
                    "syspolicyd=",
                    "macOS's program check (`syspolicyd`), read off its worker pool: `ok` below \
                     its limit, `full` at it for under 5 minutes, `stuck` at it 5 minutes or more \
                     (deadlocked: only a restart of the Mac clears it), `-` unread (off macOS, \
                     before a reading); `syspolicyd_workers=<n>/<limit>` the pool, \
                     `syspolicyd_full_ms=` how long it has read full; `syspolicyd_pid=` the \
                     `syspolicyd` the stuck check's last look read and `syspolicyd_up_ms=` how \
                     long it has run, `-` before a look reads one and once none runs (a look \
                     that reads nothing keeps the pid, with no age); `syspolicyd_age_ms=` how long \
                     ago the last look was taken, `-` before one. A look is taken a minute after \
                     the window starts, each minute while the pool reads full, each minute while a \
                     program of this user waits at the check, and when `status` finds the last a \
                     minute old or more (that ask reads the last; the next reads the new one); at \
                     rest the last stands, so a reading a minute old or more is that look's, \
                     and asking again reads a fresh one",
                ),
                row(
                    "service_mem=",
                    "the largest of macOS's own services that has held past the bar any service \
                     needs for ten minutes, `ok` when none has, `-` before the first reading, off \
                     macOS and in any aterm but the installed app's (the one that watches), and \
                     `service_mem_top=` that service with its footprint",
                ),
                row(
                    "program_check=",
                    "counts this tab's programs macOS's program check holds now, members of its \
                     jobs and their orphans, this user's only, so a lower bound, with the oldest \
                     held to now in ms; `-` off macOS or with no reading that speaks for now",
                ),
                row(
                    "program_check_stranded=",
                    "counts those of them whose check went to a `syspolicyd` that is no longer \
                     running, `0` only when every held program of this user is judged (each \
                     started after the running `syspolicyd`), and `-` also while one may be \
                     stranded and nothing read tells: aterm first read it unrun after that \
                     `syspolicyd` started, a program held since it started is its queue, it has \
                     run under 10 minutes awake, its pool reads full, or no look read it lately",
                ),
                row(
                    "left=",
                    "counts the processes this session's agent started that still run and `sys \
                     left` offers to end (born while an agent ran in the tab, or after the shell \
                     in a tab where none was seen, and a dead verify run's that rode in this \
                     session; `sys left` marks each `left_of=<sid>`), with the oldest's age in \
                     ms, `0:-` with none, `-` with no whole reading that speaks for now",
                ),
                row(
                    "occupancy_hold=",
                    "`occupancy why` explains `occupancy_hold=` to `idle_ready_min=`",
                ),
                row(
                    "waits=",
                    "counts the session's declared wakes still to be told and `waits_ready=` \
                     those whose condition held, which `waits` lists",
                ),
            ],
        },
        Section {
            name: "harness",
            about: "the supervisor claim, Autopilot, the person, launch, journal and render",
            rows: &[
                para(
                    "`supervisor_contract=<n|-> supervisor_scope=<scope,…|-> \
                     supervisor_revision=<n> supervisor= supervisor_proven= supervisor_version= \
                     supervisor_vendor= supervisor_act=<on|observe|-> \
                     autopilot=<off|cruise|land|-> human_ms= human_typed_ms= human_seq= \
                     launch=<person|since|typed|boxed|other|pending|none> launch_seq= \
                     path_evidence= copy=<managed|foreign|-> upgrade= history_lost= \
                     journal=<live|off> … integration_rev= born=<cold|spare-<tier>> \
                     render=<ok|displaced|desync-risk|unverified|unjudged> resizes= flaps=`",
                ),
                row(
                    "supervisor_contract=",
                    "the standing claim's contract, its effective scopes (every duty for a claim \
                     that names none, `-` with no claim) and the claim's revision, which a scoped \
                     harness's `if-supervisor-revision=` names (`help key`)",
                ),
                row(
                    "supervisor_act=",
                    "what aterm's harness may do in the agent it admits here: `on` while the \
                     kernel confirms the running image is that agent's (its vendor's install, its \
                     vendor's signing team, or a lab stand-in in a development build), `observe` \
                     once a read finds nothing does, whatever the program is named: its boxes are \
                     read, logged and raised for the person and nothing is typed into it; `-` with \
                     no agent admitted, or none of its image read yet (the harness reads it \
                     before it types)",
                ),
                row(
                    "autopilot=",
                    "the Autopilot the session runs under: `cruise` (the harness continues each \
                     turn end), `land` (it continues until the work is on the remote main branch \
                     and the agent's `Nothing left:` defense passes its check), `off` (it types \
                     nothing: Off, the harness off or off for this agent), `-` with no agent in \
                     front (`meta set autopilot` switches it)",
                ),
                row(
                    "journal=",
                    "the crash-restore journal: `journal=live journal_tail= journal_ring_lag= \
                     journal_ring=<live|shed|shedding> journal_bytes= journal_ledger_lag=`, or \
                     `journal=off journal_off=<why>`; after a crash restore `restore= \
                     restore_why= in_flight_lost= tail_overflow=<0|1> marks_lost= \
                     resizes_unjournaled=` follow",
                ),
            ],
        },
        Section {
            name: "landing",
            about: "Land's hold, the stop that stopped typing, and what the landing checked",
            rows: &[
                para(
                    "`land_at= task=<open|landed|theirs|failed|-> pause= land_held= land_nudges= \
                     land_since= held=<same-reply|no-work|-> \
                     waiting=<overload|api-error|connection|-> land_asked=<switch|-> \
                     landed_head=<hex|-> landed_on=<pct|-> \
                     landed_leg=<on-main|pushed|no-remote|no-repo|gone|-> landed_repo=<pct|-> \
                     goal=<pct|->`",
                ),
                row(
                    "task=",
                    "the task's face as the harness here last told it: `open` (the work goes on, \
                     or a landing is under way), `landed` (a clean `Nothing left:` defense at a \
                     land act), `theirs` (a person's own exchange), `failed` (the work ended \
                     unfinished: `Could not finish:`, or the same reply again with no work); `-` \
                     with no agent or nothing told here. The task record itself is \
                     `task_state=` (`help status records`)",
                ),
                row(
                    "land_held=",
                    "`land_held=`, `land_nudges=` and `land_since=` are Land's hold, aterm's own \
                     while it holds the claim or, under its own proven claim, codex-alab's (`meta \
                     set harness_land`): the legs of its last judged yield (`left` where \
                     processes its agent started still ran, neither ended nor kept), the nudges \
                     since its land act and that act's unix second, `-` each where no landing \
                     holds or its tab reads another mode, landed or quiet",
                ),
                row(
                    "held=",
                    "the hold that stopped the typing, aterm's own while it holds the claim or, \
                     under its own proven claim, codex-alab's (`meta set harness_held`), `-` \
                     while it types or its tab reads landed or quiet",
                ),
                row(
                    "waiting=",
                    "the API or network outage the harness waits out, typing nothing until its \
                     next try or the route's measured return, `-` while it waits on none",
                ),
                row(
                    "land_asked=switch",
                    "while Land waits for the agent's answer to the one question the person's \
                     switch to Land over a held tab typed (it types nothing more until it reads \
                     that answer), `-` otherwise",
                ),
                row(
                    "landed_head=",
                    concat!(
                        "`landed_head=`, `landed_on=`, `landed_leg=` and `landed_repo=` are what \
                         aterm checked at the landing that stands (its receipt): `HEAD`'s 12 \
                         digits, the remote main ref git named it on, the leg (`on-main`, \
                         `pushed` with no origin main, `no-remote`, `no-repo` for no repository \
                         checked, `gone` for a folder that is gone) and the repository or that \
                         folder, `-` each it does not name, and all four `-` beside no landed \
                         face or where ",
                        no_checked_record!(),
                    ),
                ),
                row(
                    "goal=",
                    "the active goal the agent in front keeps for itself, one line: Claude \
                     Code's `/goal` condition from its transcript; `-` while it has none or keeps \
                     none aterm reads",
                ),
            ],
        },
        Section {
            name: "records",
            about: "the task and log cursors, the rollout tap, the log journal, exec watch, bells",
            rows: &[
                para(
                    "`task_state= task_rev= log_seq= log_ep= log_base= run= turn= step= need= \
                     need_since_ms= rollout_asks= editing= editor=<amacs|-> bells= \
                     bells_sounded= last_bell=`",
                ),
                row(
                    "task_state=",
                    "the worker's task record: `working`, `done`, `exited`, `stalled` or \
                     `failed` (`help task states`), `-` with no task; `task_rev=` its rev (`0` \
                     with none), which `await task @<sid>:<rev>` waits past",
                ),
                row(
                    "rollout=<bound|partial|unbound|no-rollout|no-meta|other-thread|unknown-meta|\
                     no-thread|stale|stopped|->",
                    "what codex-alab's rollout tap last answered, under its proven claim or, read \
                     only, after this aterm saw that claim given back while its process runs on \
                     in front: `bound` (its rows read exactly), `partial` (a build the lab never \
                     measured, its rows read as `state=partial`) or why it bound nothing, \
                     `stopped` once it was dropped, and `rollout_asks=` how many asks it answered",
                ),
                row(
                    "log_journal=<on|armed|paused|stopped|handed|off>",
                    "what this instance's session log journal does, the one `log` reads a closed \
                     session from: `on`, `armed` while an update's successor waits for its \
                     commit, `paused` or `stopped` with `log_journal_why=` the cause, `handed` to \
                     an update's successor, or `off` with `log_journal_why=` why this launch keeps \
                     none",
                ),
                row(
                    "log_journal_why=<-|disabled|no-restore|no-desk|unsupported|no-folder|full|io|\
                     denied|no-lock|panic|spawn>",
                    "the cause beside `log_journal=` (`help log closed`)",
                ),
                row(
                    "exec_watch=<none|kqueue|sampler|connector|job>",
                    "what hears the session's `exec`s for `detail=`: `kqueue` (macOS's process \
                     watch), `sampler` (the group in front read while a block runs), `connector` \
                     (Linux's proc connector once its listener proved itself, the sampler beside \
                     it), `job` (Windows: the session's job listed as its output is read) or \
                     `none`",
                ),
                row(
                    "exec_watch_why=<-|off|no-route|proving|live|live-unmasked|socket|bind|send|\
                     no-ack|refused|no-probe|probe-unheard|probe-lost|failed>",
                    "how the connector stands: `-` where none serves (off Linux, or a process \
                     watch hears), `off`, `no-route`, `proving`, `live` (`live-unmasked`: the \
                     kernel took no event mask) or why it closed (`no-ack` in another user \
                     namespace, `bind` before Linux 6.6 without `CAP_NET_ADMIN`, `probe-unheard` \
                     when its own probe's `exec` never came, `probe-lost` when the kernel dropped \
                     messages while each probe's was awaited)",
                ),
                row(
                    "editor=amacs",
                    "while an `edit` from this session, which the `amacs` command run in its tab \
                     sends, opens an Editor tab beside it, `-` on a headless instance or with the \
                     session in no window",
                ),
                para("`-` where there is nothing to say."),
            ],
        },
        Section {
            name: "gen",
            about: "gen=, seq= and hash=, and the write fences that read them",
            rows: &[
                para(
                    "`gen=<epoch>.<seq> seq= hash=` end the line. `seq=` and `hash=` are `-` on a \
                     contended lock or an unfinished DEC 2026 frame, which `status` never waits \
                     for; `gen=` stands.",
                ),
                para(
                    "`if-gen=`, `if-human=` and `if-fp=` fence on `gen=`, `human_seq=` and \
                     `hash=`.",
                ),
            ],
        },
    ],
};
