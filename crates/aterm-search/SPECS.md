<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-search: spec recovery from its 340 unit tests

> Audit amendment: The 2026-10-16 code audit made fuzzy row matching discard zero-display-width hits, satisfying the same result invariant as literal/regex matching. The dated test inventory and §5.1 record the earlier snapshot.

> Status 2026-10-15: this is the crate's migration worklist. Each spec in §1 is proved
> through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted. The tests in §4 marked **delete** sample no spec and can go now, together with
> the `#[cfg(test)]`-only code they exercise. No test has been deleted yet.

Source: `crates/aterm-search` (read only). Every `#[test]` fn in `src/` and `tests/` was read,
along with the code each one reaches. Nothing was compiled or run; the claims below come from
reading the code. `grep '#[test]'` counts 343, but 3 of those are inside comments (the doc
comment of `tests/search_suite.rs` and the headers of `tests/index_release_memory.rs` and
`tests/index_column_map_memory.rs`), so the crate has **340** real tests.

Result: **48 specs**, covering **269 tests**. **71 tests are not spec
samples** (§4).

All paths below are relative to `crates/aterm-search/`. `tests/*.rs` means the integration
test files; `src/tests.rs` and `src/streaming/tests/*.rs` are unit test modules.

## 0. Notation used in the statements

- `Fold(s)` = `grapheme::lower_fold(s)`: `char::to_lowercase` per char, with U+03C2 (final
  sigma) mapped to U+03C3 (spec `lower-fold`). `LBM_t` = `grapheme::LowerByteMap::new(t).map_to_original`,
  which maps a byte offset in `Fold(t)` back to one in `t`.
- `Abs(idx)` for a `SearchIndex` = `(Rows: line → (text, Option<LineCells>), amb, cap, hw = len(),
  next, lowest, incomplete)`. This is the abstract state of spec `index-model`. Postings, bloom,
  `key_base`, `column_maps` and `lower_scratch` are representation, never compared.
- `CM(n)`: the column map of retained line `n`. It is `ColumnMap::with_cells(text, cells)` if the line
  was indexed with cells, else `ColumnMap::new(text, amb)`.
- `Occ_cs(q,t)`: the ascending byte offsets `p` with `t[p..p+|q|] = q`. These are all occurrences,
  overlapping ones included, and they are char boundaries because `q` and `t` are UTF-8.
  `Occ_ci(q,t)`: the ascending char-boundary offsets `p` of `Fold(t)` at which `Fold(t)[p..]`
  starts with `Fold(q)`.
- `LM_cs(n)`, `LM_ci(n)`, `LM_rx(n)`: the per-line match lists defined in `index-literal-cs`,
  `index-literal-ci` and `index-regex-batch`. Every element has `start_col < end_col`, because
  spans of zero display width are dropped. `All_mode` = `LM_mode(n)` concatenated over
  `n ∈ dom Rows` ascending, uncapped. `MAX` = `MAX_SEARCH_MATCHES`.
- `Batch(All, d)` = `SearchResults { matches: d = Forward ? take(MAX, All) : the last MAX of All
  (ascending), incomplete: results_may_be_incomplete() ∨ |matches| ≥ MAX ∨ cut_short_lines > 0,
  lowest_retained_line: lowest_retained_line(), cut_short_lines }` (spec `index-batch-cap`).
- `scan_line(re,t)` = `re.find_iter_with_limit(t, regex_line_budget(|t|))`. `Cut(n)` =
  `scan_line(re, Rows[n].text).step_limit_exceeded()` once the iterator is exhausted.
- `Contains_cs(t,q)` ⇔ `q` occurs in `t`. `Contains_ci(t,q)` ⇔ `Fold(t)` contains `Fold(q)`.
  `lines(F)` for a `LineFrame` = `base + offsets`, ascending.
- `T(t)`: the trigram set of a line (spec `index-posting-invariant`).
- Streaming: `E` = a `StreamingSearch` with fields `(state, pattern, mode, compiled, results, seen,
  current_index, scan_progress, total, config)`. `INV-1..6` are the six predicates of
  `src/streaming/engine/invariants.rs` (`#[cfg(test)]`). Spec `stream-invariants` states them over
  every production operation, so the `verify_*` helpers can be deleted with the tests.
- `project(E)` (for `stream-refines-model`) = `tests/conformance_streaming.rs::project`:
  `(state 0..3, scan_progress+1, |results|, total, current_index)`.
- `BmInv`, `Abs(bm)`: the `SparseBitmap` invariant and abstraction (specs `bitmap-*`).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (the tests this spec retires; every
test appears under exactly one spec), `gaps` (the audit §7 gap numbers that block the proof; gap 0,
a seal that publishes Clean rows, blocks every spec and is listed on each), `difficulty` (the Rust
features the proof must handle).

