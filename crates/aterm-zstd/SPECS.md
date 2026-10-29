<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-zstd: spec recovery from 18 of its 61 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 6 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-17: the rows naming `a_wrong_seed_or_length_is_caught` and the six
> `libzstd` tests are gone too. Their test file left with the `zstd` dev-dependency;
> its two raw-frame cases are cited in `tests/decode_frames.rs`, where they now run on aterm's
> decoder.

> Status 2026-10-16: this is the crate's migration worklist for the frame writer and XXH64.
> Each of the 7 specs below is proved through Trust/Clean, and then the tests it subsumes
> are deleted. The 4 checks in §4 sample no spec and can be deleted now. The 43 decoder
> tests (`src/decode/*`, `tests/cli_decode.rs`, `tests/sweep.rs`, and `tests/decode_frames.rs`
> but for its two raw-frame tests in §2) are not covered here. They need a separate recovery
> pass, which this file will gain as its own sections.

Source: `crates/aterm-zstd` (read only). I read all 18 `#[test]` fns in scope:
`src/frame.rs` 4, `src/xxh64.rs` 4, `tests/cli_frames.rs` 1, the since-deleted `libzstd` tests 6 and
`tests/xxh64_recorded.rs` 3. I also read the code they reach: `frame::{write_raw,
ContentSize::{of,flag,write}, block_header}`, `xxh64::{xxh64, Xxh64::{new,update,digest,consume},
round, merge}` and `tests/support::sanity`. Nothing was compiled or run. The claims below come
from reading the code.

Result: **7 specs**, covering **14 tests**. **4 tests are not spec samples** (§4).

## 0. Notation used in the statements

- `XXH64Spec(seed, bs : List UInt8) : UInt64` is a Lean transcription of the reference
  `xxhash_spec.md`, "XXH64 Algorithm Description". All arithmetic is mod 2^64.
  - It takes 32-byte stripes into four accumulators, starting from
    `[seed+P1+P2, seed+P2, seed, seed-P1]`, with `round(acc, lane) = rotl31(acc + lane*P2) * P1`.
  - If `len ≥ 32`, it converges the accumulators (the sum of `rotl 1/7/12/18`, then a merge
    with each of the four). Otherwise it starts from `seed+P5`.
  - It adds `len mod 2^64`. It then consumes the tail as 8-byte words, then at most one 4-byte
    word, then single bytes.
  - It finishes with the avalanche (`>>33, *P2, >>29, *P3, >>32`).
- `sanity(n)` is xxhsum's sanity buffer: byte `i` is the top byte of
  `2654435761 * 11400714785074694797^i mod 2^64`. The Lean model has exactly one definition
  (see §5.3).
- `le16/le24/le32(x)` are the little-endian byte lists of `x`, and `low32(x) = x mod 2^32`.
- `cs ∈ {Omitted, Xxh64}` is `frame::Checksum`.
- `RawFrame(c, cs)`, with `n = |c|`, is the byte list
  `le32(0xFD2FB528) ++ [fcsFlag(n)<<6 | 0x20 | (cs=Xxh64 ? 0x04 : 0)] ++ FCS(n) ++ Blocks(c) ++ (cs=Xxh64 ? le32(low32(XXH64Spec(0,c))) : [])`.
  - `fcsFlag(n)` is 0 for `n ≤ 255`, 1 for `256 ≤ n ≤ 65791`, and 2 otherwise.
  - `FCS(n)` is `[n]` for `n ≤ 255`, `le16(n-256)` for `256 ≤ n ≤ 65791`, and `le32(n)` otherwise.
  - `Blocks(c)` splits `c` into `BLOCK_MAX`-sized chunks `c_0..c_k`. The last chunk may be
    short, and there is exactly one empty chunk when `n = 0`.
    `Blocks(c) = concat_i (le24(|c_i|<<3 | [i=k]) ++ c_i)`.
  - Corollary: `|RawFrame(c,cs)| = 5 + |FCS(n)| + 3*max(1, ceil(n/BLOCK_MAX)) + n + 4*[cs=Xxh64]`.
- `LogWriter` is the writer model for `W: Write + ?Sized`. Each `write_all(b)` either appends
  `b` to an abstract byte log and returns `Ok`, or fails after appending some prefix of `b`.
  `Vec<u8>` and the test-only `Count` sink are infallible instances. The log also records
  calls, so a spec can say that no call was made.
- `Rfc8878` is a Lean model of the RFC 8878 §3.1.1 frame grammar and its decoding semantics.
  At minimum it covers single-segment frames with raw and RLE blocks. It provides
  `decodeFrame : bytes → Result (content, declaredContentSize, consumed)` and `decodeStream`
  (frames back to back).
- `Inv(h, seed, fed)` for `h : Xxh64`, where `fed` is a ghost list of every byte passed to
  `update` since `new(seed)`:
  - `h.seed = seed`;
  - `h.buffered = |fed| mod 32`, so `h.buffered < 32`;
  - `h.buf[..buffered]` is the last `|fed| mod 32` bytes of `fed`;
  - `h.acc` is the four-lane round fold over the `floor(|fed|/32)` full stripes of `fed`,
    starting from the initial accumulators for `seed`;
  - `h.total = |fed| mod 2^64`.
