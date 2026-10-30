<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-effects: spec recovery from its 3027 unit tests

> 2026-10-26: the harness switch is words alone on the rule (`● on` / `○ off`, the owner: "make
> the harness on/off switch more subtle"), so its capsule sprite is gone: `harness_switch` became
> `up_ring`, the ⬆️'s hover ring alone, and §10's row is `up-ring` (it was `harness-switch-capsule`:
> a recessed track, a ring and a gem knob). Written with the code it states; the tests named in it
> ran.
> 2026-10-26 (r23 K6): the leap ahead of a coming wrap is gone. The cat leaves the margin when the
> line has wrapped, never on a guess of when it will: a pause at the margin, or typing that stopped
> short of the wrap, kept the guessed leap in the air 1.34 to 1.81 s and brought it down on the next
> line's start, 70 cells from a caret that had not wrapped (looks8 round 3's early leave, as the
> typing neared the cat at the wall, and its paths over and under the line went with it: on main it
> flew 1.71 to 2.03 s for a held key stopped short of the wrap). `show-door` (§1): every crossing after
> the writing's wrap is the brisk bound, mid-screen too (1.04 to 1.55 s in the air before, 0.46 to
> 0.53 s now); `new-line-law` lets the crossing after a typed wrap (the typed margin wrap, and an
> Enter typed at the margin that no key tells from one) move the brisk bound's step. The margin is
> taken while the hand writes into it (the rhythm open, or its last key a letter within
> `SIT_AFTER`) and held while the caret stays at it: taken only with the rhythm open, the first
> keys typed on after a pause found it untaken, the wall side crossed the stand to the caret's
> left, onto the line's words, and the rhythm's opening brought it back (a hop up, a hop back, then
> the wrap's crossing, three flights at one line end). Once the console rests (no
> key and no caret move for `SIT_AFTER`, the cat's sit) a wall seat among letters is left for the
> nearest seat by the caret that keeps the head law with no rule across it
> (`PetBrain::margin_rest_seat`: one or two rows over the caret's or under it, two over only past
> the line's own letters and never across a rule, within `HEAD_SEAT_REACH`), held until the wrap
> (`WrapAhead::Rest`, which the head law's seat under the caret's line does not let go); where
> there is none (a composer between its rules) or the letter reader certifies nothing, the wall
> holds. A hop in the air as the writing wraps (that rest hop, the hand typing on just after the
> sit) comes down on its own seat and the wrap's brisk bound follows: drawn out to the next line's
> start at a hop's pace it was 1.30 to 1.55 s in the air, gliding low over the line it left.
> `ink-eviction-behavior`'s `the_wall_side_stand_takes_the_blank_row_below` holds that typing
> stopped short of the wrap costs no flight.

> 2026-10-26: `show-door` (§1) now says the bound that crosses after the writing's wrap (or back
> across it under a held delete) flies at the animal's own pace, `BIG_HOP_SPEED_MAX` (half its
> body a 60 Hz frame), on the big bound's own arc for its span (`PetBrain::brisk_shape`), and that
> the cat the wrap left at the margin goes for it with no notice beat (the word a composer's word
> wrap carries down with the caret, a step on along the new line, still belongs to the wrap; only
> a change of row or a jump back ends it; a wrap the hand's own key made is the writing's with its
> rhythm open or not, and a cat in the air for anything but the leap ahead of the wrap still makes
> the brisk crossing once down); every other bound, the leap ahead of a coming wrap
> among them (timed to land with it), is still paced at `quiet_speed_max`
> (r21 K5-1: paced as a quiet crossing, each of Claude Code's composer word wraps took 1.06 to
> 1.17 s, floating over the line as the writing ran 25 cells on). Two motion laws follow it: the
> frame law (`lawful_tick`) holds every flight to its own pace (`PetBrain::flight_cap`), and
> `every_new_line_reaction_is_lawful_and_brief` lets the crossing after a scrolled wrap move the
> brisk bound's 2.3 cells a frame at 80 columns. The head law takes the nearest seat for a stand
> among letters by a caret at rest, the hand away, under the box before the far margin, and goes
> there at once from a cat that must travel anyway; an eviction off the letters while the stream
> watch is still up, the caret already at rest after a program's jump, goes to that seat (r21
> K5-2: the long insert's cat rose from the far margin and came down where it stood). The effects
> suite's `a_repaint_that_walks_the_caret_is_one_flight_to_where_it_settles` reads the cat's
> actions on the ground from the walk's first move (before it, the cat walks with the writing the
> brisk wrap brought it down by).

> 2026-10-25: `rewrap-carries-word`, `glow-park`, and `typed-columns-owned` (§1) gain five
> rendered-pixel continuity tests in `tests/rainbow_pixel_continuity.rs` (§2): clean word-boundary
> (`width == 0`) wraps and 4-sided box-bordered / guttered composer wraps relay immediately without
> bleeding onto borders or indents or stalling in a cross-row park, and a flushed park's v2 events
> precede post-park typed events banked at the same `Instant`.

> 2026-10-25: `glow-landing-ring-raster` (§1) now says a veiled style lands a round rim about the
> caret (r17 CA4: on dark the bound veil alone lifted the ground some 70 levels over about 150 px, so
> "Ring on jumps" read as broken for Lumen, Comet, Sparkle, Phaser and Laser), and the classic wake's
> light ring lays its square in the accent's ink; `the_dark_landing_ring_is_a_soft_bloom_in_its_row`
> is now `the_dark_landing_ring_is_a_round_rim_over_a_soft_bloom_in_its_row` (§2).

> 2026-10-25: `host-line-head-law` (§1) now says how the stand on top of the host's line is taken
> while the person types: as a perch, its soles clear of the line by the clearance a hold asks of a
> body (with the soles on the line the stand passed and the hold did not, and a cat watching a
> deletion there stepped and sat by turns); its slide past the letters over it within 12 columns of
> the caret's column (a 29-column slide at a turn's end launched a hop short of where the caret
> settled: two flights), unless the cat already stands where the slide goes (resting past the
> letters, the first key would have flown it back to the caret); and never in a rule
> (`PetWorld::rule_line`: a row, in a pane at least eight columns wide, whose cells are at least four
> fifths `─`), so over a reserved first line of the composer the ladder seats the cat as before. Two
> tests retire to it (§2), and the escort-grace test now ends on that perch (row 18.85, not 19). The
> effects suite's
> `a_repaint_that_walks_the_caret_is_one_flight_to_where_it_settles` now wants the turn-end bound,
> with the harness on, on the top of the composer's upper rule, two rows over the caret, not on the
> caret's line under the rule. Two motion laws the resident's runs to its rest seat reached:
> `chase-convergence-and-facing` keeps a skid once drawn on glass `STILL_SECS` (a run crossing its
> seat at 9.3 cells a second had shown it one 16 ms frame), and `flight-choreography` holds a stand
> that ends a stride `STAND_MIN` before a leap leaves it, as it already held it against every other
> pose (a stream watch's stop under a long insert, then the wrap's leap, left it 3 ms short). One
> test each (§2).

> 2026-10-25: `pw-reading-kept` (§1) lets the pet keep its console reading of a redraw whose fill
> consumed the terminal's damage when nothing that reading was read from moved, so an idle or typing
> frame that reused its snapshot, or refilled it without re-extracting a row (a split's focused pane, a
> window with chrome rows), classifies no cell; its three tests are in §2. The counts elsewhere in this
> file were taken before it.

> 2026-10-24: `host-line-head-law` (§1, new) is the resident pet's rule at the host's line: no stand
> or seat puts its body under a line whose reserved cells would cover its head (aterm's footer now holds
> the top rule it repaints with its lockup and switch whole, strokes and all); while the person types
> the escort's stand there is taken on top of the line, its head off the letters over that row; the
> line coming up onto that stand carries the cat up (`protected-surface-yield` now names that ride);
> the pet's cell windows ignore f32 noise at an edge, which had refused the caret's line at some rows
> (10, 12, 13) and not others. Four tests retire to it (§2): one new, three whose contract it changed
> (the label and switch tests renamed: the cat walks on top of the line, no longer under it; the
> escort-grace test now ends over the line). `idle-frame-laws` gains
> `a_tail_flick_a_seat_comes_into_never_shows_its_last_present`: a seat that comes into a tail beat
> with under two frames of its pose left does not begin it. Three agent-pane fixtures now hold the top
> rule from the first frame (`AgentPane::hosted`): their "home under the footer" premise had come from
> the rounding noise.

> 2026-10-24: `wd-kitty-cursor-emission` (§1) now also says the flying head never starts above the grid
> top (row 0 and the row-1 sky seat used to put a fifth of it above the first row, where a top pane's open
> edge let it reach the pad or the tab strip and a lower pane's divider cut it), and `wd-pet-sprite-in-grid`
> holds both companions whole in a top pane and a lower pane on the first two rows; one new §2 test each.
> `custom_kitty_footprint_matches_emission_at_interior_right_and_top_edges` now requires row-zero custom art
> to land whole beside its row; it pinned the old partial clip.

> 2026-10-24: §8 adds the seven specs of Aos M5a's aterm-effects half (`int_bag_mlp`, `tone_int`)
> with their 50 tests. `tone-asset-decoder-fail-closed` (§1) now also names `tone::raw`, the verified
> raw view `ToneModel::from_bytes` is rebuilt on. §3's counts were taken before §7 and §8.

> 2026-10-23: `aos-lease-refines-model` (§1) is the M5b worker-lease ledger's refinement of
> `aterm_models_aos::aos_worker_lease_model`, with its eight Tier-1 tests in
> `src/aos_momentum/lease_conformance_tests.rs` (§2). The counts elsewhere in this file were taken
> before it.

> 2026-10-19: `tests/the_owners_composer_gaps/shipping_present.rs` exercises the shared
> `EffectsPipeline::cursor_step`; its finite F1–F5 laws are ordinary deterministic tests.
> Established-hand repaint proofs compare prior/retained row content and exact ordered unpaid
> keys; split insertions also preserve the whole old prefix and tail. The classifier spends
> those keys once and paints only observed spans. Committed takes are headless terminal-byte
> replays through that pipeline, not evidence of native presentation.

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 863 tests
> it deleted and 19 specs whose functions it deleted are gone. Counts elsewhere in this file
> were taken before.
>
> 2026-10-17: three more specs went with code deleted since: `pl-trail-pack` (Trail Packs),
> `hole-tripwire` (the Rainbow Kitty v2 hole tripwire) and `v1-refines-v056` (the frozen v0.56
> synth reference). Two §2 rows of deleted tests went too, and their specs' `primary_tests`
> dropped by one each. Two renamed tests now carry their new names. Four §4 rows whose work is
> done went (the two cursor benches and the v2 frame-cost budget live in `benches/`, the
> wrap-band census is deleted), and so did the §5 item about the companion router's test-only
> composition, which is deleted; the items after it moved up one.

> Status 2026-10-15: this is the crate's migration worklist. Each of the 855 specs below is
> proved through Trust/Clean about the compiled Rust function, then the tests it subsumes are
> deleted. The 97 tests in §4 sample no spec; delete or move them as §4 says. Nothing here is
> proved yet.
>
> **Blocked on seal `150efe4a` (measured 2026-10-16): the whole crate, not one spec.** A strict
> `targo trust check -p aterm-effects` refuses the unit before any obligation runs:
> `verified Targo refuses unit aterm-effects … in-process proc-macro dependencies
> [aterm-serde-derive] … Evidence-grade verification currently enforces a no-proc-macro TCB
> boundary` (Trust PM-1, `reject_in_process_proc_macro_tcb`). The crate derives through
> `serde = aterm-serde` with `features = ["derive"]`. `--allow-l0-gaps` compiles it but forfeits
> proof authority, so no row here can be Certified until either aterm-serde-derive is an audited
> provider in Trust or the derives leave this crate (hand-written impls, or the config types moved
> to a crate of their own). This gap comes before gap 0 for every row, including the four
> gap-0-only specs in §6.1 (`momentum-glow-allowed`, `mo-load-shed-exempt`, `mo-serious`,
> `rkc-heroes`). Their tests stay.

Source: `crates/aterm-effects` (read only), every `#[test]` under `src/` and `tests/`, recovered
in eleven parallel chunks and merged here. `grep -c '#\[test\]'` reports 3,031; four of those hits
are mentions in comments (`tests/effects_suite.rs`, `tests/pet_holds_through_a_torn_frame.rs`,
`tests/trick_flash_alloc.rs`, `tests/typed_tricks_alloc.rs`), so the crate has 3027 tests.
Nothing was compiled or run; every claim comes from reading the code and the tests.

Result: **855 specs**, covering **2931 tests** as primary. **97 tests are not spec
samples** (§4). Every test the recovery read is in exactly one of §2 or §4; tests written
after it (the 2026-10 cursor-trail lanes' among them) are not listed, and nothing keeps this map
in step with the suite.

Specs that were the same property seen from different chunks were merged: the exact-tile baker
LRU (pet, animal and robi bakers), the bounded regular-file read (file_feed, toy packs), the
bounded Bresenham tail (cursor_trail, trail_sweep), `foreign_to_live` (seam laws, zsh isearch),
the launch look (kitty_cursor, kitty_registry), `motion_caret` (host, torn-repaint tests), the
cohort walk invariant (ribbon, the insert-colour and walk-census tests), the glow licence, payment,
park, quad and idle-to-zero laws (the cursor_glow unit tests and the host-seam integration tests).

## 0. Notation used in the statements

- Paths in §2 and §4 are relative to `crates/aterm-effects/`. Function names are relative to the
  crate (`cursor_glow::seam::…`, `rainbow_kitty::ribbon::…`); `mod::` is `rainbow_kitty/mod.rs`.
- **Gap numbers** are the rows of §7 in the historical development audit
  `docs/AUDIT-trust-for-test-migration-2026-10-15.md`.
  Row 0 (a seal that publishes Clean rows) blocks every spec and is listed on every row. f32
  arithmetic (exp, powf, sin, sqrt, round, casts, NaN) is refused by E6 today and has no row of its
  own; it is filed under 3 (body widening) and 6 (publishing SMT-proved clauses). The audit should
  give floats a row. `&mut self` two-state reasoning is 2; Vec/String/Option/struct carriers are 1;
  `ty_model!` refinement bindings are 10; cross-crate citations (aterm_core, aterm_render,
  aterm_lexicon, aterm_scene) are 12.
- **Reach(X)**: the states of engine X reachable from its `new()`/`default()` by any sequence of
  its public calls with a non-decreasing injected clock (`aterm_time::Instant`). Every behavioural
  spec quantifies over Reach and over call scripts; the tests are samples of such scripts.
- **CursorGlow** (`cursor_glow`): a *judged move* is a tick whose observed caret (or anchored
  print-run end) differs from the mirror; its *ring row* is the `admission_log` record (reason,
  licence, origin, target). A *licence* is any key-time provenance the seam reads: typed stamps and
  the press-credit ring (`PressCredits`, depth 128, patience P = `IN_FLIGHT_PATIENCE_S`), one-shots
  (Return, newline, Backspace quench, kill retreat, navigation, user gesture, synthetic note),
  delivered or armed inserts, and the in-flight pool. *Fresh* means within its class window
  (F = `TYPE_HINT_FRESH`, `INSERT_HINT_FRESH`, …); *spent* means consumed by the one move it
  licensed. *Mints nothing* means every presented plane (glow_add, under, halos, patches, char
  tints, fire halos, forge fill), sparks, particles, rings, crowns, bolts and vapor are unchanged,
  thermals and `typing_momentum` unchanged, no sound cue, no motion pulse, `spawns()` unchanged.
  *Idle0* means tick returns fp 0, every plane is empty, `!needs_frame_cadence`,
  `next_change_deadline == None` and `!is_active`. A quad is *banded* when it lies in the effects
  box and in one cell-row band whose row tag matches its y.
- **Ribbon** (`rainbow_kitty::ribbon`): a cohort's walk is `t_at(col) = walk_zero + walk_t(walk_d(col))`
  with `walk_t` slope 1/`WALK_FAST_CELLS` then `WALK_LAY_RATE`. `Lit(r)` means some under quad
  with alpha > 0 covers row r; *coverage* is the opacity-weighted share of a cell's pixel band.
  A *hole* is a dark column strictly between two lit columns of one row.
- **Host trace** (the `tests/*.rs` seam laws): PTY bytes fed to a real `aterm_core::Terminal`
  plus the key stamps `app_input.rs` makes (`note_typed`/`_expected`/`_glyph`, `note_motion`,
  `clear_typed`, inserts), presented in `app_render.rs` order (scroll seam, row probes for the caret
  row and every `ribbon_rows()` row, then `tick`) on a 16.667, 16 or 8 ms train or GUI pacing.
  *Twin(rec, off)*: the witness saw a copy of the record's glyph at that follow offset on its arming
  frame, or on every sampled frame over at least `TWIN_MIN` (40 ms).
- **PetBrain** (`kitty_pet`): a ~300-field `&mut self` machine whose `tick` is ~2,200 lines of f32
  control flow; `[BRAIN]` specs need gaps 1, 2, 3, 5 and 7 together. Seeds are `u8`; the tests
  sample 12 hands through the `#[cfg(test)]` `force_seed`, and every per-seed spec is stated over
  all 256. *Quiescent* means `!needs_frames()` and `next_change_deadline() == None`.
- **CursorCat** (`kitty_cursor`): M(σ,t) is the decayed `TypingMomentum` value; *active* means
  state ≠ Hidden.
- **Synth** (`trail_sound`): a script is a sequence of `push_meta` and `render` calls; *MB* (the
  music box) is an event routed to the rainbow kitty v2 voice.
- **Model refinement**: there is a projection π from the Rust state to the named
  `aterm_spec::derive` (or test-local) `ty_model!` state such that π(init) is Init, every real step
  maps to an enabled model action or a stutter, and every model invariant holds on every projected
  state, with the model's `Buggy = 1` twin as the negative control (`crates/aterm-spec/src/derive.rs`).
  Today these are Tier-1 tests; the spec is the projection lemma over all inputs.
- Spec-only reference functions: several tests compare against a `#[cfg(test)]` model
  (`cursor_trail::line_cells`, `effect_util` walked ramp, `simhash_majority_reference`,
  `generate_spectrum_table`, `peak_shade_quadratic`). Those become the spec's model, not code that
  ships.


## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `blocking_gaps` (audit §7 row numbers; every spec also needs row 0, the seal), `primary_tests` (how many tests it retires as primary; the list is in §2). The Rust features each proof must handle (the `rust_features` notes of the recovery chunks) are summarised by the gap rows.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
exact-tile-baker-lru	refinement	pet_baker::ExactTileBaker::{begin_frame,clear,tile,evict_lru,version,len,resident_bytes}; animal_baker::AnimalBakeKey::bake	Model M = (slots of (key, pixels, last_used), bytes, clock, bakes_left, version, cell). begin_frame(cw,ch): clock+1, bakes_left := MAX_BAKES_PER_FRAME, a metric change clears and stores the cell. clear(): no-op when empty, else slots := [], bytes := 0, version+1. tile(k) with (w,h)=k.size(): w=0, h=0 or 4wh > MAX_ATLAS_BYTES gives None with M unchanged (no bake spent); a hit gives Some(the cached pixels, exactly 4wh bytes), stamps last_used, version unchanged; a miss with no budget gives None; a miss with budget spends one bake, evicts the minimum (last_used, index) slot until len < MAX_SLOTS and bytes+want <= MAX_ATLAS_BYTES, inserts k.bake_tile().pixels() and bumps version. Invariants over every call sequence: len <= MAX_SLOTS, bytes = sum of resident tile sizes <= MAX_ATLAS_BYTES, keys unique, every cached tile equals the bake of its key, at most MAX_BAKES_PER_FRAME cold bakes per frame.	0,1,2,7,11	10
bake-tile-geometry	contract	animal_baker::bake_species; cat_baker::{bake_variant,bake_variant_with}; BakeKeyV4::bake	bake_species(id,f,w,h) is a w x h tile (pixels().len() = 4wh) and bake_variant_with(id,acc,f,w,h,eyes) a (w+PATCH_STRIP) x h tile; both are functions of their arguments only. Table half over ANIMAL_GLYPHS and GLYPHS: for w,h >= 16 every roster entry paints at least one non-zero texel.	0,1,3,7	2
catbaker-slot-count	contract	cat_baker::CatBaker::slot_count	slot_count(0) = 0; for ch >= 1, slot_count(ch) = min(floor(MAX_ATLAS_BYTES / (32*ch^2)), MAX_SLOTS) (widened in u64, so 0 above ch = 256). Corollary for all ch: u16: slot_count(ch)*(4ch)(2ch)*4 <= MAX_ATLAS_BYTES.	0,3	1
catbaker-atlas-lru	contract	cat_baker::CatBaker::{begin_frame,clear,get_v4,peek_v4,host_tile,host_peek,touch,bake_slot,install,version}	get_v4(k) is None with no state change when cell_h = 0, k.w = 0, k.w+PATCH_STRIP > slot_w, k.h = 0 or k.h > 2ch (host_tile likewise for w > slot_w, h > 2ch or short rgba). A resident key returns CatTile{art} holding the slot's own art (the same Arc), stamps last_used, version unchanged. A miss spends one bake even when refused; with no budget it returns None; otherwise it takes the first free slot, else the LRU slot (lowest index on ties) whose last_used != clock, and returns None when every slot was served this frame. install interns the w x h top-left of the baked tile as a fresh Rgba8Straight FxArt in slot i and bumps version; it never writes into an art it handed out, so a held art outlives its slot. begin_frame resets the budget to MAX_BAKES_PER_FRAME; a metric change clears (version+1 iff something was resident). Invariant: slot i's art equals the w x h top-left of key_i.bake() (or the host rgba).	0,1,2,3,7	7
resolve-layer	contract	cat_baker::resolve_layer	role in {Outline, Whisker} gives (fills.outline, 1.0); otherwise (fills.coat | fills.iris | rgb(layer.fill)) for recolor Coat | Iris | Fixed, alpha 1.0.	0,1,3	1
coat-noninterference	other	cat_baker::{bake_variant_with,paint_glyph,resolve_glyph_layer}; ResolvedFills::from_indices	For every glyph with no Recolor::Coat layer, bake_variant_with with coat1 equals the bake with coat2 (all other inputs equal). Sensitivity: for a glyph with a Coat layer of non-empty coverage, coats with different resolved colours give different tiles.	0,1,3,5,7	2
outline-contrast	table-property	ResolvedFills::from_context; cat_baker::{paint_outline_halo,contrast_ratio}	For accent in 0..=12, band b in 0..=3 and every background luminance L in band b's range, contrast(from_context(..).outline, L) >= 3 (the band edges suffice by monotonicity); for band 4 and every L in [0,1], max(contrast(outline,L), contrast(outline_underlay,L)) >= 3. outline_underlay.is_some() <=> background = 4. Dark bands get a brighter outline than light ones. The bake paints the underlay keyline under every Outline/Whisker layer exactly when it is Some.	0,3,5	5
coat-lift	contract	cat_baker::lift_coat_luminance; ResolvedFills::from_context	from_context(..).coat = L(rgb(mix(COAT_RAMP[min(coat,15)], accent, 0.12))) with L = lift_coat_luminance(., COAT_MIN_LUM_DARK_BG) for background in {0,4}, identity otherwise. lift_coat_luminance(c, floor): lum(c) >= floor gives c bit-exact; else lum(result) >= floor-0.005 and a chromatic c keeps its hue within 12 degrees. The four darkest ramp stops stay pairwise distinct after the lift.	0,3,7	1
color-key-quantize	contract	CatColorKey::{from_rgb,from_rgb_span,default}; cat_baker::background_band; CONTEXT_ACCENTS	from_rgb(bg,fg,sur) = {accent: 12 if sat(mix(fg,sur,0.35)) < 0.12 else floor((hue+15)/30) mod 12, background: background_band(bg) in 0..=3 by thresholds 0.04/0.18/0.58}. from_rgb_span sets background := 4 iff (band >= 1 or min(hi,3) >= 3) and min(lo,3) <= 1 and min(hi,3) >= 2, so lo = hi gives from_rgb. hue(CONTEXT_ACCENTS[i]) is within 0.6 degrees of 30i. Default = {accent 12, background 0}.	0,1,3	3
accessory-transform	contract	cat_baker::{accessory_transform,accessory_attach}	With (cx,cy,s) = accessory_attach(acc) and a = max(aspect_x1000/1000, 0.001), accessory_transform(acc,w,h) has scale_x/scale_y = a, scale_x <= s*w, scale_y <= s*h with one bound tight, and its box is centred at (cx*w, cy*h).	0,1,3	1
eyes-frame-transform	contract	cat_baker::{paint_glyph,bake_variant,bake_variant_with}; EyesFrame::openness	bake_variant = bake_variant_with(.., None, .., Open). For eyes != Open, Eye/Iris/CatchLight layers get scale_y' = openness*scale_y about the authored eye line and other layers are unchanged; Blink skips Iris and CatchLight. An accessory is composited only when Some; BakeKeyV4 equality is field-wise. Sensitivity: Open, Happy and Blink tiles differ for a glyph with a non-empty Eye layer.	0,1,3,7	2
reflection-containment	invariant	cat_baker::{paint_open_eye_reflections,paint_glyph,closest_eye_path,open_eye_reflection_eligible}	Let P/G be paint_glyph with reflections off/on. eyes != Open gives G = P. Every texel where G != P lies inside an eligible eye path closest to some catch-light and was opaque dark in P. Each eligible eye keeps at least half (and at least one) of its dark core texels. A glyph with no eligible eye has G = P.	0,1,3,5,7	2
codegen-variant-name	contract	cat_glyphs_codegen::{variant_name,load_glyphs}	variant_name(id) concatenates the non-empty '_'-segments of id with each first char uppercased; load_glyphs returns Err when two glyphs derive the same variant.	0,1,7	1
codegen-schema-fail-closed	contract	cat_glyphs_codegen::{glyph_from_doc,reject_unknown_keys}	excluded = true or kind not in INCLUDED_KINDS gives Ok(None). Otherwise any key outside GLYPH_KEYS / ANCHOR_KEYS / LAYER_KEYS yields Err naming that scope and containing "path-only", before any value is used.	0,1,4,7	1
cw-motion-spawns	contract	classic_wake::ClassicWake::{tick,spawn,fingerprint}	With last = Some(p), enabled, intensity > 0, cw,ch > 0, dark_theme: a tick with cur = Some(q), q != p in the grid, appends a non-empty set of quads ending at q and returns fp != 0 (0 -> 1 remap whenever out, under or halos is non-empty); no licence input is read. Every quad is inside the effects box and one cell-row band with matching row tag, alpha = 0, and |out| <= MAX_QUADS.	0,1,2,3,7,13	2
cw-rest	contract	ClassicWake::{tick,reset,is_active}	(a) disabled, intensity <= 0, cw = 0 or ch = 0 gives empty out, return 0, state cleared, !is_active. (b) With no move after the last spawn at t, every tick at now >= t + max(spark life, 0.18 s, CROWN_TYPING_MS) is empty, returns 0, !is_active. (c) After reset(), last = None and the next tick only re-observes.	0,1,2,3,7	3
cw-time-shift	equivalence	ClassicWake::tick	Time-translation invariance: a fresh ClassicWake run on (cur_i, t_i + d) gives the same fingerprints and quads as on (cur_i, t_i); tick reads time only through differences.	0,1,2,3,7	1
cw-color-faces	other	ClassicWake::{comet_color,emit_crown,emit_ring}	Quad geometry is independent of (classic_mono, color, accent). With classic_mono = false and no landing ring the colours are independent of color and accent; with classic_mono = true comet colour = lerp_rgb(accent, color, pos) and the crown is color. resolve_glow_config sets classic_mono for a Classic trail whenever cursor_trail_color is set (the one-colour face is a colour choice) or the authored spelling is a `classic mono` alias.	0,1,3,5,13	1
cw-run-pool-monotone	invariant	ClassicWake::emit_comet	Across every tick sequence comet_runs.len() never decreases and no pooled slot's capacity decreases.	0,1,2,7	1
companion-gates	contract	companion::{resident_pet_owner_present,cursor_companion_presentable,resident_pet_surface_presentable,pet_output_burst,sing_face_live,shed_companion_presentable,shed_envelope_transitioning}	resident_pet_owner_present(p,e,s) = p and e and s = RainbowKitty; cursor_companion_presentable(d,l) = d and l; resident_pet_surface_presentable(f,u,l,r) = f and u and (l or r); pet_output_burst(sc,adv,ex,live,alt) = (sc or adv) and ex and live and not alt; sing_face_live(d) = d >= 0.33 (false for NaN); shed_companion_presentable(b,e) = b and e finite and e > 0; shed_envelope_transitioning(a,e) = e finite and (a ? e > 0 : e < 1).	0,3	4
shed-companion-alpha	contract	companion::{shed_companion_alpha,shed_companion_presentable,shed_envelope_transitioning}	shed_companion_alpha(a,e) = round(a*clamp(e,0,1)) for finite e, 0 otherwise: <= a, non-decreasing in e, a for e >= 1, 0 for e <= 0. Single-grid and composed paths share these two functions.	0,3	1
companion-duty-on-glass	contract	companion::{cursor_companion_duty,cursor_companion_on_glass}	duty(p,k,c) = Pet if p; FlyingHead{cell} if not p, c = Some(cell) and k > 0; Idle otherwise. on_glass(Idle,..) = None; Pet maps body to at_body(cursor,b) with guards_caret = false; FlyingHead gives at_head(cell,h) or at_cell(cell) with guards_caret = true.	0,1,3	2
pet-geometry-maps	contract	companion::{pet_rect_hit,pet_hit_rect_win,pet_pointer_cell}	pet_rect_hit((x0,x1,y0,y1),x,y,s) <=> x0-s <= x < x1+s and y0-s <= y < y1+s (saturating i32). pet_hit_rect_win offsets the rect by the origin (saturating). pet_pointer_cell gives None for cw = 0 or ch = 0, else Some((col,row)) iff the floored pane cell is inside (cols,rows).	0,1,3	3
wrap-fact-edge	contract	companion::wrap_fact_edge	wrap_fact_edge(&mut seen, s, n) returns true iff seen = Some((s, m)) with m != n, and sets seen := Some((s, n)).	0,1,2	1
pet-arrival-mapping	contract	companion::pet_arrival_for_sync; kitty_registry::coat_distance	pet_arrival_for_sync(rung, auth, worn, inc) = Quiet if rung != Program or auth = Quiet or (worn = Some(o) and coat_distance(o.0, inc) < SUFFICIENT_DIFFERENCE); Ceremony otherwise.	0,1,3,4	1
pin-pet-mode-exit	contract	companion::pin_pet_mode_exit	f'.exit = Plain if pm else f.exit; every other field unchanged.	0,1,2	1
cat-color-key-footprint	contract	companion::cursor_cat_color_key; CompanionOwner::emit_on	Degenerate geometry, no cells or an empty clipped footprint gives from_rgb(fbg,ffg,facc). Otherwise with S the first 64 row-major cells of the footprint's cell rectangle, the key is from_rgb_span(avg bg, first visible fg or ffg, avg visible fg or facc, min band, max band); cells outside the rectangle never matter. Over blank glass the key is from_rgb(bg, ContrastFallback.cursor, ContrastFallback.accent).	0,1,3,7	3
pet-ink-binding	contract	companion::prepare_resident_pet_tick; CompanionOwner::feed_ink; WordDecorations::{bind_pane,pet_ink}	With pane = Some((s,o)), bind_pane(s,o) happens before pet_ink() is read, and pet.species() = species afterwards.	0,1,2,4	1
reduced-handoff-refines-model	refinement	kitty_pet::PetBrain::tick; kitty_cursor::CursorCat::{set_singing,static_frame}; aterm_models_effects::reduced_motion_companion_handoff_model	Under reduced motion every custody step projected onto (phase, singer_visible, pet_ready, pet_visible) is a transition of reduced_motion_companion_handoff_model, and the resident's alpha stays 255; a cold reduced-motion PetBrain's first live-caret tick has alpha 255.	0,1,2,10	1
pet-brain-settles	other	kitty_pet::PetBrain::{tick,needs_frames}; CompanionOwner::{prepare,tick,needs_frames}	Liveness: from every reachable PetBrain, ticks with caret = None reach needs_frames() = false within bounded time; on each such owner frame on_glass = false, alpha() = 0, hit_rect() = None. An unticked brain keeps its needs_frames value.	0,1,2,5,7	2
owner-custody	contract	CompanionOwner::{prepare,tick}	For every frame: owned = enabled and not serious and resident_pet_owner_present(..); visible = owned and resident_pet_surface_presentable(..); not owned retires the brain; on_glass = visible and pet.alpha > 0; pet.alpha ignores shed_envelope; duty = cursor_companion_duty(on_glass, pet_mode ? 0 : shed_companion_alpha(..), painted_caret); body_px = on_glass ? pet.body_px : None; hit_rect = pet_hit_rect_win(body_px, origin); alpha() = on_glass ? pet.alpha : 0. An obscured or unowned surface keeps nothing clickable; an obscured one keeps the brain active.	0,1,2,4	3
owner-sense-projection	contract	CompanionOwner::prepare	sense.write = frame_unfinished and not cursor_visible; a write frame reads the latched finished caret of the same live session, else (facts.caret, cursor_visible); the latch is keyed by the grid (rows, cols) as well and written only on present non-write frames. A finished frame's hidden cursor, in a session that has never bracketed a frame, is taken at a new cell only once it has rested there HIDDEN_REST; until then the caret taken before stands (the first reading on a session's grid at once). sense.caret = visible ? caret : None; escort_caret = caret, whatever visible is (the caret a host draws every cursor companion at); pointer = present and hit_rect and visible ? pet_pointer_cell(..) : None; reduced_motion = host.reduced_motion. A withdrawn, hidden or unpresented body senses no pointer.	0,1,2	5
owner-present-latches	contract	CompanionOwner::{prepare,drain,retire_coordinate_space,retire_owner,resight_owner}	On a present frame with key (session, cmd_done seq): a changed key gives completion = Some((code,dur)) iff the previous latch had the same session and cmd_done is Some; otherwise None. burst = pet_output_burst(scrolled, content advanced in-session, executing, display_offset = 0, alt). A capture frame reads and writes no latch. drain and retire_coordinate_space keep latches; retire_owner and resight_owner clear cmd_seq and content_seq.	0,1,2	5
owner-retire-family	contract	CompanionOwner::{set_enabled_seed,set_species,retire,retire_owner,drain,withdraw_hit_rect,enabled}	set_enabled_seed(e,seed) dresses the pet in for_launch(seed), arrival Quiet, rung Launch; not e retires. retire() clears brain, hit_rect, frames, glass, last_alpha = 0, statics. retire_owner keeps species and worn pair; drain gives a reborn default brain, keeps latches; withdraw_hit_rect clears only the rect; set_species applies at once and on every prepare. Default owner is disabled.	0,1,2,4	3
owner-press	contract	CompanionOwner::press	press(now,px) = Pet and pending_pets += 1 iff hit_rect = Some(r) and pet_rect_hit(r, px, PET_HIT_SLOP_PX); else Pass with no change.	0,1,2,4	1
owner-set-pointer	contract	CompanionOwner::set_pointer	A non-finite coordinate returns false and keeps the pointer; otherwise returns (stored != px) and stores px.	0,1,2,3	1
owner-cadence-law	contract	CompanionOwner::{tick,needs_frames,next_change_deadline}	needs_frames() = not last_static and last_shed_tick = None and pet.needs_frames(); tick sets last_static = reduced_motion and last_shed_tick = shed_active ? Some(now) : None. next_change_deadline is None if static, the pet's deadline with no shed tick, else max(pet deadline, at + RESIDENT_SHED_CADENCE) clamped to now + RESIDENT_SHED_CADENCE when not past now + ARM_MIN. A capture does not set last_static.	0,1,2,4	2
shed-refines-model	refinement	CompanionOwner::{prepare,tick,needs_frames,next_change_deadline}; test model ResidentShed	The ResidentShed model's invariants hold for Buggy = 0 and each fails for Buggy = 1; the shipping owner typing through a shed edge, projected onto (phase, smooth, gait, train, paced, coarse), is a model run.	0,1,2,7,10	2
owner-emit-hello	contract	CompanionOwner::{emit,emit_on,commit_hello_due,set_look}	Unless on_glass and duty = Pet, emit pushes nothing, returns the worn pair unparked and sets last_sync = None. Otherwise it syncs the look under pet_arrival_for_sync, records last_sync and draws the latched (coat, iris). commit_hello_due(present, o) = present and last_sync = Some((p, Ceremony)) and p != o.worn.	0,1,2,4	3
pet-still-postures	contract	kitty_pet::PetBrain::{tick,tick_static_capture}; CompanionOwner::tick	A fresh brain's first tick_static_capture with a live caret returns alpha 255 on glass; its first present tick returns alpha 0 with needs_frames. Under reduced motion every live-caret tick has alpha 255, lift 0, and the caret row immediately after a jump.	0,1,2,3	2
owner-note-peeks	contract	CompanionOwner::note_peeks	Drains every cue; for each, if last_visible it calls pet.note_peek(now, cell-centre fractions), else drops it.	0,1,2,4,7	1
claim-derive	contract	companion_look::derive_identity; kitty_registry::{app_basename,canonical_app_id}	derive_identity(b) = Some({id, basename, look: for_app(id)}) iff b is Executing, app_basename(commandline) = Some(basename) and id = canonical_app_id(basename) != SHELL_APP_ID; None otherwise.	0,1,4,7	1
claim-cache	contract	companion_look::ProgramClaim::resolve	resolve(s,b) = derive_identity of the block from the most recent call whose key (s, b.id, b.state, b.commandline.is_some()) differed from the previous call's key.	0,1,2	2
tenure-gate	contract	companion_look::KittyTenure::{observe,poll,deadline,worn}; companion_look::dwell_for	Invariant: a pending candidate differs from worn. observe(raw): raw = worn clears the candidate; a candidate held for its dwell lands; a different raw becomes the candidate at now. deadline = since + dwell (TENURE for a program, RELEASE for the base cat). poll lands a due candidate and returns whether worn changed. Worn changes only to a claim observed for its whole dwell; alternation faster than the dwell never lands.	0,1,2	4
tenure-flicker-refines-model	refinement	companion_look::KittyTenure::observe; aterm_models_effects::companion_tenure_flicker_model	An alternating claim storm below TENURE keeps worn() = None and every step is a model transition preserving FlickerNeverLands; a steady claim lands (negative control).	0,1,2,10	1
roster-rate-law	contract	companion_look::KittyTenure::{land,floor,commit_hello,note_non_program_verdict,arrival}; companion_look::insert_lru	land(None) gives Quiet. land(Some(id)) first evicts greeted foreign slots aged >= HOMECOMING; a resident id refreshes and arrives Quiet if greeted else floor(now); a stranger is inserted (free slot, else oldest greeted; refused when every slot is unpaid) and arrives floor(now) if inserted. floor(now) = Ceremony iff no hello within HELLO_FLOOR. commit_hello marks the worn id greeted. Invariants: an ungreeted slot is never evicted, |roster| <= HOMECOMING_SLOTS, ids unique; observe and poll land identically.	0,1,2,7	6
companion-precedence	contract	companion_look::companion_precedence	(f, Favourite) if f = Some; (a, Program) if f = None and a = Some; (l, Launch) otherwise.	0,1	1
typed-press-cascade	contract	companions::TypedPress::{feed,chars}; companions::Companions::note_typed_press	feed calls exactly on_break if brk, else on_backspace if backspace, else on_char once per char of typed then ime in order. note_typed_press applies this to tone_tracker, kitty_sing and kitty_summon in that order (summon's on_char only with words); windows equal a bounded-stack model.	0,1,2,7,11,13	1
cursor-body-inert	contract	CursorBeamRod::tick; CursorComet::tick; CursorDroplet::tick	Not enabled, cw = 0, ch = 0 or intensity <= 0 leaves out unchanged, fill = None, fp = 0 and not is_active().	0,1,2,3	3
cursor-body-activity	contract	CursorBeamRod/CursorComet/CursorDroplet::{tick,is_active}	For an enabled tick with blaze b: fp has bit 0 set; is_active() <=> clamp(b,0,1)*clamp(intensity,0,1) > 0.02; cur None or off grid appends no quads; the fill is resolved regardless (beam: None for a bar).	0,1,2,3	5
cursor-body-fill-ramp	contract	CursorBeamRod::tick; cursor_comet::ice_ramp; effect_util::water_ramp	Beam block channel c' = min(255, c + (255-c)(0.10+0.35e)), non-decreasing in e. Comet fill = ice_ramp(color, 0.34+0.40e+0.05(breathe-0.5)), channel-sum non-decreasing in t, keeping r <= g <= b order and, for small t, the dominant channel. Droplet fill = water_ramp(..), non-decreasing with b >= g >= r.	0,3	4
cursor-body-quads	invariant	CursorBeamRod::{emit_rod,emit_aura}; CursorComet::emit_coma; CursorDroplet::emit_bead; effect_util::push_fx_rect; cursor_glow::push_ripple_ring	Every quad is banded, has coverage <= COV_CAP+1, per-tick count < 400; comet/droplet quads keep the icy/water channel order for default hues; beam bar quads stay within one cell of the bar column and quads left of the cursor have alpha <= COV_CAP.	0,1,3,7	5
cursor-body-origin-translation	equivalence	CursorBeamRod::tick; CursorComet::tick; CursorDroplet::tick	Shifting the origin by (dx,dy) in an unclipped window appends exactly the translated quads. Coma and bead boxes strictly surround the caret cell; beam quads stay at y >= origin_y for a caret below the first row and y >= origin_y.saturating_sub(head) on row 0 (where the bar rod overshoots into the head band).	0,1,3,5,7	3
cursor-body-energy-monotone	other	CursorComet::emit_coma; CursorDroplet::emit_bead	For fixed phase, radius and core coverage are affine non-decreasing in e, so ink and extent are too; at full surge a drip reaches more than ch/3 below the cell and a ripple more than 1.4*RADIUS_MAX*ch out.	0,3,5,7	3
caret-style-law	contract	cursor_fx::compose_caret_style_override	Some(Bolt) if b; Some(SteadyBlock) if not b and r; m otherwise.	0,1	1
momentum-glow-allowed	contract	cursor_fx::momentum_glow_allowed	momentum_glow_allowed(u,h,r) = u and h and not r.	0	1
project-block-fill	contract	cursor_fx::project_block_fill; aterm_render::blend_rgb	e' = clamp(e,0,1) for finite e else 0; e' <= 0 gives None; otherwise the candidate with fill = blend_rgb(terminal, fill, round(255e')), owner and base kept; e' = 1 returns the candidate unchanged.	0,1,3,4,12	1
momentum-master-gate	contract	cursor_fx::CursorFx::tick	With input.master = false, glow_scratch is empty, block_fill is not owned by Momentum and momentum_steady = None.	0,1,2,4	1
head-dx	contract	cursor_fx::head_dx_for	0.08 for BlinkingBar or SteadyBar, else 0.5.	0,3	1
glow-no-licence-no-light	invariant	CursorGlow::{tick,classify_move,spawn,admission_tally}; cursor_glow::seam::move_licensed; spawn::classify_move	For every reachable CursorGlow, style and geometry, a judged move not covered by an unspent in-window licence of a matching class (typed stamp backed by an unpaid live press, one-shot, in-flight pool paying the exact shape, held press on the hand's row, fresh insert licence) mints nothing in any plane (no quad, spark, particle, ring, crown, bolt, vapor, cue or motion pulse; thermals and typing_momentum unchanged; spawns unchanged) and retires nothing; its ring row is a decline naming its reason; a declined relocation still consumes the one-shot hints it saw. Swallowed, stale, forgotten, paid and bare-timestamp presses license nothing.	0,1,2,3,4,5,7	25
glow-licence-grant	contract	CursorGlow::{note_typed_glyph,note_synthetic_typed,note_synthetic_move,note_user_gesture,tick}	A licence licenses at most one echo; a licensed typed, gesture or synthetic move mints light (spawns +1, a visible plane non-empty, crown armed, is_active); an expired licence licenses nothing. A batch that moves no caret runs no spawn, so the press survives to its own echo; several presses in one frame price one coalesced echo, licensed once.	0,1,2,7	5
glow-licence-refines-model	refinement	CursorGlow::tick; seam::move_licensed; aterm_spec CursorHintLicense model	Every observed step of the real engine through press, echo, cold echo and expiry, projected onto the CursorHintLicense variables, is a model transition, and forbidden steps are refused.	0,1,2,10	1
glow-exact-payment	invariant	CursorGlow::{tick,typed_credits_within,share_rule,note_typed_glyph,observe_row}; PressCredits::spend_counting; rainbow_coalesce pricing	Press credits are width-denominated and banked only by presses. A same-row forward echo of n cells is admitted as typing iff unpaid credits cover it in glyphs (a probed '\0' column counts as a wide glyph's second half) and, for n >= 2, at least two credits exist; an unpaid press stays payable until spent regardless of TYPE_HINT_FRESH age; a refused sweep records a no-credits decline and lights nothing. For every licensed move laying k typed cells: k <= RAINBOW_TYPED_SWEEP_MAX; typed_credits_within drops by exactly k (oldest-first, free before held); an inflight licence is granted only when live unpaid cells equal the cells laid; a last column already paid on wrap_paid_row is not charged again; presses banked after the frame's observation are not spent; every cell of the typed span is lit and nothing outside it.	0,1,2,3,4,5,7	29
glow-band-move	contract	CursorGlow::note_band_move; cursor_glow::scroll::{band_row,band_pos,translate_band_state}	band_row(row,top,bot,d) = Some(row) outside [top,bot], Some(row+d) when it stays in the band, None otherwise; band_pos saturates. note_band_move(top,bot,d): top > bot or d = 0 is a no-op; else band_moves += 1 and every row-addressed member in the band moves by d, members carried past the edge are dropped (Move ends saturate), members outside are untouched; licences kept, row-content classifiers retired; it spawns, judges and draws nothing.	0,1,2,3,7	5
glow-idle-to-zero	invariant	CursorGlow::{tick,is_active,next_change_deadline,needs_frame_cadence,under_quads}; thermal integrators	A fresh engine emits nothing until a licensed move. Safety: if under_quads() is non-empty after a tick then needs_frame_cadence() or next_change_deadline(..) is Some; is_active() or a deadline holds iff a visible change is pending (live light, a heating forge ember above threshold, a held park, a live fade). Liveness: after the last input at t there is a bound T (configured duration plus fixed tails, at most 2.5 s) such that every tick at now >= t+T returns 0, every plane is empty, !is_active, no deadline, and thermal integrators are exactly 0; a lone key's wake is gone within 500 ms; a ribbon drains without a visible step. An engaged v2 with nothing live is Idle0; under v2 no v1 crown or brisk pool holds cadence.	0,1,2,3,4,5,7,10,13	12
frame-fingerprint	contract	cursor_glow::FrameFingerprint::{glow_quads,chars,fire_halos,finish}; CursorGlow::tick	finish() = 0 iff no stream was fed; otherwise the FNV-1a fold over (tag, length, records) of every fed stream in order, with 0 remapped to 1. tick folds every presented plane, so fp = 0 <=> every plane empty.	0,1,7	2
glow-quad-invariants	invariant	CursorGlow::tick; cursor_glow::{emit,raster}; effect_util::{push_fx_rect,push_grid_quad,push_fx_quad}	For every style and reachable state, every quad in out, under and halos lies in one cell-row band with consistent tag, inside the effects box (the grid when head = 0), halo radii >= 1, |plane| <= MAX_QUADS. With head > 0 fire may rise into the chrome band only with tag 0, root at origin_y+2; with head = 0 nothing is above the grid.	0,1,2,3,4,5,7	9
glow-window-absolute	contract	cursor_glow::spawn; cursor_glow::emit (bridge vertex, debris, stardust)	Spawn geometry is window px = origin + grid px: the bridge vertex is at origin_x + (col+head_dx)*cw; jump debris and stardust lie within 0.4 cell of the leap segment; typing debris drifts opposite the motion; comet grains have near-zero gravity; lumen sheds no debris.	0,1,3,7	2
glow-heat-momentum	contract	CursorGlow::{tick,typing_momentum}; cursor_glow::thermal; cursor_glow::companion::typing_momentum	Each licensed key adds gain*cadence(gap) heat (capped at 1), decaying as exp(-dt/tau); wake brightness and fire light are non-decreasing in heat. Momentum advances on the press-hint half of a typing-shaped echo, never on program output: a lone key stays near cold, six fast keys stay below full blaze, a sustained run climbs, human cadence keeps an ember floor > 0.	0,1,2,3	8
typing-momentum-unified	equivalence	CursorGlow::typing_momentum; kitty_cursor::CursorCat; crate::typing_momentum	Under one keystream the glow engine's and the cursor cat's typing_momentum are equal at every instant; deletes only drain; the cat wake fires iff the shared value passes its band and dwell.	0,1,2,3,4	1
fire-thermal-refines-model	refinement	cursor_glow::thermal; test model emberforge_thermal_model	The emberforge model proves its invariants and catches its mutant; the real integrators at 16 ms ticks are model runs (one tier per frame, sustained bursts reach HOT, idle returns COLD with disp_t exactly 0). A licensed jump flares and cools within about a second; Backspace never raises disp_t and raises quench; navigation earns no heat.	0,1,2,3,10	4
fire-forge-and-vapor	contract	cursor_glow::thermal (forge); cursor_glow::spawn (steam, smoke)	Forge metal heats under sustained blaze with hysteresis, its fill is black-body (R >= G >= B), and it cools to exactly 0 (fp 0, timer disarmed). Smoke is emitted iff the metal is above the smoking point; steam grows with consecutive deletes; vapor decays to empty.	0,1,2,3	3
fire-no-recolor	invariant	cursor_glow::emit (fire patches, halo cells, crown); cursor_glow::raster	Fire never recolours ink; halo cells are sorted unique with strength rising toward the head; dark theme uses additive patches and crown, light theme FireMode::Over and no crown; embers over glyph cells are dimmer (FIRE_STREAK_COV_CAP < FIRE_HEAD_COV_CAP); patches and halos decay to empty.	0,1,3,7	5
jump-morphology	contract	cursor_glow::spawn::classify_move; CursorGlow::spawn	A licensed jump flies exactly one meteor/strike along its own vector that later unlicensed hops neither retarget, duplicate nor destroy; a typing wrap, a typed re-anchor and a coalesced or wide (<= HIDE_BRIDGE_TYPED_MAX_DIST) echo are continued typing in every style (no meteor, bolt, ring, flare or burst; each swept cell its own hue); an alt-screen preview without a blink keeps the jump; a line change snuffs the old row's trail within a beat.	0,1,2,7	9
reanchor-guard-equivalence	equivalence	cursor_glow::spawn::classify_move	For every bridgeable move (Chebyshev distance <= 2 across a short hide) the frames under a typed synthetic candidate equal those under a generic synthetic move (the re-anchor branch requires raw_dist > 2).	0,1,2,5	1
hide-bridge	contract	CursorGlow::tick (hide bridge); spawn::SpawnLane	A hidden frame between visible p and q is bridged iff the hide is short and Chebyshev(p,q) <= the bound (generic 2; HIDE_BRIDGE_TYPED_MAX_DIST with a typed candidate; the insert's reach for a licensed insert); a far teleport, a long hide or a reappearance with no candidate spawns nothing.	0,1,2	4
ribbon-continuity	invariant	CursorGlow::tick; rainbow_kitty::ribbon; spawn chain lifetimes	While a hand types on a row the live typed cells form one contiguous run whatever mix of advances and batched hops arrives; every swept glyph cell is lit and the landing is the caret cell; spark life chains to cadence so at any rhythm (down to ~0.55 cps) at least the last four typed cells are lit and the trail never goes dark mid-rhythm; a typing wrap folds the ribbon without inventing cells; a burst clamps the first key's orphan life.	0,1,2,5,7	10
ribbon-text-custody	invariant	CursorGlow::tick; rainbow_kitty (field, witness rows); seam rewrite arm	Ribbon light belongs to its text: under a TUI re-wrap, rewrite, box growth or composed-pane margin fold the light moves with the cells' text, cells whose text left retract farthest-first and are gone by 12n+240 ms plus the fade, cells left of the new caret keep their light, no light stays past a row's last glyph, the new row's lit cells are contiguous from the landing, nothing leaks into a neighbouring pane; a rewrite of the insert's own span re-witnesses the row; a one-cell park flushed as typing drains nothing.	0,1,2,7	9
ribbon-hue-equivalence	equivalence	CursorGlow::tick; rainbow_kitty hue walk	For every typed run, observing glyphs one frame at a time or as one batched echo lays the same spectral stop on every cell.	0,1,2,5	1
ribbon-geometry-and-caret-primacy	invariant	rainbow_kitty ribbon emit; cursor_rainbow::CursorRainbow; renderer compositing	A hot ribbon's six stripes stay ordered and inside their band while banking visibly; on the composited frame the brightest pixel is under the caret for both kitty spellings at every cadence, field, energy and flare.	0,1,3,7,12	2
ribbon-occupied-cov-cap	contract	rainbow_kitty::ribbon::clamp_bed_cov	For every requested coverage r the occupied-cell bed emits min(max(r, 0), RAINBOW_UNDER_COV_CAP), bound to RainbowOccupiedCoverage in aterm-models-effects. On a frame the bed draws by its glass (§14, Prism Glass 1.7) the body is planned under GLASS_ALPHA (255) instead of main's cap (Ribbon's body_cov_cap), and the frame's intensity scales the glass's height, not its coverage.	0,3	1
ribbon-retract-rearm	contract	rainbow_kitty::ribbon (retract/re-arm multiplier)	Reclaiming a retracting cell never changes its visibility discontinuously: at the reversal instant the multiplier after re-arm equals the one before at every fade depth, bound to RainbowRetractRearm in aterm-models-effects.	0,1,3	1
v2-noninterference	other	CursorGlow::tick; rainbow_kitty v2 engine	For every style other than RainbowKitty and every script, every observable the host reads is independent of the v2 engine, which never engages; ribbon_flat affects only RainbowKitty.	0,1,2,5	1
style-spelling-table	table-property	GlowStyle::{parse,style_names_any_pet,style_names_underline_ribbon,style_names_flat_ribbon,has_particles}; cursor_glow::style_has_beam	parse trims and case-folds; every shipped spelling maps to its style and an unknown spelling is Lumen; kitty pet spellings name the pet and flying spellings do not; appending underline/tall never swaps the companion; Beam has particles, Comet keeps a beam, rainbow kitty draws no shared beam.	0,1,7	4
style-hue-fidelity	invariant	cursor_glow::emit per style	Beam with its default colour keeps blue >= red on every quad; laser stays within the 70% white core mix of its colour; the comet tail brightens monotonically toward the head, stays below pure white, and its far tail is dimmer than the accent.	0,3,5	3
beam-rod-morphology	contract	CursorGlow::spawn (Beam); emit beam rod	A licensed beam jump lays sparks spanning the whole leap, no bolts, at most 14 stardust motes with gy = 0 and life >= 0.5 s, tail coverage >= 0.65 of the head's; past the hold the rod thins.	0,1,3,7	2
laser-and-sparkle-bursts	contract	CursorGlow::spawn (Laser, Sparkle); bolts	A licensed laser jump forges a main channel plus forks frozen at spawn and decaying to nothing; a hot laser typing run leaves a charged trail that bleeds and drains; sparkle births jitter within the cell window at full coverage, laser births stay at the point scaled by 1/sqrt(burst).	0,1,3,7	3
light-theme-veils	invariant	cursor_glow::emit (beam family, light theme)	With dark_theme = false the beam-family band, crown and ring emit no additive quads; the band is source-over ink under the letters (the under-ink stream, glow_under), held inside the swept row, and every halo, the ring's included, is source-over.	0,1,3	1
glow-disabled-inert	contract	CursorGlow::tick	cfg.enabled = false or intensity = 0 emits nothing and returns 0.	0,1,2	1
glow-resident-caps	invariant	CursorGlow::spawn	After every spawn |sparks| <= MAX_SPARKS and |particles| <= MAX_PARTICLES (oldest-first clamp), even for a same-instant flood with cfg.length = usize::MAX; the real Water spawn tracks the saturating CursorGlowResidentCaps model, which proves both bounds and catches the missing-clamp mutant.	0,1,2,7,10	3
hsv-fire-ramp	contract	cursor_glow::raster::hsv2rgb; effect_util::fire_ramp	hsv2rgb is the standard HSV to RGB map rounded to bytes; fire_ramp's red channel is non-decreasing in t.	0,3	1
erase-poof-licence	contract	CursorGlow::{note_kill,note_backspace,observe_row,tick}; spawn::poof_scan; scroll fence	A poof spawns iff a fresh kill or Backspace licence exists, the probe has Full trust, and either the probed row shows a same-row net shrink since the key (an exact one-cell proof for a plain Backspace) or, after POOF_FALLBACK_GRACE (released early once witnessed), the caret fallback finds a real shrink; at least POOF_MIN_GAP after the previous poof. Each poof consumes its licence; stale licences, identical repaints, hint-less shrinks and no-op Backspaces poof nothing; a scroll fence retires plain-Backspace provenance.	0,1,2,7	11
erase-poof-shape	contract	spawn::poof_scan; erase poof emit	A precise poof's puffs lie inside the vanished span (a fallback's near the caret); every delete shape on the default style emits a non-empty Poof cloud; puff count is non-decreasing in the span and the cloud is at most ERASE_POOF_SPREAD_CELLS wide; rainbow kills shed only Poof, fire kills shed Steam and raise quench.	0,1,3,7	3
erase-poof-drive	contract	cursor_glow::spawn::erase_poof_drive	erase_poof_drive(m,c) = (ERASE_MOM_FLOOR + (1-ERASE_MOM_FLOOR)*clamp(m,0,1)) * min(sqrt(max(c,1)), ERASE_WEIGHT_CAP): non-decreasing in m and c, bounded by ERASE_WEIGHT_CAP, and ERASE_POOF_STARS*drive(0,1) >= 3.	0,3,5	1
erase-sound-cues	contract	CursorGlow::drain_sound_cues; trail_sound::SoundKind	A plain Backspace cues Backspace on its retreat and Poof/Puff with its cloud, never Kill; a line kill cues Kill with each fresh licence; a word kill cues the word poof, never the clause swoosh.	0,1,2,7	1
glow-scheduler-pacing	contract	CursorGlow::{is_active,needs_frame_cadence,next_change_deadline}	With no moving light, a pending kill or Backspace licence keeps is_active() and asks a coarse poll (> fi), identically for both; an expired licence disarms. A visible spark asks frame cadence with deadline <= now+fi; a coarse forge ember does not.	0,1,2,3	3
insert-licence	contract	CursorGlow::{note_insert_delivered,note_insert_delivered_from,note_insert_armed,revoke_input_hints_at,revoke_failed_input_at,clear_typed,tick}; seam insert arm	An insert licence admits a hop of width w on row r iff unspent, fresh (INSERT_HINT_FRESH priced, INSERT_GESTURE_HINT_FRESH unknown), insert_row_identity holds, r was not branded before the arm, and w equals the priced width plus surplus paid from unpaid presses (or w <= INSERT_GESTURE_CELLS for the unknown class, counted once); admission spends it. A refused matching hop is remembered for one receipt and claimed only by a late delivery dispatched at or before the refusal (note_insert_armed claims none). A fresh licence survives a hidden boundary and clear_typed; revoke_input_hints_at revokes exactly the licence armed at t and restores its credit and tally. A program flood without delivery is declined; a licensed reappearance within reach is bridged.	0,1,2,7	21
insert-row-identity	contract	cursor_glow::seam::insert_row_identity; row brands	An unknown-width licence belongs to its witnessed row only (no row when unwitnessed); a priced licence belongs to its witnessed row for any fit and to an untyped unbranded row only for its whole width; a priced width equal to INSERT_GESTURE_CELLS is still priced; a row branded before the arm refuses even a whole-width match.	0,1,3	2
insert-effects	contract	seam insert arm; rainbow_kitty sweep	A licensed insert lays its whole span as one sweep joining the typed cohort, bounded by its width; it builds no momentum, sends no pulse, cues no sound and brands no row (a late receipt lifts the brand its echo applied); every other style mints nothing for it.	0,1,2,7	6
glow-park	invariant	CursorGlow::{tick,flush_held_park,is_active,next_change_deadline}; seam::{flush_park,indented_wrap_landing}; HeldPark::{patience,fresh}; v2 event buffer	A same-row backward hop (or non-wrap foreign-row hop) with a fresh stamp, delivered insert or in-flight pool behind it is held as a park (no verdict, nothing consumed, no ring row; an indented composer wrap onto pr + 1 is never parked); while held is_active() and the deadline is the park's own expiry (same row: just past F; foreign row: min(park + P, newest funding credit's expiry)). A return within the window paid by presses banked in the park's pre-park mask is judged as the single direct move (park_returns += 1, every typed cell lit); a return to the origin drops the park silently. Any other outcome flushes it at its own clock against the banks as they stood at the park (post-park stamps and presses set aside, even at equal Instant), park_flushed += 1, with the bare move's verdict, landing sweep only over a probed glyph, before any later licence, Typed event or Erase. Scrolls and band moves carry the park; a hidden boundary spares a fresh park.	0,1,2,3,4,5,7	22
motion-pulse-fold	contract	CursorGlow cursor-cat motion pulse; spawn::classify_move	Every licensed move's motion pulse carries classify_move's fold verdict dated at the move; a navigation-licensed relocation sends no typed-fold pulse.	0,1,2	1
typed-stamps-split-merge	equivalence	TypedStamps::{split_off_after,merge,stamp,take_fresh}	For every packed bank B0 and cut c, L = B.split_off_after(c) leaves B packed with the stamps <= c and L packed with the stamps > c, both in order; for every packed M, M.merge(L) equals stamping L's stamps into M one by one (oldest dropped first past TYPED_STAMP_DEPTH).	0,1,2,3,5,7	1
press-credits-foreign-to-live	contract	PressCredits::{foreign_to_live,within,payable}; CursorGlow::{note_typed_expected,note_typed_glyph,supersede_typed_press}	Order-independent over the live pool: foreign_to_live(t,ch) = None if the pool is empty or any live press is glyph-less or multi-cell; Some(false) if every live press has an exact one-cell glyph and at least one equals ch; Some(true) otherwise. So Some(true) holds iff the pool is non-empty, every live press has an exact one-cell glyph and none equals ch. note_typed_expected banks a glyphed press; note_typed_glyph banks a glyph-less one, making the answer None while it lives.	0,1,2,3,5,7,13	2
press-credits-exact-run	contract	PressCredits::{exact_unpaid_run,exact_unpaid_run_with_tail_window,retire_before_slot,exact_run_between,spend_counting,cells_within}	With live presses l_0..l_{n-1} chronological, exact_unpaid_run_with_tail_window(t,probe,gs,w) = Some(slot(l_s)) iff 1 <= |gs| <= 128, n <= 136 and s is least with l_s.. one-cell presses whose glyphs equal gs, the last within w, and every earlier press stale, before the probe and among the first 8. retire_before_slot blanks exactly the earlier slots; exact_run_between returns the 1..=2 payable one-cell non-blank presses in (d, v]; a run once retired and spent is never found again.	0,1,2,3,5,7	4
glow-backspace-retires-newest	contract	CursorGlow::{note_backspace,note_backspace_erasing}; PressCredits::{retire_newest_cells,retire_newest}	note_backspace_erasing(t, Some(w)) retires min(w,255) cells from the newest press (removing it at 0); None and note_backspace remove the newest press whole; no other press changes.	0,1,2,3,4,5,7	2
glow-forget-edges	invariant	CursorGlow::{forget_typed_credits,note_kill,note_return,note_navigation,note_scroll,note_band_move,note_typed_swallowed_no_echo,in_flight_tally}	The live press set changes only by banking, spending, Backspace retirement, ageing past P, and forget edges that remove exactly the presses banked at or before the edge: a kill key; the move a Return, composer newline or navigation one-shot licenses (never a glyph typed after it); a declined cross-row hop or flushed backward edge; an undescribable gesture sweep; an unknown-width insert's hop. Scrolls, band moves and same-cell hidden boundaries keep the pool. forgotten += 1 iff a live press was removed; note_typed_swallowed_no_echo banks nothing and counts swallowed_no_echo.	0,1,2,3,4,5,7	15
glow-one-shot-once	invariant	CursorGlow::{tick,note_return,note_newline_break,note_backspace,note_kill,note_navigation,clear_typed}	A one-shot licenses at most one judged move and is cleared by the first move it licenses, whatever its shape; a glyph echo paid by an unpaid press neither consumes nor is attributed to it; a kill yields at most one retreat licence even across a band move or scroll; a refused stray consumes no stamp; a superseding edit, navigation or kill key cancels a pending composer-home at the key, a Tab gesture keeps it.	0,1,2,3,4,5,7	7
glow-program-row-never-spends	invariant	CursorGlow::{observe_print_anchor,echo_anchor_pass,tick}	An anchored-lane observation on a row that is not the established echo row, or carries a program-row brand, spends no stamp or press, lays nothing, and is a program-row decline. Only a keyless end-advance brands a row, the echo row is never branded, the 16-row anchor memory evicts least-recently printed rows, a visible caret move owns its tick, and a declined visible hop restores last_licensed_row. Hence judged input-row echoes equal keys typed.	0,1,2,3,4,5,7	12
glow-hand-row-hold	invariant	CursorGlow::{hold_the_live_presses,clear_typed_keeping_hand,note_return_keeping_hand}; HandRow::carried; PressCredits::hold_through; TornLay	While the hand types on row h, a run ending on another row is no key's echo whatever glyph it ends on; it forgets presses only when it is the keys' own echo below the caret with the hand off the caret row, else it holds them; a held press pays only for a move staying on h; line-editing keys, row-keeping scrolls and Enter into a foreground composer keep hand_row; a torn lay recalled at the next key leaves the key's hand on its row; a key that keeps the hand (clear_typed_keeping_hand) ends a lay's wait for the caret's rest once the caret moves, so a caret moved behind it keeps the lay, and one that rests where the key found it is recalled, handing back only what the lay took (the hand it put on its row, and the doubt it settled there unless a navigation key or Enter settled the doubt since), never over a hand or doubt that changed since the lay; a lay with no live cell is never recalled.	0,1,2,3,4,5,7	11
glow-landing-glyph-gate	contract	CursorGlow::{glyph_before,classify_move,lifted_word,observe_print_anchor,observe_row}	glyph_before(row,end,t) is the host's glyph for its own anchor run end, else the glyph left of end in this frame's probe of that row (caret or flanking row), else None. A typed re-anchor lays its landing iff foreign_to_live(t, glyph_before(landing)) != Some(true) (a soft wrap witnesses at the origin); otherwise it is a program-row decline that forgets the press. A lifted word lays no landing and is never refused by the gate.	0,1,2,3,4,5,7	4
glow-unknown-insert-orphans-refine	refinement	CursorGlow::note_insert_delivered_from; InsertSeam::retire_orphans; PressCredits::exact_run_between; CursorGlow::tick	The unknown-width-insert orphan escrow refines the derived unknown-insert seam model: for every accepted action sequence the engine's spawns, escrow presence and credits match the model, it never lights a queued key's cell for a rejected action (partial, wrong glyph, stale, unprobed, alt screen without blink, unchanged blank, unrelated print), and the escrow drops on expiry, scroll, rewrite or reset.	0,1,2,3,4,5,7,10	7
bridge-reach-contract	contract	nav_bridge_source_ok; nav_bridge_admits; hide_bridge_reach; BridgeReach::admits	nav_bridge_source_ok(key,seen) <=> key-seen <= HIDE_BRIDGE_MS and seen-key <= NAV_NOOP_SETTLE_MS (saturating ms); nav_bridge_admits adds now-key <= NAV_BRIDGE_LANDING_MS. hide_bridge_reach(typed,nav,pane) is, for nav, cols = max(pane width or HIDE_BRIDGE_NAV_UNPANED_COLS, HIDE_BRIDGE_TYPED_MAX_DIST), rows = HIDE_BRIDGE_NAV_MAX_ROWS, line_start = pane first column; else the Chebyshev ball of HIDE_BRIDGE_TYPED_MAX_DIST or HIDE_BRIDGE_MAX_DIST. admits(from,to) <=> |dr| <= rows, |dc| <= cols, and for line_start with |dr| >= 2, to is above and within HIDE_BRIDGE_TYPED_MAX_DIST of the line start.	0,1,3,5,13	2
glow-nav-bridge-refines-reach	refinement	CursorGlow::{tick,note_navigation,drain_sound_cues}; nav_bridge_admits; hide_bridge_reach	A hidden-to-visible relocation under a navigation hint is bridged (one licensed move, one nav cue, no meteor) iff nav_bridge_admits and hide_bridge_reach(..).admits hold (CursorTrail decides the same); otherwise it is declined, lays nothing and cues nothing. So n word keys whose echoes land give exactly n nav cues, and a held key whose repaint relocates the caret rows down gives none.	0,1,2,3,4,5,7	8
glow-key-time-click-ledger	invariant	CursorGlow::{cue_keystroke,take_key_cue,drain_sound_cues,settle_key_seam,sound_seam_open,tick}	cue_keystroke(t) appends exactly one Typed cue at the pre-echo caret and increments keyed_clicks iff the key seam is open; otherwise it records nothing. take_key_cue pops only that cue and keeps the ledger. Each admitted echo with keyed_clicks > 0 spends a credit and cues nothing (an exact deletion keeps the credit); with none it cues itself. Stale credits drop; a closing seam zeroes the ledger; a denied program move retires the credit but never a queued cue. Each printable key is heard exactly once.	0,1,2,3,4,5,7	8
glow-key-time-heat	equivalence	CursorGlow::{cue_keystroke,spawn}	The heat on key i's key-time cue equals (within f32 rounding) the heat its echo-time cue carries in an engine that cues only at the echo; a burst's first key carries cold heat.	0,1,2,3,4,5,6,7	1
glow-cue-classification	invariant	CursorGlow::{spawn,spawn_move_light,poof_scan,drain_sound_cues,tick}	Every cue is recorded at a licensed spawn edge with that spawn's class (typing Typed; jump, preview or snapping line feed one Jump; same-column line feed Typed; Backspace shrink Backspace; kill swoosh; navigation nav tick, no heat); the newest erase key owns the pending poof's voice; a v2 nav jump speaks only its meteor; a paste under rainbow kitty records one Strum and banks one silent credit, other styles nothing; a disabled engine and a style switch drop pending cues.	0,1,2,3,4,5,7	5
glow-typed-cell-lit	invariant	CursorGlow::tick; rainbow_kitty::ribbon::Ribbon; Cohort::t_at; Ribbon::fresh_line	Under rainbow kitty every typed cell of a licensed echo is lit on the echoing tick and a panel period later (born at max(press, t-F)); while a line is typed no cell inside its live span is dark; an unechoed key lays nothing (only the fold's previous-row last cell); Cohort::t_at(col) is independent of echo lag; a composer newline starts a fresh walk; a short pause leaves no hole.	0,1,2,3,4,5,7	7
glow-kill-once-v2	invariant	CursorGlow::{note_kill,poof_scan}; rainbow_kitty Engine::kills	Each kill key emits exactly one v2 Kill event, from whichever of its retreat or poof is witnessed first, and the Kill precedes the retreat's Move, so no cell under the prompt is lit at or after the retreat.	0,1,2,3,4,5,7	2
glow-status-row	contract	TrailStatus::{line,ribbon_active}; AdmissionRecord::line; CursorGlow::{celebrate,ribbon_segments,ribbon_hue_bands,v2_status}	The trail status line is one newline-free line with the fixed key sequence (style, resolved, gates, licensed, declined, last_decline_reason, spawns, ribbon fields, momentum, flow, combo, block fill provenance, insert, inflight, park tallies, then issue, comet_cells and comet_alpha_max), each value the engine's own counter; resolved= is the resolved style's label (a fallback's is the default it draws, issue= naming why), else off; ribbon_active == (segments >= 2 and bands >= 2) or a drawing curtain with ribbon_drawn >= 2; flow and combo rise past FLOW_KEY_DISP, latch once per crossing, reset on Backspace, kill or a sub-floor key without lowering combo_best, and freeze while celebrate drives.	0,1,2,3,4,5,7	7
glow-style-parse-and-v2-engage	table-property	GlowStyle::parse; CursorGlow::{tick,v2_status}; rainbow_kitty Engine::engaged	parse(s) == RainbowKitty iff trim-casefold(s) names the kitty (rainbow kitty, kitty, nyan, rainbow, the flying/tall/underline presentations, the v2 aliases), never a v1 spelling, another style, v1, v2, pack:* or empty. Every RainbowKitty config engages v2 on its first non-degenerate tick; every other built-in style never does.	0,1,2,3,5,7	1
glow-v2-meteor-frame-zero	contract	CursorGlow::{tick,under_quads,halos,caret_flare_at,take_companion_impulse}	Under v2 a credited same-row navigation move of at least JUMP_MIN_CELLS draws the meteor's train, white heat and coma on its observed tick (fp != 0), lays no v1 light, records only the meteor cue, asks frame cadence, sets caret_flare_at = Some(t), and offers the companion impulse once.	0,1,2,3,4,5,7	1
glow-degenerate-geom-no-panic	no-panic	CursorGlow::tick	For every reachable engine, config and Geom with rows, cols, cw or ch zero, tick does not panic, returns 0, leaves out empty and does not engage v2.	0,1,2,3,4,5,7	1
glow-style-switch-crossfade	invariant	CursorGlow::{tick,tick_fades}; fields fading, ramp_in_at	On a tick whose style or pack fingerprint changed, the live light moves into one outgoing fade (a pack swap drops old sparks), |fading| <= FADE_CAP with the oldest shed, each fade's mass strictly decreases and retires by its end, no frame with a live fade is empty, is_active holds, heat carries over; with no switch fading stays empty and ramp_in_at None.	0,1,2,3,4,5,7	4
glow-bolt-jag-deterministic	equivalence	CursorGlow::{spawn_bolt,emit_bolts}	For every seed and geometry the strike polyline is the same whatever strikes used the pooled jag buffers before.	0,1,2,3,4,5,7	1
glow-crown-radial	contract	CursorGlow::tick (crown channel); CursorGlow::halos	For every non-fire built-in style the crown is RainHalos only, radius >= 1, at least one centred on the head cell.	0,1,2,3,4,5,7	1
classic-ring-grade	contract	classic_ring_radius_factor; classic_ring_life	classic_ring_radius_factor(s) = 1 for non-finite s, else 1 + 0.4*(clamp(s,1,IMPACT_MAX)-1); classic_ring_life(b,s) = b for non-finite s, else b*(1 + 0.267*(clamp(s,1,IMPACT_MAX)-1)). Both are monotone in s with floors and caps (2.0, b*1.6675).	0,3,5	1
glow-landing-ring-raster	contract	CursorGlow::{spawn_landing_ring,emit_ring,emit_ring_rim}; classic_ring_radius_factor	A style whose landing light is a veil (every style that reads the Ring knob but Water, and Fire on dark) lands a round rim about the caret's cell, 0.7 to 1.7 rows across its life, faded at the effects box: on dark additive quads in the veil's hue over a soft halo veil bound to the caret's row; on light source-over over-text ink under LIGHT_INK_ALPHA_CAP over a source-over veil (cap 1..=120); a style that reads no Ring knob lands no rim; a capped-impact ring reaches further than impact 1 at equal age; a halo centre past the box stays true and a negative centre culls.	0,1,2,3,4,5,7	4
rainbow-sweep-and-bands	table-property	rainbow_sweep_at; rainbow_band_at; RAINBOW_BANDS; spectrum::spectrum_stop	RAINBOW_BANDS[i] == spectrum_stop(i), RAINBOW_BANDS[0] == 0xFF0000, entries distinct. rainbow_sweep_at(col,phase) is in [0,1], a reflected (non-wrapping) sweep with bounded per-step change attaining both ends; rainbow_band_at returns one of the seven anchors as a function of (col, phase) only.	0,3,5,6	2
glyph-tint-palette	contract	fresh_ink_glyph_palette; fresh_ink_glyph_ink; CursorGlow.glyph_palette memo	fresh_ink_glyph_palette(fg,bg) is None if either is unset, fg == bg, or contrast is below the usable floor; otherwise every band and weight keeps the box-corner colour's luminance on the usable side of the cap (1e-6). The bisection predicate is monotone and its answer satisfies it. The engine memoizes per (fg,bg) and re-derives exactly when they change.	0,1,3,5,6,7	4
light-ink-laws	contract	light_ink; light_ink_bold; LIGHT_INK_ALPHA_CAP	light_ink(rgb) has luma <= LIGHT_INK_MAX_LUMA and max channel <= LIGHT_INK_MAX_CHANNEL, keeps channel order, and is the identity under both bars; light_ink_bold lands chromatic input on the luma bar preserving hue, maps achromatic non-black to 0x787878 and 0 to 0; every band at LIGHT_INK_ALPHA_CAP stays dark over near-black ink and visibly darkens white.	0,3,5,6	3
water-style-laws	invariant	CursorGlow::{tick,emit_water}	Under Water every quad has blue >= red; a slow key's light is droplets falling below the row; a hot wake is one connected travelling curve within MAX_QUADS with no full-cell slab; a licensed Home/End move lays a full-vector crest with no heat, ring or splash, a one-cell arrow is no streak, an unlicensed relocation is dark; repaint corridors keep one owner per coordinate and never change composited glyph ink; a jump's reflection drips and fades to exact zero.	0,1,2,3,4,5,7	6
sparkle-glitter-survives	invariant	CursorGlow::{tick,spawn_burst_particles}	Under Sparkle a glitter grain retires only by age or a wholesale teardown; no unlicensed batch or licensed advance removes a live grain.	0,1,2,3,4,5,7	1
glow-witness-rows	contract	CursorGlow::{ribbon_rows,witness_row_slot}; rainbow_kitty::witness; NbrProbe	Under v2 star pixels are gated by v2's occupancy bitsets alone; filling a far witness row takes one terminal read into the engine's slot, and a repeated row replaces its slot; ribbon_rows(buf) returns WITNESS_ROWS + ARMING_ROWS rows when buf is large enough (content rows first, then the pending key's source and arming rows), and WITNESS_ROWS for a short buffer.	0,1,2,3,4,5,7,13	3
line-cells-tail-is-bounded-bresenham-suffix	equivalence	trail_sweep::line_cells_tail; cursor_trail::line_cells (spec-only model)	For all origin o, destination d in i32^2, limit and include: after line_cells_tail(path,o,d,limit,include), path = the last min(limit,|B|) cells of the forward Bresenham walk B from o to d, with d dropped iff not include. So |path| <= limit, cells are Chebyshev-adjacent, include implies the last cell is d, and limit = 0 gives []. The i128 lattice arithmetic cannot overflow.	0,1,2,3,5,7	4
typing-rate-value	contract	TypingRate::{value,on_key}; MomentumGlow::{value,on_key}	value(now,tau) = 0 if unstamped or tau not finite or <= 0; else v = value*exp(-dt/tau), 0 below SNAP_ZERO, capped at 1: finite, in [0,1], non-increasing, exactly 0 after finite time. on_key adds KEY_CREDIT to the decayed value (capped) and stamps now; a periodic key stream's value at its last key is strictly increasing in rate; engines fed the same timestamps agree bit-for-bit.	0,1,2,3,5,6	4
momentum-glow-tick	contract	MomentumGlow::{tick,is_active,settling,cold_at,next_change_deadline}; ramp	With a = clamp(rate*clamp(intensity,0,1),0,1) and a_q = floor(255a)/255: tick is inert (no quads, fill None, fp 0, neither active nor settling) when there is no cursor, disabled, intensity <= 0, zero cell, a < MOMENTUM_GLOW_HOT, or off grid; otherwise hot, fill = mix(base, ramp(a_q), 0.55*smoothstep(a_q)) iff block, every quad inside the box within (0.55 + radius_cells)*cw + 1 of the cell centre; on a dark page additive light whose quads lay at most OVER_INK_COV_CAP on any one pixel, on a light page one source-over ink mark in the under stream, in cfg.hue's ink solved against the theme text (LIGHT_GLOW_INK_BAR), held inside the caret's row, at most LIGHT_GLOW_PEAK; is_active() == hot and fp changed, settling() == hot and fp unchanged; next_change_deadline is None when neither, now + interval while active, and while settling the earlier of the latched amplitude's next u8 step and cold_at() = stamp + tau*ln(a/MOMENTUM_GLOW_HOT), never before now + interval; ramp is monotone in luminance on dark, white at the top on dark and below white on light.	0,1,2,3,5,6,7	7
phaser-tick-laws	contract	CursorPhaser::{tick,is_active}	Disabled or zero intensity gives fill None, fp 0, empty out, not active; a hidden cursor gives no quads but a fill; otherwise every quad is on the cursor row outside the cell's interior, single-row, in the grid, capped, |out| < 200; is_active iff charge is above settled; a settled emitter is identical across present gaps.	0,1,2,3,5,6,7	7
phaser-fill-and-charge	contract	CursorPhaser::tick; PhaserConfig::base	At full charge the fill follows the beam hue; with no base the settled fill is near white on dark and near black on light; with base Some(c) the settled fill is c on either polarity and charge moves away from it; wing ink and reach are strictly larger at full charge; idle is still lit.	0,1,2,3,5,6	4
rainbow-caret-inert	contract	CursorRainbow::{tick,is_active}	!enabled or intensity == 0 gives fill None, fp 0, no quad, not active.	0,1,2,3,5	2
rainbow-caret-flare	contract	CursorRainbow::tick; CursorGlow::{caret_flare_at,tick}	With flare = Some(t0) the tick at t0 fills 0xFFFFFF, later ticks lie strictly between white and the settled fill until the spring settles, then equal the flare-None frame; after a credited v2 jump the settled fill is the paint law applied to the landing head colour, and each step after the white frame is under 40/255 per channel.	0,1,2,3,4,5,7	2
rainbow-caret-fill	contract	CursorRainbow::{tick,tick_with_family_phase}; shade; mix_rgb; lift_to_light_floor	At rest the fill is near the base (near white dark, near black light, or the host base on either polarity) and saturates toward the spectrum under energy; a host-supplied ribbon head colour is authoritative for the hot body; otherwise the caret resolves the family sweep at its own column; full energy shade is the identity; a hot run spins faster and glows brighter; the light-theme glint keeps saturation; caret and ribbon share one clock.	0,1,2,3,4,5,7	8
rainbow-caret-geometry	invariant	CursorRainbow::tick	Every halo and twinkle quad is single-row, in the grid, premultiplied channels <= 180, and reaches at most half a cell width past the cursor cell.	0,1,2,3,5,7	3
rainbow-caret-lane	invariant	CursorRainbow::{is_active,settling,next_change_deadline,tick}	is_active() == energy > SETTLED or twinkling or (paint > SETTLED and fp changed); settling() == !is_active() and paint > SETTLED (a partition). At settled energy phase flips arm nothing and a steady block never twinkles; with paint 0 two ticks give equal fp and the rim freezes; settled frames are identical across present gaps. next_change_deadline is now+frame while active, None once unpainted, and during a release lands one frame before the first dry-run frame that differs, within the poll cap; the dry run is memoised.	0,1,2,3,5,6,7	8
rainbow-flare-rungs	refinement	CursorRainbow::tick_inner; MOMENTUM_FLARE_STEPS; MOMENTUM_FLARE_HYST	With rung(p) = #{i : p >= STEPS[i]}, a charged blinking tick that raises the rung starts exactly one flare generation however many rungs it crosses; descent uses HYST and starts none; a flare ends exactly TWINKLE_DUR later and the output then equals a never-flared twin; identical inputs give identical quads. The projection refines the derived Nyan blink model, and the idle-restart mutation is rejected.	0,1,2,3,4,5,7,10	5
rainbow-colour-continuity	invariant	CursorRainbow::{tick,tick_with_family_phase}; spectrum_at; RAINBOW_PHASE_RING	The spin accumulator stays in [0,2) and each ring colour moves per 16 ms frame by at most the continuity ceiling, across the wrap; the unit clock traverses one period-two sweep with no jump; phase 0 equals phase RAINBOW_PHASE_RING; fallback fill and offsets change by at most the sweep-step ceiling; halo coverage follows paint and is non-increasing along a falling paint; during a release the fill moves by at most the spine ceiling and ends at the energy-law fill.	0,1,2,3,5,6,7	7
rainbow-rim-light	table-property	RIM_LIGHT; RIM_LIGHT_GREY; CursorRainbow::tick	rel_luminance(RIM_LIGHT_GREY) == RIM_LIGHT within a byte, equal to the arc's mean light; for every shipped base, dark page, shipped paint and the 129 spin positions the rim's light varies by at most RIM_LIGHT_TOLERANCE (IDLE at rest) and is monotone in paint, while the raw arc spans more.	0,1,3,5,6,7	2
trail-licence-refines-model	refinement	CursorTrail::{tick,spawn,move_licensed}	CursorTrail's licence slots refine the effects licence model: a move mints light only with a fresh correlated candidate or class stamp, which it consumes; a gesture supersedes swallowed classes; navigation is licensed but lays no comet; a typed re-anchor lays no comet while a candidate-backed jump keeps it; declined relocations consume every class dark; cold moves mint nothing and retire nothing; first observations and hide gaps spawn nothing; an Enter's response leaves a type-ahead stamp; a composed margin fold lights only its typed margin cell with pane authority.	0,1,2,3,4,5,7,10	9
trail-scroll-band-translate	refinement	CursorTrail::{note_scroll,note_band_move}	A scroll of n rows or a band move by delta moves every spark and anchor by exactly the delta when it stays on grid (inside the band), removes what leaves, never clamps; refines the scroll decision model (stranded and off-top mutations rejected).	0,1,2,3,4,5,7,10	2
trail-comet-emission	contract	CursorTrail::{tick,spawn}; READABLE_ALPHA_CAP	Disabled emits nothing but tracks the cursor; a licensed same-row jump lays at most max_len cells on the source row excluding the destination, alpha non-increasing away from the cursor; a comet decays to exactly empty after its life; every cell alpha <= READABLE_ALPHA_CAP; ignition is brighter and lives 1.6x; spark life = duration*(1+1.5*warmth); fp folds colour and fp == 0 iff out is empty.	0,1,2,3,5,6,7	7
trail-dedup-equals-linear-merge	equivalence	CursorTrail::tick	The emitted cells (content and order) equal the reference fold merging each spark by a linear rescan, and fp == 0 iff out is empty.	0,1,2,3,5,7	1
trail-presentation-projection	contract	project_trail_presentation; trail_presentation_fingerprint	With o = clamp(opacity,0,1) (0 if non-finite), each alpha a becomes round(a*o), zero-alpha cells are dropped, and the result's fingerprint is returned (0 when empty); trail state is untouched; for non-empty input the fp equals the unprojected one iff o == 1.	0,1,2,3,5,6,13	1
typing-cadence-ignition	contract	ignition_intensity; TypingCadence::{on_keystroke,intensity,warmth,sample}; ignite	ignition_intensity(h,lo,hi) is the clamped smoothstep (step at hi when hi <= lo): total, in [0,1], monotone, 0.5 at the midpoint. heat decays by half-life; on_keystroke sets heat = min(heat_at+gain, knee_hi); warmth = clamp((heat_at-gain)/max(knee_lo-gain,EPS),0,1); sample == (intensity, warmth) bit for bit. A lone key has warmth and intensity 0; steady-state heat for period p is min(gain/(1-0.5^(p/half_life)), knee_hi); ignite clamps and heats colour at most halfway toward cursor_comet::ICE_WHITE (0xF2FAFF), the comet nucleus's own freeze target.	0,1,2,3,5,6	8
dog-baker-contract	contract	DogBaker::{tile,begin_frame}; bake_breed; DogBakeKey::host_id	A Some from DogBaker::tile(k) is exactly bake_breed(k.breed, k.fills(), k.w, k.h).pixels() (4wh bytes); at most MAX_BAKES_PER_FRAME misses bake per frame; len <= MAX_SLOTS and resident_bytes <= MAX_ATLAS_BYTES; host_id is a pure function of the key with bit 63 set; coat, breed and background each change texels for some key.	0,1,2,3,5,7	3
dog-roster-table	table-property	DOG_HEADS; DOG_GLYPH_IDS; DOG_GLYPHS; DogBaker::aspect; bake_breed	At least 8 breeds with equal table lengths; finite positive aspect and non-empty layers; for every breed and coat in 0..16 a 96x82 bake covers > 30% of its middle band and uses the required number of opaque colours.	0,1,3,7	2
dog-cameo-envelope	contract	DogCameo::{on_summon,alpha,active,bob,look}	alpha is 0 before a summon and after FADE_IN+HOLD+FADE_OUT, a linear fade in, 255 hold, linear fade out; active iff within that window; look survives expiry. on_summon is a pure function of (seed, resident breed), breed in DOG_HEADS, coat < 16, and an active dog is never re-rolled onto itself. bob = 0 outside the hops, else -(HOP_LIFT/2^hop)*sin(pi*phase) <= 0.	0,1,2,3,5,6	3
push-grid-quad-contract	contract	push_grid_quad	push_grid_quad pushes nothing for w <= 0, h <= 0, (premul, alpha) both 0, or an empty grid intersection; otherwise one quad per touched cell row in increasing order with the clipped span, color = color2 = premul and alpha = alpha2 = alpha (a source-over quad with zero premul is still pushed).	0,1,2,3,5,7	1
twinkle-envelope-law	contract	twinkle_env; twinkle_peak; TWINKLE_FLASH_HZ	twinkle_env(age,phase) = TWINKLE_FLOOR + TWINKLE_DEPTH*|sin(age*OMEGA+phase)| lies in [FLOOR,1] attaining both ends, with period pi/OMEGA so its peak rate is OMEGA/pi <= TWINKLE_FLASH_BOUND_HZ; twinkle_peak(env) <=> |sin| >= TWINKLE_GLINT_FRAC with duty 1 - 2 asin(GLINT_FRAC)/pi.	0,3,5,6,14	3
star-arm-and-silhouette	contract	star_arm_px; push_twinkle_star; star_waist_px; star_core_px; star_body_px; star_taper_cov	star_arm_px(arm) = max(arm as i32, 1). For arm >= 1 push_twinkle_star draws a plus with body thickness round(2 arm STAR_WAIST), body extent round(arm STAR_TAPER_BODY) at full coverage, total span exactly 2 arm + 1 contiguous columns, points dimmer and non-increasing outward, a nucleus of round(2 arm STAR_CORE), and four diagonal glints only when gold.	0,1,2,3,5,7	2
ramp-5stop-equals-walk	equivalence	ramp_5stop; fire_ramp; water_ramp; FIRE_STOPS; WATER_STOPS	FIRE_STOPS and WATER_STOPS are strictly ascending from exactly 0.0 to 1.0, and for every f32 bit pattern t (including -0.0, infinities, NaN) fire_ramp(t) and water_ramp(t) equal the test-only stop walk (which becomes the spec's model); NaN maps to the last stop.	0,1,3,5,6,14	2
file-feed-bounded-regular-read	contract	read_bounded_regular_file; read_bounded_regular_utf8; fingerprint_bounded_regular_utf8; fingerprint_admitted_utf8; open_regular; spec::read_toy_pack_file	read_bounded_regular_file(p,m) is Ok(bytes) iff p's final component is not a symlink, it opens non-blocking as a regular file (FIFO or directory gives InvalidInput) and its length <= m (else InvalidData naming m); the UTF-8 variant also requires valid UTF-8 (else InvalidData); the fingerprint variant succeeds under the same conditions and fingerprints the admitted text from the same handle.	0,1,8	4
gray-field-decode	contract	gray_decode; gray_encode; field	gray_decode and gray_encode are mutually inverse on u64, so field(g,lo,n) is a bijection onto 0..2^n depending only on bits lo..lo+n; flipping bit lo moves it by exactly 1, bit lo+k within a reflection block of size 2^(k+1); bits outside leave it unchanged.	0,3,5,7	1
simhash-bitsliced-equals-reference	equivalence	simhash_majority_bitsliced; simhash_majority_reference; simhash_ctx_bitsliced; simhash_ctx_reference; csa	For every voter set the bitsliced majority equals the reference (bit i set iff the weighted signed vote sum >= 0; no voters give u64::MAX, one voter its hash), and simhash_ctx_bitsliced equals simhash_ctx_reference on every text, span and form hash.	0,1,2,3,5,7	2
simhash-filter-silence	contract	simhash_ctx; vote_token	Replacing a context token that fails the alphabetic/length filter with another failing token at the same position leaves simhash_ctx unchanged.	0,1,2,3,5,7	1
v4-head-ticket-fairness	contract	cat_variant_v4; cat_variant_index_v4; scale_head_spill	With H = |HEADS| and t = field(g,0,6): index = t mod H if t < 64 - 64 mod H, else scale_head_spill(field(g,34,30),H) = (spill*H) >> 30; so it is < H and depends only on bits 0..6 and 34..64.	0,3,5	2
genome-feature-ranges	contract	nova_features; nova_duration_ms; nova_palette; nova_radius; cat_fills_v4; cat_age_v4; cat_variant_v4	nova_features(g) lies in its documented ranges (palette <= 7, radius in [1.6,2.2], ring in [1.5,3.5], duration 1000..=1400, debris 8..=20, chroma in [1,2.5], rot <= 15, rays 5..=8) with every value reachable; the single-field accessors equal the full decode bit for bit; cat_fills_v4 is in 0..16 x 0..8 and the age and variant decoders reach every band and head.	0,1,3,5	2
mix-nonzero	contract	mix	mix(s) != 0 for all s, and mix is injective except on at most one pair (the 0 -> 0xD1B5_4A32_D192_ED03 remap of the bijective splitmix finalizer).	0,3,5	1
motion-caret-contract	contract	host::{motion_caret,motion_print_anchor,hidden_caret_withheld,draws_ink,cursor_row_empty}; Terminal::sync_frame_unfinished; TerminalFacts::frame_unfinished	motion_caret(caret,visible,withheld) = caret.filter(visible or not withheld). motion_print_anchor(term) = (None, None) when term.sync_frame_unfinished() and the cursor is hidden, else (term.print_anchor(), term.print_anchor_glyph()): an unfinished frame's print run behind a hidden cursor ends at its write position, so it hands the hidden-caret lane no anchor, and a drawn caret keeps its anchor as it keeps the caret. hidden_caret_withheld(term) = term.sync_frame_unfinished() or (cursor hidden and every cell of the cursor row fails draws_ink); draws_ink is true for a non-space, complex or styled cell, non-default bg, inverse/underline/strike, or both super- and subscript. sync_frame_unfinished means the program's ?2026 bracket is still open by its own account and written into, including after the timeout force-cleared the mode. So a hidden cursor on an unfinished sync frame or a blank row is no caret for the motion engines, and a scrolled viewport withdraws the caret.	0,1,2,3,4,7,12,13	9
terminal-facts-reader	refinement	host::TerminalFacts::read; aterm_core::Terminal	For every Terminal state reachable by bytes, read reports caret = Some(cursor) iff the viewport is live (a DECTCEM-hidden caret still travels), cursor_visible = DECTCEM, live_viewport, alt_screen, shell_executing = last OSC 133 phase C, cmd_done = (code, completed_command_seq) after OSC 133 D, the host's scrolled flag, and a content clock that advances on output.	0,1,2,3,7,12	2
fold-placement-bounded	contract	CursorCat::placement_frame	r = placement_frame(now) has sampled_at = now; with fold Some(F) and p = (now-F.started)/D, D = CURSOR_FOLD_DURATION, r.fold = Some{F.started, F.direction, clamp(p,0,1)} if p < 1 else None; r.facing_left follows the fold direction while it lasts and sigma.facing_left otherwise; sigma is unchanged.	0,1,2,3,6	2
motion-pulse-fold-gate	contract	CursorCat::{on_motion_pulse,on_pet_mode_motion_pulse,on_motion_pulse_with_owner}	Advance and FoldForward act exactly as on_key_with_owner(P.at, true, o) then facing_left := false; FoldReverse calls no key path and sets facing_left := true. For fold kinds fold := Some{P.at, direction} iff active after the key step and (o or sing > 0); a fold pulse on a Hidden cat that its key does not summon leaves fold None.	0,1,2,3,4	0
placement-reset	contract	CursorCat::{rebase_placement,static_frame}	rebase_placement sets colors, fold := None and facing_left := false and changes nothing else; static_frame leaves fold None and facing_left false.	0,1,2	1
earn-gate	invariant	CursorCat::{on_key,on_key_with_owner,summon,on_kill,on_enter,on_curse,on_delight,set_look}	Let dt = min(time since last, 0.5) and h = M before the key. A forward key at h >= CAT_BAND adds min(dt, SUSTAIN_CREDIT_MAX) to sustain and 1 to run_keys; below the band sustain leaks by 1.5 dt and run_keys by floor(1.5 dt/0.25); a non-forward key zeroes both. Hidden -> FadeIn(now) iff the owner holds and sustain >= CAT_DWELL and run_keys >= MIN_RUN_KEYS (summon resets sustain, run_keys, colors, fold, facing and applies pending_look). The only ways out of Hidden are this summon, sing-sync's summon and on_collect; kill, enter, curse, delight, set_look, rebase and FoldReverse never leave Hidden.	0,1,2,3,4,5,6	5
earn-latency-window	other	CursorCat::on_key; TypingMomentum::{advance,value}	For every constant cadence g in [25, 120] ms, a run from default first summons at key k with (k-1)g in [2.2, 3.6] s, held in band for at least (CAT_DWELL-0.1)/g keys; no cadence >= 0.7 s ever summons; 60 ms runs with pauses <= 150 ms summon; words of <= 7 keys at 120 ms separated by >= 650 ms never summon. Each is a for-all over the cadence parameter, derived from earn-gate and the momentum closed form.	0,1,2,3,4,5,6,7,10	5
lifecycle-grounding-and-fade	contract	CursorCat::{frame,grounded_exit,begin_fade_out_at,low_crossing_at}	With n = the sample time, s = M(n) and grounded = s < LOW or (collection_hello and s < REVIVE_GATE): (a) FadeIn or Shown, no discovery and grounded moves to FadeOut at the analytic crossing (or the hold deadline), exit Plain for a hello else roll_exit(), land_at := start, and returns that FadeOut frame. (b) FadeOut past FADE_OUT goes Hidden, clears the episode and returns hidden_frame(look). (c) Alpha is the floor of 255 times the FadeIn fraction, 255 while Shown, and 255(1-t) during FadeOut with fade_out = t. A frame sampled after a skipped interval returns alpha 0, never an opaque restart.	0,1,2,3,4,6	4
hidden-frame-zero-work	contract	CursorCat::frame; CatFrame::fp; hidden_frame; CursorCat::reset_spine	alpha = 0 implies fp() = 0. A Hidden cat's frame(now) = hidden_frame(look) with pose STILL and bob 0, and it leaves the spine, clocks and chains reset and the cat not active. Two consecutive hidden frames return equal frames and change nothing after the first.	0,1,2,3,7	1
revive-continuity	contract	CursorCat::on_key_with_owner	A forward key during FadeOut(t0) with m = M after the key and f = min(1, elapsed/FADE_OUT) revives to FadeIn(now - FADE_IN(1-f)) with exit Plain iff not collection_hello and (owner or sing > 0) and m >= REVIVE_GATE, so opacity is continuous; otherwise state is unchanged. A hello's goodbye is never revived; pending_look is not consumed by a revive.	0,1,2,3,6	1
render-look-table	table-property	CatFrame::render_look; KittyLook::normalized	With E = fade_out >= EXIT_FACE_FADE, render_look returns S121 for Startled or StarWink with E, S115 for HeartMeow with E or a live sing face, SpecManeki for Celebrate, else the frame's look; the first five are normalized with no accessory. So a flourish face never shows above alpha 191, the oops face outranks the singing face, and Discovery or Cruise with sing < 0.33 shows the collected look.	0,1,3,6	5
static-frame-agrees-on-existence	invariant	CursorCat::static_frame	static_frame has pose STILL, bob 0, fade_out 0, alpha in {0,255}, and alpha > 0 implies the cat is active; a song armed while Hidden yields alpha 0.	0,1,2,3,6	1
static-frame-contract	contract	CursorCat::{static_frame,reaction_at}	(1) A live collection hello draws alpha 255 with the collected look and reaction Startled or Wince if live, else Discovery. (2) An expired hello retires to Hidden and falls through. (3) A live sing face on a non-Hidden cat draws alpha 255, Cruise. (4) With sing 0 the state ladder runs on wall time (FadeIn/Shown ground below LOW, FadeIn -> Shown after FADE_IN, FadeOut -> Hidden after FADE_OUT applying pending_look), alpha 255 iff Shown. (5) Otherwise hidden_frame. A reduced-motion flight retires by wall time alone.	0,1,2,3,4,6	5
static-deadline-future-min	contract	CursorCat::static_deadline	None without an unpaused hello; otherwise the minimum of discovery_until and a live Startled/Wince reaction_until restricted to instants after now (None if empty); read-only.	0,1,2,3,7	0
hello-pause-shift	contract	CursorCat::{set_collection_presentable,collection_sample_time,frame}	Without a hello the call only clears paused_at. Hiding a hello records paused_at; presenting it again shifts discovery_until, reaction_until, wince clocks, flight and fade stamps by the hidden span and clears paused_at; while paused frame and static_frame sample at paused_at, so hidden time consumes none of DISCOVERY_HOLD.	0,1,2,3	1
on-collect-contract	contract	CursorCat::{on_collect,begin_fade_out_at}	on_collect(now,L) sets look = L.normalized(), pending_look None, M = 1, last = now, sustain and run_keys 0, exit Plain, colors None, discovery and reaction until now + DISCOVERY_HOLD with reaction Discovery, collection_hello true, and FadeIn(now) from Hidden or FadeOut; it bypasses earning, its goodbye exits Plain showing L, and it is the only call that changes look while active.	0,1,2,3,4	3
look-latch-per-appearance	invariant	CursorCat::{set_look,summon,retire_unowned_cursor_motion,static_frame,frame,on_key_with_owner}	set_look parks L.normalized() in pending_look while active or in a hello, else applies it. Between a summon and the next Hidden the frame look is constant unless on_collect is called (a revive keeps it); pending_look is consumed only at summon, retire_unowned and static_frame's retirement, and on_collect clears it.	0,1,2,3,4,5	1
reactions-gate-and-preserve-identity	contract	CursorCat::{on_curse,on_delight,on_enter,note_oops,on_kill}	on_curse and on_delight return false and change nothing iff hits = 0 or the cat is not active; on_enter and note_oops are no-ops when inactive. Active: curse sets Wince with hold and chain min(4, chained ? chain+hits : hits); delight the same shape with Delight and no land_at; enter sets Celebrate. None changes state, look, pending_look, flight or discovery; reaction_at decays any reaction to Cruise once its hold passes.	0,1,2,3,13	0
oops-burst	contract	CursorCat::{note_oops,reaction_at}	With same = Startled and last oops within OOPS_REARM, note_oops sets Startled until now + OOPS_HOLD and oops_last = now, and kicks (oops_at = land_at = now) iff not same; a delete run with gaps < OOPS_REARM kicks once and holds until its last delete + OOPS_HOLD.	0,1,2,3,5	1
kill-contract	contract	CursorCat::on_kill; TypingMomentum::kill	on_kill zeroes sustain and run_keys, stamps last, drains M by 0.24 (floored at 0) and applies note_oops; it never summons; enough kills ground the flight and a live Startled persists into the goodbye frame.	0,1,2,3,4,6	0
pose-closed-form	equivalence	CursorCat::{resolve_pose,advance_spine}; smoothstep	resolve_pose composes, in order: the spine follower (exp ease toward the clamped target), the bank b = smoothstep(BANK_LO, BANK_HI, disp) giving stretch, thin and lead plus the meteor whip, the stride footfall (at the drawn stride phase, which follows the pump at STRIDE_TAU, as the hover bob does), the landing ring LAND_AMP e^(-LAND_DECAY u) cos(2 pi LAND_FREQ u), the Startled, Wince and Delight envelopes, and the sing squash and sway. Derived: an impact frame has scale_y < 0.95; scale_x and lead are monotone in disp; at rest everything is 1, 1, 0; a live Startled with env >= 0.5 leans by <= -OOPS_RECOIL/2.	0,1,2,3,4,6,12	2
eyes-selection	table-property	CursorCat::{resolve_pose,blink_active}; companion::sing_face_live	Eyes are Blink for Wince, Happy for Delight, Open for any non-plain reaction or exit, Happy during the meteor squint, Blink in the blink window without a sing face, Happy for a sing face or disp >= HAPPY_GATE, else Open. So the singing head never blinks at drive >= 0.33, eyes are a pure function of their inputs, and a plain cat in flight blinks exactly when (t - flight) mod 1.7 is in [1.57, 1.7).	0,1,2,3,6,12	3
stride-and-bob	contract	CursorCat::{on_key_with_owner,stride_advance,bob,frame}	stride advances by STRIDE_IDLE_HZ min(dt, 0.1) mod 1024, plus STRIDE_PER_KEY per forward key; bob = sin(2 pi stride_shown) BOB_AMP in flight else 0, where stride_shown follows stride at STRIDE_TAU, never faster than STRIDE_RATE_MAX and never a whole cycle behind, advanced to the frame before the bob reads it; blended toward the sing beat by sing; bob ignores reactions; more keys in the same time give more stride.	0,1,2,3,5,6	2
sing-sync	contract	CursorCat::set_singing	sing := clamp(drive) (0 if not finite), beat likewise; below 1 nothing else changes; at 1 M := 1, last := now, and Hidden summons iff run_keys >= MIN_RUN_KEYS; retire_unowned is a no-op while singing.	0,1,2,3,4	0
cat-retire-unowned	contract	CursorCat::retire_unowned_cursor_motion	A no-op during a hello, a song, or Hidden; otherwise state Hidden, momentum default, every episode field reset, pending_look applied and the spine reset.	0,1,2	0
episode-palette-freeze	contract	CursorCat::{colors_for_episode,episode_colors}	colors_for_episode(k) returns the frozen colours if set, else stores and returns k; colors is cleared only by summon, on_collect, a firing retire_unowned, rebase_placement and the transitions to Hidden, so one visible episode returns its first sample throughout.	0,1,2,5	1
exit-roll-table	table-property	CursorCat::{roll_exit,xorshift}	xorshift32 (13, 17, 5) never reaches 0 from the default seed; roll_exit is StarWink for x mod 5 = 0, HeartMeow for 1, Plain otherwise (the exact 20/20/60 residue split).	0,1,2,3	1
exit-fx-sinks	contract	kitty_cursor::{emit_exit_fx,push_fx,push_exit_veil}	Plain changes nothing. Dark: halos unchanged and out gains only additive quads clipped to the effects box, non-empty iff the coverage at pop = clamp(sin(pi fade_out),0,1) is positive and a rect survives. Light: out unchanged and halos gain only Over halos whose ink is the shared exit ink and whose centre alpha <= OverText's cap, non-empty only if alpha >= 24, so the light heart emits nothing near either end of the exit.	0,1,2,3,7,8,12	4
exit-ink-legibility	other	kitty_cursor::{exit_heart_light,exit_star_light}; InkRole::{ink,alpha_cap}	Closed terms: the heart and star OverText inks are non-black, the star warm (red > blue), both luma601 < 160, the Leading twinkle ink non-black, and the light veil at fade_out 0.5 has centre alpha >= 96.	0,3,6,12	0
starwink-flash-rate	other	kitty_cursor::emit_exit_fx; CursorCat::frame	The star's twinkle oscillates at EXIT_STAR_SCINT/(2 pi FADE_OUT) Hz, and the constants keep that <= TWINKLE_FLASH_BOUND_HZ (3.0) with EXIT_STAR_SCINT > 0. Held by the const assertion beside EXIT_STAR_SCINT (kitty_cursor.rs).	0,3,6	0
kr-launch-look	contract	kitty_registry::KittyLook::{for_launch,normalized}; genome::{mix,cat_fills_v4,cat_variant_v4,cat_age_v4}	for_launch(seed) = KittyLook{cat_variant_v4(g), no accessory, cat_fills_v4(g), cat_age_v4(g)}.normalized() with g = mix(seed ^ LAUNCH_LOOK_SALT): seed-pure, a fixed point of normalized(), a Head with no accessory, never the shared default. Table half: seeds 0, 1, 0x5EED map to the pinned tuples, and seeds 0..64 give at least 32 distinct looks.	0,1,3,5,12	1
dog-skin-table	table-property	PetSpecies::skin; DOG_SKIN; pet_glyphs_gen::{PET_GLYPH_IDS,PET_GLYPHS}	Cat.skin is the identity and Dog.skin is idempotent; every cat pose c has Dog.skin(c) != c named "pet_dog_" plus c's suffix, and Dog.skin is injective on cats (the fallback-to-self branch is dead for the shipped roster). Decidable by evaluating the const table.	0,1,3,7	2
station-wall	contract	PetBrain::{station,station_with}	With limit = cols - width: 0 if limit <= 0; caret + STATION_LEAD + extra when that fits under limit; else max(caret - width - STATION_LEAD - extra, 0). So 0 <= result <= max(limit,0) and at the right margin the stand is left of the caret.	0,3,5	1
quiet-pace-law	contract	quiet_speed_max; art_cols; QUIET_ONE_BOUND_SECS	q(c) = clamp(0.74c, HOP_SPEED_MAX, BIG_HOP_SPEED_MAX) is monotone, 55 for c <= 74 and 140 for c >= 190; q(c)/60 <= half an animal per frame; c/q(c) <= QUIET_ONE_BOUND_SECS exactly for c <= 238, so the quiet split needs >= 239 columns. Finite domain.	0,3,5	2
flight-lift-shape	contract	PetBrain::{flight_lift,emit}	A non-big flight lifts arc*4u(1-u) bitwise; a big flight lifts arc(1-(1-2u)^3) rising and arc(1-(2u-1)^HANG_P_DOWN) falling, continuous with h(0) = h(1) = 0 and h(0.5) = arc, hanging >= 0.9 arc for 0.4236 of its time versus 0.3162 for the parabola; a live non-big Leap frame's lift equals that formula.	0,1,2,3,5,14	2
flight-pose-schedule	contract	PetBrain::{pose_schedule,flight_pose}	pose_schedule(dur) splits dur into rise, hero >= MIN_HERO and descend, each monotone in dur. Below 3 MIN_HERO a flight is LeapRise then LeapDescend; a pounce shows exactly [LeapRise, Leap, LeapDescend]; a bound shows Apex with every pose run 2..24 frames at 60 fps; a coiled flight is PetLaunch at t <= 0 and never after.	0,1,3,5	3
coil-scaling	contract	PetBrain::{coil_dur,coil_envelope,tick}	coil_dur(s) = clamp(CROUCH_DUR + s COIL_PER_CELL, CROUCH_DUR, COIL_MAX); the coil envelope preserves volume, is continuous at release and reaches the launch stretch at dur with depth monotone in dur; a pounce of span s gathers for coil_dur(s) plus or minus a tick.	0,2,3,5,7	1
caret-delta-fold	contract	PetBrain::caret_delta; wrap_bar	With dr, dc and bar = wrap_bar(cols): a one-row move with opposite-signed dc >= bar folds to (0, dc + dr cols, true); a wrapped same-row retreat >= bar to (1, dc + cols, true); a same-row retreat in (-SNAP_COLS, -STARTLE_COLS] at home or after a scroll to (1, dc, true); otherwise (dr, dc, false). The arms partition the inputs and never fold a forward move.	0,1,3	3
ink-scroll-fact	contract	PetBrain::{sense_ink,retire_coordinate_space}	ink_scrolled holds iff the base is unchanged, the old map is non-empty, of equal length, inked, different, and the new map equals the old one shifted by one row; the map is stored afterwards; retire_coordinate_space empties it so the next map never reads as a scroll.	0,1,2,5,7	1
caret-at-home	contract	PetBrain::{caret_at_home,ink_span,ink_span_idx}	caret_at_home(row,col) iff col <= WRAP_SLOP, or the row's ink span starts within WRAP_SLOP and ends at or before col + 0.5.	0,1,2,3	1
evict-step	contract	PetBrain::{evict_toward,is_re_anchor}	evict_toward moves from toward to by at most INK_EVICT_SPEED max(dt,0) without overshoot; is_re_anchor iff |dcol| > INK_EVICT_MAX or |drow| >= 0.5.	0,3	1
ink-ladder	contract	PetBrain::{ink_stand,ink_safe_col,ink_overlaps,observed_ink_overlaps,station_safe,observe_console}	ink_stand picks the first of: want if its shaved footprint misses ink; a blank column within INK_EVICT_MAX; the blank row below or above (or a certified gap there); beside the row's ink (or want when walled). Unknown coverage stays obstructed and an unfed map is inert; every answer is inside [0, cols-w] x [0, rows).	0,1,2,3,7,12	2
seat-deal-arithmetic	table-property	PetBrain::{beat_deal,loaf_after,groom_skip,look_dur,sit_yawn,sit_tail_edges,tail_hand_len}	For all 256 seeds: beat_deal(b,n) = (b+seed) rem n; loaf_after = LOAF_AFTER + 0.8 (seed mod 3); look_dur is long iff seed mod 3 != 0, and then fits the seat and 7 tail swings, else the short look cannot; sit_yawn iff seed even and quiet in the last YAWN_DUR before loaf; groom_skip iff seed mod 4 = 3; sit_tail_edges deals flick, double or swing by beat_deal.	0,1,2,3	2
departure-lane	contract	PetBrain::{spawn_departure,resolve_departures,needs_frames}; Departure::{life,envelope,look_at,stopped_at}	resolve_departures is a pure function of (records, lane_clock); a running departure moves monotonically toward its edge facing it and resolves to None after life <= DEPART_MAX; at most PET_DEPARTURES_MAX records live with the oldest dropped; an expired lane contributes nothing to needs_frames.	0,1,2,3,7	2
note-full-screen	contract	PetBrain::note_full_screen; tick (watch, hug gates)	A rising note_full_screen(true) zeroes watch heat and time, clears watch_spent and vigil_armed; any other call only stores the level. While full screen the tick grows no stare and arms no hug; off, the same brain stares and hugs from either witness.	0,1,2,3	1
command-verdict-ledger	contract	PetBrain::{note_command_done,note_trick_submit,grieving}	A failure right after a pet-only submit (within TRICK_SUBMIT_FORGIVEN, faster than CHEER_MIN_MS) is forgiven and changes nothing; the submit stamp is one-shot. Otherwise a failure sulks (content - SULK_CONTENT, pending_sulk, sulk_at); a success of at least CHEER_MIN_MS cheers (+CHEER_CONTENT, big if forgiven or long); a fast success after a recent sulk is a big cheer; other fast successes nudge content. Every call ends the vigil; one cheer from below PURR_GATE - CHEER_CONTENT cannot reach the purr.	0,2,3,4	4
trick-latch-algebra	contract	PetBrain::{note_trick,revoke_trick,pending_trick,trick_engaged,tick}	One slot: the same trick upgrades confirmed one way and keeps its clock, a different trick replaces it (newest wins), and a confirmation inside the heard-you beat of an already answered tentative trick is a no-op (one phrase, one trick). revoke_trick drops an unconfirmed latch, or an unbegun tentative verb, and nothing else. A tentative note revoked before the beat leaves every frame identical to never noting it; a confirmation after input took the verb away re-latches and plays once.	0,1,2,5,12	6
peek-latch-ranges	contract	PetBrain::{note_peek,tick}	A non-finite peek latches nothing; within BAT_RANGE it latches a bat (greeting on odd mote_serial), within LOOK_RANGE a look, farther nothing. A non-finite pointer counts as no pointer and every frame stays finite.	0,2,3	3
v2-offer-latch	contract	PetBrain::{note_v2_offer,needs_frames,next_change_deadline,tick}	note_v2_offer changes neither needs_frames nor the deadline; it latches a new perk and a catch (when cooled) and returns the caught star at most once per latch, only on a PetBat Frolic frame that moves the body less than 0.05 col from an un-offered twin. A settled pet perks on the first tick at or after the offered instant; purr notes wear the offered mote colour.	0,1,2,4,5	4
room-facts-no-wake	contract	PetBrain::{note_room_lease,note_room_quiet,note_room_turn,note_room_sibling,note_room_inbox,tick}	Room notes never add a wake; inbox and turn seed on first call; mail, overdue and a talking sibling each latch one edge. Behaviour: one look per sibling burst facing that pane, one perk, ear twitch and extra note per mail, a hold keeps the seat but still sleeps on time, a lease stations the cat at the far stand reached and left by walking and converging to the un-leased twin after release.	0,1,2,3,4,5,7	4
flow-latch	contract	PetBrain::{note_flow,tick}; FLOW_LATCH_TTL	note_flow adds no wake; an awake settled cat plays one Frolic with no motes per note; a sleeper is not woken and the latch is dropped; an unconsumed latch expires at FLOW_LATCH_TTL.	0,1,2,3	1
time-translation-determinism	equivalence	PetBrain::tick; PetFrame::fp	For every script and offset d the frames of the script and of the script shifted by d are identical: tick reads time only through differences and stored instants.	0,1,2,5,7,13	3
empty-ink-identity	equivalence	PetBrain::{sense_ink,tick}	Feeding an empty ink map first gives fp-identical frames to never feeding one.	0,1,2,7	1
viewport-clamp	invariant	PetBrain::{emit,tick}	Every frame has 0 <= col <= max(cols - w, 0) and 0 <= row <= rows - 1, and an airborne bound's sprite top stays at or below the viewport top (the apex gives up height).	0,2,3,7	2
no-teleport-law	invariant	PetBrain::{tick,emit,begin_arc,begin_big_arc,launch_big,leave_ground,retime,content_ride,carry_with_regrid}; re-aim	After the seeding frame every step moves col by at most max(MAX_SPEED, the flight rates before and after) times dt plus a resize give, and rows change only in flight or when content_ride carries a resting cat down with the shifted rows round it (carry_with_regrid); every launch has span/dur <= HOP_SPEED_MAX (pounce) or quiet_speed_max(cols) (bound); a re-aim keeps the remaining rate within the cap, draws out a row change via retime up to ROW_REAIM_MAX_U (column re-aims up to REAIM_MAX_U) without regressing Flight::stage, rebases so the drawn body does not move, never turns a flight back in the air (taking only the caret's row and keeping its column when a row change puts the station behind, where that stand fits), and refuses any landing that does not fit; every caret-driven launch spends the jump latch. So an hour between ticks moves the body under 5 cols and a one-frame hide never moves a visible body a cell.	0,1,2,3,4,5,7,11	8
flight-wall-time	contract	PetBrain::{tick,flight_shape,big_shape}	A flight's clock advances by each tick's clamped dt, so it lands by the first tick at or after t0 + dur with at most one tick of overshoot; pounces last at most FLIGHT_MAX and bounds BIG_FLIGHT_MAX plus a tick; a wrap from a long line ends on the new row.	0,2,3,7	3
first-sighting	contract	PetBrain::{tick,tick_static_capture,needs_frames}	An off-glass pet seen at caret (r,c) is placed exactly at (station(c), r); a fade-in's seeding tick has alpha 0 and owes frames; tick_static_capture of a fresh brain returns alpha 255 at the station; a caret that hides until dark and returns elsewhere is a new sighting, never a Startle.	0,1,2,3	4
fade-and-appearance	contract	PetBrain::{tick,sync_look,colors_for_appearance,appearance_colors,invalidate_colors,is_active}; PetFrame::fp	With the caret hidden alpha reaches 0 within FADE_OUT plus a tick, then fp = 0 and not active. One appearance wears one look and palette: a visible re-sync parks and lands at alpha 0, a hidden one applies at once, an agreeing one is a no-op; colors_for_appearance returns the first sample until invalidated or the pet goes dark.	0,1,2,3,10	4
coordinate-retirement	contract	PetBrain::retire_coordinate_space	Idempotent; keeps identity (species, worn with the parked pair landed, content, quiet, clock, motes counters, cooldowns) and resets the surface (alpha 0, Sit, caret, flight, ink map, pointer, pending stimuli, trick latch, seat memory); afterwards not active, owes no frames, and the next sighting is a first sighting.	0,1,2	3
pet-retire-unowned	contract	PetBrain::{retire_unowned,tick}	After retire_unowned the brain is inactive, owes no frames and no deadline, and every later caretless tick emits alpha 0 with empty mote and departure lanes; repeating it changes nothing.	0,1,2	1
species-skin-on-glass	invariant	PetBrain::{set_species,emit}	set_species changes only species; every frame's pose is species.skin of the chosen cat pose; a switch keeps the action, moves col less than 1 and shows a dog pose next frame.	0,1,2	2
hidden-arc-deferral	contract	PetBrain::tick (hidden arm); deferred_hidden_landing; retired_flight_lift	A caret withheld mid-flight retires the flight and holds the visible feet while alpha > 0, lands the body at the target on the frame alpha reaches 0, and a caret returning earlier drops the deferral and resumes from the held feet; the retired lift is gone within FADE_IN and owes frames until then.	0,1,2,3,7	2
reduced-motion-static	invariant	PetBrain::{tick,needs_frames,sync_look}	Every reduced-motion tick has lift 0, unit scales, Sit or Sleep at the caret station, alpha 255, no motes, no departures, and owes no frames afterwards; every latch is bookkeeping only (content still moves), pointer and watch heat stay 0, a parked look applies at once, a commanded Sleep is the one allowed pose swap, nothing replays when reduced motion lifts, and there is at most one distinct fp per caret cell.	0,1,2,3	12
pet-idle-to-zero	other	PetBrain::{tick,needs_frames,next_change_deadline}; PetFrame::fp; every latch TTL	After the last input, with a constant caret, every brain reaches Sleep with no motes, owes no frames and no deadline, and stays fp-stable, by SLEEP_AFTER + BREATH_WINDOW plus the longest latch envelope; every latch is finite and none pins needs_frames; Sleep lands at SLEEP_AFTER plus or minus a tick for every seed.	0,1,2,3,5,7	11
offer-soundness	refinement	PetBrain::{next_change_deadline,next_change_secs,mote_edges,needs_frames}; first_px_step; PetFrame::body_px	A host that ticks every 16 ms while frames are owed and otherwise at the offered deadline (always > now) shows exactly what a dense 1 ms run shows: no offer is late, every wake matches a dense instant within 1 ms and a pixel, the offer is the renderer's own pixel step at any cell size, offer-riding motes never hold the frame train, commanded seats and purring seats name their ends on time, and past SLEEP_AFTER + BREATH_WINDOW the deadline is None.	0,1,2,3,5,7,10	9
needs-frames-motion	contract	PetBrain::{needs_frames,tick}	needs_frames holds whenever anything moves (flight, landing, speed, a glide of at least a pixel even on a zero-dt re-tick, pet hold, latched pet or trick, deferral, fade with a caret, ZeePop or Dust); the ZeePop never decides it alone, and landing Dust holds the train for a bounded beat.	0,1,2,3,7	4
gait-by-speed-and-distance	contract	PetBrain::{tick,gait_index,plant_stride}; CYCLE_WALK/LWALK/RUN	Walk vs Run vs the lateral walk is chosen by the gait pace (the chase speed averaged over GAIT_TAU, reset by a turn, let down at GAIT_REST_TAU at rest, dropped by a flight) with hysteresis kept across a stop; a gait is worn GAIT_DWELL before another, never dropping to a slower one while the speed rises, except past its frame-rate ceiling (WALK_CEIL, AMBLE_CEIL), which takes the faster one at once; a leg after a rest of GAIT_DWELL or a flight picks afresh; the stride advances by distance over the gait's stride cells, so a gallop cycle covers at least 3 cells, every beat lasts at least 2 ticks, walk frames step one beat at a time across a gait flip, and runs carry no lift.	0,1,2,3,7	6
keep-ahead-lead	contract	PetBrain::{lead,rhythm_open,pace,station_now,station_drift,on_move}	Each move adds 1/VEL_TAU to the decaying velocity estimate, and the pause does not decay the move that ends it; lead() > 0 only during a same-direction rhythm of RHYTHM_MOVES within RHYTHM_WINDOW; fast runs lead past the station, slow or alternating typing never leads, a pause eases back, a pounce mid-rhythm lands ahead, and a forced crossing aims at most station + LEAD_MAX and never runs.	0,1,2,3,7	8
chase-convergence-and-facing	contract	PetBrain::tick (follower, facing, drift-brake)	Under a parked caret the follower converges to station_safe without ringing; once the caret rests the step never crosses the station faster than RUN_SPEED and the drift-brake never arms from a chase; facing follows travel with FLIP_SPEED hysteresis and the first moving frame faces the gap; a sleeper turns only through travel; a held repeating jump still moves the pet; a skid once drawn stays on glass STILL_SECS from its brake before the arrival takes the cat.	0,1,2,3,5,7	8
wall-side	contract	PetBrain::{update_wall,station_now,tick}; pending_wall_transit	wall_side flips to the caret's left when the led station overhangs and back only past WALL_RETURN_MARGIN; a flip with nothing in the air is crossed by one small arc; a flight that already crossed consumes the transit; at the wall the Startle faces the caret; a far caret behind earns the shoulder peek.	0,1,2,3	5
flight-choreography	contract	PetBrain::{tick,flight_pose,coil_envelope,emit,spawn_dust}	Every caret flight is Crouch, Leap, Land; PetLaunch is the coil's last frame and never airborne; pounces and hops show rise, hero and descend, a short Enter is the tucked hop; the body leaves the floor and lands on the baseline; horizontal flight stretches width; volume changes at most 2% per tick (3% at the cut) and touchdown at most 0.12 in scale_y; the landing squash grows with span and the recovery lands on a contact frame; the screen-crossing show plays its whole sequence with dust; a flight asked for from a stand that ends a stride leaves no sooner than STAND_MIN into the stand (deferred_arc).	0,1,2,3,5,7	14
show-door	contract	PetBrain::{on_move,leave_ground,predicted_tier,launch_big,begin_big_arc}; quiet_leap; pending_big_jump; bound2	The show plays iff a chosen move opened it (a large same-row jump, a standing gap of BIG_GAP_FRAC during a hold, or a joyful word jump), never for a line change, wrap, snap home or post-burst trip; a snap home is one quiet bound; tiers by span (hop/pounce up to POUNCE_SPAN_MAX, bound beyond, two bounds past TWO_BOUND_FRAC unless the touch point is inked); every bound is paced at quiet_speed_max but the one that crosses after the writing's wrap (or back across it under a held delete): that one is paced at BIG_HOP_SPEED_MAX on the big bound's own arc for its span (PetBrain::brisk_shape) and goes with no notice beat; no bound leaves ahead of a wrap that has not come.	0,1,2,3,7,10	11
new-line-law	contract	PetBrain::{tick,on_move,caret_delta,notices,at_rest}	Every new-line shape (Enters onto 1- and 3-row prompts at any column and width, from any state or behind a one-frame hide, margin and scrolled wraps, printed lines, wide or suggested prompts through the ink seam) reacts with one or two lawful flights, no Startle, at most one notice beat (only from rest), no gather, dust or run, paws down within the shape's bound, rows at most 8 per second, ending on the prompt row without leaving the glass.	0,1,2,3,5,7	14
retreat-reactions	contract	PetBrain::{on_move,age_gesture_windows}	A retreat under STARTLE_COLS flinches (Perk with a duck), a bigger one or a run of them Startles facing the caret, once a run of deletes for STARTLE_HOLD (a held Backspace neither extends it nor fires it again, and the pet follows the caret back; a forward move or a line change ends the run's bottle), FROLIC_REVERSALS reversals Frolic, and a Startle consumed by an ink hop is never replayed.	0,1,2,3,7	5
settle-ladder	contract	PetBrain::{tick,enter_settled}; loaf_after; groom_after; groom_skip; wake arm	After arrival: Stand until SIT_AFTER, then Sit or Purr, loaf at loaf_after(seed), groom (second wash skipped iff groom_skip), Sleep at SLEEP_AFTER plus a tick at any cadence down to 1 Hz with a breathing swell; the next key wakes through front then hind stretch inside WAKE_DUR with one ZeePop; each arrival deals a fresh seed; even seeds yawn into the loaf; the loaf peeks before sleep.	0,1,2,3,5,7	14
idle-frame-laws	invariant	PetBrain::tick (settled arm); sit_tail; sit_blink_beat; sit_glance_left; side_on_holds; tail_room	For every seed, settled idle frames have lift 0, a constant row, per-tick col step <= 0.01 and total drift <= PECK_CLEAR; every idle pose run lasts at least 2 ticks; the tail changes side only between its tip-up frames; no side-on beat is cut by the look; the glance never flips facing.	0,1,2,3,5,7	6
seat-beat-reachability	table-property	PetBrain::tick (seat beats); beat_deal; sit_tail; sit_blink_beat; sit_glance_left; twitch_up	Over all 256 seeds the seat deals silence, single and double flicks; blinks are dealt single or double; face-on seats either never glance or glance and return (or yawn into the loaf); every side-on beat is reachable; the head tips toward a caret off the seat's row; a working cat still gets its whole sit window; one long dwell in three loafs; an output pulse twitches the ear per pose and mote_serial parity.	0,1,2,3,5,7	12
contentment-purr	contract	PetBrain::{purring,emit}; content ledger	content rises by CONTENT_PER_CELL per cell travelled, capped, and decays (faster while moving); purring() iff Purr, or Sit/Loaf with content >= PURR_GATE; a purring frame swells by at least PURR_DEPTH; the long dwell wears Purr on seeds with seed mod 3 != 0 and Loaf otherwise; a purring cat floats notes and hearts, never grid-aligned.	0,1,2,3,7	3
mote-laws	invariant	PetBrain::{spawn_mote,tend_motes,resolve_motes,zee_ink_fade}; Mote::{drift,look_steps}	At most ZEE_ALIVE_MAX Zees, born under 3 Hz; every drawn mote is off-grid and rotated; a Zee wears exactly its ZEE_STEPS looks; a visible Zee or ZeePop never enters an inked band and frees its slot at alpha 0.	0,1,2,3,7	3
bell-stimulus	contract	PetBrain::{note_bell,consume_stimuli}	A bell latches for BELL_LATCH_TTL and is consumed on the ground: a sleeper wakes the ordinary way, a settled pet flinches facing the caret and never Startles, a bell during a long flight expires unconsumed, and a bell ends a commanded sit.	0,1,2,3	4
petting-affection	contract	PetBrain::{note_petted,stroke_point,tick}; pet_stroke::StrokeDetector	Pets queue up to PET_LATCH_MAX within PET_LATCH_TTL and are consumed only on the ground; one pet gives a PET_HOLD Purr hold with one Heart and +PET_CONTENT without moving the body; repeats add a flick without extending holds; a gentle stroke counts as a click, a parked pointer earns nothing, a stroke crossed by a hidden, reduced, caret-move or burst frame is dropped; strokes wake a sleeper except under reduced motion; four pets reach PURR_GATE, one cannot.	0,1,2,3,7	8
verdict-choreography	contract	PetBrain::{consume_stimuli,grieving,droop_hold}; vigil pounce	A cheer plays a Perk then a Frolic with one note (from a loaf it stands and floats note and heart) and outlives holds and the walk home; a fast success plays nothing; a sulk droops in the right pose per seat and species with no motes, grieving() holds until the droop ends and suppresses notes and hearts; a droop on inked ground hops and replays in full; a vigil's green verdict pounces into Frolic and a red one droops.	0,1,2,3,7	10
stream-watch	contract	PetBrain::consume_watch; watch_heat; watch_t; watch_spent; stream; hug; stream_jump	Watch heat charges only on output bursts, so echo never perks; past WATCH_GATE a settled cat perks toward the output, the stare caps at WATCH_MAX and re-arms after decay; a caret jump pulls the watcher; the stream arms on STREAM_RUN line events and lapses after STREAM_LAPSE, seating the cat still above the log without towing, then one trip home; the live-edge hug follows a log identically with or without bursts, lagging at most 4 rows.	0,1,2,3,7,10	10
vigil	contract	PetBrain::{note_executing,note_room_quiet,waiting_up}; vigil_t; vigil_armed	While the shell executes and the stare has capped, a settled cat past VIGIL_AFTER crouches at the door as a held pose (no frames, no deadline); a quiet room withholds sleep; a commanded Sit is held then the door takes the cat back; a commanded Sleep leaves the door for good.	0,1,2,3,7	3
bat-and-look	contract	PetBrain::{note_peek,consume_bat,tick}	A near peek is batted (Crouch, Leap, PetBat Frolic for BAT_HOLD, one Dust) and the cat walks home; caret work is served first and the bat fires after within BAT_TTL; a stale latch never fires; a look-range peek is a Perk in place.	0,1,2,3	4
pointer-play	contract	PetBrain::consume_pointer_pounce; pointer_heat; pursuit; stakeout; tennis; boredom; groom_owed	pointer_heat rises with a live pointer and reaches 0 for a parked one (None clears it); the gaze follows and returns; a dash is pounced once only when content, then the cat walks home; caret moves clear play; a tease earns a chase on paws ending in an owed groom; any key ends the chase for PURSUIT_QUIET; a creeping toy pins the stakeout; a long rally sits the cat to watch; a bored content cat demands attention and may hide behind a word.	0,1,2,3,7	12
sleeper-gaze-brakes	contract	PetBrain::settle_gaze; SLEEP_FLIP_BAND; SLEEP_FLIP_DWELL; BREATH_WINDOW	A pointer never wakes a sleeper; swinging or resting it within SLEEP_FLIP_BAND never flips it; past the band a light sleeper turns after SLEEP_FLIP_DWELL and a deep sleeper never does.	0,1,2,3	3
sleeper-drop	contract	PetBrain::tick (drop arm); pending_drop; Flight::drop; DROP_DUR; DROP_SIT	An Enter one row under a sleeper over a blank column drops it straight down in its sleep pose within 13 ticks, sits it planted for DROP_SIT, then one coil and one quiet flight home with no second notice; a caret going up, a three-row prompt or ink under the column gives the ordinary wake.	0,1,2,3,7	6
scrabble-and-commit-stare	contract	scrabble; commit_t; Flight::late_move	A scrabble happens only when the landing misses its station by SCRABBLE_MISS and the one-in-four ration is on, never from ordinary typing; a flight moved twice lands, stares for COMMIT_STARE, then perks and leaps home; a third jump cuts the stare short.	0,1,2,3,7	4
ink-eviction-behavior	contract	PetBrain::{tick,station_safe}; resume; INK_EVICT_*	Ink under a grounded pose evicts it to ink_stand: a short step glides, anything further or any row change hops at once, ends on blank cells and hands the borrowed pose back; a deep sleeper's hop owes frames only while airborne; an owed verb riding a Perk is dropped; a growing line never tows the cat; a walled row sends it to the blank row below.	0,1,2,3,7	6
breed-handoff	contract	PetBrain::{sync_look,spawn_departure}; SyncLookOutcome; HANDOFF_DEBOUNCE; AdoptedLook::eased; lane_alpha	A visible repoint parks and lands after HANDOFF_DEBOUNCE on the ground, caret jumps first; the swap frame shows the live body at the arrival floor and one full-opacity ghost in the outgoing look; the body never moves and its alpha only rises; Ceremony ghosts run to their door within DEPART_MAX, Quiet ghosts fade in place over EDGE_FADE; agreeing re-syncs keep the ruling, a round trip dissolves the park, a hide fades both on lane_alpha, a clamped tick moves a ghost at most DEPART_SPEED*0.10, a restarted ramp never rewinds a stance, and reduced motion applies at once.	0,1,2,3,7,10	12
trick-performance	contract	PetBrain::{consume_trick,perform_trick,praise}; trick_settle; hold_end_q; TRICK_SETTLE/TENTATIVE/TTL/CAP	Every Trick in Trick::ALL is heard exactly TRICK_SETTLE (or TENTATIVE) after the last key, then performs its observable (each trick's pose and hold) and releases the lane; latches wait out holds and flights, play at the new prompt after an Enter, where the cat stands, or after the walk home; a sleeper is woken first except for Sleep and Stretch; praise moves content by at most TRICK_CONTENT per cooldown, which survives a surface switch.	0,1,2,3,7,12	9
trick-lifetime	contract	PetBrain::{sweep_trick_latch,drop_tricks,clear_play_intents,note_console_input}	A latch expires TRICK_TTL after the user's last key (only console input re-stamps it, up to TRICK_CAP); a missing caret, a hold or reduced motion drops it; a trick noted at an off-glass cat summons nothing; a commanded seat stands down on the next key, bell or caret move; an Enter's jump takes a roll away for good.	0,1,2,3,7	3
console-attention	contract	PetBrain::{note_console_input,begin_console_tick,tick_console_resident,console_attention,console_deadline,console_station}; PERCH_DWELL	Attention is a function of the observed world and input notes: Contact only when text input coincides with new glyphs in the body rect, then a recover pose and release; Delete holds Editing; a caretless selection is a still, wake-free Reading; OSC 9;4 progress baselines per command; a suppressed fast failure hushes celebration briefly; a spent perch is released after PERCH_DWELL; no admissible perch returns None; a late expired pose still offers one tick.	0,1,2,3,7,12	8
console-visibility-hold	contract	PetBrain::{console_observation_incoherent,emit}; VETO_BLIND; VETO_FADE	An incoherent observation licenses nothing (no move, no first appearance); a body on glass is held at its last certified place and opacity until VETO_BLIND of blindness, then fades in place over VETO_FADE.	0,1,2,3,12	1
hidden-cursor-home-refinement	refinement	ty_model! HiddenCursorHomeContinuity; PetBrain::begin_console_tick; ContentScrollState::delta_since	The model proves its invariants and catches Buggy = 1; every cursor-hiding motion with a known translation keeps cursor_home and manufactures no input (KnownMove), and every invalidating motion clears it (UnknownMove).	0,1,2,10,12	1
console-ghost-rules	contract	PetBrain::{finish_console_frame,displace_resight,resight_coordinate_space,last_drawn}	A ghost follows the body's visibility rule: ordinary ink is z-order, not a cull; a selection under a running ghost culls it, under a still one moves it with the body; a protected line lets it fade out; an own-glass ghost survives a displacement only if its pane certified nothing protected; carried ghosts keep their drawn presence; a look swap under a hidden-caret resident keeps both halves of the crossfade; the arrival ramp advances while the resident owns the tick.	0,1,2,3,7,12	6
console-trick-interplay	contract	PetBrain::{begin_console_tick,tick_console_resident}; INPUT_HOLD	With an observed world, the resident never eats an owed trick through a repair; Treat is performed directly; a commanded Sleep or Roll survives an executing block; a held selection drops a trick that never replays; streamed output never re-stamps a latch; a fresh key stands a seat down but keeps an owed latch; a forgiven pet-only submit shows no failed result.	0,1,2,3,7,12	9
full-screen-repaint-refinement	refinement	ty_model! FullScreenRepaint; companion::pet_output_burst; CompanionOwner::{prepare,tick}; PetBrain::note_full_screen	The model proves its invariants with each mutant caught by its own invariant; on every presented frame of a replayed session the projection satisfies them: on the alt screen no bursts, stares, hugs or vigil, the cat stays near the composer, a torn redraw arms no hug, an armed vigil stands down; with the alt fact withheld every invariant fails (negative control); inline on the main screen the stream is still watched.	0,1,2,7,10,12,13	6
kr-look-normalized	contract	kitty_registry::KittyLook::normalized	n = l.normalized() clamps coat to 15 and iris to 7, keeps age; an Accessory base becomes the default head with no accessory, a Special keeps its variant with no accessory, a Head keeps an Accessory-kind accessory only; so n's variant is never an accessory and normalized is idempotent.	0,1,3	1
kr-app-look	contract	KittyLook::for_app; kitty_registry::flagship_look	flagship_look(id) is Some for exactly claude, codex and agy with fixed distinct tuples; for_app(id) is the flagship look or the normalized v4 decode of mix(form_hash(id) ^ APP_LOOK_SALT), never with an accessory, always normalized, a Head on the hash path.	0,1,3,12	3
kr-app-basename	contract	kitty_registry::app_basename	app_basename(cmd) is the first whitespace token that is not a flag and has no '=', reduced to its lowercased last path segment minus ".exe", that is non-empty and not sudo, env or npx; None if no token qualifies.	0,1,7	1
kr-canonical-app-id	contract	kitty_registry::canonical_app_id	SHELL_APP_ID for zsh, bash, fish, sh, pwsh or nu; otherwise the basename itself.	0,1	1
kr-coat-distance	table-property	kitty_registry::coat_distance; COAT_DISTANCE; cat_baker::{COAT_RAMP,dark_bg_coat_stop}	coat_distance is symmetric with a zero diagonal and clamps inputs to 15; each table entry is within 0.01 of the min of the raw and lifted stop distances; the four darkest coats are pairwise insufficient and the flagship coats pairwise at least 4 SUFFICIENT_DIFFERENCE apart.	0,1,3,5	4
kr-registry-keys	table-property	KittyType::{ALL,config_key}; KittyMagic::{ALL,config_key,from_cat}	KittyType::ALL lists its 8 variants in order with injective pinned keys, KittyMagic::ALL its 5 with keys none/fortune/nebula/butterfly/sakura.	0,1	1
ks-song-signature-bijective	contract	kitty_sing::song_signature	song_signature is the lowbias32 mixer on the char's scalar, a bijection on u32 with an explicit inverse, so distinct chars give distinct signatures.	0,3,5	2
ks-bar-fan	contract	kitty_sing::bar_fan	For bar < u64::MAX, bar_fan(bar) = {bar, n: min(5 + 2 bar, 18), ring: (bar+1) mod 4 == 0}.	0,3	0
ks-arm-threshold	contract	KittySing::{note_char,note_backspace,note_break,rekey,release,is_armed,drive,beat}	A note_char arms (anchors armed_at) iff the count of consecutive same-char presses in one session, strictly increasing in time with gaps <= SING_REPEAT_GAP (equal stamps count once, any other key, break, Backspace or session switch resets), reaches the threshold (KEY_SWITCH_REPS during a live wind-down, else SING_ARM_REPEATS). So fewer than 16 never arm from rest, alternation and slow strikes never arm, an armed hold never moves its anchor, and at the arming press drive = 1, beat = 0.	0,1,2,3,4,5	10
ks-key-switch-handover	contract	KittySing::{note_char,signature,bar,drive}	While armed, a different key starts a provisional hand-over (drive stays 1, signature becomes the new key's) that commits after KEY_SWITCH_REPS repeats; a second distinct key before the commit releases into a wind-down. signature is the external run's, else the run char's, else NEUTRAL_SIGNATURE. A chain of held keys keeps drive 1 on one bar grid.	0,1,2,3,4,5	6
ks-drive-envelope	contract	KittySing::{drive,wind_start,is_armed,beat,settle,lazy_release}	drive is 0 unarmed, 1 while held, and SMOOTHSTEP_DOWN of the elapsed wind-down (from the eager stamp, the external run's end, or last + SING_REPEAT_GAP) otherwise, 0 after SING_WIND_DOWN: in [0,1] and non-increasing; settle at 0 clears the arm.	0,1,3,5	2
ks-external-arm-admission	contract	KittySing::{arm_external,cooldown_remaining,armed_external}; ArmRefusal::fmt	arm_external refuses bars outside 1..=4, a pending arm, or a cooldown under 30 s (reporting the remaining ms) and then changes nothing; otherwise it stores the arm and stamps the cooldown.	0,1,2,3	1
ks-external-fire	contract	KittySing::{note_green_block,note_keyed_enter,note_program_done,fire_external,bar,is_armed}	A green block, a keyed Enter or a program's `done` fires iff a pending arm of that trigger (Green, Enter, Done) is in that session: it clears the arm and starts an external run with its signature for bars x SING_BAR_SECONDS (note_program_done answers whether it fired); with no pending arm of that trigger every edge is a no-op.	0,1,2,3	3
ks-external-run-events	contract	KittySing::{take_bar_fan,take_outro,bar}	take_bar_fan yields bar_fan(k) once on the first frame at bar k (missed bars are not replayed); take_outro yields once at the run's end; a human hold never yields an outro.	0,1,2,4	1
ks-external-deadline-refinement	refinement	KittySing::{next_external_deadline,take_bar_fan,take_outro,settle}	The external run refines ExternalCelebrationDeadline: no deadline without a run, now while an event is due, strictly future after it is taken, stable across unrelated queries; a consumed event never re-arms now and missed bars are skipped.	0,1,2,10	3
ks-notes-cull-and-spawn	contract	MusicNotes::{update,is_active}	update removes notes older than NOTE_LIFE and spawns exactly one note per new half beat only while streaming; non-streaming updates over NOTE_LIFE leave the field inactive.	0,1,2,3,7	1
ks-note-resolve	contract	MusicNotes::{resolve,frame_array}	A note's alpha is the floor of 255 times its fade-in and fade-out ramps; under reduced motion its position is fixed by its seed, in full motion it rises with time; outside its life it draws nothing.	0,1,3	1
ks-note-art	table-property	kitty_sing::{bake_note,note_nat_size,note_host_id}; kitty_cursor::HOST_ID	At the shipped sizes the two note kinds bake distinct, translucent tiles (max alpha 100..250, coverage 5..75%); note_host_id never collides with the cursor sprite's HOST_ID ^ generation for generation < 64.	0,1,3,5,7	2
ksum-hit-is-lexicon-completion	equivalence	TypedKittySummon::{note_char,trailing_scan_start,trim}	The incremental detector equals the whole-window rescan: after each char, the class reported is the lexicon's match ending at the window's end (Profanity first, else the last match), the window clears on a match, and stays within BUF_CAP chars (BUF_CAP covers the longest lexicon surface).	0,1,2,4,7,12	3
ksum-tiers	contract	TypedKittySummon::note_char; TypedHit	keys_typed counts every char; a Profanity completion reports profanity, a Feline completion reports feline and records (stamping the clock) only past TYPED_SUMMON_COOLDOWN, a Canine completion reports canine only after DOG_SUMMON_KEYS keys.	0,1,2,4	5
ksum-window-edits	contract	TypedKittySummon::{note_backspace,note_break,rekey}	Backspace rekeys then pops a char and never reports a hit; a break clears the window; a session change clears it; a completion clears it.	0,1,2	4
ksum-merge	contract	TypedHit::merge	The field-wise OR.	0,1	1
ksum-builtin-vocabulary	table-property	aterm_lexicon::Lexicon::builtin; TypedKittySummon::note_char	Under the builtin lexicon the listed feline, profanity and canine words complete in any case and language; glued words and the Scunthorpe set complete nothing.	0,1,12	6
rb-progressive-bake	contract	matrix_rain::bake::RainBaker::{begin_frame,restart,bake_tiles,complete,tile,version}	begin_frame restarts only on a metric change; bake_tiles bakes the next 8 of 64 tiles, each one interned cell-sized white-coverage art, and bumps version unless complete; a full bake is 8 batches with increasing versions, a function of the call sequence, and a rebuilt baker bakes the same arts; tile(g) is None until glyph g is baked.	0,1,2,7	3
rb-box-filter	contract	RainBaker::bake_tile	Each texel's alpha is the exact integer area-weighted box filter of the 24x48 master over the cell, never above 255; at the live metrics every classic tile peaks at >= 64.	0,1,3,5,7	1
bf-table-lookup	table-property	matrix_rain::bitmap_font::{GLYPHS,bitmap}	GLYPHS keys are strictly increasing and no glyph is blank, so bitmap(c) is the exact table entry or None; the advertised families are present and the listed absences absent.	0,1,3,7	3
rom-plot-in-bounds	invariant	matrix_rain::rom::{plot,stamp_thick,stamp_line,stamp_dot,rasterize_master,author_material_glyph}	plot drops out-of-range pixels, so every master scanline fits MASTER_W, and stamp_line terminates.	0,1,3,7	1
rom-classic-table	table-property	matrix_rain::rom::{RECIPES,rasterize_master}	The 64 classic glyphs each have >= 12 inked pixels and are pairwise distinct.	0,1,5	2
rom-material-master	contract	matrix_rain::rom::{rasterize_material_master,reauthor_material_glyph,author_material_glyph}	Material slots with a bitmap are exactly its 2x5 expansion at offset (4,4), other slots the decorative master; reauthoring one slot equals a fresh rasterize for that slot and touches no other.	0,1,3,7	1
rf-hash	contract	matrix_rain::field::rain_hash32	The murmur3 fmix32 finalizer, a bijection on u32; the pinned values are evaluations.	0,3	1
rf-bits	contract	matrix_rain::field::bits	For a <= b < a + 32, bits(x,a,b) = (x >> a) mod 2^(b-a).	0,3,5	1
rf-col-params	contract	matrix_rain::field::col_params	For rows <= 2^16, each column's period, length, cycle and phase stay in range, and cycle x period x tick_ms >= 1000 (the WCAG flash floor), with no panic for tiny viewports.	0,3,5	4
rf-quanta	contract	matrix_rain::field::{FieldParams::quanta,ticks_of}	quanta gives mutation, dither and stammer quanta in ticks with the dither a multiple of the mutation quantum and at least 350 ms, using ticks_of(ms,t) = max(1, (ms + t/2)/t).	0,3,5	1
rf-trail-level	contract	matrix_rain::field::{trail_level,cycle_at}	trail_level is Some(15 - floor(15 e/(l p))) for elapsed e in [0, l p] and None outside, so the head row is level 15 when l >= 15 and rows far behind are unlit.	0,3	1
rf-stammer	contract	matrix_rain::field::stammer_tick	stammer_tick(t,sq) holds one tick in window 7 of every 8 and never underflows.	0,3	1
rf-density-ring	refinement	matrix_rain::field::DensityRing::{push,at,clear}	The ring refines the list of its last 8 (tick, density) pushes; at(t) is the newest entry at or before t, else the oldest, else 0.	0,1,2,7	1
rf-quad-cap	contract	matrix_rain::field::quad_cap	quad_cap(w,h) = min(2048, max(256, 409600/(max(w,1) max(h,1)))).	0,3	1
rl-execute-edge	contract	matrix_rain::step::RainLatches::note_shell_executing	Returns true iff the stored latch was (s, false) and e, then stores (s, e).	0,1,2	1
rl-completion-news	contract	RainLatches::{take_new_completion,baseline_completion,observe_without_engine}	A completion is news once per (session, seq) key and only in the session that held the previous key (Some(failed)); baseline_completion records the key silently; observe_without_engine keeps both latches observing.	0,1,2	2
mr-clock-path-equivalence	equivalence	MatrixRain::{tick,sync_clock,advance_ms,emit}	Driving the rain by tick(t_i) or by advance_ms(dt_i) then emit gives identical quads, halos and fingerprints.	0,1,2,4	1
mr-budget-and-truncation	invariant	MatrixRain::{emit_field,emit_column,emit_halos}	Every emit stays within quad_cap and MAX_RAIN_ADD, and the emitted columns are a prefix of the hash-sorted column order, each with all its lit cells (a column that would cross the cap is dropped whole).	0,1,2,7	2
mr-quad-geometry	invariant	MatrixRain::{cell_quad,row_sort_stable,emit_halos,emit}	Every rain cell is one cell at its grid position, drawing its glyph's baked cell-sized tile 1:1, with alpha <= RAIN_ALPHA_CAP; the list is row-sorted and stable; halos are single-row; 1- and 2-row panes do not panic.	0,1,2,3,7	3
mr-emission-mask	invariant	MatrixRain::{emit_column,cell_unmasked,cell_eligible,row_banded}	Every quad lands on an occupied, unbanded, unselected cell; a row is banded within 2 of the cursor, in the hidden band, or in the bottom 6 rows when the cursor ring is unknown.	0,1,3,4	2
mr-occupancy-rescan	contract	MatrixRain::{rescan_from_cells,clear_occ_span,clear_occ_neighborhood_span,occ_bit}	After a rescan a cell is occupiable iff its line is single-width, no meaningful cell or image anchor is in its 3x3 neighbourhood, it is outside every recognized frame, and no frame bound overflowed; trimmed rows' absent cells are blank.	0,1,2,3,7	2
mr-frame-regions	contract	MatrixRain::mask_framed_regions	A box is recognized from matching corners with rules and sides covering at least 3/4 and at most 2 gap rows; its interior is cleared; text cannot fake a bottom edge; unmatched corners recognize nothing; more than 64 candidates clears all occupancy.	0,1,2,3,7	4
mr-material-sample	contract	MatrixRain::{sample_material,set_config,clear_material_bank,needs_material_sample}; slot_change_mask	With material off the bank is cleared; while editing it is held and a sample is owed; otherwise the bank is the sorted last 64 distinct chars of the forward output tail and its indices, rebaked only when the char set changes, and the incremental rebake equals a full rebuild.	0,1,2,4,7	4
mr-composer-gate	refinement	MatrixRain::{note_keystroke,note_signal,note_grid_replaced,sample_material,set_material_editing}	The rain refines ComposerMaterialGate: a key sets editing, TurnStart and grid replacement clear it, and while editing no draft is sampled.	0,1,2,10	3
mr-literal-strict	contract	MatrixRain::{emit,is_active,cell_quad,semantic_material_index,material_start}	Literal mode with an empty bank emits nothing and is inactive; otherwise every quad is an unmirrored material tile, reading source order down each column, every slot reachable.	0,1,3,4	3
mr-semantic-pulse-refinement	refinement	MatrixRain::{note_signal,apply_pending_notes,step_engine_tick,set_reduced_motion,reset}; RainSignal::{from_code,priority}	The rain refines SemanticRainPulse: unknown codes are ignored, a higher-priority pending signal wins, a pulse sets phase and energy for SEMANTIC_HOLD_TICKS then expires, success starts a wave and failure an ember, reduced motion and reset drop pulses; work phases wake a slept engine and expire without gaps.	0,1,2,10	6
mr-semantic-lanes	contract	MatrixRain::{material_start,semantic_material_index}	Under a semantic phase material_start depends on the column only through its lane; Inspect lanes read row + epoch; Modify's odd columns run counterflow.	0,3	0
mr-signal-abi	round-trip	RainSignal::from_code	from_code(k) = Some(s) with s as u32 = k for k < 11, None otherwise.	0,3,5	1
mr-activity-credit-refinement	refinement	MatrixRain::{note_activity,apply_pending_notes}	The rain refines RainActivityCredit: the first note baselines, a backward seq rebases, deltas add up to 32, echo-window credit with no TurnStart is discarded, otherwise it grows the streak.	0,1,2,10	3
mr-weather-machine	contract	MatrixRain::{step_engine_tick,desired_weather}	desired_weather is Working for a live content streak, the phase's weather for a live pulse, Calm for recent keys or content, else Sleep; weather changes only to the desired one after DWELL_MS; keys alone never pour; leaving Working fires the turn wave.	0,1,2,3	3
mr-wave-limiter	invariant	MatrixRain::step_engine_tick (ign_ms ring)	At most 2 wave ignitions per 1000 ms of engine time; a pending wave is delayed, never dropped.	0,1,2,5	1
mr-drain-bound	contract	MatrixRain::{set_visibility,step_engine_tick,emit,is_active,can_emit,step_suspended}	Hidden sets drain_ticks to 30 at once, unfocused or sleeping ticks count it up, focus resets it unless sleeping; emit is empty at 30; so Hidden empties the next frame, unfocused panes empty within 30 ticks, idle drains and disarms, and a suspended step starves to sleep.	0,1,2	4
mr-lifecycle-refinement	refinement	MatrixRain::{set_visibility,step_engine_tick,emit}	The rain refines rain_lifecycle_model: activity, drain and drain ticks map to real steps, Idle at DRAIN_TICKS, and a refocus of a drained slept pane does not relight.	0,1,2,10	2
mr-gates-off	contract	MatrixRain::{emit,is_active,note_alt_scroll,tile}	Disabled or reduced motion emits nothing, is inactive and bakes nothing; scrollback, or the alt screen when suppressed or within the scroll quiet window, gates the frame and disarms until a live emit.	0,1,2	3
mr-pending-notes	contract	MatrixRain::{note_bell,note_exit_status,step_suspended,apply_pending_notes}	A suspended step drops key and content notes and keeps bell, exit, alt-scroll and signal notes; the next emit applies the bell flash and the failure ember or success wave per their knobs.	0,1,2	2
mr-fingerprint	contract	matrix_rain::fingerprint	0 for empty output, otherwise an FNV chain over the version, each quad's injective packing, the tick folded once and each halo's injective 3-word packing (`RainHalo::damage_words`); injective in the tick, so even quad counts cannot cancel.	0,1,3,5,7	3
mr-step-stable-damage	contract	MatrixRain::emit_column; field::{trail_level,dither}	Between two ticks sharing their quanta at most 3 + ceil(15/p) cells of a column change.	0,1,3,5	1
mr-luminance	contract	matrix_rain::{derive_alphas,clamp_config,srgb_over,build_ramp}	Overrides clamp to 16..=135 with head >= body; derived alphas keep every level and the head composite closer to the ground than SGR-2 dim text on both stock themes.	0,1,3,5,7	2
mr-rescan-gate	contract	MatrixRain::{needs_rescan,note_grid_replaced,scanned_epoch}	needs_rescan(e) iff never scanned or e differs from the last epoch; note_grid_replaced forces a rescan, clears the bank and the editing latch.	0,1,2	3
mr-engine-bake	contract	MatrixRain::emit (baker branch)	An ungated classic emit bakes at most one batch per engine tick until complete, versions rise per batch and stop when complete, and a metric change restarts.	0,1,2,4	1
mo-resolve-truth-table	table-property	motion::MotionPolicy::{resolve,amplitude,animate}	resolve = Full iff focused and (Full, or Auto without the system reduce flag); amplitude is 0 for Reduced and 1 for Full; animate iff Full.	0,5	2
mo-load-shed-exempt	table-property	motion::MotionEffect::load_shed_exempt	Exactly SmoothScroll and ScrollPill.	0	1
mo-parse	contract	motion::MotionMode::parse	Full for "full", Reduced for "reduced", "reduce", "none" or "off" (trimmed, ASCII case-insensitive), Auto otherwise.	0,1	1
mo-serious	table-property	motion::SeriousModePolicy::{resolve,allows}	resolve(serious).allows(e) = not serious for every effect.	0	1
mo-serious-refinement	refinement	motion::SeriousModePolicy::allows	Every gate of serious_mode_model equals want and allows(effect), the cursor gates equal the trail gate, disabling restores the request, and Buggy = 1 breaks SeriousSilencesEverything.	0,10	1
mo-reduce-motion-audible	contract	sound_policy::fold_window_audibility; CursorGlow::{tick,sound_seam_open,cue_keystroke_kind}	After folding the motion policy into the glow config, the sound seam is open iff the window is focused, whatever the motion policy.	0,1,2,4	1
nova-phase	contract	nova::{phase,dip_envelope}	phase(t,d) is Armed, Dip, Flash, Ring, Debris, Ember, Settled on fixed windows, monotone in t; dip_envelope dips by 0.35 smoothstep over 120 ms.	0,1,3	1
nova-budget-geometry	invariant	nova::{emit_nova,emit_ring,emit_rays,emit_crown,QuadSink::push,ring_band_count}	For every genome-reachable feature set and cell_h >= 14, emit_nova emits at most 392 quads, so the 512 budget never binds; quads are single-row and inside the grid.	0,1,3,5,7	1
nova-spend-order	contract	nova::{emit_nova,QuadSink::push}	Under a budget the ring and debris phases emit a prefix of the unbounded emission (ring first, rays inner to outer); the Flash crown is emitted whole or not at all.	0,1,2,3,7	1
nova-crossing	contract	nova::{crossing_ms,ring_radius,pulse_env}	crossing_ms inverts the ring's ease-out (monotone in distance), matching ring_radius within a millisecond; pulse_env is a 150 ms smoothstep bump.	0,1,3,5	1
nova-delta-l	table-property	nova::{constant_lum_toward,pulse_color}; color_math::hue_nudge	For every deployed anchor, fringe and amp, the constant-luminance mix and the pulse colour change relative luminance by at most 0.05 and keep red fraction under 0.8.	0,1,3,5,7	1
nova-quasar	contract	nova::emit_rays (Quasar)	Quasar emits exactly two vertical jets within 4 px of centre reaching past 1.5 r_max late, regardless of the ray count.	0,1,3	1
nova-singularity	contract	nova::{ring_radius,emit_ring_arc,emit_debris}	The contracting ring radius strictly decreases; ring arcs are capped Over decorations on the radius; Singularity debris radii fall.	0,1,3,7	1
nova-debris	contract	nova::emit_debris	Debris appears only in [500, D), at most min(cap, debris) Add decorations, each twinkling on the 350 ms phase grid.	0,1,3,7	1
nova-ember-pair	contract	nova::ember_pair	Singularity's fixed pair; otherwise channels scaled by 0.8 and 0.55, never brighter than the input.	0,1,3	1
nova-host-refinement	refinement	WordDecorations::{rescan,tick,hard_reset,freeze,thaw}; nova::phase	The host's nova episodes refine nova_phase_model: one flash per arm, monotone phases, re-arm only through hard_reset, freeze/thaw resumes; a re-arm into Flash is the Buggy step.	0,1,2,10,12	2
nuke-nat-size	contract	nuke::nuke_nat_size	For cell_h >= 2 every part's natural size is 4..=4ch wide and 4..=2ch tall.	0,1,3	1
nuke-draw-windows	contract	nuke::{nuke_draw,nuke_core_draw,cool}	Each part draws only in its window with finite positive scales and alpha in [0,1]; the core is smaller than the cap; the cap's tint cools.	0,1,3,5	2
nuke-bake-nonempty	table-property	nuke::bake_nuke	Each part bakes a non-empty tile at its natural size.	0,1,3,7	1
os-license	contract	OutputStreak::{note_output,rebase,note_keystroke,note_turn_boundary,within_echo_window,note_drawn_at,unwatched}	note_output always stores the token and returns true iff the token changed from a previous one, spans are non-empty, the row is not hot, no key landed within 250 ms and the screen was drawn no more than UNWATCHED_MS (one longest sweep) before the observation (note_drawn_at, the host terminal's drawn_at; no stamp refuses nothing); rebase makes the next observation a baseline; note_turn_boundary clears only older echo shadows. Known limit, the sync hold: drawn_at is when the screen last changed, not when a frame could first show it. The native host holds a frame on an open synchronized update (DEC 2026) at most SYNC_HOLD_CAP (aterm-gui's, 150 ms, under sync_timeout_ms) from the frame that first sees it open, so one update drawn at t0 is shown and lit within that cap however late it closes; but a chain of updates, each closed and reopened between two frames and written to with no screen change (a cursor move), re-arms the hold on every frame, and a screen drawn at t0 that such a chain keeps from every frame for more than UNWATCHED_MS mints no light on its first shown frame.	0,1,2	6
os-cell-observation	contract	OutputStreak::note_output_cells; ObservedGlyph	An unchanged token and viewport is no output; otherwise the span is the rightmost run of changed ink on the cursor's anchor row, None on a baseline, echo, blank or unchanged row, or a moved copy of another row.	0,1,2,7,13	2
os-hue-for	contract	output_streak::{nearest_spectrum_stop_by_hue,spectrum_stop_jitter_radius}; OutputStreak::hue_for	Each named stop maps to itself; the jitter radius is half the narrower neighbouring gap; hue_for stays inside its stop's Voronoi interval, never wrapping.	0,1,3,7	2
os-clipped-span	contract	output_streak::clipped_span	None for a reversed span or one starting past the grid, else the span clipped to the last column.	0,3	1
os-inert	contract	OutputStreak::tick	Disabled, zero intensity or degenerate geometry resets the engine (seed kept), emits nothing, returns fp 0 and no cue.	0,1,2	2
os-ignition-floor	invariant	OutputStreak::{spend_pending,refresh_ribbon}	Spawns and ribbon re-homes are at least SPAWN_MIN_GAP_MS apart and cues ride only spawns, so ignitions are bounded by 1 + T/700 ms; a row with a comet does not respawn.	0,1,2,5	2
os-flood	contract	OutputStreak::{spend_pending,emit}	At saturated momentum no comet spawns (the ribbon refreshes silently) and each frame emits at most cols + MAX_COMETS (TAIL_MAX + 2) quads.	0,1,2,7	1
os-episode-sound	invariant	OutputStreak::{open_episode_cue,close_episode_cue}	Each episode has at most one Shimmer (rate-limited) followed by exactly one Settle when momentum falls below EPISODE_LOW.	0,1,2	1
os-idle-deadline	contract	output_streak::idle_duration; OutputStreak::{next_change_deadline,pending_idle_deadline,idle_drained,tick}	idle_duration is Some only for finite positive representable seconds; the deadline is now while drawing, else the earlier of the idle cutoff and the Settle crossing; a tick at the cutoff consumes it; a settled engine owes nothing.	0,1,2,3	4
os-amplitude	invariant	OutputStreak::push_cell	Every channel stays under round(ALPHA_CLAMP 255) for any intensity; dark themes emit additive quads and light themes source-over.	0,1,3	2
os-comet-contiguous	invariant	OutputStreak::emit	Each comet paints one contiguous run on one row starting at its tail.	0,1,3,7	1
pane-crop-sampling	contract	pane::{crop_free_sprite,translate_free_into_pane}	Moving a sprite into its pane shifts its box and its clip and cuts the clip at the pane's interior box, keeping the box and the art whole, so every surviving pixel samples the texel it sampled uncut, mirrored or not; a sprite wholly outside is dropped.	0,1,2,3,5	1
pb-roster	table-property	pet_glyphs_gen::{PET_GLYPHS,PET_GLYPH_IDS}; pet_baker::{PetBaker::aspect,bake_pose}	The pet roster is non-empty with matching tables, positive aspects and layers; the stand pose draws a cat; coat, background and pose axes change texels.	0,1,5,7	3
pb-host-id	contract	PetBakeKey::host_id; host_tile_id	host_id is an FNV chain over the key fields with bit 63 set, injective in each single field except where pre-OR hashes differ only in bit 63.	0,1,3,5	1
pb-face-lod-roles	contract	pet_baker::bake_pose_with_reflections	Below FACE_DETAIL_MIN_H charm layers are skipped and eyes become dots, below MOUTH_DETAIL_MIN_H the mouth is skipped, above every layer paints; the reflection rebuild appears at small sizes and the authored catch-light at large.	0,1,3,7	2
pb-eye-reflections	contract	pet_baker::paint_eye_reflections	At small sizes every reflection texel was dark and lies in one grown eye mask; every open eye gains a bright texel; shallow eyes gain none.	0,1,3,7,13	1
pb-eye-legibility	table-property	pet_baker::paint_eye_dots	For every live pose and ground at 24 and 36 px each eye keeps a dark core of lawful size, eyes stay apart, and none is over-occluded.	0,1,3,5,7	2
ps-stroke-refinement	refinement	pet_stroke::StrokeDetector::sample	The detector refines pet_stroke_detector_model; only complete strokes earn, a reset forgets contact, the cooldown cannot be bypassed, and the mutants are rejected.	0,1,2,10	1
ps-stroke-contract	contract	pet_stroke::StrokeDetector::sample	sample earns iff the input is finite, the point moved with a gap in (0, 600 ms], normalized speed in [0.04, 2.5], not cooling, and travel reaches 0.45, then starts a 1.5 s cooldown; invalid input forgets contact but keeps the cooldown; gaps, bad speeds and cooling moves zero travel; a still pointer never earns.	0,1,2,3	4
ps-still-frames-equivalence	equivalence	pet_stroke::StrokeDetector::sample	Inserting repeated still samples within 600 ms changes no output or state.	0,1,2,5	1
ps-scale-invariance	other	pet_stroke::StrokeDetector::sample	Earning depends only on displacements normalized by body width and on time, so scaling points and width together preserves outputs (up to f32 rounding).	0,1,3,5	1
pw-coherence-gate	contract	pet_world::PetWorld::observe_with_exclusions_near	observe returns false and leaves the world retired iff a fence fails (terminal id, content sequence, alt screen, base, row revision, display offset, pane dims, region counts, protected, overlay and typed-ink reserve validity).	0,1,2	1
pw-classify	contract	PetWorld::{observe_with_exclusions_near,cell,ink_at,locomotion_ink_at}	After observing, each covered cell is Protected (selected, a protected exclusion or overlay, image, non-single line, caret ring), else Ink (glyph, reserved typed ink, or non-default bg per column), else Clear; reserved typed ink never enters the glyph plane, is in its row's ink hull, is Ink on the ground plane too, and a caret-ring cell under it is not blank; missing rows are Unknown; ink_at ignores caret and selection protection; locomotion ignores a lone Braille dot.	0,1,2,7	7
pw-clearance-planes	contract	PetWorld::{clearance,under_text_clearance,clearance_past_caret,under_text_clearance_past_caret,ink_clearance,seen_under_text_clearance_past_caret,clear,under_text_clear,window,blocked}	Clearance queries answer None without coverage, false for invalid rects, and otherwise whether the window holds no cell of their plane (placement: unknown, ink, protected; under-text: unknown, protected), with caret-ring forgiveness for the past-caret forms; the prefix tables equal the counts after every observe.	0,1,3,5,7	6
pw-reuse-equals-fresh	equivalence	PetWorld::observe_with_exclusions_near	Observing into a reused world equals observing into a fresh one on every query.	0,1,2,5,7	1
pw-reading-kept	equivalence	kitty_pet::PetBrain::observe_console_with_reserve; pet_world::PetReading::{holds,note,keep}; pet_world::PetFill::after_refill	Of a PetFill::Consumed fill whose stamps (extraction generation, damage epoch, content generation, parser batch, screen mutations, terminal, screen), caret, scroll fences, size, theme, selections, facts, pane, host reserve and, past the 64 x 256 budget, held centre equal the last reading's, or a PetFill::Retained fill (a damage-consuming fill that re-extracted no row) whose prior extraction generation is the reading's and whose other stamps equal it, an observation classifies no cell and leaves the world, the observation count, the row signatures, the faux caret and the contact exactly as a fresh reading of the same cells leaves them, so the pet draws the same frame, and the kept reading takes the scratch's new generation; any one of those moving, an Unconsumed fill or a host write (snapshot_seq off engine_fill_seq) takes a new reading. Both hosts keep: the window redraw that reuses its snapshot and the hosts that refill every frame (a split's focused pane, a window with chrome rows).	0,1,2,5	3
pw-coverage-window	contract	PetWorld::observe_with_exclusions_near (coverage)	The coverage window is at most 64 x 256 cells, centred on the live caret, else the selection target, else the held centre or the pane middle, clamped into the pane.	0,1,3	1
pw-selection-target	contract	pet_world::selection_target	The selected cell nearest the active endpoint among five candidate rows of the visible span, or None.	0,1,3	1
pw-anchors	contract	PetWorldFacts::read; PetWorld::resolve	Anchors exist only off the alt screen, one per visible block at its last glyph row, and resolve only on the same surface while the block and row are on screen.	0,1,12	2
pw-progress	contract	PetWorldFacts::read; PetWorld::progress	progress() is the terminal's last OSC 9;4 state and samples no anchor or completion.	0,1,12	1
pw-perch	contract	PetWorld::{perch_near,nearest_perch,home_perch}	perch_near returns the clear candidate minimizing the weighted distance within d (the target's own column a candidate, ties to the least row-and-column sum), or None when none is clear.	0,1,3,7	1
pw-rowscan-equivalence	equivalence	RowScan::{default_bg,line_size,cluster,combining,image}; SparseRow::get	For every row and non-decreasing query sequence the row cursors answer exactly what the per-cell lookups answer, including malformed sidecars.	0,1,2,7,12	2
pl-all-off-identity	contract	EffectsPipeline::{apply,enabled_any,is_active}	With nothing enabled apply clears every channel, retires the pet and bodies, baselines the fences and returns 0, keeping the pet opt-in; a disabled rain or streak leaves its channels as others left them and adds nothing to is_active.	0,1,2	3
pl-rain-host-latches	contract	EffectsPipeline::apply (rain arm); MatrixRain::host_frame	A torn frame runs only note_activity and advances no latch; a fresh frame runs the host frame with the execute and completion latches; a suppressed alt screen runs the suspended arm; with no engine the latches keep observing, so re-enabling replays nothing.	0,1,2,4,12	4
pl-glow-config	contract	EffectsPipeline::set_cursor_glow; cursor_fx::{GlowSpelling::parse,resolve_glow_config}; GlowStyle::{style_names_any_pet,style_names_dog_pet}	set_cursor_glow stores the native resolved config (audible), with the pet and species flags from the spelling; the spelling table holds for beam, non-finite knobs, every rainbow kitty and v2 spelling, tall vs underline and the pet spellings.	0,1,4	4
pl-history-lifecycle-refinement	refinement	EffectsPipeline::apply (history arm)	In history or on an incoherent snapshot every cursor channel clears, the caret is hidden and the pet draws nothing while its brain fades; refines cursor_viewport_lifecycle_model.	0,1,2,10,12	1
pl-coordinate-fence-refinement	refinement	EffectsPipeline::sync_cursor_coordinate_space	A change of rows, cell metrics or chrome origin makes every later frame equal to a fresh family that kept only glow, momentum and cadence (layout_coordinate_reset_model).	0,1,2,4,10	1
pl-scroll-translation	refinement	pipeline::content_scroll_decision; EffectsPipeline::sync_scroll; ContentScrollState::delta_since	The scroll decision is delta_since: a silent baseline, Translate and explained Bands move licensed trail cells exactly, Invalidate clears; each validates as its cursor_scroll_signal_model action.	0,1,2,4,10,12	4
pl-owner-fence	contract	EffectsPipeline::{fence_owner,set_effects_visibility,sync_cursor_coordinate_space}; CompanionOwner::grieving	Only an owner fence change re-baselines the pet's command probe, so a failure across a hide or resize is still grieved; while grieving the glow's momentum is zeroed.	0,1,2,4,12	2
pl-pet-contrast-cursor	contract	pipeline::{pet_contrast_cursor,terminal_cursor_color}	The stamped cursor colour unless unset, else the terminal's (OSC 12 or default foreground), as RGB.	0,1,3	1
pl-pet-ownership	contract	EffectsPipeline::{apply,set_cursor_pet,set_focused,set_effects_visibility}; CompanionOwner	The pet draws only when opted in, the master is on and the style names a pet, otherwise the output is identical to no pet; a style change retires it, history hides it, unfocus and Hidden retire it, the worn pair survives; the seed is the only entropy; it fades in, follows the caret, and sleeps opaque releasing the lane.	0,1,2,4,12	5
pl-pet-input-latch	contract	EffectsPipeline::{note_pointer_px,note_pointer_leave,press_px,companion_hit_rect,set_host_rows_px,note_trick,revoke_trick,note_trick_submit}	Pointer notes report change; a press inside the hit rect pets; notes only move latches until a tick; hit rects and pointers share frame px with host rows; strokes reach the detector and still pointers earn nothing; tricks latch and revoke.	0,1,2,4	4
pl-pet-reduced-motion	contract	EffectsPipeline::set_reduced_motion (pet arm)	Under reduced motion a caret jump moves the drawn pet to its new station in one frame; the sparkle and rain setters alias the same preference.	0,1,2,4	1
pl-scheduler	contract	EffectsPipeline::{is_active,next_deadline_ms,wake}	next_deadline_ms is None while any frame-cadence lane is live, else the minimum of the engines' deadlines (rain keeps partial progress, the streak's idle or Settle, a settling caret body's next step); is_active is the engines' disjunction, settling caret bodies included; an unfocused glow settles.	0,1,4	4
pl-pet-word-handoff	contract	EffectsPipeline::apply (companion_at); WordDecorations::tick	The pet's body rect reaches the word engine, so no other animal sprite overlaps a third of the pet's body; with the pet off the word cat peeks.	0,1,4	1
pl-glow-polarity	contract	EffectsPipeline::apply (dark_theme)	dark_theme follows the stamped default background (OSC 11, DECSCNM).	0,1,3	1
pl-auto-colors	contract	EffectsPipeline::apply (colour provenance)	apply runs the native colour step, cursor_fx::follow_live_cursor(glow, trail, live cursor, term.cursor_color_recoloured(), the page's TrailColor, explicit accent, the trail's own colour): an automatic glow colour (and its automatic accent, brightened 1.5x) is cursor_fx::automatic_glow_color — a style with no canonical hue follows the live cursor colour; Beam, Comet and Sparkle keep their canonical hue until OSC 12 pins the cursor and return to it on OSC 112; Laser never follows; the cursor's colour, chosen, is the live cursor on every style. An automatic opaque trail wears the glow's drawn colour, as the native comet body does; a trail colour the page set stays. Explicit colours never change.	0,1	3
pl-key-replay	contract	EffectsPipeline::{note_keystroke,advance}	Pending keys (capped at 64) replay evenly spaced over the next positive finite advance, so cadence equals feeding those instants and is cooler than one instant.	0,1,2,3,7	1
pl-chrome-geom	contract	EffectsPipeline::chrome_geom	The saturating u16 geometry with origin (pad, pad + head) and window size including padding and head; zero pad and head is the identity layout.	0,3	1
pl-fire-streams	contract	EffectsPipeline::{apply,publish_cursor}	Fire styles splice their patch and companion streams into the head band; non-fire styles never emit them; the forge fill is published and scrubbed on switch.	0,1,2,4	3
pl-focus-gate	contract	EffectsPipeline::apply (motion policy)	Unfocused panes draw no cursor light, clear the comet but keep the trail colour, restore the configured intensity after every apply, resume without a phantom comet, and leave word decorations unchanged.	0,1,2,4	5
pl-hidden-boundary	contract	EffectsPipeline::set_rain_visibility	Entering Hidden resets the glow, bodies, pending keys, cadence, scratch and pet surface, so nothing resurrects on refocus.	0,1,2	2
pl-reduced-motion-cursor	contract	EffectsPipeline::apply (reduced motion)	Under reduced motion no body style owns the fill, adds glow, overrides the caret or animates.	0,1,2,4	1
pl-input-provenance	contract	EffectsPipeline::{note_keystroke,apply}; cursor_trail::note_typed_batch	A comet is minted only for motion licensed by host input in the same window; program-only motion mints nothing.	0,1,2,4	1
pl-composer-toggle-refinement	refinement	EffectsPipeline::{note_keystroke,note_matrix_rain_signal,set_matrix_rain_enabled}	The pipeline refines ComposerToggleGate (the engine's editing latch matches the host's whenever rain exists), so an unsent draft never enters literal material.	0,1,2,10	2
pl-sparkle-config	contract	EffectsPipeline::{set_sparkle_classes,set_sparkle_ink,set_sparkle_custom_specs,sparkle_lexicon_warnings,set_sparkle_feline,set_sparkle_enabled}; pipeline::lexicon_warning_applies	Emphasis needs ink or a custom spec; a malformed fragment keeps the old specs and adds one warning, None clears it; warnings are filtered by the CJK opt-in; a custom rainbow word inks its leads.	0,1,9,12	2
pl-sparkle-reset-table	contract	EffectsPipeline::{refresh_sparkle_cfg,rebuild_sparkle,apply}	Every sparkle setter hard-resets the word engine; a suppressed alt screen freezes and thaws a peeking cat in place.	0,1,2,4	1
pl-word-scan-context	contract	EffectsPipeline::apply (word_grid)	The word pass scans resolved cells with the live caret: a cat's palette is sampled over its footprint and a caret-touching collision form stays provisional.	0,1,4,12	2
pl-streak-channel	contract	EffectsPipeline::{apply,streak_grid,set_output_streak,set_output_streak_reduced_motion}	The streak only appends to nova_add and a disabled streak contributes nothing. The web apply path scales its per-frame intensity by MotionPolicy::amplitude(OutputStreak) (with set_output_streak_reduced_motion gating it to zero independently): reduced motion retires visible comets and prevents new ones without changing the configured intensity, which returns when the policy allows motion.	0,1,2	1
pl-rain-integration	contract	EffectsPipeline::{apply,set_matrix_rain,set_matrix_rain_theme}	Enabled rain emits its row layers, repeat apply is stable (the same layers, halos and fingerprint, no rebake), a live mode switch resamples, the theme setter updates both configs, and scrollback defers the scan.	0,1,2,4	4
pl-web-glyph-class	equivalence	pipeline::web_typed_class; EffectsPipeline::note_committed_char	web_typed_class maps space, bang, capitals and the rest; note_committed_char yields the same glow streams as the engine-level cadence and note_typed_glyph calls.	0,1,2,4	1
pl-console-input-credits	contract	EffectsPipeline::note_console_input; cursor_glow press-credit pool	note_console_input counts and records the kind and queues no replay; Text, Navigate, Submit and Paste keep credits, Delete retires the newest press, kills retire all.	0,1,2,4	2
pl-input-credit-refinement	refinement	EffectsPipeline::note_committed_char	Each accepted one-cell printable adds exactly one licence arm and one credit (cursor_hint_license_model's PressArmsLicense).	0,1,10	1
pn-flying-head-meteor	contract	pipeline::native::EffectsPipeline::route_flying_head; companions cursor_cat on_meteor	A horizontal licensed meteor routed to the flying head snaps it to the landing on frame zero, raises its spine, leans against travel, squints through the flight, and books the squash at arrival; otherwise the pose is unchanged.	0,1,2,4	1
rkc-body-for	contract	rainbow_kitty::companion::body_for; companion::cursor_companion_duty; GlowStyle::style_names_any_pet	body_for is None for a claimed frame, else the duty's body; every pet spelling yields Pet, the flying spellings Flying at the caret.	0,1,4	3
rkc-impulse-for	contract	rainbow_kitty::companion::{impulse_for,arrival}; Flight::{disp_floor,squint_until,settled_at}	A meteor gives the flying head a Flight landing at t0 + T and the pet a Perk at arrival; land, wince and delight react; the flight's floors and clocks are fixed offsets.	0,1	1
rkc-heroes	contract	rainbow_kitty::companion::heroes_earned	1 for Delight and 0 otherwise.	0	1
rkc-placement	contract	rainbow_kitty::companion::{placement,census,cell_span,seat}	The seat is the shipped footprint if clear, the mirror if the probe is blind and the footprint covers the caret, one row up if clear where the head held it last frame or where the margin clamp put the footprint on the caret of a line written within WRITTEN_REACH, the mirror if clear, else one row up (over ink if needed) or on row 0 the side that clears the caret; so the caret cell is never covered when some candidate clears it, and a same-row seat covers only blank cells.	0,1,3,7,11,13	4
rkc-whip	contract	Flight::lead_at; timing::spring_whip	lead_at = -0.30 + 0.52 spring_whip(elapsed), overshooting its rest; v2 holds cadence until settled_at only for a Fly impulse.	0,1,3,5	2
spine-follower-step	contract	spine::Spine::{update,target,drive}	The first update sets disp to the target T = clamp01(momentum * METRIC_GAIN); later updates with dt > 0 ease disp toward T with tau DISP_RELEASE_TAU when falling, DISP_SLAM_S while a latched slam (rise > DISP_SLAM_RISE, held until the gap is under DISP_SNAP_ZERO) and DISP_ATTACK_S otherwise, snapping values under DISP_SNAP_ZERO to 0; disp_peak decays by its half-life but never below disp; dt <= 0 changes nothing but the verdict close; drive raises the metric to clamp01(f).	0,1,2,3,6	3
spine-follower-rate-independence	equivalence	spine::Spine::update	For a constant metric, splitting an update interval changes disp only by float rounding (1e-4 over 600 ms); for a decaying target 60 Hz and 120 Hz trajectories agree within 5e-3.	0,2,3,5,6,7	1
spine-gain-target	contract	spine::Spine::{target,momentum,update}	The follower chases clamp01(m METRIC_GAIN) and never writes the metric; under a pinned metric disp converges to min(1, m METRIC_GAIN) while momentum reports m.	0,1,2,3,4,6	2
spine-idle-snaps-to-exact-zero	contract	spine::Spine::{update,at_rest,birth_disp}	Once the metric stays 0, disp and disp_peak reach exactly 0 in bounded time and stay there, birth_disp is 0 and the spine is at rest.	0,2,3,6,7	1
spine-deletes-never-build	contract	spine::Spine::{drain_delete,drain_kill,advance}	drain_delete never raises momentum (strictly lowers a positive one), drain_kill drains at least as much, only advance can raise it, and both drains zero the flow run while keeping best.	0,1,2,3,4,6	1
spine-birth-price	contract	spine::Spine::{birth_disp,note_verdict,update,note_keys}	birth_disp = max(disp, DISP_PEAK_FLOOR_SHARE disp_peak, VERDICT_BIRTH_DISP while a verdict door is open); the peak never changes disp; the door closes after VERDICT_WINDOW_S and is spent by the next key.	0,1,2,3,6	2
spine-erase-mark	contract	spine::Spine::{drain_delete,advance,drain_kill,mend,erase_mark}	drain_delete keeps a mark of the highest birth price over the delete run within MEND_WINDOW_S and counts it; advance, kill and reset clear it; mend(now) returns it while the count is at most MEND_MAX_DELETES and the window is open, as a pure read that never changes disp or momentum.	0,1,2,3,6	1
spine-flow-counter	contract	spine::Spine::{note_keys,flow,drive}; spine::{flow_heat,Flow::open}	note_keys(now,k,price) spends the verdict and, outside a drive freeze, grows the run by k when price >= FLOW_KEY_DISP else breaks it, tracking best; flow heat is a smoothstep of the run between 20 and 24 keys and open iff combo >= FLOW_ENTRY_KEYS; deletes and kills take combo and heat to 0 and keep best.	0,1,2,3,4,6	2
spine-ranges-and-phase-ring	invariant	spine::Spine::{update,phase}	disp and disp_peak stay in [0,1] and phase in [0, PHASE_RING); phase advances by dt PHASE_RATE disp (rem_euclid of a non-negative value) and is frozen at disp 0.	0,1,2,3,6	1
tri-triangle-wave	contract	meteor::tri	For finite x, tri(x) = 1 - |1 - (x rem_euclid 2)|: in [0,1], the identity on [0,1], period 2, even; non-finite gives 0.	0,3,6	1
axis-and-dir-dominant	contract	meteor::Axis::of; mod::Dir::{of,sign,is_vertical}	Axis::of is Horizontal iff |dx| >= |dy| (ties x-major, NaN vertical); Dir::of resolves on the dominant axis with the stated tie rules; sign and is_vertical follow.	0,1,3	2
meteor-const-relations	table-property	meteor::{RETIRE_DECAY,BURST_ROOT_SEP_AT_MAX_N,JET_PROBE_CELLS}	exp(-RETIRE_DECAY) is within 1e-3 of CHROMA_CULL_ALPHA, BURST_ROOT_SEP_AT_MAX_N = 2 sin(pi/BURST_N_MAX), and the capped jet reach fits JET_PROBE_CELLS at every supported aspect. Decidable by evaluation.	0,3,6,14	3
meteor-grade-laws	contract	meteor::{fan_count,fan_reach_ch,burst_n,ring_ms,ring_full_radius}; timing::impact	fan_count, fan_reach_ch, burst_n and ring_ms are the stated clamped affine laws of impact(cells), each monotone in cells, with the evaluated floor and cap values.	0,3,6	1
spark-cull-exact	contract	meteor::{spark_cull_u,mint_spark,spark_at}	spark_cull_u() = SPARK_HOLD_U + (1 - SPARK_HOLD_U)(1 - sqrt(CHROMA_CULL_ALPHA)), and spark_at(s,age) is Some iff age < s.life spark_cull_u() (within a millisecond), so moving_until is exact.	0,1,3,5,6	1
landing-ink-is-hot-edge-ink	table-property	meteor::{landing_ink,landing_stop_ink}; ribbon::{hot_edge_ink,walk_pace,BedInkLut::sample}	landing_ink at every LUT entry equals hot_edge_ink(spectrum(walk_pace(.))), between entries it is within 2 levels of the lerp, and landing_stop_ink(i) = hot_edge_ink(spectrum_stop(i)); evaluated corollaries on saturation and luminance hold.	0,1,3,6,7	1
landing-walk-is-band-field	equivalence	meteor::{LandingWalk::at,landing_field}; ribbon::Ribbon::{walk_origin_at,field_at}; Cohort::t_at	LandingWalk.at(dx) = tri(zero + walk_t(d + dx)) with a band, landing_field otherwise; in every reachable ribbon zero + walk_t(d) == field_at(row,col) bitwise at every laid column, so a landing reads the band's colour bit for bit (a wake on a dark row anchors at d = 0).	0,1,3,4,5,6,7	3
meteor-spawn-latch	contract	meteor::Meteors::{on_event,on_move}; timing::{flight,flight_ms}	on_move ignores echo licences, makes a same-row hop below JUMP_MIN_CELLS at most a mini-fan sow (2..4 cells), ignores sub-pixel paths, and otherwise spawns with t_flight = flight(cells) and a latched width growing at most 1.5x across grades; Return and Pty landings are small, a Nav row change rings.	0,1,2,3,6	5
meteor-frame0-head	contract	meteor::Meteors::emit; meteor::{head_at,draw_head}; timing::enter_at_speed	On the spawn frame the lit nucleus is already at FLIGHT_P0 of the path with white trailing behind; 8 ms later it is at enter_at_speed(8/T) of the path; reduced motion puts the head at the landing from the start.	0,1,2,3,4,5,6,7	1
meteor-phase-lock	contract	meteor::Meteors::{on_event,emit}; meteor::mint_landing; Engine::tick (caret seam)	t_land is latched from the caret field at spawn; the minted landing's pin and field at 0 equal tri(t_land) whatever later frames carry; arms, petals and the nearest train station wear that stop; the seam reports this frame's field at the landing.	0,1,2,3,4,6	3
meteor-train-spectrum	contract	meteor::Meteors::emit; meteor::{build_stations,draw_train,rasterize,arc_gain}	On every flight frame the train's chromatic quads cover at least 4 named stops (5 after frame 0) spanning both ends of the LUT; vertical flights use the transposed rasterizer and cover every crossed row.	0,1,2,3,5,6,7,12	1
meteor-luminance-hierarchy	contract	meteor::Meteors::emit; meteor::{draw_head,build_stations}	The nucleus outshines every train quad; at arrival the white peaks just behind the head and dies off, the colour request falls e^-1 per its law, and the landing outshines the train; the burst's first star frame and fan stars meet their size and coverage floors.	0,1,2,3,5,6,7	3
meteor-two-tau-fade	contract	meteor::Meteors::emit; Meteor::colour_alpha; meteor::{draw_head,build_stations}	After arrival white fades with TRAIN_WHITE_TAU_MS into the caret by WHITE_CLOSE_MS and colour with TRAIN_COLOUR_TAU_MS; the white-to-colour ratio falls, no quad is grey, saturation rises, and nucleus and coma are culled exactly at their floors.	0,1,2,3,5,6,7	3
meteor-flash-is-area	contract	meteor::{flash_scale,draw_head}	The terminal flash scales the nucleus radius by flash_scale (peaking at FLASH_PEAK), never its coverage or the coma.	0,1,3,6	1
meteor-idle-and-horizon	contract	meteor::Meteors::{emit,at_rest,next_change_deadline}; Meteor::end; meteor::draw_burst	A new pool is at rest; every meteor is off glass exactly FLIGHT_OFF_GLASS_MS after arrival, after which emit writes nothing, the pool rests and names no deadline; sparks and train still show at T+400.	0,1,2,3,5,6,7	2
meteor-retire-continuity	contract	meteor::Meteors::{on_move,emit}; Meteor::{colour_alpha,retire_alpha}; meteor::shed	Retiring a meteor keeps its colour continuous and dies by FLIGHT_RETIRE_MS; a retired head freezes, its train retracts toward it, it sheds nothing more; a landed train retired by a continuation keeps its afterglow briefly.	0,1,2,3,5,6,7	4
meteor-pool-cap-and-chain	invariant	meteor::Meteors::{on_move,live}	At most FLIGHT_MAX_LIVE non-retiring meteors in a pool that never reallocates; a spawn retires unlanded meteors near its corridor or in the air on its row; a same-direction continuation near a live head chains it without the head ever moving back; chained stubs retire before another row's head.	0,1,2,3,5,6,7	3
meteor-quad-caps	invariant	meteor::Meteors::emit; meteor::{draw_train,draw_burst}	emit appends at most FLIGHT_MAX_LIVE x UNDER_QUAD_CAP under quads and draw_burst at most BURST_QUAD_CAP out quads at every grade and cell size.	0,1,2,3,6,7	2
meteor-reduced-motion-static	contract	meteor::Meteors::emit; meteor::{draw_burst,draw_flash,draw_sparks,draw_pin}; stardust::Star::pos	Under reduced motion the head is at the landing, the burst, flash and sparks are static frames, the pin draws nothing, and fan stars stay at birth.	0,1,2,3,6,7	2
meteor-fan-census	contract	meteor::Meteors::{sow_into,party}; meteor::mint_landing; stardust::Stardust	A non-small landing sows exactly fan_count stars (one gold M1, four M2, the rest grains); a Return landing sows one M2 and a few M3; a mini-fan is 1 M2 + 2 M3; party sows a fan at the caret and is off glass within 315 ms.	0,1,2,3,4,7	2
meteor-fan-geometry	contract	meteor::Meteors::sow_into; stardust::Stardust::sow; Star::{rest,pos}; GlyphProbe	Fan stars rest within FAN_RISE_MAX_CH of the landing row, in the head band only when there is one, are thrown to their jittered reach on an ease-out, and never rest on a probed glyph (nudged or dropped) while the census holds.	0,1,2,3,4,5,6,7	4
meteor-shed-stations	contract	meteor::{shed,shed_n,unit01}; Meteors::sow_into	By 0.35 T every station has shed exactly one fragment, born at its station on the path in its train colour and resting beside, not under, the train.	0,1,2,3,5,6,7	2
meteor-translate	contract	meteor::Meteors::{translate_band,translate_scroll}; meteor::mint_landing	A band move carries trains inside the band and drops ones that straddle it; a scroll moves every y up and mints landings at the moved endpoint, never above the glass.	0,1,2,3,6,7	2
meteor-pin-geometry	contract	meteor::draw_pin	At arrival the pin is a cross of 1-px hairlines spanning 2 PIN_ARM_CH ch with a white 2x2 heart; its arms shorten and close into the caret by 140 ms while the heart stays.	0,1,3,5,6,7	1
meteor-burst-geometry	contract	meteor::{draw_burst,burst_lance,burst_jet,burst_rise_limit,mint_burst}	Spikes are whole or absent, the disc emerges in 50..110 ms outside the root, every quad stays within the fan's rise, reach grows with distance, and the burst is isotropic in screen space within the stated bounds on dark and light.	0,1,3,5,6,7	5
meteor-burst-coverage	contract	meteor::{draw_burst,burst_jet}; SkyMask::row_blank	Burst coverage stays under TRANSIENT_STAR_COV_CEIL, near it at the hold and well under by T+300; jets break over probed text while the core survives; on light themes every quad is source-over within the alpha cap.	0,1,3,5,6,7	3
meteor-landing-palette-walk	contract	meteor::{draw_burst,draw_flash,draw_sparks,landing_ink_at}; Landing::field	Every lit landing quad wears the band's walk at its own column (burst, jets, flash cells, sparks), jets to the band's left are redder and to its right bluer whichever way the caret came, a warm landing throws no cyan, and the burst shows more stops at higher grade.	0,1,3,4,5,6,7	8
meteor-arrival-flash	contract	meteor::{draw_flash,Meteors::emit}; SkyMask::flash_blank	At T+8 the landing cell and its neighbour flash white; the white-to-colour ratio falls through T+200 with colour outlasting white; the white is priced by the probe (full over blank cells, capped over glyphs).	0,1,3,5,6,7	1
meteor-sparks-haloed	contract	meteor::{mint_landing,draw_sparks,spark_at}	A landing's spark count is the clamped law of its span; every spark above its hold draws a halo, and its star spans at least 5x5 px.	0,1,3,6,7	1
meteor-hand-floor	contract	meteor::{Meteors::set_hand_floor,spark_room,draw_sparks}; Engine::tick; Ribbon::hand_floor_cell	With a hand floor no spark station or quad lies left of it on that row and the count is unchanged; the engine hands the ribbon's floor to the pool on the jump's tick.	0,1,2,3,4,6,7	2
meteor-cadence	contract	meteor::Meteors::{brisk,next_change_deadline}; Landing::moving_until; Engine::{next_change_deadline,needs_frame_cadence}	Before arrival + PIN_MS the pool is brisk with frame-cadence deadlines; while anything moves it is brisk with deadlines at least TAIL_FLOOR away; until its end it names deadlines at least TAIL_FLOOR away; after its end it rests.	0,1,2,3,5,7	2
landing-pool-fifo	refinement	meteor::Meteors::{emit,party,reset}; aterm_models_effects::rainbow_landing_pool_model	The landings pool refines RainbowLandingPool: a bounded FIFO that drops the oldest on admission at capacity; dropping the arrival instead is the Buggy twin.	0,1,2,7,10,12	1
swept-space-cell	contract	mod::swept_space_cell	swept_space_cell(events,i) is Some((row, col0 + r)) iff events[i] is a Sweep followed by its Typed Move, the preceding Typed run's widths exactly fill the sweep (Spaces one cell), and r is the width before the run's last Space; None otherwise.	0,1,3,7	2
rk-engine-idle-to-zero	contract	Engine::{tick,fingerprint,needs_frame_cadence,next_change_deadline,status}; Ribbon::{plan,emit,at_rest,next_change_deadline}	With nothing live every tick writes nothing, fp and fingerprint() are 0, no cadence or deadline is owed and status counts are 0; the ribbon draws nothing and rests.	0,1,2,3,4,7	3
engine-disengaged-inert	contract	Engine::{on_event,tick,celebrate,earn_hero,translate_scroll,pet_offer}	A disengaged engine changes no observable state at any seam, writes nothing on tick, owes nothing and offers nothing.	0,1,2,4	1
engine-fresh-state	contract	Engine::{new,default}	default() equals new(): disengaged, full glint budget, at rest.	0,1	1
echo-band-fence	refinement	Engine::translate_band; EchoLedger; echo_band_fence_model (test module)	A valid band transform retires every pending press credit, keeps earned momentum, moves the mirror and fields with their text; an invalid one changes nothing; bound to EchoBandFence with the stale-ledger negative control.	0,1,2,4,10,12	1
echo-ledger-refinement	refinement	Engine::{on_event,echo_bridge,lay_bridge,observe_caret}; EchoLedger::{bank,expire,spend,clear,forget,partition}; aterm_models_effects::echo_ledger_bridge_model	The echo ledger refines EchoLedgerBridge: a press never pre-draws, one press pays one cell, only presses older than the licensing sweep pay left of it, presses expire after ECHO_PATIENCE_S, navigation, row changes, reset, erase, kill and focus loss forget them, unswept moves are paid from presses or refused whole, inserts pay late holes, and the ledger holds at least 43 presses.	0,1,2,4,5,7,10,12	16
held-key-and-sweep	contract	Engine::replay_events; Engine::on_event (Sweep); Ribbon::{sweep,hold_typed}	A Typed event with no licensed move after it in its frame lays nothing; its cell arrives with its echo, from the host's Sweep or from ledger presses, born no earlier than the birth floor.	0,1,2,4,7	4
caret-mirror-observation	contract	Engine::{observe_caret,tick,caret_mirror}	observe_caret never seeds the mirror before the first licensed move; a same-row observation never moves it; a cross-row one moves it after the next tick's ingest and forgets the presses.	0,1,2,4	3
engine-rewrite	contract	Engine::on_event (Rewrite); Ribbon::retract_suffix	A Rewrite keeps the spine and ledger, moves the mirror to its column, offers no impulse or cue, births no star, retracts the suffix, and the next echo lays at the new caret.	0,1,2,4	1
witness-rows	contract	Engine::{ribbon_rows_for,band_rows,follow_rows}	ribbon_rows_for names at most WITNESS_ROWS band rows (the hand's run first with its neighbours) then the arming rows when room allows, never a row two away, keeping the hand's lower follow row.	0,1,2,7,13	3
witness-withheld	contract	Engine::follow_rows	withheld lists, in order, the asked rows inside the grid and the focused pane that were not delivered.	0,1,2,7	1
witness-retire-engine	contract	Engine::{witness_rows,reset}; Ribbon::retire_cells	A cell retires only on a glyph that changed against its armed record, counted in status().retired; a payout beside it lays a fresh cell without reviving it; reset keeps the count and clears the records.	0,1,2,4,7	1
short-wrap-park-refinement	refinement	Engine::follow_rows_with_park; aterm_models_effects::rainbow_short_wrap_park_model	follow_rows_with_park reports proved only when the moved run belongs to the caret's own short retreat, matching the model's FollowExact or FollowOther.	0,1,2,10,12	1
engine-sound-cues	contract	Engine::{tick,on_spawn,drain_sound_cues,set_engaged}; mod::sub_floor_hop	Cues are minted only by tick into the frame: one Meteor cue per licensed jump on the flight clock, one Enter per Return move, Jump for a Pty row change, one Navigation cue per keyed sub-floor hop priced at the frame's momentum; no-ops, Pty hops and typed moves are silent; disengaging drops unticked events.	0,1,2,4,7	4
engine-companion-impulse	contract	Engine::{offer,take_companion_impulse,tick}	Erase and Kill offer Wince, a Meteor outranks it, take transfers it once; under reduced motion a credited spawn offers Land with paint 1 and no flare.	0,1,2	2
flow-draws-nothing	equivalence	Engine::{tick,fingerprint,needs_frame_cadence,next_change_deadline}	Flow only re-prices births: once the glass is dark an open-theme engine writes nothing and agrees on cadence, deadline and fingerprint with a twin whose combo stayed shut.	0,1,2,4,7	1
exit-sampling-refinement	refinement	Engine::tick; Ribbon::advance_swoosh; aterm_models_effects::rainbow_exit_sampling_model	The exit swoosh is a function of time: one late tick after any silence sees the settled state (ObserveDone); restarting unobserved swooshes is the Buggy twin.	0,1,2,10,12	1
mend-pricing	contract	Engine::tick; Ctx::mend; spine::Spine::mend; Ribbon::lay_cell	A key during a live mend is born at max(birth_disp, mark.disp), so a quick typo fix keeps the interrupted momentum; a slow correction is not a mend; the mark is a value that owes no wake.	0,1,2,3,4,6	3
caret-seam-flare-and-paint	contract	Engine::{tick,caret_paint}	flare_at is the spawn instant within SPRING_SNAP_RESPONSE_S and None after; paint = max(momentum_display, the landing relight), which is 1 during the snap, halves one constant later and is 0 past RELIGHT_LIFE_S; with no landing paint equals the display; a Pty move gives no flare or paint.	0,1,2,3,6	2
glint-budget	contract	Engine::earn_hero; StarBudget; Engine::tick	earn_hero over a probed-blank sky always births one M1 and offers Delight; a glint cue is minted iff a token is available.	0,1,2,3,6	1
party-seam	contract	Engine::{party,tick,reset}	party mints its fan on the next tick iff engaged and the caret has been observed since reset; reset drops a banked bar.	0,1,2	1
sky-probe-gate	contract	Engine::earn_hero; Stardust::sow; GlyphProbe::at	A sky star is born only over a probed-blank cell; an unprobed sky bears no star and no glint.	0,1,2	1
engine-translate	contract	Engine::{translate_band,translate_scroll}; Ribbon::{translate_band,translate_scroll}	A band move moves the caret, erased cell, owed heroes and fields with their band (light carried off is dropped, the caret saturates), keeps the spine and touches other bands not at all; a scroll keeps every star and quad moved up exactly one row height.	0,1,2,4,7	2
curtain-and-hide	contract	Engine::{curtain,hide_next_frame,hide_armed,reset,tick}	hide_next_frame blanks exactly the next tick while keeping every cell; reset or curtain drops an untaken hide; a curtain forgets the caret and ledger, keeps the ribbon easing out to nothing by CURTAIN_S, and lays nothing under it.	0,1,2,4	2
status-drawn-honesty	contract	Engine::{ribbon_drawn,ribbon_segments,curtain_left_ms}; ribbon::reads_as_ink	On every frame of a falling curtain, ink on glass implies ribbon_drawn > 0 and a live curtain clock, and a dropped segment count while ink remains implies the curtain is still falling.	0,1,2,12	1
pet-offer	contract	Engine::{pet_offer,catch_star}; companion::impulse_for	pet_offer names the meteor's arrival as the perk instant (not under reduced motion), offers a catch only for a gold M1 near a settled purring cat on its row, which catch_star finishes without minting anything, and offers the mote colour at the cat's column; a disengaged engine offers nothing.	0,1,2,3,4,6	3
frame-liveness-fingerprint	contract	mod::liveness; Engine::{tick,fingerprint}	liveness is 0 iff all three slices are empty, else 1; a tick folds only its own appended marks and leaves earlier scratch untouched.	0,1,4	1
config-from-glow	contract	mod::Config::from_glow	The field-wise projection of the glow config plus the reduced-motion flag.	0,1	1
engine-cadence-tail	contract	Engine::{next_change_deadline,needs_frame_cadence,tick}	Every deadline is at least ARM_MIN away; during the tail wakes are at least TAIL_FLOOR apart (at most 25 per second); prose settles to no deadline within 2.5 s of the last key.	0,1,2,4,5,7	1
cadence-fold	contract	mod::Cadence::{at,edge,tail,tail_at,brisk,take}	edge keeps the earliest future instant; tail offers now + max(s, TAIL_FLOOR); brisk forces now + FRAME_CADENCE; take returns the result.	0,1,2,3,6	1
pane-fold	contract	Engine::set_pane; Ribbon::{set_pane,lay,fold_pane}	With a pane set, a typed fold lays at the pane's last column and nothing outside the pane; without one it walks the grid.	0,1,2,3,7	1
pre-move-caret	contract	Engine::{record_pre_move,translate_pre_move,translate_events,reset,curtain}	A key buffered before a frame's first one-shot move replays at the caret recorded before that move, translated with scrolls and band moves (forgotten if carried off), unaffected by inserted sweeps, and dropped by reset or curtain.	0,1,2,7,13	4
bed-luma-budget-def	contract	ribbon::{bed_luma_budget,bed_ink,solve_for_luma}	bed_luma_budget(fg) = clamp((Y(fg) + 0.05)/(BAR + GUARD) - 0.05, BED_LUMA_MIN, BED_LUMA_MAX), a function of the foreground alone; bed_ink solves each stop onto that luminance from below; evaluated corollaries pin the default theme's composites.	0,3,6,7	3
bed-legibility-bar	table-property	ribbon::{bed_ink,bed_luma_budget}; aterm_render::{over_premul,premul_rgb}	For every dark theme above the floor, every arc position and coverage up to BODY_FRAME_TOP, text over the composited bed keeps contrast >= BODY_CONTRAST_BAR, within 0.10 of it on the default and Nord themes.	0,3,5,6,7,12	2
bed-ink-even-and-chromatic	table-property	ribbon::bed_ink; spectrum::spectrum_with_min_saturation	The bed's stops composite at one weight (luminance spread <= 0.004), at full chroma, with no grey dip at the green-blue crossing, and every stop clears the scanner floor on Nord.	0,3,5,6,7,12	4
rail-ink-band	table-property	ribbon::rail_ink	Each anchor's rail ink lies in [RAIL_LUMA_FLOOR, RAIL_LUMA_CEIL], carried pure inside the band, scaled through its hue above, lifted below.	0,3,6,7	1
walk-pace-perceptual	table-property	ribbon::{walk_pace,walk_arc,CROSS_PACE}	walk_pace is monotone, the identity outside the crossing window, and CROSS_PACE equals its re-derivation by equal composited steps; it changes only the crossing leg and bounds the worst bed and rail steps.	0,3,5,6,7	2
ember-seen-floor	table-property	ribbon::{seen_floor,cov_cap}; BedInkLut::{sync,at,seen_floor_at}	On every shipped ground, every stop visible at the cap stays visible at its ember floor, and floors spread at least 3x below UNDER_COV_CAP.	0,1,2,3,5,6,7,12	2
walk-t-def	contract	ribbon::walk_t	walk_t(d) = d/WALK_FAST_CELLS up to WALK_FAST_CELLS, then 1 + (d - WALK_FAST_CELLS) WALK_LAY_RATE; non-finite gives 0.	0,3,6	1
cohort-walk-invariant	invariant	Ribbon::{on_event,plan,lay_cell,place,shift_run,heal_seams,seat_fold_successor,relay_word,join_cohort,mint_cohort}; Cohort::{t_at,walk_d,walk_zero}; ribbon::walk_t	In every reachable ribbon every live cell's t equals its cohort's t_at(col) = walk_zero + walk_t(walk_d(col)) bitwise, and shift_run never moves anchor_col; cold walks start at zero; walk_origin_at agrees with field_at bitwise; a retyped cell takes back its stop. Corollaries: adjacent lit cells of one line step exactly one walk step, keys at a line's start take 0, 1/16, 2/16, and no column lit before and after an insert changes colour.	0,1,2,3,5,6,7	10
walk-same-row-origin	contract	Ribbon::{join_cohort,mint_cohort,wake_origin,lay_reach_cell,heal_seams,seat_fold_successor,revive_sweep}	One row holds one walk: a cohort minted beside a lit one takes its walk by position, reach cells continue their own cohort, wakes share the band's origin, heal merges and fold successors re-seat onto the walk, and a named revive lays a retired run back on its own walk.	0,1,2,3,5,6,7	8
walk-chain-and-fold-distance	contract	Ribbon::{join_cohort,retire,leave_row,fresh_line}	A key within CHAIN_GAP_MAX of a finished swoosh continues its walk; past it the walk starts cold; folds, completions and Pty row changes carry the walk distance across rows; a short line finishes its fast leg first.	0,1,2,3,6,7	5
walk-new-line-gate	contract	Ribbon::{arm_fresh_line,lay_cell,hold_typed,curtain}; FreshLine::takes; Engine::replay_events	A new line (Return, newline, keyed Nav to another row, line kill, curtain) arms a gate dated at its earliest instant; the first typed cell it takes mints a cohort continuing the colour one step past the old line on the fast leg; keys re-laid on the old row and the old line's late glyph do not spend it; a Pty move arms nothing.	0,1,2,3,5,6,7	9
fresh-line-takes	contract	ribbon::FreshLine::takes	takes(row,at) iff not awaiting the move, at is not the chord's own key, and at >= since or the key lands off the row the hand left.	0,1,3	1
composer-newline-gate-refinement	refinement	Ribbon::{on_event,curtain}; aterm_models_effects::rainbow_composer_newline_gate_model	fresh_line follows RainbowComposerNewlineGate: a composer key arms it awaiting the home, type-ahead does not spend it, the home clears the wait, the first real echo spends it, a curtain or a cancelling edit resolves it.	0,1,2,10,12	1
field-index-is-newest-scan	equivalence	ribbon::FieldIndex::{begin,set,at,translate,translate_band}; Ribbon::{field_at,build}	After every plan field_at(row,col) is Some iff a resident cell is there and then equals the newest such cell's t, through shadowing, rows going dark and relit.	0,1,2,3,7	1
cohort-phase-timeline	contract	ribbon::{Phase::at,swoosh_total_s,retract_start_s}; Ribbon::{retire,advance_swoosh,note_key}	A cohort runs Laying/Grace through grace_s = max(PHRASE_REST_MIN_S, 2.5 x the cadence), then Reaching, Retracting, Fading, and leaves at swoosh_total_s(grace); cells under a typing hand stay resident; the mark is off glass one swoosh after the last key.	0,1,2,3,6,7	3
swoosh-monotone-retract	contract	Ribbon::{plan,emit,retract_x,wipe_of,env_of,emit_hot_edge}	After the last key the body never brightens, the retract moves only toward the caret draining the tail first and never past the caret block, the hot edge only falls to exactly 0, and everything is empty at the swoosh's end; reduced motion only fades.	0,1,2,3,5,6,7	5
row-hold-own-clock	contract	Ribbon::{place,hold,hold_phrase,take_row_back,open_phrase,phrase_for_lay}	Only the owning row and phrase renew: a wrapped paragraph is one phrase whose rows keep their own clocks, a closed phrase is not renewed elsewhere, erases hold the row's cohort with its left end lit, an erase after the rest snaps the band back, and a typed cohort keeps its bounds.	0,1,2,3,5,6,7	6
erase-retract-suffix	contract	Ribbon::{erase,retract_suffix,on_event,place}; ribbon::kill_span_s	An Erase retracts exactly the row's cells at or right of the caret; a late echo retracts the erased cell alone; a Rewrite retracts its suffix farthest-first inside kill_span_s; a run backspaced whole or killed and retyped inside its fade rejoins on its own stops.	0,1,2,3,5,6,7	4
abandon-retract	contract	Ribbon::{abandon,abandon_row,retract_target,target_toward}	A move that leaves the hand's run abandons its cohort without a light step and drains it farthest-first in RETRACT_DUR_S + RETRACT_FADE_S, toward the landing only on the landing's own row, each other row toward its own end; a program round trip leaves it rejoinable by the hand. On a glass frame (§15, Prism Glass 1.8) the drain takes the glass's height down by the same clock, its opacity whole, to nothing; under reduced motion it is the opacity's fade, the height standing.	0,1,2,3,5,6,7	5
wake-corridor	contract	Ribbon::{wake,corridor_column,own_wake_cohort,join_wake_cohort}; timing::flight_ms	A keyed Nav jump that leaves the run lays a wake of WAKE_MAX_CELLS columns back from the landing (plus any farther kept light), born along the flight clock and living WAKE_LIFE_S, never typing, one per row, contiguous, bounded under ping-pong; row changes, Pty and typed moves lay none; a hop lays a short wake on the word's walk.	0,1,2,3,5,6,7	8
wake-overlay-no-touch	contract	Ribbon::{wake,env_of}; Cell::rearm	A wake leaves every base cell and its clock unchanged and repaints no seen stop; an overlay cell over lit light is re-armed at that light's level, over nothing it is cold-priced; the band's light never steps down at the jump; over a leaving band the corridor's width does not depend on how far it drained.	0,1,2,3,5,6,7	3
scrub-holds-and-damages-nothing	contract	Ribbon::{scrub,stands}; Cohort::leaving_at	A Nav move over standing light end to end is a scrub: it abandons, lays and leaves nothing, keeps the census byte-identical, holds only the corridor's owners, never raises a column more than 2 levels and keeps the soft end; a move over melting or leaving light is not a scrub and raises nothing.	0,1,2,3,5,6,7	5
reach-hand-floor	contract	Ribbon::{lay_reach_cell,hand_floor_on,touch_hand}	No reach cell is laid left of the first column the hand held on its row.	0,1,2,7	1
band-geometry	contract	Ribbon::{band,body_profile,plan_run,glass_planned}; Band::contains; Segment::{glass_up,glass_jump}	Every cell's band stays within TALL_UP_CH above and its dn below; the wave swings at least 0.10 ch at full momentum; the comet is fattest at the head; tall and underline price light the same, the underline's band the thinner (above the spine, and below it at the hand) and kept to a last or held row's foot; a laid cell publishes a Band containing its spine. A frame the bed draws by its glass (§14) plans main's geometry, so band() is main's for every cell; the glass's reach rides in Segment::glass_up and its hard steps in Segment::glass_jump, both 0 on every other frame.	0,1,2,3,5,6,7	4
hot-edge-gain-and-cells	contract	ribbon::edge_cells; Ribbon::hot_edge_gain	edge_cells is the lerp from HOT_EDGE_CELLS to HOT_EDGE_CELLS_MAX in max(surge, heat); hot_edge_gain is 0 off a dark theme or under reduced motion, else a floor for typed heads plus a smoothstep of disp.	0,1,3,6	1
hot-edge-emission	contract	Ribbon::{emit_hot_edge,head_col}; ribbon::hot_edge_ink	The hot edge is additive only, never past the head cell nor more than edge_cells behind, within the caret band, capped at HOT_EDGE_COV_MAX, the head stop lifted brighter than the bed; it grows with surge and flow, sits at a leftward wake's landing, is absent on a cold arrow's wake, and embers out with its body.	0,1,2,3,5,6,7,12	6
attack-and-boundary	contract	Ribbon::{plan_run,wipe_of,place,lay_cell}; ribbon::GlassTail	A new cell's light enters from the caret side (the flat spelling fades in place); a boundary takes the brighter side's coverage, so neighbours never sag; a key re-laid on a lit cell keeps the old attack. On a glass frame (§15, Prism Glass 1.8) the attack is the glass's opacity (the edge-in and its wipe), a boundary takes the brighter side's opacity with no RUN_TAIL_EASE, the run's oldest end eases in by height instead (GlassTail), and a key re-laid on a lit cell relights from its level in the glass's height.	0,1,2,3,5,6,7	3
light-theme-and-spellings	contract	Ribbon::{emit,head_rgb,comet,rail_lit}; ribbon::light_role	On light themes the ribbon is source-over ink and never additive, within the Leading cap on main's paper arm and whole (over that cap, at most 255) under the glass (§14); head_rgb is None before a caret, then the authored stop on dark and its ink on light; the flat spelling draws no comet or rail and stays legible.	0,1,2,3,6	3
body-ceiling-rail-and-pane	contract	Ribbon::{emit,pane_bot}; BedInkLut::rail_at	A hot body rides near its ceiling without passing BODY_FRAME_TOP; the rail shows its anchor below the row while text stays at the bar; nothing is drawn below the focused pane's last row.	0,1,2,3,5,6,7,12	3
ribbon-quad-budget	invariant	Ribbon::{emit,slabs_for,repaint_capped_run_from_head}; Ribbon::{glass_slabs_for,glass_slabs_per_cell}; ribbon::{glass_slab_rows,glass_tail_wedge_lights,glass_frame_price}	emit stays within RIBBON_QUAD_BUDGET, keeps the hand's row whole, gives up density first and then sheds the oldest row's distant tail contiguously; below the cap it equals the per-run concatenation. On a glass frame (§15, Prism Glass 1.8) the plan keeps main's density and the glass lays the densest whole number of slabs to each of its vertices that its price allows (Ribbon::glass_slabs_for over glass_frame_price: the glass's rows and the lip's reserved rows a slab, glass_slab_rows, and each row's open oldest end's wedge rows, glass_tail_wedge_lights, whose runs ride no ramp under RAMP_COST and are each charged whole against RIBBON_QUAD_BUDGET); its frame lays no more lights than that price less the lip's reserve, and a dark frame chooses main's density.	0,1,2,3,6,7	3
ribbon-frame-determinism	equivalence	Ribbon::{on_event,plan,emit}	Two ribbons fed the same events produce identical frames, and re-planning at the same instant gives the same frame.	0,1,2,5,7	1
ribbon-translate	contract	Ribbon::{translate_band,translate_scroll,re_anchor,translate_run,carry_glass_wrap,lift_hand}; FieldIndex::{translate,translate_band}	A band move carries cells in the band with their clocks, drops what leaves and retires empty cohorts, and moves fields, the floor, the seam and the hand row; a scroll moves every row up with clocks kept and the frame shifted exactly; a re-anchor on the scroll's own tick drains nothing. The wrap's light-only side table (GlassWrap, §17) rides the same moves, touched only when it holds a record: a scroll and a band move take each record with its row and its left_at, and a row scrolled off the screen or moved out of its band drops its record; a carry (translate_run) takes a whole run's record to its new row, gives a split's carried part a copy under its own cohort and leaves the part behind its own, and drops the record carried off the pane; the hand's lift maps a record on the hand's row with the hand.	0,1,2,3,7	9
level-step-closed-form	contract	mod::level_step_spend; ribbon::level_step_melt; timing::spend; ribbon::expiry_melt	For finite positive inputs the functions return None iff the current level rounds below 1, else the exact time until the rounded level drops (within 2e-4 s).	0,3,5,6	1
retire-and-release-identity	contract	Ribbon::{retire_cells,release_cells,restore_releases,relay_word,place}; witness::Witness::walk	retire_cells stamps exactly the named live cells, which melt monotonically to nothing within 150 ms without disturbing neighbours; release_cells abandons named cells' cohort into a retract once; restore_releases lifts a release by its stamp; a re-lay over a retired cell gets a fresh identity, and a stale id has no authority.	0,1,2,3,4,5,7	6
focus-ember	contract	Ribbon::{on_event (Focus),ember_floor,regain_focus}; ribbon::ember_level; ribbon::{GlassEnv,Envelope::ember,glass_height_floor,BedInkLut::main_seen_floor_at}	Focus loss embers every cell over FOCUS_EMBER_S at its own seen floor, so the painted set is all or nothing; regaining focus comes back through edge-in; a key under a burning ember relights from the level it had. On a glass frame (§15, Prism Glass 1.8) the ember takes the glass's height to the least it draws (GLASS_MIN_PX, with EMBER_FLOOR_MARGIN), its opacity whole, so the painted set stays all or nothing, and under reduced motion too (Envelope::ember: the ember and its relight move the glass's height), so an unfocused window never holds a translucent pastel glass; main's envelope, and every decision read from it, keeps main's seen floor (BedInkLut::main_seen_floor_at) on a glass frame too.	0,1,2,3,5,6,7	3
curtain-law	contract	Ribbon::{curtain,curtained,lit_segments,ink_segments,curtain_hand}	A curtain lays nothing while it falls, its light never rises, it gathers toward the band's own head without a cliff over at least CURTAIN_MIN_FRAMES, ends at exactly zero by CURTAIN_S, keeps letters legible, and arms the new-line gate on an empty ribbon. On a glass frame (§15, Prism Glass 1.8) the curtain gathers the glass's height, its opacity whole, and the height ends at exactly zero by CURTAIN_S.	0,1,2,3,5,6,7,12	2
typed-landing-pen	contract	Ribbon::{typed_landing,fold_top_row,hand_changed_row,on_event,re_anchor}	A typed key lands at the reported caret unless the row the lay would write is neither the landing's nor the pen's row, when it is re-homed to the pen (the caret where the hand last typed, erased or navigated); a repaint's home does not abandon the hand's row; a short backward echo below RE_ANCHOR_MIN_CELLS stamps nothing.	0,1,2,3,7	12
word-relay	contract	Ribbon::{relay_word,moved_word_len,word_space,lift_move,begin_relocate,settle_relocate}; Engine::replay_events	A composer re-wrap that moves the last word down relays its cells on the new row from the inset on the walk it had, measured from the hand's last Space or arrival, sending the old cells to retract; a shell home relays nothing; a lifted word goes up with its light; relayed letters join the run their first letter minted.	0,1,2,3,5,6,7	7
seam-heal-no-slivers	contract	Ribbon::{heal_seams,plan_run,build_runs}	A coalesced echo leaves one cohort per row with no interior feather; shared boundaries carry the older side's light; edges step at most a device row; no column falls more than FADE_STEP_MAX per frame while a neighbour retires.	0,1,2,3,5,6,7,12	3
caret-attach	contract	Ribbon::{plan_run,attach_floor_at,build}	Under a live caret every device column from the head cell to the caret block is lit near the plateau with the block standing on the head's light, after jumps and scrubs too, and stays lit with bounded fall while the cell under it leaves.	0,1,2,3,5,6,7,12	4
fold-flow	contract	Ribbon::{leave_row,flow_row,flow_cohort,flow_target,retract_x,translate_run,shift_run,hold,take_row_back}	A typed fold or carry does not abandon the row it left: the cohort flows into its frozen kept end (rightmost above the caret, left end below), its light never rises and falls smoothly, nothing passes the kept end, it goes dark within FLOW_TOTAL_S whatever the pause; reduced motion holds then fades once; a Backspace back through the fold re-wets and holds the row.	0,1,2,3,5,6,7	10
seam-hairline	contract	Ribbon::{emit_hot_edge,hand_floor_on,soft_wrap_move,reflow_move}	After a continuation the new row's seam light starts at the hand's floor, fades by FLOW_TOTAL_S, and is absent after a Return or under reduced motion.	0,1,2,3,5,6,7	1
sound-gain-laws	contract	sound_policy::{trail_sound_gain,sing_riff_gain,bonk_sound_gain}	trail_sound_gain(f,c,v) = Some(v) iff f and c and v > 0 (NaN gives None); sing_riff_gain adds the riff switch; bonk_sound_gain is None under reduced motion and otherwise adds the bonk switch; flipping any term off gives None.	0,1,3,4	3
flight-clock	contract	rainbow_kitty::timing::{flight_ms,flight}	flight_ms(c) = 60 for non-finite or non-positive c, else clamp(50 + c, 60, 120): in [60,120] and monotone; flight is its Duration.	0,3,5,6	1
impact-curve	contract	rainbow_kitty::timing::impact	impact(c) = 1 for non-finite or non-positive c, else clamp((c/8)^0.7, 1, 3.5): monotone, 1 at 8, 3.5 from about 47.9 cells.	0,3,5,6,14	1
phrase-rest-and-tempo	contract	timing::{phrase_rest_ms,phrase_rest_reached,ioi_next}	phrase_rest_ms(ioi) = max(2.5 clamp(ioi,30,600), 900) (the 250 ms value for non-finite input), in [900,1500]; phrase_rest_reached compares the gap to it; ioi_next resets to 250 on a first key or a gap >= 2 s, keeps ioi at a rest, else averages with the clamped gap.	0,3,6	1
shed-and-rain-counts	contract	timing::{shed_n,rain_spacing_ms}	shed_n(c) = clamp(c/12, 3, 6) (3 for bad input); rain_spacing_ms(T) = clamp(T/5, 18, 24) (18 for non-finite), so over every flight the spacing is in [18,24].	0,3,4,6	2
named-easing-curves	contract	timing::{clamp01,smoothstep01,edge_in,enter_at_speed,suck_in,spend,half_life,half_life_life_s,spring_snap,spring_whip}	Each curve has its stated closed form, endpoints and monotonicity; half_life halves each tau past its hold and half_life_life_s reaches STAR_CULL_ALPHA; spring_snap stays in [0,1]; spring_whip overshoots within its exponential envelope.	0,3,5,6,14	1
tone-asset-decoder-fail-closed	contract	tone::{raw,ToneModel::from_bytes,fnv1a32}	from_bytes returns Truncated, BadLength, BadMagic, BadChecksum, DimensionMismatch or BadScale in that order of checks and never panics; otherwise it dequantizes the weights. Any payload byte flip gives BadChecksum and any magic flip BadMagic (FNV-1a steps are bijections).	0,1,3,5,7,13	1
tone-encode-decode-round-trip	round-trip	tone::{encode_model,ToneModel::from_bytes}	Decoding an encoded model of the compiled shape is Ok, has ASSET_LEN bytes, recovers each quantized tensor within half its scale step and the biases bit for bit.	0,1,3,5,7	1
tone-shipped-asset-valid	table-property	tone::{ToneModel::from_bytes,builtin}	The embedded tone asset decodes, so builtin() is always Some.	0,1,7,13	1
tone-ngram-featurization	contract	tone::for_each_ngram_bucket	With whitespace runs collapsed and ASCII lowercased into k chars, it reports 0, 1 or 3k - 3 n-grams, every bucket < BUCKETS, invariant under case and whitespace length, whatever the script.	0,1,5,7,13	1
tone-scores-abstain-classify	contract	ToneModel::{scores,classify_opt,classify}	scores abstains (None) below 6 n-grams and is otherwise a deterministic function of (model, text) with non-negative scores; classify_opt takes the lowest maximal index; classify falls back to Technical.	0,1,2,3,5,7,13	3
tone-tracker-model	refinement	tone_infer::ToneTracker::{note_char,drain_pending_inference,note_backspace,note_break,effective,current,window_chars,default}	The tracker refines a model of (buffer <= 160 chars, session, tone, key count, last inference, pending): note_char appends (a session change clears the buffer) and marks inference due after 6 keys or 500 ms; drain runs the classifier only when due and keeps the tone on abstention; Backspace pops, break clears; effective(false) is Technical. So inference is throttled, deletion never classifies, and a session switch keeps the tone.	0,1,2,4,5,10	11
toy-pack-source-cap	contract	spec::compile_toy_pack_toml	A source longer than MAX_TOY_PACK_BYTES is refused with one diagnostic before any parse.	0,1	1
toy-pack-validation	contract	spec::{compile_toy_pack_toml,validate_pack_metadata,valid_id,valid_lang,validate_toy_word,compile_toy_spec,compile_toy_ink,compile_toy_burst,compile_toy_graphic,toy_dispatch_hashes}	compile succeeds iff the strict schema parses and none of the independent diagnostics fires (schema, id, metadata, counts, word rules, duplicate and possessive-collision hashes, kind and bound checks, and finally scanner reachability); every fired clause is reported, naming its field.	0,1,4,5,7,9,12	4
toy-pack-denotation	contract	spec::compile_toy_pack_toml; SpecTable::override_for; CompiledToyPack::lexicon_toml	A compiled pack counts its toys and words, maps every dispatch hash of every word to its compiled spec, and its lexicon fragment scans every word as a whole-word match.	0,1,4,5,7,9,12	1
shipped-packs-compile	table-property	spec::compile_toy_pack_toml	Every checked-in community toy pack (toy-packs/community/*/pack.toml) compiles.	0,1,7,8,9	3
custom-spec-table-keys	contract	SpecTable::{insert_custom,has_custom,override_for}; spec::{build_custom,parse_custom_toml,push_lexicon_entry}; RawCustomEntry::to_spec	insert_custom keys a non-default spec by the word's form hash (and, for spaced words, its folded possessive variants) and touches no other key; to_spec clamps chance and maps burst, graphic and ink kinds; build_custom folds entries and writes an escaped lexicon fragment; parse_custom_toml errs only on malformed TOML.	0,1,2,5,7,9	4
custom-colorway-parse	contract	spec::{custom_colorway_is_valid,parse_two_tone,parse_colorway,parse_hex}	parse_hex accepts exactly 6 hex digits after an optional '#'; parse_two_tone exactly two such fields after "twotone:"; a colorway is valid iff rainbow or a valid two-tone; parse_colorway falls back to rainbow.	0,1,5	1
spec-table-overlay	contract	SpecTable::overlay	After A.overlay(B), override_for(h) is B's answer when B has one, else A's.	0,1,2,5,7	1
spec-consumer-capabilities	contract	SpecTable::consumer_capabilities	Each capability flag is true iff some reachable override spec (not the whole arena) needs it.	0,1,5,7	1
spectrum-lut-is-generated	equivalence	spectrum::{SPECTRUM_LUT,SPECTRUM_ANCHOR_AT,generate_spectrum_table}	The committed LUT and anchor positions equal the spec-only generator's output bit for bit.	0,1,3,5,7,14	1
spectrum-lut-table-properties	table-property	spectrum::{SPECTRUM_LUT,SPECTRUM_ANCHOR_AT,SPECTRUM_ANCHORS,spectrum_hsv,spectrum_with_min_saturation}	Over the 511 entries: anchors sit at their positions, hue climbs without reversal, saturation and chroma stay full, adjacent steps are at most 4 levels, there is no deep luminance dip, and the saturation floor is the identity on every entry.	0,1,3,7,14	6
spectrum-read-and-snap	contract	spectrum::{spectrum,spectrum_stop,spectrum_stop_position,spectrum_snap_index,spectrum_snap,spectrum_max_byte_rate}	spectrum(t) lerps the LUT at clamp(t)*510 (red for t <= 0 or NaN, violet for t >= 1) and equals each anchor at its position; spectrum_snap returns the nearest of the seven named stops and is onto them.	0,1,3,5,7	2
spectrum-min-saturation	contract	spectrum::spectrum_with_min_saturation	Returns the colour unchanged for non-finite s, black, grey or a colour already at the floor; otherwise keeps value and channel order and reaches saturation within 3/255 of the floor.	0,3,5,6	2
supernova-reach	contract	supernova::r_max_for	r_max_for(ch, gh) = max(min(6 max(ch,1), max(gh,ch)/2), 1).	0,3,6	1
supernova-tier-ladder	contract	supernova::{tier_for,tier_of,combo_chance,total_ms}	tier_for picks Flash, Nova or Nuke by the draw's bucket against the level's window row (levels 0 and 1 equal tier_of, level >= 5 is always Nuke), so Flash shrinks and Nuke grows with level; combo_chance adds 30 per level up to 100; total_ms orders the tiers.	0,1,3,5	4
additive-peak-exact	equivalence	supernova::{peak_additive_channel,bound_additive_overlap,scale_rgb_floor}	peak_additive_channel equals the exhaustive per-pixel maximum of summed additive coverage; bound_additive_overlap scales every quad by one factor so the peak is at most 64 when it was higher, keeping order.	0,1,2,5,7,13	2
shade-peak-exact	equivalence	supernova::{peak_shade_probe,peak_shade_quadratic,bound_shade_opacity,is_shade}	For cell-aligned Shade stamps the probe peak equals the quadratic reference (per-cell sum of Over Shade alphas), and bound_shade_opacity equals the reference bound for every slice, leaving every cell's sum at most 64.	0,1,2,5,7	1
emit-super-quads	contract	supernova::{emit_super,QuadSink::push,emit_crown_quads,emit_ring_chords,charge_mote_pos}	emit_super is pure, appends at most min(budget, 1024) quads (with S_MAX_BOUND), each single-row, with peak additive coverage at most 64, and its charge, wash, crown and ring phases appear only in their windows and themes, with the wash centred on the blast row.	0,1,2,3,4,5,7	5
emit-super-decos	contract	supernova::{emit_super_decos,bound_shade_opacity}	emit_super_decos appends at most cap decorations purely, keeps each cell's Shade sum at most 64, emits only additive debris on dark themes and bounded Over motes and veils on light ones, debris only in [1200, 2400).	0,1,2,3,5,7	2
wit-unit-at	contract	rainbow_kitty::witness::{unit_at,Unit::is_blank}	unit_at is blank past the row, resolves a '\0' continuation to its lead glyph as a wide unit, and otherwise reads the glyph and whether a continuation follows.	0,1,3	1
wit-evidence	contract	witness::{Seen::observe,Seen::arrivals,Witness::arm,Witness::walk}	A record armed by a walk starts all-clear and observes its arming frame; per follow offset a changed unit marks clear, an equal unit at arm or stood for TWIN_MIN marks twin (sticky), unsampled offsets change nothing; arrivals = clear and not twin.	0,1,2,3,5,7	4
wit-evidence-carriage	contract	witness::Witness::{translate,translate_band,translate_cells,shift_cells}; witness::band_keeps	A scroll keeps evidence and dates scrolled-in rows from the last walk; a band move keeps only evidence that moved with the record and drops what leaves, keeping the list sorted; carried or shifted records reset their evidence and stay unique; the last walk's blanks ride along.	0,1,2,5,7	7
wit-follow-runs	contract	witness::Witness::{follow_runs,follow_runs_with_short_park,follow_verdict,relaid_suffix}	A run is followed to the first sampled offset where at least half its armed glyphs are gone and enough arrived as one block (twins neutral, a relaid suffix discounted, a single glyph only under the exact short park); the block grows over flush riders; runs that arrived around a hole count as missed.	0,1,2,4,5,7	11
wit-shift-runs	contract	witness::Witness::shift_runs; witness::{find_shift,shift_block}	shift_runs names an insert's pushed tail and width iff the run's glyphs from the edit point stand in order shifted right by 1..=SHIFT_MAX_COLS (one glyph only under the caret); rewrites and clears name nothing; after re-keying, the next walk finds every cell.	0,1,2,5,7	6
wit-walk-cell-verdict	contract	witness::Witness::{walk,find,arm}	Per resident cell: an unchanged unit is untouched, a new glyph arms, a changed glyph retires the cell (both halves of a wide glyph), a blanked glyph is decided per run; records are per identity, and cells on unsampled rows keep their records.	0,1,2,5,7	5
wit-walk-run-clock	contract	witness::Witness::{walk,find_standing_runs}	A run with a replaced glyph retires its blanked records; a run with nothing standing whose text of at least 2 glyphs stands elsewhere moved and retires; otherwise blanked records are released (single glyphs and fragments are never ruled moved).	0,1,2,5,7	5
wit-walk-blanks	contract	witness::Witness::{walk,released_span_takes,landed_goes_with_its_run}	Never-armed blank cells go with a decided run by the span-plus-tail rule (never a blank between standing letters), and a glyph landing on such a blank in the same walk goes with the run unless the run's standing text lies beyond it.	0,1,2,5,7	7
wit-released-lifecycle	contract	witness::Witness::{walk,hold_released,restored_runs,restored}	A released record stays while its cell is resident; a glyph landing under it retires it and its held mates, each counted once; restored_runs names a released run whose identities all stand again with their ink, refusing later-born retypes; after restore only changed glyphs are named.	0,1,2,5,7	4
wit-withheld-deferral	contract	witness::Witness::{walk,find_deferred_runs}	A run beside a withheld row waits exactly one walk; the deferral scratch never grows past its reserved capacity.	0,1,2,5,7	2
wit-standing-run-index	equivalence	witness::Witness::find_standing_runs	standing_runs is exactly the sorted set of retired runs that still have an unretired, non-leaving recorded cell.	0,1,2,5,7	1
wit-record-list-invariant	invariant	witness::Witness::{new,walk,len}	After every operation the record list is strictly sorted by identity, at most WITNESS_CAP long, and keeps its reserved capacity.	0,1,2,5,7	1
sd-pool-cap	invariant	stardust::Stardust::{sow,make_room}; Finish::gain	Every newborn is admitted; live unfinished stars stay at most STAR_CAP (the oldest is finished over 40 ms) and the pool at most STAR_POOL_MAX.	0,1,2,3,5,7	2
sd-paint-recipes	contract	stardust::{Paint::of,Paint::draw_add,Paint::draw_m3,Stardust::emit}; effect_util::push_twinkle_star	Each class and lane peaks at its published centre luminance as a peak, not a blob; grains and m2 wear their tint to the core while the hero stays white with four tinted stubs; on light themes every mark is source-over within its caps.	0,1,2,3,4,5,7	3
sd-chroma-fixed	invariant	Stardust::emit; stardust::halo_rgb; Star::dead	A star fades by alpha at its snapped stop (halos within its swing of the stop), halos cull under CHROMA_CULL_ALPHA, and it is gone from life + 1 ms.	0,1,3,4,5,7	2
sd-scintillation	contract	Star::{twinkle,lum_phase,is_warm,glinting,in_hold,envelope}; stardust::arm_px	A star holds its centre and arm through its hold, then scintillates in luminance and size within bounds; warm and cool neighbours are in antiphase; only dealt gold heroes glint; reduced motion freezes both clocks; the trace is a function of (birth, seed, now).	0,1,3,5,6,14	6
sd-solved-deadlines	equivalence	Star::{next_arm_step_s,next_glint_toggle_s,next_stream_step_s}; stardust::next_sine_crossing_s	Each solved deadline equals the infimum of the next change (arm pixel, glint toggle, stream pixel) within 1e-4 s.	0,1,3,5,6,14	3
sd-glyph-probe	contract	GlyphProbe::{probe_row,probe_row_ink,at,sky_at,at_px}; stardust::{cell_ink,rule_stroke_px}	cell_ink classifies the six light rules, space and glyphs; a probed row answers per cell (rules ink only inside their stroke band), an unprobed row answers None; rule_stroke_px matches the renderer's stroke and the sky zones.	0,1,2,3,5	3
sd-birth-gate	invariant	Stardust::{on_event,deal_star,sky_birth_px,sow_mini_fan}	No sky star is born unless its sky row is probed clear for the sky; no core is ever born over a probed glyph pixel.	0,1,2,3,4,5,7	2
sd-settle	contract	Stardust::settle	A thrown star keeps a clear rest, else is nudged within 2 px, else walked back along its throw (fixed stars), else dropped; a boxed-in fan keeps its hero and four m2.	0,1,2,3,5,7	2
sd-headroom-and-frontier	contract	Stardust::{rule_headroom_px,blank_frontier_px}	rule_headroom_px is the minimum headroom under every rule the flight crosses (infinite elsewhere); blank_frontier_px is the reach of the blank run behind, capped at want, and the early-stop walk equals the whole walk.	0,1,3,5,7	2
sd-flight-stays-clear	invariant	Star::pos; Stardust::{on_event,throw}	At every instant of its life a star born in a key run stays over clear pixels of its birth-time probe, behind the ink frontier, inside the pane, and an exhaust grain stays in the caret row's sky.	0,1,3,4,5,7	2
sd-probe-pricing	contract	Paint::of; Stardust::emit	A transient over inked or unknown cells is priced at a third; a sky star's luminance trace does not depend on the probe after birth.	0,1,3,4,5	2
sd-deal-rules	contract	Stardust::{on_event,deal_star,mint_seed,adopt_field}; stardust::{ladder_rung,deal_tint}	The sky is a pure function of its events: capitals and bangs always earn a hero, dealt heroes obey spacing and row caps, spaces deal no strike, the second grain is priced on the honest spine, one sky star per cell and pixel, a hot hand fields every cell, the combo ladder pays at 16, 32 and 64, every mint has a fresh seed, and gold comes only from the gold branch.	0,1,2,3,4,5,7	9
sd-glint-bucket	contract	StarBudget::{refill,try_spend,empty}; Stardust::{deal_star,emit}	The token bucket refills to GLINT_CAP; each M1 born on glass spends a token if it can and then cues exactly one glint at its column; nothing else spends; a clipped hero spends nothing.	0,1,2,3,4	4
sd-envelopes	contract	Star::envelope; stardust::{field_lane_envelope,field_fade_life_s}; Stardust::{on_event,next_change_deadline,at_rest}	A field star rides its cell's envelope (lit on the echo frame, held while its row is re-lit, melting with the swoosh, fading by half-life with no later key), retracts with an erased cell, embers on focus loss; an empty pool emits nothing and owes no deadline.	0,1,2,3,4,5,6	7
sd-throw-kinematics	contract	Star::{pos,rest}; Stardust::{sow_fan,sow_mini_fan}; stardust::{stream_px_per_s,stream_travel_px,stream_unit}	Thrown stars ease out to their reach and pin; sky stars stream back at the hand's cadence and stop by STREAM_END_MS; a stroll streams nothing.	0,1,3,5,6	5
sd-populations	contract	Stardust::{sow_shed,sow_fan,sow_mini_fan,on_event}; stardust::{fan_class,light_keeps}	Each lane sows its published census (shed m3, fan hero and four m2, mini-fan, Backspace and kill debris), fan stars wear the band's stop at their column, and light themes thin populations except the law-fixed slots.	0,1,2,3,4,5,7	4
sd-exhaust	contract	Stardust::{on_event,sow_exhaust}; StarLane::may_chime	A typed glyph after the first throws one exhaust grain iff the spine (or mend) is hot, the hand is not strolling, motion is full and the cells behind are sky-clear; it is an M3 (M2 only on its deal at a hot spine), runs back along the sky and never chimes.	0,1,2,3,4,5,7	5
sd-brisk-cadence	contract	Stardust::brisk; Star::brisk	The sky is brisk exactly while a star is inside its throw or pull window or a veil is in its edge-in.	0,1,2,3,5	1
sd-mend-pull	contract	Stardust::on_event; Star::{pos,stream_px}	A mended key drags exactly one M2 from where the typo was to the fix over MEND_PULL_MS, leaving one sky star at the caret's cell.	0,1,2,3,4,5	1
sd-frame-budgets	contract	Stardust::emit; Paint::draw_m3	emit stays within STARDUST_QUAD_BUDGET and STARDUST_HALO_BUDGET stars with halos; over budget every star keeps its core; a grain's share is spent core first.	0,1,2,5,7	3
sd-caret-light-ceiling	contract	stardust::{sky_ground,price_centre,fit_byte,stacked_centre,settle_under_ceiling,under_ceil}; Stardust::emit	sky_ground is the order-independent additive sum under a pixel; fit_byte is the largest byte satisfying a monotone predicate; price_centre and settle_under_ceiling keep every star's composited pixels under the caret's light ceiling, spending colour first, scaling by one factor and leaving a fitting star unchanged.	0,1,3,4,5,7,11,13	4
sd-sky-index-equivalence	equivalence	SkyIndex::{open,ingest,ground,slot}; stardust::sky_ground	The indexed ground equals the scan at every pixel for every geometry; wide lays spill for one frame; the slot hash is injective over every box of up to 16 cells.	0,1,2,3,5,7,11	4
sd-translate	contract	Stardust::{translate_scroll,translate_band}; cursor_glow::BandPx::point	A scroll moves every star up and drops what leaves, clearing the probe; a band move moves stars by their earning row inside the band, drops what leaves and leaves the rest; ch = 0 drops everything.	0,1,2,3,5,7,13	4
sd-aurora	contract	Stardust::{on_event,emit}; stardust::veil_gain; Veil::envelope	Each laid cell on a sky-clear row deals one veil whose gain grows with momentum, inside row r-1's band and its own column, fading with the field lane by 900 ms and retracting with erased cells.	0,1,2,3,4,5	2
sd-optical-size	contract	stardust::{px_scale,px_at}; Stardust::emit	Every pixel dimension is its 1x figure scaled by ch/CELL_1X_CH.	0,1,3,4	1
sd-catch-point-update	other	Stardust::catch; Engine::{catch_star,pet_offer}	Running the catch seam changes only the caught star's finish (once), leaves the contact frame identical to a twin's, mints nothing, and the sky still idles to zero.	0,1,2,4,5,10	1
sweep-physical-margin-fold	contract	trail_sweep::physical_margin_fold	True iff cols > 0, the caret went down exactly one row from the pane's last column to its first.	0,1,3,5	1
momentum-read-and-decay	contract	TypingMomentum::{value,set_value,reset,default}	value is 0 unstamped, else the stored value decayed by exp(-dt/TAU) and snapped to 0 under SNAP_ZERO; the stored value stays in [0,1]; reading is non-increasing and halves every TAU ln 2.	0,1,2,3,6,12	1
momentum-advance-credit-law	contract	TypingMomentum::advance	advance credits RATE min(gap, CREDIT_CAP_S) over the decayed value (capped at 1) and stamps now, so no pause banks credit and key repeat cannot outrun typing.	0,1,2,3,5,6,12,13	2
momentum-delete-kill-drain	contract	TypingMomentum::{delete,kill}	delete subtracts DELETE_DRAIN and kill twice that from the decayed value (floored at 0) and stamp now; neither ever raises the reading.	0,1,2,3,6,12	2
momentum-crossing-deadlines	contract	TypingMomentum::{low_crossing,reading_low_crossing}	low_crossing(th) is the analytic instant at + TAU ln(value/th) (at itself if already below); reading_low_crossing reaches the actual reading floor, zero snap included, a millisecond later.	0,1,2,3,4,6,12	2
flash-envelope-shape	contract	trick_flash::envelope	The envelope is 0 at both ends, rises strictly over RAMP_MS, holds exactly 1, falls strictly over FADE_MS with per-ms steps under 0.02; a revoke continues from its value and reaches 0 within REVOKE_FADE_MS.	0,1,3,4,5,6,12	1
flash-legibility-guard	contract	trick_flash::{legible_strength,resolve_sat_val}	legible_strength is the largest eighth-step strength at which the tint keeps text contrast at min(MIN_INK_CONTRAST, the original) at every hue, on fg's side of bg; 0 when the theme has no usable contrast.	0,1,3,4,5,6,7,12	1
flash-photosensitivity-bound	other	trick_flash::{envelope,flash_hue}; color_math::{hsv2rgb,relative_luminance}	Every cell's luminance trajectory over the flash has at most FLASH_REVERSALS opposing changes of 0.1, for every theme, strength, hue, position and revoke.	0,1,3,4,5,6,7,12	1
flash-find-whole-word	contract	trick_flash::{find_in_row,find_word,is_word_char,is_strong_rtl}	A candidate spells the word on lead cells as a whole word within reach of the caret; find_in_row prefers the strongest anchor (Exact, then Inside, then Slack, which counts only with the live cursor just past the word), then the rightmost; find_word tries the mirrored spelling for RTL words.	0,1,3,4,5,7,12	6
flash-note-admission	contract	TrickFlash::note; trick_flash::hue_seed	note stores a slot iff a caret is known, the last flash started at least MIN_ANIMATION_MS ago, and the word has 1..=MAX_CHARS visible chars; the slot's hue seed is a function of caret and chars; the debounce outlives the slot.	0,1,2,3,5,7,12,13	3
flash-lifecycle	invariant	TrickFlash::{phase,on_rescan,revoke,emit,clear}; WordDecorations::{note_trick_typed,revoke_trick_flash,trick_flash_phase}	phase is Idle, Pending, Live or Revoked as a function of the slot and now; a matching rescan locates, drops or gives up a slot, a revoke fades a live flash once, unpaintable rescans and emits drop it; on glass the flash emits exactly on [since, since + window).	0,1,2,3,4,5,10,12,13	6
flash-emit-is-the-law	contract	TrickFlash::emit; word_decorations::fold_ink	A live flash in scope merges its cells into ink (kept sorted and unique, other cells untouched) with the envelope law's colours from the current base and folds them into fp; otherwise ink and fp are unchanged.	0,1,2,3,4,5,6,7,12	4
flash-locate-row-rule	contract	trick_flash::{locate,scrolled}	locate tests the row the caret's line moved to; only with no reported scroll and a moved cursor does it search up to SEARCH_UP_ROWS rows above for an exact anchor.	0,1,2,3,4,5,7,12,13	5
flash-follow-rule	contract	trick_flash::{follow,capture}	A located flash survives a rescan iff the width is unchanged and its glyphs still stand on the scrolled row, then moves and recaptures colours; otherwise it ends.	0,1,2,3,4,5,7,12,13	3
flash-base-y-declaration-and-memo	contract	TrickFlash::{declare_base_y,on_rescan,remember_base_y,retain_panes}; WordDecorations::{set_scan_base_y,set_scan_session}	A base-y declaration is spent by the next rescan of its scope; the memo holds at most 16 pane entries updated by cursor-bearing rescans, evicts the oldest, and is pruned with panes; a note reads its pane's or session's entry.	0,1,2,3,5,7,13	3
flash-pane-scope	contract	trick_flash::pane_matches; TrickFlash::{on_rescan,emit,retain_panes}; WordDecorations::is_active	pane_matches is a wildcard match; another pane never paints, collects or extends a flash, a closed pane takes it along, and the scheduler wakes only on the flash's own deadline.	0,1,2,3,4,12	3
flash-typed-never-seen	invariant	WordDecorations::{rescan,tick,note_typed_edit}	Without a note_trick_typed call no flash ever exists, whatever the screen shows.	0,2,4,5,12	1
flash-rescan-twins-equivalent	equivalence	WordDecorations::{rescan,rescan_from_cells_with_geom_at_cursor}; Terminal::cell_frame_into	The cold rescan and the snapshot rescan give identical flash state and frames.	0,1,2,4,5,7,12,13	1
tt-kind-of-table	table-property	typed_tricks::kind_of	kind_of classifies each char as Word, Joiner, Alien, Space, Stop, WideStop or Code by the stated predicate order; every code-adjacent punctuation mark is Code.	0,1,3,5,12	2
tt-back-chars-arith	contract	typed_tricks::back_chars	back_chars = sat_u16(fed <= since_end ? since_end - fed : (fed - since_end) saturating-minus word_chars).	0,3,5	1
tt-fire-licence-safety	invariant	TrickListener::{note_char,note_ime,note_submit,note_backspace,note_word_kill,note_line_reset,note_break,note_no_echo,rekey}	For every lexicon and feed script with a line editor run alongside: nothing panics; a fire or confirm happens only at a boundary of a pure, unpoisoned line whose every word is pet vocabulary; it is addressed iff the pet is named (or on Enter); the fired word stands in the line at back_chars; edits only revoke; a poisoned or impure line keeps no live tentative fire. So code, paths, glued punctuation and ordinary lines never end with a final fire.	0,1,2,3,4,5,7,10,12	9
tt-count-belief	invariant	TrickListener::{note_backspace,pop_cluster,step,line_chars}	On an unpoisoned line the char count is 0 iff the editor's line is empty and the token matches the editor's trailing word; each Backspace removes one cluster, so holding it to the start recovers a fresh pure line.	0,1,2,3,5,7,10,12	4
tt-firing-tiers	contract	TrickListener::{close_token,deliver,edge}	On a judging line a closing token fires tentatively (plain trick), holds until named (address word), confirms or releases held fires (vocative), stays pure (filler), or goes impure (unknown word); letters never fire; folding is the lexicon's and the word is reported as typed; no-space scripts close as one token.	0,1,2,3,4,5,12	12
tt-phrase-law	contract	TrickListener::{close_token,deliver}	A trick word within PHRASE_GAP (inclusive) of the last fire is absorbed unless it follows a Look; past the gap or on a new line it is a new request; so two-word phrases fire once and a gap of PHRASE_GAP + 1 ms fires twice.	0,1,2,3,4,5,12	9
tt-punctuation-park	contract	TrickListener::{edge,push_token_char,deliver_tail}	A sentence stop parks the verdict until the next open edge or Enter; a token glued to a stop goes impure; a stop before any word is code but French spacing after a word is not; a wide stop is a full boundary.	0,1,2,3,4,5,12	10
tt-ime-run-and-fold	contract	TrickListener::note_ime; typed_tricks::fold	note_ime steps each committed char and folds the outcomes into at most one event (a confirm folds into its fire as addressed); a commit's end is not a boundary.	0,1,2,3,5,7	4
tt-submit	contract	TrickListener::note_submit	Enter closes the token or delivers the park, confirms a tentative fire into one addressed fire on a pure line, sets submit_pet_only, and starts a fresh line.	0,1,2,3,4	3
tt-backspace-undo	contract	TrickListener::{note_backspace,end_token}	Backspace pops a live token, restores the snapshot across one boundary (revoking a fire made there), tracks the delimiter window, and leaves a second crossed boundary impure but counted.	0,1,2,3,5	5
tt-word-kill	contract	TrickListener::note_word_kill	A word kill on an alphanumeric open token after whitespace recovers it; one the editors disagree about poisons; on an empty line it is a no-op.	0,1,2,3,7	3
tt-poison-and-reset	contract	TrickListener::{note_break,note_no_echo,note_line_reset,step,poison}	Breaks, no-echo prompts and alien chars poison the line (revoking a live fire); a poisoned line ignores input until Enter; a line reset starts fresh without judging.	0,1,2,3	7
tt-session-dirty-memory	contract	TrickListener::{rekey,forget}	Switching sessions remembers a left session as dirty (MRU, SESSION_MEMORY entries, no duplicates) if its line was non-empty or poisoned, and returns to it poisoned; clean sessions return fresh.	0,1,2,3,5,7	4
tt-token-window	contract	TrickListener::{push_token_char,close_token}; Line::token_is_live	Tokens up to TOKEN_CHARS are looked up; longer ones are only counted and spoil their own line without poisoning; trimming back makes them words again.	0,1,2,3	2
tt-needs-lexicon	contract	TrickListener::{needs_lexicon,ime_needs_lexicon,submit_needs_lexicon,close_token}	The lexicon is wanted exactly when a live token closes on a judging line (ime_needs_lexicon is sound); a missing lexicon at such a boundary fails closed.	0,1,2,3,5,7,12	2
tt-shipped-en-vocabulary	table-property	TrickLexicon::{shared_en,classify}	The shipped English lexicon classifies the owner's examples (sit, jump, good, kitty, tight, phrase second words) so that each two-word phrase fires once and "good night kitty" is one final Sleep.	0,1,4,9,12	3
tt-bounded-heap	other	TrickListener::close_token; TrickLexicon::classify	The listener's only heap is its scratch, which grows to the fold of the widest window token and no further.	0,1,2,4,8,12	1
glyph-rank-table	table-property	trail_sound::typed_glyph_rank	0 for None, 1..=26 for letters (case folded), 27..=36 for digits, the 32 ASCII marks onto eight buckets 37..=44, 45 for everything else.	0,1,3,5	1
glyph-class-table	table-property	trail_sound::typed_glyph_class	Each ASCII mark maps to its named bucket, letters, space and non-ASCII to LETTER, and SECRET is never produced.	0,1,3,5	1
voice-roster-table	table-property	SoundVoice::{ALL,ALIASES,name,parse}	parse inverts name (trimmed, case-insensitive) over the distinct roster, accepts the aliases, never yields the hidden voices, and returns None otherwise.	0,1,3,5,7	1
kill-swoosh-band-table	table-property	trail_sound::{kill_swoosh_band,palette_trim}	Every band has low < high; a style voice uses its look's band; the four sound-only voices have their own distinct bands.	0,1,3	1
engine-routing	contract	TrailSynth::{v2_engaged,push_meta_latched,push_v2}	A Trail or verdict event under the music box (by voice or by look) goes to the v2 engine and latches it; every other event takes the v1 chain without latching; the three music-box spellings render identically.	0,1,2,3,4,5	2
standalone-voice-alias	equivalence	trail_sound::palette_for; TrailSynth::{push,render}	Choosing a style's voice under any look renders bit-identically to that style's own look.	0,1,2,3,4,5,6,7	1
push-input-guard	contract	TrailSynth::{push,push_meta}	A non-finite field leaves the synth unchanged; a zero gain changes nothing; otherwise fields are clamped; push is push_meta with default meta.	0,1,2,3,6	2
block-pre-roll	contract	TrailSynth::{push_meta,spawn_seeded}	A block lead delays every voice of its own push by the lead (capped at BLOCK_LEAD_MAX_S) and nothing after; a zero lead renders like push.	0,1,2,4,5	1
first-buffer-onset	contract	TrailSynth::{push,render}; every Palette::design	For every voice, look and gesture, the push spawns a voice with zero pre-delay that sounds in the first 16 frames, and per-character gestures attack within 16 ms.	0,1,2,3,4,5,6,7	2
silence-is-exact	invariant	TrailSynth::{is_quiet,render,new}	A new synth is quiet; a quiet synth renders exact zeros and stays quiet; after the last push every script reaches quiet within 6 s.	0,1,2,3,5,6,7	3
output-bounded-finite	invariant	TrailSynth::render; trail_sound::{soft_clip,limit}	Every rendered sample is finite with |x| < 1, and every single gesture peaks audibly and unclipped.	0,1,2,3,5,6,7	1
synth-deterministic	equivalence	TrailSynth::{new,push_meta,render}; trail_sound::pan_gains	Two synths with the same rate and seed fed the same script write identical samples, in release and debug.	0,1,2,3,5,6,7,16	5
polyphony-bounded	invariant	TrailSynth::{claim,claim_lane,spawn_seeded}	At most MAX_VOICES live voices and each lane within its cap; the quietest is stolen, never the newest; bursts stay finite.	0,1,2,3,5,7	1
mb-lane-admission	contract	TrailSynth::{claim_lane,lane_make_room,v2_spawn_decoration}; rainbow_kitty_v2::lane_drops_the_newcomer	A full young lane drops the newcomer only for BLOOM and GLINT and otherwise steals with a fade; a key's TUNE voice is never dropped; the worst-case scripts never steal.	0,1,2,3,5,7	2
bed-gate	contract	TrailSynth::{push_meta_latched,kick_bed,bed_sample}; Palette::{bed_sample,bed_grain}	Only bed-flagged Trail events feed the bed; keyboards' beds are structurally silent and instruments' beds sound; the fed bed exhales to exact zero.	0,1,2,3,4,5,6,7	5
v1-admission	contract	TrailSynth::push_meta_latched	v1 events are thinned on their own clocks (Backspace, Shift, others by MIN_GAP except bypass kinds), clocks advance only with rendered samples, so one host tick's backlog speaks once.	0,1,2,3,5,6	7
governor-loudness-bound	contract	TrailSynth::{push_meta_latched,render}	The event-rate duck keeps a 25 cps flood within 2.5x a single gesture's peak plus a small allowance and under 0.5, for every look and seed and for music-box batches.	0,1,2,3,5,6,7	1
song-frame-conditions	contract	TrailSynth::{push_meta_latched,advance_song}	Only melodic Trail kinds advance the song; deletions, motion, non-Trail gestures, bonks and shifted keys leave the walk, theme and bed unchanged.	0,1,2,4,5	1
song-pulse-table	table-property	trail_sound::SONG_PULSE	Accents sit exactly at 0, 3, 6, 9; ghost offsets are small, non-zero and varied.	0,1,5	1
advance-song	contract	TrailSynth::{advance_song,push_meta_latched}	A Jump or pause re-bars the pulse; accents step the melody and ghosts play quieter and shorter without stepping; n in-phrase keys sing ceil(n/3) notes; the key after a pause, Enter, kill and navigation always sing.	0,1,2,3,4,5	4
theme-playhead	contract	TrailSynth::{advance_melody,open_next_phrase}	Accents play SONG_THEME in order and loop; a rest cadences the open phrase and opens the next, never skipping one.	0,1,2,3,4,5	3
theme-form-tables	table-property	trail_sound::{SONG_THEME,SONG_FORM,tone_register,melody_lean,fold_register}	SONG_FORM partitions the theme into phrases of 8, 8, 6, 6; no tone folds it; it repeats in under a quarter of steps and spans at least 4 degrees.	0,1,5	3
fold-register	contract	trail_sound::fold_register	fold_register is the identity inside [lo,hi] and a triangle-wave reflection outside, never a clamp (lo when the range is empty).	0,3,5	1
bass-walk	table-property	trail_sound::{SONG_BASS,CELEBRATION_BASS}; TrailSynth::space_root_hz	The bass walk matches the celebration roots, uses all five roots without immediate repeats, stays in [220, 440) for every tone, style and key, and never returns within four words.	0,1,3,5,6,7	2
tone-lattice-consonance	table-property	trail_sound::{tone_tables,penta}; TrailSynth::melody_hz; rainbow_kitty_v2::sky_bed_degrees	Every tone's table and transpose stay consonant (no interval in the bonk clash zones), each tone picks its scale and transpose, and the sky bed and riff stay on the active lattice.	0,1,3,5,6	4
tone-feel-and-bonk-exemption	contract	trail_sound::tone_feel; TrailSynth::{spawn_seeded,design_bonk}	tone_feel scales every non-exempt voice's timing; the bonk ignores tone entirely.	0,1,2,3,4,6	2
bonk-design	contract	TrailSynth::design_bonk	A bonk spawns exactly a clash at the minor second and tritone over the walk's root and a thump, moves nothing, and arms the duck.	0,1,2,3,4	1
duck-envelopes	contract	TrailSynth::{render,latch_song_key,release_song_key,v2_hand_back_key,push_meta_latched}	The bonk and sing ducks decay to exact 0 and attenuate every non-exempt voice and the bed; the song key returns when the sing ends (v1) or at the next word boundary (v2), snapped by a later v1 event; glints are muted under the sing.	0,1,2,3,4,5,6,7	6
celebration-tables	table-property	trail_sound::{CELEBRATION_PHRASE,CELEBRATION_BASS,CELEBRATION_FILL,CELEBRATION_VERSE,celebration_bar_degrees,celebration_root,celebration_mode}; kitty_sing::song_signature	The riff tables are wrap-safe, every bass beat has a carrier, bars fit the polyphony budget, degrees stay on the lattice, choruses are identical across signatures while verses walk and differ per key, and the neutral signature is the reference. Wrap safety is held by const assertions beside the tables (the phrase a power of two that divides the u16 bar counter's range, the last swung eighth inside its bar).	0,1,3,5,7	4
celebration-design	contract	TrailSynth::{design_celebration,design_celebration_outro,latch_song_key}	A bar spawns one voice per sounding slot as a function of (bar, sig); a key change damps the old ring, the same key damps nothing; the fill carries the root, mode rotation recolours the chorus, the clap joins at bar 8, and the outro holds do.	0,1,2,3,4,5,7	7
gesture-family	contract	trail_sound::{gesture_shape,gesture_bend}; TrailSynth::{design_cursor,design_trail}	Each cursor gesture has its stated shape; a glide is one in-key tone, a sweep a rate-limited run, both in the palette's register without stepping the walk; pitched instruments land on the melody note.	0,1,2,3,4,5,7	6
v1-space-downbeat	contract	TrailSynth::design_space; trail_sound::twinkle_hz	A space run's head plays one walking-bass downbeat with an air twinkle in the sparkle band, damping the previous bass; tail spaces breathe and add rate-limited rising indent steps; after a rest the head breathes.	0,1,2,3,4,5,6,7	6
v1-shift-ting	contract	TrailSynth::design_shift; trail_sound::{ting_octave,SHIFT_ROTATION}	A Shift spawns one struck ting in its band on the rotated degree, never stepping the walk, and decays out before its release.	0,1,2,3,4,5,6,7	3
v1-capital-lift	contract	trail_sound::{shift_lift,shift_gain,shift_ratio}; TrailSynth::spawn_seeded	A shifted v1 key keeps its voice count with pitches lifted an octave (never lowered), louder by its gain, without moving the walk; it is audibly louder at every walk position.	0,1,2,3,4,5,6,7	2
erase-poof-anatomy	contract	TrailSynth::{design_erase_poof,design_word_poof,design_poof,damp_pending_shimmer}	A Backspace is exactly a two-voice noise poof, the same in every palette; a word kill is a lower poof; a puff is one noise voice; a held delete run books one shimmer on release.	0,1,2,3,4,5,7	3
palette-voice-anatomy	contract	TypewriterPalette::{design,strike}	A Typewriter key spawns exactly three voices, one carrying the typebar's metal pair.	0,1,2,3,4	1
isolated-loudness-ladder	other	TrailSynth::{push,render,push_v2}; every Palette::design	For every voice the isolated gestures keep their loudness order (puff < Backspace < word kill < kill, Space and Shift near the key), heat lifts a key audibly, the ting stays under its capital, the space head is heard with top, and code is no louder than prose.	0,1,2,3,4,5,6,7	8
rendered-timbre-bounds	other	rainbow_kitty_v2::{tine,wood,pluck,forte}; TrailSynth::{design_erase_poof,v2_shift,render}	Isolated renders meet their spectral bounds: the poof is airy above the key, the ting is clean and warm, the bloomed step stays under the glass line, the felt mallet chirps into the sparkle band, families are distinct, a shifted key holds its pitch.	0,1,2,3,4,5,6,7	8
capital-weight-rendered	other	rainbow_kitty_v2::forte; Forte::of; TrailSynth::{v2_typed,render}	Under the music box a capital outweighs its plain twin by its weight in onset RMS and peak within bounds, rings a twelfth at a word head, is brighter early and mellower later than force 0, and crests no higher than before forte.	0,1,2,3,4,5,6,7	4
sin01-accuracy	contract	trail_sound::sin01	For finite ph, sin01(ph) is within 2^-15 of sin(2 pi frac(ph)).	0,1,3,5,6	1
v2-fields-identity-on-v1	invariant	TrailSynth::push_meta_latched; Voice::default	Every voice a v1 event builds has every v2 field at its identity, so no v2 branch runs for it.	0,1,2,4,5	1
mb-sky-bed	contract	rainbow_kitty_v2::sky_bed_degrees; TrailSynth::bed_rainbow_sky; CHORD_LOOP; BED_PARTIALS	The sky pad plays the live chord's lit degrees an octave under the tine, transposed with the song key, and the hue moves its tilt and width, never its pitch.	0,1,2,3,5,6	2
mb-music-box-only-gestures	contract	TrailSynth::{v2_verdict,v2_strum,design_output_pip}	Verdicts and strums sound only under the music box, a verdict is never louder than a key, and a strum is four rising tines on the chord that do not step the melody.	0,1,2,3,4,5,6	2
mb-one-key-one-step	contract	MelodyV2::{on_typed,detect_autorepeat}; TrailSynth::v2_typed	Every non-secret key steps the melody exactly once and sounds a TUNE voice; a machine-regular held key becomes mallet-only after AUTOREPEAT_RUN.	0,1,2,3,4,5	2
mb-melody-is-a-function	equivalence	MelodyV2::{on_typed,derive}; TrailSynth::push_v2	The music box's degrees are a function of the key sequence and its rhythm only: invariant under a time shift, pan, tone and seed.	0,1,2,3,5,7,16	2
mb-melodic-bounds	invariant	MelodyV2::{derive,break_run,note_run,on_typed}; rainbow_kitty_v2::reflect_deg	The walk stays in its register, never runs too long in one stride, leaps only within bounds inside a word, repeats a pitch only for a named cause, re-strikes doubled letters and keeps capital runs' contour.	0,1,2,3,5,7	7
mb-capital-forte	contract	Forte::of; rainbow_kitty_v2::{forte,tine}; TrailSynth::v2_typed	A shifted key is one unbent strike on its lifted degree with force by context and gain by its law, plus one twelfth ring only when it opens a word or run (its draws spent either way); a plain key is the bare tine; Backspace restores the opener.	0,1,2,3,4,5,6	6
mb-ting	contract	TrailSynth::{v2_shift,v2_duck_ting,v2_live_ting_at}; rainbow_kitty_v2::{ting,ting_fold}	A bare Shift is one ting on a lit chord tone in its band that never steps the melody; it rings through the next key and ducks only for a capital it announced.	0,1,2,3,4,5,6,7	3
mb-same-pitch-no-comb	contract	TrailSynth::{v2_damp_same_pitch,v2_live_ting_at,v2_nav_tick}	A new voice within a cent of a live one in its lane damps the old one first; a strike on the ting's pitch damps the ting; a ring on it is skipped with its draws still spent.	0,1,2,3,5,7	3
mb-class-voices	contract	TrailSynth::v2_typed; rainbow_kitty_v2::{wood,pluck,pluck_class,stop_breathes}; MelodyV2::on_typed	The glyph class picks the voice: digits are wood bars that count in the stardust, marks are plucked with their bucket's graft, re-struck marks graft nothing, split classes are distinct, unknown ids are letters, stops ring and commas breathe, sparkles follow the strike's level.	0,1,2,3,4,5,6	7
mb-punctuation-phrasing	contract	MelodyV2::{on_typed,close_aside}; rainbow_kitty_v2::{cadence_target,mark_chords,nearest_cand,phrase_kind}; TrailSynth::{v2_enter,v2_space}	Steering marks land on their cadence classes, sentences never end or start where the last did, asides are quieter and end correctly, quotes lift and return, a question's return lands on G with a pure rise, the next Space confirms the mark's chord, and code punctuation never moves the harmony.	0,1,2,3,5,7	8
mb-undo	contract	MelodyV2::{push_undo,pop_undo,frame,on_backspace}; TrailSynth::v2_retire	Typing then Backspace restores the melody frame, a retype replays the same degree, the deletion is an unpitched poof that mutes the last note, and a Backspace retires the key's graft.	0,1,2,3,5,7	4
mb-graft-retirement-refines-model	refinement	TrailSynth::{v2_retire,v2_cancel_unsounded,v2_damp,push_v2}; graft_retirement_model (ty_model!)	The graft retirement model proves its invariants and catches the omitted-delete mutant; real grafts and rings refine it: owned voices retire (cancelled or damped), disowned ones survive.	0,1,2,10	3
mb-secret	contract	TrailSynth::{v2_secret,v2_typed}	A password key is one fixed tone whatever it was and leaves the melody exactly where it was, advancing only the rng.	0,1,2,4,5,16	2
mb-ioi-arc	contract	rainbow_kitty_v2::{tau_v_s,g_ioi,roof_hz}	tau_v_s meets its anchors and reaches its floor; g_ioi = clamp(sqrt(s/0.25), 0.45, 1); roof_hz is non-decreasing in rate.	0,3,5,6	1
mb-take-loudness	other	TrailSynth::{push_v2,render}; rainbow_kitty_v2::{flow_key_headroom,flow_partials}	Faster typing is brighter, never louder; flow opens the ring-out without raising a take's RMS or peak.	0,1,2,3,5,6,7	3
mb-bloom-and-room	contract	rainbow_kitty_v2::{bloom,hue_air}; TrailSynth::v2_typed; MelodyV2::answer_deg_above	Each lit step carries one bloom on the lattice following the hue; the room answers only line ends and rests, from above.	0,1,2,3,4,6	2
mb-roughness	invariant	TrailSynth::push_v2	No two live non-exempt partials above 1 kHz beat in the 15..60 Hz roughness band.	0,1,2,3,5,7	1
mb-flow	contract	rainbow_kitty_v2::flow_partials; TrailSynth::{v2_flow_echo_phase,v2_typed}	Flow is the identity at heat 0 and conserves the partial sum; the flow echo needs the current key's sounding octave and always adds body.	0,1,2,3,4,5,6	4
mb-space-bass	contract	TrailSynth::v2_space; MelodyV2::on_space; rainbow_kitty_v2::{indent_step_hz,bass_dyad,head_breath}	Every Space dyad is a pure fifth or fourth in the bass lane and Enter comes home to chord I; a head is one dyad and a breath, never over the letter; tail spaces add rate-limited rising indent steps, periodic on every chord and key.	0,1,2,3,4,5,6,7	4
mb-meteor	contract	TrailSynth::{v2_meteor,v2_meteor_arm,v2_claim_arm,v2_release_arm,v2_core_voice}; timing::flight_ms; rainbow_kitty_v2::met_tick	The meteor bell and landing share the flight clock; the core carries direction; Land is silent; arms fizzle or are claimed without a click, a second meteor never eats a sounded bell; the tick is heard briefly.	0,1,2,3,4,5,6,12	5
mb-cascade	contract	TrailSynth::v2_cascade; MelodyV2::{on_jump,run_theme_step,jump_echoes_enter}	A jump run plays one cascade and then rate-limited re-strikes; a keyed Enter mints none; streamed line feeds walk the theme and a lone one stands.	0,1,2,3,4,5	2
mb-stardust-glint	contract	TrailSynth::{v2_glint,v2_glint_at}; MelodyV2::next_glint_deg	A hero star's cue is one pitched glint twinkling at the star's rate.	0,1,2,3,4	1
mb-tails-and-damps	invariant	every v2 voice builder; TrailSynth::v2_damp	Every music-box voice ends quietly (at most 0.07 of its level) and every damp is at least the 12 ms ramp; the ting decays out before its release.	0,1,2,3,5,6	1
mb-bus-limiter	contract	trail_sound::limit; TrailSynth::render	The limiter is exactly transparent below its threshold and holds the ceiling above, returning to gain 1 after.	0,1,2,3,5,6,7	1
mb-fallback-clock	contract	TrailSynth::{v2_at_ms,render}	An unstamped key's clock is the render clock wrapped into u32 like a host stamp, so keys still step across the wrap.	0,2,3,5,6	1
wd-twinkle	contract	word_decorations::twinkle; effect_util::twinkle_env	twinkle(s,t) = twinkle_env(t, phase of the seed's low byte), a function of wall time only, whose envelope peaks at most 3 times per second, so frame rate cannot add flashes.	0,3,5,14	1
wd-cat-stacked-on	contract	word_decorations::cat_is_stacked_on	True iff the rects intersect and the overlap covers at least a third of the cat's own area (computed in i64); degenerate rects never stack.	0,3	1
wd-companion-footprint	contract	CompanionOnGlass::{at_cell,at_head,at_body,footprint}; CompanionFootprint::stacked_on	A caret-cell companion claims its 2x1 caret band, a flying head that band and its head rect separately, the pet only its body rect.	0,1,3,4	1
wd-one-body-per-frame	invariant	WordDecorations::{begin_host_frame,pet_cursor,kitty_cursor,kitty_cursor_at_placement,tick}	Between two begin_host_frame calls at most one companion emitter returns Some and appends sprites; refused calls append nothing; the next frame re-admits one.	0,1,2,4	2
wd-bake-budget	invariant	WordDecorations::{begin_host_frame,tick,kitty_cursor}; CatBaker::{begin_frame,host_tile,frame_clock,version}	Each host frame admits at most MAX_BAKES_PER_FRAME new tile bakes across every pane and emitter, advances the frame clock once, and a resident bake key is a hit that bakes nothing.	0,1,2,7	2
wd-sparkle-lifecycle	contract	WordDecorations::tick; emit_ink; emit_rainbow_sparkles	A profanity sparkle draws density Add decos changing every frame through its animation, then one fading residual spark, then nothing and goes idle; a rainbow draws three star decos for RAINBOW_SPARKLE_MS while ink may stay.	0,1,2,3,7	2
wd-reduced-motion-static	invariant	WordDecorations::{tick,is_active}	Under reduced motion from birth every tick is identical between scans, no nova or supernova is granted, a profanity shows one static spark with static ink, a feline shows its static pose and logs one sighting.	0,1,2,3,5,7	5
wd-idle-fixed-point	invariant	WordDecorations::{tick,is_active}	Once idle, ticks without new input return the same streams and fp; frames whose streams differ have different fp.	0,1,2,3,5,7	2
wd-settle-identity	equivalence	WordDecorations::tick; emit_ink; emit_rainbow_ink	Settled ink equals the reduced-motion static gradient; a rainbow returns exactly to its first frame after a full cycle on dark and light; a born-done word shows the settled bytes from its first tick.	0,1,3,5,14	2
wd-empty-emits-nothing	contract	WordDecorations::tick	With no occurrence and no live flash, tick returns fp 0, clears every scratch stream and owes nothing; a word replaced in place is evicted at once.	0,1,2	2
wd-reset	contract	WordDecorations::{reset,hard_reset,hard_reset_words}	reset blanks the next tick, drops cues and the flash, and writes every started episode's done mark first; hard_reset also clears marks, combos, placement and the tile bakers' caches; hard_reset_words keeps cursor placement and those caches.	0,1,2,7	3
wd-scan-classification	contract	WordDecorations::{rescan,rescan_from_cells,rescan_from_cells_with_geom_at_cursor}; Lexicon::{classify,scan}; class_default_spec; spec::build_custom	After a scan the occurrences are exactly the whole tokens the lexicon classifies (or custom words override) whose class is enabled and not ignored, deferring ambiguous forms under the caret; no prefix, substring or partial matches; species tags pass through.	0,1,3,5,7,12	12
wd-scan-path-equivalence	equivalence	WordDecorations::{rescan,rescan_from_cells}; Terminal::cell_frame_into	Scanning the terminal and scanning its cell frame give identical occurrences, ink buffers and epoch, including wide and double-width rows.	0,1,2,7,12	1
wd-scan-memo-transparent	equivalence	ScanMemo::{fit,touch,remember}; WordDecorations::rescan_from_cells_with_geom_at_cursor; scan_inputs_fp; scan_opts_fp	The engine with the scan memo equals the engine without it after every rescan; the memo retires when gates or the lexicon change, and a hit rebuilds scan_chars.	0,1,2,5,7,11	4
wd-keyed-hashers	other	ScanMemo::{hot,cold}; DoneMarkLru::index	The memo and done-mark index use a per-process keyed hasher, never the unkeyed one; the collision bound is a property of the hasher family.	0,1,11	2
wd-bounded-storage	invariant	WordDecorations::{rescan,tick}; ScanMemo; DoneMarkLru; grant_ignition; prune_ignitions	Occurrences, persisted episodes, done marks, reservations, ink and memo rows stay within their caps, scratch planes are handed back, and periodic input stops growing capacity.	0,1,2,7,8	3
wd-done-mark-lru	refinement	DoneMarkLru::{insert,insert_with_cap,touch,contains_key,keys_oldest_first,assert_valid,detach,attach_newest}	The intrusive LRU refines a bounded ordered set (done_mark_lru_model): insert refreshes or evicts the oldest at capacity, touch refreshes, each operation makes at most 6 link writes, and the structure stays consistent without growing.	0,1,2,7,10	2
wd-ignition-limiter	contract	word_decorations::{grant_ignition,grant_pane_ignition}	A grant is refused when the pane's reservations are full; otherwise it takes the earliest start at or after now with fewer than 2 starts in the window (1 among overlapping centres), window-wide; so the limiter refines FlashLimiter (WCAG 2.3.1).	0,1,2,3,5,7,10	6
wd-ignition-prune	contract	WordDecorations::{prune_ignitions,scan_scope}	prune keeps a reservation while its window is open and it is either fired, owned by another scope, or still owned by its live episode; so fired history survives, a departed owner's future slot is cancelled, and other scopes are untouched.	0,1,2,7,13	3
wd-ignition-rekey	refinement	WordDecorations::align_pending; prune_ignitions	Whenever alignment re-keys an episode it moves that episode's pending reservation with it, so every pending reservation's owner is live and matches.	0,1,2,7,10	1
wd-identity-model	refinement	WordDecorations::rescan; Episode::fresh; grace sweep	The persist map refines sparkle_identity_model: a first sight is one birth, redraws, recognized moves and re-hits within GRACE_TTL keep the episode, expiry writes the done mark, and the next appearance is born done.	0,1,2,5,7,10	8
wd-genome-derivation	contract	word_decorations::{seed_of,mix}; genome::roll	The seed is a function of column, class and surface; magic depends only on the row's context words; context-identical twins share genomes.	0,1,3,7	2
wd-alignment-cardinality	refinement	WordDecorations::align_pending; alignment_edge; AlignScore	Each surface group is aligned by a maximum-cardinality row-anchored assignment over untainted, in-grace episodes with exact context or a short weak redraw edge; births equal net growth and survivors keep their state (SparkleReflowCardinality).	0,1,2,5,7,10	13
wd-typed-rearm	contract	WordDecorations::{note_typed_edit,typed_edit_witness,spend_typed_edit,rescan,set_scan_session,bind_pane,retain_panes}	A birth at a spent identity is armed iff a fresh typed witness of its own scope sits on its span, and spends the witness; repaints, parked carets, passive output and keys elsewhere never re-arm, one key re-arms once.	0,1,2,5,7	12
wd-done-marks	invariant	WordDecorations::{tick,rescan,reset}; mark_done; touch_done	A done mark is written when a one-shot starts, on reset, on grace expiry and on capacity drops, never for a settled birth, and removed only by hard_reset or eviction; no timer re-arms a marked word.	0,1,2,5,7	3
wd-persist-capacity	refinement	WordDecorations::align_pending; PersistAdmission	The persist map stays within PERSIST_CAP, and at capacity a fresh visible birth beats an unmatched grace episode, whose mark is written (sparkle_persist_capacity_model).	0,1,2,10	1
wd-occurrence-cap	contract	WordDecorations::rescan	Occurrences keep the longest bottom suffix of rows within MAX_OCCURRENCES (dropping whole top rows), in order, and an identical rescan keeps the same episodes.	0,1,5,7	1
wd-ink-layout	contract	WordDecorations::tick; emit_ink	Ink is sorted by cell with one entry per lead cell of each ink-bearing word, truncated by whole words within MAX_INK_CELLS; ink off gives no ink while fp still folds.	0,1,5,7	2
wd-ink-color	contract	emit_ink; emit_rainbow_ink; legible_strength; nudged_ink_pair; rainbow_base_hue	Classic gradients mix toward the genome's pair at the eighth-step strength that keeps text contrast; rainbows span at most 100 degrees per lead and check contrast at four samples; light grounds take deep tones.	0,3,5,7,14	3
wd-nova-one-flash	refinement	WordDecorations::tick (nova_prepass); emit_nova_axis	Each nova episode is granted at most once, draws its phases in its window, leaves one fading ember, then nothing; re-appearance after expiry is born done; only hard_reset re-arms (NovaPhase).	0,1,2,3,7,10	1
wd-nova-ink	contract	emit_ink; nova::{dip_envelope,ember_pair}; dim_rgb	During a nova its ink is frozen (dimmed in the dip) and afterwards takes the ember pair; under Sparkle the ink keeps sweeping.	0,3,5,14	1
wd-nova-coupling	contract	emit_ink (coupling pulse); nova::coupling	A neighbour's coupling pulse is deterministic, changes luminance by at most 0.05, and only while the ring front crosses it.	0,3,5,14	1
wd-nova-concurrency-cap	contract	WordDecorations::tick (nova_prepass)	At most MAX_CONCURRENT_NOVAS live novas emit, row-major; later ones skip to their ember.	0,1,2,7	1
wd-selection-safety	invariant	WordDecorations::tick; split_super_selection; SelView::cell_selected	No burst is granted over a selection (it waits for deselection), no burst quad or Over deco covers a selected cell (wash rows are split around it), and the summed overlay stays under MAX_VIEWPORT_OVERLAY.	0,1,2,5,7,12	3
wd-burst-mutex	refinement	WordDecorations::tick (super_prepass, nova_prepass); burst_mutex_end	Window-wide at most one supernova is live and never beside a classic nova; supernovas are granted first; a window holds for its tier's duration (supernova_burst_mutex_model).	0,1,2,5,7,10	5
wd-burst-roll	contract	WordDecorations::rescan (birth roll); combo_births ring	A burst rolls at birth with its chance (100 always, 0 never) adjusted by the typed combo, the roll moves with alignment, only typed profanity charges the combo, which cools by engine time and resets on hard_reset; level 4 makes a certain Nuke.	0,1,2,3,7	6
wd-burst-emission	contract	emit_nova_axis; emit_super_axis; burst_center; nova_env; nuke::emit	A burst is centred on its span (double-width rows doubled), takes the theme branch from the captured background even with ink off, shows magic variants only with profanity_magic, and a Nuke puts cloud sprites on the free lane.	0,1,3,7	4
wd-magic-gates	contract	special_variant_v4; WordDecorations::tick	feline_magic off uses the plain variant and no aura; the Paw style draws no feline graphic.	0,1,3	1
wd-peek-lifecycle	refinement	WordDecorations::{tick,rescan,set_presentable}; Episode::fresh; peek_total_ms	Each feline episode peeks exactly once on a clock latched at birth (rise, dwell, descend, done), focus and occlusion never replay it, reduced motion shows its pose once, and words arriving while away are born settled (OneShotPeek).	0,1,2,5,7,10	7
wd-peek-occlusion-pause	contract	WordDecorations::tick; cat_eligible; cat_peek_plan	A dwelling cat whose bands become walled pauses its clock and draws nothing, then resumes when they clear.	0,1,2,7	1
wd-peek-plan	contract	cat_peek_plan; band_occupancy; cat_eligible; peek_eligible	The cat peeks up when the band above has occupancy at most 0.75 (not from row 0 or 1), else down if that band is clear enough, else not at all; sparse blockers never veto.	0,1,3,5,7	3
wd-peek-geometry	contract	cat_geometry_for; authored_cat_size; fit_glyph; cat_hart; cat_chin; emit_cat; emit_animal; CatIdlePose::breathing	One under-text sprite per peek at the authored aspect and age-grown size, centred on the span, revealed and hidden through its rise and descend within its bounds, breathing without resizing, and fixed under reduced motion.	0,1,3,5,14	7
wd-peek-timing	contract	peek_dwell_ms; dwell_base_ms; anticipation_ms; peek_total_ms	The dwell, anticipation and total are the stated u64 functions of ident and genome, capped at 4580 ms total.	0,3	1
wd-cat-context	contract	cat_color_context_footprint; WordDecorations::rescan_from_cells_with_geom_at_cursor; cat_peek_plan	The cat's colours are sampled over its footprint at birth and frozen; its clearance is rechecked every scan with the transferred genome.	0,1,3,5,7	2
wd-companion-claim	contract	WordDecorations::tick (companion_claim, caret_word); CompanionFootprint::stacked_on; caret_on_span	An ambient cat is suppressed iff the companion's caret is on its word, it is the claimed or summoning word, or the companion's footprint stacks on it; every other cat is unchanged.	0,1,2,4	7
wd-slot-pools	invariant	WordDecorations::tick (episode_prepass)	At most MAX_CATS cat peeks and MAX_ANIMALS animal peeks arm, counting visible words only; scrolled-off peeks finish by wall clock; animals log no sightings.	0,1,2,7	1
wd-kitty-sighting	contract	WordDecorations::{tick,drain_kitty_sightings}; kitty_registry::accessory_trait_bits	Each feline episode yields exactly one sighting on its first landed frame with its traits, never for done, settled, Paw or animal episodes.	0,1,2	2
wd-peek-cue	contract	WordDecorations::{tick,drain_peek_cues}	Each landed peek records exactly one cue at the word's lead cell with its drawn rect.	0,1,2	1
wd-curse-cue	contract	WordDecorations::{tick,drain_curse_cues}	A profanity birth records one Typed cue iff it was typed (witnessed); output never does; each burst grant records one Detonated cue; cues clear at each tick and on reset.	0,1,2,5	7
wd-freeze	contract	WordDecorations::{freeze,thaw,rescan,tick}; trick_flash	While frozen rescans are no-ops and ticks are dark; thaw shifts every episode clock by the frozen span, so it resumes where it stopped.	0,1,2,5,7	2
wd-pane-scoping	invariant	WordDecorations::{bind_pane,set_scan_session,retain_panes,needs_rescan,rescan}	Per-scope state is parked per pane and session, so identical words in two panes or tabs are independent episodes; retain_panes drops departed panes; the limiter, mutex and bake budget stay window-wide.	0,1,2,7	5
wd-resize-settle	contract	WordDecorations::rescan (settle_until)	After a width change of a scope, births until RESIZE_SETTLE_MS of stable width are born settled and unmarked; widths are per scope.	0,1,2	2
fp-field-coverage	contract	word_decorations::{fold_free,fnv_mix_u64}; PetFrame::fp	Folding a sprite into the fingerprint distinguishes sprites differing in any single field; PetFrame::fp distinguishes under_ink.	0,1,3,7	2
wd-kitty-cursor-emission	contract	WordDecorations::{kitty_cursor,kitty_cursor_footprint,set_kitty_sprite_source}	The flying kitty draws one body sprite at its footprint (clamped after scale and lead, rising at the right edge, raised by the seat's whole rows of lift, which the settled follower glides while it excludes only the hover bob), sized within its slot for every age, from a user sprite when set and nothing when that source is invalid; song notes precede it scaled by the sing drive. Nothing it emits starts above the grid top: on row 0 and the row-1 sky seat the seat stops at the grid top less the hover bob's reach (BOB_AMP of a row), and no stretch, fold arc or note rises past it.	0,1,2,3,4	9
wd-cursor-placement	refinement	WordDecorations::{resolve_kitty_cursor_placement,kitty_cursor_at_placement,rebase_kitty_cursor_placement,kitty_cursor}; KittyCursorPlacementState	The placement ledger refines cursor_cat_fold_model: folds start continuous, change side only off glass, glide at most 4 px per frame onto the footprint, and coordinate or source changes retire them; the compat path draws the raw footprint.	0,1,2,3,5,10	7
wd-held-tiles	contract	WordDecorations::{pet_cursor,dog_cameo,drop_holds_if_metric_moved}	A held tile is re-emitted only for the same look; a metric change drops every held tile.	0,1,2	3
wd-pet-sprite-in-grid	contract	WordDecorations::pet_cursor; PetFrame::body_px; pane::translate_free_into_pane	Every pet sprite lies inside the grid and the body equals the clamped body_px. With the caret on row 0 or 1, the pet's and the flying head's sprites land whole in a pane at the window top (whose open top edge would let an overhang into the pad or, after the splice, the tab strip) and in a pane below a divider.	0,1,3,7	3
wd-dog-cameo	contract	WordDecorations::dog_cameo	Once baked, the cameo is one sprite at the frame's alpha trailing behind the caret.	0,1,2,3	1
wd-trick-flash	contract	WordDecorations::{note_trick_typed,tick,trick_flash_phase}	A live flash on a wordless screen emits exactly its cells and its own deadline; beside other ink it only extends the deadline; a pending flash changes nothing.	0,1,2	5
art-asset-quality	table-property	art/glyphs and art/animal TOML; cat_glyphs_gen::GLYPHS; aterm_scene::{fill_path,parse_path}	Every roster asset is semantic and bounded (layers, commands, one dark outline first, roles, anchors and aspect in band), survives a 16 px bake per role, and is distinct within its class; the maneki's paw keeps its keyline.	0,1,3,7,8	11
art-roster-completeness	table-property	Lexicon::{builtin,species_codes,classify_token}; animal_glyph_from_key; cat_glyphs_gen::{HEADS,GLYPHS}; KittyLook::{for_launch,normalized}; authored_cat_size	The lexicon's species and the authored animal roster are the same set and every head resolves; launch looks always pick a head and never the maneki, which keeps its aspect on the discovery path.	0,1,5,8,12	3
rb-row-scoped-flow	invariant	CursorGlow::tick; Ribbon::{place,leave_row}; Cohort::flow	At the host seam only the caret's row renews; a row the caret left by a wrap flows into the fold, stays lit through most of its retract, never rises, and is dark by FLOW_TOTAL_S, outliving the caret by at most 1.75 s.	0,1,2,3,5,7,10	2
rb-walk-continuity	contract	Ribbon::{join_cohort,seat_fold_successor,settle_carry,fresh_line,heal_seams}; Cohort::t_at; ribbon::walk_t	Across a shell fold, composer wrap or box growth the new row continues the old row's walk one step on at the pace it had reached, whatever the echo lag, pause or wrap shape; the relaid word keeps its t; a composer newline starts a fresh fast leg; a new line never heals onto the old remnant.	0,1,2,3,5,7	29
rb-follow-witness	contract	witness::{follow_runs,shape_verdicts,witness}; Engine::follow_rows; Ribbon::retire_cells	At the host seam a lit run whose glyphs arrive as one block a row or two away is translated with its clocks (followed counted, nothing retired beyond a wrap's slack); a run arriving around a hole is counted missed once; changed glyphs retire their cells; identical redraws keep every cell; a line killed under an identical line drains on its own row.	0,1,2,5,7	29
rb-trail-status-line	contract	cursor_glow::TrailStatus::line_v2	Under v2 the status line ends with the meteor, bridged, retired, followed and follow_missed fields, with nothing before them moved.	0,1,9	1
rb-echo-custody	invariant	CursorGlow::{tick,observe_print_anchor_glyph,observe_caret_drawn,supersede_typed_press,clear_typed_keeping_hand,hand_types_at_the_caret,refused_below_the_caret}; seam::admission; custody parks	At the host seam, for every interleaving of presses, echoes, status-row ticks, scrolls, frame phases and latencies: a press licenses only its own echo and program rows elsewhere are refused without forgetting presses; no cell lights off the composer row; footer parks flush without painting; a delayed echo is still paid; returns and margin wraps move at once; the next key after a declined relocation lays at the true caret; the re-anchor stays inside its pane.	0,1,2,5,7,10,13	15
rb-same-caret-echo	refinement	CursorGlow::tick (same-caret re-own); seam::{same_caret_typed_echo,exact_pending_prefix_slot,merged_correction,judge_merged_correction}; CursorGlow::row_shown; Ribbon::sweep	On a caret that did not move, a cell is re-owned by a typed echo iff same_caret_typed_echo_model admits it (an exact in-flight press of that glyph, a prior probe, a blank-to-glyph change after the press, no ambient particle; or a merged correction: a Backspace whose retreat no caret move has shown and the j keys typed since it rendered as one repaint, the caret moved j − k for the k cells the erases owe, the j cells before it spelling those keys, one of them changed, judged as the erases' retreat and then the keys' advance, against the last probe that showed the caret after a torn repaint; a glyph replaced with no erase claims nothing); exact coalesced batches of 9..128 keys light every cell on their first frame.	0,1,2,5,7,10	14
rb-erase-echo-order	refinement	CursorGlow::{note_backspace_erasing,erase_hop,classify_move,park_candidate,spawn_judged}; EraseWitness	Bytes are processed in order, so a Backspace's echo follows the echoes of the keys typed before it and precedes those of the keys typed after it, as erased_key_echo_order_model states. While no caret move has shown it (within the press patience) the erase licenses its own same-row retreat of up to the cells the Backspaces owe, which is the deletion and never Ink's park; a forward hop the presses typed before it pay is theirs and leaves it owed; past those, cells printing the glyphs of keys it retired before they echoed are their late echo, paid by no press and leaving it owed; any other move shows it past.	0,1,2,5,7,10	3
rb-typed-continuity	refinement	Ribbon::{revive,restore_abandoned,at_rest,next_change_deadline}; CursorGlow::tick	A retired typed run is revived only by a prompt typed continuation over its unchanged text (RainbowTypedContinuity); dormant runs owe nothing; an abandoned cohort is restored with its original clock only by its complete original text before expiry.	0,1,2,5,7,10	5
rb-band-connectivity	invariant	CursorGlow::tick; Ribbon::sweep; Ink::plan	While the hand types a row, its live cells form one interval with no dark interior run, through hops, inserts, particles and keys typed over lit cells, and a typed line ends covered.	0,1,2,5,7	3
rb-soft-wrap-indent	invariant	CursorGlow::{soft_wrapped_caret,key_pushed_text_down}; Ribbon::settle_carry; Engine::replay_events	For a soft-wrapping composer, under every present timing: nothing lights the indent, every column from the indent to the caret is lit from the landing on (first letter included), nothing lights over moved-down app text, and the old row empties on time.	0,1,2,5,7	6
rb-hand-return	contract	Ribbon::{take_row_back,clear_seam}; CursorGlow::tick	When the hand returns to a flowing row, its cohorts stop flowing and the seam clears within a frame, decaying like a never-folded row.	0,1,2,5,7	3
rb-short-wrap-park	refinement	CursorGlow::tick (park hold/flush); witness::follow_runs	Short-wrap park observations refine rainbow_short_wrap_park_model: a proved wrap relays the moved word, and a later park with no content move flushes unlicensed and drains nothing.	0,1,2,10	1
pet-contact	contract	PetBrain::{observe_console,tick,note_console_input,console_attention,console_reason}; PetWorld::observe; PetWorldFacts::read	Contact happens iff, during an unconsumed text input episode, a coherent observation saw new ink appear beside the body; scrolls, viewport round trips, incoherent snapshots and earlier ink never count; one pending contact, consumed once; unchanged ink gives Typing.	0,1,2,5,7,12	8
pet-cursor-home	refinement	PetBrain::{tick,observe_console,sense_ink,needs_frames,console_reason}; PetWorld::{under_text_clearance_past_caret,ink_clearance}; PetFrame::body_px	The resident refines console_cursor_home_model: a full body stays near the painted caret over dense ink and under text, a full selection hides it, a hidden cursor and hidden region scrolls keep its last home without fake input or fades, a moved caret is followed home, and typing keeps it visible.	0,1,2,3,5,7,10,12	14
pet-resident-handoff	refinement	PetBrain::{tick,console_legacy_motion_pending,console_resident_handoff_pending,pending_pets,needs_frames,next_change_deadline,note_petted,content}	The handoff refines console_resident_handoff_model: legacy motion owns the cadence until it lands, landing owes one resident tick that reads or yields (consuming touch), and then the brain is quiet.	0,1,2,3,10,12	2
witness-follow-refines-twin-model	refinement	witness::Witness::{follow_runs,walk}; Seen::arrivals; aterm_models_effects::ribbon_follow_twin_model	For every run of N distinct-glyph records, each arrived, standing, absent or absent-twin and gone or not, the real follow verdict equals the RibbonFollowTwin Decide successor and every invariant holds (the test checks N = 6).	0,1,2,5,7,10	1
witness-arrival-evidence-refines-model	refinement	Witness::{walk,arm}; Seen::{pending,twin}; aterm_models_effects::ribbon_arrival_evidence_model	For every RibbonArrivalEvidence trace up to its bound, the real witness's evidence projects onto the model state after each step and follow_runs names the run iff the model says clear and not twin.	0,1,2,5,7,10	1
copy-is-not-arrival	contract	Witness::{follow_runs,walk,translate,translate_band,shape_verdicts}; Engine::follow_rows; CursorGlow::tick	If a typed line is erased while every record's glyph stands at a follow offset as a twin (seen at arm or for TWIN_MIN, not first shown by the erase), no run is followed there and nothing on that row ever lights, for every key count, lag, erase key, pacing, band move, scroll, alt screen and host.	0,1,2,5,7,8,12	24
witness-row-slots	contract	Engine::ribbon_rows_for; CursorGlow::ribbon_rows; Witness::find_deferred_runs; CURSOR_WITNESS_ROWS; WITNESS_ROWS	With keys waiting the caret row and its neighbours have reserved slots after the bands' rows; a row with a slot is never withheld; a run beside an undelivered row is deferred one walk, and an undelivered row is never evidence of arrival.	0,1,2,3,5	8
twin-evidence-timing	contract	Seen::{observe,pending,twin}; Witness::{translate,translate_cells,last_walk}	A copy becomes a twin iff seen at arm or on every sampled frame over at least TWIN_MIN, and stays one; carried records start over, scrolls keep evidence; so a torn relocation onto a row that stood under TWIN_MIN follows and one that stood longer melts.	0,1,2,3,5	7
move-is-followed	contract	Witness::{follow_runs,translate_cells,find_deferred_runs}; Ribbon::translate_run; Engine::follow_rows	If a lit run's text moves as a block one or two rows (composer growth, Shift+Enter, streamed rows pushing a box, repeated or consecutive moves, a paste then a wrap, a hidden-caret relocation), its target row is sampled and no glyph there is a twin, then one frame later the run is translated with its clocks and nothing is stamped (except the stated limits).	0,1,2,5,7,8,12	15
text-gone-leaves-by-swoosh	contract	Ribbon::{abandon,retract}; Witness::{walk,landed_goes_with_its_run}; RETRACT_DUR_S; RETRACT_FADE_S	When all of a cohort's text is blanked with nothing replacing it (submit clear, relayout, split clear, shell Enter, partial then whole clear), the cohort is abandoned into its retract, coverage never rises and reaches 0 within 0.64 s without a cliff; a glyph the clear paints on a typed Space goes with it; a relocation leaving the old text intact moves only the mirror.	0,1,2,3,5,7	9
release-accounting	contract	Witness::walk (partial release); Ribbon::{release,melt}; ribbon_retired counter	A partial release stamps exactly the suffix past the last standing glyph (with its own trailing space) onto the retract once, keeps the cohort's clock and counts once; later blanks or new text under it melt fast without recounting; a rewritten or Backspace-pulled tail goes dark and later keys light only themselves.	0,1,2,3,7	8
torn-read-restore	contract	Engine::tick_with_restored_rows; Ribbon::{retire,build,restore_releases,hold_typed}; Witness::{walk,moved_text}	Identical text returning while its original run survives (blank-then-restore, torn reads, spinner tears) restores every partial-release stamp before the returning frame is planned, even after the visual melt and before another key; dark custody owns no field or geometry, expires with the original run without a pending wake, and is revoked by replaced glyphs or abandoning the run. A whole standing run's vertical move, or an inserted suffix's horizontal shift, carries its held identities too; a partial visible split guesses no destination. A blanked fragment still in place is never treated as moved.	0,1,2,3,7	14
ribbon-contiguity-invariant	invariant	Ribbon::{retract_suffix,shift_run,plan}; Witness::walk (span closure)	In every reachable ribbon each cohort's live cells on a row form one interval: every geometry path and release removes only a suffix or the whole run, so a band has no interior dark column across wraps and box growth.	0,1,2,7	6
typed-columns-owned	contract	CursorGlow::{tick,judge_observed_caret}; Engine::tick; Ribbon::{join_cohort,place,hold_typed,restore_releases}; Witness::walk	For every host trace with keys stamped as the GUI stamps them and repaints that either restore within the melt or edit, every typed column (letters and boundary spaces) is owned by a lit cell on every settled frame from the first key to the last, with no hole longer than HOLE_FRAMES_MAX, under coalescing, late echoes, pause repaints, fake cursor cells, hidden carets, torn reads and lapsed sync holds; nothing is drawn after the longest swoosh.	0,1,2,4,5,7,8,12	31
shift-pass-carries-insert	contract	Witness::shift_runs; Ribbon::{shift_run,insert_landing}; Engine::follow_rows	A key inserted inside a lit run moves the pushed tail's light by the inserted width with its clocks, in every repaint shape, and a hidden-caret composer's key lands at its column; the band stays whole.	0,1,2,3,7	2
one-walk-per-row	contract	Ribbon::{join_cohort,wake_origin,walk_origin_at,build}; meteor::LandingWalk	A cohort minted beside a live run takes its walk by position, a wake takes the freshest cohort's origin (cold only on an empty row), so landing marks wear the band they land on and no two lit neighbours step wider than 1/16.	0,1,2,3	3
revive-restores-stops	contract	CursorGlow::{hand_v2_typed_sweep,read_fresh_run_origin,remember_fold_landing}; ribbon::Revive; RecentTypedRun::origin	A key typed at a retired run's end within CHAIN_GAP_MAX re-lays the run at its old stops and continues one step past it, for late echoes, unseen runs, wrapped second rows and every shell fold; without a revive the key continues the walk and the prefix stays dark.	0,1,2,3,7	5
wake-corridor-host	contract	Ribbon::{wake,scrub}; WAKE_MAX_CELLS	Home, End or a word hop over a leaving band lays one corridor from the landing to the far edge (the cap binding only there), so no hole outlives three frames apart from the head's exit; an End over dark ground lays one interval to the island it re-owns.	0,1,2,3,7	4
wake-fold	contract	Ribbon::{fold_wakes_into_band,join_cohort,place}	A key at a hop wake's landing folds the wake into its band (never revives a retracting band), keeping one owner per column per cohort.	0,1,2,7	3
scrub-never-raises-light	contract	Ribbon::{scrub,wake}; CursorTrail::spawn (nav arm)	At the host seam a word hop or arrow over live or melting light raises no cell, lays nothing over a live band, leaves abandoned bands byte-identical, returns the census unchanged after a round trip, and the classic trail matches a twin told of no key.	0,1,2,4,7	6
licensed-insert-bridge	contract	CursorGlow::{hidden_bridge_source,visible_insert_echo}; spawn (insert arm); INSERT_GESTURE_HINT_FRESH; note_insert_armed; note_insert_delivered_from	hidden_bridge_source and visible_insert_echo share one predicate: a fresh Tab or a priced paste admits a reappearance within its reach and is laid whole in every frame shape; wider reappearances, stale Tabs and unlicensed program output lay nothing.	0,1,2,3,5	5
crossed-echo-laid	contract	Engine::{follow_rows,hand_rides,hand_line_follow,lift_hand}; Witness::{follow_hand,await_caret}; Ribbon::{lift_hand,lift_seam}; CursorGlow::{judge_observed_caret,lift_hand_row,tick}	An echo that crossed rows with the caret's run is read as the same-row advance it is in the text's frame and every typed glyph is laid on the new row: while a key is in flight the hand rides its line. The hand's own run (the cohort of its last typed glyph) is judged on its typed records alone, any folded wake riding with it, and a lone typed glyph stands with the caret shown on its new row as the second witness. That verdict moves the caret anchors, the hand's row, the mirror and the ribbon's hand-addressed members (caret, pen, arrival, floor, spaces, relocation, new-line gate) with the run, keeping the presses, before a caret shown one row away is judged, or when the follow pass carries the run while the caret is withheld; the pass carries the hand's run whether or not the general search names it, lifts at most once against the mirror it began with, arms the new-line seam only for other text leaving the hand's row as it stood before the lift, and carries the hand's line's own seam with it. Under a withheld caret, a lone glyph only the caret could follow gets no walk verdict until the caret shows. A keyless relayout lays nothing and moves the band; the earlier band stays lit, and the remembered typed run rides its line at any row distance (its glyphs at its columns, the caret at its end), so the next key revives a prefix whose cells have gone out.	0,1,2,3,4,7	17
rewrap-carries-word	contract	CursorGlow::{key_pushed_text_down,rewrapped_typed_run,soft_wrapped_caret,lifted_word,carried_word_left,ink_end_in_pane}; Ribbon::{moved_word_len,arrived,lift_word,settle_carry,flow_target,note_rewrap_start}	A key pushing a glued word down (or starting a word after a boundary space) in an unbordered, guttered or box-bordered composer is judged at once iff the source tail is erased (or preserved up to the space boundary) and its glyph stands left of the landing (otherwise a park keeps custody); the moved word is measured over its live typed cells back to the typed Space, never onto box borders or the indent; a lifted word goes up on its walk; a soft-wrapped word comes down only if its columns blanked; coalesced and stalled wraps relay the word whole.	0,1,2,3,4,7	24
typed-reanchor-no-phantom-vector	contract	CursorGlow::judge_observed_caret (typed re-anchor); hide bridge; note_repaint_blink; note_context	On the alt screen a typed cross-row re-anchor after a repaint blink lays no light along the jump vector in any style, and a frame catching the repaint hidden bridges nothing beyond Chebyshev 2.	0,1,2,3	4
rk-v2-steady-tick-contract	contract	rainbow_kitty::Engine::{tick,next_change_deadline,needs_frame_cadence,fingerprint}; CursorGlow::tick	Every tick emits at most MAX_QUADS quads and MAX_HALOS halos, warmed steady and idle ticks allocate nothing, and after the last key the engine reaches no deadline, no cadence and fp 0.	0,1,2,7,8	1
rk-ribbon-quad-share	contract	CursorGlow::under_quads; RAINBOW_RIBBON_QUAD_BUDGET; MAX_QUADS	On every tick the ribbon's under quads stay within RAINBOW_RIBBON_QUAD_BUDGET and the total under MAX_QUADS, at every geometry. On a glass frame (§15, Prism Glass 1.8) the glass's lights stay within its price less the lip's reserve (ribbon::glass_frame_price), inside RIBBON_QUAD_BUDGET, at every density it chooses.	0,1,2,7	1
trick-paths-allocation-free	contract	WordDecorations::{note_trick_typed,revoke_trick_flash,trick_flash_phase}; TrickListener	Once warmed, the trick flash and listener paths never allocate for any word input; a flash paints at most 32 cells.	0,1,2,7,8	2
output-streak-quad-bound	contract	OutputStreak::{tick,emit}; MAX_COMETS; TAIL_MAX	Each OutputStreak tick appends at most cols + MAX_COMETS (TAIL_MAX + 2) quads, independent of rows.	0,1,2,3,7	1
nova-draw-is-total	contract	aterm_render::Renderer::{draw_nova,render_input}; draw_flat_add	render_input draws every nova quad with no truncation, deterministically.	0,1,7,8,12	1
output-streak-attribution	refinement	OutputStreak::{note_output_cells,note_output,note_keystroke,rebase,tick}; aterm_models_effects::output_streak_attribution_model	note_output_cells refines OutputStreakAttribution: a frame is licensed iff fresh, undiscounted glyph content changed at fixed geometry and was not a moved row or a style-only repaint; geometry changes and rebases baseline silently; quads lie only in the changed run; viewport changes retire output.	0,1,2,5,7,10,12	17
output-streak-delivery-refines-model	refinement	OutputStreak; output synth; OutputStreak::next_change_deadline; aterm_models_effects::output_streak_episode_delivery_model	Every episode edge of the real streak and its synth refines OutputStreakEpisodeDelivery.	0,1,2,10	1
nova-phase-refines-model	refinement	nova::{phase,emit_nova}; aterm_models_effects::nova_phase_model	For every decodable duration the phase walk maps monotonically onto NovaPhase and the crown rises exactly once per Dip-to-Flash entry.	0,1,3,5,10	1
cursor-cat-refines-model	refinement	CursorCat; aterm_models_effects::{cursor_cat_model,cursor_cat_curse_wince_model}	The real CursorCat's frames project onto CursorCat model traces with every invariant held.	0,1,2,10	3
kitty-sing-activation-refines-model	refinement	KittySing; CursorCat singing bypass; aterm_models_effects::{kitty_sing_detector_model,cursor_cat_earn_floor_model}	The sing detector and the earn floor refine their models at every boundary, and the singing bypass still needs sixteen travel events.	0,1,2,10	3
ribbon-row-hold-refines-model	refinement	Ribbon row clocks; aterm_models_effects::ribbon_row_hold_model	Typing and erases renew only their own row (RibbonRowHold), and typing below never postpones an earlier row's exit.	0,1,2,10	2
console-life-episode-refines-model	refinement	PetBrain::{note_console_input,console_attention,console_anchor_id,console_input_seq,console_input_consumed_seq,console_event_seq,console_reason,tick}; aterm_models_effects::console_life_episode_model	Console state refines ConsoleLifeEpisode: echoes never replay, batched commits coalesce, input revokes a perch, stale results expire, ED2, ED3 and RIS clear what they should and nothing else, and erased ink never replays as contact.	0,1,2,10,12	8
console-z-order-refines-model	refinement	PetFrame::under_ink; PetBrain z-order hold; aterm_models_effects::console_pet_z_order_model	Every frame is a ConsolePetZOrder step: the pet is never painted over a glyph under its body, a burst changes layers at most twice, and blinking ink flips the held order once each way.	0,1,2,10,12	4
console-observation-admission	refinement	PetBrain::{sense_ink,tick,tick_static_capture,console_legacy_motion_pending}; aterm_models_effects::console_observation_admission_model	Failed and sparse observations refine ConsoleObservationAdmission: particles and one incoherent frame never send the pet far, invalid surfaces cannot materialize it, a real glyph still evicts it, protected surfaces hold, a real caret move is followed once coherent, and the idle ladder sleeps among particles.	0,1,2,10	11
pet-surface-seam-refines-model	refinement	CompanionOwner::{observe,prepare,tick,rescan,resight_owner,invalidate_colors}; Departure::stance0; PetDepartureSprite::id; aterm_models_effects::pet_surface_seam_model	At every coordinate-space boundary the owner's frames are a PetSurfaceSeam trace: the boundary frame is dark, then arriver and ghosts sum to about one cat, each ghost fading on its own glass in the look and palette it last drew and reaching zero as the arriver reaches full; incoherent frames hold then fade; sub-row scrolls change nothing.	0,1,2,4,10,12	16
pet-torn-frame-hold	refinement	TerminalFacts::read; CompanionOwner; PetBrain HIDDEN_HOLD_MAX hold; aterm_models_effects::console_cursor_home_model	Under the shipping contract a torn full repaint never moves or fades a settled pet for any bracket length, typing keeps it at the composer, and a finished frame's hidden caret is held within HIDDEN_HOLD_MAX then followed (ConsoleCursorHome).	0,1,2,10,12	7
pet-hidden-caret-bounded-hold	contract	PetBrain::tick (caret, caret_drawn); tennis watch; scroll compensation; flight birth	A hidden but known caret is followed after a bounded hold; the tennis watch never blocks travel; scroll compensation never lifts the body off the glass; a flight is not advanced on its birth tick.	0,1,2,3	2
pet-visibility-hold	contract	PetBrain visibility hold; PetFrame::alpha; set_console_presentable	Typing never blanks or cuts the pet; an output burst only fades it by bounded steps; with the console gate off there is no veto at all.	0,1,2,3,7	4
protected-surface-yield	contract	PetFrame::under_ink; PetBrain visibility veto; console placement pass	Over ordinary ink the body stays whole and renders under the glyphs; on a protected surface it moves off or goes to alpha 0: down with the rows round it where they moved down together (content_ride, whole rows, no column), up with the host's line it stands on where that line came up with the rows over it (line_ride), else to the nearest perch; a flight's arc is flattened (a half, a quarter, none) where it would rise into protected cells.	0,1,2	2
host-line-head-law	contract	PetBrain::{console_station,over_the_hosts_line,past_letters_above,line_ride}; PetWorld::{host_line,rule_line,window}; aterm-gui PetChrome (claude_footer holds its top rule whole)	No stand or seat puts the body under a host's line whose reserved cells would cover its head; while the person types, an escort's stand there is taken on top of the line at the caret's column (its head off the letters over that row, sliding right past them as beside the caret but no more than LINE_SLIDE_REACH columns, unless the cat already stands there on that row), as a perch whose body a hold keeps (its soles clear of the line by LINE_PERCH_GAP), and never with its body in a rule (PetWorld::rule_line); and the cat comes down to the caret's line where the line leaves room for its head; where the top of the line has no room (the card right over it, the first rows) the ladder seats it clear of the line. A cell window is the same at every row: an edge within 1e-4 of a cell boundary does not take the cell beyond.	0,1,2	6
escort-primacy	equivalence	PetBrain::{tick,set_console_presentable,station}; console_reason	Unless the console layer is genuinely resident, the presentable brain moves exactly like its plain-escort twin at caret + STATION_LEAD; a resident body settles within a cell of the escort.	0,1,2,4,5	6
escort-locomotion-speed-limit	contract	PetBrain chase and flight; console placement pass	The body moves at most 1.2 cells between grounded frames, never crosses the pane in a frame, and trails a typing caret by at most 4 cells.	0,1,2,3,7	3
pet-wake-liveness	contract	PetBrain::{needs_frames,next_change_deadline,pending_pets,console_resident_handoff_pending}	Whenever visible work is owed the brain asks for frames or a timely deadline, and when nothing is owed it asks for neither; stale completions start nothing, delayed ones get their full hold, flights keep their arcs, perches translate with scroll, and reduced-motion reading is opaque at once.	0,1,2,5,7	11
pet-no-nonfinite	no-panic	PetBrain::tick; PetFrame	For every sense (any u16 geometry including 0 and MAX, any caret) and tick sequence, the frame's position is finite and, when visible, within 64 cells of the grid.	0,1,2,3,7	1
pet-art-roster-table	table-property	pet_glyphs_gen pose table; PetGlyphId; cat_glyphs_codegen::quant	Over the generated pet roster every pose shares one viewbox and anchor set, stays inside it, planted poses stand on one ground line, every pose is within its layer and command budget with one dark outline first, and reading-gaze poses keep the perch body fixed.	0,1,7,14	6
pet-bake-properties	table-property	PetBaker::bake; CatBaker::host_tile; kitty_pet::{ART_ROWS,ART_ASPECT}; MOUTH_DETAIL_MIN_H	The pet's box fits the cat baker's slot at every metric, airborne gaits clear the ground, the yawn's gape survives the mouth cull, and contact beats have distinct silhouettes.	0,1,3,7	4
pack-decoders-denotation	equivalence	spec::{parse_custom_toml,compile_toy_pack_toml}	Each first-party decoder equals its schema's denotation (strict tables for packs, a lenient container with an untagged ink union for custom words), as recorded in the golden corpus.	0,1,5,7,9,12	3
sound-seam-independent-of-amplitude	contract	CursorGlow::{tick,sound_seam_open,cue_keystroke_kind,take_key_cue}; keyed_blaze; clear_sound_ledger; cue_move_sound	The key seam is open iff enabled and audible, whatever the intensity; key cues read this tick's thermal constants; a shed tick banks what a lit tick banks; unfocus, master off and reset wipe the ledger; the cue queue stays bounded while dark.	0,1,2,3	6
licence-is-precondition	equivalence	CursorGlow::tick; licence seam; GlowConfig::ribbon_flat	For every style and fully licensed script, the frames, counters, momentum and cues equal those with the licence gate forced open; earned light survives unlicensed moves; only a key's first move takes the licence; ribbon_flat collapses the comet terms.	0,1,2,4,5	4
word-deco-rekey-preserves-spent-guard	contract	WordDecorations::{rescan,tick}; Lexicon	A same-width redraw that moves a matched word is one episode: a spent supernova is not re-armed and the ink follows; a genuinely fresh occurrence still bursts.	0,1,2,7	1
minibuffer-gate	contract	seam::{echo_anchor_pass,glyph_before,refused_below_the_caret,hand_types_at_the_caret,hold_the_live_presses}; custody::forget_typed_credits; probe::{observe_print_anchor_glyph,observe_caret_drawn}	On the anchored lane, with a visible caret parked off the anchor row and either a witness glyph foreign to every live press or the hand typing on the caret row, the advance is refused program-row, lays nothing and spends nothing (forgetting the presses when the row is below and the hand is elsewhere, holding them when it types at the caret); otherwise the arm does nothing, so a hidden caret is judged as no caret. Over a whole replay no cell is laid on the search status row under a foreign glyph or at the program's fake cursor.	0,1,2,3,4,5,7	10
parked-caret-row-ownership	contract	seam::{echo_anchor_pass,spawn_judged,recall_torn_lay}; custody::{clear_typed,note_return,note_return_keeping_hand,note_motion,note_navigation,note_composer_inserting,hand_lands_on,lay_insert}; scroll::translate_row_addressed	After a generic input boundary, its source row cannot acquire visible typed ownership once the drawn caret was observed parked away from output without hand-owned typing there, even when a torn redisplay prints a live key's glyph. Cold setup alone establishes no such boundary, and no timer admits a displaced row. Established typing and anchored echoes retain their lanes; a licensed echo of the hand's input on the row (a key's or a delivered insert's) settles it until the next boundary, and a recalled lay hands it back; classified navigation or Return clears the ambiguity, including Enter into a foreground composer (which keeps hand_row), and so does the host's word that the composer takes typed text again (note_composer_inserting: an agent's vim INSERT after NORMAL), which licenses no move; generic input starts a new source-row observation, and coordinate moves carry it. A known program row's keyless or matching-glyph output stays dark without spending or forgetting the composer's pending key.	0,1,2,3,4,5,7	8
re-anchor-landing-gate	contract	seam::{spawn_judged,classify_move,probe_glyph_before,flush_held_park}; HeldPark::landing_cell; custody::forget_typed_credits	A typed re-anchor on the visible lane whose landing witness glyph is foreign to every live press lays nothing, restores last_licensed_row, forgets the presses and logs one program-row refusal; so a search match walked along its row carries no light.	0,1,2,3,4,5,7	2
soft-wrap-rejects-left-rewrite	contract	custody::CursorGlow::soft_wrapped_caret	A cross-row move is a soft-wrapped caret only if the origin row left of the origin is unchanged since its last sample; a rewritten prefix (zle's entry switch) is refused and then falls under re-anchor-landing-gate.	0,1,2,3,7	1
refused-presses-are-forgotten	invariant	custody::forget_typed_credits; seam::{echo_anchor_pass,spawn_judged}; CursorGlow::{note_typed_expected,clear_typed_keeping_hand,note_motion}	After a forgetting refusal no press banked before it ever pays for anything; later moves are licensed only by later presses or gestures, and a navigation key licenses its own hop.	0,1,2,5,7,10	1
own-glyph-echo-lights	contract	seam::{echo_anchor_pass,spawn,sweep_born}; CursorGlow::{tick,note_typed_expected,note_return}	A one-cell anchored advance witnessed with the press's own glyph (or on the caret row, or hidden, or glyph-less) within TYPE_HINT_FRESH is licensed and lays exactly that cell, born within 100 ms; Enter takes the hand off its row.	0,1,2,3,4,5,7	2
torn-lay-recall	contract	seam::CursorGlow::recall_torn_lay; torn_lay tracking; rainbow_kitty Event::Rewrite	A torn lay printed off its row, with the hand elsewhere and only its own cells right of the caret, is recalled once the caret has rested TORN_REST_S (a Rewrite back to the caret, one recall record, and the hand the lay put on its row and the doubt it settled there handed back while each stands as the lay left it, less a doubt a navigation key or Enter settled during the wait); a lift of the hand's line carries the lay and what it saved of that row; a boundary through clear_typed drops the lay unrecalled; a key press or a caret resting right of the lay never triggers it; recalled ink is gone within about 330 ms.	0,1,2,3,5,7,10	5
aos-lease-refines-model	refinement	aos_momentum::lease::Ledger::{join,hello,begin,assign,lease,result,progress,tick,lost,cancelled,void,finish,outcome,job,worker,counts,awaiting,next_deadline}; aterm_models_aos::aos_worker_lease_model	The M5b worker-lease ledger with one open job, two workers and max_attempts = MaxAttempts in {2, 1} refines AosWorkerLease (design §11.5, §11.7): the projection of the opened search (job state and attempts; each worker's state, lease epoch and Gone reason; the accepted claim and the late claim awaiting it as truth or lie; the outcome; the frames in flight, the one resend and the duplicate/stale/post-void/ignored ghosts kept by the harness) is the model's Init, and every real call is a model transition the model admits by the action's own name: lease and assign to an Idle worker; result as accept, compare, late result or drop; tick at a holder's deadline as ExpireX (the job Pending, or Exhausted and voided at the limit, or NoWorkers voided when no other worker is left) and at a drain's end as DrainTimeoutX; lost as LoseX; cancelled as AckX; void(Interrupted) as Cancel; finish as Finish. Each such call kills exactly the workers the model makes Gone and sends Cancel to exactly the holders it drains. The calls the model rules out leave the projection and the counters unchanged: a lease the model has no LeaseX for (a worker not Idle, no job Pending, a settled search), assign when it has neither, a tick before every deadline, a second void, an earlier now, and finish before completion. Every transition of the bounded space (1456 at MaxAttempts 2, 349 at 1) is taken this way, and each Buggy dial 1-6 is caught: the additive dial 1's step is refused by the real tick, and each of dials 2-6 makes the real step one the mutant does not admit while the mutant's own step breaks its law.	0,1,2,3,7,10,12	8
```

## 2. Test → spec map (TSV)

Paths are relative to `crates/aterm-effects/`. `primary` is the spec that retires the test; `also` lists other specs the same test samples. Tests of §4 are not listed here.

```tsv
file	test	primary	also
src/animal_baker.rs	bake_is_deterministic_and_exact_size	bake-tile-geometry	
src/cat_baker.rs	a_hit_hands_back_the_held_art_and_only_a_bake_bumps_the_version	catbaker-atlas-lru	
src/cat_baker.rs	a_host_tile_is_the_hosts_texels_at_its_size	catbaker-atlas-lru	
src/cat_baker.rs	a_slot_served_this_frame_is_never_evicted_under_its_own_quad	catbaker-atlas-lru	
src/cat_baker.rs	accessory_overlay_preserves_each_authored_aspect	accessory-transform	
src/cat_baker.rs	bake_budget_is_two_per_frame	catbaker-atlas-lru	
src/cat_baker.rs	context_outline_clears_wcag_contrast_at_every_band_edge	outline-contrast	
src/cat_baker.rs	context_palette_default_is_neutral_and_hue_families_are_cardinal	color-key-quantize	
src/cat_baker.rs	context_palette_is_bounded_and_color_aware	color-key-quantize	
src/cat_baker.rs	dark_average_footprint_keeps_the_pale_outline	color-key-quantize	
src/cat_baker.rs	dark_coats_are_lifted_on_dark_backgrounds_only	coat-lift	
src/cat_baker.rs	exterior_whiskers_follow_context_ink_but_internal_features_stay_fixed	resolve-layer	
src/cat_baker.rs	eyes_frame_bakes_distinct_but_open_is_authored	eyes-frame-transform	
src/cat_baker.rs	flying_reflection_roster_is_contained_and_nonvacuous	reflection-containment	
src/cat_baker.rs	get_v4_rejects_art_wider_than_slot_minus_strip	catbaker-atlas-lru	
src/cat_baker.rs	lru_evicts_oldest_and_version_bumps	catbaker-atlas-lru	
src/cat_baker.rs	metric_change_clears_and_bumps	catbaker-atlas-lru	
src/cat_baker.rs	mixed_background_bake_contains_both_keyline_inks	outline-contrast	
src/cat_baker.rs	mixed_background_dual_keyline_covers_every_luminance	outline-contrast	
src/cat_baker.rs	slot_count_is_structural	catbaker-slot-count	
src/cat_baker.rs	v4_bake_variant_is_deterministic	bake-tile-geometry	
src/cat_baker.rs	v4_coat_recolor_produces_distinct_tiles	coat-noninterference	
src/cat_baker.rs	v4_fixed_patterned_variant_is_coat_invariant	coat-noninterference	
src/cat_glyphs_codegen.rs	roster_schema_rejects_text_primitives_and_unknown_keys	codegen-schema-fail-closed	
src/classic_wake.rs	disabled_returns_the_engine_to_rest	cw-rest	
src/classic_wake.rs	reset_forgets_the_cursor_so_no_cross_space_comet_spawns	cw-rest	
src/classic_wake.rs	the_classic_wake_spawns_on_motion_alone_with_no_license	cw-motion-spawns	
src/classic_wake.rs	the_comet_run_pool_keeps_its_buffers_between_frames	cw-run-pool-monotone	
src/classic_wake.rs	the_mono_face_follows_the_configured_colour_and_the_spectrum_does_not	cw-color-faces	
src/classic_wake.rs	the_wake_decays_to_exactly_empty_and_disarms	cw-rest	
src/companion.rs	a_capture_spends_no_present_latch	owner-present-latches	
src/companion.rs	a_pointer_over_the_grid_is_sensed_and_one_off_the_grid_is_not	owner-sense-projection	
src/companion.rs	a_press_inside_the_body_plus_slop_pets_and_outside_passes	owner-press	
src/companion.rs	a_shed_edge_never_snaps_or_seats_a_moving_resident	shed-refines-model	
src/companion.rs	a_song_and_its_tail_keep_full_resident_custody	owner-custody	
src/companion.rs	a_static_capture_materialises_the_first_still	pet-still-postures	
src/companion.rs	a_torn_frames_hidden_cursor_reads_as_the_last_finished_caret	owner-sense-projection	
src/companion.rs	an_obscured_surface_keeps_the_pet_owned_but_off_glass	owner-custody	
src/companion.rs	blank_glass_keys_the_pets_contrast_off_the_cursor_colour	cat-color-key-footprint	
src/companion.rs	capture_and_reenable_do_not_leave_stale_static_scheduler_custody	owner-cadence-law	
src/companion.rs	cursor_companion_ignores_alternating_backgrounds_outside_its_footprint	cat-color-key-footprint	
src/companion.rs	cursor_companion_samples_its_actual_multiline_footprint	cat-color-key-footprint	
src/companion.rs	emit_draws_one_body_wearing_the_latched_pair_and_syncs_the_verdict	owner-emit-hello	
src/companion.rs	hidden_caret_keeps_exact_resident_body_custody_without_a_stale_cell	companion-duty-on-glass	
src/companion.rs	only_a_presented_resident_can_receive_pointer_strokes	owner-sense-projection	
src/companion.rs	peek_cues_reach_the_brain_as_fractional_cells	owner-note-peeks	
src/companion.rs	pet_hit_rect_win_offsets_the_body_by_the_effects_origin	pet-geometry-maps	
src/companion.rs	pet_mode_summons_exit_plain	pin-pet-mode-exit	
src/companion.rs	pointer_maps_into_pane_cells_and_dies_at_the_edge	pet-geometry-maps	
src/companion.rs	pre_tick_setup_binds_focused_ink_before_read_and_applies_species	pet-ink-binding	
src/companion.rs	reduced_motion_pins_the_pet_at_its_station	pet-still-postures	
src/companion.rs	resident_shed_model_proves_and_catches_the_snap	shed-refines-model	
src/companion.rs	retire_coordinate_space_keeps_the_command_latch_and_retire_owner_rebaselines_it	owner-present-latches	
src/companion.rs	same_session_change_is_a_wrap_and_a_session_switch_never_is	wrap-fact-edge	
src/companion.rs	single_and_composed_companions_share_continuous_shed_admission_and_alpha	shed-companion-alpha	
src/companion.rs	the_arrival_mapping_rations_the_theater_by_rung_and_difference	pet-arrival-mapping	
src/companion.rs	the_brain_ticks_even_when_undrawable_so_the_lane_is_released	pet-brain-settles	
src/companion.rs	the_completion_edge_is_handed_back_once_and_never_on_a_session_switch	owner-present-latches	
src/companion.rs	the_drain_is_a_fresh_brain_that_keeps_the_session_latches	owner-present-latches	
src/companion.rs	the_escort_caret_is_the_finished_caret_for_every_companion	owner-sense-projection	
src/companion.rs	an_unbracketed_repaint_s_write_position_is_no_caret	owner-sense-projection	
src/companion.rs	a_torn_frame_after_a_resize_reads_no_old_caret	owner-sense-projection	
src/companion.rs	the_hello_seam_commits_only_a_performed_ceremony_on_a_present	owner-emit-hello	
src/companion.rs	the_level_rules_retire_hide_and_preserve_the_resident_under_load	owner-custody	
src/companion.rs	the_owner_is_present_only_with_pet_mode_the_master_and_the_pet_style	companion-gates	
src/companion.rs	the_pointer_is_value_shadowed	owner-set-pointer	
src/companion.rs	the_shed_latch_takes_the_train_never_the_motion	owner-cadence-law	
src/companion.rs	typing_echo_never_reads_as_a_burst	companion-gates	
src/companion_look.rs	a_focus_move_to_another_session_re_derives_the_claim	claim-cache	
src/companion_look.rs	a_program_earns_the_cursor_only_after_tenure	tenure-gate	
src/companion_look.rs	a_stranger_spaced_by_the_floor_keeps_its_debt	roster-rate-law	
src/companion_look.rs	a_worn_cat_survives_a_long_heads_down_stretch	roster-rate-law	
src/companion_look.rs	a_worn_program_cat_lingers_and_a_relaunch_never_bounces	tenure-gate	
src/companion_look.rs	alternating_two_programs_greets_each_once	roster-rate-law	
src/companion_look.rs	an_ungreeted_slot_is_never_evicted	roster-rate-law	
src/companion_look.rs	companion_tenure_flicker_conformance_real_gate_holds_the_cat_still	tenure-flicker-refines-model	
src/companion_look.rs	poll_lands_a_due_candidate_and_disarms_without_a_present	tenure-gate	
src/companion_look.rs	poll_lands_the_roster_like_observe	roster-rate-law	
src/companion_look.rs	precedence_favourite_beats_program_beats_the_launch_kitty_floor	companion-precedence	
src/companion_look.rs	prompt_and_shells_claim_nothing_and_executing_names_the_program	claim-derive	
src/companion_look.rs	switching_programs_and_flapping_claims	tenure-gate	
src/companion_look.rs	the_base_cat_is_always_a_homecoming	roster-rate-law	
src/companion_look.rs	the_claim_caches_by_block_state_and_commandline_presence	claim-cache	
src/companions.rs	one_press_feeds_every_typed_detector_by_the_same_cascade	typed-press-cascade	
src/cursor_beam.rs	bar_rod_hugs_the_bar_and_stays_text_safe	cursor-body-quads	
src/cursor_beam.rs	disabled_or_reduced_motion_is_inert	cursor-body-inert	
src/cursor_beam.rs	rod_lands_at_the_window_origin	cursor-body-origin-translation	
src/cursor_beam.rs	settles_when_the_blaze_dies	cursor-body-activity	
src/cursor_comet.rs	blaze_swells_and_brightens	cursor-body-energy-monotone	
src/cursor_comet.rs	coma_surrounds_the_caret_in_a_padded_headed_window	cursor-body-origin-translation	
src/cursor_comet.rs	disabled_or_zero_intensity_is_inert	cursor-body-inert	
src/cursor_comet.rs	hidden_cursor_draws_no_coma	cursor-body-activity	
src/cursor_comet.rs	nucleus_follows_the_configured_hue	cursor-body-fill-ramp	
src/cursor_comet.rs	quads_respect_grid_hue_and_cap	cursor-body-quads	
src/cursor_comet.rs	settles_to_inactive_when_blaze_dies	cursor-body-activity	
src/cursor_droplet.rs	bead_surrounds_the_caret_in_a_padded_headed_window	cursor-body-origin-translation	
src/cursor_droplet.rs	disabled_or_zero_intensity_is_inert	cursor-body-inert	
src/cursor_droplet.rs	hidden_cursor_draws_no_bead	cursor-body-activity	
src/cursor_droplet.rs	quads_respect_grid_hue_and_cap	cursor-body-quads	
src/cursor_droplet.rs	settles_to_inactive_when_surge_dies	cursor-body-activity	
src/cursor_droplet.rs	surge_swells_and_brightens	cursor-body-energy-monotone	
src/cursor_fx.rs	every_effect_body_fades_to_the_terminal_cursor_through_one_seam	project-block-fill	
src/cursor_fx.rs	the_caret_style_law_is_one_truth_table	caret-style-law	
src/cursor_fx.rs	the_momentum_engine_yields_the_caret_cell_to_the_rainbow	momentum-glow-allowed	
src/cursor_fx.rs	the_momentum_glow_obeys_its_own_switch	momentum-master-gate	
src/cursor_glow/tests/band_move.rs	a_backspace_echo_that_lands_after_a_band_move_is_still_licensed	glow-band-move	
src/cursor_glow/tests/band_move.rs	a_cold_band_move_under_a_resting_caret_spawns_nothing	glow-no-licence-no-light	
src/cursor_glow/tests/band_move.rs	codex_streaming_keeps_the_ribbon_glued_to_the_sliding_composer	glow-band-move	
src/cursor_glow/tests/band_move.rs	the_codex_excursion_print_anchor_stays_inert	glow-band-move	
src/cursor_glow/tests/band_move.rs	translate_band_moves_last_and_last_visible_only_inside_the_band_and_drops_what_leaves	glow-band-move	
src/cursor_glow/tests/beam.rs	beam_jump_is_one_clean_full_vector_rod	beam-rod-morphology	
src/cursor_glow/tests/beam.rs	beam_parses_to_its_own_style	style-spelling-table	
src/cursor_glow/tests/beam.rs	beam_typing_never_goes_dark_mid_rhythm	ribbon-continuity	
src/cursor_glow/tests/cold_motion.rs	cold_program_motion_emits_no_rainbow_light	glow-no-licence-no-light	
src/cursor_glow/tests/cold_motion.rs	every_style_keeps_a_timestamp_only_relayout_dark	glow-no-licence-no-light	
src/cursor_glow/tests/cold_motion.rs	rainbow_ribbon_vanishes_without_a_visible_step	glow-idle-to-zero	
src/cursor_glow/tests/cold_motion.rs	synthetic_rainbow_moves_are_visible_and_cold_output_stays_dark	glow-licence-grant	
src/cursor_glow/tests/comet.rs	comet_bridge_attaches_window_absolute	glow-window-absolute	
src/cursor_glow/tests/comet.rs	comet_debris_born_window_absolute_with_chrome_origin	glow-window-absolute	
src/cursor_glow/tests/comet.rs	comet_light_respects_invariants_and_decays	glow-quad-invariants	
src/cursor_glow/tests/comet.rs	comet_parses_apart_from_beam_and_lumen	style-spelling-table	
src/cursor_glow/tests/erase.rs	a_bigger_erase_makes_a_bigger_cloud_not_a_thinner_one	erase-poof-shape	
src/cursor_glow/tests/erase.rs	a_plain_backspace_arms_the_host_exactly_like_a_kill	glow-scheduler-pacing	
src/cursor_glow/tests/erase.rs	a_plain_backspace_never_throws_the_kill_swoosh	erase-sound-cues	
src/cursor_glow/tests/erase.rs	a_re_rowed_kill_answers_once_and_a_stale_hint_never_does	erase-poof-licence	
src/cursor_glow/tests/erase.rs	a_witnessed_erase_releases_the_fallback_grace_early	erase-poof-licence	
src/cursor_glow/tests/erase.rs	an_unwitnessed_kill_still_serves_the_full_grace	erase-poof-licence	
src/cursor_glow/tests/erase.rs	cadence_predicate_separates_brisk_rainbow_from_coarse_ember	glow-scheduler-pacing	
src/cursor_glow/tests/erase.rs	ctrl_k_shrink_poofs_once	erase-poof-licence	
src/cursor_glow/tests/erase.rs	erase_poof_drive_grows_with_momentum_and_weight	erase-poof-drive	
src/cursor_glow/tests/erase.rs	ink_repaint_same_text_is_silent	erase-poof-licence	
src/cursor_glow/tests/erase.rs	kill_hint_paces_coarse	glow-scheduler-pacing	
src/cursor_glow/tests/erase.rs	kill_repeat_rate_limits_to_one_poof	erase-poof-licence	
src/cursor_glow/tests/erase.rs	no_op_backspace_poofs_nothing_but_a_real_one_does	erase-poof-licence	
src/cursor_glow/tests/erase.rs	rainbow_kill_poofs_fire_kill_steams	erase-poof-shape	
src/cursor_glow/tests/erase.rs	reflow_kill_poofs_at_the_caret	erase-poof-licence	
src/cursor_glow/tests/erase.rs	repaint_same_text_with_stale_hint_is_silent	erase-poof-licence	
src/cursor_glow/tests/erase.rs	scroll_fence_retires_plain_backspace_poof_provenance	erase-poof-licence	
src/cursor_glow/tests/erase.rs	the_shipping_default_puffs_a_cloud_on_every_delete_shape	erase-poof-shape	
src/cursor_glow/tests/fire.rs	emberforge_real_thermal_conforms_to_model	fire-thermal-refines-model	
src/cursor_glow/tests/fire.rs	emberforge_thermal_model_proves_and_catches_heating_quench	fire-thermal-refines-model	
src/cursor_glow/tests/fire.rs	fire_backspace_quenches_and_navigation_never_heats	fire-thermal-refines-model	
src/cursor_glow/tests/fire.rs	fire_embers_keep_off_probed_glyph_cells	fire-no-recolor	
src/cursor_glow/tests/fire.rs	fire_field_full_blaze_respects_quad_budget_with_head_band	glow-quad-invariants	
src/cursor_glow/tests/fire.rs	fire_forge_cursor_heats_holds_and_cools_to_ember	fire-forge-and-vapor	
src/cursor_glow/tests/fire.rs	fire_identity_at_origin_without_head	glow-quad-invariants	
src/cursor_glow/tests/fire.rs	fire_jump_flares_and_cools	fire-thermal-refines-model	
src/cursor_glow/tests/fire.rs	fire_light_drops_the_additive_crown	fire-no-recolor	
src/cursor_glow/tests/fire.rs	fire_light_is_radial_halos_not_squares	glow-quad-invariants	
src/cursor_glow/tests/fire.rs	fire_meteor_survives_an_unlicensed_second_hop_without_retargeting	jump-morphology	
src/cursor_glow/tests/fire.rs	fire_rises_into_the_chrome_band	glow-quad-invariants	
src/cursor_glow/tests/fire.rs	fire_row_change_snuffs_the_old_lines_trail	jump-morphology	
src/cursor_glow/tests/fire.rs	frame_fingerprint_preserves_private_stream_identity_and_idle_zero	frame-fingerprint	
src/cursor_glow/tests/fire.rs	halo_changes_change_the_fingerprint	frame-fingerprint	
src/cursor_glow/tests/fire.rs	hot_blaze_emits_halo_strengths_rising_toward_the_head_and_never_recolors	fire-no-recolor	
src/cursor_glow/tests/fire.rs	hot_cursor_smokes_and_cool_cursor_stops	fire-forge-and-vapor	
src/cursor_glow/tests/fire.rs	light_theme_fire_is_ink_tongues_beneath_the_letters	fire-no-recolor	
src/cursor_glow/tests/fire.rs	towering_blaze_chars_the_engulfed_rows_and_decays	fire-no-recolor	
src/cursor_glow/tests/fire.rs	typing_wrap_no_meteor_deliberate_enter_still_meteors	jump-morphology	
src/cursor_glow/tests/heat_and_laser.rs	heat_cools_after_a_pause	glow-heat-momentum	
src/cursor_glow/tests/heat_and_laser.rs	laser_jump_strikes_lightning	laser-and-sparkle-bursts	
src/cursor_glow/tests/heat_and_laser.rs	laser_navigation_forges_no_strike	glow-no-licence-no-light	
src/cursor_glow/tests/heat_and_laser.rs	sustained_typing_outshines_relaxed_typing	glow-heat-momentum	
src/cursor_glow/tests/insert.rs	a_delivered_insert_brands_no_row	insert-effects	
src/cursor_glow/tests/insert.rs	a_delivered_insert_builds_no_momentum_and_cues_nothing	insert-effects	
src/cursor_glow/tests/insert.rs	a_delivered_insert_is_inert_for_a_program_row	insert-licence	
src/cursor_glow/tests/insert.rs	a_delivered_insert_lays_its_span_as_one_sweep_under_a_visible_caret	insert-effects	
src/cursor_glow/tests/insert.rs	a_delivered_insert_lights_a_hidden_caret_tui_at_its_print_anchor	insert-effects	
src/cursor_glow/tests/insert.rs	a_delivered_insert_mints_nothing_in_every_other_style	insert-effects	
src/cursor_glow/tests/insert.rs	a_delivered_insert_survives_a_hidden_boundary_while_fresh_and_is_revoked_at_its_own_instant	insert-licence	
src/cursor_glow/tests/insert.rs	a_program_flood_of_the_inserts_width_with_no_delivery_stamp_still_declines	insert-licence	
src/cursor_glow/tests/insert.rs	a_receipt_that_lands_after_the_echoing_frame_still_lights_the_insert	insert-licence	
src/cursor_glow/tests/insert.rs	a_stale_or_spent_delivery_stamp_licenses_nothing	insert-licence	
src/cursor_glow/tests/insert.rs	an_insert_and_a_key_in_one_frame_pay_the_surplus_from_the_press	insert-licence	
src/cursor_glow/tests/insert.rs	the_placeholder_rewrite_re_witnesses_its_row_and_keeps_the_ribbon_whole	ribbon-text-custody	
src/cursor_glow/tests/insert.rs	the_placeholder_rewrite_retracts_the_ribbon_to_the_new_caret_like_a_kill	ribbon-text-custody	
src/cursor_glow/tests/insert_witness.rs	a_dispatch_time_insert_arm_claims_no_hop_refused_before_the_key	insert-licence	
src/cursor_glow/tests/insert_witness.rs	a_first_action_drop_with_a_late_receipt_is_retro_licensed_and_unbrands_its_row	insert-effects	
src/cursor_glow/tests/insert_witness.rs	a_late_receipt_claims_no_hop_refused_before_its_dispatch	insert-licence	
src/cursor_glow/tests/insert_witness.rs	a_licensed_insert_s_hidden_to_visible_reappearance_is_laid_as_the_insert	insert-licence	
src/cursor_glow/tests/insert_witness.rs	a_parked_carets_insert_witness_is_the_anchored_input_row_not_the_parked_row	insert-licence	
src/cursor_glow/tests/insert_witness.rs	a_priced_paste_the_width_of_the_gesture_cap_is_not_the_unknown_class	insert-licence	
src/cursor_glow/tests/insert_witness.rs	a_program_hop_after_a_tab_s_gesture_window_stays_dark	insert-licence	
src/cursor_glow/tests/insert_witness.rs	a_revoked_insert_arm_undoes_its_delivered_tally_tick	insert-licence	
src/cursor_glow/tests/insert_witness.rs	a_revoked_tab_restores_the_paste_credit_it_accumulated_into	insert-licence	
src/cursor_glow/tests/insert_witness.rs	a_row_branded_before_the_arm_refuses_even_a_whole_width_match	insert-row-identity	
src/cursor_glow/tests/insert_witness.rs	a_spinner_past_the_typed_cap_beside_a_fresh_insert_stays_dark	insert-licence	
src/cursor_glow/tests/insert_witness.rs	a_spinner_row_advancing_after_a_tab_stays_dark	insert-licence	
src/cursor_glow/tests/insert_witness.rs	a_tabs_insert_stamp_is_bound_to_the_row_the_hand_was_on	insert-licence	
src/cursor_glow/tests/insert_witness.rs	a_visible_caret_on_the_established_echo_row_is_not_parked_by_a_transcript_print	insert-licence	
src/cursor_glow/tests/insert_witness.rs	an_insert_witness_rides_a_band_move_and_the_row_it_left_spends_nothing	glow-band-move	
src/cursor_glow/tests/insert_witness.rs	an_unknown_width_receipt_that_lands_late_claims_the_hop_by_its_row_witness	insert-licence	
src/cursor_glow/tests/insert_witness.rs	insert_row_identity_is_the_witnessed_row_or_a_priced_whole_width	insert-row-identity	
src/cursor_glow/tests/insert_witness.rs	repeated_unknown_inserts_do_not_widen_the_bound_past_the_gesture_cap	insert-licence	
src/cursor_glow/tests/insert_witness.rs	two_receipts_straddling_the_frame_claim_the_refused_hop_together	insert-licence	
src/cursor_glow/tests/licence.rs	an_unlicensed_move_mints_nothing_and_retires_nothing_in_every_style	glow-no-licence-no-light	
src/cursor_glow/tests/licence.rs	content_only_probes_never_license_a_kill_poof	glow-no-licence-no-light	
src/cursor_glow/tests/licence.rs	rainbow_ribbon_survives_a_three_per_frame_mash	glow-licence-grant	
src/cursor_glow/tests/licence.rs	rainbow_ribbon_survives_word_boundaries_and_keeps_its_head	glow-licence-grant	
src/cursor_glow/tests/licence.rs	the_real_glow_engine_conforms_to_the_licence_model	glow-licence-refines-model	
src/cursor_glow/tests/licence.rs	typing_paints_through_the_esc7_esc8_streamer_cadence	glow-licence-grant	
src/cursor_glow/tests/lifecycle.rs	conpty_hide_gap_still_spawns_the_typing_spark	hide-bridge	
src/cursor_glow/tests/lifecycle.rs	every_style_keeps_the_invariants	glow-quad-invariants	
src/cursor_glow/tests/lifecycle.rs	hidden_far_teleport_stays_suppressed	hide-bridge	
src/cursor_glow/tests/lifecycle.rs	jump_spawns_light_and_fades_to_empty	glow-idle-to-zero	
src/cursor_glow/tests/lifecycle.rs	no_light_until_a_move	glow-idle-to-zero	
src/cursor_glow/tests/lifecycle.rs	typing_chain_never_goes_dark	ribbon-continuity	
src/cursor_glow/tests/momentum.rs	composed_physical_margin_fold_keeps_ribbon_material_and_kitty_direction	ribbon-text-custody	
src/cursor_glow/tests/momentum.rs	composed_wide_wrap_folds_at_the_focused_pane_bounds	ribbon-text-custody	
src/cursor_glow/tests/momentum.rs	cursor_cat_motion_pulse_preserves_licensed_fold_shape	motion-pulse-fold	
src/cursor_glow/tests/momentum.rs	momentum_unifies_glow_and_cat_metrics	typing-momentum-unified	
src/cursor_glow/tests/momentum.rs	program_output_alone_builds_no_momentum	glow-heat-momentum	
src/cursor_glow/tests/park.rs	a_box_growth_wrap_reanchors_identically_under_the_park_rule	glow-park	
src/cursor_glow/tests/park.rs	a_held_park_rides_a_band_move_or_a_scroll_and_its_return_lands_on_the_moved_row	glow-park	
src/cursor_glow/tests/park.rs	a_held_parks_return_is_recognised_on_the_anchored_lane	glow-park	
src/cursor_glow/tests/park.rs	a_late_park_flush_cannot_spend_a_stamp_banked_after_the_park	glow-park	
src/cursor_glow/tests/park.rs	a_one_cell_park_flushed_as_typing_drains_no_cell_of_the_band	ribbon-text-custody	
src/cursor_glow/tests/park.rs	a_park_flushed_after_its_window_lights_no_blank_landing	glow-park	
src/cursor_glow/tests/park.rs	a_park_flushed_before_a_scroll_or_band_move_lays_in_the_moved_rows	glow-park	
src/cursor_glow/tests/park.rs	a_park_followed_by_a_backspace_flushes_before_the_erase	glow-park	
src/cursor_glow/tests/park.rs	a_park_returning_to_its_own_origin_is_not_a_re_anchor	glow-park	
src/cursor_glow/tests/park.rs	a_park_with_no_return_flushes_as_todays_verdict	glow-park	
src/cursor_glow/tests/park.rs	a_short_retreat_beside_a_fresh_stamp_keeps_the_hide_bridge_verdict	glow-park	
src/cursor_glow/tests/park.rs	an_ink_park_after_a_stall_is_held_on_the_in_flight_pool_and_its_return_sweeps_the_batch	glow-park	
src/cursor_glow/tests/park.rs	an_ink_rewrite_observed_as_park_and_return_is_one_typed_echo_per_key	glow-park	
src/cursor_glow/tests/park.rs	an_insert_echoed_through_an_ink_park_is_laid	glow-park	
src/cursor_glow/tests/phaser.rs	disabled_or_zero_intensity_emits_nothing	glow-disabled-inert	
src/cursor_glow/tests/phaser.rs	light_theme_band_is_ink_not_additive	light-theme-veils	
src/cursor_glow/tests/phaser.rs	phaser_band_spans_three_letters_and_never_parks	ribbon-continuity	
src/cursor_glow/tests/phaser.rs	phaser_burst_origin_cannot_outlive_the_band	ribbon-continuity	
src/cursor_glow/tests/phaser.rs	vertical_navigation_timestamps_stay_dark	glow-no-licence-no-light	
src/cursor_glow/tests/phaser.rs	wide_coalesced_echo_still_sweeps_every_cell	jump-morphology	
src/cursor_glow/tests/phaser.rs	wrap_echo_continues_the_band_across_the_fold	jump-morphology	
src/cursor_glow/tests/rainbow.rs	a_caret_that_advances_with_no_unpaid_press_still_buys_nothing	glow-no-licence-no-light	
src/cursor_glow/tests/rainbow.rs	a_late_repaint_lays_every_cell_the_hand_typed	glow-exact-payment	
src/cursor_glow/tests/rainbow.rs	a_refused_rainbow_sweep_names_the_credit_budget_that_refused_it	glow-exact-payment	
src/cursor_glow/tests/rainbow.rs	a_wide_glyph_echo_pays_for_the_cells_its_own_glyphs_occupy	glow-exact-payment	
src/cursor_glow/tests/rainbow.rs	a_wide_glyphs_single_press_sweeps_its_continuation_cell	glow-exact-payment	
src/cursor_glow/tests/rainbow.rs	every_kitty_spelling_draws_the_resident_and_flying_stays_reachable	style-spelling-table	
src/cursor_glow/tests/rainbow.rs	rainbow_coalesced_typing_sweeps_every_skipped_cell	ribbon-continuity	
src/cursor_glow/tests/rainbow.rs	rainbow_decays_to_empty_when_idle	glow-idle-to-zero	
src/cursor_glow/tests/rainbow.rs	rainbow_first_key_orphan_does_not_outlive_the_ribbon	ribbon-continuity	
src/cursor_glow/tests/rainbow.rs	rainbow_kitty_parse_and_beam	style-spelling-table	
src/cursor_glow/tests/rainbow.rs	rainbow_ribbon_brightens_with_typing_momentum	glow-heat-momentum	
src/cursor_glow/tests/rainbow.rs	rainbow_ribbon_spans_four_letters_at_any_rhythm	ribbon-continuity	
src/cursor_glow/tests/rainbow.rs	rainbow_sequential_and_coalesced_typing_lay_equivalent_hues	ribbon-hue-equivalence	
src/cursor_glow/tests/rainbow.rs	rainbow_typed_hide_bridge_spans_the_batch	hide-bridge	
src/cursor_glow/tests/rainbow.rs	rainbow_typed_row_has_no_interior_gaps	ribbon-continuity	
src/cursor_glow/tests/rainbow.rs	rainbow_typing_wrap_folds_around_the_line_end	ribbon-continuity	
src/cursor_glow/tests/rainbow.rs	the_brightest_pixel_in_the_frame_is_under_the_cursor	ribbon-geometry-and-caret-primacy	
src/cursor_glow/tests/reanchor.rs	box_growth_preview_reanchors_but_cold_output_stays_dark	jump-morphology	
src/cursor_glow/tests/reanchor.rs	bridged_two_cell_move_is_byte_identical_with_hint	reanchor-guard-equivalence	
src/cursor_glow/tests/reanchor.rs	claude_code_wrap_reanchors_all_styles	jump-morphology	
src/cursor_glow/tests/reanchor.rs	synthetic_alt_without_blink_keeps_the_jump	jump-morphology	
src/cursor_glow/tests/resident_cap.rs	cursor_resident_cap_derived_model_proves_and_catches_missing_clamp	glow-resident-caps	
src/cursor_glow/tests/resident_cap.rs	cursor_resident_cap_real_spawn_conforms_to_model	glow-resident-caps	
src/cursor_glow/tests/review.rs	a_backspace_into_an_ime_run_retires_one_glyphs_credit	glow-backspace-retires-newest	
src/cursor_glow/tests/review.rs	a_backspace_retires_only_the_newest_in_flight_press	glow-backspace-retires-newest	
src/cursor_glow/tests/review.rs	a_batch_draining_inside_the_backspaces_quench_window_is_the_survivors_echo	glow-exact-payment	
src/cursor_glow/tests/review.rs	a_fast_correction_inside_one_frame_gap_lays_its_survivors	glow-exact-payment	
src/cursor_glow/tests/review.rs	a_fold_after_a_paid_pending_wrap_glyph_does_not_charge_the_last_column_twice	glow-exact-payment	
src/cursor_glow/tests/review.rs	a_forget_edge_that_finds_only_dead_presses_counts_nothing	glow-forget-edges	
src/cursor_glow/tests/review.rs	a_glyph_echo_does_not_spend_the_enter_pressed_behind_it	glow-one-shot-once	
src/cursor_glow/tests/review.rs	a_hidden_caret_reappearing_at_the_end_of_a_stalled_batch_is_the_batch_echo	glow-exact-payment	
src/cursor_glow/tests/review.rs	a_hidden_caret_tui_batch_after_a_stall_is_laid_from_the_print_anchor_and_does_not_brand_the_row	glow-program-row-never-spends	
src/cursor_glow/tests/review.rs	a_key_typed_as_the_stalled_frame_lands_keeps_its_own_echo	glow-exact-payment	
src/cursor_glow/tests/review.rs	a_keyed_motion_pair_is_not_a_park_return	glow-park	
src/cursor_glow/tests/review.rs	a_kill_forgets_the_in_flight_presses_at_the_key	glow-forget-edges	
src/cursor_glow/tests/review.rs	a_park_survives_a_hidden_caret_boundary_in_the_gap_before_its_return	glow-park	
src/cursor_glow/tests/review.rs	a_pending_wrap_print_under_a_hidden_caret_is_shown_on_the_pane_edge	glow-exact-payment	
src/cursor_glow/tests/review.rs	a_press_the_tty_will_not_echo_banks_nothing_for_a_same_row_program_hop	glow-no-licence-no-light	
src/cursor_glow/tests/review.rs	a_program_row_advancing_while_presses_are_in_flight_is_not_the_echo	glow-program-row-never-spends	
src/cursor_glow/tests/review.rs	a_reappearance_over_an_anchored_run_keeps_the_in_flight_presses	glow-forget-edges	
src/cursor_glow/tests/review.rs	a_return_licensed_move_and_a_row_change_forget_the_in_flight_credits	glow-forget-edges	
src/cursor_glow/tests/review.rs	a_scroll_or_a_band_move_keeps_the_in_flight_pool	glow-forget-edges	
src/cursor_glow/tests/review.rs	a_spinner_crossing_inside_a_hidden_park_gap_cannot_steal_the_keys_stamp	glow-program-row-never-spends	
src/cursor_glow/tests/review.rs	a_stall_longer_than_the_patience_is_refused	glow-no-licence-no-light	
src/cursor_glow/tests/review.rs	a_stalled_batch_drained_across_two_frames_keeps_its_tail_on_the_ledger	glow-exact-payment	
src/cursor_glow/tests/review.rs	a_stalled_batch_draining_across_the_wrap_wider_than_the_tight_fold_is_paid_exactly	glow-exact-payment	
src/cursor_glow/tests/review.rs	a_swallowed_batch_cannot_fund_a_program_hop_after_an_unexplained_move	glow-no-licence-no-light	
src/cursor_glow/tests/review.rs	a_typed_reanchor_spends_its_credit_so_a_keyless_plus_one_after_it_is_refused	glow-exact-payment	
src/cursor_glow/tests/review.rs	a_wide_drain_inside_the_quench_window_is_not_a_backspace_fold	glow-exact-payment	
src/cursor_glow/tests/review.rs	an_insert_delivered_into_a_stall_lays_the_keys_behind_it_once	glow-exact-payment	
src/cursor_glow/tests/review.rs	an_unpaid_credit_survives_a_hidden_boundary_for_the_whole_patience	glow-forget-edges	
src/cursor_glow/tests/review.rs	mid_line_typing_under_a_hidden_caret_is_shown_where_the_glyph_put_the_caret	glow-park	
src/cursor_glow/tests/review.rs	the_coalesced_fold_on_the_alt_screen_is_admitted_under_a_fresh_blink	glow-exact-payment	
src/cursor_glow/tests/rings.rs	classic_landing_ring_expands_wider_with_the_jump	glow-landing-ring-raster	
src/cursor_glow/tests/rings.rs	cold_navigation_timestamp_scatters_nothing	glow-no-licence-no-light	
src/cursor_glow/tests/rings.rs	halo_centres_past_the_box_stay_true_and_negative_centres_cull	glow-landing-ring-raster	
src/cursor_glow/tests/rings.rs	the_dark_landing_ring_is_a_round_rim_over_a_soft_bloom_in_its_row	glow-landing-ring-raster	
src/cursor_glow/tests/rings.rs	rainbow_landing_ring_light_theme_forks_to_source_over	glow-landing-ring-raster	
src/cursor_glow/tests/seam_laws.rs	a_caret_that_moved_behind_a_key_keeps_the_lay	glow-hand-row-hold	
src/cursor_glow/tests/seam_laws.rs	a_classic_erase_key_leaves_no_standing_wake	glow-idle-to-zero	
src/cursor_glow/tests/seam_laws.rs	a_composer_newline_starts_the_walk_again_however_soon_the_next_glyph_follows	glow-typed-cell-lit	
src/cursor_glow/tests/seam_laws.rs	a_cross_row_park_sleeps_until_its_own_expiry	glow-park	
src/cursor_glow/tests/seam_laws.rs	a_dangling_stamp_does_not_hold_the_enter_open_for_a_keyless_plus_one	glow-no-licence-no-light	
src/cursor_glow/tests/seam_laws.rs	a_flushed_park_is_witnessed_by_the_glyph_it_was_held_over	glow-park	
src/cursor_glow/tests/seam_laws.rs	a_foreign_landing_restores_the_prior_licensed_hand_row	glow-program-row-never-spends	
src/cursor_glow/tests/seam_laws.rs	a_glyph_echo_landing_after_an_arrow_is_the_glyphs_not_the_arrows	glow-forget-edges	
src/cursor_glow/tests/seam_laws.rs	a_held_park_is_a_wake_source	glow-park	
src/cursor_glow/tests/seam_laws.rs	a_key_after_the_walk_back_keeps_the_recall_and_hands_back_only_the_lay_s_own	glow-hand-row-hold	
src/cursor_glow/tests/seam_laws.rs	a_key_queued_behind_an_unknown_insert_is_retired_with_the_hop_that_echoed_it	glow-forget-edges	
src/cursor_glow/tests/seam_laws.rs	a_key_whose_echo_lands_a_frame_late_lays_no_cell_left_of_the_caret	glow-typed-cell-lit	
src/cursor_glow/tests/seam_laws.rs	a_keyless_wide_hop_under_a_one_shot_licence_lights_no_landing_on_a_dangling_stamp	glow-no-licence-no-light	
src/cursor_glow/tests/seam_laws.rs	a_kill_reaches_v2_once_whichever_witness_comes_first	glow-kill-once-v2	
src/cursor_glow/tests/seam_laws.rs	a_kill_retreat_relays_nothing_under_the_prompt	glow-kill-once-v2	
src/cursor_glow/tests/seam_laws.rs	a_lay_with_no_live_cell_is_not_recalled	glow-hand-row-hold	
src/cursor_glow/tests/seam_laws.rs	a_lift_during_the_wait_carries_the_lay_s_saved_hand_and_doubt	torn-lay-recall	
src/cursor_glow/tests/seam_laws.rs	a_lifted_word_is_not_refused_by_the_landing_gate	glow-landing-glyph-gate	
src/cursor_glow/tests/seam_laws.rs	a_mid_line_park_is_not_released_by_the_tails_re_lay	glow-park	
src/cursor_glow/tests/seam_laws.rs	a_one_shot_licensed_plus_one_lays_no_typed_cell	glow-no-licence-no-light	
src/cursor_glow/tests/seam_laws.rs	a_recall_at_the_next_key_leaves_that_key_s_hand_on_its_row	glow-hand-row-hold	
src/cursor_glow/tests/seam_laws.rs	a_soft_wrapped_re_anchor_is_witnessed_at_the_origin_it_lays	glow-landing-glyph-gate	
src/cursor_glow/tests/seam_laws.rs	a_stamp_banks_split_and_merge_match_one_by_one_banking_at_every_fill	typed-stamps-split-merge	
src/cursor_glow/tests/seam_laws.rs	a_superseding_key_cancels_a_composer_home_even_without_a_move	glow-one-shot-once	
src/cursor_glow/tests/seam_laws.rs	a_typed_re_anchor_lays_its_landing_only_under_a_live_press_s_glyph	glow-landing-glyph-gate	
src/cursor_glow/tests/seam_laws.rs	a_wide_committed_text_lays_at_most_the_sweep_cap	glow-exact-payment	
src/cursor_glow/tests/seam_laws.rs	a_zero_row_or_zero_column_grid_ticks_dark_without_panic	glow-degenerate-geom-no-panic	
src/cursor_glow/tests/seam_laws.rs	the_bands_walk_is_the_hands_whatever_the_echos_lag	glow-typed-cell-lit	
src/cursor_glow/tests/seam_laws.rs	the_forge_ember_keeps_the_host_awake_while_the_metal_heats	glow-idle-to-zero	
src/cursor_glow/tests/seam_laws.rs	the_glyph_before_a_cell_is_the_host_s_for_its_own_anchor_else_this_frame_s_probe	glow-landing-glyph-gate	
src/cursor_glow/tests/seam_laws.rs	the_glyph_gate_refuses_only_a_known_foreign_glyph	press-credits-foreign-to-live	
src/cursor_glow/tests/sound.rs	a_coalesced_echos_surviving_stamps_license_nothing_once_its_presses_are_paid	glow-no-licence-no-light	
src/cursor_glow/tests/sound.rs	a_key_whose_echo_jumps_still_clicks_exactly_once	glow-key-time-click-ledger	
src/cursor_glow/tests/sound.rs	a_keyless_hop_the_in_flight_pool_cannot_pay_for_lights_no_landing	glow-no-licence-no-light	
src/cursor_glow/tests/sound.rs	a_paste_strums_under_the_music_box_and_is_silent_under_the_nine	glow-cue-classification	
src/cursor_glow/tests/sound.rs	a_synthetic_gesture_jump_speaks_but_cold_output_is_dark	glow-no-licence-no-light	
src/cursor_glow/tests/sound.rs	admitted_spawn_arms_handle_the_key_time_ledger	glow-key-time-click-ledger	
src/cursor_glow/tests/sound.rs	flood_presses_bank_licenses_for_every_echo_sweep	glow-exact-payment	
src/cursor_glow/tests/sound.rs	key_time_click_fires_at_the_key_and_mutes_its_own_echo	glow-key-time-click-ledger	
src/cursor_glow/tests/sound.rs	key_time_click_is_silent_wherever_the_sound_gates_close	glow-key-time-click-ledger	
src/cursor_glow/tests/sound.rs	key_time_credits_expire_instead_of_banking_silence	glow-key-time-click-ledger	
src/cursor_glow/tests/sound.rs	newest_erase_key_owns_the_poof_voice	glow-cue-classification	
src/cursor_glow/tests/sound.rs	rapid_synthetic_line_feeds_cue_jump_and_typed_gestures	glow-cue-classification	
src/cursor_glow/tests/sound.rs	resume_after_pause_lays_no_interior_black_cell	glow-typed-cell-lit	
src/cursor_glow/tests/sound.rs	sound_cues_mirror_licensed_spawn_classification	glow-cue-classification	
src/cursor_glow/tests/sound.rs	take_key_cue_hands_back_the_click_without_touching_the_ledger	glow-key-time-click-ledger	
src/cursor_glow/tests/sound.rs	the_classic_wake_and_an_empty_grid_obey_the_one_key_seam_law	glow-key-time-click-ledger	
src/cursor_glow/tests/sound.rs	the_key_time_click_carries_its_own_keystrokes_heat	glow-key-time-heat	
src/cursor_glow/tests/stall.rs	a_batch_whose_last_key_is_three_seconds_old_is_lit_in_the_echoing_frame	glow-exact-payment	
src/cursor_glow/tests/stall.rs	a_hidden_caret_single_key_stall_relights_on_its_own_anchored_echo	glow-program-row-never-spends	
src/cursor_glow/tests/stall.rs	a_single_stalled_press_wider_than_one_cell_is_still_refused_and_kept	glow-no-licence-no-light	
src/cursor_glow/tests/stall.rs	a_swallowed_press_buys_at_most_its_own_one_cell_and_only_once	glow-no-licence-no-light	
src/cursor_glow/tests/stall.rs	one_key_stalled_three_seconds_lights_its_own_cell_on_the_late_echo	glow-exact-payment	
src/cursor_glow/tests/stall.rs	thirty_keys_typed_into_a_stall_are_laid_as_one_sweep_when_the_row_echoes	glow-exact-payment	
src/cursor_glow/tests/stall_fold.rs	a_deferred_wrap_echo_after_a_stall_lays_both_fold_cells	glow-exact-payment	
src/cursor_glow/tests/stall_fold.rs	a_pending_wrap_glyph_at_the_last_column_lights_on_its_own_anchored_echo	glow-exact-payment	
src/cursor_glow/tests/stall_fold.rs	a_per_key_fold_lays_the_same_cells_under_the_origin_row_sweep	glow-exact-payment	
src/cursor_glow/tests/stall_fold.rs	a_program_newline_with_no_press_in_flight_lays_nothing	glow-no-licence-no-light	
src/cursor_glow/tests/stall_fold.rs	a_stalled_fold_is_born_no_earlier_than_the_birth_floor	glow-typed-cell-lit	
src/cursor_glow/tests/stall_fold.rs	a_stalled_key_whose_echo_wraps_the_row_lights_the_last_column	glow-exact-payment	
src/cursor_glow/tests/status.rs	a_celebration_freezes_the_combo_so_a_held_key_cannot_farm_it	glow-status-row	
src/cursor_glow/tests/status.rs	a_word_typed_at_human_cadence_earns_the_full_rainbow_ribbon	glow-status-row	
src/cursor_glow/tests/status.rs	the_status_row_names_every_gate_and_derives_the_ribbon_verdict	glow-status-row	
src/cursor_glow/tests/status.rs	the_status_row_names_who_owns_the_block_cursor	glow-status-row	
src/cursor_glow/tests/status.rs	the_trail_row_reports_the_flow_the_engine_is_in	glow-status-row	
src/cursor_glow/tests/status.rs	unauthored_output_is_declined_and_the_status_verb_shows_it_dark	glow-no-licence-no-light	
src/cursor_glow/tests/stray.rs	a_held_word_key_at_the_line_start_licenses_no_relocation_the_repaint_makes	glow-nav-bridge-refines-reach	
src/cursor_glow/tests/stray.rs	a_nav_key_that_moved_nothing_licenses_no_cross_row_relocation_at_any_repaint_phase	glow-nav-bridge-refines-reach	
src/cursor_glow/tests/stray.rs	a_refused_stray_leaves_the_word_key_its_own_licence	glow-one-shot-once	
src/cursor_glow/tests/stray.rs	a_same_row_landing_inside_the_echo_window_is_ctrl_a_s_shape_and_only_the_clock_refuses_it	glow-nav-bridge-refines-reach	
src/cursor_glow/tests/stray.rs	the_stray_probe_s_bed_bridges_a_real_word_hop_across_a_repaint	glow-nav-bridge-refines-reach	
src/cursor_glow/tests/style_switch.rs	a_pooled_bolt_jag_is_deterministic_across_strikes	glow-bolt-jag-deterministic	
src/cursor_glow/tests/style_switch.rs	chained_switches_cap_fades_and_never_go_dark	glow-style-switch-crossfade	
src/cursor_glow/tests/style_switch.rs	no_switch_keeps_the_crossfade_machinery_inert	glow-style-switch-crossfade	
src/cursor_glow/tests/style_switch.rs	style_switch_crossfades_the_old_light_and_stays_active	glow-style-switch-crossfade	
src/cursor_glow/tests/style_switch.rs	style_switch_drops_pending_sound_cues	glow-cue-classification	
src/cursor_glow/tests/tint.rs	glyph_tint_bisection_predicate_is_monotone_and_its_answer_holds	glyph-tint-palette	
src/cursor_glow/tests/tint.rs	glyph_tint_invariant_holds_across_every_grey_theme	glyph-tint-palette	
src/cursor_glow/tests/tint.rs	glyph_tint_palette_is_memoized_on_the_theme_key	glyph-tint-palette	
src/cursor_glow/tests/tint.rs	glyph_tint_suppresses_itself_on_a_theme_with_no_usable_side	glyph-tint-palette	
src/cursor_glow/tests/tint.rs	light_ink_bold_holds_one_ink_weight_across_the_spectrum	light-ink-laws	
src/cursor_glow/tests/tint.rs	light_ink_cap_keeps_glyphs_legible	light-ink-laws	
src/cursor_glow/tests/tint.rs	light_ink_darkens_every_band_to_one_bar	light-ink-laws	
src/cursor_glow/tests/tint.rs	one_rainbow_sweep_is_continuous_and_shared	rainbow-sweep-and-bands	
src/cursor_glow/tests/typed_stamps.rs	a_busy_tuis_status_row_keeps_its_brand_across_five_print_rows	glow-program-row-never-spends	
src/cursor_glow/tests/typed_stamps.rs	a_composer_that_stays_put_under_a_scroll_keeps_the_hand_on_its_row	glow-hand-row-hold	
src/cursor_glow/tests/typed_stamps.rs	a_held_press_pays_for_its_own_late_echo_on_the_hand_s_row	glow-hand-row-hold	
src/cursor_glow/tests/typed_stamps.rs	a_line_editing_key_keeps_the_hand_on_its_row	glow-hand-row-hold	
src/cursor_glow/tests/typed_stamps.rs	a_press_the_hold_kept_pays_for_no_hop_off_the_hand_s_row	glow-hand-row-hold	
src/cursor_glow/tests/typed_stamps.rs	a_program_rows_first_advance_mid_burst_cannot_steal_the_echo	glow-program-row-never-spends	
src/cursor_glow/tests/typed_stamps.rs	a_status_tick_below_the_caret_forgets_no_press_while_the_hand_types	glow-hand-row-hold	
src/cursor_glow/tests/typed_stamps.rs	a_status_tick_ending_on_a_live_press_s_glyph_is_no_key_s_echo	glow-hand-row-hold	
src/cursor_glow/tests/typed_stamps.rs	a_ticking_status_row_keeps_its_brand_beside_a_streaming_transcript	glow-program-row-never-spends	
src/cursor_glow/tests/typed_stamps.rs	an_unlicensed_paste_must_not_brand_the_established_echo_row	glow-program-row-never-spends	
src/cursor_glow/tests/typed_stamps.rs	denied_move_preserves_queued_sound_but_retires_stale_keyed_credit	glow-key-time-click-ledger	
src/cursor_glow/tests/typed_stamps.rs	hidden_boundary_spares_fresh_typed_stamps_and_retires_stale	glow-forget-edges	
src/cursor_glow/tests/typed_stamps.rs	hidden_caret_typed_echo_anchors_to_print_site	glow-program-row-never-spends	
src/cursor_glow/tests/typed_stamps.rs	parked_caret_typed_echo_anchors_and_visible_moves_own_their_tick	glow-program-row-never-spends	
src/cursor_glow/tests/typed_stamps.rs	program_row_end_advance_never_consumes_a_typed_stamp	glow-program-row-never-spends	
src/cursor_glow/tests/typed_stamps.rs	the_first_key_after_a_submit_into_a_program_s_composer_is_guarded	glow-hand-row-hold	
src/cursor_glow/tests/typed_stamps.rs	the_sparkle_glitter_train_outlives_the_keystroke_that_follows_it	sparkle-glitter-survives	
src/cursor_glow/tests/typed_stamps.rs	user_gesture_keeps_banked_typed_stamps_for_in_flight_echoes	glow-forget-edges	
src/cursor_glow/tests/unknown_insert.rs	two_unknown_insert_orphan_keys_light_only_on_separate_exact_echoes	glow-unknown-insert-orphans-refine	
src/cursor_glow/tests/unknown_insert.rs	two_unknown_insert_orphans_coalesced_exact_cells_light_without_generic_credit	glow-unknown-insert-orphans-refine	
src/cursor_glow/tests/unknown_insert.rs	two_unknown_insert_orphans_reject_partial_wrong_stale_or_unprobed_batch	glow-unknown-insert-orphans-refine	
src/cursor_glow/tests/unknown_insert.rs	two_unknown_insert_orphans_reject_unrelated_or_stale_second_prints	glow-unknown-insert-orphans-refine	
src/cursor_glow/tests/unknown_insert.rs	unknown_insert_orphan_key_escrow_expires_and_drops_on_space_changes	glow-unknown-insert-orphans-refine	
src/cursor_glow/tests/unknown_insert.rs	unknown_insert_orphan_key_exact_echo_matches_derived_model	glow-unknown-insert-orphans-refine	
src/cursor_glow/tests/unknown_insert.rs	unknown_insert_orphan_space_cannot_claim_an_unchanged_blank	glow-unknown-insert-orphans-refine	
src/cursor_glow/tests/v2_seam.rs	a_line_still_being_typed_has_no_dark_cell_inside_its_live_span	glow-typed-cell-lit	
src/cursor_glow/tests/v2_seam.rs	a_v2_nav_jump_draws_the_meteor_on_the_observed_frame	glow-v2-meteor-frame-zero	
src/cursor_glow/tests/v2_seam.rs	the_frame_that_echoes_the_key_already_shows_its_ribbon_cell	glow-typed-cell-lit	
src/cursor_glow/tests/v2_seam.rs	under_v2_trail_status_claims_the_ribbon_it_lays	glow-status-row	
src/cursor_glow/tests/v2_seam.rs	v2_idle_is_exactly_zero	glow-idle-to-zero	
src/cursor_glow/tests/wake_census.rs	under_v2_the_seam_never_paces_the_frame_train_for_v1s_crown	glow-idle-to-zero	
src/cursor_glow/tests/water.rs	water_emits_a_smooth_wavy_wake	water-style-laws	
src/cursor_glow/tests/water.rs	water_jump_reflection_drips_then_fades_smoothly_to_zero	water-style-laws	
src/cursor_glow/tests/water.rs	water_navigation_jump_spans_the_vector_without_celebration	water-style-laws	
src/cursor_glow/tests/water.rs	water_repaint_corridor_deduplicates_and_cannot_cover_text	water-style-laws	
src/cursor_glow/tests/water.rs	water_stays_in_ocean_hues	water-style-laws	
src/cursor_glow/tests/witness.rs	delayed_single_exact_key_skips_only_a_prior_stale_credit	press-credits-exact-run	
src/cursor_glow/tests/witness.rs	exact_coalesced_keys_retire_older_space_but_refuse_stale_or_interleaved_runs	press-credits-exact-run	
src/cursor_glow/tests/witness.rs	exact_coalesced_run_of_32_keys_remains_one_shot_and_exact	press-credits-exact-run	
src/cursor_glow/tests/witness.rs	far_witness_row_fills_its_engine_slot_once_without_a_second_copy	glow-witness-rows	
src/cursor_glow/tests/witness.rs	full_ribbon_row_budget_keeps_pending_source_and_caret_samples	glow-witness-rows	
src/cursor_glow/tests/witness.rs	neighbor_probe_still_gates_star_pixels_after_discarding_duplicate_row_storage	glow-witness-rows	
src/cursor_glow/tests/witness.rs	swept_path_retains_only_the_bounded_landing_suffix	line-cells-tail-is-bounded-bresenham-suffix	
src/cursor_glow/tests/witness.rs	two_orphan_exact_keys_keep_press_order_across_the_credit_ring_wrap	press-credits-exact-run	
src/cursor_glow/tests/word_nav.rs	a_hidden_program_relocation_with_no_fresh_key_still_sounds_nothing	glow-no-licence-no-light	
src/cursor_glow/tests/word_nav.rs	a_kill_retreat_landing_with_a_band_move_or_a_scroll_is_not_a_navigation_meteor	glow-one-shot-once	
src/cursor_glow/tests/word_nav.rs	a_word_key_judges_a_held_park_before_its_own_landing_is_bridged	glow-park	
src/cursor_glow/tests/word_nav.rs	a_word_pair_sixty_milliseconds_apart_sounds_twice	glow-nav-bridge-refines-reach	
src/cursor_glow/tests/word_nav.rs	each_word_kill_sounds_once_and_never_eats_the_next_word_move	glow-one-shot-once	
src/cursor_glow/tests/word_nav.rs	the_nav_bridge_never_reaches_back_to_a_source_the_key_did_not_move_from	glow-nav-bridge-refines-reach	
src/cursor_glow/tests/word_nav.rs	twenty_word_keys_in_zsh_sound_twenty_times	glow-nav-bridge-refines-reach	
src/cursor_glow/tests/word_nav.rs	twenty_word_keys_through_an_ink_repaint_sound_twenty_times	glow-nav-bridge-refines-reach	
src/cursor_momentum.rs	absurd_rates_never_nan_or_exceed_one	typing-rate-value	
src/cursor_momentum.rs	blink_is_suppressed_while_hot_and_returns_when_cold	momentum-glow-tick	
src/cursor_momentum.rs	cooldown_is_monotone_and_snaps_to_exact_zero	typing-rate-value	
src/cursor_momentum.rs	deterministic_given_timestamps	typing-rate-value	
src/cursor_momentum.rs	fingerprint_settles_so_the_lane_releases	momentum-glow-tick	
src/cursor_momentum.rs	halo_hugs_the_cell_in_bounded_quads	momentum-glow-tick	
src/cursor_momentum.rs	halo_surrounds_the_caret_in_a_padded_headed_window	momentum-glow-tick	
src/cursor_momentum.rs	off_is_byte_identical	momentum-glow-tick	
src/cursor_momentum.rs	ramp_is_monotone_in_luminance_on_dark	momentum-glow-tick	
src/cursor_momentum.rs	rises_with_key_rate	typing-rate-value	
src/cursor_momentum.rs	the_wake_deadline_never_outlives_what_the_glass_draws	momentum-glow-tick	
src/cursor_phaser.rs	charge_widens_and_brightens_the_wings	phaser-fill-and-charge	
src/cursor_phaser.rs	disabled_or_zero_intensity_is_inert	phaser-tick-laws	
src/cursor_phaser.rs	fill_blooms_from_theme_base	phaser-fill-and-charge	
src/cursor_phaser.rs	fill_locks_to_the_beam_hue	phaser-fill-and-charge	
src/cursor_phaser.rs	hidden_cursor_draws_no_wings	phaser-tick-laws	
src/cursor_phaser.rs	host_base_is_the_settled_block_on_either_polarity	phaser-fill-and-charge	
src/cursor_phaser.rs	quad_count_is_bounded	phaser-tick-laws	
src/cursor_phaser.rs	quads_respect_grid_and_cap	phaser-tick-laws	
src/cursor_phaser.rs	settled_present_gaps_are_byte_identical	phaser-tick-laws	
src/cursor_phaser.rs	settles_to_inactive_when_charge_dies	phaser-tick-laws	
src/cursor_phaser.rs	wings_hug_the_beam_axis	phaser-tick-laws	
src/cursor_rainbow.rs	caret_and_ribbon_share_one_live_clock_through_a_streaming_run	rainbow-caret-fill	
src/cursor_rainbow.rs	disabled_or_zero_intensity_is_inert	rainbow-caret-inert	
src/cursor_rainbow.rs	emitted_ribbon_head_is_the_hot_caret_authority	rainbow-caret-fill	
src/cursor_rainbow.rs	fallback_fill_flows_through_the_anchors_without_temporal_hue_steps	rainbow-colour-continuity	
src/cursor_rainbow.rs	fill_blooms_from_base_with_energy	rainbow-caret-fill	
src/cursor_rainbow.rs	halo_quads_respect_grid_and_cap	rainbow-caret-geometry	
src/cursor_rainbow.rs	host_base_is_the_settled_block_on_either_polarity	rainbow-caret-fill	
src/cursor_rainbow.rs	idle_blink_transition_conforms_to_model	rainbow-flare-rungs	
src/cursor_rainbow.rs	idle_phase_flips_never_arm_effect_timer	rainbow-caret-lane	
src/cursor_rainbow.rs	momentum_rung_crossing_fires_twinkle_star	rainbow-flare-rungs	
src/cursor_rainbow.rs	momentum_rungs_fire_once_each_and_never_on_the_way_down	rainbow-flare-rungs	
src/cursor_rainbow.rs	shared_phase_complete_frame_is_exact_across_the_family_ring	rainbow-colour-continuity	
src/cursor_rainbow.rs	spin_is_continuous_across_its_wrap	rainbow-colour-continuity	
src/cursor_rainbow.rs	the_caret_colour_release_is_drawn_to_its_last_step_on_demand	rainbow-caret-lane	
src/cursor_rainbow.rs	the_caret_cools_with_its_trail_and_not_with_the_ignition_heat	rainbow-colour-continuity	
src/cursor_rainbow.rs	the_caret_settles_on_its_own_stop_without_a_pop	rainbow-caret-flare	
src/cursor_rainbow.rs	the_flare_is_white_on_frame_zero_and_the_identity_once_settled	rainbow-caret-flare	
src/cursor_rainbow.rs	the_halo_rides_the_paint_not_the_ignition_heat	rainbow-colour-continuity	
src/cursor_rainbow.rs	the_release_deadline_is_one_dry_run_per_tick	rainbow-caret-lane	
src/cursor_rainbow.rs	the_rim_light_is_hue_invariant_and_monotone_in_paint	rainbow-rim-light	
src/cursor_rainbow.rs	the_rim_turns_with_momentum_and_freezes_at_rest	rainbow-caret-lane	
src/cursor_rainbow.rs	the_settle_split_is_a_partition_of_still_moving	rainbow-caret-lane	
src/cursor_rainbow.rs	twinkle_completes_and_resettles	rainbow-flare-rungs	
src/cursor_trail.rs	a_band_move_translates_the_trail_inside_the_band_and_drops_what_leaves	trail-scroll-band-translate	
src/cursor_trail.rs	a_caret_seen_still_at_rest_after_the_press_proves_the_key_moved_nothing	bridge-reach-contract	
src/cursor_trail.rs	a_glyph_typed_before_the_enters_response_lands_keeps_its_stamp_for_its_own_echo	trail-licence-refines-model	
src/cursor_trail.rs	a_pause_decays_the_ignition	typing-cadence-ignition	
src/cursor_trail.rs	a_user_gesture_establishes_one_class_and_licenses_its_landing	trail-licence-refines-model	
src/cursor_trail.rs	alt_without_blink_keeps_the_jump_comet	trail-licence-refines-model	
src/cursor_trail.rs	bounded_tail_matches_the_legacy_full_sweep	line-cells-tail-is-bounded-bresenham-suffix	
src/cursor_trail.rs	classic_trail_requires_one_shot_correlated_candidate	trail-licence-refines-model	
src/cursor_trail.rs	comet_fades_to_empty_after_the_duration	trail-comet-emission	
src/cursor_trail.rs	composed_physical_margin_fold_lights_only_departure_material	trail-licence-refines-model	
src/cursor_trail.rs	dedup_index_matches_the_linear_scan_byte_for_byte	trail-dedup-equals-linear-merge	
src/cursor_trail.rs	fingerprint_folds_the_trail_colour	trail-comet-emission	
src/cursor_trail.rs	hinted_ink_wrap_lays_no_comet_but_typing_still_trails	trail-licence-refines-model	
src/cursor_trail.rs	ignite_stamps_intensity_and_heats_colour	typing-cadence-ignition	
src/cursor_trail.rs	ignited_comet_is_brighter_and_longer_lived_than_gentle	trail-comet-emission	
src/cursor_trail.rs	presentation_opacity_attenuates_resident_cells_and_fingerprints_exact_output	trail-presentation-projection	
src/cursor_trail.rs	readability_cap_bounds_every_cell_at_any_intensity	trail-comet-emission	
src/cursor_trail.rs	same_row_jump_spawns_a_bounded_comet_nearest_the_cursor	trail-comet-emission	
src/cursor_trail.rs	sample_matches_the_separate_reads_bit_for_bit	typing-cadence-ignition	
src/cursor_trail.rs	scroll_translates_every_live_trail_cell_and_retires_off_top_state	trail-scroll-band-translate	
src/cursor_trail.rs	the_nav_bridge_reaches_one_pane_across_and_a_wrapped_box_down	bridge-reach-contract	
src/cursor_trail.rs	warm_chain_outlives_the_gap	trail-comet-emission	
src/cursor_trail.rs	warmth_is_zero_for_a_lone_key_and_ramps_with_cadence	typing-cadence-ignition	
src/dog_baker.rs	budgets_hold	dog-baker-contract	
src/dog_baker.rs	every_breed_draws_a_dog	dog-roster-table	
src/dog_baker.rs	host_ids_are_stable_distinct_and_namespaced	dog-baker-contract	
src/dog_baker.rs	same_key_bakes_byte_identical_tiles	dog-baker-contract	
src/dog_baker.rs	tiles_are_exact_size_and_axes_are_wired	dog-baker-contract	
src/dog_cameo.rs	entry_bounce_hops_then_settles	dog-cameo-envelope	
src/dog_cameo.rs	envelope_rises_holds_and_expires	dog-cameo-envelope	
src/dog_cameo.rs	resummons_parade_distinct_breeds	dog-cameo-envelope	
src/effect_util.rs	a_source_over_quad_that_rounds_to_black_still_darkens	push-grid-quad-contract	
src/effect_util.rs	ramp_rewrite_is_bit_identical_to_the_stop_walk	ramp-5stop-equals-walk	
src/effect_util.rs	the_additive_star_draws_the_family_silhouette	star-arm-and-silhouette	
src/effect_util.rs	the_one_arm_conversion_truncates_and_floors	star-arm-and-silhouette	
src/effect_util.rs	twinkle_law_stays_under_the_photosensitivity_bound	twinkle-envelope-law	
src/file_feed.rs	oversize_directory_and_invalid_utf8_are_rejected_by_read_and_hash	file-feed-bounded-regular-read	
src/file_feed.rs	regular_file_round_trips_and_fingerprints_content	file-feed-bounded-regular-read	
src/file_feed.rs	writerless_fifo_and_final_symlink_are_rejected_at_open	file-feed-bounded-regular-read	
src/genome.rs	feature_decoders_cover_documented_ranges	genome-feature-ranges	
src/genome.rs	gray_decode_locality	gray-field-decode	
src/genome.rs	nova_single_field_accessors_match_full_decode	genome-feature-ranges	
src/genome.rs	simhash_bitsliced_equals_reference_over_voter_sets	simhash-bitsliced-equals-reference	
src/genome.rs	simhash_clock_and_spinner_immunity	simhash-filter-silence	
src/genome.rs	simhash_ctx_bitsliced_matches_reference_kernel_on_text	simhash-bitsliced-equals-reference	
src/genome.rs	v4_head_primary_tickets_are_equal_and_spill_is_bounded	v4-head-ticket-fairness	
src/genome.rs	v4_head_spill_is_independent_of_coat_iris_age_motion_and_ink	v4-head-ticket-fairness	
src/host.rs	a_hidden_cursor_keeps_its_caret_and_history_withdraws_it	terminal-facts-reader	
src/host.rs	a_hidden_cursor_on_a_blank_row_is_no_caret_for_the_motion_engines	motion-caret-contract	
src/host.rs	a_hidden_cursor_on_an_unfinished_frame_is_no_caret_for_the_motion_engines	motion-caret-contract	
src/host.rs	an_unfinished_frames_print_anchor_is_no_echo_for_the_motion_engines	motion-caret-contract	
src/host.rs	the_facts_reader_reports_what_the_emulator_said	terminal-facts-reader	
src/kitty_cursor.rs	a_key_steps_the_hover_bob_over_frames_not_in_one	pose-closed-form	
src/kitty_cursor.rs	a_kill_flood_grounds_the_flight_and_the_goodbye_keeps_the_oops	lifecycle-grounding-and-fade	
src/kitty_cursor.rs	authenticated_fold_is_bounded_by_wall_time_and_hidden_cats_ignore_it	fold-placement-bounded	
src/kitty_cursor.rs	banking_leans_and_stretches_with_momentum	pose-closed-form	
src/kitty_cursor.rs	breaths_keep_the_dwell_but_word_pauses_stay_cat_free	earn-latency-window	
src/kitty_cursor.rs	casual_word_or_two_never_summons_the_cat	earn-latency-window	
src/kitty_cursor.rs	cat_requires_the_high_band_held_not_merely_touched	earn-latency-window	
src/kitty_cursor.rs	collection_goodbye_keeps_the_discovered_art	render-look-table	
src/kitty_cursor.rs	collection_hello_uses_exact_look_and_bounded_input_reactions	earn-gate	
src/kitty_cursor.rs	coordinate_rebase_invalidates_only_the_promised_cat_palette	placement-reset	
src/kitty_cursor.rs	cursor_palette_is_frozen_for_one_visible_episode	episode-palette-freeze	
src/kitty_cursor.rs	delete_burst_is_one_sustained_oops_with_one_kick	oops-burst	
src/kitty_cursor.rs	delight_is_expression_not_displacement	stride-and-bob	
src/kitty_cursor.rs	discovery_hello_presents_the_new_look_mid_flight	on-collect-contract	
src/kitty_cursor.rs	exit_fx_emits_for_wink_and_heart_only	exit-fx-sinks	
src/kitty_cursor.rs	exit_fx_light_theme_is_a_darkening_source_over_veil	exit-fx-sinks	
src/kitty_cursor.rs	exit_fx_light_veil_vanishes_at_the_exit_ends	exit-fx-sinks	
src/kitty_cursor.rs	fading_cat_revives_on_resumed_typing	revive-continuity	
src/kitty_cursor.rs	global_companion_sync_cannot_morph_an_active_discovery	static-frame-contract	
src/kitty_cursor.rs	hello_goodbye_is_not_revived_by_typing	lifecycle-grounding-and-fade	
src/kitty_cursor.rs	hidden_settles_to_zero_work	hidden-frame-zero-work	
src/kitty_cursor.rs	hidden_time_does_not_consume_collection_hello	hello-pause-shift	
src/kitty_cursor.rs	idle_blink_is_deterministic_and_windowed	eyes-selection	
src/kitty_cursor.rs	kill_chords_fire_the_same_oops_and_never_summon	earn-gate	
src/kitty_cursor.rs	landing_squash_fires_on_stop_and_on_enter	lifecycle-grounding-and-fade	
src/kitty_cursor.rs	mid_flight_look_sync_is_latched_until_the_next_wake	look-latch-per-appearance	
src/kitty_cursor.rs	oops_holds_visibly_then_decays_to_base_look	render-look-table	
src/kitty_cursor.rs	oops_outranks_the_singing_face	render-look-table	
src/kitty_cursor.rs	reduced_collection_hello_is_one_bounded_static_pose	static-frame-contract	
src/kitty_cursor.rs	reduced_motion_celebration_is_a_static_singing_pose	static-frame-contract	
src/kitty_cursor.rs	reduced_motion_flight_retires_and_releases_the_look_latch	static-frame-contract	
src/kitty_cursor.rs	reduced_motion_never_draws_a_cat_the_machine_calls_hidden	static-frame-agrees-on-existence	
src/kitty_cursor.rs	reduced_motion_oops_is_a_static_swap_with_a_decay_wake	static-frame-contract	
src/kitty_cursor.rs	reverse_fold_changes_place_without_double_delivering_backspace	fold-placement-bounded	
src/kitty_cursor.rs	singing_pins_momentum_summons_and_dances_on_the_beat	pose-closed-form	
src/kitty_cursor.rs	sixteen_high_band_keys_are_required_even_after_full_dwell	earn-gate	
src/kitty_cursor.rs	sparse_frame_after_collection_deadline_settles_directly_hidden	lifecycle-grounding-and-fade	
src/kitty_cursor.rs	stride_pump_animates_from_cadence_not_from_the_pinned_level	stride-and-bob	
src/kitty_cursor.rs	summon_latency_is_bounded_across_typing_cadences	earn-latency-window	
src/kitty_cursor.rs	sustained_speed_squints_happily	eyes-selection	
src/kitty_cursor.rs	sustained_typing_summons_then_fades_out_completely	earn-latency-window	
src/kitty_cursor.rs	the_exit_flourish_head_never_shows_at_full_opacity	render-look-table	
src/kitty_cursor.rs	the_singing_head_never_blinks_at_the_gate_value	eyes-selection	
src/kitty_cursor.rs	typed_word_reactions_are_bounded_expression	earn-gate	
src/kitty_cursor.rs	unowned_cursor_relocation_retires_only_the_earned_flying_episode	on-collect-contract	
src/kitty_cursor.rs	wind_down_crossfades_the_dance_out	render-look-table	
src/kitty_pet.rs	a_bell_during_flight_expires_unconsumed	bell-stimulus	
src/kitty_pet.rs	a_bell_flinches_the_settled_pet	bell-stimulus	
src/kitty_pet.rs	a_bell_inside_a_commanded_sit_ends_the_seat	bell-stimulus	
src/kitty_pet.rs	a_bell_wakes_the_sleeper	bell-stimulus	
src/kitty_pet.rs	a_big_retreat_still_bottles	retreat-reactions	
src/kitty_pet.rs	a_bored_cat_hides_behind_a_word_and_comes_back	pointer-play	
src/kitty_pet.rs	a_build_log_under_the_cat_is_not_chased_across_the_pane	stream-watch	
src/kitty_pet.rs	a_burst_of_new_lines_comes_home_quietly	show-door	
src/kitty_pet.rs	a_caret_jump_interrupts_the_watch	stream-watch	
src/kitty_pet.rs	a_caret_jump_is_served_before_the_swap	breed-handoff	
src/kitty_pet.rs	a_caret_move_interrupts_a_commanded_seat	trick-lifetime	
src/kitty_pet.rs	a_caret_that_hides_and_returns_elsewhere_is_a_new_sighting_not_a_retreat	first-sighting	
src/kitty_pet.rs	a_caret_withheld_mid_flight_holds_its_fade_and_lands_when_dark	hidden-arc-deferral	
src/kitty_pet.rs	a_cat_on_vigil_gets_up_to_answer_and_is_not_dragged_back_mid_seat	vigil	
src/kitty_pet.rs	a_cat_waiting_up_with_a_job_sleeps_when_told_to	vigil	
src/kitty_pet.rs	a_cheer_survives_a_hold	verdict-choreography	
src/kitty_pet.rs	a_commanded_hide_behind_a_word_at_the_margin_still_ends	pet-idle-to-zero	
src/kitty_pet.rs	a_commanded_hide_walks_behind_a_word_in_reach	trick-performance	
src/kitty_pet.rs	a_commanded_sleep_ends_byte_stable_and_rides_the_offer	offer-soundness	
src/kitty_pet.rs	a_confirmation_after_the_cat_answered_replays_nothing	trick-latch-algebra	
src/kitty_pet.rs	a_confirmation_inside_the_heard_you_beat_is_still_owed_and_plays_once	trick-latch-algebra	
src/kitty_pet.rs	a_cursor_resting_on_a_sleeping_cat_is_not_a_side_to_turn_to	sleeper-gaze-brakes	
src/kitty_pet.rs	a_cursor_swinging_over_a_sleeping_cat_never_flips_it	sleeper-gaze-brakes	
src/kitty_pet.rs	a_dashing_pointer_gets_pounced_only_when_content	pointer-play	
src/kitty_pet.rs	a_debounced_park_swaps_in_place_and_a_departing_body_runs_off	breed-handoff	
src/kitty_pet.rs	a_deep_sleep_re_anchor_hops_then_gives_the_lane_back	pet-idle-to-zero	
src/kitty_pet.rs	a_driven_session_s_cat_keeps_its_distance_and_walks_back_at_release	room-facts-no-wake	
src/kitty_pet.rs	a_failure_droops_and_cools	verdict-choreography	
src/kitty_pet.rs	a_failure_never_fires_the_party_ring	verdict-choreography	
src/kitty_pet.rs	a_far_peek_is_ignored	peek-latch-ranges	
src/kitty_pet.rs	a_fast_command_only_nudges	verdict-choreography	
src/kitty_pet.rs	a_first_sighting_materialises_at_the_station_not_at_the_origin	first-sighting	
src/kitty_pet.rs	a_flight_lands_on_wall_time_through_synchronized_output_holds	flight-wall-time	
src/kitty_pet.rs	a_flinch_under_a_hiding_cat_ends_the_hide	pet-idle-to-zero	
src/kitty_pet.rs	a_gentle_body_stroke_earns_a_visible_finite_affection_beat	petting-affection	
src/kitty_pet.rs	a_ghost_reaches_its_door_on_a_wide_pane	breed-handoff	
src/kitty_pet.rs	a_gold_star_near_a_contented_cat_is_offered_caught_and_spent_once	v2-offer-latch	
src/kitty_pet.rs	a_good_run_earns_a_purr_and_a_cold_start_does_not	contentment-purr	
src/kitty_pet.rs	a_growing_line_never_tows_the_cat_along_behind_it	ink-eviction-behavior	
src/kitty_pet.rs	a_half_grid_standing_gap_jumps_once_the_hold_resolves	show-door	
src/kitty_pet.rs	a_held_backspace_is_one_sustained_fright_not_a_strobe	retreat-reactions	
src/kitty_pet.rs	a_held_word_jump_is_not_a_stream	stream-watch	
src/kitty_pet.rs	a_hidden_caret_fades_the_crossfade_with_the_body_instead_of_cutting_it	breed-handoff	
src/kitty_pet.rs	a_hidden_caret_fades_the_pet_out_and_the_fingerprint_settles_to_zero	fade-and-appearance	
src/kitty_pet.rs	a_hidden_caret_reduced_motion_and_a_hold_on_each_drop_a_trick	trick-lifetime	
src/kitty_pet.rs	a_hidden_frame_that_hid_no_move_keeps_the_next_show	show-door	
src/kitty_pet.rs	a_hidden_pet_takes_a_look_sync_immediately	fade-and-appearance	
src/kitty_pet.rs	a_light_sleeper_comes_round_slowly_and_a_deep_one_not_at_all	sleeper-gaze-brakes	
src/kitty_pet.rs	a_line_change_to_the_right_is_still_a_quiet_crossing	new-line-law	
src/kitty_pet.rs	a_long_rally_earns_the_tennis_watch	pointer-play	
src/kitty_pet.rs	a_long_success_cheers_and_warms	verdict-choreography	
src/kitty_pet.rs	a_low_cadence_host_still_puts_the_pet_to_sleep_on_time	settle-ladder	
src/kitty_pet.rs	a_mid_appearance_look_sync_parks_until_the_pet_has_faded_out	fade-and-appearance	
src/kitty_pet.rs	a_move_that_ends_a_pause_lands_on_windows_the_pause_has_aged	keep-ahead-lead	
src/kitty_pet.rs	a_moving_pet_still_asks_for_every_frame	needs-frames-motion	
src/kitty_pet.rs	a_nearby_peek_gets_batted	bat-and-look	
src/kitty_pet.rs	a_new_line_costs_one_flight_and_no_gallop	new-line-law	
src/kitty_pet.rs	a_one_frame_caret_hide_never_teleports_a_visible_cat	no-teleport-law	
src/kitty_pet.rs	a_parked_pointer_over_the_body_never_farms_affection_or_frames	pet-idle-to-zero	
src/kitty_pet.rs	a_pause_leaves_the_cat_where_it_stopped	keep-ahead-lead	
src/kitty_pet.rs	a_pet_during_flight_waits_for_the_ground	petting-affection	
src/kitty_pet.rs	a_pet_on_the_head_makes_a_heart	petting-affection	
src/kitty_pet.rs	a_pet_only_submit_forgives_the_fast_failure_that_follows	command-verdict-ledger	
src/kitty_pet.rs	a_pounce_mid_rhythm_lands_at_the_predicted_station	keep-ahead-lead	
src/kitty_pet.rs	a_prompt_row_that_inks_past_the_caret_still_gets_a_quiet_new_line	new-line-law	
src/kitty_pet.rs	a_purring_cat_s_commanded_seat_rides_the_offer	offer-soundness	
src/kitty_pet.rs	a_purring_seat_s_notes_and_hearts_ride_the_offer_not_the_frame_train	offer-soundness	
src/kitty_pet.rs	a_quick_caret_return_cancels_the_deferred_hidden_landing	hidden-arc-deferral	
src/kitty_pet.rs	a_quiet_redress_never_flips_the_cat	breed-handoff	
src/kitty_pet.rs	a_re_aim_in_the_last_frames_of_an_arc_is_dropped_not_solved	no-teleport-law	
src/kitty_pet.rs	a_re_aim_keeps_its_column_when_the_caret_goes_behind_and_takes_its_row	no-teleport-law	
src/kitty_pet.rs	a_re_anchor_leaves_the_ground_instead_of_cutting	ink-eviction-behavior	
src/kitty_pet.rs	a_repeating_jump_must_not_pin_the_pet_in_its_double_take	chase-convergence-and-facing	
src/kitty_pet.rs	a_restarted_ramp_never_rewinds_an_adopted_stance	breed-handoff	
src/kitty_pet.rs	a_revoke_drops_only_a_still_pending_tentative_latch	trick-latch-algebra	
src/kitty_pet.rs	a_revoke_inside_the_heard_you_beat_takes_the_verb_back	trick-latch-algebra	
src/kitty_pet.rs	a_revoked_trick_leaves_the_frames_byte_identical	trick-latch-algebra	
src/kitty_pet.rs	a_roll_interrupted_by_an_enter_jump_releases_the_lane_and_never_replays	pet-idle-to-zero	
src/kitty_pet.rs	a_round_trip_flip_never_walks	breed-handoff	
src/kitty_pet.rs	a_running_job_keeps_the_cat_awake_with_you	vigil	
src/kitty_pet.rs	a_scrolled_wrap_is_a_wrap	new-line-law	
src/kitty_pet.rs	a_seam_crossing_never_bets_past_the_station	keep-ahead-lead	
src/kitty_pet.rs	a_seat_commanded_off_its_mark_is_taken_after_the_walk_home	trick-performance	
src/kitty_pet.rs	a_settled_cat_after_a_drop_still_notices	sleeper-drop	
src/kitty_pet.rs	a_shed_frame_after_a_verdict_never_strands_the_frame_lane	pet-idle-to-zero	
src/kitty_pet.rs	a_short_new_line_does_not_stand_down_the_next_show	show-door	
src/kitty_pet.rs	a_sibling_pane_s_burst_earns_one_look	room-facts-no-wake	
src/kitty_pet.rs	a_single_backspace_flinches_without_the_full_bottle	retreat-reactions	
src/kitty_pet.rs	a_skid_once_drawn_stays_a_still_poses_floor	chase-convergence-and-facing	
src/kitty_pet.rs	a_sleeper_is_woken_for_a_trick_but_sleep_is_done_and_stretch_is_the_wake	trick-performance	
src/kitty_pet.rs	a_slow_caret_walks_laterally_and_a_brisk_one_trots	gait-by-speed-and-distance	
src/kitty_pet.rs	a_snap_home_is_a_jump_not_a_fright	show-door	
src/kitty_pet.rs	a_spent_watch_rearms_once_the_stream_ends	pet-idle-to-zero	
src/kitty_pet.rs	a_stale_peek_expires	bat-and-look	
src/kitty_pet.rs	a_stale_pet_expires	petting-affection	
src/kitty_pet.rs	a_stand_that_ends_a_stride_holds_a_stride_before_a_leap	flight-choreography	
src/kitty_pet.rs	a_startle_shoved_off_its_spot_is_not_replayed	retreat-reactions	
src/kitty_pet.rs	a_streaming_pane_is_followed_with_or_without_shell_integration	stream-watch	
src/kitty_pet.rs	a_streaming_pane_perks_the_sitter	stream-watch	
src/kitty_pet.rs	a_success_soon_after_a_sulk_is_a_big_cheer	command-verdict-ledger	
src/kitty_pet.rs	a_surface_switch_drops_a_trick_and_carries_the_praise_cooldown	coordinate-retirement	
src/kitty_pet.rs	a_suspended_host_clock_cannot_teleport_the_pet	no-teleport-law	
src/kitty_pet.rs	a_tentative_trick_settles_at_one_second_and_a_confirmed_one_at_seven_tenths	trick-performance	
src/kitty_pet.rs	a_top_down_paint_never_hops_every_row	stream-watch	
src/kitty_pet.rs	a_trick_expires_four_seconds_after_the_last_key_and_never_from_caret_moves	trick-lifetime	
src/kitty_pet.rs	a_trick_latch_arms_and_releases_the_lane_and_survives_a_hold_and_a_flight	trick-performance	
src/kitty_pet.rs	a_trick_noted_at_a_retired_cat_meets_the_slow_arm_and_is_dropped	pet-idle-to-zero	
src/kitty_pet.rs	a_trick_typed_at_speed_is_performed_where_the_cat_stands	trick-performance	
src/kitty_pet.rs	a_twice_moved_flight_lands_and_stares_before_it_admits_it	scrabble-and-commit-stare	
src/kitty_pet.rs	a_typed_word_and_enter_plays_at_the_new_prompt	trick-performance	
src/kitty_pet.rs	a_typing_rhythm_grows_the_lead_past_the_base_station	keep-ahead-lead	
src/kitty_pet.rs	a_wide_prompt_s_own_new_line_does_not_startle	new-line-law	
src/kitty_pet.rs	a_wide_prompt_still_leaves_the_caret_at_home	caret-at-home	
src/kitty_pet.rs	a_wrapping_build_log_sits_the_cat_down_to_watch	stream-watch	
src/kitty_pet.rs	a_zero_dt_tick_does_not_park_a_gliding_body	needs-frames-motion	
src/kitty_pet.rs	alternating_moves_never_lead	keep-ahead-lead	
src/kitty_pet.rs	an_agreeing_re_sync_cannot_downgrade_a_parked_ceremony	breed-handoff	
src/kitty_pet.rs	an_enter_drops_the_sleeper_off_the_line	sleeper-drop	
src/kitty_pet.rs	an_enter_during_a_gather_never_latches_the_show	show-door	
src/kitty_pet.rs	an_unfed_ink_map_changes_nothing	empty-ink-identity	
src/kitty_pet.rs	an_unrelated_failure_still_sulks	command-verdict-ledger	
src/kitty_pet.rs	an_unseen_inbox_row_perks_the_cat_and_a_hold_on_sits_it	room-facts-no-wake	
src/kitty_pet.rs	caret_work_outranks_pointer_play	pointer-play	
src/kitty_pet.rs	changing_species_preserves_the_pets_state	species-skin-on-glass	
src/kitty_pet.rs	coordinate_retirement_is_dark_idempotent_and_preserves_identity	coordinate-retirement	
src/kitty_pet.rs	coordinate_retirement_lands_the_latest_parked_identity	coordinate-retirement	
src/kitty_pet.rs	creeping_toy_pins_the_stakeout	pointer-play	
src/kitty_pet.rs	departures_cap_at_the_documented_bound_and_drop_the_oldest	departure-lane	
src/kitty_pet.rs	every_cat_pose_has_a_distinct_dog_counterpart	dog-skin-table	
src/kitty_pet.rs	every_dealt_idle_frame_holds_at_least_two_ticks	idle-frame-laws	
src/kitty_pet.rs	a_tail_flick_a_seat_comes_into_never_shows_its_last_present	idle-frame-laws	
src/kitty_pet.rs	every_idle_deal_goes_byte_stable_by_the_breath_window	pet-idle-to-zero	
src/kitty_pet.rs	every_new_line_reaction_is_lawful_and_brief	new-line-law	
src/kitty_pet.rs	every_trick_is_heard_performs_something_observable_and_releases_the_lane	trick-performance	
src/kitty_pet.rs	facing_follows_travel_with_hysteresis	chase-convergence-and-facing	
src/kitty_pet.rs	far_peek_gets_a_look_not_a_trip	bat-and-look	
src/kitty_pet.rs	fussing_back_and_forth_makes_it_frolic	retreat-reactions	
src/kitty_pet.rs	gentle_strokes_can_wake_a_sleeper_but_not_a_reduced_motion_pet	petting-affection	
src/kitty_pet.rs	grief_replays_beside_the_new_prompt	verdict-choreography	
src/kitty_pet.rs	horizontal_flight_stretches_along_the_motion_axis	flight-choreography	
src/kitty_pet.rs	hunt_and_peck_never_sees_the_lead	keep-ahead-lead	
src/kitty_pet.rs	ink_arriving_under_the_heard_you_beat_drops_the_owed_verb	ink-eviction-behavior	
src/kitty_pet.rs	landing_dust_holds_the_train_for_a_bounded_beat_past_the_landing	needs-frames-motion	
src/kitty_pet.rs	no_bound_crosses_more_than_half_the_animal_in_a_frame	quiet-pace-law	
src/kitty_pet.rs	no_flight_may_outrun_the_animal_that_flies_it	no-teleport-law	
src/kitty_pet.rs	no_idle_beat_moves_the_body	idle-frame-laws	
src/kitty_pet.rs	non_finite_inputs_never_poison_the_pose	peek-latch-ranges	
src/kitty_pet.rs	one_long_frame_cannot_move_a_ghost_a_screen	breed-handoff	
src/kitty_pet.rs	one_pounce_per_dash	pointer-play	
src/kitty_pet.rs	pointer_none_clears_heat	pointer-play	
src/kitty_pet.rs	quiet_settles_then_sleeps_and_typing_wakes_it_with_a_stretch	settle-ladder	
src/kitty_pet.rs	reduced_motion_mid_roll_releases_the_lane_and_replays_nothing	reduced-motion-static	
src/kitty_pet.rs	reduced_motion_pins_the_pet_at_its_station_with_no_arc_or_gait	reduced-motion-static	
src/kitty_pet.rs	reduced_motion_swaps_instantly	reduced-motion-static	
src/kitty_pet.rs	resident_palette_is_owned_by_one_pet_appearance	fade-and-appearance	
src/kitty_pet.rs	retire_unowned_kills_a_live_mote_where_withholding_the_caret_does_not	pet-retire-unowned	
src/kitty_pet.rs	sparse_ink_refinement_keeps_real_glyphs_and_unknown_coverage_obstructed	ink-ladder	
src/kitty_pet.rs	stand_is_emitted_between_arrival_and_sit	settle-ladder	
src/kitty_pet.rs	startle_faces_the_caret_even_at_the_wall	wall-side	
src/kitty_pet.rs	steady_typing_walks_and_fast_typing_runs	gait-by-speed-and-distance	
src/kitty_pet.rs	stroke_progress_cannot_cross_hidden_reduced_or_work_frames	petting-affection	
src/kitty_pet.rs	tease_earns_a_running_chase_then_dignity	pointer-play	
src/kitty_pet.rs	the_bound_apex_never_clips_the_viewport_top	viewport-clamp	
src/kitty_pet.rs	the_cat_never_changes_position_without_animating_it	no-teleport-law	
src/kitty_pet.rs	the_cat_walks_home_after_playing	pointer-play	
src/kitty_pet.rs	the_chase_converges_without_oscillating_around_the_station	chase-convergence-and-facing	
src/kitty_pet.rs	the_chosen_show_is_paced_like_every_other_bound	show-door	
src/kitty_pet.rs	the_departing_ghost_leaves_at_full_opacity	breed-handoff	
src/kitty_pet.rs	the_first_step_out_of_a_seat_faces_the_gap	chase-convergence-and-facing	
src/kitty_pet.rs	the_first_tick_of_a_fade_in_keeps_the_frame_lane	first-sighting	
src/kitty_pet.rs	the_first_windowless_still_materializes_the_resident_pet	first-sighting	
src/kitty_pet.rs	the_gait_cycle_is_driven_by_distance_not_by_the_clock	gait-by-speed-and-distance	
src/kitty_pet.rs	the_gallop_cycle_stays_below_nyquist_and_paces_by_stride_cells	gait-by-speed-and-distance	
src/kitty_pet.rs	the_gaze_brakes_never_reach_the_waking_chase	chase-convergence-and-facing	
src/kitty_pet.rs	the_gaze_follows_a_wandering_pointer	pointer-play	
src/kitty_pet.rs	the_ink_ladder_prefers_a_blank_row_to_a_distant_stand	ink-ladder	
src/kitty_pet.rs	the_ink_map_scroll_fact_is_only_true_for_a_one_row_scroll	ink-scroll-fact	
src/kitty_pet.rs	the_leap_arc_leaves_the_ground_and_lands_back_on_a_baseline	flight-choreography	
src/kitty_pet.rs	the_meteor_s_perk_reaches_the_resident_at_the_landing_s_own_instant	v2-offer-latch	
src/kitty_pet.rs	the_notice_beat_is_only_paid_by_a_cat_at_rest	new-line-law	
src/kitty_pet.rs	the_offer_buys_no_wake	v2-offer-latch	
src/kitty_pet.rs	the_offer_is_the_renderer_s_own_pixel_step_at_an_odd_cell_height	offer-soundness	
src/kitty_pet.rs	the_peck_stance_clears_the_caret_cell	settle-ladder	
src/kitty_pet.rs	the_pet_frolics_once_when_the_hand_finds_its_flow	flow-latch	
src/kitty_pet.rs	the_pet_never_leaves_the_viewport	viewport-clamp	
src/kitty_pet.rs	the_purr_notes_wear_the_ribbon_s_colour_under_the_cat	v2-offer-latch	
src/kitty_pet.rs	the_room_s_facts_buy_no_wake	room-facts-no-wake	
src/kitty_pet.rs	the_same_typing_still_produces_the_same_cat	time-translation-determinism	
src/kitty_pet.rs	the_sleeper_follows_the_prompt	ink-eviction-behavior	
src/kitty_pet.rs	the_wall_crossing_transits_with_a_small_arc	wall-side	
src/kitty_pet.rs	the_wall_side_stand_takes_the_blank_row_below	ink-eviction-behavior	
src/kitty_pet.rs	the_wall_transit_never_fires_after_a_flight_that_already_crossed	wall-side	
src/kitty_pet.rs	the_watch_ends_before_sleep_is_due	stream-watch	
src/kitty_pet.rs	the_watcher_hugs_the_live_edge	stream-watch	
src/kitty_pet.rs	the_zees_die_before_the_ink	mote-laws	
src/kitty_pet.rs	typed_praise_is_rationed_and_a_scold_moves_nothing	trick-performance	
src/kitty_pet.rs	typing_aborts_the_chase_instantly	pointer-play	
src/kitty_pet.rs	typing_echo_never_perks	stream-watch	
src/kitty_pet.rs	wall_hysteresis_does_not_flip_flop_at_the_crossing	wall-side	
src/kitty_pet.rs	wave_one_latches_arm_and_release_the_frame_lane	pet-idle-to-zero	
src/kitty_pet.rs	work_outranks_the_bat	bat-and-look	
src/kitty_pet/console_life.rs	a_cat_stopped_in_the_escorts_grace_never_sets_out_under_the_chrome_to_turn_back	host-line-head-law	
src/kitty_pet/console_life.rs	a_carried_ghost_keeps_what_its_own_slot_drew	console-ghost-rules	
src/kitty_pet/console_life.rs	a_commanded_sleep_survives_an_executing_block	console-trick-interplay	
src/kitty_pet/console_life.rs	a_departing_ghost_over_a_protected_line_fades_with_the_body_instead_of_vanishing	console-ghost-rules	
src/kitty_pet/console_life.rs	a_departing_ghost_over_ordinary_ink_survives_and_over_a_selection_does_not	console-ghost-rules	
src/kitty_pet/console_life.rs	a_forgiven_pet_only_submit_leaves_no_failed_result_on_the_console	console-trick-interplay	
src/kitty_pet/console_life.rs	a_ghost_on_its_own_glass_needs_its_own_worlds_admission	console-ghost-rules	
src/kitty_pet/console_life.rs	a_held_selection_drops_a_trick_and_never_replays_it	console-trick-interplay	
src/kitty_pet/console_life.rs	a_kept_console_reading_draws_what_a_fresh_one_does	pw-reading-kept	
src/kitty_pet/console_life.rs	a_late_wake_read_still_offers_the_unconsumed_expiry	console-attention	
src/kitty_pet/console_life.rs	a_look_swap_landing_under_a_hidden_caret_resident_keeps_both_halves_of_the_crossfade	console-ghost-rules	
src/kitty_pet/console_life.rs	a_look_swaps_arrival_ramp_runs_while_the_console_resident_owns_the_tick	console-ghost-rules	
src/kitty_pet/console_life.rs	a_roll_plays_through_an_executing_block	console-trick-interplay	
src/kitty_pet/console_life.rs	a_seat_typed_through_a_repair_is_taken_and_held	console-trick-interplay	
src/kitty_pet/console_life.rs	a_spent_content_perch_hands_the_pet_back_its_own_life	console-attention	
src/kitty_pet/console_life.rs	a_trick_typed_through_a_repair_is_still_performed	console-trick-interplay	
src/kitty_pet/console_life.rs	a_typed_treat_frolics_with_the_console_layer_watching	console-trick-interplay	
src/kitty_pet/console_life.rs	an_enter_inside_the_heard_you_beat_still_gets_its_trick	trick-latch-algebra	
src/kitty_pet/console_life.rs	an_incoherent_observation_never_falls_back_to_uncertified_legacy_placement	console-visibility-hold	
src/kitty_pet/console_life.rs	an_unchanged_screen_is_read_once	pw-reading-kept	
src/kitty_pet/console_life.rs	contact_requires_new_local_ink_not_a_redraw_or_body_motion	console-attention	
src/kitty_pet/console_life.rs	fresh_input_stands_a_commanded_seat_down_but_keeps_an_owed_latch	console-trick-interplay	
src/kitty_pet/console_life.rs	held_selection_has_real_presence_without_a_caret_and_stops_waking	console-attention	
src/kitty_pet/console_life.rs	hidden_cursor_home_continuity_conforms_to_real_terminal_motion	hidden-cursor-home-refinement	
src/kitty_pet/console_life.rs	no_perch_in_reach_is_not_an_opinion_about_where_the_pet_belongs	console-attention	
src/kitty_pet/console_life.rs	progress_baselines_each_command_and_duplicate_reports_are_not_events	console-attention	
src/kitty_pet/console_life.rs	repair_holds_its_body_in_the_inspect_pose	console-attention	
src/kitty_pet/console_life.rs	streamed_output_lets_a_trick_expire_and_the_resident_takes_its_perch_back	console-trick-interplay	
src/kitty_pet/console_life.rs	suppressed_failure_still_hushes_celebration_for_a_finite_window	console-attention	
src/kitty_pet/console_life.rs	the_stand_over_the_hosts_line_is_a_perch_a_hold_keeps_and_never_in_a_rule	host-line-head-law	
src/kitty_pet/console_life.rs	typed_up_to_a_label_on_the_rule_the_cat_walks_on_top_of_the_line	host-line-head-law	
src/kitty_pet/console_life.rs	typing_into_a_hosted_composer_never_draws_the_cat_under_its_upper_rule	host-line-head-law	protected-surface-yield
src/kitty_pet/full_screen_tests.rs	a_full_screen_program_stands_down_a_vigil_armed_before_it	full-screen-repaint-refinement	
src/kitty_pet/full_screen_tests.rs	a_full_screen_repaint_is_not_output	full-screen-repaint-refinement	
src/kitty_pet/full_screen_tests.rs	a_live_stare_lets_go_the_tick_the_full_screen_comes_up	note-full-screen	
src/kitty_pet/full_screen_tests.rs	a_main_screen_stream_still_perks_hugs_and_waits	full-screen-repaint-refinement	
src/kitty_pet/full_screen_tests.rs	a_torn_redraw_stream_arms_no_hug	full-screen-repaint-refinement	
src/kitty_pet/full_screen_tests.rs	full_screen_repaint_model_proves_and_catches	full-screen-repaint-refinement	
src/kitty_pet/full_screen_tests.rs	the_shipped_law_reads_the_same_take_as_a_stream	full-screen-repaint-refinement	
src/kitty_registry.rs	app_basename_strips_paths_prefixes_and_case	kr-app-basename	
src/kitty_registry.rs	coat_distance_table_is_pinned_to_the_live_lift	kr-coat-distance	
src/kitty_registry.rs	flagship_trio_is_always_sufficiently_different	kr-coat-distance	
src/kitty_registry.rs	for_app_never_emits_an_accessory	kr-app-look	
src/kitty_registry.rs	launch_kitty_is_seed_pure_normalized_head_and_bare	kr-launch-look	
src/kitty_registry.rs	look_normalization_never_leaves_an_overlay_as_the_base	kr-look-normalized	
src/kitty_registry.rs	registry_keys_are_unique_and_pinned	kr-registry-keys	
src/kitty_sing.rs	a_batched_ime_commit_of_repeats_does_not_arm	ks-arm-threshold	
src/kitty_sing.rs	a_celebration_s_fans_ride_the_bars_and_the_outro_ends_on_do	ks-external-run-events	
src/kitty_sing.rs	a_continuous_hold_does_not_rewind_its_beat_clock	ks-arm-threshold	
src/kitty_sing.rs	a_missed_gap_recovers_with_three_reps_inside_the_glow	ks-arm-threshold	
src/kitty_sing.rs	a_six_key_medley_holds_the_stage_end_to_end	ks-key-switch-handover	
src/kitty_sing.rs	an_armed_celebration_fires_on_the_next_green_block_and_only_once	ks-external-fire	
src/kitty_sing.rs	an_unarmed_session_never_celebrates_on_a_green_block	ks-external-fire	
src/kitty_sing.rs	an_on_done_arm_fires_on_the_session_s_next_done_and_only_once	ks-external-fire	
src/kitty_sing.rs	backspace_never_arms_and_breaks_a_run	ks-arm-threshold	
src/kitty_sing.rs	cold_arms_still_cost_sixteen	ks-arm-threshold	
src/kitty_sing.rs	external_deadline_conforms_through_bars_outro_and_settle	ks-external-deadline-refinement	
src/kitty_sing.rs	external_deadline_does_not_schedule_held_keys_or_replay_missed_bars	ks-external-deadline-refinement	
src/kitty_sing.rs	external_deadline_model_proves_and_catches_a_rearmed_consumed_event	ks-external-deadline-refinement	
src/kitty_sing.rs	genuine_typing_still_winds_the_song_down	ks-key-switch-handover	
src/kitty_sing.rs	interleaved_chars_never_arm	ks-arm-threshold	
src/kitty_sing.rs	letting_go_winds_down_without_an_event	ks-drive-envelope	
src/kitty_sing.rs	note_host_ids_avoid_kitty_sprite_family	ks-note-art	
src/kitty_sing.rs	re_earning_the_threshold_during_wind_down_re_arms	ks-arm-threshold	
src/kitty_sing.rs	reduced_motion_notes_hold_still	ks-note-resolve	
src/kitty_sing.rs	session_switch_releases_the_hold	ks-arm-threshold	
src/kitty_sing.rs	slow_same_key_striking_never_arms	ks-arm-threshold	
src/kitty_sing.rs	song_signature_mixer_is_bijective_over_all_chars	ks-song-signature-bijective	
src/kitty_sing.rs	the_cooldown_refuses_a_second_arm_for_thirty_seconds	ks-external-arm-admission	
src/kitty_sing.rs	the_owner_pattern_w_a_r_never_drops_the_drive	ks-key-switch-handover	
src/kitty_sing.rs	threshold_at_cadence_arms_full_sing	ks-arm-threshold	
src/kitty_sing.rs	wind_down_crossfades_to_zero	ks-drive-envelope	
src/kitty_sing.rs	wind_down_drains_the_note_field	ks-notes-cull-and-spawn	
src/kitty_summon.rs	a_feline_word_glued_to_another_is_not_the_word	ksum-builtin-vocabulary	
src/kitty_summon.rs	a_recorded_summon_is_always_a_completion	ksum-tiers	
src/kitty_summon.rs	backspace_pops_typos_and_never_summons	ksum-window-edits	
src/kitty_summon.rs	break_clears_the_run	ksum-window-edits	
src/kitty_summon.rs	cooldown_suppresses_the_record_without_restamping	ksum-tiers	
src/kitty_summon.rs	dogs_arrive_only_after_a_lot_of_typing	ksum-tiers	
src/kitty_summon.rs	incremental_scan_matches_a_whole_window_rescan_keystroke_for_keystroke	ksum-hit-is-lexicon-completion	
src/kitty_summon.rs	innocent_words_containing_a_curse_never_fire	ksum-builtin-vocabulary	
src/kitty_summon.rs	profanity_winces_multilingually_and_never_records	ksum-builtin-vocabulary	
src/kitty_summon.rs	session_switch_clears_the_window	ksum-window-edits	
src/kitty_summon.rs	the_gate_odometer_survives_window_resets	ksum-tiers	
src/kitty_summon.rs	typed_word_summons_once_at_completion	ksum-window-edits	
src/kitty_summon.rs	window_stays_bounded_and_utf8	ksum-hit-is-lexicon-completion	
src/matrix_rain/bake.rs	all_tiles_nonempty_at_live_metrics	rb-box-filter	
src/matrix_rain/bake.rs	rebuilt_bakers_replay_versions_and_tiles	rb-progressive-bake	
src/matrix_rain/bake.rs	same_metric_begin_frame_keeps_progress	rb-progressive-bake	
src/matrix_rain/bitmap_font.rs	every_advertised_family_is_present	bf-table-lookup	
src/matrix_rain/bitmap_font.rs	the_table_is_sorted_and_has_no_duplicates	bf-table-lookup	
src/matrix_rain/field.rs	density_ring_looks_up_cycle_start	rf-density-ring	
src/matrix_rain/field.rs	dither_quantum_rides_the_mutation_grid	rf-quanta	
src/matrix_rain/field.rs	flash_floor_holds_at_all_clamped_configs	rf-col-params	
src/matrix_rain/field.rs	tiny_viewports_never_panic_and_keep_invariants	rf-col-params	
src/matrix_rain/mod.rs	alpha_cap_is_pinned	mr-luminance	
src/matrix_rain/mod.rs	bell_and_exit_survive_suspension_activity_does_not	mr-pending-notes	
src/matrix_rain/mod.rs	budget_bound_holds_and_truncation_fires	mr-budget-and-truncation	
src/matrix_rain/mod.rs	can_emit_probe_tracks_visibility_and_motion	mr-drain-bound	
src/matrix_rain/mod.rs	closed_tui_frame_protects_default_background_interior	mr-frame-regions	
src/matrix_rain/mod.rs	composer_material_gate_model_proves_and_catches_draft_sampling	mr-composer-gate	
src/matrix_rain/mod.rs	deterministic_across_engines_and_clock_paths	mr-clock-path-equivalence	
src/matrix_rain/mod.rs	emission_never_rains_bottom_band_when_ring_unknown	mr-emission-mask	
src/matrix_rain/mod.rs	emits_over_trimmed_blank_rows	mr-occupancy-rescan	
src/matrix_rain/mod.rs	even_quad_counts_do_not_cancel	mr-fingerprint	
src/matrix_rain/mod.rs	exit_status_drives_wave_and_ember	mr-pending-notes	
src/matrix_rain/mod.rs	fingerprint_liveness_and_stability	mr-fingerprint	
src/matrix_rain/mod.rs	halo_fingerprint_tracks_center_radii_and_mode	mr-fingerprint	
src/matrix_rain/mod.rs	frame_detector_handles_many_unmatched_left_corners_without_masking	mr-frame-regions	
src/matrix_rain/mod.rs	frame_region_overflow_fails_closed_for_the_rescan	mr-frame-regions	
src/matrix_rain/mod.rs	literal_material_flows_in_source_order_down_each_column	mr-literal-strict	
src/matrix_rain/mod.rs	literal_material_slots_are_bounded_stable_and_never_faked	mr-material-sample	
src/matrix_rain/mod.rs	literal_material_tail_matches_an_exhaustive_forward_walk	mr-material-sample	
src/matrix_rain/mod.rs	literal_mode_never_emits_a_decorative_fallback	mr-literal-strict	
src/matrix_rain/mod.rs	live_selection_and_cursor_band_mask	mr-emission-mask	
src/matrix_rain/mod.rs	luminance_invariant_on_both_stock_themes	mr-luminance	
src/matrix_rain/mod.rs	material_bank_samples_output_not_typing	mr-material-sample	
src/matrix_rain/mod.rs	material_remaps_emitted_glyphs_unmirrored	mr-literal-strict	
src/matrix_rain/mod.rs	occupancy_mask_predicates	mr-occupancy-rescan	
src/matrix_rain/mod.rs	progressive_bake_through_the_engine	mr-engine-bake	
src/matrix_rain/mod.rs	quads_are_row_banded_and_tile_aligned	mr-quad-geometry	
src/matrix_rain/mod.rs	rain_activity_credit_derived_model_proves_and_catches_overflow	mr-activity-credit-refinement	
src/matrix_rain/mod.rs	rain_activity_credit_real_engine_conforms_to_model	mr-activity-credit-refinement	
src/matrix_rain/mod.rs	rain_lifecycle_conformance_real_engine_projects_onto_model	mr-lifecycle-refinement	
src/matrix_rain/mod.rs	reading_gates_suppress_emission	mr-gates-off	
src/matrix_rain/mod.rs	real_codex_and_claude_frames_feed_literal_output_not_composer	mr-material-sample	
src/matrix_rain/mod.rs	real_material_sampling_gate_conforms_and_defers_refresh	mr-composer-gate	
src/matrix_rain/mod.rs	semantic_pulse_derived_model_proves_and_catches_overflow	mr-semantic-pulse-refinement	
src/matrix_rain/mod.rs	semantic_pulse_real_engine_conforms_across_all_phases	mr-semantic-pulse-refinement	
src/matrix_rain/mod.rs	semantic_pulse_real_engine_priority_motion_reset_and_expiry_conform	mr-semantic-pulse-refinement	
src/matrix_rain/mod.rs	semantic_pulse_same_priority_replaces_phase_and_energy	mr-semantic-pulse-refinement	
src/matrix_rain/mod.rs	semantic_pulses_are_bounded_prioritized_and_shape_real_tape_lanes	mr-semantic-pulse-refinement	
src/matrix_rain/mod.rs	step_stable_damage_shape	mr-step-stable-damage	
src/matrix_rain/mod.rs	suspended_tick_starves_to_sleep_and_disarms	mr-drain-bound	
src/matrix_rain/mod.rs	tiny_pane_emits_without_panicking	mr-quad-geometry	
src/matrix_rain/mod.rs	turn_wave_fires_on_working_to_sleep	mr-weather-machine	
src/matrix_rain/mod.rs	turn_wave_limiter_caps_ignitions	mr-wave-limiter	
src/matrix_rain/mod.rs	typing_echo_never_pours	mr-weather-machine	
src/matrix_rain/mod.rs	unfocused_drained_engine_disarms_despite_streaming	mr-drain-bound	
src/matrix_rain/mod.rs	weather_transitions_with_dwell_hysteresis	mr-weather-machine	
src/matrix_rain/rom.rs	material_master_preserves_real_characters	rom-material-master	
src/matrix_rain/step.rs	a_completion_is_news_once_in_its_own_session	rl-completion-news	
src/matrix_rain/step.rs	a_surface_without_an_engine_keeps_both_latches_observing	rl-completion-news	
src/matrix_rain/step.rs	the_execute_edge_is_a_same_session_rising_edge	rl-execute-edge	
src/motion.rs	load_shed_exemption_is_exactly_functional_motion	mo-load-shed-exempt	
src/motion.rs	mode_parses_documented_spellings	mo-parse	
src/motion.rs	reduce_motion_never_mutes_typing	mo-reduce-motion-audible	
src/motion.rs	reduced_motion_totality	mo-resolve-truth-table	
src/motion.rs	serious_mode_policy_conforms_to_derived_transition_model	mo-serious-refinement	
src/motion.rs	serious_mode_policy_is_total_and_non_vacuous	mo-serious	
src/nova.rs	coupling_crossing_is_deterministic_and_monotone	nova-crossing	
src/nova.rs	delta_l_bound_over_palette_table	nova-delta-l	
src/nova.rs	nova_budget_geometry_invariant	nova-budget-geometry	
src/nova.rs	singularity_contracts_and_darkens	nova-singularity	
src/nova.rs	spend_order_truncation_ring_first_crown_drops	nova-spend-order	
src/nova_host_conformance_tests.rs	a_rearm_into_flash_is_the_buggy_step_the_host_bind_rejects	nova-host-refinement	
src/nova_host_conformance_tests.rs	the_real_host_conforms_to_nova_phase_model	nova-host-refinement	
src/nuke.rs	natural_sizes_stay_inside_the_baker_slot	nuke-nat-size	
src/nuke.rs	parts_live_only_in_their_phase	nuke-draw-windows	
src/nuke.rs	the_whole_window_is_finite_and_cools	nuke-draw-windows	
src/output_streak.rs	a_comet_paints_one_contiguous_run	os-comet-contiguous	
src/output_streak.rs	a_flood_coalesces_to_one_quiet_ribbon	os-flood	
src/output_streak.rs	a_two_row_alternation_cannot_strobe_the_ribbon	os-ignition-floor	
src/output_streak.rs	an_episode_is_one_pip_and_one_exhale	os-episode-sound	
src/output_streak.rs	an_unchanged_token_is_no_arrival_and_rebase_baselines	os-license	
src/output_streak.rs	cell_observation_all_blank_row_mints_nothing	os-cell-observation	
src/output_streak.rs	cell_observation_is_gated_by_the_host_token_before_geometry	os-cell-observation	
src/output_streak.rs	clipping_rejects_malformed_and_wholly_off_right_spans	os-clipped-span	
src/output_streak.rs	disabled_is_inert	os-inert	
src/output_streak.rs	echo_discount_mints_nothing_including_paste	os-license	
src/output_streak.rs	empty_damage_mints_nothing	os-license	
src/output_streak.rs	fractional_idle_deadline_is_consumed_at_the_exact_promised_boundary	os-idle-deadline	
src/output_streak.rs	huge_finite_idle_value_drains_immediately_without_panicking	os-idle-deadline	
src/output_streak.rs	idle_settles_to_fingerprint_zero_and_no_deadline	os-idle-deadline	
src/output_streak.rs	light_themes_get_shadow_not_invisible_additive_light	os-amplitude	
src/output_streak.rs	no_configuration_can_exceed_the_amplitude_ceiling	os-amplitude	
src/output_streak.rs	parked_episode_wakes_at_idle_cutoff_before_later_settle	os-idle-deadline	
src/output_streak.rs	reduced_motion_emits_nothing	os-inert	
src/output_streak.rs	submitted_turn_boundary_clears_only_older_echo_shadow	os-license	
src/output_streak.rs	the_spawn_gap_clamps_eye_and_ear_together	os-ignition-floor	
src/output_streak.rs	theme_hue_uses_named_stop_positions_with_bounded_non_wrapping_jitter	os-hue-for	
src/pane.rs	a_pane_clips_a_sprite_and_keeps_its_art_and_box_whole	pane-crop-sampling	
src/pet_baker.rs	a_cleared_baker_rebakes	exact-tile-baker-lru	
src/pet_baker.rs	bake_rate_is_capped_per_frame	exact-tile-baker-lru	
src/pet_baker.rs	degenerate_keys_are_refused_without_spending_a_bake	exact-tile-baker-lru	
src/pet_baker.rs	face_lod_keeps_a_dark_eye_core_at_ship_size	pb-eye-legibility	
src/pet_baker.rs	face_lod_rebuilds_reflections_but_culls_other_charm_ink	pb-face-lod-roles	
src/pet_baker.rs	host_ids_are_stable_and_distinct	pb-host-id	
src/pet_baker.rs	key_axes_change_the_texels	pb-roster	
src/pet_baker.rs	lru_holds_the_slot_ceiling_and_evicts_the_oldest	exact-tile-baker-lru	
src/pet_baker.rs	metric_change_clears_the_cache	exact-tile-baker-lru	
src/pet_baker.rs	retina_scale_bakes_hit_the_identity_path	pb-face-lod-roles	
src/pet_baker.rs	same_key_bakes_byte_identical_tiles	exact-tile-baker-lru	
src/pet_baker.rs	tile_is_exact_size_with_no_patch_strip	exact-tile-baker-lru	
src/pet_stroke.rs	a_stationary_pause_cannot_preserve_half_a_stroke	ps-stroke-contract	
src/pet_stroke.rs	a_still_pointer_cannot_earn_from_a_moving_or_breathing_body	ps-stroke-contract	
src/pet_stroke.rs	gaps_fast_sweeps_jitter_and_invalid_inputs_discard_partial_strokes	ps-stroke-contract	
src/pet_stroke.rs	periodic_still_frames_preserve_the_real_movement_clock	ps-still-frames-equivalence	
src/pet_stroke.rs	real_stroke_detector_transitions_conform_and_reject_negative_controls	ps-stroke-refinement	
src/pet_stroke.rs	scale_changes_do_not_change_the_stroke_threshold	ps-scale-invariance	
src/pet_world.rs	a_full_clear_moves_the_running_blocks_anchor_onto_what_followed_it	pw-anchors	
src/pet_world.rs	a_pane_local_default_background_decides_ink_column_by_column	pw-classify	
src/pet_world.rs	a_protected_cell_the_map_sees_vetoes_a_body_half_outside_it	pw-clearance-planes	
src/pet_world.rs	a_row_of_rule_strokes_across_the_pane_is_a_rule_line	host-line-head-law	
src/pet_world.rs	budget_overflow_and_missing_rows_are_unknown_not_empty	pw-clearance-planes	
src/pet_world.rs	cursor_protection_does_not_erase_or_invent_actual_ink	pw-classify	
src/pet_world.rs	every_source_a_reading_reads_moves_its_key	pw-reading-kept	
src/pet_world.rs	exact_cells_allow_a_real_gutter_and_protect_the_whole_body	pw-classify	
src/pet_world.rs	explicit_progress_is_sampled_without_claiming_completion	pw-progress	
src/pet_world.rs	ordinary_ink_and_protected_pixels_have_distinct_placement_planes	pw-clearance-planes	
src/pet_world.rs	outside_the_coverage_window_is_unknown_not_obstructed	pw-clearance-planes	
src/pet_world.rs	perch_search_finds_a_right_pocket_and_rejects_dense_screen	pw-perch	
src/pet_world.rs	a_step_off_protected_ground_keeps_the_nearest_column	pw-perch	
src/pet_world.rs	real_blocks_resolve_through_scroll_but_not_a_new_surface	pw-anchors	
src/pet_world.rs	reused_observations_match_fresh_maps_after_coverage_changes	pw-reuse-equals-fresh	
src/pet_world.rs	review_row_cursors_survive_skipped_and_sparse_query_orders	pw-rowscan-equivalence	
src/pet_world.rs	review_sparse_tail_implicit_cell_tracks_a_mid_row_background_seam	pw-classify	
src/pet_world.rs	selected_history_centers_the_budget_without_a_fake_caret	pw-coverage-window	
src/pet_world.rs	stale_frame_and_nonfinite_geometry_fail_closed	pw-coherence-gate	
src/pet_world.rs	the_caret_is_not_an_obstruction_to_its_own_escort	pw-clearance-planes	
src/pet_world.rs	the_row_cursors_answer_exactly_what_the_per_cell_lookups_do	pw-rowscan-equivalence	
src/pet_world.rs	typed_ink_reserve_is_ink_not_a_protected_surface	pw-classify	
src/pet_world.rs	visible_history_selection_survives_an_offscreen_active_endpoint	pw-selection-target	
src/pet_world.rs	wide_continuations_combining_and_expanded_lines_are_not_blank	pw-classify	
src/pipeline.rs	a_band_batch_translates_the_pipeline_engines_instead_of_resetting_them	pl-scroll-translation	
src/pipeline.rs	a_failed_command_hushes_the_caret_jump_fanfare_while_the_pet_grieves	pl-owner-fence	
src/pipeline.rs	a_torn_frame_delivers_the_execute_edge_on_the_next_fresh_snapshot	pl-rain-host-latches	
src/pipeline.rs	a_typed_trick_latches_through_the_pipeline_and_needs_ticks	pl-pet-input-latch	
src/pipeline.rs	automatic_wake_colors_follow_osc12_resets_and_theme_baselines	pl-auto-colors	
src/pipeline.rs	batched_keystrokes_spread_across_the_advance_delta	pl-key-replay	
src/pipeline.rs	chrome_geom_identity_and_padded_derivation	pl-chrome-geom	
src/pipeline.rs	composer_toggle_model_proves_and_real_pipeline_conforms	pl-composer-toggle-refinement	
src/pipeline.rs	cursor_pet_body_rect_is_handed_to_the_word_engine	pl-pet-word-handoff	
src/pipeline.rs	cursor_pet_hides_in_history_and_when_unfocused_but_keeps_its_identity	pl-pet-ownership	
src/pipeline.rs	cursor_pet_off_leaves_every_channel_and_fingerprint_untouched	pl-pet-ownership	
src/pipeline.rs	cursor_pet_retires_when_the_style_stops_naming_it	pl-pet-ownership	
src/pipeline.rs	cursor_pet_walks_beside_the_caret_then_sleeps_and_releases_cadence	pl-pet-ownership	
src/pipeline.rs	effects_off_and_non_fire_styles_leave_fire_streams_empty	pl-fire-streams	
src/pipeline.rs	embedded_pipeline_freezes_literal_tape_across_multiline_draft	pl-composer-toggle-refinement	
src/pipeline.rs	every_web_rainbow_kitty_spelling_is_v2	pl-glow-config	
src/pipeline.rs	explicit_wake_colors_and_laser_ignore_live_cursor_recolors	pl-auto-colors	
src/pipeline.rs	the_page_colours_its_cursor_as_a_native_window_does	pl-auto-colors	
src/pipeline.rs	fire_forge_cursor_fill_is_spliced_and_cleared_on_toggle	pl-fire-streams	
src/pipeline.rs	fire_glow_splices_field_streams_into_the_head_band	pl-fire-streams	
src/pipeline.rs	focus_does_not_alter_word_decoration_emission	pl-focus-gate	
src/pipeline.rs	glow_polarity_tracks_live_osc11_and_decscnm_background	pl-glow-polarity	
src/pipeline.rs	hidden_visibility_hard_drains_cursor_state_before_refocus_without_apply	pl-hidden-boundary	
src/pipeline.rs	hidden_visibility_retires_the_bodies_before_the_next_apply	pl-hidden-boundary	
src/pipeline.rs	lexicon_warnings_surface_and_respect_cjk_single_char_opt_in	pl-sparkle-config	
src/pipeline.rs	live_classic_to_literal_switch_resamples_without_grid_damage	pl-rain-integration	
src/pipeline.rs	matrix_rain_theme_updates_cached_and_live_config	pl-rain-integration	
src/pipeline.rs	next_deadline_ms_is_none_while_the_pet_moves_and_exact_while_it_breathes	pl-scheduler	
src/pipeline.rs	only_the_owner_fence_rebaselines_the_command_probe_not_a_hide_or_a_resize	pl-owner-fence	
src/pipeline.rs	output_streak_parks_until_its_exact_settle_deadline	pl-scheduler	
src/pipeline.rs	pipeline_history_uses_the_exact_frame_snapshot_and_retires_cursor_state	pl-history-lifecycle-refinement	
src/pipeline.rs	pipeline_real_alt_uniform_translates_while_ambiguous_motion_invalidates	pl-scroll-translation	
src/pipeline.rs	pipeline_rows_metrics_and_chrome_changes_reset_seeded_geometry	pl-coordinate-fence-refinement	
src/pipeline.rs	pipeline_scroll_baseline_regressions_and_wide_deltas_fail_closed	pl-scroll-translation	
src/pipeline.rs	pipeline_translates_real_zero_and_saturated_scrollback_exactly	pl-scroll-translation	
src/pipeline.rs	pipeline_typed_keys_light_and_cold_program_motion_stays_dark	pl-input-provenance	
src/pipeline.rs	pointer_and_petting_latch_only_and_need_a_tick	pl-pet-input-latch	
src/pipeline.rs	pointer_under_host_rows_is_read_where_the_frame_draws_the_pet	pl-pet-input-latch	
src/pipeline.rs	rain_enabled_emits_and_repeat_apply_is_byte_stable	pl-rain-integration	
src/pipeline.rs	rain_only_uses_engine_deadline_while_frame_effects_keep_raf	pl-scheduler	
src/pipeline.rs	rain_toggle_off_clears_stale_channels_on_every_off_path	pl-all-off-identity	
src/pipeline.rs	rain_torn_snapshot_clears_channels_and_fresh_frame_resumes	pl-rain-host-latches	
src/pipeline.rs	re_enabling_web_rain_replays_no_completion_from_while_it_was_off	pl-rain-host-latches	
src/pipeline.rs	reduced_motion_pins_the_web_pet_at_its_station	pl-pet-reduced-motion	
src/pipeline.rs	reduced_motion_turns_off_the_page_cursor_family	pl-reduced-motion-cursor	
src/pipeline.rs	refocus_resumes_without_a_phantom_comet	pl-focus-gate	
src/pipeline.rs	scrollback_reading_skips_scan_and_live_return_rescans	pl-rain-integration	
src/pipeline.rs	set_cursor_glow_resolves_through_the_native_config_law	pl-glow-config	
src/pipeline.rs	the_page_keeps_a_caret_touching_collision_form_provisional	pl-word-scan-context	
src/pipeline.rs	the_page_samples_a_peeking_cats_palette_over_its_footprint	pl-word-scan-context	
src/pipeline.rs	the_page_streak_lights_the_nova_channel_through_apply	pl-streak-channel	
src/pipeline.rs	the_pets_contrast_cursor_is_the_stamped_cursor_colour_or_the_terminals	pl-pet-contrast-cursor	
src/pipeline.rs	unfocused_glow_settles_so_the_host_stops_rebooking_the_pane	pl-scheduler	
src/pipeline.rs	unfocused_pane_emits_no_glow_but_focused_one_does	pl-focus-gate	
src/pipeline.rs	unfocused_trail_clears_its_comet_but_still_publishes_the_colour	pl-focus-gate	
src/pipeline.rs	v3_reset_table_knob_setters_hard_reset_and_alt_suppression_freezes	pl-sparkle-reset-table	
src/pipeline.rs	v3_web_resolver_pins_classes_and_custom_specs	pl-sparkle-config	
src/pipeline.rs	web_rain_hears_command_completions_and_execute_edges	pl-rain-host-latches	
src/pipeline_input_tests.rs	web_accepted_printables_conform_to_the_existing_input_credit_model	pl-input-credit-refinement	
src/pipeline_input_tests.rs	web_delete_dispatch_reaches_erase_without_a_typed_replay	pl-console-input-credits	
src/pipeline_input_tests.rs	web_glyph_classes_reach_the_real_rainbow_renderer	pl-web-glyph-class	
src/pipeline_input_tests.rs	web_non_text_intents_preserve_or_retire_the_existing_credit_pool	pl-console-input-credits	
src/pipeline_native.rs	the_flying_head_is_at_the_landing_on_frame_zero_and_squashes_on_arrival	pn-flying-head-meteor	
src/rainbow_kitty/companion.rs	a_claimed_frame_draws_no_second_body	rkc-body-for	
src/rainbow_kitty/companion.rs	a_crowded_line_yields_to_the_sky_then_draws_over_the_words	rkc-placement	
src/rainbow_kitty/companion.rs	an_ink_blind_host_keeps_the_shipped_seat_but_never_the_caret_cell	rkc-placement	
src/rainbow_kitty/companion.rs	the_flying_head_never_covers_the_cell_the_caret_is_typing_into	rkc-placement	
src/rainbow_kitty/companion.rs	at_the_line_s_end_the_head_rises_once_and_stays	rkc-placement	
src/rainbow_kitty/companion.rs	the_pet_is_offered_the_meteor_edge_but_keeps_its_own_pounce	rkc-impulse-for	
src/rainbow_kitty/meteor.rs	a_band_move_carries_a_flight_inside_the_band_and_retires_one_that_straddles_it	meteor-translate	
src/rainbow_kitty/meteor.rs	a_burst_stays_inside_its_quad_cap_at_every_grade	meteor-quad-caps	
src/rainbow_kitty/meteor.rs	a_chained_continuation_never_puts_the_head_behind_where_it_was	meteor-pool-cap-and-chain	
src/rainbow_kitty/meteor.rs	a_chained_stub_never_costs_another_row_its_head	meteor-pool-cap-and-chain	
src/rainbow_kitty/meteor.rs	a_fan_star_core_never_lands_on_a_probed_glyph	meteor-fan-geometry	
src/rainbow_kitty/meteor.rs	a_jet_breaks_over_a_glyph_and_the_star_survives	meteor-burst-coverage	
src/rainbow_kitty/meteor.rs	a_jet_is_probed_over_its_whole_capped_reach	meteor-const-relations	
src/rainbow_kitty/meteor.rs	a_party_landing_is_a_fan_off_the_glass_inside_315_ms	meteor-fan-census	
src/rainbow_kitty/meteor.rs	a_ping_pong_never_spends_more_than_its_under_share	meteor-quad-caps	
src/rainbow_kitty/meteor.rs	a_retired_head_sheds_nothing_more	meteor-retire-continuity	
src/rainbow_kitty/meteor.rs	a_row_zero_fan_may_use_the_head_band_but_never_the_tab_strip_beyond_it	meteor-fan-geometry	
src/rainbow_kitty/meteor.rs	a_scroll_between_spawn_and_arrival_moves_the_landing_with_the_train	meteor-translate	
src/rainbow_kitty/meteor.rs	a_second_meteor_retires_the_first_without_a_pop	meteor-retire-continuity	
src/rainbow_kitty/meteor.rs	a_spark_is_culled_exactly_where_the_cadence_law_says_it_is	spark-cull-exact	
src/rainbow_kitty/meteor.rs	a_spike_root_is_derived_from_the_spacing_it_must_keep_open	meteor-const-relations	
src/rainbow_kitty/meteor.rs	a_vertical_recall_meteor_draws_its_train	meteor-spawn-latch	
src/rainbow_kitty/meteor.rs	a_word_hop_is_a_mini_fan_not_a_flight	meteor-spawn-latch	
src/rainbow_kitty/meteor.rs	an_idle_meteor_pool_draws_nothing_and_names_no_deadline	meteor-spawn-latch	
src/rainbow_kitty/meteor.rs	only_one_head_is_in_the_air_per_row	meteor-pool-cap-and-chain	
src/rainbow_kitty/meteor.rs	reduced_motion_draws_the_static_form	meteor-reduced-motion-static	
src/rainbow_kitty/meteor.rs	the_attack_asks_for_every_frame_and_the_release_rides_the_host_s_train	meteor-cadence	
src/rainbow_kitty/meteor.rs	the_burst_is_off_glass_by_the_flight_horizon	meteor-idle-and-horizon	
src/rainbow_kitty/meteor.rs	the_burst_paints_inside_the_fan_s_rise_at_every_cell	meteor-burst-geometry	
src/rainbow_kitty/meteor.rs	the_head_is_already_moving_and_lit_on_frame_zero	meteor-frame0-head	
src/rainbow_kitty/meteor.rs	the_impact_is_off_glass_by_t_plus_600	meteor-idle-and-horizon	
src/rainbow_kitty/meteor.rs	the_landing_pool_conforms_to_the_model	landing-pool-fifo	
src/rainbow_kitty/meteor.rs	the_reduced_motion_burst_is_a_still_star	meteor-reduced-motion-static	
src/rainbow_kitty/meteor.rs	the_retire_decay_lands_on_the_chroma_cull	meteor-const-relations	
src/rainbow_kitty/meteor.rs	the_shower_stops_where_the_hand_stopped	meteor-hand-floor	
src/rainbow_kitty/meteor.rs	the_train_carries_the_whole_spectrum_on_every_frame_of_the_flight	meteor-train-spectrum	
src/rainbow_kitty/mod.rs	a_band_move_moves_the_caret_with_its_band_and_keeps_the_spine	engine-translate	
src/rainbow_kitty/mod.rs	a_batch_sweep_on_the_oldest_press_clock_is_born_no_earlier_than_the_birth_floor	held-key-and-sweep	
src/rainbow_kitty/mod.rs	a_bridge_sweep_inserted_after_a_one_shot_move_does_not_shift_the_pre_move_caret	pre-move-caret	
src/rainbow_kitty/mod.rs	a_cold_engine_s_first_coalesced_echo_lays_the_keys_it_held	echo-ledger-refinement	
src/rainbow_kitty/mod.rs	a_declined_row_change_moves_the_mirror_after_the_ingest_and_the_next_key_lays_on_the_new_row	caret-mirror-observation	
src/rainbow_kitty/mod.rs	a_disengaged_engine_is_inert_at_every_seam_point	engine-disengaged-inert	
src/rainbow_kitty/mod.rs	a_falling_curtain_is_reported_drawn_on_every_frame_it_composites	status-drawn-honesty	
src/rainbow_kitty/mod.rs	a_flight_asks_for_every_frame	meteor-cadence	
src/rainbow_kitty/mod.rs	a_fresh_engine_ignores_an_observed_caret_until_a_licensed_move_seeds_the_mirror	caret-mirror-observation	
src/rainbow_kitty/mod.rs	a_gold_star_near_a_contented_cat_is_offered_and_caught	pet-offer	
src/rainbow_kitty/mod.rs	a_held_space_measures_the_word_the_composer_moves_from_its_own_cell	word-relay	
src/rainbow_kitty/mod.rs	a_hide_is_taken_by_one_frame_and_an_un_taken_one_is_dropped_by_a_reset_or_a_curtain	curtain-and-hide	
src/rainbow_kitty/mod.rs	a_key_typed_before_the_first_licensed_move_is_held_not_laid_at_an_unset_mirror	held-key-and-sweep	
src/rainbow_kitty/mod.rs	a_late_tick_samples_the_settled_exit_swoosh	exit-sampling-refinement	
src/rainbow_kitty/mod.rs	a_licensed_jump_mints_one_meteor_cue_on_the_shared_clock	engine-sound-cues	
src/rainbow_kitty/mod.rs	a_pre_move_caret_carried_off_the_top_is_forgotten_not_clamped	pre-move-caret	
src/rainbow_kitty/mod.rs	a_reported_bar_mints_its_fan_and_a_withdrawn_one_mints_nothing	party-seam	
src/rainbow_kitty/mod.rs	a_reset_or_a_curtain_forgets_the_pre_move_caret_with_the_events_it_indexed	pre-move-caret	
src/rainbow_kitty/mod.rs	a_return_is_the_cadence_and_a_pty_feed_is_the_cascade	engine-sound-cues	
src/rainbow_kitty/mod.rs	a_rewrite_offers_no_impulse_drains_no_spine_throws_no_star_and_keeps_the_ledger	engine-rewrite	
src/rainbow_kitty/mod.rs	a_run_of_fast_clean_keys_opens_the_theme_and_a_delete_closes_it	spine-flow-counter	
src/rainbow_kitty/mod.rs	a_same_row_observation_leaves_the_mirror_for_the_ledger_to_measure	caret-mirror-observation	
src/rainbow_kitty/mod.rs	a_scroll_moves_every_live_star_and_the_caret_with_the_viewport	engine-translate	
src/rainbow_kitty/mod.rs	a_scroll_or_a_band_move_carries_the_pre_move_caret_with_the_text_it_names	pre-move-caret	
src/rainbow_kitty/mod.rs	a_settling_sky_asks_for_no_more_frames_than_its_light_can_show	engine-cadence-tail	
src/rainbow_kitty/mod.rs	a_slow_correction_is_not_a_mend	mend-pricing	
src/rainbow_kitty/mod.rs	a_style_switch_drops_an_unticked_event_and_never_a_minted_cue	engine-sound-cues	
src/rainbow_kitty/mod.rs	a_sub_floor_hop_is_one_nav_tick_and_a_no_op_is_silent	engine-sound-cues	
src/rainbow_kitty/mod.rs	a_sweep_on_the_origin_row_before_a_fold_move_lays_the_last_column_and_clears_the_ledger	echo-ledger-refinement	
src/rainbow_kitty/mod.rs	a_swept_space_requires_the_complete_key_run_and_typed_move	swept-space-cell	
src/rainbow_kitty/mod.rs	a_swept_space_uses_the_last_space_and_counts_wide_glyph_cells	swept-space-cell	
src/rainbow_kitty/mod.rs	a_typed_fold_wraps_at_the_pane_edge_not_the_grid_edge	pane-fold	
src/rainbow_kitty/mod.rs	a_typo_fixed_within_a_breath_is_born_at_the_momentum_it_interrupted	mend-pricing	
src/rainbow_kitty/mod.rs	a_waiting_key_s_arming_rows_have_slots_of_their_own	witness-rows	
src/rainbow_kitty/mod.rs	a_withheld_follow_lifts_the_hand_once	crossed-echo-laid	
src/rainbow_kitty/mod.rs	a_witness_retirement_is_counted_survives_a_reset_and_is_never_undone_by_a_payout	witness-retire-engine	
src/rainbow_kitty/mod.rs	an_erase_winces_the_kitty_and_a_meteor_outranks_it	engine-companion-impulse	
src/rainbow_kitty/mod.rs	an_inserts_sweep_pays_the_hole_a_late_key_left_before_it	echo-ledger-refinement	
src/rainbow_kitty/mod.rs	an_unprobed_sky_bears_no_star	sky-probe-gate	
src/rainbow_kitty/mod.rs	an_unrelated_follow_on_the_parked_row_does_not_prove_the_park	short-wrap-park-refinement	
src/rainbow_kitty/mod.rs	delight_earns_one_hero_that_chimes_only_with_a_token	glint-budget	
src/rainbow_kitty/mod.rs	flow_draws_nothing_of_its_own	flow-draws-nothing	
src/rainbow_kitty/mod.rs	forty_presses_typed_into_a_stall_fit_the_ledger	echo-ledger-refinement	
src/rainbow_kitty/mod.rs	real_band_transforms_retire_echo_credits_without_resetting_earned_light	echo-band-fence	
src/rainbow_kitty/mod.rs	reduced_motion_lands_the_kitty_without_a_flight_or_a_flash	engine-companion-impulse	
src/rainbow_kitty/mod.rs	the_cadence_fold_floors_tails_and_ignores_past_edges	cadence-fold	
src/rainbow_kitty/mod.rs	the_caret_flare_is_one_edge_that_ages_out	caret-seam-flare-and-paint	
src/rainbow_kitty/mod.rs	the_caret_paint_is_the_display_spine_floored_by_the_landing_relight	caret-seam-flare-and-paint	
src/rainbow_kitty/mod.rs	the_fingerprint_folds_only_this_ticks_own_marks	frame-liveness-fingerprint	
src/rainbow_kitty/mod.rs	the_hands_floor_rides_with_its_line	crossed-echo-laid	
src/rainbow_kitty/mod.rs	the_mend_mark_is_a_value_not_a_timer	mend-pricing	
src/rainbow_kitty/mod.rs	the_meteor_pool_is_handed_the_hand_s_floor	meteor-hand-floor	
src/rainbow_kitty/mod.rs	the_meteor_s_perk_reaches_the_resident_at_the_landing_s_own_instant	pet-offer	
src/rainbow_kitty/mod.rs	the_real_engine_conforms_to_the_echo_ledger_model	echo-ledger-refinement	
src/rainbow_kitty/mod.rs	the_seam_rides_with_the_hands_line	crossed-echo-laid	
src/rainbow_kitty/mod.rs	three_disjoint_cohorts_keep_the_hands_lower_follow_row_in_the_witness	witness-rows	
src/rainbow_kitty/mod.rs	withheld_is_what_the_host_was_asked_for_and_did_not_deliver	witness-withheld	
src/rainbow_kitty/ribbon.rs	a_backspace_run_begun_after_the_rest_snaps_the_band_back_and_runs_the_tail_from_it	row-hold-own-clock	
src/rainbow_kitty/ribbon.rs	a_backspace_run_holds_the_phrase_so_the_left_end_stays_lit_through_it	row-hold-own-clock	
src/rainbow_kitty/ribbon.rs	a_backward_typed_echo_under_the_re_anchor_floor_stamps_nothing	typed-landing-pen	
src/rainbow_kitty/ribbon.rs	a_band_carried_down_off_the_caret_s_row_drifts_into_its_kept_start_and_never_past_it	fold-flow	
src/rainbow_kitty/ribbon.rs	a_band_move_carries_the_composer_band_down_and_leaves_the_transcript_alone	ribbon-translate	
src/rainbow_kitty/ribbon.rs	a_band_move_drops_what_leaves_the_band_and_retires_its_cohort	ribbon-translate	
src/rainbow_kitty/ribbon.rs	a_band_move_under_the_hand_carries_the_seam_and_its_floor_together	ribbon-translate	
src/rainbow_kitty/ribbon.rs	a_boundary_takes_the_brighter_cell_s_coverage	attack-and-boundary	
src/rainbow_kitty/ribbon.rs	a_boundary_two_live_runs_share_carries_the_older_side_s_light	seam-heal-no-slivers	
src/rainbow_kitty/ribbon.rs	a_capped_rail_does_not_cut_the_body_of_an_adjacent_run	ribbon-quad-budget	
src/rainbow_kitty/ribbon.rs	a_capped_run_keeps_the_light_at_its_head_and_sheds_the_distant_tail	ribbon-quad-budget	
src/rainbow_kitty/ribbon.rs	a_coalesced_echo_leaves_one_cohort_and_no_feather_inside_the_row	seam-heal-no-slivers	
src/rainbow_kitty/ribbon.rs	a_cohort_still_being_typed_keeps_its_boundaries_while_its_row_mate_retracts	row-hold-own-clock	
src/rainbow_kitty/ribbon.rs	a_cold_caret_mirror_is_not_a_hand_a_key_can_be_brought_back_to	typed-landing-pen	
src/rainbow_kitty/ribbon.rs	a_composer_gate_waits_for_its_home_and_only_its_first_real_echo_spends_it	composer-newline-gate-refinement	
src/rainbow_kitty/ribbon.rs	a_composer_re_wrap_s_moved_word_fades_in_place_while_the_row_it_left_drifts	fold-flow	
src/rainbow_kitty/ribbon.rs	a_curtain_gathers_the_band_into_its_hand_over_a_quarter_second_with_no_cliff_and_lays_nothing	curtain-law	
src/rainbow_kitty/ribbon.rs	a_delivered_burst_that_wraps_keeps_the_cells_it_left_on_the_row_above	typed-landing-pen	
src/rainbow_kitty/ribbon.rs	a_downward_landing_is_questioned_only_where_lay_would_write_above_it	typed-landing-pen	
src/rainbow_kitty/ribbon.rs	a_fades_next_level_is_solved_not_polled	level-step-closed-form	
src/rainbow_kitty/ribbon.rs	a_hop_covers_a_leaving_band_from_its_level_and_leaves_a_live_one_as_it_is	wake-overlay-no-touch	
src/rainbow_kitty/ribbon.rs	a_hot_paragraph_at_retina_keeps_its_head_row_whole_and_sheds_only_the_oldest_tail	ribbon-quad-budget	
src/rainbow_kitty/ribbon.rs	a_jump_lays_its_wake_over_the_live_band_and_leaves_the_band_s_cells_untouched	wake-overlay-no-touch	
src/rainbow_kitty/ribbon.rs	a_jump_s_cap_binds_at_the_corridor_s_far_edge_never_inside_it	wake-corridor	
src/rainbow_kitty/ribbon.rs	a_jump_s_landing_column_is_stamped_on_its_own_row_alone	abandon-retract	
src/rainbow_kitty/ribbon.rs	a_jump_takes_the_band_out_from_the_light_it_has_never_by_a_step	abandon-retract	
src/rainbow_kitty/ribbon.rs	a_key_sampled_at_readlines_cr_park_stays_on_the_hands_own_row	typed-landing-pen	
src/rainbow_kitty/ribbon.rs	a_light_theme_ribbon_is_source_over_ink_and_never_additive	light-theme-and-spellings	light-glass-geometry
src/rainbow_kitty/ribbon.rs	a_line_still_being_typed_keeps_every_cell_from_its_first_key_to_the_caret	cohort-phase-timeline	
src/rainbow_kitty/ribbon.rs	a_live_retype_with_a_changed_glyph_does_not_inherit_the_old_witness	retire-and-release-identity	
src/rainbow_kitty/ribbon.rs	a_nav_moves_the_pen_so_a_fold_after_it_still_lays_where_it_folded	typed-landing-pen	
src/rainbow_kitty/ribbon.rs	a_new_phrase_holds_none_of_a_closed_one_s_cohorts_until_the_hand_resumes_it	row-hold-own-clock	
src/rainbow_kitty/ribbon.rs	a_parking_pass_after_the_row_was_backspaced_empty_still_finds_the_hand	typed-landing-pen	
src/rainbow_kitty/ribbon.rs	a_parking_pass_is_caught_at_every_column_it_can_be_sampled_at	typed-landing-pen	
src/rainbow_kitty/ribbon.rs	a_partial_release_is_restored_by_its_stamped_cells_when_a_key_joined_the_cohort	retire-and-release-identity	
src/rainbow_kitty/ribbon.rs	a_partial_reversal_keeps_the_whole_corridor_on_one_clock	wake-corridor	
src/rainbow_kitty/ribbon.rs	a_ping_pong_at_speed_does_not_accumulate_and_empties_one_wake_life_after_its_last_key	wake-corridor	
src/rainbow_kitty/ribbon.rs	a_program_round_trip_does_not_strand_the_band_under_a_typing_hand	abandon-retract	
src/rainbow_kitty/ribbon.rs	a_re_anchor_inside_the_scroll_that_moved_the_band_drains_nothing	ribbon-translate	
src/rainbow_kitty/ribbon.rs	a_relay_over_a_content_retired_cell_has_a_fresh_witness_identity	retire-and-release-identity	
src/rainbow_kitty/ribbon.rs	a_released_run_is_abandoned_into_its_retract_and_a_cell_already_leaving_keeps_its_clock	retire-and-release-identity	
src/rainbow_kitty/ribbon.rs	a_relocation_pays_a_content_retired_destination_and_preserves_live_neighbors	retire-and-release-identity	
src/rainbow_kitty/ribbon.rs	a_repaints_home_pass_does_not_abandon_the_row_the_hand_is_typing_on	typed-landing-pen	
src/rainbow_kitty/ribbon.rs	a_repaints_home_pass_poisons_the_mirror_and_the_pen_still_holds_the_hand	typed-landing-pen	
src/rainbow_kitty/ribbon.rs	a_retired_cell_melts_to_exactly_nothing_inside_150_ms_and_never_brightens	retire-and-release-identity	
src/rainbow_kitty/ribbon.rs	a_rewrite_retracts_the_suffix_from_the_given_column_not_the_mirror	erase-retract-suffix	
src/rainbow_kitty/ribbon.rs	a_row_change_a_pty_jump_and_a_typed_move_lay_no_wake	wake-corridor	
src/rainbow_kitty/ribbon.rs	a_row_the_caret_is_not_on_retracts_into_its_own_nearest_end_in_logical_order	abandon-retract	
src/rainbow_kitty/ribbon.rs	a_scroll_carries_the_ribbon_with_its_text	ribbon-translate	
src/rainbow_kitty/ribbon.rs	a_scrub_inside_the_band_keeps_every_cell_s_light_and_holds_the_row	scrub-holds-and-damages-nothing	
src/rainbow_kitty/ribbon.rs	a_scrub_into_a_leftward_corridor_does_not_move_its_soft_end	scrub-holds-and-damages-nothing	
src/rainbow_kitty/ribbon.rs	a_shell_home_relays_no_word_onto_the_prompt	word-relay	
src/rainbow_kitty/ribbon.rs	a_shift_of_a_flowing_band_carries_its_frozen_fold_edge	fold-flow	
src/rainbow_kitty/ribbon.rs	a_slow_hand_s_folded_row_flows_whole_whatever_it_paused_before_the_wrap	fold-flow	
src/rainbow_kitty/ribbon.rs	a_top_anchored_band_moves_the_transcript_up_and_pins_the_footer	ribbon-translate	
src/rainbow_kitty/ribbon.rs	a_typed_key_under_a_burning_ember_clears_it_and_relights_from_the_level	focus-ember	
src/rainbow_kitty/ribbon.rs	a_typed_row_change_lets_the_old_row_leave_on_its_own_clock	fold-flow	
src/rainbow_kitty/ribbon.rs	an_arrow_hop_lays_a_short_wake_behind_the_caret_and_leaves_live_cells_alone	wake-corridor	
src/rainbow_kitty/ribbon.rs	an_erase_whose_echo_lands_a_tick_late_still_retracts_the_erased_cell	erase-retract-suffix	
src/rainbow_kitty/ribbon.rs	focus_regained_inside_the_ember_comes_back_up_through_edge_in_not_a_snap	focus-ember	
src/rainbow_kitty/ribbon.rs	letters_stay_legible_under_the_ribbon_on_every_dark_theme_above_the_floor	bed-legibility-bar	
src/rainbow_kitty/ribbon.rs	the_attach_and_the_seam_law_hold_at_one_slab_per_cell	caret-attach	
src/rainbow_kitty/ribbon.rs	the_band_stops_at_the_focused_pane_s_floor_not_the_grid_s	body-ceiling-rail-and-pane	
src/rainbow_kitty/ribbon.rs	the_block_stands_on_whole_light_after_a_jump_and_the_keys_that_follow_it	caret-attach	
src/rainbow_kitty/ribbon.rs	the_block_stays_on_the_band_while_the_cell_under_it_leaves_and_when_it_is_gone	caret-attach	
src/rainbow_kitty/ribbon.rs	the_body_never_brightens_after_the_last_keystroke	swoosh-monotone-retract	
src/rainbow_kitty/ribbon.rs	the_caret_block_stands_on_the_band_under_a_cold_hand_and_a_hot_one	caret-attach	
src/rainbow_kitty/ribbon.rs	the_exit_swoosh_retracts_toward_the_caret_and_reaches_exactly_zero	swoosh-monotone-retract	
src/rainbow_kitty/ribbon.rs	the_feather_grows_in_as_a_drained_neighbour_goes_never_in_one_frame	seam-heal-no-slivers	
src/rainbow_kitty/ribbon.rs	the_field_index_answers_what_a_newest_first_scan_would	field-index-is-newest-scan	
src/rainbow_kitty/ribbon.rs	the_flat_spelling_draws_no_rail	light-theme-and-spellings	
src/rainbow_kitty/ribbon.rs	the_focus_ember_never_opens_a_dark_run_inside_the_band	focus-ember	
src/rainbow_kitty/ribbon.rs	the_hand_arriving_back_through_a_shell_fold_takes_the_row_back_after_the_rest_and_alone	fold-flow	
src/rainbow_kitty/ribbon.rs	the_hot_edge_embers_out_with_its_body_on_focus_loss	hot-edge-emission	
src/rainbow_kitty/ribbon.rs	the_hot_edge_lives_only_at_the_head_on_a_dark_ground	hot-edge-emission	
src/rainbow_kitty/ribbon.rs	the_reach_stops_at_the_hand_s_own_first_column	reach-hand-floor	
src/rainbow_kitty/ribbon.rs	the_underline_keeps_to_a_last_rows_foot	band-geometry	
src/rainbow_kitty/ribbon.rs	the_walk_is_a_function_of_position_so_a_retyped_cell_takes_back_its_stop	cohort-walk-invariant	
src/rainbow_kitty/ribbon.rs	the_walk_s_pace_is_the_arc_s_own_perceptual_step	walk-pace-perceptual	
src/rainbow_kitty/ribbon.rs	the_wave_breathes_a_tenth_of_a_cell_at_full_momentum_inside_the_body_s_own_rows	band-geometry	
src/rainbow_kitty/ribbon.rs	under_reduced_motion_a_flowing_band_asks_for_no_wake_before_its_fade	fold-flow	
src/rainbow_kitty/ribbon.rs	under_reduced_motion_the_exit_swoosh_moves_nothing_and_only_fades	swoosh-monotone-retract	
src/rainbow_kitty/spine.rs	a_backspace_leaves_a_mark_that_counts_the_run_and_a_typed_key_closes_it	spine-erase-mark	
src/rainbow_kitty/spine.rs	a_big_rise_slams_faster_than_it_attacks_and_the_slam_is_latched	spine-follower-step	
src/rainbow_kitty/spine.rs	a_held_key_cannot_farm_the_combo	spine-flow-counter	
src/rainbow_kitty/spine.rs	deletes_drain_and_never_build	spine-deletes-never-build	
src/rainbow_kitty/spine.rs	idle_drains_to_exactly_zero	spine-idle-snaps-to-exact-zero	
src/rainbow_kitty/spine.rs	resume_floors_births_without_touching_the_honest_spine	spine-birth-price	
src/rainbow_kitty/spine.rs	the_first_key_after_a_long_green_run_is_born_hot	spine-birth-price	
src/rainbow_kitty/spine.rs	the_follower_is_frame_rate_independent	spine-follower-rate-independence	
src/rainbow_kitty/spine.rs	the_follower_settles_on_the_gained_metric_and_reports_the_raw_one	spine-gain-target	
src/rainbow_kitty/spine.rs	the_phase_clock_rides_the_spine_and_stays_on_its_ring	spine-ranges-and-phase-ring	
src/rainbow_kitty/stardust.rs	a_band_move_moves_stars_in_the_band_and_clears_the_probe	sd-translate	
src/rainbow_kitty/stardust.rs	a_cell_less_geometry_falls_back_to_one_chain	sd-sky-index-equivalence	
src/rainbow_kitty/stardust.rs	a_fan_star_is_born_at_full_and_clears_its_cell_inside_its_hold	sd-throw-kinematics	
src/rainbow_kitty/stardust.rs	a_field_star_is_pinned_and_finishes_with_its_cell	sd-envelopes	
src/rainbow_kitty/stardust.rs	a_field_star_rides_its_cell_s_envelope_not_the_grain_s_life	sd-envelopes	
src/rainbow_kitty/stardust.rs	a_gold_star_walks_from_the_deal_through_the_paw_to_the_catch	sd-catch-point-update	
src/rainbow_kitty/stardust.rs	a_grain_opens_and_closes_at_least_twice_in_its_life	sd-scintillation	
src/rainbow_kitty/stardust.rs	a_lay_too_wide_to_file_spills_the_index_and_still_answers	sd-sky-index-equivalence	
src/rainbow_kitty/stardust.rs	a_light_rule_over_the_typing_row_keeps_the_sky_alive	sd-birth-gate	
src/rainbow_kitty/stardust.rs	a_mend_s_star_is_born_where_the_typo_was_and_dragged_to_the_fix	sd-mend-pull	
src/rainbow_kitty/stardust.rs	a_paid_hero_cues_one_glint_at_its_own_column	sd-glint-bucket	
src/rainbow_kitty/stardust.rs	a_row_zero_hero_clipped_off_the_glass_spends_nothing	sd-glint-bucket	
src/rainbow_kitty/stardust.rs	a_scroll_moves_the_sky_and_forgets_the_probe	sd-translate	
src/rainbow_kitty/stardust.rs	a_sky_star_keeps_its_centre_when_the_probe_forgets_its_row	sd-probe-pricing	
src/rainbow_kitty/stardust.rs	a_thrown_core_is_cleared_where_it_rests_nudged_walked_back_or_dropped	sd-settle	
src/rainbow_kitty/stardust.rs	a_transient_over_ink_or_unknown_is_held_to_a_third	sd-probe-pricing	
src/rainbow_kitty/stardust.rs	an_earned_hero_is_never_demoted_by_spacing_but_a_dealt_one_is	sd-deal-rules	
src/rainbow_kitty/stardust.rs	an_exhaust_grain_rests_by_the_probe_and_not_by_its_home_cell	sd-settle	
src/rainbow_kitty/stardust.rs	an_idle_sky_draws_exactly_nothing	sd-envelopes	
src/rainbow_kitty/stardust.rs	an_open_theme_holds_the_sky_open_through_a_dip	sd-deal-rules	
src/rainbow_kitty/stardust.rs	an_over_budget_frame_keeps_every_star_s_core	sd-frame-budgets	
src/rainbow_kitty/stardust.rs	band_moves_follow_the_earning_glyph_at_sky_and_footer_edges	sd-translate	
src/rainbow_kitty/stardust.rs	cell_ink_classifies_the_light_horizontal_rules_and_nothing_else	sd-glyph-probe	
src/rainbow_kitty/stardust.rs	every_v2_mark_forks_to_source_over_on_light	sd-paint-recipes	
src/rainbow_kitty/stardust.rs	focus_loss_embers_the_sky_instead_of_popping_it	sd-envelopes	
src/rainbow_kitty/stardust.rs	halos_are_budgeted_per_star_not_per_split	sd-frame-budgets	
src/rainbow_kitty/stardust.rs	hero_births_and_glints_share_one_budget	sd-glint-bucket	
src/rainbow_kitty/stardust.rs	no_mark_crosses_ink_or_the_pane_edge_at_any_instant_under_either_band_anchor	sd-flight-stays-clear	
src/rainbow_kitty/stardust.rs	no_star_core_is_ever_born_over_a_probed_glyph	sd-birth-gate	
src/rainbow_kitty/stardust.rs	no_star_outshines_the_caret_s_floor_share_on_the_shipped_ground	sd-caret-light-ceiling	
src/rainbow_kitty/stardust.rs	no_two_sky_stars_share_a_pixel	sd-deal-rules	
src/rainbow_kitty/stardust.rs	one_sky_star_per_cell_and_no_pile_outshines_the_caret	sd-deal-rules	
src/rainbow_kitty/stardust.rs	reduced_motion_freezes_both_clocks_and_takes_the_linear_fade	sd-scintillation	
src/rainbow_kitty/stardust.rs	sky_ownership_survives_uniform_scroll_before_a_band_move	sd-translate	
src/rainbow_kitty/stardust.rs	stars_over_the_cap_finish_oldest_first_within_forty_ms	sd-pool-cap	
src/rainbow_kitty/stardust.rs	the_aurora_leaves_with_the_ribbon	sd-aurora	
src/rainbow_kitty/stardust.rs	the_aurora_rises_with_momentum_and_never_enters_a_glyph_row	sd-aurora	
src/rainbow_kitty/stardust.rs	the_blank_frontiers_early_stop_is_the_whole_walks_answer	sd-headroom-and-frontier	
src/rainbow_kitty/stardust.rs	the_closer_dims_a_star_by_one_factor_until_its_arms_fit_and_leaves_one_that_fits	sd-caret-light-ceiling	
src/rainbow_kitty/stardust.rs	the_combo_ladder_pays_at_sixteen_thirty_two_and_sixty_four	sd-deal-rules	
src/rainbow_kitty/stardust.rs	the_exhaust_costs_the_frame_train_its_own_window_and_no_more	sd-brisk-cadence	
src/rainbow_kitty/stardust.rs	the_exhaust_flies_along_a_rule_row_in_the_sky_zone_under_the_stroke	sd-flight-stays-clear	
src/rainbow_kitty/stardust.rs	the_exhaust_floors_its_spine_on_the_mend_s	sd-exhaust	
src/rainbow_kitty/stardust.rs	the_exhaust_needs_a_blank_cell_behind_the_caret	sd-exhaust	
src/rainbow_kitty/stardust.rs	the_exhaust_never_chimes_skips_a_space_and_deals_its_m2_only_at_a_hot_spine	sd-exhaust	
src/rainbow_kitty/stardust.rs	the_frame_that_echoes_the_key_already_shows_its_field_star	sd-envelopes	
src/rainbow_kitty/stardust.rs	the_glint_toggle_is_solved_not_polled	sd-solved-deadlines	
src/rainbow_kitty/stardust.rs	the_headroom_under_a_rule_reads_every_column_the_flight_crosses	sd-headroom-and-frontier	
src/rainbow_kitty/stardust.rs	the_pixel_probe_answers_ink_only_inside_a_rules_stroke_band	sd-glyph-probe	
src/rainbow_kitty/stardust.rs	the_population_cap_never_refuses_a_newborn	sd-pool-cap	
src/rainbow_kitty/stardust.rs	the_second_grain_is_priced_on_the_honest_spine	sd-deal-rules	
src/rainbow_kitty/stardust.rs	the_size_clocks_next_step_is_solved_not_polled	sd-solved-deadlines	
src/rainbow_kitty/stardust.rs	the_sky_is_at_rest_one_star_life_after_the_last_key	sd-throw-kinematics	
src/rainbow_kitty/stardust.rs	the_slot_hash_is_injective_over_every_box_the_cap_allows	sd-sky-index-equivalence	
src/rainbow_kitty/stardust.rs	the_stream_step_is_solved_not_polled	sd-solved-deadlines	
src/rainbow_kitty/stardust.rs	the_stroke_band_is_one_set_of_pixels_for_the_probe_the_zones_and_the_headroom	sd-glyph-probe	
src/rainbow_kitty/timing.rs	the_seven_curves_hit_their_endpoints	named-easing-curves	
src/rainbow_kitty/witness.rs	a_band_move_carries_the_records_inside_it_drops_what_leaves_and_keeps_the_list_sorted	wit-evidence-carriage	
src/rainbow_kitty/witness.rs	a_band_move_that_opens_a_row_beside_a_line_leaves_it_unseen_until_a_frame_shows_it	wit-evidence-carriage	
src/rainbow_kitty/witness.rs	a_carried_record_starts_over_on_its_new_row_as_if_just_armed	wit-evidence-carriage	
src/rainbow_kitty/witness.rs	a_copy_that_appears_beside_a_standing_line_is_a_twin_once_it_has_stood_forty_ms	wit-evidence	
src/rainbow_kitty/witness.rs	a_different_glyph_retires_and_a_relaid_cell_arms_afresh	wit-walk-cell-verdict	
src/rainbow_kitty/witness.rs	a_glyph_landing_on_a_blank_with_the_rewrite_that_took_its_run_goes_with_the_run	wit-walk-blanks	
src/rainbow_kitty/witness.rs	a_partial_release_of_one_glyph_leaves_the_run_s_spaces_standing	wit-released-lifecycle	
src/rainbow_kitty/witness.rs	a_partial_release_takes_no_space_from_between_standing_letters	wit-walk-blanks	
src/rainbow_kitty/witness.rs	a_record_the_shape_pass_caught_up_is_armed_again	wit-evidence	
src/rainbow_kitty/witness.rs	a_released_record_is_kept_so_a_glyph_landing_under_it_later_melts_fast_and_counts_once	wit-released-lifecycle	
src/rainbow_kitty/witness.rs	a_released_run_s_never_armed_cell_is_held_with_it_and_a_glyph_on_it_melts_the_run	wit-released-lifecycle	
src/rainbow_kitty/witness.rs	a_row_a_scroll_brings_in_dates_its_copy_from_the_last_frame_before_the_scroll	wit-evidence-carriage	
src/rainbow_kitty/witness.rs	a_run_beside_a_withheld_row_waits_one_walk	wit-withheld-deferral	
src/rainbow_kitty/witness.rs	a_shift_onto_its_own_left_behind_tail_leaves_one_record_per_key	wit-evidence-carriage	
src/rainbow_kitty/witness.rs	a_short_park_cannot_relax_an_unrelated_run	wit-follow-runs	
src/rainbow_kitty/witness.rs	a_stale_cell_under_a_fresh_one_is_named_by_its_own_birth_and_the_fresh_one_never	wit-walk-cell-verdict	
src/rainbow_kitty/witness.rs	a_twin_testifies_only_about_its_own_row	wit-follow-runs	
src/rainbow_kitty/witness.rs	a_wide_glyph_s_two_cells_are_retired_as_one_unit	wit-walk-cell-verdict	
src/rainbow_kitty/witness.rs	a_word_the_wrap_relaid_at_its_row_s_start_does_not_count_against_the_line	wit-follow-runs	
src/rainbow_kitty/witness.rs	an_insert_names_the_tail_it_pushed_with_its_width	wit-shift-runs	
src/rainbow_kitty/witness.rs	an_unsampled_row_keeps_its_records_and_a_dropped_cell_is_forgotten	wit-walk-cell-verdict	
src/rainbow_kitty/witness.rs	blank_to_glyph_only_arms_and_glyph_to_blank_releases	wit-walk-cell-verdict	
src/rainbow_kitty/witness.rs	shifted_records_find_their_glyphs_on_the_next_walk	wit-shift-runs	
src/rainbow_kitty/witness.rs	standing_run_index_matches_the_retirement_oracle	wit-standing-run-index	
src/rainbow_kitty/witness.rs	the_deferral_s_scratch_is_reserved_and_never_grows	wit-withheld-deferral	
src/rainbow_kitty/witness.rs	the_last_walk_s_blanks_ride_with_their_cells	wit-evidence-carriage	
src/rainbow_kitty/witness.rs	the_witness_is_bounded_and_allocates_nothing_past_its_cap	wit-record-list-invariant	
src/sound_policy.rs	bonk_gain_policy_gates_master_focus_knob_motion_and_volume	sound-gain-laws	
src/spec.rs	checked_in_toy_pack_compiles_through_real_dispatch_and_scanner	toy-pack-denotation	
src/spec.rs	consumer_capabilities_name_only_actual_shared_tuning_consumers	spec-consumer-capabilities	
src/spec.rs	custom_fragment_parses_and_keys_all_hash_variants	custom-spec-table-keys	
src/spec.rs	custom_twotone_requires_exactly_two_colors_and_runtime_falls_back	custom-colorway-parse	
src/spec.rs	empty_spec_and_malformed_fragment_fail_safely	custom-spec-table-keys	
src/spec.rs	every_checked_in_community_toy_pack_compiles	shipped-packs-compile	
src/spec.rs	legacy_inline_customs_keep_their_lenient_compatibility_semantics	custom-spec-table-keys	
src/spec.rs	spec_table_overlay_is_deterministic_and_later_wins	spec-table-overlay	
src/spec.rs	toy_pack_file_read_is_regular_utf8_and_bounded	file-feed-bounded-regular-read	
src/spec.rs	toy_pack_recipe_word_count_is_bounded	toy-pack-validation	
src/spec.rs	toy_pack_rejects_possessive_dispatch_collisions	toy-pack-validation	
src/spec.rs	toy_pack_schema_is_strict_about_unknown_fields_and_values	toy-pack-validation	
src/spec.rs	toy_pack_semantics_collect_version_identity_budget_and_timing_errors	toy-pack-validation	
src/spec.rs	toy_pack_source_cap_precedes_toml_parse	toy-pack-source-cap	
src/spec.rs	unscannable_custom_words_warn_on_the_conflicts_channel	custom-spec-table-keys	
src/spectrum.rs	a_colour_at_or_over_the_floor_and_every_grey_is_returned_bit_for_bit	spectrum-min-saturation	
src/spectrum.rs	spectrum_endpoints_are_the_named_red_and_violet	spectrum-read-and-snap	
src/spectrum.rs	spectrum_hue_climbs_from_red_to_violet_without_reversing	spectrum-lut-table-properties	
src/spectrum.rs	spectrum_lut_has_bounded_adjacent_steps	spectrum-lut-table-properties	
src/spectrum.rs	spectrum_lut_is_byte_reproducible_from_its_generator	spectrum-lut-is-generated	
src/spectrum.rs	spectrum_reproduces_its_anchors_exactly	spectrum-lut-table-properties	
src/spectrum.rs	spectrum_snap_resolves_to_one_of_the_seven_names	spectrum-read-and-snap	
src/spectrum.rs	spectrum_stays_saturated_across_the_whole_arc	spectrum-lut-table-properties	
src/spectrum.rs	the_floor_is_reached_with_hue_and_value_untouched	spectrum-min-saturation	
src/supernova.rs	combo_baseline_is_byte_identical	supernova-tier-ladder	
src/supernova.rs	combo_ladder_climbs_to_guaranteed_nuke	supernova-tier-ladder	
src/supernova.rs	detonation_has_a_hard_aggregate_readability_ceiling	emit-super-quads	
src/supernova.rs	detonation_is_theme_branched	emit-super-quads	
src/supernova.rs	light_charge_and_debris_ride_over_stream	emit-super-decos	
src/supernova.rs	overlap_bound_preserves_falloff_and_finds_crossed_edges	additive-peak-exact	
src/supernova.rs	peak_sweep_is_bit_identical_to_exhaustive_probe	additive-peak-exact	
src/supernova.rs	phases_live_in_their_windows	emit-super-quads	
src/supernova.rs	r_max_for_matches_engine_clamp	supernova-reach	
src/supernova.rs	shade_peak_probe_is_bit_identical_to_quadratic_reference	shade-peak-exact	
src/supernova.rs	super_budget_s_max_bound	emit-super-quads	
src/supernova.rs	super_deco_stream_bounded	emit-super-decos	
src/supernova.rs	wash_window_centers_on_blast_row	emit-super-quads	
src/tone.rs	classify_opt_reports_abstention_as_none	tone-scores-abstain-classify	
src/tone.rs	corrupted_assets_are_refused	tone-asset-decoder-fail-closed	
src/tone.rs	encode_decode_round_trips_the_quantized_lattice	tone-encode-decode-round-trip	
src/tone.rs	featurization_is_script_agnostic_and_folded	tone-ngram-featurization	
src/tone.rs	shipped_weights_load_and_verify	tone-shipped-asset-valid	
src/tone_infer.rs	backspace_pops_and_window_stays_bounded	tone-tracker-model	
src/tone_infer.rs	breaks_while_inactive_keep_the_window_coherent	tone-tracker-model	
src/tone_infer.rs	effective_is_neutral_with_the_knob_off_even_with_a_cached_verdict	tone-tracker-model	
src/tone_infer.rs	inference_is_throttled_by_keystrokes	tone-tracker-model	
src/tone_infer.rs	interval_reopens_inference	tone-tracker-model	
src/tone_infer.rs	session_switch_clears_window_but_keeps_mood	tone-tracker-model	
src/tone_infer.rs	the_key_path_marks_inference_due_and_only_the_drain_runs_it	tone-tracker-model	
src/tone_infer.rs	thin_window_after_a_classified_line_keeps_the_mood	tone-tracker-model	
src/trail_sound.rs	a_deletion_does_not_advance_the_song	song-frame-conditions	
src/trail_sound.rs	a_drained_cue_batch_speaks_once_however_deep_the_backlog	v1-admission	
src/trail_sound.rs	a_held_delete_run_shimmers_once_on_release	erase-poof-anatomy	
src/trail_sound.rs	a_held_spacebar_cannot_machine_gun_the_indent_steps	v1-space-downbeat	
src/trail_sound.rs	a_rest_before_a_phrase_starts_cannot_skip_it	theme-playhead	
src/trail_sound.rs	a_rest_cadences_the_phrase_and_opens_the_next	theme-playhead	
src/trail_sound.rs	a_shifted_glyph_lands_an_octave_over_its_plain_self	v1-capital-lift	
src/trail_sound.rs	a_thinned_space_head_re_opens_the_run	v1-admission	
src/trail_sound.rs	a_whitespace_run_lands_one_downbeat	v1-space-downbeat	
src/trail_sound.rs	a_word_boundary_re_bars_the_pulse	advance-song	
src/trail_sound.rs	a_word_motion_is_a_character_motion_at_scale	gesture-family	
src/trail_sound.rs	all_styles_sound_and_decay	silence-is-exact	
src/trail_sound.rs	an_explicit_instrument_auditions_itself_under_the_music_box	engine-routing	
src/trail_sound.rs	bed_off_events_never_energise_the_bed_and_the_setting_reenables	bed-gate	
src/trail_sound.rs	bonk_bypasses_min_gap_thinning_like_jump	v1-admission	
src/trail_sound.rs	bonk_ducks_the_melody_around_it	duck-envelopes	
src/trail_sound.rs	bonk_is_discordant_against_the_walk_and_moves_nothing	bonk-design	
src/trail_sound.rs	celebration_bar_fits_the_polyphony_budget	output-bounded-finite	
src/trail_sound.rs	celebration_bass_always_has_a_carrier	celebration-tables	
src/trail_sound.rs	celebration_bypasses_min_gap_thinning	v1-admission	
src/trail_sound.rs	celebration_ducks_the_melody_and_hands_back	duck-envelopes	
src/trail_sound.rs	celebration_riff_follows_the_active_tone_lattice	tone-lattice-consonance	
src/trail_sound.rs	cross_key_difference	celebration-tables	
src/trail_sound.rs	every_gesture_speaks_in_its_first_post_cue_buffer	first-buffer-onset	
src/trail_sound.rs	every_pitched_voice_lands_on_the_melody_note	gesture-family	
src/trail_sound.rs	every_tone_recolors_and_still_decays_to_silence	silence-is-exact	
src/trail_sound.rs	fill_carries_root	celebration-design	
src/trail_sound.rs	flood_is_ducked	governor-loudness-bound	
src/trail_sound.rs	key_switch_damps_the_old_keys_ring	celebration-design	
src/trail_sound.rs	mode_rotation_alone_changes_the_chorus_color	celebration-design	
src/trail_sound.rs	neutral_signature_is_the_reference_voicing	celebration-tables	
src/trail_sound.rs	new_voice_beds_are_silent_for_keyboards_and_designed_for_instruments	bed-gate	
src/trail_sound.rs	nonfinite_event_fields_are_rejected_before_state_mutation	push-input-guard	
src/trail_sound.rs	output_cues_bypass_but_never_claim_the_human_voice_gap	v1-admission	
src/trail_sound.rs	punctuation_and_the_note_after_a_pause_always_sing	advance-song	
src/trail_sound.rs	rain_glints_are_muted_under_the_sing	duck-envelopes	
src/trail_sound.rs	shift_is_the_ting_and_never_claims_the_beat	v1-shift-ting	
src/trail_sound.rs	sin_table_stays_below_one_16bit_step	sin01-accuracy	
src/trail_sound.rs	sky_bed_pitches_stay_on_the_active_lattice_for_every_tone	tone-lattice-consonance	
src/trail_sound.rs	standalone_voice_is_the_style_palette_verbatim	standalone-voice-alias	
src/trail_sound.rs	the_bar_is_a_regular_beat_of_consonant_offsets	song-pulse-table	
src/trail_sound.rs	the_bass_walk_never_returns_inside_four_words	bass-walk	
src/trail_sound.rs	the_block_pre_roll_delays_every_voice_of_the_push_and_nothing_after	block-pre-roll	
src/trail_sound.rs	the_clouds_puff_survives_the_bell_it_follows	v1-admission	
src/trail_sound.rs	the_deletion_is_a_poof_and_only_a_poof	erase-poof-anatomy	
src/trail_sound.rs	the_glyph_class_table_is_one_for_host_bench_and_engine	glyph-class-table	
src/trail_sound.rs	the_glyph_rank_is_ordered_total_and_lossy	glyph-rank-table	
src/trail_sound.rs	the_outro_s_lead_is_do_and_holds_the_sing_duck	celebration-design	
src/trail_sound.rs	the_register_fold_is_total_and_never_flattens	fold-register	
src/trail_sound.rs	the_song_form_partitions_the_theme	theme-form-tables	
src/trail_sound.rs	the_space_is_a_lawful_walking_bass	v1-space-downbeat	
src/trail_sound.rs	the_theme_is_played_in_order_and_loops	theme-playhead	
src/trail_sound.rs	the_theme_never_folds_in_any_tone	theme-form-tables	
src/trail_sound.rs	the_ting_decays_out_instead_of_being_cut	v1-shift-ting	
src/trail_sound.rs	the_ting_rotates_and_never_drones	v1-shift-ting	
src/trail_sound.rs	tone_tables_are_mutually_consonant_and_exclude_the_bonk_clash	tone-lattice-consonance	
src/trail_sound.rs	two_word_boundaries_never_stack_one_bass_note	v1-space-downbeat	
src/trail_sound.rs	typing_transposes_with_the_song_and_returns_when_it_ends	duck-envelopes	
src/trail_sound.rs	verse_walk_always_moves_and_keeps_rhythm_and_register	celebration-tables	
src/trail_sound.rs	voice_roster_round_trips	voice-roster-table	
src/trail_sound/rainbow_kitty_v2.rs	a_bare_shift_is_a_ting_that_never_steps	mb-ting	
src/trail_sound/rainbow_kitty_v2.rs	a_doubled_letter_is_a_re_strike_wherever_the_line_stands	mb-melodic-bounds	
src/trail_sound/rainbow_kitty_v2.rs	a_held_backspace_unsings_one_note_per_repeat	mb-undo	
src/trail_sound/rainbow_kitty_v2.rs	a_held_key_is_felt_not_silenced_and_the_line_keeps_moving	mb-one-key-one-step	
src/trail_sound/rainbow_kitty_v2.rs	a_hero_star_is_a_pitched_glint_at_its_own_stars_rate	mb-stardust-glint	
src/trail_sound/rainbow_kitty_v2.rs	a_mark_confirms_its_cadence_in_the_space_bass	mb-punctuation-phrasing	
src/trail_sound/rainbow_kitty_v2.rs	a_meteor_fizzles_claims_without_a_click_and_never_eats_a_sounded_bell	mb-meteor	
src/trail_sound/rainbow_kitty_v2.rs	a_mid_word_rest_never_cadences_by_more_than_a_sixth	mb-melodic-bounds	
src/trail_sound/rainbow_kitty_v2.rs	a_password_key_is_one_tone_whatever_it_was	mb-secret	
src/trail_sound/rainbow_kitty_v2.rs	a_password_leaves_the_melody_where_it_was	mb-secret	
src/trail_sound/rainbow_kitty_v2.rs	a_question_s_return_lands_on_the_g	mb-punctuation-phrasing	
src/trail_sound/rainbow_kitty_v2.rs	a_quotation_lifts_and_comes_home_and_an_apostrophe_is_neither	mb-punctuation-phrasing	
src/trail_sound/rainbow_kitty_v2.rs	a_re_struck_mark_does_not_spark_or_graft_again	mb-class-voices	
src/trail_sound/rainbow_kitty_v2.rs	a_re_struck_nav_tick_replaces_its_predecessor_instead_of_combing	mb-same-pitch-no-comb	
src/trail_sound/rainbow_kitty_v2.rs	a_run_of_capitals_keeps_its_contour_instead_of_stacking_on_the_ceiling	mb-melodic-bounds	
src/trail_sound/rainbow_kitty_v2.rs	a_run_of_spaces_is_one_downbeat_and_a_rising_figure	mb-space-bass	
src/trail_sound/rainbow_kitty_v2.rs	a_sentence_neither_ends_nor_starts_where_the_last_one_did	mb-punctuation-phrasing	
src/trail_sound/rainbow_kitty_v2.rs	a_shifted_key_does_not_bend	mb-capital-forte	
src/trail_sound/rainbow_kitty_v2.rs	a_sing_along_never_machine_guns_and_hands_the_key_back_at_a_phrase_boundary	duck-envelopes	
src/trail_sound/rainbow_kitty_v2.rs	a_strike_or_a_ring_on_the_tings_pitch_never_combs	mb-same-pitch-no-comb	
src/trail_sound/rainbow_kitty_v2.rs	a_v1_event_leaves_every_v2_delta_at_its_identity	v2-fields-identity-on-v1	
src/trail_sound/rainbow_kitty_v2.rs	a_v1_voice_after_the_music_box_snaps_a_pending_key_instead_of_holding_it_for_ever	duck-envelopes	
src/trail_sound/rainbow_kitty_v2.rs	a_word_head_common_tone_keeps_the_steps_touch_and_still_damps_the_old_voice	mb-same-pitch-no-comb	
src/trail_sound/rainbow_kitty_v2.rs	a_word_hop_is_never_thinned_by_the_typing_gap_under_a_named_instrument	v1-admission	
src/trail_sound/rainbow_kitty_v2.rs	all_glyph_sparkles_share_the_strikes_loudness_arc	mb-class-voices	
src/trail_sound/rainbow_kitty_v2.rs	an_adjacent_pair_closes_with_its_tink	mb-class-voices	
src/trail_sound/rainbow_kitty_v2.rs	an_aside_is_sotto_voce_and_ends_where_it_should	mb-punctuation-phrasing	
src/trail_sound/rainbow_kitty_v2.rs	backspace_unsings_a_phrase	mb-undo	
src/trail_sound/rainbow_kitty_v2.rs	code_punctuation_never_moves_the_harmony	mb-punctuation-phrasing	
src/trail_sound/rainbow_kitty_v2.rs	consecutive_typed_onsets_are_never_a_leap_inside_a_word	mb-melodic-bounds	
src/trail_sound/rainbow_kitty_v2.rs	every_key_is_a_step_at_every_rate_and_no_key_is_silent	mb-one-key-one-step	
src/trail_sound/rainbow_kitty_v2.rs	every_repeated_pitch_is_charged_to_a_cause_the_design_names	mb-melodic-bounds	
src/trail_sound/rainbow_kitty_v2.rs	flow_echo_phase_requires_the_current_keys_sounding_octave	mb-flow	
src/trail_sound/rainbow_kitty_v2.rs	flow_s_voice_is_identity_at_heat_zero	mb-flow	
src/trail_sound/rainbow_kitty_v2.rs	flowing_knocks_do_not_regrow_an_octave_echo	mb-flow	
src/trail_sound/rainbow_kitty_v2.rs	graft_retirement_model_proves_and_catches_an_omitted_delete	mb-graft-retirement-refines-model	
src/trail_sound/rainbow_kitty_v2.rs	land_is_silent_under_v2	mb-meteor	
src/trail_sound/rainbow_kitty_v2.rs	later_text_keys_disown_the_previous_keys_ring_and_graft	mb-graft-retirement-refines-model	
src/trail_sound/rainbow_kitty_v2.rs	punctuation_phrases_the_line	mb-punctuation-phrasing	
src/trail_sound/rainbow_kitty_v2.rs	shifted_marks_never_steal_their_own_strike_in_a_batched_burst	mb-lane-admission	
src/trail_sound/rainbow_kitty_v2.rs	tails_end_quietly_and_damps_are_ramps	mb-tails-and-damps	
src/trail_sound/rainbow_kitty_v2.rs	the_bell_and_the_landing_share_one_clock	mb-meteor	
src/trail_sound/rainbow_kitty_v2.rs	the_bell_yields_to_the_shifted_key_it_announced_and_to_nothing_else	mb-ting	
src/trail_sound/rainbow_kitty_v2.rs	the_cascade_never_stacks	mb-cascade	
src/trail_sound/rainbow_kitty_v2.rs	the_column_and_the_mood_never_move_the_tune	mb-melody-is-a-function	
src/trail_sound/rainbow_kitty_v2.rs	the_core_carries_direction_and_the_whoosh_does_not	mb-meteor	
src/trail_sound/rainbow_kitty_v2.rs	the_fallback_clock_wraps_like_a_host_stamp_instead_of_saturating	mb-fallback-clock	
src/trail_sound/rainbow_kitty_v2.rs	the_indent_figure_is_a_cycle_on_every_chord_and_key	mb-space-bass	
src/trail_sound/rainbow_kitty_v2.rs	the_lanes_hold_their_caps_and_steals_are_zero	mb-lane-admission	
src/trail_sound/rainbow_kitty_v2.rs	the_limiter_is_transparent_below_threshold_and_holds_the_ceiling_above	mb-bus-limiter	
src/trail_sound/rainbow_kitty_v2.rs	the_line_is_a_function_of_the_text_and_the_rhythm_and_of_nothing_else	mb-melody-is-a-function	
src/trail_sound/rainbow_kitty_v2.rs	the_question_never_rises_a_wolf_in_any_key	mb-punctuation-phrasing	
src/trail_sound/rainbow_kitty_v2.rs	the_remainder_past_the_cap_is_one_strum_not_a_verse	mb-music-box-only-gestures	
src/trail_sound/rainbow_kitty_v2.rs	the_room_answers_line_ends_and_rests_and_the_subject_is_answered_from_above	mb-bloom-and-room	
src/trail_sound/rainbow_kitty_v2.rs	the_run_guard_holds_on_the_sounded_line_not_the_counted_one	mb-melodic-bounds	
src/trail_sound/rainbow_kitty_v2.rs	the_sky_is_fed_by_the_music_boxs_keys_only_behind_the_knob	bed-gate	
src/trail_sound/rainbow_kitty_v2.rs	the_sky_pad_tracks_the_chord_and_key_without_low_frequency_hum	mb-sky-bed	bed-gate
src/trail_sound/rainbow_kitty_v2.rs	the_space_walks_pure_fifths_and_enter_comes_home	mb-space-bass	
src/trail_sound/rainbow_kitty_v2.rs	the_ting_rings_through_the_next_key	mb-ting	
src/trail_sound/rainbow_kitty_v2.rs	the_verdict_is_the_music_boxs_alone_and_never_louder_than_a_key	mb-music-box-only-gestures	
src/trail_sound/rainbow_kitty_v2.rs	typed_grafts_conform_to_owned_retirement_before_and_after_onset	mb-graft-retirement-refines-model	
src/trail_sweep.rs	bounded_tail_matches_forward_bresenham_exhaustively	line-cells-tail-is-bounded-bresenham-suffix	
src/trail_sweep.rs	a_physical_margin_fold_runs_from_the_panes_last_column_to_its_first	sweep-physical-margin-fold	
src/trick_flash.rs	a_declaration_is_spent_by_the_rescan_it_was_made_for	flash-base-y-declaration-and-memo	
src/trick_flash.rs	a_flash_ends_when_its_text_is_erased_or_the_grid_changes_width	flash-follow-rule	
src/trick_flash.rs	a_flash_follows_its_line_as_output_scrolls_it	flash-follow-rule	
src/trick_flash.rs	a_hebrew_word_is_found_in_visual_order_and_in_logical_order	flash-find-whole-word	
src/trick_flash.rs	a_held_address_word_is_found_behind_the_pets_name_by_back_chars	flash-find-whole-word	
src/trick_flash.rs	a_japanese_word_is_found_on_its_lead_cells_inside_one_ime_commit	flash-find-whole-word	
src/trick_flash.rs	a_late_echo_never_sends_the_search_to_last_times_line	flash-locate-row-rule	
src/trick_flash.rs	a_late_echo_still_ramps_in_from_the_base	flash-lifecycle	
src/trick_flash.rs	a_note_that_names_no_pane_reads_the_scan_sessions_row_origin	flash-base-y-declaration-and-memo	
src/trick_flash.rs	a_pane_with_no_caret_observation_stores_no_flash	flash-note-admission	
src/trick_flash.rs	a_piece_of_a_longer_word_is_never_the_typed_word	flash-find-whole-word	
src/trick_flash.rs	a_reported_scroll_is_never_second_guessed_by_searching	flash-locate-row-rule	
src/trick_flash.rs	a_rescan_without_a_cursor_ends_the_flash	flash-lifecycle	
src/trick_flash.rs	a_revoke_fast_fades_a_live_flash_and_drops_an_unfound_one	flash-lifecycle	
src/trick_flash.rs	a_russian_word_is_found_under_ambiguous_double_width	flash-find-whole-word	
src/trick_flash.rs	a_scroll_region_scroll_is_followed_upward_by_exact_anchor_only	flash-locate-row-rule	
src/trick_flash.rs	a_second_flash_inside_the_opening_window_is_skipped_and_a_later_one_replaces	flash-note-admission	
src/trick_flash.rs	a_typed_word_flashes_then_emits_nothing_with_a_stable_fp_and_a_disarmed_scheduler	flash-lifecycle	
src/trick_flash.rs	a_word_not_found_within_the_echo_window_is_given_up_unpainted	flash-lifecycle	
src/trick_flash.rs	a_word_recoloured_mid_flash_fades_to_the_colour_it_has_now	flash-follow-rule	
src/trick_flash.rs	an_exact_anchor_beats_an_inside_one_beats_slack_and_among_equals_the_rightmost_wins	flash-find-whole-word	
src/trick_flash.rs	closing_the_pane_takes_its_flash_with_it	flash-pane-scope	
src/trick_flash.rs	echo_sit_output_never_inks_without_a_note	flash-typed-never-seen	
src/trick_flash.rs	enter_on_the_bottom_row_is_found_on_the_scrolled_row_never_on_the_shells_error_line	flash-locate-row-rule	
src/trick_flash.rs	pane_b_never_shows_pane_as_flash	flash-pane-scope	
src/trick_flash.rs	reduced_motion_and_ink_off_emit_nothing_and_never_arm	flash-lifecycle	
src/trick_flash.rs	thai_and_hindi_words_are_found_under_their_combining_marks	flash-note-admission	
src/trick_flash.rs	the_base_y_memo_is_bounded_and_pruned_with_the_panes	flash-base-y-declaration-and-memo	
src/trick_flash.rs	the_cold_path_and_the_snapshot_path_paint_the_same_bytes	flash-rescan-twins-equivalent	
src/trick_flash.rs	the_envelope_ramps_in_holds_and_fades_to_zero_at_both_ends	flash-envelope-shape	
src/trick_flash.rs	the_flash_inserts_between_other_rows_ink_in_order	flash-emit-is-the-law	
src/trick_flash.rs	the_flash_stays_under_the_photosensitivity_bound	flash-photosensitivity-bound	
src/trick_flash.rs	the_legibility_guard_holds_the_contrast_bound_at_every_hue	flash-legibility-guard	
src/trick_flash.rs	the_merge_keeps_ink_sorted_unique_over_an_overlapping_self_glow	flash-emit-is-the-law	
src/typed_tricks.rs	a_break_revokes_a_live_tentative_fire	tt-poison-and-reset	
src/typed_tricks.rs	a_character_with_no_column_of_its_own_poisons	tt-poison-and-reset	
src/typed_tricks.rs	a_delimiter_run_longer_than_the_window_still_gives_the_token_back	tt-backspace-undo	
src/typed_tricks.rs	a_drawn_out_word_fires_and_flashes_as_it_was_typed	tt-firing-tiers	
src/typed_tricks.rs	a_full_stop_parks_the_fire_until_the_next_space	tt-punctuation-park	
src/typed_tricks.rs	a_fullwidth_stop_is_a_full_boundary_for_a_spaced_token_too	tt-punctuation-park	
src/typed_tricks.rs	a_greeting_or_a_call_never_wins_the_phrase	tt-phrase-law	
src/typed_tricks.rs	a_held_backspace_that_overshoots_the_line_costs_nothing	tt-count-belief	
src/typed_tricks.rs	a_japanese_sentence_committed_whole_fires_addressed_at_the_fullwidth_stop	tt-ime-run-and-fold	
src/typed_tricks.rs	a_lone_committed_ideograph_on_the_way_to_a_longer_word_never_fires	tt-ime-run-and-fold	
src/typed_tricks.rs	a_lone_ideograph_needs_the_pet_named	tt-firing-tiers	
src/typed_tricks.rs	a_long_marked_run_is_backspaced_cluster_for_cluster_past_the_window_too	tt-count-belief	
src/typed_tricks.rs	a_missing_vocabulary_at_a_boundary_fails_closed	tt-needs-lexicon	
src/typed_tricks.rs	a_mixed_script_run_is_no_word	tt-fire-licence-safety	
src/typed_tricks.rs	a_name_before_a_stop_parks_its_confirmation_too	tt-punctuation-park	
src/typed_tricks.rs	a_name_confirms_a_tentative_fire	tt-firing-tiers	
src/typed_tricks.rs	a_name_confirms_the_tentative_fire_and_drops_a_later_held_word	tt-phrase-law	
src/typed_tricks.rs	a_named_pet_makes_the_fire_final	tt-firing-tiers	
src/typed_tricks.rs	a_narrow_stop_parks_a_no_space_run_like_any_other_token	tt-punctuation-park	
src/typed_tricks.rs	a_new_line_is_a_new_phrase	tt-phrase-law	
src/typed_tricks.rs	a_no_echo_prompt_poisons_and_says_nothing	tt-poison-and-reset	
src/typed_tricks.rs	a_plain_word_fires_tentatively_at_the_space_and_not_a_letter_sooner	tt-firing-tiers	
src/typed_tricks.rs	a_poisoned_session_is_remembered_as_dirty	tt-session-dirty-memory	
src/typed_tricks.rs	a_second_boundary_crossed_backwards_leaves_the_line_impure_but_counted	tt-backspace-undo	
src/typed_tricks.rs	a_session_left_clean_comes_back_fresh	tt-session-dirty-memory	
src/typed_tricks.rs	a_session_left_with_a_half_typed_line_comes_back_poisoned	tt-session-dirty-memory	
src/typed_tricks.rs	a_spaced_stop_after_a_word_is_french_spacing_not_code	tt-punctuation-park	
src/typed_tricks.rs	a_token_character_glued_to_the_stop_cancels_the_parked_fire	tt-punctuation-park	
src/typed_tricks.rs	a_token_longer_than_the_window_spoils_its_line_and_nothing_else	tt-token-window	
src/typed_tricks.rs	a_trick_word_after_the_phrase_gap_is_a_new_request	tt-phrase-law	
src/typed_tricks.rs	a_word_exactly_as_long_as_the_window_still_fires	tt-token-window	
src/typed_tricks.rs	a_word_kill_on_an_empty_line_is_a_no_op	tt-word-kill	
src/typed_tricks.rs	a_word_kill_on_an_open_token_recovers	tt-word-kill	
src/typed_tricks.rs	a_word_kill_the_editors_disagree_about_poisons	tt-word-kill	
src/typed_tricks.rs	an_address_word_after_the_name_fires_at_once	tt-firing-tiers	
src/typed_tricks.rs	an_address_word_is_held_until_the_pet_is_named_and_fires_as_itself	tt-firing-tiers	
src/typed_tricks.rs	an_address_word_that_is_never_named_never_fires	tt-firing-tiers	
src/typed_tricks.rs	arbitrary_input_never_panics_and_never_fires_on_a_dead_line	tt-fire-licence-safety	
src/typed_tricks.rs	back_chars_measures_from_the_caret_the_press_arrived_at	tt-back-chars-arith	
src/typed_tricks.rs	backspace_inside_a_delimiter_run_knows_what_the_run_now_ends_with	tt-backspace-undo	
src/typed_tricks.rs	backspace_takes_a_zero_width_mark_with_its_base	tt-count-belief	
src/typed_tricks.rs	backspacing_a_typo_to_the_start_of_the_line_recovers	tt-count-belief	
src/typed_tricks.rs	backspacing_into_a_tentatively_fired_word_revokes_it	tt-backspace-undo	
src/typed_tricks.rs	backspacing_over_one_boundary_restores_the_token_behind_it	tt-backspace-undo	
src/typed_tricks.rs	capitals_fold_and_the_word_is_reported_as_typed	tt-firing-tiers	
src/typed_tricks.rs	code_after_a_plain_word_revokes_it	tt-fire-licence-safety	
src/typed_tricks.rs	ctrl_u_and_ctrl_c_start_a_fresh_pure_line	tt-poison-and-reset	
src/typed_tricks.rs	enter_confirms_a_tentative_fire_on_a_still_pure_line	tt-submit	
src/typed_tricks.rs	enter_delivers_a_parked_fire_as_final	tt-punctuation-park	
src/typed_tricks.rs	enter_on_the_word_itself_is_one_final_fire	tt-submit	
src/typed_tricks.rs	escape_tab_and_arrows_poison_the_line_until_a_reset	tt-poison-and-reset	
src/typed_tricks.rs	every_code_punctuation_mark_makes_the_line_impure	tt-kind-of-table	
src/typed_tricks.rs	every_fire_and_every_empty_line_agrees_with_a_simulated_line_editor	tt-fire-licence-safety	
src/typed_tricks.rs	fillers_keep_the_line_pure_and_fire_nothing	tt-firing-tiers	
src/typed_tricks.rs	good_night_kitty_is_one_sleep_and_never_a_purr	tt-phrase-law	
src/typed_tricks.rs	hangul_committed_one_syllable_at_a_time_fires_at_the_boundary	tt-ime-run-and-fold	
src/typed_tricks.rs	leading_and_doubled_spaces_cost_nothing	tt-firing-tiers	
src/typed_tricks.rs	nothing_on_an_ordinary_line_ends_confirmed_or_unrevoked	tt-fire-licence-safety	
src/typed_tricks.rs	one_commit_reports_one_event	tt-ime-run-and-fold	
src/typed_tricks.rs	prose_after_a_tentative_fire_revokes_it	tt-fire-licence-safety	
src/typed_tricks.rs	raw_bytes_ending_in_a_newline_reset_without_judging	tt-poison-and-reset	
src/typed_tricks.rs	repeated_sentence_punctuation_keeps_the_parked_fire	tt-punctuation-park	
src/typed_tricks.rs	sentence_punctuation_before_any_word_is_code	tt-punctuation-park	
src/typed_tricks.rs	shell_and_code_lines_never_fire	tt-fire-licence-safety	
src/typed_tricks.rs	sit_down_fires_exactly_one_sit	tt-phrase-law	
src/typed_tricks.rs	submit_pet_only_means_the_whole_submitted_line_was_pet_vocabulary	tt-submit	
src/typed_tricks.rs	thai_typed_through_a_layout_fires	tt-firing-tiers	
src/typed_tricks.rs	the_dirty_memory_is_bounded_and_forgets_the_least_recently_left	tt-session-dirty-memory	
src/typed_tricks.rs	the_glue_law_holds_with_nothing_parked	tt-punctuation-park	
src/typed_tricks.rs	the_lexicons_code_context_set_is_code_here_too	tt-kind-of-table	
src/typed_tricks.rs	the_shipped_english_answers_the_owners_examples	tt-shipped-en-vocabulary	
src/typed_tricks.rs	the_vocabulary_is_wanted_only_when_a_token_closes_on_a_pure_line	tt-needs-lexicon	
src/typed_tricks.rs	two_word_phrases_fire_once_each	tt-phrase-law	
src/typed_tricks.rs	under_the_shipped_english_a_fire_is_final_only_on_an_all_vocabulary_prefix	tt-fire-licence-safety	
src/typed_tricks.rs	whole_words_only	tt-fire-licence-safety	
src/typing_momentum.rs	accrual_is_rate_capped_so_key_repeat_cannot_outrun_typing	momentum-advance-credit-law	
src/typing_momentum.rs	credit_cannot_be_banked_across_a_pause	momentum-advance-credit-law	
src/typing_momentum.rs	delete_flood_walks_an_earned_run_to_zero	momentum-delete-kill-drain	
src/typing_momentum.rs	low_crossing_predicts_the_analytic_decay_instant	momentum-crossing-deadlines	
src/typing_momentum.rs	reading_crossing_reaches_the_actual_floor_including_the_zero_snap	momentum-crossing-deadlines	
src/word_decorations.rs	a_collapsed_split_still_witnesses_the_survivors_keystrokes	wd-typed-rearm	
src/word_decorations.rs	a_downward_peek_is_cut_only_at_the_text_line	wd-peek-geometry	
src/word_decorations.rs	a_feline_word_away_from_the_caret_still_peeks_beside_the_companion	wd-companion-claim	
src/word_decorations.rs	a_fold_started_mid_glide_exits_from_the_glide_position	wd-cursor-placement	
src/word_decorations.rs	a_frozen_engine_neither_finds_nor_paints_the_flash	wd-trick-flash	
src/word_decorations.rs	a_held_dog_tile_is_refused_to_another_breed	wd-held-tiles	
src/word_decorations.rs	a_look_flip_refuses_the_old_looks_held_tile_under_bake_pressure	wd-held-tiles	
src/word_decorations.rs	a_metric_change_drops_every_held_tile	wd-held-tiles	
src/word_decorations.rs	a_panes_pending_ignition_survives_another_panes_tick	wd-ignition-prune	
src/word_decorations.rs	a_peek_landing_records_one_positioned_cue	wd-peek-cue	
src/word_decorations.rs	a_pet_in_open_grid_is_untouched_and_still_matches_body_px	wd-pet-sprite-in-grid	
src/word_decorations.rs	a_pet_on_row_zero_emits_no_sprite_above_the_grid	wd-pet-sprite-in-grid	
src/word_decorations.rs	a_repaint_midway_through_the_cat_plays_no_second_cat	wd-done-marks	
src/word_decorations.rs	a_seat_lift_glides_instead_of_jumping_a_row	wd-kitty-cursor-emission	
src/word_decorations.rs	a_second_feline_word_typed_while_the_companion_stays_up_is_claimed_too	wd-companion-claim	
src/word_decorations.rs	a_settled_keyword_kitty_breathes_without_resizing	wd-peek-geometry	
src/word_decorations.rs	a_settled_kitty_on_screen_never_plays_again_without_a_keystroke	wd-done-marks	
src/word_decorations.rs	a_split_keystroke_is_attributed_to_the_pane_it_was_typed_in	wd-typed-rearm	
src/word_decorations.rs	a_word_under_the_pet_body_never_peeks	wd-companion-claim	
src/word_decorations.rs	a_wordless_screen_with_no_flash_still_returns_fp_zero_and_disarms	wd-trick-flash	
src/word_decorations.rs	alignment_skip_semantics_old_episode_goes_unmatched	wd-alignment-cardinality	
src/word_decorations.rs	allocation_regression_all_miss_repaint_reuses_retired_buffers	wd-bounded-storage	
src/word_decorations.rs	allocation_regression_resident_capacities_stable	wd-bounded-storage	
src/word_decorations.rs	ambiguous_future_prefix_waits_for_delimiter_in_ro_and_all	wd-scan-classification	
src/word_decorations.rs	an_ambient_cat_on_the_flying_head_yields_to_it	wd-companion-footprint	
src/word_decorations.rs	an_edit_in_one_tab_does_not_license_another_tabs_replay_when_unsplit	wd-typed-rearm	
src/word_decorations.rs	an_unbound_scans_pending_ignition_is_not_session_zeros_to_cancel	wd-ignition-prune	
src/word_decorations.rs	an_unrelated_edit_does_not_license_a_repaint_replay	wd-typed-rearm	
src/word_decorations.rs	animal_words_peek_their_species_heads	wd-scan-classification	
src/word_decorations.rs	beside_other_ink_the_flash_extends_the_deadline_and_never_shortens_it	wd-trick-flash	
src/word_decorations.rs	both_companions_on_the_top_rows_land_whole_in_a_top_pane_and_a_lower_pane	wd-pet-sprite-in-grid	wd-kitty-cursor-emission
src/word_decorations.rs	bottom_rows_keep_occurrence_slots_on_a_match_saturated_screen	wd-occurrence-cap	
src/word_decorations.rs	bound_panes_keep_independent_damage_epochs	wd-pane-scoping	
src/word_decorations.rs	bow_cat_sighting_carries_trait_bow	wd-kitty-sighting	
src/word_decorations.rs	burst_chance_pct_gates_starburst_rolls	wd-burst-roll	
src/word_decorations.rs	busy_band_above_flips_the_peek_downward	wd-peek-plan	
src/word_decorations.rs	capture_during_freeze_preserves_episodes_byte_for_byte	wd-freeze	
src/word_decorations.rs	compat_settled_emission_is_the_raw_footprint_and_ignores_the_glide	wd-cursor-placement	
src/word_decorations.rs	coupling_pulse_is_deterministic_and_luminance_bounded	wd-nova-coupling	
src/word_decorations.rs	curse_cues_clear_at_tick_start_and_on_state_drop	wd-curse-cue	
src/word_decorations.rs	cursor_cat_body_clamps_after_right_shift_stretch_and_lead	wd-kitty-cursor-emission	
src/word_decorations.rs	cursor_cat_fold_conformance_real_placement_and_teleport_mutant	wd-cursor-placement	
src/word_decorations.rs	custom_kitty_footprint_matches_emission_at_interior_right_and_top_edges	wd-kitty-cursor-emission	
src/word_decorations.rs	custom_kitty_hot_reload_rebases_repaints_and_invalid_replacement_is_dark	wd-cursor-placement	
src/word_decorations.rs	custom_on_builtin_profanity_fires_with_class_disabled	wd-scan-classification	
src/word_decorations.rs	dec_rows_anchor_nova_via_row_advance	wd-burst-emission	
src/word_decorations.rs	declaring_an_unsplit_session_retires_a_stale_pane_binding	wd-typed-rearm	
src/word_decorations.rs	dog_cameo_emits_one_trailing_sprite	wd-dog-cameo	
src/word_decorations.rs	done_mark_lru_full_capacity_replacement_is_bounded_and_growth_free	wd-done-mark-lru	
src/word_decorations.rs	done_mark_lru_real_order_and_constant_work_conform	wd-done-mark-lru	
src/word_decorations.rs	done_marks_spread_keys_precomputed_to_collide	wd-keyed-hashers	
src/word_decorations.rs	emphasis_and_ink_gates	wd-ink-layout	
src/word_decorations.rs	excess_concurrent_novas_skip_to_ember	wd-nova-concurrency-cap	
src/word_decorations.rs	fbomb_combo_cools_across_freeze_thaw	wd-burst-roll	
src/word_decorations.rs	fbomb_combo_five_typed_fucks_go_nuclear_then_decay	wd-burst-roll	
src/word_decorations.rs	fbomb_combo_nuke_cloud_reaches_the_free_lane	wd-burst-emission	
src/word_decorations.rs	fbomb_combo_output_fucks_do_not_charge	wd-burst-roll	
src/word_decorations.rs	feline_magic_off_disables_rare_cats	wd-magic-gates	
src/word_decorations.rs	fix_churn_cannot_rearm_spent_profanity_on_restore	wd-alignment-cardinality	
src/word_decorations.rs	fix_never_triggers_or_inherits_profanity_rainbow	wd-empty-emits-nothing	
src/word_decorations.rs	flash_limiter_conformance_real_limiter_projects_onto_model	wd-ignition-limiter	
src/word_decorations.rs	flash_limiter_negative_control_overlap_blind_limiter_is_buggy_trace	wd-ignition-limiter	
src/word_decorations.rs	flash_limiter_stays_window_wide_across_panes	wd-ignition-limiter	
src/word_decorations.rs	fold_free_covers_every_field_including_z_art_and_clip	fp-field-coverage	
src/word_decorations.rs	freeze_thaw_mid_rise_resumes_at_same_phase	wd-freeze	
src/word_decorations.rs	fresh_episode_latches_phase_clock_at_birth	wd-peek-lifecycle	
src/word_decorations.rs	fuc_is_ordinary_and_only_complete_fuck_activates	wd-scan-classification	
src/word_decorations.rs	full_play_then_reduce_resurrects_nothing	wd-peek-lifecycle	
src/word_decorations.rs	genome_frozen_across_neighbor_churn	wd-identity-model	
src/word_decorations.rs	geometry_scan_samples_and_freezes_the_prospective_cat_footprint	wd-cat-context	
src/word_decorations.rs	global_insertion_transfers_dense_triple_without_births	wd-alignment-cardinality	
src/word_decorations.rs	graphic_only_custom_word_with_ink_off_shows_a_cat	wd-scan-classification	
src/word_decorations.rs	identical_words_in_two_panes_each_get_their_own_cat	wd-pane-scoping	
src/word_decorations.rs	identical_words_in_two_tabs_each_get_their_own_cat	wd-pane-scoping	
src/word_decorations.rs	ignition_reservation_lifecycle_real_queue_conforms	wd-ignition-prune	
src/word_decorations.rs	ignition_reservation_rekey_real_queue_conforms	wd-ignition-rekey	
src/word_decorations.rs	ignore_lists_suppress_the_bonk_with_the_decoration	wd-curse-cue	
src/word_decorations.rs	incrementally_typed_kitty_always_starts_a_visible_top_row_cat	wd-scan-classification	
src/word_decorations.rs	ink_disabled_light_bg_still_takes_the_eclipse_branch	wd-burst-emission	
src/word_decorations.rs	ink_legibility_guard_pulls_on_light_bg	wd-ink-color	
src/word_decorations.rs	ink_settles_to_constant_bytes_and_disarms	wd-idle-fixed-point	
src/word_decorations.rs	ink_truncation_is_whole_word_and_stable	wd-ink-layout	
src/word_decorations.rs	invalid_kitty_source_fails_closed_instead_of_using_builtin_art	wd-kitty-cursor-emission	
src/word_decorations.rs	kitty_emits_only_its_body_in_front_of_the_cursor	wd-kitty-cursor-emission	
src/word_decorations.rs	kitty_sighting_once_per_episode_across_occlusion	wd-kitty-sighting	
src/word_decorations.rs	limiter_delays_third_of_three_ignitions_deterministically	wd-ignition-limiter	
src/word_decorations.rs	limiter_reused_owner_label_does_not_alias_fired_episode	wd-ignition-limiter	
src/word_decorations.rs	limiter_tightens_to_one_per_second_on_overlap	wd-ignition-limiter	
src/word_decorations.rs	live_classic_nova_defers_the_supernova_grant	wd-burst-mutex	
src/word_decorations.rs	margin_staircase_glides_instead_of_stepping_per_key	wd-cursor-placement	
src/word_decorations.rs	multilingual_typed_curses_bonk_zh_ja_ko_en	wd-curse-cue	
src/word_decorations.rs	nova_dip_freezes_sweep_and_embers_ink	wd-nova-ink	
src/word_decorations.rs	nova_one_flash_per_episode_across_occlusion	wd-nova-one-flash	
src/word_decorations.rs	nuke_window_holds_the_mutex_for_its_full_cloud	wd-burst-mutex	
src/word_decorations.rs	occlusion_mid_dwell_pauses_the_peek_and_resumes_when_rows_clear	wd-peek-occlusion-pause	
src/word_decorations.rs	one_host_frame_admits_exactly_one_companion_body	wd-one-body-per-frame	
src/word_decorations.rs	one_host_frame_bakes_at_most_two_tiles_across_panes	wd-bake-budget	
src/word_decorations.rs	one_keystroke_re_arms_one_cat_however_many_repaints_follow	wd-typed-rearm	
src/word_decorations.rs	one_shot_peek_conformance_real_engine	wd-peek-lifecycle	
src/word_decorations.rs	one_typed_animal_word_grows_exactly_one_cat_centred_on_its_span	wd-peek-geometry	
src/word_decorations.rs	pane_ticks_do_not_inflate_rescan_seq	wd-pane-scoping	
src/word_decorations.rs	passive_output_at_terminal_cursor_stays_spent	wd-typed-rearm	
src/word_decorations.rs	persist_cap_drops_unmatched_grace_after_fresh_move	wd-persist-capacity	
src/word_decorations.rs	profanity_rainbow_opens_with_bounded_sparkles	wd-sparkle-lifecycle	
src/word_decorations.rs	profanity_sparkles_then_self_terminates	wd-sparkle-lifecycle	
src/word_decorations.rs	rainbow_freeze_is_byte_stable_on_both_themes_and_done_born	wd-settle-identity	
src/word_decorations.rs	rainbow_guard_samples_four_points_not_just_mid	wd-ink-color	
src/word_decorations.rs	real_burst_grants_conform_to_the_supernova_mutex_model	wd-burst-mutex	
src/word_decorations.rs	reduced_motion_forces_static	wd-reduced-motion-static	
src/word_decorations.rs	reduced_motion_nova_is_a_static_glint	wd-reduced-motion-static	
src/word_decorations.rs	repeated_complete_fuck_rearms_while_fuc_stays_ordinary	wd-typed-rearm	
src/word_decorations.rs	rescan_from_cells_matches_term_rescan	wd-scan-path-equivalence	
src/word_decorations.rs	reset_clears_a_live_trick_flash_on_the_next_tick	wd-trick-flash	
src/word_decorations.rs	reset_clears_stale_ink_on_next_tick	wd-reset	
src/word_decorations.rs	reset_flush_writes_marks_and_reentry_is_born_done	wd-reset	
src/word_decorations.rs	resize_settle_births_are_inert_and_unmarked	wd-resize-settle	
src/word_decorations.rs	retain_panes_evicts_departed_panes	wd-pane-scoping	
src/word_decorations.rs	reverse_fold_mirrors_reenters_and_rebaselines_on_coordinate_breaks	wd-cursor-placement	
src/word_decorations.rs	right_edge_clamp_rises_the_companion_instead_of_covering_the_text	wd-kitty-cursor-emission	
src/word_decorations.rs	rotation_alignment_plays_new_twin_and_pins_survivor	wd-alignment-cardinality	
src/word_decorations.rs	scan_memo_frames_are_identical_to_an_always_rescanning_engine	wd-scan-memo-transparent	
src/word_decorations.rs	scan_memo_spreads_rows_precomputed_to_collide	wd-keyed-hashers	
src/word_decorations.rs	scrolled_off_supernova_window_still_defers_classic_grants	wd-burst-mutex	
src/word_decorations.rs	selection_defers_ignition_and_attenuates_active_novas	wd-selection-safety	
src/word_decorations.rs	selection_mid_eclipse_punches_through_the_veil	wd-selection-safety	
src/word_decorations.rs	shipping_cat_sprites_preserve_authored_aspect_and_uniform_age_scale	wd-peek-geometry	
src/word_decorations.rs	singing_notes_stream_ring_capped_tinted_and_drive_scaled	wd-kitty-cursor-emission	
src/word_decorations.rs	sparkle_identity_conformance_real_persist_map	wd-identity-model	
src/word_decorations.rs	sparkle_identity_negative_control_graceless_map_is_buggy_trace	wd-identity-model	
src/word_decorations.rs	sparkle_reflow_cardinality_conformance_repeated_surfaces	wd-alignment-cardinality	
src/word_decorations.rs	sparkle_reflow_cardinality_negative_control_context_gate	wd-alignment-cardinality	
src/word_decorations.rs	sparkle_word_reset_preserves_cursor_companion_placement_and_atlas	wd-reset	
src/word_decorations.rs	supernova_detonation_cues_at_the_grant_edge_once	wd-curse-cue	
src/word_decorations.rs	supernova_roll_decorrelates_and_transfers_across_alignment	wd-burst-roll	
src/word_decorations.rs	supernova_suppressed_under_reduced_motion	wd-reduced-motion-static	
src/word_decorations.rs	supernova_wash_splits_around_selection	wd-selection-safety	
src/word_decorations.rs	the_flying_head_on_the_top_rows_stays_inside_the_grid	wd-kitty-cursor-emission	
src/word_decorations.rs	the_summoning_word_is_claimed_even_after_the_caret_moved_on	wd-companion-claim	
src/word_decorations.rs	transfer_then_expire_writes_mark_under_departure_ident	wd-done-marks	
src/word_decorations.rs	tui_prompt_box_frame_still_summons_a_cat	wd-peek-plan	
src/word_decorations.rs	two_consecutive_recent_kitty_retypes_both_arm	wd-typed-rearm	
src/word_decorations.rs	typed_curse_birth_records_one_bonk_cue	wd-curse-cue	
src/word_decorations.rs	typed_kitty_after_dense_cat_output_is_not_slot_starved	wd-slot-pools	
src/word_decorations.rs	typing_dmg_never_bonks_but_the_marked_vietnamese_form_does	wd-curse-cue	
src/word_decorations.rs	unequal_pane_widths_do_not_pin_born_settled	wd-resize-settle	
src/word_decorations.rs	vanished_classic_nova_flood_has_constant_reservation_storage	wd-bounded-storage	
src/word_decorations.rs	words_arriving_while_away_never_storm_on_return	wd-peek-lifecycle	
tests/abandoned_ribbon.rs	a_backspace_back_up_through_a_shell_fold_holds_the_row_under_the_parked_hand	rb-hand-return	
tests/abandoned_ribbon.rs	a_band_overwritten_with_different_text_is_retired	rb-follow-witness	
tests/abandoned_ribbon.rs	a_composer_repaint_does_not_cut_a_live_ribbon_at_each_space	rb-follow-witness	
tests/abandoned_ribbon.rs	a_ctrl_u_under_an_identical_line_drains_on_its_own_row	rb-follow-witness	
tests/abandoned_ribbon.rs	a_fold_s_seam_holds_when_two_keys_echoes_land_on_one_frame	rb-walk-continuity	
tests/abandoned_ribbon.rs	a_key_typed_over_an_abandoned_cohorts_cell_keeps_its_own_light	rb-follow-witness	
tests/abandoned_ribbon.rs	a_killed_line_under_an_identical_line_is_released_not_followed_onto_it	rb-follow-witness	
tests/abandoned_ribbon.rs	a_prompt_redraw_that_puts_the_same_text_back_keeps_every_cell	rb-follow-witness	
tests/abandoned_ribbon.rs	a_re_anchor_onto_the_pane_s_first_column_lights_nothing_in_the_pane_beside_it	rb-echo-custody	
tests/abandoned_ribbon.rs	a_relocated_input_box_carries_the_band_with_its_text_and_stamps_nothing	rb-follow-witness	
tests/abandoned_ribbon.rs	a_shell_fold_continues_the_walk_at_the_pace_the_row_above_had_reached	rb-walk-continuity	
tests/abandoned_ribbon.rs	a_shell_fold_s_first_cell_is_the_row_above_s_last_glyph_continued_one_step	rb-walk-continuity	
tests/abandoned_ribbon.rs	a_wide_glyph_s_two_cells_are_retired_as_one_unit	rb-follow-witness	
tests/abandoned_ribbon.rs	a_wrapped_line_s_first_row_goes_out_on_its_own_clock_while_the_hand_types_on_the_second	rb-row-scoped-flow	
tests/abandoned_ribbon.rs	the_box_growth_wrap_carries_the_band_up_and_a_silent_flush_lays_its_landing	rb-follow-witness	
tests/abandoned_ribbon.rs	the_box_growth_wrap_carries_the_band_up_with_its_text_and_the_flush_pays_its_landing	rb-follow-witness	
tests/abandoned_ribbon.rs	the_next_key_after_a_declined_relocation_lays_at_the_caret_s_true_cell	rb-echo-custody	
tests/abandoned_ribbon.rs	trail_status_appends_ribbon_retired_after_the_v2_rows	rb-trail-status-line	
tests/animal_art_quality.rs	lexicon_species_and_authored_roster_are_the_same_set	art-roster-completeness	
tests/animal_art_quality.rs	species_heads_do_not_alias_each_other_at_sprite_size	art-asset-quality	
tests/busy_status_rows_keep_their_brand.rs	a_footer_below_the_composer_leaves_no_hole_after_a_scroll	rb-echo-custody	
tests/busy_status_rows_keep_their_brand.rs	a_line_editing_key_before_the_hesitated_key_leaves_no_hole	rb-echo-custody	
tests/busy_status_rows_keep_their_brand.rs	a_scroll_that_wipes_the_brands_leaves_no_hole	rb-echo-custody	
tests/busy_status_rows_keep_their_brand.rs	a_status_row_digit_inside_a_hesitated_keys_echo_window_leaves_no_hole	rb-echo-custody	
tests/busy_status_rows_keep_their_brand.rs	a_status_tick_ending_on_the_hesitated_key_s_own_glyph_takes_nothing	rb-echo-custody	
tests/busy_status_rows_keep_their_brand.rs	a_transcript_streaming_onto_new_rows_leaves_no_hole	rb-echo-custody	
tests/cat_art_quality.rs	authored_accessories_have_no_backdrop_or_fragmented_outline	art-asset-quality	
tests/cat_art_quality.rs	authored_characters_stay_clean_bounded_and_readable	art-asset-quality	
tests/cat_art_quality.rs	cursor_reactions_remain_visibly_distinct_at_terminal_size	art-asset-quality	
tests/cat_art_quality.rs	repaired_roster_heads_are_semantic_bounded_and_legible	art-asset-quality	
tests/claude_wrap_band.rs	a_partial_word_carried_down_by_the_next_key_keeps_its_first_letter_and_no_indent_stub	rb-soft-wrap-indent	
tests/claude_wrap_band.rs	a_six_letter_word_carried_down_keeps_every_letter_and_no_indent_stub	rb-soft-wrap-indent	
tests/claude_wrap_band.rs	an_insert_whose_first_key_carries_the_caret_down_is_lit	rb-soft-wrap-indent	
tests/claude_wrap_band.rs	controls_an_insert_on_the_wrapped_row_and_an_end_of_text_wrap	rb-soft-wrap-indent	
tests/claude_wrap_band.rs	the_reflowed_word_after_fast_word_hops_keeps_its_first_letter	rb-soft-wrap-indent	
tests/claude_wrap_band.rs	the_reflowed_word_after_word_hops_keeps_its_first_letter	rb-soft-wrap-indent	
tests/codex_background_redraw.rs	a_coalesced_pair_across_the_pending_margin_wrap_lights_both_glyphs	rb-echo-custody	
tests/codex_background_redraw.rs	a_delayed_original_echo_survives_multiple_foreign_row_repaints	rb-echo-custody	
tests/codex_background_redraw.rs	a_footer_park_that_never_returns_expires_without_painting_the_footer	rb-echo-custody	
tests/codex_background_redraw.rs	a_park_and_its_real_echo_move_together_when_output_scrolls_the_grid	rb-echo-custody	
tests/codex_background_redraw.rs	a_partial_restore_cannot_recover_the_clock_but_the_complete_original_can	rb-typed-continuity	
tests/codex_background_redraw.rs	a_real_return_and_natural_wrap_keep_their_immediate_row_move	rb-echo-custody	
tests/codex_background_redraw.rs	a_submitted_composer_owns_the_next_echo_after_a_generic_boundary	parked-caret-row-ownership	
tests/codex_background_redraw.rs	an_unchanged_composer_redrawn_in_one_batch_or_across_frames_keeps_its_typed_band	rb-follow-witness	
tests/codex_background_redraw.rs	background_particles_beside_typing_preserve_the_composer_and_leave_the_footer_dark	rb-echo-custody	
tests/codex_background_redraw.rs	replaced_text_and_an_expired_original_do_not_restore	rb-typed-continuity	
tests/codex_particle_replay.rs	a_backward_edit_in_codex_never_punches_a_hole_in_the_band	rb-band-connectivity	
tests/codex_particle_replay.rs	a_key_typed_over_the_band_gets_its_own_cell	rb-band-connectivity	
tests/codex_particle_replay.rs	coalesced_wrapped_prefix_keeps_first_key_and_refuses_particle	rb-same-caret-echo	
tests/codex_particle_replay.rs	codex_same_row_continuation_restores_the_exact_retired_prefix	rb-typed-continuity	
tests/codex_particle_replay.rs	delayed_single_same_caret_echo_reowns_only_the_exact_typed_glyph	rb-same-caret-echo	
tests/codex_particle_replay.rs	dormant_typed_run_adds_no_idle_work_after_natural_exit	rb-typed-continuity	
tests/codex_particle_replay.rs	first_a_recovery_refuses_particle_and_pre_key_program_paint	rb-same-caret-echo	
tests/codex_particle_replay.rs	first_codex_key_on_wrapped_row_reowns_its_cell	rb-same-caret-echo	
tests/codex_particle_replay.rs	long_wrapped_batch_refuses_keyless_mismatch_and_interleaved_prints	rb-same-caret-echo	
tests/codex_particle_replay.rs	nine_to_128_key_wrapped_batches_reown_the_first_glyph_on_their_first_frame	rb-same-caret-echo	
tests/codex_particle_replay.rs	ordinary_first_a_reanchor_is_cached_for_the_first_resumed_frame	rb-same-caret-echo	
tests/codex_particle_replay.rs	plain_typing_under_codex_keeps_the_whole_line_lit	rb-band-connectivity	
tests/codex_particle_replay.rs	retired_prefix_rejoin_refuses_changed_keyless_navigated_and_stale_rows	rb-typed-continuity	
tests/codex_particle_replay.rs	stalled_codex_batch_reowns_first_a_from_exact_inflight_run	rb-same-caret-echo	
tests/composer_box_growth_wrap.rs	a_backspace_back_through_the_fold_rewets_the_row_the_hand_returned_to	rb-hand-return	
tests/composer_box_growth_wrap.rs	a_shift_enter_newline_starts_the_walk_again_however_soon_the_first_key_follows	rb-walk-continuity	
tests/composer_box_growth_wrap.rs	a_shift_enter_newline_starts_the_walk_again_in_a_bottom_anchored_box_too	rb-walk-continuity	
tests/composer_box_growth_wrap.rs	a_true_re_layout_with_different_text_a_row_up_still_melts_the_old_band	rb-follow-witness	
tests/composer_box_growth_wrap.rs	after_a_wrap_whose_old_row_melts_the_next_row_continues_the_walk_at_its_pace	rb-walk-continuity	
tests/composer_box_growth_wrap.rs	an_unwrap_takes_the_seam_off_the_line_the_hand_came_back_to	rb-hand-return	
tests/composer_box_growth_wrap.rs	one_frame_after_the_wrap_the_band_stands_under_its_text_a_row_up_and_is_not_melted	rb-follow-witness	
tests/composer_box_growth_wrap.rs	the_followed_row_flows_into_the_fold_and_is_dark_by_the_flows_end	rb-row-scoped-flow	
tests/composer_box_growth_wrap.rs	the_new_row_continues_the_old_rows_walk_at_the_pace_it_had_reached	rb-walk-continuity	
tests/composer_growth_follows.rs	a_line_whose_first_glyph_twins_above_and_below_follows_whole	rb-follow-witness	
tests/composer_growth_follows.rs	a_proved_short_wrap_does_not_reanchor_a_later_false_park	rb-short-wrap-park	
tests/composer_growth_follows.rs	a_short_wrap_relay_survives_a_delayed_echo	rb-walk-continuity	
tests/composer_growth_follows.rs	a_space_typed_before_the_wrap_s_echo_lands_leaves_the_moved_word_relaid	rb-walk-continuity	
tests/composer_growth_follows.rs	a_wrap_that_moves_no_word_continues_one_past_the_eaten_space_whatever_the_echo_lag	rb-walk-continuity	
tests/composer_growth_follows.rs	every_growth_at_the_glass_take_s_geometry_follows_its_text_and_flows	rb-follow-witness	
tests/composer_growth_follows.rs	every_growth_of_the_owner_s_composer_follows_and_flows_when_the_hand_stops_at_the_wrap	rb-follow-witness	
tests/composer_growth_follows.rs	every_growth_of_the_owner_s_composer_follows_its_text_and_flows	rb-follow-witness	
tests/composer_growth_follows.rs	one_coincident_letter_does_not_change_the_exit	rb-follow-witness	
tests/composer_growth_follows.rs	the_control_a_letter_typed_before_the_wrap_s_echo_lands	rb-walk-continuity	
tests/composer_multiline_wrap.rs	a_line_typed_under_a_near_copy_of_itself_follows_on_the_letters_that_differ	rb-follow-witness	
tests/composer_multiline_wrap.rs	a_second_line_that_shares_no_letter_with_the_first_follows_on_both_trees	rb-follow-witness	
tests/composer_multiline_wrap.rs	a_second_wrap_that_writes_other_text_a_row_up_follows_nothing	rb-follow-witness	
tests/composer_multiline_wrap.rs	a_space_typed_at_the_end_of_a_full_line_goes_up_with_it_when_the_next_glyph_wraps	rb-follow-witness	
tests/composer_multiline_wrap.rs	the_first_wrap_under_a_labelled_rule_follows_despite_a_letter_the_label_shares	rb-follow-witness	
tests/composer_multiline_wrap.rs	the_limit_a_line_identical_to_the_one_above_it_or_one_column_off_melts_instead_of_following	rb-follow-witness	
tests/composer_multiline_wrap.rs	the_row_after_the_second_wrap_continues_the_walk_the_second_line_left_off	rb-walk-continuity	
tests/composer_multiline_wrap.rs	the_second_wrapped_line_follows_its_text_a_row_up_despite_the_letters_it_shares	rb-follow-witness	
tests/console_contact_observation.rs	a_viewport_round_trip_discards_unconsumed_contact	pet-contact	
tests/console_contact_observation.rs	an_incoherent_observation_discards_unconsumed_contact	pet-contact	
tests/console_contact_observation.rs	duplicate_frames_do_not_extend_the_contact_hold	pet-contact	
tests/console_contact_observation.rs	newly_drawn_local_ink_is_contact_without_an_attributed_edit_witness	pet-contact	
tests/console_contact_observation.rs	observed_ink_is_not_saved_for_a_later_input_episode	pet-contact	
tests/console_contact_observation.rs	repeated_observations_preserve_one_pending_local_contact	pet-contact	
tests/console_contact_observation.rs	repeated_unchanged_observations_cannot_create_contact_from_input_alone	pet-contact	
tests/console_contact_observation.rs	uniform_output_scroll_moves_old_ink_without_creating_contact	pet-contact	
tests/console_cursor_home.rs	coding_console_cursor_hide_show_keeps_the_full_resident_without_fake_input	pet-cursor-home	
tests/console_cursor_home.rs	dense_colored_coding_prompt_keeps_a_full_pet_near_the_cursor	pet-cursor-home	
tests/console_cursor_home.rs	distant_program_output_changes_attention_without_moving_home_across_the_console	pet-cursor-home	
tests/console_cursor_home.rs	frame_fingerprint_observes_the_body_moving_behind_text	fp-field-coverage	
tests/console_cursor_home.rs	hidden_cursor_region_scrolls_keep_the_full_body_at_its_last_real_home	pet-cursor-home	
tests/console_cursor_home.rs	ordinary_dense_output_does_not_make_an_idle_resident_blink_out	pet-cursor-home	
tests/console_cursor_home.rs	ordinary_typing_keeps_the_full_body_visible_between_keys	pet-cursor-home	
tests/console_cursor_home.rs	real_cursor_home_placement_conforms_and_rejects_the_historical_hidden_body	pet-cursor-home	
tests/console_cursor_home.rs	wall_side_changes_and_line_wrap_keep_the_body_visible	pet-cursor-home	
tests/console_handoff_conformance.rs	real_flight_landing_and_resident_consumption_conform_for_clear_and_dense_reading	pet-resident-handoff	
tests/console_handoff_conformance.rs	real_no_target_retirement_consumes_touch_and_cannot_poll_or_replay	pet-resident-handoff	
tests/console_idle_output.rs	an_actual_glyph_in_the_sleeping_footprint_still_causes_eviction	console-observation-admission	
tests/console_idle_output.rs	nearby_output_particles_allow_uninterrupted_sleep_after_the_idle_ladder	console-observation-admission	
tests/console_idle_output.rs	real_navigation_releases_the_sleeping_seat_and_follows_the_new_caret	console-observation-admission	
tests/console_idle_output.rs	single_dot_particles_preserve_protected_surfaces_and_uncertain_boundaries	console-observation-admission	
tests/console_idle_output.rs	single_dot_particles_remain_ink_without_displacing_the_pet	console-observation-admission	
tests/console_life_conformance.rs	completed_and_already_stale_results_expire_without_a_backlog	console-life-episode-refines-model	
tests/console_life_conformance.rs	direct_input_revokes_a_real_command_perch_and_its_result	console-life-episode-refines-model	
tests/console_life_conformance.rs	input_echo_selection_and_surface_ownership_conform	console-life-episode-refines-model	
tests/console_life_conformance.rs	multiple_commits_coalesce_to_the_latest_subject_without_echo_replay	console-life-episode-refines-model	
tests/console_life_erase_conformance.rs	ed2_clears_selected_occupancy_without_retiring_live_coordinates_or_input	console-life-episode-refines-model	
tests/console_life_erase_conformance.rs	ed3_discards_history_without_relocating_live_cells_the_running_block_or_pet	console-life-episode-refines-model	
tests/console_life_erase_conformance.rs	erases_and_selection_release_cannot_replay_old_ink_as_contact	console-life-episode-refines-model	
tests/console_life_erase_conformance.rs	ris_retires_the_previous_surface_anchor_and_pending_input	console-life-episode-refines-model	
tests/console_life_geometry.rs	fully_occupied_output_keeps_the_resident_under_text_without_polling	pet-wake-liveness	
tests/console_life_geometry.rs	hidden_cursor_home_stays_put_while_real_output_scrolls	pet-wake-liveness	
tests/console_life_geometry.rs	output_over_the_old_perch_keeps_the_full_body_and_settles_without_polling	pet-wake-liveness	
tests/console_life_geometry.rs	reduced_motion_reading_is_opaque_on_its_first_caretless_tick	pet-wake-liveness	
tests/console_life_geometry.rs	retained_perch_translates_once_with_real_full_screen_output_scroll	pet-wake-liveness	
tests/console_life_wakes.rs	a_completion_past_its_admission_ttl_never_starts_a_response	pet-wake-liveness	
tests/console_life_wakes.rs	a_fresh_but_delayed_completion_gets_its_full_visible_hold	pet-wake-liveness	
tests/console_life_wakes.rs	a_landing_handoff_with_no_clear_reading_space_releases_its_frame_lane	pet-wake-liveness	
tests/console_life_wakes.rs	a_selection_arriving_midflight_preserves_the_arc_and_landing_cadence	pet-wake-liveness	
tests/console_life_wakes.rs	petting_suppressed_by_full_viewport_selection_is_consumed_without_polling	pet-wake-liveness	
tests/console_life_wakes.rs	settled_console_resident_offers_a_consumption_tick_for_pending_notes	pet-wake-liveness	
tests/console_pet_escort_lead.rs	the_escort_keeps_off_the_words_it_leads	escort-locomotion-speed-limit	
tests/console_pet_escort_lead.rs	a_held_backspace_frightens_the_cat_once_and_it_follows	retreat-reactions	
tests/console_pet_visibility_hold.rs	an_output_burst_yields_by_fading_never_by_cutting	pet-visibility-hold	
tests/console_pet_visibility_hold.rs	the_caret_alone_never_blanks_its_escort	pet-visibility-hold	
tests/console_pet_visibility_hold.rs	typing_never_blanks_the_pet	pet-visibility-hold	
tests/console_pet_z_order_hold.rs	claude_code_s_recorded_composer_conforms_and_never_paints_over_ink	console-z-order-refines-model	
tests/console_pet_z_order_hold.rs	ink_toggling_under_the_body_faster_than_the_dwell_flips_once_each_way	console-z-order-refines-model	
tests/console_pet_z_order_hold.rs	the_real_z_order_conforms_to_the_derived_model_and_rejects_the_old_rule	console-z-order-refines-model	
tests/console_pet_z_order_hold.rs	typing_never_paints_over_ink_and_holds_the_z_order	console-z-order-refines-model	
tests/console_sparse_ink_motion.rs	a_real_caret_move_waits_for_coherence_then_follows_normally	console-observation-admission	
tests/console_sparse_ink_motion.rs	invalid_surface_cannot_materialize_static_capture_or_replay_an_unfocused_trip	console-observation-admission	
tests/console_sparse_ink_motion.rs	real_failed_observations_conform_for_idle_and_airborne_locomotion	console-observation-admission	
tests/console_sparse_ink_motion.rs	sparse_ink_stays_local_across_observation_budget_edges	console-observation-admission	
tests/copies_are_not_moves.rs	a_completion_menu_or_a_twin_drawn_late_is_not_where_the_line_went	copy-is-not-arrival	
tests/copies_are_not_moves.rs	a_copy_beside_the_line_is_not_where_the_line_went_flat_and_paced	copy-is-not-arrival	
tests/copies_are_not_moves.rs	a_copy_drawn_after_the_last_key_then_erased_at_every_pause_and_pacing	copy-is-not-arrival	
tests/copies_are_not_moves.rs	a_copy_standing_at_the_far_target_is_not_a_move	copy-is-not-arrival	twin-evidence-timing
tests/copies_are_not_moves.rs	a_full_row_budget_hides_no_copy_behind_a_key	witness-row-slots	
tests/copies_are_not_moves.rs	a_full_row_budget_still_sees_the_twin	witness-row-slots	
tests/copies_are_not_moves.rs	a_glyph_caught_up_under_its_twin_is_armed_again	copy-is-not-arrival	
tests/copies_are_not_moves.rs	a_line_erased_under_its_twin_goes_nowhere	copy-is-not-arrival	
tests/copies_are_not_moves.rs	a_row_the_composed_host_drops_is_not_a_door_for_a_copy	witness-row-slots	
tests/copies_are_not_moves.rs	a_streamed_quote_then_ctrl_u_at_every_pause_and_pacing	copy-is-not-arrival	
tests/copies_are_not_moves.rs	a_streamed_row_quoting_the_composer_two_rows_up_is_not_where_it_went	copy-is-not-arrival	
tests/copies_are_not_moves.rs	a_typo_fixed_in_place_under_the_previous_command_then_killed_goes_nowhere	copy-is-not-arrival	
tests/copies_are_not_moves.rs	an_alternating_flicker_two_rows_up_is_not_where_the_line_went	copy-is-not-arrival	
tests/copies_are_not_moves.rs	fzf_re_sorting_its_list_under_ctrl_u_is_not_where_the_query_went	copy-is-not-arrival	
tests/copies_are_not_moves.rs	fzf_s_info_line_and_first_item_are_not_where_the_query_went	copy-is-not-arrival	
tests/copies_are_not_moves.rs	fzf_s_late_list_then_ctrl_u_at_every_pause_and_pacing	copy-is-not-arrival	
tests/copies_are_not_moves.rs	fzf_s_list_landing_after_the_query_s_first_keys_is_not_where_the_query_went	copy-is-not-arrival	
tests/copies_are_not_moves.rs	scrolls_band_moves_and_alt_screens_do_not_make_a_copy_an_arrival	copy-is-not-arrival	
tests/copies_are_not_moves.rs	the_limit_a_line_moved_beside_its_twin_and_killed_before_it_was_learned_lights_it	twin-evidence-timing	
tests/copies_are_not_moves.rs	vim_s_yyp_then_s_at_every_pause_and_pacing	copy-is-not-arrival	
tests/copy_beside_the_line.rs	a_copy_written_on_a_split_pane_s_new_last_row_by_a_scroll_is_not_where_the_line_went	copy-is-not-arrival	
tests/cursor_bench.rs	cursor_rainbow_hot_ribbon_work_stays_below_its_normalized_share	rk-ribbon-quad-share	
tests/cursor_cat_conformance.rs	real_cursor_cat_collectible_lifecycle_conforms_and_hidden_expiry_is_caught	cursor-cat-refines-model	
tests/cursor_cat_conformance.rs	real_cursor_cat_long_presentable_gap_settles_without_a_deferred_fade	cursor-cat-refines-model	
tests/cursor_cat_conformance.rs	real_cursor_cat_winces_only_for_complete_cues_and_repeats_build_force	cursor-cat-refines-model	
tests/erased_under_its_twin.rs	a_full_row_budget_still_arms_a_line_s_first_keys_on_what_stood_above_them	witness-row-slots	
tests/erased_under_its_twin.rs	a_full_row_budget_still_lets_a_line_moved_two_rows_down_carry_its_band	move-is-followed	
tests/erased_under_its_twin.rs	a_redraw_erasing_a_line_typed_under_its_twin_over_a_slow_link_follows_nothing	copy-is-not-arrival	
tests/erased_under_its_twin.rs	a_vim_line_typed_under_its_duplicate_then_undone_follows_nothing	copy-is-not-arrival	
tests/erased_under_its_twin.rs	an_fzf_query_cleared_under_its_best_match_follows_nothing	copy-is-not-arrival	
tests/hop_wake_fold.rs	a_key_in_a_hop_wake_does_not_revive_a_retracting_band	wake-fold	
tests/hop_wake_fold.rs	a_word_hop_landing_inside_the_band_s_stale_bounds_folds_its_wake	wake-fold	
tests/hop_wake_fold.rs	the_wake_fold_keeps_one_owner_per_column_per_cohort	wake-fold	
tests/ink_wrap_line_fill_repro.rs	hidden_mid_repaint_frame_suppresses_the_spawn_entirely	typed-reanchor-no-phantom-vector	
tests/ink_wrap_line_fill_repro.rs	ink_wrap_fire_stays_confined_to_typed_cells	typed-reanchor-no-phantom-vector	
tests/ink_wrap_line_fill_repro.rs	ink_wrap_laser_lays_no_full_vector_sweep	typed-reanchor-no-phantom-vector	
tests/ink_wrap_line_fill_repro.rs	ink_wrap_rainbow_keeps_the_ribbon_no_zoom	typed-reanchor-no-phantom-vector	
tests/jump_over_fading_band.rs	a_word_hop_landing_in_the_last_twenty_ms_of_the_swoosh_lays_its_corridor_whole	wake-corridor-host	
tests/jump_over_fading_band.rs	a_word_hop_over_a_live_band_is_a_scrub_and_lays_nothing	scrub-never-raises-light	
tests/jump_over_fading_band.rs	a_word_hop_over_a_retracting_band_leaves_nothing_to_the_band_s_clock	wake-corridor-host	
tests/jump_over_fading_band.rs	home_then_end_over_a_fading_band_then_typing_leaves_no_hole_in_the_line	wake-corridor-host	
tests/jump_over_fading_band.rs	the_end_back_over_dark_ground_lays_one_corridor_out_to_the_island_it_re_owns	wake-corridor-host	
tests/keys_typed_at_the_line_start.rs	keys_typed_at_the_start_of_a_lit_line_step_one_stop_a_key	cohort-walk-invariant	
tests/kitty_activation_conformance.rs	real_cursor_cat_singing_bypass_still_requires_sixteen_travel_events	kitty-sing-activation-refines-model	
tests/kitty_activation_conformance.rs	real_kitty_sing_arm_break_release_and_finish_conform	kitty-sing-activation-refines-model	
tests/kitty_activation_conformance.rs	real_kitty_sing_lazy_release_maps_to_the_same_bounded_tail	kitty-sing-activation-refines-model	
tests/licensed_insert_band.rs	a_licensed_insert_is_laid_whole_in_every_frame_shape_that_reaches_the_glass_whole	licensed-insert-bridge	
tests/licensed_insert_band.rs	a_one_frame_tear_of_the_completed_word_heals_while_the_hand_types	torn-read-restore	
tests/licensed_insert_band.rs	a_torn_reappearance_the_licence_cannot_buy_lays_nothing	licensed-insert-bridge	
tests/licensed_insert_band.rs	program_output_at_the_caret_lays_no_light_however_its_frame_reaches_the_glass	licensed-insert-bridge	
tests/licensed_typed_parity.rs	earned_light_survives_a_cold_program_move	licence-is-precondition	
tests/licensed_typed_parity.rs	split_batch_suffix_is_dark_but_keeps_resident_light_and_advances_anchors	licence-is-precondition	
tests/midline_insert_band.rs	a_tail_the_program_rewrote_still_leaves	release-accounting	
tests/midline_insert_band.rs	typing_inside_a_lit_line_keeps_the_tail_it_pushes_lit	shift-pass-carries-insert	
tests/midline_insert_colour_seam.rs	a_hop_over_the_pushed_tail_does_not_repaint_it	scrub-never-raises-light	
tests/midline_insert_colour_seam.rs	a_mid_line_backspace_takes_the_tail_s_light_and_leaves_no_seam	release-accounting	
tests/midline_insert_colour_seam.rs	every_cell_a_mid_line_insert_leaves_is_on_its_cohorts_walk	cohort-walk-invariant	
tests/midline_insert_colour_seam.rs	typing_inside_a_lit_line_keeps_the_walk_continuous_at_the_pushed_tail	cohort-walk-invariant	
tests/moved_again_without_a_key.rs	a_far_row_read_dropped_on_one_newline_s_frame_costs_no_later_follow	witness-row-slots	
tests/moved_again_without_a_key.rs	a_row_the_host_has_a_slot_for_is_never_withheld	witness-row-slots	
tests/moved_again_without_a_key.rs	a_torn_repaint_that_draws_the_new_row_a_frame_before_it_erases_the_old_still_follows	twin-evidence-timing	
tests/moved_again_without_a_key.rs	an_inline_box_at_the_screen_s_bottom_keeps_its_band_under_every_streamed_row	move-is-followed	
tests/moved_again_without_a_key.rs	an_inline_box_moved_by_streamed_rows_follows_every_move	move-is-followed	
tests/moves_are_followed.rs	a_box_moved_on_consecutive_frames_follows_every_move	move-is-followed	
tests/moves_are_followed.rs	a_box_under_streaming_output_keeps_its_band	move-is-followed	
tests/moves_are_followed.rs	a_line_relocated_again_and_again_carries_its_band	move-is-followed	
tests/moves_are_followed.rs	a_line_relocated_by_a_scroll_follows_until_its_new_row_has_stood_twin_min	twin-evidence-timing	
tests/moves_are_followed.rs	a_line_under_an_identical_command_pushed_down_by_a_notice_follows	move-is-followed	
tests/moves_are_followed.rs	a_multi_line_inline_box_pushed_by_streamed_rows_follows	move-is-followed	
tests/moves_are_followed.rs	a_paste_then_a_wrap_on_the_next_frame_follows	move-is-followed	
tests/moves_are_followed.rs	a_tall_lit_cohort_moved_past_the_follow_offsets_carries_every_row	witness-row-slots	move-is-followed
tests/moves_are_followed.rs	a_tall_lit_prompt_moved_carries_every_line_it_can	move-is-followed	
tests/moves_are_followed.rs	a_torn_repaint_across_up_to_three_frames_follows	twin-evidence-timing	
tests/moves_are_followed.rs	claude_code_s_composer_follows_after_a_pause_before_each_line	move-is-followed	
tests/moves_are_followed.rs	claude_code_s_composer_follows_every_shift_enter	witness-row-slots	
tests/moves_are_followed.rs	claude_code_s_composer_follows_every_wrap_at_every_width	move-is-followed	
tests/moves_are_followed.rs	tall_and_unsynced_composers_follow_every_wrap	move-is-followed	
tests/moves_are_followed.rs	the_limit_a_move_onto_a_row_a_copy_stood_on_for_twin_min_melts	twin-evidence-timing	
tests/moves_are_followed.rs	the_limit_a_torn_relocation_whose_new_row_stood_twin_min_is_a_copy	twin-evidence-timing	
tests/moves_are_followed.rs	the_limit_an_unsynced_composer_s_wrap_torn_past_twin_min_melts	twin-evidence-timing	
tests/new_line_fade.rs	a_declined_hidden_relocation_whose_text_went_with_it_carries_the_band_with_it	move-is-followed	
tests/new_line_fade.rs	a_declined_hidden_relocation_with_the_old_row_s_text_intact_moves_only_the_mirror	text-gone-leaves-by-swoosh	
tests/new_line_fade.rs	a_glyph_the_clear_paints_on_a_typed_space_leaves_with_the_swoosh_not_ahead_of_it	text-gone-leaves-by-swoosh	
tests/new_line_fade.rs	a_released_band_that_different_text_lands_under_it_a_frame_later_melts_fast	release-accounting	
tests/new_line_fade.rs	a_shell_s_enter_leaves_the_typed_row_s_band_to_its_own_swoosh	text-gone-leaves-by-swoosh	
tests/new_line_fade.rs	a_submit_split_across_a_hidden_frame_still_leaves_through_the_swoosh	text-gone-leaves-by-swoosh	
tests/new_line_fade.rs	a_submit_that_clears_the_composer_lets_the_band_leave_through_its_own_swoosh_not_a_cut	text-gone-leaves-by-swoosh	
tests/new_line_fade.rs	a_submit_that_moves_the_cleared_composer_down_fades_the_old_row_the_same_way	text-gone-leaves-by-swoosh	
tests/nova_channel_budget.rs	the_over_budget_channel_is_drawn_in_full_not_truncated	nova-draw-is-total	
tests/nova_channel_budget.rs	the_streak_share_is_tail_bounded_not_column_bounded	output-streak-quad-bound	
tests/nova_phase_conformance.rs	real_nova_phase_walk_conforms_to_nova_phase_model	nova-phase-refines-model	
tests/output_streak_attribution.rs	a_landing_no_frame_watched_mints_no_light	os-license	
tests/output_streak_attribution.rs	another_rows_timer_cannot_paint_the_stationary_composer	output-streak-attribution	
tests/output_streak_attribution.rs	compact_row_growth_at_fixed_dimensions_preserves_live_output	output-streak-attribution	
tests/output_streak_attribution.rs	ordinary_contiguous_output_can_still_sweep_its_full_changed_run	output-streak-attribution	
tests/output_streak_attribution.rs	real_cell_observations_refine_the_attribution_snapshot_model	output-streak-attribution	
tests/output_streak_attribution.rs	sparse_particle_rewrites_never_light_the_unchanged_composer_or_gaps	output-streak-attribution	
tests/output_streak_attribution.rs	viewport_change_retires_pending_and_live_output_even_before_a_new_token	output-streak-attribution	
tests/output_streak_attribution.rs	width_only_reflow_does_not_licence_existing_text_as_new_output	output-streak-attribution	
tests/output_streak_conformance.rs	real_output_episode_and_synth_refine_delivery_model	output-streak-delivery-refines-model	
tests/partial_release.rs	a_partial_clear_and_an_identical_rewrite_a_frame_later_restore_the_light	torn-read-restore	
tests/partial_release.rs	a_suffix_the_program_cleared_leaves_and_the_prefix_stands	release-accounting	
tests/partial_release.rs	a_suffix_the_program_cleared_leaves_no_light_ahead_of_a_typing_hand	release-accounting	
tests/partial_release.rs	a_trailing_space_after_a_suffix_the_program_cleared_leaves_with_it	release-accounting	
tests/partial_release.rs	a_whole_clear_after_a_suffix_the_program_cleared_still_leaves_through_the_swoosh	text-gone-leaves-by-swoosh	
tests/pet_art_quality.rs	a_yawn_keeps_its_gape_below_the_mouth_cull	pet-bake-properties	
tests/pet_art_quality.rs	contact_beats_have_distinct_silhouettes_at_terminal_sizes	pet-bake-properties	
tests/pet_art_quality.rs	every_planted_pose_stands_on_one_ground_line	pet-art-roster-table	
tests/pet_art_quality.rs	every_pose_shares_one_viewbox_and_one_anchor_set	pet-art-roster-table	
tests/pet_art_quality.rs	no_pose_escapes_its_viewbox	pet-art-roster-table	
tests/pet_art_quality.rs	reading_gaze_keeps_the_perch_body_geometry_fixed	pet-art-roster-table	
tests/pet_art_quality.rs	the_gaits_vertical_survives_the_pixel_grid_at_every_cell_height	pet-bake-properties	
tests/pet_coordinate_seams.rs	a_breed_leaver_crosses_a_boundary_where_it_was_drawn	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	a_carried_ghost_keeps_its_own_tile	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	a_font_step_and_a_live_drag_keep_the_resident_on_glass	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	a_ghost_is_drawn_as_the_body_the_user_last_saw	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	a_ghost_its_own_glass_refuses_hands_its_presence_to_the_arriver	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	a_ghost_wears_the_palette_its_body_was_drawn_in	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	a_quick_return_continues_the_one_cat_it_comes_back_to	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	a_refused_ghost_is_handed_over_once	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	a_split_pane_focus_switch_crossfades_across_panes	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	a_tall_screen_crossfades_the_body_it_replaced	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	an_adopted_body_goes_on_in_the_look_the_user_saw	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	an_alt_screen_enter_and_exit_crossfade_instead_of_cutting	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	an_arriver_resumes_a_ghosts_palette_only_on_the_surface_it_was_drawn_on	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	an_incoherent_frame_holds_the_pet_and_a_sub_row_scroll_moves_nothing	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	every_crossfade_ghost_leaves_glass_on_its_arrivers_full_frame	pet-surface-seam-refines-model	
tests/pet_coordinate_seams.rs	every_ghost_fades_in_its_own_pane_across_three_panes	pet-surface-seam-refines-model	
tests/pet_degenerate_geometry.rs	no_legal_sense_makes_the_pet_non_finite_or_off_grid	pet-no-nonfinite	
tests/pet_escort_primacy.rs	a_settled_cat_typed_at_keeps_its_own_animation	escort-primacy	
tests/pet_escort_primacy.rs	plain_typing_is_the_pet_with_no_console_layer	escort-primacy	
tests/pet_escort_primacy.rs	the_caret_is_not_an_obstruction_to_its_own_escort	escort-primacy	
tests/pet_follows_a_hidden_caret.rs	a_pet_follows_a_caret_that_moves_while_the_cursor_is_hidden	pet-hidden-caret-bounded-hold	
tests/pet_follows_a_hidden_caret.rs	a_repainting_console_does_not_freeze_the_pet_away_from_the_caret	pet-hidden-caret-bounded-hold	
tests/pet_holds_through_a_torn_frame.rs	a_finished_frames_hidden_caret_is_held_for_a_repaint_and_then_followed	pet-torn-frame-hold	
tests/pet_holds_through_a_torn_frame.rs	a_torn_full_repaint_neither_moves_nor_fades_the_settled_pet	pet-torn-frame-hold	
tests/pet_position_authority.rs	a_caret_that_jumps_does_not_teleport_the_body	escort-locomotion-speed-limit	
tests/pet_position_authority.rs	streaming_output_never_teleports_the_body	escort-locomotion-speed-limit	
tests/pet_resident_standoff.rs	the_resident_settles_where_the_escort_settles	escort-primacy	
tests/pet_typing_beside_its_guess.rs	a_guessed_letter_never_throws_the_pet_off_its_seat	pw-classify	
tests/pet_typing_beside_its_guess.rs	the_legs_keep_one_gait_while_typing	gait-by-speed-and-distance	
tests/pet_typing_beside_its_guess.rs	the_pet_rides_the_composer_down_when_its_chips_come_onto_it	protected-surface-yield	
tests/pet_typing_beside_its_guess.rs	the_pet_walks_on_top_of_the_composer_over_the_harness_switch	host-line-head-law	protected-surface-yield
tests/pet_typing_beside_its_guess.rs	a_wrap_typed_through_is_one_bound	no-teleport-law	
tests/phrase_pause_gaps.rs	an_ink_composer_with_a_hidden_caret_keeps_every_boundary_space_lit	typed-columns-owned	
tests/rainbow_kitty_v2_frame_cost.rs	rainbow_kitty_v2_steady_frames_allocate_nothing_and_idle_to_exactly_zero	rk-v2-steady-tick-contract	
tests/rainbow_pixel_continuity.rs	pixel_continuity_across_backspaces_retypes_glides_and_wraps	rewrap-carries-word	typed-columns-owned
tests/rainbow_pixel_continuity.rs	pixel_continuity_across_owners_phrase_and_word_boundary_pauses	typed-columns-owned	
tests/rainbow_pixel_continuity.rs	pixel_continuity_across_single_and_batched_typing_cadences	typed-columns-owned	
tests/rainbow_pixel_continuity.rs	pixel_continuity_across_tui_diff_spaces_status_prints_and_torn_frames	typed-columns-owned	
tests/rainbow_pixel_continuity.rs	pixel_continuity_with_resident_pet_flying_kitty_sparkle_words_and_classic_rainbow	typed-columns-owned	
tests/relaid_composer_echo.rs	a_full_damage_relayout_is_followed_as_main	crossed-echo-laid	
tests/relaid_composer_echo.rs	a_key_whose_echo_rides_a_relayout_a_row_up_is_laid_under_its_glyph	crossed-echo-laid	
tests/relaid_composer_echo.rs	a_key_whose_echo_rides_a_relayout_three_rows_up_is_laid_under_its_glyph	crossed-echo-laid	
tests/relaid_composer_echo.rs	a_keyless_relayout_moves_the_band_and_the_next_keys_join_it	crossed-echo-laid	
tests/relaid_composer_echo.rs	a_prefix_a_keyless_relayout_carried_revives_when_the_hand_resumes	crossed-echo-laid	
tests/relaid_composer_echo.rs	every_stall_width_around_the_relayout_lays_every_key	crossed-echo-laid	
tests/relaid_composer_echo.rs	the_band_laid_before_the_relayout_stays_lit_under_the_hand	crossed-echo-laid	
tests/review_real_bytes.rs	claude_code_s_arrow_insert_keeps_the_pushed_tail_lit_at_every_read_size	shift-pass-carries-insert	
tests/review_real_bytes.rs	the_owner_s_line_torn_at_the_same_column_stays_whole_into_the_swoosh	typed-columns-owned	
tests/revived_line_keeps_its_stops.rs	a_line_revived_when_the_key_s_echo_lands_a_frame_late_keeps_its_stops	revive-restores-stops	
tests/revived_line_keeps_its_stops.rs	a_run_no_key_saw_standing_comes_back_on_its_own_walk	revive-restores-stops	
tests/revived_line_keeps_its_stops.rs	a_wrapped_line_revived_after_a_rest_lights_its_second_row_from_its_first_glyph	revive-restores-stops	
tests/revived_line_keeps_its_stops.rs	claude_codes_composer_revived_after_a_rest_keeps_its_stops	revive-restores-stops	
tests/revived_line_keeps_its_stops.rs	without_the_revive_the_key_continues_the_walk_and_no_stop_moves	revive-restores-stops	
tests/ribbon_arrival_conformance.rs	the_real_witness_writes_the_arrival_evidence_on_every_trace	witness-arrival-evidence-refines-model	
tests/ribbon_coalesced_wrap.rs	the_space_class_is_required_to_relocate_the_word	rewrap-carries-word	
tests/ribbon_follow_conformance.rs	the_real_follow_pass_is_the_twin_contract_on_every_six_record_run	witness-follow-refines-twin-model	
tests/ribbon_row_hold_conformance.rs	continuous_typing_below_does_not_postpone_the_earlier_rows_exit	ribbon-row-hold-refines-model	
tests/ribbon_row_hold_conformance.rs	typing_and_erase_renew_only_their_own_row_and_conform	ribbon-row-hold-refines-model	
tests/scrub_gaps.rs	a_hop_back_over_a_melting_stub_raises_no_cell_s_light_at_the_host_seam	scrub-never-raises-light	
tests/scrub_gaps.rs	a_scrub_over_holes_a_hop_filled_leaves_an_abandoned_band_alone_at_the_host_seam	scrub-never-raises-light	
tests/scrub_gaps.rs	a_word_scrub_through_an_ink_repaint_leaves_no_gap_and_both_engines_agree	scrub-never-raises-light	
tests/scrub_gaps.rs	a_word_scrub_through_zsh_leaves_no_gap_and_both_engines_agree	scrub-never-raises-light	
tests/serde_corpus.rs	custom_word_fragments_answer_as_recorded	pack-decoders-denotation	
tests/serde_corpus.rs	toy_packs_answer_as_recorded	pack-decoders-denotation	
tests/shed_frame_is_still_a_heard_key.rs	a_dark_audible_run_never_fills_the_cue_backlog	sound-seam-independent-of-amplitude	
tests/shed_frame_is_still_a_heard_key.rs	a_master_off_tick_still_wipes_the_sound_ledger	sound-seam-independent-of-amplitude	
tests/shed_frame_is_still_a_heard_key.rs	a_shed_frame_keeps_the_key_seam_open	sound-seam-independent-of-amplitude	
tests/shed_frame_is_still_a_heard_key.rs	a_shed_tick_keeps_the_keyed_click_ledger	sound-seam-independent-of-amplitude	
tests/shed_frame_is_still_a_heard_key.rs	idle_zero_holds_while_the_audio_seam_is_open	glow-idle-to-zero	
tests/shed_frame_is_still_a_heard_key.rs	the_silent_visual_click_reads_this_tick_s_thermal_constants	sound-seam-independent-of-amplitude	
tests/the_owners_composer_gaps.rs	a_glyph_replaced_after_its_erase_echoed_claims_no_key	rb-same-caret-echo	
tests/the_owners_composer_gaps.rs	a_glyph_replaced_in_place_without_an_erase_claims_no_key	rb-same-caret-echo	
tests/the_owners_composer_gaps.rs	a_growth_echo_after_a_pause_lights_its_own_cell	crossed-echo-laid	
tests/the_owners_composer_gaps.rs	a_growth_echo_after_one_typed_glyph_lights_its_own_cell	crossed-echo-laid	
tests/the_owners_composer_gaps.rs	a_growth_echo_inside_a_hops_wake_lights_its_own_cell	crossed-echo-laid	
tests/the_owners_composer_gaps.rs	a_growth_echo_torn_before_its_caret_lights_its_own_cell	crossed-echo-laid	
tests/the_owners_composer_gaps.rs	a_space_retyped_over_a_typo_in_one_repaint_lights_its_own_cell	rb-same-caret-echo	
tests/the_owners_composer_gaps.rs	a_space_that_grows_the_box_lights_its_own_cell	crossed-echo-laid	
tests/the_owners_composer_gaps.rs	a_torn_box_move_down_keeps_the_edit_lit	crossed-echo-laid	
tests/the_owners_composer_gaps.rs	a_word_wrapped_in_a_stalled_repaint_is_lit_whole	rewrap-carries-word	
tests/torn_edit_gaps.rs	a_backspace_run_under_torn_reads_keeps_the_band_whole	ribbon-contiguity-invariant	
tests/torn_edit_gaps.rs	a_growing_composer_torn_at_its_wrap_keeps_the_band_whole	typed-columns-owned	
tests/torn_edit_gaps.rs	a_torn_backspace_frame_keeps_the_standing_text_lit	torn-read-restore	
tests/torn_edit_gaps.rs	a_torn_word_kill_frame_keeps_the_standing_text_lit	torn-read-restore	
tests/torn_edit_gaps.rs	claude_code_1024_byte_reads_with_the_boundary_at_the_wrap_keep_both_rows_whole	typed-columns-owned	
tests/torn_edit_gaps.rs	codex_particle_replay_stays_one_run_whole_and_torn_in_half_under_the_capped_sync_hold	typed-columns-owned	
tests/torn_edit_gaps.rs	ctrl_k_then_typing_on_keeps_one_run	ribbon-contiguity-invariant	
tests/torn_edit_gaps.rs	every_key_in_1024_byte_reads_for_three_seconds_at_one_fixed_boundary_keeps_the_band_whole	typed-columns-owned	
tests/torn_read_gaps.rs	a_really_cleared_tail_leaves_and_takes_only_its_own_spaces	release-accounting	
tests/torn_read_gaps.rs	a_released_tail_is_restored_although_the_key_is_laid_before_the_restore	torn-read-restore	
tests/torn_read_gaps.rs	a_spinner_repaint_torn_under_to_with_no_key_in_flight_keeps_the_band_whole	torn-read-restore	
tests/torn_read_gaps.rs	a_stalled_repaint_restores_the_suffix_when_typing_resumes	torn-read-restore	typed-columns-owned
tests/torn_read_gaps.rs	a_stalled_repaints_dark_tail_follows_its_standing_prefix	torn-read-restore	wit-evidence-carriage
tests/torn_read_gaps.rs	a_stalled_repaints_dark_tail_shifts_with_its_standing_prefix	torn-read-restore	wit-shift-runs
tests/torn_read_gaps.rs	a_whole_run_re_laid_on_another_row_follows_its_text	move-is-followed	
tests/torn_read_gaps.rs	an_insert_through_word_hops_under_torn_reads_keeps_the_band_whole	typed-columns-owned	
tests/torn_read_gaps.rs	abandoning_a_stalled_repaints_run_retires_its_custody	text-gone-leaves-by-swoosh	torn-read-restore
tests/torn_read_gaps.rs	claude_code_frames_in_1024_byte_reads_keep_the_band_whole_while_the_hand_types	typed-columns-owned	
tests/torn_read_gaps.rs	claude_code_s_recorded_bytes_read_whole_or_torn_keep_the_band_whole_while_typing	typed-columns-owned	
tests/torn_read_gaps.rs	the_owners_line_torn_under_to_of_into_keeps_its_band_whole	torn-read-restore	
tests/torn_read_gaps.rs	replaced_glyphs_revoke_a_stalled_repaints_custody	torn-read-restore	retire-and-release-identity
tests/torn_read_gaps.rs	moving_only_part_of_a_standing_prefix_does_not_guess_where_its_dark_tail_went	torn-read-restore	wit-evidence-carriage
tests/torn_read_gaps.rs	stalled_repaint_custody_is_dark_and_expires_with_its_run	torn-read-restore	release-accounting
tests/torn_repaint_hidden_caret.rs	a_correction_repainted_torn_keeps_its_keys_lit	rb-same-caret-echo	
tests/torn_repaint_hidden_caret.rs	a_correction_the_app_has_not_drawn_spends_no_key	rb-same-caret-echo	
tests/torn_repaint_hidden_caret.rs	a_key_erased_and_never_drawn_leaves_the_next_key_its_echo	rb-erase-echo-order	
tests/torn_repaint_hidden_caret.rs	a_key_erased_before_its_echo_keeps_the_next_key_lit	rb-erase-echo-order	
tests/torn_repaint_hidden_caret.rs	a_torn_full_screen_repaint_keeps_the_boundary_space_lit	motion-caret-contract	
tests/torn_repaint_hidden_caret.rs	a_torn_mid_line_echo_keeps_the_space_lit	motion-caret-contract	
tests/torn_repaint_hidden_caret.rs	a_torn_repaint_outliving_the_terminal_timeout_keeps_the_line_whole	motion-caret-contract	
tests/torn_repaint_hidden_caret.rs	every_bracket_length_keeps_the_line_whole_under_the_shipping_contract	motion-caret-contract	
tests/torn_repaint_hidden_caret.rs	every_correction_echoed_late_or_merged_keeps_the_line_lit	rb-erase-echo-order	
tests/trick_flash_alloc.rs	a_flash_costs_the_input_path_and_the_frame_path_no_allocation	trick-paths-allocation-free	
tests/typed_tricks_alloc.rs	a_warmed_listener_is_allocation_free_on_every_lane	trick-paths-allocation-free	
tests/wake_and_landing_walk_origins.rs	a_jump_s_landing_marks_wear_the_band_they_land_on	one-walk-per-row	
tests/wake_and_landing_walk_origins.rs	a_wake_keeps_the_colours_of_the_band_it_covers	one-walk-per-row	
tests/walk_continuity_census.rs	census	cohort-walk-invariant	
tests/word_reflow_identity.rs	spent_profanity_horizontal_redraw_does_not_rearm_or_decorate_fix	word-deco-rekey-preserves-spent-guard	
tests/wrap_code_reflow.rs	a_delayed_first_key_still_carries_the_glued_word_down	rewrap-carries-word	
tests/wrap_code_reflow.rs	a_scrub_over_the_word_keeps_the_word_lit_when_it_wraps_down	rewrap-carries-word	
tests/wrap_code_reflow.rs	a_soft_wrapped_word_that_stays_on_its_row_keeps_its_light_there	rewrap-carries-word	
tests/wrap_code_reflow.rs	a_soft_wrapped_word_the_next_key_reflows_comes_down_with_its_light	rewrap-carries-word	
tests/wrap_code_reflow.rs	a_space_that_lets_the_word_fit_the_row_above_carries_its_band_up	rewrap-carries-word	
tests/wrap_code_reflow.rs	a_stale_typed_space_lays_nothing_on_the_continuation_indent	rewrap-carries-word	
tests/wrap_code_reflow.rs	a_token_the_box_hard_breaks_keeps_its_band_on_the_row_it_fills	rewrap-carries-word	
tests/wrap_code_reflow.rs	a_typo_fixed_inside_the_word_keeps_the_word_lit_when_it_wraps_down	rewrap-carries-word	
tests/wrap_code_reflow.rs	a_word_begun_at_a_navigated_caret_keeps_its_first_glyph_when_it_wraps_down	rewrap-carries-word	
tests/wrap_code_reflow.rs	claude_codes_recorded_reflow_keeps_every_typed_glyph_lit_and_strands_nothing	rewrap-carries-word	
tests/wrap_code_reflow.rs	control_a_word_after_the_hands_own_space_is_relaid_exactly	rewrap-carries-word	
tests/wrap_code_reflow.rs	the_first_key_that_pushes_a_glued_word_down_is_judged_not_parked	rewrap-carries-word	
tests/wrapped_composer_band.rs	a_backspace_run_at_the_end_never_punches_a_hole_in_the_wrapped_band	ribbon-contiguity-invariant	
tests/wrapped_composer_band.rs	a_backspace_run_mid_word_never_punches_a_hole_in_the_wrapped_band	ribbon-contiguity-invariant	
tests/wrapped_composer_band.rs	an_insert_mid_line_never_punches_a_hole_in_the_wrapped_band	ribbon-contiguity-invariant	
tests/zsh_isearch_band.rs	a_caret_resting_right_of_the_echo_or_behind_a_key_keeps_it	torn-lay-recall	
tests/zsh_isearch_band.rs	a_click_after_the_walk_back_still_hands_back_the_row_s_doubt	torn-lay-recall	
tests/zsh_isearch_band.rs	a_hidden_caret_s_echo_is_judged_alike_on_every_path	minibuffer-gate	
tests/zsh_isearch_band.rs	a_key_after_the_walk_back_keeps_the_recall	torn-lay-recall	
tests/zsh_isearch_band.rs	a_keyless_walk_back_onto_a_typed_echo_keeps_it	torn-lay-recall	
tests/zsh_isearch_band.rs	a_longer_search_lays_no_band_on_cells_nobody_typed	minibuffer-gate	
tests/zsh_isearch_band.rs	a_match_that_hops_along_its_row_lays_no_ink_there	re-anchor-landing-gate	
tests/zsh_isearch_band.rs	a_parked_caret_s_own_echo_lights_after_enter_on_the_caret_s_row	minibuffer-gate	
tests/zsh_isearch_band.rs	a_parked_caret_s_own_echo_still_lights	minibuffer-gate	
tests/zsh_isearch_band.rs	a_wide_glyph_echo_below_a_parked_caret_still_lights	press-credits-foreign-to-live	
tests/zsh_isearch_band.rs	a_wrapped_match_lays_nothing_under_zle_s_fake_cursor	minibuffer-gate	
tests/zsh_isearch_band.rs	an_entry_switch_shaped_like_a_soft_wrap_lays_nothing_on_the_match	soft-wrap-rejects-left-rewrite	
tests/zsh_isearch_band.rs	bash_s_reverse_search_retreat_lays_nothing_under_the_match	re-anchor-landing-gate	
tests/zsh_isearch_band.rs	isearch_ink_retires_within_the_idle_ceiling	glow-idle-to-zero	
tests/zsh_isearch_band.rs	one_torn_read_at_any_boundary_lays_no_band_on_the_match_row	parked-caret-row-ownership	
tests/zsh_isearch_band.rs	ordinary_typing_keeps_its_row_and_a_classified_boundary_restores_it	parked-caret-row-ownership	
tests/zsh_isearch_band.rs	a_composer_inserting_again_settles_its_parked_row	parked-caret-row-ownership	
tests/zsh_isearch_band.rs	a_licensed_insert_takes_the_hand_s_row	parked-caret-row-ownership	
tests/zsh_isearch_band.rs	a_recalled_lay_hands_back_the_row_s_doubt	parked-caret-row-ownership	
tests/zsh_isearch_band.rs	a_row_the_hand_put_input_on_stays_its_own_after_the_hand_moves_on	parked-caret-row-ownership	
tests/zsh_isearch_band.rs	plain_prompt_typing_lights_and_retires_on_the_normal_schedule	own-glyph-echo-lights	
tests/zsh_isearch_band.rs	the_gates_refuse_nothing_in_claude_code_s_or_codex_s_composer	minibuffer-gate	
tests/zsh_isearch_band.rs	torn_reads_lay_no_band_on_the_match_row	parked-caret-row-ownership	
tests/zsh_isearch_band.rs	typeahead_into_the_minibuffer_lays_nothing	minibuffer-gate	
tests/zsh_isearch_band.rs	whole_reads_lay_no_band_on_the_match_row_or_under_zle_s_fake_cursor	minibuffer-gate	
src/aos_momentum/lease_conformance_tests.rs	the_real_ledger_follows_the_lease_model_in_lock_step	aos-lease-refines-model	
src/aos_momentum/lease_conformance_tests.rs	every_transition_of_the_lease_model_is_a_real_step	aos-lease-refines-model	
src/aos_momentum/lease_conformance_tests.rs	negative_control_expire_keeps_lease_the_real_tick_releases_the_job	aos-lease-refines-model	
src/aos_momentum/lease_conformance_tests.rs	negative_control_duplicate_an_equal_resend_is_counted_not_accepted	aos-lease-refines-model	
src/aos_momentum/lease_conformance_tests.rs	negative_control_stale_a_late_result_for_an_open_job_waits_for_its_claim	aos-lease-refines-model	
src/aos_momentum/lease_conformance_tests.rs	negative_control_post_an_interrupt_drains_the_holder_and_drops_its_result	aos-lease-refines-model	
src/aos_momentum/lease_conformance_tests.rs	negative_control_ignored_every_differing_claim_is_compared_and_voids	aos-lease-refines-model	
src/aos_momentum/lease_conformance_tests.rs	negative_control_attempts_the_last_expiry_exhausts_the_job	aos-lease-refines-model	
```


## 3. Coverage

- 3027 tests in the crate (`#[test]` count at `ab02e1da7`). 2933 map to one of the 855 specs as primary. 95 are not spec samples (§4).
- 11 specs subsume no test today (they are the real-code statements their test-local model tests should become, or are corollaries only).
- Biggest collapses (primary tests): `glow-exact-payment` (29), `rb-walk-continuity` (29), `rb-follow-witness` (29), `typed-columns-owned` (27), `glow-no-licence-no-light` (25), `copy-is-not-arrival` (23), `rewrap-carries-word` (23), `glow-park` (22), `insert-licence` (21), `output-streak-attribution` (17), `echo-ledger-refinement` (16), `pet-surface-seam-refines-model` (16).
- Gap histogram (audit §7 row → specs blocked by it): 0 → 854, 1 → 775, 2 → 586, 3 → 564, 4 → 213, 5 → 391, 6 → 162, 7 → 443, 8 → 13, 9 → 11, 10 → 78, 11 → 10, 12 → 95, 13 → 38, 14 → 17, 16 → 3.
- Non-spec tests by action: becomes-spec-after-fix 12, delete 51, move-to-bench 30.

## 4. Tests that are not spec samples

Status 2026-10-15: the 30 rows marked **deleted** below were removed from the crate (spec recovery plus a skeptic confirmed they sample no property of shipped code). The `web_binding_parity` rows went with their test file: the six web binding modules and the shared facade are one source each in `crates/aterm-wasm/src`, which aterm-gpu-web mounts, so the two surfaces cannot drift. On 2026-10-17 the twinkle's photosensitivity bound, constant arithmetic, became two const assertions beside its constants, and its row went. On 2026-10-28 `dog_baker::host_ids_are_stable_distinct_and_namespaced` and `dog_cameo::resummons_parade_distinct_breeds` dropped their FNV/statistical halves and joined §2 under `dog-baker-contract` and `dog-cameo-envelope`.

| test | action | why |
|---|---|---|
| `src/cat_baker.rs::bake_is_deterministic_across_bakers` | **deleted** (was delete) | Pure function determinism by construction; band contents are stated in catbaker-atlas-lru. |
| `src/cat_baker.rs::v4_cache_respects_bake_budget` | **deleted** (was delete) | Tests CatBakerV4, which is #[cfg(test)] with no production caller. |
| `src/cat_glyphs_codegen.rs::animal_glyphs_gen_matches_assets` | **move-to-bench** | Drift gate over art/animal (fs read). |
| `src/cat_glyphs_codegen.rs::cat_glyphs_codegen_is_deterministic` | **deleted** (was delete) | Determinism of a pure generator over sorted input, by construction. |
| `src/cat_glyphs_codegen.rs::cat_glyphs_gen_matches_assets` | **move-to-bench** | Drift gate over art/glyphs (fs read). |
| `src/cat_glyphs_codegen.rs::dog_glyphs_gen_matches_assets` | **move-to-bench** | Drift gate over art/dogs (fs read). |
| `src/cat_glyphs_codegen.rs::pet_glyphs_gen_matches_assets` | **move-to-bench** | Drift gate over art/pet (fs read). |
| `src/cursor_glow/tests/fire.rs::fire_field_is_deterministic` | **deleted** (was delete) | Two runs from the same t0 compare equal by construction; it cannot detect absolute-time dependence. |
| `src/cursor_glow/tests/rainbow.rs::rainbow_occupied_coverage_derived_model_proves_and_catches_regression` | **becomes-spec-after-fix** | Test-local ty_model with a hand-copied Cap = 236 and no binding to the ribbon; becomes ribbon-occupied-cov-cap. |
| `src/cursor_glow/tests/rainbow.rs::rainbow_retract_rearm_model_proves_and_catches_brightness_snap` | **becomes-spec-after-fix** | Test-local ty_model whose claimed Tier-1 projection does not exist; becomes ribbon-retract-rearm. |
| `src/cursor_glow/tests/tint.rs::glyph_tint_box_corner_is_stronger_than_its_endpoints` | **deleted** (was delete) | Negative control on the guard's design; samples nothing beyond glyph-tint-palette. |
| `src/cursor_glow/tests/water.rs::water_scales_from_trickle_to_torpedo` | **move-to-bench** | Aesthetic ratio between two scripted cadences. |
| `src/cursor_rainbow.rs::active_rim_reuses_its_warmed_scratch` | **move-to-bench** | Vec capacity property, an allocator-dependent performance budget. |
| `src/cursor_trail.rs::full_u16_jump_reuses_bounded_spawn_scratch` | **move-to-bench** | Allocation-capacity property; the length bound is line-cells-tail-is-bounded-bresenham-suffix. |
| `src/cursor_trail.rs::line_cells_includes_both_endpoints` | **deleted** (was delete) | Tests the #[cfg(test)] line_cells, which becomes the model of line-cells-tail-is-bounded-bresenham-suffix. |
| `src/dog_baker.rs::host_ids_are_stable_distinct_and_namespaced` | **mapped to dog-baker-contract** (was becomes-spec-after-fix) | Stability and the namespace bit are dog-baker-contract; the FNV distinctness point checks were dropped. |
| `src/dog_cameo.rs::resummons_parade_distinct_breeds` | **mapped to dog-cameo-envelope** (was becomes-spec-after-fix) | Its non-statistical half is dog-cameo-envelope; the "256 seeds reach most of the roster" half was dropped. |
| `src/effect_util.rs::the_star_arm_ladder_is_ordered_and_derived` | **deleted** (was delete) | Restates a one-line definition already const-asserted. |
| `src/genome.rs::magic_windows_hit_expected_rates` | **move-to-bench** | 1e6-sample rate test; window widths are definitional; reads #[cfg(test)] cat_accessory. |
| `src/genome.rs::simhash_locality` | **move-to-bench** | Statistical locality check of SimHash. |
| `src/genome.rs::v4_magic_windows_hit_expected_rates` | **move-to-bench** | Statistical rate test; widths are definitional. |
| `src/host.rs::press_outcomes_cross_the_boundary_as_their_u8` | **deleted** (was delete) | Tautology: the #[repr(u8)] discriminants fix the casts. |
| `src/kitty_cursor.rs::on_summon_presents_without_ever_changing_the_look` | **deleted** (was delete) | Tests the #[cfg(test)] CursorCat::on_summon (no production caller); its live content, on_delight never changes look, is reactions-gate-and-preserve-identity. |
| `src/kitty_pet.rs::a_lone_key_on_a_quiet_screen_costs_the_pet_a_counted_census` | **move-to-bench** | A wake-budget census; its correctness half repeats offer-soundness. |
| `src/kitty_sing.rs::note_ring_never_exceeds_the_cap` | **deleted** (was delete) | The bounds are type facts of a fixed array; frames() is #[cfg(test)]. |
| `src/matrix_rain/bake.rs::bake_bytes_are_deterministic` | **deleted** (was delete) | Determinism of a pure integer function; rb-box-filter states the bytes. |
| `src/matrix_rain/field.rs::field_is_deterministic` | **deleted** (was delete) | Compares pure functions with themselves; glyph_at is #[cfg(test)]. |
| `src/motion.rs::effect_set_is_complete` | **deleted** (was delete) | Checks #[cfg(test)] ALL bookkeeping; exhaustive matches already catch it. |
| `src/motion.rs::ignoring_the_system_flag_is_caught` | **deleted** (was delete) | Negative control on a test-local closure; the model's Buggy twin is the real control. |
| `src/motion.rs::serious_effect_set_is_complete_and_ordered` | **deleted** (was delete) | Same, for SeriousEffect. |
| `src/motion.rs::serious_mode_policy_catches_an_ungated_effect` | **deleted** (was delete) | Negative control on a test-local closure. |
| `src/output_streak.rs::identical_scripts_replay_identical_light` | **deleted** (was delete) | Determinism by construction. |
| `src/output_streak.rs::sound_edges_have_one_canonical_output_gesture_mapping` | **deleted** (was delete) | Restates an exhaustive match. |
| `src/pet_world.rs::safe_endpoints_do_not_license_crossing_text` | **deleted** (was delete) | Exercises the #[cfg(test)] corridor_clear. |
| `src/pipeline_input_tests.rs::glow_frame_equality_rejects_equal_count_changed_content` | **deleted** (was delete) | Tests the test harness's own equality. |
| `src/rainbow_kitty/stardust.rs::a_point_query_reads_one_cell_not_the_whole_sky` | **move-to-bench** | A performance ratio through #[cfg(test)] counters. |
| `src/rainbow_kitty/timing.rs::the_two_coverage_ceilings_hold_every_class` | **deleted** (was delete) | Compares literals with constants; the relations are already const assertions. |
| `src/tone.rs::holdout_sanity_set_clears_documented_floors` | **move-to-bench** | Model-quality accuracy gate of the trained weights. |
| `src/trail_sound/rainbow_kitty_v2.rs::a_pause_the_render_clock_never_saw_still_rests_the_phrase` | **deleted** (was delete) | Exercises the #[cfg(test)] resume_after; the shipped half is mb-fallback-clock. |
| `src/trail_sweep.rs::row_sweep_lays_exactly_the_skipped_cells` | **deleted** (was delete) | Tests the #[cfg(test)] row_sweep_cells, which no production code calls. |
| `tests/console_pet_escort_lead.rs::escort_feel_trace` | **deleted** (was delete) | An #[ignore]d diagnostic trace. |
| `tests/nova_channel_budget.rs::four_panes_of_build_output_push_the_channel_past_the_backstop` | **deleted** (was delete) | Pins the absence of a cap; a premise, not a property. |
| `tests/nova_channel_budget.rs::the_resident_scratch_grows_once_not_every_frame` | **deleted** (was delete) | Exercises a test-local replica of the host scratch. |
| `tests/ribbon_arrival_conformance.rs::the_2026_09_23_writer_is_caught_on_the_copies_that_stood` | **delete** | Becomes the mutation control of witness-arrival-evidence-refines-model. |
| `tests/torn_repaint_hidden_caret.rs::a_raw_echo_anchor_spends_a_torn_space_at_the_line_end` | **delete** | Becomes the mutation control of motion-caret-contract (the echo anchor handed over raw). |
| `web_binding_parity::effects_api_requires_matrix_rain_surface` | **deleted** (was becomes-spec-after-fix) | A symbol floor on another crate's text; single-sourcing makes it a compile error. |
| `web_binding_parity::effects_api_requires_output_streak_surface` | **deleted** (was becomes-spec-after-fix) | Same as effects_api_requires_matrix_rain_surface. |
| `web_binding_parity::every_cpu_only_web_binding_module_is_declared` | **deleted** (was delete) | Meta-test of a registry policed by a test. |
| `web_binding_parity::every_duplicated_web_binding_module_is_covered` | **deleted** (was delete) | Meta-test of the guard's own coverage set. |
| `web_binding_parity::messages_api_requires_band_surface` | **deleted** (was becomes-spec-after-fix) | Same as effects_api_requires_matrix_rain_surface. |
| `web_binding_parity::normalizer_does_not_mask_code_drift` | **deleted** (was delete) | Self-tests the test-local normalizer. |
| `web_binding_parity::production_binding_surface_is_identical` | **deleted** (was becomes-spec-after-fix) | Single-source the shared wasm-bindgen modules so parity holds by construction; until then an aterm-census guard. |
| `web_binding_parity::wasm_lib_facade_surface_is_mirrored_by_gpu_web` | **deleted** (was becomes-spec-after-fix) | A shared trait both facades implement makes the mirror type-checked. |
| `tests/wrap_code_reflow.rs::diagnostic_sweep` | **deleted** (was delete) | An #[ignore]d census that prints. |

## 5. Implied specs that look wrong, or disagree with the code

1. Resolved: `CatBaker::slot_count` returns `0` for `cell_h == 0` and widens `32 * ch^2` in `u64` without flooring to `1`, so `slot_count(ch) * (4ch)(2ch)*4 <= MAX_ATLAS_BYTES` holds for all `ch: u16` (sampled through `256`, `257`, and `u16::MAX` in `slot_count_is_structural`).
2. Resolved: `ClassicWake::fingerprint` remaps `0 -> 1` whenever `out.laid() > 0 || under.laid() > 0 || !halos.is_empty()` (`fingerprint_remaps_zero_fold_on_additive_quads_to_one`), so `fp == 0` iff every stream is empty.
3. Resolved: `cursor_beam.rs::rod_lands_at_the_window_origin` tests both a caret below row 0 (`y >= origin_y`) and row 0 with `head > 0` (`y >= origin_y.saturating_sub(head)`, with the bar rod overshooting into the head band).
4. Resolved: `RainbowOccupiedCoverage` and `RainbowRetractRearm` live in `aterm-models-effects` (`models_rainbow_ribbon_laws.rs`); `rainbow.rs` checks `Cap == ribbon::UNDER_COV_CAP`, binds `RainbowOccupiedCoverage` to `ribbon::clamp_bed_cov`, and binds `RainbowRetractRearm` to a real `Ribbon`'s cell envelope across fade, reversal, and re-arm.
5. `fire.rs::halo_changes_change_the_fingerprint` and `frame_fingerprint_preserves_private_stream_identity_and_idle_zero`: "different stream => different fp" is a point fact about one FNV input pair, not a universal property; only the idle sentinel generalises.
6. `goldens.rs` (all three): goldens are re-pinned whenever bytes move (the documented re-bake law), so they guard drift, not a specification.
7. Resolved: `PressCredits::foreign_to_live` scans all payable slots before returning `Some(false)`, so any glyph-less or multi-cell live press yields `None` independent of physical slot order (`the_glyph_gate_refuses_only_a_known_foreign_glyph`).
8. Resolved: `witness.rs::swept_path_retains_only_the_bounded_landing_suffix` asserts only the bounded landing suffix values (`len`, `first`, `last`), without depending on `Vec` growth capacity.
9. Resolved: `cursor_momentum.rs::cooldown_is_monotone_and_snaps_to_exact_zero` now checks the analytic crossing against `MomentumGlow::cold_at`, the production instant the settle schedule wakes at; the #[cfg(test)] `TypingRate::cold_at` is gone.
10. `genome.rs::simhash_bitsliced_equals_reference_over_voter_sets`: the live kernel is Reference, so the bitsliced functions have no production caller unless SIMHASH_KERNEL flips, and simhash_majority_reference is #[cfg(test)]. The equivalence is still the gate for that switch.
11. Resolved: `review.rs::a_press_the_tty_will_not_echo_banks_nothing_for_a_same_row_program_hop` no longer computes an unused per-key clock around `note_typed_swallowed_no_echo()`.
12. Resolved: `dog_baker.rs::every_breed_draws_a_dog` iterates `for coat in 0..16u8`, covering all 16 coat stops.
13. Resolved: `kitty_cursor.rs`'s `typed_word_reactions_are_bounded_expression` and `delight_is_expression_not_displacement` drive the cat through `on_collect` + `on_delight`, and `CursorCat::on_summon` is gone.
14. Resolved: `kitty_cursor.rs::delight_is_expression_not_displacement` feeds `8` keys so stride is live (`control.bob.abs() > 0.0`) before comparing reaction expression and displacement.
15. Resolved: `kitty_cursor.rs::mid_flight_look_sync_is_latched_until_the_next_wake` asserts `c.is_active()` unconditionally after mid-`FadeOut` typing before checking the revived frame.
16. `kitty_cursor.rs`'s const assert against `V056_MIN_RUN_KEYS` pins a historical constant (v0.57's raise of the travel floor from 10 to 16 keys); that is release history, not a property. `sixteen_high_band_keys_are_required_even_after_full_dwell` no longer restates it at run time, and the floor stays as that compile-time assertion.
17. Resolved: `KittyLook::for_launch`'s 32-of-64 distinctness and not-the-default checks moved from `kitty_cursor.rs` into `kitty_registry.rs::launch_kitty_is_seed_pure_normalized_head_and_bare`; the distinctness is still a point sample.
18. Resolved: `EXIT_STAR_SCINT`'s positivity and frequency bounds are compile-time `const` assertions beside it (kitty_cursor.rs), and the runtime test that restated them is gone.
19. Resolved: stale `0.75` band reference in `typing_momentum.rs` removed (`CAT_BAND` is `0.65`).
20. `static_frame`'s retirement to Hidden applies pending_look at once (`reduced_motion_flight_retires_and_releases_the_look_latch`), while `frame`'s FadeOut -> Hidden keeps it parked until the next summon. Both satisfy look-latch-per-appearance but leave `CursorCat.look` different; confirm it is intended.
21. `kitty_pet.rs`: the drift-brake run-out is unreachable from production input. Either the arm is dead code or its gate needs a trigger; no skid spec can be stated until that is decided.
22. Resolved: stale non-monotonicity comment on `kitty_pet.rs::no_bound_crosses_more_than_half_the_animal_in_a_frame` removed.
23. Resolved: `kitty_pet.rs::the_gallop_cycle_stays_below_nyquist_and_paces_by_stride_cells` names both properties it checks (`ds <= 0.5` cycles per 16 ms frame and `(eff - RUN_STRIDE_CELLS).abs() < 0.05`).
24. Resolved: `kitty_pet.rs::a_confirmation_after_the_cat_answered_replays_nothing` now asserts `pending_trick() == None` and `!needs_frames()` directly without the tautological disjunction.
25. Resolved: `DOG_SKIN` carries a compile-time `const` assertion (`kitty_pet.rs`) verifying that every shipped cat pose has a distinct `pet_dog_` counterpart without hitting the fallback.
26. `console_life.rs::hidden_cursor_home_continuity_conforms_to_real_terminal_motion` checks conformance one step from the model's init only, never along multi-step traces.
27. `kitty_pet.rs`: many tests assert exact counts over seeds 0..12 (`long == 8`, `glanced == 8`, `blinked == 4`), pinning the sample rather than the per-seed law over all 256 seeds; `force_seed` is a #[cfg(test)] seam.
28. Resolved: `kitty_sing::bar_fan` computes `(bar & (BAR_RING_EVERY - 1)) == BAR_RING_EVERY - 1` (power-of-two `const` asserted), avoiding overflow at `bar == u64::MAX`.
29. Resolved: `matrix_rain::field::{bits, ticks_of, quanta, col_params}` are now total over all `u32` inputs without shift or product overflow.
30. Resolved: `matrix_rain::fingerprint` mixes all three words of `RainHalo::damage_words` (`cx`, `cy`, `rx`, `ry`, `mode` covered by `halo_fingerprint_tracks_center_radii_and_mode`), and `fingerprint_liveness_and_stability` drops the unprovable `fp != 0` assertion on arbitrary FNV chains.
31. Resolved: `nova::crossing_ms` clamps its returned crossing instant to `RING_END_MS - 1`, staying strictly inside `FLASH_END_MS..RING_END_MS` where `ring_radius` returns `Some`.
32. Resolved: `nuke::nuke_nat_size` clamps with `4.0_f32.min(max_w)` / `4.0_f32.min(max_h)`, tested down to `cell_h = 0, 1`.
33. `pet_baker.rs::host_ids_are_stable_and_distinct`: distinct keys can collide in host_tile_id when pre-OR hashes differ only in bit 63, so distinctness is not provable.
34. `pipeline.rs::a_typed_trick_latches_through_the_pipeline_and_needs_ticks` drives #[cfg(test)] pipeline wrappers (`note_trick`, `revoke_trick`, `note_trick_submit`) that no page path ships.
35. Resolved: `rainbow_kitty::companion::placement` and `Seat::over_ink` document the fallback when every yield is inked or covers the caret cell on a crowded/narrow line.
36. Resolved: `ribbon.rs::a_line_still_being_typed_keeps_every_cell_from_its_first_key_to_the_caret` samples `last + (PHRASE_REST_MIN_S * 1000.0) as u64` and no longer carries the stale `0.75 s` comment.
37. Resolved: `ribbon.rs::a_fades_next_level_is_solved_not_polled` computes `t = (step as f64 * 1e-5) as f32` from an integer `step` counter rather than accumulating `t += 1e-5` in `f32`.
38. Resolved: vacuous clause removed from `mod.rs::flow_draws_nothing_of_its_own`.
39. Resolved: `mod.rs::a_gold_star_near_a_contented_cat_is_offered_and_caught` resets the scratch frame before checking the post-catch emission and asserts `!eng.catch_star` alongside `still.finish` on the second tick.
40. `supernova.rs::combo_ladder_climbs_to_guaranteed_nuke` measures statistically, level by level, what the window table gives exactly.
41. `supernova.rs::detonation_has_a_hard_aggregate_readability_ceiling` bundles a contrast check of `aterm_render::Theme::default()` and lower "stays vivid" pins with the real ceiling law.
42. `stardust.rs::the_exhaust_costs_the_frame_train_its_own_window_and_no_more` and `the_exhaust_never_chimes...` mix calibration ratios and a direct check of a const match arm with the real predicate.
43. `stardust.rs::a_field_star_rides_its_cell_s_envelope_not_the_grain_s_life` pins the literal horizon 1690 ms and grain life 225 ms inside a behavioural test.
44. Resolved: `spectrum.rs::spectrum_lut_has_bounded_adjacent_steps` checks the shipped `SPECTRUM_LUT` directly against `CEILING = 5` at compile time and `d <= worst` across `spectrum(t)` without pinning `worst == 4` or measuring the generator.
45. `tone_infer.rs`: five tests assert concrete classifier verdicts on fixed text, pinning the trained weights rather than the tracker.
46. `spec.rs::every_checked_in_community_toy_pack_compiles` enumerates the repository directory at test time; as a spec it should range over include_str! constants.
47. Resolved: `trail_sweep.rs`'s #[cfg(test)] `row_sweep_cells`, `wrap_fold_cells` and `wrap_fold_cells_in_pane` are gone with the half of `wrap_fold_finishes_old_row_and_starts_new` that tested them; its physical_margin_fold half, the shipped code, is `a_physical_margin_fold_runs_from_the_panes_last_column_to_its_first`.
48. Resolved: no host-facing doc calls the `#[cfg(test)]` `TrailSynth::resume_after`; its doc explicitly marks it as the unit-test clock seam while hosts use the render-clock gap path (`mb-fallback-clock`).
49. Seed-sampled loudness windows (ladder, ting, space head, capital weight, flow echo) are checked on 1-5 seeds while their docs call a single crest a lottery; restate as seed means or phase-free energy before proving the for-all-seeds forms.
50. Corpus statistics in mb-melodic-bounds tests are fixed-corpus facts.
51. `trail_sound.rs::every_pitched_voice_lands_on_the_melody_note` checks only the standalone sound-only pitched instruments (`Marimba` and `Felt`); resolved for `the_glyph_rank_is_ordered_total_and_lossy`, which now asserts `buckets == (37..=44)` directly across all eight punctuation ranks.
52. Resolved: `typing_momentum.rs::accrual_is_rate_capped_so_key_repeat_cannot_outrun_typing` checks `TypingMomentum::advance` against the exact geometric-series closed form, continuous-time floor, and `TYPING_MOMENTUM_CREDIT_CAP_S` ceiling with `flood <= sprint`.
53. `word_decorations.rs::scan_memo_spreads_rows_precomputed_to_collide` and `done_marks_spread_keys_precomputed_to_collide` are randomized with a claimed failure probability; not deterministic specs.
54. `word_decorations.rs::allocation_regression_all_miss_repaint_reuses_retired_buffers`'s `fresh < 260*rows/10` is a ratio bound, not behaviour.
55. `word_decorations.rs::coupling_pulse_is_deterministic_and_luminance_bounded`'s `pulsed_frames <= 32` depends on the 10 ms sampling step.
56. `composer_box_growth_wrap.rs`'s two hand-return tests use ratios to a control with no derivation; the exact law is "flow and seam cleared".
57. `composer_multiline_wrap.rs::the_limit_a_line_identical_to_the_one_above_it_or_one_column_off_melts_instead_of_following` pins a known limitation as expected behaviour.
58. `abandoned_ribbon.rs::a_wrapped_line_s_first_row_goes_out_on_its_own_clock...` also asserts a constants-only bound; `busy_status_rows_keep_their_brand.rs`'s default run samples 6 of 20 replays chosen by a historical red count.
59. `word_decorations.rs::one_keystroke_re_arms_one_cat_however_many_repaints_follow` depends on a timing premise against TYPED_EDIT_WITNESS_MS; `fix_never_triggers_or_inherits_profanity_rainbow` mixes classification, eviction and fingerprint claims.
60. `tests/copies_are_not_moves.rs` and `moves_are_followed.rs`: the `the_limit_*` tests pin known-wrong outcomes as expected (a real move melts, a real copy lights), and `trail_host::holds_but` is a registry policed by a test, which AGENTS.md rules out; the spec is twin-evidence-timing's rule.
61. Resolved: stale defect doc removed from `tests/wrap_code_reflow.rs::a_delayed_first_key_still_carries_the_glued_word_down`.
62. `tests/console_pet_escort_lead.rs::the_escort_keeps_off_the_words_it_leads` mixes invariants with quality ratios (cover percentage, turns per second).
63. Resolved: `tests/nova_channel_budget.rs::the_over_budget_channel_is_drawn_in_full_not_truncated` embeds the checked-in `JetBrainsMono-Regular.ttf` via `include_bytes!`, so it never skips or passes vacuously on a headless host without system monospace fonts (its render-budget assertion deliberately exercises `aterm-render` with `aterm-effects`'s `MAX_NOVA_QUADS`).
64. Resolved: `tests/cursor_bench.rs::cursor_rainbow_hot_ribbon_work_stays_below_its_normalized_share` reads `aterm_effects::rainbow_kitty::ribbon::RIBBON_QUAD_BUDGET` and `CursorGlow::MAX_QUADS` directly instead of restating literals.
65. `tests/licensed_typed_parity.rs`'s golden fold pins numbers and prints instead of asserting under `ATERM_CAPTURE_TYPED_PARITY=1`; `cursor_cat_conformance.rs` and `kitty_activation_conformance.rs` run `ty trace validate` only when installed.
66. `tests/pet_art_quality.rs` reads art/pet/*.toml rather than the compiled table; `tests/serde_corpus.rs` compares against the retired dependency's recorded answers.
67. `tests/shed_frame_is_still_a_heard_key.rs::a_shed_tick_keeps_the_keyed_click_ledger` overclaims: only the shed == lit equality is meaningful. Many host-seam files record "RED on 0.93.0 N of M" history in doc comments.
68. `zsh_isearch_band.rs::the_gates_refuse_nothing_in_claude_code_s_or_codex_s_composer`: beyond the gates' positive halves it asserts facts about 16 recordings of external programs, which no proof of CursorGlow carries; the residue belongs in a corpus lane.
69. `zsh_isearch_band.rs`'s torn-read sweeps' `tried > 100` and `refused > 0` checks are fixture non-vacuity, not code properties.
70. `zsh_isearch_band.rs::a_hidden_caret_s_echo_is_judged_alike_on_every_path`: the equivalence holds only because the arm checks caret_hidden, and seam.rs documents that a visible parked caret with a fake cursor goes dark by design; the spec keeps that exclusion.
71. `zsh_isearch_band.rs::whole_reads_lay_no_band_on_the_match_row_or_under_zle_s_fake_cursor`'s oracle accepts any status cell whose glyph is among the typed chars, weaker than cell identity.

## 6. Proof order (easiest high-value first)

Rule: prefer specs blocked only by gap 0, then those that add only gap 3 (straight-line scalar
or float bodies) or gap 1 (a small carrier), then the table properties decidable by evaluation,
then the small state machines, and the large engines last.

1. **Gap 0 only** (provable as soon as the seal publishes Clean rows):
   `momentum-glow-allowed` (`u && h && !r`), `mo-load-shed-exempt` (a two-variant enum match),
   `mo-serious` (`!serious` for every effect), `rkc-heroes` (`Delight → 1`). Each retires its one
   test and is a seal smoke test with an obvious negative control.
2. **Pure scalar and small-carrier laws** (gaps 0 plus 1 or 3): `companion-gates` (4 tests,
   seven bool/f32 predicates, including `pet_output_burst` and `sing_face_live`),
   `caret-style-law`, `companion-precedence`, `shed-companion-alpha`, `head-dx`, `catbaker-slot-count`
   (with the `cell_h <= 256` precondition of §5), `evict-step`, `os-clipped-span`, `pl-chrome-geom`,
   `wd-cat-stacked-on`, `wd-peek-timing`, `kr-canonical-app-id`, `mo-parse`, `ksum-merge`,
   `config-from-glow`, `engine-fresh-state`, the two pack source caps, and the matrix-rain field
   kernels `rf-hash`, `rf-bits`, `rf-stammer`, `rf-trail-level`, `rf-quad-cap`, `rf-quanta`
   (`rf-col-params` next: 4 tests and the WCAG flash floor).
3. **Small contracts with a real payoff**: `caret-delta-fold` (3 tests; the pet's wrap/new-line
   classifier, the cheapest behavioural kernel in kitty_pet), `station-wall`, `quiet-pace-law`,
   `flight-pose-schedule`, `pet-geometry-maps`, `wrap-fact-edge`, `erase-poof-drive`,
   `classic-ring-grade`, `fold-register`, `tt-back-chars-arith`, `mix-nonzero`,
   `ks-song-signature-bijective` (a bijection on u32, decidable by bit-vector),
   `walk-t-def` and `tri-triangle-wave` (the ribbon's colour walk; they unlock the bit-for-bit
   landing specs), `spark-cull-exact`, `bridge-reach-contract`, `sweep-physical-margin-fold`.
4. **Table properties decidable by evaluating constant data**: `outline-contrast` (5 tests),
   `color-key-quantize`, `kr-coat-distance`, `dog-skin-table` (best as a `const _: () = assert!`),
   `spectrum-lut-table-properties` (6 tests) and `spectrum-lut-is-generated`, `rail-ink-band`,
   `bed-luma-budget-def`, `glyph-rank-table`, `glyph-class-table`, `theme-form-tables`,
   `song-pulse-table`, `rom-classic-table`, `bf-table-lookup`, `seat-deal-arithmetic` (all 256 seeds).
5. **Small state machines** (gaps 1 and 2): `tenure-gate` and `roster-rate-law` (companion_look),
   `claim-cache`, `exact-tile-baker-lru` (10 tests across the animal, pet and robi bakers, and a generic over
   `ExactTileKey`, so it also needs gap 11), `catbaker-atlas-lru`, `typing-rate-value`,
   `momentum-read-and-decay` / `momentum-advance-credit-law` / `momentum-delete-kill-drain`,
   `ks-arm-threshold` (10 tests), `tt-*` (the typed-trick listener: 17 specs; `tt-fire-licence-safety`
   is the master safety law), `ps-stroke-contract`, `wd-ignition-limiter` (the WCAG 2.3.1 limiter),
   `wd-done-mark-lru`, `spine-*` (the rainbow kitty spine, ten small specs).
6. **The model refinements** wait on gap 10; each already has a model with a negative control:
   `glow-licence-refines-model`, `echo-ledger-refinement` (16 tests), `witness-follow-refines-twin-model`,
   `witness-arrival-evidence-refines-model`, `mr-*-refinement`, `output-streak-attribution` (17 tests),
   `wd-identity-model`, `wd-alignment-cardinality` (13 tests), `console-life-episode-refines-model`,
   `pet-surface-seam-refines-model` (16 tests).
7. **The big engines last**: the CursorGlow seam (`glow-no-licence-no-light`, `glow-exact-payment`,
   `glow-park`, `insert-licence`, …), the ribbon (`cohort-walk-invariant` first, since
   `landing-walk-is-band-field` and the walk specs are its corollaries), the PetBrain trace specs
   (`no-teleport-law` and `pet-idle-to-zero` first), the synth, and the host-seam laws
   (`copy-is-not-arrival`, `move-is-followed`, `typed-columns-owned`), which need nearly every gap
   at once. Factoring three pure functions out of `PetBrain::tick` (the reduced-motion arm, the
   settle ladder, `settle_gaze`) would move `reduced-motion-static`, `settle-ladder`,
   `idle-frame-laws` and `sleeper-gaze-brakes` into step 3. The bounded host-seam sweeps can hold at
   Trusted grade under a bounded model checker over the compiled code before any Clean proof exists.

## 7. Added 2026-10-23: the distributed search (aos M5b)

`aos_momentum::dist` (the wire, the claim check and the coordinator core) and `m4::Distributed`
(the fold over claims) are new since the recovery, and their tests are read the same way as §1.
Design of record: `docs/DESIGN-aos-hosted-execution-2026-10-21.md` §11. The ledger
(`aos_momentum::lease`) has its call table here as `aos-lease-calls` and its refinement of the
model as `aos-lease-refines-model` (§1).
*Local* means judged by this process's `Evaluator`; a *claim* is a worker's reply, which is never
evidence.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
aos-dist-wire	contract	dist::{Msg::encode,Msg::decode,ExecIdentity::{encode,decode,digest,first_difference},RawClaim::{encode,decode,shaped},Claim::shaped,job_id,claim_digest}	∀ m: Msg with sender s: Msg::decode(m.encode(), s) = Ok(m); ExecIdentity and RawClaim round-trip the same way. Msg::decode(b, s) is Ok(m) only when m.encode() = b; otherwise it is Err by a named FrameFault (Magic, Format, Tag, Direction, Wire, NonCanonical, Kind) and it never panics, a row count over MAX_ROWS included. job_id(exec, kind, plan) = Digest::of(JOB_DOMAIN, [exec, kind, plan.encode()]); claim_digest(job, c) = Digest::of(CLAIM_DOMAIN, [job, c.encode()]), so it covers the job, the kind, the count and every row field, and not a Result's declared stepped and judge_ns. first_difference(a, b) = None ⇔ a.digest() = b.digest(), else it names the first differing field. Claim::shaped refuses, by ShapeFault variant, a kind that disagrees with its last row, a count over the items or not ending at the first refusal, and a row whose item pin is not the split's.	0,1,3,5,7	2
aos-dist-outcome-local	contract	dist::{Coordinator::{new,join,begin,on,tick,void,take_claims},run_job,Claim::{totals,ticks,check}}; m4::{Evaluator::distributed,Distributed::{jobs,claim,finish,finish_noting}}	No claimed row reaches a Selection or a bundle: a claim is read only through kind, len, totals, ticks and check, and every row a Selection carries is judged locally. Distributed::finish(spread), once every job holds one claim: (1) every refused claim is re-judged locally on its prefix and the claimed best in full, and under CheckLevel::All every claim in full; any disagreement is DistNoOutcome::Contradicted { slot, item, field } before any held-out read, and a job with no claim or a second claim is Unclaimed or Misplaced; (2) under Dominance the other admitted claims are judged in descending claimed T until the local partial ΣT exceeds the winner's, and the rest are dominated; (3) round-0 feedback is built in proposal order from those results and rounds 1.. are asked with it, any proposal giving Adaptive with no held-out read; (4) only then NoneAdmitted or NoGain, and the held-out split is read at most once, never after a void. The mode is Distributed exactly under Dominance (stats.admitted may rest on dominated claims) and Search under All. The coordinator voids a search on any lie it can see (a differing resend or stale result: Differs; a broken stream: Frame, its sender killed; a shape, an unasked or unheld result, no progress), after which it issues no lease; a transport fault from a worker already gone is dropped unread. run_job reads no held-out item and stops at its flag.	0,1,2,3,4,5,7,13	19
aos-lease-calls	contract	lease::Ledger::{join,hello,begin,assign,lease,progress,result,cancelled,lost,tick,void,finish,outcome,counts,next_deadline}	The call table of design §11.5 over Reach(Ledger) with a non-decreasing clock: an earlier now is ClockBackwards with the state unchanged; a join past max_workers is refused with no worker made; begin and assign lease the lowest Pending jobs to Idle workers, one lease per worker; a lease lives until its deadline and is released at it (Busy → Draining Expired, Cancel sent); progress must strictly increase its k (any k < 17 first, None at lease), so a lease outlives at most 18 windows and a repeat or a k ≥ 17 is NoProgress; a draining worker is held to the same k rule and one answered result; a Cancelled answer retires its worker; the first Result for a held lease claims its job and an equal resend or stale result is counted, a differing one is Differs; a frame for a token the worker does not hold is NotHeld, an unasked one Unasked, a frame before hello or a second hello voids, a foreign hello refuses that worker alone; EOF, hello timeout or a drain past grace makes the worker Gone and returns its job to Pending, the third lost lease Exhausts it, and no live worker left with a job open is NoWorkers; the sender of every voiding frame is killed and never drained, and a void cancels every other holder; every move to Gone emits one Kill; frames from a Gone worker are dropped; finish completes only a search whose every job is Claimed with no worker Draining.	0,1,2,3,5,7	35
aos-dist-equals-local	equivalence	m4::{Evaluator::{select,select_spread,distributed},Distributed::finish}; dist::Coordinator::on; Spread	For honest claims (each equal to run_job of its plan on this evaluator) in any completion order and over any spread, Distributed::finish gives the Selection that Evaluator::select gives for the same proposer, mode and timings aside; select_spread equals select for every spread that runs each index once, and finish equals serial for a spread that skips jobs (the rest run in place), at both check levels; completion order never reaches the proposer, which sees feedback in proposal order.	0,1,2,3,5,7,11,13	5
```

```tsv
file	test	primary	also
src/aos_momentum/lease_tests.rs	a_different_late_result_voids	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_different_resend_voids	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_drain_past_grace_is_lost_and_killed	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_draining_worker_cannot_flood	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_foreign_hello_refuses_that_worker_alone	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_frame_before_hello_and_a_second_hello_void	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_lease_cannot_outlive_eighteen_windows	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_lease_lives_until_its_deadline_and_not_at_it	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_replayed_token_is_not_held	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_resend_after_its_search_ended_is_a_duplicate	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_result_crossing_a_cancel_then_its_ack_retires	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_search_completes_and_the_pool_serves_the_next	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	a_second_resend_is_unasked	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	an_ack_retires_the_worker	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	an_equal_late_result_is_counted_stale	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	an_equal_resend_changes_nothing	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	an_expired_lease_is_released_at_once	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	an_unasked_cancelled_voids	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	another_workers_token_is_not_held	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	begin_leases_the_lowest_jobs_to_idle_workers	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	cancel_all_drains_every_holder	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	eof_mid_job_reopens_it_and_keeps_the_workers_claims	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	finish_completes_only_a_claimed_quiet_search	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	frames_from_a_gone_worker_are_dropped	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	hello_timeout_is_lost	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	limits_v1_are_the_designs	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	no_more_than_max_workers_join	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	no_workers_left_voids	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	progress_after_expiry_is_dropped	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	progress_for_a_claimed_lease_is_unasked	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	progress_that_does_not_advance_voids	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	step_five_waits_for_every_drain	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	the_sender_of_a_voiding_frame_is_killed_not_drained	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	the_third_lost_lease_exhausts_the_job_and_voids	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/lease_tests.rs	time_never_runs_backwards	aos-lease-calls	aos-lease-refines-model
src/aos_momentum/dist_tests.rs	every_message_round_trips_and_every_fault_is_named	aos-dist-wire	
src/aos_momentum/dist_tests.rs	ids_and_digests_name_what_they_cover	aos-dist-wire	
src/aos_momentum/dist_tests.rs	a_refusal_claimed_for_an_admitted_plan_is_contradicted	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	a_forged_best_is_contradicted_before_any_held_out_read	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	an_inflated_winner_is_found_by_the_dominance_check	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	a_dominated_claim_stops_at_the_winners_sum	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	check_all_judges_every_admitted_claim_and_records_search	aos-dist-outcome-local	aos-dist-equals-local
src/aos_momentum/dist_tests.rs	claims_never_fill_the_grid_cache	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	a_distributed_bundle_is_checked_and_declared	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	a_proposal_after_round_zero_is_adaptive	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	claims_must_fill_every_job_once	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	every_refusal_held_is_none_admitted	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	each_shape_fault_is_refused_by_name	aos-dist-outcome-local	aos-dist-wire
src/aos_momentum/dist_tests.rs	run_job_reads_no_held_out_item_and_stops_when_told	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	lies_are_contradicted_or_refused_by_shape	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	garbage_another_job_id_and_a_foreign_hello	aos-dist-outcome-local	aos-dist-wire
src/aos_momentum/dist_tests.rs	resends_and_frames_from_the_gone	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	the_held_out_split_is_read_once_under_every_fault	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	a_broken_stream_voids_and_kills_its_sender	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	a_broken_tail_from_a_gone_worker_is_dropped	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	a_claims_rows_stay_private_to_dist	aos-dist-outcome-local	
src/aos_momentum/dist_tests.rs	honest_claims_in_any_completion_order_give_the_local_selection	aos-dist-equals-local	
src/aos_momentum/dist_tests.rs	select_spread_equals_select	aos-dist-equals-local	
src/aos_momentum/dist_tests.rs	finish_with_a_skipping_spread_equals_serial	aos-dist-equals-local	
src/aos_momentum/dist_tests.rs	completion_order_never_reaches_the_proposer	aos-dist-equals-local	
src/aos_momentum/dist_tests.rs	an_honest_pool_gives_the_local_selection	aos-dist-equals-local	aos-dist-outcome-local
```

## 8. Added 2026-10-24: shared-buffer inference (aos M5a)

`int_bag_mlp` (the `int32-exact/v1` job kind: its layout, params builder and checker, packer and
CPU reference) and `tone_int` (tone-int v1, derived from the shipped tone asset) are new since the
recovery, and their tests are read the same way as §1. Design of record:
`docs/DESIGN-aos-hosted-execution-2026-10-21.md` §12. Both modules are gated `any(test,
debug_assertions, feature = "int-bag-mlp")`, so no release binary compiles them. aterm-gpu's Metal job
reads the same bytes; its rows (`int-bag-mlp-equals-cpu-reference` and the job typestate's) belong to
aterm-gpu's SPECS (design §12.7) and cite `int-bag-mlp-forward-bounds`, so no row here waits on gap 8.
*Accepted* means `check` or `Batch::pack` returned Ok.

7 specs, 50 primary tests. Gap histogram: 0 → 7, 1 → 7, 2 → 1, 3 → 5, 5 → 5, 6 → 2, 7 → 6, 11 → 1, 12 → 3, 13 → 2.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
int-bag-mlp-layout	table-property	int_bag_mlp::{LAYOUT,JOB_FIELDS,PARAMS_FIELDS,Layout::fields,job,params,Batch::header,build,out_digest}	JOB_FIELDS and PARAMS_FIELDS are the one source of every header index: each entry is (its prefix and the index constant's own name, that constant), made by one macro list, so a name cannot drift from its index; in each table the indices are 0..n in word order (JOB_WORDS = 5, PARAMS_WORDS = 23), and the 28 names are unique across both tables, as one kernel prelude needs. Batch::header writes JOB_MAGIC_VALUE, LAYOUT_VERSION, ROWS, TOTAL_IDS and C + 1 at job's indices; build writes every params word at params's indices, little-endian, then each tensor at the next 16-byte-aligned offset in Tensor::ALL order, each *_OFF a byte offset and each *_LEN an element count. out holds per row L[0..C] as i32 bits then the class, and out_digest(w) = Digest::of(OUT_DOMAIN, [w little-endian]).	0,1,5,12	4
int-bag-mlp-params-checker	contract	int_bag_mlp::{check,build,qmax,ParamsView::{dims,min_ids,mult,qmax,g,t,m,max_abs_b1,header,emb,w1,w2,lut,b1,b2}}	check(bytes) is Ok only when all of the following hold, and otherwise returns the first that fails, in this order, by its ParamsError, never panicking: len ≤ PARAMS_BYTES_MAX (TooLarge); a whole header (Truncated); PARAMS_MAGIC_VALUE and LAYOUT_VERSION (BadMagic, BadVersion); 1 ≤ B ≤ 65,536, 1 ≤ E ≤ 32, 1 ≤ H ≤ 128, 1 ≤ C ≤ 16 (DimOutOfBounds, by Dim); MIN_IDS ≥ 1 (MinIdsZero); 1 ≤ G ≤ 31 (ShiftOutOfRange); T ≥ 0 (NegativeT); 1 ≤ MULT < 2³¹ (MultOutOfRange); the stored QMAX = ⌊(2³¹ − 1)/MULT⌋, recomputed (QmaxMismatch); per tensor in blob order, its length is its dimensions' product, B·E, E·H, H·C, 2T + 1, H or C (BadLength), it ends inside the blob (OutsideBlob) and its offset is 16-byte aligned (Unaligned); no tensor overlaps the header or another (Overlap) and no byte follows the last (Trailing); no element of emb, w1, w2 or lut is −128 (Int8Min, its first index); M − max|b1| ≥ T with M = ⌊QMAX·MULT/2^G⌋, computed in i64 (Lossy); every |b2[c]| < 2²⁹ (B2OutOfRange). check(build(s)) reads back s's dimensions, constants and tensors.	0,1,3,5,7	19
int-bag-mlp-batch-refusals	contract	int_bag_mlp::{Batch::{pack,dims,rows,total_ids,offsets,ids,row,out_stride,out_words,bytes,into_parts},JobBytes::{of,total}}	Batch::pack(p, offsets, ids) is Ok only when ROWS = len(offsets) − 1 ≥ 1 (Empty), ROWS ≤ 2²⁰ (TooManyRows), JobBytes::of(ROWS, len(ids), C).total() ≤ JOB_BYTES_MAX (TooLarge), offsets[0] = 0 and offsets[ROWS] = len(ids) (BadTotal), offsets never decrease (NotMonotone, the first row), no row holds more than MAX_ROW_IDS ids (RowTooLong) and every id is below B (IdOutOfRange, the first index), checked in that order, never panicking and making no buffer; a row of zero ids is valid. JobBytes::of counts the job header at 20 bytes, offsets at 4(ROWS + 1), ids at 2·TOTAL_IDS rounded up to 4 and out at 4·ROWS·(C + 1), each at least 4. An accepted batch keeps p's Dims, row(r) is None past the last row, and into_parts gives its vectors back with their capacity.	0,1,3,5,7	9
int-bag-mlp-forward-bounds	contract	int_bag_mlp::{forward,forward_fast,forward_fast_rows,trace,run,pre_activations,finish,Arith::*,seeded,interior_params_for,interior_rows}	For every view check accepts and every batch Batch::pack accepts against it, forward never panics, so no checked i32 add, mul, neg or abs and no u32 div, mul (kept ≤ 2³¹ − 1) or shr overflows, and forward_fast and forward_fast_rows (whose row ranges compose) write the same words. Each row is design §12.2's steps 1–8: fewer than MIN_IDS ids abstains (class C, logits 0); S = Σ emb[x·E + j], A = Σ S·w1, q = sgn(A)(|A|/n), p = sgn(q)((min(|q|, QMAX)·MULT) ≫ G) + b1, h = lut[clamp(p, −T, T) + T], L = b2 + Σ h·w2, and the class is the lowest c of the largest L. The bounds that make it so: 127²·32·4,096 < 2³¹, QMAX·MULT < 2³¹, |p| ≤ M + max|b1| ≤ 2M − T and 127²·128 + 2²⁹ < 2³¹. forward refuses a batch of other Dims (DimsMismatch), rows outside it (RowsOutOfRange) and an out of the wrong length (OutLength) before computing; trace gives each p before step 6, how many units steps 5 and 6 clamp and how many are interior (0 < q ≤ QMAX and |p| ≤ T). The 64 seeded blobs draw QMAX log-uniformly (MULT = ⌊(2³¹ − 1)/2^r⌋, r in 0..=20), so they hold at least 10,000 interior units under tables that are not the sign function; interior_params_for and interior_rows reach neither clamp at any shape.	0,1,3,5,7,11	8
tone-int-derive-pure	contract	tone::raw; tone_int::{derive,v1,id,tanh,round_half_away}	derive(asset) is a function of the asset's bytes alone. It reads them through tone::raw, so the shipped checks apply (Asset(e)), and refuses −128 in emb, w1 or w2 by name (Int8Min). It uses only binary64 +, −, ×, ÷ and round_half_away (a truncating cast and an exact subtraction), and no libm: MULT = rha(s_e·s_1·2²⁸), b1 = rha(b1·2⁸), b2 = rha(b2·127/s_2), lut[i] = rha(127·tanh((i − T)/2⁸)) for i ≤ 2T with tanh Lambert's fraction from t = 47, the i8 tensors verbatim, F = 8, G = 20, T = 1,024 and MIN_IDS = 6; its blob passes check (else Params(e)). On the shipped asset s_e, s_1 and s_2 are 0.0238046, 0.0124373 and 0.0209753, MULT 79,474, QMAX 27,021, M 2,047, max|b1| 98 and b2 (−1,402, −10,745, 12,343, −3,793, 3,599), and id = Digest::of(TONE_INT_DOMAIN, [LAYOUT_VERSION, int32-exact/v1, B, E, H, C, the params bytes]) = V1_ID. No table entry lies within 3.9·10⁻⁴ of a rounding boundary, and Lambert's fraction is within 8·2⁻⁵² of the system libm's tanh on the table's grid, a pin of aarch64-apple-darwin macOS 26A434's libm that fails there and is COULD NOT RUN on any other host where it differs.	0,1,3,6,7,12	5
tone-int-featurization	contract	tone_int::{featurize,pack,pack_into}; tone::for_each_ngram_bucket	pack(p, texts) has one row per text, in order, whose ids are exactly for_each_ngram_bucket(text) as u16 (each below 2,048), and pack_into(p, texts, offsets, ids) packs the same batch into the vectors it is given, keeping their capacity.	0,1,5,7,13	2
tone-int-workload-pins	table-property	tone_int::{workload,texts,workload_digest,class_code,F_ARGMAX_371,B371_OUT_DIGEST,WORKLOAD_DIGEST}; tone::ToneModel::{classify_opt,classify}	Over the 371 workload lines (the corpus's 300 then the held-out set's 71, in file order; B1 is ok sounds good to me; 25,368 ids; longest rows 147 and 120): workload_digest is WORKLOAD_DIGEST and forward's B371 words digest to B371_OUT_DIGEST; tone-int v1 clears the shipped held-out floors exactly as holdout_sanity_set_clears_documented_floors defines them, ABSTAIN scored as Technical, with 61 of 71 right; against the f32 path the largest pre-activation error is 1.542 steps of 2⁻⁸ and the smallest top-2 integer margin 676, and no line reaches either clamp (largest |p| 670 < T); B16384 holds 1,119,606 ids in 2,697,988 job bytes. The figures the shipped f32 path computes through the system libm's tanhf are pins of aarch64-apple-darwin macOS build 26A434 (kern.osversion): tone-int's argmax equals classify_opt's on 300 of 300 corpus and 71 of 71 held-out lines, classify_opt's is F_ARGMAX_371, the shipped classifier has 61 of 71 held-out lines right, and the largest logit error is 0.1814; each is computed first, and one that differs fails on that host and is COULD NOT RUN on any other, while the libm-free pins above hold on every host.	0,1,2,3,6,7,12,13	3
```

```tsv
file	test	primary	also
src/int_bag_mlp/tests.rs	layout_tables_have_unique_names_and_dense_indices	int-bag-mlp-layout	
src/int_bag_mlp/tests.rs	a_batch_reads_back_its_header	int-bag-mlp-layout	int-bag-mlp-batch-refusals
src/int_bag_mlp/tests.rs	out_digest_covers_every_word	int-bag-mlp-layout	
src/tone_int/tests.rs	a_three_row_batch_and_the_params_bytes_match_their_goldens	int-bag-mlp-layout	tone-int-derive-pure
src/int_bag_mlp/tests.rs	build_then_check_reads_back_the_spec	int-bag-mlp-params-checker	int-bag-mlp-layout
src/int_bag_mlp/tests.rs	refuses_a_blob_too_large	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_a_truncated_header	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_bad_magic	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_another_layout_version	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_dimensions_outside_their_bounds	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_min_ids_zero	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_a_shift_outside_its_range	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_a_negative_t	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_mult_outside_its_range	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_a_stored_qmax_that_is_not_recomputed	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_a_tensor_of_the_wrong_length	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_a_tensor_outside_the_blob	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_an_unaligned_tensor	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_overlapping_tensors	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_trailing_bytes	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_minus_128_in_each_i8_tensor	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_a_lossy_clamp	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	refuses_b2_at_two_to_the_29	int-bag-mlp-params-checker	
src/int_bag_mlp/tests.rs	an_all_abstain_batch_with_no_ids_is_valid	int-bag-mlp-batch-refusals	int-bag-mlp-forward-bounds
src/int_bag_mlp/tests.rs	job_bytes_count_each_buffer_as_it_is_made	int-bag-mlp-batch-refusals	
src/int_bag_mlp/tests.rs	refuses_an_empty_batch	int-bag-mlp-batch-refusals	
src/int_bag_mlp/tests.rs	refuses_offsets_that_decrease	int-bag-mlp-batch-refusals	
src/int_bag_mlp/tests.rs	refuses_offsets_that_do_not_span_the_ids	int-bag-mlp-batch-refusals	
src/int_bag_mlp/tests.rs	refuses_an_id_out_of_range	int-bag-mlp-batch-refusals	
src/int_bag_mlp/tests.rs	refuses_a_row_too_long	int-bag-mlp-batch-refusals	
src/int_bag_mlp/tests.rs	refuses_too_many_rows	int-bag-mlp-batch-refusals	
src/int_bag_mlp/tests.rs	refuses_a_batch_too_large	int-bag-mlp-batch-refusals	
src/int_bag_mlp/tests.rs	the_bounds_are_the_designs	int-bag-mlp-forward-bounds	int-bag-mlp-params-checker
src/int_bag_mlp/tests.rs	the_steps_follow_the_design_by_hand	int-bag-mlp-forward-bounds	
src/int_bag_mlp/tests.rs	ties_go_to_the_lowest_class	int-bag-mlp-forward-bounds	
src/int_bag_mlp/tests.rs	forward_refuses_its_arguments_by_name	int-bag-mlp-forward-bounds	
src/int_bag_mlp/tests.rs	checked_forward_holds_at_the_extremes	int-bag-mlp-forward-bounds	
src/int_bag_mlp/tests.rs	forward_fast_equals_forward_on_seeded_blobs	int-bag-mlp-forward-bounds	
src/int_bag_mlp/tests.rs	the_interior_blob_stays_unclamped_at_every_shape	int-bag-mlp-forward-bounds	
src/tone_int/tests.rs	forward_fast_equals_forward_on_the_workload	int-bag-mlp-forward-bounds	tone-int-workload-pins
src/tone_int/tests.rs	the_derivation_figures_and_id_are_pinned	tone-int-derive-pure	
src/tone_int/tests.rs	derive_is_pure_and_names_no_libm	tone-int-derive-pure	
src/tone_int/tests.rs	the_table_is_exact_and_its_rounding_is_half_away	tone-int-derive-pure	
src/tone_int/tests.rs	derive_refuses_a_bad_asset_and_minus_128	tone-int-derive-pure	tone-asset-decoder-fail-closed
src/tone_int/tests.rs	the_id_covers_its_descriptor	tone-int-derive-pure	
src/tone_int/tests.rs	packed_ids_are_the_shipped_buckets	tone-int-featurization	tone-ngram-featurization
src/tone_int/tests.rs	pack_into_reuses_the_batchs_vectors	tone-int-featurization	int-bag-mlp-batch-refusals
src/tone_int/tests.rs	tone_int_clears_the_shipped_floors_and_agrees_with_the_shipped_classifier	tone-int-workload-pins	
src/tone_int/tests.rs	fidelity_to_the_f32_path_is_pinned	tone-int-workload-pins	
src/tone_int/tests.rs	the_session_pins_reproduce	tone-int-workload-pins	
```

## 9. Added 2026-10-24: effects history and repair off the main thread (Rewind R2)

`aos_momentum::watch` (the online watcher's judgement of one retired log epoch), the off-main repair
(`RepairJob`, `PreparedRepair`, `MomentumHost::{aos_repair_job,aos_commit,aos_take_retired}`),
`MomentumCode::Safe`, `MomentumTrace::check` and the host's refusal to record the vivid light-page ink are new since the recovery, and their tests are read
the same way as §1. aterm-aos keeps no SPECS.md of its own, so the substrate's two R2 rows (the
retired ring and the pure registry) sit here beside their one host; their tests are aterm-aos's
`tests/publish_retired.rs`. Design of record: `docs/DESIGN-aos-hosted-execution-2026-10-21.md`
§14.4.4 and §14.5 R2.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
aos-retired-ring	contract	aterm_aos::publish::Boundary::{admit,restore,commit,drain_retired,retired_len,stats}; Retire	Every recording a boundary replaces goes into its retired ring, oldest first, with the cause (Full when a full log re-anchors, Broken when a broken one does, Restored by restore, Committed by commit), the code that ran it and the state it ended in: replaying its log from its checkpoint under that code reaches end_state. An admit with room retires nothing. The ring holds at most RETIRED_CAP (4); a fifth drops the oldest and counts it in stats.retired_dropped, and stats.retired counts every one. drain_retired takes them all. Commit's store count is unchanged ([Old, Old, New, New] under the fault gate).	0, 1	crates/aterm-aos/tests/publish_retired.rs::each_full_log_reanchor_retires_the_recording_it_replaced_with_its_code_and_end_state, crates/aterm-aos/tests/publish_retired.rs::the_ring_keeps_the_newest_retired_recordings_and_counts_the_ones_it_dropped, crates/aterm-aos/tests/publish_retired.rs::a_broken_log_is_retired_as_broken_when_the_next_input_reanchors, crates/aterm-aos/tests/publish_retired.rs::a_restore_retires_the_recording_it_replaced_ending_in_the_state_before_it, crates/aterm-aos/tests/publish_retired.rs::a_commit_retires_the_recording_it_replaced_with_the_code_it_replaced
aos-prepare-on-clone	contract	aterm_aos::publish::Boundary::{clone,prepare,commit}; aos_momentum::{RepairJob::prepare,MomentumHost::{aos_repair_job,aos_commit}}	A prepare made on a clone of a paused boundary (on any thread) commits on the original with every check commit makes; both shadow replays run on the clone, so the original's stats.replays is unchanged. If the original's head moved since the clone (an input after a resume) the commit is StaleHead; a re-attach since it is StaleHead or WitnessMismatch("ancestor"); a resume is NotPaused; a detach is NotAttached; a failing candidate's refusal comes back from the prepare. A refusal changes nothing: the pause, the queue and the live pair stay.	0, 1	crates/aterm-aos/tests/publish_retired.rs::a_prepare_made_on_a_clone_of_the_paused_boundary_commits_on_the_original, crates/aterm-aos/tests/publish_retired.rs::negative_control_a_head_moved_since_the_clone_is_refused_stale_or_by_its_witness, src/aos_momentum/watch_tests.rs::a_repair_prepared_on_another_thread_commits_on_the_host_with_no_replay_on_it, src/aos_momentum/watch_tests.rs::negative_control_a_job_whose_head_moved_is_refused_and_changes_nothing
aos-registry-lines	contract	aterm_aos::registry::{Registry::{insert,listings,lines},Listing::line,Row::words,Judged::{token,failed},Timeline::{push,len,dropped}}	A registry holds one listing per slot in slot order (an insert of a slot replaces it). lines() is tasks=<n>, then per listing its line and its timeline rows as slot=<s> row=<k> <words>, oldest first. A timeline keeps its newest cap rows and counts the dropped ones. A judgement's token is pass, fail:<law>@<cursor> or unjudged:<why with spaces as _>.	0	crates/aterm-aos/tests/publish_retired.rs::the_registry_lists_each_slot_once_in_order_with_its_timeline_rows
momentum-safe-code	contract	aos_momentum::{MomentumCode::{Safe,ALL,from_name,artifact_name},Sched::is_active,PrebuiltMomentum}; policy::AcceptancePolicy::base	Safe (momentum-glow.safe, ungated) is the shipped engine with next_change_deadline(now, f) = now + f while is_active or settling and None otherwise, so momentum-glow.no-stranded-light holds on every log it runs or replays: the golden trace, every lane item of the M4 corpus run live and every shipped log replayed under it, and the demonstration fault's logs (where the demonstration code fails HotHasWake). Its Frame observations equal Shipped's (and the demonstration code's) on each of those logs. It is no M4 policy base, so a plan published over it is refused BaseNotInPolicy { live: "safe" }, with the pause, queue and counters unchanged.	0, 1	src/aos_momentum/watch_tests.rs::the_safe_code_keeps_the_contract_on_the_golden_trace_with_the_shipped_frames, src/aos_momentum/watch_tests.rs::the_safe_code_keeps_the_contract_on_the_m4_corpus_live_and_over_the_shipped_logs, src/aos_momentum/watch_tests.rs::the_safe_code_keeps_the_contract_on_the_demonstration_faults_logs, src/aos_momentum/publish_tests.rs::publishing_over_the_safe_code_is_refused_base_not_in_policy, src/aos_momentum/policy_tests.rs::shipped_and_the_demo_code_are_the_only_bases, src/aos_momentum/watch_tests.rs::the_safe_code_names_a_frame_wake_while_hot_or_settling_and_none_at_rest
momentum-watch-segment	contract	aos_momentum::{MomentumHost::{aos_take_retired,aos_retired_waiting,aos_attach,aos_detach},Segment::{judge,unjudged},SegmentJudgement}	judge replays a retired segment from its checkpoint under the code that ran it, folding observations directly through NoStrandedLight::replay_judged without retaining per-frame quads. It is Unjudged when the log is broken, names another contract, its checkpoint does not restore, or the replay does not reach the state the segment ended in; otherwise it is momentum-glow.no-stranded-light's verdict. A failing segment carries itself as a MomentumTrace whose check under the same build replays to the same failure. unjudged(why) is the same judgement's cause, code, log epoch and range with Unjudged(why) and no fault, and no replay. Segments taken from one host are contiguous (each one's to is the next one's from); the shipped and safe codes' segments pass and the demonstration code's include a HotHasWake failure. A host that drops its recording (a detach, or an attach on a new epoch) keeps the recording's undrained retired segments for aos_take_retired, at most RETIRED_CAP, the oldest dropped.	0, 1	src/aos_momentum/watch_tests.rs::the_watcher_pins_the_demonstration_faults_segments_and_passes_the_shipped_ones, src/aos_momentum/watch_tests.rs::a_segment_whose_replay_misses_its_end_state_or_whose_log_broke_is_left_unjudged, src/aos_momentum/watch_tests.rs::a_commit_retires_the_live_codes_segment_and_the_watcher_judges_it, src/aos_momentum/watch_tests.rs::a_reattach_keeps_the_dropped_recordings_retired_segments_for_the_watcher, src/aos_momentum/watch_tests.rs::a_segment_the_watcher_could_not_judge_keeps_its_span_and_no_fault, src/aos_momentum/trace_tests.rs::a_full_window_of_light_page_4095_quad_frames_checks_and_judges
momentum-trace-check-identity	contract	aos_momentum::trace::{MomentumTrace::{check,identity_mismatch,replay},TraceFault::{Identity,Log,word},MOMENTUM_TRACE_MAX_BYTES}; NoStrandedLight::replay_judged	check(epoch, build) refuses Identity(m) before reading the log unless the trace's identity (task, code, contract, schema, profile) is that of one of this build's prebuilt codes; m is the first part that differs from the shipped code's identity under build (Code for another build). Otherwise it refuses Log(TooManyEvents { limit: MOMENTUM_HOST_MAX_EVENTS }) before it decodes an event of a log longer than one host log epoch, and folds each step's observation through NoStrandedLight::replay_judged (dropping per-frame quad vectors after each step) to equal replay's result and NoStrandedLight's verdict on the run. replay alone still names only the code (UnknownCode) and keeps the MOMENTUM_MAX_EVENTS limit, also folding observations without retaining them. A trace of one full host log epoch of the largest input (a frame with a cursor and a glyph span) encodes to at most half of MOMENTUM_TRACE_MAX_BYTES (2 MiB), the bound a checking reader applies before it decodes.	0	src/aos_momentum/watch_tests.rs::a_trace_from_another_build_is_refused_identity_before_it_replays, src/aos_momentum/trace_tests.rs::a_check_refuses_a_log_longer_than_one_host_epoch_before_it_replays, src/aos_momentum/trace_tests.rs::the_largest_trace_a_window_exports_fits_twice_under_the_check_bound, src/aos_momentum/trace_tests.rs::a_full_window_of_light_page_4095_quad_frames_checks_and_judges, src/aos_momentum/contract_tests.rs::replay_judged_equals_judge_over_run_across_every_law_and_refusal
momentum-vivid-recorded	contract	cursor_momentum::MomentumGlowConfig::light_vivid; aos_momentum::{ConfigRecord::light_vivid,MOMENTUM_EVENT_ENCODING,schema_artifact,MomentumHost::{aos_attach,aos_pause,aos_repair,aos_take_detached,aos_take_retired,aos_retired_waiting}}; PAST_HORIZON	MomentumGlowConfig::light_vivid and ConfigRecord::light_vivid record whether the light-page glow wears its hue's vivid ink (Sparkle's paper gold) on every Tick input under momentum-glow.state+events v2 (MOMENTUM_EVENT_ENCODING = 2), and MomentumGlow's frame fingerprint includes light_vivid on light pages. A glow in that ink attaches, pauses, repairs, exports and checks a trace, and switches between the walk-toward-white ink and the vivid ink mid-recording without detaching, drawing byte-for-byte as an unattached host's glow on every frame and retiring passing segments for aos_take_retired (at most RETIRED_CAP). The past-horizon self-detach keeps its retired segments on the same path (PAST_HORIZON).	0	src/aos_momentum/vivid_tests.rs::a_glow_in_its_vivid_ink_draws_as_an_unattached_one_through_an_attach_a_pause_and_a_repair, src/aos_momentum/vivid_tests.rs::turning_the_vivid_ink_on_while_attached_draws_as_an_unattached_glow_from_that_frame, src/aos_momentum/vivid_tests.rs::control_with_the_walk_toward_white_ink_the_repaired_glow_also_draws_as_an_unattached_one
momentum-chosen-recorded	contract	cursor_momentum::{MomentumGlowConfig::{chosen_hue,chosen_base},chosen_glow_ink,chosen_charge}; aos_momentum::ConfigRecord::{chosen_hue,chosen_base,ink_byte}; cursor_fx::CursorFx::tick	MomentumGlowConfig::chosen_hue and chosen_base say the glow's hue and the caret's base are colours the user chose (a pinned cursor colour, or the trail colour a style wears); the cursor effect sets them from the same reading of a chosen colour the caret bodies use. A chosen hue's paper halo wears the colour itself walked toward white only as far as the theme's text asks (chosen_glow_ink), never its hue's vivid ink, and a chosen base's charged block caret is darkened in its own hue on paper and lifted toward white on a dark page (chosen_charge), never mixed toward the momentum tint: either way the hue stays the colour's and the chroma never rises (review9fix round 2's judge: beside Sparkle pinned to #b0b0c8 the paper halo was laid up to #c1c1ee). ConfigRecord records them with light_vivid in one ink byte (bits 0, 1 and 2) on every Tick input, so a glow that reads no chosen colour writes the 0 or 1 light_vivid's bool was written as, every recording and pin stands under momentum-glow.state+events v2, and a byte with another bit set is BadTag cfg.ink.	0	src/cursor_fx.rs::the_momentum_glow_wears_a_chosen_colour_in_its_own_hue_and_chroma, src/aos_momentum/codec_tests.rs::a_bool_or_option_byte_no_encoding_writes_is_refused_naming_its_field
cw-paper-chosen	contract	classic_wake::{WakeInk::chosen,ClassicWake::{tick,emit_light_crown,emit_light_comet}}; cursor_glow::light_body::LightGround::{body_ink,vivid_body_ink}; CursorGlow::tick	CursorGlow::tick sets WakeInk::chosen when the trail's colour is one the user chose (ColourPins::trail on a style that draws the trail colour). On a paper page the one-colour face's crown and tracer then wear their tone (the colour, or its accent along the tracer) as itself, walked toward white only as far as the theme's text over it at the body ceiling asks (vivid_body_ink), never solved from the hue's vivid ink (body_ink): each laid pixel keeps its tone's hue and no more than its chroma (review9fix round 3: a Classic trail pinned to #b0b0c8, chroma 24 of 255, was laid up to #9595e1, 76). The spectrum face, a dark page and an unchosen colour are unchanged.	0	src/cursor_glow/tests/light_ground.rs::a_pinned_classic_s_paper_wake_keeps_the_pin_s_hue_and_chroma
```

```tsv
file	test	primary	also
src/aos_momentum/watch_tests.rs	the_safe_code_keeps_the_contract_on_the_golden_trace_with_the_shipped_frames	momentum-safe-code	
src/aos_momentum/watch_tests.rs	the_safe_code_keeps_the_contract_on_the_m4_corpus_live_and_over_the_shipped_logs	momentum-safe-code	
src/aos_momentum/watch_tests.rs	the_safe_code_keeps_the_contract_on_the_demonstration_faults_logs	momentum-safe-code	
src/aos_momentum/watch_tests.rs	the_safe_code_names_a_frame_wake_while_hot_or_settling_and_none_at_rest	momentum-safe-code	
src/aos_momentum/watch_tests.rs	a_repair_prepared_on_another_thread_commits_on_the_host_with_no_replay_on_it	aos-prepare-on-clone	
src/aos_momentum/watch_tests.rs	negative_control_a_job_whose_head_moved_is_refused_and_changes_nothing	aos-prepare-on-clone	
src/aos_momentum/watch_tests.rs	the_watcher_pins_the_demonstration_faults_segments_and_passes_the_shipped_ones	momentum-watch-segment	aos-retired-ring
src/aos_momentum/watch_tests.rs	a_segment_whose_replay_misses_its_end_state_or_whose_log_broke_is_left_unjudged	momentum-watch-segment	aos-retired-ring
src/aos_momentum/watch_tests.rs	a_commit_retires_the_live_codes_segment_and_the_watcher_judges_it	momentum-watch-segment	aos-retired-ring
src/aos_momentum/watch_tests.rs	a_trace_from_another_build_is_refused_identity_before_it_replays	momentum-trace-check-identity	
src/aos_momentum/publish_tests.rs	publishing_over_the_safe_code_is_refused_base_not_in_policy	momentum-safe-code	
src/aos_momentum/vivid_tests.rs	a_glow_in_its_vivid_ink_draws_as_an_unattached_one_through_an_attach_a_pause_and_a_repair	momentum-vivid-recorded	
src/aos_momentum/vivid_tests.rs	turning_the_vivid_ink_on_while_attached_draws_as_an_unattached_glow_from_that_frame	momentum-vivid-recorded	momentum-watch-segment
src/aos_momentum/vivid_tests.rs	control_with_the_walk_toward_white_ink_the_repaired_glow_also_draws_as_an_unattached_one	momentum-vivid-recorded	
src/aos_momentum/trace_tests.rs	a_full_window_of_light_page_4095_quad_frames_checks_and_judges	momentum-trace-check-identity	momentum-watch-segment
src/aos_momentum/contract_tests.rs	replay_judged_equals_judge_over_run_across_every_law_and_refusal	momentum-trace-check-identity	
```

## 10. Added 2026-10-26: the ⬆️'s hover ring

`up_ring` builds the hairline ring the gui lays under a top rule's ⬆️ (a newer CLI build) while the
pointer is on it (design of record: `crates/aterm-gui/SPECS.md`'s `top-rule-harness-switch`; the
harness switch itself is words and has no sprite). Its tests are read the same way as §1;
`aterm-gpu`'s `fx_parity::the_up_ring_is_byte_exact_on_the_gpu` and
`fx_parity::the_cpu_draws_the_up_rings_bytes` hold the GPU and CPU compositors to its bytes.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
up-ring	contract	up_ring::{raster,content,layer,ring_ink}; RingKey; RingHue; RING_CELLS; RING_WASH; RING_CONTRAST	raster(key) is a premultiplied RING_CELLS·cw × ch sprite, a function of key alone: coverage clamp(0.5 - d, 0, 1) at pixel centres of the signed distance to one pill 82% of the cell high, centred on the rule's `─` stroke and grown where needed to leave a clear pixel between Script::small_emoji_rows and its rim, a hairline rim in ring_ink washed RING_WASH inside, its corners clear; ring_ink(key) is the rule's tint at the line's end for the hue (Arc(palette), Accent(rgb), Neutral), floored to RING_CONTRAST (3:1) against the ground for every hue and ground. content(key) holds that one FxSprite at the origin in an Arc, empty where the cell has no size; layer(id, content, offset, clip) is an Under, Grid-space, Exact-blend FxLayer at offset.	0,1,3,7	3
```

```tsv
file	test	primary	also
src/up_ring.rs	the_ring_is_a_hairline_pill_round_the_arrow	up-ring	
src/up_ring.rs	the_rim_clears_the_small_emojis_rows_by_a_pixel	up-ring	
src/up_ring.rs	every_ring_reads_on_its_ground	up-ring	
```

## 11. Added 2026-10-27: the light latch (Prism Glass 1.2)

`host::light_latch` decides whether a window's frame may draw by the light rules (design of
record: `docs/design/PRISM-GLASS.md` §2.1 and §15.2 1.2). Its tests are read the same way as §1;
the Tier-0 of its model is `crates/aterm-models-effects/tests/derived_light_latch.rs`.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
light-latch	refinement	host::light_latch::{LightLatch::{admits,last},raw_light,scheme_light,LatchKey,LatchOut,LIGHT_ENTRY_HOLD}; aterm_models_effects::light_latch_model	raw_light(fg, bg, dark_theme) = both resolved 0x00RRGGBB, !dark_theme, Y(fg) < Y(bg) and cr(fg, bg) >= 3; scheme_light(bg) = bg resolved and !aterm_render::theme_is_dark(bg). admits(now, key, raw) runs on every frame: ok = raw && scheme_light(key.bg); scheme_changed = key differs from the last frame's key (or there was none); fresh = scheme_changed or a change no ok frame has met (a change seen on a !ok frame stays pending until the next ok frame); a !ok frame is off and ends the run of ok frames; an ok frame is on iff fresh, or the run has held LIGHT_ENTRY_HOLD (300 ms), or the last on frame is within LIGHT_ENTRY_HOLD before the run began; fell = the last frame on and this one off; last() is the last verdict. So no dark configured scheme enters under any DECSCNM or OSC 10/11 page (all 37 dark built-ins and pack themes), exit is immediate, and the latch refines LightLatch on every transition of its reachable graph, whose five Buggy controls (the short-circuit, a change spent on a !ok frame, the scheme gate dropped, a latched exit, the last lit frame written on every light frame) it leaves on some frame; the model's licence reads its own ghost of the last lit frame (glast), never the latch's last.	0,1,2,3,10,12	9
```

```tsv
file	test	primary	also
src/host/light_latch.rs	raw_light_needs_dark_ink_on_a_light_page_at_three_to_one	light-latch	
src/host/light_latch.rs	light_returning_after_more_than_300_ms_away_waits_300_ms	light-latch	
src/host/light_latch.rs	light_returning_within_300_ms_enters_at_once	light-latch	
src/host/light_latch.rs	a_configured_scheme_change_enters_on_its_first_light_frame	light-latch	
src/host/light_latch.rs	a_scheme_change_seen_on_a_dark_page_still_enters_on_its_first_light_frame	light-latch	
src/host/light_latch.rs	a_tab_switch_back_to_a_light_session_enters_at_once	light-latch	
src/host/light_latch.rs	no_dark_scheme_enters_under_decscnm_or_osc11	light-latch	
tests/light_latch_conformance.rs	the_latch_refines_its_model_on_every_reachable_frame	light-latch	
tests/light_latch_conformance.rs	negative_control_every_buggy_latch_is_left_on_some_frame	light-latch	
```

## 12. Added 2026-10-27: the Prism Glass solver (Prism Glass 1.4)

`rainbow_kitty::prism_glass` solves a light page's tables: the glass behind text, the deep lip
under it, the caret, and a lip-forward page's own lip (design of record:
`docs/design/PRISM-GLASS.md` §1, §3 and §15.2 1.4). The bed's glass draws from them (§13). The six glass
pages and two lip-forward pages ship as `prism_glass/tables.rs`, which the first test regenerates;
any other pair is solved by `PaletteSolve` in counted units. Its tests are read the same way as
§1. `palette-solve-budget` is a fourth row beside the three the design names, for the solve's
units and cache.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
glass-legibility-bar	table-property	prism_glass::{glass_ink,glass_bar,glass_anchors,headroom,solve,tables::SHIPPED,SHIPPED_PAGES}; prism_glass::scan::{scan,breakpoints,origins,Ground::pair_min}; ribbon::BedInkLut::sample; aterm_render::{over_premul,premul_rgb,glow_lerp_rgb}	The light partner of bed-legibility-bar. tables::SHIPPED is solve()'s output for SHIPPED_PAGES byte for byte, and those are the light built-in schemes and the effects tests' fixture (#24292f on white). On each glass page, with bar = min(5.25, 0.9 cr(fg, bg)): for every origin interval between the breakpoints of BedInkLut::sample(glass, t) and sample(glass, t + step), t in [0, 2), at one, two and three slabs per cell (step 1/16, 1/32, 1/48) and on the lay leg (1/36), every colour a slab lays under both column models (lerp_rgb at k/16; glow_lerp_rgb between the premul_rgb ends across 15, 8 + 7 or 5 px), composited by over_premul over the page at every alpha 1..=255, keeps cr(fg, ·) >= bar + LERP_BAR_GUARD (5.35), and so does every arc-table glass entry at every alpha. glass_ink lays each entry at the anchors' cusp L interpolated with the hue along the arc, raised to the fg's floor (bar + SOLVE_GUARD) and to the ORANGE FLOOR's room, C at the gamut edge capped at .24, L capped at .93; the lift raises every entry a ramp under the target brackets by LIFT_STEP_L until no ramp is (2 raising rounds, 25 entries on Default Light, one-slab worst 5.3844), and a page it cannot clear in MAX_LIFT_ROUNDS draws lip-forward. A page takes glass (Mode::Glass) iff every glass anchor keeps C >= HEADROOM_C (.12) at its floor; one without draws no glass (LipForward: Solarized Light, Catppuccin Latte), and a page that is not light draws nothing (Paper).	0,1,3,5,6,7,12	5
light-mud-census	table-property	prism_glass::{mud_spec,mud_bands,mud_broad,mud,deep_ink,deep_forward,amber_hold_forward,cyan_fold,FOLD_ARC,FOLD_AT,FOLD_AT_ARC,CYAN_AT,CYAN_AT_ARC,AMBER_HOLD_HUE,ORANGE_FLOOR_L,LIP_FORWARD_CR,LIP_FORWARD_GOLD_CR,LIP_FORWARD_GOLD_L,Palette::cyan_snap}; prism_glass::scan::all_lerps	mud_spec(c) = C < .12, or h in 85..125 at L < .82, or h in 40..85 at L < .62 (mud_bands is the two bands); mud_broad(c) = L < .75 and (C < .10 or h in 60..115). On every shipped page, on every light page of the theme pack (the light rows of contrib/themes/pack/manifest.tsv, solved at test time), and on four synthetic lip-forward pages that tell the lip-forward rules from their rejected forms (#e7e7e7, #e6e7ed and #eee8b8 on #616161, #eff0f6 on #676767, solved at test time), no entry of lip129, lip511, caret129 or caret511, and no colour all_lerps draws between neighbours of them except across a snap (the lip's fold snap at FOLD_AT 39 and FOLD_AT_ARC 155, its cyan snap at CYAN_AT 76 and CYAN_AT_ARC 310 where cyan_snap is set, the caret's snaps), is mud under either rule; the glass tables are held to mud_bands and mud_broad (their chroma is the HEADROOM gate's), and Default Light's glass to both whole rules. all_lerps reaches every distinct lerp_rgb colour of a pair (one u inside each interval between its channels' breakpoints) and none outside the pair's channel ranges. The lip: hues [27, 50, 84, 145, 205, 264, 290, 315] at the anchors, gold's 84 held to FOLD_ARC (.3051) and 132 after it; L the argmax of C − 0.10 |L − L_pref| at least .10 under the quantised glass and 1.6:1 against the page; held at h 58.5 while L < .765 on 58.5 < h < 85 (the AMBER HOLD); and no darker than .625 from h 38.5 up the orange band, the glass raised to leave it room (the ORANGE FLOOR). A lip-forward lip keeps 2.0:1 against the page, 1.95:1 where h 60..115 needs it to reach L .755; on every page above drawn lip-forward, each lip129 and lip511 entry keeps 2.0:1, or 1.95:1 where its own hue is in 60..115. Once both lip-forward lip tables are solved (in the last ForwardArc unit, before the caret, so no unit is added): when an entry of either on the gold stretch (58.5 < deep hue < 85, walk entries 20..39 and arc entries 77..155) is mud, every stretch entry of both takes one colour, the lip-forward lip at h 58.5 (the LIP-FORWARD AMBER HOLD: Kanagawa Lotus #fb8a00 and TokyoNight Day #f18500, within 0.5° of h 58.5 at 2.0:1); then, in each table by itself, with an entry thin at C < .12 and the caret fold's zones, the warm and green zones' first thin entry through the zone's end take the zone's last clean entry before it, and the blue zone's start through its last thin entry take the zone's first clean entry after it, a zone with no clean entry on that side left as it is (the LIP-FORWARD CYAN FOLD: on TokyoNight Day walk 73..76 take entry 72's #00b4ab and 77 takes 78's #00b1c7, arc 292..310 take 291's #00b4ab and 311..317 take 318's #00b1c6, each at C >= .12 and 2.0:1); cyan_snap is set when an entry of the pair straddling the cyan waypoint (walk (76, 77), arc (310, 311)) folded, and is false on every shipped page. Without the fold snap Default Light's fold pair lerps through spec-rule mud, and without the cyan snap TokyoNight Day's cyan pairs lerp through mud; #e7e7e7, #e6e7ed and #eee8b8 set cyan_snap, and #eff0f6's unheld gold stretch is mud in its arc table alone while its gold anchor reaches L .755 at 1.95:1, so the hold's trigger reads both tables' entries.	0,1,3,5,6,7,12	6
caret-cut-out	table-property	prism_glass::{caret_entry,caret_fill,caret_fold,caret_table,vivid,CARET_BAR,CARET_SOLVE}	caret_fill solves a lip entry to CARET_SOLVE (4.65) against a side, its lightness moved away from the side with chroma held to the lip's and the gamut edge. caret_entry cuts out the fg or the page on a glass page (whichever solved fill keeps more chroma, the page on a tie) and the page on a lip-forward one. The CARET FOLD gives a fill that is mud, or a page-side fill that is not vivid (C >= .15 and no h 35..135 under L .65), its nearest clean entry's fill and side within its zone (warm before FOLD_ARC, green before the cyan waypoint, blue after); the caret snaps at a side flip, across FOLD_ARC, where a folded entry meets another zone, and between folded entries of different fills. On every shipped page, every light page of the theme pack and the four synthetic pages light-mud-census names, at both lengths, every entry keeps cr(fill, side) >= CARET_BAR (4.6), and so does every colour all_lerps draws between unsnapped neighbours (Solarized Light's walk caret worst 4.6306, its folded runs 6..43 -> #d03400 | #3f7d00 and 61..95 -> #00812b | #0071c6). On each of those pages the walk caret (fill, sides, folds and snaps) is caret_table over the walk lip the page ships with, so a lip-forward page's caret is solved from its held and folded lip; no page yet shows the lip-forward rules changing a caret entry (every walk entry whose lip they change has its caret folded, before them and after), so the order is held by the solve, not seen in a table.	0,1,3,5,6,7,12	1
palette-solve-budget	contract	prism_glass::{PaletteSolve::{new,advance,done,units,next_unit},PaletteCache::{frame,holds,pending,len},take_solves,SOLVE_UNITS_PER_FRAME,MAX_SOLVE_UNITS,PALETTE_CACHE_LEN}	PaletteSolve(fg, bg) runs in counted units, never timed: the page's class, the 8 anchors, each walk and arc glass entry, the scan's ground, each lift round's three densities' origins and its 128 table intervals, each walk and arc lip entry, each caret entry, and the fold; it runs at most MAX_SOLVE_UNITS and ends at solve(fg, bg)'s palette (on a shipped glass page 1,682 units when its lift takes one raising round, as Gruvbox Light's does, and 1,813 when it takes two, as every other's does; 779 on a lip-forward one). PaletteCache::frame runs no unit for a shipped pair or a kept one, and at most SOLVE_UNITS_PER_FRAME (4) for the pair it is solving, returning None (main's paper arm) until the solve is done; a request for another pair that is neither shipped nor kept drops the solve in progress, and a shipped or kept pair answers without touching it; it keeps the PALETTE_CACHE_LEN (8) most recently asked pairs. take_solves() is this thread's units since the last take, the host's prism_glass_solves == 0 pin. SOLVE_UNITS_PER_FRAME is the units of the most costly kind that fit 1 ms, from aterm-bench's prism_glass_units release bench (2026-10-27: 243 µs, a lift round's interval scan).	0,1,2,3,7	4
```

```tsv
file	test	primary	also
src/rainbow_kitty/prism_glass/tests.rs	the_shipped_tables_are_the_generators_output	glass-legibility-bar	light-mud-census,caret-cut-out
src/rainbow_kitty/prism_glass/tests.rs	the_shipped_pages_are_every_light_builtin_scheme	glass-legibility-bar	
src/rainbow_kitty/prism_glass/tests.rs	default_lights_glass_anchors_are_the_spec_hex	glass-legibility-bar	
src/rainbow_kitty/prism_glass/tests.rs	the_light_glass_keeps_every_glyph_at_its_bar_at_every_alpha	glass-legibility-bar	
src/rainbow_kitty/prism_glass/tests.rs	a_theme_without_headroom_draws_no_glass	glass-legibility-bar	light-mud-census
src/rainbow_kitty/prism_glass/tests.rs	no_table_is_mud_on_any_light_theme	light-mud-census	
src/rainbow_kitty/prism_glass/tests.rs	no_lerp_between_entries_is_mud	light-mud-census	
src/rainbow_kitty/prism_glass/tests.rs	default_lights_lip_folds_at_39_and_holds_its_amber	light-mud-census	
src/rainbow_kitty/prism_glass/tests.rs	the_lip_forward_rules_lay_one_amber_and_fold_tokyonight_days_cyan	light-mud-census	
src/rainbow_kitty/prism_glass/tests.rs	all_lerps_covers_every_sixteenth	light-mud-census	caret-cut-out
src/rainbow_kitty/prism_glass/tests.rs	the_pack_light_pages_are_the_manifests_light_rows	light-mud-census	caret-cut-out
src/rainbow_kitty/prism_glass/tests.rs	every_caret_lerp_keeps_its_cut_out	caret-cut-out	
src/rainbow_kitty/prism_glass/tests.rs	a_steady_frame_advances_no_solve_unit	palette-solve-budget	
src/rainbow_kitty/prism_glass/tests.rs	a_fresh_pair_completes_within_its_unit_count	palette-solve-budget	
src/rainbow_kitty/prism_glass/tests.rs	the_palette_cache_holds_at_most_eight_pairs	palette-solve-budget	
src/rainbow_kitty/prism_glass/tests.rs	a_request_for_another_pair_drops_the_solve_in_progress	palette-solve-budget	
```

## 13. Added 2026-10-28: the bed's glass arm (Prism Glass 1.5)

The bed's ink table gains Prism Glass's glass (design of record: `docs/design/PRISM-GLASS.md`
§15.2 1.5): under `glass_lit` the bed draws by the page's solved glass129, with main's table built
beside it on every frame. Nothing sets a light yet (`prism_glass::LIVE` is `Live::Off` and no host
latches one), so every frame draws main's table. Its tests are read the same way as §1; the host's
dark pin (`aterm-gui` `light_fork_pin_tests.rs`) holds the bed's glass and the light words absent on
every dark ground.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
bed-glass-arm	contract	ribbon::{BedInkLut::{sync,sync_glass,synced,at,seen_floor_at,sample,glass,glass_lit,key,lut,seen},glass_table,Ribbon::{plan_over,emit,glass_drew_at,unlay_glass}}; prism_glass::{PaletteCache::frame,Palette::headroom,take_solves,SOLVE_UNITS_PER_FRAME}; pipeline::EffectsPipeline::{light_arms,light_words}	BedInkLut::sync builds main's table for (theme_fg, theme_bg, dark_theme, ribbon_tall) byte for byte on every frame, whatever the frame's light; sync_glass fills the glass under glass_lit = cfg.light.glass() && ribbon_tall && !dark_theme && the page's palette (PaletteCache::frame: shipped, kept, or a pending solve advanced by at most SOLVE_UNITS_PER_FRAME counted units) is solved and has headroom, once when the glass lights or the page changes, and drops it on any other frame. key(), lut(), seen() and main_seen_floor_at(t) read main's table on every frame; at(t) and seen_floor_at(t) read the glass's under it (main's sampler, BedInkLut::sample, over the palette's glass129) and main's otherwise. So on every dark ground (each dark built-in and pack scheme), on both spellings, under every light, the bed is main's, a ribbon's frame lays main's body, plans and lays no glass (§14) and no solve unit runs; on each glass page every glass129 entry composited by over_premul over the page at every alpha 1..=255 keeps cr(fg, .) >= glass_bar(fg, bg); a pending palette draws main's paper bed until its solve completes; a lip-forward page and the underline spelling draw main's. A glass frame lays the glass's own geometry (§14, since Prism Glass 1.7; until then main's body quad for quad) in the glass's ink, its plan's band() main's; Ribbon::emit stamps glass_drew_at(now) on every such frame whose plan holds a run, laid or not, and glass_span only when it laid body (unlay_glass clears both when the engine hides the frame), and EffectsPipeline::light_arms reports the glass arm, glow_under + glow_under_ramps, for the last cursor step, with the bed's glass and the palettes in light_words.	0,1,2,3,12	6
```

```tsv
file	test	primary	also
src/rainbow_kitty/ribbon.rs	the_bed_ink_lut_is_mains_on_every_dark_ground	bed-glass-arm	
src/rainbow_kitty/ribbon.rs	the_glass_bed_keeps_every_entry_its_glass_and_every_letter_its_bar	bed-glass-arm	
src/rainbow_kitty/ribbon.rs	the_glass_lights_only_the_tall_spelling_of_a_page_with_headroom	bed-glass-arm	
src/rainbow_kitty/ribbon.rs	a_pending_palette_draws_mains_paper_bed_until_its_glass_is_solved	bed-glass-arm	
src/rainbow_kitty/ribbon.rs	a_glass_frame_lays_the_glass_in_the_glass_ink	bed-glass-arm	light-glass-geometry
src/pipeline_native.rs	a_glass_step_reports_the_glass_arm_and_main_none	bed-glass-arm	
```

## 14. Added 2026-10-28: the glass geometry (Prism Glass 1.7)

A frame the bed draws by its glass (§13) lays the glass's own geometry (design of record:
`docs/design/PRISM-GLASS.md` §2.2 and §15.2 1.7): standing on its row's bottom with nothing below
it, 0.96 ch tall at the hand and 0.86 ch behind it on the comet, whole opacity under a 2.5 px top
melt, laid through `aterm_render::ribbon_glass_into`. Nothing sets a light yet
(`prism_glass::LIVE` is `Live::Off` and no host latches one), so every frame draws main's geometry
and the host's dark pin (`aterm-gui` `light_fork_pin_tests.rs`) holds the glass slabs and the
glass plan absent on every dark ground. Its tests are read the same way as §1.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
light-glass-geometry	contract	ribbon::{GLASS_BODY,GLASS_UP_CH,GLASS_TAIL_UP_CH,GLASS_CORE_SHARE,LIGHT_GLASS_WAVE_CH,GLASS_EDGE_PX,GLASS_MIN_PX,GLASS_ALPHA,GLASS_HARD_STEP_T,Segment::{glass_up,glass_jump},Ribbon::{plan_run,emit,glass_planned,glass_up_at,cov_of}}; cursor_glow::Light::{on_page,sheer,glass}; host::{PageFacts,TrailGround::ground}; aterm_render::{ribbon_glass_into,glass_profile,WEDGE_EDGE_PX}	Under glass_lit the plan keeps main's segments and geometry (band() main's for every cell) and adds per boundary the glass's reach glass_up: GLASS_UP_CH at the hand, easing to GLASS_TAIL_UP_CH over COMET_TAPER_CELLS on the comet (the flat spelling holds GLASS_UP_CH), its top swinging LIGHT_GLASS_WAVE_CH with main's wave, under the flow's settle and breath, and scaled by the frame's intensity: half intensity is half height at the same whole opacity, and a run none of whose boundaries reaches GLASS_MIN_PX draws no body, though its frame is still the glass arm's (glass_drew_at, with no glass_span for the guard or the yield): main's paper lays its body there. Coverage is planned under GLASS_ALPHA and never scaled by intensity. Between two cells of a run whose walk differs by more than GLASS_HARD_STEP_T, and at a cohort seam, the glass steps hard: the boundary closes its left cell at that cell's walk and opens its right one at its own (glass_jump), two vertices at one x, each in its side's ink; a cell boundary lerps (one vertex at the mean of its cells' walk), and main's frame lerps across both. Emit lays each run as vertices at spine = the row's bottom (under the pane's bottom and the effect's top), up = the kept glass_up, dn 0, core half the reach, through ribbon_glass_into (source-over, wedges at WEDGE_EDGE_PX, a GLASS_EDGE_PX top melt): nothing on or below the row's bottom, no rail span, no hot edge, its tail easing in without a stair (by its height and with no strip lift or head feather since Prism Glass 1.8, §15; by its opacity until then). A see-through page (PageFacts: a translucent window or a ready wallpaper) gives the light its sheer bit (Light::on_page), under which glass() is false: the bed syncs main's paper table and the frame is main's paper frame byte for byte, with no glass planned or laid and no arm.	0,1,2,3,12	8
```

```tsv
file	test	primary	also
src/rainbow_kitty/ribbon.rs	a_single_row_composer_draws_v4s_glass	light-glass-geometry	light-theme-and-spellings
src/rainbow_kitty/ribbon.rs	a_seam_or_walk_jump_is_a_hard_step_on_light_and_a_cell_boundary_is_not	light-glass-geometry	
src/rainbow_kitty/ribbon.rs	glass_at_half_intensity_is_half_height_at_full_alpha	light-glass-geometry	ribbon-occupied-cov-cap
src/rainbow_kitty/ribbon.rs	a_translucent_page_draws_mains_paper_arm	light-glass-geometry	bed-glass-arm
src/rainbow_kitty/ribbon.rs	no_rail_quad_under_glass	light-glass-geometry	
src/rainbow_kitty/ribbon.rs	the_glass_run_s_tail_eases_in_by_height_without_a_stair	glass-exits-by-height	light-glass-geometry,attack-and-boundary
src/host/trail_ground.rs	a_see_through_page_draws_no_glass	light-glass-geometry	
```

## 15. Added 2026-10-28: the glass's exits, price and cadence (Prism Glass 1.8)

A frame the bed draws by its glass (§13, §14) leaves by its height (design of record:
`docs/design/PRISM-GLASS.md` §2.2 Exits by height and Quad budget, §15.2 1.8): the glass's opacity
moves only at §2.2's four places (its 2.5 px edge, the 18 ms edge-in, BAYER4 and the 120 ms
reduced-motion fade), and the plan's density and every state word stay main's. Nothing sets a
light yet (`prism_glass::LIVE` is `Live::Off` and no host latches one), so no frame runs any of it,
and the host's dark pin (`aterm-gui` `light_fork_pin_tests.rs`) holds the glass's exits absent on
every dark ground (`take_glass_exits`). Its tests are read the same way as §1.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
glass-exits-by-height	contract	ribbon::{Envelope::{fade,ember,scale,ember_floor},MainEnv,GlassEnv,GlassTail,GLASS_TAIL_CELLS,GLASS_SHORT_TRAIL_CELLS,glass_height_floor,take_glass_exits,BedInkLut::main_seen_floor_at,Ribbon::{envelope,env_with,glass_env,cov_of,sample_with,plan_run,emit,next_change_deadline}}	Ribbon::envelope folds a cell's factors (its attack, its melt, each exit, the printed dim, the focus ember, the relight) in main's order into main's one product (MainEnv, byte for byte main's, its ember floor main's seen floor on every frame) or, under glass_lit, into the glass's opacity and height (GlassEnv): the birth's edge-in (its wipe) moves the opacity; the drawn melt, the swoosh, the retract, the retire, the curtain, the focus ember and the relights move the height, and under reduced motion their fades move the opacity while the height stands, but for the focus ember and its relight (Envelope::ember), which move the height under reduced motion too at whole opacity; the printed dim moves the height. A glass boundary's coverage is GLASS_ALPHA × cov0 × the opacity (whole, hot or cold, never scaled by intensity) and its glass_up the glass's reach × the height × the run's oldest end (GlassTail, per vertex: smooth01(d / GLASS_TAIL_CELLS) from an open oldest end, plus smooth01(1 − behind / GLASS_SHORT_TRAIL_CELLS) from the hand); RUN_TAIL_EASE, the strip lift, the head feather and the fine run ends are not laid on glass. The ember holds the height at GLASS_MIN_PX × EMBER_FLOOR_MARGIN over the glass's reach (glass_height_floor), reduced motion or not, so every run stays drawn until the band goes out whole. next_change_deadline steps a glass frame's cells by one pixel of the glass's reach at the hand (its ember the height floor) where main's step by coverage levels, so a settled glass wakes only when it moves a pixel and, its exits done, lays nothing, books no deadline and rests. Every decision the ribbon makes on a cell's light (env_of) reads main's envelope, so the state words stay main's on a glass frame: through typing and every exit, under full and reduced motion, a glass frame's ribbon state words, its bed's main table and every cell's env_of equal its paper twin's (main's light on the same page), and so do the pipeline's state words, the ink ledger's among them.	0,1,2,3,12	3
glass-quad-price	invariant	ribbon::{GLASS_LIP_RESERVE_CH,GLASS_WEDGE_ROW_LIGHTS,glass_slab_rows,glass_tail_wedge_lights,glass_frame_price,rows_of,Ribbon::{slabs_for,glass_slabs_for,glass_slabs_per_cell,emit}}	A glass slab prices glass_slab_rows(ch) lights (its reach at the hand with the wave's swing, a row's straddle, and GLASS_LIP_RESERVE_CH of reserved lip rows: 36 at 28 px), and each row's open oldest end glass_tail_wedge_lights more (every slab of the ease a wedge, GLASS_WEDGE_ROW_LIGHTS a wedge row, its runs charged whole since none rides a ramp); glass_slabs_for is the densest of 1..=SLABS_PER_CELL slabs a cell whose glass_frame_price stays within RIBBON_QUAD_BUDGET. The plan keeps main's density (slabs_for, so band() and the state words are main's) and the glass lays a whole number of its slabs to each of the plan's vertices, never fewer than one (glass_slabs_per_cell, 0 off glass): three while main's plan lays three, two to 136 cells and one past at 28 px (a Retina's 15 × 28 cell, 240 columns). A glass frame lays no more lights than its price less the lip's reserve and never past RIBBON_QUAD_BUDGET; on every dark ground, under every light, every spelling and cell height, the plan chooses main's density for every count of cells (the fold hash captured at the parent) and no glass slab.	0,1,2,3,12	2
```

```tsv
file	test	primary	also
src/rainbow_kitty/ribbon.rs	the_glass_leaves_by_height	glass-exits-by-height	abandon-retract,focus-ember,curtain-law,attack-and-boundary
src/rainbow_kitty/ribbon.rs	a_settled_glass_idles_to_exactly_zero	glass-exits-by-height	
src/rainbow_kitty/ribbon.rs	a_glass_ribbon_folds_its_paper_twins_state_through_every_exit	glass-exits-by-height	focus-ember
src/pipeline_native.rs	a_glass_frame_leaves_its_paper_twins_state_words	glass-exits-by-height	
src/rainbow_kitty/ribbon.rs	a_glass_frame_stays_within_the_ribbon_quad_budget	glass-quad-price	ribbon-quad-budget,rk-ribbon-quad-share
src/rainbow_kitty/ribbon.rs	dark_slabs_for_is_mains	glass-quad-price	ribbon-quad-budget
```

## 16. Added 2026-10-28: the yield (Prism Glass 1.9)

On a frame whose cursor step laid the light glass (§13, §14), the glass yields to the frame's
words (design of record: `docs/design/PRISM-GLASS.md` §4 and §15.2 1.9): every host's word
publish (`EffectsPipeline::publish_words`) cuts the glass in the under stream, word by word, to
the cap each word's ink keeps its bar under, after the word pass and before the word icons' shade.
The glass is light-owned: the letter guard neither weighs nor cuts it. Nothing sets a light yet
(`prism_glass::LIVE` is `Live::Off` and no host latches one), so no frame lays glass, notes a
light-owned span or yields, and the host's dark pin (`aterm-gui` `light_fork_pin_tests.rs`) holds
the light-owned glass and the yield absent on every dark ground. The selection's half on both
backends is `aterm-gpu`'s `glow_under_parity::a_selected_word_under_the_light_glass_shows_its_selection`.
The crown's twin of the first candy frame waits for 3.10. Its tests are read the same way as §1.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
glass-yield	contract	cursor_glow::glass_yield::{cap,scale_end,GlassLaid::{new,in_pane,record,clear},GlassYield::apply,YIELD_BAR,K_MIN,ORPHAN_CELLS,RAMP_KNOT_PX}; CursorGlow::{record_glass,yield_glass,yielded_at,last_yield,glass_laid,forget_glass,yield_held}; GlassYield::forget; EffectsPipeline::{yield_glass,yield_glass_unworded,publish_words,light_arms}; Ribbon::glass_span	The glass a step laid (its light-owned span of the under stream, recorded as the step left it) is found in the published stream by its pieces in draw order, each whole or narrowed by a clip, and only it is cut; no other light (a lip, a meteor, a star) is. Per cell of each glass row of the step's pane (a composed frame's cells outside the pane's columns, another session's, the record's pane, are read as no cell: they take no glass and bound no run, ease or orphan run): a ground off the page (the cell's own bg, a default-bg span off the page, a selection by RenderInput::selection_hit, an image, a wallpaper, a line of another size, an under-text sprite) takes 0; a theme-fg glyph or a blank takes 255; a maximal run of other glyphs takes cap = the least over its inks and the glass composites under it (every row of a ramp by its own ends) of the largest alpha at which the ink keeps YIELD_BAR (GLASS_BAR, §4's bar 5.25) at every lower alpha, 0 under K_MIN (200); a glyph the word pass animates (an InkCell) takes 0 on each frame it animates. A blank run between yielded cells is yielded, and so is a run of blanks and theme-fg glyphs shorter than ORPHAN_CELLS between two yielded words or a yielded word and the caret, the caret's cell with it (a ground off the page is no word and bounds no such run); a blank beside a yielded cell eases back to whole across its cell in RAMP_KNOT_PX knots. Cut pieces continue their gradient (an edge lerp), each end scaled with no channel over its alpha, and an end whose alpha rounds to 0 carries no colour. A frame whose step laid no glass is published as it came, and so is the stream the step's last cut left (CursorGlow's yield_spent and yield_left: a second word pass over its own frame leaves it as it is); every other stream holding the step's glass is cut as the frame's own was, a copy of the glow's uncut stream a retained projection lays (a windowed composed capture's publish) and the heterogeneous route's, which runs no word pass and yields over the frame's cells with no free sprites (yield_glass_unworded; aterm-gui's glass_yield_host_tests.rs drives both host routes); the light report names the yield's arm (`yield`) with the under streams it cut. The yield's state goes with the light (§10.1): every tick starts by forgetting the record, the last cut's report and the spent step (forget_glass), so a step that lays no glass, the first dark step after a light one included, holds none of them (last_yield None, yield_held (None, 0)), and a tick with the light off drops the memo of caps too.	0,1,2,3,12	13
letter-guard-light-owned	contract	InkLedger::{note_light_owned_laid,light_owned,in_light_owned,in_light_owned_ramp,fold_rails,clear_rails,guard_judged}; CursorGlow::{guard_letters,guard_judges,guard_judged}; EffectsPipeline::light_arms; letter_guard::{row_lights,FrameMemo}; InkLedger::{guard_mask_stream,guard_mask_quads,guard_mask_ramps}	The under stream's light-owned spans (quads and ramps, noted from the ribbon's glass span) are never weighed by the letter guard (its judging skips them, as it skips a foot rail) and never cut by its masks; every other light, before or after them, is guarded as before, and the guard's frame memo keys on the spans. With no span noted (every frame until a light latches) the guard's streams are its parent's, word for word. On a frame the glass drew the guard's dims are the glass frame's own (the glass under the letters is not main's paper body, and the light-owned spans go unweighed), so wherever the guard judged the frame's lights against its letters (guard_judges: not spared, with text and some light; InkLedger::guard_judged, forgotten with the rails) the light report names the guard's arm (`guard`) with every stream it cuts: glow_under, glow_under_ramps, cursor_glow_add, cursor_glow_add_ramps and glow_halo.	0,1,2,3	3
```

```tsv
file	test	primary	also
src/cursor_glow/glass_yield.rs	the_caps_are_the_measured_ones	glass-yield	
src/cursor_glow/glass_yield.rs	every_ramp_row_under_a_word_bounds_its_cap	glass-yield	
src/cursor_glow/glass_yield.rs	app_colour_wins_over_the_light_glass	glass-yield	
src/cursor_glow/glass_yield.rs	an_orphan_glass_run_beside_the_caret_is_yielded	glass-yield	
src/cursor_glow/glass_yield.rs	a_ground_off_the_page_bounds_no_orphan_run	glass-yield	
src/cursor_glow/glass_yield.rs	a_zero_scaled_alpha_carries_no_colour	glass-yield	
src/cursor_glow/glass_yield.rs	the_first_candy_frame_over_glass_meets_its_bar	glass-yield	
src/cursor_glow/glass_yield.rs	a_ground_off_the_page_takes_no_glass	glass-yield	
src/cursor_glow/glass_yield.rs	only_the_glass_yields	glass-yield	
src/cursor_glow/glass_yield.rs	a_neighbour_pane_bounds_nothing	glass-yield	
src/pipeline_native.rs	a_glass_frame_s_words_take_the_yield_and_main_none	glass-yield	letter-guard-light-owned
src/pipeline_native.rs	a_step_s_glass_yields_once_per_stream	glass-yield	
src/pipeline_native.rs	the_yield_s_state_goes_with_the_light	glass-yield	
src/cursor_glow/ink_ledger/letter_guard.rs	the_letter_guard_never_cuts_light_owned_glass	letter-guard-light-owned	
src/pipeline_native.rs	a_glass_frame_the_guard_judged_reports_the_guard_arm	letter-guard-light-owned	glass-yield
tests/letter_guard_reuse.rs	the_guarded_streams_fold_as_at_the_parent	letter-guard-light-owned	
```

## 17. Added 2026-10-29: the wrap sink and the hand-row inset (Prism Glass 1.10)

A frame the bed draws by its glass (§13, §14, §15) on a wrapped composer (design of record:
`docs/design/PRISM-GLASS.md` §2.4 and §15.2 1.10) sinks each row the hand has left to a low glass by
its height, and keeps 3 px of page above the hand's row's glass, so the wrapped band reads as one
pane. Its state is a light-only side table beside main's cells, empty on every other frame, and main's
row hold runs unchanged with no hand on the glass's height. Nothing sets a light yet
(`prism_glass::LIVE` is `Live::Off` and no host latches one), so no frame runs any of it, and the
host's dark pin (`aterm-gui` `light_fork_pin_tests.rs`) holds `left_at` and the hand's row absent on
every dark ground. Its tests are read the same way as §1.

```tsv
id	kind	functions	statement	blocking_gaps	primary_tests
glass-wrap-sink	contract	ribbon::{STRUCTURAL_ROWS_MAX,WRAP_SINK_CH,WRAP_SINK_S,WRAP_HAND_TOP_PX,GlassWrap::sunk,wrapped_up,level_step_sink,RunSample::{sunk,glass_cap},Ribbon::{structural_rows,holds_phrase_on,of_phrase,sync_glass_wrap,sunk_on,glass_wraps,wrap_hand,plan_over,plan_run,glass_up_at,next_change_deadline,light_words,reset}}	structural_rows(hand) is the hand's row and the contiguous rows beside it that each hold a cohort of the open phrase that is live (not Fading), not abandoned and not a wake, the STRUCTURAL_ROWS_MAX (8) nearest the hand at most, grown a row up then a row down; with no phrase open it is the hand's row. Each plan under glass_lit syncs the side table before it builds: each cohort of the open phrase on a row of the set the hand is not on is recorded (cohort, row, left_at = now, or the instant a record already on that row holds), the hand's row keeps none (a Backspace or a move back onto a sunk row clears its record at once), and a record whose cohort no longer stands on its row is dropped; the hand's row of a set of two or more rows is wrap_hand. A plan on any other frame clears both, so the falling edge leaves no record, no light word and no sink deadline. On glass a recorded row's glass_up is up × lerp(1, min(1, WRAP_SINK_CH / up), smooth01((now − left_at) / WRAP_SINK_S)) (wrapped_up: by height, the opacity untouched; at once under reduced motion), and wrap_hand's is capped at ch − WRAP_HAND_TOP_PX. next_change_deadline books each record's next one-pixel step of the sink's fall from the hand's reach (level_step_sink), none under reduced motion or once it has sunk. Main's row hold (ease_hold, Run.hold) runs unchanged on glass and moves main's body only: the glass's plan and quads are the same held or free. light_words prints glass.wrap and glass.wrap_hand only while either holds something, never compared.	0,1,2,3,12	5
```

```tsv
file	test	primary	also
src/rainbow_kitty/ribbon.rs	a_wrapped_row_the_hand_left_sinks_to_its_low_glass_by_height	glass-wrap-sink	light-glass-geometry
src/rainbow_kitty/ribbon.rs	the_hand_row_of_a_multi_row_band_keeps_3_px_of_page_above_its_glass	glass-wrap-sink	light-glass-geometry
src/rainbow_kitty/ribbon.rs	a_light_to_dark_flip_clears_every_left_at_and_schedules_no_sink_wake	glass-wrap-sink	glass-exits-by-height
src/rainbow_kitty/ribbon.rs	the_structural_set_is_the_hand_s_open_phrase_rows_capped_at_eight	glass-wrap-sink	
src/rainbow_kitty/ribbon.rs	main_s_row_hold_runs_and_leaves_the_glass_s_height_alone	glass-wrap-sink	light-glass-geometry
src/rainbow_kitty/ribbon.rs	a_scroll_keeps_each_sink_on_its_row	ribbon-translate	glass-wrap-sink
src/rainbow_kitty/ribbon.rs	a_carry_moves_each_sink_with_its_row	ribbon-translate	glass-wrap-sink
```
