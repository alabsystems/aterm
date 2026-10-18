<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-sixel: spec recovery from its 25 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 8 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-15: this is the crate's migration worklist. Each of the 14 specs below is
> proved through Trust/Clean, and then the tests it subsumes are deleted. The one test in §4
> that samples no spec is deleted now.

Source: `crates/aterm-sixel` (read only). I read every `#[test]` (all 25 are in
`src/tests.rs`) and the code each one reaches in `src/lib.rs`. Nothing was compiled or run.
The claims below come from reading the code.

Result: **14 specs**, covering **24 tests**. **1 test is not a spec sample** (§4). Two
statements were corrected against the code during the merge (§5.1, §5.2). One of them,
`decode-refines-model`, is false as first stated near the pixel cap.

## 0. Notation used in the statements

- `D` = `SixelDecoder`. `Reach` = the decoder states reachable from `D::new()` by any sequence of
  `hook`, `put(b)`, `unhook` and `abort`.
- `run(bs)` = `let d = D::new(); d.hook(); for b in bs { d.put(b) }; d.unhook()`. This is the tests'
  `decode` helper.
- `DIM` = `SIXEL_MAX_DIMENSION` = 4096. `CAP` = `SIXEL_MAX_PIXELS` = 4 Mi. `NREG` =
  `MAX_COLOR_REGISTERS` = 1024. `PMAX` = `SIXEL_MAX_PARAMS` = 8.
- After a run, `max_x` and `max_y` are the decoder's painted (or positional) extent.
  `declared_w = min(Ph, DIM)` and `declared_h = min(Pv, DIM)` come from the last `"` introducer
  (0 if there is none). `W(bs) = min(max(max_x, declared_w), DIM)` and
  `H(bs) = min(max(max_y, declared_h), DIM)`. These are the decoder's own extents, not the model's.
- `scale(v) = (min(v,100)*255 + 50) / 100`. `DEC[i]` = the 16-entry `(r%,g%,b%)` table in
  `default_palette`.
- `Model(bs)` = a reference interpreter of the sixel wire format, written once as a Clean function
  over `List u8`. Its state is `(palette: Fin NREG → u32, cur: Fin NREG, x, band, repeat, decl_w,
  decl_h, paint: (x,y) → Option u32)`, and its rules are:
  - A data byte `b` in `0x3F..=0x7E` paints rows `band+k` for every set bit `k` of `b-0x3F`, in
    columns `x .. x+n-1` with `n = min(max(repeat,1), DIM-x)`. Each painted pixel is
    `0xFF000000 | (palette[cur] & 0xFFFFFF)`. Then `x += n` (clamped to `DIM`) and `repeat = 0`. A
    zero pattern (`?`) advances `x` and extends the width without painting.
  - `$` sets `x=0, repeat=0`. `-` sets `x=0, band+=6, repeat=0`.
  - `#Pc` sets `cur = min(Pc, NREG-1)`. `#Pc;Pu;Px;Py;Pz` also sets
    `palette[cur] = (Pu==1 ? hls_to_rgb : rgb_percent)(Px,Py,Pz)`.
  - `!Pn` sets `repeat = Pn`. `"a;b;Ph;Pv` sets `decl = (min(Ph,DIM), min(Pv,DIM))`.
  - Any other byte is ignored. Digits saturate in `u32`, and at most `PMAX` params are collected.
  - The image is `W × H`, row-major, where `W`/`H` use the model's extent. Pixel `(x,y)` is
    `paint(x,y)`, or 0 if unpainted.
- `RI(d)`, the raster invariant: `raster.len() == alloc_width*alloc_height`,
  `alloc_width*alloc_height <= CAP`, `alloc_width <= DIM` and `alloc_height <= DIM`.
- `Box(bs)` = `W(bs) * min(DIM, 6*ceil(H(bs)/6))`. This is the band-rounded box that
  `ensure_capacity` actually allocates (§5.2).
- `ensure_capacity` is `#[cfg_attr(trust_verify, trust::skip)]` today. So every spec that touches
  growth has to un-skip it first, or prove it under an `unbounded_allocation` assumption.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; the secondary ones
