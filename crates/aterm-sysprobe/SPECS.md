<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-sysprobe: spec recovery from its 30 unit tests

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

Source: `crates/aterm-sysprobe` (read only). Every `#[test]` is in `crates/aterm-sysprobe/src/tests.rs`
(30 tests: 23 run on every host, 1 needs `unix`, 1 needs `macos` or `linux`, 5 need `macos`). I read each
one and the code it reaches in `src/lib.rs`, `src/linux.rs` and `src/macos.rs`. Nothing was compiled or
run. The claims below come from reading the code.

Result: **31 specs**, covering **27 tests**. **3 tests are not spec samples** (§4).

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
- `Tok_i(l)` is the i-th (0-based) whitespace token of l.
- `PM(s)` = `linux::percent_to_pm(s)`. Write s as `W[.F]`. W must parse as `u32`, and the first two chars of
  F must be decimal digits (F may be shorter). Then PM(s) = min(1000, (100·W + 10·F0 + F1 + 5) div 10).
  Otherwise PM(s) = None.
- `sat+`, `sat-`, `sat*` are the `u64` saturating ops. `cap64(x: u128)` = min(x, `u64::MAX`).
- `DeltaCpu(r,prev)` = `r.cpu_ns.map_or(0, |ns| ns sat- prev.get(r.pid).unwrap_or(0))`.
  `Fp(r)` = `r.footprint_kib.unwrap_or(0)`.
- `TopK(key,S)` = the first `NAMED_ROWS` of the indices i with key(i) > 0, stably sorted by key descending
  (ties keep index order).
- `Effects(...)` marks a statement about the OS (procfs, sysctl, libproc, Mach, NSProcessInfo). It can be
  proved only with a platform model (gap 8). Its pure core is stated separately wherever one exists, and
  that core is the part worth proving first.
- `ProcRow`, `Reading`, `Reading::empty`, `Psi`, `Thermal`, `MemoryLevel` come from `aterm-messages`
  (cross-crate, gap 12). `Thermal` is ordered Nominal < Fair < Serious < Critical, and the sampler takes
  its max.
