<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-windows-sys-052: spec recovery from its 1 unit tests

> Status 2026-10-16: this file is the crate's migration worklist. The one spec
> below gets discharged at compile time (or through Trust/Clean once gap 0 is
> closed). After that, the test it subsumes is deleted and the crate has zero `#[test]` fns.

Source: `crates/aterm-windows-sys-052` (read only). I read its one `#[test]`
(`tests/recorded.rs` 1), the declarations in `src/lib.rs`, and the oracle's
`tests/oracle/conformance/src/lib.rs`. Nothing was compiled or run.

Result: **1 spec**, covering **1 test**. **0 tests are not spec samples** (§4).

## 0. Notation used in the statements

- `PF` := `windows_sys::Win32::System::Threading::PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE`. It is declared in
  `src/lib.rs` as `pub const PF: PROCESSOR_FEATURE_ID = 30u32`, where `PROCESSOR_FEATURE_ID = u32`.
- `RECORDED` := the answers windows-sys 0.52.0 gave the oracle (`tests/oracle/run.sh`,
  x86_64- and aarch64-pc-windows-msvc, recorded 2026-10-15).
- `tests/recorded.rs` holds the other three recorded declarations as `const _` items, so the
  compiler checks them on every build: `BOOL = i32`, `PROCESSOR_FEATURE_ID = u32`, and
  `IsProcessorFeaturePresent : unsafe extern "system" fn(u32) -> i32`. They are not tests and need no spec.
  Only the constant's value is held by a `#[test]`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`,
`difficulty (Rust features the proof must handle)`, `blocking gaps (audit §7)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
WSYS052-PF-VALUE	table-property	windows_sys::Win32::System::Threading::PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE	PF : u32 ∧ PF == 30, the winnt.h value that windows-sys 0.52.0 RECORDED on x86_64- and aarch64-pc-windows-msvc. The statement has one row and no free variables, so it holds on every host and target where the crate compiles. Nothing is quantified. Cheapest sound discharge: `const _: () = assert!(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE == 30);` in tests/recorded.rs, next to the three existing `const _` checks. rustc CTFE proves it on every build of the test target, and planting any other value breaks the build (the negative control). A Clean island form (`def pf_arm_v8_crypto : UInt32 := 30`, with the Rust const bound to it) would only restate the same constant.	tests/recorded.rs::the_constant_holds_its_recorded_value	TRIVIAL: one scalar u32 const. No loops, no heap, no &mut, no unsafe. The spec does not reach the surrounding extern "system" fn declaration.	0
```

## 2. Test → spec map (TSV; all 1)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
tests/recorded.rs	the_constant_holds_its_recorded_value	WSYS052-PF-VALUE	
```

## 3. Coverage

- 1 original test read. 1 maps to the 1 spec. 0 are not spec samples.
- After the conversion the crate has no `#[test]` fns. All four recorded declarations are then
  checked by the compiler on every build of `tests/recorded.rs`. Planted mismatches against the registry crate
  stay with the oracle (`tests/oracle/`).

## 4. Tests that are not spec samples

None. The one test samples a real (if degenerate) property.

| test | why | recommendation |
|---|---|---|
| (none) | | |

## 5. Implied specs that look wrong, or disagree with the code

1. **`tests/recorded.rs::the_constant_holds_its_recorded_value`: the spec is right but its form is
   wrong.** The value 30 matches winnt.h and `src/lib.rs`. But the file header says it holds "the four
   declarations" so that a change "fails wherever the workspace tests run". Three of the four are
   compile-time `const _` items. The fourth is a value fixed at compile time, yet it is held by a
   runtime `#[test]`, which passes only when tests are run, not on every build. Replacing it with
   `const _: () = assert!(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE == 30);` gives a strictly stronger check
   that needs no test harness. This is how `tests/oracle/conformance/src/lib.rs` already checks the same
   value, with a `plant-const` arm (31) as its negative control. Once converted, delete the `#[test]`.

## 6. Proof order (easiest, high value first)

1. **`WSYS052-PF-VALUE`**. It is blocked only by gap 0, and only on the Clean route. The compile-time
   `const _: () = assert!(..)` discharge needs no gap closed, so it can land now: add the const
   assertion to `tests/recorded.rs`, delete `the_constant_holds_its_recorded_value`, and change the
   header comment to say all four declarations are compile-time checks. A Clean island binding
   adds nothing over CTFE for a closed scalar constant. Do it only if the migration needs every spec
   to carry a Trust-published row, and then only after the gap 0 seal update.
