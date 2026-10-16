<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-digest: spec recovery from its 28 unit tests

> Status 2026-10-15: this is the crate's migration worklist. The 12 specs below are each
> proved through Trust/Clean against the compiled Rust function, then the tests each one
> subsumes are deleted. The one test that samples no spec (§4) can be deleted now.

Source: `crates/aterm-digest` (read only). I read every `#[test]`
(src/lib.rs 5, src/sha256.rs 10, src/hmac_sha256.rs 8, tests/lengths_and_streaming.rs 5 = 28)
and the code each one reaches. Nothing was compiled or run. The claims below come from
reading the code.

Result: **12 specs**, covering **27 tests**. **1 test is not a spec sample** (§4).

Gap numbers refer to the table in
the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.
Gap 0 (a seal that publishes Clean-discharged rows) blocks every spec, so it is listed on
every row. **N1** is a gap that the audit table does not have yet: a relational
(2-safety / information-flow) lane, needed for the constant-time spec.

## 0. Notation used in the statements

- `FIPS.SHA256(m)`: a Clean model of FIPS 180-4 SHA-256 over `List UInt8`. It pads per §5.1.1,
  parses into 512-bit blocks, folds `FIPS.compress` from `H_INIT` per §6.2.2, and serialises the
  eight words big-endian. It is defined for |m| < 2^61 bytes (2^64 bits). The published
  FIPS/NIST vectors (empty, "abc", 448-bit, 896-bit, 10^6 'a') and RFC 4231 cases 1-7 become
  kernel-evaluated (`decide`/`rfl`) facts about the **model**. They validate the model against
  the standard. They are no longer tests of the Rust.
- `FIPS.compress(H, B)`: the §6.2.2 compression function on `[u32;8] × [u8;64]`.
- `Pad(st, tail, n)`: given chaining state `st`, a live tail with |tail| < 64, and a byte count
  `n : u64`, the result of absorbing `tail ++ [0x80] ++ 0^k ++ be64(n·8 mod 2^64)` with
  `FIPS.compress` (k minimal so the total is a multiple of 64: one block if |tail| ≤ 55, two if
  56..=63), then serialising big-endian.
- `HMAC.Kp(key)` = (|key| > 64 ? FIPS.SHA256(key) : key), zero-padded to 64 bytes.
  `HMAC(key, m)` = FIPS.SHA256((Kp ⊕ 0x5c^64) ++ FIPS.SHA256((Kp ⊕ 0x36^64) ++ m)) (RFC 2104).
- `ShaInv(h, M)`: the abstraction of a `Sha256` value `h` that has absorbed the ghost message `M`:
  `h.buf_len < 64` ∧ `h.buf_len = |M| mod 64` ∧ `h.len = |M| mod 2^64` ∧
  `h.buf[..buf_len]` = the last `|M| mod 64` bytes of M ∧
  `h.state` = foldl FIPS.compress H_INIT (the first ⌊|M|/64⌋ blocks of M).
  The bytes `h.buf[buf_len..]` are dead (unconstrained).
- `MacAbs(m)` = (key, M) for an `HmacSha256` value `m`:
  `m.outer_key = Kp(key) ⊕ 0x5c^64` ∧ `ShaInv(m.inner, (Kp(key) ⊕ 0x36^64) ++ M)`.
- `feed(h, [x1..xn])` = `h.update(x1); …; h.update(xn)` (any n ≥ 0, any slice lengths, empty
  pieces included).
- `Trace(f, args)`: the sequence of MIR basic-block/branch decisions an execution of `f` takes.
  Used only by the constant-time spec.
- `black_box`: `std::hint::black_box` is modelled as the identity on its value (and as an
  opaque call for `Trace`).
- **Precondition everywhere:** total absorbed length < 2^61 bytes (a legal FIPS message).
  `len` uses `wrapping_add` and `bit_len` uses `wrapping_mul`, so longer messages silently
  diverge from FIPS. The code documents this but states no `requires` (§5.8).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking_gaps (audit §7)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
