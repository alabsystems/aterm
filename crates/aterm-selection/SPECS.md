<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-selection: spec recovery from its 146 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 88 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Audit amendment: The 2026-10-16 code audit made scroll/splice's upper bound exclusive, corrected cached-bound advice, and removed the unavailable public custom-rule claim. The dated test inventory and §5 observations below describe the earlier snapshot; the scroll/splice contract rows reflect the fix.

> Status 2026-10-15: this is the crate's migration worklist. Each of the 34 specs below gets
> proved through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted. The 8 tests in §4 sample no spec and can be deleted now.

Source: `crates/aterm-selection` (read only). I read every `#[test]`:
`src/rules_tests.rs` (39), `src/tests.rs` (30), `src/text_selection/tests.rs` (75) and
`tests/conformance_text_selection.rs` (2), 146 in all. I also read the code they reach:
`rules.rs`, `rules_test_accessors.rs` (`cfg(test)` only), `builtin_patterns.rs`,
`text_selection/mod.rs`, `crates/aterm-types/src/selection.rs` (`selection_contains_linear`,
`selection_contains_block`, `glyph_cell_span`) and aterm-grapheme's `word.rs` and `grid.rs`.
Nothing was compiled or run. The claims below come from reading the code.

Result: **34 specs**, covering **138 tests**. **8 tests are not spec samples** (§4).

## 0. Notation used in the statements

Smart selection (`rules.rs`, `builtin_patterns.rs`):

- `Rules(S)` = `S.rules`, the `Vec` in its stored order. `R_b` = `BuiltinRules::all()` =
  [url, file_path, email, ipv4, ipv6, git_hash, double_quoted, single_quoted, backtick_quoted, uuid,
  semver]. Priorities: High = {url, email, uuid}; Low = {ipv6, semver}; every other rule is Normal.
- `FI(r,t)` = `r.pattern.find_iter(t)`, the leftmost-first, non-overlapping match sequence of
  aterm-regex. `Lang(P)` is the language of regex `P`. The pattern specs assume aterm-regex's own
  spec (find_iter refines leftmost-first backtracking semantics) and cite it (gap 12).
- `Hit(S,t,p)` = the first `r` in `Rules(S)` with some `m ∈ FI(r,t)` with
  `m.start ≤ p < m.end`, paired with that `m`. It is None if `p > |t|` or no such rule exists.
  The production function is `rule_match_at`. The test-only `find_at` is a thin view of it.
- `W_s(c)` = `is_word_char(s,c)`: for `s = None`, `c.is_alphanumeric() ∨ c = '_'`; for
  `s = Some(z)`, `¬z.contains(c)`.
- `Words(t,f)` = the byte ranges `aterm_grapheme::words(t,f)` yields. Which Unicode characters
  (marks, prepends, joiners, bidi and format controls) join or break a word is aterm-grapheme's
  spec, not this crate's, and is cited here (gap 12).
- `floor(t,i)` = the largest char boundary `≤ i`.
- `G_a(t)` = the list `aterm_grapheme::grid_graphemes_with(t,a)` returns, each entry
  `(byte_offset, start_col, end_col)`. `Cols(t)` = the last entry's `end_col` (`str_grid_columns`).

Text selection (`text_selection/mod.rs`):

- A `TextSelection` `sel` is `(state, type, start, end, truncated)`. An anchor is
  `(row: i32, col: u16, side)` with `Left < Right`.
- `≤lex` / `<lex` = lexicographic order on `(row, col, side)` (or on `(row, col)` where only two
  components are given).
- `NS`/`NE` = `normalized_start`/`normalized_end`. `SAB(sel)` = `side_adjusted_bounds(sel)`.
- `⊖`, `⊕` = `i32` saturating sub and add (`⊖u16`, `⊕u16`, `⊖64` for those widths).
- `Clamp(sel,m)` = the private `clamp_head_to_min_row(m)`, which every eviction entry point shares.
- `Reach` = the `TextSelection`s reachable from `TextSelection::new()` by any sequence of the
  public `&mut` methods.
- The lifecycle model is `aterm_models_core::text_selection_lifecycle_model`, with the projection
  `conformance_text_selection::project`.

Gap 0 (the installed seal publishes no Lean-discharged row) is a prerequisite for every spec, so
every row lists it. A spec is "blocked only by gap 0" when its other gaps are empty. None of the 34
is: every one needs at least gap 1 (non-scalar carriers) or gap 2 (`&self`/`&mut self` views).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps` (numbers from
`docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7).

