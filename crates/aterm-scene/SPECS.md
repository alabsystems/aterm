<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-scene: spec recovery from its 12 unit tests

> Status 2026-11-03: reconciled with current `main` (adding `mix-rgb-contract` and the four
> integer `motion` specs for MOT-1's `src/motion.rs`, and removing the deleted
> `scene_rgb_and_tile_rect_const_helpers` note from §4).

> Status 2026-10-16: this is the crate's migration worklist. Each of the 16 specs below is
> proved through Trust/Clean about the compiled Rust function, and then the tests it
> subsumes are deleted. Every test in this crate samples a spec (§4), so no test is
> deleted ahead of its proof.

Source: `crates/aterm-scene` (read only). Every `#[test]` is mapped: `src/lib.rs` has 1,
`src/atlas.rs` has 1, `src/motion.rs` has 5, and `src/vector.rs` has 5, for 12 in all. Code
reached: `clampf`, `smoothstep`, `mix_rgb`, `fill_path`, `fill_scanline`, `fill_path_fixed`,
`add_span`, `BBox::{empty,add,pixel_range}`, `sanitize`, `flatten_path`, `flatten_cubic`,
`parse_path`, `atlas::Tile::{new,over,over_run,pixels}`, and in `src/motion.rs`,
`ease_out_quad`, `ease_out_cubic`, `ease_in_out_cubic`, `frac`, `frac_fine`, `glide_span`,
`glide`, `Spring::{critical,underdamped,omega,zeta,at,settle_ms}`, and
`Cadence::{hz,per_second,frame_at,frame_start,next_frame}`.

Result: **16 specs**, covering **12 of 12 tests**. **0 tests are not spec samples** (§4).
Five specs serve as pure or structural lemmas with no primary test (`clampf-contract`,
`smoothstep-contract`, `flatten-cubic-contract`, `fill-scanline-refines-subsample-model`,
`parse-path-roundtrip`).

## 0. Notation used in the statements

The float rasterizer and RGB specs are over IEEE-754 binary32 with round-to-nearest-even (counted
under gap 3 until Clean adds an `f32` row); the `motion-*` specs are pure fixed-point/integer
contracts with no float dependency.

- `lattice(px,py)` = the 16 sample points `(px + (i+0.5)/4, py + (j+0.5)/4)` for `i,j ∈ 0..4`, the
  lattice `Tile::fill_quad` also uses.
- `E(paths,t)` = the ordered edge multiset that `flatten_path(paths[p], p, t, ..)` produces for each
  `p`. Edges are canonical: `ytop < ybot`, with `xtop` and `dxdy` in `f32` exactly as the code
  computes them. Horizontal edges are dropped, but their endpoints still enter `BBox`.
- `X_p(sy)` = the `f32` crossings `xtop + (sy − ytop)·dxdy` of the edges of path `p` with
  `ytop ≤ sy < ybot`, sorted by `total_cmp`.
- `Spans_p(sy)` = `[X_p[0],X_p[1]), [X_p[2],X_p[3]), …`; an odd trailing crossing is dropped.
- `Cov(E)(x,sy)` holds iff `∃p ∃span ∈ Spans_p(sy)` with `x ∈ span`: even-odd within a path, union
  across paths.
- `c(px,py)` = `#{s ∈ lattice(px,py) : Cov(E)(s)} ∈ 0..=16`.
- `RefFill(tile,paths,rgb,α,t)`: for every in-tile pixel, if `c > 0` then
  `tile.over(px,py,rgb,α·(c as f32)·(1/16))`, else leave the pixel unchanged. Pixels are
  independent, so order does not matter. This is the naive reference rasterizer the scanline core
  must refine.
- `lower(v: u16)` = `f32::from(v) · (1/4096)` (`fixed_to_unit`), exact because `1/4096` is a power
  of two and `v < 2^24`. `lowerP` (`PathSeg::to_cmd`) maps `lower` over every coordinate;
  `Move`/`Line`/`Cubic`/`Close` map 1:1 to `PathCmd`.
- `Render(cmds)` (spec side only) = one explicit letter per command: `M x y`, `L x y`,
  `C x1 y1 x2 y2 x y`, `Z`, each number printed with Rust's `f32` `Display` (shortest round-trip,
  no exponent), tokens separated by single spaces.
