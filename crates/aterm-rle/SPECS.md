<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-rle: spec recovery from its 58 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 46 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-15: this file is the crate's migration worklist. Each of the 14 specs below is
> proved through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted. The 2 tests in §4 sample no spec; delete them now. No test has been deleted yet.

Source: `crates/aterm-rle` (`src/lib.rs`, `src/mutations.rs`; read only). I read every `#[test]`
(`src/tests.rs` 13, `src/tests_boundary.rs` 13, `src/tests_exhaustive.rs` 18,
`src/tests_saturation.rs` 9, `src/tests_performance.rs` 3, `tests/fuzz_rle.rs` 2 = 58) and the
code each one reaches. Nothing was compiled or run. The claims below come from reading the code.

Result: **14 specs**, covering **56 tests**. **2 tests are not spec samples** (§4).

## 0. Notation used in the statements

- `MAX` = `u32::MAX`. Every statement is over all `T: Copy + PartialEq + Default`. The tests use
  `T = u8` or `u16`, so each spec is one per-definition generic proof (gap 11), or one proof per
  instantiation.
- `expand(rs: [Run<T>]) : Seq T` = `concat_k replicate(rs[k].length, rs[k].value)`.
  `model(r)` = `expand(r.runs)`. This is the abstraction function. Every functional spec below is a
  statement about `model(r)`.
- `encode(xs: Seq T) : [Run<T>]` = the canonical RLE of `xs`: one run per maximal block of equal
  adjacent values, with `length` = the block size. `encode([]) = []`.
- `Inv(r)` := `r.runs == encode(model(r))` ∧ `r.total_length as u64 == Σ_k r.runs[k].length as u64 ≤ MAX`.
  Equivalently: every run has `length > 0`, adjacent runs have different values, and the cached total
  equals the exact (u64) sum. The run-count assertions in the tests (`run_count() == n`,
  `runs()[k].length == m`) all follow from `Inv` plus the model contract of the operation.
- `Reach`: the `Rle` values reachable from `new()`, `default()`, `with_value(v,n)` and `from_iter(xs)`
  through any sequence of `push`, `try_push`, `extend_with`, `try_extend_with`, `set`, `set_range`,
  `resize`, `resize_with` and `clear`.
- `len(r)` = `r.len()` = `r.total_length`. `start(r,k)` = `Σ_{j<k} r.runs[j].length` (the first cell
  index of run `k`).
- `clampadd(r,c)` = `min(c, MAX - len(r))`.
- Sequence operations: `xs ++ ys`, `replicate(n,v)`, `xs[i:=v]`, `take(n,xs)`, and `resize(xs,n,v)`
  (`Vec::resize` semantics: truncate to `n` or pad with `v`).
- Iterator state of `RleIter{runs, run_idx, offset_in_run}`: `rem(it)` = `expand(runs)` with its first
  `start(run_idx) + offset_in_run` cells dropped. `IterInv(it)`: `run_idx ≤ runs.len` ∧
  `(run_idx < runs.len ⇒ offset_in_run < runs[run_idx].length)` ∧ `(run_idx = runs.len ⇒ offset_in_run = 0)`.
- `iters(call)`: the number of `count_run_iteration()` calls a call makes. Today that counter is a
  `#[cfg(test)]` `thread_local` `Cell` in `src/lib.rs`, called from `find_run_linear`,
  `split_range_multi_run` and `compact`. A proof needs it as a ghost counter.
- Gap numbers refer to the table in
  the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.
  Gap 0 (a seal that publishes Clean rows) blocks every row, so it is listed on each one.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps (audit §7)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
