<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-hash: spec recovery from its unit tests

Each row of §1 is a spec to prove through Trust/Clean about the compiled Rust functions of
`crates/aterm-hash` (`src/fnv.rs`, `src/lib.rs`); the tests it subsumes are then deleted. The
FNV-1a/64 loop and word step are astream-wire's `fnv1a_64_update` and `fnv1a_64_mix_u64`, so
HASH-FNV-1 and HASH-FNV-2 wait on their refinement `#[ensures]`: the loop's is open on the toolchain
side and the step's is astream's to land (`vendor/astream/docs/ROADMAP.md`, "Functional contracts
aterm cites").

## 0. Notation

- `u64` arithmetic is mod 2^64 and `^` is XOR. For a `u128` x, `lo64(x) = x as u64` and
  `hi64(x) = (x >> 64) as u64`.
- A slice `bs: &[u8]` is the Lean `List UInt8` `bs.toList`; `++` is list append.
- **FNV-1a/64.** `P = FNV64_PRIME = 0x100000001b3` (2^40 + 0x1b3), `O = FNV64_OFFSET =
  0xcbf29ce484222325`, and `fnvRef(h, bs) = List.foldl (fun h b => (h ^ b.toUInt64) * P) h bs`.
- **Fx (rustc-hash 2.1), 64-bit target.** `K = 0xf1357aea2e62a9c5`, `add(h, w) = (h + w) * K`,
  `fin(h) = rotl(h, 26)`. The 32-bit arm (`cfg(target_pointer_width = "32")`) uses
  `K32 = 0x93d765dd`, rotation 15, and two `usize` words per `u64`.
- `le64(bs, i)` = `u64::from_le_bytes(bs[i..i+8])`; `le32` is the same over 4 bytes.
- `mm(x, y) = lo64(x·y) ^ hi64(x·y)` over the full 128-bit product.
- `hbRef(bs)` is rustc-hash 2.1's `hash_bytes` as a Lean function over `List UInt8`, from
  `s0 = SEED1`, `s1 = SEED2` (`PTZC` is `PREVENT_TRIVIAL_ZERO_COLLAPSE`):
  - `8 ≤ len ≤ 16`: `s0 ^= le64(bs, 0)`, `s1 ^= le64(bs, len-8)`.
  - `4 ≤ len < 8`: the same with `le32`.
  - `0 < len < 4`: `s0 ^= bs[0]`, `s1 ^= (bs[len-1] << 8) | bs[len/2]`.
  - `len > 16`: for each `off < len-16` with `off ≡ 0 (mod 16)`,
    `t = mm(s0 ^ le64(bs, off), PTZC ^ le64(bs, off+8))` and `(s0, s1) := (s1, t)`; then
    `s0 ^= le64(bs, len-16)` and `s1 ^= le64(bs, len-8)`.
  - The result is `mm(s0, s1) ^ len`.
- `hashOneFx(x)` = `FxBuildHasher.hash_one(x)`: `fin` of the `FxHasher` state after std's
  `Hash::hash` for x's primitive type, from state 0.

## 1. Specs (TSV)

`subsumes` names the primary tests (the rest are in §2), `difficulty` the Rust features the
proof must handle, and `blocking gaps` rows of the gap table in
`docs/TRUST-LANE.md` (Blocking gaps).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
HASH-FNV-1	refinement	fnv::fnv1a_64_extend, fnv::fnv1a_64, fnv::fnv_mix_u64	∀ h: u64, bs: &[u8]: fnv1a_64_extend(h, bs) = fnvRef(h, bs) and fnv1a_64(bs) = fnvRef(O, bs). ∀ h, w: u64: fnv_mix_u64(h, w) = (h ^ w) * P, so fnv_mix_u64(h, b as u64) = fnvRef(h, [b]). With the constant facts O = 0xcbf29ce484222325 and P = 2^40 + 0x1b3 (the published FNV-1a/64 parameters), this fixes the function to the published algorithm. The published vectors ("" → O, "a" → 0xaf63dc4c8601ec8c, "foobar" → 0x85944171f73967e8) become `decide`/`native_decide` corollaries and serve as the negative control: a wrong prime fails them (through fnv1a_64 and through fnv_mix_u64 alike: P is astream-wire's exported FNV1A_64_PRIME, the prime its byte loop multiplies by, and fnv_mix_u64 is one call to astream_wire::hash::fnv1a_64_mix_u64, which the_word_fold_agrees_with_astreams_byte_loop samples against the formula written out). fnv1a_64_extend is one call to astream_wire::hash::fnv1a_64_update and O is its FNV1A_64_INIT, so the first clause is a cross-crate citation of a refinement ensures on fnv1a_64_update (= fnvRef(h, bs), with P its FNV1A_64_PRIME) that does not exist yet: astream-wire proves the fold in Clean (fnv1a_64_fold_append, fnv1a_64_update_init_eq_model), but a direct #[ensures] on a slice function is still open on the toolchain side. The contract on fnv1a_64_update is astream's to land first, with this loop invariant for its `while let` over a slice pattern (h₀ the entry state): rest is a suffix of bs ∧ h = fnvRef(h₀, bs[..bs.len() − rest.len()]); decreases rest.len(). The word-step clause is a citation of the same kind: fnv1a_64_mix_u64, straight-line with no loop, carries no ensures yet, and that one is astream's to land too.	fnv::matches_the_published_vectors, fnv::the_word_fold_agrees_with_astreams_byte_loop	EASY-MEDIUM here (cross-crate const fn calls and their citation, &[u8] as a List model); the loop and the step's XOR and wrapping_mul are astream-wire's	0,1,3,5,12,14 + the astream-wire contract (whose loop needs gap 7 on that side)
HASH-FNV-2	other (homomorphism)	fnv::fnv1a_64_extend, fnv::fnv1a_64	∀ h: u64, a, b: &[u8]: fnv1a_64_extend(fnv1a_64_extend(h, a), b) = fnv1a_64_extend(h, a ++ b); in particular fnv1a_64_extend(fnv1a_64(a), b) = fnv1a_64(a ++ b), and fnv1a_64_extend(h, []) = h. Follows from HASH-FNV-1 by List.foldl_append, stated as a Clean theorem over the kernel imports.	fnv::a_stream_of_writes_is_the_hash_of_their_concatenation (second assert)	TRIVIAL given HASH-FNV-1: two slice binders, List append in the model, composition of callee ensures	0,1,3,4,5,12 + the astream-wire contract (through HASH-FNV-1)
HASH-FNV-3	invariant	fnv::Fnv1a64::{new, default, write, write_u8, finish}	Type invariant with ghost history W: List UInt8 (every byte written): self.0 = fnvRef(O, W). new() and default() establish it with W = []. write(bs) takes W to W ++ bs (two-state: self'.0 = fnv1a_64_extend(self.0, bs)). write_u8(b) takes W to W ++ [b] (self'.0 = fnv_mix_u64(self.0, b as u64)). finish() returns self.0. Hence for any sequence of writes w1..wn (slices or single bytes), finish() = fnv1a_64(w1 ++ … ++ wn).	fnv::a_stream_of_writes_is_the_hash_of_their_concatenation	EASY given HASH-FNV-1/2: &mut self two-state postconditions, &self view, newtype struct, Default impl, ghost history	0,1,2,3,4,5
HASH-FX-1	refinement	FxHasher::{default, add_to_hash, write_u8, write_u16, write_u32, write_u64, write_usize, write_u128, finish}, FxBuildHasher::build_hasher	Bit-exact with rustc-hash 2.1 on integer writes. FxHasher::default().hash = 0 and FxBuildHasher.build_hasher() = FxHasher::default(). ∀ state h: add_to_hash(x) sets hash' = add(h, x); write_u8/u16/u32/usize(x) set hash' = add(h, x as usize); write_u64(x) sets hash' = add(h, x as usize); write_u128(x) sets hash' = add(add(h, lo64(x)), hi64(x)) (the body reads the words with read_u64_le over x.to_le_bytes() behind a `bs.len() >= 16` guard that always holds, so its else arm is dead); finish() = fin(hash). Hence for every unsigned n of type u8/u16/u32/u64/usize, hashOneFx(n) = fin(n·K); for every u128 n, hashOneFx(n) = fin((lo64(n)·K + hi64(n))·K); and for every signed n of width w, hashOneFx(n) = hashOneFx(n as u_w), because std's signed Hash impls forward through the unsigned write. 32-bit arm: the same with K32 and ROT = 15; write_u64 adds both 32-bit halves and write_u128 adds the four 32-bit little-endian words.	lib::test_fxhasher_unsigned_integers_match_rustc_hash, lib::test_fxhasher_signed_integers_match_rustc_hash	MEDIUM: &mut self two-state, Hasher/BuildHasher trait impls reached through std's generic Hash impls for primitives, truncating and sign casts, wrapping_add/mul, rotate_left, u128 to_le_bytes plus slice ranges, cfg(target_pointer_width) arms	0,1,2,3,4,5,11
HASH-FX-2	equivalence	read_u32_le, read_u64_le, multiply_mix	∀ b: &[u8]: read_u32_le(b) = le32(b, 0) if b.len() ≥ 4, else 0; read_u64_le(b) = le64(b, 0) if b.len() ≥ 8, else 0; both are panic-free on every input. ∀ x, y: u64: multiply_mix(x, y) = mm(x, y), with the product taken in u128 (64-bit arm). The 32-bit arm equals the rustc-hash 2.1 32-bit formula (lx·hy) ^ rotr32(hx·ly).	lib::test_fxhasher_bytes_match_rustc_hash (shared), lib::test_fxhasher_unsigned_integers_match_rustc_hash (u128 rows, shared)	EASY: slice indexing and ranges, u8→u32/u64 casts, constant shifts, OR, u128 multiply, early return. multiply_mix alone is straight-line scalar code	0,1,3,5
HASH-FX-3	refinement	hash_bytes, FxHasher::write	∀ bs: &[u8]: hash_bytes(bs) = hbRef(bs), and hash_bytes never panics (every range is in bounds: the loop runs only while off < len − 16, so off + 16 < len; the tail ranges need len > 16). FxHasher::write(bs) = write_u64(hash_bytes(bs)), so hash' = add(h, hash_bytes(bs) as usize) on 64-bit (32-bit adds both halves). Hence for a byte slice b that a Hash impl writes alone, hash_one returns fin(hash_bytes(b)·K). Loop invariant: off ≡ 0 (mod 16), off < len (equivalently off ≤ 16·⌈(len−16)/16⌉), and (s0, s1) equal the reference state after off/16 rounds; decreases len − off.	lib::test_fxhasher_bytes_match_rustc_hash	MEDIUM: while loop with stride 16 (invariant, decreases), slice ranges and slice patterns ([], [a], [a,b], [a,b,c], _), u64 XOR, len casts, composition with read_u64_le/read_u32_le/multiply_mix, &mut self	0,1,2,3,4,5,7
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test; `also` lists the other specs it samples. A test
goes once its primary and every `also` spec are proved.

```tsv
file	test	primary	also
src/fnv.rs	matches_the_published_vectors	HASH-FNV-1	
src/fnv.rs	the_word_fold_agrees_with_astreams_byte_loop	HASH-FNV-1	
src/fnv.rs	a_stream_of_writes_is_the_hash_of_their_concatenation	HASH-FNV-3	HASH-FNV-2, HASH-FNV-1 (the fnv_mix_u64 assert)
src/lib.rs	test_fxhasher_unsigned_integers_match_rustc_hash	HASH-FX-1	HASH-FX-2 (u128 path through read_u64_le)
src/lib.rs	test_fxhasher_signed_integers_match_rustc_hash	HASH-FX-1	
src/lib.rs	test_fxhasher_bytes_match_rustc_hash	HASH-FX-3	HASH-FX-2, HASH-FX-1 (write_u64, finish)
src/lib.rs	test_fxhasher_str_includes_terminator_byte	HASH-FX-3	HASH-FX-1 (write_u8(0xff), finish)
```

## 4. Tests that are not spec samples

None.

## 5. Implied specs that look wrong, or disagree with the code

1. **Both 32-bit and 64-bit FxHash arms checked on 64-bit hosts (resolved 2026-10-28).** `src/fx32.rs`
   shares the 32-bit `rustc-hash` 2.1 primitives (`add_to_hash_32`, `write_u64_32`, `write_u128_32`,
   `finish_32`, `multiply_mix_32`) between `cfg(target_pointer_width = "32")` and `cfg(test)`, and
   `test_hash!` verifies every unsigned, signed, and byte-slice vector against both native `FxBuildHasher`
   and 32-bit `FxBuildHasher32` on 64-bit hosts without changing the 64-bit library build.
2. **Production `str` path sampled (resolved 2026-10-27).**
   `test_fxhasher_str_includes_terminator_byte` exercises `FxBuildHasher.hash_one` on `&str` (std's
   `Hash for str`, which appends `write_u8(0xff)`). HASH-FX-1 and HASH-FX-3 compose to cover it in
   proofs once that std impl is modelled (gap 11).
3. **Short-input arms of `hash_bytes` sampled (resolved 2026-10-27).**
   `test_fxhasher_bytes_match_rustc_hash` now exercises 1-, 2-, and 3-byte inputs against
   rustc-hash's `lo = bs[0]`, `mid = bs[len / 2]`, `hi = bs[len - 1]` formula directly.
4. **The obvious loop bound is false.** "off ≤ len − 16 at the exit test" fails because off steps
   by 16 and overshoots: with len = 21, off goes from 0 to 16 > 5. HASH-FX-3 states the bound that
   holds (`off ≡ 0 mod 16 ∧ off < len`), which is also what the in-bounds argument for
   `bytes[off..off+16]` uses (off < len − 16 at the read).
