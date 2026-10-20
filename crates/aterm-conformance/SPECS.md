<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-conformance: spec recovery from its 412 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 108 tests
> it deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this
> file were taken before.

> Status 2026-10-16: this file is the crate's migration worklist. Nothing has been proved or deleted yet.
> Each of the 86 specs below is proved through Trust/Clean about the compiled Rust function, then the
> tests it subsumes are deleted. The 27 tests in §4 are deleted or moved to a bench without a spec.

Source: `crates/aterm-conformance` (read only). Every `#[test]` was read, together with the code it reaches:
`tests/{conformance 15, vt_categories 12, vt_categories2 7, vt_categories3 25, xterm_alternate_finals 7,
xterm_differential_regressions 30, responses 24, kitty_mouse 20, osc8_schemes 8, rect_ops 44, sixel 24,
paint 23, spin 2}` and `src/measure/{scan_tests 139, paint_tests 14, starvation_tests 13, instance 3, spin 2}`
= 412. The test list was re-counted from the tree. The claims come from reading the code; nothing was
compiled or run. Paths in this file are relative to `crates/aterm-conformance` unless they name another crate.

Result: **86 specs**, covering **385 tests**. **27 tests are not spec samples** (§4): 2 to delete, 25 to
move to a bench.

Outside that count: `tests/vt_oracle` (5 tests, added after the recount) is the differential VT oracle
against astream-term's independent fold. It samples no single spec; it compares two engines over a corpus
and seeded random programs, holds the cases the pinned fold cannot judge to written xterm answers, and
checks its own comparisons, so it stays a test when the specs above are proved.

**Where the proofs land.** The crate has two halves, and they migrate differently.

- **The VT half** (`tests/*.rs`, mounted in the `vt` binary through `tests/vt.rs`) tests no code in this
  crate. `Screen`/`run` in `src/lib.rs` is a thin read harness over `aterm_core::terminal::Terminal`. Its
  48 specs (215 tests) are proved on `aterm-core`, `aterm-grid`, `aterm-types` and `aterm-sixel` functions;
  the `home` column names the crate. The `merge` column names rows already in those crates' `SPECS.md` that
  look like the same property. The match is by id and function only; the statements were not compared
  clause by clause. Prove each property once, in its home crate, and delete the tests here when it lands.
  Proving one of them from this crate instead needs cross-crate citation (gap 12).
- **The measurement half** (`src/measure`: the pixel scanner, paint judge, starvation classifier, spin
  judge and instance budget) is this crate's own library code. Its 38 specs (170 tests) are proved here.

## 0. Notation used in the statements

VT side (extends `crates/aterm-core/SPECS.md` §0):

- `Reach`: the `Terminal`s reachable from `Terminal::new(R,C)`, R,C ≥ 1, by `process`; the test binary
  builds aterm-core with the `sixel` feature. `T·x` = `process(T,x)`. The harness forms
  `Screen::new(R,C)`+`feed(x)`, `run(x)` and `term_24x80(x)` are all `fresh(R,C)·x` (default 24×80).
- Projections of a terminal T: `Row(T,r)` = `row_text(r)` with trailing blanks trimmed; `Cur` = (row, col),
  0-based; `PW` = the grid's pending-wrap flag; `Sty` = the pen as (fg_packed, bg_packed, flags_bits) plus
  the underline colour (`style_fingerprint`); `Flags(r,c)` = `cell_flags_bits`; `Link(r,c)` = `hyperlink_at`;
  `Img(r)` = `images_row(r)`; `Resp` = the reply bytes since the last `take_response` (None when empty).
- Modes and margins: `Reg` = (top, bottom) from DECSTBM (default the whole screen); `HM` = horizontal
  margins (L, R); `LR` = DECLRMM; `OM` = DECOM; `AW` = DECAWM; `Stops` = the tab-stop vector; `LS(r)` = the
  row's line size; `CS` = CharacterSetState (G0..G3, GL, pending single shift); `Slot(buf)` =
  `cursor_save.{main,alt}`; `Act` ∈ {Main, Alt}; `Main`/`Alt` = the two grids' cell content.
- `Obs(T)` = all of the above plus every mode bit and the reply buffer; `T ≡ U` means `Obs(T) = Obs(U)`.
- `MaxCol(r)` = `effective_cols_for_row(r) − 1` (C−1, or C/2−1 on a double-width row). `BCE` =
  `Cell::bce_blank` for the current SGR background. `Index` = the grid's `line_feed` (xterm xtermIndex),
  `RevIndex` = reverse index. "print c" = `write_char(c)`, which covers charset translation, width and
  autowrap.
- `Rect(P)` = `parse_rect_coords(P)` → `Option<(top,left,bottom,right)>`, 0-based inclusive.
  `Ext(t,l,b,r,stream)` = the rectangle {(y,x) : t≤y≤b, l≤x≤r} when ¬stream, else
  {(y,x) : (t,l) ≤lex (y,x) ≤lex (b,r), 0 ≤ x < C}. DECSACE 0/1 select stream (the default), 2 selects rect.
