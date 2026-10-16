<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-vi: spec recovery from its 94 unit tests

> Status 2026-10-15: this file is the crate's migration worklist. Each of the 22 specs below is
> proved through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted. The 12 tests in §4 sample no spec: 8 can be deleted now, and 4 become specs once their
> code gets a production caller. No test has been deleted yet.

Source: `crates/aterm-vi` (read only). I read every `#[test]` (src/cursor.rs 13,
src/navigation.rs 15, src/search.rs 16, src/types_tests.rs 17, src/vi_mode_tests.rs 28,
src/word.rs 5 = 94) and the code each one reaches. Nothing was compiled or run. The claims below
come from reading the code.

Result: **22 specs**, covering **82 tests**. **12 tests are not spec samples** (§4).

The `gaps` column names the rows of the gap table in
the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md`
§7 that block each spec. Gap 0 (the installed seal publishes no Clean-discharged row) blocks every
row, so it is listed everywhere. No spec here is blocked by gap 0 alone: every one needs at least
gap 1 (non-scalar carriers) and gap 3 (E6 body widening), because each takes or returns a
`ViPoint` struct or an enum.

## 0. Notation used in the statements

- `g` ranges over `BufferAccess` values (a `&dyn` trait object). It is modelled as the uninterpreted
  functions `char_at`, `line_text`, `line_len`, `line_wide_continuations`, `total_lines`,
  `visible_rows`, `cols`, `display_offset` and `is_line_wrapped`.
- `WF(g)` (well-formed grid): `total_lines(g) >= 0` and `visible_rows(g) >= 1`.
- `COH(g)` (coherence):
  - For every line `l < 0`, `line_text(l) = Some(s)` implies `char_at(l,c) = s.chars().nth(c)` for
    every `c`, and `line_text(l) = None` implies `char_at(l,c) = None`.
  - `line_len(l)` = 1 + the largest `c` with `char_at(l,c)` not `' '`, or 0 if there is none.

  Every `BufferAccess` impl must satisfy `COH`, but the trait in `aterm-types` does not state it (§5.10).
- `top(g) = -total_lines(g)` (saturating), `bot(g) = visible_rows(g) - 1`, and
  `L(g) = cols(g) - 1` (saturating).
- `Region(g) = [top, bot] × [0, L]`. Cells are ordered row-major by the derived `Ord` on `ViPoint`
  (line first, then col; spec `vipoint-document-order`). `Start(g) = (top,0)` and `End(g) = (bot,L)`.
- `succ_g` and `pred_g` are the next and previous cells of `Region(g)` in that order.
  `succ_g(End)` and `pred_g(Start)` are `None`.
- `C(g,q) = char_at(q.line, q.col).unwrap_or(' ')`.
- `cont(g,q) = line_wide_continuations(q.line).unwrap_or_default().get(q.col).unwrap_or(false)`.
- `C'(g,q)` = `C(g,(q.line, q.col-1))` if `q.col > 0` and `cont(g,q)`, else `C(g,q)`. This is the
  char a word motion reads at `q`: the spacer cell of a wide char reads as that char.
- `Blank(ch)`: `ch` is one of `' '`, `'\t'`, `'\0'`. `Empty(g,l)`: `Blank(C(g,(l,c)))` for every `c < cols(g)`.
- `fnb(g,l)` = the least `c < line_len(l)` with `¬Blank(C(g,(l,c)))`, or 0 if there is none. Under
  `COH`, the scrollback branch of `first_non_blank_col` (`line < 0`) computes the same value.
- `Sem_sep(ch) = (ch == '\0' ∨ sep.contains(ch))` and `Ws(ch) = Blank(ch)`. `ViMode.separators` is
  always `DEFAULT_SEPARATORS`, because nothing sets it.
- A spec over `ViMode` or `ViSearchState` quantifies over every value of the private fields, not only
  the reachable ones.
- `cur` is `v.cursor.point` before the call, and a primed name is the value after it.
- A test may appear under more than one spec. It is deleted once every spec that lists it is
  Certified.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests, file-qualified; the
secondary ones are in §2), `difficulty` (the Rust features the proof must handle), `gaps` (the
blocking rows of audit §7).

