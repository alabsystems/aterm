<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-once-cell: spec recovery from its 23 unit tests

> Status: migration worklist. Each of the 8 specs below is proved through Trust/Clean, and then the
> tests it subsumes are deleted. Seven of the eight §4 non-spec checks (the racy-cell negative
> control and the six `consumers.rs` dependency-graph guards) are deleted; one remains in §4.

Source: `crates/aterm-once-cell` (`src/lib.rs` and `tests/behaviour.rs`).

Result: **8 specs**, covering **15 tests**. **8 tests are not spec samples** (§4).

## 0. Notation used in the statements

- `Abs(c) : Option<T>` is the abstract state of a cell.
  - For `sync::OnceCell` it is `c.inner.get()` (a `std::sync::OnceLock`).
  - For `unsync::OnceCell` it is `c.0.get()` (a `core::cell::OnceCell`).
  - For `race::OnceBox<T>` it is the inner `Option<Box<T>>` read through `Box` deref.

  Every statement is up to that deref.
- `CellModel(T)` is the sequential state machine over `Option<T>`:
  - `new()` and `default()` give `None`. `with_value(v)` and `From(v)` give `Some v`.
  - `get()` returns `Abs` and leaves it unchanged. `get_mut()` returns it as `&mut`.
  - `set(v)`: from `None`, the state becomes `Some v` and the call returns `Ok(())`. From `Some u`, the state stays and the call returns `Err(v)`.
  - `try_insert(v)`: from `None`, the state becomes `Some v` and the call returns `Ok(&v)`. From `Some u`, the state stays and the call returns `Err((&u, v))`.
  - `get_or_init(f)`: from `Some u`, `f` is not called and the result is `&u`. From `None`, `f` is called exactly once, and the state becomes `Some(f())`, which is returned.
  - `get_or_try_init(f)`: from `Some u`, `f` is not called and the result is `Ok(&u)`. From `None`, `f` is called once. `Ok v` gives the state `Some v` and returns `Ok(&v)`. `Err e` leaves the state `None` and returns `Err e`.
  - `take()` returns the old `Abs` and leaves `None`. `into_inner()` returns `Abs`.
- `Calls(f)` counts how many times the user closure `f` was invoked. It is a ghost counter.
- `LazyModel` has the state `(Abs(cell), init : Option<F>)`, with the invariant `Abs(cell) = None ⇒ init = Some _`:
  - `new(f)` gives `(None, Some f)` and does not call `f`.
  - `get` returns `Abs(cell)`.
  - `force`, `Deref`, `force_mut` and `DerefMut`, from `None`: take `init`, call it once, store the result and return a reference to it. From `Some v`, they call nothing and return `&v`.
  - `into_value` returns `Ok(v)` when `Abs = Some v`, and `Err(init)` otherwise.
- `Hist` is any linearizable concurrent history of calls, from any number of threads, on one
  `sync::OnceCell`, or on a `sync::Lazy` or `race::OnceBox` built on it. The concurrent specs
  are stated over every such interleaving.
