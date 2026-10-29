<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-sysprobe: spec recovery from its unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each spec below is proved through
> Trust/Clean about the compiled Rust function, and then the tests it subsumes are deleted. Nothing has
> been proved or deleted yet.
>
> **Re-measured on Trust seal `150efe4a` (2026-10-16): `scaled-ticks-saturating` and `used-mib-of` stay
> blocked; no test is deleted.** Both were written as `trust_contract_fn!` contracts citing a Clean
> theorem over `trust_import_aterm_sysprobe__{ticks_to_ns,ticks_ns,used_mib_of}` and run through the
> seal; the code was then restored byte for byte. Three blockers, each measured:
>
> 1. **No kernel import is minted (gaps 1 and 3).** Every island was rejected with
>    `AutoBoundHeadOverApplied { name: "trust_import_aterm_sysprobe__<fn>", .. }`: E6 refused the body,
>    so the name is unbound. `TRUST_E6_DEBUG=1` names why: `admissible=false`, with the first deficit a
>    non-scalar local (gap 1): the `Result<u64, TryFromIntError>` of `u64::try_from` in `ticks_to_ns`
>    and `linux::ticks_ns`, and the `Option<u32>` return of `used_mib_of`. Behind it, gap 3: calls to
>    `From`/`TryFrom`/`unwrap_or`/`ok`/`saturating_{sub,mul}` are "not itself structurally certified",
>    and a reachable `Assert` (checked `*`, `/` by a guarded nonzero divisor, `>>`) fails structural
>    NoPanic. A probe (`$HOME/trust-land/seal-aterm-probe/sysprobe{,2}`) found no admissible form of the
>    division at all: guarded `/`, `wrapping_div`, `checked_div` and `u128::wrapping_mul` are each
>    refused; only `as` casts, comparisons, branches and `u64::wrapping_mul` are admitted.
> 2. **The `ensures` predicate does not lower.** `result == (if .. { .. } else { .. })` with `u128::from`,
>    `min`, `as`, `try_from`, `saturating_*` is `SpecEnsuresUnparseable` (fail-closed Unknown).
> 3. **Every fn of this crate on macOS fails the `contract:global-alloc-dealloc-panic-free` caller
>    contract**, because the macOS dependency `aterm-objc` holds `global_asm!`, which could define a
>    libc allocator symbol. This is not gap 1 or 3, and it blocks Certified for every row here on a
>    macOS host until Trust verifies that assembly or `aterm-objc` drops it.
>
> The seal also confirms §5.1 on real code: `used_mib_of`'s two plain `+` are refuted
> (`arithmetic_overflow_add failed`).
>
> Later on 2026-10-16 the strain row was removed (CHANGELOG, `[Unreleased]`) and this crate was slimmed
> to the readers other crates share: `process_cwd`, `process_path`, `group_memory`,
> `physical_memory_mib`, `machine_memory`, `memory_pressure`, `MemoryLevel`, `swapped_kib` and
> `SwapMeter`. The recovery had read 30 tests for 31 specs. Eighteen specs left with their code (the
> Linux sampler and its process sweep, the macOS sampler and `Probe`, PSI, thermal zones, CPU ticks and
> the naming of busy processes), as did three tests that sampled no spec. Three specs were narrowed to
> what remains (`pressure-level-table`, `swapped-kib-counter`, and `meminfo-facts`, whose `MemInfo`
> methods had served only the sampler). This file states the 13 specs that remain over the 12 tests
> that remain. On 2026-10-17 `lone_group_leader` (and its Linux parser `stat_pgrp`) joined, for a
> file citation's shell in front, with a spec and a test each: 15 specs over 14 tests. `scaled-ticks-saturating` left with the
> sampler; the slimmed crate no longer depends on `aterm-objc`, so the re-measure's third blocker
> (its `global_asm!`) no longer applies to the rows that remain.
>
> On 2026-10-19 the system-awareness readers joined (`Probe::core_ticks`, `cpu_layout`, `memory_detail`,
> `gpu`, `volume`; the pure `CoreTicks::{busy,total}`, `machine::{layout_of, dt_assignment,
> conventional_assignment, memory_detail_of, percent_of, volume_of, c_path}`, `macos::{cores_of, le_u64,
> cluster_kind}`, `linux::{proc_stat_cores, cpulist, memory_detail}` and `read_into`), with 19 tests in
> `src/tests.rs` and the IOKit declaration probe in `tests/iokit_sdk.rs`. Their specs are not recovered
> here yet; the tables below still state the 15 specs over the 14 tests that predate them (the volume
> tests, which joined on 2026-10-17 with `volumes`, are not recovered either).
>
> Later on 2026-10-19 the job probe joined (`jobs`: `JobsReader`, the macOS arm through libproc and
> `sysctl(KERN_PROC_PID)`, the Linux arm under a root; the pure `jobs::darwin` layout decoders, the
> `jobs::linux` parsers, `tail_allowed`, `tail_has_lf`), and the sweep gained group, uid, state and the
> session and terminal flags, a zombie's row coming from `kinfo_proc`. Its 16 tests are in
> `src/jobs/tests.rs` (13 run on every host or every Unix; 3 need macOS and read the live host: this
> process's own reading, both lanes' self-checks, a stopped and a zombie child) and the layout probe in
> `tests/proc_info_sdk.rs` (a green run and its negative control). Their specs are not recovered here
> yet either.
>
> On 2026-10-20 the Stats Deck v2 readers joined: `net_links` (the macOS arm decoding
> `NET_RT_IFLIST2` through the pure `iflist::{links_of, dl_name, kind_of_functional, link_kind,
> read_growing}`, the Linux arm `linux::links_under` over the pure `linux::{net_dev, hex_flags,
> link_up, speed_bps, sys_kind}`), the pure `counts_toward_machine` and `machine::kind_by_name`, `process_limits` (with the pure
> `machine::{smaller, rlimit_ceiling}`) and `load_average`; the sweep gained each process's disk bytes
> and `gpu` the driver's allocation and recovery count. Their tests are 6 in `src/iflist.rs` (every
> host) and 9 in `src/tests.rs` (6 on every host, 3 that read the live host), and the layout probe in
> `tests/iflist_sdk.rs` (a green run and its negative control). Their specs are not recovered here yet
> either.
>
> Later on 2026-10-20 `net_links_into` and `pid_counts` joined. `net_links_into` reads the links into a
> buffer the caller keeps (the macOS arm's `iflist::read_growing` now reads into that buffer, the Linux
> arm is `linux::links_under_into`, which replaced `linux::links_under`); `net_links` is a wrapper that
> makes one per call. `pid_counts` counts the machine's processes and one user's: on macOS
> `proc_listallpids` and `proc_listpids(PROC_RUID_ONLY)` through the pure
> `machine::{pid_room, listed_count, owned_count}` (a uid that owns no process counts zero), on Linux
> `/proc/loadavg`'s total through the pure `linux::loadavg_tasks` and `linux::Root::tasks`, on Windows
> `K32GetPerformanceInfo`'s process count. Their tests are 1 more in `src/iflist.rs` (every host) and 3
> more in `src/tests.rs` (on every host). Their specs are not recovered here yet either.
>
> On 2026-10-20 `stdin_sharers` (the members of a group that read its leader's own input device: a
> program an agent handed its terminal to, which a bare Ctrl-R/Ctrl-S then reaches) and its Linux
> parser `linux::children` joined, with a spec and a test each: the tables below state 17 specs over
> 16 tests.
>
> On 2026-10-22 the program-check predicate joined the job probe (`docs/DESIGN-leftover-processes-2026-10-16.md`
> §4.2): the pure `jobs::judge_start` over `StartFacts`, the one-shot `jobs::read_start` (a two-slot
> `PROC_PIDLISTTHREADS`, then for a lone `0` only `PROC_PIDTBSDINFO`, `PROC_PIDARCHINFO`, `proc_pidpath`
> and the mount table; `read_start_in` shares one table across many pids) with `at_start` and `now_us`,
> `ProcFacts.start_facts` (read from the handles the probe's thread list already read, judged by
> `ProcFacts::program_check`), and `darwin::archinfo_cputype` with the arch, `SSTOP` and
> `PROC_FLAG_TRACED` rows of `jobs::darwin::SDK_ROWS`. A mount table cut at `MAX_MOUNTS` proves no
> volume local (`local_at`, which `tail_allowed` shares). aterm-uds' `at_program_check`,
> `thread_ran` and `held_programs` read through it. The specs `program-check-judge` and
> `program-check-read` are stated in §1 and their six tests mapped in §2; the counts and histograms
> were taken before.
>
> On 2026-10-23 `jobs::linux::parse_stat` reads `tty_nr` as the kernel prints it, a C `int`: a
> negative one (a pts minor of 2^19 or more sets the sign bit) is its 32 bits, and a field that is
> neither an `int` nor a `u32` fails the line. Both had read as no terminal, which a caller judging
> a terminal (aterm-uds' `procfs::lineage`, the sweep's `has_tty`) took for none
> (`jobs::tests::a_stat_line_gives_every_field_past_any_command_name`). Its spec is not recovered
> here yet.
>
> On 2026-10-24 `gpu_driver` joined (`GpuDriver`: the accelerator entry's `CFBundleIdentifier` and
> `IOSourceVersion`, read afresh per call; `None` off macOS), for Aos M5a's session record and its G3
> driver check. Its live test is `tests::the_gpu_driver_names_itself_on_this_host` (macOS on Apple
> silicon). Its spec is not recovered here yet.

