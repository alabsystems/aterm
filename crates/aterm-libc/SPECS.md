<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-libc: spec recovery from its 4 unit tests

> Status 2026-10-16: the 3 specs below are the crate's migration worklist. Each one is proved
> through Trust/Clean against the compiled Rust function, and then the tests it subsumes are
> deleted. Two of the three (LIBC-2, LIBC-3) need their subject code moved out of the test file
> first (§4).

Source: `crates/aterm-libc` (read only). The lib has `test = false` and no `#[test]` in `src/`. All
4 tests are in the integration test file `crates/aterm-libc/tests/target_gate.rs`. I read them and
the code they reach. Nothing was compiled or run. The claims below come from reading the code.

Result: **3 specs**, covering **4 tests**. Only 1 test (part of
`every_configured_build_lane_is_a_cell_this_crate_accepts`) samples a compiled production function
today. The other 3 sample helpers defined only in the test file, so they are not spec samples until
those helpers move into production code (§4).

## 0. Notation used in the statements

- `G`: the closed cell list in the `#[cfg(not(any(...)))]` that guards the no-cell `compile_error!`
  in `crates/aterm-libc/src/lib.rs`:
  `{(macos,*,aarch64), (macos,*,x86_64), (linux,gnu,x86_64), (linux,gnu,aarch64),
  (windows,msvc,x86_64), (windows,msvc,aarch64), (windows,gnu,x86_64), (unknown,*,wasm32)}`,
  written `(os, env, arch)`. `*` means the row names no `target_env`. `G` is a cfg predicate, not a
  Rust function, so no proof can be attached to it directly.
- `Facts = (arch, os, env)`: the `target_*` cfg values, spelled the way rustc spells them.
- `facts(t)` for a triple `t`: split `t` on `-`. `[a,o]` gives `(a,o,"")`, `[a,_v,o]` gives
  `(a,o,"")`, `[a,_v,o,e]` gives `(a,o,e)`, and any other field count gives `None`. Then os
  `darwin` becomes `macos`.
- `Admit(G, f)`: some row `r` in `G` has `r.k = f.k` for every key `k` in `{arch, os, env}` that the
  row names. A row the reader could not parse (the `UNPARSED` marker, `Cell::unreadable()`) admits
  nothing.
- `Lanes`: the `[target.<triple>]` tables of `.cargo/config.toml` (`cfg(...)` tables skipped), plus
  `{c.triple | c ∈ aterm_forge::resolve::default_cells()}`, plus the triple column of the `cshim`
  rows in `tools/cross-cell-gate.tsv` (today: one config table, `x86_64-pc-windows-gnu`, and five
  `cshim` rows).
