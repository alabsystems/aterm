<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-scene: spec recovery from its 7 unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each of the 11 specs below is
> proved through Trust/Clean about the compiled Rust function, and then the tests it
> subsumes are deleted. Every test in this crate samples a spec (§4), so no test is
> deleted ahead of its proof.

Source: `crates/aterm-scene` (read only). I read every `#[test]`. `src/lib.rs` has 1
(`clamp_and_smoothstep_are_sane`) and `src/vector.rs` has 6, for 7 in all. I also read the
code those tests reach: `clampf`, `smoothstep`, `fill_path`, `fill_scanline`,
`fill_path_fixed`, `add_span`, `BBox::{empty,add,pixel_range}`, `sanitize`, `flatten_path`,
`flatten_cubic`, `parse_path`, and in `src/atlas.rs`, `Tile::{new,over,over_run,pixels}`.
Nothing was compiled or run, so every claim below comes from reading the code.

Result: **11 specs**, covering **7 of 7 tests**. **0 tests are not spec samples** (§4).
Three specs subsume no test: `flatten-cubic-contract`, `over-run-is-over-fold` and
`parse-path-roundtrip`. The first two are lemmas that the fill refinement needs. The third is
the property that `parse_path_roundtrips_the_asset_grammar` names but does not check (§5.2).

## 0. Notation used in the statements

**Floats.** Every spec here is over IEEE-754 binary32 with round-to-nearest-even. Trust's E6
bodies refuse floats today, and audit §7 has no float row. This file counts floats under
**gap 3** (body widening). A Clean `f32` model, not reals, is a prerequisite for every spec
below. **Recommendation:** add an "IEEE f32 carrier and Clean f32 model" row to audit §7.
It blocks every spec in this crate.

- `lattice(px,py)` = the 16 sample points `(px + (i+0.5)/4, py + (j+0.5)/4)` for
  `i,j ∈ 0..4`. This is the same lattice as `Tile::fill`.
- `E(paths,t)` = the ordered edge multiset that `flatten_path(paths[p], p, t, ..)` produces for
  each `p`. Edges are canonical: `ytop < ybot`, with `xtop` and `dxdy` in `f32` exactly as the code
  computes them. Horizontal edges are dropped, but their endpoints still enter `BBox`.
- `X_p(sy)` = the `f32` crossings `xtop + (sy − ytop)·dxdy` of the edges of path `p` with
  `ytop ≤ sy < ybot`, sorted by `total_cmp`.
- `Spans_p(sy)` = `[X_p[0],X_p[1]), [X_p[2],X_p[3]), …`. An odd trailing crossing is dropped.
- `Cov(E)(x,sy)` holds iff `∃p ∃span ∈ Spans_p(sy)` with `x ∈ span`. This is even-odd within a
  path and union across paths.
- `c(px,py)` = `#{s ∈ lattice(px,py) : Cov(E)(s)} ∈ 0..=16`.
- `RefFill(tile,paths,rgb,α,t)`: for every in-tile pixel, if `c > 0` then
  `tile.over(px,py,rgb,α·(c as f32)·(1/16))`, else leave the pixel unchanged. Pixels are
  independent, so the order does not matter. This is the naive reference rasterizer that the
  scanline core must refine.
- `lower(v: u16)` = `f32::from(v) · (1/4096)`. This is exact, because `1/4096` is a power of two and
  `v < 2^24`. `lowerP` maps `lower` over every `PathSeg` coordinate (`Move`/`Line`/`Cubic`/`Close`
  map 1:1 to `PathCmd`).
- `Render(cmds)` = one explicit letter per command: `M x y`, `L x y`, `C x1 y1 x2 y2 x y`, `Z`.
  Each number is printed with Rust's `f32` `Display` (shortest round-trip, no exponent), and tokens
  are separated by single spaces. It exists only on the spec side.