```tsv
id	kind	functions	statement	subsumes (primary)	difficulty (Rust features the proof must handle)	blocking gaps (audit §7)
pat-url	refinement	builtin_patterns::BuiltinRules::url, rules::SelectionRule::find_at_position	Lang(URL_PATTERN) is exactly the strings matching `(?i)(http|https|ftp|file)` `://` B* L, where B is any char outside whitespace and outside `< > [ ] ( ) { } ' " ` , ; | \ ^`, and L is any B char outside `. ! ? :`. Corollaries: no FI(url,t) match contains whitespace, a bracket or a quote; no match ends in trailing sentence punctuation; every match starts with one of the four schemes followed by `://` (so `mailto:`, `//host` and scheme-less domains never match). Under leftmost-first greedy matching, the match at a scheme occurrence is the longest B*L run after it.	src/rules_tests.rs::builtin_patterns_reject_lookalikes	regex engine (aterm-regex Pike VM) as a cross-crate callee, &str, find_iter iterator, closures; a quantified regular-language statement	0, 1, 5, 7, 12
pat-file-path	refinement	builtin_patterns::BuiltinRules::file_path	Lang(FILE_PATH_PATTERN) = (prefix? ('/' S)+ '/'?) ∪ (drive ':' [/\\] (S [/\\])* S), with S = [A-Za-z0-9._-]+, prefix ∈ {'~', '.', '..', S}, drive = [A-Za-z]. ∀ t: (a) every match contains a separator, so a slash-free token never matches; (b) for a token X = S('/'S)+ preceded by a non-S char or the start of t, the match at X starts at X's first byte, never at its first '/' (leftmost-first plus the optional bare-segment prefix).	src/rules_tests.rs::test_file_path_bare_relative_is_whole_not_fabricated_absolute	regex engine as cross-crate callee, &str, iterators	0, 1, 5, 7, 12
pat-email	refinement	builtin_patterns::BuiltinRules::email	Lang(EMAIL_PATTERN) = local '@' label ('.' label)* '.' tld, with local = [A-Za-z0-9._%+-]+, label = an alphanumeric char optionally followed by [A-Za-z0-9-]* and an alphanumeric char, tld = [A-Za-z]{2,}. So a match has exactly one '@', a non-empty local part, a domain that starts alphanumeric and at least one '.' after the '@'; a t with no substring of that shape has empty FI(email,t).	-	regex engine as cross-crate callee, &str	0, 1, 5, 7, 12
pat-ipv4	refinement	builtin_patterns::BuiltinRules::ipv4	Lang(IPV4_PATTERN) = o '.' o '.' o '.' o (':' d{1,5})?, where o is a decimal octet in 0..=255 written with at most 3 digits (`25[0-5]` ∪ `2[0-4]d` ∪ `[01]?d d?`). A match has exactly four dot-separated octets, each ≤ 255, plus an optional port; FI(ipv4,t) is empty on a run with fewer than four octets or no 4-octet window of values ≤ 255 (e.g. 999.999.999.999).	src/rules_tests.rs::builtin_patterns_match_their_tokens	regex engine as cross-crate callee, &str	0, 1, 5, 7, 12
pat-hex-ids	refinement	builtin_patterns::BuiltinRules::git_hash, builtin_patterns::BuiltinRules::uuid	Lang(GIT_HASH_PATTERN) = \b H{7,40} \b and Lang(UUID_PATTERN) = H{8} '-' H{4} '-' H{4} '-' H{4} '-' H{12}, H = [0-9a-fA-F]. Every git_hash match is a whole word of 7..=40 hex chars (a shorter hex run never matches); every uuid match is exactly 36 bytes in the 8-4-4-4-12 shape (truncated, unhyphenated or non-hex strings never match).	-	regex engine with word-boundary assertions, &str	0, 1, 5, 7, 12
pat-quoted	refinement	builtin_patterns::BuiltinRules::{double,single,backtick}_quoted_string	∀ q ∈ {'"', '\'', '`'}: Lang = q ([^q\\] ∪ '\\' any)* q. A match starts and ends with q, and every q strictly inside it is escaped by an odd-length backslash run; at a q that opens a match, the match ends at the first unescaped q after it.	-	regex engine as cross-crate callee, &str	0, 1, 5, 7, 12
pat-semver	refinement	builtin_patterns::BuiltinRules::semver	Lang(SEMVER_PATTERN) = 'v'? N '.' N '.' N ('-' A ('.' A)*)? ('+' A ('.' A)*)?, N = \d+, A = [A-Za-z0-9]+. Greedy matching takes the full pre-release and build suffixes.	-	regex engine as cross-crate callee, &str	0, 1, 5, 7, 12
builtin-rule-table	table-property	rules::SmartSelection::with_builtin_rules, builtin_patterns::BuiltinRules::all, rules::SmartSelection::add_rule	Rules(with_builtin_rules()) has length 11 and is exactly [url, email, uuid, file_path, ipv4, git_hash, double_quoted, single_quoted, backtick_quoted, ipv6, semver] = R_b stable-sorted by priority, highest first. Each entry has the (name, kind) its constructor sets (url/Url, email/Email, uuid/Uuid, file_path/FilePath, ipv4/IpAddress, git_hash/GitHash, the three quoted/QuotedString, ipv6/IpAddress, semver/SemVer).	-	Vec of structs holding String and a compiled Regex, stable sort with a key closure, enum priorities	0, 1, 3, 7, 13
add-rule-sorted	invariant	rules::SmartSelection::add_rule	∀ S, r: Rules(add_rule(S,r)) = stable_sort_by(priority desc, Rules(S) ++ [r]). Invariant: Rules(S) is non-increasing in priority, and within one priority class rules keep insertion order.	-	Vec push, stable sort_by_key with a Reverse key closure, &mut self	0, 1, 2, 7, 13
rule-precedence	contract	rules::SmartSelection::rule_match_at, rules::SelectionRule::find_at_position, rules::SmartSelection::word_boundaries_at	∀ S, t, p: rule_match_at(S,t,p) = Hit(S,t,p) (None when p > |t| or no rule's FI match contains p, which covers every p on "" and p = |t|). word_boundaries_at(S,t,p) = (m.start, m.end) if Hit(S,t,p) = Some((_,m)), else basic_word_boundaries_with(S.word_separators, t, p); word separators therefore never change a rule hit. With add-rule-sorted and the pattern specs: uuid wins over git_hash at any byte of a UUID, and git_hash still wins on plain 7..=40-hex words, which uuid never matches.	src/rules_tests.rs::uuid_outranks_git_hash_on_first_and_last_hex_groups, src/rules_tests.rs::standalone_git_hash_still_matches_with_uuid_at_high, src/rules_tests.rs::word_separators_do_not_override_smart_rules, src/tests.rs::builtin_rules_select_the_token_under_the_cursor, src/tests.rs::test_no_match_at_position, src/tests.rs::test_word_boundaries_with_match, src/tests.rs::test_word_boundaries_fallback	iterator filter/find_map with closures over Vec<SelectionRule>, Option<(&SelectionRule, Match<'t>)> carrying lifetimes, &str, calls into aterm-regex	0, 1, 3, 4, 5, 7, 12, 13
word-classifier	contract	rules::is_word_char, rules::SmartSelection::set_word_separators	∀ c: W_None(c) = (c.is_alphanumeric() ∨ c = '_'); ∀ z, c: W_Some(z)(c) = ¬z.contains(c). set_word_separators(x) sets word_separators := x.map(to_owned) and changes nothing else, so clearing it with None restores the default classifier exactly.	-	std char classification, &str::contains, Option<String>, &mut self	0, 1, 2, 3, 12
basic-word-is-containing-word	contract	rules::SmartSelection::basic_word_boundaries_with (cites aterm_grapheme::{words,word_break_before})	∀ s, t, p: basic_word_boundaries_with(s,t,p) = None if p > |t| or t = "". Otherwise, with q = floor(t, min(p, |t|−1)), it is Some((w.start, w.end)) for the unique w ∈ Words(t, W_s) with w.start ≤ q < w.end, and None if no word contains q (by the locality lemma of word_break_before: the words of t[from..] shifted by from are the words of t starting at or after from). Consequences: every byte of a multi-byte char gives the same answer; a separator or breaking control gives None; any byte of a word gives that whole word; under Some(" ") a hyphenated token is one word. Which characters join or break a word is aterm-grapheme's spec, cited here.	src/rules_tests.rs::basic_word_boundaries_over_scripts, src/rules_tests.rs::a_joiner_that_spells_a_word_belongs_to_it, src/rules_tests.rs::an_invisible_character_between_two_words_still_splits_them, src/rules_tests.rs::word_separators_make_a_word_any_run_of_non_separators, src/rules_tests.rs::word_separators_keep_combining_marks_and_zero_width_breakers	&str char boundaries, a reverse range search, generic Words<F> iterator with a closure classifier, cross-crate callee specs, Option	0, 1, 4, 5, 7, 11, 12, 13
column-to-byte	contract	rules::column_to_byte_pos_with	∀ t, c, a: column_to_byte_pos_with(t,c,a) = g.byte_offset for the first g ∈ G_a(t) with c < g.end_col, else |t|. Since G_a's end_cols are non-decreasing, this is the start byte of the grapheme drawn in column c (a column inside a multi-cell or folded grapheme gives that grapheme's start); every c ≥ Cols(t) gives |t|.	-	for loop over a cross-crate grapheme iterator, &str	0, 1, 7, 12
byte-span-to-columns	contract	rules::byte_span_to_columns_with	∀ t, b0, b1, a: let P = the prefix of G_a(t) with byte_offset < b1; end = end_col of P's last entry (0 if P is empty); start = start_col of the first g ∈ P with byte_offset ≥ b0 (end if none). Result = (start, end). When b0..b1 is a union of whole graphemes, this is the half-open column range they are drawn in (a folded grapheme's start_col is the cell it joined).	-	loop with break and an Option accumulator over a cross-crate iterator	0, 1, 7, 12
word-at-column	contract	rules::SmartSelection::word_boundaries_at_column{,_with}	∀ S, t, c, a: let b = column_to_byte_pos_with(t,c,a). word_boundaries_at_column_with(S,t,c,a) = None if b ≥ |t|, else word_boundaries_at(S,t,b).map(|(x,y)| byte_span_to_columns_with(t,x,y,a)); word_boundaries_at_column is the Single variant. Corollaries: c ≥ Cols(t) gives None (every c on ""); on text of single-width one-char graphemes it equals word_boundaries_at(S,t,c); every column of a word's cells gives that word's cell span.	src/rules_tests.rs::word_columns_follow_the_grids_fold_across_clusters, src/rules_tests.rs::test_word_boundaries_at_column_cjk_with_space, src/rules_tests.rs::test_word_boundaries_at_column_past_the_text_is_none, src/rules_tests.rs::a_prepended_mark_belongs_to_the_word_after_it	composition of callee ensures (column-to-byte, rule-precedence, byte-span-to-columns), Option::map, &str	0, 1, 4, 7, 12
anchor-total-order	contract	text_selection::SelectionAnchor::{cmp,partial_cmp}	∀ a, b: a.cmp(b) = (a.row, a.col, a.side).cmp((b.row, b.col, b.side)) with Left < Right, and partial_cmp = Some(cmp). This is a total order consistent with the derived Eq: reflexive, antisymmetric (cmp(a,b) = cmp(b,a).reverse()), transitive, and a = b ⇔ cmp(a,b) = Equal.	src/text_selection/tests.rs::test_anchor_ordering	tuple lexicographic Ord on (i32, u16, enum), SelectionSide's derived Ord from aterm-types, trait impls	0, 1, 3, 5, 11, 12
lifecycle-refines-model	refinement	text_selection::TextSelection::{new,start_selection,update_selection,complete_selection,clear,extend_selection,has_selection}	new() = (None, Simple, (0,0,L), (0,0,L), false). start_selection(r,c,s,k): from any state, state := InProgress, type := k, start = end := (r,c,s), truncated := false. update_selection(r,c,s): end := (r,c,s) iff state = InProgress, else identity. complete_selection: InProgress → Complete, else identity. clear: state := None, truncated := false, type and anchors unchanged. extend_selection acts iff state = Complete (end := (r,c,s), state := InProgress, sides per extend-anchor-side), else identity. has_selection ⇔ state ≠ None. Under conformance_text_selection::project, every call refines the matching action of aterm_models_core::text_selection_lifecycle_model at Buggy = 0 from every state in Reach; the Buggy = 1 model mutant and a mutated body fail the refinement (negative control).	src/text_selection/tests.rs::test_extend_selection, tests/conformance_text_selection.rs::real_calls_conform_to_the_lifecycle_model, tests/conformance_text_selection.rs::a_dropped_phase_check_is_rejected	&mut self two-state contracts, enum state, struct update, model→code refinement	0, 1, 2, 10
extend-anchor-side	contract	text_selection::TextSelection::extend_selection	∀ sel with state = Complete and type ≠ Block, ∀ m = (r,c,s): start.side' = Right if m <lex (start.row, start.col, Right), else Left; start.row and start.col unchanged; end' = m. For Block, start is unchanged. Consequence (Simple, Semantic): the pressed cell (start.row, start.col) stays inside SAB on both sides of a cross-anchor extension.	src/text_selection/tests.rs::test_extend_selection_preserves_anchor_cell_when_crossing_left, src/text_selection/tests.rs::test_extend_selection_preserves_anchor_cell_when_crossing_right	&mut self, anchor Ord comparison, enum conditionals	0, 1, 2, 3
is-empty	contract	text_selection::TextSelection::is_empty	∀ sel: is_empty(sel) ⇔ state = None ∨ start = end (field equality on (row, col, side)).	src/text_selection/tests.rs::is_empty_iff_none_or_the_anchors_coincide	&self view, struct PartialEq	0, 1, 2
normalize	contract	text_selection::TextSelection::{normalized_start,normalized_end,normalized_bounds}	type = Block: NS = (min rows, min cols, Left), NE = (max rows, max cols, Right). Other types: NS = min_lex(start,end), NE = max_lex(start,end), so NS ≤lex NE and {NS,NE} = {start,end}. normalized_bounds = (NS.row, NS.col, NE.row, NE.col). Corollary (direction invariance): two selections of one type with the same unordered anchor pair (non-Block), or the same row and col extents (Block), have equal NS, NE, SAB, contains and contains_cell.	src/text_selection/tests.rs::a_backwards_drag_snaps_the_same_wide_glyph_edges_as_a_forwards_one	&self view, i32/u16 min/max, anchor Ord	0, 1, 2, 3
side-adjusted-bounds	contract	text_selection::TextSelection::{side_adjusted_bounds,apply_side_adjustment}	SAB(sel) = None if state = None. type = Lines: Some((NS.row, 0, NE.row, u16::MAX)) whatever the cols and sides. Otherwise sc = NS.col ⊕u16 [NS.side = Right]; (er,ec) = (NE.row, NE.col) if NE.side = Right; (NE.row, NE.col−1) if NE.side = Left ∧ NE.col > 0; (NE.row−1, u16::MAX) if NE.side = Left ∧ NE.col = 0 ∧ NE.row > i32::MIN, and SAB = None in that case when NE.row = i32::MIN. SAB = None if (NS.row, sc) >lex (er, ec), else Some((NS.row, sc, er, ec)). Corollary: NS.side = Left ∧ NE.side = Right ∧ type ≠ Lines (always so for Block) ⇒ SAB = Some(normalized_bounds).	src/text_selection/tests.rs::a_linear_end_before_the_first_representable_row_is_empty, src/text_selection/tests.rs::precomputed_bounds_equivalent_to_contains	&self view, Option, checked_sub, saturating_add, tuple comparisons	0, 1, 2, 3
contains-region	contract	text_selection::TextSelection::contains, aterm_types::selection::{selection_contains_linear,selection_contains_block}	∀ sel, r, c: contains(sel,r,c) = false if SAB(sel) = None. Otherwise, with SAB = (sr,sc,er,ec): Block gives sr ≤ r ≤ er ∧ sc ≤ c ≤ ec; every other type gives (sr,sc) ≤lex (r,c) ≤lex (er,ec) (selection_contains_linear: middle rows are full, the first row runs from sc to the right edge, the last row from 0 to ec). A Left end at col 0 therefore excludes its whole row, and a same-anchor Simple selection contains nothing.	src/text_selection/tests.rs::test_contains_simple, src/text_selection/tests.rs::test_contains_multiline, src/text_selection/tests.rs::test_contains_block, src/text_selection/tests.rs::test_contains_end_left_side_col_zero_multiline, src/text_selection/tests.rs::test_contains_block_end_left_side_col_zero	&self view, cross-crate pure callee (aterm-types), u16→usize widening, enum match	0, 1, 2, 3, 4, 12
project-range	contract	text_selection::TextSelection::project_range	∀ sel, lc: project_range(sel,lc) = None ⇔ SAB(sel) = None. Otherwise, with SAB = (sr,sc,er,ec): Lines gives {sr, 0, er, lc, is_block: false}; any other type gives {sr, sc, er, ec, is_block: type = Block}. Hence project_range and contains agree on the selected region.	src/text_selection/tests.rs::project_range_projects_every_shape, src/text_selection/tests.rs::lines_bounds_projection_and_hits_ignore_anchor_columns_and_sides	&self view, Option<struct>, callee composition	0, 1, 2, 4
contains-cell-glyph-whole	contract	text_selection::TextSelection::contains_cell, aterm_types::selection::glyph_cell_span	∀ sel, r, c, w, k: let (l,u) = glyph_cell_span(c,w,k) = (c ⊖u16 1, c) if k; (c, c ⊕u16 1) if w ∧ ¬k; (c,c) otherwise. contains_cell(sel,r,c,w,k) = contains(sel,r,l) ∨ contains(sel,r,u). So a glyph's lead and continuation always get the same answer: a glyph is selected iff either of its cells is; state = None gives false for every cell.	src/text_selection/tests.rs::a_linear_drag_starting_on_a_wide_glyphs_continuation_still_paints_its_lead, src/text_selection/tests.rs::a_linear_drag_ending_on_a_wide_glyphs_lead_still_paints_its_continuation, src/text_selection/tests.rs::a_lines_selection_paints_every_half_of_every_glyph_on_its_rows, src/text_selection/tests.rs::a_block_selects_a_wide_glyph_either_of_whose_cells_is_inside, src/text_selection/tests.rs::test_contains_cell_block_continuation_at_col_zero	&self view, cross-crate const fn callee, u16 saturating ops, short-circuit ∨	0, 1, 2, 3, 4, 12
include-all	contract	text_selection::TextSelection::include_all	state = None: identity. Otherwise, if start ≤lex end, start.side := Left and end.side := Right, else start.side := Right and end.side := Left; rows, cols, type, state and truncated unchanged. Corollary (Simple, Semantic): SAB' = Some((first.row, first.col, last.row, last.col)) with first/last the lex-min/lex-max of the old (row, col) pairs, so both original cells are contained.	src/text_selection/tests.rs::test_include_all_expands_sides, src/text_selection/tests.rs::include_all_covers_both_endpoints_for_either_drag_direction	&mut self two-state, anchor Ord	0, 1, 2
clamp-head	contract	text_selection::TextSelection::clamp_head_to_min_row (private; Clamp)	∀ sel, m: state = None ⇒ returns true, unchanged. Let lo = min(start.row, end.row), hi = max. lo ≥ m ⇒ true, unchanged. Else if m ≥ 0 ∨ hi < m ⇒ cleared (state None, ¬truncated), returns false. Otherwise let A be the lower anchor (start if start.row ≤ end.row, else end): A.row := m; if type ≠ Block also A.col := 0, A.side := Left; truncated := true. Then SAB = None ⇒ cleared, returns false; else returns true with state, type and the other anchor unchanged.	src/text_selection/tests.rs::test_adjust_for_scroll_deep_scrollback_survives_then_evicts, src/text_selection/tests.rs::test_adjust_for_scroll_clamps_the_evicted_end_anchor_on_an_upward_drag, src/text_selection/tests.rs::test_adjust_for_scroll_clears_when_the_clamp_collapses_the_span	&mut self, a &mut to one of two fields chosen at runtime, calls to SAB and clear (callee ensures)	0, 1, 2, 3, 4
adjust-for-scroll	contract	text_selection::TextSelection::adjust_for_scroll	∀ sel, d, M, f: state = None ⇒ true, unchanged. M ≤ 0 ⇒ clear, false. Let s' = start.row ⊖ d, e' = end.row ⊖ d; max(s',e') ≥ M ⇒ clear, false. Otherwise start.row := s', end.row := e' (cols, sides, type, state unchanged) and the result is Clamp(·, f.saturating_neg()). Corollaries: no overflow for any d (including i32::MAX); the kept window is [−f, M), because M is the live row count; InProgress stays InProgress.	src/text_selection/tests.rs::adjust_for_scroll_shifts_or_clears_at_the_bounds, src/text_selection/tests.rs::test_adjust_for_scroll_in_progress_selection	&mut self, i32 saturating_sub/neg, callee composition (clamp-head)	0, 1, 2, 3, 4
truncate-to-floor	contract	text_selection::TextSelection::truncate_to_floor	∀ sel, o, L: truncate_to_floor(sel,o,L) = Clamp(sel, min(m, 0)) where m = i32::try_from(sat_i64(o) ⊖64 sat_i64(L)).unwrap_or(i32::MIN) and sat_i64(x) = i64::try_from(x).unwrap_or(i64::MAX).	src/text_selection/tests.rs::truncate_to_floor_over_every_small_span	u64→i64→i32 try_from with unwrap_or, i64 saturating_sub, &mut self	0, 1, 2, 3, 4
row-splice	contract	text_selection::TextSelection::adjust_for_row_splice	∀ sel, b, n, M, f: state = None ⇒ true, unchanged. M ≤ 0 ∨ n < 0 ⇒ clear, false. remap(x) = x ⊖ n if x < b, else x; s' = remap(start.row), e' = remap(end.row). max(s',e') ≥ M ⇒ clear, false. Otherwise the rows become s', e' (everything else unchanged, including InProgress and the raw anchor order) and the result is Clamp(·, f.saturating_neg()).	src/text_selection/tests.rs::test_adjust_for_row_splice_moves_history_endpoint_but_keeps_footer_endpoint, src/text_selection/tests.rs::test_adjust_for_row_splice_handles_reverse_and_in_progress_anchors, src/text_selection/tests.rs::test_adjust_for_row_splice_clears_only_after_real_history_eviction	&mut self, a local closure, saturating ops, callee composition	0, 1, 2, 3, 4, 13
rows-shrink	contract	text_selection::TextSelection::adjust_for_rows_shrink	∀ sel, k, r, f: state = None ⇒ true, unchanged. k ≤ 0 ∨ r ≤ 0 ⇒ = adjust_for_scroll(sel, 0, k, f); else = adjust_for_scroll(sel, r, k, f). Corollary: a rows-only shrink is the uniform monotone relabel x ↦ x ⊖ r, so start.row ≤ end.row is preserved whenever the call returns true; k ≤ 0 always clears.	src/text_selection/tests.rs::test_adjust_for_rows_shrink_relabels_every_row_by_the_same_amount, src/text_selection/tests.rs::a_shrink_spares_a_span_that_crosses_the_cut	&mut self, callee composition	0, 2, 4
translate-rows	contract	text_selection::TextSelection::translate_rows_for_presentation	∀ sel, d: start.row' = start.row ⊕ d, end.row' = end.row ⊕ d (saturating at i32::MIN/MAX); every other field (cols, sides, type, state, truncated) unchanged, in every state.	src/text_selection/tests.rs::test_translate_rows_for_presentation_preserves_selection_semantics, src/text_selection/tests.rs::test_translate_rows_for_presentation_saturates_at_i32_boundaries	&mut self two-state on struct fields, i32 saturating_add	0, 2, 3
expand-gated	contract	text_selection::TextSelection::{expand_semantic,expand_lines}	expand_semantic(a,b): if state = InProgress ∧ type = Semantic, start := (start.row, a, Left), end := (end.row, b, Right), else identity. expand_lines(w): if state = InProgress ∧ type = Lines, start := (start.row, 0, Left), end := (end.row, w, Right), else identity. Rows, state, type and truncated are always unchanged.	src/text_selection/tests.rs::expansion_acts_only_on_an_in_progress_selection_of_its_type	&mut self two-state with an enum guard	0, 1, 2
truncated-per-span	invariant	text_selection::TextSelection (every &mut method)	Inv(sel) := state = None ⇒ ¬truncated. Inv(new()) holds and every &mut method preserves it. start_selection and clear set truncated := false; only Clamp sets truncated := true, and only on a path that returns true with state ≠ None. So a cleared selection never reports a partial loss, and a fresh span starts unreported.	src/text_selection/tests.rs::test_truncated_resets_on_new_selection_and_on_clear	type invariant over a struct, preserved by every &mut self method	0, 1, 2
selection-no-panic	no-panic	text_selection::TextSelection::*, rules::SmartSelection::{word_boundaries_at,word_boundaries_at_column_with}	No TextSelection method panics in any state of Reach for any arguments (every i32/u16/i64 step is saturating or checked). word_boundaries_at and word_boundaries_at_column_with never panic for any t: &str and any usize position (every slice goes through get; positions are floored to char boundaries).	(none; no test samples it)	panic-freedom over saturating/checked arithmetic, &str::get, iterator code	0, 1, 2, 3, 7, 12
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
Paths are relative to `crates/aterm-selection`.

