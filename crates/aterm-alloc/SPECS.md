<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-alloc: spec recovery from its 57 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 33 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-15: this is the crate's migration worklist. Each of the 18 specs below is
> proved through Trust/Clean about the compiled Rust function, and then the tests it
> subsumes are deleted. The 2 tests in §4 that sample no spec can be deleted now. No test
> has been deleted yet. The crate's floor is 17 of 105 rows
> (`crates/aterm-release/trust/floors.tsv`), and no proved row today covers any of the 55
> spec-sample tests.

Source: `crates/aterm-alloc` (read only). I read every `#[test]`
(`src/array_vec.rs` 8, `src/small_vec.rs` 27, `tests/arrayvec_differential.rs` 12,
`tests/panic_drop_parity.rs` 10 = 57), `src/lib.rs` (`drop_initialized`), and every function
those tests reach. Nothing was compiled or run. The claims below come from reading the code.

Result: **18 specs**, covering **55 tests**. **2 tests are not spec samples** (§4).

Plan: `docs/PLAN-tests-to-specs-2026-10-15.md` (historical development plan).
The gap numbers in the `blocking_gaps` column are the rows of §7 of
the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md`.
Format exemplar: [crates/aterm-parser/SPECS.md](../aterm-parser/SPECS.md).

## 0. Notation used in the statements

- `Seq(v) : List T` is the abstract contents of an `ArrayVec<T,N>` or `SmallVec<[T;N]>`.
  For `ArrayVec` it is `buf[0..len)` read as `T`. For `SmallVec` it is `buf[0..len)` for
  `Inline{buf,len}` and `vec` for `Heap(vec)`. `as_slice` and `Deref` return exactly `Seq(v)`.
- `Inv_AV(v)`: `len ≤ N`, `buf[0..len)` is initialized and owned by `v`, and `buf[len..N)`
  is not owned. `Inv_SV(v)`: either `Inline{buf,len}` with `len ≤ N` and the same prefix
  ownership, or `Heap(vec)`. Every spec assumes `Inv` on entry and proves it on exit,
  **including every unwinding exit**. Today every body that touches `MaybeUninit` is
  `#[cfg_attr(trust_verify, trust::skip)]`, so `Inv` is an audited comment, not a proved fact.
- `BV_N`: a `Vec` bounded to capacity `N` under upstream arrayvec's contract (the `model` in
  `tests/arrayvec_differential.rs`). `push`, `insert`, `extend` and `collect` panic when full.
  `try_push` and `try_extend_from_slice` refuse. An overflowing `extend` keeps the prefix it
  already wrote.
- `Panics(op)`: `op` unwinds. Where a spec says so, it also gives the post-state on the
  unwinding exit.
- `Ledger`: the sequence of element identities whose destructor ran. `ExactlyOnce(run)` holds
  when every element that was moved into a container and never moved out by value appears
  exactly once in the Ledger once the container, iterator or `Drain` is gone, and no element
  appears twice. "Moved out by value" means `pop`, `remove`, `next`/`next_back`, `into_inner`,
  a yielded drain element, or a `try_push` `Err`. The new owner drops an element that was
  moved out, never the container. `ExactlyOnce` also holds when one element destructor panics:
  the bulk drop continues through slice drop glue and the panic then propagates. A second
  panic aborts the process and is out of scope.
- `RangeRes(len, R) = (s, e)`: start `Unbounded → 0`, `Included(i) → i`,
  `Excluded(i) → i sat+ 1`; end `Unbounded → len`, `Included(j) → j sat+ 1`, `Excluded(j) → j`.
- Gap `U` (proposed new §7 row): **a model of unwinding and destructors**. It covers panic
  edges as outcomes with a post-state, `Drop` glue, `drop_in_place`, and the Ledger. The audit
  has no such row, and 8 specs here need it. It probably belongs beside gap 8 (effects) and
  proven-core's SemR, because it is a semantics question and not a carrier question.
- Gap 0 (the installed seal publishes no Clean-discharged row) is listed on every spec, because
  the audit makes it a prerequisite for every row. Gap 11 (generics) is also on every spec:
  both containers are generic in `T` and const-generic in `N`, and a proof at one instance
  would be a sample, not the spec.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary ones are
