<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-search: spec recovery from its 340 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 279 tests
> it deleted and 10 specs whose functions it deleted are gone, and the spec rows no longer cite
> those tests. The §3 gap table counts the 38 specs left. Other counts in this file were taken
> before.

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
test files; `src/tests.rs` is a unit test module.

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
- `Tr` = `{n ∈ dom Rows | Rows[n].cells = Some(c) ∧ c.is_truncated()}`: the lines whose text
  stops short of the line's end (their reader stopped at a byte bound).
- `Batch(All, d)` = `SearchResults { matches: d = Forward ? take(MAX, All) : the last MAX of All
  (ascending), incomplete: results_may_be_incomplete() ∨ |matches| ≥ MAX ∨ cut_short_lines > 0,
  lowest_retained_line: lowest_retained_line(), cut_short_lines: S + |Tr| }`, where `S` is the
  mode's scan count (0 for a literal, `index-regex-batch` for a regex), for every query, `""`
  included (spec `index-batch-cap`).
- `scan_line(re,t)` = `re.find_iter_with_limit(t, regex_line_budget(|t|))`. `Cut(n)` =
  `scan_line(re, Rows[n].text).step_limit_exceeded()` once the iterator is exhausted.
- `Contains_cs(t,q)` ⇔ `q` occurs in `t`. `Contains_ci(t,q)` ⇔ `Fold(t)` contains `Fold(q)`.
  `lines(F)` for a `LineFrame` = `base + offsets`, ascending.
- `T(t)`: the trigram set of a line (spec `index-posting-invariant`).
- `BmInv`, `Abs(bm)`: the `SparseBitmap` invariant and abstraction (specs `bitmap-*`).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (the tests this spec retires; every
test appears under exactly one spec), `gaps` (the audit §7 gap numbers that block the proof; gap 0,
a seal that publishes Clean rows, blocks every spec and is listed on each), `difficulty` (the Rust
features the proof must handle).

