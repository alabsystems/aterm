<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-cap: spec recovery from its 7 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming the 5 tests it
> deleted are gone, among them the 3 mint-reachability source scans (d6c146e948 deleted their file
> and holds the mint seal by type instead). §3 counts what is left; other counts were taken before.

> Status 2026-10-16: this is the crate's migration worklist. Each of the 4 specs below is
> proved through Trust/Clean about the compiled Rust function (for the mint seal, about the
> compiled call graph and feature graph). Then the tests it subsumes are deleted. The 2 tests
> in §4 sampled no spec and are deleted.

Source: `crates/aterm-cap` (read only). I read every `#[test]`: `src/lib.rs` had 4 in
`mod tests`, and the mint-reachability test file (since deleted) had 3 source-scan tests, 7 in all.
I also read the code each one reaches and the mint call sites the scan inspected. Nothing was
compiled or run. The claims below come from reading the code.

Result: **4 specs**, covering **5 tests**. **2 tests are not spec samples** (§4).

Gap numbers refer to the table in §7 of
`AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit).
Gap 0 (an installed seal that publishes Lean-discharged rows) is listed in every row because it
is a prerequisite for all of them. The plan's "Seal `150efe4a`" section describes a local build
that closes it, but the machine's `trust` channel has not been flipped to it.

## 0. Notation used in the statements

- `rank : Tier → Nat`, with `rank(Untrusted) = 0`, `rank(Trusted) = 1` and `rank(Certified) = 2`.
  The derived `PartialOrd`/`Ord` on `Tier` is declaration (discriminant) order, so
  `∀ a b: (a >= b) ⇔ rank(a) ≥ rank(b)`. A spec must state the derived order as this total
  order. It must not leave it as an opaque trait call.
- `tierOf(c)` is the private field `c.tier` of `c: Cap<E>`. `Cap<E>` also carries
  `PhantomData<fn() -> E>` and a private ZST `Seal`.
- `E` ranges over every effect marker type (for example `effects::Spawn`). `E` is phantom and
  does not affect behaviour, so every statement is generic in `E`.
- `Mint` is the item `aterm_cap::Authority::root_authority`. It is an `unsafe fn` compiled only
  under `#[cfg(any(test, feature = "launcher-mint"))]`.
- `Launchers = {aterm-cli, aterm-gui}`.
- `ProdBuild(k)` is crate `k`'s non-test build, for every target (lib, bins, `build.rs`,
  examples, benches). In it `cfg(test)` is off, and features are resolved by the workspace's
  `resolver = "2"`, so dev-dependency features do not reach normal builds.
- `ProdCalls(k)` is the set of MIR call edges to `Mint` in `ProdBuild(k)`.
- `MintModel` is the claim `NoUntrustedTop`: no crate outside `Launchers` obtains a root
  `Authority`. No derived model encodes it; its earlier one was unbound and was deleted.
