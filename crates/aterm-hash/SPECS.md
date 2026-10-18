<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-hash: spec recovery from its 6 unit tests

> Status 2026-10-16: this is the crate's migration worklist. The 6 specs below are each to be
> proved through Trust/Clean about the compiled Rust functions, and then the tests each one subsumes
> are deleted. The one test in §4 samples no spec and can be deleted now. The crate's floor today is
> 50 of 86 obligation rows proved (`crates/aterm-release/trust/floors.tsv`).

Source: `crates/aterm-hash` (read only). I read every `#[test]` (`src/fnv.rs` 2, `src/lib.rs` 4 = 6)
and the code each one reaches. Nothing was compiled or run. The claims below come from reading the code.

Result: **6 specs**, covering **5 tests**. **1 test was not a spec sample** (§4); it is deleted.

## 0. Notation used in the statements

- All `u64` arithmetic is mod 2^64. `^` is XOR, `lo64(x) = x as u64` and `hi64(x) = (x >> 64) as u64`
  for a `u128` x.
- A byte slice `bs: &[u8]` is modelled as the Lean `List UInt8` `bs.toList`. `++` is list append.
- **FNV-1a/64.** `P = FNV64_PRIME = 0x100000001b3` (= 2^40 + 0x1b3), `O = FNV64_OFFSET = 0xcbf29ce484222325`.
  The reference fold is `fnvRef(h, bs) = List.foldl (fun h b => (h ^ b.toUInt64) * P) h bs`.
- **Fx (rustc-hash 2.1), 64-bit target.** `K = 0xf1357aea2e62a9c5`, `add(h, w) = (h + w) * K`,
  `fin(h) = rotl(h, 26)`. On the 32-bit arm (`cfg(target_pointer_width = "32")`) the same statements
  hold with `K32 = 0x93d765dd`, `ROT = 15` and every `u64` split into two `usize` words.
- `le64(bs, i) = Σ_{j<8} bs[i+j] · 2^(8j)` (that is, `u64::from_le_bytes(bs[i..i+8])`). `le32` is the
  same over 4 bytes.
- `mm(x, y) = lo64(x·y) ^ hi64(x·y)` over the full 128-bit product.
- `hbRef(bs)` is the rustc-hash 2.1 `hash_bytes` reference, written as a Lean function over
  `List UInt8`. It starts from `s0 = SEED1`, `s1 = SEED2`.
  - `len ≤ 16`, `len ≥ 8`: `s0 ^= le64(bs, 0)`, `s1 ^= le64(bs, len-8)`.
  - `len ≤ 16`, `4 ≤ len < 8`: the same with `le32`.
  - `0 < len < 4`: `s0 ^= bs[0]`, `s1 ^= (bs[len-1] << 8) | bs[len/2]`.
  - `len > 16`: one round per 16-byte block at each `off < len-16`, `off ≡ 0 (mod 16)`. A round
    computes `t = mm(s0 ^ le64(bs, off), PTZC ^ le64(bs, off+8))` and then sets `(s0, s1) := (s1, t)`.
    After the rounds, `s0 ^= le64(bs, len-16)` and `s1 ^= le64(bs, len-8)`.
  - The result is `mm(s0, s1) ^ len`.
- `hashOneFx(x)` = `fin` of the `FxHasher` state after std's `Hash::hash` for the primitive type of x,
  starting from state 0. This is what `FxBuildHasher.hash_one(x)` returns.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps (audit §7 numbers)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
