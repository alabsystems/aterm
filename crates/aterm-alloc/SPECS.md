<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-alloc: spec recovery from unit tests

Each §1 row states, for all inputs, a property that unit tests in `crates/aterm-alloc` sample. A row
proved through Trust/Clean about the compiled function retires the tests it subsumes; §2 maps each
test to its rows. The statements come from reading the code.

## 0. Notation used in the statements

- `Seq(v) : List T` is the abstract contents of an `ArrayVec<T,N>` or `SmallVec<T,N>`: `buf[0..len)`
  read as `T` for `ArrayVec` and for `SmallVec`'s `Inline{buf,len}`, and `vec` for `Heap(vec)`.
  `as_slice` and `Deref` return exactly `Seq(v)`.
- `Inv_AV(v)`: `len ≤ N`, `buf[0..len)` is initialized and owned by `v`, and `buf[len..N)` is not
  owned. `Inv_SV(v)`: either `Inline{buf,len}` with `len ≤ N` and the same prefix ownership, or
  `Heap(vec)`. Every spec assumes `Inv` on entry and proves it on every exit, unwinding exits included.
- `BV_N`: a `Vec` bounded to capacity `N` under upstream arrayvec's contract (the `model` in
  `tests/arrayvec_differential.rs`). `push`, `insert`, `extend` and `collect` panic when full;
  `try_push` and `try_extend_from_slice` refuse; an overflowing `extend` keeps the prefix it wrote.
- `Panics(op)`: `op` unwinds. Where a spec says so, it also gives the post-state on the unwinding exit.
- `Ledger`: the sequence of element identities whose destructor ran. `ExactlyOnce(run)`: once the
  container, iterator or `Drain` is gone, every element moved in and never moved out by value
  appears in the Ledger exactly once. Moving out by value is `pop`, `remove`, `next`/`next_back`,
  `into_inner`, a yielded drain element or a `try_push` `Err`; the new owner drops that element.
  `ExactlyOnce` also holds when one destructor panics (slice drop glue drops the rest, then the panic
  propagates); a second panic aborts and is out of scope.
- `RangeRes(len, R) = (s, e)`: start `Unbounded → 0`, `Included(i) → i`, `Excluded(i) → i sat+ 1`;
  end `Unbounded → len`, `Included(j) → j sat+ 1`, `Excluded(j) → j`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary ones are in
§2), `rust_features` (what the proof must handle), `blocking_gaps` (rows of the gap table in
`docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7, plus `U`). Gap `U` is no audit row: a model
of unwinding and destructors (panic edges as outcomes with a post-state, `Drop` glue,
`drop_in_place`, the Ledger). Every row lists gap 0 (the installed seal publishes no Lean-discharged
row) and gap 11 (both containers are generic in `T` and const-generic in `N`, so a proof at one
instance is a sample, not the spec).

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

- `av-accessors`, `av-push`, `sv-seq-contracts` and `sv-representation` subsume no test (`-`); later
  rows build on them. `av-retain` has no primary test either: it is the non-panicking base case that
  `av-retain-unwind` extends.
- `av-refines-bounded-vec` is what the whole differential file samples: once it is proved, the
  per-op parity tests in `tests/arrayvec_differential.rs` have no remaining job.

## 4. Tests missing from §2

- `src/lib.rs::alloc_const_helpers_round_trip` samples no row: it checks `fits_inline`,
  `next_capacity_for` and the `encode_alloc_counts_le`/`decode_alloc_counts_le` round trip.
- `src/small_vec.rs::into_iter_keeps_the_storage_variant` samples `sv-into-iter`'s representation
  clause (§5.1).
- `src/small_vec.rs::into_iter_drops_each_element_exactly_once` samples `sv-drop-exactly-once` and
  `sv-into-iter`.

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

Gaps 1 and 11 block every row, so the first step is gap 11 (per-definition proofs of generic fns)
with a struct carrier for `ArrayVec { buf, len }`, length-only at first. Then:

1. **`capacity-error-contract`**: safe code with no `&mut`, loops or unsafe; `element`/`simplify`
   are moves, and Display/Debug write constant strings that need only a Formatter model.
2. **`av-accessors`**: `&self` scalar reads and one `saturating_sub`. Its length half needs only a
   length-only carrier, and it fixes `len ≤ N`, the core conjunct of `Inv_AV`.
3. **`av-push`** without its panic clause: `push` and `try_push` are not `trust::skip`, and the `Ok`
   branch is one `MaybeUninit` write plus a length step under `len < N`. It is the first content
   obligation for gap 1; the overflow clause waits on gap `U`.
4. **`av-slice-view`**: the one unsafe fact (`from_raw_parts(buf, len)` is `Seq v`) every later spec
   reads, and this crate's pilot for gap 8.
5. **`av-bulk-drop-exactly-once`** and **`sv-drop-exactly-once`**: they share `drop_initialized` and
   the "lower len, then drop the slice" shape, so one gap `U` model covers both (the into-iter clause
   of `sv-drop-exactly-once` also needs `sv-into-iter`).
6. ArrayVec: `av-into-inner`, `av-drain`, `av-into-iter`, then the loop and closure specs
   (`av-retain`, `av-extend-overflow`, `av-clone-from`; gaps 7 and 13), the unwind variants
   (`av-drain-unwind`, `av-retain-unwind`), and last the conjunction `av-refines-bounded-vec`.
7. SmallVec: `Inv_SV` → `sv-representation` (the spill preserves `Seq`) → `sv-seq-contracts` →
   `sv-into-iter`.
