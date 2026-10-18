<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-libc: spec recovery from its 4 unit tests

> Status 2026-10-16: the 3 specs below are the crate's migration worklist. Each one is proved
> through Trust/Clean against the compiled Rust function, and then the tests it subsumes are
> deleted. Two of the three (LIBC-2, LIBC-3) need their subject code moved out of the test file
> first (§4).

> Status 2026-10-17: the tests-and-gates audit deleted `tests/target_gate.rs` (fd110c6c94), the
> crate's only test file, with its 4 tests and the test-only helpers they sampled. LIBC-2 and
> LIBC-3 were specs of those helpers and went with them, and so did §5's five items, which were
> all about that file. LIBC-1 is the one spec left. Nothing here is proved yet.

Source: `crates/aterm-libc` (read only). The lib has `test = false` and no `#[test]` in `src/`, and
the crate has no integration tests now. Nothing was compiled or run. The claims below come from
reading the code.

Result: **1 spec**, covering **no test**. A cell the gate does not admit stops at its
`compile_error!` when that cell is built (`targo --unverified verify cells` builds each cell no
fleet machine hosts), and atpkg's `every_published_triple_is_a_forge_cell` holds every shipped
triple to a cell.

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
  row names.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty (Rust features the proof
must handle)`, `blocking gaps (audit §7 numbers)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
LIBC-1	contract	aterm_forge::resolve::default_cells	ensures: ∀ c ∈ default_cells(): facts(c.triple) = Some(f) ∧ Admit(G, f). Every cell `targo --unverified verify cells` compiles decomposes into target_* cfgs and passes the aterm-libc target gate. This is a property of a compiled production function. The contract is stated in aterm-forge and cites the constant table G from aterm-libc. Today it holds only by compilation: a cell the gate does not admit stops at its compile_error! when that cell is built (`verify cells` builds each cell no fleet machine hosts).	-	MEDIUM: Vec<Cell> return value with String fields, a universal quantifier over the result, string split/compare inside facts(), a constant table cited from another crate	0, 1, 5, 12
```

## 2. Test → spec map

None. The crate has no tests.

## 3. Coverage

- No test is left to read. LIBC-1 has a production subject and needs no code move.

## 4. Tests that are not spec samples

None.

## 5. Implied specs that look wrong, or disagree with the code

None open. The five items here were all about `tests/target_gate.rs`, which is deleted.

## 6. Proof order (easiest, high value first)

LIBC-1 is not blocked by gap 0 alone. It also needs gap 1 (string/Vec/Option content models) and
gap 5 (quantified clauses).

1. **LIBC-1** (gaps 0, 1, 5, 12). It has a production subject and can start without any code
   move. `default_cells()` is straight-line construction of a constant `Vec`, so once gap 1
   lands the quantifier unrolls over a fixed list and `facts`/`Admit` evaluate on literals. The
   goal is decidable by evaluation. Gap 12 is needed only to cite G from aterm-libc. Stating G
   inline in aterm-forge drops that until R3 lands.

Gap histogram (gap → specs blocked): 0 → 1, 1 → 1, 5 → 1, 12 → 1.