HASH-FNV-1	refinement	fnv::fnv1a_64_extend, fnv::fnv1a_64, fnv::fnv_mix_u64	∀ h: u64, bs: &[u8]: fnv1a_64_extend(h, bs) = fnvRef(h, bs) and fnv1a_64(bs) = fnvRef(O, bs). ∀ h, w: u64: fnv_mix_u64(h, w) = (h ^ w) * P, so fnv_mix_u64(h, b as u64) = fnvRef(h, [b]). With the constant facts O = 0xcbf29ce484222325 and P = 2^40 + 0x1b3 (the published FNV-1a/64 parameters), this fixes the function to the published algorithm. The published vectors ("" → O, "a" → 0xaf63dc4c8601ec8c, "foobar" → 0x85944171f73967e8) become `decide`/`native_decide` corollaries and serve as the negative control: a wrong prime fails them. Loop invariant: 0 ≤ i ≤ bs.len() ∧ h = fnvRef(hash, take i bs); decreases bs.len() − i.	fnv::matches_the_published_vectors	EASY-MEDIUM: a const fn while loop over a slice index (invariant, decreases), &[u8] as a List model, u8→u64 cast, XOR, wrapping_mul	0,1,3,5,7,14
HASH-FNV-2	other (homomorphism)	fnv::fnv1a_64_extend, fnv::fnv1a_64	∀ h: u64, a, b: &[u8]: fnv1a_64_extend(fnv1a_64_extend(h, a), b) = fnv1a_64_extend(h, a ++ b); in particular fnv1a_64_extend(fnv1a_64(a), b) = fnv1a_64(a ++ b), and fnv1a_64_extend(h, []) = h. Follows from HASH-FNV-1 by List.foldl_append, stated as a Clean theorem over the kernel imports.	fnv::a_stream_of_writes_is_the_hash_of_their_concatenation (second assert)	TRIVIAL given HASH-FNV-1: two slice binders, List append in the model, composition of callee ensures	0,1,3,4,5,7
HASH-FNV-3	invariant	fnv::Fnv1a64::{new, default, write, write_u8, finish}	Type invariant with ghost history W: List UInt8 (every byte written): self.0 = fnvRef(O, W). new() and default() establish it with W = []. write(bs) takes W to W ++ bs (two-state: self'.0 = fnv1a_64_extend(self.0, bs)). write_u8(b) takes W to W ++ [b] (self'.0 = fnv_mix_u64(self.0, b as u64)). finish() returns self.0. Hence for any sequence of writes w1..wn (slices or single bytes), finish() = fnv1a_64(w1 ++ … ++ wn).	fnv::a_stream_of_writes_is_the_hash_of_their_concatenation	EASY given HASH-FNV-1/2: &mut self two-state postconditions, &self view, newtype struct, Default impl, ghost history	0,1,2,3,4,5
HASH-FX-1	refinement	FxHasher::{default, add_to_hash, write_u8, write_u16, write_u32, write_u64, write_usize, write_u128, finish}, FxBuildHasher::build_hasher	Bit-exact with rustc-hash 2.1 on integer writes. FxHasher::default().hash = 0 and FxBuildHasher.build_hasher() = FxHasher::default(). ∀ state h: add_to_hash(x) sets hash' = add(h, x); write_u8/u16/u32/usize(x) set hash' = add(h, x as usize); write_u64(x) sets hash' = add(h, x as usize); write_u128(x) sets hash' = add(add(h, lo64(x)), hi64(x)) (the body reads the words with read_u64_le over x.to_le_bytes() behind a `bs.len() >= 16` guard that always holds, so its else arm is dead); finish() = fin(hash). Hence for every unsigned n of type u8/u16/u32/u64/usize, hashOneFx(n) = fin(n·K); for every u128 n, hashOneFx(n) = fin((lo64(n)·K + hi64(n))·K); and for every signed n of width w, hashOneFx(n) = hashOneFx(n as u_w), because std's signed Hash impls forward through the unsigned write. 32-bit arm: the same with K32 and ROT = 15; write_u64 adds both 32-bit halves and write_u128 adds the four 32-bit little-endian words.	lib::test_fxhasher_unsigned_integers_match_rustc_hash, lib::test_fxhasher_signed_integers_match_rustc_hash	MEDIUM: &mut self two-state, Hasher/BuildHasher trait impls reached through std's generic Hash impls for primitives, truncating and sign casts, wrapping_add/mul, rotate_left, u128 to_le_bytes plus slice ranges, cfg(target_pointer_width) arms	0,1,2,3,4,5,11
HASH-FX-2	equivalence	read_u32_le, read_u64_le, multiply_mix	∀ b: &[u8]: read_u32_le(b) = le32(b, 0) if b.len() ≥ 4, else 0; read_u64_le(b) = le64(b, 0) if b.len() ≥ 8, else 0; both are panic-free on every input. ∀ x, y: u64: multiply_mix(x, y) = mm(x, y), with the product taken in u128 (64-bit arm). The 32-bit arm equals the rustc-hash 2.1 32-bit formula (lx·hy) ^ rotr32(hx·ly).	lib::test_fxhasher_bytes_match_rustc_hash (shared), lib::test_fxhasher_unsigned_integers_match_rustc_hash (u128 rows, shared)	EASY: slice indexing and ranges, u8→u32/u64 casts, constant shifts, OR, u128 multiply, early return. multiply_mix alone is straight-line scalar code	0,1,3,5
HASH-FX-3	refinement	hash_bytes, FxHasher::write	∀ bs: &[u8]: hash_bytes(bs) = hbRef(bs), and hash_bytes never panics (every range is in bounds: the loop runs only while off < len − 16, so off + 16 < len; the tail ranges need len > 16). FxHasher::write(bs) = write_u64(hash_bytes(bs)), so hash' = add(h, hash_bytes(bs) as usize) on 64-bit (32-bit adds both halves). Hence for a byte slice b that a Hash impl writes alone, hash_one returns fin(hash_bytes(b)·K). Loop invariant: off ≡ 0 (mod 16), off < len (equivalently off ≤ 16·⌈(len−16)/16⌉), and (s0, s1) equal the reference state after off/16 rounds; decreases len − off.	lib::test_fxhasher_bytes_match_rustc_hash	MEDIUM: while loop with stride 16 (invariant, decreases), slice ranges and slice patterns ([], [a], [a,b], [a,b,c], _), u64 XOR, len casts, composition with read_u64_le/read_u32_le/multiply_mix, &mut self	0,1,2,3,4,5,7
```

### Distribution by kind (6)

| kind | count | ids |
|---|---|---|
| refinement | 3 | HASH-FNV-1, HASH-FX-1, HASH-FX-3 |
| equivalence | 1 | HASH-FX-2 |
| invariant | 1 | HASH-FNV-3 |
| other | 1 | HASH-FNV-2 (homomorphism) |

### Gap histogram (audit §7)

| gap | specs blocked |
|---|---|
| 0 seal publishes Clean rows | 6 |
| 1 non-scalar carriers (slices as `List UInt8`) | 6 |
| 3 E6 body widening (casts, shifts, bitwise, calls, loops) | 6 |
| 5 quantified clauses | 6 |
| 4 callee-ensures composition | 4 |
| 2 `&self`/`&mut self` views, two-state | 3 |
| 7 loops | 3 |
| 11 generics (std's generic `Hash` impls) | 1 |
| 14 Clean elaborator (`native_decide` corollaries) | 1 |

No spec is blocked by gap 0 alone. The closest are two scalar clauses: `fnv_mix_u64` (in HASH-FNV-1)
and `multiply_mix` (in HASH-FX-2). Each needs gap 0 plus the XOR/cast/shift part of gap 3, and a
two-binder quantifier (gap 5).

### Proof dependency order

HASH-FX-2 (`multiply_mix`, then the read helpers) → HASH-FX-3. HASH-FX-1 depends on HASH-FX-2
only through its u128 path. HASH-FX-1 and HASH-FX-3 compose for `FxHasher::write`. On the FNV side,
HASH-FNV-1 → HASH-FNV-2 → HASH-FNV-3.

## 2. Test → spec map (TSV; the 5 spec samples)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/fnv.rs	matches_the_published_vectors	HASH-FNV-1	
src/fnv.rs	a_stream_of_writes_is_the_hash_of_their_concatenation	HASH-FNV-3	HASH-FNV-2, HASH-FNV-1 (the fnv_mix_u64 assert)
src/lib.rs	test_fxhasher_unsigned_integers_match_rustc_hash	HASH-FX-1	HASH-FX-2 (u128 path through read_u64_le)
src/lib.rs	test_fxhasher_signed_integers_match_rustc_hash	HASH-FX-1	
src/lib.rs	test_fxhasher_bytes_match_rustc_hash	HASH-FX-3	HASH-FX-2, HASH-FX-1 (write_u64, finish)
```

