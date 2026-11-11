<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-cap: spec recovery from its unit tests

The specs are read from `src/lib.rs` and the mint's call sites. A spec proved through
Trust/Clean about the compiled function (for `mint-launcher-only`, about the compiled call graph
and feature graph) retires the tests §2 maps to it. Gap numbers are the rows of
`docs/TRUST-LANE.md` (Blocking gaps).

## 0. Notation used in the statements

- `rank` is `Tier::rank`: `Untrusted` 0, `Trusted` 1, `Certified` 2. `Cap::satisfies` compares
  ranks (`self.tier.rank() >= min.rank()`), which agrees with the declaration-order `PartialOrd`
  derive on `Tier`.
- `tierOf(c)` is the private field `c.tier` of `c: Cap<E>`, which also carries
  `PhantomData<fn() -> E>` and a private ZST `Seal`.
- `E` ranges over every effect marker type (for example `effects::Spawn`). It is phantom, so
  every statement is generic in `E`.
- `Mint` is `aterm_cap::Authority::root_authority`, an `unsafe fn` compiled only under
  `#[cfg(any(test, feature = "launcher-mint"))]`.
- `Launchers = {aterm-cli, aterm-gui}`.
- `ProdBuild(k)` is crate `k`'s non-test build for every target (lib, bins, `build.rs`,
  examples, benches): `cfg(test)` is off, and the workspace's `resolver = "2"` keeps
  dev-dependency features out.
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
cap-satisfies-is-tier-order	table-property	Cap::satisfies, Tier::rank	∀ E, c: &Cap<E>, min: Tier: c.satisfies(min) ⇔ rank(tierOf(c)) ≥ rank(min). As a 3×3 table, true exactly at (U,U), (T,U), (T,T), (C,U), (C,T), (C,C). Corollaries: monotone in the cap's tier, antitone in min; a Certified cap satisfies every minimum; every cap satisfies Untrusted.	src/lib.rs::tests::granted_cap_carries_its_tier (the rank and satisfies asserts)	A three-arm match on a fieldless enum in Tier::rank returning u8 literals and u8 >= comparison. An &self view. A generic E.	0, 1, 2, 11
require-gate	contract	require, Cap::satisfies, Cap::tier	∀ E, cap: &Cap<E>, min: Tier: require(cap, min) = if rank(tierOf(cap)) ≥ rank(min) then Ok(()) else Err(Denied { have: tierOf(cap), need: min }). Corollaries: require(cap, min).is_ok() ⇔ cap.satisfies(min); on Err(d), d.need = min and rank(d.have) < rank(d.need). Total; never panics. The proof composes cap-satisfies-is-tier-order and cap-grant-tier at the call sites.	src/lib.rs::tests::require_gates_on_tier	A Result<(), Denied> carrier (an enum over a struct of two enums). Calls to admitted fns (satisfies, tier) and an if on a bool. A generic fn. An & reference parameter.	0, 1, 2, 3, 4, 11
mint-launcher-only	invariant	Authority::root_authority, Authority's constructors, the workspace call graph and resolver-2 feature graph for aterm-cap/launcher-mint, Guard (aterm-census guards::mint::launcher_mint)	Establishes NoUntrustedTop on the tree. (a) Authority has no constructor other than Mint: its private _seal field rules out a struct literal elsewhere, and the only expression of type Authority in aterm-cap is Mint's body. (b) ∀ workspace crate k ∉ Launchers: ProdCalls(k) = ∅. (c) In the resolver-2 feature graph, the only production (non-dev) edges that enable aterm-cap/launcher-mint come from Launchers. This counts [dependencies], [target.*.dependencies], dotted [dependencies.aterm-cap] tables, renamed packages, [features] entries and workspace-inherited features. (d) Mint exists only under cfg(any(test, feature = "launcher-mint")) and is an unsafe fn: a crate whose build does not enable launcher-mint cannot name it, and every call site is an audited unsafe block. NoUntrustedTop follows from (a) and (b). Feature unification compiles aterm-cap with launcher-mint for every crate in a launcher's build graph, so (c) and (d) alone do not stop a non-launcher crate from naming Mint there (§5.1); in that build Guard checks (b) and (c) by source text today (§5.3 and §5.4 say where it falls short). The model's stronger claim also needs (e): no production call path from a non-launcher crate reaches a call of Mint, including transitively through a launcher library's API (§5.2).	-	Not a property of one function body. cfg and feature semantics plus the resolver-2 feature graph (from cargo metadata). Field privacy. A cross-crate MIR call graph under the production cfg for (b) and (e). Guard reads source text and manifests; the spec should not. Until Trust can state it, Guard is its interim home; cargo metadata resolved features and MIR call edges, plus a compile-fail negative control, would close §5.3 and §5.4.	0, 8, 12
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test
samples. A test is deleted only when every spec it samples is Certified.

```tsv
file	test	primary	also
src/lib.rs	granted_cap_carries_its_tier	cap-satisfies-is-tier-order	cap-grant-tier
src/lib.rs	require_gates_on_tier	require-gate	cap-grant-tier, cap-satisfies-is-tier-order
```

## 4. Tests that are not spec samples

None.

## 5. Implied specs that look wrong, or disagree with the code

1. **The cfg seals the mint only in a graph with no launcher.** `aterm-gui` and `aterm-cli`
   enable `launcher-mint` in `[dependencies]`, so cargo compiles `aterm-cap` with the mint for
   every crate in their build; other crates enable it only on a dev-dependency edge (harmless
   under resolver 2). `root_authority`'s `# Sealing` section and the `launcher-mint` comment in
   `Cargo.toml` say so, and name `Guard` as what holds clause (b) in the shipped build.
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
