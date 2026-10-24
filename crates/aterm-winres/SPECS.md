<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-winres: spec recovery from its 6 unit tests

> Status: migration worklist. Each of the 6 specs below is proved through Trust/Clean about the
> compiled Rust function, and then the tests it subsumes are deleted.

Source: `crates/aterm-winres/src/lib.rs` (all 6 `#[test]` fns in `mod tests` and the functions they
reach).

Result: **6 specs**, covering **all 6 tests**. **0 tests are not spec samples** (§4).

None of the functions the tests reach has effects. `compile`, `locate_compiler`, `find_on_path`
and `host_sdk_arch` (env, fs, process) have no tests, so this worklist does not cover them.

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
- Gap numbers are the rows of §7 of
  `AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit).
  Gap 0 (the installed seal publishes no Lean-discharged row) is a prerequisite for every row,
  so it is listed on each spec.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps (audit §7)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
WR1	contract	sdk_versions_newest_first	∀ finite names: sdk_versions_newest_first(names) = stable_sort_by(lexdesc on key, [n ∈ names | key(n) ≠ None]). Equivalently: (a) filter: n appears in the result iff key(n) ≠ None, with its input multiplicity; (b) order: for consecutive outputs a, b, key(a) ≥ key(b) lexicographically on the u64 vectors (numeric, not lexical, so 10.0.26100.0 precedes 10.0.9600.0); (c) stability: equal keys (10.0.0.0 and 10.00.0.0) keep input order. Corollary: the result is empty iff no element has a key.	lib.rs::sdk_versions_sort_numerically_newest_first, lib.rs::no_sdk_dirs_means_an_empty_list	HARD: &str slices, Vec<u64> and Vec<(Vec<u64>, &str)>, str::split + u64::from_str, iterator adapters with closures (filter_map, map, collect into Result), generic impl IntoIterator, stable slice sort_by with a comparator closure, bool::then_some. Needs a content model of str/Vec and a sorted-permutation spec.	0,1,3,5,6,7,11,13
WR2	contract	dialect_of	∀ path p, with n = lc(fname(p)): dialect_of(p) = if contains(n, "windres") then Windres else if contains(n, "llvm-rc") then LlvmRc else Rc. So the match is ASCII case-insensitive and looks only at the final component, windres takes precedence over llvm-rc, and a path with no UTF-8 file name gives Rc.	lib.rs::dialect_follows_the_program_name	MEDIUM: &Path/OsStr (file_name, to_str), String from to_ascii_lowercase, substring search, Option combinators, enum result.	0,1,3,5,6
WR3	table-property	arguments	∀ d: Dialect, ∀ paths inc, rc, out: arguments(d, inc, rc, out) = ARGS(d, disp(inc), disp(rc), disp(out)), element for element (lengths 6/5/8). So the include dir always directly follows the include flag, the output path directly follows its output flag, and only Windres carries the "-O coff" object-format pair.	lib.rs::argument_shapes_per_dialect	MEDIUM: Vec<String> built by vec!, String from Path::display().to_string() through the Display/fmt machinery, enum match. Quantifies over paths only through disp, which can stay opaque.	0,1,3,6
WR4	table-property	Dialect::artifact_extension	∀ d: Dialect, d.artifact_extension() = EXT(d): Rc and LlvmRc give "res" (the .res the MSVC/LLD linkers accept), and Windres gives "o" (a COFF object for GNU ld). Companion invariant with WR3: d = Windres ⇔ ARGS(d, ..) ends with ["-O", "coff"] ⇔ EXT(d) = "o".	lib.rs::artifact_extension_matches_what_the_linker_accepts	EASY: a three-arm match on a Copy enum returning &'static str literals; decidable by case split.	0,1,3,6
WR5	contract	Outcome::is_linked	∀ o: Outcome, o.is_linked() = matches!(o, Linked(_)): true for Linked(p) for every p, and false for NotWindows, NoCompiler(w) and Failed(w) for every w.	lib.rs::outcome_display_names_the_state (is_linked half)	EASY: &self on an enum with PathBuf/String payloads, matches!, bool result; the payloads are never read.	0,1,2,3,6
WR6	contract	<Outcome as Display>::fmt	∀ o: Outcome, o.to_string() = match o { Linked(p) ⇒ "linked " ++ disp(p), NotWindows ⇒ "target is not Windows; no resources apply", NoCompiler(w) ⇒ "no resource compiler found: " ++ w, Failed(w) ⇒ "resource compiler failed: " ++ w }, and fmt returns Ok whenever the sink does. The load-bearing content is payload preservation (the NoCompiler/Failed message ends with exactly the carried string, so where-it-looked and stderr reach the build warning) and that the four variant prefixes are pairwise distinct. The exact wording is not itself a spec.	lib.rs::outcome_display_names_the_state (Display half)	MEDIUM-HARD: Display impl, &mut fmt::Formatter over a dyn Write sink, write!/format_args, String concatenation model, Path::display.	0,1,2,3,6,8
```

### Distribution by kind (6)

| kind | count | ids |
|---|---|---|
| contract | 4 | WR1, WR2, WR5, WR6 |
| table-property | 2 | WR3, WR4 |

