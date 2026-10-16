<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-scrollback: spec recovery from its 341 unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each of the 53 specs below is
> proved through Trust/Clean about the compiled Rust function, and then the tests it subsumes
> are deleted. Nothing has been proved or deleted yet. The 12 checks in §4 that sample no
> spec can be deleted or moved to a bench now.

Source: `crates/aterm-scrollback` (read only). I read every `#[test]` fn (341 in 29 files under
`src/` and `tests/`) and the production code each one reaches. Nothing was compiled or run.
The claims below come from reading the code.

Result: **53 specs**, covering **329 tests**. **12 tests are not spec samples** (§4).

## 0. Notation used in the statements

- `Wire(l)` = `Line::serialize(l)`: the bytes for content, the attrs RLE runs, the WRAPPED flag,
  hyperlink spans with their OSC 8 ids, and underline-colour spans. Two lines are *Wire-equal*
  when their `Wire` bytes are equal.
- `Strip(l)` = `l` with images set to `None`. `Line::serialize` writes no image section, so
  `Wire(l) = Wire(Strip(l))`. Every line that leaves the hot tier is `Strip`-ed.
- `Caps`: the lines with `len ≤ MAX_LINE_CONTENT_BYTES`, every hyperlink URL `≤ MAX_HYPERLINK_URL_BYTES`,
  every id `≤ MAX_HYPERLINK_ID_BYTES`, at most `MAX_HYPERLINK_SPANS` hyperlink spans and at most
  `MAX_UNDERLINE_SPANS` underline spans. The decoders only ever return lines in `Caps`.
- `Reach`: the stores (`Scrollback`, `DiskBackedScrollback`, `ScrollbackStorage`) reachable from
  their public constructors by any sequence of public calls.
- `M(s)`: the ghost oldest-first sequence of `Strip`-ed lines a store `s` holds. A store with a
  corrupt warm block is modelled as *slots*: `Some(line)`, an unreadable slot (`get_line` is `Err`),
  or a blank slot (a block quarantined mid-history).
- `V(t)`: the live-line view of a tier `t` (`HotTier`, `WarmTier`, `ColdTier`, `DiskColdTier`): the
  concatenation of its blocks or pages with the first `front_offset` lines dropped.
- `take(k, xs)`, `drop(k, xs)`, `last(k, xs)`: the first `min(k,|xs|)` elements, all but those, and
  the last `min(k,|xs|)` elements.
- `T` = `QUARANTINE_THRESHOLD`.
- **Observables.** Specs are stated over production accessors (`attrs()`, `hyperlinks()`,
  `underline_colors()`, `images()`, `warm_line_count()`, `cold_line_count()`, ...). Several helpers
  the tests call are `#[cfg(test)]`-only: `has_attrs`, `attr_run_count`, `has_underline_colors`,
  `underline_color_count`, `get_image`, `hot_line_count`, `block_size()`, `HyperlinkSpan::width`,
  `take_records_walked`, `take_*_find_*_steps`, `recompute_*`, `corrupt_oldest_block`,
  `inject_corrupted_warm_block`. A proof may use them only as ghost views or as fault-injection
  preconditions.
- **Constants.** `MAX_LINE_CONTENT_BYTES` = 2 MiB; `MAX_HYPERLINK_URL_BYTES` = 8192;
  `MAX_HYPERLINK_ID_BYTES` = 256; `MAX_HYPERLINK_SPANS` = `MAX_UNDERLINE_SPANS` = 4096;
  `MAX_HYPERLINK_SPAN_BYTES` = 8460; `MAX_DECODE_PAGE_LINES` = 2^20; `MAX_DECOMPRESSED_SIZE` = 64 MiB
  with an LZ4 ratio cap of 512; `DEFAULT_LINE_LIMIT` = 100,000; watermark percentages 80 (yellow),
  50 (yellow exit) and 95 (red); `SYNC_APPEND_BYTES` = 1 MiB.