- `Guard` is the aterm-census `launcher-mint` guard (`crates/aterm-census/src/guards/mint.rs`, run by
  `verify census`). Outside `Launchers` it reports every shipped non-test product `.rs` line (`src/`,
  `build.rs`, `examples/` and `benches/`) whose code names `root_authority`, and every shipped table
  of the root `Cargo.toml` or a `crates/<name>/Cargo.toml` that enables `aterm-cap/launcher-mint`
  (including dotted `[dependencies.*]` tables and renamed `package = "aterm-cap"` / path aliases).
  Test items, test files and dev-dependency edges are not read.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `rust_features` (what the proof
must handle), `blocking_gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	rust_features	blocking_gaps
cap-grant-tier	contract	Authority::grant, Cap::tier	∀ E, a: &Authority, t: Tier: tierOf(a.grant::<E>(t)) = t, and ∀ c: &Cap<E>: c.tier() = tierOf(*c). Hence a.grant::<E>(t).tier() = t. Both are total: no precondition, no panic.	src/lib.rs::tests::granted_cap_carries_its_tier (the tier() assert)	A generic phantom E (PhantomData<fn() -> E>). A struct carrier with private fields and a ZST Seal. An enum Tier value. An &self view. Straight-line code.	0, 1, 2, 11
cap-satisfies-is-tier-order	table-property	Cap::satisfies, <Tier as PartialOrd>::ge (derived)	∀ E, c: &Cap<E>, min: Tier: c.satisfies(min) ⇔ rank(tierOf(c)) ≥ rank(min). As a 3×3 table, true exactly at (U,U), (T,U), (T,T), (C,U), (C,T), (C,C). Corollaries: monotone in the cap's tier, antitone in min; a Certified cap satisfies every minimum; every cap satisfies Untrusted.	src/lib.rs::tests::granted_cap_carries_its_tier (the three satisfies asserts)	A call to the derived PartialOrd on a fieldless enum (a discriminant compare, or a match after derive expansion). An &self view. A generic E.	0, 1, 2, 3, 11
require-gate	contract	require, Cap::satisfies, Cap::tier	∀ E, cap: &Cap<E>, min: Tier: require(cap, min) = if rank(tierOf(cap)) ≥ rank(min) then Ok(()) else Err(Denied { have: tierOf(cap), need: min }). Corollaries: require(cap, min).is_ok() ⇔ cap.satisfies(min); on Err(d), d.need = min and rank(d.have) < rank(d.need). Total; never panics. The proof composes cap-satisfies-is-tier-order and cap-grant-tier at the call sites.	src/lib.rs::tests::require_gates_on_tier	A Result<(), Denied> carrier (an enum over a struct of two enums). Calls to admitted fns (satisfies, tier) and an if on a bool. A generic fn. An & reference parameter.	0, 1, 2, 3, 4, 11
mint-launcher-only	invariant	Authority::root_authority, Authority's constructors, the workspace call graph and resolver-2 feature graph for aterm-cap/launcher-mint, Guard (aterm-census guards::mint::launcher_mint)	Establishes NoUntrustedTop on the tree. (a) Authority has no constructor other than Mint: its private _seal field rules out a struct literal elsewhere, and the only expression of type Authority in aterm-cap is Mint's body. (b) ∀ workspace crate k ∉ Launchers: ProdCalls(k) = ∅. (c) In the resolver-2 feature graph, the only production (non-dev) edges that enable aterm-cap/launcher-mint come from Launchers. This counts [dependencies], [target.*.dependencies], dotted [dependencies.aterm-cap] tables, renamed packages, [features] entries and workspace-inherited features. (d) Mint exists only under cfg(any(test, feature = "launcher-mint")) and is an unsafe fn: a crate whose build does not enable launcher-mint cannot name it, and every call site is an audited unsafe block. NoUntrustedTop follows from (a) and (b). Feature unification compiles aterm-cap with launcher-mint for every crate in a launcher's build graph, so (c) and (d) alone do not stop a non-launcher crate from naming Mint there (§5.1); in that build Guard checks (b) and (c) by source text today (§5.3 and §5.4 say where it falls short). The model's stronger claim also needs (e): no production call path from a non-launcher crate reaches a call of Mint, including transitively through a launcher library's API (§5.2).	-	Not a property of one function body. cfg and feature semantics plus the resolver-2 feature graph (from cargo metadata). Field privacy. A cross-crate MIR call graph under the production cfg for (b) and (e). Guard reads source text and manifests; the spec should not. Until Trust can state it, Guard is its interim home; cargo metadata resolved features and MIR call edges, plus a compile-fail negative control, would close §5.3 and §5.4.	0, 8, 12
```

### Distribution by kind (4)

| kind | count | ids |
|---|---|---|
| contract | 2 | cap-grant-tier, require-gate |
| table-property | 1 | cap-satisfies-is-tier-order |
| invariant | 1 | mint-launcher-only |

### Proof dependency order

`cap-grant-tier` and `cap-satisfies-is-tier-order` come first and do not depend on each other.
`require-gate` composes both. `mint-launcher-only` is independent of all three.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test
samples. A test is deleted only when every spec it samples is Certified.

```tsv
file	test	primary	also
src/lib.rs	granted_cap_carries_its_tier	cap-satisfies-is-tier-order	cap-grant-tier
src/lib.rs	require_gates_on_tier	require-gate	cap-grant-tier, cap-satisfies-is-tier-order
```

## 3. Coverage

- 7 original tests read. 5 mapped to one of the 4 specs and 2 were not spec samples (§4). 2
  remain, both `lib.rs` spec samples: the 3 mint-reachability integration tests
  and both §4 tests are deleted.
- `mint-launcher-only` has no test in this crate. The type holds clauses (a) and (d) at compile
  time, and `Guard`, with its own tests in `crates/aterm-census/src/guards/mint.rs`, checks (b)
  and (c) in the shipped build.
- The 3 function-level specs are small (straight-line, no loops). Together they retire the 2
  `lib.rs` tests.