### Gap histogram (specs blocked by each audit §7 gap)

| gap | specs | ids |
|---|---|---|
| 0 | 6 | all |
| 1 | 6 | all |
| 2 | 2 | WR5, WR6 |
| 3 | 6 | all |
| 5 | 2 | WR1, WR2 |
| 6 | 6 | all |
| 7 | 1 | WR1 |
| 8 | 1 | WR6 (the Formatter's `dyn Write` sink is an opaque effect boundary) |
| 11 | 1 | WR1 |
| 13 | 1 | WR1 |

No spec is blocked by gap 0 alone. Every spec needs gap 1 (enum or string carriers), gap 3
(enum `match` in an E6 body) and gap 6 (publishing a plain-Rust `ensures`).

### Proof dependency order

The specs are independent. The only shared lemma is the `disp` model, which WR3 and WR6 both
use and which can stay an uninterpreted function of the path. WR4 and WR3 together give the
companion invariant in WR4.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test
samples. A test is deleted only when its primary and every `also` spec are proved.

```tsv
file	test	primary	also
src/lib.rs	sdk_versions_sort_numerically_newest_first	WR1	
src/lib.rs	no_sdk_dirs_means_an_empty_list	WR1	
src/lib.rs	dialect_follows_the_program_name	WR2	
src/lib.rs	argument_shapes_per_dialect	WR3	
src/lib.rs	artifact_extension_matches_what_the_linker_accepts	WR4	
src/lib.rs	outcome_display_names_the_state	WR6	WR5
```

## 3. Coverage

- 6 original tests read. All 6 map to one of the 6 specs, and none is left over.
- The only collapse is `WR1`, which takes 2 tests. `outcome_display_names_the_state` is the
  reverse case: one test samples two specs (WR5 and WR6) and is retired only when both are proved.

## 4. Tests that are not spec samples

None. Every test samples a general property of a production function. No test exercises
test-only code, checks a tautology or measures timing.

| test | why | recommendation |
|---|---|---|
| (none) | | |

## 5. Implied specs that look wrong, or disagree with the code

1. **`outcome_display_names_the_state` mixes two specs and pins prose.** It checks `is_linked`
   (WR5) and `Display` (WR6) together. Its `Display` half is substring checks on `"not Windows"`
   and `"no resource compiler"`, so it pins wording. It never checks payload preservation, which
   is the actual contract. It also skips `Display` for `Linked` and `Failed`, and `is_linked` for
   `NotWindows` and `NoCompiler`. When WR5 and WR6 land, drop the substring asserts and do not
   replace them with a wording pin.
2. **`sdk_versions_newest_first`'s doc says "four numeric components" but the parse is
   looser.** `key()` uses `u64::from_str`, which accepts a leading `+` (so `10.+0.1.2` is kept),
   and it treats `10.00.0.0` as equal to `10.0.0.0`. Equal keys keep input order because `sort_by`
   is stable. WR1 states this behaviour exactly. Either tighten the parse to ASCII digits or
   document it. No test covers `+`, ties between equal keys, or u64 overflow.
3. **`dialect_of` uses plain substring matching.** Any name containing `windres` maps to
   `Windres` (for example `llvm-windres`, which really is windres-compatible). Every unrecognised
   name, including an arbitrary `ATERM_RC` binary, silently defaults to `Rc`. The test samples only
   the three happy names. WR2 states the precedence and the default, and the gnu-target refusal in
   `locate_compiler` relies on both.
4. **A doc comment cites the test.** `arguments()` says "Pure, so the shape is unit-tested". This
   goes stale when the test is deleted for WR3, so update it in the same commit.

## 6. Proof order (easiest, high value first)

No spec is blocked by gap 0 alone, so every one also waits on gaps 1, 3 and 6. Among them:

1. **WR4** (`Dialect::artifact_extension`). A three-arm match on a `Copy` enum with literal
   results, decidable by case split. It is the smallest test bed for enum carriers (gap 1) and
   `&'static str` results, and it backs the linker-facing half of WR3's companion invariant.
2. **WR5** (`Outcome::is_linked`). A `matches!` on `&self`, with payloads never read. It adds
   only gap 2 (a certified `&self` view) to WR4's needs.
3. **WR2** (`dialect_of`). Pure, with a short body, but it needs a string content model,
   `to_ascii_lowercase`, substring search and a quantified clause (gap 5). It is high value because
   `locate_compiler`'s gnu-target refusal depends on its precedence and default.
4. **WR3** (`arguments`). A fixed `vec!` per arm. The proof is easy once `Vec<String>` contents
   are modelled (gap 1) and `disp` is opaque. It also lets the companion invariant in WR4 be proved.
5. **WR6** (`Display for Outcome`). Needs the `Formatter`/`write!` model (gaps 2 and 8) on top of
   string concatenation. Prove the payload-preservation suffix property first. The full string
   equality follows from the same model.
6. **WR1** (`sdk_versions_newest_first`). The hardest: closures, a generic `IntoIterator`,
   `collect` into `Result`, a stable sort and a sorted-permutation postcondition (gaps 5, 7, 11, 13).
   It retires 2 tests, the most of any spec here, but it should wait until gaps 7 and 13 have
   been closed on a crate that needs them more.