```tsv
id	kind	functions	statement	subsumes	gaps	difficulty
search-result-types	contract	types::SearchMatch::{new,len,is_empty}, types::SearchResults::{new,with_cut_short_lines}, streaming::StreamingMatch::new	SearchMatch::len() = end_col.saturating_sub(start_col); is_empty() ⇔ end_col ≤ start_col. SearchResults::new(m,i,l) has cut_short_lines = 0. with_cut_short_lines(k) sets cut_short_lines = k and incomplete := incomplete ∨ k > 0. StreamingMatch::new(r,s,e).match_len = e.saturating_sub(s).	src/tests.rs::search_match_helpers	0,1,2,3	&self struct fields, saturating_sub, Vec field
regex-line-budget	contract	index::{regex_line_budget,scan_line}	∀ len: regex_line_budget(len) = clamp(min(len,u64::MAX).saturating_mul(REGEX_STEP_PER_BYTE), REGEX_STEP_LIMIT, REGEX_LINE_STEP_CEILING) = clamp(1024·len, 2^22, 2^26). Hence it is monotone in len and ≤ max(1024·len, 2^22). scan_line(re,t) = re.find_iter_with_limit(t, regex_line_budget(|t|)).	src/index.rs::the_line_budget_scales_with_the_line_under_a_ceiling	0,3,12	u64 saturating_mul, clamp, try_from
bloom-layout	contract	bloom::BloomFilter::{with_size,with_capacity,storage_layout,requested_bits_for_capacity,default}	∀ n: with_size(n) has num_bits = 64·⌈min(max(n,64),MAX_BITS)/64⌉, |bits|·64 = num_bits, every word 0, count = 0. So 64 ≤ num_bits ≤ MAX_BITS, num_bits % 64 = 0, and nothing overflows, even for n = usize::MAX. ∀ c: with_capacity(c) = with_size(max(min(c,MAX_EFFECTIVE_CAPACITY).saturating_mul(10), 64)); clamping c never changes the result. default() = with_capacity(10_000), whose num_bits is 100_032.	src/bloom_tests.rs::bloom_with_size, src/bloom_tests.rs::bloom_with_size_rounding, src/bloom_tests.rs::bloom_default, src/bloom_tests.rs::bloom_zero_capacity, src/bloom_tests.rs::bloom_large_capacity_no_overflow, src/bloom_tests.rs::bloom_with_size_max_no_overflow, src/bloom_tests.rs::bloom_capacity_near_overflow_boundary	0,1,3	scalar usize arithmetic (div, %, saturating_mul, comparisons), vec! length
stream-navigation	contract	streaming::StreamingSearch::{next_index,next_match,prev_match}	next_index(i,len,dir,wrap) = 0 if len = 0. Forward: i ≥ len ? (wrap ? 1 : i) : i+1. Backward: i ≤ 1 ? (wrap ? len : i) : i−1. next_match/prev_match set current_index := next_index(current_index, |results|, Forward/Backward, wrap_enabled) iff state = HasResults ∧ results ≠ []; otherwise they change nothing.	src/streaming/tests/navigation.rs::navigation_works, src/streaming/tests/navigation.rs::navigation_without_wrap, src/streaming/tests/boundary.rs::navigation_single_match_next_stays, src/streaming/engine/operations.rs::test_next_match_noop_when_no_results, src/streaming/engine/operations.rs::test_prev_match_noop_when_idle, src/streaming/engine/operations.rs::test_next_index_forward_wrap, src/streaming/engine/operations.rs::test_next_index_forward_no_wrap, src/streaming/engine/operations.rs::test_next_index_backward_wrap, src/streaming/engine/operations.rs::test_next_index_backward_no_wrap, src/streaming/engine/operations.rs::test_next_index_empty_len_returns_zero	0,2,3	enum match, scalar ±1, &mut self
eviction-schedule	contract	index::{final_evicted_prefix,max_cached_for_retained}, index::SearchIndex::evict_oldest_lines	Let m = max(cap,1), t = ⌊3m/4⌋, r = m+1−t. final_evicted_prefix(N,cap) = 0 if N ≤ m, else min(N, (1+⌊(N−m−1)/r⌋)·r). This equals lowest_retained_line() of an index with max_cached_lines = cap after index_line(i, text_i) for i = 0..N−1 (distinct ascending lines). max_cached_for_retained(R) = min{M | ⌊3M/4⌋ ≥ R} = ⌈4R/3⌉ (0 for R = 0), so an index with that cap keeps its newest R lines through any eviction.	src/budgeted.rs::final_evicted_prefix_matches_real_index_schedule, src/tests.rs::max_cached_for_retained_protects_the_newest_lines	0,1,2,3,5,7	the closed forms are scalar saturating/div arithmetic (gap 3 only); the equality with the real index needs index-model (HashMap, sort)
fuzzy-subsequence	contract	streaming::StreamingSearch::fuzzy_match	∀ text t, pattern p: fuzzy_match(t,p) ⇔ p.chars() is a subsequence (not necessarily contiguous) of t.chars(). In particular fuzzy_match(t,"") = true.	src/streaming/engine/matching.rs::fuzzy_match_rows	0,1,3,7	Chars iterator, nested loop with early return
bloom-estimated-fpr	contract	bloom::BloomFilter::{estimated_fpr,is_saturated}	estimated_fpr() = 0.0 if count = 0, else (1 − exp(−K·count/num_bits))^K computed in f64. is_saturated() ⇔ estimated_fpr() > 0.5.	src/bloom_tests.rs::bloom_estimated_fpr	0,3,6	f64 exp/powi, usize→f64 casts
memchr-family	equivalence	bytesearch::{memchr,memchr2,memrchr,block_mask,zero_mask,word_at,splat}	∀ needle bytes a,b and haystack h: memchr(a,h) = h.iter().position(|x| x==a); memchr2(a,b,h) = h.iter().position(|x| x==a ∨ x==b); memrchr(a,h) = h.iter().rposition(|x| x==a). Key lemma: zero_mask(w) ≠ 0 ⇔ w has a zero byte lane; the per-lane rescan makes the false positives in higher lanes harmless.	src/bytesearch.rs::word_boundaries_are_covered, src/bytesearch.rs::a_one_byte_after_a_zero_byte_does_not_shift_the_answer, src/bytesearch.rs::byte_scanning_matches_the_reference, src/bytesearch.rs::every_byte_value_is_findable	0,1,3,5,7	slices, as_chunks const-size arrays, SWAR bit tricks with wrapping_sub, nested loops with early return
ascii-ci-matches	equivalence	literal::AsciiCaseInsensitiveMatches::{new,next}	Requires h.is_ascii() ∧ n.is_ascii(). When |n| > 0, AsciiCaseInsensitiveMatches::new(h,n) yields exactly the ascending list {i | i+|n| ≤ |h| ∧ h[i..i+|n|].eq_ignore_ascii_case(n)}, overlapping occurrences included; when |n| = 0 it yields nothing. This holds on both the short (|h| ≤ 8) path and the memchr2 path.	src/literal.rs::finds_single_byte_in_both_cases, src/literal.rs::finds_two_byte_and_overlapping_matches, src/literal.rs::handles_empty_long_and_missing_needles	0,1,2,3,5,7	Iterator impl with a &mut self cursor, checked arithmetic, call to bytesearch::memchr2
lower-fold	contract	grapheme::{lower_fold_char,lower_fold,lower_fold_into,lower_need}	lower_fold(s) = lower_fold_char(c) concatenated over c ∈ s.chars(), where lower_fold_char(c) = c.to_lowercase() with U+03C2 mapped to U+03C3. lower_fold_into(s,out) leaves out = lower_fold(s). Hence lower_fold(a++b) = lower_fold(a)++lower_fold(b), the prefix property narrowed-search relies on. lower_need(t) = Unicode if ¬t.is_ascii(); Ascii if t is ASCII with an uppercase byte; None otherwise. For ASCII t, lower_fold(t) = t.to_ascii_lowercase(), and lower_need(t) = None ⇒ lower_fold(t) = t.	(none: a lemma for the ci specs)	0,1,3,5,7	String push loops, char::to_lowercase (std Unicode tables), iterator closures
lower-byte-map	equivalence	grapheme::LowerByteMap::{new,map_to_original}, grapheme::map_lower_byte_to_original (test-only reference)	∀ text t, lb: LowerByteMap::new(t).map_to_original(lb) = the original byte offset of the first char of t whose lowered (lower_fold_char) start offset is ≥ lb, or |t| if there is none. Equivalently it equals map_lower_byte_to_original(t,lb), which can then be deleted.	src/grapheme.rs::lower_byte_map_matches_ascii, src/grapheme.rs::lower_byte_map_matches_kelvin_sign, src/grapheme.rs::lower_byte_map_matches_mixed_unicode, src/grapheme.rs::lower_byte_map_long_line_all_offsets	0,1,3,5,7	Vec<(usize,usize)> built in a loop, binary_search_by_key
searcher-find-rfind	equivalence	bytesearch::Searcher::{new,find_in,rfind_in,rare,factorization}, bytesearch::{two_way,factorize,maximal_suffix,Skip::advance,View::{get,scan},rare_byte}	∀ needle n, haystack h, and any history of earlier find_in/rfind_in calls on the same Searcher (any cached forward/backward factorization and rare byte): find_in(h) = (n empty ? Some(0) : min{i | i+|n| ≤ |h| ∧ h[i..i+|n|] = n}); rfind_in(h) = (n empty ? Some(|h|) : max of the same set); both are None when the set is empty. Lemma: every position Skip::advance steps over has h[at+offset] ≠ n[offset].	src/bytesearch.rs::empty_needle_and_empty_haystack, src/bytesearch.rs::needle_longer_than_haystack, src/bytesearch.rs::overlapping_matches_are_all_reachable, src/bytesearch.rs::non_ascii_bytes_are_just_bytes, src/bytesearch.rs::a_reused_searcher_answers_the_same_as_a_fresh_one, src/bytesearch.rs::a_warm_searcher_matches_the_reference, src/bytesearch.rs::exhaustive_binary_alphabet, src/bytesearch.rs::exhaustive_ternary_alphabet, src/bytesearch.rs::periodic_needles_match_the_reference, src/bytesearch.rs::named_edges_match_the_reference, src/bytesearch.rs::realistic_haystacks_match_the_reference, src/bytesearch.rs::repository_corpus_matches_the_reference	0,1,2,3,5,7,11	Cell interior mutability, const-generic View<REV>, the Two-Way critical-factorization proof, saturating index arithmetic, nested while loops
searcher-linear-time	other (cost)	bytesearch::{two_way,Skip::advance}	find_in/rfind_in terminate on every input and make O(|h|+|n|) byte comparisons. The periodic branch carries memory, so no haystack byte is compared twice per alignment. The adaptive skip switches itself off once calls ≥ PROBE ∧ jumped < calls·MIN_STRIDE, and from then on the loop is plain Two-Way with an unchanged answer.	src/bytesearch.rs::a_useless_skip_switches_itself_off_and_the_answer_is_unchanged	0,7	loop variants/decreases, cost accounting
bitmap-set-refinement	refinement	bitmap::SparseBitmap::{default,new,insert,remove,drop_below,shift,len,is_empty,to_vec,into_iter}, bitmap::intersect_sorted, BitAnd/BitAndAssign for SparseBitmap	Let Abs(bm) = bm.to_vec(). For every bm reachable from default() by these mutators: Abs(bm) is strictly ascending, len() = |Abs|, is_empty() ⇔ Abs = []. insert(v): Abs' = sort(Abs ∪ {v}), returns v ∉ Abs. remove(v): Abs' = Abs ∖ {v}, returns v ∈ Abs. drop_below(w): Abs' = [x ∈ Abs | x ≥ w], the same as removing each x < w one at a time. shift(up,by) requires (up ? last+by ≤ u32::MAX : first ≥ by) and gives Abs' = [x ± by]. Abs(&a & &b) = Abs(a) ∩ Abs(b) ascending, and a &= &b gives the same. into_iter() yields Abs. intersect_sorted(a,b) of strictly ascending slices = their ascending intersection.	src/bitmap.rs::test_insert_and_len, src/bitmap.rs::test_insert_out_of_order_stays_sorted, src/bitmap.rs::test_insert_return_value, src/bitmap.rs::test_remove, src/bitmap.rs::test_drop_below, src/bitmap.rs::test_drop_below_equals_repeated_front_remove, src/bitmap.rs::test_into_iter_sorted, src/bitmap.rs::test_bitand, src/bitmap.rs::test_bitand_disjoint_and_empty, src/bitmap.rs::test_bitand_assign, src/bitmap.rs::test_default_is_empty, src/bitmap.rs::test_large_gap_varint_roundtrip	0,1,2,3,5,7	Vec<u8>/Vec<u32> content, LEB128 decode loops with a labelled break, &mut self, binary_search, operator traits
bitmap-canonical-encoding	invariant	bitmap::SparseBitmap (all mutators), bitmap::{push_varint,rebuild_from_sorted,remove_last,compact_prefix,from_sorted}	BmInv(bm): count = |Abs|; count > 0 ⇒ first = min Abs ∧ last = max Abs; deltas[head..] = Enc(Abs), where Enc concatenates LEB128(x_{i+1} − x_i) over consecutive values, so it equals from_sorted(Abs).deltas byte for byte; head = 0 ∨ head < |deltas| − head; |Abs| ≤ 1 ⇒ head = 0 ∧ deltas = []. BmInv holds for default() and is preserved by insert, remove (including the remove_last fast path), drop_below, shift, &=, rebuild_from_sorted (which also sets head = 0) and clone.	src/bitmap.rs::prefix_eviction_preserves_mixed_operations_and_varint_boundaries, src/bitmap.rs::test_single_value_has_no_delta_bytes, src/bitmap.rs::remove_last_is_o1_and_canonical	0,1,2,3,5,7	Vec<u8> byte-level equality, u32 cursor arithmetic, varint encoding lemmas, loops
bitmap-eviction-cost	other (cost)	bitmap::SparseBitmap::{drop_below,remove,remove_last,compact_prefix}	drop_below(w) and remove(max) never call to_vec, so no survivor is decoded. drop_below consumes exactly the gap bytes of the removed values (PREFIX_BYTES grows by those bytes). Over any sequence of operations, the bytes compact_prefix moves are ≤ the bytes consumed. Under a sliding window (drop_below(k+1); insert(k+W)), deltas.capacity() stays ≤ 2× its initial capacity. The layout clause size_of::<SparseBitmap>() = size_of::<(Vec<u8>,u32,u32,u32)>() on 64-bit becomes a const assertion.	src/bitmap.rs::full_history_eviction_work_is_amortized_and_prefix_storage_is_bounded	0,1,2,7	resource counters (thread_local Cell under cfg(test)), amortized potential argument over Vec capacity, loops
bloom-no-false-negatives	invariant	bloom::BloomFilter::{insert_bytes,insert,might_contain_bytes,might_contain,hash_pair,get_bit_index,set_bit,get_bit}	For every filter f and every sequence insert_bytes(k_1..k_m) on it with no clear() after k_i: f.might_contain_bytes(k_i) = true. insert(s) ≡ insert_bytes(s.as_bytes()) and might_contain(s) ≡ might_contain_bytes(s.as_bytes()). Every bit index is < num_bits (no out-of-bounds access), and insert_bytes only sets bits (monotone).	src/bloom_tests.rs::bloom_insert_and_query, src/bloom_tests.rs::bloom_no_false_negatives, src/bloom_tests.rs::bloom_insert_bytes, src/bloom_tests.rs::bloom_empty_string	0,1,2,3,5,7	Vec<u64> content, FNV wrapping_mul/rotate, % num_bits, loops over K, &mut self
bloom-clear-count-empty	contract	bloom::BloomFilter::{clear,item_count,count,might_contain_bytes}	item_count() = the number of insert/insert_bytes calls since construction or the last clear(). clear() sets every word to 0 and count to 0 and keeps num_bits. If every word is 0 then ∀ k: might_contain_bytes(k) = false; in particular a fresh or cleared filter rejects every key.	src/bloom_tests.rs::bloom_clear	0,1,2,5	Vec<u64>::fill, quantified Vec content, &mut self
bloom-probe-cost	other (cost)	bloom::BloomFilter::{insert_bytes,might_contain_bytes}	insert_bytes makes exactly K = 7 set_bit calls. might_contain_bytes makes get_bit calls in order i = 0..K−1 and stops at the first unset bit: exactly K calls when it returns true, ≤ K otherwise. Neither count depends on num_bits.	src/bloom_tests.rs::bloom_lookup_constant_time, src/bloom_tests.rs::bloom_insert_constant_time	0,2,3,7	cost counters, early-return loop
line-cells-model	refinement	cells::LineCells::{new,push,push_repeated,append_shifted,append_wrapped_row,end_byte,end_col,identity,is_empty,iter,push_run}, cells::CellRun::{cell,continued_by,end_byte}	Abs(c) = c.iter(), a list of CellSpan. push(r,col,w) = push_repeated(r,col,w,1). push_repeated(r,col,w,k): if |r| > 0 ∧ k > 0 ∧ r.start ≥ end_byte(c), with |r| and k fitting u32, then Abs' = Abs ++ [(r.start+i|r|, r.start+(i+1)|r|, col+i·w', col+(i+1)·w') | i < k] where w' = min(w, u16::MAX); otherwise unchanged. append_shifted(o,b,cols): each run of o shifted by (b,cols) is appended iff its byte start ≥ end_byte. append_wrapped_row(line,text,cells,rs): pads columns end_col..rs with one-column ' ' cells (pushing ' ' onto line), then append_shifted(cells,|line|,rs), then line += text. end_byte/end_col are those of the last cell (0 if none). identity(n) = n one-byte, one-column cells from 0. Representation: head = None ⇒ tail = [], and no two consecutive runs satisfy continued_by (runs are merged maximally).	src/cells.rs::cells_read_back_as_pushed_in_runs_of_one_shape, src/cells.rs::empty_and_overlapping_cells_are_not_recorded, src/cells.rs::appended_cells_are_shifted_and_merged, src/cells.rs::a_wrapped_row_joins_at_its_stride	0,1,2,3,5,7	Option head + Vec tail, &mut self, saturating arithmetic, String push
column-map-walk	refinement	grapheme::ColumnMap::{new,span,lookup,start_column,end_column,identity}, grapheme::RunBuilder::{push,finish}, grapheme::ClusterRun::grapheme	Let Ref(t,a) = [(g.byte_offset, g.start_col, before_g) for g in grid_graphemes_with(t,a)] ++ [(|t|, total, total)], where before_g is the end_col of the previous grapheme (0 for the first) and total is the last end_col. Let look(x) = the first entry with offset ≥ x (else total). ∀ t, a, s, e: ColumnMap::new(t,a).span(s,e) = (look(s).start, look(e).before). Offsets above u32::MAX saturate. Corollary: end_column(o) = byte_to_column(t,o), the cursor column after every grapheme that starts before o.	src/grapheme.rs::the_runs_answer_as_the_per_grapheme_map_did, src/grapheme.rs::column_map_matches_byte_to_column_ascii, src/grapheme.rs::column_map_matches_byte_to_column_cjk, src/grapheme.rs::column_map_empty_string, src/grapheme.rs::column_map_matches_byte_to_column_mixed_unicode, src/grapheme.rs::column_map_long_line_all_offsets	0,1,3,5,7,12	Box<[ClusterRun]>, partition_point, u32 narrowing, cross-crate aterm_grapheme::grid_graphemes_with
column-map-with-cells	contract	grapheme::ColumnMap::with_cells, cells::LineCells::{iter,is_identity}	For text t and cells c (cells with byte ≥ |t| are ignored), each grapheme at offset o gets (start,before): before any cell (0,0); at o = cell.byte, (cell.col, the previous cell's end_col or 0); inside a cell whose first char has char_width > 0, (cell.col, cell.end_col); inside a cell and zero-width, (cell.end_col, cell.end_col); past the cell's text, (cell.end_col, cell.end_col). total = the end_col of the last cell with byte < |t| (0 if none). with_cells(t,c).span(s,e) is look() over that list, as in column-map-walk. Corollary: if c is what a grid stores when it writes t left to right in one go (GridColumns steps; VS15 on a wide glyph excluded), then with_cells(t,c).span = ColumnMap::new(t,a).span for all s,e.	src/grapheme.rs::cells_of_a_line_written_in_one_go_answer_as_the_walk, src/grapheme.rs::cells_the_grid_kept_apart_are_where_matches_are_drawn, src/grapheme.rs::graphemes_inside_a_cell_and_bytes_outside_every_cell	0,1,3,5,7,12	peekable iterator merge, grapheme_indices (cross-crate), Option state machine
column-map-runs-compact	invariant	grapheme::RunBuilder::{push,finish}, grapheme::ColumnMap::heap_bytes	The runs of a ColumnMap are the greedy maximal run-length encoding of its per-grapheme list. A new run starts exactly when a grapheme folds or follows a gap (start ≠ before), or breaks the run's byte/column stride. A map whose list is the identity (every byte one grapheme one column wide from 0, total = |t| > 0) stores zero runs. Hence a line that repeats one shape is one run, whatever its length.	src/grapheme.rs::a_line_of_one_shape_is_one_run	0,1,5,7	Vec push with last_mut, u16/u32 narrowing
display-columns	equivalence	grapheme::{display_columns,display_columns_with}	∀ t, a: display_columns_with(t,a) = ColumnMap::new(t,a).span(|t|,|t|).1, the total grid columns of t (str_grid_columns_with). For printable-ASCII t it is |t|. display_columns(t) = display_columns_with(t, Single).	src/grapheme.rs::width_only_matches_coordinate_map_for_unicode_and_controls	0,1,5,7,12	&str byte-predicate loop, cross-crate width routine
line-cells-column-queries	contract	cells::LineCells::{byte_at_column,bytes_in_columns,columns_of}	byte_at_column(col) = Some(byte of the first cell in line order with cell.col ≤ col < cell.end_col and width > 0), else None. bytes_in_columns(s,e) = Some(min byte .. max end) over the cells with width > 0, cell.col < e and cell.end_col > s, else None (so None whenever e ≤ s). columns_of(t,b0,b1) = ColumnMap::with_cells(t,self).span(b0,b1).	src/cells.rs::a_column_names_the_cell_drawn_over_it, src/cells.rs::a_column_range_takes_the_cells_drawn_over_it	0,1,3,5,7	div_ceil, NonZeroUsize, Range<usize>, iterator find_map
index-model	refinement	index::SearchIndex::{new,with_capacity,with_capacity_and_max,with_max_cached_lines,set_max_cached_lines,index_line,index_line_cow,push_line,make_room_for,rekey,evict_below_window,note_window_eviction,evict_oldest_lines,clear,release,drop_history_below,retain_history_from,set_history_prefix_eviction,set_ambiguous_width,len,lowest_retained_line,results_may_be_incomplete}, TerminalSearch (delegating mutators)	Constructors give Rows = ∅, hw = next = lowest = 0, incomplete = false, cap = max(cap,1). index_line_cow(n,t,c): if Rows[n].text = t, only Rows[n].cells := c, and only when c is given. Otherwise let newest = max(hw−1, n) and floor = newest − MAX_KEY_SPAN. If Rows ≠ ∅ ∧ n < floor, the line is dropped, lowest := max(lowest, floor) and incomplete := true. Otherwise rows below floor are dropped (with the same honesty update if any were), Rows[n] := (t,c), hw := max(hw, n+1), next := max(next, n+1), and if |Rows| > cap, the |Rows| − ⌊3cap/4⌋ smallest lines are dropped, lowest := the smallest remaining line (or next) and incomplete := true. push_line(t) = index_line(next, t), returning next. clear/release: Rows = ∅, hw = next = lowest = 0, incomplete = false (cap and amb kept). drop_history_below(f): if f > the smallest retained line, drop rows < f; honesty unchanged. retain_history_from(f): the same drop, then lowest := max(lowest,f), incomplete := true. set_history_prefix_eviction(Some(b>0)): lowest := b, incomplete := true; any other argument sets lowest := 0, incomplete := false. Observers: len() = hw, lowest_retained_line() = lowest, results_may_be_incomplete() = incomplete. Owned text is moved into the cache, not copied. TerminalSearch delegates: index_visible_content(b,ls) = index_line(b+i, ls[i]) in order; index_scrollback_line = push_line.	src/tests.rs::push_line, src/tests.rs::index_clear, src/tests.rs::len_is_a_high_water_mark, src/tests.rs::eviction_with_sparse_line_numbers, src/tests.rs::eviction_sets_incomplete_signal_and_watermark, src/tests.rs::cap_is_configurable_and_clear_resets_signal, src/tests.rs::terminal_search_surfaces_eviction_signal, src/tests.rs::drop_history_below_matches_fresh_rebuild_including_completeness, src/tests.rs::drop_history_below_is_a_noop_at_or_below_the_first_cached_line, src/index_key_window_tests.rs::lines_that_cannot_share_the_newest_lines_window_are_reported_evicted, src/index_key_window_tests.rs::an_index_kept_across_four_billion_rows_rekeys_and_stays_complete, src/index_key_window_tests.rs::a_descending_run_of_lines_moves_the_window_once	0,1,2,3,4,5,7,12	FxHashMap<usize,String>, sort_unstable of keys, Cow, &mut self, u32 key-window arithmetic, logging side effect
index-posting-invariant	invariant	index::SearchIndex::{index_line_cow,reindex_changed_row_postings,posting_trigram_set,insert_posting,evict_oldest_lines,evict_below_window,drop_history_below,make_room_for,rekey,might_contain,search (cfg(test))}	T(t) = windows3(bytes t) ∪ (lower_need(t) = Ascii ? windows3(ascii_lower t) : lower_need(t) = Unicode ? windows3(Fold t) : ∅). ∀ reachable idx: dom(trigrams) = ⋃_{n ∈ Rows} T(Rows[n].text); ∀ w: {key_base+k | k ∈ Abs(trigrams[w])} = {n ∈ dom Rows | w ∈ T(Rows[n].text)}, and no list is empty; every cached line is in [key_base, key_base+MAX_KEY]; ∀ w ∈ dom(trigrams): bloom.might_contain_bytes(w). Consequences: might_contain(q) is true whenever |q| < 3 or every trigram of q is in dom(trigrams), so it has no false negatives; the cfg(test) candidate iterator search(q) yields ascending lines ⊇ {n | q occurs in Rows[n].text}, and every retained line when |q| < 3.	src/tests.rs::index_and_search, src/tests.rs::empty_query, src/tests.rs::no_matches, src/tests.rs::bloom_filter_rejection, src/tests.rs::reindex_line, src/tests.rs::search_index_with_capacity	0,1,2,3,5,7	FxHashMap<[u8;3],SparseBitmap>, set difference over FxHashSet, quantified map invariant
index-bloom-dictionary	invariant	index::{insert_posting,SearchIndex::rebuild_bloom,SearchIndex::index_line_cow}	bloom.item_count() = the number of times a trigram's posting list went from absent or empty to non-empty since the last rebuild_bloom/clear, because insert_posting inserts into the bloom only then. Re-indexing a line with unchanged text leaves the bloom and postings unchanged. After rebuild_bloom: item_count = |dom trigrams| and num_bits = BloomFilter::with_capacity(min(max(|dom trigrams|,1000), MAX_EFFECTIVE_CAPACITY)).num_bits. Whenever index_line_cow returns with |dom trigrams| < MAX_EFFECTIVE_CAPACITY, ¬bloom.is_saturated(). Neither the row count nor per-row occurrence counts ever size the filter.	src/index_bloom_tests.rs::repeated_output_and_unchanged_refreshes_do_not_grow_the_filter, src/index_bloom_tests.rs::dictionary_growth_rebuilds_without_losing_live_trigrams, src/tests.rs::bloom_rebuild_tracks_trigram_volume_for_bulk_scrollback	0,1,2,3,5,6,7	HashMap entry API, float saturation predicate
index-literal-cs	refinement	index::SearchIndex::{search_with_positions,search_from_line,search_before_line,search_results_opts_direction (cs literal)}, iterators::{SearchMatchIterator,SearchMatchReverseIterator,next_literal_match,CandidateSource}, TerminalSearch::search	LM_cs(n) = [SearchMatch(n,s,e) | p ∈ Occ_cs(q,Rows[n].text) ascending, (s,e) = CM(n).span(p,p+|q|), s ≠ e]. ∀ reachable idx, q: if q = "" every call yields nothing. Otherwise search_with_positions(q) = take(MAX, All_cs); search_from_line(q,f) lazily yields [m ∈ All_cs | m.line ≥ f] (uncapped); search_before_line(q,b) yields reverse([m ∈ All_cs | m.line < b]); search_results_opts_direction(q,true,false,d) = Batch(All_cs,d). Candidate pruning (bloom, trigram postings, key base) never removes a match.	src/tests.rs::empty_query_search_with_positions, src/tests.rs::empty_query_terminal_search, src/tests.rs::search_with_positions, src/tests.rs::terminal_search_basic, src/tests.rs::search_from_line_basic, src/tests.rs::search_from_line_empty_query, src/tests.rs::search_from_line_short_query, src/tests.rs::search_from_line_no_matches, src/tests.rs::search_before_line_basic, src/tests.rs::search_before_line_empty_query, src/tests.rs::search_iterator_early_termination, src/tests.rs::search_match_iterator_multiple_matches_per_line, src/tests.rs::search_reverse_iterator_multiple_matches_per_line, src/tests.rs::search_multibyte_query_no_panic, src/tests.rs::search_iterator_multibyte_no_panic, src/tests.rs::search_single_multibyte_char_repeated, src/tests.rs::terminal_search_index_scrollback_lines_batch, src/tests.rs::terminal_search_index_visible_content, src/tests.rs::search_before_line_zero_returns_empty, src/tests.rs::search_10k_lines_common_char, src/tests.rs::search_before_line_descending_order_at_scale, src/tests.rs::search_before_line_range_bounded, src/tests.rs::zero_display_width_matches_are_skipped, src/iterators.rs::forward_literal_matches_equal_a_str_find_reference, tests/search_vs_bruteforce.rs::verified_search_matches_bruteforce, src/index_key_window_tests.rs::rows_past_u32_max_are_found_at_their_own_line_numbers	0,1,2,3,4,5,7	lazy iterators over HashMap lookups, Searcher, ColumnMap, u32 key/line conversion
index-literal-ci	refinement	index::SearchIndex::search_case_insensitive, index::CaseInsensitiveMatcher::{new,visit_matches}, index::SearchIndex::literal_candidates_{forward,backward}	Let t = Rows[n].text and let LBM_t be the identity when t and Fold(q) are ASCII, LowerByteMap::new(t) otherwise. LM_ci(n) = [SearchMatch(n,s,e) | p ∈ Occ_ci(q,t) ascending, (s,e) = CM(n).span(LBM_t(p), LBM_t(p+|Fold q|)), s ≠ e]. ∀ reachable idx, q: search_results_opts_direction(q,false,false,d) = Batch(All_ci,d), with no matches when Fold(q) = "". The ASCII ≤2-byte path, the lowered-buffer path and trigram candidate selection all agree with this.	src/tests.rs::case_insensitive_ascii_search_reports_overlapping_matches, src/tests.rs::case_insensitive_ascii_search, src/tests.rs::case_insensitive_non_ascii_search, src/tests.rs::case_insensitive_greek_final_sigma_unified, src/tests.rs::case_insensitive_overlapping_matches, src/tests.rs::case_insensitive_search_uses_trigram_index	0,1,2,3,4,5,7	String buffer reuse, closures with early termination, VecDeque, std str::find
index-batch-cap	contract	index::SearchIndex::{search_matches,search_results_opts_direction,search_regex,search_case_insensitive,search_with_positions}	For every mode the batch result is Batch(All,d) (§0). The cap can cut mid-line: a single line with more than MAX occurrences yields its first (or last) MAX. The result does not depend on hash-map iteration order.	src/tests.rs::batch_search_caps_matches_and_flags_incomplete, src/tests.rs::single_line_overlaps_are_streamed_under_the_batch_cap, src/tests.rs::regex_batch_search_caps_matches_deterministically	0,1,4,5,7	Vec reverse, take, VecDeque window
index-regex-batch	contract	index::SearchIndex::{search_regex,search_results_opts_direction (regex),search_with_positions_opts_direction}, index::scan_line	Let re = compile_regex(q,cs); compile errors propagate unchanged. LM_rx(n) = [SearchMatch(n,s,e) | cap ∈ scan_line(re,t), cap.start < cap.end, (s,e) = CM(n).span(cap), s ≠ e]. search_results_opts_direction(q,cs,true,d) = Batch over the ascending concatenation of LM_rx, where cut_short_lines = #{n | Cut(n)} over the lines whose scan ran to exhaustion before the cap stopped the walk (forward excludes the line on which the cap is reached; backward includes it). Lines after a cut-short line are still scanned, and a cut line keeps the matches it yielded before the cut. search_with_positions_opts_direction (cfg(test)) returns Err(ScanCutShort) iff cut_short_lines > 0.	src/tests.rs::regex_search_filters_zero_length_matches, src/tests.rs::regex_search_preserves_nonzero_matches_alongside_zero_length, src/index.rs::a_pattern_that_cannot_finish_a_line_is_reported_never_passed_off, src/index.rs::the_pathological_pattern_over_a_long_line_stops_at_the_ceiling	0,1,2,4,5,7,12	cross-crate aterm_regex Matches iterator with a step budget, labelled break
regex-compile	contract	index::SearchIndex::compile_regex, streaming::StreamingSearch::compile_regex, budgeted::BudgetedSearch::with_max_cached_lines	SearchIndex::compile_regex(q,cs) = Err(PatternTooLong) if |q| > 1024. Otherwise it builds RegexBuilder(cs ? q : "(?i)"+q) with size_limit = 128 KiB, dfa_size_limit = 1 MiB, step_limit = REGEX_STEP_LIMIT; the result is Ok(re) with re.step_limit() = REGEX_STEP_LIMIT and ¬re.step_limit_exceeded(), or Err(InvalidRegex(builder message)). It never panics. StreamingSearch::compile_regex is the same builder keyed on config.case_sensitive, with errors mapped to SearchError::InvalidRegex, and start_search(p,Regex) fails with InvalidRegex exactly then. BudgetedSearch::new/with_max_cached_lines(q,cs,true,..) = Err(e) iff compile_regex(q,cs) = Err(e).	src/index.rs::the_compiled_pattern_carries_the_scan_budget, src/tests.rs::regex_pattern_too_long_rejected, src/tests.rs::regex_compilation_size_limit_enforced, src/budgeted.rs::invalid_regex_fails_at_construction, src/streaming/engine/operations.rs::the_streaming_engine_compiles_with_the_scan_budget, src/streaming/regex_tests.rs::regex_invalid_pattern	0,1,4,12	String/format!, Result enum mapping, cross-crate builder
index-point-find	contract	TerminalSearch::find_direction_opts, index::SearchIndex::{find_next_case_insensitive_from,find_prev_case_insensitive_from,find_regex_direction,find_compiled_regex_from,point_answer}, TerminalSearch::{find_next,find_prev,find_next_opts,find_prev_opts} (cfg(test))	Let All be the UNCAPPED match list of the mode (All_cs, All_ci or regex) and ≺ the lexicographic order on (line, start_col). find_direction_opts(q,cs,rx,{anchor a, dir, inclusive, wrap}): Forward returns the ≺-first m ∈ All with (m.line,m.start_col) ≻ a (⪰ if inclusive); Backward returns the ≺-last m with (m.line,m.start_col) ≺ a (⪯ if inclusive). If there is none and wrap is set, Forward returns the ≺-first m ∈ All and Backward the ≺-last; otherwise None. Regex mode first propagates compile errors, and returns Err(ScanCutShort) iff the answer is None and some line the walk(s) scanned had Cut(n). Results never depend on MAX. An empty query (or empty fold) gives Ok(None).	src/tests.rs::terminal_search_find_next_prev, src/tests.rs::find_next_prev_same_line_column_boundary, src/tests.rs::find_next_optimized, src/tests.rs::find_prev_optimized, src/tests.rs::find_prev_usize_max_does_not_overflow, src/tests.rs::find_prev_boundary_at_first_line, src/tests.rs::find_next_past_last_line, src/tests.rs::terminal_search_option_navigation_is_ordered_and_case_insensitive	0,1,2,4,5,7,12	closures over iterators, saturating anchor arithmetic, Result
literal-candidate-bound	contract	index::SearchIndex::literal_candidate_bound	Let e = cs ? q : Fold(q). literal_candidate_bound(q,cs) = None if |e| < 3; Some(0) if Rows = ∅ or some trigram of e has no posting list (a bloom rejection implies this); otherwise Some(min over trigrams w of e of |{n ∈ Rows | w ∈ T(Rows[n].text)}|). This bound is ≥ the number of candidate lines the engine's own walk visits for e.	src/tests.rs::literal_candidate_bound_reports_the_engines_own_candidate_count	0,1,3,5,7	Option, iterator min over posting lengths
narrowed-search	contract	index::SearchIndex::search_literal_narrowed, index::CaseInsensitiveMatcher::has_occurrence, types::LineFrame::{with_base,push_offset,lines,contains,len,is_empty}	For q ≠ "": search_literal_narrowed(q,cs,None).results = Batch(All_mode,Forward), and its occurrence_lines is None if the walk was capped, else Some(F) with lines(F) = the ascending {n ∈ dom Rows | Contains(Rows[n].text,q)}, as absolute lines (base+offset) unaffected by later re-keying. search_literal_narrowed(q,cs,Some(F)).results.matches = take(MAX, LM_mode(n) concatenated over n ∈ lines(F) ∩ dom Rows ascending), with incomplete/lowest as in Batch; its occurrence_lines = (capped ? None : Some({n ∈ lines(F) ∩ dom Rows | Contains(Rows[n].text,q)})); no posting list or bloom probe is touched. Hence, if F is the occurrence frame of any char-prefix q' of q (q' = q included) on an index with the same Rows, results = Batch(All_mode,Forward), because every match line of q contains q' (Fold(q'++r) = Fold(q')++Fold(r)). q = "" gives (SearchResults::new([], incomplete, lowest), None).	src/tests.rs::narrowed_typing_equals_batch_for_every_prefix_step, src/tests.rs::occurrence_frame_keeps_zero_width_only_lines_narrable, src/tests.rs::frame_reverifies_its_own_query_after_backspace, src/tests.rs::capped_narrowed_walk_matches_forward_batch_and_yields_no_frame, src/tests.rs::narrowed_walk_carries_eviction_honesty_like_batch, src/index_key_window_tests.rs::a_frame_survives_the_index_rekeying_under_it	0,1,2,4,5,7,11	generic enum over iterators, labelled break, u32 offsets
index-candidate-cost	other (cost)	index::SearchIndex::{literal_candidates_forward,literal_candidates_backward,posting_lists,search_from_line,search_before_line,find_next_case_insensitive_from,find_prev_case_insensitive_from}, index::CaseInsensitiveMatcher::visit_matches, iterators::CandidateSource	For a literal walk with folded query fq from line f: if |fq| < 3 the candidate sequence is the numeric range [max(f, first_cached_line), line_count) (descending for backward walks). It is empty when Rows = ∅, the bloom rejects, or a trigram has no posting. Otherwise it is the ascending intersection of the DISTINCT posting lists of fq's trigrams, restricted to ≥ f; posting_lists deduplicates identical lists, so LAST_UNIQUE = the number of distinct trigrams. Walks visit candidates lazily in order, once per candidate line, and stop at the line holding the first qualifying match. The case-insensitive verifier lowers a line into its buffer iff ¬(text ASCII ∧ fq ASCII ∧ |fq| ≤ 2), exactly once per visited line.	src/tests.rs::find_next_range_query_skips_prefix_matches, src/tests.rs::case_sensitive_point_navigation_builds_no_owned_intersection, src/tests.rs::case_insensitive_100k_ascii_short_query_work_is_bounded, src/tests.rs::case_insensitive_short_query_skips_sparse_empty_prefix, src/tests.rs::case_sensitive_reverse_short_query_skips_sparse_empty_prefix, src/tests.rs::case_insensitive_navigation_visits_only_trigram_candidates, src/tests.rs::case_insensitive_repeated_prefix_uses_linear_matcher_path	0,1,2,5,7	cost counters, lazy iterators, pointer-identity dedup
index-observational-equivalence	equivalence	SearchIndex (all observers), TerminalSearch::{index_numbered_content_owned,index_visible_content}, SearchIndex::{reindex_changed_row_postings,rebuild_bloom,rekey}	A corollary of index-model plus the query specs. ∀ reachable idx1, idx2 with Abs(idx1) = Abs(idx2), every observer returns equal values: search_with_positions, search_from_line, search_before_line, search_results_opts[_direction], find_*, search_literal_narrowed, literal_candidate_bound (as defined by Abs), len, lowest_retained_line, results_may_be_incomplete. In particular an arbitrary edit script equals a fresh build over the final texts; A→B→A equals no edit; rebuild_bloom and re-keying are invisible; owned and borrowed indexing are indistinguishable.	src/tests.rs::changed_row_reindex_matches_from_scratch_rebuild, src/tests.rs::changed_row_reindex_round_trip_is_identity, src/tests.rs::lowercase_fast_path_keeps_insert_remove_symmetric, src/tests.rs::owned_numbered_content_matches_borrowed_indexing, src/index_bloom_tests.rs::replacing_last_posting_and_reintroducing_it_preserves_all_search_modes	0,1,2,4,5,7	relational (two-state) reasoning over HashMap-backed state
budgeted-refines-one-shot	equivalence	budgeted::BudgetedSearch::{new,with_max_cached_lines,with_ambiguous_width,feed_row,feed_row_owned,feed_row_cells,feed_row_cow,verify_row,results}, index::final_evicted_prefix	∀ rows r_0..r_{N−1} (text with optional LineCells), base, q, cs, rx, cap, amb: if BudgetedSearch::with_max_cached_lines(q,cs,rx,base,N,cap) = Ok(b), with with_ambiguous_width(amb) applied, and each r_i is fed in order by feed_row_owned or feed_row_cells (feed_row is the cfg(test) borrowed twin), then after the N-th feed b.results() equals the one-shot search: a SearchIndex with max_cached_lines = cap and ambiguous_width = amb, built by index_line_cow(base+i, r_i.text, r_i.cells) for i < N, then search_results_opts(q,cs,rx).unwrap(). The equality covers matches, incomplete, lowest_retained_line and cut_short_lines. Construction fails exactly when that batch call would (regex-compile).	src/budgeted.rs::owned_feed_matches_borrowed_and_batch_unicode_coordinates, src/budgeted.rs::any_slicing_equals_one_shot_across_all_modes, src/budgeted.rs::match_cap_matches_one_shot_and_reports_incomplete, src/budgeted.rs::eviction_matches_one_shot_capped_index, src/budgeted.rs::combined_match_cap_and_eviction_equals_one_shot, src/budgeted.rs::a_row_the_regex_scan_cannot_finish_is_reported_like_the_one_shot	0,1,2,4,5,7,12	Cow, enum matcher with captured closures, cross-crate regex, relational equality with the index model
budgeted-progressive	contract	budgeted::BudgetedSearch::{feed_row_cow,rows_fed,total_rows,is_complete,results,results_range,result_count}	rows_fed grows by exactly 1 per feed while rows_fed < total_rows; a feed when rows_fed ≥ total_rows changes nothing. is_complete() ⇔ rows_fed ≥ total_rows. results().lowest_retained_line is the final watermark fixed at construction (base + final_evicted_prefix(total,cap), or 0), and no emitted match lies below it. results().matches after k feeds is a prefix of results().matches after k+1 feeds. results_range(s,l) = results() with matches[min(s,n) .. min(s+l,n)] (n = result_count()) and the same incomplete/lowest/cut_short_lines.	src/budgeted.rs::partial_results_are_a_prefix_and_overfeed_is_a_noop, src/budgeted.rs::result_ranges_are_bounded_and_stable_across_eviction	0,1,2,3,5	Vec slice/clone, saturating min arithmetic
stream-start	contract	streaming::StreamingSearch::{new,with_config,start_search,state,scan_progress,results,current_index,total_matches}	new() and with_config(c) give state = Idle, scan_progress = −1, results = [], current_index = 0, total = 0. start_search(p,mode) = Err(EmptyPattern) if p = ""; Err(PatternTooLong) if |p| > max_pattern_len; Err(InvalidRegex) if mode = Regex and the compile fails; on Err the engine is unchanged. On Ok: pattern = p, mode, state = Searching, results = [], seen = ∅, current_index = 0, scan_progress = 0, total = 0, and in Regex mode the compiled regex is replaced.	src/streaming/tests/lifecycle.rs::new_search_is_idle, src/streaming/tests/lifecycle.rs::start_search_transitions_to_searching, src/streaming/tests/lifecycle.rs::empty_pattern_returns_error, src/streaming/tests/boundary.rs::pattern_too_long_returns_error, src/streaming/tests/boundary.rs::pattern_at_max_length_is_accepted	0,1,2,3,4	&mut self, String, Result, enum state
stream-row-matches	contract	streaming::StreamingSearch::{find_matches_in_row,literal_matches_in_row,fuzzy_matches_in_row,prepare_case_folded_inputs}, streaming::engine::matching::{literal_find_matches,resolve_columns,case_fold,MatchColumns::resolve}	Let CM = ColumnMap::new(t, Single), and fold = case_fold (identity, to_ascii_lowercase or lower_fold, chosen by lower_need) when ¬case_sensitive, the identity otherwise. Pattern "" gives []. Literal: [StreamingMatch(row,s,e) | p a char-boundary offset of fold(t) at which fold(t)[p..] starts with fold(P); (b0,b1) = (lowered ∧ ¬t.is_ascii()) ? (LBM_t(p), LBM_t(p+|fold P|)) : (p, p+|fold P|); (s,e) = CM.span(b0,b1); e > s], ascending. Regex: [StreamingMatch(row, CM.span(cap)) | cap ∈ scan_line(compiled,t), cap.start < cap.end, span non-empty], keeping the matches found before a budget cut. Fuzzy: [StreamingMatch(row, 0, display_columns(t))] if fuzzy_match(fold t, fold P), else [] (see §5.1: this arm lacks the e > s filter).	src/streaming/engine/matching.rs::lazy_coordinate_maps_match_reference_for_overlaps_and_unicode, src/streaming/engine/matching.rs::streaming_sigma_matches_all_forms_at_original_display_columns, src/streaming/engine/matching.rs::width_only_search_paths_preserve_unicode_match_columns, src/streaming/engine/matching.rs::literal_rows_report_display_column_spans, src/streaming/engine/matching.rs::test_find_matches_empty_pattern_returns_empty, src/streaming/engine/matching.rs::test_case_insensitive_scan_all, src/streaming/tests/boundary.rs::case_insensitive_match_extent_uses_lowered_length, src/streaming/tests/query.rs::case_insensitive_search, src/streaming/tests/query.rs::case_sensitive_search, src/streaming/tests/query.rs::fuzzy_match, src/streaming/tests/query.rs::fuzzy_no_match, src/streaming/tests/query.rs::multi_match_cjk_line_column_offsets_correct, src/streaming/tests/query.rs::case_insensitive_multi_match_casefold_byte_shrink, src/streaming/tests/utf8.rs::utf8_cjk_result_positions_valid, src/streaming/tests/utf8.rs::utf8_multibyte_pattern_positions_valid, src/streaming/tests/utf8.rs::utf8_emoji_no_duplicate_results, src/streaming/tests/utf8.rs::overlapping_ascii_matches_production_path, src/streaming/tests/utf8.rs::utf8_case_insensitive_mixed_content, src/streaming/tests/utf8.rs::utf8_multirow_all_invariants, src/streaming/tests/utf8.rs::utf8_literal_combining_mark_zero_display_width_filtered, src/streaming/tests/utf8.rs::utf8_case_insensitive_kelvin_sign_case_folding, src/streaming/tests/navigation.rs::current_match_returns_correct_match, src/streaming/regex_tests.rs::regex_basic_pattern, src/streaming/regex_tests.rs::regex_character_class, src/streaming/regex_tests.rs::regex_word_boundary, src/streaming/regex_tests.rs::regex_vs_literal_metacharacters, src/streaming/regex_tests.rs::regex_multiple_matches_per_row, src/streaming/regex_tests.rs::regex_multi_match_cjk_line_column_offsets_correct, src/streaming/regex_tests.rs::regex_alternation, src/streaming/regex_tests.rs::regex_quantifiers, src/streaming/regex_tests.rs::regex_empty_match_handling, src/streaming/regex_tests.rs::regex_zero_length_anchor_filtered, src/streaming/regex_tests.rs::regex_preserves_nonzero_matches_alongside_zero_length, src/streaming/regex_tests.rs::regex_combining_mark_zero_display_width_filtered, src/streaming/regex_tests.rs::regex_case_insensitive_default, src/streaming/regex_tests.rs::regex_case_sensitive_toggled_on	0,1,2,3,5,7,12	Cow<str>, lazy Option maps, std str::find, cross-crate regex
stream-lazy-coordinates	other (cost)	streaming::engine::matching::{MatchColumns::{new,resolve},literal_find_matches}	literal_find_matches builds MatchColumns.maps only if it finds at least one occurrence, so a row with no hit builds neither a ColumnMap nor a LowerByteMap. The LowerByteMap is built only when lowered ∧ ¬text.is_ascii() (ASCII lowering preserves offsets); control characters still go through ColumnMap.	src/streaming/engine/matching.rs::unmatched_rows_do_not_build_coordinate_maps, src/streaming/engine/matching.rs::ascii_lowercase_offsets_need_no_map_but_controls_keep_display_columns	0,1,2,5	Option::get_or_insert_with laziness, &mut borrow
stream-scan-row	contract	streaming::StreamingSearch::{scan_row,complete_search,record_seen_position,result_count}, scan_all (cfg(test); unwrapped rows only)	scan_row(row,text,max): if state ≠ Searching ∨ row ≠ scan_progress ∨ row ≥ max, it returns 0 and changes nothing. Otherwise let M = find_matches_in_row(row,text); for each m ∈ M in order: skip it if (m.row, m.start_col) ∈ seen; else if |results| ≥ max_results, total += 1; else push m, add its key to seen, total += 1. Then scan_progress = row+1. If row+1 ≥ max: state := results ≠ [] ? HasResults (current_index = 1) : NoResults (current_index = 0), and scan_progress = −1. Returns |M|. scan_all over content with no wrapped rows ≡ scan_row(i, row_i, n) for i = 0..n−1, then complete_search; n = 0 gives NoResults.	src/streaming/tests/scanning.rs::scan_finds_matches, src/streaming/tests/scanning.rs::scan_all_completes_search, src/streaming/tests/scanning.rs::no_matches_transitions_to_no_results, src/streaming/tests/scanning.rs::memory_bounded, src/streaming/tests/scanning.rs::no_duplicate_results, src/streaming/tests/scanning.rs::scan_progress_consistent, src/streaming/tests/scanning.rs::non_wrapped_rows_unaffected_by_wrapping_logic, src/streaming/tests/boundary.rs::scan_empty_content_transitions_to_no_results, src/streaming/tests/boundary.rs::scan_single_empty_row, src/streaming/tests/boundary.rs::scan_row_with_max_rows_zero_is_noop, src/streaming/engine/operations.rs::test_memory_bound_respected	0,1,2,3,5,7	HashSet<(usize,usize)>, isize/usize conversions, &mut self
stream-content-changes	contract	streaming::StreamingSearch::{content_added,content_invalidated,content_reflowed,cancel}	content_added(row,t) acts only in HasResults/NoResults: it inserts find_matches_in_row(row,t) under the seen/max_results/total rules of scan_row and re-sorts results by (row,start_col) when they grew; if results ≠ [], NoResults → HasResults and current_index 0 → 1. content_invalidated(a,b) acts only in HasResults/NoResults: it removes the results and seen keys with a ≤ row ≤ b and does total −= #removed stored; then results = [] ⇒ NoResults ∧ current_index = 0, else current_index := min(current_index, |results|). content_reflowed() restarts in Searching/HasResults/NoResults (Searching, results = [], seen = ∅, current = 0, progress = 0, total = 0) and is a no-op in Idle. cancel() in a non-Idle state goes to Idle with pattern "", results = [], seen = ∅, current = 0, progress = −1, total = 0 and the compiled regex dropped; a no-op in Idle.	src/streaming/tests/boundary.rs::content_added_finds_new_matches, src/streaming/tests/boundary.rs::content_invalidated_removes_matches, src/streaming/tests/boundary.rs::content_invalidated_single_row, src/streaming/tests/boundary.rs::content_invalidated_removes_all_matches, src/streaming/tests/boundary.rs::content_added_in_no_results_with_match_transitions, src/streaming/tests/boundary.rs::content_added_after_initial_match, src/streaming/tests/scanning.rs::content_added_in_no_results_transitions_to_has_results, src/streaming/tests/scanning.rs::content_added_in_no_results_no_match_stays_no_results, src/streaming/engine/operations.rs::test_cancel_noop_when_idle, src/streaming/engine/operations.rs::test_content_added_noop_when_idle, src/streaming/engine/operations.rs::test_content_invalidated_all_transitions_to_no_results, src/streaming/engine/operations.rs::test_content_invalidated_clamps_current_index, src/streaming/engine/operations.rs::test_content_reflowed_restarts_search, src/streaming/engine/operations.rs::test_content_reflowed_noop_when_idle, src/streaming/engine/operations.rs::test_no_duplicate_results_after_content_added, src/streaming/tests/lifecycle.rs::cancel_clears_state, src/streaming/regex_tests.rs::regex_cancel_clears_compiled	0,1,2,3,5,7	Vec retain/sort_unstable_by_key, HashSet retain, enum state
stream-incomplete	contract	streaming::StreamingSearch::{incomplete,scan_row}	incomplete() ⇔ total_matches > |results| ∨ (filter_mode = Regex ∧ compiled_regex.step_limit_exceeded()). The step flag is sticky for one compiled pattern across rows; start_search clears it with a fresh compile and cancel clears it by dropping the regex. A cut row contributes the matches its scan yielded before the cut.	src/streaming/engine/operations.rs::a_row_that_exhausts_the_budget_yields_no_matches, src/streaming/engine/operations.rs::a_result_cap_is_observable_for_literal_and_regex_search	0,1,2,3,12	Option<Regex> &self query, cross-crate sticky flag
stream-invariants	invariant	streaming::StreamingSearch (all production operations), engine::invariants::verify_* (cfg(test))	From new()/with_config(c) and after any sequence of start_search, scan_row, next_match, prev_match, content_added, content_invalidated, content_reflowed and cancel: INV-1 current_index = 0 ∨ current_index ≤ |results|; INV-2 ∀ m ∈ results: m.start_col < m.end_col ∧ m.match_len = m.end_col − m.start_col; INV-3 |results| ≤ max_results; INV-4 no two results share (row,start_col), and seen = their keys; INV-5 state = Searching ⇔ scan_progress ≥ 0 (otherwise −1); INV-6 total_matches ≥ |results|; and results are ascending by (row,start_col) once state ≠ Searching. INV-2 fails today for Fuzzy rows of zero display width (§5.1); the proof needs that fix first.	src/streaming/tests/scanning.rs::all_invariants_hold_after_operations, src/streaming/regex_tests.rs::regex_invariants_after_operations	0,1,2,5,7	quantified Vec/HashSet invariants, &mut self two-state reasoning
stream-refines-model	refinement	streaming::StreamingSearch::{start_search,scan_row,next_match,prev_match,content_added,content_invalidated,content_reflowed,cancel}, aterm_spec::derive::streaming_search_model	project(with_config{max_results = MaxResults, wrap = Wrap}) = Init. Every unit-effect engine call maps to the corresponding enabled action of streaming_search_model with project(e') = Next(project(e)). The unit-effect calls are: Start; scan_row adding exactly one or zero matches (last-row completion folded in); NextMatch; PrevMatch; content_added of one fresh match; content_invalidated removing exactly one stored match; Reflow; Cancel. Disabled model actions are engine no-ops. Calls with multi-match effects are outside the abstraction; stream-invariants covers them.	tests/conformance_streaming.rs::model_consts_match_driver, tests/conformance_streaming.rs::real_engine_conforms_to_spec_exhaustively, tests/conformance_streaming.rs::forged_observation_is_rejected, tests/conformance_streaming.rs::disabled_navigation_is_a_real_noop, tests/conformance_streaming.rs::at_capacity_hit_counts_but_does_not_store, tests/conformance_streaming.rs::multi_effect_calls_are_rejected_by_the_unit_effect_model	0,1,2,10	model-to-code refinement over enum/Vec state, ty_model! binding
```