- Gap numbers are the rows of `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7. Gap 0 (the installed
  seal publishes no Lean-discharged row) is a prerequisite for every spec, so every row lists it. "Blocked
  only by gap 0" means no row here is blocked by gap 0 alone. The two closest rows need gaps 0 and 3.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary in §2),
`difficulty` (the Rust features the proof must handle), `blocking_gaps` (audit §7).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
whole-lines-torn-tail-ignored	invariant	linux::whole_lines, linux::cpu_ticks, linux::psi, linux::meminfo, linux::swap_pages, linux::status_uid	∀ P ∈ {cpu_ticks, psi, meminfo, swap_pages, status_uid}, ∀ t that is empty or ends with '\n', ∀ s with no '\n': P(t ++ s) = P(t). Equivalently, P(x) depends only on WL(x). Corollary: when x is t cut at any byte c that does not fall right after a '\n', the line containing c is never parsed, so no number's prefix (e.g. `pswpout 34` of `pswpout 3400`) is ever read as a value.	T::every_proc_read_is_bounded_to_4_kib (torn vmstat and torn stat parts)	&str slicing at rfind, Iterator::lines/find/find_map with closures, Option	0,1,3,5,7,13
cpu-ticks-aggregate	contract	linux::cpu_ticks	∀ stat: let l be the first line of WL(stat) that starts with "cpu ", and v the tokens of l after the first. cpu_ticks(stat) = None if there is no such l, if any token of v fails to parse as u64, or if |v| < 4. Otherwise, with v_i = 0 for i ≥ |v|, b = v0+v1+v2+v5+v6+v7 and cpu_ticks(stat) = Some((b, b+v3+v4)). guest (v8) and guest_nice (v9) are never added, and a cpuN line never matches. requires (no panic): the sums do not overflow u64, since the code uses plain + (§5).	T::proc_stat_reads_the_aggregate_line_and_skips_guest	&str, Vec<u64> via collect::<Result>, closures, unchecked u64 add	0,1,3,5,7,13
psi-avg10-permille	contract	linux::psi, linux::percent_to_pm	∀ text: psi(text) = (A("some"), A("full")). A(k) takes the first line l of WL(text) with Tok_0(l) = k, then the first token of l with prefix "avg10=". If both exist, A(k) = PM(the rest of that token), otherwise None. PM(s) ≤ 1000. PM rounds half up to one permille ("42.87" gives 429, "10.05" gives 101, "21.94" gives 219, "100.00" gives 1000). PM is None on a non-numeric whole part or on a non-digit in the first two fraction chars. requires (no panic): W ≤ 42,949,672 (§5).	T::pressure_files_read_avg10_as_permille (psi part)	&str split_once/strip_prefix/chars, closures, u32 arithmetic	0,1,3,5,7,13
psi-of-projection	contract	linux::psi_of	∀ cpu, memory, io: Option<&str>: psi_of(cpu,memory,io) = None ⇔ all three are None. Otherwise it is Some(Psi{cpu_some_pm: cpu.and_then(|t| psi(t).0), memory_some_pm: memory.and_then(|t| psi(t).0), memory_full_pm: memory.and_then(|t| psi(t).1), io_full_pm: io.and_then(|t| psi(t).1)}).	(secondary only: T::pressure_files_read_avg10_as_permille, psi_of part)	Option<&str>, cross-crate struct Psi, composition with psi-avg10-permille	0,1,4,12
meminfo-and-shares	contract	linux::meminfo, linux::keyed, linux::MemInfo::{mib,used_mib,used_pm}	∀ text: meminfo(text) = Some(MemInfo{total_kib: Key(text,"MemTotal"), available_kib: Key(text,"MemAvailable")}) when both are Some, otherwise None. ∀ m: m.mib() = min(total/1024, u32::MAX); m.used_mib() = min((total sat- avail)/1024, u32::MAX); m.used_pm() = None if total = 0, otherwise Some(1000 − (min(avail,total)·1000) div total) ∈ 0..=1000. requires (no panic in used_pm): min(avail,total)·1000 ≤ u64::MAX (§5).	T::meminfo_gives_size_and_the_used_share	&str parsing via keyed (iterator, closures), Copy struct by value, u64→u32/u16 try_from	0,1,3,5,7,13
swap-pages-sum	contract	linux::swap_pages	∀ vmstat: swap_pages(vmstat) = Some(a sat+ b) when Key(vmstat,"pswpin") = Some(a) and Key(vmstat,"pswpout") = Some(b), otherwise None.	T::vmstat_sums_swap_in_and_out	&str, Option `?`, composition with keyed (the meminfo-and-shares Key lemma)	0,1,3,4,5,7,13
zone-thermal-table	table-property	linux::millidegrees, linux::zone_thermal	∀ text: millidegrees(text) = text.trim().parse::<i64>().ok(). ∀ temp: i64, trips: &[(&str,i64)]: let Past(K) ⇔ ∃ (k,a) ∈ trips with k.trim() ∈ K, a > 0 and temp ≥ a. zone_thermal(temp,trips) = Critical if Past({critical, hot}), else Serious if Past({passive}), else Nominal. A zero or negative trip is never crossed, and every other kind (e.g. active) never counts. Corollary: zone_thermal is monotone non-decreasing in temp for fixed trips.	T::thermal_trips_map_to_states	slice of (&str,i64), Iterator::any with closure, enum result	0,1,3,5,7,13
pid-stat-last-paren	contract	linux::pid_stat, linux::statm_resident	∀ text: let o = the first index of '(' and c = the last index of ')'. Field n is the (n−3)-th whitespace token of text[c+1..]. pid_stat(text) = None if o or c is missing, if c < o, if text[..o].trim() is not a u32, if field 4, 14 or 15 is missing or not a u64, or if field 4 does not fit u32. Otherwise it is Some(PidStat{pid, comm: text[o+1..c], ppid: field4, ticks: field14 sat+ field15}), so comm may contain spaces and parentheses. ∀ t: statm_resident(t) = Tok_1(t).parse::<u64>().ok().	T::pid_stat_takes_the_comm_to_the_last_paren (pid_stat and statm parts)	&str find/rfind/slicing, String allocation, Vec<&str>, closure field indexing	0,1,3,5,13
scaled-ticks-saturating	contract	ticks_to_ns, linux::ticks_ns	∀ ticks: u64, numer, denom: u32: ticks_to_ns(ticks,numer,denom) = 0 if denom = 0, otherwise cap64(⌊ticks·numer / denom⌋), computed exactly in u128, so it never overflows and never divides by zero. ∀ ticks, clk: u64: ticks_ns(ticks,clk) = 0 if clk = 0, otherwise cap64(⌊ticks·10^9 / clk⌋). Equivalence: ∀ clk ≤ u32::MAX, ticks_ns(t,clk) = ticks_to_ns(t, 10^9, clk).	T::rusage_time_is_scaled_by_the_timebase	u128 widening multiply and divide, u64::try_from(u128).unwrap_or; scalar, straight-line plus one branch; seal 150efe4a: E6 refuses (Result local, From/TryFrom calls, Assert)	0,1,3
proc-row-composition	contract	linux::proc_row	∀ stat, statm, clk, page_kib, ours: proc_row(stat,statm,clk,page_kib,ours) = pid_stat(stat).map(|s| ProcRow{pid: s.pid, ppid: s.ppid, uid_is_ours: ours, name: s.comm, bundle: None, cpu_ns: Some(ticks_ns(s.ticks,clk)), footprint_kib: statm.and_then(statm_resident).map(|p| p sat* page_kib)}).	(secondary only: T::pid_stat_takes_the_comm_to_the_last_paren, proc_row part)	cross-crate struct with String/Option fields, composition of three callee contracts	0,1,4,12
linux-reading-from-texts	contract	linux::reading	∀ at, cores, page_kib, texts, thermal: let mem = texts.meminfo.and_then(meminfo). reading(at,cores,page_kib,texts,thermal) = Reading{at, cores, page_kib, thermal, mem_mib: mem.map_or(0, MemInfo::mib), busy_ticks: texts.stat.and_then(cpu_ticks), mem_used_pm: mem.and_then(used_pm), mem_used_mib: mem.map(used_mib), swap_pages: texts.vmstat.and_then(swap_pages), psi: psi_of(texts.psi_cpu, texts.psi_memory, texts.psi_io), pressure: None, low_power: None}. Corollary: reading(at,c,p,Texts::default(),None) = Reading{cores: c, page_kib: p, ..Reading::empty(at)}.	T::a_linux_reading_assembles_from_the_texts	struct with lifetime-borrowed &str fields, cross-crate Reading/Reading::empty, composition	0,1,4,12
stat-cores-whole-block	contract	linux::stat_cores	∀ stat: walk the lines L = stat.split('\n') in order, starting with n = 0. If l starts with "cpu" and the next char is an ASCII digit, n sat+= 1 (u16). If l starts with "cpu" otherwise, skip it. If l is empty or a proper prefix of "cpu" ("c", "cp"), skip it. Any other l returns (n > 0).then_some(n). If L runs out, the result is None. So stat_cores is Some only when a non-cpu line has begun after the block, however torn that line is (e.g. "intr 14…" or "i").	T::the_core_count_reads_only_a_whole_cpu_block (stat_cores part)	&str split/strip_prefix/starts_with with a char closure, loop with early return, u16 saturating	0,1,3,5,7,13
status-effective-uid	contract	linux::status_uid	∀ status: status_uid(status) = Tok_2(l).parse::<u32>().ok(), where l is the first line of WL(status) that starts with "Uid:" (Tok_2 is the effective uid), and None if there is no such line.	(secondary only: T::the_core_count_reads_only_a_whole_cpu_block, status_uid part)	&str, iterator find/nth with closures	0,1,3,5,7,13
auxv-lookup	contract	linux::auxv_value	∀ bytes: &[u8], key: u64, with W = size_of::<usize>(): split bytes into the full 2W-byte chunks P_0..P_{m−1}, with m = len div 2W; a trailing partial chunk is ignored. Let k_j = usize::from_ne_bytes(P_j[..W]) and v_j = usize::from_ne_bytes(P_j[W..]). auxv_value(bytes,key) = Some(v_j) for the least j with k_j = key, and None if no j matches. In particular a cut pair (len < 2W) gives None. The scan does not stop at AT_NULL (§5).	T::the_auxiliary_vector_gives_the_clock_tick_and_the_page_size (auxv_value part)	&[u8] as_chunks (const generic), copy_from_slice, from_ne_bytes, find_map closure	0,1,3,5,7,11,13
linux-sampler-facts	refinement	linux::Sampler::new, linux::Sampler::facts, linux::Roots::live	Effects(fs under roots). ∀ roots: let A(k) = read_bounded_bytes(proc/self/auxv).ok().and_then(|b| auxv_value(b,k)).filter(> 0). Sampler::new(roots).facts() is the 4-tuple (A(17).unwrap_or(100), A(6).map(/1024).filter(> 0 and fits u32).unwrap_or(4), read_bounded(proc/stat).and_then(stat_cores).unwrap_or_else(logical_cores), read_bounded(proc/self/status).and_then(status_uid)). Roots::live() = Roots{proc: "/proc", sys: "/sys"}.	T::the_linux_sampler_reads_the_fixed_facts_under_its_root	fs reads, PathBuf join, &self view, struct construction with HashMaps	0,1,2,4,8
linux-sampler-reading-refines	refinement	linux::Sampler::reading, linux::Sampler::thermal	Effects(fs). ∀ sampler state s, instant at: s.reading(at) = linux::reading(at, s.cores, s.page_kib, Texts{each field = read_bounded(proc/<file>)}, Th), and reading leaves s unchanged. Th is computed over the first MAX_ZONES entries of sys/class/thermal named thermal_zone* whose temp file reads through millidegrees. Each such zone's trips are (trip_point_i_type.trim(), millidegrees(trip_point_i_temp) or 0) for i ascending from 0, stopping at the first missing type file or at MAX_TRIPS. Th = the max over those zones of zone_thermal(temp, trips), and None if there is no such zone or the directory is unreadable.	T::the_linux_sampler_reads_a_whole_reading_from_the_fixture_tree	fs read_dir, loops with break/continue, Vec<(String,i64)>, &mut self, Ord::max on an enum	0,1,2,4,7,8,12
sweep-rows-and-path-cache	refinement	linux::Sampler::scan, linux::Sampler::row, macos::Mac::scan	Effects(fs, libproc). Linux: the rows are a map over pids = sorted(the first MAX_PIDS entries of the proc root whose name parses as u32). For each pid, read_bounded_kind(<pid>/stat) decides the row. Ok(t) gives proc_row(t, read_bounded(statm), clk, page_kib, uid.is_some() ∧ uid = our_uid) with uid = status_uid(status), kept only if row.pid = pid. PermissionDenied gives row_of(pid, None, Refused, u32::MAX). Any other error gives no row. macOS: one row_of per listed pid > 0 whose rusage is not Gone. On both arms, after sweep k: (a) |rows| ≤ MAX_PIDS; (b) at most MAX_PATH_READS bundles were read new in this sweep, and every other Some bundle equals the cache entry for the same (pid, name); (c) a row that had bundle b in sweep k and has the same (pid, name) in sweep k+1 has bundle b again; (d) the cache keys ⊆ the pids of the latest rows; (e) prev = {pid ↦ cpu_ns | cpu_ns is Some}.	T::a_linux_sweep_reads_every_pid_with_its_uid	&mut self two-state, HashMap/HashSet, Vec<ProcRow>, fs read_dir/read_link, libproc FFI, loops with a read budget, String truncate at a char boundary	0,1,2,4,7,8,11,12
read-bounded-cap	contract	read_bounded, read_bounded_kind, read_bounded_bytes	Effects(fs). ∀ path p: read_bounded_bytes(p) = Err(kind) when open fails, or when a read fails with any kind other than Interrupted (which is retried). Otherwise it is Ok(the first min(N, READ_CAP) bytes the file yields), where N is the bytes yielded before EOF. |result| ≤ READ_CAP for every file, including a device that never ends (exactly READ_CAP bytes). read_bounded_kind = map(from_utf8_lossy), and read_bounded = read_bounded_kind(p).ok(). Pure core (provable modulo gaps 2 and 7): the loop invariant 0 ≤ n ≤ READ_CAP with buf[..n] equal to the concatenation of the reads.	T::every_proc_read_is_bounded_to_4_kib	fs::File, io::Read loop over &mut [u8; 4096], ErrorKind, Vec<u8>, String::from_utf8_lossy	0,1,2,7,8
row-of-total	contract	row_of	∀ pid, ident, usage, our_uid: row_of(pid, ident, Gone, our_uid) = None. Otherwise, with id = ident.unwrap_or_default() (ppid 0, uid 0, comm ""), row_of = Some(ProcRow{pid, ppid: id.ppid, uid_is_ours: id.uid = our_uid, name: id.comm, bundle: None, (cpu_ns, footprint_kib)}), where that pair is (None, None) for Refused and (Some(c), Some(f)) for Measured{c, f}.	T::eperm_rows_are_kept_unmeasured	enum match with a struct variant, Option of a struct with String, Default, cross-crate ProcRow	0,1,3,12
cpu-ticks-wrapping-accumulator	invariant	CpuTicks::advance	∀ sequences x_1..x_n of [u32;4] (user, system, idle, nice) fed to CpuTicks::default(): let d_1 = x_1 widened, and for j > 1 let d_j[i] = (x_j[i] wrapping− x_{j−1}[i]) as u64. Let b_j = d_j[0]+d_j[1]+d_j[3], B_n = Σ_sat b_j and T_n = Σ_sat (b_j + d_j[2]). Then the n-th advance returns (B_n, T_n). Hence across successive calls (two-state) busy and total never decrease and busy ≤ total, even when a 32-bit counter wraps.	T::cpu_ticks_accumulate_across_a_wrapped_counter	&mut self two-state, [u32;4] array map and a for loop, wrapping_sub, saturating_add, Option<[u32;4]> field	0,1,2,3,5,7,13
mac-kernel-word-tables	table-property	pressure_of, used_of_level, thermal_of, c_text	∀ level: i64: pressure_of(level) = {1 ↦ Normal, 2 ↦ Warn, 4 ↦ Critical, otherwise None}; used_of_level(level) = Some(1000 − 10·level) when 0 ≤ level ≤ 100, otherwise None, so the result ∈ 0..=1000. ∀ state: isize: thermal_of(state) = {0 ↦ Nominal, 1 ↦ Fair, 2 ↦ Serious, 3 ↦ Critical, otherwise None}. ∀ bytes: c_text(bytes) = from_utf8_lossy(bytes[..k]), where k is the index of the first 0 byte, or bytes.len() if there is none.	T::kernel_levels_map_to_the_engine_words	literal match into cross-crate enums, u16::try_from; c_text needs a &[u8] position closure and String	0,1,3,12,13
rows-to-name-selection	contract	rows_to_name	∀ rows, prev: let C = TopK(i ↦ DeltaCpu(rows[i],prev)) and M = TopK(i ↦ Fp(rows[i])). Let F = the indices not in C or M with DeltaCpu ≥ NAME_FLOOR_CPU_NS or Fp ≥ NAME_FLOOR_KIB, stably sorted by DeltaCpu descending. Then rows_to_name(rows,prev) = C ++ [m ∈ M, in order, m ∉ C] ++ F. Consequences: the indices are distinct and all < rows.len(); every listed row has DeltaCpu > 0 or Fp > 0, so an unmeasured or idle row is never named; every row past either floor is listed.	T::only_the_leaders_by_cpu_and_by_footprint_get_a_path, T::every_helper_past_a_floor_is_named_not_only_the_leaders (selection part)	&[ProcRow], &HashMap<u32,u64>, Vec<usize> filter/sort_by_key(Reverse)/truncate/contains/extend, closures	0,1,3,5,7,11,12,13
fill-cached-paths	contract	fill_cached_paths	∀ rows, cache, ∀ index i, with r = old rows[i] and r' = new rows[i]: if r.bundle is Some, r' = r. Else if cache.get(r.pid) = Some((comm, path)) and comm = r.name, r' = r with bundle := path.clone(). Else r' = r. No other field changes, and the length is unchanged.	(secondary only: T::every_helper_past_a_floor_is_named_not_only_the_leaders, cache part)	&mut [ProcRow] two-state over a slice, HashMap get, String compare, Option<String> clone_from, let-chains	0,1,2,5,7,11,12
used-mib-of	contract	used_mib_of	∀ internal, purgeable, wired, compressor, page: used_mib_of(...) = None if page = 0. Otherwise, with pages = (internal sat- purgeable) + wired + compressor, it is u32::try_from((pages sat* page) >> 20).ok(). requires (no panic): the two plain additions do not overflow u64 (§5). The result is not capped at physical memory.	T::memory_in_use_is_counted_from_the_vm_pages	u64 scalar arithmetic, saturating ops, unchecked add, u32::try_from; seal 150efe4a: E6 refuses (Option<u32> return, saturating calls, Assert)	0,1,3
available-mib-of	contract	available_mib_of	∀ free, inactive, purgeable, page, physical: available_mib_of(...) = None if page = 0. Otherwise it is Some(min(((free sat+ inactive sat+ purgeable) sat* page) >> 20, physical)), where the min applies only when physical is Some. Corollary: the result never exceeds physical when physical is known.	T::memory_available_is_free_inactive_and_purgeable_pages	u64 scalar arithmetic, Option<u64> map_or with a closure	0,1,3,13
swap-meter-rate	refinement	SwapMeter::new, SwapMeter::observe	State (base: Option<(Instant,u64)>, rate: Option<u32>), with new() = (None, None). observe(at, kib): kib = None resets to (None, None) and returns None. With no base, it sets base = (at, kib) and returns None. Otherwise let span = at sat- base_at. If span > SWAP_SPAN_MAX, the state becomes (Some((at,kib)), None) and observe returns None. If span < SWAP_SPAN_MIN, the state is unchanged and observe returns rate. Otherwise r = min(u32::MAX, ((kib sat- base_kib) sat* 1000) div max(1, span_ms)), the state becomes (Some((at,kib)), Some(r)), and observe returns Some(r). Corollaries: a counter that went backwards gives rate 0; a second reader within SWAP_SPAN_MIN repeats the last rate; a rate is never taken over a span longer than SWAP_SPAN_MAX.	T::the_swap_meter_reads_a_rate_over_a_span_of_its_readings	&mut self two-state, Option<(Instant,u64)>, Duration comparison/as_millis (u128), try_from	0,1,2,3,12
mac-host-reading-sane	other	Probe::reading, macos::Mac::reading, macos::Mac::cpu, macos::Mac::vm_info	Effects(Mach, sysctl, NSProcessInfo). For two successive readings a, b of one Probe: b.at > a.at; when both busy_ticks are Some, busy and total never decrease and Δbusy ≤ Δtotal (from cpu-ticks-wrapping-accumulator); mem_used_pm ∈ 0..=1000 (from used_of_level); mem_used_mib = used_mib_of(vm counters, page_kib·1024); swap_pages = swapins sat+ swapouts, and it is non-decreasing; psi = None; page_kib = vm.pagesize/1024. The live assertions (cores > 0, mem ≥ 1 GiB, page ∈ {4, 16}, the counter advances) are facts about the host, not about the code.	T::the_probe_reads_cpu_memory_and_thermal_on_this_host	FFI host_statistics/sysctlbyname/objc msgSend, unsafe, &mut self, OnceLock	0,2,4,8,12
process-cwd-path-own	other	process_cwd, process_path, macos::process_cwd, macos::process_path	Effects(proc_pidinfo/proc_pidpath, or /proc/<pid>/{cwd,exe}). ∀ pid: process_cwd(pid) = Some(p) ⇒ p is absolute and equals the OS's current-directory path for pid. process_path(pid) = Some(p) ⇒ p is absolute and equals the OS's executable path for pid (truncated to PATH_CAP bytes on macOS). process_cwd(0) = process_path(0) = None, and both are None for pid > i32::MAX (on macOS pid is filtered to 0 < pid ≤ i32::MAX).	T::a_processs_working_directory_and_executable_are_its_own	FFI, unsafe zeroed structs, OsStr from bytes, PathBuf, fs::read_link	0,1,8
group-memory-members	other	group_memory, macos::group_memory	Effects(libproc). ∀ pid: group_memory(pid) = None if pid = 0, if pid > i32::MAX, if the short info is unreadable, if pgid = 0, or if no member was measured. Otherwise it is Some{pgid = pbsi_pgid(pid), measured = the number of the first min(n, MAX_GROUP_PIDS) listed group members > 0 whose rusage is Measured, footprint_mib = (sat-sum of their footprint_kib)/1024}. So a child that took its own process group is never counted in its parent's group. On non-macOS platforms it is always None.	T::a_groups_memory_is_its_own_members_footprint	FFI proc_listpids into a fixed array, unsafe, loop with a saturating sum	0,7,8
machine-memory-within-physical	other	physical_memory_mib, machine_memory, memory_pressure, macos::machine_memory, macos::physical_memory_mib	Effects(sysctl and HOST_VM_INFO64, or /proc/meminfo). machine_memory() = Some(m) ∧ physical_memory_mib() = Some(p) ⇒ m.available_mib ≤ p, and on Linux also m.used_mib ≤ p. On macOS used_mib ≤ p holds only empirically, because used_mib_of is not capped (§5). Pure cores: macOS uses available_mib_of(free, inactive, purgeable, page, physical) and used_mib_of(internal, purgeable, wire, compressor, page); Linux uses available = min(avail/1024, total/1024) and used = (total sat- avail)/1024 over meminfo-and-shares' (total, avail). physical_memory_mib is (hw.memsize >> 20).filter(> 0) on macOS and (total_kib ≥ 1024).then(total_kib/1024) on Linux. memory_pressure is the sysctl level through pressure_of.	T::the_machines_memory_and_pressure_are_read_on_this_host	FFI, OnceLock, composition of pure callees	0,4,8
swapped-kib-matches-reading	other	swapped_kib, macos::swapped_kib, Probe::reading	Effects(HOST_VM_INFO64, or /proc/vmstat). swapped_kib() = (swapins sat+ swapouts) sat* page_kib, over the same page size and counter that Reading::swap_pages and Reading::page_kib read. The counter is monotone, so for a swapped_kib() read k followed by a reading r, k ≤ r.swap_pages · r.page_kib. On Linux the pure core is swap_pages(vmstat) sat* page_kib.	T::the_swap_counter_reads_on_this_host_as_the_strain_rows_count	FFI, OnceLock, composition	0,4,8
```