- `PathTok`: the tokenizer exactly as `parse_path` scans. Separators are ASCII whitespace and `,`. A
  letter token is any ASCII alphabetic byte. A number token is a maximal run of an optional leading
  sign followed by `[0-9 . e E]`, where a sign may also follow `e`/`E`; its value is
  `str::parse::<f32>`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests), `difficulty` (the Rust
features the proof must handle), `blocking gaps` (rows of §7 of
`docs/AUDIT-trust-for-test-migration-2026-10-15.md`). Gap 0 (an installed seal that publishes
Lean-discharged rows) blocks every row.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
clampf-contract	contract	clampf	∀ v, lo, hi : f32, with r = clampf(v,lo,hi). (a) ¬v.is_nan() ∧ lo ≤ hi ⇒ lo ≤ r ≤ hi, where r = hi if v > hi, r = lo if v < lo, and r = v bitwise otherwise. (b) ¬v.is_nan() ∧ lo > hi ⇒ r = lo (the documented misuse fallback). (c) v.is_nan() ⇒ r.is_nan(), so callers must sanitize first (§5.4).	-	EASY: f32 comparisons and if/else only; straight-line and panic-free. Needs an IEEE f32 carrier with a NaN case.	0,3,5,6
smoothstep-contract	contract	smoothstep, clampf	∀ t : f32 with ¬t.is_nan(): smoothstep(t) = smoothstep(clampf(t,0,1)); t ≤ 0 ⇒ smoothstep(t) = 0.0; t ≥ 1 ⇒ smoothstep(t) = 1.0 exactly; 0 ≤ smoothstep(t) ≤ 1. Monotone: ∀ t1 ≤ t2 (both non-NaN), smoothstep(t1) ≤ smoothstep(t2). Monotonicity under f32 rounding of t·t·(3−2t) must be proved over the f32 model, or dropped if it is false.	-	EASY-MEDIUM: f32 mul/sub and a call to clampf (callee-ensures composition); straight-line. Monotonicity is the hard part.	0,3,4,5,6
mix-rgb-contract	contract	mix_rgb	∀ a, b : u32, t : f32: mix_rgb(a, b, t) & !0x00ff_ffff = 0. Let u = 1.0 if t > 1.0, t if t > 0.0, else 0.0 (so NaN and t ≤ 0 map to u = 0.0): u = 0.0 ⇒ mix_rgb(a, b, t) = a & 0x00ff_ffff, u = 1.0 ⇒ mix_rgb(a, b, t) = b & 0x00ff_ffff, and each channel c ∈ {16, 8, 0} equals ⌊fl(ca + (cb − ca)·u) + 0.5⌋ & 0xff.	lib.rs::clampf_smoothstep_and_mix_rgb_handle_bounds_and_nan	EASY: non-capturing/single-float closure over three constant shifts, f32 clamp with NaN → 0.0, panic-free.	0,3,5,6
motion-easings-q16	contract	motion::{ease_out_quad,ease_out_cubic,ease_in_out_cubic}	∀ e ∈ {ease_out_quad, ease_out_cubic, ease_in_out_cubic}, u : u32: e(0) = 0, ∀ u ≥ ONE: e(u) = ONE, 0 ≤ e(u) ≤ ONE, and ∀ u1 ≤ u2: e(u1) ≤ e(u2). Symmetry: ∀ u ≤ ONE: ease_in_out_cubic(ONE − u) = ONE − ease_in_out_cubic(u).	motion.rs::easings_hit_their_ends_and_rise_monotonically	EASY: pure u32/u64 fixed-point polynomial shifts and TryFrom with no loops or floats.	0,3,5,6
motion-glide	contract	motion::{frac,frac_fine,glide_span,glide}	∀ t, span : Duration: frac(t, span) ∈ 0..=ONE and frac_fine(t, span) ∈ 0..=ONE, with span = 0 ⇒ ONE and t = 0 ∧ span > 0 ⇒ 0. ∀ from, to : u16: GLIDE ≤ glide_span(from, to) ≤ GLIDE_MAX, glide_span(from, to) = glide_span(to, from), and ∀ from, to ≤ 1000: glide(from, to, 0) = from, glide(from, to, glide_span(from, to)) = to, and glide(from, to, t) ∈ 0..=1000.	motion.rs::a_glide_spans_its_floor_per_point_and_ceiling	EASY-MEDIUM: core::time::Duration millis/micros conversions, u64/u128/i64 bounded arithmetic, and composition with ease_out_quad.	0,1,3,4,6,12
motion-spring	no-panic + contract	motion::Spring::{critical,underdamped,omega,zeta,at,settle_ms}	∀ omega, zeta, t_ms : u32: critical(omega).zeta() = ONE, underdamped(omega, zeta).zeta() = min(zeta, ONE − 1), neither at(t_ms) nor settle_ms() panics (intermediate Q32 Euler step widened to i128 and clamped to i64), at(0) = 0, ∀ t_ms ≥ SPRING_MAX_MS: at(t_ms) = ONE as i32, and settle_ms() ≤ SPRING_MAX_MS.	motion.rs::a_critical_spring_arrives_without_passing_its_rest; motion.rs::an_underdamped_spring_overshoots_once_and_settles	MEDIUM: bounded 0..SPRING_MAX_MS loop over SpringState { x, v } with i128 multiply-accumulate clamped to i64 and unsigned abs_diff rest check.	0,1,2,3,4,6,7
motion-cadence	no-panic + contract	motion::Cadence::{hz,per_second,frame_at,frame_start,next_frame}	∀ r : u16: Cadence::hz(r).per_second() = max(r, 1). ∀ c : Cadence, s, t : u64: t ≤ s ⇒ c.frame_at(s, t) = 0; c.frame_at(s, t) = min(⌊(t −_sat s)·hz / 1000⌋, u64::MAX); c.next_frame(s, t) never panics; ∀ t < u64::MAX: c.next_frame(s, t) > t; and when t ≥ s and c.next_frame(s, t) < u64::MAX and c.frame_at(s, t) < u64::MAX, c.frame_at(s, c.next_frame(s, t)) = c.frame_at(s, t) + 1.	motion.rs::a_cadence_keeps_its_grid	EASY-MEDIUM: pure const u64/u128 saturating ratio and div_ceil arithmetic; no loops or floats.	0,1,2,3,5,6
over-run-is-over-fold	equivalence	atlas::Tile::over_run, atlas::Tile::over	∀ tile, x, y, n, rgb, a: over_run(x,y,n,rgb,a) has the same effect on tile.pixels() as over(x+k,y,rgb,a) for k = 0..n in order; when a ≤ 0 ∨ a.is_nan(), both leave tile.pixels() unchanged. The opaque fast path (sa ≥ 1, straight quad copy) is byte-identical to the general mix: oa = 1, so the mix reduces to the source exactly. The i64 clip of [x,x+n) to [0,w) never overflows and equals over's per-pixel bounds check. Requires removing trust::skip from Tile::over.	atlas.rs::nan_alpha_leaves_painted_pixels_unchanged	MEDIUM: &mut Tile with the invariant px.len() = w·h·4, a bounded for loop, local closures, f32→u8 saturating casts, i32/i64/u32 casts, copy_from_slice.	0,1,2,3,5,7,13
flatten-cubic-contract	contract	vector::flatten_cubic	∀ finite p0..p3 with every coordinate in [−COORD_LIMIT, COORD_LIMIT]: flatten_cubic calls emit exactly n times, where n = 1 if err ≤ FLATTEN_TOL and n = clampf(ceil(sqrt(err/FLATTEN_TOL)),1,64) otherwise, so 1 ≤ n ≤ 64. The k-th call is (q_{k−1}, q_k), where q_0 = p0 and q_k is the f32 Bernstein evaluation at k/n. The chain closes exactly: q_n = p3 bitwise, because at t = 1 the weights are 0,0,0,1. All q_k are finite. This is a lemma for the subsample model and for implicit closure in flatten_path (cur = p3).	(none; lemma)	MEDIUM: bounded for loop 1..=n, a generic impl FnMut emit (higher-order spec), f32 sqrt/ceil/abs/max, f32→u32 cast.	0,3,5,7,11,13
parse-path-language	contract	vector::parse_path	∀ d: &str: parse_path(d) = Some(v) ⇔ PathTok(d) succeeds (every number token parses as f32), its token sequence matches (Letter Num*)* with every Letter ∈ {M,L,C,Z}, and no number precedes the first letter. Each M or L group has 2k numbers with k ≥ 1, each C group has 6k numbers with k ≥ 1, and each Z group has none. Then v is the concatenation of the group expansions: M gives Move(first pair) followed by a Line for each later pair; L gives a Line per pair; C gives a Cubic per 6-tuple; Z gives Close. Otherwise the result is None: unsupported letters, a bare M/L/C, a dangling argument count, a malformed number and numbers after Z all give None. "" gives Some([]). parse_path never panics: indices stay at UTF-8 char boundaries because tokens consume only ASCII, and every iteration either advances i or returns.	vector.rs::parse_path_roundtrips_the_asset_grammar	MEDIUM: a &str/as_bytes byte loop with a manual index, str::get(s..i), f32 FromStr (needs a parse model), Vec<f32>/Vec<PathCmd> push/clear, chunks_exact with slice patterns, an FnMut flush closure taking &mut Vec, Option<char> state, char classification.	0,1,3,5,7,13
fill-path-noop-guards	contract	vector::fill_path, fill_scanline, flatten_path, BBox::pixel_range	∀ tile, paths, rgb, α, t: if α ≤ 0 ∨ α.is_nan() ∨ tile.width() = 0 ∨ tile.height() = 0 ∨ paths.is_empty() ∨ E(paths,t) is empty, then tile.pixels() is unchanged by fill_path. E(paths,t) is empty when every subpath is empty, a lone Move, a lone Close, or has only horizontal or degenerate segments. This is the c ≡ 0 case of fill-scanline-refines-subsample-model, stated on its own as an easier first proof.	vector.rs::degenerate_paths_are_safe	MEDIUM: &mut Tile, slices of slices, Vec<Edge> push through a closure, thread_local RefCell with_borrow_mut, for/while loops. Needs the loop invariant "no edge ⇒ no add_span call".	0,1,2,3,5,7,13
fill-path-total-and-bounded	no-panic	vector::fill_path, fill_scanline, add_span, sanitize, flatten_path, flatten_cubic, atlas::Tile::over, Tile::over_run	∀ tile, paths, rgb, α, t, with any f32 values (NaN, ±inf, ±f32::MAX and denormals included) in coordinates and transform fields: fill_path terminates without panic, and its work is bounded by O(Σ|path|·64·log + h·4·(|E|·log|E| + w)). Sub-lemmas: (i) sanitize(v) is finite with |sanitize(v)| ≤ COORD_LIMIT. (ii) Every crossing xtop + (sy−ytop)·dxdy at an active sub-scanline is finite: sy ≥ 0.125 and ytop ≤ sy < ybot bound ybot−ytop below by an ulp, so no NaN span reaches add_span. (iii) All usize/i32 index arithmetic in add_span and the flush pass stays within the lengths width+1 and width and within the tile. (iv) The FILL_SCRATCH RefCell is never borrowed twice, because nothing under fill_scanline re-enters fill_path. No pixel outside [0,w)×[0,h) is written.	vector.rs::out_of_range_coords_clamp (no-panic and in-tile half); vector.rs::degenerate_paths_are_safe (no-panic half)	HARD: panic-freedom over loops, Vec growth, a RefCell borrow, slice get/get_mut, f32→usize saturating casts, and termination measures for the while loops (next, i, j).	0,1,2,3,5,7,13
fill-path-fixed-refines-fill-path	refinement	vector::fill_path_fixed, fill_path	∀ tile, paths: &[&[PathSeg]], rgb, α, t: fill_path_fixed(tile,paths,rgb,α,t) has the same effect on tile as fill_path(tile, paths.map(|p| p.map(lowerP)), rgb, α, t). The per-path grouping is preserved: |refs| = |paths| and refs[i] = lowerP(paths[i]). Hence every fill_path spec (noop-guards, total-and-bounded, the refinement and the geometry) transfers to the fixed-point entry point. In particular, α ≤ 0 ∨ paths.is_empty() ∨ no path has an edge ⇒ tile unchanged. Lemma: lower(v) = v/4096 exactly, so q(k/8) round-trips bit-exactly.	vector.rs::fill_path_fixed_matches_float_frame	MEDIUM: Vec::with_capacity/extend with a mapping closure over an enum, slice get(off..end) with the offset loop invariant off = Σ prior lengths, u16→f32 conversion, callee composition into fill_path.	0,1,2,3,4,5,7,13
fill-scanline-refines-subsample-model	refinement	vector::fill_path, fill_scanline, add_span, flatten_path, flatten_cubic, BBox::pixel_range, atlas::Tile::over, Tile::over_run	∀ tile, paths, rgb, α > 0 (so α is not NaN), t, and ∀ prior contents and lengths of FILL_SCRATCH: the tile after fill_path(tile,paths,rgb,α,t) equals RefFill(tile,paths,rgb,α,t). Sub-lemmas: (i) The result does not depend on the scratch state, so two calls on equal inputs give byte-identical pixels. (ii) Clipping to the BBox is lossless: every covered sample lies in the pixel range [x0,x1)×[y0,y1). (iii) Active-edge invariant: after the activation loop at sub-scanline sy, active = {e ∈ E : e.ytop ≤ sy < e.ybot} as a multiset, because sy strictly increases over (py,sub). (iv) add_span adds, for each pixel, exactly #{lattice x-samples in [a,b)}, through diff (prefix sum ±4) plus extra; diff and extra are all zero at every row start. (v) over-run-is-over-fold, and α·16·(1/16) = α exactly, so the run branch and the partial branch both agree with RefFill. (vi) The union merge of sorted spans counts each sample at most once, so c ≤ 16 holds without relying on the clamp.	-	VERY HARD: thread_local RefCell, &mut struct destructuring, six Vec scratch buffers (clear/resize/reserve/push/retain/sort_unstable_by with total_cmp), while-let over get(), nested for/while loops with loop invariants, Option match, closures (an FnMut push and a generic impl FnMut emit), f32 arithmetic and f32→usize/i32 casts, mem::take, &mut Tile with the px.len() = w·h·4 invariant. Tile::over is trust::skip today.	0,1,2,3,4,5,6,7,11,13
fill-path-coverage-geometry	other (geometric meaning)	vector::fill_path, flatten_path, atlas::Tile::over	For polygonal paths (Move/Line/Close only) under any transform t: ∀ sample s = (x,sy) whose distance to every device-space edge exceeds δ (the f32 crossing error bound, O(ulp(COORD_LIMIT))): Cov(E)(s) ⇔ ∃ path p such that a horizontal ray from s crosses p's implicitly closed subpaths an odd number of times. This is even-odd within a path regardless of winding, and union (never cancellation) across paths. Device geometry is clipped to the tile, so no pixel outside [0,w)×[0,h) is touched. Corollaries through RefFill and Tile::over: c(px,py) = 16 ∧ α ≥ 1 ⇒ the alpha byte is 255 whatever the prior contents; c = 0 ⇒ the pixel is unchanged.	vector.rs::even_odd_holes_work; vector.rs::out_of_range_coords_clamp (clip-to-edge half)	HARD: builds on fill-scanline-refines-subsample-model (cited as a lemma) plus a real-geometry model of polygons and f32 error bounds; quantified over samples and paths; a Lean List/inductive model of the path.	0,1,4,5,6,14
parse-path-roundtrip	round-trip	vector::parse_path	∀ cmds: Vec<PathCmd> whose coordinates are all finite: parse_path(Render(cmds)) = Some(cmds), using PathCmd's PartialEq. This relies on f32 Display being shortest-round-trip with no exponent, and on "-0" parsing to −0.0. Conversely, ∀ d with parse_path(d) = Some(v): parse_path(Render(v)) = Some(v), so Render∘parse is idempotent on the accepted language.	(secondary only: vector.rs::parse_path_roundtrips_the_asset_grammar)	HARD: everything in parse-path-language, plus a model of f32 Display/FromStr round-trip (Ryu-style shortest representation) and String building in the spec-side Render.	0,1,3,5,7,13,14
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test; `also` lists the other specs it samples. A test is
deleted only when its primary spec and every spec under `also` are proved.

