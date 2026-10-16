<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-getrandom: spec recovery from its 9 unit tests

> Status 2026-10-16: this file is the crate's migration worklist. Each spec below is proved
> through Trust/Clean against the compiled Rust function, and then the tests it subsumes are
> deleted. Nothing has been proved or deleted yet.

Source: `crates/aterm-getrandom` (read only). I read every `#[test]`
(`src/error.rs` 1, `tests/fill.rs` 5, `tests/recorded.rs` 3 = 9) and the code each one reaches:
`src/lib.rs`, `src/error.rs`, and `crates/aterm-uds/src/rand.rs` (`fill`, `fill_from`, the Windows
`fill`). Nothing was compiled or run. The claims below come from reading the code.

Result: **8 specs**, covering **7 tests**. **2 tests are not spec samples** (§4).

Gap numbers refer to `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7. Gap 0 (the
installed seal publishes no Lean-discharged row) is a prerequisite for every row, so the
`blocking_gaps` column lists only the gaps beyond it. No spec here is blocked by gap 0 alone.

## 0. Notation used in the statements

- `E(c)` = `Error(c)` for `c: NonZeroU32`, the same value as `Error::from(c)`.
- `IS` = `Error::INTERNAL_START` = 2^31. `CS` = `Error::CUSTOM_START` = 2^31 + 2^30.
- `DESC`: the 13-row table in `error.rs::internal_desc`. Offsets `n` in {0..8, 11..14} from `IS`
  each map to a fixed `&'static str`. Every other code maps to `None`.
- `OsText(n)` = `std::io::Error::from_raw_os_error(n).to_string()`.
- `os_description(n)` = `OsText(n)` with the suffix `" (os error n)"` removed. It is `None` when the
  suffix is missing, when the rest is empty, or when the rest starts with `"Unknown error"`. This is
  the Unix behaviour; on every other platform it is `None`.
- `PlatformSpec`: the effect assumptions on the OS entropy calls.
  - `getentropy(p, k)` with `k <= 256` returning 0 means `k` CSPRNG bytes were written at `p[0..k]`.
  - `read_exact` returning `Ok` means all `len` bytes were written.
  - `BCryptGenRandom` returning status 0 means `chunk.len()` bytes were written.
- `Written(buf, i)`: byte `i` of `buf` was stored by an OS entropy source during this call.
- The CSPRNG assumption: OS output is unpredictable, so two fills differ. This is the statistical
  part of `tests/fill.rs`. It is an assumption about the OS, and no spec can prove it. The specs
  below only cite it.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary in §2),
