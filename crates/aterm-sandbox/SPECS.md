<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-sandbox: spec recovery from its 16 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the row naming the 1 test it
> deleted (`apply_actually_sets_the_limit`) is gone, and the spec rows no longer cite that test.
> Counts elsewhere in this file were taken before.

> Status 2026-10-16: this file is the crate's migration worklist. Each of the 7 specs below is
> proved through Trust/Clean about the compiled Rust function. Then the tests it subsumes are
> deleted. No test has been deleted yet.

Source: `crates/aterm-sandbox` (read only). I read every `#[test]`
(`src/lib.rs` 8, one of them `cfg(windows)`; `src/windows.rs` 8, all `cfg(windows)`, for 16) and the code
they reach: `src/unix.rs` (`apply_limits`, `set_limit`), `src/windows.rs` (`apply_limits`, `apply_to_job`,
`apply_ui_restrictions`), and `aterm_cap::require` / `Cap::satisfies`. Nothing was compiled or run.
The claims below come from reading the code.

Result: **7 specs**, covering **16 tests**. **0 tests are non-spec** (§4).
Gap numbers are the rows of `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.

## 0. Notation used in the statements

- `Gate(cap)` := `aterm_cap::require(cap, Tier::Trusted)`, which is `cap.satisfies(Tier::Trusted)`
  (tier ≥ Trusted in the `Tier` order).
- `Fields(l)` := the four POSIX pairs, in order:
  `[(RLIMIT_CPU, l.cpu_seconds), (RLIMIT_AS, l.address_space), (RLIMIT_FSIZE, l.file_size),
  (RLIMIT_NOFILE, l.open_files)]`. `active_processes` and `restrict_ui` have no effect on POSIX.
- `RL`: an abstract OS rlimit state, `resource -> (soft, hard)`. `getrlimit`/`setrlimit` are modelled
  as a platform spec (gap 8): `setrlimit(r,(s,h))` succeeds only if `s ≤ h` and
  (`h ≤ old hard` or the caller is privileged). Otherwise it leaves `RL` unchanged. An unprivileged
  caller can never raise `hard`.
- `Eff(call)`: the ordered trace of syscalls a call issues (`getrlimit`/`setrlimit`, or
  `QueryInformationJobObject`/`SetInformationJobObject`). "No effect" means `Eff = []`.
- `JobInfo`: the `repr(C)` `JobObjectExtendedLimitInformation`.
- `Fold(I, l)` := `I` with these changes:
  - `ProcessMemoryLimit := usize::try_from(as).unwrap_or(usize::MAX)` if `l.address_space = Some(as)`;
  - `PerProcessUserTimeLimit := i64::try_from(s).unwrap_or(i64::MAX).saturating_mul(10_000_000)` if
    `l.cpu_seconds = Some(s)`;
  - `ActiveProcessLimit := m` if `l.active_processes = Some(m)`;
  - `LimitFlags := I.LimitFlags | (MEM if as set) | (TIME if cpu set) | (ACTIVE if active set)`;
  - every other field is unchanged.

  **Recommendation:** factor `Fold` out of `windows::apply_to_job` as a pure function. That gives
  the effect lane a pure core to wrap, and `sandbox-job-fold` can then be proved before gap 8 closes.
- `HARDENED_UI` := `0x02|0x04|0x08|0x10|0x20|0x40|0x80` = `0xFE`.
- Per-target: specs about `cfg` arms are proved once per arm (unix with macOS / non-macOS, and windows).
  The Windows specs need a Windows-target Trust build.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking_gaps` (audit §7). Gap 0, the installed
