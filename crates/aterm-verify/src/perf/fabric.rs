// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify perf fabric` benchmark lane runner and provenance ledger writer.
//!
//! # The fabric ledger — what aterm's use of the astream bus costs
//!
//! Every speed claim about the fabric (aterm-link's bridge on astream's broker) is
//! a pair of entries in the ledger: a run before the change and a run after
//! it, on one machine. The runner is the `fabric` lane of `verify perf`
//! (this module); the measurement is Trust code in
//! the package it measures, `crates/aterm/benches/fabric_bench.rs`. Its shared pure
//! helpers remain checked by `tests/fabric_bench` during ordinary test runs. What the
//! committed entries read, the dated sections this protocol cites as *the readings*, is
//! `docs/measured/fabric/README.md`.
//!
//! ```sh
//! targo --unverified verify perf fabric --no-windows
//! # knobs (defaults shown):
//! targo --unverified verify perf fabric --no-windows --profile dev --latency-samples 200 \
//!   --burst 500 --pingpong-rounds 200 --attach-sessions 1,8,32 --attach-rounds 3 \
//!   --durability ordered --horizon-ms 100 --retention on --log auto --json-out /tmp/fabric.json
//! # what ships, taken apart: no bus mirror; no retention rules (and so no mirror);
//! # the retention tick every second instead of astream's minute
//! targo --unverified verify perf fabric --no-windows --log off
//! targo --unverified verify perf fabric --no-windows --retention off
//! targo --unverified verify perf fabric --no-windows --retention-interval-ms 1000
//! # idle sessions beside the two that talk, in the messaging and ping-pong worlds:
//! targo --unverified verify perf fabric --no-windows --bystanders 64
//! # the strict baseline, the default arm of every entry before 2026-10-20's re-gate:
//! targo --unverified verify perf fabric --no-windows --durability strict
//! # the optimized build of the same stack:
//! targo --unverified verify perf fabric --no-windows --profile opt
//! # the release's code generation, with only the development seams on top:
//! targo --unverified verify perf fabric --no-windows --profile shipped
//! # one scenario only, with no worlds started for the others:
//! targo --unverified verify perf fabric --no-windows --scenario pingpong
//! # what agents and people wait on (asks, receipts, the person lane, two nodes):
//! targo --unverified verify perf fabric --no-windows --scenario ask --asks 100 --mix-every 10
//! # the same with the asked table pre-filled to its cap of 1024 rows (C04):
//! targo --unverified verify perf fabric --no-windows --scenario ask --asked-prefill 1024
//! # the attach with every session's refill watermark N records behind the head, and a
//! # paced writer timing its acks across it (B09):
//! targo --unverified verify perf fabric --no-windows --profile opt --scenario attach \
//!   --attach-sessions 8,32 --attach-backlog 500000
//! # the ceiling: the same stack with the broker's flush taken out
//! targo --unverified verify perf fabric --no-windows --durability relaxed
//! # the tiers side by side, one entry per arm, interleaved A B C D D C B A:
//! targo --unverified verify perf fabric --no-windows \
//!   --durability strict,ordered,relaxed --horizon-ms 1000,100 --passes 2
//! # every world's broker as `aterm link broker` in a process of its own, started as the
//! # shipped Background job's task type against an Interactive job's, A B B A (F02):
//! targo --unverified verify perf fabric --no-windows --profile opt --attach-sessions 8,32 \
//!   --broker-process background,interactive --passes 2
//! ```
//!
//! A default run took about a minute and a half on an M-series Mac before the
//! `ask` scenario joined it, which adds 25-46 s (`-185` to `-192`, the runs no
//! other disk work disturbed: relaxed the shortest, strict the longest); it opens
//! no window. Each run writes `<date>-fabric.md` (the tables, under the run's
//! provenance) and `<date>-fabric.json` (every raw value beside its summary) into
//! the verify state directory, `<target>/verify/.ubd/perf.noindex/fabric/`
//! ([`super::ledger_dir`]); a second run on one day gets `-2`, `-3`, …. An entry is
//! local state and never edited: a number a claim rests on goes in the lab tracker
//! with the command that produced it (`goals.py measure`). A list of arms (`--durability a,b,…`, an
//! ordered tier once per `--horizon-ms`) is one build and one run and entry per
//! arm, back to back; `--passes N` runs the list N times, forward then backward,
//! so one invocation interleaves the arms as 'How to compare two
//! runs', below, asks. Each entry's **Run** line reproduces its
//! own arm alone.
//!
//! ## What runs
//!
//! `--scenario all|messaging|pingpong|attach|handoff|ask` selects the scenarios in
//! each run. The default is `all`, every scenario in turn (`ask` joined it with
//! B11, so an `all` entry before it has no `ask` or `cross_node` section).
//! `messaging` includes both post-to-deliver and burst measurements. A selected
//! run names its scenario in the entry and reproducing command; the other JSON
//! sections are `null` and their tables are absent. Existing all-scenario reports
//! keep their populated sections and canonical run words.
//!
//! `--paced-notes N` (C13; default 0, none) adds a phase to the messaging world
//! after its burst: N one-way notes from A to B, 1.1 s apart (`NOTE_PACE`, past
//! the bus mirror's one snapshot of a moved mail count a second), none answered,
//! each after the one before has landed its inbox commit. Each delivery so owes
//! the mirror B's snapshot, as an agent's delivery does, which the latency
//! phase's round trips, faster than that rate, mostly do not. Its section (the
//! JSON's `messaging.paced_notes`) gives each note's writer batches, the flushes,
//! visible -> the inbox commit acked, the `ev` rows that landed between a
//! delivery and its commit or in the commit's own batch, and each note's shape:
//! its records in the visible head's steps (`|` between steps, `+` within one;
//! `note` the delivery, `commit` its inbox commit, `evA` the sender's mirror row,
//! `evB` the recipient's), then, after `||`, what landed past the commit's step
//! before the next post (`paced_shape`; entries before `2026-11-03-fabric-59`
//! have no shapes). A step is one writer batch as far as the head's 100 µs
//! sampling tells two apart. An entry that names no `paced_notes` word ran
//! without it.
//!
//! `--rate-limit on|off` (E06; default on) opens each world's broker under
//! aterm's default rate limit (`aterm_link::broker_rules::limit`, what `aterm
//! link broker` sets at every open), or under none. Every entry since E06 names
//! `rate_limit=on` or `rate_limit=off` and says the limit in its **Rate limit**
//! line; an entry that names neither ran with no limit. Each window counts the
//! publishes the broker refused for the rate (`BrokerStats::rate_limited`). A
//! broker the bench restarts (`World::start_broker`) opens under the default
//! again, in an `off` run too; the messaging scenario restarts none.
//!
//! The real stack, booted the way the fabric test worlds boot it
//! (`crates/aterm/tests/fabric_world/harness.rs`): astream's broker opened as aterm
//! opens it — `Broker::open_opts` with aterm's own options for the arm's tier
//! (`aterm_link::durability::Durability::options`: the tier and, on a weak one,
//! aterm's escalation table), guarded — inside the bench process; the headless
//! `aterm` this checkout builds; and the `aterm link serve` bridge that `aterm`
//! launches and supervises. Nothing is stubbed. Four worlds, each opened at the
//! arm's tier: one for messaging, one for the ping-pong arm (whose bridge runs with
//! a development seam set, its wait tally, and whose aterm with its handler tally, in
//! every run), one for attach, and one for the handoff (whose instances run with
//! the `ATERM_DEBUG_QUIT_BYE=handoff` seam; entries before `2026-10-22-fabric-104`
//! have three worlds and no handoff section). The messaging, attach and handoff
//! worlds run both tallies only in a run that names `--tally` (since D02). The
//! `ask` scenario (B11) adds two: one node whose bridge runs with the wait tally on
//! (and its aterm with the handler tally under `--tally`), and a fleet of two
//! headless aterms, each with its own bridge and tally, on one broker
//! (`harness::Fleet`). Each world's section says
//! what its broker SERVES, read from the broker (astream serves ordered strict
//! off Apple and off the boot volume).
//!
//! **Retention and the mirror, as shipped** (since `2026-10-22-fabric-104`; every
//! earlier entry measured a broker that retained nothing and a bridge whose
//! mirror was off). Every world's broker is opened through the harness's
//! `open_broker`, which calls `aterm_link::broker_rules::retain` on it, as
//! `aterm link broker` does at every start: aterm's four rules (the `ev` bulk
//! stream a 7-day, 256 MiB prefix; presence the last value; mail held until read,
//! 64 MiB; everything else 32 MiB, 7 days). Setting them makes them durable in
//! the log's held store (`<log>.held`) before it returns, one flush at the open,
//! outside every window; asks a retention tick at once; and from then a tick runs
//! when a batch's bytes pass a rule's bound or the interval passes (astream's
//! minute). The bench's bytes are far below every bound and no world lives a
//! minute, so at the shipped interval no window holds a tick: each window's
//! `Retention ticks` line says how many ran and were skipped
//! (`BrokerStats::retention_ticks`, `retention_skips`). Over a retaining broker
//! the bridge's bus mirror is on by default (`--log auto`, session log step 6E-3):
//! it mirrors the session logs' bus class onto the sessions' `ev` faces, and the
//! census counts those records by kind. Each world's section reads the rules and
//! the tick's interval off its broker and names the bridge's `--log`.
//!
//! - `--retention off`: every world's broker opened with no rules: what `aterm
//!   link broker` still serves, unretained, when its rules cannot be set (a
//!   poisoned log, a held store that cannot be written; `aterm-link` `cli.rs`), and
//!   every broker before astream's R4. The `auto` mirror stays off over it too, so
//!   this takes both away. (The Retention line of `-127` to `-130` says aterm no
//!   longer opens such a broker; it does, as that fallback.)
//! - `--log off`: the rules kept, the mirror off (`--log off` on the bridge's
//!   serve line), so `--log off` against what ships is the mirror's cost and
//!   `--retention off` against `--log off` is the rules'.
//! - `--retention-interval-ms N`: the retaining broker's tick every N ms
//!   (`Broker::set_retention_interval`, after the rules), so ticks land inside
//!   the windows. Not what ships.
//! - `--at-rest on|off`: every world's broker log sealed at rest under a bench key,
//!   as a `rest.key` beside the log seals aterm's; `on` is the default, what ships.
//!   `--at-rest off,on --passes 2` is one A B B A set. The backlog arm's 'A
//!   broker's start on this log' row (`backlog.reopen` in the JSON) is what the
//!   seal costs a start. An entry that names no `at_rest` word ran plaintext: every
//!   entry from before C10, and the off arms of its own set.
//!
//! Each reaches the entry's Run line only when given; the entry's words always
//! name `retention=`, `log=` and `at_rest=`.
//!
//! **The arms, `--durability strict|ordered|relaxed`.**
//!
//! - **strict** (every entry's yardstick unit, and the default arm of every entry
//!   before the readings' re-gate): every ack waits for a full flush, which is
//!   `F_FULLFSYNC` on macOS. *Acked* and *durable* are one moment.
//! - **ordered** at `--horizon-ms` (aterm's 100 unless named): every ack waits
//!   for an ordering barrier (`F_BARRIERFSYNC`); once the horizon has passed since
//!   the first ack no full flush covers, the writer flushes in full after the
//!   batch in hand — so an ack is durable at most the horizon plus a batch and a
//!   full flush after it (up to 145 ms at 100 ms in the readings' re-gate) — and it
//!   flushes in full before the ack of every record aterm's table escalates (a
//!   halt, a person's own words, mail to a person, a person's read mark — none of
//!   which the bench's messages are, so no number here includes that flush, but
//!   the `ask` scenario's person-lane legs, which post mail to a person). The
//!   real table and aterm's horizon, so the arm is what `aterm fabric on`'s broker
//!   runs by default (since the readings' re-gate; astream serves it strict off macOS
//!   and off the boot volume, and the entry says what was served). It is the
//!   DEFAULT ARM: a run naming no `--durability` measures it, and its entry names
//!   it (`durability=ordered horizon_ms=100`, and `--durability ordered
//!   --horizon-ms 100` on its Run line), so a bare entry of either era says which
//!   it is.
//! - **relaxed**: acks after the batch's page-cache write, no flush. It is not what
//!   aterm runs by default; it is the bound a cheaper durable tier could approach
//!   and never pass, so an entry beside it says how much of a message is the
//!   broker's flush. No record of its run is ever *durable* (its durable head
//!   moves only at the open and a clean close): those rows say `not seen`.
//!
//! The yardstick's unit stays one strict publish, on a strict broker of its own
//! when the arm is not strict, so the arms share a unit. The bridge's own
//! state-file writes are unchanged in every arm.
//!
//! ## Three builds: `--profile dev|opt|shipped`
//!
//! `dev` and `opt` enable debug assertions, so the harness's test seams
//! (`#[cfg(any(test, debug_assertions))]`, and the `ATERM_FABRIC_COMMAND` seam that
//! points `aterm` at the bridge) are compiled in. They differ from each other in
//! code generation only. `shipped` is the code the release builds, with the seams
//! asked for by feature instead:
//!
//! - **`dev`** (the default, and the arm of every entry written before the opt arm
//!   existed; an entry's **aterm** line names its arm):
//!   the explicit `test` profile, preserving the original measurement's build
//!   instead of the bench default's release profile. Opt-level 0 except the root
//!   manifest's per-crate
//!   speed rows, from two tables: `[profile.dev.package.<name>]`, which `test`
//!   inherits, and `[profile.test.package.<name>]`, the test profile's own
//!   (aterm-effects is at 1 by that one). So aterm-link, astream-broker,
//!   aterm-gui and most of the path run unoptimized, and its times overstate the
//!   CPU part of every cost. Overflow checks on. The entry's **Build** line names
//!   which crates on the path the rows reach.
//! - **`opt`**: the root manifest's `[profile.opt-test]`, which inherits
//!   `release`: opt-level 3 on every crate, no LTO, 16 codegen units, debug
//!   assertions on, overflow checks off (release's — the arm stands in for the
//!   shipped build). Not a profile inheriting `dev`: that keeps the dev speed
//!   rows, which outrank its own `package."*"` (measured: aterm-spec stays at 1,
//!   aterm-core, aterm-grid, aterm-render and four more at 2).
//! - **`shipped`** (since C07): the root manifest's `[profile.fabric-shipped]`, which
//!   inherits `release` and changes only `strip = false`, so a sample of it reads: fat
//!   LTO, one codegen unit, debug assertions off and overflow checks off are the
//!   release's own. A later change to `[profile.release]` moves the arm with it,
//!   and the audit below then refuses the run until `Profile::Shipped` promises what
//!   the release now does. Built with `--features dev-seams`, which makes it the
//!   release's code generation of more code than the release has. The feature compiles
//!   in every `dev_seam!` read, where a release build compiles the read to `None`, and
//!   the ping-pong world sets seven: the launch line (`ATERM_FABRIC_COMMAND`), the stale
//!   horizon, the two tallies, the `aterm fabric` socket bound, and the bridge's ack and
//!   live read deadlines, which a world holds at the hang detector so a late answer on a
//!   loaded machine does not take the link down. It compiles in the two tallies the
//!   bench reads (the bridge's rows of the state dir, aterm's handlers), which count
//!   while their seams are set, at what they cost in the other arms: a clock read and a
//!   short lock a span, and the bridge's idle write. And it compiles in code with no
//!   seam read at all: `metrics.rs`'s `submit_split` reads the clock, the thread's CPU
//!   time and the page faults on each frame, and `as=person` and the security
//!   fixtures are compiled in though the bench calls neither. The bench compiles and
//!   runs with `cfg(any(test, debug_assertions))` off, so it calls nothing that gate
//!   holds and drives aterm by those seams alone. A profile of its own means a directory of
//!   its own, `target/fabric-shipped/`, so another
//!   session's `--release` build never rewrites the binary the identity check below
//!   pins. Its fat LTO links two binaries (`aterm` and the bench, which holds the
//!   broker) and takes far longer to build than `opt`. `restore-e2e` refuses it:
//!   that bench drives seams only debug assertions compile in.
//!
//! Before measuring, the lane reads cargo's plan for the arm (`--unit-graph`) down
//! to the units the measured processes execute — the bench (it holds the
//! broker), the `aterm` binary (the window, and the bridge it launches), and every
//! library they link; not build scripts, proc macros or what only they link — and
//! refuses an arm whose units are not what it promises: `opt` and `shipped` at
//! opt-level 3; `dev` and `opt` with debug assertions, `shipped` without; overflow
//! checks on in `dev` and off in the other arms; `opt` with
//! no LTO and 16 codegen units, `shipped` with fat LTO and one. The plan cannot show
//! the rustflags cargo appends to every rustc line, where a later `-C opt-level`
//! wins over the profile's. So the lane removes every environment source of them
//! (`RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, `CARGO_BUILD_RUSTFLAGS`,
//! `CARGO_TARGET_<TRIPLE>_RUSTFLAGS`: the caller's, which AGENTS.md forbids
//! anyway) from every targo it runs. It reads the config files' rustflags
//! (`targo config get`, a user-level `~/.cargo/config.toml` included) and refuses
//! any that sets opt-level, debug assertions, overflow checks, LTO or codegen units. The entry's
//! **Build** line quotes the plan and every config rustflag (today the checkout's
//! own `-Zthreads=4`), and the entry's JSON carries the same line as `lane_build`.
//!
//! Then the lane builds, and takes the `aterm` cargo reports
//! (`target/debug/aterm`, `target/opt-test/aterm` or `target/fabric-shipped/aterm`) and that file's identity
//! (length and modification time). The bench reports the path it launched
//! (`aterm_binary`) and the file's identity at its start and its end
//! (`aterm_binary_identity`). The lane refuses numbers from any other path, and
//! from that path once another build has rewritten it: the harness launches by
//! path, the bridge is relaunched on every attach round, and `targo build -p aterm`
//! rewrites `target/debug/aterm` with the dev profile, not `test`.
//!
//! **The build's git facts are pinned for the whole plan.** aterm's build script
//! stamps the binary with HEAD's commit, and unless `ATERM_BUILD_GIT_COMMIT`,
//! `ATERM_BUILD_GIT_COMMIT_FULL` and `ATERM_BUILD_DEV_COMMITS` are all set it
//! watches git paths in the repository's shared common dir (`packed-refs`, the
//! nearest `refs/heads/…`): a commit, `fetch` or `pack-refs` in any worktree
//! re-runs it, and the next `targo bench` relinks `aterm`. In a shared checkout
//! that lands between the runs of a plan of several arms, which the identity
//! check then refuses (the re-gate's first pass, in the readings). So the lane reads the
//! three facts once, as the build script would (a caller's own three pins are
//! kept), and sets them on every targo it runs, the plan, the build and each run:
//! the script watches no git path, and each run's `targo bench` builds nothing.
//! The entry's **Build** line names the pins. The stamp's `-dirty` is of every
//! path but `docs/measured`, which no build reads (`crates/aterm/stamp_paths.rs`),
//! and the entries a set's runs write go to the verify state directory under the
//! build's target directory, so with the default target (`target/`, which git
//! ignores) or one outside the work tree they leave the next run's pins, and its
//! build, as they were; an edited or new source file still marks the stamp dirty.
//!
//! **Disk.** The opt arm shares no unit with dev. Each arm's build of this bench
//! was measured on 2026-10-17 in an empty target directory:
//!
//! | arm | total | files | `deps` | incremental | `aterm` | build scripts |
//! |---|---:|---:|---:|---:|---:|---:|
//! | `opt` | 727 MiB | 1,289 | 615 MiB | none | 83 MiB | 25 MiB |
//! | `dev` | 3.7 GiB | 14,207 | 934 MiB | 2.6 GiB | 203 MiB | 32 MiB |
//!
//! The dev arm's bulk is incremental state. It carries no line tables:
//! `[profile.test]` sets `debug = false`, and only build scripts and proc macros
//! get them (`[profile.dev.build-override]`). The lane builds where a shell's
//! `targo --unverified test` builds. Under atpkg's shim, the units go to the
//! shim's build directory
//! (`~/Library/Caches/aterm/cargo-build.noindex/<checkout>/.cargo-build/<profile>`)
//! and the binaries go to `target/<profile dir>/`. The lane passes the shim's own
//! `--config` for that, so the lane, `verify`'s test builds and a shell build of
//! the same profile all share one copy. A caller's `CARGO_TARGET_DIR` turns the
//! shim's rule off, and then every build goes to that directory. Until 2026-10-17
//! the lane compiled into `target/<profile>/deps` while shell builds compiled into
//! the shim's directory, which kept two copies of every unit.
//!
//! ## What each number means
//!
//! Every distribution is `n`, mean, and nearest-rank p50/p95/p99 in milliseconds
//! (the value at rank `ceil(p·n)`, always one that was measured). With fewer than
//! 100 values the p99 is the maximum; with fewer than 20 so is the p95. Read `n`
//! first.
//!
//! All times are stamps on one monotonic clock in the bench process. Outside
//! stamps: before a request is written and after its reply is read. The broker's
//! side is the one thing the outside cannot see, and it has two moments, both
//! read off the in-process broker's atomics, sampled together every 100 µs: each
//! message's record offset is read back from the log after the run, and the
//! record is **acked** at the first sample of the broker's visible head past its
//! offset (its publish acked and subscribers may see it: on strict after the full
//! flush, on ordered after the barrier, on relaxed after the page-cache write) and
//! **durable** at the first sample of its durable head past it (on stable media by
//! a full flush the broker saw succeed: on strict the same moment, on ordered the
//! horizon's or an escalation's flush, on relaxed never). An ordered world waits
//! after its timed phases for its durable head to reach its acked one, so the
//! last records are seen durable.
//!
//! ENTRIES BEFORE THE ORDERED ARM (every `2026-10-16` entry, and `2026-10-17-fabric`
//! through `-36`) called the acked moment *durable*: on the strict broker they all
//! measured, the two were one, and on `-36`'s relaxed one it meant *acked into the
//! page cache*. Their JSON's `post_to_durable_ms`, `durable_to_visible_ms`,
//! `visible_to_commit_durable_ms`, `all_durable_ms` and `durable_rate_per_s` are
//! today's `post_to_acked_ms`, `acked_to_visible_ms`,
//! `visible_to_commit_acked_ms`, `all_acked_ms` and `acked_rate_per_s`; the burst's
//! `broker_flushes` (writer batches that appended a record) is `broker_batches`;
//! the ping-pong's `pong_post_to_durable_ms` and `pong_durable_to_visible_ms`
//! (`-35`, `-36`) are `pong_post_to_acked_ms` and `pong_acked_to_visible_ms`. The
//! durable head's own columns are new: `post_to_durable_head_ms`,
//! `all_durable_head_ms`, the ping-pong's `ping_post_to_durable_head_ms` and
//! `pong_post_to_durable_head_ms`, and attach's `attach_durable_ms`. The entries
//! `-37` to `-53` were measured the same day on the unported ordered/5's own
//! branch, and renumbered past `-35` and `-36` when it was ported (each says so):
//! they use today's names, and have no ping-pong section.
//!
//! **Messaging, post -> deliver (one node).** Session A posts `kind=note` with no
//! `--wait` (aterm answers `OK` once the post is queued in A's outbox) to session
//! B; the row is *visible* when B's parked `await inbox` answers. One message at a
//! time **on an idle bridge**: the bridge delivers a record and then commits its
//! inbox cursor past it — one more Strict append after the row is visible — so
//! each round trip waits for that commit to land before the next post, or every
//! sample would carry the previous message's tail. Rows:
//!
//! - *post call*: the `post` request to its `OK`.
//! - ***post -> visible***: the headline — outbox drain, publish + fsync, the
//!   bridge reading its inbox subscription, `deliver` into B's ring, B's wait
//!   waking.
//! - *post -> acked* and *acked -> visible*: the same span split at the record's
//!   ack.
//! - *post -> durable*: when the record was on stable media — on ordered, up to
//!   the horizon after it, which is the number the horizon bounds.
//! - *visible -> the inbox commit acked*: the commit's tail, paid after the row
//!   is visible (and by any message right behind it).
//! - *broker flushes*: barrier, full and skipped full flushes over the window
//!   (`Broker::flush_counts`), each per delivered message — strict spends a full
//!   flush per batch, ordered a barrier per batch plus a full flush per horizon
//!   (and per escalated batch), relaxed nothing.
//! - *retention ticks* (since `-104`; also the burst's and the ping-pong's): the
//!   retaining broker's ticks run and skipped over the same window
//!   (`BrokerStats::retention_ticks`, `retention_skips`): 0 at the shipped
//!   minute, about one a second under `--retention-interval-ms 1000`, and none
//!   ever under `--retention off`.
//!
//! **Burst.** N posts back to back to B, from `ceil(N/100)` sender sessions (so
//! no session's share nears aterm's 128 queued posts, `OUTBOX_CAP`). B reads like
//! a reader: after each wake it LISTS what arrived (`inbox since=<id>`, not
//! `--peek`), because aterm's ring holds at most 64 unlisted rows from one peer
//! node (`SENDER_QUOTA`), every sender here is the same node, and a refused
//! delivery is final (an `undeliverable` on the sender's lane, never retried). So
//! the sender also keeps **at most 56 posts in flight** — posted and not yet listed
//! — and waits for the reader before the next: a reader can always be descheduled
//! for longer than a fast bridge takes to deliver 64 rows. Back to back, then,
//! wherever the endpoint permits it. Milestones from the first post: every post
//! accepted (which trails the reader by at most the window), every record acked,
//! every message visible (with messages/s), every record durable, and the moment
//! the recipient bridge's inbox commit covers the last one. Then each message's
//! own post ->
//! visible, which in a burst is mostly its wait in that window. Every message must
//! be seen exactly once — a duplicate delivery or a late repeat fails the run by
//! name.
//!
//! **Ping-pong (request and reply).** A posts `ping-<i>` to B; the moment B's
//! `await inbox` answers, B lists it and posts `pong-<i>` back; the moment A's
//! answers, A lists it and posts the next — `--pingpong-rounds` rounds (default
//! 200) after the warm-up, back to back, in every tier arm. It is what the latency
//! phase leaves out on purpose: there the bench waits for the bridge's inbox commit
//! before the next post; here every message but the first is posted while the
//! commit of the delivery it answers is still owed. The bridge has one loop, which
//! sends that delivery's commit before it reads the outbox the answer sits in, and
//! reads every publish's ack on the same loop. Until aterm-0 (2026-10-21, in the readings) it
//! parked on the commit's answer and on every owed ack at an idle moment, so the
//! answer sat in the outbox until the commit was back; since, the commit rides the
//! publisher's pipeline and the loop takes the answer while the commit is in the
//! broker's flush, publishing it behind the commit on the same connection. Not
//! always in that order: the roster round at the top of a loop pass reads the
//! outbox too (`Bridge::roster_backstop`), before that pass commits, and publishes
//! an answer it finds there ahead of the commit. When the bridge has also
//! delivered that answer by the time it commits, one commit covers both
//! deliveries (a fold; see which counts compare, below). Rows:
//!
//! - ***round trip***: A's post to B's reply visible to A, and its legs (each
//!   post `OK` -> visible); B's turnaround is the bench's own wake, list and post.
//! - *ping and pong: post -> acked*, *pong: acked -> visible* and *ping and pong:
//!   post -> durable*: each leg split at its record's two moments, as in the
//!   messaging rows.
//! - *the answer's wait for the delivery's commit*, by direction (a pong answers
//!   the ping before it, the next ping that pong): from its post `OK` to the moment
//!   the commit covering the delivery it answers was acked (what the bridge's loop
//!   waits for: on strict its full flush, on ordered its barrier), read off the log
//!   and the acked head's sampler like every acked time here; 0 when the commit
//!   landed first. (`-35` and `-36` read it as durable: on their strict and
//!   relaxed brokers the sampled head was the acked one.) Beside it, how many answers' records landed after that commit. Only
//!   those carry a wait: an answer recorded before the commit was published ahead
//!   of it, so it never waited on it (`-36`, taken before this rule, counted the
//!   gap for its one such answer too). The report says how many of those were
//!   folds — the commit covered the answer's own delivery as well, one commit for
//!   two deliveries — and how many were not: an answer published ahead but
//!   delivered after the commit has a commit of its own, and its round trip
//!   still wrote two.
//! - ***blocked on the broker***: the share of the window — first measured post to
//!   the commit covering the last reply acked — that the bridge's loop spent
//!   waiting on broker replies, and per round trip, by kind: requests that read
//!   their own reply and pipelined acks, and of both the inbox group's commits
//!   (requests until aterm-0, pipelined acks since), in count and time. The bridge
//!   counts these itself, where it blocks (`aterm_link::wait_tally`:
//!   `Bridge::observe` for every request, `Bridge::settled` around each ack read
//!   alone, a read that does not wait included at what it cost), because no
//!   outside clock sees them. With the development seam `ATERM_LINK_WAIT_TALLY` set — in the
//!   ping-pong world alone — the loop writes its tally to `<state>/wait-tally` at
//!   its idle moments (one positioned write of a constant-length line carrying its
//!   own checksum, so a racing read is refused, never half counted). The bench
//!   waits for the tally past the warm-up's last reply and for the one past the
//!   measured last reply, refuses a pair from two bridge processes, and divides
//!   their difference by the window. The rest of a round trip is the loop's own
//!   work (its `deliver` and outbox round trips to aterm, its state files),
//!   aterm's, the scheduler's, and the seam's own: its clock reads, and that
//!   write, which a delivery that arrives while it runs waits behind
//!   (`aterm_link::wait_tally` lists each cost; before aterm-0 the write came
//!   after a settle that blocked on every owed ack, and a delivery queued during
//!   that settle waited for both).
//! - ***where the loop's time went*** (wait tally v=2, since `2026-10-22-fabric-133`):
//!   the same two tallies carry every step the loop takes, each a count, a
//!   total, its largest and its log2 bands (`aterm_link::wait_tally::Row`): the
//!   broker rows above; each verb-lane round trip to aterm by verb (the outbox
//!   peek, a drain's roster read `sessions bridge`, a post's retirement `outbox
//!   sent`, `deliver`, every other); each state-dir write by kind (replace,
//!   unlink, append, flush) and by the call that made it (`set_post_pin`,
//!   `set_in_flight`, `clear_in_flight`, `clear_post_seq`,
//!   `append_self_acked`); each mailbox item's time from its push to its take,
//!   apart for one that came while the loop was parked (a wake) and one that came
//!   while it worked (a wait behind that work); a post's path from its `post`
//!   event to its publish written; the posts each peek listed; the loop's park;
//!   and the tally's own writes. With the bridge's own clock at each write, the
//!   window is partitioned: the rows marked `partitions` are disjoint, and the
//!   bridge's clock less them is the REST, the rest of the loop's wall time. It
//!   includes uninstrumented work and time the thread was not scheduled; no CPU
//!   clock is read. The attribution table ranks them per round trip, each a
//!   share of the round trip's mean. The bridge's clock runs from one tally
//!   write to the other and
//!   holds time that no round trip does, of two kinds. One is the bench's own
//!   work between rounds: A's `list_since` of its inbox after each pong
//!   (answered by aterm alone), then the next post's start. It makes the bench's
//!   window longer than n round trips: by 11.3-11.8 ms on quiet dev (0.06 ms a
//!   round trip), 14.6-17.0 ms loaded and 4.9-5.4 ms on opt, in `-133` to
//!   `-148`. The other is the window's edges, the bridge's clock past the window
//!   (4.7-7.4 ms). Together they come to 0.084-0.096 ms a round trip on quiet
//!   dev, 0.103-0.114 loaded and 0.051-0.058 on opt. Whatever the loop did in
//!   that time (its park, a commit's ack, a tally write) is in its rows. So the
//!   shares sum past 100% (102.5-108.5% in `-133` to `-148`), and each row's
//!   share, the park's included, is an upper bound on its share inside the round
//!   trips. Since the B10 review, the report states this excess in its
//!   wait-tally line. Columns `≥ 1.05 ms` and `≥ 8.39 ms` count a row's tail from
//!   its bands; a row's largest in a window is exact when it is the largest the
//!   process saw, and otherwise bounded by its top band (`< x`). The tally's own
//!   writes are its own row (about 7 us each, 4 a round trip).
//! - ***aterm's handlers*** (handler tally v=1, since C07): the ping-pong world's
//!   aterm runs with the development seam `ATERM_FABRIC_HANDLER_TALLY`, which times
//!   the bodies of what the bridge's verb lane and the bench reach: `deliver`
//!   (`wake_watchers` apart, the ring of the session's watchers inside it and
//!   `outbox sent`), the outbox peek, `outbox sent`, `sessions bridge`, every other
//!   bridge-only verb, `post` from its entry to its push-lane event and the wakes
//!   behind it, `inbox`, `await inbox` from its entry to its first park, and a parked
//!   `await inbox` from its row's arrival in the ring to its answer
//!   (`aterm_types::wait_tally::HandlerRow`). A body runs from the dispatcher's call
//!   to its return: the socket, the reader thread's wake and the request's framing
//!   are outside it. aterm has no loop and so no idle moment to write at, and a write
//!   on these paths would count in what they measure. So the line goes to
//!   `<state>/handler-tally` (the instance's own state root, `dirs::state_dir`) on
//!   the one request that has nothing to measure, an `await inbox … timeout=0`, before
//!   it answers: its reply proves the file holds every span that ended before the
//!   request, and the request counts in no row. The bench sends it right after each
//!   of the bridge's tallies, whose idle write proves the bridge's verbs are done, and
//!   divides the difference by the round trips. The seam's cost is a clock read at
//!   each end of a span and one short lock.
//! - ***the requests, split***: each verb the bridge's tally times, and the bench's
//!   own `post`, from outside and by aterm's handler, in µs a call and ms a round
//!   trip. What lies between is the socket both ways, the reader thread's wake, and
//!   framing and dispatch. A row of *the verb lane, all* says what lever A could take:
//!   a round trip's handler time is what faster handlers can reach, and the rest is
//!   what only fewer round trips can.
//! - ***the park, split***: the bridge's park is its loop waiting for aterm or the
//!   broker, so it holds aterm's answers to the requests, the bench's turn and the
//!   wakes. Means per round trip from the bench's stamps, beside the tally's park: each
//!   wake to its `inbox` listing's reply (aterm's answer and its socket, which a real
//!   agent calls too after a wake), that reply to the post's request (the bench's own
//!   turn, which a real agent replaces), each `post` request to its reply (aterm's
//!   answer and its socket), and what is left, wakes and idle. A's two are outside the
//!   round trip. Before C07's review the first two were one row, `the bench's turn`,
//!   in `2026-11-02-fabric-25` to `-32` too, and its JSON `b_turn` and `a_turn` held the
//!   listing. They are
//!   measured beside the park and not inside it: the loop may be working (a commit's
//!   ack, say) through part of a span, so the rest can read below zero. A negative
//!   rest says the spans overlap the loop's work by at least that much.
//! - ***paced*** (since `-133`): `paced=` more round trips (50 by default) after the
//!   back-to-back ones, each 20 ms after the last one's reply, past the bridge's 5
//!   ms idle backstop, so each finds the loop parked with nothing owed: a request
//!   and its reply on an idle node. Their round trips and the tally's rows over
//!   their window, with the three splits above; the park there holds the gaps.
//! - *bystanders* (since `-133`, `--bystanders N`, 0 unless named): N idle sessions
//!   spawned beside the messaging and ping-pong worlds' two, which never post or
//!   read, so every walk over the node's sessions carries them (the outbox
//!   peek, a drain's roster read, the bridge's per-session maps). The latency,
//!   burst and attach worlds still run with no seam set.
//! - *broker flushes*: barrier, full and skipped full flushes over the same
//!   window, read at its two tallies (`Broker::flush_counts`), per delivered message
//!   and per round trip.
//!
//! **Attach.** The bridge is SIGKILLed and aterm's supervisor relaunches it at once
//! with N sessions to announce. The clock starts when the new bridge writes its pid
//! file (its aterm connection is up; a thread of its own watches for that from the
//! kill on, and until `2026-11-02-fabric-21` the poll came after the kill window's
//! posts) and stops when its node row and every
//! session's presence row — the rows carrying its own pid and incarnation,
//! whichever lands last — are acked again (pid -> every row durable is the same
//! on the durable head). Inside that window the bench asks the
//! broker nothing: it holds one subscription to the node's presence rows, opened
//! before the kill and served as the bridge's own subscriptions are, and reads the
//! times off the log afterwards. Also: pid -> node row, node row -> last session
//! row (the per-session part; negative for a bridge that publishes a session's row
//! before its node's), kill -> new pid (the killed bridge's lanes ending plus exec;
//! before C05 a 200 ms back-off floor as well), kill -> every row. Each restart
//! first lets the bridge run 5.5 s, past the supervisor's 5 s `RELAUNCH_HEALTHY`
//! mark, so it has no back-off.
//!
//! The pid file is written right after the new bridge takes the state dir's lock,
//! so the attach window holds everything the bridge does before its rows: since
//! 768104f73e (2026-10-21, after `-103`) that includes `StateDir::begin_run`'s
//! per-open OPEN fence, at every attach, a restart's and a handoff's included:
//! the `run` record replaced with a full flush of the file and of its directory
//! (two full flushes; the bridge's stderr says so, `the run record says a bridge
//! runs in this boot: 0 barriers and 2 full flushes`, and `2 full flushes` before
//! B07). B07 (10bfba9cdb) flushed both by the barrier over any visible OPEN, a
//! restart's and a handoff's; ea323ca39b kept the barrier only over an earlier
//! boot's record (`2 barriers and 0 full flushes`), the first start of a boot,
//! which no restart or handoff here is. See 'The run fence by the barrier' in
//! the readings.
//!
//! **Attach's second stop, the live mail** (since `-104`). The moment the bridge
//! is killed, the boot session posts a note to itself; aterm holds it in the
//! session's outbox, and only the new bridge can publish it, once its live drain
//! starts: after the presence rows and the refill they owe (`Bridge::attach`).
//! *kill -> the first live mail visible* ends when the session's parked `await
//! inbox` answers with it; *every row acked -> the live mail visible* is the
//! part after the rows: over a retaining broker at `--log auto` (what ships), the
//! bus mirror's start in the fresh bridge, which holds up its first delivery
//! (about 8.5 ms of it on ordered, set 2 of the 2026-10-22 section), then the
//! refill and the drain. The bench checks that the
//! mail's record lands after the new bridge's node row (it is the new bridge's)
//! and leaves it out of the attach's census.
//!
//! **The asked table pre-filled, `--asked-prefill N`** (since C04; none by default,
//! and an entry without it names none; at most 1024, the table's cap). Before the
//! ask world's legs, N rows are written to `asked` in the state dir as a build
//! before the asks journal wrote the table, and the bridge is killed and restarted,
//! so it takes them in and rewrites the journal once. Each ask past the first 1024
//! offsets then trims the lowest row. The rows name a principal no ask goes to.
//!
//! **Attach's backlog, `--attach-backlog N`** (since `-207`, B09; none by
//! default, and an entry without it names none). Before each population's
//! restarts every session is sent a note and marks it seen, and the bench waits
//! until the bridge has written that session's refill watermark there
//! (`seen/<sid>` in its state dir); then N 128-byte records are filled,
//! pipelined, on `/f/<F>/pub/<node>/backlog/ev`, an `ev` face no session owns and
//! no bridge face reads. Each restart's bridge then refills every session from
//! its watermark across the backlog. In the kill window the boot session also
//! posts one note to every other session (*kill -> the kill window's posts
//! answered* times them; before C05 they ended before the new pid, and now they can
//! outlast it, which is why the pid is watched on a thread of its own from the kill
//! on), and *kill -> every session's live mail visible* ends when the last of them
//! is listed, so it waits out every session's refill. A paced writer, one bare publish every 5 ms on
//! `/f/<F>/bench/steady` over its own connection, runs across each population's
//! restarts; its acks are split into those that meet a round's kill -> every
//! live mail and those outside them (the backlog table), so a refill's hold of
//! the broker's log lock would show as that writer's stall. Its records and the
//! other notes stay out of the census. Run it with `--profile opt`: the broker
//! scans in the bench's own process, at opt-level 0 in the dev arm. Each
//! population fills N records of its own, 213 bytes each as astream's
//! retention counts them (`record_bytes`), so the bench refuses an N whose
//! fills pass the `ev` rule's 256 MiB at the run's attach sizes (420,086 at
//! the default three, 630,130 at two): past it the broker's retention, which
//! checks every 60 s from the open, would cut the fill inside the run (since
//! the B09 review; the `5e5` entries `-209`, `-210`, `-213` to `-216` filled
//! three populations, about 305 MiB).
//!
//! **Handoff** (since `-104`). A headless instance cannot update, so the arm
//! measures a seamless update's bridges' half on a fourth world. A successor
//! headless instance armed with the same fabric command (the same broker, cap
//! file and `--state`; a runtime dir and control socket of its own) comes up
//! beside the holder; its bridge finds the state dir held, reports `link down
//! reason=state-locked` and stands by: since H1 (8410b07cee) its take of the lock
//! waits in the kernel's queue and the kernel hands it the lock the moment the
//! holder lets go (`bridge.rs` `hold_state_dir`, `stand_by`); before H1 it tried
//! the lock every `STANDBY_POLL` (1 s). It hosts two sessions. The holder then
//! quits saying `bye handoff` (the QA seam `ATERM_DEBUG_QUIT_BYE=handoff`, as
//! `roster_bye.rs` and `handoff_cost.rs` use it): its bridge withdraws no session,
//! flushes nothing and exits as its lanes close, and the broker fires its will.
//! The moment the bench sees the old bridge gone (`kill(pid, 0)` refused, read
//! every millisecond), one successor session posts a note to the other, queued
//! while no bridge carries the node (before H1; since it the successor has taken
//! the dir by then, and the entry counts no post queued in the gap). The
//! successor's bridge takes the dir (before H1, at its next try), begins its run,
//! attaches (its node row and both sessions' presence rows under its own pid and
//! incarnation) and drains the post. Then the successor holds the node, and the
//! next round hands it on (`--attach-rounds` handoffs). Rows: quit -> the old
//! bridge gone; ***the old bridge gone -> the successor's every row acked*** (the
//! outage), split at the successor's pid file (it took the dir) and its attach
//! after it (until 5a4d746b26 the bench read that pid file only after 'gone' and
//! the gap's post, so on a build since H1, whose successor takes the dir before
//! 'gone', these three rows started late and are read as bounds in 'The standby
//! woken by the release' in the readings; since, it reads the pid file from the quit on,
//! beside its `kill(pid, 0)` poll, so a take before 'gone' reads below zero in
//! 'gone -> pid' and the attach row starts at the take: 'The take timed' in
//! the readings); quit -> the successor's every row acked (end to end; printed since
//! 5a4d746b26); the old bridge gone -> its will acked (the broker's view of
//! the same exit; a small negative value is the bench's poll seeing the exit
//! after the broker did); and the gap's post -> acked, -> ***visible***. Round k
//! of n waits (k - 1)/n of a second before its quit, so on a build before H1 the
//! rounds sample where in the one-second poll the exit lands; on a build since,
//! the spread changes only the moment of the quit, and both arms of an A B B A run
//! the same rounds. With no spread, the bench's own setup time fixed it (a smoke
//! run read 964 and 966 ms in two rounds). The spread is counted from that setup,
//! which starts at the standby's report of a failed try, so the rounds land near
//! 0.03, 0.37 and 0.70 of the poll (2026-10-22), and their mean wait, about 0.63
//! of the poll, overstates a uniformly placed exit's half. The census of a
//! handoff, from the mark to the successor's last row, leaves out the gap's post
//! and counts the old bridge's fired will apart.
//!
//! **Asks: what agents and people wait on** (`--scenario ask`, B11; in `all`
//! since its first entry). A world of its own, its bridge's wait tally on. An
//! ask (`post kind=ask dl=600000`) waits for its landing by default, as a task
//! does: aterm answers `OK <id> off=<n>`, RELEASING the caller, once the bridge
//! retires the post (`outbox sent`), after the post's ack and `land_group`'s
//! writes (the self-ack append; for an ask also its recipient and its deadline,
//! one append to the asks journal since C04, and before it the asked and deadline
//! tables, each a state-dir file rewritten whole). Every release is split at the
//! broker's heads: ***post call -> released***, post call -> acked, ***acked ->
//! released*** (the bridge's side: reading the ack, its writes, the `outbox
//! sent` round trip, aterm's wake), post call -> durable. A release is LATE
//! when it comes more than the bridge's idle ack grace (`IDLE_ACK_GRACE`, 5 ms)
//! plus one barrier (the ordered yardstick's p50) after its ack: the loop did
//! not read the ack while it waited for it. Every block waits for the bridge's
//! tally past its last post's inbox commit and past every post retired, so the
//! tally's rows are each block's own; they are given per post. The legs, each
//! `--asks` posts (100):
//!
//! - *Idle*: A asks B, one at a time, B lists each, against the control, the
//!   same post as `kind=note --wait` (the same wait without the ask's writes), in
//!   blocks note, ask, ask, note; with each kind's tally rows and their
//!   difference, and each kind's records and flushes per post.
//! - *Busy*: four sessions post asks to B at once, each waiting for its release
//!   before its next, while a reader lists B's inbox; and a ping-pong in which A
//!   asks B and B, the moment it lists the ask, sends a `kind=task` back (which
//!   waits too; it reaches A as a note, since the world's bridge accepts tasks
//!   from no node, `WakeClass::classify_for_sender`), with nothing waiting for the commit
//!   between them. Idle, the bridge's loop waits for a waited post's ack before
//!   it parks; busy (anything queued for it), it does not, and the ack is read by
//!   the first BLOCKING settle that comes: the next outbox drain (which settles
//!   every post owed before its peek), a broker read (`conn_now` settles what is
//!   owed first), a send made outside a round, or a round whose park, at most
//!   `IDLE_ACK_GRACE`, took nothing. The non-blocking pass (`try_settle_acks`)
//!   runs only on an idle loop.
//! - *Receipt*: A asks with `--wait-ack`; B, on a second connection, wakes on it,
//!   lists it and answers `inbox seen <id> handled`; A's post answers `OK <id>
//!   off=<n> ack=handled`. The landing (the ask acked) and the receipt (the ack's
//!   record acked, read off A's listing of it) are timed apart.
//! - *Person lane*: A asks the person `h-bench`, whose lane aterm's table
//!   escalates by its subject (`/f/*/in/p/>`; `PrincipalPrefix("h-")` names a
//!   sender), so the broker flushes it in full before the ack; `--asks` of each
//!   variant, one at a time, at least 60 ms apart call to call, in blocks
//!   unpaged, paged, paged, unpaged.
//!   The unpaged asks are the escalation alone. In the paged ones `aterm link
//!   notify --on ask:h-bench --exec true` follows the lane on the same machine
//!   (its cap the person's, its rate unbounded), and ***post call -> paged***
//!   ends when its journal's `<off> fired` line appears. Until C02 a page was four
//!   files the pager replaced and flushed in full (`F_FULLFSYNC`) on the volume
//!   the bridge's state dir is on: its rate stamps, the `firing` line, the
//!   `fired` line once the command had run, then its cursor. Since C02 it orders
//!   the stamps and the `firing` line (since the review the line first, the
//!   same flushes) and flushes the journal's directory in full before the
//!   command, and orders the `fired` line and the cursor (one full flush a page
//!   on macOS); its flush tally counts them. The bench waits for the
//!   cursor before the next ask, so the paged variant's difference from the
//!   unpaged is what the pager's writes cost the bridge, not an overlap the
//!   bench made.
//! - *Escalated mix*: the messaging phase's latency loop (A notes B, B lists, the
//!   next waits for the commit) for `--latency-samples` messages, without and
//!   with a person-lane note by a third session just before every
//!   `--mix-every`th (10), in blocks plain, mixed, mixed, plain: the ordinary
//!   messages' post -> visible with and without, those right after a person-lane
//!   post apart, and the broker's full flushes per ordinary message.
//! - *Need pager* (since C02): a writer on the bench's broker connection
//!   publishes a 128-byte row on `/f/<F>/pub/<node>/needbench/ev`, an `ev` face
//!   no session owns (so no need folds from it and nothing pages), every 5 ms,
//!   five times `--latency-samples` rows without and as many with a pager, in
//!   blocks no pager, need pager, need pager, no pager, and times each row's
//!   publish -> ack. The pager, `aterm link notify --on need --exec true` from
//!   the head, follows `/f/<F>/pub/*/*/*`, every row the fleet publishes there,
//!   on `ATERM_LINK_NOTIFY_FLUSH_TALLY` (a dev seam: it rewrites
//!   `notify/flush-tally` with the full and ordered flushes its stores asked
//!   for, and the bytes they wrote); a paged block counts them from a row
//!   published after it claimed the head until its cursor stands past the
//!   block's last row, against the rows
//!   under `pub` in that span.
//!
//! **Cross-node** (in the `ask` scenario). Two headless aterms, each with its own
//! bridge and tally, on one broker in the bench process, opened as every world's
//! is. Session A posts `kind=note` to a second session on its own node (the
//! control), to a bare `@s-<sid>` the other node hosts, and to `@s-<sid>@n-<node>`:
//! `--latency-samples` of each one at a time (the next waits for the recipient's
//! commit and for A's bridge to retire the post), in blocks local, bare, at-node
//! and back; then bursts, one sender, at most 56 in flight, `--burst` messages of
//! each form over two bursts in the same order. A's tally per post is the
//! SENDER's work: the reads a post to another node costs (a bare address
//! re-reads aterm's sessions and confirms the pinned node's presence, `settle_acks`
//! then `fetch_kept`; a named node walks its advertisers) are its `request` and
//! `lane_sessions` rows. On the local form it holds the delivery too.
//!
//! **Records per delivered message.** Every record the broker wrote over the
//! measured window, read from the log file once the broker has stopped — hidden
//! records included. That matters: astream writes a consumer-group commit as a
//! hidden `/a/commit` record (an offset and a Strict fsync like any publish), and
//! `Fetch` never returns hidden subjects, so a census over `Fetch` would miss the
//! very records the commit-batching work removes. Each record kind is its subject
//! with identities replaced (`/f/f1/in/<node>/<sid>/<node>/note` is a post on an
//! inbox lane). Since `2026-11-05-fabric-6065` each kind also has its bytes, each
//! record's `subject + body` as astream's byte bucket counts a publish, and each
//! table names the window's largest record. The latency window runs from the first measured post to the
//! commit covering the last; the burst window likewise, and the ping-pong's
//! (counted per message, two a round trip). The latency phase's
//! idle-bridge wait assumes the record right after each delivery is its commit,
//! and the bench checks that against the log for every round. For attach, the
//! window runs from the kill to the new bridge's last row and is shown by kind; it
//! also holds **the killed bridge's will** — the `state=gone` node row the broker
//! publishes when the kill drops the old connection — which is the kill's record,
//! not the attach's, so it has its own column and is left out of the total (today:
//! `/a/will` + node row + one row per session, so 3 / 10 / 34 for 1 / 8 / 32
//! sessions).
//!
//! ## What it does not see
//!
//! - **The band the broker ships in, by default.** Every world's broker is astream's
//!   `Broker` served inside the bench process, at the bench's own priority (31 at the
//!   default class on this Mac), not the shipped launchd job: the LaunchAgent runs
//!   `aterm link broker` as `ProcessType` `Background`, darwin-bg, every thread at 4.
//!   `--broker-process background|interactive` (since F02) starts each world's broker
//!   as this build's `aterm link broker` with the job's argv, as a `Background` job's
//!   task type or an `Interactive` one's, and reads its threads' priorities to prove the
//!   band. That arm sees only the outside stamps (`run/outside.rs`: no acked or durable
//!   moment, no flush or tick counts, no census) and runs only the messaging, ping-pong
//!   and attach worlds (the set: 'The broker's launchd band: Background against
//!   Interactive (2026-11-05, F02)' in the readings).
//! - **Local durable writes inside the bridge**, outside the ping-pong world. The
//!   bridge persists per-post state (its producer sequence, sent/in-flight/acked
//!   bookkeeping) in files under its state directory; those writes are part of
//!   *post -> durable* but are not broker records, so no census counts them. The
//!   ping-pong world's tally times them (since `-133`), and the `ask`
//!   scenario's worlds' tallies; the latency, burst and attach worlds run
//!   seam-free by default and do not, nor does the handoff world, whose instances run
//!   with the `ATERM_DEBUG_QUIT_BYE=handoff` seam alone. A run with `--tally`
//!   (since D02) boots them with both tallies on and reports each round's rows;
//!   such a run is not comparable with one without it.
//! - **aterm's own side of a verb, outside the ping-pong world**, unless the run names
//!   `--tally` (since D02). The handler tally runs in the ping-pong world in every run
//!   since C07, and in the messaging, attach, handoff and `ask` worlds only with
//!   `--tally`. It times handler bodies: the reader thread's wake and the dispatch before
//!   a body are in the *rest* of the requests, split ('What each number means', above), with the
//!   socket, not apart from it. The presence reads (`@<sid> status`, `@<sid> meta`,
//!   `@<sid> text`, `sessions status`) have bridge-side rows since D02 and no handler
//!   row.
//! - **A release binary byte for byte.** The `shipped` arm (since C07) is the
//!   release's code generation of the release's code and what `--features dev-seams`
//!   adds (the arm's bullet above lists it, and two of the seams it sets are deadlines
//!   the bridge runs), with symbols kept. Its numbers are the nearest this ledger has
//!   to a person's binary, and a difference smaller than that addition is not one this
//!   arm can tell from it. Every
//!   entry names its arm, and an `opt` entry is a test build with debug assertions,
//!   no fat LTO and 16 codegen units; compare like with like.
//! - **Cross-host links.** Unix sockets on one machine: the bus is one machine's,
//!   and no astream frame crosses between machines. The `ask` scenario's two
//!   nodes share that broker and that disk.
//!
//! ## The yardstick, and what it does not correct
//!
//! Each world times 50 bare Strict publishes (one client in the bench process, no
//! aterm) before and after its phases, and every headline is also given in those
//! units. Beside them it times, on an ordered broker of its own (aterm's options,
//! the default horizon), 50 bare publishes on an ordinary subject (one barrier
//! each) and 50 on a person-lane subject aterm's table escalates (one full flush
//! each, before the ack): what a barrier and an escalation cost on this disk at
//! that moment, never a unit. That takes **the disk's** speed out of a comparison — an `F_FULLFSYNC`
//! on a disk another job is writing to costs more for everyone — and nothing else.
//! It is one thread and one fsync; a message here hops through three processes
//! and several threads, and on a loaded host each hop waits for a core the
//! yardstick never asked for. The 2026-10-16 runs of one build read 2.40, 1.65 and
//! 1.55 yardstick units for post -> visible at load averages of 15-28 and 12-13.
//! Each entry records the load average before the build and after the run, and
//! the CPUs to read it against (logical, and performance cores on Apple silicon:
//! a load of 12 is idle for 18 cores and a queue for 6). That is the only CPU
//! measure.
//!
//! astream gates the primitive itself with bench claims of its own, the
//! `broker.bench.*` rows of `vendor/astream/evidence/manifest.toml`. For the tier
//! aterm runs they are four: `broker.bench.ordered-floor` and
//! `broker.bench.ordered-pipeline-floor`, floors low enough to hold beside a busy
//! volume, each paired with a ceiling on the full flushes the run asked astream-sys
//! for, a count a busy host cannot raise (`broker.bench.ordered-full-flush-ceiling`,
//! `broker.bench.ordered-pipeline-full-flush-ceiling`). They catch a regression in
//! astream and are no unit here: this ledger keeps its same-moment, same-disk
//! yardstick and no fixed reference number.
//!
//! ## How to compare two runs
//!
//! Same machine, one `--profile` for both sides (a dev baseline against an opt
//! candidate measures the compiler, not the change), and at least two runs of the
//! baseline and of the candidate, interleaved when you can (A B B A), so both sides
//! meet the same load. A ledger entry is not a fixed reference to measure a new
//! build against on another day: re-run the baseline beside the candidate. Compare medians, and compare record counts
//! by the absolute `records` column, not the per-message one (`2` and `3`
//! commits over 500 messages are 0.004 and 0.006 per message). Which counts are
//! deterministic for a given build:
//!
//! - the latency window's posts and commits: one post and one commit per message
//!   on an idle bridge (the "of which commit records" row reads exactly the
//!   message count). At `--log off` that is the whole window, 2 records per
//!   message (`-203`: 400 over 200). At the shipped `--log auto` the bus mirror's
//!   `ev` rows land in the window too, and how many is scheduling (`-204`: 416
//!   records, 2.080 per message; `-205` and `-206`: 402, 2.010): some land between
//!   a delivery and its commit (`between_delivery_and_commit` in the JSON, the
//!   "Between a delivery and its inbox commit" line: 15, 2 and 2), some after the
//!   commit and before the next post (`-204`: 1). Compare the post and commit rows,
//!   and read the `/f/<F>/pub/<node>/<sid>/ev` row apart. Nothing checks that a
//!   round trip wrote exactly that pair: since F1 the run holds each commit below
//!   the visible head read before the next post (`census::lands_before`, in
//!   `run::messaging`) and counts what lands between a delivery and its commit
//!   (`census::before_its_commit`) rather than refusing it (F1's section, "The
//!   latency phase waits for the commit itself", in the readings);
//! - NOT the ping-pong window's commits, though nearly: its posts are one per
//!   message and, in the bridge's wait tally, its publishes' acks 2 per round
//!   trip, but a delivery's commit can fold into the next delivery's. When the bridge has
//!   delivered an answer by the time it commits the delivery that answer
//!   answers, one commit covers both deliveries, and the round trip has 1 commit
//!   where it usually has 2. (An answer recorded before that commit is not
//!   always a fold: one published ahead but delivered after the commit gets a
//!   commit of its own. The report counts the two apart.) So on an otherwise
//!   idle node the window holds up to 2 records per message, and the tally up to
//!   2 group commits per round trip, less one for each fold — its only requests
//!   before aterm-0, and since then acks beside the publishes' (up to 4 acks a
//!   round trip, no request):
//!   `2026-10-17-fabric-36` folded once, 399 commits over 200 round trips (1.995
//!   per round trip, 0.998 per message; its table printed that as "2.00", from a
//!   renderer that has printed three decimals since), and `-35` never. Read a
//!   commit count a few short of 2 per round trip (399, not 400) as scheduling,
//!   like the burst's below; a roster round that republishes a row in the window
//!   adds to the tally's counts;
//! - the attach window's: one row per session plus the bridge's own (see the
//!   baseline in the readings);
//! - NOT the burst window's commits. The bridge commits its inbox cursor once no
//!   inbox record is queued behind the one it delivered, and every 256 records
//!   (`INBOX_COMMIT_EVERY`, `Bridge::round`), so a burst's commit count is
//!   `ceil(n / 256)` or more, by however often the queue drained mid-burst: four
//!   500-message runs of one build wrote 2, 3, 2 and 2. Its posts are
//!   deterministic (one per message); read its commits as scheduling, and a
//!   difference of one or two as noise.
//!
//! The times are not deterministic. A ratio between two numbers of one run (say, burst messages/s against
//! `1 / (post -> durable)`) is more portable across machines than either absolute,
//! but no number here is judged.
//!

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::arena::{self, Halt};
use super::bench_build::{
    self, Built, Provenance, Targo, another_build, cpus, ledger_paths, load_average,
};
use super::{LaneRun, workspace_root};

