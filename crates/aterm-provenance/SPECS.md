<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-provenance: spec recovery from its 5 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 3 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-16: this is the crate's migration worklist. Each spec below is proved through
> Trust/Clean against the compiled Rust function, and then the tests it subsumes are deleted.
> Nothing has been proved or deleted yet.

Source: `crates/aterm-provenance` (read only). I read every `#[test]`
(`src/pty_wrap.rs` 3, `tests/unit.rs` 2 = 5) and the code each one reaches
(`src/pty_wrap.rs::pty_wrap_ref`, `src/provenance.rs::{Provenance, Provenance::from_pty, Provenance::as_ref}`).
Nothing was compiled or run. The claims below come from reading the code.

Result: **3 specs**, covering **5 tests**. **0 tests are not spec samples** (§4).

## 0. Notation used in the statements

- `Prov<T>` = `aterm_provenance::Provenance<T, Pty>`: `#[repr(transparent)]` over `T`, with one other
  field, `_origin: PhantomData<fn() -> Pty>`, a 1-aligned ZST. `Provenance<T, O>` is the same for any
  `O: Origin`.
- `wrap` = `pty_wrap_ref`, `inner(p)` = `Provenance::as_ref(p)` (returns `&p.value`),
  `mk` = `Provenance::from_pty`.
- For a reference `r: &T` with `T: ?Sized`, `addr(r)` is its data pointer and `meta(r)` its pointer
  metadata (`()` for a sized `T`, the element length for `[U]`, the byte length for `str`).
- "`r1` is `r2`" means `addr(r1) = addr(r2) ∧ meta(r1) = meta(r2)`: the same fat pointer
  (`core::ptr::eq` over address and metadata), hence equal contents.
- Every spec is generic over `T`, unsized `T` included. A proof has to be per-definition (gap 11); checking
  it only at `u8`, `[u8]` and `str` is the sampling the tests already do.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty (Rust features the proof must
handle)`, `blocking gaps` (numbers from §7 of `docs/AUDIT-trust-for-test-migration-2026-10-15.md`; gap 0,
the installed seal, is the prerequisite for every row).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
layout-transparent	invariant	provenance::Provenance	∀ Sized T, ∀ O: Origin: size_of::<Provenance<T,O>>() = size_of::<T>() ∧ align_of::<Provenance<T,O>>() = align_of::<T>(). ∀ T: ?Sized, ∀ p: &Provenance<T,O>: size_of_val(p) = size_of_val(inner(p)) and the pointer metadata of p and inner(p) are equal. Under #[repr(transparent)] this is the language's layout guarantee, so the obligation reduces to: the attribute is present and _origin is a 1-ZST. It is the precondition pty-wrap-ref-identity's unsafe discharge cites. Stateable today as a const assertion over representative T (sized and unsized); the general form needs layout facts in Trust.	src/pty_wrap.rs::provenance_adds_no_bytes	EASY as a const assertion; for the general form, type-layout facts (size_of, align_of, size_of_val) under repr(transparent), generic T including unsized T, a PhantomData ZST field. No runtime code.	0, 1, 8, 11
from-pty-as-ref-roundtrip	round-trip	provenance::Provenance::from_pty, provenance::Provenance::as_ref	∀ T, ∀ v: T, with p = mk(v): *inner(&p) is the moved v, unchanged (no Clone call, no transformation), so *inner(&p) = v whenever T: PartialEq. Structural form (preferred, since a generic T has no PartialEq): mk moves v into p.value, and ∀ T: ?Sized, ∀ p: &Prov<T>: inner(p) = &p.value, a pure projection with p's borrow lifetime. The test's String equality is one instance.	-	EASY: generic struct with a PhantomData field, a const fn constructor, a &self accessor returning a reference into self (T: ?Sized); String (a heap-owning carrier) and its PartialEq<str> only in the sample	0, 1, 2, 4, 11
pty-wrap-ref-identity	contract	pty_wrap::pty_wrap_ref, provenance::Provenance::as_ref	∀ T: ?Sized, ∀ r: &'a T: wrap(r) has no UB and returns a &'a Prov<T> with inner(wrap(r)) is r (core::ptr::from_ref(inner(wrap(r))) = core::ptr::from_ref(r) as a fat-pointer comparison). Corollaries: for [U] and str, as_ptr and len are unchanged and *inner(wrap(r)) = *r (multi-byte UTF-8 included); for Sized T, *inner(wrap(r)) = *r. The call reads and writes no memory. Carries the soundness obligation of the crate's one unsafe block: &*(from_ref(r) as *const Prov<T>) is valid because layout-transparent makes the target's size, alignment and validity invariant equal T's.	src/pty_wrap.rs::wrap_scalar_reference_preserves_identity, src/pty_wrap.rs::wrap_str_preserves_bytes	MEDIUM: raw-pointer cast plus unsafe deref inside a const fn; fat pointers over ?Sized generics ([u8], str); references and lifetimes; composition with the as_ref field projection; a per-definition generic proof. No loops, no heap.	0, 1, 2, 4, 8, 11
```