in §2), `rust_features` (what the proof must handle), `blocking_gaps` (audit §7 numbers, plus `U`).

```tsv
id	kind	functions	statement	subsumes	rust_features	blocking_gaps
av-accessors	contract	ArrayVec::{new,new_const,default,len,is_empty,capacity,remaining_capacity,is_full}	∀ N: Seq(new()) = Seq(new_const()) = Seq(default()) = [], and each satisfies Inv_AV. ∀ v with Inv_AV(v): len(v) = |Seq v|; is_empty(v) ⇔ |Seq v| = 0; capacity(v) = N; remaining_capacity(v) = N − |Seq v| (the saturation never fires under Inv); is_full(v) ⇔ |Seq v| = N.	-	&self scalar reads of a struct with a const-generic array field; const fn; saturating_sub	0,1,2,3,11
av-push	contract	ArrayVec::{push,try_push,set_single}, CapacityError::element	∀ v, x with Inv_AV(v). If |Seq v| < N: push(v,x) and try_push(v,x) give Seq v' = Seq v ++ [x], and try_push returns Ok. If |Seq v| = N: push panics with a message containing "ArrayVec overflow", and Seq v is unchanged on the unwinding exit; try_push returns Err(e) with e.element() = x, Seq v unchanged and x not dropped. set_single(v,x): Seq v' = [x] when N ≥ 1, the old elements are dropped exactly once, and it panics only when N = 0.	-	&mut self two-state post; MaybeUninit slot write; const generic N; panic as a contract outcome; generic T	0,1,2,11,U
av-refines-bounded-vec	refinement	ArrayVec::{push,try_push,pop,insert,remove,truncate,clear,retain,extend_from_slice,try_extend_from_slice,drain,as_mut_slice}, Extend::extend, FromIterator::from_iter, Clone::clone	∀ N, ∀ finite op sequence from {push, try_push, pop, insert(i), remove(i), truncate(n), clear, retain(f), extend_from_slice(s), try_extend_from_slice(s), extend(it), drain(r) fully consumed, a mutation through DerefMut such as sort}, starting from new(): after every step Seq(ArrayVec) = the contents of BV_N; the observable results are equal (popped and removed values, the Ok/Err polarity of try_*, try_push's returned element, yielded drain elements); and Panics(op) on ArrayVec ⇔ Panics(op) on BV_N. This master simulation is the conjunction of the per-op contracts: pop returns last(Seq) and removes it, or None on []. insert(i,x) panics ⇔ i > len ∨ len = N (no message is part of the spec) and otherwise gives Seq[..i] ++ [x] ++ Seq[i..]. remove(i) panics ⇔ i ≥ len and otherwise returns Seq[i] with Seq' = Seq[..i] ++ Seq[i+1..]. truncate(n) gives Seq' = take n Seq and drops each cut element once. clear = truncate 0. try_extend_from_slice(s) is all-or-nothing: Err ⇔ |s| > N − len, and Seq is unchanged on Err.	tests/arrayvec_differential.rs::scripted_operation_sequence, tests/arrayvec_differential.rs::lcg_operation_fuzz	&mut self two-state; unsafe ptr::copy/read/write shifts; MaybeUninit; while loop (retain); for loops (extend); FnMut closures; RangeBounds dispatch; T: Clone; panics; const generics; every body #[trust::skip] today	0,1,2,4,5,7,8,11,13
av-extend-overflow	contract	Extend::extend for ArrayVec, FromIterator::from_iter for ArrayVec, ArrayVec::{extend_from_slice,try_extend_from_slice}	∀ v with Inv_AV(v), ∀ finite xs. If |Seq v| + |xs| ≤ N: extend(v,xs) and extend_from_slice(v,xs) give Seq v' = Seq v ++ xs without panicking. Otherwise both panic ("ArrayVec overflow"), and on the unwinding exit Seq v' = Seq v ++ take (N − |Seq v|) xs: the prefix that fit is kept and nothing is truncated silently. from_iter(xs) panics ⇔ |xs| > N and otherwise gives Seq = xs. try_extend_from_slice: Err ⇔ |xs| > N − |Seq v|, nothing is appended on Err, and Seq v' = Seq v ++ xs on Ok.	tests/arrayvec_differential.rs::overflow_policy_parity	for loop over a caller-chosen IntoIterator; T: Clone; post-state on the unwinding exit; &mut self	0,1,2,5,7,11,13,U
av-into-inner	contract	ArrayVec::{into_inner,into_inner_unchecked}	∀ v with Inv_AV(v): into_inner(v) = Ok(arr) ⇔ |Seq v| = N. Then arr read as a list = Seq v, and no element is dropped during the call (ownership moves to arr, so ExactlyOnce holds once arr is dropped). Otherwise the result is Err(v) with Seq unchanged. into_inner_unchecked requires len = N and has the same result.	src/array_vec.rs::test_into_inner_moves_without_double_drop, tests/arrayvec_differential.rs::into_inner_polarity_parity	by-value self; ManuallyDrop; unsafe ptr::read of [MaybeUninit<T>;N] as [T;N]; Result<_, Self>; Ledger	0,1,8,11
av-drain	contract	ArrayVec::drain, Drain::{next,next_back,size_hint}, Drop for Drain	∀ v with Inv_AV(v), ∀ R: RangeBounds<usize>, with (s,e) = RangeRes(|Seq v|, R): drain(v,R) panics ⇔ s > e ∨ e > |Seq v|, and Seq v is then unchanged. Otherwise let D = Seq v[s..e]. For any interleaving of next and next_back, the front calls yield D's prefix in order, the back calls yield D's suffix in reverse order, and no element is yielded twice. size_hint = (r, Some r), where r is the number not yet yielded. When the Drain ends (exhausted, dropped early, or dropped untouched): Seq v' = Seq v[..s] ++ Seq v[e..], every element of D not yielded is dropped exactly once by the Drain, and the yielded elements belong to the caller. If the Drain is forgotten (mem::forget), v still satisfies Inv_AV and holds Seq v[..s] ++ (D minus the yielded ones) ++ Seq v[e..].	src/array_vec.rs::test_drain_double_ended, src/array_vec.rs::test_drain_past_len_panics, tests/arrayvec_differential.rs::drain_parity_over_every_range_shape	struct holding &mut ArrayVec across calls; RangeBounds + Bound match; saturating_add; Iterator/DoubleEndedIterator; Drop impl; slice rotate_left; remove's unsafe shift; Ledger	0,1,2,3,5,8,11,13,U
av-drain-unwind	contract	Drop for Drain, ArrayVec::truncate, drop_initialized	∀ v, (s,e) valid, any number of elements already yielded, and a panicking destructor in the un-yielded part of D: dropping the Drain unwinds, and on the unwinding exit Seq v' = Seq v[..s] ++ Seq v[e..] (the whole range is removed and the tail is closed up), Inv_AV(v') holds, and ExactlyOnce holds over the run, including the later drop of v.	tests/panic_drop_parity.rs::array_vec_drain_drop_removes_and_drops_the_range_once_when_a_destructor_panics	Drop glue unwinding; slice rotate; truncate lowering len before drop_in_place on a subslice; unsafe raw slice cast	0,1,2,8,11,U
av-retain	contract	ArrayVec::retain, RetainGuard::drop	∀ v with Inv_AV(v), ∀ f: FnMut(&mut T) -> bool that does not panic: f is called exactly once per element, in index order, on &mut Seq v[i]. Let y_i be the value after f's write and k_i the returned bool. Then Seq v' = [y_i | k_i] in order, so f's writes to kept elements persist. Each rejected element is dropped exactly once during the call, and no kept element is dropped.	(none; the base case of av-retain-unwind)	while loop with a Drop guard holding &mut self; FnMut closure (higher-order spec); MaybeUninit assume_init_read/assume_init_drop; unsafe raw &mut; Ledger	0,1,2,7,8,11,13
av-retain-unwind	contract	ArrayVec::retain, RetainGuard::drop	∀ v with |Seq v| = L, ∀ k < L. (a) If f panics on element k after returning k_i for each i < k: on the unwinding exit Seq v' = [Seq v[i] | i < k ∧ k_i] ++ Seq v[k..L], so the element under examination and the undecided tail are kept in order. (b) If f returns false for k and k's destructor panics: Seq v' = [Seq v[i] | i < k ∧ k_i] ++ Seq v[k+1..L], and k is dropped exactly once. In both cases the drops during the call are exactly the rejected i < k (plus k in case (b)), in index order, which equals std Vec::retain_mut's Ledger. ExactlyOnce holds once v is dropped.	tests/arrayvec_differential.rs::retain_with_a_panicking_predicate_parity, tests/panic_drop_parity.rs::array_vec_retain_drops_each_element_once_when_a_rejected_destructor_panics	panics out of an FnMut and out of a destructor; Drop guard with rotate_left; loop invariant write ≤ read ≤ original_len ≤ N; Ledger order	0,1,2,7,8,11,13,U
av-into-iter	contract	IntoIterator for ArrayVec, IntoIter::{next,next_back,size_hint,as_slice}, Debug/Clone for IntoIter	∀ v with Seq v = xs, it = v.into_iter(): any interleaving of next and next_back yields xs from the front and the back with no repeats, and None once all are yielded. At every point it.as_slice() = the un-yielded middle of xs, size_hint = (|as_slice|, Some |as_slice|), and Debug renders as_slice as a list. Dropping it drops every un-yielded element exactly once, also when one of those destructors panics: the drop unwinds, the rest are still dropped, and none is dropped twice.	tests/arrayvec_differential.rs::into_iter_parity_including_drop_counts, tests/panic_drop_parity.rs::array_vec_into_iter_drop_drops_each_element_once_when_a_destructor_panics	by-value iterator owning an ArrayVec; remove(0)/pop (unsafe shifts); Iterator/DEI/ExactSizeIterator; fmt	0,1,2,8,11,U
av-clone-from	contract	Clone::{clone,clone_from} for ArrayVec	∀ dst, src with Inv_AV: Seq(src.clone()) = Seq src. dst.clone_from(&src) gives Seq dst' = Seq src for every relation between the two lengths: the common prefix is overwritten in place with T::clone_from, dst's surplus is dropped exactly once, and src's extra elements are cloned in.	tests/arrayvec_differential.rs::clone_from_parity	&mut self beside &Self; clone_from_slice; truncate; extend_from_slice loop; user T: Clone	0,1,2,7,11,13
av-slice-view	contract	ArrayVec::{as_slice,as_mut_slice,iter,iter_mut}, Deref/DerefMut, IntoIterator for &ArrayVec and &mut ArrayVec, PartialEq/Debug/Hash for ArrayVec	∀ v with Inv_AV(v): as_slice(v), &*v and the sequence (&v).into_iter() yields all equal Seq v, of length len (never N). as_mut_slice and &mut iteration expose exactly Seq v, and writes through them become Seq v'. A slice method reached through Deref computes that slice function of Seq v (first, last, get, contains, ranges, sort). a == b ⇔ Seq a = Seq b. format!("{v:?}") is the debug_list rendering of Seq v. ∀ H: Hasher, hash(v,H) = hash(Seq v as &[T], H), so it equals BV_N's hash.	tests/arrayvec_differential.rs::by_reference_iteration_parity	unsafe slice::from_raw_parts over MaybeUninit; lifetimes on returned &[T]/&mut [T]; Deref traits; Hasher/Formatter trait objects; generic T	0,1,2,8,11
av-bulk-drop-exactly-once	invariant	ArrayVec::{clear,truncate,remove}, Drop for ArrayVec, drop_initialized	∀ v with Inv_AV(v), ∀ n: truncate(v,n) lowers len to min(n,len) before any destructor runs, then drops Seq v[n..] once each, in index order, with slice drop glue. If one destructor panics, the rest are still dropped, the unwinding exit has Seq v' = take n (Seq v), and the later Drop of v drops only that prefix. clear = truncate 0, so v is empty even on unwind. Drop for ArrayVec = clear. Hence every program over ArrayVec with an owning T (truncate, remove, drain, retain, then drop) satisfies ExactlyOnce, and its Ledger equals BV_N's.	tests/panic_drop_parity.rs::array_vec_clear_drops_each_element_once_when_a_destructor_panics, tests/panic_drop_parity.rs::array_vec_truncate_drops_each_element_once_when_a_destructor_panics, tests/panic_drop_parity.rs::array_vec_drop_drops_each_element_once_when_a_destructor_panics, tests/arrayvec_differential.rs::owning_elements_drop_parity	drop_in_place on a raw slice (unsafe); Drop recursion through clear; ordering of the len store before the destructor calls; Ledger	0,1,2,8,11,U
capacity-error-contract	contract	CapacityError::{new,element,simplify}, Display/Debug for CapacityError	∀ T, x: CapacityError::new(x).element() = x, and new(x).simplify() = CapacityError::new(()). ∀ e: Display writes exactly "insufficient capacity" and Debug writes exactly "CapacityError: insufficient capacity". Neither requires T: Debug or reads the payload. These are upstream arrayvec's strings, kept for compatibility with the patched dependency graph.	src/array_vec.rs::test_capacity_error_shape	generic struct moves; &str constants; fmt::Formatter write_str/write! (needs a Formatter model)	0,1,3,11
sv-seq-contracts	refinement	SmallVec::{new,default,len,is_empty,push,pop,clear,extend_from_slice,from_slice,from_vec,from_elem,as_slice,as_mut_slice}, Clone/PartialEq/Debug/Deref/DerefMut/FromIterator/Extend for SmallVec, IntoIterator for &SmallVec and &mut SmallVec	SmallVec refines an unbounded Vec<T> through Seq. Seq(new()) = []. len = |Seq| and is_empty ⇔ Seq = []. push(x) gives Seq' = Seq ++ [x] and never panics, spilled or not. pop returns last and gives Seq' = init, or None on []. clear gives []. extend_from_slice(s) and extend(xs) append in order. from_slice(s) = s, from_vec(v) = v, from_elem(x,n) = replicate n x, from_iter(xs) = xs. Seq(clone) = Seq. a == b ⇔ Seq a = Seq b. Debug renders Seq as a list. as_slice, Deref and indexing read Seq, and writes through as_mut_slice go to Seq'. Whether the vec is inline or spilled never changes any of these.	-	enum with an inline MaybeUninit array arm and a Vec arm; match on &mut self.data; std Vec model; unsafe from_raw_parts; spill (ensure_heap loop with assume_init_read); for loops; Clone/fmt traits; generic T; const N	0,1,2,3,7,8,11,13
sv-representation	contract	SmallVec::{new,with_capacity,push,from_vec,from_slice,is_inline,spilled,capacity,ensure_heap}	spilled(v) = ¬is_inline(v), and is_inline(v) ⇔ v.data is Inline. new() is Inline with capacity N. with_capacity(c) is Inline with capacity() = N when c ≤ N; otherwise it is Heap with capacity() ≥ min(c, 16_777_216). push keeps the vec Inline while |Seq| < N before the push. A push onto an Inline vec with |Seq| = N moves every element, in order and without dropping any, into a Heap vec and appends there. A Heap vec never returns to Inline. from_vec(v) is Inline ⇔ |v| ≤ N. The spill leaves Seq unchanged.	-	enum discriminant reasoning; Vec::with_capacity capacity postcondition (std model); min; the spill loop moving MaybeUninit slots; &mut self two-state	0,1,2,3,7,8,11
sv-into-iter	contract	IntoIterator for SmallVec, IntoIter::{next,next_back,size_hint}, ExactSizeIterator/FusedIterator for IntoIter	∀ v with Seq v = xs, it = v.into_iter(): any interleaving of next and next_back yields xs from the front and the back with no repeats, then None from both ends forever (fused). size_hint = (r, Some r) and len() = r, where r is the number not yet yielded. Representation: into_iter of an Inline vec builds IntoIterRepr::Inline { start: 0, end: |xs| } and allocates nothing; a Heap vec builds IntoIterRepr::Heap(vec.into_iter()). The representation clause is stated on the private repr, not on the #[cfg(test)] helper IntoIter::is_inline.	src/small_vec.rs::test_into_iter_inline_double_ended	mem::replace of an enum field; by-value MaybeUninit array move; cursor invariant start ≤ end ≤ N; assume_init_read; std vec::IntoIter model; Iterator/DEI traits	0,1,2,8,11
sv-drop-exactly-once	invariant	SmallVec::clear, Drop for SmallVec, Drop for IntoIter, IntoIter::{next,next_back}, drop_initialized	Inline or spilled alike: clear sets the length to 0 before any destructor runs, then drops Seq once each, in order, with slice drop glue. If one destructor panics, the rest are still dropped and v is empty on the unwinding exit. Drop for SmallVec = clear (Vec's drop when spilled). For it = v.into_iter() under any pattern of front and back consumption, yielded elements belong to the caller, and dropping it drops exactly the un-yielded range buf[start..end] once each, also when one destructor panics. So ExactlyOnce holds for every run, and the Ledger's contents do not depend on whether v spilled.	src/small_vec.rs::into_iter_drops_each_element_exactly_once, tests/panic_drop_parity.rs::small_vec_clear_drops_each_element_once_when_a_destructor_panics, tests/panic_drop_parity.rs::small_vec_drop_drops_each_element_once_when_a_destructor_panics, tests/panic_drop_parity.rs::small_vec_into_iter_drop_drops_each_element_once_when_a_destructor_panics	drop_in_place on a raw subslice; Drop impls; std Vec/vec::IntoIter drop semantics as an assumed model; Ledger; enum arms	0,1,2,8,11,U
```