- `PathTok`: the tokenizer exactly as `parse_path` scans. Separators are ASCII whitespace and
  `,`. A letter token is any ASCII alphabetic byte. A number token is a maximal run of an optional
  leading sign followed by `[0-9 . e E]`, where a sign may also appear right after `e`/`E`. Its
  value is `str::parse::<f32>`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`,
`difficulty (Rust features the proof must handle)`, `blocking gaps (audit §7 numbers)`.
Gap 0 (an installed seal that publishes Lean-discharged rows) is a prerequisite for every row,
so it is listed on each one. No spec is blocked by gap 0 alone, because floats (gap 3) block
all of them.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
clampf-contract	contract	clampf	∀ v, lo, hi : f32, with r = clampf(v,lo,hi). (a) ¬v.is_nan() ∧ lo ≤ hi ⇒ lo ≤ r ≤ hi, where r = hi if v > hi, r = lo if v < lo, and r = v bitwise otherwise. (b) ¬v.is_nan() ∧ lo > hi ⇒ r = lo (the documented misuse fallback). (c) v.is_nan() ⇒ r.is_nan(), so callers must sanitize first (§5.4).	lib.rs::clamp_and_smoothstep_are_sane (clampf half)	EASY: f32 comparisons and if/else only; straight-line and panic-free. Needs an IEEE f32 carrier with a NaN case.	0,3,5,6
smoothstep-contract	contract	smoothstep, clampf	∀ t : f32 with ¬t.is_nan(): smoothstep(t) = smoothstep(clampf(t,0,1)); t ≤ 0 ⇒ smoothstep(t) = 0.0; t ≥ 1 ⇒ smoothstep(t) = 1.0 exactly; 0 ≤ smoothstep(t) ≤ 1. Monotone: ∀ t1 ≤ t2 (both non-NaN), smoothstep(t1) ≤ smoothstep(t2). Monotonicity under f32 rounding of t·t·(3−2t) must be proved over the f32 model, or dropped if it is false.	lib.rs::clamp_and_smoothstep_are_sane (smoothstep half)	EASY-MEDIUM: f32 mul/sub and a call to clampf (callee-ensures composition); straight-line. Monotonicity is the hard part.	0,3,4,5,6
over-run-is-over-fold	equivalence	atlas::Tile::over_run, atlas::Tile::over	∀ tile, x, y, n, rgb, a: over_run(x,y,n,rgb,a) has the same effect on tile.pixels() as over(x+k,y,rgb,a) for k = 0..n in order. The opaque fast path (sa ≥ 1, straight quad copy) is byte-identical to the general mix: oa = 1, so the mix reduces to the source exactly. The i64 clip of [x,x+n) to [0,w) never overflows and equals over's per-pixel bounds check. Requires removing trust::skip from Tile::over.	(none; lemma (v) of fill-scanline-refines-subsample-model)	MEDIUM: &mut Tile with the invariant px.len() = w·h·4, a bounded for loop, local closures, f32→u8 saturating casts, i32/i64/u32 casts, copy_from_slice.	0,1,2,3,5,7,13
flatten-cubic-contract	contract	vector::flatten_cubic	∀ finite p0..p3 with every coordinate in [−COORD_LIMIT, COORD_LIMIT]: flatten_cubic calls emit exactly n times, where n = 1 if err ≤ FLATTEN_TOL and n = clampf(ceil(sqrt(err/FLATTEN_TOL)),1,64) otherwise, so 1 ≤ n ≤ 64. The k-th call is (q_{k−1}, q_k), where q_0 = p0 and q_k is the f32 Bernstein evaluation at k/n. The chain closes exactly: q_n = p3 bitwise, because at t = 1 the weights are 0,0,0,1. All q_k are finite. This is a lemma for the subsample model and for implicit closure in flatten_path (cur = p3).	(none; lemma)	MEDIUM: bounded for loop 1..=n, a generic impl FnMut emit (higher-order spec), f32 sqrt/ceil/abs/max, f32→u32 cast.	0,3,5,7,11,13
parse-path-language	contract	vector::parse_path	∀ d: &str: parse_path(d) = Some(v) ⇔ PathTok(d) succeeds (every number token parses as f32), its token sequence matches (Letter Num*)* with every Letter ∈ {M,L,C,Z}, and no number precedes the first letter. Each M or L group has 2k numbers with k ≥ 1, each C group has 6k numbers with k ≥ 1, and each Z group has none. Then v is the concatenation of the group expansions: M gives Move(first pair) followed by a Line for each later pair; L gives a Line per pair; C gives a Cubic per 6-tuple; Z gives Close. Otherwise the result is None: unsupported letters, a bare M/L/C, a dangling argument count, a malformed number and numbers after Z all give None. "" gives Some([]). parse_path never panics: indices stay at UTF-8 char boundaries because tokens consume only ASCII, and every iteration either advances i or returns.	vector.rs::parse_path_roundtrips_the_asset_grammar	MEDIUM: a &str/as_bytes byte loop with a manual index, str::get(s..i), f32 FromStr (needs a parse model), Vec<f32>/Vec<PathCmd> push/clear, chunks_exact with slice patterns, an FnMut flush closure taking &mut Vec, Option<char> state, char classification.	0,1,3,5,7,13
fill-path-noop-guards	contract	vector::fill_path, fill_scanline, flatten_path, BBox::pixel_range	∀ tile, paths, rgb, α, t: if α ≤ 0 ∨ tile.width() = 0 ∨ tile.height() = 0 ∨ paths.is_empty() ∨ E(paths,t) is empty, then tile.pixels() is unchanged by fill_path. E(paths,t) is empty when every subpath is empty, a lone Move, a lone Close, or has only horizontal or degenerate segments. This is the c ≡ 0 case of fill-scanline-refines-subsample-model, stated on its own as an easier first proof.	vector.rs::degenerate_paths_are_safe	MEDIUM: &mut Tile, slices of slices, Vec<Edge> push through a closure, thread_local RefCell with_borrow_mut, for/while loops. Needs the loop invariant "no edge ⇒ no add_span call".	0,1,2,3,5,7,13
fill-path-total-and-bounded	no-panic	vector::fill_path, fill_scanline, add_span, sanitize, flatten_path, flatten_cubic, atlas::Tile::over, Tile::over_run	∀ tile, paths, rgb, α, t, with any f32 values (NaN, ±inf, ±f32::MAX and denormals included) in coordinates and transform fields: fill_path terminates without panic, and its work is bounded by O(Σ|path|·64·log + h·4·(|E|·log|E| + w)). Sub-lemmas: (i) sanitize(v) is finite with |sanitize(v)| ≤ COORD_LIMIT. (ii) Every crossing xtop + (sy−ytop)·dxdy at an active sub-scanline is finite: sy ≥ 0.125 and ytop ≤ sy < ybot bound ybot−ytop below by an ulp, so no NaN span reaches add_span. (iii) All usize/i32 index arithmetic in add_span and the flush pass stays within the lengths width+1 and width and within the tile. (iv) The FILL_SCRATCH RefCell is never borrowed twice, because nothing under fill_scanline re-enters fill_path. No pixel outside [0,w)×[0,h) is written.	vector.rs::out_of_range_coords_clamp (no-panic and in-tile half); vector.rs::degenerate_paths_are_safe (no-panic half)	HARD: panic-freedom over loops, Vec growth, a RefCell borrow, slice get/get_mut, f32→usize saturating casts, and termination measures for the while loops (next, i, j).	0,1,2,3,5,7,13
fill-path-fixed-refines-fill-path	refinement	vector::fill_path_fixed, fill_path	∀ tile, paths: &[&[PathSeg]], rgb, α, t: fill_path_fixed(tile,paths,rgb,α,t) has the same effect on tile as fill_path(tile, paths.map(|p| p.map(lowerP)), rgb, α, t). The per-path grouping is preserved: |refs| = |paths| and refs[i] = lowerP(paths[i]). Hence every fill_path spec (noop-guards, total-and-bounded, the refinement and the geometry) transfers to the fixed-point entry point. In particular, α ≤ 0 ∨ paths.is_empty() ∨ no path has an edge ⇒ tile unchanged. Lemma: lower(v) = v/4096 exactly, so q(k/8) round-trips bit-exactly.	vector.rs::fill_path_fixed_matches_float_frame	MEDIUM: Vec::with_capacity/extend with a mapping closure over an enum, slice get(off..end) with the offset loop invariant off = Σ prior lengths, u16→f32 conversion, callee composition into fill_path.	0,1,2,3,4,5,7,13
fill-scanline-refines-subsample-model	refinement	vector::fill_path, fill_scanline, add_span, flatten_path, flatten_cubic, BBox::pixel_range, atlas::Tile::over, Tile::over_run	∀ tile, paths, rgb, α > 0 (so α is not NaN), t, and ∀ prior contents and lengths of FILL_SCRATCH: the tile after fill_path(tile,paths,rgb,α,t) equals RefFill(tile,paths,rgb,α,t). Sub-lemmas: (i) The result does not depend on the scratch state, so two calls on equal inputs give byte-identical pixels. (ii) Clipping to the BBox is lossless: every covered sample lies in the pixel range [x0,x1)×[y0,y1). (iii) Active-edge invariant: after the activation loop at sub-scanline sy, active = {e ∈ E : e.ytop ≤ sy < e.ybot} as a multiset, because sy strictly increases over (py,sub). (iv) add_span adds, for each pixel, exactly #{lattice x-samples in [a,b)}, through diff (prefix sum ±4) plus extra; diff and extra are all zero at every row start. (v) over-run-is-over-fold, and α·16·(1/16) = α exactly, so the run branch and the partial branch both agree with RefFill. (vi) The union merge of sorted spans counts each sample at most once, so c ≤ 16 holds without relying on the clamp.	vector.rs::two_layer_asset_is_deterministic	VERY HARD: thread_local RefCell, &mut struct destructuring, six Vec scratch buffers (clear/resize/reserve/push/retain/sort_unstable_by with total_cmp), while-let over get(), nested for/while loops with loop invariants, Option match, closures (an FnMut push and a generic impl FnMut emit), f32 arithmetic and f32→usize/i32 casts, mem::take, &mut Tile with the px.len() = w·h·4 invariant. Tile::over is trust::skip today.	0,1,2,3,4,5,6,7,11,13
fill-path-coverage-geometry	other (geometric meaning)	vector::fill_path, flatten_path, atlas::Tile::over	For polygonal paths (Move/Line/Close only) under any transform t: ∀ sample s = (x,sy) whose distance to every device-space edge exceeds δ (the f32 crossing error bound, O(ulp(COORD_LIMIT))): Cov(E)(s) ⇔ ∃ path p such that a horizontal ray from s crosses p's implicitly closed subpaths an odd number of times. This is even-odd within a path regardless of winding, and union (never cancellation) across paths. Device geometry is clipped to the tile, so no pixel outside [0,w)×[0,h) is touched. Corollaries through RefFill and Tile::over: c(px,py) = 16 ∧ α ≥ 1 ⇒ the alpha byte is 255 whatever the prior contents; c = 0 ⇒ the pixel is unchanged.	vector.rs::even_odd_holes_work; vector.rs::out_of_range_coords_clamp (clip-to-edge half); vector.rs::two_layer_asset_is_deterministic (center-covered half)	HARD: builds on fill-scanline-refines-subsample-model (cited as a lemma) plus a real-geometry model of polygons and f32 error bounds; quantified over samples and paths; a Lean List/inductive model of the path.	0,1,4,5,6,14
parse-path-roundtrip	round-trip	vector::parse_path	∀ cmds: Vec<PathCmd> whose coordinates are all finite: parse_path(Render(cmds)) = Some(cmds), using PathCmd's PartialEq. This relies on f32 Display being shortest-round-trip with no exponent, and on "-0" parsing to −0.0. Conversely, ∀ d with parse_path(d) = Some(v): parse_path(Render(v)) = Some(v), so Render∘parse is idempotent on the accepted language.	(none; the property the asset-grammar test is named for, §5.2)	HARD: everything in parse-path-language, plus a model of f32 Display/FromStr round-trip (Ryu-style shortest representation) and String building in the spec-side Render.	0,1,3,5,7,13,14
```