```tsv
id	kind	functions	statement	subsumes	gaps	difficulty
search-result-types	contract	types::SearchMatch::{new,len,is_empty}, types::SearchResults::{new,with_cut_short_lines}	SearchMatch::len() = end_col.saturating_sub(start_col); is_empty() ⇔ end_col ≤ start_col. SearchResults::new(m,i,l) has cut_short_lines = 0. with_cut_short_lines(k) sets cut_short_lines = k and incomplete := incomplete ∨ k > 0.	-	0,1,2,3	&self struct fields, saturating_sub, Vec field
regex-line-budget	contract	index::{regex_line_budget,scan_line}	∀ len: regex_line_budget(len) = clamp(min(len,u64::MAX).saturating_mul(REGEX_STEP_PER_BYTE), REGEX_STEP_LIMIT, REGEX_LINE_STEP_CEILING) = clamp(1024·len, 2^22, 2^26). Hence it is monotone in len and ≤ max(1024·len, 2^22). scan_line(re,t) = re.find_iter_with_limit(t, regex_line_budget(|t|)).	src/index.rs::the_line_budget_scales_with_the_line_under_a_ceiling	0,3,12	u64 saturating_mul, clamp, try_from
bloom-layout	contract	bloom::BloomFilter::{with_size,with_capacity,storage_layout,requested_bits_for_capacity,default}	∀ n: with_size(n) has num_bits = 64·⌈min(max(n,64),MAX_BITS)/64⌉, |bits|·64 = num_bits, every word 0, count = 0. So 64 ≤ num_bits ≤ MAX_BITS, num_bits % 64 = 0, and nothing overflows, even for n = usize::MAX. ∀ c: with_capacity(c) = with_size(max(min(c,MAX_EFFECTIVE_CAPACITY).saturating_mul(10), 64)); clamping c never changes the result. default() = with_capacity(10_000), whose num_bits is 100_032.	src/bloom_tests.rs::bloom_with_size_rounding, src/bloom_tests.rs::bloom_large_capacity_no_overflow, src/bloom_tests.rs::bloom_with_size_max_no_overflow	0,1,3	scalar usize arithmetic (div, %, saturating_mul, comparisons), vec! length
eviction-schedule	contract	index::max_cached_for_retained, index::SearchIndex::evict_oldest_lines	Let m = max(cap,1), t = ⌊3m/4⌋, r = m+1−t. After index_line(i, text_i) for i = 0..N−1 (distinct ascending lines) into an index with max_cached_lines = cap, lowest_retained_line() = 0 if N ≤ m, else min(N, (1+⌊(N−m−1)/r⌋)·r). max_cached_for_retained(R) = min{M | ⌊3M/4⌋ ≥ R} = ⌈4R/3⌉ (0 for R = 0), so an index with that cap keeps its newest R lines through any eviction.	src/tests.rs::max_cached_for_retained_protects_the_newest_lines	0,1,2,3,5,7	the closed forms are scalar saturating/div arithmetic (gap 3 only); the equality with the real index needs index-model (HashMap, sort)
bloom-estimated-fpr	contract	bloom::BloomFilter::{estimated_fpr,is_saturated}	estimated_fpr() = 0.0 if count = 0, else (1 − exp(−K·count/num_bits))^K computed in f64. is_saturated() ⇔ estimated_fpr() > 0.5.	src/bloom_tests.rs::bloom_estimated_fpr	0,3,6	f64 exp/powi, usize→f64 casts
memchr-family	equivalence	bytesearch::{memchr,memchr2,memrchr,block_mask,zero_mask,word_at,splat}	∀ needle bytes a,b and haystack h: memchr(a,h) = h.iter().position(|x| x==a); memchr2(a,b,h) = h.iter().position(|x| x==a ∨ x==b); memrchr(a,h) = h.iter().rposition(|x| x==a). Key lemma: zero_mask(w) ≠ 0 ⇔ w has a zero byte lane; the per-lane rescan makes the false positives in higher lanes harmless.	src/bytesearch.rs::a_one_byte_after_a_zero_byte_does_not_shift_the_answer, src/bytesearch.rs::byte_scanning_matches_the_reference, src/bytesearch.rs::every_byte_value_is_findable	0,1,3,5,7	slices, as_chunks const-size arrays, SWAR bit tricks with wrapping_sub, nested loops with early return
ascii-ci-matches	equivalence	literal::AsciiCaseInsensitiveMatches::{new,next}	Requires h.is_ascii() ∧ n.is_ascii(). When |n| > 0, AsciiCaseInsensitiveMatches::new(h,n) yields exactly the ascending list {i | i+|n| ≤ |h| ∧ h[i..i+|n|].eq_ignore_ascii_case(n)}, overlapping occurrences included; when |n| = 0 it yields nothing. This holds on both the short (|h| ≤ 8) path and the memchr2 path.	src/literal.rs::finds_every_ascii_case_insensitive_occurrence	0,1,2,3,5,7	Iterator impl with a &mut self cursor, checked arithmetic, call to bytesearch::memchr2
lower-fold	contract	grapheme::{lower_fold_char,lower_fold,lower_fold_into,lower_need}	lower_fold(s) = lower_fold_char(c) concatenated over c ∈ s.chars(), where lower_fold_char(c) = c.to_lowercase() with U+03C2 mapped to U+03C3. lower_fold_into(s,out) leaves out = lower_fold(s). Hence lower_fold(a++b) = lower_fold(a)++lower_fold(b), the prefix property narrowed-search relies on. lower_need(t) = Unicode if ¬t.is_ascii(); Ascii if t is ASCII with an uppercase byte; None otherwise. For ASCII t, lower_fold(t) = t.to_ascii_lowercase(), and lower_need(t) = None ⇒ lower_fold(t) = t.	(none: a lemma for the ci specs)	0,1,3,5,7	String push loops, char::to_lowercase (std Unicode tables), iterator closures
lower-byte-map	equivalence	grapheme::LowerByteMap::{new,map_to_original}, grapheme::map_lower_byte_to_original (test-only reference)	∀ text t, lb: LowerByteMap::new(t).map_to_original(lb) = the original byte offset of the first char of t whose lowered (lower_fold_char) start offset is ≥ lb, or |t| if there is none. Equivalently it equals map_lower_byte_to_original(t,lb), which can then be deleted.	src/grapheme.rs::lower_byte_map_matches_map_lower_byte_to_original	0,1,3,5,7	Vec<(usize,usize)> built in a loop, binary_search_by_key
searcher-find-rfind	equivalence	bytesearch::Searcher::{new,find_in,rfind_in,rare,factorization}, bytesearch::{two_way,factorize,maximal_suffix,Skip::advance,View::{get,scan},rare_byte}	∀ needle n, haystack h, and any history of earlier find_in/rfind_in calls on the same Searcher (any cached forward/backward factorization and rare byte): find_in(h) = (n empty ? Some(0) : min{i | i+|n| ≤ |h| ∧ h[i..i+|n|] = n}); rfind_in(h) = (n empty ? Some(|h|) : max of the same set); both are None when the set is empty. Lemma: every position Skip::advance steps over has h[at+offset] ≠ n[offset].	src/bytesearch.rs::a_warm_searcher_matches_the_reference, src/bytesearch.rs::exhaustive_binary_alphabet, src/bytesearch.rs::exhaustive_ternary_alphabet, src/bytesearch.rs::periodic_needles_match_the_reference, src/bytesearch.rs::named_edges_match_the_reference, src/bytesearch.rs::realistic_haystacks_match_the_reference	0,1,2,3,5,7,11	Cell interior mutability, const-generic View<REV>, the Two-Way critical-factorization proof, saturating index arithmetic, nested while loops
searcher-linear-time	other (cost)	bytesearch::{two_way,Skip::advance}	find_in/rfind_in terminate on every input and make O(|h|+|n|) byte comparisons. The periodic branch carries memory, so no haystack byte is compared twice per alignment. The adaptive skip switches itself off once calls ≥ PROBE ∧ jumped < calls·MIN_STRIDE, and from then on the loop is plain Two-Way with an unchanged answer.	src/bytesearch.rs::a_useless_skip_switches_itself_off_and_the_answer_is_unchanged	0,7	loop variants/decreases, cost accounting
bitmap-set-refinement	refinement	bitmap::SparseBitmap::{default,new,insert,remove,drop_below,shift,len,is_empty,to_vec,into_iter}, bitmap::intersect_sorted, BitAnd/BitAndAssign for SparseBitmap	Let Abs(bm) = bm.to_vec(). For every bm reachable from default() by these mutators: Abs(bm) is strictly ascending, len() = |Abs|, is_empty() ⇔ Abs = []. insert(v): Abs' = sort(Abs ∪ {v}), returns v ∉ Abs. remove(v): Abs' = Abs ∖ {v}, returns v ∈ Abs. drop_below(w): Abs' = [x ∈ Abs | x ≥ w], the same as removing each x < w one at a time. shift(up,by) requires (up ? last+by ≤ u32::MAX : first ≥ by) and gives Abs' = [x ± by]. Abs(&a & &b) = Abs(a) ∩ Abs(b) ascending, and a &= &b gives the same. into_iter() yields Abs. intersect_sorted(a,b) of strictly ascending slices = their ascending intersection.	src/bitmap.rs::test_bitand_disjoint_and_empty	0,1,2,3,5,7	Vec<u8>/Vec<u32> content, LEB128 decode loops with a labelled break, &mut self, binary_search, operator traits
bitmap-canonical-encoding	invariant	bitmap::SparseBitmap (all mutators), bitmap::{push_varint,rebuild_from_sorted,remove_last,compact_prefix,from_sorted}	BmInv(bm): count = |Abs|; count > 0 ⇒ first = min Abs ∧ last = max Abs; deltas[head..] = Enc(Abs), where Enc concatenates LEB128(x_{i+1} − x_i) over consecutive values, so it equals from_sorted(Abs).deltas byte for byte; head = 0 ∨ head < |deltas| − head; |Abs| ≤ 1 ⇒ head = 0 ∧ deltas = []. BmInv holds for default() and is preserved by insert, remove (including the remove_last fast path), drop_below, shift, &=, rebuild_from_sorted (which also sets head = 0) and clone.	src/bitmap.rs::prefix_eviction_preserves_mixed_operations_and_varint_boundaries, src/bitmap.rs::remove_last_is_o1_and_canonical	0,1,2,3,5,7	Vec<u8> byte-level equality, u32 cursor arithmetic, varint encoding lemmas, loops
bitmap-eviction-cost	other (cost)	bitmap::SparseBitmap::{drop_below,remove,remove_last,compact_prefix}	drop_below(w) and remove(max) never call to_vec, so no survivor is decoded. drop_below consumes exactly the gap bytes of the removed values (PREFIX_BYTES grows by those bytes). Over any sequence of operations, the bytes compact_prefix moves are ≤ the bytes consumed. Under a sliding window (drop_below(k+1); insert(k+W)), deltas.capacity() stays ≤ 2× its initial capacity. The layout clause size_of::<SparseBitmap>() = size_of::<(Vec<u8>,u32,u32,u32)>() on 64-bit becomes a const assertion.	src/bitmap.rs::full_history_eviction_work_is_amortized_and_prefix_storage_is_bounded	0,1,2,7	resource counters (thread_local Cell under cfg(test)), amortized potential argument over Vec capacity, loops
bloom-no-false-negatives	invariant	bloom::BloomFilter::{insert_bytes,insert,might_contain_bytes,might_contain,hash_pair,get_bit_index,set_bit,get_bit}	For every filter f and every sequence insert_bytes(k_1..k_m) on it with no clear() after k_i: f.might_contain_bytes(k_i) = true. insert(s) ≡ insert_bytes(s.as_bytes()) and might_contain(s) ≡ might_contain_bytes(s.as_bytes()). Every bit index is < num_bits (no out-of-bounds access), and insert_bytes only sets bits (monotone).	src/bloom_tests.rs::bloom_no_false_negatives	0,1,2,3,5,7	Vec<u64> content, FNV wrapping_mul/rotate, % num_bits, loops over K, &mut self
bloom-clear-count-empty	contract	bloom::BloomFilter::{clear,item_count,count,might_contain_bytes}	item_count() = the number of insert/insert_bytes calls since construction or the last clear(). clear() sets every word to 0 and count to 0 and keeps num_bits. If every word is 0 then ∀ k: might_contain_bytes(k) = false; in particular a fresh or cleared filter rejects every key.	src/bloom_tests.rs::bloom_clear	0,1,2,5	Vec<u64>::fill, quantified Vec content, &mut self
bloom-probe-cost	other (cost)	bloom::BloomFilter::{insert_bytes,might_contain_bytes}	insert_bytes makes exactly K = 7 set_bit calls. might_contain_bytes makes get_bit calls in order i = 0..K−1 and stops at the first unset bit: exactly K calls when it returns true, ≤ K otherwise. Neither count depends on num_bits.	-	0,2,3,7	cost counters, early-return loop
line-cells-model	refinement	cells::LineCells::{new,push,push_repeated,append_shifted,append_wrapped_row,end_byte,end_col,identity,is_empty,iter,push_run,mark_truncated,is_truncated}, cells::CellRun::{cell,continued_by,end_byte}	Abs(c) = c.iter(), a list of CellSpan. push(r,col,w) = push_repeated(r,col,w,1). push_repeated(r,col,w,k): if |r| > 0 ∧ k > 0 ∧ r.start ≥ end_byte(c), with |r| and k fitting u32, then Abs' = Abs ++ [(r.start+i|r|, r.start+(i+1)|r|, col+i·w', col+(i+1)·w') | i < k] where w' = min(w, u16::MAX); otherwise unchanged. append_shifted(o,b,cols): each run of o shifted by (b,cols) is appended iff its byte start ≥ end_byte, and is_truncated() := is_truncated() ∨ o.is_truncated(). new() is not truncated; mark_truncated() sets is_truncated() and changes no cell. append_wrapped_row(line,text,cells,rs): pads columns end_col..rs with one-column ' ' cells (pushing ' ' onto line), then append_shifted(cells,|line|,rs), then line += text. end_byte/end_col are those of the last cell (0 if none). identity(n) = n one-byte, one-column cells from 0. Representation: head = None ⇒ tail = [], and no two consecutive runs satisfy continued_by (runs are merged maximally).	src/cells.rs::cells_read_back_as_pushed_in_runs_of_one_shape, src/cells.rs::empty_and_overlapping_cells_are_not_recorded, src/cells.rs::appended_cells_are_shifted_and_merged, src/cells.rs::a_wrapped_row_joins_at_its_stride, src/cells.rs::a_joined_line_stops_short_when_a_row_does	0,1,2,3,5,7	Option head + Vec tail, &mut self, saturating arithmetic, String push
column-map-walk	refinement	grapheme::ColumnMap::{new,span,lookup,start_column,end_column,identity}, grapheme::RunBuilder::{push,finish}, grapheme::ClusterRun::grapheme	Let Ref(t,a) = [(g.byte_offset, g.start_col, before_g) for g in grid_graphemes_with(t,a)] ++ [(|t|, total, total)], where before_g is the end_col of the previous grapheme (0 for the first) and total is the last end_col. Let look(x) = the first entry with offset ≥ x (else total). ∀ t, a, s, e: ColumnMap::new(t,a).span(s,e) = (look(s).start, look(e).before). Offsets above u32::MAX saturate. Corollary: end_column(o) = byte_to_column(t,o), the cursor column after every grapheme that starts before o.	src/grapheme.rs::the_runs_answer_as_the_per_grapheme_map_did, src/grapheme.rs::column_map_matches_byte_to_column	0,1,3,5,7,12	Box<[ClusterRun]>, partition_point, u32 narrowing, cross-crate aterm_grapheme::grid_graphemes_with
column-map-with-cells	contract	grapheme::ColumnMap::with_cells, cells::LineCells::{iter,is_identity}	For text t and cells c (cells with byte ≥ |t| are ignored), each grapheme at offset o gets (start,before): before any cell (0,0); at o = cell.byte, (cell.col, the previous cell's end_col or 0); inside a cell whose first char has char_width > 0, (cell.col, cell.end_col); inside a cell and zero-width, (cell.end_col, cell.end_col); past the cell's text, (cell.end_col, cell.end_col). total = the end_col of the last cell with byte < |t| (0 if none). with_cells(t,c).span(s,e) is look() over that list, as in column-map-walk. Corollary: if c is what a grid stores when it writes t left to right in one go (GridColumns steps; VS15 on a wide glyph excluded), then with_cells(t,c).span = ColumnMap::new(t,a).span for all s,e.	src/grapheme.rs::cells_of_a_line_written_in_one_go_answer_as_the_walk, src/grapheme.rs::cells_the_grid_kept_apart_are_where_matches_are_drawn, src/grapheme.rs::graphemes_inside_a_cell_and_bytes_outside_every_cell	0,1,3,5,7,12	peekable iterator merge, grapheme_indices (cross-crate), Option state machine
column-map-runs-compact	invariant	grapheme::RunBuilder::{push,finish}, grapheme::ColumnMap::heap_bytes	The runs of a ColumnMap are the greedy maximal run-length encoding of its per-grapheme list. A new run starts exactly when a grapheme folds or follows a gap (start ≠ before), or breaks the run's byte/column stride. A map whose list is the identity (every byte one grapheme one column wide from 0, total = |t| > 0) stores zero runs. Hence a line that repeats one shape is one run, whatever its length.	src/grapheme.rs::a_line_of_one_shape_is_one_run	0,1,5,7	Vec push with last_mut, u16/u32 narrowing
display-columns	equivalence	grapheme::{display_columns,display_columns_with}	∀ t, a: display_columns_with(t,a) = ColumnMap::new(t,a).span(|t|,|t|).1, the total grid columns of t (str_grid_columns_with). For printable-ASCII t it is |t|. display_columns(t) = display_columns_with(t, Single).	src/grapheme.rs::width_only_matches_coordinate_map_for_unicode_and_controls	0,1,5,7,12	&str byte-predicate loop, cross-crate width routine
line-cells-column-queries	contract	cells::LineCells::{byte_at_column,bytes_in_columns,columns_of}	byte_at_column(col) = Some(byte of the first cell in line order with cell.col ≤ col < cell.end_col and width > 0), else None. bytes_in_columns(s,e) = Some(min byte .. max end) over the cells with width > 0, cell.col < e and cell.end_col > s, else None (so None whenever e ≤ s). columns_of(t,b0,b1) = ColumnMap::with_cells(t,self).span(b0,b1).	src/cells.rs::a_column_names_the_cell_drawn_over_it, src/cells.rs::a_column_range_takes_the_cells_drawn_over_it	0,1,3,5,7	div_ceil, NonZeroUsize, Range<usize>, iterator find_map
index-model	refinement	index::SearchIndex::{new,with_capacity,with_capacity_and_max,with_max_cached_lines,set_max_cached_lines,index_line,index_line_cow,push_line,make_room_for,rekey,evict_below_window,note_window_eviction,evict_oldest_lines,clear,release,drop_history_below,retain_history_from,set_history_prefix_eviction,set_ambiguous_width,len,lowest_retained_line,results_may_be_incomplete}, TerminalSearch (delegating mutators)	Constructors give Rows = ∅, hw = next = lowest = 0, incomplete = false, cap = max(cap,1). index_line_cow(n,t,c): if Rows[n].text = t, only Rows[n].cells := c, and only when c is given. Otherwise let newest = max(hw−1, n) and floor = newest − MAX_KEY_SPAN. If Rows ≠ ∅ ∧ n < floor, the line is dropped, lowest := max(lowest, floor) and incomplete := true. Otherwise rows below floor are dropped (with the same honesty update if any were), Rows[n] := (t,c), hw := max(hw, n+1), next := max(next, n+1), and if |Rows| > cap, the |Rows| − ⌊3cap/4⌋ smallest lines are dropped, lowest := the smallest remaining line (or next) and incomplete := true. push_line(t) = index_line(next, t), returning next. clear/release: Rows = ∅, hw = next = lowest = 0, incomplete = false (cap and amb kept). drop_history_below(f): if f > the smallest retained line, drop rows < f; honesty unchanged. retain_history_from(f): the same drop, then lowest := max(lowest,f), incomplete := true. set_history_prefix_eviction(Some(b>0)): lowest := b, incomplete := true; any other argument sets lowest := 0, incomplete := false. Observers: len() = hw, lowest_retained_line() = lowest, results_may_be_incomplete() = incomplete. Owned text is moved into the cache, not copied. TerminalSearch delegates: index_visible_content(b,ls) = index_line(b+i, ls[i]) in order; index_scrollback_line = push_line.	src/tests.rs::len_is_a_high_water_mark, src/tests.rs::eviction_with_sparse_line_numbers, src/tests.rs::eviction_sets_incomplete_signal_and_watermark, src/tests.rs::cap_is_configurable_and_clear_resets_signal, src/tests.rs::terminal_search_surfaces_eviction_signal, src/tests.rs::drop_history_below_matches_fresh_rebuild_including_completeness, src/tests.rs::drop_history_below_is_a_noop_at_or_below_the_first_cached_line, src/index_key_window_tests.rs::lines_that_cannot_share_the_newest_lines_window_are_reported_evicted, src/index_key_window_tests.rs::an_index_kept_across_four_billion_rows_rekeys_and_stays_complete, src/index_key_window_tests.rs::a_descending_run_of_lines_moves_the_window_once	0,1,2,3,4,5,7,12	FxHashMap<usize,String>, sort_unstable of keys, Cow, &mut self, u32 key-window arithmetic, logging side effect
index-posting-invariant	invariant	index::SearchIndex::{index_line_cow,reindex_changed_row_postings,posting_trigram_set,insert_posting,evict_oldest_lines,evict_below_window,drop_history_below,make_room_for,rekey,might_contain,search (cfg(test))}	T(t) = windows3(bytes t) ∪ (lower_need(t) = Ascii ? windows3(ascii_lower t) : lower_need(t) = Unicode ? windows3(Fold t) : ∅). ∀ reachable idx: dom(trigrams) = ⋃_{n ∈ Rows} T(Rows[n].text); ∀ w: {key_base+k | k ∈ Abs(trigrams[w])} = {n ∈ dom Rows | w ∈ T(Rows[n].text)}, and no list is empty; every cached line is in [key_base, key_base+MAX_KEY]; ∀ w ∈ dom(trigrams): bloom.might_contain_bytes(w). Consequences: might_contain(q) is true whenever |q| < 3 or every trigram of q is in dom(trigrams), so it has no false negatives; the cfg(test) candidate iterator search(q) yields ascending lines ⊇ {n | q occurs in Rows[n].text}, and every retained line when |q| < 3.	src/tests.rs::bloom_filter_rejection, src/tests.rs::reindex_line	0,1,2,3,5,7	FxHashMap<[u8;3],SparseBitmap>, set difference over FxHashSet, quantified map invariant
index-bloom-dictionary	invariant	index::{insert_posting,SearchIndex::rebuild_bloom,SearchIndex::index_line_cow}	bloom.item_count() = the number of times a trigram's posting list went from absent or empty to non-empty since the last rebuild_bloom/clear, because insert_posting inserts into the bloom only then. Re-indexing a line with unchanged text leaves the bloom and postings unchanged. After rebuild_bloom: item_count = |dom trigrams| and num_bits = BloomFilter::with_capacity(min(max(|dom trigrams|,1000), MAX_EFFECTIVE_CAPACITY)).num_bits. Whenever index_line_cow returns with |dom trigrams| < MAX_EFFECTIVE_CAPACITY, ¬bloom.is_saturated(). Neither the row count nor per-row occurrence counts ever size the filter.	src/index_bloom_tests.rs::repeated_output_and_unchanged_refreshes_do_not_grow_the_filter, src/index_bloom_tests.rs::dictionary_growth_rebuilds_without_losing_live_trigrams, src/tests.rs::bloom_rebuild_tracks_trigram_volume_for_bulk_scrollback	0,1,2,3,5,6,7	HashMap entry API, float saturation predicate
index-literal-cs	refinement	index::SearchIndex::{search_with_positions,search_from_line,search_before_line,search_results_opts_direction (cs literal)}, iterators::{SearchMatchIterator,SearchMatchReverseIterator,next_literal_match,CandidateSource}, TerminalSearch::search	LM_cs(n) = [SearchMatch(n,s,e) | p ∈ Occ_cs(q,Rows[n].text) ascending, (s,e) = CM(n).span(p,p+|q|), s ≠ e]. ∀ reachable idx, q: if q = "" every call yields nothing. Otherwise search_with_positions(q) = take(MAX, All_cs); search_from_line(q,f) lazily yields [m ∈ All_cs | m.line ≥ f] (uncapped); search_before_line(q,b) yields reverse([m ∈ All_cs | m.line < b]); search_results_opts_direction(q,true,false,d) = Batch(All_cs,d). Candidate pruning (bloom, trigram postings, key base) never removes a match.	src/tests.rs::empty_query_search_with_positions, src/tests.rs::empty_query_terminal_search, src/tests.rs::search_from_line_basic, src/tests.rs::search_from_line_short_query, src/tests.rs::search_before_line_basic, src/tests.rs::search_match_iterator_multiple_matches_per_line, src/tests.rs::search_reverse_iterator_multiple_matches_per_line, src/tests.rs::search_iterator_multibyte_no_panic, src/tests.rs::search_single_multibyte_char_repeated, src/tests.rs::terminal_search_index_scrollback_lines_batch, src/tests.rs::terminal_search_index_visible_content, src/tests.rs::search_before_line_zero_returns_empty, src/tests.rs::zero_display_width_matches_are_skipped, src/iterators.rs::forward_literal_matches_equal_a_str_find_reference, tests/search_vs_bruteforce.rs::verified_search_matches_bruteforce, src/index_key_window_tests.rs::rows_past_u32_max_are_found_at_their_own_line_numbers	0,1,2,3,4,5,7	lazy iterators over HashMap lookups, Searcher, ColumnMap, u32 key/line conversion
index-literal-ci	refinement	index::SearchIndex::search_case_insensitive, index::CaseInsensitiveMatcher::{new,visit_matches}, index::SearchIndex::literal_candidates_{forward,backward}	Let t = Rows[n].text and let LBM_t be the identity when t and Fold(q) are ASCII, LowerByteMap::new(t) otherwise. LM_ci(n) = [SearchMatch(n,s,e) | p ∈ Occ_ci(q,t) ascending, (s,e) = CM(n).span(LBM_t(p), LBM_t(p+|Fold q|)), s ≠ e]. ∀ reachable idx, q: search_results_opts_direction(q,false,false,d) = Batch(All_ci,d), with no matches when Fold(q) = "". The ASCII ≤2-byte path, the lowered-buffer path and trigram candidate selection all agree with this.	src/tests.rs::case_insensitive_ascii_search_reports_overlapping_matches, src/tests.rs::case_insensitive_greek_final_sigma_unified	0,1,2,3,4,5,7	String buffer reuse, closures with early termination, VecDeque, std str::find
index-batch-cap	contract	index::SearchIndex::{search_matches,search_results_opts_direction,search_regex,search_case_insensitive,search_with_positions}	For every mode the batch result is Batch(All,d) (§0). The cap can cut mid-line: a single line with more than MAX occurrences yields its first (or last) MAX. Every line of Tr counts in cut_short_lines whatever the mode and the cap, and leaves Tr when it is re-indexed with cells read whole or leaves dom Rows. The result does not depend on hash-map iteration order.	src/tests.rs::batch_search_caps_matches_and_flags_incomplete, src/tests.rs::single_line_overlaps_are_streamed_under_the_batch_cap, src/tests.rs::regex_batch_search_caps_matches_deterministically, src/index.rs::a_line_read_short_of_its_end_is_counted_cut_short	0,1,4,5,7	Vec reverse, take, VecDeque window
index-regex-batch	contract	index::SearchIndex::{search_regex,search_results_opts_direction (regex),search_with_positions_opts_direction}, index::scan_line	Let re = compile_regex(q,cs); compile errors propagate unchanged. LM_rx(n) = [SearchMatch(n,s,e) | cap ∈ scan_line(re,t), cap.start < cap.end, (s,e) = CM(n).span(cap), s ≠ e]. search_results_opts_direction(q,cs,true,d) = Batch over the ascending concatenation of LM_rx, where S = #{n ∉ Tr | Cut(n)} over the lines whose scan ran to exhaustion before the cap stopped the walk (forward excludes the line on which the cap is reached; backward includes it). Lines after a cut-short line are still scanned, and a cut line keeps the matches it yielded before the cut. search_with_positions_opts_direction (cfg(test)) returns Err(ScanCutShort) iff cut_short_lines > 0.	src/tests.rs::regex_search_filters_zero_length_matches, src/tests.rs::regex_search_preserves_nonzero_matches_alongside_zero_length, src/index.rs::a_pattern_that_cannot_finish_a_line_is_reported_never_passed_off, src/index.rs::the_pathological_pattern_over_a_long_line_stops_at_the_ceiling	0,1,2,4,5,7,12	cross-crate aterm_regex Matches iterator with a step budget, labelled break
regex-compile	contract	index::SearchIndex::compile_regex, budgeted::BudgetedSearch::with_max_cached_lines	SearchIndex::compile_regex(q,cs) = Err(PatternTooLong) if |q| > 1024. Otherwise it builds RegexBuilder(cs ? q : "(?i)"+q) with size_limit = 128 KiB, step_limit = REGEX_STEP_LIMIT; the result is Ok(re) with re.step_limit() = REGEX_STEP_LIMIT and ¬re.step_limit_exceeded(), or Err(InvalidRegex(builder message)). It never panics. BudgetedSearch::new/with_max_cached_lines(q,cs,true,..) = Err(e) iff compile_regex(q,cs) = Err(e).	src/tests.rs::regex_pattern_too_long_rejected, src/budgeted.rs::invalid_regex_fails_at_construction	0,1,4,12	String/format!, Result enum mapping, cross-crate builder
index-point-find	contract	TerminalSearch::find_direction_opts, index::SearchIndex::{find_next_case_insensitive_from,find_prev_case_insensitive_from,find_regex_direction,find_compiled_regex_from,point_answer}, TerminalSearch::{find_next,find_prev,find_next_opts,find_prev_opts} (cfg(test))	Let All be the UNCAPPED match list of the mode (All_cs, All_ci or regex) and ≺ the lexicographic order on (line, start_col). find_direction_opts(q,cs,rx,{anchor a, dir, inclusive, wrap}): Forward returns the ≺-first m ∈ All with (m.line,m.start_col) ≻ a (⪰ if inclusive); Backward returns the ≺-last m with (m.line,m.start_col) ≺ a (⪯ if inclusive). If there is none and wrap is set, Forward returns the ≺-first m ∈ All and Backward the ≺-last; otherwise None. Regex mode first propagates compile errors, and returns Err(ScanCutShort) iff the answer is None and some line the walk(s) scanned had Cut(n). Results never depend on MAX. An empty query (or empty fold) gives Ok(None).	src/tests.rs::terminal_search_find_next_prev, src/tests.rs::find_next_prev_same_line_column_boundary, src/tests.rs::find_prev_usize_max_does_not_overflow, src/tests.rs::find_next_past_last_line, src/tests.rs::terminal_search_option_navigation_is_ordered_and_case_insensitive	0,1,2,4,5,7,12	closures over iterators, saturating anchor arithmetic, Result
literal-candidate-bound	contract	index::SearchIndex::literal_candidate_bound	Let e = cs ? q : Fold(q). literal_candidate_bound(q,cs) = None if |e| < 3; Some(0) if Rows = ∅ or some trigram of e has no posting list (a bloom rejection implies this); otherwise Some(min over trigrams w of e of |{n ∈ Rows | w ∈ T(Rows[n].text)}|). This bound is ≥ the number of candidate lines the engine's own walk visits for e.	src/tests.rs::literal_candidate_bound_reports_the_engines_own_candidate_count	0,1,3,5,7	Option, iterator min over posting lengths
narrowed-search	contract	index::SearchIndex::search_literal_narrowed, index::CaseInsensitiveMatcher::has_occurrence, types::LineFrame::{with_base,push_offset,lines,contains,len,is_empty}	For q ≠ "": search_literal_narrowed(q,cs,None).results = Batch(All_mode,Forward), and its occurrence_lines is None if the walk was capped, else Some(F) with lines(F) = the ascending {n ∈ dom Rows | Contains(Rows[n].text,q)}, as absolute lines (base+offset) unaffected by later re-keying. search_literal_narrowed(q,cs,Some(F)).results.matches = take(MAX, LM_mode(n) concatenated over n ∈ lines(F) ∩ dom Rows ascending), with incomplete/lowest as in Batch; its occurrence_lines = (capped ? None : Some({n ∈ lines(F) ∩ dom Rows | Contains(Rows[n].text,q)})); no posting list or bloom probe is touched. Hence, if F is the occurrence frame of any char-prefix q' of q (q' = q included) on an index with the same Rows, results = Batch(All_mode,Forward), because every match line of q contains q' (Fold(q'++r) = Fold(q')++Fold(r)). q = "" gives (SearchResults::new([], incomplete, lowest).with_cut_short_lines(|Tr|), None). Every result counts |Tr| in cut_short_lines, as Batch does.	src/tests.rs::narrowed_typing_equals_batch_for_every_prefix_step, src/tests.rs::occurrence_frame_keeps_zero_width_only_lines_narrable, src/tests.rs::frame_reverifies_its_own_query_after_backspace, src/tests.rs::capped_narrowed_walk_matches_forward_batch_and_yields_no_frame, src/tests.rs::narrowed_walk_carries_eviction_honesty_like_batch, src/index_key_window_tests.rs::a_frame_survives_the_index_rekeying_under_it	0,1,2,4,5,7,11	generic enum over iterators, labelled break, u32 offsets
index-candidate-cost	other (cost)	index::SearchIndex::{literal_candidates_forward,literal_candidates_backward,posting_lists,search_from_line,search_before_line,find_next_case_insensitive_from,find_prev_case_insensitive_from}, index::CaseInsensitiveMatcher::visit_matches, iterators::CandidateSource	For a literal walk with folded query fq from line f: if |fq| < 3 the candidate sequence is the numeric range [max(f, first_cached_line), line_count) (descending for backward walks). It is empty when Rows = ∅, the bloom rejects, or a trigram has no posting. Otherwise it is the ascending intersection of the DISTINCT posting lists of fq's trigrams, restricted to ≥ f; posting_lists deduplicates identical lists, so LAST_UNIQUE = the number of distinct trigrams. Walks visit candidates lazily in order, once per candidate line, and stop at the line holding the first qualifying match. The case-insensitive verifier lowers a line into its buffer iff ¬(text ASCII ∧ fq ASCII ∧ |fq| ≤ 2), exactly once per visited line.	src/tests.rs::find_next_range_query_skips_prefix_matches, src/tests.rs::case_sensitive_point_navigation_stops_at_the_neighbouring_match, src/tests.rs::case_insensitive_100k_ascii_short_query_work_is_bounded, src/tests.rs::case_insensitive_short_query_skips_sparse_empty_prefix, src/tests.rs::case_sensitive_reverse_short_query_skips_sparse_empty_prefix, src/tests.rs::case_insensitive_navigation_visits_only_trigram_candidates, src/tests.rs::case_insensitive_repeated_prefix_uses_linear_matcher_path	0,1,2,5,7	cost counters, lazy iterators, pointer-identity dedup
index-observational-equivalence	equivalence	SearchIndex (all observers), TerminalSearch::{index_numbered_content_owned,index_visible_content}, SearchIndex::{reindex_changed_row_postings,rebuild_bloom,rekey}	A corollary of index-model plus the query specs. ∀ reachable idx1, idx2 with Abs(idx1) = Abs(idx2), every observer returns equal values: search_with_positions, search_from_line, search_before_line, search_results_opts[_direction], find_*, search_literal_narrowed, literal_candidate_bound (as defined by Abs), len, lowest_retained_line, results_may_be_incomplete. In particular an arbitrary edit script equals a fresh build over the final texts; A→B→A equals no edit; rebuild_bloom and re-keying are invisible; owned and borrowed indexing are indistinguishable.	src/tests.rs::changed_row_reindex_matches_from_scratch_rebuild, src/tests.rs::changed_row_reindex_round_trip_is_identity, src/tests.rs::lowercase_fast_path_keeps_insert_remove_symmetric, src/tests.rs::owned_numbered_content_matches_borrowed_indexing, src/index_bloom_tests.rs::replacing_last_posting_and_reintroducing_it_preserves_all_search_modes	0,1,2,4,5,7	relational (two-state) reasoning over HashMap-backed state
budgeted-refines-one-shot	equivalence	budgeted::BudgetedSearch::{new,with_max_cached_lines,with_ambiguous_width,feed_row,feed_row_owned,feed_row_cells,feed_row_cow,verify_row,results}	∀ rows r_0..r_{N−1} (text with optional LineCells), base, q, cs, rx, cap, amb: if BudgetedSearch::with_max_cached_lines(q,cs,rx,base,N,cap) = Ok(b), with with_ambiguous_width(amb) applied, and each r_i is fed in order by feed_row_owned or feed_row_cells (feed_row is the cfg(test) borrowed twin), then after the N-th feed b.results() equals the one-shot search over the depth window: with k = N − min(N, max(cap,1)), a SearchIndex with max_cached_lines = cap and ambiguous_width = amb, built by index_line_cow(base+i, r_i.text, r_i.cells) for k ≤ i < N and set_history_prefix_eviction(Some(base+k) if k > 0, else None), then search_results_opts(q,cs,rx).unwrap(). It is not the append-path index fed every row, which evicts to its low-water mark (eviction-schedule) once past the cap. The equality covers matches, incomplete, lowest_retained_line and cut_short_lines. Construction fails exactly when that batch call would (regex-compile).	src/budgeted.rs::owned_feed_matches_borrowed_and_batch_unicode_coordinates, src/budgeted.rs::match_cap_matches_one_shot_and_reports_incomplete, src/budgeted.rs::past_its_cap_the_search_equals_the_depth_window_not_the_eviction_schedule, src/budgeted.rs::combined_match_cap_and_depth_window_equals_one_shot, src/budgeted.rs::a_row_the_regex_scan_cannot_finish_is_reported_like_the_one_shot, src/budgeted.rs::a_row_read_short_of_its_end_is_reported_like_the_one_shot, src/budgeted.rs::the_row_feed_equals_one_shot_across_all_modes	0,1,2,4,5,7,12	Cow, enum matcher with captured closures, cross-crate regex, relational equality with the index model
budgeted-progressive	contract	budgeted::BudgetedSearch::{feed_row_cow,rows_fed,total_rows,is_complete,results,results_range,result_count}	rows_fed grows by exactly 1 per feed while rows_fed < total_rows; a feed when rows_fed ≥ total_rows changes nothing. is_complete() ⇔ rows_fed ≥ total_rows. results().lowest_retained_line is the final watermark fixed at construction (base + total − max(cap,1) when total > max(cap,1), else 0), and no emitted match lies below it. results().matches after k feeds is a prefix of results().matches after k+1 feeds. results_range(s,l) = results() with matches[min(s,n) .. min(s+l,n)] (n = result_count()) and the same incomplete/lowest/cut_short_lines.	src/budgeted.rs::partial_results_are_a_prefix_and_overfeed_is_a_noop, src/budgeted.rs::result_ranges_are_bounded_and_stable_past_the_cap	0,1,2,3,5	Vec slice/clone, saturating min arithmetic
```