### Kinds

| kind | count | ids |
|---|---|---|
| contract | 18 | cpu-ticks-aggregate, psi-avg10-permille, psi-of-projection, meminfo-and-shares, swap-pages-sum, pid-stat-last-paren, scaled-ticks-saturating, proc-row-composition, linux-reading-from-texts, stat-cores-whole-block, status-effective-uid, auxv-lookup, read-bounded-cap, row-of-total, rows-to-name-selection, fill-cached-paths, used-mib-of, available-mib-of |
| table-property | 2 | zone-thermal-table, mac-kernel-word-tables |
| invariant | 2 | whole-lines-torn-tail-ignored, cpu-ticks-wrapping-accumulator |
| refinement | 4 | linux-sampler-facts, linux-sampler-reading-refines, sweep-rows-and-path-cache, swap-meter-rate |
| other (effectful host facts) | 5 | mac-host-reading-sane, process-cwd-path-own, group-memory-members, machine-memory-within-physical, swapped-kib-matches-reading |

Total: 18 + 2 + 2 + 4 + 5 = 31.

### Gap histogram (number of the 31 specs each gap blocks)

| gap | specs blocked |
|---|---|
| 0 | 31 |
| 1 | 25 |
| 2 | 8 |
| 3 | 18 |
| 4 | 10 |
| 5 | 13 |
| 7 | 16 |
| 8 | 9 |
| 11 | 4 |
| 12 | 11 |
| 13 | 14 |

