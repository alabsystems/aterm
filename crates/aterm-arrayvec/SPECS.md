<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-arrayvec: spec recovery from its 6 unit tests

> Status 2026-10-16: this file is the crate's migration worklist. The 10 specs below
> are proved through Trust/Clean about the compiled functions in `aterm-alloc`, and then
> the runtime `#[test]` bodies they subsume are deleted. Nothing has been proved or
> deleted yet.

Source: `crates/aterm-arrayvec` (read only). The crate has **no implementation**:
`src/lib.rs` only re-exports `aterm_alloc::{ArrayVec, ArrayVecDrain as Drain,
ArrayVecIntoIter as IntoIter, CapacityError}`. Every spec below is therefore a contract
of `crates/aterm-alloc/src/array_vec.rs` and is proved **there**. That `arrayvec::X` is
`aterm_alloc::X` is a type-system fact, not a spec.

The tests are the 6 `#[test]` fns in `tests/consumer_forms.rs`: `naga_forms`,
`wgpu_forms`, `wgpu_core_forms`, `wgpu_hal_forms`, `deref_sites_forms` and
`iterator_types_are_nameable`. (A `#[test]` substring count gives 7; the seventh hit is
the doc comment on line 29.) Nothing was compiled or run. The claims below come from
reading the code. `tiny_skia_forms` and its `mod tiny_skia` left on 2026-10-17 with
tiny-skia itself (linux-windowing LW.5); every spec it sampled is still sampled by
another test.

Result: **10 specs**, covering **all 6 tests**. **0 tests are non-spec samples** (§4).
Every test is a multi-spec smoke test, so each one is retired only when every spec it
maps to in §2 is Certified.

## 0. Notation used in the statements

- `av : ArrayVec<T,N>`. `view(av) : Seq T` is the list `av.as_slice()`: the `len`
  initialized slots, in order.
- `Inv(av)`: `|view(av)| = av.len ≤ N`, slots `[0,len)` are initialized and slots
  `[len,N)` are not. Every statement is for all `T`, all `N` and all `av` with `Inv(av)`,
  and every operation preserves `Inv`.
- `x'` is the value of `x` after the call (two-state).
- `xs(it)` is the finite sequence that an `IntoIterator` `it` yields.
- `Own`: the ownership observation (a drop counter, or `Arc::strong_count`). Over the life
  of a vec and its iterators, every element is either moved out once or dropped once.
  It is never leaked and never dropped twice.