## 2. Test → spec map (TSV)

`spec` is the spec that retires the test. A test in §4 has `-` here and its §4 action in
`action`. Rows are grouped by file.

```tsv
file	test	spec	action
src/bitmap.rs	full_history_eviction_work_is_amortized_and_prefix_storage_is_bounded	bitmap-eviction-cost	
src/bitmap.rs	prefix_eviction_preserves_mixed_operations_and_varint_boundaries	bitmap-canonical-encoding	
src/bitmap.rs	remove_last_is_o1_and_canonical	bitmap-canonical-encoding	
src/bitmap.rs	test_bitand_disjoint_and_empty	bitmap-set-refinement	
src/bloom_tests.rs	bloom_clear	bloom-clear-count-empty	
src/bloom_tests.rs	bloom_estimated_fpr	bloom-estimated-fpr	
src/bloom_tests.rs	bloom_false_positive_rate	-	move-to-bench
src/bloom_tests.rs	bloom_large_capacity_no_overflow	bloom-layout	
src/bloom_tests.rs	bloom_no_false_negatives	bloom-no-false-negatives	
src/bloom_tests.rs	bloom_with_size_max_no_overflow	bloom-layout	
src/bloom_tests.rs	bloom_with_size_rounding	bloom-layout	
src/budgeted.rs	a_row_read_short_of_its_end_is_reported_like_the_one_shot	budgeted-refines-one-shot	
src/budgeted.rs	a_row_the_regex_scan_cannot_finish_is_reported_like_the_one_shot	budgeted-refines-one-shot	
src/budgeted.rs	the_row_feed_equals_one_shot_across_all_modes	budgeted-refines-one-shot	
src/budgeted.rs	combined_match_cap_and_depth_window_equals_one_shot	budgeted-refines-one-shot	
src/budgeted.rs	invalid_regex_fails_at_construction	regex-compile	
src/budgeted.rs	match_cap_matches_one_shot_and_reports_incomplete	budgeted-refines-one-shot	
src/budgeted.rs	owned_feed_matches_borrowed_and_batch_unicode_coordinates	budgeted-refines-one-shot	
src/budgeted.rs	partial_results_are_a_prefix_and_overfeed_is_a_noop	budgeted-progressive	
src/budgeted.rs	past_its_cap_the_search_equals_the_depth_window_not_the_eviction_schedule	budgeted-refines-one-shot	
src/budgeted.rs	result_ranges_are_bounded_and_stable_past_the_cap	budgeted-progressive	
src/bytesearch.rs	a_one_byte_after_a_zero_byte_does_not_shift_the_answer	memchr-family	
src/bytesearch.rs	a_useless_skip_switches_itself_off_and_the_answer_is_unchanged	searcher-linear-time	
src/bytesearch.rs	a_warm_searcher_matches_the_reference	searcher-find-rfind	
src/bytesearch.rs	byte_scanning_matches_the_reference	memchr-family	
src/bytesearch.rs	every_byte_value_is_findable	memchr-family	
src/bytesearch.rs	exhaustive_binary_alphabet	searcher-find-rfind	
src/bytesearch.rs	exhaustive_ternary_alphabet	searcher-find-rfind	
src/bytesearch.rs	named_edges_match_the_reference	searcher-find-rfind	
src/bytesearch.rs	periodic_needles_match_the_reference	searcher-find-rfind	
src/bytesearch.rs	realistic_haystacks_match_the_reference	searcher-find-rfind	
src/cells.rs	a_column_names_the_cell_drawn_over_it	line-cells-column-queries	
src/cells.rs	a_column_range_takes_the_cells_drawn_over_it	line-cells-column-queries	
src/cells.rs	a_joined_line_stops_short_when_a_row_does	line-cells-model	
src/cells.rs	a_wrapped_row_joins_at_its_stride	line-cells-model	
src/cells.rs	appended_cells_are_shifted_and_merged	line-cells-model	
src/cells.rs	cells_read_back_as_pushed_in_runs_of_one_shape	line-cells-model	
src/cells.rs	empty_and_overlapping_cells_are_not_recorded	line-cells-model	
src/grapheme.rs	a_line_of_one_shape_is_one_run	column-map-runs-compact	
src/grapheme.rs	cells_of_a_line_written_in_one_go_answer_as_the_walk	column-map-with-cells	
src/grapheme.rs	cells_the_grid_kept_apart_are_where_matches_are_drawn	column-map-with-cells	
src/grapheme.rs	column_map_matches_byte_to_column	column-map-walk	
src/grapheme.rs	graphemes_inside_a_cell_and_bytes_outside_every_cell	column-map-with-cells	
src/grapheme.rs	lower_byte_map_matches_map_lower_byte_to_original	lower-byte-map	
src/grapheme.rs	the_runs_answer_as_the_per_grapheme_map_did	column-map-walk	
src/grapheme.rs	width_only_matches_coordinate_map_for_unicode_and_controls	display-columns	
src/index.rs	a_line_read_short_of_its_end_is_counted_cut_short	index-batch-cap	
src/index.rs	a_pattern_that_cannot_finish_a_line_is_reported_never_passed_off	index-regex-batch	
src/index.rs	ordinary_patterns_over_a_long_joined_line_find_their_matches	-	move-to-bench
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
src/literal.rs	finds_every_ascii_case_insensitive_occurrence	ascii-ci-matches	
src/tests.rs	batch_search_caps_matches_and_flags_incomplete	index-batch-cap	
src/tests.rs	bloom_filter_rejection	index-posting-invariant	
src/tests.rs	bloom_rebuild_tracks_trigram_volume_for_bulk_scrollback	index-bloom-dictionary	
src/tests.rs	cap_is_configurable_and_clear_resets_signal	index-model	
src/tests.rs	capped_narrowed_walk_matches_forward_batch_and_yields_no_frame	narrowed-search	
src/tests.rs	case_insensitive_100k_ascii_short_query_work_is_bounded	index-candidate-cost	
src/tests.rs	case_insensitive_ascii_search_reports_overlapping_matches	index-literal-ci	
src/tests.rs	case_insensitive_greek_final_sigma_unified	index-literal-ci	
src/tests.rs	case_insensitive_navigation_visits_only_trigram_candidates	index-candidate-cost	
src/tests.rs	case_insensitive_repeated_prefix_uses_linear_matcher_path	index-candidate-cost	
src/tests.rs	case_insensitive_short_query_skips_sparse_empty_prefix	index-candidate-cost	
src/tests.rs	case_sensitive_point_navigation_stops_at_the_neighbouring_match	index-candidate-cost	
src/tests.rs	case_sensitive_reverse_short_query_skips_sparse_empty_prefix	index-candidate-cost	
src/tests.rs	changed_row_reindex_matches_from_scratch_rebuild	index-observational-equivalence	
src/tests.rs	changed_row_reindex_round_trip_is_identity	index-observational-equivalence	
src/tests.rs	drop_history_below_is_a_noop_at_or_below_the_first_cached_line	index-model	
src/tests.rs	drop_history_below_matches_fresh_rebuild_including_completeness	index-model	
src/tests.rs	empty_query_search_with_positions	index-literal-cs	
src/tests.rs	empty_query_terminal_search	index-literal-cs	
src/tests.rs	eviction_sets_incomplete_signal_and_watermark	index-model	
src/tests.rs	eviction_with_sparse_line_numbers	index-model	
src/tests.rs	find_next_past_last_line	index-point-find	
src/tests.rs	find_next_prev_same_line_column_boundary	index-point-find	
src/tests.rs	find_next_range_query_skips_prefix_matches	index-candidate-cost	
src/tests.rs	find_prev_usize_max_does_not_overflow	index-point-find	
src/tests.rs	frame_reverifies_its_own_query_after_backspace	narrowed-search	
src/tests.rs	len_is_a_high_water_mark	index-model	
src/tests.rs	literal_candidate_bound_reports_the_engines_own_candidate_count	literal-candidate-bound	
src/tests.rs	lowercase_fast_path_keeps_insert_remove_symmetric	index-observational-equivalence	
src/tests.rs	max_cached_for_retained_protects_the_newest_lines	eviction-schedule	
src/tests.rs	narrowed_typing_equals_batch_for_every_prefix_step	narrowed-search	
src/tests.rs	narrowed_walk_carries_eviction_honesty_like_batch	narrowed-search	
src/tests.rs	occurrence_frame_keeps_zero_width_only_lines_narrable	narrowed-search	
src/tests.rs	owned_numbered_content_matches_borrowed_indexing	index-observational-equivalence	
src/tests.rs	regex_batch_search_caps_matches_deterministically	index-batch-cap	
src/tests.rs	regex_pattern_too_long_rejected	regex-compile	
src/tests.rs	regex_search_filters_zero_length_matches	index-regex-batch	
src/tests.rs	regex_search_preserves_nonzero_matches_alongside_zero_length	index-regex-batch	
src/tests.rs	reindex_line	index-posting-invariant	
src/tests.rs	search_before_line_basic	index-literal-cs	
src/tests.rs	search_before_line_zero_returns_empty	index-literal-cs	
src/tests.rs	search_from_line_basic	index-literal-cs	
src/tests.rs	search_from_line_short_query	index-literal-cs	
src/tests.rs	search_iterator_multibyte_no_panic	index-literal-cs	
src/tests.rs	search_match_iterator_multiple_matches_per_line	index-literal-cs	
src/tests.rs	search_reverse_iterator_multiple_matches_per_line	index-literal-cs	
src/tests.rs	search_single_multibyte_char_repeated	index-literal-cs	
src/tests.rs	single_line_overlaps_are_streamed_under_the_batch_cap	index-batch-cap	
src/tests.rs	terminal_search_find_next_prev	index-point-find	
src/tests.rs	terminal_search_index_scrollback_lines_batch	index-literal-cs	
src/tests.rs	terminal_search_index_visible_content	index-literal-cs	
src/tests.rs	terminal_search_option_navigation_is_ordered_and_case_insensitive	index-point-find	
src/tests.rs	terminal_search_surfaces_eviction_signal	index-model	
src/tests.rs	zero_display_width_matches_are_skipped	index-literal-cs	
tests/index_column_map_memory.rs	an_index_of_non_ascii_rows_holds_a_small_multiple_of_their_text	-	move-to-bench
tests/index_release_memory.rs	release_reclaims_real_heap_that_clear_retains_without_disturbing_siblings	-	move-to-bench
tests/search_vs_bruteforce.rs	verified_search_matches_bruteforce	index-literal-cs	
```