- Gap numbers refer to the table in
  `AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit) §7.
  Gap 0 (a seal that publishes Clean rows) is a prerequisite for every row, so it is listed on each.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps (audit §7)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
sync-oncecell-refines-cellmodel	refinement	sync::OnceCell::{new,default,with_value,from,get,get_mut,set,try_insert,get_or_init,get_or_try_init,take,into_inner}	∀ T, every sequential sequence of operations, and every closure f that does not panic and does not touch the cell: each operation's result, post-state Abs and Calls(f) are exactly what CellModel(T) gives from the pre-state Abs (Calls(f) = 0 from Some, 1 from None). In particular, a failed get_or_try_init leaves Abs = None, so a later call can retry. After any success, Abs never changes until take or into_inner.	behaviour::a_failed_initialiser_leaves_the_cell_empty_and_retryable, set_is_single_assignment_and_reports_the_loser, take_and_into_inner_empty_the_cell	MEDIUM: generics over T, F and E; FnOnce closures (a higher-order spec with a ghost call counter); Option and Result returns that carry references; &self and &mut self views of OnceLock; library models of std::sync::OnceLock and of Mutex lock/unlock; PoisonError::into_inner; a match on the uninhabited Infallible	0,1,2,4,5,8,11,13
unsync-oncecell-refines-cellmodel	refinement	unsync::OnceCell::{new,default,with_value,from,get,get_mut,set,try_insert,get_or_init,get_or_try_init,take,into_inner}	∀ T, every sequence of operations, and every closure f that does not panic and does not reenter the cell: each operation's result, post-state Abs and Calls(f) are what CellModel(T) gives. With sync-oncecell-refines-cellmodel, this makes the two cells observationally equal on single-threaded, non-reentrant histories.	behaviour::unsync_once_cell_matches_its_sync_sibling	MEDIUM: generics; FnOnce closures; Option and Result; &self and &mut self; a contract for core::cell::OnceCell, cited from core; the try_insert trick, a closure that captures and takes from a mutable Option	0,1,2,4,5,11,12,13
oncebox-refines-cellmodel	refinement	race::OnceBox::{new,default,with_value,get,set,get_or_init,get_or_try_init}	∀ T: OnceBox<T> refines CellModel(Box<T>). get returns Abs mapped through Box deref. set(b) returns Ok(()) from None, and from Some returns Err(b) carrying the identical box that was passed in (same allocation), so the box's identity is preserved. get_or_init and get_or_try_init follow CellModel, and a failed get_or_try_init leaves Abs = None. Each clause follows from sync-oncecell-refines-cellmodel by composition.	behaviour::once_box_is_a_single_assignment_cell_over_a_box	EASY given the sync spec: Box ownership and deref; closures; composition with the contracts of sync::OnceCell; Option::map and Result::map on references; generics	0,1,4,11,12,13
sync-init-exactly-once-concurrent	invariant (concurrent)	sync::OnceCell::{get_or_try_init,get_or_init,set,try_insert}, sync::Lazy::{force,deref}, race::OnceBox::get_or_init	∀ Hist on one sync::OnceCell from any number of threads: (a) the gate serialises user initialisers, so at most one runs at any instant; (b) at most one initialiser ever returns Ok, and after one has, no initialiser is invoked again; (c) every call that returns a value returns a reference to the one value Abs holds, and a call whose own closure ran and returned Ok is the call that set Abs. Hence, on an empty cell where every thread's closure succeeds, exactly one closure runs: concurrent sync::Lazy::force calls invoke init exactly once, and race::OnceBox::get_or_init invokes f exactly once. Negative control: removing the gate (leaving check, compute, set) must make the proof fail.	behaviour::get_or_try_init_runs_the_closure_exactly_once_under_contention, lazy_forced_concurrently_runs_its_initialiser_once, once_box_runs_its_initialiser_once_under_contention	HARD: threads; a Mutex used as a critical section; OnceLock's acquire ordering; a concurrent (C1/linearizability) model, most likely a ty_model! of the gate protocol bound to the code by a conformance test; closures; generics; Arc sharing	0,2,8,10,11,13
sync-panic-leaves-cell-empty-and-unpoisoned	contract	sync::OnceCell::{get_or_try_init,get_or_init}	∀ sync::OnceCell c with Abs(c) = None and ∀ closure f that panics: the panic propagates out of get_or_try_init or get_or_init, and Abs(c) is still None afterwards. Every later operation on c, from any thread, then behaves as CellModel from None. A poisoned gate mutex never causes a panic or a block, because every lock site maps PoisonError::into_inner.	behaviour::a_panicking_initialiser_leaves_the_cell_usable	HARD: unwinding semantics (a panic edge out of a closure call, guard drop during unwind); the poison state of Mutex; threads (the test panics on a spawned thread); closures	0,2,8,13
lazy-refines-lazymodel	refinement	sync::Lazy::{new,get,get_mut,force,force_mut,deref,deref_mut,into_value,default}, unsync::Lazy::{new,get,get_mut,force,force_mut,deref,deref_mut,into_value,default}	∀ T and ∀ F: FnOnce() -> T that does not panic, every sequential sequence of operations on a sync::Lazy or unsync::Lazy refines LazyModel. new makes no call of init (Calls(init) = 0), and get returns None until the first force or deref. The first force, deref, force_mut or deref_mut calls init exactly once. Every later one returns a reference equal to that first result, and Calls(init) stays 1. into_value returns Ok(v) iff the Lazy was forced, and Err(init) otherwise. That new is a const fn with no bound on F is a compile-time fact; the const _ block in behaviour.rs already pins it.	behaviour::lazy_does_not_run_its_initialiser_until_forced, unsync_lazy_forces_once_and_derefs, lazy_new_is_unbounded_in_f_the_way_wasm_bindgen_needs, lazy_is_sync_when_upstream_says_it_is	MEDIUM given the cell specs: generics with a default type parameter F = fn() -> T; closures; Cell<Option<F>>::take and Mutex<Option<F>>::take; Deref and DerefMut impls; &self and &mut self; composition with the cell refinements; statics (the Sync test uses a static Lazy)	0,1,2,4,8,11,13
unsync-reentrant-init-panics	contract	unsync::OnceCell::{get_or_try_init,get_or_init}	∀ unsync::OnceCell c with Abs(c) = None and ∀ closure f that fills c (through set, try_insert or get_or_init on c) and then returns Ok(v) to get_or_try_init or v to get_or_init: the outer call panics and does not return. So v is never silently dropped, and the caller never receives a value its own closure did not produce. The spec requires only that the call panics: get_or_try_init panics with the crate's own assert!, and get_or_init with core::cell::OnceCell's reentrancy panic.	behaviour::unsync_get_or_try_init_refuses_a_reentrant_initialisation	MEDIUM: closures that capture &self of the cell being initialised (higher-order reentrancy); panic and assert semantics; the reentrancy contract of core::cell::OnceCell; generics	0,2,8,11,12,13
cell-debug-eq-clone-shapes	contract	sync::OnceCell::{fmt,eq,clone,clone_from}, unsync::OnceCell::{fmt,eq,clone,clone_from}, sync::Lazy::fmt, unsync::Lazy::fmt, race::OnceBox::{fmt,clone}	∀ cell c: format!("{c:?}") is "OnceCell(Uninit)" when Abs(c) = None, and otherwise the debug_tuple("OnceCell").field(v) rendering of Some v. Lazy's Debug is debug_struct("Lazy") with the fields cell (that cell's Debug) and init (".."), and it never calls init. OnceBox's Debug is "OnceBox({p:?})", where p is null iff Abs = None and is the box's address otherwise. a == b ⇔ Abs(a) == Abs(b). Abs(c.clone()) = Abs(c).cloned(). After d.clone_from(&s), Abs(d) = Abs(s).cloned().	behaviour::debug_and_eq_match_the_upstream_shapes	EASY-MEDIUM, with no closures and no threads: core::fmt Formatter, debug_tuple and debug_struct (String output); the Debug, PartialEq and Clone impls; Debug formatting of a raw pointer; generics bounded on T: Debug, PartialEq or Clone	0,1,3,4,11,12
```