/// THE BENCH'S OWN GRAMMAR for a run, mounted from the bench (std-only) the way
/// `crate::toolchain` is shared with atpkg: the lane refuses exactly what the
/// bench would — before the build, as a usage error, never as a red lane after
/// one — and no bound (`BURST_MAX`, `ATTACH_MAX`, the rising list) is kept in
/// two places to drift. The `fabric-counts` lane composes its worlds in it too.
#[path = "../../../aterm/tests/fabric_bench/spec.rs"]
#[expect(
    dead_code,
    reason = "the lanes validate with `parse`; the knobs a run reads back and `from_env` are the \
              bench's"
)]
pub(super) mod bench_spec;

use bench_build::bench_profile::Profile;

/// One benchmark arm: broker durability tier and optional ordered horizon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Arm {
    pub tier: bench_spec::Tier,
    /// Ordered-tier horizon in milliseconds (`Some` iff `tier` is ordered).
    pub horizon_ms: Option<u64>,
    /// Whether the arm's logs are sealed at rest (`--at-rest`), as a fabric root's are: on unless
    /// a run names it off.
    pub at_rest: bool,
    /// Where the arm's brokers run (`--broker-process`): in the bench's process unless a run
    /// names the shipped job's band (`background`) or an `Interactive` job's (`interactive`).
    pub broker_process: bench_spec::BrokerProcess,
}