- Test-only helpers: `facts_of`, `gate_cells`, `parse_cell`, `Cell::admits` and the three lane
  readers are all defined only in `crates/aterm-libc/tests/target_gate.rs`. "Moved to census" means
  moved into `aterm-census` as guard code (`verify guards`), where they become compiled production
  functions that a proof can target.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty (Rust features the proof
must handle)`, `blocking gaps (audit §7 numbers)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
LIBC-1	contract	aterm_forge::resolve::default_cells	ensures: ∀ c ∈ default_cells(): facts(c.triple) = Some(f) ∧ Admit(G, f). Every cell `xtask gate cells` compiles decomposes into target_* cfgs and passes the aterm-libc target gate. This is the only part of the lane-parity law that is a property of a compiled production function. The contract is stated in aterm-forge and cites the constant table G from aterm-libc. Today the test reaches it by scraping `triple: "` lines out of resolve.rs.	target_gate.rs::every_configured_build_lane_is_a_cell_this_crate_accepts (the default_cells part)	MEDIUM: Vec<Cell> return value with String fields, a universal quantifier over the result, string split/compare inside facts(), a constant table cited from another crate	0, 1, 5, 12
LIBC-2	contract	facts_of (test-only today; moved to census)	∀ t: &str: facts_of(t) = facts(t). Field counts 2, 3 and 4 decompose as stated, with the vendor dropped and darwin read as macos. Every other field count gives None. The test's table is seven samples of this: apple-darwin -> macos with empty env; linux-gnu -> gnu; windows msvc and gnu; wasm32-unknown-unknown -> os unknown with empty env.	target_gate.rs::a_triple_decomposes_the_way_rustc_spells_its_cfgs	EASY-MEDIUM: &str split into Vec<&str>, a slice-pattern match, Option of a three-String struct, to_string, string equality	0, 1, 3, 5, 7
LIBC-3	contract	Cell::admits, parse_cell, gate_cells (test-only today; moved to census)	(a) Cell::admits(c, f) ⇔ c.arch ≠ Some(UNPARSED) ∧ ∀ k ∈ {arch, os, env}: c.k = None ∨ c.k = Some(f.k). (b) parse_cell(s) yields the cell that names exactly the target_arch/target_os/target_env clauses of s, or Cell::unreadable() if any clause lacks `=` or uses another key. (c) gate_cells(src) returns one cell per all(...) arm of the #[cfg(not(any(...)))] immediately before the no-cell compile_error!, in source order, and panics if that shape is absent. Corollary for the committed G: 8 rows, none unreadable; refuses i686-unknown-linux-gnu and aarch64-pc-windows-gnu; admits x86_64-pc-windows-gnu.	target_gate.rs::the_law_can_say_no, target_gate.rs::the_scan_still_sees_the_gate (matcher half)	MEDIUM: Option<String> fields, a closure with is_none_or, a str find/slice loop over source text, Vec push, panic as the error path	0, 1, 3, 5, 7
```

## 2. Test → spec map (TSV; all 4)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
`*` marks a test whose primary spec retires it only after the move in §4.

```tsv
file	test	primary	also
tests/target_gate.rs	every_configured_build_lane_is_a_cell_this_crate_accepts	LIBC-1	LIBC-2, LIBC-3 (via facts_of, gate_cells, Cell::admits); the .cargo/config.toml and cross-cell-gate.tsv parts move to a census guard (§4)
tests/target_gate.rs	the_scan_still_sees_the_gate	LIBC-3*	LIBC-2 (lanes decompose); the reader-non-empty half moves with the census guard (§4)
tests/target_gate.rs	the_law_can_say_no	LIBC-3*	LIBC-2
tests/target_gate.rs	a_triple_decomposes_the_way_rustc_spells_its_cfgs	LIBC-2*	
```

## 3. Coverage

- 4 tests read. Each maps to a spec. Only LIBC-1 is about a compiled production function today.
- LIBC-2 and LIBC-3 have a subject only after `facts_of`, `parse_cell`, `Cell::admits` and
  `gate_cells` move into `aterm-census`. If that move is not made, their 3 tests test test-only code
  and are deleted, not proved.
- The repository-data half of the lane-parity law (`.cargo/config.toml`, `tools/cross-cell-gate.tsv`)
  is not a spec of any function. It belongs to a census guard.

## 4. Tests that are not spec samples

| test | why | action |
|---|---|---|
| target_gate.rs::every_configured_build_lane_is_a_cell_this_crate_accepts | Only its `default_cells` part is a property of production code (LIBC-1). The `.cargo/config.toml` and `tools/cross-cell-gate.tsv` parts check committed data against a cfg predicate. That is a repository-consistency rule no lint can name, and there is no compiled function to carry a proof. AGENTS.md puts such a rule in an `aterm-census` guard (`verify guards`). It overlaps `crates/atpkg/tests/shipped_triples_have_a_compile_cell.rs`, which holds shipped triples to G. | **move-to-bench** (to an `aterm-census` guard that absorbs both scrapers). Drop the forge scrape once LIBC-1 is proved. |
| target_gate.rs::the_scan_still_sees_the_gate | A self-check of test-only parsers: the gate scrape and the three lane readers are non-empty and decomposable. No production caller. Its matcher half becomes LIBC-3 after the move. Its reader-non-empty half is the census guard's own negative control and moves with the guard. | **becomes-spec-after-fix** (LIBC-3) |
| target_gate.rs::the_law_can_say_no | A negative control for the test-only matcher over the live gate. It is a sample of LIBC-3 only once `Cell::admits`/`gate_cells` are census production code. | **becomes-spec-after-fix** (LIBC-3) |
| target_gate.rs::a_triple_decomposes_the_way_rustc_spells_its_cfgs | Samples `facts_of`, which lives only in the test file. It becomes the universally quantified LIBC-2 once `facts_of` moves to census. If it does not move, delete the test as a test of test-only code. | **becomes-spec-after-fix** (LIBC-2) |

No test is a pure regression of a removed bug.

## 5. Implied specs that look wrong, or disagree with the code

1. **Stale module doc.** The module doc of `tests/target_gate.rs` says `x86_64-apple-darwin` and
   `aarch64-unknown-linux-gnu` are "a cell no lane names ... whose lanes live in release scripts".
   Both are now named by `aterm_forge::resolve::default_cells()` (mac-x64 and the ARM Linux cell) and
   by `cshim` rows in `tools/cross-cell-gate.tsv`.
2. **Stale threshold.** `the_scan_still_sees_the_gate` asserts `cells.len() >= 6`, but G has 8 rows.
   Losing up to two arms (for example both Windows gnu/aarch64 rows) passes silently. The real spec
   is equality with the parsed arm count (LIBC-3c), not a lower bound.
3. **Coincidence stated as a property.** The comment in `the_scan_still_sees_the_gate`, "every triple
   tools/cross-cell-gate.tsv names is also a forge cell", holds today for all five `cshim` triples
   (`x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`, `x86_64-apple-darwin`,
   `aarch64-unknown-linux-gnu`, `aarch64-pc-windows-msvc`), but nothing enforces it. (An earlier
   draft of this file said six; the TSV has five `cshim` rows.)
4. **Two scrapers police one table.** `every_configured_build_lane_is_a_cell_this_crate_accepts`
   duplicates the intent of `crates/atpkg/tests/shipped_triples_have_a_compile_cell.rs` (named in the
   lib.rs gate comment). AGENTS.md says to avoid this ("no registry plus a test that polices it").
   One census guard should replace both.
5. **The forge lane is read as text, not called.** `forge_cell_triples` in `tests/target_gate.rs`
   scrapes `crates/aterm-forge/src/resolve.rs` for lines starting `triple: "` instead of calling
   `default_cells()`. A triple built any other way (`format!`, a `const`, a helper) is invisible to
   the law. LIBC-1, stated as a contract on the function, removes that blind spot.