`difficulty` (the Rust features the proof must handle), `blocking_gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
error-code-table	table-property	error::internal_error, Error::UNSUPPORTED..Error::NODE_ES_MODULE (13 consts), Error::INTERNAL_START, Error::CUSTOM_START, Error::code	IS = 2^31 and CS = 2^31 + 2^30. Each named constant K_n has K_n.code().get() = IS + n, for UNSUPPORTED=0, ERRNO_NOT_POSITIVE=1, UNEXPECTED=2, IOS_SEC_RANDOM=3, WINDOWS_RTL_GEN_RANDOM=4, FAILED_RDRAND=5, NO_RDRAND=6, WEB_CRYPTO=7, WEB_GET_RANDOM_VALUES=8, VXWORKS_RAND_SECURE=11, NODE_CRYPTO=12, NODE_RANDOM_FILL_SYNC=13 and NODE_ES_MODULE=14. internal_error(n) never reaches its panic arm for any n: u16. The layout part (size_of::<Error>() = 4 = size_of::<Result<(), Error>>(), the NonZeroU32 niche) is a compiler-layout fact, not a Trust clause; it becomes a `const _: () = assert!(...)` in src/error.rs.	tests/recorded.rs::the_named_codes_are_upstreams	EASY: newtype struct over NonZeroU32, const fn with a match and a panic arm, u16-to-u32 cast and add; decidable by const evaluation.	1, 3
error-raw-os-error-partition	contract	Error::raw_os_error, Error::code, <Error as From<NonZeroU32>>::from	∀ c: NonZeroU32: Error::from(c).code() = c, and E(c).raw_os_error() = Some(c.get() as i32) if c.get() < IS, else None. So raw_os_error is Some exactly on 1..IS, and every Some value is positive.	(secondary only, see §2)	EASY: newtype struct, NonZeroU32::get, i32::try_from(u32).ok() (std call), an Option result, one u32 binder.	1, 3, 5
error-internal-format	contract	<Error as Display>::fmt, <Error as Debug>::fmt, error::internal_desc, <std::io::Error as From<Error>>::from	∀ c: NonZeroU32 with c.get() ≥ IS: E(c).to_string() = d if DESC(c) = Some(d), else format!("Unknown Error: {c}"). format!("{:?}", E(c)) = `Error { internal_code: c, description: "d" }` when DESC(c) = Some(d) (d Debug-escaped), else `Error { unknown_code: c }`. io::Error::from(E(c)) has kind() = Other, raw_os_error() = None and to_string() = E(c).to_string(). internal_desc(e) = DESC(e.code()) exactly: Some on the 13 named codes, None on every other u32.	tests/recorded.rs::internal_and_custom_codes_answer_as_upstream	MEDIUM: a match on struct constants (PartialEq), &'static str, String and format!, fmt::Formatter and the debug_struct builder, io::Error::other (a boxed trait object), quantification over a u32 range.	1, 3, 4, 5, 6
error-os-format	contract	<Error as Display>::fmt, <Error as Debug>::fmt, error::os_description, <std::io::Error as From<Error>>::from	∀ c: NonZeroU32 with c.get() < IS, let n = c.get() as i32. E(c).to_string() = d when os_description(n) = Some(d), else format!("OS Error: {n}"). format!("{:?}", E(c)) = `Error { os_error: n, description: "d" }` when os_description(n) = Some(d), else `Error { os_error: n }`. io::Error::from(E(c)) equals io::Error::from_raw_os_error(n): the same raw_os_error Some(n) and the same kind. Stated against std's text, the spec does not depend on the platform. Only the five literal rows are platform effect assumptions on std's strerror text (Darwin and glibc agree): errnos 1, 2, 4, 5 and 13 give "Operation not permitted", "No such file or directory", "Interrupted system call", "Input/output error" and "Permission denied"; i32::MAX is unknown, so it gives "OS Error: 2147483647".	tests/recorded.rs::os_codes_answer_as_upstream	HARD: String, format!, strip_suffix and starts_with on &str, Option, io::Error::from_raw_os_error and to_string (std reaching strerror_r over FFI), fmt::Formatter, cfg(unix).	1, 3, 4, 5, 6, 8
from-io-errno-mapping	contract	Error::from_io	∀ e: &std::io::Error: Error::from_io(e) = E(n) when e.raw_os_error() = Some(n) with n > 0, so from_io(e).raw_os_error() = Some(n). It is ERRNO_NOT_POSITIVE when e.raw_os_error() = Some(n) with n ≤ 0, and UNEXPECTED when e.raw_os_error() = None. The None case covers a kind-only error such as UnexpectedEof and an io::Error::other text error.	src/error.rs::tests::an_aterm_uds_error_maps_to_upstreams_codes	MEDIUM: &std::io::Error is an opaque std type and needs a raw_os_error model; an Option match, u32::try_from(i32).ok(), and_then(NonZeroU32::new), map_or with a tuple-struct constructor as a fn item.	1, 3, 4
getrandom-forwards	contract	getrandom::getrandom, imp::fill	∀ dest: &mut [u8]: if dest.len() = 0, then getrandom(dest) = Ok(()), dest' = dest and no OS source is called. If dest.len() > 0 on unix or windows, then getrandom(dest) = aterm_uds::rand::fill(dest).map_err(Error::from_io), and dest' is what fill left. Off unix and windows it is Err(UNSUPPORTED). It never returns Ok unless fill returned Ok, so no partly filled buffer comes back as Ok.	tests/fill.rs::an_empty_request_succeeds	MEDIUM: &mut [u8] with two-state contents, is_empty, composition with aterm_uds::rand::fill's ensures (cross-crate), Result::map_err with a closure calling Error::from_io, cfg arms.	1, 2, 4, 12
getrandom-uninit-refines-getrandom	refinement	getrandom::getrandom_uninit	∀ dest: &mut [MaybeUninit<u8>]: getrandom_uninit(dest) first initialises every element of dest to 0, then behaves as getrandom on the same memory viewed as bytes. If the result is Ok(b), then b.as_ptr() = dest.as_ptr() as *const u8, b.len() = dest.len(), and b's contents are what getrandom(b) left. If the result is Err(e), then e is getrandom's error and every element of dest is initialised. With dest.len() = 0 it returns Ok of an empty slice.	tests/fill.rs::getrandom_uninit_writes_every_byte_and_returns_the_same_memory	HARD: MaybeUninit slices, write_copy_of_slice (initialisation, pointer identity and provenance), a vec![0; n] heap allocation, the ? operator, a returned &mut borrow tied to the input, composition with getrandom-forwards.	1, 2, 3, 4, 8
os-fill-writes-every-byte	contract	aterm_uds::rand::fill (unix), aterm_uds::rand::fill_from, aterm_uds::rand::fill (windows)	∀ buf: &mut [u8]: if aterm_uds::rand::fill(buf) = Ok(()), then Written(buf, i) holds for every i < buf.len(), under PlatformSpec. Unix: buf.chunks_mut(256) splits [0, len) into consecutive chunks of at most 256 bytes. Either the entropy closure returned true for every chunk, and getentropy(chunk) = 0 means every chunk was written, or the read_exact(buf) fallback over the whole buffer returned Ok. Windows: every chunks_mut(u32::MAX) chunk got BCryptGenRandom status 0. If any OS step fails without a successful fallback, the result is Err. Composed with getrandom-forwards, this gives getrandom(dest) = Ok ⇒ every byte of dest is OS CSPRNG output, at every length (fill.rs samples lengths 0 to 4096). That two fills differ is the CSPRNG assumption and is not part of the proof.	tests/fill.rs::getrandom_writes_every_byte_of_every_length	XL: the chunks_mut iterator plus Iterator::all with an FnMut closure (a loop with a coverage invariant over the chunk partition), unsafe extern "C" getentropy and BCryptGenRandom FFI, std::fs::File::open and read_exact (fs effects), an atomic once-flag and poll(2) on Linux, &mut [u8]. fill_from is trust::skip today.	1, 2, 7, 8, 12, 13
```

