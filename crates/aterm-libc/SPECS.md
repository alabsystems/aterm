<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-libc: spec recovery

> Status 2026-10-16: the 3 specs below are the crate's migration worklist. Each one is proved
> through Trust/Clean against the compiled Rust function, and then the tests it subsumes are
> deleted. Two of the three (LIBC-2, LIBC-3) need their subject code moved out of the test file
> first (§4).

> Status 2026-10-17: the tests-and-gates audit deleted the crate's only test file (fd110c6c94),
> with its 4 tests and the test-only helpers they sampled. LIBC-2 and LIBC-3 were specs of those
> helpers and went with them, and so did §5's five items, which were all about that file. LIBC-1
> is the one spec left. Nothing here is proved yet.

Source: `crates/aterm-libc` (read only). The lib has `test = false` and no `#[test]` in `src/`, and
the crate has no integration tests now. Nothing was compiled or run. The claims below come from
reading the code.

Result: **1 spec**, covering **no test**. A cell the gate does not admit stops at its
`compile_error!` when that cell is built (`targo --unverified verify cells` builds every cell but
this machine's own), and atpkg's `every_published_triple_is_a_forge_cell` holds every shipped
triple to a cell.

## 0. Notation used in the statements

- `G`: the cell list in the `#[cfg(not(any(...)))]` that guards the no-cell `compile_error!` in
  `src/lib.rs`, written `(os, env, arch)`:
  `{(macos,*,aarch64), (macos,*,x86_64), (linux,gnu,x86_64), (linux,gnu,aarch64),
  (windows,msvc,x86_64), (windows,msvc,aarch64), (windows,gnu,x86_64), (unknown,*,wasm32)}`.
  `*` means the row names no `target_env`. `G` is a cfg predicate, not a function, so no proof
  attaches to it directly.
- `facts(t)` for a triple `t`: its `(arch, os, env)` cfg values, spelled as rustc spells them. Split
  `t` on `-`: `[a,o]` and `[a,_v,o]` give `(a,o,"")`, `[a,_v,o,e]` gives `(a,o,e)`, any other field
  count gives `None`; then os `darwin` becomes `macos`.
- `Admit(G, f)`: some row of `G` equals `f` on every key it names.

## 1. Specs (TSV)

`difficulty` is the Rust features a proof must handle; `blocking_gaps` are rows of
`docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
LIBC-1	contract	aterm_forge::resolve::default_cells	ensures: ∀ c ∈ default_cells(): facts(c.triple) = Some(f) ∧ Admit(G, f). Every cell `targo --unverified verify cells` compiles decomposes into target_* cfgs and passes the aterm-libc target gate. This is a property of a compiled production function. The contract is stated in aterm-forge and cites the constant table G from aterm-libc. Today it holds only by compilation: a cell the gate does not admit stops at its compile_error! when that cell is built (`verify cells` builds every cell but this machine's own).	-	MEDIUM: Vec<Cell> return value with String fields, a universal quantifier over the result, string split/compare inside facts(), a constant table cited from another crate	0, 1, 5, 12
```

## 2. Test → spec map

None. The crate has no tests.

## 6. Proof order

LIBC-1 needs gaps 0, 1 (string, `Vec` and `Option` content models) and 5 (quantified clauses).
`default_cells()` builds a constant `Vec`, so once gap 1 lands the quantifier unrolls over a fixed
list and the goal is decided by evaluation. Gap 12 (cross-crate citation) is needed only to cite
`G` from aterm-libc; stating `G` inline in aterm-forge avoids it.