```tsv
file	test	primary	also
src/rules_tests.rs	builtin_patterns_match_their_tokens	pat-ipv4	pat-hex-ids, pat-quoted, pat-semver, pat-email
src/rules_tests.rs	builtin_patterns_reject_lookalikes	pat-url	pat-email, pat-ipv4, pat-hex-ids, pat-file-path
src/rules_tests.rs	basic_word_boundaries_over_scripts	basic-word-is-containing-word	
src/rules_tests.rs	test_file_path_bare_relative_is_whole_not_fabricated_absolute	pat-file-path	
src/rules_tests.rs	uuid_outranks_git_hash_on_first_and_last_hex_groups	rule-precedence	pat-hex-ids, add-rule-sorted
src/rules_tests.rs	standalone_git_hash_still_matches_with_uuid_at_high	rule-precedence	pat-hex-ids, builtin-rule-table
src/rules_tests.rs	a_joiner_that_spells_a_word_belongs_to_it	basic-word-is-containing-word	
src/rules_tests.rs	an_invisible_character_between_two_words_still_splits_them	basic-word-is-containing-word	
src/rules_tests.rs	a_prepended_mark_belongs_to_the_word_after_it	word-at-column	basic-word-is-containing-word
src/rules_tests.rs	word_columns_follow_the_grids_fold_across_clusters	word-at-column	column-to-byte, byte-span-to-columns
src/rules_tests.rs	test_word_boundaries_at_column_cjk_with_space	word-at-column	column-to-byte, byte-span-to-columns
src/rules_tests.rs	test_word_boundaries_at_column_past_the_text_is_none	word-at-column	
src/rules_tests.rs	word_separators_make_a_word_any_run_of_non_separators	basic-word-is-containing-word	word-classifier
src/rules_tests.rs	word_separators_keep_combining_marks_and_zero_width_breakers	basic-word-is-containing-word	word-classifier
src/rules_tests.rs	word_separators_do_not_override_smart_rules	rule-precedence	word-classifier
src/tests.rs	builtin_rules_select_the_token_under_the_cursor	rule-precedence	builtin-rule-table, pat-url, pat-email, pat-file-path, pat-ipv4, pat-hex-ids, pat-quoted, pat-semver
src/tests.rs	test_no_match_at_position	rule-precedence	
src/tests.rs	test_word_boundaries_with_match	rule-precedence	
src/tests.rs	test_word_boundaries_fallback	rule-precedence	basic-word-is-containing-word
src/text_selection/tests.rs	test_contains_simple	contains-region	
src/text_selection/tests.rs	test_contains_multiline	contains-region	
src/text_selection/tests.rs	test_contains_block	contains-region	
src/text_selection/tests.rs	test_extend_selection	lifecycle-refines-model	extend-anchor-side
src/text_selection/tests.rs	test_extend_selection_preserves_anchor_cell_when_crossing_left	extend-anchor-side	
src/text_selection/tests.rs	test_extend_selection_preserves_anchor_cell_when_crossing_right	extend-anchor-side	
src/text_selection/tests.rs	test_anchor_ordering	anchor-total-order	
src/text_selection/tests.rs	test_translate_rows_for_presentation_preserves_selection_semantics	translate-rows	
src/text_selection/tests.rs	test_translate_rows_for_presentation_saturates_at_i32_boundaries	translate-rows	
src/text_selection/tests.rs	adjust_for_scroll_shifts_or_clears_at_the_bounds	adjust-for-scroll	clamp-head
src/text_selection/tests.rs	test_adjust_for_scroll_in_progress_selection	adjust-for-scroll	
src/text_selection/tests.rs	test_adjust_for_scroll_deep_scrollback_survives_then_evicts	clamp-head	adjust-for-scroll
src/text_selection/tests.rs	test_adjust_for_scroll_clamps_the_evicted_end_anchor_on_an_upward_drag	clamp-head	adjust-for-scroll
src/text_selection/tests.rs	test_adjust_for_scroll_clears_when_the_clamp_collapses_the_span	clamp-head	adjust-for-scroll, side-adjusted-bounds
src/text_selection/tests.rs	test_truncated_resets_on_new_selection_and_on_clear	truncated-per-span	lifecycle-refines-model, clamp-head
src/text_selection/tests.rs	test_adjust_for_row_splice_moves_history_endpoint_but_keeps_footer_endpoint	row-splice	clamp-head
src/text_selection/tests.rs	test_adjust_for_row_splice_handles_reverse_and_in_progress_anchors	row-splice	
src/text_selection/tests.rs	test_adjust_for_row_splice_clears_only_after_real_history_eviction	row-splice	clamp-head
src/text_selection/tests.rs	test_include_all_expands_sides	include-all	side-adjusted-bounds
src/text_selection/tests.rs	include_all_covers_both_endpoints_for_either_drag_direction	include-all	contains-region
src/text_selection/tests.rs	a_linear_drag_starting_on_a_wide_glyphs_continuation_still_paints_its_lead	contains-cell-glyph-whole	
src/text_selection/tests.rs	a_linear_drag_ending_on_a_wide_glyphs_lead_still_paints_its_continuation	contains-cell-glyph-whole	
src/text_selection/tests.rs	a_backwards_drag_snaps_the_same_wide_glyph_edges_as_a_forwards_one	normalize	contains-cell-glyph-whole
src/text_selection/tests.rs	a_lines_selection_paints_every_half_of_every_glyph_on_its_rows	contains-cell-glyph-whole	
src/text_selection/tests.rs	a_block_selects_a_wide_glyph_either_of_whose_cells_is_inside	contains-cell-glyph-whole	contains-region
src/text_selection/tests.rs	project_range_projects_every_shape	project-range	side-adjusted-bounds
src/text_selection/tests.rs	lines_bounds_projection_and_hits_ignore_anchor_columns_and_sides	project-range	side-adjusted-bounds, contains-region
src/text_selection/tests.rs	a_linear_end_before_the_first_representable_row_is_empty	side-adjusted-bounds	
src/text_selection/tests.rs	precomputed_bounds_equivalent_to_contains	side-adjusted-bounds	normalize, contains-region
src/text_selection/tests.rs	test_contains_cell_block_continuation_at_col_zero	contains-cell-glyph-whole	
src/text_selection/tests.rs	test_contains_end_left_side_col_zero_multiline	contains-region	
src/text_selection/tests.rs	test_contains_block_end_left_side_col_zero	contains-region	
src/text_selection/tests.rs	test_adjust_for_rows_shrink_relabels_every_row_by_the_same_amount	rows-shrink	adjust-for-scroll
src/text_selection/tests.rs	a_shrink_spares_a_span_that_crosses_the_cut	rows-shrink	adjust-for-scroll
src/text_selection/tests.rs	expansion_acts_only_on_an_in_progress_selection_of_its_type	expand-gated	
src/text_selection/tests.rs	truncate_to_floor_over_every_small_span	truncate-to-floor	clamp-head
src/text_selection/tests.rs	is_empty_iff_none_or_the_anchors_coincide	is-empty	
tests/conformance_text_selection.rs	real_calls_conform_to_the_lifecycle_model	lifecycle-refines-model	
tests/conformance_text_selection.rs	a_dropped_phase_check_is_rejected	lifecycle-refines-model	
```