## 4. Tests that are not spec samples

> Status 2026-10-16: both tests are deleted. `effect_gated_operation` was a duplicate sample of
> `require-gate` through test-only code; the (Trusted, Trusted) Ok case is covered by the
> production gate tests in `aterm-pty`. The other checked only that `Cap` is `Copy`, which the
> type system already guarantees.

## 5. Implied specs that look wrong, or disagree with the code

1. **The cfg seals the mint only in a graph with no launcher.** `aterm-gui` and `aterm-cli`
   enable `launcher-mint` in `[dependencies]`, so cargo compiles `aterm-cap` with the mint for
   every crate in their build. `aterm-pty`, `aterm-sandbox` and `aterm-buffer` also enable it on
   their dev-dependency edge (harmless under resolver 2 for their normal builds). The crate docs,
   the `# Sealing` section of `root_authority` and the `launcher-mint` comment in `Cargo.toml` say
   so, and name `Guard` as what holds the shipped build: no non-launcher production call site,
   clause (b).
2. **`Guard` falls short of the model.** It exempts every file of `aterm-cli` and `aterm-gui`,
   and the production mints there are in library code (`aterm-cli/src/lib.rs`'s `session_main`
   among them), which the shipped `aterm` crate depends on. It checks only who names the symbol,
   not who can reach a call of it through a launcher's library API, which is what
   `NoUntrustedTop` claims. That is clause (e).
3. *(Partially resolved)* **`Guard` is a text check.** It closes the deleted scan's holes and now
   inspects every non-test `.rs` target under `crates/` (`src/`, `build.rs`, `examples/`, and
   `benches/`), masking only items whose `cfg` proves them test-only and matching the word
   `root_authority` across aliases of `Authority`. The MIR call-edge statement in clause (b) still
   replaces lexical scanning when Trust exposes workspace call graphs.
4. *(Partially resolved)* **`Guard` reads manifests as TOML.** It reads `[dependencies]`,
   `[dependencies.<name>]`, `[target.…]` and `[workspace.dependencies]` edges targeting `aterm-cap`
   (by key, `package = "aterm-cap"`, or `path`), and `[features]` items forwarding
   `<alias>/launcher-mint` or `<alias>?/launcher-mint`; dev and build edges are skipped. Clause (c)
   will still move to `cargo metadata`'s resolved features when proved.
5. **The seal has no negative control.** The cfg itself is the seal: without `test` or
   `launcher-mint` the item does not exist. `#[doc(hidden)]` has no effect on reachability.
   Nothing compiles a crate without the feature and checks that naming the mint fails; that
   belongs in a compile-fail negative control.

## 6. Proof order (easiest, high value first)

No spec here is blocked by gap 0 alone. Every row also needs the non-scalar carriers (gap 1)
or the effects and cross-crate surfaces (gaps 8, 12). The order runs from the smallest gap set up.

1. **`cap-grant-tier`** (gaps 0, 1, 2, 11). A struct constructor and a field read with no
   branches. Gap 11 can be sidestepped by proving the monomorphic instance `Cap<effects::Spawn>`
   first, because `E` is phantom and the body does not depend on it. This is the smallest test of
   an enum and struct carrier with an `&self` view in Trust.
2. **`cap-satisfies-is-tier-order`** (gaps 0, 1, 2, 3, 11). Same shape, plus the derived `ge`
   call. Gap 3 can be avoided by admitting derived `PartialOrd` on fieldless enums as a
   discriminant compare, or by writing the body as `self.tier as u8 >= min as u8`, which means
   the same thing. Being a 3×3 table, it is decidable by evaluation once stated. With item 1 it
   retires `granted_cap_carries_its_tier`.
3. **`require-gate`** (gaps 0, 1, 2, 3, 4, 11). It adds the `Result`/`Denied` carriers and
   callee-ensures composition. Once items 1 and 2 are Certified, it retires
   `require_gates_on_tier`.
4. **`mint-launcher-only`** (gaps 0, 8, 12). It has the highest security value, but it is not
   a function-body proof. The type holds (a) and (d) now; (b) and (c) wait on a workspace
   call-graph query and the resolver-2 feature graph, and (e) on reachability through a
   launcher's library API. Meanwhile `Guard` holds the shipped build by text. Moving it onto `cargo metadata`
   and MIR call edges with a compile-fail negative control fixes §5.3, §5.4 and §5.5.