```tsv
id	kind	functions	statement	subsumes	difficulty	gaps
vi-cursor-motion	contract	cursor::ViModeCursor::motion	requires top ≤ bot. ∀ cols:u16, point (l,c), motion m, boundary b, with L = cols.saturating_sub(1): the result is (clamp(l1,top,bot), min(c1,L)), where the pre-clamp point (l1,c1) is: Up → (l-1,c) if l>top else (l,c); Down → (l+1,c) if l<bot else (l,c); Left → (l,c-1) if c>0, else (l-1,L) if b=None ∧ l>top, else (l,c); Right → (l,c+1) if c<L, else (l+1,0) if b=None ∧ l<bot, else (l,c); First → (l,0); Last → (l,L); every other motion (H/M/L, FirstOccupied, word, bracket, paragraph, search, marks) → (l,c). Corollary: the result is in [top,bot]×[0,L], and the call never panics.	cursor.rs::{motion_up, motion_down, motion_left, motion_right, motion_first_last, motion_up_at_top_clamped, motion_down_at_bottom_clamped, motion_left_at_edge_grid_boundary, motion_left_at_edge_wraps_no_boundary, motion_right_at_edge_wraps_no_boundary, motion_with_scrollback}	EASY: by-value Copy structs (ViModeCursor, ViPoint), a match on a #[non_exhaustive] enum with char payloads, enum equality, i32::clamp (panics when min>max), u16::min, saturating ops	0,1,3
vi-cursor-scroll	contract	cursor::ViModeCursor::scroll	requires top ≤ bot. ∀ point (l,c), delta:i32: the result is (clamp(l.saturating_add(delta),top,bot), c), and the call never panics.	cursor.rs::{scroll_up_and_down, scroll_clamped_to_bounds}	TRIVIAL: by-value struct, saturating_add, i32::clamp	0,1,3
inline-search-kind-algebra	table-property	InlineSearchKind::{direction, reversed, is_till}	∀ k: direction(k) = Right ⇔ k ∈ {FindRight, TillRight}; is_till(k) ⇔ k ∈ {TillRight, TillLeft}; reversed(reversed(k)) = k; direction(reversed(k)) = opposite(direction(k)); is_till(reversed(k)) = is_till(k). These facts determine reversed completely.	types_tests.rs::{inline_search_kind_directions, inline_search_kind_reverse, inline_search_kind_is_till}	TRIVIAL: fieldless enum matches, matches!; 4 cases, decidable by evaluation	0,1,3,5
vimotion-to-buffercommand-table	table-property	<BufferCommand as From<ViMotion>>::from	from is the fixed table Up→PreviousLine, Down→NextLine, Left→BackwardChar, Right→ForwardChar, First→BeginningOfLine, Last→EndOfLine, FirstOccupied→FirstNonBlank, High→ScreenTop, Middle→ScreenMiddle, Low→ScreenBottom, SemanticLeft→BackwardWord, SemanticRight→ForwardWord, SemanticLeftEnd→BackwardWordEnd, SemanticRightEnd→ForwardWordEnd, WordLeft→BackwardWordBig, WordRight→ForwardWordBig, WordLeftEnd→BackwardWordEndBig, WordRightEnd→ForwardWordEndBig, Bracket→MatchBracket, ParagraphUp→ParagraphUp, ParagraphDown→ParagraphDown, SearchNext→SearchNext, SearchPrevious→SearchPrevious, and ∀ c:char, GotoMark(c)→GotoMark(c), GotoMarkLine(c)→GotoMarkLine(c). Hence from is injective: from(m1) = from(m2) ⇒ m1 = m2.	types_tests.rs::{vi_motion_to_buffer_command_up, vi_motion_to_buffer_command_down, vi_motion_to_buffer_command_navigation, vi_motion_to_buffer_command_screen_position, vi_motion_to_buffer_command_word_motions, vi_motion_to_buffer_command_structural, vi_motion_to_buffer_command_search, vi_motion_to_buffer_command_marks}	EASY: a match on an enum with char payloads into a cross-crate enum (aterm-types BufferCommand), a trait impl (From). Check first whether the impl is dead (§5.4)	0,1,3,5
vipoint-document-order	equivalence	<ViPoint as Ord>::cmp, <ViPoint as PartialOrd>::partial_cmp	∀ p, q: p < q ⇔ p.line < q.line ∨ (p.line = q.line ∧ p.col < q.col). This is lexicographic (line, col) order, with negative scrollback lines first. Region order and ViSearchState's partition_point depend on it.	search.rs::test_vi_point_ordering	TRIVIAL: derived Ord/PartialOrd on a two-field struct (the derive must be modelled)	0,1,3
point-step-succ-pred	contract	navigation::{point_forward, point_backward}	requires WF(g). ∀ p ∈ Region(g): point_forward(g,p) = succ_g(p) and point_backward(g,p) = pred_g(p). Both read only cols, visible_rows and total_lines, so the result does not depend on display_offset (#5612). In particular point_backward(g,Start) = None, and pred(succ(p)) = p whenever succ(p) is defined.	navigation.rs::{test_point_backward_scrollback_boundary_ignores_display_offset, test_point_backward_allows_navigation_to_scrollback}	EASY: &dyn BufferAccess calls (uninterpreted), Option<ViPoint>, saturating ops, no loops	0,1,3,4
line-cache-refines-cell-char	refinement	navigation::LineCache::{char_at, word_char_at, is_wide_continuation}, navigation::is_line_empty, first_non_blank_col	requires COH(g). ∀ cache state reachable from LineCache::new() by any sequence of reads, ∀ q: char_at(g,q) = C(g,q) and word_char_at(g,q) = C'(g,q); the cache state never changes the value a read returns. Separately, ∀ l: is_line_empty(g,l) = Empty(g,l) (for l < 0 this needs line_text(l) to have no non-blank char past cols; line_text(l) = None reads as empty), and first_non_blank_col(g,l) = fnb(g,l).	navigation.rs::paragraph_up_finds_empty_line_in_scrollback, vi_mode_tests.rs::first_occupied_in_scrollback_finds_non_blank	MEDIUM: &mut self cache, Option<(i32, Vec<char>)>, Vec<bool>, String from line_text with chars().collect()/all(), loops, trait-object calls. COH is a requires on the trait, not a fact about the code	0,1,2,3,4,5,7,12
bracket-match-depth	contract	navigation::bracket_match	requires WF(g) ∧ COH(g). ∀ p ∈ Region(g), with ch = C(g,p): if ch ∉ {( ) [ ] { } < >}, the result is None. If ch is the opener o of pair (o,c), the result is Some(q) for the least q > p in Region order with #{r ∈ [p,q] : C(g,r)=o} = #{r ∈ [p,q] : C(g,r)=c}, or None if there is no such q. If ch is the closer c of pair (o,c), the result is Some(q) for the greatest q < p with #{r ∈ [q,p] : C(g,r)=c} = #{r ∈ [q,p] : C(g,r)=o}, or None if there is none. Only ch's bracket kind is counted. The scan spans scrollback and visible rows and ignores display_offset.	navigation.rs::{bracket_match_cases, test_bracket_match_respects_scrollback_boundary, bracket_match_scans_into_scrollback_content}	MEDIUM-HARD: an unbounded loop with a u32 depth counter (decreases: Region cells left), iter().find_map with a closure over a const slice of tuples, Option ?, trait-object calls, LineCache	0,1,3,4,5,7,13
paragraph-motion	contract	navigation::{paragraph_up, paragraph_down}	requires WF(g). ∀ p: paragraph_up(g,p) = (top,0) if p.line ≤ top; otherwise (l,0) for the greatest l ∈ (top, p.line-1] with Empty(g,l), or (top,0) if there is none. paragraph_down(g,p) = (bot,0) if p.line ≥ bot; otherwise (l,0) for the least l ∈ [p.line+1, bot) with Empty(g,l), or (bot,0) if there is none. top does not depend on display_offset (#5612). The result has col 0 and line in [top,bot].	navigation.rs::{paragraph_up_finds_empty_line, paragraph_down_finds_empty_line, paragraph_up_clamps_to_top, paragraph_down_clamps_to_bottom, test_paragraph_up_scrollback_boundary_ignores_display_offset}	MEDIUM: while loops over i32 lines, a nested for loop over cols, String chars().all() on the scrollback branch (via line-cache-refines-cell-char), trait-object calls	0,1,3,4,5,7
inline-search-scan	contract	navigation::{inline_search_right, inline_search_left}	∀ g, p, needle n: inline_search_right(g,p,n) = Some((p.line,c)) for the least c with p.col < c < cols(g) ∧ C(g,(p.line,c)) = n, or None if there is none. inline_search_left(g,p,n) = Some((p.line,c)) for the greatest c < p.col with C(g,(p.line,c)) = n, or None if there is none. The line never changes, and neither scan reads the cell at p.	navigation.rs::{inline_search_right_finds_char, inline_search_left_finds_char, inline_search_right_not_found, inline_search_left_not_found}	EASY-MEDIUM: one while loop over u16 columns (decreases cols-c or c), char equality, trait-object calls	0,1,3,4,5,7
word-motion-semantics	contract	word::{semantic_word_right, semantic_word_left, semantic_word_right_end, semantic_word_left_end, whitespace_word_right, whitespace_word_left, whitespace_word_right_end, whitespace_word_left_end}	requires WF(g) ∧ COH(g), p ∈ Region(g). Let S(q) = Sem_sep(C'(g,q)) for the semantic motions (any separators) and Ws(C'(g,q)) for the whitespace ones; order is Region order. right (w/W): q1 = the least q ≥ p with S(q); the result is the least q ≥ q1 with ¬S(q); a scan that runs off the end yields End. right_end (e/E): p = End ⇒ p; else q1 = the least q ≥ succ(p) with ¬S(q) (End if none), and the result is the greatest q ≥ q1 with ¬S(r) ∀ r ∈ [q1,q], so q = End ∨ S(succ(q)). left (b/B): p = Start ⇒ p; else q1 = the greatest q ≤ pred(p) with ¬S(q) (none ⇒ Start), and the result is the least q ≤ q1 with ¬S(r) ∀ r ∈ [q,q1], so q = Start ∨ S(pred(q)). left_end (ge/gE): p = Start ⇒ p; else q1 = the greatest q ≤ pred(p) with S(q) (Start if none), and the result is the greatest q ≤ q1 with ¬S(q), or Start if there is none. Every result is in Region(g). Because S reads C', a wide char's spacer never separates (a CJK run is one word), and a real ' ' still does.	word.rs::{word_motion_landings, word_right_stops_at_end, word_left_traverses_scrollback_content, cjk_runs_are_crossed_as_one_word, a_real_space_between_cjk_runs_still_separates}	HARD: several sequential while loops and a loop with early returns (decreases: Region cells left), &str::contains(char), &mut LineCache, Option matches, trait-object calls; quantified over Region intervals. Uses point-step-succ-pred and line-cache-refines-cell-char	0,1,2,3,4,5,7
search-focus-circular	contract	search::ViSearchState::{focus_next, focus_prev}	∀ state whose matches M (length n) are sorted ascending, ∀ point a: n = 0 ⇒ both return None and the state is unchanged. n > 0: let i = #{m ∈ M : m ≤ a} and j = #{m ∈ M : m < a}. focus_next(a) sets focused := Some(i if i < n else 0) and returns Some(M[focused]): the first match strictly after a, wrapping to the first. focus_prev(a) sets focused := Some(j-1 if j > 0 else n-1) and returns Some(M[focused]): the last match strictly before a, wrapping to the last. matches is never modified, and neither call panics, sorted or not.	search.rs::{test_focus_next_basic, test_focus_next_between_matches, test_focus_next_wraps_around, test_focus_next_on_match_advances, test_focus_prev_basic, test_focus_prev_between_matches, test_focus_prev_wraps_around, test_focus_prev_on_match_retreats, test_focus_empty_returns_none, test_single_match_wraps_to_self, test_scrollback_matches}	MEDIUM: &mut self, Vec<ViPoint> content (a sortedness precondition), slice::partition_point with a closure, Vec::get, Option<usize>. Production never fills matches (§5.2)	0,1,2,3,5,13
vimode-toggle	contract	ViMode::toggle	∀ v, t: after v.toggle(t), active' = ¬active. active' ⇒ cursor' = t. ¬active' ⇒ cursor' = cursor ∧ visual_anchor' = visual_type' = None. marks, inline_search, search and separators are unchanged.	vi_mode_tests.rs::toggle_activates_at_cursor	EASY: &mut self on a struct holding a HashMap, a String and a Vec; a call to cancel_visual	0,1,2,3,4
vimode-inactive-is-inert	invariant	ViMode::{motion_with_grid, scroll, toggle_visual, inline_search_execute, inline_search_repeat, inline_search_repeat_reverse}	∀ v with active = false, ∀ g and other arguments: motion_with_grid, scroll and toggle_visual leave v unchanged; the three inline_search_* methods return false and leave v unchanged (inline_search_execute does not record the search).	vi_mode_tests.rs::{motion_ignored_when_inactive, search_no_op_when_inactive}	EASY: a two-state frame condition over the whole struct (v' = v) behind an early return	0,1,2,3
vimode-motion-dispatch	refinement	ViMode::motion_with_grid	requires WF(g), v.active. Up/Down/Left/Right/First/Last with boundary b: cursor' = cur.motion(cols, top, bot, m, b) (vi-cursor-motion). Semantic*/Word*: cursor' = the matching word:: function of cur with separators = v.separators (word-motion-semantics). Bracket: cursor' = bracket_match(g,cur).unwrap_or(cur). ParagraphUp/Down: cursor' = paragraph_up/down(g,cur). For every motion only the cursor changes (and search.focused for SearchNext/SearchPrevious); marks, inline_search, visual state and separators are unchanged.	vi_mode_tests.rs::{basic_motion_down, word_motion_dispatches_through_vi_mode, bracket_motion_dispatches_through_vi_mode, paragraph_motion_dispatches_through_vi_mode}	MEDIUM: &mut self, a match on a non-exhaustive enum, composition with the callees' ensures, trait-object calls	0,1,2,3,4
vimode-first-occupied	contract	ViMode::motion_with_grid (FirstOccupied arm), first_non_blank_col	requires WF(g) ∧ COH(g), v.active, cur = (l,c). If c = fnb(g,l) ∧ is_line_wrapped(l) ∧ l > top: let s = the greatest s ≤ l-1 with s = top ∨ ¬is_line_wrapped(s) (the start of the logical line); cursor' = (s, fnb(g,s)). Otherwise cursor' = (l, fnb(g,l)).	vi_mode_tests.rs::{first_occupied_finds_non_space, first_occupied_all_spaces_goes_to_zero, first_occupied_double_tap_walks_back_through_wrapped_lines, first_occupied_in_scrollback_finds_non_blank, first_occupied_no_double_tap_on_unwrapped_line}	MEDIUM: a while loop over wrapped lines, a for loop / chars().enumerate() in first_non_blank_col, u16::try_from, trait-object calls, &mut self	0,1,2,3,4,5,7
vimode-high-middle-low	contract	ViMode::motion_with_grid (High/Middle/Low arms)	requires WF(g), v.active. Let R = visible_rows(g), d = display_offset(g). High/Middle/Low set cursor'.line = clamp(-d + k, top, bot) with k = 0, R/2, R-1 respectively, and cursor'.col = min(cur.col, L). For every d ∈ [0, total_lines] this is the k-th row the viewport draws.	vi_mode_tests.rs::high_middle_low_name_the_viewport_rows_at_every_display_offset	EASY: saturating i32 ops, i32 division, clamp, &mut self, trait-object calls	0,1,2,3,4
vimode-goto-mark-clamped	contract	ViMode::motion_with_grid (GotoMark/GotoMarkLine arms), ViMarks::get	requires WF(g), v.active. ∀ ch: v.marks.get(ch) = Some(t) ⇒ GotoMark(ch) sets cursor' = (clamp(t.line,top,bot), min(t.col,L)) and GotoMarkLine(ch) sets cursor' = (t', fnb(g,t')) with t' = clamp(t.line,top,bot). v.marks.get(ch) = None ⇒ the cursor is unchanged.	vi_mode_tests.rs::mark_set_and_goto	EASY-MEDIUM: a HashMap<char,ViPoint> content model, &mut self, clamp, trait-object calls. Production never sets a mark (§5.1)	0,1,2,3,4
vimode-search-motion-clamped	contract	ViMode::motion_with_grid (SearchNext/SearchPrevious arms)	requires WF(g), v.active, search.matches sorted. SearchNext: r = search.focus_next(cur) (focused updated as in search-focus-circular); r = Some(t) ⇒ cursor' = (clamp(t.line,top,bot), min(t.col,L)); r = None ⇒ the cursor is unchanged. SearchPrevious: the same with focus_prev. The cursor ends in Region(g) even when a stale match lies outside the grid (#7622).	vi_mode_tests.rs::{search_next_navigates_to_match, search_next_wraps_to_first, search_prev_navigates_backward, search_prev_wraps_to_last, search_no_op_with_empty_matches, search_clamps_cursor_to_grid_bounds, search_clamps_respects_scrollback_bounds}	MEDIUM: composition with the focus_next/focus_prev ensures, &mut self over a nested struct, a Vec content model, clamp	0,1,2,3,4,5
vimode-inline-search	contract	ViMode::{inline_search_execute, inline_search_repeat, inline_search_repeat_reverse, perform_inline_search}	requires v.active. Define P(n,k) from cur = (l,c): base = inline_search_right(g,cur,n) if direction(k) = Right, else inline_search_left(g,cur,n). base = Some(q) ⇒ target = q for FindRight/FindLeft; (l, q.col-1 if q.col > 0 else q.col) for TillRight; (l, min(q.col+1, cols-1)) for TillLeft; P sets cursor := target and returns true. base = None ⇒ P returns false and the cursor is unchanged. inline_search_execute(g,n,k) sets inline_search := Some({n,k}) and then runs P(n,k). inline_search_repeat runs P(n,k) if inline_search = Some({n,k}), else returns false and changes nothing. inline_search_repeat_reverse is the same with P(n, reversed(k)). The repeat methods never modify inline_search.	vi_mode_tests.rs::{inline_search_execute_moves_cursor, inline_search_till_stops_before_char, inline_search_repeat_and_reverse}	MEDIUM: &mut self, Option<struct>, composition with inline-search-scan and inline-search-kind-algebra, trait-object calls	0,1,2,3,4
vimode-scroll	contract	ViMode::scroll	requires top ≤ bot, v.active. ∀ last_col, delta: cursor' = (clamp(cur.line.saturating_add(delta), top, bot), min(cur.col, last_col)). Nothing else changes.	vi_mode_tests.rs::{scroll_moves_cursor, scroll_clamps_the_column_to_the_last_one}	EASY: &mut self, composition with vi-cursor-scroll	0,1,2,3,4
vimode-no-panic-and-in-region	no-panic/bounds	ViMode::motion_with_grid, ViMode::scroll, ViModeCursor::{motion, scroll}	∀ v, ∀ g with WF(g), ∀ motion and boundary: motion_with_grid does not panic, and cur ∈ Region(g) ⇒ cursor' ∈ Region(g). ViModeCursor::motion, ViModeCursor::scroll and ViMode::scroll need requires top ≤ bot. Without WF(g) the call can panic: visible_rows = 0 ∧ total_lines = 0 gives top = 0 > bot = -1, and i32::clamp panics (§5.5).	(none yet; it states the precondition the other specs rely on)	MEDIUM: proving clamp's min ≤ max assert unreachable across the whole dispatch match, termination of the loops in the called motions	0,1,2,3,4,5,7
```

