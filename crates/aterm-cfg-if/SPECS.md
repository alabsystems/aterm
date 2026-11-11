<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-cfg-if: spec recovery from its unit tests

The crate's migration worklist: each spec is to be proved through Trust/Clean, and the tests it
subsumes then deleted. The claims come from reading the code, not from running it.

The directory is `aterm-cfg-if`, the package `cfg-if`, the lib `cfg_if`. The crate has no
runtime function: its whole surface is one `macro_rules! cfg_if` in `src/lib.rs`, with three rule
families (the entry rule, the `@__arms` accumulator with its empty base case, and the `@__group`
splice). The lib sets `test = false`, so the tests are `tests/differential.rs` and the macro's
doctests (§3).

## 0. Notation used in the statements

- A chain `C = [(p_1,T_1), …, (p_n,T_n)]`, `n ≥ 1`, with an optional else body `T_e`. Each `p_i`
  is a cfg predicate token tree; each `T_i` is a token sequence (items, statements or one
  expression).
- `cfg(p)` is rustc's evaluation of `p` for the target being compiled, the value `cfg!(p)` gives.
  `cfg(all()) = true` and `cfg(any()) = false` on every target.
- `sel(C) = min{ i | cfg(p_i) }`; if no `p_i` holds, `sel(C) = else` when `T_e` is present and
  `none` when it is absent.
- `E(C)` is what `cfg_if::cfg_if!{ if #[cfg(p_1)]{T_1} else if … else {T_e} }` leaves after
  cfg-stripping. The entry rule normalizes the arms into `((p_i) (T_i))` pairs, the else arm with
  an empty predicate; `@__arms` emits arm `i` under
  `g_i = #[cfg(all(p_i, not(any(p_1, …, p_{i-1}))))]`, and the else arm under
  `all(not(any(p_1, …, p_n)))`. Each arm's tokens go through one `@__group` call, the item the
  guard attaches to.
- `depth(C)` is the `macro_rules!` recursion depth of the expansion: one `@__arms` level per arm
  (else included) and the base case, plus the entry level and one `@__group` level.
- **N1, `macro_rules!` expansion semantics.** These specs are about macro expansion, while
  Trust/Clean reason about compiled MIR, and no row of the audit §7 gap table covers expansion.
  N1 names that gap. Route (a) is a `ty_model!`/Clean model of the three rules (tt-muncher
  recursion with an accumulator of the predicates seen, attribute binding on a macro-call item,
  `#[cfg(all(.., not(any(..))))]` evaluation, splicing in item, statement and expression
  position), bound to the real macro by a conformance test with a negative control. Route (b)
  is an expansion certificate from rustc's `rustc_expand`. Gap 10 (model→code refinement) is the
  nearest existing row but does not cover a macro, which has no compiled body; hence
  `blocking_gaps = 0, 10, N1` on every row.