## 3. Coverage

- 340 tests read. 269 map to one of the 48 specs. 71 are not spec samples (§4): 47 delete, 16 becomes-spec-after-fix, 8 move-to-bench.
- The biggest collapses: `index-literal-cs` (26), `searcher-find-rfind` (12), `bitmap-set-refinement` (12), `index-model` (12).
- `lower-fold` retires no test by itself; it is the lemma under every case-insensitive spec. `index-observational-equivalence` is a corollary of `index-model` and the query specs.
- Specs blocked by each audit §7 gap (a spec counts once per gap it lists):

| gap | specs blocked |
|---|---|
| 0 | 38 |
| 1 | 34 |
| 2 | 23 |
| 3 | 25 |
| 4 | 10 |
| 5 | 30 |
| 6 | 2 |
| 7 | 31 |
| 11 | 2 |
| 12 | 9 |

## 4. Tests that are not spec samples

> Status 2026-10-16: 46 of the 47 tests marked **delete** have been deleted, with the
> `#[cfg(test)]`-only `SparseBitmap::remove_range`, `range_before` and `range` they alone used;
> the last of them went with the streaming engine. One **delete** row was not confirmed and
> remains: `src/grapheme.rs::a_map_is_no_larger_than_before`.
> Status 2026-10-17: that test is a `const _` assertion beside `ColumnMap` now, and the
> generation counter (§5.2) is deleted with `generation_stable_across_read_operations` and
> `generation_bumps_once_per_mutation`; their rows are gone.
> Rows marked `move-to-bench` remain.

