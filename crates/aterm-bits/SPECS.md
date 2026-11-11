<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-bits: spec recovery from unit tests

The specs that the tests in `src/lib.rs`'s `mod tests` sample. Once a spec is proved through
Trust/Clean about the compiled function, the tests it subsumes are deleted.

## 0. Notation used in the statements

- `sA = size_of::<A>()`, `sB = size_of::<B>()`, `alB = align_of::<B>()`.
- `addr(s) = s.as_ptr() as usize`.
- `byte_len(src) = size_of_val(src) = sA · src.len()`, which cannot overflow: a slice spans at
  most `isize::MAX` bytes.
- `Bytes(s, n)`: the `n` initialized bytes reachable from `s`'s data pointer (the object
  representation). For a `Pod` type every such byte sequence is a valid value.
  `Bytes(&[v], size_of::<T>())` is `v`'s native-endian representation, for example
  `u32::to_ne_bytes(v)`. `Bytes(s)` means `Bytes(s, size_of_val(s))`.
- `Disp(e)`: the `Display` text of a `PodCastError` variant.

  | variant | `Disp` |
  |---|---|
  | `TargetAlignmentGreaterAndInputNotAligned` | `"target alignment is greater than the input slice's alignment"` |
  | `OutputSliceWouldHaveSlop` | `"input byte length is not an integer multiple of the output element size"` |
  | `SizeMismatch` | `"input slice is smaller than one output value"` |

- `Classify(A, B, src)`: the decision list that `try_cast_slice` implements, in this order:
  1. `sA = 0 ∨ sB = 0`: `sA = sB ⇒ Ok(empty)` (a fresh `&[]`, not at `addr(src)`), else `Err(SizeMismatch)`.
  2. `addr(src) mod alB ≠ 0 ⇒ Err(TargetAlignmentGreaterAndInputNotAligned)`.
  3. `byte_len(src) mod sB ≠ 0 ⇒ Err(OutputSliceWouldHaveSlop)`.
  4. Otherwise `Ok(out)` with `out.len() = byte_len(src) / sB`, `addr(out) = addr(src)`, and
     `Bytes(out) = Bytes(src)`. The `ptr_b as usize == 0` guard before it is unreachable.

  Alignment is decided before slop, so an empty non-ZST `src` whose address is under-aligned for
  `B` is an `Err`, not `Ok(empty)`.
- `blocking_gaps` numbers are rows of the gap table in
  `docs/TRUST-LANE.md` (Blocking gaps). Gap 0 (an installed seal that publishes
  Clean rows) blocks every row.

## 1. Specs (TSV)