Gaps 6, 9, 10, 14, 15, 16 and 17 are not listed per spec. Gap 6 (plain-Rust clause publication) and
gap 14 (Clean elaborator coverage) apply to every spec, depending on which lane discharges it, and gap 16
(soundness) applies to every proof.

## 2. Test → spec map (TSV; all 30)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples. A
non-spec test's primary is `-` (§4).

```tsv
file	test	primary	also
src/tests.rs	proc_stat_reads_the_aggregate_line_and_skips_guest	cpu-ticks-aggregate	whole-lines-torn-tail-ignored
src/tests.rs	pressure_files_read_avg10_as_permille	psi-avg10-permille	psi-of-projection
src/tests.rs	meminfo_gives_size_and_the_used_share	meminfo-and-shares	
src/tests.rs	vmstat_sums_swap_in_and_out	swap-pages-sum	
src/tests.rs	thermal_trips_map_to_states	zone-thermal-table	
src/tests.rs	pid_stat_takes_the_comm_to_the_last_paren	pid-stat-last-paren	scaled-ticks-saturating, proc-row-composition
src/tests.rs	a_linux_reading_assembles_from_the_texts	linux-reading-from-texts	
src/tests.rs	the_linux_sampler_reads_the_fixed_facts_under_its_root	linux-sampler-facts	
src/tests.rs	the_linux_sampler_reads_a_whole_reading_from_the_fixture_tree	linux-sampler-reading-refines	
src/tests.rs	a_linux_sweep_reads_every_pid_with_its_uid	sweep-rows-and-path-cache	
src/tests.rs	the_auxiliary_vector_gives_the_clock_tick_and_the_page_size	auxv-lookup	linux-sampler-facts, linux-sampler-reading-refines, sweep-rows-and-path-cache
src/tests.rs	the_core_count_reads_only_a_whole_cpu_block	stat-cores-whole-block	status-effective-uid
src/tests.rs	rusage_time_is_scaled_by_the_timebase	scaled-ticks-saturating	
src/tests.rs	every_proc_read_is_bounded_to_4_kib	read-bounded-cap	whole-lines-torn-tail-ignored, cpu-ticks-aggregate
src/tests.rs	eperm_rows_are_kept_unmeasured	row-of-total	
src/tests.rs	cpu_ticks_accumulate_across_a_wrapped_counter	cpu-ticks-wrapping-accumulator	
src/tests.rs	kernel_levels_map_to_the_engine_words	mac-kernel-word-tables	
src/tests.rs	only_the_leaders_by_cpu_and_by_footprint_get_a_path	rows-to-name-selection	
src/tests.rs	every_helper_past_a_floor_is_named_not_only_the_leaders	rows-to-name-selection	fill-cached-paths
src/tests.rs	memory_in_use_is_counted_from_the_vm_pages	used-mib-of	
src/tests.rs	memory_available_is_free_inactive_and_purgeable_pages	available-mib-of	
src/tests.rs	the_services_noun_is_the_platforms	-	
src/tests.rs	the_probe_can_move_to_its_thread	-	
src/tests.rs	the_probe_reads_cpu_memory_and_thermal_on_this_host	mac-host-reading-sane	sweep-rows-and-path-cache, process-cwd-path-own, cpu-ticks-wrapping-accumulator
src/tests.rs	a_reading_and_a_sweep_cost_milliseconds_not_more	-	
src/tests.rs	a_processs_working_directory_and_executable_are_its_own	process-cwd-path-own	
src/tests.rs	a_groups_memory_is_its_own_members_footprint	group-memory-members	
src/tests.rs	the_machines_memory_and_pressure_are_read_on_this_host	machine-memory-within-physical	available-mib-of, used-mib-of, mac-kernel-word-tables
src/tests.rs	the_swap_counter_reads_on_this_host_as_the_strain_rows_count	swapped-kib-matches-reading	
src/tests.rs	the_swap_meter_reads_a_rate_over_a_span_of_its_readings	swap-meter-rate	
```

