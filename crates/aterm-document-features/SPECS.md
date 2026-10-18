<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-document-features: spec recovery from its 1 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 1 test it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-16: this is the crate's migration worklist. The one spec below is to be
> proved through Trust/Clean, and then the test it subsumes is deleted. No test has been
> deleted yet. The spec is blocked by gaps 0, 1, 3, 8 and 9 (§6), and the crate retires
> with wgpu, so the recommended action is to defer.

Source: `crates/aterm-document-features` (read only). The directory is
`aterm-document-features`, the package is `document-features` and the lib is
`document_features`, with `proc-macro = true`. I read the one `#[test]`
(`tests/consumer_forms.rs` 1) and the code it reaches: `src/lib.rs`, one `#[proc_macro]` fn.
Nothing was compiled or run. The claims below come from reading the code.

Result: **1 spec**, covering **1 test**. **0 tests are not spec samples** (§4).

Gap numbers refer to the table in §7 of
`AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit).

## 0. Notation used in the statements

- `Expand(args)` = the `TokenStream` that `document_features(args)` returns. That is the
  `#[proc_macro]` fn in `crates/aterm-document-features/src/lib.rs`, on input `args`.
- `EmptyStrLit` = `TokenStream::from(TokenTree::Literal(Literal::string("")))`. A stream `ts`
  equals `EmptyStrLit` when it has exactly one `TokenTree`, that tree is `TokenTree::Literal(l)`,
  and `l` is the plain (neither raw nor byte) string literal `""`, whose token text is `"\"\""`.
- `TokenStream`, `TokenTree` and `Literal` are `proc_macro` bridge handles. Their contents can be
  observed only through the compiler's client/server bridge, inside a macro expansion. The spec is
  therefore stated over the bridge's observable token-tree view: iterate the stream and compare
  each tree's `to_string()`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty (Rust features the proof
must handle)`, `blocking_gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
docfeat-expands-to-empty-str	contract	document_features::document_features (src/lib.rs)	∀ args: TokenStream (the empty stream, upstream's `feature_label = "..."` form and any malformed token sequence included): Expand(args) returns normally (no panic, no compile_error!) ∧ Expand(args) = EmptyStrLit. The result does not depend on args. Corollary: every call site `document_features::document_features!(..)` expands to a &'static str constant equal to "". That covers `const X: &str = document_features!(..)` and the inner-attribute form `#![doc = document_features!()]` that wgpu and wgpu-core use, which adds an empty doc string.	-	The fn runs inside rustc as a proc-macro dylib. TokenStream, TokenTree and Literal are opaque, handle-based non-scalar carriers, and TokenTree is an enum (gap 1). Literal::string and From<TokenTree> for TokenStream are calls across the proc_macro client/server bridge, an RPC to rustc that needs an assumption channel and content models (gaps 3, 8). The proc-macro crate must be admitted on strict (gap 9). The argument is ignored: no loop, no &mut, and no allocation reasoning beyond the bridge.	0, 1, 3, 8, 9
```

### Distribution by kind (1)

| kind | count |
|---|---|
| contract | 1 |

### Proof dependency order

There is one spec and it has no dependencies inside the crate. It depends on bridge models
(`Literal::string`, `From<TokenTree> for TokenStream`) that any proc-macro crate's specs would
share.

## 2. Test → spec map (TSV; all 1)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
```

## 3. Coverage

- 1 test: 1 is retired by the spec, and 0 are not spec samples.
- The test samples the spec at 2 points of an unbounded input space: no arguments, and upstream's
  `feature_label = "**`{feature}`**"` form.
- **Not a `#[test]`, and not covered by the spec.** `tests/consumer_forms.rs` also holds
  `mod consumer { #![doc = document_features::document_features!()] }`. It is a compile-only
  witness that the shim builds as a proc macro and that rustc accepts the inner-attribute form
  wgpu and wgpu-core use (§5.2).

## 4. Tests that are not spec samples

None. Actions by kind: delete 0, keep 0, rewrite 0.

## 5. Implied specs that look wrong, or disagree with the code

1. **The implied spec matches the code, but the test samples it thinly.** `document_features`
   returns `TokenTree::Literal(Literal::string("")).into()` and ignores `_args`. That agrees with
   the crate documentation's "Not equivalent" list, which says the shim accepts any input and
   never emits `compile_error!`. The test checks only 2 inputs, and no malformed-argument case is
   tested, so the "accepts any input" claim rests on reading the code. The spec states it for
   every `args`.
2. **The compile witness must survive the test's deletion.** A Trust contract on
   `document_features` cannot state that rustc accepts the `#![doc = ..]` form, because the macro
   runs only inside rustc's expansion. When the `#[test]` is deleted, keep `mod consumer`, or move
   the form into a doc comment in `src/lib.rs`. Do not delete the whole file.
3. **A proof may cost more than it is worth.** The crate is a 40-line shim that retires with wgpu
   (the gpu-wgpu unit of `docs/AUDIT-third-party-2026-10-15.md`). Its one spec is blocked on the
   proc-macro boundary and the bridge (gaps 1, 8 and 9). Keeping the test until wgpu leaves, and
   deleting it together with the crate, is cheaper than widening Trust for this shim alone.

## 6. Proof order

No spec here is blocked only by gap 0.

1. **`docfeat-expands-to-empty-str`** (gaps 0, 1, 3, 8, 9). The body is straight-line code with
   no loops, but every value in it is an opaque bridge handle. The proof needs four things.
   Gap 9: the proc-macro crate must be admitted on strict. Gaps 1 and 8: `Literal::string` and
   `TokenStream::from` need content models, an ensures over an opaque `TokenStream` handle, and an
   assumption channel for the bridge. Gap 3: the body calls those admitted fns. Gap 0 blocks every
   proof. **Recommendation:** defer. Leave the test in place until the crate retires with wgpu, or
   until gap 9 lands for a proc-macro crate with more tests behind it. Then prove this spec as the
   smallest proc-macro instance of that gap.
