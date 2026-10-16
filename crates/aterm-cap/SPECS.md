<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-cap: spec recovery from its 7 unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each of the 4 specs below is
> proved through Trust/Clean about the compiled Rust function (for the mint seal, about the
> compiled call graph and feature graph). Then the tests it subsumes are deleted. The 2 tests
> in §4 sample no spec and can be deleted now.

Source: `crates/aterm-cap` (read only). I read every `#[test]`: `src/lib.rs` has 4 in
`mod tests`, and `tests/mint_reachability.rs` has 3 source-scan tests, 7 in all. I also read
the code each one reaches and the mint call sites the scan inspects. Nothing was compiled or
run. The claims below come from reading the code.

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
- `MintModel` is `aterm_spec::derive::mint_reachability_model` (in `models_core.rs`), with
  invariant `NoUntrustedTop` and its negative control `Buggy = 1`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `rust_features` (what the proof
must handle), `blocking_gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	rust_features	blocking_gaps
cap-grant-tier	contract	Authority::grant, Cap::tier	∀ E, a: &Authority, t: Tier: tierOf(a.grant::<E>(t)) = t, and ∀ c: &Cap<E>: c.tier() = tierOf(*c). Hence a.grant::<E>(t).tier() = t. Both are total: no precondition, no panic.	src/lib.rs::tests::granted_cap_carries_its_tier (the tier() assert)	A generic phantom E (PhantomData<fn() -> E>). A struct carrier with private fields and a ZST Seal. An enum Tier value. An &self view. Straight-line code.	0, 1, 2, 11
cap-satisfies-is-tier-order	table-property	Cap::satisfies, <Tier as PartialOrd>::ge (derived)	∀ E, c: &Cap<E>, min: Tier: c.satisfies(min) ⇔ rank(tierOf(c)) ≥ rank(min). As a 3×3 table, true exactly at (U,U), (T,U), (T,T), (C,U), (C,T), (C,C). Corollaries: monotone in the cap's tier, antitone in min; a Certified cap satisfies every minimum; every cap satisfies Untrusted.	src/lib.rs::tests::granted_cap_carries_its_tier (the three satisfies asserts)	A call to the derived PartialOrd on a fieldless enum (a discriminant compare, or a match after derive expansion). An &self view. A generic E.	0, 1, 2, 3, 11
require-gate	contract	require, Cap::satisfies, Cap::tier	∀ E, cap: &Cap<E>, min: Tier: require(cap, min) = if rank(tierOf(cap)) ≥ rank(min) then Ok(()) else Err(Denied { have: tierOf(cap), need: min }). Corollaries: require(cap, min).is_ok() ⇔ cap.satisfies(min); on Err(d), d.need = min and rank(d.have) < rank(d.need). Total; never panics. The proof composes cap-satisfies-is-tier-order and cap-grant-tier at the call sites.	src/lib.rs::tests::require_gates_on_tier	A Result<(), Denied> carrier (an enum over a struct of two enums). Calls to admitted fns (satisfies, tier) and an if on a bool. A generic fn. An & reference parameter.	0, 1, 2, 3, 4, 11
mint-launcher-only	refinement	Authority::root_authority, Authority's constructors, the workspace call graph and resolver-2 feature graph for aterm-cap/launcher-mint	Binds MintModel (NoUntrustedTop at Buggy = 0) to the tree. (a) Authority has no constructor other than Mint: its private _seal field rules out a struct literal elsewhere, and the only expression of type Authority in aterm-cap is Mint's body. (b) ∀ workspace crate k ∉ Launchers: ProdCalls(k) = ∅. (c) In the resolver-2 feature graph, the only production (non-dev) edges that enable aterm-cap/launcher-mint come from Launchers. This counts [dependencies], [target.*.dependencies], dotted [dependencies.aterm-cap] tables, renamed packages, [features] entries and workspace-inherited features. (d) Mint stays unsafe, so every call site is an audited unsafe block. NoUntrustedTop follows from (a) and (b). Feature unification compiles aterm-cap with launcher-mint for every crate in a launcher's build graph, so (c) alone does not stop a non-launcher crate from naming Mint there (§5.1). The model's stronger claim also needs (e): no production call path from a non-launcher crate reaches a call of Mint, including transitively through a launcher library's API (§5.2). Keep the ty_model! conformance test and its negative control until this row is Certified.	tests/mint_reachability.rs::root_authority_stays_feature_sealed_and_hidden, tests/mint_reachability.rs::only_launchers_enable_launcher_mint_in_production, tests/mint_reachability.rs::no_production_code_outside_launchers_reaches_the_mint	Not a property of one function body. cfg and feature semantics plus the resolver-2 feature graph (from cargo metadata). Field privacy. A cross-crate MIR call graph under the production cfg for (b) and (e). The model-to-code refinement surface. The current tests read files (fs, String, Vec); the spec should not. Until Trust can state it, the honest interim home is an aterm-census guard (verify guards) over cargo metadata resolved features and MIR call edges, plus a compile-fail negative control.	0, 8, 10, 12
```

