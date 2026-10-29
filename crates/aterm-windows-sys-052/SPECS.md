<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-windows-sys-052: spec recovery (1 compile-time-discharged spec, 0 unit tests)

> Status 2026-10-28: `WSYS052-PF-VALUE` is discharged at compile time in `tests/recorded.rs`,
> and its former `#[test]` fn is deleted, so the crate has zero runtime `#[test]` fns.

Source: `crates/aterm-windows-sys-052` (read only). `tests/recorded.rs` checks all four
recorded declarations at compile time (`0` `#[test]` fns), against `src/lib.rs` and
`tests/oracle/conformance/src/lib.rs`.

Result: **1 spec** (discharged at compile time), **0 runtime `#[test]` fns**. **0 tests are not spec samples** (§4).

## 0. Notation used in the statements

- `PF` := `windows_sys::Win32::System::Threading::PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE`. It is declared in
  `src/lib.rs` as `pub const PF: PROCESSOR_FEATURE_ID = 30u32`, where `PROCESSOR_FEATURE_ID = u32`.
- `RECORDED` := the answers windows-sys 0.52.0 gave the oracle (`tests/oracle/PROVENANCE.md`;
  `targo --unverified run -p aterm-win32-oracle -- oracle-052` runs it; x86_64- and
  aarch64-pc-windows-msvc, recorded 2026-10-15).
- `tests/recorded.rs` checks all four recorded declarations as `const _` items on every build:
  `BOOL = i32`, `PROCESSOR_FEATURE_ID = u32`,
  `IsProcessorFeaturePresent : unsafe extern "system" fn(u32) -> i32`, and
  `assert!(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE == 30)`. No runtime `#[test]` fn remains.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`,
`difficulty (Rust features the proof must handle)`, `blocking gaps (audit §7)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
WSYS052-PF-VALUE	table-property	windows_sys::Win32::System::Threading::PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE	PF : u32 ∧ PF == 30, the winnt.h value that windows-sys 0.52.0 RECORDED on x86_64- and aarch64-pc-windows-msvc. The statement has one row and no free variables, so it holds on every host and target where the crate compiles. Nothing is quantified. Cheapest sound discharge: `const _: () = assert!(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE == 30);` in tests/recorded.rs, next to the three existing `const _` checks. rustc CTFE proves it on every build of the test target, and planting any other value breaks the build (the negative control). A Clean island form (`def pf_arm_v8_crypto : UInt32 := 30`, with the Rust const bound to it) would only restate the same constant.	(none: discharged at compile time in tests/recorded.rs)	TRIVIAL: one scalar u32 const. No loops, no heap, no &mut, no unsafe. The spec does not reach the surrounding extern "system" fn declaration.	0
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
`the_constant_holds_its_recorded_value` was replaced by the compile-time `const _` assertion in
`tests/recorded.rs`, leaving `0` runtime `#[test]` fns.

```tsv
file	test	primary	also
```

## 3. Coverage

- 0 runtime `#[test]` fns remain (1 original test converted to CTFE; 0 non-spec samples).
- All four recorded declarations, including `WSYS052-PF-VALUE`, are checked by the compiler on
  every build of `tests/recorded.rs`. Planted mismatches against the registry crate
  stay with the oracle (`tests/oracle/`).

## 4. Tests that are not spec samples

None. The one test samples a real (if degenerate) property.

| test | why | recommendation |
|---|---|---|
| (none) | | |

## 5. Implied specs that look wrong, or disagree with the code

1. **(Fixed 2026-10-28.) `tests/recorded.rs` now checks all four declarations at compile time.**
   `the_constant_holds_its_recorded_value` was replaced with
   `const _: () = assert!(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE == 30);`, discharging
   `WSYS052-PF-VALUE` on every build of `tests/recorded.rs` without needing a runtime test.

## 6. Proof order (easiest, high value first)

1. **`WSYS052-PF-VALUE`** *(discharged at compile time in `tests/recorded.rs`)*. A Clean island
   binding adds nothing over CTFE for a closed scalar constant; add one only if a future policy
   requires every spec row to carry a Trust-published entry after the gap 0 seal update.