## 2. Test → spec map (TSV; all 340)

`spec` is the spec that retires the test. A test in §4 has `-` here and its §4 action in
`action`. Rows are grouped by file.

```tsv
file	test	spec	action
src/bitmap.rs	full_history_eviction_work_is_amortized_and_prefix_storage_is_bounded	bitmap-eviction-cost	
src/bitmap.rs	prefix_eviction_preserves_mixed_operations_and_varint_boundaries	bitmap-canonical-encoding	
src/bitmap.rs	remove_last_is_o1_and_canonical	bitmap-canonical-encoding	
src/bitmap.rs	test_bitand	bitmap-set-refinement	
src/bitmap.rs	test_bitand_assign	bitmap-set-refinement	
src/bitmap.rs	test_bitand_disjoint_and_empty	bitmap-set-refinement	
src/bitmap.rs	test_clone	-	delete
src/bitmap.rs	test_contains	-	delete
src/bitmap.rs	test_default_is_empty	bitmap-set-refinement	
src/bitmap.rs	test_drop_below	bitmap-set-refinement	
src/bitmap.rs	test_drop_below_equals_repeated_front_remove	bitmap-set-refinement	
src/bitmap.rs	test_insert_and_len	bitmap-set-refinement	
src/bitmap.rs	test_insert_out_of_order_stays_sorted	bitmap-set-refinement	
src/bitmap.rs	test_insert_return_value	bitmap-set-refinement	
src/bitmap.rs	test_into_iter_sorted	bitmap-set-refinement	
src/bitmap.rs	test_iter_borrowed	-	delete
src/bitmap.rs	test_large_gap_varint_roundtrip	bitmap-set-refinement	
src/bitmap.rs	test_range_from_and_before	-	delete
src/bitmap.rs	test_range_query	-	delete
src/bitmap.rs	test_remove	bitmap-set-refinement	
src/bitmap.rs	test_remove_range	-	delete
src/bitmap.rs	test_single_value_has_no_delta_bytes	bitmap-canonical-encoding	
src/bloom_tests.rs	bloom_capacity_near_overflow_boundary	bloom-layout	
src/bloom_tests.rs	bloom_clear	bloom-clear-count-empty	
src/bloom_tests.rs	bloom_default	bloom-layout	
src/bloom_tests.rs	bloom_empty_string	bloom-no-false-negatives	
src/bloom_tests.rs	bloom_estimated_fpr	bloom-estimated-fpr	
src/bloom_tests.rs	bloom_false_positive_rate	-	move-to-bench
src/bloom_tests.rs	bloom_insert_and_query	bloom-no-false-negatives	
src/bloom_tests.rs	bloom_insert_bytes	bloom-no-false-negatives	
src/bloom_tests.rs	bloom_insert_constant_time	bloom-probe-cost	
src/bloom_tests.rs	bloom_large_capacity_no_overflow	bloom-layout	
src/bloom_tests.rs	bloom_lookup_constant_time	bloom-probe-cost	
src/bloom_tests.rs	bloom_lookup_early_return_on_miss	-	move-to-bench
src/bloom_tests.rs	bloom_no_false_negatives	bloom-no-false-negatives	
src/bloom_tests.rs	bloom_with_size	bloom-layout	
src/bloom_tests.rs	bloom_with_size_max_no_overflow	bloom-layout	
src/bloom_tests.rs	bloom_with_size_rounding	bloom-layout	
src/bloom_tests.rs	bloom_zero_capacity	bloom-layout	
src/budgeted.rs	a_row_the_regex_scan_cannot_finish_is_reported_like_the_one_shot	budgeted-refines-one-shot	
src/budgeted.rs	any_slicing_equals_one_shot_across_all_modes	budgeted-refines-one-shot	
src/budgeted.rs	combined_match_cap_and_eviction_equals_one_shot	budgeted-refines-one-shot	
src/budgeted.rs	eviction_matches_one_shot_capped_index	budgeted-refines-one-shot	
src/budgeted.rs	final_evicted_prefix_matches_real_index_schedule	eviction-schedule	
src/budgeted.rs	invalid_regex_fails_at_construction	regex-compile	
src/budgeted.rs	match_cap_matches_one_shot_and_reports_incomplete	budgeted-refines-one-shot	
src/budgeted.rs	owned_feed_matches_borrowed_and_batch_unicode_coordinates	budgeted-refines-one-shot	
src/budgeted.rs	partial_results_are_a_prefix_and_overfeed_is_a_noop	budgeted-progressive	
src/budgeted.rs	result_ranges_are_bounded_and_stable_across_eviction	budgeted-progressive	
src/bytesearch.rs	a_one_byte_after_a_zero_byte_does_not_shift_the_answer	memchr-family	
src/bytesearch.rs	a_reused_searcher_answers_the_same_as_a_fresh_one	searcher-find-rfind	
src/bytesearch.rs	a_useless_skip_switches_itself_off_and_the_answer_is_unchanged	searcher-linear-time	
src/bytesearch.rs	a_warm_searcher_matches_the_reference	searcher-find-rfind	
src/bytesearch.rs	byte_scanning_matches_the_reference	memchr-family	
src/bytesearch.rs	empty_needle_and_empty_haystack	searcher-find-rfind	
src/bytesearch.rs	every_byte_value_is_findable	memchr-family	
src/bytesearch.rs	exhaustive_binary_alphabet	searcher-find-rfind	
src/bytesearch.rs	exhaustive_ternary_alphabet	searcher-find-rfind	
src/bytesearch.rs	named_edges_match_the_reference	searcher-find-rfind	
src/bytesearch.rs	needle_longer_than_haystack	searcher-find-rfind	
src/bytesearch.rs	non_ascii_bytes_are_just_bytes	searcher-find-rfind	
src/bytesearch.rs	overlapping_matches_are_all_reachable	searcher-find-rfind	
src/bytesearch.rs	periodic_needles_match_the_reference	searcher-find-rfind	
src/bytesearch.rs	realistic_haystacks_match_the_reference	searcher-find-rfind	
src/bytesearch.rs	repository_corpus_matches_the_reference	searcher-find-rfind	
src/bytesearch.rs	word_boundaries_are_covered	memchr-family	
src/cells.rs	a_column_names_the_cell_drawn_over_it	line-cells-column-queries	
src/cells.rs	a_column_range_takes_the_cells_drawn_over_it	line-cells-column-queries	
src/cells.rs	a_wrapped_row_joins_at_its_stride	line-cells-model	
src/cells.rs	appended_cells_are_shifted_and_merged	line-cells-model	
src/cells.rs	cells_read_back_as_pushed_in_runs_of_one_shape	line-cells-model	
src/cells.rs	empty_and_overlapping_cells_are_not_recorded	line-cells-model	
src/grapheme.rs	a_line_of_one_shape_is_one_run	column-map-runs-compact	
src/grapheme.rs	a_map_is_no_larger_than_before	-	delete
src/grapheme.rs	cells_of_a_line_written_in_one_go_answer_as_the_walk	column-map-with-cells	
src/grapheme.rs	cells_the_grid_kept_apart_are_where_matches_are_drawn	column-map-with-cells	
src/grapheme.rs	column_map_empty_string	column-map-walk	
src/grapheme.rs	column_map_long_line_all_offsets	column-map-walk	
src/grapheme.rs	column_map_matches_byte_to_column_ascii	column-map-walk	
src/grapheme.rs	column_map_matches_byte_to_column_cjk	column-map-walk	
src/grapheme.rs	column_map_matches_byte_to_column_mixed_unicode	column-map-walk	
src/grapheme.rs	graphemes_inside_a_cell_and_bytes_outside_every_cell	column-map-with-cells	
src/grapheme.rs	lower_byte_map_long_line_all_offsets	lower-byte-map	
src/grapheme.rs	lower_byte_map_matches_ascii	lower-byte-map	
src/grapheme.rs	lower_byte_map_matches_kelvin_sign	lower-byte-map	
src/grapheme.rs	lower_byte_map_matches_mixed_unicode	lower-byte-map	
src/grapheme.rs	the_runs_answer_as_the_per_grapheme_map_did	column-map-walk	
src/grapheme.rs	width_only_matches_coordinate_map_for_unicode_and_controls	display-columns	
src/index.rs	a_pattern_that_cannot_finish_a_line_is_reported_never_passed_off	index-regex-batch	
src/index.rs	ordinary_patterns_over_a_long_joined_line_find_their_matches	-	move-to-bench
src/index.rs	ordinary_regex_queries_are_unaffected	-	move-to-bench
src/index.rs	the_compiled_pattern_carries_the_scan_budget	regex-compile	
src/index.rs	the_line_budget_scales_with_the_line_under_a_ceiling	regex-line-budget	
src/index.rs	the_pathological_pattern_over_a_long_line_stops_at_the_ceiling	index-regex-batch	
src/index_bloom_tests.rs	dictionary_growth_rebuilds_without_losing_live_trigrams	index-bloom-dictionary	
src/index_bloom_tests.rs	repeated_output_and_unchanged_refreshes_do_not_grow_the_filter	index-bloom-dictionary	
src/index_bloom_tests.rs	replacing_last_posting_and_reintroducing_it_preserves_all_search_modes	index-observational-equivalence	
src/index_key_window_tests.rs	a_descending_run_of_lines_moves_the_window_once	index-model	
src/index_key_window_tests.rs	a_frame_survives_the_index_rekeying_under_it	narrowed-search	
src/index_key_window_tests.rs	an_index_kept_across_four_billion_rows_rekeys_and_stays_complete	index-model	
src/index_key_window_tests.rs	lines_that_cannot_share_the_newest_lines_window_are_reported_evicted	index-model	
src/index_key_window_tests.rs	rows_past_u32_max_are_found_at_their_own_line_numbers	index-literal-cs	
src/iterators.rs	forward_literal_matches_equal_a_str_find_reference	index-literal-cs	
src/literal.rs	finds_single_byte_in_both_cases	ascii-ci-matches	
src/literal.rs	finds_two_byte_and_overlapping_matches	ascii-ci-matches	
src/literal.rs	handles_empty_long_and_missing_needles	ascii-ci-matches	
src/streaming/engine/matching.rs	ascii_lowercase_offsets_need_no_map_but_controls_keep_display_columns	stream-lazy-coordinates	
src/streaming/engine/matching.rs	fuzzy_match_rows	fuzzy-subsequence	
src/streaming/engine/matching.rs	lazy_coordinate_maps_match_reference_for_overlaps_and_unicode	stream-row-matches	
src/streaming/engine/matching.rs	literal_rows_report_display_column_spans	stream-row-matches	
src/streaming/engine/matching.rs	streaming_sigma_matches_all_forms_at_original_display_columns	stream-row-matches	
src/streaming/engine/matching.rs	test_case_insensitive_scan_all	stream-row-matches	
src/streaming/engine/matching.rs	test_empty_pattern_returns_no_matches	-	delete
src/streaming/engine/matching.rs	test_find_matches_empty_pattern_returns_empty	stream-row-matches	
src/streaming/engine/matching.rs	test_fuzzy_match_subsequence	-	delete
src/streaming/engine/matching.rs	test_fuzzy_no_match	-	delete
src/streaming/engine/matching.rs	unmatched_rows_do_not_build_coordinate_maps	stream-lazy-coordinates	
src/streaming/engine/matching.rs	width_only_search_paths_preserve_unicode_match_columns	stream-row-matches	
src/streaming/engine/operations.rs	a_long_logical_line_uses_the_same_budget_as_indexed_search	-	move-to-bench
src/streaming/engine/operations.rs	a_result_cap_is_observable_for_literal_and_regex_search	stream-incomplete	
src/streaming/engine/operations.rs	a_row_that_exhausts_the_budget_yields_no_matches	stream-incomplete	
src/streaming/engine/operations.rs	ordinary_patterns_scan_rows_normally	-	move-to-bench
src/streaming/engine/operations.rs	test_cancel_noop_when_idle	stream-content-changes	
src/streaming/engine/operations.rs	test_content_added_noop_when_idle	stream-content-changes	
src/streaming/engine/operations.rs	test_content_invalidated_all_transitions_to_no_results	stream-content-changes	
src/streaming/engine/operations.rs	test_content_invalidated_clamps_current_index	stream-content-changes	
src/streaming/engine/operations.rs	test_content_reflowed_noop_when_idle	stream-content-changes	
src/streaming/engine/operations.rs	test_content_reflowed_restarts_search	stream-content-changes	
src/streaming/engine/operations.rs	test_memory_bound_respected	stream-scan-row	
src/streaming/engine/operations.rs	test_next_index_backward_no_wrap	stream-navigation	
src/streaming/engine/operations.rs	test_next_index_backward_wrap	stream-navigation	
src/streaming/engine/operations.rs	test_next_index_empty_len_returns_zero	stream-navigation	
src/streaming/engine/operations.rs	test_next_index_forward_no_wrap	stream-navigation	
src/streaming/engine/operations.rs	test_next_index_forward_wrap	stream-navigation	
src/streaming/engine/operations.rs	test_next_match_noop_when_no_results	stream-navigation	
src/streaming/engine/operations.rs	test_no_duplicate_results_after_content_added	stream-content-changes	
src/streaming/engine/operations.rs	test_prev_match_noop_when_idle	stream-navigation	
src/streaming/engine/operations.rs	test_update_pattern_same_is_noop	-	delete
src/streaming/engine/operations.rs	test_update_pattern_to_empty_resets_to_idle	-	delete
src/streaming/engine/operations.rs	the_streaming_engine_compiles_with_the_scan_budget	regex-compile	
src/streaming/regex_tests.rs	regex_alternation	stream-row-matches	
src/streaming/regex_tests.rs	regex_basic_pattern	stream-row-matches	
src/streaming/regex_tests.rs	regex_cancel_clears_compiled	stream-content-changes	
src/streaming/regex_tests.rs	regex_case_insensitive_default	stream-row-matches	
src/streaming/regex_tests.rs	regex_case_sensitive_toggled_on	stream-row-matches	
src/streaming/regex_tests.rs	regex_character_class	stream-row-matches	
src/streaming/regex_tests.rs	regex_combining_mark_zero_display_width_filtered	stream-row-matches	
src/streaming/regex_tests.rs	regex_empty_match_handling	stream-row-matches	
src/streaming/regex_tests.rs	regex_invalid_pattern	regex-compile	
src/streaming/regex_tests.rs	regex_invariants_after_operations	stream-invariants	
src/streaming/regex_tests.rs	regex_multi_match_cjk_line_column_offsets_correct	stream-row-matches	
src/streaming/regex_tests.rs	regex_multiple_matches_per_row	stream-row-matches	
src/streaming/regex_tests.rs	regex_preserves_nonzero_matches_alongside_zero_length	stream-row-matches	
src/streaming/regex_tests.rs	regex_quantifiers	stream-row-matches	
src/streaming/regex_tests.rs	regex_set_filter_mode_invalid_regex_error	-	delete
src/streaming/regex_tests.rs	regex_set_filter_mode_switch	-	delete
src/streaming/regex_tests.rs	regex_update_pattern	-	delete
src/streaming/regex_tests.rs	regex_update_to_invalid	-	delete
src/streaming/regex_tests.rs	regex_vs_literal_metacharacters	stream-row-matches	
src/streaming/regex_tests.rs	regex_word_boundary	stream-row-matches	
src/streaming/regex_tests.rs	regex_zero_length_anchor_filtered	stream-row-matches	
src/streaming/tests/boundary.rs	case_insensitive_match_extent_uses_lowered_length	stream-row-matches	
src/streaming/tests/boundary.rs	content_added_after_initial_match	stream-content-changes	
src/streaming/tests/boundary.rs	content_added_finds_new_matches	stream-content-changes	
src/streaming/tests/boundary.rs	content_added_in_no_results_with_match_transitions	stream-content-changes	
src/streaming/tests/boundary.rs	content_invalidated_removes_all_matches	stream-content-changes	
src/streaming/tests/boundary.rs	content_invalidated_removes_matches	stream-content-changes	
src/streaming/tests/boundary.rs	content_invalidated_single_row	stream-content-changes	
src/streaming/tests/boundary.rs	navigation_single_match_next_stays	stream-navigation	
src/streaming/tests/boundary.rs	pattern_at_max_length_is_accepted	stream-start	
src/streaming/tests/boundary.rs	pattern_too_long_returns_error	stream-start	
src/streaming/tests/boundary.rs	scan_empty_content_transitions_to_no_results	stream-scan-row	
src/streaming/tests/boundary.rs	scan_row_with_max_rows_zero_is_noop	stream-scan-row	
src/streaming/tests/boundary.rs	scan_single_empty_row	stream-scan-row	
src/streaming/tests/lifecycle.rs	cancel_clears_state	stream-content-changes	
src/streaming/tests/lifecycle.rs	clear_pattern_returns_to_idle	-	delete
src/streaming/tests/lifecycle.rs	config_getters_reflect_defaults_and_custom_values	-	delete
src/streaming/tests/lifecycle.rs	empty_pattern_returns_error	stream-start	
src/streaming/tests/lifecycle.rs	new_search_is_idle	stream-start	
src/streaming/tests/lifecycle.rs	start_search_transitions_to_searching	stream-start	
src/streaming/tests/navigation.rs	current_match_returns_correct_match	stream-row-matches	
src/streaming/tests/navigation.rs	navigation_without_wrap	stream-navigation	
src/streaming/tests/navigation.rs	navigation_works	stream-navigation	
src/streaming/tests/navigation.rs	toggle_highlight_all_flips_setting	-	delete
src/streaming/tests/navigation.rs	toggle_wrap_flips_setting_and_changes_navigation_behavior	-	delete
src/streaming/tests/query.rs	case_insensitive_multi_match_casefold_byte_shrink	stream-row-matches	
src/streaming/tests/query.rs	case_insensitive_search	stream-row-matches	
src/streaming/tests/query.rs	case_sensitive_search	stream-row-matches	
src/streaming/tests/query.rs	fuzzy_match	stream-row-matches	
src/streaming/tests/query.rs	fuzzy_no_match	stream-row-matches	
src/streaming/tests/query.rs	multi_match_cjk_line_column_offsets_correct	stream-row-matches	
src/streaming/tests/query.rs	set_filter_mode_empty_pattern_no_restart	-	delete
src/streaming/tests/query.rs	set_filter_mode_idle_with_pattern_no_restart	-	delete
src/streaming/tests/query.rs	set_filter_mode_restarts_cleanly_during_long_scan	-	delete
src/streaming/tests/query.rs	set_filter_mode_restarts_while_scanning	-	delete
src/streaming/tests/query.rs	set_filter_mode_same_mode_is_noop	-	delete
src/streaming/tests/query.rs	toggle_case_sensitive_restarts_cleanly_during_long_scan	-	delete
src/streaming/tests/query.rs	toggle_case_sensitive_restarts_search	-	delete
src/streaming/tests/query.rs	toggle_case_sensitive_restarts_while_scanning	-	delete
src/streaming/tests/query.rs	update_pattern_idle_returns_invalid_state	-	delete
src/streaming/tests/query.rs	update_pattern_restarts_search	-	delete
src/streaming/tests/query.rs	update_pattern_same_pattern_is_noop	-	delete
src/streaming/tests/query.rs	update_pattern_too_long_returns_error	-	delete
src/streaming/tests/scanning.rs	all_invariants_hold_after_operations	stream-invariants	
src/streaming/tests/scanning.rs	content_added_in_no_results_no_match_stays_no_results	stream-content-changes	
src/streaming/tests/scanning.rs	content_added_in_no_results_transitions_to_has_results	stream-content-changes	
src/streaming/tests/scanning.rs	content_modified_after_scan_complete	-	delete
src/streaming/tests/scanning.rs	content_modified_bumps_generation	-	delete
src/streaming/tests/scanning.rs	content_modified_clamps_current_index	-	delete
src/streaming/tests/scanning.rs	content_modified_in_no_results_transitions_to_has_results	-	delete
src/streaming/tests/scanning.rs	content_modified_noop_for_unscanned_row	-	delete
src/streaming/tests/scanning.rs	content_modified_noop_in_idle	-	delete
src/streaming/tests/scanning.rs	content_modified_removes_all_matches_transitions_to_no_results	-	delete
src/streaming/tests/scanning.rs	content_modified_removes_stale_matches_during_scan	-	delete
src/streaming/tests/scanning.rs	content_modified_replaces_with_new_matches	-	delete
src/streaming/tests/scanning.rs	memory_bounded	stream-scan-row	
src/streaming/tests/scanning.rs	no_duplicate_results	stream-scan-row	
src/streaming/tests/scanning.rs	no_matches_transitions_to_no_results	stream-scan-row	
src/streaming/tests/scanning.rs	non_wrapped_rows_unaffected_by_wrapping_logic	stream-scan-row	
src/streaming/tests/scanning.rs	scan_all_completes_search	stream-scan-row	
src/streaming/tests/scanning.rs	scan_finds_matches	stream-scan-row	
src/streaming/tests/scanning.rs	scan_progress_consistent	stream-scan-row	
src/streaming/tests/scanning.rs	streaming_generation_bumps_on_cancel	-	becomes-spec-after-fix
src/streaming/tests/scanning.rs	streaming_generation_bumps_on_content_added_with_match	-	becomes-spec-after-fix
src/streaming/tests/scanning.rs	streaming_generation_bumps_on_content_invalidated	-	becomes-spec-after-fix
src/streaming/tests/scanning.rs	streaming_generation_bumps_on_start_search	-	becomes-spec-after-fix
src/streaming/tests/scanning.rs	streaming_generation_bumps_on_update_pattern	-	delete
src/streaming/tests/scanning.rs	streaming_generation_stable_on_content_added_no_match	-	becomes-spec-after-fix
src/streaming/tests/scanning.rs	streaming_generation_stable_on_navigation	-	becomes-spec-after-fix
src/streaming/tests/scanning.rs	streaming_generation_stable_on_noop_invalidation	-	becomes-spec-after-fix
src/streaming/tests/scanning.rs	streaming_generation_starts_at_zero	-	becomes-spec-after-fix
src/streaming/tests/scanning.rs	wrapped_line_match_coordinates_within_physical_width	-	delete
src/streaming/tests/scanning.rs	wrapped_line_match_spanning_boundary_is_clamped	-	delete
src/streaming/tests/utf8.rs	overlapping_ascii_matches_production_path	stream-row-matches	
src/streaming/tests/utf8.rs	utf8_case_insensitive_kelvin_sign_case_folding	stream-row-matches	
src/streaming/tests/utf8.rs	utf8_case_insensitive_mixed_content	stream-row-matches	
src/streaming/tests/utf8.rs	utf8_cjk_result_positions_valid	stream-row-matches	
src/streaming/tests/utf8.rs	utf8_emoji_no_duplicate_results	stream-row-matches	
src/streaming/tests/utf8.rs	utf8_literal_combining_mark_zero_display_width_filtered	stream-row-matches	
src/streaming/tests/utf8.rs	utf8_multibyte_pattern_positions_valid	stream-row-matches	
src/streaming/tests/utf8.rs	utf8_multirow_all_invariants	stream-row-matches	
src/tests.rs	batch_search_caps_matches_and_flags_incomplete	index-batch-cap	
src/tests.rs	bloom_filter_rejection	index-posting-invariant	
src/tests.rs	bloom_rebuild_tracks_trigram_volume_for_bulk_scrollback	index-bloom-dictionary	
src/tests.rs	cap_is_configurable_and_clear_resets_signal	index-model	
src/tests.rs	capped_narrowed_walk_matches_forward_batch_and_yields_no_frame	narrowed-search	
src/tests.rs	case_insensitive_100k_ascii_short_query_work_is_bounded	index-candidate-cost	
src/tests.rs	case_insensitive_ascii_search	index-literal-ci	
src/tests.rs	case_insensitive_ascii_search_reports_overlapping_matches	index-literal-ci	
src/tests.rs	case_insensitive_greek_final_sigma_unified	index-literal-ci	
src/tests.rs	case_insensitive_navigation_visits_only_trigram_candidates	index-candidate-cost	
src/tests.rs	case_insensitive_non_ascii_search	index-literal-ci	
src/tests.rs	case_insensitive_overlapping_matches	index-literal-ci	
src/tests.rs	case_insensitive_repeated_prefix_uses_linear_matcher_path	index-candidate-cost	
src/tests.rs	case_insensitive_search_uses_trigram_index	index-literal-ci	
src/tests.rs	case_insensitive_short_query_skips_sparse_empty_prefix	index-candidate-cost	
src/tests.rs	case_sensitive_point_navigation_builds_no_owned_intersection	index-candidate-cost	
src/tests.rs	case_sensitive_reverse_short_query_skips_sparse_empty_prefix	index-candidate-cost	
src/tests.rs	changed_row_reindex_matches_from_scratch_rebuild	index-observational-equivalence	
src/tests.rs	changed_row_reindex_round_trip_is_identity	index-observational-equivalence	
src/tests.rs	drop_history_below_is_a_noop_at_or_below_the_first_cached_line	index-model	
src/tests.rs	drop_history_below_matches_fresh_rebuild_including_completeness	index-model	
src/tests.rs	empty_query	index-posting-invariant	
src/tests.rs	empty_query_search_with_positions	index-literal-cs	
src/tests.rs	empty_query_terminal_search	index-literal-cs	
src/tests.rs	eviction_sets_incomplete_signal_and_watermark	index-model	
src/tests.rs	eviction_with_sparse_line_numbers	index-model	
src/tests.rs	find_next_optimized	index-point-find	
src/tests.rs	find_next_past_last_line	index-point-find	
src/tests.rs	find_next_prev_same_line_column_boundary	index-point-find	
src/tests.rs	find_next_range_query_skips_prefix_matches	index-candidate-cost	
src/tests.rs	find_prev_boundary_at_first_line	index-point-find	
src/tests.rs	find_prev_optimized	index-point-find	
src/tests.rs	find_prev_usize_max_does_not_overflow	index-point-find	
src/tests.rs	frame_reverifies_its_own_query_after_backspace	narrowed-search	
src/tests.rs	generation_bumps_on_clear	-	becomes-spec-after-fix
src/tests.rs	generation_bumps_on_index_scrollback_line	-	becomes-spec-after-fix
src/tests.rs	generation_bumps_on_index_scrollback_lines_batch	-	becomes-spec-after-fix
src/tests.rs	generation_bumps_on_index_visible_content	-	becomes-spec-after-fix
src/tests.rs	generation_bumps_on_invalidate	-	becomes-spec-after-fix
src/tests.rs	generation_counter_starts_at_zero	-	becomes-spec-after-fix
src/tests.rs	generation_detects_stale_results	-	becomes-spec-after-fix
src/tests.rs	generation_stable_across_read_operations	-	becomes-spec-after-fix
src/tests.rs	get_line	-	delete
src/tests.rs	index_and_search	index-posting-invariant	
src/tests.rs	index_clear	index-model	
src/tests.rs	len_is_a_high_water_mark	index-model	
src/tests.rs	literal_candidate_bound_reports_the_engines_own_candidate_count	literal-candidate-bound	
src/tests.rs	lowercase_fast_path_keeps_insert_remove_symmetric	index-observational-equivalence	
src/tests.rs	max_cached_for_retained_protects_the_newest_lines	eviction-schedule	
src/tests.rs	narrowed_typing_equals_batch_for_every_prefix_step	narrowed-search	
src/tests.rs	narrowed_walk_carries_eviction_honesty_like_batch	narrowed-search	
src/tests.rs	no_matches	index-posting-invariant	
src/tests.rs	occurrence_frame_keeps_zero_width_only_lines_narrable	narrowed-search	
src/tests.rs	owned_numbered_content_matches_borrowed_indexing	index-observational-equivalence	
src/tests.rs	push_line	index-model	
src/tests.rs	regex_batch_search_caps_matches_deterministically	index-batch-cap	
src/tests.rs	regex_compilation_size_limit_enforced	regex-compile	
src/tests.rs	regex_pattern_too_long_rejected	regex-compile	
src/tests.rs	regex_search_filters_zero_length_matches	index-regex-batch	
src/tests.rs	regex_search_preserves_nonzero_matches_alongside_zero_length	index-regex-batch	
src/tests.rs	reindex_line	index-posting-invariant	
src/tests.rs	search_10k_lines_common_char	index-literal-cs	
src/tests.rs	search_before_line_basic	index-literal-cs	
src/tests.rs	search_before_line_descending_order_at_scale	index-literal-cs	
src/tests.rs	search_before_line_empty_query	index-literal-cs	
src/tests.rs	search_before_line_range_bounded	index-literal-cs	
src/tests.rs	search_before_line_zero_returns_empty	index-literal-cs	
src/tests.rs	search_from_line_basic	index-literal-cs	
src/tests.rs	search_from_line_empty_query	index-literal-cs	
src/tests.rs	search_from_line_no_matches	index-literal-cs	
src/tests.rs	search_from_line_short_query	index-literal-cs	
src/tests.rs	search_index_with_capacity	index-posting-invariant	
src/tests.rs	search_iterator_early_termination	index-literal-cs	
src/tests.rs	search_iterator_multibyte_no_panic	index-literal-cs	
src/tests.rs	search_match_helpers	search-result-types	
src/tests.rs	search_match_iterator_multiple_matches_per_line	index-literal-cs	
src/tests.rs	search_match_struct	-	delete
src/tests.rs	search_multibyte_query_no_panic	index-literal-cs	
src/tests.rs	search_ordered	-	delete
src/tests.rs	search_reverse_iterator_multiple_matches_per_line	index-literal-cs	
src/tests.rs	search_single_multibyte_char_repeated	index-literal-cs	
src/tests.rs	search_with_positions	index-literal-cs	
src/tests.rs	single_line_overlaps_are_streamed_under_the_batch_cap	index-batch-cap	
src/tests.rs	terminal_search_basic	index-literal-cs	
src/tests.rs	terminal_search_find_next_prev	index-point-find	
src/tests.rs	terminal_search_index_scrollback_lines_batch	index-literal-cs	
src/tests.rs	terminal_search_index_visible_content	index-literal-cs	
src/tests.rs	terminal_search_option_navigation_is_ordered_and_case_insensitive	index-point-find	
src/tests.rs	terminal_search_surfaces_eviction_signal	index-model	
src/tests.rs	zero_display_width_matches_are_skipped	index-literal-cs	
tests/conformance_streaming.rs	at_capacity_hit_counts_but_does_not_store	stream-refines-model	
tests/conformance_streaming.rs	disabled_navigation_is_a_real_noop	stream-refines-model	
tests/conformance_streaming.rs	forged_observation_is_rejected	stream-refines-model	
tests/conformance_streaming.rs	model_consts_match_driver	stream-refines-model	
tests/conformance_streaming.rs	multi_effect_calls_are_rejected_by_the_unit_effect_model	stream-refines-model	
tests/conformance_streaming.rs	real_engine_conforms_to_spec_exhaustively	stream-refines-model	
tests/index_column_map_memory.rs	an_index_of_non_ascii_rows_holds_a_small_multiple_of_their_text	-	move-to-bench
tests/index_release_memory.rs	release_reclaims_real_heap_that_clear_retains_without_disturbing_siblings	-	move-to-bench
tests/search_vs_bruteforce.rs	verified_search_matches_bruteforce	index-literal-cs	
```

