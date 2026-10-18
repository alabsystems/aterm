<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-bidi: spec recovery from its 32 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 19 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-15: this is the crate's migration worklist. Each of the 12 specs below is
> proved through Trust/Clean about the compiled Rust fn, and then the tests it subsumes are
> deleted. None of the 32 tests is a non-spec sample, so nothing is deleted ahead of a proof.
> The `#[cfg(test)]` wrappers (`reorder_str`, `reorder_cells`, `paragraph_level`) go with
> the last test that calls them (§5.4).

Source: `crates/aterm-bidi` (read only). I read every `#[test]` (30 in `src/lib.rs`: 26 in
`mod tests` and 4 in `mod generated`; 2 in `tests/uax9_conformance.rs`, 32 in all) and the
code each one reaches. Nothing was compiled or run. The claims below come from reading the
code.

Result: **12 specs** covering **32 tests**. **0 tests are non-spec samples** (§4).

The gap numbers in the `blocking_gaps` column are the rows of the gap list in
the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.
Gap 0 (the installed seal publishes no Lean-discharged row) is a prerequisite for every
spec, so it is listed on every row. No spec is blocked by gap 0 alone.

## 0. Notation used in the statements

- Test paths: `T::` = `src/lib.rs::tests::`, `G::` = `src/lib.rs::generated::`,
  `C::` = `tests/uax9_conformance.rs::`.
- `cls(t)` = `map bidi_class t`; `brk(t)` = `map bracket_of t`. A missing bracket entry reads
  as `Bracket::None`.
- `Paras(c)`: the split of `0..|c|` into maximal half-open ranges, each ending just after a
  `B` or at `|c|` (UAX #9 P1). `resolve_into` and `reorder_paragraphs_into` both use this split.
- `para(P, base)` = `paragraph_level_of(c[P], match_isolates(c[P]), base)`. `Ltr` gives 0 and
  `Rtl` gives 1. `Auto` gives 1 iff the first `L`/`R`/`AL` at isolate depth 0 is `R` or `AL`
  (BD8/BD9: jump from each `LRI`/`RLI`/`FSI` to its matching `PDI`, and give up at an
  unmatched initiator), and 0 otherwise.
- `Lv(c, br, b)` = `resolve_levels_from_classes(c, br, b)`.
- `Perm(v, n)`: `v` is a permutation of `0..n` (`|v| = n`, and each index appears once).
- `L2ref(l)`: start from the identity on `0..|l|`. For `k` from `max(l)` down to the least odd
  number `>= min(l)`, reverse every maximal run of positions with `l[i] >= k`. That is the
  UAX #9 wording. The code (`reverse_by_levels`) stops at the lowest odd level actually
  present. The result is the same, because the passes at `k` and `k-1` reverse identical runs
  when no level equals `k-1`, so they cancel. The L2 spec proves that lemma rather than
  restating the code.
- `L2para(c, l)` = the concatenation over `P ∈ Paras(c)` of `start(P) + L2ref(l[P])`. This is
  what `reorder_paragraphs_into` computes.
- `Fold(classes, wide)`: the cell fold in `fold_cells_and_resolve`. `i` walks `0..n`. Cell `i`
  starts a logical char that owns cell `i+1` iff `wide[i+1]` (`cont`), and the walk advances 2
  if `cont`, else 1. `lead[k]` and `cont[k]` are logical char `k`'s lead cell and continuation
  flag. `wide[0]` is never consulted, so a stray leading continuation is its own char. `fc` and
  `fb` are the folded classes and brackets.
- `UAX9Ref(c, br, b)`: a Lean reference model, still to be authored, of UAX #9 (Unicode 18.0)
  P1–P3, X1–X10 (max_depth 125), W1–W7, N0 (BD16 with the 63-entry stack and
  canonical-equivalent brackets), N1–N2, I1–I2 and L1. It returns an `Option` level per char,
  with `None` for X9-removed chars (`removed_by_x9`). The crate's convention fills a removed
  position with the previous kept char's post-I level (`para` at a paragraph's start), and L1
  resets it to `para` when it trails.