Source: `crates/aterm-sysprobe`. The recovered spec table below maps 44 `#[test]` functions across
`src/tests.rs` (18), `src/jobs/tests.rs` (18) and `src/procargs_tests.rs` (8). Nothing was compiled
or run for this file; the claims come from reading the code.

Result: **28 specs**, covering **44 tests**. Every mapped test is a spec sample (§4).

> 2026-10-20: `procargs-parse` (the workspace's one `KERN_PROCARGS2` reader and parser,
> `procargs`, which `kproc`, atpkg, aterm-gui and aterm-uds' `process` read through; aterm-uds
> re-exports it) moved here from aterm-uds with its tests (`src/procargs_tests.rs`). The counts
> below were taken before.
>
> 2026-10-23: `vnode-terminal` (the one rule that calls a descriptor a terminal, which the job
> probe's `FdKind::Device` and aterm-uds' `holders::fd_is_terminal` both read) joins §1 with the
> vnode decoder's test. The counts below were taken before.
>
> 2026-10-23, later: `vnode-terminal` is restated over `jobs::terminal_path`, the one list of
> terminal device names both platforms' readers judge by (macOS's vnode path, Linux's `fd/<n>`
> link), which now names the console, a pty master and a serial call-out line on both; the
> `/proc` link test cites it too.
>
> 2026-10-23, later still: `fresh-dylib` (the job probe's judge of a dylib a build just wrote,
> `FdKind::File::fresh_dylib`, which the stuck engine's dylib heuristic reads) joins §1 with its
> two tests, and the vnode decoder reads `vst_mtime` (a `jobs::darwin::SDK_ROWS` row). `AtStart`
> lost its `word`: the engine's `StartWait::word` is the one a row prints.
>
> 2026-10-24: `socket-settled` and `socket-read-gate` (when the job probe, and the lab's `lsof -i`
> each second, may read another process's sockets: `PROC_PIDFDSOCKETINFO` drains a socket its
> holder closes meanwhile) join §1 with their six tests. The counts below were taken before.
>
> 2026-10-27: `group-usage-members` (`group_usage`, what each member of a process group used,
> which aterm-verify's ceiling clock reads to leave out a check's wait for a core) and
> `hid-idle-clock` (`hid_idle`, the time since the keyboard or mouse was last touched, which
> decides whether verify clamps a compile to utility QoS) join §1 with a test each
> (`docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT). The counts below were taken before.
>
> 2026-10-29: `group-usage-members` gains each member's footprint now and its lifetime peak
> (`ri_phys_footprint`, `ri_lifetime_max_phys_footprint`, from the same `RUSAGE_INFO_V4` read),
> which aterm-verify books into the stage's memory meter (VERIFY-MEM), and the macOS sweep reads
> each process's resident size (`ri_resident_size`, `ProcessReading::resident`; the Linux sweep
> reads its `RssAnon` there, `linux::status_memory`), which the capacity ledger credits a
> claim's tree with where it is below the footprint (VERIFY-MEM's honest room).
> `available-mib-of` is unchanged: the ledger reads its own room from `memory_detail`
> (aterm-capacity's `claimable`).

## 0. Notation used in the statements

- `T` = `crates/aterm-sysprobe/src/tests.rs`. Function names are relative to `crates/aterm-sysprobe/src`
  (`linux::f` is in `linux.rs`, `macos::f` in `macos.rs`, and a bare name is in `lib.rs`).
- `WL(t)` ("whole lines") = `linux::whole_lines(t)`, which is `t[..e].lines()`. Here e = `t.len()` if t ends
  with `'\n'`, otherwise (index of the last `'\n'`)+1, or 0 if t has no `'\n'`. A text cut at `READ_CAP`
  keeps the WL of the cut, so its torn last line is never parsed.
- `Key(t,k)` = `linux::keyed(t,k)`. It takes the first line in WL(t) whose first whitespace token, with any
  trailing `':'` stripped, equals k. The result is that line's second token parsed as `u64`, or None if no
  line matches or the token does not parse. The search stops at the first matching line, even when that
  line's value does not parse.
- `sat+`, `sat-`, `sat*` are the `u64` saturating ops.
- `Effects(...)` marks a statement about the OS (procfs, sysctl, libproc, Mach). It can be proved only with
  a platform model (gap 8). Its pure core is stated separately wherever one exists, and that core is the
  part worth proving first.
- `MemoryLevel` is this crate's own enum, ordered Normal < Warn < Critical. `Instant` is `std::time::Instant`.
- Gap numbers are the rows of `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7. Gap 0 (the installed
  seal publishes no Lean-discharged row) is a prerequisite for every spec, so every row lists it. No row
  here is blocked by gap 0 alone. The closest row needs gaps 0 and 3.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary in §2),
`difficulty` (the Rust features the proof must handle), `blocking_gaps` (audit §7).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
whole-lines-torn-tail-ignored	invariant	linux::whole_lines, linux::keyed, linux::meminfo, linux::swap_pages	∀ P ∈ {meminfo, swap_pages}, ∀ t that is empty or ends with '\n', ∀ s with no '\n': P(t ++ s) = P(t). Equivalently, P(x) depends only on WL(x). Corollary: when x is t cut at any byte c that does not fall right after a '\n', the line containing c is never parsed, so no number's prefix (e.g. `pswpout 34` of `pswpout 3400`) is ever read as a value.	(secondary only: T::every_proc_read_is_bounded_to_4_kib, torn vmstat part)	&str slicing at rfind, Iterator::lines/find_map with closures, Option	0,1,3,5,7,13
meminfo-facts	contract	linux::meminfo, linux::keyed	∀ text: meminfo(text) = Some(MemInfo{total_kib: Key(text,"MemTotal"), available_kib: Key(text,"MemAvailable")}) when both are Some, otherwise None.	T::meminfo_gives_the_total_and_the_available	&str parsing via keyed (iterator, closures), Option `?`, Copy struct construction	0,1,3,5,7,13
swap-pages-sum	contract	linux::swap_pages	∀ vmstat: swap_pages(vmstat) = Some(a sat+ b) when Key(vmstat,"pswpin") = Some(a) and Key(vmstat,"pswpout") = Some(b), otherwise None.	T::vmstat_sums_swap_in_and_out	&str, Option `?`, composition with keyed (the meminfo-facts Key lemma)	0,1,3,4,5,7,13
auxv-lookup	contract	linux::auxv_value	∀ bytes: &[u8], key: u64, with W = size_of::<usize>(): split bytes into the full 2W-byte chunks P_0..P_{m−1}, with m = len div 2W; a trailing partial chunk is ignored. Let k_j = usize::from_ne_bytes(P_j[..W]) and v_j = usize::from_ne_bytes(P_j[W..]), and let n ≤ m be the least index with k_n = 0 (AT_NULL), or m if none is 0. auxv_value(bytes,key) = Some(v_j) for the least j < n with k_j = key, and None if no j < n matches (in particular key = 0 always gives None, and a cut pair len < 2W gives None).	T::the_auxiliary_vector_gives_the_page_size	&[u8] as_chunks (const generic), copy_from_slice, from_ne_bytes, take_while/find_map closures	0,1,3,5,7,11,13
read-bounded-cap	contract	read_bounded, read_bounded_bytes	Effects(fs). ∀ path p: read_bounded_bytes(p) = Err(kind) when open fails, or when a read fails with any kind other than Interrupted (which is retried). Otherwise it is Ok(the first min(N, READ_CAP) bytes the file yields), where N is the bytes yielded before EOF. |result| ≤ READ_CAP for every file, including a device that never ends (exactly READ_CAP bytes). read_bounded(p) = read_bounded_bytes(p).ok().map(from_utf8_lossy). Pure core (provable modulo gaps 2 and 7): the loop invariant 0 ≤ n ≤ READ_CAP with buf[..n] equal to the concatenation of the reads.	T::every_proc_read_is_bounded_to_4_kib	fs::File, io::Read loop over &mut [u8; 4096], ErrorKind, Vec<u8>, String::from_utf8_lossy	0,1,2,7,8
pressure-level-table	table-property	pressure_of	∀ level: i64: pressure_of(level) = {1 ↦ Normal, 2 ↦ Warn, 4 ↦ Critical, otherwise None}.	T::kernel_pressure_levels_map_to_memory_levels	literal match into an Option of a local enum	0,1,3
used-mib-of	contract	used_mib_of	∀ internal, purgeable, wired, compressor, page: used_mib_of(...) = None if page = 0. Otherwise, with pages = ((internal sat- purgeable) sat+ wired) sat+ compressor, it is u32::try_from((pages sat* page) >> 20).ok(). Total on all u64 inputs. The result is not capped at physical memory.	T::memory_in_use_is_counted_from_the_vm_pages	u64 scalar arithmetic, saturating ops, u32::try_from; seal 150efe4a: E6 refuses (Option<u32> return, saturating calls, Assert)	0,1,3
available-mib-of	contract	available_mib_of	∀ free, inactive, purgeable, page, physical: available_mib_of(...) = None if page = 0. Otherwise it is Some(min(((free sat+ inactive sat+ purgeable) sat* page) >> 20, physical)), where the min applies only when physical is Some. Corollary: the result never exceeds physical when physical is known.	T::memory_available_is_free_inactive_and_purgeable_pages	u64 scalar arithmetic, Option<u64> map_or with a closure	0,1,3,13
swap-meter-rate	refinement	SwapMeter::new, SwapMeter::observe	State (base: Option<(Instant,u64)>, rate: Option<u32>), with new() = (None, None). observe(at, kib): kib = None resets to (None, None) and returns None. With no base, it sets base = (at, kib) and returns None. Otherwise let span = at sat- base_at. If span > SWAP_SPAN_MAX, the state becomes (Some((at,kib)), None) and observe returns None. If span < SWAP_SPAN_MIN, the state is unchanged and observe returns rate. Otherwise r = min(u32::MAX, ((kib sat- base_kib) sat* 1000) div max(1, span_ms)), the state becomes (Some((at,kib)), Some(r)), and observe returns Some(r). Corollaries: a counter that went backwards gives rate 0; a second reader within SWAP_SPAN_MIN repeats the last rate; a rate is never taken over a span longer than SWAP_SPAN_MAX.	T::the_swap_meter_reads_a_rate_over_a_span_of_its_readings	&mut self two-state, Option<(Instant,u64)>, Duration comparison/as_millis (u128), try_from	0,1,2,3
process-cwd-path-own	other	process_cwd, process_path, macos::process_cwd, macos::process_path, win_paths::process_path	Effects(proc_pidinfo/proc_pidpath, /proc/<pid>/{cwd,exe}, or QueryFullProcessImageNameW). ∀ pid: process_cwd(pid) = Some(p) ⇒ p is absolute and equals the OS's current-directory path for pid. process_path(pid) = Some(p) ⇒ p is absolute and equals the OS's executable path for pid (truncated to PATH_CAP bytes on macOS). process_cwd(0) = process_path(0) = None, and on macOS both are None for pid > i32::MAX (pid is filtered to 0 < pid ≤ i32::MAX). On Windows process_cwd reads the process environment block's CurrentDirectory (peb::cwd: Effects(OpenProcess, NtQueryInformationProcess, ReadProcessMemory)), spelled as the process's own GetCurrentDirectoryW answers it (no trailing separator but at a root), None for a process that refuses PROCESS_VM_READ or has exited (peb::tests::a_processs_working_directory_is_read_from_its_environment_block); process_path reads QueryFullProcessImageNameW through an OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) handle (with SYNCHRONIZE when granted, refusing an exited process), None when refused or exited. Every other platform answers None.	T::a_processs_working_directory_and_executable_are_its_own	FFI, unsafe zeroed structs, OsStr from bytes, PathBuf, fs::read_link	0,1,8
group-memory-members	other	group_memory, macos::group_memory, macos::footprint_kib	Effects(libproc). ∀ pid: group_memory(pid) = None if pid = 0, if pid > i32::MAX, if the short info is unreadable, if pgid = 0, or if no member was measured. Otherwise it is Some{pgid = pbsi_pgid(pid), measured = the number of the first min(n, MAX_GROUP_PIDS) listed group members > 0 whose footprint_kib is Some, footprint_mib = (sat-sum of those footprint_kib)/1024}, where footprint_kib(m) = proc_pid_rusage(m, RUSAGE_INFO_V4).ri_phys_footprint / 1024 when the call succeeds. So a child that took its own process group is never counted in its parent's group. On every platform but macOS it is always None.	T::a_groups_memory_is_its_own_members_footprint	FFI proc_listpids into a fixed array, unsafe, loop with a saturating sum	0,7,8
machine-memory-within-physical	other	physical_memory_mib, machine_memory, memory_pressure, clamp_physical_mib, macos::machine_memory, macos::physical_memory_mib, macos::memory_pressure	Effects(sysctl and HOST_VM_INFO64, or /proc/meminfo). machine_memory() = Some(m) ∧ physical_memory_mib() = Some(p) ⇒ m.available_mib ≤ p ∧ m.used_mib ≤ p on both macOS and Linux. Pure cores: macOS uses available_mib_of(free, inactive, purgeable, page, physical) and clamp_physical_mib(u64::from(used_mib_of(internal, purgeable, wire, compressor, page)), physical); Linux uses available = min(avail/1024, total/1024) and used = (total sat- avail)/1024 over meminfo-facts' (total, avail). physical_memory_mib is (hw.memsize >> 20).filter(> 0) on macOS and (total_kib ≥ 1024).then(total_kib/1024) on Linux. memory_pressure is kern.memorystatus_vm_pressure_level through pressure_of on macOS and None elsewhere.	T::the_machines_memory_and_pressure_are_read_on_this_host	FFI, OnceLock, composition of pure callees	0,4,8
swapped-kib-counter	other	swapped_kib, macos::swapped_kib	Effects(HOST_VM_INFO64 and vm.pagesize, or /proc/vmstat and /proc/self/auxv). On macOS swapped_kib() = (swapins sat+ swapouts) sat* page_kib with page_kib = vm.pagesize div 1024, and None when that is 0 or unread or the kernel refuses the statistics. On Linux it is swap_pages(read_bounded(/proc/vmstat)) sat* page_kib, with page_kib = auxv_value(read_bounded_bytes(/proc/self/auxv), 6 = AT_PAGESZ) div 1024 when that is > 0, else 4, read once per process. Every other platform answers None. The kernel's counters only grow, so two reads k1 then k2 on one host satisfy k1 ≤ k2.	T::the_swap_counter_reads_on_this_host_and_never_runs_backwards	FFI, OnceLock, composition with swap-pages-sum and auxv-lookup	0,4,8
stat-pgrp	contract	linux::stat_pgrp	∀ stat: stat_pgrp(stat) = the third whitespace token after the LAST ')' of stat, parsed as u32 (the pgrp field after state and ppid), or None when stat has no ')', fewer than three tokens follow it, or that token does not parse. So a command name holding spaces or ')' cannot move the field.	T::a_stat_line_gives_its_group_past_any_command_name	&str rsplit_once, iterator nth, u32 parse	0,1,3,5,7,13
lone-group-leader	other	lone_group_leader, macos::lone_group_leader, macos::complete_lone_group, macos::short_info	Effects(libproc). macOS returns a nonempty kernel name only for a readable positive leader whose aligned, nonempty, nonfull listing contains exactly that one positive pid (zero padding permitted). Failed, empty, misaligned, full, overlong, absent-leader or extra-member listings return None. The pure complete_lone_group predicate enforces the listing rule and its test reads captured arrays only. Every non-macOS platform returns None: Linux direct children are not a complete group census. All observations are snapshots, without a process-lifetime guarantee.	T::only_a_complete_group_census_admits_its_leader	FFI proc_pidinfo and proc_listpids into fixed arrays; pure byte-count and membership adapter	0,7,8
children-pids	contract	linux::children	∀ text: children(text) = the words w of text.split_inclusive(' ') that end in ' ' and whose w minus that space parses as u32, parsed, in order. So a pid cut at the 4 KiB cap (no closing space) is never read as a shorter pid, and a word that is not a pid is skipped.	T::a_children_text_names_only_the_pids_its_spaces_close	&str split_inclusive, strip_suffix, u32 parse, filter_map	0,1,3,5,7,13
stdin-sharers	other	stdin_sharers, macos::stdin_sharers, macos::stdin_device, macos::short_info, linux_stdin_sharers	Effects(libproc, or /proc). macOS: stdin_sharers(l) = None if l = 0, l > i32::MAX, the short info is unreadable or names a group other than l, l's descriptor 0 is not a character device (proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO) answers other than the whole struct, or vst_mode is not S_IFCHR), or the group listing writes no bytes. Otherwise it is Some(the positive members ≠ l among the first min(n, MAX_GROUP_PIDS) listed whose descriptor 0 is a character device with l's (vst_dev, vst_ino)). Linux: the same over l's children read from /proc/l/task/*/children (children-pids) whose stat_pgrp is l and whose /proc/<pid>/fd/0 has l's (dev, ino), None when no children file was read. Every other platform answers None. A snapshot, without a process-lifetime guarantee.	T::a_leaders_input_is_shared_only_by_members_on_that_very_device	FFI proc_pidinfo, proc_listpids and proc_pidfdinfo into fixed arrays; the jobs::darwin::vnode decoder	0,7,8
group-usage-members	other	group_usage, macos::group_usage, procs::imp::rusage, procs::ticks_to_ns	Effects(libproc). group_usage(g) = None if g = 0, g > i32::MAX, the listing fails, or off macOS. Otherwise it is Some(the positive pids proc_listpids(PROC_PGRP_ONLY, g) lists, at most MAX_GROUP_PIDS, each whose proc_pid_rusage(RUSAGE_INFO_V4) reads, as MemberUsage{pid, start = ri_proc_start_abstime, cpu_ns = ticks_to_ns(ri_user_time sat+ ri_system_time), runnable_ns = ticks_to_ns(ri_runnable_time), io_bytes = ri_diskio_bytesread sat+ ri_diskio_byteswritten sat+ ri_logical_writes, footprint = ri_phys_footprint, peak_footprint = ri_lifetime_max_phys_footprint}), in listing order; a group with no member left is Some([]). A member's counters are the kernel's as read, and they can fall while its (pid, start) lives: ri_system_time fell by 0.2-0.42 ms within 600 ms of a child's exec in 5 of 12 spawns on this Mac (2026-10-27), so a reader of their growth saturates it, as aterm-verify's used_between does. ri_runnable_time includes the time on a core, but the kernel books it apart, so it may trail cpu_ns slightly.	T::a_groups_usage_is_its_own_members_cpu_and_runnable_time	libproc FFI into a fixed array, a repr(C) struct read, saturating u64 sums, Mach timebase	0,1,3,8
hid-idle-clock	other	hid_idle, macos::hid_idle	Effects(IOKit). On macOS hid_idle() = Some(Duration::from_nanos(n)) when the IOHIDSystem service matches and its HIDIdleTime property reads as an unsigned number n, read from the IORegistry with no WindowServer connection; else None. Off macOS it is None.	T::the_input_idle_clock_reads_on_a_mac_and_nowhere_else	IOKit FFI, CFNumber read, an io_object released on every path	0,8
program-check-judge	contract	jobs::judge_start	∀ f: StartFacts: judge_start(f) = Started unless f.lone_zero ∧ ¬f.stopped ∧ ¬f.traced ∧ f.age_ms ≥ Some(PROGRAM_CHECK_AGE_MS = 20000) (None < Some, so an unread age is Started); otherwise Translating if f.translated, else Unstarted if f.local ≠ Some(true) (an unread mount proves nothing local), else Held. Corollaries: only Held for a lone unstarted thread past the bound, native and local; monotone in age past the bound.	src/jobs/tests.rs::the_program_check_judge_holds_only_an_old_lone_unstarted_thread	a struct of bools and Options, Option<u64> ordering, a fieldless enum return	0,1,3
program-check-read	other	jobs::read_start, jobs::read_start_in, jobs::at_start, jobs::local_at, jobs::macos::{read_start,start_facts,thread_handles,translated}, JobsReader::read_all (start_facts), ProcFacts::program_check, tail_allowed	Effects(libproc, sysctl, getfsstat). read_start(pid, now) = read_start_in(pid, now, a fresh mount cell); read_start_in(pid, now, cell) = None for pid = 0, pid > i32::MAX, a refused or empty two-slot PROC_PIDLISTTHREADS, and, when the list is [0], a refused PROC_PIDTBSDINFO or (on an arm64 Mac) PROC_PIDARCHINFO; off macOS always None. A list other than [0] is Some(StartFacts::default()) with nothing more read. For [0]: stopped ⇔ pbi_status = SSTOP, traced ⇔ pbi_flags & PROC_FLAG_TRACED ≠ 0, translated ⇔ the host is arm64 ∧ p_cputype = CPU_TYPE_X86_64, age_ms = (now − pbi_start µs) div 1000 (None when the start is after now), local = local_at(proc_pidpath(pid), the cell's table, read into it at first need). local_at(path, t) = None when no mount of t holds path, None when t is cut (¬t.complete) and that mount is local, else Some(local ∧ ¬automounted ∧ fstype ≠ autofs); tail_allowed(path, t) needs local_at(path, t) = Some(true). No argument or environment is read. at_start(pid) = read_start(pid, now_us()).map(judge_start). JobsReader::read_all = read_all_at(now_us()), which sets start_facts to the same facts from the full thread list and the BSD record it reads (local from the executable path it already reads and its one mount cell), None for a zombie or a refused list, and None on the Linux arm; ProcFacts::program_check() = start_facts.map(judge_start).	src/jobs/tests.rs::a_start_suspended_child_reads_one_unstarted_thread_and_is_not_held, src/jobs/tests.rs::a_program_stopped_after_it_ran_keeps_its_thread_handles, src/jobs/tests.rs::a_member_reading_carries_its_program_check, src/jobs/tests.rs::an_arch_answer_gives_its_cpu_type, src/jobs/tests.rs::a_cut_mount_table_proves_no_volume_local	FFI proc_pidinfo into fixed buffers, OnceLock and OnceCell, composition with program-check-judge and volumes::mount_of	0,7,8
procargs-parse	contract	procargs::{parse,read,read_into,ProcArgs2::whole,ProcArgs2::env}	∀ buf: parse(buf) = None iff |buf| < 4, argc = i32 ne(buf[..4]) < 0, or no NUL ends the exec path after it; else exec_path = the bytes up to that NUL, and after skipping every NUL that follows it (an empty argv[0] reads as padding, as ps reads it) argv = the first min(argc, k) NUL-terminated words, k the number the buffer holds. whole ⇔ |argv| = argc. env = the NUL-terminated strings after the words up to the first empty one, and none when not whole. Nothing is reserved past 1024 words for any argc. read(pid) and read_into(pid, buf) answer the kernel's whole record from one read (read into a kern.argmax + 4 buffer; a buffer sized by a size query races an exec, measured 2026-10-20), and None for an answer that fills the buffer: a record past the buffer is handed over as its tail, never its start.	src/procargs_tests.rs::a_whole_answer_gives_the_path_the_words_and_the_environment, src/procargs_tests.rs::the_padding_is_skipped_whatever_its_length, src/procargs_tests.rs::an_argc_of_zero_reads_no_words_and_the_environment, src/procargs_tests.rs::a_cut_buffer_answers_the_words_it_holds_and_no_environment, src/procargs_tests.rs::a_buffer_without_its_header_is_no_answer, src/procargs_tests.rs::this_process_reads_back_as_it_was_exec_d, src/procargs_tests.rs::a_record_past_what_the_kernel_hands_over_reads_as_nothing, src/procargs_tests.rs::a_reused_buffer_reads_a_record_whole_or_not_at_all	&[u8] slicing with get, position scans, i32 from_ne_bytes, a bounded Vec, an iterator closure over a cursor. read/read_into are the sysctl FFI.	0,1,3,5,7
vnode-terminal	contract	jobs::terminal_path, jobs::darwin::{vnode,VnodeFd::char_device,VnodeFd::terminal,VnodeFd::controlling}, jobs::macos::vnode_kind, jobs::linux::parse_fd_link	terminal_path(p) ⇔ p starts with /dev/tty, /dev/pty, /dev/pts/ or /dev/cu., or p = /dev/ptmx or p = /dev/console. ∀ b with |b| ≥ VNODE_PATH_SIZE and v = vnode(b): v.terminal() ⇔ v.mode & S_IFMT = S_IFCHR ∧ terminal_path(v.path), v.path being the bytes at VNODE_PATH_AT up to the first NUL; vnode_kind's Device { tty, ctty } is (v.terminal(), v.controlling()), v.controlling() ⇔ v.mode & S_IFMT = S_IFCHR ∧ v.path = /dev/tty, and the Linux reader's ctty is its `fd/<n>` link = /dev/tty: a descriptor opened as the controlling terminal, never one inherited or duplicated from the terminal's own name. parse_fd_link(t) = Tty for every t that is no pipe, socket or anon inode and terminal_path(t); else Device for any other /dev/ path. So /dev/ttys003, /dev/tty, /dev/tty.usbserial-10, /dev/cu.usbserial-10, /dev/ptyp0, /dev/ptmx, /dev/pts/3 and /dev/console are terminals on both platforms; /dev/null, /dev/zero, /dev/consoles and /dev/ptmx0 are not, and no regular file is, whatever its path.	src/jobs/tests.rs::a_vnode_answer_gives_mode_size_identity_and_path, src/jobs/tests.rs::fd_links_and_fdinfo_parse	&[u8] starts_with, bit masks	0,1,3
fresh-dylib	contract	jobs::fresh_dylib, jobs::darwin::vnode (mtime_s), jobs::macos::{vnode_kind,fds,read}	∀ path, mtime_s, start: fresh_dylib(path, mtime_s, start) ⇔ start = Some(s) ∧ mtime_s ≥ 0 ∧ path ends with ".dylib" ∧ mtime_s × 10⁶ + FRESH_DYLIB_US (3,600,000,000) ≥ s (saturating). vnode(b).mtime_s = i64 ne(b[VNODE_MTIME_AT = 64..]). A macOS member reading's FdKind::File.fresh_dylib = regular ∧ fresh_dylib(the descriptor's vip_path, its vst_mtime, the reading's start); the Linux arm's is false.	src/jobs/tests.rs::a_fresh_dylib_is_one_written_at_most_an_hour_before_its_holder_started; src/jobs/tests.rs::a_members_descriptors_name_a_fresh_dylib_it_holds_open	byte-slice suffix, saturating u64 arithmetic, an Option start	0,1,3
unix-listener-decode	contract	jobs::darwin::unix_listeners	∀ t: unix_listeners(t) reads net.local.stream.pcblist_n's records from h = u32 ne(t[0..]) (its struct xunpgen head's length; the same 24-byte record ends the table) while more than h bytes remain (its tail), each a u32 length len and a u32 kind, the next at len rounded up to 8. None when t has no head, a record runs past the end or has len < 8, or an XSO_UNPCB (0x200) record's len ≠ XUNPCB_SIZE (588) or an XSO_SOCKET (1) record's ≠ XSOCKET_SIZE (104); a record of another kind is passed over. An XSO_UNPCB sets the bound path p to its bytes [XUNPCB_PATH_AT = 78 .. 78 + SUN_PATH_LEN (104)] up to the first NUL (all 104 with none), unset when that is empty; the next XSO_SOCKET takes p and, when u32 ne at XSOCKET_OPTIONS_AT (20) & SO_ACCEPTCONN (2) ≠ 0, yields {pid: u32 at XSOCKET_LAST_PID_AT (68), uid: u32 at XSOCKET_UID_AT (64), path: p}. So a stream a listener accepted, which carries its path, and an unbound listener yield no row, and a table with no stream is Some([]). The records' layout (xunpcb_n, xsocket_n) is macOS 27.0.1's private one, measured: no SDK header declares them (sys/unpcb.h declares only xunpgen), so its reader (aterm_uds::holders::scan_listener_holders) refuses a read that does not name a listener of its own.	src/jobs/tests.rs::the_unix_stream_table_names_each_bound_listener	&[u8] get, from_ne_bytes, checked offsets	0,1,3
net-unix-listening	contract	jobs::linux::listening_unix	∀ bytes text: listening_unix(text) = [(inode, path bytes)] for each line after the header and before the last '\n' whose seven space-separated fields parse with Flags (hex) & __SO_ACCEPTCON (0x10000) ≠ 0 and Inode (decimal), and whose path, every byte after the one space past the inode, is not empty and does not start with '@' (an abstract name). A row cut at the read's end, an unbound socket and a connected or accepted stream at a listener's path are left out; a path that is no UTF-8 is kept by its bytes, and no row spoils another.	src/jobs/tests.rs::listening_rows_of_proc_net_unix	byte splitting, from_str_radix, filter_map	0,1,3
socket-settled	contract	jobs::macos::settled	∀ flags, e, a: settled(flags, e, a) = false when flags & PROC_FLAG_INEXIT ≠ 0; otherwise e when flags & PROC_FLAG_EXEC ≠ 0; otherwise a ≥ Some(SETTLE_US = 2,000,000) (an unread age, None, is not). So an exiting process is never settled, a program it exec'd is settled exactly when the caller says the reader's last pass found it running that program, and a fork that never exec'd is settled once it is SETTLE_US µs old.	src/jobs/tests.rs::only_a_settled_process_has_its_sockets_read	bit masks, an Option<u64> comparison	0,1,3
socket-read-gate	other	jobs::macos::{fds,spawning,spawning_with,sockets_readable,read}, machine::owned_count, jobs::sockets_readable, JobsReader::read_all_at	Effects(libproc, sysctl). spawning(p, now) = true when p's PROC_PIDTBSDINFO is refused or its pbi_flags has PROC_FLAG_INEXIT; otherwise spawning_with(now, the MAX_CHILDREN (256) slots, n = proc_listchildpids(p), the errno it left, kinfo's start). spawning_with is true when the list is incomplete (machine::owned_count is None: n < 0, n ≥ the slots, or n = 0 with errno set) or a listed pid > 0 has a start that is unread (a child gone since the listing too), 0, or less than SETTLE_US before now (saturating, so a start after now counts); otherwise false. fds(p, …, now, s) reads PROC_PIDFDSOCKETINFO of none of p's descriptors unless s ∧ some socket lies within the detail budget (the first MAX_FD_DETAILS (128) vnode, pipe, socket and kqueue descriptors of the list, in order) ∧ ¬spawning(p, now), checked after the list and before any detail read; then it reads every socket within the budget before any other detail. Every other socket reads FdKind::Other. read(pid, fds = true, …, now, execd_then) passes s = settled(pbi_flags, execd_then(start, name), now − start) (socket-settled), false when the BSD record is unread. On macOS sockets_readable(pid) = settled(pbi_flags, true, now − start) at now_us() (a timed reader's gate: no spawning check, which cannot see a spawn that starts after it), false for pid 0, pid > i32::MAX or a refused BSD record; it is true on every other platform. Each is a snapshot: a process can start a spawn, exit or close a socket right after it.	src/jobs/tests.rs::a_programs_socket_reads_from_the_second_pass_that_finds_it_running, src/jobs/tests.rs::a_spawners_socket_reads_other_while_its_child_is_young, src/jobs/tests.rs::a_forks_socket_reads_once_it_kept_its_parents_program_long_enough, src/jobs/tests.rs::a_timed_readers_gate_passes_a_program_at_once_and_a_fork_once_it_is_settled, src/jobs/tests.rs::only_complete_child_observations_clear_the_spawn_guard	FFI proc_pidinfo, proc_listchildpids and proc_pidfdinfo into fixed buffers, sysctl(KERN_PROC_PID), composition with socket-settled	0,7,8
```

### Kinds (28)

| kind | count | ids |
|---|---|---|
| contract | 15 | meminfo-facts, swap-pages-sum, auxv-lookup, read-bounded-cap, used-mib-of, available-mib-of, stat-pgrp, children-pids, program-check-judge, procargs-parse, vnode-terminal, fresh-dylib, unix-listener-decode, net-unix-listening, socket-settled |
| table-property | 1 | pressure-level-table |
| invariant | 1 | whole-lines-torn-tail-ignored |
| refinement | 1 | swap-meter-rate |
| other (effectful host facts) | 10 | process-cwd-path-own, group-memory-members, machine-memory-within-physical, swapped-kib-counter, lone-group-leader, stdin-sharers, group-usage-members, hid-idle-clock, program-check-read, socket-read-gate |

Total: 15 + 1 + 1 + 1 + 10 = 28.

### Gap histogram (number of the 28 specs each gap blocks)

| gap | specs blocked |
|---|---|
| 0 | 28 |
| 1 | 20 |
| 2 | 2 |
| 3 | 18 |
| 4 | 3 |
| 5 | 7 |
| 7 | 13 |
| 8 | 11 |
| 11 | 1 |
| 13 | 7 |

Gaps 6, 9, 10, 12, 14, 15, 16 and 17 are not listed per spec. Gap 6 (plain-Rust clause publication) and
gap 14 (Clean elaborator coverage) apply to every spec, depending on which lane discharges it, and gap 16
(soundness) applies to every proof. No spec here cites another crate's types any more, so gap 12 blocks
none.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/tests.rs	meminfo_gives_the_total_and_the_available	meminfo-facts	
src/tests.rs	vmstat_sums_swap_in_and_out	swap-pages-sum	
src/tests.rs	the_auxiliary_vector_gives_the_page_size	auxv-lookup	
src/tests.rs	every_proc_read_is_bounded_to_4_kib	read-bounded-cap	whole-lines-torn-tail-ignored
src/tests.rs	kernel_pressure_levels_map_to_memory_levels	pressure-level-table	
src/tests.rs	memory_in_use_is_counted_from_the_vm_pages	used-mib-of	
src/tests.rs	memory_available_is_free_inactive_and_purgeable_pages	available-mib-of	
src/tests.rs	a_processs_working_directory_and_executable_are_its_own	process-cwd-path-own	
src/tests.rs	a_groups_memory_is_its_own_members_footprint	group-memory-members	
src/tests.rs	the_machines_memory_and_pressure_are_read_on_this_host	machine-memory-within-physical	available-mib-of, used-mib-of, pressure-level-table
src/tests.rs	the_swap_counter_reads_on_this_host_and_never_runs_backwards	swapped-kib-counter	
src/tests.rs	the_swap_meter_reads_a_rate_over_a_span_of_its_readings	swap-meter-rate	
src/tests.rs	a_stat_line_gives_its_group_past_any_command_name	stat-pgrp	
src/tests.rs	only_a_complete_group_census_admits_its_leader	lone-group-leader	
src/tests.rs	a_children_text_names_only_the_pids_its_spaces_close	children-pids	
src/tests.rs	a_leaders_input_is_shared_only_by_members_on_that_very_device	stdin-sharers	
src/tests.rs	a_groups_usage_is_its_own_members_cpu_and_runnable_time	group-usage-members	
src/tests.rs	the_input_idle_clock_reads_on_a_mac_and_nowhere_else	hid-idle-clock	
src/jobs/tests.rs	the_program_check_judge_holds_only_an_old_lone_unstarted_thread	program-check-judge	
src/jobs/tests.rs	a_start_suspended_child_reads_one_unstarted_thread_and_is_not_held	program-check-read	program-check-judge
src/jobs/tests.rs	a_program_stopped_after_it_ran_keeps_its_thread_handles	program-check-read	
src/jobs/tests.rs	a_member_reading_carries_its_program_check	program-check-read	program-check-judge
src/jobs/tests.rs	an_arch_answer_gives_its_cpu_type	program-check-read	
src/jobs/tests.rs	a_cut_mount_table_proves_no_volume_local	program-check-read	
src/jobs/tests.rs	a_vnode_answer_gives_mode_size_identity_and_path	vnode-terminal	
src/jobs/tests.rs	fd_links_and_fdinfo_parse	vnode-terminal	
src/jobs/tests.rs	a_fresh_dylib_is_one_written_at_most_an_hour_before_its_holder_started	fresh-dylib	
src/jobs/tests.rs	a_members_descriptors_name_a_fresh_dylib_it_holds_open	fresh-dylib	vnode-terminal
src/jobs/tests.rs	the_unix_stream_table_names_each_bound_listener	unix-listener-decode	
src/jobs/tests.rs	listening_rows_of_proc_net_unix	net-unix-listening	
src/jobs/tests.rs	only_a_settled_process_has_its_sockets_read	socket-settled	
src/jobs/tests.rs	a_programs_socket_reads_from_the_second_pass_that_finds_it_running	socket-read-gate	socket-settled
src/jobs/tests.rs	a_spawners_socket_reads_other_while_its_child_is_young	socket-read-gate	socket-settled
src/jobs/tests.rs	a_forks_socket_reads_once_it_kept_its_parents_program_long_enough	socket-read-gate	socket-settled
src/jobs/tests.rs	a_timed_readers_gate_passes_a_program_at_once_and_a_fork_once_it_is_settled	socket-read-gate	socket-settled
src/jobs/tests.rs	only_complete_child_observations_clear_the_spawn_guard	socket-read-gate	
src/procargs_tests.rs	a_whole_answer_gives_the_path_the_words_and_the_environment	procargs-parse	
src/procargs_tests.rs	the_padding_is_skipped_whatever_its_length	procargs-parse	
src/procargs_tests.rs	an_argc_of_zero_reads_no_words_and_the_environment	procargs-parse	
src/procargs_tests.rs	a_cut_buffer_answers_the_words_it_holds_and_no_environment	procargs-parse	
src/procargs_tests.rs	a_buffer_without_its_header_is_no_answer	procargs-parse	
src/procargs_tests.rs	this_process_reads_back_as_it_was_exec_d	procargs-parse	
src/procargs_tests.rs	a_record_past_what_the_kernel_hands_over_reads_as_nothing	procargs-parse	
src/procargs_tests.rs	a_reused_buffer_reads_a_record_whole_or_not_at_all	procargs-parse	
```