## 6. Proof order (easiest, high value first)

No spec is blocked by gap 0 alone. Every one also needs gap 1 (string/Vec/Option content models)
and gap 5 (quantified clauses), so the order below is by remaining gaps and by value.

1. **LIBC-1** (gaps 0, 1, 5, 12). It is the only spec with a production subject today, and it can
   start without any code move. `default_cells()` is straight-line construction of a constant
   `Vec`, so once gap 1 lands the quantifier unrolls over a fixed list and `facts`/`Admit` evaluate
   on literals. The goal is decidable by evaluation. Gap 12 is needed only to cite G from aterm-libc.
   Stating G inline in aterm-forge drops that until R3 lands. Proving it retires the forge scrape.
2. **LIBC-2** (gaps 0, 1, 3, 5, 7), after `facts_of` moves to census. It is a small total function
   over `&str` with a four-arm slice match. It is the lemma LIBC-1 and LIBC-3 both rest on.
3. **LIBC-3** (gaps 0, 1, 3, 5, 7), after the matcher moves to census. Part (a) is easy once gap 1
   lands. Parts (b) and (c) are loop-and-find text scanning (gap 7) and are the hardest work in this
   crate. Prove (a) first, because the `the_law_can_say_no` corollary needs only (a) plus the fixed G.

Gap histogram (gap → specs blocked): 0 → 3, 1 → 3, 3 → 2, 5 → 3, 7 → 2, 12 → 1.
