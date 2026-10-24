<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-spec-macros: spec recovery from its 9 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 4 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-16: this is the crate's migration worklist. Each of the 7 specs below is
> proved through Trust/Clean about the compiled Rust functions, and then the tests it
> subsumes are deleted. No test has been deleted yet.

Source: `crates/aterm-spec-macros` (read only). All 9 `#[test]` fns are in
`crates/aterm-spec-macros/tests/derive_and_attrs.rs`; `src/lib.rs` has none. The macro side is
`crates/aterm-spec-macros/src/lib.rs` (`refines`, `spec_unmodeled`,
`inject_entry_probe`, `entry_id_for`, `item_fn_ident`, `sanitize_ident`), with the fn reader
`FnItem` in `src/item.rs`; since SP.8 (docs/AUDIT-third-party-2026-10-15.md §7) both read std
`proc_macro` tokens through `aterm-macro-support`, not syn. The runtime side is
`crates/aterm-spec-anchor/src/xref.rs` and `crates/aterm-spec-anchor/src/registry.rs`. Nothing was compiled or
run. The claims below come from reading the code.

Result: **7 specs**, covering **all 9 tests**. **0 tests are not spec samples** (§4).

Gap numbers in this file are the rows of the gap table in
`AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit) §7.

## 0. Notation used in the statements

- `Expand_R(M,A,P,F)`: the token output of `#[refines(machine=M, action=A, project=P)]` on item `F`.
  `Expand_U` is the same for `#[spec_unmodeled]`.
- `Probe(id)`: the statement `::aterm_spec_anchor::xref::note_entered(id);`.
- `Probeable(F)`: `F` is a fn with a `{ .. }` body and no `const` (`FnItem::fn_with_body` returns
  `Some(false)`). Before SP.8 this was: `F` parses as `syn::ImplItemFn` or `syn::ItemFn`, and
  `sig.constness` is `None`.
- `Prepend(F,s)`: `F` with `block.stmts := [s] ++ F.block.stmts`. Attributes (inner `#![..]`
  included), visibility and signature are unchanged.
- `f`: `item_fn_ident(F).to_string()`.
- `Eid(M,A,f)` = `format!("{M}::{A} @ {f}")` (`entry_id_for`).
- `W_t = (armed: bool, S: Set<&'static str>)`: the abstract evidence-window state of thread `t`, made of
  the `ARMED` `Cell<bool>` and the `ENTERED_ANCHORS` `RefCell<BTreeSet>` thread-locals.
- `Live(t)`: the TLS slots of `t` are not destroyed and no borrow of `ENTERED_ANCHORS` is outstanding.
- `Sem(F)(args, σ)`: what a call to `F` does that the caller can observe (return value, effects on
  `self` and `&mut` arguments, panic, divergence), leaving `W_t` out.
- `Reg_T(B)`: the multiset of `T` records (`RefinementAnchor`, `Waiver`) that
  `registry::iter::<T>()` yields in binary `B`. `Sub_T(B)`: the multiset of `registry::submit!{T => ..}`
  expansions linked into `B`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