- **Lifting `trust::skip`.** Many target functions carry `#[cfg_attr(trust_verify, trust::skip)]`:
  `serialize`/`deserialize`, `walk_records`, the strict decoders, `clear_hyperlinks`, `set_images`
  and `Clone`. Lifting that skip is part of each proof.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (count of primary tests; the list is in
§2), `difficulty` (Rust features the proof must handle), `blocking_gaps` (row numbers of
`AUDIT-trust-for-test-migration-2026-10-15.md` §7 (historical internal audit)). Gap 0, the seal that publishes
Clean rows, blocks every spec on the machine's `trust` channel. Seal `150efe4a` closes it locally
(`PLAN-tests-to-specs-2026-10-15.md` (historical internal plan)), but admitting it to the channel is owner-gated.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
line-wire-roundtrip	round-trip	Line::{serialize,append_serialized,deserialize}	∀ l ∈ Caps: Line::deserialize(&l.serialize()) = Some(d) with d = Strip(l) (content bytes, attrs runs, WRAPPED, hyperlink spans with ids, underline-colour spans; images() = None). Hence Wire(d) = Wire(l).	8	Vec<u8>/slices, Option<Box<Rle>>, SmallVec sidecars, Arc<str>, loops over runs and spans, u32/u16 LE encoding; serialize/deserialize are trust::skip today	0,1,3,7
line-wire-version	contract	Line::{serialize,append_serialized}	∀ l: l.serialize()[0] = 4 if l.underline_colors().is_some() else 3; Wire(l) = Wire(Strip(l)) (images contribute no bytes); a line with no underline colours has no bytes after its hyperlink section.	2	Vec<u8> writer, Option sidecars	0,1,3
block-roundtrip	round-trip	serialize_lines, deserialize_lines, deserialize_lines_capped, walk_records	∀ ls ∈ Caps* with |ls| ≤ u32::MAX: serialize_lines(ls) = u32le(|ls|) ++ concat(map Line::serialize ls), and deserialize_lines(serialize_lines(ls)) = map Strip ls (equal length, Wire-equal elementwise).	6	Vec<Line>, nested loops, framing arithmetic over slices, closure passed to walk_records; trust::skip to lift	0,1,3,5,7,13
serialized-size-exact	contract	Line::serialized_size_estimate, HyperlinkSpan::serialized_size, serialize_lines, Line::serialize	∀ l: |l.serialize()| = l.serialized_size_estimate() = 6 + len + (1 or 5+14·runs) + (2 + Σ span.serialized_size) + (0 or 2+8·ul_spans); span.serialized_size = 12 + |url| + |id|; |serialize_lines(ls)| = 4 + Σ estimate, and below MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES the Vec's capacity = len. A plain line is len+9 bytes.	3	saturating folds over slices; Vec capacity as an observable (needs a capacity model)	0,1,3,7
block-decode-total-bounded	no-panic	deserialize_lines, deserialize_lines_capped, walk_records, declared_count, line_size_v0, line_size_v1v2	∀ d: deserialize_lines(d) returns without panic; |d| < 4 ⇒ []; otherwise, with count = u32le(d[0..4]), the result is the Line::deserialize-accepted records among the framed records walked from offset 4 (stopping at the first unframeable or overrunning record, or after count records). So |result| ≤ min(count, (|d|-4)/5) and capacity = len.	10	slice walk with checked arithmetic, loop invariant offset ≤ |d|, shrink_to_fit, closure	0,1,3,5,7,13
line-decode-total-capped	no-panic	Line::{deserialize,deserialize_attrs,deserialize_hyperlinks_v3,deserialize_underline_colors,deserialize_legacy}	∀ d: Line::deserialize(d) returns without panic; it is None if d = [], if d[0] ≠ 0 ∧ |d| < 7, or if the declared content length exceeds MAX_LINE_CONTENT_BYTES or overruns d. If Some(l) then l ∈ Caps and images() = None: an over-cap URL drops its span, an over-cap id becomes None with the URL kept, span counts are capped, and RLE run lengths saturate when summed.	5	slice indexing, u32 LE reads, Arc<str> from bytes, SmallVec, loops, saturating RLE extend	0,1,3,5,7,12
record-is-valid-mirrors-deserialize	equivalence	Line::record_is_valid, Line::deserialize, count_page_lines, deserialize_lines_capped	∀ d: Line::record_is_valid(d) ⇔ Line::deserialize(d).is_some(); hence ∀ d, m: count_page_lines(d, m) = |deserialize_lines_capped(d, m)|.	1	two slice readers related by an equivalence; the corollary composes callee ensures through walk_records	0,1,3,4,5,7
capped-decode-prefix	contract	deserialize_lines_capped, deserialize_page_lines, deserialize_lines	∀ d, m: deserialize_lines_capped(d, m) = take(m, deserialize_lines(d)); deserialize_page_lines(d) = deserialize_lines_capped(d, MAX_DECODE_PAGE_LINES).	1	Vec<Line> prefix relation, loop with two bounds	0,1,4,5,7
strict-decode-contract	contract	deserialize_lines_strict, decode_line_strict, line_char_count	∀ d, L, C, B, R: deserialize_lines_strict(d,L,C,B,R) = Some(ls) ⇔ |d| ≥ 4, count = u32le(d[0..4]) ≤ L, and d splits exactly (no trailing byte) into count framed records r_i, each with declared content_len ≤ B, |r_i| ≤ R, Line::deserialize(r_i) = Some(l_i), |l_i| ≤ B, attrs RLE total ≤ max(C, line_char_count(content)) (bytes b with b & 0xC0 ≠ 0x80), and every hyperlink and underline span has start_col ≤ end_col ≤ C; then ls = [l_i]. An admitted block re-serializes to the same bytes.	6	Option-returning slice walk, Vec::try_reserve_exact, iterator all/filter/count, u16/usize casts	0,1,3,5,7,13
stripping-decode-refines-strict	refinement	deserialize_lines_strict_dropping_over_cap_links, deserialize_lines_strict, decode_line_strict, max_linked_record_bytes, Line::clear_hyperlinks	∀ d, L, C, B, R: (a) strict(d) = Some(ls) ⇒ stripping(d) = Some((ls, [])); (b) stripping(d) = Some((ls, S)) ⇔ every record i ∉ S passes decode_line_strict under R, and every i ∈ S has R < |r_i| ≤ max_linked_record_bytes(R, C), passes every other strict bound, r_i = Wire(decoded_i), decoded_i.has_hyperlinks(), and ls[i] = decoded_i with hyperlinks cleared and |Wire(ls[i])| ≤ R; S is strictly ascending and there are no trailing bytes; otherwise None.	3	Vec<Line>, Vec<usize>, byte-slice equality, callee ensures of decode_line_strict and clear_hyperlinks	0,1,2,3,4,5,7
link-ceiling-formula	contract	max_linked_record_bytes	∀ r, c: usize: max_linked_record_bytes(r, c) = r.saturating_add(c.saturating_mul(MAX_HYPERLINK_SPAN_BYTES)), and MAX_HYPERLINK_SPAN_BYTES = 12 + MAX_HYPERLINK_URL_BYTES + MAX_HYPERLINK_ID_BYTES = 8460.	1	const fn, saturating_add/saturating_mul on usize	0,3
clear-hyperlinks-frame	contract	Line::clear_hyperlinks, Line::serialize	∀ l: after l.clear_hyperlinks(), l'.hyperlinks() = None and every other field (content, attrs, flags, underline colours, images) is unchanged; so Wire(l') = Wire(l with an empty hyperlink vector), and on a link-free line Wire is unchanged.	1	&mut self frame condition over a struct with boxed sidecars; drop glue (trust::skip today)	0,1,2
attr-lookup-and-cursor	equivalence	Line::{get_attr,attr_cursor}, AttrRunCursor::attr_at, attr_runs	∀ l, i: l.get_attr(i) = expand(runs(l))[i] if i < Σ run.length else CellAttrs::DEFAULT (also when attrs is None); ∀ query sequences q_1..q_n, a fresh c = l.attr_cursor() gives c.attr_at(q_k) = l.get_attr(q_k) for every k (cursor invariant: run_start = Σ lengths of runs[0..i]).	2	loop over &[Run<CellAttrs>], &mut cursor state, saturating u32, usize→u32 try_from	0,1,2,3,5,7
line-constructors-normalize	contract	Line::{new,from,from_bytes,with_attrs,with_hyperlinks,from_parts,set_wrapped,is_wrapped,hyperlink_count,has_hyperlinks}, LineContent::from_bytes	∀ b, a, h: Line::from_bytes(b).as_bytes() = b; Line::new() is empty with no sidecars; from_parts stores attrs = None ⇔ a has 0 runs or one run with is_default(), else Some(a); hyperlinks = None ⇔ h = [], else Some(h) with hyperlink_count = |h|; LineContent::from_bytes(b) is Inline ⇔ |b| ≤ INLINE_SIZE (32); ∀ l, w: after set_wrapped(w), is_wrapped() = w and nothing else changes.	7	enum LineContent (inline array vs Vec), Box/SmallVec, bitflags insert/remove, &mut self	0,1,2,3
span-column-lookup	contract	HyperlinkSpan::contains, ImageSpan::{contains,tile_at}, UnderlineColorSpan::contains, Line::{get_hyperlink,get_hyperlink_span,get_underline_color}	∀ span s, column c: s.contains(c) ⇔ s.start_col ≤ c < s.end_col; ImageSpan::tile_at(c) = Some((image_row, first_cell_col.saturating_add(c - start_col))) if contains(c) else None; Line::get_hyperlink/get_hyperlink_span/get_underline_color(c) return the field of the first stored span containing c, and None when none does or the sidecar is None.	5	u16 compares on &self, loops over &[Span], Option<&Arc<str>> returns, tuple return	0,1,2,3,5,7
cell-attrs-codec-mask	contract	CellAttrs::{serialize,deserialize,from_raw,is_default}	∀ fg, bg: u32, flags: u16: CellAttrs::deserialize(&CellAttrs::new(fg,bg,flags).serialize()) = Some(CellAttrs::new(fg,bg,flags)); from_raw(fg,bg,flags).flags = flags & 0x3BFF; is_default(a) ⇔ a = CellAttrs::DEFAULT = (0xFFFF_FFFF, 0xFF00_0000, 0); deserialize(d) = None ⇔ |d| < 10.	3	[u8;10] array, LE byte packing, bit masks	0,1,3
image-memory-share	contract	Line::memory_used, ImageSpan::memory_used, aterm_types::ImageData::per_row_bytes	∀ payload P with R ≥ 1 rows, ∀ r < R, with line_r the bare line carrying one span of row r: memory_used(line_r) - memory_used(Line::new()) = size_of::<SmallVec<ImageSpan,1>>() + size_of::<ImageSpan>() + per_row_bytes(P, r), each share is ⌊|P|/R⌋ or ⌈|P|/R⌉, and |P| ≤ Σ_r share < |P| + R (the payload is charged once, not once per row).	2	saturating sums, Arc<ImageData>, cross-crate per_row_bytes, size_of constants	0,1,3,7,12
lz4-decode-classification	contract	lz4::decompress_size_prepended, map_decompress_error, decompress_lz4_bounded, decode_cold_bounded, WarmBlock::{decompress,to_cold_compressed}	∀ x, with claim = u32le(x[0..4]) and body = |x|-4: decompress_size_prepended(x) = Err(InputTooShort) if |x| < 4; else Err(OutputTooLarge(claim)) if claim > MAX_DECOMPRESSED_SIZE; else Ok([]) if claim = 0 ∧ body = 0; else Err(CompressionBombSuspected{claim, body}) if claim > body·512 (saturating); else Err(_) or Ok(out) with |out| = claim. Never panics. decompress_lz4_bounded maps Err e to ScrollbackError::Decompression(e.to_message()), and the WarmBlock decoders propagate it.	13	enum result classification, saturating_mul, cross-crate aterm_lz4 decoder (needs its ensures)	0,1,3,4,12
lz4-roundtrip	round-trip	lz4::{compress_prepend_size,decompress_size_prepended}, encode_cold_block, decode_cold_bounded, decompress_lz4_bounded	∀ x with |x| ≤ MAX_DECOMPRESSED_SIZE: compress_prepend_size(x) = Ok(c) and decompress_size_prepended(&c) = Ok(x); compress_prepend_size errs ⇔ |x| > u32::MAX; likewise for encode_cold_block/decode_cold_bounded, decompress_lz4_bounded, and data compressed by aterm_lz4 directly. Needs the aterm_lz4 lemma that its body length ≥ ⌈|x|/255⌉, so the 512:1 gate never fires on honest output.	5	Vec<u8>, cross-crate LZ4 correctness lemma	0,1,4,7,12
dec-string-decimal	contract	error::dec_string	∀ v: usize: dec_string(v) = format!("{v}") (shortest base-10 rendering, no sign, no leading zero except "0").	1	String building in a digit loop	0,1,7
error-source-io-only	contract	ScrollbackError::source	∀ e: e.source().is_some() ⇔ e is ScrollbackError::Io(_), and then it is that io::Error.	1	enum match, Option<&dyn Error>	0,1,3,11
watermark-transition-table	table-property	watermark::recompute_watermark, Scrollback::update_watermark_level, DiskBackedScrollback::update_watermark_level	∀ level c, ∀ b, r, y, x: recompute_watermark(c, b, r, y, x) = Red if b ≥ r; else Yellow if b ≥ y; else Yellow if c = Red; else Green if c = Yellow ∧ b < x; else c. update_watermark_level sets level := recompute_watermark(level, budgeted_bytes, red, yellow, yellow_exit).	8	3-variant non_exhaustive enum match, usize compares; &mut self for the method	0,1,2,3
threshold-bytes-bounds	contract	watermark::threshold_bytes	∀ p ≤ 100, ∀ b: threshold_bytes(p, b) = ⌊b·p/100⌋ (in u128); hence ≤ b, monotone in p (exit(50) ≤ yellow(80) ≤ red(95)), and = 0 ⇔ b·p < 100.	1	u128 widening, saturating_mul, division, narrowing cast with min	0,3
store-watermark-invariant	invariant	Scrollback::{new,with_block_size,clear,set_memory_budget,set_watermark_thresholds,sync_accounting}, DiskBackedScrollback::sync_accounting, ScrollbackStorage::watermark_level	∀ s ∈ Reach: (yellow, yellow_exit, red) = (threshold_bytes(Y, budget), threshold_bytes(50, budget), threshold_bytes(R, budget)) for the configured Y, R (default 80, 95); a fresh or cleared store is Green; after every mutator, level = recompute_watermark(previous level, budgeted_memory_used(), red, yellow, yellow_exit). ScrollbackStorage::watermark_level() is the inner store's.	8	type invariant over a large struct with &mut self methods; composes watermark-transition-table	0,1,2,4,6
eager-promotion-at-yellow	contract	Scrollback::push_line, DiskBackedScrollback::push_line, promote_hot_to_warm	∀ s ∈ Reach, l: push_line promotes one block hot→warm before appending ⇔ hot.len() ≥ hot_limit, or level ≥ Yellow ∧ hot.len() ≥ block_size. So below Yellow, fewer than hot_limit pushes leave warm and cold unchanged.	2	&mut self over VecDeque/Vec tiers, composition into LZ4 compression	0,1,2,4,7
scrollback-refines-seq	refinement	Scrollback::{push_line,push_str,get_line,get_line_rev,line_count,iter,truncate,remove_newest,clear,set_line_limit}, promote_hot_to_warm, evict_warm_to_cold	∀ s ∈ Reach with no corrupt block (any limits, block size, budget): line_count() = |M|; get_line(i) = Ok(Some(l)) with Strip(l) = M[i] for i < |M|, else Ok(None); get_line_rev(i) = get_line(|M|-1-i); push_line(l): M := drop(k, M ++ [Strip l]) with k the excess over line_limit plus the pressure-evicted prefix (budget-enforcement-fifo); truncate(n): M := last(n, M); remove_newest(n): M := take(|M| - min(n, |M|), M); clear: M := []; set_line_limit(Some(k)): M := last(k, M); set_line_limit(None) keeps M. Wire-equality survives hot→warm→cold, including rows larger than the warm page cap.	28	ghost Seq bound to VecDeque<Line> + VecDeque<WarmBlock> + cold pages; &mut self; loops; LZ4 calls; Cow returns	0,1,2,4,5,7,10
disk-backed-refines-seq	refinement	DiskBackedScrollback::{push_line,push_str,get_line,truncate,remove_newest,clear,set_line_limit,evict_warm_to_cold}	scrollback-refines-seq verbatim for DiskBackedScrollback when every I/O call succeeds (each mutator returns Ok); cold reads go through the .dtrm file and every line is readable from its tier.	15	as scrollback-refines-seq, plus fs/mmap effects on the cold path (assume successful I/O)	0,1,2,4,5,7,8,10
tier-shape-invariant	invariant	Scrollback::{new,with_block_size,truncate,remove_newest}, DiskBackedScrollback::with_config, DiskBackedScrollbackConfig::{new,with_unlimited_lines}	∀ s ∈ Reach: line_count = hot.len() + warm.line_count() + cold.line_count(); hot.len() ≤ hot_limit after every completed push/truncate/remove_newest; warm.line_count() ≤ warm_limit after every push; hot_limit = max(h, 1); block_size = clamp(bs, 1, min(hot_limit, MAX_DECODE_PAGE_LINES)) (Scrollback::new uses DEFAULT_BLOCK_SIZE); a fresh store is empty with line_limit = Some(DEFAULT_LINE_LIMIT) (DiskBackedScrollbackConfig::new likewise; with_unlimited_lines gives None).	7	type invariant across every &mut self method, min/max clamps, constants	0,1,2,3,6
truncate-oldest-tier-first	contract	Scrollback::{truncate,set_line_limit}, DiskBackedScrollback::truncate	∀ s ∈ Reach with tier counts (c, w, h), ∀ n < c+w+h, r = c+w+h-n: a successful truncate(n) takes rc = min(r, c) from cold, rw = min(r-rc, w) from warm and r-rc-rw from hot. So r ≤ c changes only cold, and n ≤ h empties cold and warm.	2	scalar arithmetic over tier counts plus &mut tier calls	0,1,2,4
failure-atomicity	contract	Scrollback::{truncate,remove_newest,set_memory_budget}, DiskBackedScrollback::remove_newest, ColdTier::truncate_back_lines, DiskColdTier::truncate_back_lines, WarmTier::truncate_front_lines	∀ s ∈ Reach: truncate(n) decodes no warm block or cold page, so it returns Ok with line_count = min(n, line_count) even when the warm boundary block is corrupt; if remove_newest(n) or ColdTier/DiskColdTier::truncate_back_lines returns Err then line_count, every tier count, budgeted_memory_used and every line's readable content are unchanged (on disk a failed cold write leaves the RAM tiers and counters unchanged); if set_memory_budget(b) cannot evict because the warm block it must move is corrupt, it returns Err(EnforcementFailed) with warm and cold unchanged and memory_budget() = b.	7	two-state &mut self postconditions on the Err path, Result, fs write failure on the disk path	0,1,2,4,8
warm-block-quarantine-fsm	table-property	WarmBlock::{record_failure,is_quarantined,decompress}, WarmTier::get_line	∀ block with failure counter f (Cell<u8>): record_failure sets f := f+1 (saturating) and returns f+1 ≥ T; is_quarantined ⇔ f ≥ T; a successful decompress sets f := 0; WarmTier::get_line on a block with f < T decodes and on failure increments f and returns a non-Quarantined Err; with f ≥ T it returns Err(Quarantined(lines)) without decoding. So exactly T decode errors precede the first Quarantined error.	2	interior mutability (Cell<u8>) through &self, small state machine	0,1,2,3
store-corruption-semantics	contract	Scrollback::{evict_warm_to_cold,push_line,get_line}, WarmTier::{pop_front,push_front}, ColdTier::push_block	∀ s ∈ Reach with a corrupt warm block B of k live lines (slot model): a failed warm→cold eviction of B puts B back at the warm front, losing no line (get_line on B's rows is Err, other rows unchanged); after the T-th failed attempt (reads count too) B is removed: if cold is empty its k rows are dropped (line_count -= k), otherwise they stay as blank slots at the same indices and pressure_evicted_lines += k; a repaired B serves only its live suffix (front_offset respected) on read and on eviction; ColdTier::push_block of an undecodable block returns 0 and changes nothing; the line limit is enforced again once B is gone.	9	VecDeque reordering, Cell counters, Result propagation, ghost slot model with holes	0,1,2,4,7,10
iter-and-dense-walk-match-get-line	equivalence	Scrollback::iter, ScrollbackStorage::{iter,iter_rev,dense_from}, ScrollbackIter::skipped_lines, DenseScrollbackIter::{placeholders,len}	∀ s ∈ Reach (memory or disk, any truncation state): iter() yields [l | i < line_count, get_line(i) = Ok(Some(l))] in order, skipped_lines() = #{i | get_line(i) is Err}, and yielded + skipped = line_count; iter_rev() yields the reverse; ∀ start: dense_from(start).len() = line_count.saturating_sub(start), it yields [get_line(i).ok().flatten() | start ≤ i < line_count], and placeholders() counts the Nones.	11	iterator structs over tier segments, Cow, ExactSizeIterator, loops	0,1,2,5,7,13
hot-reads-borrow	contract	Scrollback::{get_line,get_line_rev}, ScrollbackStorage::dense_from	∀ s ∈ Reach, i < line_count: get_line(i) is Cow::Borrowed ⇔ i ≥ line_count - hot.len(), else Cow::Owned; the dense walk yields Some(Cow::Borrowed) exactly for hot rows.	2	Cow variant as an observable, lifetimes	0,1,2,3
memory-accounting-exact	invariant	Scrollback::{sync_accounting,total_memory_used,memory_used,cold_memory_used,budgeted_memory_used}, DiskBackedScrollback::total_memory_used, WarmTier::memory_used, HotTier::memory_used, Line::memory_used	∀ s ∈ Reach after every public call: total_memory_used() = recompute_total_memory_used() = memory_used() + cold_memory_used(); budgeted_memory_used() = recompute_budgeted_bytes(); over_budget() ⇔ budgeted > memory_budget; read-only calls (get_line, iter) leave budgeted_memory_used() unchanged but may raise memory_used()/cold_memory_used() by the caches they fill; a push_line with no promotion raises memory_used() by Line::memory_used(l).	12	running counters vs recomputation (folds over all tiers), Cell caches mutated through &self	0,1,2,5,7
budget-enforcement-fifo	contract	Scrollback::{set_memory_budget,handle_memory_pressure,push_line}, ColdTier::pop_front_batch	∀ s ∈ Reach with no corrupt block: after push_line or a successful set_memory_budget(b), ¬over_budget() or cold is empty; eviction removes whole oldest cold pages only (M loses a prefix, the newest line survives); pressure_evicted_lines grows by exactly the lines budget eviction removed, and set_line_limit/truncate never change it; memory_budget() = b afterwards.	6	loop until a predicate, monotone counter, composes cold-tier-refines-seq	0,1,2,4,7
image-retention-horizon	contract	Scrollback::{push_line,promote_hot_to_warm,image_rows_dropped_by_compression,clear}, DiskBackedScrollback::promote_hot_to_warm, count_image_rows	∀ s ∈ Reach, i: if row i is hot, get_line(i).images() is the pushed span list with the same Arc payload (ptr_eq); otherwise images() = None and Strip content is unchanged; each promotion adds count_image_rows(batch) = #{l ∈ batch | l.has_images()} to image_rows_dropped_by_compression, which is monotone and survives clear(); when image rows leave hot their per_row_bytes shares leave total_memory_used.	7	Arc pointer identity, monotone u64 counter, composition across promotion	0,1,2,4,7,12
checkpoint-snapshots	contract	ScrollbackStorage::{checkpoint_snapshot,checkpoint_snapshot_fast}, Scrollback::checkpoint_snapshot_fast, DiskBackedScrollback::checkpoint_snapshot_fast	∀ st ∈ Reach: checkpoint_snapshot(st) is a fresh Scrollback with line_limit = st.line_limit() whose view is st.iter() (healthy lines in order, cut to the limit); checkpoint_snapshot_fast(st) has the same line_limit and view = the last hot+warm lines (cold excluded), i.e. snapshot.get_line(i) = st.get_line(cold + i).	6	struct built by repeated push, composes refines-seq	0,1,2,4,7
storage-delegates	refinement	ScrollbackStorage::*, From<Scrollback>, From<DiskBackedScrollback>, ScrollbackStorage::default	∀ st and delegated method m: st.m(args) = inner.m(args) for its variant (Memory returns Ok for an infallible inner call); From keeps the variant; default() = Memory(Scrollback::with_defaults()); tier_line_counts() sums to line_count(); is_disk_backed() ⇔ Disk; cold_metrics().disk_used = None for Memory and Some(cold_disk_used()) for Disk; cold_metrics().memory_used = cold_memory_used(); hot_limit/warm_limit/memory_budget/line_limit read back what was configured or last set.	14	enum dispatch macro, Result wrapping	0,1,2,3,4
hot-tier-deque	refinement	HotTier::{push,get,len,take_front,truncate_front,truncate_back}	HotTier refines Seq<Line> H: push(l): H := H ++ [l]; get(i) = H.get(i); len = |H|; take_front(k) returns take(k, H) and H := drop(k, H); truncate_front(keep): H := last(keep, H); truncate_back(n): H := take(|H| - min(n,|H|), H); bytes_used = size_of::<HotTier>() + Σ Line::memory_used(H).	3	VecDeque<Line>, Vec return, &mut self	0,1,2,7
warm-tier-refines-seq	refinement	WarmBlock::{from_lines,decompress,get_line}, WarmTier::{push_block,pop_front,push_front,truncate_front_lines,get_line,locate,segment_len_at,rebuild_cumulative}	WarmBlock::from_lines(ls).decompress() = Ok(map Strip ls) and get_line(i) = Ok(ls.get(i)); WarmTier refines V(warm): push_block appends a block's lines; pop_front removes and returns the front block (its live lines leave line_count); push_front(pop_front()) is the identity; truncate_front_lines(n) drops n lines without decoding; get_line(i) = V[i]; the amortized cumulative index (cum_start, cumulative_base) answers locate/segment_len_at/line_count as rebuild_cumulative() would; memory_used() = recompute_memory_used().	6	VecDeque<WarmBlock>, amortized index with dead-prefix compaction, Cell caches, LZ4 calls	0,1,2,4,5,7
warm-block-count-validation	contract	WarmBlock::{decompress,stored_line_count}	∀ b: b.decompress() = Ok(ls) ⇒ |ls| = b.line_count and the page's header count equals the number of complete records decoded; a page whose header declares more records than it holds returns Err naming the decoded count.	1	Result, slice header read, composes block-decode-total-bounded	0,1,4
cold-tier-refines-seq	refinement	ColdTier::{push_block,get_line,line_count,page_count,pop_front,pop_front_batch,truncate_front_lines,compressed_size}, ColdPage::{from_warm_block,decompress}	ColdTier refines V(cold) = concat(pages)[o..] with front_offset o: push_block(b) appends b's lines and returns b.line_count when b decodes, else returns 0 and changes nothing; get_line(i) = Ok(V.get(i)); truncate_front_lines(n) drops min(n, |V|) lines, freeing whole consumed pages and keeping o < |first page|; pop_front() removes the front page, returns its live count (|page| - o) and sets o := 0; pop_front_batch(k) = k × pop_front (same sum, same state); compressed_size() = recompute_compressed_size(), unchanged by a partial truncate; ColdPage::from_warm_block(b).decompress() = b's lines.	15	VecDeque<ColdPage>, cumulative index with base, LZ4 calls, &mut self	0,1,2,4,5,7
binary-search-counted	equivalence	codec::binary_search_counted, ColdTier::find_page, WarmTier::find_block, DiskColdTier::find_page	∀ strictly increasing c, target t, counter s: binary_search_counted(c, t, &mut s) = c.binary_search(&t) and adds between 1 and ⌈log2(|c|+1)⌉ to s when c ≠ [] (0 when c = [], result Err(0)); find_page/find_block(i) = Some(p) ⇔ i < line_count, with start(p) ≤ i < cumulative[p], using one such search.	3	loop with invariant left ≤ right ≤ len, &mut usize counter, slices	0,1,2,5,7
disk-cold-refines-seq	refinement	DiskColdTier::{push_compressed,get_line,truncate_front_lines,clear,compact,find_page,live_index}	DiskColdTier refines the cold-tier-refines-seq model on a file: push_compressed(c, n) appends an n-line page when c ≠ [] ∧ n > 0, else is a no-op; get_line(i) = Ok(V.get(i)) whether the page is inside the load-time mapping or past it (positional read), and appends never remap; truncate_front_lines(n) drops min(n, |V|) lines, keeps o < the first live page's line_count, and may compact; compaction preserves V whether it succeeds or fails (a failure re-maps or reopens the original file, and later appends still reach disk); compact() with no dead bytes leaves the file size unchanged; clear() empties V and resets the file to the header.	20	file + mmap state, VecDeque index with base, compaction via rename, &mut self, io::Result	0,1,2,4,5,7,8
disk-cold-reload-scan	round-trip	DiskColdTier::{with_config,load,scan_pages}	∀ .dtrm file F: with_config(F) refuses with InvalidData when the header version ≠ current; otherwise it removes orphan F.dtrm.tmp and F.dtrm.compact files, then recovers exactly the maximal prefix of pages from offset HEADER_SIZE with compressed_size > 0, line_count > 0 and lying wholly inside the file (header counts are hints only; preallocation is capped; each line_count is clamped to MAX_DECODE_PAGE_LINES). So a store written then reloaded (with or without sync/Drop, or after a crash mid-append) has V = the concatenation of its complete pages; after compaction V(reload) = V(before drop); without it V(before drop) is a suffix of V(reload).	14	parsing loop over untrusted file bytes, fs remove, mmap, u64/u32 casts	0,1,3,5,7,8
disk-backed-reload	round-trip	DiskBackedScrollback::{with_config,push_line,truncate,remove_newest,set_line_limit}, ScrollbackStorage::checkpoint_snapshot	∀ session at path P that drops with cold view Vc: a new with_config(P) has line_count = cold_line_count = |Vc|, empty hot and warm, and M = Vc (hot and warm lines are not persisted); from there every operation behaves as disk-backed-refines-seq with initial M = Vc (push appends after Vc, truncate/set_line_limit keep the newest, remove_newest drops the newest cold lines, checkpoint_snapshot reproduces Vc).	10	composes disk-cold-reload-scan with refines-seq; fs effects	0,1,2,4,8,10
disk-header-durability	contract	DiskColdTier::{create,clear,sync,push_compressed,sync_appends,compact}, Drop for DiskColdTier	∀ DiskColdTier on F: after create, clear, sync, compaction and Drop, the on-disk header (page_count, line_count) = (live pages, live lines) and is synced; clear() leaves F exactly HEADER_SIZE bytes; push_compressed rewrites the header only when the appended-bytes counter crosses SYNC_APPEND_BYTES, so header.page_count ≤ pages pushed at every instant.	6	file writes and fsync ordering, byte-counter policy	0,1,2,8
disk-cache-bounds	invariant	DiskColdTier::{cache_page,get_line}, DiskColdConfig::{with_cache_size,with_cache_byte_limit}	∀ DiskColdTier after every get_line: |cache| ≤ max(cache_size, 1); cache_bytes = Σ entry.bytes ≤ cache_byte_limit; a page whose decoded size exceeds cache_byte_limit is never inserted (it is re-decoded on each read) and reads still succeed; with_cache_size(0) is clamped to 1.	4	RefCell<HashMap> LRU, Cell counters through &self, eviction loop	0,1,2,7
disk-read-bounds-safe	no-panic	DiskColdTier::{decompress_page,get_line}	∀ DiskColdTier, page p, any PageIndexEntry (offset, compressed_size) and live file length: decompress_page(p) never reads outside the mapping or the file; offset + compressed_size beyond the mapped or current file length gives Err(Io); a tier with no backing file (DiskColdTier::new) tracks metadata only and every get_line is Err(Io).	4	raw mmap pointer + length, fs metadata re-check, unsafe slice construction	0,1,2,8
disk-file-permissions-lifecycle	contract	storage::create_dir_restricted, DiskColdTier::with_config, Drop for DiskColdTier	∀ path P: with_config(P) creates missing parent directories and the file; create_dir_restricted(D) leaves D (fresh, or pre-existing with any mode) at 0o700 on unix; dropping a tier releases its mapping and handle so the file can be removed.	4	fs and permission syscalls, Drop	0,8
mmap-wrapper-contract	contract	mmap::MmapMut::{map_mut,map_read,flush,slice,as_mut_slice}	∀ file f of length n: map_mut(f)/map_read(f) = Err if n = 0, else Ok(m) with |m| = n and *m = the file bytes; m.slice(o, l) = Some(&bytes[o..o+l]) ⇔ o+l ≤ n; writes through as_mut_slice then flush() are in the file after unmap; flush() of a read-only mapping is Ok and writes nothing.	4	unsafe FFI (mmap/msync/munmap, Windows MapViewOfFile), trust::backing spatial bounds	0,8
decode-work-bound	other (cost)	Scrollback::{push_line,truncate,remove_newest}, walk_records, count_page_lines	Over the ghost counter W (records accepted by walk_records), ∀ Scrollback full to its line limit: each push_line adds exactly 1 to W (the warm→cold framing check of the line aging out of warm); truncate adds 0; remove_newest(1) adds ≤ 2·block_size (≤ block_size for a warm boundary), independent of line_count.	2	ghost cost counter (a #[cfg(test)] thread_local today), composition through promotion and eviction	0,1,2,4,7
```

## 2. Test → spec map (TSV; all 341)

`primary` is the spec that retires the test. `also` lists other specs the same test samples. A
test in §4 has `-` as its primary and its action in `also`. Paths are relative to
`crates/aterm-scrollback`.

```tsv
file	test	primary	also
src/cold_tier_tests.rs	cold_tier_find_page_logarithmic_scaling	binary-search-counted	
src/cold_tier_tests.rs	cold_tier_multiple_pages	cold-tier-refines-seq	
src/cold_tier_tests.rs	cold_tier_push_get	cold-tier-refines-seq	
src/cold_tier_tests.rs	cold_tier_rotation_interleave_matches_oracle	cold-tier-refines-seq	
src/cold_tier_tests.rs	compression_ratio	cold-tier-refines-seq	
src/cold_tier_tests.rs	pop_front_batch_matches_individual_pop_front	cold-tier-refines-seq	
src/cold_tier_tests.rs	pop_front_batch_with_nonzero_front_offset	cold-tier-refines-seq	
src/cold_tier_tests.rs	pop_front_cost_scales_with_page_count	-	move-to-bench
src/cold_tier_tests.rs	pop_front_preserves_cumulative_invariant	cold-tier-refines-seq	
src/cold_tier_tests.rs	pop_front_with_nonzero_front_offset	cold-tier-refines-seq	
src/cold_tier_tests.rs	push_block_returns_line_count_on_success	cold-tier-refines-seq	
src/cold_tier_tests.rs	push_block_returns_zero_for_corrupt_block	cold-tier-refines-seq	store-corruption-semantics
src/cold_tier_tests.rs	truncate_back_lines_aborts_on_corrupt_boundary_page	failure-atomicity	
src/cold_tier_tests.rs	truncate_front_lines_crosses_pages	cold-tier-refines-seq	
src/cold_tier_tests.rs	truncate_front_lines_exact_page_boundary	cold-tier-refines-seq	
src/cold_tier_tests.rs	truncate_front_lines_partial_page	cold-tier-refines-seq	
src/cold_tier_tests.rs	truncate_front_lines_then_push_block	cold-tier-refines-seq	
src/cold_tier_tests.rs	truncate_front_lines_updates_compressed_size	cold-tier-refines-seq	
src/decode_fuzz_tests.rs	fuzz_deserialize_lines_never_panics	block-decode-total-bounded	
src/decode_fuzz_tests.rs	fuzz_line_deserialize_never_panics	line-decode-total-capped	
src/decode_fuzz_tests.rs	fuzz_lz4_decompress_never_panics	lz4-decode-classification	
src/decode_fuzz_tests.rs	fuzz_lz4_then_deserialize_round_path_never_panics	block-decode-total-bounded	
src/disk_backed_reload_tests.rs	disk_backed_cold_data_persists_on_reload	disk-backed-reload	
src/disk_backed_reload_tests.rs	disk_backed_reload_line_count_matches_cold	disk-backed-reload	
src/disk_backed_reload_tests.rs	disk_backed_reload_push_increments_correctly	disk-backed-reload	
src/disk_backed_reload_tests.rs	disk_backed_reload_remove_newest_preserves_oldest	disk-backed-reload	
src/disk_backed_reload_tests.rs	disk_backed_reload_set_line_limit_truncates_immediately	disk-backed-reload	
src/disk_backed_reload_tests.rs	disk_backed_reload_truncate_keeps_newest	disk-backed-reload	
src/disk_backed_tests.rs	disk_backed_eager_promotion_fires_under_pressure	eager-promotion-at-yellow	
src/disk_backed_tests.rs	disk_backed_incremental_bytes_used_matches_recomputed	memory-accounting-exact	
src/disk_backed_tests.rs	disk_backed_push_with_line_limit_keeps_the_newest_lines	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_remove_newest_all_lines	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_remove_newest_hot_only	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_remove_newest_memory_bounded_cross_tier	-	move-to-bench
src/disk_backed_tests.rs	disk_backed_remove_newest_partial_warm_block	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_remove_newest_slow_path_restores_tier_structure	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_remove_newest_spans_cold	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_remove_newest_spans_warm	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_scrollback_cold_memory_used	memory-accounting-exact	
src/disk_backed_tests.rs	disk_backed_scrollback_evict_warm_cold_no_data_loss	disk-backed-refines-seq	tier-shape-invariant
src/disk_backed_tests.rs	disk_backed_scrollback_line_limit_default_is_safe_cap	tier-shape-invariant	
src/disk_backed_tests.rs	disk_backed_scrollback_line_limit_enforced_on_push	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_scrollback_line_limit_zero_disables	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_scrollback_opt_in_unlimited_lines	tier-shape-invariant	
src/disk_backed_tests.rs	disk_backed_scrollback_set_line_limit	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_scrollback_set_line_limit_truncates_immediately	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_scrollback_total_memory_used	memory-accounting-exact	
src/disk_backed_tests.rs	disk_backed_tier_aware_truncate_avoids_rebuild	truncate-oldest-tier-first	
src/disk_backed_tests.rs	disk_backed_truncate_medium_path_restores_tier_structure	disk-backed-refines-seq	
src/disk_backed_tests.rs	disk_backed_truncate_slow_path_restores_tier_structure	disk-backed-refines-seq	
src/disk_backed_tests.rs	push_str_get_line_roundtrip	disk-backed-refines-seq	
src/disk_backed_truncate.rs	a_failed_cold_write_preserves_newer_tiers_and_their_accounting	failure-atomicity	
src/disk_compaction_tests.rs	disk_cold_compaction_idempotent	disk-cold-refines-seq	
src/disk_compaction_tests.rs	disk_cold_compaction_preserves_data	disk-cold-refines-seq	
src/disk_compaction_tests.rs	disk_cold_compaction_triggers_on_truncate	disk-header-durability	
src/disk_compaction_tests.rs	disk_cold_compaction_trims_front_offset	disk-cold-reload-scan	disk-cold-refines-seq
src/disk_compaction_tests.rs	disk_cold_load_cleans_orphan_compact_temp_file	disk-cold-reload-scan	
src/disk_compaction_tests.rs	failed_compaction_keeps_cold_reads_working	disk-cold-refines-seq	
src/disk_tests.rs	corrupt_page_count_header_does_not_panic_on_reload	disk-cold-reload-scan	
src/disk_tests.rs	disk_cold_append_barrier_fires_on_byte_policy	disk-header-durability	
src/disk_tests.rs	disk_cold_append_defers_header_until_barrier	disk-header-durability	
src/disk_tests.rs	disk_cold_append_leaves_map_extent_reads_still_work	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_cache_byte_limit_is_enforced	disk-cache-bounds	
src/disk_tests.rs	disk_cold_cache_size_zero_no_infinite_loop	disk-cache-bounds	
src/disk_tests.rs	disk_cold_clear	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_clear_header_durable_on_disk	disk-header-durability	
src/disk_tests.rs	disk_cold_clear_persists_empty_header	disk-cold-reload-scan	disk-header-durability
src/disk_tests.rs	disk_cold_clear_remap	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_compaction_survives_blocked_rename	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_crash_before_barrier_recovers_from_page_scan	disk-cold-reload-scan	
src/disk_tests.rs	disk_cold_crash_recovery_partial_page_discarded	disk-cold-reload-scan	
src/disk_tests.rs	disk_cold_crash_recovery_truncated_header	disk-cold-reload-scan	
src/disk_tests.rs	disk_cold_create_header_durable_on_disk	disk-header-durability	
src/disk_tests.rs	disk_cold_decompress_external_truncation_returns_err	disk-read-bounds-safe	
src/disk_tests.rs	disk_cold_decompress_oob_length_returns_err	disk-read-bounds-safe	
src/disk_tests.rs	disk_cold_decompress_oob_offset_returns_err	disk-read-bounds-safe	
src/disk_tests.rs	disk_cold_drop_releases_mmap	disk-file-permissions-lifecycle	
src/disk_tests.rs	disk_cold_file_create	disk-file-permissions-lifecycle	
src/disk_tests.rs	disk_cold_file_reload	disk-cold-reload-scan	
src/disk_tests.rs	disk_cold_get_line	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_get_line_cache_hit_single_extraction	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_in_memory	disk-read-bounds-safe	
src/disk_tests.rs	disk_cold_lru_cache	disk-cache-bounds	
src/disk_tests.rs	disk_cold_push_compressed_empty_is_noop	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_push_compressed_header_consistent	disk-header-durability	
src/disk_tests.rs	disk_cold_push_compressed_state_consistency	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_reload_and_read	disk-cold-reload-scan	
src/disk_tests.rs	disk_cold_rotation_interleave_matches_oracle	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_skips_caching_an_oversized_page	disk-cache-bounds	
src/disk_tests.rs	disk_cold_truncate_front_lines_all	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_truncate_front_lines_crosses_page	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_truncate_front_lines_exact_boundary	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_truncate_front_lines_incremental	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_truncate_front_lines_partial_page	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_truncate_multi_page_drain	disk-cold-refines-seq	
src/disk_tests.rs	disk_cold_truncate_then_push	disk-cold-refines-seq	
src/disk_tests.rs	find_page_and_front_truncation_over_every_small_layout	disk-cold-refines-seq	
src/disk_tests.rs	scan_pages_bounds_prealloc_on_large_sparse_file	disk-cold-reload-scan	
src/disk_tests.rs	scan_pages_clamps_legacy_oversized_page_line_count	disk-cold-reload-scan	
src/disk_tests.rs	scan_pages_stops_at_zero_line_page	disk-cold-reload-scan	
src/disk_tests.rs	version_1_store_is_refused_at_load	disk-cold-reload-scan	
src/error.rs	dec_string_matches_display	dec-string-decimal	
src/error.rs	display_messages_are_stable	-	delete
src/error.rs	io_source_is_preserved	error-source-io-only	
src/line_tests.rs	a_line_reports_the_footprint_tile_each_of_its_columns_paints	span-column-lookup	
src/line_tests.rs	a_line_with_no_image_serializes_exactly_as_it_did_before_images_existed	line-wire-version	
src/line_tests.rs	a_link_dense_line_over_the_cap_keeps_its_text_and_loses_only_its_links	stripping-decode-refines-strict	
src/line_tests.rs	a_span_starting_mid_footprint_keeps_its_tile_offsets	span-column-lookup	
src/line_tests.rs	attr_cursor_matches_get_attr_everywhere	attr-lookup-and-cursor	
src/line_tests.rs	cell_attrs_default	cell-attrs-codec-mask	
src/line_tests.rs	cell_attrs_from_raw_preserves_main_cell_wide_geometry	cell-attrs-codec-mask	
src/line_tests.rs	cell_attrs_serialize_roundtrip	cell-attrs-codec-mask	
src/line_tests.rs	clearing_hyperlinks_drops_the_links_and_keeps_everything_else	clear-hyperlinks-frame	
src/line_tests.rs	deserialize_caps_hyperlink_span_count	line-decode-total-capped	
src/line_tests.rs	deserialize_caps_oversized_hyperlink_url_and_id	line-decode-total-capped	
src/line_tests.rs	deserialize_lines_bounds_capacity_for_inflated_count	block-decode-total-bounded	
src/line_tests.rs	deserialize_lines_capped_bounds_reconstructed_count	capped-decode-prefix	
src/line_tests.rs	deserialize_lines_count_exceeds_actual	block-decode-total-bounded	
src/line_tests.rs	deserialize_lines_malicious_count_over_allocates	block-decode-total-bounded	
src/line_tests.rs	deserialize_lines_truncated_header	block-decode-total-bounded	
src/line_tests.rs	deserialize_lines_zero_count	block-decode-total-bounded	
src/line_tests.rs	deserialize_rejects_oversized_line_content	line-decode-total-capped	
src/line_tests.rs	dropping_one_line_of_a_footprint_drops_exactly_its_share	image-memory-share	
src/line_tests.rs	fuzz_crash_4950_line_codec_no_overflow	block-decode-total-bounded	
src/line_tests.rs	fuzz_deserialize_lines_never_panics_on_corrupt_disk_data	block-decode-total-bounded	
src/line_tests.rs	fuzz_regression_large_content_len_no_overflow	block-decode-total-bounded	
src/line_tests.rs	fuzz_regression_rle_run_length_overflow	line-decode-total-capped	
src/line_tests.rs	fuzz_serialize_deserialize_roundtrip	block-roundtrip	line-wire-roundtrip
src/line_tests.rs	hyperlink_span_contains	span-column-lookup	
src/line_tests.rs	hyperlink_span_serialized_size	serialized-size-exact	
src/line_tests.rs	line_content_heap	line-constructors-normalize	
src/line_tests.rs	line_content_inline	line-constructors-normalize	
src/line_tests.rs	line_empty	line-constructors-normalize	
src/line_tests.rs	line_from_str	line-constructors-normalize	
src/line_tests.rs	line_hyperlink_serialize_roundtrip	line-wire-roundtrip	
src/line_tests.rs	line_hyperlink_with_id_serialize_roundtrip	line-wire-roundtrip	
src/line_tests.rs	line_memory_used	-	delete
src/line_tests.rs	line_no_hyperlinks	line-constructors-normalize	
src/line_tests.rs	line_serialize_roundtrip	line-wire-roundtrip	
src/line_tests.rs	line_serialize_roundtrip_with_attrs	line-wire-roundtrip	
src/line_tests.rs	line_underline_color_serialize_roundtrip	line-wire-roundtrip	
src/line_tests.rs	line_underline_colors_and_hyperlinks_roundtrip_together	line-wire-roundtrip	
src/line_tests.rs	line_underline_colors_no_hyperlinks_roundtrip	line-wire-roundtrip	
src/line_tests.rs	line_with_attrs	attr-lookup-and-cursor	
src/line_tests.rs	line_with_attrs_all_default	line-constructors-normalize	
src/line_tests.rs	line_with_hyperlinks_basic	span-column-lookup	
src/line_tests.rs	line_with_multiple_hyperlinks	span-column-lookup	
src/line_tests.rs	line_without_underline_colors_stays_v3	line-wire-version	
src/line_tests.rs	line_wrapped_flag	line-constructors-normalize	
src/line_tests.rs	record_is_valid_agrees_with_deserialize_on_malformed_records	record-is-valid-mirrors-deserialize	
src/line_tests.rs	serialization_drops_the_image_and_keeps_everything_else	line-wire-roundtrip	
src/line_tests.rs	serialize_lines_capacity_covers_plain_text	serialized-size-exact	
src/line_tests.rs	serialize_lines_capacity_covers_styled_hyperlinked_rows	serialized-size-exact	
src/line_tests.rs	serialize_lines_empty_and_single	block-roundtrip	
src/line_tests.rs	serialize_lines_roundtrip	block-roundtrip	
src/line_tests.rs	serialize_lines_roundtrip_with_attrs	block-roundtrip	
src/line_tests.rs	serialize_lines_underline_colors_block_framing	block-roundtrip	
src/line_tests.rs	serialize_lines_with_hyperlinks_roundtrip	block-roundtrip	
src/line_tests.rs	strict_checkpoint_decoder_admits_a_full_styled_row_with_a_combining_mark	strict-decode-contract	
src/line_tests.rs	strict_checkpoint_decoder_admits_a_full_styled_row_with_a_zwj_emoji	strict-decode-contract	
src/line_tests.rs	strict_checkpoint_decoder_keeps_span_columns_bounded_by_cols	strict-decode-contract	
src/line_tests.rs	strict_checkpoint_decoder_rejects_attr_run_beyond_columns	strict-decode-contract	
src/line_tests.rs	strict_checkpoint_decoder_rejects_excess_rows_before_reconstruction	strict-decode-contract	
src/line_tests.rs	strict_checkpoint_decoder_still_refuses_attrs_beyond_both_bounds	strict-decode-contract	
src/line_tests.rs	the_link_ceiling_is_the_record_cap_plus_one_maximal_link_per_column	link-ceiling-formula	
src/line_tests.rs	the_rows_of_one_footprint_together_charge_the_payload_exactly_once	image-memory-share	
src/line_tests.rs	the_stripping_decoder_reads_whatever_the_strict_one_admits_unchanged	stripping-decode-refines-strict	
src/line_tests.rs	the_stripping_decoder_refuses_every_record_its_links_do_not_explain	stripping-decode-refines-strict	
src/lz4.rs	garbage_after_valid_prefix_errors_not_panics	lz4-decode-classification	
src/lz4.rs	small_claims_decode_to_their_claim_or_fail	lz4-decode-classification	
src/lz4.rs	test_compression_bomb_rejected_tiny_body_huge_claim	lz4-decode-classification	
src/lz4.rs	test_compression_bomb_rejected_under_limit	lz4-decode-classification	
src/lz4.rs	test_cross_impl_compatibility_with_aterm_lz4	lz4-roundtrip	
src/lz4.rs	test_decompress_too_short	lz4-decode-classification	
src/lz4.rs	test_legitimate_high_ratio_still_accepted	lz4-roundtrip	
src/lz4.rs	test_max_decompressed_size_at_limit	lz4-roundtrip	
src/lz4.rs	test_max_decompressed_size_rejection	lz4-decode-classification	
src/lz4.rs	test_ratio_boundary_exactly_at_limit_accepted	lz4-decode-classification	
src/lz4.rs	zero_size_prefix_empty_body_is_ok	lz4-decode-classification	
src/memory_tracking_tests.rs	disk_backed_budget_ignores_cold_cache_population	memory-accounting-exact	
src/memory_tracking_tests.rs	disk_backed_budget_ignores_warm_cache_population	memory-accounting-exact	
src/memory_tracking_tests.rs	scrollback_budget_ignores_warm_cache_population	memory-accounting-exact	
src/memory_tracking_tests.rs	scrollback_budgeted_bytes_tracks_through_operations	memory-accounting-exact	
src/memory_tracking_tests.rs	scrollback_incremental_bytes_used_matches_recomputed	memory-accounting-exact	
src/mmap.rs	map_empty_file_fails	mmap-wrapper-contract	
src/mmap.rs	map_mut_write_and_flush	mmap-wrapper-contract	
src/mmap.rs	map_read_roundtrip_and_flush_is_noop	mmap-wrapper-contract	
src/mmap.rs	map_roundtrip	mmap-wrapper-contract	
src/performance_tests.rs	push_with_line_limit_constant_time_truncate	decode-work-bound	
src/performance_tests.rs	remove_newest_all_lines	scrollback-refines-seq	
src/performance_tests.rs	remove_newest_hot_only	scrollback-refines-seq	
src/performance_tests.rs	remove_newest_memory_bounded_cross_tier	-	move-to-bench
src/performance_tests.rs	remove_newest_partial_warm_block	scrollback-refines-seq	
src/performance_tests.rs	remove_newest_slow_path_scales_with_kept_lines	scrollback-refines-seq	tier-shape-invariant
src/performance_tests.rs	remove_newest_spans_cold	scrollback-refines-seq	
src/performance_tests.rs	remove_newest_spans_warm	scrollback-refines-seq	
src/performance_tests.rs	remove_newest_work_sublinear_in_total_size	decode-work-bound	scrollback-refines-seq
src/performance_tests.rs	truncate_slow_path_scales_with_kept_lines	scrollback-refines-seq	tier-shape-invariant
src/storage.rs	create_dir_restricted_makes_fresh_dir_0700	disk-file-permissions-lifecycle	
src/storage.rs	create_dir_restricted_tightens_preexisting_loose_dir	disk-file-permissions-lifecycle	
src/storage_tests.rs	checkpoint_snapshot_fast_all_hot_preserves_lines	checkpoint-snapshots	
src/storage_tests.rs	checkpoint_snapshot_fast_disk_backed_skips_cold	checkpoint-snapshots	
src/storage_tests.rs	checkpoint_snapshot_fast_empty_scrollback	checkpoint-snapshots	
src/storage_tests.rs	checkpoint_snapshot_fast_preserves_line_limit	checkpoint-snapshots	
src/storage_tests.rs	checkpoint_snapshot_fast_skips_cold_tier_lines	checkpoint-snapshots	
src/storage_tests.rs	dense_walk_borrows_hot_lines	hot-reads-borrow	
src/storage_tests.rs	dense_walk_matches_per_line_oracle_across_tiers_and_front_truncation	iter-and-dense-walk-match-get-line	
src/storage_tests.rs	dense_walk_matches_per_line_oracle_disk	iter-and-dense-walk-match-get-line	
src/storage_tests.rs	dense_walk_yields_placeholders_for_a_corrupt_segment_and_keeps_coordinates	iter-and-dense-walk-match-get-line	
src/storage_tests.rs	get_line_returns_borrowed_for_hot_tier	hot-reads-borrow	
src/storage_tests.rs	scrollback_storage_default	storage-delegates	
src/storage_tests.rs	scrollback_storage_disk_clear	storage-delegates	
src/storage_tests.rs	scrollback_storage_disk_get_line_rev	storage-delegates	
src/storage_tests.rs	scrollback_storage_disk_iter	iter-and-dense-walk-match-get-line	
src/storage_tests.rs	scrollback_storage_disk_memory_budget	storage-delegates	
src/storage_tests.rs	scrollback_storage_disk_push_and_get	storage-delegates	
src/storage_tests.rs	scrollback_storage_disk_tier_metrics	storage-delegates	
src/storage_tests.rs	scrollback_storage_from_scrollback	storage-delegates	
src/storage_tests.rs	scrollback_storage_memory_budget	storage-delegates	
src/storage_tests.rs	scrollback_storage_memory_clear	storage-delegates	
src/storage_tests.rs	scrollback_storage_memory_get_line_rev	storage-delegates	
src/storage_tests.rs	scrollback_storage_memory_iter	iter-and-dense-walk-match-get-line	
src/storage_tests.rs	scrollback_storage_memory_line_limit	storage-delegates	
src/storage_tests.rs	scrollback_storage_memory_push_and_get	storage-delegates	
src/storage_tests.rs	scrollback_storage_memory_tier_metrics	storage-delegates	
src/storage_tests.rs	streaming_iter_matches_get_line_walk_disk	iter-and-dense-walk-match-get-line	
src/storage_tests.rs	streaming_iter_matches_get_line_walk_memory	iter-and-dense-walk-match-get-line	
src/storage_tests.rs	streaming_iter_skips_corrupt_block_with_same_totals	iter-and-dense-walk-match-get-line	
src/storage_tests.rs	test_corrupt_warm_block_quarantined_after_threshold	store-corruption-semantics	
src/storage_tests.rs	test_iteration_complete_after_quarantine	iter-and-dense-walk-match-get-line	store-corruption-semantics
src/storage_tests.rs	test_line_limit_enforced_after_quarantine	store-corruption-semantics	
src/tests/basic.rs	scrollback_clear	scrollback-refines-seq	
src/tests/basic.rs	scrollback_eviction	scrollback-refines-seq	tier-shape-invariant
src/tests/basic.rs	scrollback_get_line	scrollback-refines-seq	
src/tests/basic.rs	scrollback_get_line_rev	scrollback-refines-seq	
src/tests/basic.rs	scrollback_iterator	iter-and-dense-walk-match-get-line	
src/tests/basic.rs	scrollback_new	tier-shape-invariant	
src/tests/basic.rs	scrollback_promotion	scrollback-refines-seq	tier-shape-invariant
src/tests/basic.rs	scrollback_push_line	scrollback-refines-seq	
src/tests/basic.rs	tier_transition_content_fidelity_multi_cycle	scrollback-refines-seq	
src/tests/decompression.rs	corrupt_warm_block_quarantined_after_threshold	store-corruption-semantics	
src/tests/decompression.rs	corrupt_warm_block_quarantines_only_on_final_eviction_attempt	store-corruption-semantics	
src/tests/decompression.rs	decode_cold_bounded_roundtrip	lz4-roundtrip	
src/tests/decompression.rs	decompress_lz4_bounded_rejects_oversized_prefix	lz4-decode-classification	
src/tests/decompression.rs	decompress_lz4_bounded_rejects_too_short	lz4-decode-classification	
src/tests/decompression.rs	decompress_lz4_bounded_roundtrip	lz4-roundtrip	
src/tests/decompression.rs	evict_warm_to_cold_preserves_trimmed_corrupt_block_until_quarantine	store-corruption-semantics	
src/tests/decompression.rs	evict_warm_to_cold_restores_block_on_failure	store-corruption-semantics	
src/tests/decompression.rs	iterator_skips_quarantined_block_without_redundant_decompression	iter-and-dense-walk-match-get-line	store-corruption-semantics
src/tests/decompression.rs	line_limit_enforced_past_corrupt_block	store-corruption-semantics	
src/tests/decompression.rs	quarantine_mid_history_keeps_rows_and_counts_the_loss	store-corruption-semantics	
src/tests/decompression.rs	remove_newest_aborts_on_decompression_failure	failure-atomicity	
src/tests/decompression.rs	set_memory_budget_reports_cold_rejection	failure-atomicity	budget-enforcement-fifo
src/tests/decompression.rs	truncate_succeeds_on_corrupt_warm_block	failure-atomicity	
src/tests/decompression.rs	truncate_three_tier_succeeds_with_corrupt_warm_block	failure-atomicity	
src/tests/generated.rs	scrollback_fifo_ordering	scrollback-refines-seq	
src/tests/generated.rs	scrollback_line_limit_enforced	scrollback-refines-seq	
src/tests/generated.rs	scrollback_order_preserved	scrollback-refines-seq	
src/tests/generated.rs	scrollback_tier_content_preserved	scrollback-refines-seq	
src/tests/image_horizon.rs	a_footprint_straddling_the_horizon_ages_out_from_its_top	image-retention-horizon	
src/tests/image_horizon.rs	a_footprints_payload_leaves_the_byte_budget_when_its_rows_cross	image-retention-horizon	memory-accounting-exact, image-memory-share
src/tests/image_horizon.rs	a_picture_is_retained_for_exactly_as_long_as_its_line_is_uncompressed	image-retention-horizon	
src/tests/image_horizon.rs	crossing_the_horizon_costs_the_line_its_picture_and_nothing_else	image-retention-horizon	
src/tests/image_horizon.rs	the_disk_backed_store_ends_a_picture_at_compression_not_at_the_disk	image-retention-horizon	
src/tests/image_horizon.rs	the_horizon_count_never_decreases_and_survives_a_clear	image-retention-horizon	
src/tests/image_horizon.rs	the_horizon_is_reported_only_for_lines_that_actually_carried_a_picture	image-retention-horizon	
src/tests/line_limit.rs	binary_search_counted_matches_stdlib	binary-search-counted	
src/tests/line_limit.rs	scrollback_default_caps_runaway_push_from_oldest	scrollback-refines-seq	
src/tests/line_limit.rs	scrollback_line_limit_bounds_massive_push	scrollback-refines-seq	
src/tests/line_limit.rs	scrollback_line_limit_default_is_safe_cap	tier-shape-invariant	
src/tests/line_limit.rs	scrollback_line_limit_enforced_on_push	scrollback-refines-seq	
src/tests/line_limit.rs	scrollback_line_limit_evicts_from_oldest_tier_first	truncate-oldest-tier-first	
src/tests/line_limit.rs	scrollback_line_limit_zero_disables	scrollback-refines-seq	
src/tests/line_limit.rs	scrollback_set_line_limit	scrollback-refines-seq	
src/tests/line_limit.rs	scrollback_set_line_limit_truncates_immediately	scrollback-refines-seq	
src/tests/memory_budget.rs	cold_eviction_preserves_newest_lines	budget-enforcement-fifo	
src/tests/memory_budget.rs	over_budget_false_after_cold_eviction	budget-enforcement-fifo	
src/tests/memory_budget.rs	pressure_evictions_are_counted_and_limit_truncation_is_not	budget-enforcement-fifo	
src/tests/memory_budget.rs	push_line_triggers_cold_eviction_when_over_budget	budget-enforcement-fifo	
src/tests/memory_budget.rs	scrollback_cold_memory_used	memory-accounting-exact	
src/tests/memory_budget.rs	scrollback_memory_tracking	memory-accounting-exact	
src/tests/memory_budget.rs	scrollback_total_memory_used	memory-accounting-exact	
src/tests/memory_budget.rs	set_memory_budget_evicts_cold_pages	budget-enforcement-fifo	
src/tests/repaired_trimmed.rs	repaired_trimmed_warm_block_keeps_logical_suffix	store-corruption-semantics	
src/tests/threading.rs	cell_failure_counting_works_through_mutex_across_threads	-	delete
src/tests/threading.rs	scrollback_is_send_but_not_sync	-	delete
src/tests/threading.rs	scrollback_moves_between_threads_when_guarded_by_mutex	-	delete
src/tests/truncation.rs	remove_newest_slow_path_restores_tier_structure	tier-shape-invariant	
src/tests/truncation.rs	scrollback_truncate	scrollback-refines-seq	
src/tests/truncation.rs	scrollback_truncate_cross_tier	scrollback-refines-seq	
src/tests/truncation.rs	scrollback_truncate_zero	scrollback-refines-seq	
src/tests/truncation.rs	truncate_slow_path_restores_tier_structure	tier-shape-invariant	
src/tier_capabilities.rs	capabilities_track_the_built_features	-	delete
src/tier_decode_tests.rs	read_failures_reach_quarantine_without_manual_record_failure	warm-block-quarantine-fsm	
src/tier_decode_tests.rs	warm_block_decompress_rejects_truncated_serialized_payload	warm-block-count-validation	
src/tier_decode_tests.rs	warm_block_successful_decompress_clears_failure_streak	warm-block-quarantine-fsm	
src/tier_tests.rs	block_size_clamped_to_decode_page_cap	tier-shape-invariant	
src/tier_tests.rs	hot_tier_push_get	hot-tier-deque	
src/tier_tests.rs	hot_tier_take_front	hot-tier-deque	
src/tier_tests.rs	hot_tier_truncate_front	hot-tier-deque	
src/tier_tests.rs	rows_of_links_past_the_page_cap_age_out_whole	scrollback-refines-seq	line-wire-roundtrip
src/tier_tests.rs	warm_block_decompress_rejects_oversized_lz4_prefix	lz4-decode-classification	
src/tier_tests.rs	warm_block_get_line	warm-tier-refines-seq	
src/tier_tests.rs	warm_block_roundtrip	warm-tier-refines-seq	
src/tier_tests.rs	warm_block_to_cold_rejects_oversized_lz4_prefix	lz4-decode-classification	
src/tier_tests.rs	warm_tier_cumulative_index_matches_rebuild_across_front_and_back_ops	warm-tier-refines-seq	
src/tier_tests.rs	warm_tier_find_block_logarithmic_scaling	binary-search-counted	
src/tier_tests.rs	warm_tier_memory_used_includes_cache	memory-accounting-exact	
src/tier_tests.rs	warm_tier_pop_front	warm-tier-refines-seq	
src/tier_tests.rs	warm_tier_push_front_restores_popped_block	warm-tier-refines-seq	
src/tier_tests.rs	warm_tier_push_get	warm-tier-refines-seq	
src/watermark_tests.rs	thresholds_are_bounded_for_every_budget	threshold-bytes-bounds	
src/watermark_tests.rs	watermark_boundary_at_exact_red_entry	watermark-transition-table	
src/watermark_tests.rs	watermark_boundary_at_exact_yellow_entry	watermark-transition-table	
src/watermark_tests.rs	watermark_clear_resets_to_green	store-watermark-invariant	
src/watermark_tests.rs	watermark_compressible_data_stays_green_longer	-	delete
src/watermark_tests.rs	watermark_crosses_red_under_pressure	store-watermark-invariant	
src/watermark_tests.rs	watermark_crosses_yellow_under_pressure	store-watermark-invariant	
src/watermark_tests.rs	watermark_custom_thresholds_lower_yellow	store-watermark-invariant	
src/watermark_tests.rs	watermark_default_is_green	-	delete
src/watermark_tests.rs	watermark_eager_promotion_fires_under_pressure	eager-promotion-at-yellow	
src/watermark_tests.rs	watermark_green_at_exact_yellow_exit_stays_yellow	watermark-transition-table	
src/watermark_tests.rs	watermark_level_ord_is_green_yellow_red	-	delete
src/watermark_tests.rs	watermark_not_over_budget_after_sustained_push	budget-enforcement-fifo	store-watermark-invariant
src/watermark_tests.rs	watermark_red_drops_to_yellow_between_yellow_entry_and_red	watermark-transition-table	
src/watermark_tests.rs	watermark_red_drops_to_yellow_not_green	watermark-transition-table	
src/watermark_tests.rs	watermark_red_to_yellow_to_green_cascade	watermark-transition-table	
src/watermark_tests.rs	watermark_returns_to_green_after_budget_increase	store-watermark-invariant	
src/watermark_tests.rs	watermark_starts_green	store-watermark-invariant	
src/watermark_tests.rs	watermark_storage_delegates_green	storage-delegates	
src/watermark_tests.rs	watermark_storage_reflects_pressure	store-watermark-invariant	
src/watermark_tests.rs	watermark_visits_all_three_levels	store-watermark-invariant	
src/watermark_tests.rs	watermark_yellow_drops_to_green_below_exit	watermark-transition-table	
src/watermark_tests.rs	watermark_yellow_sticky_between_exit_and_entry	watermark-transition-table	
tests/disk_cold_safety.rs	disk_cold_checkpoint_snapshot_after_reload_preserves_cold_lines	checkpoint-snapshots	disk-backed-reload
tests/disk_cold_safety.rs	disk_cold_data_persists_on_reload	disk-backed-reload	disk-cold-reload-scan
tests/disk_cold_safety.rs	disk_cold_load_cleans_orphan_compact_temp_file	disk-cold-reload-scan	
tests/disk_cold_safety.rs	disk_cold_push_after_reload_increments_from_cold_base	disk-backed-reload	
tests/disk_cold_safety.rs	disk_cold_remove_newest_after_reload_preserves_remaining_history	disk-backed-reload	
tests/disk_cold_safety.rs	disk_cold_set_line_limit_after_reload_truncates_oldest	disk-backed-reload	
tests/disk_cold_safety.rs	disk_cold_truncate_back_corrupt_boundary_preserves_state	failure-atomicity	
```

## 3. Coverage

- 341 tests read. 329 map to one of the 53 specs. 12 are not spec samples (§4).
- The biggest collapses: `scrollback-refines-seq` (28 primary), `disk-cold-refines-seq` (20),
  `disk-backed-refines-seq` (15), `cold-tier-refines-seq` (15), `storage-delegates` (14),
  `disk-cold-reload-scan` (14), `lz4-decode-classification` (13), `memory-accounting-exact` (12),
  `iter-and-dense-walk-match-get-line` (11).
- `hot-tier-deque`, `warm-tier-refines-seq` and `cold-tier-refines-seq` are the lemmas the store
  refinement `scrollback-refines-seq` is built on. Once they hold, the store refinement (28 tests),
  `checkpoint-snapshots`, `iter-and-dense-walk-match-get-line` and `storage-delegates` follow by
  composition (gap 4).
- About 105 tests (the `disk_*` files, `mmap.rs`, `storage.rs`, `disk_cold_safety.rs` and the
  `disk`-named tests elsewhere) touch the file system. Their specs wait on gap 8. A pure core can be factored
  out of `scan_pages` (a parser over a byte slice) and proved first.

## 4. Tests that are not spec samples

Status (2026-10-16): deleted `line_memory_used`, `capabilities_track_the_built_features`, `display_messages_are_stable`, `watermark_default_is_green`, `watermark_compressible_data_stays_green_longer`, `scrollback_moves_between_threads_when_guarded_by_mutex`, `cell_failure_counting_works_through_mutex_across_threads`. Not yet deleted: `watermark_level_ord_is_green_yellow_red`, `scrollback_is_send_but_not_sync` (awaits its static assertion), and the move-to-bench rows.

| test | why | action |
|---|---|---|
| `src/line_tests.rs::line_memory_used` | A tautology: `memory_used() ≥ size_of::<Line>() > 0` for every line, so the assertion cannot fail. The real property is `memory-accounting-exact` and `image-memory-share`. | **delete** |
| `src/tier_capabilities.rs::capabilities_track_the_built_features` | A tautology: `TierCapabilities::current().disk_spill` is literally `cfg!(feature = "disk-tier")`, and the test compares that expression with itself. | **delete** |
| `src/error.rs::display_messages_are_stable` | Pins message prose. Its only general content (`dec_string` renders like `Display`) is `dec-string-decimal`. | **delete** |
| `src/watermark_tests.rs::watermark_default_is_green` | The `#[default]` attribute on `Green` already guarantees it; the test restates the derive. | **delete** |
| `src/watermark_tests.rs::watermark_level_ord_is_green_yellow_red` | Derived `PartialOrd`/`Ord` follow declaration order; the type system guarantees it. | **delete** |
| `src/watermark_tests.rs::watermark_compressible_data_stays_green_longer` | Compares LZ4 effectiveness on two data sets with `>=`. It samples nothing beyond `store-watermark-invariant` and depends on compressor heuristics. | **delete** |
| `src/tests/threading.rs::scrollback_is_send_but_not_sync` | `Send` and `!Sync` are auto-trait facts the compiler decides from field types. A compile-time static assertion states it with no test fn. | **delete** (replace with a static assertion) |
| `src/tests/threading.rs::scrollback_moves_between_threads_when_guarded_by_mutex` | Exercises std `Mutex`/`thread::spawn`. The content it checks is one sample of `scrollback-refines-seq`. | **delete** |
| `src/tests/threading.rs::cell_failure_counting_works_through_mutex_across_threads` | Duplicates `read_failures_reach_quarantine_without_manual_record_failure` (`warm-block-quarantine-fsm`) through a `Mutex`. The `Cell` is only touched under the lock, so the thread adds nothing. | **delete** |
| `src/cold_tier_tests.rs::pop_front_cost_scales_with_page_count` | A wall-clock ratio (best of 5, at most 20× for 10× pages). It is timing-dependent and states no functional property. Its correctness half is `pop_front_batch_matches_individual_pop_front`. | **move-to-bench** |
| `src/performance_tests.rs::remove_newest_memory_bounded_cross_tier` | A memory-ratio heuristic (after/before < 1.5×) over a diagnostic counter that includes caches. Its correctness half is `scrollback-refines-seq`; the bound it means is `decode-work-bound`. | **move-to-bench** |
| `src/disk_backed_tests.rs::disk_backed_remove_newest_memory_bounded_cross_tier` | The disk twin of the heuristic above. Its correctness half is `disk-backed-refines-seq`. | **move-to-bench** |

Counts: 9 **delete**, 3 **move-to-bench**. aterm-bench already owns throughput, so a move-to-bench
row becomes a bench there, or is deleted if that bench already covers it.

## 5. Implied specs that look wrong, or disagree with the code

1. **A test is weaker than its name.**
   `tests/disk_cold_safety.rs::disk_cold_truncate_back_corrupt_boundary_preserves_state` promises
   "fails without changing persisted state" but accepts Ok or Err. On Err it checks only that the file
   did not grow, and on Ok only `get_line(0)`. After reload, hot and warm are empty, so `remove_count`
   is 3 cold lines and must reach the corrupted last page. The implied spec (`failure-atomicity`) is a
   deterministic Err with line count, tier counts and view unchanged.
2. **Mislabelled limit tests.** `src/tests/line_limit.rs::scrollback_line_limit_zero_disables` and
   `src/disk_backed_tests.rs::disk_backed_scrollback_line_limit_zero_disables`: `Some(0)` does not
   disable the limit, it keeps zero lines (`truncate(0)` clears). `None` disables it. The specs
   (`scrollback-refines-seq`, `disk-backed-refines-seq`) state the real behaviour.
3. **"Scales with" tests that measure no scaling.**
   `src/performance_tests.rs::truncate_slow_path_scales_with_kept_lines` and
   `remove_newest_slow_path_scales_with_kept_lines` claim a scaling proof but assert only first/last-line
   correctness. They are plain samples of `scrollback-refines-seq`. The cost claim is `decode-work-bound`.
4. **An unmeasured claim and a missed invariant.**
   `src/tests/decompression.rs::iterator_skips_quarantined_block_without_redundant_decompression`
   never measures "without redundant decompression" (no `take_records_walked` or failure-count check).
   It also leaves `line_count() = 15` while `iter()` yields 10. That is consistent with the
   skipping-iterator contract, but the test never asserts `skipped_lines`, so
   `yielded + skipped = line_count` (`iter-and-dense-walk-match-get-line`) goes unchecked.
5. **A false doc contract.** `Scrollback::new` in `src/lib.rs:258` documents
   `ENSURES: self.memory_used() == 0`. That is false: `HotTier::new` and `WarmTier::new` start
   `bytes_used` at `size_of::<Self>()`, and `src/tests/memory_budget.rs::scrollback_total_memory_used`
   says so ("initial is non-zero"). Correct the doc before it becomes a Trust clause.
6. **A stale cleanup path.** `tests/disk_cold_safety.rs::disk_cold_load_cleans_orphan_compact_temp_file`
   tests a `.dtrm.compact` orphan, but compaction now writes `.dtrm.tmp` (`src/disk_compaction.rs:50`).
   The `.dtrm.compact` cleanup in `src/disk.rs:158` looks like a legacy path. The same-named test in
   `src/disk_compaction_tests.rs` covers the live `.dtrm.tmp` path. `disk-cold-reload-scan` states
   both; drop the `.dtrm.compact` clause if the legacy path is removed.
7. **"Exactly" asserted as a range.** `src/line_tests.rs::dropping_one_line_of_a_footprint_drops_exactly_its_share`
   asserts `[10_000, 20_000)` for a 40,000/4 share. The implied spec (`image-memory-share`) is the exact
   `per_row_bytes` share.
8. **Budget claimed, diagnostic measured.**
   `src/tests/image_horizon.rs::a_footprints_payload_leaves_the_byte_budget_when_its_rows_cross` talks
   about the byte *budget* but measures `total_memory_used()` (diagnostic, includes caches), not
   `budgeted_memory_used()`. The budget claim is untested; `image-retention-horizon` should state it over
   `budgeted_memory_used()` as well.
9. **A test of test-only code.** `src/line_tests.rs::hyperlink_span_contains` asserts
   `HyperlinkSpan::width()`, which is `#[cfg(test)]` with no production caller. Only its `contains`
   half samples `span-column-lookup`; delete `width()` with the test.
10. **Data-dependent asserts.** `src/cold_tier_tests.rs::compression_ratio` asserts
    `lz4_size < uncompressed_size` and `cold_size < uncompressed_size`. Those depend on the data and are
    not a spec. Only its integrity half (decompress = input) samples `cold-tier-refines-seq`.

## 6. Proof order (easiest high-value first)

No spec here is blocked by gap 0 alone: the crate's smallest functions still need E6 widening
(gap 3) for `saturating_*`, casts or enum matches. The order starts with those, because gap 3 is
the cheapest Trust work and the seal `150efe4a` already admits range matches on `u8`.

1. **`link-ceiling-formula`** and **`threshold-bytes-bounds`** (gaps 0, 3). These are a `const fn`
   with `saturating_add`/`saturating_mul`, and a `u128` multiply-divide with a narrowing `min`. Both are
   `omega`-class over the E6 import. They retire 2 tests, and `threshold-bytes-bounds` is a lemma of
   `store-watermark-invariant` (8 tests).
2. **`watermark-transition-table`** (gaps 0, 1, 2, 3). The free function `recompute_watermark` needs
   only gap 3 (an enum match over 3 variants with `usize` compares); state it on the free function
   first, and the `update_watermark_level` half (gap 2) after. It retires 8 tests and is the other lemma
   of `store-watermark-invariant`.
3. **`cell-attrs-codec-mask`** (gaps 0, 1, 3). A `[u8; 10]` LE round-trip and one bit mask. It needs
   fixed-size arrays from gap 1, but no loops or heap data. It retires 3 tests and is the leaf of every
   line codec spec.
4. Then the scalar-state and short-loop specs: `warm-block-quarantine-fsm` (a `Cell<u8>` state
   machine), `span-column-lookup`, `binary-search-counted` (one loop with an obvious invariant) and
   `dec-string-decimal`.
5. The codec cluster: `lz4-decode-classification` (13 tests), `record-is-valid-mirrors-deserialize`,
   `line-decode-total-capped`, `block-decode-total-bounded`, then the strict and stripping decoders and
   the round-trips. These need gaps 1, 5 and 7 and the `trust::skip` lifts.
6. The tier refinements: `hot-tier-deque`, `warm-tier-refines-seq`, `cold-tier-refines-seq`.
7. The store refinements and invariants by composition (gap 4): `scrollback-refines-seq`,
   `tier-shape-invariant`, `memory-accounting-exact`, `store-watermark-invariant`, `failure-atomicity`,
   `store-corruption-semantics`, `iter-and-dense-walk-match-get-line`, `storage-delegates`.
8. The disk and mmap specs last (gap 8). Factor the page scan of `disk-cold-reload-scan` into a pure
   function over `&[u8]` so its parsing half can move up to step 5.

## 7. Gap histogram

Specs blocked by each audit §7 gap (a spec counts once per gap it needs):

| gap | specs |
|---|---|
| 0 seal | 53 |
| 1 non-scalar carriers | 49 |
| 2 `&self`/`&mut self` views | 32 |
| 3 E6 body widening | 24 |
| 4 callee-ensures composition | 22 |
| 5 quantified clauses | 18 |
| 6 plain-Rust clause publication | 2 |
| 7 loops | 31 |
| 8 effects | 9 |
| 10 model→code refinement | 4 |
| 11 generics | 1 |
| 12 cross-crate citation | 5 |
| 13 closures | 4 |
