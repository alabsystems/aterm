<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-digest: the SHA-256 specs

aterm-digest re-exports astream-digest's `Sha256`, `HmacSha256`, `hmac_sha256_parts`,
`hkdf_extract`, `hkdf_expand_32`, `ct_eq`, `InvalidLength` and `MacError`. This file holds the
specs of astream-digest's SHA-256 (the `sha256-*` rows and `opaque-debug`), which the HMAC and HKDF
rows of `vendor/astream/crates/astream-digest/SPECS.md` build on.

Each row is a property its cited tests sample, stated so that a Trust/Clean proof against the
compiled Rust function can retire those tests. Test paths under `../../vendor/astream/` are
astream-digest's.

Gap numbers refer to the gap table in `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.
Gap 0 (a seal that publishes Clean-discharged rows) blocks every spec, so every row lists it.

## 0. Notation used in the statements

- `FIPS.SHA256(m)`: a Clean model of FIPS 180-4 SHA-256 over `List UInt8`. It pads per §5.1.1,
  parses into 512-bit blocks, folds `FIPS.compress` from `H_INIT` per §6.2.2, and serialises the
  eight words big-endian. It is defined for |m| < 2^61 bytes (2^64 bits). The published
  FIPS/NIST vectors (empty, "abc", 448-bit, 896-bit, 10^6 'a') become kernel-evaluated
  (`decide`/`rfl`) facts about the **model**: they validate the model against the standard and
  are no longer tests of the Rust.
- `FIPS.compress(H, B)`: the §6.2.2 compression function on `[u32;8] × [u8;64]`.
- `Pad(st, tail, n)`: given chaining state `st`, a live tail with |tail| < 64, and a byte count
  `n : u64`, the result of absorbing `tail ++ [0x80] ++ 0^k ++ be64(n·8 mod 2^64)` with
  `FIPS.compress` (k minimal so the total is a multiple of 64: one block if |tail| ≤ 55, two if
  56..=63), then serialising big-endian.
- `ShaInv(h, M)`: the abstraction of a `Sha256` value `h` that has absorbed the ghost message `M`:
  `h.buf_len < 64` ∧ `h.buf_len = |M| mod 64` ∧ `h.len = |M| mod 2^64` ∧
  `h.buf[..buf_len]` = the last `|M| mod 64` bytes of M ∧
  `h.state` = foldl FIPS.compress H_INIT (the first ⌊|M|/64⌋ blocks of M).
  The bytes `h.buf[buf_len..]` are dead (unconstrained).
- `feed(h, [x1..xn])` = `h.update(x1); …; h.update(xn)` (any n ≥ 0, any slice lengths, empty
  pieces included).
- **Precondition everywhere:** total absorbed length < 2^61 bytes (a legal FIPS message).
  `update_slice` adds to `len` with `wrapping_add` and `finalize` takes `len * 8` with
  `wrapping_mul`, so longer messages silently diverge from FIPS. The `len` field's doc states the
  bound, but no `requires` does (§4.5).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking_gaps (audit §7)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
sha256-compress-refines-fips	refinement	compress (with sha256_round, sha256_ring_word)	∀ state : [u32;8], block : [u8;64]: after compress(&mut state, &block), state' = FIPS.compress(state, block) (§6.2.2: W[0..16] = big-endian words of block; W[t] = σ1(W[t-2]) + W[t-7] + σ0(W[t-15]) + W[t-16] mod 2^32 for t ∈ 16..64; 64 rounds with K; add the working variables into state). The code keeps W as a ring of its last sixteen words, W[t] overwriting W[t-16] in slot t & 15, and runs two rounds a loop pass. Total and panic-free: every index into the ring and into K is a literal or masked (& 15, & 63).	../../vendor/astream/crates/astream-digest/src/lib.rs::the_ring_schedule_compression_gives_the_transcriptions_state_on_random_blocks (secondary for the_length_field_is_64_bits_wide)	MEDIUM: &mut [u32;8], a [u32;16] ring updated in place, two fixed-bound while loops (0..16 and 16..64, two rounds a pass), a by-value [u32;8] round fn with array destructuring, literal-index big-endian loads, rotate_right, shifts, wrapping_add. No heap. The Clean model needs UInt32 bit-vector arithmetic.	0,1,2,3,7,14
sha256-finalize-pads	contract	Sha256::finalize (with write_pad_len_be, encode_state_be)	∀ h with h.buf_len < 64 (any state, any len, reachable or not): h.finalize() = Pad(h.state, h.buf[..h.buf_len], h.len). So the length field is the full 64-bit big-endian encoding of len·8 mod 2^64 (bits above 31 included), a tail of 56..=63 bytes spills the length into a second block, and the 0x80 lands at buf[buf_len] (the index mask & 63 is the identity there). With ShaInv(h,M): h.finalize() = FIPS.SHA256(M).	../../vendor/astream/crates/astream-digest/src/lib.rs::the_length_field_is_64_bits_wide	MEDIUM: self by value, masked array writes, two zero-fill while loops, a branch on pos > 56, calls to compress (callee ensures), write_pad_len_be and encode_state_be (shifts, literal indexes)	0,1,2,3,4,7
sha256-state-invariant	invariant	Sha256::new, Sha256::default, Sha256::update_slice, Sha256::update, Sha256::clone	ShaInv(Sha256::new(), []) ∧ ShaInv(Sha256::default(), []) (Default delegates to new). ∀ h, M, x with ShaInv(h,M) ∧ |M|+|x| < 2^61: after h.update_slice(x), ShaInv(h', M ++ x), and update_slice does not panic (buf_len < 64 on entry, and every buffer index is masked & 63). The whole blocks it compresses straight from x while the buffer is empty are the blocks the byte path would fill the buffer with, so both paths keep ShaInv. h.update(x) = h.update_slice(x.as_ref()). Clone preserves the abstraction: ShaInv(h,M) ⇒ ShaInv(h.clone(),M), and the two values evolve independently afterwards (no shared state).	../../vendor/astream/crates/astream-digest/src/lib.rs::{default_matches_new,clone_forks_the_hash_state}	HARD: &mut self on a struct with [u32;8], [u8;64], usize and u64 fields; a loop around a while-let over split_first_chunk::<64> and a let-else slice pattern; masked indexes; a call to the buf_len_is_partial contract; wrapping_add; derive(Clone); the generic impl AsRef<[u8]> wrapper update	0,1,2,3,4,5,7,11
sha256-digest-refines-fips	refinement	Sha256::digest_slice, Sha256::digest (via new, update_slice, finalize)	∀ m : &[u8] with |m| < 2^61: Sha256::digest_slice(m) = Sha256::digest(m) = FIPS.SHA256(m). A corollary of sha256-state-invariant and sha256-finalize-pads. The FIPS 180-4/NIST vectors and the recorded length sweeps (every length 0..=1024, and selected lengths to 100,000 bytes) are instances. The vectors move to evaluated facts about FIPS.SHA256 (model validation).	../../vendor/astream/crates/astream-digest/src/lib.rs::{fips_empty_message,fips_abc,fips_448_bit_message,fips_896_bit_message,fips_one_million_a}; ../../vendor/astream/crates/astream-digest/tests/lengths_and_streaming.rs::sha256_over_every_length_and_large_inputs_is_pinned; ../../vendor/astream/crates/astream-digest/tests/lengths_and_splits.rs::every_length_to_1024_is_the_recorded_answer	EASY given the two lemmas: composition of new/update_slice/finalize (callee ensures), the generic impl AsRef<[u8]> wrapper digest. The Clean model needs List UInt8 and UInt32 bit-vector arithmetic.	0,1,4,5,11,14
sha256-streaming-equals-one-shot	equivalence	Sha256::update_slice, Sha256::update, Sha256::finalize, Sha256::digest	∀ partitions [x1..xn] (n ≥ 0, any lengths, empty pieces included) with total length < 2^61: feed(Sha256::new(),[x1..xn]).finalize() = Sha256::digest(x1 ++ … ++ xn). More generally, ∀ h, M with ShaInv(h,M): feed(h,xs).finalize() = FIPS.SHA256(M ++ concat xs). FREE given sha256-state-invariant (update is a monoid action on the abstraction) and sha256-finalize-pads.	../../vendor/astream/crates/astream-digest/tests/lengths_and_streaming.rs::sha256_fed_in_arbitrary_chunks_matches_one_shot; ../../vendor/astream/crates/astream-digest/src/lib.rs::{padding_boundary_lengths_stream_identically,whole_blocks_from_the_input_digest_as_every_byte_through_the_buffer_does}	EASY given the invariant: induction over a sequence of &mut self calls, a ghost message, list append	0,1,2,4,5
opaque-debug	contract	<Sha256 as Debug>::fmt	∀ h : Sha256: format!("{h:?}") = "Sha256 { .. }", a constant independent of every field, so no absorbed message byte reaches the output. This is the general form of "does not contain \"secret\"". astream-digest's `opaque-debug-hmac` is the same spec for `HmacSha256`.	../../vendor/astream/crates/astream-digest/src/lib.rs::debug_does_not_leak_state	EASY-MEDIUM: a trait impl on &self, std::fmt::Formatter and debug_struct/finish_non_exhaustive (std calls that need models), String output from format!	0,1,2,3,4,11
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
../../vendor/astream/crates/astream-digest/src/lib.rs	fips_empty_message	sha256-digest-refines-fips	
../../vendor/astream/crates/astream-digest/src/lib.rs	fips_abc	sha256-digest-refines-fips	
../../vendor/astream/crates/astream-digest/src/lib.rs	fips_448_bit_message	sha256-digest-refines-fips	sha256-finalize-pads (56 bytes spill the length, §4.1)
../../vendor/astream/crates/astream-digest/src/lib.rs	fips_896_bit_message	sha256-digest-refines-fips	
../../vendor/astream/crates/astream-digest/src/lib.rs	fips_one_million_a	sha256-digest-refines-fips	
../../vendor/astream/crates/astream-digest/src/lib.rs	padding_boundary_lengths_stream_identically	sha256-streaming-equals-one-shot	sha256-finalize-pads
../../vendor/astream/crates/astream-digest/src/lib.rs	the_length_field_is_64_bits_wide	sha256-finalize-pads	sha256-compress-refines-fips (uses compress as its own oracle, §4.3)
../../vendor/astream/crates/astream-digest/src/lib.rs	the_ring_schedule_compression_gives_the_transcriptions_state_on_random_blocks	sha256-compress-refines-fips	
../../vendor/astream/crates/astream-digest/src/lib.rs	whole_blocks_from_the_input_digest_as_every_byte_through_the_buffer_does	sha256-streaming-equals-one-shot	sha256-state-invariant, sha256-digest-refines-fips
../../vendor/astream/crates/astream-digest/src/lib.rs	default_matches_new	sha256-state-invariant	
../../vendor/astream/crates/astream-digest/src/lib.rs	debug_does_not_leak_state	opaque-debug	
../../vendor/astream/crates/astream-digest/src/lib.rs	clone_forks_the_hash_state	sha256-state-invariant	sha256-digest-refines-fips
../../vendor/astream/crates/astream-digest/tests/lengths_and_streaming.rs	sha256_over_every_length_and_large_inputs_is_pinned	sha256-digest-refines-fips	sha256-finalize-pads
../../vendor/astream/crates/astream-digest/tests/lengths_and_streaming.rs	sha256_fed_in_arbitrary_chunks_matches_one_shot	sha256-streaming-equals-one-shot	
../../vendor/astream/crates/astream-digest/tests/lengths_and_splits.rs	every_length_to_1024_is_the_recorded_answer	sha256-digest-refines-fips	sha256-streaming-equals-one-shot
```