seal that publishes no Lean-discharged row, blocks every row, so it is listed on each one.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
sandbox-presets	contract	Limits::shell_default, Limits::contained, Limits::inherit	shell_default() == Limits{cpu_seconds:None, address_space: if target_os=macos {None} else {Some(17179869184)}, file_size:None, open_files:Some(8192), active_processes:Some(512), restrict_ui:true, binding:false}; contained() == Limits{binding:true, ..shell_default()}; inherit() == Limits::default() == every Option None, restrict_ui:false, binding:false. Proved once per cfg arm.	lib.rs::shell_default_omits_unsupported_address_space_on_macos	EASY: struct literal and functional-update syntax returning a Copy struct; Option<u64>/Option<u32> fields; cfg!(target_os) compile-time constant; constant 16*1024^3 (overflow check folded); derived Default and PartialEq	0,1,6
sandbox-windows-apply-noop	contract	windows::apply_limits, Limits::apply	On Windows, ∀ l: imp::apply_limits(l) == Ok(()) with Eff = []. Hence ∀ cap with Gate(cap): Limits::apply(l, cap) == Ok(()) and installs nothing.	lib.rs::windows_apply_is_a_capgated_noop (noop half)	TRIVIAL body returning Ok(()); io::Result<()> carrier; cfg(windows) build; composition with sandbox-apply-cap-gate	0,1,4
sandbox-job-fold	contract	windows::apply_to_job (pure core, to be factored as Fold)	∀ I: JobInfo, l: Limits: the struct apply_to_job passes to SetInformationJobObject(JobObjectExtendedLimitInformation) is Fold(I, l), where I is what the query returned. In particular: Fold(I,l).LimitFlags & I.LimitFlags == I.LimitFlags (pre-existing flags such as KILL_ON_JOB_CLOSE are kept); the MEM/TIME/ACTIVE bit is newly set iff the matching field is Some; with every field None, Fold(I,l) == I; PerProcessUserTimeLimit never wraps (it saturates at i64::MAX); ProcessMemoryLimit saturates at usize::MAX.	windows.rs::apply_to_job_installs_memory_and_cpu_limits, windows.rs::apply_to_job_preserves_preexisting_limit_flags, windows.rs::apply_to_job_installs_active_process_cap, windows.rs::apply_to_job_with_no_limits_leaves_flags_untouched	MEDIUM once factored: nested repr(C) struct field updates through &mut; bitwise OR on u32; u64->usize/i64 try_from with unwrap_or; i64::saturating_mul; if-let on Option fields. Unfactored, it lives inside an unsafe FFI body (then also gap 8)	0,1,2,3
sandbox-apply-cap-gate	contract	Limits::apply, Limits::apply_to_job, aterm_cap::require	∀ l: Limits, cap: Cap<Sandbox> (and on Windows every job handle): if ¬Gate(cap), then apply(l,cap) (and apply_to_job(l,cap,job)) returns Err(e) with e.kind() == PermissionDenied and Eff = []. If Gate(cap), the result and Eff equal those of imp::apply_limits(l) (resp. imp::apply_to_job(l,job)).	lib.rs::apply_requires_a_trusted_capability, lib.rs::windows_apply_is_a_capgated_noop (gate half)	MEDIUM: &self struct with Option<u64> fields; Cap<E> phantom-typed newtype with a Tier enum and its order; io::Error built by map_err and e.to_string() (String allocation on the error path); ? early return; an extern-crate callee (aterm_cap::require) whose ensures must be composed; cfg(unix)/cfg(windows) module selection; "no effect" needs an effect model	0,1,4,6,8,12
sandbox-set-limit	contract	unix::set_limit	∀ r, v, b, RL: set_limit(r, None, b) returns Ok(()) with Eff = []. set_limit(r, Some(v), b) issues getrlimit(r) and then exactly one setrlimit(r, lim), where H = RL[r].hard if getrlimit succeeds, else v; soft = min(v, H); lim = (soft, if b {soft} else {H}). It returns Ok(()) iff that setrlimit returned 0, else Err(last_os_error). Corollaries under the RL model, with getrlimit succeeding and an unprivileged caller: after Ok, RL[r] = (min(v,H), H) when ¬b (the hard ceiling is kept, so the soft limit can later be raised to any s ≤ H), and RL[r] = (min(v,H), min(v,H)) when b (so any later setrlimit that raises soft or hard above min(v,H) fails).	lib.rs::apply_preserves_the_hard_ceiling_so_a_shell_can_raise_its_soft_limit, lib.rs::a_contained_limit_cannot_be_raised_back	HARD: unsafe FFI (libc::getrlimit/setrlimit) with an &mut out-param struct; Option let-else; core::cmp::min; u64 -> rlim_t cast; bool-selected field; io::Error::last_os_error; needs a PlatformSpec for rlimit semantics (privilege, hard-limit monotonicity) and the post-fork no-allocation context	0,1,3,8
sandbox-apply-limits-best-effort	contract	unix::apply_limits, Limits::apply	∀ l: Limits: apply_limits(l) calls set_limit(r_i, v_i, l.binding) for each (r_i, v_i) in Fields(l), in order and unconditionally (an Err from one call never skips a later one). It returns Ok(()) iff every call returned Ok, otherwise Err(e_k), where k is the first index whose call failed. It reads neither l.active_processes nor l.restrict_ui. Corollary with sandbox-set-limit: if every field is None, Eff = [] and RL is unchanged for every resource; each field whose setrlimit succeeds has RL[r_i] set per sandbox-set-limit, whatever happens on the other fields.	lib.rs::none_limits_are_a_no_op, lib.rs::unsupported_limit_does_not_block_the_working_ones	HARD: for loop over a fixed-size array of tuples (loop invariant: first_err = the first error in the processed prefix); Option<io::Error> with get_or_insert (&mut, a non-Copy heap-owning error); match on Option; composition of set_limit's contract	0,1,2,4,7,8
sandbox-job-write-protocol	contract	windows::apply_to_job, windows::apply_ui_restrictions, Limits::apply_to_job	∀ l, job, and Job Object platform state, Eff(apply_to_job(l,job)) is: Query(job, EXTENDED). If that fails, return Err(last_os_error) and issue no Set. Otherwise Set(job, EXTENDED, Fold(I,l)). If that fails, return Err and make no UI write. Otherwise, iff l.restrict_ui, Set(job, BASIC_UI, {UIRestrictionsClass: HARDENED_UI}) and return its error or Ok; else return Ok(()) and leave the job's UI restrictions untouched. Under the platform model, after Ok the job's UI class is HARDENED_UI when restrict_ui, and unchanged (0 on a fresh job) otherwise. Composed with sandbox-presets: apply_to_job(shell_default()) sets ACTIVE (512), MEM (16 GiB) and HARDENED_UI.	windows.rs::apply_to_job_installs_ui_restrictions_when_requested, windows.rs::apply_to_job_leaves_ui_unrestricted_by_default, windows.rs::shell_default_hardens_the_job, windows.rs::apply_to_job_fails_closed_on_invalid_handle	HARD: unsafe extern "system" kernel32 FFI with raw pointers (addr_of_mut, cast to c_void), mem::zeroed, size_of with u32::try_from, RawHandle; ? propagation; io::Error::last_os_error; needs a Windows Job Object PlatformSpec and an FFI assumption channel; cfg(windows) only, so a Windows-target Trust build	0,1,4,8
```

### Distribution by kind (7)

| kind | count | ids |
|---|---|---|
| contract | 7 | every spec above |

### Gap histogram (specs blocked by each gap)

| gap | specs |
|---|---|
| 0 | 7 |
| 1 | 7 |
| 2 | 2 |
| 3 | 2 |
| 4 | 4 |
| 6 | 2 |
| 7 | 1 |
| 8 | 4 |
| 12 | 1 |

### Proof dependency order

`sandbox-presets`, `sandbox-job-fold` (after factoring), `sandbox-set-limit` → `sandbox-apply-limits-best-effort`
(composes `set-limit`) → `sandbox-apply-cap-gate` (composes `aterm_cap::require` and both `imp` arms) →
`sandbox-windows-apply-noop` (composes the gate) and `sandbox-job-write-protocol` (composes `job-fold`
and `presets`).

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/lib.rs	apply_requires_a_trusted_capability	sandbox-apply-cap-gate	
src/lib.rs	windows_apply_is_a_capgated_noop	sandbox-windows-apply-noop	sandbox-apply-cap-gate
src/lib.rs	none_limits_are_a_no_op	sandbox-apply-limits-best-effort	sandbox-set-limit, sandbox-presets (inherit)
src/lib.rs	unsupported_limit_does_not_block_the_working_ones	sandbox-apply-limits-best-effort	sandbox-set-limit
src/lib.rs	shell_default_omits_unsupported_address_space_on_macos	sandbox-presets	
src/lib.rs	apply_preserves_the_hard_ceiling_so_a_shell_can_raise_its_soft_limit	sandbox-set-limit	sandbox-apply-limits-best-effort
src/lib.rs	a_contained_limit_cannot_be_raised_back	sandbox-set-limit	sandbox-presets (contained), sandbox-apply-limits-best-effort
src/windows.rs	apply_to_job_installs_memory_and_cpu_limits	sandbox-job-fold	sandbox-job-write-protocol
src/windows.rs	apply_to_job_preserves_preexisting_limit_flags	sandbox-job-fold	sandbox-job-write-protocol
src/windows.rs	apply_to_job_installs_active_process_cap	sandbox-job-fold	sandbox-job-write-protocol
src/windows.rs	apply_to_job_with_no_limits_leaves_flags_untouched	sandbox-job-fold	sandbox-job-write-protocol
src/windows.rs	apply_to_job_installs_ui_restrictions_when_requested	sandbox-job-write-protocol	
src/windows.rs	apply_to_job_leaves_ui_unrestricted_by_default	sandbox-job-write-protocol	sandbox-presets (inherit)
src/windows.rs	shell_default_hardens_the_job	sandbox-job-write-protocol	sandbox-presets, sandbox-job-fold
src/windows.rs	apply_to_job_fails_closed_on_invalid_handle	sandbox-job-write-protocol	sandbox-apply-cap-gate
```

