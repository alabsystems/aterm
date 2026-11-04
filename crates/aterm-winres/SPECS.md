<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-winres: spec recovery from unit tests

Each row states a property of a pure function in `crates/aterm-winres/src/lib.rs`, recovered from
the tests in its `mod tests`. A test is deleted once Trust/Clean proves its primary spec and every
`also` spec. Functions no test reaches, among them the effectful `compile`, `locate_compiler`,
`find_on_path` and `host_sdk_arch`, have no row.

## 0. Notation used in the statements

- `disp(p)` = `p.display().to_string()`. `Path::display` is lossy on non-UTF-8 on the host.
- `fname(p)` = `p.file_name().and_then(OsStr::to_str).unwrap_or("")`, with host `Path`
  semantics: `/` is a separator on every host, and `\` is one only on Windows.
- `lc(s)` = `s.to_ascii_lowercase()`; `contains(s, t)` = `str::contains`.
- `key(n)` = `Some(v)` iff every `.`-separated component of `n` parses with `u64::from_str`
  **and** `v.len() == 4` **and** `v[0] == 10`; otherwise `None`. `u64::from_str` rejects `""`
  and overflow, and accepts a leading `+`.
- `lexdesc`: lexicographic order on `Vec<u64>`, descending.
- `ARGS(d, I, R, O)`, the argument table:
  - `Rc` ⇒ `["/nologo", "/I", I, "/fo", O, R]`
  - `LlvmRc` ⇒ `["/I", I, "/FO", O, R]`
  - `Windres` ⇒ `["-I", I, "-i", R, "-o", O, "-O", "coff"]`
- `EXT(d)`: `Rc | LlvmRc` ⇒ `"res"`; `Windres` ⇒ `"o"`.
- `blocking_gaps`: rows of §7 of `docs/AUDIT-trust-for-test-migration-2026-10-15.md`. Gap 0 (the
  installed seal publishes no Lean-discharged row) is on every row.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; the others are in §2),
`difficulty` (the Rust features the proof must handle), `blocking_gaps`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
WR1	contract	sdk_versions_newest_first	∀ finite names: sdk_versions_newest_first(names) = stable_sort_by(lexdesc on key, [n ∈ names | key(n) ≠ None]). Equivalently: (a) filter: n appears in the result iff key(n) ≠ None, with its input multiplicity; (b) order: for consecutive outputs a, b, key(a) ≥ key(b) lexicographically on the u64 vectors (numeric, not lexical, so 10.0.26100.0 precedes 10.0.9600.0); (c) stability: equal keys (10.0.0.0 and 10.00.0.0) keep input order. Corollary: the result is empty iff no element has a key.	lib.rs::sdk_versions_sort_numerically_newest_first, lib.rs::no_sdk_dirs_means_an_empty_list	HARD: &str slices, Vec<([u64; 4], &str)> with bitwise-complement descending keys [!v0, !v1, !v2, !v3], explicit for loop over generic impl IntoIterator, str::split + u64::from_str, stable slice sort_by_key with a projection closure. Needs a content model of str/Vec and a sorted-permutation spec.	0,1,3,5,6,7,11,13
WR2	contract	dialect_of	∀ path p, with n = lc(fname(p)): dialect_of(p) = if contains(n, "windres") then Windres else if contains(n, "llvm-rc") then LlvmRc else Rc. So the match is ASCII case-insensitive and looks only at the final component, windres takes precedence over llvm-rc, and a path with no UTF-8 file name gives Rc.	lib.rs::dialect_follows_the_program_name	MEDIUM: &Path/OsStr (file_name, to_str), String from to_ascii_lowercase, substring search, Option combinators, enum result.	0,1,3,5,6
WR3	table-property	arguments	∀ d: Dialect, ∀ paths inc, rc, out: arguments(d, inc, rc, out) = ARGS(d, disp(inc), disp(rc), disp(out)), element for element (lengths 6/5/8). So the include dir always directly follows the include flag, the output path directly follows its output flag, and only Windres carries the "-O coff" object-format pair.	lib.rs::argument_shapes_per_dialect	MEDIUM: Vec<String> built by vec!, String from Path::display().to_string() through the Display/fmt machinery, enum match. Quantifies over paths only through disp, which can stay opaque.	0,1,3,6
WR4	table-property	Dialect::artifact_extension	∀ d: Dialect, d.artifact_extension() = EXT(d): Rc and LlvmRc give "res" (the .res the MSVC/LLD linkers accept), and Windres gives "o" (a COFF object for GNU ld). Companion invariant with WR3: d = Windres ⇔ ARGS(d, ..) ends with ["-O", "coff"] ⇔ EXT(d) = "o".	lib.rs::artifact_extension_matches_what_the_linker_accepts	EASY: a three-arm match on a Copy enum returning &'static str literals; decidable by case split.	0,1,3,6
WR5	contract	Outcome::{is_linked,is_failure}	∀ o: Outcome, o.is_linked() = matches!(o, Linked(_)) and o.is_failure() = matches!(o, NoCompiler(_) | Failed(_)): Linked(p) is linked and not failure, NotWindows is neither, and NoCompiler(w) and Failed(w) are failure and not linked for every w.	lib.rs::outcome_display_names_the_state (is_linked/is_failure half)	EASY: &self on an enum with PathBuf/String payloads, matches!, bool result; the payloads are never read.	0,1,2,3,6
WR6	contract	<Outcome as Display>::fmt	∀ o: Outcome, o.to_string() = match o { Linked(p) ⇒ "linked " ++ disp(p), NotWindows ⇒ "target is not Windows; no resources apply", NoCompiler(w) ⇒ "no resource compiler found: " ++ w, Failed(w) ⇒ "resource compiler failed: " ++ w }, and fmt returns Ok whenever the sink does. The load-bearing content is payload preservation (the NoCompiler/Failed message ends with exactly the carried string, so where-it-looked and stderr reach the build warning) and that the four variant prefixes are pairwise distinct. The exact wording is not itself a spec.	lib.rs::outcome_display_names_the_state (Display half)	MEDIUM-HARD: Display impl, &mut fmt::Formatter over a dyn Write sink, write!/format_args, String concatenation model, Path::display.	0,1,2,3,6,8
WR7	round-trip	encode_winres_version_le, decode_winres_version_le	∀ (a, b, c, d): (u32, u32, u32, u32): encode_winres_version_le(a, b, c, d) = a.to_le_bytes() ++ b.to_le_bytes() ++ c.to_le_bytes() ++ d.to_le_bytes(), and ∀ bytes: [u8; 16]: encode_winres_version_le(decode_winres_version_le(bytes)) = bytes ∧ decode_winres_version_le(encode_winres_version_le(a, b, c, d)) = (a, b, c, d). Both are total const fns.	lib.rs::winres_version_le_round_trips_and_lays_out_four_u32_words	TRIVIAL: const fixed-width u32↔u8 shifts, casts and bitwise OR over [u8; 16] and (u32, u32, u32, u32); bitvector-decidable with no loops or heap.	0,1
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test; `also` lists the other specs it samples.

```tsv
file	test	primary	also
src/lib.rs	sdk_versions_sort_numerically_newest_first	WR1	
src/lib.rs	no_sdk_dirs_means_an_empty_list	WR1	
src/lib.rs	dialect_follows_the_program_name	WR2	
src/lib.rs	argument_shapes_per_dialect	WR3	
src/lib.rs	artifact_extension_matches_what_the_linker_accepts	WR4	
src/lib.rs	outcome_display_names_the_state	WR6	WR5
src/lib.rs	winres_version_le_round_trips_and_lays_out_four_u32_words	WR7	
```

## 3. Coverage

- 7 tests: all 7 are retired by a spec (WR1 retires 2; WR2, WR3, WR4, WR6 and WR7 retire 1 each,
  with WR5 sampled alongside WR6).

## 4. Tests that are not spec samples

None.

## 5. Implied specs that look wrong, or disagree with the code

1. **Resolved — `outcome_display_names_the_state` checks payload preservation, `is_linked` and `is_failure` across all variants.**
   It checks `is_linked`, `is_failure` and `Display` across `Linked`, `NotWindows`, `NoCompiler`
   and `Failed`, asserting payload suffix preservation and pairwise distinctness without pinning
   prose substrings.
2. **Resolved — `sdk_versions_newest_first` documents `u64::from_str` parsing and preserves tie order.**
   `sdk_versions_newest_first` sorts solely on the numeric key via stable `sort_by_key` over
   `[!v0, !v1, !v2, !v3]` (rather than `sort_unstable` over `(key, name)`), documents `u64::from_str`
   component parsing and input-order tie stability, and `sdk_versions_sort_numerically_newest_first`
   covers leading `+`, equal-key tie order in both directions (`10.00.0.0` vs `10.0.0.0`), and `u64`
   overflow rejection.
3. **Resolved — `dialect_follows_the_program_name` covers WR2 precedence, final-component scoping, and `Rc` defaults.**
   Covers `llvm-windres` and `windres-llvm-rc` (`windres` precedence over `llvm-rc`), parent-directory
   exclusion (`/opt/windres/llvm-rc`, `/opt/llvm-rc/custom-rc`), empty/unrecognised names, and non-UTF-8
   file names defaulting to `Rc`.
4. **Resolved — `arguments()` doc comment no longer cites the unit test.** `arguments()` is documented
   as `"The argument vector for one dialect."`.

## 6. Proof order

The specs are independent; WR3 and WR6 share only `disp`, which can stay an uninterpreted function
of the path.

1. **WR7**: fixed-width `u32`↔`[u8; 16]` bitvector round-trip with no branches, loops or heap
   (needs only gaps 0 and 1).
2. **WR4**: a three-arm match with literal results, the smallest test of enum carriers.
3. **WR5**: `matches!` predicates on `&self`; adds gap 2.
4. **WR2**: needs a string content model, `to_ascii_lowercase`, substring search and gap 5.
5. **WR3**: a fixed `vec!` per arm, once `Vec<String>` contents are modelled; with WR4 it gives
   WR4's companion invariant.
6. **WR6**: the `Formatter`/`write!` model (gaps 2, 8); prove the payload suffix first.
7. **WR1**: a loop, a generic `IntoIterator`, a sort and a sorted-permutation postcondition (gaps 5,
   7, 11, 13), stated with §5.2's tie order.