- `NoBidi(c)` = no `c[i]` is in `{R, AL, AN, RLE, RLO, RLI, FSI}` (= `!has_bidi_classes(c)`).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary ones
in §2), `difficulty` (the Rust features the proof must handle), `blocking_gaps` (audit §7).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
bidi-class-table	table-property	bidi_class, tables::BIDI_CLASS_RANGES	(a) Well-formedness: for consecutive rows (lo_j,hi_j,_), (lo_{j+1},..): lo_j ≤ hi_j < lo_{j+1} (sorted, disjoint), so the binary_search_by comparator is monotone and Ok(i) is the unique row containing u. (b) ∀ char c: bidi_class(c) = UCD 18.0.0 DerivedBidiClass(c), @missing defaults included. A scalar in no row is L, and the generator guarantees the UCD agrees there. Decidable by evaluation over all 0x110000 scalars once (a) gives the search lemma.	(secondary only: C::bidi_character_test_txt_subset_conforms)	EASY-MEDIUM: a static slice of (u32,u32,enum) tuples, slice::binary_search_by with a closure returning Ordering, Option::map_or, char→u32. The UCD must be Lean data.	0, 1, 3, 5, 7
bracket-of-table	table-property	bracket_of, canonical_bracket, tables::BRACKET_PAIRS, tables::BRACKET_CLOSERS	BRACKET_PAIRS is sorted strictly by opener and BRACKET_CLOSERS strictly by closer. ∀ char c: bracket_of(c) = Open(canon(c)) iff Bidi_Paired_Bracket_Type(c) = Open; Close(canon(Bidi_Paired_Bracket(c))) iff the type is Close; None otherwise. canon maps 0x2329→0x3008 and 0x232A→0x3009, and fixes everything else. Corollary (BD16 with canonical equivalence): an opener o and a closer x get equal keys iff x pairs with o up to canonical equivalence.	(secondary only: C::bidi_character_test_txt_subset_conforms)	EASY: binary_search_by_key over static tuple slices, an enum with a payload, a match on u32 literals	0, 1, 3, 5, 7
char-entry-points-agree	equivalence	classes_and_brackets, has_bidi, has_bidi_classes, resolve_levels, resolve_levels_from_classes, reorder_cells_with_classes, reorder_cells (cfg(test))	∀ text: (a) classes_and_brackets(text) = (cls(text), brk(text)); (b) has_bidi(text) = has_bidi_classes(cls(text)) = ¬NoBidi(cls(text)). Hence ∀ base: resolve_levels(text, base) = Lv(cls(text), brk(text), base). Also reorder_cells(cc, w, base) = reorder_cells_with_classes(cls(cc), brk(cc), w, base), which is definitional given (a).	G::class_taking_and_into_variants_match_char_taking	EASY: a for loop over &[char] with Vec::with_capacity/push, Iterator::any with a closure, a tuple return, calls to admitted fns	0, 1, 3, 4, 5, 7
scratch-independence	equivalence	reorder_cells_with_classes_into, reorder_cells_with_classes, resolve_into, fold_cells_and_resolve, reorder_from_levels_into, reorder_from_levels, Scratch, Resolve	∀ scratch s, ∀ out vector o (any prior contents), ∀ classes, brackets, wide, base: after reorder_cells_with_classes_into(classes, brackets, wide, base, &mut s, &mut o), o = reorder_cells_with_classes(classes, brackets, wide, base). ∀ o, l: after reorder_from_levels_into(l, &mut o), o = reorder_from_levels(l). Core lemma: resolve_into(c, br, b, r, lv) sets lv = Lv(c, br, b) for every prior r and lv, because every buffer in Resolve and Scratch is cleared or overwritten before it is read. Determinism of the pure fns follows.	T::into_variants_match_vec_returning	MEDIUM: a &mut struct of 7-8 Vecs, Vec clear/resize/extend/push, std::mem::take round-trips, a frame argument that each buffer is written before it is read	0, 1, 2, 3, 4, 5, 7
levels-shape	invariant	resolve_levels_from_classes, resolve_into, resolve_paragraph, resolve_sequence, least_greater_odd, least_greater_even	∀ c, br, b, with l = Lv(c, br, b): |l| = |c|, and ∀ P ∈ Paras(c), ∀ i ∈ P: para(P, b) ≤ l[i] ≤ MAX_DEPTH + 1 = 126. Sub-lemmas: every directional-status-stack entry has para ≤ level ≤ 125; I1/I2 add only 0, 1 or 2 to an entry's level and never go past 126; L1 assigns exactly para; a removed char copies the previous kept char's level, or gets para.	G::levels_well_formed	HARD: nested while/for loops over slices, a Vec<StatusEntry> stack with push/pop, saturating u8 arithmetic, is_multiple_of, a match over a 23-variant enum, &mut [u8] writes through set()	0, 1, 2, 3, 5, 7
paragraph-level-p2p3	contract	match_isolates, first_strong_level, paragraph_level_of, paragraph_level (cfg(test))	(BD9) ∀ c, after match_isolates(c, &mut m): |m| = |c|. For each initiator i (LRI/RLI/FSI), m[i] is the index of its matching PDI under stack matching, or |c| if it has none, and m[m[i]] = i for a matched pair. Every other position holds usize::MAX. (P2/P3) ∀ c, b, from ≤ to ≤ |c|: first_strong_level(c, m, from, to) returns Some(0) or Some(1) for the first L or R/AL in [from, to) outside isolates, skipping each initiator..matching-PDI span. It returns None if no such char exists or an initiator's match is ≥ to. paragraph_level_of(c, m, b) = para as in §0.	(secondary only: T::pure_rtl_reverses)	EASY-MEDIUM: a Vec<usize> stack in a loop, enumerate, Option<u8> returns, a while loop with an index jump, &mut Vec resize	0, 1, 2, 3, 5, 7
l2-reorder-from-levels	refinement	reorder_from_levels, reorder_from_levels_into, reverse_by_levels	∀ level slice l: reorder_from_levels(l) = L2ref(l). Corollaries: Perm(reorder_from_levels(l), |l|); if every l[i] is even, the result is the identity 0..|l|. The proof includes the lemma that stopping at the lowest odd level present equals UAX #9's lowest odd level, the least odd number ≥ min(l).	(secondary only: G::reorder_is_always_a_permutation, C::bidi_test_txt_subset_conforms)	MEDIUM: nested while loops with a descending level counter and loop/break, slice::reverse on get_mut subranges, iterator max/filter/min, &mut [usize]	0, 1, 2, 3, 5, 7
reorder-visual-composition-and-permutation	contract	reorder_visual_to_logical, reorder_paragraphs_into, reorder_str (cfg(test))	∀ text, base: reorder_visual_to_logical(text, base) = L2para(cls(text), resolve_levels(text, base)). Hence Perm(reorder_visual_to_logical(text, base), |text|), and every paragraph's indices stay inside that paragraph's range. For text = [] the result is [].	G::reorder_is_always_a_permutation	EASY given l2-reorder-from-levels and char-entry-points-agree: a while loop over paragraphs with position(), paired subslice get/get_mut, Vec extend from a range	0, 1, 2, 3, 4, 5, 7
cell-reorder-fold	contract	reorder_cells_with_classes, reorder_cells_with_classes_into, fold_cells_and_resolve, reorder_cells (cfg(test))	requires |wide| = |classes| = n (§5.2). ∀ classes, brackets (any length), base b: let (lead, cont, fc, fb) = Fold(classes, wide) and ord = L2para(fc, Lv(fc, fb, b)). Then reorder_cells_with_classes(classes, brackets, wide, b) = concat over k ∈ ord of ([lead[k]] ++ (cont[k] ? [lead[k]+1] : [])). Hence Perm(out, n), and every continuation cell immediately follows its lead cell (a wide glyph moves as a unit and is never mirrored). A continuation flag at cell 0, or one after another continuation, makes that cell its own char.	T::reorder_cells_is_always_a_permutation, T::reorder_cells_malformed_leading_continuation_is_safe, T::reorder_cells_rtl_with_wide_glyph_keeps_pair_unmirrored	MEDIUM: a while loop with a variable stride (saturating_add 1 or 2), Option-guarded slice gets, pushes into &mut Scratch fields, a for loop expanding the char order; composes resolve and L2	0, 1, 2, 3, 4, 5, 7
cell-levels-fold	contract	cell_levels, fold_cells_and_resolve	requires |wide| = |classes| = n. ∀ classes, brackets, base b: let (lead, cont, fc, fb) = Fold(classes, wide) and lv = Lv(fc, fb, b). Then |cell_levels(classes, brackets, wide, b)| = n, its entry at lead[k] is lv[k], and so is its entry at lead[k]+1 when cont[k]. The fold is the one cell-reorder-fold uses, so the level at cell out[v] is the level of the logical char that cell-reorder-fold places at visual position v.	T::cell_levels_marks_rtl_cells_and_pairs_wide_glyphs	EASY given cell-reorder-fold's fold lemma: vec![0; n], enumerate over lead_cell, Option-guarded get_mut writes, if-let chains	0, 1, 2, 3, 4, 5, 7
no-rtl-is-identity	contract	has_bidi, has_bidi_classes, resolve_levels_from_classes, reorder_visual_to_logical, reorder_cells_with_classes, reorder_cells_with_classes_into	∀ c, br, ∀ b ∈ {Auto, Ltr}: NoBidi(c) ⇒ every Lv(c, br, b)[i] is even, so L2para(c, Lv(c, br, b)) is the identity. Therefore ∀ text with ¬has_bidi(text), b ∈ {Auto, Ltr}: reorder_visual_to_logical(text, b) = 0..|text|; and ∀ classes with ¬has_bidi_classes(classes), b ∈ {Auto, Ltr}, |wide| = |classes|: reorder_cells_with_classes(classes, br, wide, b) = 0..|classes|. Proof sketch: para = 0; only LRE/LRO/LRI push, and they push even levels; W1/W7 with sos = L turn every EN into L, and with no R/AL nothing becomes AN; N0/N1/N2 resolve neutrals to L or to the even embedding direction; I1 adds 0 or 2. FALSE for b = Rtl (§5.1).	G::ltr_only_is_identity, T::reorder_cells_wide_glyph_stays_paired_ltr	HARD: a whole-algorithm invariant (no odd type and no odd stack level) carried through every loop of resolve_paragraph and resolve_sequence, then composed with L2 and the cell fold	0, 1, 2, 3, 4, 5, 7
uax9-levels-refine-reference	refinement	resolve_levels_from_classes, resolve_into, resolve_paragraph, resolve_sequence, match_isolates, first_strong_level, removed_by_x9	∀ c, br (missing entries are None), b: ∀ i with ¬removed_by_x9(c[i]), Lv(c, br, b)[i] = UAX9Ref(c, br, b)[i]; for every removed i, Lv[i] follows the convention in §0. Then for every line whose only B (if any) is its last char: filter(¬removed_by_x9 ∘ c, reorder_from_levels(Lv(c, br, b))) = the UAX #9 L2 visual order of the kept chars. Composed with bidi-class-table, bracket-of-table and reorder-visual-composition-and-permutation, this yields every point example: the pure-RTL reversals, EN/AN at level 2 inside RTL, N1 neutrals between R, the L1 trailing-WS reset, and forced-RTL Latin at level 2. The conformance subsets are samples of this statement; once UAX9Ref exists, their data validates UAX9Ref itself.	C::bidi_test_txt_subset_conforms, C::bidi_character_test_txt_subset_conforms, T::pure_rtl_reverses	VERY HARD: the whole algorithm (~470 lines of nested loops, Vec stacks, closures capturing &Resolve and &mut Resolve, sort_unstable on pairs, rposition/truncate on the bracket stack, find over reversed ranges, &mut buffers, a 23-variant enum, saturating u8 arithmetic), plus a Lean list/inductive UAX #9 model bound to Rust data	0, 1, 2, 3, 4, 5, 7, 10, 14
```

### Distribution by kind (12)

| kind | count | ids |
|---|---|---|
| contract | 5 | paragraph-level-p2p3, reorder-visual-composition-and-permutation, cell-reorder-fold, cell-levels-fold, no-rtl-is-identity |
| equivalence | 2 | char-entry-points-agree, scratch-independence |
| refinement | 2 | l2-reorder-from-levels, uax9-levels-refine-reference |
| table-property | 2 | bidi-class-table, bracket-of-table |
| invariant | 1 | levels-shape |

### Gap histogram (specs blocked by each audit §7 gap)

| gap | specs | which |
|---|---|---|
| 0 | 12 | all (prerequisite) |
| 1 | 12 | all |
| 3 | 12 | all |
| 5 | 12 | all |
| 7 | 12 | all |
| 2 | 10 | all but the two table properties |
| 4 | 7 | char-entry-points-agree, scratch-independence, reorder-visual-composition-and-permutation, cell-reorder-fold, cell-levels-fold, no-rtl-is-identity, uax9-levels-refine-reference |
| 10 | 1 | uax9-levels-refine-reference |
| 14 | 1 | uax9-levels-refine-reference |

### Proof dependency order

The two table properties, `paragraph-level-p2p3` and `l2-reorder-from-levels` →
`char-entry-points-agree` → `levels-shape` → `reorder-visual-composition-and-permutation`
→ the fold lemma, then `cell-reorder-fold` → `cell-levels-fold` → `scratch-independence` →
`no-rtl-is-identity` → `uax9-levels-refine-reference` (needs the Lean `UAX9Ref`).

## 2. Test → spec map (TSV; all 32)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/lib.rs	tests::pure_rtl_reverses	uax9-levels-refine-reference	paragraph-level-p2p3
src/lib.rs	tests::reorder_cells_wide_glyph_stays_paired_ltr	no-rtl-is-identity	cell-reorder-fold
src/lib.rs	tests::reorder_cells_rtl_with_wide_glyph_keeps_pair_unmirrored	cell-reorder-fold	uax9-levels-refine-reference
src/lib.rs	tests::cell_levels_marks_rtl_cells_and_pairs_wide_glyphs	cell-levels-fold	uax9-levels-refine-reference
src/lib.rs	tests::reorder_cells_is_always_a_permutation	cell-reorder-fold	
src/lib.rs	tests::reorder_cells_malformed_leading_continuation_is_safe	cell-reorder-fold	
src/lib.rs	tests::into_variants_match_vec_returning	scratch-independence	
src/lib.rs	generated::reorder_is_always_a_permutation	reorder-visual-composition-and-permutation	l2-reorder-from-levels
src/lib.rs	generated::levels_well_formed	levels-shape	
src/lib.rs	generated::ltr_only_is_identity	no-rtl-is-identity	
src/lib.rs	generated::class_taking_and_into_variants_match_char_taking	char-entry-points-agree	scratch-independence
tests/uax9_conformance.rs	bidi_test_txt_subset_conforms	uax9-levels-refine-reference	l2-reorder-from-levels
tests/uax9_conformance.rs	bidi_character_test_txt_subset_conforms	uax9-levels-refine-reference	bidi-class-table, bracket-of-table
```