## 2. Test → spec map (TSV; all 7)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
A test is deleted only when its primary spec and every spec under `also` are proved.

```tsv
file	test	primary	also
src/lib.rs	clamp_and_smoothstep_are_sane	smoothstep-contract	clampf-contract
src/vector.rs	two_layer_asset_is_deterministic	fill-scanline-refines-subsample-model	fill-path-coverage-geometry
src/vector.rs	even_odd_holes_work	fill-path-coverage-geometry	fill-scanline-refines-subsample-model
src/vector.rs	fill_path_fixed_matches_float_frame	fill-path-fixed-refines-fill-path	fill-path-noop-guards
src/vector.rs	degenerate_paths_are_safe	fill-path-noop-guards	fill-path-total-and-bounded
src/vector.rs	out_of_range_coords_clamp	fill-path-total-and-bounded	fill-path-coverage-geometry
src/vector.rs	parse_path_roundtrips_the_asset_grammar	parse-path-language	
```

## 3. Coverage

- 7 tests read. All 7 map to a spec, and none is a non-spec check.
- Two tests retire on cheap specs: `clamp_and_smoothstep_are_sane` (the `clampf` and
  `smoothstep` contracts) and `parse_path_roundtrips_the_asset_grammar` (`parse-path-language`).
- The four `fill_path` behaviour tests (`two_layer_asset_is_deterministic`, `even_odd_holes_work`,
  `out_of_range_coords_clamp` and the coverage half of `fill_path_fixed_matches_float_frame`) all
  sit behind `fill-scanline-refines-subsample-model`, so that proof is the crate's bottleneck.
  `fill-path-coverage-geometry` adds real geometry on top of it.

