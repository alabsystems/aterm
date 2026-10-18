<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-smallvec: spec recovery from its 10 unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each of the 8 specs below is
> proved through Trust/Clean about the compiled Rust, and then the tests it subsumes are
> deleted. The 2 checks in §4 sample no spec and can be deleted now. None of the 8 specs is
> blocked only by gap 0 (the seal). §6 names a scalar stepping-stone that is.
> Re-verified 2026-10-16 by a second recovery pass, which added gap 8 (unsafe-block discharge in
> `aterm_alloc` callees) to `sv-repr-inv` and `sv-ops-refine-vec` (§5.7).

Source: `crates/aterm-smallvec` (read only). I read every `#[test]`
(tests/{differential 3, upstream_transcript 2, consumer_forms 5} = 10) and the code each one reaches.
That code is `src/lib.rs`, the `aterm_alloc::SmallVec<T, N>` it wraps
(`crates/aterm-alloc/src/small_vec.rs`), and the recorder `tests/transcript/script.rs`.
Nothing was compiled or run. The claims below come from reading the code.

Result: **8 specs**, covering **8 tests**. **2 tests are not spec samples** (§4).
The 8 specs share their 8 tests heavily. The fuzz and the transcript replay each sample 5 to 7 specs.

## 0. Notation used in the statements

The statements are over `SmallVec<[T; N]>` (`src/lib.rs`), a wrapper over `aterm_alloc::SmallVec<T, N>`.

- `items(v)` = `v.as_slice()` as a `Seq T`. `sp(v)` = `v.spilled()`. `cap(v)` = `v.capacity()`.
  A primed name (`v'`) is the state after the call.
- `CAP` = 16_777_216 (2^24). This is the clamp in `aterm_alloc::SmallVec::with_capacity`
  (`crates/aterm-alloc/src/small_vec.rs:49-60`). **Every spec assumes `N <= CAP`** (§5.3).
- `Inv(v)` := (¬sp(v) ⇒ cap(v) = N ∧ |items(v)| ≤ N) ∧ (sp(v) ⇒ cap(v) > N ∧ cap(v) ≥ |items(v)|).
- `Reach(N)`: the vectors reachable from any constructor (`new`, `new_const`, `default`,
  `with_capacity`, `from_vec`, `from_elem`, `from_slice`, `From<&[T]>`, `From<Vec>`, `FromIterator`,
  `clone`, `smallvec!`) by any sequence of public operations. Panicking calls caught by unwind count.
- `VecOp(op)`: the std `Vec<T>` operation with the same name and arguments, applied to `items(v)`.
  `retain` and `retain_mut` map to `Vec::retain_mut`; `resize`, `drain` and `into_iter` map to their
  `Vec` namesakes. **Refines** means three things: the return value is equal, `items(v')` equals the
  `Vec` result, and the call panics exactly when the `Vec` call panics. A panicking call leaves
  `items(v)` and `sp(v)` unchanged, except that the element passed in is dropped.
- **Growth ops**: `push`, `insert`, `resize`/`resize_with` to a larger length, `extend`,
  `extend_from_slice`, `clone_from`. **Non-growth ops**: `pop`, `remove`, `swap_remove`, `truncate`,
  `clear`, `retain`, `retain_mut`, `drain` (when the `Drain` drops), `resize` to a smaller length, and
  any panicking call.
- `Drops(op)`: the sequence of element destructor calls an op makes, in order. It is observed with
  an element type whose `Drop` records its value, like `E` in `tests/transcript/script.rs`.