### Distribution by kind (4)

| kind | count | ids |
|---|---|---|
| contract | 2 | cap-grant-tier, require-gate |
| table-property | 1 | cap-satisfies-is-tier-order |
| refinement | 1 | mint-launcher-only |

### Proof dependency order

`cap-grant-tier` and `cap-satisfies-is-tier-order` come first and do not depend on each other.
`require-gate` composes both. `mint-launcher-only` is independent of all three and binds an
existing `ty_model!`.

## 2. Test → spec map (TSV; all 7)

`primary` is the spec that retires the test. `also` lists the other specs the same test
samples. A test is deleted only when every spec it samples is Certified.

```tsv
file	test	primary	also
src/lib.rs	granted_cap_carries_its_tier	cap-satisfies-is-tier-order	cap-grant-tier
src/lib.rs	require_gates_on_tier	require-gate	cap-grant-tier, cap-satisfies-is-tier-order
src/lib.rs	effect_gated_operation	NON-SPEC (duplicate via test-only code)	require-gate
src/lib.rs	caps_are_copyable_witnesses	NON-SPEC (tautology)	
tests/mint_reachability.rs	root_authority_stays_feature_sealed_and_hidden	mint-launcher-only	
tests/mint_reachability.rs	only_launchers_enable_launcher_mint_in_production	mint-launcher-only	
tests/mint_reachability.rs	no_production_code_outside_launchers_reaches_the_mint	mint-launcher-only	
```

## 3. Coverage

- 7 original tests read. 5 map to one of the 4 specs. 2 are not spec samples (§4).
- `mint-launcher-only` retires all 3 integration tests. The `ty_model!` conformance test in
  `aterm-spec` stays until the binding is Certified.
- The 3 function-level specs are small (straight-line, no loops). Together they retire the 2
  `lib.rs` tests that are spec samples.

## 4. Tests that are not spec samples

> Status 2026-10-16: `effect_gated_operation` is deleted. It was a duplicate sample of
> `require-gate` through test-only code; the (Trusted, Trusted) Ok case is covered by the
> production gate tests in `aterm-pty`. `caps_are_copyable_witnesses` is still present.

| test | why | action |
|---|---|---|
| src/lib.rs::tests::caps_are_copyable_witnesses | `let d = c;` followed by a use of `c` compiles only if `Cap<E>: Copy`, so the type system already guarantees what it checks. A Copy is bitwise, so equal tiers is trivially true, and the hand-written `Clone` (`*self`) is never called. It is a tautology. | **delete** |
| src/lib.rs::tests::effect_gated_operation | It exercises `privileged_spawn`, a helper local to the `#[cfg(test)]` module with no production caller, which does only `require(cap, Trusted)?; Ok(..)`. Its two asserts are `require-gate` at (Trusted, Trusted) and (Untrusted, Trusted), which duplicate `require_gates_on_tier`. The `?` propagation it adds is plain caller composition (gap 4), not a property of aterm-cap. | **delete** |

Count by action: delete 2.