impl Arm {
    fn shipped() -> Arm {
        let tier = bench_spec::Tier::default();
        Arm {
            tier,
            horizon_ms: (tier == bench_spec::Tier::Ordered)
                .then_some(bench_spec::ORDERED_HORIZON_MS),
            at_rest: true,
            broker_process: bench_spec::BrokerProcess::Inproc,
        }
    }

    fn words(self) -> Vec<String> {
        let mut words = vec![format!("durability={}", self.tier.word())];
        if let Some(ms) = self.horizon_ms {
            words.push(format!("horizon_ms={ms}"));
        }
        if !self.at_rest {
            words.push("at_rest=off".to_string());
        }
        if self.broker_process.outside() {
            words.push(format!("broker_process={}", self.broker_process.word()));
        }
        words
    }

    fn flags(self) -> String {
        let mut flags = format!(" --durability {}", self.tier.word());
        if let Some(ms) = self.horizon_ms {
            flags.push_str(&format!(" --horizon-ms {ms}"));
        }
        if !self.at_rest {
            flags.push_str(" --at-rest off");
        }
        if self.broker_process.outside() {
            flags.push_str(&format!(" --broker-process {}", self.broker_process.word()));
        }
        flags
    }
}

/// Parsed `verify perf fabric` CLI options.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Args {
    /// Scenarios to measure (`--scenario`).
    pub scenario: bench_spec::Scenario,
    pub samples: Option<u32>,
    pub burst: Option<u32>,
    /// Ping-pong measured round trips (`--pingpong-rounds`).
    pub pingpong: Option<u32>,
    pub attach: Option<Vec<u32>>,
    pub rounds: Option<u32>,
    /// Arms to measure in order (`--durability` and `--horizon-ms`).
    pub arms: Vec<Arm>,
    /// Number of alternating passes over `arms` (`--passes`).
    pub passes: u32,
    pub json_out: Option<PathBuf>,
    /// Cargo profile to measure (`--profile`).
    pub profile: Profile,
    /// Broker log retention override (`--retention on|off`).
    pub retention: Option<bool>,
    /// Broker rate limit override (`--rate-limit on|off`).
    pub rate_limit: Option<bool>,
    /// Bridge bus mirror mode (`--log auto|off`).
    pub log: Option<bench_spec::Log>,
    /// Retaining broker tick interval override in milliseconds (`--retention-interval-ms`).
    pub retention_interval_ms: Option<u64>,
    /// Idle bystander sessions spawned alongside measured sessions (`--bystanders`).
    pub bystanders: Option<u32>,
    /// Posts per leg of the ask scenario (`--asks`).
    pub asks: Option<u32>,
    /// One person-lane post every this many messages in the ask scenario's mix (`--mix-every`).
    pub mix_every: Option<u32>,
    /// The attach world's backlog (`--attach-backlog`): in the bench's process, records behind
    /// every session's refill watermark; with an outside broker, records for each population
    /// seeded into its log before it starts (`bench_spec::Spec::backlog`).
    pub backlog: Option<u32>,
    /// Rows the ask world's asked table is pre-filled to (`--asked-prefill`).
    pub asked_prefill: Option<u32>,
    /// One-way notes the messaging world posts paced after its burst (`--paced-notes`).
    pub paced_notes: Option<u32>,
    /// Both tallies in the messaging, attach, handoff and ask worlds (`--tally`).
    pub tally: bool,
}