## 3. Coverage

- 340 tests read. 269 map to one of the 48 specs. 71 are not spec samples (§4): 47 delete, 16 becomes-spec-after-fix, 8 move-to-bench.
- The biggest collapses: `stream-row-matches` (36), `index-literal-cs` (26), `stream-content-changes` (17), `searcher-find-rfind` (12), `bitmap-set-refinement` (12), `index-model` (12).
- `lower-fold` retires no test by itself; it is the lemma under every case-insensitive spec. `index-observational-equivalence` is a corollary of `index-model` and the query specs.
- Specs blocked by each audit §7 gap (a spec counts once per gap it lists):

| gap | specs blocked |
|---|---|
| 0 | 48 |
| 1 | 43 |
| 2 | 32 |
| 3 | 32 |
| 4 | 11 |
| 5 | 35 |
| 6 | 2 |
| 7 | 36 |
| 10 | 1 |
| 11 | 2 |
| 12 | 11 |

## 4. Tests that are not spec samples

> Status 2026-10-15: 45 of the rows below marked **delete** have been deleted, with the
> `#[cfg(test)]`-only `SparseBitmap::remove_range`, `range_before` and `range` they alone used.
> Two **delete** rows were not confirmed and remain: `src/grapheme.rs::a_map_is_no_larger_than_before`
> and `src/streaming/tests/lifecycle.rs::config_getters_reflect_defaults_and_custom_values`.
> Rows marked `move-to-bench` or `becomes-spec-after-fix` remain.