- `Upstream`: the behaviour of crates.io `smallvec` 1.15.1, as `tests/transcript/smallvec-1.15.1.txt`
  records it. It needs a written model before `sv-drop-order-upstream` can be stated as a theorem (§5.6).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty (Rust features the proof
must handle)`, `blocking gaps`. The gap numbers are rows of the table in §7 of
`AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit).
Gap 0 (the seal) is a prerequisite for every row and is not repeated. It is closed locally by seal
`150efe4a`, which is not yet on the machine's `trust` channel.
In `subsumes`, `diff` is `tests/differential.rs`, `up` is `tests/upstream_transcript.rs` and `cf` is
`tests/consumer_forms.rs`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
sv-repr-inv	invariant	SmallVec::{new,new_const,with_capacity,from_vec,from_elem,from_slice,push,pop,insert,remove,swap_remove,truncate,clear,retain,retain_mut,resize,resize_with,reserve,reserve_exact,regrow,extend_from_slice,drain,into_vec,clone,clone_from,len,is_empty,capacity,spilled,inline_size}, Extend::extend, FromIterator::from_iter, Drain::{next,next_back,drop}, aterm_alloc::SmallVec::{new,with_capacity,from_vec,push,pop,spill_and_push,ensure_heap,len,capacity,spilled}	∀ N ≤ CAP, T, v ∈ Reach(N): Inv(v) ∧ v.len() = |items(v)| ∧ (v.is_empty() ⇔ |items(v)| = 0) ∧ v.inline_size() = N. Inductively, every constructor establishes Inv and every public &mut op (and each Drain step and drop) preserves it, including on the unwinding panic path.	diff::lcg_operation_fuzz_at_five_inline_sizes, cf::rustybuzz_forms	MEDIUM-HARD: generic A: Array with an associated Store type and a const N; aterm_alloc's Inline/Heap enum; Vec capacity; &mut self two-state; loops (truncate, regrow, retain, ensure_heap); unwinding panics; aterm_alloc callees (pop, ensure_heap) that are trust::skip over unsafe MaybeUninit reads	1, 2, 3, 7, 8, 11, 12
sv-ops-refine-vec	refinement	SmallVec::{push,pop,insert,remove,swap_remove,truncate,clear,retain,retain_mut,resize,resize_with,reserve,reserve_exact,extend_from_slice,into_vec,as_slice,as_mut_slice}, DerefMut (iter_mut), Extend::extend, FromIterator::from_iter, IntoIterator for SmallVec/&SmallVec/&mut SmallVec, IntoIter::{next,next_back,size_hint}, Index/IndexMut, Clone::clone_from	∀ N ≤ CAP, v ∈ Reach(N), each op above and any arguments: op refines VecOp(op) on items(v). In particular: insert(i,x) panics iff i > len; remove(i) and swap_remove(i) panic iff i ≥ len; truncate(n) gives items[..min(n,len)]; resize(n,x) gives items[..n] or items ++ [x]^(n−len); retain(f) calls f once per item, front to back, mutably, and keeps the accepted items in order; reserve/reserve_exact leave items unchanged; extend(it) and extend_from_slice(s) append in order; into_iter() yields items front to back and next_back from the back; into_vec(v) = items(v); c.clone_from(&s) gives items(c') = items(s); v[i] = x writes items[i]. Corollary: collecting a Result adapter gives Ok(items) on all-Ok input.	diff::lcg_operation_fuzz_at_five_inline_sizes, up::the_facade_replays_the_upstream_transcript, cf::rustybuzz_forms, cf::parking_lot_core_forms, cf::wayland_forms, cf::wgpu_forms	HARD: content models of Vec<T> and slices; rotate_left/rotate_right/swap/reverse on &mut [T]; closures (retain's FnMut, resize_with's FnMut); iterators (size_hint, repeat().take()); loops; &mut self; panics and unwinding; generics over A: Array; cross-crate aterm_alloc callees, some trust::skip over unsafe	1, 2, 3, 4, 5, 7, 8, 11, 12, 13
sv-spill-rule	contract	SmallVec::{push,insert,resize,resize_with,extend_from_slice,reserve,reserve_exact,regrow,pop,remove,swap_remove,truncate,clear,retain,drain,into_vec,from_vec}, Extend::extend, Clone::clone_from	∀ N ≤ CAP, v ∈ Reach(N): (a) a growth op gives sp(v') = sp(v) ∨ |items(v')| > N. (b) reserve(n) and reserve_exact(n) give sp(v') = sp(v) ∨ |items|+n > N; when |items|+n ≤ CAP they also give cap(v') − |items| ≥ n; they panic iff |items|+n overflows usize, and reserve also panics when its checked_next_power_of_two overflows. (c) A non-growth op gives sp(v') = sp(v): nothing un-spills in place. (d) For w = into_vec(v): sp(v) ⇒ w.capacity() ≥ cap(v), and from_vec(w) has items(v) and sp = (w.capacity() > N).	diff::lcg_operation_fuzz_at_five_inline_sizes, up::the_facade_replays_the_upstream_transcript	MEDIUM: &mut self two-state; checked_add and checked_next_power_of_two; mem::replace in regrow; loops; Vec capacity across aterm_alloc (cross-crate); generics. Part (b)'s arithmetic is scalar (§6.0)	1, 2, 3, 7, 11, 12
sv-constructors	contract	SmallVec::{new,new_const,with_capacity,from_vec,from_elem,from_slice}, Default::default, From<&[T]>::from, From<Vec<T>>::from, FromIterator::from_iter, Clone::clone	∀ N ≤ CAP: new(), new_const() and default() give items = [] ∧ ¬sp. with_capacity(n) gives items = [], sp ⇔ n > N, and sp ⇒ cap ≥ min(n, CAP). from_vec(w) gives items = w, sp ⇔ w.capacity() > N (a short Vec with a long buffer stays spilled), and sp ⇒ cap ≥ min(w.capacity(), CAP). from_elem(e,n) gives items = [e]^n and sp ⇔ n > N. from_slice(s), From<&[T]>(s) and from_iter(it) give items = s (or the iterator's sequence), sp ⇔ |items| > N, and cap ≥ |items|. clone(v) gives items(v) and sp ⇔ |items(v)| > N, whatever sp(v) was.	diff::constructors_follow_upstreams_spill_rules, cf::wayland_forms, cf::wgpu_forms, cf::rustybuzz_forms, up::the_facade_replays_the_upstream_transcript	MEDIUM: Vec content and capacity; a const fn (new_const); Clone and Copy bounds; loops; iterator size_hint; generics; aterm_alloc's with_capacity clamp (cross-crate). No &mut self	1, 3, 4, 7, 11, 12
sv-macro	contract	smallvec! (macro_rules expansion), SmallVec::{new,push,from_vec,from_elem,inline_size}	∀ N ≤ CAP, k ≥ 0, expressions x1..xk, e, n: smallvec![] = new(); smallvec![x1,…,xk] gives items = [x1..xk] and sp ⇔ k > N, with each xi evaluated once, in order; smallvec![e; n] = from_elem(e, n). The same holds when the macro is reached through a re-export ($crate path).	cf::wayland_forms, cf::wgpu_forms, up::the_facade_replays_the_upstream_transcript	EASY given sv-constructors and push: the spec is about the expanded code (push, from_vec, vec!); Vec; generics	1, 4, 11
sv-drain	contract	SmallVec::drain, Drain::{next,next_back,size_hint,drop}, SmallVec::{remove,truncate}	∀ N ≤ CAP, v ∈ Reach(N), RangeBounds r resolved to (s,e): drain(r) panics iff the bound arithmetic overflows, s > e, or e > len, and items(v) is then unchanged. Otherwise, for any interleaving of next and next_back on the Drain: next yields items[s..e] from the front, next_back from the back, and they never overlap; size_hint = (remaining, Some(remaining)). After the Drain drops: items(v') = items[..s] ++ items[e..], sp(v') = sp(v), and the items not yielded have been dropped front to back. This covers a full drain, a partial take and a by_ref().rev() take.	diff::lcg_operation_fuzz_at_five_inline_sizes, cf::wgpu_forms, up::the_facade_replays_the_upstream_transcript	HARD: a struct holding &'a mut SmallVec (borrowed iterator state); reverse and rotate_left on slices; the Drop impl; the DoubleEndedIterator protocol; RangeBounds/Bound trait dispatch; checked_add; panics	1, 2, 3, 5, 7, 11, 13
sv-drop-order-upstream	refinement	SmallVec::{truncate,clear,retain,retain_mut,resize,from_elem,insert,remove,swap_remove,pop,clone_from,into_vec}, Drain::drop, IntoIter (drop), Clone::clone	∀ op, inputs: Drops(op) = Drops of Upstream. truncate(n) and clear() drop items[n..] last first. retain(f) runs f over every item, then drops the rejected items in the order upstream's swap-then-truncate leaves them (the reverse of the post-swap tail). resize(n,x) with n > len clones x (n−len) times, then drops x once; with n ≤ len it drops like truncate(n). from_elem(e,n) with n ≤ N clones e n times, then drops e. An insert that panics drops its element and nothing else. A dropped Drain drops the items it did not yield, front to back. A dropped IntoIter drops the rest in order. clone_from truncates first, then overwrites by clone_from_slice, dropping each overwritten item.	up::the_facade_replays_the_upstream_transcript	HARD: observable Drop side effects (a trait call per element); Clone side effects; loops; &mut self; panics and unwinding; generics. Needs a written Upstream model first (§5.6)	1, 2, 3, 7, 8, 11, 13
sv-traits-are-slice	equivalence	<SmallVec as Debug>::fmt, <SmallVec as Hash>::hash, <SmallVec<A> as PartialEq<SmallVec<B>>>::eq, <SmallVec as PartialOrd>::partial_cmp, <SmallVec as Ord>::cmp, Deref/DerefMut/AsRef/AsMut/Borrow/BorrowMut/Index	∀ N, M, T, a: SmallVec<[T;N]>, b: SmallVec<[T;M]>, c: SmallVec<[T;N]>, Hasher H: (a == b) ⇔ items(a) == items(b); a.partial_cmp(&c) = items(a).partial_cmp(items(c)); a.cmp(&c) = items(a).cmp(items(c)); a.hash(H) writes to H what items(a).hash(H) writes, and so equals Vec<T>'s hash of the same items; format!("{:?}", a) = format!("{:?}", items(a)); Deref, AsRef, Borrow and Index return items(a) or the indexed sub-slice.	diff::comparison_spans_inline_sizes, diff::lcg_operation_fuzz_at_five_inline_sizes, up::the_facade_replays_the_upstream_transcript	EASY-MEDIUM: straight delegation to the slice, with no loops and no &mut; trait impls generic over two Array types; Hasher as a generic &mut; fmt::Formatter and debug_list; slice Ord/PartialEq/Hash from core (cross-crate citation)	1, 4, 11, 12, 13
```

### Distribution by kind (8)

| kind | count | ids |
|---|---|---|
| contract | 4 | sv-spill-rule, sv-constructors, sv-macro, sv-drain |
| refinement | 2 | sv-ops-refine-vec, sv-drop-order-upstream |
| invariant | 1 | sv-repr-inv |
| equivalence | 1 | sv-traits-are-slice |

### Gap histogram (specs blocked by each gap)

| gap | specs | which |
|---|---|---|
| 1 non-scalar carriers | 8 | all |
| 11 generics | 8 | all |
| 3 E6 body widening | 6 | repr-inv, ops-refine-vec, spill-rule, constructors, drain, drop-order |
| 7 loops | 6 | repr-inv, ops-refine-vec, spill-rule, constructors, drain, drop-order |
| 2 `&mut self` views | 5 | repr-inv, ops-refine-vec, spill-rule, drain, drop-order |
| 12 cross-crate citation | 5 | repr-inv, ops-refine-vec, spill-rule, constructors, traits-are-slice |
| 4 callee-ensures composition | 4 | ops-refine-vec, constructors, macro, traits-are-slice |
| 13 closures, higher-order | 4 | ops-refine-vec, drain, drop-order, traits-are-slice |
| 8 effects (unsafe-block discharge; observable Drop) | 3 | repr-inv, ops-refine-vec, drop-order |
| 5 quantified clauses | 2 | ops-refine-vec, drain |

Gaps 1 and 11 block the whole crate. It is a generic container, so a proof about an uninstantiated
`SmallVec<A>` with a content model of `Vec<T>` is the minimum. Gap 12 is needed because the storage
lives in `aterm-alloc`.

### Proof dependency order

`sv-constructors` and `sv-repr-inv`'s constructor half → `sv-traits-are-slice` (needs only `items`) →
`sv-macro` (free given `sv-constructors` and `push`) → `sv-spill-rule` and `sv-repr-inv`'s preservation
half (two-state over each `&mut` op) → `sv-ops-refine-vec` → `sv-drain` → `sv-drop-order-upstream`
(needs the written Upstream model).

## 2. Test → spec map (TSV; the 9 tests left)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
A test that samples several specs is deleted only when all of its listed specs are Certified.

```tsv
file	test	primary	also
tests/differential.rs	lcg_operation_fuzz_at_five_inline_sizes	sv-ops-refine-vec	sv-repr-inv, sv-spill-rule, sv-drain, sv-traits-are-slice
tests/differential.rs	constructors_follow_upstreams_spill_rules	sv-constructors	
tests/differential.rs	comparison_spans_inline_sizes	sv-traits-are-slice	
tests/upstream_transcript.rs	the_facade_replays_the_upstream_transcript	sv-drop-order-upstream	sv-ops-refine-vec, sv-spill-rule, sv-constructors, sv-macro, sv-drain, sv-traits-are-slice
tests/consumer_forms.rs	rustybuzz_forms	sv-ops-refine-vec	sv-repr-inv, sv-constructors (compile-time surface check stays, §5.2)
tests/consumer_forms.rs	parking_lot_core_forms	sv-ops-refine-vec	(compile-time surface check stays, §5.2)
tests/consumer_forms.rs	wayland_forms	sv-macro	sv-ops-refine-vec, sv-constructors (compile-time surface check stays, §5.2)
tests/consumer_forms.rs	wgpu_forms	sv-drain	sv-ops-refine-vec, sv-constructors, sv-macro (compile-time surface check stays, §5.2)
tests/consumer_forms.rs	send_and_sync_follow_the_item	NON-SPEC (type-system fact)	
```

## 3. Coverage

- 10 tests read. 8 map to the 8 specs. 2 are not spec samples (§4).
- The two broad tests are the main collapse. The LCG fuzz samples 5 specs at 5 inline sizes, and
  the transcript replay samples 7. Neither can be deleted until its last spec is Certified.
  For the replay, that last spec is `sv-drop-order-upstream`, the hardest one.
- The 4 consumer-form tests lose only their `#[test]` attribute and runtime asserts. Their
  compile-time content stays (§5.2).

## 4. Tests that are not spec samples

> Status 2026-10-16: `the_recording_covers_spills_drops_and_panics` is **deleted** (it read only the fixture; the replay still pins the fixture's zero `c0`/`h0` lines). The §5.6 Upstream-model check against the fixture is still owed before any re-recording.

> Status 2026-10-17: its row here and in §2 is gone.

| test | why | action |
|---|---|---|
| consumer_forms::send_and_sync_follow_the_item | `Send` and `Sync` are auto traits, derived structurally through `SmallVec { store: aterm_alloc::SmallVec<T, N> }`. The type checker already guarantees the property, and the fn body has no runtime content. | **delete**. If a regression guard is still wanted, write a compile-time static assertion (a `const _` fn in `lib.rs`). It should not be a `#[test]` or a proof obligation. |

Action count, of the rows left: delete 1. It is not a regression for a removed bug.

## 5. Implied specs that look wrong, or disagree with the code

1. **The test count.** The chunk manifest gave 11 `#[test]` fns, but there are 10. The 11th grep hit is
   the doc comment at `tests/consumer_forms.rs:11` ("The `#[test]`s run the forms too").
2. **The consumer forms mostly check at compile time.** `rustybuzz_forms`, `parking_lot_core_forms`,
   `wayland_forms` and `wgpu_forms` type-check each third-party consumer's API shape against the facade:
   inference of `[_; N]`, `A: Array` generic impls, the `$crate` re-export macro path, and
   `Box<dyn FnOnce>` and `ManuallyDrop` items. Their few runtime asserts are samples of
   `sv-ops-refine-vec`, `sv-constructors`, `sv-macro` and `sv-drain`. No Trust proof replaces the
   API-surface check. So once the specs are proved, keep the forms as compile-only items (non-`#[test]`
   fns or doctests) and delete only the runtime asserts.
3. **`with_capacity` and `reserve`/`reserve_exact` are clamped, but their docs do not say so.** The docs
   say "a heap buffer of that size" and "makes room for `additional`". `aterm_alloc::SmallVec::with_capacity`
   clamps the allocation to 16_777_216 (`crates/aterm-alloc/src/small_vec.rs:57`). Above that size,
   `capacity() >= n` and `capacity() - len >= additional` are false right after the call. The tests
   sample only small `n`, so they cannot see this. The specs carry an `n ≤ CAP` precondition. The
   function docs should state the clamp; the crate-level doc already half does.
4. **`spilled ⇒ capacity() > inline_size()` fails when N > CAP.** `differential.rs::same` asserts this.
   With `N > 16_777_216`, `with_capacity(N+1)` gives a heap `Vec` with capacity 16Mi < N. The tests use only
   `N ≤ 8`, so they cannot see this. `sv-repr-inv` assumes `N ≤ CAP`. That bound could become a
   `const` assertion on `Array`. Another fix is to clamp at `max(N+1, CAP)` in `aterm-alloc`.
5. **A documented divergence that no test exercises.** The crate doc (`src/lib.rs:68`) says that on
   `insert` past the end of a full inline vector, "upstream spills before it checks the index". No test
   exercises that case. The transcript's fixed `insert(3)` panic is on a 2-of-3 vector, which is not
   full, and the fuzz's out-of-bounds inserts are compared against the crate's own `Vec` model, not
   upstream. So nothing confirms that the facade really diverges here. `sv-ops-refine-vec` and
   `sv-spill-rule` pin the facade's behaviour: it panics and leaves `items` and `spilled` unchanged.
6. **The replay is the only link to real upstream.** `the_facade_replays_the_upstream_transcript` is
   the only artifact that ties the `Vec` model, and the drop-order model, to `smallvec` 1.15.1. Once
   proofs against a written model replace it, the crate doc's claim ("contents, lengths, panics and
   element drop order match upstream 1.15.1") depends on that model being right. Keep the recorder
   recipe in `tests/transcript/script.rs`, and check the written model against the fixture once
   before you delete the replay.
7. **Unsafe callees under the proofs.** The storage ops these specs reach in `aterm_alloc`
   (`pop`, `ensure_heap`, `from_elem` in `crates/aterm-alloc/src/small_vec.rs`) are marked
   `#[cfg_attr(trust_verify, trust::skip)]` over unsafe `MaybeUninit` reads. So `sv-repr-inv`,
   `sv-ops-refine-vec` and `sv-drop-order-upstream` also need either unsafe-block discharge (gap 8)
   or a contract on each skipped callee, cited across the crate boundary (gap 12) and proved in
   `aterm-alloc` once gap 8 closes. The first pass of this file left gap 8 off the first two.

## 6. Proof order (easiest high-value first)

No spec here is blocked only by gap 0. Every one needs at least gaps 1 and 11, because the crate is a
generic container. So the order starts with a scalar stepping-stone that the seal alone admits.

0. **Stepping-stone: the capacity arithmetic in `sv-spill-rule` (b).** Gaps 0 and 3 only. Factor out
   `reserve`'s target computation (`len.checked_add(additional).and_then(usize::checked_next_power_of_two)`,
   `src/lib.rs:430-433`) and `aterm_alloc`'s clamp (`capacity.min(16_777_216)`, `small_vec.rs:57`) as pure
   `usize -> Option<usize>` fns. Then prove: `Some(c) ⇒ c ≥ len + additional ∧ c` is a power of two;
   `None ⇔` overflow; `min(c, CAP) ≤ CAP`. Scalar `omega` work like this retires no test by itself.
   It is the arithmetic core of `sv-spill-rule`, and it is where the CAP precondition (§5.3, §5.4) gets
   a proof.
1. **`sv-traits-are-slice`.** Gaps 1, 4, 11, 12 and 13. Every impl is a one-line delegation to
   `as_slice()`, with no loops and no `&mut self`. It retires `comparison_spans_inline_sizes` and is the
   first generic proof over `A: Array`.
2. **`sv-constructors`.** Gaps 1, 3, 4, 7, 11 and 12. These are value constructors with no
   `&mut self`, so gap 2 is not needed. It retires `constructors_follow_upstreams_spill_rules` and
   establishes `Inv` at every root of `Reach`.
3. **`sv-macro`.** Gaps 1, 4 and 11. It is free given `sv-constructors` and `push`. It removes the
   runtime asserts from `wayland_forms`.

Then come `sv-repr-inv` with `sv-spill-rule` (a two-state proof over each `&mut` op, gap 2), then
`sv-ops-refine-vec` (the fuzz's main spec; both need gap 8 or a cited contract for the unsafe
`aterm_alloc` callees, §5.7), then `sv-drain`, and last `sv-drop-order-upstream` (it needs
the written Upstream model and gap 8). The fuzz and the replay go only when the last of their specs is Certified.