### Distribution by kind (22)

| kind | count | ids |
|---|---|---|
| contract | 15 | vi-cursor-motion, vi-cursor-scroll, point-step-succ-pred, bracket-match-depth, paragraph-motion, inline-search-scan, word-motion-semantics, search-focus-circular, vimode-toggle, vimode-first-occupied, vimode-high-middle-low, vimode-goto-mark-clamped, vimode-search-motion-clamped, vimode-inline-search, vimode-scroll |
| table-property | 2 | inline-search-kind-algebra, vimotion-to-buffercommand-table |
| refinement | 2 | line-cache-refines-cell-char, vimode-motion-dispatch |
| equivalence | 1 | vipoint-document-order |
| invariant | 1 | vimode-inactive-is-inert |
| no-panic/bounds | 1 | vimode-no-panic-and-in-region |

### Gap histogram (specs blocked by each audit §7 gap)

| gap | specs |
|---|---|
| 0 seal publishes no Clean row | 22 |
| 1 non-scalar carriers | 22 |
| 2 `&self`/`&mut self` views, two-state | 13 |
| 3 E6 body widening | 22 |
| 4 callee-ensures composition | 15 |
| 5 quantified clauses | 11 |
| 7 loops | 7 |
| 12 cross-crate citation | 1 |
| 13 closures | 2 |