- **Recorded vectors.** These are the 577 TSV rows, the 14 xxhsum sanity vectors, the 6
  literal inputs and the CLI frames. They are the only evidence that `XXH64Spec` and
  `RawFrame` match the reference C code and the zstd 1.5.7 CLI. They are kept as Lean
  table-properties on the models, closed by evaluation, and not as Rust tests.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty (Rust features the
proof must handle)` and `blocking gaps`. The gaps are the numbers in the table in
`AUDIT-trust-for-test-migration-2026-10-15.md` §7 (historical internal audit).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
xxh64-model-reference-vectors	table-property	XXH64Spec (Lean), sanity (Lean)	∀ row (seed, len, want) of tests/recorded/xxh64-libzstd-1.5.7.tsv (577 rows: every length through two stripes plus the 4/8-byte tails, a 1 MiB input, wrapping seeds): XXH64Spec(seed, sanity(len)) = want. It is closed by evaluating the model. Together with xxh64-oneshot-refines-model, it puts the same answers on the compiled Rust fn.	xxh64_recorded::every_recorded_answer (each retired only when xxh64-oneshot-refines-model is also proved)	EASY-MEDIUM. There is no Rust side: this is a Lean evaluation over a data table. The TSV is embedded as Lean data, so the test's rows.len()=577 check becomes the length of that table. The 1 MiB row needs native_decide or compiled evaluation, not kernel decide.	0, 14
xxh64-oneshot-refines-model	refinement	xxh64::{xxh64, Xxh64::new, Xxh64::update, Xxh64::digest, Xxh64::consume, round, merge}	∀ seed:u64, bytes:&[u8]: xxh64(seed, bytes) = XXH64Spec(seed, bytes.to_list). This includes the wrap of total and of seed+P5 mod 2^64. The recorded rows with seed = u64::MAX−P5 and |bytes| < 32 reach that wrap: seed+P5+len wraps.	xxh64_recorded::every_recorded_answer (with xxh64-model-reference-vectors)	HARD. &[u8] slices, as_chunks::<32>/<8>/<4> with u64/u32::from_le_bytes, and a [u64;4] array iterated with iter_mut().zip. A [u8;32] buffer filled with copy_from_slice. for-loops over chunk arrays, and wrapping_add/mul/sub, rotate_left, shifts and xor on u64. The one-shot calls new, then update through &mut self, then digest through &self. It needs a Lean List UInt8 model of the input and induction over stripes.	0, 1, 2, 3, 4, 5, 7, 14
xxh64-streaming-refines-oneshot	invariant	xxh64::Xxh64::{new, update, digest, consume}	new(seed) establishes Inv(h, seed, []). For Inv(h, seed, fed) and any bytes, update(&mut h, bytes) establishes Inv(h', seed, fed ++ bytes). Inv(h, seed, fed) ⇒ digest(&h) = XXH64Spec(seed, fed), and digest leaves h unchanged. Corollary: ∀ seed and piece lists ps, digest(foldl update (new seed) ps) = xxh64(seed, concat ps). So any split and any interleaving of digest between updates gives the one-shot answer. The splits tried are two or three cuts across the stripe boundary, 13-byte pieces and one byte at a time.	xxh64.rs::split_feeding_matches_one_shot, xxh64.rs::digest_then_continue	HARD. Certified two-state &mut self reasoning on a struct of [u64;4], [u8;32], a usize and a u64. Inside update: an early return, split_at, a copy of self.buf, as_chunks, a for-loop over stripes and copy_from_slice into a subslice. digest is a &self view. It needs a ghost fed sequence (or an abstraction function) and a List model of buf.	0, 1, 2, 3, 5, 7
xxh64-update-no-panic	no-panic/bounds	xxh64::Xxh64::{update, digest}	∀ h with Inv(h) (so h.buffered < 32) and any bytes: update(&mut h, bytes) does not panic. With take = min(32−buffered, |bytes|), buf[buffered..buffered+take] is in bounds. Both copy_from_slice calls have equal-length operands, and buf[..tail.len()] is in bounds because tail.len() < 32. digest(&h) does not panic, because buf[..buffered] is in bounds.	xxh64.rs::split_feeding_matches_one_shot (with xxh64-streaming-refines-oneshot)	MEDIUM. Slice range indexing and the copy_from_slice length preconditions. usize arithmetic: 32−buffered must not underflow and buffered+take must not overflow. The as_chunks remainder bound, and &mut self.	0, 1, 2, 3
write-raw-layout	contract	frame::{write_raw, ContentSize::of, ContentSize::flag, ContentSize::write, block_header}, xxh64::xxh64	∀ content:&[u8] with |content| ≤ WINDOW_MAX, ∀ cs, ∀ LogWriter out whose write_all calls all succeed: write_raw(out, content, cs) = Ok(()) and appends exactly RawFrame(content, cs) to out's log. Instances: an empty frame is 28 B5 2F FD 24 00 01 00 00 99 E9 D8 51. abc is 28 B5 2F FD 24 03 19 00 00 'a' 'b' 'c' 99 09 77 AD under Xxh64 and 28 B5 2F FD 20 03 19 00 00 'a' 'b' 'c' under Omitted. The descriptor is 0x20/0x60/0x60/0xA0 at n = 255/256/65791/65792. Every block before the last is full and has the Last_Block bit clear. At n = WINDOW_MAX, Omitted, the length is 9 + 3·(WINDOW_MAX/BLOCK_MAX) + WINDOW_MAX. The CLI conformance rows (zstd 1.5.7 -19, with and without --no-check, on sanity prefixes) become a Lean table-property: SHA-256(RawFrame(sanity(n), cs)) = the recorded hash.	cli_frames::write_raw_writes_what_the_cli_wrote	MEDIUM-HARD. A generic W: Write + ?Sized behind &mut, modelled as an append log (write_all through a ?Sized generic), and the ? operator on io::Result. A loop with split_at(min(len, BLOCK_MAX)) that breaks on the last block. A private enum ContentSize with a match on ranges, and u8/u16/u32 casts that the match arms make exact. Shifts and or on u8/u32, and to_le_bytes with array destructuring. A call to xxh64 that cites its ensures. The CLI table needs a SHA-256 Lean model, or the recorded frames embedded as bytes.	0, 1, 3, 4, 7, 8, 11
write-raw-refuses-oversize	contract	frame::{write_raw, ContentSize::of}	∀ content with |content| > WINDOW_MAX, ∀ cs, out: write_raw(out, content, cs) = Err(e) with e.kind() = InvalidInput, and it makes no call on out, so the log is unchanged. Scalar core: ContentSize::of(n) = Err(InvalidInput) ⇔ n > WINDOW_MAX, and for n ≤ WINDOW_MAX it returns the narrowest variant per FCS(n). On either branch write_raw reads no content byte when cs = Omitted: content is only split and handed to write_all.	frame.rs::content_past_the_window_cap_is_refused (its in-range half with write-raw-layout)	EASY for the ContentSize::of core (a range match on usize, which seal 150efe4a admits for u8). MEDIUM for write_raw: io::Error::new with a format!-built message (the message is opaque to the spec; only the kind is observed), the ? before any write, and a call-log writer model.	0, 1, 3, 8, 11
raw-frame-rfc8878-conformance	round-trip	RawFrame (Lean), Rfc8878.decodeFrame / decodeStream (Lean), frame::write_raw (through write-raw-layout)	∀ content with |content| ≤ WINDOW_MAX, ∀ cs: Rfc8878.decodeFrame(RawFrame(content, cs)) = Ok(content, declaredContentSize = |content|, consumed = |RawFrame(content, cs)|). When cs = Xxh64, the trailing field equals the RFC's Content_Checksum, which the RFC defines as le32(low32(XXH64(0, content))). ∀ a, b, ca, cb: Rfc8878.decodeStream(RawFrame(a, ca) ++ RawFrame(b, cb)) = a ++ b. With write-raw-layout this gives: any RFC-conforming decoder returns exactly the content write_raw was given. An optional refinement leg is shared with the decoder specs: aterm_zstd::Decoder::new(RawFrame(c, cs)).read_to_end = c.	decode_frames::every_raw_frame_decodes_to_its_content, decode_frames::concatenated_raw_frames_decode_as_one_stream (through the Decoder leg)	MEDIUM on the Lean side: list append/split lemmas over the block chunking, and an inductive model of the frame grammar. The optional Decoder leg is HARD: it pulls in the whole decode core (the Read adapter, Vec, loops, &mut self and the window buffer).	0, 1, 5, 7, 10, 14
```