`delete` means the test samples no spec (usually it tests `#[cfg(test)]`-only code with no
production caller); delete it now, together with that code. `move-to-bench` means it asserts a
statistical, latency or heap budget: a measurement, not a universal property, so it belongs in
`aterm-bench`. `becomes-spec-after-fix` means it pins state production cannot observe; fix the
code (expose the accessor, or remove the field) and it becomes a spec or goes away (§5.2).

| test | why | action |
|---|---|---|
| src/bloom_tests.rs::bloom_false_positive_rate | A statistical false-positive ratio over a hash. A Bloom filter permits any false positive, so there is no universal property. | **move-to-bench** |
| src/index.rs::ordinary_patterns_over_a_long_joined_line_find_their_matches | Capacity check that aterm-regex step costs for chosen patterns over 100 KB–4.3 MB lines stay under the budget: a performance property of another crate. | **move-to-bench** |
| tests/index_column_map_memory.rs::an_index_of_non_ascii_rows_holds_a_small_multiple_of_their_text | Net-heap ratio bounds under a counting global allocator (CJK ≤ 4× text, etc.): a memory budget. The run-encoding property is `column-map-runs-compact`. | **move-to-bench** |
| tests/index_release_memory.rs::release_reclaims_real_heap_that_clear_retains_without_disturbing_siblings | Net/gross heap ratios under a counting allocator (release frees > 80%, clear retains 2× what release does, refresh growth < 16 KiB): memory budgets. The observable reset is `index-model`; eviction cost is `bitmap-eviction-cost`. | **move-to-bench** |

