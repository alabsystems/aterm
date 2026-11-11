<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-provenance: spec recovery from its unit tests

Each row states a property its tests sample, so that a Trust/Clean proof against the compiled Rust
function can retire them; none is proved yet. The code is `src/provenance.rs` (`Provenance`,
`from_pty`, `as_ref`) and `src/pty_wrap.rs` (`pty_wrap_ref`, the crate's one `unsafe` block).

## 0. Notation used in the statements

- `Prov<T>` = `Provenance<T, Pty>`: `#[repr(transparent)]` over `T`; its other field,
  `_origin: PhantomData<fn() -> Pty>`, is a 1-aligned ZST. `Provenance<T, O>` is the same for any `O: Origin`.
- `wrap` = `pty_wrap_ref`, `inner(p)` = `Provenance::as_ref(p)` (returns `&p.value`),
  `mk` = `Provenance::from_pty`.
- "`r1` is `r2`" means the same fat pointer: equal address and pointer metadata (`core::ptr::eq`).
- Every spec is generic over `T`, unsized `T` included, and needs a per-definition proof (gap 11).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty` (Rust features the proof must
handle), `blocking gaps` (rows of `docs/TRUST-LANE.md` (Blocking gaps); gap 0,
the installed seal, blocks every row).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
layout-transparent	invariant	provenance::Provenance	∀ Sized T, ∀ O: Origin: size_of::<Provenance<T,O>>() = size_of::<T>() ∧ align_of::<Provenance<T,O>>() = align_of::<T>(). ∀ T: ?Sized, ∀ p: &Provenance<T,O>: size_of_val(p) = size_of_val(inner(p)) and the pointer metadata of p and inner(p) are equal. Under #[repr(transparent)] this is the language's layout guarantee, so the obligation reduces to: the attribute is present and _origin is a 1-ZST. It is the precondition pty-wrap-ref-identity's unsafe discharge cites. Stateable today as a const assertion over representative T (sized and unsized); the general form needs layout facts in Trust.	src/pty_wrap.rs::provenance_adds_no_bytes	EASY as a const assertion; for the general form, type-layout facts (size_of, align_of, size_of_val) under repr(transparent), generic T including unsized T, a PhantomData ZST field. No runtime code.	0, 1, 8, 11
from-pty-as-ref-roundtrip	round-trip	provenance::Provenance::from_pty, provenance::Provenance::as_ref	∀ T, ∀ v: T, with p = mk(v): *inner(&p) is the moved v, unchanged (no Clone call, no transformation), so *inner(&p) = v whenever T: PartialEq. Structural form (preferred, since a generic T has no PartialEq): mk moves v into p.value, and ∀ T: ?Sized, ∀ p: &Prov<T>: inner(p) = &p.value, a pure projection with p's borrow lifetime. The test's String equality is one instance.	-	EASY: generic struct with a PhantomData field, a const fn constructor, a &self accessor returning a reference into self (T: ?Sized); String (a heap-owning carrier) and its PartialEq<str> only in the sample	0, 1, 2, 4, 11
pty-wrap-ref-identity	contract	pty_wrap::pty_wrap_ref, provenance::Provenance::as_ref	∀ T: ?Sized, ∀ r: &'a T: wrap(r) has no UB and returns a &'a Prov<T> with inner(wrap(r)) is r (core::ptr::from_ref(inner(wrap(r))) = core::ptr::from_ref(r) as a fat-pointer comparison). Corollaries: for [U] and str, as_ptr and len are unchanged and *inner(wrap(r)) = *r (multi-byte UTF-8 included); for Sized T, *inner(wrap(r)) = *r. The call reads and writes no memory. Carries the soundness obligation of the crate's one unsafe block: &*(from_ref(r) as *const Prov<T>) is valid because layout-transparent makes the target's size, alignment and validity invariant equal T's.	src/pty_wrap.rs::wrap_scalar_reference_preserves_identity, src/pty_wrap.rs::wrap_str_preserves_bytes	MEDIUM: raw-pointer cast plus unsafe deref inside a const fn; fat pointers over ?Sized generics ([u8], str); references and lifetimes; composition with the as_ref field projection; a per-definition generic proof. No loops, no heap.	0, 1, 2, 4, 8, 11
pty-slice-inspectors	contract	provenance::{pty_slice_byte_at, pty_slice_u16_le_at, pty_slice_u16_be_at, pty_slice_u32_le_at, pty_slice_u32_be_at, pty_slice_u64_le_at, pty_slice_u64_be_at, pty_slice_has_csi_prefix, pty_slice_has_osc_prefix, pty_slice_has_dcs_prefix}, Provenance<u8,Pty>::{is_c0_or_del, is_printable_ascii, is_csi_param_byte, is_csi_final_byte, to_ascii_digit}, Provenance<u16,Pty>::{clamp_param, push_csi_digit_capped, to_le_bytes_pty, from_le_bytes_pty}, Provenance<(u16,u16,u16,u16),Pty>::{encode_csi_rect_le, decode_csi_rect_le}	Every slice reader returns None iff offset + width > bytes.len() (checked without usize overflow) and otherwise wraps the exact LE/BE scalar in Provenance<_, Pty>; the prefix predicates check ESC followed by '[', ']' or 'P' on slices of length ≥ 2; byte classifiers and CSI parameter combinators preserve Pty provenance, clamp/saturate without overflow, and round-trip u16 and 4×u16 LE wire frames.	src/provenance.rs::pty_slice_inspectors_preserve_provenance_and_bounds	EASY: const fn checked_add/saturating arithmetic, fixed-array LE packing, scalar range predicates	0, 1, 3
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/provenance.rs	pty_slice_inspectors_preserve_provenance_and_bounds	pty-slice-inspectors	from-pty-as-ref-roundtrip, pty-wrap-ref-identity
src/pty_wrap.rs	provenance_adds_no_bytes	layout-transparent	
src/pty_wrap.rs	wrap_scalar_reference_preserves_identity	pty-wrap-ref-identity	from-pty-as-ref-roundtrip (as_ref projection)
src/pty_wrap.rs	wrap_str_preserves_bytes	pty-wrap-ref-identity	from-pty-as-ref-roundtrip (as_ref projection), layout-transparent (unsized str)
```

## 4. Tests that are not spec samples

None: every test maps to a spec in §2, and the sized/fat-reference layout equalities in `src/pty_wrap.rs` are additionally enforced at compile time (§5.1).

## 5. Implied specs that look wrong, or disagree with the code

1. **Resolved: layout invariants now checked at compile time and over unsized values.**
   `src/pty_wrap.rs` now carries a compile-time `const` block asserting `size_of` and `align_of`
   equality for `()`, `u8`, `u32`, `[u16; 3]`, `&[u8]`, `&Provenance<[u8], Pty>`, and
   `&Provenance<str, Pty>`, while `provenance_adds_no_bytes` checks `size_of_val` and `align_of_val`
   over unsized `[u8]`, `[u32]`, and `str` references.
2. **Resolved: identity tests compare fat pointers and cover ZST and `[u32]` slices.**
   `wrap_scalar_reference_preserves_identity` now covers `u8` and `()`, and `wrap_str_preserves_bytes`
   checks `core::ptr::eq` (address + metadata) on `str`, `[u32]`, and `[()]`.
3. **Resolved: test-pinning prose already removed.** Neither `src/lib.rs` nor `src/pty_wrap.rs` cites
   test names as safety evidence; `pty_wrap_ref`'s SAFETY note cites `#[repr(transparent)]` directly.
4. **Resolved: `as_ref` doc states that the returned borrow strips its [`Origin`] tag.**
5. **Resolved: `_origin` field-order comment states Rust's unsized-last-field rule accurately.**