### Distribution by kind (18)

| kind | count | ids |
|---|---|---|
| contract | 14 | av-accessors, av-push, av-extend-overflow, av-into-inner, av-drain, av-drain-unwind, av-retain, av-retain-unwind, av-into-iter, av-clone-from, av-slice-view, capacity-error-contract, sv-representation, sv-into-iter |
| refinement | 2 | av-refines-bounded-vec, sv-seq-contracts |
| invariant | 2 | av-bulk-drop-exactly-once, sv-drop-exactly-once |

### Gap histogram (specs blocked by each gap)

| gap | specs | note |
|---|---|---|
| 0 | 18 | prerequisite for every row |
| 1 | 18 | `Seq` is a List model bound to a struct/enum over `MaybeUninit` |
| 2 | 16 | all except `av-into-inner` (by-value `self`) and `capacity-error-contract` |
| 3 | 5 | `saturating_sub`/`saturating_add`/`min` and enum matches in bodies |
| 4 | 1 | the master simulation composes per-op contracts |
| 5 | 3 | quantified over op sequences or ranges |
| 7 | 7 | `while`/`for` loops (retain, extend, spill, clone_from) |
| 8 | 13 | unsafe block discharge: every `MaybeUninit`/raw-pointer body is `trust::skip` |
| 11 | 18 | generic `T` and const `N` everywhere |
| 13 | 7 | `FnMut` predicates and caller-chosen iterators (higher-order specs) |
| U | 8 | proposed new row: unwind and destructor model |