No test is a pure regression of a removed bug with no general property: every regression test
read is an instance of one of the §1 specs.

## 5. Implied specs that look wrong, or disagree with the code

1. **Vacuous assertions.**
   - (Fixed 2026-10-17.) `case_sensitive_point_navigation_builds_no_owned_intersection` asserted
     `take_owned_intersection_builds() == 0`, which no production path could increment; the
     counter is deleted and the test, now `case_sensitive_point_navigation_stops_at_the_neighbouring_match`,
     keeps its real candidate-count assertions (`index-candidate-cost`).
   - (Fixed 2026-10-28.) `src/tests.rs::changed_row_reindex_matches_from_scratch_rebuild` now asserts
     that overwritten substrings leave zero matches and includes a genuine negative control showing
     that a stale row-0 index diverges from `fresh` under the `search_results_opts` oracle.
   - (Fixed 2026-10-17.) `BudgetedSearch` has no batched feed API, so the slice-size loops of
     `the_row_feed_equals_one_shot_across_all_modes` (was `any_slicing_…`) and the two eviction
     tests issued the same `feed_row` sequence at every size; they feed once now, and the module
     doc no longer claims slicings.
2. **(Fixed 2026-10-17.) The generation counter could not be observed in production.**
   `TerminalSearch::generation()` was `#[cfg(test)]`, so production bumped a field nothing read.
   The field, its bumps, the `#[cfg(test)]` `invalidate` and the two tests that pinned it are
   deleted.