## 3. Coverage

- 44 tests mapped across `src/tests.rs` (18), `src/jobs/tests.rs` (18) and `src/procargs_tests.rs` (8).
  All 44 map to one of the 28 specs in §1.
- One spec has no primary test of its own (`whole-lines-torn-tail-ignored`). It is sampled as a secondary
  part of `every_proc_read_is_bounded_to_4_kib`, whose primary spec is `read-bounded-cap`. That test is
  deleted only when both of its specs are proved.
- Additional unrecovered test suites live in `src/tests.rs`, `src/jobs/tests.rs`, `src/coalitions_tests.rs`,
  `src/cpu_hardware.rs`, `src/cpu_hardware_macos.rs`, `src/cpu_policy.rs`, `src/identity_tests.rs`,
  `src/iflist.rs`, `src/kproc_tests.rs`, `src/linux_tests.rs`, `src/macos.rs`, `src/paths_tests.rs`,
  `src/peb.rs`, `src/power.rs`, `src/procs.rs`, `src/slots_tests.rs`, `src/timeline_tests.rs`,
  `src/volumes_tests.rs`, `src/windows_tests.rs` and `tests/{iflist,iokit,proc_info}_sdk.rs`.
- Ten specs in §1 are `Effects(...)` rows (plus `read-bounded-cap` and the live `read`/`read_into` part of
  `procargs-parse`); they retire only with a platform model (gap 8). Until then, the pure-core halves
  can be proved and the host test can be cut down to the effectful part. The lone-group test exercises
  its production pure census adapter, not libproc. The unsafe child-process fixture was removed because
  its cleanup did not run on assertion failure and could signal a recycled group id. The stdin-sharers test
  starts its `sleep`s through `spawnfd::command` and holds each in a guard that kills and reaps it by its
  own unreaped pid, on a panic too, so it never signals a group. The `unix` test builds its
  auxiliary vector from fixed pairs and reads nothing of the host.