Gaps 6, 9, 10, 12, 14, 15, 16 and 17 block no spec here directly. Gap 6 matters only if a
spec is routed through SMT instead of Clean, and gap 17 only for running the bridge lane.

### Proof dependency order

`Inv_AV` as a type invariant, with `av-accessors` → `av-push` and `capacity-error-contract`
→ `av-slice-view` (the `Seq` view every later spec reads) → `av-bulk-drop-exactly-once`
(`truncate`/`clear`/`Drop`; `drop_initialized` is shared with SmallVec) → `av-into-inner`,
`av-drain`, `av-retain`, `av-into-iter`, `av-clone-from`, `av-extend-overflow` → the unwind
variants (`av-drain-unwind`, `av-retain-unwind`) → `av-refines-bounded-vec`, the conjunction.
SmallVec: `Inv_SV` → `sv-representation` (the spill preserves `Seq`) → `sv-seq-contracts` →
`sv-into-iter` → `sv-drop-exactly-once`.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
Paths are relative to `crates/aterm-alloc`.

```tsv
file	test	primary	also
src/array_vec.rs	test_into_inner_moves_without_double_drop	av-into-inner	av-bulk-drop-exactly-once
src/array_vec.rs	test_drain_double_ended	av-drain	
src/array_vec.rs	test_drain_forget_keeps_unyielded_elements	av-drain	av-bulk-drop-exactly-once
src/array_vec.rs	test_drain_past_len_panics	av-drain	
src/array_vec.rs	test_capacity_error_shape	capacity-error-contract	av-push
src/small_vec.rs	test_into_iter_inline_double_ended	sv-into-iter	
src/small_vec.rs	into_iter_drops_each_element_exactly_once	sv-drop-exactly-once	sv-into-iter
tests/arrayvec_differential.rs	scripted_operation_sequence	av-refines-bounded-vec	av-push, av-extend-overflow, av-drain, av-retain
tests/arrayvec_differential.rs	lcg_operation_fuzz	av-refines-bounded-vec	av-push, av-extend-overflow (panic polarity only, §5.2)
tests/arrayvec_differential.rs	overflow_policy_parity	av-extend-overflow	av-refines-bounded-vec
tests/arrayvec_differential.rs	into_inner_polarity_parity	av-into-inner	
tests/arrayvec_differential.rs	into_iter_parity_including_drop_counts	av-into-iter	av-bulk-drop-exactly-once
tests/arrayvec_differential.rs	drain_parity_over_every_range_shape	av-drain	av-refines-bounded-vec
tests/arrayvec_differential.rs	clone_from_parity	av-clone-from	
tests/arrayvec_differential.rs	by_reference_iteration_parity	av-slice-view	
tests/arrayvec_differential.rs	owning_elements_drop_parity	av-bulk-drop-exactly-once	av-refines-bounded-vec
tests/arrayvec_differential.rs	retain_with_a_panicking_predicate_parity	av-retain-unwind	av-retain
tests/arrayvec_differential.rs	insert_out_of_bounds_panics_like_model	av-refines-bounded-vec	
tests/panic_drop_parity.rs	array_vec_clear_drops_each_element_once_when_a_destructor_panics	av-bulk-drop-exactly-once	
tests/panic_drop_parity.rs	array_vec_truncate_drops_each_element_once_when_a_destructor_panics	av-bulk-drop-exactly-once	
tests/panic_drop_parity.rs	array_vec_drop_drops_each_element_once_when_a_destructor_panics	av-bulk-drop-exactly-once	
tests/panic_drop_parity.rs	array_vec_into_iter_drop_drops_each_element_once_when_a_destructor_panics	av-into-iter	av-bulk-drop-exactly-once
tests/panic_drop_parity.rs	array_vec_drain_drop_removes_and_drops_the_range_once_when_a_destructor_panics	av-drain-unwind	av-drain
tests/panic_drop_parity.rs	small_vec_clear_drops_each_element_once_when_a_destructor_panics	sv-drop-exactly-once	
tests/panic_drop_parity.rs	small_vec_drop_drops_each_element_once_when_a_destructor_panics	sv-drop-exactly-once	
tests/panic_drop_parity.rs	small_vec_into_iter_drop_drops_each_element_once_when_a_destructor_panics	sv-drop-exactly-once	sv-into-iter
tests/panic_drop_parity.rs	array_vec_retain_drops_each_element_once_when_a_rejected_destructor_panics	av-retain-unwind	av-bulk-drop-exactly-once
```