rle-inv	invariant	Rle::{new,default,with_value,from_iter,from_iter_inner,push,try_push,extend_with,try_extend_with,extend_with_unclamped,set,set_range,resize,resize_with,clear}; internal: truncate, split_and_set, split_range_single_run, split_range_multi_run, replace_run_span, compact, compact_around	Inv(r) ∀ r∈Reach. The constructors establish it: Inv(new()), Inv(default()), ∀ v,n Inv(with_value(v,n)), ∀ xs Inv(from_iter(xs)). Each public mutator preserves it: Inv(r) ⇒ Inv(r') for all arguments. Hence every run has length>0, adjacent runs differ, total_length = Σ length (in u64) ≤ MAX, r.runs = encode(model(r)), and run_count(r) is the number of maximal equal-value blocks of model(r). The helpers carry local preconditions: split_and_set requires offset < runs[run_idx].length; split_range_single_run requires start_offset < end_offset ≤ runs[run_idx].length; split_range_multi_run and replace_run_span require start_run_idx < end_run_idx < runs.len and 1 ≤ end_offset ≤ runs[end_run_idx].length; compact_around requires idx < runs.len and runs canonical except at idx. Under those preconditions each turns a run vector that is canonical except around the edited span into a canonical one with the same expand.	tests::rle_run_structure_consistent_across_mutations, tests_exhaustive::post_clamp_mutations_preserve_invariant	HARD but central: a type invariant over the private pair (runs, total_length); Vec<Run<T>> contents (index, insert, remove, truncate, last_mut, push, clear); loop invariants in compact, the replace_run_span tail shift and truncate; PartialEq calls on generic T; u32/usize saturating arithmetic; Option<Run<T>> arguments	0,1,2,3,5,7,11
rle-find-run	contract	Rle::{find_run,find_run_linear,checked_run_length_sum}	∀ r with Inv(r), ∀ index: u32: index < len(r) ⇒ find_run(r,index) = Some((k,o)) for the unique k with start(r,k) ≤ index < start(r,k+1), and o = index − start(r,k); so o < runs[k].length and model(r)[index] = runs[k].value. index ≥ len(r) ⇒ None. Loop invariant: accumulated = start(r,i) ≤ index, and checked_run_length_sum(accumulated, run.length) returns the exact sum (it never saturates, because start(r,i+1) ≤ total_length ≤ MAX).	-	MEDIUM: a for loop over iter().enumerate() with an accumulator invariant and early return, Option<(usize,u32)> result, saturating_add/saturating_sub, &self	0,1,2,3,5,7,11
rle-ctor	contract	Rle::new, <Rle as Default>::default, Rle::with_value, <Rle as FromIterator>::from_iter, Rle::from_iter_inner, <Run as Default>::default	model(new()) = [] with 0 runs; default() = new(). ∀ v,n: model(with_value(v,n)) = replicate(n,v), so n = 0 gives 0 runs and n > 0 exactly one run {v,n}. ∀ finite xs: model(from_iter(xs)) = take(MAX, xs) (elements past MAX are dropped silently, because push is a no-op at capacity). Run::<T>::default() = Run{value: T::default(), length: 0}.	-	EASY for new/default/with_value (Vec::with_capacity + push, no loop). MEDIUM for from_iter: a for loop over a generic IntoIterator (Iterator::next trait calls on a type parameter, the documented L0 gap in from_iter_inner); T::default() trait call	0,1,3,7,11
rle-observers	contract	Rle::{len,is_empty,run_count,runs,remaining_capacity}	∀ r with Inv(r): len(r) = |model(r)| as u32; is_empty(r) ⇔ model(r) = []; run_count(r) = r.runs.len() = |encode(model(r))|; runs(r) is the run vector as a slice; remaining_capacity(r) = MAX − len(r), and the subtraction cannot underflow (for any u32 total_length).	-	TRIVIAL: &self field reads, a &[Run<T>] slice return, one u32 subtraction. The model halves need rle-inv.	0,1,2,11
rle-extend-clamped	refinement	Rle::{push,extend_with,extend_with_unclamped,checked_run_length_sum}	∀ r with Inv(r), v, count: after extend_with(v,count): model(r') = model(r) ++ replicate(clampadd(r,count), v) and len(r') = len(r) + clampadd(r,count) ≤ MAX. push(v) = extend_with(v,1), so at capacity push is a no-op on both the same-value merge path and the new-run path. The run vector changes only at its tail: unchanged when the clamped count is 0; the last run grows when its value = v; otherwise exactly one run {v, clamped count} is appended. Every checked_run_length_sum call here returns the exact sum.	-	EASY-MEDIUM: &mut self, Vec::last_mut (Option<&mut Run<T>>) in a let-chain, Vec::push, u32::min, saturating_add, PartialEq on T; no loop	0,1,2,3,4,11
rle-try-extend	contract	Rle::{try_extend_with,try_push}	∀ r with Inv(r), v, count: count ≤ MAX − len(r) ⇒ try_extend_with(v,count) = Ok(()) and r' is what extend_with(v,count) gives (model(r') = model(r) ++ replicate(count,v)). Otherwise it returns Err(RleCapacityError{requested: count, available: MAX − len(r)}) and r' = r (runs and total_length unchanged; nothing partly inserted), on both the merge and the new-run path. try_push(v) = try_extend_with(v,1).	-	EASY: &mut self, a Result<(), struct> return, a two-state frame condition (r' = r on Err), composition with extend_with_unclamped's contract	0,1,2,4,11
rle-get-index	refinement	Rle::get, <Rle as Index<u32>>::index	∀ r with Inv(r), index: get(r,index) = model(r).get(index): Some(model(r)[index]) when index < len(r), None otherwise (including index = len and index = MAX). r[index] returns a reference to a value equal to model(r)[index] when index < len(r), and panics (slice bounds check on runs[runs.len()]) exactly when index ≥ len(r).	tests_exhaustive::rle_index_trait_valid_access, tests_exhaustive::rle_index_trait_oob_panics	EASY given rle-find-run: Option<T> return, an Index trait impl returning &T, Option::unwrap_or, a panic-iff clause on a slice index	0,1,2,3,4,11
rle-set	refinement	Rle::set; internal: split_and_set, compact_around	∀ r with Inv(r), index, v: set returns (index < len(r)). index < len(r) ⇒ model(r') = model(r)[index := v]; otherwise r' = r. len(r') = len(r) in both cases. With rle-inv: a set to the run's own value leaves the runs unchanged, a middle split gives 3 runs, a start or end split gives 2, and a set that matches both neighbours merges them into 1.	tests::rle_set_single_element, tests_exhaustive::rle_set_correctness_exhaustive	MEDIUM: &mut self, Vec::insert/remove/index-assign, a let-else on Option, a case split on the run offset, composition (find_run, split_and_set, compact_around), a sequence update in the model	0,1,2,3,4,5,11
rle-set-range	refinement	Rle::set_range; internal: split_range_single_run, split_range_multi_run, replace_run_span, compact	∀ r with Inv(r), start, end, v: let e = min(end, len(r)). After set_range(start,end,v): len(r') = len(r) and ∀ j < len(r): model(r')[j] = (start ≤ j < e ? v : model(r)[j]). start ≥ end or start ≥ len(r) ⇒ r' = r (covers empty ranges, start > end, an empty Rle, and end clipped past len). Run counts follow from rle-inv (a range matching both neighbours merges into one run; the whole-sequence fast path leaves one run). replace_run_span, with start_run_idx ≤ end_run_idx < runs.len, yields runs[..start_run_idx] ++ [before?, mid, after?] ++ runs[end_run_idx+1..] (the Vec::splice equivalence its doc claims). In split_range_multi_run the range_len loop computes exactly e − start.	tests::rle_set_range_cases	HARD: &mut self, Vec content edits (index-assign, insert, truncate, clear, push), a for loop over a RangeInclusive, a while-loop tail shift, an in-place compaction loop with read/write cursors, Option<Run<T>> parameters, composition through 5 levels, a universally quantified pointwise postcondition	0,1,2,3,4,5,7,11
rle-resize	refinement	Rle::{resize,resize_with,clear}; internal: truncate	∀ r with Inv(r), n, v: model(resize_with(r,n,v)) = resize(model(r), n, v), and resize(n) = resize_with(n, T::default()). On shrink the value is ignored; n = len is a no-op; n = 0 empties the Rle (0 runs). After clear: model = [] with 0 runs. truncate(n) with 0 < n < len(r): model = take(n, model(r)), keeps exactly runs[..=k] where k is the run holding cell n−1, and shortens run k to n − start(r,k) > 0. Growth goes through extend_with and is exact because n ≤ MAX.	-	MEDIUM: &mut self, an iter_mut().enumerate() loop with an accumulator invariant and early return, Vec::truncate/clear, T::default(), composition with extend_with	0,1,2,3,4,7,11
rle-iter	refinement	Rle::iter, <&Rle as IntoIterator>::into_iter, <RleIter as Iterator>::{next,size_hint}, <RleIter as ExactSizeIterator>::len	∀ r with Inv(r): IterInv(iter(r)) ∧ rem(iter(r)) = model(r). ∀ it with IterInv(it) over runs with no zero-length run: rem(it) = x::xs ⇒ next(it) = Some(x), rem(it') = xs, IterInv(it'); rem(it) = [] ⇒ next(it) = None and it' = it (fused). Collecting gives model(r). size_hint(it) = (|rem(it)|, Some(|rem(it)|)), so len(it) = |rem(it)| at every point, 0 after exhaustion. (&r).into_iter() = r.iter().	tests_exhaustive::rle_exact_size_iter_after_partial_iteration	MEDIUM: a struct holding a borrowed &'a [Run<T>], &mut self iterator state, slice::get, an iter().skip() loop in size_hint, u32→usize casts, saturating ops, Iterator/ExactSizeIterator impls, an iterator-protocol spec	0,1,2,3,5,7,11
rle-no-panic	no-panic	Rle::{push,try_push,extend_with,try_extend_with,get,set,set_range,resize,resize_with,clear,iter,run_count,is_empty,checked_run_length_sum}, <RleIter as Iterator>::{next,size_hint}; internal: split_and_set, split_range_single_run, split_range_multi_run, replace_run_span, compact, compact_around, truncate	∀ r with Inv(r) and all arguments (any u32 index/start/end/count/new_length, any T): none of these panics: no slice or Vec index out of bounds, no u32/usize overflow, no out-of-range Vec::insert/remove. Every saturating_add/saturating_sub in lib.rs and mutations.rs equals its exact result on these paths, so each saturating spelling is the identity on reachable states. Index is the one panicking public entry (specified in rle-get-index).	fuzz_rle::fuzz_rle_mutators_never_panic	MEDIUM given rle-inv: bounds and overflow obligations under a type invariant carried across method boundaries (the code comments name exactly this as what the verifier cannot carry today), loops, Vec indexing	0,1,2,3,4,7,11
rle-capacity-error-display	contract	<RleCapacityError as Display>::fmt	∀ requested, available: u32: RleCapacityError{requested, available}.to_string() = "RLE capacity exceeded: requested " ++ dec(requested) ++ " but only " ++ dec(available) ++ " available", with dec the u32 Display decimal. If the formatter does not error, fmt returns Ok(()).	-	MEDIUM: &mut fmt::Formatter, Write::write_str, the u32 Display call, String content, ? on fmt::Result. Not generic.	0,1,2,3,4,8
rle-find-run-cost	other (cost: ghost counter)	Rle::{find_run_linear,get,set}; internal: split_and_set	Over the ghost counter iters: ∀ r with Inv(r), index < len(r) with find_run(r,index) = Some((k,_)): find_run_linear makes exactly k+1 counted iterations; for index ≥ len it makes exactly run_count(r) (get and set return before calling it in that case). set(index,v) makes no counted iterations beyond its one find_run, because split_and_set and compact_around have no loops. So set on a one-run Rle costs exactly 1 counted iteration whatever len(r) is.	tests_performance::rle_set_well_compressed_is_constant	MEDIUM: a ghost counter in a loop (replacing the cfg(test) thread_local Cell), loop-iteration accounting, &self	0,1,3,5,7,11
```

### Distribution by kind (14)

| kind | count | ids |
|---|---|---|
| contract | 5 | rle-find-run, rle-ctor, rle-observers, rle-try-extend, rle-capacity-error-display |
| refinement | 6 | rle-extend-clamped, rle-get-index, rle-set, rle-set-range, rle-resize, rle-iter |
| invariant | 1 | rle-inv |
| no-panic | 1 | rle-no-panic |
| other | 1 | rle-find-run-cost (cost over a ghost counter) |

### Gap histogram (number of specs each gap blocks)

| gap | specs blocked |
|---|---|
| 0 | 14 |
| 1 | 14 |
| 2 | 12 (all but rle-ctor, rle-find-run-cost) |
| 3 | 12 (all but rle-observers, rle-try-extend) |
| 4 | 8 |
| 5 | 6 |
| 7 | 8 |
| 8 | 1 (rle-capacity-error-display) |
| 11 | 13 (all but rle-capacity-error-display) |

No spec is blocked by gap 0 alone. Every spec needs gap 1 (`Vec<Run<T>>` content models or a
struct/String carrier). Gap 11 (generics) can be sidestepped for a first proof by stating each spec
at `T = u8`, which is what the tests use, and generalising once per-definition proofs land.

### Proof dependency order

`rle-observers` → `rle-ctor` (new/default/with_value half) → `rle-extend-clamped` → `rle-try-extend`
→ `rle-find-run` → `rle-get-index` → `rle-set` → `rle-find-run-cost` → `rle-resize` → `rle-iter`
→ `rle-ctor` (from_iter half) → `rle-set-range` → `rle-inv` (assembled from the per-operation
preservation lemmas proved alongside each contract) → `rle-no-panic`. `rle-capacity-error-display`
is independent.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
The tests deleted since have no row.

```tsv
file	test	primary	also
src/tests.rs	rle_set_single_element	rle-set	
src/tests.rs	rle_set_range_cases	rle-set-range	
src/tests.rs	local_compaction_observations_refine_the_derived_model	rle-set-range	rle-inv
src/tests.rs	rle_run_structure_consistent_across_mutations	rle-inv	rle-find-run, rle-set, rle-set-range, rle-resize
src/tests.rs	rle_const_helpers_invariants	rle-observers	rle-extend-clamped, rle-try-extend, rle-set, rle-set-range
src/tests_exhaustive.rs	rle_index_trait_valid_access	rle-get-index	
src/tests_exhaustive.rs	rle_index_trait_oob_panics	rle-get-index	
src/tests_exhaustive.rs	rle_exact_size_iter_after_partial_iteration	rle-iter	
src/tests_exhaustive.rs	rle_set_range_exhaustive	rle-set-range	rle-inv
src/tests_saturation.rs	extend_and_push_clamp_to_the_remaining_capacity	rle-extend-clamped	rle-observers, rle-inv
src/tests_saturation.rs	try_forms_take_a_whole_request_or_change_nothing	rle-try-extend	rle-inv
src/tests_saturation.rs	post_clamp_mutations_preserve_invariant	rle-inv	rle-set, rle-set-range, rle-resize, rle-no-panic
src/tests_exhaustive.rs	rle_set_correctness_exhaustive	rle-set	rle-inv
src/tests_performance.rs	rle_set_well_compressed_is_constant	rle-find-run-cost	
src/tests_performance.rs	rle_set_range_linear_in_runs	rle-find-run-cost	
tests/fuzz_rle.rs	fuzz_rle_mutators_never_panic	rle-no-panic	rle-inv
```

## 3. Coverage

- 58 tests read. 56 map to one of the 14 specs. 2 are not spec samples (§4).
- The biggest collapses: `rle-set-range` (11 primary), `rle-ctor`, `rle-extend-clamped`,
  `rle-try-extend` and `rle-resize` (6 each).
- `rle-inv` has only 3 primary tests, but it is sampled by 16 tests and is the precondition of every
  other functional spec. The fuzz harness's `assert_invariant` and the per-test structure checks
  retire with it.

## 4. Tests that are not spec samples

> Status 2026-10-15: `rle_set_individual_cost_scales_with_run_count` deleted.
>
> Status 2026-10-28: `tests_performance::rle_set_range_linear_in_runs` was rewritten (§5.2) to assert exact `count_run_iteration()` counts rather than a growth-rate ratio band, so 0 non-spec tests remain.

Once `rle-find-run-cost` is proved over a ghost counter and its tests are deleted, the
`#[cfg(test)]` `iteration_counter` module in `src/lib.rs` (and the `count_run_iteration()` calls in
`src/mutations.rs`) can go too.

## 5. Implied specs that look wrong, or disagree with the code

1. *(Resolved)* **`tests/fuzz_rle.rs::assert_invariant` now checks the full `Inv(r)` invariant**
   (`run.length > 0`, `w[0].value != w[1].value`, `u64` sum `<= u32::MAX`, `rle.len() == sum`, and
   `remaining_capacity == u32::MAX - len`).
2. *(Resolved)* **`tests_performance` module doc and `rle_set_range_linear_in_runs` now check exact
   `count_run_iteration()` counts** (`1` on a uniform run, `(start + 1) + end + (end - start)` across
   unit-length runs via `compact_around`) rather than ratio bands.
3. *(Resolved)* **`src/lib.rs::checked_run_length_sum` states the `total_length <= u32::MAX` invariant
   directly** without citing a test file.
4. *(Resolved)* **`tests_exhaustive::rle_index_trait_oob_panics` uses bare `#[should_panic]`** without
   pinning `core::slice`'s panic message text.

## 6. The specs I would prove first (easiest, high value)

No spec here is blocked by gap 0 alone; all need at least gap 1 (a `Vec<Run<T>>` content model) and,
unless stated at `T = u8`, gap 11.

1. **`rle-observers`** (gaps 0, 1, 2, 11). Field reads and one `u32` subtraction that cannot
   underflow, with no loop and no call. It retires 1 test, but `len`/`run_count`/`remaining_capacity`
   are the vocabulary every other spec is stated in, so it is the first `&self`-view proof to land.
2. **`rle-try-extend`** with **`rle-extend-clamped`** (gaps 0, 1, 2, 4, 11; `extend-clamped` adds 3
   for `u32::min`/`saturating_add`). No loops: `last_mut`, one compare, one push or one length bump.
   Together they retire 12 tests and give the tail-only frame condition that `rle-ctor`'s
   `with_value` and `rle-resize`'s growth path reuse.
3. **`rle-find-run`** then **`rle-get-index`** (gaps add 3, 5, 7). One accumulator loop with an
   obvious invariant (`accumulated = start(r,i) ≤ index`). It is the lemma `rle-set`, `rle-set-range`
   and `rle-find-run-cost` stand on, and it retires 5 tests directly (4 via `rle-get-index`).

Runner-up: `rle-ctor`'s `new`/`default`/`with_value` half (gaps 0, 1, 3, 11, no loop); its `from_iter`
half waits on gap 7 (a loop over a generic iterator).