## 4. Tests that are not spec samples

None. The three the recovery found (`the_probe_can_move_to_its_thread`, `the_services_noun_is_the_platforms`
and the latency ceiling `a_reading_and_a_sweep_cost_milliseconds_not_more`) were removed with `Probe`,
`SERVICES_NOUN` and the sweep.

## 5. Implied specs that look wrong, or disagree with the code

1. **Resolved — `used_mib_of` saturates every addition.** `used_mib_of` (lib.rs) uses
   `internal.saturating_sub(purgeable).saturating_add(wired).saturating_add(compressor)`, pinned at
   `u64::MAX` by `memory_in_use_is_counted_from_the_vm_pages`.
2. **Resolved — `macos::machine_memory` caps `used_mib` at physical memory.** `used_mib_of` remains the
   uncapped page-class sum (`used-mib-of`), and `macos::machine_memory` passes its result through
   `clamp_physical_mib(u64::from(used_mib), physical)` alongside `available_mib_of`, so
   `machine-memory-within-physical` (`used_mib ≤ physical` when `physical` is known) holds by construction on
   macOS as well as Linux.
3. **Resolved — `linux::auxv_value` stops at `AT_NULL` (`0`).** `auxv_value` terminates at the first
   `k == 0` pair, so `auxv_value(bytes, 0)` is `None` and pairs past `AT_NULL` are unread.