- ESC is 0x1B, ST is `ESC \`, `CSI` is `ESC [`, `DCS` is `ESC P`.

Measurement side (`src/measure`; constants are the `pub const`s of `scan.rs`, `paint.rs`, `starvation.rs`):

- A `Frame` is a w×h RGBA8 raster. `F[p]` is the RGB of pixel p = y·w + x; row and frame *equality*
  include alpha.
- `Changed(B,F)` = {p : F[p] ≠ B[p]}. `Sat(B,F)` = the SatMap {p ↦ d : p ∈ Changed(B,F),
  saturated_hue_degrees(F[p]) = Some(d)}; a SatMap maps pixel index to hue degrees.
- `comp4(S)` = the 4-connected components of S inside w×h (no wrap across rows).
- `maxcirc(A)` = max over i<j of min(|aᵢ−aⱼ|, 360−|aᵢ−aⱼ|), 0 when |A| < 2
  (= `max_circular_hue_distance_degrees`).
- `Shape` is ordered lexicographically by (pixels, width, height). `isolating(n,c)` =
  `compact_pet_witness_frame(n,c)`.
- A status row (`StatusRow`) is a whitespace-separated `key=value` line. In `scan_frames`, per-frame
  quantities are read from `ctx.statuses[i]`; "paired take" means `statuses = Some`.

Gap numbers in the `blocking gaps` column are the rows of the table in
`AUDIT-trust-for-test-migration-2026-10-15.md` §7 (historical internal audit).
Gap 0 (a seal that publishes Clean rows) blocks every spec on the installed `trust` channel. Seal
`150efe4a`, built locally at `~/toolchains/trust-150efe4a`, closes it, but admitting it to the channel is
owner-gated (`docs/PLAN-tests-to-specs-2026-10-15.md`).

## 1. Specs (TSV)

Columns: `id`, `kind`, `home` (the crate where it is proved), `functions`, `statement`,
`subsumes` (every test it retires; a test listed under two specs is primary under the one §2 names),
`difficulty` (the Rust features the proof must handle), `blocking gaps` (audit §7), `merge` (rows in the
home crate's `SPECS.md` that look like the same property; `-` for none).

```tsv
id	kind	home	functions	statement	subsumes	difficulty	blocking gaps	merge
saturated-hue-floor	contract	aterm-conformance	measure::scan::{saturated_hue_degrees,hsv_hue}	∀ rgb:[u8;3], mx=max(r,g,b), mn=min(r,g,b): saturated_hue_degrees(rgb)=Some(d) ⇔ mx ≥ SAT_MIN (60) ∧ mx−mn ≥ SAT_SPREAD (40), and then d = 360·hsv_hue(rgb), where hsv_hue is colorsys.rgb_to_hsv's hue (rem_euclid 1), so 0 ≤ d < 360. The Some/None half is pure u8 arithmetic; the value half needs a float model.	-	u8 max/min chains; f64 division and rem_euclid (E6 refuses floats today)	0,3,6	-
cyan-band	contract	aterm-conformance	measure::scan::{cyan_band_hue,dynamic_metrics}	∀ rgb: cyan_band_hue(rgb)=Some(d) ⇔ saturated_hue_degrees(rgb)=Some(d) ∧ CYAN_HUE_LO ≤ d ≤ CYAN_HUE_HI ∧ (mx−mn)/mx ≥ CYAN_SAT_MIN. ∀ base, frame of equal size: dynamic_metrics(..).cyan_pixels = |{p ∈ Sat : cyan_band_hue(F[p]) is Some}| ≤ ink, and cyan_worst = the first such sample in row-major order that minimises |d−180| (strict improvement only).	src/measure/scan_tests.rs::the_worst_sample_reported_is_the_one_nearest_pure_cyan	f64 comparisons, Option, loop over frame pixels	0,1,3,6,7	-
circular-hue-max	equivalence	aterm-conformance	measure::scan::{max_circular_hue_distance_degrees,circular_hue_distance_degrees}	∀ finite a:[f64] with every aᵢ ∈ [0,360): max_circular_hue_distance_degrees(a) = maxcirc(a), i.e. the sort + partition_point search equals the O(n²) oracle (0 when |a| < 2). Corollary (monotone): b a sub-multiset of a ⇒ maxcirc(b) ≤ maxcirc(a).	src/measure/scan_tests.rs::fast_circular_max_matches_the_exact_oracle_including_wrap	Vec clone + sort_by(total_cmp), chained iterators, partition_point, f64 abs/min/max	0,1,3,5,6,7	-
ink-objects-4-connected	contract	aterm-conformance	measure::scan::ink_objects	∀ w>0, h, sat:SatMap with keys < w·h: ink_objects(w,h,sat) = comp4(keys(sat)) (p±1 only within one row, p±w only within 0..h), listed in ascending order of each component's least key; diagonal-only neighbours never share a component.	-	BTreeSet/BTreeMap, Vec worklist loop, usize div/mod	0,1,5,7	-
row-hue-separation	contract	aterm-conformance	measure::scan::{object_row_hue_separation,banded_row_span,dynamic_metrics}	∀ sat over a w×h frame: object_row_hue_separation = max over O ∈ ink_objects and rows y of maxcirc({sat[p] : p ∈ O, p/w = y}); banded_row_span = max over rows y of maxcirc(hues of the pixels in horizontal runs of length ≥ BAND_MIN_RUN_PX whose columns each hold ≤ BAND_MAX_SLAB_HEIGHT_PX sat pixels); dynamic_metrics(..).row_hue_separation = max of the two; row_hue_peak = max over rows of |HueSet of that row's Sat pixels|. Corollaries: one hue gives 0; pixels in different objects and not in one qualifying slab run contribute no pair.	-	Vec/BTreeMap/BTreeSet loops, slices, sort_by_key, windows(2), f64	0,1,3,5,6,7	-
band-ink-partition	contract	aterm-conformance	measure::scan::{band_ink_partition,band_ink_metrics,count_band_object,bounds}	∀ w,h,sat,cell_h,exclude, cap = cell_h + BAND_OBJECT_ABOVE_CELL_MAX_PX: band_ink_partition = (band, hues, animal) where, per O ∈ ink_objects: if bbox(O) lies inside some box of exclude, animal += |O|; else if count_band_object(O) = Some(k,hs) (O spans ≤ cap rows, or its rows holding ≥ max(6, densest/2) pixels span ≤ cap rows and only those rows are kept; pixels in columns holding ≥ cell_h kept pixels are not counted) add (k,hs); else re-split O without its columns holding ≥ cell_h pixels and add count_band_object of each piece. band_ink_metrics = the first two. Properties: exclude=[] ⇒ animal=0; band+animal ≤ |sat|; an object of height ≤ cap with no cell_h-tall column counts |O|.	src/measure/scan_tests.rs::a_star_on_a_long_band_s_edge_leaves_the_band_lit, src/measure/scan_tests.rs::a_tall_object_whose_band_rows_still_exceed_the_cap_is_dropped, src/measure/scan_tests.rs::a_box_that_holds_the_band_whole_refuses_it, src/measure/scan_tests.rs::a_companion_coat_lends_the_population_arm_nothing	BTreeMap/BTreeSet, Vec, nested loops, i64 boxes, Option early return	0,1,3,5,7	-
hue-set-buckets	contract	aterm-conformance	measure::scan::HueSet::{bucket,of,insert,union_with,len,is_empty,buckets}	HueSet is a set of buckets 0..12 (bit i of a u16): bucket(d) = trunc(d/30) mod 12 for finite d ≥ 0; insert(b) adds b mod 12; union_with is set union; len = cardinality; is_empty ⇔ len = 0; buckets() = members ascending; of(bs) = fold insert. Hence a take-level union of per-frame hues counts distinct buckets.	-	u16 bit ops, f64→u32 cast, Vec collect	0,1,3	-
thin-ribbon-component	contract	aterm-conformance	measure::scan::thin_ribbon_component	∀ w,h,dynamic,cell, min_w = max(6, ⌈3·cell.w/4⌉), min_px = max(8, min_w+2): the result is the Shape-maximal component of dynamic under 8-connectivity plus same-row ±2 bridging with pixels ≥ min_px, width ≥ min_w and height ≤ RIBBON_COMPONENT_MAX_HEIGHT; Shape::NONE if none qualifies.	src/measure/scan_tests.rs::one_hue_thin_highlighter_is_ribbon_presence, src/measure/scan_tests.rs::one_pixel_highlighter_seam_still_has_presence, src/measure/scan_tests.rs::compact_resident_pet_is_not_ribbon_presence, src/measure/scan_tests.rs::subthreshold_horizontal_spark_is_not_ribbon_presence, src/measure/scan_tests.rs::moving_cursor_and_colored_glyphs_are_not_ribbon_presence	BTreeSet pop_first/remove worklist, nested ranges, derived Ord on a struct	0,1,3,5,7	-
attached-ribbon-run	contract	aterm-conformance	measure::scan::attached_ribbon_run	∀ w,h, dynamic ⊆ changed, cell: cursor spans are the maximal runs of ≥ cell.w consecutive x present in changed on every row top..top+cell.h−1 (bottom = top+cell.h−1); on each such bottom row the dynamic xs are split where adjacent xs differ by > 2; the result is the (width,pixels)-maximal piece with width ≥ ATTACHED_RIBBON_MIN_CELLS·cell.w, 4·pixels ≥ 3·width and some span with lo ≤ right+2 ∧ hi+2 ≥ left, else RibbonRun::NONE. The cursor is read from changed (any colour), the run from dynamic only.	src/measure/scan_tests.rs::dense_two_cell_run_on_cursor_bottom_is_attached_ribbon, src/measure/scan_tests.rs::remote_run_on_cursor_bottom_is_not_attached_ribbon, src/measure/scan_tests.rs::a_caret_under_the_colour_floor_still_anchors_the_attached_run, src/measure/scan_tests.rs::a_pale_caret_alone_is_still_nothing, src/measure/scan_tests.rs::a_remote_run_does_not_attach_to_a_pale_caret_either	BTreeMap<usize,BTreeSet>, retain, nested loops	0,1,3,5,7	-
ribbon-lit-predicates	contract	aterm-conformance	measure::scan::{ribbon_present,driven_lit}	∀ m:DynamicMetrics: ribbon_present(m) ⇔ m.ribbon_component.pixels > 0 ∨ m.ribbon_run.pixels > 0; driven_lit(m) ⇔ ribbon_present(m) ∨ m.band_ink ≥ RIBBON_LIT_MIN_PX (so the population arm can only move a frame from dark to lit).	src/measure/scan_tests.rs::geometry_alone_still_lights_a_frame, src/measure/scan_tests.rs::a_young_tall_band_is_lit_though_it_is_not_thin_geometry, src/measure/scan_tests.rs::a_subthreshold_blob_cannot_buy_continuity, src/measure/scan_tests.rs::a_lone_caret_block_is_never_lit	&struct field reads, bool	0,1,2	-
dynamic-metrics-denotation	contract	aterm-conformance	measure::scan::dynamic_metrics	∀ base, frame of equal size, cell, exclude: ink = |Sat|; hues = HueSet of the Sat buckets; ribbon_component = thin_ribbon_component(Sat); ribbon_run = attached_ribbon_run(Sat, Changed); (band_ink, band_hues, animal_ink) = band_ink_partition(Sat, cell.h, exclude); row_hue_* and cyan_* as in row-hue-separation and cyan-band. The row-equality short-cut is sound: an RGBA-equal row contains no Changed pixel.	src/measure/scan_tests.rs::a_young_tall_band_is_lit_though_it_is_not_thin_geometry	nested pixel loops, BTreeSet/BTreeMap building, calls to the classifiers	0,1,2,3,4,5,7	-
cell-new-positive	contract	aterm-conformance	measure::scan::Cell::new	∀ w,h:usize: Cell::new(w,h) = Some(Cell{w,h}) ⇔ w > 0 ∧ h > 0, else None.	-	bool::then_some on a Copy struct	0,1	-
status-row-tokens	contract	aterm-conformance	measure::scan::StatusRow::{value,quoted,bool,uint,nonneg_float,pet_pose}	∀ line, key: value(key) = v for the first whitespace token equal to key++'='++v, else None; quoted(key) = the text after the first occurrence of key++'="' at index 0 or after an ASCII-whitespace byte, up to the next '"'; bool(key) = Ok(true|false) exactly for the values 'true'|'false', else Err{key, raw = value(key)}; uint(key) = Ok(n) ⇔ the value is non-empty, all ASCII digits and fits u64; nonneg_float(key) = Ok(v) ⇔ the value parses as f64, is finite and v ≥ 0; pet_pose = value('pet_pose').	src/measure/scan_tests.rs::status_tokens_are_exact, src/measure/scan_tests.rs::required_status_tokens_fail_closed	&str split_whitespace/strip_prefix/match_indices, String, Result, f64 parse	0,1,3,5,7	-
pet-body-parse	contract	aterm-conformance	measure::scan::StatusRow::pet_body	∀ rows: pet_body() = Ok(None) when value('pet_body') is absent or 'none'; Ok(Some(PixelBox{x0, y0, x1−1, y1−1})) when it is exactly four comma-separated i64 'x0,x1,y0,y1' with x1 > x0 ∧ y1 > y0; Err(StatusError{key:'pet_body', raw}) otherwise (wrong arity, non-integer, empty or reversed).	src/measure/scan_tests.rs::the_emitted_body_uses_exclusive_ends_and_refuses_malformed_geometry	&str split/parse into Vec<i64>, slice pattern, i64 subtraction	0,1,3	-
driven-last-frame	contract	aterm-conformance	measure::scan::driven_last_frame	∀ rows: Err(e) for the first row whose spawns is not a uint, or whose speed is present but not a nonneg_float (both read before either is tested); otherwise Ok(max{i : (i > 0 ∧ spawnsᵢ > spawnsᵢ₋₁) ∨ speedᵢ > 0}), None when that set is empty (absent speed reads as 0).	src/measure/scan_tests.rs::a_skewed_row_is_refused_rather_than_read_as_undriven	loop over a slice of rows, Result early return, Option<u64>	0,1,4,5,7	-
bool-run-lengths	contract	aterm-conformance	measure::scan::{longest_interior_gap,longest_true_run,trailing_true_run}	∀ flags:[bool]: longest_true_run = the longest run of trues (0 if none); trailing_true_run = the length of the true suffix; longest_interior_gap = 0 if there is no true, else the longest run of falses strictly between the first and the last true.	src/measure/scan_tests.rs::only_interior_rainbow_blackouts_count	slice iteration, position/rposition, usize counters	0,1,5,7	-
rainbow-summary-over-distinct-samples	contract	aterm-conformance	measure::scan::{distinct_samples,sustained_rainbow_bounds,longest_sustained_interior_gap,rainbow_summary}	requires |flags| = |duplicates| (else panic). distinct_samples = [(i, flags[i]) | ¬duplicates[i]]; with E its verdicts, sustained_rainbow_bounds = Some(original index of the first i with E[i] ∧ E[i+1], original index of the last i with E[i−1] ∧ E[i]) when first < last, else None; longest_sustained_interior_gap = longest_interior_gap of E restricted to those bounds (0 if None); rainbow_summary = (#true in E, longest_true_run(E), longest_sustained_interior_gap). Hence a duplicate frame can neither add a visible frame nor lengthen a run or a gap, and a lone glint cannot open the mature window.	src/measure/scan_tests.rs::duplicate_capture_cannot_manufacture_rainbow_maturity, src/measure/scan_tests.rs::duplicate_capture_does_not_lengthen_an_interior_blackout, src/measure/scan_tests.rs::only_interior_rainbow_blackouts_count	Vec of tuples, zip/enumerate/filter, assert_eq	0,1,5,7	-
companion-witness-floor	contract	aterm-conformance	measure::scan::{compact_pet_witness_frame,companion_witness_floor,resting_pose,resting_pet_min_width}	∀ n,c: isolating(n,c) ⇔ n = 0 ∨ n ≥ c.saturating_sub(3); companion_witness_floor(pet,n,c,cat) = None unless isolating(n,c), else Some(COMPACT_PET_MIN_H) if pet, else Some(CAT_STILL_MIN_H) if cat, else None. ∀ pose: resting_pose(pose) ⇔ pose is Some and non-empty and the first of the prefixes 'pet_dog_', 'pet_' it starts with leaves a stem starting with one of RESTING_POSE_STEMS ('sleep', 'run_0'). resting_pet_min_width(w) = 2w+1.	src/measure/scan_tests.rs::two_drawn_companions_are_two_components	usize saturating arithmetic, &str strip_prefix/starts_with, Option	0,1,3	-
tall-component-records	contract	aterm-conformance	measure::scan::{tall_component_records,tall_components,largest_tall_component,frame_background,clamp_to}	requires frame width, height > 0 (§5). ∀ frame, bg, min_h, region: let Fg = {p ∈ region (inclusive, clamped to the frame) : some channel k has |pxₖ − bgₖ| ≥ FOREGROUND_DELTA}; the result is the comp4(Fg) components with bbox width ≥ COMP_MIN_W, height ≥ min_h and size ≥ COMP_MIN_PIXELS, each with its inclusive bbox, stably sorted by Shape descending. tall_components = their shapes; frame_background = the majority colour of the four corners and the centre (first wins ties); largest_tall_component(f) = the first of tall_components(f, frame_background(f), COMP_MIN_H), else NONE. Saturation is not required (charcoal counts).	src/measure/scan_tests.rs::cursor_row_cannot_impersonate_a_companion, src/measure/scan_tests.rs::a_small_whole_body_is_a_companion_witness, src/measure/scan_tests.rs::two_drawn_companions_are_two_components, src/measure/scan_tests.rs::wide_one_row_ribbon_or_text_cannot_impersonate_a_companion, src/measure/scan_tests.rs::a_charcoal_companion_is_not_rejected_for_lacking_saturation	Vec<bool> masks, worklist loop, sort_by_key(Reverse), i64↔usize conversions	0,1,3,5,7	-
companion-component-selection	contract	aterm-conformance	measure::scan::{companion_component,companion_component_box,companion_record}	∀ args: companion_record = first(strict) if non-empty (strict defaults to tall_component_records(frame, bg, COMP_MIN_H, None)); else the first record at companion_witness_floor(pet,n,c,cat) when that is Some; else None. companion_component = its shape or NONE and companion_component_box = its bbox, so the two always describe the same component.	src/measure/scan_tests.rs::compact_pet_is_allowed_only_in_isolated_endpoint_witnesses, src/measure/scan_tests.rs::the_box_sibling_frames_exactly_the_companion_component, src/measure/scan_tests.rs::wide_one_row_ribbon_or_text_cannot_impersonate_a_companion	Option<&[Component]>, owned/borrowed local, calls	0,1,3,4	-
located-pet-component	contract	aterm-conformance	measure::scan::{located_pet_component,resting_pet_component}	∀ args: body = None ⇒ NONE. Else floor = COMPACT_PET_MIN_H if isolating(n,c) else COMP_MIN_H; result = the first record of tall_component_records(frame, bg, floor, Some(body)) if any; else if cell_w = Some(w) ∧ floor > COMPACT_PET_MIN_H ∧ resting_pose(pose) then resting_pet_component(frame, bg, body, w) = the first record at COMPACT_PET_MIN_H inside body with width ≥ 2w+1 whose bbox touches none of body.x0, body.x1, body.y0; else NONE. Pixels outside body never contribute.	src/measure/scan_tests.rs::body_identity_keeps_strict_and_isolated_morphology_floors, src/measure/scan_tests.rs::pixels_outside_the_emitted_rectangle_cannot_supply_missing_height, src/measure/scan_tests.rs::the_pose_claim_selects_the_floor_inside_the_emitted_rect, src/measure/scan_tests.rs::the_positive_control_is_a_sleeper_shaped_body_on_the_cell_floor, src/measure/scan_tests.rs::a_glyph_pair_inside_the_cell_is_not_the_sleeper, src/measure/scan_tests.rs::two_touching_cells_are_not_the_sleeper_and_the_edge_is_where_it_is_declared, src/measure/scan_tests.rs::a_row_running_through_the_cell_is_the_row_s_not_the_sleeper_s	Option, iterator find, calls into tall_component_records	0,1,3,4	-
driven-window-darkness	contract	aterm-conformance	measure::scan::driven_window_darkness	∀ index, driven_ms, names, flags, duplicates: NOT_MEASURED if index = None, driven_ms = None, fewer than 2 names are stamped, or fewer than 2 stamped frames t satisfy anchor + lo·1000 ≤ t ≤ anchor + hi·1000 + DRIVEN_GRACE_US (anchor = covered_from, else the first stamp). Otherwise, with nominal = the upper median of consecutive stamp deltas, resolved verdicts (a duplicate inherits the previous resolved verdict, seeded false), onset = the time of the first adjacent pair of DISTINCT lit samples and charge_from = onset or the window start: dark_us = max over maximal dark runs at or after charge_from of (last − first + nominal), where the run reaching the window's end is reduced by EXHALE_CLOSE_ALLOWANCE_US only when onset exists, floored at 0; mature_lead_us = charge_from − window start; frames = the window's size; sampling_hole_us = the largest gap in the sorted union of window stamps and ticks inside [window start, window end].	src/measure/scan_tests.rs::a_sampling_hole_between_lit_frames_is_never_charged_as_dark, src/measure/scan_tests.rs::a_static_windows_frame_gap_is_not_a_hole_when_the_ledger_ticked, src/measure/scan_tests.rs::a_starved_ledger_still_reads_as_a_hole, src/measure/scan_tests.rs::a_render_stall_under_a_live_loop_is_still_a_hole, src/measure/scan_tests.rs::a_stall_beside_one_dark_frame_charges_only_that_frame, src/measure/scan_tests.rs::a_duplicate_cannot_open_the_mature_window, src/measure/scan_tests.rs::darkness_outside_the_driven_window_is_not_the_rows_business, src/measure/scan_tests.rs::no_bracket_reports_not_measured_rather_than_zero, src/measure/scan_tests.rs::no_index_reports_not_measured	BTreeMap<String,i64> lookups, Vec sort_unstable, windows(2), BTreeSet union, unchecked i64 arithmetic	0,1,3,5,7	-
recording-index-parse	contract	aterm-conformance	measure::scan::RecordingIndex::parse	∀ bytes: parse = Err unless bytes are a JSON object (per aterm_json); then stamps = {row.file ↦ row.t_us : row ∈ frames with file a string and t_us an i64}, covered_from = /meta/covered_us/0 as i64, ticks = the i64 elements of 'ticks' in order (non-integers dropped).	src/measure/scan_tests.rs::the_recording_index_parses_stamps_coverage_and_integer_ticks	cross-crate JSON Value model, String, BTreeMap, iterators	0,1,3,12	-
scan-frames-refusals	contract	aterm-conformance	measure::scan::scan_frames	∀ names, frames, ctx: Err(NoFrames) if names is empty or frames runs out early; Err(StatusCount{rows,frames}) if ctx.statuses has a length other than |names|; Err(MalformedStatus) if driven_last_frame or a per-frame pet_active/cat_active/ribbon_hue_bands/pet_body read fails; Err(SizeMismatch) for the first frame whose (width,height) differs from frame 0's; a frame Err is propagated; Ok otherwise.	src/measure/scan_tests.rs::a_take_with_mismatched_rows_or_sizes_is_refused	impl Iterator<Item=Result<..>> generic, ? propagation	0,1,4,7,11	-
scan-claim-bound-darkness	contract	aterm-conformance	measure::scan::scan_frames	∀ paired takes (statuses = Some): ribbon_claimed = #{i : ribbon_hue_bandsᵢ ≥ 4}; a claimed frame i is bound iff i ≤ driven_last − RIBBON_CLOSE_GUARD_FRAMES (driven_last = −1 when undriven), else exhale; ribbon_bound = #bound; a bound frame is a dark candidate iff i > 0 ∧ metricsᵢ.band_ink < RIBBON_LIT_MIN_PX (frame 0 never); ribbon_dark = the summed length of maximal dark-candidate runs of length ≥ 2; when ribbon_bound > 0, rainbow_gap = longest_interior_gap of the bound frames' (i > 0) lit verdicts and ribbon_window_hues = |union of their band_hues|. Frame 0 and a duplicate carry the previous frame's metrics.	src/measure/scan_tests.rs::a_claim_on_the_baseline_frame_is_never_charged, src/measure/scan_tests.rs::the_exemption_stops_at_frame_zero, src/measure/scan_tests.rs::a_blanked_layer_mid_take_charges_its_full_run, src/measure/scan_tests.rs::the_composed_fused_take_is_not_charged	large fold with many &mut locals, Vec pushes, struct update, i64/usize casts	0,1,2,3,5,7	-
scan-companion-accounting	contract	aterm-conformance	measure::scan::scan_frames	∀ paired takes, per frame i: located = pet ∧ pet_body present; component = located_pet_component(.., claimed body, pose, cell.w) when located, else companion_component over the strict list; the exclusion box handed to dynamic_metrics is the claimed body when located and a component was found, else the companion's bbox, and is dropped when fused (= ¬located ∧ component.width > COMPANION_ALONE_MAX_WIDTH_CELL_HEIGHTS·cell.h); pet_missing/cat_missing count claimed frames with no component; companion_frames counts frames with one; companion_stack counts isolating frames with ≥ 2 strict components; companion_fused counts fused frames; notes record CompactPetWitness (height < COMP_MIN_H), RestingPoseWitness (located and non-isolating), CompanionFused, CompanionMissing, CompanionStack; pet_final_run, cat_final_run, cat_gap and cat_last_present are the run statistics of per-frame (claim ∧ component) visibility. An unclaimed frame excludes nothing.	src/measure/scan_tests.rs::a_coat_fragment_cannot_stand_in_for_a_missing_ribbon, src/measure/scan_tests.rs::the_emitted_body_identity_separates_a_pet_from_a_larger_typed_row, src/measure/scan_tests.rs::a_body_claim_requires_pixels_at_that_body, src/measure/scan_tests.rs::known_body_geometry_excludes_coat_even_when_fused_with_a_band, src/measure/scan_tests.rs::body_identity_does_not_hide_a_second_animal_outside_the_rectangle, src/measure/scan_tests.rs::a_band_crossing_the_animal_s_box_keeps_the_frame_lit, src/measure/scan_tests.rs::an_unclaimed_plane_excludes_nothing, src/measure/scan_tests.rs::a_fused_companion_s_box_is_the_row_s_and_excludes_nothing, src/measure/scan_tests.rs::the_sleeper_is_located_on_the_mid_take_frames_it_is_claimed_on, src/measure/scan_tests.rs::the_pose_claim_is_load_bearing_on_the_wired_scanner, src/measure/scan_tests.rs::the_runner_is_located_on_the_mid_take_frames_it_is_claimed_on, src/measure/scan_tests.rs::the_runner_s_pose_claim_is_load_bearing	large fold, String and Vec<FrameNote> enum payloads, Option<PixelBox>	0,1,2,3,5,7	-
scan-ink-and-recorded-continuity	contract	aterm-conformance	measure::scan::{scan_frames,scan_dir,take_frames}, measure::frame::Frame::{decode_png,load}	∀ takes: duplicateᵢ ⇔ i > 0 ∧ frameᵢ = frameᵢ₋₁ (RGBA equality); total_ink = Σ metricsᵢ.ink over non-duplicate i > 0; union_hues = |union of their hues|; (best_ink, best_hues) = the lexicographic max of (ink, |hues|); (rainbow_frames, rainbow_run, rainbow_gap) = rainbow_summary(ribbon_present flags, duplicates) (gap overridden by the claim-bound rule); driven = driven_window_darkness(index, driven_ms, names, driven_lit flags, duplicates) when an index is given, else NOT_MEASURED. scan_dir(dir, ctx) = scan_frames over the *.png entries of dir sorted by path, each decoded by Frame::decode_png (aterm_png RGBA8); Err(NoFrames) for an unreadable or png-less dir.	src/measure/scan_tests.rs::a_mono_hue_band_is_continuous_however_dark_the_hue_witness, src/measure/scan_tests.rs::a_vanished_band_is_still_a_blackout_through_the_same_wiring, src/measure/scan_tests.rs::a_take_scans_from_png_files_through_the_one_decoder	fs read_dir + read (effect), PNG decode (cross-crate), large fold	0,1,3,5,7,8,12	-
paint-judge-denotation	contract	aterm-conformance	measure::paint::{judge,ink_holds,continuity_holds}, measure::paint::Ledger::{trail_quiet,companions_off,style_bound}, measure::paint::Style::{binding,bound_by}	∀ row, ledger, s: judge = Err if s.frames < MIN_FRAMES, or if the expectation evaluates ink_holds on a video row with driven.dark_us < 0 once the floors hold. Otherwise judge = Pass ⇔ pass(row.expect) ∧ ledger.style_bound(row.style, s.frames) ∧ companion_ok(row.companion, s), where ink_holds = floors (total_ink ≥ MIN_INK, union_hues ≥ MIN_HUES, best_ink ≥ MIN_BEST_INK, rainbow_frames ≥ 2, rainbow_run ≥ 2) ∧ continuity (video: 2·dark_us ≤ 3·nominal_dt_us ∧ mature_lead_us ≤ MATURE_LEAD_MAX_US; burst: rainbow_gap ≤ 1) ∧ ribbon_dark = 0 ∧ (ribbon_bound < 2 ∨ ribbon_window_hues ≥ 4); pass(Ink) = ink_holds ∧ (status_frames = 0 ∨ ribbon_bound ≥ 2) ∧ fx_under_frames ≥ 2; pass(Quiet) = ¬ink_holds; pass(Dark) = total_ink = 0; pass(TrailDark) = trail_quiet ∧ ribbon_claimed = 0 ∧ ribbon_dark = 0; pass(EffectOff) and companion_ok as coded. Corollaries: Ink and Quiet never both Pass on one scan; a companion obligation can only turn a Pass into a Fail.	src/measure/paint_tests.rs::a_healthy_burst_is_ink_and_quiet_is_its_exact_negation, src/measure/paint_tests.rs::the_coverage_floor_binds_ink_but_never_buys_the_quiet_control, src/measure/paint_tests.rs::ink_needs_the_compose_side_witness, src/measure/paint_tests.rs::a_driven_claim_over_an_unlit_raster_is_red, src/measure/paint_tests.rs::recorded_continuity_tolerates_one_dark_frame_and_refuses_two, src/measure/paint_tests.rs::a_recorded_take_that_lost_its_bracket_decides_nothing, src/measure/paint_tests.rs::a_near_empty_take_is_never_a_dark_pass, src/measure/paint_tests.rs::trail_dark_reads_the_engine_ledger_and_refuses_without_one, src/measure/paint_tests.rs::effect_off_needs_typed_pixels_a_quiet_ledger_and_no_witness, src/measure/paint_tests.rs::a_recorded_effect_off_window_must_read_dead, src/measure/paint_tests.rs::a_spelling_that_resolved_elsewhere_is_red, src/measure/paint_tests.rs::companion_obligations_ride_on_top_of_ink	struct reads, enum matches, i64/usize arithmetic (2·dark_us unchecked), Vec<StatusRow> all()	0,1,2,3,5	-
paint-row-validate	contract	aterm-conformance	measure::paint::PaintRow::validate	∀ rows: validate() = Err ⇔ (expect = EffectOff ∧ (style ≠ Off ∨ keys empty ∨ companion is Some)) ∨ (companion = Some(c) ∧ (capture is video ∨ scene ≠ Prompt ∨ ¬((c = Pet ∧ style ∈ {Default, Classic}) ∨ (c = Cat ∧ style = Flying)))).	src/measure/paint_tests.rs::self_contradicting_rows_are_refused	enum matches on a Copy struct, &str is_empty	0,1,2,3	-
paint-record-secs	contract	aterm-conformance	measure::paint::PaintRow::{keys,record_secs}	∀ rows, n = |keys()| (the non-empty comma-separated items): record_secs = 5 if n = 0, else max(5, ⌈0.7 + 0.12(n−1) + 0.15n + 1.0⌉).	-	&str split/filter/collect, f64 ceil and casts	0,1,3,6	-
starvation-classify	contract	aterm-conformance	measure::starvation::classify, measure::starvation::Starvation::{label,evidence}	∀ i:SchedInputs (requires 10·wall_us, 9·budget_us and 2·nominal_dt_us to fit u64; §5): classify(i) is the first of NoTake (¬produced_take), NoSamples (ticks = 0), Descheduled{late_max_us, threshold, qos} (late_max_us ≥ late_threshold_us), CadenceBlown (nominal_dt_us > 0 ∧ sampling_hole_us > 2·nominal_dt_us), BudgetPressure (budget_us > 0 ∧ 10·wall_us ≥ 9·budget_us), else KeptCadence{late_max_us, threshold, ticks}. label: NoTake, NoSamples ↦ Unknown; Descheduled, CadenceBlown, BudgetPressure ↦ Yes; KeptCadence ↦ No. evidence = Sound ⇔ label = No.	src/measure/starvation_tests.rs::a_take_that_got_its_time_disowns_only_driver_starvation, src/measure/starvation_tests.rs::no_take_at_all_is_unknown_not_a_verdict_about_paint, src/measure/starvation_tests.rs::a_descheduled_driver_is_named_and_the_take_is_disowned, src/measure/starvation_tests.rs::a_blown_capture_cadence_is_starvation_even_when_the_driver_ran, src/measure/starvation_tests.rs::a_take_racing_its_budget_is_not_evidence, src/measure/starvation_tests.rs::no_samples_is_unknown_and_never_a_clean_bill, src/measure/starvation_tests.rs::a_clamped_tier_is_named_and_repeating_its_take_is_still_zero_evidence	u64 comparisons and multiplication, enum construction with payloads	0,1,3	-
paint-verdict-monotone	contract	aterm-conformance	measure::starvation::paint_verdict	∀ shape, evidence: paint_verdict(Fail, e) = Fail; paint_verdict(Pass, Sound) = Pass; paint_verdict(Pass, Unproved) = Unproved. Hence starvation can turn a Pass into Unproved but never change a Fail, and the result is a function of one take only.	src/measure/starvation_tests.rs::a_starved_pass_is_unproved_and_a_sound_take_is_decided_by_its_pixels, src/measure/starvation_tests.rs::a_clamped_tier_is_named_and_repeating_its_take_is_still_zero_evidence	match on two fieldless enums	0,1,3	-
qos-table	table-property	aterm-conformance	measure::starvation::Qos::{from_raw,clamped,name}	from_raw is exactly {0x21 ↦ UserInteractive, 0x19 ↦ UserInitiated, 0x15 ↦ Default, 0x11 ↦ Utility, 0x09 ↦ Background, 0x00 ↦ Unspecified} and None on every other u32; clamped(q) ⇔ q ∈ {Utility, Background}.	src/measure/starvation_tests.rs::a_clamped_tier_is_named_and_repeating_its_take_is_still_zero_evidence	match on u32 literals, fieldless enum	0,3	-
sched-reading-book	contract	aterm-conformance	measure::starvation::SchedReading::{book,deciding}	∀ r, deadline, woke, in_window (requires the running totals not to overflow; §5): with late = micros(woke.saturating_sub(deadline)) and at = micros(deadline), book increments ticks, adds late to late_total_us and sets (late_max_us, late_max_at_us) = (late, at) iff late > late_max_us; when in_window it also increments win_ticks, sets win_first_at_us if unset, sets win_last_at_us = at, adds late to win_late_total_us and sets win_late_max_us = max(win_late_max_us, late); nothing else changes. deciding() = (win_ticks, win_late_max_us) if win_ticks > 0 else (ticks, late_max_us).	src/measure/starvation_tests.rs::lateness_is_measured_from_the_fixed_deadline, src/measure/starvation_tests.rs::the_capture_window_is_booked_apart_from_the_life	&mut self on a struct, Duration saturating_sub, u128→u64 try_from, Option get_or_insert	0,1,2,3	-
sampler-lifecycle	other	aterm-conformance	measure::starvation::Sampler::{start,open_window,close_window,finish}, measure::starvation::sample	For every Sampler::start(tick) followed after elapsed time T by finish(): the reading has ticks ≥ 1, tick_us = micros(max(tick, 1 ms)), win_ticks ≤ ticks, win_late_max_us ≤ late_max_us and sampled_us ≥ T; every booked tick satisfies sched-reading-book with deadlines tick·n from a fixed origin; a second finish() returns SchedReading::default().	-	std::thread spawn/join, Arc<AtomicBool>, mpsc channel, Instant, sleep, Drop	0,2,8,13	-
spin-judge	contract	aterm-conformance	measure::spin::judge	∀ row, arms, timer_wakes: judge = Pass ⇔ arms ≤ row.max_arms ∧ (row.max_timer_wakes = None ∨ (timer_wakes = Some(n) ∧ n ≤ max)); an unreported wake count under a bound is never a pass.	src/measure/spin.rs::a_healthy_window_passes_and_either_witness_fails_it, src/measure/spin.rs::a_bounded_wake_count_that_was_not_reported_is_not_a_pass	Option match on a Copy struct field, u64 compare	0,1,2	-
budget-await-event	contract	aterm-conformance	measure::instance::Budget::{from_now,left,await_event}	∀ budgets b and probes: left() = None ⇔ now ≥ ends; await_event(what, probe) calls probe at least once; it returns Ok(v) for the first call returning Some(v) and calls probe no more after it; it returns Err only after a probe returned None at an instant ≥ min(start + MUST_HAPPEN, ends), and that Err is b.spent(..) iff b.left() = None then. Corollary: a spent budget asks the probe exactly once.	src/measure/instance.rs::a_spent_budget_gives_up_at_once_but_still_asks_the_probe_once, src/measure/instance.rs::a_live_budget_leaves_the_moment_the_event_lands	generic FnMut closure, Instant clock, thread::sleep loop, String formatting	0,5,7,8,11,13	-
ctl-call-spent-budget	contract	aterm-conformance	measure::instance::Ctl::{call,ok}	∀ clients: budget.left() = None ⇒ call(args) spawns nothing and returns Reply{code: None, stdout: '', stderr: spent(..)}; otherwise it runs `bin ctl --sock S --timeout min(left_secs+1, CTL_TIMEOUT_SECS) args..` with a cleared environment and returns its exit code and output; ok(args) = Ok(first stdout line) ⇔ call(args).code = Some(0). The spent-budget half is pure and can be proved first.	src/measure/instance.rs::a_client_whose_budget_is_spent_runs_nothing	process spawn (effect), String, Result	0,1,8	-
vt-print-advance	contract	aterm-grid, aterm-core	aterm_core::terminal::Terminal::process (print path), aterm_grid::Grid::write_char	∀ T ∈ Reach with Cur = (r,c), ¬PW, IRM reset and GL = ASCII: print of a narrow g writes g at (r,c) and moves to (r,c+1), or sets PW at MaxCol(r); a wide char occupies (r,c),(r,c+1) and advances by 2. SGR changes only the pen, never cell characters or the cursor. Plain text from fresh lands on row 0 with the cursor after it.	tests/conformance.rs::plain_text_lands_on_the_top_row, tests/conformance.rs::sgr_interleaved_attrs_and_color_keep_text, tests/vt_categories.rs::wide_char_advances_cursor_by_two	&mut self on Terminal (Vec<Row> grid, modes struct), parser loop, generic sink	0,1,2,3,4,5,7,12	aterm-grid write-char-denotation; aterm-core print-cell-placement
pending-wrap-transitions	contract	aterm-grid, aterm-core	aterm_grid::Grid::{write_char (pending-wrap arm),tab,tab_margin,line_feed,line_feed_margined,scroll_region_up,scroll_region_down,set_pending_wrap,clear_pending_wrap}; aterm-core DECSET/DECRST 7, SU/SD, CR/BS/CUx/CUP	PW is independent of AW. A print at col M = MaxCol(row) sets PW' = true and col' = M, whatever AW is. A print with PW set: if AW, CR + Index and then the char at col 0 (or HM.left under LR), scrolling the region at its bottom margin; if ¬AW, PW is cleared, the char overwrites col M and the cursor stays. HT/CHT, SU, SD, ?7h and ?7l preserve PW (?7/SU/SD also preserve the cursor); HT at col M with PW set keeps col M. LF/Index (moving or scrolling), RI, CR, BS, CUx/CUP and every erase clear PW without moving to the next row.	tests/conformance.rs::autowrap_wraps_at_the_right_margin, tests/vt_categories2.rs::pending_wrap_is_cancelled_by_cr, tests/xterm_differential_regressions.rs::ht_preserves_pending_wrap_next_printable_wraps, tests/xterm_differential_regressions.rs::ht_at_pending_wrap_keeps_cursor_at_margin, tests/xterm_differential_regressions.rs::ht_ride_to_last_stop_matches_xterm_layout, tests/xterm_differential_regressions.rs::scrolling_lf_clears_pending_wrap, tests/xterm_differential_regressions.rs::moving_lf_clears_pending_wrap, tests/xterm_differential_regressions.rs::su_preserves_pending_wrap, tests/xterm_differential_regressions.rs::margin_fill_with_decawm_off_arms_wrap_for_later, tests/xterm_differential_regressions.rs::margin_fill_with_decawm_off_overstrikes_without_wrap, tests/xterm_differential_regressions.rs::decawm_reset_does_not_clear_armed_wrap	&mut self; a per-operation frame condition on one bool across ~20 grid ops (two-state x'); the print fast paths must agree with write_char on PW	0,1,2,3,4,5,7,12	aterm-grid pending-wrap-motion-table, autowrap-deferred; aterm-core erase-resets-pending-wrap
vt-c0-cursor-controls	contract	aterm-grid, aterm-core	aterm_core::terminal::Terminal::process (C0 execute handlers), aterm_grid::Grid::{carriage_return,line_feed,backspace,tab}	∀ T ∈ Reach: CR sets col := 0 (HM.left under LR), row kept; LF moves to row+1, Index at Reg.bottom (index-and-scroll-region), and keeps the column when LNM is reset; BS sets col := max(col−1, 0); HT is tab-stop-motion (default stops every 8, clamped to MaxCol). CR LF therefore lands at (r+1, 0).	tests/conformance.rs::crlf_moves_to_next_row_col0, tests/conformance.rs::carriage_return_returns_to_col0_overwriting, tests/conformance.rs::backspace_moves_cursor_left, tests/conformance.rs::tab_advances_to_next_8col_stop	&mut self on Terminal, parser loop, grid scrolling	0,1,2,3,4,5,7,12	aterm-grid carriage-return, line-feed, ht-next-stop
vt-cursor-csi-motion	contract	aterm-grid, aterm-core	aterm_core::terminal::Terminal::process (CUP/CUU/CUD/CUF/CUB handlers), aterm_grid::Grid::{set_cursor,cursor_up,cursor_down,cursor_forward,cursor_backward}	∀ T ∈ Reach and params: CUP(Pr,Pc) sets Cur = (min(max(Pr,1)−1 + o, bot), min(max(Pc,1)−1, C−1)) where o = Reg.top and bot = Reg.bottom under OM, o = 0 and bot = R−1 otherwise; CUU/CUD/CUF/CUB move by max(n,1), clamped to the screen (and to the margins when the cursor is inside them).	tests/conformance.rs::cup_positions_cursor_1based, tests/conformance.rs::cursor_up_down_forward_back, tests/vt_categories2.rs::decom_origin_mode_homes_into_scroll_region	&mut self on Terminal, u16 saturating arithmetic, params slice	0,1,2,3,4,5,7,12	aterm-grid cursor-set-clamp, cursor-vertical-margins; aterm-core motion-counts-columns
vt-sgr-pen-algebra	contract	aterm-core	aterm_core::terminal::Terminal::process (SGR handler, flags-only and colour fast paths)	∀ SGR parameter lists a, b: Sty after CSI a m CSI b m = Sty after CSI a;b m (the fast paths compose like the generic path); lists touching disjoint fields commute; each attribute's reset code (22 bold/faint, 24 underline, 25 blink, 27 inverse, …) clears exactly its field, so setting then resetting every attribute yields the default pen; SGR 0 is the default pen. Observed through style_fingerprint().	tests/conformance.rs::sgr_bold_then_color_equals_color_then_bold, tests/conformance.rs::sgr_attr_reset_returns_to_default_style	&mut self on Terminal, SGR param loop, packed colour/flag bit ops	0,1,2,3,4,5,7,12	aterm-core sgr-denotation
vt-erase	contract	aterm-grid, aterm-core	aterm-core ED/EL handlers, aterm_grid::Grid::{erase_screen,erase_line,erase_to_end_of_screen,erase_from_start_of_screen}	With Cur = (r,c): EL0 blanks (r, c..C−1); EL1 blanks (r, 0..=c); EL2 blanks row r; ED0 blanks from (r,c) to the end of the screen; ED1 blanks rows 0..r−1 and (r, 0..=c); ED2 blanks every visible cell. Blank = BCE. Every other cell and the cursor are unchanged, and PW is cleared. Line size: an erase that blanks a complete row (ED2, and ED0/ED1 on the rows they erase whole) sets that row's LS to SingleWidth; EL0/1/2 blank characters only and keep LS.	tests/conformance.rs::el0_erases_from_cursor_to_end_of_line, tests/conformance.rs::ed2_erases_the_whole_screen, tests/vt_categories.rs::el1_erases_from_start_to_cursor, tests/vt_categories.rs::el2_erases_the_whole_line, tests/vt_categories.rs::ed1_erases_above_the_cursor, tests/vt_categories3.rs::ed2_erases_complete_lines_so_a_decdwl_row_comes_back_single_width, tests/vt_categories3.rs::el2_leaves_a_decdwl_row_double_because_xterm_does	&mut self over Vec rows; loops over row ranges; enum field update; any_double_width cache coherence	0,1,2,3,4,5,7,12	aterm-grid el-span, ed-span, ed-line-attributes, erase-pending-wrap-table
vt-insert-delete	contract	aterm-grid, aterm-core	aterm-core ICH/DCH/IL/DL handlers and the IRM print path, aterm_grid::Grid::{insert_chars,delete_chars,insert_lines,delete_lines}	∀ T ∈ Reach and n ≥ 1 (0 reads as 1): ICH n shifts row r's cells c.. right by n (dropping cells past the right margin) and blanks c..c+n; DCH n shifts c+n.. left by n and blanks the vacated tail; ICH and DCH leave the cursor unchanged; IL n / DL n insert/delete n lines at row r within Reg, lines outside it unchanged; with IRM set, a print inserts at the cursor (ICH 1 then print).	tests/vt_categories.rs::ich_inserts_blanks_shifting_right, tests/vt_categories.rs::dch_deletes_char_shifting_left, tests/vt_categories.rs::il_inserts_a_blank_line, tests/vt_categories.rs::dl_deletes_a_line, tests/vt_categories2.rs::irm_insert_mode_shifts_right	&mut self on Terminal, Vec<Cell> shifts, row rotation	0,1,2,3,4,5,7,12	aterm-grid ich-dch-shift, il-dl, il-dl-margined
index-and-scroll-region	contract	aterm-grid, aterm-core	aterm_grid::Grid::{line_feed,line_feed_margined,reverse_index,scroll_region_up,scroll_region_down}; LF/IND/RI dispatch; write_char autowrap (CR+Index)	Let (t,b) = Reg and r = Cur.row. Index: if r < b then r' = r+1 and no cell changes; if r = b then rows t..b−1 := old rows t+1..b in every channel (glyph, flags, truecolour, extras), row b := BCE, and rows outside [t,b] are unchanged; if r > b then r' = min(r+1, R−1) and no cell changes, so on the last screen row below the region Index is the identity on cells and cursor. Autowrap is CR then Index (so it overwrites the bottom row in place there). RevIndex mirrors Index (at r = t it scrolls t..=b down, row b lost, row t BCE). Scrolling never touches CS, Sty or any mode, so a locking shift and the pen stay in force for later output.	tests/conformance.rs::newline_at_bottom_scrolls, tests/vt_categories.rs::scroll_region_index_scrolls_only_the_region, tests/vt_categories2.rs::ri_at_top_scrolls_down, tests/vt_categories3.rs::so_shift_and_truecolor_survive_a_scroll_region_scroll, tests/xterm_differential_regressions.rs::autowrap_below_scroll_region_at_bottom_row_does_not_scroll, tests/xterm_differential_regressions.rs::lf_below_scroll_region_at_bottom_row_does_not_scroll	&mut self on Grid; ring row rotation; extras rekeying; BCE template; Ordering match; u16 saturating ops; scrollback push	0,1,2,3,4,5,7,12	aterm-grid line-feed, reverse-line-feed, scroll-region-shift
decstbm-validity	contract	aterm-grid, aterm-core	aterm-core CsiHandler::SetScrollRegion, aterm_grid::Grid::set_scroll_region	∀ T and params p: top = max(p0 or 1, 1) − 1; bot = (p1 absent or 0 ? R : p1) − 1 (saturating); mb = R−1. If top < min(bot, mb): Reg' = (top, min(bot, mb)), the cursor homes ((Reg.top, HM.left or 0) under OM, else (0,0)) and PW' = false. Otherwise T·CSI p r ≡ T: margins, cursor and the previous region are all unchanged. Instances on 24 rows: 10;10r, 20;5r, 28r and 114r are no-ops; 4;14r homes.	tests/xterm_differential_regressions.rs::invalid_decstbm_keeps_previous_region, tests/xterm_differential_regressions.rs::valid_decstbm_still_sets_region_and_homes	&[u16] first/get; Option map/unwrap_or; u16 max/saturating_sub/min; whole-state frame condition for the no-op clause (the scalar validity predicate can be factored into a pure fn)	0,1,2,3,6	aterm-grid region-and-margin-setters
decsc-decrc	contract	aterm-core	aterm-core CursorStateHandler::{save_cursor_state,restore_cursor_state}, restore_cursor_snapshot, ESC 7 / ESC 8 dispatch	DECSC: Slot(Act)' = Some{cursor, style, origin_mode, charset, pending_wrap, underline_color}. DECRC with Slot(Act) = Some(s): OM' = s.origin_mode, Sty' = s.style, CS' = s.charset, underline' = s.underline_color, Cur' = s.cursor (clamped to Reg, and to HM under LR, when s.origin_mode), PW' = s.pending_wrap ∧ col' ≥ MaxCol(row'); the slot is kept. DECRC with Slot(Act) = None: OM' = false, Sty' = default, CS' = default, underline' = None, Cur' = (0,0). In both cases AW' = AW (DECAWM is never saved, restored or reset) and Reg/HM are unchanged. Corollary: ∀ y without ESC 7, RIS or a screen switch, T·ESC 7·y·ESC 8 restores the Cur, pen, OM and CS of T, while the cell effects of y persist.	tests/vt_categories.rs::decsc_decrc_save_and_restore_cursor, tests/vt_categories3.rs::decsc_decrc_saves_and_restores_charset, tests/xterm_differential_regressions.rs::unsaved_decrc_leaves_decawm_unchanged, tests/xterm_differential_regressions.rs::saved_decrc_does_not_restore_decawm, tests/xterm_differential_regressions.rs::unsaved_decrc_still_resets_origin_and_homes	&mut self over several borrowed handler-view fields; Option<Copy struct>; u16 clamp; two-state reasoning	0,1,2,3,4,12	-
alt-screen-trio-model	refinement	aterm-core	aterm-core TerminalHandler::{enter_alternate_screen_raw,enter_alternate_screen,exit_alternate_screen,exit_alternate_screen_1047}, DECSET/DECRST 47/1047/1049 dispatch, CursorStateHandler::{save_cursor_state,restore_cursor_state}	Let P(T) = ⟨Act, Main, Alt, Cur, Slot(main), Slot(alt)⟩. ∀ T ∈ Reach: 47h: if Act = Main then Act' = Alt, Alt kept (never cleared), Main and Cur kept; otherwise identity. 47l: Act' = Main, Main as it was before entry, Alt and Cur kept. 1047h = 47h. 1047l: if Act = Alt then Alt' = all BCE and otherwise as 47l (no cursor restore); else identity. 1049h: Slot(main)' := the DECSC snapshot of T (the slot ESC 7 writes, so it overwrites an earlier DECSC); then Act' = Alt, Alt' all BCE, PW' = false, Cur' = Cur (no home). 1049l: Act' = Main and DECRC from Slot(main); the slot is not consumed, so a later bare ESC 8 restores it again. Corollary: ∀ x free of 47/1047/1049/RIS/resize, Main and Cur after T·ESC[?1049h·x·ESC[?1049l equal those of T, and no cell written in x is visible in Main.	tests/vt_categories.rs::alt_screen_isolates_then_restores, tests/vt_categories3.rs::mode_1049_saves_cursor_switches_and_clears_alt, tests/vt_categories3.rs::mode_1047_exit_clears_the_alt_screen, tests/vt_categories3.rs::mode_1047_does_not_save_or_restore_cursor, tests/vt_categories3.rs::mode_47_round_trip_keeps_primary_content, tests/vt_categories3.rs::mode_47_swap_does_not_move_cursor, tests/vt_categories3.rs::mode_47_alt_content_persists_across_reentry, tests/xterm_differential_regressions.rs::mode_1049_enter_keeps_cursor_position, tests/xterm_differential_regressions.rs::bare_decrc_after_1049_round_trip_restores_saved_cursor, tests/xterm_differential_regressions.rs::mode_1049_save_overwrites_decsc_slot	&mut self over handler views; two swapped Grid values; Option<SavedCursorState>; whole-grid clear loop; quantification over an arbitrary byte stream x. Best authored as a ty_model! AltScreen model with a conformance binding and a negative control.	0,1,2,3,4,5,7,10,12	aterm-core alt-screen-switch-wrap-and-save, alt-1049-roundtrip
alt-switch-keeps-margins	invariant	aterm-core	aterm-core DECSET/DECRST 47/1047/1049 handlers, aterm_grid::Grid::{scroll_region,horizontal_margins,reverse_index}	∀ T ∈ Reach, m ∈ {47, 1047, 1049}, s ∈ {h, l}: Reg, HM, LR and Stops after T·CSI ? m s equal those of T, because one margin state is shared by both buffers. Hence, before and after any switch, RI with Cur.row < Reg.top only moves the cursor up (clamped at 0) and never scrolls.	tests/xterm_differential_regressions.rs::scroll_region_persists_into_alt_screen, tests/xterm_differential_regressions.rs::scroll_region_set_in_alt_screen_persists_after_exit	&mut self; a frame condition across a grid swap; u16 comparisons	0,1,2,3,4,12	-
charset-designate-shift-translate	contract	aterm-core	aterm-core CharacterSetState::{default,translate,designate,invoke,single_shift}, ESC ( ) * + designator dispatch, SO/SI/SS2 dispatch, write_char_body (translate)	CS(fresh) = G0..G3 all ASCII, GL = G0, no pending single shift. ESC ( F / ) F / * F / + F set G0/G1/G2/G3 to charset(F) ('0' = DEC Special Graphics, 'B' = ASCII). SO sets GL = G1; SI sets GL = G0. SS2 (ESC N) makes exactly the next graphic char use G2, then GL is back in force. Every print c lands translate(G_active, c), where translate(ASCII, c) = c and translate(DecSpecial, c) is the VT100 table on 0x5F..=0x7E ('q' ↦ U+2500, 'x' ↦ U+2502, …) and c elsewhere. Consequently SO with no prior SCS prints plain ASCII. The table half is decidable by evaluation over 128 code points.	tests/vt_categories2.rs::dec_line_drawing_maps_glyphs, tests/vt_categories3.rs::so_si_locking_shift_g1_line_drawing, tests/vt_categories3.rs::ss2_single_shifts_exactly_one_char_from_g2, tests/xterm_differential_regressions.rs::g1_defaults_to_ascii_so_prints_plain_text	Copy struct with an enum per G slot; literal-char match table; char; the end-to-end clause composes parser ESC dispatch with write_char	0,1,2,3,4,12	-
rep-is-repeated-write	equivalence	aterm-core	aterm-core CsiHandler::RepeatChar (bulk write_cell_run arm and write_char arm), write_char_body (last_graphic_char), aterm_grid::Grid::write_cell_run	∀ T with last_graphic_char = Some(c), c the raw scalar before charset translation, ∀ n: T·CSI n b ≡ T·print(c)^max(n,1) modulo the ZWJ flag, so each repeat re-translates c through the GL charset in force at REP time. The bulk arm (ASCII c, passthrough charset, no extras, AW, ¬IRM, n ≥ 4, cursor within the margin span) gives the same Obs as the write_char loop. With last_graphic_char = None, REP is a no-op.	tests/vt_categories3.rs::rep_repeats_preceding_graphic_char, tests/xterm_differential_regressions.rs::rep_translates_raw_char_through_current_charset, tests/xterm_differential_regressions.rs::rep_after_charset_switch_back_repeats_ascii, tests/xterm_differential_regressions.rs::rep_plain_ascii_bulk_path_still_repeats	&mut self; a write_char loop against a bulk fill (a loop invariant relates them); Option<char>; charset translate; damage bracket	0,1,2,3,4,5,7,12	aterm-core rep-repeats-the-last-graphic-character
deccolm-gate	contract	aterm-core	aterm-core DECSET/DECRST 3, 40, 95; handler_dec_refinement::set_column_mode_132; query_dec_mode (DECRQM ?3)	∀ T ∈ Reach, s ∈ {h, l}: if ¬deccolm_enable then T·CSI ? 3 s ≡ T. If enabled: column_mode_132' = (s = h); if the value changed and ¬decncsm, then Reg, LR and HM are reset to full, every visible cell := BCE, every LS := SingleWidth, Stops := every 8, and Cur := (0,0); if decncsm is set, or the value did not change, only column_mode_132 changes. The grid width never changes (the host resizes). DECRQM ?3 replies CSI ? 3 ; (column_mode_132 ? 1 : 2) $ y. ?40 sets deccolm_enable and ?95 sets decncsm.	tests/vt_categories3.rs::deccolm_ignored_when_mode_40_is_reset, tests/vt_categories3.rs::deccolm_with_mode_40_clears_screen_and_homes_cursor, tests/vt_categories3.rs::decncsm_suppresses_clear_on_deccolm_switch	&mut self; bool mode flags; erase loop over Vec rows; tab-stop reset loop; DECRQM response formatting into a Vec<u8>	0,1,2,3,4,7,12	-
decscnm-is-global-inverse	equivalence	aterm-core	aterm-core color_resolve::{resolve_colors,resolve_colors_raw_opts} (effective_inverse), Terminal::render_row, handler_dec_refinement::set_reverse_video	(a) ∀ cell, palette, default pair: resolve(cell, reverse_video = true) = resolve(cell with INVERSE toggled, reverse_video = false), i.e. effective_inverse = INVERSE xor reverse_video, so a default-styled cell renders (default_bg, default_fg). (b) T·CSI ? 5 s changes only modes.reverse_video (plus damage); stored cells are unchanged, so render_row swaps fg/bg for every existing and newly written default cell, and ?5l restores render_row(T).	tests/vt_categories3.rs::decscnm_reverse_video_flips_existing_and_new_cells	(a) a pure function over Copy structs (CellFlags bitflags, [u8;3], palette lookup) with a bool xor; (b) a frame condition over &mut self and the Vec that render_row returns	0,1,2,3,5	aterm-core resolve-colors-denotation
declrmm-margin-cursor	contract	aterm-core	aterm-core DECSET/DECRST 69, DECSLRM (CSI Pl;Pr s), aterm_grid::Grid::{cursor_forward_margin,cursor_backward}, write_char autowrap with margins	∀ T with LR set and 1 ≤ l < r ≤ C: CSI l;r s sets HM = (l−1, r−1) and homes the cursor (origin-aware). Then for every col c ∈ [L,R]: CUF n gives min(c+n, R) and CUB n gives max(c−n, L); a print at R with AW sets PW, and the next print lands at (row+1, or after Index, col L) and leaves col L+1. After CSI ? 69 l: LR = false, HM = full, and CUF clamps at C−1.	tests/vt_categories3.rs::declrmm_autowrap_wraps_to_left_margin, tests/vt_categories3.rs::declrmm_cursor_clamps_and_reset_restores_full_width	&mut self; u16 saturating clamps; write path with pending wrap; margins struct	0,1,2,3,4,5,12	aterm-core decslrm-validity, decslrm-run-wraps-at-the-right-margin; aterm-grid cursor-horizontal-margins
tab-stop-motion	contract	aterm-grid	aterm_grid::Grid::{tab,tab_margin,back_tab,back_tab_margin,back_tab_n_margin,clear_tab_stop,clear_all_tab_stops}; aterm-core CHT (CSI n I), CBT (CSI n Z), TBC (CSI 0/3 g) handlers	Let S = Stops (power-on: every multiple of 8), c = Cur.col, M = MaxCol(row). HT: col' = min{k : c < k ≤ M ∧ S[k]}, or M if there is none. CHT n = HT applied max(n,1) times. back_tab: col' = max{k < c : S[k]}, or 0. back_tab_margin(lr, om) is back_tab unless lr ∧ om; then, with L = HM.left, a cursor at c ≤ L stands still, else col' = max{k : L ≤ k < c ∧ S[k]}, or L. CBT n = back_tab_margin(LR, OM) applied max(n,1) times. TBC 0 clears S[c]; TBC 3 clears every stop. None of them changes Cur.row, PW or any cell.	tests/vt_categories3.rs::cht_forward_and_cbt_backward_tab_stops, tests/vt_categories3.rs::cbt_stops_at_the_left_margin_only_under_decom, tests/vt_categories3.rs::tbc_clears_one_stop_then_all_stops	&mut self on Grid; forward and reverse for-loops over Vec<bool> with early return; u16↔usize conversion; loop invariant stated as min/max over a slice range	0,1,2,3,5,7	aterm-grid ht-next-stop, cbt-prev-stop, tab-stop-set-clear
decaln-contract	contract	aterm-grid	aterm_grid::Grid::screen_alignment_pattern, ESC # 8 dispatch	∀ T ∈ Reach: after ESC # 8, every visible cell is Cell::EMPTY with glyph 'E' (no colours, flags or extras), every LS = SingleWidth, Reg and HM are full, Cur = (0,0) and PW = false.	tests/vt_categories3.rs::decaln_fills_screen_with_e_and_homes_cursor	&mut self on Grid; nested loops with Row::clear/write_char; extras.clear(); a quantified postcondition over a 2-D Vec	0,1,2,3,5,7	aterm-grid decaln
line-size-dwl-swl	contract	aterm-grid, aterm-core	aterm-core ESC # 6 / # 5 / # 3 / # 4 dispatch, aterm_grid::Row::set_line_size, aterm_grid::Grid::{effective_cols_for_row,max_col_for_row,clamp_col_for_row}, write_char on a double-width row	∀ T and row r: ESC # 6 sets LS(r) = DoubleWidth and blanks every cell of r at col ≥ C/2, keeping the rest. While LS(r) ≠ SingleWidth, MaxCol(r) = C/2−1, each char takes one storage column, and autowrap fires at MaxCol(r). ESC # 5 sets SingleWidth, keeps the stored text, and gives MaxCol(r) = C−1.	tests/vt_categories3.rs::decdwl_truncates_right_half_and_decswl_keeps_text	&mut self; enum LineSize; u16 division; range erase loop; per-row width in the write path	0,1,2,3,5,12	aterm-grid row-line-size; aterm-core line-size-run-at
decsca-selective-erase	contract	aterm-grid, aterm-core	aterm-core DECSCA (CSI Ps " q) handler, aterm_grid::Grid::{selective_erase_screen,selective_erase_to_end_of_screen,selective_erase_from_start_of_screen,selective_erase_line*}, Row::selective_erase_chars, Grid::erase_screen	DECSCA 1 sets style.protected, so later writes carry PROTECTED; DECSCA 0 clears it. DECSED/DECSEL (CSI ? Ps J / K) blank exactly the unprotected cells in their range and leave protected cells bit-identical. ED/EL blank every cell in their range regardless of PROTECTED.	tests/vt_categories3.rs::decsca_protects_from_decsed_but_not_from_ed	&mut self; per-cell loop with a bitflag test; two-state 'unchanged where protected'	0,1,2,3,5,7	aterm-grid selective-erase-decsca, selective-erase-span
csi-alternate-final-aliases	equivalence	aterm-core	aterm-core TerminalHandler::csi_dispatch_untabled (("",'^'), ("#",'p'), ("#",'q')), dispatch_csi_handler (ScrollDown, Xtpushsgr, Xtpopsgr), handle_scroll	∀ T ∈ Reach, params P: T·CSI P ^ ≡ T·CSI P T; T·CSI P # p ≡ T·CSI P # {; T·CSI P # q ≡ T·CSI # } (params ignored). SD: CSI n T scrolls Reg down by max(p0 or 1, 1) (rectangularly within HM when HM is not full), BCE rows entering at Reg.top; rows outside Reg, Cur and PW are unchanged. The aliases share one handler and one sgr_stack, so a push under one spelling is popped by the other.	tests/xterm_alternate_finals.rs::sd_spelled_with_caret_scrolls_down_like_sd_spelled_with_t, tests/xterm_alternate_finals.rs::sd_spelled_with_caret_defaults_to_one_line, tests/xterm_alternate_finals.rs::sd_spelled_with_caret_scrolls_only_inside_the_scroll_region, tests/xterm_alternate_finals.rs::xtpushsgr_spelled_hash_p_restores_the_same_style_as_hash_brace, tests/xterm_alternate_finals.rs::xtpopsgr_spelled_hash_q_pops_a_stack_pushed_with_hash_brace	match on (&[u8], u8); a call to the same handler with the same arguments (the alias half is pure callee-ensures composition, no unfolding); the SD half needs Grid row rotation	0,1,2,3,4	aterm-core csi-dispatch-table
sgr-stack-model	refinement	aterm-core	aterm-core TerminalHandler::{handle_xtpushsgr,handle_xtpopsgr}, SgrPushMask::{from_params,is_all,has_*}, transient.sgr_stack	The stack holds (style, underline, mask) entries, length ≤ SGR_STACK_MAX_DEPTH (10). push(P): mask = ALL if P ∈ {[], [0]}, else from_params(P); when the stack is full, drop the oldest entry; then append (Sty, underline, mask). pop: on an empty stack, the identity on Obs; otherwise remove the newest entry e; if e.mask = ALL, Sty := e.style (keeping the current DECSCA bit) and underline := e.underline; otherwise restore exactly the groups in e.mask (1/2, 3, 4/21 plus underline colour, 5, 7, 8, 9, 30-39, 40-49, 53) and keep the rest of the current style.	tests/xterm_alternate_finals.rs::xtpushsgr_spelled_hash_p_takes_the_same_pm_selection_as_hash_brace, tests/xterm_alternate_finals.rs::unbalanced_alias_pops_are_ignored_without_disturbing_the_style	VecDeque with a capacity bound; Copy style struct with bitflags; masked merge; &mut self	0,1,2,3,5,10	-
vt-ris-is-fresh	contract	aterm-core	aterm_core::terminal::Terminal::process (RIS, ESC c)	∀ T of size R×C: T·ESC c ≡ Terminal::new(R,C): empty screen, cursor home, default modes and pen, full margins, no pending responses.	tests/vt_categories2.rs::ris_full_reset_clears_and_homes	&mut self on Terminal, whole-state reset	0,1,2,3,4,5,7,12	-
process-total-and-recovers	no-panic	aterm-core	aterm_core::terminal::Terminal::process	∀ R, C ≥ 1, ∀ byte strings x: process(x) terminates without panicking and with bounded allocation (sixel repeats clamp to SIXEL_MAX_DIMENSION, DCS payload capped by MAX_DCS_GLOBAL_BUDGET); after x followed by ST (or CAN) and CSI 1;1 H !, cell (0,0) is '!' (the parser is back in Ground).	tests/rect_ops.rs::adversarial_rect_sequences_never_panic, tests/sixel.rs::enormous_decgri_repeat_count_no_panic_no_oom, tests/sixel.rs::payload_larger_than_global_dcs_budget_is_dropped_without_panic	whole engine: parser loop, handlers, sixel decoder allocation bounds	0,1,2,3,4,5,7,12,16	aterm-core terminal-wf-invariant; aterm-sixel params-bound, growth-amortized
process-chunk-invariance	equivalence	aterm-core	aterm_core::terminal::Terminal::process	∀ T ∈ Reach, byte strings x, y: T·(x++y) ≡ (T·x)·y (cells, images, cursor, modes, pending responses).	tests/sixel.rs::sixel_dcs_split_across_feeds_renders_identically	whole engine; parser and DCS state carried across calls	0,1,2,3,4,5,7,12	aterm-core process-chunk-invariance
vt-response-buffer-fifo	contract	aterm-core	aterm_core::terminal::Terminal::{process,take_response}	∀ inputs x1..xn fed with no drain between: take_response() = Some(the concatenation of the replies to the queries in x1..xn, in arrival order) when non-empty, else None; take_response() right after a drain is None; input without a query (plain text, SGR, a sixel DCS) adds no reply.	tests/responses.rs::responses_accumulate_until_drained_then_buffer_is_empty, tests/responses.rs::plain_text_produces_no_response	&mut self on Terminal, Vec<u8> append and take	0,1,2,3,4,5,7,12	aterm-core response-buffer-bounded, resp-take-response
vt-device-attributes	table-property	aterm-core	aterm_core::terminal::Terminal::process (handler_report DA1/DA2/DA3/DSR)	With the sixel feature, independent of terminal state: CSI c and CSI 0 c reply ESC[?64;6;22;28;4c; CSI > c replies ESC[>41;100;0c; CSI = c replies DCS !|30 ST; CSI 5 n replies ESC[0n.	-	&mut self on Terminal, const byte strings, cfg(feature)	0,1,2,3,4,5,7,12	-
vt-cpr-reports-cursor	contract	aterm-core	aterm_core::terminal::Terminal::process (handler_report::handle_dsr)	∀ T with Cur = (r,c): CSI 6 n replies ESC[ (r−o+1) ; (c+1) R and CSI ? 6 n replies ESC[? (r−o+1) ; (c+1) ; 1 R, where o = Reg.top if OM else 0.	tests/sixel.rs::sixel_dcs_leaves_parser_state_intact_for_subsequent_csi	&mut self on Terminal, decimal formatting into Vec<u8>	0,1,2,3,4,5,7,12	-
vt-decrqm-reports-mode	contract	aterm-core	aterm_core::terminal::Terminal::process (handler_dec::handle_decrqm, DECSTR)	∀ recognised DEC private modes m (including 25, 6, 80, 2048): CSI ? m $ p replies ESC[? m ; s $ y with s = 1 when m is set and s = 2 when reset, reflecting the current state (power-on: 25 set; 6, 80, 2048 reset; pushing kitty flags does not change 2048); DECSTR resets 80. Kitty flags have no mode number and are reported only through CSI ? u.	tests/sixel.rs::decsdm_mode80_set_reset_roundtrip_via_decrqm, tests/sixel.rs::decsdm_is_reset_by_decstr_soft_reset	&mut self on Terminal, match over mode numbers	0,1,2,3,4,5,7,12	aterm-core decrqm-mode-agreement
vt-decrqss-replayable	round-trip	aterm-core	aterm_core::terminal::Terminal::process (handler_decrqss::handle_decrqss and the decrqss_* helpers)	∀ known DECRQSS selectors Pt (m, r, "q, SP q, "p, *x, …): DCS $ q Pt ST replies DCS 1 $ r <setting> ST such that feeding CSI <setting> to a fresh terminal reproduces the queried setting (SGR is reported self-contained with a leading 0;, DECSCL's C1 field is fixed at 1 (7-bit), DECSACE reports the current extent); an unknown Pt replies DCS 0 $ r ST; either way the parser returns to Ground and later text prints normally.	tests/rect_ops.rs::decrqss_decsace_roundtrips_current_extent	&mut self on Terminal, String building, DCS payload buffer	0,1,2,3,4,5,7,12	aterm-types vt-decscl-roundtrip
kitty-keyboard-flag-stack	contract	aterm-core, aterm-types	aterm_core::terminal::Terminal::{process,keyboard_mode} (kitty push/pop/set/query handlers)	Per screen there is a stack of flag sets with current = top (0 when empty): CSI > f u pushes f; CSI < n u pops min(max(n,1), depth) entries and never underflows; CSI = f ; 1 u sets current to exactly f; CSI ? u replies ESC[? current u; keyboard_mode() is the KeyboardMode whose bits are current.	tests/kitty_mouse.rs::kitty_query_default_reports_zero_flags, tests/kitty_mouse.rs::kitty_push_query_pop_round_trip, tests/kitty_mouse.rs::kitty_pop_on_empty_stack_is_clamped_no_underflow	&mut self on Terminal, bounded Vec stack, bitflags	0,1,2,3,4,5,7,12	aterm-types kitty-push-pop-stack-model, kitty-flags-apply; aterm-core keyboard-mode-projection
kitty-encode-key	contract	aterm-types	aterm_types::keyboard::encode_key	∀ keys k, modifiers m, modes M: without DISAMBIGUATE_ESC_CODES and REPORT_ALL_KEYS_AS_ESC the legacy encoding is produced (Esc ↦ 0x1B, Ctrl+letter ↦ its C0 byte, arrows ↦ CSI A..D); with DISAMBIGUATE: Esc ↦ CSI 27 u, Ctrl+char c ↦ CSI cp(c) ; 1+mods u, and keys with legacy non-text encodings (arrows, Home/End, F-keys) are unchanged; with REPORT_ALL_KEYS_AS_ESC: text keys ↦ CSI cp u (plus the modifier field when m is non-empty).	tests/kitty_mouse.rs::kitty_disambiguate_flag_switches_esc_and_ctrl_c_to_csi_u, tests/kitty_mouse.rs::kitty_disambiguate_keeps_legacy_arrow_encoding, tests/kitty_mouse.rs::kitty_report_all_keys_as_esc_encodes_plain_text_key_as_csi_u, tests/kitty_mouse.rs::kitty_pop_restores_legacy_encoding	enum matches on Key/NamedKey, bitflags, Vec<u8> building, decimal formatting loop	0,1,3,7,12	aterm-types kb-kitty-csi-u-denotation, kb-kitty-progressive, kb-legacy-named; aterm-core kitty-keyboard-protocol
mouse-encode-format	contract	aterm-types, aterm-core	aterm_types::mouse::{encode_mouse,encode_x10,encode_utf8,encode_sgr,encode_urxvt,push_dec_u16}, aterm_core::terminal::Terminal::encode_mouse_{press,release,wheel}	∀ cb:u8, col,row:u16, enc, release: lcb = release ? (cb & 0x3C) | 3 : cb. X10: [ESC, '[', 'M', sat(lcb+32), min(col+1, 223)+32, min(row+1, 223)+32] (out-of-range coordinates clamp inside the X10 frame, never promote to SGR); UTF8: ESC [ M then u(lcb), u(col+1), u(row+1) with u(v) = [v+32] if v+32 < 128, else the 2-byte UTF-8 of min(v+32, 2047); SGR and SGR-pixel: ESC [ < dec(cb) ; dec(col+1) ; dec(row+1) then 'm' if release else 'M' (button identity kept); URXVT: ESC [ dec(lcb+32) ; dec(col+1) ; dec(row+1) M. The Terminal wrappers pass cb = button|mods for press and release and cb = WheelDir::code (64, 65, 66, 67)|mods for the wheel.	tests/kitty_mouse.rs::mouse_1000_default_encoding_press_release_exact_bytes, tests/kitty_mouse.rs::mouse_1000_wheel_up_is_button_64_and_shift_adds_4, tests/kitty_mouse.rs::mouse_1006_horizontal_wheel_is_buttons_66_and_67, tests/kitty_mouse.rs::mouse_1006_thumb_buttons_are_128_and_129, tests/kitty_mouse.rs::mouse_1006_sgr_press_release_keeps_button, tests/kitty_mouse.rs::mouse_1015_urxvt_press_form, tests/kitty_mouse.rs::mouse_1005_utf8_two_byte_coordinate_beyond_95, tests/kitty_mouse.rs::mouse_x10_coordinate_beyond_223_clamps_and_keeps_the_x10_frame	Vec<u8> push/extend, u8/u16 saturating arithmetic and casts, digit loop (push_dec_u16), enum match	0,1,3,7,12	aterm-types mouse-x10, mouse-utf8, mouse-decimal-forms, mouse-dispatch; aterm-core mouse-wheel-encoding
mouse-mode-gating	contract	aterm-core	aterm_core::terminal::Terminal::{encode_mouse_press,encode_mouse_release,encode_mouse_motion,encode_mouse_wheel}, DECSET/DECRST 9/1000/1002/1003	∀ T with mouse mode M: press = None ⇔ M = None; release = None and wheel = None ⇔ M ∈ {None, X10}; motion = Some(encode_mouse(button|mods|32, ..)) ⇔ M = AnyEvent ∨ (M = ButtonEvent ∧ button ≠ 3), else None; DECSET 9/1000/1002/1003 set M to X10/Normal/ButtonEvent/AnyEvent and the matching DECRST sets None.	tests/kitty_mouse.rs::mouse_1002_button_event_motion_requires_held_button, tests/kitty_mouse.rs::mouse_1003_any_event_motion_without_button_uses_code_3, tests/kitty_mouse.rs::mouse_1000_normal_tracking_reports_no_motion, tests/kitty_mouse.rs::mouse_no_tracking_mode_all_encoders_return_none, tests/kitty_mouse.rs::mouse_mode_9_x10_compat_is_press_only	&self on Terminal.modes, enum match, Option<Vec<u8>>	0,1,2,3,4,12	-
osc8-admission	contract	aterm-core	aterm_core::terminal::handler_osc::{handle_osc_8,is_allowed_scheme}, aterm_core::terminal::hyperlink_auth::is_rfc3986_scheme_shape	∀ URI byte strings U (params[2..] rejoined with ';'): after OSC 8 ; P ; U ST the open link is Some(U) ⇔ U is UTF-8, non-empty, |U| ≤ MAX_HYPERLINK_URL_BYTES (8192), is_allowed_scheme(U, extra) holds, and U contains no control char other than TAB and no U+202A..=U+202E or U+2066..=U+2069; otherwise the link is closed (None). is_allowed_scheme(U) ⇔ U contains ':' and its prefix before the first ':' has RFC 3986 scheme shape and equals, ASCII-case-insensitively, one of http, https, mailto, sftp, tel or a host-minted extra. The printed glyph is unaffected by admission.	tests/osc8_schemes.rs::osc8_allowed_schemes_retained, tests/osc8_schemes.rs::osc8_allowed_scheme_is_case_insensitive, tests/osc8_schemes.rs::osc8_dangerous_schemes_rejected, tests/osc8_schemes.rs::osc8_garbage_schemes_rejected, tests/osc8_schemes.rs::osc8_overlength_uri_rejected_independently, tests/osc8_schemes.rs::osc8_control_char_in_uri_rejected_independently, tests/osc8_schemes.rs::osc8_bidi_override_in_uri_rejected_independently	&[&[u8]] join, &str chars()/find/eq_ignore_ascii_case, Arc<str>, &mut self	0,1,2,3,5,7	aterm-core osc8-uri-acceptance, hyperlink-auth
hyperlink-attaches-while-open	invariant	aterm-core	aterm_core::terminal::Terminal::{process,hyperlink_at}	∀ inputs: a cell printed while the open link is Some(U) carries U (hyperlink_at = Some(U)), and a cell printed while it is None carries no link; closing (OSC 8 ;; ST) or a refused URI ends the link for every later cell.	tests/osc8_schemes.rs::osc8_link_applies_only_while_open	&mut self on Terminal, per-cell extras map	0,1,2,3,4,5,7,12	aterm-grid hyperlink-bound-invariant
rect-coords-resolution	contract	aterm-core	aterm_core::terminal::handler_dec::parse_rect_coords	∀ params, T: with origin (o_r, o_c) and extent (E_r, E_c) = the margins under OM and the page otherwise: top = min(max(Pt,1)−1, E_r−1) + o_r, left likewise from Pl, bottom = min((Pb = 0 or absent ? E_r : Pb)−1, E_r−1) + o_r, right likewise from Pr; Rect is None (the operation is a no-op) iff top > bottom ∨ left > right. Hence 0 or absent takes the default, values past the page or margin clamp, a 1×1 rectangle is legal, and an inverted rectangle is ignored by every rect op (DECFRA, DECERA, DECSERA, DECCARA, DECRARA, DECCRA source).	tests/rect_ops.rs::decfra_default_coords_fill_full_screen, tests/rect_ops.rs::decera_default_params_erase_full_screen, tests/rect_ops.rs::rect_coords_are_relative_to_margins_when_decom_set, tests/rect_ops.rs::rect_coords_are_page_absolute_when_decom_reset, tests/rect_ops.rs::reversed_coords_are_ignored, tests/rect_ops.rs::zero_params_take_defaults, tests/rect_ops.rs::rect_clipped_at_screen_right_edge, tests/rect_ops.rs::one_by_one_rect_affects_single_cell	u16 saturating arithmetic, min/max, Option tuple, &self modes, params slice	0,1,2,3	-
decfra-fill	contract	aterm-core, aterm-grid	aterm_core::terminal::handler_dec::handle_decfra	∀ T, Pch, R = Rect(params[1..]): if Pch ∈ 32..=126 ∪ 160..=255, every cell of R becomes (char Pch, the current SGR rendition, the current DECSCA protection) and every other cell is unchanged; otherwise (controls, DEL, C1, 0) nothing changes.	tests/rect_ops.rs::decfra_fills_exact_rect_and_leaves_outside_untouched, tests/rect_ops.rs::decfra_rejects_non_printable_fill_char, tests/rect_ops.rs::decfra_fill_uses_current_sgr_rendition, tests/rect_ops.rs::decfra_fill_carries_current_decsca_protection	&mut self on Terminal, nested row/col loops over the grid	0,1,2,3,4,5,7,12	aterm-grid rect-fill-erase
decera-erase	contract	aterm-core, aterm-grid	aterm_core::terminal::handler_dec::handle_decera	∀ T, R = Rect(P): DECERA sets every cell of R to a blank with default attributes and no protection, regardless of DECSCA; cells outside R are unchanged.	tests/rect_ops.rs::decera_erases_rect_to_spaces_outside_untouched, tests/rect_ops.rs::decera_erases_visual_attributes, tests/rect_ops.rs::decera_ignores_decsca_protection	&mut self on Terminal, nested grid loops	0,1,2,3,4,5,7,12	aterm-grid rect-fill-erase
decsera-selective-erase	contract	aterm-core, aterm-grid	aterm_core::terminal::handler_dec::handle_decsera	∀ T, R = Rect(P): DECSERA replaces the character of every unprotected cell in R with a space, keeping its SGR attributes and protection; protected cells and cells outside R are unchanged. Protection is set by DECSCA at write time (DECFRA fills included) and is changed by neither DECCARA nor DECRARA.	tests/rect_ops.rs::decsera_preserves_decsca_protected_cells, tests/rect_ops.rs::decsera_erases_only_inside_rect, tests/rect_ops.rs::decsera_preserves_visual_attributes_of_erased_positions, tests/rect_ops.rs::decrara_does_not_change_dec_sca_protection	&mut self on Terminal, nested grid loops, CellFlags	0,1,2,3,4,5,7,12	aterm-grid decsera
deccra-copy-buffered	contract	aterm-core, aterm-grid	aterm_core::terminal::handler_dec::handle_deccra	∀ T, source rect S (resolved and clipped), destination (td, ld): let B be a snapshot of S's cells (characters and attributes) taken before any write; each destination cell (td+i, ld+j) on the page becomes B[i][j]; off-page parts are clipped with no wrap; every other cell, including non-overlapped source cells, is unchanged.	tests/rect_ops.rs::deccra_copies_rect_and_source_remains, tests/rect_ops.rs::deccra_overlapping_copy_behaves_as_if_buffered, tests/rect_ops.rs::deccra_copies_character_attributes, tests/rect_ops.rs::deccra_destination_clipped_at_page_edge	&mut self on Terminal, temporary Vec<Vec<Cell>> buffer, nested loops	0,1,2,3,4,5,7,12	aterm-grid deccra-copy
deccara-change-attrs	contract	aterm-core, aterm-grid	aterm_core::terminal::handler_dec::handle_deccara, DECSACE	∀ T, rect (t,l,b,r), SGR list Ps: the extent is Ext(t,l,b,r, stream) with stream ⇔ DECSACE ≠ 2 (0, default or 1). Every cell of the extent has its attributes updated by the subset {0, 1, 4, 5, 7 and their resets} of Ps; characters and DECSCA protection are unchanged; cells outside the extent are unchanged.	tests/rect_ops.rs::deccara_single_row_applies_attrs_and_keeps_characters, tests/rect_ops.rs::deccara_rect_extent_decsace2_changes_exact_rectangle, tests/rect_ops.rs::deccara_stream_extent_decsace1_runs_streamwise, tests/rect_ops.rs::deccara_default_extent_is_stream, tests/rect_ops.rs::deccara_sgr0_clears_attrs_but_not_protection	&mut self on Terminal, nested grid loops, CellFlags bit ops	0,1,2,3,4,5,7,12	aterm-grid deccara-decrara
decrara-reverse-involution	contract	aterm-core, aterm-grid	aterm_core::terminal::handler_dec::handle_decrara	∀ T, rect, Pm: with the DECSACE extent (as for DECCARA) and A(Pm) = the union over codes of: 0 or absent ↦ {bold, underline, blink, inverse}; 1, 4, 5, 7, 8 ↦ that attribute; any other code ↦ nothing, each cell of the extent has each attribute in A toggled (underline: any underline style ↦ none, none ↦ single); characters, protection, the cursor and cells outside the extent are unchanged. Corollary: applying the same DECRARA twice is the identity.	tests/rect_ops.rs::decrara_turns_a_named_attribute_on_where_absent_and_off_where_present, tests/rect_ops.rs::decrara_applied_twice_restores_the_original_attributes, tests/rect_ops.rs::decrara_leaves_every_cell_outside_the_rectangle_alone, tests/rect_ops.rs::decrara_default_pm_reverses_modes_1_4_5_7, tests/rect_ops.rs::decrara_reversing_mode_8_is_the_documented_xterm_extension, tests/rect_ops.rs::decrara_reversing_mode_4_never_produces_an_underline_style_sgr_4_cannot, tests/rect_ops.rs::decrara_ignores_sgr_codes_the_pm_list_does_not_name, tests/rect_ops.rs::decrara_rect_extent_decsace2_reverses_the_exact_rectangle, tests/rect_ops.rs::decrara_stream_extent_decsace1_runs_streamwise, tests/rect_ops.rs::decrara_default_extent_is_stream_like_deccara	&mut self on Terminal, nested grid loops, CellFlags bit ops	0,1,2,3,4,5,7,12	aterm-grid deccara-decrara, reverse-cell-attrs
rect-ops-preserve-cursor	invariant	aterm-core	aterm_core::terminal::handler_dec::{handle_decfra,handle_decera,handle_decsera,handle_deccara,handle_decrara,handle_deccra}	∀ T and parameters: each rectangular operation leaves Cur and PW unchanged.	tests/rect_ops.rs::rect_ops_do_not_move_cursor	&mut self frame condition on Terminal	0,1,2,3,4,5,7,12	-
sixel-placement	contract	aterm-core, aterm-sixel	aterm-core handler_dcs sixel path, aterm_core::terminal::handler_osc_1337::place_sixel_image	∀ T with Cur = (r,c), cell metric (cw, ch), and sixel DCS bodies that decode to a w×h image with w, h > 0: at ST one ImageRef is placed whose top-left tile (0,0) is at (r,c), spanning ⌈w/cw⌉ × ⌈h/ch⌉ cells (clipped to the grid), with format RawRgba8{w,h}, bytes = the decoded RGBA raster (4wh bytes, palette colours as defined) and scaling PixelExact; glyph cells are unchanged and no reply is produced; afterwards Cur = (r + footprint rows, 0) (scrolling as needed) when DECSDM is reset and (r,c) when it is set. A body decoding to no image (0×0) places nothing and leaves the cursor unchanged.	tests/sixel.rs::sixel_dcs_renders_image_then_text_flows_below, tests/sixel.rs::cursor_moves_after_sixel_with_decsdm_reset, tests/sixel.rs::cursor_unmoved_after_sixel_with_decsdm_set, tests/sixel.rs::sixel_anchors_at_cursor_column_mid_line, tests/sixel.rs::sixel_mid_line_does_not_overprint_cells_to_its_left, tests/sixel.rs::sixel_mid_line_anchors_at_cursor_in_display_mode, tests/sixel.rs::sixel_image_decodes_to_expected_raster_and_is_placed, tests/sixel.rs::sixel_placement_is_pixel_exact_not_scaled_to_its_footprint, tests/sixel.rs::zero_size_sixel_image_is_harmless	&mut self on Terminal, cross-crate aterm-sixel decoder, Arc<ImageData>, per-cell extras	0,1,2,3,4,5,7,12	aterm-sixel decode-refines-model, image-shape
sixel-string-lifecycle	contract	aterm-core	aterm_core::terminal::Terminal::process (DCS passthrough to the sixel decoder)	∀ sixel DCS bodies b (any bytes other than ESC, CAN, SUB and ST): no byte of b is printed as a glyph or executed; an ESC that interrupts the string and does not begin ST cancels it as CAN does (nothing is placed) and is then processed normally; CAN or SUB aborts the string with no placement and the cursor unchanged; after the string ends, later text prints normally.	tests/sixel.rs::truncated_sixel_consumes_following_text_until_an_escape_breaks_it_off, tests/sixel.rs::garbage_inside_sixel_payload_is_contained, tests/sixel.rs::can_aborts_sixel_dcs_and_returns_to_ground	&mut self on Terminal, parser DCS states, decoder hook/unhook	0,1,2,3,4,5,7,12	aterm-core sixel-dcs-never-leaks-glyphs, can-sub-cancels-every-string-consumer
xtsmgraphics-replies	table-property	aterm-core	aterm_core::terminal::handler_xtsmgraphics::handle_xtsmgraphics	∀ CSI ? Pi ; Pa ; Pv S with no window-ops callback: (Pi = 1, Pa ∈ {1,4}) ↦ ESC[?1;0;1024S; (Pi = 2, Pa ∈ {1,4}) ↦ ESC[?2;0;1000;1000S; Pi = 3 ↦ ESC[?3;3;0S; Pi ∈ {1,2} with Pa ∈ {2,3} ↦ ESC[?Pi;3;0S; any other Pi ↦ ESC[?Pi;1;0S; Pi ∈ {1,2} with an unknown or omitted Pa ↦ ESC[?Pi;2;0S.	-	match on u16 params, decimal formatting into the response Vec	0,1,2,3,12	aterm-core xtsmgraphics-geometry
```

## 2. Test → spec map (TSV)

`primary` is the spec whose proof retires the test; `also` lists the other specs the same test samples.
A `§4` entry in `primary` means the test samples no spec.

```tsv
file	test	primary	also
src/measure/instance.rs	a_spent_budget_gives_up_at_once_but_still_asks_the_probe_once	budget-await-event	
src/measure/instance.rs	a_live_budget_leaves_the_moment_the_event_lands	budget-await-event	
src/measure/instance.rs	a_client_whose_budget_is_spent_runs_nothing	ctl-call-spent-budget	
src/measure/paint_tests.rs	a_healthy_burst_is_ink_and_quiet_is_its_exact_negation	paint-judge-denotation	
src/measure/paint_tests.rs	the_coverage_floor_binds_ink_but_never_buys_the_quiet_control	paint-judge-denotation	
src/measure/paint_tests.rs	ink_needs_the_compose_side_witness	paint-judge-denotation	
src/measure/paint_tests.rs	a_driven_claim_over_an_unlit_raster_is_red	paint-judge-denotation	
src/measure/paint_tests.rs	recorded_continuity_tolerates_one_dark_frame_and_refuses_two	paint-judge-denotation	
src/measure/paint_tests.rs	a_recorded_take_that_lost_its_bracket_decides_nothing	paint-judge-denotation	
src/measure/paint_tests.rs	a_near_empty_take_is_never_a_dark_pass	paint-judge-denotation	
src/measure/paint_tests.rs	trail_dark_reads_the_engine_ledger_and_refuses_without_one	paint-judge-denotation	
src/measure/paint_tests.rs	effect_off_needs_typed_pixels_a_quiet_ledger_and_no_witness	paint-judge-denotation	
src/measure/paint_tests.rs	a_recorded_effect_off_window_must_read_dead	paint-judge-denotation	
src/measure/paint_tests.rs	a_spelling_that_resolved_elsewhere_is_red	paint-judge-denotation	
src/measure/paint_tests.rs	companion_obligations_ride_on_top_of_ink	paint-judge-denotation	
src/measure/paint_tests.rs	self_contradicting_rows_are_refused	paint-row-validate	
src/measure/scan_tests.rs	the_worst_sample_reported_is_the_one_nearest_pure_cyan	cyan-band	
src/measure/scan_tests.rs	a_star_on_a_long_band_s_edge_leaves_the_band_lit	band-ink-partition	
src/measure/scan_tests.rs	a_tall_object_whose_band_rows_still_exceed_the_cap_is_dropped	band-ink-partition	
src/measure/scan_tests.rs	a_skewed_row_is_refused_rather_than_read_as_undriven	driven-last-frame	
src/measure/scan_tests.rs	status_tokens_are_exact	status-row-tokens	
src/measure/scan_tests.rs	required_status_tokens_fail_closed	status-row-tokens	
src/measure/scan_tests.rs	the_emitted_body_uses_exclusive_ends_and_refuses_malformed_geometry	pet-body-parse	
src/measure/scan_tests.rs	cursor_row_cannot_impersonate_a_companion	tall-component-records	
src/measure/scan_tests.rs	a_small_whole_body_is_a_companion_witness	tall-component-records	
src/measure/scan_tests.rs	compact_pet_is_allowed_only_in_isolated_endpoint_witnesses	companion-component-selection	
src/measure/scan_tests.rs	two_drawn_companions_are_two_components	companion-witness-floor	tall-component-records
src/measure/scan_tests.rs	wide_one_row_ribbon_or_text_cannot_impersonate_a_companion	tall-component-records	companion-component-selection
src/measure/scan_tests.rs	a_charcoal_companion_is_not_rejected_for_lacking_saturation	tall-component-records	
src/measure/scan_tests.rs	only_interior_rainbow_blackouts_count	bool-run-lengths	rainbow-summary-over-distinct-samples
src/measure/scan_tests.rs	duplicate_capture_cannot_manufacture_rainbow_maturity	rainbow-summary-over-distinct-samples	
src/measure/scan_tests.rs	duplicate_capture_does_not_lengthen_an_interior_blackout	rainbow-summary-over-distinct-samples	
src/measure/scan_tests.rs	moving_cursor_and_colored_glyphs_are_not_ribbon_presence	thin-ribbon-component	
src/measure/scan_tests.rs	one_hue_thin_highlighter_is_ribbon_presence	thin-ribbon-component	
src/measure/scan_tests.rs	one_pixel_highlighter_seam_still_has_presence	thin-ribbon-component	
src/measure/scan_tests.rs	compact_resident_pet_is_not_ribbon_presence	thin-ribbon-component	
src/measure/scan_tests.rs	dense_two_cell_run_on_cursor_bottom_is_attached_ribbon	attached-ribbon-run	
src/measure/scan_tests.rs	remote_run_on_cursor_bottom_is_not_attached_ribbon	attached-ribbon-run	
src/measure/scan_tests.rs	a_caret_under_the_colour_floor_still_anchors_the_attached_run	attached-ribbon-run	
src/measure/scan_tests.rs	a_pale_caret_alone_is_still_nothing	attached-ribbon-run	
src/measure/scan_tests.rs	a_remote_run_does_not_attach_to_a_pale_caret_either	attached-ribbon-run	
src/measure/scan_tests.rs	subthreshold_horizontal_spark_is_not_ribbon_presence	thin-ribbon-component	
src/measure/scan_tests.rs	fast_circular_max_matches_the_exact_oracle_including_wrap	circular-hue-max	
src/measure/scan_tests.rs	a_young_tall_band_is_lit_though_it_is_not_thin_geometry	ribbon-lit-predicates	dynamic-metrics-denotation
src/measure/scan_tests.rs	a_lone_caret_block_is_never_lit	ribbon-lit-predicates	
src/measure/scan_tests.rs	a_companion_coat_lends_the_population_arm_nothing	band-ink-partition	
src/measure/scan_tests.rs	a_subthreshold_blob_cannot_buy_continuity	ribbon-lit-predicates	
src/measure/scan_tests.rs	geometry_alone_still_lights_a_frame	ribbon-lit-predicates	
src/measure/scan_tests.rs	a_claim_on_the_baseline_frame_is_never_charged	scan-claim-bound-darkness	
src/measure/scan_tests.rs	the_exemption_stops_at_frame_zero	scan-claim-bound-darkness	
src/measure/scan_tests.rs	a_blanked_layer_mid_take_charges_its_full_run	scan-claim-bound-darkness	
src/measure/scan_tests.rs	the_box_sibling_frames_exactly_the_companion_component	companion-component-selection	
src/measure/scan_tests.rs	a_box_that_holds_the_band_whole_refuses_it	band-ink-partition	
src/measure/scan_tests.rs	body_identity_keeps_strict_and_isolated_morphology_floors	located-pet-component	
src/measure/scan_tests.rs	pixels_outside_the_emitted_rectangle_cannot_supply_missing_height	located-pet-component	
src/measure/scan_tests.rs	a_coat_fragment_cannot_stand_in_for_a_missing_ribbon	scan-companion-accounting	
src/measure/scan_tests.rs	the_emitted_body_identity_separates_a_pet_from_a_larger_typed_row	scan-companion-accounting	
src/measure/scan_tests.rs	a_body_claim_requires_pixels_at_that_body	scan-companion-accounting	
src/measure/scan_tests.rs	known_body_geometry_excludes_coat_even_when_fused_with_a_band	scan-companion-accounting	
src/measure/scan_tests.rs	body_identity_does_not_hide_a_second_animal_outside_the_rectangle	scan-companion-accounting	
src/measure/scan_tests.rs	a_band_crossing_the_animal_s_box_keeps_the_frame_lit	scan-companion-accounting	
src/measure/scan_tests.rs	an_unclaimed_plane_excludes_nothing	scan-companion-accounting	
src/measure/scan_tests.rs	a_fused_companion_s_box_is_the_row_s_and_excludes_nothing	scan-companion-accounting	
src/measure/scan_tests.rs	a_sampling_hole_between_lit_frames_is_never_charged_as_dark	driven-window-darkness	
src/measure/scan_tests.rs	a_static_windows_frame_gap_is_not_a_hole_when_the_ledger_ticked	driven-window-darkness	
src/measure/scan_tests.rs	a_starved_ledger_still_reads_as_a_hole	driven-window-darkness	
src/measure/scan_tests.rs	a_render_stall_under_a_live_loop_is_still_a_hole	driven-window-darkness	
src/measure/scan_tests.rs	a_stall_beside_one_dark_frame_charges_only_that_frame	driven-window-darkness	
src/measure/scan_tests.rs	a_duplicate_cannot_open_the_mature_window	driven-window-darkness	
src/measure/scan_tests.rs	darkness_outside_the_driven_window_is_not_the_rows_business	driven-window-darkness	
src/measure/scan_tests.rs	no_bracket_reports_not_measured_rather_than_zero	driven-window-darkness	
src/measure/scan_tests.rs	no_index_reports_not_measured	driven-window-darkness	
src/measure/scan_tests.rs	the_recording_index_parses_stamps_coverage_and_integer_ticks	recording-index-parse	
src/measure/scan_tests.rs	a_mono_hue_band_is_continuous_however_dark_the_hue_witness	scan-ink-and-recorded-continuity	
src/measure/scan_tests.rs	a_vanished_band_is_still_a_blackout_through_the_same_wiring	scan-ink-and-recorded-continuity	
src/measure/scan_tests.rs	a_take_scans_from_png_files_through_the_one_decoder	scan-ink-and-recorded-continuity	
src/measure/scan_tests.rs	a_take_with_mismatched_rows_or_sizes_is_refused	scan-frames-refusals	
src/measure/scan_tests.rs	the_sleeper_is_located_on_the_mid_take_frames_it_is_claimed_on	scan-companion-accounting	
src/measure/scan_tests.rs	the_pose_claim_is_load_bearing_on_the_wired_scanner	scan-companion-accounting	
src/measure/scan_tests.rs	the_pose_claim_selects_the_floor_inside_the_emitted_rect	located-pet-component	
src/measure/scan_tests.rs	the_positive_control_is_a_sleeper_shaped_body_on_the_cell_floor	located-pet-component	
src/measure/scan_tests.rs	a_glyph_pair_inside_the_cell_is_not_the_sleeper	located-pet-component	
src/measure/scan_tests.rs	two_touching_cells_are_not_the_sleeper_and_the_edge_is_where_it_is_declared	located-pet-component	
src/measure/scan_tests.rs	a_row_running_through_the_cell_is_the_row_s_not_the_sleeper_s	located-pet-component	
src/measure/scan_tests.rs	the_runner_is_located_on_the_mid_take_frames_it_is_claimed_on	scan-companion-accounting	
src/measure/scan_tests.rs	the_runner_s_pose_claim_is_load_bearing	scan-companion-accounting	
src/measure/scan_tests.rs	the_composed_fused_take_is_not_charged	scan-claim-bound-darkness	
src/measure/spin.rs	a_healthy_window_passes_and_either_witness_fails_it	spin-judge	
src/measure/spin.rs	a_bounded_wake_count_that_was_not_reported_is_not_a_pass	spin-judge	
src/measure/starvation_tests.rs	a_take_that_got_its_time_disowns_only_driver_starvation	starvation-classify	
src/measure/starvation_tests.rs	no_take_at_all_is_unknown_not_a_verdict_about_paint	starvation-classify	
src/measure/starvation_tests.rs	a_descheduled_driver_is_named_and_the_take_is_disowned	starvation-classify	
src/measure/starvation_tests.rs	a_blown_capture_cadence_is_starvation_even_when_the_driver_ran	starvation-classify	
src/measure/starvation_tests.rs	a_take_racing_its_budget_is_not_evidence	starvation-classify	
src/measure/starvation_tests.rs	no_samples_is_unknown_and_never_a_clean_bill	starvation-classify	
src/measure/starvation_tests.rs	a_starved_pass_is_unproved_and_a_sound_take_is_decided_by_its_pixels	paint-verdict-monotone	
src/measure/starvation_tests.rs	a_clamped_tier_is_named_and_repeating_its_take_is_still_zero_evidence	starvation-classify	paint-verdict-monotone, qos-table
src/measure/starvation_tests.rs	lateness_is_measured_from_the_fixed_deadline	sched-reading-book	
src/measure/starvation_tests.rs	the_capture_window_is_booked_apart_from_the_life	sched-reading-book	
tests/conformance.rs	plain_text_lands_on_the_top_row	vt-print-advance	
tests/conformance.rs	crlf_moves_to_next_row_col0	vt-c0-cursor-controls	
tests/conformance.rs	carriage_return_returns_to_col0_overwriting	vt-c0-cursor-controls	
tests/conformance.rs	backspace_moves_cursor_left	vt-c0-cursor-controls	
tests/conformance.rs	cup_positions_cursor_1based	vt-cursor-csi-motion	
tests/conformance.rs	sgr_interleaved_attrs_and_color_keep_text	vt-print-advance	
tests/conformance.rs	sgr_bold_then_color_equals_color_then_bold	vt-sgr-pen-algebra	
tests/conformance.rs	sgr_attr_reset_returns_to_default_style	vt-sgr-pen-algebra	
tests/conformance.rs	el0_erases_from_cursor_to_end_of_line	vt-erase	
tests/conformance.rs	ed2_erases_the_whole_screen	vt-erase	
tests/conformance.rs	tab_advances_to_next_8col_stop	vt-c0-cursor-controls	
tests/conformance.rs	autowrap_wraps_at_the_right_margin	pending-wrap-transitions	
tests/conformance.rs	cursor_up_down_forward_back	vt-cursor-csi-motion	
tests/conformance.rs	newline_at_bottom_scrolls	index-and-scroll-region	
tests/kitty_mouse.rs	kitty_query_default_reports_zero_flags	kitty-keyboard-flag-stack	
tests/kitty_mouse.rs	kitty_push_query_pop_round_trip	kitty-keyboard-flag-stack	
tests/kitty_mouse.rs	kitty_pop_on_empty_stack_is_clamped_no_underflow	kitty-keyboard-flag-stack	
tests/kitty_mouse.rs	kitty_disambiguate_flag_switches_esc_and_ctrl_c_to_csi_u	kitty-encode-key	
tests/kitty_mouse.rs	kitty_disambiguate_keeps_legacy_arrow_encoding	kitty-encode-key	
tests/kitty_mouse.rs	kitty_report_all_keys_as_esc_encodes_plain_text_key_as_csi_u	kitty-encode-key	
tests/kitty_mouse.rs	kitty_pop_restores_legacy_encoding	kitty-encode-key	
tests/kitty_mouse.rs	mouse_1000_default_encoding_press_release_exact_bytes	mouse-encode-format	
tests/kitty_mouse.rs	mouse_1000_wheel_up_is_button_64_and_shift_adds_4	mouse-encode-format	
tests/kitty_mouse.rs	mouse_1006_horizontal_wheel_is_buttons_66_and_67	mouse-encode-format	
tests/kitty_mouse.rs	mouse_1006_thumb_buttons_are_128_and_129	mouse-encode-format	
tests/kitty_mouse.rs	mouse_1006_sgr_press_release_keeps_button	mouse-encode-format	
tests/kitty_mouse.rs	mouse_1015_urxvt_press_form	mouse-encode-format	
tests/kitty_mouse.rs	mouse_1005_utf8_two_byte_coordinate_beyond_95	mouse-encode-format	
tests/kitty_mouse.rs	mouse_x10_coordinate_beyond_223_clamps_and_keeps_the_x10_frame	mouse-encode-format	
tests/kitty_mouse.rs	mouse_1002_button_event_motion_requires_held_button	mouse-mode-gating	
tests/kitty_mouse.rs	mouse_1003_any_event_motion_without_button_uses_code_3	mouse-mode-gating	
tests/kitty_mouse.rs	mouse_1000_normal_tracking_reports_no_motion	mouse-mode-gating	
tests/kitty_mouse.rs	mouse_no_tracking_mode_all_encoders_return_none	mouse-mode-gating	
tests/kitty_mouse.rs	mouse_mode_9_x10_compat_is_press_only	mouse-mode-gating	
tests/osc8_schemes.rs	osc8_allowed_schemes_retained	osc8-admission	
tests/osc8_schemes.rs	osc8_allowed_scheme_is_case_insensitive	osc8-admission	
tests/osc8_schemes.rs	osc8_dangerous_schemes_rejected	osc8-admission	
tests/osc8_schemes.rs	osc8_garbage_schemes_rejected	osc8-admission	
tests/osc8_schemes.rs	osc8_overlength_uri_rejected_independently	osc8-admission	
tests/osc8_schemes.rs	osc8_control_char_in_uri_rejected_independently	osc8-admission	
tests/osc8_schemes.rs	osc8_bidi_override_in_uri_rejected_independently	osc8-admission	
tests/osc8_schemes.rs	osc8_link_applies_only_while_open	hyperlink-attaches-while-open	
tests/rect_ops.rs	decfra_fills_exact_rect_and_leaves_outside_untouched	decfra-fill	
tests/rect_ops.rs	decfra_default_coords_fill_full_screen	rect-coords-resolution	
tests/rect_ops.rs	decfra_rejects_non_printable_fill_char	decfra-fill	
tests/rect_ops.rs	decfra_fill_uses_current_sgr_rendition	decfra-fill	
tests/rect_ops.rs	decfra_fill_carries_current_decsca_protection	decfra-fill	
tests/rect_ops.rs	decera_erases_rect_to_spaces_outside_untouched	decera-erase	
tests/rect_ops.rs	decera_erases_visual_attributes	decera-erase	
tests/rect_ops.rs	decera_ignores_decsca_protection	decera-erase	
tests/rect_ops.rs	decera_default_params_erase_full_screen	rect-coords-resolution	
tests/rect_ops.rs	decsera_preserves_decsca_protected_cells	decsera-selective-erase	
tests/rect_ops.rs	decsera_erases_only_inside_rect	decsera-selective-erase	
tests/rect_ops.rs	decsera_preserves_visual_attributes_of_erased_positions	decsera-selective-erase	
tests/rect_ops.rs	deccra_copies_rect_and_source_remains	deccra-copy-buffered	
tests/rect_ops.rs	deccra_overlapping_copy_behaves_as_if_buffered	deccra-copy-buffered	
tests/rect_ops.rs	deccra_copies_character_attributes	deccra-copy-buffered	
tests/rect_ops.rs	deccra_destination_clipped_at_page_edge	deccra-copy-buffered	
tests/rect_ops.rs	deccara_single_row_applies_attrs_and_keeps_characters	deccara-change-attrs	
tests/rect_ops.rs	deccara_rect_extent_decsace2_changes_exact_rectangle	deccara-change-attrs	
tests/rect_ops.rs	deccara_stream_extent_decsace1_runs_streamwise	deccara-change-attrs	
tests/rect_ops.rs	deccara_default_extent_is_stream	deccara-change-attrs	
tests/rect_ops.rs	deccara_sgr0_clears_attrs_but_not_protection	deccara-change-attrs	
tests/rect_ops.rs	decrqss_decsace_roundtrips_current_extent	vt-decrqss-replayable	
tests/rect_ops.rs	decrara_turns_a_named_attribute_on_where_absent_and_off_where_present	decrara-reverse-involution	
tests/rect_ops.rs	decrara_applied_twice_restores_the_original_attributes	decrara-reverse-involution	
tests/rect_ops.rs	decrara_leaves_every_cell_outside_the_rectangle_alone	decrara-reverse-involution	
tests/rect_ops.rs	decrara_default_pm_reverses_modes_1_4_5_7	decrara-reverse-involution	
tests/rect_ops.rs	decrara_reversing_mode_8_is_the_documented_xterm_extension	decrara-reverse-involution	
tests/rect_ops.rs	decrara_reversing_mode_4_never_produces_an_underline_style_sgr_4_cannot	decrara-reverse-involution	
tests/rect_ops.rs	decrara_ignores_sgr_codes_the_pm_list_does_not_name	decrara-reverse-involution	
tests/rect_ops.rs	decrara_rect_extent_decsace2_reverses_the_exact_rectangle	decrara-reverse-involution	
tests/rect_ops.rs	decrara_stream_extent_decsace1_runs_streamwise	decrara-reverse-involution	
tests/rect_ops.rs	decrara_default_extent_is_stream_like_deccara	decrara-reverse-involution	
tests/rect_ops.rs	decrara_does_not_change_dec_sca_protection	decsera-selective-erase	
tests/rect_ops.rs	rect_coords_are_relative_to_margins_when_decom_set	rect-coords-resolution	
tests/rect_ops.rs	rect_coords_are_page_absolute_when_decom_reset	rect-coords-resolution	
tests/rect_ops.rs	reversed_coords_are_ignored	rect-coords-resolution	
tests/rect_ops.rs	zero_params_take_defaults	rect-coords-resolution	
tests/rect_ops.rs	rect_clipped_at_screen_right_edge	rect-coords-resolution	
tests/rect_ops.rs	one_by_one_rect_affects_single_cell	rect-coords-resolution	
tests/rect_ops.rs	rect_ops_do_not_move_cursor	rect-ops-preserve-cursor	
tests/rect_ops.rs	adversarial_rect_sequences_never_panic	process-total-and-recovers	
tests/responses.rs	responses_accumulate_until_drained_then_buffer_is_empty	vt-response-buffer-fifo	
tests/responses.rs	plain_text_produces_no_response	vt-response-buffer-fifo	
tests/sixel.rs	sixel_dcs_renders_image_then_text_flows_below	sixel-placement	
tests/sixel.rs	sixel_dcs_leaves_parser_state_intact_for_subsequent_csi	vt-cpr-reports-cursor	
tests/sixel.rs	sixel_dcs_split_across_feeds_renders_identically	process-chunk-invariance	
tests/sixel.rs	truncated_sixel_consumes_following_text_until_an_escape_breaks_it_off	sixel-string-lifecycle	
tests/sixel.rs	garbage_inside_sixel_payload_is_contained	sixel-string-lifecycle	
tests/sixel.rs	can_aborts_sixel_dcs_and_returns_to_ground	sixel-string-lifecycle	
tests/sixel.rs	zero_size_sixel_image_is_harmless	sixel-placement	
tests/sixel.rs	enormous_decgri_repeat_count_no_panic_no_oom	process-total-and-recovers	
tests/sixel.rs	payload_larger_than_global_dcs_budget_is_dropped_without_panic	process-total-and-recovers	
tests/sixel.rs	decsdm_mode80_set_reset_roundtrip_via_decrqm	vt-decrqm-reports-mode	
tests/sixel.rs	decsdm_is_reset_by_decstr_soft_reset	vt-decrqm-reports-mode	
tests/sixel.rs	cursor_moves_after_sixel_with_decsdm_reset	sixel-placement	
tests/sixel.rs	cursor_unmoved_after_sixel_with_decsdm_set	sixel-placement	
tests/sixel.rs	sixel_anchors_at_cursor_column_mid_line	sixel-placement	
tests/sixel.rs	sixel_mid_line_does_not_overprint_cells_to_its_left	sixel-placement	
tests/sixel.rs	sixel_mid_line_anchors_at_cursor_in_display_mode	sixel-placement	
tests/sixel.rs	sixel_image_decodes_to_expected_raster_and_is_placed	sixel-placement	
tests/sixel.rs	sixel_placement_is_pixel_exact_not_scaled_to_its_footprint	sixel-placement	
tests/vt_categories.rs	decsc_decrc_save_and_restore_cursor	decsc-decrc	
tests/vt_categories.rs	ich_inserts_blanks_shifting_right	vt-insert-delete	
tests/vt_categories.rs	dch_deletes_char_shifting_left	vt-insert-delete	
tests/vt_categories.rs	il_inserts_a_blank_line	vt-insert-delete	
tests/vt_categories.rs	dl_deletes_a_line	vt-insert-delete	
tests/vt_categories.rs	el1_erases_from_start_to_cursor	vt-erase	
tests/vt_categories.rs	el2_erases_the_whole_line	vt-erase	
tests/vt_categories.rs	ed1_erases_above_the_cursor	vt-erase	
tests/vt_categories.rs	alt_screen_isolates_then_restores	alt-screen-trio-model	
tests/vt_categories.rs	scroll_region_index_scrolls_only_the_region	index-and-scroll-region	
tests/vt_categories.rs	wide_char_advances_cursor_by_two	vt-print-advance	
tests/vt_categories2.rs	irm_insert_mode_shifts_right	vt-insert-delete	
tests/vt_categories2.rs	dec_line_drawing_maps_glyphs	charset-designate-shift-translate	
tests/vt_categories2.rs	ri_at_top_scrolls_down	index-and-scroll-region	
tests/vt_categories2.rs	ris_full_reset_clears_and_homes	vt-ris-is-fresh	
tests/vt_categories2.rs	decom_origin_mode_homes_into_scroll_region	vt-cursor-csi-motion	
tests/vt_categories2.rs	pending_wrap_is_cancelled_by_cr	pending-wrap-transitions	
tests/vt_categories3.rs	mode_1049_saves_cursor_switches_and_clears_alt	alt-screen-trio-model	
tests/vt_categories3.rs	mode_1047_exit_clears_the_alt_screen	alt-screen-trio-model	
tests/vt_categories3.rs	mode_1047_does_not_save_or_restore_cursor	alt-screen-trio-model	
tests/vt_categories3.rs	mode_47_round_trip_keeps_primary_content	alt-screen-trio-model	
tests/vt_categories3.rs	mode_47_swap_does_not_move_cursor	alt-screen-trio-model	
tests/vt_categories3.rs	mode_47_alt_content_persists_across_reentry	alt-screen-trio-model	
tests/vt_categories3.rs	deccolm_ignored_when_mode_40_is_reset	deccolm-gate	
tests/vt_categories3.rs	deccolm_with_mode_40_clears_screen_and_homes_cursor	deccolm-gate	
tests/vt_categories3.rs	decncsm_suppresses_clear_on_deccolm_switch	deccolm-gate	
tests/vt_categories3.rs	decscnm_reverse_video_flips_existing_and_new_cells	decscnm-is-global-inverse	
tests/vt_categories3.rs	declrmm_autowrap_wraps_to_left_margin	declrmm-margin-cursor	
tests/vt_categories3.rs	declrmm_cursor_clamps_and_reset_restores_full_width	declrmm-margin-cursor	
tests/vt_categories3.rs	cht_forward_and_cbt_backward_tab_stops	tab-stop-motion	
tests/vt_categories3.rs	cbt_stops_at_the_left_margin_only_under_decom	tab-stop-motion	
tests/vt_categories3.rs	tbc_clears_one_stop_then_all_stops	tab-stop-motion	
tests/vt_categories3.rs	so_si_locking_shift_g1_line_drawing	charset-designate-shift-translate	
tests/vt_categories3.rs	ss2_single_shifts_exactly_one_char_from_g2	charset-designate-shift-translate	
tests/vt_categories3.rs	so_shift_and_truecolor_survive_a_scroll_region_scroll	index-and-scroll-region	
tests/vt_categories3.rs	decaln_fills_screen_with_e_and_homes_cursor	decaln-contract	
tests/vt_categories3.rs	decdwl_truncates_right_half_and_decswl_keeps_text	line-size-dwl-swl	
tests/vt_categories3.rs	ed2_erases_complete_lines_so_a_decdwl_row_comes_back_single_width	vt-erase	
tests/vt_categories3.rs	el2_leaves_a_decdwl_row_double_because_xterm_does	vt-erase	
tests/vt_categories3.rs	decsc_decrc_saves_and_restores_charset	decsc-decrc	
tests/vt_categories3.rs	rep_repeats_preceding_graphic_char	rep-is-repeated-write	
tests/vt_categories3.rs	decsca_protects_from_decsed_but_not_from_ed	decsca-selective-erase	
tests/xterm_alternate_finals.rs	sd_spelled_with_caret_scrolls_down_like_sd_spelled_with_t	csi-alternate-final-aliases	
tests/xterm_alternate_finals.rs	sd_spelled_with_caret_defaults_to_one_line	csi-alternate-final-aliases	
tests/xterm_alternate_finals.rs	sd_spelled_with_caret_scrolls_only_inside_the_scroll_region	csi-alternate-final-aliases	
tests/xterm_alternate_finals.rs	xtpushsgr_spelled_hash_p_restores_the_same_style_as_hash_brace	csi-alternate-final-aliases	
tests/xterm_alternate_finals.rs	xtpushsgr_spelled_hash_p_takes_the_same_pm_selection_as_hash_brace	sgr-stack-model	
tests/xterm_alternate_finals.rs	xtpopsgr_spelled_hash_q_pops_a_stack_pushed_with_hash_brace	csi-alternate-final-aliases	
tests/xterm_alternate_finals.rs	unbalanced_alias_pops_are_ignored_without_disturbing_the_style	sgr-stack-model	
tests/xterm_differential_regressions.rs	g1_defaults_to_ascii_so_prints_plain_text	charset-designate-shift-translate	
tests/xterm_differential_regressions.rs	ht_preserves_pending_wrap_next_printable_wraps	pending-wrap-transitions	
tests/xterm_differential_regressions.rs	ht_at_pending_wrap_keeps_cursor_at_margin	pending-wrap-transitions	
tests/xterm_differential_regressions.rs	ht_ride_to_last_stop_matches_xterm_layout	pending-wrap-transitions	
tests/xterm_differential_regressions.rs	mode_1049_enter_keeps_cursor_position	alt-screen-trio-model	
tests/xterm_differential_regressions.rs	invalid_decstbm_keeps_previous_region	decstbm-validity	
tests/xterm_differential_regressions.rs	valid_decstbm_still_sets_region_and_homes	decstbm-validity	
tests/xterm_differential_regressions.rs	autowrap_below_scroll_region_at_bottom_row_does_not_scroll	index-and-scroll-region	
tests/xterm_differential_regressions.rs	lf_below_scroll_region_at_bottom_row_does_not_scroll	index-and-scroll-region	
tests/xterm_differential_regressions.rs	rep_translates_raw_char_through_current_charset	rep-is-repeated-write	
tests/xterm_differential_regressions.rs	rep_after_charset_switch_back_repeats_ascii	rep-is-repeated-write	
tests/xterm_differential_regressions.rs	rep_plain_ascii_bulk_path_still_repeats	rep-is-repeated-write	
tests/xterm_differential_regressions.rs	unsaved_decrc_leaves_decawm_unchanged	decsc-decrc	
tests/xterm_differential_regressions.rs	saved_decrc_does_not_restore_decawm	decsc-decrc	
tests/xterm_differential_regressions.rs	unsaved_decrc_still_resets_origin_and_homes	decsc-decrc	
tests/xterm_differential_regressions.rs	bare_decrc_after_1049_round_trip_restores_saved_cursor	alt-screen-trio-model	
tests/xterm_differential_regressions.rs	mode_1049_save_overwrites_decsc_slot	alt-screen-trio-model	
tests/xterm_differential_regressions.rs	scrolling_lf_clears_pending_wrap	pending-wrap-transitions	
tests/xterm_differential_regressions.rs	moving_lf_clears_pending_wrap	pending-wrap-transitions	
tests/xterm_differential_regressions.rs	su_preserves_pending_wrap	pending-wrap-transitions	
tests/xterm_differential_regressions.rs	margin_fill_with_decawm_off_arms_wrap_for_later	pending-wrap-transitions	
tests/xterm_differential_regressions.rs	margin_fill_with_decawm_off_overstrikes_without_wrap	pending-wrap-transitions	
tests/xterm_differential_regressions.rs	decawm_reset_does_not_clear_armed_wrap	pending-wrap-transitions	
tests/xterm_differential_regressions.rs	scroll_region_persists_into_alt_screen	alt-switch-keeps-margins	
tests/xterm_differential_regressions.rs	scroll_region_set_in_alt_screen_persists_after_exit	alt-switch-keeps-margins	
```

## 3. Coverage

- The 2026-10-16 survey read 412 tests: 385 mapped to one of the 86 specs; 27 were not spec samples. Section 4 records the subsequent deletions and measurement migration.
- Measurement half: 38 specs cover 170 tests (all of `src/measure` except one constant pin, since deleted).
  VT half: 48 specs covered 215 tests (excluding the 25 acceptance measurements now in `benches/` and the duplicate since deleted).
- The biggest collapses (primary tests): `driven-window-darkness` 20, `paint-judge-denotation` 12,
  `scan-companion-accounting` 12, `alt-screen-trio-model` 11, `pending-wrap-transitions` 11,
  `decrara-reverse-involution` 11, `rect-coords-resolution` 10, and 9 each for `band-ink-partition`,
  `companion-component-selection`, `scan-claim-bound-darkness`, `vt-decrqss-replayable` and `sixel-placement`.
- Six pairs from the two recovery passes were the same property and are merged here: the alt-screen
  isolation corollary into `alt-screen-trio-model`, EL/ED into `vt-erase`, the DECSC/DECRC round trip into
  `decsc-decrc`, the region IND/RI property into `index-and-scroll-region`, DEC line drawing into
  `charset-designate-shift-translate`, and autowrap/CR cancellation into `pending-wrap-transitions`.
- `rect-coords-resolution` is the lemma under all six rectangle specs, and `alt-screen-trio-model`,
  `decsc-decrc` and `pending-wrap-transitions` share the DECSC snapshot and the PW frame condition, so
  proving those first makes the dependent proofs short.

## 4. Measurements outside the ordinary test suite

The 23 paint rows and 2 spin rows are explicit `harness = false` benchmarks in
`benches/paint.rs` and `benches/spin.rs`. `verify measure --app <aterm.app>` runs both
with the test profile against the handed release binary. Each matrix executes its
canonical row table once, sequentially, and reports passed, failed and undecided
counts. A host refusal or an incomplete matrix never passes the check. Ordinary
`--tests` runs do not select these acceptance measurements and report no ignored
paint or spin rows.

Paint rows boot a private headless instance, type real keys and judge captured
pixels. Their pure halves remain `paint-judge-denotation`, `starvation-classify`
and the scan specs. Spin rows count past-deadline arms over 10 s / 6 s windows;
their pure half remains `spin-judge`. Runner fixtures exercise complete execution,
panics, refusals and empty plans without launching an instance.

The two tests previously marked for deletion (a constant pin and a duplicate)
remain deleted.

## 5. Implied specs that look wrong, or disagree with the code

1. **Defects the specs have to carry as preconditions until the code is fixed**
   - `src/measure/starvation.rs::classify`: `i.wall_us * BUDGET_PRESSURE_DEN`, `i.budget_us * BUDGET_PRESSURE_NUM`
     and `2 * i.nominal_dt_us` are unchecked `u64` multiplications on a `pub` input struct (a panic in
     debug, a wrap in release for huge inputs). `SchedReading::book`'s `+=` totals are the same.
     `starvation-classify` and `sched-reading-book` carry a no-overflow `requires`; a no-panic spec is false
     until these saturate.
   - `src/measure/scan.rs::frame_background` indexes (w−1, h−1), so it panics on a 0-width or 0-height
     `Frame`, which `Frame::filled` accepts. `scan_frames` inherits the panic. `tall-component-records`
     carries `requires width, height > 0`.
   - `src/measure/scan.rs::scan_frames` reports a `driven_last_frame` error as
     `ScanError::MalformedStatus { frame: 0, .. }` whichever row was malformed. The frame index in the
     error is wrong, and no test covers it. `scan-frames-refusals` should state the row index once fixed.
2. **Rulings needed before a spec is fixed**
   - `tests/vt_categories3.rs::decncsm_suppresses_clear_on_deccolm_switch`: with DECNCSM set,
     `set_column_mode_132` (`crates/aterm-core/src/terminal/handler_dec_refinement.rs:360`) skips the whole
     block: the margin, DECLRMM and tab-stop resets, the line-attribute clear and the cursor home, not only
     the clear. VT510 and xterm document DECNCSM as suppressing only the clear (xterm still homes). The test
     checks only that content is kept. `deccolm-gate` states the code; decide which is intended.
   - `tests/vt_categories3.rs::cbt_stops_at_the_left_margin_only_under_decom`: its doc comment quotes xterm
     `TabToPrevStop` (the clamp gated on ORIGIN alone). `Grid::back_tab_margin`
     (`crates/aterm-grid/src/grid/tab_ops.rs:138`) differs twice: it clamps only under DECLRMM and DECOM
     together, and a cursor already at or left of the margin does not move (Ghostty's guard). The test arms
     DECLRMM in both cases, so it cannot tell the rules apart. `tab-stop-motion` states aterm's rule; the
     comment overstates xterm parity.
   - `tests/sixel.rs::payload_larger_than_global_dcs_budget_is_dropped_without_panic`: the disjunctive
     assertion (row 0 ends with OK, or row 1 is OK) leaves placement after the budget trips unspecified.
     `process-total-and-recovers` should say whether an over-budget image is dropped.
3. **Tests that sample less than their name or comment claims**
   - `tests/conformance.rs::backspace_moves_cursor_left`: the name claims a cursor property, but only the
     row text is asserted. BS at column 0 (saturation) is never sampled.
   - `tests/vt_categories.rs::scroll_region_index_scrolls_only_the_region`: the comment promises row 5
     untouched and `d` moved up, but only rows 0 and 1 are asserted.
   - `tests/vt_categories.rs::ed1_erases_above_the_cursor` checks only row 0. ED1's inclusive partial erase
     of the cursor row (0..=c) and the untouched rest of that row are not asserted.
   - `tests/vt_categories3.rs::deccolm_ignored_when_mode_40_is_reset`: the second half ("still 80 columns",
     `CSI 1;999H` lands at col 79) cannot detect a broken gate, because `set_column_mode_132` never resizes
     the grid even when DECCOLM is honoured. Only the no-clear and no-home assertions sample the gate.
4. **Duplicates and redundant assertions**
   - `tests/xterm_differential_regressions.rs::scrolling_lf_clears_pending_wrap` says it mirrors aterm-grid's
     `each_operation_keeps_or_consumes_the_parked_wrap`
     (`crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs`). One `pending-wrap-transitions`
     proof in aterm-grid retires both.
   - `tests/kitty_mouse.rs::mouse_x10_coordinate_beyond_223_clamps_and_keeps_the_x10_frame`: the
     `assert_ne!` against the old SGR-promotion bytes is implied by the preceding `assert_eq!`. It pins a
     past defect rather than an extra property.
5. **Stale prose**
   - The `tests/rect_ops.rs` header says spec-correct tests the engine fails are marked
     `#[ignore = "ENGINE BUG: ..."]`, but no test in the file carries that marker.
   - The module header of `tests/xterm_differential_regressions.rs`, and its §7 DECSC comment, say xterm
     DECSC saves ATTRIBUTES|ORIGIN|PROTECTED plus the pending-wrap flag. aterm's snapshot also carries
     `underline_color` and the charset state. That is consistent with the tests, but `decsc-decrc`, not the
     comment, should be the reference for what is saved.

## 6. Proof order

No spec here is blocked by gap 0 alone: every one needs at least one non-scalar carrier (gap 1) or a
body shape E6 refuses today (gap 3). The order below starts with the specs closest to gap 0 only (scalar
inputs, `match` on literals, no loops, no `&mut self`) and moves outwards. The first five are all in this
crate, so none needs gap 12.

1. **`qos-table`** (gaps 0, 3). A `match` on `u32` literals into a fieldless enum, decidable by evaluation.
   Seal `150efe4a` already admits a range `match` on `u8` for E6 import, so this is the smallest widening.
   It also exercises the negative-control loop on this crate.
2. **`paint-verdict-monotone`** and **`starvation-classify`** (gaps 0, 1, 3). Both take `Copy` scalar inputs
   and return fieldless or scalar-payload enums. Together they retire 10 tests, and `classify` forces the
   overflow fix in §5.1 (state the spec without the `requires`, watch it fail, then saturate).
3. **`cell-new-positive`** (0, 1), **`spin-judge`** (0, 1, 2) and **`ribbon-lit-predicates`** (0, 1, 2).
   Straight-line `bool` logic over `Copy` structs, read through `&self`. They retire 9 tests and are the
   first users of struct carriers (gap 1) and certified `&self` views (gap 2).
4. **`saturated-hue-floor`, Some/None half** (0, 3 for that half). Pure `u8` max/min arithmetic.
   It retires the five floor tests once the value half (gap 6, floats) is split into its own clause.
   **`hue-set-buckets`** (0, 1, 3) follows: `u16` bit operations.
5. **`paint-row-validate`** then **`paint-judge-denotation`** (0, 1, 2, 3, 5). Enum matches and scalar
   comparisons over the scan summary struct, with no loops apart from the `all()` over status rows. They
   retire 13 tests and make the paint verdict a theorem rather than a sample.
6. **`rect-coords-resolution`** (0, 1, 2, 3), in aterm-core. Saturating `u16` arithmetic over a params
   slice and two mode bits. It retires 10 tests and is the lemma under DECFRA, DECERA, DECSERA, DECCARA,
   DECRARA and DECCRA (another 31 tests). **`decstbm-validity`** (0, 1, 2, 3, 6) is the same shape once its
   validity predicate is factored into a pure function.
7. **`mouse-encode-format`** (0, 1, 3, 7, 12), in aterm-types. A pure encoder over `u8`/`u16` into a
   `Vec<u8>`, with one digit loop. It retires 8 tests, and its home crate already lists `mouse-x10`,
   `mouse-utf8` and `mouse-decimal-forms`.
8. Then the loop-and-collection scanner specs in dependency order: `bool-run-lengths` →
   `rainbow-summary-over-distinct-samples` → `circular-hue-max` → `ink-objects-4-connected` →
   `row-hue-separation`, `band-ink-partition`, `tall-component-records` → `companion-component-selection`,
   `located-pet-component` → `dynamic-metrics-denotation` → `driven-window-darkness` (20 tests) → the three
   `scan_frames` specs. These need gaps 5 and 7 (quantifiers and loops).
9. The VT state specs in their home crates, built on a `ty_model!` terminal model (gap 10 for the two
   refinements): `pending-wrap-transitions` and `decsc-decrc` first (shared by most others), then
   `alt-screen-trio-model`, the rectangle family, and the reply tables (`vt-device-attributes`,
   `xtsmgraphics-replies`, `vt-cpr-reports-cursor`).
10. Last, the effectful specs: `scan-ink-and-recorded-continuity`'s `scan_dir` half, `ctl-call-spent-budget`'s
    spawn half, `budget-await-event` and `sampler-lifecycle`. They wait on gaps 8 and 13 (effects and
    threads). Factor the pure core out first; `ctl-call-spent-budget`'s spent-budget branch is already pure.