15 tests map to the 6 rows; `sha256-digest-refines-fips` retires the most (7). astream-digest's
HMAC, HKDF and `ct_eq` tests are mapped in its own file.

## 3. Tests that are not spec samples

- `the_digest_still_evaluates_at_compile_time` pins that `update_slice`, `compress` and
  `finalize` stay `const fn` (a const item calls them). Its values are instances of
  `sha256-digest-refines-fips`, but a proof does not keep a function `const`.
- `clean_origin_streaming_sha256_kernels_enforce_tla_invariants` samples the streaming buffer's
  Clean-island kernels (`buf_len_is_partial`, `sha256_partial_take`, `sha256_clamp_partial_len`,
  `sha256_next_buf_len`); no row here states them.

## 4. Where the specs and the code disagree, or could

1. **astream-digest's `fips_448_bit_message` has a wrong comment about 56 bytes.** It calls the
   56-byte message "the tightest single-block case" with "exactly 8 bytes free". After the 0x80
   byte `pos = 57 > 56`, so `finalize` spills the length into a second block. 55 bytes is the
   tightest single-block case, as `finalize`'s own comment says (messages of 56..=63 bytes mod 64
   spill).
2. **astream-digest's `fips_896_bit_message` miscounts the 112-byte message.** It says "two blocks
   plus a padding block". 112 bytes is 1 full block plus a 48-byte tail, and 48+1+8 ≤ 64, so the
   digest takes 2 compressions, not 3. The 896-bit string is the SHA-384/512 two-block example
   message.