4. **Resolved — scratch directory uses `aterm_tempfile::TempDir`.** `every_proc_read_is_bounded_to_4_kib`
   now holds an `aterm_tempfile::TempDir` so its scratch files are cleaned up on panic too.
5. **Resolved — child spawns use `aterm_uds::spawnfd::command`.** `a_groups_memory_is_its_own_members_footprint`
   and `a_groups_usage_is_its_own_members_cpu_and_runnable_time` build their children through
   `aterm_uds::spawnfd::command`. The +384 MiB-after-512 MiB margin remains a host-load heuristic, not a
   spec bound.
6. **Resolved — swap counter test checks monotonicity only.** `the_swap_counter_reads_on_this_host_and_never_runs_backwards`
   now asserts only `second >= first`, dropping the `< 1 GiB` host-load heuristic.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone. The one scalar spec needs only gaps 0 and 3 (casts, shifts, std
min/max in E6 bodies), so it comes first.

1. **`used-mib-of`** (gaps 0, 3). Scalar `u64` saturating arithmetic and a `try_from`. Proving it forces
   the fix of §5.1 (saturating adds or a `requires`). It retires `memory_in_use_is_counted_from_the_vm_pages`
   and is a pure core of `machine-memory-within-physical`.
2. **`pressure-level-table`** (gaps 0, 1, 3: a finite match into an `Option` of a local enum) and
   **`available-mib-of`** (gaps 0, 1, 3, 13: one `Option` and a `map_or` closure).
3. **`swap-meter-rate`**, once gap 2 (two-state `&mut self`) lands. A small state machine with scalar
   fields that retires one test.
4. The `/proc` text parsers, once gaps 1, 5 and 7 land: first a shared `WL`/`Key` lemma, then
   `whole-lines-torn-tail-ignored`, `meminfo-facts` and `swap-pages-sum`; `auxv-lookup` also needs
   gap 11 (`as_chunks`' const generic). These retire 3 tests.
5. The `Effects` rows, last, with a platform model (gap 8): `read-bounded-cap` (its pure loop core can be
   proved at step 3), `process-cwd-path-own`, `group-memory-members`, `machine-memory-within-physical` and
   `swapped-kib-counter`.