### Distribution by kind (8)

| kind | count | ids |
|---|---|---|
| refinement | 4 | sync-oncecell-refines-cellmodel, unsync-oncecell-refines-cellmodel, oncebox-refines-cellmodel, lazy-refines-lazymodel |
| contract | 3 | sync-panic-leaves-cell-empty-and-unpoisoned, unsync-reentrant-init-panics, cell-debug-eq-clone-shapes |
| invariant (concurrent) | 1 | sync-init-exactly-once-concurrent |

### Blocking-gap histogram (specs blocked by each gap)

| gap | specs | what it is here |
|---|---|---|
| 0 | 8 | a seal that publishes Clean rows (prerequisite for all) |
| 1 | 5 | `Option`/`Result`/`Box`/`String` carriers in clauses |
| 2 | 6 | certified `&self`/`&mut self` views and the two-state `Abs` before and after a call |
| 3 | 1 | calls and matches in reflected bodies (the `fmt` impls) |
| 4 | 5 | composing callee `ensures` (`OnceLock`, `core::cell::OnceCell`, the cell under `Lazy`/`OnceBox`) |
| 5 | 2 | quantified clauses ("every sequence of operations") |
| 8 | 5 | `Mutex`, poisoning, unwinding and panic edges |
| 10 | 1 | binding a `ty_model!` of the gate protocol to the code |
| 11 | 7 | generic `T`/`F`/`E`: every function in this crate is generic |
| 12 | 4 | citing contracts from `core`/`std` |
| 13 | 7 | closures (`FnOnce` higher-order specs) and threads |