are in §2), `difficulty` (the Rust features the proof must handle), and `blocking_gaps` (row
numbers in the gap table, audit §7). Gap 0 (a seal that publishes Clean rows) blocks every row.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
inactive-noop	contract	SixelDecoder::{new,put,unhook,abort}	∀ d∈Reach with d.active = false, ∀ b: put(b) leaves d unchanged, and unhook() returns None and leaves d unchanged. D::new() is inactive. Every unhook and every abort leaves d inactive.	unhook_without_hook_is_none	EASY: &mut self, an early return on a bool field, an Option result, a struct-equality frame condition	0,1,2,4
unhook-accept	contract	SixelDecoder::unhook	∀ bs: run(bs) is Some ⇔ W(bs) > 0 ∧ H(bs) > 0 ∧ W(bs)*H(bs) ≤ CAP (over the decoder's own extent). In particular: run([]) = None; a DECGRA-only declaration Ph×Pv with 0 < Ph*Pv ≤ CAP is Some with exactly those dimensions; any declaration or paint whose box exceeds CAP is None, and the width*height output Vec is never allocated for it. Lemma: the `final_bytes > SIXEL_MAX_IMAGE_BYTES` branch is unreachable, because total ≤ CAP gives total*4 ≤ 16 MiB.	empty_sequence_is_none, capped_raster_at_the_pixel_limit_still_decodes, decgra_only_oversized_yields_none_without_transient	MEDIUM: saturating_mul, min/max, Option, and a quantifier over byte strings (a fold over put)	0,1,2,3,4,5,7
image-shape	invariant	SixelDecoder::unhook, SixelImage::{width,height,pixels}	∀ d∈Reach: if d.unhook() = Some(img), then 1 ≤ img.width ≤ DIM, 1 ≤ img.height ≤ DIM, img.width*img.height ≤ CAP and img.pixels.len() = img.width*img.height. This holds for every input, including a hostile DECGRI count such as `!4294967295`. It is a type invariant on SixelImage.	dimensions_are_clamped	MEDIUM: Vec length after extend_from_slice and resize inside a loop, a struct type invariant, &mut self	0,1,2,3,7
decode-refines-model	refinement	SixelDecoder::{hook,put,unhook,emit_sixel,paint_pixelwise,finish_params,apply_color,apply_raster,apply_repeat,ensure_capacity}	∀ bs with Box(bs) ≤ CAP (see §5.2; the weaker precondition W*H ≤ CAP is false): run(bs) = Model(bs), meaning equal width, height and every pixel. A painted pixel is 0xFF000000 | (palette[reg] & 0xFFFFFF) of the register current when it was painted. Every other pixel is 0, including the right-edge and bottom padding up to the declared geometry. Consequences: DECGRI `!Pn` repeats only the next data byte; `$` and `-` cancel a pending repeat; `-` advances the band by 6; the output does not depend on the over-allocated raster stride.	partial_column_sets_only_low_bits, repeat_does_not_survive_band_control, decgra_only_within_cap_uses_declared_geometry, compose_pads_declared_edges_transparent	VERY HARD: a Vec<u32> content model (raster, palette, params, output), get_mut/fill/extend_from_slice/resize, for loops over arrays and ranges, an enum match (ParamMode), &mut self two-state reasoning, a Clean List u8 model bound to the Rust fold, and the trust::skip on ensure_capacity	0,1,2,3,4,5,7,10
color-register	contract	SixelDecoder::{apply_color,finish_params}	∀ params p in mode Color: if p = [], nothing changes. Otherwise let reg = min(p[0], NREG-1). Afterwards current_color = reg and palette.len() = NREG still. If |p| ≥ 5, then palette'[reg] = (p[1] = 1 ? hls_to_rgb(p[2],p[3],p[4]) : rgb_percent(p[2],p[3],p[4])). Every other palette entry is unchanged. So an out-of-range register never panics and never grows the palette.	register_select_out_of_range_is_clamped_no_panic	MEDIUM: &mut self, Vec get/get_mut, a tuple of Options, an `as usize` cast, min	0,1,2,3,4
rgb-percent	equivalence	rgb_percent, scale_pct	∀ r,g,b: u32: rgb_percent(r,g,b) = (scale(r) << 16) | (scale(g) << 8) | scale(b), where scale(v) ∈ 0..=255, scale(0) = 0 and scale(100) = 255. Hence rgb_percent(r,g,b) ≤ 0x00FFFFFF.	-	EASY: pure u32 arithmetic (saturating ops, division by a constant, shifts, bitwise or) and a call to an admitted fn	0,3,5
hls-range	no-panic	hls_to_rgb	∀ h,l,s: u32: hls_to_rgb(h,l,s) does not panic, and its result is ≤ 0x00FFFFFF, so each channel is in 0..=255. The intended stronger spec (standard HSL with DEC's +240° hue offset) is not sampled by any test (§5.3).	-	HARD: f64 arithmetic (mul_add, abs, %, round), float-to-int casts, a closure capturing m, a match on `h6 as u32`. No row of audit §7 covers float reasoning.	0,3,5,13
default-palette	table-property	default_palette	default_palette().len() = NREG. For i < 16: pal[i] = rgb_percent(DEC[i]), so pal[0] = 0. For 16 ≤ i < NREG: pal[i] = 0. Decidable by evaluation once the body is admitted.	-	EASY-MEDIUM: a vec![0; N] content model, a loop over a const array with enumerate, get_mut	0,1,3,5,7
spans	contract	SixelImage::{rows_spanned,cols_spanned}	∀ img, ∀ c: u16, with c' = max(c,1): rows_spanned(c) = ceil(height / c') and cols_spanned(c) = ceil(width / c'). Each is 0 exactly when its dimension is 0, and ≥ 1 otherwise.	span_helpers_round_up	EASY: &self field reads, u16→usize widening, is_multiple_of, saturating_add, max with a bool cast	0,2,3
hook-reset	equivalence	SixelDecoder::hook	∀ d∈Reach: after d.hook(), d is observationally equal to D::new() followed by hook(). That is, ∀ bs: (d.hook(); put* bs; d.unhook()) = run(bs). No palette, geometry, extent, params or pending repeat carries over from an earlier cycle.	reuse_across_cycles_resets_state	MEDIUM: &mut self two-state reasoning over every field, Vec clear and reassignment; composes with decode-refines-model for the image half	0,1,2,4,5
release	contract	SixelDecoder::{abort,release,pixel_alloc_bytes,unhook}	∀ d∈Reach: after d.abort(), d.active = false, pixel_alloc_bytes() = 0 and params = [], so a following unhook() is None. After any d.unhook(), pixel_alloc_bytes() = 0 as well.	abort_frees_and_yields_no_image	EASY: &mut self, Vec::new reassignment, Vec len times size_of	0,1,2
lazy-alloc	invariant	SixelDecoder::{put,apply_raster,emit_sixel,ensure_capacity,pixel_alloc_bytes}	∀ d∈Reach: RI(d), so pixel_alloc_bytes() ≤ 4*CAP. Allocation is driven only by painting: ∀ bs, the raster state after hook; put* bs is unchanged when every `"` introducer is deleted from bs, and if bs contains no data byte in 0x40..=0x7E then pixel_alloc_bytes() = 0. A declared raster of any size never pre-sizes the buffer.	oversized_raster_declaration_is_refused_without_allocating	HARD: a Vec length model across resize and vec! reallocation, &mut self, a state invariant over Reach, a fold over put, and the trust::skip on ensure_capacity (its unbounded_allocation obligation, which sits nearest gap 8)	0,1,2,5,7,8
growth-amortized	contract	SixelDecoder::ensure_capacity	Assume RI(d). Let aw, ah = alloc_width, alloc_height before the call; need_w = min(wx+1, DIM); need_h = min(wy+1, DIM); new_h = max(need_h, ah); geo = min(max(2*aw, need_w), DIM); new_w = (need_w ≤ aw) ? aw : (geo*new_h ≤ CAP ? geo : max(min(⌊CAP/new_h⌋, DIM), need_w, aw)). Then ensure_capacity(wx,wy) gives: (a) RI(d'). (b) If need_w ≤ aw ∧ need_h ≤ ah, d is unchanged. Else if new_w*new_h ≤ CAP, then alloc_width' = new_w ≥ need_w and alloc_height' = new_h ≥ need_h. Otherwise d is unchanged (fail closed). (c) If need_w ≤ aw, alloc_width' = aw: height-only growth keeps the stride and takes the in-place resize path. (d) Corollary: painting columns 0..N one at a time at a fixed height reallocates the width O(log N) times. (e) The old raster's rows are kept as a row-major prefix at the new stride, and new cells are 0.	width_growth_is_amortized_and_preserves_the_height_fast_path, tall_raster_width_growth_stays_amortized_near_the_pixel_cap	HARD: &mut self, checked_mul and an is_some_and closure, NonZeroUsize division, Vec resize and vec! allocation, nested chunks/chunks_mut/zip loops; trust::skip must be lifted	0,1,2,3,7,8,13
params-bound	invariant	SixelDecoder::{put,start_params,finish_params}	∀ d∈Reach: d.params.len() ≤ PMAX (8). Because PMAX ≥ 5, a well-formed `#Pc;Pu;Px;Py;Pz` is collected intact, so the cap never changes the decoded result of a colour define.	semicolon_flood_keeps_params_bounded	MEDIUM: Vec push guarded by a len() comparison in a match guard, last_mut, &mut self, an invariant over Reach	0,1,2,5
```

## 2. Test → spec map (TSV; all 25)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
Every path is `src/tests.rs`.

```tsv
file	test	primary	also
src/tests.rs	unhook_without_hook_is_none	inactive-noop	
src/tests.rs	empty_sequence_is_none	unhook-accept	
src/tests.rs	width_growth_is_amortized_and_preserves_the_height_fast_path	growth-amortized	
src/tests.rs	tall_raster_width_growth_stays_amortized_near_the_pixel_cap	growth-amortized	
src/tests.rs	oversized_raster_declaration_is_refused_without_allocating	lazy-alloc	unhook-accept
src/tests.rs	capped_raster_at_the_pixel_limit_still_decodes	unhook-accept	image-shape
src/tests.rs	partial_column_sets_only_low_bits	decode-refines-model	
src/tests.rs	dimensions_are_clamped	image-shape	decode-refines-model
src/tests.rs	register_select_out_of_range_is_clamped_no_panic	color-register	image-shape
src/tests.rs	span_helpers_round_up	spans	
src/tests.rs	reuse_across_cycles_resets_state	hook-reset	decode-refines-model
src/tests.rs	abort_frees_and_yields_no_image	release	inactive-noop
src/tests.rs	repeat_does_not_survive_band_control	decode-refines-model	
src/tests.rs	decgra_only_oversized_yields_none_without_transient	unhook-accept	lazy-alloc
src/tests.rs	decgra_only_within_cap_uses_declared_geometry	decode-refines-model	unhook-accept
src/tests.rs	compose_pads_declared_edges_transparent	decode-refines-model	
src/tests.rs	semicolon_flood_keeps_params_bounded	params-bound	
```

## 3. Coverage

- 25 tests read. 24 map to one of the 14 specs. 1 is not a spec sample (§4).
- The biggest collapse is `decode-refines-model`, with 8 primary tests plus 3 shared.
  `unhook-accept` has 3 primary tests plus 3 shared, and `growth-amortized` has 2.
- Gap histogram (gap → specs it blocks, out of 14): 0 → 14, 1 → 11, 2 → 11, 3 → 9, 5 → 8,
  7 → 6, 4 → 5, 8 → 2, 13 → 2, 10 → 1. No spec is blocked by gap 0 alone.

## 4. Tests that are not spec samples

Status (2026-10-15): `param_cap_preserves_valid_color_define` is deleted, and its §2 row is gone. Its `SIXEL_MAX_PARAMS >= 5` const assert is not a tautology (it guards the second clause of `params-bound`), so it was kept as a module-level `const _` in `src/tests.rs` until that clause is proved.

Every regression test I found is an instance of one of the general specs above. That includes
the DECGRA-only transient, the tall-raster amortization, the `!3$~` repeat leak and the
semicolon flood.

## 5. Implied specs that look wrong, or disagree with the code

1. **The chunk draft of `growth-amortized` misread `new_h`.** It wrote `new_h = max(need_h, alloc_width)`.
   The code is `need_h.max(self.alloc_height)` (`ensure_capacity`, `src/lib.rs`). The fallback width is
   `max(min(CAP/new_h, DIM), need_w, alloc_width)`, and the result is refused only when
   `new_w*new_h > CAP`. §1 now states the exact `new_w`.
2. **`decode-refines-model` is false with the precondition `W*H ≤ CAP`.** `emit_sixel` asks
   `ensure_capacity` for rows up to `band_top + 5`, whatever bits are set. So the raster it requests is
   rounded up to the whole band, while the image height counts only painted rows. Take 681 × `-`
   followed by `!1026A`:
   - The model paints row 4087 in columns 0..1025, so `W = 1026`, `H = 4088` and
     `W*H = 4,194,288 ≤ CAP`. The model's answer is `Some(1026×4088)`.
   - The decoder asks for `need_h = 4092`. Then `1026*4092 > CAP`, so `ensure_capacity` fails
     closed. `alloc_width` stays 0, and `stride == 0` returns from `emit_sixel` *before* `x` advances.
     So `max_x = 0` and `run` is `None`.

   With an earlier paint (`stride > 0`), the same refusal leaves the column unpainted while `x` still
   advances, so the pixels differ instead. §1 therefore states the spec under `Box(bs) ≤ CAP`.
   The better fix is in the code: pass `band_top + highest_set_row(bits)` to `ensure_capacity`.
   After that, `need_h ≤ H` always holds, and `⌊CAP/new_h⌋ ≥ W` means the grow is never refused for
   a `W*H ≤ CAP` image. The precondition can then go back to `W*H ≤ CAP`.
   (Found while merging, by reading the code; not run.)
3. **The intended HLS spec is unstated and untested.** `hls-range` states only that `hls_to_rgb` does
   not panic and returns at most `0x00FFFFFF`. The intended spec is standard HSL with DEC's +240° hue
   offset, so that `hls(120,50,100)` is pure red.
4. **`oversized_raster_declaration_is_refused_without_allocating` has stale comments.** They say
   "ensure_capacity refused the 4096x4096 declaration" and mention a "~150 MB raster/mask/compose".
   `apply_raster` never calls `ensure_capacity`. The raster stays small because it grows only to the
   painted 1×6 column, and there is no mask buffer any more. The `None` comes from `unhook`'s
   `W*H > CAP` check.
5. **`decgra_only_oversized_yields_none_without_transient` credits the wrong guard.** Its assertion
   message credits `SIXEL_MAX_IMAGE_BYTES` (4096·4096·4 > 16 MiB), but the `None` comes from the earlier
   `total > SIXEL_MAX_PIXELS` check. The `SIXEL_MAX_IMAGE_BYTES` branch in `unhook` is unreachable
   (see the lemma in `unhook-accept`). Delete the branch, or keep it with a proved-dead note.
6. **`partial_column_sets_only_low_bits` is misnamed.** `A` (value 2) sets only bit 1 (row 1), not the
   "low bits", and the test checks only rows 0–2.
7. **`dimensions_are_clamped` is weak.** It asserts only `≤ DIM`. The code deterministically gives
   `width = 4096` and `height = 6` for `!4294967295~`, and `decode-refines-model` pins that exactly.
8. **`register_select_out_of_range_is_clamped_no_panic` checks only the dimensions.** Its comment says
   "not grow the palette", but nothing asserts `palette.len()`, or that register 1023 (default black, so
   the painted pixel is `0xFF000000`) was selected. `color-register` states both.

## 6. Proof order (easiest high-value first)

No spec here is blocked by gap 0 alone. Every one needs at least gap 3 (body widening) or
gaps 1 and 2 (Vec content and `&mut self` views). So the order starts with the narrowest
widening.

1. **`rgb-percent`** (gaps 0, 3, 5). This is pure `u32` arithmetic with no heap and no `self`, so
   it is an `omega`-class proof once shifts, `|`, `min` and calls are admitted. It retires 1 test
   and is the lemma `default-palette` and `color-register` cite.
2. **`spans`** (gaps 0, 2, 3). Two `&self` scalar reads and a ceiling division. It retires 1 test
   and is the first certified `&self` view in this crate.
3. **`release`** together with **`inactive-noop`** (gaps 0, 1, 2, 4). These are field
   assignments plus `Vec::new`, and a bool early return. They need only Vec length, not content,
   which Trust already models. They retire 2 tests and give the frame lemmas every later spec uses.
4. **`params-bound`** (gaps 0, 1, 2, 5). This is a length-only invariant over `Reach`. It retires
   1 test and makes the §4 deletion safe in fact as well as by duplication.
5. **`color-register`**, then **`default-palette`**. These are the first Vec *content* specs
   (gap 1 proper). Each is small and indexed by a clamped register. They retire 2 tests.
6. **`unhook-accept`** and **`image-shape`**. These are the security-relevant cap specs (no
   over-cap allocation, exact output length). They need the fold over `put` (gaps 5, 7). They retire
   4 tests and prove the dead-branch lemma (§5.5).
7. **`growth-amortized`**, then **`lazy-alloc`**. Both first need `ensure_capacity`'s
   `trust::skip` lifted. They retire 3 tests, and `RI` is the invariant `decode-refines-model`
   stands on.
8. **`decode-refines-model`**, after the §5.2 fix (or under `Box(bs) ≤ CAP`). This is the
   highest-value spec here (8 primary tests), but it needs every gap above plus gap 10. Then
   **`hook-reset`** follows from it and `release`.
9. **`hls-range`** last. It is f64 code with a closure, and no gap row covers float reasoning yet.
   Its real spec (§5.3) has to be written before it is worth proving.