`subsumes` lists the primary tests (secondary ones are in §2); `difficulty` names the Rust
features the proof must handle.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
try-cast-slice-classify	contract	try_cast_slice	∀ A: Pod, B: Pod, src: &[A]: try_cast_slice::<A,B>(src) = Classify(A,B,src). It never panics and never makes UB. The ZST mismatch is the only SizeMismatch. A misaligned addr(src) is TargetAlignmentGreaterAndInputNotAligned. A byte length that sB does not divide is OutputSliceWouldHaveSlop. Otherwise the result aliases src exactly (same address, length byte_len/sB, same bytes), with its lifetime tied to src. Corollary (empty case): src.len()=0 ∧ sA,sB>0 ∧ addr(src) mod alB = 0 ⇒ Ok(out) with out.len()=0.	try_cast_slice_slop_returns_err, empty_slice_cast	MEDIUM-HARD: generic A,B over unsafe traits (size_of/align_of/size_of_val as symbolic per-instance constants), slices, pointer-to-int cast, is_multiple_of and division, ptr cast, unsafe slice::from_raw_parts (needs a memory/provenance model to state "same bytes"), Result of a borrowed slice	0, 1, 3, 5, 8, 11
cast-slice-refines-try	refinement	cast_slice, try_cast_slice, panic_cast_failed!	∀ A: Pod, B: Pod, src: &[A]: try_cast_slice::<A,B>(src) = Ok(out) ⇒ cast_slice::<A,B>(src) returns out. try_cast_slice::<A,B>(src) = Err(e) ⇒ cast_slice panics with payload exactly "cast_slice failed: " ++ Disp(e) (= format!("cast_slice failed: {e}")). With try-cast-slice-classify, this fixes cast_slice's panic set to the three Classify error branches.	exact_panic_message_matches_display	EASY-MEDIUM: call composition (callee ensures → caller), a match on an enum, panic! with a static concat! literal (needs a panic-payload model in the spec language; no §7 row names it), String equality	0, 1, 3, 4, 11
pod-cast-error-display-table	table-property	<PodCastError as Display>::fmt	∀ e: PodCastError: e.to_string() = Disp(e) (the 3-row table in §0), and fmt calls f.write_str exactly once with that literal. This is the Display half of cast-slice-refines-try; the panic_cast_failed! literals then equal "cast_slice failed: " ++ Disp(e) by construction.	(secondary only: exact_panic_message_matches_display)	EASY: enum match to &str literals; MEDIUM for the Formatter/write_str machinery and ToString	0, 1, 3, 4
cast-slice-bytes-round-trip	round-trip	cast_slice, try_cast_slice	∀ A: Pod with sA > 0, src: &[A]: cast_slice::<A,u8>(src) does not panic and returns bytes with bytes.len() = sA·src.len() and Bytes(bytes) = Bytes(src); and try_cast_slice::<u8,A>(bytes) = Ok(back) with back = src elementwise (same address, same length, same bit patterns; compare floats by bits, so NaN payloads round-trip). For a primitive A, A::from_ne_bytes of each sA-byte chunk of bytes is the matching element. Derived from try-cast-slice-classify instantiated twice (u8 has size and align 1; addr(src) is aligned for A; byte_len is a multiple of sA).	cast_slice_u32_to_u8_native_endian, cast_slice_roundtrip_f32_u8	FREE given try-cast-slice-classify and cast-slice-refines-try; otherwise composition of two generic calls, slice equality quantified over elements, float bit equality, from_raw_parts aliasing	0, 1, 4, 5, 8, 11
bytes-of-view	contract	bytes_of	∀ T: Pod, v: &T: bytes_of(v).len() = size_of::<T>(), bytes_of(v).as_ptr() = v as *const T as *const u8 (it aliases v; the is_null branch is unreachable), and Bytes(bytes_of(v)) is v's object representation. For every primitive integer/float T: T::from_ne_bytes(bytes_of(&x)) = x (bitwise for floats). Never panics.	bytes_of_single_value	EASY-MEDIUM: generic T over an unsafe trait, ptr::from_ref + cast + is_null, unsafe slice::from_raw_parts, slice result; from_ne_bytes/try_into for the corollary	0, 1, 3, 8, 11
zeroed-is-all-zero-bits	contract	Zeroable::zeroed	∀ T: Zeroable: every byte of the object representation of T::zeroed() is 0. Hence T::zeroed() = 0 for each integer primitive, +0.0 (bits 0) for f32/f64, [T::zeroed(); N] for [T; N], and each field's zeroed() for a #[repr(C)] struct of Zeroable fields. For T: Pod: bytes_of(&T::zeroed()) = [0u8; size_of::<T>()].	zeroed_returns_all_zero_bits	EASY once core::mem::zeroed has an intrinsic model: an unsafe trait default method, generics over every impl (primitives via macro, arrays with const generic N, unit), float bit patterns	0, 1, 8, 11
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/lib.rs	cast_slice_u32_to_u8_native_endian	cast-slice-bytes-round-trip	try-cast-slice-classify, cast-slice-refines-try
src/lib.rs	cast_slice_roundtrip_f32_u8	cast-slice-bytes-round-trip	try-cast-slice-classify, cast-slice-refines-try
src/lib.rs	try_cast_slice_slop_returns_err	try-cast-slice-classify	cast-slice-bytes-round-trip
src/lib.rs	bytes_of_single_value	bytes-of-view	
src/lib.rs	exact_panic_message_matches_display	cast-slice-refines-try	pod-cast-error-display-table, try-cast-slice-classify
src/lib.rs	empty_slice_cast	try-cast-slice-classify	cast-slice-refines-try, cast-slice-bytes-round-trip
src/lib.rs	zeroed_returns_all_zero_bits	zeroed-is-all-zero-bits	bytes-of-view
```

## 4. Tests that are not spec samples

None.

## 5. Implied specs that look wrong, or disagree with the code

1. *(Partially resolved)* **`SizeMismatch`'s doc names the ZST mismatch**; its `Display` text
   (`"input slice is smaller than one output value"`) is kept unchanged while `panic_cast_failed!`
   and `exact_panic_message_matches_display` pin it.
2. *(Resolved)* **`cast_slice`'s `# Panics` doc lists all three error conditions in check order**
   (ZST mismatch, alignment, slop).
3. *(Resolved)* **`empty_slice_cast` and `try_cast_slice_slop_returns_err` explicitly test alignment ordering.**
   `empty_slice_cast` verifies both aligned and unaligned empty slices, and
   `try_cast_slice_slop_returns_err` asserts `u32` alignment before testing slop, matching `Classify`.
4. **The ZST `Ok` branch does not alias `src`.** It returns a fresh `&[]`. That is sound and
   unobservable through the slice API, so `addr(out) = addr(src)` holds only on branch 4.
5. **`bytes_of` keeps `ptr.is_null()` because it is `pub const fn`.** `try_cast_slice` avoids
   `is_null()` via `ptr as usize == 0`, whereas rustc const-eval rejects pointer-to-integer casts
   inside `pub const fn bytes_of`. Lowering `bytes-of-view` in Trust therefore needs a verifier
   model for `ptr::is_null` (or a const-compatible lowering) rather than `ptr as usize == 0`.
6. **Cleanup on retirement:** delete the `panic_message` helper (`catch_unwind` + downcast)
   together with `exact_panic_message_matches_display` once `cast-slice-refines-try` is proved.