## 3. Coverage

- 57 tests read. 55 map to one of the 18 specs. 2 are not spec samples (§4).
- The biggest collapses: `sv-seq-contracts` (11 primary), `sv-drop-exactly-once` (11),
  `sv-into-iter` (5), `av-drain` (4), `av-bulk-drop-exactly-once` (4), `av-retain-unwind` (3),
  `sv-representation` (3).
- `av-retain` has no primary test. It is the non-panicking base case that the unwind tests
  build on, and it is written down so that `av-retain-unwind` has a lemma to extend.
- `av-refines-bounded-vec` has only 2 primary tests, but it is the statement the whole
  differential file samples. Once it is proved, the per-op parity tests in
  `tests/arrayvec_differential.rs` also have no remaining job.

## 4. Tests that are not spec samples

The two tests that sampled no spec are deleted: the negative control for the `Recorder` fixture
in `tests/panic_drop_parity.rs` (every sibling drop test already asserts the unwind and the full
drop log) and the shim-identity check (a compile-only type equality).

No test is a pure regression of a removed bug with no general property. Every panic-safety and
drop-count test samples one of the exactly-once specs.

## 5. Implied specs that look wrong, or disagree with the code

1. **Test-only representation helper removed (resolved 2026-10-27).**
   `small_vec::IntoIter::is_inline` (`#[cfg(test)]`) and `into_iter_keeps_the_storage_variant`
   have been deleted. The representation clause on the private repr stays in `sv-into-iter`.
