---
name: system-state
description: Read what this machine, every aterm process and this tab are doing now (CPU per core, GPU, memory and swap, disk space, which processes use them, and which commands stopped making progress). Use for "slow", "hung", "stuck", "frozen", "out of memory", "swap", "disk full", "which process", "load", "is my command still working", "wait for a quiet machine", or "benchmark".
---
<!-- aterm skill v4 — MANAGED FILE, rewritten by `aterm agents` on every install/update; remove this marker line and aterm will leave the file alone (reported as `foreign`) -->

# System state in aterm

aterm samples the machine and counts what it sees in three scopes:

| scope | what it holds |
|---|---|
| `tab` | this tab: its shell and every process under it |
| `aterm` | every process of this aterm instance: all tabs, aterm itself and its helpers |
| `global` | the machine: this tab, the rest of aterm, your other processes, the OS's services |

`sys global` splits the machine's CPU into `tab`, the rest of aterm, your other processes and
services (other users' processes, computed by subtraction; a negative residual is printed signed).
Where ownership cannot be established (Windows supplies no UIDs), processes outside aterm have
`scope=unknown`: their measured CPU and memory remain available, while the own-processes/services
split reads `unavailable(not_sampled)`.

## Read

```sh
aterm ctl @self sys                 # one sentence per scope
aterm ctl @self sys tab --json      # this tab, for parsing
aterm ctl @self sys cores           # use per core, grouped by cluster
aterm ctl @self sys mem             # memory, compressor, swap, pressure, and who holds it
aterm ctl @self sys procs           # this tab's processes, busiest first
aterm ctl @self sys disk            # this tab's folders' volumes, and who writes
aterm ctl @self sys jobs            # this tab's commands and your tool calls: still making progress?
aterm ctl @self sys since=<cursor>  # what changed since the reply that printed next=<cursor>
```

Every reply's header (stderr) carries `as_of=`, `age_ms=` and `next=`. No Deck needs to be on
screen (a stale reading triggers a fresh sample within 1-1.5 s):

- `2.3c` is 2.3 cores' worth of CPU time per second. macOS does not report core placement:
  `sys cores` shows each scope's share of each cluster.
- `≥` marks a lower bound (a process refused a read); `≤` the upper bound beside it.
- `unavailable(<why>)` means not measured here, never zero (`unsupported`, `not_sampled`,
  `refused`, `warming`, `lane_off:…`). `state=stale` keeps the last values with their age.
- On Apple silicon the GPU reports one device-wide figure; writes are page-cache writes, not net
  disk growth; record lines use base units (`cpu_mc=`, `fp_bytes=`, `write_bps=`).
- `sys jobs` lists one `job` row per command in this tab or tool call (`kind=agent-call`;
  `kill -TERM -<pgid>` ends one). `class=` is `running`, `waiting` (with `wait=`), `polling`,
  `busy-silent`, `stopped`, …; `verdict=` is `fine`, `quiet`, `busy`, `slow`, `stuck`,
  `never-returns`, `waiting-you`, `parked` or `exited`. `stuck` requires a named wait and long
  silence against the job's usual gaps (`phi=` >= 8); an unnamed wait stays `quiet`, and `busy` /
  `slow` are never stuck. `no_progress=1` beside `quiet` means zero CPU in that silence.
- `--json` is one object (`schema` 1, `kind` `sys.<view>`); every figure is a bare integer,
  `{"v","bound","cause"}`, `{"why"}` or `{"lo","hi"}`.

## Wait instead of polling

```sh
aterm ctl @self await sys quiet for=20s timeout=600000              # the machine has gone quiet
aterm ctl @self await sys 'cpu<30%' and mem=normal timeout=600000
aterm ctl @self await sys 'tab<0.5c' for=10s timeout=600000         # this tab has settled
aterm ctl @self await sys 'free>50GB' timeout=600000
aterm ctl @self await sys 'free>=room' timeout=600000               # waits while aterm frees unused build caches
aterm ctl @self await sys job=a40977 done,stuck timeout=600000     # one job ends or sticks
aterm ctl @self await sys jobs.stuck=0 timeout=600000              # nothing in this tab is stuck
```

Quote terms with `<` or `>`. `await` answers `OK sys …` with the matching reading, or `OK timeout`
(exit 124) after at most 600 s; loop on 124 to wait longer. An unreadable term refuses at once
(`ERR unavailable <term> why=…`), except under `quiet`, which skips it (`skipped=`). An exited job
still meets `done` (`verdict=exited exit=<n>`); an unmeetable wait or unknown job answers
`ERR gone job=<id>`. Verdict words follow `job=`; class words follow `class=`.

## Hear about your jobs without asking

```sh
aterm ctl @self status                       # … disk=<vol> disk_free_mb=<mb> job=<worst live verdict|none|-> job_quiet_ms=<ms|-> …
aterm ctl @self brief                        # a `jobs …` line while one of this tab's jobs is live
aterm ctl subscribe @self events             # EVENT <n> job <verdict> job=<id> class=<class> rev=<n> [task=<id>] [code=<n>]
```

`job=` is the worst live verdict (`stuck` > `quiet` > `fine`), `none` with no live job, or `-`
when no recent reading speaks for now. `EVENT … job` fires only on verdict transitions (`task=`
names your tool call, `code=` its exit code).

## Acting on what you read

Processes of other tabs, other aterm instances and other users are shown as data, not yours to
signal. Process names come from those processes; arguments, environment and titles of other
processes are never printed, and paths print only for your own tab. `Operation not permitted` on
the control socket means a sandbox refuses it (`aterm help sys`).
