<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-png: spec recovery from its 25 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 2 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-16: this file is the crate's migration worklist. Each of the 18 specs below is
> proved through Trust/Clean about the compiled Rust fn, and then the tests it subsumes are
> deleted. No test has been deleted yet. Two tests also carry a compression-ratio or
> repository-corpus assertion that samples no spec; that part moves to a bench when the specs
> retire the rest of the test (§4).

Source: `crates/aterm-png` (read only). I read every `#[test]`: `src/checksum.rs` 4,
`src/deflate.rs` 5 and `tests/decode_encode.rs` 16, for 25 in total, and the code they reach
(`checksum.rs`, `deflate.rs`, `decode.rs`, `encode.rs`, `rgba.rs`, `lib.rs`, with
`aterm_codec::inflate` as an external callee). Nothing was compiled or run. The claims below come
from reading the code.

Result: **18 specs**, covering **all 25 tests**. **No test is a pure non-spec**, but 2 tests keep a
quality-metric part that samples no spec (§4, action `move-to-bench`).

Gap numbers are the rows of `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7. Gap 0 (the
installed seal publishes no Lean-discharged row) blocks every spec, so it is listed first on every
row; no spec here is blocked by gap 0 alone.

## 0. Notation used in the statements

- `crc_step*(c, bs)`: the bitwise reflected CRC-32 fold. For each byte `b`: `c ^= b`, then 8 times
  `c = (c&1) ? 0xEDB8_8320 ^ (c>>1) : c>>1`. No seed and no final xor.
- `crc_ref(bs) = crc_step*(0xFFFF_FFFF, bs) ^ 0xFFFF_FFFF` (CRC-32/ISO-HDLC).
- `adler_ref(bs) = (B << 16) | A`, where `A = (1 + Σ bs) mod 65521` and
  `B = (Σ_k (1 + Σ bs[0..=k])) mod 65521` (RFC 1950 §9).
- `Inflate(z)`: the RFC 1950/1951 denotation of a byte string, a Lean model (a partial function to
  bytes). It is deliberately **not** `aterm_codec::inflate`, because citing that needs gap 12.
- `Tok(data)`: the LZ77 token list `deflate_fixed` emits, each `Lit(b)` or `Match(len, dist)` with
  `3 ≤ len ≤ 258` and `1 ≤ dist ≤ 32768`. `expand(Tok)` is the RFC 1951 back-reference expansion.
- `Bits`: the bit-list model of `BitWriter` (LSB-first packing, RFC 1951 §3.1.1).
- `PngFile F`: an abstract well-formed PNG. IHDR has a legal (colour, depth) pair, width and
  height ≥ 1, interlace 0 or 1; optional PLTE and tRNS. The IDAT payload is a zlib stream whose
  `Inflate` is exactly the filtered scanlines the geometry needs (Adam7 passes when interlaced).
  `bytes(F)` is any serialization of `F` with correct CRCs.
- `PngSem(F, T)`: the PNG 3rd-edition denotation. Unfilter each row (filter types 0-4, Paeth),
  deinterlace, then map every pixel through the transform: `output_info`'s (colour, depth) table,
  then `transform_pixel`'s per-pixel rule. It includes the resolutions of malformed input listed
  in `decode-denotation` (i)-(vi).
- `Dec(bytes, T, L)`: `Decoder::new(bytes)`, `set_limits(L)`, `set_transformations(T)`,
  `read_info`, then `next_frame` into a buffer of `output_buffer_size`. The result is the
  `(OutputInfo, pixels[..buffer_size])` pair or a `DecodingError`.
- `T` ranges over every `Transformations` value (IDENTITY, EXPAND, STRIP_16, EXPAND|STRIP_16).
- `enc8(color, w, h, px)`: `Encoder::new` with `set_color(color)`, `set_depth(Eight)`,
  `write_header`, `write_image_data(px)`, `finish`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; all paths relative to
`crates/aterm-png`), `rust features the proof must handle`, `blocking gaps` (audit §7).

```tsv
id	kind	functions	statement	subsumes	rust features	blocking gaps
crc32-table-generation	table-property	checksum::tables	∀ n<256: tables()[0][n] == crc_step*(n, []) after 8 bit-steps of n. ∀ 1≤k<8: tables()[k][n] == (tables()[k-1][n] >> 8) ^ tables()[0][tables()[k-1][n] & 0xFF]. Hence tables()[k][n] is the CRC state after folding byte n then k zero bytes from state 0 (the slice-by-8 lemma).	(none; lemma for crc32-denotation)	while loops over [[u32;256];8], shifts, xor, masks, as-casts; static OnceLock lazy init (model of get_or_init returning the closure value)	0,3,5,7,8
crc32-denotation	refinement	checksum::Crc32::{new,update,finish,of}	∀ s: u32, bytes: after s.update(bytes), s' == crc_step*(s, bytes); so update is a monoid action: update(a++b) == update(a);update(b) for every split. ∀ parts: &[&[u8]]: Crc32::of(parts) == crc_ref(concat(parts)). Corollary by decide: crc_ref("123456789") == 0xCBF4_3926.	crc32_is_associative_over_splits, slicing_agrees_with_the_byte_at_a_time_definition_at_every_alignment	&mut self two-state update, split_first_chunk::<8>, for over the tail, while-let, u32 bit ops, casts, slice of slices	0,1,2,3,5,7
adler32-no-overflow	no-panic	checksum::adler32	∀ data of any length, no += in adler32 overflows u32. Invariant: at the start of each NMAX=5552 chunk a<65521 ∧ b<65521; inside a chunk of n≤5552 bytes a ≤ 65520+255n and b ≤ 65520 + Σ_{k=1..n}(65520+255k) < 2^32. With overflow checks on, it never panics.	(none; lemma for adler32-denotation)	nested for over data.chunks(NMAX) and chunk bytes, u32 arithmetic, %; loop invariant with a nonlinear bound	0,1,3,7
adler32-denotation	refinement	checksum::adler32	∀ data: adler32(data) == adler_ref(data). Reducing once per NMAX bytes equals reducing per byte, since mod 65521 is a ring homomorphism and the sums never overflow (adler32-no-overflow). Corollaries by decide: adler32("") == 1, adler32("Wikipedia") == 0x11E6_0398.	src/checksum.rs::adler32_check_vectors	slice content, chunks iterator, nested loops, modular arithmetic, shift/or	0,1,3,5,7
deflate-code-tables	table-property	deflate::{fixed_litlen,length_code,distance_code,BitWriter::huffman}	(a) requires s ≤ 287: fixed_litlen(s) == the RFC 1951 §3.2.6 fixed code (0x30+s/8 bits for 0..=143; 0x190+(s-144)/9 for 144..=255; s-256/7 for 256..=279; 0xC0+(s-280)/8 for 280..=287), a prefix-free canonical code. (b) ∀ 3≤len≤258: length_code(len) == (c,e,n) with 257≤c≤285, n == LENGTH_EXTRA[c-257], e < 2^n, LENGTH_BASE[c-257]+e == len. (c) requires 1≤d≤32768: distance_code(d) == (c,e,n) with c≤29, n == DIST_EXTRA[c], e < 2^n, DIST_BASE[c]+e == d. (d) in (b) and (c), c is the unique RFC 1951 §3.2.5 code for the value.	(none directly; covers the d=32768 case roundtrips_extreme_match_geometry claims but misses, §5.1)	match on ranges, while loop down a const array, u16/u32 casts, bit-reversal loop in huffman	0,3,5,7
bitwriter-denotation	refinement	deflate::BitWriter::{new,bits,huffman,finish}	Invariant: out ++ (low n bits of acc, LSB-first) == Bits, with n < 8 between calls. bits(v,k) for k≤24 appends the k low bits of v LSB-first. huffman(code,len) appends the len bits of code MSB-first. finish() returns Bits zero-padded to whole bytes, packed LSB-first per byte (RFC 1951 §3.1.1).	(none; lemma for zlib-compress-roundtrip)	&mut self struct with Vec<u8> content, u32 accumulator, while loop, shifts, as u8 truncation	0,1,2,3,7
lz77-tokens-reconstruct	invariant	deflate::{deflate_fixed,hash3}	∀ data, with Tok the emitted tokens: (a) every Match(len,dist) at position i has 3≤len≤258, 1≤dist≤32768=WINDOW, dist≤i, i+len≤data.len(), and data[i-dist+j] == data[i+j] ∀ j<len. (b) expand(Tok) == data. (c) no index is out of bounds and no arithmetic overflows, via head[h], prev[k] ∈ {usize::MAX} ∪ [0,i) and candidate < i whenever ≠ usize::MAX.	(none; lemma for zlib-compress-roundtrip)	Vec<usize> hash chains, nested while with break/continue, slice indexing, iter_mut().enumerate().skip().take(); ghost token list	0,1,3,5,7
zlib-compress-roundtrip	round-trip	deflate::{zlib_compress,deflate_fixed}	∀ data: Inflate(zlib_compress(data)) == Some(data): the stream is one final fixed-Huffman block encoding Tok(data) then end-of-block. Composes bitwriter-denotation, deflate-code-tables and lz77-tokens-reconstruct. Corollary once aterm_codec::inflate has its own refinement spec (gap 12): ∀ cap ≥ data.len(), zlib_decompress(&zlib_compress(data), cap) == Ok(data).	src/deflate.rs::roundtrips_edge_shapes, roundtrips_extreme_match_geometry, roundtrips_incompressible_noise, roundtrips_image_like_input (round-trip half)	Vec<u8> content, composition of callee ensures, Lean RFC 1951 inflate model bound to Rust bytes	0,1,4,5,7,10,12
zlib-framing	contract	deflate::zlib_compress	∀ data, z = zlib_compress(data): z.len() ≥ 6; z[0] == 0x78 (CM=8, CINFO=7); z[1] == 0x01 (FDICT=0, FLEVEL=0); (z[0]*256+z[1]) % 31 == 0; the last four bytes of z == adler32(data).to_be_bytes() (== adler_ref(data) by adler32-denotation).	src/deflate.rs::zlib_header_is_well_formed	Vec content (prefix/suffix), extend_from_slice, to_be_bytes	0,1,4
decode-total	no-panic	decode::{Decoder::read_info,Reader::parse,read_chunk,Reader::next_frame,unfilter,read_sample,Reader::transform_pixel,write_pixel}, rgba::{decode_rgba8,to_rgba8}	∀ bytes, T, L: read_info returns Ok or Err without panicking. ∀ Reader r it returns and buf with len ≥ r.output_buffer_size(): r.next_frame(buf) returns Ok or Err without panicking and allocates at most O(L.bytes) beyond the input. ∀ max_dimension: decode_rgba8(bytes, max_dimension) returns Some or None without panicking.	tests/decode_encode.rs::the_repository_corpus_decodes_and_round_trips (decode half)	slices, Vec, loops over chunks/rows/passes, &mut self, io::Read over &[u8], Option/Result, bit packing at depths 1/2/4/8/16	0,1,2,3,5,7,8,11
decode-denotation	refinement	decode::{Decoder::read_info,Reader::next_frame,output_info,Reader::transform_pixel,build_palette_lut,mask_trns_key,read_sample,unfilter,paeth_predictor,adam7_pass_size,Reader::place_row,Reader::place_interlaced_row}	∀ PngFile F, T, and L with L.bytes ≥ |PngSem(F,T)| and len(bytes(F)) ≤ L.encoded_bytes(): Dec(bytes(F),T,L) == Ok(PngSem(F,T)), where PngSem includes: (i) output (colour, depth, line_size) is output_info's table: EXPAND makes Indexed → Rgb (Rgba with tRNS), Grayscale<8 → 8 bits (GrayscaleAlpha with tRNS), Rgb → Rgba with tRNS; STRIP_16 takes the high byte of depth 16. (ii) an index ≥ the palette entry count reads RGB (0,0,0), alpha 255 when tRNS is present. (iii) an indexed tRNS longer than the palette is present but empty: every alpha is 255. (iv) a grey/RGB colour key at depth < 16 is masked with 0x00FF and compared to the raw sample. (v) a zero-length ancillary chunk is absent and does not change the output shape. (vi) a sub-8-bit grey sample under EXPAND scales as v*255/(2^d-1).	tests/decode_encode.rs::decodes_the_whole_format_matrix_to_its_pinned_digest, an_index_past_the_palette_renders_black, a_trns_longer_than_the_palette_is_ignored, an_out_of_range_trns_key_still_matches_its_low_byte, a_zero_length_trns_is_absent	enums (ColorType/BitDepth), Option<Vec<u8>> palette/tRNS, packed bit reads, nested Adam7 loops, &mut [u8] output, Lean PNG model with list/inductive carriers, cross-crate inflate stream	0,1,2,3,4,5,7,10,12
chunk-structure	contract	decode::{Reader::parse,read_chunk,Decoder::read_info}	∀ bytes, T, L: read_info succeeds ⇒ the chunk sequence up to the first IEND starts with the signature and IHDR with no later IHDR; ≤ 1 non-empty PLTE, none after IDAT; ≤ 1 non-empty tRNS, none after IDAT and, for indexed, none before PLTE; all IDATs form one contiguous run (no chunk, empty or not, between two IDATs); ≥ 1 IDAT; IEND present; indexed has PLTE; every chunk CRC == crc_ref(type ++ data). Conversely, Dec(F with its IDAT payload split into any consecutive IDATs) == Dec(F). None of these refusals depends on T.	tests/decode_encode.rs::illegal_chunk_orderings_are_refused	while loop over chunks with a boolean state machine, slices, [u8;4] tag matching, Vec concatenation, Result early return	0,1,3,5,7
idat-trailing-and-excess	contract	decode::{Reader::next_frame,read_image_data}	∀ PngFile F with zlib stream Z and L, T admitting F: (a) ∀ pad: Dec(F with IDAT payload Z ++ pad) == Dec(bytes(F),T,L) while the encoded-input limit still admits it; the Adler-32 is read at the end of the stream, not of the buffer. (b) let F_e decompress to e bytes past the scanlines: 0 ≤ e ≤ 65535 (TRAILING_SLACK) ⇒ Dec(F_e) == Dec(F); e > 65535 ⇒ next_frame returns Err(ExcessImageData).	tests/decode_encode.rs::idat_padded_after_the_zlib_stream_still_decodes, image_data_far_past_the_declared_size_is_refused_as_its_own_error	io::Read loop with saturating counter, error variants, stream composition with inflate	0,1,3,4,5,7,8,12
decode-limits-output	contract	decode::{Decoder::read_info,Reader::parse,output_info,Reader::output_buffer_size}, lib::row_bytes	∀ bytes, T, L: read_info returns Ok(r) ⇒ r.output_buffer_size() ≤ L.bytes. ∀ PngFile F, T, with S = PngSem(F,T).buffer_size: L.bytes ≥ S ∧ len(bytes(F)) ≤ L.encoded_bytes() ⇒ Dec(bytes(F),T,L) == Dec(bytes(F),T,unlimited) (image data is streamed, nothing else scales with the image). L.bytes < S ⇒ read_info returns Err(LimitsExceeded) before any pixel buffer exists. The outcome depends on L only through these two comparisons.	tests/decode_encode.rs::a_limit_of_exactly_the_output_buffer_decodes_every_format	checked/saturating usize arithmetic, Result, struct Limits; quantification over a model PNG	0,1,4,5,7,8
decode-limits-encoded-input	contract	decode::Decoder::read_info, lib::Limits::encoded_bytes	∀ L: L.encoded_bytes() == min(usize::MAX, L.bytes*4 + 65536), saturating, never overflowing (also for L.bytes == usize::MAX). ∀ R: io::Read, L: read_info consumes at most L.encoded_bytes()+1 bytes from R. With N the source length (∞ if unbounded): N > L.encoded_bytes() ⇒ Err(LimitsExceeded); N ≤ L.encoded_bytes() ⇒ the outcome of Reader::parse on those N bytes. The boundary is inclusive.	tests/decode_encode.rs::encoded_input_cannot_bypass_the_pixel_budget	generic R: Read (effectful read), Read::take/read_to_end, saturating arithmetic	0,3,8,11
decode-rgba8-contract	contract	rgba::{decode_rgba8,to_rgba8}	∀ bytes, m: (a) Some((rgba,w,h)) ⇒ 1≤w≤m ∧ 1≤h≤m ∧ rgba.len() == w*h*4. (b) ∀ PngFile F of any colour type and depth with 1≤width,height≤m and len(bytes(F)) ≤ encoded_bytes(Limits{bytes: max(m*m*4, DECODE_BYTE_LIMIT)}): decode_rgba8(bytes(F), m) == Some((to_rgba8(PngSem(F, EXPAND|STRIP_16)), width, height)); completeness holds because the byte budget is never below the dimension budget and the output after STRIP_16 is ≤ 4 bytes per pixel. (c) width > m ∨ height > m ⇒ None.	tests/decode_encode.rs::decode_rgba8_decodes_every_image_inside_the_callers_dimension_budget	Option, Vec content, checked_mul, composition with decoder ensures, enum match in to_rgba8	0,1,4,5,7,8
encode-decode-roundtrip	round-trip	encode::{Encoder::new,Encoder::set_color,Encoder::set_depth,Encoder::write_header,Writer::write_image_data,Writer::finish,filter_image,apply_filter,write_chunk}, deflate::zlib_compress	∀ color ∈ {Grayscale, Rgb, GrayscaleAlpha, Rgba}, w,h ≥ 1 with w*h*samples(color) inside usize, px with px.len() == w*h*samples(color): enc8(color,w,h,px) succeeds with bytes B; B is a well-formed PngFile (every chunk CRC correct, filter_image's rows unfilter to px), and Dec(B, IDENTITY, L) == Ok((w, h, color, Eight, line_size = w*samples(color), px)) for every L admitting it. px.len() ≠ w*h*samples ⇒ write_image_data returns Err(WrongDataSize).	tests/decode_encode.rs::this_decoder_reads_back_exactly_what_this_encoder_wrote, the_repository_corpus_decodes_and_round_trips (round-trip half)	generic W: Write sink (&mut Vec<u8>), &mut self builder, Vec content, filter selection loop, composition of zlib-compress-roundtrip and decode-denotation	0,1,2,4,5,7,8,11
encode-indexed-and-metadata	round-trip	encode::{Encoder::set_palette,Encoder::set_trns,Encoder::set_source_srgb,Encoder::write_header,Writer::write_image_data}, decode::Reader::info	(a) ∀ palette P of 1..=256 RGB triples, trns t with len(t) ≤ len(P)/3, indices idx of length w*h with every index < len(P)/3: the indexed depth-8 encode succeeds; under IDENTITY the decode returns Indexed with pixels == idx; under EXPAND|STRIP_16 it returns Rgba (Rgb without tRNS) with pixel k == P[idx[k]] ++ [t.get(idx[k]).unwrap_or(255)]. (b) some index ≥ len(P)/3 ⇒ write_image_data returns Err(EncodingError::Parameter(_)) and writes no IDAT. (c) ∀ SrgbRenderingIntent i: an encode with set_source_srgb(i) decodes with Reader::info().srgb == Some(i).	tests/decode_encode.rs::indexed_encoding_round_trips_and_refuses_an_index_past_the_palette, srgb_intent_round_trips	Vec<u8> palette/trns, Option builder fields, iterator max(), enum round-trip, generic Write sink	0,1,2,4,5,7,8,11
```

## 2. Test → spec map (TSV; all 25)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
`+bench` marks a test whose leftover quality-metric assertion moves to a bench (§4).

```tsv
file	test	primary	also
src/checksum.rs	crc32_is_associative_over_splits	crc32-denotation	crc32-table-generation
src/checksum.rs	slicing_agrees_with_the_byte_at_a_time_definition_at_every_alignment	crc32-denotation	crc32-table-generation
src/checksum.rs	adler32_check_vectors	adler32-denotation	adler32-no-overflow
src/deflate.rs	roundtrips_edge_shapes	zlib-compress-roundtrip	lz77-tokens-reconstruct, bitwriter-denotation, deflate-code-tables
src/deflate.rs	roundtrips_extreme_match_geometry	zlib-compress-roundtrip	lz77-tokens-reconstruct, deflate-code-tables
src/deflate.rs	roundtrips_incompressible_noise	zlib-compress-roundtrip	lz77-tokens-reconstruct
src/deflate.rs	roundtrips_image_like_input	zlib-compress-roundtrip	+bench (20x compression ratio)
src/deflate.rs	zlib_header_is_well_formed	zlib-framing	adler32-denotation
tests/decode_encode.rs	decodes_the_whole_format_matrix_to_its_pinned_digest	decode-denotation	decode-total, decode-limits-output
tests/decode_encode.rs	an_index_past_the_palette_renders_black	decode-denotation	
tests/decode_encode.rs	a_trns_longer_than_the_palette_is_ignored	decode-denotation	
tests/decode_encode.rs	an_out_of_range_trns_key_still_matches_its_low_byte	decode-denotation	
tests/decode_encode.rs	a_zero_length_trns_is_absent	decode-denotation	chunk-structure
tests/decode_encode.rs	illegal_chunk_orderings_are_refused	chunk-structure	crc32-denotation
tests/decode_encode.rs	idat_padded_after_the_zlib_stream_still_decodes	idat-trailing-and-excess	decode-denotation
tests/decode_encode.rs	image_data_far_past_the_declared_size_is_refused_as_its_own_error	idat-trailing-and-excess	
tests/decode_encode.rs	a_limit_of_exactly_the_output_buffer_decodes_every_format	decode-limits-output	decode-denotation
tests/decode_encode.rs	encoded_input_cannot_bypass_the_pixel_budget	decode-limits-encoded-input	
tests/decode_encode.rs	decode_rgba8_decodes_every_image_inside_the_callers_dimension_budget	decode-rgba8-contract	decode-denotation
tests/decode_encode.rs	this_decoder_reads_back_exactly_what_this_encoder_wrote	encode-decode-roundtrip	zlib-compress-roundtrip, decode-denotation
tests/decode_encode.rs	the_repository_corpus_decodes_and_round_trips	encode-decode-roundtrip	decode-total, +bench (file-count floors, encoded_total < raw_total)
tests/decode_encode.rs	indexed_encoding_round_trips_and_refuses_an_index_past_the_palette	encode-indexed-and-metadata	decode-denotation
tests/decode_encode.rs	srgb_intent_round_trips	encode-indexed-and-metadata	
```

## 3. Coverage

- 25 tests read; all 25 map to one of the 18 specs. 5 specs (`crc32-table-generation`,
  `adler32-no-overflow`, `deflate-code-tables`, `bitwriter-denotation`, `lz77-tokens-reconstruct`)
  retire no test directly: they are lemmas the round-trip and
  denotation specs are built on, and the tests sample them only through those.
- The biggest collapse is `decode-denotation` (6 primary tests, plus the denotational half of 5
  others). `zlib-compress-roundtrip` retires 4 and `crc32-denotation` 3.

## 4. Tests that are not spec samples

No test is a pure non-spec. Two mixed tests keep an assertion no spec covers.

| test | why | action |
|---|---|---|
| `src/deflate.rs::roundtrips_image_like_input` | `zlib-compress-roundtrip` covers its round-trip half. What is left is that filtered scanlines compress at least 20x: a compression-ratio quality metric on one fixed input, not a correctness property. | **move-to-bench** (the ratio); delete the test once the spec is proved |
| `tests/decode_encode.rs::the_repository_corpus_decodes_and_round_trips` | `decode-total` and `encode-decode-roundtrip` cover its decode and round-trip halves. What is left is the `encoded_total < raw_total` ratio; the ≥24 tracked-file and ≥10 image count floors (repository contents pinned through a filesystem walk) are dropped. | **move-to-bench** (corpus ratio); delete the test once the specs are proved |

Count by action: move-to-bench 2, delete 0, become-spec 0.

## 5. Implied specs that look wrong, or disagree with the code

1. **`src/deflate.rs::roundtrips_extreme_match_geometry` claims a distance it never reaches.** Its
   doc says a maximal distance (32768) is exercised, but the greedy matcher never emits it: the
   second run of 7s finds the most recent `[7,7,7]` candidates first (positions 255 down to 224);
   the candidate at position 0 (distance 32768) lies past `MAX_CHAIN=32`, and `GOOD_ENOUGH=32`
   stops the search early anyway. Distance code 29 with 13 extra bits is never produced.
   `deflate-code-tables` (c) covers it for every d; the test's claim should be corrected or dropped.
2. **`distance_code`'s precondition is vacuous for 0.**
   `debug_assert!((1..=32768).contains(&u32::from(dist).max(1)))` lifts `dist == 0` into range. The
   intended precondition `1 ≤ dist ≤ 32768` should become the `requires` of `distance_code`.
3. **`fixed_litlen` has an unstated precondition.** Its `_` arm silently maps symbols > 287 to
   `0xC0+(s-280)`, and the comment says such symbols cannot be produced. That is
   `requires symbol ≤ 287`, and it belongs on the function.
4. **`decodes_the_whole_format_matrix_to_its_pinned_digest` is a golden digest with no reference.**
   `MATRIX_DIGEST` (FNV) was checked against the `png` crate once, when that crate was replaced; the
   reference is gone, so the digest detects change but cannot show correctness.
   `decode-denotation` (the `PngSem` model) is the spec that replaces it.
5. **`encoded_input_cannot_bypass_the_pixel_budget` over-pins the implementation.** It asserts
   `consumed == encoded_limit + 1` exactly, which pins `read_to_end(take(limit+1))`. The spec is
   only "consumes at most `encoded_bytes()+1`".
6. **`an_index_past_the_palette_renders_black` says "opaque black".** Without tRNS the output is RGB
   with no alpha, so "opaque" holds only trivially; the tRNS case (alpha 255) is the one where it
   means something, and `decode-denotation` (ii) states both.

## 6. Proof order (easiest high-value first)

Every spec is blocked by gap 0; none is blocked by gap 0 alone, so the order goes by the smallest
remaining gap set and by what each proof unlocks.

1. **`deflate-code-tables`** (gaps 0,3,5,7). Pure tables over bounded domains, decidable by
   evaluation once the loops and casts are admitted (gap 3). It forces the two missing `requires`
   (§5.2, §5.3) and is a lemma of `zlib-compress-roundtrip`.
2. **`crc32-table-generation`** (0,3,5,7,8) then **`crc32-denotation`** (0,1,2,3,5,7). The table
   is bounded evaluation; the `OnceLock` init is the only effect (gap 8). The denotation retires 3
   tests and feeds `chunk-structure`.
3. **`adler32-no-overflow`** (0,1,3,7) then **`adler32-denotation`** (0,1,3,5,7). One nonlinear
   loop invariant, then a ring-homomorphism argument; retires 1 test and feeds `zlib-framing`.
4. **`zlib-framing`** (0,1,4). The smallest gap set in the crate: prefix/suffix of one `Vec`, plus
   composition with `adler32-denotation`. Retires 1 test.
5. **`bitwriter-denotation`**, **`lz77-tokens-reconstruct`**, then **`zlib-compress-roundtrip`**
   (adds gaps 10 and 12 for the Lean inflate model). Retires 4 tests.
6. **`decode-limits-encoded-input`** (0,3,8,11), **`decode-limits-output`**, **`chunk-structure`**,
   **`decode-total`**. Arithmetic and state-machine contracts on the decoder front end.
7. **`decode-denotation`**, **`idat-trailing-and-excess`**, **`decode-rgba8-contract`**. These need
   the Lean `PngSem` model (gaps 1, 10) and the cross-crate inflate citation (gap 12);
   `decode-denotation` alone retires 6 tests.
8. **`encode-decode-roundtrip`** and **`encode-indexed-and-metadata`**, which compose the deflate
   specs with `decode-denotation` (gap 4).