2. **Differential post-panic scope.** `apply` in `tests/arrayvec_differential.rs` runs each op
   that may panic (`Push`, `Insert`, `ExtendFromSlice`, `Extend`) on a clone and writes the
   clone back only on success, so `scripted_operation_sequence` and `lcg_operation_fuzz` compare
   panic polarity for those ops while `overflow_policy_parity` checks prefix retention.
3. **`ArrayVec::insert` out-of-bounds panic sampled (resolved 2026-10-27).** `apply` reduces the
   index mod `len + 1`, and `insert_out_of_bounds_panics_like_model` now exercises `index > len`
   panics directly. Because `ArrayVec` checks index before fullness and the model checks fullness
   first, `av-refines-bounded-vec` states the panic condition as `i > len ∨ len = N` and names no
   message.
4. **`Inv_AV`/`Inv_SV` are not proved anywhere.** Every function these specs cover that touches
   `MaybeUninit`, raw pointers or `Drop` is `#[cfg_attr(trust_verify, trust::skip)]`
   (`drop_initialized`, `pop`, `clear`, `truncate`, `as_slice`, `as_mut_slice`, `insert`,
   `remove`, `retain`, `into_inner`, `from_iter`, `extend` in `array_vec.rs`; most of
   `small_vec.rs`). No current Trust row covers any of the subsumed tests, so the floor
   cannot rise beyond the safe accessors until gap 8 (unsafe discharge) and gap `U` exist.