## 3. Coverage

- 32 tests read. All 32 map to one of the 12 specs. None is a non-spec sample (§4).
- Primary tests per spec: `uax9-levels-refine-reference` 11, `no-rtl-is-identity` 6,
  `char-entry-points-agree` 3, `reorder-visual-composition-and-permutation` 3,
  `cell-reorder-fold` 3, `levels-shape` 2, and 1 each for `bidi-class-table`,
  `scratch-independence`, `l2-reorder-from-levels` and `cell-levels-fold`.
  `bracket-of-table` and `paragraph-level-p2p3` have no primary test. They are lemmas the
  conformance and point tests sample only through the whole algorithm.
- `uax9-levels-refine-reference` is the largest collapse and the hardest proof. Its 11 tests
  stay until the Lean `UAX9Ref` exists and the refinement is Certified.

## 4. Tests that are not spec samples

None. Every test samples one of the specs above.

| item | why | action |
|---|---|---|
| (no test) | All 32 tests sample a general property. | **keep until the primary spec is Certified**, then delete |
| `#[cfg(test)]` wrappers `reorder_str`, `reorder_cells`, `paragraph_level` | They are test-only code with no production caller. The specs are stated on the production fns. | **delete** together with the last test that calls them (§5.4) |

## 5. Implied specs that look wrong, or disagree with the code