## 3. Coverage

- 30 tests read. 27 map to one of the 31 specs, and 3 are not spec samples (§4).
- Four specs have no primary test of their own (`psi-of-projection`, `proc-row-composition`,
  `status-effective-uid`, `fill-cached-paths`). Each is sampled as a secondary part of a test whose
  primary spec is elsewhere. That test is deleted only when all of its specs are proved.
- Only `rows-to-name-selection` collapses two tests. Every other spec has one primary test. The value of
  this crate's migration is the generalisation: each test samples one or two inputs of a total function
  over all inputs.
- Seven tests (the 5 `macos` ones, the `unix` one and the `macos`/`linux` one) read the live host. Their
  specs are `Effects(...)` rows. They retire only with a platform model (gap 8). Until then, the
  pure-core halves can be proved and the host test can be cut down to the effectful part.

## 4. Tests that are not spec samples

| test | why | action |
|---|---|---|
| `the_probe_can_move_to_its_thread` | The type system already guarantees `Probe: Send`, and the test only restates it. | **delete**, and put `const _: () = { fn f<T: Send>() {} let _ = f::<Probe>; };` beside the type so the check stays without a runtime test |
| `the_services_noun_is_the_platforms` | It restates the definition of the `cfg!`-selected `SERVICES_NOUN` const, and pins prose that `aterm-messages` owns. There is no behaviour to specify. | **delete** |
| `a_reading_and_a_sweep_cost_milliseconds_not_more` | A latency ceiling (median read < 50 ms, scan < 250 ms). It samples no functional spec, and AGENTS.md forbids latency budgets in tests. | **move to bench** (`aterm-bench`), quoted as percentiles |