5. **Forgotten `Drain` clause sampled (resolved 2026-10-27).**
   `src/array_vec.rs::test_drain_forget_keeps_unyielded_elements` now checks `av-drain`'s
   `mem::forget` retention and drop-count clause.

## 6. Proof order (easiest high-value first)

No spec here is blocked by gap 0 alone: gaps 1 and 11 block all 18, because both containers are
generic and their contents are a List model over a struct. So the first step is gap 11
(per-definition proofs of generic fns) together with a struct carrier for `ArrayVec { buf, len }`
that is length-only in its first form. With those, the order is:

1. **`capacity-error-contract`** (gaps 0, 1, 3, 11). Safe code with no `&mut`, no loops and no
   unsafe. `element`/`simplify` are moves; Display/Debug are constant strings and need only a
   Formatter model. It retires `test_capacity_error_shape`.
2. **`av-accessors`** (gaps 0, 1, 2, 3, 11). `&self` scalar reads and one `saturating_sub`. Its
   length half needs only a length-only carrier (what Trust has today for `Vec`), so it is the
   first step past the crate's current floor. It fixes `len ≤ N`, the core conjunct of `Inv_AV`.
3. **`av-push`** without its panic clause (gaps 0, 1, 2, 11). `push` and `try_push` are not
   `trust::skip`, and the `Ok` branch is one `MaybeUninit::write` plus `len += 1` under
   `len < N`. The overflow clause waits on gap `U`. It is the first content (not length)
   obligation for gap 1.
4. **`av-slice-view`** (gaps 0, 1, 2, 8, 11). This is the single unsafe fact
   (`from_raw_parts(buf, len)` is `Seq v`) that every later spec reads. Proving it is the
   pilot for gap 8 in this crate. It retires 2 differential tests.
5. **`av-bulk-drop-exactly-once`** and **`sv-drop-exactly-once`** (gaps 0, 1, 2, 8, 11, U).
   They share `drop_initialized` and the "lower len, then drop the slice" shape, so a gap `U`
   model proved once covers both. Together they retire 15 tests, the largest single gain in the
   crate.

Then `sv-representation` → `sv-seq-contracts` (11 tests) → `sv-into-iter`, then the ArrayVec
specs with loops and closures (`av-retain`, `av-extend-overflow`, `av-clone-from`, gaps 7 and
13), the unwind variants, and last the master simulation `av-refines-bounded-vec`.