```tsv
file	test	primary	also
src/lib.rs	clampf_smoothstep_and_mix_rgb_handle_bounds_and_nan	mix-rgb-contract	clampf-contract, smoothstep-contract
src/atlas.rs	nan_alpha_leaves_painted_pixels_unchanged	over-run-is-over-fold	fill-path-noop-guards
src/motion.rs	easings_hit_their_ends_and_rise_monotonically	motion-easings-q16	
src/motion.rs	a_glide_spans_its_floor_per_point_and_ceiling	motion-glide	motion-easings-q16
src/motion.rs	a_critical_spring_arrives_without_passing_its_rest	motion-spring	
src/motion.rs	an_underdamped_spring_overshoots_once_and_settles	motion-spring	
src/motion.rs	a_cadence_keeps_its_grid	motion-cadence	
src/vector.rs	even_odd_holes_work	fill-path-coverage-geometry	fill-scanline-refines-subsample-model
src/vector.rs	fill_path_fixed_matches_float_frame	fill-path-fixed-refines-fill-path	fill-path-noop-guards
src/vector.rs	degenerate_paths_are_safe	fill-path-noop-guards	fill-path-total-and-bounded
src/vector.rs	out_of_range_coords_clamp	fill-path-total-and-bounded	fill-path-coverage-geometry
src/vector.rs	parse_path_roundtrips_the_asset_grammar	parse-path-language	parse-path-roundtrip
```