## 3. Coverage

- 16 original tests read. All 16 map to one of the 7 specs. None is a non-spec test (§4).
- 9 of the 16 tests are `cfg(windows)` (8 in `windows.rs`, plus `lib.rs::windows_apply_is_a_capgated_noop`),
  so a macOS dev run never executes them. Their specs need a Windows-target Trust build to be proved.
- The biggest collapses are `sandbox-job-fold` (4 primary), `sandbox-job-write-protocol` (4) and
  `sandbox-set-limit` (3).

## 4. Tests that are not spec samples

None. Every test samples one of the specs above, and none is a pure regression of a removed bug with
no general property. The tests in §5.1 are weak samples of their specs, so they are deleted once the spec
is proved, not kept as they are.

## 5. Implied specs that look wrong, or disagree with the code

1. **Weak or mislabelled tests**
   - `windows.rs::apply_to_job_fails_closed_on_invalid_handle` asserts only
     `err.kind() != ErrorKind::Other`. A null-handle `ERROR_INVALID_HANDLE` decodes to `Uncategorized` in
     std, so the assertion holds for almost any OS error and pins nothing. The real property (a failed
     query returns Err and issues no Set) is in `sandbox-job-write-protocol`.
   - `windows.rs::shell_default_hardens_the_job` says it checks "memory, cpu-adjacent caps, the
     active-process bound, AND the UI restrictions", but it asserts only `ACTIVE_PROCESS` and the UI
     class. `shell_default` sets `cpu_seconds: None`, so no CPU cap applies, and the memory flag goes
     unchecked.
   - `windows.rs::apply_to_job_installs_ui_restrictions_when_requested` compares `query_ui()` against the
     `HARDENED_UI_RESTRICTIONS` constant from the same module, so a wrong bitset in the constant passes.
     The spec pins the value `0xFE`.
   - `lib.rs::none_limits_are_a_no_op` reads back only `RLIMIT_CPU`, so an apply that changed AS, FSIZE or
     NOFILE on all-None limits would pass. `sandbox-apply-limits-best-effort` states `Eff = []` for every
     resource.
   - `lib.rs::apply_preserves_the_hard_ceiling_so_a_shell_can_raise_its_soft_limit` ends with
     `current >= applied`, not `current == min(applied*4, hard)`. A raise that silently failed and left
     256 in place still passes.
   - `lib.rs::unsupported_limit_does_not_block_the_working_ones` asserts soft NOFILE `== 256` exactly.
     `set_limit` installs `min(v, hard)`, so the test holds only while the inherited hard limit is
     ≥ 256. It does not check that precondition (the hard-ceiling test does check its own).
   - `lib.rs::a_contained_limit_cannot_be_raised_back`: off macOS, `contained()` also requests a binding
     16 GiB `RLIMIT_AS`. If the host's AS hard limit is lower, `apply` returns Err and the forked child
     returns `NOT_INSTALLED`, so an environment precondition reads as a containment failure. The spec
     (`sandbox-set-limit` with `sandbox-presets`) states the clamp and the per-field result instead.
   - `lib.rs::shell_default_omits_unsupported_address_space_on_macos`: off macOS it asserts only
     `address_space.is_some()`, not the 16 GiB value. `sandbox-presets` pins `Some(17179869184)`.