anchor-entry-id	contract	aterm_spec_macros::{refines,entry_id_for,inject_entry_probe}	∀ M,A,P,F with fn ident f: the RefinementAnchor that Expand_R submits has machine=M, action=A, rust_method=f, project=P. Its entry_id is Eid(M,A,f) when Probeable(F), and is the very literal passed to the injected Probe; otherwise entry_id = "" and no probe is injected. Corollaries: a probed anchor's entry_id ends with " @ f"; an anchor with entry_id "" records nothing when F is called.	probe_records_the_anchor_id_the_record_carries, a_const_fn_anchor_reports_itself_unprobed_rather_than_lying	EASY-MEDIUM: String format!, Option match, q! literal interpolation; lives in a proc-macro crate	0, 1, 9
macro-expansion-shape	contract	aterm_spec_macros::{refines,inject_entry_probe,spec_unmodeled,item_fn_ident}	∀ M,A,P and every item token stream F: inject_entry_probe(F,id) = Some(Prepend(F,Probe(id))) when Probeable(F), None otherwise. So the item part of Expand_R(M,A,P,F) is Prepend(F,Probe(Eid(M,A,f))) when Probeable(F), and F verbatim otherwise. Expand_U emits F verbatim. Every expansion adds exactly one uniquely named associated const of type () holding one registry submit, and no other item.	-	MEDIUM: proc_macro::TokenStream token trees (FnItem::fn_with_body scans the item; item::prepend splices the probe into the body group after its inner attributes, every other token passed through), q!, format!; needs a token-tree model (no syn parse/print round-trip since SP.8)	0, 1, 3, 9
evidence-window-state-machine	invariant	aterm_spec::xref::{note_entered,reset_entered_anchors,disarm_entered_anchors,entered_anchor_ids,entered_anchor_count,window_is_armed}	∀ t with Live(t), ∀ W_t=(a,S): note_entered(id) moves W_t to (a, a ? S∪{id} : S). reset_entered_anchors() moves W_t to (true, ∅) and returns true. disarm_entered_anchors() moves W_t to (false, S); idempotent, keeps S. entered_anchor_ids() returns S; entered_anchor_count() returns Some(|S|); window_is_armed() returns Some(a); readers leave W_t unchanged. When ¬Live(t): reset returns false, entered_anchor_count and window_is_armed return None. No function panics in either case. (The doc comment's "one bool load when unarmed" is a cost claim, not part of this spec.)	-	MEDIUM: thread_local! Cell<bool> and RefCell<BTreeSet<&'static str>>, LocalKey::try_with closures, try_borrow/try_borrow_mut, BTreeSet clear/insert/len/clone; a two-state ghost view of TLS. All six fns carry #[cfg_attr(trust_verify, trust::skip)] today	0, 1, 2, 5, 8, 13
probed-call-records-its-anchors	contract	aterm_spec_macros::refines, aterm_spec::xref::note_entered	∀ probeable F carrying k ≥ 1 refines anchors with ids I = {Eid(M_i,A_i,f)}, ∀ t with Live(t) and W_t=(a,S): a call to F that reaches its body leaves W_t' = (a, a ? S∪I : S), for every k (stacked expansions each prepend their own probe, outside-in). Before F is called no id in I is added. This is the composition of macro-expansion-shape, anchor-entry-id and evidence-window-state-machine.	every_anchor_of_a_multi_anchored_fn_is_recorded_by_one_call	MEDIUM given its three lemmas: callee-ensures composition across a macro expansion, TLS state, a &mut self method	0, 1, 2, 4, 8, 9
step-evidence-refuses-nesting	contract	aterm_spec::xref::StepEvidence::step	∀ machine m, action, drive closure d, with Live(t): step(action,d) runs reset; panics unless reset returned true and entered_anchor_count()=Some(0); runs d. It panics with a message containing "was CLOSED while this step ran" iff window_is_armed() ≠ Some(true) after d returns. Otherwise it pushes StepRecord{action, entered: S after d}, leaves armed=false and returns d's value. Corollary: any d that calls StepEvidence::step or disarm_entered_anchors and returns normally makes the outer step panic.	a_nested_window_panics_instead_of_lending_the_outer_step_its_evidence	MEDIUM-HARD: generic FnOnce parameter, panic as the specified outcome (should_panic → requires, audit §8), &mut self Vec::push, TLS, re-entry through a closure	0, 1, 2, 4, 8, 11, 13
registry-collects-each-submit-once	invariant	aterm_spec::registry::{submit,iter}, aterm_spec::xref::{refinements,waivers}	∀ binary B on a supported target (Mach-O, ELF, Windows), ∀ T ∈ {RefinementAnchor, Waiver}: once main starts, iter::<T>() yields Reg_T(B) = Sub_T(B) as multisets. Every submitted record appears exactly once and nothing else appears.	-	HARD: unsafe link_section load-time constructors (__mod_init_func/.init_array/.CRT$XCU), AtomicPtr intrusive list with Acquire/Release, generic T: Collect, raw-pointer as_ref; a whole-program linker property	0, 1, 8, 11, 13
probe-preserves-semantics	refinement	aterm_spec_macros::refines, aterm_spec::xref::note_entered	∀ fn F that type-checks, ∀ M,A,P, args, caller state σ: Sem(Expand_R(M,A,P,F))(args,σ) = Sem(F)(args,σ). Covers early return, generics, recursion, self by value, &mut self, inner attributes, diverging bodies and n-fold stacking of refines on one fn. Proof sketch: Prepend inserts only Probe(id), and note_entered (a) never panics or diverges, for any W_t and whether or not Live(t), and (b) writes only W_t. For a const fn Expand_R leaves F verbatim, so F stays const-evaluable. This is the semantic half of macro-expansion-shape.	probe_preserves_behaviour_of_every_awkward_fn_shape	VERY HARD: a statement over the semantics of all Rust fns, quantified over syntax (needs a kernel Rust semantics, proven-core SemR); thread_local!, LocalKey::try_with closures, RefCell::try_borrow_mut, BTreeSet::insert, panic-freedom	0, 1, 2, 3, 4, 8, 9, 11, 13
```

### Distribution by kind (7)

| kind | count |
|---|---|
| contract | 4 |
| invariant | 2 |
| refinement | 1 |

### Gap histogram (specs blocked by each audit §7 gap)

| gap | specs |
|---|---|
| 0 | 7 |
| 1 | 7 |
| 2 | 4 |
| 3 | 2 |
| 4 | 3 |
| 5 | 1 |
| 8 | 5 |
| 9 | 4 |
| 11 | 3 |
| 13 | 4 |

No spec is blocked by gap 0 alone: every one needs non-scalar carriers (gap 1), because the data
is `&str`, `String`, token streams or a `BTreeSet`.

### Proof dependency order

`anchor-entry-id` and `macro-expansion-shape` are the macro-side lemmas.
`evidence-window-state-machine` is the runtime-side lemma. `probed-call-records-its-anchors`
composes all three. `step-evidence-refuses-nesting` uses `evidence-window-state-machine`.
`probe-preserves-semantics` uses `macro-expansion-shape` plus the panic-freedom half of
`evidence-window-state-machine`. `registry-collects-each-submit-once` is independent of the rest.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
All paths are under `crates/aterm-spec-macros/`.

```tsv
file	test	primary	also
tests/anchors/suite.rs	probe_preserves_behaviour_of_every_awkward_fn_shape	probe-preserves-semantics	macro-expansion-shape
tests/anchors/suite.rs	probe_records_the_anchor_id_the_record_carries	anchor-entry-id	evidence-window-state-machine, probed-call-records-its-anchors
tests/anchors/suite.rs	every_anchor_of_a_multi_anchored_fn_is_recorded_by_one_call	probed-call-records-its-anchors	anchor-entry-id
tests/anchors/suite.rs	a_const_fn_anchor_reports_itself_unprobed_rather_than_lying	anchor-entry-id	macro-expansion-shape
tests/anchors/suite.rs	a_nested_window_panics_instead_of_lending_the_outer_step_its_evidence	step-evidence-refuses-nesting	evidence-window-state-machine
```

## 3. Coverage

- 9 tests read. All 9 map to one of the 7 specs. None is a non-spec check (§4).
- The spec recovery input reported 10 tests. The file has 9 `#[test]` fns; a plain grep finds a
  tenth `#[test]` inside the comment at `tests/derive_and_attrs.rs:75`. This table counts 9.
- `probe-preserves-semantics` has 1 primary test but is the most general spec here: it is the
  property the stale "unchanged" wording in `src/lib.rs` (§5.2) should name.
- The test helpers (`early_return`, `generic_identity`, `recursive_sum`, `inner_attr_body`,
  `open_window`, `anchor_entry_id`) are the annotated fixtures and the harness, not tests.

## 4. Tests that are not spec samples

None. Every test samples one of the specs above.

| action | count |
|---|---|
| delete | 0 |
| become the spec | 0 |
| keep | 0 |

## 5. Implied specs that look wrong, or disagree with the code

1. **A vacuous final check.** `a_const_fn_anchor_reports_itself_unprobed_rather_than_lying` ends with
   `assert!(entered_anchor_ids().is_empty())`. That is the form the file's own `open_window` comment
   rejects: it also passes on a destroyed TLS slot or an outstanding borrow. It should assert
   `entered_anchor_count() == Some(0)`, which is what `evidence-window-state-machine` states.
2. **Stale "unchanged" wording.** The anchor-emitter comment in `src/lib.rs` still says the
   annotated item "is left 100% unchanged at runtime". That is false for `refines`, which prepends a
   `note_entered` probe to every non-const fn body. Only `spec_unmodeled` is a token pass-through.
   The spec is "semantics preserved" (`probe-preserves-semantics`), not "tokens unchanged".

## 6. Proof order (easiest, high value first)

No spec is blocked by gap 0 alone, so the order goes by the fewest and smallest remaining gaps.

1. **`anchor-entry-id`** (gaps 0, 1, 9). `entry_id_for` is a pure `format!` over three strings, and the
   rest is an `Option` match. It retires 2 tests and fixes the record contents every other spec uses.
   Moving `entry_id_for`, `sanitize_ident` and the `Probeable` decision into a plain library crate that
   the proc-macro crate calls would drop gap 9 for this spec and for `macro-expansion-shape`.
2. **`evidence-window-state-machine`** (gaps 0, 1, 2, 5, 8, 13). Six small fns over two thread-locals
   with an obvious two-state view. It needs removing the `trust::skip` on all six, a set-valued carrier
   and a TLS effect model. It retires 2 tests and is the lemma for three other specs.
3. **`macro-expansion-shape`** (gaps 0, 1, 3, 9). Syntactic and finite: one splice into the body
   group after its inner attributes (`item::prepend`). It needs a token-tree model. It retires 1 test
   and is the syntactic half of `probe-preserves-semantics`.
4. **`probed-call-records-its-anchors`** (gaps 0, 1, 2, 4, 8, 9). Composition only, once 1–3 hold.
5. **`step-evidence-refuses-nesting`** (gaps 0, 1, 2, 4, 8, 11, 13). Restate the `should_panic` as a
   `requires` on `step` (audit §8).
6. **`registry-collects-each-submit-once`** (gaps 0, 1, 8, 11, 13). A whole-program linker property
   over unsafe load-time constructors.
7. **`probe-preserves-semantics`** (gaps 0, 1, 2, 3, 4, 8, 9, 11, 13). Quantifies over the semantics of
   every Rust fn, so it waits on proven-core SemR. Until then, keep its test.