## 3. Coverage

- 58 live tests mapped in §2 across the 34 specs (0 non-spec tests remain; 146 original tests read in the initial 2026-10-15 audit).
- One spec, `selection-no-panic`, has no test of its own. Every test above samples it in passing,
  and it is what makes the arithmetic in the other specs total.
- The biggest collapses are `basic-word-is-containing-word` (15 primary), `project-range` (12),
  `adjust-for-scroll` (12), `contains-region` (10), `rule-precedence` (10), `contains-cell-glyph-whole`
  (9), `clamp-head` (6), `lifecycle-refines-model` (6) and `word-at-column` (6).
- The seven `pat-*` specs retire 23 tests. All of them hinge on the aterm-regex crate's spec
  being citable across crates (gap 12).
- Gap histogram (specs blocked by each gap): 0 → 34, 1 → 32, 2 → 20, 3 → 15, 4 → 11, 5 → 10,
  7 → 15, 10 → 1, 11 → 2, 12 → 17, 13 → 5. Gaps 6, 8, 9 and 14–17 block no spec here directly.
  6, 14, 16 and 17 still apply to every proof, as the audit says.

## 4. Tests that are not spec samples

**Status (2026-10-15): all 8 tests below are deleted, along with the now-dead `cfg(test)` accessors `SelectionRuleKind::name` and `SelectionRule::pattern`.**