3. **(Fixed 2026-10-28.) The tests of `index-point-find` did not run the shipped code.**
   `TerminalSearch::find_next` and `find_prev` (`#[cfg(test)]`) now delegate to `find_next_opts`
   and `find_prev_opts` (`case_sensitive = true, is_regex = false`), so every `index-point-find`
   unit test executes the shipped `TerminalSearch::find_direction_opts`.
4. **(Fixed 2026-10-28.) `bloom_filter_rejection` no longer pins FNV-1a hash bits.**
   `src/tests.rs::bloom_filter_rejection` checks rejection on an empty `SearchIndex` (where zero bits
   are set, so every length >= 3 query is deterministically rejected) alongside positive and length < 3
   queries after indexing.
5. **Test-only code that exists only for its tests**, deletable with them:
   `SparseBitmap::{new, contains}` and the `SparseBitmapIntoIter` alias;
   `SearchIndex::{search, intersect_trigrams, intersect_posting_lists, get_line,
   search_with_positions_opts[_direction], find_regex_from, find_{next,prev}_case_insensitive,
   retain_history_from, with_max_cached_lines, set_max_cached_lines, max_cached_lines}`;
   `types::SearchResult`; `grapheme::map_lower_byte_to_original` once `lower-byte-map` is proved.
   Several spec-sample tests use some of these as harness, which is why they survive until their
   spec is proved.