## 4. Tests that are not spec samples

None. Every test samples a general property in §1, so there is nothing to delete now and
nothing to convert into a different kind of check.

## 5. Implied specs that look wrong, or disagree with the code

1. **A mislabelled assertion.** In `vector.rs::parse_path_roundtrips_the_asset_grammar`,
   `parse_path("M x y").is_none()` is labelled "non-numeric", but it never reaches the
   number-parse failure (`tok.parse().ok()?`). `x` is ASCII alphabetic, so `flush('M', [])` runs
   first and returns false (a bare `M`), which gives `None`. A truly malformed number, such as
   `"M 1.2.3 4"` or `"M 1e 2"`, is never tested. `parse-path-language` covers both branches.
2. **The test name claims a round trip that the test does not check.**
   `parse_path_roundtrips_the_asset_grammar` only compares parse results with hand-written
   expected values. No `Render → parse` round trip is exercised. The real property is
   `parse-path-roundtrip`.
3. **`parse_path` has no production caller.** `aterm_scene::parse_path` (`src/vector.rs:618`)
   has no caller in the workspace outside tests. Its only user outside this crate is the
   integration test `crates/aterm-effects/tests/pet_art_quality.rs:129`. Prove its two specs only
   if the `pub` API is kept. Otherwise move the parser next to that test and drop both specs.