## 3. Coverage

- 12 live tests mapped. All 12 map to a spec, and none is a non-spec check.
- The four `motion-*` specs in `src/motion.rs` are pure integer/fixed-point contracts (5 tests) that
  do not depend on a float model; `parse_path_roundtrips_the_asset_grammar` also retires on
  `parse-path-language`.
- The three `fill_path` behaviour tests (`even_odd_holes_work`, `out_of_range_coords_clamp` and the
  coverage half of `fill_path_fixed_matches_float_frame`) all sit behind
  `fill-scanline-refines-subsample-model`, so that proof is the rasterizer's bottleneck.
  `fill-path-coverage-geometry` adds real geometry on top of it.

## 4. Tests that are not spec samples

None.

## 5. Implied specs that look wrong, or disagree with the code

1. **Resolved — malformed number tokens in `parse_path_roundtrips_the_asset_grammar`.** Previously
   `parse_path("M x y").is_none()` was labelled `"non-numeric"` even though `x` triggers the
   letter-flush branch (`flush('M', [])`) rather than `tok.parse().ok()?`. The test now labels
   `"M x y"` accurately and also exercises `"M 1.2.3 4"`, `"M 1e 2"` and `"Z 1 2"`.
   `parse-path-language` covers all branches.
2. **Resolved — `parse_path_roundtrips_the_asset_grammar` now exercises `Render → parse` round-tripping.**
   It renders parsed explicit, implicit-`L`/repeated-`C`, and signed/fractional command lists back to
   `M x y` / `L x y` / `C x1 y1 x2 y2 x y` / `Z` text and asserts `parse_path(&render(&cmds)) == Some(cmds)`
   (`parse-path-roundtrip`).