No spec is blocked by gap 0 alone. Every operation in the crate is generic and takes or stores a
closure, so gaps 11 and 13 cover almost everything here.

### Proof dependency order

`unsync-oncecell-refines-cellmodel` and `sync-oncecell-refines-cellmodel` (the sequential
CellModel) → `oncebox-refines-cellmodel` and `lazy-refines-lazymodel` (by composition) →
`cell-debug-eq-clone-shapes` (uses Abs from the refinements) → `unsync-reentrant-init-panics` and
`sync-panic-leaves-cell-empty-and-unpoisoned` (panic edges) → `sync-init-exactly-once-concurrent`
(the concurrent model).

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples. The
tests deleted since have no row.

```tsv
file	test	primary	also
tests/behaviour.rs	get_or_try_init_runs_the_closure_exactly_once_under_contention	sync-init-exactly-once-concurrent	sync-oncecell-refines-cellmodel
tests/behaviour.rs	a_failed_initialiser_leaves_the_cell_empty_and_retryable	sync-oncecell-refines-cellmodel	
tests/behaviour.rs	set_is_single_assignment_and_reports_the_loser	sync-oncecell-refines-cellmodel	
tests/behaviour.rs	take_and_into_inner_empty_the_cell	sync-oncecell-refines-cellmodel	
tests/behaviour.rs	debug_and_eq_match_the_upstream_shapes	cell-debug-eq-clone-shapes	
tests/behaviour.rs	lazy_does_not_run_its_initialiser_until_forced	lazy-refines-lazymodel	
tests/behaviour.rs	unsync_lazy_forces_once_and_derefs	lazy-refines-lazymodel	
tests/behaviour.rs	lazy_new_is_unbounded_in_f_the_way_wasm_bindgen_needs	lazy-refines-lazymodel	(compile-time half already pinned by the const _ block)
tests/behaviour.rs	the_unwind_safety_surface_matches_upstream	NON-SPEC (trait-solver fact; becomes a const item)	
tests/behaviour.rs	lazy_is_sync_when_upstream_says_it_is	lazy-refines-lazymodel	(the Sync half is a trait-solver fact; move it to a const item)
tests/behaviour.rs	lazy_forced_concurrently_runs_its_initialiser_once	sync-init-exactly-once-concurrent	lazy-refines-lazymodel
tests/behaviour.rs	once_box_is_a_single_assignment_cell_over_a_box	oncebox-refines-cellmodel	
tests/behaviour.rs	once_box_runs_its_initialiser_once_under_contention	sync-init-exactly-once-concurrent	oncebox-refines-cellmodel
tests/behaviour.rs	unsync_once_cell_matches_its_sync_sibling	unsync-oncecell-refines-cellmodel	sync-oncecell-refines-cellmodel
tests/behaviour.rs	unsync_get_or_try_init_refuses_a_reentrant_initialisation	unsync-reentrant-init-panics	
tests/behaviour.rs	a_panicking_initialiser_leaves_the_cell_usable	sync-panic-leaves-cell-empty-and-unpoisoned	sync-oncecell-refines-cellmodel
```

## 3. Coverage

- I read all 23 tests. 15 map to one of the 8 specs, and 8 are not spec samples (§4).
- The biggest collapses are `lazy-refines-lazymodel` (4 primary), `sync-oncecell-refines-cellmodel`
  (3 primary, plus 4 secondary) and `sync-init-exactly-once-concurrent` (3 primary).
- `sync-oncecell-refines-cellmodel` is the base the other specs rest on. `oncebox-refines-cellmodel`
  and the `sync::Lazy` half of `lazy-refines-lazymodel` are compositions over it.

## 4. Tests that are not spec samples

| test | why | action |
|---|---|---|
| behaviour::the_unwind_safety_surface_matches_upstream | The trait solver decides this at compile time, and the runtime body does nothing. Move the `rus`/`us` assertions into a module-level `const _: () = { .. }` or `fn _assert()` item, like the existing `WasmBindgenShapedCell` const block, so rustc itself is the proof. Then delete the `#[test]`. | **delete** (after moving the assertions to a const item) |