Actions: delete 2, move-to-bench 1.

## 5. Implied specs that look wrong, or disagree with the code

1. **Unchecked arithmetic next to saturating neighbours.** These block an unconditional no-panic spec.
   Each needs a `requires` clause or a switch to saturating ops; the switch is the general fix.
   - `used_mib_of` (lib.rs): `internal.saturating_sub(purgeable) + wired + compressor` uses plain `+`,
     while every neighbour saturates.
   - `linux::cpu_ticks`: busy and total use plain `u64 +`, so adversarial `/proc/stat` values overflow and
     panic in debug.
   - `linux::percent_to_pm`: `whole * 100` overflows `u32` for avg10 whole parts > 42,949,672. PSI never
     produces such values.
   - `linux::MemInfo::used_pm`: `avail * 1000` overflows `u64` for avail > 1.8e16 KiB. This is
     unreachable on real hardware.
2. **`used_mib_of` is not capped at physical memory**, unlike `available_mib_of`. So the claim of
   `the_machines_memory_and_pressure_are_read_on_this_host` that used ≤ physical is only empirical on
   macOS (`machine-memory-within-physical`).
3. **`linux::auxv_value` disagrees with its doc.** The doc says the vector is "ended by AT_NULL", but the
   code scans every full pair, past AT_NULL too. A search for key 0 returns AT_NULL's value, and bytes
   after the terminator can match. `auxv-lookup` states the code's behaviour; either the doc or the code
   should change.