2. **The model is stronger than the code.** `Sandbox.tla`'s `AllSupportedApplied` (cited in the
   `Limits::apply` SPEC comment in `lib.rs`) says every supported requested limit is installed. But
   `set_limit` clamps to the inherited hard ceiling, installing `min(v, H)`, not `v`. Restate the model
   with the clamp, as `sandbox-set-limit` does.
3. **The non-binding promise fails on one branch.** When `getrlimit` fails, `set_limit` takes `H = v` and
   sets `hard = v` even when `binding` is false. So "a non-binding limit keeps the hard ceiling" does not
   hold on that branch. Either change the code on that branch (for example, skip the call), or keep the
   branch in the spec, as `sandbox-set-limit` does now.
4. **The refinement anchor points at nothing.** The `#[refines]` attribute on `Limits::apply` names
   `project = "aterm_sandbox::Sandbox::project_apply"`. No such function exists anywhere in `crates/`
   (`Sandbox` is an empty enum). The anchor works only because Trust does not run the projection. Add the
   projection, with a conformance test that has a negative control, or drop the anchor.
5. **Stale manifest text (not a test).** `crates/aterm-sandbox/Cargo.toml` describes the Windows actuator
   as "a documented, capability-gated no-op (src/windows.rs) with zero FFI". `windows.rs` binds kernel32
   `Query/SetInformationJobObject` for `apply_to_job`, so the text is out of date. Only `apply_limits` is a
   no-op on Windows.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone. Every one also needs gap 1 (struct/`Option` carriers). The order