### Distribution by kind (7)

| kind | count | ids |
|---|---|---|
| contract | 2 | write-raw-layout, write-raw-refuses-oversize |
| refinement | 1 | xxh64-oneshot-refines-model |
| invariant | 1 | xxh64-streaming-refines-oneshot |
| table-property | 1 | xxh64-model-reference-vectors |
| no-panic/bounds | 1 | xxh64-update-no-panic |
| round-trip | 1 | raw-frame-rfc8878-conformance |

### Proof dependency order

`xxh64-model-reference-vectors` (Lean only) and the `ContentSize::of`/`block_header` scalar
lemmas → `xxh64-update-no-panic` → `xxh64-oneshot-refines-model` → `xxh64-streaming-refines-oneshot`
→ `write-raw-refuses-oversize` and `write-raw-layout` (each cites `xxh64`'s ensures) →
`raw-frame-rfc8878-conformance` (Lean over `RawFrame`, lifted to the Rust fn through `write-raw-layout`).

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test
samples, or that must also be proved before the test goes.

```tsv
file	test	primary	also
src/frame.rs	content_past_the_window_cap_is_refused	write-raw-refuses-oversize	write-raw-layout (the n = WINDOW_MAX half)
src/xxh64.rs	split_feeding_matches_one_shot	xxh64-streaming-refines-oneshot	xxh64-update-no-panic
src/xxh64.rs	digest_then_continue	xxh64-streaming-refines-oneshot	
tests/cli_frames.rs	write_raw_writes_what_the_cli_wrote	write-raw-layout	xxh64-oneshot-refines-model
tests/xxh64_recorded.rs	every_recorded_answer	xxh64-oneshot-refines-model	xxh64-model-reference-vectors (required)
tests/decode_frames.rs	every_raw_frame_decodes_to_its_content	raw-frame-rfc8878-conformance	write-raw-layout
tests/decode_frames.rs	concatenated_raw_frames_decode_as_one_stream	raw-frame-rfc8878-conformance	write-raw-layout
```

## 3. Coverage

- 18 tests read. 14 map to one of the 7 specs. 4 are not spec samples (§4).
- The biggest collapses are `write-raw-layout` (4 primary), `xxh64-oneshot-refines-model`
  (3 primary, 577 + 14 + 6 recorded answers) and `xxh64-streaming-refines-oneshot`
  (3 primary).
- `xxh64-oneshot-refines-model` and `xxh64-model-reference-vectors` are two legs of one
  claim. The Rust fn equals the model, and the model equals the reference answers. The
  vector tests go only when both legs are proved.

## 4. Tests that are not spec samples

Status (2026-10-16): `xxh64_recorded::a_wrong_seed_or_length_is_caught` is deleted. The two `libzstd::` entries were not found: their test file no longer exists on this branch.

Status (2026-10-17): all four are gone, and so is their table. The three `libzstd::` tests went with their test file when the `zstd` dev-dependency left; its raw-frame cases now run on aterm's decoder in `tests/decode_frames.rs`. Two were negative controls on a checksum comparison, and two tested libzstd itself, not aterm code. The `WINDOW_MAX` doc in `src/frame.rs` names libzstd's `ZSTD_WINDOWLOG_LIMIT_DEFAULT` (2^27) and its streaming decoder's `frameParameter_windowTooLarge` cap (2^27 + 1), so the comments no longer disagree.

## 5. Implied specs that look wrong, or disagree with the code

1. **Resolved: `digest_then_continue` carries no redundant ownership doc.** It tests only that
   updates after `digest(&self)` produce the streaming digest (`xxh64-streaming-refines-oneshot`).
2. **The streaming invariant needs the length mod 2^64.** The `Xxh64` doc says "Every byte
   fed, modulo 2^64". `XXH64Spec` must take `len mod 2^64` (the reference's `total_len`).
   Otherwise `Inv` is false past 2^64 fed bytes. That cannot happen in practice, but a
   quantified proof must handle it.
3. **Resolved: `sanity()` is defined once** in `tests/support/sanity.rs`, re-exported by
   `tests/support/mod.rs` and mounted via `#[path]` in `src/xxh64.rs`'s test module.

## 6. Proof order (easiest high-value first)

No spec here is blocked by gap 0 alone. Every spec that touches the Rust fn also needs
gap 1 (slices and byte lists) and gap 3 (calls, casts, shifts and loops in reflected bodies).

1. **`xxh64-model-reference-vectors`** (gaps 0, 14). This is Lean only, with no Rust
   carrier. It is the evidence base for every other XXH64 spec. It retires nothing by itself.
2. **The scalar core of `write-raw-refuses-oversize`**: `ContentSize::of`, `flag` and
   `block_header`. These are a range `match` on `usize`, casts that the arms make exact, and
   shifts. They fit gap 0 plus the cast/shift slice of gap 3, which is the nearest E6 widening
   to the range match on `u8` that seal `150efe4a` already admits. They are the lemmas
   `write-raw-layout` is built on. The `write_raw` half then waits on the writer model
   (gaps 1, 8, 11).
3. **`xxh64-update-no-panic`** (gaps 0, 1, 2, 3). This is bounds arithmetic over `usize`
   with `buffered < 32` as the only fact needed. It is `omega`-class once slices and
   `&mut self` are carried. It is also the first conjunct of the streaming invariant.
4. **`xxh64-oneshot-refines-model`** (gaps 0, 1, 2, 3, 4, 5, 7, 14). This is the
   highest-value spec: with item 1 it retires 3 tests and 597 recorded answers.
   `xxh64-streaming-refines-oneshot` (gaps 0, 1, 2, 3, 5, 7) follows from the same
   stripe-fold lemma plus `Inv`. Together they retire all 4 `xxh64.rs` tests and both
   recorded-answer tests.
5. **`write-raw-layout`**, then **`raw-frame-rfc8878-conformance`**. The first carries the
   generic writer (gaps 8, 11). The second needs the `Rfc8878` model and the model→code
   surface (gap 10). They retire the remaining frame tests and the raw-frame tests of `tests/decode_frames.rs`.

## 7. The encoder (`src/encode`, S17 E1 and E2)

> Status 2026-10-24: the producer (atpkg-publish's `tarwrite::options`) now declares a pack's
> size when its tar is within 2^27 bytes, so that frame is a single segment whose window is the
> tar (`encode-content-size-contract`, and `encode-window-bound`'s single-segment leg); only a
> longer tar is streamed under the 2^27 window with no declared size. No encoder code changed.
> The corpus config below named the producer's is the shape of the packs past 2^27 bytes; a
> smaller pack's frame is the level 19 one-shot config's shape (a declared size, a single
> segment), and atpkg-publish's oracle test decodes one under `zstd --memory=8MB`.

> Status 2026-10-23: written with the encoder (E1); the rows for the parse by price, its
> binary tree and the long-distance anchors (levels 16 to 19) came with E2. Nothing here is proved; the tests named in
> the map are what runs. Each row states the claim its tests sample, so a proof can retire
> them as §1's rows do for the frame writer and XXH64.

Public surface: `encode::compress`, `encode::Encoder::{new, write, flush, finish, get_ref}`
(`write` and `flush` through `std::io::Write`), `encode::Options::{new, level, window_log,
checksum, content_size}` and `Default`, and the constants `WINDOW_LOG_MIN = 10`,
`WINDOW_LOG_MAX = 27`, `LEVEL_MAX = 19`, `LEVEL_DEFAULT = 3`. `compress` and `Encoder` are
re-exported at the crate root.

Notation, beyond §0:

- `Opts(o)` is `o`'s resolved fields: `level ∈ 1..=19`, `wlog ∈ 10..=27` (the level's when
  unset), `cs`, and `declared : Option u64`.
- `Stream(o, ops)` runs `Encoder::new(log, o)` and then `ops`, a list of `write_all(b)`
  and `flush()` calls, then `finish()`, against a `LogWriter` whose calls all succeed.
  `fed(ops)` is the concatenation of every `b` written.
- `Rfc8878` (§0) is needed here in full: compressed blocks (§3.1.1.3), Huffman literals
  (§4.2), FSE sequences (§4.1) and repeat offsets (§3.1.1.5), not only raw and RLE blocks.
- `block_max(o) = min(128 KiB, 2^wlog)` (§3.1.1.2.4), and `cap(o) = 2·2^wlog + 2·block_max(o)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
encode-round-trip	round-trip	encode::{compress, Encoder::{new, write, flush, finish}, Options::*}, Decoder	∀ content, ∀ o: Decoder(compress(content, o)).read_to_end = content. ∀ o with declared ∈ {None, Some(|fed(ops)|)}, ∀ ops: Stream(o, ops) = Ok and Decoder(log).read_to_end = fed(ops). The cases sampled are levels 1, 2, 3, 4, 5, 9, 12, 15 and 19, windows 2^10, 2^11, 2^13, 2^16 and each level's own (2^19 to 2^23), both checksum choices, one-shot, writes of 1 to 65,537 bytes, flushes every 1,000 or 300,001 bytes, and inputs empty, tiny, one byte repeated, random, text, long repeats past 128 KiB blocks, past the window, a block whose coded sequences are written raw before blocks that would repeat its state, a block of 32,768 sequences, and the crate's recorded inputs. The frames together carry every block type, literals section type, table mode and width of Number_of_Sequences (§3.1.1.3.2.1).	encode_frames::every_frame_the_encoder_writes_decodes_to_its_input, src/encode/mod.rs::a_stream_past_many_rebases_decodes, the fuzz target's encoder half	XL. The whole encoder: Vec growth and drain, the matcher's u32 tables and chain ring, the entropy coders' bit packing, a held-table state machine across blocks, and an io::Write impl over a generic W. Every component row below is a lemma of it.	0, 1, 2, 3, 4, 5, 7, 8, 11
encode-rfc8878-conformance	round-trip	encode::{compress, Encoder}, Rfc8878.decodeFrame (Lean)	∀ content, o: Rfc8878.decodeFrame(compress(content, o)) = Ok(content, declaredContentSize = Some(|content|), consumed = |frame|), and likewise for Stream(o, ops) with declaredContentSize = Some(|fed|) exactly when the size was declared or the frame is one block. So any RFC-conforming decoder returns the content, not only aterm's. Evidence until proved: zstd 1.5.7's -t and -d pass every frame of the corpus at eight configurations (frames that together carry 1-, 2- and 3-byte sequence counts; two of them are streamed parses by price, level 17 at its own window and level 19 at 2^27, the producer's for a tar past 2^27 bytes, whose blocks are split), and frames at the largest window (2^27), one-shot and streamed, whose matches use offset codes 24 to 27, the predefined offset table's "less than 1" codes.	encode_cli::the_zstd_cli_decodes_every_frame_the_encoder_writes, encode_cli::the_zstd_cli_decodes_frames_at_the_largest_window	XL on the Lean side: the full Rfc8878 model above, with FSE and Huffman decoding as Lean functions. The Rust side is encode-round-trip's.	0, 1, 3, 5, 7, 10, 14
encode-split-invariance	refinement	encode::{compress, Encoder::{new, write, finish}}	∀ content, ∀ o with declared = Some(|content|), ∀ piece lists ps with concat ps = content and no flush: Stream(o, map write_all ps) = compress(content, o) byte for byte. Blocks are cut at fixed multiples of block_max(o) and the match finder sees the same history, whatever the write sizes.	encode_frames::the_frame_does_not_depend_on_how_writes_are_cut	HARD. Induction over write calls with an abstraction function from (hist, encoded, base, matcher) to (fed, blocks emitted); the room/slide arithmetic.	0, 1, 2, 3, 5, 7, 11
encode-flush-prefix	contract	Encoder::{write, flush}	∀ o, ops ending in flush(): the log decodes (Rfc8878, or Decoder) to exactly fed(ops) and then ends early (UnexpectedEof), because no block of a flushed frame carries Last_Block. Before the flush, what the log decodes to is a prefix of fed(ops), shorter by what is pending (at most block_max(o)).	encode_frames::a_flush_makes_everything_written_decodable	HARD. encode-round-trip's block lemma on a frame prefix, and the decoder's early-end behaviour.	0, 1, 2, 3, 7, 8, 11
encode-window-bound	contract	encode::{compress, Encoder}, Options::window_log	The frame's header declares Window_Size = 2^wlog (Window_Descriptor exponent wlog-10, mantissa 0, §3.1.1.1.2), or is a single segment exactly when the content's size is known and ≤ 2^wlog. Every sequence's offset is < 2^wlog and ≤ the bytes already emitted (§3.1.1.4), so a decoder honouring the declared window can execute it. Consequence sampled: a repeat 9 MiB back costs a few bytes under window_log 24 and its length again under level 19's 2^23; copies 2^24 + 9 to 2^27 - 1 back are all matched under window_log 27, and the two past 2^26 are not under 26.	encode_frames::a_window_reaches_as_far_as_it_declares, encode_cli::the_zstd_cli_decodes_frames_at_the_largest_window	MEDIUM-HARD. The matcher's distance checks (max_dist = 2^wlog - 1) in a loop over the chain, and the header byte arithmetic.	0, 1, 3, 7
encode-header-layout	contract	encode::write_header (private), observed through compress and Encoder	write_header(log, size, single, cs) appends le32(0xFD2FB528), the descriptor (FCS flag in bits 6-7: 0 for no size or a single-segment size < 256, 1 for 256 ≤ size < 65,792, 2 for any other size up to 2^32-1, so a windowed frame's size below 256 takes four bytes, and 3 beyond; Single_Segment in bit 5; Content_Checksum in bit 2), the Window_Descriptor unless single, and FCS(size) in that field (§3.1.1.1.4, the 2-byte field holding size-256). header::read of it gives back size, cs and the window (the size, at least 1 KiB, when single).	src/encode/mod.rs::headers_read_back, src/encode/mod.rs::a_header_with_its_segment_bit_cleared_reads_otherwise (its control)	MEDIUM. A range match on u64 with casts the arms make exact; Vec appends; to_le_bytes. The decoder's header::read for the read-back leg.	0, 1, 3
encode-content-size-contract	contract	Options::content_size, Encoder::{write, finish}	With declared = Some(n): a write taking the total past n returns Err(InvalidInput) and writes nothing; finish with total ≠ n returns Err(InvalidInput); after an Err from either check or from W's write_all, every later write, flush and finish returns Err (the frame is never silently completed; an Err from W's own flush leaves the encoder's state whole and usable). With the right count the header carries n from the first block, in a single segment when n ≤ 2^wlog and otherwise beside the Window_Descriptor.	encode_frames::a_declared_content_size_is_held	MEDIUM. A failed flag as a state invariant over &mut self, io::Error kinds, and the header row above.	0, 1, 2, 3, 8, 11
encode-memory-bound	invariant	Encoder::{new, write, flush, finish}	At every call boundary hist.capacity() ≤ cap(o) and hist.len() - encoded ≤ block_max(o) (a write takes at most room = cap(o) - len, sliding first), and the match finder's tables are fixed at the first block: the chains' 2^hash_log + 2^chain_log u32 entries, or the tree's 2^hash_log + 2^(tree_log+1) and, where the window is wider than the tree, 2^ldm_table_log anchors. So memory is O(2^wlog + the level's tables), whatever is streamed. Positions stay in u32: past 2^30 they move down by a multiple of the ring's length (the chains' or the tree's) and every older entry is forgotten, which changes no match, also where the ring is wider than a block and flushes cut blocks at odd places.	src/encode/mod.rs::the_buffer_stays_within_its_bound, src/encode/mod.rs::below_the_threshold_nothing_rebases (the rebase leg and its control), src/encode/mod.rs::a_rebase_past_a_wide_ring_changes_no_match	MEDIUM. Vec capacity is not in the length-only Vec model (gap 1); the reserve_exact policy and drain need it.	0, 1, 2, 3, 7
encode-options-clamp	contract	Options::{new, level, window_log, checksum, content_size}	new() = (level 3, wlog unset, Xxh64, undeclared) = default(). level(l) sets clamp(l, 1, 19); window_log(w) sets Some(clamp(w, 10, 27)); checksum and content_size set their field and nothing else. Every resolved option names a row of the level table.	src/encode/mod.rs::options_clamp_to_valid_bounds	EASY. const fns on a Copy struct of scalars and Options; an if-chain clamp.	0, 3
encode-entropy-components	round-trip	encode::{bits::BitWriter, fse::{normalize, write_description, Table}, huffman::Code, literals::write, sequences::{write, offset_value}} (crate-private)	Each coder inverts its decoder: a BitWriter field read forward or reversed gives the value back; a normalized histogram's description reads back as the same distribution and a stream encoded under its Table decodes under the decoder's FseTable; a Huffman Code's description (direct or FSE weights, §4.2.1) builds the same canonical code and its 1- or 4-stream output decodes (§4.2.2); every literals section kind and width decodes (§3.1.1.3.1); every literal and match length has a code whose base plus extra bits is that length (§3.1.1.3.2.1.1); every offset value resolves through the decoder's repeat-offset rule to its distance (§3.1.1.5). Each has a control where the wrong table, width or rule gives other bytes.	src/encode/bits.rs::{fields_read_back_forward_and_backward, a_field_read_at_the_wrong_width_differs}, src/encode/fse.rs::{histograms_round_trip_through_the_decoder, predefined_distributions_round_trip, a_stream_decoded_under_another_table_differs}, src/encode/huffman.rs::{skewed_flat_and_sparse_alphabets_round_trip, deep_trees_are_cut_to_eleven_bits, all_256_bytes_need_the_fse_weight_form, a_stream_under_another_description_differs}, src/encode/literals.rs::{every_kind_and_width_decodes, a_treeless_section_without_a_table_is_refused}, src/encode/sequences.rs::{length_codes_cover_every_value, offset_values_resolve_to_their_distance, a_repeat_code_under_the_other_literal_rule_differs}	HARD. u64 bit packing with shifts and masks, i16 normalized counts, sorting and a two-queue tree with a Kraft fix-up, table construction loops; each against the decoder's own table builder.	0, 1, 3, 4, 5, 7
encode-matcher-sound	contract	encode::matcher::Matcher::find (crate-private)	The sequences find returns for block h[start..end] tile a prefix of it (their literal and match lengths sum to at most end-start; the rest is the block's last literals); each match copies equal bytes from a distance d with 1 ≤ d ≤ max_dist and d ≤ the bytes before it, and is at least 3 bytes, the format's shortest (§3.1.1.3.2.1.1).	src/encode/matcher.rs::{matches_copy_equal_bytes_within_the_window, a_wrong_match_is_caught (its control), common_prefix_lengths}	HARD. u32 virtual positions against usize indices, a hash table and chain ring, nested loops with early exits and lazy steps.	0, 1, 2, 3, 7
encode-tree-sound	contract	encode::tree::Tree::matches (crate-private)	The candidates matches returns at position p come in strictly increasing length; each is at least min_match bytes (3 or 4) and copies equal bytes for its length from a distance d with 1 ≤ d ≤ max_dist and d ≤ p, never reading past the block's end. A repeat candidate names its distance through the decoder's repeat offsets under the literal-length-zero rule (§3.1.1.5); any other is Offset_Value d + 3 (§3.1.1.3.2.1.1).	src/encode/tree.rs::{matches_are_real_and_grow, a_wrong_match_is_caught (its control)}	HARD. A binary tree of u32 links in a ring, walked with two common-prefix bounds and early exits; u32 virtual positions against usize indices.	0, 1, 2, 3, 7
encode-parse-sound	contract	encode::opt::Parser::find (crate-private)	The sequences find returns for block h[start..end] tile a prefix of it as encode-matcher-sound's do, each match copying equal bytes from a distance within the window, whatever the prices chose; with anchors (encode-ldm-sound) a copy past the tree's reach is parsed as matches at its distance. Prices, the horizon and the statistics are coding choices (the format constrains only what a sequence says), so nothing is claimed of their optimality: the bar is measured (docs/DESIGN-init-system-2026-10-20.md, S17), and held where the CLI is, level 19 within 3% of `zstd -19` on archive-shaped inputs, a bar level 9 misses.	src/encode/opt.rs::{a_parse_tiles_the_block_with_real_matches, the_anchors_carry_a_copy_past_the_tree_into_the_parse (its control: the tree alone never reaches the copy), weights_and_scaling}, encode_cli::level_19_stays_within_3_percent_of_zstd_19 (the bar, not a soundness sample)	HARD. A forward dynamic program over a 4 KiB horizon of i32 prices, a backtrack through it, and frequency tables rescaled per block.	0, 1, 2, 3, 7
encode-ldm-sound	contract	encode::ldm::Ldm::find (crate-private)	The long matches find returns for block h[start..end] are in order and apart, each inside the block and copying equal bytes from a distance d with 1 ≤ d ≤ max_dist and d ≤ its start. Anchors depend only on the last min_len bytes, so a copy of a few dozen bytes or more inside the window shares anchors with its source and is found; past the window it is not.	src/encode/ldm.rs::{a_far_copy_is_found_block_by_block, a_copy_past_the_window_is_not (its control)}	MEDIUM-HARD. A gear hash over u64 with wrapping shifts, xxh64-keyed buckets each a ring, forward and backward extension.	0, 1, 2, 3, 7
encode-raw-block-state	contract	encode::block::BlockEncoder::write (crate-private), observed through compress and Encoder	A block written raw or RLE leaves the entropy state a decoder carries (the Huffman code, the three held sequence tables, the repeat offsets) as it was, even when its sequences were coded first and only then found no smaller than the raw block; only a block written compressed commits the state its trial computed. Raw and RLE blocks carry no table and move no repeat offset (§3.1.1.2.2, §3.1.1.5), so every later block's Treeless_Literals_Block, Repeat_Mode and repeat-offset code names state the decoder holds. Its control keeps the raw block's state and gets frames the decoder refuses.	src/encode/mod.rs::a_block_written_raw_leaves_the_decoder_state_alone, src/encode/mod.rs::keeping_a_raw_block_s_state_breaks_the_frame (its control)	MEDIUM. A clone of the state as the trial, committed on one branch; the lemma is that the committed state equals the decoder's after every block, by induction over blocks.	0, 1, 3, 5, 7
encode-block-split	contract	encode::block::BlockEncoder::write with split (crate-private)	With split and at least 2·300 sequences, a block may go out as parts cut at sequence boundaries: each part a block of its own (§3.1.1.2) holding exactly its share of the whole (from its first sequence's literals to its last's match end, the last part also the block's last literals), only the last carrying Last_Block, so the parts decode to the block. Repeat offsets and held tables carry across parts as across any blocks (§3.1.1.5, §3.1.1.3.2.1). A part is halved, at most three times deep, only where its halves, coded one after the other from the state before it, cost less than it does whole; where to cut is a coding choice, and the test samples a block where it pays.	src/encode/block.rs::a_block_of_two_kinds_is_split_where_its_parts_cost_less (its control: unsplit, the block is one)	MEDIUM. A recursive bisection over sequence indices and their prefix sums, with trial encodes from cloned entropy states.	0, 1, 3, 5, 7
encode-damage-refused	round-trip	Decoder, zstd CLI, over frames of compress	The control on the round trips above: a frame of ours with a flipped checksum is refused as a checksum error, cut short is an early end, and with any one bit flipped never decodes to other bytes passed off as the content (most flips are refused); zstd 1.5.7 refuses the same damage. This is a decoder property sampled on the encoder's frames, so it is §1's decoder recovery pass's, not a spec of the encoder.	encode_frames::a_damaged_frame_of_ours_is_refused, encode_cli::the_zstd_cli_refuses_our_frames_damaged	(the decoder's; not assessed here)	—
```

### Test → spec map, encoder (TSV)

```tsv
file	test	primary	also
tests/encode_frames.rs	every_frame_the_encoder_writes_decodes_to_its_input	encode-round-trip	encode-entropy-components, encode-matcher-sound
tests/encode_frames.rs	the_frame_does_not_depend_on_how_writes_are_cut	encode-split-invariance	encode-round-trip
tests/encode_frames.rs	a_window_reaches_as_far_as_it_declares	encode-window-bound	encode-header-layout
tests/encode_frames.rs	a_declared_content_size_is_held	encode-content-size-contract	encode-header-layout
tests/encode_frames.rs	a_damaged_frame_of_ours_is_refused	encode-damage-refused	encode-round-trip (its control)
tests/encode_frames.rs	a_flush_makes_everything_written_decodable	encode-flush-prefix	encode-round-trip
tests/encode_cli.rs	the_zstd_cli_decodes_every_frame_the_encoder_writes	encode-rfc8878-conformance	encode-round-trip
tests/encode_cli.rs	the_zstd_cli_decodes_frames_at_the_largest_window	encode-rfc8878-conformance	encode-window-bound
tests/encode_cli.rs	the_zstd_cli_refuses_our_frames_damaged	encode-damage-refused	encode-rfc8878-conformance (its control)
tests/encode_cli.rs	level_19_stays_within_3_percent_of_zstd_19	encode-parse-sound	(the measured bar; level 9 is its control)
src/encode/mod.rs	a_block_written_raw_leaves_the_decoder_state_alone	encode-raw-block-state	encode-round-trip
src/encode/mod.rs	keeping_a_raw_block_s_state_breaks_the_frame	encode-raw-block-state	(its control)
src/encode/block.rs	a_block_of_two_kinds_is_split_where_its_parts_cost_less	encode-block-split	encode-raw-block-state
src/encode/mod.rs	a_rebase_past_a_wide_ring_changes_no_match	encode-memory-bound	encode-split-invariance
src/encode/mod.rs	a_stream_past_many_rebases_decodes	encode-memory-bound	encode-round-trip
src/encode/mod.rs	below_the_threshold_nothing_rebases	encode-memory-bound	(the control on the rebase leg)
src/encode/mod.rs	the_buffer_stays_within_its_bound	encode-memory-bound	encode-round-trip
src/encode/mod.rs	headers_read_back	encode-header-layout	
src/encode/mod.rs	a_header_with_its_segment_bit_cleared_reads_otherwise	encode-header-layout	(its control)
src/encode/mod.rs	options_clamp_to_valid_bounds	encode-options-clamp	
src/encode/bits.rs	fields_read_back_forward_and_backward	encode-entropy-components	
src/encode/bits.rs	a_field_read_at_the_wrong_width_differs	encode-entropy-components	(its control)
src/encode/fse.rs	histograms_round_trip_through_the_decoder	encode-entropy-components	
src/encode/fse.rs	predefined_distributions_round_trip	encode-entropy-components	
src/encode/fse.rs	a_stream_decoded_under_another_table_differs	encode-entropy-components	(its control)
src/encode/huffman.rs	skewed_flat_and_sparse_alphabets_round_trip	encode-entropy-components	
src/encode/huffman.rs	deep_trees_are_cut_to_eleven_bits	encode-entropy-components	
src/encode/huffman.rs	all_256_bytes_need_the_fse_weight_form	encode-entropy-components	
src/encode/huffman.rs	a_stream_under_another_description_differs	encode-entropy-components	(its control)
src/encode/literals.rs	every_kind_and_width_decodes	encode-entropy-components	
src/encode/literals.rs	a_treeless_section_without_a_table_is_refused	encode-entropy-components	(its control)
src/encode/sequences.rs	length_codes_cover_every_value	encode-entropy-components	
src/encode/sequences.rs	offset_values_resolve_to_their_distance	encode-entropy-components	
src/encode/sequences.rs	a_repeat_code_under_the_other_literal_rule_differs	encode-entropy-components	(its control)
src/encode/matcher.rs	matches_copy_equal_bytes_within_the_window	encode-matcher-sound	
src/encode/matcher.rs	a_wrong_match_is_caught	encode-matcher-sound	(its control)
src/encode/matcher.rs	common_prefix_lengths	encode-matcher-sound	
src/encode/tree.rs	matches_are_real_and_grow	encode-tree-sound	
src/encode/tree.rs	a_wrong_match_is_caught	encode-tree-sound	(its control)
src/encode/opt.rs	a_parse_tiles_the_block_with_real_matches	encode-parse-sound	encode-tree-sound
src/encode/opt.rs	the_anchors_carry_a_copy_past_the_tree_into_the_parse	encode-parse-sound	encode-ldm-sound
src/encode/opt.rs	weights_and_scaling	encode-parse-sound	
src/encode/ldm.rs	a_far_copy_is_found_block_by_block	encode-ldm-sound	
src/encode/ldm.rs	a_copy_past_the_window_is_not	encode-ldm-sound	(its control)
```