**Status (2026-10-17): their table and their §2 rows are gone.**

No test is a pure regression of a removed bug with no general property. Every regression-shaped
test (uuid vs git_hash precedence, the bare-relative file path, the zero-floor clear, the
cross-anchor extension, the wide-glyph halves) is an instance of a general spec above.

## 5. Implied specs that look wrong, or disagree with the code

1. **The precomputed-bounds advice was wrong in general; the comment now says so.** The doc
   comment on `src/text_selection/tests.rs::precomputed_bounds_equivalent_to_contains` said render
   paths should compute `normalized_bounds()` once instead of calling `contains()`. That holds only
   when `NS.side = Left` and `NE.side = Right` (or for Block), because `contains` applies the side
   adjustment and `normalized_bounds` does not. The test's three fixtures (forward, backward, block)
   are chosen so that the adjustment is the identity. With a Right-sided start or a Left-sided end,
   the advised optimisation is off by one column, or by a whole row at a Left end on col 0. The
   comment now says the fixtures make the two bounds equal and that a renderer caching arbitrary
   anchors must use `side_adjusted_bounds()`. The `side-adjusted-bounds` corollary states exactly
   when the two agree.
2. **The upper row bound was inclusive; it is exclusive now.** `adjust_for_scroll` and
   `adjust_for_row_splice` kept a row equal to `max_rows` (the check was `> max_rows`), yet
   `max_rows` is a row count, so row `max_rows` does not exist on screen. Both checks are now
   `>= max_rows`, as `adjust-for-scroll` and `row-splice` state: the kept window is `[−f, M)`.