1. **`has_bidi`/`has_bidi_classes` overclaim (probably a real deviation).** The doc
   (`src/lib.rs:190-196`) says a line for which they return `false` "resolves to even levels
   only, so its visual order is the identity". That holds only for `BaseDirection::Auto`/`Ltr`.
   With `Rtl`, `"a!"` resolves to `[2,1]` and reorders to `[1,0]`, and `"! a"` resolves to
   `[1,1,2]` and reorders to `[2,1,0]`. The tests (`G::ltr_only_is_identity` and the identity
   point tests) sample only `Auto`. `crates/aterm-core/src/terminal/bidi_reorder.rs` skips the
   reorder on `!has_bidi_classes` before it computes the base (around :173, :259 and :376). So
   under SCP RTL, or AutoRtl without autodetect, a row of Latin and punctuation is shown in
   logical order where UAX #9 says to reorder it. `no-rtl-is-identity` carries `b ≠ Rtl`. Fix the
   doc, and either fix the callers or document the shortcut as deliberate.
2. **The cell fns do not check that `wide` and `classes` have equal length.**
   `reorder_cells_with_classes(_into)` and `cell_levels` never check
   `|is_wide_continuation| = |cell_classes|`; the doc says only "parallel".
   `fold_cells_and_resolve` reads `wide[i+1]` even when `i+1 = n`. So a `wide` slice longer
   than `classes` with `wide[n] = true` puts index `n` in the reorder output, which is then no
   longer a permutation of `0..n`. The specs carry `requires |wide| = |classes|`. The
   alternative is to bound the lookup by `cell_classes.len()` and drop the precondition.
   Production callers pass equal lengths.