- `map f s`: pointwise image of a sequence. `s[a..b]`: a subsequence. `++`: concatenation.
- "panics(m)": the call panics with a message containing `m`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions` (all in `aterm_alloc`), `statement`, `subsumes`
(tests in `tests/consumer_forms.rs`), `difficulty (Rust features the proof must handle)`,
`blocking gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
AV-EMPTY	contract	ArrayVec::{new,new_const,clear,is_empty,len}, <ArrayVec as Default>::default	∀ T,N: view(new()) = view(new_const()) = view(default()) = []. After av.clear(): view(av') = [] and every element of view(av) is dropped exactly once (Own). av.is_empty() ⇔ av.len() = 0 ⇔ view(av) = []. av.len() = |view(av)|.	wgpu_core_forms, wgpu_hal_forms	EASY: const generics, MaybeUninit array, &mut self, element drop glue (clear is trust::skip today)	0, 1, 2, 8, 11
AV-PUSH	contract	ArrayVec::{push,try_push,pop}, CapacityError::element	If |view(av)| < N: push(x) and try_push(x) give view(av') = view(av) ++ [x], and try_push returns Ok(()). If |view(av)| = N: push panics("ArrayVec overflow"), and try_push returns Err(e) with e.element() = x and view(av') = view(av). pop: view(av) = [] ⇒ None and view unchanged; view(av) = s ++ [y] ⇒ Some(y) and view(av') = s.	naga_forms, wgpu_core_forms, wgpu_hal_forms, iterator_types_are_nameable	EASY-MEDIUM: two-state &mut self, Result/Option of generic T, panic contract (push already carries contract_panic), MaybeUninit write / assume_init_read (pop is trust::skip)	0, 1, 2, 8, 11
AV-DEREF-VIEW	contract	<ArrayVec as Deref>::deref, <ArrayVec as DerefMut>::deref_mut, ArrayVec::{as_slice,as_mut_slice}	&*av = av.as_slice() denotes exactly view(av): length len, never N. &mut *av = av.as_mut_slice() denotes the same len slots, and any in-place slice mutation m through it (sort, get_mut, in-bounds get_unchecked_mut, last_mut) gives view(av') = m(view(av)) with len unchanged.	deref_sites_forms, wgpu_core_forms, wgpu_hal_forms	EASY: slice from the MaybeUninit prefix (unsafe from_raw_parts; both fns are trust::skip), &mut self	0, 1, 2, 8, 11
AV-INTO-INNER	contract	ArrayVec::{into_inner,into_inner_unchecked}	av.into_inner() = Ok(a) with a (as a Seq) = view(av) iff |view(av)| = N; otherwise Err(b) with view(b) = view(av). No element is dropped or duplicated in either arm (Own). into_inner_unchecked: requires len = N, ensures the same as the Ok arm.	naga_forms	EASY for the len comparison; MEDIUM for the unsafe ptr::read of [MaybeUninit<T>;N] as [T;N] under ManuallyDrop (both fns are trust::skip)	0, 1, 8, 11
AV-EQ-HASH	equivalence	<ArrayVec as PartialEq>::eq, <ArrayVec as Hash>::hash	(a == b) ⇔ view(a) == view(b) as slices. ∀ H: Hasher, a.hash(h) makes exactly the writes <[T] as Hash>::hash(view(a), h) makes. Hence a == b ⇒ hash(a) = hash(b) for any T whose Eq and Hash agree, which the consumers' HashMap-key derives rely on (AttachmentData, ProgramCacheKey, RenderPassKey, FramebufferKey).	wgpu_core_forms, wgpu_hal_forms	EASY once delegation to the slice impls is expressible: trait-generic T: PartialEq/Hash and H: Hasher (higher-order over the hasher)	0, 1, 11, 13
AV-REF-ITER	contract	<&ArrayVec as IntoIterator>::into_iter, <&mut ArrayVec as IntoIterator>::into_iter, ArrayVec::{iter,iter_mut}	(&av).into_iter() and av.iter() yield references to exactly view(av), in order, and never to slots [len,N). (&mut av).into_iter() and av.iter_mut() yield exactly len mutable references, in order, so applying f through each gives view(av') = map f (view(av)) with |view(av')| = |view(av)|.	naga_forms, wgpu_hal_forms	EASY given AV-DEREF-VIEW plus a slice-iterator model; &mut aliasing; the loop is in the caller	0, 1, 2, 7, 11
AV-INTO-ITER	contract	<ArrayVec as IntoIterator>::into_iter, <IntoIter as Iterator>::{next,size_hint}, <IntoIter as DoubleEndedIterator>::next_back, IntoIter::as_slice	it = av.into_iter() yields exactly view(av) in order (next) or in reverse (next_back), then None. At every point it.as_slice() is the not-yet-yielded remainder and it.len() = size_hint().0 = size_hint().1 = |it.as_slice()|. Own: a yielded element is moved out once; an un-yielded one is dropped once when it is dropped (no leak, no double drop; Arc::strong_count returns to its pre-push value).	naga_forms, wgpu_core_forms, wgpu_hal_forms, iterator_types_are_nameable	MEDIUM: Iterator impl over owned state; next is remove(0) and next_back is pop, so it composes their contracts (both trust::skip); drop reasoning without an impl Drop	0, 1, 2, 3, 4, 7, 8, 11
AV-DRAIN	contract	ArrayVec::drain, <Drain as Iterator>::next, <Drain as DoubleEndedIterator>::next_back, <Drain as Drop>::drop	For a range R resolving to [s,e): s > e or e > len ⇒ drain panics. Otherwise d = av.drain(R) has d.len() = e − s, yields view(av)[s..e) in order (next) or in reverse (next_back), and once d is dropped, whether fully, partly or not consumed, view(av') = view(av)[..s] ++ view(av)[e..], with each un-yielded element of [s,e) dropped exactly once (also when one of their destructors panics). If d is forgotten, the un-yielded elements stay in the vec.	wgpu_hal_forms, iterator_types_are_nameable	MEDIUM-HARD: RangeBounds generic and a Bound match with saturating_add, an &mut borrow held by the iterator, remove-based next, Drop with rotate + truncate, an unannotated panic contract (§5)	0, 1, 2, 3, 4, 7, 8, 11
AV-CLONE	contract	<ArrayVec as Clone>::{clone,clone_from}	∀ T: Clone: view(av.clone()) = map clone (view(av)) and view(av) is unchanged. After self.clone_from(&src): view(self') = map clone (view(src)) (pointwise under T's Clone), and self's surplus tail when |self| > |src| is dropped exactly once.	wgpu_core_forms, wgpu_hal_forms	MEDIUM: calls the user's Clone (higher-order through a trait bound), loops, truncate / extend_from_slice, &mut self	0, 1, 2, 3, 7, 11, 13
AV-FROM-ITER-EXTEND	contract	<ArrayVec as FromIterator>::from_iter, <ArrayVec as Extend>::extend	For any it with finite xs(it): from_iter(it) returns av with view(av) = xs(it) when |xs(it)| ≤ N, and panics("ArrayVec overflow") otherwise; it never truncates. av.extend(it) gives view(av') = view(av) ++ xs(it) when |view(av)| + |xs(it)| ≤ N; otherwise it panics at the first element past capacity, with the elements before it already pushed. Sampled instances: collect from slice iterators, ranges, arrays, Option (0 or 1 element) and mapped iterators.	naga_forms, wgpu_forms, wgpu_core_forms, wgpu_hal_forms, deref_sites_forms, iterator_types_are_nameable	HARD: a for loop over a caller-chosen generic IntoIterator (higher-order), a quantified statement over its sequence, a panic contract; both impls are trust::skip	0, 1, 2, 5, 7, 11, 13
```

### Distribution by kind (10)

| kind | count | ids |
|---|---|---|
| contract | 9 | AV-EMPTY, AV-PUSH, AV-DEREF-VIEW, AV-INTO-INNER, AV-REF-ITER, AV-INTO-ITER, AV-DRAIN, AV-CLONE, AV-FROM-ITER-EXTEND |
| equivalence | 1 | AV-EQ-HASH |

### Gap histogram (specs blocked by each audit §7 gap)

| gap | specs |
|---|---|
| 0 seal publishes Clean rows | 10 |
| 1 non-scalar carriers (`Seq T` view of the buffer) | 10 |
| 11 generics (`T`, const `N`) | 10 |
| 2 `&self`/`&mut self` views, two-state | 8 |
| 8 unsafe / init tracking (`MaybeUninit`, drop-once) | 6 |
| 7 loops | 5 |
| 3 E6 body widening (calls, matches) | 3 |
| 13 higher-order (user `Clone`, `Hasher`, caller iterator) | 3 |
| 4 callee-ensures composition (`remove`, `pop`) | 2 |
| 5 quantified clauses | 1 |

No spec is blocked by gap 0 alone. Gaps 1 and 11 block all ten, so the cheapest real start
is a **monomorphised, length-only** slice of a spec (concrete `T` and `N`; Vec-style
carriers are length-only today, audit §7 row 1), which needs gaps 0 and 2 only.

## 2. Test → spec map (TSV; all 6)

`primary` is the spec that only this test (or this test most directly) samples. `also`
lists the other specs it samples. Each test is deleted only when its primary and every
`also` spec are Certified. Paths are relative to `crates/aterm-arrayvec`.

```tsv
file	test	primary	also
tests/consumer_forms.rs	naga_forms	AV-INTO-INNER	AV-PUSH, AV-FROM-ITER-EXTEND, AV-INTO-ITER, AV-REF-ITER
tests/consumer_forms.rs	wgpu_forms	AV-FROM-ITER-EXTEND	
tests/consumer_forms.rs	wgpu_core_forms	AV-EQ-HASH	AV-EMPTY, AV-PUSH, AV-FROM-ITER-EXTEND, AV-INTO-ITER, AV-CLONE, AV-DEREF-VIEW
tests/consumer_forms.rs	wgpu_hal_forms	AV-DRAIN	AV-EMPTY, AV-PUSH, AV-FROM-ITER-EXTEND, AV-INTO-ITER, AV-REF-ITER, AV-CLONE, AV-EQ-HASH, AV-DEREF-VIEW
tests/consumer_forms.rs	deref_sites_forms	AV-DEREF-VIEW	AV-FROM-ITER-EXTEND
tests/consumer_forms.rs	iterator_types_are_nameable	AV-INTO-ITER	AV-PUSH, AV-FROM-ITER-EXTEND, AV-DRAIN
```

## 3. Coverage

- 6 tests read. All 6 map to the 10 specs. None is a non-spec sample.
- `AV-FROM-ITER-EXTEND` is sampled by all 6 tests and is the hardest spec (§6), so it
  decides when this file's tests can go. Until it is Certified, no test here can be deleted,
  even if all the other nine are.
- `wgpu_forms` is the only test retired by a single spec (`AV-FROM-ITER-EXTEND`).
- Overlap: `crates/aterm-alloc/tests/arrayvec_differential.rs` samples the same contracts
  against a `Vec`-backed model. The `aterm-alloc` worklist should reuse these spec ids so
  that each is proved once, in `aterm-alloc`, and retires the tests of both crates.

## 4. Tests that are not spec samples

| test | why | recommendation |
|---|---|---|
| (none) | Every `#[test]` samples at least one spec. | — |

Not a test, and not subsumed by any spec: the compile-time half of
`tests/consumer_forms.rs` (the `mod naga`, `wgpu`, `wgpu_core`, `wgpu_hal`,
`deref_sites` items, which type-check every consumer invocation form verbatim). It is the
crate's stated deliverable (file header, lines 7-32), a check of the *surface*, not of
behaviour. **Keep it.** When the runtime `#[test]` bodies are deleted, the `pub` items
must stay so the integration-test target still type-checks them.

## 5. Implied specs that look wrong, or disagree with the code

1. **Most of the functions these specs cover carry no proof obligation today.** In
   `crates/aterm-alloc/src/array_vec.rs` these are `#[cfg_attr(trust_verify, trust::skip)]`:
   `pop`, `clear`, `truncate`, `as_slice`, `as_mut_slice`, `insert`, `remove`, `retain`,
   `into_inner`, `into_inner_unchecked`, `FromIterator::from_iter` and `Extend::extend`.
   The skips have to come off (or be replaced by contracts the callers can cite) before
   `AV-PUSH` (pop), `AV-EMPTY` (clear), `AV-DEREF-VIEW`, `AV-INTO-INNER`,
   `AV-FROM-ITER-EXTEND`, and, through `remove`, `AV-INTO-ITER` and `AV-DRAIN` can be proved.
2. **`drain`'s panics are unannotated.** `push`, `insert`, `set_single` and
   `extend_from_slice` carry `trust::contract_panic(message_contains = "ArrayVec overflow")`
   with const messages for that purpose. `drain`'s two `assert!`s use formatted messages
   (`"ArrayVec::drain: start {start} > end {end}"`, `"… end {end} > len {len}"`) and no
   `contract_panic`, so the panic half of `AV-DRAIN` has nothing to bind to. Give them
   const messages and an annotation.
3. **`drain`'s bound arithmetic saturates.** `Bound::Excluded(usize::MAX)` as a start
   becomes `usize::MAX`, not `MAX + 1`. That is harmless, because `end ≤ len < MAX` then
   fails the first assert, but the spec's "R resolving to [s,e)" must be stated through the
   same saturating resolution, not through mathematical `+1`.
4. **The `Drain` behaviour differs from upstream `arrayvec`.** A forgotten `Drain` keeps
   the un-drained elements, and a panicking destructor during `Drop` still drops the rest
   and closes the tail up (std `Vec::Drain`'s outcome; upstream leaks both). `AV-DRAIN`
   states aterm's behaviour. The crate's claim of "the same behaviour" as upstream holds
   only for the non-panicking, non-forgotten case, so the claim in `src/lib.rs` is
   stronger than the code.
5. **`IntoIter` forward iteration is O(n²).** `next` is `remove(0)`. That is correct and
   documented (capacities ≤ 32 in the patched graph), but it means `AV-INTO-ITER` is
   proved by composing `remove`'s contract, not by a pointer-cursor invariant.
6. **Stale instructions.** `src/lib.rs` tells the reader to re-check with
   `cargo test -p arrayvec` / `cargo test -p aterm-alloc`. AGENTS.md requires
   `targo --unverified test`. This is stale prose, not a test defect.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone (§1 histogram), so each step starts with the
monomorphised length-only slice and widens to the full `Seq` statement as gaps 1 and 11
close.

1. **`AV-EMPTY`**. Straight-line constructors and `len`/`is_empty`. The length half
   (`len(new()) = 0`, `is_empty ⇔ len = 0`, `len(clear(av)) = 0`) is near-scalar. It also
   fixes `Inv`, which every later spec reuses.
2. **`AV-DEREF-VIEW`**. It defines `view` itself (`as_slice` is the `Seq` carrier), so
   every other statement depends on it. It needs the `as_slice`/`as_mut_slice` skips
   replaced by a contract over the initialized prefix.
3. **`AV-PUSH`**. `push` already carries `contract_panic`. The length half
   (`len' = len + 1` or a panic at `len = N`; `try_push` leaves `len` unchanged on `Err`) is
   linear arithmetic. `pop` needs its skip removed.
4. **`AV-INTO-INNER`**. The `Ok`/`Err` split is a pure `len` comparison. The `ptr::read`
   arm waits on gap 8.
5. **`AV-EQ-HASH`**. Delegation to the slice impls. It is high value (four HashMap-key
   derives in wgpu rely on it) once gap 13's trait-generic form is admitted.
6. **`AV-REF-ITER`**. Free given `AV-DEREF-VIEW` and a slice-iterator model.
7. **`AV-INTO-ITER`**, then **`AV-DRAIN`**. Both compose `remove`'s contract (gap 4).
   Prove `remove` first, in `aterm-alloc`.
8. **`AV-CLONE`**. Loops plus the user's `Clone`.
9. **`AV-FROM-ITER-EXTEND`** last. It is the hardest (a loop over a caller-chosen
   iterator, a quantified statement, gaps 5, 7 and 13), and it is sampled by all 6 tests,
   so the file's tests are deleted together when it is Certified.
