<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-cfg-if: spec recovery from its 6 unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each of the 3 specs below is
> to be proved through Trust/Clean, and then the tests it subsumes are deleted. No test has
> been deleted yet. All 3 specs are blocked by gap 0, by gap 10 (the nearest audit §7 row)
> and by a gap that is **not in audit §7**: N1, `macro_rules!` expansion semantics (§0).

Source: `crates/aterm-cfg-if` (read only). The directory is `aterm-cfg-if`, the package is
`cfg-if` and the lib is `cfg_if`. The crate has **no runtime function**. Its whole production
surface is one `macro_rules! cfg_if` in `src/lib.rs`, which has three rule families: the entry
rule, the `@__arms` accumulator (with its empty base case) and the `@__group` splice. The
manifest sets `test = false` on the lib, so all 6 `#[test]` fns are in `tests/differential.rs`.
The 4 doctests on the macro are not counted among the 6 (see §3). Nothing was compiled or run.
The claims below come from reading the code.

Result: **3 specs**, covering **5 tests**. **1 test is not a spec sample** (§4).

## 0. Notation used in the statements

- A chain `C = [(p_1,T_1), …, (p_n,T_n)]` with `n ≥ 1`, plus an optional else body `T_e`. Each
  `p_i` is a cfg predicate token tree. Each `T_i` is a token sequence: items, statements or one
  expression.
- `cfg(p)` is rustc's evaluation of predicate `p` for the target being compiled, which is the
  value `cfg!(p)` gives. `cfg(all()) = true` and `cfg(any()) = false` on every target.
- `sel(C) = min{ i | cfg(p_i) }`. If no `p_i` holds, `sel(C) = else` when `T_e` is present and
  `none` when it is absent.
- `E(C)` is what `cfg_if::cfg_if!{ if #[cfg(p_1)]{T_1} else if … else {T_e} }` leaves after
  cfg-stripping. The entry rule normalizes the arms into `((p_i) (T_i))` pairs, with the else arm
  given an empty predicate. `@__arms` then emits arm `i` under the guard
  `g_i = #[cfg(all(p_i, not(any(p_1, …, p_{i-1}))))]`. The else arm's guard is
  `all(not(any(p_1, …, p_n)))`. Each arm's tokens go through one `@__group` call, which is the
  item the guard attaches to.
- `depth(C)` is the `macro_rules!` recursion depth the expansion uses. The code spends one
  `@__arms` level per arm (else included), plus the entry level and one `@__group` level.

**Why none of this fits audit §7.** These are properties of macro expansion, not of a compiled
Rust function. Clean/Trust reason about compiled MIR, so they cannot state these properties today,
and no row 0–17 of the gap table covers `macro_rules!` expansion semantics. This file therefore
uses a new gap:

- **N1. `macro_rules!` expansion semantics (new; not in audit §7).** Two routes are possible.
  (a) A `ty_model!`/Clean model of this three-rule transcription: tt-muncher recursion with an
  accumulator of the predicates seen so far, attribute binding on a macro-call item,
  `#[cfg(all(.., not(any(..))))]` evaluation, and splicing in item, statement and expression
  position. It would be bound to the real macro by a conformance test with a negative control.
  (b) A compiler-side expansion certificate from rustc's `rustc_expand`.
  The cost is S–M for (a) and L for (b). N1 should be added to the audit §7 gap table.

Route (a) is the nearest fit to an existing row: **gap 10** (model→code refinement compiler
surface beyond scalars), because the conformance binding of a `ty_model!` to the code is what
route (a) needs. Gap 10 alone does not cover it: gap 10 refines models against compiled Rust,
and a macro has no compiled body. So the `blocking_gaps` column below lists `0, 10, N1`: gap 0
(the installed seal) blocks every proof, gap 10 is the nearest existing row, and N1 is the real
blocker.