## 3. Coverage

- 6 tests read. 5 map to one of the 6 specs; the 1 that sampled no spec (§4) is deleted.
- Retiring `test_fxhasher_bytes_match_rustc_hash` needs HASH-FX-1, -2 and -3 all Certified.
  Retiring `a_stream_of_writes_is_the_hash_of_their_concatenation` needs all three FNV specs.
- Every test here is a short list of sampled vectors. The specs replace them with the reference
  algorithm over all inputs, and the published vectors survive as `decide` corollaries that act as
  negative controls.

## 4. Tests that are not spec samples

> Status 2026-10-16: `lib::test_fxhasher_default_via_buildhasherdefault` is deleted.

> Status 2026-10-17: none left. Its row here and in §2 is gone; its one real fact (both build
> hashers start in state 0) is a clause of HASH-FX-1.

## 5. Implied specs that look wrong, or disagree with the code

1. **The 32-bit halves have never been checked on this host.** In the unsigned, signed and bytes tests,
   every `if B32 { .. }` expected value is a 32-bit-target constant, and none of them runs on the
   64-bit host. So that half of the claimed rustc-hash compatibility is unchecked here. A few
   hand spot-checks agree with the model (32-bit `i32::MIN` and `i64::MIN` → 16384, `usize::MAX = u32::MAX`
   → 1293006356). The specs must be proved per `cfg` arm, or the 32-bit arm must be named as unverified.