3. **`parse_path` has no production caller.** `aterm_scene::parse_path` (`src/vector.rs:618`)
   has no caller in the workspace outside tests. Its only user outside this crate is the
   integration test `crates/aterm-effects/tests/pet_art_quality.rs:129`. Prove its two specs only
   if the `pub` API is kept. Otherwise move the parser next to that test and drop both specs.
4. **Resolved — `clampf` doc and `NaN` alpha guards.** `src/lib.rs` now documents that `NaN` `v`
   propagates through `clampf`, and `fill_path`, `fill_path_fixed`, `Tile::over` and
   `Tile::over_run` all guard `alpha.is_nan()` / `a.is_nan()` alongside `<= 0.0`, leaving existing
   pixels untouched instead of erasing them to transparent black (`atlas::tests::nan_alpha_leaves_painted_pixels_unchanged`).
5. **`Tile::over` is `trust::skip`.** Every fill spec composes through it, so the skip must be
   removed (and `over` given its own contract) before `over-run-is-over-fold` or any fill refinement
   can be proved.

## 6. Proof order (easiest high-value first)

Every spec needs the `f32` model, so the order starts with the narrowest remaining gaps.

1. **`clampf-contract`**: two comparisons, straight-line; the first proof over an `f32` carrier
   with a NaN case, which every other spec needs.