ct-eq-is-slice-equality	contract	aterm_digest::ct_eq	∀ a b : &[u8]: ct_eq(a,b) = (a == b), i.e. true ⇔ |a| = |b| ∧ ∀ i<|a|, a[i] = b[i]. Loop invariant after i pairs of the zip fold: diff = OR_{j<i}(a[j] ⊕ b[j]), and diff = 0 ⇔ a[..i] = b[..i]. black_box = identity. The existing Clean lemma fold_or_xor_zero_iff_eq (crates/aterm-spec-models/proofs/clean/ct_eq.lean) is exactly the loop lemma. It must be re-bound to the compiled body (trust_import_aterm_digest__ct_eq) instead of the hand model.	lib.rs::ct_eq_accepts_identical_slices, ct_eq_rejects_same_length_differences, ct_eq_rejects_length_mismatch	EASY-MEDIUM: two slice params, early return on length, for loop over iter().zip(), u8 xor/or accumulator, a black_box call (needs an identity model), bool result. The loop lemma already exists.	0,1,3,5,7
ct-eq-data-independent-control-flow	other (2-safety / non-interference)	aterm_digest::ct_eq	∀ a b a' b' with |a| = |a'| ∧ |b| = |b'|: Trace(ct_eq,(a,b)) = Trace(ct_eq,(a',b')). Equivalently: the only input-dependent branch is a.len() != b.len(); the fold runs exactly |a| iterations with no data-dependent exit; the result is read from the accumulator only after the loop, through the opaque barrier. This is a MIR-level property. It says nothing about LLVM's machine code (black_box is the code's own guard for that).	lib.rs::ct_eq_stays_branch_free	HARD: relational (product-program) reasoning over a loop, slice iterators, black_box as an opaque call. No Trust lane exists for information-flow / constant-time properties.	0,1,5,7,N1
sha256-compress-refines-fips	refinement	sha256::compress	∀ state : [u32;8], block : [u8;64]: after compress(&mut state, &block), state' = FIPS.compress(state, block) (§6.2.2: W[0..16] = big-endian words of block; W[t] = σ1(W[t-2]) + W[t-7] + σ0(W[t-15]) + W[t-16] mod 2^32 for t ∈ 16..64; 64 rounds with K; add the working variables into state). Total and panic-free: every index t-2, t-7, t-15, t-16 with t ∈ 16..64 is in 0..64.	(lemma; secondary for sha256.rs::the_length_field_is_64_bits_wide)	MEDIUM: &mut [u32;8], fixed-bound for loops (16..64, zip over K and w), as_chunks::<4>, u32::from_be_bytes, rotate_right, shifts, wrapping_add, array destructuring. No heap. The Clean model needs UInt32 bit-vector arithmetic.	0,1,2,3,7,14
sha256-finalize-pads	contract	Sha256::finalize	∀ h with h.buf_len < 64 (any state, any len, reachable or not): h.finalize() = Pad(h.state, h.buf[..h.buf_len], h.len). So the length field is the full 64-bit big-endian encoding of len·8 mod 2^64 (bits above 31 included), a tail of 56..=63 bytes spills the length into a second block, and the write buf[buf_len] = 0x80 is in bounds. With ShaInv(h,M): h.finalize() = FIPS.SHA256(M).	sha256.rs::the_length_field_is_64_bits_wide	MEDIUM: self by value, array index, fill on subslices, u64::to_be_bytes, a branch on buf_len > 56, calls to compress (callee ensures), an as_chunks_mut::<4> zip loop writing to_be_bytes	0,1,2,3,4,7
sha256-state-invariant	invariant	Sha256::new, Sha256::default, Sha256::update, Sha256::clone	ShaInv(Sha256::new(), []) ∧ ShaInv(Sha256::default(), []) (Default delegates to new). ∀ h, M, x with ShaInv(h,M) ∧ |M|+|x| < 2^61: after h.update(x), ShaInv(h', M ++ x), and update does not panic (every slice range and the debug_assert hold because buf_len < 64 on entry; the early return when the topped-up block is still partial is what keeps buf[..buf_len] right). Clone preserves the abstraction: ShaInv(h,M) ⇒ ShaInv(h.clone(),M), and the two values evolve independently afterwards (no shared state).	sha256.rs::default_matches_new, clone_forks_the_hash_state	HARD: &mut self on a struct with [u32;8], [u8;64], usize and u64 fields; a generic impl AsRef<[u8]> param; copy_from_slice with computed ranges; as_chunks::<64> and a for loop over blocks; min; wrapping_add; debug_assert; derive(Clone)	0,1,2,3,4,5,7,11
sha256-digest-refines-fips	refinement	Sha256::digest (via new, update, finalize)	∀ m : &[u8] with |m| < 2^61: Sha256::digest(m) = FIPS.SHA256(m). A corollary of sha256-state-invariant and sha256-finalize-pads. The FIPS 180-4/NIST vectors and the length sweep (0..=200 and 255..100000 bytes) are instances. The vectors move to evaluated facts about FIPS.SHA256 (model validation).	sha256.rs::fips_empty_message, fips_abc, fips_448_bit_message, fips_896_bit_message, fips_one_million_a; tests/lengths_and_streaming.rs::sha256_over_every_length_and_large_inputs_is_pinned	EASY given the two lemmas: composition of new/update/finalize (callee ensures), a generic impl AsRef<[u8]>. The Clean model needs List UInt8 and UInt32 bit-vector arithmetic.	0,1,4,5,11,14
sha256-streaming-equals-one-shot	equivalence	Sha256::update, Sha256::finalize, Sha256::digest	∀ partitions [x1..xn] (n ≥ 0, any lengths, empty pieces included) with total length < 2^61: feed(Sha256::new(),[x1..xn]).finalize() = Sha256::digest(x1 ++ … ++ xn). More generally, ∀ h, M with ShaInv(h,M): feed(h,xs).finalize() = FIPS.SHA256(M ++ concat xs). FREE given sha256-state-invariant (update is a monoid action on the abstraction) and sha256-finalize-pads.	sha256.rs::padding_boundary_lengths_stream_identically; tests/lengths_and_streaming.rs::sha256_fed_in_arbitrary_chunks_matches_one_shot	EASY given the invariant: induction over a sequence of &mut self calls, a ghost message, list append	0,1,2,4,5
hmac-new-total	contract	HmacSha256::new_from_slice	∀ key : &[u8] (any length < 2^61): new_from_slice(key) = Ok(m) with MacAbs(m) = (key, []). It never returns Err(InvalidLength) and never panics (the two copy_from_slice ranges are [..32] for |key| > 64 and [..|key|] for |key| ≤ 64).	hmac_sha256.rs::every_key_length_is_accepted	MEDIUM: Result return, a branch on key.len() > 64, slice copies, a call to Sha256::digest (callee ensures), a triple zip loop over [u8;64] with xor, Sha256::update	0,1,3,4,7
hmac-refines-rfc2104	refinement	HmacSha256::new_from_slice, update, finalize	∀ key, m with |key|, |m| < 2^61 - 128: (let mac = new_from_slice(key).unwrap(); mac.update(m); mac.finalize()) = HMAC(key, m), where Kp hashes the key iff |key| > 64 (a 64-byte key is used verbatim). Corollaries: |key| > 64 ⇒ HMAC(key,m) = HMAC(FIPS.SHA256(key),m); |key| ≤ 64 ⇒ HMAC(key,m) = HMAC(key ++ 0^(64-|key|),m). RFC 4231 cases 1-7 and the key/message length sweeps are instances. The vectors become evaluated facts about the HMAC model.	hmac_sha256.rs::rfc4231_test_vectors, exactly_block_size_key_is_not_hashed; tests/lengths_and_streaming.rs::hmac_over_every_message_and_key_length_is_pinned	MEDIUM given the Sha256 specs: a struct holding a Sha256 and a [u8;64]; composition of the Sha256 specs across calls (callee ensures); self by value; xor loops	0,1,2,3,4,5,7
hmac-streaming-equals-one-shot	equivalence	HmacSha256::update, HmacSha256::finalize	∀ key, partitions [x1..xn]: feeding the pieces to new_from_slice(key).unwrap() and finalizing gives HMAC(key, x1 ++ … ++ xn). Proof: MacAbs(m) = (key,M) ⇒ MacAbs(m.update(x)) = (key, M ++ x) (update only forwards to inner; outer_key is unchanged), then hmac-refines-rfc2104.	hmac_sha256.rs::streaming_updates_match_one_shot; tests/lengths_and_streaming.rs::hmac_fed_in_arbitrary_chunks_matches_one_shot	EASY given the Sha256 invariant: &mut self delegation to Sha256::update, induction over call sequences	0,1,2,4,5
hmac-verify-slice-iff	contract	HmacSha256::verify_slice (via finalize, ct_eq)	∀ m with MacAbs(m) = (key,M), ∀ tag : &[u8]: m.verify_slice(tag) = Ok(()) ⇔ tag == HMAC(key,M) as slices, and Err(MacError) otherwise. So |tag| = 32 is required (a 16-byte prefix or a 33-byte extension is refused), and a tag differing from the true one in any bit is refused. The comparison inherits ct-eq-data-independent-control-flow. The wrong-key and wrong-message rejections in the tests also need the point facts HMAC("j","payload") ≠ HMAC("k","payload") and HMAC("k","payloaD") ≠ HMAC("k","payload"), evaluated on the model.	hmac_sha256.rs::verify_slice_accepts_the_right_tag, verify_slice_rejects_a_forged_tag, verify_slice_rejects_wrong_key_message_and_length; tests/lengths_and_streaming.rs::hmac_verify_slice_accepts_the_tag_and_refuses_a_flipped_bit	EASY given ct-eq-is-slice-equality and hmac-refines-rfc2104: self by value, calls to finalize and ct_eq (callee ensures), Result<(), MacError> with a unit-struct error, slice-vs-array comparison	0,1,2,4
opaque-debug	contract	<Sha256 as Debug>::fmt, <HmacSha256 as Debug>::fmt	∀ h : Sha256, m : HmacSha256: format!("{h:?}") = "Sha256 { .. }" and format!("{m:?}") = "HmacSha256 { .. }". Both are constants independent of every field, so no absorbed message byte and no key-derived byte reaches the output. This is the general form of "does not contain \"secret\"".	sha256.rs::debug_does_not_leak_state; hmac_sha256.rs::errors_render_without_leaking (Debug half)	EASY-MEDIUM: a trait impl on &self, std::fmt::Formatter and debug_struct/finish_non_exhaustive (std calls that need models), String output from format!. Writing the impl as f.write_str("Sha256 { .. }") would drop the debug_struct model.	0,1,2,3,4,11
```

### Distribution by kind (12)

| kind | count | ids |
|---|---|---|
| contract | 5 | ct-eq-is-slice-equality, sha256-finalize-pads, hmac-new-total, hmac-verify-slice-iff, opaque-debug |
| refinement | 3 | sha256-compress-refines-fips, sha256-digest-refines-fips, hmac-refines-rfc2104 |
| equivalence | 2 | sha256-streaming-equals-one-shot, hmac-streaming-equals-one-shot |
| invariant | 1 | sha256-state-invariant |
| other | 1 | ct-eq-data-independent-control-flow (2-safety) |

### Blocking-gap histogram (specs blocked by each gap)

| gap | specs | note |
|---|---|---|
| 0 | 12 | prerequisite for every row |
| 1 | 12 | slices, `[u8;64]`/`[u32;8]` arrays, structs, `List UInt8` models |
| 4 | 9 | callee-ensures composition (every spec above `compress`) |
| 2 | 8 | `&mut self` / `&self` views |
| 3 | 7 | calls, shifts, bitwise ops, `min` in reflected bodies |
| 5 | 7 | quantified clauses |
| 7 | 7 | `for` loops (zip folds, block loops, the 16..64 schedule) |
| 11 | 3 | `impl AsRef<[u8]>` generics, `Debug` trait impls |
| 14 | 2 | the FIPS model's bit-vector Lean idioms |
| N1 | 1 | relational / constant-time lane (not in the audit table) |

No spec is blocked by gap 0 alone.

### Proof dependency order

`ct-eq-is-slice-equality` and `sha256-compress-refines-fips` (independent leaves) →
`sha256-finalize-pads` → `sha256-state-invariant` → `sha256-digest-refines-fips` and
`sha256-streaming-equals-one-shot` (free corollaries) → `hmac-new-total` →
`hmac-refines-rfc2104` → `hmac-streaming-equals-one-shot` → `hmac-verify-slice-iff`
(also needs `ct-eq-is-slice-equality`). `opaque-debug` is independent.
`ct-eq-data-independent-control-flow` waits for gap N1.

## 2. Test → spec map (TSV; all 28)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/lib.rs	ct_eq_accepts_identical_slices	ct-eq-is-slice-equality	
src/lib.rs	ct_eq_rejects_same_length_differences	ct-eq-is-slice-equality	
src/lib.rs	ct_eq_rejects_length_mismatch	ct-eq-is-slice-equality	
src/lib.rs	ct_eq_stays_branch_free	ct-eq-data-independent-control-flow	(pins source text, §5.4)
src/lib.rs	hex_helper_round_trips	NON-SPEC (test-only code)	
src/sha256.rs	fips_empty_message	sha256-digest-refines-fips	(vector moves to a model fact)
src/sha256.rs	fips_abc	sha256-digest-refines-fips	(vector moves to a model fact)
src/sha256.rs	fips_448_bit_message	sha256-digest-refines-fips	sha256-finalize-pads (wrong comment, §5.1)
src/sha256.rs	fips_896_bit_message	sha256-digest-refines-fips	(mislabelled, §5.2)
src/sha256.rs	fips_one_million_a	sha256-digest-refines-fips	sha256-state-invariant
src/sha256.rs	padding_boundary_lengths_stream_identically	sha256-streaming-equals-one-shot	sha256-finalize-pads
src/sha256.rs	the_length_field_is_64_bits_wide	sha256-finalize-pads	sha256-compress-refines-fips (uses compress as its own oracle, §5.6)
src/sha256.rs	default_matches_new	sha256-state-invariant	
src/sha256.rs	clone_forks_the_hash_state	sha256-state-invariant	sha256-streaming-equals-one-shot
src/sha256.rs	debug_does_not_leak_state	opaque-debug	
src/hmac_sha256.rs	rfc4231_test_vectors	hmac-refines-rfc2104	(vectors move to model facts)
src/hmac_sha256.rs	exactly_block_size_key_is_not_hashed	hmac-refines-rfc2104	hmac-new-total
src/hmac_sha256.rs	every_key_length_is_accepted	hmac-new-total	
src/hmac_sha256.rs	streaming_updates_match_one_shot	hmac-streaming-equals-one-shot	
src/hmac_sha256.rs	verify_slice_accepts_the_right_tag	hmac-verify-slice-iff	
src/hmac_sha256.rs	verify_slice_rejects_a_forged_tag	hmac-verify-slice-iff	
src/hmac_sha256.rs	verify_slice_rejects_wrong_key_message_and_length	hmac-verify-slice-iff	hmac-refines-rfc2104 (two model point facts)
src/hmac_sha256.rs	errors_render_without_leaking	opaque-debug	(Display half pins prose, §5.5)
tests/lengths_and_streaming.rs	sha256_over_every_length_and_large_inputs_is_pinned	sha256-digest-refines-fips	sha256-finalize-pads
tests/lengths_and_streaming.rs	sha256_fed_in_arbitrary_chunks_matches_one_shot	sha256-streaming-equals-one-shot	
tests/lengths_and_streaming.rs	hmac_over_every_message_and_key_length_is_pinned	hmac-refines-rfc2104	hmac-new-total
tests/lengths_and_streaming.rs	hmac_fed_in_arbitrary_chunks_matches_one_shot	hmac-streaming-equals-one-shot	
tests/lengths_and_streaming.rs	hmac_verify_slice_accepts_the_tag_and_refuses_a_flipped_bit	hmac-verify-slice-iff	
```

## 3. Coverage

- 28 tests read. 27 map to one of the 12 specs. 1 is not a spec sample (§4).
- The biggest collapses: `sha256-digest-refines-fips` (6 primary), `hmac-verify-slice-iff` (4),
  `ct-eq-is-slice-equality` (3), `hmac-refines-rfc2104` (3).
- `sha256-compress-refines-fips` has no primary test, but every SHA-256 and HMAC spec is built on
  it. `sha256-state-invariant` has 2 primary tests, but it makes both streaming specs and the
  digest spec nearly free.

## 4. Tests that are not spec samples

> Status 2026-10-15: `lib.rs::hex_helper_round_trips` is deleted, with the `unhex` helper only it used. `hex` stays (the vector tests use it).

| test | why | recommendation |
|---|---|---|
| lib.rs::hex_helper_round_trips | It tests `hex`/`unhex`, which are `#[cfg(test)]` helpers with no production caller. Its third assertion (`unhex(hex(digest)) = digest`) is a round trip of test code, not of the crate. | **delete** now. Delete `hex`/`unhex` with the last test that uses them (the vector tests use them until their specs are Certified). |
| hmac_sha256.rs::errors_render_without_leaking (Display half only) | Its two `Display` assertions ("invalid key length", "MAC tag mismatch") pin prose. Only its `Debug` half samples a spec (`opaque-debug`). | **drop the two Display lines** now. The rest of the test goes with `opaque-debug`. |

No test is a pure regression of a removed bug with no general property. The sweep tests and the
FIPS/RFC vectors are all instances of the refinement specs; their recorded outputs become
facts about the Clean model.

## 5. Implied specs that look wrong, or disagree with the code

1. **`fips_448_bit_message` has a wrong comment.** It calls the 56-byte message "the tightest
   single-block case" with "exactly 8 bytes free". After the 0x80 byte `buf_len = 57 > 56`, so
   `finalize` spills the length into a second block. 55 bytes is the tightest single-block case,
   as `padding_boundary_lengths_stream_identically` itself says ("55 fits the length field;
   56..=63 spill").
2. **`fips_896_bit_message` is mislabelled.** It calls itself "FIPS 180-4 appendix B.3" and says
   "two blocks plus a padding block". B.3 is the 10^6 'a' message (`fips_one_million_a` also
   claims B.3), and the 896-bit string is the SHA-384/512 two-block example message. 112 bytes is
   1 full block plus a 48-byte tail, and 48+1+8 ≤ 64, so the digest takes 2 compressions, not 3.
3. **A stale claim in the sha256.rs module doc.** It (and the reason given for the test-local
   `hex`) says `aterm-verify` "compiles this file by path". No `#[path]` or include of
   `crates/aterm-digest/src/sha256.rs` exists anywhere under `crates/` (checked by grep). If it
   were true, the proofs would also have to cover a second compilation of the same body (gap 12).
   Remove the claim. Done 2026-10-15: both comments drop it, and the module doc names the
   bulk path, atpkg's `tree::BulkSha256`, instead.
4. **`ct_eq_stays_branch_free` pins source text.** It splits `lib.rs` on the `ct_eq` signature and
   on `for (x, y)`, then forbids `return` and `if ` up to the first `}`. Renaming the loop variables
   or reformatting breaks it, and a data-dependent branch written another way (`match`, `?`,
   `break`, a short-circuit `&&` in the fold) passes it. The property it samples is a relational
   constant-time spec (`ct-eq-data-independent-control-flow`). Trust has no lane for that (gap N1,
   not in audit §7). Keep the test until N1 lands.
5. **`errors_render_without_leaking` pins prose** in its Display half (§4). Drop those lines
   rather than turn them into a spec.
6. **`the_length_field_is_64_bits_wide` samples finalize, not digest.** It sets the private field
   `h.len = 2^32` with `buf_len = 0` and calls the private `compress` as its own oracle. The state is
   consistent with `ShaInv` mod 64 but unreachable without 4 GiB of input. It is therefore a sample
   of `sha256-finalize-pads` over arbitrary `(state, tail, len)`, which is why that spec is stated
   for every `h` with `buf_len < 64`, not only reachable ones.
7. **The sweep oracle cannot be regenerated.** The `tests/lengths_and_streaming.rs` sweeps compare
   against `SHA256_SWEEP`/`HMAC_SWEEP`, recorded once against the removed `sha2`/`hmac` crates.
   Under `sha256-digest-refines-fips` and `hmac-refines-rfc2104` the oracle becomes the Clean
   FIPS/RFC model.
8. **An implicit precondition.** `update`'s `len` uses `wrapping_add` and `finalize`'s `len*8` uses
   `wrapping_mul`, so digests of messages ≥ 2^61 bytes silently diverge from FIPS. Every spec here
   carries |M| < 2^61. The field docs justify the wrapping as "for a legal message", but no
   `requires` states it. Add one (as a ghost precondition on the abstraction) when the specs land.
9. **`unhex` is documented as "lowercase hex" but accepts `A`..=`F`.** Test-only code and
   harmless; it is deleted with the hex helpers (§4).

## 6. Proof order (easiest, high value first)

No spec is blocked only by gap 0, so the order goes by fewest and smallest gaps, then by tests
retired and by what each spec unlocks.

1. **`ct-eq-is-slice-equality`** (gaps 0,1,3,5,7). The loop lemma `fold_or_xor_zero_iff_eq`
   already exists in Clean, so the work is binding it to `trust_import_aterm_digest__ct_eq` (one
   zip loop over two slices, an early length return, a `black_box` identity model). It retires 3
   tests and is the comparison lemma for `hmac-verify-slice-iff`.
2. **`sha256-compress-refines-fips`** (0,1,2,3,7,14). Fixed-size arrays, fixed-bound loops, no
   heap and no data-dependent control flow: the best first target for gap 7 on bit-vector code. It
   retires no test alone, but every other SHA-256/HMAC spec is built on it.
3. **`sha256-finalize-pads`** (0,1,2,3,4,7). One branch and two `compress` calls (the first use of
   gap 4). It retires `the_length_field_is_64_bits_wide`.
4. **`sha256-state-invariant`** (0,1,2,3,4,5,7,11), then **`sha256-digest-refines-fips`** and
   **`sha256-streaming-equals-one-shot`** as near-free corollaries. Together they retire 10 tests,
   including the 10^6-byte vector and the 100,000-byte sweep. Gap 11 can be sidestepped by giving
   `update` and `digest` a monomorphic `&[u8]` inner fn and leaving the `AsRef` wrapper trivial.
5. **`hmac-new-total`**, **`hmac-refines-rfc2104`**, **`hmac-streaming-equals-one-shot`**, then
   **`hmac-verify-slice-iff`**. These are compositions of the Sha256 specs plus xor loops. They
   retire the remaining 9 HMAC tests.
6. **`opaque-debug`** (0,1,2,3,4,11). It is independent and small once `Formatter` is modelled.
   Rewriting both impls as `f.write_str(..)` removes the `debug_struct` model from the gap.
7. **`ct-eq-data-independent-control-flow`** last. It waits for the new relational lane N1. Its
   one test stays until then.