2. **The production `str` path is not tested.** `test_fxhasher_bytes_match_rustc_hash` reaches
   `FxHasher::write` only through the test-only `HashBytes` wrapper. Production `str` keys go through
   std's `Hash for str`, which also calls `write_u8(0xff)`, and no test covers that path.
   HASH-FX-1 and HASH-FX-3 compose to cover it only once that std impl is modelled (gap 11).
3. **The short-input arms of `hash_bytes` are thinly sampled.** The comment on the `len < 4` arm
   gives `lo = bytes[0]`, `mid = bytes[len/2]`, `hi = bytes[len-1]`. The literal slice-pattern arms
   match that for lengths 1, 2 and 3. The only 2- or 3-byte test input is `b"uwu"`, so the real check
   is HASH-FX-3's case for each length.
4. **The loop invariant in the first draft was false.** It said "off ≤ len − 16 at the exit test".
   Because off steps by 16, it overshoots: with len = 21, off goes from 0 to 16 > 5. HASH-FX-3 states
   the corrected bound (`off ≡ 0 mod 16 ∧ off < len`), which is also what the in-bounds argument for
   `bytes[off..off+16]` uses (off < len − 16 at the read).

## 6. Proof order (easiest, high value first)

1. **The scalar cores: `fnv_mix_u64` (HASH-FNV-1) and `multiply_mix` (HASH-FX-2).** These are
   straight-line, panic-free `u64`/`u128` arithmetic and the closest to "gap 0 only". After the seal,
   each needs only XOR, casts and a shift admitted for E6 import (gap 3). They are `bv_decide`/`omega`-class.
2. **HASH-FNV-1 in full.** One const-fn loop with an obvious invariant. Its published vectors are a
   built-in negative control. It is the base for HASH-FNV-2 and -3.
3. **HASH-FNV-2.** Free once HASH-FNV-1 holds (`List.foldl_append`).
4. **HASH-FNV-3.** A newtype with a ghost history. Together with 2 and 3 it retires both FNV tests,
   and it is a small first case for gap 2 (two-state `&mut self`).
5. **HASH-FX-2 read helpers** (`read_u32_le`, `read_u64_le`). Slice indexing with a length guard.
6. **HASH-FX-3.** The stride-16 loop plus slice patterns, built on step 5. This is the hardest spec here.
7. **HASH-FX-1.** Needs std's generic `Hash` impls for primitives modelled (gap 11) before the
   `hash_one` corollaries are about the real call path. Prove the per-method two-state clauses first.