## 2. Test → spec map (TSV; all 9)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
A test is deleted only when its primary and every `also` spec are proved.

```tsv
file	test	primary	also
src/error.rs	tests::an_aterm_uds_error_maps_to_upstreams_codes	from-io-errno-mapping	
tests/recorded.rs	the_named_codes_are_upstreams	error-code-table	
tests/recorded.rs	internal_and_custom_codes_answer_as_upstream	error-internal-format	error-raw-os-error-partition
tests/recorded.rs	os_codes_answer_as_upstream	error-os-format	error-raw-os-error-partition
tests/fill.rs	getrandom_writes_every_byte_of_every_length	os-fill-writes-every-byte	getrandom-forwards
tests/fill.rs	getrandom_uninit_writes_every_byte_and_returns_the_same_memory	getrandom-uninit-refines-getrandom	os-fill-writes-every-byte
tests/fill.rs	an_empty_request_succeeds	getrandom-forwards	getrandom-uninit-refines-getrandom
tests/fill.rs	ring_system_random_writes_every_byte_of_every_length	(none, §4)	
tests/fill.rs	the_property_catches_a_fill_that_skips_bytes	(none, §4)	
```

## 3. Coverage

- 9 tests read. 7 map to one of the 8 specs. 2 are not spec samples (§4).
- `error-raw-os-error-partition` has no primary test. It is the shared lemma under both
  `recorded.rs` formatting tests, which sample it at the `IS` boundary.
- The `fill.rs` statistical checks (two fills of 16 bytes or more differ; a missed position is
  caught with probability 1 - 2^-64) sample the CSPRNG assumption, not a crate property. No proof
  carries them over. Deleting `getrandom_writes_every_byte_of_every_length` and
  `getrandom_uninit_writes_every_byte_and_returns_the_same_memory` after their specs are proved
  drops only that statistical check, which tests the OS, not this crate.

## 4. Tests that are not spec samples

> Status 2026-10-16: `ring_system_random_writes_every_byte_of_every_length` is deleted. `the_property_catches_a_fill_that_skips_bytes` is kept until `os-fill-writes-every-byte`, `getrandom-forwards` and `getrandom-uninit-refines-getrandom` are proved.

