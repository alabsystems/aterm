<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-lz4: spec recovery from its 26 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 10 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-15: this is the crate's migration worklist. The 12 specs below are each
> proved through Trust/Clean about the compiled Rust function, and then the tests each one
> subsumes are deleted. The one test in §4 that samples no spec can be deleted now.
>
> **Re-measured on Trust seal `150efe4a`, 2026-10-16: `token-fit-table` is still blocked.** The
> crate builds under the hardened profile; the PM-1 boundary does not refuse it. Nothing was changed
> (the tests-and-gates audit deleted `check_token` separately). There are three blockers:
> 1. **The E6 shape gate (gap 3).** The mask body `!(t & 0x0F == 0x0F || t & 0xF0 == 0xF0)` passes
>    the facet gate (`TRUST_E6_DEBUG … admissible=true … recognize=None`), but no
>    `trust_import_aterm_lz4__block__decompress_safe__does_token_fit` is minted, because integer `&`
>    is outside the importable fragment. The island then fails with
>    `AutoBoundHeadOverApplied { name: "trust_import_…does_token_fit", … }`. The same function
>    written as a `u8` `match` (`0xF0..=0xFF | 0x0F | 0x1F | … | 0xEF => false, _ => true`) is
>    admitted and minted.
> 2. **The contract lane cannot state the spec.** In an `ensures`, `>>` and `&` are refused as
>    `unsupported contract predicate expression`, and `%` is refused as `contract uses unmodeled
>    fixed-width machine arithmetic` (the Machine{w} lane excludes `/` and `%`). Wrapping `*` is
>    admitted, so the closest form that can be stated is
>    `result == (token < 240 && token * 16 != 240)`.
> 3. **Clean cannot prove a `UInt8` bit fact for a variable byte.** `omega` cannot prove even
>    `(t : UInt8) (h : t < 240) : t < 241` (the same goal over `Nat` passes). `decide` cannot
>    synthesize `Decidable` for `∀ t : UInt8` or `∀ g : Fin 3`. `fin_cases` does not substitute
>    the case value into the goal, and neither does `interval_cases` (whose range is also capped at
>    100). `ay_bv`/`ay_omega`/`ay_smt` report the `UInt8` operations as uninterpreted ("lossy
>    translation"). `BitVec 8` has no `LT` or `HMul … Nat` instance.
>
> Two ways forward. One is a Clean decision procedure for fixed-width words: `decide` over
> `∀ x : UInt8`, or `UInt8` support in `omega`/`ay_bv`. The other is a Nat-reflection lemma
> library in the island. Probe files: `$HOME/trust-land/seal-aterm-probe/lz4tok`, `…/lz4clean`.

Source: `crates/aterm-lz4` (read only). I read `src/{lib.rs, sink.rs, fastcpy.rs}`,
`src/block/{mod.rs, compress.rs, decompress_safe.rs, hashtable.rs}` and
`tests/{fuzz_decode_never_panics.rs, round_trip.rs, lz4_suite.rs}`, and every `#[test]` in them:
src/block/compress.rs 3, src/block/decompress_safe.rs 4, src/fastcpy.rs 1, src/sink.rs 1,
tests/fuzz_decode_never_panics.rs 3, tests/round_trip.rs 14 = **26**. Nothing was compiled or run.
The claims below come from reading the code.

The migration manifest says 27 tests. The extra grep hit is the text "`#[test]`s" in a `//!` doc
comment in `tests/lz4_suite.rs`, which only declares modules and holds no test.

Result: **12 specs**, covering **25 tests**. **1 test is not a spec sample** (§4).

Gap numbers in the `blocking_gaps` column are the rows of the gap table in
the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md`
§7. Gap 0 (an installed seal that publishes Clean-discharged rows) blocks every spec and is
listed on each.

## 0. Notation used in the statements

- `le32(b)` = `u32::from_le_bytes([b0,b1,b2,b3])`.
- `Seq`: one LZ4 block sequence, in the format of `lz4_Block_format.md`.
  - A token byte `t` holds a literal nibble `L = t>>4`. When `L = 15` it is extended by
    `ExtLen` bytes: a run of `0xFF` bytes ended by one non-`0xFF` byte, all summed.
  - The `L` literal bytes follow.
  - Unless the input ends right after the literals, a 2-byte little-endian offset `o` follows,
    then a match nibble `M = t & 0xF`, extended the same way when `M = 15`. The match length is
    `ml = 4 + M (+ ext)`.
- `RefDecode(input, cap)`: a list-level reference semantics. It folds `Seq` over `input` from
  `out = []` and returns:
  - `Ok(out)` when the input ends exactly after a literal section;
  - `Err(ExpectedAnotherByte)` on a truncated token, extension byte or offset;
  - `Err(LiteralOutOfBounds)` when a literal run passes the end of the input;
  - `Err(OutputTooSmall)` when `|out|+L > cap` or `|out|+ml > cap`;
  - `Err(OffsetOutOfBounds)` when `o = 0` or `o > |out|`.

  Otherwise it appends `out[|out|-o ..]` cyclically for `ml` bytes. This is byte-by-byte overlap
  semantics: `o < ml` repeats the period.
- `ValidBlock(input, cap)` :⇔ `RefDecode(input, cap)` is `Ok`.
- `LitBlock(x)` = `[token(|x|)] ++ ExtLen(|x|-15 if |x| ≥ 15) ++ x`. This is the literal-only last
  sequence that `handle_last_literals` writes.
- `Conformant(c)`: `c` parses as `Seq`s, and
  - the last `Seq` is literal-only with `≥ 5` literals (`LAST_LITERALS`);
  - every match starts `≥ 12` bytes (`MFLIMIT`) before the end of the decoded block;
  - every offset is in `1..=65535`.
- `lcp(a, b)`: the length of the longest common prefix of `a` and `b`.
- Sink model for `SliceSink`: `(buf: [u8], pos)`, with invariant `pos ≤ |buf|`. `cap` = `|buf|`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking_gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
lz4-round-trip	round-trip	block::compress::{compress_prepend_size,compress,compress_into_vec_with_dict,compress_into_sink_with_dict,compress_internal}, block::decompress_safe::{decompress_size_prepended,decompress,decompress_internal}, block::uncompressed_size	∀ x:[u8] with |x| ≤ u32::MAX: decompress_size_prepended(compress_prepend_size(x)) = Ok(x), and ∀ m ≥ |x|: decompress(compress(x), m) = Ok(x). The lemma the proof goes through: RefDecode(compress(x), |x|) = Ok(x) ∧ ValidBlock(compress(x), |x|); then decode-refines-reference-on-valid-blocks finishes it.	round_trip_repeated_pattern, round_trip_boundary_sizes; fuzz_decode_never_panics::size_prepended_round_trip_is_lossless	VERY HARD: Vec/slices throughout; generic S: Sink, T: HashTable and const USE_DICT over &mut S; two nested loops in compress_internal with loop-carried cursors and hash-table state (invariant: every table entry is a past position, and every emitted sequence decodes to the input prefix it covers); &mut usize outputs (count_same_bytes, backtrack_match); Box<[u16;4096]>; vec!/truncate/shrink_to_fit; the decoder's 16-byte try_into window, copy_within (memmove) and wild copies past pos. compress_internal and decompress_internal are trust::skip today.	0,1,2,3,4,5,7,11
compress-total-and-bounded	no-panic/bounds	block::compress::{compress,compress_prepend_size,compress_into_vec_with_dict,compress_internal,get_maximum_output_size}	∀ x:[u8]: compress(x) does not panic (allocation aside). compress_internal never returns OutputTooSmall when the sink has get_maximum_output_size(|x|) free bytes, so the .unwrap() in compress_into_vec_with_dict is unreachable. Hence |compress(x)| ≤ get_maximum_output_size(|x|) and |compress_prepend_size(x)| ≤ get_maximum_output_size(|x|) + 4.	compress::test_bug	HARD: a loop invariant on output size (bytes emitted ≤ 1.1·consumed + 20) from the per-sequence overhead bound (1 token, ext bytes ≤ len/255 + 1, 2 offset bytes); Sink push/extend capacity contracts on SliceSink; &mut sink; saturating arithmetic. compress_internal, push_byte and push_slice are trust::skip today.	0,1,2,3,4,7,11
compress-short-input-is-literal-and-conformant	contract	block::compress::{compress,compress_internal,handle_last_literals,token_from_literal,write_integer}	∀ x with |x| < LZ4_MIN_LENGTH (13): compress(x) = LitBlock(x), so |compress(x)| = |x| + 1. ∀ x: Conformant(compress(x)).	compress::test_conformant_last_block	MEDIUM for the short branch (straight-line code, write_integer's while loop, Sink push/extend). HARD for Conformant: a compress_internal loop invariant (cur ≤ len − MFLIMIT at a match; the match end bounded by count_same_bytes' input_match_end = len − END_OFFSET), generic over Sink/HashTable.	0,1,2,3,4,7,11; fragment: `block::compress::token_from_literal` F1 (VF5 first target, docs/PLAN-tests-to-specs-2026-10-15.md "F1-ready specs").
prepend-size-header	contract	block::compress::{compress_prepend_size,compress,compress_into_vec_with_dict}	∀ x with |x| ≤ u32::MAX: compress_prepend_size(x)[0..4] = (|x| as u32).to_le_bytes() ∧ compress_prepend_size(x)[4..] = compress(x). For |x| > u32::MAX the header is |x| mod 2^32 (a truncation).	round_trip::size_prefix_is_little_endian_original_length	EASY-MEDIUM: Vec content (not only length), get_mut(..4) + copy_from_slice, to_le_bytes, a mask/cast; composes with compress via callee-ensures. compress_into_vec_with_dict is trust::skip today (vec! allocation).	0,1,3,4
uncompressed-size-contract	contract	block::uncompressed_size, block::decompress_safe::decompress_size_prepended	∀ b:[u8]: |b| < 4 ⇒ uncompressed_size(b) = Err(ExpectedAnotherByte); otherwise uncompressed_size(b) = Ok((le32(b[0..4]) as usize, b[4..])). decompress_size_prepended(b) = uncompressed_size(b).and_then(|(n,r)| decompress(r,n)); hence |b| < 4 ⇒ decompress_size_prepended(b) = Err(ExpectedAnotherByte).	round_trip::decompress_rejects_too_short_input	EASY: slice get returning Options and a subslice, a tuple match, Result<(usize,&[u8]),enum>, u32::from_le_bytes.	0,1,3,4
decode-never-panics	no-panic/bounds	block::decompress_safe::{decompress_into,decompress,decompress_size_prepended,decompress_internal,read_integer,read_u16,duplicate_slice,duplicate_overlapping_slice}, sink::SliceSink (Sink impl)	∀ input:[u8], out:&mut [u8]: decompress_into(input,out) returns without panicking or indexing out of bounds, and gives Ok(n) with n ≤ |out| or Err(_). ∀ m: decompress(input,m) is Ok(v) with |v| ≤ m or Err(_) (allocation of m bytes aside). decompress_size_prepended(b) likewise (allocation of le32(b[0..4]) aside). Loop-head invariant: input_pos ≤ |input| ∧ sink.pos ≤ sink.capacity, and every SliceSink call meets its slicesink-model precondition.	fuzz_decode_never_panics::fuzz_safe_decode_never_panics, fuzz_decode_match_copy_shapes_never_panic	HARD: one loop with ? early returns; the fast path's input[input_pos..input_pos+16].try_into().unwrap() is in bounds only via the safe_input_pos guard (saturating_sub); generic S: Sink whose SliceSink methods are trust::skip and need their caller contracts as requires; &mut usize cursors. decompress_internal is trust::skip today.	0,1,2,3,4,7,11
decode-refines-reference-on-valid-blocks	refinement	block::decompress_safe::{decompress_into,decompress,decompress_internal,read_integer,read_u16,duplicate_slice,duplicate_overlapping_slice}, fastcpy::slice_copy	∀ input, out with ValidBlock(input,|out|) ∧ RefDecode(input,|out|) = Ok(y): decompress_into(input,out) = Ok(|y|) ∧ out'[0..|y|] = y (bytes beyond |y| are unspecified: wild copies scribble there), and ∀ m ≥ |y|: decompress(input,m) = Ok(y). Lemmas: (a) the fast path (token fits, input_pos ≤ |input| − 18, pos < cap − 34) computes the same Seq step as the slow path; (b) an 18/32/64-byte wild copy_within with offset ≥ ml writes the same first ml bytes as a byte-by-byte copy; (c) duplicate_overlapping_slice equals the cyclic copy, including the offset = 1 fill.	-	VERY HARD: everything in decode-never-panics, plus content reasoning over [u8] against a list model of RefDecode, memmove semantics of copy_within, and the slice-copy-contract; loop invariant out'[0..pos] = the RefDecode prefix of the consumed sequences.	0,1,2,3,4,5,7,11
decode-rejects-malformed	contract	block::decompress_safe::{decompress_into,decompress,decompress_size_prepended,decompress_internal,read_integer,read_u16,duplicate_slice}	∀ input, out: RefDecode(input,|out|) = Err(e) with e ∈ {ExpectedAnotherByte, LiteralOutOfBounds, OutputTooSmall, OffsetOutOfBounds} ⇒ decompress_into(input,out) = Err(e) (both the fast path and the slow path reject `offset == 0` and `offset > pos` with `OffsetOutOfBounds`). read_integer: ∀ input, p: Err(ExpectedAnotherByte) if the 0xFF run starting at p reaches |input|; otherwise Ok(min(sum, u32::MAX)) with p advanced past the terminating byte.	decompress_safe::corrupt_integer_run_errors_without_panic, offset_oob; round_trip::decompress_rejects_corrupt_payload, malformed_literal_length_overruns_output_errors, malformed_match_length_overruns_output_errors	HARD: loops (read_integer and the main decode loop), &mut usize, Result enums with struct variants, saturating u32, generic Sink. decompress_internal is trust::skip today.	0,1,2,3,4,5,7,11
token-fit-table	table-property	block::decompress_safe::does_token_fit	∀ t:u8: does_token_fit(t) ⇔ (t >> 4) < 15 ∧ (t & 0x0F) < 15. The domain has 256 values, so this is decidable by enumeration.	-	TRIVIAL: scalar u8 shift, &, ==, ||, !; no loops. (On seal 150efe4a: integer & is not E6-importable; the ensures cannot spell >>, & or %; Clean has no UInt8 decision procedure. See the status note.)	0,3; fragment: `block::decompress_safe::does_token_fit` F1 (VF5 first target, docs/PLAN-tests-to-specs-2026-10-15.md "F1-ready specs").
count-same-bytes-lcp	contract	block::compress::{count_same_bytes,count_same_bytes_tail}	∀ input, source:[u8], cur, candidate with cur ≤ |input| − END_OFFSET ∧ candidate ≤ |source|, let E = |input| − 6: count_same_bytes(input, &mut cur, source, candidate) returns n = lcp(input[cur..E], source[candidate..]) (so n ≤ E − cur and n ≤ |source| − candidate) and sets cur' = cur + n. Outside the precondition the clamped gets make it the lcp of the clamped slices, never a panic.	compress::test_count_same_bytes	MEDIUM-HARD: &mut usize with a two-state cur'; as_chunks::<8>, a zip loop with early return, usize::from_ne_bytes, to_le().trailing_zeros()/8 (endianness), Iterator skip/take_while/count in the tail.	0,1,2,3,5,7
slice-copy-contract	contract	fastcpy::{slice_copy,short_copy,double_copy_trick}	∀ src:[u8], dst:&mut [u8]: slice_copy(src,dst) panics ⇔ |src| ≠ |dst|; otherwise dst' = src. Lemma: ∀ SIZE, ∀ n ∈ SIZE..=2·SIZE, the two overlapping SIZE-byte copies (prefix and suffix) cover every index of 0..n.	fastcpy::test_fast_short_slice_copy_edge_cases	MEDIUM: &mut [u8] content, const generic SIZE (instantiated at 2/4/8/16/32), copy_from_slice on subslice ranges, cfg(target_arch), a diverging panic fn. All trust::skip today.	0,1,2,3,11
slicesink-model	invariant	sink::SliceSink::new, sink::SliceSink (Sink impl): pos, capacity, push, extend_from_slice, extend_from_slice_wild, extend_from_within, extend_from_within_overlapping, extend_with_fill, byte_at	SliceSink::new(output,p) panics ⇔ p > |output|; otherwise s.pos() = p ∧ s.capacity() = |output|. pos ≤ cap holds on creation and is preserved by every method under its precondition: push: pos < cap; extend_from_slice_wild: pos + |data| ≤ cap ∧ copy_len ≤ |data|; extend_from_within: start + wild ≤ cap ∧ pos + wild ≤ cap; extend_from_within_overlapping: start ≤ pos ∧ pos + n ≤ cap; extend_with_fill: pos + len ≤ cap. Each method's effect on buf[0..pos'] is the documented append. pos and capacity are panic-free.	-	MEDIUM: a struct holding &mut [u8], &self/&mut self views with a two-state pos', a trait impl, an indexing loop in extend_from_within_overlapping. The methods are trust::skip today.	0,1,2,3,7
```

### Distribution by kind (12)

| kind | count | ids |
|---|---|---|
| contract | 6 | compress-short-input-is-literal-and-conformant, prepend-size-header, uncompressed-size-contract, decode-rejects-malformed, count-same-bytes-lcp, slice-copy-contract |
| no-panic/bounds | 2 | compress-total-and-bounded, decode-never-panics |
| round-trip | 1 | lz4-round-trip |
| refinement | 1 | decode-refines-reference-on-valid-blocks |
| table-property | 1 | token-fit-table |
| invariant | 1 | slicesink-model |

### Proof dependency order

`token-fit-table`, `uncompressed-size-contract`, `slicesink-model`, `slice-copy-contract`,
`count-same-bytes-lcp` → `decode-never-panics` (uses the sink preconditions) →
`decode-rejects-malformed` and `decode-refines-reference-on-valid-blocks` (uses `slice-copy-contract`)
→ `prepend-size-header`, `compress-short-input-is-literal-and-conformant`,
`compress-total-and-bounded` (uses `count-same-bytes-lcp` and the sink model) → `lz4-round-trip`
(the compressor lemma `RefDecode(compress(x),|x|) = Ok(x)`, then the decoder refinement).

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/block/mod.rs	lz4_block_const_helpers_invariants	prepend-size-header	compress-short-input-is-literal-and-conformant, compress-total-and-bounded, decode-rejects-malformed
src/block/compress.rs	test_count_same_bytes	count-same-bytes-lcp	
src/block/compress.rs	test_bug	compress-total-and-bounded	lz4-round-trip
src/block/compress.rs	test_conformant_last_block	compress-short-input-is-literal-and-conformant	lz4-round-trip
src/block/decompress_safe.rs	corrupt_integer_run_errors_without_panic	decode-rejects-malformed	decode-never-panics
src/block/decompress_safe.rs	offset_oob	decode-rejects-malformed	
src/fastcpy.rs	test_fast_short_slice_copy_edge_cases	slice-copy-contract	
tests/fuzz_decode_never_panics.rs	fuzz_safe_decode_never_panics	decode-never-panics	
tests/fuzz_decode_never_panics.rs	fuzz_decode_match_copy_shapes_never_panic	decode-never-panics	
tests/fuzz_decode_never_panics.rs	size_prepended_round_trip_is_lossless	lz4-round-trip	prepend-size-header, uncompressed-size-contract
tests/round_trip.rs	round_trip_repeated_pattern	lz4-round-trip	
tests/round_trip.rs	round_trip_boundary_sizes	lz4-round-trip	compress-short-input-is-literal-and-conformant
tests/round_trip.rs	size_prefix_is_little_endian_original_length	prepend-size-header	
tests/round_trip.rs	decompress_rejects_too_short_input	uncompressed-size-contract	
tests/round_trip.rs	decompress_rejects_corrupt_payload	decode-rejects-malformed	decode-never-panics
tests/round_trip.rs	malformed_literal_length_overruns_output_errors	decode-rejects-malformed	
tests/round_trip.rs	malformed_match_length_overruns_output_errors	decode-rejects-malformed	
```

## 3. Coverage

- 26 tests read. 25 map to one of the 12 specs. 1 is not a spec sample (§4).
- The biggest collapses: `lz4-round-trip` (8 primary), `decode-rejects-malformed` (5),
  `decode-never-panics` (3).
- `decode-refines-reference-on-valid-blocks` has only 1 primary test, but `lz4-round-trip` (8 tests)
  is proved through it, so it is the decoder half of the crate's main property.

## 4. Tests that are not spec samples

> Status 2026-10-15: `round_trip::deterministic_output_for_same_input` has been deleted.

> Status 2026-10-17: none left. Its row here and in §2 is gone (same input, same output, which
> any proved spec of `compress` as a function of its input implies).

Every regression test in the crate (`compress::test_bug`, the `malformed_*` tests, the corrupt
integer run) is an instance of one of the general specs above.

## 5. Implied specs that look wrong, or disagree with the code

1. *(Resolved)* **`round_trip::round_trip_repeated_pattern` drops the `< 1/4` heuristic ratio check**
   and asserts lossless round-tripping (`lz4-round-trip`) only.
2. *(Resolved)* **`compress::test_conformant_last_block` now checks `LitBlock` and `Conformant` directly.**
   Across `0..LZ4_MIN_LENGTH` it checks exact `LitBlock` encoding (`token` + literal payload), and across
   `0..=32` it parses the emitted sequence stream to verify `LAST_LITERALS >= 5`, `MFLIMIT >= 12` before
   end of block, `offset in 1..=65535`, and lossless decompression instead of asserting a heuristic output
   ratio.
3. *(Resolved)* **`decompress_safe::offset_oob` now asserts exact variants across both paths.** It
   checks `Err(DecompressError::OffsetOutOfBounds)` on slow-path and fast-path `offset > pos` and
   `offset == 0`, and checks `Err(DecompressError::LiteralOutOfBounds)` separately on the truncated
   literal run.
4. *(Resolved)* **The fast path now rejects every out-of-range offset.** `decompress_internal`'s fast
   path uses `output.pos().checked_sub(offset)` instead of `saturating_sub`, returning
   `Err(DecompressError::OffsetOutOfBounds)` whenever `offset > output.pos()` (matching the slow path).
5. *(Resolved)* **Both paths now reject offset `0`.** `decompress_internal` returns
   `Err(DecompressError::OffsetOutOfBounds)` when `offset == 0` on both the fast path and the slow
   path, matching the LZ4 block format (`1..=65535`) and `valid_match_distance`.
6. **The manifest count is off by one.** It says 27 tests. There are 26 `#[test]` fns. The extra hit
   is the doc-comment text in `tests/lz4_suite.rs`.

## 6. Proof order (easiest high-value first)

No spec here is blocked by gap 0 alone. Every one needs at least the E6 body widening (gap 3), and
all but one need non-scalar carriers (gap 1). The order below starts with the smallest gap sets.

1. **`token-fit-table`** (gaps 0, 3). A scalar `u8` predicate over 256 values, decidable by
   evaluation once E6 admits shifts and bitwise ops. It retires `check_token`, and the proof is the
   smallest test case for the gap-3 widening.
2. **`uncompressed-size-contract`** (gaps 0, 1, 3, 4). Straight-line slice `get`s and
   `u32::from_le_bytes` with no loops. It is the first use of a slice carrier in this crate and the
   header half of every size-prepended spec.
3. **`prepend-size-header`** (gaps 0, 1, 3, 4). It composes with `compress` through callee-ensures
   and needs Vec content, not only length. It is the encoder dual of item 2.
4. **`slicesink-model`** (gaps 0, 1, 2, 3, 7). Small methods with explicit preconditions. Turning the
   `trust::skip` caller contracts into `requires` is the prerequisite for both decoder specs.
5. **`slice-copy-contract`** (gaps 0, 1, 2, 3, 11) and **`count-same-bytes-lcp`**
   (gaps 0, 1, 2, 3, 5, 7). These are leaf lemmas for the decoder refinement and the compressor.
6. **`decode-never-panics`**, then **`decode-rejects-malformed`**, then
   **`decode-refines-reference-on-valid-blocks`**. Together they retire 9 tests and need
   `decompress_internal`'s `trust::skip` removed (§5.4 and §5.5 are already resolved).
7. **`compress-short-input-is-literal-and-conformant`**, **`compress-total-and-bounded`**, and last
   **`lz4-round-trip`** (8 tests), which needs every gap above plus quantified clauses (gap 5).