3. **The per-paragraph L2 path has no conformance coverage.** `C::check` reorders with
   `reorder_from_levels` over the whole line, while production `reorder_visual_to_logical` and
   the cell reorder use per-paragraph L2 (`reorder_paragraphs_into`). The checked-in subsets
   have `B` only at line end, so the two coincide. Only the generated permutation tests (lines
   containing `'\n'`) touch the production path. `reorder-visual-composition-and-permutation`
   closes this once it is proved.
4. **Tests target `cfg(test)` wrappers.** Several tests call `reorder_str`, `reorder_cells`
   or `paragraph_level` instead of the production entry points. The specs are stated on
   `reorder_visual_to_logical`, `reorder_cells_with_classes(_into)` and
   `paragraph_level_of`/`first_strong_level`. Delete the wrappers with the tests.
5. **`resolve_sequence` has a dead parameter.** It takes `matching` and discards it at once
   (`let _ = matching;`, `src/lib.rs:934`). That is harmless, but a contract on the fn should
   not mention it, and the parameter can go.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone. Every one also needs gaps 1, 3, 5 and 7 (content models,
E6 body widening, quantified clauses, loops). So the order below favours specs that need
nothing beyond those four, then those that add gap 2 (`&mut`) and gap 4 (callee-ensures
composition), and leaves the reference-model refinement (gaps 10 and 14) for last.