`delete` means the test samples no spec (usually it tests `#[cfg(test)]`-only code with no
production caller); delete it now, together with that code. `move-to-bench` means it asserts a
statistical, latency or heap budget: a measurement, not a universal property, so it belongs in
`aterm-bench`. `becomes-spec-after-fix` means it pins state production cannot observe; fix the
code (expose the accessor, or remove the field) and it becomes a spec or goes away (§5.5).

| test | why | action |
|---|---|---|
| src/bitmap.rs::test_remove_range | Tests the `#[cfg(test)]`-only `SparseBitmap::remove_range`, which no shipping path calls. | **delete** (with the helper) |
| src/bitmap.rs::test_range_query | Tests the `#[cfg(test)]`-only `SparseBitmap::range`. | **delete** |
| src/bitmap.rs::test_range_from_and_before | Tests the `#[cfg(test)]`-only `range_from`/`range_before`. | **delete** |
| src/bitmap.rs::test_contains | Tests the `#[cfg(test)]`-only `SparseBitmap::contains`. | **delete** |
| src/bitmap.rs::test_clone | `derive(Clone)` tautology: a clone has the same length. | **delete** |
| src/bitmap.rs::test_iter_borrowed | `iter()` is the `#[cfg(test)]` alias `to_vec().into_iter()`. "Still usable after borrow" is the borrow checker's guarantee, and the order check duplicates `test_into_iter_sorted`. | **delete** |
| src/bloom_tests.rs::bloom_false_positive_rate | A statistical false-positive ratio over a hash. A Bloom filter permits any false positive, so there is no universal property. | **move-to-bench** |
| src/bloom_tests.rs::bloom_lookup_early_return_on_miss | Statistical average probe count and FPR ratio on a sparse filter. The deterministic part (stop at the first zero bit) is `bloom-probe-cost`. | **move-to-bench** |
| src/grapheme.rs::a_map_is_no_larger_than_before | `size_of::<ColumnMap>() ≤ 32` is a compile-time fact. Replace it with a const assertion. | **delete** |
| src/tests.rs::search_match_struct | Tautology: `SearchMatch::new` stores its three arguments. | **delete** |
| src/tests.rs::get_line | Tests the `#[cfg(test)]`-only `SearchIndex::get_line`. | **delete** |
| src/tests.rs::search_ordered | Tests the `#[cfg(test)]`-only `SearchIndex::search_ordered`. Forward order is `index-literal-cs`. | **delete** |
| src/tests.rs::generation_counter_starts_at_zero | `TerminalSearch::generation()` is `#[cfg(test)]`: production bumps a counter nothing reads. | **becomes-spec-after-fix** |
| src/tests.rs::generation_bumps_on_index_scrollback_line | Pins the unobservable `TerminalSearch` generation counter. | **becomes-spec-after-fix** |
| src/tests.rs::generation_bumps_on_index_scrollback_lines_batch | Same, through the `#[cfg(test)]` `index_scrollback_lines`. | **becomes-spec-after-fix** |
| src/tests.rs::generation_bumps_on_index_visible_content | Same counter. | **becomes-spec-after-fix** |
| src/tests.rs::generation_bumps_on_clear | Same counter. | **becomes-spec-after-fix** |
| src/tests.rs::generation_bumps_on_invalidate | Same counter, through the `#[cfg(test)]` `invalidate()`. | **becomes-spec-after-fix** |
| src/tests.rs::generation_detects_stale_results | Same counter. | **becomes-spec-after-fix** |
| src/tests.rs::generation_stable_across_read_operations | Same counter, through the `#[cfg(test)]` `find_next`/`find_prev`/`might_contain`. | **becomes-spec-after-fix** |
| src/index.rs::ordinary_patterns_over_a_long_joined_line_find_their_matches | Capacity check that aterm-regex step costs for chosen patterns over 100 KB–4.3 MB lines stay under the budget: a performance property of another crate. | **move-to-bench** |
| src/index.rs::ordinary_regex_queries_are_unaffected | Checks that ordinary patterns are never cut short; depends on aterm-regex's cost model. Its match-line assertions are already sampled by `index-regex-batch` tests. | **move-to-bench** |
| src/streaming/engine/operations.rs::a_long_logical_line_uses_the_same_budget_as_indexed_search | Capacity check that an ordinary pattern fits the per-line budget over a 1.1 MB line (aterm-regex cost). | **move-to-bench** |
| src/streaming/engine/operations.rs::ordinary_patterns_scan_rows_normally | Capacity check that an ordinary pattern fits the per-row budget (aterm-regex cost). | **move-to-bench** |
| src/streaming/engine/operations.rs::test_update_pattern_to_empty_resets_to_idle | Tests the `#[cfg(test)]`-only `StreamingSearch::update_pattern`, which has no production caller. | **delete** |
| src/streaming/engine/operations.rs::test_update_pattern_same_is_noop | Tests the `#[cfg(test)]`-only `update_pattern` and `generation()`. | **delete** |
| src/streaming/engine/matching.rs::test_empty_pattern_returns_no_matches | Duplicate of `src/streaming/tests/lifecycle.rs::empty_pattern_returns_error`. | **delete** |
| src/streaming/engine/matching.rs::test_fuzzy_match_subsequence | Duplicate of `src/streaming/tests/query.rs::fuzzy_match`. | **delete** |
| src/streaming/engine/matching.rs::test_fuzzy_no_match | Duplicate of `src/streaming/tests/query.rs::fuzzy_no_match`. | **delete** |
| src/streaming/tests/lifecycle.rs::config_getters_reflect_defaults_and_custom_values | Tests `#[cfg(test)]`-only config getters that echo a struct literal. | **delete** |
| src/streaming/tests/lifecycle.rs::clear_pattern_returns_to_idle | Tests the `#[cfg(test)]`-only `update_pattern`. | **delete** |
| src/streaming/tests/navigation.rs::toggle_wrap_flips_setting_and_changes_navigation_behavior | Tests the `#[cfg(test)]`-only `toggle_wrap`/`wrap_enabled`. Wrap behaviour itself is `stream-navigation`. | **delete** |
| src/streaming/tests/navigation.rs::toggle_highlight_all_flips_setting | Tests the `#[cfg(test)]`-only `toggle_highlight_all` (a bool flip). | **delete** |
| src/streaming/tests/query.rs::update_pattern_restarts_search | Tests the `#[cfg(test)]`-only `update_pattern`. | **delete** |
| src/streaming/tests/query.rs::toggle_case_sensitive_restarts_search | Tests the `#[cfg(test)]`-only `toggle_case_sensitive`. | **delete** |
| src/streaming/tests/query.rs::toggle_case_sensitive_restarts_while_scanning | Tests the `#[cfg(test)]`-only `toggle_case_sensitive`. | **delete** |
| src/streaming/tests/query.rs::set_filter_mode_restarts_while_scanning | Tests the `#[cfg(test)]`-only `set_filter_mode`. | **delete** |
| src/streaming/tests/query.rs::toggle_case_sensitive_restarts_cleanly_during_long_scan | Tests the `#[cfg(test)]`-only `toggle_case_sensitive`. | **delete** |
| src/streaming/tests/query.rs::set_filter_mode_restarts_cleanly_during_long_scan | Tests the `#[cfg(test)]`-only `set_filter_mode`. | **delete** |
| src/streaming/tests/query.rs::update_pattern_idle_returns_invalid_state | Tests the `#[cfg(test)]`-only `update_pattern`. | **delete** |
| src/streaming/tests/query.rs::update_pattern_too_long_returns_error | Tests the `#[cfg(test)]`-only `update_pattern`. | **delete** |
| src/streaming/tests/query.rs::update_pattern_same_pattern_is_noop | Tests the `#[cfg(test)]`-only `update_pattern`. | **delete** |
| src/streaming/tests/query.rs::set_filter_mode_same_mode_is_noop | Tests the `#[cfg(test)]`-only `set_filter_mode`. | **delete** |
| src/streaming/tests/query.rs::set_filter_mode_empty_pattern_no_restart | Tests the `#[cfg(test)]`-only `set_filter_mode`/`update_pattern`. | **delete** |
| src/streaming/tests/query.rs::set_filter_mode_idle_with_pattern_no_restart | Tests the `#[cfg(test)]`-only `set_filter_mode`, and is mislabelled (§5.4). | **delete** |
| src/streaming/tests/scanning.rs::streaming_generation_starts_at_zero | `StreamingSearch::generation()` is `#[cfg(test)]`, so the counter is unobservable in production. | **becomes-spec-after-fix** |
| src/streaming/tests/scanning.rs::streaming_generation_bumps_on_start_search | Pins the unobservable `StreamingSearch` generation counter. | **becomes-spec-after-fix** |
| src/streaming/tests/scanning.rs::streaming_generation_bumps_on_cancel | Same counter. | **becomes-spec-after-fix** |
| src/streaming/tests/scanning.rs::streaming_generation_bumps_on_content_invalidated | Same counter. | **becomes-spec-after-fix** |
| src/streaming/tests/scanning.rs::streaming_generation_stable_on_noop_invalidation | Same counter. | **becomes-spec-after-fix** |
| src/streaming/tests/scanning.rs::streaming_generation_bumps_on_content_added_with_match | Same counter. | **becomes-spec-after-fix** |
| src/streaming/tests/scanning.rs::streaming_generation_stable_on_content_added_no_match | Same counter. | **becomes-spec-after-fix** |
| src/streaming/tests/scanning.rs::streaming_generation_stable_on_navigation | Same counter. | **becomes-spec-after-fix** |
| src/streaming/tests/scanning.rs::streaming_generation_bumps_on_update_pattern | Tests the `#[cfg(test)]`-only `update_pattern` and `generation()`. | **delete** |
| src/streaming/tests/scanning.rs::content_modified_removes_stale_matches_during_scan | Tests the `#[cfg(test)]`-only `StreamingSearch::content_modified`, which has no production caller. | **delete** |
| src/streaming/tests/scanning.rs::content_modified_noop_for_unscanned_row | Tests the `#[cfg(test)]`-only `content_modified`. | **delete** |
| src/streaming/tests/scanning.rs::content_modified_after_scan_complete | Tests the `#[cfg(test)]`-only `content_modified`. | **delete** |
| src/streaming/tests/scanning.rs::content_modified_removes_all_matches_transitions_to_no_results | Tests the `#[cfg(test)]`-only `content_modified`. | **delete** |
| src/streaming/tests/scanning.rs::content_modified_replaces_with_new_matches | Tests the `#[cfg(test)]`-only `content_modified`. | **delete** |
| src/streaming/tests/scanning.rs::content_modified_in_no_results_transitions_to_has_results | Tests the `#[cfg(test)]`-only `content_modified`. | **delete** |
| src/streaming/tests/scanning.rs::content_modified_bumps_generation | Tests the `#[cfg(test)]`-only `content_modified` and `generation()`. | **delete** |
| src/streaming/tests/scanning.rs::content_modified_noop_in_idle | Tests the `#[cfg(test)]`-only `content_modified`. | **delete** |
| src/streaming/tests/scanning.rs::content_modified_clamps_current_index | Tests the `#[cfg(test)]`-only `content_modified`. | **delete** |
| src/streaming/tests/scanning.rs::wrapped_line_match_coordinates_within_physical_width | Tests the wrapped-row join/remap branch of the `#[cfg(test)]`-only `scan_all`, which has no production caller. | **delete** |
| src/streaming/tests/scanning.rs::wrapped_line_match_spanning_boundary_is_clamped | Tests the wrapped-row branch of the `#[cfg(test)]`-only `scan_all`. | **delete** |
| src/streaming/regex_tests.rs::regex_set_filter_mode_switch | Tests the `#[cfg(test)]`-only `set_filter_mode`/`update_pattern`. | **delete** |
| src/streaming/regex_tests.rs::regex_update_pattern | Tests the `#[cfg(test)]`-only `update_pattern`. | **delete** |
| src/streaming/regex_tests.rs::regex_update_to_invalid | Tests the `#[cfg(test)]`-only `update_pattern`. | **delete** |
| src/streaming/regex_tests.rs::regex_set_filter_mode_invalid_regex_error | Tests the `#[cfg(test)]`-only `set_filter_mode`. | **delete** |
| tests/index_column_map_memory.rs::an_index_of_non_ascii_rows_holds_a_small_multiple_of_their_text | Net-heap ratio bounds under a counting global allocator (CJK ≤ 4× text, etc.): a memory budget. The run-encoding property is `column-map-runs-compact`. | **move-to-bench** |
| tests/index_release_memory.rs::release_reclaims_real_heap_that_clear_retains_without_disturbing_siblings | Net/gross heap ratios under a counting allocator (release frees > 80%, clear retains 2× what release does, refresh growth < 16 KiB): memory budgets. The observable reset is `index-model`; eviction cost is `bitmap-eviction-cost`. | **move-to-bench** |

