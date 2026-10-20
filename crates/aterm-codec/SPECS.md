<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-codec: spec recovery from its 57 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 19 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Audit amendment: The 2026-10-16 code audit corrected the error docs in §5.2/4 and removed render's unused last parameter/dead arm (§5.3); SafeStream::finish retains end-of-input handling. The dated inventory below records the earlier snapshot.

> Status 2026-10-15: this is the crate's migration worklist. Each spec below is proved through
> Trust/Clean about the compiled Rust function, and then the tests it subsumes are deleted. No test
> here samples nothing, so nothing is deleted ahead of a proof (§4).
>
> **Re-measured 2026-10-16 on Trust seal `150efe4a`: `tty-active-class` and `hex-digit-tables` are
> still blocked; no test is deleted.** Each fn was given a `trust_contract_fn!` contract citing a Clean
> theorem over `trust_import_aterm_codec__<fn>`, run under `TRUST_E6_DEBUG=1`, then reverted. No
> import is minted for any of the five fns, so every theorem fails with
> `Clean island declaration rejected: AutoBoundHeadOverApplied { name: "trust_import_aterm_codec__<fn>", … }`.
> A scratch probe crate separated the causes:
> - `tty::is_active`: the FACET gate refuses the `char as u32` cast (`_2=cast`: "validated
>   closed-fragment firewall declined Pure/Total/Deterministic/NoPanic"). Without the cast a `char`
>   compare is admitted, but the citation is refused: "this clause is outside the exact statement
>   fragment: missing supported type for clause variable `c`". `char` is not a clause carrier (gap 1),
>   and the cast is gap 3.
> - `tty::needs_escape`: "call to `aterm_codec::tty::is_active`, which is not itself structurally
>   certified". It waits on `is_active`.
> - `hex::digit_lower` / `digit_upper`: `admissible=true` but `recognize=None`. The shape gate does
>   not read a 16-arm literal `switch`, and a `u8 → u8` table of the same shape is refused the same
>   way (gap 3). The `char` result is also no clause carrier (gap 1).
> - `hex::nibble`: "local _0 has type Adt { name: "core::option::Option", … } outside the v1 scalar
>   fragment" (gap 1). Its `c - b'0'` arms keep checked `Assert`s, which the NoPanic facet refuses
>   even though the range guard makes them safe (gap 3). The new seal does admit and mint a `u8` range
>   `match` that returns `bool`.
> - Contract predicates that use `as` casts, a `char` literal compare or a Clean-def call
>   (`result == nibble_spec(c)`) are reported "unsupported contract predicate expression".

Source: `crates/aterm-codec` (read only). I read every `#[test]`: `src/base64.rs` 7, `src/crc32.rs` 3,
`src/hex.rs` 2, `src/inflate.rs` 21, `src/json.rs` 3, `src/pct.rs` 2, `src/tty.rs` 5,
`tests/base64.rs` 6 and `tests/inflate_stream.rs` 8, which makes 57. `tests/codec_suite.rs` has no
tests: it only declares `mod base64; mod inflate_stream;`. Its doc comment contains the text
`#[test]`, which is why a line count gives 58. I also read the code each test reaches, including the
digit writers in `src/lib.rs` and `src/inflate/stream.rs` (which has no tests of its own). Nothing was
compiled or run. The claims below come from reading the code.

Result: **35 specs**, covering **all 57 tests**. **0 tests are non-spec samples** (§4).

## 0. Notation used in the statements

- `MAX` = `crate::MAX_INPUT_LEN` = 64 MiB.
- `B64(x)`: the RFC 4648 §4 padded standard-alphabet encoding, as a Lean model
  `List UInt8 → List UInt8`. Each 3-byte group becomes four 6-bit symbols, MSB first. A 1-byte tail
  gives `XX==` and a 2-byte tail gives `XXX=`. `|B64(x)| = 4·⌈|x|/3⌉`.
- `Sym(c)`: the index of `c` in `STANDARD_ALPHABET`, or none (the `STANDARD_DECODE` table).
- `Bits6(t)`: the concatenation of the 6-bit values of `t`, MSB first. `Pack(t)` is the first
  `⌊6|t|/8⌋` bytes of `Bits6(t)`, with the leftover low bits discarded.
- `Lossy(b)`: the Unicode §3.9 / WHATWG maximal-subpart decomposition of a byte string `b`. It is a
  list of items, each `Char c`, `Bad(bytes)` for a maximal ill-formed subpart (std's
  `error_len = Some(n)`), or `Cut(bytes)` for a trailing incomplete well-formed prefix
  (`error_len = None`). It agrees with `std::str::from_utf8`'s `valid_up_to`/`error_len`, and
  `String::from_utf8_lossy(b)` is its rendering with one U+FFFD per `Bad`/`Cut`.
- `bitsLSB(x)`: the bit list of `x`, each byte LSB first (DEFLATE order).
- `RFC1951(x)`: a Clean model of raw DEFLATE decoding over `bitsLSB(x)`, in puff.c semantics, with the
  same error lattice as `InflateError` (`Truncated`, `BadBlockType`, `BadStoredLength`, `BadSymbol`,
  `BadDistance`). It returns `Ok(out, nbits)`, where `nbits` counts the bits up to the end of the final
  block. It includes RFC 1951's completeness rules: the code-length code and the lit/len code are
  complete, and the distance code is complete, empty, or a single length-1 code.
  `used(nbits) = ⌈nbits/8⌉`.
- `Canon(lengths)`: the RFC 1951 §3.2.2 canonical code. Symbol `s` gets code word `Canon(lengths)[s]`
  of length `lengths[s]`, MSB first. `Kraft(lengths) = Σ_{l≠0} 2^-l`.
- `CRC(d)`: CRC-32/ISO-HDLC, defined bitwise with no table: reflected polynomial `0xEDB88320`, init
  `0xFFFFFFFF`, final xor `0xFFFFFFFF`.
- `ADLER(d)`: RFC 1950 §8.2. `A = 1 + Σ dᵢ mod 65521`, `B = Σ_k A_k mod 65521`, `ADLER = B<<16 | A`.
- `ZOK(x, o, n)`: `x[0..2]` is a valid zlib header (CM = 8, `(CMF·256+FLG) % 31 = 0`, FDICT clear),
  `RFC1951(x[2..]) = Ok(o, nb)`, `n = 2 + used(nb) + 4 ≤ |x|`, and `BE32(x[n-4..n]) = ADLER(o)`.
- `GzMember(m, p)`: `m = H ++ D ++ LE32(CRC(p)) ++ LE32(|p| mod 2^32)`. `H` is an RFC 1952 header that
  `parse_gzip_header` accepts with length `|H|`, and `D` is raw DEFLATE with `RFC1951(D) = Ok(p, nb)`
  and `used(nb) = |D|`.
- `ReadAll(R, src, drip, chunk)`: wrap a source that yields at most `drip` bytes of `src` per read,
  then call `read` with a buffer of `chunk` bytes until it returns 0 or an error. The result is the
  concatenation, or the first error.
- `E(c)`: the `tty::push_escape` spelling of one char (spec `tty-push-escape`). `hex2(b)` is
  `format!("{b:02x}")`.
- `JsonRef(s)`: the per-char map `'"'` → `\"`, `'\\'` → `\\`, `\n \r \t \b \f` by name,
  `is_active(c)` → `\u` plus 4 lowercase hex digits, and every other char unchanged. This is the
  `#[cfg(test)]` `reference` fn in `src/json.rs`, lifted to a spec-only model.
- Gap numbers refer to the table in
  the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md`
  §7. **Gap 0 (a seal that publishes Clean rows) blocks every spec and is not repeated in the
  column.** No spec is blocked by gap 0 alone. The two closest (`tty-active-class`,
  `hex-digit-tables`) also need gap 3.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary ones are in §2),
`difficulty` (the Rust features the proof must handle), `blocking_gaps` (audit §7 numbers, gap 0
implied).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
b64-encode-padded	contract	base64::encode, base64::encode_padded	∀ x: |x| > MAX ⇒ encode(x) = Err(InputTooLarge(|x|)). |x| ≤ MAX ⇒ encode(x) = Ok(s) with s.as_bytes() = B64(x). So |s| = 4·⌈|x|/3⌉, s uses only the alphabet and '=', and s ends with exactly (3 − |x| mod 3) mod 3 '=' bytes. The from_utf8_unchecked call is sound because every pushed byte is ASCII.	tests/base64.rs::rfc4648_vectors, tests/base64.rs::ed25519_key_and_signature_shapes	&[u8] → String, as_chunks::<3> loop over fixed arrays, u32 shifts/masks, Vec push, match on rem.len(), unsafe String::from_utf8_unchecked (needs an ASCII invariant on out)	1,5,7,8
b64-roundtrip	round-trip	base64::{encode,decode,decode_strict,decode_with_table,decode_strict_with_table}	∀ x with |x| ≤ MAX: decode_strict(encode(x)?) = Ok(x) and decode(encode(x)?) = Ok(x). A corollary of b64-encode-padded with b64-lenient-decode and b64-strict-canonical. It can also be proved directly by induction over 3-byte groups: Pack(B64 group) = group.	tests/base64.rs::every_short_length_and_every_byte_value_round_trips, tests/base64.rs::fuzz_both_directions	Vec<u8>/String, as_chunks::<3>/<4> loops, Result ? propagation, u32 bit packing, saturating usize counters	1,4,5,7
b64-strict-canonical	contract	base64::{decode_strict,decode_strict_with_table,decode_byte}	∀ t: decode_strict(t) = Ok(x) ⇔ |x| ≤ MAX ∧ B64(x) = t: strict accepts exactly the image of encode and inverts it. Errors: |t| > MAX ⇒ InputTooLarge(|t|); |t| mod 4 = 1 ⇒ InvalidLength(|t|); |t| mod 4 ∈ {2,3} ⇒ InvalidPadding(|t|); otherwise the first failing check, scanning quads left to right, decides: '=' outside the last one or two bytes of the final quad, or any non-alphabet byte ⇒ InvalidByte(pos, byte); nonzero discarded low bits of the final symbol (b & 0xF for XX==, c & 0x3 for XXX=) ⇒ InvalidLastSymbol(pos, byte). Test form: decode_strict(t).ok() = (from_utf8(t) ∧ decode(t) = Ok(x) ∧ encode(x) = t) ? Some(x) : None.	tests/base64.rs::exhaustive_short_strings_are_accepted_only_when_canonical, tests/base64.rs::non_canonical_spellings_are_refused	enumerate over as_chunks::<4>, last-quad branch, early-return Result, Vec push, bit masks	1,5,7
b64-lenient-decode	contract	base64::{decode,decode_with_table,decode_byte}	∀ s: &str, t = s.as_bytes(): |t| > MAX ⇒ InputTooLarge(|t|). t = [] ⇒ Ok([]). Otherwise let p = the number of trailing '=' bytes; p > 2 ⇒ InvalidLength(|t|). Let body = t[..|t|−p]. |body| mod 4 = 1 ⇒ InvalidLength(|body|) (the stripped length). If some byte of body has no Sym, the result is InvalidByte(i, body[i]) for the least such i ('=' inside body counts as invalid). Otherwise Ok(Pack(body)). The padding count is not checked against the length, and the discarded bits are ignored.	-	&str as_bytes, rev().take_while().count(), get(..).unwrap_or, as_chunks::<4> loop with slice-pattern remainder, const-fn-built [u8;256] table (evaluate build_decode_table), Result ?	1,3,5,7
fmt-digit-writers	equivalence	write_usize_decimal, write_hex_byte_upper, base64::DecodeError::fmt, base64::EncodeError::fmt	∀ v: usize: write_usize_decimal writes exactly v.to_string() to f, and returns Ok iff f.write_str does. ∀ b: u8: write_hex_byte_upper writes exactly format!("{b:02X}"). Hence each Display arm of DecodeError and EncodeError writes the concatenation of its literal pieces and these renderings (the literal pieces are definitional, not spec).	-	fmt::Formatter (&mut, write_str as an opaque trace sink), [u8;20] buffer loop with get_mut, usize div/mod, str::from_utf8 on an ASCII slice	1,2,5,7
codec-no-panic	no-panic/bounds	base64::{decode,decode_strict}, inflate::{inflate,inflate_prefix,zlib_decompress,zlib_decompress_prefix}, inflate::stream::{DeflateReader,ZlibReader,GzipReader}::read, pct::decode, tty::{safe,SafeStream::push}	∀ inputs (any byte string, any max_output, any Read source that returns Ok(n ≤ buf.len()) or Err): these calls terminate and neither panic nor index out of bounds. Every decode returns Ok or one of its documented errors; for DEFLATE every input either decodes or returns an InflateError.	src/inflate.rs::fuzz_never_panics_and_respects_cap, tests/inflate_stream.rs::corrupted_raw_deflate_never_panics	slice indexing incl. input[2..len−4] after a len ≥ 6 guard, BitReader u64 shifts (wrapping_shl/shr), loops with a decreasing measure (bits consumed, i < lengths.len()), generic R: Read, Box<Huffman>, drain	1,2,7,8,11
hex-digit-tables	table-property	hex::{digit_lower,digit_upper,nibble}	∀ n: u8: digit_lower(n) / digit_upper(n) is the (n & 0xF)-th char of "0123456789abcdef" / "0123456789ABCDEF". ∀ c: u8: nibble(c) = char::from(c).to_digit(16).map(|d| d as u8), i.e. Some(v) iff c ∈ [0-9a-fA-F] with v its value. Each is a finite case table (16 or 256 cases), decidable by evaluation.	(none primary; sampled by both src/hex.rs tests)	const fn, & mask, match on literals and u8 ranges, u8 → char. Scalar only: no carrier, no loop	1,3
hex-encode	equivalence	hex::{encode_lower,encode_upper,push_lower}	∀ bytes, out: encode_lower(bytes) = concat_b format!("{b:02x}"); encode_upper(bytes) = concat_b format!("{b:02X}"); push_lower(out, bytes) leaves out' = out ++ encode_lower(bytes). Built on hex-digit-tables with the b/16, b%16 split.	src/hex.rs::every_byte_and_digit_matches_std	&mut String push, for over &[u8], u8 div/rem, saturating_mul capacity hint	1,2,5,7
hex-validate	contract	hex::{is_hex,is_lower}	∀ text, len: is_hex(text, len) ⇔ |text| = len ∧ ∀ b ∈ text.bytes(): nibble(b).is_some(). is_lower(text, len) ⇔ |text| = len ∧ ∀ b ∈ text.bytes(): b ∈ [0-9a-f].	(none primary; sampled by src/hex.rs::decoders_refuse_what_is_not_exactly_hex)	&str bytes loop with early return, call to nibble	1,3,7
hex-decode	contract	hex::{decode_vec,decode_into,byte}	∀ text: decode_vec(text) = Some(v) ⇔ |text| is even ∧ every byte has nibble ≠ None, and then v[i] = 16·nibble(text[2i]) + nibble(text[2i+1]); otherwise None (odd length, a sign, a non-hex byte). ∀ text, out: decode_into(text, out) = true ⇔ |text| = 2|out| ∧ decode_vec(text) = Some(v), and then out' = v; out is unspecified after false. Corollary: decode_vec(encode_lower(b)) = decode_vec(encode_upper(b)) = Some(b).	src/hex.rs::decoders_refuse_what_is_not_exactly_hex	as_chunks::<2>, zip with iter_mut over &mut [u8], checked_mul, Option ?, Vec push	1,2,4,5,7
pct-encode	contract	pct::{encode,encode_into}	∀ s: &str, out: encode_into(out, s) leaves out' = out ++ concat_{b ∈ s.bytes()} (b.is_ascii_graphic() ∧ b ≠ '%' ? [b] : "%" ++ HEXUPPER(b>>4) ++ HEXUPPER(b&15)). encode(s) = encode_into("", s). Corollary: the output holds only ASCII-graphic bytes, so no space.	src/pct.rs::encode_is_the_wires_spelling_and_decode_inverts_it	&mut String push, s.bytes() loop, const [u8;16] index	1,2,5,7
pct-decode-total	contract	pct::decode	∀ s: &str: decode(s) = String::from_utf8_lossy(D(s.bytes())). D scans left to right: at i, if bytes[i] = '%' ∧ i+2 < len ∧ bytes[i+1], bytes[i+2] are ASCII hex digits (either case), emit 16·hi + lo and skip 3; otherwise emit bytes[i] and skip 1. Never panics. Corollaries: a string with no well-formed %XX is returned unchanged, and a stray '%' does not consume the escape that follows it.	src/pct.rs::decode_is_total	while loop over byte indices with an i+2 bound, char::to_digit(16) on a byte-as-char, Vec push, from_utf8_lossy (needs the Lossy model)	1,5,7
pct-roundtrip	round-trip	pct::{encode,decode}	∀ s: &str: decode(encode(s)) = s. By pct-encode every '%' in encode(s) starts a well-formed uppercase escape and D inverts each token; the bytes are then s's own valid UTF-8, so from_utf8_lossy is the identity.	(none primary; sampled by src/pct.rs::encode_is_the_wires_spelling_and_decode_inverts_it)	composition of the two contracts, String/UTF-8 validity facts	1,4,5,7
pct-word-line	round-trip	pct::{encode_word,join_words,split_words,decode}	∀ argv of non-empty words w1..wn: split_words(join_words(argv)) = argv. encode_word(w) escapes exactly '%', every char::is_whitespace char and every char::is_control char as uppercase %XX of each UTF-8 byte and keeps every other char, so it holds no whitespace and every '%' in it starts a well-formed escape that decode inverts; join_words joins the encoded words with one ' '. Corollary: a word with no '%', whitespace or control char is its own encoding, so join_words(argv) = argv.join(" ") for an argv of such words.	src/pct.rs::an_argv_line_keeps_every_word_whole	chars loop, encode_utf8, split_whitespace, composition with pct-decode-total	1,3,7
json-escape-reference	equivalence	json::{escape,escape_into,push_u,utf8_len}	∀ s: &str, out: escape_into(out, s) leaves out' = out ++ JsonRef(s), and escape(s) = JsonRef(s). The fast path copies plain runs and decodes only lead bytes C2/D8/E2; it is correct because every is_active char ≥ 0x80 has one of those leads (C2 for U+0080–009F, D8 for U+061C, E2 for U+200E–2069). push_u's two-byte lowercase hex equals {:04x} because every active char is ≤ 0xFFFF.	src/json.rs::escape_spells_rfc_8259_and_the_active_class, src/json.rs::the_fast_path_is_the_reference	while-let over bytes.get(at) with a char-boundary invariant (start ≤ at, both on boundaries), s.get(a..b) str slicing, chars().next(), c.len_utf8(), &mut String	1,2,5,7
json-escape-inert-and-invertible	round-trip	json::escape	∀ s: escape(s) contains no char c with tty::is_active(c), and reading escape(s) as an RFC 8259 string body (a spec-only JSON string decoder) gives back s. A corollary of json-escape-reference: JsonRef emits only non-active chars plus ASCII escapes, and each escape token decodes to its source char.	src/json.rs::the_output_holds_no_active_byte_and_decodes_back	String/char model; needs a Lean JSON-string-body decoder as the reference	1,4,5
tty-active-class	table-property	tty::{is_active,needs_escape}	∀ c: char: is_active(c) ⇔ (c as u32) ∈ [0,0x1F] ∪ [0x7F,0x9F] ∪ {0x061C, 0x200E, 0x200F} ∪ [0x2028,0x202E] ∪ [0x2066,0x2069]. needs_escape(c) ⇔ is_active(c) ∧ c ∉ {'\n','\t'}.	src/tty.rs::the_active_class_is_controls_bidi_and_the_separators	const fn, char as u32 cast, boolean range comparisons, one call (needs_escape → is_active). Straight-line scalar	1,3
tty-push-escape	contract	tty::{push_escape,push_byte_escape}	∀ out, c: push_escape(out, c) leaves out' = out ++ E(c). E('\n') = "\\n", E('\r') = "\\r", E('\t') = "\\t"; for other c < 0x20 and for DEL, E(c) = "\\x" ++ hex2(c); otherwise E(c) = format!("\\u{{{:04x}}}", c as u32) (lowercase, at least four digits, no zeros beyond four). push_byte_escape(out, b) appends "\\x" ++ hex2(b).	src/tty.rs::push_escape_spells_every_class	&mut String, match with guard, iter().skip over a [u32;6] shift array, char as u32	1,2,3,7
tty-safe	contract	tty::{safe,render,SafeStream::{push,finish}}	∀ b: &[u8]: safe(b) = concat_{item ∈ Lossy(b)} R(item), R(Char c) = needs_escape(c) ? E(c) : c, R(Bad bs) = R(Cut bs) = concat_{x ∈ bs} "\\x" ++ hex2(x). safe(b) is Cow::Borrowed(b) exactly when b is valid UTF-8 with no needs_escape char. Corollary: no char of safe(b) satisfies needs_escape, and every printable char of b appears in order.	src/tty.rs::safe_spells_each_class_and_keeps_text	str::from_utf8 with Utf8Error valid_up_to/error_len (needs a std model), Cow, chars() loops, split_at, skip/take iterators	1,2,5,7
tty-stream-split-invariance	equivalence	tty::{SafeStream::{new,push,finish,hold},render}	∀ pieces p1..pk (any k, any lengths, empty pieces included), out: new(); push(p1, out); …; push(pk, out); finish(out) leaves out' = out ++ safe(p1 ++ … ++ pk). Invariant between pushes: the held bytes (≤ 3) are exactly the Cut item of Lossy(prefix so far), and out holds the rendering of everything before it.	src/tty.rs::a_stream_cut_anywhere_renders_as_the_whole	&mut self struct with a [u8;3] carry, [u8;6] joint buffer copy_from_slice, slice re-borrowing, the render loop; needs a Lossy prefix/append lemma	1,2,5,7
crc32-reference	equivalence	crc32::{Crc32::{new,update,finish,of},TABLE}	∀ d: Crc32::of(d) = CRC(d), the bitwise definition. TABLE[n] = 8 reflected shift-xor steps of n (const evaluation over 256 entries), and the table step entry ^ (crc >> 8) equals 8 bitwise steps on the byte. CRC("123456789") = 0xCBF43926 and CRC([]) = 0 are instances.	src/crc32.rs::check_vector	const-block-generated [u32;256], &mut self, for over &[u8], TABLE.get, u32 xor/shift	1,2,5,7
crc32-fold	other (homomorphism)	crc32::Crc32::update	∀ states s, byte strings a, b: update(update(s, a), b) = update(s, a ++ b) and update(s, []) = s. Hence finish after any split of d equals of(d).	src/crc32.rs::folding_is_associative_over_every_split	&mut self two-state reasoning, a slice loop as a fold, List.foldl_append	1,2,7
adler32-reference	equivalence	inflate::{Adler32::{new,update,finish},adler32}	∀ d: adler32(d) = ADLER(d). ∀ states with a, b < 65521 and all splits: update(update(s, x), y) = update(s, x ++ y), and a, b < 65521 holds after every update. The NMAX = 5552 deferred reduction never wraps u32 (from a, b < 65521, 5552 bytes of 0xFF keep b < 2^32), so wrapping_add equals mathematical addition.	-	chunks(NMAX) nested loops, wrapping_add with a non-wrap bound proof (nonlinear: 5552·255 plus a triangular sum), u32 mod, &mut self	1,2,5,7
huffman-new-kraft	contract	inflate::Huffman::new	∀ lengths: &[u16]: |lengths| > 288 or some length > 15 ⇒ Err(BadSymbol). Kraft(lengths) > 1 (over-subscribed) ⇒ Err(BadSymbol). Otherwise Ok((h, left)) with left = 2^15·(1 − Kraft(lengths)) ≥ 0, so left = 0 ⇔ the code is complete; h.count[l] = #{s : lengths[s] = l} for 1 ≤ l ≤ 15; h.symbol lists the used symbols sorted by (length, symbol), the canonical order of Canon(lengths).	src/inflate.rs::over_subscribed_huffman_rejected	[u16;16] counts, i32 Kraft recurrence with saturating ops, prefix-sum loop over zipped iterators, Vec<u16> fill via next[] cursors, a struct in a returned tuple	1,3,5,7
huffman-walk-canonical	contract	inflate::{Huffman::decode_bitwise,fixed_lit,fixed_dist}	∀ h = Huffman::new(lengths) (Ok), ∀ reader states br: decode_bitwise(br) = Ok(s) ⇔ the next lengths[s] stream bits, read MSB first, equal Canon(lengths)[s], and it then consumes exactly lengths[s] bits. No code word prefixing the remaining bits within 15 bits ⇒ Err(BadSymbol); bits running out first ⇒ Err(Truncated). fixed_lit/fixed_dist are Huffman::new of the RFC 1951 §3.2.6 lengths (8/9/7/8 and 30×5), so new never fails there.	(none primary; sampled by the two src/inflate.rs root_table_* tests)	&mut BitReader, i32 canonical-walk loop over count.iter().skip(1), Vec get, Result ?	1,2,5,7
root-table-agrees-with-walk	equivalence	inflate::{build_root_table,reverse_code,Huffman::{arm_root,decode},block_loop,inflate_block}	∀ h from Huffman::new with left ≥ 0, armed or not, ∀ br: decode(br) = decode_bitwise(br). On Ok both leave br in the same state; on Err both return the same error value, and the reader post-state may differ (unobservable: every caller aborts or restores a checkpoint). Table: build_root_table puts packed (len, sym) in exactly the slots whose low len bits equal reverse(Canon[sym], len) for len ≤ ROOT_BITS, and every other slot is 0; for a complete code with max length ≤ 9 (fixed_lit) no slot is 0. Lifted: inflate/zlib_decompress return the same Result for every input whether or not any block arms its tables, which makes the cfg(test) oracle::without_root_table switch removable.	src/inflate.rs::root_table_resolves_every_fixed_literal_code, src/inflate.rs::root_table_falls_back_for_codes_longer_than_root_bits, src/inflate.rs::table_and_bitwise_decoders_agree_on_corpus_prefixes_and_mutations	Vec<u16> of 512 with stride-filling while-let get_mut, nested enumerate loops, peek_bits/consume_bits on &mut BitReader, const generic bool TABLE in block_loop	1,2,5,7,11
bitreader-is-bitstream	refinement	inflate::BitReader::{new,refill,read_bit,read_bits,peek_bits,consume_bits,align_to_byte,read_aligned_byte,read_aligned_u16}	Abstraction pos(br) = 8·byte_pos − bits_in_buffer, with Inv: bits_in_buffer ≤ 64, byte_pos ≤ |data|, and bit_buffer's low bits_in_buffer bits = bitsLSB(data)[pos .. pos+bits_in_buffer] with the higher bits zero. Under Inv: read_bits(n ≤ 32) = Ok(v) ⇔ pos+n ≤ 8|data|, and then v = Σ_{i<n} bitsLSB(data)[pos+i] << i and pos' = pos+n; otherwise Err(Truncated). read_bit is n = 1. peek_bits(w) returns (the low min(w, available) bits, avail) without moving pos. consume_bits(k ≤ avail) advances pos by k. align_to_byte rounds pos up to a multiple of 8. read_aligned_byte/u16 read whole little-endian bytes at an aligned pos. Every op preserves Inv.	-	&mut self struct with a u64 accumulator, wrapping_shl/shr under bounds, refill while-loop, saturating ops; an abstraction function over a borrowed slice	1,2,5,7
inflate-refines-rfc1951	refinement	inflate::{inflate,inflate_prefix,inflate_stored,inflate_block,block_loop,read_dynamic_tables}	∀ x, max_output = usize::MAX: RFC1951(x) = Ok(o, nb) ⇒ inflate_prefix(x, MAX) = Ok((o, used(nb))); RFC1951(x) = Err(e) ⇒ inflate_prefix(x, MAX) = Err(e); inflate = inflate_prefix(..).0. Composes bitreader-is-bitstream, huffman-walk-canonical and root-table-agrees-with-walk. Back-references copy out[cur−d+i] byte by byte, and the extend_from_within fast path equals that when d ≥ length. The python-zlib known-answer vectors (empty, fixed literals, stored, fixed backrefs, dynamic, raw) are instances.	-	Vec<u8> push/extend_from_within/reserve, nested loops (block loop, repeat loops), Option<(Huffman,Huffman)> get_or_insert_with closure, &mut Huffman, match on btype; a Lean RFC 1951 model as the reference	1,2,4,5,7,10,13
inflate-dynamic-table-completeness	contract	inflate::read_dynamic_tables	∀ br: read_dynamic_tables(br) = Ok((lit, dist)) ⇒ hlit ≤ 286 ∧ hdist ≤ 30, the code-length code is complete (left = 0), the lit/len code is complete (Kraft = 1), the dist code is complete, empty, or a single length-1 code, the repeat codes 16/17/18 never write past hlit+hdist, and 16 never comes first. Any violation ⇒ Err(BadSymbol) (or Truncated if the bits run out first). Corollary: a stream whose lit/len code is incomplete is refused even when its Adler-32 matches.	src/inflate.rs::incomplete_dynamic_huffman_rejected	[u16;19] and Vec<u16> length arrays, while loop with inner repeat for-loops, split_at, calls to Huffman::new	1,2,4,7
inflate-output-cap	contract	inflate::{inflate,inflate_prefix,zlib_decompress,zlib_decompress_prefix}	∀ x, m: inflate(x, m) = Ok(o) ⇒ |o| ≤ m. inflate(x, m) = Ok(o) ⇔ inflate(x, usize::MAX) = Ok(o) ∧ |o| ≤ m, so the cap only turns an over-long decode into an error and never changes accepted output. inflate(x, m) = Err(OutputTooLarge) ⇒ ¬(inflate(x, MAX) = Ok(o) ∧ |o| ≤ m). The same three facts hold for zlib_decompress and zlib_decompress_prefix.	src/inflate.rs::output_cap_rejects_bomb	invariant out.len() ≤ max_output across loops; saturating_add guards; Vec length reasoning	1,4,5,7
inflate-prefix-locality	contract	inflate::inflate_prefix	∀ a, m with inflate_prefix(a, m) = Ok((o, u)): u ≤ |a|, and ∀ a' with a'[..u] = a[..u]: inflate_prefix(a', m) = Ok((o, u)). The decoder reads no bit beyond the final block: refill's look-ahead is undone in the `used` computation, and a partly consumed last byte counts.	(none primary; sampled by src/inflate.rs::prefix_form_tolerates_bytes_after_the_stream)	reasoning that BitReader observations depend only on data[..⌈pos/8⌉] (look-ahead bytes never decide a result: peek's len ≤ avail check), loops	1,5,7
zlib-framing	contract	inflate::{zlib_decompress,zlib_decompress_prefix,check_zlib_header}	∀ x (cap as in inflate-output-cap; shown with m = MAX): zlib_decompress_prefix(x) = Ok((o, n)) ⇔ ZOK(x, o, n). zlib_decompress(x) = Ok(o) ⇔ ZOK(x, o, |x|), so the checksum must sit exactly where the stream ends; hence zlib_decompress(x) = Ok(o) ⇔ zlib_decompress_prefix(x) = Ok((o, |x|)). With inflate-prefix-locality, ZOK(s, o, |s|) ⇒ ∀ p: zlib_decompress_prefix(s ++ p) = Ok((o, |s|)), and for p ≠ [] zlib_decompress(s ++ p) = Err. Errors: |x| < 6 ⇒ Truncated; a bad CM, FCHECK or FDICT ⇒ BadZlibHeader; an Adler-32 mismatch ⇒ BadChecksum; in the whole-buffer form a stream that ends before the last 4 bytes ⇒ BadChecksum; in the prefix form a trailer that runs past the end ⇒ Truncated.	src/inflate.rs::prefix_form_tolerates_bytes_after_the_stream, src/inflate.rs::whole_buffer_form_reads_the_checksum_where_the_stream_ends, src/inflate.rs::prefix_form_still_checks_the_adler_and_the_length, src/inflate.rs::bad_zlib_header_rejected	slice indexing after length guards, u16 header arithmetic mod 31, from_be_bytes, composition with the inflate_prefix and adler32 contracts	1,4,5
stream-deflate-refines-oneshot	equivalence	inflate::stream::{DeflateReader::{new,read,advance,refill},Inflater::{pump,compact},decode_one,take_pending}	∀ body, drip ≥ 1, chunk ≥ 1: ReadAll(DeflateReader, body, drip, chunk) = Ok(o) ⇔ inflate(body, usize::MAX) = Ok(o). An Err from inflate maps through io_err (Truncated → UnexpectedEof, else InvalidData(e)). Bytes after the final block are never consumed. Lemmas: (1) each pump unit either completes or returns NeedInput with the Inflater unchanged; (2) compact preserves the observable state (≥ WINDOW bytes of history kept, distance ≤ 32768 checked against `produced`); (3) every byte reaches the caller exactly once and in order through take_pending.	tests/inflate_stream.rs::every_read_and_refill_granularity_matches_the_one_shot	generic R: Read (trait, io::Result, Interrupted retry loop), enum Phase with Box<Huffman> fields matched by &mut, closures returning Result, Vec drain/extend, u64 counters, a bisimulation between two drivers	1,2,4,5,7,8,11,13
stream-zlib-refines-prefix	equivalence	inflate::stream::{ZlibReader::read,DeflateReader::fill_to}, inflate::{check_zlib_header,Adler32}	∀ x, drip ≥ 1, chunk ≥ 1: ReadAll(ZlibReader, x, drip, chunk) = Ok(o) ⇔ ∃ n: zlib_decompress_prefix(x, usize::MAX) = Ok((o, n)). Bytes after x[..n] are never read, so ReadAll on x ++ p equals ReadAll on x. read returns Ok(0) only after the Adler-32 matched. A truncated x or a spoiled header, body or trailer gives Err.	tests/inflate_stream.rs::zlib_streams_like_the_one_shot_prefix_form	state enum ZlibState, generic R: Read, running Adler32 over output pieces (the adler32-reference fold), cursor surgery (cursor.pos = 2)	1,2,4,5,7,8,11
gzip-header-parse	contract	inflate::stream::parse_gzip_header	∀ buf: parse_gzip_header(buf) = Ok(Some(n)) ⇔ buf[0..2] = 1F 8B ∧ buf[2] = 8 ∧ FLG & 0xE0 = 0 and buf[..n] is the RFC 1952 header: 10 fixed bytes, then (FEXTRA ? 2+XLEN bytes), (FNAME ? NUL-terminated), (FCOMMENT ? NUL-terminated), (FHCRC ? 2 bytes equal to CRC(buf[..n−2]) & 0xFFFF), with n ≤ MAX_GZIP_HEADER. Ok(None) ⇔ buf is a proper prefix of such a header that could still complete within the limit. Err otherwise (bad magic or method, a reserved bit, an FHCRC mismatch, too long). Monotone: Ok(Some(n)) on buf ⇒ Ok(Some(n)) on buf ++ more.	tests/inflate_stream.rs::optional_header_fields_are_skipped_and_reserved_bits_refused, tests/inflate_stream.rs::a_wrong_header_checksum_is_refused	slice get chains, iter().position, loop over a [bool;2], saturating usize, a Crc32::of call, io::Error construction	1,4,5,7,8
gzip-reader-refines-members	equivalence	inflate::stream::GzipReader::{new,read,begin_body,end_body,verify_trailer,read_source,read_source_into_frame}	∀ x, drip ≥ 1, chunk ≥ 1: ReadAll(GzipReader, x, drip, chunk) = Ok(o) ⇔ ∃ k ≥ 1, members m1..mk, payloads p1..pk with x = m1 ++ … ++ mk, ∀ i GzMember(mi, pi), and o = p1 ++ … ++ pk. Otherwise Err (empty x, truncated x, a trailing non-member, a CRC or ISIZE mismatch, a bad header, bad DEFLATE). Corollaries: every proper prefix of a one-member stream is refused; a one-bit flip of a valid member is refused or yields the same payload (CRC-32 detects every single-bit error).	tests/inflate_stream.rs::concatenated_members_are_one_stream, tests/inflate_stream.rs::truncated_and_corrupted_gzip_is_refused, tests/inflate_stream.rs::a_wrong_trailer_is_refused	generic R: Read, GzState machine, mem::take buffer hand-off between frame and inflater, split_off/drain, Crc32 fold over delivered pieces, u64 member length mod 2^32	1,2,4,5,7,8,11
```

### Distribution by kind (36)

| kind | count | ids |
|---|---|---|
| contract | 16 | b64-encode-padded, b64-strict-canonical, b64-lenient-decode, hex-validate, hex-decode, pct-encode, pct-decode-total, tty-push-escape, tty-safe, huffman-new-kraft, huffman-walk-canonical, inflate-dynamic-table-completeness, inflate-output-cap, inflate-prefix-locality, zlib-framing, gzip-header-parse |
| equivalence | 10 | fmt-digit-writers, hex-encode, json-escape-reference, tty-stream-split-invariance, crc32-reference, adler32-reference, root-table-agrees-with-walk, stream-deflate-refines-oneshot, stream-zlib-refines-prefix, gzip-reader-refines-members |
| round-trip | 4 | b64-roundtrip, pct-roundtrip, pct-word-line, json-escape-inert-and-invertible |
| refinement | 2 | bitreader-is-bitstream, inflate-refines-rfc1951 |
| table-property | 2 | hex-digit-tables, tty-active-class |
| no-panic/bounds | 1 | codec-no-panic |
| other | 1 | crc32-fold (homomorphism) |

### Blocking-gap histogram (gap → specs it blocks)

| gap | specs | note |
|---|---|---|
| 0 | 35 | every spec (seal) |
| 1 | 33 | all but `hex-digit-tables`, `tty-active-class` |
| 2 | 20 | |
| 3 | 6 | the only gap besides 0 on the two scalar specs |
| 4 | 12 | |
| 5 | 28 | |
| 7 | 31 | |
| 8 | 6 | `from_utf8_unchecked`, `Read` sources |
| 10 | 1 | `inflate-refines-rfc1951` |
| 11 | 5 | generic `R: Read`, const-generic `block_loop` |
| 13 | 2 | closures (`get_or_insert_with`, pump closures) |

## 2. Test → spec map (TSV; all 57)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/crc32.rs	check_vector	crc32-reference	
src/crc32.rs	folding_is_associative_over_every_split	crc32-fold	crc32-reference
src/hex.rs	every_byte_and_digit_matches_std	hex-encode	hex-digit-tables, hex-decode
src/hex.rs	decoders_refuse_what_is_not_exactly_hex	hex-decode	hex-validate, hex-digit-tables
src/inflate.rs	prefix_form_tolerates_bytes_after_the_stream	zlib-framing	inflate-prefix-locality
src/inflate.rs	whole_buffer_form_reads_the_checksum_where_the_stream_ends	zlib-framing	
src/inflate.rs	prefix_form_still_checks_the_adler_and_the_length	zlib-framing	adler32-reference
src/inflate.rs	output_cap_rejects_bomb	inflate-output-cap	
src/inflate.rs	bad_zlib_header_rejected	zlib-framing	
src/inflate.rs	incomplete_dynamic_huffman_rejected	inflate-dynamic-table-completeness	adler32-reference
src/inflate.rs	over_subscribed_huffman_rejected	huffman-new-kraft	
src/inflate.rs	root_table_resolves_every_fixed_literal_code	root-table-agrees-with-walk	huffman-walk-canonical
src/inflate.rs	root_table_falls_back_for_codes_longer_than_root_bits	root-table-agrees-with-walk	huffman-walk-canonical
src/inflate.rs	table_and_bitwise_decoders_agree_on_corpus_prefixes_and_mutations	root-table-agrees-with-walk	
src/inflate.rs	fuzz_never_panics_and_respects_cap	codec-no-panic	inflate-output-cap
src/json.rs	escape_spells_rfc_8259_and_the_active_class	json-escape-reference	tty-active-class
src/json.rs	the_fast_path_is_the_reference	json-escape-reference	
src/json.rs	the_output_holds_no_active_byte_and_decodes_back	json-escape-inert-and-invertible	
src/pct.rs	encode_is_the_wires_spelling_and_decode_inverts_it	pct-encode	pct-roundtrip
src/pct.rs	decode_is_total	pct-decode-total	codec-no-panic
src/pct.rs	an_argv_line_keeps_every_word_whole	pct-word-line	pct-decode-total
src/tty.rs	the_active_class_is_controls_bidi_and_the_separators	tty-active-class	
src/tty.rs	safe_spells_each_class_and_keeps_text	tty-safe	tty-push-escape
src/tty.rs	a_stream_cut_anywhere_renders_as_the_whole	tty-stream-split-invariance	tty-safe
src/tty.rs	push_escape_spells_every_class	tty-push-escape	
tests/base64.rs	rfc4648_vectors	b64-encode-padded	b64-roundtrip
tests/base64.rs	every_short_length_and_every_byte_value_round_trips	b64-roundtrip	
tests/base64.rs	exhaustive_short_strings_are_accepted_only_when_canonical	b64-strict-canonical	
tests/base64.rs	non_canonical_spellings_are_refused	b64-strict-canonical	b64-lenient-decode
tests/base64.rs	ed25519_key_and_signature_shapes	b64-encode-padded	b64-roundtrip
tests/base64.rs	fuzz_both_directions	b64-roundtrip	b64-strict-canonical
tests/inflate_stream.rs	every_read_and_refill_granularity_matches_the_one_shot	stream-deflate-refines-oneshot	gzip-reader-refines-members
tests/inflate_stream.rs	concatenated_members_are_one_stream	gzip-reader-refines-members	
tests/inflate_stream.rs	truncated_and_corrupted_gzip_is_refused	gzip-reader-refines-members	
tests/inflate_stream.rs	corrupted_raw_deflate_never_panics	codec-no-panic	stream-deflate-refines-oneshot
tests/inflate_stream.rs	optional_header_fields_are_skipped_and_reserved_bits_refused	gzip-header-parse	
tests/inflate_stream.rs	a_wrong_header_checksum_is_refused	gzip-header-parse	crc32-reference
tests/inflate_stream.rs	a_wrong_trailer_is_refused	gzip-reader-refines-members	
tests/inflate_stream.rs	zlib_streams_like_the_one_shot_prefix_form	stream-zlib-refines-prefix	zlib-framing
```

## 3. Coverage

- 57 tests read; all 57 map to one of the 35 specs as primary. None is a non-spec sample (§4).
- The biggest collapses: `inflate-refines-rfc1951` (6 primary), `b64-lenient-decode` (5),
  `zlib-framing` (5), `gzip-reader-refines-members` (3), `root-table-agrees-with-walk` (3),
  `b64-roundtrip` (3), `codec-no-panic` (3).
- Five specs have no primary test. They are lemmas the others are built on: `hex-digit-tables`,
  `hex-validate`, `pct-roundtrip`, `huffman-walk-canonical` and `inflate-prefix-locality`. Each is
  sampled as a secondary in §2, and proving it is what makes the primary spec cheap.
- `inflate-refines-rfc1951` needs a Clean RFC 1951 model, which does not exist yet. With it, the
  inflate specs `inflate-output-cap`, `zlib-framing` and the three stream refinements become
  compositions.

## 4. Tests that are not spec samples

None. Every test samples a general property. Counts by action: **delete 0, become a spec 0, keep 0**.

The migration still deletes test-only **scaffolding** together with the tests that use it, once the
spec that replaces it is Certified:

| scaffolding | where | deleted with |
|---|---|---|
| `oracle::without_root_table` switch | `crates/aterm-codec/src/inflate.rs` (the `#[cfg(test)]` hook in the block decoder) | `root-table-agrees-with-walk` |
| the `reference` escape fn | `crates/aterm-codec/src/json.rs` `#[cfg(test)]` | `json-escape-reference` (it becomes the spec-only `JsonRef` model) |

## 5. Implied specs that look wrong, or disagree with the code

1. **The 58-test count is off by one.** There are 57 `#[test]` fns. `tests/codec_suite.rs` has none.
   Its doc comment contains the text `#[test]`, and the file only declares
   `mod base64; mod inflate_stream;`.
2. **`Huffman::decode`'s doc overclaims.** It says the table and walk paths return "the same symbol or
   the same error at the same point". That holds for the Ok result and for the error value. On a
   `Truncated` error the walk has consumed bits and the peek path has not, so the reader post-states
   differ. This is harmless because callers abort or restore a checkpoint, but
   `root-table-agrees-with-walk` must not claim equal post-state on Err.
3. **`tty::render`'s `last` parameter is dead.** Every caller passes `last = false`
   (`src/tty.rs:144` and `:154`), so the `Some(None) =>` escape-the-trailing-cut arm is never taken.
   `SafeStream::finish` handles that case itself. Drop the parameter, or state `tty-safe` without it.
4. **`DecodeError::InvalidLength`'s doc disagrees with lenient decode.** The doc says "not a multiple of
   4 when padded", but the lenient decode never checks that: `"Zg="` decodes to `"f"`. For a 1-mod-4
   body it reports the *stripped* length, so `"A="` gives `InvalidLength(1)`, not 2. Either the doc or
   the reported length is wrong. `b64-lenient-decode` states the code as it is.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone. The cheapest are the two blocked by gap 0 plus gap 3, which are
scalar `const fn`s. They are the natural first E6 widening increments (casts, `&`, literal/range
`match`, one call).

1. **`tty-active-class`** (gaps 1, 3; re-measured 2026-10-16, see the status note). `is_active` is a straight-line `char as u32` range test, and
   `needs_escape` adds one call. It retires one test and is the class every `tty`/`json` spec cites.
2. **`hex-digit-tables`** (gaps 1, 3; re-measured 2026-10-16, see the status note). `digit_lower`/`digit_upper` are a 16-arm literal `match` on
   `n & 0xF`, and `nibble` is a 256-case range `match`. These are decidable by evaluation. This spec is
   the lemma under `hex-encode`, `hex-validate` and `hex-decode`.
3. **Pure-arithmetic cores as Clean theorems first**, bound to code later: the Adler-32 NMAX non-wrap
   bound and the CRC table-step lemma (`TABLE[n]` equals 8 bitwise steps). Each is a closed Lean fact
   that needs no carrier. They become `adler32-reference` and `crc32-reference` once gaps 1/2/7 land.
4. **The first carrier-and-loop specs** (gaps 1, 2, 5, 7), in this order: `hex-validate`,
   `hex-encode`, `pct-encode`, `crc32-fold`, `crc32-reference`, `hex-decode`. Each is one byte loop
   that appends to a `String` or folds a `u32`. They are the smallest instances of the gap-1/gap-7
   work and retire 6 tests between them.
5. **base64**: `b64-encode-padded` (also gap 8 for `from_utf8_unchecked`), `b64-lenient-decode`,
   `b64-strict-canonical`, then `b64-roundtrip` as a gap-4 composition, and `fmt-digit-writers`. Together
   they retire 13 tests.
6. **Text escaping** (this needs the `Lossy` model): `tty-push-escape`, `pct-decode-total`,
   `pct-roundtrip`, `json-escape-reference`, `json-escape-inert-and-invertible`, `tty-safe`, and
   `tty-stream-split-invariance`.
7. **DEFLATE core**: `bitreader-is-bitstream` → `huffman-new-kraft` → `huffman-walk-canonical` →
   `root-table-agrees-with-walk` (gap 11) → `inflate-dynamic-table-completeness` → the RFC 1951 Clean
   model and `inflate-refines-rfc1951` (gaps 10, 13) → `inflate-output-cap`, `inflate-prefix-locality`
   → `zlib-framing`. `codec-no-panic` comes free once the strict lane discharges panic-freedom.
8. **Streams last** (gaps 8, 11, 13: `R: Read`, closures): `gzip-header-parse`,
   `stream-deflate-refines-oneshot`, `stream-zlib-refines-prefix`, `gzip-reader-refines-members`.
   Factor each reader's pure step out of the `Read` shell first (plan step 4).