below goes by how few gaps beyond 0 and 1 each spec needs, weighted by the tests it retires.

1. **`sandbox-presets`** (gaps 0, 1, 6). Three constant constructors with no loops, calls or effects;
   it is decidable by evaluation once struct carriers and plain-Rust publication land. It retires one
   test and is the lemma that `sandbox-job-write-protocol`'s `shell_default` corollary and the
   `contained` half of `sandbox-set-limit` cite.
2. **`sandbox-job-fold`** (gaps 0, 1, 2, 3), after the `Fold` factoring in §0. It is pure field updates,
   bitwise OR and saturating casts, with no FFI. It retires 4 Windows tests and is the data core of
   `sandbox-job-write-protocol`.
3. **`sandbox-windows-apply-noop`** (gaps 0, 1, 4). The body is `Ok(())`; the only work is composing
   the cap gate. It retires one test.
4. **`sandbox-apply-cap-gate`** (gaps 0, 1, 4, 6, 8, 12). The denial half needs only the
   `aterm_cap::require` contract across crates and an empty-effect claim. It retires 1 test outright and
   shares one more.
5. **`sandbox-set-limit`** (gaps 0, 1, 3, 8). This is the high-value POSIX spec (3 tests, and the
   containment guarantee itself). It waits on the rlimit PlatformSpec.
6. **`sandbox-apply-limits-best-effort`** (gaps 0, 1, 2, 4, 7, 8). It composes `set-limit` over a
   4-element loop and retires 2 tests.
7. **`sandbox-job-write-protocol`** (gaps 0, 1, 4, 8). It needs a Windows Job Object PlatformSpec, an FFI
   assumption channel and a Windows-target Trust build. It retires 4 tests.