No test is a pure regression of a removed bug with no general property: every regression test
read is an instance of one of the §1 specs.

## 5. Implied specs that look wrong, or disagree with the code

1. **Bug, covered by no test: streaming fuzzy mode stores a zero-width result, violating
   INV-SEARCH-2.** `src/streaming/engine/matching.rs::fuzzy_matches_in_row` pushes
   `StreamingMatch(row, 0, display_columns(text))` without the `match_len > 0` filter the literal
   and regex arms apply, and `scan_row`/`content_added` store it. Example: `start_search("\t", Fuzzy)`
   then `scan_row(0, "\t", 1)` stores `(0,0,0)`, because `display_columns("\t") = 0`, and
   `verify_result_positions_valid()` is then false. The invariant table in `src/streaming/mod.rs`
   claims INV-SEARCH-2 is discharged. Fix the arm (drop a zero-width fuzzy hit) before proving
   `stream-invariants`.
2. **A test name claims the opposite of the code.**
   `src/streaming/engine/operations.rs::a_row_that_exhausts_the_budget_yields_no_matches` says an
   unfinishable row "yields no matches — fail closed". But `find_matches_in_row` keeps every match
   `scan_line` yielded before the budget ran out, as `scan_row`'s own comment says ("A row matcher
   may return the real prefix of a regex scan it could not finish"). The test row (4096 `x`, and the
   pattern needs a `z`) has no match at all, so it cannot tell the two apart. The real spec is
   `stream-incomplete`: prefix kept, `incomplete()` true.