3. **(Fixed 2026-10-28.) A comment did not match its case.**
   `test_adjust_for_rows_shrink_relabels_every_row_by_the_same_amount` put "A live span and an
   archived span move by the same 4 rows: one relabel." above the degenerate `kept_rows = 0` case.
   The misplaced comment is gone and the `kept_rows = 0` assertion explicitly states that the
   selection clears (`rows-shrink`).
4. **`enabled` was dead in production, and the extensibility doc was false; both are gone.**
   `SelectionRule.enabled` and the `is_enabled` filter in `rule_match_at` and `find_all` had no
   non-test setter, and the crate docs advertised "Extensible: Add custom rules" with no public
   API for it. The field, the filter, the claim and the tests `test_disable_rule` and
   `test_custom_rule` are deleted, so `Hit` and `rule-precedence` range over every rule.
5. **Most word-boundary assertions belong to aterm-grapheme.** `src/rules_tests.rs::basic_word_boundaries_*`
   and the joiner, invisible-character and prepend tests mostly sample
   `aterm_grapheme::words`' Unicode classification, not this crate's code. That classification's spec
   belongs in aterm-grapheme. Here these tests reduce to `basic-word-is-containing-word` plus a
   citation (gap 12). Move the Unicode table claims to aterm-grapheme's SPECS.md when that crate is
   migrated, so the property is proved once.