### Proof dependency order

`vipoint-document-order`, `inline-search-kind-algebra`, `vimotion-to-buffercommand-table` →
`vi-cursor-motion`, `vi-cursor-scroll` → `point-step-succ-pred`, `line-cache-refines-cell-char` →
`inline-search-scan`, `paragraph-motion`, `bracket-match-depth`, `word-motion-semantics`,
`search-focus-circular` → the `ViMode` specs (`vimode-toggle`, `vimode-inactive-is-inert`,
`vimode-scroll`, `vimode-high-middle-low`, `vimode-goto-mark-clamped`, `vimode-first-occupied`,
`vimode-inline-search`, `vimode-search-motion-clamped`) → `vimode-motion-dispatch` →
`vimode-no-panic-and-in-region`.

## 2. Test → spec map (TSV; all 94)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/cursor.rs	motion_up	vi-cursor-motion	
src/cursor.rs	motion_down	vi-cursor-motion	
src/cursor.rs	motion_left	vi-cursor-motion	
src/cursor.rs	motion_right	vi-cursor-motion	
src/cursor.rs	motion_first_last	vi-cursor-motion	
src/cursor.rs	motion_up_at_top_clamped	vi-cursor-motion	
src/cursor.rs	motion_down_at_bottom_clamped	vi-cursor-motion	
src/cursor.rs	motion_left_at_edge_grid_boundary	vi-cursor-motion	
src/cursor.rs	motion_left_at_edge_wraps_no_boundary	vi-cursor-motion	
src/cursor.rs	motion_right_at_edge_wraps_no_boundary	vi-cursor-motion	
src/cursor.rs	motion_with_scrollback	vi-cursor-motion	
src/cursor.rs	scroll_up_and_down	vi-cursor-scroll	
src/cursor.rs	scroll_clamped_to_bounds	vi-cursor-scroll	
src/navigation.rs	bracket_match_cases	bracket-match-depth	
src/navigation.rs	paragraph_up_finds_empty_line	paragraph-motion	
src/navigation.rs	paragraph_down_finds_empty_line	paragraph-motion	
src/navigation.rs	paragraph_up_clamps_to_top	paragraph-motion	
src/navigation.rs	paragraph_down_clamps_to_bottom	paragraph-motion	
src/navigation.rs	inline_search_right_finds_char	inline-search-scan	
src/navigation.rs	inline_search_left_finds_char	inline-search-scan	
src/navigation.rs	inline_search_right_not_found	inline-search-scan	
src/navigation.rs	inline_search_left_not_found	inline-search-scan	
src/navigation.rs	test_point_backward_scrollback_boundary_ignores_display_offset	point-step-succ-pred	
src/navigation.rs	test_paragraph_up_scrollback_boundary_ignores_display_offset	paragraph-motion	
src/navigation.rs	test_point_backward_allows_navigation_to_scrollback	point-step-succ-pred	
src/navigation.rs	test_bracket_match_respects_scrollback_boundary	bracket-match-depth	
src/navigation.rs	paragraph_up_finds_empty_line_in_scrollback	paragraph-motion	line-cache-refines-cell-char
src/navigation.rs	bracket_match_scans_into_scrollback_content	bracket-match-depth	line-cache-refines-cell-char
src/search.rs	test_focus_next_basic	search-focus-circular	
src/search.rs	test_focus_next_between_matches	search-focus-circular	
src/search.rs	test_focus_next_wraps_around	search-focus-circular	
src/search.rs	test_focus_next_on_match_advances	search-focus-circular	
src/search.rs	test_focus_prev_basic	search-focus-circular	
src/search.rs	test_focus_prev_between_matches	search-focus-circular	
src/search.rs	test_focus_prev_wraps_around	search-focus-circular	
src/search.rs	test_focus_prev_on_match_retreats	search-focus-circular	
src/search.rs	test_focus_empty_returns_none	search-focus-circular	
src/search.rs	test_is_match	NON-SPEC (test-only code)	
src/search.rs	test_is_focused	NON-SPEC (test-only code)	search-focus-circular
src/search.rs	test_clear_resets_state	NON-SPEC (test-only code)	
src/search.rs	test_set_matches_resets_focus	NON-SPEC (becomes a spec after fix)	
src/search.rs	test_single_match_wraps_to_self	search-focus-circular	
src/search.rs	test_scrollback_matches	search-focus-circular	vipoint-document-order
src/search.rs	test_vi_point_ordering	vipoint-document-order	
src/types_tests.rs	vi_point_default_is_origin	NON-SPEC (tautology)	
src/types_tests.rs	vi_motion_directions	NON-SPEC (test-only code)	
src/types_tests.rs	inline_search_kind_directions	inline-search-kind-algebra	
src/types_tests.rs	inline_search_kind_reverse	inline-search-kind-algebra	
src/types_tests.rs	inline_search_kind_is_till	inline-search-kind-algebra	
src/types_tests.rs	vi_marks_set_get_remove	NON-SPEC (becomes a spec after fix)	
src/types_tests.rs	vi_marks_rejects_invalid_chars	NON-SPEC (becomes a spec after fix)	
src/types_tests.rs	vi_marks_accepts_special_chars	NON-SPEC (becomes a spec after fix)	
src/types_tests.rs	vi_motion_to_buffer_command_up	vimotion-to-buffercommand-table	
src/types_tests.rs	vi_motion_to_buffer_command_down	vimotion-to-buffercommand-table	
src/types_tests.rs	vi_motion_to_buffer_command_navigation	vimotion-to-buffercommand-table	
src/types_tests.rs	vi_motion_to_buffer_command_screen_position	vimotion-to-buffercommand-table	
src/types_tests.rs	vi_motion_to_buffer_command_word_motions	vimotion-to-buffercommand-table	
src/types_tests.rs	vi_motion_to_buffer_command_structural	vimotion-to-buffercommand-table	
src/types_tests.rs	vi_motion_to_buffer_command_search	vimotion-to-buffercommand-table	
src/types_tests.rs	vi_motion_to_buffer_command_marks	vimotion-to-buffercommand-table	
src/types_tests.rs	vi_marks_clear	NON-SPEC (test-only code)	
src/vi_mode_tests.rs	toggle_activates_at_cursor	vimode-toggle	
src/vi_mode_tests.rs	motion_ignored_when_inactive	vimode-inactive-is-inert	(drives the #[cfg(test)] ViMode::motion, §5.3)
src/vi_mode_tests.rs	basic_motion_down	vimode-motion-dispatch	vi-cursor-motion (drives ViMode::motion, §5.3)
src/vi_mode_tests.rs	mark_set_and_goto	vimode-goto-mark-clamped	(drives ViMode::motion and set_mark, §5.1, §5.3)
src/vi_mode_tests.rs	first_occupied_finds_non_space	vimode-first-occupied	
src/vi_mode_tests.rs	first_occupied_all_spaces_goes_to_zero	vimode-first-occupied	
src/vi_mode_tests.rs	first_occupied_double_tap_walks_back_through_wrapped_lines	vimode-first-occupied	
src/vi_mode_tests.rs	first_occupied_in_scrollback_finds_non_blank	vimode-first-occupied	line-cache-refines-cell-char
src/vi_mode_tests.rs	first_occupied_no_double_tap_on_unwrapped_line	vimode-first-occupied	
src/vi_mode_tests.rs	scroll_moves_cursor	vimode-scroll	vi-cursor-scroll
src/vi_mode_tests.rs	scroll_clamps_the_column_to_the_last_one	vimode-scroll	
src/vi_mode_tests.rs	inline_search_stored	NON-SPEC (test-only code)	vimode-inline-search
src/vi_mode_tests.rs	word_motion_dispatches_through_vi_mode	vimode-motion-dispatch	word-motion-semantics
src/vi_mode_tests.rs	bracket_motion_dispatches_through_vi_mode	vimode-motion-dispatch	bracket-match-depth
src/vi_mode_tests.rs	paragraph_motion_dispatches_through_vi_mode	vimode-motion-dispatch	paragraph-motion
src/vi_mode_tests.rs	inline_search_execute_moves_cursor	vimode-inline-search	inline-search-scan
src/vi_mode_tests.rs	inline_search_till_stops_before_char	vimode-inline-search	inline-search-kind-algebra
src/vi_mode_tests.rs	inline_search_repeat_and_reverse	vimode-inline-search	inline-search-kind-algebra
src/vi_mode_tests.rs	search_next_navigates_to_match	vimode-search-motion-clamped	search-focus-circular
src/vi_mode_tests.rs	search_next_wraps_to_first	vimode-search-motion-clamped	search-focus-circular
src/vi_mode_tests.rs	search_prev_navigates_backward	vimode-search-motion-clamped	search-focus-circular
src/vi_mode_tests.rs	search_prev_wraps_to_last	vimode-search-motion-clamped	search-focus-circular
src/vi_mode_tests.rs	search_no_op_with_empty_matches	vimode-search-motion-clamped	search-focus-circular
src/vi_mode_tests.rs	search_no_op_when_inactive	vimode-inactive-is-inert	
src/vi_mode_tests.rs	search_accessor_exposes_state	NON-SPEC (test-only code)	
src/vi_mode_tests.rs	search_clamps_cursor_to_grid_bounds	vimode-search-motion-clamped	vimode-no-panic-and-in-region
src/vi_mode_tests.rs	search_clamps_respects_scrollback_bounds	vimode-search-motion-clamped	vimode-no-panic-and-in-region
src/vi_mode_tests.rs	high_middle_low_name_the_viewport_rows_at_every_display_offset	vimode-high-middle-low	
src/word.rs	word_motion_landings	word-motion-semantics	
src/word.rs	word_right_stops_at_end	word-motion-semantics	(asserts only the line, §5.7)
src/word.rs	word_left_traverses_scrollback_content	word-motion-semantics	line-cache-refines-cell-char
src/word.rs	cjk_runs_are_crossed_as_one_word	word-motion-semantics	line-cache-refines-cell-char
src/word.rs	a_real_space_between_cjk_runs_still_separates	word-motion-semantics	
```

## 3. Coverage

- 94 tests read. 82 map to one of the 22 specs. 12 are not spec samples (§4).
- The biggest collapses: `vi-cursor-motion` (11), `search-focus-circular` (11),
  `vimotion-to-buffercommand-table` (8), `vimode-search-motion-clamped` (7), `paragraph-motion` (5),
  `word-motion-semantics` (5), `vimode-first-occupied` (5).
- `vimode-no-panic-and-in-region` has no test of its own. It names the `WF(g)` / `top ≤ bot`
  precondition that every clamp-based spec needs, and the panic no test covers (§5.5).
- 18 of the 82 tests (the 11 focus tests and the 7 `search_*` cursor tests) and the one mark test
  specify behaviour that the shipped binary cannot reach today (§5.1, §5.2). Their specs are still
  true of the code; whether to prove them or delete the dead feature is an owner decision.

## 4. Tests that are not spec samples

**Status 2026-10-15:** the 8 **delete** rows below have been deleted from the crate; the 4 "becomes a spec after fix" tests remain.

| test | why | action |
|---|---|---|
| types_tests::vi_point_default_is_origin | A tautology: `#[derive(Default)]` on a struct of `i32`/`u16` already gives `(0,0)`. | **delete** |
| types_tests::vi_motion_directions | `ViMotion::direction` is `#[cfg(test)]` with no production caller. The test pins an arbitrary table (FirstOccupied, Bracket and GotoMark map to Right). | **delete** (with `ViMotion::direction`) |
| types_tests::vi_marks_clear | `ViMarks::clear` and `contains` are `#[cfg(test)]` with no production caller. | **delete** |
| search::test_is_match | `ViSearchState::is_match` is `#[cfg(test)]` only. It reduces to the std `binary_search` contract. | **delete** |
| search::test_is_focused | `is_focused` and `focused_point` are `#[cfg(test)]` only. Its `focus_next` step is covered by `search-focus-circular`. | **delete** |
| search::test_clear_resets_state | `clear`, `is_empty` and `match_count` are `#[cfg(test)]` only. | **delete** |
| vi_mode_tests::inline_search_stored | Round-trips the `#[cfg(test)]` pair `set_inline_search`/`inline_search`. The production property (execute records the search) is part of `vimode-inline-search`. | **delete** |
| vi_mode_tests::search_accessor_exposes_state | Tests only the `#[cfg(test)]` accessors `search`, `search_mut`, `set_matches`, `is_empty` and `match_count`. | **delete** |
| types_tests::vi_marks_set_get_remove | `ViMarks::{new,set,remove,contains}` are `#[cfg(test)]`. Production can only call `get`, on a map nothing fills. | **becomes a spec after fix**: once a production mark setter exists (vi `m`), set-then-get is a real spec |
| types_tests::vi_marks_rejects_invalid_chars | The mark-name rule (a–z, `'`, `` ` ``) lives in the `#[cfg(test)]`-only `ViMarks::set`. | **becomes a spec after fix** (same condition) |
| types_tests::vi_marks_accepts_special_chars | Same rule, same `#[cfg(test)]`-only `ViMarks::set`. | **becomes a spec after fix** (same condition) |
| search::test_set_matches_resets_focus | `set_matches` is `#[cfg(test)]`, so production can never fill `matches`. | **becomes a spec after fix**: once a production setter exists, "set_matches resets focus to None" is a real spec |

Counts: **8 delete**, **4 become a spec after fix**.

## 5. Implied specs that look wrong, or disagree with the code

1. **Marks are dead in production.** `ViMarks::set`, `ViMode::set_mark` (`src/lib.rs:427`) and every
   other mark mutator are `#[cfg(test)]`, and no crate outside `aterm-vi` sets a mark. So
   `ViMode.marks` is always empty in the shipped binary, and `GotoMark`/`GotoMarkLine` are always
   no-ops. `vi_mode_tests::mark_set_and_goto` and the four `vi_marks_*` tests specify a feature that
   cannot be reached. Either wire a production `m` command or delete the mark machinery.
2. **n/N is dead in production.** `ViSearchState::set_matches` (`src/search.rs:41`) is
   `#[cfg(test)]`, although its doc says "The caller populates matches via set_matches". No
   production code fills `ViMode.search`, so `SearchNext`/`SearchPrevious` always find no match. The
   11 focus tests in `search.rs` and the 7 `search_*` tests in `vi_mode_tests.rs` pin behaviour the
   binary never shows.
3. **Three tests drive a test-only twin.** `motion_ignored_when_inactive`, `basic_motion_down` and
   `mark_set_and_goto` go through `ViMode::motion`, a `#[cfg(test)]` twin of `motion_with_grid` that
   takes caller-supplied bounds instead of deriving them from the grid. They do not exercise the
   production dispatch path. Delete the wrapper with them once the `motion_with_grid` specs are
   Certified.
4. **`From<ViMotion> for BufferCommand` may be dead.** No explicit caller of the impl
   (`src/types.rs:155`) was found outside `types_tests.rs`; an implicit `.into()` was not ruled
   out. The 8 `vi_motion_to_buffer_command_*` tests pin it. Confirm whether it is used before
   proving `vimotion-to-buffercommand-table`; if it is dead, delete the impl and the tests instead.
5. **Latent panic.** `i32::clamp(topmost, bottommost)` in `ViModeCursor::motion`
   (`src/cursor.rs:110`), `ViModeCursor::scroll` (`src/cursor.rs:128`) and the GotoMark, search and
   H/M/L arms of `motion_with_grid` (`src/lib.rs:226`, `:232`, `:308`, `:315`, `:334`) panics when
   `topmost > bottommost`. That happens for a `BufferAccess` with `visible_rows() = 0` and
   `total_lines() = 0`, or when `ViMode::scroll` gets reversed bounds. No test covers it. The specs
   carry `WF(g)` / `top ≤ bot` as a `requires`; the alternative is a code fix that makes the calls total.
6. **Dead code in the left word motions.** The trailing `point_forward(grid, cur).unwrap_or(cur)` in
   `word::semantic_word_left` and `word::whitespace_word_left` can never run: the second loop is
   entered with `¬S(cur)` and leaves only by `return`. Delete it, or the proof has an unreachable
   branch to discharge.
7. **A test asserts less than its name.** `word::word_right_stops_at_end` asserts only
   `result.line == 0`. The actual result is `(0,9)`, the End cell. The spec states the full result.
8. **Repeating a till search stalls.** After `t` lands next to the needle, `;` searches from that
   column, finds the same needle at `col+1` and steps back to the same column. It returns `true` but
   never moves. No test covers repeating a till search, and vim's `cpo-;` behaviour differs. The spec
   `vimode-inline-search` states what the code does; decide whether that is the intended behaviour
   before proving it.
9. **The kind tests sample less than the algebra.** `types_tests::inline_search_kind_reverse` checks 2
   of the 4 kinds, and `inline_search_kind_is_till` omits FindLeft. The algebra spec (involution,
   direction flip, `is_till` preserved) is stronger than the samples.
10. **An unstated trait contract.** The `LineCache` and the scrollback fast paths (`is_line_empty` and
    `first_non_blank_col` for `line < 0`) are correct only under `COH`: `line_text` must agree with
    `char_at` cell for cell, and `line_len` must match the last non-blank cell. `is_line_empty` also
    scans every char of `line_text` but only `cols` cells on visible rows. `COH` belongs on the
    `aterm-types` `BufferAccess` trait (which is why `line-cache-refines-cell-char` lists gap 12)
    before the refinement can be stated about real impls.

## 6. The specs I would prove first (easiest, high value)

No spec is blocked by gap 0 alone, so every one needs Trust work first. The first three need only
gaps 0, 1 and 3, the smallest set in this crate: by-value structs and fieldless enums as carriers,
with `clamp`/`min`/saturating ops and a `match` admitted in E6 bodies.

1. **`vi-cursor-motion`** with **`vi-cursor-scroll`**. These are straight-line code over two `Copy`
   structs and an enum `match`, with no loops, no `&mut self` and no trait calls. Together they
   retire all 13 `cursor.rs` tests. They also force the `top ≤ bot` precondition (§5.5) into a
   `requires`, and `vimode-scroll`, `vimode-motion-dispatch` and the no-panic spec are built on them.
2. **`vipoint-document-order`**. A derived lexicographic `Ord` on two scalars. It retires one test,
   and the Region order behind `point-step-succ-pred`, `bracket-match-depth`,
   `word-motion-semantics` and `search-focus-circular` depends on it.
3. **`inline-search-kind-algebra`** and **`vimotion-to-buffercommand-table`**. Fieldless and
   char-payload enum tables, decidable by evaluation over their cases (gap 5 only for the
   injectivity corollary, which can be dropped to a per-case table). Together they retire 11 tests.
   Settle §5.4 before the second one.

Runner-up: **`point-step-succ-pred`** (adds gap 4 for the trait-object calls; no loops). After it,
**`search-focus-circular`** retires 11 tests, but it needs a `Vec` content model, `&mut self` and a
closure in `partition_point`. It also covers behaviour production never reaches (§5.2), so prove it
only once the owner keeps the search feature.
