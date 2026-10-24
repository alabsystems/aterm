<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-grapheme: spec recovery from its 119 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 93 tests it
> deleted (the 17 §4 tests among them) are gone, and the spec rows no longer cite those tests.
> Counts elsewhere in this file were taken before.

> Status 2026-10-15: nothing is deleted yet. The 26 specs below are the crate's migration
> worklist: each is proved through Trust/Clean, then the tests it subsumes are deleted. The 21
> tests in §4 sample no production spec; they go now, together with the `#[cfg(test)]`-only API
> they exercise.

Source: `crates/aterm-grapheme` (read only). I read every `#[test]`
(src/{tests.rs 63, ucs_detect_tests.rs 34, emoji_presentation.rs 5, grapheme_iter.rs 5, word.rs 7},
tests/{emoji_data.rs 2, uax29_conformance.rs 3} = 119) and the code they reach (`width.rs`,
`grid.rs`, `position.rs`, `types.rs`, `tables/{width,gcb}.rs`). Nothing was compiled or run. The
claims below come from reading the code.

Result: **26 specs**, covering **98 tests**. **21 tests are not spec samples** (§4).

Gap numbers in §1 are the rows of the gap table in
the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.

## 0. Notation used in the statements

- `UCD16.P(c)`: the value of Unicode 16.0.0 property `P` at scalar `c`, as a Clean def generated
  from the UCD file. Already vendored in `tests/data`: `emoji-data` (Emoji, Emoji_Presentation,
  Emoji_Modifier_Base, Extended_Pictographic), `DerivedCombiningClass`, `DerivedJoiningType` and
  `GraphemeBreakTest`. **Missing**, and needed as oracles by S4/S5: `GraphemeBreakProperty.txt`,
  `DerivedCoreProperties.txt` (InCB), `EastAsianWidth.txt`, plus the zero-width class rule the table
  generator applies.
- `a ∈ {Single, Double}` is the ambiguous-width setting; `cjk = (a = Double)`.
  `cw_a(c)` = `tables::char_width(c)` when `a = Single`, `char_width_cjk(c)` when `a = Double`.
  `Amb(c)` = `tables::is_ambiguous_width(c)`.
- `chars(s)`: the list of scalars of `s`; `|s|` its byte length.
- `UAX29(s)`: the extended grapheme clusters of `s` under GB1–GB999 (with GB9c and GB11–GB13) over
  UCD16 GCB, InCB and Extended_Pictographic. It is a declarative boundary predicate `Brk(s,i)` on char
  positions, not the iterator's state machine.
- `G(s)` = `s.graphemes()` as a list; `GI(s)` = `s.grapheme_indices()` as a list.
- `W(g,cjk)` = `width::grapheme_display_width_inner(g,cjk)`.
- `GC_a`: the `GridColumns` state machine. `step(st,c)` = `(st', GridStep)`; `col(st)` = its cursor.
  `GridCols_a(s)` = `str_grid_columns_with(s,a)` = `col(foldl step GridColumns::with_ambiguous_width(a) chars(s))`.
- `GG_a(s)` = `grid_graphemes_with(s,a)` as a list of `{text, byte_offset, start_col, end_col}`.
- `floor(t,p)`: the largest char boundary of `t` that is ≤ `min(p,|t|)`.
- `Words(t,F)` = `words(t,F)` collected, a list of byte ranges. `Role(c) ∈ {Base, Mark, Prepend,
  Joiner, Break}` = `word::role`. `InWord(t,u,F)`: declarative membership of unit `u`, given by the
  rules in `word.rs`'s module doc and stated as W2.