fn positive(flag: &str, raw: &str) -> Result<u32, String> {
    match raw
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| raw.parse::<u32>().ok())
        .flatten()
    {
        Some(v) if v > 0 => Ok(v),
        _ => Err(format!("`{flag}` needs a positive integer, got `{raw}`")),
    }
}

impl Args {
    /// Parse `verify perf fabric` CLI options.
    ///
    /// # Errors
    /// Returns an error string if an option value or combination is rejected by the benchmark grammar.
    pub(crate) fn from_options(options: &BTreeMap<&'static str, String>) -> Result<Args, String> {
        let mut a = Args {
            passes: 1,
            ..Args::default()
        };
        let mut tiers: Vec<bench_spec::Tier> = Vec::new();
        let mut horizons: Vec<u64> = Vec::new();
        let mut at_rest: Vec<bool> = Vec::new();
        let mut processes: Vec<bench_spec::BrokerProcess> = Vec::new();
        for (flag, raw) in options {
            match *flag {
                "--at-rest" => {
                    at_rest = raw
                        .split(',')
                        .map(|word| match word {
                            "on" => Ok(true),
                            "off" => Ok(false),
                            _ => Err(format!(
                                "`--at-rest` takes on or off, comma-separated, not `{raw}`"
                            )),
                        })
                        .collect::<Result<_, _>>()?;
                }
                "--broker-process" => {
                    processes = raw
                        .split(',')
                        .map(|word| {
                            bench_spec::BrokerProcess::parse(word).map_err(|_| {
                                format!(
                                    "`--broker-process` takes inproc, background or \
                                     interactive, comma-separated, not `{raw}`"
                                )
                            })
                        })
                        .collect::<Result<_, _>>()?;
                }
                "--scenario" => {
                    a.scenario = bench_spec::Scenario::parse(raw).map_err(|_| {
                        format!(
                            "`--scenario` takes all, messaging, pingpong, attach, handoff or ask, \
                             not `{raw}`"
                        )
                    })?;
                }
                "--asks" => a.asks = Some(positive(flag, raw)?),
                "--mix-every" => a.mix_every = Some(positive(flag, raw)?),
                "--latency-samples" => a.samples = Some(positive(flag, raw)?),
                "--burst" => a.burst = Some(positive(flag, raw)?),
                "--pingpong-rounds" => a.pingpong = Some(positive(flag, raw)?),
                "--attach-rounds" => a.rounds = Some(positive(flag, raw)?),
                "--passes" => a.passes = positive(flag, raw)?,
                "--attach-sessions" => {
                    a.attach = Some(raw.split(',').map(|n| positive(flag, n)).collect::<Result<
                        Vec<u32>,
                        String,
                    >>(
                    )?);
                }
                "--durability" => {
                    tiers = raw
                        .split(',')
                        .map(|word| {
                            bench_spec::Tier::parse(word).map_err(|_| {
                                format!(
                                    "`--durability` takes strict, ordered or relaxed, \
                                     comma-separated, not `{raw}`"
                                )
                            })
                        })
                        .collect::<Result<_, _>>()?;
                }
                "--horizon-ms" => {
                    horizons = raw
                        .split(',')
                        .map(|ms| {
                            ms.bytes()
                                .all(|b| b.is_ascii_digit())
                                .then(|| ms.parse::<u64>().ok())
                                .flatten()
                                .ok_or_else(|| {
                                    format!(
                                        "`--horizon-ms` takes milliseconds, comma-separated, not \
                                         `{raw}`"
                                    )
                                })
                        })
                        .collect::<Result<_, _>>()?;
                }
                "--json-out" => a.json_out = Some(PathBuf::from(raw)),
                "--profile" => a.profile = Profile::parse(raw)?,
                "--retention" => {
                    a.retention = Some(match raw.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => {
                            return Err(format!("`--retention` takes on or off, not `{raw}`"));
                        }
                    });
                }
                "--rate-limit" => {
                    a.rate_limit = Some(match raw.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => {
                            return Err(format!("`--rate-limit` takes on or off, not `{raw}`"));
                        }
                    });
                }
                "--log" => {
                    a.log = Some(
                        bench_spec::Log::parse(raw)
                            .map_err(|_| format!("`--log` takes auto or off, not `{raw}`"))?,
                    );
                }
                "--retention-interval-ms" => {
                    a.retention_interval_ms = Some(u64::from(positive(flag, raw)?));
                }
                "--bystanders" => {
                    a.bystanders = Some(
                        raw.bytes()
                            .all(|b| b.is_ascii_digit())
                            .then(|| raw.parse::<u32>().ok())
                            .flatten()
                            .ok_or_else(|| {
                                format!("`{flag}` needs a non-negative integer, got `{raw}`")
                            })?,
                    );
                }
                "--attach-backlog" => {
                    a.backlog = Some(
                        raw.bytes()
                            .all(|b| b.is_ascii_digit())
                            .then(|| raw.parse::<u32>().ok())
                            .flatten()
                            .ok_or_else(|| {
                                format!("`{flag}` needs a non-negative integer, got `{raw}`")
                            })?,
                    );
                }
                "--asked-prefill" => {
                    a.asked_prefill = Some(
                        raw.bytes()
                            .all(|b| b.is_ascii_digit())
                            .then(|| raw.parse::<u32>().ok())
                            .flatten()
                            .ok_or_else(|| {
                                format!("`{flag}` needs a non-negative integer, got `{raw}`")
                            })?,
                    );
                }
                "--paced-notes" => {
                    a.paced_notes = Some(
                        raw.bytes()
                            .all(|b| b.is_ascii_digit())
                            .then(|| raw.parse::<u32>().ok())
                            .flatten()
                            .ok_or_else(|| {
                                format!("`{flag}` needs a non-negative integer, got `{raw}`")
                            })?,
                    );
                }
                "--tally" => a.tally = true,
                "--no-windows" | "--admit" => {}
                other => {
                    return Err(format!(
                        "internal: `{other}` has no parser in the fabric lane"
                    ));
                }
            }
        }
        if !horizons.is_empty() && !tiers.contains(&bench_spec::Tier::Ordered) {
            return Err(
                "`--horizon-ms` is the ordered arm's: name it in `--durability`".to_string(),
            );
        }
        for tier in tiers {
            match tier {
                bench_spec::Tier::Ordered if !horizons.is_empty() => {
                    a.arms.extend(horizons.iter().map(|&ms| Arm {
                        tier,
                        horizon_ms: Some(ms),
                        at_rest: true,
                        broker_process: bench_spec::BrokerProcess::Inproc,
                    }));
                }
                bench_spec::Tier::Ordered => a.arms.push(Arm {
                    tier,
                    horizon_ms: Some(bench_spec::ORDERED_HORIZON_MS),
                    at_rest: true,
                    broker_process: bench_spec::BrokerProcess::Inproc,
                }),
                _ => a.arms.push(Arm {
                    tier,
                    horizon_ms: None,
                    at_rest: true,
                    broker_process: bench_spec::BrokerProcess::Inproc,
                }),
            }
        }
        if !at_rest.is_empty() {
            let arms = if a.arms.is_empty() {
                vec![Arm::shipped()]
            } else {
                std::mem::take(&mut a.arms)
            };
            a.arms = arms
                .into_iter()
                .flat_map(|arm| at_rest.iter().map(move |&at_rest| Arm { at_rest, ..arm }))
                .collect();
        }
        if !processes.is_empty() {
            let arms = if a.arms.is_empty() {
                vec![Arm::shipped()]
            } else {
                std::mem::take(&mut a.arms)
            };
            a.arms = arms
                .into_iter()
                .flat_map(|arm| {
                    processes.iter().map(move |&broker_process| Arm {
                        broker_process,
                        ..arm
                    })
                })
                .collect();
        }
        for arm in a.plan() {
            bench_spec::parse(&a.bench_words(arm), Path::new("/scratch"))
                .map_err(|why| format!("the fabric bench refuses this run: {why}"))?;
        }
        if a.json_out.is_some() && a.plan().len() > 1 {
            return Err(
                "`--json-out` names one file, and this plan is more than one run: each run's \
                 JSON is its ledger entry's"
                    .to_string(),
            );
        }
        Ok(a)
    }

    /// Build the alternating pass schedule of [`Arm`]s (forward on even passes, reversed on odd passes).
    pub(crate) fn plan(&self) -> Vec<Arm> {
        let arms: Vec<Arm> = if self.arms.is_empty() {
            vec![Arm::shipped()]
        } else {
            self.arms.clone()
        };
        (0..self.passes.max(1))
            .flat_map(|pass| {
                let mut once = arms.clone();
                if pass % 2 == 1 {
                    once.reverse();
                }
                once
            })
            .collect()
    }

    /// Format the `FABRIC_BENCH` measurement knobs for `arm`.
    pub(crate) fn bench_words(&self, arm: Arm) -> String {
        let mut words = Vec::new();
        if self.scenario != bench_spec::Scenario::All {
            words.push(format!("scenario={}", self.scenario.word()));
        }
        if let Some(n) = self.samples {
            words.push(format!("samples={n}"));
        }
        if let Some(n) = self.burst {
            words.push(format!("burst={n}"));
        }
        if let Some(n) = self.pingpong {
            words.push(format!("pingpong={n}"));
        }
        if let Some(sizes) = &self.attach {
            let sizes: Vec<String> = sizes.iter().map(u32::to_string).collect();
            words.push(format!("attach={}", sizes.join(",")));
        }
        if let Some(n) = self.rounds {
            words.push(format!("rounds={n}"));
        }
        if let Some(on) = self.retention {
            words.push(format!("retention={}", if on { "on" } else { "off" }));
        }
        if let Some(on) = self.rate_limit {
            words.push(format!("rate_limit={}", if on { "on" } else { "off" }));
        }
        if let Some(log) = self.log {
            words.push(format!("log={}", log.word()));
        }
        if let Some(ms) = self.retention_interval_ms {
            words.push(format!("retention_interval_ms={ms}"));
        }
        if let Some(n) = self.bystanders {
            words.push(format!("bystanders={n}"));
        }
        if let Some(n) = self.asks {
            words.push(format!("asks={n}"));
        }
        if let Some(n) = self.mix_every {
            words.push(format!("mix_every={n}"));
        }
        if let Some(n) = self.backlog {
            words.push(format!("backlog={n}"));
        }
        if let Some(n) = self.asked_prefill {
            words.push(format!("asked_prefill={n}"));
        }
        if let Some(n) = self.paced_notes {
            words.push(format!("paced_notes={n}"));
        }
        if self.tally {
            words.push("tally=on".to_string());
        }
        words.extend(arm.words());
        words.join(" ")
    }

    /// Format the reproducing `verify perf fabric` command line for `arm`.
    pub(crate) fn command_line(&self, arm: Arm) -> String {
        let mut line = String::from("targo --unverified verify perf fabric");
        if self.scenario != bench_spec::Scenario::All {
            line.push_str(&format!(" --scenario {}", self.scenario.word()));
        }
        if self.profile != Profile::Dev {
            line.push_str(&format!(" --profile {}", self.profile.name()));
        }
        if let Some(n) = self.samples {
            line.push_str(&format!(" --latency-samples {n}"));
        }
        if let Some(n) = self.burst {
            line.push_str(&format!(" --burst {n}"));
        }
        if let Some(n) = self.pingpong {
            line.push_str(&format!(" --pingpong-rounds {n}"));
        }
        if let Some(sizes) = &self.attach {
            let sizes: Vec<String> = sizes.iter().map(u32::to_string).collect();
            line.push_str(&format!(" --attach-sessions {}", sizes.join(",")));
        }
        if let Some(n) = self.rounds {
            line.push_str(&format!(" --attach-rounds {n}"));
        }
        if let Some(on) = self.retention {
            line.push_str(&format!(" --retention {}", if on { "on" } else { "off" }));
        }
        if let Some(on) = self.rate_limit {
            line.push_str(&format!(" --rate-limit {}", if on { "on" } else { "off" }));
        }
        if let Some(log) = self.log {
            line.push_str(&format!(" --log {}", log.word()));
        }
        if let Some(ms) = self.retention_interval_ms {
            line.push_str(&format!(" --retention-interval-ms {ms}"));
        }
        if let Some(n) = self.bystanders {
            line.push_str(&format!(" --bystanders {n}"));
        }
        if let Some(n) = self.asks {
            line.push_str(&format!(" --asks {n}"));
        }
        if let Some(n) = self.mix_every {
            line.push_str(&format!(" --mix-every {n}"));
        }
        if let Some(n) = self.backlog {
            line.push_str(&format!(" --attach-backlog {n}"));
        }
        if let Some(n) = self.asked_prefill {
            line.push_str(&format!(" --asked-prefill {n}"));
        }
        if let Some(n) = self.paced_notes {
            line.push_str(&format!(" --paced-notes {n}"));
        }
        if self.tally {
            line.push_str(" --tally");
        }
        line.push_str(&arm.flags());
        line
    }

    /// Cargo arguments naming the benchmark target and profile for this run.
    pub(crate) fn build_args(&self) -> Vec<&'static str> {
        let mut args = vec![
            "--profile",
            self.profile.cargo_profile(),
            "-p",
            "aterm",
            "--bench",
            "fabric_bench",
        ];
        args.extend_from_slice(self.profile.cargo_features());
        args
    }
}