6. **(Fixed 2026-10-28.) `truncate_to_floor` had an unstated debug-only precondition.** It carried
   a `debug_assert!(min_row <= 0)` that no caller contract stated, while release builds clamped via
   `min_row.min(0)`. The assert is removed so debug and release share the total `min_row.min(0)`
   contract (`truncate-to-floor`, `selection-no-panic`).

## 6. Proof order (easiest high-value first)

No spec here is blocked only by gap 0. The cheapest lane is the text-selection specs, which need
only gaps 1–4 (in-crate structs and enums, `&self`/`&mut self` views, straight-line saturating
arithmetic, callee composition). The smart-selection specs also need loops (7), cross-crate citation
(12) and, for the patterns, a regex-crate spec. So prove them later.

1. **`translate-rows`** (gaps 0, 2, 3). These are two saturating `i32` field updates with a frame
   condition, the simplest `&mut self` two-state contract in the crate. It retires 2 tests and is
   the probe for gap 2's primed state.
2. **`is-empty`, `expand-gated`, `include-all`** (gaps 0, 1, 2). These are straight-line `&self`/`&mut self`
   contracts over the struct and one enum guard. They retire 5 tests and establish the
   anchor-struct carrier (gap 1) that every later text-selection spec reuses.
3. **`normalize` → `side-adjusted-bounds` → `contains-region` → `project-range` →
   `contains-cell-glyph-whole`** (gaps 0–4, 12 for the aterm-types callees). This is one chain:
   each spec is the lemma for the next. Together they retire 35 tests, the largest block in the
   crate, and they fix the §5.1 advice. Add `anchor-total-order` (gaps 0, 1, 3, 5, 11, 12) next to
   `normalize` if the derived `Ord` of `SelectionSide` can be cited.