6. **Budgets, not specs.** `src/bloom_tests.rs::bloom_false_positive_rate`, the regex capacity
   check in `src/index.rs`, and the two counting-allocator tests in `tests/` assert statistical,
   capacity or heap ratios. They move to `aterm-bench` (§4).

## 6. Proof order

No spec here is blocked by gap 0 alone: each needs at least one Trust/Clean widening (gap 3 for
scalar arithmetic beyond straight-line code, gap 1 once a `Vec`/`&str` appears). The order is
easiest high-value first.

1. **Scalar or near-scalar, gap 3 (plus 1, 2 or 12) only:** `regex-line-budget` (pure `u64`
   clamp; retires 1 test and is the lemma under every regex spec), `bloom-layout` (`usize`
   rounding and saturation; retires 7 tests), `search-result-types`, `bloom-estimated-fpr` (also
   gap 6, f64), and the closed forms of `eviction-schedule` (the low-water watermark and
   `max_cached_for_retained`; the equality with the real index waits on `index-model`).
2. **Small loops over slices:** `memchr-family`, `ascii-ci-matches`, `lower-fold`,
   `lower-byte-map`, then `searcher-find-rfind` (12 tests, including the exhaustive binary/ternary
   alphabets) with `searcher-linear-time`.
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

The two to prove first:

1. **`regex-line-budget`**: one `u64` saturating multiply and a clamp, an `omega`-class proof once
   gap 3 admits `saturating_mul`/`clamp`. Every regex spec cites it.
2. **`bloom-layout`**: `usize` division, modulo and saturation with no loop. It retires 7 tests and
   fixes the sizes `index-bloom-dictionary` depends on.