| test | why | action |
|---|---|---|
| tests/fill.rs::the_property_catches_a_fill_that_skips_bytes | This is the negative control for the test-only helper `every_byte_is_written` in fill.rs. No production code calls that helper, so this test checks the test, not the crate. Once `os-fill-writes-every-byte` and `getrandom-forwards` are proved, the proof's own negative control does this job: mutate `fill_from`'s chunk loop or `getrandom`'s empty check, and the check goes red. | **delete** (with the helper, once its last caller is deleted) |
| tests/fill.rs::ring_system_random_writes_every_byte_of_every_length | It samples no property of this crate. Its byte coverage repeats `getrandom_writes_every_byte_of_every_length` through ring's wrapper, a third-party crate. Its other claim, that ring reaches this package through the `[patch.crates-io]` row, is a dependency-graph fact owned by the `forge check` OB-12 guard named in Cargo.toml. This test cannot see that fact (§5.1). | **delete** |

## 5. Suspicious specs

1. **`ring_system_random_writes_every_byte_of_every_length` does not test what it claims.** Its doc
   comment says it checks the path "through the patch row it is this crate". Nothing in it can tell
   this crate from the crates.io getrandom 0.2, because both fill every byte. It passes even when
   the patch row replaces nothing.
2. **`os_codes_answer_as_upstream` pins std's unstable text.** It asserts that
   `format!("{:?}", io.kind())` is `"Uncategorized"` for errno 5 and for `i32::MAX`. `Uncategorized` is
   std's unstable internal `ErrorKind`. Its Debug text and the errno-to-kind mapping can change
   between std versions. That pins std, not upstream getrandom. `error-os-format` instead compares
   the converted error with `io::Error::from_raw_os_error(n)`.
3. **`os_codes_answer_as_upstream` states a Darwin recording as a Unix fact.** The strerror strings
   were recorded on aarch64-apple-darwin, and the test asserts them on every `cfg(unix)` target. The
   module comment says Darwin and glibc share them. On musl and the BSDs, "Interrupted system call"
   and "Input/output error" can read differently. `error-os-format` keeps the five literal rows as a
   platform assumption, not a property of the crate.
4. **`the_named_codes_are_upstreams` tests layout at run time.** `size_of::<Result<(), Error>>() == 4`
   is a compiler niche-layout fact, not getrandom behaviour. A const assert in `src/error.rs` holds it;
   it is not a Trust clause.
5. **The `tests/fill.rs` module doc overstates its bound.** It says a missed position "misses all of
   them with probability 2^-64". That holds only if all 8 rounds run. The loop correctly stops after
   round 1 once every byte has been written, so the bound covers only the failure path. The "two
   fills of 16 bytes or more must differ" check compares later rounds with round 0 only. Neither
   point is a bug, but both make the check statistical. That is the CSPRNG assumption
   `os-fill-writes-every-byte` cites, and no proof carries it over.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone. Every one needs at least gap 1 (a struct, slice or Option
carrier), so the order follows the smallest remaining gap set.

1. **`error-code-table`** (gaps 1, 3). A `const fn` over a `NonZeroU32` newtype with u16→u32
   arithmetic, decidable by evaluating 13 constants plus a bound on `n: u16`. It retires
   `the_named_codes_are_upstreams` once the layout row moves to a const assert. Do the const assert
   now; it needs no proof.
2. **`error-raw-os-error-partition`** (gaps 1, 3, 5). One u32 binder and a comparison with `IS`. It
   is the lemma both formatting specs stand on.
3. **`from-io-errno-mapping`** (gaps 1, 3, 4). Needs a `raw_os_error` model of `std::io::Error`, then
   it is a three-way Option/sign case split. It retires the only in-crate unit test.
4. **`getrandom-forwards`** (gaps 1, 2, 4, 12). Small body. The empty case is local; the non-empty
   case cites `aterm_uds::rand::fill`'s ensures across crates. It retires `an_empty_request_succeeds`.
5. **`error-internal-format`** (gaps 1, 3, 4, 5, 6). String and Formatter carriers over a finite
   table plus a fallback arm.
6. **`getrandom-uninit-refines-getrandom`** (gaps 1, 2, 3, 4, 8). Needs MaybeUninit initialisation and
   pointer identity on top of `getrandom-forwards`.
7. **`error-os-format`** (gaps 1, 3, 4, 5, 6, 8). Rests on std's strerror text, an FFI effect.
8. **`os-fill-writes-every-byte`** (gaps 1, 2, 7, 8, 12, 13). It is the most valuable spec (it is the
   crate's reason to exist) and the hardest: a chunk-partition loop invariant through a closure, FFI
   PlatformSpec rows, fs effects, and `fill_from` is `trust::skip` today. It lives in `aterm-uds`, so
   it is proved there and cited here.
