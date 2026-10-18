<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-zstd: spec recovery from 18 of its 61 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 6 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-17: the rows naming `a_wrong_seed_or_length_is_caught` and the six
> `tests/libzstd.rs` tests are gone too. `tests/libzstd.rs` left with the `zstd` dev-dependency;
> its two raw-frame cases are cited in `tests/decode_frames.rs`, where they now run on aterm's
> decoder.

> Status 2026-10-16: this is the crate's migration worklist for the frame writer and XXH64.
> Each of the 7 specs below is proved through Trust/Clean, and then the tests it subsumes
> are deleted. The 4 checks in §4 sample no spec and can be deleted now. The 43 decoder
> tests (`src/decode/*`, `tests/cli_decode.rs`, `tests/decode_libzstd.rs`) are not covered
> here. They need a separate recovery pass, which this file will gain as its own sections.

Source: `crates/aterm-zstd` (read only). I read all 18 `#[test]` fns in scope:
`src/frame.rs` 4, `src/xxh64.rs` 4, `tests/cli_frames.rs` 1, `tests/libzstd.rs` 6 and
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
src/xxh64.rs	digest_then_continue	xxh64-streaming-refines-oneshot	(its stated claim holds by typing; §5.1)
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

Status (2026-10-16): `xxh64_recorded::a_wrong_seed_or_length_is_caught` is deleted. The two `libzstd::` entries were not found: `tests/libzstd.rs` no longer exists on this branch.

Status (2026-10-17): all four are gone, and so is their table. The three `libzstd::` tests went with `tests/libzstd.rs` when the `zstd` dev-dependency left; its raw-frame cases now run on aterm's decoder in `tests/decode_frames.rs`. Two were negative controls on a checksum comparison, and two tested libzstd itself, not aterm code. The `WINDOW_MAX` doc in `src/frame.rs` names libzstd's `ZSTD_WINDOWLOG_LIMIT_DEFAULT` (2^27) and its streaming decoder's `frameParameter_windowTooLarge` cap (2^27 + 1), so the comments no longer disagree.

## 5. Implied specs that look wrong, or disagree with the code

1. **`digest_then_continue` states a claim the types already guarantee.** Its doc says
   "digest reads the state without consuming it", and `digest` takes `&self`. Its live content
   is only that updates after a digest stay correct, which `xxh64-streaming-refines-oneshot`
   covers.
2. **The streaming invariant needs the length mod 2^64.** The `Xxh64` doc says "Every byte
   fed, modulo 2^64". `XXH64Spec` must take `len mod 2^64` (the reference's `total_len`).
   Otherwise `Inv` is false past 2^64 fed bytes. That cannot happen in practice, but a
   quantified proof must handle it.
3. **`sanity()` is defined twice.** `src/xxh64.rs` (test module) and `tests/support/mod.rs`
   both define the same test-input generator. The Lean model needs exactly one definition.

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