- `Cost`: a ghost counter of `word::role` calls, the `ROLE_LOOKUPS` seam lifted to spec-only state.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps (audit §7)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
S1-graphemes-are-uax29	refinement	grapheme_iter::{Graphemes::next, find_first_cluster_end, is_break}, GraphemeClusters::graphemes	∀ s: G(s) = UAX29(s). Every item is non-empty, the items concatenate to s, and a boundary falls between chars i-1 and i exactly when Brk(s,i) holds: GB9c conjuncts; GB11 only after ExtPict Extend* ZWJ (a second ZWJ, or an Extend after the ZWJ, breaks); GB12/13 RI pairing by parity. Corollary (size_hint): |s|=0 ⇒ (0,Some(0)); otherwise 1 ≤ |G(s)| ≤ |s|.	uax29_conformance::uax29_official_conformance, where_unicode_segmentation_disagreed_this_crate_follows_unicode_16; grapheme_iter::test_ethiopic_combining, test_extpict_zwj_extpict, test_gb11_needs_extend_only_between_pictograph_and_zwj	HARDEST pure spec: &str slicing, char_indices loop with early return, Iterator::next(&mut self), a Copy state struct (u32 + bools), GCB/InCB enum matches, a 15×15 static bool matrix, two-level gcb_class arrays, incb_class binary_search_by with a closure; needs a declarative Lean UAX #29 over List Char	0,1,2,3,5,7,13,14
S2-indices-are-running-offsets	equivalence	grapheme_iter::GraphemeIndices::next, GraphemeClusters::grapheme_indices	∀ s: GI(s) = zip(prefix_sums(map len G(s)), G(s)); the k-th item is (Σ_{j<k} |G(s)[j]|, G(s)[k]). The pointer-difference offset equals the byte position of the slice in s, and the lengths sum to |s|.	uax29_conformance::grapheme_indices_agree_with_graphemes	MEDIUM: as_ptr as usize subtraction (needs a provenance / slice-of model; gap 8 for that model), Option::map with a closure, &mut self iterator	0,1,2,3,7,8,13
S3-boundary-class-reuse	equivalence	grapheme_iter::{find_first_cluster_end, Graphemes::next}	∀ non-empty s with first char c0: find_first_cluster_end(s, Some((gcb_class(c0), incb_class(c0)))) = find_first_cluster_end(s, None). A return (end, Some(k)) with end < |s| has k = (gcb_class, incb_class) of s[end..]'s first char; (end, None) ⇒ end = |s|. Hence the cached next_base in Graphemes::next yields the same clusters as reclassifying each cluster's first char.	grapheme_iter::test_boundary_class_reuse_across_clusters	MEDIUM: Option<(enum,enum)>, unwrap_or_else closure, char_indices loop, tuple return	0,1,3,5,7,13
S4-gcb-incb-tables-are-ucd	table-property	tables::gcb::{gcb_class, incb_class}	∀ c: gcb_class(c) = UCD16.Grapheme_Cluster_Break(c), with Extended_Pictographic from emoji-data taking precedence as the generator does; incb_class(c) = UCD16.InCB(c), None where unassigned. Both total and panic-free. INCB_RANGES is sorted and disjoint, so binary_search_by finds the one containing range.	grapheme_iter::test_plane14_tag_classification	EASY by evaluation once tables are carriers: [u16;4352] and [u8;13568] arrays with get().copied().unwrap_or, nibble extraction, a 15-way match, binary_search_by over [(u32,u32,u8);403]; exhaustive over 1,112,064 scalars; needs GraphemeBreakProperty.txt and DerivedCoreProperties.txt vendored	0,1,5,13
S5-char-width-table	table-property	tables::width::{char_width, char_width_cjk}	∀ c: char_width(c) = Wref(c): 0 when c is zero-width by the generator rule (C0/C1/DEL, Mn/Me/Cf, ZWSP/ZWJ/BOM, all of U+E0000..U+E0FFF, Hangul V/T U+1160..U+11FF, U+D7B0..U+D7C6, U+D7CB..U+D7FB); 2 when UCD16.EastAsianWidth(c) ∈ {W,F} (including Ext G/H U+30000..U+323AF and Ext I); 1 otherwise (including Ambiguous, U+00AD, the unassigned code points beside the Jamo, U+323B0, U+E1000). char_width_cjk(c) = the same with Ambiguous ↦ 2.	tests::test_char_width_table_boundaries, plane14_default_ignorables_are_zero_width, hangul_vowel_and_final_jamo_are_zero_width	EASY by evaluation: const fn, two-level static arrays ([u16;768], [u8;7104]) by shifts and masks, a match ending in unreachable!(), range matches; needs EastAsianWidth.txt and the zero-width rule as a Clean def	0,1,3,5
S6-cjk-differs-only-on-ambiguous	equivalence	tables::width::{char_width_cjk, char_width, is_ambiguous_width}	∀ c: char_width_cjk(c) = (Amb(c) ? 2 : char_width(c)), and Amb(c) ⇒ char_width(c) = 1. Side condition, decidable on the tables: no Hangul Extended-B V/T code point has raw class 3.	tests::test_char_width_cjk_widens_ambiguous	EASY: const fn, same table reads as S5, straight-line apart from range checks	0,1,3
S7-char-width-range-no-panic	no-panic/bounds	tables::width::{char_width, char_width_cjk, is_ambiguous_width}	∀ c: char_width(c) ∈ {0,1,2} and char_width_cjk(c) ∈ {0,1,2}. No index is out of bounds: ∀ cp < 0x30000, WIDTH_LEVEL1[cp>>8]·64 + (cp&0xFF)/4 < 7104. The unreachable!() arm is dead because raw = x & 3.	(none; lemma for S5, S9, S17)	EASY: array indexing with a bounds proof over table contents, u32 shifts and masks, unreachable!()	0,1,3
S8-str-width-is-sum	equivalence	tables::width::str_width	∀ s: str_width(s) = Σ_{c ∈ chars(s)} char_width(c).	(none directly; secondary of plane14_default_ignorables_are_zero_width)	EASY: Chars, map, sum (iterator adapter chain)	0,1,3,7,13
S9-grapheme-display-width-cases	contract	width::{grapheme_display_width, grapheme_display_width_inner, spacing_chars, is_emoji_char, is_skin_tone_modifier}	For chars(g) empty, W = 0. A lone control has W = 1; every other lone c has W = cw_a(c). For a multi-codepoint cluster, drop the leading zero-width prefix to find spacing base f and its remaining tail t; no base gives W = 0. Otherwise W = 2 if is_vs16_emoji_capable(f) and VS16 occurs in g, or if (is_emoji_char(f) or is_vs16_emoji_capable(f)) and (the next character is a skin tone or t contains ZWJ). Otherwise W = min(cw_a(f), 2). Always W is in 0..=2. A GB9b Prepend sign does not hide its base or emoji presentation.	tests::prepend_cluster_is_measured_at_its_base	MEDIUM: chars() stepped by hand, str::contains(char), Iterator::any with a closure, a selected fn pointer (char_width vs char_width_cjk), range matches, usize::min; composes with S1 for multi-cluster samples	0,1,3,7,13
S10-config-dispatch	equivalence	width::{grapheme_width_with_config, grapheme_display_width_with_config, split_graphemes_with_config, is_cjk}	∀ s, cfg: grapheme_width_with_config(s,cfg) = grapheme_width_inner(s, cfg.ambiguous_width = Double); likewise for grapheme_display_width_with_config and split_graphemes_with_config. TextShapingConfig::default().ambiguous_width ≠ Double, so the *_with_config(·, default) functions equal grapheme_width, grapheme_display_width, split_graphemes. With S6, S9: first spacing base of g not Amb ⇒ W(g,true) = W(g,false); a lone Amb non-control char has W(g,true) = 2, W(g,false) = 1.	-	EASY dispatch: &TextShapingConfig from aterm_types (cross-crate), matches! on an enum field, a cross-crate Default impl, struct equality	0,1,2,3,12
S11-grapheme-width-aggregate	equivalence	width::{grapheme_width, grapheme_width_inner, is_emoji_grapheme}	∀ s, cjk: grapheme_width_inner(s,cjk) = {byte_count = |s|, grapheme_count = |G(s)|, display_width = Σ_{g∈G(s)} W(g,cjk), codepoint_count = |chars(s)|, has_emoji = ∃g∈G(s). is_emoji_grapheme(g) (the first spacing base, or the first scalar if all are zero-width, passes is_emoji_char), has_combining = ∃g. |chars(g)|>1 ∧ W(g,cjk) ≤ 1, has_wide = ∃g. W(g,cjk) = 2}. In particular s = "" ⇒ GraphemeInfo::default().	tests::prepend_cluster_is_measured_at_its_base	MEDIUM: a for loop over Graphemes, a struct accumulator with += and |=, chars().count(); sample values follow by composing with S1, S9, S5	0,1,2,3,4,7,13
S12-split-graphemes-pointwise	equivalence	width::{split_graphemes, split_graphemes_inner}	∀ s, cjk: split_graphemes_inner(s,cjk) = map (λ(off,g). Grapheme{text: g, byte_offset: off, width: W(g,cjk), codepoint_count: |chars(g)|, is_emoji: is_emoji_grapheme(g) (the first spacing base, or the first scalar if all are zero-width, passes is_emoji_char), has_combining: |chars(g)|>1 ∧ W(g,cjk) ≤ 1}) GI(s). Corollary with S11: Σ width = display_width, count = grapheme_count, Σ codepoint_count = codepoint_count.	tests::test_split_graphemes_with_config_matches_aggregate; tests::prepend_cluster_is_measured_at_its_base	MEDIUM: impl Iterator + use<'a> returned from a fn, a move closure in map, a struct holding &'a str	0,1,2,7,11,13
S13-emoji-tables-are-ucd	table-property	emoji_presentation::{is_emoji_presentation, is_vs16_emoji_capable}, width::is_emoji_modifier_base	∀ c: is_emoji_presentation(c) ⇔ UCD16.Emoji_Presentation(c); is_vs16_emoji_capable(c) ⇔ UCD16.Emoji(c) ∧ ¬UCD16.Emoji_Presentation(c); is_emoji_modifier_base(c) ⇔ UCD16.Emoji_Modifier_Base(c) (134 elements, a corollary).	emoji_data::the_emoji_tables_are_the_ucds	EASY by evaluation: static &[(u32,u32)] slices, binary_search_by with a closure (via S14), a large matches! on u32 ranges; emoji-data as a Clean def (already vendored)	0,1,5,13
S14-in-ranges-membership	contract	emoji_presentation::in_ranges	requires SortedDisjoint(t) := ∀i. t[i].0 ≤ t[i].1 ∧ (i+1<|t| ⇒ t[i].1 < t[i+1].0); ensures in_ranges(t,cp) ⇔ ∃i. t[i].0 ≤ cp ≤ t[i].1. Table facts, decidable on the constants: SortedDisjoint holds for EMOJI_PRESENTATION, TEXT_DEFAULT_EMOJI, word::VIRAMA and word::JOINING, so every caller meets the precondition.	-	EASY-MEDIUM: slice binary_search_by with an Ordering closure (needs a std model), slice windows over static data	0,1,3,5,7,13
S15-virama-joining-tables-are-ucd	table-property	word::{is_virama, joins_cursively}	∀ c: is_virama(c) ⇔ UCD16.Canonical_Combining_Class(c) = 9; joins_cursively(c) ⇔ UCD16.Joining_Type(c) ∈ {D, L, R, C}.	word::the_virama_table_is_the_ucds_combining_class_9, the_joining_table_is_the_ucds_joining_letters	EASY by evaluation: in_ranges over a static slice (S14); two vendored UCD files as Clean defs	0,1,5,13
S16-small-char-predicates	table-property	width::{is_skin_tone_modifier, is_regional_indicator}, word::is_word_char	∀ c: is_skin_tone_modifier(c) ⇔ UCD16.Emoji_Modifier(c) (= U+1F3FB..=U+1F3FF); is_regional_indicator(c) ⇔ UCD16.Regional_Indicator(c) (= U+1F1E6..=U+1F1FF); is_word_char(c) ⇔ char::is_alphanumeric(c) ∨ c = '_'.	-	TRIVIAL: char range matches (an E6 decision tree once char is a carrier); is_word_char needs a std char model for is_alphanumeric	0,1
S17-grid-columns-refine-the-grid	refinement	grid::{GridColumns::step, GridColumns::advance_str, Cell::folds, Cell::store, str_grid_columns, str_grid_columns_with}	∀ s, a: GridCols_a(s) = the cursor column aterm-core's write path (handler_write: write_char, widen_previous_cell_for_vs16, narrow_previous_cell_for_vs15, try_combine_skin_tone_modifier, should_combine_with_previous_zwj, print_ascii_bulk) reaches writing s from column 0 under ambiguous_width a, on a line wide enough that nothing wraps. Locally, step is grid.rs's case function: a zero-width char joins the open cell; VS16 widens a narrow is_vs16_emoji_capable base by 1; VS15 narrows a wide cell and reopens at col+1; otherwise the char folds when Cell::folds holds (a skin tone onto a wide Emoji_Modifier_Base, a second RI onto a lone RI (ris < 2), or non-ASCII after a stored ZWJ, at most 16 joined); otherwise it opens a cell of width cw_a(c).	tests::skin_tone_grid_columns_follow_the_grids_fold, vs16_zwj_and_vs15_grid_columns_follow_the_grid, grid_columns_carry_fold_state_across_cluster_boundaries	HARD, cross-crate: &mut self state machine with Option<Cell>::as_mut, u8 saturating counters, an enum return; refinement against aterm-core's Terminal (today only aterm-core/tests/grid_column_agreement.rs links the two)	0,1,2,3,7,10,12
S18-grid-walk-additive	invariant	grid::{GridColumns::step, GridStep::advance, GridColumns::advance_str}	∀ st, c: col(step(st,c).0) = col(st) + step(st,c).1.advance(), with no saturation below usize::MAX. advance_str returns Σ advances and leaves col = col(st) + that sum. Hence GridCols_a(s ++ t) = GridCols_a(s) + (state after s).advance_str(t), and GridCols_a is monotone in prefix order.	(none; lemma for S20, S21)	EASY-MEDIUM: &mut self, enum match, saturating_add, a chars() loop	0,1,2,3,7
S19-vs16-width-agrees-with-grid	equivalence	width::grapheme_display_width, grid::str_grid_columns	∀ c with ¬c.is_control(): grapheme_display_width([c, U+FE0F]) = str_grid_columns([c, U+FE0F]). A finite family of 1.1M strings: decidable by evaluation once both sides are reflected, or by case analysis on cw(c) ∈ {0,1,2} and is_vs16_emoji_capable(c) from S9 and S17's step cases.	emoji_data::a_base_and_vs16_are_as_wide_as_the_grid_draws_them	MEDIUM: String building from a char array, two reflected callees (composition), char::is_control	0,1,2,4,5,7
S20-grid-graphemes-spans	contract	grid::{GridGraphemes::next, grid_graphemes, grid_graphemes_with}	∀ s, a: map (text, byte_offset) GG_a(s) = GI(s). For the k-th item (off,g), with st the GridColumns state after chars(s[..off]): end_col = col(state after chars(s[..off+|g|])); start_col = col(st), unless step(st, first(g)) = Folds{col}, then start_col = col. So start_col ≤ end_col, end_col is non-decreasing in k, and the last end_col = GridCols_a(s) (0 when s is empty).	(secondary only: tests::grid_columns_carry_fold_state_across_cluster_boundaries)	MEDIUM: a struct iterator holding GraphemeIndices plus GridColumns (&mut self), if-let on an enum, a chars loop	0,1,2,3,7,13
S21-position-converters-count-grid-cells	contract	position::{byte_to_column, byte_to_column_with, column_to_char_index}	∀ s, b, a: byte_to_column_with(s,b,a) = end_col of the last item of GG_a(s) with byte_offset < b, else 0. So it lies in 0..=GridCols_a(s), byte_to_column(s,|s|) = GridCols_Single(s), and a b inside a cluster maps to the column after that cluster. ∀ s, col: column_to_char_index(s,col) = Σ_{items before k} |chars(text)|, where k is the first item of GG_Single(s) with col < end_col, else |chars(s)|. A zero-column cluster (end_col = previous end_col) is never named by a column. Corollary (the corpus loop): ∀ item k with columns [c0,c1), column_to_char_index(s,c) = chars before item k for every c ∈ [prev_end, c1).	position_converters_count_grid_cells	MEDIUM given S20: a for loop with early return over GridGraphemes, saturating_add, chars().count()	0,1,2,3,4,7,13
W1-words-well-formed	invariant	word::{words, Words::next, Words::step}	∀ t, F: Fn(char)->bool: the ranges of Words(t,F) are non-empty, on char boundaries of t, strictly increasing and separated (r_k.end < r_{k+1}.start, with at least one not-in-word unit between). A range's first char never has Role Mark or Joiner. Each range is a maximal run of units u with InWord(t,u,F). step consumes ≥ 1 char per call and returns None exactly at |t|.	(none; lemma for W3, W4)	MEDIUM: a generic closure F, &mut self iterator, Option<char> state, Range<usize>, str::get(at..), chars, saturating_add	0,1,2,3,7,11,13
W2-word-membership-rules	contract	word::{Words::step, Words::spells_word, role, script_controls, ScriptControls::has_control, ScriptControls::has_letter}	∀ t, F, and each unit u of t (a char, a maximal Prepend run, or a maximal run of one script's own controls): Base c: InWord ⇔ F(c). Mark (GCB Extend/SpacingMark, or any non-control zero-width char): InWord ⇔ the previous unit is in a word. Break (a zero-width Control/CR/LF): never. Prepend run: InWord ⇔ the next non-Prepend char n has Role Base ∧ F(n). Script-control run (Mongolian U+180E, Egyptian U+13430..U+1343F, Duployan U+1BCA0..U+1BCA3): InWord ⇔ previous unit in a word ∧ the last letter is in that script's block ∧ the next char n is a letter of the script with Role Base ∧ F(n). ZWNJ/ZWJ j: InWord ⇔ previous unit in a word ∧ the next char continues (a Mark, or Base with F) ∧ (previous char is a virama ∨ next char is a virama ∨ the letters before and after, marks skipped, both joins_cursively ∨ (j = ZWJ ∧ both letters Extended_Pictographic)). Holds for every F: no Break or misplaced joiner is ever in a word.	word::a_joiner_belongs_to_a_word_only_where_it_spells_it, marks_attach_and_format_controls_separate	HARD: enum Role match, a generic closure, Chars clone/find with closures, Option combinators (is_some_and, then_some, filter), static [ScriptControls;3] iter().find; callees gcb_class/char_width/in_ranges (S4, S5, S14, S15)	0,1,2,3,4,7,11,13
W3-word-break-before-locality	contract	word::{word_break_before, floor_char_boundary}	∀ t, p, F, with at = word_break_before(t,p,F): at ≤ floor(t,p), at is a char boundary, and at is 0 or the offset of the last char x before floor(t,p) with Role(x) = Break ∨ (Role(x) = Base ∧ ¬F(x)). map (+at) Words(t[at..],F) = filter (start ≥ at) Words(t,F). ∀ r ∈ Words(t,F): r.end > p ⇒ r.start ≥ at. floor_char_boundary(t,p) = floor(t,p).	word::the_words_after_a_break_are_the_words_of_the_whole_text	MEDIUM-HARD: str::get(..), char_indices().rev().find with a closure, map_or, a while loop on is_char_boundary, a generic closure; the shift lemma needs W1/W2 over the suffix	0,1,3,5,7,11,13
W4-word-start-before	contract	word::word_start_before	∀ t, p, F: word_start_before(t,p,F) = the start of the last r ∈ Words(t,F) with r.start < floor(t,p), else None. Termination: each iteration strictly lowers end, or returns.	word::the_word_before_a_caret_is_the_last_word_that_starts_before_it	MEDIUM given W3: a nested loop with a decreasing measure, direct use of private Words fields (step/at/prev_in), a generic closure by reference, saturating_add	0,1,2,3,5,7,11,13
W5-word-walk-linear-cost	other (cost bound)	word::{words, Words::step, word_break_before, word_start_before, role}	With Cost = number of role calls: a full words(t,F).count() makes Cost ≤ 3·|chars(t)|; word_break_before(t,p,F) makes Cost ≤ 1 + #chars in (result, floor(t,p)); word_start_before(t,p,F) makes Cost ≤ c·(#chars from the break before the returned word to floor(t,p)) for a small constant c. The tests sample this as ≤ 3, ≤ 8 and ≤ 3·len.	word::the_walk_classifies_each_character_a_bounded_number_of_times	HARD today: needs ghost state / cost instrumentation, which the audit gap table does not list as its own row (closest: 15); the ROLE_LOOKUPS thread_local seam is its runtime stand-in and goes when the spec lands	0,1,2,7,11,15
```

### Distribution by kind (26)

| kind | count | ids |
|---|---|---|
| contract | 7 | S9, S14, S20, S21, W2, W3, W4 |
| equivalence | 8 | S2, S3, S6, S8, S10, S11, S12, S19 |
| table-property | 5 | S4, S5, S13, S15, S16 |
| refinement | 2 | S1, S17 |
| invariant | 2 | S18, W1 |
| no-panic/bounds | 1 | S7 |
| other | 1 | W5 (cost bound) |

Four specs (S7, S8, S18, W1) and S20 retire no test as primary. They are the lemmas the others are
built on, and are kept because the tests sample their consequences.

### Gap histogram (specs blocked by each audit §7 gap)

| gap | specs |
|---|---|
| 0 (seal publishes no Clean row) | 26 |
| 1 (non-scalar carriers) | 26 |
| 2 (`&self`/`&mut self` views) | 14 |
| 3 (E6 body widening) | 19 |
| 4 (callee-ensures composition) | 4 |
| 5 (quantified clauses) | 10 |
| 7 (loops) | 18 |
| 8 (pointer provenance model) | 1 |
| 10 (model→code refinement surface) | 1 |
| 11 (generics) | 6 |
| 12 (cross-crate citation) | 2 |
| 13 (closures, higher-order) | 17 |
| 14 (Clean elaborator coverage) | 1 |
| 15 (test→spec tooling; ghost cost) | 1 |

No spec is blocked by gap 0 alone: every one needs at least `char`/`&str` as a carrier (gap 1).
The cheapest set is gaps {0,1}: S16; then {0,1,3}: S6, S7.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/grapheme_iter.rs	test_ethiopic_combining	S1-graphemes-are-uax29	
src/grapheme_iter.rs	test_extpict_zwj_extpict	S1-graphemes-are-uax29	
src/grapheme_iter.rs	test_gb11_needs_extend_only_between_pictograph_and_zwj	S1-graphemes-are-uax29	
src/grapheme_iter.rs	test_boundary_class_reuse_across_clusters	S3-boundary-class-reuse	S1-graphemes-are-uax29 (comment mislabelled, §5)
src/grapheme_iter.rs	test_plane14_tag_classification	S4-gcb-incb-tables-are-ucd	
src/word.rs	the_virama_table_is_the_ucds_combining_class_9	S15-virama-joining-tables-are-ucd	S14-in-ranges-membership
src/word.rs	the_joining_table_is_the_ucds_joining_letters	S15-virama-joining-tables-are-ucd	S14-in-ranges-membership
src/word.rs	a_joiner_belongs_to_a_word_only_where_it_spells_it	W2-word-membership-rules	W1-words-well-formed
src/word.rs	marks_attach_and_format_controls_separate	W2-word-membership-rules	W1-words-well-formed
src/word.rs	the_walk_classifies_each_character_a_bounded_number_of_times	W5-word-walk-linear-cost	
src/word.rs	the_word_before_a_caret_is_the_last_word_that_starts_before_it	W4-word-start-before	W3-word-break-before-locality
src/word.rs	the_words_after_a_break_are_the_words_of_the_whole_text	W3-word-break-before-locality	W1-words-well-formed
src/tests.rs	skin_tone_grid_columns_follow_the_grids_fold	S17-grid-columns-refine-the-grid	
src/tests.rs	vs16_zwj_and_vs15_grid_columns_follow_the_grid	S17-grid-columns-refine-the-grid	
src/tests.rs	grid_columns_carry_fold_state_across_cluster_boundaries	S17-grid-columns-refine-the-grid	S20-grid-graphemes-spans
src/tests.rs	position_converters_count_grid_cells	S21-position-converters-count-grid-cells	S20-grid-graphemes-spans
src/tests.rs	test_char_width_cjk_widens_ambiguous	S6-cjk-differs-only-on-ambiguous	S5-char-width-table
src/tests.rs	test_char_width_table_boundaries	S5-char-width-table	S7-char-width-range-no-panic
src/tests.rs	plane14_default_ignorables_are_zero_width	S5-char-width-table	S8-str-width-is-sum
src/tests.rs	hangul_vowel_and_final_jamo_are_zero_width	S5-char-width-table	
src/tests.rs	test_split_graphemes_with_config_matches_aggregate	S12-split-graphemes-pointwise	S11-grapheme-width-aggregate
tests/emoji_data.rs	the_emoji_tables_are_the_ucds	S13-emoji-tables-are-ucd	
tests/emoji_data.rs	a_base_and_vs16_are_as_wide_as_the_grid_draws_them	S19-vs16-width-agrees-with-grid	S9-grapheme-display-width-cases, S17-grid-columns-refine-the-grid
tests/uax29_conformance.rs	uax29_official_conformance	S1-graphemes-are-uax29	
tests/uax29_conformance.rs	grapheme_indices_agree_with_graphemes	S2-indices-are-running-offsets	S1-graphemes-are-uax29
tests/uax29_conformance.rs	where_unicode_segmentation_disagreed_this_crate_follows_unicode_16	S1-graphemes-are-uax29	
```

## 3. Coverage

- 119 tests read. 98 map to one of the 26 specs. 21 are not spec samples (§4).
- The biggest collapses: S9 grapheme display width (22 primary), S11 aggregate (16), S21 position
  converters (14), S10 config dispatch (8), S1 UAX #29 (6) and S13 emoji tables (6).
- S1 has 6 primary tests but carries the official `GraphemeBreakTest` corpus; it is also the
  segmentation half of S2, S3, S11, S12, S20 and S21.

## 4. Tests that are not spec samples

The 21 tests that sampled no production spec are deleted, together with the `#[cfg(test)]`-only API
they exercised, which had no production caller in the workspace.

## 5. Implied specs that look wrong, or disagree with the code

1. **Two width rules disagree on every lone control.** `grapheme_display_width_inner` returns 1 by an
   explicit branch for a lone C0/DEL/C1 (`tests::single_cluster_widths` pins it for NUL, ESC and
   DEL), while `char_width('\x00') = 0` and `GridColumns` gives a control 0 columns. So
   `grapheme_display_width` and `str_grid_columns` disagree on every lone control;
   `tests/emoji_data.rs` filters controls out to stay green (S19's `¬c.is_control()`). The owner
   should pick one rule.
2. **Mislabelled prose.**
   - `grapheme_iter::test_boundary_class_reuse_across_clusters`: the third case's comment says "ExtPict
     boundary then standalone ZWJ+ExtPict", but the input U+1F600 U+1F601 has no ZWJ.
3. **Missing oracles.** S4 and S5 cannot be stated against the UCD until `GraphemeBreakProperty.txt`,
   `DerivedCoreProperties.txt` and `EastAsianWidth.txt` are vendored beside the files already in
   `tests/data`, and the generator's zero-width rule is written as a Clean def.
4. **A runtime seam stands in for a cost spec.** W5's tests read the `ROLE_LOOKUPS` thread_local. The
   spec needs a ghost counter, which no audit §7 row names; once W5 is Certified the seam goes.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone; each needs `char`/`&str` carriers (gap 1). Within that:

1. **S16 small char predicates** (gaps 0,1). Range matches on `char`, an E6 decision tree. Retires 2
   tests and is the first `char`-carrier probe.
2. **S7 char-width range and no-panic, then S6 CJK-vs-default** (gaps 0,1,3). Const fns with table
   reads, shifts and masks; S7 is the bounds lemma for every width spec, S6 retires 1 test and feeds S10.
3. **S5 width table, then S13, S15, S4** (table equalities, decidable by evaluation over 0x110000
   scalars once tables are carriers). S13 and S15 have their oracles vendored already (8 tests); S5
   (4 tests) and S4 (1) first need the missing UCD files (§5.3). **S14 in_ranges** is the shared lemma
   for S13/S15 and retires 1.
4. **S8 str_width fold** (lemma).
5. **S9 grapheme display width** — the single highest-value spec: 22 tests, straight case analysis over
   S5/S6/S13/S16 plus a `chars()` walk.
6. **S10 config dispatch** (8 tests; needs cross-crate `TextShapingConfig`, gap 12).
7. **S2 indices, S3 class reuse.**
8. **S1 UAX #29 refinement** — the hardest pure spec; it unlocks the exact sample values of S11, S12,
   S20, S21.
9. **S11 aggregate (16 tests), S12 split pointwise (4).**
10. **S18 grid walk, S20 grid spans, S21 position converters** (14 tests).
11. **S19 VS16 width vs grid**, after S9 and S17's local step cases.
12. **S17 grid refinement** — cross-crate against aterm-core's `handler_write` (gaps 10, 12).
13. **W1–W4 word walk** — generic closures (gap 11); W1 → W2 → W3 → W4.
14. **W5 cost bound** — needs ghost state; last.