3. **`the_length_field_is_64_bits_wide` samples finalize, not digest.** It sets the private field
   `h.len = 2^32` with `buf_len = 0` and calls the private `compress` as its own oracle. The state
   is consistent with `ShaInv` mod 64 but unreachable without 4 GiB of input. It is therefore a
   sample of `sha256-finalize-pads` over arbitrary `(state, tail, len)`, which is why that spec is
   stated for every `h` with `buf_len < 64`, not only reachable ones.
4. **The sweep oracles cannot be regenerated.** `SHA256_SWEEP` (`tests/lengths_and_streaming.rs`)
   and `SWEEP` and `BOUNDARIES` (`tests/lengths_and_splits.rs`) are recorded answers that matched
   the `sha2` crate when they were taken; nothing recomputes them. Under
   `sha256-digest-refines-fips` the oracle becomes the Clean FIPS model.
5. **An implicit precondition.** `update_slice`'s `len` uses `wrapping_add` and `finalize`'s
   `len * 8` uses `wrapping_mul`, so digests of messages ≥ 2^61 bytes silently diverge from FIPS.
   Every spec here carries |M| < 2^61, but no `requires` states it. Add one (as a ghost
   precondition on the abstraction) when the specs land.

## 5. Proof order (easiest, high value first)

No spec is blocked by gap 0 alone, so the order goes by fewest and smallest gaps, then by tests
retired and by what each spec unlocks. `sha256-compress-refines-fips` is the one leaf the other
`sha256-*` rows build on; `opaque-debug` is independent.

1. **`sha256-compress-refines-fips`** (0,1,2,3,7,14). Fixed-size arrays, fixed-bound loops, no
   heap and no data-dependent control flow: the best first target for gap 7 on bit-vector code.
   It retires 1 test, and every other `sha256-*` row, and astream-digest's HMAC rows, build on it.
2. **`sha256-finalize-pads`** (0,1,2,3,4,7). One branch and two `compress` calls (the first use
   of gap 4). It retires `the_length_field_is_64_bits_wide`.
3. **`sha256-state-invariant`** (0,1,2,3,4,5,7,11), then **`sha256-digest-refines-fips`** and
   **`sha256-streaming-equals-one-shot`** as near-free corollaries. Together they retire 12
   tests, including the 10^6-byte vector and the 100,000-byte sweep. `update_slice` and
   `digest_slice` take a plain `&[u8]`, so gap 11 reaches only the `AsRef` wrappers `update` and
   `digest`.
4. **`opaque-debug`** (0,1,2,3,4,11). It is small once `Formatter` is modelled. Writing the impl
   as `f.write_str("Sha256 { .. }")` drops the `debug_struct` model from the gap.