4. **Constant pinning.** `every_proc_read_is_bounded_to_4_kib` also asserts
   `READ_CAP == PATH_CAP == MAX_PIDS == 4096`. No spec stands behind those equalities. The bound worth
   keeping is |read_bounded_bytes| ≤ READ_CAP (`read-bounded-cap`). Drop the equality asserts when the
   spec lands.
5. **Test-only helpers and an ABI assumption.** `the_probe_reads_cpu_memory_and_thermal_on_this_host`
   reaches three `#[cfg(test)]` helpers with no production caller (`macos::sent_encodings`, `own_ident`,
   `own_path`). The `sent_encodings` check (the objc encodings of `thermalState` and
   `isLowPowerModeEnabled`) is an ABI assumption of `heat_and_power` and belongs as a precondition of that
   FFI call. The test also mixes four specs in one body. Its spin-until-moved loop is a hang detector,
   which is allowed.
6. **Another crate's spec sampled here.** The last assertion of
   `every_helper_past_a_floor_is_named_not_only_the_leaders` exercises `aterm_messages::strain::group`
   (Culprit::App grouping). Move that sample to aterm-messages' spec recovery. Its
   `const { assert!(MAX_PATH_READS >= 2 * NAMED_ROWS) }` is a compile-time fact that belongs beside the
   constants in lib.rs.