const PROGRESS: &str = "fabric bench:";

/// Run the `fabric` performance lane.
pub(crate) fn run(args: &Args) -> LaneRun {
    arena::outcome(measure(args))
}

fn measure(args: &Args) -> Result<String, Halt> {
    let ledger_dir = super::ledger_dir("fabric");
    std::fs::create_dir_all(&ledger_dir)
        .map_err(|e| format!("could not make {}: {e}", ledger_dir.display()))?;
    let driver = crate::perf::driver::cargo_driver()?;
    let (targo, built, load_before) =
        bench_build::prepare(driver, args.profile, &args.build_args())?;
    let plan = args.plan();
    let mut written = Vec::with_capacity(plan.len());
    let mut load = format!("{load_before} before the build");
    for (i, arm) in plan.iter().copied().enumerate() {
        if plan.len() > 1 {
            eprintln!(
                "  run {} of {}: {}",
                i + 1,
                plan.len(),
                arm.words().join(" ")
            );
        }
        let entry = run_arm(&targo, args, arm, &built, &ledger_dir, &load)?;
        written.push(entry.display().to_string());
        load = format!("{} before the run", load_average());
    }
    Ok(format!("ledger entries {}", written.join(", ")))
}

fn run_arm(
    targo: &Targo<'_>,
    args: &Args,
    arm: Arm,
    built: &Built,
    ledger_dir: &Path,
    load_before: &str,
) -> Result<PathBuf, Halt> {
    let profile = args.profile;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-fabric-")
        .suffix(".noindex")
        .tempdir()
        .map_err(|e| format!("could not make a scratch directory: {e}"))?;
    let words = args.bench_words(arm);
    bench_build::run_bench(
        targo,
        &args.build_args(),
        (bench_spec::ENV, &words),
        (bench_spec::OUT_ENV, scratch.path()),
        PROGRESS,
        built,
        bench_build::Entry::Timed,
    )?;
    let read = |name: &str| {
        let path = scratch.path().join(name);
        crate::read_regular_text(&path)
            .map_err(|e| format!("the bench wrote no {}: {e}", path.display()))
    };
    let tables = read("fabric.md")?;
    let mut report: aterm_json::Value = aterm_json::from_str(&read("fabric.json")?)
        .map_err(|e| format!("the bench's fabric.json does not parse: {e}"))?;
    if let Some(why) = another_build(&report, &built.path, &built.identity) {
        return Err(Halt::Fail(format!(
            "{why}: these numbers are not the `{}` arm's build, and no ledger entry was written",
            profile.name()
        )));
    }
    if report.get("scenario").and_then(aterm_json::Value::as_str) != Some(args.scenario.word()) {
        return Err(Halt::Fail(format!(
            "the fabric bench did not report the requested scenario `{}`; no ledger entry was written",
            args.scenario.word()
        )));
    }
    if let Some(fields) = report.as_object_mut() {
        fields.insert("lane_build".to_string(), built.lane_build.clone().into());
    }
    let json = aterm_json::to_string(&report)
        .map_err(|e| format!("the bench's report does not serialize: {e}"))?;
    let load = format!("{load_before}, {} after the run", load_average());
    eprintln!("{tables}");

    let date = super::trend::utc_date();
    let header = Provenance {
        heading: &heading(args.scenario, arm.broker_process),
        protocol: "crates/aterm-verify/src/perf/fabric.rs",
        date: &date,
        machine: &arena::chip(),
        macos: &arena::macos_version(),
        aterm: &arena::git_provenance(&workspace_root()),
        astream: &arena::git_provenance(&workspace_root().join("vendor/astream")),
        spec: report
            .get("spec")
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("?"),
        profile: report
            .get("profile")
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("?"),
        elapsed_s: report
            .get("elapsed_s")
            .and_then(aterm_json::Value::as_f64)
            .unwrap_or(f64::NAN),
        command: &args.command_line(arm),
        build: &built.lane_build,
        thermal: &arena::thermal(),
        cpus: &cpus(),
        load: &load,
    }
    .render();
    let (md_path, json_path) = ledger_paths(ledger_dir, &date, "fabric");
    crate::write_fresh(&md_path, format!("{header}\n{tables}\n"))
        .map_err(|e| format!("could not write {}: {e}", md_path.display()))?;
    crate::write_fresh(&json_path, &json)
        .map_err(|e| format!("could not write {}: {e}", json_path.display()))?;
    eprintln!(
        "ledger entry written: {} (+ {})",
        md_path.display(),
        json_path.display()
    );
    if let Some(out) = &args.json_out {
        super::publication::publish_named(out, &json)
            .map_err(|e| format!("could not write {}: {e}", out.display()))?;
        eprintln!("json written: {}", out.display());
    }
    Ok(md_path)
}

/// An entry's title: the scenarios it measured, and the broker's process when it ran in one of
/// its own, whose `all` is the messaging world without its burst, the ping-pong and the attach
/// (`run/outside.rs`).
fn heading(scenario: bench_spec::Scenario, process: bench_spec::BrokerProcess) -> String {
    let scenarios = match (scenario, process.outside()) {
        (bench_spec::Scenario::All, false) => {
            "post -> deliver, burst, ping-pong, attach, handoff, ask, cross-node"
        }
        (bench_spec::Scenario::All, true) => "post -> deliver, ping-pong, attach",
        (selected, _) => selected.word(),
    };
    if process.outside() {
        format!(
            "Fabric: {scenarios}, on `aterm link broker` as {}",
            process.runs_as()
        )
    } else {
        format!("Fabric: {scenarios}")
    }
}

#[cfg(test)]
#[path = "fabric_tests.rs"]
mod tests;