The per-instance corollaries are straight-line constant fns. Examples are `tail_value()`, which
equals its `cfg!()` cascade, and `pick()`, which equals `expected()`. Trust can already prove them,
blocked only by gap 0, but they are samples, not the spec. Proving them is not a migration (PLAN,
"Rules for agents").

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty (Rust features the proof
must handle)`, `blocking_gaps` (audit §7 numbers, plus `N1`, the new gap in §0).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
cfgif-first-match-selection	equivalence	cfg_if::cfg_if! (entry rule, @__arms, @__group)	∀ chain C (any n≥1, any predicates p_i, any bodies T_i, with or without T_e), in every syntactic position the macro accepts (item, statement, tail expression): E(C) is token-for-token T_{sel(C)}. When sel(C)=none, E(C) is empty: no item, no statement, no `()` and no `;`. Equivalently, in expression position E(C) behaves like `if cfg!(p_1) {T_1} else if cfg!(p_2) {T_2} … else {T_e}`. Core lemma: the guards g_i are pairwise mutually exclusive, and g_i holds ⇔ cfg(p_i) ∧ ∀j<i ¬cfg(p_j). So when several p_i hold, the FIRST one wins.	differential::real_macro_matches_rustc_on_every_chain, differential::no_arm_matches_expands_to_nothing, differential::tail_expression_produces_the_selected_arms_value	EASY propositional core (the guard algebra is a Lean lemma by induction on i). MEDIUM binding: induction over the @__arms accumulator, cfg evaluation of all/not/any, and statement/expression-position splicing with the caller's hygiene. The per-instance corollaries (const fns returning &'static str or u32) need only gap 0, but they are samples.	0, 10, N1
cfgif-dead-arm-fully-absent	invariant	cfg_if::cfg_if! (@__arms guard, @__group splice)	∀ chain C, ∀ arm i ≠ sel(C): NO token of T_i survives cfg-stripping, whatever the number of items in T_i (k≥1). The guard g_i attaches to the single `@__group` macro-call item that carries all of T_i, never only to T_i's first item (upstream cfg-if issue #90). Corollary: a name defined only in a dead arm may be redefined beside the macro without a duplicate-definition error.	differential::tripwire_ungrouped_leaks_and_the_real_macro_does_not (the leak_correct half)	EASY once N1 models attribute binding: one structural fact about which node the cfg attribute covers, and no induction over T_i is needed.	0, 10, N1
cfgif-linear-recursion-depth	other	cfg_if::cfg_if!	∀ chain C with m arms (m = n, plus 1 if T_e is present): depth(C) ≤ m + c for a small constant c (one @__arms level per arm and the base case, plus the entry and @__group levels). So any chain with m + c ≤ 128 (the default recursion_limit) expands with no #![recursion_limit]. This covers getrandom's backend chain and the 40-arm sample.	differential::forty_arms_stay_inside_the_default_recursion_limit	MEDIUM: a bound on expansion depth by induction over the arm list. N1 must model rustc's recursion counter, which counts nesting depth and not the total number of expansions.	0, 10, N1
```

### Distribution by kind (3)

| kind | count |
|---|---|
| equivalence | 1 |
| invariant | 1 |
| other (resource bound) | 1 |

### Proof dependency order

`cfgif-first-match-selection` and `cfgif-dead-arm-fully-absent` are independent of each other and
share the N1 model. Together they give the crate-doc contract: E(C) emits exactly T_{sel(C)} and
nothing from any other arm. `cfgif-linear-recursion-depth` reuses the same induction over
`@__arms`.