4. **The `clampf` doc overstates its guarantee, and NaN alpha erases pixels.** The doc in
   `src/lib.rs` says that when `lo ≤ hi` is violated the result "is still finite and bounded".
   That holds for `lo > hi`, but `clampf(NaN, lo, hi)` returns NaN because both comparisons are
   false. The consequence: `fill_path` with `α = NaN` passes the `alpha <= 0.0` guard, then
   `Tile::over`/`over_run` compute `oa = NaN` and write 0 to every channel, so covered pixels are
   erased to transparent black instead of being left alone. Either the fill specs must require
   `¬α.is_nan()` (as `fill-scanline-refines-subsample-model` does, through `α > 0`), or the guard
   should become `!(alpha > 0.0)`. The guard change is the better fix, because it makes the
   noop-guards spec total.
5. **"Determinism" within one call is a tautology.** In `two_layer_asset_is_deterministic`, the
   only content beyond `a == a` is independence from the thread-local `FILL_SCRATCH` left by the
   previous call, plus one coverage sample. Both are captured by sub-lemma (i) of
   `fill-scanline-refines-subsample-model` and by `fill-path-coverage-geometry`. The test does not
   vary the prior scratch state on purpose (for example by filling a larger tile first), so it
   samples the scratch-independence claim only weakly.
6. **`Tile::over` is `trust::skip`.** Every fill spec composes through it, so the skip must be
   removed (and `over` given its own contract) before `over-run-is-over-fold` or any fill refinement
   can be proved.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone, because every spec needs the `f32` model (gap 3, §0).
The order below therefore starts with the specs whose only extra gaps are the narrowest ones.

1. **`clampf-contract`** (gaps 0, 3, 5, 6). Two comparisons, straight-line, no loops. It is the
   first proof that exercises the `f32` carrier with a NaN case, which every other spec here needs.
   It also fixes the doc in §5.4. Together with step 2 it retires `clamp_and_smoothstep_are_sane`.
2. **`smoothstep-contract`** (adds gap 4). Straight-line, and it composes `clampf`'s ensures. The
   endpoint and range clauses are easy. If monotonicity fails under rounding, drop that clause; the
   test does not sample it.
3. **`parse-path-language`** (gaps 0, 1, 3, 5, 7, 13). Self-contained, with no `Tile` and no thread
   local. It retires `parse_path_roundtrips_the_asset_grammar`. Do this only after the decision in
   §5.3; if `parse_path` moves next to its one test, skip it.
4. **`fill-path-noop-guards`** (gaps 0, 1, 2, 3, 5, 7, 13). It retires `degenerate_paths_are_safe`
   once step 6 is done, and it is the first proof over `&mut Tile` and the scratch `RefCell`.
5. **`over-run-is-over-fold`** and **`flatten-cubic-contract`**. These are bounded-loop lemmas with
   no test of their own, but the main refinement cites both. `over-run-is-over-fold` first needs
   `trust::skip` removed from `Tile::over` (§5.6).
6. **`fill-path-total-and-bounded`**. Panic-freedom and termination across the whole rasterizer.
   It completes `degenerate_paths_are_safe` and half of `out_of_range_coords_clamp`.
7. **`fill-scanline-refines-subsample-model`** (gaps 0–7, 11, 13). This is the bottleneck: it
   retires `two_layer_asset_is_deterministic`, and steps 8 and 9 depend on it.
8. **`fill-path-fixed-refines-fill-path`**. A thin wrapper once step 7 is done. It retires
   `fill_path_fixed_matches_float_frame`.
9. **`fill-path-coverage-geometry`** (adds gap 14 and a real-geometry model). It retires
   `even_odd_holes_work` and the rest of `out_of_range_coords_clamp`.
10. **`parse-path-roundtrip`**. Last, because it needs an `f32` `Display`/`FromStr` model and retires
    no test on its own.