## 5. Implied specs that look wrong, or disagree with the code

1. **The sealing docs overclaim.** The crate docs, the `# Sealing` section of `root_authority`
   and the `launcher-mint` comment in `Cargo.toml` say that outside the launchers the symbol
   does not exist and cannot be named. Cargo feature unification makes that false whenever a
   crate is built in `aterm-gui`'s or `aterm-cli`'s graph: both enable `launcher-mint` in
   `[dependencies]`, so `aterm-cap` is compiled with the mint for every crate in that build.
   `aterm-pty`, `aterm-sandbox` and `aterm-buffer` also enable it on their dev-dependency edge
   (harmless under resolver 2 for their normal builds). Only the call-site scan enforces the real
   property. Restate the prose as "no non-launcher production call site", clause (b).
2. **`no_production_code_outside_launchers_reaches_the_mint`: prose and check disagree, and both
   fall short of the model.** The doc says only a launcher `main.rs` may mint, and
   `models_core.rs` says the constructor is named in exactly one product location. The check
   instead exempts every file of `aterm-cli` and `aterm-gui`. The real production mints are in
   libraries: `aterm-cli/src/lib.rs` (line 3024) and `aterm-gui/src/{lib,document_store,
   keeper_link,handoff_rendezvous}.rs`. The shipped `aterm` crate depends on both libraries. The scan
   checks only who names the symbol, not who can reach a call of it through a launcher's library
   API, which is what `NoUntrustedTop` claims. That is clause (e).
3. **The same scan is unsound as a binding of `NoUntrustedTop`.**
   - It sets `in_test = true` at the first line equal to `#[cfg(test)]` and never clears it, so
     everything after that line counts as test code. `aterm-cli/src/lib.rs` has `#[cfg(test)]`
     at line 289, before its real production mint at line 3024. Any production call placed after
     any `#[cfg(test)]` item, which most files have, is missed.
   - A call written without the literal `Authority::root_authority(` text is missed, for example
     `use aterm_cap::Authority as A; A::root_authority()`.
   - It scans only `crates/*/src/**`, so a mint in `build.rs`, `examples/` or `benches/` is not
     inspected.
   The MIR call-edge statement in clause (b) has none of these holes.
4. **`only_launchers_enable_launcher_mint_in_production` parses TOML by line prefix.** It
   matches only lines that start with `aterm-cap` and contain `launcher-mint`, and only in
   `crates/*/Cargo.toml` one level deep. It misses a dotted `[dependencies.aterm-cap]` table with
   a separate `features = [...]` line, a multi-line inline table, a renamed dependency
   (`cap = { package = "aterm-cap", ... }`), a `[features]` entry such as
   `foo = ["aterm-cap/launcher-mint"]`, and a `[workspace.dependencies]` entry carrying the
   feature. Clause (c) should be read from `cargo metadata`'s resolved features.
5. **`root_authority_stays_feature_sealed_and_hidden` pins prose.** It looks for the attribute
   text inside a 400-byte window before the fn, so a longer doc comment or reordered attributes
   break it while the seal is intact, and a weaker `cfg` spelled differently could pass.
   `#[doc(hidden)]` has no effect on reachability. The semantic content (the symbol is absent
   without `test` or `launcher-mint`) belongs in a compile-fail negative control, not a substring
   search.

## 6. Proof order (easiest, high value first)

No spec here is blocked by gap 0 alone. Every row also needs the non-scalar carriers (gap 1)
or the effects and refinement surfaces (gaps 8, 10). The order runs from the smallest gap set up.

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
4. **`mint-launcher-only`** (gaps 0, 8, 10, 12). It has the highest security value, but it is not
   a function-body proof. It waits on the model-to-code refinement surface and a workspace
   call-graph query. Meanwhile, move it to an `aterm-census` guard over `cargo metadata` and MIR
   call edges with a compile-fail negative control, which fixes §5.3 and §5.4. Do not delete the
   three tests until the binding is Certified.

Delete now, with no proof needed: `caps_are_copyable_witnesses` and `effect_gated_operation` (§4).