### Distribution by kind (3)

| kind | count | ids |
|---|---|---|
| invariant | 1 | layout-transparent |
| round-trip | 1 | from-pty-as-ref-roundtrip |
| contract | 1 | pty-wrap-ref-identity |

### Proof dependency order

`layout-transparent` and `from-pty-as-ref-roundtrip` are independent. `pty-wrap-ref-identity` cites both:
the layout fact discharges the unsafe cast, and `inner(p) = &p.value` lets the identity be stated through
`as_ref`.

## 2. Test → spec map (TSV; all 3)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/pty_wrap.rs	provenance_adds_no_bytes	layout-transparent	
src/pty_wrap.rs	wrap_scalar_reference_preserves_identity	pty-wrap-ref-identity	from-pty-as-ref-roundtrip (as_ref projection)
src/pty_wrap.rs	wrap_str_preserves_bytes	pty-wrap-ref-identity	from-pty-as-ref-roundtrip (as_ref projection), layout-transparent (unsized str)
```

## 3. Coverage

- 5 tests read. All 5 map to one of the 3 specs.
- `pty-wrap-ref-identity` retires 2 of the 3 tests in `src/pty_wrap.rs`, and the crate's only `unsafe`
  block gets a proof in place of two samples (one scalar, one `str`). `layout-transparent` retires the third.
- Once all three specs are proved and their tests deleted, the `#[cfg(test)] mod tests` in
  `src/pty_wrap.rs` is empty and goes away.

## 4. Tests that are not spec samples

None. Every test samples one of the specs above.

| test | why | action |
|---|---|---|
| (none) | | |

## 5. Implied specs that look wrong, or disagree with the code

1. **The layout test checks less than the cast needs.** `pty_wrap.rs::provenance_adds_no_bytes`
   checks only `size_of` at three sized `T` (`u8`, `u32`, `&[u8]`), though its doc says the cast is
   sound only while the layouts are identical. It does not check alignment, an unsized `T`, or that
   the attribute is present. Without `#[repr(transparent)]` the sizes would very likely still match,
   because `PhantomData` is a ZST, so the test does not guard what `pty_wrap_ref`'s unsafe cast relies
   on. Its `&[u8]` row measures a sized reference field, not the unsized `Provenance<[u8], Pty>` /
   `Provenance<str, Pty>` the cast produces. The spec it stands for is `layout-transparent`; the
   cheapest faithful replacement is a const assertion covering size and alignment at sized `T` plus
   `size_of_val` on an unsized `&Prov<[u8]>` and `&Prov<str>`.
2. **The identity tests never compare fat pointers.** `wrap_scalar_reference_preserves_identity` compares
   thin pointers for a sized `u8`; `wrap_str_preserves_bytes` compares `as_ptr` and `len` separately.
   Neither samples a non-`u8` element type or a ZST `T`. `pty-wrap-ref-identity` states the general
   fat-pointer identity.
3. **Prose that pins tests.** The `# Coverage` section of the module doc in `src/pty_wrap.rs` gives the
   tests as the evidence for the unsafe cast, and the header comment in `src/lib.rs` says the reinterpret
   is "pinned by its layout-equivalence tests". Both go stale when the tests are deleted; rewrite them to
   cite `pty-wrap-ref-identity` and `layout-transparent`.
4. **`as_ref`'s doc overstates.** It opens "Preserves origin" and then says the borrow is not tagged. It
   returns a bare `&T` with the `Pty` tag stripped, so only the lifetime bound holds for the returned borrow.
   Reword the doc to say that.
5. **Field-order comment.** `Provenance` says `_origin` is placed before `value` "so the unsized-trailing
   layout works". Under `#[repr(transparent)]` field order does not affect layout; an unsized `value` must be
   the last field only because Rust requires the unsized field to be last. The comment's reason is
   imprecise, not the code.

## 6. Proof order (easiest, high value first)

No spec is blocked by gap 0 alone; all three also need generics (11) and some carrier support (1).

1. **`layout-transparent`**. Retires 1 test and is the precondition the unsafe discharge cites. Its const
   assertion form needs no Trust lane at all and can replace `provenance_adds_no_bytes` now (fixing
   §5.1). The general form waits on layout facts (gaps 1, 8, 11).
2. **`from-pty-as-ref-roundtrip`**. Safe code: one constructor that moves its argument into a field and one
   accessor that borrows it. The structural form needs no `T: PartialEq` and no `String` content model, so
   beyond gap 0 it needs the `&self` view (2), composition (4), generics (11) and only a minimal carrier
   (1). It retires no test now and supplies the `inner(p) = &p.value` lemma.
3. **`pty-wrap-ref-identity`**. Highest value: the crate's only `unsafe`, and it retires 2 tests. It adds
   the unsafe-block discharge (gap 8) and fat-pointer metadata for `[U]`/`str` (gap 1) on top of step 2.
   Prove it once over generic `T: ?Sized`, not per instance.

After all three: delete the 5 subsumed tests and fix the prose in §5.3, §5.4 and §5.5.