2. **`smoothstep-contract`**: composes `clampf`'s ensures. Drop the monotonicity clause if rounding
   breaks it.
3. **`parse-path-language`**: self-contained (no `Tile`, no thread local); retires
   `parse_path_roundtrips_the_asset_grammar`. Skip it if `parse_path` leaves the `pub` API (§5.3).
4. **`fill-path-noop-guards`**: the first proof over `&mut Tile` and the scratch `RefCell`.
5. **`over-run-is-over-fold`** and **`flatten-cubic-contract`**: bounded-loop lemmas the main
   refinement cites; the first needs §5.5.
6. **`fill-path-total-and-bounded`**: panic-freedom and termination of the whole rasterizer; with
   step 4 it retires `degenerate_paths_are_safe`.
7. **`fill-scanline-refines-subsample-model`**: the bottleneck (§3).
8. **`fill-path-fixed-refines-fill-path`**: thin once step 7 holds (§5.6); retires
   `fill_path_fixed_matches_float_frame`.
9. **`fill-path-coverage-geometry`**: adds gap 14 and a real-geometry model; retires
   `even_odd_holes_work` and, with step 6, `out_of_range_coords_clamp`.
10. **`parse-path-roundtrip`**: last; it needs an `f32` `Display`/`FromStr` model and retires no
    test.