3. **Vacuous assertions.**
   - `src/tests.rs::case_sensitive_point_navigation_builds_no_owned_intersection`:
     `assert_eq!(take_owned_intersection_builds(), 0)` can never fail. The only increment is in the
     `#[cfg(test)]` `intersect_posting_lists`, reachable only from the `#[cfg(test)]`
     `SearchIndex::search`, so no production path (`find_direction_opts` included) can increment it.
     The `take_last_unique_posting_lists` and candidate-count assertions in the same test are real
     (`index-candidate-cost`).
   - `src/tests.rs::regex_compilation_size_limit_enforced` accepts either `Ok(_)` or
     `Err(InvalidRegex)`, so it pins nothing beyond no-panic, though its doc says it "verifies the
     RegexBuilder size limits are effective". `regex-compile` states the real contract.
   - `src/tests.rs::changed_row_reindex_matches_from_scratch_rebuild`: the block labelled "Negative
     control" compares match counts with the fresh build, which the full `SearchResults` equality
     just above already implies. It never shows that the oracle can fail, so it is not a negative
     control.
   - `src/budgeted.rs` module doc ("regardless of how the rows were sliced across calls", "Pinned
     by the slicing-oracle tests") and `any_slicing_equals_one_shot_across_all_modes`:
     `BudgetedSearch` has no batched feed API, so every slice size issues the same per-row
     `feed_row` sequence and the slicing quantifier is vacuous. The test really checks equality
     across query modes (`budgeted-refines-one-shot`).
4. **Mislabelled or stale.**
   - `src/streaming/tests/query.rs::set_filter_mode_idle_with_pattern_no_restart`: its comment says
     the named scenario (Idle with a non-empty pattern) is unreachable, and the body tests the
     opposite case, a mode change in Searching restarts.
   - `src/streaming/engine/matching.rs` has a stale section header ("Gap coverage:
     map_lower_byte_to_original monotonicity/identity, Part of #2875") above a test module that
     tests neither property.
5. **The generation counters cannot be observed in production.** `TerminalSearch::generation()` and
   `StreamingSearch::generation()` are `#[cfg(test)]`, so production bumps fields nothing can read,
   yet the public docs (`src/lib.rs` "Staleness Detection", `src/streaming/engine/mod.rs`) tell
   consumers to snapshot `generation()`. 16 tests pin them (§4, `becomes-spec-after-fix`), plus
   `streaming_generation_bumps_on_update_pattern` and `content_modified_bumps_generation`, which go
   with the test-only code they call. Either
   expose the accessors and spec them (TerminalSearch: every mutator bumps once, reads never bump;
   StreamingSearch: start/cancel/reflow and result-changing add/invalidate bump, navigation and
   no-op changes do not), or delete the fields, the doc claims and the tests.
6. **The tests of `index-point-find` do not run the shipped code.** `TerminalSearch::find_next` and
   `find_prev` (`#[cfg(test)]`) re-implement the case-sensitive closure of `find_direction_opts`
   instead of calling it; `find_next_opts`/`find_prev_opts` do call it. So
   `terminal_search_find_next_prev`, `find_next_prev_same_line_column_boundary`,
   `find_{next,prev}_optimized`, `find_prev_usize_max_does_not_overflow`,
   `find_prev_boundary_at_first_line`, `find_next_past_last_line`, `empty_query_terminal_search` and
   `find_next_range_query_skips_prefix_matches` never execute the shipped `find_direction_opts`.
   The spec is stated on `find_direction_opts`; make the test helpers delegate to it, or delete them.
7. **Hash-pinning negatives that sample no spec.** `src/tests.rs::bloom_filter_rejection` asserts
   `!might_contain("zzqxj")` and `src/bloom_tests.rs::bloom_insert_bytes` asserts
   `!might_contain_bytes(b"world")`. A Bloom filter may answer true for both; these pin particular
   FNV-1a bit placements, not behaviour. Both tests are subsumed only for their positive and
   length < 3 assertions.
8. **Test-only code that exists only for its tests**, deletable with them:
   `SparseBitmap::{new, remove_range, range_from, range_before, range, contains, iter}` and the
   `SparseBitmapRange`/`SparseBitmapIntoIter` aliases;
   `SearchIndex::{search, intersect_trigrams, intersect_posting_lists, search_ordered, get_line,
   search_with_positions_opts[_direction], find_regex_from, find_{next,prev}_case_insensitive,
   retain_history_from, with_max_cached_lines, set_max_cached_lines, max_cached_lines}`;
   `types::SearchResult`; `StreamingSearch::{update_pattern, scan_all, content_modified, toggle_*,
   set_filter_mode, current_match, pattern, filter_mode, wrap_enabled, case_sensitive,
   highlight_all}`; `grapheme::map_lower_byte_to_original` once `lower-byte-map` is proved. Several
   spec-sample tests use some of these as harness, which is why they survive until their spec is
   proved. But `content_modified` and the `scan_all` wrap/remap logic (#7244, #7471, #7572) are whole
   features that no production code calls.
9. **Budgets, not specs.** `src/bloom_tests.rs::bloom_false_positive_rate`,
   `bloom_lookup_early_return_on_miss`, the four regex capacity checks, and the two
   counting-allocator tests in `tests/` assert statistical, capacity or heap ratios. They move to
   `aterm-bench` (§4).

## 6. Proof order

No spec here is blocked by gap 0 alone: each needs at least one Trust/Clean widening (gap 3 for
scalar arithmetic beyond straight-line code, gap 1 once a `Vec`/`&str` appears). The order is
easiest high-value first.

1. **Scalar or near-scalar, gap 3 (plus 1, 2 or 12) only:** `regex-line-budget` (pure `u64`
   clamp; retires 1 test and is the lemma under every regex spec), `bloom-layout` (`usize`
   rounding and saturation; retires 7 tests), `stream-navigation` (`next_index` is a 5-way scalar
   case split; retires 10 tests, the largest scalar collapse), `search-result-types`,
   `bloom-estimated-fpr` (also gap 6, f64), and the two closed forms of `eviction-schedule`
   (`final_evicted_prefix`, `max_cached_for_retained`; the equality with the real index waits on
   `index-model`).
2. **Small loops over slices:** `fuzzy-subsequence`, `memchr-family`, `ascii-ci-matches`,
   `lower-fold`, `lower-byte-map`, then `searcher-find-rfind` (12 tests, including the exhaustive
   binary/ternary alphabets) with `searcher-linear-time`.
3. **`SparseBitmap`:** `bitmap-set-refinement` + `bitmap-canonical-encoding` (15 tests), then
   `bitmap-eviction-cost`.
4. **Bloom filter:** `bloom-no-false-negatives`, `bloom-clear-count-empty`, `bloom-probe-cost`.
5. **Coordinates:** `line-cells-model`, `column-map-walk`, `column-map-with-cells`,
   `column-map-runs-compact`, `display-columns`, `line-cells-column-queries` (gap 12 for the
   cross-crate `aterm_grapheme` calls).
6. **The index:** `index-model` + `index-posting-invariant` + `index-bloom-dictionary`, then
   `index-literal-cs` (26 tests), `index-literal-ci`, `index-regex-batch`, `index-batch-cap`,
   `regex-compile`, then `index-point-find`, `literal-candidate-bound`, `narrowed-search`,
   `index-candidate-cost`, then `index-observational-equivalence` (a corollary).
7. **Budgeted search:** `budgeted-refines-one-shot`, `budgeted-progressive`.
8. **Streaming engine:** `stream-start`, `stream-row-matches` (36 tests, the largest collapse),
   `stream-lazy-coordinates`, `stream-scan-row`, `stream-content-changes`, `stream-incomplete`,
   then `stream-invariants` (after the §5.1 fix).
9. **Last:** `stream-refines-model`, which needs gap 10 (model→code refinement beyond scalars) and
   replaces the `tests/conformance_streaming.rs` driver.

The three to prove first:

1. **`regex-line-budget`**: one `u64` saturating multiply and a clamp, an `omega`-class proof once
   gap 3 admits `saturating_mul`/`clamp`. Every regex spec cites it.
2. **`stream-navigation`**: `next_index` is pure scalar case analysis, and the `next_match`/
   `prev_match` half is a guarded `&mut self` field write (gap 2). It retires 10 tests.
3. **`bloom-layout`**: `usize` division, modulo and saturation with no loop. It retires 7 tests and
   fixes the sizes `index-bloom-dictionary` depends on.