7. **A bare `Command::new` and a host heuristic.** `a_groups_memory_is_its_own_members_footprint` spawns
   `/bin/sleep` with a bare `std::process::Command::new` (allowed by the crate-level test allow on
   `disallowed_types`). AGENTS.md requires `aterm_uds::spawnfd::command`. Its +384 MiB-after-512 MiB
   margin is a host-load heuristic, not a spec bound.
8. **Host heuristic.** In `the_swap_counter_reads_on_this_host_as_the_strain_rows_count`, the bound
   "< 1 GiB swapped between two reads" is a heuristic. Only the monotone part is a spec.
9. **An arm that may never run.** In `the_auxiliary_vector_gives_the_clock_tick_and_the_page_size`, the
   refused-pid branch runs only when `chmod 000` actually denies the open. It is silently skipped as root,
   so the PermissionDenied arm of `Sampler::row` may never be exercised. The test builds its tree under
   `std::env::temp_dir()` by pid instead of `aterm_tempfile`, and leaves it behind on panic.
10. **Unsampled cases.** `thermal_trips_map_to_states` never samples the `hot` trip kind, which
    `zone_thermal` treats as Critical, or a negative trip temperature (excluded by a > 0).
    `zone-thermal-table` covers both.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone. The two scalar specs need only gaps 0 and 3 (casts, shifts, std
min/max in E6 bodies), so they come first.

1. **`scaled-ticks-saturating`** (gaps 0, 3). Exact `u128` arithmetic, one branch, no loops. It retires
   `rusage_time_is_scaled_by_the_timebase` and is the CPU-time core of both platform sweeps.
2. **`used-mib-of`** (gaps 0, 3). Scalar `u64` saturating arithmetic and a `try_from`. Proving it forces
   the fix of §5.1 (saturating adds or a `requires`). It retires `memory_in_use_is_counted_from_the_vm_pages`
   and is a pure core of `machine-memory-within-physical` and `mac-host-reading-sane`.
3. **`available-mib-of`** (gaps 0, 1, 3, 13: one `Option` and a `map_or` closure) and the integer half of
   **`mac-kernel-word-tables`** (`pressure_of`, `used_of_level`, `thermal_of`; gaps 0, 3, 12 for the
   cross-crate enums). Both are finite case splits or scalar arithmetic. `c_text` waits for gap 1.
4. **`swap-meter-rate`** and **`cpu-ticks-wrapping-accumulator`**, once gap 2 (two-state `&mut self`)
   lands. They are small state machines with scalar fields and retire one test each. The accumulator's
   monotonicity is the lemma `mac-host-reading-sane` cites.
5. **`row-of-total`** and **`zone-thermal-table`** (enum and slice-of-tuple carriers, gap 1).
6. The `/proc` text parsers, once gaps 1, 5 and 7 land: first a shared `WL`/`Key` lemma, then
   `whole-lines-torn-tail-ignored`, `cpu-ticks-aggregate`, `psi-avg10-permille`, `meminfo-and-shares`,
   `swap-pages-sum`, `status-effective-uid`, `stat-cores-whole-block`, `pid-stat-last-paren` and
   `auxv-lookup`. These retire 9 tests.
7. The compositions, once gap 4 lands: `psi-of-projection`, `proc-row-composition`,
   `linux-reading-from-texts`. Then `rows-to-name-selection` and `fill-cached-paths` (gaps 11, 12, 13).
8. The `Effects` rows, last, with a platform model (gap 8): `read-bounded-cap` (its pure loop core can be
   proved at step 4), `linux-sampler-facts`, `linux-sampler-reading-refines`,
   `sweep-rows-and-path-cache`, and the five host facts.

Deletions that need no proof can land now: the two `delete` rows of §4, and the move of the latency test
to `aterm-bench`.