The per-instance corollaries (`tail_value()` equals its `cfg!()` cascade, `pick()` equals
`expected()`) are straight-line constant fns that need only gap 0, but they are point samples of
the spec and retire no test (`docs/TRUST-LANE.md`, "Rules for agents doing
this").

## 1. Specs (TSV)

`subsumes` lists the primary tests; `difficulty` names the Rust features the proof must handle;
`blocking_gaps` are rows of the gap table in `docs/TRUST-LANE.md` (Blocking gaps)
§7, plus `N1` (§0).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
cfgif-first-match-selection	equivalence	cfg_if::cfg_if! (entry rule, @__arms, @__group)	∀ chain C (any n≥1, any predicates p_i, any bodies T_i, with or without T_e), in every syntactic position the macro accepts (item, statement, tail expression): E(C) is token-for-token T_{sel(C)} in the caller's syntax context (so a `let` bound in the selected arm is visible after the invocation). When sel(C)=none, E(C) is empty: no item, no statement, no `()` and no `;`. Equivalently, in expression position E(C) behaves like `if cfg!(p_1) {T_1} else if cfg!(p_2) {T_2} … else {T_e}`. Core lemma: the guards g_i are pairwise mutually exclusive, and g_i holds ⇔ cfg(p_i) ∧ ∀j<i ¬cfg(p_j). So when several p_i hold, the FIRST one wins.	differential::real_macro_matches_rustc_on_every_chain, differential::no_arm_matches_expands_to_nothing, differential::tail_expression_produces_the_selected_arms_value, differential::statement_arm_preserves_caller_hygiene_for_let_bindings, differential::cfg_if_expansion_conformance_and_tripwire_negative_controls	EASY propositional core (`CfgIfExpansion` in aterm-models-core proves FirstMatchSelected and DeadArmFullyAbsent across all predicate valuations and catches both tripwire mutants). MEDIUM binding: induction over the @__arms accumulator, cfg evaluation of all/not/any, and statement/expression-position splicing with the caller's hygiene.	0, 10, N1
cfgif-dead-arm-fully-absent	invariant	cfg_if::cfg_if! (@__arms guard, @__group splice)	∀ chain C, ∀ arm i ≠ sel(C): NO token of T_i is expanded or survives cfg-stripping, whatever the number of items in T_i (k≥1). The guard g_i attaches to the single `@__group` macro-call item that carries all of T_i, never only to T_i's first item (upstream cfg-if issue #90). Corollary: a name defined only in a dead arm may be redefined beside the macro without a duplicate-definition error, and `compile_error!` in a dead arm does not fire.	differential::a_dead_arm_contributes_none_of_its_items, differential::cfg_if_expansion_conformance_and_tripwire_negative_controls	EASY once N1 models attribute binding: one structural fact about which node the cfg attribute covers (`DeadArmFullyAbsent` in `CfgIfExpansion`, caught by `Buggy = 2`).	0, 10, N1
cfgif-linear-recursion-depth	other	cfg_if::cfg_if!	∀ chain C with m arms (m = n, plus 1 if T_e is present): depth(C) ≤ m + c for a small constant c (one @__arms level per arm and the base case, plus the entry and @__group levels). So any chain with m + c ≤ 128 (the default recursion_limit) expands with no #![recursion_limit]. This covers getrandom's backend chain and the 40-arm sample.	differential::forty_arms_stay_inside_the_default_recursion_limit	MEDIUM: a bound on expansion depth by induction over the arm list. N1 must model rustc's recursion counter, which counts nesting depth and not the total number of expansions.	0, 10, N1
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test; `also` lists the other specs it samples.

```tsv
file	test	primary	also
tests/differential.rs	real_macro_matches_rustc_on_every_chain	cfgif-first-match-selection	cfgif-dead-arm-fully-absent
tests/differential.rs	a_dead_arm_contributes_none_of_its_items	cfgif-dead-arm-fully-absent	
tests/differential.rs	no_arm_matches_expands_to_nothing	cfgif-first-match-selection	
tests/differential.rs	tail_expression_produces_the_selected_arms_value	cfgif-first-match-selection	
tests/differential.rs	forty_arms_stay_inside_the_default_recursion_limit	cfgif-linear-recursion-depth	cfgif-first-match-selection
tests/differential.rs	statement_arm_preserves_caller_hygiene_for_let_bindings	cfgif-first-match-selection	
tests/differential.rs	cfg_if_expansion_conformance_and_tripwire_negative_controls	cfgif-first-match-selection	cfgif-dead-arm-fully-absent, cfgif-linear-recursion-depth
```

`real_macro_matches_rustc_on_every_chain` samples `cfgif-dead-arm-fully-absent` only weakly: every
arm of its chains defines the same `pick()`, so a leaked first item would be a duplicate-definition
error.

## 4. Findings

Status 2026-10-28: `CfgIfExpansion` (`aterm_models_core::cfg_if_expansion_model`) and
`cfg_if_expansion_conformance_and_tripwire_negative_controls` in `tests/differential.rs` now carry
the N1 route (a) `ty_model!` and both negative-control tripwire mutations (`Buggy = 1` last-match-wins
and `Buggy = 2` un-grouped multi-item arms).

## 5. Implied specs that look wrong, or disagree with the code

2. **`a_dead_arm_contributes_none_of_its_items` rests on compilation.** It samples
   `cfgif-dead-arm-fully-absent`, and the evidence is that `leak_correct` compiles at all; the
   runtime asserts `(99, 2)` are nearly tautological.
3. **The host-dependent chains sample one point each.** In
   `real_macro_matches_rustc_on_every_chain`, the `parking_lot_core` and `target_family` assertions
   depend on the host by design. On any one host they check one value of `sel(C)`. The overlap
   they are named for (on Linux both of the first two arms hold) is exercised only on Linux and
   Android cells. Only the `all()`/`any()` chains are host-independent first-match samples. The
   spec quantifies over all predicate valuations, which closes this.
4. **Resolved — conflicting `getrandom` arm counts removed.** `src/lib.rs` no longer cites a
   specific `getrandom` backend arm count in its docs or macro rules.
5. **Resolved — misplaced `@__group` recursion-depth comment removed.** `src/lib.rs` no longer
   attaches the per-arm recursion-depth comment to `@__group`; §0 states the exact accounting
   (one `@__arms` level per arm plus the entry and `@__group` levels).
6. **The gap table has no row for these specs.** None of the 3 specs is blocked by any audit §7
   row as written; `macro_rules!` expansion semantics is missing from the table. This file cites
   gap 10 as the nearest row and adds N1 (§0).