1. **`bracket-of-table`** and **`bidi-class-table`** (gaps 0, 1, 3, 5, 7; no `&mut`, no
   composition). Each is a sorted-table search lemma plus an evaluated fact over a finite
   domain. The two together are the data half of both conformance tests.
2. **`char-entry-points-agree`** (adds gap 4 only). This is a map loop and an `any`, with no
   `&mut` state. It retires 3 tests and is the bridge every char-taking spec composes through.
3. **`l2-reorder-from-levels`** (adds gap 2). This is self-contained arithmetic over levels,
   with the "lowest odd present" lemma as its one real proof step. It retires 1 test and is
   used by every reorder spec.
4. **`paragraph-level-p2p3`**, then **`reorder-visual-composition-and-permutation`**
   (3 tests, composing 2 and 3 with a levels-length lemma), then the **cell fold** specs
   **`cell-reorder-fold`** (3 tests) and **`cell-levels-fold`** (1). These carry
   `requires |wide| = |classes|` (§5.2).
5. **`levels-shape`** (2 tests), **`scratch-independence`** (1) and **`no-rtl-is-identity`**
   (6 tests, carrying `b ≠ Rtl`, §5.1). Each of these needs a loop invariant across the whole
   resolver.
6. **`uax9-levels-refine-reference`** (11 tests). Author `UAX9Ref` in Lean (gaps 10 and 14),
   validate it against the conformance data, then prove the refinement.