## 2. Test → spec map (TSV; all 6)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
tests/differential.rs	real_macro_matches_rustc_on_every_chain	cfgif-first-match-selection	cfgif-dead-arm-fully-absent
tests/differential.rs	no_arm_matches_expands_to_nothing	cfgif-first-match-selection	
tests/differential.rs	tail_expression_produces_the_selected_arms_value	cfgif-first-match-selection	
tests/differential.rs	tripwire_ungrouped_leaks_and_the_real_macro_does_not	cfgif-dead-arm-fully-absent	
tests/differential.rs	forty_arms_stay_inside_the_default_recursion_limit	cfgif-linear-recursion-depth	cfgif-first-match-selection
tests/differential.rs	tripwire_last_match_wins_is_live	(none: §4)	
```

`real_macro_matches_rustc_on_every_chain` samples `cfgif-dead-arm-fully-absent` only weakly.
Every arm of its chains defines the same `pick()`, so a leaked first item would be a
duplicate-definition compile error.

## 3. Coverage

- 6 tests: 5 are retired by a spec and 1 is not a spec sample.
- 1 spec retires 3 tests; 2 specs retire 1 test each.
- **Doctests (4, not in the 6).** They are on the macro in `src/lib.rs`: the unix/windows example,
  an empty expansion in statement position, `compile_error!` parked in a dead arm, and a
  `compile_fail` control. The first two sample `cfgif-first-match-selection`. The third samples a
  STRONGER form of `cfgif-dead-arm-fully-absent`: a dead arm is never *expanded*, not only
  stripped afterwards. The N1 model should state that form, so that this doctest is subsumed as
  well. The `compile_fail` control is the negative control for the third and goes with it.
- **The crate docs claim a property that nothing tests.** Hygiene is the caller's: a `let` bound
  in an arm is visible after the invocation (`naga`'s `error.rs:114`). It belongs in the statement
  of `cfgif-first-match-selection` ("token-for-token, in the caller's syntax context"), and the N1
  model must carry it.

## 4. Tests that are not spec samples

| test | why | recommendation |
|---|---|---|
| differential::tripwire_last_match_wins_is_live | It exercises only `last_match_wins!`, a deliberately wrong macro defined in the test file with no production caller. It is the negative control that keeps the `cfg!()` differential from being vacuous. Once `cfgif-first-match-selection` is proved, no differential is left to arm. Until then it must stay with `real_macro_matches_rustc_on_every_chain`, because deleting either one alone leaves that differential unarmed. | **delete**, in the same commit as `real_macro_matches_rustc_on_every_chain`, once `cfgif-first-match-selection` is Certified. The N1 conformance test must carry its own negative control (`crates/aterm-spec/src/derive.rs`); `last_match_wins!` is the natural mutant for it. |

By action: delete 1, keep 0, become-spec 0.

The `ungrouped!` tripwire half of `tripwire_ungrouped_leaks_and_the_real_macro_does_not` is the
same kind of negative control (§5.2). It is deleted with its test, and `ungrouped!` becomes the
N1 mutant for `cfgif-dead-arm-fully-absent`.

## 5. Implied specs that look wrong, or disagree with the code

1. **The `case!` comment is stale.** In `tests/differential.rs` it says "ONE chain in, four answers
   out", spliced "into the real macro, into both tripwires' inputs and into the `cfg!()` cascade".
   In fact `case!` generates three things: `real`, `wrong` (from `last_match_wins!`) and
   `expected`. The `ungrouped!` tripwire is never fed by `case!`; it is used only by hand in
   `leak_tripwire`.
2. **`tripwire_ungrouped_leaks_and_the_real_macro_does_not` mixes two things.** Its first half
   asserts how the test-only `ungrouped!` tripwire behaves. That is a negative control, not a spec
   sample. Only the `leak_correct` half samples `cfgif-dead-arm-fully-absent`. Even there, the
   evidence is that `leak_correct` compiles at all; the runtime asserts `(99, 2)` are nearly
   tautological.
3. **The host-dependent chains sample one point each.** In
   `real_macro_matches_rustc_on_every_chain`, the `parking_lot_core` and `target_family` assertions
   depend on the host by design. On any one host they check one value of `sel(C)`. The overlap
   they are named for (on Linux both of the first two arms hold) is exercised only on Linux and
   Android cells. Only the `all()`/`any()` chains are host-independent first-match samples. The
   spec quantifies over all predicate valuations, which closes this.
4. **The getrandom arm count disagrees with itself.** `src/lib.rs` says "twenty-eight
   `mod X; pub use X::*;` pairs in `getrandom`'s backend chain" in the crate docs, and "`getrandom`'s
   27-arm chain" in the comment on `@__group`. The difference does not matter to the depth bound
   (both are far below 128), but one of the two is wrong, and the spec should cite neither.
5. **The `@__group` comment is in the wrong place.** The comment "One recursion level per arm …" is
   attached to the `@__group` rule, but the per-arm level is spent by `@__arms`. `@__group` adds
   one constant level. This matters for stating `cfgif-linear-recursion-depth` correctly.
6. **The gap table has no row for these specs.** None of the 3 specs is blocked by any audit §7
   row as written; `macro_rules!` expansion semantics is missing from the table. This file cites
   gap 10 as the nearest row and adds N1 (§0).

## 6. Proof order (easiest high-value first)

None of the 3 specs is blocked only by gap 0; all 3 wait on gap 10 and N1. So the first step is N1 route (a):
a `ty_model!`/Clean model of the three rules, with a conformance test whose negative controls are
the two tripwire macros.

1. **`cfgif-first-match-selection`**. It has the highest value: it retires 3 tests and frees
   `tripwire_last_match_wins_is_live`, which is the whole wrong-platform-code risk that
   `parking_lot_core`, `wgpu-hal` and `half` rely on. Its propositional core (the guards are
   pairwise exclusive and select the first true predicate) is provable in Clean today as a lemma
   over the model, by induction on `i`.
2. **`cfgif-dead-arm-fully-absent`**. It is the easiest once the model represents attribute
   binding: one structural fact, with no induction over the arm body. It retires the leak test and
   subsumes the `compile_error!` doctest if stated in its never-expanded form (§3).
3. **`cfgif-linear-recursion-depth`**. It is the least valuable, because a violation is a loud
   compile error rather than a silent miscompile. It reuses the `@__arms` induction from 1.

Not a migration: the gap-0-only per-instance theorems (for example `tail_value() = 42`). They are
point samples of spec 1 and must not be counted as retiring any test.