Counts by action: delete 1.

## 5. Implied specs that look wrong, or disagree with the code

1. **The crate doc claims `Lazy` recovers from a panic, but the code does not.** The "Poisoning"
   section of `src/lib.rs` says a panic inside an initialiser leaves the cell empty and
   retryable. That holds for `sync::OnceCell`. `sync::Lazy` and `unsync::Lazy`, however, take `init`
   before calling it, so after a panic they are permanently poisoned, and every later force panics
   with "Lazy instance has previously been poisoned". This matches upstream, but no test covers it,
   and the doc reads as if it covered `Lazy` too. Recommendation: either narrow the doc or add a
   clause to `lazy-refines-lazymodel`: a panicking `init` leaves `(None, None)`, and every later
   force panics.
2. **The contention tests are probabilistic.** The three contention tests widen the race window
   with `thread::sleep`. That does not affect their passing direction, but they catch a broken cell
   only by chance. The proof must not inherit this: its negative control is a mutation (remove the
   gate), not a race.
3. **Prose-pinning on std's panic message.** `unsync_get_or_try_init_refuses_a_reentrant_initialisation`
   pins the panic text "reentrant init". In the `get_or_init` and control cases that text comes from
   `core::cell::OnceCell`, not from this crate. The spec requires only that the call panics.
4. **Pinning std's pointer formatting.** `debug_and_eq_match_the_upstream_shapes` asserts that the
   Debug output of a null `OnceBox` is exactly `"OnceBox(0x0)"`. That pins std's `{:p}` rendering,
   not a property of this crate. The spec says only that the pointer is null iff `Abs = None`. The
   `Lazy` Debug field layout it checks is a legitimate compatibility contract with upstream, and it
   stays in the spec.
5. **Two tests in the map mix a compile-time fact with a runtime sample.**
   `lazy_new_is_unbounded_in_f_the_way_wasm_bindgen_needs` and `lazy_is_sync_when_upstream_says_it_is`
   mainly assert trait or const facts, and only their final `assert_eq!` samples `lazy-refines-lazymodel`.
   Move the `Sync` assertion into a `const _` item like the existing one, then delete both tests once
   the Lazy spec is proved.

## 6. Proof order (easiest high-value first)

No spec here is blocked by gap 0 alone. Every function is generic (gap 11) and almost every one takes
a closure (gap 13). So the first step is the Trust work on those two gaps. Until it lands, a
monomorphic instance (`T = u32`, a concrete closure type) can serve as a stepping stone, but it does
not retire a test.

1. **`unsync-oncecell-refines-cellmodel`**: sequential, no threads and no `Mutex`. It is a thin
   wrapper over `core::cell::OnceCell` (gaps 1, 2, 4, 11, 12, 13). It establishes CellModel and the
   ghost `Calls(f)` pattern that every other spec reuses.
2. **`sync-oncecell-refines-cellmodel`**: the same CellModel over `OnceLock` plus the gate. Stated
   sequentially, it needs only a library model of `Mutex` lock/unlock (gap 8). It retires 3 tests and
   is the contract wgpu-core depends on.
3. **`oncebox-refines-cellmodel`** and **`lazy-refines-lazymodel`**: compositions over steps 1 and 2
   (gap 4). Together they retire 5 tests.
4. **`cell-debug-eq-clone-shapes`**: it needs no closures and no threads, but it does need models of
   `core::fmt` (gaps 1, 3, 12). It is cheap once the carriers exist.
5. **`unsync-reentrant-init-panics`**, then **`sync-panic-leaves-cell-empty-and-unpoisoned`**: these
   need panic and unwind edges (gap 8).
6. **`sync-init-exactly-once-concurrent`**: the most valuable spec and the hardest. Author the gate
   protocol with `ty_model!` now, check it, and bind it to the code with a conformance test whose
   negative control removes the gate (`crates/aterm-spec/src/derive.rs`). That can land before
   Trust's concurrency lane (gaps 10, 13). Do not delete the three contention tests until the
   proof about the compiled code is Certified.

Do the §4 action now. It needs no proof: move the unwind-safety and `Sync` assertions into
`const _` items and delete `the_unwind_safety_surface_matches_upstream`.