4. **`clamp-head` → `adjust-for-scroll` → `rows-shrink`, `row-splice`, `truncate-to-floor`**
   (gaps 0–4, plus 13 for the closure in `row-splice`). These retire 24 tests.
5. **`truncated-per-span`, `extend-anchor-side`, `lifecycle-refines-model`** (adds gap 10 for the
   model refinement). These retire 9 tests, including the conformance test and its negative control.
6. **`word-classifier`, `column-to-byte`, `byte-span-to-columns` → `word-at-column`** (gaps 0, 1, 7,
   12; plus 2 and 3 for `word-classifier` and 4 for `word-at-column`). These retire 11 tests once the
   aterm-grapheme grid spec can be cited.
7. **`add-rule-sorted`, `builtin-rule-table`, `rule-precedence`, `basic-word-is-containing-word`** (adds
   gap 13 for the sort-key and classifier closures). These retire 28 tests.
8. **The seven `pat-*` specs** (gaps 0, 1, 5, 7, 12). Each is a regular-language statement over
   aterm-regex. Prove them after aterm-regex's `find_iter` spec exists. They retire 23 tests.
9. **`selection-no-panic`** comes free once steps 1–8 are proved, because each spec there is total.
   Until then, it is the cheap panic-freedom pass for the text-selection half.
